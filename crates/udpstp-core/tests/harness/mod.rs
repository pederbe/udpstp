//! Seam-1 test harness: scenarios drive a Client core and a Server core
//! through a deterministic simulator. Tests use only this module, never the
//! core API directly, so interface changes are absorbed here.
//!
//! Every failed check, and every panic inside the simulation, prints the seed
//! and the trace hash. Re-running
//! the scenario with the same seed reproduces the same trace exactly.

#![allow(dead_code)]

pub mod requests;
mod scripted;
mod sim;
pub mod status_script;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::time::Duration;

use udpstp_core::{
    ClientAuth, ClientConfig, DelayVarStatistic, Direction, Event, KeyRole, SocketId, TestPlan,
};
use udpstp_wire::{ActivationPdu, activation};

pub use scripted::{ClientReplyScript, ClientScript, ScriptClock, ServerScript};
// Tests name drop reasons through the harness, not the core API.
pub use sim::TraceEntry;
#[allow(unused_imports)]
pub use udpstp_core::{
    ActivationParameter, ActivationRejection, Coercion, ControlExchange, ControlPhaseFailure,
    DataEvent, KeyEntry, KeyTable, KeyTableError, Lifetime, ResponseIgnoreReason, RowRequest,
    ServerConfig, SetupIgnoreReason, SilentRejectionReason,
};

/// Every host's wall clock at the start, unless a scenario sets another
/// (2026-09-21T18:00:00Z).
pub const WALL_START_UNIX_SECS: u64 = 1_790_013_600;

/// Where the simulated Server listens.
pub const SERVER_ADDR: &str = "192.0.2.1:24601";
/// Where the first simulated Client sends from. Further Clients in the same
/// Test use the same address with the following ports.
pub const CLIENT_ADDR: &str = "198.51.100.2:40000";

/// The Server and first Client in an IPv6 scenario (documentation
/// addresses, RFC 3849).
pub const SERVER_ADDR_V6: &str = "[2001:db8::1]:24601";
pub const CLIENT_ADDR_V6: &str = "[2001:db8::2]:40000";

/// The address of the `k`th Server added with `Scenario::add_server`.
pub fn extra_server_addr(k: usize, ipv6: bool) -> SocketAddr {
    let addr: SocketAddr = if ipv6 { SERVER_ADDR_V6 } else { SERVER_ADDR }
        .parse()
        .unwrap();
    let ip = match addr.ip() {
        std::net::IpAddr::V4(ip) => {
            let [a, b, c, d] = ip.octets();
            std::net::IpAddr::from([a, b, c, d + 1 + k as u8])
        }
        std::net::IpAddr::V6(ip) => {
            let mut octets = ip.octets();
            octets[15] += 1 + k as u8;
            std::net::IpAddr::from(octets)
        }
    };
    SocketAddr::new(ip, addr.port())
}

/// A window in which the network drops PDUs of one kind.
#[derive(Debug, Clone, Copy)]
pub struct DropWindow {
    pub pdu_id: [u8; 2],
    pub from: Duration,
    pub until: Option<Duration>,
}

/// One Client in the scenario. Each runs a Multi-Connection Coordinator, as
/// in production, and a Coordinator runs a whole Test from one host: the
/// first Client's Test, or, for each Client added after it, a Test of its
/// own (another mcIdent). A Test spread over several Clients, or one our
/// Coordinator would never make, is played by scripted Clients
/// (`Scenario::add_scripted_client`) sending requests built from the RFC
/// (`requests`). The Test's rules themselves are Admission's own tests
/// (src/admission.rs): scenarios here prove how they are wired.
#[derive(Clone)]
pub struct ClientSpec {
    pub direction: Direction,
    /// A Test of its own (another mcIdent), not the first Client's: every
    /// Client after the first that isn't scripted.
    pub own_test: bool,
    /// When it starts.
    pub start_at: Duration,
    /// Its authentication, instead of the scenario's.
    pub auth: Option<ClientAuth>,
    /// Its maxBandwidth, instead of the scenario's.
    pub max_bandwidth: Option<u16>,
}

impl ClientSpec {
    /// A Client in the first Client's Test, starting at once: the first
    /// Client, or a scripted one.
    pub fn in_same_test(direction: Direction) -> Self {
        Self {
            direction,
            own_test: false,
            start_at: Duration::ZERO,
            auth: None,
            max_bandwidth: None,
        }
    }
}

/// A PDU the network replays.
#[derive(Debug, Clone, Copy)]
pub struct Replay {
    pub pdu_id: [u8; 2],
    pub client: usize,
    pub at: Duration,
    pub to_sibling: bool,
}

/// A change a test makes to settings the harness built.
type Patch<T> = Box<dyn Fn(&mut T)>;

/// A scenario to run: the Server's and Client's settings plus a seed.
pub struct Scenario {
    seed: u64,
    server_allows_unauthenticated: bool,
    /// The Server's Key Table.
    server_keys: KeyTable,
    /// How every Client in the scenario authenticates.
    client_auth: Option<ClientAuth>,
    /// The Sending Rate Table row every Client asks for.
    client_row_request: RowRequest,
    /// Changes to every Client's other settings, in order.
    client_config: Vec<Patch<ClientConfig>>,
    /// Changes to the plan of the first Client's Test, in order.
    test_plan: Vec<Patch<TestPlan>>,
    /// Steps of the Server's wall clock: (when, seconds ahead).
    server_wall_steps: Vec<(Duration, i64)>,
    client_wall_steps: Vec<(Duration, i64)>,
    client_drift_ppm: i64,
    client_slew: Option<(Duration, i64)>,
    client_kernel_shift_ns: i64,
    client_steps_during_reads: bool,
    path_delay: Option<(Duration, Duration)>,
    bottleneck: Option<(u64, u64)>,
    server_delay_var: DelayVarStatistic,
    server_tests_per_source: usize,
    /// Changes to the Server's other settings.
    server_config: Option<fn(&mut ServerConfig)>,
    /// The maxBandwidth every Client states, in Mbps.
    client_max_bandwidth: u16,
    setup_source: Option<std::net::IpAddr>,
    setup_local: Option<std::net::IpAddr>,
    activation_source: Option<std::net::IpAddr>,
    /// Another address of the Server's host, which all its replies come
    /// from.
    server_alias: Option<std::net::IpAddr>,
    server_scope: Option<u32>,
    nat_port: Option<u16>,
    partial_sends: Option<(u64, u32)>,
    flip_byte: Option<([u8; 2], usize, Option<std::net::IpAddr>)>,
    /// When the network starts flipping that byte.
    flip_byte_from: Duration,
    /// Bytes the network writes into Setup Requests from the Client at
    /// this index, at this offset.
    setup_writes: Vec<(usize, usize, Vec<u8>)>,
    /// The network delivers every Setup Request twice.
    duplicate_setup_requests: bool,
    /// The network replays a PDU: which pduId, from which Client, when,
    /// and whether to the Server's newest other Test Connection.
    replay: Option<Replay>,
    /// How many of the Server's first socket opens fail.
    server_socket_open_failures: usize,
    client_server: Option<SocketAddr>,
    /// The Clients' time limit for the Setup and Activation exchanges.
    client_time_limit: Duration,
    /// Further Servers, each with the first's settings changed.
    extra_servers: Vec<fn(&mut ServerConfig)>,
    /// Hosts the network cuts off, from when.
    blackholes: Vec<(std::net::IpAddr, Duration)>,
    /// One bottleneck per receiving host, not per pair of hosts.
    bottleneck_per_receiver: bool,
    /// Only datagrams to this address pass the bottleneck.
    bottleneck_to: Option<SocketAddr>,
    /// A window in which the Server's Data Phase Engine runs nothing.
    server_engine_stall: Option<(Duration, Duration)>,
    client_engine_stall: Option<(Duration, Duration)>,
    client_stops_at: Option<Duration>,
    /// When the Client user interrupts the Test.
    client_interrupted_at: Option<Duration>,
    /// When the Server administrator stops the Server.
    server_shuts_down_at: Option<Duration>,
    /// The Server uses the optional checksum.
    checksum: bool,
    /// The trace keeps Load PDU content, not just headers.
    keep_load_content: bool,
    reorder_load_pdus: Option<(u64, u32)>,
    clear_test_action: bool,
    ipv6: bool,
    drops: Vec<DropWindow>,
    status_test_action_from: Option<(Duration, u8)>,
    duplicate_load_pdus: Option<u64>,
    clients: Vec<ClientSpec>,
    runtime_closes_test_connections: bool,
    /// Wall-clock time at the start (default 2026-09-21T18:00:00Z).
    wall_start: Option<Duration>,
    /// How far the Clients' wall clocks are ahead of the Server's, in seconds.
    client_wall_offset: i64,
    /// The scripted Clients, by their place among the Clients: what each
    /// sends as it starts, and how it answers.
    scripted_clients: BTreeMap<usize, (ClientScript, Option<ClientReplyScript>)>,
    scripted_server: Option<ServerScript>,
}

impl Scenario {
    /// A scenario with default settings at both ends (Unauthenticated Mode off).
    pub fn new(seed: u64) -> Self {
        Self {
            seed,
            server_allows_unauthenticated: false,
            server_keys: KeyTable::default(),
            client_auth: None,
            client_row_request: RowRequest::ServerDefault,
            client_config: Vec::new(),
            test_plan: Vec::new(),
            server_wall_steps: Vec::new(),
            client_wall_steps: Vec::new(),
            client_drift_ppm: 0,
            client_slew: None,
            client_kernel_shift_ns: 0,
            client_steps_during_reads: false,
            path_delay: None,
            bottleneck: None,
            server_delay_var: DelayVarStatistic::default(),
            server_tests_per_source: 1,
            server_config: None,
            client_max_bandwidth: 0,
            setup_source: None,
            setup_local: None,
            activation_source: None,
            server_alias: None,
            server_scope: None,
            nat_port: None,
            partial_sends: None,
            flip_byte: None,
            flip_byte_from: Duration::ZERO,
            setup_writes: Vec::new(),
            duplicate_setup_requests: false,
            replay: None,
            server_socket_open_failures: 0,
            client_server: None,
            client_time_limit: Duration::from_secs(3),
            extra_servers: Vec::new(),
            blackholes: Vec::new(),
            bottleneck_per_receiver: false,
            bottleneck_to: None,
            server_engine_stall: None,
            client_engine_stall: None,
            client_stops_at: None,
            client_interrupted_at: None,
            server_shuts_down_at: None,
            checksum: false,
            keep_load_content: false,
            reorder_load_pdus: None,
            clear_test_action: false,
            ipv6: false,
            drops: Vec::new(),
            status_test_action_from: None,
            duplicate_load_pdus: None,
            clients: vec![ClientSpec::in_same_test(Direction::Downstream)],
            runtime_closes_test_connections: false,
            wall_start: None,
            client_wall_offset: 0,
            scripted_clients: BTreeMap::new(),
            scripted_server: None,
        }
    }

    /// Every host's wall clock starts at `unix_secs`.
    pub fn wall_start(mut self, unix_secs: u64) -> Self {
        self.wall_start = Some(Duration::from_secs(unix_secs));
        self
    }

    /// The Clients' wall clocks run `secs` ahead of the Server's (negative: behind).
    pub fn client_clock_offset(mut self, secs: i64) -> Self {
        self.client_wall_offset = secs;
        self
    }

    /// Replaces our first Client with a scripted one that sends one
    /// datagram.
    pub fn scripted_client(
        mut self,
        script: impl FnOnce(ScriptClock) -> Vec<u8> + 'static,
    ) -> Self {
        self.scripted_clients.insert(0, (Box::new(script), None));
        self
    }

    /// Replaces our first Client with a scripted one that sends one
    /// datagram and then answers each datagram it receives with a datagram
    /// to the given port on the Server's address (or stays silent).
    pub fn scripted_client_replying(
        mut self,
        start: impl FnOnce(ScriptClock) -> Vec<u8> + 'static,
        replies: impl FnMut(ScriptClock, &[u8]) -> Option<(u16, Vec<u8>)> + 'static,
    ) -> Self {
        self.scripted_clients
            .insert(0, (Box::new(start), Some(Box::new(replies))));
        self
    }

    /// Adds a scripted Client, from the first Client's address and the
    /// next port, that sends one datagram at `start_at` and answers as
    /// [`Self::scripted_client_replying`]'s does: one Test Connection of a
    /// Test spread over several Clients, built with `requests`.
    pub fn add_scripted_client(
        mut self,
        start_at: Duration,
        start: impl FnOnce(ScriptClock) -> Vec<u8> + 'static,
        replies: impl FnMut(ScriptClock, &[u8]) -> Option<(u16, Vec<u8>)> + 'static,
    ) -> Self {
        let index = self.clients.len();
        self.clients.push(ClientSpec {
            start_at,
            ..ClientSpec::in_same_test(Direction::Downstream)
        });
        self.scripted_clients
            .insert(index, (Box::new(start), Some(Box::new(replies))));
        self
    }

    /// Replaces our Server with a scripted one that answers each datagram.
    pub fn scripted_server(
        self,
        mut script: impl FnMut(ScriptClock, &[u8]) -> Option<Vec<u8>> + 'static,
    ) -> Self {
        self.scripted_server_replying_many(move |clock, pdu| {
            script(clock, pdu).into_iter().collect()
        })
    }

    /// Replaces our Server with a scripted one that answers each datagram
    /// with any number of datagrams, in order.
    pub fn scripted_server_replying_many(
        mut self,
        script: impl FnMut(ScriptClock, &[u8]) -> Vec<Vec<u8>> + 'static,
    ) -> Self {
        self.scripted_server = Some(Box::new(script));
        self
    }

    /// The simulated runtime closes every Test Connection socket the Server
    /// opens once the network is idle, as a Data Phase engine exiting would.
    pub fn runtime_closes_test_connections(mut self) -> Self {
        self.runtime_closes_test_connections = true;
        self
    }

    /// The Server opts in to unauthenticated Tests.
    pub fn server_allows_unauthenticated(mut self) -> Self {
        self.server_allows_unauthenticated = true;
        self
    }

    /// Both ends use Authentication Mode 1 with the same Shared Key.
    pub fn mode1(self, key_id: u8, key: &[u8]) -> Self {
        self.server_key(key_id, key).client_mode1(key_id, key)
    }

    /// Mode 1 with an Operator Key, valid at all times, on both ends.
    pub fn operator(mut self, key_id: u8, key: &[u8]) -> Self {
        let mut row = always(key_id, key);
        row.role = KeyRole::Operator {
            max_test_interval: None,
        };
        self.server_keys
            .insert(row)
            .expect("the harness adds one row per keyId");
        self.client_mode1(key_id, key)
    }

    /// Adds an Operator Key allowing Tests up to `max_test_interval`
    /// seconds, valid at all times, to the Server's Key Table only.
    pub fn server_operator_key(
        mut self,
        key_id: u8,
        key: &[u8],
        max_test_interval: Option<u64>,
    ) -> Self {
        let mut row = always(key_id, key);
        row.role = KeyRole::Operator {
            max_test_interval: max_test_interval.map(Duration::from_secs),
        };
        self.server_keys
            .insert(row)
            .expect("the harness adds one row per keyId");
        self
    }

    /// Changes every Client's settings for its Test, after the scenario's
    /// own and before each Client's (`ClientSpec`). What the
    /// Multi-Connection Coordinator decides per Test Connection (the
    /// Server, mcIndex and mcCount, the maxBandwidth share and the time
    /// limit) comes from the Test's plan: [`Self::test_plan`].
    pub fn client_config(mut self, change: impl Fn(&mut ClientConfig) + 'static) -> Self {
        self.client_config.push(Box::new(change));
        self
    }

    /// Changes the plan of the Test the first Client runs through its
    /// Multi-Connection Coordinator: one Test Connection, which must
    /// succeed, to the Servers in order (the first, then those added with
    /// [`Self::add_server`]), with the scenario's time limit and
    /// maxBandwidth, by default.
    pub fn test_plan(mut self, change: impl Fn(&mut TestPlan) + 'static) -> Self {
        self.test_plan.push(Box::new(change));
        self
    }

    /// Adds a Shared Key, valid at all times, to the Server's Key Table.
    pub fn server_key(mut self, key_id: u8, key: &[u8]) -> Self {
        self.server_keys
            .insert(always(key_id, key))
            .expect("the harness adds one row per keyId");
        self
    }

    /// The Server's Key Table.
    pub fn server_keys(mut self, keys: KeyTable) -> Self {
        self.server_keys = keys;
        self
    }

    /// The Clients authenticate in mode 1 with this Shared Key, valid at
    /// all times.
    pub fn client_mode1(self, key_id: u8, key: &[u8]) -> Self {
        let mut keys = KeyTable::default();
        keys.insert(always(key_id, key)).expect("one row");
        self.client_keys(keys, None)
    }

    /// The Clients authenticate in mode 1 with this Key Table, limited to
    /// keyId `pinned` when it is given.
    pub fn client_keys(mut self, keys: KeyTable, pinned: Option<u8>) -> Self {
        self.client_auth = Some(ClientAuth::Mode1 {
            keys,
            key_id: pinned,
        });
        self
    }

    /// The Clients authenticate in mode 2: mode 1, and Status PDUs too.
    pub fn client_mode2(mut self, key_id: u8, key: &[u8]) -> Self {
        let mut keys = KeyTable::default();
        keys.insert(always(key_id, key)).expect("one row");
        self.client_auth = Some(ClientAuth::Mode2 { keys, key_id: None });
        self
    }

    /// Both ends use Authentication Mode 2 with the same Operator Key, so
    /// Fixed-Rate Tests run.
    pub fn operator_mode2(self, key_id: u8, key: &[u8]) -> Self {
        self.operator(key_id, key).client_mode2(key_id, key)
    }

    /// The Clients opt in to an unauthenticated Test.
    pub fn client_unauthenticated(mut self) -> Self {
        self.client_auth = Some(ClientAuth::Unauthenticated);
        self
    }

    /// The direction of the first Client's Test (Downstream by default).
    pub fn client_direction(mut self, direction: Direction) -> Self {
        self.clients[0].direction = direction;
        self
    }

    /// The Clients ask for this Sending Rate Table row (the Server's
    /// default by default).
    pub fn client_requests_row(mut self, request: RowRequest) -> Self {
        self.client_row_request = request;
        self
    }

    /// Both ends use the optional checksum (RFC 9946 §5.6).
    pub fn checksum(self) -> Self {
        self.server_checksum().client_config(|c| c.checksum = true)
    }

    /// Only the Server uses the optional checksum.
    pub fn server_checksum(mut self) -> Self {
        self.checksum = true;
        self
    }

    /// The trace keeps each Load PDU's content, not just its header.
    pub fn keep_load_content(mut self) -> Self {
        self.keep_load_content = true;
        self
    }

    /// The network holds back every `every`th Load PDU and delivers it
    /// after the next `behind` Load PDUs, counting Load PDUs on all paths.
    /// Those still held when the run ends are delivered then.
    pub fn reorder_load_pdus(mut self, every: u64, behind: u32) -> Self {
        assert!(
            every > 0 && behind > 0,
            "reordering needs every, behind > 0"
        );
        self.reorder_load_pdus = Some((every, behind));
        self
    }

    /// The network drops every PDU with this pduId (for example
    /// `[0xBE, 0xEF]` for Load PDUs) from `from` until `until`, or for good.
    pub fn network_drops(
        mut self,
        pdu_id: [u8; 2],
        from: Duration,
        until: Option<Duration>,
    ) -> Self {
        self.drops.push(DropWindow {
            pdu_id,
            from,
            until,
        });
        self
    }

    /// The Server and Clients use IPv6 addresses ([`SERVER_ADDR_V6`],
    /// [`CLIENT_ADDR_V6`]).
    pub fn ipv6(mut self) -> Self {
        self.ipv6 = true;
        self
    }

    /// The network clears every Load and Status PDU's testAction, as an
    /// on-path attacker might (RFC 9946 §11).
    pub fn network_clears_test_action(mut self) -> Self {
        self.clear_test_action = true;
        self
    }

    /// From `at`, the network sets every Status PDU's testAction to `value`,
    /// as an OB-UDPST Client's STOP1 or an on-path attacker would.
    pub fn network_sets_status_test_action_from(mut self, at: Duration, value: u8) -> Self {
        self.status_test_action_from = Some((at, value));
        self
    }

    /// The network delivers every `every`th Load PDU twice in a row, unless
    /// it holds that one back to reorder it.
    pub fn duplicate_load_pdus(mut self, every: u64) -> Self {
        assert!(every > 0, "duplication needs every > 0");
        self.duplicate_load_pdus = Some(every);
        self
    }

    /// The Clients' wall clocks step `secs` ahead (negative: back) at `at`.
    pub fn client_wall_step(mut self, at: Duration, secs: i64) -> Self {
        self.client_wall_steps.push((at, secs));
        self
    }

    /// The Clients' clocks, monotonic and wall alike, run `ppm` parts per
    /// million fast (negative: slow) against the Server's.
    pub fn client_clock_drift_ppm(mut self, ppm: i64) -> Self {
        self.client_drift_ppm = ppm;
        self
    }

    /// From `at`, the Clients' wall clocks alone run `ppm` parts per
    /// million fast, as NTP slewing does; their monotonic clocks keep time.
    pub fn client_wall_slew(mut self, at: Duration, ppm: i64) -> Self {
        self.client_slew = Some((at, ppm));
        self
    }

    /// Every datagram takes `one_way` plus up to `jitter` (drawn from the
    /// seed) to arrive, in order on each path.
    pub fn path_delay(mut self, one_way: Duration, jitter: Duration) -> Self {
        self.path_delay = Some((one_way, jitter));
        self
    }

    /// Each direction between two hosts passes a bottleneck of `rate_bps`
    /// at the IP layer (IP and UDP headers count), with a drop-tail buffer
    /// of `buffer_bytes` that holds the datagram being sent too, before the
    /// path delay. Every Test Connection between the two hosts shares it.
    pub fn bottleneck(mut self, rate_bps: u64, buffer_bytes: u64) -> Self {
        assert!(rate_bps > 0, "a bottleneck has a rate");
        assert!(
            buffer_bytes >= 1500,
            "a buffer smaller than a datagram drops everything"
        );
        self.bottleneck = Some((rate_bps, buffer_bytes));
        self
    }

    /// The bottleneck is the receiving host's access link: everything
    /// toward one host shares it, whichever Server sends.
    pub fn bottleneck_per_receiver(mut self) -> Self {
        self.bottleneck_per_receiver = true;
        self
    }

    /// Only datagrams to `addr` pass the bottleneck, as one Test
    /// Connection's path; the others are not held up.
    pub fn bottleneck_only_to(mut self, addr: SocketAddr) -> Self {
        self.bottleneck_to = Some(addr);
        self
    }

    /// Every Setup PDU arrives as if sent from `ip` (spoofed by an
    /// attacker, say), port unchanged.
    pub fn setup_pdus_from(mut self, ip: std::net::IpAddr) -> Self {
        self.setup_source = Some(ip);
        self
    }

    /// Every Setup PDU arrives as if sent to `ip` (a group or broadcast
    /// address the receiving host listens on), port unchanged.
    pub fn setup_pdus_to(mut self, ip: std::net::IpAddr) -> Self {
        self.setup_local = Some(ip);
        self
    }

    /// The Server's host also has address `ip` and sends everything from
    /// it, as a multi-homed host's wildcard sockets may; datagrams to `ip`
    /// reach it. The Clients still send their Setup Requests to its first
    /// address.
    pub fn server_answers_from(mut self, ip: std::net::IpAddr) -> Self {
        self.server_alias = Some(ip);
        self
    }

    /// The IPv6 interface on which the Client receives Server PDUs.
    pub fn server_answers_with_scope(mut self, scope: u32) -> Self {
        self.server_scope = Some(scope);
        self
    }

    /// A NAT in front of the first Client maps it to `port` for everything
    /// after its Setup Requests, as a NAT that rebinds may: the Server sees
    /// the Activation Request and all later PDUs from that port.
    pub fn client_nat_rebinds_to(mut self, port: u16) -> Self {
        self.nat_port = Some(port);
        self
    }

    /// Every Activation PDU arrives as if sent from `ip`, port unchanged.
    pub fn activation_pdus_from(mut self, ip: std::net::IpAddr) -> Self {
        self.activation_source = Some(ip);
        self
    }

    /// Every `every`th Send Plan, the Load Sender's host takes only `sent`
    /// of its datagrams (a full send buffer).
    pub fn partial_sends(mut self, every: u64, sent: u32) -> Self {
        self.partial_sends = Some((every, sent));
        self
    }

    /// The network writes `bytes` at offset `at` into the Setup Requests of
    /// the Client at `client` (0 is the first).
    pub fn network_writes_setup(mut self, client: usize, at: usize, bytes: &[u8]) -> Self {
        self.setup_writes.push((client, at, bytes.to_vec()));
        self
    }

    /// The network captures the first PDU with `pdu_id` from the Client at
    /// `client` and delivers a copy again at `at`, to the same address.
    pub fn network_replays(mut self, pdu_id: [u8; 2], client: usize, at: Duration) -> Self {
        self.replay = Some(Replay {
            pdu_id,
            client,
            at,
            to_sibling: false,
        });
        self
    }

    /// As [`Self::network_replays`], but to the Server's newest Test
    /// Connection other than the one it was sent to.
    pub fn network_replays_to_sibling(
        mut self,
        pdu_id: [u8; 2],
        client: usize,
        at: Duration,
    ) -> Self {
        self.replay = Some(Replay {
            pdu_id,
            client,
            at,
            to_sibling: true,
        });
        self
    }

    /// The network delivers every Setup Request twice.
    pub fn network_duplicates_setup_requests(mut self) -> Self {
        self.duplicate_setup_requests = true;
        self
    }

    /// The Server's host fails to open its first `count` sockets.
    pub fn server_socket_opens_fail(mut self, count: usize) -> Self {
        self.server_socket_open_failures = count;
        self
    }

    /// The network flips byte `at` of every PDU whose pduId is `pdu_id`.
    pub fn network_flips_byte(mut self, pdu_id: [u8; 2], at: usize) -> Self {
        self.flip_byte = Some((pdu_id, at, None));
        self
    }

    /// As [`Self::network_flips_byte`], only from `from` on.
    pub fn network_flips_byte_from(mut self, pdu_id: [u8; 2], at: usize, from: Duration) -> Self {
        self.flip_byte = Some((pdu_id, at, None));
        self.flip_byte_from = from;
        self
    }

    /// As [`Self::network_flips_byte`], only on the way to `ip`.
    pub fn network_flips_byte_to(
        mut self,
        ip: std::net::IpAddr,
        pdu_id: [u8; 2],
        at: usize,
    ) -> Self {
        self.flip_byte = Some((pdu_id, at, Some(ip)));
        self
    }

    /// The Clients are configured with this Server address.
    pub fn client_server_address(mut self, addr: SocketAddr) -> Self {
        self.client_server = Some(addr);
        self
    }

    /// The Server runs this many Tests at once from one Source.
    pub fn server_tests_per_source(mut self, tests: usize) -> Self {
        self.server_tests_per_source = tests;
        self
    }

    /// Changes the Server's other settings.
    pub fn server_config(mut self, change: fn(&mut ServerConfig)) -> Self {
        self.server_config = Some(change);
        self
    }

    /// The first Client runs a Test of `test_connections` Test
    /// Connections, `minimum` of which must succeed. They send from the
    /// Client's address and the ports after it, in mcIndex order.
    pub fn coordinated(self, test_connections: u8, minimum: u8) -> Self {
        self.test_plan(move |plan| {
            plan.test_connections = test_connections.try_into().expect("at least one");
            plan.minimum = minimum.try_into().expect("at least one");
        })
    }

    /// The Clients' time limit for the Setup and Activation exchanges.
    pub fn client_time_limit(mut self, limit: Duration) -> Self {
        self.client_time_limit = limit;
        self
    }

    /// Adds a Server at the next address (192.0.2.2, then .3, ...), with
    /// the first's settings changed by `change`; a coordinated Client sends
    /// Test Connections to each in turn.
    pub fn add_server(mut self, change: fn(&mut ServerConfig)) -> Self {
        self.extra_servers.push(change);
        self
    }

    /// From `at` on, the network drops everything to or from `host`'s
    /// address, as a path that died.
    pub fn network_cuts_off(mut self, host: SocketAddr, at: Duration) -> Self {
        self.blackholes.push((host.ip(), at));
        self
    }

    /// Every Client states this maxBandwidth, in Mbps.
    pub fn client_max_bandwidth(mut self, mbps: u16) -> Self {
        self.client_max_bandwidth = mbps;
        self
    }

    /// The Server classifies Trial Intervals by this statistic of one-way
    /// delay variation.
    pub fn server_delay_var(mut self, statistic: DelayVarStatistic) -> Self {
        self.server_delay_var = statistic;
        self
    }

    /// The Server's wall clock jumps `secs` ahead (negative: back) at `at`.
    pub fn server_wall_step(mut self, at: Duration, secs: i64) -> Self {
        self.server_wall_steps.push((at, secs));
        self
    }

    /// The Server's Data Phase Engine runs nothing from `at` for `length`,
    /// as a descheduled Engine Thread would.
    pub fn server_engine_stall(mut self, at: Duration, length: Duration) -> Self {
        self.server_engine_stall = Some((at, length));
        self
    }

    /// The Clients' runtimes stop their Data Phases at `at`, as on a
    /// user's interrupt (`DataPhase::stop`).
    pub fn client_stops_data_phase_at(mut self, at: Duration) -> Self {
        self.client_stops_at = Some(at);
        self
    }

    /// The Client user interrupts the Test at `at` (Ctrl-C): the runtime
    /// tells the Client's endpoint (`Endpoint::handle_interrupt`).
    pub fn client_interrupted_at(mut self, at: Duration) -> Self {
        self.client_interrupted_at = Some(at);
        self
    }

    /// The Server administrator stops the first Server at `at` (a signal
    /// to stop): its runtime tells the core (`Server::handle_shutdown`).
    pub fn server_shuts_down_at(mut self, at: Duration) -> Self {
        self.server_shuts_down_at = Some(at);
        self
    }

    /// The Clients' kernel receive timestamps of Load PDUs and Status PDUs
    /// lie `ns` from the read that returns them (negative: the datagram waited in the
    /// socket buffer; positive: a clock error).
    pub fn client_kernel_timestamp_shift(mut self, ns: i64) -> Self {
        self.client_kernel_shift_ns = ns;
        self
    }

    /// The Clients' wall clocks step 2 ms during every read of Load PDUs,
    /// so the clock pair around it disagrees.
    pub fn client_wall_steps_during_reads(mut self) -> Self {
        self.client_steps_during_reads = true;
        self
    }

    /// The Clients' Data Phase Engines run nothing from `at` for `length`.
    pub fn client_engine_stall(mut self, at: Duration, length: Duration) -> Self {
        self.client_engine_stall = Some((at, length));
        self
    }

    /// Adds a Client, from the same address, that runs a Test of its own.
    pub fn add_client_in_another_test(self, direction: Direction) -> Self {
        self.add_client_in_another_test_at(direction, Duration::ZERO)
    }

    /// As [`Self::add_client_in_another_test`], starting at `at`.
    pub fn add_client_in_another_test_at(mut self, direction: Direction, at: Duration) -> Self {
        self.clients.push(ClientSpec {
            own_test: true,
            start_at: at,
            ..ClientSpec::in_same_test(direction)
        });
        self
    }

    /// Adds a Client as `spec` describes: one that runs a Test of its own.
    pub fn add_client_with(mut self, spec: ClientSpec) -> Self {
        self.clients.push(spec);
        self
    }

    /// Runs the scenario for up to `duration` of simulated time.
    pub fn run_for(mut self, duration: Duration) -> Outcome {
        let mut sim = sim::Sim::new(&mut self);
        // A panic inside the simulator or a core must also say how to replay it.
        let run = catch_unwind(AssertUnwindSafe(|| {
            sim.run_for(duration);
            if self.runtime_closes_test_connections {
                sim.close_server_test_connections();
            }
        }));
        if let Err(panic) = run {
            eprintln!(
                "panic during the simulation\n  seed: {}\n  trace hash so far: {:016x}\n  replay: Scenario::new({}) with the same settings",
                self.seed,
                sim.trace_hash(),
                self.seed
            );
            resume_unwind(panic);
        }
        sim.into_outcome()
    }
}

/// A Key Table row valid at all times.
pub fn always_valid(key_id: u8, key: &[u8]) -> KeyEntry {
    always(key_id, key)
}

/// A Key Table row valid at all times.
fn always(key_id: u8, key: &[u8]) -> KeyEntry {
    key_row(
        &format!("key {key_id}"),
        key_id,
        key,
        (None, None),
        (None, None),
    )
}

/// A Key Table row with send and accept lifetimes given as (start, end) in
/// whole seconds since the Unix epoch, `None` for an open end.
pub fn key_row(
    name: &str,
    key_id: u8,
    key: &[u8],
    send: (Option<u64>, Option<u64>),
    accept: (Option<u64>, Option<u64>),
) -> KeyEntry {
    KeyEntry::new(
        name,
        key_id,
        key.to_vec(),
        Lifetime::new(send.0, send.1),
        Lifetime::new(accept.0, accept.1),
    )
}

/// Loads rows into a Key Table, as a key file loader would. Table-only
/// checks have no simulation, so tests may assert on the result directly.
pub fn load_keys(rows: impl IntoIterator<Item = KeyEntry>) -> Result<KeyTable, KeyTableError> {
    let mut table = KeyTable::default();
    for row in rows {
        table.insert(row)?;
    }
    Ok(table)
}

/// What happened during a scenario run.
pub struct Outcome {
    pub seed: u64,
    /// Events of the first Client.
    pub client_events: Vec<Event>,
    /// Events of every Client, in the order they were added.
    pub clients_events: Vec<Vec<Event>>,
    pub server_events: Vec<Event>,
    /// The Server's sockets it connected, and to whom, in order.
    pub server_connects: Vec<(SocketId, SocketAddr)>,
    /// The Server's Data Phase events, by Test Connection socket.
    pub server_data_events: Vec<(SocketId, DataEvent)>,
    /// When each of `server_data_events` happened, in simulation time.
    pub server_data_event_times: Vec<Duration>,
    /// For each of `server_events`, how many of `server_data_events` came
    /// before it: the order a Server's log would show them in.
    pub server_event_data_marks: Vec<usize>,
    /// The first Client's Data Phase events.
    pub client_data_events: Vec<(SocketId, DataEvent)>,
    /// The first Client's events of each Test Connection, by socket
    /// (mcIndex + 1), when it runs a Multi-Connection Coordinator.
    pub client_connection_events: Vec<(SocketId, Event)>,
    /// The events of each Server added with `add_server`.
    pub other_servers_events: Vec<Vec<Event>>,
    /// The DS field each Data Phase sends with, by node (the Server is 0,
    /// then the Clients in order), as the runtime would apply it.
    pub dscp_ecn: Vec<(usize, u8)>,
    pub trace: Vec<TraceEntry>,
    pub network: NetworkCounts,
}

/// What the simulated network carried.
#[derive(Debug, Clone, Copy)]
pub struct NetworkCounts {
    /// Datagrams sent onto the network.
    pub sent: u64,
    /// Datagrams still on their way when the run ended.
    pub in_transit: u64,
    /// Datagrams the bottleneck dropped.
    pub bottleneck_drops: u64,
}

impl Outcome {
    /// A hash of the full datagram trace; equal hashes mean identical runs.
    pub fn trace_hash(&self) -> u64 {
        sim::hash_trace(&self.trace)
    }

    /// Fails the test with the seed, trace hash and trace if `ok` is false.
    #[track_caller]
    pub fn check(&self, ok: bool, what: &str) {
        if !ok {
            panic!("{}", self.failure_report(what));
        }
    }

    /// The testPort the Client accepted, if the Setup exchange succeeded.
    pub fn client_accepted_test_port(&self) -> Option<u16> {
        self.client_events.iter().find_map(|e| match e {
            Event::SetupAccepted { test_port } => Some(*test_port),
            _ => None,
        })
    }

    /// The direction Client `index` prepared after an accepted Activation.
    pub fn client_activated_direction(&self, index: usize) -> Option<Direction> {
        self.clients_events[index].iter().find_map(|e| match e {
            Event::TestActivated { direction, .. } => Some(*direction),
            _ => None,
        })
    }

    /// Coercions the Server reported, in order.
    pub fn server_coercions(&self) -> Vec<Coercion> {
        self.server_events
            .iter()
            .filter_map(|e| match e {
                Event::ActivationParameterCoerced { coercion, .. } => Some(*coercion),
                _ => None,
            })
            .collect()
    }

    /// Coercions the first Client saw in the Activation Response, in order.
    pub fn client_coercions(&self) -> Vec<Coercion> {
        self.client_events
            .iter()
            .filter_map(|e| match e {
                Event::ParameterCoerced { coercion } => Some(*coercion),
                _ => None,
            })
            .collect()
    }

    /// The coercion the first Client refused, ending the Test.
    pub fn client_refused_coercion(&self) -> Option<Coercion> {
        self.client_events.iter().find_map(|e| match e {
            Event::ControlPhaseFailed(ControlPhaseFailure::UnacceptableCoercion(c)) => Some(*c),
            _ => None,
        })
    }

    /// The Server rejected an Activation Request for `reason`.
    pub fn server_rejected_activation_because(&self, reason: ActivationRejection) -> bool {
        self.server_events.iter().any(
            |e| matches!(e, Event::ActivationRequestRejected { reason: r, .. } if *r == reason),
        )
    }

    pub fn client_activation_rejected(&self, index: usize) -> bool {
        self.clients_events[index]
            .iter()
            .any(|e| matches!(e, Event::ActivationRejected))
    }

    /// Activation Requests sent to the given Server port.
    pub fn activation_requests_sent_to_port(&self, port: u16) -> usize {
        self.trace
            .iter()
            .filter(|t| t.destination.port() == port)
            .filter_map(|t| ActivationPdu::decode(&t.payload).ok())
            .filter(|pdu| pdu.cmd_response == activation::cmd_response::NONE)
            .count()
    }

    /// Activation Requests the Clients sent, decoded.
    pub fn activation_requests(&self) -> Vec<ActivationPdu> {
        let server_ip = SERVER_ADDR.parse::<SocketAddr>().unwrap().ip();
        self.trace
            .iter()
            .filter(|t| t.destination.ip() == server_ip)
            .filter_map(|t| ActivationPdu::decode(&t.payload).ok())
            .collect()
    }

    /// Activation Responses the Server sent, decoded.
    pub fn activation_responses(&self) -> Vec<ActivationPdu> {
        let server_ip = SERVER_ADDR.parse::<SocketAddr>().unwrap().ip();
        self.trace
            .iter()
            .filter(|t| t.source.ip() == server_ip)
            .filter_map(|t| ActivationPdu::decode(&t.payload).ok())
            .collect()
    }

    pub fn server_dropped_unauthenticated_setup(&self) -> bool {
        self.server_events.iter().any(|e| {
            matches!(
                e,
                Event::SetupRequestSilentlyRejected {
                    reason: SilentRejectionReason::UnauthenticatedNotAllowed,
                    ..
                }
            )
        })
    }

    /// The Server reported ignoring a Setup Request for `reason`.
    pub fn server_ignored_setup_because(&self, reason: SetupIgnoreReason) -> bool {
        self.server_events
            .iter()
            .any(|e| matches!(e, Event::SetupRequestIgnored { reason: r, .. } if *r == reason))
    }

    /// The Server reported refusing a Setup Request silently for `reason`.
    pub fn server_dropped_setup_because(&self, reason: SilentRejectionReason) -> bool {
        self.server_events.iter().any(
            |e| matches!(e, Event::SetupRequestSilentlyRejected { reason: r, .. } if *r == reason),
        )
    }

    /// The Server reported dropping an Activation Request for `reason`.
    pub fn server_dropped_activation_because(&self, reason: SilentRejectionReason) -> bool {
        self.server_events
            .iter()
            .any(|e| matches!(e, Event::ActivationRequestSilentlyRejected { reason: r, .. } if *r == reason))
    }

    /// The first Client reported ignoring an Activation Response for `reason`.
    pub fn client_dropped_activation_response_because(&self, reason: ResponseIgnoreReason) -> bool {
        self.client_events
            .contains(&Event::ActivationResponseIgnored { reason })
    }

    /// The first Client reported that the Control Phase failed for `failure`.
    pub fn client_failed_because(&self, failure: ControlPhaseFailure) -> bool {
        self.client_events
            .contains(&Event::ControlPhaseFailed(failure))
    }

    /// The first Client reported ignoring a Setup Response for `reason`.
    pub fn client_dropped_setup_response_because(&self, reason: ResponseIgnoreReason) -> bool {
        self.client_events
            .contains(&Event::SetupResponseIgnored { reason })
    }

    pub fn client_reported_no_authentication(&self) -> bool {
        self.client_events.contains(&Event::ControlPhaseFailed(
            ControlPhaseFailure::NoAuthenticationConfigured,
        ))
    }

    pub fn server_admitted_count(&self) -> usize {
        self.server_events
            .iter()
            .filter(|e| matches!(e, Event::TestConnectionAdmitted { .. }))
            .count()
    }

    pub fn server_released_count(&self) -> usize {
        self.server_events
            .iter()
            .filter(|e| matches!(e, Event::TestConnectionReleased { .. }))
            .count()
    }

    /// Load PDUs (pduId 0xBEEF) the Server sent and the first Client
    /// received, in order.
    pub fn load_pdus_to_client(&self) -> Vec<&TraceEntry> {
        let client: SocketAddr = CLIENT_ADDR.parse().unwrap();
        self.trace
            .iter()
            .filter(|t| {
                t.destination == client && t.delivered && t.payload.starts_with(&[0xBE, 0xEF])
            })
            .collect()
    }

    /// Datagrams sent from the given address.
    pub fn sent_by(&self, addr: &str) -> Vec<&TraceEntry> {
        let addr: SocketAddr = addr.parse().unwrap();
        self.trace.iter().filter(|t| t.source == addr).collect()
    }

    fn failure_report(&self, what: &str) -> String {
        let mut report = format!(
            "expectation failed: {what}\n  seed: {}\n  trace hash: {:016x}\n  replay: Scenario::new({}) with the same settings\n  trace:\n",
            self.seed,
            self.trace_hash(),
            self.seed
        );
        for entry in &self.trace {
            let _ = writeln!(report, "    {entry}");
        }
        let _ = writeln!(report, "  client events: {:?}", self.client_events);
        let _ = writeln!(report, "  server events: {:?}", self.server_events);
        report
    }
}
