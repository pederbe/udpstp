//! One Engine Thread of the pooled executor: many Test
//! Connections' slots, every socket in one edge-triggered epoll set, a
//! deadline heap and a ready list, served from one loop.
//!
//! A turn, after a wake-up:
//!
//! 1. One clock pair (`times::Turn`): the turn's, which every later reading
//!    of the Test Time Base alone carries the wall clock on from.
//! 2. **Commands,** every turn, without a system call: new Test
//!    Connections, opened here (nix's header sets aren't `Send`), added to
//!    the epoll set, keyed in the heap and put on the ready list for what
//!    arrived before they joined; and stops.
//! 3. **Readiness,** harvested from the epoll set with a zero timeout,
//!    unless a hold-off runs or the wait before timed out with nothing
//!    pending: a socket that signalled goes on the ready list, one with an
//!    error pending (EPOLLERR) marked so. A hold-off's end always
//!    harvests.
//! 4. **Sends,** earliest deadline first, every slot whose next Burst is
//!    due, for at most one slice (1 ms) in all: once it is spent the rest
//!    wait for the next turn, which starts without a wait.
//! 5. **Reads,** unless a hold-off runs: each slot on the ready list as the
//!    turn started, its errors drained first, then up to its budget; one
//!    whose budget ran out goes to the back for the next turn.
//! 6. **Timeouts and Status PDUs** of every slot the turn touched: a Load
//!    Receiver whose timeout is due, and which wasn't read this turn, is
//!    read first, so its Trial Interval closes with what arrived.
//! 7. **Events,** passed on; a slot whose Data Phase ended leaves.
//! 8. **The heap,** each slot touched re-keyed at its next deadline.
//! 9. **The wait** (`schedule::plan_wait`), from a fresh reading: `ppoll`
//!    on the epoll fd, unless a hold-off runs, and the eventfd, until the
//!    deadline or the hold-off's end, at nanosecond resolution. Where the
//!    kernel refuses `ppoll`, and while the spin guard backs off, it
//!    sleeps instead, at most one slice or the guard's sleep, and harvests
//!    after each.
//!
//! Each slot's work runs inside a `catch_unwind` of its own: a panic
//! retires that Test Connection only, out of the epoll set, the heap and
//! the ready list, and reports its end. The Engine Thread's own state is
//! touched only outside those closures.

use std::io;
use std::os::fd::AsFd;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Duration;

use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags};
use nix::sys::epoll::{Epoll, EpollEvent, EpollFlags, EpollTimeout};
use nix::sys::time::TimeSpec;
use tokio::sync::mpsc as reports;
use udpstp_core::{EndReason, MonoTime};

use super::{Counting, ExitReporter, Handover, Hooks, Shared};
use crate::buffers::SocketBuffers;
use crate::clock::Clock;
use crate::engine::schedule::{self, DeadlineHeap, HoldOff, Holding, ReadyList};
use crate::engine::slot::Slot;
use crate::engine::times::{Times, Turn};
use crate::engine::{EngineFacts, EngineSettings, FromEngine, SLICE};
use crate::socket::classify::{Action, Call, classify};
use crate::socket::send_stamps::Numbering;
use crate::socket::{Adapter, Open, OverflowCount, Settings, Socket};
use crate::wait::{BACKOFF_SLEEP, SpinGuard};

/// The events harvested with one epoll_wait call.
const EVENTS: usize = 256;

/// What a slot that panicked reports.
const PANICKED: &str = "the Data Phase panicked";

// : a diagnostic inventory, not a layout contract or a rate gate.
// Run alone so the counting allocator observes only this fixture.
#[cfg(test)]
#[test]
#[ignore = "explicit layout inventory, run in its own process"]
fn layout_inventory() {
    use crate::socket::batch_read::{BatchReader, HEADER_SLOT, Received, STATUS_SLOT};
    use crate::socket::{Linux, Platform};
    use std::mem::{align_of, size_of};
    macro_rules! row {
        ($t:ty) => {
            println!(
                "layout,{},{},{}",
                stringify!($t),
                size_of::<$t>(),
                align_of::<$t>()
            );
        };
    }
    row!(Slot<Linux>);
    row!(Entry<Linux>);
    row!(Option<Entry<Linux>>);
    row!(Linux);
    row!(BatchReader);
    row!(Received);
    row!(crate::socket::batch::Message);
    row!(udpstp_core::RxDatagram);
    let received = Received::default();
    let fields = std::mem::size_of_val(&received.len)
        + std::mem::size_of_val(&received.source)
        + std::mem::size_of_val(&received.kernel_wall)
        + std::mem::size_of_val(&received.tos);
    println!("padding,Received,{}", size_of::<Received>() - fields);
    let region = stats_alloc::Region::new(crate::ALLOCATOR);
    let lateness = crate::socket::send_stamps::Micros::allocated();
    let change = region.change();
    println!("lateness,{},{}", change.allocations, change.bytes_allocated);
    std::hint::black_box(lateness);
    for (role, slot) in [("receiver", HEADER_SLOT), ("sender", STATUS_SLOT)] {
        let region = stats_alloc::Region::new(crate::ALLOCATOR);
        let reader = BatchReader::new(slot);
        let change = region.change();
        println!(
            "reader,{role},{},{},{}",
            slot, change.allocations, change.bytes_allocated
        );
        std::hint::black_box(reader);

        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let peer = socket.local_addr().unwrap();
        socket.connect(peer).unwrap();
        let settings = Settings {
            peer,
            connected: true,
            load_receiver: role == "receiver",
            rx_timestamps: true,
            send_stamps: false,
            explicit_numbers: true,
            clock: Clock::new(),
        };
        let region = stats_alloc::Region::new(crate::ALLOCATOR);
        let socket = Socket::open(Platform, socket, &settings);
        let change = region.change();
        println!(
            "socket,{role},{},{}",
            change.allocations, change.bytes_allocated
        );
        std::hint::black_box(socket);
    }
}

#[cfg(test)]
mod tests;

/// What an Engine Thread runs with.
pub(super) struct Context<O> {
    pub(super) shared: Arc<Shared>,
    pub(super) epoll: Epoll,
    pub(super) starts: mpsc::Receiver<Handover<O>>,
    pub(super) stops: mpsc::Receiver<u64>,
    pub(super) clock: Clock,
    pub(super) settings: EngineSettings,
    pub(super) report: reports::UnboundedSender<FromEngine>,
    pub(super) overflows: Option<mpsc::Sender<Counting>>,
    pub(super) hooks: Hooks,
}

/// Marks the Engine Thread dead as it ends, however it ends.
struct Alive(Arc<Shared>);

impl Drop for Alive {
    fn drop(&mut self) {
        self.0.alive.store(false, Ordering::Relaxed);
    }
}

/// Runs an Engine Thread until its pool is dropped.
pub(super) fn run<O: Open>(context: Context<O>) {
    let _alive = Alive(context.shared.clone());
    #[cfg(test)]
    context
        .shared
        .tid
        .store(nix::unistd::gettid().as_raw(), Ordering::Relaxed);
    // Timer slack of 1 ns (ADR 0008), as the per-Test-Connection
    // executor's; not getting it is no reason to stop.
    #[cfg(target_os = "linux")]
    let _ = nix::sys::prctl::set_timerslack(1);
    EngineThread::new(&context).run(&context);
}

/// One Test Connection on the Engine Thread.
struct Entry<A> {
    slot: Slot<A>,
    key: u64,
    reporter: ExitReporter,
    buffers: Option<SocketBuffers>,
    load_receiver: bool,
    receive_buffer: usize,
    /// Bytes read in the hold-off's current window.
    window_bytes: u64,
    /// An error is pending on its socket (EPOLLERR).
    error: bool,
    /// Touched this turn, and read this turn.
    touched: bool,
    read: bool,
}

/// How a wait ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Woke {
    TimedOut,
    Ready {
        epoll: bool,
        wake: bool,
    },
    Interrupted,
    /// The kernel refuses ppoll: sleep from now on.
    Refused,
}

struct EngineThread<A> {
    entries: Vec<Option<Entry<A>>>,
    /// Each entry's generation, which its epoll events carry, so an event
    /// harvested for a slot that has left, its index reused, is ignored.
    generations: Vec<u32>,
    free: Vec<usize>,
    heap: DeadlineHeap,
    ready: ReadyList,
    touched: Vec<usize>,
    events: Vec<EpollEvent>,
    hold: HoldOff,
    guard: SpinGuard,
    /// Whether the next turn harvests readiness: not after a wait that
    /// timed out, or ended for a command alone, with the epoll set in it.
    harvest: bool,
    /// Whether the last wait ended early, judged after the turn that
    /// follows it.
    woke: bool,
    /// The kernel refused ppoll: the Engine Thread sleeps.
    sleeping: bool,
    opened: u64,
}

impl<A: Adapter> EngineThread<A> {
    fn new<O>(context: &Context<O>) -> Self {
        Self {
            entries: Vec::new(),
            generations: Vec::new(),
            free: Vec::new(),
            heap: DeadlineHeap::default(),
            ready: ReadyList::default(),
            touched: Vec::new(),
            events: vec![EpollEvent::empty(); EVENTS],
            hold: HoldOff::new(context.settings.hold_off),
            guard: SpinGuard::default(),
            harvest: true,
            woke: false,
            sleeping: false,
            opened: 0,
        }
    }

    fn run<O: Open<Adapter = A>>(&mut self, context: &Context<O>) {
        loop {
            if context.shared.shutdown.load(Ordering::Relaxed) {
                return;
            }
            let turn = Turn::start(context.clock);
            let (now, again) = self.turn(context, &turn);
            self.wait(context, now, again);
        }
    }

    /// One turn's work, with its executor-supplied clock readings. Waiting
    /// stays outside the turn so the socket operations can be checked with
    /// scripted time, including a host pause between turns.
    fn turn<O: Open<Adapter = A>>(
        &mut self,
        context: &Context<O>,
        turn: &impl Times,
    ) -> (MonoTime, bool) {
        let started = turn.latest().mono;
        let commands = self.commands(context, turn);
        let (harvest, holding) = match self.hold.check(started) {
            Holding::Yes => (false, true),
            Holding::Ended => (true, false),
            Holding::No => (self.harvest, false),
        };
        let harvested = harvest && self.harvest_readiness(context);
        let again = self.send_due(context, turn, started.as_duration() + SLICE);
        if !holding {
            self.read_ready(context, turn);
        }
        self.timeouts(context, turn);
        let read = self.finish(context);
        if std::mem::take(&mut self.woke) {
            self.guard.woke(commands || harvested || read > 0);
        }
        let now = turn.now().mono;
        self.hold.turn(started, now, read);
        if let Some(elapsed) = self.hold.window_closes(now) {
            let share = self.close_window(elapsed);
            self.hold.close(now, elapsed, share);
        }
        if harvest && read > 0 {
            self.hold.start(read, now);
        }
        (now, again)
    }

    /// Takes the commands waiting: whether there were any.
    fn commands<O: Open<Adapter = A>>(&mut self, context: &Context<O>, turn: &impl Times) -> bool {
        let mut any = false;
        while let Ok(handover) = context.starts.try_recv() {
            any = true;
            self.open(context, handover);
        }
        while let Ok(key) = context.stops.try_recv() {
            any = true;
            let find = |entries: &[Option<Entry<A>>]| {
                entries
                    .iter()
                    .position(|entry| entry.as_ref().is_some_and(|entry| entry.key == key))
            };
            // A stop is sent after its start, so a start sent since the
            // starts were taken is there now: taken, lest the stop be lost.
            let mut found = find(&self.entries);
            if found.is_none() {
                while let Ok(handover) = context.starts.try_recv() {
                    self.open(context, handover);
                }
                found = find(&self.entries);
            }
            if let Some(index) = found {
                self.touch(index);
                self.with_slot(context, index, |entry| {
                    entry.slot.stop(turn);
                    Ok(())
                });
            }
        }
        any
    }

    /// Opens a Test Connection handed over: its socket's adapter, on this
    /// thread, and its place in the epoll set, the heap and the ready list.
    fn open<O: Open<Adapter = A>>(&mut self, context: &Context<O>, handover: Handover<O>) {
        let Handover {
            socket_id,
            key,
            data_phase,
            socket,
            connected,
            buffers,
            receive_buffer,
            open,
            reporter,
        } = handover;
        let numbering = context.settings.send_stamps;
        let load_receiver = data_phase.is_load_receiver();
        let settings = Settings {
            peer: data_phase.peer(),
            connected,
            load_receiver,
            rx_timestamps: true,
            send_stamps: numbering.is_some(),
            // Unless the settings ask for the kernel's own count.
            explicit_numbers: numbering != Some(Numbering::Kernel),
            clock: context.clock,
        };
        let opened = catch_unwind(AssertUnwindSafe(|| {
            Slot::new(socket_id, data_phase, Socket::open(open, socket, &settings))
        }));
        let Ok(mut slot) = opened else {
            let facts = reporter.plain_facts();
            reporter.exited(None, Some(PANICKED.into()), facts);
            context.shared.serving.fetch_sub(1, Ordering::Relaxed);
            return;
        };
        let index = self.free.pop().unwrap_or_else(|| {
            self.entries.push(None);
            self.generations.push(0);
            let slots = self.entries.len();
            self.heap.reserve(slots);
            self.ready.reserve(slots);
            self.touched
                .reserve(slots.saturating_sub(self.touched.len()));
            slots - 1
        });
        let data = ((index as u64) << 32) | u64::from(self.generations[index]);
        let event = EpollEvent::new(EpollFlags::EPOLLIN | EpollFlags::EPOLLET, data);
        if let Err(errno) = context.epoll.add(slot.socket(), event) {
            self.free.push(index);
            let facts = slot.facts_on_engine_thread(buffers).0;
            reporter.exited(
                None,
                Some(format!("can't watch the Test Connection's socket: {errno}")),
                facts,
            );
            context.shared.serving.fetch_sub(1, Ordering::Relaxed);
            return;
        }
        slot.joined_epoll();
        let deadline = slot.next_deadline();
        self.entries[index] = Some(Entry {
            slot,
            key,
            reporter,
            buffers,
            load_receiver,
            receive_buffer,
            window_bytes: 0,
            error: false,
            touched: false,
            read: false,
        });
        self.heap.set(index, deadline);
        // What arrived before it joined the epoll set.
        self.ready.push(index);
        self.opened += 1;
        if let Some((opens, panicked)) = &context.hooks.panic_after_opens
            && *opens == self.opened
            && !panicked.swap(true, Ordering::SeqCst)
        {
            panic!("injected fault: the Engine Thread panics in its own loop (UDPSTP_FAULTS)");
        }
    }

    /// Harvests the epoll set's readiness onto the ready list: whether
    /// anything signalled.
    fn harvest_readiness<O>(&mut self, context: &Context<O>) -> bool {
        let mut any = false;
        loop {
            let count = match context.epoll.wait(&mut self.events, EpollTimeout::ZERO) {
                Ok(count) => count,
                Err(Errno::EINTR) => continue,
                Err(_) => return any,
            };
            for event in &self.events[..count] {
                let data = event.data();
                let (index, generation) = ((data >> 32) as usize, data as u32);
                let current = self.generations.get(index) == Some(&generation);
                let Some(Some(entry)) = self.entries.get_mut(index).filter(|_| current) else {
                    continue;
                };
                if event.events().contains(EpollFlags::EPOLLERR) {
                    entry.error = true;
                }
                self.ready.push(index);
                any = true;
            }
            if count < self.events.len() {
                return any;
            }
        }
    }

    /// Sends every due Burst, earliest deadline first, until `until` on the
    /// Test Time Base: whether some were left due.
    fn send_due<O>(&mut self, context: &Context<O>, turn: &impl Times, until: Duration) -> bool {
        while let Some((deadline, index)) = self.heap.peek() {
            let latest = turn.latest().mono;
            if deadline > latest {
                return false;
            }
            if latest.as_duration() >= until {
                return true;
            }
            self.heap.remove(index);
            self.touch(index);
            self.with_slot(context, index, |entry| {
                if entry.slot.burst_due(latest) {
                    entry.slot.send_due(turn, until)?;
                }
                Ok(())
            });
        }
        false
    }

    /// Reads each slot on the ready list as the turn started, up to its
    /// budget, its pending errors drained first.
    fn read_ready<O>(&mut self, context: &Context<O>, turn: &impl Times) {
        for _ in 0..self.ready.len() {
            let Some(index) = self.ready.pop() else {
                return;
            };
            if !self.touch(index) {
                continue;
            }
            let reads = self.with_slot(context, index, |entry| {
                if std::mem::take(&mut entry.error) {
                    entry.slot.drain_errors(turn);
                }
                entry.read = true;
                entry.slot.read(turn)
            });
            if reads.is_some_and(|reads| reads.more) {
                self.ready.push(index);
            }
        }
    }

    /// Each touched slot's due timeout and Status PDUs: a Load Receiver
    /// whose timeout is due, not read this turn, is read first.
    fn timeouts<O>(&mut self, context: &Context<O>, turn: &impl Times) {
        for position in 0..self.touched.len() {
            let index = self.touched[position];
            self.with_slot(context, index, |entry| {
                if entry.load_receiver {
                    let due = entry.slot.timeout_due(turn.latest().mono);
                    if due && !std::mem::replace(&mut entry.read, true) {
                        entry.slot.read(turn)?;
                    }
                    entry.slot.timeouts(turn);
                    // Only a Load Receiver sends Status PDUs.
                    entry.slot.transmit_status(turn)?;
                } else {
                    entry.slot.timeouts(turn);
                }
                Ok(())
            });
        }
    }

    /// Passes on each touched slot's events, retires the ones that ended,
    /// and re-keys the rest: the datagrams they read this turn.
    fn finish<O>(&mut self, context: &Context<O>) -> usize {
        let mut read = 0;
        for position in 0..self.touched.len() {
            let index = self.touched[position];
            let Some(entry) = self.entry(index) else {
                continue;
            };
            entry.touched = false;
            entry.read = false;
            let reads = entry.slot.take_reads();
            read += reads.datagrams;
            entry.window_bytes += reads.bytes;
            let outcome = self.with_slot(context, index, |entry| {
                let ended = entry.slot.forward_events(&context.report);
                Ok((ended, entry.slot.ended(), entry.slot.next_deadline()))
            });
            match outcome {
                Some((true, reason, _)) => self.retire(context, index, reason, None),
                Some((false, _, deadline)) => self.heap.set(index, deadline),
                None => {}
            }
        }
        self.touched.clear();
        read
    }

    /// Closes the hold-off's window, `elapsed` long: the shortest share of
    /// a slot that read in it.
    fn close_window(&mut self, elapsed: Duration) -> Option<Duration> {
        self.entries
            .iter_mut()
            .flatten()
            .filter_map(|entry| {
                let bytes = std::mem::take(&mut entry.window_bytes);
                schedule::share(entry.receive_buffer, bytes, elapsed)
            })
            .min()
    }

    /// Waits after a turn that ended at `now`, as `schedule::plan_wait`
    /// says; `again` when its sending time ran out with Bursts due.
    fn wait<O>(&mut self, context: &Context<O>, now: MonoTime, again: bool) {
        self.harvest = true;
        let Some(plan) = schedule::plan_wait(
            &self.heap,
            context.settings.grouping,
            self.hold.until(),
            !self.ready.is_empty(),
            again,
            now,
        ) else {
            return;
        };
        let timeout = plan
            .until
            .map(|until| until.as_duration().saturating_sub(now.as_duration()));
        if self.sleeping || self.guard.sleep_instead() {
            let cap = if self.sleeping { SLICE } else { BACKOFF_SLEEP };
            std::thread::sleep(timeout.map_or(cap, |timeout| timeout.min(cap)));
            return;
        }
        match ppoll(context, plan.arrivals, timeout) {
            Woke::TimedOut => {
                self.guard.timed_out();
                self.harvest = false;
            }
            Woke::Ready { epoll, wake } => {
                // Reset before the commands are taken, so one sent in
                // between is never missed.
                if wake {
                    let _ = context.shared.wake.read();
                }
                self.harvest = epoll;
                self.woke = true;
            }
            Woke::Interrupted => self.woke = true,
            Woke::Refused => self.sleeping = true,
        }
    }

    /// Marks `index` touched this turn: whether it is there.
    fn touch(&mut self, index: usize) -> bool {
        let Some(entry) = self.entry(index) else {
            return false;
        };
        if !std::mem::replace(&mut entry.touched, true) {
            self.touched.push(index);
        }
        true
    }

    fn entry(&mut self, index: usize) -> Option<&mut Entry<A>> {
        self.entries.get_mut(index)?.as_mut()
    }

    /// Runs `work` on slot `index`, inside a `catch_unwind` of its own: a
    /// panic, or a socket that became unusable, retires the slot. What
    /// `work` returned, if the slot is still there.
    fn with_slot<O, R>(
        &mut self,
        context: &Context<O>,
        index: usize,
        work: impl FnOnce(&mut Entry<A>) -> io::Result<R>,
    ) -> Option<R> {
        let entry = self.entry(index)?;
        match catch_unwind(AssertUnwindSafe(|| work(entry))) {
            Ok(Ok(value)) => Some(value),
            Ok(Err(error)) => {
                self.retire(context, index, None, Some(error.to_string()));
                None
            }
            Err(_) => {
                self.retire(context, index, None, Some(PANICKED.into()));
                None
            }
        }
    }

    /// Retires slot `index`: out of the epoll set, before its socket
    /// closes, the heap and the ready list; its end reported, its
    /// Receive Buffer Overflows read off this thread where they are
    /// listed.
    fn retire<O>(
        &mut self,
        context: &Context<O>,
        index: usize,
        reason: Option<EndReason>,
        error: Option<String>,
    ) {
        let Some(entry) = self.entries.get_mut(index).and_then(Option::take) else {
            return;
        };
        self.generations[index] = self.generations[index].wrapping_add(1);
        self.free.push(index);
        self.heap.remove(index);
        self.ready.remove(index);
        let Entry {
            slot,
            reporter,
            buffers,
            ..
        } = entry;
        // Nothing about a slot that panicked may take the Engine Thread
        // down: each look at it is guarded.
        let _ = catch_unwind(AssertUnwindSafe(|| context.epoll.delete(slot.socket())));
        let ended = catch_unwind(AssertUnwindSafe(|| {
            let (facts, count) = slot.facts_on_engine_thread(buffers);
            let listed = (count == OverflowCount::Listed)
                .then(|| slot.socket().try_clone().ok())
                .flatten();
            (facts, count, listed)
        }));
        let _ = catch_unwind(AssertUnwindSafe(move || drop(slot)));
        match ended {
            Ok((facts, OverflowCount::Known(count), _)) => {
                let facts = EngineFacts {
                    receive_buffer_overflows: count,
                    ..facts
                };
                reporter.exited(reason, error, facts);
            }
            Ok((facts, OverflowCount::Listed, Some(socket))) => {
                let counting = Counting {
                    reporter,
                    reason,
                    error,
                    facts,
                    socket,
                };
                match &context.overflows {
                    Some(reader) => {
                        if let Err(mpsc::SendError(counting)) = reader.send(counting) {
                            count_here(counting);
                        }
                    }
                    None => count_here(counting),
                }
            }
            Ok((facts, OverflowCount::Listed, None)) => reporter.exited(reason, error, facts),
            Err(_) => {
                let facts = reporter.plain_facts();
                reporter.exited(reason, error, facts);
            }
        }
        context.shared.serving.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Reads one Test Connection's Receive Buffer Overflows on this thread,
/// where no reader thread takes them.
fn count_here(counting: Counting) {
    let count = crate::socket::listed_overflows(std::iter::once(&counting.socket))
        .pop()
        .flatten();
    counting.report(count);
}

/// Waits in ppoll on the epoll set, for arrivals when `arrivals`, and on
/// the eventfd, at most `timeout`, or with none, until one of them.
fn ppoll<O>(context: &Context<O>, arrivals: bool, timeout: Option<Duration>) -> Woke {
    let hooks = &context.hooks;
    if let Some(polls) = &hooks.polls {
        polls.fetch_add(1, Ordering::Relaxed);
    }
    if hooks.ppoll_refused {
        return Woke::Refused;
    }
    if hooks.epoll_stuck {
        return Woke::Ready {
            epoll: true,
            wake: false,
        };
    }
    let events = if arrivals {
        PollFlags::POLLIN
    } else {
        PollFlags::empty()
    };
    let mut fds = [
        PollFd::new(context.epoll.0.as_fd(), events),
        PollFd::new(context.shared.wake.as_fd(), PollFlags::POLLIN),
    ];
    match nix::poll::ppoll(&mut fds, timeout.map(TimeSpec::from_duration), None) {
        Ok(0) => Woke::TimedOut,
        Ok(_) => {
            let readable = |fd: &PollFd<'_>| {
                fd.revents()
                    .is_some_and(|revents| revents.intersects(PollFlags::POLLIN))
            };
            Woke::Ready {
                epoll: readable(&fds[0]),
                wake: readable(&fds[1]),
            }
        }
        Err(Errno::EINTR) => Woke::Interrupted,
        Err(errno) => match classify(Call::Wait, &io::Error::from(errno)) {
            Action::Unsupported(_) => Woke::Refused,
            _ => Woke::Interrupted,
        },
    }
}
