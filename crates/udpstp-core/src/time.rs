//! Time as the core receives it. The core never reads a clock itself.

use std::time::Duration;

/// A reading of a monotonic clock, relative to an arbitrary origin chosen by
/// the runtime. Used for every timer and interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MonoTime(Duration);

impl MonoTime {
    pub const fn from_duration(since_origin: Duration) -> Self {
        Self(since_origin)
    }

    pub const fn as_duration(self) -> Duration {
        self.0
    }
}

/// A reading of the wall clock, as time since the Unix epoch. Used only
/// where the protocol carries wall-clock time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WallTime(Duration);

impl WallTime {
    pub const fn from_unix(since_epoch: Duration) -> Self {
        Self(since_epoch)
    }

    pub const fn as_unix(self) -> Duration {
        self.0
    }
}

/// Monotonic and wall-clock readings taken together by the runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Now {
    pub mono: MonoTime,
    pub wall: WallTime,
}
