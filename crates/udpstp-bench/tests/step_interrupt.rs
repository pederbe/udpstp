//! Design §7.5: a real SIGINT at every scripted host-call boundary in
//! both Round bodies, with the same complete comparisons as the ordinary
//! transcript tests. Opt in on Linux with UDPSTP_BENCH_INTERRUPTS.
//! Each case gets a fresh Rust child process and signal handlers.
//! Scripted peers finish normally; process cancellation has separate tests.

#[cfg(target_os = "linux")]
mod common;

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::BTreeMap;
    use std::fs;
    use std::os::unix::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use bench_rounds::interrupt;
    use bench_rounds::py::Json;
    use bench_rounds::secrets::SecretDir;
    use nix::sys::signal::{Signal, killpg};
    use nix::unistd::Pid;
    use serde_json::{Value, json};

    use crate::common::{self, step_signal::Interrupt};

    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn read(path: &Path) -> Value {
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
    }

    fn tagged(value: &Json) -> Value {
        let mut text = String::new();
        common::tag(value, &mut text);
        serde_json::from_str(&text).unwrap()
    }

    fn target(input: &Json, expected: &Json) -> Option<String> {
        let answers = input.get("answers").ok()?.as_dict()?;
        let mut selected = Vec::new();
        for (role, answers) in answers {
            for (at, answer) in answers.as_dict().unwrap() {
                if answer.as_list().unwrap()[0].as_str() == Some("interrupt") {
                    let at: usize = at.parse().unwrap();
                    let calls = expected
                        .get("calls")
                        .unwrap()
                        .get(role)
                        .unwrap()
                        .as_list()
                        .unwrap();
                    assert_eq!(
                        calls[at].as_list().unwrap()[1],
                        *answer,
                        "the selected call was reached"
                    );
                    selected.push(format!("{role} call {at}"));
                }
            }
        }
        if selected.is_empty() {
            return None;
        }
        assert_eq!(selected.len(), 1, "one signal per child");
        selected.pop()
    }

    struct Case {
        kind: &'static str,
        index: usize,
        target: String,
        input: Value,
        expected: Value,
    }

    fn cases() -> Vec<Case> {
        let directory = root().join("crates/udpstp-bench/tests/golden");
        let mut cases = Vec::new();
        let first = read(&directory.join("round.json"));
        for (index, case) in first["cases"].as_array().unwrap().iter().enumerate() {
            if case["fn"] != "first_round.steps" {
                continue;
            }
            let input = &case["args"][0];
            let expected = &case["python"]["ok"];
            if let Some(target) = target(&common::untag(input), &common::untag(expected)) {
                cases.push(Case {
                    kind: "first",
                    index,
                    target,
                    input: input.clone(),
                    expected: expected.clone(),
                });
            }
        }
        let scale = read(&directory.join("scale-round.json"));
        let pool = common::scale_judge::pool(&scale["values"]);
        for (index, case) in scale["cases"].as_array().unwrap().iter().enumerate() {
            let input = &pool[case["args"][0].as_u64().unwrap() as usize];
            let expected = &pool[case["python"]["ok"].as_u64().unwrap() as usize];
            if let Some(target) = target(input, expected) {
                cases.push(Case {
                    kind: "scale",
                    index,
                    target,
                    input: tagged(input),
                    expected: tagged(expected),
                });
            }
        }
        cases
    }

    fn child(source: &Path, kind: &str) {
        // The custom main, before any thread, just as in the executable.
        let flag = interrupt::install().unwrap();
        let interrupt = Interrupt::installed(flag.clone());
        let case = read(source);
        let index = case["index"].as_u64().unwrap() as usize;
        if kind == "cap" {
            common::first_steps::capped_server_interrupted(
                case["target"].as_str().unwrap(),
                interrupt,
            );
        } else if kind == "first" {
            let differences =
                common::first_steps::steps(index, &case["input"], &case["expected"], interrupt);
            assert!(differences.is_empty(), "{}", differences.join("\n"));
        } else {
            common::scale_steps::steps(
                index,
                &common::untag(&case["input"]),
                &common::untag(&case["expected"]),
                interrupt,
                false,
            );
        }
        assert_eq!(flag.cause(), Some("SIGINT"));
        assert_eq!(flag.count(), 1);
        assert!(!flag.acting(), "the Round Teardown finished");
    }

    struct Running(Child);
    impl Drop for Running {
        fn drop(&mut self) {
            if self.0.try_wait().is_ok_and(|status| status.is_none()) {
                let _ = killpg(Pid::from_raw(self.0.id() as i32), Signal::SIGKILL);
            }
            let _ = self.0.wait();
        }
    }

    fn run(case: &Case, directory: &Path) {
        let source = directory.join("case.json");
        let log_path = directory.join("child.log");
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.arg("--child").arg(&source).arg(case.kind);
        let log = fs::File::create(&log_path).unwrap();
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LC_ALL", "C.UTF-8")
            .env("TMPDIR", directory)
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log);
        let mut child = Running(command.spawn().unwrap());
        let began = Instant::now();
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                assert!(
                    status.success(),
                    "{} case {}, {}, {}: {status}\n{}",
                    case.kind,
                    case.index,
                    case.target,
                    "Rust",
                    fs::read_to_string(&log_path).unwrap()
                );
                break;
            }
            assert!(
                began.elapsed() < Duration::from_secs(20),
                "{} case {}, {}: child timed out\n{}",
                case.kind,
                case.index,
                case.target,
                fs::read_to_string(&log_path).unwrap()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    pub fn main() {
        let args: Vec<_> = std::env::args_os().collect();
        if args.get(1).is_some_and(|arg| arg == "--child") {
            child(Path::new(&args[2]), args[3].to_str().unwrap());
            return;
        }
        if std::env::var_os("UDPSTP_BENCH_INTERRUPTS").is_none() {
            println!("step_interrupt: opt in with UDPSTP_BENCH_INTERRUPTS on Linux");
            return;
        }
        let cases = cases();
        let mut coverage = BTreeMap::<String, usize>::new();
        for case in &cases {
            let input = common::untag(&case.input);
            let path = input
                .get("select")
                .ok()
                .filter(|v| !v.is_null())
                .map_or_else(
                    || input.get("matrix").unwrap().as_str().unwrap().to_string(),
                    bench_rounds::py::dumps,
                );
            *coverage.entry(format!("{} {path}", case.kind)).or_default() += 1;
        }
        assert!(coverage.contains_key("first scripts/bench/tests/smoke-interop.toml"));
        assert!(coverage.contains_key("scale scripts/bench/tests/home-lab-dry.toml"));
        for (engine, kind) in [
            ("udpstp", "curve"),
            ("udpstp", "p1"),
            ("udpstp", "probe"),
            ("obudpst", "curve"),
            ("prototype-a", "curve"),
            ("prototype-b", "curve"),
            ("prototype-b", "p1"),
            ("prototype-b", "probe"),
            ("prototype-b-core", "curve"),
        ] {
            let select = Json::List(vec![Json::Str(engine.into()), Json::Str(kind.into())]);
            let key = format!("scale {}", bench_rounds::py::dumps(&select));
            assert!(coverage.contains_key(&key), "no signal targets for {key}");
        }
        for case in &cases {
            let directory = SecretDir::create().unwrap();
            fs::write(
                directory.path().join("case.json"),
                json!({"index": case.index, "input": case.input, "expected": case.expected})
                    .to_string(),
            )
            .unwrap();
            run(case, directory.path());
        }
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
            let directory = SecretDir::create().unwrap();
            fs::write(
                directory.path().join("case.json"),
                json!({"index":0,"target":at}).to_string(),
            )
            .unwrap();
            run(
                &Case {
                    kind: "cap",
                    index: 0,
                    target: at.into(),
                    input: Value::Null,
                    expected: Value::Null,
                },
                directory.path(),
            );
        }
        println!("first explicit Server cap: 9 real SIGINT boundaries, cleanup completed");
        for (path, count) in coverage {
            println!(
                "{path}: {count} real SIGINT boundaries, complete transcripts match the independent reference"
            );
        }
        println!("step_interrupt: {} boundaries passed", cases.len());
    }
}

fn main() {
    #[cfg(target_os = "linux")]
    linux::main();
}
