//! Batched receiving on Linux: a Test Connection's socket reads many
//! datagrams in one system call with recvmmsg, and the kernel reports with
//! each one its receive time and its TOS / Traffic Class. The socket module
//! drives it and handles the errors; this module makes the calls and reads
//! what the kernel reported.
//!
//! **Header-only reads.** A Load Receiver reads only each datagram's first
//! [`HEADER_SLOT`] bytes, the Load PDU header, which is all the core looks
//! at: MSG_TRUNC makes the kernel report the datagram's full length all the
//! same, which the core counts (RFC 9946 §8.1, udpPayload). A Load Sender reads
//! Status PDUs whole, into [`STATUS_SLOT`] bytes.
//!
//! **Kernel timestamps.** SO_TIMESTAMPNS gives each datagram the wall-clock
//! time (CLOCK_REALTIME) the kernel received it. The core converts it into
//! the Test Time Base with the clock pair the socket reads around each
//! call (docs/spec.md Time model). The kernel turns receive timestamping on a
//! moment after the socket asks for it; until then it stamps a datagram as
//! it is read, so every datagram carries one. SO_TIMESTAMPING's software
//! receive timestamp, which the prototype used, is the same time, but
//! leaves those first datagrams without one, which would vary the set of
//! control messages (below). A socket asked for kernel send timestamps
//! (`send_stamps.rs`) reports receive timestamps as SO_TIMESTAMPING's
//! too, so the headers have room for that control message as well.
//!
//! **One set of control messages.** nix's recvmmsg doesn't reset each
//! header's control-buffer length between calls, and the kernel shrinks it
//! to what the last datagram in that slot used (data-phase-prototype.md
//! §9). So every datagram must carry the same control messages: the
//! socket asks for the timestamp and the TOS only, and turns off the
//! arrival addresses the Client's Control Phase asked for. Should a
//! datagram still arrive with more, the kernel truncates them, and nix
//! gives none of a truncated set: the datagram loses its timestamp and
//! TOS, and the headers are built anew, which is the only allocation after
//! the first.
//!
//! **Receive Buffer Overflows.** Where the process can't read its UDP
//! tables, the socket asks the kernel to attach its count of Receive
//! Buffer Overflows to each datagram (SO_RXQ_OVFL, `overflows.rs`),
//! and the reader keeps the highest. The kernel attaches it only once the
//! count isn't 0, so after the first overflow datagrams carry one control
//! message more: those of the first read after it lose their timestamp,
//! TOS and count as above, once as a rule, and the headers built anew have
//! room for it. While the last datagram read lost its count that way, the
//! count is unknown rather than too low.
//!
//! **Addresses.** The kernel writes each datagram's source address, which
//! the socket module checks, as the socket may not be connected.

use std::io::{self, IoSliceMut};
use std::net::{SocketAddr, SocketAddrV4, SocketAddrV6, UdpSocket};
use std::os::fd::AsRawFd;
use std::time::Duration;

use crate::protocol::WallTime;
use nix::libc;
use nix::sys::socket::{
    ControlMessageOwned, MsgFlags, MultiHeaders, SockaddrStorage, recvmmsg, setsockopt, sockopt,
};
use nix::sys::time::TimeSpec;

/// The most datagrams one recvmmsg call reads.
pub(crate) const BATCH: usize = 64;

/// What a Load Receiver reads of each datagram: the Load PDU header.
pub(crate) const HEADER_SLOT: usize = 32;

/// What a Load Sender reads of each datagram: more than a Status PDU, so
/// a longer datagram, which the read truncates, still shows as too long
/// and fails validation.
pub(crate) const STATUS_SLOT: usize = 256;

// A Status PDU is 204 bytes (RFC 9946 §8.2).
const _: () = assert!(STATUS_SLOT > 204);

/// Asks the kernel to report each datagram's receive timestamp, unless
/// `timestamps` is false, and its TOS / Traffic Class, and stops it
/// reporting arrival addresses. Best effort: what the kernel refuses, a
/// datagram arrives without.
pub(crate) fn enable(socket: &UdpSocket, timestamps: bool) {
    if timestamps {
        let _ = setsockopt(socket, sockopt::ReceiveTimestampns, &true);
    }
    // An IPv6 socket hears an IPv4 peer's TOS through the IPv4 option.
    let _ = setsockopt(socket, sockopt::IpRecvTos, &true);
    let _ = setsockopt(socket, sockopt::Ipv4PacketInfo, &false);
    if socket.local_addr().is_ok_and(|a| a.is_ipv6()) {
        let _ = setsockopt(socket, sockopt::Ipv6RecvTClass, &true);
        let _ = setsockopt(socket, sockopt::Ipv6RecvPacketInfo, &false);
    }
}

/// What the kernel reported about one datagram.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Received {
    /// Its full length, however much of it was read.
    pub(crate) len: usize,
    pub(crate) source: Option<SocketAddr>,
    /// When it reached the socket, by the wall clock.
    pub(crate) kernel_wall: Option<WallTime>,
    /// Its TOS / Traffic Class byte.
    pub(crate) tos: Option<u8>,
}

/// The preallocated state of batched receiving on one Engine Thread. The
/// headers aren't `Send`: the Engine Thread builds them.
pub(crate) struct BatchReader {
    headers: MultiHeaders<SockaddrStorage>,
    /// [`BATCH`] slots of `slot` bytes each.
    buf: Box<[u8]>,
    slot: usize,
    received: [Received; BATCH],
    /// The highest count of Receive Buffer Overflows the kernel attached
    /// to a datagram read, where the socket asked for it (SO_RXQ_OVFL).
    overflows: u32,
    /// Whether the last datagram read lost its control messages, and with
    /// them any count it carried.
    truncated: bool,
}

impl BatchReader {
    /// A reader that reads `slot` bytes of each datagram.
    pub(crate) fn new(slot: usize) -> Self {
        Self {
            headers: headers(),
            buf: vec![0u8; BATCH * slot].into_boxed_slice(),
            slot,
            received: [Received::default(); BATCH],
            overflows: 0,
            truncated: false,
        }
    }

    /// One recvmmsg call on the non-blocking `socket` for at most `max`
    /// datagrams: how many arrived, which [`Self::datagram`] then gives.
    pub(crate) fn read(&mut self, socket: &UdpSocket, max: usize) -> io::Result<usize> {
        let max = max.clamp(1, BATCH);
        let mut control_truncated = false;
        let mut count = 0;
        {
            let mut slots = self.buf.chunks_exact_mut(self.slot);
            let mut slices: [[IoSliceMut<'_>; 1]; BATCH] = std::array::from_fn(|_| {
                [IoSliceMut::new(
                    slots.next().expect("the buffer holds a slot per header"),
                )]
            });
            let results = recvmmsg(
                socket.as_raw_fd(),
                &mut self.headers,
                slices[..max].iter_mut(),
                MsgFlags::MSG_TRUNC,
                None,
            )?;
            for (received, message) in self.received.iter_mut().zip(results) {
                *received = Received {
                    len: message.bytes,
                    source: message.address.as_ref().and_then(socket_addr),
                    kernel_wall: None,
                    tos: None,
                };
                match message.cmsgs() {
                    Ok(cmsgs) => {
                        self.truncated = false;
                        for cmsg in cmsgs {
                            match cmsg {
                                ControlMessageOwned::ScmTimestampns(stamp) => {
                                    received.kernel_wall = wall(stamp);
                                }
                                ControlMessageOwned::Ipv4Tos(tos) => received.tos = Some(tos),
                                ControlMessageOwned::Ipv6TClass(class) => {
                                    received.tos = u8::try_from(class).ok();
                                }
                                // The overflows before the kernel queued
                                // it, since the socket opened.
                                ControlMessageOwned::RxqOvfl(count) => {
                                    self.overflows = self.overflows.max(count);
                                }
                                _ => {}
                            }
                        }
                    }
                    Err(_) => {
                        control_truncated = true;
                        self.truncated = true;
                    }
                }
                count += 1;
            }
        }
        if control_truncated {
            self.headers = headers();
        }
        Ok(count)
    }

    /// The `index`th datagram of the last read: the bytes read of it, and
    /// what the kernel reported.
    pub(crate) fn datagram(&self, index: usize) -> (&[u8], Received) {
        let received = self.received[index];
        let at = index * self.slot;
        (&self.buf[at..at + received.len.min(self.slot)], received)
    }

    /// The highest count of Receive Buffer Overflows the kernel attached
    /// to a datagram read so far, where the socket asked for it: the
    /// overflows before it queued the latest of them, since the socket
    /// opened; 0 until one arrives with a count. None while the last
    /// datagram read lost its control messages, as the count it carried
    /// may have been higher.
    pub(crate) fn overflows(&self) -> Option<u32> {
        (!self.truncated).then_some(self.overflows)
    }
}

/// Headers for [`BATCH`] datagrams, with room for the control messages
/// asked for (a timestamp, the TOS or Traffic Class, both for an IPv4 peer
/// of an IPv6 socket, and the count of Receive Buffer Overflows) and for an
/// arrival address queued before the engine turned them off.
fn headers() -> MultiHeaders<SockaddrStorage> {
    MultiHeaders::preallocate(
        BATCH,
        Some(nix::cmsg_space!(
            TimeSpec,
            // scm_timestamping, on a Load Sender's socket asked for kernel
            // send timestamps, which then reports receive
            // timestamps that way too.
            [TimeSpec; 3],
            // SO_RXQ_OVFL, where the socket asked for it.
            u32,
            libc::c_int,
            libc::c_int,
            libc::in6_pktinfo
        )),
    )
}

fn socket_addr(address: &SockaddrStorage) -> Option<SocketAddr> {
    address
        .as_sockaddr_in()
        .map(|v4| SocketAddr::from(SocketAddrV4::from(*v4)))
        .or_else(|| {
            address
                .as_sockaddr_in6()
                .map(|v6| SocketAddr::from(SocketAddrV6::from(*v6)))
        })
}

/// A software timestamp as wall time; none where the kernel left it zero.
fn wall(stamp: TimeSpec) -> Option<WallTime> {
    (stamp.tv_sec() > 0).then(|| WallTime::from_unix(Duration::from(stamp)))
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    use std::time::{Instant, SystemTime};

    use super::*;

    /// A non-blocking receiver on `ip`'s loopback, set up as the engine sets
    /// up its socket, and a sender connected to it; `None` for IPv6 on a
    /// host without it.
    fn pair(ip: IpAddr) -> Option<(UdpSocket, UdpSocket)> {
        let receiver = UdpSocket::bind((ip, 0)).ok()?;
        receiver.set_nonblocking(true).unwrap();
        enable(&receiver, true);
        let sender = UdpSocket::bind((ip, 0)).unwrap();
        sender.connect(receiver.local_addr().unwrap()).unwrap();
        Some((sender, receiver))
    }

    /// Reads `count` datagrams in batches, within 2 s: the bytes read of
    /// each, and what the kernel reported.
    fn read_all(
        reader: &mut BatchReader,
        socket: &UdpSocket,
        count: usize,
    ) -> Vec<(Vec<u8>, Received)> {
        let until = Instant::now() + Duration::from_secs(2);
        let mut out = Vec::new();
        while out.len() < count && Instant::now() < until {
            match reader.read(socket, BATCH) {
                Ok(read) => {
                    assert!(read <= BATCH);
                    for index in 0..read {
                        let (bytes, received) = reader.datagram(index);
                        out.push((bytes.to_vec(), received));
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(error) => panic!("{error}"),
            }
        }
        out
    }

    // , docs/spec.md S2b (header-only recvmmsg over real sockets):
    // datagrams read in batches arrive whole and in order: each one's first
    // bytes, its full length, however much of it was read (RFC 9946 §8.1:
    // receive truncation with accurate statistics), and its sender, over
    // more calls than one.
    #[udpstp_req::req("RFC9946-8.1-L7")]
    #[test]
    fn batched_datagrams_arrive_whole_and_in_order() {
        let (sender, receiver) = pair(Ipv4Addr::LOCALHOST.into()).unwrap();
        let lengths: Vec<usize> = [20, 32, 100, 597, 1222]
            .into_iter()
            .cycle()
            .take(150)
            .collect();
        for (index, &len) in lengths.iter().enumerate() {
            sender.send(&vec![index as u8; len]).unwrap();
        }
        let mut reader = BatchReader::new(HEADER_SLOT);
        let received = read_all(&mut reader, &receiver, lengths.len());
        assert_eq!(received.len(), lengths.len(), "every datagram arrived");
        for (index, ((bytes, received), &len)) in received.iter().zip(&lengths).enumerate() {
            assert_eq!(received.len, len, "datagram {index}: its full length");
            assert_eq!(
                *bytes,
                vec![index as u8; len.min(HEADER_SLOT)],
                "datagram {index}: in order, header-only"
            );
            assert_eq!(received.source, Some(sender.local_addr().unwrap()));
        }
    }

    // : a Load Sender reads Status PDUs whole; a longer datagram shows
    // as longer than one.
    #[test]
    fn a_load_sender_reads_status_pdus_whole() {
        let (sender, receiver) = pair(Ipv4Addr::LOCALHOST.into()).unwrap();
        sender.send(&[0xFE; 204]).unwrap();
        sender.send(&[0xFE; 300]).unwrap();
        let mut reader = BatchReader::new(STATUS_SLOT);
        let received = read_all(&mut reader, &receiver, 2);
        let got: Vec<(usize, usize)> = received.iter().map(|(b, r)| (b.len(), r.len)).collect();
        assert_eq!(got, [(204, 204), (STATUS_SLOT, 300)]);
    }

    // , docs/spec.md Time model: each datagram carries the kernel's receive
    // time, by the wall clock: after it was sent, before it was read.
    #[test]
    fn kernel_timestamps_lie_between_the_send_and_the_read() {
        let (sender, receiver) = pair(Ipv4Addr::LOCALHOST.into()).unwrap();
        // The kernel turns receive timestamping on a moment after the
        // socket asks for it (until then it stamps each datagram as it is
        // read).
        std::thread::sleep(Duration::from_millis(100));
        let wall = || {
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
        };
        let mut reader = BatchReader::new(HEADER_SLOT);
        for round in 0..3 {
            let sent = wall();
            for _ in 0..10 {
                sender.send(&[round; 64]).unwrap();
            }
            // They wait in the socket buffer a while before the read.
            std::thread::sleep(Duration::from_millis(20));
            let received = read_all(&mut reader, &receiver, 10);
            let read = wall();
            assert_eq!(received.len(), 10);
            // The two clocks are one (CLOCK_REALTIME); 1 ms allows for how
            // each rounds.
            let slack = Duration::from_millis(1);
            for (_, received) in received {
                let stamp = received
                    .kernel_wall
                    .expect("the kernel timestamps every datagram")
                    .as_unix();
                assert!(
                    stamp + slack >= sent && stamp <= read + slack,
                    "{stamp:?} not within {sent:?}..{read:?}"
                );
                assert!(
                    read.saturating_sub(stamp) >= Duration::from_millis(19),
                    "the timestamp is the arrival, not the read"
                );
            }
        }
    }

    // , , ADR 0007: a Load PDU sent with the agreed DSCP arrives
    // with it, and ECN Not-ECT, over IPv4 and IPv6.
    #[test]
    fn a_load_pdu_arrives_with_the_agreed_dscp() {
        const EF: u8 = 46 << 2;
        for ip in [
            IpAddr::from(Ipv4Addr::LOCALHOST),
            IpAddr::from(Ipv6Addr::LOCALHOST),
        ] {
            let Some((sender, receiver)) = pair(ip) else {
                assert!(ip.is_ipv6(), "{ip}");
                continue;
            };
            crate::runtime::ip_options::set_ds_field(
                &socket2::SockRef::from(&sender),
                ip.is_ipv4(),
                EF,
            )
            .unwrap();
            let mut load_pdu = vec![0u8; 1222];
            load_pdu[..2].copy_from_slice(&[0xBE, 0xEF]);
            sender.send(&load_pdu).unwrap();
            let mut reader = BatchReader::new(HEADER_SLOT);
            let received = read_all(&mut reader, &receiver, 1);
            let (bytes, received) = &received[0];
            assert!(bytes.starts_with(&[0xBE, 0xEF]), "{ip}");
            assert_eq!(received.tos, Some(EF), "{ip}");
        }
    }

    // : nix's recvmmsg keeps each header's control-buffer length from
    // the last read. A datagram whose control messages outgrow it loses its
    // own timestamp and TOS, and the headers are built anew for the next.
    #[test]
    fn a_datagram_with_truncated_control_messages_loses_only_its_own() {
        let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
        receiver.set_nonblocking(true).unwrap();
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        sender.connect(receiver.local_addr().unwrap()).unwrap();
        let mut reader = BatchReader::new(HEADER_SLOT);
        // No control messages yet: the first header's buffer shrinks to 0.
        sender.send(b"first").unwrap();
        assert_eq!(read_all(&mut reader, &receiver, 1)[0].1.kernel_wall, None);
        enable(&receiver, true);
        sender.send(b"second").unwrap();
        let second = read_all(&mut reader, &receiver, 1)[0].1;
        assert_eq!((second.kernel_wall, second.tos), (None, None));
        sender.send(b"third").unwrap();
        let third = read_all(&mut reader, &receiver, 1)[0].1;
        assert!(
            third.kernel_wall.is_some() && third.tos == Some(0),
            "{third:?}"
        );
    }
}
