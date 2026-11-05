//! The first Round's body (stage C5): what `scripts/bench/round.py`'s
//! `Round` does between the ledger entry and the Round Teardown, and
//! `make_keys`. `bench_rounds::round::execute` runs it, as the Python's
//! `execute` runs its `Round`.
//!
//! - **Keys:** the Round's Operator Key, in both engines' key files, in the
//!   Round's private directory, never in `--out` ([`make_keys`]).
//! - **Hosts:** the provider's, and this machine as `home` where the
//!   matrix has Tests from home.
//! - **Prepare:** each host answers (a fresh cloud host takes a minute),
//!   a rented one's dead-man switch is checked and the host quieted, the
//!   binaries and key files are copied in, and the host's environment is
//!   recorded; then, with Jumbo, both hosts' paths to each other must carry
//!   9000-byte packets, and the Servers start, each in a session of its
//!   own with its PID in a file, and must still be running 2 s later.
//! - **The path check:** a short Fixed-Rate Test each way from each kind
//!   of Client host: a broken path ends the Round cheaply.
//! - **The matrix:** each Round Run in order, as the small [`Schedule`]
//!   says: until the deadline, or three Tests failing in a row. Each
//!   Round Run's counters before and after, its result and error output,
//!   and its line of `summary.jsonl`.
//! - **Collect** the Servers' logs, and **stop** the Servers, the Round
//!   Teardown's first step.
//!
//! Apart from peer-path counters and an explicit Server Engine Thread cap,
//! every call a host is given, its time limit and whether its exit
//! status is checked are the Python's, byte for byte: the step
//! comparisons in `tests/round.rs` hold them to the Python's on scripted
//! hosts (the design's §7.5).
//!
//! A capped udpstp Server case uses the existing isolated precision Server
//! lifecycle, also for Multi-Connection and Upstream Tests. Its requested cap
//! and complete startup log are retained, then the ordinary Server is restored.
//!
//! # Where it departs from the Python
//!
//! - A Client's output that Python's `json` reads but the port can't (a
//!   NaN, an integer past 128 bits, nesting past 128 levels) gives no
//!   Maximum in `summary.jsonl`, where the Python keeps its value, or ends
//!   the Round where that value isn't a JSON object. No Client prints
//!   one; the report reads the result itself.
//! - Where the Python ends with a traceback, a result that isn't a JSON
//!   object and a file it can't write among them, the port ends the Round
//!   with a message: both stop, and both Round Teardowns run.
//! - A PID is ASCII digits; the Python's `isdigit()` also takes other
//!   scripts' digits, which no `sh` writes.
//! - Cloud-pair counter snapshots follow the Test peer's route and its
//!   underlying NIC, retaining all driver counters. Home Tests retain the
//!   historical collector until the Server's home-facing peer is known.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use bench_rounds::counters;
use bench_rounds::dead_man;
use bench_rounds::host::{self, Host, Limit, RUN_LIMIT, RemotePath, Shell, Stop};
use bench_rounds::interrupt::RoundTeardown;
use bench_rounds::num;
use bench_rounds::process::Runner;
use bench_rounds::provider::Provider;
use bench_rounds::py::{self, Json};
use bench_rounds::quote::{join, quote};
use bench_rounds::round::{self, Clock, Log, RoundBody, Start, Stopped};
use bench_rounds::secrets::{self, SecretDir};
use indexmap::IndexMap;
use zeroize::Zeroizing;

use super::commands::{self, OBUDPST_KEYS, UDPSTP_KEYS};
use super::matrix::{ClientHost, Direction, Engine, Rate, RoundRun};
use crate::obudpst;

/// The Operator Key's ID in the Round's key files (`KEY_ID`).
pub const KEY_ID: u8 = 7;

/// How long a fresh host may take to answer, in tries of
/// [`ANSWER_LIMIT`] with [`ANSWER_PAUSE`] between them.
pub const ANSWER_TRIES: u32 = 30;
pub const ANSWER_LIMIT: Limit = Limit::secs(30);
pub const ANSWER_PAUSE: Duration = Duration::from_secs(10);

/// The Python's limit for the counter script.
pub const COUNTERS_LIMIT: Limit = Limit::secs(30);

/// The Python's limit for a Server's `kill` in the Round Teardown, and for
/// reading its PID file there where the Round never got to.
pub const STOP_LIMIT: Limit = Limit::secs(30);

/// The Python's limit for fetching a Server's log.
pub const COLLECT_LIMIT: Limit = Limit::secs(60);

/// How much longer than its Test Interval a Test may take.
pub const TEST_GRACE_S: i64 = 90;

/// The Round stops this long before it expires, so that its Round
/// Teardown ends before the hosts' dead-man switches (`round.py:790`).
pub const STOP_BEFORE_EXPIRY_S: f64 = 10.0 * 60.0;

fn stop_server_script(pid: &str) -> String {
    format!(
        "kill {pid} 2>/dev/null || true; for n in 1 2 3 4 5 6 7 8 9 10; do \
        if ! kill -0 {pid} 2>/dev/null; then echo stopped; exit 0; fi; \
        case \"$(ps -o stat= -p {pid})\" in Z*) echo stopped; exit 0;; esac; \
        sleep 1; done; exit 1"
    )
}

/// The MTU of the interface a host routes through towards `peer`
/// (`MTU_TOWARDS`).
pub fn mtu_towards(peer: &str) -> String {
    format!(
        r"dev=$(ip -o route get {} 2>/dev/null | sed -n 's/.* dev \([^ ]*\).*/\1/p'); cat /sys/class/net/$dev/mtu 2>/dev/null",
        quote(peer)
    )
}

/// Why the host of `role` can't carry the Round's 9000-byte packets to
/// `peer`, from the MTU of the interface it routes through, as the host
/// printed it; or `None` (`jumbo_path_problem`).
pub fn jumbo_path_problem(role: &str, peer: &str, mtu_text: &str) -> Option<String> {
    let mtu = py::strip(mtu_text);
    if mtu.is_empty() || !mtu.bytes().all(|b| b.is_ascii_digit()) {
        let said = if mtu.is_empty() { "nothing" } else { mtu };
        return Some(format!(
            "{role}: couldn't read the MTU of its interface towards {peer} ({said})"
        ));
    }
    // Past u64's digits it is well above 9000, as Python's int reads it.
    if mtu.parse::<u64>().is_ok_and(|mtu| mtu < 9000) {
        return Some(format!(
            "{role}: its interface towards {peer} has an MTU of {mtu}, below the 9000-byte \
             packets of jumbo = true; udpstp's Tests would lose them and OB-UDPST's be \
             fragmented, so set jumbo = false"
        ));
    }
    None
}

/// The Round's Operator Key: its ID and its text, which only the key files
/// hold.
pub struct OperatorKey {
    pub id: u8,
    text: Zeroizing<String>,
}

impl OperatorKey {
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// The Round's Operator Key, fresh, in both engines' key files in `keys`,
/// the owner's alone from their creation (`make_keys`, `round.py:210-221`).
pub fn make_keys(keys: &SecretDir) -> Result<OperatorKey, Stopped> {
    let failed = |e: std::io::Error| Stopped::Failed(format!("the Round's key files: {e}"));
    let text = Zeroizing::new(secrets::random_hex(16).map_err(failed)?);
    let toml = Zeroizing::new(format!(
        "[[key]]\nadmin_key_name = \"round\"\nkey_id = {KEY_ID}\nkey_text = \"{}\"\noperator = true\n",
        text.as_str()
    ));
    keys.write(UDPSTP_KEYS, toml.as_bytes()).map_err(failed)?;
    let udpst = Zeroizing::new(format!("{KEY_ID} {}\n", text.as_str()));
    keys.write(OBUDPST_KEYS, udpst.as_bytes()).map_err(failed)?;
    Ok(OperatorKey { id: KEY_ID, text })
}

/// Where OB-UDPST comes from, where the matrix runs it (`--obudpst`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Obudpst {
    /// Its `udpst` binary, copied to each host.
    Binary(PathBuf),
    /// Built on each host from the pinned source ([`obudpst::build`]).
    Build,
}

/// What the first Round's body takes from the command line.
#[derive(Clone, Debug)]
pub struct Options {
    /// `--udpstp`: the udpstp binary for the hosts.
    pub udpstp: PathBuf,
    /// `--udpstp-home`: the udpstp binary for this machine, if other.
    pub udpstp_home: Option<PathBuf>,
    /// A Linux udpstp-bench executable for host sampling, if not this tool.
    pub measurement_tool: Option<PathBuf>,
    pub obudpst: Option<Obudpst>,
    /// `--raise-buffers`: the hosts' socket-buffer limits raised.
    pub raise_buffers: bool,
    pub udpstp_port: u16,
    pub obudpst_port: u16,
    /// `--home-cap-mbps`: the home line's cap.
    pub home_cap_mbps: i64,
    /// `--out`, which must hold a directory `tests`.
    pub out: PathBuf,
    /// `--ssh-dir`.
    pub ssh_dir: Option<PathBuf>,
    /// The OB-UDPST build script ([`obudpst::script`], or a test's).
    pub obudpst_script: PathBuf,
}

/// What comes next in the first Round's run loop (`Round.run_matrix`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// The Round Run at this index.
    Run(usize),
    /// Out of time: the rest is skipped.
    OutOfTime,
    /// Three Tests failed in a row: the Round ends.
    Failing,
    /// Every Round Run made.
    Done,
}

/// The first Round's run loop, which decides and does nothing: each Round
/// Run in order, until one would end past the deadline, or three Tests
/// fail in a row.
#[derive(Clone, Debug, Default)]
pub struct Schedule {
    next: usize,
    failures: u32,
}

/// Three Tests failing in a row end the Round.
pub const FAILURES_IN_A_ROW: u32 = 3;

impl Schedule {
    pub fn new() -> Schedule {
        Schedule::default()
    }

    /// What comes next at `now`, for Round Runs `runs` that must end
    /// before `deadline`: a Round Run whose Test Interval and overhead
    /// would pass it isn't started.
    pub fn next(&mut self, runs: &[RoundRun], now: f64, deadline: f64) -> Step {
        let Some(run) = runs.get(self.next) else {
            return Step::Done;
        };
        // time.time() + run.test_interval + OVERHEAD_S, in Python's order.
        if now + run.test_interval as f64 + run.overhead_s() as f64 > deadline {
            return Step::OutOfTime;
        }
        self.next += 1;
        Step::Run(self.next - 1)
    }

    /// Whether the Round Run just made passed: [`Step::Failing`] where
    /// three failed in a row.
    pub fn record(&mut self, passed: bool) -> Option<Step> {
        if passed {
            self.failures = 0;
            return None;
        }
        self.failures += 1;
        (self.failures == FAILURES_IN_A_ROW).then_some(Step::Failing)
    }

    /// What the log says for `step`, as the Python's `run_matrix` says it.
    pub fn said(step: &Step, runs: &[RoundRun]) -> Option<String> {
        match step {
            Step::Run(i) => Some(runs[*i].label()),
            Step::OutOfTime => Some("out of time: the rest of the matrix is skipped".to_string()),
            Step::Failing => Some("three Tests in a row failed: the Round ends".to_string()),
            Step::Done => None,
        }
    }
}

/// The first Round's run loop (`Round.run_matrix`): each of `runs` as the
/// [`Schedule`] says, `now` read before each, until one would end past
/// `deadline` or three fail in a row, what the schedule says logged by
/// `log`; `run_test` makes a Round Run and says whether it passed.
pub fn run_loop(
    runs: &[RoundRun],
    deadline: f64,
    now: &mut dyn FnMut() -> f64,
    log: &mut dyn FnMut(&str),
    run_test: &mut dyn FnMut(&RoundRun) -> Result<bool, Stopped>,
) -> Result<(), Stopped> {
    let mut schedule = Schedule::new();
    loop {
        let step = schedule.next(runs, now(), deadline);
        if let Some(said) = Schedule::said(&step, runs) {
            log(&said);
        }
        let Step::Run(i) = step else {
            return Ok(());
        };
        let passed = run_test(&runs[i])?;
        if let Some(end) = schedule.record(passed) {
            if let Some(said) = Schedule::said(&end, runs) {
                log(&said);
            }
            return Ok(());
        }
    }
}

/// The first Round's body.
pub struct FirstRound<'a> {
    options: Options,
    runs: Vec<RoundRun>,
    /// The roles the provider creates: `server`, and `client` where a Test
    /// runs between the Round's hosts.
    roles: Vec<String>,
    /// Round-wide: one Server of each engine serves every Test.
    jumbo: bool,
    clock: &'a dyn Clock,
    /// Runs this machine's commands: the home host's, `ssh-keygen`.
    runner: Runner,
    hosts: IndexMap<String, Box<dyn Shell + 'a>>,
    /// Each Server started: its host's role, its log/PID stem, and what its PID
    /// file said, `None` until the Round read it.
    servers: Vec<StartedServer>,
    samplers: Vec<(String, String)>,
    plan_written: bool,
    udpstp_pid: Option<String>,
    /// The Tests made, the path check's among them.
    tests: usize,
    /// A test's rented hosts' SSH key, in `ssh-keygen`'s place.
    ssh_key: Option<&'a MakeKey>,
    /// A test's home host, in this machine's place.
    home: Option<&'a MakeHome<'a>>,
}

struct StartedServer {
    role: String,
    stem: String,
    pid: Option<String>,
}

/// Makes the home host, this machine, whose work directory is given.
pub type MakeHome<'a> = dyn Fn(&Path) -> Box<dyn Shell + 'a> + 'a;

/// Makes a rented host's SSH key in the Round's key directory.
pub type MakeKey = dyn Fn(&SecretDir) -> Result<secrets::SshKey, Stopped>;

impl<'a> FirstRound<'a> {
    /// The body of a Round of `runs` (not empty) on hosts of `roles`.
    pub fn new(
        options: Options,
        runs: Vec<RoundRun>,
        roles: Vec<String>,
        clock: &'a dyn Clock,
        runner: Runner,
    ) -> FirstRound<'a> {
        let jumbo = runs.first().is_some_and(|run| run.jumbo);
        FirstRound {
            options,
            runs,
            roles,
            jumbo,
            clock,
            runner,
            hosts: IndexMap::new(),
            servers: Vec::new(),
            samplers: Vec::new(),
            plan_written: false,
            udpstp_pid: None,
            tests: 0,
            ssh_key: None,
            home: None,
        }
    }

    /// The home host made by `make`, not as this machine: for tests, which
    /// script it.
    pub fn with_home(mut self, make: &'a MakeHome<'a>) -> FirstRound<'a> {
        self.home = Some(make);
        self
    }

    /// The rented hosts' SSH key made by `make`, not by `ssh-keygen`: for
    /// tests, whose calls must hold the same key.
    pub fn with_ssh_key(mut self, make: &'a MakeKey) -> FirstRound<'a> {
        self.ssh_key = Some(make);
        self
    }

    /// The Tests made so far, the path check's among them.
    pub fn tests(&self) -> usize {
        self.tests
    }

    /// Everything the body does once it has its hosts: prepare them, check
    /// the path, make the Round Runs until `deadline`, collect. `rented`
    /// is whether the hosts are rented, and so have a dead-man switch to
    /// check and timers to quiet.
    pub fn steps(
        &mut self,
        hosts: IndexMap<String, Box<dyn Shell + 'a>>,
        rented: bool,
        key: &OperatorKey,
        keys: &Path,
        deadline: f64,
        log: &Log,
    ) -> Result<(), Stopped> {
        self.hosts = hosts;
        self.save_plan()?;
        self.prepare(rented, keys, log)?;
        self.check_path(key)?;
        self.run_matrix(key, deadline, log)?;
        self.collect()
    }

    fn host(&self, role: &str) -> Result<&dyn Shell, Stopped> {
        self.hosts
            .get(role)
            .map(|host| host.as_ref())
            .ok_or_else(|| Stopped::Failed(format!("the Round has no {role} host")))
    }

    fn port(&self, engine: Engine) -> u16 {
        match engine {
            Engine::Udpstp => self.options.udpstp_port,
            Engine::Obudpst => self.options.obudpst_port,
        }
    }

    /// The Servers' engines, in order of name, as the Python sorts them.
    fn server_engines(&self) -> Vec<Engine> {
        let mut engines: Vec<Engine> = Vec::new();
        for run in &self.runs {
            if !engines.contains(&run.server) {
                engines.push(run.server);
            }
        }
        engines.sort_by_key(|e| e.name());
        engines
    }

    fn write(&self, name: &str, text: &str) -> Result<(), Stopped> {
        let path = self.options.out.join(name);
        fs::write(&path, text)
            .map_err(|e| Stopped::Failed(format!("writing {}: {e}", path.display())))
    }

    /// Waits as the Python's `time.sleep`, which an interrupt ends.
    fn sleep(&self, seconds: u64) -> Result<(), Stopped> {
        Ok(self.clock.sleep(Duration::from_secs(seconds))?)
    }

    fn prepare(&mut self, rented: bool, keys: &Path, log: &Log) -> Result<(), Stopped> {
        if self.runs.iter().any(|r| r.host_samples) {
            fs::write(
                keys.join("first-capture.sh"),
                include_str!("../../../../scripts/first-capture.sh"),
            )
            .map_err(|e| Stopped::Failed(e.to_string()))?;
            let current = std::env::current_exe().map_err(|e| Stopped::Failed(e.to_string()))?;
            let tool = self.options.measurement_tool.as_deref().unwrap_or(&current);
            host::stage_executable(tool, &keys.join("hosts/udpstp-bench"))
                .map_err(|e| Stopped::Failed(e.to_string()))?;
            if self.hosts.contains_key("home") {
                host::stage_executable(&current, &keys.join("home/udpstp-bench"))
                    .map_err(|e| Stopped::Failed(e.to_string()))?;
            }
        }
        let wanted = |engine: Engine| {
            self.runs
                .iter()
                .any(|run| run.client == engine || run.server == engine)
        };
        let name = |name: &str| RemotePath::new(name).expect("a plain name");
        for (role, host) in &self.hosts {
            // A fresh cloud host takes a minute before ssh answers.
            for attempt in 0..ANSWER_TRIES {
                match host.run_checked("true", ANSWER_LIMIT) {
                    Ok(_) => break,
                    Err(Stop::Host(e)) if attempt + 1 == ANSWER_TRIES => return Err(e.into()),
                    Err(Stop::Host(_)) => self.sleep(ANSWER_PAUSE.as_secs())?,
                    Err(Stop::Interrupted) => return Err(Stopped::Interrupted),
                }
            }
            let mut quieted = String::new();
            if rented && role != "home" {
                log.line(&format!("{role}: {}", dead_man::check(host.as_ref())?));
                quieted = host.run(host::QUIET_SCRIPT, host::QUIET_LIMIT)?.stdout;
            }
            let binary = match (&self.options.udpstp_home, role.as_str()) {
                (Some(home), "home") => home,
                _ => &self.options.udpstp,
            };
            host.put(binary, &name("udpstp"), 0o755)?;
            if self.runs.iter().any(|r| r.host_samples) {
                host.put(
                    &keys.join("first-capture.sh"),
                    &name("first-capture"),
                    0o755,
                )?;
                let source = if role == "home" {
                    "home/udpstp-bench"
                } else {
                    "hosts/udpstp-bench"
                };
                host.put(&keys.join(source), &name("udpstp-bench"), 0o755)?;
            }
            if wanted(Engine::Obudpst) {
                match &self.options.obudpst {
                    None => {
                        return Err(Stopped::Failed(
                            "the matrix runs OB-UDPST: give --obudpst PATH, or --obudpst build"
                                .to_string(),
                        ));
                    }
                    Some(Obudpst::Build) => {
                        obudpst::build(host.as_ref(), &self.options.obudpst_script)?
                    }
                    Some(Obudpst::Binary(path)) => host.put(path, &name("udpst"), 0o755)?,
                }
            }
            host.put(&keys.join(UDPSTP_KEYS), &name(UDPSTP_KEYS), 0o600)?;
            host.put(&keys.join(OBUDPST_KEYS), &name(OBUDPST_KEYS), 0o600)?;
            if self.options.raise_buffers && role != "home" {
                host.run(
                    "sudo -n sysctl -qw net.core.rmem_max=16777216 net.core.wmem_max=16777216",
                    RUN_LIMIT,
                )?;
            }
            // After the settings, so that the record shows what the Tests
            // ran with.
            let env = host.run(host::ENVIRONMENT_SCRIPT, RUN_LIMIT)?;
            self.write(
                &format!("env-{role}.txt"),
                &format!("{}{}{quieted}", env.stdout, env.stderr),
            )?;
        }
        let server = self
            .hosts
            .get("server")
            .map(|host| host.as_ref())
            .ok_or_else(|| Stopped::Failed("the Round has no server host".to_string()))?;
        // A path that can't carry the Jumbo packets would fail only
        // udpstp's Tests (it sets DF), so stop here rather than compare
        // unlike Round Runs.
        if self.jumbo
            && let Some(client) = self.hosts.get("client")
        {
            for (host, peer) in [
                (client.as_ref(), server.address()),
                (server, client.address()),
            ] {
                let mtu = host.run(&mtu_towards(peer), RUN_LIMIT)?.stdout;
                if let Some(problem) = jumbo_path_problem(host.role(), peer, &mtu) {
                    return Err(Stopped::Failed(problem));
                }
            }
        }
        for engine in self.server_engines() {
            let pid = self.start_server(
                engine,
                &format!("server-{}", engine.name()),
                false,
                None,
                RUN_LIMIT,
            )?;
            if engine == Engine::Udpstp {
                self.udpstp_pid = Some(pid);
            }
        }
        Ok(())
    }

    fn start_server(
        &mut self,
        engine: Engine,
        stem: &str,
        stamps: bool,
        threads: Option<std::num::NonZeroUsize>,
        limit: Limit,
    ) -> Result<String, Stopped> {
        let command =
            commands::server_settings(engine, self.port(engine), self.jumbo, stamps, threads)
                .join(" ");
        let script = format!("echo $$ > {stem}.pid; exec {command}");
        self.host("server")?.run_checked(
            &format!(
                "rm -f -- {stem}.pid && setsid -f sh -c {} >> {stem}.log 2>&1 < /dev/null",
                quote(&script)
            ),
            limit,
        )?;
        // Kept before its PID is read, for interruption during startup.
        self.servers.push(StartedServer {
            role: "server".into(),
            stem: stem.into(),
            pid: None,
        });
        self.sleep(1)?;
        let pid = py::strip(
            &self
                .host("server")?
                .run(&format!("cat {stem}.pid"), limit)?
                .stdout,
        )
        .to_string();
        if pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
            return Err(Stopped::Failed(format!(
                "the {} Server has no valid PID",
                engine.name()
            )));
        }
        if let Some(started) = self.servers.last_mut() {
            started.pid = Some(pid.clone());
        }
        self.sleep(2)?;
        let alive = self
            .host("server")?
            .run(&format!("kill -0 {pid} && echo alive"), limit)?;
        if !alive.stdout.contains("alive") {
            let said = self
                .host("server")?
                .run(&format!("tail -5 {stem}.log"), limit)?;
            return Err(Stopped::Failed(format!(
                "the {} Server exited at once: {}",
                engine.name(),
                py::strip(&said.stdout)
            )));
        }
        Ok(pid)
    }

    fn stop_test_server(&mut self, purpose: &str) -> Result<(), Stopped> {
        let pid = self
            .udpstp_pid
            .as_deref()
            .filter(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
            .ok_or_else(|| Stopped::Failed(format!("the {purpose} Server has no valid PID")))?;
        let done = self
            .host("server")?
            .run(&stop_server_script(pid), Limit::secs(15))?;
        if done.returncode != 0 || py::strip(&done.stdout) != "stopped" {
            return Err(Stopped::Failed(format!(
                "the {purpose} Server did not stop; the Round ends"
            )));
        }
        self.servers
            .retain(|server| server.pid.as_deref() != Some(pid));
        self.udpstp_pid = None;
        Ok(())
    }

    /// `ADDRESS:PORT` of the Server `run`'s Client reaches: from a host of
    /// the Round its address, from home its public address.
    fn server_address(&self, run: &RoundRun) -> Result<String, Stopped> {
        let server = self.host("server")?;
        let ip = match run.client_host {
            ClientHost::Cloud => server.address(),
            ClientHost::Home => server.public_address().unwrap_or(server.address()),
        };
        Ok(format!("{ip}:{}", self.port(run.server)))
    }

    /// Each host's counters, `when` this Round Run starts or ends.
    fn counters(&self, run: &RoundRun, when: &str) -> Result<(), Stopped> {
        let label = run.label();
        for role in ["server", "client"] {
            if role == "client" && !self.hosts.contains_key("client") {
                continue;
            }
            let script = if run.client_host == ClientHost::Cloud {
                let peer = self.host(if role == "server" { "client" } else { "server" })?;
                counters::towards(peer.address())
            } else {
                counters::SCRIPT.to_owned()
            };
            let done = self.host(role)?.run(&script, COUNTERS_LIMIT)?;
            self.write(&format!("tests/{label}.{role}.{when}.txt"), &done.stdout)?;
        }
        Ok(())
    }

    /// Makes one Test: whether its Client exited with 0.
    fn run_test(&mut self, run: &RoundRun, key: &OperatorKey) -> Result<bool, Stopped> {
        let purpose = if run.server_precision() {
            "precision"
        } else {
            "capped"
        };
        let stem = format!("server-{purpose}-{}", run.index);
        if run.isolated_server() {
            self.stop_test_server(purpose)?;
            self.udpstp_pid = Some(self.start_server(
                Engine::Udpstp,
                &stem,
                run.server_precision(),
                run.server_engine_threads,
                Limit::secs(10),
            )?);
        }
        let client_role = match run.client_host {
            ClientHost::Home => "home",
            ClientHost::Cloud => "client",
        };
        let command = join(&commands::client(run, &self.server_address(run)?, key.id));
        let label = run.label();
        self.counters(run, "before")?;
        let mut captures = Vec::new();
        if run.host_samples {
            for role in ["server", client_role] {
                let sample_stem = format!("vm-{}", run.index);
                let server_stem = if run.isolated_server() {
                    stem.clone()
                } else {
                    format!("server-{}", run.server.name())
                };
                let target = if role == "server" {
                    self.servers
                        .iter()
                        .rev()
                        .find(|s| s.stem == server_stem)
                        .and_then(|s| s.pid.clone())
                        .unwrap_or_else(|| "self".into())
                } else {
                    "self".into()
                };
                let log = if role == "server" && run.server == Engine::Udpstp {
                    format!("{server_stem}.log")
                } else {
                    "-".into()
                };
                self.samplers.push((role.into(), sample_stem.clone()));
                let command = join(&[
                    "./first-capture".into(),
                    "start".into(),
                    sample_stem.clone(),
                    target,
                    (run.test_interval + 100).to_string(),
                    log,
                ]);
                if let Err(Stop::Interrupted) =
                    self.host(role)?.run_checked(&command, Limit::secs(3))
                {
                    return Err(Stopped::Interrupted);
                }
                captures.push((role.to_string(), sample_stem));
            }
        }
        let started = self.clock.now();
        let limit = Limit::new(num::Num::Int((run.test_interval + TEST_GRACE_S).into()))
            .ok_or_else(|| Stopped::Failed(format!("{label}: its Test Interval")))?;
        let (output, error, status) = match self.host(client_role)?.run(&command, limit) {
            Ok(done) => (done.stdout, done.stderr, Some(done.returncode)),
            Err(Stop::Host(e)) => (String::new(), e.message().to_string(), None),
            Err(Stop::Interrupted) => return Err(Stopped::Interrupted),
        };
        if !run.host_samples {
            self.counters(run, "after")?;
        }
        self.write(&format!("tests/{label}.json"), &output)?;
        if !error.is_empty() {
            self.write(&format!("tests/{label}.stderr"), &error)?;
        }
        let seconds = num::py_round(self.clock.now() - started, 1);
        let rate = match run.rate {
            Rate::Search => Json::Str("search".into()),
            Rate::FixedRate(row) => Json::Int(row.into()),
        };
        let mut result: IndexMap<String, Json> = IndexMap::new();
        let mut field = |key: &str, value: Json| {
            result.insert(key.to_string(), value);
        };
        field("label", Json::Str(label.clone()));
        field("status", status.map_or(Json::Null, |s| Json::Int(s.into())));
        field("seconds", Json::Float(seconds));
        field("name", Json::Str(run.name.clone()));
        field("direction", Json::Str(run.direction.name().into()));
        field("rate", rate);
        field("test_connections", Json::Int(run.test_connections.into()));
        field("client_impl", Json::Str(run.client.name().into()));
        field("server_impl", Json::Str(run.server.name().into()));
        field("client_host", Json::Str(run.client_host.name().into()));
        field("repeat", Json::Int(run.repeat.into()));
        if run.kernel_send_timestamps {
            field("kernel_send_timestamps", Json::Bool(true));
        }
        if run.host_samples {
            field("host_samples", Json::Bool(true));
        }
        if run.host_samples || self.runs.iter().any(|r| r.server_engine_threads.is_some()) {
            field("test_interval", Json::Int(run.test_interval.into()));
        }
        if let Some(threads) = run.server_engine_threads {
            field("server_engine_threads", Json::Int(threads.get() as i128));
        }
        field("max_ip_layer_capacity", maximum(&label, &output)?);
        self.tests += 1;
        let line = py::dumps(&Json::Dict(result)) + "\n";
        let path = self.options.out.join("summary.jsonl");
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut f| f.write_all(line.as_bytes()))
            .map_err(|e| Stopped::Failed(format!("writing {}: {e}", path.display())))?;
        if run.host_samples {
            let mut captured_hosts = IndexMap::new();
            let mut stopped = true;
            for (role, sample_stem) in captures {
                let capture = self.stop_sampler(&role, &sample_stem)?;
                stopped &=
                    capture.as_dict().and_then(|c| c.get("stopped")) == Some(&Json::Bool(true));
                captured_hosts.insert(role, capture);
            }
            self.write(
                &format!("tests/{label}.vm.json"),
                &py::dumps(&Json::Dict(IndexMap::from([
                    ("label".into(), Json::Str(label.clone())),
                    ("hosts".into(), Json::Dict(captured_hosts)),
                ]))),
            )?;
            if !stopped {
                return Err(Stopped::Failed(
                    "a host sampler did not stop; the Round ends before another Test".into(),
                ));
            }
            self.counters(run, "after")?;
        }
        if run.isolated_server() {
            // Preserve the Client result before post-Test operations can fail.
            self.stop_test_server(purpose)?;
            let (status, log) = match self
                .host("server")?
                .run(&format!("cat {stem}.log"), Limit::secs(15))
            {
                Ok(done) => (Json::Int(done.returncode.into()), done.stdout),
                Err(Stop::Host(_)) => (Json::Null, String::new()),
                Err(Stop::Interrupted) => return Err(Stopped::Interrupted),
            };
            let capture = Json::Dict(IndexMap::from([
                ("label".into(), Json::Str(label.clone())),
                ("status".into(), status),
                ("log".into(), Json::Str(log)),
            ]));
            self.write(&format!("tests/{label}.server.json"), &py::dumps(&capture))?;
            self.udpstp_pid = Some(self.start_server(
                Engine::Udpstp,
                "server-udpstp",
                false,
                None,
                Limit::secs(10),
            )?);
        }
        Ok(status == Some(0))
    }

    fn stop_sampler(&mut self, role: &str, stem: &str) -> Result<Json, Stopped> {
        let command = join(&["./first-capture", "stop", stem]);
        let capture = match self.host(role)?.run(&command, Limit::secs(3)) {
            Ok(done) if done.returncode == 0 => Json::parse(&done.stdout)
                .ok()
                .filter(|c| c.as_dict().is_some()),
            Err(Stop::Interrupted) => return Err(Stopped::Interrupted),
            _ => None,
        }
        .unwrap_or_else(|| Json::Dict(IndexMap::new()));
        if capture.as_dict().and_then(|c| c.get("stopped")) == Some(&Json::Bool(true)) {
            self.samplers.retain(|(r, s)| r != role || s != stem);
        }
        Ok(capture)
    }

    fn save_plan(&mut self) -> Result<(), Stopped> {
        if self.plan_written
            || !self
                .runs
                .iter()
                .any(|r| r.host_samples || r.server_engine_threads.is_some())
        {
            return Ok(());
        }
        let runs = self
            .runs
            .iter()
            .map(|r| {
                let mut fields = IndexMap::from([
                    ("label".into(), Json::Str(r.label())),
                    ("status".into(), Json::Null),
                    ("name".into(), Json::Str(r.name.clone())),
                    ("direction".into(), Json::Str(r.direction.name().into())),
                    (
                        "rate".into(),
                        match r.rate {
                            Rate::Search => Json::Str("search".into()),
                            Rate::FixedRate(row) => Json::Int(row.into()),
                        },
                    ),
                    (
                        "test_connections".into(),
                        Json::Int(r.test_connections.into()),
                    ),
                    ("client_impl".into(), Json::Str(r.client.name().into())),
                    ("server_impl".into(), Json::Str(r.server.name().into())),
                    ("client_host".into(), Json::Str(r.client_host.name().into())),
                    ("repeat".into(), Json::Int(r.repeat.into())),
                    ("test_interval".into(), Json::Int(r.test_interval.into())),
                    ("host_samples".into(), Json::Bool(r.host_samples)),
                    (
                        "kernel_send_timestamps".into(),
                        Json::Bool(r.kernel_send_timestamps),
                    ),
                ]);
                if let Some(threads) = r.server_engine_threads {
                    fields.insert(
                        "server_engine_threads".into(),
                        Json::Int(threads.get() as i128),
                    );
                }
                Json::Dict(fields)
            })
            .collect();
        let plan = Json::Dict(IndexMap::from([
            (
                "version".into(),
                Json::Int(
                    if self.runs.iter().any(|r| r.server_engine_threads.is_some()) {
                        2
                    } else {
                        1
                    },
                ),
            ),
            ("runs".into(), Json::List(runs)),
        ]));
        let path = self.options.out.join("first-plan.json");
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .and_then(|mut f| f.write_all(py::dumps(&plan).as_bytes()))
            .map_err(|e| Stopped::Failed(format!("writing {}: {e}", path.display())))?;
        self.plan_written = true;
        Ok(())
    }

    /// A short Fixed-Rate Test each way before the matrix, from each kind
    /// of Client host: a broken path ends the Round cheaply.
    fn check_path(&mut self, key: &OperatorKey) -> Result<(), Stopped> {
        let mut client_hosts: Vec<ClientHost> = Vec::new();
        for run in &self.runs {
            if !client_hosts.contains(&run.client_host) {
                client_hosts.push(run.client_host);
            }
        }
        client_hosts.sort_by_key(|h| h.name());
        for client_host in client_hosts {
            for direction in Direction::ALL {
                let row = match client_host {
                    ClientHost::Cloud => 100,
                    ClientHost::Home => 100.min(self.options.home_cap_mbps.div_euclid(2)),
                };
                let row = u16::try_from(row).map_err(|_| {
                    Stopped::Failed(format!("the path check's row {row} from home"))
                })?;
                let probe = RoundRun {
                    index: 0,
                    repeat: 0,
                    name: "path check".to_string(),
                    direction,
                    rate: Rate::FixedRate(row),
                    test_connections: 1,
                    client: Engine::Udpstp,
                    server: Engine::Udpstp,
                    client_host,
                    test_interval: 5,
                    jumbo: self.jumbo,
                    kernel_send_timestamps: false,
                    host_samples: false,
                    server_engine_threads: None,
                };
                if !self.run_test(&probe, key)? {
                    return Err(Stopped::Failed(format!(
                        "the path check failed ({}, {}): see {}/tests/{}.stderr",
                        client_host.name(),
                        direction.name(),
                        self.options.out.display(),
                        probe.label()
                    )));
                }
            }
        }
        Ok(())
    }

    fn run_matrix(&mut self, key: &OperatorKey, deadline: f64, log: &Log) -> Result<(), Stopped> {
        let runs = self.runs.clone();
        let clock = self.clock;
        run_loop(
            &runs,
            deadline,
            &mut || clock.now(),
            &mut |line| log.line(line),
            &mut |run| self.run_test(run, key),
        )
    }

    fn collect(&mut self) -> Result<(), Stopped> {
        if let Some(server) = self.hosts.get("server") {
            for engine in self.server_engines() {
                let done =
                    server.run(&format!("cat server-{}.log", engine.name()), COLLECT_LIMIT)?;
                self.write(&format!("server-{}.log", engine.name()), &done.stdout)?;
            }
        }
        Ok(())
    }
}

/// `summary.jsonl`'s Maximum IP-Layer Capacity: the Client's
/// `Output.AtMax.MaxIPLayerCapacity`, or `null` where its output isn't
/// JSON; the Round stops where it is JSON but not objects down to there,
/// as the Python's `.get` stops it.
fn maximum(label: &str, output: &str) -> Result<Json, Stopped> {
    let Ok(parsed) = Json::parse(output) else {
        // What Python refuses too, and what Python reads but no Client
        // writes (the module's note).
        return Ok(Json::Null);
    };
    let empty = &*py::EMPTY_DICT;
    let found = parsed
        .get_or("Output", empty)
        .and_then(|output| output.get_or("AtMax", empty))
        .and_then(|at_max| at_max.get("MaxIPLayerCapacity"));
    match found {
        Ok(value) => Ok(value.clone()),
        Err(e) => Err(Stopped::Failed(format!(
            "{label}: its result can't be read: {e}"
        ))),
    }
}

impl<'a> RoundBody for FirstRound<'a> {
    fn run(&mut self, provider: &mut dyn Provider, start: &Start<'_>) -> Result<(), Stopped> {
        self.save_plan()?;
        let key = make_keys(start.keys)?;
        let runner = self.runner.clone();
        let made = self.ssh_key;
        let access = round::access(
            provider,
            start.keys,
            self.options.ssh_dir.as_deref(),
            &mut |keys| match made {
                Some(make) => make(keys),
                None => secrets::ssh_key(keys, &runner),
            },
        )?;
        let roles: Vec<&str> = self.roles.iter().map(String::as_str).collect();
        let mut hosts: IndexMap<String, Box<dyn Shell + 'a>> = provider
            .create(start.round, start.expiry, &roles, &access)?
            .into_iter()
            .map(|(role, host)| (role, host as Box<dyn Shell + 'a>))
            .collect();
        if self.runs.iter().any(|r| r.client_host == ClientHost::Home) {
            let work = start.keys.path().join("home");
            let home: Box<dyn Shell + 'a> = match self.home {
                Some(make) => make(&work),
                None => {
                    let named = work
                        .to_str()
                        .and_then(|w| RemotePath::new(w).ok())
                        .ok_or_else(|| {
                            Stopped::Failed(format!(
                                "this machine's work directory {} can't be named in a script",
                                work.display()
                            ))
                        })?;
                    Box::new(Host::local("home", "", named, self.runner.clone()))
                }
            };
            hosts.insert("home".to_string(), home);
        }
        let listed: Vec<String> = hosts
            .iter()
            .map(|(role, host)| {
                format!(
                    "{}: ({}, {})",
                    py_repr(role),
                    py_repr(host.destination()),
                    py_repr(host.address())
                )
            })
            .collect();
        start.log.line(&format!("hosts: {{{}}}", listed.join(", ")));
        // Wall-clock time: a monotonic clock stops while this machine
        // sleeps, and the hosts' dead-man switches don't.
        let deadline = start.expiry - STOP_BEFORE_EXPIRY_S;
        let rented = provider.rented();
        self.steps(hosts, rented, &key, start.keys.path(), deadline, start.log)
    }

    fn stop_servers(&mut self, begun: &RoundTeardown, log: &Log) {
        let _ = begun;
        for (role, stem) in self.samplers.clone() {
            let stopped = self
                .stop_sampler(&role, &stem)
                .ok()
                .and_then(|c| c.as_dict().cloned())
                .is_some_and(|c| c.get("stopped") == Some(&Json::Bool(true)));
            if !stopped {
                log.line(&format!("{role}: sampler stop unconfirmed for {stem}"));
            }
        }
        for server in &self.servers {
            let _ = crate::cleanup::guarded(|| {
                let Some(host) = self.hosts.get(&server.role) else {
                    return;
                };
                // A Server whose PID the Round never got to read: read it now.
                let pid = match &server.pid {
                    Some(pid) => pid.clone(),
                    None => match host.run(&format!("cat {}.pid", server.stem), STOP_LIMIT) {
                        Ok(done) => py::strip(&done.stdout).to_string(),
                        Err(_) => return,
                    },
                };
                if !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()) {
                    // A host that doesn't answer is passed over.
                    let _ = host.run(&format!("kill {pid}"), STOP_LIMIT);
                }
            });
        }
        crate::cleanup::final_sweep(&self.hosts, log);
    }

    fn made(&self) -> String {
        format!("{} Tests", self.tests)
    }
}

/// Python's `repr` of a string, for the log's list of hosts.
fn py_repr(s: &str) -> String {
    super::matrix::repr_str(s)
}
