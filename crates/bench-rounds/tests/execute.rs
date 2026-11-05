//! The Round bracket against fakes: a body and a provider that succeed, fail, panic or are
//! interrupted at each step, and a ledger or key directory that can't be
//! written. Whatever happens, the Round Teardown runs once the ledger
//! entry is written, what is left is said before the ledger entry is
//! closed, the keys go, and the exit status says how it ended.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Mutex;
use std::time::Duration;

use bench_rounds::cost::ledger;
use bench_rounds::host::Shell;
use bench_rounds::interrupt::{Flag, Interrupted, RoundTeardown};
use bench_rounds::provider::{Access, Leftover, Provider, ProviderError, RoundId, UnixTime};
use bench_rounds::py::Json;
use bench_rounds::round::{Bracket, Clock, Log, RoundBody, Start, Status, Stopped, execute};
use indexmap::IndexMap;

type Events = Rc<RefCell<Vec<String>>>;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "bench-rounds-execute-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A clock that starts at a fixed time and moves on an hour at each
/// reading, whose waits are only recorded.
struct Hours {
    now: Mutex<f64>,
    paused: Mutex<Vec<Duration>>,
}

impl Hours {
    fn new() -> Hours {
        Hours {
            now: Mutex::new(1_790_000_000.0),
            paused: Mutex::new(Vec::new()),
        }
    }
}

impl Clock for Hours {
    fn now(&self) -> UnixTime {
        let mut now = self.now.lock().unwrap();
        let then = *now;
        *now += 3600.0;
        then
    }

    fn sleep(&self, duration: Duration) -> Result<(), Interrupted> {
        self.paused.lock().unwrap().push(duration);
        Ok(())
    }

    fn pause(&self, duration: Duration) {
        self.paused.lock().unwrap().push(duration);
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Destroy {
    Clean,
    Kept,
    KeptPanics,
    Leave,
    Fail,
    Panic,
}

struct FakeProvider {
    events: Events,
    destroy: Destroy,
    flag: Flag,
}

impl Provider for FakeProvider {
    fn name(&self) -> &'static str {
        "static"
    }

    fn rented(&self) -> bool {
        false
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
        round: &RoundId,
        _: UnixTime,
        _: &[&str],
        _: &Access,
    ) -> Result<IndexMap<String, Box<dyn Shell>>, ProviderError> {
        self.events.borrow_mut().push(format!("create {round}"));
        Ok(IndexMap::new())
    }

    fn destroy(&mut self, _: &RoundId) -> Result<Vec<Leftover>, ProviderError> {
        // The Round Teardown's calls run whatever the flag says.
        let stops = self.flag.stops();
        self.events
            .borrow_mut()
            .push(format!("destroy (the flag stops: {stops})"));
        match self.destroy {
            Destroy::Clean | Destroy::Kept | Destroy::KeptPanics => Ok(Vec::new()),
            Destroy::Leave => Ok(vec![Leftover::new("work directory", "h:/tmp/x")]),
            Destroy::Fail => Err(ProviderError::Failed("no answer".into())),
            Destroy::Panic => panic!("the provider broke"),
        }
    }

    fn leftovers(&mut self, _: &RoundId) -> Result<Vec<Leftover>, ProviderError> {
        Ok(Vec::new())
    }

    fn recheck_after(&self) -> Option<Duration> {
        None
    }

    fn manual_cleanup(&self, _: &RoundId) -> Vec<String> {
        vec!["rm -rf /tmp/x".into()]
    }

    fn kept_lines(&self) -> Vec<String> {
        match self.destroy {
            Destroy::Kept => vec!["kept on purpose: the Server binaries".into()],
            Destroy::KeptPanics => panic!("the kept-files diagnostic broke"),
            _ => Vec::new(),
        }
    }
}

#[derive(Clone, PartialEq)]
enum Ends {
    Done,
    Fails,
    Panics,
    Interrupted,
    /// It puts a directory where the ledger is, so that closing the entry
    /// fails.
    BreaksTheLedger,
}

struct FakeBody {
    events: Events,
    ends: Ends,
    stop_panics: bool,
    flag: Flag,
    ledger: PathBuf,
    /// Where its keys were, to see they go.
    keys: Option<PathBuf>,
}

impl RoundBody for FakeBody {
    fn run(&mut self, provider: &mut dyn Provider, start: &Start<'_>) -> Result<(), Stopped> {
        // The ledger entry is written before anything is created.
        let ledger = ledger::load(&self.ledger).unwrap();
        let entry = &ledger.item("rounds").unwrap().as_list().unwrap()[0];
        assert_eq!(entry.item("state").unwrap(), &Json::Str("started".into()));
        assert_eq!(
            entry.item("id").unwrap(),
            &Json::Str(start.round.to_string())
        );
        assert!(start.keys.path().is_dir());
        self.keys = Some(start.keys.path().to_path_buf());
        self.events.borrow_mut().push("run".into());
        start.log.line("the body's own line");
        provider.create(start.round, start.expiry, &["server"], &Access::default())?;
        match self.ends {
            Ends::Done => Ok(()),
            Ends::Fails => Err(Stopped::Failed("server: exit 1: true".into())),
            Ends::Panics => panic!("the body broke"),
            Ends::Interrupted => {
                self.flag.raise("a test");
                Err(Stopped::Interrupted)
            }
            Ends::BreaksTheLedger => {
                std::fs::remove_file(&self.ledger).unwrap();
                std::fs::create_dir(&self.ledger).unwrap();
                Ok(())
            }
        }
    }

    fn stop_servers(&mut self, _begun: &RoundTeardown, _log: &Log) {
        let stops = self.flag.stops();
        self.events
            .borrow_mut()
            .push(format!("stop servers (the flag stops: {stops})"));
        if self.stop_panics {
            panic!("stopping broke");
        }
    }

    fn made(&self) -> String {
        "5 Tests".into()
    }
}

struct Ran {
    status: Status,
    events: Vec<String>,
    log: Vec<String>,
    alerts: Vec<String>,
    ledger: PathBuf,
    keys: Option<PathBuf>,
    dir: PathBuf,
}

impl Ran {
    fn entry(&self) -> Json {
        let ledger = ledger::load(&self.ledger).unwrap();
        ledger.item("rounds").unwrap().as_list().unwrap()[0].clone()
    }
}

fn run(name: &str, ends: Ends, destroy: Destroy, stop_panics: bool, keys_in: Option<&Path>) -> Ran {
    let dir = scratch(name);
    let ledger = dir.join("state").join("ledger.json");
    let events: Events = Rc::default();
    let flag = Flag::new();
    let mut provider = FakeProvider {
        events: events.clone(),
        destroy,
        flag: flag.clone(),
    };
    let mut body = FakeBody {
        events: events.clone(),
        ends,
        stop_panics,
        flag: flag.clone(),
        ledger: ledger.clone(),
        keys: None,
    };
    let clock = Hours::new();
    let log = Log::kept();
    let mut alerts = Vec::new();
    let keys_parent = dir.join("keys");
    std::fs::create_dir_all(&keys_parent).unwrap();
    let status = execute(
        &mut body,
        &mut provider,
        Bracket {
            ledger: &ledger,
            worst_case: 0.57,
            max_hours: 2.0,
            round: Some(RoundId::from("r20260930T100000-abcd")),
            flag: &flag,
            clock: &clock,
            log: &log,
            keys_in: Some(keys_in.unwrap_or(&keys_parent)),
            alert: &mut |line: &str| {
                alerts.push(line.to_string());
                events.borrow_mut().push("alert".into());
            },
        },
    );
    let events = events.borrow().clone();
    Ran {
        status,
        events,
        log: log.lines(),
        alerts,
        ledger,
        keys: body.keys,
        dir,
    }
}

const ID: &str = "r20260930T100000-abcd";

#[test]
fn retained_files_are_said_and_a_broken_diagnostic_cannot_keep_keys_or_the_ledger_open() {
    for (destroy, said) in [(Destroy::Kept, true), (Destroy::KeptPanics, false)] {
        let ran = run("kept-files", Ends::Done, destroy, false, None);
        assert_eq!(ran.status, Status::Done);
        assert_eq!(
            ran.entry().item("state").unwrap(),
            &Json::Str("torn down".into())
        );
        assert!(!ran.keys.unwrap().exists());
        assert_eq!(
            ran.log
                .iter()
                .any(|line| line == "kept on purpose: the Server binaries"),
            said
        );
        std::fs::remove_dir_all(&ran.dir).unwrap();
    }
}

#[test]
fn a_round_that_ends_well_is_torn_down_and_closed() {
    let ran = run("done", Ends::Done, Destroy::Clean, false, None);
    assert_eq!(ran.status, Status::Done);
    assert_eq!(ran.status.code(), 0);
    assert_eq!(
        ran.events,
        [
            "run".to_string(),
            format!("create {ID}"),
            "stop servers (the flag stops: false)".into(),
            "destroy (the flag stops: false)".into(),
        ]
    );
    assert_eq!(
        ran.log,
        [
            format!("Round {ID}: until 2026-09-21 16:13:20+00:00"),
            "the body's own line".into(),
            format!("Round {ID}: torn down; 5 Tests"),
        ]
    );
    let entry = ran.entry();
    assert_eq!(entry.item("state").unwrap(), &Json::Str("torn down".into()));
    // Started at the first reading, ended at the second, an hour on.
    assert_eq!(entry.item("hours").unwrap(), &Json::Float(1.0));
    assert_eq!(entry.item("worst_case").unwrap(), &Json::Float(0.57));
    assert!(!ran.keys.unwrap().exists(), "the keys are gone");
    assert!(ran.alerts.is_empty());
    std::fs::remove_dir_all(&ran.dir).unwrap();
}

#[test]
fn a_body_that_fails_or_panics_stops_the_round_and_is_torn_down() {
    for (ends, said) in [
        (Ends::Fails, "the Round stopped: server: exit 1: true"),
        (Ends::Panics, "the Round stopped: panicked: the body broke"),
    ] {
        let ran = run("fails", ends, Destroy::Clean, false, None);
        assert_eq!(ran.status, Status::Stopped);
        assert_eq!(ran.status.code(), 1);
        assert_eq!(ran.log[2], said);
        assert!(
            ran.events
                .contains(&"destroy (the flag stops: false)".to_string())
        );
        assert_eq!(
            ran.entry().item("state").unwrap(),
            &Json::Str("torn down".into())
        );
        assert!(!ran.keys.unwrap().exists());
        std::fs::remove_dir_all(&ran.dir).unwrap();
    }
}

#[test]
fn an_interrupted_round_is_torn_down_with_the_flag_stopping_nothing() {
    let ran = run(
        "interrupted",
        Ends::Interrupted,
        Destroy::Clean,
        false,
        None,
    );
    assert_eq!(ran.status, Status::Stopped);
    assert_eq!(ran.log[2], "the Round stopped: interrupted by a test");
    // The flag is raised, but stops nothing of the Round Teardown.
    assert_eq!(
        ran.events[2..],
        [
            "stop servers (the flag stops: false)".to_string(),
            "destroy (the flag stops: false)".to_string(),
        ]
    );
    std::fs::remove_dir_all(&ran.dir).unwrap();
}

#[test]
fn what_is_left_is_said_before_the_ledger_and_exits_3() {
    for destroy in [Destroy::Leave, Destroy::Fail, Destroy::Panic] {
        let ran = run("left", Ends::Done, destroy, false, None);
        assert_eq!(ran.status, Status::Left);
        assert_eq!(ran.status.code(), 3);
        assert!(
            ran.alerts[0].contains("LEFT BEHIND, delete by hand: "),
            "{:?}",
            ran.alerts
        );
        assert!(ran.alerts.last().unwrap().contains("rm -rf /tmp/x"));
        assert_eq!(
            ran.entry().item("state").unwrap(),
            &Json::Str("left resources".into())
        );
        assert_eq!(
            ran.log.last().unwrap(),
            &format!("Round {ID}: LEFT RESOURCES; 5 Tests")
        );
        assert!(!ran.keys.unwrap().exists());
        std::fs::remove_dir_all(&ran.dir).unwrap();
    }
}

#[test]
fn a_panic_stopping_the_servers_stops_nothing_else() {
    let ran = run("stop-panics", Ends::Done, Destroy::Clean, true, None);
    assert_eq!(ran.status, Status::Done);
    assert_eq!(
        ran.events.last().unwrap(),
        "destroy (the flag stops: false)"
    );
    assert_eq!(
        ran.entry().item("state").unwrap(),
        &Json::Str("torn down".into())
    );
    std::fs::remove_dir_all(&ran.dir).unwrap();
}

#[test]
fn a_ledger_that_cant_be_written_first_stops_the_round_before_anything() {
    let dir = scratch("no-ledger");
    // The ledger's directory is a file.
    std::fs::write(dir.join("state"), "").unwrap();
    let ledger = dir.join("state").join("ledger.json");
    let events: Events = Rc::default();
    let flag = Flag::new();
    let mut provider = FakeProvider {
        events: events.clone(),
        destroy: Destroy::Clean,
        flag: flag.clone(),
    };
    let mut body = FakeBody {
        events: events.clone(),
        ends: Ends::Done,
        stop_panics: false,
        flag: flag.clone(),
        ledger: ledger.clone(),
        keys: None,
    };
    let (clock, log) = (Hours::new(), Log::kept());
    let mut alerts = Vec::new();
    let status = execute(
        &mut body,
        &mut provider,
        Bracket {
            ledger: &ledger,
            worst_case: 0.57,
            max_hours: 2.0,
            round: None,
            flag: &flag,
            clock: &clock,
            log: &log,
            keys_in: Some(&dir),
            alert: &mut |line: &str| alerts.push(line.to_string()),
        },
    );
    assert_eq!(status, Status::Stopped);
    assert!(events.borrow().is_empty(), "{:?}", events.borrow());
    assert!(alerts[0].starts_with("the ledger "), "{alerts:?}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_ledger_that_cant_be_closed_is_said_after_what_is_left() {
    let ran = run(
        "ledger-breaks",
        Ends::BreaksTheLedger,
        Destroy::Leave,
        false,
        None,
    );
    assert_eq!(ran.status, Status::Left);
    let ledger_alert = ran
        .alerts
        .iter()
        .position(|a| a.starts_with("the ledger "))
        .expect("the ledger's failure said");
    assert!(ledger_alert > 0, "{:?}", ran.alerts);
    assert!(ran.alerts[0].contains("LEFT BEHIND"));
    std::fs::remove_dir_all(&ran.dir).unwrap();
}

#[test]
fn keys_that_cant_be_kept_stop_the_body_but_not_the_round_teardown() {
    let dir = scratch("no-keys");
    let missing = dir.join("not-here");
    let ran = run(
        "no-keys-run",
        Ends::Done,
        Destroy::Clean,
        false,
        Some(&missing),
    );
    assert_eq!(ran.status, Status::Stopped);
    assert!(!ran.events.contains(&"run".to_string()), "{:?}", ran.events);
    assert!(
        ran.events
            .contains(&"destroy (the flag stops: false)".to_string())
    );
    assert!(
        ran.log[1].starts_with("the Round stopped: the Round's key directory: "),
        "{:?}",
        ran.log
    );
    assert_eq!(
        ran.entry().item("state").unwrap(),
        &Json::Str("torn down".into())
    );
    std::fs::remove_dir_all(&ran.dir).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_round_without_an_id_is_given_one_from_its_start() {
    let dir = scratch("id");
    let ledger = dir.join("ledger.json");
    let events: Events = Rc::default();
    let flag = Flag::new();
    let mut provider = FakeProvider {
        events: events.clone(),
        destroy: Destroy::Clean,
        flag: flag.clone(),
    };
    let mut body = FakeBody {
        events: events.clone(),
        ends: Ends::Done,
        stop_panics: false,
        flag: flag.clone(),
        ledger: ledger.clone(),
        keys: None,
    };
    let (clock, log) = (Hours::new(), Log::kept());
    let status = execute(
        &mut body,
        &mut provider,
        Bracket {
            ledger: &ledger,
            worst_case: 0.0,
            max_hours: 0.5,
            round: None,
            flag: &flag,
            clock: &clock,
            log: &log,
            keys_in: Some(&dir),
            alert: &mut |_: &str| {},
        },
    );
    assert_eq!(status, Status::Done);
    let created = &events.borrow()[1];
    let id = created.strip_prefix("create ").unwrap();
    assert!(id.starts_with("r20260921T141320-"), "{id}");
    assert_eq!(id.len(), "r20260920T142000-abcd".len());
    assert_eq!(
        log.lines()[0],
        format!("Round {id}: until 2026-09-21 14:43:20+00:00")
    );
    std::fs::remove_dir_all(&dir).unwrap();
}
