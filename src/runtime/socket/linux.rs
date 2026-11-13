//! The Linux adapter of a Test Connection's socket: sendmmsg, with UDP GSO
//! where the kernel knows it (`batch.rs`), header-only recvmmsg with
//! kernel receive timestamps and TOS (`batch_read.rs`), ppoll,
//! the error queue (`errqueue.rs`, ADR 0006), and kernel send
//! timestamps when `UDPSTP_KERNEL_TX_TIMESTAMPS` asks for them
//! (`send_stamps.rs`). With them, every message it sends is counted,
//! and where the kernel takes it each carries its own number, so
//! it goes with a sendmsg call of its own, unless the variable asks for
//! the kernel's own count; where the kernel refuses the numbers after
//! all, messages go without them and the count stops.
//! Otherwise single sends and reads are the portable adapter's.

use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::time::Duration;

use crate::protocol::{MonoTime, TransportError};
use nix::sys::socket::{getsockopt, sockopt};
use socket2::SockAddr;

use super::batch::{self, Message};
use super::batch_read::{self, BatchReader, Received};
use super::classify::{Action, Call, FastPath, classify};
use super::errqueue::{self, Queued};
use super::send_stamps::{self, SendLag, SendStamps};
use super::{Adapter, Offers, Portable, Settings, Woke, overflows};
use crate::runtime::clock::Clock;

pub(crate) struct Linux {
    plain: Portable,
    /// sendmmsg's preallocated headers, on a connected socket.
    sender: Option<batch::Sender>,
    /// Whether the kernel knows UDP GSO.
    udp_gso: bool,
    reader: BatchReader,
    /// How the socket's Receive Buffer Overflows are counted.
    overflows: overflows::Counter,
    /// Kernel send timestamps, when asked for.
    stamps: Option<SendStamps>,
    /// Where a numbered single send goes when the socket isn't connected.
    to: Option<SockAddr>,
    clock: Clock,
}

impl Linux {
    /// Opens the adapter on the Engine Thread: the header sets aren't
    /// `Send`.
    pub(crate) fn open(socket: UdpSocket, settings: &Settings) -> Self {
        let (sender, udp_gso) = if settings.connected {
            (Some(batch::Sender::new()), batch::gso_supported(&socket))
        } else {
            (None, false)
        };
        batch_read::enable(&socket, settings.rx_timestamps);
        let overflows = overflows::Counter::open(&socket);
        let slot = if settings.load_receiver {
            batch_read::HEADER_SLOT
        } else {
            batch_read::STATUS_SLOT
        };
        // A Load Sender's Load PDUs, when asked for.
        let stamps = (settings.send_stamps && !settings.load_receiver)
            .then(|| send_stamps::enable(&socket, settings.explicit_numbers).ok())
            .flatten()
            .map(SendStamps::new);
        Self {
            plain: Portable::open(socket, settings),
            sender,
            udp_gso,
            reader: BatchReader::new(slot),
            overflows,
            stamps,
            to: (!settings.connected).then(|| SockAddr::from(settings.peer)),
            clock: settings.clock,
        }
    }

    /// Sends `messages` from `buf` in one sendmmsg call.
    fn send_mmsg(
        &mut self,
        buf: &[u8],
        messages: &[Message],
        segment: Option<usize>,
    ) -> io::Result<usize> {
        self.sender
            .as_mut()
            .expect("sendmmsg is offered on a connected socket only")
            .send(self.plain.socket(), buf, messages, segment)
    }

    /// The answer to a send whose messages carried their numbers:
    /// where the kernel may have refused the numbers themselves, as the
    /// classifier reads its error (ADR 0006), the send is made again
    /// without them, by `unnumbered`. If that goes, the numbers were what
    /// the kernel refused: messages carry none from then on and no more
    /// lags are counted, rather than an experimental measurement failing
    /// the Test. If it doesn't, its answer stands.
    fn or_unnumbered(
        &mut self,
        numbered: io::Result<usize>,
        unnumbered: impl FnOnce(&mut Self) -> io::Result<usize>,
    ) -> io::Result<usize> {
        let refused = Action::Unsupported(FastPath::CarriedNumbers);
        if !matches!(&numbered, Err(error) if classify(Call::NumberedSend, error) == refused) {
            return numbered;
        }
        let answer = unnumbered(self);
        if answer.is_ok()
            && let Some(stamps) = &mut self.stamps
        {
            stamps.numbers_refused();
        }
        answer
    }
}

impl Adapter for Linux {
    fn send(&mut self, datagram: &[u8], stamped: Option<MonoTime>) -> io::Result<usize> {
        let sent = match self.stamps.as_ref().and_then(SendStamps::carried) {
            Some(number) => {
                let numbered = send_stamps::send_numbered(
                    self.plain.socket(),
                    self.to.as_ref(),
                    datagram,
                    number,
                    None,
                );
                self.or_unnumbered(numbered, |linux| linux.plain.send(datagram))
            }
            None => self.plain.send(datagram),
        };
        if let Some(stamps) = &mut self.stamps {
            stamps.sent(1, usize::from(sent.is_ok()), stamped);
        }
        sent
    }

    fn recv(&mut self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        // Without the Receive Buffer Overflows the kernel may attach.
        self.overflows.read_alone();
        self.plain.recv(buf)
    }

    fn offers(&self) -> Offers {
        let batches = self.sender.is_some();
        Offers {
            batches,
            // Unless each message carries its own number.
            sendmmsg: batches && self.stamps.as_ref().and_then(SendStamps::carried).is_none(),
            udp_gso: self.udp_gso,
        }
    }

    fn send_batch(
        &mut self,
        buf: &[u8],
        messages: &[Message],
        segment: Option<usize>,
        stamped: Option<MonoTime>,
    ) -> io::Result<usize> {
        let taken = match self.stamps.as_ref().and_then(SendStamps::carried) {
            Some(first) => {
                let numbered = send_stamps::send_batch_numbered(
                    self.plain.socket(),
                    buf,
                    messages,
                    segment,
                    first,
                );
                self.or_unnumbered(numbered, |linux| linux.send_mmsg(buf, messages, segment))
            }
            None => self.send_mmsg(buf, messages, segment),
        };
        if let Some(stamps) = &mut self.stamps {
            let offered = messages.len().min(batch::BATCH);
            stamps.sent(offered, *taken.as_ref().unwrap_or(&0), stamped);
        }
        taken
    }

    fn read_batch(&mut self, max: usize) -> io::Result<usize> {
        self.reader.read(self.plain.socket(), max)
    }

    fn batch_datagram(&self, index: usize) -> (&[u8], Received) {
        self.reader.datagram(index)
    }

    fn ppoll(&mut self, readable: bool, timeout: Duration) -> io::Result<Woke> {
        ppoll(self.plain.socket(), readable, timeout)
    }

    fn drain_queue(&mut self, each: &mut impl FnMut(TransportError)) -> (bool, bool) {
        let (stamps, clock) = (&mut self.stamps, self.clock);
        let mut errors = false;
        let any = errqueue::drain_with(self.plain.socket(), |queued| match queued {
            Queued::Error(error) => {
                errors = true;
                each(error);
            }
            Queued::Sent { key, wall } => {
                if let Some(stamps) = stamps {
                    stamps.stamped(key, wall, clock.now());
                }
            }
        });
        (any, errors)
    }

    fn pending_error(&mut self) -> Option<io::Error> {
        match getsockopt(self.plain.socket(), sockopt::SocketError) {
            Ok(0) | Err(_) => None,
            Ok(code) => Some(io::Error::from_raw_os_error(code)),
        }
    }

    fn receive_buffer_overflows(&self) -> Option<u64> {
        let attached = self.reader.overflows();
        self.overflows.count(self.plain.socket(), attached)
    }

    fn socket(&self) -> &UdpSocket {
        self.plain.socket()
    }

    fn overflow_count(&self) -> super::OverflowCount {
        match self.overflows {
            overflows::Counter::Listed => super::OverflowCount::Listed,
            counter => super::OverflowCount::Known(
                counter.count(self.plain.socket(), self.reader.overflows()),
            ),
        }
    }

    fn send_lag(&self) -> Option<SendLag> {
        self.stamps.as_ref().and_then(SendStamps::lag)
    }
}

/// Waits on `socket` for at most `timeout`: for it to become readable
/// when `readable`, and in any case for an error to be pending (POLLERR,
/// which ppoll always reports).
fn ppoll(socket: &UdpSocket, readable: bool, timeout: Duration) -> io::Result<Woke> {
    use std::os::fd::AsFd;

    use nix::poll::{PollFd, PollFlags};
    use nix::sys::time::TimeSpec;

    let events = if readable {
        PollFlags::POLLIN
    } else {
        PollFlags::empty()
    };
    let mut fds = [PollFd::new(socket.as_fd(), events)];
    match nix::poll::ppoll(&mut fds, Some(TimeSpec::from_duration(timeout)), None) {
        Ok(0) => Ok(Woke::TimedOut),
        Ok(_) => {
            let revents = fds[0].revents().unwrap_or_else(PollFlags::empty);
            Ok(Woke::Ready {
                readable: revents.intersects(PollFlags::POLLIN),
                error: revents.intersects(PollFlags::POLLERR),
            })
        }
        Err(errno) => {
            let error = io::Error::from(errno);
            if error.kind() == io::ErrorKind::Interrupted {
                Ok(Woke::Interrupted)
            } else {
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::protocol::{RxBatch, RxPdu};

    use super::*;
    use crate::runtime::socket::faults::{Faults, Faulty};
    use crate::runtime::socket::{FastPaths, Platform, Sink, Socket};

    /// Keeps the path errors it hears; nothing is read here.
    #[derive(Default)]
    struct Heard(Vec<TransportError>);

    impl Sink for Heard {
        fn status_pdu(&mut self, _: RxPdu<'_>) {
            unreachable!("nothing is read")
        }

        fn load_pdus(&mut self, _: RxBatch<'_>) {
            unreachable!("nothing is read")
        }

        fn path_error(&mut self, error: TransportError) {
            self.0.push(error);
        }
    }

    /// A Load Sender's socket over loopback, connected, asking for kernel
    /// send timestamps, over the fault adapter with `faults`: the socket, a
    /// second handle on it, its peer's socket, which never reads, and its
    /// clock, past the 9 ms [`send_plans`] stamps plans back.
    fn stamped_socket(faults: &str) -> (Socket<Faulty<Linux>>, UdpSocket, UdpSocket, Clock) {
        let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
        let peer = receiver.local_addr().unwrap();
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.connect(peer).unwrap();
        socket.set_nonblocking(true).unwrap();
        let handle = socket.try_clone().unwrap();
        let clock = Clock::new();
        let settings = Settings {
            peer,
            connected: true,
            load_receiver: false,
            rx_timestamps: true,
            send_stamps: true,
            explicit_numbers: true,
            clock,
        };
        let faulty = Faulty {
            inner: Platform,
            faults: Faults::parse(faults),
        };
        let socket = Socket::open(faulty, socket, &settings);
        // The Test Time Base starts with the clock.
        std::thread::sleep(Duration::from_millis(10));
        (socket, handle, receiver, clock)
    }

    /// Sends Send Plans `plans` of one Load PDU each. Plan i's lpduTime is
    /// stamped i ms before its send, so a lag shows which plan it was
    /// matched to; a plan the kernel didn't take goes again, stamped
    /// afresh, as its Deferred Datagram would, up to 100 times before the
    /// test fails rather than hangs. After each send the socket drains its
    /// error queue, as the engine's wait does.
    fn send_plans(
        socket: &mut Socket<Faulty<Linux>>,
        clock: &Clock,
        plans: std::ops::Range<u64>,
        heard: &mut Heard,
    ) {
        let (plan, buf) = ([(0, 1222)], [0u8; 1222]);
        for index in plans {
            let taken = (0..100).any(|_| {
                let now = clock.now().mono.as_duration();
                let stamped =
                    MonoTime::from_duration(now.saturating_sub(Duration::from_millis(index)));
                let sent = socket.send_plan(&plan[..], &buf, stamped, heard).unwrap();
                socket.ready(false, Duration::ZERO, heard);
                sent == 1
            });
            assert!(taken, "plan {index} wasn't taken in 100 sends");
        }
    }

    /// Ten Send Plans sent as [`send_plans`] sends them, over the fault
    /// adapter with `faults`: the lags matched, and the fast paths the
    /// socket used.
    fn send_ten_plans(faults: &str) -> (SendLag, FastPaths) {
        let (mut socket, _handle, _receiver, clock) = stamped_socket(faults);
        let mut heard = Heard::default();
        send_plans(&mut socket, &clock, 0..10, &mut heard);
        assert!(heard.0.is_empty(), "no path errors: {:?}", heard.0);
        let lag = socket.send_lag().expect("timestamps came back");
        (lag, socket.fast_paths())
    }

    /// Turns OPT_ID off under the socket, so that the kernel refuses the
    /// number a message carries with EINVAL, as a kernel refuses
    /// SCM_TS_OPT_ID where it doesn't take it.
    fn refuse_numbers(handle: &UdpSocket) {
        use nix::sys::socket::{TimestampingFlag, setsockopt};
        let flags = TimestampingFlag::SOF_TIMESTAMPING_TX_SOFTWARE
            | TimestampingFlag::SOF_TIMESTAMPING_SOFTWARE
            | TimestampingFlag::SOF_TIMESTAMPING_OPT_TSONLY;
        setsockopt(handle, sockopt::Timestamping, &flags).unwrap();
    }

    //  (kernel send timestamps matched to the right Load PDUs after a
    // failed send; real sockets and the fault adapter): where the messages
    // carry their own numbers, a queue discipline's drop of a message the
    // kernel numbered, a real refusal, shifts nothing. A shift would leave
    // a plan unmatched; here every plan is matched, each lag its own
    // plan's. Each message went with a sendmsg call of its own, so no
    // sendmmsg call is reported.
    #[test]
    fn send_timestamps_stay_matched_after_a_numbered_message_is_dropped() {
        if !send_stamps::kernel_takes_numbers() {
            // The kernel numbers the messages: the test below.
            return;
        }
        let (lag, paths) = send_ten_plans("qdisc_drop_every=6");
        assert_eq!(lag.samples, 10, "every plan, none shifted: {lag:?}");
        assert!(lag.p50 >= Duration::from_millis(4), "{lag:?}");
        assert!(lag.max >= Duration::from_millis(9), "{lag:?}");
        assert!(!paths.sendmmsg, "{paths:?}");
    }

    // , : where the kernel numbers the messages (before Linux 6.13;
    // forced here), it numbered the message dropped, so the count stops
    // there: only the five plans before it are matched. The batches went
    // with sendmmsg.
    #[test]
    fn with_the_kernels_numbers_the_count_stops_at_a_numbered_drop() {
        let (lag, paths) = send_ten_plans("qdisc_drop_every=6,ts_opt_id_unsupported=1");
        assert_eq!(lag.samples, 5, "the plans before the drop: {lag:?}");
        assert!(lag.max >= Duration::from_millis(4), "{lag:?}");
        assert!(paths.sendmmsg, "{paths:?}");
    }

    // : where the kernel refuses the numbers messages carry (EINVAL,
    // as it refuses SCM_TS_OPT_ID it doesn't take), a Send Plan's batch
    // goes again without them, and so does every message after it: the
    // sends go on, now in sendmmsg calls, which the socket reports, and
    // the lags counted before stand, with none after
    #[test]
    fn a_batch_whose_numbers_are_refused_goes_without_them() {
        if !send_stamps::kernel_takes_numbers() {
            return;
        }
        let (mut socket, handle, _receiver, clock) = stamped_socket("");
        let mut heard = Heard::default();
        send_plans(&mut socket, &clock, 0..3, &mut heard);
        assert!(
            !socket.fast_paths().sendmmsg,
            "numbered, a sendmsg call each"
        );
        refuse_numbers(&handle);
        send_plans(&mut socket, &clock, 3..6, &mut heard);
        assert!(heard.0.is_empty(), "no path errors: {:?}", heard.0);
        assert_eq!(socket.send_lag().map(|lag| lag.samples), Some(3));
        assert!(socket.fast_paths().sendmmsg, "then in sendmmsg calls");
    }

    // : the same for a datagram of no Send Plan, sent on its own
    #[test]
    fn a_datagram_whose_number_is_refused_goes_without_it() {
        if !send_stamps::kernel_takes_numbers() {
            return;
        }
        let (mut socket, handle, _receiver, clock) = stamped_socket("");
        let mut heard = Heard::default();
        send_plans(&mut socket, &clock, 0..2, &mut heard);
        refuse_numbers(&handle);
        assert!(socket.send(&[0; 64], &mut heard).unwrap(), "it went");
        send_plans(&mut socket, &clock, 2..4, &mut heard);
        assert!(heard.0.is_empty(), "no path errors: {:?}", heard.0);
        assert_eq!(socket.send_lag().map(|lag| lag.samples), Some(2));
        assert!(
            socket.fast_paths().sendmmsg,
            "the plans after, in sendmmsg calls"
        );
    }

    // , ADR 0006: only an error the classifier reads as the numbers'
    // refusal makes the adapter send again without them. Any other error
    // stands, and the messages keep their numbers; the refusal, where the
    // send then goes without them, ends the numbering, and where it fails
    // too, the numbers weren't what the kernel refused, and they stay.
    #[test]
    fn only_the_numbers_refusal_sends_without_them() {
        use nix::errno::Errno;

        use crate::runtime::socket::send_stamps::Numbering;

        let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
        let settings = Settings {
            peer: receiver.local_addr().unwrap(),
            connected: false,
            load_receiver: false,
            rx_timestamps: true,
            send_stamps: false,
            explicit_numbers: true,
            clock: Clock::new(),
        };
        let mut linux = Linux::open(UdpSocket::bind("127.0.0.1:0").unwrap(), &settings);
        // Messages that carry their numbers, whatever this kernel takes.
        linux.stamps = Some(SendStamps::new(Numbering::Explicit));
        let carried = |linux: &Linux| linux.stamps.as_ref().and_then(SendStamps::carried);
        let first = carried(&linux);
        assert!(first.is_some());
        let refused = |errno: Errno| -> io::Result<usize> { Err(errno.into()) };

        let mut again = false;
        let answer = linux.or_unnumbered(refused(Errno::ENOBUFS), |_| {
            again = true;
            Ok(1)
        });
        let errno = answer.unwrap_err().raw_os_error();
        assert_eq!(errno, Some(Errno::ENOBUFS as i32), "the error stands");
        assert!(!again, "not sent again");
        assert_eq!(carried(&linux), first, "the numbers stay");

        let answer = linux.or_unnumbered(refused(Errno::EINVAL), |_| refused(Errno::EINVAL));
        assert!(answer.is_err(), "sent again, and failed too");
        assert_eq!(carried(&linux), first, "the numbers weren't refused");

        let answer = linux.or_unnumbered(refused(Errno::EINVAL), |_| Ok(1));
        assert_eq!(answer.unwrap(), 1, "sent again without them");
        assert_eq!(carried(&linux), None, "no numbers from now on");
    }
}
