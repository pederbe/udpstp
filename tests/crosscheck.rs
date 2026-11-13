//! The simulator against a real impaired network (docs/spec.md Seam
//! S2c). Each scenario runs in the simulator (`udpstp-core`'s harness,
//! included here) and through the network-namespace tier's bottleneck
//! (`common/netns.rs`), and the two are compared on what the network model
//! decides: when the search first meets congestion, where it settles and
//! how far it swings, the loss there, and the delay variation the queue
//! adds.
//!
//! The tolerances come from stage 1's measured spread on a quiet native
//! Linux host: the netns runs repeat to
//! within a few tenths of a Mbit/s, and differ from the simulator mostly
//! in the first Sub-Interval and in how many Load PDUs the queue drops
//! (tbf's burst passes some that the simulator's byte queue drops). Every
//! run's figures go to `$UDPSTP_CROSSCHECK_OUT`, one JSON line each. It
//! runs only when `UDPSTP_CROSSCHECK` is set, through
//! `scripts/crosscheck.sh`: nightly, not in the pre-push hook.
#![cfg(target_os = "linux")]

#[path = "../src/protocol/mod.rs"]
#[allow(dead_code, unused_imports)]
mod protocol;

mod common;
#[path = "../src/tests/protocol/harness/mod.rs"]
mod harness;
#[path = "common/key_file.rs"]
mod key_file;
#[path = "common/netns.rs"]
mod netns;
#[path = "../src/wire/mod.rs"]
#[allow(dead_code, unused_imports)]
mod wire;

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Stdio};
use std::time::Duration;

use crate::protocol::Direction;
use common::run_bounded;
use harness::{DataEvent, RowRequest, Scenario};
use key_file::KeyFile;
use netns::{Node, Spec, Topology, addr};

const UDPSTP: &str = env!("CARGO_BIN_EXE_udpstp");

/// tbf's burst in the netns bottleneck (`common/netns.rs`), which its queue
/// holds on top of rate × latency.
const TBF_BURST: u64 = 32 * 1024;

/// One Sub-Interval as either side reports it.
#[derive(Debug, Clone, Copy)]
struct SubInterval {
    /// IP-Layer Capacity, Mbit/s.
    capacity: f64,
    loss: u64,
    delivered: u64,
    /// The largest one-way delay variation sample, ms.
    delay_variation_max: Option<f64>,
}

/// A scenario: one Test through one bottleneck.
#[derive(Debug, Clone, Copy)]
struct Case {
    name: &'static str,
    direction: Direction,
    rate_mbit: u32,
    queue_ms: u32,
    /// Added each way on the Client's link.
    delay_ms: u32,
    /// A Fixed-Rate Test at this row, or a Search Test.
    fixed_row: Option<u16>,
}

const CASES: &[Case] = &[
    Case {
        name: "search-down-50M-q200-d5",
        direction: Direction::Downstream,
        rate_mbit: 50,
        queue_ms: 200,
        delay_ms: 5,
        fixed_row: None,
    },
    Case {
        name: "search-up-50M-q200-d5",
        direction: Direction::Upstream,
        rate_mbit: 50,
        queue_ms: 200,
        delay_ms: 5,
        fixed_row: None,
    },
    Case {
        name: "search-down-50M-q200-d20",
        direction: Direction::Downstream,
        rate_mbit: 50,
        queue_ms: 200,
        delay_ms: 20,
        fixed_row: None,
    },
    Case {
        name: "search-down-50M-q20-d5",
        direction: Direction::Downstream,
        rate_mbit: 50,
        queue_ms: 20,
        delay_ms: 5,
        fixed_row: None,
    },
    Case {
        name: "fixed-down-45of50M-q200-d5",
        direction: Direction::Downstream,
        rate_mbit: 50,
        queue_ms: 200,
        delay_ms: 5,
        fixed_row: Some(45),
    },
    Case {
        name: "fixed-down-49of50M-q200-d5",
        direction: Direction::Downstream,
        rate_mbit: 50,
        queue_ms: 200,
        delay_ms: 5,
        fixed_row: Some(49),
    },
];

/// The simulator's buffer for a tbf of `rate_mbit` and `queue_ms`: tc sets
/// tbf's limit to rate × latency + burst, in bytes at the IP layer (the
/// netns tier's `stab overhead -14` takes off the Ethernet header), as the
/// simulator counts.
fn sim_buffer(case: &Case) -> u64 {
    u64::from(case.rate_mbit) * 1_000_000 / 8 * u64::from(case.queue_ms) / 1000 + TBF_BURST
}

fn run_sim(case: &Case, seed: u64) -> Vec<SubInterval> {
    let mut scenario = Scenario::new(seed)
        .client_direction(case.direction)
        .bottleneck(u64::from(case.rate_mbit) * 1_000_000, sim_buffer(case))
        .path_delay(Duration::from_millis(case.delay_ms.into()), Duration::ZERO);
    scenario = match case.fixed_row {
        Some(row) => scenario
            .operator(7, b"crosscheck")
            .client_requests_row(RowRequest::FixedRate(row)),
        None => scenario
            .server_allows_unauthenticated()
            .client_unauthenticated(),
    };
    let outcome = scenario.run_for(Duration::from_millis(11_500));
    outcome
        .client_data_events
        .iter()
        .filter_map(|(_, e)| match e {
            DataEvent::SubIntervalMeasured(m) => Some(SubInterval {
                capacity: m.ip_layer_capacity as f64 / 1e6,
                loss: u64::from(m.loss),
                delivered: u64::from(m.datagrams),
                delay_variation_max: m.delay_variation.map(|d| f64::from(d.max)),
            }),
            _ => None,
        })
        .collect()
}

/// A `udpstp server` in the Server's namespace, killed when dropped.
struct Server(Child);

impl Server {
    fn start(topology: &Topology, keys: &KeyFile) -> Self {
        let mut child = topology
            .command(Node::Server, UDPSTP)
            .args(["server", "--key-file", keys.path()])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the Server starts");
        let stdout = child.stdout.take().expect("piped");
        let mut lines = BufReader::new(stdout).lines();
        let mut listening = 0;
        while listening < 2 {
            match lines.next() {
                Some(Ok(line)) if line.contains("listening") => listening += 1,
                Some(Ok(_)) => {}
                _ => panic!("the Server didn't start listening"),
            }
        }
        std::thread::spawn(move || for _ in lines {});
        Self(child)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The bottleneck qdiscs' drop counts, both directions.
fn tbf_drops(topology: &Topology) -> u64 {
    [(Node::Router1, "r1"), (Node::Router2, "r2")]
        .into_iter()
        .map(|(node, dev)| {
            let output = std::process::Command::new("ip")
                .args([
                    "netns",
                    "exec",
                    &topology.ns(node),
                    "tc",
                    "-s",
                    "qdisc",
                    "show",
                    "dev",
                    dev,
                ])
                .output()
                .expect("tc");
            let text = String::from_utf8_lossy(&output.stdout);
            text.split("dropped ")
                .nth(1)
                .and_then(|rest| rest.split(',').next())
                .and_then(|n| n.trim().parse::<u64>().ok())
                .unwrap_or(0)
        })
        .sum()
}

struct NetnsRun {
    sub_intervals: Vec<SubInterval>,
    tbf_drops: u64,
    receive_buffer_overflows: u64,
}

fn run_netns(topology: &Topology, keys: &KeyFile, case: &Case) -> NetnsRun {
    let direction = match case.direction {
        Direction::Downstream => "downstream",
        Direction::Upstream => "upstream",
    };
    let before = tbf_drops(topology);
    let mut command = topology.command(Node::Client, UDPSTP);
    command.args(["client", "--key-file", keys.path(), "--format", "json"]);
    command.args(["--direction", direction, "--test-interval", "10"]);
    let row;
    if let Some(fixed) = case.fixed_row {
        row = fixed.to_string();
        command.args(["--fixed-rate", &row]);
    }
    command.arg(addr::SERVER_V4);
    let output = run_bounded(command, Duration::from_secs(40));
    assert!(
        output.status.success(),
        "{}: {}",
        case.name,
        String::from_utf8_lossy(&output.stderr)
    );
    let results: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON");
    let sub_intervals = results["Output"]["IncrementalResult"]
        .as_array()
        .expect("Sub-Intervals")
        .iter()
        .map(|s| SubInterval {
            capacity: s["IPLayerCapacity"].as_f64().unwrap_or(0.0),
            loss: s["LossCount"].as_u64().unwrap_or(0),
            // DeliveredPercent and LossCount give the Load PDUs delivered.
            delivered: {
                let loss = s["LossCount"].as_f64().unwrap_or(0.0);
                let percent = s["DeliveredPercent"].as_f64().unwrap_or(100.0);
                if percent >= 100.0 {
                    0
                } else {
                    (loss * percent / (100.0 - percent)).round() as u64
                }
            },
            delay_variation_max: s["PDVMax"].as_f64().map(|seconds| seconds * 1000.0),
        })
        .collect();
    let receive_buffer_overflows = results["udpstp"]["Warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|w| w["Code"] == "ReceiveBufferOverflows")
        .filter_map(|w| w["Count"].as_u64())
        .sum();
    NetnsRun {
        sub_intervals,
        tbf_drops: tbf_drops(topology) - before,
        receive_buffer_overflows,
    }
}

/// What the comparison looks at, from one run's Sub-Intervals.
fn summary(rate: f64, subs: &[SubInterval]) -> serde_json::Value {
    let caps: Vec<f64> = subs.iter().map(|s| s.capacity).collect();
    // The first Sub-Interval to reach 90 % of the bottleneck.
    let reached = caps.iter().position(|&c| c >= 0.9 * rate).map(|i| i + 1);
    // The first with loss: where the search first met the bottleneck's
    // drops.
    let first_loss = subs.iter().position(|s| s.loss > 0).map(|i| i + 1);
    // The last half of the Test: where the search settled, and its swing.
    let tail: Vec<f64> = caps.iter().skip(caps.len() / 2).copied().collect();
    let mut sorted = tail.clone();
    sorted.sort_by(f64::total_cmp);
    let median = sorted.get(sorted.len() / 2).copied();
    let swing = sorted.last().zip(sorted.first()).map(|(hi, lo)| hi - lo);
    let (at_max, max) = caps
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .map_or((0, 0.0), |(i, c)| (i, *c));
    let loss_at_max = subs.get(at_max).map_or(0.0, |s| {
        let sent = s.loss + s.delivered;
        if sent == 0 {
            0.0
        } else {
            s.loss as f64 / sent as f64
        }
    });
    let dv_max = subs
        .iter()
        .filter_map(|s| s.delay_variation_max)
        .fold(None, |m: Option<f64>, d| Some(m.map_or(d, |m| m.max(d))));
    serde_json::json!({
        "capacities": caps,
        "loss": subs.iter().map(|s| s.loss).collect::<Vec<_>>(),
        "reached_90pct": reached,
        "first_loss": first_loss,
        "settled_median": median,
        "settled_swing": swing,
        "max": max,
        "loss_ratio_at_max": loss_at_max,
        "delay_variation_max_ms": dv_max,
    })
}

// docs/spec.md Seam S2c;  (the simulator agrees with a real impaired
// network on what its model decides)
#[test]
fn the_simulator_agrees_with_netem() {
    if std::env::var_os("UDPSTP_CROSSCHECK").is_none() {
        eprintln!("the cross-check runs only with UDPSTP_CROSSCHECK set");
        return;
    }
    if !netns::available("the_simulator_agrees_with_netem") {
        return;
    }
    let repeats: u64 = std::env::var("UDPSTP_CROSSCHECK_REPEATS")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(3);
    let only = std::env::var("UDPSTP_CROSSCHECK_ONLY").ok();
    let mut out: Box<dyn Write> = match std::env::var("UDPSTP_CROSSCHECK_OUT") {
        Ok(path) => Box::new(std::fs::File::create(path).expect("the output file")),
        Err(_) => Box::new(std::io::stderr()),
    };
    let mut failures = Vec::new();
    let keys = KeyFile::new(
        "crosscheck",
        "keys.toml",
        "[[key]]\nadmin_key_name = \"crosscheck\"\nkey_id = 1\nkey_text = \"crosscheck-key\"\noperator = true\n",
    );
    for case in CASES {
        if only.as_deref().is_some_and(|o| !case.name.contains(o)) {
            continue;
        }
        let rate = f64::from(case.rate_mbit);
        // The simulator is deterministic: one run is all of them.
        let sim = summary(rate, &run_sim(case, 1));
        let line = serde_json::json!({
            "case": case.name, "side": "sim",
            "sim_buffer_bytes": sim_buffer(case),
            "summary": sim,
        });
        writeln!(out, "{line}").unwrap();
        eprintln!("{line}");
        let mut netns_runs = Vec::new();
        let spec = Spec {
            rate_mbit: case.rate_mbit,
            queue_ms: case.queue_ms,
            delay_ms: case.delay_ms,
            ..Spec::default()
        };
        let topology = Topology::new("xcheck", spec);
        let _server = Server::start(&topology, &keys);
        for run in 1..=repeats {
            let result = run_netns(&topology, &keys, case);
            let line = serde_json::json!({
                "case": case.name, "side": "netns", "run": run,
                "tbf_drops": result.tbf_drops,
                "receive_buffer_overflows": result.receive_buffer_overflows,
                "summary": summary(rate, &result.sub_intervals),
            });
            writeln!(out, "{line}").unwrap();
            eprintln!("{line}");
            netns_runs.push((result.tbf_drops, summary(rate, &result.sub_intervals)));
        }
        failures.extend(compare(case, &sim, &netns_runs));
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// The median of `values`.
fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

/// Where the simulator and the netns runs (their median) disagree beyond
/// the tolerances.
fn compare(
    case: &Case,
    sim: &serde_json::Value,
    netns: &[(u64, serde_json::Value)],
) -> Vec<String> {
    let rate = f64::from(case.rate_mbit);
    let f = |v: &serde_json::Value, key: &str| v[key].as_f64();
    let mid = |key: &str| median(netns.iter().filter_map(|(_, s)| f(s, key)).collect());
    let mut failures = Vec::new();
    let mut check = |ok: bool, what: String| {
        if !ok {
            failures.push(format!("{}: {what}", case.name));
        }
    };
    // Where the search settles: within 2 % of the bottleneck's rate.
    let (sim_median, netns_median) = (
        f(sim, "settled_median").unwrap_or(0.0),
        mid("settled_median"),
    );
    check(
        (sim_median - netns_median).abs() <= 0.02 * rate,
        format!("settled at {sim_median} Mbit/s in the simulator, {netns_median} under netem"),
    );
    // How far it swings there: within 1.5 Mbit/s.
    let (sim_swing, netns_swing) = (f(sim, "settled_swing").unwrap_or(0.0), mid("settled_swing"));
    check(
        (sim_swing - netns_swing).abs() <= 1.5,
        format!("swings {sim_swing} Mbit/s in the simulator, {netns_swing} under netem"),
    );
    // When the search first meets the bottleneck's drops: the same
    // Sub-Interval, or none on both sides.
    let firsts: Vec<Option<u64>> = netns
        .iter()
        .map(|(_, s)| s["first_loss"].as_u64())
        .collect();
    check(
        firsts.contains(&sim["first_loss"].as_u64()),
        format!(
            "first loss in Sub-Interval {:?} in the simulator, {firsts:?} under netem",
            sim["first_loss"]
        ),
    );
    // What the queue adds to the delay: within 10 ms.
    let (sim_dv, netns_dv) = (
        f(sim, "delay_variation_max_ms").unwrap_or(0.0),
        mid("delay_variation_max_ms"),
    );
    check(
        (sim_dv - netns_dv).abs() <= 10.0,
        format!("delay variation up to {sim_dv} ms in the simulator, {netns_dv} under netem"),
    );
    // Load PDUs lost: within 25 % of the netns count, and 20 PDUs; every
    // netns loss at the bottleneck, none in a host.
    let loss = |s: &serde_json::Value| {
        s["loss"]
            .as_array()
            .map_or(0.0, |l| l.iter().filter_map(|n| n.as_f64()).sum())
    };
    let (sim_loss, netns_loss) = (
        loss(sim),
        median(netns.iter().map(|(_, s)| loss(s)).collect()),
    );
    check(
        (sim_loss - netns_loss).abs() <= (0.25 * netns_loss).max(20.0),
        format!("{sim_loss} Load PDUs lost in the simulator, {netns_loss} under netem"),
    );
    for (drops, s) in netns {
        check(
            loss(s) <= *drops as f64 + 10.0,
            format!(
                "{} lost under netem but the bottleneck dropped {drops}: a host lost the rest",
                loss(s)
            ),
        );
    }
    failures
}
