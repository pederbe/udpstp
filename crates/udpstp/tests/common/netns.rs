//! Network namespaces for the S2c tier: a topology of a Client, two
//! routers and a Server, with a shaped bottleneck between the routers.
//!
//! The tests run inside a user, network, mount and PID namespace that
//! `scripts/netns-test.sh` sets up, so they need no root: `ip netns`, veth
//! pairs and qdiscs all belong to that user namespace. Everything they
//! create vanishes with it.
//!
//! ```text
//! cli  c0 ── r0  rtr1  r1 ══ r2  rtr2  r3 ── s0  srv
//!      10.1.0.2 .1     10.2.0.1 .2     10.3.0.1  .2 (and .3)
//!      fd01::2  ::1    fd02::1  ::2    fd03::1   ::2 (and ::3)
//! ```
//!
//! The rtr1–rtr2 link (══) is the bottleneck: a token bucket each way at
//! the rate asked for, counting IP bytes (the 14-byte Ethernet header is
//! taken off), with a queue of `queue_ms`, on an MTU of `middle_mtu`; or
//! the same token bucket with fq_codel as its queue, or CAKE's own shaper,
//! or no shaping at all (`queue`). The cli–rtr1 link adds `delay_ms` each
//! way, and on the way to the Client the delay variation of `slots`, if
//! any; its netem queues hold `netem_limit` packets if set.

// Each test binary that includes this module uses part of it.
#![allow(dead_code)]

use std::process::{Command, Stdio};

/// The nodes of the topology.
#[derive(Debug, Clone, Copy)]
pub enum Node {
    Client,
    Router1,
    Router2,
    Server,
}

/// How to build a topology.
#[derive(Debug, Clone, Copy)]
pub struct Spec {
    /// The bottleneck's rate at the IP layer, in Mbit/s.
    pub rate_mbit: u32,
    /// The bottleneck's queue, as the most delay it adds.
    pub queue_ms: u32,
    /// Delay added each way on the cli–rtr1 link.
    pub delay_ms: u32,
    /// The MTU of the rtr1–rtr2 link.
    pub middle_mtu: u32,
    /// The bottleneck's queue.
    pub queue: Queue,
    /// Delay variation on the way to the Client, if any.
    pub slots: Option<Slots>,
    /// How many packets the cli–rtr1 link's netem queues hold, if not
    /// netem's default of 1000: about 5 ms of `delay_ms` at 200 000 Load
    /// PDUs a second, which is under 2.5 Gbit/s, so above it netem itself
    /// would drop them.
    pub netem_limit: Option<u32>,
    /// NAT on rtr1 for the Client's IPv4 subnet, if any.
    pub nat: Option<Nat>,
    /// A stateful firewall on the Server, if any.
    pub firewall: Option<Firewall>,
}

/// The bottleneck's queue, each way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Queue {
    /// The token bucket's own drop-tail queue, `queue_ms` deep.
    Tbf,
    /// fq_codel under the token bucket: a queue per flow, from which CoDel
    /// drops while its delay stays above 5 ms (RFC 8290).
    FqCodel,
    /// CAKE's own shaper at the rate, counting IP bytes, with its COBALT
    /// queue management; only where the kernel has sch_cake
    /// ([`cake_available`]).
    Cake,
    /// No shaping: a FIFO of 100 000 packets, for what the hosts and links
    /// carry by themselves.
    Unshaped,
}

/// Delay variation that netem adds on rtr1's link to the Client, on top of
/// `delay_ms`: datagrams released in bursts, one every `min_ms` to
/// `max_ms`, as a Wi-Fi or DOCSIS link aggregates them (netem's `slot`),
/// each waiting up to the gap, in order, so every burst spans the same
/// range of delay. netem's `delay` jitter makes no steady delay variation:
/// kept in order (its `rate`), datagrams sent steadily wait behind the
/// slowest of them, so the variation depends on the few samples before,
/// which set the minimum it is measured from; let reorder, its late datagrams
/// count as lost where a Trial Interval ends between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slots {
    pub min_ms: u32,
    pub max_ms: u32,
}

/// A stateful nftables firewall in the Server's namespace: inbound
/// only to the Standard Port and to flows already established, outbound
/// anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Firewall {
    /// The Null Request, the first datagram from an Ephemeral Port, opens
    /// the way in for the Client's traffic to that port (RFC 9946 §5.5).
    StandardPortOnly,
    /// The negative control: the same, but a new flow from the Ephemeral
    /// Port range is dropped, so the Null Request never leaves.
    StandardPortOnlyWithoutNullRequest,
}

/// How rtr1 translates the Client's IPv4 address and ports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nat {
    /// One mapping per socket whatever it sends to: the Client's address
    /// rewritten statelessly, ports kept. nftables `masquerade` isn't
    /// this: Linux keeps the port for a socket's first flow and remaps the
    /// next one's, even with no clash.
    EndpointIndependent,
    /// nftables `masquerade fully-random`: a new, random port for each
    /// destination, so one socket's Setup and Activation Requests leave
    /// from different ports.
    PortDependent,
}

impl Default for Spec {
    fn default() -> Self {
        Self {
            rate_mbit: 50,
            // Deep enough that a search past the bottleneck raises delay
            // variation above the default upperThresh (90 ms) before loss
            // settles below seqErrThresh: a shallow queue holds the search
            // at a lossy row.
            queue_ms: 200,
            delay_ms: 5,
            middle_mtu: 1400,
            queue: Queue::Tbf,
            slots: None,
            netem_limit: None,
            nat: None,
            firewall: None,
        }
    }
}

/// Addresses in the topology.
pub mod addr {
    pub const SERVER_V4: &str = "10.3.0.2";
    /// The Server's second address, for replies from the arrival address.
    pub const SERVER_V4_SECOND: &str = "10.3.0.3";
    pub const SERVER_V6: &str = "fd03::2";
}

/// A topology of namespaces, deleted when dropped.
pub struct Topology {
    prefix: String,
    pub spec: Spec,
}

/// Runs a command that sets the topology up where the kernel allows it:
/// whether it succeeded.
fn succeeds(args: &[&str]) -> bool {
    Command::new(args[0])
        .args(&args[1..])
        .output()
        .is_ok_and(|output| output.status.success())
}

/// Runs a command that sets the topology up, panicking if it fails.
fn run(args: &[&str]) {
    let output = Command::new(args[0])
        .args(&args[1..])
        .output()
        .unwrap_or_else(|e| panic!("{args:?}: {e}"));
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

impl Topology {
    /// Builds the topology, with namespaces named after `test`.
    pub fn new(test: &str, spec: Spec) -> Self {
        let topology = Self {
            prefix: test.to_string(),
            spec,
        };
        for node in [Node::Client, Node::Router1, Node::Router2, Node::Server] {
            let ns = topology.ns(node);
            run(&["ip", "netns", "add", &ns]);
            run(&["ip", "-n", &ns, "link", "set", "lo", "up"]);
        }
        let links = [
            (Node::Client, "c0", Node::Router1, "r0", 1),
            (Node::Router1, "r1", Node::Router2, "r2", 2),
            (Node::Router2, "r3", Node::Server, "s0", 3),
        ];
        for (a, a_dev, b, b_dev, net) in links {
            let (a_ns, b_ns) = (topology.ns(a), topology.ns(b));
            run(&[
                "ip", "link", "add", "name", a_dev, "netns", &a_ns, "type", "veth", "peer", "name",
                b_dev, "netns", &b_ns,
            ]);
            // The left end of each link is .1/::1, except the Client's.
            let (a_host, b_host) = if net == 1 { (2, 1) } else { (1, 2) };
            for (ns, dev, host) in [(&a_ns, a_dev, a_host), (&b_ns, b_dev, b_host)] {
                run(&[
                    "ip",
                    "-n",
                    ns,
                    "addr",
                    "add",
                    &format!("10.{net}.0.{host}/24"),
                    "dev",
                    dev,
                ]);
                run(&[
                    "ip",
                    "-n",
                    ns,
                    "-6",
                    "addr",
                    "add",
                    &format!("fd0{net}::{host}/64"),
                    "dev",
                    dev,
                    "nodad",
                ]);
                if net == 2 {
                    run(&[
                        "ip",
                        "-n",
                        ns,
                        "link",
                        "set",
                        dev,
                        "mtu",
                        &spec.middle_mtu.to_string(),
                    ]);
                }
                run(&["ip", "-n", ns, "link", "set", dev, "up"]);
                // Segmentation offloads would let the qdiscs see
                // super-packets; tolerate a missing ethtool.
                let _ = Command::new("ip")
                    .args(["netns", "exec", ns, "ethtool", "-K", dev])
                    .args(["gro", "off", "gso", "off", "tso", "off"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
                // A UDP GSO send stays one super-packet across veth
                // links, which `gso off` doesn't split. At most one segment
                // per packet makes the kernel split it before the device,
                // so captures and qdiscs see the datagrams a wire carries.
                let _ = Command::new("ip")
                    .args(["-n", ns, "link", "set", "dev", dev, "gso_max_segs", "1"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        }
        let server_ns = topology.ns(Node::Server);
        run(&[
            "ip",
            "-n",
            &server_ns,
            "addr",
            "add",
            "10.3.0.3/24",
            "dev",
            "s0",
        ]);
        run(&[
            "ip",
            "-n",
            &server_ns,
            "-6",
            "addr",
            "add",
            "fd03::3/64",
            "dev",
            "s0",
            "nodad",
        ]);
        let routes: [(Node, &[&str]); 6] = [
            (Node::Client, &["default", "via", "10.1.0.1"]),
            (Node::Client, &["-6", "default", "via", "fd01::1"]),
            (Node::Server, &["default", "via", "10.3.0.1"]),
            (Node::Server, &["-6", "default", "via", "fd03::1"]),
            (Node::Router1, &["10.3.0.0/24", "via", "10.2.0.2"]),
            (Node::Router2, &["10.1.0.0/24", "via", "10.2.0.1"]),
        ];
        for (node, route) in routes {
            let ns = topology.ns(node);
            let mut args = vec!["ip", "-n", &ns];
            let (family, rest) = match route.split_first() {
                Some((&"-6", rest)) => (Some("-6"), rest),
                _ => (None, route),
            };
            args.extend(family);
            args.extend(["route", "add"]);
            args.extend(rest);
            run(&args);
        }
        run(&[
            "ip",
            "-n",
            &topology.ns(Node::Router1),
            "-6",
            "route",
            "add",
            "fd03::/64",
            "via",
            "fd02::2",
        ]);
        run(&[
            "ip",
            "-n",
            &topology.ns(Node::Router2),
            "-6",
            "route",
            "add",
            "fd01::/64",
            "via",
            "fd02::1",
        ]);
        for router in [Node::Router1, Node::Router2] {
            let ns = topology.ns(router);
            for key in ["net.ipv4.ip_forward=1", "net.ipv6.conf.all.forwarding=1"] {
                run(&["ip", "netns", "exec", &ns, "sysctl", "-qw", key]);
            }
        }
        let rate = format!("{}mbit", spec.rate_mbit);
        let queue = format!("{}ms", spec.queue_ms);
        for (node, dev) in [(Node::Router1, "r1"), (Node::Router2, "r2")] {
            let ns = topology.ns(node);
            let tc = |args: &[&str]| {
                let mut command =
                    vec!["ip", "netns", "exec", &ns, "tc", "qdisc", "add", "dev", dev];
                command.extend_from_slice(args);
                run(&command);
            };
            let tbf = [
                "stab", "overhead", "-14", "tbf", "rate", &rate, "burst", "32kb", "latency", &queue,
            ];
            match spec.queue {
                Queue::Tbf => tc(&[&["root"][..], &tbf].concat()),
                // The token bucket's queue replaced by fq_codel's.
                Queue::FqCodel => {
                    tc(&[&["root", "handle", "1:"][..], &tbf].concat());
                    tc(&["parent", "1:1", "fq_codel"]);
                }
                // With an overhead set, CAKE counts from the IP header, so
                // `overhead 0` counts IP bytes, as stab makes the token
                // bucket do.
                Queue::Cake => tc(&["root", "cake", "bandwidth", &rate, "overhead", "0"]),
                Queue::Unshaped => tc(&["root", "pfifo", "limit", "100000"]),
            }
        }
        let delay = format!("{}ms", spec.delay_ms);
        let slots = spec
            .slots
            .map(|s| [format!("{}ms", s.min_ms), format!("{}ms", s.max_ms)]);
        let limit = spec.netem_limit.map(|packets| packets.to_string());
        for (node, dev) in [(Node::Client, "c0"), (Node::Router1, "r0")] {
            let ns = topology.ns(node);
            let mut netem = vec![
                "ip", "netns", "exec", &ns, "tc", "qdisc", "add", "dev", dev, "root", "netem",
                "delay", &delay,
            ];
            // On the way to the Client only.
            if let (Some([min, max]), Node::Router1) = (&slots, node) {
                netem.extend(["slot", min, max]);
            }
            if let Some(limit) = &limit {
                netem.extend(["limit", limit]);
            }
            run(&netem);
        }
        if let Some(nat) = spec.nat {
            let ns = topology.ns(Node::Router1);
            // `UDPSTP_NFT`: nft, which scripts/netns-test.sh fetches where
            // it isn't installed.
            let nft = std::env::var("UDPSTP_NFT").unwrap_or_else(|_| "nft".to_string());
            let nft_rules = |rules: &[&str]| {
                for rule in rules {
                    run(&["ip", "netns", "exec", &ns, &nft, rule]);
                }
            };
            match nat {
                // A stateless NAT on r1: the Client's address becomes
                // rtr1's on the way out, and UDP to rtr1's high ports goes
                // to the Client, ports kept. tc's `nat` action does it,
                // fixing the checksums; where the kernel lacks it,
                // nftables' payload rewrites do, which Linux 7 refuses in
                // an unprivileged namespace (EPERM) and Linux 6 allows.
                Nat::EndpointIndependent => {
                    let tc = |args: &[&str]| {
                        let mut command = vec!["ip", "netns", "exec", &ns, "tc"];
                        command.extend_from_slice(args);
                        succeeds(&command)
                    };
                    let by_tc = tc(&["qdisc", "add", "dev", "r1", "clsact"])
                        && tc(&[
                            "filter",
                            "add",
                            "dev",
                            "r1",
                            "ingress",
                            "protocol",
                            "ip",
                            "flower",
                            "ip_proto",
                            "udp",
                            "dst_ip",
                            "10.2.0.1",
                            "dst_port",
                            "1024-65535",
                            "action",
                            "nat",
                            "ingress",
                            "10.2.0.1/32",
                            "10.1.0.2",
                        ])
                        && tc(&[
                            "filter",
                            "add",
                            "dev",
                            "r1",
                            "egress",
                            "protocol",
                            "ip",
                            "matchall",
                            "action",
                            "nat",
                            "egress",
                            "10.1.0.2/32",
                            "10.2.0.1",
                        ]);
                    if !by_tc {
                        // Its filters go with it, so no half of a NAT stays.
                        tc(&["qdisc", "del", "dev", "r1", "clsact"]);
                        nft_rules(&[
                            "add table ip eim",
                            "add chain ip eim pre { type filter hook prerouting priority -300 ; }",
                            "add rule ip eim pre ip daddr 10.2.0.1 udp dport 1024-65535 ip daddr set 10.1.0.2",
                            "add chain ip eim post { type filter hook postrouting priority 300 ; }",
                            "add rule ip eim post oifname r1 ip saddr 10.1.0.2 ip saddr set 10.2.0.1",
                        ]);
                    }
                }
                Nat::PortDependent => nft_rules(&[
                    "add table ip nat",
                    "add chain ip nat post { type nat hook postrouting priority 100 ; }",
                    "add rule ip nat post ip saddr 10.1.0.0/24 oifname r1 masquerade fully-random",
                ]),
            }
        }
        if let Some(firewall) = spec.firewall {
            let nft = std::env::var("UDPSTP_NFT").unwrap_or_else(|_| "nft".to_string());
            let ns = topology.ns(Node::Server);
            let mut rules = vec![
                "add table inet fw",
                "add chain inet fw in { type filter hook input priority 0 ; policy drop ; }",
                "add rule inet fw in iif lo accept",
                "add rule inet fw in ct state established,related accept",
                "add rule inet fw in udp dport 24601 accept",
                // ICMPv6 neighbour discovery, or nothing reaches the Server
                // over IPv6.
                "add rule inet fw in meta l4proto ipv6-icmp accept",
            ];
            if firewall == Firewall::StandardPortOnlyWithoutNullRequest {
                rules.extend([
                    "add chain inet fw out { type filter hook output priority 0 ; policy accept ; }",
                    "add rule inet fw out udp sport 49152-65535 ct state new drop",
                ]);
            }
            for rule in rules {
                run(&["ip", "netns", "exec", &ns, &nft, rule]);
            }
        }
        topology
    }

    /// The name of a node's namespace.
    pub fn ns(&self, node: Node) -> String {
        let name = match node {
            Node::Client => "cli",
            Node::Router1 => "rtr1",
            Node::Router2 => "rtr2",
            Node::Server => "srv",
        };
        format!("{}-{name}", self.prefix)
    }

    /// A command that runs `program` in a node's namespace.
    pub fn command(&self, node: Node, program: &str) -> Command {
        let mut command = Command::new("ip");
        command.args(["netns", "exec", &self.ns(node), program]);
        command
    }
}

impl Drop for Topology {
    fn drop(&mut self) {
        for node in [Node::Client, Node::Router1, Node::Router2, Node::Server] {
            let _ = Command::new("ip")
                .args(["netns", "del", &self.ns(node)])
                .status();
        }
    }
}

/// Whether the tests run in the namespace `scripts/netns-test.sh` sets up.
/// Outside it a test is skipped, unless `UDPSTP_NETNS_REQUIRED` is set, in
/// which case it fails: a required run can't pass by skipping.
pub fn available(test: &str) -> bool {
    if std::env::var_os("UDPSTP_NETNS").is_some() {
        return true;
    }
    assert!(
        std::env::var_os("UDPSTP_NETNS_REQUIRED").is_none(),
        "{test}: UDPSTP_NETNS_REQUIRED is set, but the test isn't in the namespace scripts/netns-test.sh sets up"
    );
    eprintln!("{test}: skipped; run scripts/netns-test.sh");
    false
}

/// Whether capacity is checked to the tier's strict tolerance (5%), as CI
/// and a quiet host do, or only loosely, as the pre-push hook does on a
/// shared machine.
pub fn strict() -> bool {
    std::env::var_os("UDPSTP_NETNS_STRICT").is_some()
}

/// Whether the kernel has CAKE (sch_cake), for [`Queue::Cake`]: tried on
/// the loopback device of a namespace of its own.
pub fn cake_available() -> bool {
    let ns = "cake-probe";
    let available = succeeds(&["ip", "netns", "add", ns])
        && succeeds(&[
            "ip",
            "netns",
            "exec",
            ns,
            "tc",
            "qdisc",
            "add",
            "dev",
            "lo",
            "root",
            "cake",
            "bandwidth",
            "10mbit",
        ]);
    let _ = Command::new("ip").args(["netns", "del", ns]).status();
    available
}
