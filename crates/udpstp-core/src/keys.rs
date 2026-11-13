//! The in-core Key Table and authUnixTime handling (RFC 9946 §5.3.1, §5.4,
//! RFC 7210 §2-§3, ADR 0010).

use std::time::Duration;

use udpstp_wire::auth::SharedKey;

use crate::WallTime;

/// The KDF column: the only one UDPSTP uses, NIST SP 800-108 counter mode
/// with HMAC-SHA-256 and the UDPSTP label (RFC 9946 §5.4.1, ADR 0001). Its
/// IANA "KeyTable KDFs" name is `HMAC-SHA-256`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Kdf {
    #[default]
    HmacSha256,
}

impl Kdf {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HmacSha256 => "HMAC-SHA-256",
        }
    }
}

/// The AlgID column: the authDigest algorithm, HMAC-SHA-256. IANA's
/// "KeyTable AlgIDs" registry has no value for it, so the name mirrors the
/// KDF's (ADR 0010).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AlgId {
    #[default]
    HmacSha256,
}

impl AlgId {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HmacSha256 => "HMAC-SHA-256",
        }
    }
}

/// A send or accept lifetime (RFC 7210 §2): UTC bounds in whole seconds
/// since the Unix epoch, both inclusive, as RFC 7210's YYYYMMDDHHMMSSZ
/// format has. `None` leaves that end open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lifetime {
    pub start: Option<u64>,
    pub end: Option<u64>,
}

impl Lifetime {
    /// Valid at every time.
    pub const ALWAYS: Lifetime = Lifetime {
        start: None,
        end: None,
    };

    pub fn new(start: Option<u64>, end: Option<u64>) -> Self {
        Self { start, end }
    }

    /// A bound written in RFC 7210's `YYYYMMDDHHMMSSZ` format, as whole
    /// UTC seconds since the Unix epoch; `None` if it isn't a valid time in
    /// that format from 1970 on. Leap seconds (`60`) are not accepted.
    pub fn parse_time(text: &str) -> Option<u64> {
        let digits = text.strip_suffix('Z')?;
        if digits.len() != 14 || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let field = |range: std::ops::Range<usize>| digits[range].parse::<u64>().ok();
        let (year, month, day) = (field(0..4)?, field(4..6)?, field(6..8)?);
        let (hour, minute, second) = (field(8..10)?, field(10..12)?, field(12..14)?);
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let month_days = [
            31,
            if leap { 29 } else { 28 },
            31,
            30,
            31,
            30,
            31,
            31,
            30,
            31,
            30,
            31,
        ];
        if year < 1970
            || !(1..=12).contains(&month)
            || day == 0
            || day > month_days[month as usize - 1]
            || hour > 23
            || minute > 59
            || second > 59
        {
            return None;
        }
        let leap_years = |y: u64| y / 4 - y / 100 + y / 400;
        let year_days = 365 * (year - 1970) + leap_years(year - 1) - leap_years(1969);
        let days: u64 = year_days + month_days[..month as usize - 1].iter().sum::<u64>() + day - 1;
        Some(((days * 24 + hour) * 60 + minute) * 60 + second)
    }

    /// Whether `now`, in whole seconds, lies within the lifetime, edges
    /// included.
    pub fn contains(&self, now: WallTime) -> bool {
        let now = now.as_unix().as_secs();
        self.start.is_none_or(|start| start <= now) && self.end.is_none_or(|end| now <= end)
    }

    /// Whether no time at all lies within it.
    fn is_empty(&self) -> bool {
        matches!((self.start, self.end), (Some(start), Some(end)) if start > end)
    }

    /// Whether some time lies within both lifetimes.
    fn overlaps(&self, other: &Lifetime) -> bool {
        let later_start = self.start.max(other.start);
        let earlier_end = match (self.end, other.end) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        match (later_start, earlier_end) {
            (Some(start), Some(end)) => start <= end,
            _ => true,
        }
    }
}

/// Whose key a row holds: an extension to RFC 7210 (ADR 0010).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KeyRole {
    /// An ordinary Shared Key.
    #[default]
    Consumer,
    /// An Operator Key: its holder may run Fixed-Rate Tests, and Tests up
    /// to its own maximum Test Interval when it has one (beyond 60 s).
    Operator { max_test_interval: Option<Duration> },
}

/// One row of the Key Table (RFC 9946 §5.4, RFC 7210 §2), plus the
/// [`KeyRole`] extension (ADR 0010).
#[derive(Debug, Clone)]
pub struct KeyEntry {
    /// AdminKeyName: identifies the row for people.
    pub admin_key_name: String,
    /// LocalKeyName: the keyId in the PDUs.
    pub key_id: u8,
    pub kdf: Kdf,
    pub alg_id: AlgId,
    pub key: SharedKey,
    pub send_lifetime: Lifetime,
    pub accept_lifetime: Lifetime,
    /// Extension: whether this is an Operator Key.
    pub role: KeyRole,
    /// Extension: the row's Quota, per the Server's Quota Period; none by
    /// default. An Operator Key has none.
    pub quota: crate::QuotaLimits,
}

impl KeyEntry {
    /// A row with the given lifetimes, not an Operator Key.
    pub fn new(
        admin_key_name: &str,
        key_id: u8,
        key: Vec<u8>,
        send_lifetime: Lifetime,
        accept_lifetime: Lifetime,
    ) -> Self {
        Self {
            admin_key_name: admin_key_name.to_owned(),
            key_id,
            kdf: Kdf::HmacSha256,
            alg_id: AlgId::HmacSha256,
            key: SharedKey::new(key),
            send_lifetime,
            accept_lifetime,
            role: KeyRole::Consumer,
            quota: crate::QuotaLimits::default(),
        }
    }
}

/// Why a row can't join the Key Table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyTableError {
    /// Two rows share a keyId and their send or accept lifetimes overlap
    /// (ADR 0010).
    OverlappingLifetimes { key_id: u8, names: (String, String) },
    /// A lifetime starts after it ends, so the row could never be used.
    EmptyLifetime { name: String },
}

/// Why the Client has no key to send with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendKeyError {
    /// No row's send lifetime includes the current time.
    None,
    /// Several rows tie for the newest SendLifetimeStart.
    Ambiguous,
}

/// Why no row was found for a received keyId.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyLookupError {
    /// No row has this keyId.
    Unknown,
    /// Rows have this keyId, but none is within its accept lifetime now.
    OutsideLifetime,
}

/// The Key Table (RFC 9946 §5.4, RFC 7210 §2).
#[derive(Debug, Clone)]
pub struct KeyTable {
    entries: Vec<KeyEntry>,
    /// The position in `entries` of each row, bucketed by keyId, in insertion
    /// order. A received-keyId lookup then scans one keyId's rows rather than
    /// the whole table: the scan runs before the global budget for unverified
    /// Setup Requests, so a forged flood naming an absent or last keyId would
    /// otherwise pay a whole-table scan per datagram, unbounded by the budget
    ///. keyId is one byte, so 256 buckets cover every value.
    by_key_id: Box<[Vec<usize>; 256]>,
}

impl Default for KeyTable {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            by_key_id: Box::new(std::array::from_fn(|_| Vec::new())),
        }
    }
}

impl KeyTable {
    /// The rows, in the order they were added.
    pub fn entries(&self) -> &[KeyEntry] {
        &self.entries
    }

    /// Adds a row. Rows may share a keyId only when neither their send nor
    /// their accept lifetimes overlap.
    pub fn insert(&mut self, entry: KeyEntry) -> Result<(), KeyTableError> {
        if entry.send_lifetime.is_empty() || entry.accept_lifetime.is_empty() {
            return Err(KeyTableError::EmptyLifetime {
                name: entry.admin_key_name,
            });
        }
        let clash = self.entries.iter().find(|e| {
            e.key_id == entry.key_id
                && (e.send_lifetime.overlaps(&entry.send_lifetime)
                    || e.accept_lifetime.overlaps(&entry.accept_lifetime))
        });
        if let Some(existing) = clash {
            return Err(KeyTableError::OverlappingLifetimes {
                key_id: entry.key_id,
                names: (
                    existing.admin_key_name.clone(),
                    entry.admin_key_name.clone(),
                ),
            });
        }
        // The new row's position, indexed by its keyId. `entries` only ever
        // grows, so these positions stay valid.
        self.by_key_id[entry.key_id as usize].push(self.entries.len());
        self.entries.push(entry);
        Ok(())
    }

    /// The row for validating a received keyId now (RFC 7210 §3): its
    /// LocalKeyName is `key_id` and its accept lifetime includes `now`. With
    /// its position in the table, which identifies it. The keyId index gives
    /// the same row, in the same order, as a scan of the whole table would.
    pub(crate) fn accepting(
        &self,
        key_id: u8,
        now: WallTime,
    ) -> Result<(usize, &KeyEntry), KeyLookupError> {
        let rows = &self.by_key_id[key_id as usize];
        if rows.is_empty() {
            return Err(KeyLookupError::Unknown);
        }
        rows.iter()
            .map(|&i| (i, &self.entries[i]))
            .find(|(_, e)| e.accept_lifetime.contains(now))
            .ok_or(KeyLookupError::OutsideLifetime)
    }

    /// The row to send with now (RFC 7210 §3): its send lifetime includes
    /// `now`, it has keyId `pinned` if one is given, and among those the
    /// most recent SendLifetimeStart wins. A tie is an error rather than a
    /// guess: OB-UDPST key files, for one, have no lifetimes at all.
    pub fn sending(&self, now: WallTime, pinned: Option<u8>) -> Result<&KeyEntry, SendKeyError> {
        let valid = || {
            self.entries.iter().filter(move |e| {
                e.send_lifetime.contains(now) && pinned.is_none_or(|id| e.key_id == id)
            })
        };
        let newest = valid()
            .map(|e| e.send_lifetime.start)
            .max()
            .ok_or(SendKeyError::None)?;
        let mut candidates = valid().filter(|e| e.send_lifetime.start == newest);
        let chosen = candidates.next().ok_or(SendKeyError::None)?;
        match candidates.next() {
            Some(_) => Err(SendKeyError::Ambiguous),
            None => Ok(chosen),
        }
    }
}

/// Half of the 10-second authUnixTime tolerance window (RFC 9946 §5.3.1).
const AUTH_TIME_TOLERANCE_SECS: i64 = 5;

/// The wall clock as a 32-bit authUnixTime. The field wraps on
/// 2106-02-07 06:28:16 UTC; only the low 32 bits of the seconds are sent.
pub(crate) fn auth_unix_time(now: WallTime) -> u32 {
    now.as_unix().as_secs() as u32
}

/// Whether a received authUnixTime is within ±5 s of this host's wall
/// clock, edges included. The difference is taken modulo 2^32, so values on
/// either side of the 2106 wraparound compare correctly.
pub(crate) fn auth_time_acceptable(received: u32, now: WallTime) -> bool {
    let diff = i64::from(received.wrapping_sub(auth_unix_time(now)) as i32);
    diff.abs() <= AUTH_TIME_TOLERANCE_SECS
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A row with keyId `key_id` and send and accept lifetimes both
    /// `[start, end]`, so rows sharing a keyId can be added when disjoint.
    fn row(name: &str, key_id: u8, start: Option<u64>, end: Option<u64>) -> KeyEntry {
        let lifetime = Lifetime::new(start, end);
        KeyEntry::new(name, key_id, vec![0; 32], lifetime, lifetime)
    }

    /// RFC 7210 §3's lookup by a plain scan of the whole table, the
    /// independent oracle the keyId index must match.
    fn linear(table: &KeyTable, key_id: u8, now: WallTime) -> Result<usize, KeyLookupError> {
        let mut any = false;
        for (i, e) in table.entries().iter().enumerate() {
            if e.key_id == key_id {
                any = true;
                if e.accept_lifetime.contains(now) {
                    return Ok(i);
                }
            }
        }
        if any {
            Err(KeyLookupError::OutsideLifetime)
        } else {
            Err(KeyLookupError::Unknown)
        }
    }

    // The keyId index returns the same row, and the same Unknown vs
    // OutsideLifetime, as a whole-table scan (RFC 7210 §3).
    #[test]
    fn accepting_by_index_matches_a_linear_scan() {
        let mut table = KeyTable::default();
        table.insert(row("a", 1, None, Some(100))).unwrap();
        // keyId 7 has three rows with disjoint lifetimes.
        table.insert(row("b", 7, Some(0), Some(50))).unwrap();
        table.insert(row("c", 7, Some(51), Some(100))).unwrap();
        table.insert(row("d", 7, Some(101), Some(200))).unwrap();
        table.insert(row("e", 200, Some(150), None)).unwrap();
        for key_id in [0u8, 1, 7, 200, 255] {
            for secs in [0u64, 25, 50, 51, 100, 101, 150, 250] {
                let now = WallTime::from_unix(Duration::from_secs(secs));
                let got = table.accepting(key_id, now).map(|(i, _)| i);
                assert_eq!(
                    got,
                    linear(&table, key_id, now),
                    "key_id={key_id} secs={secs}"
                );
            }
        }
    }
}
