//! The socket module's own rules, against a scripted adapter whose calls
//! fail as a test says: which calls the socket makes, in which order, and
//! what it reports, the same on every OS. The expected calls are written
//! out from ADR 0006 and 's rules, not taken from the socket.

use std::collections::VecDeque;
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::time::Duration;

use udpstp_core::{MonoTime, RxBatch, RxPdu, TransportError};

use super::{Adapter, Open, Settings, Sink, Socket};
use crate::clock::Clock;

/// A call the socket made on its adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Made {
    /// A single send of a datagram this long.
    Send(usize),
    /// A sendmmsg call: each message's offset in the send buffer and its
    /// datagrams, and the UDP GSO segment size.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    Batch {
        messages: Vec<(usize, u32)>,
        segment: Option<usize>,
    },
}

/// An adapter whose calls return what the test scripted, in order, and
/// succeed once the script runs out; it records each call.
#[derive(Default)]
struct Scripted {
    sends: VecDeque<io::Result<usize>>,
    /// What sendmmsg calls return, and whether it and UDP GSO are offered.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    batches: VecDeque<io::Result<usize>>,
    #[cfg(any(target_os = "linux", target_os = "android"))]
    sendmmsg: bool,
    #[cfg(any(target_os = "linux", target_os = "android"))]
    gso: bool,
    /// Whether each message carries the number of its kernel send
    /// timestamp, so a batch goes with a sendmsg call per message.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    numbered: bool,
    made: Vec<Made>,
}

impl Open for Scripted {
    type Adapter = Scripted;

    fn open(self, _: UdpSocket, _: &Settings) -> Scripted {
        self
    }
}

impl Adapter for Scripted {
    fn send(&mut self, datagram: &[u8], _: Option<MonoTime>) -> io::Result<usize> {
        self.made.push(Made::Send(datagram.len()));
        self.sends.pop_front().unwrap_or(Ok(datagram.len()))
    }

    fn recv(&mut self, _: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        Err(io::ErrorKind::WouldBlock.into())
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn offers(&self) -> super::Offers {
        super::Offers {
            batches: self.sendmmsg,
            sendmmsg: self.sendmmsg && !self.numbered,
            udp_gso: self.gso,
        }
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn send_batch(
        &mut self,
        _: &[u8],
        messages: &[super::batch::Message],
        segment: Option<usize>,
        _: Option<MonoTime>,
    ) -> io::Result<usize> {
        self.made.push(Made::Batch {
            messages: messages.iter().map(|m| (m.at, m.datagrams)).collect(),
            segment,
        });
        self.batches.pop_front().unwrap_or(Ok(messages.len()))
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn read_batch(&mut self, _: usize) -> io::Result<usize> {
        Err(io::ErrorKind::WouldBlock.into())
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn batch_datagram(&self, _: usize) -> (&[u8], super::batch_read::Received) {
        unreachable!("nothing was read")
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn ppoll(&mut self, _: bool, _: Duration) -> io::Result<super::Woke> {
        Ok(super::Woke::TimedOut)
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn drain_queue(&mut self, _: &mut impl FnMut(TransportError)) -> (bool, bool) {
        (false, false)
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn pending_error(&mut self) -> Option<io::Error> {
        None
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn socket(&self) -> &UdpSocket {
        unreachable!("no Engine Thread watches it")
    }
}

/// A sink that keeps the path errors it hears; nothing is read here.
#[derive(Default)]
struct Heard {
    errors: Vec<TransportError>,
}

impl Sink for Heard {
    fn status_pdu(&mut self, _: RxPdu<'_>) {
        unreachable!("nothing was read")
    }

    fn load_pdus(&mut self, _: RxBatch<'_>) {
        unreachable!("nothing was read")
    }

    fn path_error(&mut self, error: TransportError) {
        self.errors.push(error);
    }
}

fn open(adapter: Scripted) -> Socket<Scripted> {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let settings = Settings {
        peer: socket.local_addr().unwrap(),
        connected: true,
        load_receiver: false,
        rx_timestamps: true,
        send_stamps: false,
        explicit_numbers: true,
        clock: Clock::new(),
    };
    Socket::open(adapter, socket, &settings)
}

/// A Burst of row 25: two 1222-byte Load PDUs and a 597-byte Add-on.
const BURST: [(usize, usize); 3] = [(0, 1222), (1222, 1222), (2444, 597)];

const STAMPED: MonoTime = MonoTime::from_duration(Duration::ZERO);

fn refused() -> io::Error {
    io::ErrorKind::ConnectionRefused.into()
}

// ADR 0006: an ICMP-type error a send reports is about an earlier
// datagram, so it is reported and the send made once more; refused again,
// it is reported again, and the datagram waits for the next turn
#[test]
fn an_icmp_error_on_a_send_is_reported_and_the_send_made_once_more() {
    let mut socket = open(Scripted {
        sends: [Err(refused())].into(),
        ..Scripted::default()
    });
    let mut heard = Heard::default();
    assert!(
        socket.send(&[0; 100], &mut heard).unwrap(),
        "the second send took it"
    );
    assert_eq!(heard.errors, [TransportError::Icmp]);
    assert_eq!(socket.adapter.made, [Made::Send(100), Made::Send(100)]);

    let mut socket = open(Scripted {
        sends: [Err(refused()), Err(refused())].into(),
        ..Scripted::default()
    });
    let mut heard = Heard::default();
    assert!(!socket.send(&[0; 100], &mut heard).unwrap(), "it waits");
    assert_eq!(heard.errors, [TransportError::Icmp, TransportError::Icmp]);
    assert_eq!(socket.adapter.made.len(), 2, "made once more, only once");
}

// ADR 0008: a full send buffer ends a Send Plan's sending, and what didn't
// go is left for the Data Phase to send later as Deferred Datagrams
#[test]
fn a_send_plan_goes_until_the_send_buffer_is_full() {
    let mut socket = open(Scripted {
        sends: [Ok(1222), Err(io::ErrorKind::WouldBlock.into())].into(),
        ..Scripted::default()
    });
    let mut heard = Heard::default();
    let sent = socket
        .send_plan(&BURST[..], &[0; 3041], STAMPED, &mut heard)
        .unwrap();
    assert_eq!(sent, 1, "a prefix of the plan");
    assert_eq!(socket.adapter.made, [Made::Send(1222), Made::Send(1222)]);
    assert!(heard.errors.is_empty());
}

// ADR 0006: a single send too large to leave with DF set counts as sent
// and lost, a Path MTU event, rather than holding back every later Burst
#[test]
fn a_datagram_too_large_for_the_path_counts_as_sent() {
    let too_large = std::net::UdpSocket::bind("127.0.0.1:0")
        .and_then(|s| s.send_to(&vec![0; 70_000], s.local_addr()?))
        .expect_err("larger than any UDP datagram");
    let mut socket = open(Scripted {
        sends: [Err(too_large)].into(),
        ..Scripted::default()
    });
    let mut heard = Heard::default();
    let sent = socket
        .send_plan(&BURST[..], &[0; 3041], STAMPED, &mut heard)
        .unwrap();
    assert_eq!(sent, 3, "the first counts as sent, the rest went");
    assert_eq!(heard.errors, [TransportError::MsgSize]);
    assert_eq!(
        socket.adapter.made,
        [Made::Send(1222), Made::Send(1222), Made::Send(597)]
    );
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod linux {
    use nix::errno::Errno;

    use super::*;

    fn batch(messages: &[(usize, u32)], segment: Option<usize>) -> Made {
        Made::Batch {
            messages: messages.to_vec(),
            segment,
        }
    }

    // ADR 0006 (an ICMP error on a send is reported and the call made once
    // more), for a sendmmsg call
    #[test]
    fn an_icmp_error_on_a_batch_is_reported_and_the_batch_made_once_more() {
        let mut socket = open(Scripted {
            batches: [Err(Errno::ECONNREFUSED.into())].into(),
            sendmmsg: true,
            ..Scripted::default()
        });
        let mut heard = Heard::default();
        let sent = socket
            .send_plan(&BURST[..], &[0; 3041], STAMPED, &mut heard)
            .unwrap();
        assert_eq!(sent, 3);
        assert_eq!(heard.errors, [TransportError::Icmp]);
        let all = [(0, 1), (1222, 1), (2444, 1)];
        assert_eq!(socket.adapter.made, [batch(&all, None), batch(&all, None)]);
    }

    // ADR 0006, : a message too large to leave with DF set, on a call
    // without GSO, counts as sent and lost, and the call goes on with the
    // next message
    #[test]
    fn a_message_too_large_for_the_path_counts_as_sent() {
        let mut socket = open(Scripted {
            batches: [Err(Errno::EMSGSIZE.into())].into(),
            sendmmsg: true,
            ..Scripted::default()
        });
        let mut heard = Heard::default();
        let sent = socket
            .send_plan(&BURST[..], &[0; 3041], STAMPED, &mut heard)
            .unwrap();
        assert_eq!(sent, 3);
        assert_eq!(heard.errors, [TransportError::MsgSize]);
        assert_eq!(
            socket.adapter.made,
            [
                batch(&[(0, 1), (1222, 1), (2444, 1)], None),
                batch(&[(1222, 1), (2444, 1)], None),
            ]
        );
    }

    // : a GSO send the kernel refuses (here EIO, a device without
    // checksum offload) turns GSO off for the Test Connection, and the
    // plan's datagrams not yet sent go without it, from the first of them.
    // Deferred Datagrams and a new Burst make two GSO messages of 1222-byte
    // segments: the kernel takes the first, then refuses GSO.
    #[test]
    fn a_refused_gso_send_goes_on_without_gso_from_the_first_datagram_not_sent() {
        let layout = [
            (0, 1222),
            (1222, 597),
            (1819, 1222),
            (3041, 1222),
            (4263, 597),
        ];
        let mut socket = open(Scripted {
            batches: [Ok(1), Err(Errno::EIO.into())].into(),
            sendmmsg: true,
            gso: true,
            ..Scripted::default()
        });
        let mut heard = Heard::default();
        let sent = socket
            .send_plan(&layout[..], &[0; 4860], STAMPED, &mut heard)
            .unwrap();
        assert_eq!(sent, 5);
        assert!(heard.errors.is_empty());
        assert_eq!(
            socket.adapter.made,
            [
                batch(&[(0, 2), (1819, 3)], Some(1222)),
                batch(&[(1819, 3)], Some(1222)),
                batch(&[(1819, 1), (3041, 1), (4263, 1)], None),
            ]
        );
        // And from then on.
        socket.adapter.made.clear();
        socket
            .send_plan(&BURST[..], &[0; 3041], STAMPED, &mut heard)
            .unwrap();
        assert_eq!(
            socket.adapter.made,
            [batch(&[(0, 1), (1222, 1), (2444, 1)], None)]
        );
        let paths = socket.fast_paths();
        assert!(paths.sendmmsg && paths.udp_gso, "GSO went once: {paths:?}");
    }

    // : where the kernel refuses sendmmsg, the plan's datagrams not yet
    // sent go one at a time, and every later plan's too
    #[test]
    fn a_refused_sendmmsg_sends_one_datagram_at_a_time_from_then_on() {
        let mut socket = open(Scripted {
            batches: [Err(Errno::ENOSYS.into())].into(),
            sendmmsg: true,
            ..Scripted::default()
        });
        let mut heard = Heard::default();
        let sent = socket
            .send_plan(&BURST[..], &[0; 3041], STAMPED, &mut heard)
            .unwrap();
        assert_eq!(sent, 3);
        let singles = [Made::Send(1222), Made::Send(1222), Made::Send(597)];
        assert_eq!(
            socket.adapter.made[..],
            [
                &[batch(&[(0, 1), (1222, 1), (2444, 1)], None)][..],
                &singles[..]
            ]
            .concat()
        );
        socket.adapter.made.clear();
        socket
            .send_plan(&BURST[..], &[0; 3041], STAMPED, &mut heard)
            .unwrap();
        assert_eq!(socket.adapter.made, singles);
        assert!(!socket.fast_paths().sendmmsg, "no sendmmsg call went");
    }

    // : where each message carries the number of its kernel send
    // timestamp, the adapter sends a batch's messages with a sendmsg call
    // each: the Burst still goes as one UDP GSO message, and the socket
    // reports UDP GSO without sendmmsg
    #[test]
    fn a_batch_sent_a_sendmsg_call_per_message_is_not_reported_as_sendmmsg() {
        let mut socket = open(Scripted {
            sendmmsg: true,
            gso: true,
            numbered: true,
            ..Scripted::default()
        });
        let mut heard = Heard::default();
        let sent = socket
            .send_plan(&BURST[..], &[0; 3041], STAMPED, &mut heard)
            .unwrap();
        assert_eq!(sent, 3);
        assert_eq!(socket.adapter.made, [batch(&[(0, 3)], Some(1222))]);
        let paths = socket.fast_paths();
        assert!(paths.udp_gso && !paths.sendmmsg, "{paths:?}");
    }
}
