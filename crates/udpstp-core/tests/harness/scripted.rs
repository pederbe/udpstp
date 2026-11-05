//! Scripted peers: hand-written stand-ins for the Client or the Server whose
//! PDUs a test builds byte by byte from the RFC text, so a check doesn't rely
//! on our own Client or Server getting the other side right.

use std::collections::VecDeque;
use std::net::SocketAddr;

use udpstp_core::{DataPhase, DatagramMeta, Endpoint, MonoTime, Now, Output, SocketId, Transmit};

/// The scripted host's wall clock, in whole seconds since the Unix epoch.
#[derive(Debug, Clone, Copy)]
pub struct ScriptClock {
    pub unix_secs: u64,
    /// The wall clock in ns since the Unix epoch.
    pub unix_nanos: u128,
}

impl ScriptClock {
    fn at(now: Now) -> Self {
        Self {
            unix_secs: now.wall.as_unix().as_secs(),
            unix_nanos: now.wall.as_unix().as_nanos(),
        }
    }
}

/// Builds the one datagram a scripted Client sends to the Server at the start.
pub type ClientScript = Box<dyn FnOnce(ScriptClock) -> Vec<u8>>;

/// Answers each datagram a scripted Client receives with a datagram to the
/// given port on the Server's address (or stays silent).
pub type ClientReplyScript = Box<dyn FnMut(ScriptClock, &[u8]) -> Option<(u16, Vec<u8>)>>;

/// Answers each datagram a scripted Server receives (or stays silent).
pub type ServerScript = Box<dyn FnMut(ScriptClock, &[u8]) -> Vec<Vec<u8>>>;

pub struct ScriptedClient {
    server: SocketAddr,
    replies: Option<ClientReplyScript>,
    outputs: VecDeque<Output>,
}

impl ScriptedClient {
    pub fn new(
        server: SocketAddr,
        script: ClientScript,
        replies: Option<ClientReplyScript>,
        now: Now,
    ) -> Self {
        let payload = script(ScriptClock::at(now));
        Self {
            server,
            replies,
            outputs: VecDeque::from([Output::Transmit(Transmit {
                socket: SocketId::PRIMARY,
                destination: server,
                payload,
                source: None,
            })]),
        }
    }
}

pub struct ScriptedServer {
    script: ServerScript,
    outputs: VecDeque<Output>,
}

impl ScriptedServer {
    pub fn new(script: ServerScript) -> Self {
        Self {
            script,
            outputs: VecDeque::new(),
        }
    }
}

impl Endpoint for ScriptedClient {
    fn handle_datagram(&mut self, now: Now, _meta: DatagramMeta, payload: &[u8]) {
        let Some(replies) = &mut self.replies else {
            return;
        };
        if let Some((port, reply)) = replies(ScriptClock::at(now), payload) {
            self.outputs.push_back(Output::Transmit(Transmit {
                socket: SocketId::PRIMARY,
                destination: SocketAddr::new(self.server.ip(), port),
                payload: reply,
                source: None,
            }));
        }
    }
    fn handle_socket_opened(&mut self, _now: Now, _socket: SocketId, _local: SocketAddr) {}
    fn handle_socket_closed(&mut self, _now: Now, _socket: SocketId) {}
    fn handle_timeout(&mut self, _now: Now) {}
    fn poll_timeout(&self) -> Option<MonoTime> {
        None
    }
    fn poll_output(&mut self) -> Option<Output> {
        self.outputs.pop_front()
    }
    fn take_data_phase(&mut self, _socket: SocketId) -> Option<DataPhase> {
        None
    }
}

impl Endpoint for ScriptedServer {
    fn handle_datagram(&mut self, now: Now, meta: DatagramMeta, payload: &[u8]) {
        for reply in (self.script)(ScriptClock::at(now), payload) {
            self.outputs.push_back(Output::Transmit(Transmit {
                socket: meta.socket,
                destination: meta.source,
                payload: reply,
                source: None,
            }));
        }
    }
    fn handle_socket_opened(&mut self, _now: Now, _socket: SocketId, _local: SocketAddr) {}
    fn handle_socket_closed(&mut self, _now: Now, _socket: SocketId) {}
    fn handle_timeout(&mut self, _now: Now) {}
    fn poll_timeout(&self) -> Option<MonoTime> {
        None
    }
    fn poll_output(&mut self) -> Option<Output> {
        self.outputs.pop_front()
    }
    fn take_data_phase(&mut self, _socket: SocketId) -> Option<DataPhase> {
        None
    }
}
