//! The provider's part of the Round Teardown: every resource the provider
//! made destroyed, with three attempts, and listed again a while later;
//! what is left said, with the commands that delete it by hand. The rest of the Round
//! Teardown, stopping the Servers, removing the keys and closing the ledger
//! entry, is the Round's own.
//!
//! Nothing stops it. Each attempt, and the list after, runs under
//! `catch_unwind`, as the Python catches every `Exception` there: a panic in
//! a provider is a failed attempt, not a way out. A panic in the log, the
//! alert or the clock the caller hands in is passed over. What is left is said on
//! the Round's standard error before [`destroy_resources`] returns, so
//! before the ledger is written: an operator sees it even if the ledger
//! can't be written.
//!
//! The Round Teardown runs its commands to their own limits whatever the
//! interrupt flag says, and waits only for fixed times. So
//! [`destroy_resources`] takes the proof that the flag has stopped acting,
//! [`RoundTeardown`], which only [`crate::interrupt::Flag::stop_acting`]
//! gives: without it, a raised flag would stop every command of every
//! attempt, and an AWS provider would never terminate its instances.

use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Duration;

use crate::interrupt::RoundTeardown;
use crate::provider::{Leftover, Provider, ProviderError, RoundId, listed};

/// How many times destroying is tried.
pub const ATTEMPTS: u32 = 3;

/// The pause after an attempt that failed or left something.
pub const BETWEEN_ATTEMPTS: Duration = Duration::from_secs(10);

/// Destroys what `provider` created for `round`, trying [`ATTEMPTS`]
/// times with [`BETWEEN_ATTEMPTS`] after each attempt that failed or left
/// something, and logging what each of those failed with or left; then,
/// where the provider says to, lists it again a little later. What is
/// left, if anything, is said on `alert` in red, with the provider's
/// commands that delete it by hand, before this returns.
///
/// `begun` is the proof that the Round's flag stops nothing any more. `log`
/// is the Round's log (the terminal and `round.log`), `alert` its standard
/// error, and `sleep` the clock's wait.
pub fn destroy_resources(
    provider: &mut dyn Provider,
    round: &RoundId,
    begun: RoundTeardown,
    log: &mut dyn FnMut(&str),
    alert: &mut dyn FnMut(&str),
    sleep: &mut dyn FnMut(Duration),
) -> Vec<Leftover> {
    let _ = begun;
    // Nor may what the caller hands in stop it: a panic in the log, on the
    // terminal or in the clock is passed over (review m8).
    let mut log = |line: &str| {
        let _ = catch_unwind(AssertUnwindSafe(|| log(line)));
    };
    let mut alert = |line: &str| {
        let _ = catch_unwind(AssertUnwindSafe(|| alert(line)));
    };
    let mut sleep = |pause: Duration| {
        let _ = catch_unwind(AssertUnwindSafe(|| sleep(pause)));
    };
    let mut left = Vec::new();
    for attempt in 1..=ATTEMPTS {
        match guarded(|| provider.destroy(round)) {
            Ok(nothing) if nothing.is_empty() => {
                left = nothing;
                break;
            }
            Ok(something) => {
                // As the Python logs it (`round.py:845`), in the glossary's
                // words.
                log(&format!(
                    "Round Teardown attempt {attempt} left: {}",
                    listed(&something)
                ));
                left = something;
            }
            Err(e) => {
                log(&format!("Round Teardown attempt {attempt} failed: {e}"));
                left = vec![Leftover::unknown(e)];
            }
        }
        sleep(BETWEEN_ATTEMPTS);
    }
    if left.is_empty()
        && let Some(after) = guarded_value(|| provider.recheck_after()).flatten()
    {
        sleep(after);
        left = match guarded(|| provider.leftovers(round)) {
            Ok(found) => found,
            Err(e) => {
                log(&format!("listing what the Round left failed: {e}"));
                vec![Leftover::unknown(e)]
            }
        };
        if !left.is_empty() {
            log(&format!(
                "listed again {} s after destroying it, the Round left: {}",
                after.as_secs(),
                listed(&left)
            ));
        }
    }
    if !left.is_empty() {
        alert(&red(&format!(
            "LEFT BEHIND, delete by hand: {}",
            listed(&left)
        )));
        let commands = guarded_value(|| provider.manual_cleanup(round)).unwrap_or_default();
        for command in commands {
            alert(&red(&format!("  {command}")));
        }
    }
    left
}

/// A provider's call, with a panic in it as a failure.
fn guarded<T>(call: impl FnOnce() -> Result<T, ProviderError>) -> Result<T, String> {
    match catch_unwind(AssertUnwindSafe(call)) {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(e)) => Err(e.to_string()),
        Err(panic) => Err(panicked(&panic)),
    }
}

/// A provider's answer, or `None` where it panicked.
fn guarded_value<T>(call: impl FnOnce() -> T) -> Option<T> {
    catch_unwind(AssertUnwindSafe(call)).ok()
}

/// What a panic said.
fn panicked(panic: &Box<dyn Any + Send>) -> String {
    let said = panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("no message");
    format!("panicked: {said}")
}

fn red(text: &str) -> String {
    format!("\x1b[31m{text}\x1b[0m")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::Shell;
    use crate::interrupt::Flag;
    use crate::provider::{Access, UnixTime};
    use indexmap::IndexMap;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    /// One event of the Round Teardown, in order.
    #[derive(Debug, PartialEq)]
    enum Event {
        Destroy,
        Leftovers,
        Slept(u64),
        Log(String),
        Alert(String),
    }

    type Events = Rc<RefCell<Vec<Event>>>;

    /// A provider that answers each call from its script, and panics where
    /// the script says so; its calls go into the events.
    #[derive(Default)]
    struct Scripted {
        destroys: Vec<Answer>,
        lists: Vec<Answer>,
        recheck: Option<Duration>,
        cleanup: Vec<String>,
        cleanup_panics: bool,
        events: Events,
    }

    #[derive(Clone)]
    enum Answer {
        Left(Vec<Leftover>),
        Fail(&'static str),
        Panic(&'static str),
    }

    fn answer(script: &mut Vec<Answer>) -> Result<Vec<Leftover>, ProviderError> {
        match script.remove(0) {
            Answer::Left(left) => Ok(left),
            Answer::Fail(why) => Err(ProviderError::Failed(why.into())),
            Answer::Panic(why) => panic!("{why}"),
        }
    }

    impl Provider for Scripted {
        fn name(&self) -> &'static str {
            "scripted"
        }
        fn rented(&self) -> bool {
            true
        }
        fn price_per_hour(&self, _: &str) -> Result<f64, ProviderError> {
            Ok(0.0)
        }
        fn check(&mut self, _: f64, _: UnixTime) -> Result<(), ProviderError> {
            Ok(())
        }
        fn reap(&mut self, _: UnixTime) -> Result<Vec<RoundId>, ProviderError> {
            Ok(Vec::new())
        }
        fn create(
            &mut self,
            _: &RoundId,
            _: UnixTime,
            _: &[&str],
            _: &Access,
        ) -> Result<IndexMap<String, Box<dyn Shell>>, ProviderError> {
            Ok(IndexMap::new())
        }
        fn destroy(&mut self, _: &RoundId) -> Result<Vec<Leftover>, ProviderError> {
            self.events.borrow_mut().push(Event::Destroy);
            answer(&mut self.destroys)
        }
        fn leftovers(&mut self, _: &RoundId) -> Result<Vec<Leftover>, ProviderError> {
            self.events.borrow_mut().push(Event::Leftovers);
            answer(&mut self.lists)
        }
        fn recheck_after(&self) -> Option<Duration> {
            self.recheck
        }
        fn manual_cleanup(&self, round: &RoundId) -> Vec<String> {
            assert!(!self.cleanup_panics, "no commands for {round}");
            self.cleanup.clone()
        }
    }

    /// Runs the provider's part of the Round Teardown: what is left, and
    /// every event in order, the provider's calls among them.
    fn run(mut provider: Scripted) -> (Vec<Leftover>, Vec<Event>) {
        let events = Events::default();
        provider.events = Rc::clone(&events);
        let mut log = |line: &str| events.borrow_mut().push(Event::Log(line.into()));
        let mut alert = |line: &str| events.borrow_mut().push(Event::Alert(line.into()));
        let mut sleep = |d: Duration| events.borrow_mut().push(Event::Slept(d.as_secs()));
        let left = destroy_resources(
            &mut provider,
            &RoundId::from("r1"),
            Flag::new().stop_acting(),
            &mut log,
            &mut alert,
            &mut sleep,
        );
        assert!(provider.destroys.is_empty(), "every destroy answer used");
        assert!(provider.lists.is_empty(), "every list answer used");
        drop(provider);
        let events = Rc::try_unwrap(events).expect("one owner left").into_inner();
        (left, events)
    }

    fn instance(id: &str) -> Leftover {
        Leftover::new("instance", id)
    }

    fn alerts(events: &[Event]) -> Vec<&str> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::Alert(line) => Some(line.as_str()),
                _ => None,
            })
            .collect()
    }

    fn sleeps(events: &[Event]) -> Vec<u64> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::Slept(s) => Some(*s),
                _ => None,
            })
            .collect()
    }

    /// The Python's `Teardown.test_what_a_later_list_finds_is_left`: after
    /// a destroy that left nothing, the Round lists again a while later.
    #[test]
    fn what_a_later_list_finds_is_left() {
        let (left, events) = run(Scripted {
            destroys: vec![Answer::Left(vec![])],
            lists: vec![Answer::Left(vec![instance("i-9")])],
            recheck: Some(Duration::from_secs(30)),
            cleanup: vec!["aws ec2 terminate-instances --instance-ids <IDs above>".into()],
            ..Scripted::default()
        });
        assert_eq!(left, [instance("i-9")]);
        assert_eq!(
            events,
            [
                Event::Destroy,
                Event::Slept(30),
                Event::Leftovers,
                Event::Log(
                    "listed again 30 s after destroying it, the Round left: instance i-9".into()
                ),
                Event::Alert("\x1b[31mLEFT BEHIND, delete by hand: instance i-9\x1b[0m".into()),
                Event::Alert(
                    "\x1b[31m  aws ec2 terminate-instances --instance-ids <IDs above>\x1b[0m"
                        .into()
                ),
            ]
        );
    }

    /// The Python's
    /// `Teardown.test_a_provider_that_creates_nothing_isnt_listed_again`.
    #[test]
    fn a_provider_that_creates_nothing_isnt_listed_again() {
        let (left, events) = run(Scripted {
            destroys: vec![Answer::Left(vec![])],
            ..Scripted::default()
        });
        assert!(left.is_empty());
        assert_eq!(events, [Event::Destroy]);
    }

    #[test]
    fn a_failed_attempt_is_tried_again_after_a_pause() {
        let (left, events) = run(Scripted {
            destroys: vec![
                Answer::Fail("aws ec2 terminate-instances: RequestLimitExceeded"),
                Answer::Left(vec![instance("i-1")]),
                Answer::Left(vec![]),
            ],
            lists: vec![Answer::Left(vec![])],
            recheck: Some(Duration::from_secs(30)),
            ..Scripted::default()
        });
        assert!(left.is_empty());
        assert_eq!(
            events,
            [
                Event::Destroy,
                Event::Log(
                    "Round Teardown attempt 1 failed: aws ec2 terminate-instances: RequestLimitExceeded"
                        .into()
                ),
                Event::Slept(10),
                Event::Destroy,
                Event::Log("Round Teardown attempt 2 left: instance i-1".into()),
                Event::Slept(10),
                Event::Destroy,
                Event::Slept(30),
                Event::Leftovers,
            ]
        );
    }

    #[test]
    fn three_failures_leave_the_last_one_to_check_by_hand() {
        let (left, events) = run(Scripted {
            destroys: vec![
                Answer::Fail("one"),
                Answer::Fail("two"),
                Answer::Fail("three"),
            ],
            recheck: Some(Duration::from_secs(30)),
            cleanup: vec!["by hand".into()],
            ..Scripted::default()
        });
        assert_eq!(left, [Leftover::unknown("three")]);
        // As the Python: a pause after each attempt, the last one too; no
        // list, since something is left already.
        assert_eq!(sleeps(&events), [10, 10, 10]);
        assert!(!events.contains(&Event::Leftovers));
        assert_eq!(
            alerts(&events),
            [
                "\x1b[31mLEFT BEHIND, delete by hand: unknown (three)\x1b[0m",
                "\x1b[31m  by hand\x1b[0m"
            ]
        );
        // What is left is said last: before the caller writes the ledger.
        assert!(matches!(events.last(), Some(Event::Alert(_))));
    }

    /// Each attempt that left something says what, as the Python's
    /// `teardown attempt N left [...]` (`round.py:845`), before the pause
    /// after it (B3's review, F3).
    #[test]
    fn what_destroy_left_three_times_is_left() {
        let (left, events) = run(Scripted {
            destroys: vec![
                Answer::Left(vec![instance("i-1"), instance("i-2")]),
                Answer::Left(vec![instance("i-2")]),
                Answer::Left(vec![instance("i-2")]),
            ],
            recheck: Some(Duration::from_secs(30)),
            ..Scripted::default()
        });
        assert_eq!(left, [instance("i-2")]);
        assert_eq!(
            events[..4],
            [
                Event::Destroy,
                Event::Log("Round Teardown attempt 1 left: instance i-1, instance i-2".into()),
                Event::Slept(10),
                Event::Destroy,
            ]
        );
        let logged: Vec<&Event> = events
            .iter()
            .filter(|e| matches!(e, Event::Log(_)))
            .collect();
        assert_eq!(
            logged,
            [
                &Event::Log("Round Teardown attempt 1 left: instance i-1, instance i-2".into()),
                &Event::Log("Round Teardown attempt 2 left: instance i-2".into()),
                &Event::Log("Round Teardown attempt 3 left: instance i-2".into()),
            ]
        );
        assert_eq!(sleeps(&events), [10, 10, 10]);
        assert_eq!(
            alerts(&events),
            ["\x1b[31mLEFT BEHIND, delete by hand: instance i-2\x1b[0m"]
        );
    }

    #[test]
    fn a_panic_is_a_failed_attempt_not_a_way_out() {
        let (left, events) = run(Scripted {
            destroys: vec![Answer::Panic("boom"), Answer::Left(vec![])],
            lists: vec![Answer::Panic("list boom")],
            recheck: Some(Duration::from_secs(30)),
            cleanup_panics: true,
            ..Scripted::default()
        });
        assert_eq!(left, [Leftover::unknown("panicked: list boom")]);
        assert_eq!(
            events,
            [
                Event::Destroy,
                Event::Log("Round Teardown attempt 1 failed: panicked: boom".into()),
                Event::Slept(10),
                Event::Destroy,
                Event::Slept(30),
                Event::Leftovers,
                Event::Log("listing what the Round left failed: panicked: list boom".into()),
                Event::Log(
                    "listed again 30 s after destroying it, the Round left: unknown (panicked: list boom)"
                        .into()
                ),
                // The commands to delete it by hand panicked too: what is
                // left is still said.
                Event::Alert(
                    "\x1b[31mLEFT BEHIND, delete by hand: unknown (panicked: list boom)\x1b[0m".into()
                ),
            ]
        );
    }

    #[test]
    fn a_failed_list_is_left_to_check_by_hand() {
        let (left, events) = run(Scripted {
            destroys: vec![Answer::Left(vec![])],
            lists: vec![Answer::Fail("aws ec2 describe-instances: ExpiredToken")],
            recheck: Some(Duration::from_secs(30)),
            ..Scripted::default()
        });
        assert_eq!(
            left,
            [Leftover::unknown(
                "aws ec2 describe-instances: ExpiredToken"
            )]
        );
        assert_eq!(sleeps(&events), [30]);
        assert_eq!(alerts(&events).len(), 1);
    }

    /// Review m8: a log, a terminal or a clock that panics at every call
    /// stops none of the Round Teardown.
    #[test]
    fn a_panic_in_the_log_the_alert_or_the_clock_stops_nothing() {
        let events = Events::default();
        let mut provider = Scripted {
            destroys: vec![
                Answer::Fail("one"),
                Answer::Fail("two"),
                Answer::Fail("three"),
            ],
            cleanup: vec!["by hand".into()],
            events: Rc::clone(&events),
            ..Scripted::default()
        };
        let (logged, alerted, slept) = (Cell::new(0), Cell::new(0), Cell::new(0));
        let left = destroy_resources(
            &mut provider,
            &RoundId::from("r1"),
            Flag::new().stop_acting(),
            &mut |_: &str| {
                logged.set(logged.get() + 1);
                panic!("the log is gone");
            },
            &mut |_: &str| {
                alerted.set(alerted.get() + 1);
                panic!("the terminal is gone");
            },
            &mut |_: Duration| {
                slept.set(slept.get() + 1);
                panic!("the clock is gone");
            },
        );
        assert_eq!(left, [Leftover::unknown("three")]);
        let destroys = events
            .borrow()
            .iter()
            .filter(|e| **e == Event::Destroy)
            .count();
        assert_eq!(destroys, 3);
        // Every attempt logged and paused after, and what is left said with
        // its command.
        assert_eq!((logged.get(), slept.get(), alerted.get()), (3, 3, 2));
    }
}
