//! The fault adapter: [`Faulty`] wraps another adapter of a Test
//! Connection's socket and injects faults into its calls, as [`Faults`]
//! says, passing them on otherwise. Only tests build it: the runtime's
//! own, which give it to an engine directly (`engine::spawn_with`), and the
//! `udpstp` binary's, through `UDPSTP_FAULTS` and the runtime's
//! `fault-injection` feature, which only they enable. Release builds can't
//! inject anything.
//!
//! Faults are spelled as comma-separated `name=N`, in `UDPSTP_FAULTS` too:
//!
//! - `panic_after=N`: the first engine given these faults to reach its Nth
//!   send panics, once, as a bug in the engine would. `UDPSTP_FAULTS` gives
//!   every engine of a process the same faults, so it panics once per
//!   process;
//! - `panic_on_open=1`: every socket given these faults panics as it opens
//!   on its engine's thread, before the engine's first turn, as a bug in an
//!   adapter would;
//! - `would_block_every=N`: every Nth send fails with EAGAIN, a full send
//!   buffer;
//! - `no_buffers_every=N`: every Nth send fails with ENOBUFS;
//! - `slow_send_us=N`: every send first waits N µs, a host that can't send
//!   as fast as the schedule asks.
//!
//! On Linux, where Load PDUs go in batches, a batch's sendmmsg call
//! counts as one send for the faults above, and four more apply to
//! batches only:
//!
//! - `partial_batch_every=N`: every Nth batch of more than one message
//!   hands the kernel only its first message, as a kernel whose send
//!   buffer filled after it does, and the next send fails with EAGAIN;
//! - `gso_unsupported=1`: every batch with a UDP GSO message fails with
//!   EIO, as on a device without checksum offload;
//! - `sendmmsg_unsupported=1`: every batch fails with ENOSYS, as on a
//!   kernel without sendmmsg;
//! - `qdisc_drop_every=N`: every Nth batch loses a message to the queue
//!   discipline, which the kernel had numbered for its send timestamp
//!   its second where it has more than one, its first going
//!   through the inner adapter, whose answer the call passes on, as
//!   sendmmsg answers with the messages before a drop; else its only one,
//!   and the call fails with ENOBUFS. The kernel numbers the dropped
//!   message and refuses it for real: the fault adapter hands it one of
//!   more UDP GSO segments than UDP_MAX_SEGMENTS (EINVAL) in its place.
//!
//! One more applies to kernel send timestamps:
//!
//! - `ts_opt_id_unsupported=1`: the socket doesn't number its messages
//!   itself (SCM_TS_OPT_ID), as on a kernel before Linux 6.13, so the
//!   kernel numbers their send timestamps.
//!
//! Two more apply to receiving, in batches on Linux:
//!
//! - `recvmmsg_unsupported=1`: every recvmmsg call fails with ENOSYS, as
//!   on a kernel without it;
//! - `rx_timestamps_unsupported=1`: the socket doesn't ask for kernel
//!   receive timestamps, as on a kernel that refuses SO_TIMESTAMPING.
//!
//! Two more apply to the wait between turns, in ppoll on Linux:
//!
//! - `pollerr_stuck=1`: every ppoll call returns at once with POLLERR and
//!   nothing to drain, as an error that stays pending on the socket would;
//! - `ppoll_unsupported=1`: every ppoll call fails with ENOSYS, as on a
//!   kernel without it.
//!
//! On Linux these two apply to the pooled executor's wait too, in
//! ppoll on its Engine Thread's epoll set: `pollerr_stuck` makes each
//! ppoll call end at once with the set readable and nothing to harvest,
//! and its ppoll calls are counted with the sockets'. One more applies to
//! the pooled executor alone:
//!
//! - `engine_thread_panic_after_opens=N`: an Engine Thread panics in its
//!   own loop, outside every Test Connection's work, once it has opened N
//!   Test Connections given these faults' recipe, as a bug in the loop
//!   would.
//!
//! Elsewhere these ten are accepted and do nothing.
//!
//! A failed send ends the engine's turn with the rest of its Send Plan
//! unsent: a partial send, which the Data Phase sends later as Deferred
//! Datagrams.

use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::protocol::MonoTime;
#[cfg(any(target_os = "linux", target_os = "android"))]
use crate::protocol::TransportError;

use super::classify::inject;
use super::{Adapter, Open, SendLag, Settings};

/// The faults to inject, and what the sockets given them did.
#[derive(Debug, Clone, Default)]
pub(crate) struct Faults {
    panic_after: Option<u64>,
    panic_on_open: bool,
    would_block_every: Option<u64>,
    no_buffers_every: Option<u64>,
    slow_send_us: Option<u64>,
    #[cfg_attr(not(any(target_os = "linux", target_os = "android")), allow(dead_code))]
    partial_batch_every: Option<u64>,
    #[cfg_attr(not(any(target_os = "linux", target_os = "android")), allow(dead_code))]
    gso_unsupported: bool,
    #[cfg_attr(not(any(target_os = "linux", target_os = "android")), allow(dead_code))]
    sendmmsg_unsupported: bool,
    #[cfg_attr(not(any(target_os = "linux", target_os = "android")), allow(dead_code))]
    qdisc_drop_every: Option<u64>,
    #[cfg_attr(not(any(target_os = "linux", target_os = "android")), allow(dead_code))]
    ts_opt_id_unsupported: bool,
    #[cfg_attr(not(any(target_os = "linux", target_os = "android")), allow(dead_code))]
    recvmmsg_unsupported: bool,
    #[cfg_attr(not(any(target_os = "linux", target_os = "android")), allow(dead_code))]
    rx_timestamps_unsupported: bool,
    #[cfg_attr(not(any(target_os = "linux", target_os = "android")), allow(dead_code))]
    pollerr_stuck: bool,
    #[cfg_attr(not(any(target_os = "linux", target_os = "android")), allow(dead_code))]
    ppoll_unsupported: bool,
    #[cfg_attr(not(any(target_os = "linux", target_os = "android")), allow(dead_code))]
    engine_thread_panic_after_opens: Option<u64>,
    /// Whether a socket given these faults, or a copy of them, has
    /// panicked already.
    panicked: Arc<AtomicBool>,
    /// The ppoll calls the sockets given these faults, or a copy of them,
    /// made, injected or not.
    #[cfg_attr(not(any(target_os = "linux", target_os = "android")), allow(dead_code))]
    polls: Arc<AtomicU64>,
    /// The reads (recvmmsg or single) the sockets given these faults, or a
    /// copy of them, made: at least one a turn of their engine's, each
    /// receive ending with the one that found nothing more.
    #[cfg_attr(not(any(target_os = "linux", target_os = "android")), allow(dead_code))]
    reads: Arc<AtomicU64>,
    /// This socket's sends, its batches, and those of more than one
    /// message.
    sends: u64,
    #[cfg_attr(not(any(target_os = "linux", target_os = "android")), allow(dead_code))]
    batch_calls: u64,
    #[cfg_attr(not(any(target_os = "linux", target_os = "android")), allow(dead_code))]
    batches: u64,
    /// A partial batch went: the next send fails with EAGAIN.
    after_partial: bool,
}

/// What a fault does to one batch.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[derive(Debug)]
enum BatchFault {
    /// The batch fails with this error, and sends nothing.
    Fail(io::Error),
    /// The kernel takes only this many of its messages.
    Take(usize),
    /// The kernel numbers a message and refuses it: the batch's second,
    /// after its first went, or its only one, when the batch fails with
    /// ENOBUFS.
    QdiscDrop,
}

/// A message the kernel numbers for its send timestamp and then refuses:
/// more UDP GSO segments than UDP_MAX_SEGMENTS (EINVAL from udp_send_skb,
/// after the kernel numbered it). A kernel without UDP GSO sends it
/// instead, which numbers it all the same; its peer drops it, since it
/// holds no PDU.
#[cfg(any(target_os = "linux", target_os = "android"))]
const REFUSED: super::batch::Message = super::batch::Message {
    at: 0,
    bytes: 4096,
    datagrams: 4096,
    segment: 1,
};

#[cfg(any(target_os = "linux", target_os = "android"))]
static REFUSED_BYTES: [u8; REFUSED.bytes] = [0; REFUSED.bytes];

impl Faults {
    /// The faults `spec` names, as `UDPSTP_FAULTS` spells them; none for
    /// an empty one. Panics on a fault it doesn't know, or a count that
    /// isn't positive.
    pub(crate) fn parse(spec: &str) -> Self {
        let mut faults = Self::default();
        for item in spec.split(',').map(str::trim).filter(|i| !i.is_empty()) {
            let (name, value) = item
                .split_once('=')
                .unwrap_or_else(|| panic!("UDPSTP_FAULTS: {item} isn't name=N"));
            let value: u64 = value
                .parse()
                .ok()
                .filter(|&n| n > 0)
                .unwrap_or_else(|| panic!("UDPSTP_FAULTS: {item} needs a positive N"));
            match name {
                "panic_after" => faults.panic_after = Some(value),
                "panic_on_open" => faults.panic_on_open = true,
                "would_block_every" => faults.would_block_every = Some(value),
                "no_buffers_every" => faults.no_buffers_every = Some(value),
                "slow_send_us" => faults.slow_send_us = Some(value),
                "partial_batch_every" => faults.partial_batch_every = Some(value),
                "gso_unsupported" => faults.gso_unsupported = true,
                "sendmmsg_unsupported" => faults.sendmmsg_unsupported = true,
                "qdisc_drop_every" => faults.qdisc_drop_every = Some(value),
                "ts_opt_id_unsupported" => faults.ts_opt_id_unsupported = true,
                "recvmmsg_unsupported" => faults.recvmmsg_unsupported = true,
                "rx_timestamps_unsupported" => faults.rx_timestamps_unsupported = true,
                "pollerr_stuck" => faults.pollerr_stuck = true,
                "ppoll_unsupported" => faults.ppoll_unsupported = true,
                "engine_thread_panic_after_opens" => {
                    faults.engine_thread_panic_after_opens = Some(value);
                }
                _ => panic!("UDPSTP_FAULTS: unknown fault {name}"),
            }
        }
        faults
    }

    /// The ppoll calls the sockets given these faults, or a copy of them,
    /// have made so far.
    #[cfg(all(test, any(target_os = "linux", target_os = "android")))]
    pub(crate) fn ppoll_calls(&self) -> u64 {
        self.polls.load(Ordering::Relaxed)
    }

    /// The reads the sockets given these faults, or a copy of them, have
    /// made so far: an engine makes at least one a turn, so this bounds its
    /// turns from below.
    #[cfg(all(test, any(target_os = "linux", target_os = "android")))]
    pub(crate) fn read_calls(&self) -> u64 {
        self.reads.load(Ordering::Relaxed)
    }

    /// Called before each send: panics, or returns the error to fail it
    /// with, when a fault is due.
    fn before_send(&mut self) -> Option<io::Error> {
        self.sends += 1;
        if let Some(us) = self.slow_send_us {
            std::thread::sleep(std::time::Duration::from_micros(us));
        }
        let due = |every: Option<u64>| every.is_some_and(|n| self.sends.is_multiple_of(n));
        if self.panic_after == Some(self.sends) && !self.panicked.swap(true, Ordering::SeqCst) {
            panic!("injected fault: the Data Phase Engine panics (UDPSTP_FAULTS)");
        }
        if std::mem::take(&mut self.after_partial) || due(self.would_block_every) {
            return Some(io::Error::from(io::ErrorKind::WouldBlock));
        }
        if due(self.no_buffers_every) {
            return Some(inject::no_buffers());
        }
        None
    }

    /// Called before each batch of `messages` messages, `gso` when one of
    /// them is a UDP GSO message: what a fault does to it, if one is due.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn before_batch(&mut self, messages: usize, gso: bool) -> Option<BatchFault> {
        if let Some(error) = self.before_send() {
            return Some(BatchFault::Fail(error));
        }
        if self.sendmmsg_unsupported {
            return Some(BatchFault::Fail(inject::not_implemented()));
        }
        if gso && self.gso_unsupported {
            return Some(BatchFault::Fail(inject::device_error()));
        }
        self.batch_calls += 1;
        if self
            .qdisc_drop_every
            .is_some_and(|n| self.batch_calls.is_multiple_of(n))
        {
            return Some(BatchFault::QdiscDrop);
        }
        if messages < 2 {
            return None;
        }
        self.batches += 1;
        if self
            .partial_batch_every
            .is_some_and(|n| self.batches.is_multiple_of(n))
        {
            self.after_partial = true;
            return Some(BatchFault::Take(1));
        }
        None
    }
}

/// The fault adapter around `A`, or the recipe that opens it around the
/// adapter recipe `A` opens.
#[derive(Clone)]
pub(crate) struct Faulty<A> {
    pub(crate) inner: A,
    pub(crate) faults: Faults,
}

/// The faults `UDPSTP_FAULTS` asks for, around the platform's adapter:
/// every engine of the process gets the same, read when the first starts;
/// none when it's unset.
#[cfg(feature = "fault-injection")]
pub(crate) fn from_env() -> Option<Faulty<super::Platform>> {
    static FAULTS: std::sync::OnceLock<Option<Faults>> = std::sync::OnceLock::new();
    FAULTS
        .get_or_init(|| {
            std::env::var("UDPSTP_FAULTS")
                .ok()
                .map(|spec| Faults::parse(&spec))
        })
        .clone()
        .map(|faults| Faulty {
            inner: super::Platform,
            faults,
        })
}

impl<O: Open> Open for Faulty<O> {
    type Adapter = Faulty<O::Adapter>;

    fn open(self, socket: UdpSocket, settings: &Settings) -> Self::Adapter {
        if self.faults.panic_on_open {
            panic!("injected fault: the socket panics as it opens (UDPSTP_FAULTS)");
        }
        let settings = Settings {
            rx_timestamps: settings.rx_timestamps && !self.faults.rx_timestamps_unsupported,
            explicit_numbers: settings.explicit_numbers && !self.faults.ts_opt_id_unsupported,
            ..*settings
        };
        Faulty {
            inner: self.inner.open(socket, &settings),
            faults: self.faults,
        }
    }

    /// The pooled executor's wait, as the ppoll faults say: refused,
    /// or ending at once with its epoll set readable and nothing pending;
    /// its ppoll calls counted with the sockets'; and a panic in the Engine
    /// Thread's own loop.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn pool_hooks(&self) -> crate::runtime::engine::pool::Hooks {
        crate::runtime::engine::pool::Hooks {
            ppoll_refused: self.faults.ppoll_unsupported,
            epoll_stuck: self.faults.pollerr_stuck,
            polls: Some(self.faults.polls.clone()),
            panic_after_opens: self
                .faults
                .engine_thread_panic_after_opens
                .map(|opens| (opens, self.faults.panicked.clone())),
        }
    }
}

impl<A: Adapter> Adapter for Faulty<A> {
    fn send(&mut self, datagram: &[u8], stamped: Option<MonoTime>) -> io::Result<usize> {
        if let Some(error) = self.faults.before_send() {
            return Err(error);
        }
        self.inner.send(datagram, stamped)
    }

    fn recv(&mut self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        self.faults.reads.fetch_add(1, Ordering::Relaxed);
        self.inner.recv(buf)
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn offers(&self) -> super::Offers {
        self.inner.offers()
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn send_batch(
        &mut self,
        buf: &[u8],
        messages: &[super::batch::Message],
        segment: Option<usize>,
        stamped: Option<MonoTime>,
    ) -> io::Result<usize> {
        let messages = match self.faults.before_batch(messages.len(), segment.is_some()) {
            Some(BatchFault::Fail(error)) => return Err(error),
            Some(BatchFault::Take(taken)) => &messages[..taken.min(messages.len())],
            Some(BatchFault::QdiscDrop) => {
                // The message before the one dropped goes, as sendmmsg's
                // do; if it doesn't, its answer stands.
                let before = if messages.len() > 1 {
                    match self.inner.send_batch(buf, &messages[..1], segment, stamped) {
                        Ok(1) => 1,
                        answer => return answer,
                    }
                } else {
                    0
                };
                let _ = self.inner.send_batch(
                    &REFUSED_BYTES,
                    &[REFUSED],
                    Some(REFUSED.segment),
                    stamped,
                );
                return if before > 0 {
                    Ok(before)
                } else {
                    Err(inject::no_buffers())
                };
            }
            None => messages,
        };
        self.inner.send_batch(buf, messages, segment, stamped)
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn read_batch(&mut self, max: usize) -> io::Result<usize> {
        self.faults.reads.fetch_add(1, Ordering::Relaxed);
        if self.faults.recvmmsg_unsupported {
            return Err(inject::not_implemented());
        }
        self.inner.read_batch(max)
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn batch_datagram(&self, index: usize) -> (&[u8], super::batch_read::Received) {
        self.inner.batch_datagram(index)
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn ppoll(&mut self, readable: bool, timeout: std::time::Duration) -> io::Result<super::Woke> {
        self.faults.polls.fetch_add(1, Ordering::Relaxed);
        if self.faults.ppoll_unsupported {
            return Err(inject::not_implemented());
        }
        if self.faults.pollerr_stuck {
            return Ok(super::Woke::Ready {
                readable: false,
                error: true,
            });
        }
        self.inner.ppoll(readable, timeout)
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn drain_queue(&mut self, each: &mut impl FnMut(TransportError)) -> (bool, bool) {
        self.inner.drain_queue(each)
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn pending_error(&mut self) -> Option<io::Error> {
        self.inner.pending_error()
    }

    fn receive_buffer_overflows(&self) -> Option<u64> {
        self.inner.receive_buffer_overflows()
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn socket(&self) -> &UdpSocket {
        self.inner.socket()
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn overflow_count(&self) -> super::OverflowCount {
        self.inner.overflow_count()
    }

    fn send_lag(&self) -> Option<SendLag> {
        self.inner.send_lag()
    }
}
