//! The Round bracket: what every Round does around its body, whatever the body is, so
//! that no way out of a Round skips its Round Teardown. The Python's
//! `execute` (`scripts/bench/round.py:743-815`) and its `say` and
//! `log_line` (`:70-89`).
//!
//! A Round's body, a [`RoundBody`], creates its hosts with the provider
//! and makes its Round Runs; `udpstp-bench` gives the first Round's. The
//! bracket around it knows nothing of udpstp.
//!
//! What the bracket and the body print never stops the Round: a closed
//! terminal, or a `round.log` that can't be written, is passed over
//! ([`say`], [`Log`]), as the Python's `say` passes over its errors, so
//! that above all the Round Teardown goes on.

use std::any::Any;
use std::fmt;
use std::fs::OpenOptions;
use std::io::Write;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::cost::ledger;
use crate::host::{HostError, Stop};
use crate::interrupt::{Flag, Interrupted, RoundTeardown};
use crate::provider::{Access, Provider, ProviderError, RoundId, UnixTime};
use crate::round_teardown::destroy_resources;
use crate::secrets::{SecretDir, SshKey};
use crate::utc::Utc;

/// Prints `text` and a line break on standard output, unless the terminal
/// is gone: a closed terminal mustn't stop the Round, and above all not
/// its Round Teardown (`say`). `println!` would panic there.
pub fn say(text: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{text}").and_then(|()| out.flush());
}

/// [`say`] on standard error.
pub fn alert(text: &str) {
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "{text}").and_then(|()| err.flush());
}

/// The Round's log: each line stamped with the time of day in UTC,
/// `[HH:MM:SS]`, on the terminal and at the end of `--out`'s `round.log`,
/// neither of which may stop the Round when it fails (`log_line`).
#[derive(Debug)]
pub struct Log {
    /// `round.log`, where lines go besides the terminal.
    file: Option<PathBuf>,
    /// A test's: the lines kept, unstamped, and nothing printed.
    kept: Option<Mutex<Vec<String>>>,
}

impl Log {
    /// The terminal, and `round.log` in `out`.
    pub fn new(out: &Path) -> Log {
        Log {
            file: Some(out.join("round.log")),
            kept: None,
        }
    }

    /// A log that keeps its lines, unstamped, and prints nothing: for
    /// tests.
    pub fn kept() -> Log {
        Log {
            file: None,
            kept: Some(Mutex::new(Vec::new())),
        }
    }

    /// Logs `message`.
    pub fn line(&self, message: &str) {
        if let Some(kept) = &self.kept {
            kept.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(message.to_string());
            return;
        }
        let stamp = Utc::from_timestamp(wall_clock())
            .map(|t| t.hms())
            .unwrap_or_else(|| "??:??:??".to_string());
        let line = format!("[{stamp}] {message}");
        say(&line);
        if let Some(file) = &self.file {
            let _ = OpenOptions::new()
                .create(true)
                .append(true)
                .open(file)
                .and_then(|mut f| writeln!(f, "{line}"));
        }
    }

    /// The lines a [`Log::kept`] kept.
    pub fn lines(&self) -> Vec<String> {
        self.kept
            .as_ref()
            .map(|kept| kept.lock().unwrap_or_else(PoisonError::into_inner).clone())
            .unwrap_or_default()
    }
}

/// The time now, as Python's `time.time()` gives it.
pub fn wall_clock() -> UnixTime {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// A Round's clock: the time, and its waits. Wall-clock time, since a
/// monotonic clock stops while this machine sleeps, and the rented hosts'
/// dead-man switches don't (`round.py:788-790`).
pub trait Clock: Sync {
    /// The time now.
    fn now(&self) -> UnixTime;

    /// Waits `duration`, or until the flag stops the Round.
    fn sleep(&self, duration: Duration) -> Result<(), Interrupted>;

    /// Waits `duration` whatever the flag says: the Round Teardown's
    /// pauses.
    fn pause(&self, duration: Duration);
}

/// A Round's own clock: the wall clock, and waits that look at `flag`.
#[derive(Clone, Debug)]
pub struct WallClock {
    pub flag: Flag,
}

impl Clock for WallClock {
    fn now(&self) -> UnixTime {
        wall_clock()
    }

    fn sleep(&self, duration: Duration) -> Result<(), Interrupted> {
        self.flag.sleep(duration)
    }

    fn pause(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// Why a Round's body stopped before its end: the Round was interrupted,
/// or it can't go on, for what the message says (the Python's
/// `Refused`, a host's error, the provider's, or the tooling's).
#[derive(Debug, PartialEq, Eq)]
pub enum Stopped {
    Interrupted,
    Failed(String),
}

impl fmt::Display for Stopped {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Stopped::Interrupted => f.write_str("interrupted"),
            Stopped::Failed(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for Stopped {}

impl From<Stop> for Stopped {
    fn from(stop: Stop) -> Stopped {
        match stop {
            Stop::Host(e) => Stopped::Failed(e.message().to_string()),
            Stop::Interrupted => Stopped::Interrupted,
        }
    }
}

impl From<HostError> for Stopped {
    fn from(e: HostError) -> Stopped {
        Stopped::Failed(e.message().to_string())
    }
}

impl From<Interrupted> for Stopped {
    fn from(_: Interrupted) -> Stopped {
        Stopped::Interrupted
    }
}

impl From<ProviderError> for Stopped {
    fn from(e: ProviderError) -> Stopped {
        match e {
            ProviderError::Failed(why) => Stopped::Failed(why),
            ProviderError::Interrupted => Stopped::Interrupted,
        }
    }
}

/// What the bracket gives a Round's body: the Round's ID, when it expires,
/// its private directory for keys, and its log.
pub struct Start<'a> {
    pub round: &'a RoundId,
    pub expiry: UnixTime,
    pub keys: &'a SecretDir,
    pub log: &'a Log,
}

/// A Round's body, which the bracket runs between the ledger entry and the
/// Round Teardown.
pub trait RoundBody {
    /// Creates the Round's hosts with `provider`, prepares and checks them,
    /// makes the Round Runs and collects what they left, as far as it
    /// gets.
    fn run(&mut self, provider: &mut dyn Provider, start: &Start<'_>) -> Result<(), Stopped>;

    /// Stops the Servers the body started: the Round Teardown's first
    /// step, which the proof that the flag stops nothing any more lets
    /// run to the end.
    fn stop_servers(&mut self, begun: &RoundTeardown, log: &Log);

    /// What the Round made, for its last line: `12 Tests`.
    fn made(&self) -> String;
}

/// How the Round reaches the hosts `provider` creates, with the Round's
/// keys in `keys`: for rented hosts, a fresh SSH key of the Round's own
/// (`ssh_key`), whose public half their first boot installs, and a
/// known-hosts file of the Round's own; hosts that exist are reached as
/// they are. `ssh_dir` is `--ssh-dir`.
pub fn access(
    provider: &dyn Provider,
    keys: &SecretDir,
    ssh_dir: Option<&Path>,
    make_key: &mut dyn FnMut(&SecretDir) -> Result<SshKey, Stopped>,
) -> Result<Access, Stopped> {
    let mut access = Access {
        ssh_dir: ssh_dir.map(Path::to_path_buf),
        ..Access::default()
    };
    if provider.rented() {
        let (identity, public_key) = make_key(keys)?;
        access.public_key = public_key;
        access.known_hosts = Some(keys.path().join("known_hosts"));
        access.identity = Some(identity);
    }
    Ok(access)
}

/// How a Round ended, as its exit status says it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// It made its Round Runs and left nothing: 0.
    Done,
    /// It stopped early, or couldn't start, and left nothing: 1.
    Stopped,
    /// It left something to delete by hand: 3, whatever else happened.
    Left,
}

impl Status {
    /// The exit status: 0, 1 or 3. (2 is a Round refused before its
    /// ledger entry.)
    pub fn code(self) -> u8 {
        match self {
            Status::Done => 0,
            Status::Stopped => 1,
            Status::Left => 3,
        }
    }
}

/// What [`execute`] needs besides the body and the provider.
pub struct Bracket<'a> {
    /// The ledger file both tools share.
    pub ledger: &'a Path,
    /// The worst case's total, $, which the ledger entry keeps.
    pub worst_case: f64,
    /// How long the Round may run, h: its hosts expire then.
    pub max_hours: f64,
    /// The Round's ID where the caller made it already (to name the raw
    /// records of its calls); else one is made from its start.
    pub round: Option<RoundId>,
    /// The flag the signals raise, which stops acting when the Round
    /// Teardown begins.
    pub flag: &'a Flag,
    pub clock: &'a dyn Clock,
    pub log: &'a Log,
    /// Where the Round's key directory goes: the system's temporary
    /// directory where `None`, never `--out`.
    pub keys_in: Option<&'a Path>,
    /// The Round's standard error ([`alert`]), where what is left is
    /// said.
    pub alert: &'a mut dyn FnMut(&str),
}

/// Runs a Round: the bracket around `body`.
///
/// 1. The Round's ID and expiry, and its ledger entry, first, so that a
///    crash from here on leaves a record for the next preflight's reaper.
///    A ledger that can't be written stops the Round before anything is
///    created.
/// 2. Its private key directory, outside `--out`.
/// 3. The body, under `catch_unwind`: a panic is a failure, not a way
///    out. How it stopped, if it did, is logged.
/// 4. The Round Teardown, whatever happened: the flag stops acting (its
///    proof goes to each step); the body's Servers are stopped, under
///    `catch_unwind`; the provider's resources are destroyed and listed
///    again ([`destroy_resources`]), which says what is left before the
///    ledger is written; the keys are removed; the ledger entry is
///    closed, and a ledger that can't be written is said.
///
/// Its status: [`Status::Left`] whenever something is left, else
/// [`Status::Stopped`] where the body didn't end well, the flag was
/// raised, or the ledger or the keys couldn't be kept, else
/// [`Status::Done`].
pub fn execute(
    body: &mut dyn RoundBody,
    provider: &mut dyn Provider,
    bracket: Bracket<'_>,
) -> Status {
    let Bracket {
        ledger: ledger_file,
        worst_case,
        max_hours,
        round,
        flag,
        clock,
        log,
        keys_in,
        alert,
    } = bracket;
    let now = clock.now();
    let round = match round {
        Some(round) => Some(round),
        None => crate::secrets::random_hex(2)
            .ok()
            .and_then(|suffix| RoundId::new(now, &suffix)),
    };
    let expiry = now + max_hours * 3600.0;
    let Some(round) = round else {
        alert(&format!("the Round's ID: the clock says {now}"));
        return Status::Stopped;
    };
    // The ledger first: a crash from here on leaves a record for the next
    // preflight's reaper.
    let entry = ledger::entry(round.as_str(), provider.name(), now, expiry, worst_case);
    let started = match entry {
        // Its error names the ledger.
        Some(entry) => ledger::start(ledger_file, entry).map_err(|e| e.to_string()),
        None => Err(format!(
            "the ledger {}: the clock says {now}",
            ledger_file.display()
        )),
    };
    if let Err(e) = started {
        alert(&e);
        return Status::Stopped;
    }
    let until = Utc::from_timestamp(expiry).map_or_else(|| expiry.to_string(), |t| t.to_string());
    log.line(&format!("Round {round}: until {until}"));
    let keys = match keys_in {
        Some(parent) => SecretDir::create_in(parent),
        None => SecretDir::create(),
    };
    let mut ran_well = false;
    let mut keys = match keys {
        Err(e) => {
            log.line(&format!(
                "the Round stopped: the Round's key directory: {e}"
            ));
            None
        }
        Ok(keys) => {
            let start = Start {
                round: &round,
                expiry,
                keys: &keys,
                log,
            };
            let ran = catch_unwind(AssertUnwindSafe(|| body.run(provider, &start)));
            let ran = match ran {
                Ok(ran) => ran,
                Err(panic) => Err(Stopped::Failed(format!("panicked: {}", panicked(&panic)))),
            };
            // A signal after the body's last call still stops the Round,
            // as it would have raised in the Python.
            let ran = ran.and_then(|()| {
                if flag.raised() {
                    Err(Stopped::Interrupted)
                } else {
                    Ok(())
                }
            });
            if let Err(stopped) = &ran {
                let why = match stopped {
                    Stopped::Interrupted => {
                        format!("interrupted by {}", flag.cause().unwrap_or("a signal"))
                    }
                    Stopped::Failed(why) => why.clone(),
                };
                log.line(&format!("the Round stopped: {why}"));
            }
            ran_well = ran.is_ok();
            Some(keys)
        }
    };
    // Nothing may stop the Round Teardown.
    let begun = flag.stop_acting();
    let _ = catch_unwind(AssertUnwindSafe(|| body.stop_servers(&begun, log)));
    let left = destroy_resources(
        provider,
        &round,
        begun,
        &mut |line: &str| log.line(line),
        alert,
        &mut |pause| clock.pause(pause),
    );
    // A provider's diagnostic must not keep the keys or ledger open.
    let kept_lines = catch_unwind(AssertUnwindSafe(|| provider.kept_lines())).unwrap_or_default();
    for line in kept_lines {
        log.line(&line);
    }
    if let Some(keys) = &mut keys
        && let Err(e) = keys.remove()
    {
        // Drop tries again; the keys mustn't stay unsaid.
        alert(&format!(
            "the Round's keys are still in {}: {e}; delete it by hand",
            keys.path().display()
        ));
    }
    let mut kept = true;
    let ended = clock.now();
    let state = if left.is_empty() {
        "torn down"
    } else {
        "left resources"
    };
    if let Err(e) = ledger::finish(ledger_file, round.as_str(), state, (ended - now) / 3600.0) {
        alert(&e.to_string());
        kept = false;
    }
    let made = catch_unwind(AssertUnwindSafe(|| body.made())).unwrap_or_default();
    let ended = if left.is_empty() {
        "torn down"
    } else {
        "LEFT RESOURCES"
    };
    log.line(&format!("Round {round}: {ended}; {made}"));
    if !left.is_empty() {
        Status::Left
    } else if ran_well && kept {
        Status::Done
    } else {
        Status::Stopped
    }
}

/// What a panic said.
fn panicked(panic: &Box<dyn Any + Send>) -> String {
    panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("no message")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_stops_a_round_stays_what_it_is() {
        assert_eq!(Stopped::from(Stop::Interrupted), Stopped::Interrupted);
        assert_eq!(
            Stopped::from(Stop::Host(HostError::new("server: exit 1: x"))),
            Stopped::Failed("server: exit 1: x".into())
        );
        assert_eq!(
            Stopped::from(ProviderError::Interrupted),
            Stopped::Interrupted
        );
        assert_eq!(Stopped::from(Interrupted), Stopped::Interrupted);
    }

    #[test]
    fn a_kept_log_keeps_its_lines_unstamped() {
        let log = Log::kept();
        log.line("one");
        log.line("two");
        assert_eq!(log.lines(), ["one", "two"]);
    }

    #[test]
    fn a_log_writes_round_log_stamped_and_never_fails() {
        let dir = std::env::temp_dir().join(format!("bench-rounds-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = Log::new(&dir);
        log.line("the first");
        log.line("the second");
        let text = std::fs::read_to_string(dir.join("round.log")).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with('[') && lines[0].ends_with("] the first"));
        assert_eq!(lines[0].len(), "[00:00:00] the first".len());
        std::fs::remove_dir_all(&dir).unwrap();
        // A directory that isn't there any more stops nothing.
        log.line("the third");
    }
}
