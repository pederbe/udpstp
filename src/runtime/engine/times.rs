//! When a Test Connection slot reads the clock, and which reading each
//! core call is given. Two schemes:
//!
//! - [`Fresh`], the per-Test-Connection executor's: every reading is a
//!   fresh clock pair, the Test Time Base and the wall clock read together
//!   (three `clock_gettime` calls on Linux), before and after every read
//!   of the socket too, whether it returned anything or not. So the
//!   Client, and every endpoint on Windows and macOS, read the clock as
//!   they always have.
//! - [`Turn`], for an executor that serves many slots a turn: one pair
//!   when the turn starts, and a fresh read of the Test Time Base alone
//!   wherever the core judges or stamps time (before a slot's first
//!   `poll_burst`, before each Send Plan's `write_datagrams`, for each
//!   `report_sent`, a due timeout, judged by the latest reading, and each
//!   `poll_transmit`), given with the wall clock
//!   carried on from the turn's pair: the core reads the wall clock only
//!   at second resolution there, and lpduTime is the Test Time Base plus
//!   the offset fixed at activation. A read of the socket is converted
//!   with the latest pair before it and a pair read after it, only when it
//!   returned datagrams, so every read that returned datagrams is
//!   bracketed by pairs, and one that returned nothing reads no clock.
//!   This reads docs/spec.md's Time model (receive timestamps are converted
//!   with clock pairs taken before and after each read) as a pair from any
//!   time before the read, with no other read of the socket between that
//!   returned datagrams, rather than one taken just before it.
//!
//! A wall-clock step within a turn shows at the next turn's pair.

use std::cell::Cell;

use crate::protocol::{MonoTime, Now, WallTime};

use crate::runtime::clock::Reading;

/// The readings a slot's work takes.
pub(crate) trait Times {
    /// For a core call that judges or stamps time.
    fn now(&self) -> Now;

    /// Where the latest reading will do: a later `poll_burst` of a turn's
    /// sending, and whether the turn's sending time is spent.
    fn latest(&self) -> Now;

    /// The pair a read of the socket is converted with, from before it.
    fn before_read(&self) -> Now;

    /// The pair after a read of the socket that returned `datagrams`.
    fn after_read(&self, datagrams: usize) -> Now;

    /// A reading for a core call due `at`, if it is due.
    fn due(&self, at: Option<MonoTime>) -> Option<Now>;
}

/// Every reading a fresh pair: the per-Test-Connection executor's.
pub(crate) struct Fresh<C>(pub(crate) C);

impl<C: Reading> Times for Fresh<C> {
    fn now(&self) -> Now {
        self.0.now()
    }

    fn latest(&self) -> Now {
        self.0.now()
    }

    fn before_read(&self) -> Now {
        self.0.now()
    }

    fn after_read(&self, _: usize) -> Now {
        self.0.now()
    }

    /// Judged by a fresh pair, which the call is then given.
    fn due(&self, at: Option<MonoTime>) -> Option<Now> {
        let now = self.0.now();
        at.is_some_and(|at| at <= now.mono).then_some(now)
    }
}

/// One pair a turn, the Test Time Base alone where the core needs it
/// fresh, and pairs only after reads that returned datagrams: for an
/// executor that serves many slots a turn. The slot's tests pin it
/// (`slot_tests.rs`); the Linux Server's pooled executor runs on it.
#[cfg_attr(
    not(any(test, target_os = "linux", target_os = "android")),
    allow(dead_code)
)]
pub(crate) struct Turn<C> {
    clock: C,
    /// The pair read as the turn started.
    started: Now,
    /// The latest pair: the turn's, or the one after the latest read that
    /// returned datagrams.
    pair: Cell<Now>,
    /// The latest reading of any kind.
    latest: Cell<Now>,
}

#[cfg_attr(
    not(any(test, target_os = "linux", target_os = "android")),
    allow(dead_code)
)]
impl<C: Reading> Turn<C> {
    /// A turn starting now: it reads a pair.
    pub(crate) fn start(clock: C) -> Self {
        let started = clock.now();
        Self {
            clock,
            started,
            pair: Cell::new(started),
            latest: Cell::new(started),
        }
    }
}

impl<C: Reading> Times for Turn<C> {
    fn now(&self) -> Now {
        let mono = self.clock.mono();
        let since = mono
            .as_duration()
            .saturating_sub(self.started.mono.as_duration());
        let now = Now {
            mono,
            wall: WallTime::from_unix(self.started.wall.as_unix() + since),
        };
        self.latest.set(now);
        now
    }

    fn latest(&self) -> Now {
        self.latest.get()
    }

    fn before_read(&self) -> Now {
        self.pair.get()
    }

    fn after_read(&self, datagrams: usize) -> Now {
        if datagrams == 0 {
            return self.pair.get();
        }
        let pair = self.clock.now();
        self.pair.set(pair);
        self.latest.set(pair);
        pair
    }

    /// Judged by the latest reading, reading nothing; a fresh reading of
    /// the Test Time Base for the call only when it is due.
    fn due(&self, at: Option<MonoTime>) -> Option<Now> {
        at.is_some_and(|at| at <= self.latest.get().mono)
            .then(|| self.now())
    }
}
