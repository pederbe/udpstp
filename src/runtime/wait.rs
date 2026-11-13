//! How the Data Phase Engine waits between turns.
//!
//! On Linux it waits in ppoll on its socket, so a datagram that arrives
//! ends the wait at once rather than after a sleep of up to 1 ms, which on
//! loopback above 2 Gbps let the receive buffer overflow
//!. The wait ends at the next
//! Burst's deadline or the core's next timeout, at nanosecond resolution,
//! and at most after [`IDLE_CAP`], so the engine still sees its stop flag.
//! Elsewhere, and where the kernel refuses ppoll, the engine sleeps in
//! slices of at most 1 ms as before.
//!
//! **The receive hold-off.** Waking the Load Receiver for each datagram
//! would cost it a wake-up per datagram, and on loopback the Load Sender
//! too, whose send does the waking. So after a turn that read datagrams, a
//! Load Receiver leaves POLLIN out of its next wait for the hold-off:
//! what arrives meanwhile waits in the receive buffer and is read in one
//! batch. The hold-off is a quarter of the time the receive buffer holds
//! at the rate Load PDUs arrive, within [`HOLD_OFF_MIN`] and
//! [`HOLD_OFF_MAX`]. The rate counts payload bytes, which on loopback and
//! veth understate what the kernel charges the buffer for a datagram
//! split from a UDP GSO message; that matters most for the Linux Server's
//! pooled executor, whose Engine Thread holds off for all its sockets, and
//! is recorded as a known gap there (`engine/schedule.rs`, `HoldOff`).
//!
//! A Load Receiver holds off only when more than one datagram is due
//! within the hold-off. A hold-off that ends with nothing to read
//! costs a wake-up of its own, since the wait after it wakes for the next
//! arrival anyway, and one that ends with a single datagram saves none.
//! So at row 4, a Load PDU every 1 ms against a hold-off of 250 µs, the
//! Load Receiver waits for each arrival and wakes once per Load PDU, not
//! twice; a fast one reads in batches as before. What is due comes from
//! the rate datagrams arrived at in the last window of at least
//! [`RATE_WINDOW`] to close, not from the row: the Client, the Load
//! Receiver of a Downstream Test, doesn't know the Server's row, and what
//! wakes the engine is what arrives, after loss, a Load Sender that fell
//! behind, or a row changed a round trip ago. Until a window with arrivals
//! in it has closed, at the start and after a pause, the rate isn't known,
//! and the Load Receiver holds off as before.
//!
//! The rate counts datagrams, not Bursts. Rows 41 to 99 (49 to 119 with
//! Traditional MTU) send five to ten Load PDUs every 1 ms, so a Load
//! Receiver holds off there, and wakes twice a Burst as before where the
//! Burst arrives back to back, as over loopback, though one wake-up would
//! read it whole; behind a bottleneck that spreads the Burst out, holding
//! off is right. Rows 31 to 40 (37 to 48) send four: exactly one is due
//! within 250 µs, so their windows can fall either way.
//!
//! Waiting for each arrival leaves the wake-up to whoever makes it: the
//! receive softirq, which now more often wakes a CPU that halted, where
//! before the Load Receiver's own timer had woken it. Where that is
//! costly, as in a VM, the softirq's cost per Load PDU rises. In the netns
//! tier on WSL2, where it runs on the Load Senders' CPUs, 150 Test
//! Connections of row 4 Upstream left the Load Senders 7 to 8% short of
//! their schedule, against under 1% before, while the Server's CPU fell by
//! a third; a SCHED_IDLE spinner on each of the Server's CPUs, so none
//! halted, brought them back to about 1%.
//!
//! A Load Sender reads only Status PDUs, so it takes each at once, unless
//! datagrams come closer together than [`FLOOD_SPACING`], which only a
//! flood does; it then holds off too, whatever the rate, so a flooding
//! peer can't make it spin.
//!
//! `UDPSTP_ENGINE_HOLD_OFF_US`, an experimental setting that isn't
//! stable, which the binary reads and passes at spawn
//! ([`EngineSettings::hold_off`](crate::runtime::EngineSettings)), sets the
//! hold-off in µs for measurements, 0 for none, and it is
//! then taken as before , whatever is due within it: by a Load
//! Receiver after every turn that read, and by a Load Sender when flooded.
//!
//! **The spin guard.** A wait that ends early and makes no progress (a
//! pending error that can't be drained, a readable socket with nothing to
//! read, a signal) would otherwise repeat at once. After [`SPIN_LIMIT`]
//! such wake-ups in a row, the engine sleeps instead for its next
//! [`BACKOFF_SLEEPS`] waits, [`BACKOFF_SLEEP`] at most each, then tries
//! ppoll again.
//!
//! This module holds that arithmetic, which is pure: the wait policy. The
//! ppoll call is the socket's (`socket::Socket::ready`).

use std::time::Duration;

use crate::protocol::MonoTime;

use crate::runtime::socket::Woke;

/// The longest a readiness wait lasts when nothing is due: arrivals end it
/// anyway, and the engine checks its stop flag, which the Client's
/// interrupt and the Server's shutdown set, once a turn.
pub(crate) const IDLE_CAP: Duration = Duration::from_millis(10);

/// The shortest receive hold-off, however fast the receive buffer fills.
pub(crate) const HOLD_OFF_MIN: Duration = Duration::from_micros(100);

/// The longest receive hold-off, however slowly the receive buffer fills.
/// On loopback a datagram that the kernel split from a UDP GSO message is
/// charged about the whole message's memory, so the buffer holds far
/// fewer datagrams than its size says: about 0.5 ms of load in WSL2
///.
pub(crate) const HOLD_OFF_MAX: Duration = Duration::from_micros(250);

/// The hold-off is at most this share of the time the receive buffer
/// holds, as a margin for bursts and a late wake-up.
const HOLD_OFF_SHARE: u32 = 4;

/// How long the arrival rate is measured over.
pub(crate) const RATE_WINDOW: Duration = Duration::from_millis(10);

/// A Load Sender that reads datagrams again within this time of its last
/// read is flooded: Status PDUs come a Trial Interval apart.
pub(crate) const FLOOD_SPACING: Duration = Duration::from_millis(1);

/// Wake-ups in a row without progress before the engine backs off.
pub(crate) const SPIN_LIMIT: u32 = 8;

/// How many waits the engine sleeps through once it backs off.
pub(crate) const BACKOFF_SLEEPS: u32 = 64;

/// The longest sleep of a backed-off wait.
pub(crate) const BACKOFF_SLEEP: Duration = Duration::from_micros(250);

/// How long to wait from `now`: until the earliest of `deadlines`, at most
/// `cap`; zero if one is due.
pub(crate) fn timeout(
    now: MonoTime,
    deadlines: impl IntoIterator<Item = Option<MonoTime>>,
    cap: Duration,
) -> Duration {
    deadlines.into_iter().flatten().min().map_or(cap, |at| {
        at.as_duration().saturating_sub(now.as_duration()).min(cap)
    })
}

/// The receive hold-off for a receive buffer of `buffer` bytes that fills
/// at `rate` bytes a second: a [`HOLD_OFF_SHARE`] of the time it holds,
/// within [`HOLD_OFF_MIN`] and [`HOLD_OFF_MAX`]; the shortest while the
/// rate isn't known.
pub(crate) fn hold_off(buffer: usize, rate: Option<u64>) -> Duration {
    let Some(rate) = rate.filter(|&r| r > 0) else {
        return HOLD_OFF_MIN;
    };
    let holds_ns = buffer as u128 * 1_000_000_000 / u128::from(rate);
    let share = holds_ns / u128::from(HOLD_OFF_SHARE);
    Duration::from_nanos(u64::try_from(share).unwrap_or(u64::MAX)).clamp(HOLD_OFF_MIN, HOLD_OFF_MAX)
}

/// Whether more than one datagram is due within `hold_off` at `rate`
/// datagrams a second: otherwise holding off saves no wake-up. Yes
/// while the rate isn't known.
pub(crate) fn more_than_one_due(rate: Option<u64>, hold_off: Duration) -> bool {
    rate.is_none_or(|rate| u128::from(rate) * hold_off.as_nanos() > 1_000_000_000)
}

/// The rate datagrams arrive at, and their bytes, measured over windows of
/// [`RATE_WINDOW`].
#[derive(Debug, Default)]
struct ArrivalRate {
    since: Option<MonoTime>,
    datagrams: u64,
    bytes: u64,
    /// Over the last window closed, if anything arrived in it: a window in
    /// which nothing arrived says nothing of the rate to come.
    rate: Option<Arrivals>,
}

/// Datagrams, and their bytes, a second.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Arrivals {
    datagrams: u64,
    bytes: u64,
}

impl ArrivalRate {
    fn add(&mut self, datagrams: u64, bytes: u64, now: MonoTime) {
        let since = *self.since.get_or_insert(now);
        self.datagrams += datagrams;
        self.bytes += bytes;
        let elapsed = now.as_duration().saturating_sub(since.as_duration());
        if elapsed >= RATE_WINDOW {
            let per_second = |count: u64| {
                let rate = u128::from(count) * 1_000_000_000 / elapsed.as_nanos();
                u64::try_from(rate).unwrap_or(u64::MAX)
            };
            self.rate = (self.datagrams > 0).then(|| Arrivals {
                datagrams: per_second(self.datagrams),
                bytes: per_second(self.bytes),
            });
            self.since = Some(now);
            self.datagrams = 0;
            self.bytes = 0;
        }
    }
}

/// Counts wake-ups that made no progress, and says when to back off.
#[derive(Debug, Default)]
pub(crate) struct SpinGuard {
    streak: u32,
    sleeps: u32,
}

impl SpinGuard {
    /// Whether this wait should sleep rather than poll: the engine backed
    /// off and hasn't slept through its [`BACKOFF_SLEEPS`] yet.
    pub(crate) fn sleep_instead(&mut self) -> bool {
        if self.sleeps == 0 {
            return false;
        }
        self.sleeps -= 1;
        true
    }

    /// A wait ended early; `progressed` if what ended it was handled.
    pub(crate) fn woke(&mut self, progressed: bool) {
        if progressed {
            self.streak = 0;
            return;
        }
        self.streak += 1;
        if self.streak >= SPIN_LIMIT {
            self.sleeps = BACKOFF_SLEEPS;
            // One more wake-up without progress backs off again.
            self.streak = SPIN_LIMIT - 1;
        }
    }

    /// A wait ran to its timeout: nothing spins.
    pub(crate) fn timed_out(&mut self) {
        self.streak = 0;
    }
}

/// What ended a wait early, judged after the turn that follows it.
#[derive(Debug, Clone, Copy)]
struct Wake {
    /// The socket was readable: the turn should have read something.
    readable: bool,
    /// An error was pending and the wait drained one.
    drained: bool,
}

/// A Test Connection's readiness waits: its receive hold-off and its spin
/// guard.
#[derive(Debug)]
pub(crate) struct Readiness {
    load_receiver: bool,
    /// The receive buffer the OS granted, in bytes.
    buffer: usize,
    /// The hold-off the engine's settings fix (`UDPSTP_ENGINE_HOLD_OFF_US`).
    fixed: Option<Duration>,
    rate: ArrivalRate,
    last_read: Option<MonoTime>,
    guard: SpinGuard,
    wake: Option<Wake>,
}

impl Readiness {
    pub(crate) fn new(load_receiver: bool, buffer: usize, fixed: Option<Duration>) -> Self {
        Self {
            load_receiver,
            buffer,
            fixed,
            rate: ArrivalRate::default(),
            last_read: None,
            guard: SpinGuard::default(),
            wake: None,
        }
    }

    /// After a turn that read `read` datagrams of `bytes` bytes in all:
    /// judges the wake-up before it, and returns the hold-off for the next
    /// wait, during which POLLIN is left out; `None` to wait for it.
    pub(crate) fn after_turn(
        &mut self,
        read: usize,
        bytes: u64,
        now: MonoTime,
    ) -> Option<Duration> {
        if let Some(wake) = self.wake.take() {
            self.guard.woke(wake.drained || (wake.readable && read > 0));
        }
        self.rate.add(read as u64, bytes, now);
        if read == 0 {
            return None;
        }
        let flooded = self
            .last_read
            .is_some_and(|at| now.as_duration().saturating_sub(at.as_duration()) < FLOOD_SPACING);
        self.last_read = Some(now);
        if !self.load_receiver && !flooded {
            return None;
        }
        let hold_off = match self.fixed {
            // Set for a measurement: whatever is due within it.
            Some(fixed) => fixed,
            None => {
                let rate = self.rate.rate;
                let derived = hold_off(self.buffer, rate.map(|rate| rate.bytes));
                let due = more_than_one_due(rate.map(|rate| rate.datagrams), derived);
                if self.load_receiver && !due {
                    return None;
                }
                derived
            }
        };
        (!hold_off.is_zero()).then_some(hold_off)
    }

    /// Whether this wait should sleep rather than poll (the spin guard).
    pub(crate) fn sleep_instead(&mut self) -> bool {
        self.guard.sleep_instead()
    }

    /// How the wait ended; `drained` when it drained a pending error.
    pub(crate) fn woke(&mut self, woke: Woke, drained: bool) {
        match woke {
            Woke::TimedOut => self.guard.timed_out(),
            Woke::Ready { readable, .. } => self.wake = Some(Wake { readable, drained }),
            Woke::Interrupted => {
                self.wake = Some(Wake {
                    readable: false,
                    drained: false,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(us: u64) -> MonoTime {
        MonoTime::from_duration(Duration::from_micros(us))
    }

    const US: fn(u64) -> Duration = Duration::from_micros;

    // : the wait lasts until the earliest deadline, at nanosecond
    // resolution, and at most the cap; a deadline that has passed means
    // no wait.
    #[test]
    fn the_wait_lasts_until_the_earliest_deadline_within_the_cap() {
        let now = at(1_000);
        assert_eq!(timeout(now, [None, None], IDLE_CAP), IDLE_CAP);
        assert_eq!(
            timeout(now, [Some(at(1_150)), Some(at(5_000))], IDLE_CAP),
            US(150)
        );
        assert_eq!(timeout(now, [None, Some(at(5_000))], IDLE_CAP), US(4_000));
        assert_eq!(
            timeout(now, [Some(at(900)), None], IDLE_CAP),
            Duration::ZERO
        );
        assert_eq!(timeout(now, [Some(at(50_000)), None], IDLE_CAP), IDLE_CAP);
        let fine = MonoTime::from_duration(Duration::from_nanos(1_000_000_123));
        assert_eq!(
            timeout(at(1_000_000), [Some(fine)], IDLE_CAP),
            Duration::from_nanos(123)
        );
    }

    // : the hold-off is a quarter of the time the receive buffer
    // holds at the arrival rate, within 100 and 250 µs, and the shortest
    // while the rate isn't known.
    #[test]
    fn the_hold_off_follows_the_receive_buffer_and_the_rate() {
        const MIB: usize = 1 << 20;
        assert_eq!(hold_off(2 * MIB, None), HOLD_OFF_MIN);
        assert_eq!(hold_off(2 * MIB, Some(0)), HOLD_OFF_MIN);
        // 2 MiB at 1 Gbps holds 16.8 ms: capped.
        assert_eq!(hold_off(2 * MIB, Some(125_000_000)), HOLD_OFF_MAX);
        // 64 KiB at 1 Gbps holds 524 µs: a quarter is 131 µs.
        assert_eq!(
            hold_off(64 * 1024, Some(125_000_000)),
            Duration::from_nanos(131_072)
        );
        // 64 KiB at 10 Gbps holds 52 µs: at least the floor.
        assert_eq!(hold_off(64 * 1024, Some(1_250_000_000)), HOLD_OFF_MIN);
    }

    // , : the arrival rate, in datagrams and in bytes a second, is
    // measured over 10 ms windows; one in which nothing arrived leaves it
    // unknown, and one in which a datagram arrived doesn't, however long.
    #[test]
    fn the_arrival_rate_is_measured_over_windows() {
        let mut rate = ArrivalRate::default();
        rate.add(0, 0, at(0));
        rate.add(400, 500_000, at(5_000));
        assert_eq!(rate.rate, None, "no window has closed yet");
        rate.add(600, 750_000, at(10_000));
        let arrivals = Arrivals {
            datagrams: 100_000,
            bytes: 125_000_000,
        };
        assert_eq!(
            rate.rate,
            Some(arrivals),
            "1,000 datagrams, 1.25 MB, in 10 ms"
        );
        rate.add(0, 0, at(30_000));
        assert_eq!(rate.rate, None, "an idle window");
        rate.add(1, 472, at(2_030_000));
        let sparse = Arrivals {
            datagrams: 0,
            bytes: 236,
        };
        assert_eq!(rate.rate, Some(sparse), "a datagram in 2 s");
    }

    // : holding off is worth a wake-up only when more than one
    // datagram is due within the hold-off. Row 4 sends one Load PDU every
    // 1 ms (rows 1 to 9 send one Add-on every 1 ms, on the second
    // Transmitter): a quarter of one is due within the 250 µs hold-off the
    // ticket names. Row 100 sends one every 100 µs: two and a half are
    // due. Exactly one is not more than one.
    #[test]
    fn a_hold_off_is_worth_it_only_when_more_than_one_datagram_is_due() {
        assert!(!more_than_one_due(Some(1_000), US(250)), "row 4");
        assert!(more_than_one_due(Some(10_000), US(250)), "row 100");
        assert!(!more_than_one_due(Some(4_000), US(250)), "exactly one");
        assert!(more_than_one_due(Some(4_001), US(250)));
        assert!(
            !more_than_one_due(Some(10_000), HOLD_OFF_MIN),
            "exactly one"
        );
        assert!(!more_than_one_due(Some(0), US(250)), "under one a second");
        assert!(more_than_one_due(None, HOLD_OFF_MIN), "not known yet");
    }

    // , : a Load Receiver holds off after a turn that read, while
    // the rate isn't known and while more than one datagram is due within
    // the hold-off, and waits for arrivals after a turn that read nothing.
    #[test]
    fn a_load_receiver_holds_off_after_a_turn_that_read() {
        let mut readiness = Readiness::new(true, 2 << 20, None);
        assert_eq!(readiness.after_turn(0, 0, at(0)), None);
        assert_eq!(readiness.after_turn(3, 3_000, at(100)), Some(HOLD_OFF_MIN));
        assert_eq!(readiness.after_turn(0, 0, at(400)), None);
        // Once the rate is known, from the buffer and the rate: 1,000 Load
        // PDUs of 1,250 bytes in 10.5 ms, about 1 Gbps.
        readiness.after_turn(1_000, 1_250_000, at(10_500));
        assert_eq!(
            readiness.after_turn(1, 1_000, at(10_600)),
            Some(HOLD_OFF_MAX)
        );
    }

    // : at row 4 a Load Receiver holds off while the rate isn't known,
    // then waits for each arrival: a quarter of a Load PDU is due within
    // the hold-off. Row 4's IP packets are 500 bytes: 472 of UDP payload.
    #[test]
    fn a_load_receiver_at_row_4_waits_for_each_arrival() {
        let mut readiness = Readiness::new(true, 2 << 20, None);
        for ms in 0..10 {
            let hold_off = readiness.after_turn(1, 472, at(ms * 1_000));
            assert_eq!(hold_off, Some(HOLD_OFF_MIN), "{ms} ms: not known yet");
        }
        for ms in 10..100 {
            let hold_off = readiness.after_turn(1, 472, at(ms * 1_000));
            assert_eq!(hold_off, None, "{ms} ms");
        }
    }

    //  (fast Tests read in batches as before): a window in which
    // nothing arrived says nothing of the rate to come, so after one a Load
    // Receiver holds off as while the rate isn't known, as it did before
    // : a fast Test that starts, or goes on, after a pause of a window
    // reads in batches from its first turn.
    #[test]
    fn after_a_window_with_no_arrivals_a_load_receiver_holds_off_as_before() {
        let mut readiness = Readiness::new(true, 2 << 20, None);
        assert_eq!(readiness.after_turn(0, 0, at(0)), None);
        assert_eq!(readiness.after_turn(0, 0, at(10_000)), None, "it closes");
        // A Burst of row 1000: ten Load PDUs, IP packets of 1,250 bytes,
        // 1,222 bytes of UDP payload each.
        let hold_off = readiness.after_turn(10, 12_220, at(15_000));
        assert_eq!(hold_off, Some(HOLD_OFF_MIN));
    }

    /// The wake-ups of an engine whose waits `readiness` decides, as
    /// datagrams of `bytes` bytes arrive at `arrivals`, in order: a wait
    /// that leaves POLLIN out ends at its hold-off, one that doesn't at
    /// the next arrival, and each wake-up's turn reads what has arrived.
    /// No deadline ends a wait, and no wake-up is late.
    fn wake_ups(readiness: &mut Readiness, arrivals: &[Duration], bytes: u64) -> usize {
        let (mut now, mut read, mut next, mut wake_ups) = (Duration::ZERO, 0, 0, 0);
        loop {
            let turn_bytes = read as u64 * bytes;
            let hold_off = readiness.after_turn(read, turn_bytes, MonoTime::from_duration(now));
            let (woke, until) = match (hold_off, arrivals.get(next)) {
                (Some(hold_off), _) => (Woke::TimedOut, now + hold_off),
                (None, Some(&arrival)) => {
                    let readable = Woke::Ready {
                        readable: true,
                        error: false,
                    };
                    (readable, arrival.max(now))
                }
                (None, None) => return wake_ups,
            };
            readiness.woke(woke, false);
            wake_ups += 1;
            now = until;
            let first = next;
            while arrivals.get(next).is_some_and(|&arrival| arrival <= now) {
                next += 1;
            }
            read = next - first;
        }
    }

    //  (at row 4 a Load Receiver wakes about once per Load PDU; fast
    // Tests still read in batches): over 1 s of row 4 a Load Receiver
    // wakes once per Load PDU, and twice for each of the 9 before its
    // first window of 10 ms closes, while the rate isn't known: 991 + 2 ×
    // 9. Where Load PDUs come closer together than the hold-off, it wakes
    // once per hold-off, as before: at row 100, one Load PDU every 100 µs,
    // and at row 1000, ten every 100 µs, every 100 µs in the first window
    // (the shortest hold-off), then every 250 µs for 990 ms, and once more
    // for the hold-off after the last: 100 + 3,960 + 1. Row 50 sends a
    // Burst of five every 1 ms, 1.25 due within 250 µs, so it still holds
    // off and, its Bursts arriving back to back, wakes twice a Burst, as
    // before: the limit of counting datagrams, not Bursts.
    #[test]
    fn a_load_receiver_wakes_once_per_load_pdu_at_row_4_and_per_hold_off_when_fast() {
        let every = |spacing: u64, burst: usize, count: u64| -> Vec<Duration> {
            (1..=count)
                .flat_map(|i| std::iter::repeat_n(US(spacing * i), burst))
                .collect()
        };
        let receiver = || Readiness::new(true, 2 << 20, None);
        let row_4 = wake_ups(&mut receiver(), &every(1_000, 1, 1_000), 472);
        assert_eq!(row_4, 991 + 2 * 9, "row 4");
        let row_100 = wake_ups(&mut receiver(), &every(100, 1, 10_000), 1_222);
        assert_eq!(row_100, 100 + 3_960 + 1, "row 100");
        let row_1000 = wake_ups(&mut receiver(), &every(100, 10, 10_000), 1_222);
        assert_eq!(row_1000, 100 + 3_960 + 1, "row 1000");
        let row_50 = wake_ups(&mut receiver(), &every(1_000, 5, 1_000), 1_222);
        assert_eq!(row_50, 2 * 1_000, "row 50");
    }

    // , : a Load Sender takes each Status PDU at once, but holds
    // off when datagrams arrive closer together than a flood's spacing,
    // whatever the rate: 's rule is the Load Receiver's.
    #[test]
    fn a_load_sender_holds_off_only_when_flooded() {
        let mut readiness = Readiness::new(false, 2 << 20, None);
        assert_eq!(readiness.after_turn(1, 204, at(0)), None);
        assert_eq!(readiness.after_turn(1, 204, at(50_000)), None);
        assert_eq!(readiness.after_turn(2, 408, at(50_300)), Some(HOLD_OFF_MAX));
        assert_eq!(readiness.after_turn(0, 0, at(50_600)), None);
    }

    // , : UDPSTP_ENGINE_HOLD_OFF_US sets the hold-off for
    // measurements, taken after every turn that read, as before ,
    // whatever is due within it: 250 µs at row 4, a Load PDU every 1 ms,
    // too. 0 turns it off.
    #[test]
    fn a_set_hold_off_replaces_the_derived_one() {
        let mut fixed = Readiness::new(true, 2 << 20, Some(US(250)));
        for ms in 0..20 {
            let hold_off = fixed.after_turn(1, 472, at(ms * 1_000));
            assert_eq!(hold_off, Some(US(250)), "{ms} ms");
        }
        let mut none = Readiness::new(true, 2 << 20, Some(Duration::ZERO));
        assert_eq!(none.after_turn(1, 1_000, at(0)), None);
    }

    // : wake-ups that make no progress back the engine off to sleeps
    // after SPIN_LIMIT in a row; it then tries ppoll again, and backs off
    // at once if that still makes no progress.
    #[test]
    fn the_spin_guard_backs_off_after_wake_ups_without_progress() {
        let mut guard = SpinGuard::default();
        for _ in 0..SPIN_LIMIT - 1 {
            guard.woke(false);
            assert!(!guard.sleep_instead());
        }
        guard.woke(false);
        for _ in 0..BACKOFF_SLEEPS {
            assert!(guard.sleep_instead());
        }
        assert!(!guard.sleep_instead(), "then ppoll again");
        guard.woke(false);
        assert!(guard.sleep_instead(), "still no progress: back off again");
    }

    // : progress, or a wait that runs to its timeout, resets the
    // count.
    #[test]
    fn progress_resets_the_spin_guard() {
        let mut guard = SpinGuard::default();
        for _ in 0..SPIN_LIMIT - 1 {
            guard.woke(false);
        }
        guard.woke(true);
        for _ in 0..SPIN_LIMIT - 1 {
            guard.woke(false);
        }
        guard.timed_out();
        guard.woke(false);
        assert!(!guard.sleep_instead());
    }

    // : a wake-up is judged by what ended it: readable and then read,
    // or an error drained, is progress; readable with nothing to read, an
    // error left pending, or a signal, is not.
    #[test]
    fn a_wake_up_is_judged_by_what_ended_it() {
        let spins = |woke: Woke, drained: bool, read: usize| {
            let mut readiness = Readiness::new(false, 2 << 20, None);
            for i in 0..SPIN_LIMIT {
                readiness.woke(woke, drained);
                readiness.after_turn(read, read as u64 * 204, at(u64::from(i) * 10_000));
            }
            readiness.sleep_instead()
        };
        let readable = Woke::Ready {
            readable: true,
            error: false,
        };
        let error = Woke::Ready {
            readable: false,
            error: true,
        };
        assert!(!spins(readable, false, 1));
        assert!(spins(readable, false, 0));
        assert!(!spins(error, true, 0));
        assert!(spins(error, false, 0));
        assert!(spins(Woke::Interrupted, false, 0));
        assert!(!spins(Woke::TimedOut, false, 0));
    }
}
