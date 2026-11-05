//! The `aws` CLI adapter and the AWS provider on the `fake-aws` binary
//!: each call a
//! process of its own, as a Round makes it, in the sealed environment, in
//! which no real credentials or configuration can be found. No test here
//! reaches AWS: the CLI is always `fake-aws`, which `Cli::new` takes and
//! would refuse the real CLI for, or a script of the test's own.
//!
//! - Every scenario of `tests/golden/aws.json`, run through the adapter on
//!   the binary, makes the Python's calls, returns and refuses what it
//!   did, and waits as long.
//! - `tests/golden/aws-failures.json` holds what the Python's provider and
//!   `round.teardown` did on this same fake with a launch failing for good,
//!   throttled past its retries or interrupted mid-call, an instance that
//!   outlives the Round Teardown and a group held; the Rust provider and
//!   its Round Teardown must do the same, and leave nothing they made that
//!   they don't say is left.
//! - The provider as a Round's: its hosts, an interrupt, the CLI's own
//!   limit.
//! - On Unix, with a script of the test's own as the CLI or as ssh: the
//!   environment the CLI runs in, the credentials read for their expiry
//!   alone and never recorded, a Round recorded and replayed without a
//!   difference, and the hosts reached over ssh with the Round's files.
//!
//! The goldens are read with serde_json, not the parser under test.

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use bench_rounds::aws::cli::{Cli, Clock};
use bench_rounds::aws::{AwsError, AwsProvider, CallError, Calls, Launched, plan};
use bench_rounds::host::Shell;
use bench_rounds::interrupt::Flag;
use bench_rounds::num::Num;
use bench_rounds::process::Runner;
use bench_rounds::provider::{Access, Leftover, Provider, ProviderError, RoundId};
use bench_rounds::py::{self, Json};
use bench_rounds::round_teardown::destroy_resources;
use common::untag;
use indexmap::IndexMap;
use serde_json::Value;

const FAKE: &str = env!("CARGO_BIN_EXE_fake-aws");
const REGION: &str = "eu-north-1";
const ZONE: &str = "eu-north-1a";
/// The goldens' clock, `export_goldens.py`'s `NOW`.
const NOW: f64 = 1_790_000_000.25;
/// A paused call's file, never made: the call ends only when it is killed.
const NEVER: &str = "/nonexistent/udpstp-bench-go";

/// One test at a time, each taking it first. A Unix test writes a script
/// and then runs it as the CLI or as ssh; another test's fork meanwhile
/// copies the script's open write handle into its child, which holds it
/// until it execs, and the script can't be run while any process holds it
/// open for writing: "Text file busy" (ETXTBSY, rust-lang/rust),
/// about 1 run in 10 on Linux without it.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A directory of its own under the system's temporary directory, removed
/// when the test ends.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> TempDir {
        let dir = std::env::temp_dir().join(format!(
            "bench-rounds-aws-cli-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A PATH on which no real `aws` is found; the fake is run by its path.
fn minimal_path() -> String {
    if cfg!(windows) {
        std::env::var("SystemRoot")
            .map(|root| format!("{root}\\System32"))
            .unwrap_or_default()
    } else {
        "/usr/bin:/bin".to_string()
    }
}

/// The sealed environment (the Python's `sealed_env`), with the fake's
/// state file.
fn sealed(state: &Path) -> IndexMap<String, String> {
    let vars = std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)));
    let mut env = plan::sealed_env(vars, &minimal_path());
    env.insert(
        "FAKE_AWS_STATE".to_string(),
        state.to_string_lossy().into_owned(),
    );
    env
}

type Waits = Arc<Mutex<Vec<u64>>>;

/// The adapter on `fake-aws` with its state in `state`, stopped by `flag`,
/// its clock fixed at `now` and its waits written into `waits`. A call
/// may take a minute here, so that a test that goes wrong fails rather
/// than hangs.
fn fake_cli(state: &Path, flag: &Flag, now: f64, waits: &Waits) -> Cli {
    Cli::new(FAKE, REGION, Runner::new(flag.clone()))
        .expect("fake-aws isn't the real CLI")
        .with_env(sealed(state))
        .with_limit(Duration::from_secs(60))
        .with_clock(Clock::Fixed {
            now,
            waits: Arc::clone(waits),
        })
}

fn two_hosts(cli: Cli) -> AwsProvider<Cli> {
    AwsProvider::new(
        cli,
        REGION,
        ZONE,
        IndexMap::from([
            ("server".to_string(), "c6in.large".to_string()),
            ("client".to_string(), "c6in.large".to_string()),
        ]),
        IndexMap::from([("c6in.large".to_string(), Num::Float(0.142))]),
        "203.0.113.7/32",
        true,
    )
}

fn read_state(state: &Path) -> Json {
    Json::parse_bytes(&std::fs::read(state).unwrap()).unwrap()
}

/// The calls the fake recorded, each its argv after the CLI.
fn calls(state: &Path) -> Vec<Vec<String>> {
    read_state(state)
        .get("calls")
        .unwrap()
        .as_list()
        .map(|calls| {
            calls
                .iter()
                .map(|c| {
                    c.as_list()
                        .unwrap()
                        .iter()
                        .map(|a| a.as_str().unwrap().to_string())
                        .collect()
                })
                .collect()
        })
        .unwrap_or_default()
}

fn made(state: &Path, operation: &str) -> usize {
    calls(state)
        .iter()
        .filter(|c| c[1] == operation && !c.iter().any(|a| a == "--dry-run"))
        .count()
}

/// Each instance of the fake's state and its state, each group's ID.
fn resources(state: &Path) -> (Vec<(String, String)>, Vec<String>) {
    let state = read_state(state);
    let instances = state
        .get("instances")
        .unwrap()
        .as_dict()
        .map(|found| {
            found
                .iter()
                .map(|(id, i)| {
                    let st = i.item("state").unwrap().as_str().unwrap();
                    (id.clone(), st.to_string())
                })
                .collect()
        })
        .unwrap_or_default();
    let groups = state
        .get("groups")
        .unwrap()
        .as_dict()
        .map(|found| found.keys().cloned().collect())
        .unwrap_or_default();
    (instances, groups)
}

/// The Round Teardown's part the provider plays, as a Round runs it after
/// its flag stopped acting: what is left, what it logged and alerted, its
/// waits into `waits`.
fn round_teardown(
    provider: &mut dyn Provider,
    round: &RoundId,
    flag: &Flag,
    waits: &Waits,
) -> (Vec<Leftover>, Vec<String>, Vec<String>) {
    let begun = flag.stop_acting();
    let (mut logged, mut alerted) = (Vec::new(), Vec::new());
    let left = destroy_resources(
        provider,
        round,
        begun,
        &mut |line: &str| logged.push(line.to_string()),
        &mut |line: &str| alerted.push(line.to_string()),
        &mut |pause: Duration| waits.lock().unwrap().push(pause.as_secs()),
    );
    (left, logged, alerted)
}

fn scenarios(file: &str) -> Vec<Value> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(file);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
    let mut golden: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path:?}: {e}"));
    match golden["scenarios"].take() {
        Value::Array(scenarios) if !scenarios.is_empty() => scenarios,
        _ => panic!("{file} has no scenarios"),
    }
}

fn string(value: &Value) -> &str {
    value
        .as_str()
        .unwrap_or_else(|| panic!("not a string: {value}"))
}

/// A scenario's provider, as the Python's was made.
struct Setup {
    host_types: IndexMap<String, String>,
    prices: IndexMap<String, Num>,
    home_cidr: String,
    home_udp: bool,
    now: f64,
}

impl Setup {
    fn of(scenario: &Value) -> Setup {
        Setup {
            host_types: scenario["host_types"]
                .as_object()
                .expect("host types")
                .iter()
                .map(|(role, t)| (role.clone(), string(t).to_string()))
                .collect(),
            prices: untag(&scenario["prices"])
                .as_dict()
                .expect("prices")
                .iter()
                .map(|(t, p)| (t.clone(), Num::from_json(p).expect("a price")))
                .collect(),
            home_cidr: string(&scenario["home_cidr"]).to_string(),
            home_udp: scenario["home_udp"].as_bool().expect("home_udp"),
            now: Num::from_json(&untag(&scenario["now"]))
                .expect("a time")
                .to_f64(),
        }
    }

    fn provider(&self, cli: Cli) -> AwsProvider<Cli> {
        AwsProvider::new(
            cli,
            REGION,
            ZONE,
            self.host_types.clone(),
            self.prices.clone(),
            &self.home_cidr,
            self.home_udp,
        )
    }
}

/// The Python's `Host` of a launched host, as the goldens have it: `[ssh,
/// address, public_address]`.
fn launched_host(launched: &Launched) -> Json {
    let or_null = |a: &Option<String>| a.clone().map_or(Json::Null, Json::Str);
    Json::List(vec![
        Json::Str(format!(
            "ubuntu@{}",
            launched.public_address.as_deref().unwrap_or("None")
        )),
        or_null(&launched.private_address),
        or_null(&launched.public_address),
    ])
}

/// The same of a host the Round was handed.
fn shell_host(host: &dyn Shell) -> Json {
    Json::List(vec![
        Json::Str(host.destination().to_string()),
        Json::Str(host.address().to_string()),
        host.public_address()
            .map_or(Json::Null, |a| Json::Str(a.to_string())),
    ])
}

fn pairs(left: Vec<(String, String)>) -> Json {
    Json::List(
        left.into_iter()
            .map(|(kind, id)| Json::List(vec![Json::Str(kind), Json::Str(id)]))
            .collect(),
    )
}

fn leftover_pairs(left: &[Leftover]) -> Json {
    pairs(
        left.iter()
            .map(|l| (l.kind.clone(), l.id.clone()))
            .collect(),
    )
}

fn number(n: Num) -> Json {
    match n {
        Num::Int(i) => Json::Int(i),
        Num::Float(f) => Json::Float(f),
    }
}

/// One of an `aws.json` scenario's actions, as the exporter's `perform`
/// does it, on the provider over the adapter.
fn perform(
    provider: &mut AwsProvider<Cli>,
    action: &[Json],
    state: &Path,
    fresh: &dyn Fn() -> AwsProvider<Cli>,
) -> Result<Json, CallError> {
    let text = |i: usize| action[i].as_str().expect("a string").to_string();
    let float = |i: usize| Num::from_json(&action[i]).expect("a number").to_f64();
    match action[0].as_str().expect("an action") {
        "check" => provider.check(float(1)),
        "check_permissions" => provider.check_permissions(&text(1)).map(|verdicts| {
            Json::Dict(
                verdicts
                    .into_iter()
                    .map(|(action, verdict)| (action, Json::Str(verdict)))
                    .collect(),
            )
        }),
        "price" => provider
            .price_per_hour(&text(1))
            .map(number)
            .map_err(CallError::Aws),
        "create" => {
            let roles: Vec<String> = action[3]
                .as_list()
                .expect("roles")
                .iter()
                .map(|r| r.as_str().expect("a role").to_string())
                .collect();
            provider
                .create(&text(1), float(2), &roles, &text(4))
                .map(|hosts| {
                    Json::Dict(
                        hosts
                            .iter()
                            .map(|(role, launched)| (role.clone(), launched_host(launched)))
                            .collect(),
                    )
                })
        }
        "destroy" => provider.destroy(&text(1)).map(pairs),
        "leftovers" => provider.leftovers(&text(1)).map(pairs),
        "reap" => provider
            .reap(float(1))
            .map(|reaped| Json::List(reaped.into_iter().map(Json::Str).collect())),
        "manual_cleanup" => Ok(Json::List(
            provider
                .manual_cleanup(&text(1))
                .into_iter()
                .map(Json::Str)
                .collect(),
        )),
        "configure" => {
            // `state.setdefault("config", {}).update(changes)`, in the
            // fake's file, as the exporter's `json.dump` writes it.
            let mut whole = read_state(state);
            let Json::Dict(top) = &mut whole else {
                panic!("the state isn't a dict");
            };
            let config = top
                .entry("config".to_string())
                .or_insert_with(|| Json::Dict(IndexMap::new()));
            let Json::Dict(config) = config else {
                panic!("the config isn't a dict");
            };
            for (key, value) in action[1].as_dict().expect("changes") {
                config.insert(key.clone(), value.clone());
            }
            std::fs::write(state, py::dumps(&whole)).unwrap();
            Ok(Json::Null)
        }
        "fresh" => {
            *provider = fresh();
            Ok(Json::Null)
        }
        other => panic!("no action {other}"),
    }
}

/// Every scenario of the Python's provider on its fake, through the
/// adapter on the `fake-aws` binary: the same calls, byte for byte, in the
/// same order (the final state holds them all), the same results and
/// refusals, the same waits.
#[test]
fn the_provider_makes_the_pythons_calls_through_the_cli() {
    let _serial = serial();
    for (n, scenario) in scenarios("aws.json").iter().enumerate() {
        let name = string(&scenario["name"]);
        let setup = Setup::of(scenario);
        let dir = TempDir::new(&format!("golden-{n}"));
        let state = dir.0.join("state.json");
        std::fs::write(&state, string(&scenario["state"])).unwrap();
        let flag = Flag::new();
        let waits = Waits::default();
        let fresh = || setup.provider(fake_cli(&state, &flag, setup.now, &waits));
        let mut provider = fresh();
        let actions = untag(&scenario["actions"]);
        let results = scenario["results"].as_array().expect("results");
        for (i, (action, expected)) in actions
            .as_list()
            .expect("actions")
            .iter()
            .zip(results)
            .enumerate()
        {
            let action = action.as_list().expect("an action");
            let got = perform(&mut provider, action, &state, &fresh);
            let at = format!("{name}, action {i} {:?}", action[0]);
            match (expected.get("ok"), got) {
                (Some(value), Ok(json)) => assert_eq!(json, untag(value), "{at}"),
                (None, Err(CallError::Aws(AwsError(message)))) => {
                    assert_eq!(string(&expected["raises"]), "AwsError", "{at}");
                    assert_eq!(message, string(&expected["message"]), "{at}");
                }
                (_, got) => panic!("{at}: the Python {expected}, the port {got:?}"),
            }
        }
        let slept: Vec<u64> = scenario["slept"]
            .as_array()
            .expect("waits")
            .iter()
            .map(|s| s.as_u64().expect("seconds"))
            .collect();
        assert_eq!(*waits.lock().unwrap(), slept, "{name}: the waits");
        assert_eq!(
            std::fs::read_to_string(&state).unwrap(),
            string(&scenario["final"]),
            "{name}: the calls and the final state"
        );
    }
}

/// The paused call a scenario's state sets, if any: its operation and
/// which of its calls.
fn paused_call(state: &str) -> Option<(String, usize)> {
    let state: Value = serde_json::from_str(state).expect("a state");
    let pause = state["config"].get("pause_call")?;
    Some((
        string(&pause["command"]).to_string(),
        pause["n"].as_u64().expect("n") as usize,
    ))
}

/// Raises `flag` once the fake's state holds the `n`-th call of
/// `operation`, which waits for a file never made: the Round interrupted
/// while that call hangs, as by a Ctrl-C. When it was raised.
///
/// It reads the state while `fake-aws` replaces it, which on Windows can
/// fail for a moment: such a read counts as nothing made yet.
fn interrupt_when_made(
    state: PathBuf,
    operation: String,
    n: usize,
    flag: Flag,
) -> thread::JoinHandle<Instant> {
    thread::spawn(move || {
        let started = Instant::now();
        let made_so_far = || {
            let read = std::fs::read(&state).ok()?;
            let whole: Value = serde_json::from_slice(&read).ok()?;
            let calls = whole.get("calls")?.as_array()?.iter().filter(|call| {
                let words = call.as_array().map(Vec::as_slice).unwrap_or_default();
                words.get(1).and_then(Value::as_str) == Some(operation.as_str())
                    && !words.iter().any(|w| w.as_str() == Some("--dry-run"))
            });
            Some(calls.count())
        };
        while made_so_far().unwrap_or(0) < n {
            assert!(
                started.elapsed() < Duration::from_secs(60),
                "the paused call never came"
            );
            thread::sleep(Duration::from_millis(20));
        }
        flag.raise("a test");
        Instant::now()
    })
}

/// A line the Rust tool's Round Teardown logs in the Python's words
///: the Rust tool's
/// keep the glossary's, and list what is left as `instance i-1` where the
/// Python prints its list of pairs. A whole line at a time, as stage C's
/// table will: a line it doesn't hold fails the comparison.
fn in_the_pythons_words(line: &str) -> String {
    let pairs = |listed: &str| -> String {
        let each: Vec<String> = listed
            .split(", ")
            .map(|item| match item.split_once(' ') {
                Some((kind @ ("instance" | "security-group"), id)) => format!("('{kind}', '{id}')"),
                _ => panic!("no translation for what is left in {line:?}"),
            })
            .collect();
        format!("[{}]", each.join(", "))
    };
    if let Some(rest) = line.strip_prefix("Round Teardown attempt ") {
        if let Some((n, listed)) = rest.split_once(" left: ") {
            return format!("teardown attempt {n} left {}", pairs(listed));
        }
        if let Some((n, error)) = rest.split_once(" failed: ") {
            return format!("teardown attempt {n} failed: {error}");
        }
    }
    panic!("no translation for the log line {line:?}")
}

/// §7.5 item 3: the provider under failure, and its Round Teardown after
/// it, against what the Python's provider and `round.teardown` did on the
/// same fake: the same results and refusals, an interrupt as the Round's,
/// the same waits, the same log in the Python's words, the same calls in
/// the same order (the final state holds them all), and what is left.
/// Whatever the Round made is then gone, or said to be left, with the
/// commands that delete it by hand.
#[test]
fn under_failure_the_provider_and_its_round_teardown_do_as_the_pythons() {
    let _serial = serial();
    for (n, scenario) in scenarios("aws-failures.json").iter().enumerate() {
        let name = string(&scenario["name"]);
        let setup = Setup::of(scenario);
        let dir = TempDir::new(&format!("failure-{n}"));
        let state = dir.0.join("state.json");
        std::fs::write(&state, string(&scenario["state"])).unwrap();
        let flag = Flag::new();
        let waits = Waits::default();
        let mut provider = setup.provider(fake_cli(&state, &flag, setup.now, &waits));
        let watcher = paused_call(string(&scenario["state"]))
            .map(|(operation, n)| interrupt_when_made(state.clone(), operation, n, flag.clone()));
        let actions = untag(&scenario["actions"]);
        let results = scenario["results"].as_array().expect("results");
        let mut left = None;
        let mut logged = Vec::new();
        let mut interrupted = None;
        for (i, (action, expected)) in actions
            .as_list()
            .expect("actions")
            .iter()
            .zip(results)
            .enumerate()
        {
            let action = action.as_list().expect("an action");
            let text = |i: usize| action[i].as_str().expect("a string").to_string();
            let round = RoundId::from(text(1));
            let at = format!("{name}, action {i} {:?}", action[0]);
            let got = match action[0].as_str().expect("an action") {
                "create" => {
                    let roles: Vec<String> = action[3]
                        .as_list()
                        .expect("roles")
                        .iter()
                        .map(|r| r.as_str().expect("a role").to_string())
                        .collect();
                    let roles: Vec<&str> = roles.iter().map(String::as_str).collect();
                    let access = Access {
                        public_key: text(4),
                        ..Access::default()
                    };
                    let expiry = Num::from_json(&action[2]).expect("a time").to_f64();
                    Provider::create(&mut provider, &round, expiry, &roles, &access).map(|hosts| {
                        Json::Dict(
                            hosts
                                .iter()
                                .map(|(role, host)| (role.clone(), shell_host(host.as_ref())))
                                .collect(),
                        )
                    })
                }
                "teardown" => {
                    let (found, lines, alerted) =
                        round_teardown(&mut provider, &round, &flag, &waits);
                    logged.extend(lines);
                    if !found.is_empty() {
                        // Said in red, with every command that deletes it.
                        assert!(alerted[0].contains("LEFT BEHIND, delete by hand"), "{at}");
                        assert_eq!(
                            alerted[1..].len(),
                            provider.manual_cleanup(round.as_str()).len(),
                            "{at}"
                        );
                    }
                    let as_json = leftover_pairs(&found);
                    left = Some(found);
                    Ok(as_json)
                }
                other => panic!("no action {other}"),
            };
            match (expected.get("ok"), got) {
                (Some(value), Ok(json)) => assert_eq!(json, untag(value), "{at}"),
                (None, Err(ProviderError::Failed(message))) => {
                    assert_eq!(string(&expected["raises"]), "AwsError", "{at}");
                    assert_eq!(message, string(&expected["message"]), "{at}");
                }
                (None, Err(ProviderError::Interrupted)) => {
                    assert_eq!(string(&expected["raises"]), "Interrupted", "{at}");
                    interrupted = Some(Instant::now());
                }
                (_, got) => panic!("{at}: the Python {expected}, the port {got:?}"),
            }
        }
        if let Some(watcher) = watcher {
            let raised = watcher.join().expect("the interrupt was raised");
            let ended = interrupted.expect("the Round was interrupted");
            // The call in flight was stopped as the flag went up, not at
            // its limit.
            assert!(
                ended.saturating_duration_since(raised) < Duration::from_secs(10),
                "{name}: {:?}",
                ended.saturating_duration_since(raised)
            );
        }
        let slept: Vec<u64> = scenario["slept"]
            .as_array()
            .expect("waits")
            .iter()
            .map(|s| s.as_u64().expect("seconds"))
            .collect();
        assert_eq!(*waits.lock().unwrap(), slept, "{name}: the waits");
        // What the Round Teardown logged, a line for each of the Python's.
        let python_logged: Vec<&str> = scenario["logged"]
            .as_array()
            .expect("the log")
            .iter()
            .map(string)
            .collect();
        let translated: Vec<String> = logged.iter().map(|l| in_the_pythons_words(l)).collect();
        assert_eq!(translated, python_logged, "{name}: the log");
        assert_eq!(
            std::fs::read_to_string(&state).unwrap(),
            string(&scenario["final"]),
            "{name}: the calls and the final state"
        );
        // Whatever the Round made is gone, or said to be left.
        let left = left.expect("a Round Teardown");
        let (instances, groups) = resources(&state);
        assert!(!instances.is_empty(), "{name}: the Round made something");
        for (id, st) in instances {
            assert!(
                st == "terminated" || left.contains(&Leftover::new("instance", &id)),
                "{name}: {id} is {st}, and not said to be left"
            );
        }
        for id in groups {
            assert!(
                left.contains(&Leftover::new("security-group", &id)),
                "{name}: {id} is there, and not said to be left"
            );
        }
    }
}

/// The provider as a Round uses it, through the trait: the preflight, the
/// hosts it hands the Round, what it lists, and the Round Teardown after a
/// Ctrl-C, whose calls the raised flag no longer stops.
#[test]
fn the_provider_hands_the_round_hosts_and_takes_them_back() {
    let _serial = serial();
    let dir = TempDir::new("round");
    let state = dir.0.join("state.json");
    std::fs::write(
        &state,
        r#"{"config": {"expiration": "2099-01-01T00:00:00Z"}}"#,
    )
    .unwrap();
    let flag = Flag::new();
    let waits = Waits::default();
    let mut aws = two_hosts(fake_cli(&state, &flag, NOW, &waits));
    let provider: &mut dyn Provider = &mut aws;
    assert_eq!(provider.name(), "aws");
    assert!(provider.rented());
    assert_eq!(provider.price_per_hour("c6in.large").unwrap(), 0.142);
    assert!(matches!(
        provider.price_per_hour("m7i.large"),
        Err(ProviderError::Failed(m)) if m == "no pinned price for m7i.large: add it to prices.toml"
    ));
    provider.check(2.0, NOW).unwrap();
    assert!(provider.reap(NOW).unwrap().is_empty());
    let round = RoundId::from("r20260929T120000-ab12");
    let access = Access {
        public_key: "ssh-ed25519 AAAA t".into(),
        known_hosts: Some(dir.0.join("known_hosts")),
        identity: Some(dir.0.join("id_ed25519")),
        ssh_dir: None,
    };
    let hosts = provider
        .create(&round, NOW + 7200.0, &["server", "client"], &access)
        .unwrap();
    let handed: Vec<(&str, Json)> = hosts
        .iter()
        .map(|(role, host)| (role.as_str(), shell_host(host.as_ref())))
        .collect();
    let host = |ssh: &str, address: &str, public: &str| {
        Json::List(vec![
            Json::Str(ssh.into()),
            Json::Str(address.into()),
            Json::Str(public.into()),
        ])
    };
    assert_eq!(
        handed,
        [
            (
                "server",
                host("ubuntu@198.51.100.1", "10.0.0.1", "198.51.100.1")
            ),
            (
                "client",
                host("ubuntu@198.51.100.2", "10.0.0.2", "198.51.100.2")
            ),
        ]
    );
    assert_eq!(hosts["client"].role(), "client");
    assert_eq!(
        provider.leftovers(&round).unwrap(),
        [
            Leftover::new("instance", "i-1"),
            Leftover::new("instance", "i-2"),
            Leftover::new("security-group", "sg-1"),
        ]
    );
    assert_eq!(provider.recheck_after(), Some(Duration::from_secs(30)));
    assert!(
        provider
            .manual_cleanup(&round)
            .iter()
            .any(|c| c.contains("Values=r20260929T120000-ab12"))
    );
    // Interrupted: nothing more starts, until the Round Teardown begins.
    flag.raise("SIGINT");
    assert!(matches!(
        provider.leftovers(&round),
        Err(ProviderError::Interrupted)
    ));
    let before = calls(&state).len();
    let (left, logged, alerted) = round_teardown(provider, &round, &flag, &waits);
    assert!(left.is_empty() && logged.is_empty() && alerted.is_empty());
    assert!(
        calls(&state).len() > before,
        "the Round Teardown ran its calls"
    );
    assert_eq!(*waits.lock().unwrap(), [30]);
    let (instances, groups) = resources(&state);
    assert!(instances.iter().all(|(_, st)| st == "terminated"));
    assert!(groups.is_empty());
}

/// A call still running at the CLI's own limit is stopped and fails the
/// Round as the tool's failure, never the CLI's "timed out", which a
/// launch is retried on: one launch, no wait. What it made before it was
/// stopped goes in the Round Teardown, found by its tag.
#[test]
fn a_call_past_the_clis_own_limit_fails_and_is_never_retried() {
    let _serial = serial();
    let dir = TempDir::new("limit");
    let state = dir.0.join("state.json");
    std::fs::write(
        &state,
        format!(
            r#"{{"config": {{"pause_call": {{"command": "run-instances", "n": 1, "until": "{NEVER}"}}}}}}"#
        ),
    )
    .unwrap();
    let flag = Flag::new();
    let waits = Waits::default();
    // Native Windows can spend more than a second starting the fresh fake
    // executable. Let setup calls finish before timing out the paused launch.
    let limit = if cfg!(windows) { 5 } else { 1 };
    let mut cli = fake_cli(&state, &flag, NOW, &waits);
    // Start the fixture with its normal budget before imposing the short
    // deadline on the deliberately paused launch.
    cli.call(&plan::describe_subnets(ZONE)).unwrap();
    let mut aws = two_hosts(cli.with_limit(Duration::from_secs(limit)));
    let round = RoundId::from("r1");
    let started = Instant::now();
    let message = match Provider::create(
        &mut aws,
        &round,
        NOW + 7200.0,
        &["server"],
        &Access::default(),
    ) {
        Err(ProviderError::Failed(message)) => message,
        other => panic!("the Round's failure, not {:?}", other.map(|h| h.len())),
    };
    assert_eq!(
        message,
        format!("aws ec2 run-instances: no answer within {limit} s, so the CLI was stopped")
    );
    assert!(started.elapsed() < Duration::from_secs(30));
    assert_eq!(made(&state, "run-instances"), 1, "never retried");
    assert!(waits.lock().unwrap().is_empty());
    assert_eq!(aws.launched("r1"), None, "its answer never came");
    // The instance the stopped call made, by its tag.
    let (left, _, _) = round_teardown(&mut aws, &round, &flag, &waits);
    assert!(left.is_empty(), "{left:?}");
    let (instances, groups) = resources(&state);
    assert_eq!(instances, [("i-1".to_string(), "terminated".to_string())]);
    assert!(groups.is_empty());
}

/// A Round of two hosts on the fake, set to fail or hang its calls as
/// `setting` says, then its Round Teardown, with a call limit of a second:
/// what the fake still holds that the Round Teardown doesn't say is left,
/// if anything.
fn round_teardown_meets(name: &str, setting: &str) -> Vec<String> {
    let dir = TempDir::new(&format!("sweep-{name}"));
    let state = dir.0.join("state.json");
    std::fs::write(&state, format!("{{\"config\": {setting}}}")).unwrap();
    let flag = Flag::new();
    let waits = Waits::default();
    let cli = fake_cli(&state, &flag, NOW, &waits).with_limit(Duration::from_secs(1));
    let mut aws = two_hosts(cli);
    let round = RoundId::from("r1");
    let access = Access {
        public_key: "ssh-ed25519 AAAA t".into(),
        ..Access::default()
    };
    // Some settings fail the Round already here: the Round Teardown comes
    // all the same.
    let _ = Provider::create(
        &mut aws,
        &round,
        NOW + 7200.0,
        &["server", "client"],
        &access,
    );
    let (left, _, alerted) = round_teardown(&mut aws, &round, &flag, &waits);
    let (instances, groups) = resources(&state);
    let running: Vec<&String> = instances
        .iter()
        .filter(|(_, st)| st != "terminated")
        .map(|(id, _)| id)
        .collect();
    // An unknown is what a failed attempt may have left: to check by hand.
    let by_hand = left.iter().any(|l| l.kind == Leftover::UNKNOWN);
    let mut unsaid = Vec::new();
    for id in &running {
        if !by_hand && !left.contains(&Leftover::new("instance", *id)) {
            unsaid.push(format!(
                "{name}: instance {id} runs, and isn't said to be left"
            ));
        }
    }
    for id in &groups {
        if !by_hand && !left.contains(&Leftover::new("security-group", id)) {
            unsaid.push(format!("{name}: {id} is there, and isn't said to be left"));
        }
    }
    if left.is_empty() && !(running.is_empty() && groups.is_empty()) {
        unsaid.push(format!(
            "{name}: nothing is said to be left, but something is there"
        ));
    }
    if !left.is_empty() && alerted.is_empty() {
        unsaid.push(format!("{name}: something is left, and not said in red"));
    }
    unsaid
}

/// The review's sweep, trimmed (2026-09-30): the Round Teardown's own
/// calls failing for good, each from where it first comes, and three of
/// them running past the CLI's limit, as a real `ec2 wait` can (it polls
/// for up to 600 s, and the CLI is stopped at 300). Whatever the fake
/// still holds must be said to be left, by its ID or as an unknown to
/// check by hand, and nothing left must mean nothing is there.
#[test]
fn whatever_the_round_teardown_meets_what_is_there_is_said_to_be_left() {
    let _serial = serial();
    let fails = |operation: &str, n: usize| {
        (
            format!("{operation} {n} fails"),
            format!("{{\"fail_call\": {{\"command\": \"{operation}\", \"n\": {n}}}}}"),
        )
    };
    let hangs = |operation: &str, n: usize| {
        (
            format!("{operation} {n} hangs"),
            format!(
                "{{\"pause_call\": {{\"command\": \"{operation}\", \"n\": {n}, \"until\": \"{NEVER}\"}}}}"
            ),
        )
    };
    let cases = [
        // The Round's last call, then each the Round Teardown makes.
        fails("describe-instances", 1),
        fails("describe-instances", 2),
        fails("terminate-instances", 1),
        fails("wait", 2),
        fails("describe-security-groups", 1),
        fails("delete-security-group", 1),
        fails("describe-instances", 3),
        fails("describe-security-groups", 2),
        hangs("wait", 2),
        hangs("delete-security-group", 1),
        hangs("terminate-instances", 1),
    ];
    // Side by side: none writes a script, so no fork can hold one open.
    let unsaid: Vec<String> = thread::scope(|scope| {
        let each: Vec<_> = cases
            .iter()
            .map(|(name, setting)| scope.spawn(move || round_teardown_meets(name, setting)))
            .collect();
        each.into_iter()
            .flat_map(|case| case.join().expect("a case ran"))
            .collect()
    });
    assert!(unsaid.is_empty(), "{unsaid:#?}");
}

/// Scripts of the test's own as the CLI, or as ssh: they need a Unix
/// shell.
#[cfg(unix)]
mod on_unix {
    use super::*;
    use bench_rounds::aws::Calls;
    use bench_rounds::aws::cli::{CREDENTIALS_CAPACITY, WORK};
    use bench_rounds::aws::record::{self, Recorder};
    use bench_rounds::aws::replay;
    use bench_rounds::dead_man;
    use std::os::unix::fs::PermissionsExt;

    fn script(path: &Path, text: &str) {
        std::fs::write(path, format!("#!/bin/sh\n{text}")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// Every file under `dir`, with its bytes.
    fn everything(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut found = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                found.extend(everything(&path));
            } else {
                found.push((path.clone(), std::fs::read(&path).unwrap()));
            }
        }
        found
    }

    /// The CLI runs in the environment it is given and in nothing of the
    /// tool's own: no `AWS_` variable but the two that point the CLI's
    /// files at the null device, and the minimal PATH.
    #[test]
    fn the_cli_runs_in_the_environment_it_is_given_and_nothing_else() {
        let _serial = serial();
        let dir = TempDir::new("env");
        let cli_path = dir.0.join("aws-stand-in");
        let (env_out, argv_out) = (dir.0.join("env"), dir.0.join("argv"));
        script(
            &cli_path,
            "env > \"$ENV_OUT\"\nprintf '%s\\n' \"$@\" > \"$ARGV_OUT\"\necho '{\"Account\": \"123456789012\"}'\n",
        );
        let given = [
            ("AWS_PROFILE", "admin"),
            ("AWS_ACCESS_KEY_ID", "AKIAABCDEFGHIJKLMNOP"),
            ("AWS_SESSION_TOKEN", "a token"),
            ("ENV_OUT", env_out.to_str().unwrap()),
            ("ARGV_OUT", argv_out.to_str().unwrap()),
            ("KEPT", "yes"),
        ]
        .map(|(k, v)| (k.to_string(), v.to_string()));
        let mut cli = Cli::new(&cli_path, REGION, Runner::new(Flag::new()))
            .unwrap()
            .with_env(plan::sealed_env(given, "/usr/bin:/bin"));
        let answer = cli
            .call(&["sts".to_string(), "get-caller-identity".to_string()])
            .unwrap();
        assert_eq!(
            answer,
            Json::parse("{\"Account\": \"123456789012\"}").unwrap()
        );
        let env = std::fs::read_to_string(&env_out).unwrap();
        let vars: Vec<(&str, &str)> = env.lines().filter_map(|l| l.split_once('=')).collect();
        let aws: Vec<(&str, &str)> = vars
            .iter()
            .copied()
            .filter(|(k, _)| k.starts_with("AWS_"))
            .collect();
        assert_eq!(
            aws.len(),
            2,
            "only the CLI's two files, at the null device: {aws:?}"
        );
        assert!(aws.contains(&("AWS_CONFIG_FILE", "/dev/null")));
        assert!(aws.contains(&("AWS_SHARED_CREDENTIALS_FILE", "/dev/null")));
        assert!(vars.contains(&("PATH", "/usr/bin:/bin")));
        assert!(vars.contains(&("KEPT", "yes")));
        // Nothing of the tool's own environment it wasn't given.
        if std::env::var_os("HOME").is_some() {
            assert!(!vars.iter().any(|(k, _)| *k == "HOME"), "{vars:?}");
        }
        assert_eq!(
            std::fs::read_to_string(&argv_out).unwrap(),
            "sts\nget-caller-identity\n--region\neu-north-1\n--output\njson\n"
        );
    }

    const KEY: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
    const TOKEN: &str = "IQoJb3JpZ2luX2VjEXAMPLETOKEN";

    /// `configure export-credentials` is read for when the credentials
    /// expire, and nothing else of its output goes on, to the provider or
    /// into the records; its error output is kept, as any call's that
    /// doesn't look like credentials. More than the buffer holds fails the
    /// call.
    #[test]
    fn the_credentials_are_read_for_their_expiry_alone_and_never_recorded() {
        let _serial = serial();
        let dir = TempDir::new("credentials");
        let cli_path = dir.0.join("aws-stand-in");
        script(
            &cli_path,
            &format!(
                "case \"$1 $2\" in\n\
                 'configure export-credentials') printf '%s\\n' '{{\"Version\": 1, \"AccessKeyId\": \
                 \"ASIAABCDEFGHIJKLMNOP\", \"SecretAccessKey\": \"{KEY}\", \"SessionToken\": \"{TOKEN}\", \
                 \"Expiration\": \"2026-09-29T20:00:00+00:00\"}}'; echo 'a note' >&2 ;;\n\
                 *) echo '{{}}' ;;\n\
                 esac\n"
            ),
        );
        let recorder = Recorder::create(&dir.0.join("state").join("calls"), "r1", &[]).unwrap();
        let records = recorder.dir().to_path_buf();
        let mut cli = Cli::new(&cli_path, REGION, Runner::new(Flag::new()))
            .unwrap()
            .with_env(plan::sealed_env(Vec::new(), "/usr/bin:/bin"))
            .with_recorder(recorder);
        assert_eq!(
            cli.credentials_expire(),
            Ok(Some("2026-09-29T20:00:00+00:00".to_string()))
        );
        cli.call(&["sts".to_string(), "get-caller-identity".to_string()])
            .unwrap();
        assert!(cli.unrecorded().is_none());
        for (path, bytes) in everything(&records) {
            for secret in [KEY.as_bytes(), TOKEN.as_bytes(), b"ASIAABCDEFGHIJKLMNOP"] {
                assert!(
                    !bytes.windows(secret.len()).any(|w| w == secret),
                    "{} holds a credential",
                    path.display()
                );
            }
        }
        let read = record::read(&records).unwrap();
        assert_eq!(read.len(), 2);
        assert_eq!(
            read[0].argv,
            [
                "configure",
                "export-credentials",
                "--format",
                "process",
                "--region",
                "eu-north-1",
                "--output",
                "json"
            ]
        );
        assert_eq!(
            read[0].credentials,
            Some(Some("2026-09-29T20:00:00+00:00".to_string()))
        );
        assert_eq!(read[0].stdout, None);
        assert_eq!(read[0].stderr.as_deref(), Some(&b"a note\n"[..]));
        assert_eq!(read[1].stdout.as_deref(), Some(&b"{}\n"[..]));

        // More than the buffer holds: the call fails, and nothing of what
        // it printed is kept.
        script(
            &cli_path,
            &format!(
                "head -c {} /dev/zero | tr '\\0' 'x'\n",
                CREDENTIALS_CAPACITY + 1000
            ),
        );
        assert_eq!(
            cli.credentials_expire(),
            Err(CallError::Broken(format!(
                "aws configure export-credentials: more than {CREDENTIALS_CAPACITY} bytes of output, so the CLI was stopped"
            )))
        );
        let read = record::read(&records).unwrap();
        assert_eq!(read.len(), 3);
        assert_eq!(
            (read[2].status, read[2].credentials.clone()),
            (None, Some(None))
        );
    }

    /// A Round on the fake, recorded through the adapter as G's will be:
    /// every call in order, never the CLI's path, and the records replay
    /// on the fake without a difference.
    #[test]
    fn a_round_recorded_through_the_cli_replays_without_a_difference() {
        let _serial = serial();
        let dir = TempDir::new("recorded");
        let state = dir.0.join("state.json");
        std::fs::write(
            &state,
            r#"{"config": {"expiration": "2099-01-01T00:00:00Z", "throttle_run_instances": 2,
                "dependency_violations": 1, "unauthorized": "create-security-group"}}"#,
        )
        .unwrap();
        let round = "r20260929T120000-ab12";
        let recorder = Recorder::create(&dir.0.join("records"), round, &[]).unwrap();
        let records = recorder.dir().to_path_buf();
        let flag = Flag::new();
        let waits = Waits::default();
        let mut aws = two_hosts(fake_cli(&state, &flag, NOW, &waits).with_recorder(recorder));
        aws.check(2.0).unwrap();
        aws.check_permissions("permission-check").unwrap();
        let roles = ["server".to_string(), "client".to_string()];
        aws.create(round, NOW + 7200.0, &roles, "ssh-ed25519 AAAA t")
            .unwrap();
        assert_eq!(aws.destroy(round).unwrap(), []);
        assert!(aws.calls.unrecorded().is_none());
        let read = record::read(&records).unwrap();
        let made = calls(&state);
        assert!(made.len() > 20);
        assert_eq!(read.len(), made.len(), "every call recorded");
        for (record, call) in read.iter().zip(&made) {
            assert_eq!(&record.argv, call);
            assert!(!record.argv.iter().any(|a| a.contains("fake-aws")));
        }
        assert_eq!(replay::replay(&read), []);
    }

    /// Makes a directory writable again when dropped, so that a test that
    /// failed leaves it removable.
    struct Writable<'a>(&'a Path);

    impl Drop for Writable<'_> {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(self.0, std::fs::Permissions::from_mode(0o700));
        }
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    /// A record that can't be written, as on a full disk, ends the
    /// recording, never the Round: the calls go on and answer, and the CLI
    /// says why, which stage C prints in red. G goes on so (decided,
    /// 2026-09-30): a Round Teardown that stopped with the disk would leave
    /// its hosts running. No record appears after the one that failed.
    #[test]
    fn a_record_that_cant_be_written_ends_the_recording_not_the_round() {
        let _serial = serial();
        let dir = TempDir::new("unwritable");
        let state = dir.0.join("state.json");
        std::fs::write(&state, "{}").unwrap();
        let recorder = Recorder::create(&dir.0.join("records"), "r1", &[]).unwrap();
        let records = recorder.dir().to_path_buf();
        let flag = Flag::new();
        let waits = Waits::default();
        let mut cli = fake_cli(&state, &flag, NOW, &waits).with_recorder(recorder);
        let identity = ["sts".to_string(), "get-caller-identity".to_string()];
        let answer = Json::parse("{\"Account\": \"123456789012\"}").unwrap();
        assert_eq!(cli.call(&identity).unwrap(), answer);
        assert_eq!(record::read(&records).unwrap().len(), 1);
        let _writable = Writable(&records);
        std::fs::set_permissions(&records, std::fs::Permissions::from_mode(0o500)).unwrap();
        if std::fs::write(records.join("probe"), "").is_ok() {
            eprintln!("skipped: as root, the directory's mode holds nothing back");
            return;
        }
        for _ in 0..2 {
            assert_eq!(cli.call(&identity).unwrap(), answer);
        }
        let why = cli.unrecorded().expect("why the recording ended");
        assert_eq!(why.kind(), std::io::ErrorKind::PermissionDenied, "{why}");
        assert!(cli.recorder().is_none(), "no longer recording");
        assert_eq!(made(&state, "get-caller-identity"), 3);
        std::fs::set_permissions(&records, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(names(&records), ["0001.json", "0001.stderr", "0001.stdout"]);
        // Even where it could write again: the recording has ended.
        assert_eq!(cli.credentials_expire(), Ok(None));
        assert_eq!(cli.call(&identity).unwrap(), answer);
        assert_eq!(names(&records), ["0001.json", "0001.stderr", "0001.stdout"]);
    }

    /// What reached the CLI is recorded, and only that: not a call the
    /// Round was interrupted before, nor one whose CLI couldn't start. A
    /// CLI that a signal ended is recorded with no status, as the replay
    /// takes it, not as a status of its own.
    #[test]
    fn only_what_reached_the_cli_is_recorded() {
        let _serial = serial();
        let dir = TempDir::new("reached");
        let state = dir.0.join("state.json");
        std::fs::write(&state, "{}").unwrap();
        let identity = ["sts".to_string(), "get-caller-identity".to_string()];
        let recorder = |name: &str| Recorder::create(&dir.0.join("records"), name, &[]).unwrap();

        // Interrupted before the call: nothing ran, nothing is recorded.
        let flag = Flag::new();
        let waits = Waits::default();
        let first = recorder("interrupted");
        let records = first.dir().to_path_buf();
        let mut cli = fake_cli(&state, &flag, NOW, &waits).with_recorder(first);
        flag.raise("SIGINT");
        assert_eq!(cli.call(&identity), Err(CallError::Interrupted));
        assert_eq!(cli.credentials_expire(), Err(CallError::Interrupted));
        assert!(names(&records).is_empty());
        assert_eq!(made(&state, "get-caller-identity"), 0);

        // A CLI that isn't there: nothing ran.
        let second = recorder("absent");
        let records = second.dir().to_path_buf();
        let mut cli = Cli::new(dir.0.join("no-such-cli"), REGION, Runner::new(Flag::new()))
            .unwrap()
            .with_env(sealed(&state))
            .with_recorder(second);
        let Err(CallError::Broken(why)) = cli.call(&identity) else {
            panic!("a CLI that isn't there")
        };
        assert!(
            why.starts_with("aws sts get-caller-identity: couldn't start "),
            "{why}"
        );
        assert!(names(&records).is_empty());

        // A CLI a signal ended: its failure, recorded without a status.
        let killed = dir.0.join("killed-cli");
        script(&killed, "kill -KILL $$\n");
        let third = recorder("killed");
        let records = third.dir().to_path_buf();
        let mut cli = Cli::new(&killed, REGION, Runner::new(Flag::new()))
            .unwrap()
            .with_env(plan::sealed_env(Vec::new(), "/usr/bin:/bin"))
            .with_recorder(third);
        assert_eq!(
            cli.call(&identity),
            Err(CallError::Aws(AwsError(
                "aws sts get-caller-identity: ".into()
            )))
        );
        let read = record::read(&records).unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].status, None);
    }

    /// A stand-in for OpenSSH in `dir/bin`: `ssh` records its argv and its
    /// standard input, and answers as a rented host's dead-man switch
    /// check does, armed.
    fn stand_in_ssh(dir: &Path) -> PathBuf {
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let d = dir.display();
        script(
            &bin.join("ssh"),
            &format!(
                "printf '%s\\n' \"$@\" > {d}/ssh-argv\ncat > {d}/ssh-stdin\n\
                 printf 'USEC=1790007200000000 WARN_WALL=1 MODE=poweroff at every boot until 1790007200\\n'\n"
            ),
        );
        bin
    }

    /// The hosts the provider hands the Round are reached as the Round
    /// reaches a rented host: its ssh, as `ubuntu` at the public address,
    /// with the Round's own known-hosts file and key, the script on
    /// standard input, in the work directory; the dead-man switch checked
    /// through it.
    #[test]
    fn the_hosts_are_reached_over_ssh_with_the_rounds_files() {
        let _serial = serial();
        let dir = TempDir::new("ssh");
        let bin = stand_in_ssh(&dir.0);
        let state = dir.0.join("state.json");
        std::fs::write(&state, "{}").unwrap();
        let flag = Flag::new();
        let waits = Waits::default();
        let mut aws = two_hosts(fake_cli(&state, &flag, NOW, &waits));
        let (known, id) = (dir.0.join("known_hosts"), dir.0.join("id_ed25519"));
        let access = Access {
            public_key: "ssh-ed25519 AAAA t".into(),
            known_hosts: Some(known.clone()),
            identity: Some(id.clone()),
            ssh_dir: Some(bin),
        };
        let round = RoundId::from("r1");
        let hosts = Provider::create(
            &mut aws,
            &round,
            NOW + 7200.0,
            &["server", "client"],
            &access,
        )
        .unwrap();
        assert_eq!(
            dead_man::check(hosts["client"].as_ref()).unwrap(),
            "dead-man switch armed: USEC=1790007200000000 WARN_WALL=1 MODE=poweroff at every boot until 1790007200"
        );
        let argv = std::fs::read_to_string(dir.0.join("ssh-argv")).unwrap();
        let argv: Vec<&str> = argv.lines().collect();
        assert_eq!(
            argv,
            [
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=10",
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "StrictHostKeyChecking=accept-new",
                "-o",
                &format!("UserKnownHostsFile={}", known.display()),
                "-i",
                &id.display().to_string(),
                "-o",
                "IdentitiesOnly=yes",
                "-T",
                "ubuntu@198.51.100.2",
                "bash -s"
            ]
        );
        assert_eq!(
            std::fs::read_to_string(dir.0.join("ssh-stdin")).unwrap(),
            format!("mkdir -p {WORK} && cd {WORK} && {}", dead_man::CHECK_SCRIPT)
        );
        let (left, _, _) = round_teardown(&mut aws, &round, &flag, &waits);
        assert!(left.is_empty());
    }
}
