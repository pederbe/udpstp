//! ADR 0008 receive diagnostics. These fixtures prove diagnostic accounting,
//! never physical rate, GRO feasibility or one-core acceptance.

use std::net::SocketAddrV4;
use std::path::PathBuf;

use udpstp_bench::receive_probe::*;

fn workload() -> Workload {
    Workload {
        bind: "192.0.2.1:30001".parse().unwrap(),
        peer: "192.0.2.2:30002".parse().unwrap(),
        run_id: 0x0102030405060708,
        source: "0123456789012345678901234567890123456789".into(),
        count: 1_000_000,
        pps: 1_000_000,
        deadline_ms: 3000,
        drain_ms: 100,
        start_delay_ms: 100,
        cpu: 2,
        socket_buffer: 1048576,
        out: PathBuf::from("unused.json"),
    }
}
fn fixture() -> (ProbeResult, ProbeResult) {
    let w = workload();
    let s = ProbeResult {
        version: 1,
        role: "sender".into(),
        result_id: "sender-a".into(),
        workload: w.clone(),
        mode: None,
        binary_sha256: "a".repeat(64),
        hostname: "sender".into(),
        kernel: "fixture".into(),
        profiling_enabled: false,
        socket: SocketFacts {
            local: w.bind.to_string(),
            peer: w.peer.to_string(),
            family: "IPv4".into(),
            payload_bytes: 1172,
            ipv4_bytes: 1200,
            ip_options: "fresh socket; no IP options installed".into(),
            df: Some(true),
            udp_gso: Some(0),
            ..SocketFacts::default()
        },
        placement: Placement::default(),
        start_raw_ns: 1000,
        end_raw_ns: 1_000_001_000,
        elapsed_ns: 1_000_000_000,
        offered_pps: 1_000_000.0,
        accepted_pps: 1_000_000.0,
        unique_pps: 0.0,
        batch_datagrams: 100,
        batch_period_ns: 100_000,
        complete: true,
        errors: vec![],
        sender: Some(SendCounts {
            planned: 1_000_000,
            offered: 1_000_000,
            accepted: 1_000_000,
            accepted_ranges: Ranges {
                values: vec![SequenceRange {
                    first: 0,
                    end: 1_000_000,
                }],
                truncated: false,
            },
            offered_per_second: vec![1_000_000],
            accepted_per_second: vec![1_000_000],
            ..SendCounts::default()
        }),
        receiver: None,
        timestamps: TimestampEvidence::default(),
        counters_before: None,
        counters_after: None,
    };
    let mut r = s.clone();
    r.role = "receiver".into();
    r.result_id = "receiver-a".into();
    r.hostname = "receiver".into();
    r.workload.bind = w.peer;
    r.workload.peer = w.bind;
    r.mode = Some(ReceiveMode::Gro);
    r.socket.local = r.workload.bind.to_string();
    r.socket.peer = r.workload.peer.to_string();
    r.socket.udp_gro = Some(true);
    r.sender = None;
    r.receiver = Some(ReceiveCounts {
        messages: 500_000,
        datagrams: 1_000_000,
        genuine_gro_messages: 500_000,
        aggregate_sizes: [(2344, 500_000)].into(),
        segment_sizes: [(1172, 1_000_000)].into(),
        group_sizes: [(2, 500_000)].into(),
        sequence: SequenceCounts {
            unique: 1_000_000,
            ..SequenceCounts::default()
        },
        unique_per_second: vec![1_000_000],
        socket_drops: Some(0),
        ..ReceiveCounts::default()
    });
    let distribution = Distribution {
        samples: 1_000_000,
        minimum_ns: Some(0),
        maximum_ns: Some(0),
        p1_ns: Some(0),
        p50_ns: Some(0),
        p99_ns: Some(0),
        p999_ns: Some(0),
        p99_minus_p1_ns: Some(0),
        range_ns: Some(0),
        quantization_ns: 1000,
        ..Distribution::default()
    };
    r.timestamps = TimestampEvidence {
        source: "fixture kernel receive timestamp".into(),
        sender_granularity: "shared sendmmsg batch".into(),
        receiver_granularity: "shared GRO message".into(),
        clock_pairs: 2,
        first_raw_ns: Some(1000),
        first_wall_ns: Some(2000),
        last_raw_ns: Some(1_000_001_000),
        last_wall_ns: Some(1_000_002_000),
        read_minus_kernel: distribution.clone(),
        offset_removed_receive_minus_sender: distribution,
        ..TimestampEvidence::default()
    };
    (s, r)
}

#[test]
fn header_is_independent_big_endian_fixture() {
    let expected = [
        85, 68, 80, 82, 67, 86, 48, 49, 1, 2, 3, 4, 5, 6, 7, 8, 0, 0, 0, 0, 0, 0, 0, 9, 0, 0, 0, 0,
        0, 0, 3, 232,
    ];
    assert_eq!(packet_header(0x0102030405060708, 9, 1000), expected);
    assert_eq!(decode_header(&expected, 0x0102030405060708), Ok((9, 1000)));
    assert!(decode_header(&expected, 7).is_err());
    assert!(decode_header(&expected[..31], 0x0102030405060708).is_err());
    let mut wrong = expected;
    wrong[0] = 0;
    assert!(decode_header(&wrong, 0x0102030405060708).is_err());
    assert_eq!((PAYLOAD, IP_BYTES), (1172, 1200));
}

#[test]
fn ordinary_and_gro_split_include_short_final_segment() {
    assert_eq!(
        segments(1172, None).unwrap().collect::<Vec<_>>(),
        vec![0..1172]
    );
    assert_eq!(
        segments(3516, Some(1172)).unwrap().collect::<Vec<_>>(),
        vec![0..1172, 1172..2344, 2344..3516]
    );
    assert_eq!(
        segments(2500, Some(1172)).unwrap().collect::<Vec<_>>(),
        vec![0..1172, 1172..2344, 2344..2500]
    );
    for (bytes, size) in [
        (0, None),
        (65536, None),
        (1172, Some(0)),
        (1172, Some(-1)),
        (1172, Some(1173)),
        (1172, Some(1)),
    ] {
        assert!(segments(bytes, size).is_err());
    }
}

#[test]
fn sequence_bitset_preserves_duplicates_reorder_and_unseen_tail() {
    let mut q = Sequences::new(10);
    for n in [2, 4, 3, 4, 7, 10] {
        q.insert(n);
    }
    let q = q.finish();
    assert_eq!(
        (q.unique, q.duplicates, q.reordered, q.out_of_range),
        (4, 1, 1, 1)
    );
    assert_eq!(
        (
            q.missing,
            q.missing_leading,
            q.missing_interior,
            q.missing_trailing
        ),
        (6, 2, 2, 2)
    );
    assert_eq!(
        q.missing_ranges.values,
        vec![
            SequenceRange { first: 0, end: 2 },
            SequenceRange { first: 5, end: 7 },
            SequenceRange { first: 8, end: 10 }
        ]
    );
    let empty = Sequences::new(3).finish();
    assert_eq!(
        (empty.missing, empty.missing_leading, empty.missing_trailing),
        (3, 3, 0)
    );
}

#[test]
fn fixed_histogram_retains_resolution_extrema_and_shared_stamps() {
    let mut h = Histogram::default();
    for n in [1500, 1500, 2500, 4500] {
        h.record(n);
    }
    let d = h.finish(false);
    assert_eq!(
        (d.samples, d.minimum_ns, d.maximum_ns),
        (4, Some(1500), Some(4500))
    );
    assert_eq!(
        (d.p1_ns, d.p50_ns, d.p99_ns, d.p99_minus_p1_ns, d.range_ns),
        (Some(1000), Some(1000), Some(4000), Some(3000), Some(3000))
    );
    let centered = h.finish(true);
    assert_eq!((centered.p50_ns, centered.p99_ns), (Some(0), Some(3000)));
    h.record(2_000_000_000);
    let d = h.finish(false);
    assert_eq!(d.samples, 5);
    assert!(d.histogram_overflow);
    assert_eq!(d.overflow_samples, 1);
    assert_eq!(d.p99_ns, None);
}

#[test]
fn exact_endpoint_conservation_does_not_claim_physical_acceptance() {
    let (s, r) = fixture();
    let e = evaluate(&s, &r, None);
    assert!(e.zero_missing_workload_datagrams);
    assert_eq!(e.missing_after_kernel_acceptance, Some(0));
    assert!(e.timestamp_results_available);
    assert!(!e.declared_placement_consistent);
    assert!(!e.eligible_for_physical_review);
    assert!(!e.physical_acceptance);
}

#[test]
fn trailing_loss_and_sender_unsent_are_distinct() {
    let (mut s, mut r) = fixture();
    let a = s.sender.as_mut().unwrap();
    a.accepted = 999_998;
    a.unsent = 2;
    a.accepted_per_second = vec![999_998];
    a.accepted_ranges.values = vec![SequenceRange {
        first: 0,
        end: 999_998,
    }];
    let b = r.receiver.as_mut().unwrap();
    b.sequence.unique = 999_997;
    b.sequence.missing = 3;
    b.sequence.missing_trailing = 3;
    b.unique_per_second = vec![999_997];
    b.sequence.missing_ranges.values = vec![SequenceRange {
        first: 999_997,
        end: 1_000_000,
    }];
    let e = evaluate(&s, &r, None);
    assert!(!e.zero_missing_workload_datagrams);
    assert_eq!(e.sender_unsent, Some(2));
    assert_eq!(e.missing_after_kernel_acceptance, Some(1));
}

#[test]
fn fixed_grid_and_partial_syscalls_keep_unsent_sequence_slots() {
    assert_eq!(scheduled_slot(250, 1_000_000, 0), Some((0, 100, 0)));
    assert_eq!(scheduled_slot(250, 1_000_000, 1), Some((100, 100, 100_000)));
    assert_eq!(scheduled_slot(250, 1_000_000, 2), Some((200, 50, 200_000)));
    assert_eq!(scheduled_slot(250, 1_000_000, 3), None);
    assert_eq!(scheduled_slot(3, 1000, 2), Some((2, 1, 2_000_000)));
    let mut sends = SendCounts {
        planned: 10,
        offered_per_second: vec![0],
        accepted_per_second: vec![0],
        ..SendCounts::default()
    };
    sends.record_attempt(0, 4, 0, Some(2));
    sends.record_attempt(4, 4, 0, None);
    sends.record_attempt(8, 2, 0, Some(2));
    assert_eq!(
        (
            sends.offered,
            sends.accepted,
            sends.partial_calls,
            sends.refused_calls
        ),
        (10, 4, 1, 1)
    );
    assert_eq!(
        sends.accepted_ranges.values,
        vec![
            SequenceRange { first: 0, end: 2 },
            SequenceRange { first: 8, end: 10 }
        ]
    );
}

#[test]
fn under_offered_malformed_mismatched_and_unaggregated_are_ineligible() {
    let (s, r) = fixture();
    for mutation in 0..8 {
        let (mut a, mut b) = (s.clone(), r.clone());
        match mutation {
            0 => {
                a.elapsed_ns = 1_000_001_000;
                a.end_raw_ns = a.start_raw_ns + a.elapsed_ns;
            }
            1 => a.sender.as_mut().unwrap().offered_per_second = vec![999_999, 1],
            2 => b.workload.run_id = 5,
            3 => b.receiver.as_mut().unwrap().genuine_gro_messages = 0,
            4 => b.receiver.as_mut().unwrap().data_truncated = 1,
            5 => b.receiver.as_mut().unwrap().control_truncated = 1,
            6 => b.receiver.as_mut().unwrap().sequence.unique = 1_000_001,
            _ => b.result_id = a.result_id.clone(),
        }
        let e = evaluate(&a, &b, None);
        assert!(!e.eligible_for_physical_review);
        assert!(e.reasons.len() > 1, "mutation {mutation}: {:?}", e.reasons);
    }
}

#[test]
fn missing_and_stepped_timestamps_do_not_destroy_packet_conservation() {
    let (s, r) = fixture();
    for stepped in [false, true] {
        let mut r = r.clone();
        if stepped {
            r.timestamps.clock_discontinuities = 1;
        } else {
            r.timestamps.missing = 1;
            r.timestamps.read_minus_kernel.samples -= 1;
        }
        let e = evaluate(&s, &r, None);
        assert!(e.zero_missing_workload_datagrams);
        assert!(!e.timestamp_results_available);
    }
}

#[test]
fn smt_siblings_count_as_one_core_but_other_cores_and_migration_do_not() {
    let (s, mut r) = fixture();
    let topology = vec![
        CpuIdentity {
            cpu: 2,
            package: 0,
            core: 1,
            siblings: vec![2, 6],
            online: true,
        },
        CpuIdentity {
            cpu: 6,
            package: 0,
            core: 1,
            siblings: vec![2, 6],
            online: true,
        },
        CpuIdentity {
            cpu: 3,
            package: 0,
            core: 2,
            siblings: vec![3, 7],
            online: true,
        },
        CpuIdentity {
            cpu: 7,
            package: 0,
            core: 2,
            siblings: vec![3, 7],
            online: true,
        },
    ];
    r.placement = Placement {
        requested_cpu: 2,
        allowed_before: vec![2, 3, 6],
        affinity_before: Some(vec![2]),
        affinity_after: Some(vec![2]),
        topology: topology.clone(),
        application_threads: 1,
        helper_cpus_before: Some(vec![3]),
        helper_cpus_after: Some(vec![3]),
    };
    let receipts = [
        "topology",
        "queue-before",
        "queue-after",
        "irq-before",
        "irq-after",
        "rps-rfs-before",
        "rps-rfs-after",
        "counters-before",
        "counters-after",
    ]
    .into_iter()
    .map(|n| {
        (
            n.into(),
            Receipt {
                path: n.into(),
                sha256: "a".repeat(64),
            },
        )
    })
    .collect();
    let mut h = HostEvidence {
        version: 1,
        run_id: s.workload.run_id,
        receiver_hostname: r.hostname.clone(),
        receiver_binary_sha256: r.binary_sha256.clone(),
        topology,
        receive_processing_cpus: vec![6],
        sampler_cpus: vec![3],
        sender_on_receiver_host: false,
        sender_cpus: vec![],
        queue_irq_rps_rfs_observed: true,
        receipts,
        counters: vec![CounterFact {
            name: "Udp.RcvbufErrors".into(),
            unit: "kernel messages".into(),
            before: None,
            after: None,
        }],
    };
    let e = evaluate(&s, &r, Some(&h));
    assert!(e.declared_placement_consistent);
    assert!(e.eligible_for_physical_review);
    assert!(!e.physical_acceptance);
    assert!(!e.counter_limits.is_empty());
    h.counters.push(CounterFact {
        name: "rx_queue_0_packets".into(),
        unit: "driver packets".into(),
        before: Some(100),
        after: Some(1100),
    });
    let e = evaluate(&s, &r, Some(&h));
    assert!(e.eligible_for_physical_review);
    assert!(
        e.counter_observations
            .iter()
            .any(|s| s.contains("rose by 1000 driver packets"))
    );
    let (mut unknown_h, mut unknown_r) = (h.clone(), r.clone());
    let outside = unknown_h.topology.iter_mut().find(|c| c.cpu == 3).unwrap();
    outside.package = -1;
    outside.core = -1;
    unknown_r.placement.topology = unknown_h.topology.clone();
    assert!(!evaluate(&s, &unknown_r, Some(&unknown_h)).declared_placement_consistent);
    let (mut offline_h, mut offline_r) = (h.clone(), r.clone());
    offline_h
        .topology
        .iter_mut()
        .find(|c| c.cpu == 6)
        .unwrap()
        .online = false;
    offline_r.placement.topology = offline_h.topology.clone();
    offline_h.receive_processing_cpus = vec![2];
    assert!(evaluate(&s, &offline_r, Some(&offline_h)).declared_placement_consistent);
    offline_h.topology.retain(|c| c.cpu != 6);
    offline_r.placement.topology = offline_h.topology.clone();
    assert!(!evaluate(&s, &offline_r, Some(&offline_h)).declared_placement_consistent);
    let mut unknown_r = r.clone();
    unknown_r.placement.affinity_after = None;
    assert!(!evaluate(&s, &unknown_r, Some(&h)).declared_placement_consistent);
    let mut unknown_r = r.clone();
    unknown_r.placement.helper_cpus_after = None;
    assert!(!evaluate(&s, &unknown_r, Some(&h)).declared_placement_consistent);
    h.receive_processing_cpus = vec![3];
    assert!(!evaluate(&s, &r, Some(&h)).declared_placement_consistent);
    h.receive_processing_cpus = vec![6];
    r.placement.affinity_after = Some(vec![2, 3]);
    assert!(!evaluate(&s, &r, Some(&h)).declared_placement_consistent);
}

#[test]
fn finite_input_rejects_impossible_or_unbounded_shapes() {
    let mut w = workload();
    assert!(w.validate().is_ok());
    w.count = 120_000_001;
    assert!(w.validate().is_err());
    w.count = 1_000_000;
    w.pps = 0;
    assert!(w.validate().is_err());
    w.pps = 1_000_000;
    w.bind = "0.0.0.0:30001".parse().unwrap();
    assert!(w.validate().is_err());
}

#[cfg(target_os = "linux")]
mod process {
    use super::*;
    use bench_rounds::{interrupt::Flag, process::Runner};
    use nix::sched::{CpuSet, sched_getaffinity};
    use nix::unistd::Pid;
    use std::fs;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    fn cpu() -> usize {
        let a = sched_getaffinity(Pid::from_raw(0)).unwrap();
        (0..CpuSet::count())
            .find(|&c| a.is_set(c).unwrap_or(false))
            .unwrap()
    }
    fn temporary() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "udpstp-receive-probe-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        p
    }
    fn address() -> SocketAddrV4 {
        match std::net::UdpSocket::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
        {
            std::net::SocketAddr::V4(address) => address,
            std::net::SocketAddr::V6(_) => unreachable!("bound IPv4"),
        }
    }
    fn command(role: &str, bind: SocketAddrV4, peer: SocketAddrV4, out: &PathBuf) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_udpstp-bench"));
        c.args([
            "receive-probe",
            role,
            "--bind",
            &bind.to_string(),
            "--peer",
            &peer.to_string(),
            "--run-id",
            "42",
            "--source",
            "0123456789012345678901234567890123456789",
            "--count",
            "20",
            "--pps",
            "1000",
            "--deadline-ms",
            "10000",
            "--drain-ms",
            "100",
            "--cpu",
            &cpu().to_string(),
            "--out",
        ])
        .arg(out);
        c
    }
    #[test]
    #[ignore = "native receive diagnostics; run explicitly or with Nightly"]
    fn tiny_native_pair_writes_saved_evaluation_without_rate_gate() {
        for mode in ["header-only", "gro"] {
            let dir = temporary();
            let (a, b) = (address(), address());
            let (s, r) = (dir.join("sender.json"), dir.join("receiver.json"));
            let mut receive = command("receive", b, a, &r);
            receive.args(["--mode", mode]);
            let runner = Runner::new(Flag::new());
            let rr = runner.clone();
            let thread =
                std::thread::spawn(move || rr.run(&mut receive, None, Duration::from_secs(15)));
            let ready = r.with_extension("ready.json");
            let started = Instant::now();
            while !ready.exists() && !r.exists() && started.elapsed() < Duration::from_secs(10) {
                std::thread::sleep(Duration::from_millis(10));
            }
            if ready.exists() {
                let sent = runner
                    .run(
                        &mut command("send", a, b, &s),
                        None,
                        Duration::from_secs(15),
                    )
                    .unwrap();
                assert!(sent.success(), "{}", sent.stderr_text().unwrap());
            }
            let received = thread.join().unwrap().unwrap();
            assert!(received.success(), "{}", received.stderr_text().unwrap());
            let receiver: ProbeResult = read_json(&r).unwrap();
            if ready.exists() {
                let sender: ProbeResult = read_json(&s).unwrap();
                assert!(sender.complete);
                assert!(receiver.complete);
                assert_eq!(sender.sender.as_ref().unwrap().accepted, 20);
                assert_eq!(receiver.receiver.as_ref().unwrap().sequence.unique, 20);
                let evaluation = dir.join("evaluation.json");
                let done = runner
                    .run(
                        Command::new(env!("CARGO_BIN_EXE_udpstp-bench"))
                            .args(["receive-probe", "evaluate", "--sender"])
                            .arg(&s)
                            .arg("--receiver")
                            .arg(&r)
                            .arg("--out")
                            .arg(&evaluation),
                        None,
                        Duration::from_secs(5),
                    )
                    .unwrap();
                assert!(done.success());
                let e: Evaluation = read_json(&evaluation).unwrap();
                assert!(e.zero_missing_workload_datagrams);
                assert!(!e.physical_acceptance);
                assert!(!e.eligible_for_physical_review);
                if mode == "gro" {
                    assert_eq!(receiver.socket.udp_gro, Some(true));
                }
            } else {
                assert_eq!(mode, "gro");
                assert!(receiver.errors.iter().any(|s| s.contains("unsupported")));
            }
            fs::remove_dir_all(dir).unwrap();
        }
    }
    #[test]
    #[ignore = "native receive diagnostics; run explicitly or with Nightly"]
    fn absent_peer_stops_at_explicit_deadline_and_preserves_failure() {
        let dir = temporary();
        let out = dir.join("sender.json");
        let done = Runner::new(Flag::new())
            .run(
                &mut command("send", address(), address(), &out),
                None,
                Duration::from_secs(15),
            )
            .unwrap();
        assert!(done.success());
        let result: ProbeResult = read_json(&out).unwrap();
        assert!(result.sender.is_some());
        assert!(!result.complete || result.sender.as_ref().unwrap().accepted == 20);
        assert!(!result.errors.is_empty() || result.sender.unwrap().accepted == 20);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[ignore = "native receive diagnostics; run explicitly or with Nightly"]
    fn real_signal_ends_receiver_and_preserves_partial_counts() {
        let dir = temporary();
        let out = dir.join("receiver.json");
        let mut command = command("receive", address(), address(), &out);
        command.args(["--mode", "header-only"]);
        let thread = std::thread::spawn(move || {
            Runner::new(Flag::new()).run(&mut command, None, Duration::from_secs(15))
        });
        let ready = out.with_extension("ready.json");
        let start = Instant::now();
        while !ready.exists() && start.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(10));
        }
        let ready: Readiness = read_json(&ready).expect("receiver became ready");
        nix::sys::signal::kill(
            Pid::from_raw(ready.pid as i32),
            nix::sys::signal::Signal::SIGTERM,
        )
        .unwrap();
        let done = thread.join().unwrap().unwrap();
        assert!(done.success(), "{}", done.stderr_text().unwrap());
        let result: ProbeResult = read_json(&out).unwrap();
        assert!(!result.complete);
        assert!(result.errors.iter().any(|e| e == "interrupted"));
        assert_eq!(result.receiver.unwrap().sequence.unique, 0);
        fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn incompatible_or_inconsistent_records_have_no_joined_conservation_claim() {
    let (s, r) = fixture();
    for mutation in 0..8 {
        let (mut s, mut r) = (s.clone(), r.clone());
        match mutation {
            0 => r.workload.run_id += 1,
            1 => r.workload.source = "f".repeat(40),
            2 => r.binary_sha256 = "b".repeat(64),
            3 => r.workload.peer = "192.0.2.9:30001".parse().unwrap(),
            4 => r.socket.payload_bytes = 100,
            5 => r.result_id = s.result_id.clone(),
            6 => s.sender.as_mut().unwrap().accepted_per_second = vec![999_999],
            _ => r.receiver.as_mut().unwrap().group_sizes.clear(),
        }
        let e = evaluate(&s, &r, None);
        assert!(!e.zero_missing_workload_datagrams, "mutation {mutation}");
        assert_eq!(
            e.missing_after_kernel_acceptance, None,
            "mutation {mutation}"
        );
        assert!(!e.diagnostic_complete, "mutation {mutation}");
    }
}

#[test]
fn portable_cli_evaluates_saved_fixtures_without_network_traffic() {
    use bench_rounds::{interrupt::Flag, process::Runner};
    use std::process::Command;
    use std::time::Duration;
    let dir = std::env::temp_dir().join(format!("udpstp-receive-portable-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    let (s, r) = fixture();
    let sp = dir.join("sender.json");
    let rp = dir.join("receiver.json");
    let out = dir.join("evaluation.json");
    std::fs::write(&sp, serde_json::to_vec(&s).unwrap()).unwrap();
    std::fs::write(&rp, serde_json::to_vec(&r).unwrap()).unwrap();
    let runner = Runner::new(Flag::new());
    let mut command = Command::new(env!("CARGO_BIN_EXE_udpstp-bench"));
    command
        .args(["receive-probe", "evaluate", "--sender"])
        .arg(&sp)
        .arg("--receiver")
        .arg(&rp)
        .arg("--out")
        .arg(&out);
    let done = runner
        .run(&mut command, None, Duration::from_secs(5))
        .unwrap();
    assert!(done.success(), "{}", done.stderr_text().unwrap());
    let e: Evaluation = read_json(&out).unwrap();
    assert!(e.diagnostic_complete);
    assert!(e.zero_missing_workload_datagrams);
    assert!(!e.physical_acceptance);
    let done = runner
        .run(
            Command::new(env!("CARGO_BIN_EXE_udpstp-bench")).args(["receive-probe", "--help"]),
            None,
            Duration::from_secs(5),
        )
        .unwrap();
    assert!(done.success());
    // The maintained benchmark CLI writes clap's help to stderr, with status0.
    assert!(done.stderr_text().unwrap().contains("evaluate"));
    std::fs::remove_dir_all(dir).unwrap();
}

fn clock(raw: u64, offset: i64) -> ClockPair {
    ClockPair {
        raw_before_ns: raw,
        wall_ns: raw as i64 + offset,
        raw_after_ns: raw,
    }
}
fn copied_packets(stamps: &[(u64, u64)]) -> Vec<u8> {
    let mut bytes = vec![0; stamps.len() * 1172];
    for (i, (sequence, stamp)) in stamps.iter().enumerate() {
        bytes[i * 1172..i * 1172 + 32].copy_from_slice(&packet_header(42, *sequence, *stamp));
    }
    bytes
}
fn ingest_pair(
    ingest: &mut MessageIngest,
    before: ClockPair,
    after: ClockPair,
    message: ReceiveMessage,
    bytes: &[u8],
    read: u64,
) {
    ingest.clocks.observe(before);
    ingest.clocks.observe(after);
    ingest.ingest(message, bytes, before, after, read, 0);
}

#[test]
fn actual_ingest_maps_constant_offset_and_shares_gro_timestamps() {
    let peer: SocketAddrV4 = "192.0.2.1:10000".parse().unwrap();
    for mode in [ReceiveMode::HeaderOnly, ReceiveMode::Gro] {
        let n = if mode == ReceiveMode::Gro { 2 } else { 1 };
        let mut ingest = MessageIngest::new(n, 42, peer, mode);
        let bytes = copied_packets(&[(0, 1000), (1, 1000)]);
        let message = ReceiveMessage {
            length: n as usize * 1172,
            peer: Some(peer),
            wall_ns: Some(1_001_500),
            gro: (n == 2).then_some(1172),
            data_truncated: mode == ReceiveMode::HeaderOnly,
            ..ReceiveMessage::default()
        };
        ingest_pair(
            &mut ingest,
            clock(1000, 1_000_000),
            clock(2000, 1_000_000),
            message,
            if n == 1 { &bytes[..32] } else { &bytes },
            2100,
        );
        let (counts, ts) = ingest.finish();
        assert_eq!(counts.sequence.unique, n);
        assert_eq!(counts.genuine_gro_messages, u64::from(n == 2));
        assert_eq!(ts.read_minus_kernel.samples, n);
        assert_eq!(ts.read_minus_kernel.minimum_ns, Some(600));
        assert_eq!(ts.read_minus_kernel.maximum_ns, Some(600));
        assert_eq!(ts.transit_anchor_ns, Some(500));
        assert_eq!(ts.offset_removed_receive_minus_sender.range_ns, Some(0));
        assert!(timestamp_available(&ts, n));
    }
}

#[test]
fn actual_clock_accounting_distinguishes_drift_step_and_wide_empty_pair() {
    let peer: SocketAddrV4 = "192.0.2.1:10000".parse().unwrap();
    let bytes = copied_packets(&[(0, 1_000_000)]);
    let mut ingest = MessageIngest::new(1, 42, peer, ReceiveMode::Gro);
    // A wide empty/startup pair has the same midpoint offset. It retains
    // uncertainty without invalidating the later narrow message observation.
    ingest.clocks.observe(ClockPair {
        raw_before_ns: 0,
        wall_ns: 3_500_000,
        raw_after_ns: 5_000_000,
    });
    let message = ReceiveMessage {
        length: 1172,
        peer: Some(peer),
        wall_ns: Some(7_500_500),
        ..ReceiveMessage::default()
    };
    ingest_pair(
        &mut ingest,
        clock(6_000_000, 1_000_000),
        clock(7_000_000, 1_001_000),
        message,
        &bytes,
        7_000_100,
    );
    let (counts, ts) = ingest.finish();
    assert_eq!(counts.sequence.unique, 1);
    assert_eq!(ts.clock_discontinuities, 0);
    assert_eq!(ts.wide_clock_pairs, 1);
    assert_eq!(ts.maximum_pair_span_ns, 5_000_000);
    assert_eq!(ts.maximum_mapping_uncertainty_ns, 1);
    assert_eq!(ts.read_minus_kernel.minimum_ns, Some(500_100));
    assert!(timestamp_available(&ts, 1));
    let mut ingest = MessageIngest::new(1, 42, peer, ReceiveMode::Gro);
    let wide = ClockPair {
        raw_before_ns: 0,
        wall_ns: 3_500_000,
        raw_after_ns: 5_000_000,
    };
    let message = ReceiveMessage {
        length: 1172,
        peer: Some(peer),
        wall_ns: Some(5_000_000),
        ..ReceiveMessage::default()
    };
    ingest_pair(
        &mut ingest,
        wide,
        clock(6_000_000, 1_000_000),
        message,
        &bytes,
        6_000_100,
    );
    let (_, ts) = ingest.finish();
    assert_eq!(ts.clock_discontinuities, 0);
    assert_eq!(ts.maximum_mapping_uncertainty_ns, 1_250_001);
    assert!(timestamp_available(&ts, 1));
    let mut ingest = MessageIngest::new(1, 42, peer, ReceiveMode::Gro);
    let message = ReceiveMessage {
        length: 1172,
        peer: Some(peer),
        wall_ns: Some(1_001_500),
        ..ReceiveMessage::default()
    };
    ingest_pair(
        &mut ingest,
        clock(1000, 1_000_000),
        clock(2000, 6_000_000),
        message,
        &bytes,
        2100,
    );
    let (counts, ts) = ingest.finish();
    assert_eq!(counts.sequence.unique, 1);
    assert_eq!(counts.sequence.missing, 0);
    assert_eq!(ts.clock_discontinuities, 1);
    assert!(!timestamp_available(&ts, 1));
}

#[test]
fn actual_ingest_preserves_control_truncation_metadata_errors_and_short_segment() {
    let peer: SocketAddrV4 = "192.0.2.1:10000".parse().unwrap();
    let before = clock(1000, 1_000_000);
    let after = clock(2000, 1_000_000);
    let bytes = copied_packets(&[(0, 1000), (1, 1000)]);
    let mut ingest = MessageIngest::new(2, 42, peer, ReceiveMode::Gro);
    let message = ReceiveMessage {
        length: 1172,
        peer: Some(peer),
        wall_ns: Some(1_001_500),
        control_truncated: true,
        ..ReceiveMessage::default()
    };
    ingest_pair(&mut ingest, before, after, message, &bytes[..1172], 2100);
    let message = ReceiveMessage {
        length: 1272,
        peer: Some(peer),
        wall_ns: Some(1_001_500),
        gro: Some(1172),
        ..ReceiveMessage::default()
    };
    ingest_pair(
        &mut ingest,
        clock(3000, 1_000_000),
        clock(4000, 1_000_000),
        message,
        &bytes[..1272],
        4100,
    );
    let message = ReceiveMessage {
        length: 1172,
        peer: Some(peer),
        gro: Some(0),
        ..ReceiveMessage::default()
    };
    ingest_pair(
        &mut ingest,
        clock(5000, 1_000_000),
        clock(6000, 1_000_000),
        message,
        &bytes[..1172],
        6100,
    );
    let message = ReceiveMessage {
        length: 1172,
        peer: Some(peer),
        data_truncated: true,
        ..ReceiveMessage::default()
    };
    ingest_pair(
        &mut ingest,
        clock(7000, 1_000_000),
        clock(8000, 1_000_000),
        message,
        &bytes[..32],
        8100,
    );
    let (counts, ts) = ingest.finish();
    assert_eq!(
        (
            counts.messages,
            counts.datagrams,
            counts.sequence.unique,
            counts.sequence.duplicates
        ),
        (4, 3, 1, 1)
    );
    assert_eq!(
        (
            counts.control_truncated,
            counts.wrong_size,
            counts.invalid_metadata,
            counts.data_truncated
        ),
        (1, 1, 1, 1)
    );
    assert_eq!(counts.sequence.missing_trailing, 1);
    assert_eq!(ts.missing, 1);
    assert!(!timestamp_available(&ts, 1));
}
