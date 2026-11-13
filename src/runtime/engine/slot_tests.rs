//! Which reading of the clock each core call a Test Connection slot makes
//! is given: the slot over a scripted clock,
//! which logs every reading it gives, and a recording of its Data Phase,
//! which logs every core call that takes the time with the reading it
//! got, in one log. The socket is a scripted wire. The plan-executor
//! contract tolerates far larger timing changes than a stale reading
//! makes, so this is what pins the readings.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::protocol::{
    DataEvent, DataPhase, Direction, MonoTime, Now, RxBatch, RxPdu, SendPlan, SocketId,
    TransportError, WallTime,
};

use super::slot::{Core, Slot};
use super::times::{Fresh, Times, Turn};
use crate::runtime::clock::{Clock, Reading};
use crate::runtime::contract::activate;
use crate::runtime::socket::{Adapter, Open, Settings, Socket};

/// Where the scripted wall clock starts: 2027, within any key's lifetime.
const WALL: Duration = Duration::from_secs(1_800_000_000);

/// A reading given, or a core call made with the time, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Entry {
    /// A clock pair was read: its Test Time Base, in µs.
    Pair(u64),
    /// The Test Time Base alone was read, in µs.
    Mono(u64),
    /// A core call, with the Test Time Base of the reading it was given,
    /// in µs.
    Call(&'static str, u64),
    /// A batch of Load PDUs handed on, with its pairs from before and
    /// after the read.
    Batch { before: u64, after: u64 },
}

type Log = Rc<RefCell<Vec<Entry>>>;

fn micros(at: MonoTime) -> u64 {
    at.as_duration().as_micros() as u64
}

/// A clock whose every reading is 1 µs after the one before, and whose
/// wall clock steps 1 ms at each pair read, so a reading carried on from
/// an earlier pair is told apart from a fresh pair by its wall clock.
struct Scripted {
    mono: Cell<Duration>,
    pairs: Cell<u32>,
    log: Log,
}

impl Scripted {
    fn new(log: &Log) -> Self {
        Self {
            mono: Cell::new(Duration::from_secs(1)),
            pairs: Cell::new(0),
            log: log.clone(),
        }
    }

    /// The time now, unlogged and without moving on: for the Control Phase
    /// that sets the Test up.
    fn quiet(&self) -> Now {
        Now {
            mono: MonoTime::from_duration(self.mono.get()),
            wall: WallTime::from_unix(
                WALL + self.mono.get() + Duration::from_millis(self.pairs.get().into()),
            ),
        }
    }

    fn set(&self, at: MonoTime) {
        self.mono.set(at.as_duration());
    }

    fn tick(&self) -> MonoTime {
        self.mono.set(self.mono.get() + Duration::from_micros(1));
        MonoTime::from_duration(self.mono.get())
    }
}

impl Reading for &Scripted {
    fn now(&self) -> Now {
        let mono = self.tick();
        self.pairs.set(self.pairs.get() + 1);
        self.log.borrow_mut().push(Entry::Pair(micros(mono)));
        self.quiet()
    }

    fn mono(&self) -> MonoTime {
        let mono = self.tick();
        self.log.borrow_mut().push(Entry::Mono(micros(mono)));
        mono
    }
}

/// A Data Phase whose core calls that take the time are logged with the
/// reading each got, and the readings themselves kept whole.
struct Recorded {
    inner: DataPhase,
    log: Log,
    given: Vec<Now>,
}

impl Recorded {
    fn call(&mut self, name: &'static str, now: Now) {
        self.log
            .borrow_mut()
            .push(Entry::Call(name, micros(now.mono)));
        self.given.push(now);
    }
}

impl Core for Recorded {
    fn is_load_receiver(&self) -> bool {
        self.inner.is_load_receiver()
    }
    fn take_load_bytes(&mut self) -> u64 {
        self.inner.take_load_bytes()
    }
    fn stop(&mut self, now: Now) {
        self.call("stop", now);
        self.inner.stop(now);
    }

    fn poll_burst(&mut self, now: Now, horizon: Duration) -> Option<SendPlan> {
        self.call("poll_burst", now);
        self.inner.poll_burst(now, horizon)
    }

    fn write_datagrams(&mut self, plan: &SendPlan, now: Now, buf: &mut [u8]) {
        self.call("write_datagrams", now);
        self.inner.write_datagrams(plan, now, buf);
    }

    fn report_sent(&mut self, plan: &SendPlan, sent: u32, now: Now) {
        self.call("report_sent", now);
        self.inner.report_sent(plan, sent, now);
    }

    fn handle_load_batch(&mut self, batch: RxBatch<'_>) {
        self.log.borrow_mut().push(Entry::Batch {
            before: micros(batch.before.mono),
            after: micros(batch.after.mono),
        });
        self.given.extend([batch.before, batch.after]);
        self.inner.handle_load_batch(batch);
    }

    fn handle_pdu(&mut self, pdu: RxPdu<'_>) {
        self.call("handle_pdu", pdu.now);
        self.inner.handle_pdu(pdu);
    }

    fn handle_transport_error(&mut self, now: Now, error: TransportError) {
        self.call("handle_transport_error", now);
        self.inner.handle_transport_error(now, error);
    }

    fn handle_timeout(&mut self, now: Now) {
        self.call("handle_timeout", now);
        self.inner.handle_timeout(now);
    }

    fn poll_transmit(&mut self, now: Now, buf: &mut [u8]) -> Option<usize> {
        self.call("poll_transmit", now);
        self.inner.poll_transmit(now, buf)
    }

    fn poll_event(&mut self) -> Option<DataEvent> {
        self.inner.poll_event()
    }

    fn next_burst_at(&self) -> Option<MonoTime> {
        self.inner.next_burst_at()
    }

    fn poll_timeout(&self) -> Option<MonoTime> {
        self.inner.poll_timeout()
    }
}

/// A wire the socket sends onto and reads from as a test scripts it: one
/// datagram at a time, or in batches on Linux, as the kernel would give
/// them, all from the peer.
struct Wire {
    fail_after: Option<(usize, bool)>,
    peer: SocketAddr,
    inbox: Arc<Mutex<VecDeque<Vec<u8>>>>,
    sent: Arc<Mutex<Vec<Vec<u8>>>>,
    /// The last batch read (Linux).
    #[cfg(any(target_os = "linux", target_os = "android"))]
    batch: Vec<(Vec<u8>, crate::runtime::socket::batch_read::Received)>,
}

impl Open for Wire {
    type Adapter = Wire;

    fn open(self, _: UdpSocket, _: &Settings) -> Wire {
        self
    }
}

impl Adapter for Wire {
    fn send(&mut self, datagram: &[u8], _: Option<MonoTime>) -> io::Result<usize> {
        if let Some((limit, panic)) = self.fail_after
            && self.sent.lock().unwrap().len() >= limit
        {
            assert!(!panic, "injected send panic");
            return Err(io::ErrorKind::InvalidInput.into());
        }
        self.sent.lock().unwrap().push(datagram.to_vec());
        Ok(datagram.len())
    }

    fn recv(&mut self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        let datagram = self
            .inbox
            .lock()
            .unwrap()
            .pop_front()
            .ok_or(io::ErrorKind::WouldBlock)?;
        buf[..datagram.len()].copy_from_slice(&datagram);
        Ok((datagram.len(), self.peer))
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn offers(&self) -> crate::runtime::socket::Offers {
        crate::runtime::socket::Offers::default()
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn send_batch(
        &mut self,
        _: &[u8],
        _: &[crate::runtime::socket::batch::Message],
        _: Option<usize>,
        _: Option<MonoTime>,
    ) -> io::Result<usize> {
        unreachable!("no batches are offered")
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn read_batch(&mut self, max: usize) -> io::Result<usize> {
        let mut inbox = self.inbox.lock().unwrap();
        if inbox.is_empty() {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        self.batch.clear();
        while self.batch.len() < max
            && let Some(datagram) = inbox.pop_front()
        {
            let received = crate::runtime::socket::batch_read::Received {
                len: datagram.len(),
                source: Some(self.peer),
                ..Default::default()
            };
            self.batch.push((datagram, received));
        }
        Ok(self.batch.len())
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn batch_datagram(
        &self,
        index: usize,
    ) -> (&[u8], crate::runtime::socket::batch_read::Received) {
        let (datagram, received) = &self.batch[index];
        (datagram, *received)
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn ppoll(&mut self, _: bool, _: Duration) -> io::Result<crate::runtime::socket::Woke> {
        Ok(crate::runtime::socket::Woke::TimedOut)
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

/// A Downstream Fixed-Rate Test at `row`, set up at the scripted clock's
/// time: the Server's slot, the Load Sender, on a scripted wire, with the
/// wire's inbox; the clock stands at its first Burst and the log is empty.
struct Sender {
    slot: Slot<Wire, Recorded>,
    inbox: Arc<Mutex<VecDeque<Vec<u8>>>>,
    sent: Arc<Mutex<Vec<Vec<u8>>>>,
}

fn load_sender(clock: &Scripted, log: &Log, row: u16) -> Sender {
    load_sender_reporting(clock, log, row, super::reports::channel().0, None)
}

fn load_sender_reporting(
    clock: &Scripted,
    log: &Log,
    row: u16,
    report: super::reports::Sender,
    fail_after: Option<(usize, bool)>,
) -> Sender {
    let client: SocketAddr = "127.0.0.1:40001".parse().unwrap();
    let test_port: SocketAddr = "127.0.0.1:40002".parse().unwrap();
    let (_, server, _) = activate(
        || clock.quiet(),
        Direction::Downstream,
        row,
        client,
        test_port,
    );
    let first = server
        .next_burst_at()
        .expect("a Load Sender has a Burst due");
    clock.set(first);
    let inbox = Arc::new(Mutex::new(VecDeque::new()));
    let sent = Arc::new(Mutex::new(Vec::new()));
    let wire = Wire {
        fail_after,
        peer: client,
        inbox: inbox.clone(),
        sent: sent.clone(),
        #[cfg(any(target_os = "linux", target_os = "android"))]
        batch: Vec::new(),
    };
    let settings = Settings {
        peer: client,
        connected: true,
        load_receiver: false,
        rx_timestamps: true,
        send_stamps: false,
        explicit_numbers: true,
        clock: Clock::new(),
    };
    let socket = Socket::open(wire, UdpSocket::bind("127.0.0.1:0").unwrap(), &settings);
    let recorded = Recorded {
        inner: server,
        log: log.clone(),
        given: Vec::new(),
    };
    log.borrow_mut().clear();
    Sender {
        slot: Slot::new(SocketId(2), recorded, socket, report),
        inbox,
        sent,
    }
}

// Spec story 73. Final accounting must precede Admission release even
// when unwinding or an I/O error prevents the usual forwarding turn.
#[test]
fn dropping_a_slot_charges_its_actual_partial_transfer_once() {
    for forwarded in [false, true] {
        for unwind in [false, true] {
            let log: Log = Rc::default();
            let clock = Scripted::new(&log);
            let (report, mut received) = super::reports::channel();
            let mut sender = load_sender_reporting(&clock, &log, 4, report.clone(), None);
            let times = Fresh(&clock);
            sender
                .slot
                .send_due(
                    &times,
                    clock.quiet().mono.as_duration() + Duration::from_millis(1),
                )
                .unwrap();
            let bytes: usize = sender.sent.lock().unwrap().iter().map(Vec::len).sum();
            assert!(bytes > 0);
            if forwarded {
                sender.slot.forward_events(&report);
            }
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                let _sender = sender;
                if unwind {
                    panic!("injected executor failure");
                }
            }));
            let mut charged = Vec::new();
            while let Ok(message) = received.try_recv() {
                if let super::FromEngine::Event {
                    event: DataEvent::LoadBytes { bytes },
                    ..
                } = message
                {
                    charged.push(bytes);
                }
            }
            assert_eq!(charged, [bytes as u64]);
        }
    }
}

#[test]
fn a_failed_send_plan_keeps_its_successfully_sent_prefix() {
    for panic in [false, true] {
        let log: Log = Rc::default();
        let clock = Scripted::new(&log);
        let (report, mut received) = super::reports::channel();
        let sender = load_sender_reporting(&clock, &log, 40, report, Some((1, panic)));
        let sent = sender.sent.clone();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let mut sender = sender;
            let times = Fresh(&clock);
            sender.slot.send_due(
                &times,
                clock.quiet().mono.as_duration() + Duration::from_millis(1),
            )
        }));
        assert!(outcome.is_err() || outcome.unwrap().is_err());
        let bytes: usize = sent.lock().unwrap().iter().map(Vec::len).sum();
        assert!(bytes > 0);
        let mut charged = 0;
        while let Ok(super::FromEngine::Event { event, .. }) = received.try_recv() {
            if let DataEvent::LoadBytes { bytes } = event {
                charged += bytes;
            }
        }
        assert_eq!(charged, bytes as u64);
    }
}

/// The entries logged since the last call, taken.
fn taken(log: &Log) -> Vec<Entry> {
    std::mem::take(&mut *log.borrow_mut())
}

/// A Status PDU's first bytes, and a Load PDU's, as the wire brings them:
/// the core reads them as invalid, which is no matter here.
const STATUS_PDU: [u8; 64] = {
    let mut pdu = [0; 64];
    pdu[0] = 0xFE;
    pdu[1] = 0xED;
    pdu
};
const LOAD_PDU: [u8; 64] = {
    let mut pdu = [0; 64];
    pdu[0] = 0xBE;
    pdu[1] = 0xEF;
    pdu
};

use Entry::{Batch, Call, Mono, Pair};

// : in a turn that serves many slots, the turn reads one
// clock pair; the core gets a fresh read of the Test Time Base alone
// before a slot's first poll_burst, before each Send Plan's
// write_datagrams and for its report_sent, and before each timeout and
// Status PDU, with the wall clock carried on from the turn's pair; a later
// poll_burst of the same sending takes the latest reading. A read of the
// socket that returns nothing reads no clock; one that returns datagrams
// reads a pair after it, which times a Status PDU and closes a Load PDU
// batch whose other end is the latest pair before it.
#[test]
fn a_turn_reads_the_clock_where_the_core_judges_or_stamps_time() {
    let log: Log = Rc::default();
    let clock = Scripted::new(&log);
    let mut sender = load_sender(&clock, &log, 4);
    let t = micros(MonoTime::from_duration(clock.mono.get()));
    let turn = Turn::start(&clock);
    // The turn's pair, which later readings carry the wall clock on from.
    let started = turn.before_read();
    let slice = Duration::from_millis(1);
    let until = turn.latest().mono.as_duration() + slice;

    sender.slot.send_due(&turn, until).unwrap();
    assert_eq!(
        taken(&log),
        [
            Pair(t + 1),
            Mono(t + 2),
            Call("poll_burst", t + 2),
            Mono(t + 3),
            Call("write_datagrams", t + 3),
            Mono(t + 4),
            Call("report_sent", t + 4),
            // Row 4's next Burst is 1 ms away.
            Call("poll_burst", t + 4),
        ]
    );
    assert_eq!(sender.sent.lock().unwrap().len(), 1, "one Load PDU went");

    // Nothing arrived: no clock is read.
    sender.slot.read(&turn).unwrap();
    assert_eq!(taken(&log), []);

    // A Status PDU, then a Load PDU: a pair after each read.
    sender.inbox.lock().unwrap().push_back(STATUS_PDU.to_vec());
    sender.slot.read(&turn).unwrap();
    assert_eq!(taken(&log), [Pair(t + 5), Call("handle_pdu", t + 5)]);
    sender.inbox.lock().unwrap().push_back(LOAD_PDU.to_vec());
    sender.slot.read(&turn).unwrap();
    assert_eq!(
        taken(&log),
        [
            Pair(t + 6),
            Batch {
                before: t + 5,
                after: t + 6
            }
        ]
    );

    // No timeout is due by the latest reading: no clock is read for it.
    sender.slot.timeouts(&turn);
    sender.slot.transmit_status(&turn).unwrap();
    assert_eq!(taken(&log), [Mono(t + 7), Call("poll_transmit", t + 7)]);

    // The core's readings of the Test Time Base alone carry the wall clock
    // on from the turn's pair; the pairs are the clock's own.
    let given = &sender.slot.data_phase().given;
    for now in given {
        let mono = micros(now.mono);
        let fresh_pair = [t + 1, t + 5, t + 6].contains(&mono);
        let carried =
            started.wall.as_unix() + (now.mono.as_duration() - started.mono.as_duration());
        if fresh_pair {
            assert_ne!(now.wall.as_unix(), carried, "a pair at {mono} µs");
        } else if mono > t + 1 {
            assert_eq!(now.wall.as_unix(), carried, "a reading at {mono} µs");
        }
    }
}

//  (ADR 0012): in a catch-up, each later Burst's
// poll_burst takes the latest reading, the one its Send Plan was reported
// with, and each Send Plan is still stamped with a fresh one.
#[test]
fn a_catch_up_judges_each_later_burst_by_the_latest_reading() {
    let log: Log = Rc::default();
    let clock = Scripted::new(&log);
    // Row 200 sends a Burst every 100 µs.
    let mut sender = load_sender(&clock, &log, 200);
    // Two Bursts are due: the first, 110 µs late, and the one after it,
    // 10 µs late; both within the catch-up budget.
    clock.set(MonoTime::from_duration(
        clock.mono.get() + Duration::from_micros(110),
    ));
    let t = micros(MonoTime::from_duration(clock.mono.get()));
    let turn = Turn::start(&clock);
    let until = turn.latest().mono.as_duration() + Duration::from_millis(1);
    sender.slot.send_due(&turn, until).unwrap();
    assert_eq!(
        taken(&log),
        [
            Pair(t + 1),
            Mono(t + 2),
            Call("poll_burst", t + 2),
            Mono(t + 3),
            Call("write_datagrams", t + 3),
            Mono(t + 4),
            Call("report_sent", t + 4),
            // The next Burst is due: what arrived is read first, and
            // nothing had, so no clock is read.
            Call("poll_burst", t + 4),
            Mono(t + 5),
            Call("write_datagrams", t + 5),
            Mono(t + 6),
            Call("report_sent", t + 6),
            Call("poll_burst", t + 6),
        ]
    );
}

// : the per-Test-Connection executor reads a fresh clock pair for
// every reading, before and after every read of the socket too, as it
// always has: the Client, and every endpoint on Windows and macOS, read
// the clock as before.
#[test]
fn the_per_test_connection_executor_reads_a_fresh_pair_each_time() {
    let log: Log = Rc::default();
    let clock = Scripted::new(&log);
    let mut sender = load_sender(&clock, &log, 4);
    let t = micros(MonoTime::from_duration(clock.mono.get()));
    let fresh = Fresh(&clock);
    let until = fresh.now().mono.as_duration() + Duration::from_millis(1);
    sender.slot.send_due(&fresh, until).unwrap();
    sender.slot.read(&fresh).unwrap();
    sender.slot.timeouts(&fresh);
    sender.slot.transmit_status(&fresh).unwrap();
    assert_eq!(
        taken(&log),
        [
            // The turn's sending time.
            Pair(t + 1),
            Pair(t + 2),
            Call("poll_burst", t + 2),
            Pair(t + 3),
            Call("write_datagrams", t + 3),
            Pair(t + 4),
            Call("report_sent", t + 4),
            // Whether the sending time is spent, then the next poll_burst.
            Pair(t + 5),
            Pair(t + 6),
            Call("poll_burst", t + 6),
            // Around the read that found nothing.
            Pair(t + 7),
            Pair(t + 8),
            // The timeout, not due, and the Status PDUs, none due.
            Pair(t + 9),
            Pair(t + 10),
            Call("poll_transmit", t + 10),
        ]
    );
}

// : in a turn, whether the Data Phase's timeout is due is judged by the
// latest reading, which reads no clock; a due one is handled with a fresh
// read of the Test Time Base.
#[test]
fn a_turn_reads_the_clock_for_a_timeout_only_when_it_is_due() {
    let log: Log = Rc::default();
    let clock = Scripted::new(&log);
    let mut sender = load_sender(&clock, &log, 4);
    let due = sender
        .slot
        .data_phase()
        .inner
        .poll_timeout()
        .expect("a Load Sender times its Status PDUs out");
    clock.set(due);
    let t = micros(due);
    let turn = Turn::start(&clock);
    sender.slot.timeouts(&turn);
    assert_eq!(
        taken(&log),
        [Pair(t + 1), Mono(t + 2), Call("handle_timeout", t + 2)]
    );
}
