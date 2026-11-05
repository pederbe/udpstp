//! OB-UDPST built on a host, against the Python's `round.build_obudpst`
//!: `tests/golden/
//! obudpst.json` holds what the Python did on a scripted host in each
//! scenario, each call it made and how it ended. [`obudpst::build`] on a
//! host scripted the same must make the same calls, byte for byte, and end
//! the same: built, the host's error in the same words (the Python's
//! refusal or a host's own), or interrupted.
//!
//! On Linux, an ignored test builds OB-UDPST for real on this machine as
//! a Round's local host, by hand:
//!
//! ```sh
//! cargo test -p udpstp-bench --test obudpst -- --ignored --nocapture
//! ```

use std::path::Path;
use std::sync::Mutex;

use bench_rounds::host::{Completed, HostError, Limit, RemotePath, Shell, Stop};
use bench_rounds::num::Num;
use serde_json::{Value, json};
use udpstp_bench::obudpst;

/// A host that records each call and answers it from the scenario's
/// script, as the exporter's `ScriptedHost` does.
struct Scripted {
    role: String,
    answers: Mutex<Vec<Value>>,
    calls: Mutex<Vec<Value>>,
}

impl Scripted {
    fn new(role: &str, answers: &[Value]) -> Scripted {
        Scripted {
            role: role.to_string(),
            answers: Mutex::new(answers.iter().rev().cloned().collect()),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn answer(&self) -> Value {
        self.answers
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| json!(["done", {"int": "0"}, "", ""]))
    }

    fn record(&self, call: Value) {
        self.calls.lock().unwrap().push(call);
    }

    fn call(&self, script: &str, limit: Limit, check: bool) -> Result<Completed, Stop> {
        let seconds = match limit.seconds() {
            Num::Int(i) => json!({"int": i.to_string()}),
            Num::Float(f) => json!({"float": format!("{:016x}", f.to_bits())}),
        };
        self.record(json!(["run", script, seconds, check]));
        let answer = self.answer();
        match answer[0].as_str().unwrap() {
            "timeout" => Err(HostError::timed_out(&self.role, script, limit).into()),
            "interrupt" => Err(Stop::Interrupted),
            _ => {
                let status: i32 = answer[1]["int"].as_str().unwrap().parse().unwrap();
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

impl Shell for Scripted {
    fn work(&self) -> &RemotePath {
        static WORK: std::sync::OnceLock<RemotePath> = std::sync::OnceLock::new();

        WORK.get_or_init(|| RemotePath::new("/work").unwrap())
    }
    fn role(&self) -> &str {
        &self.role
    }

    fn address(&self) -> &str {
        "10.0.0.2"
    }

    fn public_address(&self) -> Option<&str> {
        None
    }

    fn destination(&self) -> &str {
        "host"
    }

    fn run(&self, script: &str, limit: Limit) -> Result<Completed, Stop> {
        self.call(script, limit, false)
    }

    fn run_checked(&self, script: &str, limit: Limit) -> Result<Completed, Stop> {
        self.call(script, limit, true)
    }

    fn put(&self, local: &Path, name: &RemotePath, mode: u32) -> Result<(), Stop> {
        let same = std::fs::read(local).ok() == std::fs::read(obudpst::script()).ok();
        self.record(json!(["put", name.as_str(), format!("{mode:04o}"), same]));
        let answer = self.answer();
        match answer[0].as_str().unwrap() {
            "fail" => Err(HostError::new(answer[1].as_str().unwrap()).into()),
            "interrupt" => Err(Stop::Interrupted),
            _ => Ok(()),
        }
    }

    fn output(&self, _script: &str, _limit: Limit) -> Result<Vec<u8>, Stop> {
        panic!("building OB-UDPST reads no output")
    }
}

/// The Python's outcome as the port's: the refusal and the host's error
/// are both the host's error, in the same words.
fn outcome(result: &Result<(), Stop>) -> Value {
    match result {
        Ok(()) => json!({"ok": null}),
        Err(Stop::Host(e)) => json!({"host_error": e.message()}),
        Err(Stop::Interrupted) => json!({"interrupted": null}),
    }
}

/// A tagged value of the golden with its ints as the port writes them.
fn python_outcome(tagged: &Value) -> Value {
    let pairs = tagged["dict"].as_array().expect("an outcome");
    let (kind, value) = (&pairs[0][0], &pairs[0][1]);
    match kind.as_str().unwrap() {
        "refused" | "host_error" => json!({"host_error": value}),
        "interrupted" => json!({"interrupted": null}),
        "ok" => json!({"ok": null}),
        other => panic!("no outcome {other}"),
    }
}

#[test]
fn obudpst_is_built_as_the_python_builds_it() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/obudpst.json");
    let golden: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let cases = golden["cases"].as_array().expect("cases");
    assert!(!cases.is_empty());
    for (i, case) in cases.iter().enumerate() {
        let role = case["args"][0].as_str().unwrap();
        let answers = case["args"][1].as_array().unwrap();
        let python = &case["python"]["ok"]["dict"];
        let (calls, ended) = (&python[0][1], &python[1][1]);
        let host = Scripted::new(role, answers);
        let result = obudpst::build(&host, &obudpst::script());
        let made = Value::Array(host.calls.lock().unwrap().clone());
        assert_eq!(&made, calls, "case {i}: the calls");
        assert_eq!(outcome(&result), python_outcome(ended), "case {i}: the end");
    }
}

#[test]
#[ignore = "builds OB-UDPST for real, fetching its source: by hand, on Linux"]
#[cfg(target_os = "linux")]
fn obudpst_builds_on_a_local_host() {
    use bench_rounds::host::Host;
    use bench_rounds::interrupt::Flag;
    use bench_rounds::process::Runner;
    let dir = std::env::temp_dir().join(format!("udpstp-bench-obudpst-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let work = RemotePath::new(dir.to_str().unwrap()).unwrap();
    let host = Host::local("server", "127.0.0.1", work, Runner::new(Flag::new()));
    let started = std::time::Instant::now();
    obudpst::build(&host, &obudpst::script()).unwrap_or_else(|e| panic!("{e}"));
    eprintln!("built in {:?}", started.elapsed());
    let usage = host
        .run("./udpst -? 2>&1; true", bench_rounds::host::RUN_LIMIT)
        .unwrap();
    assert!(usage.stdout.contains("udpst"), "{}", usage.stdout);
    let _ = std::fs::remove_dir_all(&dir);
}
