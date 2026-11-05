//! `udpstp-bench round` against the independent command-line reference.
//! `tests/golden/cli.json` holds what `round.py`'s command line did in the
//! dry-run table, on AWS through the Rust `fake-aws` and on static hosts
//! as far as a plan goes without reaching one, and in Rounds whose second
//! launch fails, with instances or a security group the Round Teardown
//! can't end at once. `scripts/bench/tests/export_goldens.py` recorded it.
//!
//! The port, with the AWS provider on the same model in-process, must end
//! with the same exit status, print the same plan and refusals, log the
//! same lines, leave the same ledger, byte for byte, and make the same
//! calls to AWS, which the model's final state holds. The Round's ID, its
//! SSH key and the time are fixed alike in both. Its words are read into
//! the Python's a whole line at a time (§7.5's translation table), and
//! where the Python ends with a traceback, the port must refuse.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bench_rounds::aws::cli::HostsRunner;
use bench_rounds::aws::fake::Local;
use bench_rounds::aws::{CallError, Calls};
use bench_rounds::interrupt::{Flag, Interrupted};
use bench_rounds::process::Runner;
use bench_rounds::provider::UnixTime;
use bench_rounds::py::{self, Json};
use bench_rounds::round::{Clock, Stopped};
use bench_rounds::secrets::{SecretDir, SshKey};
use serde_json::Value;
use udpstp_bench::cli::{self, Built, Env, RoundArgs, RoundCalls, Setup, aws_provider};

const NOW: f64 = 1_790_000_000.0;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The model of EC2, shared with the test, which reads its state after.
#[derive(Clone)]
struct Shared(Arc<Mutex<Local>>);

impl Calls for Shared {
    fn call(&mut self, args: &[String]) -> Result<Json, CallError> {
        self.0.lock().unwrap().call(args)
    }

    fn credentials_expire(&mut self) -> Result<Option<String>, CallError> {
        self.0.lock().unwrap().credentials_expire()
    }

    fn sleep(&mut self, seconds: u64) -> Result<(), CallError> {
        self.0.lock().unwrap().sleep(seconds)
    }

    fn now(&mut self) -> f64 {
        self.0.lock().unwrap().now()
    }
}

impl HostsRunner for Shared {
    fn hosts_runner(&self) -> Runner {
        Runner::new(Flag::new())
    }
}

impl RoundCalls for Shared {}

struct Fixed;

impl Clock for Fixed {
    fn now(&self) -> UnixTime {
        NOW
    }

    fn sleep(&self, _: Duration) -> Result<(), Interrupted> {
        Ok(())
    }

    fn pause(&self, _: Duration) {}
}

fn fixed_key(keys: &SecretDir) -> Result<SshKey, Stopped> {
    let path = keys.path().join("id_ed25519");
    Ok((path, "ssh-ed25519 AAAAtest udpstp-bench\n".to_string()))
}

/// A tagged value as JSON.
fn untag(tagged: &Value) -> Json {
    match tagged {
        Value::Null => Json::Null,
        Value::Bool(b) => Json::Bool(*b),
        Value::String(s) => Json::Str(s.clone()),
        Value::Array(items) => Json::List(items.iter().map(untag).collect()),
        Value::Object(tags) => {
            if let Some(Value::String(digits)) = tags.get("int") {
                Json::Int(digits.parse().unwrap())
            } else if let Some(Value::String(bits)) = tags.get("float") {
                Json::Float(f64::from_bits(u64::from_str_radix(bits, 16).unwrap()))
            } else {
                Json::Dict(
                    tags["dict"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|pair| (pair[0].as_str().unwrap().to_string(), untag(&pair[1])))
                        .collect(),
                )
            }
        }
        Value::Number(n) => panic!("an untagged number {n}"),
    }
}

/// Leftovers listed as the port lists them, `instance i-1, ...`, as the
/// Python's `repr` of its list of pairs.
fn pairs(listed: &str) -> String {
    let each: Vec<String> = listed
        .split(", ")
        .map(|item| {
            let (kind, id) = item.split_once(' ').unwrap_or((item, ""));
            format!("('{kind}', '{id}')")
        })
        .collect();
    format!("[{}]", each.join(", "))
}

/// The port's line in the Python's words, a whole line at a time.
fn in_the_pythons_words(line: &str) -> String {
    let lower_round = |s: &str| {
        s.replace("Server-scale Round", "Server-scale round")
            .replace("the Round could end", "the round could end")
    };
    if let Some(why) = line.strip_prefix("udpstp-bench round: refused: ") {
        return format!("round: refused: {}", lower_round(why));
    }
    if let Some(ids) = line.strip_prefix("reaped expired Rounds: ") {
        let quoted: Vec<String> = ids.split(", ").map(|id| format!("'{id}'")).collect();
        return format!("reaped expired rounds: [{}]", quoted.join(", "));
    }
    if line.starts_with("plan only: ") {
        return line
            .replace("expired Rounds", "expired rounds")
            .replace("run the Round.", "run the round.");
    }
    if let Some(rest) = line.strip_prefix("\x1b[31mLEFT BEHIND, delete by hand: ")
        && let Some(listed) = rest.strip_suffix("\x1b[0m")
    {
        return format!(
            "\x1b[31mLEFT BEHIND, delete by hand: {}\x1b[0m",
            pairs(listed)
        );
    }
    if let Some(rest) = line.strip_prefix("Round Teardown attempt ") {
        let (n, rest) = rest.split_once(' ').unwrap();
        if let Some(listed) = rest.strip_prefix("left: ") {
            return format!("teardown attempt {n} left {}", pairs(listed));
        }
        if let Some(error) = rest.strip_prefix("failed: ") {
            return format!("teardown attempt {n} failed: {error}");
        }
    }
    if let Some(rest) = line.strip_prefix("listed again ")
        && let Some((seconds, listed)) = rest.split_once(" s after destroying it, the Round left: ")
    {
        return format!(
            "listed again {seconds} s after the teardown: {}",
            pairs(listed)
        );
    }
    if let Some(error) = line.strip_prefix("listing what the Round left failed: ") {
        return format!("listing what the round left failed: {error}");
    }
    if let Some(why) = line.strip_prefix("the Round stopped: ") {
        return format!("the round stopped: {why}");
    }
    if let Some(rest) = line.strip_prefix("Round r") {
        return format!("round r{rest}");
    }
    line.to_string()
}

fn scratch(n: usize) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "udpstp-bench-cli-golden-{n}-{serial}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// One case run by the port: what differs from the Python.
fn case(n: usize, args: &[Value], python: &Value) -> Vec<String> {
    let dir = scratch(n);
    let (out, ledger_path) = (dir.join("out"), dir.join("ledger.json"));
    let root_dir = format!("{}/", root().display());
    let argv: Vec<String> = args[0]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            a.as_str()
                .unwrap()
                .replace("<out>", out.to_str().unwrap())
                .replace("<ledger>", ledger_path.to_str().unwrap())
                .replace("<root>/", &root_dir)
        })
        .collect();
    let state = untag(&args[1]);
    if let Some(ledger) = args[2].as_str() {
        std::fs::write(&ledger_path, ledger).unwrap();
    }
    let model: Arc<Mutex<Option<Shared>>> = Arc::default();
    let factory = |args: &RoundArgs, setup: &Setup| -> Result<Built, String> {
        if args.provider != "aws" {
            return cli::build_provider(args, setup, 4242);
        }
        let shared = Shared(Arc::new(Mutex::new(Local::new(
            state.clone(),
            &args.region,
            NOW,
        ))));
        *model.lock().unwrap() = Some(shared.clone());
        Ok(Built::Rented(Box::new(aws_provider(shared, args, setup)?)))
    };
    let (mut printed, mut said) = (Vec::new(), Vec::new());
    let mut argv_os: Vec<std::ffi::OsString> = vec!["udpstp-bench".into(), "round".into()];
    argv_os.extend(argv.iter().map(Into::into));
    let code = cli::main(
        argv_os,
        &mut Env {
            flag: Flag::new(),
            clock: &Fixed,
            out: &mut |text: &str| printed.extend(text.split('\n').map(str::to_string)),
            err: &mut |text: &str| said.push(text.to_string()),
            factory: Some(&factory),
            native_windows: false,
            xdg_state_home: Some(dir.join("state").to_str().unwrap().to_string()),
            home: dir.clone(),
            pid: 4242,
            keys_in: Some(dir.clone()),
            round_suffix: Some("abcd".into()),
            ssh_key: Some(&fixed_key),
        },
    );
    let mut differ = Vec::new();
    let mut expect = |what: &str, python: Value, port: Value| {
        if python != port {
            differ.push(format!(
                "case {n} ({}): {what}: the Python {python}, the port {port}",
                argv.join(" ")
            ));
        }
    };
    if python.get("raises").is_some() {
        // A traceback in the Python; the port refuses.
        expect(
            "the Python's traceback, the port's refusal",
            Value::from(2),
            Value::from(code),
        );
        let _ = std::fs::remove_dir_all(&dir);
        return differ;
    }
    let python: serde_json::Map<String, Value> = python["ok"]["dict"]
        .as_array()
        .unwrap()
        .iter()
        .map(|pair| (pair[0].as_str().unwrap().to_string(), pair[1].clone()))
        .collect();
    let int = |v: &Value| -> i64 { v["int"].as_str().unwrap().parse().unwrap() };
    expect(
        "the exit status",
        Value::from(int(&python["code"])),
        Value::from(code),
    );
    let words = |lines: &[String]| -> Value {
        Value::from(
            lines
                .iter()
                .map(|l| in_the_pythons_words(l))
                .collect::<Vec<_>>(),
        )
    };
    expect("what it printed", python["stdout"].clone(), words(&printed));
    expect("what it said", python["stderr"].clone(), words(&said));
    let log: Vec<String> = std::fs::read_to_string(out.join("round.log"))
        .map(|text| {
            text.lines()
                .map(|l| l.get("[00:00:00] ".len()..).unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default();
    expect("the log", python["log"].clone(), words(&log));
    let ledger = std::fs::read_to_string(&ledger_path).ok();
    expect(
        "the ledger",
        python["ledger"].clone(),
        ledger.map_or(Value::Null, Value::from),
    );
    let final_state = match model.lock().unwrap().as_ref() {
        Some(shared) => py::dumps(&shared.0.lock().unwrap().state),
        None => py::dumps(&state),
    };
    let python_final = Json::parse(python["final"].as_str().unwrap()).unwrap();
    expect(
        "the calls to AWS and the state they left",
        Value::from(py::dumps(&python_final)),
        Value::from(final_state),
    );
    let _ = std::fs::remove_dir_all(&dir);
    differ
}

#[test]
fn round_does_what_round_py_does() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/cli.json");
    let golden: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let cases = golden["cases"].as_array().unwrap();
    assert!(cases.len() >= 80, "only {} cases", cases.len());
    let mut differ = Vec::new();
    for (n, c) in cases.iter().enumerate() {
        differ.extend(case(n, c["args"].as_array().unwrap(), &c["python"]));
    }
    assert!(
        differ.is_empty(),
        "{} differences, the first:\n{}",
        differ.len(),
        differ[..differ.len().min(8)].join("\n")
    );
}

#[test]
fn scale_round_plans_and_provider_failures_match_the_reference() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/scale-cli.json");
    let golden: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let cases = golden["cases"].as_array().unwrap();
    assert!(cases.len() >= 100);
    let differ: Vec<_> = cases
        .iter()
        .enumerate()
        .flat_map(|(n, c)| case(n, c["args"].as_array().unwrap(), &c["python"]))
        .collect();
    assert!(
        differ.is_empty(),
        "{} differences, the first:\n{}",
        differ.len(),
        differ[..differ.len().min(8)].join("\n")
    );
}
