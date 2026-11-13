//! 's measurement above the 1 Gbps row, kept to run again: Search
//! Tests of 10 s through real queues at multi-gigabit rates in the netns
//! tier's topology, each Trial Interval the Client counted classified again
//! from the Status PDUs captured on the Client's link, as the Client's
//! count classifies them (by the maximum delay variation), and judged by
//! the rule the Client judges a search's end by. It gates nothing: the
//! simulated Tests in `src/tests/protocol/congestion_never_confirmed.rs`
//! gate that rule. Both tests are ignored, and outside the tier's
//! namespace they skip, so run them by hand on Linux, with an optimized
//! build, alone on the host (under its measurement lock where it has one):
//!
//! ```text
//! UDPSTP_NETNS_REQUIRED=1 \
//! CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER=$PWD/scripts/netns-runner.sh \
//! cargo test --release -p udpstp --test gigabit_queues -- --ignored --nocapture
//! ```
//!
//! Why optimized: in these namespaces the routers forward and queue in
//! softirq on the sending host's CPU. Through any shaped queue the tier's
//! debug build of the Server then tops out just under 2.5 Gbit/s
//! Downstream on an Intel Core i7-8550U, skipping about 2.3 million Load
//! PDUs a Test: such a run measures the host, not the queue, and its
//! LoadSender warning is true. The release build keeps up there.
//!
//! The topology is the tier's with a 1500-byte middle MTU, since the rows
//! above 1 Gbps send Load PDUs of up to 1500 bytes behind a Server's
//! default MTU, and netem queues of 100 000 packets, since netem's default
//! of 1000 holds about 5 ms of them at 200 000 a second, under 2.5 Gbit/s.
//! A host that can't forward 1500-byte packets at the rate, as WSL2 can't,
//! can run every link and the Server at 9000 bytes: the Jumbo rows then
//! send half as many at 2.5 Gbit/s, and the search moves through the same
//! rows. WSL2's Load Senders fall behind through the shaped queues even so,
//! and at 500 Mbit/s too, where a bare host keeps up at 2.5 Gbit/s
//!.
//!
//! `an_unshaped_path_carries_the_rate` checks the hosts and links first:
//! Fixed-Rate Tests at the rate and 0.5 and 1 Gbit/s above it, each way,
//! should deliver their row's rate in every Sub-Interval with no loss; an
//! unshaped Search Test shows where a host tops out.
//!
//! `searches_through_real_queues_above_the_gigabit_row` prints a `SUMMARY`
//! line per Test: its Trial Intervals (Clean, Errored, and Hold with and
//! without a delay sample), whether Congestion was confirmed and the
//! longest run of Errored Intervals, its last 64 Trial Intervals from the
//! first Errored one on (the share the rule judges, with Hold Intervals
//! and without) and the share of the Load PDUs they lost, the Client's
//! warnings (the cause of a CongestionNeverConfirmed, and whether it warned
//! SubIntervalsAfterMaximumMissed), the Maximum IP-Layer Capacity it
//! reported, the one udpstp's default Performance Criterion (a Loss
//! Ratio of 1%, or one Load PDU) gives and the Highest IP-Layer Capacity,
//! where its lost Load PDUs were dropped (the bottleneck's qdisc, netem's,
//! the veths, where the forwarding CPU's backlog overflowed, or the Load
//! Receiver's Receive Buffer Overflows; through CAKE, also the highest
//! BLUE drop probability of CAKE's queues) and what the Load Sender left
//! unsent;
//! each Sub-Interval's IP-Layer Capacity, lost Load PDUs and Loss Ratio;
//! then a tally. A Multi-Connection Test's line counts the Trial Intervals
//! of all its Test Connections, whose Congestion is confirmed, or whose
//! limit met at the end, where any one's is, as the Client warns only where
//! none found the path's limit; its Sub-Intervals are the Test's, as the
//! Client aggregates them; through CAKE it shows each of CAKE's queues'
//! BLUE drop probability; and a line for each Test Connection follows,
//! numbered in the order its first Status PDU was captured, with its
//! Trial Intervals, its verdict and the share of its Load PDUs it lost. It asserts that every Test completed and that each
//! capture is complete and agrees with the Client's own verdict, so the
//! numbers can be trusted. Beyond that, look for:
//! - no CongestionNeverConfirmed warning where the Load Sender left at
//!   most 1% of its Load PDUs unsent, as Skipped or Withheld Bursts (the
//!   tally counts them). A Load Sender can fall behind by sending Bursts
//!   late, too, which that doesn't show: a Maximum near where the
//!   unshaped check's search topped out is the host's;
//! - through tbf with 1 ms of queue, searches that never confirm Congestion,
//!   their Errored Intervals two at a time, but meet the limit at their
//!   end, about half their last Trial Intervals errored;
//! - through tbf with 20 ms of queue, searches that cycle around the
//!   bottleneck's rate, three Errored Intervals at a time, losing about
//!   half a row's worth of Load PDUs, about 2% at 2.5 Gbit/s, where the
//!   rows are 100 Mbps apart, so that no Sub-Interval there meets the
//!   Performance Criterion and the Maximum IP-Layer Capacity comes from the
//!   search's climb, well below the Highest;
//! - through fq_codel, with a Load Sender only just above the bottleneck,
//!   a standing queue of 30 to 60 ms that CoDel drains too slowly to lose
//!   more than seqErrThresh a Trial Interval: Hold Intervals, the limit
//!   the running-a-test guide describes;
//! - through CAKE, a search that collapses after its first overshoot. The
//!   overshoot fills CAKE's memory limit (10,240 times the MTU plus the
//!   Ethernet header in packet buffers, about 25 ms at 2.5 Gbit/s), each
//!   overflow raises the BLUE drop probability of the Load PDUs' queue,
//!   and CAKE lowers it only when it finds that queue empty while another
//!   of its queues holds packets, which a Test's lone Load PDUs never give
//!   it: it keeps dropping that share of them, 8-10% here, at any rate to
//!   the end of the Test.
//!   `cake-memN` raises the memory limit to N MB. So does
//!   `UDPSTP_GIGABIT_MTU=9000`, since the limit follows the MTU up to four
//!   times what the rate sends in 100 ms: sixfold at 2.5 Gbit/s, where CAKE
//!   shouldn't collapse, 3.2 times at 1 Gbit/s and 1.6 at 500 Mbit/s.
//!
//! Settings: `UDPSTP_GIGABIT_RATE`, the bottleneck in Mbit/s (2500);
//! `UDPSTP_GIGABIT_QUEUES` (`fq-codel,cake,tbf-20,tbf-1`; also `tbf-N` for
//! N ms of queue, `cake-memN`, and `police-N`, a policer that drops at the
//! rate past a burst of N ms at it and queues nothing);
//! `UDPSTP_GIGABIT_RATE_CHANGE` (`S:M`, a token bucket's rate changed to M
//! Mbit/s S seconds into each Search Test, and back after it, as a path
//! that slows during a Test, ; unset); `UDPSTP_GIGABIT_DIRECTIONS`
//! (`downstream,upstream`); `UDPSTP_GIGABIT_REPS`, Tests per queue and
//! direction (3); `UDPSTP_GIGABIT_MTU`, every link's MTU and the Server's
//! (1500, or 9000); `UDPSTP_GIGABIT_TEST_CONNECTIONS`, the Test
//! Connections of each Search Test, the unshaped check's too (1; OB-UDPST's
//! Client takes it as `-C`); `UDPSTP_GIGABIT_ENGINE`, whose Server and Client run
//! the Search Tests: `udpstp`, or `obudpst`, the pinned OB-UDPST that
//! `scripts/obudpst-build.sh` built in `UDPSTP_OBUDPST_DIR` (default
//! `target/obudpst`), with jumbo sizes off (`-j`) over 1500-byte links, so
//! that its rows above the 1 Gbps row are 100 Mbps apart in datagrams the
//! middle MTU carries, and its default Jumbo rows, udpstp's, over 9000-byte
//! ones; and `UDPSTP_GIGABIT_OUT`, where each Test's JSON results, its
//! Trial Intervals as CSV (`<tag>.N.csv` for Test Connection N where there
//! are several), the bottleneck's qdisc statistics after it and
//! the Server's log go (`udpstp-gigabit-queues` in the temporary
//! directory). A Test takes 10.6 s: the default set, 24 of them, about 4.5
//! minutes, and the unshaped check about 80 s.
#![cfg(target_os = "linux")]

#[path = "common/capture.rs"]
#[allow(dead_code)]
mod capture;
mod common;
#[path = "common/key_file.rs"]
mod key_file;
#[path = "common/netns.rs"]
mod netns;
#[path = "../src/wire/mod.rs"]
#[allow(dead_code, unused_imports)]
mod wire;

use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Write as _};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::wire::{SendingRate, StatusPdu};
use common::run_bounded;
use key_file::KeyFile;
use netns::{Node, Queue, Spec, Topology, addr};

const UDPSTP: &str = env!("CARGO_BIN_EXE_udpstp");

/// The Client's IPv4 address in the topology.
const CLIENT_V4: &str = "10.1.0.2";

/// The Shared Key OB-UDPST's ends use, and its keyId.
const OB_KEY: &str = "gigabit-key";
const OB_KEY_ID: &str = "5";

/// One measurement at a time.
static SERIAL: Mutex<()> = Mutex::new(());

/// A setting from the environment, or its default.
fn setting(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// Where the Tests' files go.
fn out_dir() -> PathBuf {
    let dir = std::env::var_os("UDPSTP_GIGABIT_OUT").map_or_else(
        || std::env::temp_dir().join("udpstp-gigabit-queues"),
        PathBuf::from,
    );
    std::fs::create_dir_all(&dir).expect("the output directory");
    dir
}

/// An Operator Key, which Fixed-Rate Tests need.
fn keys(test: &str) -> KeyFile {
    KeyFile::new(
        test,
        "keys.toml",
        "[[key]]\nadmin_key_name = \"netns\"\nkey_id = 1\nkey_text = \"netns-key\"\noperator = true\n",
    )
}

/// Whose Server and Client run the Search Tests.
enum Engine {
    Udpstp,
    /// The pinned OB-UDPST's `udpst`.
    Obudpst(String),
}

impl Engine {
    fn from_settings() -> Self {
        match setting("UDPSTP_GIGABIT_ENGINE", "udpstp").as_str() {
            "udpstp" => Engine::Udpstp,
            "obudpst" => {
                let dir = std::env::var_os("UDPSTP_OBUDPST_DIR").map_or_else(
                    || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/obudpst"),
                    PathBuf::from,
                );
                let udpst = dir.join("v9.1.0").join("auth").join("udpst");
                assert!(
                    udpst.exists(),
                    "no OB-UDPST build at {}: run scripts/obudpst-build.sh",
                    udpst.display()
                );
                Engine::Obudpst(udpst.to_str().expect("a UTF-8 path").to_string())
            }
            other => panic!("UDPSTP_GIGABIT_ENGINE: no engine {other:?}"),
        }
    }

    /// Its key file: udpstp's with an Operator Key, or OB-UDPST's.
    fn keys(&self, test: &str) -> KeyFile {
        match self {
            Engine::Udpstp => keys(test),
            Engine::Obudpst(_) => {
                KeyFile::new(test, "udpst.keys", &format!("{OB_KEY_ID} {OB_KEY}\n"))
            }
        }
    }

    /// What its Tests' files are named with first.
    fn prefix(&self) -> &'static str {
        match self {
            Engine::Udpstp => "",
            Engine::Obudpst(_) => "obudpst-",
        }
    }
}

/// OB-UDPST's option for its datagram sizes above the 1 Gbps row: jumbo
/// sizes off (`-j`), 1250-byte datagrams 100 Mbps apart, unless the links
/// carry 9000 bytes, where its default Jumbo rows are udpstp's.
fn obudpst_jumbo() -> &'static [&'static str] {
    if mtu() >= 9000 { &[] } else { &["-j"] }
}

/// The MTU of every link and of udpstp's Server (`UDPSTP_GIGABIT_MTU`,
/// 1500). At 9000 the Jumbo rows above the 1 Gbps row send packets of up
/// to 9000 bytes, 3125 at 2.5 Gbit/s, half as many as at 1500, for a host
/// that can't forward 1500-byte packets at the rate; OB-UDPST's Server and
/// Client then run with jumbo sizes on.
fn mtu() -> u32 {
    setting("UDPSTP_GIGABIT_MTU", "1500")
        .parse()
        .expect("UDPSTP_GIGABIT_MTU in bytes")
}

/// The tier's topology at `rate_mbit`, every link's MTU `mtu()` (the
/// middle one's 1500 by default, since the rows above 1 Gbps send Load
/// PDUs of up to 1500 bytes behind a Server's default MTU) and netem
/// queues of 100 000 packets.
fn topology(name: &str, rate_mbit: u32, queue: Queue, queue_ms: u32) -> Topology {
    let mtu = mtu();
    let topology = Topology::new(
        name,
        Spec {
            rate_mbit,
            queue_ms,
            middle_mtu: mtu,
            queue,
            netem_limit: Some(100_000),
            ..Spec::default()
        },
    );
    if mtu != 1500 {
        let mtu = mtu.to_string();
        for (node, dev) in [
            (Node::Client, "c0"),
            (Node::Router1, "r0"),
            (Node::Router2, "r3"),
            (Node::Server, "s0"),
        ] {
            let status = topology
                .command(node, "ip")
                .args(["link", "set", "dev", dev, "mtu", &mtu])
                .status()
                .expect("ip runs");
            assert!(status.success(), "{dev}'s MTU set to {mtu}");
        }
    }
    topology
}

/// A queue `UDPSTP_GIGABIT_QUEUES` names: the queue, how much delay it
/// holds, and CAKE's memory limit in MB if it sets one. A policer
/// (`police-N`) runs over an unshaped queue, set up by [`police`].
fn named_queue(name: &str) -> (Queue, u32, Option<u32>) {
    let number = |digits: &str| {
        digits
            .parse::<u32>()
            .unwrap_or_else(|_| panic!("UDPSTP_GIGABIT_QUEUES: no queue {name:?}"))
    };
    match name {
        // fq_codel under the token bucket: its queue replaces the token
        // bucket's, whose depth then doesn't matter.
        "fq-codel" => (Queue::FqCodel, 200, None),
        "cake" => (Queue::Cake, 200, None),
        _ => {
            if let Some(ms) = name.strip_prefix("tbf-") {
                (Queue::Tbf, number(ms), None)
            } else if let Some(ms) = name.strip_prefix("police-") {
                number(ms);
                (Queue::Unshaped, 200, None)
            } else if let Some(mb) = name.strip_prefix("cake-mem") {
                (Queue::Cake, 200, Some(number(mb)))
            } else {
                panic!("UDPSTP_GIGABIT_QUEUES: no queue {name:?}")
            }
        }
    }
}

/// The burst of a policer `UDPSTP_GIGABIT_QUEUES` names (`police-N`): N ms
/// at the rate.
fn policer_burst_ms(name: &str) -> Option<u32> {
    name.strip_prefix("police-")
        .map(|ms| ms.parse().expect("police-N: N in ms"))
}

/// A policer at the routers' egress toward the Client and the Server
///: a token bucket of `burst_ms` at `rate_mbit` that drops what
/// exceeds it and queues nothing, as an ISP's ingress policer does. A
/// large burst lets a search's first overshoot through, then drops at the
/// rate. Its drops are counted in the filter's statistics, not a qdisc's.
fn police(topology: &Topology, rate_mbit: u32, burst_ms: u32) {
    let rate = format!("{rate_mbit}mbit");
    let burst = (u64::from(rate_mbit) * 1_000_000 / 8 * u64::from(burst_ms) / 1000).to_string();
    for (node, dev) in [(Node::Router1, "r1"), (Node::Router2, "r2")] {
        tc(topology, node, &["qdisc", "add", "dev", dev, "clsact"]);
        tc(
            topology,
            node,
            &[
                "filter",
                "add",
                "dev",
                dev,
                "egress",
                "matchall",
                "action",
                "police",
                "rate",
                &rate,
                "burst",
                &burst,
                "conform-exceed",
                "drop",
            ],
        );
    }
}

/// `UDPSTP_GIGABIT_RATE_CHANGE=S:M`: the token bucket's rate changed to M
/// Mbit/s S seconds after each Search Test starts, and back after it
///, as a path that slows during a Test.
fn rate_change() -> Option<(Duration, u32)> {
    let value = std::env::var("UDPSTP_GIGABIT_RATE_CHANGE").ok()?;
    let (secs, mbit) = value
        .split_once(':')
        .expect("UDPSTP_GIGABIT_RATE_CHANGE=S:M");
    Some((
        Duration::from_secs_f64(secs.parse().expect("S in seconds")),
        mbit.parse().expect("M in Mbit/s"),
    ))
}

/// Sets the token bucket of both routers to `rate_mbit`, `queue_ms` deep,
/// as the topology set it up (`Queue::Tbf`).
fn set_tbf_rate(topology: &Topology, rate_mbit: u32, queue_ms: u32) {
    let (rate, latency) = (format!("{rate_mbit}mbit"), format!("{queue_ms}ms"));
    for (node, dev) in [(Node::Router1, "r1"), (Node::Router2, "r2")] {
        tc(
            topology,
            node,
            &[
                "qdisc", "change", "dev", dev, "root", "stab", "overhead", "-14", "tbf", "rate",
                &rate, "burst", "32kb", "latency", &latency,
            ],
        );
    }
}

/// A `key=value` field of a log line, as a number.
fn field(line: &str, key: &str) -> Option<u64> {
    let at = line.find(&format!(" {key}="))? + key.len() + 2;
    line[at..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()
}

/// The first number after `label` in `text`, if any.
fn number_after(text: &str, label: &str) -> Option<u64> {
    text.split(label)
        .nth(1)?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()
}

/// What `tc` prints in a node's namespace.
fn tc(topology: &Topology, node: Node, args: &[&str]) -> String {
    let output = topology
        .command(node, "tc")
        .args(args)
        .output()
        .expect("tc runs");
    assert!(
        output.status.success(),
        "tc {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The packets a device's qdiscs dropped: its root qdisc's count, which
/// takes in what the qdiscs under it dropped.
fn qdisc_drops(topology: &Topology, node: Node, dev: &str) -> u64 {
    let text = tc(topology, node, &["-s", "qdisc", "show", "dev", dev]);
    number_after(&text, "dropped ")
        .unwrap_or_else(|| panic!("no drop count in {dev}'s qdisc statistics: {text}"))
}

/// A UDP counter of a node's namespace, from `/proc/net/snmp`.
fn udp_counter(topology: &Topology, node: Node, name: &str) -> u64 {
    let output = topology
        .command(node, "cat")
        .arg("/proc/net/snmp")
        .output()
        .expect("cat runs");
    let text = String::from_utf8_lossy(&output.stdout);
    let mut lines = text.lines().filter(|line| line.starts_with("Udp:"));
    let (Some(names), Some(values)) = (lines.next(), lines.next()) else {
        panic!("no UDP counters in /proc/net/snmp: {text}");
    };
    names
        .split_whitespace()
        .zip(values.split_whitespace())
        .find(|(n, _)| *n == name)
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or_else(|| panic!("no {name} in /proc/net/snmp"))
}

/// The bottleneck's device on the Load PDUs' way, and its node: rtr2's
/// toward the Client for a Downstream Test, rtr1's toward the Server for an
/// Upstream one.
fn bottleneck(downstream: bool) -> (Node, &'static str) {
    if downstream {
        (Node::Router2, "r2")
    } else {
        (Node::Router1, "r1")
    }
}

/// The packets the topology's veths couldn't hand to their peers. Without
/// RPS a veth hands each packet to the backlog of the CPU that sent it, so
/// these are the forwarding CPU's backlog overflowing. The sending end
/// counts such a drop as sent-and-dropped and the backlog counts it as
/// received-and-dropped at the peer, so only the sending ends' count is
/// taken, once per drop.
fn veth_drops(topology: &Topology) -> u64 {
    [
        (Node::Client, "c0"),
        (Node::Router1, "r0"),
        (Node::Router1, "r1"),
        (Node::Router2, "r2"),
        (Node::Router2, "r3"),
        (Node::Server, "s0"),
    ]
    .into_iter()
    .map(|(node, dev)| {
        let output = topology
            .command(node, "ip")
            .args(["-s", "-j", "link", "show", "dev", dev])
            .output()
            .expect("ip runs");
        let links: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("ip's JSON statistics");
        links[0]["stats64"]["tx"]["dropped"]
            .as_u64()
            .unwrap_or_else(|| panic!("no tx drops for {dev}: {links}"))
    })
    .sum()
}

/// The packets the host's per-CPU backlogs dropped since it booted (the
/// second column of `/proc/net/softnet_stat`), host-wide.
fn backlog_drops() -> u64 {
    std::fs::read_to_string("/proc/net/softnet_stat")
        .expect("/proc/net/softnet_stat")
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .map(|hex| u64::from_str_radix(hex, 16).expect("a hex count"))
        .sum()
}

/// Where a Test's Load PDUs can be lost, as counters read before and after
/// it: the bottleneck's qdisc, netem's on the cli–rtr1 link, the veths
/// (and the host's backlogs, which they overflow), and the Load Receiver's
/// Receive Buffer Overflows (its namespace's RcvbufErrors).
#[derive(Clone, Copy)]
struct Drops {
    bottleneck: u64,
    netem: u64,
    veths: u64,
    backlogs: u64,
    receive_buffer: u64,
}

impl Drops {
    fn read(topology: &Topology, downstream: bool) -> Self {
        let (node, dev) = bottleneck(downstream);
        let (netem_node, netem_dev, receiver) = if downstream {
            (Node::Router1, "r0", Node::Client)
        } else {
            (Node::Client, "c0", Node::Server)
        };
        Self {
            bottleneck: qdisc_drops(topology, node, dev),
            netem: qdisc_drops(topology, netem_node, netem_dev),
            veths: veth_drops(topology),
            backlogs: backlog_drops(),
            receive_buffer: udp_counter(topology, receiver, "RcvbufErrors"),
        }
    }

    fn since(self, before: Self) -> Self {
        Self {
            bottleneck: self.bottleneck - before.bottleneck,
            netem: self.netem - before.netem,
            veths: self.veths - before.veths,
            backlogs: self.backlogs - before.backlogs,
            receive_buffer: self.receive_buffer - before.receive_buffer,
        }
    }
}

/// One of CAKE's queues in `tc -s class` output: its class, the packets it
/// dropped and its BLUE drop probability, as a share.
#[derive(Debug, PartialEq)]
struct CakeQueue {
    class: String,
    dropped: u64,
    blue: f64,
}

/// CAKE's queues in `tc -s class` output, in its order. CAKE hashes each
/// flow to a queue, so a Test Connection's Load PDUs have one of their own
/// unless two hash alike; which is which the output doesn't say.
fn cake_queues(classes: &str) -> Vec<CakeQueue> {
    classes
        .split("class cake ")
        .skip(1)
        .map(|block| CakeQueue {
            class: block.split_whitespace().next().unwrap_or("").to_string(),
            dropped: number_after(block, "(dropped ").unwrap_or(0),
            blue: number_after(block, "blue_prob ").unwrap_or(0) as f64 / 4_294_967_296.0,
        })
        .collect()
}

/// The highest BLUE drop probability among CAKE's queues, as a share: the
/// Load PDUs' queue's, the one an overflow of CAKE's memory raises. `None`
/// where no queue shows one.
fn blue_probability(classes: &str) -> Option<f64> {
    classes.contains("blue_prob ").then(|| {
        cake_queues(classes)
            .iter()
            .map(|queue| queue.blue)
            .fold(0.0, f64::max)
    })
}

/// A Server in the Server's namespace, its log in a file.
struct Server {
    child: Child,
    log: PathBuf,
    /// Whether it is udpstp's, whose log has `TestSummary` lines.
    udpstp: bool,
}

impl Server {
    /// Starts one. udpstp's allows two Tests from a Source at once, so each
    /// Test can follow the last before it is released, and sends packets
    /// up to the links' MTU; OB-UDPST's has jumbo sizes as its Client.
    fn start(topology: &Topology, engine: &Engine, keys: &KeyFile, log: PathBuf) -> Self {
        let (mut command, ready, needed) = match engine {
            Engine::Udpstp => {
                let mut command = topology.command(Node::Server, UDPSTP);
                command
                    .args([
                        "server",
                        "--key-file",
                        keys.path(),
                        "--tests-per-source",
                        "2",
                        "--server-mtu",
                    ])
                    .arg(mtu().to_string());
                // It listens on the IPv4 and the IPv6 wildcard.
                (command, "listening", 2)
            }
            Engine::Obudpst(udpst) => {
                let mut command = topology.command(Node::Server, udpst);
                command
                    .arg("-v")
                    .args(obudpst_jumbo())
                    .args(["-K", keys.path(), addr::SERVER_V4]);
                (command, "Awaiting setup requests", 1)
            }
        };
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the Server starts");
        let stdout = child.stdout.take().expect("piped");
        let (listening, heard) = std::sync::mpsc::channel();
        let file = log.clone();
        std::thread::spawn(move || {
            let mut file = std::fs::File::create(&file).expect("the Server's log");
            let mut seen = 0;
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = writeln!(file, "{line}");
                if line.contains(ready) {
                    seen += 1;
                    if seen == needed {
                        let _ = listening.send(());
                    }
                }
            }
        });
        heard
            .recv_timeout(Duration::from_secs(10))
            .expect("the Server listens");
        Self {
            child,
            log,
            udpstp: matches!(engine, Engine::Udpstp),
        }
    }

    /// Its `TestSummary` lines so far.
    fn summaries(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter(|line| line.contains("event=\"TestSummary\""))
            .map(String::from)
            .collect()
    }

    /// The `TestSummary` after the first `before`, waiting up to 5 s for
    /// the Test to be released; none from OB-UDPST's.
    fn summary_after(&self, before: usize) -> Option<String> {
        if !self.udpstp {
            return None;
        }
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(line) = self.summaries().get(before) {
                return Some(line.clone());
            }
            if Instant::now() > until {
                return None;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `tcpdump` of the Status PDUs only (UDP length 212) on the Client's link.
struct StatusCapture {
    child: Child,
    file: PathBuf,
    log: PathBuf,
}

impl StatusCapture {
    fn start(topology: &Topology, tag: &str) -> Self {
        let dir = std::env::temp_dir();
        let file = dir.join(format!("gigabit-{tag}-{}.pcap", std::process::id()));
        let log = dir.join(format!("gigabit-{tag}-{}.log", std::process::id()));
        let _ = std::fs::remove_file(&file);
        // --immediate-mode: libpcap otherwise hands packets over a block at
        // a time, and the last ones are lost when tcpdump stops.
        let child = topology
            .command(Node::Client, "tcpdump")
            .args(["-Z", "root", "-i", "c0", "--immediate-mode", "-U", "-n"])
            .args(["-s", "300", "-B", "8192", "-w"])
            .arg(&file)
            .arg("udp and udp[4:2] = 212")
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).expect("tcpdump's log"))
            .spawn()
            .expect("tcpdump starts (is it installed?)");
        let until = Instant::now() + Duration::from_secs(10);
        while std::fs::metadata(&file).map_or(true, |m| m.len() < 24) {
            assert!(Instant::now() < until, "tcpdump didn't start");
            std::thread::sleep(Duration::from_millis(50));
        }
        Self { child, file, log }
    }

    /// Stops it: the datagrams captured, and whether it captured every one
    /// its filter passed.
    fn stop(mut self) -> (Vec<capture::Datagram>, bool) {
        std::thread::sleep(Duration::from_millis(500));
        let _ = Command::new("kill")
            .args(["-INT", &self.child.id().to_string()])
            .status();
        let _ = self.child.wait();
        let bytes = std::fs::read(&self.file).expect("the pcap file");
        let _ = std::fs::remove_file(&self.file);
        let log = std::fs::read_to_string(&self.log).unwrap_or_default();
        let _ = std::fs::remove_file(&self.log);
        let count = |what: &str| {
            log.lines()
                .find(|line| line.ends_with(what))
                .and_then(|line| line.split_whitespace().next()?.parse::<u64>().ok())
        };
        let complete = count("packets captured").is_some()
            && count("packets captured") == count("packets received by filter")
            && count("packets dropped by kernel") == Some(0);
        (capture::parse_pcap(&bytes), complete)
    }
}

impl Drop for StatusCapture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// How a Trial Interval classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Clean,
    Errored,
    Hold,
    HoldWithoutSample,
}

impl Class {
    fn letter(self) -> char {
        match self {
            Class::Clean => 'C',
            Class::Errored => 'E',
            Class::Hold => 'H',
            Class::HoldWithoutSample => 'h',
        }
    }
}

/// The accepted thresholds, from the JSON results' `Input`.
struct Thresholds {
    low: u32,
    upper: u32,
    seq_err: u32,
    ignore_ooo_dup: bool,
    one_way: bool,
    slow_adj: u32,
}

impl Thresholds {
    fn of(results: &serde_json::Value) -> Self {
        let input = &results["Input"];
        let int = |key: &str| {
            let value = input[key]
                .as_u64()
                .unwrap_or_else(|| panic!("Input.{key}: {input:#}"));
            u32::try_from(value).expect("a u32")
        };
        Self {
            low: int("LowerThresh"),
            upper: int("UpperThresh"),
            seq_err: int("SeqErrThresh"),
            ignore_ooo_dup: int("ReordDupIgnoreEnable") != 0,
            one_way: int("IPDVEnable") != 0,
            slow_adj: int("SlowAdjThresh"),
        }
    }

    /// As the core's classification does, by the maximum delay variation.
    fn classify(&self, status: &StatusPdu) -> Class {
        let mut seq_errors = status.seq_err_loss;
        if !self.ignore_ooo_dup {
            seq_errors = seq_errors
                .saturating_add(status.seq_err_ooo)
                .saturating_add(status.seq_err_dup);
        }
        let delay_var = if !self.one_way {
            (status.rtt_var_sample != u32::MAX).then_some(status.rtt_var_sample)
        } else if status.delay_var_cnt == 0 {
            None
        } else {
            Some(status.delay_var_max)
        };
        if seq_errors > self.seq_err || delay_var.is_some_and(|d| d > self.upper) {
            Class::Errored
        } else if delay_var.is_some_and(|d| d < self.low) {
            Class::Clean
        } else if delay_var.is_none() {
            Class::HoldWithoutSample
        } else {
            Class::Hold
        }
    }
}

/// A Trial Interval as a Status PDU reported it.
struct TrialInterval {
    class: Class,
    lost: u64,
    received: u64,
}

fn count(intervals: &[TrialInterval], class: Class) -> u32 {
    u32::try_from(intervals.iter().filter(|t| t.class == class).count()).expect("a u32")
}

/// Items captured from several Test Connections, each with its source and
/// destination, grouped by Test Connection in the order each one's first
/// item was captured.
fn by_test_connection<T>(
    items: impl IntoIterator<Item = (SocketAddr, SocketAddr, T)>,
) -> Vec<Vec<T>> {
    let mut keys: Vec<(SocketAddr, SocketAddr)> = Vec::new();
    let mut groups: Vec<Vec<T>> = Vec::new();
    for (source, destination, item) in items {
        let at = keys
            .iter()
            .position(|key| *key == (source, destination))
            .unwrap_or_else(|| {
                keys.push((source, destination));
                groups.push(Vec::new());
                keys.len() - 1
            });
        groups[at].push(item);
    }
    groups
}

/// How one Test Connection's search went, from its Trial Intervals, as the
/// Client counts and judges them.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Searched {
    clean: u32,
    errored: u32,
    hold: u32,
    unsampled: u32,
    longest: u32,
    confirmed: bool,
    /// Its last 64 Trial Intervals from the first Errored one on, their
    /// Errored and Hold Intervals of both kinds.
    window: u32,
    window_errored: u32,
    window_hold: u32,
    met_at_end: bool,
    /// The Load PDUs its last 64 Trial Intervals lost and received, and
    /// all of them.
    last_lost: u64,
    last_received: u64,
    lost: u64,
    received: u64,
}

impl Searched {
    fn of(intervals: &[TrialInterval], slow_adj: u32) -> Self {
        let (mut run, mut longest) = (0, 0);
        for interval in intervals {
            run = if interval.class == Class::Errored {
                run + 1
            } else {
                0
            };
            longest = longest.max(run);
        }
        // The rule the Client judges a search's end by: of its last 64
        // Trial Intervals, from the first Errored one on, at least 4
        // errored and at least 1 in 4, Hold Intervals of both kinds
        // counted.
        let last = last_64(intervals);
        let from_first = last
            .iter()
            .position(|t| t.class == Class::Errored)
            .map_or(&last[last.len()..], |first| &last[first..]);
        let window_errored = count(from_first, Class::Errored);
        let window = u32::try_from(from_first.len()).expect("a u32");
        let sum = |intervals: &[TrialInterval]| {
            intervals
                .iter()
                .fold((0, 0), |(l, r), t| (l + t.lost, r + t.received))
        };
        let (last_lost, last_received) = sum(last);
        let (lost, received) = sum(intervals);
        Self {
            clean: count(intervals, Class::Clean),
            errored: count(intervals, Class::Errored),
            hold: count(intervals, Class::Hold),
            unsampled: count(intervals, Class::HoldWithoutSample),
            longest,
            confirmed: longest >= slow_adj,
            window,
            window_errored,
            window_hold: count(from_first, Class::Hold)
                + count(from_first, Class::HoldWithoutSample),
            met_at_end: window_errored >= 4 && 4 * window_errored >= window,
            last_lost,
            last_received,
            lost,
            received,
        }
    }

    /// Several Test Connections' together: their counts summed, and
    /// Congestion confirmed or the limit met at the end by any of them, as
    /// the Client warns only where none found the path's limit.
    fn together(each: &[Self]) -> Self {
        each.iter().fold(Self::default(), |all, one| Self {
            clean: all.clean + one.clean,
            errored: all.errored + one.errored,
            hold: all.hold + one.hold,
            unsampled: all.unsampled + one.unsampled,
            longest: all.longest.max(one.longest),
            confirmed: all.confirmed || one.confirmed,
            window: all.window + one.window,
            window_errored: all.window_errored + one.window_errored,
            window_hold: all.window_hold + one.window_hold,
            met_at_end: all.met_at_end || one.met_at_end,
            last_lost: all.last_lost + one.last_lost,
            last_received: all.last_received + one.last_received,
            lost: all.lost + one.lost,
            received: all.received + one.received,
        })
    }

    /// Whether the search found the path's limit.
    fn found_the_limit(&self) -> bool {
        self.confirmed || self.met_at_end
    }
}

/// The last 64 Trial Intervals, the ones the Client judges a search's end
/// by.
fn last_64(intervals: &[TrialInterval]) -> &[TrialInterval] {
    &intervals[intervals.len().saturating_sub(64)..]
}

/// A share of Load PDUs lost, in percent.
fn lost_percent(lost: u64, received: u64) -> f64 {
    100.0 * lost as f64 / (lost + received).max(1) as f64
}

fn percent(part: u32, whole: u32) -> String {
    if whole == 0 {
        "-".into()
    } else {
        format!("{:.1}%", 100.0 * f64::from(part) / f64::from(whole))
    }
}

/// The IPv4 rate of a Sending Rate Structure, in Mbit/s: both
/// Transmitters' Bursts and the Add-on, each datagram with 28 bytes of IP
/// and UDP header (ADR 0004). Zero in a Downstream Test's Status PDUs.
fn sending_rate_mbps(rate: &SendingRate) -> f64 {
    let per = |interval: u32, payload: u32, count: u32| {
        if interval == 0 || payload == 0 {
            0.0
        } else {
            f64::from(payload + 28) * f64::from(count) * 8.0 / f64::from(interval)
        }
    };
    per(rate.tx_interval1, rate.udp_payload1, rate.burst_size1)
        + per(rate.tx_interval2, rate.udp_payload2, rate.burst_size2)
        + per(rate.tx_interval2, rate.udp_addon2, 1)
}

/// Whether the Client warned that the Sub-Intervals after its Maximum IP-Layer
/// Capacity all missed the Performance Criterion, one of them far faster or
/// the last far slower (`SubIntervalsAfterMaximumMissed`).
fn after_maximum_missed(results: &serde_json::Value) -> bool {
    results["udpstp"]["Warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|w| w["Code"] == "SubIntervalsAfterMaximumMissed")
}

/// The Client's CongestionNeverConfirmed warning, as the cause its message
/// names, if it gave one.
fn warning(results: &serde_json::Value) -> Option<&'static str> {
    let message = results["udpstp"]["Warnings"]
        .as_array()?
        .iter()
        .find(|w| w["Code"] == "CongestionNeverConfirmed")?["Message"]
        .as_str()?;
    Some(if message.contains("maxBandwidth") {
        "MaxBandwidth"
    } else if message.contains("top of the Sending Rate Table") {
        "TableTop"
    } else if message.contains("Hold Intervals") {
        "DelayVariation"
    } else if message.contains("still rising") {
        "StillRising"
    } else if message.contains("stopped rising") {
        "LoadSender"
    } else {
        "another cause"
    })
}

/// The Load PDUs a Client's warnings say it didn't send: Skipped and
/// Withheld Bursts.
fn client_not_sent(results: &serde_json::Value) -> u64 {
    results["udpstp"]["Warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|w| w["Code"] == "BurstsSkipped" || w["Code"] == "BurstsWithheld")
        .filter_map(|w| w["Count"].as_u64())
        .sum()
}

/// The Sub-Intervals of JSON results, either engine's.
fn sub_interval_results(results: &serde_json::Value) -> &[serde_json::Value] {
    results["Output"]["IncrementalResult"]
        .as_array()
        .map_or(&[], Vec::as_slice)
}

/// Each Sub-Interval's IP-Layer Capacity, Load PDUs lost and Loss Ratio.
fn sub_intervals(results: &serde_json::Value) -> String {
    sub_interval_results(results)
        .iter()
        .map(|s| {
            format!(
                "{:.0}/{}/{:.2}%",
                s["IPLayerCapacity"].as_f64().unwrap_or(-1.0),
                s["LossCount"].as_u64().unwrap_or(u64::MAX),
                100.0 * s["LossRatio"].as_f64().unwrap_or(-0.01)
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The highest IP-Layer Capacity of the Sub-Intervals that meet udpstp's
/// default Performance Criterion, a Loss Ratio of at most 1% or one Load
/// PDU lost, and of them all: what udpstp would report as the Maximum and
/// the Highest IP-Layer Capacity from either engine's Sub-Intervals.
fn maximum_and_highest(results: &serde_json::Value) -> (Option<f64>, Option<f64>) {
    let (mut maximum, mut highest) = (None::<f64>, None::<f64>);
    for s in sub_interval_results(results) {
        let Some(capacity) = s["IPLayerCapacity"].as_f64() else {
            continue;
        };
        highest = Some(highest.map_or(capacity, |h| h.max(capacity)));
        let ratio = s["LossRatio"].as_f64().unwrap_or(1.0);
        let lost = s["LossCount"].as_u64().unwrap_or(u64::MAX);
        if capacity > 0.0 && (ratio <= 0.01 || lost <= 1) {
            maximum = Some(maximum.map_or(capacity, |m| m.max(capacity)));
        }
    }
    (maximum, highest)
}

fn mbps(value: Option<f64>) -> String {
    value.map_or_else(|| "none".into(), |v| format!("{v:.1}"))
}

/// How many Test Connections each Search Test runs
/// (`UDPSTP_GIGABIT_TEST_CONNECTIONS`, 1), to the one Server.
fn test_connections() -> u8 {
    setting("UDPSTP_GIGABIT_TEST_CONNECTIONS", "1")
        .parse()
        .ok()
        .filter(|n| *n >= 1)
        .expect("UDPSTP_GIGABIT_TEST_CONNECTIONS, from 1 to 255")
}

/// A Test the Client ran, with its JSON results.
fn run_client(topology: &Topology, keys: &KeyFile, args: &[&str]) -> (Output, Duration) {
    let mut command = topology.command(Node::Client, UDPSTP);
    command
        .args(["client", "--key-file", keys.path(), "--format", "json"])
        .args(args);
    let started = Instant::now();
    let output = run_bounded(command, Duration::from_secs(60));
    (output, started.elapsed())
}

/// A Search Test of 10 s, run by the engine's Client.
fn run_search(
    topology: &Topology,
    engine: &Engine,
    keys: &KeyFile,
    direction: &str,
) -> (Output, Duration) {
    let connections = test_connections().to_string();
    let several = test_connections() > 1;
    match engine {
        Engine::Udpstp => {
            let mut args = vec!["--direction", direction, "--test-interval", "10"];
            if several {
                args.extend(["--test-connections", &connections]);
            }
            args.push(addr::SERVER_V4);
            run_client(topology, keys, &args)
        }
        Engine::Obudpst(udpst) => {
            let flag = if direction == "downstream" {
                "-d"
            } else {
                "-u"
            };
            let mut command = topology.command(Node::Client, udpst);
            if several {
                command.args(["-C", &connections]);
            }
            command.arg(flag).args(obudpst_jumbo()).args([
                "-a",
                OB_KEY,
                "-y",
                OB_KEY_ID,
                "-t",
                "10",
                "-f",
                "json",
                addr::SERVER_V4,
            ]);
            let started = Instant::now();
            let output = run_bounded(command, Duration::from_secs(60));
            (output, started.elapsed())
        }
    }
}

fn results(output: &Output, what: &str) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "{what}: no JSON results ({e}); stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

/// What a Search Test showed.
struct Judged {
    line: String,
    complete: bool,
    agrees: bool,
    warned: bool,
    /// It warned of the Sub-Intervals after its Maximum IP-Layer Capacity
    /// (`SubIntervalsAfterMaximumMissed`).
    after_maximum: bool,
    /// The Load Sender left at most 1% unsent, as Skipped or Withheld
    /// Bursts (Bursts it sent late don't show).
    little_unsent: bool,
    confirmed: bool,
    met_at_end: bool,
}

/// A token bucket's rate changed during a Search Test, and back after it
/// ([`rate_change`]).
#[derive(Clone, Copy)]
struct RateChange {
    after: Duration,
    to_mbit: u32,
    from_mbit: u32,
    queue_ms: u32,
}

/// Runs a Search Test of 10 s with its Status PDUs captured, and judges it.
#[allow(clippy::too_many_arguments)] // a measurement's settings, passed once
fn search(
    topology: &Topology,
    engine: &Engine,
    keys: &KeyFile,
    server: &Server,
    direction: &str,
    tag: &str,
    out: &Path,
    change: Option<RateChange>,
) -> Judged {
    let downstream = direction == "downstream";
    let several = test_connections() > 1;
    let summaries = server.summaries().len();
    let before = Drops::read(topology, downstream);
    let capture = StatusCapture::start(topology, tag);
    let (output, wall) = std::thread::scope(|scope| {
        if let Some(change) = change {
            scope.spawn(move || {
                std::thread::sleep(change.after);
                set_tbf_rate(topology, change.to_mbit, change.queue_ms);
            });
        }
        run_search(topology, engine, keys, direction)
    });
    if let Some(change) = change {
        set_tbf_rate(topology, change.from_mbit, change.queue_ms);
    }
    let (datagrams, complete) = capture.stop();
    let drops = Drops::read(topology, downstream).since(before);
    // The bottleneck's statistics after the Test, with CAKE's queues' and a
    // policer's.
    let (node, dev) = bottleneck(downstream);
    let mut qdisc = tc(topology, node, &["-s", "qdisc", "show", "dev", dev]);
    if qdisc.contains("qdisc clsact") {
        let filters = tc(
            topology,
            node,
            &["-s", "filter", "show", "dev", dev, "egress"],
        );
        let _ = write!(qdisc, "--- egress filters\n{filters}");
    }
    let (blue, queues) = if qdisc.contains("qdisc cake") {
        let classes = tc(topology, node, &["-s", "class", "show", "dev", dev]);
        let _ = write!(qdisc, "--- classes\n{classes}");
        (blue_probability(&classes), cake_queues(&classes))
    } else {
        (None, Vec::new())
    };
    std::fs::write(out.join(format!("{tag}.qdisc.txt")), &qdisc).expect("the qdisc statistics");
    std::fs::write(out.join(format!("{tag}.json")), &output.stdout).expect("the JSON results");
    let results = results(&output, tag);
    let summary = server.summary_after(summaries).unwrap_or_default();
    let (completed, end) = match engine {
        Engine::Udpstp => (
            results["udpstp"]["EndReason"] == "Completed",
            results["udpstp"]["EndReason"].to_string(),
        ),
        // OB-UDPST's warnings are ErrorStatus codes too: what counts is a
        // result for each Sub-Interval.
        Engine::Obudpst(_) => (
            sub_interval_results(&results).len() >= 10,
            format!("ErrorStatus {}", results["ErrorStatus"]),
        ),
    };

    // The Status PDUs the Client counts: its own Downstream, the Server's
    // fresh ones Upstream, testAction TEST only; each Test Connection's
    // apart, by its addresses.
    let from: IpAddr = if downstream {
        CLIENT_V4
    } else {
        addr::SERVER_V4
    }
    .parse()
    .expect("an address");
    let thresholds = Thresholds::of(&results);
    let each_status = by_test_connection(
        datagrams
            .iter()
            .filter(|d| d.source.ip() == from)
            .filter_map(|d| Some((d.source, d.destination, StatusPdu::decode(&d.payload).ok()?))),
    );
    let mut each = Vec::new();
    let mut tails = Vec::new();
    for (index, statuses) in each_status.iter().enumerate() {
        let mut last_seq_no = 0;
        let mut csv = String::from(
            "spdu_seq_no,class,seq_err_loss,seq_err_ooo,delay_var_min,delay_var_max,delay_var_cnt,rtt_var_sample,ti_rx_datagrams,loss_share,ti_rx_mbps,sending_rate_mbps\n",
        );
        let mut intervals = Vec::new();
        for status in statuses {
            if status.test_action != 0 || (!downstream && status.spdu_seq_no <= last_seq_no) {
                continue;
            }
            last_seq_no = status.spdu_seq_no;
            let class = thresholds.classify(status);
            let (lost, received) = (
                u64::from(status.seq_err_loss),
                u64::from(status.ti_rx_datagrams),
            );
            let rx = f64::from(status.ti_rx_bytes) * 8.0 / f64::from(status.ti_delta_time.max(1));
            let rtt = if status.rtt_var_sample == u32::MAX {
                String::new()
            } else {
                status.rtt_var_sample.to_string()
            };
            let _ = writeln!(
                csv,
                "{},{},{lost},{},{},{},{},{rtt},{received},{:.4},{rx:.1},{:.1}",
                status.spdu_seq_no,
                class.letter(),
                status.seq_err_ooo,
                status.delay_var_min,
                status.delay_var_max,
                status.delay_var_cnt,
                lost as f64 / (lost + received).max(1) as f64,
                sending_rate_mbps(&status.sending_rate),
            );
            intervals.push(TrialInterval {
                class,
                lost,
                received,
            });
        }
        // One Test Connection's CSV keeps its name; several are numbered
        // in the order their first Status PDU was captured.
        let csv_name = if several {
            format!("{tag}.{index}.csv")
        } else {
            format!("{tag}.csv")
        };
        std::fs::write(out.join(csv_name), csv).expect("the CSV");
        each.push(Searched::of(&intervals, thresholds.slow_adj));
        tails.push(
            last_64(&intervals)
                .iter()
                .map(|t| t.class.letter())
                .collect::<String>(),
        );
    }
    let all = Searched::together(&each);

    // The Client's own verdict and what the Load Sender left unsent: only
    // udpstp's Client and Server report them.
    let (verdict, agrees, warned, not_sent, little_unsent) = match engine {
        Engine::Udpstp => {
            let warned = warning(&results);
            let agrees = all.found_the_limit() == warned.is_none();
            let (not_sent, little_unsent) = if downstream {
                let skipped = field(&summary, "skipped_bytes").unwrap_or(u64::MAX);
                let withheld = field(&summary, "withheld_bytes").unwrap_or(0);
                let sent = field(&summary, "bytes_sent").unwrap_or(0);
                let unsent = skipped.saturating_add(withheld);
                (
                    format!("the Server left {unsent} bytes unsent"),
                    unsent.saturating_mul(100) <= sent.saturating_add(unsent),
                )
            } else {
                // The Load PDUs the Server's Status PDUs counted, received
                // and lost.
                let unsent = client_not_sent(&results);
                let sent = all.received + all.lost;
                (
                    format!("this Client left {unsent} Load PDUs unsent"),
                    unsent.saturating_mul(100) <= sent.saturating_add(unsent),
                )
            };
            (
                format!(
                    "warning {}, SubIntervalsAfterMaximumMissed {}",
                    warned.unwrap_or("none"),
                    after_maximum_missed(&results)
                ),
                agrees,
                warned.is_some(),
                format!("{not_sent}, at most 1% {little_unsent}"),
                little_unsent,
            )
        }
        Engine::Obudpst(_) => (
            "no warnings judged".to_string(),
            true,
            false,
            "unsent not reported".to_string(),
            true,
        ),
    };
    let (maximum, highest) = maximum_and_highest(&results);
    let lost: u64 = sub_interval_results(&results)
        .iter()
        .filter_map(|s| s["LossCount"].as_u64())
        .sum();
    let mut blue = blue.map_or_else(String::new, |p| {
        format!(", CAKE's highest BLUE drop probability {:.2}%", 100.0 * p)
    });
    if several && !queues.is_empty() {
        let each_queue: Vec<String> = queues
            .iter()
            .map(|q| format!("{} {:.2}% ({} dropped)", q.class, 100.0 * q.blue, q.dropped))
            .collect();
        let _ = write!(blue, ", by queue [{}]", each_queue.join(", "));
    }
    let Searched {
        clean,
        errored,
        hold,
        unsampled,
        longest,
        confirmed,
        window,
        window_errored,
        window_hold,
        met_at_end,
        ..
    } = all;
    let last_share = lost_percent(all.last_lost, all.last_received);
    // Several Test Connections: the figures of them all, where one
    // confirming Congestion or meeting the limit at its end is enough, then
    // a line for each.
    let (intervals_of, found) = if several {
        (
            format!(" of the {} Test Connections", each.len()),
            " by one",
        )
    } else {
        (String::new(), "")
    };
    let mut line = format!(
        "{tag}: {:.1} s, {end} | Trial Intervals{intervals_of}: C {clean} E {errored} H {hold} h {unsampled} | confirmed{found} {confirmed} (longest Errored run {longest}) | last 64 from the first Errored: {window} (E {window_errored}, H {window_hold}), Errored {} with Hold Intervals, {} without; met at the end{found} {met_at_end}; the last 64 lost {last_share:.2}% of their Load PDUs | {verdict} | reported maximum {} Mbit/s; at the 1% Performance Criterion {} Mbit/s, Highest {} Mbit/s | the Sub-Intervals lost {lost} Load PDUs; during the Test the bottleneck dropped {}, netem {}, the veths {} (the host's backlogs {}), Receive Buffer Overflows {}{blue} | {not_sent} | capture complete {complete}, agrees with the Client {agrees}\n    capacity/lost/Loss Ratio [{}]",
        wall.as_secs_f64(),
        percent(window_errored, window),
        percent(window_errored, window - window_hold),
        results["Output"]["AtMax"]["MaxIPLayerCapacity"],
        mbps(maximum),
        mbps(highest),
        drops.bottleneck,
        drops.netem,
        drops.veths,
        drops.backlogs,
        drops.receive_buffer,
        sub_intervals(&results),
    );
    if several {
        for (index, (one, tail)) in each.iter().zip(&tails).enumerate() {
            let _ = write!(
                line,
                "\n    Test Connection {index}: C {} E {} H {} h {} | confirmed {} (longest {}) | last 64 from the first Errored {} (E {}), met at the end {} | lost {:.2}% of its Load PDUs, the last 64 {:.2}% | last 64 {tail}",
                one.clean,
                one.errored,
                one.hold,
                one.unsampled,
                one.confirmed,
                one.longest,
                one.window,
                one.window_errored,
                one.met_at_end,
                lost_percent(one.lost, one.received),
                lost_percent(one.last_lost, one.last_received),
            );
        }
    } else {
        let _ = write!(line, "\n    last 64 {}", tails.concat());
    }
    Judged {
        line,
        // Every Test Connection's Status PDUs captured.
        complete: completed && complete && each.len() == usize::from(test_connections()),
        agrees,
        warned,
        after_maximum: after_maximum_missed(&results),
        little_unsent,
        confirmed,
        met_at_end,
    }
}

fn say(line: &str, log: &mut std::fs::File) {
    eprintln!("{line}");
    let _ = writeln!(log, "{line}");
}

// : the hosts and links carry the rate the queues are measured at,
// each way, with no shaping: Fixed-Rate Tests at it and above deliver
// their row's rate with no loss (a measurement run by hand; see the module
// docs)
#[test]
#[ignore = "a measurement run by hand in the netns tier's namespace, optimized: see the module docs"]
fn an_unshaped_path_carries_the_rate() {
    if !netns::available("an_unshaped_path_carries_the_rate") {
        return;
    }
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let rate: u32 = setting("UDPSTP_GIGABIT_RATE", "2500")
        .parse()
        .expect("UDPSTP_GIGABIT_RATE in Mbit/s");
    let out = out_dir();
    let mut log = std::fs::File::create(out.join("unshaped.log")).expect("the log");
    let topology = topology("unshaped", rate, Queue::Unshaped, 20);
    let engine = Engine::Udpstp;
    let keys = keys("unshaped");
    let server = Server::start(&topology, &engine, &keys, out.join("unshaped-server.log"));
    // The Jumbo rows above 1 Gbps are 100 Mbps apart up to 7.2 Gbps.
    let row = |mbps: u32| (1000 + (mbps.clamp(1100, 7200) - 1000) / 100).to_string();
    for direction in ["downstream", "upstream"] {
        for mbps in [rate, rate + 500, rate + 1000] {
            let tag = format!("unshaped-{direction}-{mbps}");
            let (output, wall) = run_client(
                &topology,
                &keys,
                &[
                    "--direction",
                    direction,
                    "--test-interval",
                    "5",
                    "--fixed-rate",
                    &row(mbps),
                    addr::SERVER_V4,
                ],
            );
            let results = results(&output, &tag);
            say(
                &format!(
                    "SUMMARY {tag}: {:.1} s, {} | capacity/lost/Loss Ratio [{}] | the Client left {} Load PDUs unsent",
                    wall.as_secs_f64(),
                    results["udpstp"]["EndReason"],
                    sub_intervals(&results),
                    client_not_sent(&results),
                ),
                &mut log,
            );
            assert_eq!(results["udpstp"]["EndReason"], "Completed", "{tag}");
        }
        let judged = search(
            &topology,
            &engine,
            &keys,
            &server,
            direction,
            &format!("unshaped-{direction}-search"),
            &out,
            None,
        );
        say(&format!("SUMMARY {}", judged.line), &mut log);
    }
}

// : Search Tests above the 1 Gbps row through real queues, each Trial
// Interval classified again from the Status PDUs captured, and judged as
// the Client judges them: every capture complete and agreeing with the
// Client's own verdict (a measurement run by hand; see the module docs for
// what to look for). : the same through OB-UDPST's Server and Client,
// and where each Test's lost Load PDUs were dropped
#[test]
#[ignore = "a measurement run by hand in the netns tier's namespace, optimized: see the module docs"]
fn searches_through_real_queues_above_the_gigabit_row() {
    if !netns::available("searches_through_real_queues_above_the_gigabit_row") {
        return;
    }
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let rate: u32 = setting("UDPSTP_GIGABIT_RATE", "2500")
        .parse()
        .expect("UDPSTP_GIGABIT_RATE in Mbit/s");
    let reps: u32 = setting("UDPSTP_GIGABIT_REPS", "3")
        .parse()
        .expect("UDPSTP_GIGABIT_REPS");
    let queues = setting("UDPSTP_GIGABIT_QUEUES", "fq-codel,cake,tbf-20,tbf-1");
    let directions = setting("UDPSTP_GIGABIT_DIRECTIONS", "downstream,upstream");
    let engine = Engine::from_settings();
    let out = out_dir();
    let mut log =
        std::fs::File::create(out.join(format!("{}queues.log", engine.prefix()))).expect("the log");
    let mut judged = Vec::new();
    for name in queues.split(',') {
        let (queue, queue_ms, memory_limit) = named_queue(name);
        if queue == Queue::Cake && !netns::cake_available() {
            say(&format!("SUMMARY {name}: skipped, no sch_cake"), &mut log);
            continue;
        }
        let started = Instant::now();
        let topology = topology(name, rate, queue, queue_ms);
        if let Some(mb) = memory_limit {
            let (bandwidth, limit) = (format!("{rate}mbit"), format!("{mb}mb"));
            for (node, dev) in [(Node::Router1, "r1"), (Node::Router2, "r2")] {
                tc(
                    &topology,
                    node,
                    &[
                        "qdisc",
                        "replace",
                        "dev",
                        dev,
                        "root",
                        "cake",
                        "bandwidth",
                        &bandwidth,
                        "overhead",
                        "0",
                        "memlimit",
                        &limit,
                    ],
                );
            }
        }
        if let Some(ms) = policer_burst_ms(name) {
            police(&topology, rate, ms);
        }
        let change = rate_change().map(|(after, to_mbit)| {
            assert_eq!(
                queue,
                Queue::Tbf,
                "UDPSTP_GIGABIT_RATE_CHANGE changes a token bucket (tbf-N)"
            );
            RateChange {
                after,
                to_mbit,
                from_mbit: rate,
                queue_ms,
            }
        });
        let keys = engine.keys(name);
        let server = Server::start(
            &topology,
            &engine,
            &keys,
            out.join(format!("{}{name}-server.log", engine.prefix())),
        );
        for rep in 1..=reps {
            for direction in directions.split(',') {
                let rates = match change {
                    Some(c) => format!("{rate}to{}at{}", c.to_mbit, c.after.as_secs_f64()),
                    None => rate.to_string(),
                };
                let tag = match test_connections() {
                    1 => format!("{}{name}-{rates}-{direction}-{rep}", engine.prefix()),
                    n => format!("{}{name}-{rates}-tc{n}-{direction}-{rep}", engine.prefix()),
                };
                let one = search(
                    &topology, &engine, &keys, &server, direction, &tag, &out, change,
                );
                say(&format!("SUMMARY {}", one.line), &mut log);
                judged.push((tag, one));
            }
        }
        say(
            &format!(
                "SUMMARY {name}: {:.1} s for its Tests",
                started.elapsed().as_secs_f64()
            ),
            &mut log,
        );
    }
    let tally = |test: fn(&Judged) -> bool| judged.iter().filter(|(_, j)| test(j)).count();
    say(
        &format!(
            "SUMMARY {} Tests: {} confirmed Congestion, {} met the limit at their end, {} warned CongestionNeverConfirmed, {} of them where the Load Sender left at most 1% unsent, {} warned SubIntervalsAfterMaximumMissed",
            judged.len(),
            tally(|j| j.confirmed),
            tally(|j| j.met_at_end),
            tally(|j| j.warned),
            tally(|j| j.warned && j.little_unsent),
            tally(|j| j.after_maximum),
        ),
        &mut log,
    );
    for (tag, one) in &judged {
        assert!(
            one.complete,
            "{tag}: the Test didn't complete, or its capture missed Status PDUs"
        );
        assert!(
            one.agrees,
            "{tag}: the capture's verdict differs from the Client's: {}",
            one.line
        );
    }
}

// The harness's own reading of what it captured, Multi-Connection Tests
// included: no namespace needed.

#[test]
fn cake_queues_are_read_from_tc_class_output() {
    let classes = "class cake 8426:714 parent 8426: \n (dropped 51280, overlimits 0 requeues 0) \n backlog 0b 0p requeues 0\n  deficit -808 count 469 blue_prob 486539264 blue_timer 4.87s\nclass cake 8426:8ee parent 8426: \n (dropped 0, overlimits 0 requeues 0) \n backlog 0b 0p requeues 0\n  deficit 1486 count 0 blue_prob 0\n";
    assert_eq!(
        cake_queues(classes),
        vec![
            CakeQueue {
                class: "8426:714".into(),
                dropped: 51280,
                blue: 486_539_264.0 / 4_294_967_296.0,
            },
            CakeQueue {
                class: "8426:8ee".into(),
                dropped: 0,
                blue: 0.0,
            },
        ]
    );
    assert_eq!(
        blue_probability(classes),
        Some(486_539_264.0 / 4_294_967_296.0)
    );
    assert_eq!(blue_probability(""), None);
}

#[test]
fn captured_status_pdus_are_grouped_by_test_connection_in_order_of_first_capture() {
    let at = |port: u16| SocketAddr::from(([10, 1, 0, 2], port));
    let server = |port: u16| SocketAddr::from(([10, 3, 0, 2], port));
    let captured = [
        (at(40001), server(50001), 'a'),
        (at(40000), server(50000), 'x'),
        (at(40001), server(50001), 'b'),
        // Another destination is another Test Connection.
        (at(40001), server(50002), 'p'),
        (at(40000), server(50000), 'y'),
    ];
    assert_eq!(
        by_test_connection(captured),
        vec![vec!['a', 'b'], vec!['x', 'y'], vec!['p']]
    );
}

fn trial_intervals(classes: &str) -> Vec<TrialInterval> {
    classes
        .chars()
        .map(|letter| {
            let class = match letter {
                'C' => Class::Clean,
                'E' => Class::Errored,
                'H' => Class::Hold,
                _ => Class::HoldWithoutSample,
            };
            let lost = if class == Class::Errored { 10 } else { 0 };
            TrialInterval {
                class,
                lost,
                received: 90,
            }
        })
        .collect()
}

#[test]
fn a_test_connections_search_is_judged_as_the_client_judges_it() {
    // Three Errored Intervals in a row confirm Congestion.
    let confirmed = Searched::of(&trial_intervals("CCCEEECHh"), 3);
    assert_eq!(
        (
            confirmed.clean,
            confirmed.errored,
            confirmed.hold,
            confirmed.unsampled
        ),
        (4, 3, 1, 1)
    );
    assert_eq!(confirmed.longest, 3);
    assert!(confirmed.confirmed);
    // From the first Errored one on: EEECHh, 3 of 6, fewer than 4 errored.
    assert_eq!(
        (
            confirmed.window,
            confirmed.window_errored,
            confirmed.window_hold
        ),
        (6, 3, 2)
    );
    assert!(!confirmed.met_at_end);
    assert_eq!((confirmed.lost, confirmed.received), (30, 810));

    // Errored two at a time never confirm, but 4 of 8 meet the limit.
    let cycling = Searched::of(&trial_intervals("CCEECCEECC"), 3);
    assert!(!cycling.confirmed);
    assert_eq!((cycling.window, cycling.window_errored), (8, 4));
    assert!(cycling.met_at_end);

    // Only the last 64 are judged at the end.
    let early = format!("EEEE{}", "C".repeat(64));
    let late = Searched::of(&trial_intervals(&early), 5);
    assert_eq!(late.window, 0);
    assert!(!late.met_at_end);
    assert_eq!((late.last_lost, late.lost), (0, 40));
}

#[test]
fn several_test_connections_found_the_limit_where_any_one_did() {
    let none = Searched::of(&trial_intervals("CCCCEC"), 3);
    let one = Searched::of(&trial_intervals("CCEEEC"), 3);
    assert!(!none.found_the_limit());
    assert!(one.found_the_limit());
    let both = Searched::together(&[none, one]);
    assert!(both.found_the_limit());
    assert_eq!((both.clean, both.errored, both.longest), (8, 4, 3));
    assert!(!Searched::together(&[none, none]).found_the_limit());
    // One Test Connection's figures are its own.
    assert_eq!(Searched::together(&[one]), one);
}

#[test]
fn the_clients_warnings_are_read_from_its_json_results() {
    let results = serde_json::json!({"udpstp": {"Warnings": [
        {"Code": "BurstsSkipped", "Count": 3},
        {"Code": "SubIntervalsAfterMaximumMissed", "Message": "..."},
    ]}});
    assert_eq!(warning(&results), None);
    assert!(after_maximum_missed(&results));
    let none = serde_json::json!({"udpstp": {"Warnings": []}});
    assert!(!after_maximum_missed(&none));
    assert!(!after_maximum_missed(&serde_json::json!({})));
}
