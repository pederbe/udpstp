//! Receiving datagrams in the Server's Control Phase: only local errors
//! stop a socket's reader, as the classifier reads them (ADR 0006,
//! `socket::classify`); every other error is reported and reading goes on.
//! The Data Phase Engine and the Client's Control Phase report ICMP-type
//! errors to the core; the Server's Control Phase readers only log
//! them.

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::protocol::SocketId;

use crate::runtime::arrival::Arrival;
use crate::runtime::socket::classify::{Action, Call, classify};

/// A read buffer as large as any UDP payload, so no datagram is truncated
/// (and none makes Windows fail the read with WSAEMSGSIZE).
pub(crate) const MAX_DATAGRAM: usize = 65_536;

/// How many received datagrams may wait for the core. Beyond this they are
/// dropped and counted, as a full kernel receive buffer would drop them.
pub(crate) const QUEUE: usize = 1024;

/// Something a socket's reader passes to the loop that owns the core.
pub(crate) enum FromReader {
    Datagram {
        socket: SocketId,
        source: SocketAddr,
        local: SocketAddr,
        payload: Vec<u8>,
    },
    /// Datagrams dropped because the queue was full.
    Dropped { socket: SocketId, count: u64 },
    /// A receive error; reading goes on.
    Error { socket: SocketId, error: io::Error },
    /// A local error ended the reader.
    Stopped { socket: SocketId, error: io::Error },
}

/// A socket a reader task can read: with the local address each datagram
/// was sent to, where it knows it.
pub(crate) trait Receive: Send + Sync + 'static {
    fn local_addr(&self) -> io::Result<SocketAddr>;

    fn receive(
        &self,
        buf: &mut [u8],
    ) -> impl Future<Output = io::Result<(usize, SocketAddr, Option<Arrival>)>> + Send;
}

impl Receive for UdpSocket {
    fn local_addr(&self) -> io::Result<SocketAddr> {
        UdpSocket::local_addr(self)
    }

    async fn receive(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr, Option<Arrival>)> {
        let (len, source) = self.recv_from(buf).await?;
        Ok((len, source, None))
    }
}

impl Receive for crate::runtime::arrival::Listener {
    fn local_addr(&self) -> io::Result<SocketAddr> {
        crate::runtime::arrival::Listener::local_addr(self)
    }

    async fn receive(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr, Option<Arrival>)> {
        self.recv_from(buf).await
    }
}

/// Reads `socket` until a local error, passing what it reads to `sender`.
pub(crate) fn spawn_reader<R: Receive>(
    socket_id: SocketId,
    socket: Arc<R>,
    sender: mpsc::Sender<FromReader>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let local = match socket.local_addr() {
            Ok(local) => local,
            Err(error) => {
                let _ = sender
                    .send(FromReader::Stopped {
                        socket: socket_id,
                        error,
                    })
                    .await;
                return;
            }
        };
        let mut buf = vec![0u8; MAX_DATAGRAM];
        let mut dropped = 0u64;
        loop {
            let message = match socket.receive(&mut buf).await {
                // The address it was sent to, where the socket is bound to
                // a wildcard one.
                Ok((len, source, arrived)) => FromReader::Datagram {
                    socket: socket_id,
                    source,
                    local: arrived.map_or(local, |a| a.at(local.port())),
                    payload: buf[..len].to_vec(),
                },
                Err(error) if classify(Call::ControlRead, &error) == Action::Fatal => {
                    let _ = sender
                        .send(FromReader::Stopped {
                            socket: socket_id,
                            error,
                        })
                        .await;
                    return;
                }
                Err(error) => FromReader::Error {
                    socket: socket_id,
                    error,
                },
            };
            match sender.try_send(message) {
                Ok(()) if dropped > 0 => {
                    let report = FromReader::Dropped {
                        socket: socket_id,
                        count: dropped,
                    };
                    if sender.try_send(report).is_ok() {
                        dropped = 0;
                    }
                }
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => dropped += 1,
                Err(mpsc::error::TrySendError::Closed(_)) => return,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // : a connected socket takes datagrams from its peer only, and the
    // ICMP error a closed peer port sends back (WSAECONNRESET on Windows,
    // ECONNREFUSED elsewhere) doesn't stop it.
    #[test]
    fn a_connected_socket_hears_only_its_peer_and_survives_icmp_errors() {
        use std::net::UdpSocket;
        use std::time::Duration;
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let peer = UdpSocket::bind("127.0.0.1:0").unwrap();
        let stranger = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.connect(peer.local_addr().unwrap()).unwrap();
        let here = socket.local_addr().unwrap();
        stranger.send_to(b"stranger", here).unwrap();
        peer.send_to(b"peer", here).unwrap();
        let mut buf = [0u8; 16];
        let (len, from) = socket.recv_from(&mut buf).unwrap();
        assert_eq!(
            (&buf[..len], from),
            (&b"peer"[..], peer.local_addr().unwrap())
        );

        // The peer goes away; sending to it draws an ICMP port unreachable,
        // which the next read reports (or not at all, if it was lost).
        let peer_addr = peer.local_addr().unwrap();
        drop(peer);
        socket.send(b"hello").unwrap();
        socket
            .set_read_timeout(Some(Duration::from_millis(300)))
            .unwrap();
        if let Err(error) = socket.recv_from(&mut buf) {
            assert_ne!(classify(Call::Read, &error), Action::Fatal, "{error:?}");
            assert_ne!(classify(Call::ControlRead, &error), Action::Fatal);
        }
        // The socket still works: a new peer on the same port is heard.
        let Ok(back) = UdpSocket::bind(peer_addr) else {
            return; // The port was taken meanwhile.
        };
        back.send_to(b"again", here).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let (len, _) = socket.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..len], b"again");
    }
}
