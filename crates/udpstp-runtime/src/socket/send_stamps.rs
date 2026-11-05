//! Kernel send timestamps, an experimental measurement: with
//! `UDPSTP_KERNEL_TX_TIMESTAMPS=1`, which the binary reads and passes at
//! spawn ([`EngineSettings::send_stamps`](crate::EngineSettings)), on Linux, a Load Sender's socket asks
//! the kernel for a software timestamp of each message it sends
//! (SO_TIMESTAMPING with SOF_TIMESTAMPING_TX_SOFTWARE, OPT_ID and
//! OPT_TSONLY), which comes back on the socket's error queue with the
//! message's number. Each timestamp is matched by its number to the Send
//! Plan it sent and the time it stamped that plan's lpduTime, and the
//! module keeps how much later the kernel sent it: the lag the nightly
//! precision test checks against 100 µs at p99. Both times are
//! taken on the Test Time Base: the kernel's wall-clock timestamp is
//! converted with a clock pair read when it is drained, as a receive
//! timestamp is.
//!
//! **Numbering**. Where the kernel takes it (Linux 6.13 and
//! later), each message carries its number in an SCM_TS_OPT_ID control
//! message: the messages the kernel takes are numbered in order, from half
//! the u32 range away from the kernel's own numbers, and one it refuses
//! leaves its number to the next, so no refused send can shift a later
//! number. nix's sendmmsg shares one set of control messages across
//! a call, so each message then goes with a sendmsg call of its own
//! (socket2's), whose control messages are built here. Elsewhere the kernel
//! numbers the messages itself (OPT_ID alone), and it can number one it
//! then refuses (a UDP GSO send, ENOBUFS from the queue discipline), after
//! which its numbers run ahead of the count: from the first send refused,
//! whole or in part, nothing more is counted. A probe finds, once a
//! process, whether the kernel takes SCM_TS_OPT_ID: a numbered datagram a
//! loopback socket sends itself. `UDPSTP_KERNEL_TX_TIMESTAMPS=kernel-count`
//! leaves the numbering to the kernel on every kernel, so a result can be
//! checked on the send path of kernels before 6.13, with batches in
//! sendmmsg calls. Matching each timestamp by the Load PDU of its message
//! instead, which the kernel queues with the timestamp without
//! OPT_TSONLY, was measured and not chosen.
//!
//! A message is a Load PDU, or a UDP GSO message of several, which the
//! kernel stamps once. The lags are counted in 1 µs buckets up to
//! [`BUCKETS`] µs, allocated once; later ones count in the top bucket and
//! the maximum.

use std::collections::VecDeque;
use std::time::Duration;

use udpstp_core::{MonoTime, Now};

#[cfg(all(test, any(target_os = "linux", target_os = "android")))]
pub(crate) use kernel::kernel_takes_numbers;
#[cfg(any(target_os = "linux", target_os = "android"))]
pub(crate) use kernel::{enable, send_batch_numbered, send_numbered};

/// 1 µs buckets up to 10 ms.
const BUCKETS: usize = 10_000;

/// Send Plans remembered until their timestamps come back.
const PENDING: usize = 4096;

/// The number the first message udpstp numbers carries: half the u32
/// range away from the kernel's own numbers, which start at 0. A kernel
/// that ignored the numbers messages carry would stamp them with its own,
/// which stay about half the range from udpstp's as both count on and
/// wrap, so `stamped` reads them as coming before the oldest Send Plan
/// remembered and matches none, rather than another plan's.
const FIRST_CARRIED: u32 = 0x8000_0000;

/// How much later than its lpduTime the kernel sent each message it
/// stamped: results metadata for the precision test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendLag {
    /// Messages stamped and matched to a Send Plan.
    pub samples: u64,
    pub p50: Duration,
    pub p99: Duration,
    pub max: Duration,
}

/// Who numbers the messages whose kernel send timestamps come back
///: what `UDPSTP_KERNEL_TX_TIMESTAMPS` asks for, `1` or
/// `kernel-count`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Numbering {
    /// udpstp: each message carries its number (SCM_TS_OPT_ID).
    Explicit,
    /// The kernel, as it takes each message (OPT_ID alone).
    Kernel,
}

/// The messages sent and the lags of those stamped.
#[derive(Debug)]
pub(crate) struct SendStamps {
    numbering: Numbering,
    /// The number the next message sent gets.
    next_key: u32,
    /// Send Plans awaiting timestamps: their first message's number, how
    /// many messages, and when their lpduTime was stamped.
    pending: VecDeque<(u32, u32, MonoTime)>,
    lags: Micros,
    /// A send the kernel numbers itself was refused, after which its
    /// numbering may run ahead of this one: no plan sent from then on is
    /// remembered, while those sent before still match their timestamps.
    lost_count: bool,
}

impl SendStamps {
    pub(crate) fn new(numbering: Numbering) -> Self {
        Self {
            numbering,
            next_key: match numbering {
                Numbering::Explicit => FIRST_CARRIED,
                Numbering::Kernel => 0,
            },
            pending: VecDeque::with_capacity(PENDING),
            lags: Micros::allocated(),
            lost_count: false,
        }
    }

    /// The number the next message sent must carry, when udpstp numbers
    /// them ([`Numbering::Explicit`]); the numbers after it follow in
    /// order.
    pub(crate) fn carried(&self) -> Option<u32> {
        (self.numbering == Numbering::Explicit).then_some(self.next_key)
    }

    /// A send call offered the kernel `offered` messages, and it took the
    /// first `taken` of them, a Send Plan's whose lpduTime was stamped at
    /// `stamped`, or something else's (a STOP, a Status PDU) with none.
    /// The messages taken get the next numbers. A message refused leaves
    /// its number to the next when it carried it; when the kernel numbers
    /// them, it may have numbered it anyway, so a later message's number
    /// can't be known: the plans sent before it still match their
    /// timestamps, and no plan sent after it is remembered.
    pub(crate) fn sent(&mut self, offered: usize, taken: usize, stamped: Option<MonoTime>) {
        if self.lost_count {
            return;
        }
        let messages = u32::try_from(taken).unwrap_or(u32::MAX);
        if let (Some(stamped), 1..) = (stamped, messages) {
            if self.pending.len() == PENDING {
                self.pending.pop_front();
            }
            self.pending.push_back((self.next_key, messages, stamped));
        }
        self.next_key = self.next_key.wrapping_add(messages);
        self.lost_count = taken < offered && self.numbering == Numbering::Kernel;
    }

    /// The kernel refused a message the number it carried (EINVAL for
    /// SCM_TS_OPT_ID) and took it without one: the messages sent from now
    /// on carry none, and the kernel's own numbers for them can't be told
    /// from udpstp's, so no more lags are counted, not even of the plans
    /// already sent. The lags counted so far stand.
    pub(crate) fn numbers_refused(&mut self) {
        self.numbering = Numbering::Kernel;
        self.lost_count = true;
        self.pending.clear();
    }

    /// The kernel sent message `key` at `wall`, by its wall clock; `now`
    /// is a clock pair read just now, to take it onto the Test Time Base.
    pub(crate) fn stamped(&mut self, key: u32, wall: Duration, now: Now) {
        // Plans before this message's won't be stamped any more.
        while let Some(&(first, count, stamped)) = self.pending.front() {
            let offset = key.wrapping_sub(first);
            if offset >= u32::MAX / 2 {
                // Before the oldest plan remembered.
                return;
            }
            if offset >= count {
                self.pending.pop_front();
                continue;
            }
            let sent = (wall + now.mono.as_duration()).saturating_sub(now.wall.as_unix());
            self.lags.record(sent.saturating_sub(stamped.as_duration()));
            if offset + 1 == count {
                self.pending.pop_front();
            }
            return;
        }
    }

    /// The lags so far, if any message was stamped.
    pub(crate) fn lag(&self) -> Option<SendLag> {
        self.lags.summary()
    }
}

/// Durations counted in 1 µs buckets up to [`BUCKETS`] µs, the later ones
/// in the top bucket and the maximum: the send lag's, and a Burst's
/// lateness. The buckets are allocated once, at the first count
/// where they are lazy.
#[derive(Debug, Default)]
pub(crate) struct Micros {
    buckets: Vec<u64>,
    samples: u64,
    max: Duration,
}

impl Micros {
    /// With its buckets allocated now.
    pub(crate) fn allocated() -> Self {
        Self {
            buckets: vec![0; BUCKETS],
            ..Self::default()
        }
    }

    /// Counts one duration.
    pub(crate) fn record(&mut self, duration: Duration) {
        if self.buckets.is_empty() {
            self.buckets = vec![0; BUCKETS];
        }
        let bucket =
            usize::try_from(duration.as_micros()).map_or(BUCKETS - 1, |us| us.min(BUCKETS - 1));
        self.buckets[bucket] += 1;
        self.samples += 1;
        self.max = self.max.max(duration);
    }

    /// Their median, 99th percentile and maximum, if any was counted.
    pub(crate) fn summary(&self) -> Option<SendLag> {
        let at = |share: u64| {
            let rank = (self.samples * share).div_ceil(100).max(1);
            let mut seen = 0;
            let bucket = self
                .buckets
                .iter()
                .position(|&n| {
                    seen += n;
                    seen >= rank
                })
                .unwrap_or(BUCKETS - 1);
            Duration::from_micros(bucket as u64).min(self.max)
        };
        (self.samples > 0).then(|| SendLag {
            samples: self.samples,
            p50: at(50),
            p99: at(99),
            max: self.max,
        })
    }
}

/// Asking the kernel for send timestamps, and the numbered sends, on
/// Linux.
#[cfg(any(target_os = "linux", target_os = "android"))]
mod kernel {
    use std::io::{self, IoSlice};
    use std::net::{Ipv4Addr, Ipv6Addr, UdpSocket};
    use std::sync::OnceLock;
    use std::time::Duration;

    use nix::libc::{self, c_int};
    use nix::sys::socket::{TimestampingFlag, setsockopt, sockopt};
    use socket2::{MsgHdr, SockAddr, SockRef};

    use super::Numbering;
    use crate::socket::batch::{self, Message};
    use crate::socket::errqueue::{self, Queued};

    /// SCM_TS_OPT_ID (Linux 6.13): 81 in include/uapi/asm-generic/socket.h
    /// and on MIPS and Alpha, which number their socket options apart.
    #[cfg(not(any(target_arch = "sparc", target_arch = "sparc64")))]
    const SCM_TS_OPT_ID: c_int = 81;
    /// SPARC's (arch/sparc/include/uapi/asm/socket.h). PA-RISC's is 0x404C,
    /// but Rust has no Linux target for it.
    #[cfg(any(target_arch = "sparc", target_arch = "sparc64"))]
    const SCM_TS_OPT_ID: c_int = 0x5a;

    /// Room for a message's control messages: its number and a UDP GSO
    /// segment size, each at most 24 bytes with its header.
    const CONTROL: usize = 64;

    /// Asks the kernel to timestamp each message `socket` sends, numbered,
    /// and says who numbers them: udpstp where `explicit` allows it and the
    /// kernel takes SCM_TS_OPT_ID, else the kernel.
    pub(crate) fn enable(socket: &UdpSocket, explicit: bool) -> io::Result<Numbering> {
        ask(socket)?;
        Ok(if explicit && kernel_takes_numbers() {
            Numbering::Explicit
        } else {
            Numbering::Kernel
        })
    }

    fn ask(socket: &UdpSocket) -> io::Result<()> {
        let flags = TimestampingFlag::SOF_TIMESTAMPING_TX_SOFTWARE
            | TimestampingFlag::SOF_TIMESTAMPING_SOFTWARE
            | TimestampingFlag::SOF_TIMESTAMPING_OPT_ID
            | TimestampingFlag::SOF_TIMESTAMPING_OPT_TSONLY;
        setsockopt(socket, sockopt::Timestamping, &flags)?;
        Ok(())
    }

    /// Whether the kernel takes SCM_TS_OPT_ID, found once a process.
    pub(crate) fn kernel_takes_numbers() -> bool {
        static TAKES: OnceLock<bool> = OnceLock::new();
        *TAKES.get_or_init(|| probe().unwrap_or(false))
    }

    /// A loopback socket sends itself a datagram numbered `PROBE`: a
    /// kernel before 6.13 refuses the control message (EINVAL), and one
    /// that takes it numbers messages so. Its timestamp, which loopback
    /// queues before the send returns, is checked where it comes within
    /// 2 ms, which bounds how long the first Load Sender's engine waits
    /// for it: one with another number means the kernel ignored it. A host
    /// too slow for that still counts as taking the numbers: a kernel that
    /// ignored them would stamp a Test Connection's messages with its own
    /// numbers, half the u32 range from those they carry, which match no
    /// Send Plan (FIRST_CARRIED): no lags would be counted, rather than
    /// wrong ones.
    fn probe() -> io::Result<bool> {
        const PROBE: u32 = 0x5EED_0185;
        let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .or_else(|_| UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)))?;
        socket.connect(socket.local_addr()?)?;
        socket.set_nonblocking(true)?;
        ask(&socket)?;
        if send_numbered(&socket, None, &[0], PROBE, None).is_err() {
            return Ok(false);
        }
        for _ in 0..20 {
            let mut numbered = None;
            errqueue::drain_with(&socket, |queued| {
                if let Queued::Sent { key, .. } = queued {
                    numbered = Some(key);
                }
            });
            if let Some(key) = numbered {
                return Ok(key == PROBE);
            }
            std::thread::sleep(Duration::from_micros(100));
        }
        Ok(true)
    }

    /// Sends `datagram` to `to`, or on the connected socket without an
    /// address, as a message numbered `number` for its send timestamp: a
    /// UDP GSO message of `segment`-byte datagrams when given. The bytes
    /// sent.
    pub(crate) fn send_numbered(
        socket: &UdpSocket,
        to: Option<&SockAddr>,
        datagram: &[u8],
        number: u32,
        segment: Option<usize>,
    ) -> io::Result<usize> {
        let mut control = [0u8; CONTROL];
        let mut len = control_message(
            &mut control,
            0,
            libc::SOL_SOCKET,
            SCM_TS_OPT_ID,
            &number.to_ne_bytes(),
        );
        if let Some(segment) = segment {
            // The segment size fits: every Load PDU is at most 65,535 bytes.
            let size = u16::try_from(segment).unwrap_or(u16::MAX);
            len = control_message(
                &mut control,
                len,
                libc::SOL_UDP,
                libc::UDP_SEGMENT,
                &size.to_ne_bytes(),
            );
        }
        let buffers = [IoSlice::new(datagram)];
        let mut message = MsgHdr::new()
            .with_buffers(&buffers)
            .with_control(&control[..len]);
        if let Some(to) = to {
            message = message.with_addr(to);
        }
        SockRef::from(socket).sendmsg(&message, 0)
    }

    /// Sends `messages` (at most [`batch::BATCH`] of them) from `buf` on
    /// the connected `socket`, each with a sendmsg call of its own and
    /// numbered in order from `first`, with UDP GSO when `segment` is
    /// given: how many of them the kernel took whole, in order. As with
    /// sendmmsg, a message refused after others were taken ends the call
    /// without an error, which the next call meets again.
    pub(crate) fn send_batch_numbered(
        socket: &UdpSocket,
        buf: &[u8],
        messages: &[Message],
        segment: Option<usize>,
        first: u32,
    ) -> io::Result<usize> {
        let messages = &messages[..messages.len().min(batch::BATCH)];
        for (index, message) in messages.iter().enumerate() {
            let datagrams = &buf[message.at..message.at + message.bytes];
            let number = first.wrapping_add(index as u32);
            let sent = send_numbered(socket, None, datagrams, number, segment);
            // A datagram goes whole or not at all.
            if !matches!(sent, Ok(bytes) if bytes == message.bytes) {
                return if index > 0 {
                    Ok(index)
                } else {
                    sent.map(|_| 0)
                };
            }
        }
        Ok(messages.len())
    }

    /// Appends a control message to `buf` at `at`, laid out as the kernel
    /// reads one (struct cmsghdr: a size_t length, the level, the type,
    /// then the data, padded to the next size_t): where the next one goes.
    fn control_message(buf: &mut [u8], at: usize, level: c_int, kind: c_int, data: &[u8]) -> usize {
        const WORD: usize = std::mem::size_of::<usize>();
        const HEADER: usize = std::mem::size_of::<libc::cmsghdr>();
        let space = HEADER + data.len().next_multiple_of(WORD);
        let message = &mut buf[at..at + space];
        message.fill(0);
        message[..WORD].copy_from_slice(&(HEADER + data.len()).to_ne_bytes());
        message[WORD..WORD + 4].copy_from_slice(&level.to_ne_bytes());
        message[WORD + 4..WORD + 8].copy_from_slice(&kind.to_ne_bytes());
        message[HEADER..HEADER + data.len()].copy_from_slice(data);
        at + space
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        // : a control message is laid out as the C library's CMSG_LEN
        // and CMSG_SPACE lay it out (here nix's cmsg_space!, which calls
        // CMSG_SPACE): a number in 4 bytes, a segment size in 2
        #[test]
        fn control_messages_are_laid_out_as_cmsghdr() {
            let mut buf = [0xAAu8; CONTROL];
            let number = control_message(&mut buf, 0, 1, 81, &0x0102_0304u32.to_ne_bytes());
            assert_eq!(number, nix::cmsg_space!(u32).len());
            let both = control_message(&mut buf, number, 17, 103, &1222u16.to_ne_bytes());
            assert_eq!(both - number, nix::cmsg_space!(u16).len());
            let word = std::mem::size_of::<usize>();
            let header = std::mem::size_of::<libc::cmsghdr>();
            let field = |at: usize, len: usize| &buf[at..at + len];
            assert_eq!(field(0, word), &(header + 4).to_ne_bytes()[..], "cmsg_len");
            assert_eq!(field(word, 4), &1i32.to_ne_bytes()[..], "cmsg_level");
            assert_eq!(field(word + 4, 4), &81i32.to_ne_bytes()[..], "cmsg_type");
            assert_eq!(field(header, 4), &0x0102_0304u32.to_ne_bytes()[..]);
            assert!(
                field(header + 4, number - header - 4)
                    .iter()
                    .all(|&b| b == 0)
            );
            assert_eq!(field(number + header, 2), &1222u16.to_ne_bytes()[..]);
        }

        /// The kernel's release, as (major, minor).
        fn kernel_release() -> Option<(u32, u32)> {
            let release = std::fs::read_to_string("/proc/sys/kernel/osrelease").ok()?;
            let mut parts = release.split(['.', '-']);
            Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
        }

        //  (real sockets: the kernel's answer): a kernel from 6.13 on
        // takes SCM_TS_OPT_ID and stamps each message with the number it
        // carried, across the u32 wrap and over IPv4 and IPv6 alike, a
        // message it refused (a UDP GSO message of more segments than
        // UDP_MAX_SEGMENTS: EINVAL, after the kernel would have numbered it
        // itself) shifting nothing; an older one may take it too, from a
        // backport. IPv6 is skipped on a host without IPv6 loopback.
        #[test]
        fn the_kernel_stamps_each_message_with_the_number_it_carried() {
            let takes = kernel_takes_numbers();
            if !takes {
                assert!(
                    kernel_release().is_some_and(|release| release < (6, 13)),
                    "Linux 6.13 and later take SCM_TS_OPT_ID"
                );
                return;
            }
            for loopback in ["127.0.0.1:0", "[::1]:0"] {
                let Ok(receiver) = UdpSocket::bind(loopback) else {
                    continue;
                };
                let sender = UdpSocket::bind(loopback).unwrap();
                sender.connect(receiver.local_addr().unwrap()).unwrap();
                sender.set_nonblocking(true).unwrap();
                assert_eq!(enable(&sender, true).unwrap(), Numbering::Explicit);
                send_numbered(&sender, None, &[1; 100], u32::MAX - 1, None).unwrap();
                send_numbered(&sender, None, &[0; 2000], u32::MAX, Some(1))
                    .expect_err("2000 one-byte segments are more than UDP_MAX_SEGMENTS");
                send_numbered(&sender, None, &[2; 100], u32::MAX, None).unwrap();
                let buf = [3u8; 300];
                let messages = [100, 200].map(|at| Message {
                    at,
                    bytes: 100,
                    datagrams: 1,
                    segment: 100,
                });
                assert_eq!(
                    send_batch_numbered(&sender, &buf, &messages, None, 0).unwrap(),
                    2
                );
                let mut numbers = Vec::new();
                for _ in 0..100 {
                    errqueue::drain_with(&sender, |queued| {
                        if let Queued::Sent { key, .. } = queued {
                            numbers.push(key);
                        }
                    });
                    if numbers.len() == 4 {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                assert_eq!(numbers, [u32::MAX - 1, u32::MAX, 0, 1], "over {loopback}");
            }
        }

        //  (real sockets): a numbered batch answers as sendmmsg would.
        // A message the kernel refuses after it took others (here EINVAL, a
        // UDP GSO message of more segments than UDP_MAX_SEGMENTS) ends the
        // call with how many it took, its error left for the next call;
        // one it refuses first is the call's error, which the socket acts
        // on (turning GSO off here)
        #[test]
        fn a_numbered_batch_answers_as_sendmmsg_would() {
            if !kernel_takes_numbers() {
                return;
            }
            let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
            let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
            sender.connect(receiver.local_addr().unwrap()).unwrap();
            sender.set_nonblocking(true).unwrap();
            enable(&sender, true).unwrap();
            // One 1-byte datagram, which one-byte segments leave whole, and
            // 2000 bytes, which they would cut into too many.
            let buf = [0u8; 2001];
            let messages = [(0, 1), (1, 2000)].map(|(at, bytes)| Message {
                at,
                bytes,
                datagrams: 1,
                segment: 1,
            });
            assert_eq!(
                send_batch_numbered(&sender, &buf, &messages, Some(1), 5).unwrap(),
                1,
                "the first went, the second was refused"
            );
            let refused = send_batch_numbered(&sender, &buf, &messages[1..], Some(1), 6)
                .expect_err("refused at its first message");
            assert_eq!(refused.raw_os_error(), Some(libc::EINVAL));
        }
    }
}

#[cfg(test)]
mod tests {
    use udpstp_core::WallTime;

    use super::*;

    fn mono(us: u64) -> MonoTime {
        MonoTime::from_duration(Duration::from_micros(us))
    }

    /// A clock pair whose wall clock is 1000 s ahead of the Test Time Base.
    fn pair() -> Now {
        Now {
            mono: mono(0),
            wall: WallTime::from_unix(Duration::from_secs(1000)),
        }
    }

    fn wall(us: u64) -> Duration {
        Duration::from_secs(1000) + Duration::from_micros(us)
    }

    /// The number of message `n` of `stamps`, counted from its first.
    fn number(stamps: &SendStamps, n: u32) -> u32 {
        let first = match stamps.numbering {
            Numbering::Explicit => FIRST_CARRIED,
            Numbering::Kernel => 0,
        };
        first.wrapping_add(n)
    }

    // : each timestamp is matched to its Send Plan by the message's
    // number, and the lag is measured on the Test Time Base
    #[test]
    fn timestamps_are_matched_to_their_send_plans() {
        for numbering in [Numbering::Explicit, Numbering::Kernel] {
            let mut stamps = SendStamps::new(numbering);
            stamps.sent(2, 2, Some(mono(100))); // messages 0 and 1
            stamps.sent(1, 1, None); // 2: a STOP
            stamps.sent(1, 1, Some(mono(300))); // 3
            for (n, us) in [(0, 110), (1, 140), (3, 320)] {
                stamps.stamped(number(&stamps, n), wall(us), pair());
            }
            let lag = stamps.lag().unwrap();
            assert_eq!(lag.samples, 3, "{numbering:?}");
            assert_eq!(lag.p50, Duration::from_micros(20));
            assert_eq!(lag.max, Duration::from_micros(40));
            // A message whose plan is gone, or never was, isn't counted.
            stamps.stamped(number(&stamps, 2), wall(250), pair());
            stamps.stamped(number(&stamps, 0), wall(900), pair());
            assert_eq!(stamps.lag().unwrap().samples, 3);
        }
    }

    // : a message the kernel refused, whole or after others of its
    // call, leaves its number to the next when it carried it, so the
    // messages after it keep being matched to their Send Plans
    #[test]
    fn with_carried_numbers_a_refused_send_shifts_nothing() {
        let mut stamps = SendStamps::new(Numbering::Explicit);
        assert_eq!(stamps.carried(), Some(number(&stamps, 0)));
        stamps.sent(1, 1, Some(mono(100))); // message 0
        stamps.sent(1, 0, Some(mono(200))); // refused: 1 is the next's
        assert_eq!(stamps.carried(), Some(number(&stamps, 1)));
        stamps.sent(3, 1, Some(mono(300))); // 1, then one refused
        stamps.sent(2, 2, Some(mono(400))); // 2 and 3
        assert_eq!(stamps.carried(), Some(number(&stamps, 4)));
        for (n, us) in [(0, 110), (1, 305), (2, 420), (3, 450)] {
            stamps.stamped(number(&stamps, n), wall(us), pair());
        }
        // Lags of 10, 5, 20 and 50 µs.
        let lag = stamps.lag().unwrap();
        assert_eq!(lag.samples, 4);
        assert_eq!(lag.p50, Duration::from_micros(10));
        assert_eq!(lag.max, Duration::from_micros(50));
    }

    // : the numbers messages carry wrap only after 2^31 messages, and
    // keep matching their Send Plans across the wrap: here plans of five
    // messages each, all sent before any is stamped, from 50 messages
    // before it
    #[test]
    fn carried_numbers_keep_matching_across_the_wrap() {
        let mut stamps = SendStamps::new(Numbering::Explicit);
        let first = u32::MAX - 49;
        stamps.next_key = first;
        for plan in 0..20 {
            stamps.sent(5, 5, Some(mono(plan * 100)));
        }
        assert_eq!(stamps.carried(), Some(50), "past the wrap");
        for n in 0..100 {
            let plan = u64::from(n / 5);
            stamps.stamped(first.wrapping_add(n), wall(plan * 100 + 7), pair());
        }
        let lag = stamps.lag().unwrap();
        assert_eq!((lag.samples, lag.max), (100, Duration::from_micros(7)));
    }

    // : a kernel that ignored the numbers messages carry would stamp
    // them with its own, from 0, half the u32 range from udpstp's: each
    // reads as coming before the oldest Send Plan remembered, so it
    // matches none, not even when it comes back thousands of messages late
    #[test]
    fn the_kernels_own_numbers_match_no_carried_one() {
        let mut stamps = SendStamps::new(Numbering::Explicit);
        for n in 0..1000 {
            stamps.sent(1, 1, Some(mono(n * 100)));
            stamps.stamped(n as u32, wall(n * 100 + 7), pair());
        }
        // Timestamps coming back 3000 messages late.
        for n in 1000..4000 {
            stamps.sent(1, 1, Some(mono(n * 100)));
        }
        for n in 1000..4000 {
            stamps.stamped(n as u32, wall(n * 100 + 7), pair());
        }
        assert_eq!(stamps.lag(), None);
        // Each came before the oldest plan: none was dropped as passed.
        assert_eq!(stamps.pending.len(), 4000);
    }

    // , : when the kernel numbers the messages, it may have
    // numbered one it refused, whole or after others of its call, after
    // which its numbering may have moved on without this count: the plans
    // numbered before still match their timestamps, whenever they come,
    // and no plan sent after is matched
    #[test]
    fn with_the_kernels_numbers_a_refused_send_stops_the_count() {
        for (offered, taken) in [(1, 0), (3, 1)] {
            let mut stamps = SendStamps::new(Numbering::Kernel);
            assert_eq!(stamps.carried(), None);
            stamps.sent(1, 1, Some(mono(100))); // 0
            stamps.sent(offered, taken, Some(mono(200))); // 1, if taken
            stamps.sent(1, 1, Some(mono(300))); // unknown
            // Message 0's timestamp comes after the refusal; message 1, if
            // taken, is the call's first, numbered before the refusal. The
            // kernel's 1 or 2 may also be the message sent after it.
            stamps.stamped(0, wall(110), pair());
            stamps.stamped(1, wall(205), pair());
            stamps.stamped(2, wall(305), pair());
            let lag = stamps.lag().unwrap();
            let want = if taken == 0 { 1 } else { 2 };
            assert_eq!(lag.samples, want, "{taken} of {offered} taken");
            assert_eq!(lag.max, Duration::from_micros(10));
        }
    }

    // : a lost timestamp doesn't stop later ones from matching, and a
    // lag beyond the buckets counts in the top one and the maximum
    #[test]
    fn missing_and_late_timestamps() {
        let mut stamps = SendStamps::new(Numbering::Kernel);
        for n in 0..100 {
            stamps.sent(1, 1, Some(mono(n * 1000)));
        }
        for n in (0..100).step_by(2) {
            stamps.stamped(n as u32, wall(n * 1000 + 5), pair());
        }
        stamps.stamped(99, wall(99_000 + 50_000), pair());
        let lag = stamps.lag().unwrap();
        assert_eq!(lag.samples, 51);
        assert_eq!(lag.p50, Duration::from_micros(5));
        assert_eq!(
            lag.p99,
            Duration::from_millis(50).min(Duration::from_micros(9_999))
        );
        assert_eq!(lag.max, Duration::from_millis(50));
        assert_eq!(SendStamps::new(Numbering::Kernel).lag(), None);
    }
}
