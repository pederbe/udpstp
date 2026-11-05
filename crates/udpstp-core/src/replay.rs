//! The Server's replay cache (RFC 9946 §5.3.1; docs/spec.md "Control Phase and
//! admission (Server)", story 84).
//!
//! The ±5 s authUnixTime window alone lets a captured Setup or Activation
//! Request be replayed within it. The
//! cache remembers each verified request's keyId and authDigest for as long
//! as it could pass the window, so a copy is dropped whichever socket it
//! reaches: the Standard Port or any Ephemeral Port, including a sibling
//! Test Connection's. Only requests that passed the HMAC and time checks
//! are remembered, so unauthenticated datagrams can't fill it.
//! Unauthenticated Mode has no digest and bypasses it.
//!
//! Activation Requests are also keyed on the address they came from. The
//! Test Connections of a Multi-Connection Test set up in the same second
//! derive the same keys, and their Activation Requests carry nothing of
//! their own, so they are identical, digest and all. Each Test Connection
//! sends from its own port and an Activation Request must come from its
//! Client's IP address, so a verbatim copy sent to a sibling's Ephemeral
//! Port is caught, while the sibling's own request passes. A copy spoofed
//! from another port of the Client's IP address isn't
//! (conformance: replay-cache-activation-keyed-on-sender, replay-cache-lifetime-and-capacity).

use std::collections::{HashSet, VecDeque};
use std::net::SocketAddr;
use std::time::Duration;

use udpstp_wire::AUTH_DIGEST_LEN;

use crate::MonoTime;

/// How long a request can pass the ±5 s window after it arrives: 5 s after
/// an authUnixTime stamped up to 5 s ahead, plus the second authUnixTime
/// truncates. Measured on the monotonic clock, so a wall clock step forward
/// can't expire entries early; a step back of 5 s or more can bring an
/// expired request's time back into the window.
const LIFETIME: Duration = Duration::from_secs(11);

/// The most requests remembered, whatever the budgets allow: about 2 million,
/// some 150 MB at most.
const MAX_CAPACITY: usize = 1 << 21;

/// The capacity for a Server admitting `setups_per_second` unverified Setup
/// Requests a second and holding up to `test_connections` Test Connections:
/// every Setup Request the global budget lets through within a lifetime,
/// plus an Activation Request for each Test Connection several times over.
/// Setup Requests are remembered only once they are within their Source's
/// budget too, so the cache can't fill faster than the budgets allow.
pub(crate) fn capacity(setups_per_second: u32, test_connections: usize) -> usize {
    let setups = u64::from(setups_per_second) * LIFETIME.as_secs();
    let activations = test_connections.saturating_mul(4) as u64;
    usize::try_from(setups.saturating_add(activations))
        .unwrap_or(MAX_CAPACITY)
        .clamp(1024, MAX_CAPACITY)
}

/// keyId, authDigest, and for an Activation Request the sender's address.
type Entry = (u8, [u8; AUTH_DIGEST_LEN], Option<SocketAddr>);

/// What the cache made of a verified request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sight {
    First,
    Replay,
    /// The cache is full, so it can't tell.
    Full,
}

#[derive(Debug)]
pub(crate) struct ReplayCache {
    capacity: usize,
    entries: HashSet<Entry>,
    /// The entries in arrival order, with when each expires.
    expiries: VecDeque<(MonoTime, Entry)>,
}

impl ReplayCache {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: HashSet::new(),
            expiries: VecDeque::new(),
        }
    }

    /// How many requests it holds, and the most it may.
    pub(crate) fn usage(&self) -> (usize, usize) {
        (self.entries.len(), self.capacity)
    }

    fn expire(&mut self, now: MonoTime) {
        while let Some(&(at, entry)) = self.expiries.front() {
            if at > now {
                break;
            }
            self.expiries.pop_front();
            self.entries.remove(&entry);
        }
    }

    /// Whether a verified request was seen within its lifetime, without
    /// remembering it.
    pub(crate) fn seen(
        &mut self,
        key_id: u8,
        auth_digest: [u8; AUTH_DIGEST_LEN],
        sender: Option<SocketAddr>,
        now: MonoTime,
    ) -> bool {
        self.expire(now);
        self.entries.contains(&(key_id, auth_digest, sender))
    }

    /// Remembers a verified request not yet seen, and whether there was
    /// room.
    pub(crate) fn remember(
        &mut self,
        key_id: u8,
        auth_digest: [u8; AUTH_DIGEST_LEN],
        sender: Option<SocketAddr>,
        now: MonoTime,
    ) -> bool {
        self.expire(now);
        if self.entries.len() >= self.capacity {
            return false;
        }
        let entry = (key_id, auth_digest, sender);
        if self.entries.insert(entry) {
            let expiry = MonoTime::from_duration(now.as_duration() + LIFETIME);
            self.expiries.push_back((expiry, entry));
        }
        true
    }

    /// Remembers a verified request's keyId and authDigest, with the
    /// sender's address for an Activation Request.
    pub(crate) fn sight(
        &mut self,
        key_id: u8,
        auth_digest: [u8; AUTH_DIGEST_LEN],
        sender: Option<SocketAddr>,
        now: MonoTime,
    ) -> Sight {
        if self.seen(key_id, auth_digest, sender, now) {
            Sight::Replay
        } else if self.remember(key_id, auth_digest, sender, now) {
            Sight::First
        } else {
            Sight::Full
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: u64) -> MonoTime {
        MonoTime::from_duration(Duration::from_millis(ms))
    }

    const CAPACITY: usize = 1 << 16;

    #[test]
    fn an_entry_lasts_11_s() {
        let mut cache = ReplayCache::new(CAPACITY);
        assert_eq!(cache.sight(1, [7; 32], None, at(0)), Sight::First);
        assert_eq!(cache.sight(1, [7; 32], None, at(10_999)), Sight::Replay);
        // Another keyId, digest or sender is another request.
        assert_eq!(cache.sight(2, [7; 32], None, at(10_999)), Sight::First);
        assert_eq!(cache.sight(1, [8; 32], None, at(10_999)), Sight::First);
        let from = "192.0.2.9:4000".parse().ok();
        assert_eq!(cache.sight(1, [7; 32], from, at(10_999)), Sight::First);
        assert_eq!(cache.sight(1, [7; 32], from, at(10_999)), Sight::Replay);
        assert_eq!(cache.sight(1, [7; 32], None, at(11_000)), Sight::First);
    }

    #[test]
    fn a_full_cache_refuses_new_requests_until_entries_expire() {
        let mut cache = ReplayCache::new(CAPACITY);
        for i in 0..CAPACITY {
            let digest: [u8; 32] = std::array::from_fn(|b| (i >> (8 * (b % 4))) as u8);
            assert_eq!(cache.sight(1, digest, None, at(0)), Sight::First);
        }
        assert_eq!(cache.sight(2, [0; 32], None, at(1)), Sight::Full);
        assert_eq!(cache.sight(2, [0; 32], None, at(11_000)), Sight::First);
        assert_eq!(cache.entries.len(), 1);
    }

    #[test]
    fn seen_doesnt_remember() {
        let mut cache = ReplayCache::new(CAPACITY);
        assert!(!cache.seen(1, [7; 32], None, at(0)));
        assert!(!cache.seen(1, [7; 32], None, at(0)));
        assert!(cache.remember(1, [7; 32], None, at(0)));
        assert!(cache.seen(1, [7; 32], None, at(1)));
    }

    #[test]
    fn the_capacity_covers_what_the_budgets_let_through() {
        assert_eq!(capacity(20_000, 256), 220_000 + 1024);
        assert_eq!(capacity(1, 1), 1024);
        assert_eq!(capacity(u32::MAX, usize::MAX), MAX_CAPACITY);
    }
}
