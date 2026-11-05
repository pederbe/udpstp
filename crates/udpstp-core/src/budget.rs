//! Defences against floods of Setup Requests (RFC 9946 §11; docs/spec.md "Rate
//! limits", stories 85 to 87): a global budget for unverified requests,
//! spent once the keyId's row is found and before any KDF run or HMAC; a
//! cache of Derived Keys, so requests sharing a Key Table row and an
//! authUnixTime cost one KDF run between them; and a budget per Source,
//! spent only by authentic ones.

use std::collections::HashMap;
use std::hash::Hash;
use std::time::Duration;

use udpstp_wire::auth::DerivedKeys;

use crate::keys::{KeyEntry, auth_time_acceptable};
use crate::{MonoTime, WallTime};

/// At most a fixed number of events in each one-second window. A window
/// starts with its first event, so a burst at a window's end and the next
/// window's start can reach twice the limit within a second.
#[derive(Debug, Clone)]
pub(crate) struct PerSecond {
    limit: u32,
    start: Option<MonoTime>,
    count: u32,
}

impl PerSecond {
    pub(crate) fn new(limit: u32) -> Self {
        Self {
            limit,
            start: None,
            count: 0,
        }
    }

    /// Spends one event of the budget, and whether there was one to spend.
    pub(crate) fn take(&mut self, now: MonoTime) -> bool {
        let elapsed = self
            .start
            .map(|start| now.as_duration().saturating_sub(start.as_duration()));
        if elapsed.is_none_or(|e| e >= Duration::from_secs(1)) {
            (self.start, self.count) = (Some(now), 0);
        }
        if self.count >= self.limit {
            return false;
        }
        self.count += 1;
        true
    }
}

/// The most keys tracked at once. When a new key finds the table full, keys
/// whose budget is full again are forgotten, as their next request would
/// find it full anyway; if that frees less than half, only the most
/// recently used half is kept. A forgotten key starts full, so an attacker
/// cycling through many keys gains nothing it couldn't by spoofing them.
const KEY_CAPACITY: usize = 4096;

/// A nanosecond's worth of one token a second: tokens are counted in these,
/// so no refill time is lost to rounding.
const PER_TOKEN: u128 = 1_000_000_000;

/// What a key's budget made of a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Spend {
    /// A token was spent.
    Spent,
    /// None left, the first refusal since the key last spent one.
    FirstRefusal,
    /// None left, and refused before.
    Refused,
}

/// A key's bucket.
#[derive(Debug, Clone, Copy)]
struct Bucket {
    /// Tokens, in units of PER_TOKEN, as of `at`.
    units: u128,
    at: MonoTime,
    /// Refused since the last token was spent.
    refused: bool,
}

/// A token bucket per key: each holds up to `burst` tokens and gains
/// `per_second` a second; each request takes one.
#[derive(Debug)]
pub(crate) struct Budgets<K> {
    burst: u32,
    per_second: u32,
    buckets: HashMap<K, Bucket>,
}

impl<K: Hash + Eq + Copy> Budgets<K> {
    pub(crate) fn new(burst: u32, per_second: u32) -> Self {
        Self {
            burst,
            per_second,
            buckets: HashMap::new(),
        }
    }

    /// How many keys have a bucket, and the most that may.
    pub(crate) fn usage(&self) -> (usize, usize) {
        (self.buckets.len(), KEY_CAPACITY)
    }

    fn full(&self) -> u128 {
        u128::from(self.burst) * PER_TOKEN
    }

    fn refilled(&self, bucket: Bucket, now: MonoTime) -> u128 {
        let elapsed = now.as_duration().saturating_sub(bucket.at.as_duration());
        let gained = elapsed
            .as_nanos()
            .saturating_mul(u128::from(self.per_second));
        bucket.units.saturating_add(gained).min(self.full())
    }

    /// Makes room for a new key.
    fn prune(&mut self, now: MonoTime) {
        let full = self.full();
        let buckets = std::mem::take(&mut self.buckets);
        let mut kept: Vec<(K, Bucket)> = buckets
            .into_iter()
            .filter(|&(_, b)| self.refilled(b, now) < full)
            .collect();
        if kept.len() > KEY_CAPACITY / 2 {
            kept.sort_unstable_by_key(|&(_, b)| std::cmp::Reverse(b.at));
            kept.truncate(KEY_CAPACITY / 2);
        }
        self.buckets = kept.into_iter().collect();
    }

    /// Spends one token of `key`'s budget, if there is one.
    pub(crate) fn spend(&mut self, key: K, now: MonoTime) -> Spend {
        let bucket = match self.buckets.get(&key) {
            Some(&bucket) => bucket,
            None => {
                if self.buckets.len() >= KEY_CAPACITY {
                    self.prune(now);
                }
                Bucket {
                    units: self.full(),
                    at: now,
                    refused: false,
                }
            }
        };
        let units = self.refilled(bucket, now);
        let (units, refused, spend) = if units >= PER_TOKEN {
            (units - PER_TOKEN, false, Spend::Spent)
        } else if bucket.refused {
            (units, true, Spend::Refused)
        } else {
            (units, true, Spend::FirstRefusal)
        };
        self.buckets.insert(
            key,
            Bucket {
                units,
                at: now,
                refused,
            },
        );
        spend
    }
}

/// The most Derived Keys cached. Only authUnixTimes within the window are
/// cached, and at most 256 rows accept at once (one per keyId, ADR 0010),
/// so 11 seconds of distinct times for all of them fit.
const KEY_CACHE_CAPACITY: usize = 4096;

/// Derived Keys by Key Table row and authUnixTime. The row, not the keyId,
/// identifies the Shared Key, since rows sharing a keyId take turns (ADR
/// 0010); the caller looks the row up by its accept lifetime first, so a
/// hit never outlives the row. The Key Table is loaded once; reloading it
/// would have to clear the cache.
pub(crate) struct DerivedKeyCache {
    keys: HashMap<(usize, u32), DerivedKeys>,
    /// KDF runs so far.
    kdf_runs: u64,
}

impl Default for DerivedKeyCache {
    /// Allocated at full size, so growing never copies keys into memory
    /// that is freed without being wiped.
    fn default() -> Self {
        Self {
            keys: HashMap::with_capacity(KEY_CACHE_CAPACITY),
            kdf_runs: 0,
        }
    }
}

impl DerivedKeyCache {
    /// The Derived Keys of the row at `row` for a first Setup Request
    /// stamped `auth_unix_time`, derived once while that time can pass the
    /// window. One outside it is derived without being kept: the request
    /// fails anyway, and keeping it would let old times fill the cache.
    /// (conformance: derived-keys-depend-only-on-the-row-and-time)
    pub(crate) fn keys(
        &mut self,
        row: usize,
        entry: &KeyEntry,
        auth_unix_time: u32,
        now: WallTime,
    ) -> DerivedKeys {
        if !auth_time_acceptable(auth_unix_time, now) {
            self.kdf_runs += 1;
            return entry.key.derive(auth_unix_time);
        }
        if let Some(keys) = self.keys.get(&(row, auth_unix_time)) {
            return keys.clone();
        }
        if self.keys.len() >= KEY_CACHE_CAPACITY {
            self.keys.retain(|&(_, t), _| auth_time_acceptable(t, now));
            if self.keys.len() >= KEY_CACHE_CAPACITY {
                self.keys.clear();
            }
        }
        self.kdf_runs += 1;
        let keys = entry.key.derive(auth_unix_time);
        self.keys.insert((row, auth_unix_time), keys.clone());
        keys
    }

    /// KDF runs so far.
    /// How many Derived Keys are cached, and the most that may be.
    pub(crate) fn usage(&self) -> (usize, usize) {
        (self.keys.len(), KEY_CACHE_CAPACITY)
    }

    pub(crate) fn kdf_runs(&self) -> u64 {
        self.kdf_runs
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{AlgId, Kdf, KeyRole, Lifetime};
    use udpstp_wire::auth::SharedKey;

    fn at(ms: u64) -> MonoTime {
        MonoTime::from_duration(Duration::from_millis(ms))
    }

    fn spent(budgets: &mut Budgets<u32>, key: u32, ms: u64) -> bool {
        budgets.spend(key, at(ms)) == Spend::Spent
    }

    #[test]
    fn each_key_has_its_own_bucket_that_refills() {
        let mut budgets = Budgets::new(2, 4);
        assert!(spent(&mut budgets, 1, 0) && spent(&mut budgets, 1, 0));
        assert_eq!(budgets.spend(1, at(0)), Spend::FirstRefusal);
        assert_eq!(budgets.spend(1, at(0)), Spend::Refused);
        assert!(spent(&mut budgets, 2, 0), "another key's bucket is full");
        // A quarter of a second gains one.
        assert!(!spent(&mut budgets, 1, 249));
        assert!(spent(&mut budgets, 1, 250));
        // Refusals start over once a token was spent.
        assert_eq!(budgets.spend(1, at(250)), Spend::FirstRefusal);
        // Never more than the burst.
        assert!(spent(&mut budgets, 3, 0));
        assert!(spent(&mut budgets, 3, 60_000) && spent(&mut budgets, 3, 60_000));
        assert!(!spent(&mut budgets, 3, 60_000));
    }

    #[test]
    fn refill_time_isnt_lost_to_frequent_requests() {
        // Asked every 0.1 ms, a key gaining 1000 a second still gets them.
        let mut budgets = Budgets::new(1, 1000);
        let mut spent_count = 0;
        for tick in 0..10_000u64 {
            let now = MonoTime::from_duration(Duration::from_micros(tick * 100));
            if budgets.spend(1, now) == Spend::Spent {
                spent_count += 1;
            }
        }
        assert!((1000..=1001).contains(&spent_count), "{spent_count}");
    }

    #[test]
    fn a_huge_refill_rate_doesnt_overflow() {
        let mut budgets = Budgets::new(u32::MAX, u32::MAX);
        assert!(spent(&mut budgets, 1, 0));
        assert!(
            budgets
                .spend(1, MonoTime::from_duration(Duration::from_secs(1 << 40)))
                .eq(&Spend::Spent)
        );
    }

    #[test]
    fn the_table_stays_within_its_capacity() {
        // Nothing full again: the most recent half is kept.
        let mut budgets = Budgets::new(1, 1);
        for key in 0..KEY_CAPACITY as u32 {
            budgets.spend(key, at(u64::from(key) / 1000));
        }
        budgets.spend(u32::MAX, at(10));
        assert_eq!(budgets.buckets.len(), KEY_CAPACITY / 2 + 1);
        for _ in 0..3 {
            for key in 0..KEY_CAPACITY as u32 * 2 {
                budgets.spend(key + 10_000, at(20));
            }
        }
        assert!(budgets.buckets.len() <= KEY_CAPACITY);
        // Full again: forgotten first.
        let mut budgets = Budgets::new(1, 1);
        for key in 0..KEY_CAPACITY as u32 {
            budgets.spend(key, at(0));
        }
        budgets.spend(u32::MAX, at(1000));
        assert_eq!(budgets.buckets.len(), 1);
    }

    #[test]
    fn a_budget_allows_its_limit_each_second() {
        let mut budget = PerSecond::new(3);
        assert!((0..3).all(|_| budget.take(at(0))));
        assert!(!budget.take(at(999)));
        assert!(budget.take(at(1000)));
    }

    const T0: u32 = 1_790_000_000;

    fn row() -> KeyEntry {
        KeyEntry {
            admin_key_name: "k".into(),
            key_id: 1,
            kdf: Kdf::default(),
            alg_id: AlgId::default(),
            key: SharedKey::new(b"a key".to_vec()),
            send_lifetime: Lifetime::ALWAYS,
            accept_lifetime: Lifetime::ALWAYS,
            role: KeyRole::Consumer,
            quota: crate::QuotaLimits::default(),
        }
    }

    fn wall(secs: u32) -> WallTime {
        WallTime::from_unix(Duration::from_secs(secs.into()))
    }

    #[test]
    fn requests_sharing_a_row_and_time_cost_one_kdf_run() {
        let mut cache = DerivedKeyCache::default();
        let entry = row();
        let first = cache.keys(0, &entry, T0, wall(T0));
        for _ in 0..100 {
            let again = cache.keys(0, &entry, T0, wall(T0 + 2));
            assert_eq!(again.client(), first.client());
        }
        assert_eq!(cache.kdf_runs(), 1);
        // Another row or time is another derivation.
        cache.keys(1, &entry, T0, wall(T0));
        cache.keys(0, &entry, T0 + 1, wall(T0));
        assert_eq!(cache.kdf_runs(), 3);
    }

    #[test]
    fn a_time_outside_the_window_is_derived_but_not_kept() {
        let mut cache = DerivedKeyCache::default();
        let entry = row();
        let keys = cache.keys(0, &entry, T0 - 60, wall(T0));
        assert_eq!(keys.client(), entry.key.derive(T0 - 60).client());
        cache.keys(0, &entry, T0 - 60, wall(T0));
        assert_eq!(cache.kdf_runs(), 2);
        assert!(cache.keys.is_empty());
    }

    #[test]
    fn a_full_cache_drops_times_that_left_the_window() {
        let mut cache = DerivedKeyCache::default();
        let entry = row();
        for r in 0..KEY_CACHE_CAPACITY - 1 {
            cache.keys(r, &entry, T0, wall(T0));
        }
        // One still in the window six seconds on.
        cache.keys(0, &entry, T0 + 3, wall(T0 + 3));
        // Six seconds on the T0 entries are out of the window and go; the
        // T0 + 3 one stays.
        cache.keys(1, &entry, T0 + 6, wall(T0 + 6));
        assert_eq!(cache.keys.len(), 2);
        let runs = cache.kdf_runs();
        cache.keys(0, &entry, T0 + 3, wall(T0 + 6));
        assert_eq!(cache.kdf_runs(), runs, "kept, not derived again");
    }
}
