//! The pooled executor's policy, pure: no sockets, clocks or
//! threads, so it is tested deterministically on every OS. The Linux
//! Server's Engine Threads (`pool.rs`) run it.
//!
//! - [`DeadlineHeap`]: each Test Connection's next deadline, its next
//!   Burst or its Data Phase's next timeout, in an indexed binary min-heap,
//!   re-keyed in place; the earliest first, ties by slot.
//! - [`ReadyList`]: the Test Connections whose sockets signalled, served
//!   in the order they did, each a bounded number of datagrams a turn; one
//!   whose budget ran out waits at the back for the next turn.
//! - [`place`]: a new Test Connection goes to the Engine Thread serving
//!   the fewest, a new Engine Thread while one may start and every one
//!   started serves at least one, ties to the lowest index, never to one
//!   that died.
//! - [`HoldOff`]: 's receive hold-off, the Engine Thread's rather than
//!   each socket's: after a turn that harvested readiness and read, the
//!   next waits leave arrivals out until a fixed end, `H` later, so what
//!   arrives meanwhile is read in one turn. `H` is at most 250 µs, and at
//!   most the shortest share of a reading Test Connection's receive buffer
//!   (a quarter of the time it holds at its arrival rate) less the
//!   Engine Thread's longest turn: a socket goes unread for the hold-off
//!   plus a turn. Under 100 µs, there is none. It is taken only when more
//!   than one datagram is due within it at the rate datagrams arrived on
//!   all the Engine Thread's sockets over its last window of at least
//!   10 ms; while that rate isn't known, at the start and after a window
//!   in which nothing arrived, it is 100 µs, as a Load Receiver's is.
//!   **A known gap:** the share counts the payload bytes read, not the
//!   memory the kernel charges for them, so it can't see that loopback
//!   and veth charge a datagram split from a UDP GSO message far more
//!   than its size, where a receive buffer holds only about 0.5 ms of a
//!   multi-Gbit/s load. There
//!   a fast Load Receiver sharing an Engine Thread with Load Senders
//!   waits for the hold-off plus up to a turn's 1 ms of sends and can
//!   overflow; over a NIC the charge is close to the datagram's size and
//!   the share holds. A later fix would judge the share by what the
//!   kernel charges (the socket's receive memory, `SO_MEMINFO`, against
//!   the bytes read); an ignored measurement in `pool_tests.rs`, a fast
//!   Load Receiver sharing an Engine Thread with Load Senders, shows the
//!   case.
//! - [`plan_wait`]: until the earliest deadline, or the hold-off's end,
//!   never past the hold-off's end; no wait
//!   while the turn's sending time ran out with Bursts still due, or a
//!   Test Connection's socket may still hold datagrams outside a hold-off.

use std::collections::VecDeque;
use std::time::Duration;

use udpstp_core::MonoTime;

use crate::wait::{HOLD_OFF_MAX, HOLD_OFF_MIN, RATE_WINDOW, more_than_one_due};

/// A slot not in the heap.
const ABSENT: usize = usize::MAX;

/// A hold-off is at most this share of the time a receive buffer holds.
const SHARE: u32 = 4;

/// Each slot's next deadline, earliest first.
#[derive(Debug, Default)]
pub(crate) struct DeadlineHeap {
    /// Deadlines and their slots, a binary min-heap by (deadline, slot).
    heap: Vec<(MonoTime, usize)>,
    /// Each slot's position in `heap`, or [`ABSENT`].
    at: Vec<usize>,
}

impl DeadlineHeap {
    /// Room for `slots` slots, so keying them allocates nothing.
    pub(crate) fn reserve(&mut self, slots: usize) {
        if self.at.len() < slots {
            self.at.resize(slots, ABSENT);
        }
        self.heap.reserve(slots.saturating_sub(self.heap.len()));
    }

    /// Keys `slot` at `deadline`, or takes it out for none.
    pub(crate) fn set(&mut self, slot: usize, deadline: Option<MonoTime>) {
        let Some(deadline) = deadline else {
            return self.remove(slot);
        };
        if slot >= self.at.len() {
            self.at.resize(slot + 1, ABSENT);
        }
        match self.at[slot] {
            ABSENT => {
                let position = self.heap.len();
                self.heap.push((deadline, slot));
                self.at[slot] = position;
                self.up(position);
            }
            position => {
                let before = self.heap[position].0;
                self.heap[position].0 = deadline;
                if deadline < before {
                    self.up(position);
                } else {
                    self.down(position);
                }
            }
        }
    }

    /// Takes `slot` out, if it is in.
    pub(crate) fn remove(&mut self, slot: usize) {
        let Some(&position) = self.at.get(slot) else {
            return;
        };
        if position == ABSENT {
            return;
        }
        let last = self.heap.len() - 1;
        self.swap(position, last);
        self.heap.pop();
        self.at[slot] = ABSENT;
        if position < self.heap.len() {
            self.down(position);
            self.up(position);
        }
    }

    /// The earliest deadline and its slot.
    pub(crate) fn peek(&self) -> Option<(MonoTime, usize)> {
        self.heap.first().copied()
    }

    /// The earliest deadline.
    pub(crate) fn earliest(&self) -> Option<MonoTime> {
        self.peek().map(|(at, _)| at)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.heap.len()
    }

    fn swap(&mut self, i: usize, j: usize) {
        self.heap.swap(i, j);
        self.at[self.heap[i].1] = i;
        self.at[self.heap[j].1] = j;
    }

    fn up(&mut self, mut i: usize) {
        while i > 0 {
            let parent = (i - 1) / 2;
            if self.heap[i] >= self.heap[parent] {
                return;
            }
            self.swap(i, parent);
            i = parent;
        }
    }

    fn down(&mut self, mut i: usize) {
        loop {
            let (left, right) = (2 * i + 1, 2 * i + 2);
            let mut least = i;
            if left < self.heap.len() && self.heap[left] < self.heap[least] {
                least = left;
            }
            if right < self.heap.len() && self.heap[right] < self.heap[least] {
                least = right;
            }
            if least == i {
                return;
            }
            self.swap(i, least);
            i = least;
        }
    }
}

/// The slots whose sockets signalled and aren't read yet, in the order
/// they did: longest-unread first.
#[derive(Debug, Default)]
pub(crate) struct ReadyList {
    queue: VecDeque<usize>,
    queued: Vec<bool>,
}

impl ReadyList {
    /// Room for `slots` slots, so queueing them allocates nothing.
    pub(crate) fn reserve(&mut self, slots: usize) {
        if self.queued.len() < slots {
            self.queued.resize(slots, false);
        }
        self.queue.reserve(slots.saturating_sub(self.queue.len()));
    }

    /// Queues `slot`, unless it is queued already.
    pub(crate) fn push(&mut self, slot: usize) {
        if slot >= self.queued.len() {
            self.queued.resize(slot + 1, false);
        }
        if !self.queued[slot] {
            self.queued[slot] = true;
            self.queue.push_back(slot);
        }
    }

    /// The slot longest unread.
    pub(crate) fn pop(&mut self) -> Option<usize> {
        let slot = self.queue.pop_front()?;
        self.queued[slot] = false;
        Some(slot)
    }

    /// Takes `slot` out, if it is queued.
    pub(crate) fn remove(&mut self, slot: usize) {
        if self.queued.get(slot).copied().unwrap_or(false) {
            self.queue.retain(|&queued| queued != slot);
            self.queued[slot] = false;
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.queue.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

/// An Engine Thread started, as placement sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Load {
    /// The Test Connections it serves, those handed to it and not yet
    /// ended included.
    pub(crate) serving: usize,
    /// Whether it is still running.
    pub(crate) alive: bool,
}

/// Where a new Test Connection goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Place {
    /// On the Engine Thread of this index.
    On(usize),
    /// On a new Engine Thread.
    Start,
}

/// Where a new Test Connection goes among the Engine Threads `started`,
/// of which at most `max` may run: the one serving the fewest, ties to
/// the lowest index, unless every one serves at least one and another may
/// start. A dead one is never chosen, and doesn't count against `max`.
pub(crate) fn place(started: &[Load], max: usize) -> Place {
    let alive = started.iter().filter(|load| load.alive).count();
    let fewest = started
        .iter()
        .enumerate()
        .filter(|(_, load)| load.alive)
        .min_by_key(|&(index, load)| (load.serving, index));
    match fewest {
        Some((index, load)) if load.serving == 0 || alive >= max => Place::On(index),
        _ => Place::Start,
    }
}

/// Where an Engine Thread's hold-off stands as a turn starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Holding {
    /// None is running.
    No,
    /// One is running: arrivals wait, and readiness isn't harvested.
    Yes,
    /// One ended by now: this turn harvests what arrived meanwhile.
    Ended,
}

/// An Engine Thread's receive hold-off.
#[derive(Debug)]
pub(crate) struct HoldOff {
    /// The hold-off the settings fix (`UDPSTP_ENGINE_HOLD_OFF_US`), taken
    /// after every harvesting turn that read, whatever is due; zero for
    /// none.
    fixed: Option<Duration>,
    /// The window being measured: since when, the datagrams read in it,
    /// and its longest turn.
    since: Option<MonoTime>,
    datagrams: u64,
    longest: Duration,
    /// Over the last window closed: datagrams a second, if any arrived;
    /// the shortest reading slot's share; and the longest turn.
    rate: Option<u64>,
    share: Option<Duration>,
    turn: Duration,
    /// The running hold-off's end.
    until: Option<MonoTime>,
}

impl HoldOff {
    pub(crate) fn new(fixed: Option<Duration>) -> Self {
        Self {
            fixed,
            since: None,
            datagrams: 0,
            longest: Duration::ZERO,
            rate: None,
            share: None,
            turn: Duration::ZERO,
            until: None,
        }
    }

    /// As a turn starts at `now`: whether a hold-off runs, and ends one
    /// that has run out.
    pub(crate) fn check(&mut self, now: MonoTime) -> Holding {
        match self.until {
            None => Holding::No,
            Some(until) if now < until => Holding::Yes,
            Some(_) => {
                self.until = None;
                Holding::Ended
            }
        }
    }

    /// The running hold-off's end.
    pub(crate) fn until(&self) -> Option<MonoTime> {
        self.until
    }

    /// A turn ran from `started` to `ended` and read `datagrams`.
    pub(crate) fn turn(&mut self, started: MonoTime, ended: MonoTime, datagrams: usize) {
        self.since.get_or_insert(started);
        self.datagrams += datagrams as u64;
        let took = ended.as_duration().saturating_sub(started.as_duration());
        self.longest = self.longest.max(took);
    }

    /// How long the window has run at `now`, once it has run at least
    /// [`RATE_WINDOW`]: then it is closed with [`HoldOff::close`].
    pub(crate) fn window_closes(&self, now: MonoTime) -> Option<Duration> {
        let elapsed = now.as_duration().saturating_sub(self.since?.as_duration());
        (elapsed >= RATE_WINDOW).then_some(elapsed)
    }

    /// Closes the window at `now`, `elapsed` long, in which the shortest
    /// share of a reading slot's receive buffer was `share` ([`share`]).
    pub(crate) fn close(&mut self, now: MonoTime, elapsed: Duration, share: Option<Duration>) {
        let rate = u128::from(self.datagrams) * 1_000_000_000 / elapsed.as_nanos().max(1);
        self.rate = (self.datagrams > 0).then(|| u64::try_from(rate).unwrap_or(u64::MAX));
        self.share = share;
        self.turn = self.longest;
        self.since = Some(now);
        self.datagrams = 0;
        self.longest = Duration::ZERO;
    }

    /// After a turn that harvested readiness and read `datagrams`, at
    /// `now`: starts a hold-off where one is due, and returns its end.
    pub(crate) fn start(&mut self, datagrams: usize, now: MonoTime) -> Option<MonoTime> {
        if datagrams == 0 {
            return None;
        }
        let length = self.length()?;
        let until = MonoTime::from_duration(now.as_duration() + length);
        self.until = Some(until);
        Some(until)
    }

    /// How long a hold-off lasts now; none where it wouldn't pay.
    fn length(&self) -> Option<Duration> {
        if let Some(fixed) = self.fixed {
            return (!fixed.is_zero()).then_some(fixed);
        }
        let Some(rate) = self.rate else {
            // Not known yet: as a Load Receiver's.
            return Some(HOLD_OFF_MIN);
        };
        let length = self
            .share
            .map_or(HOLD_OFF_MAX, |share| share.saturating_sub(self.turn))
            .min(HOLD_OFF_MAX);
        (length >= HOLD_OFF_MIN && more_than_one_due(Some(rate), length)).then_some(length)
    }
}

/// A reading slot's share: a quarter of the time a receive buffer of
/// `buffer` bytes holds at the rate `bytes` arrived over `elapsed`,
/// unclamped; none if nothing arrived. `bytes` are payload bytes, which
/// on loopback and veth understate what the buffer is charged: the known
/// gap in the module's documentation.
pub(crate) fn share(buffer: usize, bytes: u64, elapsed: Duration) -> Option<Duration> {
    (bytes > 0).then(|| {
        let holds = buffer as u128 * elapsed.as_nanos() / u128::from(bytes);
        Duration::from_nanos(u64::try_from(holds / u128::from(SHARE)).unwrap_or(u64::MAX))
    })
}

/// How an Engine Thread waits after a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Wait {
    /// Until then at most; with no end, until an arrival or a command.
    pub(crate) until: Option<MonoTime>,
    /// Whether an arrival on a socket ends it: not during a hold-off.
    pub(crate) arrivals: bool,
}

/// How an Engine Thread waits at `now`, after a turn: none (`None`, the
/// next turn at once) when `again`, the turn's sending time having run
/// out with Bursts still due, when a deadline is due, or when the ready
/// list holds a slot outside a hold-off; else until the earliest deadline
/// in `heap`, never past the hold-off's end `hold`.
pub(crate) fn plan_wait(
    heap: &DeadlineHeap,
    hold: Option<MonoTime>,
    ready: bool,
    again: bool,
    now: MonoTime,
) -> Option<Wait> {
    if again || (ready && hold.is_none()) {
        return None;
    }
    let deadline = heap.earliest();
    let until = deadline.into_iter().chain(hold).min();
    if until.is_some_and(|until| until <= now) {
        return None;
    }
    Some(Wait {
        until,
        arrivals: hold.is_none(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(us: u64) -> MonoTime {
        MonoTime::from_duration(Duration::from_micros(us))
    }

    const US: fn(u64) -> Duration = Duration::from_micros;

    /// The heap's deadlines, popped in order.
    fn drain(heap: &mut DeadlineHeap) -> Vec<(u64, usize)> {
        let mut popped = Vec::new();
        while let Some((deadline, slot)) = heap.peek() {
            heap.remove(slot);
            popped.push((deadline.as_duration().as_micros() as u64, slot));
        }
        popped
    }

    //  (design §3.3, the deadline heap): the heap gives the earliest
    // deadline first, ties to the lowest slot; re-keying a slot moves it
    // either way in place, and a slot keyed at no deadline leaves.
    #[test]
    fn the_heap_gives_the_earliest_deadline_and_rekeys_in_place() {
        let mut heap = DeadlineHeap::default();
        heap.reserve(8);
        for (slot, us) in [(0, 500), (1, 100), (2, 300), (3, 100), (4, 900)] {
            heap.set(slot, Some(at(us)));
        }
        assert_eq!(heap.peek(), Some((at(100), 1)), "ties to the lowest slot");
        heap.set(4, Some(at(50)));
        heap.set(1, Some(at(1_000)));
        heap.set(2, None);
        assert_eq!(heap.len(), 4);
        assert_eq!(drain(&mut heap), [(50, 4), (100, 3), (500, 0), (1_000, 1)]);
        assert_eq!(heap.peek(), None);
    }

    // : the heap orders many deadlines, keyed and re-keyed at random,
    // as a sort of their latest keys would.
    #[test]
    fn the_heap_orders_many_deadlines_as_a_sort_would() {
        let mut heap = DeadlineHeap::default();
        let mut keys = [None; 64];
        let mut rng = crate::clock::SplitMix64(7);
        for _ in 0..2_000 {
            let slot = (rng.next() % 64) as usize;
            let key = (!rng.next().is_multiple_of(5)).then(|| rng.next() % 10_000);
            heap.set(slot, key.map(at));
            keys[slot] = key;
        }
        let mut expected: Vec<(u64, usize)> = keys
            .iter()
            .enumerate()
            .filter_map(|(slot, key)| key.map(|key| (key, slot)))
            .collect();
        expected.sort();
        assert_eq!(drain(&mut heap), expected);
    }

    //  (design §3.2, the ready list): slots are served in the order
    // they signalled, once each however often they did; a slot whose
    // budget ran out goes to the back, after the slots that signalled
    // before its turn; one that leaves is taken out.
    #[test]
    fn the_ready_list_serves_the_longest_unread_first() {
        let mut ready = ReadyList::default();
        ready.reserve(4);
        for slot in [2, 0, 2, 3] {
            ready.push(slot);
        }
        assert_eq!(ready.len(), 3);
        // A turn serves the three; slot 2's budget runs out.
        let turn = ready.len();
        let mut served = Vec::new();
        for _ in 0..turn {
            let slot = ready.pop().unwrap();
            served.push(slot);
            if slot == 2 {
                ready.push(2);
            }
        }
        assert_eq!(served, [2, 0, 3]);
        ready.push(1);
        ready.push(3);
        ready.remove(1);
        assert_eq!(
            std::iter::from_fn(|| ready.pop()).collect::<Vec<_>>(),
            [2, 3]
        );
        assert!(ready.is_empty());
    }

    fn load(serving: usize) -> Load {
        Load {
            serving,
            alive: true,
        }
    }

    //  (D9, design §2.1): the first Test Connection starts an Engine
    // Thread; while every one started serves at least one and another may
    // start, the next starts one; then each goes to the one serving the
    // fewest, ties to the lowest index. An idle one started takes the next
    // before another starts.
    #[test]
    fn placement_starts_engine_threads_lazily_and_fills_the_emptiest() {
        assert_eq!(place(&[], 2), Place::Start);
        assert_eq!(place(&[load(1)], 2), Place::Start);
        assert_eq!(place(&[load(1), load(1)], 2), Place::On(0));
        assert_eq!(place(&[load(2), load(1)], 2), Place::On(1));
        assert_eq!(place(&[load(3), load(0)], 4), Place::On(1));
        assert_eq!(place(&[load(3), load(2), load(2)], 3), Place::On(1));
    }

    //  (design §4): an Engine Thread that died is never chosen, and
    // doesn't count against the most that may run: another starts in its
    // place.
    #[test]
    fn placement_never_chooses_a_dead_engine_thread() {
        let dead = Load {
            serving: 0,
            alive: false,
        };
        assert_eq!(place(&[dead, load(5)], 2), Place::Start);
        assert_eq!(place(&[dead, load(5), load(4)], 2), Place::On(2));
        assert_eq!(place(&[dead], 1), Place::Start);
    }

    /// A hold-off whose last closed window was 10 ms of `datagrams`, with
    /// the shortest share `share` and the longest turn `turn`.
    fn measured(datagrams: usize, share: Option<Duration>, turn: Duration) -> HoldOff {
        let mut hold = HoldOff::new(None);
        hold.turn(at(0), MonoTime::from_duration(turn), datagrams);
        hold.close(at(10_000), US(10_000), share);
        hold
    }

    //  (D7, design §3.6): after a harvesting turn that read, the
    // Engine Thread holds off 250 µs where its readers' buffers hold far
    // longer: at H4uc, 150 row-4 Load Receivers on one Engine Thread
    // arrive at 150,000 a second, each share about a second with 2 MiB,
    // and the turn's cost, 1.5 to 3.5 µs a read for about 37 sockets,
    // leaves it at 250 µs.
    #[test]
    fn the_hold_off_is_250_us_at_h4ucs_arrivals_whatever_a_read_costs() {
        const MIB: usize = 1 << 20;
        // 150 sockets × 10 Load PDUs of 472 bytes in 10 ms.
        let share = share(2 * MIB, 10 * 472, US(10_000));
        assert!(share.unwrap() > Duration::from_secs(1), "{share:?}");
        for per_read_ns in [1_500, 2_500, 3_500] {
            let turn = Duration::from_nanos(37 * per_read_ns);
            let mut hold = measured(1_500, share, turn);
            assert_eq!(hold.start(37, at(20_000)), Some(at(20_250)), "{turn:?}");
        }
    }

    //  (design §3.6): H is the shortest reader's share less the
    // longest turn: a loopback multi-Gbps buffer holding 0.5 ms gives a
    // share of 125 µs, so a turn of 30 µs leaves 95 µs, under 100: no
    // hold-off; one of 20 µs leaves 105 µs. It is never raised to 100 µs.
    #[test]
    fn the_hold_off_is_the_shortest_share_less_the_longest_turn() {
        let share = Some(US(125));
        assert_eq!(measured(10_000, share, US(30)).start(10, at(0)), None);
        assert_eq!(
            measured(10_000, share, US(20)).start(10, at(0)),
            Some(at(105))
        );
    }

    //  on one Engine Thread: it holds off only when more than one
    // datagram is due within the hold-off at the rate all its sockets
    // received at: Status PDUs of 75 Load Senders, 1,500 a second, don't
    // make it hold off; a flood does. While the rate isn't known it holds
    // off 100 µs, and after a turn that read nothing, not at all.
    #[test]
    fn the_hold_off_is_taken_only_when_more_than_one_datagram_is_due() {
        let long = Some(Duration::from_secs(1));
        assert_eq!(measured(15, long, US(5)).start(1, at(0)), None);
        assert_eq!(measured(1_000, long, US(5)).start(1, at(0)), Some(at(250)));
        let mut unknown = HoldOff::new(None);
        assert_eq!(unknown.start(1, at(0)), Some(at(100)));
        assert_eq!(HoldOff::new(None).start(0, at(0)), None);
    }

    //  (design §3.6, `UDPSTP_ENGINE_HOLD_OFF_US`): a fixed hold-off is
    // taken after every harvesting turn that read, whatever is due; zero
    // is none.
    #[test]
    fn a_fixed_hold_off_is_taken_whatever_is_due() {
        let mut fixed = HoldOff::new(Some(US(300)));
        assert_eq!(fixed.start(1, at(0)), Some(at(300)));
        assert_eq!(HoldOff::new(Some(Duration::ZERO)).start(9, at(0)), None);
    }

    //  (design §3.6, a hold-off's end is absolute): it runs until its
    // end whatever the turns inside it read, and ends at the first turn at
    // or after it, which harvests.
    #[test]
    fn a_hold_off_ends_at_its_end_and_that_turn_harvests() {
        let mut hold = HoldOff::new(Some(US(250)));
        assert_eq!(hold.check(at(0)), Holding::No);
        hold.start(3, at(0));
        hold.turn(at(100), at(110), 5);
        assert_eq!(hold.check(at(120)), Holding::Yes);
        assert_eq!(hold.until(), Some(at(250)), "reads inside don't move it");
        assert_eq!(hold.check(at(250)), Holding::Ended);
        assert_eq!(hold.check(at(260)), Holding::No);
    }

    // : the rate and the longest turn are measured over windows of at
    // least 10 ms, closed when one has run that long.
    #[test]
    fn the_rate_window_closes_after_10_ms() {
        let mut hold = HoldOff::new(None);
        hold.turn(at(0), at(5), 1);
        assert_eq!(hold.window_closes(at(9_999)), None);
        assert_eq!(hold.window_closes(at(12_000)), Some(US(12_000)));
    }

    //  (design §3.6): a slot's share is a quarter of the time its
    // receive buffer holds at its rate: 2 MiB at 1 Gbps holds 16.8 ms, and
    // nothing arrived is no share.
    #[test]
    fn a_share_is_a_quarter_of_the_time_the_buffer_holds() {
        let bytes = 1_250_000; // 1 Gbps for 10 ms
        assert_eq!(
            share(2 << 20, bytes, US(10_000)),
            Some(Duration::from_nanos(4_194_304))
        );
        assert_eq!(share(2 << 20, 0, US(10_000)), None);
    }

    fn heap(deadlines: &[u64]) -> DeadlineHeap {
        let mut heap = DeadlineHeap::default();
        for (slot, &us) in deadlines.iter().enumerate() {
            heap.set(slot, Some(at(us)));
        }
        heap
    }

    // ADR 0008: the wait ends at the earliest deadline.
    #[test]
    fn the_wait_ends_at_the_earliest_deadline() {
        let deadlines = heap(&[1_000, 1_030, 1_060, 1_200]);
        assert_eq!(
            plan_wait(&deadlines, None, false, false, at(0)),
            Some(Wait {
                until: Some(at(1_000)),
                arrivals: true,
            })
        );
    }

    //  (design §3.6): during a hold-off arrivals don't end the wait,
    // which ends at the hold-off's end at the latest, and a non-empty
    // ready list doesn't cut it short.
    #[test]
    fn a_hold_off_bounds_the_wait_and_leaves_arrivals_out() {
        let deadlines = heap(&[1_000, 1_040]);
        let hold = Some(at(1_020));
        let expected = Some(Wait {
            until: Some(at(1_020)),
            arrivals: false,
        });
        assert_eq!(
            plan_wait(&deadlines, hold, true, false, at(900)),
            Some(Wait {
                until: Some(at(1_000)),
                arrivals: false,
            })
        );
        assert_eq!(
            plan_wait(&heap(&[1_040]), hold, true, false, at(900)),
            expected
        );
        let no_deadline = DeadlineHeap::default();
        assert_eq!(
            plan_wait(&no_deadline, hold, false, false, at(900)),
            expected
        );
    }

    //  (design §3.3): no wait when the turn's sending time ran out
    // with Bursts due, when a deadline is due, or when the ready list holds
    // a slot outside a hold-off; with nothing due and no hold-off, a wait
    // with no end, which an arrival or a command ends.
    #[test]
    fn the_next_turn_starts_at_once_when_work_is_left() {
        let deadlines = heap(&[1_000]);
        assert_eq!(plan_wait(&deadlines, None, false, true, at(0)), None);
        assert_eq!(plan_wait(&deadlines, None, true, false, at(0)), None);
        assert_eq!(plan_wait(&deadlines, None, false, false, at(1_000)), None);
        assert_eq!(
            plan_wait(&DeadlineHeap::default(), None, false, false, at(0)),
            Some(Wait {
                until: None,
                arrivals: true
            })
        );
    }
}
