//! What interrupts a Round: SIGINT, SIGTERM and SIGHUP on Unix; the
//! console's Ctrl-C, Ctrl-Break and close on Windows. Each only raises a
//! [`Flag`] that the Round reads: no signal ends the tool, and none breaks
//! into its Round Teardown.
//!
//! [`install`] is the first thing a Round's `main` does, before any thread
//! starts. On Unix it unblocks the three signals in the calling thread,
//! which every later thread and child inherits, then takes all three with
//! Tokio's signal streams on a current-thread runtime in a thread of its
//! own. It takes them unconditionally, unlike udpstp's Server, which
//! leaves alone a signal it started with ignored: a disposition of
//! "ignored" and a blocked mask both pass to children across `exec`, so a
//! Server a Round started on a local host would outlive its stop signal.
//! Once the handlers are in, children start with the three unblocked and
//! at their default dispositions, whatever the tool inherited (from
//! `nohup`, from `cmd &` in a script, or from a launcher that blocks them).
//!
//! Tokio never unregisters its handlers, and they restart an interrupted
//! call (SA_RESTART), so no blocking call returns early on a signal:
//! every wait of a Round polls the flag or is bounded. [`Flag::sleep`] is
//! such a wait, and so is [`crate::process::Runner::run`].
//!
//! On Windows, a closed console ends the process a few seconds after its
//! close event, whatever the handler does: Tokio parks the thread the
//! event comes on, and Windows then ends the process. That is why a paid
//! Round runs from Linux or WSL.

use std::fmt;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

/// How often a wait looks at the flag: every wait of a Round notices an
/// interrupt within this long.
pub const POLL: Duration = Duration::from_millis(50);

/// Whether the Round was interrupted, and whether that still stops what it
/// runs. Clones share one flag.
#[derive(Clone, Debug)]
pub struct Flag(Arc<State>);

#[derive(Debug)]
struct State {
    count: AtomicU64,
    cause: OnceLock<&'static str>,
    acting: AtomicBool,
}

impl Flag {
    /// A flag that nothing raises but [`Flag::raise`]: for tests, and for
    /// what runs without a Round's signals. [`install`] gives the one the
    /// signals raise.
    pub fn new() -> Flag {
        Flag(Arc::new(State {
            count: AtomicU64::new(0),
            cause: OnceLock::new(),
            acting: AtomicBool::new(true),
        }))
    }

    /// Raises the flag, as a signal does; `cause` names what raised it.
    pub fn raise(&self, cause: &'static str) {
        let _ = self.0.cause.set(cause);
        self.0.count.fetch_add(1, Ordering::SeqCst);
    }

    /// Whether anything raised the flag.
    pub fn raised(&self) -> bool {
        self.count() > 0
    }

    /// How many times it was raised. Signals of one kind that come close
    /// together may count once.
    pub fn count(&self) -> u64 {
        self.0.count.load(Ordering::SeqCst)
    }

    /// What raised it first: `SIGINT`, `SIGTERM`, `SIGHUP`, `Ctrl-C`,
    /// `Ctrl-Break`, `the console's close`, or what a test gave.
    pub fn cause(&self) -> Option<&'static str> {
        self.0.cause.get().copied()
    }

    /// From here on the flag stops nothing and is only read: the Round
    /// Teardown begins, and its commands run to their own limits whatever
    /// the flag says. There is no way back. The proof it returns is what
    /// the Round Teardown's steps require.
    pub fn stop_acting(&self) -> RoundTeardown {
        self.0.acting.store(false, Ordering::SeqCst);
        RoundTeardown { _begun: () }
    }

    /// Whether a raised flag still stops what the Round runs.
    pub fn acting(&self) -> bool {
        self.0.acting.load(Ordering::SeqCst)
    }

    /// Whether a wait should stop now: the flag is raised, and the Round
    /// Teardown hasn't begun.
    pub fn stops(&self) -> bool {
        self.acting() && self.raised()
    }

    /// Sleeps for `duration`, or until the flag stops the Round, looking at
    /// it every [`POLL`].
    pub fn sleep(&self, duration: Duration) -> Result<(), Interrupted> {
        let start = Instant::now();
        loop {
            if self.stops() {
                return Err(Interrupted);
            }
            let left = duration.saturating_sub(start.elapsed());
            if left.is_zero() {
                return Ok(());
            }
            std::thread::sleep(left.min(POLL));
        }
    }
}

impl Default for Flag {
    fn default() -> Flag {
        Flag::new()
    }
}

/// Proof that the Round Teardown has begun: that the Round's flag stops
/// nothing any more, so that a raised flag can't cut short the commands
/// that stop its Servers and destroy what it created. Only
/// [`Flag::stop_acting`] makes one, and the Round Teardown's steps require
/// it: [`crate::round_teardown::destroy_resources`] here, and the Round's
/// own `stop_servers` in `udpstp-bench`. So no step of the Round Teardown
/// can run before the flag has stopped acting (review S2).
///
/// ```
/// use bench_rounds::interrupt::Flag;
/// let flag = Flag::new();
/// flag.raise("SIGINT");
/// let begun = flag.stop_acting();
/// assert!(!flag.stops());
/// # let _ = begun;
/// ```
///
/// Nothing else makes one:
///
/// ```compile_fail
/// let begun = bench_rounds::interrupt::RoundTeardown { _begun: () };
/// ```
#[derive(Clone, Copy, Debug)]
#[must_use = "the Round Teardown's steps take it"]
pub struct RoundTeardown {
    _begun: (),
}

/// A wait stopped because the Round was interrupted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Interrupted;

impl fmt::Display for Interrupted {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("interrupted")
    }
}

impl std::error::Error for Interrupted {}

/// Installs the handlers and returns the flag they raise. Call it once,
/// first in `main`, before any thread starts: on Unix it unblocks the
/// three signals in the calling thread, and a thread started earlier keeps
/// the mask it started with.
///
/// An error means the handlers may be half in, so that a signal would be
/// taken and raise nothing: a Round must not start then.
#[cfg(unix)]
pub fn install() -> io::Result<Flag> {
    use nix::sys::signal::{SigSet, Signal};
    use tokio::signal::unix::{SignalKind, signal};

    let mut three = SigSet::empty();
    for signal in [Signal::SIGINT, Signal::SIGTERM, Signal::SIGHUP] {
        three.add(signal);
    }
    three.thread_unblock().map_err(io::Error::from)?;
    // Tokio's signal driver runs on its I/O driver.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()?;
    let streams = {
        let _entered = runtime.enter();
        [
            (signal(SignalKind::interrupt())?, "SIGINT"),
            (signal(SignalKind::terminate())?, "SIGTERM"),
            (signal(SignalKind::hangup())?, "SIGHUP"),
        ]
    };
    let flag = Flag::new();
    let raised = flag.clone();
    std::thread::Builder::new()
        .name("bench-rounds-signals".into())
        .spawn(move || {
            let mut streams = streams;
            runtime.block_on(async move {
                loop {
                    // A stream ends (`None`) only with its runtime, which
                    // lives as long as this thread.
                    let cause = std::future::poll_fn(|cx| {
                        for (stream, cause) in &mut streams {
                            if let std::task::Poll::Ready(Some(())) = stream.poll_recv(cx) {
                                return std::task::Poll::Ready(*cause);
                            }
                        }
                        std::task::Poll::Pending
                    })
                    .await;
                    raised.raise(cause);
                }
            })
        })?;
    Ok(flag)
}

/// Installs the handlers for the console's Ctrl-C, Ctrl-Break and close,
/// and returns the flag they raise. See the Unix version.
#[cfg(windows)]
pub fn install() -> io::Result<Flag> {
    use tokio::signal::windows::{ctrl_break, ctrl_c, ctrl_close};

    // The console events need no driver: a runtime to poll them is enough.
    let runtime = tokio::runtime::Builder::new_current_thread().build()?;
    let (mut c, mut b, mut close) = {
        let _entered = runtime.enter();
        (ctrl_c()?, ctrl_break()?, ctrl_close()?)
    };
    let flag = Flag::new();
    let raised = flag.clone();
    std::thread::Builder::new()
        .name("bench-rounds-console".into())
        .spawn(move || {
            runtime.block_on(async move {
                loop {
                    use std::task::Poll::{Pending, Ready};
                    let cause = std::future::poll_fn(|cx| {
                        if let Ready(Some(())) = c.poll_recv(cx) {
                            return Ready("Ctrl-C");
                        }
                        if let Ready(Some(())) = b.poll_recv(cx) {
                            return Ready("Ctrl-Break");
                        }
                        if let Ready(Some(())) = close.poll_recv(cx) {
                            return Ready("the console's close");
                        }
                        Pending
                    })
                    .await;
                    raised.raise(cause);
                }
            })
        })?;
    Ok(flag)
}

/// Neither Unix nor Windows: nothing raises the flag.
#[cfg(not(any(unix, windows)))]
pub fn install() -> io::Result<Flag> {
    Ok(Flag::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_flag_is_down_and_acting() {
        let flag = Flag::new();
        assert!(!flag.raised());
        assert_eq!(flag.count(), 0);
        assert_eq!(flag.cause(), None);
        assert!(flag.acting());
        assert!(!flag.stops());
    }

    #[test]
    fn raising_counts_and_keeps_the_first_cause() {
        let flag = Flag::new();
        let clone = flag.clone();
        clone.raise("SIGINT");
        flag.raise("SIGTERM");
        assert!(flag.raised() && clone.raised());
        assert_eq!(flag.count(), 2);
        assert_eq!(flag.cause(), Some("SIGINT"));
        assert!(flag.stops());
    }

    #[test]
    fn once_the_round_teardown_begins_the_flag_stops_nothing() {
        let flag = Flag::new();
        flag.raise("SIGINT");
        let _begun = flag.clone().stop_acting();
        assert!(flag.raised(), "still read");
        assert!(!flag.acting());
        assert!(!flag.stops());
        // Raised again during the Round Teardown: counted, and still
        // stopping nothing.
        flag.raise("SIGTERM");
        assert_eq!(flag.count(), 2);
        assert!(!flag.stops());
        let start = Instant::now();
        assert_eq!(flag.sleep(Duration::from_millis(120)), Ok(()));
        assert!(start.elapsed() >= Duration::from_millis(120));
    }

    #[test]
    fn a_sleep_ends_early_on_an_interrupt() {
        let flag = Flag::new();
        let start = Instant::now();
        assert_eq!(flag.sleep(Duration::from_millis(60)), Ok(()));
        assert!(start.elapsed() >= Duration::from_millis(60));

        let raiser = flag.clone();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            raiser.raise("a test");
        });
        let start = Instant::now();
        assert_eq!(flag.sleep(Duration::from_secs(30)), Err(Interrupted));
        let took = start.elapsed();
        assert!(took < Duration::from_secs(5), "took {took:?}");
        thread.join().unwrap();
        // Raised already: no sleep at all.
        let start = Instant::now();
        assert_eq!(flag.sleep(Duration::from_secs(30)), Err(Interrupted));
        assert!(start.elapsed() < POLL);
    }
}
