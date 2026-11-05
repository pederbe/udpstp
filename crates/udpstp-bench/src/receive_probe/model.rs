//! Pure packet, finite sequence and saved-evidence contracts.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{ReceiveMode, Workload};

pub const VERSION: u32 = 1;
pub const HEADER: usize = 32;
pub const PAYLOAD: usize = 1172;
pub const IP_BYTES: usize = 1200;
pub const MAX_AGGREGATE: usize = 65535;
const MAGIC: &[u8; 8] = b"UDPRCV01";
const RANGE_LIMIT: usize = 4096;
const HISTOGRAM_HALF: i64 = 1_000_000;
const HISTOGRAM_BINS: usize = (HISTOGRAM_HALF as usize) * 2 + 1;

/// All fields are big endian. The last field is CLOCK_MONOTONIC_RAW nanoseconds.
pub fn packet_header(run_id: u64, sequence: u64, sender_ns: u64) -> [u8; HEADER] {
    let mut bytes = [0; HEADER];
    bytes[..8].copy_from_slice(MAGIC);
    bytes[8..16].copy_from_slice(&run_id.to_be_bytes());
    bytes[16..24].copy_from_slice(&sequence.to_be_bytes());
    bytes[24..32].copy_from_slice(&sender_ns.to_be_bytes());
    bytes
}

pub fn decode_header(bytes: &[u8], run_id: u64) -> Result<(u64, u64), &'static str> {
    if bytes.len() < HEADER || &bytes[..8] != MAGIC {
        return Err("malformed diagnostic header");
    }
    let word =
        |at| u64::from_be_bytes(bytes[at..at + 8].try_into().expect("checked header length"));
    if word(8) != run_id {
        return Err("wrong run identity");
    }
    Ok((word(16), word(24)))
}

/// Segment ranges include a final short segment. Shape validation follows splitting.
#[cfg_attr(feature = "hotpath", hotpath::measure)]
pub fn segments(bytes: usize, gro_size: Option<i32>) -> Result<Segments, &'static str> {
    if bytes == 0 || bytes > MAX_AGGREGATE {
        return Err("invalid aggregate length");
    }
    let size = match gro_size {
        Some(n) if n > 0 && n as usize <= bytes => n as usize,
        Some(_) => return Err("invalid UDP GRO segment size"),
        None => bytes,
    };
    // At least a diagnostic header is required per segment. This also bounds
    // splitting malformed tiny-segment metadata to at most 2048 entries.
    if size < HEADER {
        return Err("GRO segment shorter than diagnostic header");
    }
    Ok(Segments { at: 0, bytes, size })
}

/// Allocation-free ranges over one bounded receive message.
pub struct Segments {
    at: usize,
    bytes: usize,
    size: usize,
}
impl Iterator for Segments {
    type Item = std::ops::Range<usize>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.at == self.bytes {
            return None;
        }
        let start = self.at;
        self.at = (start + self.size).min(self.bytes);
        Some(start..self.at)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SequenceRange {
    pub first: u64,
    pub end: u64,
}

/// Consecutive ranges, capped for bounded saved output. Overflow loses knownness,
/// never counts. Normal complete traffic needs one range.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ranges {
    pub values: Vec<SequenceRange>,
    pub truncated: bool,
}
impl Ranges {
    pub fn push(&mut self, sequence: u64) {
        if let Some(last) = self.values.last_mut()
            && last.end == sequence
        {
            last.end += 1;
            return;
        }
        if self.values.len() < RANGE_LIMIT {
            self.values.push(SequenceRange {
                first: sequence,
                end: sequence + 1,
            });
        } else {
            self.truncated = true;
        }
    }
    fn valid(&self, count: u64, expected: u64) -> bool {
        if self.truncated || self.values.len() > RANGE_LIMIT {
            return false;
        }
        let mut last = 0;
        let mut sum = 0;
        for r in &self.values {
            if r.first < last || r.end <= r.first || r.end > count {
                return false;
            }
            last = r.end;
            sum += r.end - r.first;
        }
        sum == expected
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SequenceCounts {
    pub unique: u64,
    pub duplicates: u64,
    pub reordered: u64,
    pub out_of_range: u64,
    pub missing: u64,
    pub missing_leading: u64,
    pub missing_interior: u64,
    pub missing_trailing: u64,
    pub missing_ranges: Ranges,
}

pub struct Sequences {
    bits: Vec<u64>,
    count: u64,
    highest: Option<u64>,
    counts: SequenceCounts,
}
impl Sequences {
    pub fn new(count: u64) -> Self {
        assert!(count <= 120_000_000, "validated finite sequence range");
        Self {
            bits: vec![0; count.div_ceil(64) as usize],
            count,
            highest: None,
            counts: SequenceCounts::default(),
        }
    }
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub fn insert(&mut self, sequence: u64) -> bool {
        if sequence >= self.count {
            self.counts.out_of_range += 1;
            return false;
        }
        let bit = 1_u64 << (sequence % 64);
        let slot = &mut self.bits[(sequence / 64) as usize];
        if *slot & bit != 0 {
            self.counts.duplicates += 1;
            return false;
        }
        *slot |= bit;
        self.counts.unique += 1;
        if self.highest.is_some_and(|h| sequence < h) {
            self.counts.reordered += 1;
        }
        self.highest = Some(self.highest.map_or(sequence, |h| h.max(sequence)));
        true
    }
    pub fn finish(mut self) -> SequenceCounts {
        let first = (0..self.count).find(|&n| self.has(n));
        let last = self.highest;
        for n in 0..self.count {
            if !self.has(n) {
                self.counts.missing += 1;
                self.counts.missing_ranges.push(n);
                if first.is_none_or(|first| n < first) {
                    self.counts.missing_leading += 1;
                } else if last.is_some_and(|last| n > last) {
                    self.counts.missing_trailing += 1;
                } else {
                    self.counts.missing_interior += 1;
                }
            }
        }
        self.counts
    }
    fn has(&self, n: u64) -> bool {
        self.bits[(n / 64) as usize] & (1 << (n % 64)) != 0
    }
    pub fn unique(&self) -> u64 {
        self.counts.unique
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Distribution {
    pub samples: u64,
    pub minimum_ns: Option<i64>,
    pub maximum_ns: Option<i64>,
    pub p1_ns: Option<i64>,
    pub p50_ns: Option<i64>,
    pub p99_ns: Option<i64>,
    pub p999_ns: Option<i64>,
    pub p99_minus_p1_ns: Option<i64>,
    pub range_ns: Option<i64>,
    pub quantization_ns: u64,
    pub histogram_overflow: bool,
    pub overflow_samples: u64,
}

/// Preallocated one-microsecond bins from -1 to +1 second, exact extrema.
/// Outliers retain sample counts/extrema and make percentiles unavailable.
/// The transit distribution removes its first constant offset before recording.
pub struct Histogram {
    bins: Vec<u64>,
    samples: u64,
    minimum: Option<i64>,
    maximum: Option<i64>,
    overflow: u64,
}
impl Default for Histogram {
    fn default() -> Self {
        Self {
            bins: vec![0; HISTOGRAM_BINS],
            samples: 0,
            minimum: None,
            maximum: None,
            overflow: 0,
        }
    }
}
impl Histogram {
    pub fn record(&mut self, ns: i64) {
        self.samples += 1;
        self.minimum = Some(self.minimum.map_or(ns, |x| x.min(ns)));
        self.maximum = Some(self.maximum.map_or(ns, |x| x.max(ns)));
        let bin = ns.div_euclid(1000);
        if (-HISTOGRAM_HALF..=HISTOGRAM_HALF).contains(&bin) {
            self.bins[(bin + HISTOGRAM_HALF) as usize] += 1;
        } else {
            self.overflow += 1;
        }
    }
    pub fn finish(&self, remove_offset: bool) -> Distribution {
        let quantile = |p: u64| {
            if self.overflow != 0 || self.samples == 0 {
                return None;
            }
            let target = (self.samples * p).div_ceil(1000);
            let mut n = 0;
            self.bins.iter().enumerate().find_map(|(bin, &count)| {
                n += count;
                (n >= target).then_some((bin as i64 - HISTOGRAM_HALF) * 1000)
            })
        };
        let median = quantile(500);
        let offset = if remove_offset {
            median.unwrap_or(0)
        } else {
            0
        };
        let p1 = quantile(10);
        let p99 = quantile(990);
        Distribution {
            samples: self.samples,
            minimum_ns: self.minimum.and_then(|x| x.checked_sub(offset)),
            maximum_ns: self.maximum.and_then(|x| x.checked_sub(offset)),
            p1_ns: p1.and_then(|x| x.checked_sub(offset)),
            p50_ns: median.and_then(|x| x.checked_sub(offset)),
            p99_ns: p99.and_then(|x| x.checked_sub(offset)),
            p999_ns: quantile(999).and_then(|x| x.checked_sub(offset)),
            p99_minus_p1_ns: p99.zip(p1).and_then(|(a, b)| a.checked_sub(b)),
            range_ns: self
                .maximum
                .zip(self.minimum)
                .and_then(|(a, b)| a.checked_sub(b)),
            quantization_ns: 1000,
            histogram_overflow: self.overflow != 0,
            overflow_samples: self.overflow,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendCounts {
    pub planned: u64,
    pub offered: u64,
    pub accepted: u64,
    pub unsent: u64,
    pub partial_calls: u64,
    pub refused_calls: u64,
    pub omitted_slots: u64,
    pub deadline_misses: u64,
    pub accepted_ranges: Ranges,
    pub offered_per_second: Vec<u64>,
    pub accepted_per_second: Vec<u64>,
    pub schedule_lateness: Distribution,
}

impl SendCounts {
    /// One finalized syscall attempt. Accepted message indices are a prefix,
    /// as sendmmsg specifies. The unaccepted suffix is never silently retried.
    pub fn record_attempt(
        &mut self,
        first: u64,
        attempted: u64,
        second: usize,
        accepted: Option<u64>,
    ) {
        self.offered += attempted;
        self.offered_per_second[second] += attempted;
        if let Some(n) = accepted {
            assert!(n <= attempted, "sendmmsg accepted prefix");
            self.accepted += n;
            self.accepted_per_second[second] += n;
            self.partial_calls += u64::from(n < attempted);
            for sequence in first..first + n {
                self.accepted_ranges.push(sequence);
            }
        } else {
            self.refused_calls += 1;
        }
    }
}

/// The fixed M9 burst grid and the smaller functional-workload grid.
pub fn scheduled_slot(count: u64, pps: u64, slot: u64) -> Option<(u64, u64, u64)> {
    let burst = if pps == 1_000_000 { 100 } else { 1 };
    let first = slot.checked_mul(burst)?;
    if first >= count || pps == 0 {
        return None;
    }
    let offset = first.checked_mul(1_000_000_000)?.checked_div(pps)?;
    Some((first, burst.min(count - first), offset))
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiveCounts {
    pub messages: u64,
    pub datagrams: u64,
    pub genuine_gro_messages: u64,
    pub aggregate_sizes: BTreeMap<usize, u64>,
    pub segment_sizes: BTreeMap<usize, u64>,
    pub group_sizes: BTreeMap<usize, u64>,
    pub malformed: u64,
    pub wrong_peer: u64,
    pub wrong_size: u64,
    pub data_truncated: u64,
    pub control_truncated: u64,
    pub invalid_metadata: u64,
    pub sequence: SequenceCounts,
    pub unique_per_second: Vec<u64>,
    /// Kernel sk_drops units, not necessarily original datagrams with GRO.
    pub socket_drops: Option<u64>,
    pub socket_drop_source: String,
    pub attached_drop_lower_bound: Option<u32>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimestampEvidence {
    pub source: String,
    pub sender_granularity: String,
    pub receiver_granularity: String,
    pub missing: u64,
    pub clock_discontinuities: u64,
    pub clock_pairs: u64,
    pub first_raw_ns: Option<u64>,
    pub first_wall_ns: Option<i64>,
    pub last_raw_ns: Option<u64>,
    pub last_wall_ns: Option<i64>,
    pub maximum_pair_span_ns: u64,
    pub maximum_offset_change_ns: u64,
    pub wide_clock_pairs: u64,
    pub maximum_mapping_uncertainty_ns: u64,
    pub transit_anchor_ns: Option<i64>,
    pub read_minus_kernel: Distribution,
    pub offset_removed_receive_minus_sender: Distribution,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CpuIdentity {
    pub cpu: usize,
    pub package: i32,
    pub core: i32,
    pub siblings: Vec<usize>,
    pub online: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Placement {
    pub requested_cpu: usize,
    pub allowed_before: Vec<usize>,
    pub affinity_before: Option<Vec<usize>>,
    pub affinity_after: Option<Vec<usize>>,
    pub topology: Vec<CpuIdentity>,
    pub application_threads: usize,
    pub helper_cpus_before: Option<Vec<usize>>,
    pub helper_cpus_after: Option<Vec<usize>>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SocketFacts {
    pub local: String,
    pub peer: String,
    pub family: String,
    pub payload_bytes: usize,
    pub ipv4_bytes: usize,
    pub ip_options: String,
    pub df: Option<bool>,
    pub path_mtu: Option<u32>,
    pub send_buffer: Option<usize>,
    pub receive_buffer: Option<usize>,
    pub udp_gso: Option<i32>,
    pub udp_gro: Option<bool>,
    pub timestampns: Option<bool>,
    pub gro_error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Readiness {
    pub version: u32,
    pub pid: u32,
    pub result_id: String,
    pub socket: SocketFacts,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeResult {
    pub version: u32,
    pub role: String,
    pub result_id: String,
    pub workload: Workload,
    pub mode: Option<ReceiveMode>,
    pub binary_sha256: String,
    pub hostname: String,
    pub kernel: String,
    pub profiling_enabled: bool,
    pub socket: SocketFacts,
    pub placement: Placement,
    pub start_raw_ns: u64,
    pub end_raw_ns: u64,
    pub elapsed_ns: u64,
    pub offered_pps: f64,
    pub accepted_pps: f64,
    pub unique_pps: f64,
    pub batch_datagrams: u64,
    pub batch_period_ns: u64,
    pub complete: bool,
    pub errors: Vec<String>,
    pub sender: Option<SendCounts>,
    pub receiver: Option<ReceiveCounts>,
    pub timestamps: TimestampEvidence,
    pub counters_before: Option<PathBuf>,
    pub counters_after: Option<PathBuf>,
}

/// The physical plan supplies these facts and the raw receipts they rest on.
/// CPU sets describe observed workload processing, not global NET_RX activity.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostEvidence {
    pub version: u32,
    pub run_id: u64,
    pub receiver_hostname: String,
    pub receiver_binary_sha256: String,
    pub topology: Vec<CpuIdentity>,
    pub receive_processing_cpus: Vec<usize>,
    pub sampler_cpus: Vec<usize>,
    pub sender_on_receiver_host: bool,
    pub sender_cpus: Vec<usize>,
    pub queue_irq_rps_rfs_observed: bool,
    /// Named raw before/after receipts, with paths relative to this JSON.
    pub receipts: BTreeMap<String, Receipt>,
    pub counters: Vec<CounterFact>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub path: PathBuf,
    pub sha256: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CounterFact {
    pub name: String,
    pub unit: String,
    pub before: Option<u64>,
    pub after: Option<u64>,
}
impl HostEvidence {
    pub fn verify_files(&self, base: &Path) -> Result<(), String> {
        if self.receipts.len() > 32 || self.counters.len() > 4096 {
            return Err("too many host evidence records".into());
        }
        for r in self.receipts.values() {
            if r.path.is_absolute()
                || r.path
                    .components()
                    .any(|c| !matches!(c, Component::Normal(_)))
            {
                return Err("host receipt paths must be relative normal components".into());
            }
            let path = base.join(&r.path);
            if std::fs::metadata(&path).map_err(|e| e.to_string())?.len() > 64 * 1024 * 1024 {
                return Err("host receipt exceeds 64 MiB".into());
            }
            if super::hash_file(&path)? != r.sha256 {
                return Err(format!("host receipt hash mismatch: {}", r.path.display()));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Evaluation {
    pub version: u32,
    pub run_id: u64,
    pub diagnostic_complete: bool,
    pub zero_missing_workload_datagrams: bool,
    pub sender_unsent: Option<u64>,
    pub missing_after_kernel_acceptance: Option<u64>,
    pub timestamp_results_available: bool,
    /// Supplied facts are internally consistent. Raw receipt contents still
    /// require judgment against the frozen physical plan.
    pub declared_placement_consistent: bool,
    pub eligible_for_physical_review: bool,
    /// A physical plan and independent evidence review make the acceptance decision.
    pub physical_acceptance: bool,
    pub reasons: Vec<String>,
    pub counter_limits: Vec<String>,
    pub counter_observations: Vec<String>,
}

/// Compare finalized, independently supplied observations. No I/O or traffic.
pub fn evaluate(s: &ProbeResult, r: &ProbeResult, host: Option<&HostEvidence>) -> Evaluation {
    let mut reasons = Vec::new();
    let mut counter_limits = Vec::new();
    let mut counter_observations = Vec::new();
    let compatible = s.version == VERSION
        && r.version == VERSION
        && s.role == "sender"
        && r.role == "receiver"
        && s.mode.is_none()
        && r.mode.is_some()
        && s.result_id != r.result_id
        && s.workload.validate().is_ok()
        && r.workload.validate().is_ok()
        && s.workload.run_id == r.workload.run_id
        && s.workload.count == r.workload.count
        && s.workload.pps == r.workload.pps
        && s.workload.source == r.workload.source
        && s.workload.bind == r.workload.peer
        && s.workload.peer == r.workload.bind
        && s.binary_sha256 == r.binary_sha256
        && [s, r].iter().all(|p| {
            p.binary_sha256.len() == 64
                && p.binary_sha256.bytes().all(|c| c.is_ascii_hexdigit())
                && p.socket.local == p.workload.bind.to_string()
                && p.socket.peer == p.workload.peer.to_string()
                && p.socket.payload_bytes == PAYLOAD
                && p.socket.ipv4_bytes == IP_BYTES
                && p.socket.family == "IPv4"
                && p.end_raw_ns >= p.start_raw_ns
                && p.elapsed_ns == p.end_raw_ns - p.start_raw_ns
        });
    let mut diagnostic_complete =
        compatible && s.complete && r.complete && s.errors.is_empty() && r.errors.is_empty();
    let mut check = |ok: bool, why: &str| {
        if !ok {
            reasons.push(why.to_owned());
        }
    };
    check(
        s.version == VERSION && r.version == VERSION,
        "unsupported result version",
    );
    check(
        s.role == "sender" && r.role == "receiver" && s.result_id != r.result_id,
        "missing or duplicate endpoint identities",
    );
    check(
        s.workload.validate().is_ok() && r.workload.validate().is_ok(),
        "invalid workload inputs",
    );
    check(
        s.workload.run_id == r.workload.run_id
            && s.workload.count == r.workload.count
            && s.workload.pps == r.workload.pps
            && s.workload.source == r.workload.source
            && s.workload.bind == r.workload.peer
            && s.workload.peer == r.workload.bind
            && s.binary_sha256 == r.binary_sha256,
        "endpoint run, source, shape or peer mismatch",
    );
    check(
        s.complete && r.complete && s.errors.is_empty() && r.errors.is_empty(),
        "incomplete endpoint observation",
    );
    check(
        !s.profiling_enabled && !r.profiling_enabled,
        "profiling was enabled",
    );
    for p in [s, r] {
        check(
            p.binary_sha256.len() == 64 && p.binary_sha256.bytes().all(|c| c.is_ascii_hexdigit()),
            "invalid binary identity",
        );
        check(
            p.socket.local == p.workload.bind.to_string()
                && p.socket.peer == p.workload.peer.to_string(),
            "socket address readbacks mismatch",
        );
        check(
            p.socket.payload_bytes == PAYLOAD
                && p.socket.ipv4_bytes == IP_BYTES
                && p.socket.family == "IPv4"
                && p.socket.df == Some(true)
                && p.socket.udp_gso == Some(0)
                && p.socket.ip_options == "fresh socket; no IP options installed",
            "socket shape, DF or GSO evidence is unavailable",
        );
        check(
            p.end_raw_ns >= p.start_raw_ns && p.elapsed_ns == p.end_raw_ns - p.start_raw_ns,
            "malformed elapsed time",
        );
    }
    let send = s.sender.as_ref();
    let recv = r.receiver.as_ref();
    let count_roles =
        send.is_some() && recv.is_some() && s.receiver.is_none() && r.sender.is_none();
    diagnostic_complete &= count_roles;
    check(count_roles, "endpoint counts missing or wrong role");
    let count = s.workload.count;
    let sum = |values: &[u64]| values.iter().try_fold(0_u64, |sum, n| sum.checked_add(*n));
    let histogram_count = |values: &BTreeMap<usize, u64>| {
        values
            .values()
            .try_fold(0_u64, |sum, n| sum.checked_add(*n))
    };
    let histogram_weight = |values: &BTreeMap<usize, u64>| {
        values.iter().try_fold(0_u64, |sum, (size, n)| {
            (*size as u64)
                .checked_mul(*n)
                .and_then(|n| sum.checked_add(n))
        })
    };
    let mut zero_missing = false;
    let mut missing_after = None;
    if let (Some(a), Some(b)) = (send, recv) {
        let q = &b.sequence;
        let send_counts = a.planned == count
            && a.offered <= count
            && a.accepted <= a.offered
            && a.unsent == count - a.accepted
            && a.offered_per_second.len() <= 181
            && a.accepted_per_second.len() <= 181
            && a.accepted_per_second
                .iter()
                .enumerate()
                .all(|(i, n)| *n <= a.offered_per_second.get(i).copied().unwrap_or(0))
            && sum(&a.offered_per_second) == Some(a.offered)
            && sum(&a.accepted_per_second) == Some(a.accepted);
        let recv_counts = q.unique <= count
            && q.missing == count - q.unique
            && q.missing_leading
                .checked_add(q.missing_interior)
                .and_then(|n| n.checked_add(q.missing_trailing))
                == Some(q.missing)
            && q.unique
                .checked_add(q.duplicates)
                .and_then(|n| n.checked_add(q.out_of_range))
                .is_some_and(|n| n <= b.datagrams)
            && b.unique_per_second.len() <= 181
            && sum(&b.unique_per_second) == Some(q.unique)
            && q.reordered <= q.unique
            && b.genuine_gro_messages <= b.messages
            && histogram_count(&b.aggregate_sizes) == Some(b.messages)
            && histogram_count(&b.segment_sizes) == Some(b.datagrams)
            && histogram_count(&b.group_sizes) == Some(b.messages)
            && histogram_weight(&b.group_sizes) == Some(b.datagrams);
        check(send_counts && recv_counts, "malformed endpoint counts");
        let send_ranges = a.accepted_ranges.valid(count, a.accepted);
        let missing_ranges = q.missing_ranges.valid(count, q.missing);
        let counts_consistent = send_counts && recv_counts && send_ranges && missing_ranges;
        diagnostic_complete &= counts_consistent;
        if compatible && count_roles && counts_consistent {
            let mut n = 0;
            for a in &a.accepted_ranges.values {
                for b in &q.missing_ranges.values {
                    n += a.end.min(b.end).saturating_sub(a.first.max(b.first));
                }
            }
            missing_after = Some(n);
        }
        check(
            send_ranges && missing_ranges,
            "sequence-range evidence incomplete or malformed",
        );
        let clean = b.malformed == 0
            && b.wrong_peer == 0
            && b.wrong_size == 0
            && b.data_truncated == 0
            && b.invalid_metadata == 0
            && q.out_of_range == 0;
        check(b.control_truncated == 0, "truncated control evidence");
        check(clean, "invalid or truncated datagram evidence");
        zero_missing = compatible
            && count_roles
            && send_counts
            && recv_counts
            && clean
            && send_ranges
            && missing_ranges
            && a.accepted == count
            && q.unique == count
            && q.missing == 0;
        check(
            zero_missing,
            "declared workload range was not entirely accepted and uniquely received",
        );
        check(
            a.offered == count && a.unsent == 0 && a.omitted_slots == 0,
            "generator omitted or refused workload datagrams",
        );
        let duration_ns = count
            .checked_mul(1_000_000_000)
            .and_then(|n| n.checked_div(s.workload.pps))
            .unwrap_or(u64::MAX);
        let offered_ok = s.elapsed_ns > 0
            && a.offered as u128 * 1_000_000_000 >= s.workload.pps as u128 * s.elapsed_ns as u128
            && s.elapsed_ns <= duration_ns;
        check(
            offered_ok,
            "generator elapsed rate is below the requested rate",
        );
        // Full one-second bins must each reach the requested rate. A shortened
        // final bin cannot hide a deficient complete second.
        let full_seconds = (duration_ns / 1_000_000_000).min(181);
        check(
            (0..full_seconds as usize)
                .all(|i| a.offered_per_second.get(i).copied().unwrap_or(0) >= s.workload.pps),
            "an offered-rate second is below the requested rate",
        );
        if r.mode == Some(ReceiveMode::Gro) {
            check(
                r.socket.udp_gro == Some(true) && b.genuine_gro_messages > 0,
                "no genuine multi-segment GRO observation",
            );
        } else {
            check(false, "header-only mode is a baseline, not GRO feasibility");
        }
        if b.socket_drops.is_none() {
            counter_limits.push(
                "socket sk_drops unavailable; attached values are tail-incomplete lower bounds"
                    .into(),
            );
        }
        if b.socket_drops.is_some_and(|n| n > 0) {
            reasons.push(
                "socket discard counter rose; kernel message units are not added to sequence loss"
                    .into(),
            );
        }
    }
    let ts = &r.timestamps;
    let timestamp_ok = compatible && timestamp_available(ts, recv.map_or(0, |x| x.sequence.unique));
    if !timestamp_ok {
        reasons.push("software timestamp diagnostics unavailable or incomplete".into());
    }
    let physical = host.is_some_and(|h| declared_placement(h, r));
    if !physical {
        reasons.push(
            "one-physical-core placement declarations are unavailable or inconsistent".into(),
        );
    }
    if s.workload.pps != 1_000_000 || count < 1_000_000 {
        reasons.push("not the M9 rate or at least one complete offered second".into());
    }
    if let Some(h) = host {
        for c in &h.counters {
            match c.before.zip(c.after) {
                Some((a, b)) if b >= a => counter_observations.push(format!(
                    "{} rose by {} {}; retained for physical-plan review",
                    c.name,
                    b - a,
                    c.unit
                )),
                _ => counter_limits.push(format!(
                    "{} unavailable or decreased; unit {}",
                    c.name, c.unit
                )),
            }
        }
    } else {
        counter_limits.push("actual-peer host counters were not supplied".into());
    }
    Evaluation {
        version: VERSION,
        run_id: s.workload.run_id,
        diagnostic_complete: diagnostic_complete && send.is_some() && recv.is_some(),
        zero_missing_workload_datagrams: zero_missing,
        sender_unsent: send.map(|x| x.unsent),
        missing_after_kernel_acceptance: missing_after,
        timestamp_results_available: timestamp_ok,
        declared_placement_consistent: physical,
        eligible_for_physical_review: reasons.is_empty(),
        physical_acceptance: false,
        reasons,
        counter_limits,
        counter_observations,
    }
}

pub fn timestamp_available(ts: &TimestampEvidence, unique: u64) -> bool {
    let distribution = |d: &Distribution| {
        d.quantization_ns == 1000
            && !d.histogram_overflow
            && d.overflow_samples == 0
            && d.minimum_ns
                .zip(d.maximum_ns)
                .is_some_and(|(a, b)| a <= b && b.checked_sub(a) == d.range_ns)
            && d.p1_ns.zip(d.p50_ns).is_some_and(|(a, b)| a <= b)
            && d.p50_ns.zip(d.p99_ns).is_some_and(|(a, b)| a <= b)
            && d.p99_ns.zip(d.p999_ns).is_some_and(|(a, b)| a <= b)
            && d.p99_ns
                .zip(d.p1_ns)
                .is_some_and(|(a, b)| a.checked_sub(b) == d.p99_minus_p1_ns)
    };
    !ts.source.is_empty()
        && !ts.sender_granularity.is_empty()
        && !ts.receiver_granularity.is_empty()
        && ts.missing == 0
        && ts.clock_discontinuities == 0
        && ts.clock_pairs >= 2
        && ts.first_raw_ns.is_some()
        && ts.first_wall_ns.is_some()
        && ts.last_raw_ns.is_some()
        && ts.last_wall_ns.is_some()
        && ts.read_minus_kernel.samples == unique
        && ts.offset_removed_receive_minus_sender.samples == unique
        && !ts.read_minus_kernel.histogram_overflow
        && !ts.offset_removed_receive_minus_sender.histogram_overflow
        && distribution(&ts.read_minus_kernel)
        && distribution(&ts.offset_removed_receive_minus_sender)
        && ts.read_minus_kernel.samples > 0
}

fn declared_placement(h: &HostEvidence, r: &ProbeResult) -> bool {
    if h.version != VERSION
        || h.run_id != r.workload.run_id
        || h.receiver_hostname != r.hostname
        || h.receiver_binary_sha256 != r.binary_sha256
        || !h.queue_irq_rps_rfs_observed
        || h.receive_processing_cpus.is_empty()
        || h.sampler_cpus.is_empty()
        || h.topology != r.placement.topology
        || r.placement.affinity_before.as_deref() != Some(&[r.workload.cpu])
        || r.placement.affinity_after.as_deref() != Some(&[r.workload.cpu])
        || r.placement.helper_cpus_before.is_none()
        || r.placement.helper_cpus_after.is_none()
        || r.placement.application_threads != 1
    {
        return false;
    }
    let required = [
        "topology",
        "queue-before",
        "queue-after",
        "irq-before",
        "irq-after",
        "rps-rfs-before",
        "rps-rfs-after",
        "counters-before",
        "counters-after",
    ];
    if required.iter().any(|key| !h.receipts.contains_key(*key)) {
        return false;
    }
    let Some(app) = h
        .topology
        .iter()
        .find(|c| c.cpu == r.workload.cpu && c.online)
    else {
        return false;
    };
    let same_core = |cpu: &usize| {
        h.topology.iter().any(|c| {
            c.cpu == *cpu
                && c.online
                && c.package == app.package
                && c.core == app.core
                && app.siblings.contains(cpu)
        })
    };
    let outside_core = |cpu: &usize| {
        h.topology.iter().any(|c| {
            c.cpu == *cpu
                && c.online
                && c.package >= 0
                && c.core >= 0
                && (c.package, c.core) != (app.package, app.core)
        })
    };
    let unique: BTreeSet<_> = h.topology.iter().map(|c| c.cpu).collect();
    unique.len() == h.topology.len()
        && app.package >= 0
        && app.core >= 0
        && app.siblings.contains(&app.cpu)
        && app.siblings.iter().collect::<BTreeSet<_>>().len() == app.siblings.len()
        && app.siblings.iter().all(|id| {
            h.topology.iter().any(|c| {
                c.cpu == *id
                    && (c.package, c.core) == (app.package, app.core)
                    && c.siblings.iter().collect::<BTreeSet<_>>()
                        == app.siblings.iter().collect::<BTreeSet<_>>()
            })
        })
        && h.receive_processing_cpus.iter().all(same_core)
        && h.sampler_cpus.iter().all(outside_core)
        && r.placement
            .helper_cpus_before
            .as_ref()
            .is_some_and(|cpus| cpus.iter().all(outside_core))
        && r.placement
            .helper_cpus_after
            .as_ref()
            .is_some_and(|cpus| cpus.iter().all(outside_core))
        && (!h.sender_on_receiver_host
            || (!h.sender_cpus.is_empty() && h.sender_cpus.iter().all(outside_core)))
}

/// A wall-clock read bracketed by RAW reads. Its span is uncertainty, not a step.
#[derive(Clone, Copy, Debug)]
pub struct ClockPair {
    pub raw_before_ns: u64,
    pub wall_ns: i64,
    pub raw_after_ns: u64,
}
impl ClockPair {
    fn middle(self) -> Option<u64> {
        self.raw_after_ns
            .checked_sub(self.raw_before_ns)
            .map(|span| self.raw_before_ns + span / 2)
    }
    fn span(self) -> u64 {
        self.raw_after_ns.saturating_sub(self.raw_before_ns)
    }
}

#[derive(Default)]
pub struct ClockTracker {
    previous: Option<ClockPair>,
    pub evidence: TimestampEvidence,
}
impl ClockTracker {
    pub fn observe(&mut self, pair: ClockPair) {
        let ts = &mut self.evidence;
        ts.clock_pairs += 1;
        let Some(middle) = pair.middle() else {
            ts.clock_discontinuities += 1;
            return;
        };
        let span = pair.span();
        ts.maximum_pair_span_ns = ts.maximum_pair_span_ns.max(span);
        ts.wide_clock_pairs += u64::from(span > 1_000_000);
        let offset = pair.wall_ns as i128 - middle as i128;
        if let Some(old) = self.previous {
            let old_middle = old.middle().expect("only coherent pairs are retained");
            let change = (offset - (old.wall_ns as i128 - old_middle as i128))
                .unsigned_abs()
                .min(u64::MAX as u128) as u64;
            ts.maximum_offset_change_ns = ts.maximum_offset_change_ns.max(change);
            let uncertainty = span / 2 + old.span() / 2;
            let elapsed = middle.saturating_sub(old_middle);
            // A suspected step exceeds bracketing uncertainty plus a conservative
            // 1 ms + 1000 ppm continuity allowance. This is not a spread gate.
            if middle < old_middle
                || change.saturating_sub(uncertainty) > 1_000_000 + elapsed / 1000
            {
                ts.clock_discontinuities += 1;
            }
        }
        if ts.first_raw_ns.is_none() {
            ts.first_raw_ns = Some(middle);
            ts.first_wall_ns = Some(pair.wall_ns);
        }
        ts.last_raw_ns = Some(middle);
        ts.last_wall_ns = Some(pair.wall_ns);
        self.previous = Some(pair);
    }
    fn mapped(&mut self, before: ClockPair, after: ClockPair, wall: i64) -> Option<u64> {
        let a = before.middle()?;
        let b = after.middle()?;
        if b < a {
            return None;
        }
        let raw = a as i128 + (b - a) as i128 / 2;
        let realtime =
            before.wall_ns as i128 + (after.wall_ns as i128 - before.wall_ns as i128) / 2;
        // Mean of the two pair offsets. Preserve its bracketing uncertainty
        // even for a wide pair. Later narrow reads remain usable.
        let uncertainty = (before.span() as u128 + after.span() as u128).div_ceil(4) + 1;
        self.evidence.maximum_mapping_uncertainty_ns = self
            .evidence
            .maximum_mapping_uncertainty_ns
            .max(uncertainty.min(u64::MAX as u128) as u64);
        u64::try_from(raw + wall as i128 - realtime).ok()
    }
}

/// Full kernel metadata and the copied bytes are kept separate for header-only
/// MSG_TRUNC. A truncated control set never supplies a usable timestamp.
#[derive(Clone, Copy, Debug, Default)]
pub struct ReceiveMessage {
    pub length: usize,
    pub peer: Option<std::net::SocketAddrV4>,
    pub wall_ns: Option<i64>,
    pub gro: Option<i32>,
    pub data_truncated: bool,
    pub control_truncated: bool,
}

/// Actual receive accounting shared by Linux I/O and hand-derived fixtures.
/// Fixed arrays and bitsets are allocated once before readiness.
pub struct MessageIngest {
    run_id: u64,
    peer: std::net::SocketAddrV4,
    mode: ReceiveMode,
    sequences: Sequences,
    counts: ReceiveCounts,
    lengths: Vec<u64>,
    segment_lengths: Vec<u64>,
    group_sizes: Vec<u64>,
    read_delay: Histogram,
    transit: Histogram,
    anchor: Option<i64>,
    pub clocks: ClockTracker,
}
impl MessageIngest {
    pub fn new(count: u64, run_id: u64, peer: std::net::SocketAddrV4, mode: ReceiveMode) -> Self {
        Self { run_id,peer,mode,sequences:Sequences::new(count),
            counts:ReceiveCounts {unique_per_second:vec![0;181],..ReceiveCounts::default()},
            lengths:vec![0;MAX_AGGREGATE+1],segment_lengths:vec![0;MAX_AGGREGATE+1],
            group_sizes:vec![0;MAX_AGGREGATE.div_ceil(HEADER)+1],
            read_delay:Histogram::default(),transit:Histogram::default(),anchor:None,
            clocks:ClockTracker { previous:None,evidence:TimestampEvidence {
                source:"SO_TIMESTAMPNS CLOCK_REALTIME, mapped by bracketing CLOCK_MONOTONIC_RAW pairs".into(),
                sender_granularity:"one pre-syscall software stamp shared by each sendmmsg call (at most 64 datagrams)".into(),
                receiver_granularity:"one kernel stamp per receive message; GRO segments share it".into(),
                ..TimestampEvidence::default()
            }} }
    }
    pub fn unique(&self) -> u64 {
        self.sequences.unique()
    }
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub fn ingest(
        &mut self,
        message: ReceiveMessage,
        bytes: &[u8],
        before: ClockPair,
        after: ClockPair,
        read_raw_ns: u64,
        second: usize,
    ) {
        let counts = &mut self.counts;
        counts.messages += 1;
        if message.length <= MAX_AGGREGATE {
            self.lengths[message.length] += 1;
        }
        if message.control_truncated {
            counts.control_truncated += 1;
        }
        if message.peer != Some(self.peer) {
            counts.wrong_peer += 1;
            return;
        }
        if self.mode == ReceiveMode::Gro && (message.data_truncated || bytes.len() < message.length)
        {
            counts.data_truncated += 1;
            return;
        }
        if self.mode == ReceiveMode::HeaderOnly && message.gro.is_some() {
            counts.invalid_metadata += 1;
            return;
        }
        let splits = match segments(message.length, message.gro) {
            Ok(ranges) => ranges,
            Err(_) => {
                counts.invalid_metadata += 1;
                return;
            }
        };
        let size = message.gro.map_or(message.length, |n| n as usize);
        self.group_sizes[message.length.div_ceil(size)] += 1;
        if message.gro.is_some_and(|n| message.length > n as usize) {
            counts.genuine_gro_messages += 1;
        }
        let kernel = (!message.control_truncated)
            .then_some(message.wall_ns)
            .flatten()
            .and_then(|wall| self.clocks.mapped(before, after, wall));
        for range in splits {
            counts.datagrams += 1;
            let len = range.end - range.start;
            self.segment_lengths[len] += 1;
            if len != PAYLOAD {
                counts.wrong_size += 1;
                continue;
            }
            let copied = if self.mode == ReceiveMode::HeaderOnly {
                bytes
            } else {
                &bytes[range]
            };
            let (sequence, stamp) = match decode_header(copied, self.run_id) {
                Ok(header) => header,
                Err(_) => {
                    counts.malformed += 1;
                    continue;
                }
            };
            if !self.sequences.insert(sequence) {
                continue;
            }
            counts.unique_per_second[second.min(180)] += 1;
            let sample = kernel.and_then(|kernel| {
                let delay = i64::try_from(read_raw_ns as i128 - kernel as i128).ok()?;
                let difference = i64::try_from(kernel as i128 - stamp as i128).ok()?;
                let first = *self.anchor.get_or_insert(difference);
                Some((delay, difference.checked_sub(first)?))
            });
            if let Some((delay, centered)) = sample {
                self.read_delay.record(delay);
                self.transit.record(centered);
            } else {
                self.clocks.evidence.missing += 1;
            }
        }
    }
    pub fn finish(mut self) -> (ReceiveCounts, TimestampEvidence) {
        self.counts.sequence = self.sequences.finish();
        while self.counts.unique_per_second.len() > 1
            && self.counts.unique_per_second.last() == Some(&0)
        {
            self.counts.unique_per_second.pop();
        }
        self.counts.aggregate_sizes = self
            .lengths
            .into_iter()
            .enumerate()
            .filter(|(_, n)| *n > 0)
            .collect();
        self.counts.segment_sizes = self
            .segment_lengths
            .into_iter()
            .enumerate()
            .filter(|(_, n)| *n > 0)
            .collect();
        self.counts.group_sizes = self
            .group_sizes
            .into_iter()
            .enumerate()
            .filter(|(_, n)| *n > 0)
            .collect();
        self.clocks.evidence.transit_anchor_ns = self.anchor;
        self.clocks.evidence.read_minus_kernel = self.read_delay.finish(false);
        self.clocks.evidence.offset_removed_receive_minus_sender = self.transit.finish(true);
        (self.counts, self.clocks.evidence)
    }
}
