//! The Standard Port listeners: each Setup Request's arrival address, and
//! replies sent from it (RFC 9946 §6.2.2; ADR 0007). A listener is bound to
//! a wildcard address, so without this the OS picks a reply's source, and
//! on a host with several addresses the Client may drop a Setup Response
//! from another one. Linux and macOS read the address and set the source
//! with IP_PKTINFO / IPV6_PKTINFO through `nix`; Windows through
//! `quinn-udp`, on the listener only. Elsewhere the listeners read and send
//! plainly, and the core drops what arrives without an address.

use std::io;
use std::net::{IpAddr, SocketAddr};

use tokio::io::Interest;
use tokio::net::UdpSocket;

/// Where a datagram arrived: the local address it was sent to, and the
/// interface it came in on.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Arrival {
    pub(crate) ip: IpAddr,
    pub(crate) interface: u32,
}

impl Arrival {
    /// The address with `port`, and the interface as its scope when it is
    /// IPv6 link-local, the only kind that needs one.
    pub(crate) fn at(self, port: u16) -> SocketAddr {
        match self.ip {
            IpAddr::V6(ip) if ip.is_unicast_link_local() => {
                std::net::SocketAddrV6::new(ip, port, 0, self.interface).into()
            }
            ip => SocketAddr::new(ip, port),
        }
    }
}

/// A listener that knows where each datagram arrived.
pub(crate) struct Listener {
    socket: UdpSocket,
    /// Windows: `quinn-udp`'s state, for a Standard Port listener only.
    #[cfg(windows)]
    state: Option<quinn_udp::UdpSocketState>,
}

impl Listener {
    /// Turns on arrival addresses for `socket`, IPv4 or IPv6-only.
    pub(crate) fn new(socket: UdpSocket) -> io::Result<Self> {
        #[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
        unix::enable(&socket)?;
        Ok(Self {
            #[cfg(windows)]
            state: Some(quinn_udp::UdpSocketState::new((&socket).into())?),
            socket,
        })
    }

    /// Arrival addresses for a Client's Test Connection socket,
    /// whose Data Phase follows on the same socket: where `nix` reads
    /// them. On Windows none, since `quinn-udp`, which would set DF and
    /// overwrite the DSCP, is for the Standard Port listener only (ADR
    /// 0007).
    pub(crate) fn for_client(socket: UdpSocket) -> io::Result<Self> {
        #[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
        unix::enable(&socket)?;
        Ok(Self {
            #[cfg(windows)]
            state: None,
            socket,
        })
    }

    /// As [`Listener::recv_from`], polled.
    pub(crate) fn poll_recv_from(
        &self,
        cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> std::task::Poll<io::Result<(usize, SocketAddr, Option<Arrival>)>> {
        loop {
            if let Err(error) = std::task::ready!(self.socket.poll_recv_ready(cx)) {
                return std::task::Poll::Ready(Err(error));
            }
            match self
                .socket
                .try_io(Interest::READABLE, || self.try_recv(buf))
            {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
                result => return std::task::Poll::Ready(result),
            }
        }
    }

    pub(crate) fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    /// The same socket, out of Tokio's reactor: for a Client's Test
    /// Connection whose Data Phase has started, which the Control Phase
    /// keeps open but no longer reads.
    pub(crate) fn into_std(self) -> io::Result<std::net::UdpSocket> {
        self.socket.into_std()
    }

    /// A datagram: its length, its sender, and the local address it was
    /// sent to, where the OS says.
    pub(crate) async fn recv_from(
        &self,
        buf: &mut [u8],
    ) -> io::Result<(usize, SocketAddr, Option<Arrival>)> {
        loop {
            self.socket.readable().await?;
            match self
                .socket
                .try_io(Interest::READABLE, || self.try_recv(buf))
            {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
                result => return result,
            }
        }
    }

    /// Sends `payload` to `destination`, from `source` when given and not
    /// a wildcard address.
    pub(crate) async fn send_to(
        &self,
        payload: &[u8],
        destination: SocketAddr,
        source: Option<IpAddr>,
    ) -> io::Result<()> {
        let source = source.filter(|ip| !ip.is_unspecified());
        loop {
            self.socket.writable().await?;
            match self.socket.try_io(Interest::WRITABLE, || {
                self.try_send(payload, destination, source)
            }) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
                result => return result,
            }
        }
    }

    /// One polled Control Phase send. `before_send` is the caller's private
    /// fault seam and runs only when a write is actually attempted.
    pub(crate) fn poll_send_to(
        &self,
        cx: &mut std::task::Context<'_>,
        payload: &[u8],
        destination: SocketAddr,
        source: Option<IpAddr>,
        mut before_send: impl FnMut() -> io::Result<()>,
    ) -> std::task::Poll<io::Result<()>> {
        let source = source.filter(|ip| !ip.is_unspecified());
        loop {
            if let Err(error) = std::task::ready!(self.socket.poll_send_ready(cx)) {
                return std::task::Poll::Ready(Err(error));
            }
            match self.socket.try_io(Interest::WRITABLE, || {
                before_send()?;
                self.try_send(payload, destination, source)
            }) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
                result => return std::task::Poll::Ready(result),
            }
        }
    }

    #[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
    fn try_recv(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr, Option<Arrival>)> {
        unix::recv(&self.socket, buf)
    }

    #[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
    fn try_send(
        &self,
        payload: &[u8],
        destination: SocketAddr,
        source: Option<IpAddr>,
    ) -> io::Result<()> {
        unix::send(&self.socket, payload, destination, source)
    }

    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_vendor = "apple",
        windows
    )))]
    fn try_recv(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr, Option<Arrival>)> {
        let (len, source) = self.socket.try_recv_from(buf)?;
        Ok((len, source, None))
    }

    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_vendor = "apple",
        windows
    )))]
    fn try_send(
        &self,
        payload: &[u8],
        destination: SocketAddr,
        _: Option<IpAddr>,
    ) -> io::Result<()> {
        self.socket.try_send_to(payload, destination).map(drop)
    }

    #[cfg(windows)]
    fn try_recv(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr, Option<Arrival>)> {
        let Some(state) = &self.state else {
            let (len, source) = self.socket.try_recv_from(buf)?;
            return Ok((len, source, None));
        };
        let mut meta = [quinn_udp::RecvMeta::default()];
        let mut bufs = [io::IoSliceMut::new(buf)];
        state.recv((&self.socket).into(), &mut bufs, &mut meta)?;
        let arrival = meta[0].dst_ip.map(|ip| Arrival {
            ip,
            interface: meta[0].interface_index.unwrap_or(0),
        });
        Ok((meta[0].len, meta[0].addr, arrival))
    }

    #[cfg(windows)]
    fn try_send(
        &self,
        payload: &[u8],
        destination: SocketAddr,
        source: Option<IpAddr>,
    ) -> io::Result<()> {
        let Some(state) = &self.state else {
            return self.socket.try_send_to(payload, destination).map(drop);
        };
        let transmit = quinn_udp::Transmit {
            destination,
            ecn: None,
            contents: payload,
            segment_size: None,
            src_ip: source,
        };
        state.try_send((&self.socket).into(), &transmit)
    }
}

#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
mod unix {
    use std::io::{self, IoSlice, IoSliceMut};
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
    use std::os::fd::AsRawFd;

    use nix::libc;
    use nix::sys::socket::{
        ControlMessage, ControlMessageOwned, MsgFlags, SockaddrIn, SockaddrIn6, SockaddrStorage,
        recvmsg, sendmsg, setsockopt, sockopt,
    };
    use tokio::net::UdpSocket;

    use super::Arrival;

    pub(super) fn enable(socket: &UdpSocket) -> io::Result<()> {
        if socket.local_addr()?.is_ipv4() {
            setsockopt(socket, sockopt::Ipv4PacketInfo, &true)?;
        } else {
            setsockopt(socket, sockopt::Ipv6RecvPacketInfo, &true)?;
        }
        Ok(())
    }

    pub(super) fn recv(
        socket: &UdpSocket,
        buf: &mut [u8],
    ) -> io::Result<(usize, SocketAddr, Option<Arrival>)> {
        let mut iov = [IoSliceMut::new(buf)];
        let mut cmsg = nix::cmsg_space!(libc::in6_pktinfo);
        let message = recvmsg::<SockaddrStorage>(
            socket.as_raw_fd(),
            &mut iov,
            Some(&mut cmsg),
            MsgFlags::empty(),
        )?;
        let source = message
            .address
            .and_then(|a| {
                a.as_sockaddr_in()
                    .map(|v4| SocketAddr::from(std::net::SocketAddrV4::from(*v4)))
                    .or_else(|| {
                        a.as_sockaddr_in6()
                            .map(|v6| SocketAddr::from(std::net::SocketAddrV6::from(*v6)))
                    })
            })
            .ok_or_else(|| io::Error::other("a datagram without a sender"))?;
        let mut local = None;
        for control in message.cmsgs()? {
            match control {
                // ipi_ifindex is a c_int on Linux and a u32 on macOS.
                #[allow(clippy::useless_conversion)]
                ControlMessageOwned::Ipv4PacketInfo(info) => {
                    local = Some(Arrival {
                        ip: Ipv4Addr::from(u32::from_be(info.ipi_addr.s_addr)).into(),
                        interface: u32::try_from(info.ipi_ifindex).unwrap_or(0),
                    });
                }
                ControlMessageOwned::Ipv6PacketInfo(info) => {
                    local = Some(Arrival {
                        ip: Ipv6Addr::from(info.ipi6_addr.s6_addr).into(),
                        interface: info.ipi6_ifindex,
                    });
                }
                _ => {}
            }
        }
        Ok((message.bytes, source, local))
    }

    pub(super) fn send(
        socket: &UdpSocket,
        payload: &[u8],
        destination: SocketAddr,
        source: Option<IpAddr>,
    ) -> io::Result<()> {
        let iov = [IoSlice::new(payload)];
        let fd = socket.as_raw_fd();
        match (destination, source) {
            (SocketAddr::V4(to), Some(IpAddr::V4(from))) => {
                let info = libc::in_pktinfo {
                    ipi_ifindex: 0,
                    ipi_spec_dst: libc::in_addr {
                        s_addr: u32::from(from).to_be(),
                    },
                    ipi_addr: libc::in_addr { s_addr: 0 },
                };
                let cmsgs = [ControlMessage::Ipv4PacketInfo(&info)];
                sendmsg(
                    fd,
                    &iov,
                    &cmsgs,
                    MsgFlags::empty(),
                    Some(&SockaddrIn::from(to)),
                )?;
            }
            (SocketAddr::V6(to), Some(IpAddr::V6(from))) => {
                let info = libc::in6_pktinfo {
                    ipi6_addr: libc::in6_addr {
                        s6_addr: from.octets(),
                    },
                    // A link-local destination's interface.
                    ipi6_ifindex: to.scope_id(),
                };
                let cmsgs = [ControlMessage::Ipv6PacketInfo(&info)];
                sendmsg(
                    fd,
                    &iov,
                    &cmsgs,
                    MsgFlags::empty(),
                    Some(&SockaddrIn6::from(to)),
                )?;
            }
            (to, _) => {
                let to = SockaddrStorage::from(to);
                sendmsg(fd, &iov, &[], MsgFlags::empty(), Some(&to))?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    // : on every OS that reports arrival addresses, a datagram's arrival
    // address is the address it was sent to, and a reply can leave from it.
    #[tokio::test]
    async fn a_listener_reports_the_arrival_address_and_replies_from_it() {
        if !cfg!(any(
            target_os = "linux",
            target_os = "android",
            target_vendor = "apple",
            windows
        )) {
            return;
        }
        for (wildcard, loopback) in [
            (
                IpAddr::from(Ipv4Addr::UNSPECIFIED),
                IpAddr::from(Ipv4Addr::LOCALHOST),
            ),
            (
                IpAddr::from(Ipv6Addr::UNSPECIFIED),
                IpAddr::from(Ipv6Addr::LOCALHOST),
            ),
        ] {
            let Ok(bound) = crate::server::bind_std(wildcard, 0, crate::IpOptions::default())
            else {
                continue; // A host without IPv6.
            };
            let listener = Listener::new(UdpSocket::from_std(bound.into()).unwrap()).unwrap();
            let port = listener.local_addr().unwrap().port();
            let peer = std::net::UdpSocket::bind((loopback, 0)).unwrap();
            peer.set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .unwrap();
            peer.send_to(b"setup", (loopback, port)).unwrap();
            let mut buf = [0u8; 16];
            let (len, from, arrival) = listener.recv_from(&mut buf).await.unwrap();
            assert_eq!(&buf[..len], b"setup");
            assert_eq!(arrival.map(|a| a.ip), Some(loopback), "{loopback}");

            listener
                .send_to(b"response", from, Some(loopback))
                .await
                .unwrap();
            let (len, replier) = peer.recv_from(&mut buf).unwrap();
            assert_eq!(&buf[..len], b"response");
            assert_eq!(replier, SocketAddr::new(loopback, port));
        }
    }

    // : a Client's Test Connection socket reports where each datagram
    // arrived where nix reads it (Linux, macOS); on Windows it reads
    // plainly, since quinn-udp is for the Standard Port listener only (ADR
    // 0007).
    #[tokio::test]
    async fn a_client_socket_reports_the_arrival_address_where_nix_can() {
        // The plain-read fallback needs no wildcard socket. Keep it
        // where the test proves that ancillary data supplies the address.
        let reports_arrival = cfg!(any(
            target_os = "linux",
            target_os = "android",
            target_vendor = "apple"
        ));
        let address = if reports_arrival {
            Ipv4Addr::UNSPECIFIED
        } else {
            Ipv4Addr::LOCALHOST
        };
        let bound = crate::server::bind_std(address.into(), 0, crate::IpOptions::default())
            .expect("an IPv4 socket");
        let socket = UdpSocket::from_std(bound.into()).unwrap();
        let port = socket.local_addr().unwrap().port();
        let client = Listener::for_client(socket).unwrap();
        let sender = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        sender
            .send_to(b"a Setup Response", ("127.0.0.1", port))
            .unwrap();
        let mut buf = [0u8; 64];
        let (len, _, arrival) = client.recv_from(&mut buf).await.unwrap();
        assert_eq!(&buf[..len], b"a Setup Response");
        let expected = reports_arrival.then_some(IpAddr::from(Ipv4Addr::LOCALHOST));
        assert_eq!(arrival.map(|a| a.ip), expected);
    }
}
