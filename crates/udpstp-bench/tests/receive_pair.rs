//! Receive probe pair wiring. The real Linux process observation is explicit
//! Nightly coverage and never establishes physical acceptance.
use clap::Parser;

#[test]
fn pair_cli_requires_explicit_pinned_payload_and_static_endpoints() {
    let parse = |extra: &[&str]| {
        udpstp_bench::cli::Command::try_parse_from(
            ["udpstp-bench", "receive-probe", "pair"]
                .into_iter()
                .chain(extra.iter().copied()),
        )
    };
    assert!(parse(&[]).is_err());
    let args = [
        "--sender-host",
        "local",
        "--receiver-host",
        "receiver",
        "--sender",
        "192.0.2.1:31001",
        "--receiver",
        "192.0.2.2:31002",
        "--sender-cpu",
        "1",
        "--receiver-cpu",
        "2",
        "--sampler-cpu",
        "3",
        "--measurement-tool",
        "endpoint",
        "--endpoint-sha256",
        &"a".repeat(64),
        "--endpoint-source",
        &"b".repeat(40),
        "--run-id",
        "77",
        "--count",
        "20",
        "--deadline-ms",
        "20000",
        "--mode",
        "header-only",
        "--out",
        "new-evidence",
    ];
    assert!(parse(&args).is_ok());
    assert!(parse(&["--provider", "aws"]).is_err());
}

#[test]
#[cfg(target_os = "linux")]
#[ignore = "bounded native host snapshots and sampler lifecycle, run explicitly for integration"]
fn tiny_native_pair_retains_results_and_confirms_sampler_stop() {
    use std::net::UdpSocket;
    use std::path::PathBuf;
    use udpstp_bench::receive_pair::{self, Args};
    let allowed = nix::sched::sched_getaffinity(nix::unistd::Pid::from_raw(0)).unwrap();
    let cpus: Vec<usize> = (0..nix::sched::CpuSet::count())
        .filter(|c| allowed.is_set(*c).unwrap_or(false))
        .collect();
    let core = |cpu| {
        let base = format!("/sys/devices/system/cpu/cpu{cpu}/topology");
        (
            std::fs::read_to_string(format!("{base}/physical_package_id")).unwrap(),
            std::fs::read_to_string(format!("{base}/core_id")).unwrap(),
        )
    };
    let receiver_cpu = cpus[0];
    let sampler_cpu = *cpus
        .iter()
        .find(|c| core(**c) != core(receiver_cpu))
        .expect("two permitted physical cores");
    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
    let v4 = |socket: &UdpSocket| match socket.local_addr().unwrap() {
        std::net::SocketAddr::V4(a) => a,
        _ => unreachable!(),
    };
    let (sender_address, receiver_address) = (v4(&sender), v4(&receiver));
    drop((sender, receiver));
    let executable = std::env::var_os("UDPSTP_RECEIVE_PAIR_ENDPOINT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_udpstp-bench")));
    let out = std::env::temp_dir().join(format!(
        "udpstp-pair-native-{}",
        bench_rounds::secrets::random_hex(8).unwrap()
    ));
    let args = Args {
        sender_host: "local".into(),
        receiver_host: "local".into(),
        sender: sender_address,
        receiver: receiver_address,
        sender_cpu: sampler_cpu,
        receiver_cpu,
        sampler_cpu,
        endpoint_sha256: udpstp_bench::receive_probe::hash_file(&executable).unwrap(),
        measurement_tool: executable,
        endpoint_source: "b40b52bd39ecf58cb610fb70c65c7bf64f54bcb8".into(),
        endpoint_features: "default".into(),
        endpoint_profiling: false,
        run_id: 250,
        count: 19,
        pps: 1000,
        deadline_ms: 20000,
        drain_ms: 50,
        start_delay_ms: 100,
        socket_buffer: 1048576,
        mode: udpstp_bench::receive_probe::ReceiveMode::HeaderOnly,
        readiness_ms: 10000,
        out: out.clone(),
        ssh_dir: None,
    };
    let code = receive_pair::run(&args, &bench_rounds::interrupt::Flag::new()).unwrap();
    let outcome: serde_json::Value =
        serde_json::from_slice(&std::fs::read(out.join("outcome.json")).unwrap()).unwrap();
    assert_eq!(code, 0, "{}", out.display());
    assert_eq!(outcome["endpoints_stopped"], true);
    assert_eq!(outcome["sampler_stopped"], true);
    assert_eq!(outcome["export_verified"], true);
    assert_eq!(outcome["physical_acceptance"], false);
    let result: serde_json::Value =
        serde_json::from_slice(&std::fs::read(out.join("receiver/result.json")).unwrap()).unwrap();
    assert_eq!(result["receiver"]["sequence"]["unique"], 19);
    assert!(out.join("receiver/capture.stop.json").is_file());
    eprintln!("retained native pair observation: {}", out.display());
    let mut interrupted = args.clone();
    interrupted.out = out.with_extension("interrupted");
    interrupted.count = 1;
    interrupted.start_delay_ms = 1000;
    let flag = bench_rounds::interrupt::Flag::new();
    let raised = flag.clone();
    let destination = interrupted.out.clone();
    let finished = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let complete = finished.clone();
    let interrupt = std::thread::spawn(move || {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(90);
        while std::time::Instant::now() < until
            && !complete.load(std::sync::atomic::Ordering::SeqCst)
        {
            let launched = std::fs::read_dir(&destination).ok().is_some_and(|entries| {
                entries.flatten().any(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .ends_with("-sender-launch.status")
                })
            });
            if launched {
                raised.raise("native pair interruption fixture");
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if !complete.load(std::sync::atomic::Ordering::SeqCst) {
            raised.raise("native fixture startup bound");
        }
    });
    let partial = receive_pair::run(&interrupted, &flag);
    finished.store(true, std::sync::atomic::Ordering::SeqCst);
    interrupt.join().unwrap();
    assert_eq!(partial.unwrap(), 1, "{}", interrupted.out.display());
    let partial_outcome: serde_json::Value =
        serde_json::from_slice(&std::fs::read(interrupted.out.join("outcome.json")).unwrap())
            .unwrap();
    assert_eq!(partial_outcome["operation_complete"], false);
    assert_eq!(partial_outcome["sampler_stopped"], true);
    assert_eq!(partial_outcome["export_verified"], true);
    let receiver_partial: serde_json::Value = serde_json::from_slice(
        &std::fs::read(interrupted.out.join("receiver/result.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(receiver_partial["complete"], false);
    eprintln!(
        "retained interrupted native observation: {}",
        interrupted.out.display()
    );
    // Integration evidence is intentionally retained for the coordinator's
    // export. Ordinary unit fixtures remove their own temporary observations.
}
