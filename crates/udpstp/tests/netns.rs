//! Seam 2c: Tests through network namespaces with a known bottleneck
//! (docs/spec.md Testing Decisions). Linux only; `scripts/netns-test.sh`
//! runs them inside an unprivileged user namespace. Anywhere else they are
//! skipped, or fail when `UDPSTP_NETNS_REQUIRED` is set.
//!
//! The topology is in `common/netns.rs`: a Client and a Server two routers
//! apart, with a token-bucket bottleneck between the routers that counts IP
//! bytes, so a Test's Maximum IP-Layer Capacity should be its rate. The
//! tests run one at a time, since they measure.
#![cfg(target_os = "linux")]

#[path = "common/capture.rs"]
mod capture;
mod common;
#[path = "common/key_file.rs"]
mod key_file;
#[path = "common/netns.rs"]
mod netns;

use std::io::{BufRead, BufReader};
use std::process::{Child, Output, Stdio};
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::Duration;

use common::run_bounded;
use key_file::KeyFile;
use netns::{Firewall, Nat, Node, Queue, Slots, Spec, Topology, addr};

/// One Test at a time: they measure capacity.
static SERIAL: Mutex<()> = Mutex::new(());

const UDPSTP: &str = env!("CARGO_BIN_EXE_udpstp");

fn keys(test: &str) -> KeyFile {
    KeyFile::new(
        test,
        "keys.toml",
        "[[key]]\nadmin_key_name = \"netns\"\nkey_id = 1\nkey_text = \"netns-key\"\noperator = true\n",
    )
}

/// A `udpstp server` in the Server's namespace, killed when dropped.
struct Server(Child);

impl Server {
    fn start(topology: &Topology, keys: &KeyFile) -> Self {
        Self::start_with(topology, keys, &[])
    }

    /// A Server for a test that runs Tests from one Client back to back: a
    /// Test's Setup Request can arrive before the Server has released the
    /// last Test, which one Test per Source would refuse, and these
    /// tests aren't about that limit.
    fn back_to_back(topology: &Topology, keys: &KeyFile) -> Self {
        Self::start_with(topology, keys, &["--tests-per-source", "2"])
    }

    fn start_with(topology: &Topology, keys: &KeyFile, args: &[&str]) -> Self {
        let mut child = topology
            .command(Node::Server, UDPSTP)
            .args(["server", "--key-file", keys.path()])
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the Server starts");
        let stdout = child.stdout.take().expect("piped");
        let (lines, received) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = lines.send(line);
            }
        });
        // Wait for both wildcards, then keep draining the output.
        let mut listening = 0;
        while listening < 2 {
            match received.recv_timeout(Duration::from_secs(10)) {
                Ok(line) if line.contains("listening") => listening += 1,
                Ok(_) => {}
                Err(_) => panic!("the Server didn't start listening"),
            }
        }
        std::thread::spawn(move || while received.recv().is_ok() {});
        Self(child)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Runs a Client in the Client's namespace, with JSON output.
fn client(topology: &Topology, keys: &KeyFile, args: &[&str]) -> Output {
    let mut command = topology.command(Node::Client, UDPSTP);
    command
        .args(["client", "--key-file", keys.path(), "--format", "json"])
        .args(args);
    run_bounded(command, Duration::from_secs(40))
}

/// The Client's JSON results.
fn results(output: &Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "the Client failed: {:?}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("JSON results")
}

/// The Maximum IP-Layer Capacity, in Mbit/s.
fn maximum(results: &serde_json::Value) -> f64 {
    results["Output"]["AtMax"]["MaxIPLayerCapacity"]
        .as_f64()
        .unwrap_or_else(|| panic!("a Maximum IP-Layer Capacity: {results:#}"))
}

/// The messages of the warnings the JSON results list under `code`.
fn warned(results: &serde_json::Value, code: &str) -> Vec<String> {
    results["udpstp"]["Warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|w| w["Code"] == code)
        .filter_map(|w| w["Message"].as_str().map(str::to_owned))
        .collect()
}

/// Checks a measured capacity against the bottleneck's rate: within 5%
/// when strict, otherwise between 70% and 103% of it.
fn check_capacity(what: &str, measured: f64, rate: f64) {
    let (low, high) = if netns::strict() {
        (0.95, 1.05)
    } else {
        (0.70, 1.03)
    };
    assert!(
        (low * rate..=high * rate).contains(&measured),
        "{what}: {measured} Mbit/s against a {rate} Mbit/s bottleneck (allowed {low}-{high} of it)"
    );
}

// docs/spec.md Seam 2(c) (a Test through a shaped bottleneck reports its
// capacity; two hops, a lower-MTU hop)
#[udpstp_req::req("RFC9097-6.3-L1", "RFC9946-5-1")]
#[test]
fn downstream_and_upstream_tests_measure_the_bottleneck() {
    if !netns::available("downstream_and_upstream_tests_measure_the_bottleneck") {
        return;
    }
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let topology = Topology::new("capacity", Spec::default());
    let keys = keys("capacity");
    let _server = Server::back_to_back(&topology, &keys);
    let rate = f64::from(topology.spec.rate_mbit);
    for direction in ["downstream", "upstream"] {
        let results = results(&client(
            &topology,
            &keys,
            &[
                "--direction",
                direction,
                "--test-interval",
                "10",
                addr::SERVER_V4,
            ],
        ));
        check_capacity(direction, maximum(&results), rate);
        // 1250-byte packets cross the 1400-byte hop with DF set.
        assert_eq!(results["udpstp"]["DontFragment"], true, "{direction}: DF");
    }
}

// docs/spec.md Seam 2(c) (dual-stack)
#[udpstp_req::req("RFC9946-6-L1")]
#[test]
fn a_test_over_ipv6_measures_the_bottleneck() {
    if !netns::available("a_test_over_ipv6_measures_the_bottleneck") {
        return;
    }
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let topology = Topology::new("ipv6", Spec::default());
    let keys = keys("ipv6");
    let _server = Server::start(&topology, &keys);
    let results = results(&client(
        &topology,
        &keys,
        &["--test-interval", "10", addr::SERVER_V6],
    ));
    check_capacity(
        "IPv6",
        maximum(&results),
        f64::from(topology.spec.rate_mbit),
    );
}

// docs/spec.md Seam 2(c) (IPv4-mapped cases): a Client given an
// IPv4-mapped address tests over IPv4
#[test]
fn a_client_given_an_ipv4_mapped_address_tests_over_ipv4() {
    if !netns::available("a_client_given_an_ipv4_mapped_address_tests_over_ipv4") {
        return;
    }
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let topology = Topology::new("mapped", Spec::default());
    let keys = keys("mapped");
    let _server = Server::start(&topology, &keys);
    let mapped = format!("::ffff:{}", addr::SERVER_V4);
    let output = client(&topology, &keys, &["--test-interval", "5", &mapped]);
    let results = results(&output);
    assert_eq!(
        results["Input"]["HostIPAddress"].as_str(),
        Some(addr::SERVER_V4),
        "the Server's address is IPv4: {results:#}"
    );
}

// ADR 0007 (replies leave from the address the Setup Request arrived on),
// through routers
#[test]
fn a_test_to_the_server_s_second_address_completes() {
    if !netns::available("a_test_to_the_server_s_second_address_completes") {
        return;
    }
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let topology = Topology::new("second", Spec::default());
    let keys = keys("second");
    let _server = Server::start(&topology, &keys);
    let results = results(&client(
        &topology,
        &keys,
        &["--test-interval", "5", addr::SERVER_V4_SECOND],
    ));
    assert_eq!(
        results["Input"]["HostIPAddress"].as_str(),
        Some(addr::SERVER_V4_SECOND)
    );
}

// IP-Layer Capacity counts IP headers: a Fixed-Rate Test above the
// bottleneck delivers exactly its rate, whatever the search would do
#[udpstp_req::req("RFC9097-5.3-L1")]
#[test]
fn a_fixed_rate_test_above_the_bottleneck_delivers_its_rate() {
    if !netns::available("a_fixed_rate_test_above_the_bottleneck_delivers_its_rate") {
        return;
    }
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let topology = Topology::new("fixed", Spec::default());
    let keys = keys("fixed");
    let _server = Server::start(&topology, &keys);
    let rate = f64::from(topology.spec.rate_mbit);
    let results = results(&client(
        &topology,
        &keys,
        &[
            "--fixed-rate",
            "60",
            "--test-interval",
            "5",
            addr::SERVER_V4,
        ],
    ));
    let highest =
        results["udpstp"]["CapacityModes"][0]["HighestIPLayerCapacity"]["IPLayerCapacity"]
            .as_f64()
            .unwrap_or_else(|| panic!("a Highest IP-Layer Capacity: {results:#}"));
    let (low, high) = if netns::strict() {
        (0.98, 1.02)
    } else {
        (0.70, 1.03)
    };
    assert!(
        (low * rate..=high * rate).contains(&highest),
        "row 60 through {rate} Mbit/s delivered {highest} Mbit/s"
    );
}

// RFC 9946 §5 (DF set on test traffic), ADR 0006: a hop whose MTU is
// below the Test's packet size is reported, not mistaken for low capacity;
//
#[test]
fn a_lower_mtu_hop_is_reported() {
    if !netns::available("a_lower_mtu_hop_is_reported") {
        return;
    }
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let topology = Topology::new("mtu", Spec::default());
    let keys = keys("mtu");
    let _server = Server::start_with(&topology, &keys, &["--traditional-mtu"]);
    // Upstream, so the Client sends the 1500-byte packets the 1400-byte
    // hop can't carry, and hears the router's ICMP errors.
    let output = client(
        &topology,
        &keys,
        &[
            "--traditional-mtu",
            "--direction",
            "upstream",
            "--test-interval",
            "5",
            addr::SERVER_V4,
        ],
    );
    let results: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "JSON results: {e}: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    let warnings = &results["udpstp"]["Warnings"];
    assert!(
        warnings
            .as_array()
            .is_some_and(|w| w.iter().any(|w| w["Code"] == "PathMtuExceeded")),
        "the path MTU is reported: {warnings}"
    );
}

// RFC 9946 §10 (MaxHops), ADR 0006: ICMP time exceeded from a MaxHops too
// small for the path is reported; the Setup Request itself expires;
#[test]
fn a_max_hops_too_small_for_the_path_is_reported() {
    if !netns::available("a_max_hops_too_small_for_the_path_is_reported") {
        return;
    }
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let topology = Topology::new("hops", Spec::default());
    let keys = keys("hops");
    let _server = Server::start(&topology, &keys);
    // Three hops away; two let a datagram reach the second router only.
    let output = client(
        &topology,
        &keys,
        &["--max-hops", "2", "--test-interval", "5", addr::SERVER_V4],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "the Test can't run: {stderr}");
    assert!(
        stderr.contains("ICMP time exceeded: --max-hops is too small for the path"),
        "the hop limit is reported: {stderr}"
    );
}

// docs/spec.md Seam 2(c) (NAT with endpoint-independent and port-dependent
// mappings); ADR 0007; . One socket per Test Connection sends the
// Setup Request to the Standard Port and the Activation Request and load
// to testPort, so behind a port-dependent NAT the Server sees them come
// from different ports; it connects on the first verified Activation
// Request, which carries the Test on.
#[test]
fn tests_complete_through_nat() {
    if !netns::available("tests_complete_through_nat") {
        return;
    }
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for (name, nat, same_port) in [
        ("nat-eim", Nat::EndpointIndependent, true),
        ("nat-pd", Nat::PortDependent, false),
    ] {
        let topology = Topology::new(
            name,
            Spec {
                nat: Some(nat),
                ..Spec::default()
            },
        );
        let keys = keys(name);
        let _server = Server::back_to_back(&topology, &keys);
        // Beyond the NAT, on rtr2's side of the middle link.
        let capture = capture::Capture::start(&topology, Node::Router2, "r2");
        for direction in ["downstream", "upstream"] {
            let results = results(&client(
                &topology,
                &keys,
                &[
                    "--direction",
                    direction,
                    "--test-interval",
                    "5",
                    addr::SERVER_V4,
                ],
            ));
            assert_eq!(
                results["udpstp"]["EndReason"], "Completed",
                "{name}, {direction}: {results:#}"
            );
        }
        let datagrams = capture.stop();
        let from_client = |id: [u8; 2], len: usize| -> Vec<std::net::SocketAddr> {
            datagrams
                .iter()
                .filter(|d| d.payload.starts_with(&id) && d.length == len)
                .filter(|d| d.destination.ip().to_string() == addr::SERVER_V4)
                .map(|d| d.source)
                .collect()
        };
        let setups = from_client([0xAC, 0xE1], 56);
        let activations = from_client([0xAC, 0xE2], 104);
        assert_eq!(setups.len(), 2, "{name}: {setups:?}");
        assert_eq!(activations.len(), 2, "{name}: {activations:?}");
        for (setup, activation) in setups.iter().zip(&activations) {
            assert_eq!(setup.ip().to_string(), "10.2.0.1", "{name}: translated");
            assert_eq!(
                setup.port() == activation.port(),
                same_port,
                "{name}: the Setup Request left from {setup}, the Activation Request from {activation}"
            );
        }
    }
}

// RFC 9946 §5.5 (the Null Request opens a stateful firewall for the
// Ephemeral Port); . A Server whose firewall lets in only the
// Standard Port and established flows runs Downstream and Upstream Tests,
// because the Null Request leaves from the Ephemeral Port first. The
// negative control drops it, and the Test fails: the firewall is strict
// enough to prove the point.
#[udpstp_req::req("RFC9946-5.5-2")]
#[test]
fn a_stateful_firewall_needs_only_the_standard_port_open() {
    if !netns::available("a_stateful_firewall_needs_only_the_standard_port_open") {
        return;
    }
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let topology = Topology::new(
        "fw",
        Spec {
            firewall: Some(Firewall::StandardPortOnly),
            ..Spec::default()
        },
    );
    let fw_keys = keys("fw");
    let server = Server::back_to_back(&topology, &fw_keys);
    for direction in ["downstream", "upstream"] {
        let results = results(&client(
            &topology,
            &fw_keys,
            &[
                "--direction",
                direction,
                "--test-interval",
                "5",
                addr::SERVER_V4,
            ],
        ));
        assert_eq!(
            results["udpstp"]["EndReason"], "Completed",
            "{direction}: {results:#}"
        );
    }
    drop(server);
    drop(topology);

    let topology = Topology::new(
        "fw-no-null",
        Spec {
            firewall: Some(Firewall::StandardPortOnlyWithoutNullRequest),
            ..Spec::default()
        },
    );
    let keys = keys("fw-no-null");
    let _server = Server::start(&topology, &keys);
    for direction in ["downstream", "upstream"] {
        let output = client(
            &topology,
            &keys,
            &[
                "--direction",
                direction,
                "--test-interval",
                "5",
                addr::SERVER_V4,
            ],
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success(),
            "{direction}: without the Null Request the firewall blocks the Test: {stderr}"
        );
        assert!(
            stderr.contains("Setup accepted"),
            "{direction}: the Standard Port was open, so the Setup Request got through: {stderr}"
        );
    }
}

// : through a bottleneck whose queue is managed, fq_codel under the
// token bucket, or CAKE's own shaper where the kernel has it, a Search
// Test's search still meets loss or growing delay past the path's limit
// and confirms Congestion, so the Client doesn't say it fell short
#[test]
fn a_search_through_a_managed_queue_confirms_congestion() {
    if !netns::available("a_search_through_a_managed_queue_confirms_congestion") {
        return;
    }
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut queues = vec![("fq-codel", Queue::FqCodel)];
    if netns::cake_available() {
        queues.push(("cake", Queue::Cake));
    } else {
        eprintln!(
            "a_search_through_a_managed_queue_confirms_congestion: CAKE skipped, the kernel has no sch_cake"
        );
    }
    for (name, queue) in queues {
        let topology = Topology::new(
            name,
            Spec {
                queue,
                ..Spec::default()
            },
        );
        let keys = keys(name);
        let _server = Server::back_to_back(&topology, &keys);
        for direction in ["downstream", "upstream"] {
            let results = results(&client(
                &topology,
                &keys,
                &[
                    "--direction",
                    direction,
                    "--test-interval",
                    "5",
                    addr::SERVER_V4,
                ],
            ));
            assert_eq!(
                results["udpstp"]["EndReason"], "Completed",
                "{name}, {direction}: {results:#}"
            );
            let never = warned(&results, "CongestionNeverConfirmed");
            assert!(
                never.is_empty(),
                "{name}, {direction}: {never:?}\n{results:#}"
            );
        }
    }
}

// : delay variation between lowThresh (30 ms) and upperThresh (90 ms)
// on the way to the Client, from Load PDUs released in bursts 40 to 70 ms
// apart, as a Wi-Fi or DOCSIS link aggregates them (netem's slots), makes
// its Trial Intervals Hold Intervals, which hold the search far below the
// 50 Mbit/s bottleneck: the Client says its search never confirmed
// Congestion, delay variation holding it
#[test]
fn delay_variation_that_holds_the_search_is_warned_of() {
    if !netns::available("delay_variation_that_holds_the_search_is_warned_of") {
        return;
    }
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let topology = Topology::new(
        "slots",
        Spec {
            slots: Some(Slots {
                min_ms: 40,
                max_ms: 70,
            }),
            ..Spec::default()
        },
    );
    let keys = keys("slots");
    let _server = Server::start(&topology, &keys);
    let results = results(&client(
        &topology,
        &keys,
        &["--test-interval", "5", addr::SERVER_V4],
    ));
    let never = warned(&results, "CongestionNeverConfirmed");
    assert!(
        matches!(never.as_slice(), [one] if one.contains("most Trial Intervals were Hold Intervals")),
        "{never:?}\n{results:#}"
    );
}
