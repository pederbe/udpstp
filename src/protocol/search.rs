//! A Search Test's searches, as its Client judges them: whether one
//! found the path's limit, and if none did, why each fell short of it, so
//! the results can say they may understate the path. Below the 1 Gbps row
//! a search that rises past the limit confirms Congestion at its first
//! overshoot, Fast Mode's jump bringing slowAdjThresh Errored Intervals
//! back to back. Above it the search steps a row at a time, and through a
//! shallow queue its Errored Intervals can come one or two at a time, too
//! few back to back to confirm it: such a search met the limit when its
//! last Trial Intervals were often errored (`met_the_limit_at_its_end`).
//! The Client knows no sending rate in a Search Test: the Server's Load
//! Rate Adjustment Algorithm picks each (RFC 9946 §8.1), and a Client's
//! Status PDUs carry none (§8.2).
//!
//! Each Test Connection's search is judged from its Data Phase's events:
//! its Client's count of its Trial Intervals
//! ([`DataEvent::TrialIntervalsCounted`]) and the IP-Layer Sender Bit Rate
//! over each Sub-Interval, which the Client measures as Load Sender and
//! estimates as Load Receiver. It is tested at its interface, on
//! hand-built events, and in simulated Tests (`congestion_never_confirmed.rs`).
//!
//! A Fixed-Rate Test's Client knows its sending rate, the rows its
//! Activation Responses set, and judges, by the same margin, whether the
//! Server kept it where the result was measured ([`FixedRateTest`]).

use crate::protocol::coordinator::max_bandwidth_share;
use crate::protocol::results::{
    Evaluation, MeasurementResults, PerformanceCriterion, header_bytes,
};
use crate::protocol::sending_rate_table::{
    LAST_ROW, highest_rate_within, least_sending_rate, top_rate_with_packets,
};
use crate::protocol::{DataEvent, IpVersion, SentInterval, TestParameters, TrialIntervalCounts};

/// How far below a rate the IP-Layer Sender Bit Rate may be and still be
/// taken to be at it, in percent: more than the error of a Load Receiver's
/// estimate, a few Load PDUs across the edges of an st, and than a
/// Late Burst's delay.
pub const SENDING_RATE_MARGIN_PERCENT: u64 = 5;

/// Whether `measured`, an IP-Layer Sender Bit Rate, is at `rate`: at most
/// [`SENDING_RATE_MARGIN_PERCENT`] below it. A search stopped at a rate so,
/// and a Fixed-Rate Test's Server kept its sending rate so.
fn at_rate(measured: u64, rate: u64) -> bool {
    u128::from(measured) * 100 >= u128::from(rate) * u128::from(100 - SENDING_RATE_MARGIN_PERCENT)
}

/// How much more than the margin an IPv6 Test's IP-Layer Sender Bit Rate
/// may be above a maxBandwidth and still be at it, in percent: a row's rate
/// is its IPv4 packets' (ADR 0004), and over IPv6 it may send up to 1.6%
/// more, 20 bytes of header a 1250-byte packet.
/// (conformance: search-capped-at-stated-max-bandwidth)
const IPV6_HEADER_ALLOWANCE_PERCENT: u64 = 2;

/// The fewest Errored Intervals among a search's last Trial Intervals with
/// which it met the path's limit at its end, so a short cluster of them
/// doesn't.
const LAST_ERRORED_AT_LEAST: u32 = 4;

/// And the share of those last Trial Intervals, from the first Errored one
/// among them on, that must be errored, at least one in this many: half of
/// what a search alternating around the limit shows, each Errored Interval
/// a row down and each Clean one a row up.
const LAST_ERRORED_ONE_IN: u32 = 4;

/// Whether a search met the path's limit at its end: of its last
/// [`LAST_TRIAL_INTERVALS`](crate::protocol::LAST_TRIAL_INTERVALS) Trial Intervals,
/// from the first Errored Interval among them on, at least
/// [`LAST_ERRORED_AT_LEAST`] were Errored Intervals and at least one in
/// [`LAST_ERRORED_ONE_IN`], counting Hold Intervals of both kinds among
/// them: a stalling Load Sender's Trial Intervals, in which no Load PDU
/// arrived, don't make its few Errored ones more (the maintainer's
/// decision on , 2026-09-29).
fn met_the_limit_at_its_end(counts: &TrialIntervalCounts) -> bool {
    counts.last_errored >= LAST_ERRORED_AT_LEAST
        && u64::from(counts.last_errored) * u64::from(LAST_ERRORED_ONE_IN)
            >= u64::from(counts.last_from_first_errored)
}

/// Why a search that never found the path's limit, neither confirming
/// Congestion nor meeting it at its end, fell short of it: the first of
/// these that fits, in this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchShortfall {
    /// Its IP-Layer Sender Bit Rate was at the maxBandwidth its Test
    /// Connection stated, this many Mbps: within the margin below the
    /// highest row at or below it, where a udpstp Server's search stops,
    /// and within the margin above it, allowing for IPv6's longer headers.
    /// An OB-UDPST Server stops there only in a RATE_LIMITING build run with
    /// -B. (conformance: search-capped-at-stated-max-bandwidth)
    MaxBandwidth(u16),
    /// It reached the top of the Sending Rate Table: at least the least
    /// rate any Server offers there with the Test's Setup modifiers, and
    /// more where the Load PDUs sent at its peak were larger, which a
    /// Server's MTU decides: 12 Gbps at 1500 bytes, up to 72 with
    /// 9000-byte Jumbo packets.
    TableTop,
    /// More than half its Trial Intervals were Hold Intervals with a delay
    /// sample, on which the algorithm changes nothing: delay variation
    /// between lowThresh and upperThresh held it. The Client judges it by
    /// the maximum delay variation, as a udpstp Server does by default; an
    /// OB-UDPST Server classifies by the average, which is lower, so it may
    /// have seen Clean Intervals and searched on where the Client counted
    /// Hold Intervals (conformance: delay-var-is-the-maximum-by-default).
    /// Hold Intervals with no delay sample, in which no Load PDU arrived in
    /// order, as when the Load Sender stalls or the path drops everything
    /// for a while, fit no cause of their own: the later causes judge such
    /// a search by its IP-Layer Sender Bit Rate.
    DelayVariation,
    /// It was still rising in the last Sub-Interval measured, by more than
    /// Fast Mode adds with half its Trial Intervals clean: the Test Interval
    /// ended first.
    StillRising,
    /// It stopped rising though few of its last Trial Intervals were
    /// errored: the Load Sender may not have kept up with the rate the
    /// algorithm set, as a host that skips or withholds Bursts, whose Load
    /// PDUs take no lpduSeqNo, doesn't.
    LoadSender,
}

/// A Search Test as its Client ran it: what its searches are judged by.
#[derive(Debug, Clone, Copy)]
pub struct SearchTest {
    /// Its parameters, as the Activation Responses gave them.
    pub parameters: TestParameters,
    /// Its IP version, whose headers the IP-Layer Sender Bit Rate counts.
    pub ip_version: IpVersion,
    /// Its maxBandwidth in Mbps, 0 for none, and how many Test Connections
    /// it attempted, which each stated an equal share of it, rounded down.
    pub max_bandwidth: u16,
    pub test_connections: u8,
    /// CHSR_JUMBO_STATUS and CHSR_TRADITIONAL_MTU, which shape the top of
    /// the Sending Rate Table.
    pub jumbo: bool,
    pub traditional_mtu: bool,
}

impl SearchTest {
    /// Why each of its Test Connections' searches fell short of the path's
    /// limit, given each one's Data Phase events, in the same order, when
    /// none found it. None when one did, by confirming Congestion or by
    /// meeting the limit at its end (`met_the_limit_at_its_end`); nor when
    /// one's Trial Intervals weren't counted, as for a Data Phase that
    /// never ended, since then it can't be told.
    pub fn shortfalls(&self, test_connections: &[&[DataEvent]]) -> Vec<SearchShortfall> {
        let counted: Option<Vec<TrialIntervalCounts>> = test_connections
            .iter()
            .map(|events| counts(events))
            .collect();
        let Some(counted) = counted else {
            return Vec::new();
        };
        if counted
            .iter()
            .any(|c| c.congestion_confirmed || met_the_limit_at_its_end(c))
        {
            return Vec::new();
        }
        test_connections
            .iter()
            .zip(counted)
            .map(|(events, counts)| self.shortfall(events, counts))
            .collect()
    }

    /// Why one Test Connection's search, which never confirmed Congestion,
    /// fell short.
    fn shortfall(&self, events: &[DataEvent], counts: TrialIntervalCounts) -> SearchShortfall {
        let sent = self.sent(events);
        let rates: Vec<u64> = sent.iter().map(|s| s.ip_layer_sender_bit_rate).collect();
        let at_peak = sent.iter().max_by_key(|s| s.ip_layer_sender_bit_rate);
        let peak = at_peak.map_or(0, |s| s.ip_layer_sender_bit_rate);
        // The peak is at `rate`, within the margin.
        let at = |rate: u64| rate > 0 && at_rate(peak, rate);
        let share = max_bandwidth_share(self.max_bandwidth, self.test_connections);
        let cap = u64::from(share) * 1_000_000;
        // Where a udpstp Server's search stops: the highest row at or below
        // the share, or the share if the table tops out within it.
        let stops_at =
            highest_rate_within(self.jumbo, self.traditional_mtu, u32::from(share)).unwrap_or(cap);
        let over = match self.ip_version {
            IpVersion::V4 => SENDING_RATE_MARGIN_PERCENT,
            IpVersion::V6 => SENDING_RATE_MARGIN_PERCENT + IPV6_HEADER_ALLOWANCE_PERCENT,
        };
        // Not above it either: a Server need not keep to it.
        let within = u128::from(peak) * 100 <= u128::from(cap) * u128::from(100 + over);
        if share > 0 && at(stops_at) && within {
            return SearchShortfall::MaxBandwidth(share);
        }
        if self.table_top(at_peak).is_some_and(at) {
            return SearchShortfall::TableTop;
        }
        // Hold Intervals with no delay sample saw no delay variation.
        let varied = counts.hold.saturating_sub(counts.hold_without_sample);
        if u64::from(varied) * 2 > u64::from(counts.trial_intervals()) {
            return SearchShortfall::DelayVariation;
        }
        if let [earlier @ .., last] = rates.as_slice()
            && earlier
                .iter()
                .max()
                .is_some_and(|&before| *last > before.saturating_add(self.fast_mode_rise()))
        {
            return SearchShortfall::StillRising;
        }
        SearchShortfall::LoadSender
    }

    /// What the Load Sender sent over each Sub-Interval measured, in order:
    /// its IP-Layer Sender Bit Rate and Load PDUs.
    fn sent(&self, events: &[DataEvent]) -> Vec<SentInterval> {
        let evaluation = Evaluation {
            test_interval: self.parameters.test_interval,
            sub_interval: self.parameters.sub_interval,
            ip_version: self.ip_version,
            // Neither changes the IP-Layer Sender Bit Rate.
            criterion: PerformanceCriterion::DEFAULT,
            first_mode: None,
        };
        MeasurementResults::of_test_connection(events, &evaluation)
            .sender_bit_rates
            .into_iter()
            .flatten()
            .collect()
    }

    /// The top rate of the Sending Rate Table the Load Sender's rows came
    /// from, as far as the Client can tell: the least any Server offers
    /// with the Test's Setup modifiers, or more where the Load PDUs sent at
    /// the peak show a Server whose MTU fits its fastest rows to larger
    /// ones, up to 9000-byte Jumbo packets.
    fn table_top(&self, at_peak: Option<&SentInterval>) -> Option<u64> {
        let least = least_sending_rate(self.jumbo, self.traditional_mtu, LAST_ROW)?;
        let fitted = at_peak.filter(|s| s.datagrams > 0).map_or(0, |s| {
            let packet =
                s.bytes / u64::from(s.datagrams) + header_bytes(self.ip_version == IpVersion::V4);
            top_rate_with_packets(packet)
        });
        Some(least.max(fitted))
    }

    /// The least a search in Fast Mode raises the IP-Layer Sender Bit Rate
    /// over a Sub-Interval with at least half its Trial Intervals clean, in
    /// bit/s: highSpeedDelta rows a Clean Interval, rows being at least
    /// 1 Mbps apart (row N is N Mbps up to row 1000, and the rows above are
    /// further apart). Algorithm C's doubling adds more once its row is
    /// twice highSpeedDelta. A Load Sender that can't keep up, held to a
    /// packet rate, steps up only by what the larger Load PDUs of the rows
    /// above 1 Gbps carry, once, as its row passes them: a fifth more from
    /// 1250 to 1500 bytes at a Server's default MTU, more with Jumbo
    /// packets. From about 500 Mbps that step is more than this rise, so a
    /// short Test whose last Sub-Interval takes it reads as still rising.
    fn fast_mode_rise(&self) -> u64 {
        let trial = self.parameters.trial_interval.as_nanos().max(1);
        let trials = self.parameters.sub_interval.as_nanos() / trial;
        let rise = u128::from(self.parameters.high_speed_delta) * 1_000_000 * trials / 2;
        u64::try_from(rise).unwrap_or(u64::MAX)
    }
}

/// A Fixed-Rate Test as its Client ran it: what its Server's sending is
/// judged by.
#[derive(Debug, Clone, Copy)]
pub struct FixedRateTest {
    /// CHSR_JUMBO_STATUS and CHSR_TRADITIONAL_MTU, which set its rows'
    /// sending rates.
    pub jumbo: bool,
    pub traditional_mtu: bool,
}

/// A Fixed-Rate Test whose Server sent more than the margin below its
/// sending rate where the result was measured, as its Client estimated
/// what it sent: it may not have kept up, as an overloaded host skips or
/// withholds Bursts, whose Load PDUs take no lpduSeqNo, and the result may
/// understate the path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendingRateNotKept {
    /// The lowest IP-Layer Sender Bit Rate estimated in the Sub-Interval of
    /// a Maximum IP-Layer Capacity that fell short, in bit/s.
    pub estimated_bps: u64,
    /// The sending rate, its Test Connections' rows' summed, in bit/s.
    pub sending_bps: u64,
    /// How many Capacity Modes' Maximum IP-Layer Capacity fell short.
    pub modes: u64,
}

impl FixedRateTest {
    /// Whether its Server sent more than the margin below its sending rate,
    /// the sum of its Test Connections' `rows`, in the Sub-Interval of a
    /// Maximum IP-Layer Capacity of its `results`, as the Client estimated
    /// what it sent. The least any Server offers for each row
    /// counts, so a larger Server MTU never makes a false finding. None
    /// where the Client didn't estimate it, sending the Load PDUs itself in
    /// an Upstream Test, or where a row isn't one of the Sending Rate
    /// Table's.
    pub fn sending_rate_not_kept(
        &self,
        rows: &[u16],
        results: &MeasurementResults,
    ) -> Option<SendingRateNotKept> {
        if !results.sender_estimated {
            return None;
        }
        let sending: u64 = rows
            .iter()
            .map(|&row| least_sending_rate(self.jumbo, self.traditional_mtu, row))
            .sum::<Option<u64>>()?;
        let short: Vec<u64> = results
            .capacity_modes
            .iter()
            .filter_map(|mode| mode.maximum)
            .filter_map(|m| {
                let at = usize::try_from(m.sub_int_seq_no).ok()?.checked_sub(1)?;
                results.sender_bit_rates.get(at).copied().flatten()
            })
            .map(|sent| sent.ip_layer_sender_bit_rate)
            .filter(|&estimated| !at_rate(estimated, sending))
            .collect();
        Some(SendingRateNotKept {
            estimated_bps: short.iter().copied().min()?,
            sending_bps: sending,
            modes: short.len() as u64,
        })
    }
}

/// A Test Connection's Trial Interval counts, if its Client reported them.
fn counts(events: &[DataEvent]) -> Option<TrialIntervalCounts> {
    events.iter().find_map(|event| match event {
        DataEvent::TrialIntervalsCounted(counts) => Some(*counts),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::wire::status::{STATUS_NODEL, SubIntervalStats};

    use super::*;
    use crate::protocol::{SentInterval, SubIntervalMeasurement};

    /// A 5 s Test of 1 s Sub-Intervals and 50 ms Trial Intervals, with the
    /// default thresholds, over IPv4, searching with algorithm B from the
    /// Server's default row, with no maxBandwidth and Jumbo rows.
    fn test() -> SearchTest {
        SearchTest {
            parameters: TestParameters {
                test_interval: Duration::from_secs(5),
                sub_interval: Duration::from_secs(1),
                trial_interval: Duration::from_millis(50),
                low_thresh: 30,
                upper_thresh: 90,
                seq_err_thresh: 10,
                slow_adj_thresh: 3,
                high_speed_delta: 10,
                ignore_ooo_dup: true,
                one_way_delay_variation: true,
                random_payload: false,
                rate_adj_algo: 0,
                dscp: 0,
                sr_index_conf: u16::MAX,
                sr_index_is_start: false,
            },
            ip_version: IpVersion::V4,
            max_bandwidth: 0,
            test_connections: 1,
            jumbo: true,
            traditional_mtu: false,
        }
    }

    /// A Load Receiver's events: each Sub-Interval's 20 st at its rate in
    /// Mbps, the Server's as the Client estimates them, in 1250-byte IPv4
    /// packets, then the counts.
    fn events(mbps: &[u64], counts: TrialIntervalCounts) -> Vec<DataEvent> {
        sized(mbps, 1250, IpVersion::V4, counts)
    }

    /// The same in IP packets of `packet` bytes over `ip_version`, which
    /// must make each rate a whole number of packets an st.
    fn sized(
        mbps: &[u64],
        packet: u64,
        ip_version: IpVersion,
        counts: TrialIntervalCounts,
    ) -> Vec<DataEvent> {
        let header = if ip_version == IpVersion::V4 { 28 } else { 48 };
        let mut events: Vec<DataEvent> = mbps
            .iter()
            .enumerate()
            .flat_map(|(i, &rate)| {
                // 50 ms at `rate` Mbps: rate × 6250 IP bytes.
                assert_eq!(
                    rate * 6250 % packet,
                    0,
                    "{rate} Mbps in {packet}-byte packets"
                );
                let datagrams = (rate * 6250 / packet) as u32;
                (1..=20).map(move |st| {
                    DataEvent::SentEstimated(SentInterval {
                        st_seq_no: i as u32 * 20 + st,
                        datagrams,
                        bytes: u64::from(datagrams) * (packet - header),
                        ip_layer_sender_bit_rate: rate * 1_000_000,
                        duration: Duration::from_millis(50),
                    })
                })
            })
            .collect();
        events.push(DataEvent::TrialIntervalsCounted(counts));
        events
    }

    /// Counts of 100 Trial Intervals, `hold` of them Hold Intervals, each
    /// with a delay sample, and the rest clean, none errored.
    fn clean_but(hold: u32) -> TrialIntervalCounts {
        TrialIntervalCounts {
            clean: 100 - hold,
            errored: 0,
            hold,
            hold_without_sample: 0,
            congestion_confirmed: false,
            last_errored: 0,
            last_from_first_errored: 0,
        }
    }

    const FLAT: [u64; 5] = [80, 280, 330, 330, 330];

    /// Counts of 100 Trial Intervals, `errored` of them errored among the
    /// last ones, `from_first` from the first of those on, and the rest
    /// clean.
    fn ending(errored: u32, from_first: u32) -> TrialIntervalCounts {
        TrialIntervalCounts {
            clean: 100 - errored,
            errored,
            last_errored: errored,
            last_from_first_errored: from_first,
            ..clean_but(0)
        }
    }

    // : a search that never confirmed Congestion met the path's limit
    // at its end when, of its last 64 Trial Intervals, from the first
    // Errored Interval among them, at least 4 were Errored Intervals and at
    // least 1 in 4 of those Trial Intervals, Hold Intervals of both kinds
    // counted among them (the maintainer's decision on , 2026-09-29):
    // as through a shallow queue above the 1 Gbps row, where the search
    // steps a row at a time and its Errored Intervals can come too few back
    // to back to confirm Congestion. Then, as for Congestion, no warning,
    // whatever else fits
    // (conformance: shallow-queue-search-alternates-at-the-bottleneck)
    #[test]
    fn a_search_whose_last_trial_intervals_were_often_errored_met_the_limit() {
        let at = |counts| test().shortfalls(&[&events(&FLAT, counts)]);
        assert_eq!(at(ending(4, 16)), []);
        assert_eq!(at(ending(32, 64)), []);
        // Fewer than 1 in 4, or fewer than 4.
        assert_eq!(at(ending(4, 17)), [SearchShortfall::LoadSender]);
        assert_eq!(at(ending(3, 3)), [SearchShortfall::LoadSender]);
        assert_eq!(at(ending(0, 0)), [SearchShortfall::LoadSender]);
        // Hold Intervals count among them, with a delay sample or not: 8
        // Errored among 40 Trial Intervals, 20 of them Hold Intervals, are
        // fewer than 1 in 4.
        let held = |without| TrialIntervalCounts {
            clean: 72,
            hold: 20,
            hold_without_sample: without,
            ..ending(8, 40)
        };
        assert_eq!(at(held(0)), [SearchShortfall::LoadSender]);
        assert_eq!(at(held(20)), [SearchShortfall::LoadSender]);
        assert_eq!(at(ending(8, 32)), []);
        // One Test Connection's is enough, as one's Congestion is.
        let two = SearchTest {
            test_connections: 2,
            ..test()
        };
        let busy = events(&FLAT, clean_but(0));
        let met = events(&FLAT, ending(4, 16));
        assert_eq!(two.shortfalls(&[&busy, &met]), []);
        // Whatever else fits: here the maxBandwidth.
        let capped = SearchTest {
            max_bandwidth: 330,
            ..test()
        };
        assert_eq!(capped.shortfalls(&[&met]), []);
        assert_eq!(
            capped.shortfalls(&[&busy]),
            [SearchShortfall::MaxBandwidth(330)]
        );
    }

    // : the warning's trigger is that no Test Connection's search found
    // the path's limit (the maintainer's decisions, 2026-09-29): one that
    // confirmed Congestion found it, whatever the others did, as one that
    // met it at its end does (below); and one whose Trial Intervals weren't
    // counted can't be judged
    #[test]
    fn one_confirmed_search_is_enough() {
        let confirmed = TrialIntervalCounts {
            congestion_confirmed: true,
            ..clean_but(0)
        };
        let busy = events(&FLAT, clean_but(0));
        let congested = events(&FLAT, confirmed);
        let test = SearchTest {
            test_connections: 2,
            ..test()
        };
        assert_eq!(test.shortfalls(&[&congested]), []);
        assert_eq!(test.shortfalls(&[&busy, &congested]), []);
        assert_eq!(
            test.shortfalls(&[&busy, &busy]),
            [SearchShortfall::LoadSender, SearchShortfall::LoadSender]
        );
        let uncounted: Vec<DataEvent> = busy
            .iter()
            .copied()
            .filter(|e| !matches!(e, DataEvent::TrialIntervalsCounted(_)))
            .collect();
        assert_eq!(test.shortfalls(&[&busy, &uncounted]), []);
        assert_eq!(test.shortfalls(&[]), []);
    }

    // : at a maxBandwidth of 200 Mbps the Server searches no higher
    // than row 200; within 5% of it is at it. A Multi-Connection
    // Test's Test Connections each stated an equal share, rounded down
    // (conformance: max-bandwidth-split-across-test-connections)
    #[test]
    fn a_search_at_its_max_bandwidth_is_capped() {
        let capped = SearchTest {
            max_bandwidth: 200,
            ..test()
        };
        let at = |mbps: u64| events(&[100, mbps, mbps, mbps, mbps], clean_but(0));
        assert_eq!(
            capped.shortfalls(&[&at(200)]),
            [SearchShortfall::MaxBandwidth(200)]
        );
        assert_eq!(
            capped.shortfalls(&[&at(190)]),
            [SearchShortfall::MaxBandwidth(200)]
        );
        assert_eq!(
            capped.shortfalls(&[&at(189)]),
            [SearchShortfall::LoadSender]
        );
        // 401 Mbps over two: 200 each.
        let shared = SearchTest {
            max_bandwidth: 401,
            test_connections: 2,
            ..test()
        };
        assert_eq!(
            shared.shortfalls(&[&at(200), &at(199)]),
            [SearchShortfall::MaxBandwidth(200); 2]
        );
        // Before the jitter that also held it.
        let held = events(&[100, 200, 200, 200, 200], clean_but(90));
        assert_eq!(
            capped.shortfalls(&[&held]),
            [SearchShortfall::MaxBandwidth(200)]
        );
    }

    // : a udpstp Server stops its search at the highest row at or below
    // the maxBandwidth, and above 1 Gbps its rows are 100 Mbps apart
    // up to 7.2 Gbps and 720 Mbps apart beyond with Jumbo rows, 120 Mbps
    // with Traditional MTU: a share between two rows is named where
    // the search stopped at the lower one's rate. 1190 stops at 1100, 8500
    // at 7920, 10000 at 9360, 1300 at 1200 with Traditional MTU; a peak
    // above the share by more than the margin still isn't the share's
    // (conformance: search-capped-at-stated-max-bandwidth)
    #[test]
    fn a_max_bandwidth_between_rows_is_named_at_the_row_below_it() {
        // The share in Mbps, where the search stops, the Load PDUs' IP size
        // there, and whether the table has Jumbo rows.
        let cases = [
            (1190, 1100, 1375, true),
            (8500, 7920, 1500, true),
            (10_000, 9360, 1500, true),
            (1300, 1200, 1500, false),
        ];
        for (share, stops_at, packet, jumbo) in cases {
            let test = SearchTest {
                max_bandwidth: share,
                jumbo,
                traditional_mtu: !jumbo,
                ..test()
            };
            let rates = [stops_at; 5];
            let stopped = sized(&rates, packet, IpVersion::V4, clean_but(0));
            assert_eq!(
                test.shortfalls(&[&stopped]),
                [SearchShortfall::MaxBandwidth(share)],
                "{share} Mbps"
            );
        }
        // Above 1190 and 10000 by more than 5%, in the Load PDUs of the
        // rows there: not at the share.
        let above = |share: u16, mbps: u64, packet: u64| {
            let test = SearchTest {
                max_bandwidth: share,
                ..test()
            };
            test.shortfalls(&[&sized(&[mbps; 5], packet, IpVersion::V4, clean_but(0))])
        };
        assert_eq!(above(1190, 1250, 1250), [SearchShortfall::LoadSender]);
        assert_eq!(above(10_000, 10_560, 1500), [SearchShortfall::LoadSender]);
        // A share above the top of a table without Jumbo rows, 10 Gbps,
        // stops nothing: the search at the top reached the table's.
        let no_jumbo = SearchTest {
            max_bandwidth: 20_000,
            jumbo: false,
            ..test()
        };
        let top = [1000, 10_000, 10_000, 10_000, 10_000];
        assert_eq!(
            no_jumbo.shortfalls(&[&sized(&top, 1250, IpVersion::V4, clean_but(0))]),
            [SearchShortfall::TableTop]
        );
    }

    // : the maxBandwidth is named only where the peak is at it, within
    // the margin either way. A udpstp Server keeps its search to it, but an
    // OB-UDPST Server only in a RATE_LIMITING build run with -B, so a search
    // well above it fell short for another reason. A row's rate is its IPv4
    // packets' (ADR 0004), and over IPv6 it may send up to 1.6% more, 20
    // bytes a 1250-byte packet: 2% more is allowed there
    // (conformance: search-capped-at-stated-max-bandwidth)
    #[test]
    fn a_search_above_its_max_bandwidth_was_not_capped_by_it() {
        let capped = SearchTest {
            max_bandwidth: 200,
            ..test()
        };
        let at = |mbps: u64, ip_version| {
            let rates = [100, mbps, mbps, mbps, mbps];
            sized(&rates, 1250, ip_version, clean_but(0))
        };
        assert_eq!(
            capped.shortfalls(&[&at(210, IpVersion::V4)]),
            [SearchShortfall::MaxBandwidth(200)]
        );
        assert_eq!(
            capped.shortfalls(&[&at(211, IpVersion::V4)]),
            [SearchShortfall::LoadSender]
        );
        // An OB-UDPST Server that doesn't keep to it, on a path twice as
        // fast.
        assert_eq!(
            capped.shortfalls(&[&at(400, IpVersion::V4)]),
            [SearchShortfall::LoadSender]
        );
        let v6 = SearchTest {
            ip_version: IpVersion::V6,
            ..capped
        };
        assert_eq!(
            v6.shortfalls(&[&at(214, IpVersion::V6)]),
            [SearchShortfall::MaxBandwidth(200)]
        );
        assert_eq!(
            v6.shortfalls(&[&at(215, IpVersion::V6)]),
            [SearchShortfall::LoadSender]
        );
    }

    // : the least top rate of the Sending Rate Table is 10 Gbps with
    // Jumbo rows (bursts of 100 1250-byte packets where the Server's MTU
    // is small) and 12 Gbps with Traditional MTU (bursts of 100 1500-byte
    // packets); within 5% of it is at it
    #[test]
    fn a_search_at_the_top_of_the_table_is_named() {
        let top = |mbps: u64| events(&[1000, 5000, mbps, mbps, mbps], clean_but(0));
        assert_eq!(
            test().shortfalls(&[&top(9_500)]),
            [SearchShortfall::TableTop]
        );
        assert_eq!(
            test().shortfalls(&[&top(9_499)]),
            [SearchShortfall::LoadSender]
        );
        let traditional = SearchTest {
            jumbo: false,
            traditional_mtu: true,
            ..test()
        };
        let top = |mbps: u64| {
            let rates = [1008, 5004, mbps, mbps, mbps];
            sized(&rates, 1500, IpVersion::V4, clean_but(0))
        };
        assert_eq!(
            traditional.shortfalls(&[&top(9_504)]),
            [SearchShortfall::LoadSender]
        );
        assert_eq!(
            traditional.shortfalls(&[&top(11_400)]),
            [SearchShortfall::TableTop]
        );
    }

    // : a Server's MTU decides how large the Load PDUs of its rows
    // above 1 Gbps grow, and so where its table tops out: a burst of 100
    // of them every 100 µs, 12 Gbps at the default 1500 bytes, 72 with
    // 9000-byte Jumbo packets. The Load PDUs sent at the peak show
    // it, so a Load Sender at 9.6 Gbps behind a Server whose table goes to
    // 72 Gbps didn't reach its top
    #[test]
    fn the_top_of_the_table_follows_the_load_pdus_sent() {
        let top = |mbps: u64, packet: u64| {
            let rates = [1008, 5004, mbps, mbps, mbps];
            sized(&rates, packet, IpVersion::V4, clean_but(0))
        };
        assert_eq!(
            test().shortfalls(&[&top(9_648, 9000)]),
            [SearchShortfall::LoadSender]
        );
        assert_eq!(
            test().shortfalls(&[&top(68_400, 9000)]),
            [SearchShortfall::TableTop]
        );
        assert_eq!(
            test().shortfalls(&[&top(11_394, 1500)]),
            [SearchShortfall::LoadSender]
        );
        assert_eq!(
            test().shortfalls(&[&top(11_400, 1500)]),
            [SearchShortfall::TableTop]
        );
        // The same Load PDUs over IPv6, 20 bytes of payload fewer.
        let v6 = SearchTest {
            ip_version: IpVersion::V6,
            ..test()
        };
        let top_v6 = sized(
            &[1008, 5004, 9_648, 9_648, 9_648],
            9000,
            IpVersion::V6,
            clean_but(0),
        );
        assert_eq!(v6.shortfalls(&[&top_v6]), [SearchShortfall::LoadSender]);
    }

    // : a maxBandwidth of 10 Gbps stops a search at the least top rate
    // of the Sending Rate Table: both fit, and the maxBandwidth, which comes
    // first, is named, as it is what the Client user can change
    #[test]
    fn a_max_bandwidth_at_the_top_of_the_table_is_named_first() {
        let at_top = events(&[1000, 5000, 10_000, 10_000, 10_000], clean_but(0));
        let capped = SearchTest {
            max_bandwidth: 10_000,
            ..test()
        };
        assert_eq!(
            capped.shortfalls(&[&at_top]),
            [SearchShortfall::MaxBandwidth(10_000)]
        );
        assert_eq!(test().shortfalls(&[&at_top]), [SearchShortfall::TableTop]);
        // 9.5 Gbps is both too.
        let high = SearchTest {
            max_bandwidth: 9_500,
            ..test()
        };
        assert_eq!(
            high.shortfalls(&[&events(&[1000, 5000, 9_500, 9_500, 9_500], clean_but(0))]),
            [SearchShortfall::MaxBandwidth(9_500)]
        );
    }

    // : a Hold Interval changes nothing, so a search mostly in them was
    // held by delay variation: more than half, not half
    #[test]
    fn a_search_mostly_in_hold_intervals_was_held_by_delay_variation() {
        let low = [20, 30, 30, 30, 30];
        assert_eq!(
            test().shortfalls(&[&events(&low, clean_but(51))]),
            [SearchShortfall::DelayVariation]
        );
        assert_eq!(
            test().shortfalls(&[&events(&low, clean_but(50))]),
            [SearchShortfall::LoadSender]
        );
        // Before a rise at the end.
        let rising = events(&[20, 30, 30, 30, 300], clean_but(51));
        assert_eq!(
            test().shortfalls(&[&rising]),
            [SearchShortfall::DelayVariation]
        );
    }

    // : a Hold Interval with no delay sample, in which no Load PDU
    // arrived in order, as when the Load Sender stalls or the path drops
    // everything for a while, shows no delay variation: only Hold
    // Intervals with a sample count towards DelayVariation, more than half
    // of all the Trial Intervals, and a search held otherwise is judged by
    // its IP-Layer Sender Bit Rate, as the later causes judge it
    #[test]
    fn hold_intervals_without_a_delay_sample_are_no_delay_variation() {
        let low = [20, 30, 30, 30, 30];
        let held = |without| TrialIntervalCounts {
            hold_without_sample: without,
            ..clean_but(60)
        };
        assert_eq!(
            test().shortfalls(&[&events(&low, held(9))]),
            [SearchShortfall::DelayVariation]
        );
        assert_eq!(
            test().shortfalls(&[&events(&low, held(10))]),
            [SearchShortfall::LoadSender]
        );
        assert_eq!(
            test().shortfalls(&[&events(&low, held(60))]),
            [SearchShortfall::LoadSender]
        );
        let rising = events(&[20, 30, 30, 30, 300], held(60));
        assert_eq!(
            test().shortfalls(&[&rising]),
            [SearchShortfall::StillRising]
        );
    }

    // : in Fast Mode algorithm B adds highSpeedDelta (10) rows, 10 Mbps
    // below the 1 Gbps row, each Clean Interval: 200 Mbps over a
    // Sub-Interval of 20 Trial Intervals, 100 with half of them clean. A
    // last Sub-Interval above every earlier one by more is still rising; a
    // Load Sender that can't keep up adds less, a step as its Load PDUs
    // grow past the 1 Gbps row included
    #[test]
    fn a_search_still_rising_at_the_end_ended_too_early() {
        let rising = [83, 283, 483, 683, 883];
        assert_eq!(
            test().shortfalls(&[&events(&rising, clean_but(0))]),
            [SearchShortfall::StillRising]
        );
        // 100 Mbps more is Fast Mode's least; 101 more rises.
        let by = |more: u64| events(&[80, 280, 330, 330, 330 + more], clean_but(0));
        assert_eq!(
            test().shortfalls(&[&by(101)]),
            [SearchShortfall::StillRising]
        );
        assert_eq!(
            test().shortfalls(&[&by(100)]),
            [SearchShortfall::LoadSender]
        );
        // A step of 20% as 1250-byte Load PDUs become 1500 bytes, at the
        // same packet rate.
        assert_eq!(
            test().shortfalls(&[&events(&[80, 280, 330, 330, 396], clean_but(0))]),
            [SearchShortfall::LoadSender]
        );
        // Above every earlier one, not just the one before.
        assert_eq!(
            test().shortfalls(&[&events(&[80, 480, 300, 330, 470], clean_but(0))]),
            [SearchShortfall::LoadSender]
        );
        // A wider highSpeedDelta rises faster.
        let wider = SearchTest {
            parameters: TestParameters {
                high_speed_delta: 20,
                ..test().parameters
            },
            ..test()
        };
        assert_eq!(wider.shortfalls(&[&by(150)]), [SearchShortfall::LoadSender]);
        // One Sub-Interval measured is no rise.
        assert_eq!(
            test().shortfalls(&[&events(&[500], clean_but(0))]),
            [SearchShortfall::LoadSender]
        );
    }

    // : otherwise the Load Sender stopped rising while its Trial
    // Intervals were clean: it may not have kept up, the Server's Skipped
    // or Withheld Bursts taking no lpduSeqNo, so its search saw no loss
    #[test]
    fn a_search_that_stopped_rising_while_clean_names_the_load_sender() {
        assert_eq!(
            test().shortfalls(&[&events(&FLAT, clean_but(0))]),
            [SearchShortfall::LoadSender]
        );
        // Falling off at the end too.
        assert_eq!(
            test().shortfalls(&[&events(&[80, 280, 380, 380, 360], clean_but(0))]),
            [SearchShortfall::LoadSender]
        );
        // With no IP-Layer Sender Bit Rate at all.
        assert_eq!(
            test().shortfalls(&[&events(&[], clean_but(0))]),
            [SearchShortfall::LoadSender]
        );
    }

    /// A 5 s Fixed-Rate Test's results, whose Load Receiver received
    /// `received` Mbps in each of its 1 s Sub-Intervals, losing nothing, and
    /// estimated its Load Sender sent `sent` Mbps in each, in 1250-byte IPv4
    /// packets; its first `first_mode` Sub-Intervals a Capacity Mode of
    /// their own, when split.
    fn fixed_rate(
        received: [u64; 5],
        sent: [u64; 5],
        first_mode: Option<u32>,
    ) -> MeasurementResults {
        let mut events = sized(&sent, 1250, IpVersion::V4, clean_but(0));
        for (seq, mbps) in (1..).zip(received) {
            // A second at `mbps` Mbps: 100 packets of 1250 bytes a Mbps.
            let datagrams = u32::try_from(mbps * 100).expect("fits");
            let stats = SubIntervalStats {
                rx_datagrams: datagrams,
                rx_bytes: u64::from(datagrams) * (1250 - 28),
                delta_time: 1_000_000,
                rtt_var_minimum: STATUS_NODEL,
                rtt_var_maximum: STATUS_NODEL,
                delay_var_min: STATUS_NODEL,
                ..SubIntervalStats::default()
            };
            events.push(DataEvent::SubIntervalMeasured(SubIntervalMeasurement::new(
                seq,
                &stats,
                0,
                Duration::from_secs(1),
                true,
            )));
        }
        MeasurementResults::of_test_connection(
            &events,
            &Evaluation {
                test_interval: Duration::from_secs(5),
                sub_interval: Duration::from_secs(1),
                ip_version: IpVersion::V4,
                criterion: PerformanceCriterion::DEFAULT,
                first_mode,
            },
        )
    }

    // : a Downstream Fixed-Rate Test whose Server sent more than 5%
    // below its sending rate, its Test Connections' rows summed, in the
    // Sub-Interval of a Maximum IP-Layer Capacity, as its Client estimated
    // what it sent, didn't keep it: the lowest such estimate, and in how
    // many Capacity Modes. Within the margin, or away from a maximum, it
    // kept it; and nothing is judged where the Client counted its own
    // sending, Upstream, or a row isn't the table's. Row N offers N Mbps up
    // to row 1000 (TR-471 Annex A)
    #[test]
    fn a_fixed_rate_server_that_sent_below_its_rate_at_a_maximum_did_not_keep_it() {
        let test = FixedRateTest {
            jumbo: true,
            traditional_mtu: false,
        };
        let not_kept = |estimated_mbps: u64, sending_mbps: u64, modes| {
            Some(SendingRateNotKept {
                estimated_bps: estimated_mbps * 1_000_000,
                sending_bps: sending_mbps * 1_000_000,
                modes,
            })
        };
        // The maximum is Sub-Interval 3's.
        let received = [60, 70, 72, 71, 65];
        let row_90 = |sent| test.sending_rate_not_kept(&[90], &fixed_rate(received, sent, None));
        assert_eq!(row_90([90; 5]), None);
        // 5% below 90 Mbps is 85.5.
        assert_eq!(row_90([86; 5]), None);
        assert_eq!(row_90([85; 5]), not_kept(85, 90, 1));
        // Exactly 5% below is at the rate: 95 Mbps of row 100's 100.
        let row_100 = |sent| test.sending_rate_not_kept(&[100], &fixed_rate(received, sent, None));
        assert_eq!(row_100([95; 5]), None);
        assert_eq!(row_100([94; 5]), not_kept(94, 100, 1));
        // Only where the maximum was measured.
        assert_eq!(row_90([90, 90, 72, 90, 90]), not_kept(72, 90, 1));
        assert_eq!(row_90([72, 72, 90, 72, 72]), None);
        // Two Test Connections at row 45: 90 Mbps between them.
        let both = fixed_rate(received, [65; 5], None);
        assert_eq!(
            test.sending_rate_not_kept(&[45, 45], &both),
            not_kept(65, 90, 1)
        );
        // Each Capacity Mode's maximum, in Sub-Intervals 2 and 4 here: the
        // lowest estimate of those short, and how many were.
        let split = |sent| {
            let results = fixed_rate([70, 72, 60, 71, 65], sent, Some(2));
            test.sending_rate_not_kept(&[90], &results)
        };
        assert_eq!(split([90, 80, 90, 84, 90]), not_kept(80, 90, 2));
        assert_eq!(split([90, 80, 90, 90, 90]), not_kept(80, 90, 1));
        // Upstream the Client counts what it sent itself: not judged.
        let upstream = MeasurementResults {
            sender_estimated: false,
            ..fixed_rate(received, [72; 5], None)
        };
        assert_eq!(test.sending_rate_not_kept(&[90], &upstream), None);
        // No row 2000.
        let beyond = fixed_rate(received, [72; 5], None);
        assert_eq!(test.sending_rate_not_kept(&[2000], &beyond), None);
    }
}
