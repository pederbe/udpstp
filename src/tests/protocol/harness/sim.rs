//! Deterministic, zero-delay, lossless network simulator, which can
//! reorder and duplicate Load PDUs on request.
//!
//! It plays the runtime's part: it opens sockets when a core asks, delivers
//! datagrams with their metadata, calls cores at their deadlines, and records
//! a datagram trace. All randomness comes from the seed. Its Data Phase
//! executor runs each Test Connection's `DataPhase` the way a Data Phase
//! Engine does: it sends every Send Plan at its scheduled time, whole.
//!
//! A Client runs a Multi-Connection Coordinator, as in production, unless
//! it is scripted: a Test the scenario spreads over several Clients is
//! played by scripted ones (see `ClientSpec`).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fmt;
use std::net::SocketAddr;
use std::num::NonZeroU8;
use std::time::Duration;

use super::scripted::{ScriptedClient, ScriptedServer};
use crate::protocol::{
    ClientConfig, Coordinator, DataEvent, DataPhase, DatagramMeta, DelayVariation, Endpoint, Event,
    IpVersion, MonoTime, Now, Output, RateAdjAlgorithm, RxBatch, RxDatagram, RxPdu, Server,
    ServerConfig, SocketId, TestPlan, WallTime,
};

/// pduId of the Status PDU: the executor hands these to `handle_pdu` and
/// every other datagram to the Load PDU path (RFC 9946 §8.2).
const STATUS_PDU_ID: [u8; 2] = [0xFE, 0xED];

/// Wall-clock time at the start of every simulation (2026-09-21T18:00:00Z).
const WALL_START: Duration = Duration::from_secs(super::WALL_START_UNIX_SECS);

/// One datagram delivered (or dropped) by the simulator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceEntry {
    pub at: Duration,
    pub source: SocketAddr,
    pub destination: SocketAddr,
    /// The datagram's bytes; for a Load PDU only its header, unless the
    /// scenario keeps Load PDU content.
    pub payload: Vec<u8>,
    /// The datagram's full length.
    pub len: usize,
    pub delivered: bool,
}

impl fmt::Display for TraceEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:>12?} {} -> {} {} bytes{}",
            self.at,
            self.source,
            self.destination,
            self.payload.len(),
            if self.delivered {
                ""
            } else {
                " (dropped: no socket)"
            }
        )
    }
}

/// FNV-1a over the trace; stable across platforms and Rust versions.
pub fn hash_trace(trace: &[TraceEntry]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for b in bytes {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    for t in trace {
        feed(&t.at.as_nanos().to_be_bytes());
        feed(t.source.to_string().as_bytes());
        feed(t.destination.to_string().as_bytes());
        feed(&(t.len as u64).to_be_bytes());
        feed(&t.payload);
        feed(&[u8::from(t.delivered)]);
    }
    h
}

/// SplitMix64: a small, well-known deterministic generator.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn in_range(&mut self, low: u64, high_inclusive: u64) -> u64 {
        low + self.next() % (high_inclusive - low + 1)
    }
}

/// `at` on a clock running `ppm` parts per million fast.
fn drifted(at: Duration, ppm: i64) -> Duration {
    let ns = at.as_nanos() as i128;
    Duration::from_nanos((ns + ns * i128::from(ppm) / 1_000_000).max(0) as u64)
}

/// A core as the simulator drives it: through its `Endpoint`, and a Server
/// also through its shutdown, which its runtime asks for outside that
/// interface.
trait Core: Endpoint {
    /// The Server administrator stops the Server; other cores have no such
    /// input.
    fn handle_shutdown(&mut self, _now: Now) {}
}

impl Core for Server {
    fn handle_shutdown(&mut self, now: Now) {
        Server::handle_shutdown(self, now);
    }
}

impl Core for Coordinator {}
impl Core for ScriptedClient {}
impl Core for ScriptedServer {}

struct Node {
    endpoint: Box<dyn Core>,
    ip: std::net::IpAddr,
    events: Vec<Event>,
    /// For each of `events`, how many of `data_events` came before it.
    event_data_marks: Vec<usize>,
    /// A Multi-Connection Coordinator's events of each Test Connection.
    connection_events: Vec<(SocketId, Event)>,
    /// A Client's first port: a Multi-Connection Coordinator's Test
    /// Connection `i` binds this port plus `i`, so its first sends from the
    /// Client's address; a scripted Client's own socket binds it.
    base_port: Option<u16>,
    /// This host's wall clock minus the true time, in seconds.
    wall_offset: i64,
    /// Steps of this host's wall clock: from `at` on, it is `secs` further ahead.
    wall_steps: Vec<(Duration, i64)>,
    /// How many parts per million this host's clocks run fast.
    drift_ppm: i64,
    /// From when, and how many parts per million, this host's wall clock
    /// alone runs fast.
    slew: Option<(Duration, i64)>,
    /// Test Connections in their Data Phase, by socket.
    data_phases: BTreeMap<SocketId, DataPhase>,
    /// The sockets it connected, and to whom, in order.
    connects: Vec<(SocketId, SocketAddr)>,
    data_events: Vec<(SocketId, DataEvent)>,
    /// When each of `data_events` was polled, in simulation time.
    data_event_times: Vec<Duration>,
    /// A window in which this host's Data Phase Engine runs nothing.
    engine_stall: Option<(Duration, Duration)>,
    /// When the runtime stops every Data Phase of this node.
    stop_at: Option<Duration>,
    /// When the Client user interrupts the Test.
    interrupt_at: Option<Duration>,
    /// When the Server administrator stops the Server.
    shutdown_at: Option<Duration>,
    /// Until when this node's outputs wait: a Client that starts later.
    held_until: Duration,
    /// How far each kernel receive timestamp lies from the read, in ns
    /// (negative: the datagram waited in the socket buffer).
    kernel_shift_ns: i64,
    /// Whether the wall clock steps during every read, so its clock pair
    /// disagrees.
    steps_during_reads: bool,
}

impl Node {
    fn new(endpoint: Box<dyn Core>, ip: std::net::IpAddr, wall_offset: i64) -> Self {
        Self {
            endpoint,
            ip,
            events: Vec::new(),
            event_data_marks: Vec::new(),
            connection_events: Vec::new(),
            base_port: None,
            wall_offset,
            wall_steps: Vec::new(),
            drift_ppm: 0,
            slew: None,
            data_phases: BTreeMap::new(),
            connects: Vec::new(),
            data_events: Vec::new(),
            data_event_times: Vec::new(),
            engine_stall: None,
            stop_at: None,
            interrupt_at: None,
            shutdown_at: None,
            held_until: Duration::ZERO,
            kernel_shift_ns: 0,
            steps_during_reads: false,
        }
    }

    /// The simulation time at which this host's clock first reads `local`.
    fn global(&self, local: Duration) -> Duration {
        if self.drift_ppm == 0 {
            return local;
        }
        let target = local.as_nanos() as i128;
        let mut at = target * 1_000_000 / (1_000_000 + i128::from(self.drift_ppm));
        let reads = |at: i128| {
            drifted(Duration::from_nanos(at.max(0) as u64), self.drift_ppm).as_nanos() as i128
        };
        while reads(at) < target {
            at += 1;
        }
        while at > 0 && reads(at - 1) >= target {
            at -= 1;
        }
        Duration::from_nanos(at.max(0) as u64)
    }

    /// When the engine is next free to run at or after `at`.
    fn engine_free_at(&self, at: Duration) -> Duration {
        match self.engine_stall {
            Some((from, length)) if at >= from && at < from + length => from + length,
            _ => at,
        }
    }

    /// Records an event the core emitted, after the Data Events recorded
    /// so far.
    fn record(&mut self, event: Event) {
        self.event_data_marks.push(self.data_events.len());
        self.events.push(event);
    }

    /// Collects the Data Phase's events, as the runtime passes them on
    /// to the endpoint too; whether it has ended, so its engine exits.
    fn drain_data_events(&mut self, socket: SocketId, now: Duration, local: Now) -> bool {
        let mut ended = false;
        if let Some(data_phase) = self.data_phases.get_mut(&socket) {
            while let Some(event) = data_phase.poll_event() {
                ended |= matches!(event, DataEvent::Ended(_));
                self.endpoint.handle_data_event(local, socket, &event);
                self.data_events.push((socket, event));
                self.data_event_times.push(now);
            }
        }
        ended
    }
}

#[derive(Clone)]
struct Datagram {
    source: SocketAddr,
    destination: SocketAddr,
    payload: Vec<u8>,
}

pub struct Sim {
    seed: u64,
    /// The DS field of each Data Phase started, by node.
    dscp_ecn: Vec<(usize, u8)>,
    rng: Rng,
    wall_start: Duration,
    now: Duration,
    nodes: Vec<Node>,
    /// Bound sockets: address -> (node index, socket handle).
    sockets: HashMap<SocketAddr, (usize, SocketId)>,
    /// Reverse lookup: (node index, socket handle) -> address.
    socket_addrs: HashMap<(usize, SocketId), SocketAddr>,
    /// Connected sockets and their peers: they take datagrams from the
    /// peer only.
    connected: HashMap<(usize, SocketId), SocketAddr>,
    in_flight: VecDeque<Datagram>,
    /// Datagrams on a path with delay, by arrival time.
    delayed: BTreeMap<(Duration, u64), Datagram>,
    delayed_count: u64,
    /// The path delay and jitter, if any.
    path_delay: Option<(Duration, Duration)>,
    /// The latest arrival on each path, so datagrams stay in order.
    path_last: HashMap<(SocketAddr, SocketAddr), Duration>,
    /// The bottleneck in each direction between two hosts: its rate in
    /// bit/s and buffer in bytes.
    bottleneck: Option<(u64, u64)>,
    /// When each direction's bottleneck has sent everything queued so far.
    /// Every Test Connection between the two hosts shares it.
    link_busy: HashMap<(std::net::IpAddr, std::net::IpAddr), Duration>,
    /// Datagrams put on the network so far.
    sent: u64,
    /// Datagrams the bottleneck dropped.
    bottleneck_drops: u64,
    trace: Vec<TraceEntry>,
    /// Keep Load PDU content in the trace, not just headers.
    keep_load_content: bool,
    /// Every how many Load PDUs one is held back, and behind how many.
    reorder_load_pdus: Option<(u64, u32)>,
    /// Every how many Load PDUs one is delivered twice.
    duplicate_load_pdus: Option<u64>,
    /// Load PDUs seen by the network so far.
    load_pdus: u64,
    /// Windows in which the network drops PDUs of one kind.
    drops: Vec<super::DropWindow>,
    /// The network sets every Load and Status PDU's testAction to
    /// TEST_ACT_TEST, as an on-path attacker might (RFC 9946 §11).
    clear_test_action: bool,
    /// From when, and to what, the network sets Status PDUs' testAction.
    status_test_action_from: Option<(Duration, u8)>,
    /// Held-back Load PDUs and how many Load PDUs must pass them first.
    held: Vec<(Datagram, u32)>,
    /// The source address every Setup PDU seems to come from.
    setup_source: Option<std::net::IpAddr>,
    /// The local address every Setup PDU seems to arrive at.
    setup_local: Option<std::net::IpAddr>,
    /// The source address every Activation PDU seems to come from.
    activation_source: Option<std::net::IpAddr>,
    server_alias: Option<std::net::IpAddr>,
    server_scope: Option<u32>,
    /// A NAT in front of the first Client maps it to this port for
    /// everything after its Setup Requests.
    nat_port: Option<u16>,
    /// Every how many Send Plans the host takes only this many datagrams.
    partial_sends: Option<(u64, u32)>,
    /// The network flips this byte of every PDU with this pduId.
    flip_byte: Option<([u8; 2], usize, Option<std::net::IpAddr>)>,
    flip_byte_from: Duration,
    /// Bytes written into Setup Requests: from which Client's address, at
    /// which offset.
    setup_writes: Vec<(SocketAddr, usize, Vec<u8>)>,
    duplicate_setup_requests: bool,
    /// The PDU to replay, the Client address it comes from, and the copy
    /// once captured.
    replay: Option<(super::Replay, SocketAddr)>,
    replay_copy: Option<Datagram>,
    socket_open_failures: usize,
    /// Send Plans executed so far.
    plans: u64,
    /// How many Client nodes follow the Server; further Servers follow them.
    client_count: usize,
    /// Hosts cut off, from when.
    blackholes: Vec<(std::net::IpAddr, Duration)>,
    bottleneck_per_receiver: bool,
    bottleneck_to: Option<SocketAddr>,
}

const SERVER: usize = 0;

impl Sim {
    pub fn new(scenario: &mut super::Scenario) -> Self {
        let seed = scenario.seed;
        let (server_addr, first_client) = if scenario.ipv6 {
            (super::SERVER_ADDR_V6, super::CLIENT_ADDR_V6)
        } else {
            (super::SERVER_ADDR, super::CLIENT_ADDR)
        };
        let server_addr: SocketAddr = server_addr.parse().unwrap();
        let first_client: SocketAddr = first_client.parse().unwrap();
        let mut rng = Rng(seed);
        let wall_start = scenario.wall_start.unwrap_or(WALL_START);
        let server: Box<dyn Core> = match scenario.scripted_server.take() {
            Some(script) => Box::new(ScriptedServer::new(script)),
            None => {
                let mut config = ServerConfig {
                    allow_unauthenticated: scenario.server_allows_unauthenticated,
                    keys: scenario.server_keys.clone(),
                    checksum: scenario.checksum,
                    delay_var: scenario.server_delay_var,
                    tests_per_source: scenario.server_tests_per_source,
                    // Tests of other features start where they ask; the
                    // cap's own tests set it.
                    max_start_row: 0xFFFE,
                    ..ServerConfig::default()
                };
                if let Some(change) = scenario.server_config {
                    change(&mut config);
                }
                Box::new(Server::new(config))
            }
        };
        let mc_ident = std::num::NonZeroU16::new(rng.in_range(1, 0xFFFF) as u16).unwrap();
        // Each Client's Test, by the mcIdent it states: the first Client's,
        // or, for a Client in a Test of its own, another.
        let idents: Vec<std::num::NonZeroU16> = scenario
            .clients
            .iter()
            .enumerate()
            .map(|(index, spec)| {
                if spec.own_test {
                    let own = mc_ident
                        .get()
                        .wrapping_add(7919u16.wrapping_mul(index as u16));
                    std::num::NonZeroU16::new(own).unwrap_or(mc_ident)
                } else {
                    mc_ident
                }
            })
            .collect();
        let servers: Vec<SocketAddr> =
            std::iter::once(scenario.client_server.unwrap_or(server_addr))
                .chain(
                    (0..scenario.extra_servers.len())
                        .map(|k| super::extra_server_addr(k, scenario.ipv6)),
                )
                .collect();
        let mut server_node = Node::new(server, server_addr.ip(), 0);
        server_node.wall_steps = scenario.server_wall_steps.clone();
        server_node.engine_stall = scenario.server_engine_stall;
        server_node.shutdown_at = scenario.server_shuts_down_at;
        let mut nodes = vec![server_node];
        // Each Client's first address, and whether its own socket is bound
        // there before it starts: a scripted Client's; a Multi-Connection
        // Coordinator asks for its Test Connections' sockets.
        let mut client_addrs = Vec::new();
        let mut own_sockets = Vec::new();
        let mut next_port = first_client.port();
        for (index, spec) in scenario.clients.iter().enumerate() {
            let wall_offset = scenario.client_wall_offset;
            // A Client that starts later is created then.
            let now = Self::clock(spec.start_at, wall_start, wall_offset);
            // How many ports from `next_port` on the Client takes.
            let mut ports = 1;
            let mut own_socket = true;
            let client: Box<dyn Core> = match scenario.scripted_clients.remove(&index) {
                Some((script, replies)) => {
                    Box::new(ScriptedClient::new(server_addr, script, replies, now))
                }
                None => {
                    // A Multi-Connection Coordinator runs a whole Test from
                    // one host, numbering its Test Connections itself: a
                    // Test spread over several Clients is played by
                    // scripted ones.
                    assert!(
                        index == 0 || spec.own_test,
                        "Client {index} would join the first Client's Test: a Test spread over \
                         several Clients is played by scripted Clients \
                         (Scenario::add_scripted_client)"
                    );
                    let mut config = ClientConfig {
                        direction: spec.direction,
                        auth: scenario.client_auth.clone(),
                        row_request: scenario.client_row_request,
                        unacceptable_coercions: Vec::new(),
                        random_payload: false,
                        delay_variation: DelayVariation::OneWay,
                        checksum: false,
                        jumbo: true,
                        traditional_mtu: false,
                        test_int_time: 10,
                        algorithm: RateAdjAlgorithm::B,
                        dscp: 0,
                    };
                    for change in &scenario.client_config {
                        change(&mut config);
                    }
                    // This Client's own settings go last.
                    config.auth = spec.auth.clone().or(config.auth);
                    let mut plan = TestPlan {
                        servers: servers.clone(),
                        alternate_server_addresses: Vec::new(),
                        test_connections: NonZeroU8::MIN,
                        minimum: NonZeroU8::MIN,
                        time_limit: scenario.client_time_limit,
                        max_bandwidth: spec.max_bandwidth.unwrap_or(scenario.client_max_bandwidth),
                        auth_mode_1: Vec::new(),
                    };
                    // The first Client's Test is the scenario's to plan.
                    if index == 0 {
                        for change in &scenario.test_plan {
                            change(&mut plan);
                        }
                    }
                    ports = u16::from(plan.test_connections.get());
                    own_socket = false;
                    Box::new(Coordinator::new(plan, idents[index], config, now))
                }
            };
            let mut client_node = Node::new(client, first_client.ip(), wall_offset);
            client_node.engine_stall = scenario.client_engine_stall;
            client_node.held_until = spec.start_at;
            client_node.stop_at = scenario.client_stops_at;
            client_node.interrupt_at = scenario.client_interrupted_at;
            client_node.wall_steps = scenario.client_wall_steps.clone();
            client_node.drift_ppm = scenario.client_drift_ppm;
            client_node.slew = scenario.client_slew;
            client_node.kernel_shift_ns = scenario.client_kernel_shift_ns;
            client_node.steps_during_reads = scenario.client_steps_during_reads;
            client_node.base_port = Some(next_port);
            nodes.push(client_node);
            client_addrs.push(SocketAddr::new(first_client.ip(), next_port));
            own_sockets.push(own_socket);
            next_port += ports;
        }
        let client_count = nodes.len() - 1;
        let mut extra_server_addrs = Vec::new();
        for (k, change) in scenario.extra_servers.iter().enumerate() {
            let mut config = ServerConfig {
                allow_unauthenticated: scenario.server_allows_unauthenticated,
                keys: scenario.server_keys.clone(),
                checksum: scenario.checksum,
                delay_var: scenario.server_delay_var,
                tests_per_source: scenario.server_tests_per_source,
                max_start_row: 0xFFFE,
                ..ServerConfig::default()
            };
            change(&mut config);
            let addr = super::extra_server_addr(k, scenario.ipv6);
            nodes.push(Node::new(Box::new(Server::new(config)), addr.ip(), 0));
            extra_server_addrs.push(addr);
        }
        let mut sim = Self {
            client_count,
            blackholes: scenario.blackholes.clone(),
            bottleneck_per_receiver: scenario.bottleneck_per_receiver,
            bottleneck_to: scenario.bottleneck_to,
            dscp_ecn: Vec::new(),
            seed,
            rng,
            wall_start,
            now: Duration::ZERO,
            nodes,
            sockets: HashMap::new(),
            socket_addrs: HashMap::new(),
            connected: HashMap::new(),
            in_flight: VecDeque::new(),
            delayed: BTreeMap::new(),
            delayed_count: 0,
            path_delay: scenario.path_delay,
            path_last: HashMap::new(),
            bottleneck: scenario.bottleneck,
            link_busy: HashMap::new(),
            sent: 0,
            bottleneck_drops: 0,
            trace: Vec::new(),
            keep_load_content: scenario.keep_load_content,
            reorder_load_pdus: scenario.reorder_load_pdus,
            duplicate_load_pdus: scenario.duplicate_load_pdus,
            load_pdus: 0,
            drops: scenario.drops.clone(),
            clear_test_action: scenario.clear_test_action,
            status_test_action_from: scenario.status_test_action_from,
            held: Vec::new(),
            setup_source: scenario.setup_source,
            setup_local: scenario.setup_local,
            activation_source: scenario.activation_source,
            server_alias: scenario.server_alias,
            server_scope: scenario.server_scope,
            nat_port: scenario.nat_port,
            partial_sends: scenario.partial_sends,
            flip_byte: scenario.flip_byte,
            flip_byte_from: scenario.flip_byte_from,
            setup_writes: Vec::new(),
            duplicate_setup_requests: scenario.duplicate_setup_requests,
            replay: None,
            replay_copy: None,
            socket_open_failures: scenario.server_socket_open_failures,
            plans: 0,
        };
        sim.replay = scenario.replay.map(|r| (r, client_addrs[r.client]));
        sim.setup_writes = scenario
            .setup_writes
            .iter()
            .map(|(client, at, bytes)| (client_addrs[*client], *at, bytes.clone()))
            .collect();
        sim.bind(SERVER, SocketId::PRIMARY, server_addr);
        for (index, addr) in client_addrs.into_iter().enumerate() {
            if own_sockets[index] {
                sim.bind(1 + index, SocketId::PRIMARY, addr);
            }
        }
        for (k, addr) in extra_server_addrs.into_iter().enumerate() {
            sim.bind(1 + client_count + k, SocketId::PRIMARY, addr);
        }
        sim
    }

    fn clock(elapsed: Duration, wall_start: Duration, wall_offset: i64) -> Now {
        Self::clock_stepped(elapsed, wall_start, wall_offset, &[])
    }

    fn clock_stepped(
        elapsed: Duration,
        wall_start: Duration,
        wall_offset: i64,
        steps: &[(Duration, i64)],
    ) -> Now {
        Self::clock_drifted(elapsed, wall_start, wall_offset, steps, 0, None)
    }

    /// A host's clocks at simulation time `elapsed`: its monotonic clock
    /// runs `drift` ppm fast, its wall clock also `slew`'s ppm fast from its
    /// start, plus its offset and steps. Steps fall at simulation times;
    /// a slew starts at a local (monotonic) time.
    fn clock_drifted(
        elapsed: Duration,
        wall_start: Duration,
        wall_offset: i64,
        steps: &[(Duration, i64)],
        drift: i64,
        slew: Option<(Duration, i64)>,
    ) -> Now {
        let stepped: i64 = steps
            .iter()
            .filter(|(at, _)| elapsed >= *at)
            .map(|(_, secs)| secs)
            .sum();
        let local = drifted(elapsed, drift);
        let slewed = slew.map_or(0, |(from, ppm)| {
            local.saturating_sub(from).as_nanos() as i128 * i128::from(ppm) / 1_000_000
        });
        let wall = (wall_start + local).as_nanos() as i128
            + slewed
            + i128::from(wall_offset + stepped) * 1_000_000_000;
        Now {
            mono: MonoTime::from_duration(local),
            wall: WallTime::from_unix(Duration::from_nanos(wall as u64)),
        }
    }

    /// The current time as `node` sees it.
    fn now_for(&self, node: usize) -> Now {
        let n = &self.nodes[node];
        Self::clock_drifted(
            self.now,
            self.wall_start,
            n.wall_offset,
            &n.wall_steps,
            n.drift_ppm,
            n.slew,
        )
    }

    fn bind(&mut self, node: usize, socket: SocketId, addr: SocketAddr) {
        self.sockets.insert(addr, (node, socket));
        self.socket_addrs.insert((node, socket), addr);
    }

    pub fn run_for(&mut self, duration: Duration) {
        loop {
            self.settle();
            let next = self
                .nodes
                .iter()
                .flat_map(|n| {
                    let engine = n
                        .data_phases
                        .values()
                        .flat_map(|d| [d.next_burst_at(), d.poll_timeout()])
                        .flatten()
                        .map(|t| n.engine_free_at(n.global(t.as_duration())));
                    let stop = n
                        .stop_at
                        .filter(|_| !n.data_phases.is_empty())
                        .map(|at| n.engine_free_at(at));
                    let held = (n.held_until > self.now).then_some(n.held_until);
                    n.endpoint
                        .poll_timeout()
                        .map(|t| n.global(t.as_duration()))
                        .into_iter()
                        .chain(engine)
                        .chain(stop)
                        .chain(held)
                        .chain(n.interrupt_at)
                        .chain(n.shutdown_at)
                })
                .chain(self.delayed.keys().next().map(|&(at, _)| at))
                .chain(
                    self.replay
                        .filter(|(r, _)| self.replay_copy.is_some() && r.at > self.now)
                        .map(|(r, _)| r.at),
                )
                .min();
            match next {
                Some(at) if at <= duration => {
                    self.now = self.now.max(at);
                    self.deliver_due();
                    for i in 0..self.nodes.len() {
                        let now = self.now_for(i);
                        let node = &mut self.nodes[i];
                        if node.interrupt_at.is_some_and(|at| self.now >= at) {
                            node.interrupt_at = None;
                            node.endpoint.handle_interrupt(now);
                        }
                        if node.shutdown_at.is_some_and(|at| self.now >= at) {
                            node.shutdown_at = None;
                            node.endpoint.handle_shutdown(now);
                        }
                        if node.endpoint.poll_timeout().is_some_and(|t| t <= now.mono) {
                            node.endpoint.handle_timeout(now);
                        }
                        self.run_engine(i);
                        // Deliver what this node sent before the next node's
                        // turn, so the trace shows the order things happened:
                        // Load PDUs the Server sends at t reach the Client
                        // before the Client's turn at t, and the Status PDU
                        // tests count them in that order.
                        self.settle();
                    }
                }
                _ => break,
            }
        }
        // Held-back Load PDUs arrive late rather than never.
        for (late, _) in std::mem::take(&mut self.held) {
            self.deliver_now(late);
        }
        self.settle();
    }

    /// The Data Phase Engine's turn on `node`: every due Send Plan is
    /// written and sent whole, then due timeouts are handled.
    fn run_engine(&mut self, node: usize) {
        if self.nodes[node].engine_free_at(self.now) != self.now {
            return;
        }
        let now = self.now_for(node);
        if self.nodes[node].stop_at.is_some_and(|at| self.now >= at) {
            self.nodes[node].stop_at = None;
            for data_phase in self.nodes[node].data_phases.values_mut() {
                data_phase.stop(now);
            }
        }
        let sockets: Vec<SocketId> = self.nodes[node].data_phases.keys().copied().collect();
        for socket in sockets {
            let source = self.socket_addrs[&(node, socket)];
            let data_phase = self.nodes[node]
                .data_phases
                .get_mut(&socket)
                .expect("listed above");
            let peer = data_phase.peer();
            while let Some(plan) = data_phase.poll_burst(now, Duration::ZERO) {
                let mut buf = vec![0xA5; plan.bytes()];
                data_phase.write_datagrams(&plan, now, &mut buf);
                // The host takes only part of some Send Plans.
                self.plans += 1;
                let sent = match self.partial_sends {
                    Some((every, sent)) if self.plans.is_multiple_of(every) => {
                        sent.min(plan.datagrams())
                    }
                    _ => plan.datagrams(),
                };
                data_phase.report_sent(&plan, sent, now);
                for (at, len) in plan.layout().take(sent as usize) {
                    self.in_flight.push_back(Datagram {
                        source,
                        destination: peer,
                        payload: buf[at..at + len].to_vec(),
                    });
                }
                // A full send buffer ends the turn, as in the engine.
                if sent < plan.datagrams() {
                    break;
                }
            }
            if data_phase.poll_timeout().is_some_and(|t| t <= now.mono) {
                data_phase.handle_timeout(now);
            }
            let mut buf = [0u8; 2048];
            while let Some(len) = data_phase.poll_transmit(now, &mut buf) {
                self.in_flight.push_back(Datagram {
                    source,
                    destination: peer,
                    payload: buf[..len].to_vec(),
                });
            }
            self.pass_on_data_events(node, socket);
        }
    }

    /// Passes the Data Phase's events on to its endpoint, as the runtime
    /// does. Once it has ended, its engine exits, and the runtime closes the
    /// socket and reports it, after the engine's last Data Event
    /// (`Endpoint::handle_socket_closed`).
    fn pass_on_data_events(&mut self, node: usize, socket: SocketId) {
        let local = self.now_for(node);
        if self.nodes[node].drain_data_events(socket, self.now, local) {
            self.close_socket(node, socket);
        }
    }

    /// Whether `node` is a Server: the first, or one added after the
    /// Clients.
    fn is_server(&self, node: usize) -> bool {
        node == SERVER || node > self.client_count
    }

    /// Closes `node`'s `socket`, ending any Data Phase on it, and reports it
    /// closed, as the runtime does.
    fn close_socket(&mut self, node: usize, socket: SocketId) {
        let now = self.now_for(node);
        self.connected.remove(&(node, socket));
        if let Some(addr) = self.socket_addrs.remove(&(node, socket)) {
            self.sockets.remove(&addr);
        }
        self.nodes[node].data_phases.remove(&socket);
        self.nodes[node].endpoint.handle_socket_closed(now, socket);
    }

    /// Processes outputs and deliveries until nothing is left to do at
    /// `now`. Each node's outputs are applied after every delivery, so a
    /// Data Phase starts before the next datagram arrives.
    fn settle(&mut self) {
        loop {
            for i in 0..self.nodes.len() {
                if self.now < self.nodes[i].held_until {
                    continue;
                }
                while let Some(output) = self.nodes[i].endpoint.poll_output() {
                    self.apply(i, output);
                }
            }
            let Some(datagram) = self.in_flight.pop_front() else {
                break;
            };
            self.sent += 1;
            if self.path_delay.is_some() || self.bottleneck.is_some() {
                self.transit(datagram);
            } else {
                self.deliver(datagram);
            }
        }
    }

    /// Puts `datagram` on its path. It waits its turn at the bottleneck
    /// between its two hosts, where a full buffer drops it, and is
    /// serialized there with its IP and UDP headers. It then arrives after
    /// the delay plus jitter, and not before the one sent ahead of it on
    /// its path.
    fn transit(&mut self, datagram: Datagram) {
        let path = (datagram.source, datagram.destination);
        let from = if self.bottleneck_per_receiver {
            std::net::IpAddr::from([0, 0, 0, 0])
        } else {
            datagram.source.ip()
        };
        let link = (from, datagram.destination.ip());
        let mut departs = self.now;
        if let Some((rate, buffer)) = self.bottleneck
            && self
                .bottleneck_to
                .is_none_or(|to| to == datagram.destination)
        {
            let headers = if datagram.destination.is_ipv4() {
                28
            } else {
                48
            };
            let bytes = (datagram.payload.len() + headers) as u128;
            let busy = self
                .link_busy
                .get(&link)
                .copied()
                .unwrap_or_default()
                .max(self.now);
            // Bytes still queued, including what is left of the datagram
            // being sent.
            let queued = ((busy - self.now).as_nanos() * u128::from(rate)).div_ceil(8_000_000_000);
            if queued + bytes > u128::from(buffer) {
                self.bottleneck_drops += 1;
                self.trace.push(TraceEntry {
                    at: self.now,
                    source: datagram.source,
                    destination: datagram.destination,
                    payload: datagram.payload[..datagram.payload.len().min(32)].to_vec(),
                    len: datagram.payload.len(),
                    delivered: false,
                });
                return;
            }
            let serialization = bytes * 8_000_000_000 / u128::from(rate);
            departs = busy + Duration::from_nanos(serialization as u64);
            self.link_busy.insert(link, departs);
        }
        let (one_way, jitter) = self.path_delay.unwrap_or_default();
        let jitter = if jitter.is_zero() {
            Duration::ZERO
        } else {
            Duration::from_nanos(self.rng.in_range(0, jitter.as_nanos() as u64))
        };
        let at = (departs + one_way + jitter)
            .max(self.path_last.get(&path).copied().unwrap_or_default());
        self.path_last.insert(path, at);
        self.delayed_count += 1;
        self.delayed.insert((at, self.delayed_count), datagram);
    }

    /// Delivers every delayed datagram due by now.
    fn deliver_due(&mut self) {
        if let Some((replay, _)) = self.replay
            && replay.at <= self.now
            && let Some(mut copy) = self.replay_copy.take()
        {
            self.replay = None;
            if replay.to_sibling {
                let sibling = self
                    .socket_addrs
                    .iter()
                    .filter(|((node, socket), addr)| {
                        *node == SERVER
                            && *socket != SocketId::PRIMARY
                            && **addr != copy.destination
                    })
                    .max_by_key(|((_, socket), _)| *socket)
                    .map(|(_, addr)| *addr);
                if let Some(sibling) = sibling {
                    copy.destination = sibling;
                }
            }
            self.deliver(copy);
            self.settle();
        }
        while let Some(entry) = self.delayed.first_entry() {
            if entry.key().0 > self.now {
                break;
            }
            let datagram = entry.remove();
            self.deliver(datagram);
            self.settle();
        }
    }

    fn apply(&mut self, node: usize, output: Output) {
        let now = self.now_for(node);
        match output {
            Output::Transmit(t) => {
                let source = self.socket_addrs[&(node, t.socket)];
                self.in_flight.push_back(Datagram {
                    source,
                    destination: t.destination,
                    payload: t.payload,
                });
            }
            // A socket that can't be opened is reported closed, as the
            // runtime does.
            Output::OpenSocket { socket, .. }
                if node == SERVER && self.socket_open_failures > 0 =>
            {
                self.socket_open_failures -= 1;
                self.nodes[node].endpoint.handle_socket_closed(now, socket);
            }
            // A Multi-Connection Coordinator asks for a wildcard address:
            // its Test Connections' sockets take the ports from its first
            // on, in mcIndex order.
            Output::OpenSocket {
                socket, local_ip, ..
            } if local_ip.is_unspecified() => {
                let ip = self.nodes[node].ip;
                let base = self.nodes[node].base_port.expect("a Client node");
                let mc_index =
                    Coordinator::mc_index_of(socket).expect("a Test Connection's socket");
                let addr = SocketAddr::new(ip, base + u16::from(mc_index));
                assert!(
                    !self.sockets.contains_key(&addr),
                    "{addr} is taken: give Clients room"
                );
                self.bind(node, socket, addr);
                self.nodes[node]
                    .endpoint
                    .handle_socket_opened(now, socket, addr);
            }
            Output::OpenSocket {
                socket, local_ip, ..
            } => {
                assert_eq!(
                    local_ip, self.nodes[node].ip,
                    "a core asked to bind an address its host doesn't have"
                );
                let addr = loop {
                    let port = self.rng.in_range(32768, 60999) as u16;
                    let addr = SocketAddr::new(local_ip, port);
                    if !self.sockets.contains_key(&addr) {
                        break addr;
                    }
                };
                self.bind(node, socket, addr);
                self.nodes[node]
                    .endpoint
                    .handle_socket_opened(now, socket, addr);
            }
            Output::ConnectSocket { socket, peer } => {
                self.connected.insert((node, socket), peer);
                self.nodes[node].connects.push((socket, peer));
            }
            Output::CloseSocket(socket) => {
                // Only its engine's exit closes a Data Phase's socket
                // (`Endpoint::handle_socket_closed`).
                assert!(
                    !self.nodes[node].data_phases.contains_key(&socket),
                    "a core asked to close a socket its Data Phase runs on"
                );
                self.close_socket(node, socket);
            }
            Output::StartDataPhase { socket, peer } => {
                let data_phase = self.nodes[node]
                    .endpoint
                    .take_data_phase(socket)
                    .expect("a core announced a Data Phase it doesn't hand over");
                assert_eq!(
                    data_phase.peer(),
                    peer,
                    "the announced peer is the Data Phase's"
                );
                self.dscp_ecn.push((node, data_phase.dscp_ecn()));
                self.nodes[node].data_phases.insert(socket, data_phase);
            }
            Output::StopDataPhase(socket) => {
                if let Some(data_phase) = self.nodes[node].data_phases.get_mut(&socket) {
                    data_phase.stop(now);
                    // A silenced Data Phase ends here, with no send or
                    // timeout left to wake the engine and drain its end.
                    self.pass_on_data_events(node, socket);
                } else if self.is_server(node) {
                    // A Server's runtime closes a Test Connection whose
                    // Data Phase isn't running, and reports it. A Client's
                    // has nothing to do: its Multi-Connection Coordinator
                    // stops only Data Phases that run.
                    self.close_socket(node, socket);
                }
            }
            Output::Event(event) => self.nodes[node].record(event),
            Output::TestConnectionEvent { socket, event } => {
                self.nodes[node].record(event.clone());
                self.nodes[node].connection_events.push((socket, event));
            }
        }
    }

    /// Delivers `datagram`, reordering or duplicating Load PDUs as the
    /// scenario asks.
    fn deliver(&mut self, mut datagram: Datagram) {
        if let Some(scope) = self.server_scope
            && datagram.source.ip() == self.nodes[SERVER].ip
            && let SocketAddr::V6(source) = &mut datagram.source
        {
            source.set_scope_id(scope);
        }
        if let Some(port) = self.nat_port {
            let client: SocketAddr = super::CLIENT_ADDR.parse().unwrap();
            let mapped = SocketAddr::new(client.ip(), port);
            if datagram.source == client && !datagram.payload.starts_with(&[0xAC, 0xE1]) {
                datagram.source = mapped;
            }
            if datagram.destination == mapped {
                datagram.destination = client;
            }
        }
        if let Some(alias) = self.server_alias {
            let server = self.nodes[SERVER].ip;
            if datagram.source.ip() == server {
                datagram.source.set_ip(alias);
            }
        }
        let pdu_id = datagram.payload.first_chunk::<2>().copied();
        if let Some((replay, from)) = self.replay
            && self.replay_copy.is_none()
            && pdu_id == Some(replay.pdu_id)
            && datagram.source == from
        {
            self.replay_copy = Some(datagram.clone());
        }
        let now = self.now;
        let cut_off = self.blackholes.iter().any(|&(ip, at)| {
            now >= at && (datagram.source.ip() == ip || datagram.destination.ip() == ip)
        });
        if cut_off
            || self.drops.iter().any(|d| {
                pdu_id == Some(d.pdu_id) && now >= d.from && d.until.is_none_or(|until| now < until)
            })
        {
            self.trace.push(TraceEntry {
                at: now,
                source: datagram.source,
                destination: datagram.destination,
                payload: datagram.payload[..datagram.payload.len().min(32)].to_vec(),
                len: datagram.payload.len(),
                delivered: false,
            });
            return;
        }
        if self.clear_test_action
            && (pdu_id == Some([0xBE, 0xEF]) || pdu_id == Some(STATUS_PDU_ID))
            && datagram.payload.len() > 2
        {
            datagram.payload[2] = 0;
        }
        if pdu_id == Some([0xAC, 0xE1]) && datagram.payload.get(8) == Some(&1) {
            for (from, at, bytes) in &self.setup_writes {
                if *from == datagram.source {
                    datagram.payload[*at..*at + bytes.len()].copy_from_slice(bytes);
                }
            }
        }
        if let Some((id, at, toward)) = self.flip_byte
            && now >= self.flip_byte_from
            && pdu_id == Some(id)
            && toward.is_none_or(|ip| ip == datagram.destination.ip())
            && let Some(byte) = datagram.payload.get_mut(at)
        {
            *byte ^= 0xFF;
        }
        if let Some((at, value)) = self.status_test_action_from
            && self.now >= at
            && pdu_id == Some(STATUS_PDU_ID)
        {
            datagram.payload[2] = value;
        }
        if self.duplicate_setup_requests
            && pdu_id == Some([0xAC, 0xE1])
            && datagram.payload.get(8) == Some(&1)
        {
            self.deliver_now(datagram.clone());
        }
        if !datagram.payload.starts_with(&[0xBE, 0xEF]) {
            return self.deliver_now(datagram);
        }
        self.load_pdus += 1;
        let nth = |every: Option<u64>| every.is_some_and(|e| self.load_pdus.is_multiple_of(e));
        if nth(self.reorder_load_pdus.map(|(every, _)| every)) {
            let behind = self.reorder_load_pdus.map_or(0, |(_, behind)| behind);
            self.held.push((datagram, behind));
            return;
        }
        if nth(self.duplicate_load_pdus) {
            self.deliver_now(datagram.clone());
        }
        self.deliver_now(datagram);
        for (_, behind) in &mut self.held {
            *behind -= 1;
        }
        let (due, waiting) = std::mem::take(&mut self.held)
            .into_iter()
            .partition(|(_, behind)| *behind == 0);
        self.held = waiting;
        for (late, _) in due {
            self.deliver_now(late);
        }
    }

    fn deliver_now(&mut self, datagram: Datagram) {
        // Route an alternate address to the same host, keeping the wire
        // destination in the trace and received metadata.
        let mut routed = datagram.destination;
        if self.server_alias == Some(routed.ip()) {
            routed.set_ip(self.nodes[SERVER].ip);
        }
        // Scopes identify the simulated host's interface, not a different
        // host. Keep them in the wire trace and metadata.
        if let SocketAddr::V6(address) = &mut routed {
            address.set_scope_id(0);
        }
        let mut target = self.sockets.get(&routed).copied();
        // A Test Connection in its Data Phase takes datagrams from its peer
        // only, as a connected socket would.
        if let Some((node, socket)) = target
            && let Some(data_phase) = self.nodes[node].data_phases.get(&socket)
            && data_phase.peer() != datagram.source
        {
            target = None;
        }
        // A connected socket takes datagrams from its peer only.
        if let Some(key) = target
            && self
                .connected
                .get(&key)
                .is_some_and(|&peer| peer != datagram.source)
        {
            target = None;
        }
        let is_load = datagram.payload.starts_with(&[0xBE, 0xEF]);
        let kept = if is_load && !self.keep_load_content {
            datagram.payload.len().min(32)
        } else {
            datagram.payload.len()
        };
        self.trace.push(TraceEntry {
            at: self.now,
            source: datagram.source,
            destination: datagram.destination,
            payload: datagram.payload[..kept].to_vec(),
            len: datagram.payload.len(),
            delivered: target.is_some(),
        });
        let Some((node, socket)) = target else {
            return;
        };
        let now = self.now_for(node);
        let wall_ns = |wall: WallTime, by: i64| {
            let ns = wall.as_unix().as_nanos() as i128 + i128::from(by);
            WallTime::from_unix(Duration::from_nanos(ns as u64))
        };
        let shift_ns = self.nodes[node].kernel_shift_ns;
        if let Some(data_phase) = self.nodes[node].data_phases.get_mut(&socket) {
            if datagram.payload.starts_with(&STATUS_PDU_ID) {
                data_phase.handle_pdu(RxPdu {
                    now,
                    kernel_wall: Some(wall_ns(now.wall, shift_ns)),
                    pdu: &datagram.payload,
                });
            } else {
                let mut header = [0u8; 32];
                let len = datagram.payload.len().min(32);
                header[..len].copy_from_slice(&datagram.payload[..len]);
                let n = &self.nodes[node];
                let received = [RxDatagram {
                    header,
                    actual_len: datagram.payload.len(),
                    kernel_wall: Some(wall_ns(now.wall, n.kernel_shift_ns)),
                    tos: Some(0),
                }];
                let before = if n.steps_during_reads {
                    Now {
                        wall: wall_ns(now.wall, -2_000_000),
                        ..now
                    }
                } else {
                    now
                };
                let data_phase = self.nodes[node]
                    .data_phases
                    .get_mut(&socket)
                    .expect("looked up above");
                data_phase.handle_load_batch(RxBatch {
                    before,
                    after: now,
                    datagrams: &received,
                });
            }
            self.pass_on_data_events(node, socket);
            return;
        }
        // Setup PDUs (Figure 3) may be shown as if from, or sent to, other
        // addresses.
        let is_setup = datagram.payload.starts_with(&[0xAC, 0xE1]);
        let is_activation = datagram.payload.starts_with(&[0xAC, 0xE2]);
        let source = match (self.setup_source, self.activation_source) {
            (Some(ip), _) if is_setup => SocketAddr::new(ip, datagram.source.port()),
            (_, Some(ip)) if is_activation => SocketAddr::new(ip, datagram.source.port()),
            _ => datagram.source,
        };
        let local = match self.setup_local {
            Some(ip) if is_setup => SocketAddr::new(ip, datagram.destination.port()),
            _ => datagram.destination,
        };
        let meta = DatagramMeta {
            socket,
            source,
            local,
            ip_version: if datagram.destination.is_ipv4() {
                IpVersion::V4
            } else {
                IpVersion::V6
            },
            // As the runtimes: not read on Control Phase sockets.
            tos: None,
            actual_len: datagram.payload.len(),
        };
        let now = self.now_for(node);
        self.nodes[node]
            .endpoint
            .handle_datagram(now, meta, &datagram.payload);
    }

    /// Closes every Server socket except the standard-port one, as engines
    /// that exit would, reporting each closure to the Server, then lets the
    /// cores react.
    pub fn close_server_test_connections(&mut self) {
        let mut sockets: Vec<SocketId> = self
            .socket_addrs
            .keys()
            .filter(|(node, socket)| *node == SERVER && *socket != SocketId::PRIMARY)
            .map(|(_, socket)| *socket)
            .collect();
        sockets.sort();
        for socket in sockets {
            self.close_socket(SERVER, socket);
        }
        self.settle();
    }

    pub fn trace_hash(&self) -> u64 {
        hash_trace(&self.trace)
    }

    pub fn into_outcome(self) -> super::Outcome {
        let mut nodes = self.nodes.into_iter();
        let server = nodes.next().unwrap();
        let clients: Vec<Node> = nodes.by_ref().take(self.client_count).collect();
        let other_servers: Vec<Node> = nodes.collect();
        let clients_events: Vec<Vec<Event>> = clients.iter().map(|n| n.events.clone()).collect();
        super::Outcome {
            seed: self.seed,
            client_events: clients_events[0].clone(),
            clients_events,
            server_connects: server.connects,
            server_events: server.events,
            server_data_events: server.data_events,
            server_data_event_times: server.data_event_times,
            server_event_data_marks: server.event_data_marks,
            client_data_events: clients[0].data_events.clone(),
            client_connection_events: clients[0].connection_events.clone(),
            other_servers_events: other_servers.iter().map(|n| n.events.clone()).collect(),
            dscp_ecn: self.dscp_ecn,
            trace: self.trace,
            network: super::NetworkCounts {
                sent: self.sent,
                in_transit: self.delayed.len() as u64,
                bottleneck_drops: self.bottleneck_drops,
            },
        }
    }
}
