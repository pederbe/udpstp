//! The `aws` CLI v2 as the AWS provider's [`Calls`], and the provider as a
//! Round's [`Provider`].
//!
//! [`Cli`] runs one call per [`Calls::call`]: its argv from
//! [`plan::argv`], run through the Round's [`Runner`], read as
//! [`plan::read_answer`] reads it. It decides nothing; [`AwsProvider`]
//! does.
//!
//! - **Its own limit:** a call still running after [`plan::CALL_LIMIT`] is
//!   stopped, its process group killed, and fails as
//!   [`CallError::Broken`], never as [`CallError::Aws`]. So it is never
//!   retried, as the Python's `TimeoutExpired` isn't, and never taken for
//!   the CLI's own "timed out", which a launch is retried on
//!   ([`plan::RETRY_LAUNCH`]).
//! - **Interrupts:** until the Round Teardown begins, a raised flag stops a
//!   call in flight, and a wait between two attempts, as
//!   [`CallError::Interrupted`]; in the Round Teardown, calls and waits run
//!   to their end whatever the flag says.
//! - **Credentials:** `configure export-credentials` runs through
//!   [`Runner::run_secret`], which keeps its output in one buffer that is
//!   wiped. Output that doesn't begin as a JSON object isn't parsed; in
//!   one, every value but `Expiration`'s is passed over where it is, and
//!   only a string of that is kept. Only the expiry goes on: to the
//!   provider, and to the recorder.
//! - **Never the real CLI by mistake:** [`Cli::new`] refuses a program
//!   named as the real CLI is ([`is_real_cli`]); only [`Cli::live`], for a
//!   Round given `--live`, takes it.
//! - **Recorded:** with a [`Recorder`], each call that reached the CLI is
//!   recorded as it ends, in order; one the runner stopped as ended by a
//!   signal, with no output. A record that can't be written ends the
//!   recording, never the Round: [`Cli::unrecorded`] says why. On native
//!   Windows there is no recorder ([`Recorder::create`] refuses), and paid
//!   Rounds are refused there anyway.
//!
//! `AwsProvider<Cli>` is a [`Provider`]: an interrupt stays one, what the
//! Round left is [`Leftover`]s, and each instance it launched is a
//! [`Host`] over ssh, as [`USER`] at its public address, with its private
//! address for the Round's Tests, in the work directory [`WORK`], and with
//! the Round's known-hosts file, key and `--ssh-dir`. An instance without a
//! public or a private IPv4 address is refused, where the Python goes on
//! with `ubuntu@None` (the design's §7.7).

use std::fmt;
use std::io;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use indexmap::IndexMap;
use serde::Deserialize;
use serde::de::{Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};

use super::record::{Call, Recorder, Stdout};
use super::{AwsProvider, CallError, Calls, Launched, plan};
use crate::host::{Host, RemotePath, Shell};
use crate::num::Num;
use crate::process::{Finished, FinishedSecret, RunError, Runner, text};
use crate::provider::{Access, Leftover, Provider, ProviderError, RoundId, UnixTime};
use crate::py::{self, Json};

/// The user a launched host is reached as: the Ubuntu image's own.
pub const USER: &str = "ubuntu";

/// A launched host's work directory, the Python's for a rented host
/// (`aws.py:263`).
pub const WORK: &str = "/home/ubuntu/udpstp-bench";

/// The most `configure export-credentials` may print: far more than any
/// identity's credentials take, allocated before the CLI starts and never
/// grown. More fails the call.
pub const CREDENTIALS_CAPACITY: usize = 64 << 10;

/// Whether `program` is the real `aws` CLI, by its name, as the Python
/// tells (`round.py:532-535`): `aws`, `aws.exe` or `aws.cmd`, in any case,
/// wherever it is.
pub fn is_real_cli(program: &Path) -> bool {
    program
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            ["aws", "aws.exe", "aws.cmd"].contains(&name.to_ascii_lowercase().as_str())
        })
}

/// The real CLI, which [`Cli::new`] refuses: it reaches a real account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RealCli(pub PathBuf);

impl fmt::Display for RealCli {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{}: the real aws CLI reaches a real account, which only --live allows",
            self.0.display()
        )
    }
}

impl std::error::Error for RealCli {}

/// The CLI's clock: the time it gives the provider, and its waits between
/// two attempts of a call.
#[derive(Clone, Debug)]
pub enum Clock {
    /// A Round's: the wall clock, and each wait slept, the flag looked at
    /// every [`crate::interrupt::POLL`].
    Wall,
    /// A test's: the time fixed at `now`, and each wait's seconds pushed to
    /// `waits` rather than slept. A raised flag still ends a wait, until
    /// the Round Teardown begins.
    Fixed {
        now: f64,
        waits: Arc<Mutex<Vec<u64>>>,
    },
}

/// The `aws` CLI, run for the AWS provider: a [`Calls`].
pub struct Cli {
    program: PathBuf,
    region: String,
    runner: Runner,
    /// The environment the CLI runs in, whole; this process's where
    /// `None`.
    env: Option<IndexMap<String, String>>,
    limit: Duration,
    clock: Clock,
    recorder: Option<Recorder>,
    /// Why recording ended, if it did.
    unrecorded: Option<io::Error>,
}

impl fmt::Debug for Cli {
    /// The environment by its variables' names only: a value may be a
    /// secret.
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Cli")
            .field("program", &self.program)
            .field("region", &self.region)
            .field("runner", &self.runner)
            .field(
                "env",
                &self.env.as_ref().map(|env| env.keys().collect::<Vec<_>>()),
            )
            .field("limit", &self.limit)
            .field("clock", &self.clock)
            .field("recorder", &self.recorder)
            .field("unrecorded", &self.unrecorded)
            .finish()
    }
}

impl Cli {
    /// A CLI that can't be the real one: `program`, such as a test's
    /// `fake-aws`, run for `region` through `runner`, in this process's
    /// environment, with a Round's clock. Refused where `program` is named
    /// as the real CLI is, whoever asks: only [`Cli::live`] takes that.
    pub fn new(program: impl Into<PathBuf>, region: &str, runner: Runner) -> Result<Cli, RealCli> {
        let program = program.into();
        if is_real_cli(&program) {
            return Err(RealCli(program));
        }
        Ok(Cli::live(program, region, runner))
    }

    /// Any CLI, the real one among them: only for a Round given `--live`
    /// (`round.py:556-557`).
    pub fn live(program: impl Into<PathBuf>, region: &str, runner: Runner) -> Cli {
        Cli {
            program: program.into(),
            region: region.to_string(),
            runner,
            env: None,
            limit: plan::CALL_LIMIT,
            clock: Clock::Wall,
            recorder: None,
            unrecorded: None,
        }
    }

    /// The CLI runs in `env` alone, nothing of this process's: for tests,
    /// [`plan::sealed_env`]'s, in which no real credentials or
    /// configuration can be found.
    pub fn with_env(mut self, env: IndexMap<String, String>) -> Cli {
        self.env = Some(env);
        self
    }

    /// Each call may take `limit`, not [`plan::CALL_LIMIT`]: for tests.
    pub fn with_limit(mut self, limit: Duration) -> Cli {
        self.limit = limit;
        self
    }

    pub fn with_clock(mut self, clock: Clock) -> Cli {
        self.clock = clock;
        self
    }

    /// Each call is recorded by `recorder` from now on.
    pub fn with_recorder(mut self, recorder: Recorder) -> Cli {
        self.recorder = Some(recorder);
        self.unrecorded = None;
        self
    }

    /// What runs the CLI, whose flag stops it; the hosts the provider
    /// hands the Round run through a clone of it.
    pub fn runner(&self) -> &Runner {
        &self.runner
    }

    /// The recorder, while it records.
    pub fn recorder(&self) -> Option<&Recorder> {
        self.recorder.as_ref()
    }

    /// Why recording ended: the first record that couldn't be written. The
    /// calls went on; the records stop before the one that failed.
    pub fn unrecorded(&self) -> Option<&io::Error> {
        self.unrecorded.as_ref()
    }

    fn command(&self, argv: &[String]) -> Command {
        let mut command = Command::new(&self.program);
        command.args(argv);
        if let Some(env) = &self.env {
            command.env_clear().envs(env);
        }
        command
    }

    /// Writes one call's record, where there is a recorder. The first that
    /// can't be written ends the recording, and is kept.
    fn record(&mut self, call: &Call) {
        let Some(recorder) = &mut self.recorder else {
            return;
        };
        if let Err(e) = recorder.record(call) {
            self.recorder = None;
            self.unrecorded = Some(e);
        }
    }

    /// Records a call the runner didn't let finish, where it reached the
    /// CLI: as ended by a signal, with no output. One that never started
    /// did nothing.
    fn unfinished(&mut self, argv: &[String], seconds: f64, error: &RunError, stdout: Stdout) {
        if !matches!(error, RunError::Spawn(_)) {
            self.record(&Call {
                argv,
                status: None,
                seconds,
                stdout,
                stderr: b"",
            });
        }
    }
}

impl Calls for Cli {
    fn call(&mut self, args: &[String]) -> Result<Json, CallError> {
        let argv = plan::argv(&[], args, &self.region);
        // An interrupt before the call is caught here, where it is known
        // that nothing ran, so nothing is recorded: the runner says
        // Interrupted alike for a call it wouldn't start and for one it
        // stopped, which is recorded. One raised between this look and the
        // runner's is recorded as a call a signal ended, though none ran.
        if self.runner.flag().stops() {
            return Err(CallError::Interrupted);
        }
        let started = Instant::now();
        let ran = self.runner.run(&mut self.command(&argv), None, self.limit);
        let seconds = started.elapsed().as_secs_f64();
        match ran {
            Ok(done) => {
                self.record(&Call {
                    argv: &argv,
                    status: status(done.returncode),
                    seconds,
                    stdout: Stdout::Bytes(&done.stdout),
                    stderr: &done.stderr,
                });
                answer_of(args, &done)
            }
            Err(error) => {
                self.unfinished(&argv, seconds, &error, Stdout::Bytes(b""));
                Err(stopped(args, &self.program, self.limit, error))
            }
        }
    }

    fn credentials_expire(&mut self) -> Result<Option<String>, CallError> {
        let args = plan::export_credentials();
        let argv = plan::argv(&[], &args, &self.region);
        // As in `call`.
        if self.runner.flag().stops() {
            return Err(CallError::Interrupted);
        }
        let started = Instant::now();
        let ran = self.runner.run_secret(
            &mut self.command(&argv),
            None,
            self.limit,
            CREDENTIALS_CAPACITY,
        );
        let seconds = started.elapsed().as_secs_f64();
        match ran {
            Ok(done) => {
                let expiry = expiry_of(&args, &done);
                let expiration = expiry.as_ref().ok().and_then(Option::as_deref);
                self.record(&Call {
                    argv: &argv,
                    status: status(done.returncode),
                    seconds,
                    stdout: Stdout::Credentials { expiration },
                    stderr: &done.stderr,
                });
                expiry
            }
            Err(error) => {
                self.unfinished(
                    &argv,
                    seconds,
                    &error,
                    Stdout::Credentials { expiration: None },
                );
                Err(stopped(&args, &self.program, self.limit, error))
            }
        }
    }

    fn sleep(&mut self, seconds: u64) -> Result<(), CallError> {
        match &self.clock {
            Clock::Wall => self
                .runner
                .flag()
                .sleep(Duration::from_secs(seconds))
                .map_err(|_| CallError::Interrupted),
            Clock::Fixed { waits, .. } => {
                if self.runner.flag().stops() {
                    return Err(CallError::Interrupted);
                }
                waits
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(seconds);
                Ok(())
            }
        }
    }

    /// Python's `time.time()`.
    fn now(&mut self) -> f64 {
        match &self.clock {
            Clock::Wall => SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0.0, |since| since.as_secs_f64()),
            Clock::Fixed { now, .. } => *now,
        }
    }
}

/// `aws X Y`: a call as its messages name it.
fn said(args: &[String]) -> String {
    let words: Vec<&str> = args.iter().take(2).map(String::as_str).collect();
    format!("aws {}", words.join(" "))
}

/// Python's `returncode` as a status: `None` where a signal ended the CLI.
fn status(returncode: i32) -> Option<i32> {
    (returncode >= 0).then_some(returncode)
}

fn not_text(args: &[String], stream: &str) -> CallError {
    CallError::Broken(format!("{}: its {stream} isn't UTF-8", said(args)))
}

/// What a finished call gave, as the Python's `aws()` reads it: both its
/// outputs as text first, as `text=True` decodes them before the status is
/// looked at, so that either one not UTF-8 raises, whatever the status;
/// then [`plan::read_answer`].
fn answer_of(args: &[String], done: &Finished) -> Result<Json, CallError> {
    let stdout = text(&done.stdout).map_err(|_| not_text(args, "output"))?;
    let stderr = text(&done.stderr).map_err(|_| not_text(args, "error output"))?;
    plan::read_answer(args, status(done.returncode), &stdout, &stderr)
}

/// When the credentials expire, as the Python's `check` reads
/// `configure export-credentials` (`aws.py:181-186`): none where the call
/// failed or printed nothing; else the output must be one JSON object, of
/// which only `Expiration` is read ([`expiration_field`]), and taken as
/// [`plan::expiration`] takes it: a string's text, nothing where it is
/// false, and anything else refused. No error says anything of the output.
fn expiry_of(args: &[String], done: &FinishedSecret) -> Result<Option<String>, CallError> {
    // Read where it is: `text()` would copy it. Line ends matter to JSON
    // only inside a string, where a raw one is refused either way.
    let stdout = std::str::from_utf8(&done.stdout).map_err(|_| not_text(args, "output"))?;
    text(&done.stderr).map_err(|_| not_text(args, "error output"))?;
    if status(done.returncode) != Some(0) || py::strip(stdout).is_empty() {
        return Ok(None);
    }
    match expiration_field(stdout) {
        None => Err(CallError::Broken(format!(
            "{}: its output isn't a JSON object",
            said(args)
        ))),
        Some(Expiry::Text(text)) => Ok(Some(text)),
        Some(Expiry::False) => Ok(None),
        Some(Expiry::Other(kind)) => Err(CallError::Broken(format!(
            "the credentials' expiry is a {kind}"
        ))),
    }
}

/// An `Expiration` as the Python's `if expiration:` and `_parse_time` take
/// it: a string's text; false, which leaves nothing to check; or the kind
/// of anything else, which the Python can't read as a time, as
/// [`Json::type_name`] names it.
#[derive(Debug, PartialEq)]
enum Expiry {
    Text(String),
    False,
    Other(&'static str),
}

impl<'de> Deserialize<'de> for Expiry {
    /// Only a string is kept, borrowed from the output where serde_json can
    /// lend it and copied only as the expiry; a list's or a dict's items
    /// are passed over where they are, counted only for being there.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Expiry, D::Error> {
        struct Value;

        impl<'de> Visitor<'de> for Value {
            type Value = Expiry;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a JSON value")
            }

            fn visit_borrowed_str<E>(self, v: &'de str) -> Result<Expiry, E> {
                Ok(Expiry::of(v))
            }

            fn visit_str<E>(self, v: &str) -> Result<Expiry, E> {
                Ok(Expiry::of(v))
            }

            fn visit_unit<E>(self) -> Result<Expiry, E> {
                Ok(Expiry::False)
            }

            fn visit_bool<E>(self, v: bool) -> Result<Expiry, E> {
                Ok(if v {
                    Expiry::Other("bool")
                } else {
                    Expiry::False
                })
            }

            fn visit_i64<E>(self, v: i64) -> Result<Expiry, E> {
                Ok(if v == 0 {
                    Expiry::False
                } else {
                    Expiry::Other("int")
                })
            }

            fn visit_u64<E>(self, v: u64) -> Result<Expiry, E> {
                Ok(if v == 0 {
                    Expiry::False
                } else {
                    Expiry::Other("int")
                })
            }

            fn visit_f64<E>(self, v: f64) -> Result<Expiry, E> {
                Ok(if v == 0.0 {
                    Expiry::False
                } else {
                    Expiry::Other("float")
                })
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut items: A) -> Result<Expiry, A::Error> {
                let mut any = false;
                while items.next_element::<IgnoredAny>()?.is_some() {
                    any = true;
                }
                Ok(if any {
                    Expiry::Other("list")
                } else {
                    Expiry::False
                })
            }

            fn visit_map<A: MapAccess<'de>>(self, mut entries: A) -> Result<Expiry, A::Error> {
                let mut any = false;
                while entries.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {
                    any = true;
                }
                Ok(if any {
                    Expiry::Other("dict")
                } else {
                    Expiry::False
                })
            }
        }

        deserializer.deserialize_any(Value)
    }
}

impl Expiry {
    fn of(text: &str) -> Expiry {
        if text.is_empty() {
            Expiry::False
        } else {
            Expiry::Text(text.to_string())
        }
    }
}

/// Whether a key is `Expiration`, read where it is.
struct IsExpiration(bool);

impl<'de> Deserialize<'de> for IsExpiration {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<IsExpiration, D::Error> {
        struct Key;

        impl Visitor<'_> for Key {
            type Value = IsExpiration;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a key")
            }

            fn visit_str<E>(self, v: &str) -> Result<IsExpiration, E> {
                Ok(IsExpiration(v == "Expiration"))
            }
        }

        deserializer.deserialize_str(Key)
    }
}

/// `json.loads(text).get("Expiration")`, reading nothing else out of the
/// buffer the runner wipes. Output that doesn't begin as a JSON object
/// isn't parsed at all: serde_json would copy a string into the error it
/// then drops, unwiped. In an object, each key is only compared with
/// `Expiration`, every other value is passed over where it is, and of
/// `Expiration`'s only a string is kept ([`Expiry`]). The last
/// `Expiration` counts, as in a dict built from the object; `None` where
/// `text` isn't one JSON object.
fn expiration_field(text: &str) -> Option<Expiry> {
    if !text
        .trim_start_matches([' ', '\t', '\n', '\r'])
        .starts_with('{')
    {
        return None;
    }

    struct Object;

    impl<'de> Visitor<'de> for Object {
        type Value = Expiry;

        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a JSON object")
        }

        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Expiry, A::Error> {
            // No `Expiration` is `None` to the Python's `.get`: false.
            let mut found = Expiry::False;
            while let Some(IsExpiration(is)) = map.next_key()? {
                if is {
                    found = map.next_value()?;
                } else {
                    map.next_value::<IgnoredAny>()?;
                }
            }
            Ok(found)
        }
    }

    let mut reader = serde_json::Deserializer::from_str(text);
    let found = reader.deserialize_map(Object).ok()?;
    reader.end().ok()?;
    Some(found)
}

/// What a call the runner didn't let finish is to the provider: the
/// Round's interrupt, or a call that didn't end as a call does, never an
/// error of the CLI's own.
fn stopped(args: &[String], program: &Path, limit: Duration, error: RunError) -> CallError {
    let call = said(args);
    let program = program.display();
    match error {
        RunError::Interrupted => CallError::Interrupted,
        RunError::TimedOut => CallError::Broken(format!(
            "{call}: no answer within {}, so the CLI was stopped",
            seconds(limit)
        )),
        RunError::TooMuchOutput { cap } => CallError::Broken(format!(
            "{call}: more than {cap} bytes of output, so the CLI was stopped"
        )),
        RunError::Spawn(e) => CallError::Broken(format!("{call}: couldn't start {program}: {e}")),
        RunError::Wait(e) => CallError::Broken(format!("{call}: waiting for {program}: {e}")),
    }
}

/// `300 s`, `0.5 s`.
fn seconds(limit: Duration) -> String {
    if limit.subsec_nanos() == 0 {
        format!("{} s", limit.as_secs())
    } else {
        format!("{} s", limit.as_secs_f64())
    }
}

impl From<CallError> for ProviderError {
    /// An interrupt stays one; the rest is the provider's failure, in the
    /// words the Python's error would have.
    fn from(error: CallError) -> ProviderError {
        match error {
            CallError::Interrupted => ProviderError::Interrupted,
            CallError::Aws(e) => ProviderError::Failed(e.0),
            CallError::Broken(e) => ProviderError::Failed(e),
        }
    }
}

fn as_leftovers(left: Vec<(String, String)>) -> Vec<Leftover> {
    left.into_iter()
        .map(|(kind, id)| Leftover::new(kind, id))
        .collect()
}

/// A launched instance as the Round reaches it (the Python's `Host(role,
/// f"ubuntu@{public}", private, work, ...)`, `aws.py:303-310`): over ssh as
/// [`USER`] at its public address, from the machine running the Round; its
/// private address the one the Round's Tests between its hosts use;
/// [`WORK`] its work directory; with the Round's known-hosts file, key and
/// ssh. Refused without a public or a private IPv4 address, where the
/// Python goes on with `None` (the design's §7.7).
fn reached(
    role: &str,
    launched: &Launched,
    access: &Access,
    runner: &Runner,
) -> Result<Host, ProviderError> {
    let ipv4 = |address: &Option<String>| {
        address
            .as_deref()
            .filter(|a| a.parse::<Ipv4Addr>().is_ok())
            .map(str::to_string)
    };
    let id = &launched.instance_id;
    let public = ipv4(&launched.public_address).ok_or_else(|| {
        ProviderError::Failed(format!(
            "{role}: instance {id} has no public IPv4 address, so the Round can't reach it"
        ))
    })?;
    let private = ipv4(&launched.private_address).ok_or_else(|| {
        ProviderError::Failed(format!(
            "{role}: instance {id} has no private IPv4 address for the Round's Tests"
        ))
    })?;
    let work = RemotePath::new(WORK).expect("a path of safe characters");
    let mut host = Host::ssh(
        role,
        format!("{USER}@{public}"),
        private,
        work,
        runner.clone(),
    )
    .map_err(|e| ProviderError::Failed(e.to_string()))?
    .with_public_address(public);
    if let Some(file) = &access.known_hosts {
        host = host.with_known_hosts(file);
    }
    if let Some(file) = &access.identity {
        host = host.with_identity(file);
    }
    if let Some(dir) = &access.ssh_dir {
        host = host.with_ssh_dir(dir);
    }
    Ok(host)
}

/// What runs the commands of the hosts a provider's calls launched: the
/// Round's own [`Runner`], whose flag stops them.
pub trait HostsRunner {
    fn hosts_runner(&self) -> Runner;
}

impl HostsRunner for Cli {
    fn hosts_runner(&self) -> Runner {
        self.runner.clone()
    }
}

/// The in-process model's hosts are inert: their addresses are the model's
/// made-up ones, so their runner's flag is raised from the start, and each
/// of their calls ends at once as interrupted, before anything is run.
/// A test that gets past `create` on the model never starts `ssh`.
impl HostsRunner for super::fake::Local {
    fn hosts_runner(&self) -> Runner {
        let inert = crate::interrupt::Flag::new();
        inert.raise("the model's hosts are made up");
        Runner::new(inert)
    }
}

/// The AWS provider as a Round's, over the `aws` CLI ([`Cli`]), or over
/// the in-process model for tests ([`super::fake::Local`]).
impl<C: Calls + HostsRunner> Provider for AwsProvider<C> {
    fn name(&self) -> &'static str {
        "aws"
    }

    fn rented(&self) -> bool {
        true
    }

    fn price_per_hour(&self, host_type: &str) -> Result<f64, ProviderError> {
        AwsProvider::price_per_hour(self, host_type)
            .map(Num::to_f64)
            .map_err(|e| ProviderError::Failed(e.0))
    }

    /// Credentials that outlast the Round, and an image whose volumes go
    /// with the instance. The time is the CLI's clock, read when the
    /// credentials are compared, as the Python reads `time.time()` there,
    /// not `now`.
    fn check(&mut self, max_hours: f64, _now: UnixTime) -> Result<(), ProviderError> {
        AwsProvider::check(self, max_hours)?;
        Ok(())
    }

    fn reap(&mut self, now: UnixTime) -> Result<Vec<RoundId>, ProviderError> {
        let reaped = AwsProvider::reap(self, now)?;
        Ok(reaped.into_iter().map(RoundId::from).collect())
    }

    /// The Round's security group and one instance per role, as
    /// [`AwsProvider::create`] makes them, each a [`Host`] over ssh as the
    /// module says. Whatever it launched before a failure is kept for the
    /// Round Teardown, by its ID and by its tag.
    fn create(
        &mut self,
        round: &RoundId,
        expiry: UnixTime,
        roles: &[&str],
        access: &Access,
    ) -> Result<IndexMap<String, Box<dyn Shell>>, ProviderError> {
        let roles: Vec<String> = roles.iter().map(|role| (*role).to_string()).collect();
        let launched =
            AwsProvider::create(self, round.as_str(), expiry, &roles, &access.public_key)?;
        let runner = self.calls.hosts_runner();
        launched
            .iter()
            .map(|(role, host)| {
                let host = reached(role, host, access, &runner)?;
                Ok((role.clone(), Box::new(host) as Box<dyn Shell>))
            })
            .collect()
    }

    fn destroy(&mut self, round: &RoundId) -> Result<Vec<Leftover>, ProviderError> {
        Ok(as_leftovers(AwsProvider::destroy(self, round.as_str())?))
    }

    fn leftovers(&mut self, round: &RoundId) -> Result<Vec<Leftover>, ProviderError> {
        Ok(as_leftovers(AwsProvider::leftovers(self, round.as_str())?))
    }

    fn recheck_after(&self) -> Option<Duration> {
        Some(plan::RECHECK)
    }

    fn manual_cleanup(&self, round: &RoundId) -> Vec<String> {
        AwsProvider::manual_cleanup(self, round.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aws::AwsError;
    use crate::interrupt::Flag;
    use zeroize::Zeroizing;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    fn runner() -> Runner {
        Runner::new(Flag::new())
    }

    #[test]
    fn the_real_cli_is_refused_without_live() {
        for real in [
            "aws",
            "/usr/local/bin/aws",
            "AWS",
            "aws.exe",
            "C:/Program Files/Amazon/AWSCLIV2/aws.exe",
            "aws.cmd",
            "Aws.Cmd",
        ] {
            assert!(is_real_cli(Path::new(real)), "{real}");
            let refused = Cli::new(real, "eu-north-1", runner()).unwrap_err();
            assert_eq!(
                refused.to_string(),
                format!(
                    "{real}: the real aws CLI reaches a real account, which only --live allows"
                )
            );
        }
        for fake in [
            "fake-aws",
            "/target/debug/fake-aws",
            "fake-aws.exe",
            "aws2",
            "awscli",
            "aws.py",
            "python3",
        ] {
            assert!(!is_real_cli(Path::new(fake)), "{fake}");
            assert!(Cli::new(fake, "eu-north-1", runner()).is_ok(), "{fake}");
        }
    }

    /// A Round's CLI: its own limit the Python's 300 s, the wall clock,
    /// this process's environment and no recorder, until a test or the
    /// Round says otherwise. The review's mutation C33 made it 30 s.
    #[test]
    fn a_call_may_take_300_s_unless_told_otherwise() {
        let cli = Cli::new("fake-aws", "eu-north-1", runner()).unwrap();
        assert_eq!(plan::CALL_LIMIT, Duration::from_secs(300));
        assert_eq!(cli.limit, plan::CALL_LIMIT);
        assert!(matches!(cli.clock, Clock::Wall));
        assert!(cli.env.is_none() && cli.recorder.is_none() && cli.unrecorded.is_none());
        let cli = cli.with_limit(Duration::from_secs(1));
        assert_eq!(cli.limit, Duration::from_secs(1));
    }

    #[test]
    fn a_call_runs_the_cli_with_the_region_and_json_output() {
        let env = plan::sealed_env(
            [("HOME".to_string(), "/home/u".to_string())],
            "/usr/bin:/bin",
        );
        let cli = Cli::new("/bin/fake-aws", "eu-north-1", runner())
            .unwrap()
            .with_env(env);
        let argv = plan::argv(&[], &args(&["ec2", "describe-subnets"]), &cli.region);
        let command = cli.command(&argv);
        assert_eq!(command.get_program(), "/bin/fake-aws");
        let given: Vec<&std::ffi::OsStr> = command.get_args().collect();
        assert_eq!(
            given,
            [
                "ec2",
                "describe-subnets",
                "--region",
                "eu-north-1",
                "--output",
                "json"
            ]
        );
        let envs: Vec<(String, String)> = command
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                )
            })
            .collect();
        assert!(envs.contains(&("HOME".to_string(), "/home/u".to_string())));
        assert!(envs.contains(&("PATH".to_string(), "/usr/bin:/bin".to_string())));
        // Its Debug says the environment by its names only.
        let debug = format!("{cli:?}");
        assert!(
            debug.contains("\"HOME\"") && !debug.contains("/home/u"),
            "{debug}"
        );
    }

    fn finished(returncode: i32, stdout: &[u8], stderr: &[u8]) -> Finished {
        Finished {
            returncode,
            stdout: stdout.to_vec(),
            stderr: stderr.to_vec(),
        }
    }

    #[test]
    fn a_call_is_read_as_the_pythons_aws_reads_it() {
        let launch = args(&["ec2", "run-instances", "--image-id", "ami-1"]);
        assert_eq!(
            answer_of(&launch, &finished(0, b"{\"GroupId\": \"sg-1\"}\r\n", b"")),
            Ok(Json::parse("{\"GroupId\": \"sg-1\"}").unwrap())
        );
        assert_eq!(
            answer_of(
                &launch,
                &finished(
                    254,
                    b"",
                    b"\r\nAn error occurred (Throttling) when calling\r\n"
                )
            ),
            Err(CallError::Aws(AwsError(
                "aws ec2 run-instances: An error occurred (Throttling) when calling".into()
            )))
        );
        // Ended by a signal: failed, as a status other than 0.
        assert!(matches!(
            answer_of(&launch, &finished(-9, b"", b"")),
            Err(CallError::Aws(_))
        ));
        assert_eq!(
            answer_of(&launch, &finished(0, b" \n", b"")),
            Ok(Json::Dict(IndexMap::new()))
        );
        assert!(matches!(
            answer_of(&launch, &finished(0, b"not json", b"")),
            Err(CallError::Broken(_))
        ));
        // Output that isn't text raises in the Python whatever the status:
        // never the CLI's error, never retried.
        assert_eq!(
            answer_of(&launch, &finished(0, b"{\"x\": \"\xff\"}", b"")),
            Err(CallError::Broken(
                "aws ec2 run-instances: its output isn't UTF-8".into()
            ))
        );
        assert_eq!(
            answer_of(
                &launch,
                &finished(254, b"", b"An error occurred (Throttling) \xfe")
            ),
            Err(CallError::Broken(
                "aws ec2 run-instances: its error output isn't UTF-8".into()
            ))
        );
    }

    #[test]
    fn the_clis_own_limit_is_never_its_timed_out() {
        let launch = args(&["ec2", "run-instances"]);
        let program = Path::new("/bin/fake-aws");
        let stopped = |error| stopped(&launch, program, plan::CALL_LIMIT, error);
        let CallError::Broken(message) = stopped(RunError::TimedOut) else {
            panic!("the CLI's own limit is no AWS error");
        };
        assert_eq!(
            message,
            "aws ec2 run-instances: no answer within 300 s, so the CLI was stopped"
        );
        // Not even in its words: RETRY_LAUNCH retries an AWS error that says
        // "timed out", and this one must never look like it.
        assert!(!message.contains("timed out"));
        assert_eq!(stopped(RunError::Interrupted), CallError::Interrupted);
        assert!(matches!(
            stopped(RunError::TooMuchOutput { cap: 7 }),
            CallError::Broken(m) if m == "aws ec2 run-instances: more than 7 bytes of output, so the CLI was stopped"
        ));
        assert!(matches!(
            stopped(RunError::Spawn(io::Error::other("no such file"))),
            CallError::Broken(m) if m.starts_with("aws ec2 run-instances: couldn't start ")
        ));
        assert_eq!(
            super::stopped(
                &launch,
                program,
                Duration::from_millis(1500),
                RunError::TimedOut
            ),
            CallError::Broken(
                "aws ec2 run-instances: no answer within 1.5 s, so the CLI was stopped".into()
            )
        );
    }

    const KEY: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
    const TOKEN: &str = "IQoJb3JpZ2luX2VjEXAMPLETOKEN";

    fn exported(returncode: i32, stdout: &str, stderr: &[u8]) -> FinishedSecret {
        FinishedSecret {
            returncode,
            stdout: Zeroizing::new(stdout.as_bytes().to_vec()),
            stderr: stderr.to_vec(),
        }
    }

    #[test]
    fn only_the_expiry_is_read_of_the_credentials() {
        let export = plan::export_credentials();
        let expiry =
            |returncode, stdout: &str| expiry_of(&export, &exported(returncode, stdout, b""));
        let full = format!(
            "{{\"Version\": 1, \"AccessKeyId\": \"ASIAABCDEFGHIJKLMNOP\", \"SecretAccessKey\": \"{KEY}\", \
             \"SessionToken\": \"{TOKEN}\", \"Nested\": {{\"a\": [\"{KEY}\", 1.5, null]}}, \
             \"Expiration\": \"2026-09-29T20:00:00+00:00\"}}\n"
        );
        assert_eq!(
            expiry(0, &full),
            Ok(Some("2026-09-29T20:00:00+00:00".into()))
        );
        // The last of two, as in a dict.
        assert_eq!(
            expiry(0, "{\"Expiration\": \"a\", \"Expiration\": \"b\"}"),
            Ok(Some("b".into()))
        );
        // Nothing to check: none, empty, null, no output, a failed call.
        for (returncode, stdout) in [
            (0, "{\"AccessKeyId\": \"ASIAABCDEFGHIJKLMNOP\"}"),
            (0, "{\"Expiration\": null}"),
            (0, "{\"Expiration\": \"\"}"),
            (0, "{\"Expiration\": 0}"),
            (0, " \n"),
            (0, ""),
            (254, "not even JSON"),
            (-9, ""),
        ] {
            assert_eq!(
                expiry(returncode, stdout),
                Ok(None),
                "{returncode} {stdout:?}"
            );
        }
        // What the Python can't read either, and never an error that says
        // what the output held.
        for stdout in [
            format!("[\"{KEY}\"]"),
            format!("\"{KEY}\""),
            format!("{{\"SecretAccessKey\": \"{KEY}\""),
            format!("{{\"SecretAccessKey\": \"{KEY}\"}} {TOKEN}"),
            format!("{{\"SecretAccessKey\": \"{KEY}\", \"Expiration\": 5}}"),
            format!("{{\"Expiration\": [\"{KEY}\"]}}"),
        ] {
            let Err(CallError::Broken(message)) = expiry(0, &stdout) else {
                panic!("{stdout}: refused");
            };
            assert!(
                !message.contains(KEY) && !message.contains(TOKEN),
                "{message}"
            );
        }
        assert_eq!(
            expiry_of(&export, &exported(0, "{}", b"\xfe")),
            Err(CallError::Broken(
                "aws configure export-credentials: its error output isn't UTF-8".into()
            ))
        );
        let mut not_text = exported(0, "", b"");
        not_text.stdout = Zeroizing::new(b"{\"Expiration\": \"\xff\"}".to_vec());
        assert_eq!(
            expiry_of(&export, &not_text),
            Err(CallError::Broken(
                "aws configure export-credentials: its output isn't UTF-8".into()
            ))
        );
    }

    /// An `Expiration` of every kind is taken as `plan::expiration` takes
    /// it from the whole answer read as the Python reads it, though only a
    /// string is kept of it (B3's review, F5).
    #[test]
    fn the_expiry_of_any_kind_is_taken_as_the_python_takes_it() {
        let export = plan::export_credentials();
        for value in [
            "null",
            "true",
            "false",
            "0",
            "7",
            "0.0",
            "1.5",
            "\"\"",
            "\"2026-09-29T20:00:00+00:00\"",
            "\"\\u0032026-09-29T20:00:00+00:00\"",
            "[]",
            "[0]",
            "[[]]",
            "{}",
            "{\"a\": 1}",
            "{\"\": null}",
        ] {
            let answer = format!(
                "{{\"SecretAccessKey\": \"{KEY}\", \"Expiration\": {value}, \"SessionToken\": \"{TOKEN}\"}}"
            );
            let python = plan::expiration(&Json::parse(&answer).unwrap());
            let port = expiry_of(&export, &exported(0, &answer, b""));
            assert_eq!(port, python, "{value}");
        }
        // -0, an int to Python, which serde_json reads as a float: false
        // either way.
        let minus_zero = "{\"Expiration\": -0}";
        assert_eq!(expiry_of(&export, &exported(0, minus_zero, b"")), Ok(None));
    }

    /// What doesn't begin as a JSON object isn't parsed, so no part of it
    /// reaches serde_json's error; one that does is read to its end.
    #[test]
    fn only_a_json_object_is_parsed_for_the_expiry() {
        for text in [
            format!("\"{KEY}\""),
            format!("[\"{KEY}\", {{\"Expiration\": \"x\"}}]"),
            format!("{KEY} {{\"Expiration\": \"x\"}}"),
            "7".to_string(),
            "null".to_string(),
        ] {
            assert_eq!(expiration_field(&text), None, "{text}");
        }
        assert_eq!(
            expiration_field(" \r\n\t{\"Expiration\": \"x\"} \n"),
            Some(Expiry::Text("x".into()))
        );
        assert_eq!(expiration_field("{} {}"), None, "one object, no more");
        assert_eq!(
            expiration_field("{\"Expir\\u0061tion\": \"x\", \"expiration\": \"y\"}"),
            Some(Expiry::Text("x".into())),
            "a key as JSON spells it, and in its case"
        );
    }

    #[test]
    fn a_wait_between_attempts_ends_on_an_interrupt_until_the_round_teardown() {
        // A test's clock: waits written down, not slept.
        let waits = Arc::new(Mutex::new(Vec::new()));
        let flag = Flag::new();
        let mut cli = Cli::new("fake-aws", "eu-north-1", Runner::new(flag.clone()))
            .unwrap()
            .with_clock(Clock::Fixed {
                now: 1_790_000_000.25,
                waits: Arc::clone(&waits),
            });
        assert_eq!(cli.now(), 1_790_000_000.25);
        assert_eq!(cli.sleep(1), Ok(()));
        flag.raise("a test");
        assert_eq!(cli.sleep(2), Err(CallError::Interrupted));
        // Nothing starts either.
        assert_eq!(
            cli.call(&args(&["sts", "get-caller-identity"])),
            Err(CallError::Interrupted)
        );
        assert_eq!(cli.credentials_expire(), Err(CallError::Interrupted));
        let _begun = flag.stop_acting();
        assert_eq!(cli.sleep(4), Ok(()));
        assert_eq!(*waits.lock().unwrap(), [1, 4]);

        // A Round's clock: a real wait, cut short.
        let flag = Flag::new();
        let mut cli = Cli::new("fake-aws", "eu-north-1", Runner::new(flag.clone())).unwrap();
        let raiser = std::thread::spawn({
            let flag = flag.clone();
            move || {
                std::thread::sleep(Duration::from_millis(100));
                flag.raise("a test");
            }
        });
        let start = Instant::now();
        assert_eq!(cli.sleep(30), Err(CallError::Interrupted));
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "{:?}",
            start.elapsed()
        );
        raiser.join().unwrap();
        let wall = cli.now();
        let since = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        assert!((since - wall).abs() < 60.0);
    }

    #[test]
    fn an_interrupt_stays_one_for_the_round() {
        assert!(matches!(
            ProviderError::from(CallError::Interrupted),
            ProviderError::Interrupted
        ));
        assert!(matches!(
            ProviderError::from(CallError::Aws(AwsError("aws ec2 run-instances: x".into()))),
            ProviderError::Failed(m) if m == "aws ec2 run-instances: x"
        ));
        assert!(matches!(
            ProviderError::from(CallError::Broken("aws ec2 wait: no answer".into())),
            ProviderError::Failed(m) if m == "aws ec2 wait: no answer"
        ));
    }

    fn launched(public: Option<&str>, private: Option<&str>) -> Launched {
        Launched {
            instance_id: "i-1".into(),
            public_address: public.map(str::to_string),
            private_address: private.map(str::to_string),
        }
    }

    #[test]
    fn a_launched_host_is_reached_at_its_public_address() {
        let access = Access {
            public_key: "ssh-ed25519 AAAA t".into(),
            known_hosts: Some(PathBuf::from("/k/known_hosts")),
            identity: Some(PathBuf::from("/k/id_ed25519")),
            ssh_dir: None,
        };
        let host = reached(
            "server",
            &launched(Some("198.51.100.1"), Some("10.0.0.1")),
            &access,
            &runner(),
        )
        .unwrap();
        assert_eq!(host.destination(), "ubuntu@198.51.100.1");
        assert_eq!(host.address(), "10.0.0.1");
        assert_eq!(host.public_address(), Some("198.51.100.1"));
        assert_eq!(host.work().as_str(), WORK);
        assert!(!host.is_local());
        // Without a usable address, refused: the Python goes on with
        // `ubuntu@None`.
        for (public, private, why) in [
            (
                None,
                Some("10.0.0.1"),
                "server: instance i-1 has no public IPv4 address, so the Round can't reach it",
            ),
            (
                Some("None"),
                Some("10.0.0.1"),
                "server: instance i-1 has no public IPv4 address, so the Round can't reach it",
            ),
            (
                Some("198.51.100.1 -oProxyCommand=x"),
                Some("10.0.0.1"),
                "server: instance i-1 has no public IPv4 address, so the Round can't reach it",
            ),
            (
                Some("2001:db8::1"),
                Some("10.0.0.1"),
                "server: instance i-1 has no public IPv4 address, so the Round can't reach it",
            ),
            (
                Some("198.51.100.1"),
                None,
                "server: instance i-1 has no private IPv4 address for the Round's Tests",
            ),
            (
                Some("198.51.100.1"),
                Some("10.0.0.1; x"),
                "server: instance i-1 has no private IPv4 address for the Round's Tests",
            ),
        ] {
            let Err(ProviderError::Failed(message)) =
                reached("server", &launched(public, private), &access, &runner())
            else {
                panic!("{public:?} {private:?}: refused");
            };
            assert_eq!(message, why);
        }
    }
}
