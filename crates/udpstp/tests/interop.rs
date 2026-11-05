//! Seam 2c: interop with OB-UDPST v9.1.0 through network namespaces
//! (docs/spec.md Testing Decisions). Linux only; run by
//! `scripts/netns-test.sh`, against the pinned build from
//! `scripts/obudpst-build.sh` in `$UDPSTP_OBUDPST_DIR` (default
//! `target/obudpst`).
//!
//! Results are compared with the known bottleneck, not with OB-UDPST's.
//! Control PDUs are compared field by field with OB-UDPST's in the same
//! role, ignoring per-run fields and the fields `docs/conformance/
//! interop-masks.toml` masks under the run's conditions.
#![cfg(target_os = "linux")]

#[path = "common/capture.rs"]
mod capture;
mod common;
#[path = "common/key_file.rs"]
mod key_file;
#[path = "common/netns.rs"]
mod netns;

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Output, Stdio};
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::Duration;

use capture::{Capture, Datagram};
use common::run_bounded;
use key_file::KeyFile;
use netns::{Node, Spec, Topology, addr};
use udpstp_wire::{ActivationPdu, SetupPdu};

static SERIAL: Mutex<()> = Mutex::new(());

const UDPSTP: &str = env!("CARGO_BIN_EXE_udpstp");

/// The Shared Key both ends use, in an OB-UDPST key file, which udpstp
/// reads too.
const KEY: &str = "interop-key";
const KEY_ID: &str = "5";

const MASKS: &str = include_str!("../../../docs/conformance/interop-masks.toml");

/// The pinned OB-UDPST build's `udpst`, or `None` (the test is skipped,
/// or fails if the tier is required).
fn obudpst(variant: &str) -> Option<PathBuf> {
    let dir = std::env::var_os("UDPSTP_OBUDPST_DIR").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/obudpst"),
        PathBuf::from,
    );
    let udpst = dir.join("v9.1.0").join(variant).join("udpst");
    if udpst.exists() {
        return Some(udpst);
    }
    assert!(
        std::env::var_os("UDPSTP_NETNS_REQUIRED").is_none(),
        "no OB-UDPST build at {}: run scripts/obudpst-build.sh",
        udpst.display()
    );
    eprintln!("skipped: no OB-UDPST build at {}", udpst.display());
    None
}

fn key_file(test: &str) -> KeyFile {
    KeyFile::new(test, "udpst.keys", &format!("{KEY_ID} {KEY}\n"))
}

/// An OB-UDPST Server in the Server's namespace, killed when dropped.
struct ObServer(Child);

impl ObServer {
    fn start(topology: &Topology, udpst: &Path, keys: &KeyFile, address: &str) -> Self {
        Self::start_with(topology, udpst, &["-K", keys.path(), address])
    }

    /// Starts it with these options; `-v` is added.
    fn start_with(topology: &Topology, udpst: &Path, args: &[&str]) -> Self {
        let mut child = topology
            .command(Node::Server, udpst.to_str().expect("UTF-8 path"))
            .arg("-v")
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the OB-UDPST Server starts");
        let stdout = child.stdout.take().expect("piped");
        let (lines, received) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = lines.send(line);
            }
        });
        loop {
            match received.recv_timeout(Duration::from_secs(10)) {
                Ok(line) if line.contains("Awaiting setup requests") => break,
                Ok(_) => {}
                Err(_) => panic!("the OB-UDPST Server didn't start"),
            }
        }
        std::thread::spawn(move || while received.recv().is_ok() {});
        Self(child)
    }
}

impl Drop for ObServer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn succeeded(what: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{what} failed: {:?}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The Setup and Activation Requests one Client sent, from the capture.
struct Requests {
    setup: SetupPdu,
    activation: ActivationPdu,
}

/// The Control PDUs in a capture, by the Client's port: requests the
/// Client sent, and the responses it got.
fn control(datagrams: &[Datagram], client_port: u16) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let is_control = |d: &&Datagram| {
        matches!(
            (d.payload.get(..2), d.payload.len()),
            (Some([0xAC, 0xE1]), 56) | (Some([0xAC, 0xE2]), 104)
        )
    };
    let requests = datagrams
        .iter()
        .filter(is_control)
        .filter(|d| d.source.port() == client_port)
        .map(|d| d.payload.clone())
        .collect();
    let responses = datagrams
        .iter()
        .filter(is_control)
        .filter(|d| d.destination.port() == client_port)
        .map(|d| d.payload.clone())
        .collect();
    (requests, responses)
}

/// The Client ports that sent a Setup Request, in order.
fn clients(datagrams: &[Datagram]) -> Vec<u16> {
    let mut ports = Vec::new();
    for d in datagrams {
        if d.payload.starts_with(&[0xAC, 0xE1])
            && d.payload.get(8) == Some(&1)
            && !ports.contains(&d.source.port())
        {
            ports.push(d.source.port());
        }
    }
    ports
}

fn requests(pdus: &[Vec<u8>]) -> Requests {
    let setup = pdus
        .iter()
        .find(|p| p.len() == 56)
        .map(|p| SetupPdu::decode(p).expect("a Setup Request"))
        .expect("a Setup Request");
    let activation = pdus
        .iter()
        .find(|p| p.len() == 104)
        .map(|p| ActivationPdu::decode(p).expect("an Activation Request"))
        .expect("an Activation Request");
    Requests { setup, activation }
}

/// A Setup PDU's fields, named as in udpstp-wire.
fn setup_fields(p: &SetupPdu) -> Vec<(&'static str, String)> {
    vec![
        ("protocol_version", p.protocol_version.to_string()),
        ("mc_index", p.mc_index.to_string()),
        ("mc_count", p.mc_count.to_string()),
        ("mc_ident", p.mc_ident.to_string()),
        ("cmd_request", p.cmd_request.to_string()),
        ("cmd_response", p.cmd_response.to_string()),
        ("max_bandwidth", p.max_bandwidth.to_string()),
        ("test_port", p.test_port.to_string()),
        ("modifier_bitmap", p.modifier_bitmap.to_string()),
        ("auth_mode", p.auth_mode.to_string()),
        ("auth_unix_time", p.auth_unix_time.to_string()),
        ("auth_digest", format!("{:02x?}", p.auth_digest)),
        ("key_id", p.key_id.to_string()),
        ("checksum", p.checksum.to_string()),
    ]
}

/// An Activation PDU's fields, named as in udpstp-wire.
fn activation_fields(p: &ActivationPdu) -> Vec<(&'static str, String)> {
    let r = &p.sending_rate;
    vec![
        ("protocol_version", p.protocol_version.to_string()),
        ("cmd_request", p.cmd_request.to_string()),
        ("cmd_response", p.cmd_response.to_string()),
        ("low_thresh", p.low_thresh.to_string()),
        ("upper_thresh", p.upper_thresh.to_string()),
        ("trial_int", p.trial_int.to_string()),
        ("test_int_time", p.test_int_time.to_string()),
        ("dscp_ecn", p.dscp_ecn.to_string()),
        ("sr_index_conf", p.sr_index_conf.to_string()),
        ("use_ow_del_var", p.use_ow_del_var.to_string()),
        ("high_speed_delta", p.high_speed_delta.to_string()),
        ("slow_adj_thresh", p.slow_adj_thresh.to_string()),
        ("seq_err_thresh", p.seq_err_thresh.to_string()),
        ("ignore_ooo_dup", p.ignore_ooo_dup.to_string()),
        ("modifier_bitmap", p.modifier_bitmap.to_string()),
        ("rate_adj_algo", p.rate_adj_algo.to_string()),
        ("sending_rate.tx_interval1", r.tx_interval1.to_string()),
        ("sending_rate.udp_payload1", r.udp_payload1.to_string()),
        ("sending_rate.burst_size1", r.burst_size1.to_string()),
        ("sending_rate.tx_interval2", r.tx_interval2.to_string()),
        ("sending_rate.udp_payload2", r.udp_payload2.to_string()),
        ("sending_rate.burst_size2", r.burst_size2.to_string()),
        ("sending_rate.udp_addon2", r.udp_addon2.to_string()),
        ("sub_int_period", p.sub_int_period.to_string()),
        ("auth_mode", p.auth_mode.to_string()),
        ("auth_unix_time", p.auth_unix_time.to_string()),
        ("auth_digest", format!("{:02x?}", p.auth_digest)),
        ("key_id", p.key_id.to_string()),
        ("checksum", p.checksum.to_string()),
    ]
}

/// Fields that differ from run to run: never compared.
const PER_RUN: &[&str] = &["mc_ident", "auth_unix_time", "auth_digest", "checksum"];

/// The fields `interop-masks.toml` masks for `pdu` when every one of a
/// mask's conditions is among `conditions`.
fn masked(pdu: &str, conditions: &[&str]) -> BTreeSet<String> {
    let masks: toml::Value = toml::from_str(MASKS).expect("the mask list");
    masks["mask"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["pdu"].as_str() == Some(pdu))
        .filter(|m| {
            m["when"]
                .as_array()
                .into_iter()
                .flatten()
                .all(|c| c.as_str().is_some_and(|c| conditions.contains(&c)))
        })
        .filter_map(|m| m["field"].as_str().map(str::to_string))
        .collect()
}

/// The fields in which ours differs from OB-UDPST's, other than per-run
/// and masked ones.
fn differences(
    ours: &[(&'static str, String)],
    theirs: &[(&'static str, String)],
    masked: &BTreeSet<String>,
) -> Vec<String> {
    ours.iter()
        .zip(theirs)
        .filter(|((name, _), _)| !PER_RUN.contains(name) && !masked.contains(*name))
        .filter(|((_, a), (_, b))| a != b)
        .map(|((name, a), (_, b))| format!("{name}: ours {a}, OB-UDPST's {b}"))
        .collect()
}

/// Whether a PDU OB-UDPST sent has every field outside those udpstp-wire
/// decodes (its reserved fields) zero: decoding and re-encoding it, which
/// writes reserved fields as zero, gives the same bytes.
fn reserved_zero(pdu: &[u8]) -> bool {
    match pdu.len() {
        56 => SetupPdu::decode(pdu).is_ok_and(|p| p.encode().as_slice() == pdu),
        104 => ActivationPdu::decode(pdu).is_ok_and(|p| p.encode().as_slice() == pdu),
        _ => false,
    }
}

// docs/spec.md Seam 2(c) (our Client against the pinned OB-UDPST Server,
// Downstream, mode 1: capacity against a shaped bottleneck; Control PDUs
// compared field by field with masks; OB-UDPST's reserved-field extensions
// stay zero);
#[udpstp_req::req("RFC9946-5.3-1")]
#[test]
fn our_client_completes_a_mode_1_downstream_test_against_obudpst() {
    const TEST: &str = "our_client_completes_a_mode_1_downstream_test_against_obudpst";
    if !netns::available(TEST) {
        return;
    }
    let Some(udpst) = obudpst("auth") else {
        return;
    };
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let topology = Topology::new("ob-down", Spec::default());
    let keys = key_file("ob-down");
    let _server = ObServer::start(&topology, &udpst, &keys, addr::SERVER_V4);
    let capture = Capture::start(&topology, Node::Router1, "r0");

    // OB-UDPST's own Client first, for its Control PDUs; useOwDelVar is
    // False, OB-UDPST's default.
    let mut theirs = topology.command(Node::Client, udpst.to_str().expect("UTF-8"));
    theirs.args([
        "-d",
        "-a",
        KEY,
        "-y",
        KEY_ID,
        "-t",
        "10",
        "-s",
        addr::SERVER_V4,
    ]);
    succeeded(
        "OB-UDPST's Client",
        &run_bounded(theirs, Duration::from_secs(40)),
    );

    let mut ours = topology.command(Node::Client, UDPSTP);
    ours.args(["client", "--key-file", keys.path(), "--auth-mode", "1"])
        .args([
            "--delay-var",
            "rtt",
            "--test-interval",
            "10",
            "--format",
            "json",
        ])
        .arg(addr::SERVER_V4);
    let output = run_bounded(ours, Duration::from_secs(40));
    succeeded("our Client", &output);
    let datagrams = capture.stop();

    let results: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON");
    assert_eq!(results["udpstp"]["EndReason"], "Completed", "{results:#}");
    let capacity = results["Output"]["AtMax"]["MaxIPLayerCapacity"]
        .as_f64()
        .unwrap_or_else(|| panic!("a Maximum IP-Layer Capacity: {results:#}"));
    let rate = f64::from(topology.spec.rate_mbit);
    let (low, high) = if netns::strict() {
        (0.90, 1.10)
    } else {
        (0.70, 1.10)
    };
    assert!(
        (low * rate..=high * rate).contains(&capacity),
        "{capacity} Mbit/s against a {rate} Mbit/s bottleneck"
    );

    let [their_port, our_port] = clients(&datagrams)[..] else {
        panic!("two Clients' Setup Requests: {:?}", clients(&datagrams));
    };
    let (their_requests, their_responses) = control(&datagrams, their_port);
    let (our_requests, our_responses) = control(&datagrams, our_port);
    let (theirs, ours) = (requests(&their_requests), requests(&our_requests));
    let conditions = [
        "downstream",
        "default-search",
        "no-max-bandwidth-stated",
        "consumer-key",
    ];
    let mut found = differences(
        &setup_fields(&ours.setup),
        &setup_fields(&theirs.setup),
        &masked("setup-request", &conditions),
    );
    found.extend(differences(
        &activation_fields(&ours.activation),
        &activation_fields(&theirs.activation),
        &masked("activation-request", &conditions),
    ));
    assert!(
        found.is_empty(),
        "our Client's requests differ from OB-UDPST's:\n{}",
        found.join("\n")
    );
    for response in their_responses.iter().chain(&our_responses) {
        assert!(
            reserved_zero(response),
            "OB-UDPST's reserved fields are zero: {response:02x?}"
        );
    }
}

// The comparison itself: a differing field is reported, a per-run or
// masked one isn't.
#[test]
fn a_differing_field_is_reported_unless_per_run_or_masked() {
    let ours = vec![
        ("mc_ident", "1".to_string()),
        ("sr_index_conf", "10".to_string()),
        ("use_ow_del_var", "1".to_string()),
    ];
    let theirs = vec![
        ("mc_ident", "2".to_string()),
        ("sr_index_conf", "20".to_string()),
        ("use_ow_del_var", "0".to_string()),
    ];
    let masked: BTreeSet<String> = ["use_ow_del_var".to_string()].into();
    assert_eq!(
        differences(&ours, &theirs, &masked),
        ["sr_index_conf: ours 10, OB-UDPST's 20"]
    );
    assert!(masked_fields_exist());
}

/// The mask list parses, and a mask whose conditions all hold applies.
fn masked_fields_exist() -> bool {
    masked("setup-request", &["upstream", "no-max-bandwidth-stated"]).contains("max_bandwidth")
        && !masked("setup-request", &["upstream"]).contains("max_bandwidth")
}

// docs/spec.md Seam 2(c) (our Client against the OB-UDPST Server, Upstream,
// mode 1, over IPv4 and IPv6); ADR 0004 (sizes are IPv4-based, an IPv6
// sender takes 20 bytes off); OB-UDPST's random-size flag;
#[udpstp_req::req("RFC9946-6.1-8")]
#[test]
fn our_client_completes_upstream_tests_against_obudpst() {
    const TEST: &str = "our_client_completes_upstream_tests_against_obudpst";
    if !netns::available(TEST) {
        return;
    }
    let Some(udpst) = obudpst("auth") else {
        return;
    };
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for (family, server, headers) in [("IPv4", addr::SERVER_V4, 28), ("IPv6", addr::SERVER_V6, 48)]
    {
        let name = format!("ob-up-{}", family.to_lowercase());
        let topology = Topology::new(&name, Spec::default());
        let keys = key_file(&name);
        let _server = ObServer::start(&topology, &udpst, &keys, server);
        let capture = Capture::start(&topology, Node::Router1, "r0");
        let mut ours = topology.command(Node::Client, UDPSTP);
        ours.args(["client", "--key-file", keys.path(), "--auth-mode", "1"])
            .args(["--direction", "upstream", "--delay-var", "rtt"])
            .args(["--test-interval", "10", "--format", "json", server]);
        let output = run_bounded(ours, Duration::from_secs(40));
        succeeded(&format!("{family}: our Client"), &output);
        let datagrams = capture.stop();

        let results: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON");
        assert_eq!(
            results["udpstp"]["EndReason"], "Completed",
            "{family}: {results:#}"
        );
        let capacity = results["Output"]["AtMax"]["MaxIPLayerCapacity"]
            .as_f64()
            .unwrap_or_else(|| panic!("{family}: a Maximum IP-Layer Capacity: {results:#}"));
        let rate = f64::from(topology.spec.rate_mbit);
        let (low, high) = if netns::strict() {
            (0.90, 1.10)
        } else {
            (0.70, 1.10)
        };
        assert!(
            (low * rate..=high * rate).contains(&capacity),
            "{family}: {capacity} Mbit/s against a {rate} Mbit/s bottleneck"
        );

        // OB-UDPST's search starts at its row 0, whose add-on has the
        // random-size flag.
        let response = datagrams
            .iter()
            .find(|d| {
                d.payload.starts_with(&[0xAC, 0xE2])
                    && d.length == 104
                    && d.source.ip().to_string() == server
            })
            .map(|d| ActivationPdu::decode(&d.payload).expect("an Activation Response"))
            .unwrap_or_else(|| panic!("{family}: an Activation Response"));
        assert_ne!(
            response.sending_rate.udp_addon2 & 0x8000_0000,
            0,
            "{family}: OB-UDPST's first srStruct has a random-size add-on: {:?}",
            response.sending_rate
        );
        let loads: Vec<usize> = datagrams
            .iter()
            .filter(|d| {
                d.payload.starts_with(&[0xBE, 0xEF]) && d.destination.ip().to_string() == server
            })
            .map(|d| d.length)
            .collect();
        let sizes: BTreeSet<usize> = loads.iter().copied().collect();
        assert!(
            loads.len() > 1000 && sizes.len() > 10,
            "{family}: Load PDUs of many sizes, the random add-ons among them: {} PDUs, {} sizes",
            loads.len(),
            sizes.len()
        );
        let largest = loads.iter().max().copied().unwrap_or_default();
        assert!(
            largest + headers <= 1250,
            "{family}: IP packets of at most 1250 bytes; the largest UDP payload was {largest}"
        );
    }
}

/// Our `udpstp server` in the Server's namespace, listening on `address`
/// only, killed when dropped.
struct OurServer(Child);

impl OurServer {
    fn start(topology: &Topology, keys: &KeyFile, address: &str) -> Self {
        Self::start_with(topology, &["--key-file", keys.path(), "--listen", address])
    }

    /// Starts it with these options, which must include one `--listen`.
    fn start_with(topology: &Topology, args: &[&str]) -> Self {
        let mut child = topology
            .command(Node::Server, UDPSTP)
            .arg("server")
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("our Server starts");
        let stdout = child.stdout.take().expect("piped");
        let (lines, received) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = lines.send(line);
            }
        });
        loop {
            match received.recv_timeout(Duration::from_secs(10)) {
                Ok(line) if line.contains("listening") => break,
                Ok(_) => {}
                Err(_) => panic!("our Server didn't start listening"),
            }
        }
        std::thread::spawn(move || while received.recv().is_ok() {});
        Self(child)
    }
}

impl Drop for OurServer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The Setup and Activation Responses a Client got.
struct Responses {
    setup: SetupPdu,
    activation: ActivationPdu,
}

fn responses(pdus: &[Vec<u8>]) -> Responses {
    let Requests { setup, activation } = requests(pdus);
    Responses { setup, activation }
}

/// Fields of a response that differ from run to run, besides `PER_RUN`:
/// the testPort the Server chose.
const PER_RUN_RESPONSE: &[&str] = &["test_port"];

// docs/spec.md Seam 2(c) (the OB-UDPST Client against our Server,
// Downstream and Upstream, mode 1: capacity against the bottleneck; our
// coercions accepted; Control PDUs compared with masks);
#[test]
fn the_obudpst_client_completes_tests_against_our_server() {
    const TEST: &str = "the_obudpst_client_completes_tests_against_our_server";
    if !netns::available(TEST) {
        return;
    }
    let Some(udpst) = obudpst("auth") else {
        return;
    };
    let udpst_path = udpst.to_str().expect("UTF-8").to_string();
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for (direction, flag) in [("downstream", "-d"), ("upstream", "-u")] {
        let name = format!("ob-client-{direction}");
        let topology = Topology::new(&name, Spec::default());
        let keys = key_file(&name);
        // OB-UDPST's Server on the second address, for its responses to
        // compare ours with; ours on the first.
        let _theirs = ObServer::start(&topology, &udpst, &keys, addr::SERVER_V4_SECOND);
        let _ours = OurServer::start(&topology, &keys, addr::SERVER_V4);
        let capture = Capture::start(&topology, Node::Router1, "r0");
        let run = |server: &str| {
            let mut client = topology.command(Node::Client, &udpst_path);
            client.args([
                flag, "-a", KEY, "-y", KEY_ID, "-t", "10", "-f", "json", server,
            ]);
            run_bounded(client, Duration::from_secs(40))
        };
        let with_theirs = run(addr::SERVER_V4_SECOND);
        succeeded(
            &format!("{direction}: against OB-UDPST's Server"),
            &with_theirs,
        );
        let with_ours = run(addr::SERVER_V4);
        succeeded(&format!("{direction}: against our Server"), &with_ours);
        let datagrams = capture.stop();

        let results: serde_json::Value =
            serde_json::from_slice(&with_ours.stdout).expect("OB-UDPST's JSON");
        let capacity = results["Output"]["AtMax"]["MaxIPLayerCapacity"]
            .as_f64()
            .unwrap_or_else(|| panic!("{direction}: OB-UDPST's AtMax: {results:#}"));
        let rate = f64::from(topology.spec.rate_mbit);
        let (low, high) = if netns::strict() {
            (0.90, 1.10)
        } else {
            (0.70, 1.10)
        };
        assert!(
            (low * rate..=high * rate).contains(&capacity),
            "{direction}: {capacity} Mbit/s against a {rate} Mbit/s bottleneck"
        );

        let [with_theirs_port, with_ours_port] = clients(&datagrams)[..] else {
            panic!(
                "{direction}: two Clients' Setup Requests: {:?}",
                clients(&datagrams)
            );
        };
        let theirs = responses(&control(&datagrams, with_theirs_port).1);
        let ours = responses(&control(&datagrams, with_ours_port).1);
        let conditions = [
            direction,
            "default-search",
            "no-max-bandwidth-stated",
            "consumer-key",
        ];
        let mut setup_masked = masked("setup-response", &conditions);
        setup_masked.extend(PER_RUN_RESPONSE.iter().map(|f| f.to_string()));
        let mut found = differences(
            &setup_fields(&ours.setup),
            &setup_fields(&theirs.setup),
            &setup_masked,
        );
        found.extend(differences(
            &activation_fields(&ours.activation),
            &activation_fields(&theirs.activation),
            &masked("activation-response", &conditions),
        ));
        assert!(
            found.is_empty(),
            "{direction}: our Server's responses differ from OB-UDPST's:\n{}",
            found.join("\n")
        );
    }
}

/// Checks a capacity against an expected rate: within `strict` of it when
/// strict, otherwise 70% to 110%.
fn check_rate(what: &str, capacity: f64, rate: f64, strict: f64) {
    let (low, high) = if netns::strict() {
        (1.0 - strict, 1.0 + strict)
    } else {
        (0.70, 1.10)
    };
    assert!(
        (low * rate..=high * rate).contains(&capacity),
        "{what}: {capacity} Mbit/s, expected {rate} Mbit/s"
    );
}

/// OB-UDPST's or our Maximum IP-Layer Capacity from JSON results.
fn at_max(what: &str, output: &Output) -> f64 {
    let results: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "{what}: JSON: {e}: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    results["Output"]["AtMax"]["MaxIPLayerCapacity"]
        .as_f64()
        .unwrap_or_else(|| panic!("{what}: a Maximum IP-Layer Capacity: {results:#}"))
}

// docs/spec.md Seam 2(c) (4-connection Tests in both pairings, their
// aggregate against the bottleneck); Multi-Connection Tests, RFC 9946 §4;
//
#[test]
fn four_connection_tests_work_in_both_pairings() {
    const TEST: &str = "four_connection_tests_work_in_both_pairings";
    if !netns::available(TEST) {
        return;
    }
    let Some(udpst) = obudpst("auth") else {
        return;
    };
    let udpst_path = udpst.to_str().expect("UTF-8").to_string();
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let topology = Topology::new("ob-multi", Spec::default());
    let keys = key_file("ob-multi");
    let _theirs = ObServer::start(&topology, &udpst, &keys, addr::SERVER_V4_SECOND);
    let _ours = OurServer::start(&topology, &keys, addr::SERVER_V4);
    let rate = f64::from(topology.spec.rate_mbit);
    for direction in ["downstream", "upstream"] {
        let mut ours = topology.command(Node::Client, UDPSTP);
        ours.args(["client", "--key-file", keys.path(), "--auth-mode", "1"])
            .args(["--direction", direction, "--delay-var", "rtt"])
            .args(["--test-connections", "4", "--test-interval", "10"])
            .args(["--format", "json", addr::SERVER_V4_SECOND]);
        let output = run_bounded(ours, Duration::from_secs(40));
        succeeded(&format!("{direction}: our Client, 4 connections"), &output);
        check_rate(
            &format!("{direction}: our Client"),
            at_max("ours", &output),
            rate,
            0.10,
        );

        let flag = if direction == "downstream" {
            "-d"
        } else {
            "-u"
        };
        let mut theirs = topology.command(Node::Client, &udpst_path);
        theirs
            .args([
                flag, "-C", "4", "-a", KEY, "-y", KEY_ID, "-t", "10", "-f", "json",
            ])
            .arg(addr::SERVER_V4);
        let output = run_bounded(theirs, Duration::from_secs(40));
        succeeded(
            &format!("{direction}: OB-UDPST's Client, 4 connections"),
            &output,
        );
        check_rate(
            &format!("{direction}: OB-UDPST's Client"),
            at_max("theirs", &output),
            rate,
            0.10,
        );
    }
}

// docs/spec.md Seam 2(c) (Unauthenticated Mode in both pairings with both
// ends opted in; Fixed-Rate Tests with an Operator Key checked against the
// known rate; an OB-UDPST Client's Fixed-Rate request without an Operator
// Key refused as documented: BADPARAM, which it exits with as 152);
#[udpstp_req::req("RFC9946-5.1-18", "RFC9097-2-4", "RFC9946-7.2.1-L2")]
#[test]
fn unauthenticated_and_fixed_rate_tests_interoperate() {
    const TEST: &str = "unauthenticated_and_fixed_rate_tests_interoperate";
    if !netns::available(TEST) {
        return;
    }
    let Some(udpst) = obudpst("auth") else {
        return;
    };
    let udpst_path = udpst.to_str().expect("UTF-8").to_string();
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let topology = Topology::new("ob-modes", Spec::default());
    let run = |program: &str, args: &[&str]| {
        let mut command = topology.command(Node::Client, program);
        command.args(args);
        run_bounded(command, Duration::from_secs(40))
    };

    // Unauthenticated: OB-UDPST's Server without keys, ours opted in.
    {
        let _theirs = ObServer::start_with(&topology, &udpst, &[addr::SERVER_V4_SECOND]);
        let _ours = OurServer::start_with(
            &topology,
            &["--allow-unauthenticated", "--listen", addr::SERVER_V4],
        );
        let output = run(
            UDPSTP,
            &[
                "client",
                "--unauthenticated",
                "--delay-var",
                "rtt",
                "--test-interval",
                "5",
                "--format",
                "json",
                addr::SERVER_V4_SECOND,
            ],
        );
        succeeded(
            "our unauthenticated Client against OB-UDPST's Server",
            &output,
        );
        let output = run(
            &udpst_path,
            &["-d", "-t", "5", "-f", "json", addr::SERVER_V4],
        );
        succeeded(
            "OB-UDPST's unauthenticated Client against our Server",
            &output,
        );
    }

    // Fixed-Rate at row 30, below the 50 Mbit/s bottleneck: the Test
    // delivers row 30's 30 Mbit/s.
    let operator = KeyFile::new(
        "ob-modes",
        "keys.toml",
        &format!(
            "[[key]]\nadmin_key_name = \"operator\"\nkey_id = {KEY_ID}\nkey_text = \"{KEY}\"\noperator = true\n"
        ),
    );
    let consumer = key_file("ob-modes-consumer");
    {
        let _theirs = ObServer::start(&topology, &udpst, &consumer, addr::SERVER_V4_SECOND);
        let _ours = OurServer::start(&topology, &operator, addr::SERVER_V4);
        let output = run(
            UDPSTP,
            &[
                "client",
                "--key-file",
                consumer.path(),
                "--auth-mode",
                "1",
                "--delay-var",
                "rtt",
                "--fixed-rate",
                "30",
                "--test-interval",
                "5",
                "--format",
                "json",
                addr::SERVER_V4_SECOND,
            ],
        );
        succeeded("our Fixed-Rate Client against OB-UDPST's Server", &output);
        check_rate("our Fixed-Rate Client", at_max("ours", &output), 30.0, 0.03);
        let output = run(
            &udpst_path,
            &[
                "-d",
                "-I",
                "30",
                "-a",
                KEY,
                "-y",
                KEY_ID,
                "-t",
                "5",
                "-f",
                "json",
                addr::SERVER_V4,
            ],
        );
        succeeded("OB-UDPST's Fixed-Rate Client with an Operator Key", &output);
        check_rate(
            "OB-UDPST's Fixed-Rate Client",
            at_max("theirs", &output),
            30.0,
            0.03,
        );
    }
    {
        let _ours = OurServer::start(&topology, &consumer, addr::SERVER_V4);
        let output = run(
            &udpst_path,
            &[
                "-d",
                "-I",
                "30",
                "-a",
                KEY,
                "-y",
                KEY_ID,
                "-t",
                "5",
                addr::SERVER_V4,
            ],
        );
        assert_eq!(
            output.status.code(),
            Some(152),
            "OB-UDPST's Client refused a Fixed-Rate Test (BADPARAM): {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}
