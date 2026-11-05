//! The coordinator's real RoundBody and teardown, with independently scripted
//! SSH/process observations. This is diagnostic tooling, not an RFC requirement.
use super::*;
use bench_rounds::host::{Completed, HostError, Stop};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, PartialEq)]
enum Case {
    Clean,
    Missing,
    Malformed,
    Mismatch,
    Stale,
    EarlyReceiver,
    EarlySender,
    StartupInterrupt,
    TrafficInterrupt,
    ExportInterrupt,
    StopUnknown,
    ExportFailure,
    Panic,
    ResultMismatch,
    ClosedSocket,
    UnknownTopology,
    UnreadableTopology,
}
struct State {
    case: Case,
    calls: Mutex<Vec<String>>,
    flag: Flag,
    args: Args,
}
struct Scripted {
    role: String,
    work: RemotePath,
    state: Arc<State>,
}
fn done(text: impl Into<String>) -> Result<Completed, Stop> {
    Ok(Completed {
        returncode: 0,
        stdout: text.into(),
        stderr: String::new(),
    })
}
impl Shell for Scripted {
    fn role(&self) -> &str {
        &self.role
    }
    fn address(&self) -> &str {
        "192.0.2.1"
    }
    fn public_address(&self) -> Option<&str> {
        None
    }
    fn destination(&self) -> &str {
        &self.role
    }
    fn work(&self) -> &RemotePath {
        &self.work
    }
    fn run(&self, script: &str, limit: Limit) -> Result<Completed, Stop> {
        assert!(limit.duration() <= Duration::from_secs(45));
        self.state.calls.lock().unwrap().push(script.into());
        let case = self.state.case;
        if script.starts_with("# pair-launch-receiver") {
            if case == Case::StartupInterrupt {
                self.state.flag.raise("fixture startup");
                return Err(Stop::Interrupted);
            }
            if case == Case::Panic {
                panic!("fixture launch panic");
            }
        }
        if script.starts_with("# pair-placement") {
            if case == Case::UnreadableTopology {
                return Ok(Completed {
                    returncode: 1,
                    stdout: String::new(),
                    stderr: "topology fact unreadable".into(),
                });
            }
            let package = if case == Case::UnknownTopology { -1 } else { 0 };
            return done(format!(
                "receiver_cpu=2 package={package} core=2\nsampler_cpu=3 package=0 core=3\n0-3\n2\n3\n"
            ));
        }
        if script.starts_with("# pair-readiness") {
            match case {
                Case::Missing => return done("pending\n"),
                Case::Malformed => return done("garbage"),
                Case::EarlyReceiver => {
                    return Ok(Completed {
                        returncode: 1,
                        stdout: String::new(),
                        stderr: "receiver exited".into(),
                    });
                }
                _ => {}
            }
            let a = &self.state.args;
            let ready = Readiness {
                version: 1,
                pid: 12345,
                result_id: format!("{}-12345-r-999", a.run_id),
                socket: crate::receive_probe::SocketFacts {
                    local: if case == Case::Mismatch {
                        "192.0.2.9:9".into()
                    } else {
                        a.receiver.to_string()
                    },
                    peer: a.sender.to_string(),
                    family: "IPv4".into(),
                    payload_bytes: 1172,
                    ipv4_bytes: 1200,
                    df: Some(true),
                    udp_gso: Some(0),
                    udp_gro: Some(false),
                    ..Default::default()
                },
            };
            let hash = if case == Case::Stale {
                "0".repeat(64)
            } else {
                a.endpoint_sha256.clone()
            };
            return done(format!(
                "12345\n{hash}\n{}\n---fds---\n{}\n---udp---\n sl local_address rem_address st tx_queue rx_queue tr tm->when retrnsmt uid timeout inode\n 0: 020200C0:791A 010200C0:7919 01 00000000:00000000 00:00000000 00000000 1000 0 998877 2\n---ready---\n{}",
                scripts::argv_hex(&scripts::endpoint_args(a, "receiver")),
                if case == Case::ClosedSocket {
                    ""
                } else {
                    "socket:[998877]"
                },
                serde_json::to_string(&ready).unwrap()
            ));
        }
        if script.starts_with("# pair-wait-sender") && case == Case::EarlySender {
            return done("7\n");
        }
        if script.starts_with("# pair-wait") {
            if case == Case::TrafficInterrupt {
                self.state.flag.raise("fixture traffic");
                return Err(Stop::Interrupted);
            }
            return done("0\n");
        }
        if script.starts_with("# pair-stop-receiver") && case == Case::StopUnknown {
            return Err(HostError::new("transport lost during stop").into());
        }
        if script.starts_with("# pair-capture-stop") {
            return done(r#"{"status":0,"stopped":true}"#);
        }
        done("")
    }
    fn put(&self, _: &Path, _: &RemotePath, _: u32) -> Result<(), Stop> {
        Ok(())
    }
    fn output(&self, _: &str, _: Limit) -> Result<Vec<u8>, Stop> {
        unreachable!("fetch override")
    }
    fn fetch_dir(&self, _: &RemotePath, local: &Path) -> Result<(), Stop> {
        self.state
            .calls
            .lock()
            .unwrap()
            .push(format!("EXPORT {}", self.role));
        fs::create_dir_all(local).unwrap();
        fs::write(local.join("partial.json"), b"partial observation\n").unwrap();
        if self.state.case == Case::ExportInterrupt {
            return Err(Stop::Interrupted);
        }
        if self.state.case == Case::ExportFailure {
            return Err(HostError::new("archive stream broke").into());
        }
        let a = &self.state.args;
        let mut w = a.workload(&self.role);
        w.out = "result.json".into();
        let result = ProbeResult {
            version: 1,
            role: self.role.clone(),
            result_id: if self.role == "receiver" {
                "77-12345-r-999".into()
            } else {
                "77-54321-s-999".into()
            },
            workload: w,
            mode: if self.role == "receiver" {
                Some(ReceiveMode::HeaderOnly)
            } else {
                None
            },
            binary_sha256: if self.state.case == Case::ResultMismatch {
                "0".repeat(64)
            } else {
                a.endpoint_sha256.clone()
            },
            hostname: "fixture".into(),
            kernel: "fixture".into(),
            profiling_enabled: false,
            socket: Default::default(),
            placement: Default::default(),
            start_raw_ns: 1,
            end_raw_ns: 2,
            elapsed_ns: 1,
            offered_pps: 0.0,
            accepted_pps: 0.0,
            unique_pps: 0.0,
            batch_datagrams: 1,
            batch_period_ns: 1000000,
            complete: true,
            errors: vec![],
            sender: None,
            receiver: None,
            timestamps: Default::default(),
            counters_before: None,
            counters_after: None,
        };
        receive_probe::write_json(&local.join("result.json"), &result).unwrap();
        let result_hash = receive_probe::hash_file(&local.join("result.json")).unwrap();
        let hash = receive_probe::hash_file(&local.join("partial.json")).unwrap();
        fs::write(
            local.join("SHA256SUMS"),
            format!("{hash}  ./partial.json\n{result_hash}  ./result.json\n"),
        )
        .unwrap();
        Ok(())
    }
}
struct FixtureProvider {
    state: Arc<State>,
    safe: Rc<Cell<bool>>,
    destroyed: bool,
}
impl Provider for FixtureProvider {
    fn name(&self) -> &'static str {
        "static"
    }
    fn rented(&self) -> bool {
        false
    }
    fn price_per_hour(&self, _: &str) -> Result<f64, ProviderError> {
        Ok(0.0)
    }
    fn check(&mut self, _: f64, _: UnixTime) -> Result<(), ProviderError> {
        Ok(())
    }
    fn reap(&mut self, _: UnixTime) -> Result<Vec<RoundId>, ProviderError> {
        Ok(vec![])
    }
    fn create(
        &mut self,
        _: &RoundId,
        _: UnixTime,
        roles: &[&str],
        _: &Access,
    ) -> Result<IndexMap<String, Box<dyn Shell>>, ProviderError> {
        Ok(roles
            .iter()
            .map(|role| {
                (
                    role.to_string(),
                    Box::new(Scripted {
                        role: role.to_string(),
                        work: RemotePath::new(format!("/tmp/fixture-{role}")).unwrap(),
                        state: self.state.clone(),
                    }) as Box<dyn Shell>,
                )
            })
            .collect())
    }
    fn destroy(&mut self, _: &RoundId) -> Result<Vec<Leftover>, ProviderError> {
        self.destroyed = self.safe.get();
        Ok(if self.destroyed {
            vec![]
        } else {
            vec![Leftover::new("workspace", "fixture")]
        })
    }
    fn leftovers(&mut self, _: &RoundId) -> Result<Vec<Leftover>, ProviderError> {
        Ok(vec![])
    }
    fn recheck_after(&self) -> Option<Duration> {
        None
    }
    fn manual_cleanup(&self, _: &RoundId) -> Vec<String> {
        vec![]
    }
}
struct FastClock;
impl Clock for FastClock {
    fn now(&self) -> UnixTime {
        1_800_000_000.0
    }
    fn sleep(&self, _: Duration) -> Result<(), bench_rounds::interrupt::Interrupted> {
        Ok(())
    }
    fn pause(&self, _: Duration) {}
}
fn args(out: PathBuf) -> Args {
    Args {
        sender_host: "sender".into(),
        receiver_host: "receiver".into(),
        sender: "192.0.2.1:31001".parse().unwrap(),
        receiver: "192.0.2.2:31002".parse().unwrap(),
        sender_cpu: 1,
        receiver_cpu: 2,
        sampler_cpu: 3,
        measurement_tool: PathBuf::from("unused"),
        endpoint_sha256: "a".repeat(64),
        endpoint_source: "b".repeat(40),
        endpoint_features: "default".into(),
        endpoint_profiling: false,
        run_id: 77,
        count: 20,
        pps: 1000,
        deadline_ms: 10000,
        drain_ms: 100,
        start_delay_ms: 100,
        socket_buffer: 1048576,
        mode: ReceiveMode::HeaderOnly,
        readiness_ms: 1,
        out,
        ssh_dir: None,
    }
}
fn fixture(case: Case) -> (round::Status, serde_json::Value, Vec<String>, bool, PathBuf) {
    let out = std::env::temp_dir().join(format!(
        "udpstp-pair-{}-{}",
        std::process::id(),
        bench_rounds::secrets::random_hex(6).unwrap()
    ));
    fs::create_dir(&out).unwrap();
    let a = args(out.clone());
    let flag = Flag::new();
    let state = Arc::new(State {
        case,
        calls: Mutex::new(vec![]),
        flag: flag.clone(),
        args: a.clone(),
    });
    let safe = Rc::new(Cell::new(false));
    let mut provider = FixtureProvider {
        state: state.clone(),
        safe: safe.clone(),
        destroyed: false,
    };
    let mut body = Pair::new(&a, &flag, &FastClock, safe);
    let status = round::execute(
        &mut body,
        &mut provider,
        Bracket {
            ledger: &out.join("ledger.json"),
            worst_case: 0.0,
            max_hours: 0.25,
            round: Some("fixture".into()),
            flag: &flag,
            clock: &FastClock,
            log: &Log::kept(),
            keys_in: None,
            alert: &mut |_| {},
        },
    );
    let outcome = serde_json::from_slice(&fs::read(out.join("outcome.json")).unwrap()).unwrap();
    let calls = state.calls.lock().unwrap().clone();
    (status, outcome, calls, provider.destroyed, out)
}
#[test]
fn coordinator_admits_one_matching_pair_then_stops_exports_and_removes() {
    let (status, out, calls, removed, directory) = fixture(Case::Clean);
    assert_eq!(status, round::Status::Done);
    assert!(removed);
    assert_eq!(out["physical_acceptance"], false);
    assert_eq!(out["operation_complete"], true);
    assert_eq!(out["export_verified"], true);
    let position = |s: &str| calls.iter().position(|v| v.starts_with(s)).unwrap();
    assert!(position("# pair-launch-receiver") < position("# pair-readiness"));
    assert!(position("# pair-readiness") < position("# pair-capture-start"));
    assert!(position("# pair-capture-start") < position("# pair-launch-sender"));
    assert!(position("# pair-stop-receiver") < position("EXPORT receiver"));
    let capture = &calls[position("# pair-capture-start")];
    assert!(capture.contains("taskset -c 3 ../endpoint capture start vm-probe 12345 85 -"));
    fs::remove_dir_all(directory).unwrap();
}
#[test]
fn bad_or_absent_readiness_never_admits_sender_and_retains_observations() {
    for case in [
        Case::Missing,
        Case::Malformed,
        Case::Mismatch,
        Case::Stale,
        Case::EarlyReceiver,
        Case::ClosedSocket,
        Case::UnknownTopology,
        Case::UnreadableTopology,
    ] {
        let (status, out, calls, removed, directory) = fixture(case);
        assert_eq!(status, round::Status::Stopped, "{case:?}");
        assert!(removed);
        assert_eq!(out["operation_complete"], false);
        assert!(!calls.iter().any(|s| s.starts_with("# pair-launch-sender")));
        assert!(directory.join("receiver/partial.json").is_file());
        fs::remove_dir_all(directory).unwrap();
    }
}
#[test]
fn sender_failure_interrupts_and_panic_still_reach_both_stops_and_exports() {
    for case in [
        Case::EarlySender,
        Case::StartupInterrupt,
        Case::TrafficInterrupt,
        Case::Panic,
    ] {
        let (status, out, calls, removed, directory) = fixture(case);
        assert_eq!(status, round::Status::Stopped, "{case:?}");
        assert!(removed);
        assert_eq!(out["operation_complete"], false);
        for marker in [
            "# pair-stop-sender",
            "# pair-stop-receiver",
            "# pair-capture-stop",
            "EXPORT sender",
            "EXPORT receiver",
        ] {
            assert!(
                calls.iter().any(|s| s.starts_with(marker)),
                "{case:?}: {marker}"
            );
        }
        fs::remove_dir_all(directory).unwrap();
    }
}
#[test]
fn unknown_stop_and_broken_or_interrupted_exports_leave_recoverable_workspaces() {
    for case in [
        Case::StopUnknown,
        Case::ExportFailure,
        Case::ExportInterrupt,
    ] {
        let (status, out, calls, removed, directory) = fixture(case);
        assert_eq!(status, round::Status::Left, "{case:?}");
        assert!(!removed);
        assert!(!out["errors"].as_array().unwrap().is_empty());
        assert!(calls.iter().any(|s| s == "EXPORT sender"));
        assert!(calls.iter().any(|s| s == "EXPORT receiver"));
        assert!(directory.join("receiver/partial.json").is_file());
        fs::remove_dir_all(directory).unwrap();
    }
}
#[test]
fn endpoint_command_is_the_explicit_finite_workload_without_rfc_matrix_flags() {
    let a = args("unused".into());
    assert_eq!(
        scripts::endpoint_args(&a, "receiver"),
        [
            "../endpoint",
            "receive-probe",
            "receive",
            "--bind",
            "192.0.2.2:31002",
            "--peer",
            "192.0.2.1:31001",
            "--run-id",
            "77",
            "--source",
            &"b".repeat(40),
            "--count",
            "20",
            "--pps",
            "1000",
            "--deadline-ms",
            "10000",
            "--drain-ms",
            "100",
            "--start-delay-ms",
            "100",
            "--cpu",
            "2",
            "--socket-buffer",
            "1048576",
            "--out",
            "result.json",
            "--mode",
            "header-only"
        ]
    );
    let launch = scripts::launch("receiver", &scripts::endpoint_args(&a, "receiver"), 10000);
    assert!(launch.contains("timeout --signal=TERM --kill-after=3s 85s"));
    let stop = scripts::stop("receiver");
    assert!(stop.contains("same \"$receipt\" || exit 1"));
    assert!(!stop.contains("pkill"));
    assert!(!stop.contains("killall"));
}
#[test]
fn production_static_adapter_defaults_to_retaining_every_role() {
    let flag = Flag::new();
    let hosts = ROLES.map(|r| {
        Host::local(
            r,
            "127.0.0.1",
            RemotePath::new(format!("/tmp/{r}")).unwrap(),
            Runner::new(flag.clone()),
        )
    });
    let mut p = RetainedHosts {
        inner: StaticHosts::new(hosts),
        safe_to_remove: Rc::new(Cell::new(false)),
    };
    assert_eq!(p.destroy(&"fixture".into()).unwrap().len(), 2);
    assert_eq!(p.leftovers(&"fixture".into()).unwrap().len(), 2);
}
#[test]
fn exported_hashes_must_match_and_manifest_paths_cannot_escape() {
    let dir = std::env::temp_dir().join(format!(
        "udpstp-pair-manifest-{}",
        bench_rounds::secrets::random_hex(6).unwrap()
    ));
    fs::create_dir(&dir).unwrap();
    fs::write(dir.join("partial.json"), "a").unwrap();
    for manifest in [
        format!("{}  ./partial.json\n", "0".repeat(64)),
        format!("{}  ./../elsewhere\n", "0".repeat(64)),
        String::new(),
    ] {
        fs::write(dir.join("SHA256SUMS"), manifest).unwrap();
        assert!(verify_manifest(&dir).is_err());
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn exported_endpoint_identity_mismatch_is_an_explicit_failed_observation() {
    let (_, out, _, removed, directory) = fixture(Case::ResultMismatch);
    assert!(
        removed,
        "complete verified bytes remain locally recoverable"
    );
    assert_eq!(out["operation_complete"], false);
    assert!(
        out["errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s.as_str().unwrap().contains("saved endpoint identity"))
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn invalid_plans_and_reused_output_are_refused_before_host_calls() {
    let dir = std::env::temp_dir().join(format!(
        "udpstp-pair-refusal-{}",
        bench_rounds::secrets::random_hex(6).unwrap()
    ));
    fs::create_dir(&dir).unwrap();
    let payload = dir.join("payload");
    fs::write(&payload, "fixture bytes").unwrap();
    let mut a = args(dir.clone());
    a.measurement_tool = payload;
    a.endpoint_sha256 = receive_probe::hash_file(&a.measurement_tool).unwrap();
    assert!(a.validate().is_ok());
    assert!(run(&a, &Flag::new()).is_err());
    assert!(!dir.join("plan.json").exists());
    a.sampler_cpu = a.receiver_cpu;
    assert!(a.validate().is_err());
    a.sampler_cpu += 1;
    a.endpoint_sha256 = "0".repeat(64);
    assert!(a.validate().is_err());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
#[cfg(target_os = "linux")]
fn topology_collection_retains_unknown_and_unreadable_facts_before_admission() {
    let root = std::env::temp_dir().join(format!(
        "udpstp-pair-topology-{}",
        bench_rounds::secrets::random_hex(6).unwrap()
    ));
    for scenario in [
        "valid",
        "negative-receiver-package",
        "negative-receiver-core",
        "negative-sampler-package",
        "negative-sampler-core",
        "nonnumeric",
        "missing",
        "unreadable",
        "same-core",
    ] {
        let work = root.join(scenario);
        let sys = work.join("sys");
        fs::create_dir_all(work.join("receiver")).unwrap();
        for cpu in [2, 3] {
            let topology = sys.join(format!("cpu{cpu}/topology"));
            fs::create_dir_all(&topology).unwrap();
            fs::write(topology.join("physical_package_id"), "0\n").unwrap();
            fs::write(topology.join("core_id"), format!("{cpu}\n")).unwrap();
            fs::write(topology.join("thread_siblings_list"), format!("{cpu}\n")).unwrap();
        }
        fs::write(sys.join("online"), "0-3\n").unwrap();
        let file = match scenario {
            "negative-receiver-package" => Some((2, "physical_package_id", "-1")),
            "negative-receiver-core" => Some((2, "core_id", "-1")),
            "negative-sampler-package" => Some((3, "physical_package_id", "-1")),
            "negative-sampler-core" => Some((3, "core_id", "-1")),
            "nonnumeric" => Some((3, "core_id", "unknown")),
            "same-core" => Some((3, "core_id", "2")),
            _ => None,
        };
        if let Some((cpu, name, value)) = file {
            fs::write(sys.join(format!("cpu{cpu}/topology/{name}")), value).unwrap();
        }
        let target = sys.join("cpu3/topology/core_id");
        if ["missing", "unreadable"].contains(&scenario) {
            fs::remove_file(&target).unwrap();
        }
        if scenario == "unreadable" {
            fs::create_dir(&target).unwrap();
        }
        let host = Host::local(
            "fixture",
            "127.0.0.1",
            RemotePath::new(work.to_str().unwrap()).unwrap(),
            Runner::new(Flag::new()),
        );
        let observation = host
            .run(
                &scripts::placement_from(2, 3, sys.to_str().unwrap()),
                Limit::secs(2),
            )
            .unwrap();
        let admitted = observation.success()
            && validate_placement(&args("unused".into()), &observation.stdout).is_ok();
        assert_eq!(admitted, scenario == "valid", "{scenario}: {observation:?}");
        if scenario.starts_with("negative") || ["nonnumeric", "same-core"].contains(&scenario) {
            assert!(work.join("receiver/placement.txt").is_file());
            assert!(!observation.stdout.is_empty());
        }
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[cfg(target_os = "linux")]
fn live_process_after_socket_close_has_no_owned_connected_udp_proof() {
    use std::net::UdpSocket;
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let peer = UdpSocket::bind("127.0.0.1:0").unwrap();
    let address = |s: &UdpSocket| match s.local_addr().unwrap() {
        std::net::SocketAddr::V4(a) => a,
        _ => unreachable!(),
    };
    let (local, remote) = (address(&socket), address(&peer));
    socket.connect(remote).unwrap();
    let fds = || {
        fs::read_dir("/proc/self/fd")
            .unwrap()
            .flatten()
            .filter_map(|e| fs::read_link(e.path()).ok())
            .map(|p| p.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert!(owns_udp_socket(
        &fds(),
        &fs::read_to_string("/proc/self/net/udp").unwrap(),
        local,
        remote
    ));
    drop(socket);
    assert!(
        Path::new("/proc/self/stat").exists(),
        "the endpoint process remains alive"
    );
    assert!(!owns_udp_socket(
        &fds(),
        &fs::read_to_string("/proc/self/net/udp").unwrap(),
        local,
        remote
    ));
}
