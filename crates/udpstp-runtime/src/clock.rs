//! The clocks the core reads through [`Now`].

use std::time::{Duration, Instant, SystemTime};

use udpstp_core::{MonoTime, Now, WallTime};

/// Monotonic time since the runtime started, paired with the wall clock:
/// the Test Time Base (docs/spec.md "Time model"). The Control Phase and the
/// Data Phase Engine share it, as lpduTime needs.
///
/// On Linux it reads CLOCK_MONOTONIC_RAW, which NTP never slews, where
/// `Instant` reads CLOCK_MONOTONIC. Linux can't sleep on RAW, so a
/// wait is a duration from now on CLOCK_MONOTONIC (a sleep, a ppoll
/// timeout, a Tokio instant mapped from a fresh reading) and whoever waits
/// checks the time again on RAW when it wakes. Elsewhere it is `Instant`,
/// the platform's monotonic clock.
#[derive(Clone, Copy)]
pub(crate) struct Clock {
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    start: Instant,
    /// CLOCK_MONOTONIC_RAW at `start`.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    start_raw: Duration,
}

impl Clock {
    pub(crate) fn new() -> Self {
        Self {
            #[cfg(not(any(target_os = "linux", target_os = "android")))]
            start: Instant::now(),
            #[cfg(any(target_os = "linux", target_os = "android"))]
            start_raw: raw(),
        }
    }

    pub(crate) fn now(&self) -> Now {
        let (mono, wall) = self.pair();
        Now {
            mono: MonoTime::from_duration(mono),
            wall: WallTime::from_unix(wall),
        }
    }

    /// The Test Time Base alone: one clock read, where [`Clock::now`]
    /// takes three on Linux. For an executor that serves many Test
    /// Connections a turn (`engine::times::Turn`).
    #[cfg_attr(
        not(any(test, target_os = "linux", target_os = "android")),
        allow(dead_code)
    )]
    pub(crate) fn mono(&self) -> MonoTime {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let mono = raw().saturating_sub(self.start_raw);
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        let mono = self.start.elapsed();
        MonoTime::from_duration(mono)
    }

    /// The Test Time Base and the wall clock read together. On Linux the
    /// wall clock is read between two RAW readings, and paired with their
    /// midpoint, so a pair is off by at most half the time between them.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn pair(&self) -> (Duration, Duration) {
        let before = raw();
        let wall = unix_time();
        let after = raw();
        let mid = before + (after - before) / 2;
        (mid.saturating_sub(self.start_raw), wall)
    }

    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    fn pair(&self) -> (Duration, Duration) {
        (self.start.elapsed(), unix_time())
    }

    /// The Tokio instant of a core deadline: as far from now on Tokio's
    /// clock as it is on the Test Time Base.
    pub(crate) fn instant(&self, at: MonoTime) -> tokio::time::Instant {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            let now = Instant::now();
            let base = raw().saturating_sub(self.start_raw);
            tokio::time::Instant::from_std(now + at.as_duration().saturating_sub(base))
        }
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        tokio::time::Instant::from_std(self.start + at.as_duration())
    }
}

/// What reads the time: the [`Clock`], or in tests a scripted one.
pub(crate) trait Reading {
    /// The Test Time Base and the wall clock, read together.
    fn now(&self) -> Now;

    /// The Test Time Base alone.
    #[cfg_attr(
        not(any(test, target_os = "linux", target_os = "android")),
        allow(dead_code)
    )]
    fn mono(&self) -> MonoTime;
}

impl Reading for Clock {
    fn now(&self) -> Now {
        Clock::now(self)
    }

    fn mono(&self) -> MonoTime {
        Clock::mono(self)
    }
}

/// CLOCK_MONOTONIC_RAW.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn raw() -> Duration {
    use rustix::time::{ClockId, clock_gettime};
    let t = clock_gettime(ClockId::MonotonicRaw);
    // Never negative: it counts from boot.
    Duration::new(t.tv_sec as u64, t.tv_nsec as u32)
}

/// The wall clock, since the Unix epoch.
fn unix_time() -> Duration {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
}

/// A seed that differs between processes and runs; not for security.
pub(crate) fn seed() -> u64 {
    let wall = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    wall ^ u64::from(std::process::id()).rotate_left(32)
}

/// SplitMix64: a small, well-known generator; not for security.
pub(crate) struct SplitMix64(pub(crate) u64);

impl SplitMix64 {
    pub(crate) fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // docs/spec.md Time model: the Test Time Base never goes back, and a
    // Tokio deadline maps to the same distance ahead
    #[test]
    fn the_test_time_base_is_monotonic_and_deadlines_map_ahead() {
        let clock = Clock::new();
        let mut last = clock.now().mono;
        for _ in 0..10_000 {
            let now = clock.now().mono;
            assert!(now >= last);
            last = now;
        }
        let ahead = Duration::from_millis(250);
        let before = Instant::now();
        let at = clock.instant(MonoTime::from_duration(
            clock.now().mono.as_duration() + ahead,
        ));
        let after = Instant::now();
        let at = at.into_std();
        assert!(
            at >= before + ahead - Duration::from_millis(5),
            "{:?}",
            at - before
        );
        assert!(
            at <= after + ahead + Duration::from_millis(5),
            "{:?}",
            at - after
        );
        // An expired deadline is immediately ready. Linux rebases it onto
        // Instant; other platforms retain the runtime's original time base.
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let before = Instant::now();
        let past = clock
            .instant(MonoTime::from_duration(Duration::ZERO))
            .into_std();
        assert!(past <= Instant::now());
        #[cfg(any(target_os = "linux", target_os = "android"))]
        assert!(past >= before);
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        assert_eq!(past, clock.start);
    }

    // : the Test Time Base read alone is the one a pair reads, never
    // behind a pair read before it nor ahead of one read after it
    #[test]
    fn the_test_time_base_alone_is_the_pairs() {
        let clock = Clock::new();
        for _ in 0..10_000 {
            let before = clock.now().mono;
            let alone = clock.mono();
            let after = clock.now().mono;
            assert!(
                before <= alone && alone <= after,
                "{before:?} {alone:?} {after:?}"
            );
        }
    }

    // docs/spec.md Time model: the wall clock is read with the Test Time Base,
    // not apart from it
    #[test]
    fn a_pair_tracks_the_wall_clock() {
        let clock = Clock::new();
        let first = clock.now();
        std::thread::sleep(Duration::from_millis(50));
        let second = clock.now();
        let mono = second.mono.as_duration() - first.mono.as_duration();
        let wall = second.wall.as_unix().saturating_sub(first.wall.as_unix());
        let apart = mono.abs_diff(wall);
        assert!(apart < Duration::from_millis(5), "{mono:?} vs {wall:?}");
    }
}
