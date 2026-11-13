//! Per-Sub-Interval results (RFC 9097 §5.3, §5.4; RFC 9946 §8.2; docs/spec.md
//! "Results and output"): what the Load Receiver measured in each
//! Sub-Interval, from its own statistics, or on the Load Sender's side from
//! the Sub-Interval statistics (sisSav) its Status PDUs carry. Only
//! Sub-Intervals that closed while the Test ran, up to the number the Test
//! Interval holds, are reported.
//! (conformance: sub-interval-results-from-the-test-only)
//!
//! [`MeasurementResults`] turns a Test Connection's Data Phase events, or
//! those of a Multi-Connection Test's Test Connections, into the Test's
//! measurement results: the Sub-Intervals, the IP-Layer Sender Bit
//! Rate over each, and each Capacity Mode's Maximum and Highest IP-Layer
//! Capacity, as the Client user's [`Evaluation`] asks.

use std::ops::RangeInclusive;
use std::time::Duration;

use crate::wire::status::{STATUS_NODEL, SubIntervalStats};

use crate::protocol::{DataEvent, Direction, IpVersion, MonoTime};

/// How a Test's measurement results are evaluated: over the Sub-Intervals
/// its Test Interval holds, counting the headers of its IP version, and
/// judged by the Client user's Performance Criterion and Capacity Modes
/// (RFC 9097 §5.3, §6.3, §6.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Evaluation {
    /// testIntTime and subIntPeriod, as the Activation Response gave them.
    pub test_interval: Duration,
    pub sub_interval: Duration,
    /// The Test's IP version: every rate counts its IP and UDP headers.
    pub ip_version: IpVersion,
    /// What a Sub-Interval must meet to count towards the Maximum IP-Layer
    /// Capacity.
    pub criterion: PerformanceCriterion,
    /// The first this many Sub-Intervals in a Capacity Mode of their own,
    /// when that leaves two (TR-471's NumberFirstModeTestSubIntervals).
    pub first_mode: Option<u32>,
}

/// The measurement results of one Test Connection, or of a
/// Multi-Connection Test's Test Connections together (RFC 9097 §6, §7,
/// §9): what the Client reports of a Test once it has run.
///
/// They are built from each Test Connection's Data Phase events as a
/// [`DataPhase`](crate::protocol::DataPhase) reports them, at either end: each
/// Sub-Interval at most once, numbered from 1 to the number the Test
/// Interval holds. The results rely on it, and debug builds check it: a
/// second copy of a Sub-Interval, or one beyond the Test Interval, would
/// count for nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeasurementResults {
    /// Each Sub-Interval of the Test Interval, by subIntSeqNo from 1; one
    /// never measured, such as a final one whose Status PDU was lost, is
    /// `None`, never a zero (docs/spec.md). A later copy of a Sub-Interval
    /// replaces none: the first one counts.
    pub sub_intervals: Vec<Option<SubIntervalMeasurement>>,
    /// The IP-Layer Sender Bit Rate over each Sub-Interval, where every st
    /// in it was reported.
    pub sender_bit_rates: Vec<Option<SentInterval>>,
    /// Whether those are the Load Receiver's estimate of what the Load
    /// Sender sent, rather than the Load Sender's own count.
    pub sender_estimated: bool,
    /// Each Capacity Mode's results: one, or two when the Client user split
    /// the Sub-Intervals.
    pub capacity_modes: Vec<CapacityModeResult>,
    /// In an Upstream Multi-Connection Test each Test Connection's Load
    /// Receiver keeps its own Sub-Intervals, which are summed by number:
    /// they lie up to this far apart.
    /// (conformance: approximate-upstream-aggregation)
    pub approximate: Option<Duration>,
}

impl MeasurementResults {
    /// One Test Connection's results, from its Data Phase's events: its
    /// Sub-Intervals, and the st its Load Sender measured, or failing
    /// those, the ones its Load Receiver estimated.
    pub fn of_test_connection(data_events: &[DataEvent], evaluation: &Evaluation) -> Self {
        debug_assert!(
            reported_as_a_data_phase_does(data_events, evaluation),
            "each Sub-Interval once, within the Test Interval"
        );
        let (sent, sender_estimated) = sent_intervals(data_events);
        Self::of(
            &measured_sub_intervals(data_events),
            &sent,
            sender_estimated,
            None,
            evaluation,
        )
    }

    /// A Multi-Connection Test's results, from each of its Test
    /// Connections' Data Phase events: their Sub-Intervals of each number
    /// summed (docs/spec.md "Multi-Connection aggregation"), exactly in a
    /// Downstream Test, whose Load Receivers keep to one grid, and
    /// approximately in an Upstream one, whose Load Receivers are the
    /// Servers'; their st likewise. clockDeltaMin is kept only when
    /// every Test Connection goes to `one_server`.
    pub fn of_test(
        connections: &[&[DataEvent]],
        direction: Direction,
        one_server: bool,
        evaluation: &Evaluation,
    ) -> Self {
        debug_assert!(
            connections
                .iter()
                .all(|events| reported_as_a_data_phase_does(events, evaluation)),
            "each Test Connection's Sub-Intervals once, within the Test Interval"
        );
        let per_connection: Vec<Vec<SubIntervalMeasurement>> = connections
            .iter()
            .map(|events| measured_sub_intervals(events))
            .collect();
        // Measured by the Load Senders, or estimated by the Load Receivers
        //
        let sent_of = |estimated: bool| {
            let each: Vec<Vec<SentInterval>> = connections
                .iter()
                .map(|events| {
                    events
                        .iter()
                        .filter_map(|e| match (e, estimated) {
                            (DataEvent::Sent(s), false) | (DataEvent::SentEstimated(s), true) => {
                                Some(*s)
                            }
                            _ => None,
                        })
                        .collect()
                })
                .collect();
            aggregate_sent(&each)
        };
        let measured = sent_of(false);
        let (sent, sender_estimated) = if measured.is_empty() {
            let estimated = sent_of(true);
            let any = !estimated.is_empty();
            (estimated, any)
        } else {
            (measured, false)
        };
        let approximate =
            (direction == Direction::Upstream).then(|| largest_offset(&per_connection));
        Self::of(
            &aggregate(&per_connection, one_server),
            &sent,
            sender_estimated,
            approximate,
            evaluation,
        )
    }

    fn of(
        measured: &[SubIntervalMeasurement],
        sent: &[SentInterval],
        sender_estimated: bool,
        approximate: Option<Duration>,
        evaluation: &Evaluation,
    ) -> Self {
        let expected = expected_sub_intervals(evaluation.test_interval, evaluation.sub_interval);
        Self {
            sub_intervals: by_sub_interval(expected, measured),
            sender_bit_rates: sent_by_sub_interval(
                expected,
                evaluation.sub_interval,
                sent,
                evaluation.ip_version == IpVersion::V4,
            ),
            sender_estimated,
            capacity_modes: capacity_modes(
                expected,
                measured,
                evaluation.criterion,
                evaluation.first_mode,
            ),
            approximate,
        }
    }

    /// Whether a Capacity Mode has no Maximum IP-Layer Capacity: no
    /// Sub-Interval of it met the Performance Criterion (RFC 9097 §6.3).
    pub fn lacks_a_maximum(&self) -> bool {
        self.capacity_modes
            .iter()
            .any(|mode| mode.maximum.is_none())
    }

    /// Each Capacity Mode in which no Sub-Interval after the Maximum
    /// IP-Layer Capacity's met the `criterion`, and a Sub-Interval that
    /// missed it, before the Maximum's or after it, ran more than
    /// `faster_margin_percent` faster than the Maximum, or the last one
    /// measured, after it, ran under [`SLOW_LAST_SUB_INTERVAL_PERCENT`] of
    /// it. A later Sub-Interval that met the criterion, at any rate,
    /// means the search found its rate again, and nothing is found.
    ///
    /// A faster one after it is what a Search Test through a drop-tail
    /// queue gives: after its first overshoot the search cycles a row or
    /// two around the bottleneck, and where every Sub-Interval there loses
    /// more than the criterion allows, the Maximum falls to one of the
    /// climb, well below the path. A faster one before it is a search that
    /// climbed back slowly after its overshoot, as CAKE gives with two
    /// Test Connections each under the 1 Gbps row, or a path that carried
    /// more earlier: one that slowed, or a policer or shaper that let a
    /// burst through at first. A slow last one is loss that doesn't follow
    /// the rate: a queue that kept dropping after it overflowed, as CAKE
    /// does with one Test Connection, or a path that slowed during the
    /// Test.
    ///
    /// A Load Sender that fell behind can explain both a slow last one and
    /// a Maximum below an earlier faster one, though not a faster one after
    /// it. So those two are judged only where the Load Sender isn't known
    /// to have fallen behind: not in an Upstream Test in which this host
    /// left more than [`UNSENT_LOAD_PERCENT`] of its Load PDUs unsent,
    /// which its own warning covers. A Downstream Test's Server is judged,
    /// since what it left unsent can't be known here.
    pub fn sub_intervals_after_maximum_missed(
        &self,
        criterion: PerformanceCriterion,
        faster_margin_percent: u32,
        load_sender: LoadSender,
    ) -> Vec<SubIntervalsAfterMaximumMissed> {
        // Whether the Load Sender is judged: see above.
        let judged = !load_sender.fell_behind();
        self.capacity_modes
            .iter()
            .enumerate()
            .filter_map(|(capacity_mode, mode)| {
                let maximum = mode.maximum?;
                let before = self.sub_intervals.get(
                    *mode.sub_intervals.start() as usize - 1..maximum.sub_int_seq_no as usize - 1,
                )?;
                let after: Vec<SubIntervalMeasurement> = self
                    .sub_intervals
                    .get(maximum.sub_int_seq_no as usize..*mode.sub_intervals.end() as usize)?
                    .iter()
                    .flatten()
                    .copied()
                    .collect();
                if after.iter().any(|m| criterion.is_met_by(m)) {
                    return None;
                }
                // Whether `m` ran above `percent` of the Maximum.
                let above = |m: &SubIntervalMeasurement, percent: u32| {
                    u128::from(m.ip_layer_capacity) * 100
                        > u128::from(maximum.ip_layer_capacity) * u128::from(percent)
                };
                let under = |m: &SubIntervalMeasurement, percent: u32| {
                    u128::from(m.ip_layer_capacity) * 100
                        < u128::from(maximum.ip_layer_capacity) * u128::from(percent)
                };
                // The fastest that missed the criterion, the earliest of
                // equals, when more than the margin faster.
                let fastest = |measured: &[SubIntervalMeasurement]| {
                    measured
                        .iter()
                        .rev()
                        .filter(|m| !criterion.is_met_by(m))
                        .max_by_key(|m| m.ip_layer_capacity)
                        .copied()
                        .filter(|m| above(m, 100 + faster_margin_percent))
                };
                let before: Vec<SubIntervalMeasurement> =
                    before.iter().flatten().copied().collect();
                let faster_before = fastest(&before).filter(|_| judged);
                let faster = fastest(&after);
                let slow_last = after
                    .last()
                    .copied()
                    .filter(|m| judged && under(m, SLOW_LAST_SUB_INTERVAL_PERCENT));
                (faster_before.is_some() || faster.is_some() || slow_last.is_some()).then_some(
                    SubIntervalsAfterMaximumMissed {
                        capacity_mode,
                        maximum,
                        faster_before,
                        faster,
                        slow_last,
                    },
                )
            })
            .collect()
    }
}

/// How much faster than the Maximum IP-Layer Capacity, in percent, a later
/// Sub-Interval that missed the Performance Criterion must run for
/// [`MeasurementResults::sub_intervals_after_maximum_missed`] to find it, by
/// default: above the 1-3 % of the points simulated whose Maximum
/// was the bottleneck's rate (2420 of 2500, 1188 of 1200 Mbit/s), below
/// the 8 % of the mildest cliff (3714 of 4000).
pub const FASTER_SUB_INTERVAL_MARGIN_PERCENT: u32 = 5;

/// The share of the Maximum IP-Layer Capacity, in percent, under which the
/// last Sub-Interval of a Capacity Mode, after Sub-Intervals that all
/// missed the Performance Criterion, is found by
/// [`MeasurementResults::sub_intervals_after_maximum_missed`]:
/// below the 87-91 % that a normal Test's dips after its Maximum reached,
/// losing almost nothing, and above the 44-46 % that CAKE left at
/// 2.5 Gbit/s once it kept dropping, losing about 9 %.
pub const SLOW_LAST_SUB_INTERVAL_PERCENT: u32 = 60;

/// The share of its Load PDUs, in percent, that this host may leave unsent
/// over an Upstream Test, in Skipped and Withheld Bursts, for a slow last
/// Sub-Interval to be judged: beyond it, a Load Sender that fell
/// behind may explain it, and its own warning says so.
pub const UNSENT_LOAD_PERCENT: u32 = 1;

/// The Load Sender of the Test the Client judges, and what the Client
/// knows of what it left unsent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadSender {
    /// The Server, in a Downstream Test: what it left unsent isn't known
    /// here.
    Server,
    /// This host, in an Upstream Test: the Load PDUs it sent over the
    /// Test, and those its Skipped and Withheld Bursts left unsent.
    ThisHost { sent: u64, unsent: u64 },
}

impl LoadSender {
    /// The Load Sender of a Test in `direction`, from its Test Connections'
    /// Data Phase events on this host.
    pub fn of(direction: Direction, connections: &[&[DataEvent]]) -> Self {
        match direction {
            Direction::Downstream => Self::Server,
            Direction::Upstream => {
                let (mut sent, mut unsent) = (0u64, 0u64);
                for event in connections.iter().flat_map(|events| events.iter()) {
                    match *event {
                        DataEvent::Sent(s) => sent += u64::from(s.datagrams),
                        DataEvent::BurstsSkipped { datagrams, .. }
                        | DataEvent::BurstsWithheld { datagrams, .. } => {
                            unsent += u64::from(datagrams);
                        }
                        _ => {}
                    }
                }
                Self::ThisHost { sent, unsent }
            }
        }
    }

    /// Whether it is known to have left more than [`UNSENT_LOAD_PERCENT`]
    /// of its Load PDUs unsent.
    fn fell_behind(self) -> bool {
        match self {
            Self::Server => false,
            Self::ThisHost { sent, unsent } => {
                u128::from(unsent) * 100
                    > (u128::from(sent) + u128::from(unsent)) * u128::from(UNSENT_LOAD_PERCENT)
            }
        }
    }
}

/// A Capacity Mode in which no Sub-Interval after the Maximum IP-Layer
/// Capacity's met the Performance Criterion, with a faster one that missed
/// it, before the Maximum's or after it, or a slow last one: the
/// Maximum may be well below the path, or no longer what it carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubIntervalsAfterMaximumMissed {
    /// The Capacity Mode, from 0.
    pub capacity_mode: usize,
    /// The Sub-Interval of its Maximum IP-Layer Capacity.
    pub maximum: SubIntervalMeasurement,
    /// The fastest Sub-Interval before it in the Capacity Mode that missed
    /// the criterion, when more than the margin faster and the Load Sender
    /// isn't known to have fallen behind.
    pub faster_before: Option<SubIntervalMeasurement>,
    /// The fastest Sub-Interval after it, when more than the margin faster.
    pub faster: Option<SubIntervalMeasurement>,
    /// The Capacity Mode's last Sub-Interval measured, when under
    /// [`SLOW_LAST_SUB_INTERVAL_PERCENT`] of the Maximum and the Load
    /// Sender isn't known to have fallen behind.
    pub slow_last: Option<SubIntervalMeasurement>,
}

/// Whether a Test Connection's `data_events` report each Sub-Interval at
/// most once, numbered from 1 to the number the Test Interval holds, as a
/// Data Phase does (data_phase.rs: the Load Sender takes a Status PDU's
/// Sub-Interval only past the last one it took, and the Load Receiver
/// reports one as it closes; neither beyond the Test Interval).
fn reported_as_a_data_phase_does(data_events: &[DataEvent], evaluation: &Evaluation) -> bool {
    let expected = expected_sub_intervals(evaluation.test_interval, evaluation.sub_interval);
    let mut seen = vec![false; expected as usize];
    data_events.iter().all(|event| match event {
        DataEvent::SubIntervalMeasured(m) => m
            .sub_int_seq_no
            .checked_sub(1)
            .and_then(|i| seen.get_mut(i as usize))
            .is_some_and(|seen| !std::mem::replace(seen, true)),
        _ => true,
    })
}

/// The Sub-Intervals measured, in the order reported.
fn measured_sub_intervals(data_events: &[DataEvent]) -> Vec<SubIntervalMeasurement> {
    data_events
        .iter()
        .filter_map(|e| match e {
            DataEvent::SubIntervalMeasured(m) => Some(*m),
            _ => None,
        })
        .collect()
}

/// The st a Load Sender measured, or, a Load Receiver's, those it
/// estimated, and which.
fn sent_intervals(data_events: &[DataEvent]) -> (Vec<SentInterval>, bool) {
    let measured: Vec<SentInterval> = data_events
        .iter()
        .filter_map(|e| match e {
            DataEvent::Sent(s) => Some(*s),
            _ => None,
        })
        .collect();
    if !measured.is_empty() {
        return (measured, false);
    }
    let estimated: Vec<SentInterval> = data_events
        .iter()
        .filter_map(|e| match e {
            DataEvent::SentEstimated(s) => Some(*s),
            _ => None,
        })
        .collect();
    let any = !estimated.is_empty();
    (estimated, any)
}

/// A range of delay variation samples in one Sub-Interval, in ms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DelayRange {
    pub min: u32,
    pub max: u32,
    /// The samples' mean, rounded half up.
    pub mean: u32,
}

/// One Sub-Interval's measurements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubIntervalMeasurement {
    /// subIntSeqNo: 1 for the first Sub-Interval of the Test.
    pub sub_int_seq_no: u32,
    /// Load PDUs received.
    pub datagrams: u32,
    /// Their UDP payload bytes.
    pub bytes: u64,
    /// The IP-Layer Capacity, C(T,dt,PM) of RFC 9097 §5.3: the IP-layer
    /// bit rate received, IP and UDP headers included (28 bytes a datagram
    /// on IPv4, 48 on IPv6), over the nominal Sub-Interval (subIntPeriod),
    /// in bit/s. The Server makes subIntPeriod a whole number of Trial
    /// Intervals, so every Sub-Interval runs its nominal length (RFC 9097
    /// §5.3: equal durations).
    pub ip_layer_capacity: u64,
    /// Sequence Errors: Load PDUs lost, out of order and duplicated.
    pub loss: u32,
    pub out_of_order: u32,
    pub duplicates: u32,
    /// One-way delay variation, if any sample was taken.
    pub delay_variation: Option<DelayRange>,
    /// The delay variation samples' sum, in ms, and count: for a mean
    /// pooled over several Test Connections.
    pub delay_variation_sum: u64,
    pub delay_variation_count: u32,
    /// The range of round-trip time variation samples, if any.
    pub rtt_variation: Option<(u32, u32)>,
    /// The RTT variation samples' sum, in ms, and count: for a mean pooled
    /// over several Sub-Intervals.
    pub rtt_variation_sum: u64,
    pub rtt_variation_count: u32,
    /// The mean of the RTT variation samples, rounded half up, if any. The
    /// Load Receiver averages every sample; a Load Sender only the one
    /// each Status PDU carries, an approximation that can even fall
    /// outside `rtt_variation`.
    pub rtt_variation_mean: Option<u32>,
    /// rttMinimum as the Sub-Interval closed, in ms: the smallest RTT so
    /// far, which the variation figures are measured from.
    pub rtt_minimum: Option<u32>,
    /// clockDeltaMin as the Sub-Interval closed, in ms: the smallest
    /// receive time minus lpduTime so far, the minimum one-way delay when
    /// the clocks are synchronized. A Load Receiver without a sample yet
    /// has `None`; a Status PDU carries 0 then (docs/spec.md Time model: never
    /// read as "no value").
    pub clock_delta_min: Option<i32>,
    /// How many Test Connections received Load PDUs in it: 0 or 1 for one
    /// Test Connection's own measurement, more in a Multi-Connection
    /// Test's aggregate.
    pub active_test_connections: u8,
    /// When this end learned the Sub-Interval's results: as it closed, or,
    /// for a Load Sender, when the Status PDU carrying them arrived. In an
    /// Upstream Multi-Connection Test, how far apart the Test Connections'
    /// Sub-Intervals lie.
    pub measured_at: MonoTime,
    /// How long the Sub-Interval actually ran.
    pub duration: Duration,
    /// How long the Sub-Intervals so far ran together, as the Load
    /// Receiver measured them (accumTime).
    pub accum_time: Duration,
}

/// What the Load Sender sent in one st (RFC 9097 §7.2), a Trial Interval
/// long, on a grid from its first send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SentInterval {
    /// 1 for the first st of the Test.
    pub st_seq_no: u32,
    /// Load PDUs that reached the kernel.
    pub datagrams: u32,
    /// Their UDP payload bytes.
    pub bytes: u64,
    /// The IP-Layer Sender Bit Rate: IP and UDP headers included, over the
    /// nominal st, in bit/s.
    pub ip_layer_sender_bit_rate: u64,
    /// st.
    pub duration: Duration,
}

impl SentInterval {
    pub(crate) fn new(
        st_seq_no: u32,
        datagrams: u32,
        bytes: u64,
        duration: Duration,
        ipv4: bool,
    ) -> Self {
        let ip_bytes = bytes + u64::from(datagrams) * header_bytes(ipv4);
        let nanos = duration.as_nanos().max(1);
        Self {
            st_seq_no,
            datagrams,
            bytes,
            ip_layer_sender_bit_rate: u64::try_from(
                u128::from(ip_bytes) * 8 * 1_000_000_000 / nanos,
            )
            .unwrap_or(u64::MAX),
            duration,
        }
    }
}

/// The IP-Layer Sender Bit Rate over each Sub-Interval: the st intervals
/// that start in it, summed, over the time they cover. A Sub-Interval
/// whose st weren't all reported, such as one after sending stopped, is
/// `None`. Each end keeps its own grid, so a Sub-Interval of the Load
/// Receiver lags the Load Sender's by the one-way delay; the sums are aligned
/// only to within that.
fn sent_by_sub_interval(
    expected: u32,
    sub_interval: Duration,
    sent: &[SentInterval],
    ipv4: bool,
) -> Vec<Option<SentInterval>> {
    let dt = sub_interval.as_nanos().max(1);
    // Per Sub-Interval: st seen, st expected, datagrams, bytes, duration.
    let mut sums = vec![(0u32, 0u32, 0u32, 0u64, Duration::ZERO); expected as usize];
    let Some(st) = sent.first().map(|s| s.duration.as_nanos().max(1)) else {
        return vec![None; expected as usize];
    };
    let slot_of = |k: u128| usize::try_from(k * st / dt).unwrap_or(usize::MAX);
    for k in 0..(u128::from(expected) * dt).div_ceil(st) {
        if let Some(sum) = sums.get_mut(slot_of(k)) {
            sum.1 += 1;
        }
    }
    for interval in sent {
        let k = u128::from(interval.st_seq_no.saturating_sub(1));
        if let Some(sum) = sums.get_mut(slot_of(k)) {
            sum.0 += 1;
            sum.2 += interval.datagrams;
            sum.3 += interval.bytes;
            sum.4 += interval.duration;
        }
    }
    sums.into_iter()
        .zip(1..)
        .map(|((seen, whole, datagrams, bytes, duration), seq)| {
            (seen == whole).then(|| SentInterval::new(seq, datagrams, bytes, duration, ipv4))
        })
        .collect()
}

/// IP and UDP header bytes of one datagram.
pub(crate) fn header_bytes(ipv4: bool) -> u64 {
    if ipv4 { 28 } else { 48 }
}

impl SubIntervalMeasurement {
    /// The measurements in `stats`, with the delay variation sum kept
    /// unsaturated where it can be, for a Sub-Interval of `nominal` length
    /// over IPv4 or IPv6.
    pub(crate) fn new(
        sub_int_seq_no: u32,
        stats: &SubIntervalStats,
        delay_var_sum: u64,
        nominal: Duration,
        ipv4: bool,
    ) -> Self {
        let ip_bytes = stats.rx_bytes + u64::from(stats.rx_datagrams) * header_bytes(ipv4);
        let nanos = nominal.as_nanos().max(1);
        let ip_layer_capacity =
            u64::try_from(u128::from(ip_bytes) * 8 * 1_000_000_000 / nanos).unwrap_or(u64::MAX);
        let delay_variation = (stats.delay_var_cnt > 0).then(|| DelayRange {
            min: stats.delay_var_min,
            max: stats.delay_var_max,
            mean: u32::try_from(
                (u128::from(delay_var_sum) * 2 + u128::from(stats.delay_var_cnt))
                    / (2 * u128::from(stats.delay_var_cnt)),
            )
            .unwrap_or(u32::MAX),
        });
        let rtt_variation = (stats.rtt_var_maximum != STATUS_NODEL
            && stats.rtt_var_minimum != STATUS_NODEL)
            .then_some((stats.rtt_var_minimum, stats.rtt_var_maximum));
        Self {
            sub_int_seq_no,
            datagrams: stats.rx_datagrams,
            bytes: stats.rx_bytes,
            ip_layer_capacity,
            loss: stats.seq_err_loss,
            out_of_order: stats.seq_err_ooo,
            duplicates: stats.seq_err_dup,
            delay_variation,
            delay_variation_sum: delay_var_sum,
            delay_variation_count: stats.delay_var_cnt,
            rtt_variation,
            rtt_variation_sum: 0,
            rtt_variation_count: 0,
            rtt_variation_mean: None,
            rtt_minimum: None,
            clock_delta_min: None,
            active_test_connections: u8::from(stats.rx_datagrams > 0),
            measured_at: MonoTime::from_duration(Duration::ZERO),
            duration: Duration::from_micros(stats.delta_time.into()),
            accum_time: Duration::from_millis(stats.accum_time.into()),
        }
    }
}

/// How many Sub-Intervals a Test Interval holds.
pub(crate) fn expected_sub_intervals(test_interval: Duration, sub_interval: Duration) -> u32 {
    let nanos = sub_interval.as_nanos().max(1);
    u32::try_from(test_interval.as_nanos() / nanos).unwrap_or(u32::MAX)
}

/// The measurements laid out by Sub-Interval, 1 to `expected`: a
/// Sub-Interval never measured, such as a final one whose Status PDU was
/// lost, is `None`, never a zero (docs/spec.md). A later copy of a
/// Sub-Interval replaces none: the first one counts.
fn by_sub_interval(
    expected: u32,
    measured: &[SubIntervalMeasurement],
) -> Vec<Option<SubIntervalMeasurement>> {
    let mut slots = vec![None; expected as usize];
    for m in measured {
        if let Some(slot) = m
            .sub_int_seq_no
            .checked_sub(1)
            .and_then(|i| slots.get_mut(i as usize))
            && slot.is_none()
        {
            *slot = Some(*m);
        }
    }
    slots
}

impl SubIntervalMeasurement {
    /// The Load PDUs sent in the Sub-Interval, as the Load Receiver can
    /// tell: received and lost. Duplicates count as received, as in
    /// OB-UDPST. (conformance: duplicates-count-as-received)
    pub fn datagrams_sent(&self) -> u64 {
        u64::from(self.datagrams) + u64::from(self.loss)
    }
}

/// The Performance Criterion (RFC 9097 §6.3, the PM criterion): a
/// Sub-Interval meets it if it lost at most `max_loss_ppm` parts per
/// million of the Load PDUs sent, or a single one, so that one loss at a
/// low rate doesn't disqualify a Sub-Interval: one per active Test
/// Connection in a Multi-Connection Test's aggregate, so that it is no
/// harsher than its Test Connections alone.
/// (conformance: loss-allowance-per-test-connection)
///
/// Loss is counted from lpduSeqNo gaps, and a late Load PDU cancels the
/// loss it closes only within the Sub-Interval that counted it: Tmax, the
/// longest a Load PDU may take to count as received, is the rest of that
/// Sub-Interval, at most dt. (conformance: tmax-within-the-sub-interval)
///
/// A Sub-Interval in which no Load PDU was received meets no criterion,
/// even when the allowance covers what was lost: RFC 9097 §6.3 takes the
/// maximum over bits correctly received, and none were; with none sent
/// either, its Loss Ratio is undefined, not zero.
/// (conformance: empty-sub-interval-meets-no-criterion)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PerformanceCriterion {
    pub max_loss_ppm: u32,
}

impl PerformanceCriterion {
    /// 1 % of the Load PDUs sent: loose enough that Algorithm B's own
    /// search, which tolerates up to seqErrThresh losses a Trial Interval,
    /// leaves Sub-Intervals near capacity that meet it. That holds only
    /// where the search's cycle around the bottleneck loses under 1 %.
    /// Through a drop-tail queue whose delay variation stays below
    /// upperThresh it often loses more, from the 1 Gbps row up and over long
    /// round trips, and the Maximum IP-Layer Capacity then falls to a
    /// Sub-Interval of the search's climb, which
    /// [`MeasurementResults::sub_intervals_after_maximum_missed`] finds for a
    /// warning.
    pub const DEFAULT: Self = Self {
        max_loss_ppm: 10_000,
    };

    pub fn is_met_by(self, measurement: &SubIntervalMeasurement) -> bool {
        let sent = measurement.datagrams_sent();
        let loss = u64::from(measurement.loss);
        let allowance = u64::from(measurement.active_test_connections.max(1));
        measurement.datagrams > 0
            && (loss <= allowance || loss * 1_000_000 <= u64::from(self.max_loss_ppm) * sent)
    }
}

impl Default for PerformanceCriterion {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// One Capacity Mode's results (RFC 9097 §6.3, §6.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapacityModeResult {
    /// Its Sub-Intervals, by subIntSeqNo.
    pub sub_intervals: RangeInclusive<u32>,
    /// The Sub-Interval of the Maximum IP-Layer Capacity: the highest
    /// IP-Layer Capacity of those that meet the Performance Criterion, the
    /// earliest on a tie. `None` when none does: then the Capacity Mode
    /// has no valid result.
    pub maximum: Option<SubIntervalMeasurement>,
    /// The Sub-Interval of the Highest IP-Layer Capacity, whether or not
    /// it meets the criterion: what OB-UDPST reports as its maximum.
    pub highest: Option<SubIntervalMeasurement>,
    /// How many of its Sub-Intervals were never measured, such as a final
    /// one whose Status PDU was lost. None of them can be the maximum.
    pub missing: u32,
}

/// The results of each Capacity Mode of a Test whose Test Interval holds
/// `expected` Sub-Intervals: one Capacity Mode, or two when the user put
/// the first `first_mode` Sub-Intervals in one of their own (RFC 9097
/// §6.6; TR-471's NumberFirstModeTestSubIntervals). A split outside the
/// Test Interval leaves one. A Sub-Interval measured twice counts once, as
/// first measured. In a Multi-Connection Test, `measured` is the
/// aggregate of its Test Connections.
fn capacity_modes(
    expected: u32,
    measured: &[SubIntervalMeasurement],
    criterion: PerformanceCriterion,
    first_mode: Option<u32>,
) -> Vec<CapacityModeResult> {
    let slots = by_sub_interval(expected, measured);
    let modes: Vec<RangeInclusive<u32>> = match first_mode {
        Some(split) if split > 0 && split < expected => vec![1..=split, split + 1..=expected],
        _ => std::iter::once(1..=expected).collect(),
    };
    modes
        .into_iter()
        .map(|sub_intervals| {
            let slots = slots
                .get(*sub_intervals.start() as usize - 1..*sub_intervals.end() as usize)
                .unwrap_or_default();
            // The highest, the earliest of equals (max_by_key keeps the
            // last, so it runs backwards).
            let highest = |meets: &dyn Fn(&SubIntervalMeasurement) -> bool| {
                slots
                    .iter()
                    .rev()
                    .flatten()
                    .filter(|m| meets(m))
                    .max_by_key(|m| m.ip_layer_capacity)
                    .copied()
            };
            CapacityModeResult {
                maximum: highest(&|m| criterion.is_met_by(m)),
                highest: highest(&|_| true),
                missing: slots.iter().filter(|s| s.is_none()).count() as u32,
                sub_intervals,
            }
        })
        .collect()
}

/// A Multi-Connection Test's Sub-Intervals: each Test Connection's
/// measurements of the same subIntSeqNo combined, in order (docs/spec.md
/// "Multi-Connection aggregation"; ). The Test Connections must share
/// one grid, as the Load Receivers of a Downstream Test do, or the sums
/// mix different times. Counts and the IP-Layer Capacity add up; delay
/// and RTT variation keep the extremes and pool the means; rttMinimum and
/// clockDeltaMin take the smallest, as OB-UDPST does. A Sub-Interval
/// measured twice by one Test Connection counts once, the first copy.
/// clockDeltaMin mixes the Test Connections' clock offsets, so it is kept
/// only when `one_server`. (conformance: aggregate-clock-delta-min)
fn aggregate(
    connections: &[Vec<SubIntervalMeasurement>],
    one_server: bool,
) -> Vec<SubIntervalMeasurement> {
    let mut bins: std::collections::BTreeMap<u32, SubIntervalMeasurement> =
        std::collections::BTreeMap::new();
    for measured in connections {
        let mut seen = std::collections::BTreeSet::new();
        for m in measured.iter().filter(|m| seen.insert(m.sub_int_seq_no)) {
            let m = SubIntervalMeasurement {
                clock_delta_min: m.clock_delta_min.filter(|_| one_server),
                ..*m
            };
            bins.entry(m.sub_int_seq_no)
                .and_modify(|bin| *bin = combine(bin, &m))
                .or_insert(m);
        }
    }
    bins.into_values().collect()
}

/// What a Multi-Connection Test's Test Connections sent, st by st: each
/// Load Sender keeps its own st from its first send, so, like Upstream
/// Sub-Intervals, the sum by number is approximate.
fn aggregate_sent(connections: &[Vec<SentInterval>]) -> Vec<SentInterval> {
    let mut sums: std::collections::BTreeMap<u32, SentInterval> = std::collections::BTreeMap::new();
    for sent in connections {
        for s in sent {
            sums.entry(s.st_seq_no)
                .and_modify(|sum| {
                    sum.datagrams = sum.datagrams.saturating_add(s.datagrams);
                    sum.bytes = sum.bytes.saturating_add(s.bytes);
                    sum.ip_layer_sender_bit_rate = sum
                        .ip_layer_sender_bit_rate
                        .saturating_add(s.ip_layer_sender_bit_rate);
                })
                .or_insert(*s);
        }
    }
    sums.into_values().collect()
}

/// How far apart, at most, the Test Connections learned the results of
/// the same Sub-Interval: in an Upstream Multi-Connection Test each Test
/// Connection's Load Receiver keeps its own Sub-Intervals, so their sum by
/// number is only
/// approximate, off by up to this much (docs/spec.md; ).
/// (conformance: approximate-upstream-aggregation)
fn largest_offset(connections: &[Vec<SubIntervalMeasurement>]) -> Duration {
    let mut spans: std::collections::BTreeMap<u32, (MonoTime, MonoTime)> =
        std::collections::BTreeMap::new();
    for measured in connections {
        let mut seen = std::collections::BTreeSet::new();
        for m in measured.iter().filter(|m| seen.insert(m.sub_int_seq_no)) {
            spans
                .entry(m.sub_int_seq_no)
                .and_modify(|(low, high)| {
                    *low = (*low).min(m.measured_at);
                    *high = (*high).max(m.measured_at);
                })
                .or_insert((m.measured_at, m.measured_at));
        }
    }
    spans
        .values()
        .map(|(low, high)| high.as_duration().saturating_sub(low.as_duration()))
        .max()
        .unwrap_or_default()
}

/// Two measurements of the same Sub-Interval by different Test
/// Connections, as one.
fn combine(a: &SubIntervalMeasurement, b: &SubIntervalMeasurement) -> SubIntervalMeasurement {
    let smallest = |x: Option<u32>, y: Option<u32>| x.into_iter().chain(y).min();
    let delay_variation_sum = a.delay_variation_sum + b.delay_variation_sum;
    let delay_variation_count = a
        .delay_variation_count
        .saturating_add(b.delay_variation_count);
    let delay_variation = match (a.delay_variation, b.delay_variation) {
        (Some(x), Some(y)) => Some(DelayRange {
            min: x.min.min(y.min),
            max: x.max.max(y.max),
            mean: mean_of(delay_variation_sum, delay_variation_count).unwrap_or(x.mean),
        }),
        (x, y) => x.or(y),
    };
    let rtt_variation = match (a.rtt_variation, b.rtt_variation) {
        (Some((low, high)), Some((other_low, other_high))) => {
            Some((low.min(other_low), high.max(other_high)))
        }
        (x, y) => x.or(y),
    };
    let rtt_variation_sum = a.rtt_variation_sum + b.rtt_variation_sum;
    let rtt_variation_count = a.rtt_variation_count.saturating_add(b.rtt_variation_count);
    SubIntervalMeasurement {
        sub_int_seq_no: a.sub_int_seq_no,
        datagrams: a.datagrams.saturating_add(b.datagrams),
        bytes: a.bytes.saturating_add(b.bytes),
        ip_layer_capacity: a.ip_layer_capacity.saturating_add(b.ip_layer_capacity),
        loss: a.loss.saturating_add(b.loss),
        out_of_order: a.out_of_order.saturating_add(b.out_of_order),
        duplicates: a.duplicates.saturating_add(b.duplicates),
        delay_variation,
        delay_variation_sum,
        delay_variation_count,
        rtt_variation,
        rtt_variation_sum,
        rtt_variation_count,
        rtt_variation_mean: mean_of(rtt_variation_sum, rtt_variation_count)
            .or(a.rtt_variation_mean)
            .or(b.rtt_variation_mean),
        rtt_minimum: smallest(a.rtt_minimum, b.rtt_minimum),
        clock_delta_min: a.clock_delta_min.into_iter().chain(b.clock_delta_min).min(),
        active_test_connections: a
            .active_test_connections
            .saturating_add(b.active_test_connections),
        measured_at: a.measured_at.max(b.measured_at),
        duration: a.duration.max(b.duration),
        accum_time: a.accum_time.max(b.accum_time),
    }
}

/// The mean of samples given as a sum and a count, rounded half up.
fn mean_of(sum: u64, count: u32) -> Option<u32> {
    (count > 0).then(|| {
        let count = u64::from(count);
        u32::try_from((sum * 2 + count) / (2 * count)).unwrap_or(u32::MAX)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(datagrams: u32, bytes: u64) -> SubIntervalStats {
        SubIntervalStats {
            rx_datagrams: datagrams,
            rx_bytes: bytes,
            delta_time: 1_000_000,
            rtt_var_minimum: STATUS_NODEL,
            rtt_var_maximum: STATUS_NODEL,
            delay_var_min: STATUS_NODEL,
            ..SubIntervalStats::default()
        }
    }

    #[test]
    fn the_rate_includes_the_headers_over_the_nominal_sub_interval() {
        // 1000 datagrams of 1222 bytes: 1,250,000 IP bytes a second on IPv4.
        let v4 = SubIntervalMeasurement::new(
            1,
            &stats(1000, 1_222_000),
            0,
            Duration::from_secs(1),
            true,
        );
        assert_eq!(v4.ip_layer_capacity, 10_000_000);
        let v6 = SubIntervalMeasurement::new(
            1,
            &stats(1000, 1_222_000),
            0,
            Duration::from_secs(1),
            false,
        );
        assert_eq!(v6.ip_layer_capacity, 10_160_000);
        // The nominal Sub-Interval, not the measured one.
        let half = SubIntervalMeasurement::new(
            1,
            &stats(1000, 1_222_000),
            0,
            Duration::from_millis(500),
            true,
        );
        assert_eq!(half.ip_layer_capacity, 20_000_000);
    }

    #[test]
    fn no_sample_is_none_not_zero() {
        let m = SubIntervalMeasurement::new(1, &stats(0, 0), 0, Duration::from_secs(1), true);
        assert_eq!((m.delay_variation, m.rtt_variation), (None, None));
        let with = SubIntervalStats {
            delay_var_min: 1,
            delay_var_max: 9,
            delay_var_sum: 10,
            delay_var_cnt: 4,
            rtt_var_minimum: 2,
            rtt_var_maximum: 7,
            ..stats(4, 400)
        };
        let m = SubIntervalMeasurement::new(1, &with, 10, Duration::from_secs(1), true);
        assert_eq!(
            m.delay_variation,
            Some(DelayRange {
                min: 1,
                max: 9,
                mean: 3
            })
        );
        assert_eq!(m.rtt_variation, Some((2, 7)));
    }

    fn at(seq: u32, datagrams: u32, loss: u32) -> SubIntervalMeasurement {
        SubIntervalMeasurement {
            loss,
            ..SubIntervalMeasurement::new(
                seq,
                &stats(datagrams, u64::from(datagrams) * 1000),
                0,
                Duration::from_secs(1),
                true,
            )
        }
    }

    //
    #[udpstp_req::req("RFC9097-6.3-L1", "RFC9097-4-4", "RFC9097-6.3-6")]
    #[test]
    fn the_maximum_meets_the_criterion_and_the_highest_need_not() {
        let measured = [at(1, 500, 0), at(2, 1000, 50), at(3, 900, 9), at(4, 900, 1)];
        let modes = capacity_modes(4, &measured, PerformanceCriterion::DEFAULT, None);
        assert_eq!(modes.len(), 1);
        let mode = &modes[0];
        // 50 of 1050 is over 1 %; 9 of 909 isn't; 3 and 4 tie, 3 is earlier.
        assert_eq!(mode.maximum.map(|m| m.sub_int_seq_no), Some(3));
        assert_eq!(mode.highest.map(|m| m.sub_int_seq_no), Some(2));
        assert_eq!((mode.sub_intervals.clone(), mode.missing), (1..=4, 0));
    }

    // : a single loss never disqualifies a Sub-Interval, whatever the
    // rate
    #[test]
    fn one_loss_always_meets_the_criterion() {
        let strict = PerformanceCriterion { max_loss_ppm: 0 };
        assert!(strict.is_met_by(&at(1, 10, 1)));
        assert!(!strict.is_met_by(&at(1, 10_000, 2)));
        assert!(PerformanceCriterion::DEFAULT.is_met_by(&at(1, 198, 2)));
        assert!(!PerformanceCriterion::DEFAULT.is_met_by(&at(1, 197, 3)));
    }

    // : a Sub-Interval in which no Load PDU was received meets no
    // criterion, however loose, even when the one-loss allowance covers
    // what was lost, so it can't be the Maximum IP-Layer Capacity: RFC 9097
    // §6.3 takes that over bits correctly received. It can still be the
    // highest where every Sub-Interval is as empty
    // (conformance: empty-sub-interval-meets-no-criterion)
    #[udpstp_req::req("RFC9097-6.3-L1")]
    #[test]
    fn a_sub_interval_with_nothing_received_meets_no_criterion() {
        let empty = at(2, 0, 0);
        assert_eq!(empty.datagrams_sent(), 0);
        // One Load PDU lost and none received: within the allowance, but
        // nothing correctly received.
        let lost = at(2, 0, 1);
        for max_loss_ppm in [0, 10_000, 1_000_000] {
            let criterion = PerformanceCriterion { max_loss_ppm };
            assert!(!criterion.is_met_by(&empty) && !criterion.is_met_by(&lost));
        }
        // One Load PDU received is something, with one lost beside it too.
        assert!(PerformanceCriterion::DEFAULT.is_met_by(&at(2, 1, 0)));
        assert!(PerformanceCriterion::DEFAULT.is_met_by(&at(2, 1, 1)));
        // The only Sub-Intervals that would have passed were empty.
        for nothing_received in [empty, lost] {
            let modes = capacity_modes(
                3,
                &[at(1, 100, 50), nothing_received, at(3, 100, 60)],
                PerformanceCriterion::DEFAULT,
                None,
            );
            assert_eq!(modes[0].maximum, None);
            assert_eq!(modes[0].highest.map(|m| m.sub_int_seq_no), Some(1));
        }
        let nothing = capacity_modes(1, &[at(1, 0, 0)], PerformanceCriterion::DEFAULT, None);
        assert_eq!(
            (
                nothing[0].maximum,
                nothing[0].highest.map(|m| m.sub_int_seq_no)
            ),
            (None, Some(1))
        );
    }

    // A result meeting no PM criterion is not the Maximum IP-Layer
    // Capacity;
    #[udpstp_req::req("RFC9097-6.3-3", "RFC9097-6.3-L1")]
    #[test]
    fn no_sub_interval_meeting_the_criterion_means_no_maximum() {
        let modes = capacity_modes(
            2,
            &[at(1, 100, 50), at(2, 100, 60)],
            PerformanceCriterion::DEFAULT,
            None,
        );
        assert_eq!(modes[0].maximum, None);
        assert_eq!(modes[0].highest.map(|m| m.sub_int_seq_no), Some(1));
    }

    // : a Maximum IP-Layer Capacity followed only by Sub-Intervals
    // that missed the Performance Criterion, one of them more than the
    // margin faster, is found, with the fastest of them; one that a later
    // Sub-Interval met the criterion after, or none faster by the margin,
    // isn't
    #[test]
    fn faster_sub_intervals_that_all_missed_the_criterion_are_found() {
        let evaluation = |first_mode| Evaluation {
            test_interval: Duration::from_secs(5),
            sub_interval: Duration::from_secs(1),
            ip_version: IpVersion::V4,
            criterion: PerformanceCriterion::DEFAULT,
            first_mode,
        };
        let found = |measured: &[SubIntervalMeasurement], first_mode, margin| {
            let events: Vec<DataEvent> = measured
                .iter()
                .map(|m| DataEvent::SubIntervalMeasured(*m))
                .collect();
            MeasurementResults::of_test_connection(&events, &evaluation(first_mode))
                .sub_intervals_after_maximum_missed(
                    PerformanceCriterion::DEFAULT,
                    margin,
                    LoadSender::Server,
                )
                .into_iter()
                .map(|f| {
                    (
                        f.capacity_mode,
                        f.maximum.sub_int_seq_no,
                        f.faster.map(|m| m.sub_int_seq_no),
                    )
                })
                .collect::<Vec<_>>()
        };
        // The climb's Sub-Interval 2 meets it; 3-5 run 50-60 % faster and
        // lose 2-5 %: 4 is the fastest.
        let cliff = [
            at(1, 400, 0),
            at(2, 800, 0),
            at(3, 1200, 30),
            at(4, 1250, 60),
            at(5, 1200, 25),
        ];
        assert_eq!(found(&cliff, None, 5), [(0, 2, Some(4))]);
        // A margin above what they reached finds nothing.
        assert!(found(&cliff, None, 60).is_empty());
        // A later Sub-Interval that met the criterion, even a slower one:
        // the search found the rate again.
        let met_later = [cliff[0], cliff[1], cliff[2], at(4, 790, 0), cliff[4]];
        assert!(found(&met_later, None, 5).is_empty());
        // Faster, but within the margin.
        let close = [at(1, 400, 0), at(2, 1000, 0), at(3, 1040, 30)];
        assert!(found(&close, None, 5).is_empty());
        // Nothing after the Maximum's: nothing to say.
        assert!(found(&[at(1, 400, 0), at(2, 800, 0)], None, 5).is_empty());
        // Each Capacity Mode on its own: the second holds the cliff; split
        // after Sub-Interval 2, the second has no Maximum to warn of, and
        // the first nothing after its own.
        assert_eq!(found(&cliff, Some(1), 5), [(1, 2, Some(4))]);
        assert!(found(&cliff, Some(2), 5).is_empty());
    }

    // : a Maximum IP-Layer Capacity followed only by Sub-Intervals
    // that missed the Performance Criterion, the last of them under 60 % of
    // it, is found with that last one, as CAKE gives when it keeps dropping
    // a lone Test Connection's Load PDUs after it overflowed; a brief outage
    // the search recovered from isn't, nor is an Upstream Test whose host
    // left more than 1 % of its Load PDUs unsent, which its own warning
    // covers
    #[test]
    fn a_slow_last_sub_interval_after_the_maximum_is_found() {
        let evaluation = |first_mode| Evaluation {
            test_interval: Duration::from_secs(8),
            sub_interval: Duration::from_secs(1),
            ip_version: IpVersion::V4,
            criterion: PerformanceCriterion::DEFAULT,
            first_mode,
        };
        let found = |measured: &[SubIntervalMeasurement], first_mode, load_sender| {
            let events: Vec<DataEvent> = measured
                .iter()
                .map(|m| DataEvent::SubIntervalMeasured(*m))
                .collect();
            MeasurementResults::of_test_connection(&events, &evaluation(first_mode))
                .sub_intervals_after_maximum_missed(
                    PerformanceCriterion::DEFAULT,
                    FASTER_SUB_INTERVAL_MARGIN_PERCENT,
                    load_sender,
                )
                .into_iter()
                .map(|f| {
                    (
                        f.capacity_mode,
                        f.maximum.sub_int_seq_no,
                        f.faster.map(|m| m.sub_int_seq_no),
                        f.slow_last.map(|m| m.sub_int_seq_no),
                    )
                })
                .collect::<Vec<_>>()
        };
        // CAKE at 2.5 Gbit/s: the climb's 1960 is the Maximum, the
        // overshoot's 1976 loses 11.7 %, only 0.8 % faster, and the rest
        // run at 44-46 % of it, losing about 8.7 %.
        let cake = [
            at(1, 500, 0),
            at(2, 1000, 0),
            at(3, 1500, 0),
            at(4, 1960, 0),
            at(5, 1976, 262),
            at(6, 910, 87),
            at(7, 886, 84),
            at(8, 870, 83),
        ];
        assert_eq!(
            found(&cake, None, LoadSender::Server),
            [(0, 4, None, Some(8))]
        );
        // Upstream, this host left 1 % unsent: judged; more: not.
        let this_host = |unsent| LoadSender::ThisHost {
            sent: 100_000 - unsent,
            unsent,
        };
        assert_eq!(
            found(&cake, None, this_host(1_000)),
            [(0, 4, None, Some(8))]
        );
        assert!(found(&cake, None, this_host(1_001)).is_empty());
        // The faster Sub-Interval still counts where this host fell
        // behind: only the slow last one is left to its own warning.
        let both = [
            cake[0],
            cake[1],
            cake[2],
            cake[3],
            at(5, 2200, 300),
            cake[5],
            cake[6],
            cake[7],
        ];
        assert_eq!(
            found(&both, None, LoadSender::Server),
            [(0, 4, Some(5), Some(8))]
        );
        assert_eq!(
            found(&both, None, this_host(5_000)),
            [(0, 4, Some(5), None)]
        );
        // A brief outage after a correct Maximum, which the search
        // recovered from: the last Sub-Interval met the criterion.
        let outage = [at(1, 500, 0), at(2, 500, 0), at(3, 101, 460), at(4, 500, 4)];
        assert!(found(&outage, None, LoadSender::Server).is_empty());
        // Any later Sub-Interval that met it, even before a slow last one.
        let met_later = [
            cake[0],
            cake[1],
            cake[2],
            cake[3],
            cake[4],
            at(6, 910, 0),
            cake[6],
            cake[7],
        ];
        assert!(found(&met_later, None, LoadSender::Server).is_empty());
        // At 60 % of the Maximum or above: nothing to say.
        let at_60 = [
            cake[0],
            cake[1],
            cake[2],
            cake[3],
            cake[4],
            cake[5],
            cake[6],
            at(8, 1176, 83),
        ];
        assert!(found(&at_60, None, LoadSender::Server).is_empty());
        // The last one measured: a final Sub-Interval never measured
        // leaves the one before it.
        assert_eq!(
            found(&cake[..7], None, LoadSender::Server),
            [(0, 4, None, Some(7))]
        );
        // Each Capacity Mode on its own: split after Sub-Interval 6, the
        // first has the slow Sub-Interval 6 last, and the second, whose
        // Sub-Intervals all miss the criterion, no Maximum.
        assert_eq!(
            found(&cake, Some(6), LoadSender::Server),
            [(0, 4, None, Some(6))]
        );
    }

    // : a Maximum IP-Layer Capacity after a Sub-Interval that missed
    // the Performance Criterion more than the margin faster, with none
    // after it that met the criterion, is found with that one, as CAKE
    // gives with two Test Connections at 2.5 Gbit/s, whose search climbs
    // back slowly after its overshoot and ends at 78 % of the link; a
    // correct Maximum after a lossy Sub-Interval within the margin isn't,
    // nor one a later Sub-Interval met the criterion after, nor an Upstream
    // Test whose host left more than 1 % of its Load PDUs unsent
    #[test]
    fn a_faster_sub_interval_before_the_maximum_is_found() {
        let evaluation = |first_mode| Evaluation {
            test_interval: Duration::from_secs(10),
            sub_interval: Duration::from_secs(1),
            ip_version: IpVersion::V4,
            criterion: PerformanceCriterion::DEFAULT,
            first_mode,
        };
        let found = |measured: &[SubIntervalMeasurement], first_mode, load_sender| {
            let events: Vec<DataEvent> = measured
                .iter()
                .map(|m| DataEvent::SubIntervalMeasured(*m))
                .collect();
            MeasurementResults::of_test_connection(&events, &evaluation(first_mode))
                .sub_intervals_after_maximum_missed(
                    PerformanceCriterion::DEFAULT,
                    FASTER_SUB_INTERVAL_MARGIN_PERCENT,
                    load_sender,
                )
                .into_iter()
                .map(|f| {
                    (
                        f.capacity_mode,
                        f.maximum.sub_int_seq_no,
                        f.faster_before.map(|m| m.sub_int_seq_no),
                        f.faster.map(|m| m.sub_int_seq_no),
                        f.slow_last.map(|m| m.sub_int_seq_no),
                    )
                })
                .collect::<Vec<_>>()
        };
        // A Downstream Test through CAKE at 2.5 Gbit/s with two Test
        // Connections (testhost, 2026-09-30), in Load PDUs received at a
        // tenth of its Mbit/s, with its Loss Ratios: the overshoot in
        // Sub-Interval 6 reached 2262 Mbit/s losing 9.0 %, and the climb
        // back, 40 Mbit/s a Sub-Interval, ended with the Maximum, 1953, in
        // the last, 15.9 % below it.
        let cake = [
            at(1, 1665, 0),
            at(2, 5660, 0),
            at(3, 9660, 0),
            at(4, 13662, 0),
            at(5, 17659, 0),
            at(6, 22623, 2239),
            at(7, 18312, 1401),
            at(8, 18745, 569),
            at(9, 19126, 22),
            at(10, 19526, 0),
        ];
        assert_eq!(
            found(&cake, None, LoadSender::Server),
            [(0, 10, Some(6), None, None)]
        );
        // Upstream, this host left 1 % unsent: judged; more: not, since a
        // Load Sender that fell behind may explain a Maximum below an
        // earlier faster Sub-Interval.
        let this_host = |unsent| LoadSender::ThisHost {
            sent: 100_000 - unsent,
            unsent,
        };
        assert_eq!(
            found(&cake, None, this_host(1_000)),
            [(0, 10, Some(6), None, None)]
        );
        assert!(found(&cake, None, this_host(1_001)).is_empty());
        // A Sub-Interval after the Maximum that met the criterion, even a
        // slower one: the search found the path's rate again. Sub-Interval
        // 9, 1913, is the Maximum then.
        let met_after = [&cake[..9], &[at(10, 19000, 0)]].concat();
        assert!(found(&met_after, None, LoadSender::Server).is_empty());
        // A correct Maximum, at the link's rate in the last Sub-Interval,
        // after a lossy Sub-Interval only 3 % faster, as a queue at its
        // rate gives: within the margin.
        let at_the_rate = [
            at(1, 5000, 0),
            at(2, 10000, 0),
            at(3, 25750, 1000),
            at(4, 24000, 600),
            at(5, 25000, 0),
        ];
        assert!(found(&at_the_rate, None, LoadSender::Server).is_empty());
        // Each Capacity Mode on its own: split after Sub-Interval 6, the
        // overshoot is after the first's Maximum, Sub-Interval 5, and the
        // second has nothing faster before its own.
        assert_eq!(
            found(&cake, Some(6), LoadSender::Server),
            [(0, 5, None, Some(6), None)]
        );
    }

    // : an Upstream Test's Load Sender is this host, with what it sent
    // and what its Skipped and Withheld Bursts left unsent over every Test
    // Connection; a Downstream Test's is the Server
    #[test]
    fn the_load_sender_counts_what_this_host_left_unsent() {
        let first = [
            DataEvent::Sent(SentInterval::new(
                1,
                90,
                90_000,
                Duration::from_millis(50),
                true,
            )),
            DataEvent::BurstsSkipped {
                bursts: 1,
                datagrams: 4,
                bytes: 4_000,
            },
        ];
        let second = [
            DataEvent::Sent(SentInterval::new(
                1,
                5,
                5_000,
                Duration::from_millis(50),
                true,
            )),
            DataEvent::BurstsWithheld {
                bursts: 1,
                datagrams: 1,
                bytes: 1_000,
            },
        ];
        let connections: [&[DataEvent]; 2] = [&first, &second];
        assert_eq!(
            LoadSender::of(Direction::Upstream, &connections),
            LoadSender::ThisHost {
                sent: 95,
                unsent: 5
            }
        );
        assert!(LoadSender::of(Direction::Upstream, &connections).fell_behind());
        assert_eq!(
            LoadSender::of(Direction::Downstream, &connections),
            LoadSender::Server
        );
        assert!(!LoadSender::Server.fell_behind());
    }

    //
    #[udpstp_req::req("RFC9097-6.6-3", "RFC9097-9-6")]
    #[test]
    fn a_split_makes_two_capacity_modes() {
        let measured = [at(1, 900, 0), at(2, 800, 0), at(4, 300, 0)];
        let modes = capacity_modes(4, &measured, PerformanceCriterion::DEFAULT, Some(2));
        let summary: Vec<_> = modes
            .iter()
            .map(|m| {
                (
                    m.sub_intervals.clone(),
                    m.maximum.map(|m| m.sub_int_seq_no),
                    m.missing,
                )
            })
            .collect();
        assert_eq!(summary, [(1..=2, Some(1), 0), (3..=4, Some(4), 1)]);
        // A split outside the Test Interval leaves one Capacity Mode.
        for split in [Some(0), Some(4), Some(9)] {
            assert_eq!(
                capacity_modes(4, &measured, PerformanceCriterion::DEFAULT, split).len(),
                1
            );
        }
    }

    fn st(seq: u32, ms: u64) -> SentInterval {
        SentInterval::new(seq, 10, 10_000, Duration::from_millis(ms), true)
    }

    // : per Sub-Interval, only whole ones; a partial one would read low
    #[test]
    fn sender_sums_need_every_st_of_the_sub_interval() {
        // Two 500 ms st per 1 s Sub-Interval; the third is missing.
        let sums = sent_by_sub_interval(
            2,
            Duration::from_secs(1),
            &[st(1, 500), st(2, 500), st(3, 500)],
            true,
        );
        assert_eq!(
            sums[0].map(|s| (s.datagrams, s.duration)),
            Some((20, Duration::from_secs(1)))
        );
        assert_eq!(sums[1], None);
    }

    // : a Sub-Interval that isn't a multiple of st still sums the st
    // starting in it, over the time they cover
    #[test]
    fn sender_sums_follow_time_when_st_does_not_divide_the_sub_interval() {
        // 300 ms st, 1 s Sub-Intervals: st 1-4 start in the first (0, 300,
        // 600, 900 ms), 5-7 in the second (1200, 1500, 1800 ms).
        let sent: Vec<SentInterval> = (1..=7).map(|seq| st(seq, 300)).collect();
        let sums = sent_by_sub_interval(2, Duration::from_secs(1), &sent, true);
        let got: Vec<_> = sums
            .iter()
            .map(|s| s.map(|s| (s.datagrams, s.duration)))
            .collect();
        assert_eq!(
            got,
            [
                Some((40, Duration::from_millis(1200))),
                Some((30, Duration::from_millis(900)))
            ]
        );
    }

    #[test]
    fn a_missing_sub_interval_is_none() {
        let m =
            |seq| SubIntervalMeasurement::new(seq, &stats(1, 1), 0, Duration::from_secs(1), true);
        let slots = by_sub_interval(4, &[m(1), m(2), m(2), m(3), m(9)]);
        let seqs: Vec<Option<u32>> = slots.iter().map(|s| s.map(|m| m.sub_int_seq_no)).collect();
        assert_eq!(seqs, [Some(1), Some(2), Some(3), None]);
        assert_eq!(
            expected_sub_intervals(Duration::from_secs(10), Duration::from_secs(1)),
            10
        );
    }

    // docs/spec.md stories 61 and 63 (Downstream Sub-Intervals summed on
    // one grid, each with its active count);
    #[test]
    fn test_connections_sum_by_sub_interval() {
        let first = vec![
            SubIntervalMeasurement {
                clock_delta_min: Some(12),
                rtt_minimum: Some(20),
                ..at(1, 100, 1)
            },
            at(2, 200, 0),
            // A second copy counts once.
            at(2, 999, 0),
        ];
        let second = vec![
            SubIntervalMeasurement {
                clock_delta_min: Some(7),
                rtt_minimum: Some(30),
                ..at(2, 300, 1)
            },
            at(3, 0, 0),
        ];
        let summed = aggregate(&[first.clone(), second.clone()], true);
        let seqs: Vec<(u32, u32, u8)> = summed
            .iter()
            .map(|m| (m.sub_int_seq_no, m.datagrams, m.active_test_connections))
            .collect();
        // Sub-Interval 3's only Test Connection received nothing.
        assert_eq!(seqs, [(1, 100, 1), (2, 500, 2), (3, 0, 0)]);
        let two = summed[1];
        assert_eq!(
            two.ip_layer_capacity,
            first[1].ip_layer_capacity + second[0].ip_layer_capacity
        );
        assert_eq!(
            (two.loss, two.clock_delta_min, two.rtt_minimum),
            (1, Some(7), Some(30))
        );
        // Several Servers' clocks can't be compared.
        let apart = aggregate(&[first, second], false);
        assert!(apart.iter().all(|m| m.clock_delta_min.is_none()));
    }

    // : the one-lost-Load-PDU allowance is one per active Test
    // Connection, so the sum is no harsher than its parts
    #[test]
    fn each_active_test_connection_may_lose_one() {
        let summed = aggregate(&[vec![at(1, 10, 1)], vec![at(1, 10, 1)]], true);
        assert_eq!(summed[0].loss, 2);
        assert!(PerformanceCriterion::DEFAULT.is_met_by(&summed[0]));
        let one = SubIntervalMeasurement {
            loss: 2,
            ..at(1, 20, 0)
        };
        assert!(!PerformanceCriterion::DEFAULT.is_met_by(&one));
    }

    /// A 4 s Test of 1 s Sub-Intervals over IPv4, judged as by default.
    const EVALUATION: Evaluation = Evaluation {
        test_interval: Duration::from_secs(4),
        sub_interval: Duration::from_secs(1),
        ip_version: IpVersion::V4,
        criterion: PerformanceCriterion::DEFAULT,
        first_mode: None,
    };

    /// st `seq`, 500 ms long, of `datagrams` Load PDUs of 1000 bytes.
    fn half_second(seq: u32, datagrams: u32) -> SentInterval {
        SentInterval::new(
            seq,
            datagrams,
            u64::from(datagrams) * 1000,
            Duration::from_millis(500),
            true,
        )
    }

    // : the IP-Layer Sender Bit Rate is the Load Sender's own count
    // where the Test Connection has one, and the Load Receiver's estimate
    // otherwise;  (a Test's measurement results at their interface)
    #[test]
    fn the_sender_bit_rate_is_counted_where_it_can_be_and_estimated_otherwise() {
        let counted: Vec<DataEvent> = (1..=8)
            .map(|seq| DataEvent::Sent(half_second(seq, 10)))
            .collect();
        let estimated: Vec<DataEvent> = (1..=8)
            .map(|seq| DataEvent::SentEstimated(half_second(seq, 30)))
            .collect();
        let both: Vec<DataEvent> = counted.iter().chain(&estimated).copied().collect();
        // Two st of a Sub-Interval each.
        let results = MeasurementResults::of_test_connection(&both, &EVALUATION);
        assert!(!results.sender_estimated);
        assert_eq!(
            results.sender_bit_rates[0].map(|s| (s.datagrams, s.duration)),
            Some((20, Duration::from_secs(1)))
        );
        let results = MeasurementResults::of_test_connection(&estimated, &EVALUATION);
        assert!(results.sender_estimated);
        assert_eq!(results.sender_bit_rates[3].map(|s| s.datagrams), Some(60));
        let nothing = MeasurementResults::of_test_connection(&[], &EVALUATION);
        assert!(!nothing.sender_estimated);
        assert_eq!(nothing.sender_bit_rates, [None; 4]);
        // Nothing measured: no Sub-Interval, and no maximum.
        assert_eq!(nothing.sub_intervals, [None; 4]);
        assert!(nothing.lacks_a_maximum());
    }

    // : the results take Sub-Intervals as a Data Phase reports them,
    // each at most once and within the Test Interval, and debug builds
    // check it
    #[test]
    fn sub_intervals_come_once_each_within_the_test_interval() {
        let reported = |seqs: &[u32]| -> Vec<DataEvent> {
            seqs.iter()
                .map(|&seq| DataEvent::SubIntervalMeasured(at(seq, 1, 0)))
                .collect()
        };
        // Four Sub-Intervals; one may be missing.
        assert!(reported_as_a_data_phase_does(
            &reported(&[1, 2, 4]),
            &EVALUATION
        ));
        for wrong in [&[1, 1][..], &[0], &[5], &[2, 1, 2]] {
            assert!(
                !reported_as_a_data_phase_does(&reported(wrong), &EVALUATION),
                "{wrong:?}"
            );
        }
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "each Sub-Interval once")]
    fn a_sub_interval_reported_twice_fails_a_debug_build() {
        let twice = [
            DataEvent::SubIntervalMeasured(at(1, 100, 0)),
            DataEvent::SubIntervalMeasured(at(1, 100, 0)),
        ];
        MeasurementResults::of_test_connection(&twice, &EVALUATION);
    }

    // docs/spec.md stories 61 to 63; , : a Multi-Connection Test's
    // Sub-Intervals and st are summed by number; Upstream, the Test
    // Connections' Sub-Intervals lie apart, by up to the largest offset
    #[test]
    fn a_multi_connection_tests_results_sum_its_test_connections() {
        let measured_at = |seq: u32, datagrams: u32, ms: u64| {
            DataEvent::SubIntervalMeasured(SubIntervalMeasurement {
                measured_at: MonoTime::from_duration(Duration::from_millis(ms)),
                ..at(seq, datagrams, 0)
            })
        };
        let first = [
            measured_at(1, 100, 1000),
            measured_at(2, 100, 2000),
            DataEvent::Sent(half_second(1, 10)),
            DataEvent::Sent(half_second(2, 10)),
        ];
        let second = [
            measured_at(1, 50, 1300),
            measured_at(2, 60, 2250),
            DataEvent::Sent(half_second(1, 5)),
            DataEvent::Sent(half_second(2, 5)),
        ];
        let each: [&[DataEvent]; 2] = [&first, &second];
        let downstream =
            MeasurementResults::of_test(&each, Direction::Downstream, true, &EVALUATION);
        let sums: Vec<Option<(u32, u8)>> = downstream
            .sub_intervals
            .iter()
            .map(|m| m.map(|m| (m.datagrams, m.active_test_connections)))
            .collect();
        assert_eq!(sums, [Some((150, 2)), Some((160, 2)), None, None]);
        assert_eq!(
            downstream.sender_bit_rates[0].map(|s| s.datagrams),
            Some(30)
        );
        assert_eq!(downstream.approximate, None);
        // The highest of the two, meeting the criterion.
        assert_eq!(
            downstream.capacity_modes[0]
                .maximum
                .map(|m| m.sub_int_seq_no),
            Some(2)
        );
        assert!(!downstream.lacks_a_maximum());
        let upstream = MeasurementResults::of_test(&each, Direction::Upstream, true, &EVALUATION);
        assert_eq!(upstream.sub_intervals, downstream.sub_intervals);
        assert_eq!(upstream.approximate, Some(Duration::from_millis(300)));
    }
}
