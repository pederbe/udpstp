//! The Client's Control Phase on Tokio.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::num::NonZeroU16;
use std::task::{Context, Poll};
use std::time::Duration;

use crate::protocol::{
    ClientConfig, ControlPhaseFailure, Coordinator, DataEvent, DatagramMeta, Direction, EndReason,
    Endpoint, Event, Output, SocketId, TestPlan, Transmit, TransportError,
};

use crate::runtime::arrival::{Arrival, Listener};
use crate::runtime::clock::{Clock, SplitMix64, seed};
use crate::runtime::engine::{self, Engine, EngineFacts, EngineSettings, FromEngine};
use crate::runtime::ip_options::IpOptions;
use crate::runtime::server::{bind_shared, ip_version};
use crate::runtime::socket::classify::{Action, Call, classify};
use crate::runtime::transport::MAX_DATAGRAM;

/// The UDPSTP Standard Port (RFC 9946 §12.1).
use crate::wire::STANDARD_PORT;

// ENOBUFS and EINTR need not clear write readiness. Yield between attempts so
// persistent trouble cannot spin or hold up the core's timers (ADR 0006).
const CONTROL_RETRY_DELAY: Duration = Duration::from_millis(1);

/// How the Client's Test ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientResult {
    /// The Data Phase ran and ended, for `reason` (`None` when the Data
    /// Phase Engine failed, with `error`).
    Finished {
        direction: Direction,
        reason: Option<EndReason>,
        error: Option<String>,
    },
    /// The Server rejected the Activation Request (CHTA_CRSP_BADPARAM).
    ActivationRejected,
    /// The Control Phase couldn't proceed.
    Failed(ControlPhaseFailure),
    /// The Multi-Connection Coordinator gave it up before its Data Phase.
    Abandoned,
    /// A local socket error ended it.
    SocketError(String),
}

/// What the Client did, in order, and how it ended.
#[derive(Debug, Clone)]
pub struct ClientOutcome {
    pub server: SocketAddr,
    pub events: Vec<Event>,
    pub data_events: Vec<DataEvent>,
    /// When each of `data_events` reached the runtime, since the Unix
    /// epoch: the time a result was processed, as OB-UDPST stamps it.
    pub data_event_times: Vec<Duration>,
    /// The Client's socket's local address.
    pub local: Option<SocketAddr>,
    /// The Test's mcIdent, retained for diagnostic output.
    #[allow(dead_code)]
    pub mc_ident: u16,
    pub result: ClientResult,
    /// How its Data Phase ran on this host, as its Data Phase Engine
    /// reported when it exited: the default if its Data Phase never
    /// started. In an Upstream Test the Server's receive buffer matters
    /// more than this host's, which its operator sees instead.
    pub engine_facts: EngineFacts,
}

/// The wall clock since the Unix epoch.
fn wall_now() -> Duration {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
}

/// The Server's address for `server`: `host`, `host:port`, `ip`, `ip:port`
/// or `[ipv6]:port`, the Standard Port when none is given. A host name
/// resolves to the first address the resolver returns (docs/spec.md story 4).
pub async fn resolve(server: &str) -> io::Result<SocketAddr> {
    // An IPv4-mapped address is the IPv4 address: our sockets are
    // single-stack.
    let canonical = |addr: SocketAddr| match addr {
        SocketAddr::V6(v6) if v6.ip().to_ipv4_mapped().is_some() => {
            SocketAddr::new(addr.ip().to_canonical(), addr.port())
        }
        addr => addr,
    };
    if let Ok(addr) = server.parse::<SocketAddr>() {
        return Ok(canonical(addr));
    }
    if let Ok(ip) = server.trim_matches(['[', ']']).parse::<IpAddr>() {
        return Ok(canonical(SocketAddr::new(ip, STANDARD_PORT)));
    }
    let (host, port) = match server.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') => {
            let port = port.parse().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("invalid port in {server}"),
                )
            })?;
            (host, port)
        }
        _ => (server, STANDARD_PORT),
    };
    tokio::net::lookup_host((host, port))
        .await?
        .next()
        .map(canonical)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("{host} has no address")))
}

/// What a Test did: its own events, such as an abort, and each Test
/// Connection's outcome, by mcIndex.
#[derive(Debug, Clone)]
pub struct TestOutcome {
    /// The Test's mcIdent, retained for diagnostic output.
    #[allow(dead_code)]
    pub mc_ident: u16,
    pub events: Vec<Event>,
    pub connections: Vec<ClientOutcome>,
}

/// A Test Connection's socket.
struct Connection {
    /// It reports where each datagram arrived, for the multicast and
    /// broadcast check (RFC 9946 §6; ). Until its Data Phase starts:
    /// then it leaves Tokio's reactor, which would otherwise be woken by
    /// every datagram that arrives on it and every send the kernel
    /// completes, and is kept in `held`.
    socket: Option<Listener>,
    /// The socket once its Data Phase has started, open but unread until
    /// the Test ends, so the peer's last datagrams draw no ICMP errors.
    held: Option<std::net::UdpSocket>,
    local: SocketAddr,
    /// The same socket for its Data Phase Engine, until the engine takes
    /// it: from then on the Control Phase no longer reads it.
    engine_socket: Option<std::net::UdpSocket>,
    engine: Option<Engine>,
    pending: VecDeque<PendingSend>,
}

struct PendingSend {
    transmit: Transmit,
    retry_at: Option<tokio::time::Instant>,
    retried_icmp: bool,
}

impl Connection {
    fn poll_send(
        &mut self,
        cx: &mut Context<'_>,
        before_send: &mut impl FnMut(&Transmit) -> io::Result<()>,
    ) -> Poll<io::Result<()>> {
        let (Some(socket), Some(pending)) = (&self.socket, self.pending.front_mut()) else {
            return Poll::Pending;
        };
        if pending
            .retry_at
            .is_some_and(|at| at > tokio::time::Instant::now())
        {
            return Poll::Pending;
        }
        pending.retry_at = None;
        let result = std::task::ready!(socket.poll_send_to(
            cx,
            &pending.transmit.payload,
            pending.transmit.destination,
            None,
            || before_send(&pending.transmit),
        ));
        match &result {
            Ok(()) => {
                self.pending.pop_front();
            }
            Err(error) => match classify(Call::ControlSend, error) {
                Action::Fatal | Action::TooLarge => {
                    self.pending.pop_front();
                }
                Action::PathError(_) if pending.retried_icmp => {
                    // The failed call consumed the pending ICMP error. Only
                    // one retry belongs to this PDU, as in the Data Phase.
                    self.pending.pop_front();
                }
                Action::PathError(_) => pending.retried_icmp = true,
                _ => pending.retry_at = Some(tokio::time::Instant::now() + CONTROL_RETRY_DELAY),
            },
        }
        Poll::Ready(result)
    }
}

/// The Test's mcIdent, from the port of a UDP socket the Client holds for
/// the Test and its address toward `server`, and the socket; a
/// random one if the host won't give such a socket.
fn draw_mc_ident(server: SocketAddr) -> (NonZeroU16, Option<std::net::UdpSocket>) {
    let unspecified = match server {
        SocketAddr::V4(_) => SocketAddr::from(([0, 0, 0, 0], 0)),
        SocketAddr::V6(_) => SocketAddr::from(([0u16; 8], 0)),
    };
    // Connecting sends nothing: it picks the local address a datagram to
    // the Server would leave from.
    let held = std::net::UdpSocket::bind(unspecified).and_then(|socket| {
        socket.connect(server)?;
        Ok((socket.local_addr()?, socket))
    });
    if let Ok((local, socket)) = held
        && let Some(mc_ident) = crate::protocol::mc_ident_for(local)
    {
        return (mc_ident, Some(socket));
    }
    let mut rng = SplitMix64(seed());
    let random = NonZeroU16::new((rng.next() % 0xFFFF) as u16 + 1).expect("non-zero");
    (random, None)
}

/// Runs the Test `plan` describes: the core's Multi-Connection Coordinator
/// opens a socket for each Test Connection, and each Test Connection's
/// Data Phase runs on a Data Phase Engine of its own. It returns once every
/// Test Connection has ended. `config` is the Client's settings for the
/// Test, and the Test's mcIdent is drawn fresh here; the Coordinator
/// completes them for each Test Connection from the plan. Their
/// datagrams leave with `ip`'s DF and MaxHops, and each Data Phase asks for
/// socket buffers of `socket_buffer_size` bytes, its engine running as
/// `engine` says. `notify` hears the Test's
/// events, with the mcIndex of the Test Connection one is about;
/// `notify_data` each Data Phase's. A socket error ends only its Test
/// Connection, which the Coordinator then counts as lost. When
/// `interrupted` completes, the Coordinator hears that the Client user
/// interrupted the Test, and stops it.
#[allow(clippy::too_many_arguments)]
pub async fn run_test(
    plan: TestPlan,
    config: ClientConfig,
    ip: IpOptions,
    socket_buffer_size: usize,
    engine: EngineSettings,
    notify: impl FnMut(Option<u8>, &Event),
    notify_data: impl FnMut(u8, &DataEvent),
    interrupted: impl std::future::Future<Output = ()>,
) -> io::Result<TestOutcome> {
    run_test_with_send(
        plan,
        config,
        ip,
        socket_buffer_size,
        engine,
        notify,
        notify_data,
        interrupted,
        |_| Ok(()),
    )
    .await
}

// The same caller with a private send-attempt seam. Production passes a no-op;
// tests can fail an actual queued Control PDU before the socket accepts it.
#[allow(clippy::too_many_arguments)]
async fn run_test_with_send(
    plan: TestPlan,
    config: ClientConfig,
    ip: IpOptions,
    socket_buffer_size: usize,
    engine: EngineSettings,
    mut notify: impl FnMut(Option<u8>, &Event),
    mut notify_data: impl FnMut(u8, &DataEvent),
    interrupted: impl std::future::Future<Output = ()>,
    mut before_send: impl FnMut(&Transmit) -> io::Result<()>,
) -> io::Result<TestOutcome> {
    let mut interrupted = std::pin::pin!(interrupted);
    let mut was_interrupted = false;
    let clock = Clock::new();
    // Held until the Test ends, so no other Client on this host draws its
    // mcIdent meanwhile.
    let (mc_ident, _held) = draw_mc_ident(plan.servers[0]);
    let servers = plan.servers.clone();
    let count = usize::from(plan.test_connections.get());
    let mut coordinator = Coordinator::new(plan, mc_ident, config, clock.now());
    let mut outcome = TestOutcome {
        mc_ident: mc_ident.get(),
        events: Vec::new(),
        connections: (0..count)
            .map(|i| ClientOutcome {
                server: servers[i % servers.len()],
                events: Vec::new(),
                data_events: Vec::new(),
                data_event_times: Vec::new(),
                local: None,
                mc_ident: mc_ident.get(),
                result: ClientResult::Abandoned,
                engine_facts: EngineFacts::default(),
            })
            .collect(),
    };
    // Whether each Test Connection's result is final.
    let mut decided = vec![false; count];
    let mut directions: Vec<Option<Direction>> = vec![None; count];
    let (engine_sender, mut from_engine) = crate::runtime::engine::reports::channel();
    let mut connections: HashMap<SocketId, Connection> = HashMap::new();
    let mut sends_first = false;
    // Each Test Connection's outcome is kept by its mcIndex.
    let index = |socket: SocketId| {
        usize::from(Coordinator::mc_index_of(socket).expect("a Test Connection's socket"))
    };
    // A Test Connection's socket failed: its result says so, and the
    // Coordinator counts it lost.
    macro_rules! socket_failed {
        ($socket:expr, $error:expr) => {{
            let socket = $socket;
            let i = index(socket);
            if !decided[i] {
                decided[i] = true;
                outcome.connections[i].result = ClientResult::SocketError($error);
            }
            if let Some(connection) = connections.remove(&socket) {
                drop(connection.engine);
            }
            coordinator.handle_socket_closed(clock.now(), socket);
        }};
    }
    macro_rules! control_send_error {
        ($socket:expr, $error:expr) => {{
            let socket = $socket;
            let error = $error;
            match classify(Call::ControlSend, &error) {
                Action::Fatal => socket_failed!(socket, error.to_string()),
                Action::TooLarge => {
                    coordinator.handle_transport_error(
                        clock.now(),
                        socket,
                        TransportError::MsgSize,
                    );
                }
                Action::PathError(kind) => {
                    let mut errors = path_errors(&connections[&socket], &error);
                    if errors.is_empty() {
                        errors.push(kind);
                    }
                    for kind in errors {
                        coordinator.handle_transport_error(clock.now(), socket, kind);
                    }
                }
                _ => {}
            }
        }};
    }
    loop {
        while let Some(output) = coordinator.poll_output() {
            match output {
                Output::OpenSocket {
                    socket, local_ip, ..
                } => {
                    let bound = bind_shared(local_ip, 0, ip).and_then(|(tokio_socket, engine)| {
                        let local = tokio_socket.local_addr()?;
                        Ok((local, Listener::for_client(tokio_socket)?, engine))
                    });
                    match bound {
                        Ok((local, tokio_socket, engine_socket)) => {
                            connections.insert(
                                socket,
                                Connection {
                                    socket: Some(tokio_socket),
                                    held: None,
                                    local,
                                    engine_socket: Some(engine_socket),
                                    engine: None,
                                    pending: VecDeque::new(),
                                },
                            );
                            outcome.connections[index(socket)].local = Some(local);
                            coordinator.handle_socket_opened(clock.now(), socket, local);
                        }
                        Err(error) => socket_failed!(socket, error.to_string()),
                    }
                }
                Output::Transmit(transmit) => {
                    // Its Data Phase's engine alone sends on it once started.
                    let socket = transmit.socket;
                    let Some(connection) = connections.get_mut(&socket) else {
                        continue;
                    };
                    if connection.socket.is_none() {
                        continue;
                    }
                    connection.pending.push_back(PendingSend {
                        transmit,
                        retry_at: None,
                        retried_icmp: false,
                    });
                    // Try immediately, without waiting here. A blocked send
                    // stays with its connection while the loop handles others.
                    let sent = std::future::poll_fn(|cx| {
                        Poll::Ready(connection.poll_send(cx, &mut before_send))
                    })
                    .await;
                    if let Poll::Ready(Err(error)) = sent {
                        control_send_error!(socket, error);
                    }
                }
                Output::TestConnectionEvent { socket, event } => {
                    let i = index(socket);
                    if matches!(
                        event,
                        Event::TestConnectionAbandoned
                            | Event::ControlPhaseFailed(_)
                            | Event::ActivationRejected
                    ) && let Some(connection) = connections.get_mut(&socket)
                    {
                        // The core ended this Control Phase. Its unsent PDU
                        // must not start the peer after that decision.
                        connection.pending.clear();
                    }
                    notify(Some(i as u8), &event);
                    let result = match &event {
                        Event::TestActivated { direction, .. } => {
                            directions[i] = Some(*direction);
                            None
                        }
                        Event::ActivationRejected => Some(ClientResult::ActivationRejected),
                        Event::ControlPhaseFailed(failure) => Some(ClientResult::Failed(*failure)),
                        Event::TestConnectionAbandoned => Some(ClientResult::Abandoned),
                        _ => None,
                    };
                    if let Some(result) = result
                        && !decided[i]
                    {
                        decided[i] = true;
                        outcome.connections[i].result = result;
                    }
                    outcome.connections[i].events.push(event);
                }
                Output::Event(event) => {
                    notify(None, &event);
                    outcome.events.push(event);
                }
                Output::StartDataPhase { socket, .. } => {
                    let data_phase = coordinator.take_data_phase(socket).expect("announced");
                    let Some(connection) = connections.get_mut(&socket) else {
                        continue;
                    };
                    let Some(engine_socket) = connection.engine_socket.take() else {
                        continue;
                    };
                    // From now on the engine reads the socket, and it leaves
                    // Tokio's reactor.
                    connection.pending.clear();
                    if let Some(listener) = connection.socket.take() {
                        match listener.into_std() {
                            Ok(held) => connection.held = Some(held),
                            Err(error) => {
                                socket_failed!(socket, error.to_string());
                                continue;
                            }
                        }
                    }
                    let spawned = engine::spawn(
                        socket,
                        data_phase,
                        engine_socket,
                        false,
                        socket_buffer_size,
                        clock,
                        engine,
                        engine_sender.clone(),
                    );
                    match spawned {
                        Ok(spawned) => {
                            if let Some(connection) = connections.get_mut(&socket) {
                                connection.engine = Some(spawned);
                            }
                        }
                        Err(error) => socket_failed!(socket, error.to_string()),
                    }
                }
                Output::StopDataPhase(socket) => {
                    if let Some(engine) = connections.get(&socket).and_then(|c| c.engine.as_ref()) {
                        engine.stop();
                    }
                }
                Output::CloseSocket(socket) => {
                    connections.remove(&socket);
                    coordinator.handle_socket_closed(clock.now(), socket);
                }
                // Only a Server's Test Connections are connected.
                Output::ConnectSocket { .. } => {}
            }
        }
        let running = connections.values().any(|c| c.engine.is_some());
        let deadline = coordinator.poll_timeout().map(|at| clock.instant(at));
        let retry = connections
            .values()
            .filter_map(|connection| connection.pending.front()?.retry_at)
            .min();
        if deadline.is_none() && !running {
            break;
        }
        tokio::select! {
            () = &mut interrupted, if !was_interrupted => {
                was_interrupted = true;
                coordinator.handle_interrupt(clock.now());
            }
            // One datagram at a time, so everything after the one that
            // starts a Data Phase stays in the socket for its engine.
            io = std::future::poll_fn(|cx| control_io(&mut connections, cx, &mut before_send, sends_first)) => {
                sends_first = !sends_first;
                match io {
                    ControlIo::Sent(socket, Err(error)) => control_send_error!(socket, error),
                    ControlIo::Sent(_, Ok(())) => {},
                    ControlIo::Received((socket, received)) => match received {
                    Ok((payload, source, arrival)) => {
                        let bound = connections[&socket].local;
                        let meta = DatagramMeta {
                            socket,
                            source,
                            // Where it arrived, or the bound (wildcard)
                            // address where the OS can't tell.
                            local: arrival.map_or(bound, |a| a.at(bound.port())),
                            ip_version: ip_version(source),
                            // Not asked of the OS, as on the Server.
                            tos: None,
                            actual_len: payload.len(),
                        };
                        coordinator.handle_datagram(clock.now(), meta, &payload);
                    }
                    Err(error) if classify(Call::ControlRead, &error) == Action::Fatal => {
                        socket_failed!(socket, error.to_string())
                    }
                    // For example an ICMP error from an earlier send,
                    // reported on the next receive (ADR 0006): a Path MTU
                    // or time-exceeded one goes to the Coordinator.
                    Err(error) => {
                        for kind in path_errors(&connections[&socket], &error) {
                            coordinator.handle_transport_error(clock.now(), socket, kind);
                        }
                    }
                    }
                }
            }
            message = from_engine.recv() => match message.expect("the loop holds a sender") {
                FromEngine::Event { socket, event } => {
                    let i = index(socket);
                    coordinator.handle_data_event(clock.now(), socket, &event);
                    notify_data(i as u8, &event);
                    outcome.connections[i].data_events.push(event);
                    outcome.connections[i].data_event_times.push(wall_now());
                }
                FromEngine::Exited {
                    socket,
                    reason,
                    error,
                    facts,
                } => {
                    let i = index(socket);
                    outcome.connections[i].engine_facts = facts;
                    if let Some(connection) = connections.get_mut(&socket) {
                        connection.engine = None;
                    }
                    if !decided[i] {
                        decided[i] = true;
                        outcome.connections[i].result = ClientResult::Finished {
                            direction: directions[i].expect("the Data Phase follows activation"),
                            reason,
                            error,
                        };
                    }
                    // Reported on every exit, after the engine's last Data
                    // Event (Endpoint::handle_socket_closed). The socket
                    // itself stays open, unread, until the Test ends, so
                    // the peer's last datagrams draw no ICMP errors.
                    coordinator.handle_socket_closed(clock.now(), socket);
                }
            },
            () = async {
                match retry {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            } => {},
            () = async {
                match deadline {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            } => {
                // An ICMP error doesn't always wake a read: before a timer
                // decides a Setup went unanswered, the error queues say
                // whether a path error explains it.
                #[cfg(any(target_os = "linux", target_os = "android"))]
                for (&socket, connection) in &connections {
                    if let Some(engine_socket) = &connection.engine_socket {
                        for kind in crate::runtime::socket::errqueue::drain(engine_socket) {
                            coordinator.handle_transport_error(clock.now(), socket, kind);
                        }
                    }
                }
                coordinator.handle_timeout(clock.now());
            }
        }
    }
    Ok(outcome)
}

/// What a receive error on a Control Phase socket was: on Linux each error
/// the socket queued, by its ICMP type; elsewhere the path error the error
/// itself reports, as the classifier reads it: on Windows WSAENETRESET is
/// time exceeded (ADR 0006). The Coordinator ignores other ICMP
/// errors.
fn path_errors(connection: &Connection, error: &io::Error) -> Vec<TransportError> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let _ = error;
        // The same socket, as the engine will have it.
        connection
            .engine_socket
            .as_ref()
            .map(crate::runtime::socket::errqueue::drain)
            .unwrap_or_default()
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        let _ = connection;
        match classify(Call::ControlRead, error) {
            Action::PathError(kind) => vec![kind],
            _ => Vec::new(),
        }
    }
}

/// A datagram received on a Test Connection's socket, with where it
/// arrived if the OS says, or the error.
type Received = (SocketId, io::Result<(Vec<u8>, SocketAddr, Option<Arrival>)>);

enum ControlIo {
    Received(Received),
    Sent(SocketId, io::Result<()>),
}

fn control_io(
    connections: &mut HashMap<SocketId, Connection>,
    cx: &mut Context<'_>,
    before_send: &mut impl FnMut(&Transmit) -> io::Result<()>,
    sends_first: bool,
) -> Poll<ControlIo> {
    // Alternate which side is polled first. Buffered reads cannot starve a
    // pending Control PDU, and repeated send errors cannot starve reads.
    if !sends_first && let Poll::Ready(received) = receive(connections, cx) {
        return Poll::Ready(ControlIo::Received(received));
    }
    for (&socket, connection) in connections.iter_mut() {
        if let Poll::Ready(result) = connection.poll_send(cx, before_send) {
            return Poll::Ready(ControlIo::Sent(socket, result));
        }
    }
    if sends_first && let Poll::Ready(received) = receive(connections, cx) {
        return Poll::Ready(ControlIo::Received(received));
    }
    Poll::Pending
}

/// A datagram from one of the sockets the Control Phase still reads, or
/// its receive error.
fn receive(
    connections: &HashMap<SocketId, Connection>,
    cx: &mut std::task::Context<'_>,
) -> std::task::Poll<Received> {
    let mut buf = [0u8; MAX_DATAGRAM];
    for (&id, connection) in connections {
        let Some(listener) = &connection.socket else {
            continue;
        };
        if let std::task::Poll::Ready(received) = listener.poll_recv_from(cx, &mut buf) {
            let received =
                received.map(|(len, source, arrival)| (buf[..len].to_vec(), source, arrival));
            return std::task::Poll::Ready((id, received));
        }
    }
    std::task::Poll::Pending
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    async fn control_send_test(
        count: u8,
        stop_on_activation: bool,
        interrupted: impl std::future::Future<Output = ()>,
        before_send: impl FnMut(&Transmit) -> io::Result<()>,
    ) -> TestOutcome {
        control_send_test_observed(
            count,
            stop_on_activation,
            interrupted,
            before_send,
            |_, _| {},
            |_| {},
        )
        .await
    }

    async fn control_send_test_observed(
        count: u8,
        stop_on_activation: bool,
        interrupted: impl std::future::Future<Output = ()>,
        before_send: impl FnMut(&Transmit) -> io::Result<()>,
        mut notify: impl FnMut(Option<u8>, &Event),
        mut notify_server: impl FnMut(&Event),
    ) -> TestOutcome {
        use tokio::sync::oneshot;

        use crate::runtime::contract::{fixed_rate_test, server_config};
        use crate::runtime::{DEFAULT_SOCKET_BUFFER_SIZE, ServerNotice, ServerOptions, serve};

        let (listening, address) = oneshot::channel();
        let mut listening = Some(listening);
        let (stop_server, stopped) = oneshot::channel();
        let server = serve(
            ServerOptions {
                listen: vec![std::net::Ipv4Addr::LOCALHOST.into()],
                port: 0,
                ephemeral_ports: 49152..=65535,
                ip: IpOptions::default(),
                socket_buffer_size: DEFAULT_SOCKET_BUFFER_SIZE,
                engine: EngineSettings::default(),
                core: server_config(),
            },
            move |notice| {
                if let ServerNotice::Core(event) = &notice {
                    notify_server(event);
                }
                if let ServerNotice::Listening(addresses) = notice
                    && let Some(listening) = listening.take()
                {
                    let _ = listening.send(addresses[0]);
                }
            },
            async {
                let _ = stopped.await;
                "the Control send test is over"
            },
        );
        let client = async {
            let standard = address.await.expect("the Server listens");
            let (mut plan, config) = fixed_rate_test(standard, Direction::Upstream, 1);
            plan.time_limit = Duration::from_secs(1);
            plan.test_connections = std::num::NonZeroU8::new(count).unwrap();
            let (activated, activation) = oneshot::channel();
            let mut activated = Some(activated);
            let outcome = run_test_with_send(
                plan,
                config,
                IpOptions::default(),
                DEFAULT_SOCKET_BUFFER_SIZE,
                EngineSettings::default(),
                |index, event| {
                    notify(index, event);
                    if stop_on_activation
                        && matches!(event, Event::TestActivated { .. })
                        && let Some(activated) = activated.take()
                    {
                        let _ = activated.send(());
                    }
                },
                |_, _| {},
                async {
                    tokio::select! {
                        _ = activation => {},
                        () = interrupted => {},
                    }
                },
                before_send,
            )
            .await;
            let _ = stop_server.send(());
            outcome
        };
        let (server, outcome) = tokio::join!(server, client);
        server.expect("the Server stops cleanly");
        outcome.expect("the Client ran")
    }

    // ADR 0006, "Transport errors are classified on both receive and send,
    // not suppressed": a Control PDU refused with ENOBUFS remains unsent.
    // Exercise the real Client caller and Server over loopback, rather than
    // only proving that the classifier names the error correctly.
    #[tokio::test(flavor = "current_thread")]
    async fn control_send_retries_activation_after_no_buffers() {
        let mut attempts = Vec::new();
        let outcome = control_send_test(1, true, std::future::pending(), |transmit| {
            if transmit.payload.starts_with(&[0xac, 0xe2]) {
                attempts.push((transmit.destination, transmit.payload.clone()));
                if attempts.len() == 1 {
                    return Err(crate::runtime::socket::classify::inject::no_buffers());
                }
            }
            Ok(())
        })
        .await;
        assert_eq!(attempts.len(), 2, "the unsent Activation must be retried");
        assert_eq!(attempts[0], attempts[1], "retry the same queued PDU");
        assert_eq!(attempts[0].1.len(), 104);
        assert!(
            outcome.connections[0]
                .events
                .iter()
                .any(|event| matches!(event, Event::TestActivated { .. })),
            "the real Server must accept the retried Activation: {outcome:?}"
        );
    }

    // ADR 0006: both an interrupted call and a consumed ICMP error leave
    // this PDU unsent. Neither permits a second successful transmission.
    #[tokio::test(flavor = "current_thread")]
    async fn control_send_retries_interrupted_and_icmp_errors() {
        for kind in [io::ErrorKind::Interrupted, io::ErrorKind::ConnectionRefused] {
            let mut attempts = 0;
            let outcome = control_send_test(1, true, std::future::pending(), |transmit| {
                if transmit.payload.starts_with(&[0xac, 0xe2]) {
                    attempts += 1;
                    if attempts == 1 {
                        return Err(io::Error::from(kind));
                    }
                }
                Ok(())
            })
            .await;
            assert_eq!(attempts, 2, "{kind:?}: exactly one successful send");
            assert!(
                outcome.connections[0]
                    .events
                    .iter()
                    .any(|event| matches!(event, Event::TestActivated { .. }))
            );
        }
    }

    // ADR 0006 permits one retry after consuming an ICMP error, rather than
    // spinning on a repeatedly failing path.
    #[tokio::test(flavor = "current_thread")]
    async fn control_send_icmp_retry_is_bounded() {
        let mut attempts = 0;
        let outcome = control_send_test(1, false, std::future::pending(), |transmit| {
            if transmit.payload.starts_with(&[0xac, 0xe2]) {
                attempts += 1;
                assert!(attempts <= 2, "only one ICMP retry is permitted");
                return Err(io::Error::from(io::ErrorKind::ConnectionRefused));
            }
            Ok(())
        })
        .await;
        assert_eq!(attempts, 2);
        assert!(matches!(
            outcome.connections[0].result,
            ClientResult::Failed(ControlPhaseFailure::TestInitiationTimedOut { .. })
        ));
    }

    // ADR 0006: EMSGSIZE is reported to the core as a Path MTU event. This
    // datagram cannot leave with DF set, so it is not retried.
    #[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
    #[tokio::test(flavor = "current_thread")]
    async fn control_send_reports_path_mtu_without_retrying() {
        let mut attempts = 0;
        let outcome = control_send_test(1, false, std::future::pending(), |transmit| {
            if transmit.payload.starts_with(&[0xac, 0xe2]) {
                attempts += 1;
                return Err(nix::errno::Errno::EMSGSIZE.into());
            }
            Ok(())
        })
        .await;
        assert_eq!(attempts, 1);
        assert!(
            outcome.connections[0]
                .events
                .contains(&Event::PathError(TransportError::MsgSize))
        );
    }

    // ADR 0006 and the core's Test initiation timer: persistent send-buffer
    // pressure yields to that timer instead of spinning or losing the PDU.
    #[tokio::test(flavor = "current_thread")]
    async fn control_send_backpressure_keeps_core_deadline_responsive() {
        let mut attempts = 0;
        let outcome = control_send_test(1, false, std::future::pending(), |transmit| {
            if transmit.payload.starts_with(&[0xac, 0xe2]) {
                attempts += 1;
                assert!(
                    attempts < 10_000,
                    "persistent backpressure must yield to the core timer"
                );
                return Err(crate::runtime::socket::classify::inject::no_buffers());
            }
            Ok(())
        })
        .await;
        assert!(attempts > 1, "keep the PDU pending until the core ends it");
        assert!(
            matches!(
                outcome.connections[0].result,
                ClientResult::Failed(ControlPhaseFailure::TestInitiationTimedOut {
                    exchange: crate::protocol::ControlExchange::Activation,
                    ..
                })
            ),
            "the core still owns the deadline: {outcome:?}"
        );
    }

    // ADR 0006 and Conformance Record client-interrupt: a blocked write
    // does not prevent interruption, and abandoned PDUs are no longer sent.
    #[tokio::test(flavor = "current_thread")]
    async fn control_send_backpressure_keeps_interrupt_responsive() {
        let (interrupt, interrupted) = tokio::sync::oneshot::channel();
        let mut interrupt = Some(interrupt);
        let mut attempts = 0;
        let outcome = control_send_test(
            1,
            false,
            async {
                let _ = interrupted.await;
            },
            |transmit| {
                if transmit.payload.starts_with(&[0xac, 0xe2]) {
                    attempts += 1;
                    assert!(
                        attempts < 10_000,
                        "persistent backpressure must yield to interruption"
                    );
                    if let Some(interrupt) = interrupt.take() {
                        let _ = interrupt.send(());
                    }
                    return Err(crate::runtime::socket::classify::inject::no_buffers());
                }
                Ok(())
            },
        )
        .await;
        assert!(attempts > 0);
        assert!(
            outcome
                .events
                .contains(&Event::TestAborted(crate::protocol::TestAbort::Interrupted))
        );
        assert!(matches!(
            outcome.connections[0].result,
            ClientResult::Abandoned
        ));
    }

    // ADR 0006 and Conformance Record multi-connection-isolated-failures:
    // one blocked Control Phase socket does not hold up another connection.
    #[tokio::test(flavor = "current_thread")]
    async fn control_send_backpressure_does_not_hold_up_other_connections() {
        let mut blocked = 0;
        let outcome = control_send_test(2, true, std::future::pending(), |transmit| {
            if transmit.payload.starts_with(&[0xac, 0xe2])
                && Coordinator::mc_index_of(transmit.socket) == Some(0)
            {
                blocked += 1;
                assert!(blocked < 10_000, "the other connection must still progress");
                return Err(crate::runtime::socket::classify::inject::no_buffers());
            }
            Ok(())
        })
        .await;
        assert!(blocked > 0);
        assert!(
            !outcome.connections[0]
                .events
                .iter()
                .any(|event| matches!(event, Event::TestActivated { .. }))
        );
        assert!(
            outcome.connections[1]
                .events
                .iter()
                .any(|event| matches!(event, Event::TestActivated { .. })),
            "the unblocked Test Connection activates: {outcome:?}"
        );
    }

    // ADR 0006 and Conformance Record multi-connection-isolated-failures:
    // once the core ends one Control Phase, its unsent PDU must not activate
    // the Server later while the Minimum Test Connections lets another run.
    #[tokio::test(flavor = "current_thread")]
    async fn control_send_discards_pending_activation_after_connection_deadline() {
        let released = std::cell::Cell::new(false);
        let mut blocked = 0;
        let mut late_attempts = 0;
        let mut server_activations = 0;
        let (expired, expiration) = tokio::sync::oneshot::channel();
        let mut expired = Some(expired);
        let outcome = control_send_test_observed(
            2,
            false,
            async {
                expiration
                    .await
                    .expect("the blocked Control Phase times out");
                // Leave the healthy connection running through further I/O
                // turns after pressure is released, before interrupting it.
                tokio::time::sleep(Duration::from_millis(50)).await;
            },
            |transmit| {
                if transmit.payload.starts_with(&[0xac, 0xe2])
                    && Coordinator::mc_index_of(transmit.socket) == Some(0)
                {
                    if released.get() {
                        late_attempts += 1;
                    } else {
                        blocked += 1;
                        assert!(blocked < 10_000, "the core deadline must remain responsive");
                        return Err(crate::runtime::socket::classify::inject::no_buffers());
                    }
                }
                Ok(())
            },
            |index, event| {
                if index == Some(0)
                    && matches!(
                        event,
                        Event::ControlPhaseFailed(ControlPhaseFailure::TestInitiationTimedOut {
                            exchange: crate::protocol::ControlExchange::Activation,
                            ..
                        })
                    )
                {
                    released.set(true);
                    if let Some(expired) = expired.take() {
                        let _ = expired.send(());
                    }
                }
            },
            |event| {
                if matches!(event, Event::TestConnectionActivated { .. }) {
                    server_activations += 1;
                }
            },
        )
        .await;
        assert!(blocked > 1, "the Activation was pending until its deadline");
        assert!(
            released.get(),
            "release pressure only after the core failure"
        );
        assert_eq!(
            late_attempts, 0,
            "an ended connection must never send its queued Activation"
        );
        assert_eq!(
            server_activations, 1,
            "only the healthy connection activates the Server"
        );
        assert!(matches!(
            outcome.connections[0].result,
            ClientResult::Failed(ControlPhaseFailure::TestInitiationTimedOut {
                exchange: crate::protocol::ControlExchange::Activation,
                ..
            })
        ));
        assert!(
            !outcome.connections[0]
                .events
                .iter()
                .any(|event| matches!(event, Event::TestActivated { .. }))
        );
        assert!(
            outcome.connections[1]
                .events
                .iter()
                .any(|event| matches!(event, Event::TestActivated { .. })),
            "the Minimum Test Connections of one keeps the healthy connection active"
        );
        assert!(
            outcome
                .events
                .contains(&Event::TestAborted(crate::protocol::TestAbort::Interrupted))
        );
    }

    // Conformance Record: setup-response-from-another-address. The locally
    // selected IPv6 scope must survive name/address resolution
    // before it becomes the core's destination authorization.
    #[tokio::test(flavor = "current_thread")]
    async fn resolution_preserves_ipv6_scope_and_canonicalizes_mapped_ipv4() {
        assert_eq!(
            resolve("[fe80::1%7]:24602").await.unwrap(),
            "[fe80::1%7]:24602".parse::<SocketAddr>().unwrap()
        );
        assert_eq!(
            resolve("[::ffff:192.0.2.1]:24602").await.unwrap(),
            "192.0.2.1:24602".parse::<SocketAddr>().unwrap()
        );
    }

    // : many Clients on one host, all starting together, never share
    // an mcIdent while they hold their sockets (a random draw would: among
    // 1000, a collision is all but certain)
    #[test]
    fn clients_on_one_host_draw_different_mc_idents() {
        // This check holds 1,000 descriptors. Give it its own process so
        // concurrent socket tests do not exhaust a 1,024-descriptor limit.
        const CHILD: &str = "UDPSTP_MC_IDENT_CHECK_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "runtime::client::tests::clients_on_one_host_draw_different_mc_idents",
                    "--test-threads=1",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
                "{output:?}"
            );
            return;
        }
        let server: SocketAddr = "127.0.0.1:24601".parse().unwrap();
        let draws: Vec<_> = (0..1000).map(|_| draw_mc_ident(server)).collect();
        assert!(draws.iter().all(|(_, held)| held.is_some()));
        let idents: HashSet<u16> = draws.iter().map(|(ident, _)| ident.get()).collect();
        assert_eq!(idents.len(), draws.len());
    }
}
