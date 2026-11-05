//! Batched and offloaded sending on Linux: a Send Plan's Load PDUs
//! go to the kernel with sendmmsg, many in one system call, and where the
//! kernel offers UDP GSO, each run of equal-sized Load PDUs goes as one
//! message the kernel splits into datagrams. The socket module drives it
//! and handles the errors; this module groups datagrams into messages, and
//! the Linux adapter's [`Sender`] makes the calls.
//!
//! **The GSO rule.** The kernel splits a GSO message into segments of its
//! segment size, the last of which may be shorter. So a message holds
//! Load PDUs of one size, and may end with one smaller Load PDU: a Burst
//! and its Add-on, which is never larger than the Burst's Load PDUs in the
//! Sending Rate Table (RFC 9946 §7.1, udpAddon2). The grouping doesn't
//! assume it: an Add-on larger than the Burst's Load PDUs, which a Server
//! may still assign, or a randomly drawn size, starts a message of its
//! own. A message holds at most [`MAX_SEGMENTS`] Load PDUs and
//! [`MAX_GSO_BYTES`] bytes.
//!
//! **Header sets.** nix's `sendmmsg` shares its control messages across
//! every message of one call and needs preallocated headers, one set per
//! control-message set: one
//! set without control messages and one with the UDP_SEGMENT size. So a
//! call with GSO carries messages of one segment size only. The headers
//! aren't `Send`: the Engine Thread builds them.
//!
//! **No addresses.** The socket is connected to the peer, so the messages
//! carry no address. nix's results would otherwise read addresses the
//! kernel never wrote.

use std::io::{self, IoSlice};
use std::net::UdpSocket;
use std::os::fd::AsRawFd;

use nix::sys::socket::{ControlMessage, MsgFlags, MultiHeaders, getsockopt, sendmmsg, sockopt};

/// The most messages one sendmmsg call carries.
pub(crate) const BATCH: usize = 64;

/// The most Load PDUs one GSO message carries: the kernel's
/// UDP_MAX_SEGMENTS, which was 64 until Linux 6.9 raised it to 128.
pub(crate) const MAX_SEGMENTS: u32 = 64;

/// The most UDP payload one GSO message carries: what fits in one IPv4
/// datagram of 65,535 bytes after its IP and UDP headers. The kernel
/// refuses more (EMSGSIZE).
pub(crate) const MAX_GSO_BYTES: usize = 65_535 - 20 - 8;

/// Datagrams that go to the kernel as one message: `datagrams` of
/// `segment` bytes each, the last possibly shorter, `bytes` in all,
/// starting at `at` in the send buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Message {
    pub(crate) at: usize,
    pub(crate) bytes: usize,
    pub(crate) datagrams: u32,
    pub(crate) segment: usize,
}

/// Groups datagrams, each `(offset, length)` in the send buffer and back
/// to back in it, into messages in order: with `gso`, as many as the GSO
/// rule lets share one; without, one each.
pub(crate) fn group(
    datagrams: impl Iterator<Item = (usize, usize)>,
    gso: bool,
    out: &mut Vec<Message>,
) {
    out.clear();
    // Whether the last message may take another datagram: not once a
    // shorter one ended it.
    let mut open = false;
    for (at, len) in datagrams {
        if let Some(last) = out.last_mut().filter(|_| gso && open)
            && len <= last.segment
            && last.datagrams < MAX_SEGMENTS
            && last.bytes + len <= MAX_GSO_BYTES
        {
            last.bytes += len;
            last.datagrams += 1;
            open = len == last.segment;
            continue;
        }
        out.push(Message {
            at,
            bytes: len,
            datagrams: 1,
            segment: len,
        });
        open = true;
    }
}

/// The messages one call sends, from `messages[0]`: how many, and the
/// segment size to send them with, if any. A call with GSO carries
/// messages of one segment size; a call without, single datagrams.
pub(crate) fn next_call(messages: &[Message]) -> (usize, Option<usize>) {
    let Some(first) = messages.first() else {
        return (0, None);
    };
    let count = if first.datagrams > 1 {
        messages
            .iter()
            .take(BATCH)
            .take_while(|m| m.segment == first.segment)
            .count()
    } else {
        messages
            .iter()
            .take(BATCH)
            .take_while(|m| m.datagrams == 1)
            .count()
    };
    (count, (first.datagrams > 1).then_some(first.segment))
}

/// Whether the kernel knows UDP GSO: it has had the UDP_SEGMENT socket
/// option since Linux 4.18. Which errors refuse it, or sendmmsg, on a send
/// is the classifier's to say (ADR 0006, `socket::classify`).
pub(crate) fn gso_supported(socket: &UdpSocket) -> bool {
    getsockopt(socket, sockopt::UdpGsoSegment).is_ok()
}

/// The preallocated headers of batched sending on one Engine Thread.
pub(crate) struct Sender {
    plain: MultiHeaders<()>,
    gso: MultiHeaders<()>,
}

impl Sender {
    pub(crate) fn new() -> Self {
        Self {
            plain: MultiHeaders::preallocate(BATCH, None),
            gso: MultiHeaders::preallocate(BATCH, Some(nix::cmsg_space!(u16))),
        }
    }

    /// Sends `messages` (at most [`BATCH`] of them) from `buf` on the
    /// connected `socket` in one sendmmsg call, with GSO when `segment` is
    /// given: how many of them the kernel took whole, in order.
    pub(crate) fn send(
        &mut self,
        socket: &UdpSocket,
        buf: &[u8],
        messages: &[Message],
        segment: Option<usize>,
    ) -> io::Result<usize> {
        let messages = &messages[..messages.len().min(BATCH)];
        let mut slices = [[IoSlice::new(&[])]; BATCH];
        for (slice, message) in slices.iter_mut().zip(messages) {
            *slice = [IoSlice::new(&buf[message.at..message.at + message.bytes])];
        }
        let addresses = [None::<()>; BATCH];
        let count = messages.len();
        // The segment size fits: every Load PDU is at most 65,535 bytes.
        let size = segment.map_or(0, |s| u16::try_from(s).unwrap_or(u16::MAX));
        let control = [ControlMessage::UdpGsoSegments(&size)];
        let (headers, control): (_, &[ControlMessage]) = match segment {
            Some(_) => (&mut self.gso, &control),
            None => (&mut self.plain, &[]),
        };
        let sent = sendmmsg(
            socket.as_raw_fd(),
            headers,
            &slices[..count],
            &addresses[..count],
            control,
            MsgFlags::empty(),
        )?;
        // A datagram goes whole or not at all; count only messages the
        // kernel reports it took whole.
        Ok(sent
            .zip(messages)
            .take_while(|(result, message)| result.bytes == message.bytes)
            .count())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Back-to-back datagrams of these lengths, as a Send Plan's layout.
    fn layout(lengths: &[usize]) -> Vec<(usize, usize)> {
        let mut at = 0;
        lengths
            .iter()
            .map(|&len| {
                let datagram = (at, len);
                at += len;
                datagram
            })
            .collect()
    }

    fn grouped(lengths: &[usize], gso: bool) -> Vec<(u32, usize, usize)> {
        let mut out = Vec::new();
        group(layout(lengths).into_iter(), gso, &mut out);
        out.iter()
            .map(|m| (m.datagrams, m.segment, m.bytes))
            .collect()
    }

    // : a Burst and its smaller Add-on share one GSO message (the
    // kernel's rule: all segments equal but the last, which is no larger).
    #[test]
    fn a_burst_and_its_add_on_are_one_message() {
        assert_eq!(
            grouped(&[1222, 1222, 597], true),
            [(3, 1222, 3041)],
            "the Add-on ends the message"
        );
        // A Load PDU after the Add-on starts a new one.
        assert_eq!(
            grouped(&[1222, 597, 1222], true),
            [(2, 1222, 1819), (1, 1222, 1222)]
        );
    }

    // : an Add-on larger than its Burst's Load PDUs, which a Server may
    // assign, can't end their message.
    #[test]
    fn a_larger_add_on_is_a_message_of_its_own() {
        assert_eq!(
            grouped(&[500, 500, 1222], true),
            [(2, 500, 1000), (1, 1222, 1222)]
        );
    }

    // : a message holds at most UDP_MAX_SEGMENTS Load PDUs and 64 KiB.
    #[test]
    fn a_message_is_bounded() {
        let many = grouped(&[100; 150], true);
        assert_eq!(
            many,
            [(64, 100, 6400), (64, 100, 6400), (22, 100, 2200)],
            "at most 64 segments"
        );
        let jumbo = grouped(&[8972; 10], true);
        assert_eq!(
            jumbo,
            [(7, 8972, 62_804), (3, 8972, 26_916)],
            "at most 64 KiB"
        );
    }

    #[test]
    fn without_gso_each_datagram_is_a_message() {
        assert_eq!(
            grouped(&[1222, 1222, 597], false),
            [(1, 1222, 1222), (1, 1222, 1222), (1, 597, 597)]
        );
    }

    // : nix's sendmmsg shares one control-message set per call, so a
    // call with GSO carries one segment size.
    #[test]
    fn a_call_carries_one_segment_size() {
        let mut messages = Vec::new();
        group(
            layout(&[500, 500, 1222, 1222, 64]).into_iter(),
            true,
            &mut messages,
        );
        assert_eq!(next_call(&messages), (1, Some(500)));
        assert_eq!(next_call(&messages[1..]), (1, Some(1222)));
        group(layout(&[500, 700, 900]).into_iter(), true, &mut messages);
        assert_eq!(
            next_call(&messages),
            (3, None),
            "single datagrams share one"
        );
    }

    /// A connected pair of loopback sockets: the sender and the receiver.
    fn pair() -> (UdpSocket, UdpSocket) {
        let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
        receiver
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        sender.connect(receiver.local_addr().unwrap()).unwrap();
        (sender, receiver)
    }

    /// Sends datagrams of `lengths`, each filled with its index, as the
    /// engine does: grouped, one call at a time. Returns what arrived: each
    /// datagram's length and fill.
    fn send_and_receive(lengths: &[usize], gso: bool) -> Vec<(usize, u8)> {
        let (sender, receiver) = pair();
        let datagrams = layout(lengths);
        let mut buf = vec![0u8; lengths.iter().sum()];
        for (index, &(at, len)) in datagrams.iter().enumerate() {
            buf[at..at + len].fill(index as u8);
        }
        assert!(!gso || gso_supported(&sender), "this kernel has UDP GSO");
        let mut messages = Vec::new();
        group(datagrams.into_iter(), gso, &mut messages);
        let mut batches = Sender::new();
        let mut next = 0;
        while next < messages.len() {
            let (count, segment) = next_call(&messages[next..]);
            let sent = batches
                .send(&sender, &buf, &messages[next..next + count], segment)
                .unwrap();
            assert_eq!(sent, count, "loopback takes every message");
            next += sent;
        }
        let mut received = Vec::new();
        let mut datagram = vec![0u8; 65_536];
        for _ in lengths {
            let len = receiver.recv(&mut datagram).unwrap();
            assert!(
                datagram[..len].iter().all(|&b| b == datagram[0]),
                "a datagram holds one Load PDU"
            );
            received.push((len, datagram[0]));
        }
        received
    }

    // , docs/spec.md S2b (real sockets: GSO over loopback): Load PDUs sent
    // with sendmmsg and UDP GSO arrive as the datagrams they were, in
    // order, the Add-on at its own size.
    #[test]
    fn batched_datagrams_arrive_whole_and_in_order() {
        // Deferred Datagrams, then a Burst and its Add-on; then more than
        // one call's worth, and more than one message's.
        let mut lengths = vec![1222, 1222, 1222, 1222, 597];
        lengths.extend([100; 70]);
        lengths.extend([700, 64]);
        let expected: Vec<(usize, u8)> = lengths
            .iter()
            .enumerate()
            .map(|(index, &len)| (len, index as u8))
            .collect();
        assert_eq!(send_and_receive(&lengths, true), expected, "with GSO");
        assert_eq!(send_and_receive(&lengths, false), expected, "without");
    }
}
