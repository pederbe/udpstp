//! What crosses the boundary between the core and its runtime.

use std::net::{IpAddr, SocketAddr};

use crate::protocol::Event;

/// A handle to one of an endpoint's sockets. [`SocketId::PRIMARY`] is the
/// socket the runtime opens before the endpoint starts: the Server's
/// Standard Port listeners. Every other socket is a Test Connection's
/// socket, opened on the core's request; the Multi-Connection Coordinator
/// numbers its own by mcIndex ([`crate::protocol::Coordinator::socket_of`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SocketId(pub u32);

impl SocketId {
    pub const PRIMARY: SocketId = SocketId(0);
}

/// The IP version of a received packet (not of the socket it arrived on).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IpVersion {
    V4,
    V6,
}

impl IpVersion {
    /// The IP version of packets to or from `addr`: an IPv4-mapped IPv6
    /// address (`::ffff:a.b.c.d`) is IPv4 (ADR 0004).
    pub fn of(addr: std::net::SocketAddr) -> Self {
        match addr.ip() {
            std::net::IpAddr::V4(_) => Self::V4,
            std::net::IpAddr::V6(v6) if v6.to_ipv4_mapped().is_some() => Self::V4,
            std::net::IpAddr::V6(_) => Self::V6,
        }
    }
}

/// Facts about a received datagram that only the runtime can know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatagramMeta {
    /// The socket it arrived on.
    pub socket: SocketId,
    /// The sender's address.
    pub source: SocketAddr,
    /// The local address and port it was sent to.
    pub local: SocketAddr,
    /// The IP version of the packet itself.
    pub ip_version: IpVersion,
    /// The received TOS / traffic class byte, where the runtime asked the
    /// OS for it and got it; `None` otherwise, never a guessed 0 (spec
    /// ; ADR 0007: Windows and macOS can't always supply it). The
    /// runtimes don't ask on Control Phase sockets, where the core has no
    /// use for it: Control PDUs carry no DSCP. A Data Phase's datagrams
    /// carry theirs in [`crate::protocol::RxDatagram::tos`].
    pub tos: Option<u8>,
    /// The datagram's full length, even if the payload was read truncated.
    pub actual_len: usize,
}

/// A datagram to send from one of the endpoint's sockets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transmit {
    pub socket: SocketId,
    pub destination: SocketAddr,
    pub payload: Vec<u8>,
    /// The local address to send from, for a socket bound to a wildcard
    /// address: the one the request being answered arrived on (RFC 9946
    /// §6.2.2; ADR 0007). `None`: the socket's own.
    pub source: Option<IpAddr>,
}

/// Something the runtime must act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    /// Send a datagram.
    Transmit(Transmit),
    /// Open a new socket bound to `local_ip` on a port of the runtime's
    /// choosing, then report it with `Endpoint::handle_socket_opened`.
    /// `scope_id` is the interface of an IPv6 link-local `local_ip`, 0
    /// otherwise.
    OpenSocket {
        socket: SocketId,
        local_ip: IpAddr,
        scope_id: u32,
    },
    /// Close a socket, then report it with `Endpoint::handle_socket_closed`.
    CloseSocket(SocketId),
    /// Connect a socket to `peer`, before sending anything that follows:
    /// from now on the OS delivers only its datagrams, and reports ICMP
    /// errors and Path MTU events about it (docs/spec.md; ADR 0006). Datagrams
    /// already queued from others may still arrive: the core checks the
    /// source itself.
    ConnectSocket { socket: SocketId, peer: SocketAddr },
    /// The Test Connection on `socket` enters its Data Phase: take it with
    /// `Endpoint::take_data_phase` and route the socket's datagrams from
    /// `peer` to it from now on; drop datagrams from any other address.
    StartDataPhase { socket: SocketId, peer: SocketAddr },
    /// Stop the Data Phase on `socket`, started with `StartDataPhase`: call
    /// its `DataPhase::stop`. The Client's Multi-Connection Coordinator
    /// stops a Test it aborts this way.
    StopDataPhase(SocketId),
    /// Something happened that the runtime should log or show.
    Event(Event),
    /// Client: something happened to the Test Connection on `socket`, one
    /// of a Test's several.
    TestConnectionEvent { socket: SocketId, event: Event },
}

/// Whether `ip` is a multicast or broadcast address, which Setup traffic
/// must not use (RFC 9946 §6): IPv4 multicast (224.0.0.0/4), the limited
/// broadcast 255.255.255.255, and IPv6 multicast (ff00::/8), in an
/// IPv4-mapped IPv6 address too. A subnet's directed broadcast address
/// can't be told without its prefix length, which the core doesn't know.
/// Deprecated IPv4-compatible addresses (::a.b.c.d) aren't looked into.
/// (conformance: broadcast-means-limited-broadcast)
pub(crate) fn is_multicast_or_broadcast(ip: std::net::IpAddr) -> bool {
    let v4 = |ip: std::net::Ipv4Addr| ip.is_multicast() || ip.is_broadcast();
    match ip {
        std::net::IpAddr::V4(ip) => v4(ip),
        std::net::IpAddr::V6(ip) => ip.is_multicast() || ip.to_ipv4_mapped().is_some_and(v4),
    }
}
