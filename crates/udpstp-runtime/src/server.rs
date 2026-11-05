//! The Server's Control Phase on Tokio: the standard-port listeners, the
//! Test Connections' sockets, and the core's deadlines (ADR 0003, ADR 0007).

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::ops::RangeInclusive;
use std::sync::Arc;

use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::arrival::Listener;
use crate::buffers::SocketBuffers;
use crate::engine::{self, Engine, EngineFacts, EngineSettings, Executor, FromEngine};
use crate::ip_options::{self, IpOptions};
use crate::transport::{FromReader, QUEUE, spawn_reader};

use udpstp_core::{
    DataEvent, DatagramMeta, EndReason, Endpoint, Event, IpVersion, Output, Server, ServerConfig,
    SocketId,
};

use crate::clock::{Clock, SplitMix64, seed};

/// How many ports of the range are tried in order after the random ones.
const IN_ORDER_ATTEMPTS: usize = 1024;

/// How the Server runs.
pub struct ServerOptions {
    /// The addresses to listen on; empty, the IPv4 wildcard and, if the
    /// host has IPv6, the IPv6 wildcard.
    pub listen: Vec<IpAddr>,
    /// The Standard Port the listeners bind (24601 unless configured).
    pub port: u16,
    /// The Ephemeral Ports Test Connections' sockets are bound to, picked
    /// at random: the Server's own range, not the OS's.
    pub ephemeral_ports: RangeInclusive<u16>,
    /// DF and MaxHops on every socket, so on all test traffic.
    pub ip: IpOptions,
    /// Each Test Connection's receive and send buffer in its Data Phase,
    /// in bytes.
    pub socket_buffer_size: usize,
    /// How its Data Phase Engines run, where that may differ from their
    /// own choices: experimental.
    pub engine: EngineSettings,
    pub core: ServerConfig,
}

/// Something the Server runtime reports for logging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerNotice {
    /// The listeners are bound; the Server is ready.
    Listening(Vec<SocketAddr>),
    /// No IPv6 listener: the host has no IPv6.
    NoIpv6Listener(String),
    /// An event from the core.
    Core(Event),
    /// An event from a Test Connection's Data Phase.
    Data { socket: SocketId, event: DataEvent },
    /// A Test Connection's Data Phase Engine exited; the Test Connection is
    /// closed. `reason` is `None` when the engine failed.
    DataPhaseEnded {
        socket: SocketId,
        reason: Option<EndReason>,
        error: Option<String>,
        /// How its Data Phase ran on this host, as its engine reported:
        /// the default if its engine never started.
        engine_facts: EngineFacts,
    },
    /// No port in the range could be bound for a Test Connection; the
    /// accepted Setup is abandoned.
    NoEphemeralPort { socket: SocketId, error: String },
    /// A socket error that doesn't stop the Server.
    /// The binary's log limits these, which an attacker can trigger.
    SocketError { socket: SocketId, error: String },
    /// The OS's limits held a Test Connection's socket buffers below the
    /// size configured: at high rates datagrams may be dropped locally.
    SocketBuffersShort {
        socket: SocketId,
        buffers: SocketBuffers,
    },
    /// Received datagrams dropped because the core fell behind.
    DatagramsDropped { socket: SocketId, count: u64 },
    /// Once a second, for whatever reports periodically.
    Tick,
    /// How the Server's Data Phase Engines run, once, as it starts:
    /// the executor, the Engine Threads the pooled one may start (none for
    /// one thread per Test Connection), and its wake-up grouping window.
    /// `error` says why the CPUs the process may use couldn't be found, in
    /// which case the pooled executor starts one Engine Thread.
    DataPhaseEngines {
        executor: Executor,
        engine_threads: Option<usize>,
        grouping: std::time::Duration,
        error: Option<String>,
    },
    /// The Server is stopping, for `reason`: the core stops its Test
    /// Connections, and [`serve`] returns once they are released, or after
    /// 2 s.
    Stopping { reason: &'static str },
}

/// The longest a stopping Server waits for its Test Connections to end: a
/// Data Phase Engine that doesn't stop by then is left behind.
pub(crate) const STOP_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// A Test Connection's socket and the task reading it, then its Data
/// Phase Engine.
struct Owned {
    /// Tokio's handle, until its Data Phase starts: then it leaves Tokio's
    /// reactor, which would otherwise be woken by every datagram that
    /// arrives on it and every send the kernel completes. The core
    /// sends nothing on it from then on.
    socket: Option<Arc<UdpSocket>>,
    /// The Client it is connected to, once connected.
    peer: Option<SocketAddr>,
    reader: JoinHandle<()>,
    /// The same socket, for the engine's blocking reads.
    engine_socket: Option<std::net::UdpSocket>,
    engine: Option<Engine>,
    /// Windows: its port held on the wildcard address too.
    _guard: Option<Socket>,
}

/// Runs the Server until a local error on a listener stops it, or until
/// `shutdown` completes with its reason: then it answers no more Setup
/// Requests, stops every Test Connection, and returns once they are
/// released, their ends reported.
pub async fn serve(
    options: ServerOptions,
    mut notify: impl FnMut(ServerNotice),
    shutdown: impl std::future::Future<Output = &'static str>,
) -> io::Result<()> {
    let mut shutdown = std::pin::pin!(shutdown);
    // When a stopping Server returns at the latest.
    let mut stopping: Option<tokio::time::Instant> = None;
    // Every admitted Test Connection needs a port of its own.
    // (conformance: unactivated-test-connection-share)
    let ports = options.ephemeral_ports.clone().count();
    let unactivated = options
        .core
        .max_unactivated_test_connections
        .unwrap_or(options.core.max_test_connections / 2)
        .max(options.core.test_connections_per_test);
    if ports
        < options
            .core
            .max_test_connections
            .saturating_add(unactivated)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "ephemeral_ports has {ports} ports, fewer than max_test_connections ({}) plus the Unactivated Test Connection share ({unactivated})",
                options.core.max_test_connections
            ),
        ));
    }
    let clock = Clock::new();
    let (sender, mut received) = mpsc::channel(QUEUE);
    let (engine_sender, mut from_engines) = mpsc::unbounded_channel();
    // Each listener with the address it's bound to. The configured ones
    // must all bind; by default the IPv6 wildcard may be missing.
    let mut listeners: Vec<(IpAddr, Arc<Listener>)> = Vec::new();
    if options.listen.is_empty() {
        let v4 = Ipv4Addr::UNSPECIFIED.into();
        listeners.push((v4, Arc::new(bind_listener(v4, options.port, options.ip)?)));
        let v6 = Ipv6Addr::UNSPECIFIED.into();
        match bind_listener(v6, options.port, options.ip) {
            Ok(socket) => listeners.push((v6, Arc::new(socket))),
            Err(error) => notify(ServerNotice::NoIpv6Listener(error.to_string())),
        }
    } else {
        for &ip in &options.listen {
            let bound = bind_listener(ip, options.port, options.ip)
                .map_err(|e| io::Error::new(e.kind(), format!("listening on {ip}: {e}")))?;
            listeners.push((ip, Arc::new(bound)));
        }
    }
    for (_, listener) in &listeners {
        spawn_reader(SocketId::PRIMARY, listener.clone(), sender.clone());
    }
    let (mut engines, setup) = Engines::new(clock, options.engine, &engine_sender);
    notify(setup);
    notify(ServerNotice::Listening(
        listeners
            .iter()
            .filter_map(|(_, l)| l.local_addr().ok())
            .collect(),
    ));

    let mut server = Server::new(options.core);
    let mut owned: HashMap<SocketId, Owned> = HashMap::new();
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut rng = SplitMix64(seed());
    loop {
        // Act on everything the core has to say before waiting again.
        while let Some(output) = server.poll_output() {
            match output {
                Output::Transmit(transmit) if transmit.socket == SocketId::PRIMARY => {
                    // From the address the request arrived on (ADR 0007):
                    // its own listener, or its family's wildcard.
                    let family = |ip: &IpAddr| ip.is_ipv6() == transmit.destination.is_ipv6();
                    let listener = listeners
                        .iter()
                        .find(|(ip, _)| Some(*ip) == transmit.source)
                        .or_else(|| {
                            listeners
                                .iter()
                                .find(|(ip, _)| ip.is_unspecified() && family(ip))
                        })
                        .or_else(|| listeners.iter().find(|(ip, _)| family(ip)))
                        .map(|(_, l)| l.clone());
                    if let Some(listener) = listener
                        && let Err(error) = listener
                            .send_to(&transmit.payload, transmit.destination, transmit.source)
                            .await
                    {
                        notify(ServerNotice::SocketError {
                            socket: transmit.socket,
                            error: error.to_string(),
                        });
                    }
                }
                Output::Transmit(transmit) => {
                    let Some(o) = owned.get(&transmit.socket) else {
                        continue;
                    };
                    let peer = o.peer;
                    // A Test Connection's socket is bound to the address the
                    // Setup arrived on. A connected one sends without an
                    // address: macOS refuses one (EISCONN).
                    let sent = match o.socket.clone() {
                        Some(socket) if peer == Some(transmit.destination) => {
                            socket.send(&transmit.payload).await.map(drop)
                        }
                        Some(socket) => socket
                            .send_to(&transmit.payload, transmit.destination)
                            .await
                            .map(drop),
                        // Its Data Phase has started: the engine alone
                        // sends on it.
                        None => Err(io::Error::other(
                            "a Control PDU for a Test Connection in its Data Phase, not sent",
                        )),
                    };
                    if let Err(error) = sent {
                        notify(ServerNotice::SocketError {
                            socket: transmit.socket,
                            error: error.to_string(),
                        });
                    }
                }
                Output::OpenSocket {
                    socket,
                    local_ip,
                    scope_id,
                } => {
                    let local = match local_ip {
                        IpAddr::V6(ip) => {
                            SocketAddr::from(std::net::SocketAddrV6::new(ip, 0, 0, scope_id))
                        }
                        ip => SocketAddr::new(ip, 0),
                    };
                    match bind_ephemeral_port(local, &options.ephemeral_ports, options.ip, &mut rng)
                    {
                        Ok(port) => {
                            let bound = Arc::new(port.socket);
                            let reader = spawn_reader(socket, bound.clone(), sender.clone());
                            owned.insert(
                                socket,
                                Owned {
                                    socket: Some(bound),
                                    peer: None,
                                    reader,
                                    engine_socket: Some(port.engine),
                                    engine: None,
                                    _guard: port.guard,
                                },
                            );
                            server.handle_socket_opened(clock.now(), socket, port.local);
                        }
                        Err(error) => {
                            notify(ServerNotice::NoEphemeralPort {
                                socket,
                                error: error.to_string(),
                            });
                            server.handle_socket_closed(clock.now(), socket);
                        }
                    }
                }
                Output::CloseSocket(socket) => {
                    close(&mut owned, socket);
                    server.handle_socket_closed(clock.now(), socket);
                }
                Output::ConnectSocket { socket, peer } => {
                    // Bound to the Setup's arrival address, so connecting
                    // keeps it.
                    if let Some(o) = owned.get_mut(&socket)
                        && let Some(bound) = &o.socket
                    {
                        match bound.connect(peer).await {
                            Ok(()) => o.peer = Some(peer),
                            // Not fatal: the runtime still drops datagrams
                            // from other addresses, and sends with one.
                            Err(error) => notify(ServerNotice::SocketError {
                                socket,
                                error: format!("connecting to {peer}: {error}"),
                            }),
                        }
                    }
                }
                Output::StartDataPhase { socket, .. } => {
                    let started = match (server.take_data_phase(socket), owned.get_mut(&socket)) {
                        (Some(data_phase), Some(o)) => {
                            // From now on the engine reads the socket, and
                            // it leaves Tokio's reactor: once the
                            // aborted reader has dropped its handle too.
                            o.reader.abort();
                            o.socket = None;
                            let engine_socket = o.engine_socket.take().expect("taken once");
                            let connected = o.peer == Some(data_phase.peer());
                            engines
                                .start(
                                    socket,
                                    data_phase,
                                    engine_socket,
                                    connected,
                                    options.socket_buffer_size,
                                    clock,
                                    options.engine,
                                    &engine_sender,
                                )
                                .map(|engine| {
                                    if let Some(buffers) =
                                        engine.buffers().filter(SocketBuffers::short)
                                    {
                                        notify(ServerNotice::SocketBuffersShort {
                                            socket,
                                            buffers,
                                        });
                                    }
                                    o.engine = Some(engine);
                                })
                                .map_err(|e| e.to_string())
                        }
                        _ => Err("no Data Phase or socket".into()),
                    };
                    if let Err(error) = started {
                        notify(ServerNotice::DataPhaseEnded {
                            socket,
                            reason: None,
                            error: Some(error),
                            engine_facts: EngineFacts::default(),
                        });
                        close(&mut owned, socket);
                        server.handle_socket_closed(clock.now(), socket);
                    }
                }
                Output::Event(event) => notify(ServerNotice::Core(event)),
                // The core stops the Server's Test Connections this way:
                // the engine's exit ends it.
                Output::StopDataPhase(socket) => {
                    match owned.get(&socket).and_then(|o| o.engine.as_ref()) {
                        Some(engine) => engine.stop(),
                        None => {
                            close(&mut owned, socket);
                            server.handle_socket_closed(clock.now(), socket);
                        }
                    }
                }
                // Only the Client's Multi-Connection Coordinator asks.
                Output::TestConnectionEvent { .. } => {}
            }
        }
        if stopping.is_some() && owned.is_empty() {
            return Ok(());
        }
        let deadline = server.poll_timeout().map(|at| clock.instant(at));
        tokio::select! {
            reason = &mut shutdown, if stopping.is_none() => {
                notify(ServerNotice::Stopping { reason });
                stopping = Some(tokio::time::Instant::now() + STOP_WAIT);
                server.handle_shutdown(clock.now());
            }
            () = sleep_until(stopping), if stopping.is_some() => return Ok(()),
            message = received.recv() => match message.expect("the loop holds a sender") {
                FromReader::Datagram { socket, source, local, payload } => {
                    let meta = DatagramMeta {
                        socket,
                        source,
                        local,
                        ip_version: ip_version(source),
                        // Not asked of the OS: Control PDUs carry no DSCP
                        // to check, and the core doesn't read it.
                        tos: None,
                        actual_len: payload.len(),
                    };
                    server.handle_datagram(clock.now(), meta, &payload);
                }
                FromReader::Dropped { socket, count } => {
                    notify(ServerNotice::DatagramsDropped { socket, count });
                }
                FromReader::Error { socket, error } => notify(ServerNotice::SocketError {
                    socket,
                    error: error.to_string(),
                }),
                // A listener that can't read any more stops the Server.
                FromReader::Stopped { socket: SocketId::PRIMARY, error } => return Err(error),
                FromReader::Stopped { socket, error } => {
                    notify(ServerNotice::SocketError { socket, error: error.to_string() });
                    // Queued before its Data Phase started: the engine's
                    // exit reports the close, after its last Data Event
                    // (Endpoint::handle_socket_closed).
                    match owned.get(&socket).and_then(|o| o.engine.as_ref()) {
                        Some(engine) => engine.stop(),
                        None => {
                            close(&mut owned, socket);
                            server.handle_socket_closed(clock.now(), socket);
                        }
                    }
                }
            },
            message = from_engines.recv() => match message.expect("the loop holds a sender") {
                FromEngine::Event { socket, event } => {
                    // The core counts each Test's bytes for its Quotas.
                    server.handle_data_event(clock.now(), socket, &event);
                    notify(ServerNotice::Data { socket, event });
                }
                // The engine's exit ends the Test Connection and releases
                // its admission.
                FromEngine::Exited {
                    socket,
                    reason,
                    error,
                    facts,
                } => {
                    notify(ServerNotice::DataPhaseEnded {
                        socket,
                        reason,
                        error,
                        engine_facts: facts,
                    });
                    close(&mut owned, socket);
                    server.handle_socket_closed(clock.now(), socket);
                }
            },
            () = sleep_until(deadline) => server.handle_timeout(clock.now()),
            _ = tick.tick() => notify(ServerNotice::Tick),
        }
    }
}

/// Where the Server's Data Phase Engines run: the pooled executor
/// on Linux, a thread per Test Connection elsewhere.
enum Engines {
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    PerTestConnection,
    #[cfg(any(target_os = "linux", target_os = "android"))]
    Pool(Box<engine::pool::EnginePool<engine::ServerOpen>>),
}

impl Engines {
    /// The engines that run here, and the notice that says how they run:
    /// the pooled executor on Linux, its only one there (ADR 0008), a
    /// thread per Test Connection elsewhere.
    fn new(
        clock: Clock,
        settings: EngineSettings,
        report: &mpsc::UnboundedSender<FromEngine>,
    ) -> (Self, ServerNotice) {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            let open = engine::server_open();
            let (pool, allowed) =
                engine::pool::EnginePool::new(clock, settings, report.clone(), open);
            let (engine_threads, error) = match allowed {
                Ok(threads) => (threads, None),
                Err(error) => (1, Some(error.to_string())),
            };
            let notice = ServerNotice::DataPhaseEngines {
                executor: Executor::Pool,
                engine_threads: Some(engine_threads),
                grouping: settings.grouping,
                error,
            };
            (Self::Pool(Box::new(pool)), notice)
        }
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        {
            let _ = (clock, settings, report);
            let notice = ServerNotice::DataPhaseEngines {
                executor: Executor::PerTestConnection,
                engine_threads: None,
                grouping: std::time::Duration::ZERO,
                error: None,
            };
            (Self::PerTestConnection, notice)
        }
    }

    /// Starts Test Connection `socket`'s Data Phase, as `engine::spawn`
    /// does, on its own thread or on an Engine Thread.
    #[allow(clippy::too_many_arguments)]
    fn start(
        &mut self,
        socket: SocketId,
        data_phase: udpstp_core::DataPhase,
        engine_socket: std::net::UdpSocket,
        connected: bool,
        buffer_size: usize,
        clock: Clock,
        settings: EngineSettings,
        report: &mpsc::UnboundedSender<FromEngine>,
    ) -> io::Result<Engine> {
        match self {
            #[cfg(not(any(target_os = "linux", target_os = "android")))]
            Self::PerTestConnection => engine::spawn(
                socket,
                data_phase,
                engine_socket,
                connected,
                buffer_size,
                clock,
                settings,
                report.clone(),
            ),
            #[cfg(any(target_os = "linux", target_os = "android"))]
            Self::Pool(pool) => {
                let _ = (clock, settings, report);
                pool.start(socket, data_phase, engine_socket, connected, buffer_size)
            }
        }
    }
}

/// The IP version of the packet a datagram came in: an IPv4 peer on a
/// dual-stack IPv6 socket (`::ffff:a.b.c.d`) is IPv4 (ADR 0004).
pub(crate) fn ip_version(source: SocketAddr) -> IpVersion {
    IpVersion::of(source)
}

async fn sleep_until(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

fn close(owned: &mut HashMap<SocketId, Owned>, socket: SocketId) {
    if let Some(o) = owned.remove(&socket) {
        o.reader.abort();
        if let Some(engine) = o.engine {
            engine.stop();
        }
    }
}

/// A standard-port listener on a wildcard address. The IPv6 one is
/// IPv6-only, so no IPv4-mapped addresses reach it (ADR 0007).
fn bind_listener(ip: IpAddr, port: u16, options: IpOptions) -> io::Result<Listener> {
    Listener::new(UdpSocket::from_std(bind_std(ip, port, options)?.into())?)
}

/// A non-blocking socket bound to `ip` and `port` that sends with
/// `options`; IPv6-only for IPv6.
pub(crate) fn bind_std(ip: IpAddr, port: u16, options: IpOptions) -> io::Result<Socket> {
    bind_std_at(SocketAddr::new(ip, port), options)
}

/// As [`bind_std`], at `at`, with its IPv6 scope id.
fn bind_std_at(at: SocketAddr, options: IpOptions) -> io::Result<Socket> {
    let ip = at.ip();
    let domain = if ip.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };
    let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
    if ip.is_ipv6() {
        socket.set_only_v6(true)?;
    }
    ip_options::apply(&socket, ip.is_ipv4(), options)?;
    socket.set_nonblocking(true)?;
    socket.bind(&at.into())?;
    Ok(socket)
}

/// A socket for Tokio and a second handle to the same socket for a Data
/// Phase Engine.
pub(crate) fn bind_shared(
    ip: IpAddr,
    port: u16,
    options: IpOptions,
) -> io::Result<(UdpSocket, std::net::UdpSocket)> {
    bind_shared_at(SocketAddr::new(ip, port), options)
}

fn bind_shared_at(
    at: SocketAddr,
    options: IpOptions,
) -> io::Result<(UdpSocket, std::net::UdpSocket)> {
    let socket = bind_std_at(at, options)?;
    let engine = socket.try_clone()?;
    Ok((UdpSocket::from_std(socket.into())?, engine.into()))
}

/// A Test Connection's socket, bound as [`bind_ephemeral_port`] does.
pub(crate) struct TestPort {
    pub(crate) socket: UdpSocket,
    /// The same socket, for the Data Phase Engine.
    pub(crate) engine: std::net::UdpSocket,
    pub(crate) local: SocketAddr,
    /// Windows: a socket on the wildcard address and the same port, held
    /// while the Test Connection lives (see [`bind_ephemeral_port`]).
    pub(crate) guard: Option<Socket>,
}

/// A Test Connection's socket bound to `local`'s address and scope, the
/// address the Setup Request arrived on, on a random free port from
/// `range`: random ports first, then ports in order, so a nearly full range
/// still finds a free one. Windows lets a socket bound to
/// a specific address take over a port another socket holds on the
/// wildcard address, and receive its datagrams, where Linux and macOS
/// refuse the bind. So on Windows the port is first taken on the wildcard
/// address too: that bind fails if another socket has the port, and holding
/// it keeps anyone from taking the port over from us.
pub(crate) fn bind_ephemeral_port(
    local: SocketAddr,
    range: &RangeInclusive<u16>,
    options: IpOptions,
    rng: &mut SplitMix64,
) -> io::Result<TestPort> {
    let (first, last) = (u32::from(*range.start()), u32::from(*range.end()));
    let span = last - first + 1;
    let random = (0..16).map(|_| (first + (rng.next() % u64::from(span)) as u32) as u16);
    let mut error = io::Error::new(
        io::ErrorKind::AddrInUse,
        "the Ephemeral Port range is exhausted",
    );
    let ip = local.ip();
    for port in random.chain(range.clone().take(IN_ORDER_ATTEMPTS)) {
        let guard = if cfg!(windows) && !ip.is_unspecified() {
            let wildcard = if ip.is_ipv4() {
                Ipv4Addr::UNSPECIFIED.into()
            } else {
                Ipv6Addr::UNSPECIFIED.into()
            };
            match bind_std(wildcard, port, options) {
                Ok(guard) => Some(guard),
                Err(e) => {
                    error = e;
                    continue;
                }
            }
        } else {
            None
        };
        let mut at = local;
        at.set_port(port);
        match bind_shared_at(at, options)
            .and_then(|(s, e)| s.local_addr().map(|local| (s, e, local)))
        {
            Ok((socket, engine, local)) => {
                return Ok(TestPort {
                    socket,
                    engine,
                    local,
                    guard,
                });
            }
            Err(e) => error = e,
        }
    }
    Err(error)
}

#[cfg(test)]
mod tests {
    use super::*;

    // : a Test Connection's socket, bound to a specific address, never
    // takes over a port another socket holds on the wildcard address, as
    // Windows would otherwise allow.
    #[tokio::test]
    async fn a_test_port_never_shadows_a_wildcard_socket() {
        let holder = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
        let port = holder.local_addr().unwrap().port();
        let bound = bind_ephemeral_port(
            (Ipv4Addr::LOCALHOST, 0).into(),
            &(port..=port),
            IpOptions::default(),
            &mut SplitMix64(1),
        );
        assert!(bound.is_err(), "port {port} was taken over");
    }

    // , : a Test Connection bound to the arrival address keeps it as
    // its local address once connected to the Client.
    #[tokio::test]
    async fn a_connected_test_port_keeps_the_arrival_address() {
        let arrival = std::net::Ipv4Addr::new(127, 0, 0, 2);
        if std::net::UdpSocket::bind((arrival, 0)).is_err() {
            return; // No second loopback address here (stock macOS).
        }
        let port = bind_ephemeral_port(
            (arrival, 0).into(),
            &(49152..=65535),
            IpOptions::default(),
            &mut SplitMix64(2),
        )
        .unwrap();
        port.socket.connect("127.0.0.1:9").await.unwrap();
        assert_eq!(
            port.socket.local_addr().unwrap().ip(),
            IpAddr::from(arrival)
        );
    }
}
