//! A whole first Round against the independent reference: `tests/golden/execute.json` holds
//! what `round.py`'s `execute` did around its `Round` on a scripted
//! provider and scripted hosts, this machine's among them, which
//! `scripts/bench/tests/export_goldens.py` recorded: rented and static
//! Rounds, the deadline ten minutes before the hosts expire, a dead-man
//! switch that isn't armed, a Round Teardown that leaves something or
//! fails, a provider that can't create, and each call of a rented Round
//! with a Test from home failing, then interrupted, in turn.
//!
//! `bench_rounds::round::execute` around the port's body, on hosts that
//! replay the same answers, must make the same calls on each host, ask the
//! provider the same, write the same files and the same ledger, byte for
//! byte, log and say the same lines (read into the Python's words a whole
//! line at a time) and end with the same exit status; where the Python
//! ends with a traceback, with 1. The Round's key files must be the
//! Python's but for the key.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bench_rounds::counters;
use bench_rounds::dead_man;
use bench_rounds::host::{self, Completed, HostError, Limit, RemotePath, Shell, Stop};
use bench_rounds::interrupt::{Flag, Interrupted};
use bench_rounds::num::Num;
use bench_rounds::process::Runner;
use bench_rounds::provider::{Access, Leftover, Provider, ProviderError, RoundId, UnixTime};
use bench_rounds::round::Stopped;
use bench_rounds::round::{Bracket, Clock, Log, execute};
use bench_rounds::secrets::{SecretDir, SshKey};
use indexmap::IndexMap;
use serde_json::{Value, json};
use udpstp_bench::first_round::matrix::{self, ClientHost, RoundRun};
use udpstp_bench::first_round::round::{FirstRound, Obudpst, Options, make_keys};

const START: f64 = 1_790_000_000.0;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn dict(tagged: &Value) -> BTreeMap<String, Value> {
    tagged["dict"]
        .as_array()
        .unwrap_or_else(|| panic!("not a dict: {tagged}"))
        .iter()
        .map(|pair| (pair[0].as_str().unwrap().to_string(), pair[1].clone()))
        .collect()
}

fn pairs(tagged: &Value) -> Vec<(String, Value)> {
    tagged["dict"]
        .as_array()
        .unwrap()
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

fn tagged_float(f: f64) -> Value {
    json!({"float": format!("{:016x}", f.to_bits())})
}

fn tagged_limit(limit: Limit) -> Value {
    match limit.seconds() {
        Num::Int(i) => json!({"int": i.to_string()}),
        Num::Float(f) => tagged_float(f),
    }
}

/// The Python's patched `time`: it moves on `step` seconds at each host
/// call; waits are passed over.
struct FakeClock {
    now: Mutex<f64>,
    step: f64,
    flag: Flag,
}

impl FakeClock {
    fn tick(&self) {
        *self.now.lock().unwrap() += self.step;
    }
}

impl Clock for FakeClock {
    fn now(&self) -> UnixTime {
        *self.now.lock().unwrap()
    }

    fn sleep(&self, _: Duration) -> Result<(), Interrupted> {
        if self.flag.stops() {
            return Err(Interrupted);
        }
        Ok(())
    }

    fn pause(&self, _: Duration) {}
}

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

/// A host that answers each call as the Python's was answered at the same
/// place, and records the calls it was given.
struct Replay {
    role: String,
    destination: String,
    address: String,
    public_address: Option<String>,
    counter_peer: Option<String>,
    answers: Vec<Value>,
    made: Arc<Mutex<Vec<Value>>>,
    clock: Arc<FakeClock>,
}

impl Replay {
    fn answer(&self, call: Value) -> Value {
        self.clock.tick();
        let mut made = self.made.lock().unwrap();
        let answer = self.answers.get(made.len()).cloned();
        made.push(call);
        answer.unwrap_or_else(|| json!(["fail", "a call the Python didn't make"]))
    }

    fn call(&self, script: &str, limit: Limit, check: bool) -> Result<Completed, Stop> {
        // Normalize the exact peer-path script and its timeout diagnostic.
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
                self.clock.flag.raise("SIGINT");
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

impl Shell for Replay {
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
                self.clock.flag.raise("SIGINT");
                Err(Stop::Interrupted)
            }
            _ => Ok(()),
        }
    }

    fn output(&self, _: &str, _: Limit) -> Result<Vec<u8>, Stop> {
        panic!("the first Round fetches nothing")
    }
}

/// Each host of a case: its spec, the answers the Python's got, and what
/// the port asks of it.
#[derive(Clone)]
struct HostCase {
    role: String,
    destination: String,
    address: String,
    public_address: Option<String>,
    counter_peer: Option<String>,
    answers: Vec<Value>,
    made: Arc<Mutex<Vec<Value>>>,
}

impl HostCase {
    fn replay(&self, clock: &Arc<FakeClock>) -> Replay {
        Replay {
            role: self.role.clone(),
            destination: self.destination.clone(),
            address: self.address.clone(),
            public_address: self.public_address.clone(),
            counter_peer: self.counter_peer.clone(),
            answers: self.answers.clone(),
            made: self.made.clone(),
            clock: clock.clone(),
        }
    }
}

/// The Python's `ScriptedProvider`.
struct Scripted {
    rented: bool,
    hosts: Vec<HostCase>,
    clock: Arc<FakeClock>,
    destroy: String,
    create_fails: bool,
    asked: Vec<Value>,
}

impl Provider for Scripted {
    fn name(&self) -> &'static str {
        if self.rented { "aws" } else { "static" }
    }

    fn rented(&self) -> bool {
        self.rented
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
        expiry: UnixTime,
        roles: &[&str],
        access: &Access,
    ) -> Result<IndexMap<String, Box<dyn Shell>>, ProviderError> {
        self.asked.push(json!([
            "create",
            round.as_str(),
            tagged_float(expiry),
            roles,
            access.public_key
        ]));
        if self.create_fails {
            return Err(ProviderError::Failed(
                "aws ec2 run-instances: the fake's failure for good".into(),
            ));
        }
        Ok(self
            .hosts
            .iter()
            .filter(|h| roles.contains(&h.role.as_str()))
            .map(|h| {
                (
                    h.role.clone(),
                    Box::new(h.replay(&self.clock)) as Box<dyn Shell>,
                )
            })
            .collect())
    }

    fn destroy(&mut self, round: &RoundId) -> Result<Vec<Leftover>, ProviderError> {
        self.asked.push(json!(["destroy", round.as_str()]));
        match self.destroy.as_str() {
            "fail" => Err(ProviderError::Failed("no answer".into())),
            "leave" => Ok(vec![Leftover::new("instance", "i-1")]),
            _ => Ok(Vec::new()),
        }
    }

    fn leftovers(&mut self, round: &RoundId) -> Result<Vec<Leftover>, ProviderError> {
        self.asked.push(json!(["leftovers", round.as_str()]));
        Ok(Vec::new())
    }

    fn recheck_after(&self) -> Option<Duration> {
        None
    }

    fn manual_cleanup(&self, round: &RoundId) -> Vec<String> {
        vec![format!("delete {round} by hand")]
    }
}

fn runs_of(matrix_arg: &str, home: bool) -> Vec<RoundRun> {
    let text = if matrix_arg.contains('\n') {
        matrix_arg.to_string()
    } else {
        std::fs::read_to_string(root().join(matrix_arg)).unwrap()
    };
    matrix::parse(&text)
        .unwrap()
        .into_iter()
        .filter(|r| home || r.client_host != ClientHost::Home)
        .collect()
}

fn fixed_key(keys: &SecretDir) -> Result<SshKey, Stopped> {
    Ok((
        keys.path().join("id_ed25519"),
        "ssh-ed25519 AAAAtest udpstp-bench\n".to_string(),
    ))
}

/// Leftovers as the Python's `repr` of its list of pairs.
fn listed(left: &str) -> String {
    let each: Vec<String> = left
        .split(", ")
        .map(|item| match item.strip_prefix("unknown (") {
            Some(e) => format!("('unknown', '{}')", e.strip_suffix(')').unwrap_or(e)),
            None => {
                let (kind, id) = item.split_once(' ').unwrap();
                format!("('{kind}', '{id}')")
            }
        })
        .collect();
    format!("[{}]", each.join(", "))
}

/// The port's line in the Python's words, a whole line at a time.
fn in_the_pythons_words(line: &str) -> String {
    if let Some(rest) = line.strip_prefix("\x1b[31mLEFT BEHIND, delete by hand: ")
        && let Some(left) = rest.strip_suffix("\x1b[0m")
    {
        return format!(
            "\x1b[31mLEFT BEHIND, delete by hand: {}\x1b[0m",
            listed(left)
        );
    }
    if let Some(rest) = line.strip_prefix("Round Teardown attempt ") {
        let (n, rest) = rest.split_once(' ').unwrap();
        if let Some(left) = rest.strip_prefix("left: ") {
            return format!("teardown attempt {n} left {}", listed(left));
        }
        if let Some(e) = rest.strip_prefix("failed: ") {
            return format!("teardown attempt {n} failed: {e}");
        }
    }
    if line == "the Round stopped: interrupted by SIGINT" {
        return "the round stopped: signal 2".to_string();
    }
    if let Some(why) = line.strip_prefix("the Round stopped: ") {
        return format!("the round stopped: {why}");
    }
    if line == "three Tests in a row failed: the Round ends" {
        return "three Tests in a row failed: the round ends".to_string();
    }
    if let Some(rest) = line.strip_prefix("Round r") {
        return format!("round r{rest}");
    }
    line.to_string()
}

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
    let dir = std::env::temp_dir().join(format!(
        "udpstp-bench-execute-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// One case run by the port: what differs from the Python.
fn case(n: usize, args: &BTreeMap<String, Value>, python: &BTreeMap<String, Value>) -> Vec<String> {
    let options = dict(&args["options"]);
    let dir = scratch(&n.to_string());
    let out = dir.join("out");
    std::fs::create_dir_all(out.join("tests")).unwrap();
    let ledger = dir.join("ledger.json");
    let flag = Flag::new();
    let clock = Arc::new(FakeClock {
        now: Mutex::new(START),
        step: float(&args["step"]),
        flag: flag.clone(),
    });
    let calls = pairs(&python["calls"]);
    // The answers the Python's hosts were given; none for a host it never
    // made.
    let answers_of = |role: &str| -> Vec<Value> {
        calls
            .iter()
            .find(|(r, _)| r == role)
            .map(|(_, c)| c.as_array().unwrap().iter().map(|c| c[1].clone()).collect())
            .unwrap_or_default()
    };
    let hosts: Vec<HostCase> = args["hosts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|spec| {
            let role = spec[0].as_str().unwrap();
            HostCase {
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
                answers: answers_of(role),
                made: Arc::default(),
            }
        })
        .collect();
    let rented = args["provider"].as_str() == Some("aws");
    let mut provider = Scripted {
        rented,
        hosts: hosts.iter().filter(|h| h.role != "home").cloned().collect(),
        clock: clock.clone(),
        destroy: args["destroy"].as_str().unwrap().to_string(),
        create_fails: args["create_fails"].as_bool().unwrap(),
        asked: Vec::new(),
    };
    let home = hosts.iter().find(|h| h.role == "home").cloned();
    let make_home = |_: &Path| -> Box<dyn Shell> {
        Box::new(home.as_ref().expect("a home host").replay(&clock))
    };
    let path = |v: &Value| v.as_str().map(PathBuf::from);
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
    let runs = runs_of(
        args["matrix"].as_str().unwrap(),
        args["home_client"].as_bool().unwrap(),
    );
    let roles: Vec<String> = provider.hosts.iter().map(|h| h.role.clone()).collect();
    let clock_ref: &FakeClock = &clock;
    let mut body = FirstRound::new(options, runs, roles, clock_ref, Runner::new(flag.clone()))
        .with_ssh_key(&fixed_key)
        .with_home(&make_home);
    let log = Log::kept();
    let mut said = Vec::new();
    let status = execute(
        &mut body,
        &mut provider,
        Bracket {
            ledger: &ledger,
            worst_case: 0.57,
            max_hours: float(&args["max_hours"]),
            round: RoundId::new(START, "abcd"),
            flag: &flag,
            clock: clock_ref,
            log: &log,
            keys_in: Some(&dir),
            alert: &mut |line: &str| said.push(line.to_string()),
        },
    );
    drop(body);

    let mut differ = Vec::new();
    let mut expected = Vec::new();
    let mut expect = |what: &str, python: Value, port: Value| {
        if python != port {
            expected.push(format!(
                "case {n}: {what}: the Python {python}, the port {port}"
            ));
        }
    };
    let ended = python["ended"].as_array().unwrap();
    let python_status = match ended[0].as_str().unwrap() {
        // A traceback, after the Python's Round Teardown.
        "raises" => 1,
        _ => int(&ended[1]),
    };
    expect(
        "the exit status",
        json!(python_status),
        json!(status.code()),
    );
    for host in &hosts {
        let python_calls: Vec<Value> = calls
            .iter()
            .find(|(r, _)| *r == host.role)
            .map(|(_, c)| c.as_array().unwrap().iter().map(|c| c[0].clone()).collect())
            .unwrap_or_default();
        let made = host.made.lock().unwrap().clone();
        if made != python_calls {
            let at = made
                .iter()
                .zip(&python_calls)
                .position(|(a, b)| a != b)
                .unwrap_or(made.len().min(python_calls.len()));
            differ.push(format!(
                "case {n}: {}'s calls differ at {at} of {} (the Python's {}): the Python {:?}, the port {:?}",
                host.role,
                made.len(),
                python_calls.len(),
                python_calls.get(at),
                made.get(at)
            ));
        }
    }
    expect(
        "what the provider was asked",
        python["asked"].clone(),
        Value::from(provider.asked.clone()),
    );
    let words = |lines: &[String]| -> Value {
        Value::from(
            lines
                .iter()
                .map(|l| in_the_pythons_words(l))
                .collect::<Vec<_>>(),
        )
    };
    // round.log holds a message of several lines as they are.
    let mut logged: Vec<String> = log
        .lines()
        .iter()
        .flat_map(|l| l.split('\n').map(str::to_string).collect::<Vec<_>>())
        // --out's path, which is this run's own.
        .map(|l| l.replace(&out.display().to_string(), "<out>"))
        .collect();
    if ended[0] == "raises" {
        // The Python ends with a traceback where the port logs why the
        // Round stopped (the design's §7.7).
        if let Some(at) = logged
            .iter()
            .position(|l| l.starts_with("the Round stopped: "))
        {
            logged.remove(at);
        }
    }
    expect("the log", python["log"].clone(), words(&logged));
    expect("what it said", python["stderr"].clone(), words(&said));
    expect(
        "the ledger",
        python["ledger"].clone(),
        Value::from(std::fs::read_to_string(&ledger).unwrap_or_default()),
    );
    let python_files: BTreeMap<String, String> = pairs(&python["files"])
        .into_iter()
        .map(|(k, v)| (k, v.as_str().unwrap().to_string()))
        .collect();
    let port_files = files_in(&out);
    if python_files != port_files {
        let first = python_files
            .keys()
            .chain(port_files.keys())
            .find(|k| python_files.get(*k) != port_files.get(*k))
            .unwrap()
            .clone();
        differ.push(format!(
            "case {n}: the file {first}: the Python {:?}, the port {:?}",
            python_files.get(&first),
            port_files.get(&first)
        ));
    }
    // The key directory is gone, and nothing else is left beside --out.
    let mut left: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_str().unwrap().to_string())
        .collect();
    left.sort();
    expect(
        "what is left beside --out",
        json!(["ledger.json", "out"]),
        json!(left),
    );
    let _ = std::fs::remove_dir_all(&dir);
    differ.extend(expected);
    differ
}

#[test]
fn a_whole_first_round_is_the_pythons() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/execute.json");
    let golden: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let cases = golden["cases"].as_array().unwrap();
    assert!(cases.len() > 100, "only {} cases", cases.len());
    common::for_each_case(cases, |n, c| {
        let python = dict(&c["python"]["ok"]);
        let differ = case(n, &dict(&c["args"][0]), &python);
        assert!(differ.is_empty(), "{}", differ.join("\n"));
    });
}

/// The Round's key files are the Python's, byte for byte, but for the
/// key, which is random.
#[test]
fn the_rounds_key_files_are_the_pythons() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/execute.json");
    let golden: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let python = dict(&dict(&golden["cases"][0]["python"]["ok"])["keys"]);
    let dir = scratch("keys");
    let keys = SecretDir::create_in(&dir).unwrap();
    let key = make_keys(&keys).unwrap();
    assert_eq!(key.id, 7);
    assert_eq!(key.text().len(), 32);
    for name in ["keys.toml", "udpst.keys"] {
        let text = std::fs::read_to_string(keys.path().join(name))
            .unwrap()
            .replace(key.text(), "<key>");
        assert_eq!(Value::from(text), python[name], "{name}");
    }
    drop(keys);
    std::fs::remove_dir_all(&dir).unwrap();
}
