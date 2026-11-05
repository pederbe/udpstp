//! A Test Connection's socket in its Data Phase: the per-socket I/O the
//! Data Phase Engine drives, behind one interface, [`Socket`], over an
//! adapter per platform.
//!
//! **What it hides.** A Send Plan's datagrams go with the fastest calls
//! the kernel takes: on Linux sendmmsg with UDP GSO, then sendmmsg without
//! GSO, then one send per datagram, each fallback kept from the first
//! refusal on. Reads go with recvmmsg, header-only for a Load
//! Receiver, each datagram with its kernel receive timestamp and TOS, then
//! one read per datagram. Every failed call's error is classified
//! once ([`classify`](mod@classify), ADR 0006): a full send buffer ends a
//! Send Plan's sending, the rest becoming Deferred Datagrams; an ICMP-type
//! error is reported, of the kind the socket's error queue says on Linux,
//! and a send made once more; a datagram too large for the Path MTU counts
//! as sent, and lost. Only a socket that is unusable fails a call. Kernel
//! send timestamps are numbered and matched inside the Linux
//! adapter, each message carrying its own number where the kernel takes
//! it. The socket reports the fast paths it used, and at the end its
//! Receive Buffer Overflows.
//!
//! **What stays outside.** The turn loop, and when and how long to wait
//! between turns (`wait.rs`), are the engine's: the socket waits only when
//! asked, for itself to be ready (ppoll, Linux). The Linux Server's pooled
//! executor (ADR 0008) drives it with a turn loop and a wait of its
//! own.
//!
//! **Adapters.** [`Portable`] sends and reads one datagram per call with
//! std's socket, on Windows and macOS. `Linux` adds the batched calls, the
//! readiness wait, the error queue and kernel send timestamps, and makes
//! its single sends and reads with `Portable`'s calls. `faults::Faulty`
//! wraps either and injects faults into its calls, in tests only. An
//! [`Open`] recipe, given to the engine at spawn, opens the adapter on the
//! Engine Thread, since nix's header sets aren't `Send`. Everything is
//! dispatched statically, and nothing is allocated in the steady state
//! (ADR 0008): only the recvmmsg headers rebuilt after a datagram's
//! control messages didn't fit (`batch_read.rs`), and the Receive Buffer
//! Overflow count read when the Data Phase ends.

use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::time::Duration;

use udpstp_core::{MonoTime, RxBatch, RxDatagram, RxPdu, SendPlan, TransportError};

use crate::buffers::{self, SocketBuffers};
use crate::clock::Clock;
use crate::engine::times::Times;
use crate::ip_options;
use crate::transport::MAX_DATAGRAM;
use classify::{Action, Call, classify};

#[cfg(any(target_os = "linux", target_os = "android"))]
pub(crate) mod batch;
#[cfg(any(target_os = "linux", target_os = "android"))]
pub(crate) mod batch_read;
pub(crate) mod classify;
#[cfg(any(target_os = "linux", target_os = "android"))]
pub(crate) mod errqueue;
#[cfg(any(test, feature = "fault-injection"))]
pub(crate) mod faults;
#[cfg(any(target_os = "linux", target_os = "android"))]
mod linux;
#[cfg(any(target_os = "linux", target_os = "android"))]
mod overflows;
mod portable;
// Only Linux asks for kernel send timestamps; their arithmetic is tested
// everywhere.
#[cfg_attr(not(any(target_os = "linux", target_os = "android")), allow(dead_code))]
pub(crate) mod send_stamps;
#[cfg(test)]
mod tests;

#[cfg(any(target_os = "linux", target_os = "android"))]
pub(crate) use linux::Linux;
pub(crate) use portable::Portable;
pub use send_stamps::SendLag;

/// pduId of the Status PDU: these go to [`Sink::status_pdu`], everything
/// else to [`Sink::load_pdus`], where validation sorts it (RFC 9946 §8.2).
const STATUS_PDU_ID: [u8; 2] = [0xFE, 0xED];

/// The most datagrams read in one turn from a batched read (recvmmsg), so
/// a flood can't starve sending.
#[cfg(any(target_os = "linux", target_os = "android"))]
const READ_BUDGET: usize = 256;

/// How long one turn may read datagrams one at a time, where reads aren't
/// batched (Windows, macOS, and Linux without recvmmsg), so a flood can't
/// starve sending: time rather than a count. With a count of 256 a
/// Windows Load Receiver fell behind at 2.1 Gbps (100,000 Load PDUs a
/// second), sleeping between turns with Load PDUs waiting, and lost 5-8 %
/// of them.
const READ_SLICE: Duration = Duration::from_millis(1);

/// The fast paths a Data Phase Engine sent Load PDUs, read
/// datagrams and waited between turns with: results
/// metadata. A Load Receiver sends no Load PDUs. Linux only; elsewhere
/// every datagram goes with a system call of its own, is timed by the
/// read, and the engine sleeps between turns.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FastPaths {
    /// Load PDUs went to the kernel in batches, with sendmmsg.
    pub sendmmsg: bool,
    /// Runs of equal-sized Load PDUs went as one message each, which the
    /// kernel split into datagrams (UDP GSO): in batches with sendmmsg, or
    /// each with a sendmsg call of its own where each carried the number
    /// of its kernel send timestamp.
    pub udp_gso: bool,
    /// Datagrams were read in batches, with recvmmsg: the Load Receiver's
    /// Load PDUs header-only.
    pub recvmmsg: bool,
    /// Datagrams from the peer came with the kernel's receive timestamps,
    /// which time them instead of the read.
    pub kernel_rx_timestamps: bool,
    /// Between turns the engine waited in ppoll on its socket, so an
    /// arrival ended the wait, rather than sleeping.
    pub ppoll: bool,
    /// The socket was in its Engine Thread's epoll set: the Linux
    /// Server's pooled executor, whose Engine Thread an arrival
    /// wakes, unless the kernel refuses ppoll and it sleeps.
    pub epoll: bool,
}

/// What a Test Connection's socket hands on: what arrived from its peer,
/// and the errors the path returned about what it sent (ADR 0006). The
/// Data Phase Engine hands it all to the Test Connection's `DataPhase`.
pub(crate) trait Sink {
    /// A Status PDU from the peer, whole, with the clock pair read just
    /// after the read that returned it and its kernel receive time.
    fn status_pdu(&mut self, pdu: RxPdu<'_>);

    /// Load PDUs from the peer, header-only, from one read, in arrival
    /// order, with the clock pairs from before and after it.
    fn load_pdus(&mut self, batch: RxBatch<'_>);

    /// An error the path returned about a datagram sent: an ICMP error of
    /// its kind, or one too large for the Path MTU (`MsgSize`).
    fn path_error(&mut self, error: TransportError);
}

/// Where a Send Plan's datagrams are in the send buffer: each one's offset
/// and length, in order, back to back. A test hands the socket a list of
/// them instead of a Send Plan.
pub(crate) trait Layout {
    fn layout(&self) -> impl Iterator<Item = (usize, usize)> + '_;
}

impl Layout for SendPlan {
    fn layout(&self) -> impl Iterator<Item = (usize, usize)> + '_ {
        SendPlan::layout(self)
    }
}

#[cfg(test)]
impl Layout for [(usize, usize)] {
    fn layout(&self) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.iter().copied()
    }
}

/// The datagrams a receive read, from any source, and their bytes: what
/// the engine judges its next wait by.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Reads {
    pub(crate) datagrams: usize,
    pub(crate) bytes: u64,
    /// The turn's budget ran out before the socket was found empty: more
    /// may be waiting, which no new arrival signals (the pooled
    /// executor's ready list).
    pub(crate) more: bool,
}

/// How a socket's Receive Buffer Overflows are known as its Data Phase
/// ends (Linux): counted already, or to be read from the process's own
/// UDP tables, which the pooled executor leaves to a thread of its own
/// rather than stall its other Test Connections.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OverflowCount {
    Known(Option<u64>),
    Listed,
}

/// How a wait on the socket ended (ppoll, Linux).
#[cfg_attr(not(any(target_os = "linux", target_os = "android")), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Woke {
    /// The timeout passed.
    TimedOut,
    /// The socket was readable, or had an error pending, or both.
    Ready { readable: bool, error: bool },
    /// A signal interrupted it, or the call failed for a reason of its
    /// own.
    Interrupted,
}

/// The calls a Test Connection's socket makes, which an OS answers: one
/// adapter per platform, and one that injects faults into another's
///. The socket's datagrams go to its peer, and it reads them from
/// anyone.
pub(crate) trait Adapter {
    /// Sends one datagram to the peer. `stamped`, for a datagram of a Send
    /// Plan, is when the plan's lpduTime was stamped, which kernel send
    /// timestamps are measured from.
    fn send(&mut self, datagram: &[u8], stamped: Option<MonoTime>) -> io::Result<usize>;

    /// Reads one datagram into `buf`: its length and its source.
    fn recv(&mut self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)>;

    /// The batched sending the adapter offers (Linux), which the kernel
    /// may still refuse: as it opens, and after each batch, for how it
    /// went.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn offers(&self) -> Offers;

    /// Sends `messages`, each datagrams back to back in `buf`, in one
    /// sendmmsg call, with UDP GSO when `segment` is given: how many
    /// messages the kernel took whole, in order. Where each message
    /// carries the number of its kernel send timestamp, each goes
    /// with a sendmsg call of its own, and the answer is the same.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn send_batch(
        &mut self,
        buf: &[u8],
        messages: &[batch::Message],
        segment: Option<usize>,
        stamped: Option<MonoTime>,
    ) -> io::Result<usize>;

    /// Reads at most `max` datagrams in one recvmmsg call: how many
    /// arrived, which [`Adapter::batch_datagram`] then gives.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn read_batch(&mut self, max: usize) -> io::Result<usize>;

    /// The `index`th datagram of the last batch read: the bytes read of
    /// it, and what the kernel reported.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn batch_datagram(&self, index: usize) -> (&[u8], batch_read::Received);

    /// Waits at most `timeout` for the socket to become readable, when
    /// `readable`, and in any case for an error to be pending (POLLERR,
    /// which ppoll always reports).
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn ppoll(&mut self, readable: bool, timeout: Duration) -> io::Result<Woke>;

    /// Hands each error queued on the socket to `each`, oldest first, and
    /// matches the kernel send timestamps queued among them:
    /// whether anything was queued, and whether an error was among it.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn drain_queue(&mut self, each: &mut impl FnMut(TransportError)) -> (bool, bool);

    /// An error pending on the socket but not queued, which reading
    /// clears.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn pending_error(&mut self) -> Option<io::Error>;

    /// Receive Buffer Overflows at the socket since it was opened, where
    /// the OS counts them.
    fn receive_buffer_overflows(&self) -> Option<u64> {
        None
    }

    /// The socket (Linux): what an Engine Thread's epoll set watches, and
    /// what its Receive Buffer Overflows are read by.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn socket(&self) -> &UdpSocket;

    /// Its Receive Buffer Overflows as its Data Phase ends, without
    /// reading the process's UDP tables (Linux).
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn overflow_count(&self) -> OverflowCount {
        OverflowCount::Known(self.receive_buffer_overflows())
    }

    /// How much later than their lpduTime the kernel sent the Load PDUs,
    /// when asked to timestamp them.
    fn send_lag(&self) -> Option<SendLag> {
        None
    }
}

/// The batched sending an adapter offers when it opens (Linux): batches
/// only on a connected socket, as they carry no address, and UDP GSO where
/// the kernel knows it.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Offers {
    /// Batches of messages ([`Adapter::send_batch`]).
    pub(crate) batches: bool,
    /// Whether a batch goes in one sendmmsg call. It doesn't while each
    /// message carries the number of its kernel send timestamp:
    /// each then goes with a sendmsg call of its own, until the kernel
    /// refuses the numbers.
    pub(crate) sendmmsg: bool,
    pub(crate) udp_gso: bool,
}

/// What an adapter opens with.
#[cfg_attr(not(any(target_os = "linux", target_os = "android")), allow(dead_code))]
#[derive(Clone, Copy)]
pub(crate) struct Settings {
    /// The Test Connection's peer: where its datagrams go, and the only
    /// source it takes datagrams from.
    pub(crate) peer: SocketAddr,
    /// Whether the socket is connected to the peer, so it sends without an
    /// address.
    pub(crate) connected: bool,
    /// Whether this end is the Load Receiver, which reads Load PDUs
    /// header-only (Linux).
    pub(crate) load_receiver: bool,
    /// Whether to ask the kernel for receive timestamps (Linux): only the
    /// fault adapter says no.
    pub(crate) rx_timestamps: bool,
    /// Whether a Load Sender asks the kernel for send timestamps (Linux,
    /// ): what `UDPSTP_KERNEL_TX_TIMESTAMPS` asks, set to `1` or
    /// `kernel-count`, or a test.
    pub(crate) send_stamps: bool,
    /// Whether its messages may carry their own numbers where the kernel
    /// takes them (SCM_TS_OPT_ID), rather than the kernel numbering
    /// them: `UDPSTP_KERNEL_TX_TIMESTAMPS=kernel-count` and the fault
    /// adapter say no.
    pub(crate) explicit_numbers: bool,
    pub(crate) clock: Clock,
}

/// Opens a Test Connection's socket adapter on the Engine Thread, which
/// the recipe is sent to.
pub(crate) trait Open: Send + 'static {
    type Adapter: Adapter;

    fn open(self, socket: UdpSocket, settings: &Settings) -> Self::Adapter;

    /// Test-only hooks into the pooled executor's own wait and loop (Linux,
    /// ): none but the fault adapter's.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn pool_hooks(&self) -> crate::engine::pool::Hooks {
        crate::engine::pool::Hooks::default()
    }
}

/// The platform's own adapter: `Linux` on Linux, [`Portable`] elsewhere.
#[derive(Clone, Copy)]
pub(crate) struct Platform;

impl Open for Platform {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    type Adapter = Linux;
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    type Adapter = Portable;

    fn open(self, socket: UdpSocket, settings: &Settings) -> Self::Adapter {
        Self::Adapter::open(socket, settings)
    }
}

/// The Receive Buffer Overflows the process's own UDP tables list for each
/// of `sockets`, the tables read once (Linux): none for a socket
/// they don't list, or when they can't be read.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub(crate) fn listed_overflows<'a>(
    sockets: impl Iterator<Item = &'a UdpSocket>,
) -> Vec<Option<u64>> {
    overflows::listed_each(sockets)
}

/// Makes a Test Connection's socket ready for its Data Phase, before its
/// engine starts: non-blocking, its buffers raised to `buffer_size` within
/// the OS's limits, and every Load, Status and STOP PDU from now on marked
/// with the agreed `dscp_ecn`. On Linux it is connected to `peer`, on the
/// Client too, as batches carry no address; the Server's is already,
/// unless that failed. It then hears only its peer, whose datagrams alone
/// it takes anyway. Returns the buffers it got, if they could be read, and
/// whether it is connected.
pub(crate) fn prepare(
    socket: &UdpSocket,
    peer: SocketAddr,
    connected: bool,
    dscp_ecn: u8,
    buffer_size: usize,
) -> io::Result<(Option<SocketBuffers>, bool)> {
    socket.set_nonblocking(true)?;
    // Not knowing the sizes is no reason to stop the Test.
    let buffers = buffers::raise(&socket2::SockRef::from(socket), buffer_size).ok();
    let ipv4 = socket.local_addr()?.is_ipv4();
    ip_options::set_ds_field(&socket2::SockRef::from(socket), ipv4, dscp_ecn)?;
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let connected = connected || socket.connect(peer).is_ok();
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let _ = peer;
    Ok((buffers, connected))
}

/// The messages of the Send Plan being sent in batches, and whether they
/// go with UDP GSO (Linux).
#[cfg(any(target_os = "linux", target_os = "android"))]
struct Batches {
    messages: Vec<batch::Message>,
    gso: bool,
}

/// How a Send Plan's batches went.
#[cfg(any(target_os = "linux", target_os = "android"))]
enum Batched {
    /// These datagrams, a prefix, reached the kernel.
    Sent(u32),
    /// These did, and then sendmmsg turned out not to work here.
    Refused(u32),
}

/// A Test Connection's socket in its Data Phase, over adapter `A`.
pub(crate) struct Socket<A> {
    adapter: A,
    peer: SocketAddr,
    /// One datagram, for reads one at a time.
    buf: Box<[u8]>,
    /// Batched sending, while the adapter offers it and the kernel takes
    /// it.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    batches: Option<Batches>,
    /// Batched reading, while the kernel takes it.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    batch_reads: bool,
    /// The fast paths Load PDUs went, and datagrams came, with so far.
    fast_paths: FastPaths,
}

impl<A: Adapter> Socket<A> {
    /// Opens `socket`, made ready with [`prepare`], with `open`'s adapter.
    pub(crate) fn open<O: Open<Adapter = A>>(
        open: O,
        socket: UdpSocket,
        settings: &Settings,
    ) -> Self {
        let adapter = open.open(socket, settings);
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let offers = adapter.offers();
        Self {
            peer: settings.peer,
            buf: vec![0u8; MAX_DATAGRAM].into_boxed_slice(),
            #[cfg(any(target_os = "linux", target_os = "android"))]
            batches: offers.batches.then(|| Batches {
                // Room for the largest Send Plan without GSO
                // (data-phase-prototype.md §7: 2 × 200 + 2 datagrams), so
                // it never grows.
                messages: Vec::with_capacity(512),
                gso: offers.udp_gso,
            }),
            #[cfg(any(target_os = "linux", target_os = "android"))]
            batch_reads: true,
            fast_paths: FastPaths::default(),
            adapter,
        }
    }

    /// The fast paths it used so far.
    pub(crate) fn fast_paths(&self) -> FastPaths {
        self.fast_paths
    }

    /// Receive Buffer Overflows at the socket, where the OS counts them
    /// (Linux; ).
    pub(crate) fn receive_buffer_overflows(&self) -> Option<u64> {
        self.adapter.receive_buffer_overflows()
    }

    /// How much later than their lpduTime the kernel sent the Load PDUs,
    /// when asked to timestamp them (Linux; ).
    pub(crate) fn send_lag(&self) -> Option<SendLag> {
        self.adapter.send_lag()
    }

    /// The socket joined its Engine Thread's epoll set.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub(crate) fn joined_epoll(&mut self) {
        self.fast_paths.epoll = true;
    }

    /// The socket itself (Linux).
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub(crate) fn handle(&self) -> &UdpSocket {
        self.adapter.socket()
    }

    /// Its Receive Buffer Overflows as its Data Phase ends, without
    /// reading the process's UDP tables (Linux).
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub(crate) fn overflow_count(&self) -> OverflowCount {
        self.adapter.overflow_count()
    }

    /// Drains the errors pending on the socket, which its Engine Thread's
    /// epoll set reported (EPOLLERR), and hands them to `sink`: whether
    /// there were any (Linux).
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub(crate) fn drain_pending(&mut self, sink: &mut impl Sink) -> bool {
        self.drain_errors(sink)
    }

    /// Sends `plan`'s datagrams, written into `buf` and their lpduTime
    /// stamped at `stamped`: how many reached the kernel, a prefix; the
    /// rest are the plan's Deferred Datagrams. On Linux they go in batches
    /// while the kernel takes them; elsewhere, and once batches can't be
    /// sent, one at a time. Path errors go to `sink`. It fails only when
    /// the socket is unusable.
    pub(crate) fn send_plan(
        &mut self,
        plan: &(impl Layout + ?Sized),
        buf: &[u8],
        stamped: MonoTime,
        sink: &mut impl Sink,
    ) -> io::Result<u32> {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        if let Some(mut batches) = self.batches.take() {
            let sent = self.send_batched(&mut batches, plan, buf, stamped, sink);
            // Unless sendmmsg turned out to be unusable.
            if !matches!(sent, Ok(Batched::Refused(_))) {
                self.batches = Some(batches);
            }
            return match sent? {
                Batched::Sent(sent) => Ok(sent),
                // The rest go one at a time, now and from now on.
                Batched::Refused(sent) => {
                    let layout = plan.layout().skip(sent as usize);
                    Ok(sent + self.send_each(layout, buf, Some(stamped), sink)?)
                }
            };
        }
        self.send_each(plan.layout(), buf, Some(stamped), sink)
    }

    /// Sends one datagram that belongs to no Send Plan, such as a Status
    /// PDU: whether it reached the kernel, as [`Socket::send_plan`] sends.
    pub(crate) fn send(&mut self, datagram: &[u8], sink: &mut impl Sink) -> io::Result<bool> {
        self.send_datagram(datagram, None, sink)
    }

    /// Sends each of `datagrams`, `(offset, length)` in `buf`, on its own
    /// until one doesn't reach the kernel: how many did.
    fn send_each(
        &mut self,
        datagrams: impl Iterator<Item = (usize, usize)>,
        buf: &[u8],
        stamped: Option<MonoTime>,
        sink: &mut impl Sink,
    ) -> io::Result<u32> {
        let mut sent = 0;
        for (at, len) in datagrams {
            if !self.send_datagram(&buf[at..at + len], stamped, sink)? {
                break;
            }
            sent += 1;
        }
        Ok(sent)
    }

    /// Sends one datagram; whether it reached the kernel. A full send
    /// buffer is backpressure: the rest of the Burst waits (ADR 0008). An
    /// ICMP error a connected socket reports on a send is about an earlier
    /// datagram, so it is reported and the send tried once more (ADR 0006).
    fn send_datagram(
        &mut self,
        datagram: &[u8],
        stamped: Option<MonoTime>,
        sink: &mut impl Sink,
    ) -> io::Result<bool> {
        let mut sent = self.adapter.send(datagram, stamped);
        if let Err(error) = &sent
            && let Action::PathError(kind) = classify(Call::Send, error)
        {
            self.path_error(kind, sink);
            sent = self.adapter.send(datagram, stamped);
        }
        let Err(error) = sent else {
            return Ok(true);
        };
        match classify(Call::Send, &error) {
            Action::Fatal => Err(error),
            // Too large to leave with DF set: it will never go, so it counts
            // as sent and is lost, rather than holding back every later
            // Burst. The algorithm then backs off to rows whose datagrams
            // fit (ADR 0006: a Path MTU event).
            Action::TooLarge => {
                sink.path_error(TransportError::MsgSize);
                Ok(true)
            }
            Action::PathError(kind) => {
                self.path_error(kind, sink);
                Ok(false)
            }
            Action::Retry | Action::Blocked | Action::Unsupported(_) => Ok(false),
        }
    }

    /// Sends `plan`'s datagrams in batches: the runs of equal-sized
    /// Load PDUs as UDP GSO messages while the kernel and the path take
    /// them, and with sendmmsg without GSO once they don't. Errors are
    /// handled as [`Socket::send_datagram`] handles them, per message; a
    /// message the kernel refuses ends the Send Plan's sending, and what it
    /// and the messages after it hold are Deferred Datagrams.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn send_batched(
        &mut self,
        batches: &mut Batches,
        plan: &(impl Layout + ?Sized),
        buf: &[u8],
        stamped: MonoTime,
        sink: &mut impl Sink,
    ) -> io::Result<Batched> {
        use batch::next_call;
        use classify::FastPath;
        let mut sent = 0u32;
        let mut retried = false;
        batch::group(plan.layout(), batches.gso, &mut batches.messages);
        let mut next = 0;
        while next < batches.messages.len() {
            let (count, segment) = next_call(&batches.messages[next..]);
            let messages = &batches.messages[next..next + count];
            let error = match self
                .adapter
                .send_batch(buf, messages, segment, Some(stamped))
            {
                Ok(0) => break,
                Ok(taken) => {
                    let messages = &batches.messages[next..next + taken];
                    sent += messages.iter().map(|m| m.datagrams).sum::<u32>();
                    next += taken;
                    // The calls the batch went with, as the adapter made
                    // them, which can change as the Test goes.
                    self.fast_paths.sendmmsg |= self.adapter.offers().sendmmsg;
                    self.fast_paths.udp_gso |= segment.is_some();
                    // Fewer than all: the next call sends more, or fails
                    // with the error that stopped this one.
                    continue;
                }
                Err(error) => error,
            };
            let call = Call::SendBatch {
                gso: segment.is_some(),
            };
            match classify(call, &error) {
                Action::Retry => {}
                // GSO can't be used here: the rest go without it, from now
                // on.
                Action::Unsupported(FastPath::UdpGso) => {
                    batches.gso = false;
                    batch::group(
                        plan.layout().skip(sent as usize),
                        false,
                        &mut batches.messages,
                    );
                    next = 0;
                }
                // sendmmsg itself doesn't work here: the rest go one at a
                // time, from now on.
                Action::Unsupported(_) => return Ok(Batched::Refused(sent)),
                // An ICMP error about an earlier datagram: reported, and
                // the batch tried once more (ADR 0006).
                Action::PathError(kind) if !retried => {
                    retried = true;
                    self.path_error(kind, sink);
                }
                // The first message is too large to leave with DF set: it
                // counts as sent and lost, as in `send_datagram`.
                Action::TooLarge => {
                    sink.path_error(TransportError::MsgSize);
                    sent += batches.messages[next].datagrams;
                    next += 1;
                }
                Action::Fatal => return Err(error),
                // Reported again: the rest waits.
                Action::PathError(kind) => {
                    self.path_error(kind, sink);
                    break;
                }
                // A full send buffer (EAGAIN, ENOBUFS): the rest waits.
                Action::Blocked => break,
            }
        }
        Ok(Batched::Sent(sent))
    }

    /// Reports an ICMP-type error the socket returned (ADR 0006): on Linux
    /// each error queued on the socket, by its ICMP type; elsewhere, and if
    /// the queue holds none, `kind`, as the classifier read the error.
    fn path_error(&mut self, kind: TransportError, sink: &mut impl Sink) {
        // Unless the queue holds the error: drained past any send
        // timestamps queued ahead of it, a bounded number of times.
        #[cfg(any(target_os = "linux", target_os = "android"))]
        for _ in 0..16 {
            match self
                .adapter
                .drain_queue(&mut |error| sink.path_error(error))
            {
                (_, true) => return,
                (true, false) => continue,
                (false, false) => break,
            }
        }
        sink.path_error(kind);
    }

    /// Hands every datagram from the peer that has arrived, up to a turn's
    /// budget, to `sink`: what it read, from any source. On Linux they are
    /// read in batches while the kernel takes them; elsewhere, and once
    /// recvmmsg turns out not to work, one at a time. The clock pairs each
    /// read is converted with are `times`' (`engine::times`). It
    /// fails only when the socket is unusable.
    pub(crate) fn receive(
        &mut self,
        times: &impl Times,
        sink: &mut impl Sink,
    ) -> io::Result<Reads> {
        let mut reads = Reads::default();
        #[cfg(any(target_os = "linux", target_os = "android"))]
        if self.batch_reads {
            if self.receive_batched(times, &mut reads, sink)? {
                return Ok(reads);
            }
            // recvmmsg is unusable: one at a time, now and from now on.
            self.batch_reads = false;
        }
        self.receive_each(times, &mut reads, sink)?;
        Ok(reads)
    }

    /// Reads what has arrived in batches, up to the budget, and hands it to
    /// `sink`: whether recvmmsg works here. Each batch gets the clock pairs
    /// from before and after its call, which convert its kernel timestamps
    /// into the Test Time Base (docs/spec.md Time model). Errors are handled as
    /// [`Socket::receive_each`] handles them.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn receive_batched(
        &mut self,
        times: &impl Times,
        reads: &mut Reads,
        sink: &mut impl Sink,
    ) -> io::Result<bool> {
        let mut budget = READ_BUDGET;
        while budget > 0 {
            let max = budget.min(batch_read::BATCH);
            let before = times.before_read();
            let read = self.adapter.read_batch(max);
            let after = times.after_read(*read.as_ref().unwrap_or(&0));
            let count = match read {
                Ok(count) => count,
                Err(error) => match classify(Call::ReadBatch, &error) {
                    Action::Blocked => return Ok(true),
                    Action::Unsupported(_) => return Ok(false),
                    Action::Fatal => return Err(error),
                    // An ICMP-type error from an earlier send (ADR 0006).
                    // One that arrives after some datagrams of a batch ends
                    // the call early with those; the next call reports it.
                    Action::PathError(kind) => {
                        self.path_error(kind, sink);
                        budget -= 1;
                        continue;
                    }
                    Action::Retry | Action::TooLarge => {
                        budget -= 1;
                        continue;
                    }
                },
            };
            self.fast_paths.recvmmsg = true;
            self.deliver_batch(count, before, after, reads, sink);
            budget = budget.saturating_sub(count);
            // Fewer than asked for: the socket had no more.
            if count < max {
                return Ok(true);
            }
        }
        reads.more = true;
        Ok(true)
    }

    /// Hands the `count` datagrams of the last batch from the peer to
    /// `sink`, in order: Status PDUs one by one, with their kernel receive
    /// time, and the Load PDUs between them as one batch each, with the
    /// clock pairs from before and after the call.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn deliver_batch(
        &mut self,
        count: usize,
        before: udpstp_core::Now,
        after: udpstp_core::Now,
        reads: &mut Reads,
        sink: &mut impl Sink,
    ) {
        const EMPTY: RxDatagram = RxDatagram {
            header: [0; 32],
            actual_len: 0,
            kernel_wall: None,
            tos: None,
        };
        let mut loads = [EMPTY; batch_read::BATCH];
        let mut held = 0;
        for index in 0..count {
            let (bytes, received) = self.adapter.batch_datagram(index);
            reads.datagrams += 1;
            reads.bytes += received.len as u64;
            // The Test Connection takes datagrams from its peer only.
            if received.source != Some(self.peer) {
                continue;
            }
            self.fast_paths.kernel_rx_timestamps |= received.kernel_wall.is_some();
            if bytes.starts_with(&STATUS_PDU_ID) {
                load_pdus(&loads[..held], before, after, sink);
                held = 0;
                // A Load Receiver reads only a Status PDU's first bytes;
                // it has no use for one anyway.
                sink.status_pdu(RxPdu {
                    now: after,
                    kernel_wall: received.kernel_wall,
                    pdu: bytes,
                });
                continue;
            }
            let load = &mut loads[held];
            load.header = [0; 32];
            load.header[..bytes.len().min(32)].copy_from_slice(&bytes[..bytes.len().min(32)]);
            // The full length, which a header-only read still reports.
            load.actual_len = received.len;
            load.kernel_wall = received.kernel_wall;
            load.tos = received.tos;
            held += 1;
        }
        load_pdus(&loads[..held], before, after, sink);
    }

    /// Reads what has arrived one datagram at a time, for at most
    /// [`READ_SLICE`], and hands what is from the peer to `sink`, timed by
    /// the read.
    fn receive_each(
        &mut self,
        times: &impl Times,
        reads: &mut Reads,
        sink: &mut impl Sink,
    ) -> io::Result<()> {
        let until = times.latest().mono.as_duration() + READ_SLICE;
        loop {
            let before = times.before_read();
            if before.mono.as_duration() >= until {
                reads.more = true;
                return Ok(());
            }
            let (len, source) = match self.adapter.recv(&mut self.buf) {
                Ok(received) => received,
                Err(error) => match classify(Call::Read, &error) {
                    Action::Blocked => return Ok(()),
                    Action::Fatal => return Err(error),
                    // An ICMP-type error from an earlier send (ADR 0006).
                    Action::PathError(kind) => {
                        self.path_error(kind, sink);
                        continue;
                    }
                    Action::Retry | Action::TooLarge | Action::Unsupported(_) => continue,
                },
            };
            let after = times.after_read(1);
            reads.datagrams += 1;
            reads.bytes += len as u64;
            // The Test Connection takes datagrams from its peer only.
            if source != self.peer {
                continue;
            }
            deliver(&self.buf[..len], before, after, sink);
        }
    }

    /// Waits at most `timeout` for the socket to become readable, when
    /// `readable`, or to have an error pending (ppoll), which it
    /// drains and hands to `sink`, since one left pending would end every
    /// wait at once: how the wait ended, and whether it drained an error;
    /// `None` if the kernel refuses ppoll, so the engine sleeps between
    /// turns from now on. Any other failure is this call's alone: a wake-up
    /// without progress.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub(crate) fn ready(
        &mut self,
        readable: bool,
        timeout: Duration,
        sink: &mut impl Sink,
    ) -> Option<(Woke, bool)> {
        let woke = match self.adapter.ppoll(readable, timeout) {
            Ok(woke) => {
                self.fast_paths.ppoll = true;
                woke
            }
            Err(error) => match classify(Call::Wait, &error) {
                Action::Unsupported(_) => return None,
                _ => Woke::Interrupted,
            },
        };
        let drained = matches!(woke, Woke::Ready { error: true, .. }) && self.drain_errors(sink);
        Some((woke, drained))
    }

    /// Drains the errors pending on the socket, which a wait reported
    /// (POLLERR), and hands them to `sink`: whether there were any. Kernel
    /// send timestamps arrive on the error queue too, and raise
    /// POLLERR after every send while they are asked for.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn drain_errors(&mut self, sink: &mut impl Sink) -> bool {
        if self
            .adapter
            .drain_queue(&mut |error| sink.path_error(error))
            .0
        {
            return true;
        }
        // An error pending on the socket but not queued: what the next read
        // would have returned. Reading it cleared it.
        let Some(error) = self.adapter.pending_error() else {
            return false;
        };
        if let Action::PathError(kind) = classify(Call::Read, &error) {
            sink.path_error(kind);
        }
        true
    }
}

/// Hands the Load PDUs of one read to `sink`, if there are any.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn load_pdus(
    datagrams: &[RxDatagram],
    before: udpstp_core::Now,
    after: udpstp_core::Now,
    sink: &mut impl Sink,
) {
    if !datagrams.is_empty() {
        sink.load_pdus(RxBatch {
            before,
            after,
            datagrams,
        });
    }
}

/// Hands one datagram read on its own to `sink`: a Status PDU whole, and
/// anything else as a Load PDU's header.
fn deliver(
    payload: &[u8],
    before: udpstp_core::Now,
    after: udpstp_core::Now,
    sink: &mut impl Sink,
) {
    if payload.starts_with(&STATUS_PDU_ID) {
        sink.status_pdu(RxPdu {
            now: after,
            kernel_wall: None,
            pdu: payload,
        });
        return;
    }
    let mut header = [0u8; 32];
    let copied = payload.len().min(header.len());
    header[..copied].copy_from_slice(&payload[..copied]);
    let datagram = [RxDatagram {
        header,
        actual_len: payload.len(),
        kernel_wall: None,
        tos: None,
    }];
    sink.load_pdus(RxBatch {
        before,
        after,
        datagrams: &datagram,
    });
}
