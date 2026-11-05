//! The Client's Multi-Connection Coordinator (docs/spec.md "Control Phase
//! (Client)", stories 54-58 and 93; ): it runs a Test's Test
//! Connections, one [`Client`] each on a socket of its own, and starts them
//! in two phases.
//!
//! 1. Every Setup Request goes out at once. The Setup phase ends when every
//!    Setup has succeeded or failed, or at the Setup deadline once at least
//!    the Minimum Test Connections have succeeded: the rest are abandoned.
//!    Fewer than the minimum, and the Test aborts before any Activation
//!    Request is sent. (conformance: multi-connection-setup-deadline)
//! 2. The Activation Requests go out together. Each Test Connection's Data
//!    Phase starts as soon as its own Activation Response accepts it, since
//!    the Server starts sending then; the parameters of each are checked
//!    against the first's, and the Test aborts if they differ.
//!    (conformance: multi-connection-parameters-differ)
//!
//! A Minimum Test Connections below all is a recorded choice.
//! (conformance: minimum-test-connections)
//!
//! Once running, a Test Connection fails when its Data Phase stops hearing
//! its peer (the 1 s Load PDU or Status PDU timeout, ADR 0002), or ends
//! any way but complete (stories 59 and 60; ). The Test goes on while
//! at least the Minimum Test Connections haven't failed. Two or more
//! falling silent within three Trial Intervals of each other are a path
//! failure, and stop the Test even when enough remain: the RFC's Load
//! Rate Adjustment Algorithm ends a Test on a loss of connectivity (RFC
//! 9946 §4.2), and the Test Connections of one Server share a path.
//! (conformance: multi-connection-isolated-failures, multi-connection-path-failure)
//!
//! The Client user may interrupt the Test too (Ctrl-C), unless every Test
//! Connection has ended already. (conformance: client-interrupt)
//!
//! Any abort stops the healthy Test Connections, and only if one is
//! left: one in its Data Phase through [`Output::StopDataPhase`], one
//! whose Activation Response is still on its way when that response
//! arrives. One that failed has stopped sending already, and its Watchdog
//! ends it (ADR 0002).

use std::collections::VecDeque;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::num::{NonZeroU8, NonZeroU16};
use std::time::Duration;

use crate::client::{Aborted, Client, ClientAuth, ClientConfig, TestConnectionConfig};
use crate::event::{TestAbort, TestParameters};
use crate::server::CONTROL_WATCHDOG;
use crate::{
    ControlPhaseFailure, DataEvent, DatagramMeta, Endpoint, Event, MonoTime, Now, Output, SocketId,
    TransportError,
};

/// The mcIdent for a Test whose Client holds a UDP socket bound to `local`
/// for as long as the Test runs: the socket's port, which no other
/// socket on the host holds meanwhile, mixed with a hash of its address
/// (RFC 9946 §6.1 names a source port as one source of mcIdent). Clients
/// on one host, whose Tests a Server tells apart only by mcIdent, then
/// never draw the same one at once, so neither is refused with
/// `MultiConnectionIndexInUse` nor sends a Setup Request identical to
/// another's, which the replay cache would drop. Clients on several hosts
/// behind one NAT have different local addresses, so their mcIdents are as
/// good as random. The mapping is one-to-one over the non-zero ports, so it
/// never gives zero. (conformance: mc-ident-from-a-held-port)
pub fn mc_ident_for(local: SocketAddr) -> Option<NonZeroU16> {
    let port = NonZeroU16::new(local.port())?;
    let bits = match local.ip() {
        IpAddr::V4(ip) => u128::from(ip.to_bits()),
        IpAddr::V6(ip) => ip.to_bits(),
    };
    // SplitMix64's finalizer over both halves, folded to 16 bits.
    let mut z = (bits as u64) ^ ((bits >> 64) as u64).rotate_left(29);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    let key = (z ^ (z >> 16) ^ (z >> 32) ^ (z >> 48)) as u16;
    // The port equal to the key would give zero: it takes the value no
    // port gives, the key itself (port zero's).
    NonZeroU16::new(port.get() ^ key).or(NonZeroU16::new(key))
}

/// What a Test is to be: where its Test Connections go, and how many must
/// succeed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestPlan {
    /// The Servers' standard-port addresses, of one address family: Test
    /// Connection `i` goes to `servers[i % servers.len()]` (round-robin).
    pub servers: Vec<SocketAddr>,
    /// Alternate IP addresses authorized locally for each selected Server,
    /// keyed by its standard-port address. Empty by default. Only
    /// authenticated Test Connections accept these, from that Server's
    /// Standard Port and IPv6 scope. Other selected Servers are not
    /// implicitly authorized as alternates.
    pub alternate_server_addresses: Vec<(SocketAddr, IpAddr)>,
    /// How many Test Connections the Test has: mcCount.
    pub test_connections: NonZeroU8,
    /// The Minimum Test Connections, at most `test_connections`.
    pub minimum: NonZeroU8,
    /// How long the Setup and Activation exchanges may take, at most the
    /// Test initiation timer's 3 s (RFC 9946 §6.1).
    pub time_limit: Duration,
    /// The Test's maxBandwidth in Mbps, 0 for none. It is the whole Test's:
    /// each Test Connection states an equal share, rounded down, so together
    /// they state no more than this. It is at least 1 Mbps a Test
    /// Connection, or [`Coordinator::new`] refuses it, as the binary does.
    /// (conformance: max-bandwidth-split-across-test-connections)
    pub max_bandwidth: u16,
    /// Servers of `servers` that the Test Connections use authentication
    /// mode 1 with instead of mode 2, chosen by the Client user for a
    /// Server that doesn't support mode 2 (docs/spec.md story 36).
    pub auth_mode_1: Vec<SocketAddr>,
}

/// The maxBandwidth each of a Test's `test_connections` Test Connections
/// states: an equal share of the Test's `max_bandwidth`, rounded down.
/// (conformance: max-bandwidth-split-across-test-connections)
pub(crate) fn max_bandwidth_share(max_bandwidth: u16, test_connections: u8) -> u16 {
    max_bandwidth / u16::from(test_connections.max(1))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Progress {
    /// Its socket isn't open yet.
    Opening,
    SettingUp,
    SetupAccepted,
    Activating,
    Activated,
    /// Its Data Phase completed.
    Completed,
    Failed,
    Abandoned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Setup,
    Activation,
    /// Every Test Connection has been activated or has failed.
    Running,
    Aborted,
}

struct TestConnection {
    socket: SocketId,
    config: TestConnectionConfig,
    client: Option<Client>,
    progress: Progress,
    /// Its Data Phase runs.
    running: bool,
    /// When its Data Phase stopped hearing its peer, if it did.
    silent_since: Option<MonoTime>,
    /// When its Setup Request went out, and how long its answer took.
    setup_sent: Option<MonoTime>,
    setup_rtt: Option<Duration>,
    /// The path errors already reported for it.
    path_errors: Vec<TransportError>,
}

/// The Client's Multi-Connection Coordinator: the endpoint that runs a
/// Test. Each Test Connection uses a socket of its own, which it asks the
/// runtime to open: [`Coordinator::socket_of`] its mcIndex.
pub struct Coordinator {
    connections: Vec<TestConnection>,
    minimum: usize,
    phase: Phase,
    /// When the Setup phase goes on without Setups still unanswered; `None`
    /// once passed.
    setup_deadline: Option<MonoTime>,
    /// The first Test Connection activated's parameters.
    parameters: Option<TestParameters>,
    /// An abort a Test Connection's event called for.
    abort: Option<TestAbort>,
    outputs: VecDeque<Output>,
}

impl Coordinator {
    /// The socket of the Test Connection with mcIndex `mc_index`:
    /// `SocketId(mc_index + 1)`, never [`SocketId::PRIMARY`].
    pub fn socket_of(mc_index: u8) -> SocketId {
        SocketId(u32::from(mc_index) + 1)
    }

    /// The mcIndex of the Test Connection on `socket`, as
    /// [`Coordinator::socket_of`] numbers them; `None` for a socket no
    /// Test Connection has.
    pub fn mc_index_of(socket: SocketId) -> Option<u8> {
        socket
            .0
            .checked_sub(1)
            .and_then(|index| u8::try_from(index).ok())
    }

    /// Starts the Test `plan` describes, identified by `mc_ident`, with the
    /// Client's settings for it, `test`, which every Test Connection
    /// shares. It decides the rest for each: its Server, in turn, its
    /// mcIndex and mcCount, its share of the plan's maxBandwidth, its time
    /// limit, and mode 1 with a Server the plan names. It asks for every
    /// socket at once; each Setup Request goes out when its socket opens.
    ///
    /// # Panics
    ///
    /// If `plan` has no Server, a minimum above its Test Connections, or a
    /// maxBandwidth below 1 Mbps a Test Connection.
    pub fn new(plan: TestPlan, mc_ident: NonZeroU16, test: ClientConfig, now: Now) -> Self {
        assert!(!plan.servers.is_empty(), "a Test needs a Server");
        assert!(
            plan.minimum <= plan.test_connections,
            "the minimum is at most the Test Connections"
        );
        let count = plan.test_connections.get();
        // A share rounded down to 0 would state none, and 1 each more than
        // the Test's maxBandwidth.
        // (conformance: max-bandwidth-split-across-test-connections)
        assert!(
            plan.max_bandwidth == 0 || plan.max_bandwidth >= u16::from(count),
            "a maxBandwidth of at least 1 Mbps a Test Connection, or none"
        );
        let time_limit = plan.time_limit.min(CONTROL_WATCHDOG);
        let share = max_bandwidth_share(plan.max_bandwidth, count);
        let connections = (0..count)
            .map(|index| {
                let server = plan.servers[usize::from(index) % plan.servers.len()];
                let auth = match &test.auth {
                    Some(ClientAuth::Mode2 { keys, key_id })
                        if plan.auth_mode_1.contains(&server) =>
                    {
                        Some(ClientAuth::Mode1 {
                            keys: keys.clone(),
                            key_id: *key_id,
                        })
                    }
                    auth => auth.clone(),
                };
                (index, server, auth)
            })
            .map(|(index, server, auth)| TestConnection {
                socket: Self::socket_of(index),
                config: TestConnectionConfig {
                    test: ClientConfig {
                        auth,
                        ..test.clone()
                    },
                    mc_ident,
                    server,
                    alternate_server_addresses: plan
                        .alternate_server_addresses
                        .iter()
                        .filter(|(selected, _)| *selected == server)
                        .map(|(_, ip)| ip.to_canonical())
                        .collect(),
                    mc_index: index,
                    mc_count: count,
                    max_bandwidth: share,
                    time_limit,
                },
                client: None,
                progress: Progress::Opening,
                running: false,
                silent_since: None,
                setup_sent: None,
                setup_rtt: None,
                path_errors: Vec::new(),
            })
            .collect();
        // Setups still unanswered this close to the time limit are given
        // up, so the Activation exchange has time left: 1 s, or half the
        // limit if that is shorter.
        let setup_wait = time_limit - Duration::from_secs(1).min(time_limit / 2);
        let mut coordinator = Self {
            connections,
            minimum: usize::from(plan.minimum.get()),
            phase: Phase::Setup,
            setup_deadline: Some(MonoTime::from_duration(now.mono.as_duration() + setup_wait)),
            parameters: None,
            abort: None,
            outputs: VecDeque::new(),
        };
        for connection in &coordinator.connections {
            let local_ip = match connection.config.server.ip() {
                IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
            };
            coordinator.outputs.push_back(Output::OpenSocket {
                socket: connection.socket,
                local_ip,
                scope_id: 0,
            });
        }
        coordinator
    }

    fn index_of(&self, socket: SocketId) -> Option<usize> {
        self.connections.iter().position(|c| c.socket == socket)
    }

    fn count(&self, progress: &[Progress]) -> usize {
        self.connections
            .iter()
            .filter(|c| progress.contains(&c.progress))
            .count()
    }

    /// Passes Test Connection `index`'s outputs on, on its socket.
    fn pump(&mut self, index: usize, now: Now) {
        let socket = self.connections[index].socket;
        while let Some(output) = self.connections[index]
            .client
            .as_mut()
            .and_then(Endpoint::poll_output)
        {
            let output = match output {
                Output::Transmit(mut transmit) => {
                    transmit.socket = socket;
                    Output::Transmit(transmit)
                }
                Output::StartDataPhase { peer, .. } => Output::StartDataPhase { socket, peer },
                Output::Event(event) => {
                    self.observe(index, &event, now);
                    Output::TestConnectionEvent { socket, event }
                }
                // A Client asks for nothing else.
                other => other,
            };
            self.outputs.push_back(output);
        }
    }

    /// Follows Test Connection `index` through its events.
    fn observe(&mut self, index: usize, event: &Event, now: Now) {
        let mc_index = self.connections[index].config.mc_index;
        let progress = &mut self.connections[index].progress;
        match event {
            Event::SetupAccepted { .. } if *progress == Progress::SettingUp => {
                *progress = Progress::SetupAccepted;
                let connection = &mut self.connections[index];
                connection.setup_rtt = connection
                    .setup_sent
                    .map(|sent| now.mono.as_duration().saturating_sub(sent.as_duration()));
            }
            Event::ControlPhaseFailed(failure) => {
                *progress = Progress::Failed;
                // The Client user refused the change, or the Client can't
                // run it: the whole Test ends, as with one Test Connection.
                if matches!(
                    failure,
                    ControlPhaseFailure::UnacceptableCoercion(_)
                        | ControlPhaseFailure::UnrunnableParameter { .. }
                ) {
                    self.abort
                        .get_or_insert(TestAbort::TestConnectionEnded { mc_index });
                }
            }
            Event::ActivationRejected => *progress = Progress::Failed,
            Event::TestActivated { parameters, .. } => {
                *progress = Progress::Activated;
                self.connections[index].running = true;
                match self.parameters {
                    None => self.parameters = Some(*parameters),
                    Some(first) if !first.same_test(parameters) => {
                        self.abort
                            .get_or_insert(TestAbort::ParametersDiffer { mc_index });
                    }
                    Some(_) => {}
                }
            }
            _ => {}
        }
    }

    /// Moves the Test on as far as its Test Connections allow.
    fn evaluate(&mut self, now: Now) {
        if self.phase == Phase::Aborted {
            return;
        }
        // Once the Data Phases run, a failure aborts the Test only to stop
        // the healthy ones: with none left, the Test Connections' own ends
        // tell how it went.
        let healthy = self.phase == Phase::Setup
            || self.connections.iter().any(|c| {
                matches!(
                    c.progress,
                    Progress::SetupAccepted | Progress::Activating | Progress::Activated
                )
            });
        match self.abort.take() {
            Some(TestAbort::TestConnectionsTimedOut) if !healthy => {}
            Some(reason) => return self.abort(reason),
            None => {}
        }
        let lost = self.count(&[Progress::Failed, Progress::Abandoned]);
        if self.connections.len() - lost < self.minimum && healthy {
            let minimum = u8::try_from(self.minimum).expect("at most 255");
            return self.abort(TestAbort::TooFewTestConnections { minimum });
        }
        if self.setup_deadline.is_some_and(|at| now.mono >= at) {
            self.setup_deadline = None;
        }
        if self.phase == Phase::Setup {
            let pending = self.count(&[Progress::Opening, Progress::SettingUp]);
            let accepted = self.count(&[Progress::SetupAccepted]);
            // Past the deadline the Test goes on without the unanswered
            // Setups once enough have succeeded; until then, and if too few
            // have, it waits for them, up to their own time limit.
            let go_on = pending == 0 || (self.setup_deadline.is_none() && accepted >= self.minimum);
            if !go_on {
                return;
            }
            let grid = self.grid(now);
            for index in 0..self.connections.len() {
                match self.connections[index].progress {
                    Progress::Opening | Progress::SettingUp => self.abandon(index),
                    Progress::SetupAccepted => {
                        self.connections[index].progress = Progress::Activating;
                        if let Some(client) = &mut self.connections[index].client {
                            client.activate(now, grid);
                        }
                        self.pump(index, now);
                    }
                    _ => {}
                }
            }
            self.phase = Phase::Activation;
        }
        if self.phase == Phase::Activation && self.count(&[Progress::Activating]) == 0 {
            self.phase = Phase::Running;
        }
    }

    /// Test Connection `index`'s Data Phase has failed: silent since
    /// `silent_since`, or ended some other way.
    fn fail_data_phase(&mut self, index: usize, silent_since: Option<MonoTime>) {
        let connection = &mut self.connections[index];
        if connection.progress != Progress::Activated {
            return;
        }
        connection.progress = Progress::Failed;
        connection.silent_since = silent_since;
        let Some(onset) = silent_since else {
            return;
        };
        // Three Trial Intervals: a Load Sender hears its peer only once a
        // Trial Interval, so two Test Connections that lost their path at
        // once may notice up to one apart, or two when the last Status PDU
        // before the loss was lost too.
        let window = 3 * self
            .parameters
            .map_or(Duration::from_millis(50), |p| p.trial_interval);
        let together = self
            .connections
            .iter()
            .filter_map(|c| c.silent_since)
            .filter(|at| at.as_duration().abs_diff(onset.as_duration()) <= window)
            .count();
        if together >= 2 {
            self.abort.get_or_insert(TestAbort::TestConnectionsTimedOut);
        }
    }

    /// The common grid of a Multi-Connection Test's Load Receivers, whose
    /// Activation Requests go out at `now`: from when the first Load
    /// PDUs should arrive, the shortest Setup round trip on. Its last
    /// Sub-Interval then ends before the Servers' STOP, which follows the
    /// Test Interval by half a second. A single Test Connection keeps its
    /// own. (conformance: multi-connection-common-grid)
    fn grid(&self, now: Now) -> Option<MonoTime> {
        (self.connections.len() > 1).then(|| {
            let rtt = self
                .connections
                .iter()
                .filter_map(|c| c.setup_rtt)
                .min()
                .unwrap_or_default();
            MonoTime::from_duration(now.mono.as_duration() + rtt)
        })
    }

    /// Gives up Test Connection `index`, which hasn't started its Data
    /// Phase.
    fn abandon(&mut self, index: usize) {
        let connection = &mut self.connections[index];
        // A Test Connection waiting for its Activation Response keeps its
        // socket, to stop it; any other is done with its socket.
        let done = connection
            .client
            .as_mut()
            .is_some_and(|client| client.abort() == Aborted::Ended);
        connection.progress = Progress::Abandoned;
        let socket = connection.socket;
        self.outputs.push_back(Output::TestConnectionEvent {
            socket,
            event: Event::TestConnectionAbandoned,
        });
        if done {
            self.outputs.push_back(Output::CloseSocket(socket));
        }
    }

    fn abort(&mut self, reason: TestAbort) {
        self.phase = Phase::Aborted;
        self.outputs
            .push_back(Output::Event(Event::TestAborted(reason)));
        for index in 0..self.connections.len() {
            let connection = &mut self.connections[index];
            // One whose Data Phase failed has stopped sending already, and
            // its Watchdog ends it (ADR 0002).
            if matches!(
                connection.progress,
                Progress::Abandoned | Progress::Completed | Progress::Failed
            ) {
                continue;
            }
            match connection.client.as_mut().map(Client::abort) {
                Some(Aborted::StopDataPhase) => {
                    self.outputs
                        .push_back(Output::StopDataPhase(connection.socket));
                }
                // Still waiting for an Activation Response: it is stopped
                // when it comes.
                Some(Aborted::Aborting | Aborted::Ended) | None => self.abandon(index),
            }
        }
    }

    /// After anything happened to Test Connection `index`.
    fn after(&mut self, index: usize, now: Now) {
        self.pump(index, now);
        self.evaluate(now);
    }
}

impl Endpoint for Coordinator {
    fn handle_datagram(&mut self, now: Now, meta: DatagramMeta, payload: &[u8]) {
        let Some(index) = self.index_of(meta.socket) else {
            return;
        };
        let Some(client) = &mut self.connections[index].client else {
            return;
        };
        let meta = DatagramMeta {
            socket: SocketId::PRIMARY,
            ..meta
        };
        client.handle_datagram(now, meta, payload);
        self.after(index, now);
    }

    fn handle_socket_opened(&mut self, now: Now, socket: SocketId, _local: SocketAddr) {
        let Some(index) = self.index_of(socket) else {
            return;
        };
        let connection = &mut self.connections[index];
        if connection.progress != Progress::Opening {
            return self.outputs.push_back(Output::CloseSocket(socket));
        }
        connection.progress = Progress::SettingUp;
        connection.setup_sent = Some(now.mono);
        connection.client = Some(Client::held(connection.config.clone(), now));
        self.after(index, now);
    }

    fn handle_socket_closed(&mut self, now: Now, socket: SocketId) {
        let Some(index) = self.index_of(socket) else {
            return;
        };
        // A socket that couldn't be opened, or failed before the Data
        // Phase: the Test Connection is lost.
        let connection = &mut self.connections[index];
        if matches!(
            connection.progress,
            Progress::Opening
                | Progress::SettingUp
                | Progress::SetupAccepted
                | Progress::Activating
        ) {
            if let Some(client) = &mut connection.client {
                client.abort();
            }
            connection.progress = Progress::Failed;
            self.outputs.push_back(Output::TestConnectionEvent {
                socket,
                event: Event::ControlPhaseFailed(ControlPhaseFailure::SocketUnavailable),
            });
            self.evaluate(now);
        } else if connection.running {
            // Its Data Phase Engine failed, or its socket did.
            connection.running = false;
            self.fail_data_phase(index, None);
            self.evaluate(now);
        }
    }

    fn handle_transport_error(&mut self, _now: Now, socket: SocketId, error: TransportError) {
        let Some(index) = self.index_of(socket) else {
            return;
        };
        // Other ICMP errors are counted by nobody here and ignored.
        if error == TransportError::Icmp {
            return;
        }
        let connection = &mut self.connections[index];
        if connection.path_errors.contains(&error) {
            return;
        }
        connection.path_errors.push(error);
        self.outputs.push_back(Output::TestConnectionEvent {
            socket,
            event: Event::PathError(error),
        });
    }

    fn handle_data_event(&mut self, now: Now, socket: SocketId, event: &DataEvent) {
        let Some(index) = self.index_of(socket) else {
            return;
        };
        match event {
            DataEvent::TrafficStopped { silent_since } => {
                self.fail_data_phase(index, Some(*silent_since));
            }
            DataEvent::Ended(reason) => {
                self.connections[index].running = false;
                if reason.completes() {
                    if self.connections[index].progress == Progress::Activated {
                        self.connections[index].progress = Progress::Completed;
                    }
                } else {
                    self.fail_data_phase(index, None);
                }
            }
            _ => return,
        }
        self.evaluate(now);
    }

    fn handle_interrupt(&mut self, _now: Now) {
        let ongoing = self.connections.iter().any(|c| {
            !matches!(
                c.progress,
                Progress::Completed | Progress::Failed | Progress::Abandoned
            )
        });
        if self.phase != Phase::Aborted && ongoing {
            self.abort(TestAbort::Interrupted);
        }
    }

    fn handle_timeout(&mut self, now: Now) {
        for index in 0..self.connections.len() {
            if let Some(client) = &mut self.connections[index].client
                && client.poll_timeout().is_some_and(|at| at <= now.mono)
            {
                client.handle_timeout(now);
                self.pump(index, now);
            }
        }
        self.evaluate(now);
    }

    fn poll_timeout(&self) -> Option<MonoTime> {
        let setup = self.setup_deadline.filter(|_| self.phase == Phase::Setup);
        self.connections
            .iter()
            .filter_map(|c| c.client.as_ref()?.poll_timeout())
            .chain(setup)
            .min()
    }

    fn poll_output(&mut self) -> Option<Output> {
        self.outputs.pop_front()
    }

    fn take_data_phase(&mut self, socket: SocketId) -> Option<crate::DataPhase> {
        let index = self.index_of(socket)?;
        self.connections[index]
            .client
            .as_mut()?
            .take_data_phase(SocketId::PRIMARY)
    }
}
