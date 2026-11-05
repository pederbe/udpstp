//! `udpstp-bench round` and `report`: the refusals before anything is created, the `--live`
//! rules, a plan on the in-process model of EC2, and a whole Round on
//! scripted hosts, through the command line as the runbooks run it.
//! Nothing here reaches a host, AWS or a socket.

use std::cell::RefCell;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bench_rounds::aws::fake::Local;
use bench_rounds::cost::ledger;
use bench_rounds::host::{Completed, Limit, RemotePath, Shell, Stop};
use bench_rounds::interrupt::{Flag, Interrupted};
use bench_rounds::provider::{Access, Leftover, Provider, ProviderError, RoundId, UnixTime};
use bench_rounds::py::Json;
use bench_rounds::round::{Clock, Stopped};
use bench_rounds::secrets::{SecretDir, SshKey};
use bench_rounds::{counters, dead_man, host};
use indexmap::IndexMap;
use udpstp_bench::cli::{self, Built, Env, Factory, Rented, RoundArgs, Setup, aws_provider};

const NOW: f64 = 1_790_000_000.0;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("udpstp-bench-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct Fixed(f64);

impl Clock for Fixed {
    fn now(&self) -> UnixTime {
        self.0
    }

    fn sleep(&self, _: Duration) -> Result<(), Interrupted> {
        Ok(())
    }

    fn pause(&self, _: Duration) {}
}

/// What a run of the command line printed, and its exit status.
struct Ran {
    code: u8,
    out: Vec<String>,
    err: Vec<String>,
}

fn fixed_key(keys: &SecretDir) -> Result<SshKey, Stopped> {
    let path = keys
        .write("id_ed25519", b"not a key")
        .map_err(|e| Stopped::Failed(e.to_string()))?;
    Ok((path, "ssh-ed25519 AAAAtest udpstp-bench\n".to_string()))
}

fn run(argv: &[&str], dir: &Path, factory: Option<Factory>, native_windows: bool) -> Ran {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let clock = Fixed(NOW);
    let mut argv: Vec<OsString> = argv.iter().map(OsString::from).collect();
    argv.insert(0, "udpstp-bench".into());
    let code = cli::main(
        argv,
        &mut Env {
            flag: Flag::new(),
            clock: &clock,
            out: &mut |line: &str| out.push(line.to_string()),
            err: &mut |line: &str| err.push(line.to_string()),
            factory,
            native_windows,
            xdg_state_home: Some(dir.join("state").to_str().unwrap().to_string()),
            home: dir.to_path_buf(),
            pid: 4242,
            keys_in: Some(dir.to_path_buf()),
            round_suffix: Some("abcd".into()),
            ssh_key: Some(&fixed_key),
        },
    );
    Ran { code, out, err }
}

fn matrix(name: &str) -> String {
    root().join(name).to_str().unwrap().to_string()
}

/// The Python's `Cli.test_no_test_passes_the_flag_that_reaches_a_real_account`,
/// and B3's rule: only the command line's `build_provider` makes the real
/// CLI (`Cli::live`), and no test passes the option that lets it.
#[test]
fn no_test_passes_the_flag_that_reaches_a_real_account() {
    let flag = format!("--{}", "live");
    let live = format!("Cli::{}(", "live");
    let mut makers = Vec::new();
    for krate in ["crates/bench-rounds", "crates/udpstp-bench"] {
        let mut stack = vec![root().join(krate)];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    if path.file_name().is_some_and(|n| n != "golden") {
                        stack.push(path);
                    }
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                let name = path.display().to_string();
                assert!(!text.contains(&format!("\"{flag}\"")), "{name}");
                assert!(!text.contains(&format!("'{flag}'")), "{name}");
                // Nor sets the option's field by hand.
                assert!(!text.contains(&format!("{}: true", "live")), "{name}");
                if text.contains(&live) {
                    makers.push(
                        path.strip_prefix(root())
                            .unwrap()
                            .to_str()
                            .unwrap()
                            .replace('\\', "/"),
                    );
                }
            }
        }
    }
    makers.sort();
    // Its definition, whose tests pass it a fake's path, and the command
    // line's one place.
    assert_eq!(
        makers,
        [
            "crates/bench-rounds/src/aws/cli.rs",
            "crates/udpstp-bench/src/cli.rs"
        ]
    );
    let cli = std::fs::read_to_string(root().join("crates/udpstp-bench/src/cli.rs")).unwrap();
    assert_eq!(cli.matches(&live).count(), 1);
}

fn plan_args<'a>(dir: &'a str, provider: &'a str) -> Vec<&'a str> {
    vec![
        "round",
        "--provider",
        provider,
        "--udpstp",
        "x",
        "--out",
        dir,
        "--home-cidr",
        "203.0.113.7/32",
    ]
}

/// The Python's `Cli.test_aws_needs_live`.
#[test]
fn aws_needs_live() {
    let dir = scratch("live");
    let out = dir.join("out");
    let first = matrix("scripts/bench/first-round.toml");
    let mut argv = plan_args(out.to_str().unwrap(), "aws");
    argv.extend(["--matrix", &first]);
    let ran = run(&argv, &dir, None, false);
    assert_eq!(ran.code, 2);
    assert_eq!(
        ran.err,
        ["udpstp-bench round: refused: --provider aws reaches a real account: add --live"]
    );
    assert!(!out.exists(), "nothing was made");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The Python's `Cli.test_a_paid_round_refuses_to_start_on_windows_itself`:
/// with --yes the AWS provider is refused on Windows itself, before it is
/// built; without it, the plan goes as far as the provider.
#[test]
fn a_paid_round_refuses_to_start_on_windows_itself() {
    let dir = scratch("windows");
    let out = dir.join("out");
    let first = matrix("scripts/bench/first-round.toml");
    let built = RefCell::new(0);
    let factory = |_: &RoundArgs, _: &Setup| -> Result<Built, String> {
        *built.borrow_mut() += 1;
        Err("as far as the provider".into())
    };
    let mut argv = plan_args(out.to_str().unwrap(), "aws");
    argv.extend(["--matrix", &first, "--yes"]);
    let ran = run(&argv, &dir, Some(&factory), true);
    assert_eq!(ran.code, 2);
    assert!(
        ran.err[0].starts_with(
            "udpstp-bench round: refused: a paid Round runs from Linux or WSL, not from Windows itself"
        ),
        "{:?}",
        ran.err
    );
    assert_eq!(*built.borrow(), 0);
    argv.pop();
    let ran = run(&argv, &dir, Some(&factory), true);
    assert_eq!(
        ran.err,
        ["udpstp-bench round: refused: as far as the provider"]
    );
    assert_eq!(*built.borrow(), 1);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_server_scale_matrix_checks_its_registered_options_before_contacting_hosts() {
    let dir = scratch("scale");
    let out = dir.join("out");
    let scale = matrix("scripts/bench/tests/scale-smoke.toml");
    let mut argv = plan_args(out.to_str().unwrap(), "static");
    argv.extend(["--matrix", &scale, "--host", "server=local,127.0.0.1"]);
    let ran = run(&argv, &dir, None, false);
    assert_eq!(ran.code, 2);
    assert!(
        ran.err[0].contains("give --prototype PATH")
            && ran.err[0].contains("--as-found takes them as they are"),
        "{:?}",
        ran.err
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Refusals of a static Round before its hosts are asked anything.
#[test]
fn a_static_round_is_refused_what_it_lacks() {
    let dir = scratch("static");
    let out = dir.join("out");
    let smoke = matrix("scripts/bench/tests/smoke.toml");
    let refused = |extra: &[&str]| {
        let mut argv = plan_args(out.to_str().unwrap(), "static");
        argv.extend(["--matrix", &smoke]);
        argv.extend(extra);
        let ran = run(&argv, &dir, None, false);
        assert_eq!(ran.code, 2, "{extra:?}: {:?}", ran.err);
        ran.err[0]
            .strip_prefix("udpstp-bench round: refused: ")
            .unwrap()
            .to_string()
    };
    assert_eq!(
        refused(&[]),
        "--provider static needs --host server=SSH,ADDRESS"
    );
    assert_eq!(
        refused(&["--host", "server=bench@testhost,192.168.178.22"]),
        "the matrix has Tests between hosts: give --host client=SSH,ADDRESS"
    );
    assert_eq!(
        refused(&[
            "--host",
            "server=bench@testhost,192.168.178.22",
            "--check-permissions"
        ]),
        "--check-permissions is for --provider aws"
    );
    assert_eq!(
        refused(&["--host", "server=h,1.2.3.4", "--as-found"]),
        "--as-found is for a Server-scale Round"
    );
    assert_eq!(
        refused(&["--host", "server=h,1.2.3.4", "--record-calls"]),
        "--record-calls is for a paid Round, on --provider aws"
    );
    assert_eq!(
        refused(&["--host", "server=-oProxyCommand=x,1.2.3.4"]),
        "server: \"-oProxyCommand=x\" isn't an ssh destination"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_matrix_or_an_option_that_cant_be_read_is_refused() {
    let dir = scratch("options");
    let out = dir.join("out");
    let smoke = matrix("scripts/bench/tests/smoke.toml");
    let mut argv = plan_args(out.to_str().unwrap(), "static");
    argv.extend(["--matrix", &smoke, "--max-h", "1"]);
    // No abbreviated options: argparse took --max-h for --max-hours.
    let ran = run(&argv, &dir, None, false);
    assert_eq!(ran.code, 2);
    let missing = dir.join("none.toml");
    let mut argv = plan_args(out.to_str().unwrap(), "static");
    argv.extend(["--matrix", missing.to_str().unwrap()]);
    let ran = run(&argv, &dir, None, false);
    assert_eq!(ran.code, 2);
    assert!(
        ran.err[0].starts_with(&format!(
            "udpstp-bench round: refused: {}: ",
            missing.display()
        )),
        "{:?}",
        ran.err
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The factory a test hands in: the AWS provider on the in-process model
/// of EC2, at the fixed time, in `state`.
fn on_the_model(state: &'static str) -> impl Fn(&RoundArgs, &Setup) -> Result<Built, String> {
    move |args: &RoundArgs, setup: &Setup| {
        let local = Local::new(Json::parse(state).unwrap(), &args.region, NOW);
        let provider = aws_provider(local, args, setup)?;
        Ok(Built::Rented(Box::new(provider)))
    }
}

const FRESH: &str = r#"{"config": {"expiration": "2099-01-01T00:00:00Z"}}"#;

#[test]
fn a_plan_on_aws_creates_nothing_and_says_so() {
    let dir = scratch("plan");
    let out = dir.join("out");
    let first = matrix("scripts/bench/first-round.toml");
    let factory = on_the_model(FRESH);
    let mut argv = plan_args(out.to_str().unwrap(), "aws");
    argv.extend(["--matrix", &first]);
    let ran = run(&argv, &dir, Some(&factory), false);
    assert_eq!(ran.code, 0, "{:?}", ran.err);
    let plan = Json::parse(&ran.out[0]).unwrap();
    assert_eq!(plan.item("provider").unwrap(), &Json::Str("aws".into()));
    assert_eq!(plan.item("tests").unwrap(), &Json::Int(80));
    assert_eq!(
        plan.item("roles").unwrap().item("client").unwrap(),
        &Json::Str("c6in.large".into())
    );
    assert_eq!(plan.item("month_spent").unwrap(), &Json::Int(0));
    assert_eq!(
        ran.out.last().unwrap(),
        "plan only: nothing was created. Add --yes to run the Round."
    );
    assert!(!out.exists());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn over_the_round_cap_is_refused_after_the_plan() {
    let dir = scratch("cap");
    let out = dir.join("out");
    let first = matrix("scripts/bench/first-round.toml");
    let factory = on_the_model(FRESH);
    let mut argv = plan_args(out.to_str().unwrap(), "aws");
    argv.extend(["--matrix", &first, "--round-cap", "0.01", "--yes"]);
    let ran = run(&argv, &dir, Some(&factory), false);
    assert_eq!(ran.code, 2);
    assert!(Json::parse(&ran.out[0]).is_ok(), "the plan first");
    assert!(
        ran.err[0].starts_with("udpstp-bench round: refused: the worst case, $"),
        "{:?}",
        ran.err
    );
    assert!(!out.exists());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// NaN passes every cap, an infinity makes no deadline, and nothing can
/// be run in no time or for no money: refused.
#[test]
fn caps_and_hours_that_arent_above_0_are_refused() {
    let dir = scratch("nan");
    let out = dir.join("out");
    let first = matrix("scripts/bench/first-round.toml");
    let factory = on_the_model(FRESH);
    for (option, value, shown) in [
        ("--max-hours", "nan", "NaN"),
        ("--round-cap", "NaN", "NaN"),
        ("--month-cap", "inf", "inf"),
        ("--max-hours", "0", "0"),
        ("--round-cap", "-1", "-1"),
        ("--month-cap", "-0.0", "-0"),
    ] {
        let said = format!("{option} must be a number above 0, not {shown}");
        let given = format!("{option}={value}");
        let mut argv = plan_args(out.to_str().unwrap(), "aws");
        argv.extend(["--matrix", &first, &given]);
        let ran = run(&argv, &dir, Some(&factory), false);
        assert_eq!(ran.code, 2, "{option}");
        assert_eq!(ran.err, [format!("udpstp-bench round: refused: {said}")]);
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Without a home directory the ledger would land in the working
/// directory, where the other tool's Rounds aren't: refused.
#[test]
fn a_ledger_without_a_home_is_refused() {
    let dir = scratch("no-home");
    let out = dir.join("out");
    let first = matrix("scripts/bench/first-round.toml");
    let factory = on_the_model(FRESH);
    let clock = Fixed(NOW);
    let mut err = Vec::new();
    let mut argv: Vec<OsString> = vec!["udpstp-bench".into()];
    let mut words = plan_args(out.to_str().unwrap(), "aws");
    words.extend(["--matrix", &first]);
    argv.extend(words.iter().map(OsString::from));
    let code = cli::main(
        argv,
        &mut Env {
            flag: Flag::new(),
            clock: &clock,
            out: &mut |_: &str| {},
            err: &mut |line: &str| err.push(line.to_string()),
            factory: Some(&factory),
            native_windows: false,
            xdg_state_home: None,
            home: PathBuf::new(),
            pid: 4242,
            keys_in: Some(dir.clone()),
            round_suffix: None,
            ssh_key: None,
        },
    );
    assert_eq!(code, 2);
    assert!(
        err[0].contains("no home directory to keep the ledger in"),
        "{err:?}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A paid Round's calls are recorded only when --record-calls asks, and
/// where they can't be, it is refused, saying how to keep them.
#[test]
fn a_paid_round_alone_is_recorded_where_it_can_be() {
    use clap::Parser;
    #[derive(Parser)]
    struct Args {
        #[command(flatten)]
        round: RoundArgs,
    }
    let args = |extra: &[&str]| {
        let mut argv = vec![
            "t",
            "--provider",
            "aws",
            "--matrix",
            "m",
            "--out",
            "o",
            "--udpstp",
            "x",
        ];
        argv.extend(extra);
        Args::try_parse_from(argv).unwrap().round
    };
    let live = format!("--{}", "live");
    // Only when asked, and only for a paid Round.
    assert!(cli::recorded(&args(&[&live, "--yes", "--record-calls"])));
    assert!(!cli::recorded(&args(&[&live, "--yes"])));
    assert!(!cli::recorded(&args(&[&live, "--record-calls"])));
    assert!(!cli::recorded(&args(&["--yes", "--record-calls"])));
    let mut static_args = args(&[&live, "--yes", "--record-calls"]);
    static_args.provider = "static".into();
    assert!(!cli::recorded(&static_args));
    // Inside this checkout, or on a system whose file modes can't be
    // checked, the records can't be kept.
    let prices = toml::Table::new();
    let types = IndexMap::new();
    let runner = bench_rounds::process::Runner::new(Flag::new());
    let state = root().join("target").join("udpstp-bench-recorder-test");
    let setup = Setup {
        prices: &prices,
        types: &types,
        home_udp: false,
        runner: &runner,
        recorded_as: None,
        xdg_state_home: state.to_str(),
        home: &root(),
    };
    let round = RoundId::from("r20260921T141320-abcd");
    let Err(why) = cli::recorder(&round, &root().join("results"), &setup) else {
        panic!("recorded inside the checkout")
    };
    assert!(
        why.starts_with("a paid Round's calls to AWS are recorded, and these can't be: "),
        "{why}"
    );
    assert!(
        why.ends_with("outside any repository and --out; or leave out --record-calls"),
        "{why}"
    );
    assert!(!state.join("udpstp-bench").exists());
}

#[test]
fn a_report_of_nothing_is_no_report() {
    let dir = scratch("report");
    let ran = run(&["report", dir.to_str().unwrap()], &dir, None, false);
    assert_eq!(ran.code, 1);
    assert!(
        ran.err[0].starts_with("udpstp-bench report: no report: "),
        "{:?}",
        ran.err
    );
    let ran = run(&["scale-report", dir.to_str().unwrap()], &dir, None, false);
    assert_eq!(ran.code, 1);
    assert!(ran.out.is_empty());
    assert!(ran.err[0].starts_with("udpstp-bench scale-report: no report: "));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The standalone parser accepts the same replacement decoding and
/// universal newlines as proxy_report.py, including its feasibility mode.
#[test]
fn proxy_reports_read_the_file_and_the_feasibility_option() {
    let dir = scratch("proxy-report");
    let source = dir.join("input.txt");
    std::fs::write(&source, b"not a result\xff\r\nsecond line\rthird line\n").unwrap();
    for feasibility in [false, true] {
        let mut argv = vec!["proxy-report", source.to_str().unwrap()];
        if feasibility {
            argv.push("--feasibility");
        }
        let ran = run(&argv, &dir, None, false);
        let parsed = udpstp_bench::proxy::parse(&[
            "not a result\u{fffd}\n",
            "second line\n",
            "third line\n",
        ])
        .unwrap();
        let expected = if feasibility {
            udpstp_bench::proxy::feasibility(&parsed).unwrap().1
        } else {
            udpstp_bench::proxy::reading(&parsed).unwrap().1
        };
        assert_eq!(ran.code, 0, "{:?}", ran.err);
        assert_eq!(ran.out, [expected.join("\n")]);
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A host of a whole Round that answers each call as a healthy one would.
struct Healthy {
    role: String,
    calls: Arc<Mutex<Vec<String>>>,
}

impl Shell for Healthy {
    fn work(&self) -> &RemotePath {
        static WORK: std::sync::OnceLock<RemotePath> = std::sync::OnceLock::new();

        WORK.get_or_init(|| RemotePath::new("/work").unwrap())
    }
    fn role(&self) -> &str {
        &self.role
    }

    fn address(&self) -> &str {
        "10.0.0.5"
    }

    fn public_address(&self) -> Option<&str> {
        None
    }

    fn destination(&self) -> &str {
        "ubuntu@203.0.113.10"
    }

    fn run(&self, script: &str, _: Limit) -> Result<Completed, Stop> {
        self.calls.lock().unwrap().push(script.to_string());
        let stdout = if script == dead_man::CHECK_SCRIPT {
            "USEC=1 at every boot until 1790007200\n"
        } else if script == counters::SCRIPT || script == counters::towards("10.0.0.5") {
            "Udp: InDatagrams\nUdp: 5\n"
        } else if script == host::ENVIRONMENT_SCRIPT {
            "Linux h\n"
        } else if script.starts_with("dev=$(ip") {
            "9001\n"
        } else if script.starts_with("cat server-") && script.ends_with(".pid") {
            "4242\n"
        } else if script.starts_with("kill -0 ") {
            "alive\n"
        } else if script.starts_with("env ") || script.starts_with("./udpst ") {
            r#"{"Output": {"AtMax": {"MaxIPLayerCapacity": 500.5}}}"#
        } else {
            ""
        };
        Ok(Completed {
            returncode: 0,
            stdout: stdout.into(),
            stderr: String::new(),
        })
    }

    fn put(&self, _: &Path, name: &RemotePath, _: u32) -> Result<(), Stop> {
        self.calls.lock().unwrap().push(format!("put {name}"));
        Ok(())
    }

    fn output(&self, _: &str, _: Limit) -> Result<Vec<u8>, Stop> {
        Ok(Vec::new())
    }
}

/// A rented provider whose hosts are [`Healthy`].
struct Rents {
    calls: Arc<Mutex<Vec<String>>>,
    made: bool,
}

impl Provider for Rents {
    fn name(&self) -> &'static str {
        "aws"
    }

    fn rented(&self) -> bool {
        true
    }

    fn price_per_hour(&self, _: &str) -> Result<f64, ProviderError> {
        Ok(0.142)
    }

    fn check(&mut self, _: f64, _: UnixTime) -> Result<(), ProviderError> {
        Ok(())
    }

    fn reap(&mut self, _: UnixTime) -> Result<Vec<RoundId>, ProviderError> {
        Ok(vec![RoundId::from("r20260101T000000-dead")])
    }

    fn create(
        &mut self,
        _: &RoundId,
        _: UnixTime,
        roles: &[&str],
        access: &Access,
    ) -> Result<IndexMap<String, Box<dyn Shell>>, ProviderError> {
        assert_eq!(access.public_key, "ssh-ed25519 AAAAtest udpstp-bench\n");
        assert!(access.known_hosts.is_some() && access.identity.is_some());
        self.made = true;
        Ok(roles
            .iter()
            .map(|role| {
                let host = Healthy {
                    role: role.to_string(),
                    calls: self.calls.clone(),
                };
                (role.to_string(), Box::new(host) as Box<dyn Shell>)
            })
            .collect())
    }

    fn destroy(&mut self, _: &RoundId) -> Result<Vec<Leftover>, ProviderError> {
        self.made = false;
        Ok(Vec::new())
    }

    fn leftovers(&mut self, _: &RoundId) -> Result<Vec<Leftover>, ProviderError> {
        Ok(Vec::new())
    }

    fn recheck_after(&self) -> Option<Duration> {
        Some(Duration::from_secs(30))
    }

    fn manual_cleanup(&self, _: &RoundId) -> Vec<String> {
        Vec::new()
    }
}

impl Rented for Rents {
    fn check_permissions(&mut self) -> Result<IndexMap<String, String>, ProviderError> {
        Ok(IndexMap::from([("run-instances".into(), "allowed".into())]))
    }

    fn unrecorded(&self) -> Option<String> {
        Some("the disk is full".into())
    }
}

/// A signal during the plan ends it before anything is created.
#[test]
fn a_round_interrupted_during_its_plan_creates_nothing() {
    let dir = scratch("interrupted-plan");
    let out = dir.join("out");
    let smoke = matrix("scripts/bench/tests/smoke.toml");
    let calls: Arc<Mutex<Vec<String>>> = Arc::default();
    let factory = |_: &RoundArgs, _: &Setup| -> Result<Built, String> {
        Ok(Built::Rented(Box::new(Rents {
            calls: calls.clone(),
            made: false,
        })))
    };
    let flag = Flag::new();
    flag.raise("a test");
    let clock = Fixed(NOW);
    let mut err = Vec::new();
    let mut argv: Vec<OsString> = vec!["udpstp-bench".into()];
    let mut words = plan_args(out.to_str().unwrap(), "aws");
    words.extend(["--matrix", &smoke, "--yes"]);
    argv.extend(words.iter().map(OsString::from));
    let code = cli::main(
        argv,
        &mut Env {
            flag,
            clock: &clock,
            out: &mut |_: &str| {},
            err: &mut |line: &str| err.push(line.to_string()),
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
    assert_eq!(code, 2);
    assert_eq!(err, ["udpstp-bench round: refused: interrupted"]);
    assert!(calls.lock().unwrap().is_empty());
    assert!(!out.exists());
    assert!(!dir.join("state/udpstp-bench/ledger.json").exists());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A Round asked to record its calls whose recording ended during its plan
/// is refused before anything is created; a plan asked to says it records
/// nothing.
#[test]
fn a_round_whose_recording_ended_in_its_plan_creates_nothing() {
    let dir = scratch("unrecorded");
    let out = dir.join("out");
    let smoke = matrix("scripts/bench/tests/smoke.toml");
    let calls: Arc<Mutex<Vec<String>>> = Arc::default();
    let factory = |_: &RoundArgs, _: &Setup| -> Result<Built, String> {
        Ok(Built::Rented(Box::new(Rents {
            calls: calls.clone(),
            made: false,
        })))
    };
    let mut argv = plan_args(out.to_str().unwrap(), "aws");
    argv.extend(["--matrix", &smoke, "--record-calls", "--yes"]);
    let ran = run(&argv, &dir, Some(&factory), false);
    assert_eq!(ran.code, 2);
    assert_eq!(
        ran.err,
        [
            "udpstp-bench round: refused: recording the Round's calls to AWS ended in its plan: \
          the disk is full; nothing was created"
        ]
    );
    assert!(calls.lock().unwrap().is_empty());
    assert!(!out.exists());
    argv.pop();
    let ran = run(&argv, &dir, Some(&factory), false);
    assert_eq!(ran.code, 0);
    assert_eq!(
        ran.out.last().unwrap(),
        "--record-calls records a Round run with --yes: this plan recorded nothing."
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A whole Round through the command line, on a rented provider of
/// healthy scripted hosts: its plan, its Round Runs' files, its ledger
/// entry torn down, and the recording's end said.
#[test]
fn a_whole_round_on_healthy_hosts_is_made_and_torn_down() {
    let dir = scratch("whole");
    let out = dir.join("out");
    let smoke = matrix("scripts/bench/tests/smoke-interop.toml");
    let calls: Arc<Mutex<Vec<String>>> = Arc::default();
    let factory = |_: &RoundArgs, _: &Setup| -> Result<Built, String> {
        Ok(Built::Rented(Box::new(Rents {
            calls: calls.clone(),
            made: false,
        })))
    };
    let mut argv = plan_args(out.to_str().unwrap(), "aws");
    argv.extend(["--matrix", &smoke, "--obudpst", "build", "--yes"]);
    let ran = run(&argv, &dir, Some(&factory), false);
    assert_eq!(ran.code, 0, "{:?}", ran.err);
    assert_eq!(ran.out[1], "reaped expired Rounds: r20260101T000000-dead");
    assert!(
        ran.err
            .last()
            .unwrap()
            .contains("recording the Round's calls to AWS ended early: the disk is full")
    );
    let summary = std::fs::read_to_string(out.join("summary.jsonl")).unwrap();
    // The path check's two Tests and the matrix's six.
    assert_eq!(summary.lines().count(), 8);
    let log = std::fs::read_to_string(out.join("round.log")).unwrap();
    assert!(
        log.trim_end()
            .ends_with("Round r20260921T141320-abcd: torn down; 8 Tests"),
        "{log}"
    );
    let book = ledger::load(&dir.join("state/udpstp-bench/ledger.json")).unwrap();
    let entry = &book.item("rounds").unwrap().as_list().unwrap()[0];
    assert_eq!(
        entry.item("id").unwrap(),
        &Json::Str("r20260921T141320-abcd".into())
    );
    assert_eq!(entry.item("state").unwrap(), &Json::Str("torn down".into()));
    // The Servers were stopped in the Round Teardown.
    assert!(calls.lock().unwrap().iter().any(|c| c == "kill 4242"));
    // The keys' directory is gone; only --out and the state are left.
    let mut left: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_str().unwrap().to_string())
        .collect();
    left.sort();
    assert_eq!(left, ["out", "state"]);
    std::fs::remove_dir_all(&dir).unwrap();
}
