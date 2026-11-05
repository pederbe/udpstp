//! The portable adapter of a Test Connection's socket: std's socket, one
//! datagram per call, timed by the read. Windows and macOS use it; the
//! Linux adapter makes its own single sends and reads with it.

use std::io;
use std::net::{SocketAddr, UdpSocket};

use super::Settings;

pub(crate) struct Portable {
    socket: UdpSocket,
    peer: SocketAddr,
    /// Whether the socket is connected to the peer, so it sends without an
    /// address (macOS refuses one: EISCONN).
    connected: bool,
}

impl Portable {
    pub(crate) fn open(socket: UdpSocket, settings: &Settings) -> Self {
        Self {
            socket,
            peer: settings.peer,
            connected: settings.connected,
        }
    }

    /// Sends one datagram to the peer.
    pub(crate) fn send(&mut self, datagram: &[u8]) -> io::Result<usize> {
        if self.connected {
            self.socket.send(datagram)
        } else {
            self.socket.send_to(datagram, self.peer)
        }
    }

    /// Reads one datagram into `buf`: its length and its source.
    pub(crate) fn recv(&mut self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        self.socket.recv_from(buf)
    }

    /// The socket, for the Linux adapter's own calls.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub(crate) fn socket(&self) -> &UdpSocket {
        &self.socket
    }
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
impl super::Adapter for Portable {
    fn send(
        &mut self,
        datagram: &[u8],
        _stamped: Option<udpstp_core::MonoTime>,
    ) -> io::Result<usize> {
        Portable::send(self, datagram)
    }

    fn recv(&mut self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        Portable::recv(self, buf)
    }
}
