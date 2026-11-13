//! Quotas: how often, and how much, one Source or one key tests (RFC 9946
//! §11 item 6: "Testing ... SHOULD be limited in frequency and/or overall
//! volume of test traffic"; ). Off unless configured.
//!
//! A Quota allows a number of Tests and a number of load bytes per Quota
//! Period, as a bucket each that refills evenly over the period: a
//! subject that used it all waits until enough has refilled, rather than
//! for a window to end. A Test is charged when it first activates, and
//! its bytes when it ends, so a Test can overdraw the bytes: the subject
//! then waits longer.

use std::collections::HashMap;
use std::hash::Hash;
use std::time::Duration;

use crate::protocol::MonoTime;

/// What a Quota allows per Quota Period; `None` is no limit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QuotaLimits {
    /// Tests begun (a Test counts once, when it first activates).
    pub tests: Option<u32>,
    /// Load bytes sent and received, all its Test Connections together.
    pub bytes: Option<u64>,
}

impl QuotaLimits {
    pub fn is_limited(self) -> bool {
        self.tests.is_some() || self.bytes.is_some()
    }
}

/// The most Sources tracked at once. When a new one finds the table full,
/// those whose buckets are full again are forgotten; if that frees less
/// than half, only the most depleted half is kept, so a flood of new
/// Sources can't make the Server forget one that used its Quota. A
/// forgotten Source starts with a full Quota.
const CAPACITY: usize = 4096;

/// A bucket of units, in units × nanoseconds of the period: it refills by
/// `limit` every nanosecond, so a whole period refills `limit` units.
#[derive(Debug, Clone, Copy)]
struct Bucket {
    level: i128,
    at: MonoTime,
}

#[derive(Debug, Clone, Copy)]
struct Usage {
    tests: Bucket,
    bytes: Bucket,
}

/// The Quotas of a kind of subject: Sources, or Key Table rows.
#[derive(Debug)]
pub(crate) struct Quotas<K> {
    period: Duration,
    usage: HashMap<K, Usage>,
}

impl<K: Hash + Eq + Copy> Quotas<K> {
    pub(crate) fn new(period: Duration) -> Self {
        Self {
            period,
            usage: HashMap::new(),
        }
    }

    fn period_ns(&self) -> i128 {
        i128::try_from(self.period.as_nanos().max(1)).unwrap_or(i128::MAX)
    }

    fn full(&self, limit: Option<u64>) -> i128 {
        limit.map_or(i128::MAX, |l| {
            i128::from(l).saturating_mul(self.period_ns())
        })
    }

    fn refilled(&self, bucket: Bucket, limit: Option<u64>, now: MonoTime) -> i128 {
        let Some(l) = limit else {
            return i128::MAX;
        };
        let elapsed = now.as_duration().saturating_sub(bucket.at.as_duration());
        let gained = i128::try_from(elapsed.as_nanos())
            .unwrap_or(i128::MAX)
            .saturating_mul(i128::from(l));
        bucket.level.saturating_add(gained).min(self.full(limit))
    }

    fn fresh(&self, limits: QuotaLimits, now: MonoTime) -> Usage {
        Usage {
            tests: Bucket {
                level: self.full(limits.tests.map(u64::from)),
                at: now,
            },
            bytes: Bucket {
                level: self.full(limits.bytes),
                at: now,
            },
        }
    }

    /// Whether `key` may begin a Test: it has a whole Test and a byte left.
    pub(crate) fn allows(&self, key: K, limits: QuotaLimits, now: MonoTime) -> bool {
        if !limits.is_limited() {
            return true;
        }
        let Some(&usage) = self.usage.get(&key) else {
            return true;
        };
        let one = self.period_ns();
        self.refilled(usage.tests, limits.tests.map(u64::from), now) >= one
            && self.refilled(usage.bytes, limits.bytes, now) >= one
    }

    /// Charges `key` a Test begun.
    pub(crate) fn charge_test(&mut self, key: K, limits: QuotaLimits, now: MonoTime) {
        self.charge(key, limits, now, 1, 0);
    }

    /// Charges `key` the bytes a Test moved.
    pub(crate) fn charge_bytes(&mut self, key: K, limits: QuotaLimits, bytes: u64, now: MonoTime) {
        self.charge(key, limits, now, 0, bytes);
    }

    fn charge(&mut self, key: K, limits: QuotaLimits, now: MonoTime, tests: u64, bytes: u64) {
        if !limits.is_limited() {
            return;
        }
        if !self.usage.contains_key(&key) && self.usage.len() >= CAPACITY {
            self.prune(limits, now);
        }
        let usage = self.usage.get(&key).copied();
        let usage = usage.unwrap_or_else(|| self.fresh(limits, now));
        let one = self.period_ns();
        let spend = |quota: &Self, bucket: Bucket, limit: Option<u64>, units: u64| Bucket {
            level: quota
                .refilled(bucket, limit, now)
                .saturating_sub(i128::from(units).saturating_mul(one)),
            at: now,
        };
        let usage = Usage {
            tests: spend(self, usage.tests, limits.tests.map(u64::from), tests),
            bytes: spend(self, usage.bytes, limits.bytes, bytes),
        };
        self.usage.insert(key, usage);
    }

    /// Makes room for a new subject.
    fn prune(&mut self, limits: QuotaLimits, now: MonoTime) {
        let (tests, bytes) = (limits.tests.map(u64::from), limits.bytes);
        let (full_tests, full_bytes) = (self.full(tests), self.full(bytes));
        let depletion = |quota: &Self, usage: &Usage| {
            let t = full_tests.saturating_sub(quota.refilled(usage.tests, tests, now));
            let b = full_bytes.saturating_sub(quota.refilled(usage.bytes, bytes, now));
            // Relative to each bucket's size, so neither dominates.
            let rel = |spent: i128, full: i128| {
                if full <= 0 || full == i128::MAX {
                    0.0
                } else {
                    spent as f64 / full as f64
                }
            };
            rel(t, full_tests).max(rel(b, full_bytes))
        };
        let usage = std::mem::take(&mut self.usage);
        let mut kept: Vec<(K, Usage, f64)> = usage
            .into_iter()
            .map(|(k, u)| {
                let d = depletion(self, &u);
                (k, u, d)
            })
            .filter(|&(_, _, d)| d > 0.0)
            .collect();
        if kept.len() > CAPACITY / 2 {
            kept.sort_unstable_by(|a, b| b.2.total_cmp(&a.2));
            kept.truncate(CAPACITY / 2);
        }
        self.usage = kept.into_iter().map(|(k, u, _)| (k, u)).collect();
    }

    #[cfg(any(test, feature = "fuzzing"))]
    fn len(&self) -> usize {
        self.usage.len()
    }

    /// How many keys have Quota use, and the most that may.
    #[cfg(any(test, feature = "fuzzing"))]
    pub(crate) fn usage(&self) -> (usize, usize) {
        (self.len(), CAPACITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: u64) -> MonoTime {
        MonoTime::from_duration(Duration::from_secs(secs))
    }

    const HOUR: Duration = Duration::from_secs(3600);

    // RFC 9946 §11 item 6 (frequency);
    #[test]
    fn a_test_quota_refills_evenly_over_the_period() {
        let limits = QuotaLimits {
            tests: Some(2),
            bytes: None,
        };
        let mut quotas = Quotas::new(HOUR);
        assert!(quotas.allows(1, limits, at(0)));
        quotas.charge_test(1, limits, at(0));
        quotas.charge_test(1, limits, at(0));
        assert!(!quotas.allows(1, limits, at(0)), "both Tests used");
        assert!(!quotas.allows(1, limits, at(1799)), "half a Test refilled");
        assert!(quotas.allows(1, limits, at(1800)), "one Test refilled");
        assert!(quotas.allows(2, limits, at(0)), "another subject is apart");
    }

    // RFC 9946 §11 item 6 (volume);
    #[test]
    fn bytes_charged_after_a_test_can_overdraw_the_quota() {
        let limits = QuotaLimits {
            tests: None,
            bytes: Some(1_000_000),
        };
        let mut quotas = Quotas::new(HOUR);
        quotas.charge_bytes(1, limits, 3_000_000, at(0));
        // Two periods to pay back the overdraft, then one byte more.
        assert!(!quotas.allows(1, limits, at(3600 * 2)));
        assert!(quotas.allows(1, limits, at(3600 * 2 + 1)));
    }

    #[test]
    fn no_limit_allows_everything_and_tracks_nothing() {
        let mut quotas = Quotas::new(HOUR);
        let none = QuotaLimits::default();
        quotas.charge_test(1, none, at(0));
        assert!(quotas.allows(1, none, at(0)));
        assert_eq!(quotas.len(), 0);
    }

    // : a flood of new Sources can't make the Server forget one that
    // used its Quota
    #[test]
    fn pruning_keeps_the_most_depleted_subjects() {
        let limits = QuotaLimits {
            tests: Some(10),
            bytes: None,
        };
        let mut quotas = Quotas::new(HOUR);
        for _ in 0..10 {
            quotas.charge_test(0u32, limits, at(0));
        }
        for key in 1..=(CAPACITY as u32 + 10) {
            quotas.charge_test(key, limits, at(0));
        }
        assert!(quotas.len() <= CAPACITY);
        assert!(
            !quotas.allows(0, limits, at(0)),
            "the exhausted one is remembered"
        );
    }
}
