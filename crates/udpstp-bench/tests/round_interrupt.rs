//! Design §7.5: both complete Round brackets interrupted by a real SIGINT while the
//! second fake launch waits. Opt in with UDPSTP_BENCH_FAKE_AWS on Linux.
//! Neither child can reach real AWS.

#[cfg(target_os = "linux")]
mod linux {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use bench_rounds::aws::{cli as aws_cli, plan, record};
    use bench_rounds::interrupt::{self, Interrupted};
    use bench_rounds::process::Runner;
    use bench_rounds::round::{Clock, Stopped};
    use bench_rounds::secrets::{SecretDir, SshKey};
    use indexmap::IndexMap;
    use nix::sys::signal::{Signal, kill, killpg};
    use nix::unistd::Pid;
    use serde_json::{Value, json};
    use udpstp_bench::cli::{self, Built, Env, RoundArgs, Setup};

    const NOW: f64 = 1_790_000_000.0;
    struct Fixed;
    impl Clock for Fixed {
        fn now(&self) -> f64 {
            NOW
        }
        fn sleep(&self, _: Duration) -> Result<(), Interrupted> {
            Ok(())
        }
        fn pause(&self, _: Duration) {}
    }

    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn child(directory: &Path, kind: &str) {
        // Before any thread, just as in the executable's main.
        let flag = interrupt::install().unwrap();
        let keys = directory.join("keys");
        fs::create_dir_all(&keys).unwrap();
        let key_directory = directory.to_owned();
        let key = move |keys: &SecretDir| -> Result<SshKey, Stopped> {
            fs::write(
                key_directory.join("key-path"),
                keys.path().to_str().unwrap(),
            )
            .unwrap();
            Ok((
                keys.path().join("id_ed25519"),
                "ssh-ed25519 AAAAtest udpstp-bench\n".into(),
            ))
        };
        let factory = |args: &RoundArgs, setup: &Setup| -> Result<Built, String> {
            let mut env = plan::sealed_env([], "/usr/bin:/bin");
            env.insert(
                "FAKE_AWS_STATE".into(),
                directory.join("state.json").to_str().unwrap().into(),
            );
            let calls = aws_cli::Cli::new(
                std::env::var_os("UDPSTP_BENCH_FAKE_AWS").unwrap(),
                &args.region,
                Runner::new(flag.clone()),
            )
            .unwrap()
            .with_env(env)
            .with_limit(Duration::from_secs(20))
            .with_clock(aws_cli::Clock::Fixed {
                now: NOW,
                waits: Arc::new(Mutex::new(Vec::new())),
            })
            .with_recorder(
                record::Recorder::create(&directory.join("calls"), "interrupted", &[]).unwrap(),
            );
            Ok(Built::Rented(Box::new(cli::aws_provider(
                calls, args, setup,
            )?)))
        };
        let mut argv = vec![
            "udpstp-bench".into(),
            "round".into(),
            "--provider".into(),
            "aws".into(),
            "--matrix".into(),
            root()
                .join(format!("scripts/bench/tests/{kind}.toml"))
                .into_os_string(),
            "--udpstp".into(),
            "/build/udpstp".into(),
            "--out".into(),
            directory.join("out").into_os_string(),
            "--ledger".into(),
            directory.join("ledger.json").into_os_string(),
            "--home-cidr".into(),
            "203.0.113.7/32".into(),
            "--yes".into(),
        ];
        if kind == "scale-smoke" {
            argv.extend(
                [
                    "--prototype",
                    "/build/dpproto",
                    "--obudpst",
                    "build",
                    "--hold-irqs",
                    "--raise-buffers",
                ]
                .map(Into::into),
            );
        }
        let code = cli::main(
            argv,
            &mut Env {
                flag: flag.clone(),
                clock: &Fixed,
                out: &mut |line| println!("{line}"),
                err: &mut |line| eprintln!("{line}"),
                factory: Some(&factory),
                native_windows: false,
                xdg_state_home: None,
                home: directory.into(),
                pid: 4242,
                keys_in: Some(keys),
                round_suffix: Some("abcd".into()),
                ssh_key: Some(&key),
            },
        );
        assert!(flag.raised());
        fs::write(directory.join("status"), code.to_string()).unwrap();
    }

    struct Running {
        process: Child,
        release: PathBuf,
    }
    impl Drop for Running {
        fn drop(&mut self) {
            let _ = fs::write(&self.release, b"");
            if self.process.try_wait().ok().flatten().is_none() {
                let _ = killpg(Pid::from_raw(self.process.id() as i32), Signal::SIGKILL);
                let _ = self.process.wait();
            }
        }
    }

    fn json_file(path: &Path) -> Value {
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
    }

    fn run(directory: &Path, kind: &str) -> Value {
        use std::os::unix::process::CommandExt;
        fs::create_dir_all(directory).unwrap();
        let release = directory.join("release");
        let state = json!({"config": {"expiration": "2099-01-01T00:00:00Z",
            "pause_call": {"command": "run-instances", "n": 2, "until": release}}});
        fs::write(directory.join("state.json"), state.to_string()).unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.arg("--child");
        let fake = std::env::var_os("UDPSTP_BENCH_FAKE_AWS").unwrap();
        let env: IndexMap<_, _> = plan::sealed_env([], "/usr/bin:/bin");
        let log = fs::File::create(directory.join("child.log")).unwrap();
        command
            .args([directory.as_os_str(), kind.as_ref()])
            .env_clear()
            .envs(env)
            .env("UDPSTP_BENCH_FAKE_AWS", fake)
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log);
        let mut child = Running {
            process: command.spawn().unwrap(),
            release,
        };
        let began = Instant::now();
        loop {
            assert!(
                child.process.try_wait().unwrap().is_none(),
                "child stopped before the second launch; {}",
                directory.display()
            );
            let state = json_file(&directory.join("state.json"));
            let launches = state
                .get("calls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|c| {
                    c[1] == "run-instances" && !c.as_array().unwrap().contains(&json!("--dry-run"))
                })
                .count();
            if launches == 2 {
                break;
            }
            assert!(
                began.elapsed() < Duration::from_secs(30),
                "no second launch"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        kill(Pid::from_raw(child.process.id() as i32), Signal::SIGINT).unwrap();
        let sent = Instant::now();
        loop {
            if let Some(status) = child.process.try_wait().unwrap() {
                assert!(status.success(), "child: {status}; {}", directory.display());
                break;
            }
            assert!(
                sent.elapsed() < Duration::from_secs(30),
                "Round Teardown did not finish"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(fs::read_to_string(directory.join("status")).unwrap(), "1");
        let key = fs::read_to_string(directory.join("key-path")).unwrap();
        assert!(!Path::new(&key).exists(), "key directory survived");
        assert_eq!(fs::read_dir(directory.join("keys")).unwrap().count(), 0);
        {
            let records = record::read(&directory.join("calls/interrupted")).unwrap();
            let stopped = records
                .iter()
                .position(|r| r.status.is_none())
                .expect("the interrupted launch");
            assert!(
                records.len() > stopped + 3,
                "Round Teardown made real calls"
            );
            assert!(
                records[stopped + 1..].iter().all(|r| r.status == Some(0)),
                "no Round Teardown child was killed"
            );
        }
        let mut final_state = json_file(&directory.join("state.json"));
        final_state["config"]["pause_call"]["until"] = json!("<pause>");
        final_state
    }

    pub fn main() {
        let args: Vec<_> = std::env::args_os().collect();
        if args.get(1).is_some_and(|a| a == "--child") {
            child(Path::new(&args[2]), args[3].to_str().unwrap());
            return;
        }
        if std::env::var_os("UDPSTP_BENCH_FAKE_AWS").is_none() {
            println!("round_interrupt: opt in with UDPSTP_BENCH_FAKE_AWS");
            return;
        }
        let directory = SecretDir::create().unwrap();
        for kind in ["smoke", "scale-smoke"] {
            let rust = directory.path().join(format!("{kind}-rust"));
            let actual = run(&rust, kind);
            assert!(actual["groups"].as_object().unwrap().is_empty());
            assert!(
                actual["instances"]
                    .as_object()
                    .unwrap()
                    .values()
                    .all(|instance| instance["state"] == "terminated")
            );
            let ledger = json_file(&rust.join("ledger.json"));
            assert_eq!(ledger["rounds"][0]["state"], "torn down");
            println!(
                "{kind}: SIGINT finishes Round Teardown, terminates instances, closes the ledger and removes keys"
            );
        }
    }
}

fn main() {
    #[cfg(target_os = "linux")]
    linux::main();
}
