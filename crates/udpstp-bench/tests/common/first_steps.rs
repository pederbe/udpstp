//! The first Round's orchestration against the independent reference: `tests/golden/round.json`
//! holds what `round.py`'s `Round` did on scripted hosts, which
//! `scripts/bench/tests/export_goldens.py` recorded.
//!
//! - **One Round's steps**, `first_round.steps`: prepare, the path check,
//!   the matrix and collect, then stopping the Servers, on hosts that
//!   answer each call from the case's script, each call of a small Round
//!   failing and interrupted in turn among the cases. The port, on hosts
//!   that replay the same answers, must make the same calls in the same
//!   order on each host (the script byte for byte, its time limit, whether
//!   its exit status is checked), sleep the same, write the same files,
//!   log the same lines and end the same way.
//!   The corrected cloud-pair counter script is matched against the exact
//!   opposite host address, then normalized to the original script's name.
//! - **The run loop**, `first_round.run_loop`: `Round.run_matrix` with each
//!   Test's verdict and time given, against [`Schedule`]: the same Round
//!   Runs, in order, and the same lines.
//!
//! The log's lines are read into the Python's words, a whole line at a
//! time (§7.5's translation table): the port writes the glossary's.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::common::step_signal::Interrupt;
use bench_rounds::counters;
use bench_rounds::dead_man;
use bench_rounds::host::{self, Completed, HostError, Limit, RemotePath, Shell, Stop};
use bench_rounds::interrupt::Interrupted;
use bench_rounds::num::Num;
use bench_rounds::process::Runner;
use bench_rounds::round::{Clock, Log, RoundBody, Stopped};
use bench_rounds::secrets::SecretDir;
use indexmap::IndexMap;
use serde_json::{Value, json};
use udpstp_bench::first_round::matrix::{self, ClientHost, RoundRun};
use udpstp_bench::first_round::round::{FirstRound, Obudpst, Options, make_keys, run_loop};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn golden() -> Vec<Value> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/round.json");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
    let golden: Value = serde_json::from_str(&text).unwrap();
    golden["cases"].as_array().expect("cases").clone()
}

/// A tagged dict as its pairs, by key.
fn dict(tagged: &Value) -> BTreeMap<String, Value> {
    tagged["dict"]
        .as_array()
        .unwrap_or_else(|| panic!("not a dict: {tagged}"))
        .iter()
        .map(|pair| (pair[0].as_str().unwrap().to_string(), pair[1].clone()))
        .collect()
}

/// A tagged dict's pairs, in order.
fn pairs(tagged: &Value) -> Vec<(String, Value)> {
    tagged["dict"]
        .as_array()
        .unwrap_or_else(|| panic!("not a dict: {tagged}"))
        .iter()
        .map(|pair| (pair[0].as_str().unwrap().to_string(), pair[1].clone()))
        .collect()
}

fn int(tagged: &Value) -> i128 {
    tagged["int"].as_str().unwrap().parse().unwrap()
}

fn float(tagged: &Value) -> f64 {
    f64::from_bits(u64::from_str_radix(tagged["float"].as_str().unwrap(), 16).unwrap())
}

fn tagged_limit(limit: Limit) -> Value {
    match limit.seconds() {
        Num::Int(i) => json!({"int": i.to_string()}),
        Num::Float(f) => json!({"float": format!("{:016x}", f.to_bits())}),
    }
}

/// The runs of the committed matrix at `path`, without the Tests from
/// home unless `home`.
fn runs_of(path: &str, home: bool) -> Vec<RoundRun> {
    let text = if path.contains('\n') {
        path.to_string()
    } else {
        std::fs::read_to_string(root().join(path)).unwrap()
    };
    let runs = matrix::parse(&text).unwrap();
    runs.into_iter()
        .filter(|r| home || r.client_host != ClientHost::Home)
        .collect()
}

/// The Python's patched `time`: it moves on `step` seconds at each host
/// call, and its sleeps are only recorded.
struct FakeClock {
    now: Mutex<f64>,
    step: f64,
    slept: Mutex<Vec<u64>>,
    interrupt: Interrupt,
}

impl FakeClock {
    fn tick(&self) {
        *self.now.lock().unwrap() += self.step;
    }
}

impl Clock for FakeClock {
    fn now(&self) -> f64 {
        *self.now.lock().unwrap()
    }

    fn sleep(&self, duration: Duration) -> Result<(), Interrupted> {
        if self.interrupt.flag.stops() {
            return Err(Interrupted);
        }
        self.slept.lock().unwrap().push(duration.as_secs());
        Ok(())
    }

    fn pause(&self, duration: Duration) {
        self.slept.lock().unwrap().push(duration.as_secs());
    }
}

/// The name a transcript gives a long script: `scripts.json` and
/// `dead_man.json` hold their texts.
fn named(script: &str) -> &str {
    if script == counters::SCRIPT {
        "<counters script>"
    } else if script == host::ENVIRONMENT_SCRIPT {
        "<environment script>"
    } else if script == host::QUIET_SCRIPT {
        "<quiet script>"
    } else if script == dead_man::CHECK_SCRIPT {
        "<dead-man check>"
    } else {
        script
    }
}

/// A host that answers each call as the Python's host was answered at the
/// same place, and records the calls it was given.
struct Replay<'a> {
    role: String,
    destination: String,
    address: String,
    public_address: Option<String>,
    counter_peer: Option<String>,
    answers: Vec<Value>,
    made: Arc<Mutex<Vec<Value>>>,
    clock: &'a FakeClock,
}

impl Replay<'_> {
    fn answer(&self, call: Value) -> Value {
        self.clock.tick();
        let mut made = self.made.lock().unwrap();
        let answer = self.answers.get(made.len()).cloned();
        made.push(call);
        // A call the Python didn't make: the transcripts differ, which the
        // test says; the host fails it meanwhile.
        answer.unwrap_or_else(|| json!(["fail", "a call the Python didn't make"]))
    }

    fn call(&self, script: &str, limit: Limit, check: bool) -> Result<Completed, Stop> {
        // Normalize the intentional script delta, including timeout text.
        let script = if self
            .counter_peer
            .as_deref()
            .is_some_and(|p| script == counters::towards(p))
        {
            counters::SCRIPT
        } else {
            script
        };
        let answer = self.answer(json!(["run", named(script), tagged_limit(limit), check]));
        match answer[0].as_str().unwrap() {
            "timeout" => Err(HostError::timed_out(&self.role, script, limit).into()),
            "interrupt" => {
                self.clock.interrupt.raise();
                Err(Stop::Interrupted)
            }
            "fail" => Err(HostError::new(answer[1].as_str().unwrap()).into()),
            _ => {
                let status = i32::try_from(int(&answer[1])).unwrap();
                let done = Completed {
                    returncode: status,
                    stdout: answer[2].as_str().unwrap().to_string(),
                    stderr: answer[3].as_str().unwrap().to_string(),
                };
                if check && !done.success() {
                    return Err(HostError::exited(&self.role, script, status, &done.stderr).into());
                }
                Ok(done)
            }
        }
    }
}

impl Shell for Replay<'_> {
    fn work(&self) -> &RemotePath {
        static WORK: std::sync::OnceLock<RemotePath> = std::sync::OnceLock::new();

        WORK.get_or_init(|| RemotePath::new("/work").unwrap())
    }
    fn role(&self) -> &str {
        &self.role
    }

    fn address(&self) -> &str {
        &self.address
    }

    fn public_address(&self) -> Option<&str> {
        self.public_address.as_deref()
    }

    fn destination(&self) -> &str {
        &self.destination
    }

    fn run(&self, script: &str, limit: Limit) -> Result<Completed, Stop> {
        self.call(script, limit, false)
    }

    fn run_checked(&self, script: &str, limit: Limit) -> Result<Completed, Stop> {
        self.call(script, limit, true)
    }

    fn put(&self, local: &Path, name: &RemotePath, mode: u32) -> Result<(), Stop> {
        let file = local.file_name().unwrap().to_str().unwrap();
        let answer = self.answer(json!(["put", file, name.as_str(), format!("{mode:04o}")]));
        match answer[0].as_str().unwrap() {
            "fail" => Err(HostError::new(answer[1].as_str().unwrap()).into()),
            "interrupt" => {
                self.clock.interrupt.raise();
                Err(Stop::Interrupted)
            }
            _ => Ok(()),
        }
    }

    fn output(&self, _script: &str, _limit: Limit) -> Result<Vec<u8>, Stop> {
        panic!("the first Round's steps fetch nothing")
    }
}

/// The port's log line in the Python's words, a whole line at a time.
fn in_the_pythons_words(line: &str) -> String {
    match line {
        "three Tests in a row failed: the Round ends" => {
            "three Tests in a row failed: the round ends".to_string()
        }
        _ => line.to_string(),
    }
}

/// What a directory holds, by its path relative to `dir` with `/`.
fn files_in(dir: &Path) -> BTreeMap<String, String> {
    let mut found = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(at) = stack.pop() {
        for entry in std::fs::read_dir(&at).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let name = path
                    .strip_prefix(dir)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .replace('\\', "/");
                found.insert(name, std::fs::read_to_string(&path).unwrap());
            }
        }
    }
    found
}

fn scratch(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("udpstp-bench-round-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// One case of `first_round.steps`, run by the port: what differs from the
/// Python, if anything.
pub fn steps(n: usize, case: &Value, python: &Value, interrupt: Interrupt) -> Vec<String> {
    let args = dict(case);
    let options = dict(&args["options"]);
    let python = dict(python);
    let path = |v: &Value| v.as_str().map(PathBuf::from);
    let out = scratch(&format!("out-{n}"));
    std::fs::create_dir_all(out.join("tests")).unwrap();
    let keys_parent = scratch(&format!("keys-{n}"));
    let keys = SecretDir::create_in(&keys_parent).unwrap();
    let key = make_keys(&keys).unwrap();
    let options = Options {
        measurement_tool: None,
        udpstp: path(&options["udpstp"]).unwrap(),
        udpstp_home: path(&options["udpstp_home"]),
        obudpst: match options["obudpst"].as_str() {
            None => None,
            Some("build") => Some(Obudpst::Build),
            Some(binary) => Some(Obudpst::Binary(binary.into())),
        },
        raise_buffers: options["raise_buffers"].as_bool().unwrap(),
        udpstp_port: int(&options["udpstp_port"]) as u16,
        obudpst_port: int(&options["obudpst_port"]) as u16,
        home_cap_mbps: int(&options["home_cap_mbps"]) as i64,
        out: out.clone(),
        ssh_dir: None,
        obudpst_script: udpstp_bench::obudpst::script(),
    };
    let flag = interrupt.flag.clone();
    let clock = FakeClock {
        now: Mutex::new(1_790_000_000.0),
        step: float(&args["step"]),
        slept: Mutex::new(Vec::new()),
        interrupt,
    };
    let runs = runs_of(
        args["matrix"].as_str().unwrap(),
        args["home_client"].as_bool().unwrap(),
    );
    let calls = dict(&python["calls"]);
    let mut replays: Vec<Replay> = Vec::new();
    for spec in args["hosts"].as_array().unwrap() {
        let role = spec[0].as_str().unwrap();
        replays.push(Replay {
            role: role.to_string(),
            destination: spec[1].as_str().unwrap().to_string(),
            address: spec[2].as_str().unwrap().to_string(),
            public_address: spec[3].as_str().map(str::to_string),
            counter_peer: args["hosts"]
                .as_array()
                .unwrap()
                .iter()
                .find(|s| s[0] == if role == "server" { "client" } else { "server" })
                .map(|s| s[2].as_str().unwrap().to_string()),
            answers: calls[role]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c[1].clone())
                .collect(),
            made: Arc::new(Mutex::new(Vec::new())),
            clock: &clock,
        });
    }
    let made: Vec<(String, Arc<Mutex<Vec<Value>>>)> = replays
        .iter()
        .map(|r| (r.role.clone(), r.made.clone()))
        .collect();
    let hosts: IndexMap<String, Box<dyn Shell + '_>> = replays
        .into_iter()
        .map(|r| (r.role.clone(), Box::new(r) as Box<dyn Shell>))
        .collect();
    let log = Log::kept();
    let rented = args["provider"].as_str() == Some("aws");
    let mut body = FirstRound::new(options, runs, vec![], &clock, Runner::new(flag.clone()));
    let ended = body.steps(
        hosts,
        rented,
        &key,
        keys.path(),
        float(&args["deadline"]),
        &log,
    );
    let stopping: BTreeMap<String, usize> = made
        .iter()
        .map(|(role, made)| (role.clone(), made.lock().unwrap().len()))
        .collect();
    body.stop_servers(&flag.stop_acting(), &log);

    let mut differ = Vec::new();
    let expect = |differ: &mut Vec<String>, what: &str, python: Value, port: Value| {
        if python != port {
            differ.push(format!(
                "case {n}: {what}: the Python {python}, the port {port}"
            ));
        }
    };
    let outcome = python["outcome"].as_array().unwrap();
    let port_outcome = match (&ended, outcome[0].as_str().unwrap()) {
        (Ok(()), _) => json!(["ok"]),
        (Err(Stopped::Interrupted), _) => json!(["interrupted"]),
        // A traceback in the Python; the port stops the Round with words.
        (Err(Stopped::Failed(_)), "raises") => Value::Array(outcome.clone()),
        (Err(Stopped::Failed(why)), _) => {
            json!(["stopped", why.replace(&out.display().to_string(), "<out>")])
        }
    };
    expect(
        &mut differ,
        "the end",
        Value::Array(outcome.clone()),
        port_outcome,
    );
    expect(
        &mut differ,
        "the Tests made",
        python["tests"].clone(),
        json!({"int": body_tests(&body).to_string()}),
    );
    for (role, made) in &made {
        let made = Value::Array(made.lock().unwrap().clone());
        let python_calls: Vec<Value> = calls[role]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c[0].clone())
            .collect();
        let python_calls = Value::Array(python_calls);
        if made != python_calls {
            let (m, p) = (made.as_array().unwrap(), python_calls.as_array().unwrap());
            let at = m
                .iter()
                .zip(p)
                .position(|(a, b)| a != b)
                .unwrap_or(m.len().min(p.len()));
            differ.push(format!(
                "case {n}: {}'s calls differ at {at} of {} (the Python's {}): the Python {:?}, the port {:?}",
                role,
                m.len(),
                p.len(),
                p.get(at),
                m.get(at)
            ));
        }
        expect(
            &mut differ,
            &format!("{role}'s Round Teardown begins at"),
            dict(&python["stopping"])[role].clone(),
            json!({"int": stopping[role].to_string()}),
        );
    }
    let slept: Vec<Value> = clock
        .slept
        .lock()
        .unwrap()
        .iter()
        .map(|s| json!({"int": s.to_string()}))
        .collect();
    expect(
        &mut differ,
        "the sleeps",
        python["slept"].clone(),
        Value::Array(slept),
    );
    let logged: Vec<Value> = log
        .lines()
        .iter()
        .map(|l| Value::String(in_the_pythons_words(l)))
        .collect();
    expect(
        &mut differ,
        "the log",
        python["log"].clone(),
        Value::Array(logged),
    );
    let python_files: BTreeMap<String, String> = pairs(&python["files"])
        .into_iter()
        .map(|(k, v)| (k, v.as_str().unwrap().to_string()))
        .collect();
    let port_files = files_in(&out);
    if python_files != port_files {
        let names: Vec<&String> = python_files.keys().chain(port_files.keys()).collect();
        let first = names
            .into_iter()
            .find(|name| python_files.get(*name) != port_files.get(*name))
            .unwrap();
        differ.push(format!(
            "case {n}: the file {first}: the Python {:?}, the port {:?}",
            python_files.get(first),
            port_files.get(first)
        ));
    }
    drop(keys);
    let _ = std::fs::remove_dir_all(&out);
    let _ = std::fs::remove_dir_all(&keys_parent);
    differ
}

fn body_tests(body: &FirstRound) -> usize {
    body.tests()
}

pub fn the_first_rounds_steps_are_the_pythons() {
    let cases = golden();
    let n = cases
        .iter()
        .filter(|case| case["fn"] == "first_round.steps")
        .count();
    assert!(n > 100, "only {n} cases");
    crate::common::for_each_case(&cases, |i, case| {
        if case["fn"] != "first_round.steps" {
            return;
        }
        let python = &case["python"]["ok"];
        assert!(!python.is_null(), "case {i}: the Python's harness failed");
        let differ = steps(i, &case["args"][0], python, Interrupt::default());
        assert!(differ.is_empty(), "{}", differ.join("\n"));
    });
}

pub fn the_run_loop_is_the_pythons() {
    let cases = golden();
    let n = cases
        .iter()
        .filter(|case| case["fn"] == "first_round.run_loop")
        .count();
    assert!(n >= 300, "only {n} cases");
    crate::common::for_each_case(&cases, |i, case| {
        if case["fn"] != "first_round.run_loop" {
            return;
        }
        let args = case["args"].as_array().unwrap();
        let runs = runs_of(args[0].as_str().unwrap(), args[1].as_bool().unwrap());
        let passes: Vec<bool> = args[2]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p.as_bool().unwrap())
            .collect();
        let seconds: Vec<f64> = args[3].as_array().unwrap().iter().map(float).collect();
        let (now, deadline) = (std::cell::Cell::new(float(&args[4])), float(&args[5]));
        let (mut ran, mut logged) = (Vec::new(), Vec::new());
        // The body's own loop, each Round Run's verdict and time given.
        run_loop(
            &runs,
            deadline,
            &mut || now.get(),
            &mut |line| logged.push(in_the_pythons_words(line)),
            &mut |run| {
                let made = ran.len();
                ran.push(run.label());
                now.set(now.get() + seconds[made % seconds.len()]);
                Ok(passes[made % passes.len()])
            },
        )
        .unwrap();
        let python = dict(&case["python"]["ok"]);
        assert_eq!(json!(ran), python["ran"], "case {i}: the Round Runs");
        assert_eq!(json!(logged), python["log"], "case {i}: the log");
    });
}

/// The new cap uses the same external host seam, without changing reference goldens.
struct CapHost<'a> {
    role: &'static str,
    calls: Arc<Mutex<Vec<String>>>,
    interrupt: &'a Interrupt,
    fault: Option<(&'static str, bool)>,
    fired: Arc<Mutex<bool>>,
}

// ethtool(8)'s five read-only setting groups, as a host returns them.
// First-Round diagnostics in docs/testing.md retain this text without judging it.
const NIC_SETTINGS: &str = "\
nic_settings_begin: udp4_rx_flow_hash
nic_settings_command: ethtool -n private0 rx-flow-hash udp4
nic_settings_status: 0
  nic_settings: IP SA
  nic_settings: IP DA
  nic_settings: L4 bytes 0 & 1 [TCP/UDP src port]
  nic_settings: L4 bytes 2 & 3 [TCP/UDP dst port]
nic_settings_end: udp4_rx_flow_hash
nic_settings_begin: rss
nic_settings_command: ethtool -x private0
nic_settings_status: 0
  nic_settings: RX flow hash indirection table for private0 with 2 RX ring(s):
  nic_settings:     0: 0 1
  nic_settings: RSS hash key:
  nic_settings: 01:23:45:67
  nic_settings: RSS hash function:
  nic_settings:     toeplitz: on
nic_settings_end: rss
nic_settings_begin: channels
nic_settings_command: ethtool -l private0
nic_settings_status: 0
  nic_settings: Current hardware settings:
  nic_settings: Combined: 2
nic_settings_end: channels
nic_settings_begin: rings
nic_settings_command: ethtool -g private0
nic_settings_unavailable: rings
nic_settings_status: 95
  nic_settings: Cannot get device ring settings: Operation not supported
nic_settings_end: rings
nic_settings_begin: coalescing
nic_settings_command: ethtool -c private0
nic_settings_status: 0
  nic_settings: rx-usecs: 1
nic_settings_end: coalescing
";

impl Shell for CapHost<'_> {
    fn role(&self) -> &str {
        self.role
    }
    fn address(&self) -> &str {
        if self.role == "server" {
            "10.0.0.1"
        } else {
            "10.0.0.2"
        }
    }
    fn public_address(&self) -> Option<&str> {
        None
    }
    fn destination(&self) -> &str {
        self.role
    }
    fn work(&self) -> &RemotePath {
        static WORK: std::sync::OnceLock<RemotePath> = std::sync::OnceLock::new();
        WORK.get_or_init(|| RemotePath::new("/work").unwrap())
    }
    fn run(&self, script: &str, limit: Limit) -> Result<Completed, Stop> {
        let capped = {
            let mut calls = self.calls.lock().unwrap();
            let capped = calls
                .iter()
                .any(|c| c.contains("setsid -f") && c.contains("server-capped-1.pid"));
            calls.push(script.into());
            capped
        };
        if let Some((at, interrupted)) = self.fault {
            let matches = match at {
                "launch" => script.contains("setsid -f") && script.contains("server-capped-1.pid"),
                "pid" => script == "cat server-capped-1.pid",
                "alive" => script == "kill -0 42 && echo alive",
                "client" => {
                    capped
                        && script.contains("./udpstp client")
                        && script.contains("--test-connections 4")
                }
                "sampler-start" => script.contains("first-capture start vm-1 "),
                "sampler-stop" => script.contains("first-capture stop vm-1"),
                "stop" => {
                    capped && script.starts_with("kill 42 ") && script.contains("echo stopped")
                }
                "log" => script == "cat server-capped-1.log",
                "restore" => {
                    capped && script.contains("setsid -f") && script.contains("server-udpstp.pid")
                }
                _ => panic!("unknown fault {at}"),
            };
            let mut fired = self.fired.lock().unwrap();
            if matches && !*fired {
                *fired = true;
                if interrupted {
                    self.interrupt.raise();
                    return Err(Stop::Interrupted);
                }
                return Err(HostError::new(format!("scripted {at} failure")).into());
            }
        }
        if script.starts_with("counter_peer=") {
            let peer = if self.role == "server" {
                "10.0.0.2"
            } else {
                "10.0.0.1"
            };
            assert_eq!(script, counters::towards(peer));
            assert_eq!(limit, Limit::secs(30));
            return Ok(Completed {
                returncode: 0,
                stdout: format!("counter_peer: {peer}\n{NIC_SETTINGS}"),
                stderr: String::new(),
            });
        }
        let stdout = if script.starts_with("cat ") && script.ends_with(".pid") {
            if script.contains("server-udpstp") {
                "41\n"
            } else if script.contains("server-obudpst") {
                "40\n"
            } else {
                "42\n"
            }
        } else if script.contains("echo stopped") {
            "stopped\n"
        } else if script.contains("echo alive") {
            "alive\n"
        } else if script.contains("first-capture stop") {
            r#"{"stopped":true,"status":0}"#
        } else if script.starts_with("cat server-") && script.ends_with(".log") {
            "{\"event\":\"data_phase_engines\",\"executor\":\"pool\",\"engine_threads\":2,\"grouping_us\":0}\n"
        } else if script.contains("./udpstp client") || script.contains("./udpst -d") {
            "{}"
        } else {
            ""
        };
        Ok(Completed {
            returncode: 0,
            stdout: stdout.into(),
            stderr: String::new(),
        })
    }
    fn put(&self, _: &Path, _: &RemotePath, _: u32) -> Result<(), Stop> {
        Ok(())
    }
    fn output(&self, _: &str, _: Limit) -> Result<Vec<u8>, Stop> {
        unreachable!()
    }
}

fn cap_round(
    host_samples: bool,
    fault: Option<(&'static str, bool)>,
    interrupt: Interrupt,
) -> (SecretDir, Vec<RoundRun>, Vec<String>, Result<(), Stopped>) {
    let work = SecretDir::create().unwrap();
    let out = work.path().join("out");
    std::fs::create_dir_all(out.join("tests")).unwrap();
    let keys = SecretDir::create_in(work.path()).unwrap();
    let key = make_keys(&keys).unwrap();
    let text = format!(
        r#"
[defaults]
jumbo = false
host_samples = {host_samples}
[[test]]
name = "capped multi"
directions = ["downstream"]
test_connections = 4
server_engine_threads = 2
pairs = ["udpstp/udpstp", "obudpst/obudpst"]
[[test]]
name = "capped precision"
directions = ["downstream"]
server_engine_threads = 1
kernel_send_timestamps = true
[[test]]
name = "capped receive"
directions = ["upstream"]
server_engine_threads = 3
[[test]]
name = "ordinary"
directions = ["downstream"]
"#
    );
    let runs = runs_of(&text, false);
    let options = Options {
        measurement_tool: None,
        udpstp: "/build/udpstp".into(),
        udpstp_home: None,
        obudpst: Some(Obudpst::Binary("/build/udpst".into())),
        raise_buffers: false,
        udpstp_port: 24601,
        obudpst_port: 25000,
        home_cap_mbps: 250,
        out: out.clone(),
        ssh_dir: None,
        obudpst_script: udpstp_bench::obudpst::script(),
    };
    let clock = FakeClock {
        now: Mutex::new(100.0),
        step: 0.0,
        slept: Mutex::new(Vec::new()),
        interrupt,
    };
    let calls = Arc::new(Mutex::new(Vec::new()));
    let fired = Arc::new(Mutex::new(false));
    let hosts = ["server", "client"]
        .into_iter()
        .map(|role| {
            (
                role.into(),
                Box::new(CapHost {
                    role,
                    calls: calls.clone(),
                    interrupt: &clock.interrupt,
                    fault,
                    fired: fired.clone(),
                }) as Box<dyn Shell>,
            )
        })
        .collect();
    let log = Log::kept();
    let mut body = FirstRound::new(
        options,
        runs.clone(),
        vec![],
        &clock,
        Runner::new(clock.interrupt.flag.clone()),
    );
    let ended = body.steps(hosts, false, &key, keys.path(), 10_000.0, &log);
    body.stop_servers(&clock.interrupt.flag.stop_acting(), &log);
    if fault.is_some() {
        assert!(*fired.lock().unwrap(), "fault not reached: {fault:?}");
    }
    let calls = calls.lock().unwrap().clone();
    (work, runs, calls, ended)
}

pub fn counter_snapshots_follow_the_private_test_peers() {
    let (work, runs, calls, ended) = cap_round(false, None, Interrupt::default());
    ended.unwrap();
    let out = work.path().join("out");
    let files = udpstp_bench::first_round::report::OutDir::new(&out).unwrap();
    let with_settings = udpstp_bench::first_round::report::report(&files).unwrap();
    // Each cloud host, before and after the two path checks and every Test.
    for (role, peer) in [("server", "10.0.0.2"), ("client", "10.0.0.1")] {
        let script = counters::towards(peer);
        assert_eq!(
            calls.iter().filter(|c| **c == script).count(),
            (runs.len() + 2) * 2
        );
        for run in &runs {
            for when in ["before", "after"] {
                let path = out.join(format!("tests/{}.{role}.{when}.txt", run.label()));
                assert_eq!(
                    std::fs::read_to_string(&path).unwrap(),
                    format!("counter_peer: {peer}\n{NIC_SETTINGS}")
                );
                std::fs::write(path, format!("counter_peer: {peer}\n")).unwrap();
            }
        }
    }
    assert!(!calls.iter().any(|c| c == counters::SCRIPT));
    assert_eq!(
        with_settings,
        udpstp_bench::first_round::report::report(&files).unwrap(),
        "retaining receive settings must not change report judgments"
    );
}

pub fn capped_servers_are_isolated_recorded_and_restored() {
    let (work, runs, calls, ended) = cap_round(true, None, Interrupt::default());
    ended.unwrap();
    let out = work.path().join("out");
    let servers: Vec<_> = calls
        .iter()
        .filter(|c| c.contains("setsid -f") && c.contains("./udpstp server"))
        .collect();
    assert_eq!(
        servers.len(),
        7,
        "baseline plus three isolated starts and restores: {servers:?}"
    );
    for (i, cap, stamps) in [(1, 2, 0), (3, 1, 1), (5, 3, 0)] {
        assert!(
            servers[i].contains(&format!("UDPSTP_ENGINE_THREADS={cap}")),
            "{}",
            servers[i]
        );
        assert!(servers[i].contains(&format!("UDPSTP_KERNEL_TX_TIMESTAMPS={stamps}")));
        assert!(calls.iter().any(|c| c.contains("first-capture start")
            && c.contains(if i == 3 {
                "server-precision-3.log"
            } else if i == 1 {
                "server-capped-1.log"
            } else {
                "server-capped-4.log"
            })));
    }
    for i in [0, 2, 4, 6] {
        assert!(!servers[i].contains("UDPSTP_ENGINE_THREADS="));
    }
    let summary: Vec<Value> = std::fs::read_to_string(out.join("summary.jsonl"))
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .filter(|r: &Value| r["repeat"] == 1)
        .collect();
    let plan: Value =
        serde_json::from_slice(&std::fs::read(out.join("first-plan.json")).unwrap()).unwrap();
    assert_eq!(
        plan["version"], 2,
        "old reporters must refuse capped schedules"
    );
    for (i, cap) in [Some(2), None, Some(1), Some(3), None]
        .into_iter()
        .enumerate()
    {
        assert_eq!(
            summary[i].get("server_engine_threads").cloned(),
            cap.map(|n| json!(n))
        );
        assert_eq!(
            plan["runs"][i].get("server_engine_threads").cloned(),
            cap.map(|n| json!(n))
        );
        let capture = out.join(format!("tests/{}.server.json", runs[i].label()));
        assert_eq!(capture.exists(), cap.is_some());
        if cap.is_some() {
            let saved: Value = serde_json::from_slice(&std::fs::read(capture).unwrap()).unwrap();
            assert!(
                saved["log"]
                    .as_str()
                    .unwrap()
                    .contains("data_phase_engines")
            );
        }
    }
}

pub fn capped_server_cleanup_survives_host_failures_and_interruptions() {
    for at in [
        "launch",
        "pid",
        "alive",
        "client",
        "sampler-start",
        "sampler-stop",
        "stop",
        "log",
        "restore",
    ] {
        for interrupted in [true, false] {
            let (work, _, calls, ended) =
                cap_round(true, Some((at, interrupted)), Interrupt::default());
            if interrupted {
                assert!(
                    matches!(ended, Err(Stopped::Interrupted)),
                    "{at}: {ended:?}"
                );
            } else if ["client", "sampler-start", "log"].contains(&at) {
                // These existing optional host-result failures remain recoverable.
                assert!(ended.is_ok(), "{at}: {ended:?}");
            } else {
                assert!(ended.is_err(), "{at}: {ended:?}");
            }
            assert!(
                calls
                    .iter()
                    .any(|c| c == udpstp_bench::scale::hosts::STRAYS_SCRIPT),
                "{at}: no final sweep"
            );
            if at != "launch" {
                assert!(
                    calls
                        .iter()
                        .any(|c| c == "kill 42" || c.starts_with("kill 42 2>/dev/null")),
                    "{at}: isolated PID not stopped"
                );
            }
            if matches!(at, "stop" | "restore") {
                let summary =
                    std::fs::read_to_string(work.path().join("out/summary.jsonl")).unwrap();
                assert!(
                    summary.contains("capped multi"),
                    "Client result must survive {at}"
                );
                assert!(
                    !summary.contains("capped precision"),
                    "another Test after {at}"
                );
            }
        }
    }
    let (work, _, _, ended) = cap_round(false, None, Interrupt::default());
    ended.unwrap();
    let out = work.path().join("out");
    assert!(
        out.join("first-plan.json").exists(),
        "explicit caps retain a schedule without sampling"
    );
    let report = udpstp_bench::first_round::report::report(
        &udpstp_bench::first_round::report::OutDir::new(&out).unwrap(),
    )
    .unwrap();
    assert!(report.contains("Server Engine Thread caps"));
    assert!(report.contains("| 2 | 2 | unavailable |"), "{report}");
}

#[cfg(target_os = "linux")]
pub fn capped_server_interrupted(at: &str, interrupt: Interrupt) {
    let at = [
        "launch",
        "pid",
        "alive",
        "client",
        "sampler-start",
        "sampler-stop",
        "stop",
        "log",
        "restore",
    ]
    .into_iter()
    .find(|name| *name == at)
    .expect("known cap boundary");
    let (_, _, calls, ended) = cap_round(true, Some((at, true)), interrupt);
    assert!(
        matches!(ended, Err(Stopped::Interrupted)),
        "{at}: {ended:?}"
    );
    assert!(
        calls
            .iter()
            .any(|c| c == udpstp_bench::scale::hosts::STRAYS_SCRIPT),
        "{at}: no final sweep"
    );
}
