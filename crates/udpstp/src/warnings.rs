//! A Test's warnings (docs/spec.md "Results and output"; ): what makes its
//! results less than they seem. The text results list each one's message
//! under Warnings; the JSON results list each as `{Code, Count, Message}`
//! in `udpstp.Warnings`, so both come from this one list. A code is the
//! Server log's name for the same event where it has one, or one of
//! [`names::warning`]; the message is for people and may change.

use std::time::Duration;

use udpstp_core::{DataEvent, Direction, EndReason, SearchShortfall};
use udpstp_runtime::{EngineFacts, SocketBuffers};

use crate::names::{self, event};

/// What a Test's warnings come from, or one Test Connection's.
pub struct Sources<'a> {
    /// The Data Phase's events: one Test Connection's, or every one's.
    pub data_events: &'a [DataEvent],
    /// How each Data Phase ended: the one Test Connection's, or in a
    /// Multi-Connection Test each one's, named.
    pub ends: &'a [DataPhaseEnd<'a>],
    /// The Client user interrupted the Test.
    pub interrupted: bool,
    /// `--first-mode-sub-intervals`, and the Sub-Intervals the Test
    /// Interval held.
    pub first_mode: Option<u32>,
    pub sub_intervals: u32,
    /// How the Data Phase ran on this host, as its Data Phase Engines
    /// reported: the Receive Buffer Overflows at its sockets, where the OS
    /// counts them, and the socket buffers the OS granted.
    pub engine_facts: EngineFacts,
    /// What to raise when the socket buffers come out short, on this OS.
    pub socket_buffer_limits: &'static str,
    /// An Upstream Multi-Connection Test's Sub-Intervals were summed by
    /// number, up to this far apart.
    pub approximate: Option<Duration>,
}

/// How a Data Phase ended, for the warnings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataPhaseEnd<'a> {
    /// Its Test Connection's mcIndex, in a Multi-Connection Test.
    pub test_connection: Option<u8>,
    /// Its end, if it ended.
    pub reason: Option<EndReason>,
    /// Why its Data Phase Engine failed, if it did.
    pub failure: Option<&'a str>,
}

/// Everything that makes the results less than they seem, in the order
/// the text and the JSON results list it.
pub fn of(sources: &Sources<'_>) -> Vec<Warning> {
    let mut warnings = of_data_events(sources.data_events);
    for end in sources.ends {
        if let Some(error) = end.failure {
            warnings.push(Warning::DataPhaseEngineFailed {
                error: error.to_owned(),
                test_connection: end.test_connection,
            });
        }
        match end.reason {
            Some(EndReason::Completed) | None => {}
            // Stopped by the interrupt, not by the path: said once.
            Some(EndReason::Stopped) if sources.interrupted => {
                if !warnings.contains(&Warning::Interrupted) {
                    warnings.push(Warning::Interrupted);
                }
            }
            Some(reason) => warnings.push(Warning::DataPhaseEnded {
                reason,
                test_connection: end.test_connection,
            }),
        }
    }
    if let Some(split) = sources
        .first_mode
        .filter(|&split| split >= sources.sub_intervals)
    {
        warnings.push(Warning::FirstModeSplitIgnored {
            split,
            sub_intervals: sources.sub_intervals,
        });
    }
    if let Some(count) = sources
        .engine_facts
        .receive_buffer_overflows
        .filter(|&n| n > 0)
    {
        warnings.push(Warning::ReceiveBufferOverflows { count });
    }
    warnings.extend(socket_buffers(
        sources.engine_facts.socket_buffers,
        sources.socket_buffer_limits,
    ));
    if let Some(offset) = sources.approximate {
        warnings.push(Warning::UpstreamResultsApproximate {
            offset_ms: offset.as_millis(),
        });
    }
    warnings
}

/// One warning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Warning {
    /// A Data Phase count, and how many.
    Counted { what: Counted, count: u64 },
    /// Nothing arrived for 1 s, so this end stopped sending: how often.
    TrafficStopped { times: u64 },
    /// The Client kept to its maxBandwidth though the Server assigned more.
    SendingRateCapped {
        structures: u64,
        highest_kbps: u64,
        cap_kbps: u64,
    },
    /// The Data Phase Engine failed, so the results are partial: in a
    /// Multi-Connection Test, that of the Test Connection named.
    DataPhaseEngineFailed {
        error: String,
        test_connection: Option<u8>,
    },
    /// The Test ended without the Server's STOP; in a Multi-Connection
    /// Test, the Test Connection named did.
    DataPhaseEnded {
        reason: EndReason,
        test_connection: Option<u8>,
    },
    /// The Client user interrupted the Test, so the results are partial.
    Interrupted,
    /// `--first-mode-sub-intervals` left no second Capacity Mode.
    FirstModeSplitIgnored { split: u32, sub_intervals: u32 },
    /// The OS discarded datagrams at this host's socket before udpstp read
    /// them: Receive Buffer Overflows.
    ReceiveBufferOverflows { count: u64 },
    /// The OS held the socket buffers below the size asked for.
    SocketBuffersShort {
        requested: usize,
        limits: &'static str,
    },
    /// An Upstream Multi-Connection Test's Sub-Intervals were summed by
    /// number from each Test Connection's own, up to this many ms apart
    ///
    UpstreamResultsApproximate { offset_ms: u128 },
    /// No Sub-Interval of a Capacity Mode met the Performance Criterion.
    NoValidMaximum,
    /// In a Downstream Fixed-Rate Test, the IP-Layer Sender Bit Rate the
    /// Client estimated in the Sub-Interval of a Maximum IP-Layer Capacity
    /// fell more than [`SENDING_RATE_MARGIN_PERCENT`] short of the Server's
    /// sending rate: the Server may not have kept up, and the result may
    /// understate the path. The lowest estimate, in bit/s, the
    /// sending rate, and how many Capacity Modes it concerns.
    SendingRateNotKept {
        estimated_bps: u64,
        sending_bps: u64,
        modes: u64,
    },
    /// In a Search Test, no Test Connection's search found the path's
    /// limit, as the Client counted its Trial Intervals, neither confirming
    /// Congestion nor meeting it at its end, and the result may understate
    /// the path:
    /// why this one fell short, and in a Multi-Connection Test which Test
    /// Connection it was. The Load Sender is the Server in a Downstream
    /// Test and this host in an Upstream one; and algorithm C, which rises
    /// faster, may help a Test that ended too early.
    CongestionNeverConfirmed {
        shortfall: SearchShortfall,
        direction: Direction,
        algorithm_c: bool,
        test_connection: Option<u8>,
    },
    /// In a Search Test, no Sub-Interval after the Maximum IP-Layer
    /// Capacity's met the Performance Criterion, as the core judges it
    ///, and one that missed it, before the Maximum's or after it,
    /// ran more than `--faster-sub-interval-margin` faster, or the last one
    /// ran under 60% of the Maximum; or several of those, in one warning.
    /// A faster one after it is what a search cycling around a drop-tail
    /// bottleneck gives, so the Maximum may be well below the path. A
    /// faster one before it is what a search that climbed back slowly after
    /// an overshoot, a path that slowed, a policer or shaper that let a
    /// burst through at first or, Downstream, a Server that fell behind
    /// gives. A slow last one is what a queue that kept dropping after it
    /// overflowed, a path that slowed or, Downstream, a Server that fell
    /// behind gives. The Capacity Mode, from 1, when there are two; the
    /// Maximum, in bit/s; each of those Sub-Intervals; the criterion; and
    /// the Test's direction, which names its Load Sender.
    SubIntervalsAfterMaximumMissed {
        capacity_mode: Option<usize>,
        maximum_bps: u64,
        faster_before: Option<MissedSubInterval>,
        faster: Option<MissedSubInterval>,
        slow_last: Option<MissedSubInterval>,
        max_loss_ppm: u32,
        direction: Direction,
    },
}

/// A Sub-Interval that missed the Performance Criterion, as a warning
/// names it: its number, IP-Layer Capacity in bit/s, and Load PDUs lost
/// and sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MissedSubInterval {
    pub sub_interval: u32,
    pub bps: u64,
    pub lost: u64,
    pub sent: u64,
}

impl MissedSubInterval {
    fn loss_ratio(&self) -> f64 {
        self.lost as f64 / self.sent.max(1) as f64
    }
}

/// How far short of the sending rate the IP-Layer Sender Bit Rate a Load
/// Receiver estimated may fall before the Load Sender is taken not to
/// have kept up: the core's margin for a rate the IP-Layer Sender Bit Rate
/// is at, which judges a Search Test's searches too.
pub use udpstp_core::SENDING_RATE_MARGIN_PERCENT;

/// The Data Phase counts that are warnings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Counted {
    PathMtuExceeded,
    HopLimitExceeded,
    IcmpErrors,
    BurstsSkipped,
    BurstsWithheld,
    InvalidLoadPdus,
    IgnoredLoadPdus,
    DelaySamplesDropped,
    InvalidStatusPdus,
    StaleStatusPdus,
    StatusPdusMissing,
    StatusPdusFailedVerification,
    PeerMissedStatusPdus,
    PeerRxStopped,
}

impl Counted {
    /// Every count, in the order the results list them.
    const ALL: [Self; 14] = [
        Self::PathMtuExceeded,
        Self::HopLimitExceeded,
        Self::IcmpErrors,
        Self::BurstsSkipped,
        Self::BurstsWithheld,
        Self::InvalidLoadPdus,
        Self::IgnoredLoadPdus,
        Self::DelaySamplesDropped,
        Self::InvalidStatusPdus,
        Self::StaleStatusPdus,
        Self::StatusPdusMissing,
        Self::StatusPdusFailedVerification,
        Self::PeerMissedStatusPdus,
        Self::PeerRxStopped,
    ];

    /// Its code: the Server log's event name for the same count.
    fn code(self) -> &'static str {
        match self {
            Self::PathMtuExceeded => event::PATH_MTU_EXCEEDED,
            Self::HopLimitExceeded => event::HOP_LIMIT_EXCEEDED,
            Self::IcmpErrors => event::ICMP_ERRORS,
            Self::BurstsSkipped => event::BURSTS_SKIPPED,
            Self::BurstsWithheld => event::BURSTS_WITHHELD,
            Self::InvalidLoadPdus => event::INVALID_LOAD_PDUS,
            Self::IgnoredLoadPdus => event::IGNORED_LOAD_PDUS,
            Self::DelaySamplesDropped => event::DELAY_SAMPLES_DROPPED,
            Self::InvalidStatusPdus => event::INVALID_STATUS_PDUS,
            Self::StaleStatusPdus => event::STALE_STATUS_PDUS,
            Self::StatusPdusMissing => event::STATUS_PDUS_MISSING,
            Self::StatusPdusFailedVerification => event::STATUS_PDUS_FAILED_VERIFICATION,
            Self::PeerMissedStatusPdus => event::PEER_MISSED_STATUS_PDUS,
            Self::PeerRxStopped => event::PEER_RX_STOPPED,
        }
    }

    /// What the results say after the count.
    fn what(self) -> &'static str {
        match self {
            Self::PathMtuExceeded => {
                "datagrams too large for the path with DF set: a hop's MTU is below the Test's packet size, so this Test didn't measure the path's capacity. Drop --traditional-mtu, or ask a Server administrator whose Server runs Jumbo rows to lower its server_mtu"
            }
            Self::HopLimitExceeded => {
                "ICMP time-exceeded errors: --max-hops is too small for the path"
            }
            Self::IcmpErrors => "ICMP errors from the path (unreachable or refused) ignored",
            Self::BurstsSkipped => "Load PDUs not sent: this host fell behind (Skipped Bursts)",
            Self::BurstsWithheld => {
                "Load PDUs not sent: the send buffer was full (Withheld Bursts)"
            }
            Self::InvalidLoadPdus => "invalid Load PDUs ignored",
            Self::IgnoredLoadPdus => "Load PDUs ignored for an implausible lpduSeqNo jump",
            Self::DelaySamplesDropped => "Load PDUs without a delay sample: the wall clock stepped",
            Self::InvalidStatusPdus => "invalid Status PDUs ignored",
            Self::StaleStatusPdus => "stale Status PDUs, not used for the sending rate",
            Self::StatusPdusMissing => "Status PDUs never arrived",
            Self::StatusPdusFailedVerification => {
                "Status PDUs failed authentication and were ignored (mode 2)"
            }
            Self::PeerMissedStatusPdus => {
                "Load PDUs said the peer had missed Status PDUs (spduSeqErr)"
            }
            Self::PeerRxStopped => "PDUs from the peer said it had stopped receiving (rxStopped)",
        }
    }

    /// The count a Data Phase event adds to, and how much.
    fn of(data: &DataEvent) -> Option<(Self, u32)> {
        Some(match *data {
            DataEvent::PathMtuExceeded { count } => (Self::PathMtuExceeded, count),
            DataEvent::HopLimitExceeded { count } => (Self::HopLimitExceeded, count),
            DataEvent::IcmpErrors { count } => (Self::IcmpErrors, count),
            DataEvent::BurstsSkipped { datagrams, .. } => (Self::BurstsSkipped, datagrams),
            DataEvent::BurstsWithheld { datagrams, .. } => (Self::BurstsWithheld, datagrams),
            DataEvent::InvalidLoadPdus { count } => (Self::InvalidLoadPdus, count),
            DataEvent::IgnoredLoadPdus { count } => (Self::IgnoredLoadPdus, count),
            DataEvent::DelaySamplesDropped { count } => (Self::DelaySamplesDropped, count),
            DataEvent::InvalidStatusPdus { count } => (Self::InvalidStatusPdus, count),
            DataEvent::StaleStatusPdus { count } => (Self::StaleStatusPdus, count),
            DataEvent::StatusPdusMissing { count } => (Self::StatusPdusMissing, count),
            DataEvent::StatusPdusFailedVerification { count } => {
                (Self::StatusPdusFailedVerification, count)
            }
            DataEvent::PeerMissedStatusPdus { count } => (Self::PeerMissedStatusPdus, count),
            DataEvent::PeerRxStopped { count } => (Self::PeerRxStopped, count),
            _ => return None,
        })
    }
}

impl Warning {
    /// Its code: the Server log's event name for it, or its own.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Counted { what, .. } => what.code(),
            Self::TrafficStopped { .. } => event::TRAFFIC_STOPPED,
            Self::SendingRateCapped { .. } => names::warning::SENDING_RATE_CAPPED,
            Self::DataPhaseEngineFailed { .. } | Self::DataPhaseEnded { .. } => {
                event::DATA_PHASE_ENDED
            }
            Self::Interrupted => names::warning::INTERRUPTED,
            Self::FirstModeSplitIgnored { .. } => names::warning::FIRST_MODE_SPLIT_IGNORED,
            Self::SocketBuffersShort { .. } => event::SOCKET_BUFFERS_SHORT,
            Self::ReceiveBufferOverflows { .. } => names::warning::RECEIVE_BUFFER_OVERFLOWS,
            Self::UpstreamResultsApproximate { .. } => names::warning::UPSTREAM_RESULTS_APPROXIMATE,
            Self::NoValidMaximum => names::warning::NO_VALID_MAXIMUM,
            Self::SendingRateNotKept { .. } => names::warning::SENDING_RATE_NOT_KEPT,
            Self::CongestionNeverConfirmed { .. } => names::warning::CONGESTION_NEVER_CONFIRMED,
            Self::SubIntervalsAfterMaximumMissed { .. } => {
                names::warning::SUB_INTERVALS_AFTER_MAXIMUM_MISSED
            }
        }
    }

    /// How many times it happened: the datagrams or PDUs a count is of,
    /// the Sending Rate Structures capped, or 1.
    pub fn count(&self) -> u64 {
        match self {
            Self::Counted { count, .. } => *count,
            Self::TrafficStopped { times } => *times,
            Self::SendingRateCapped { structures, .. } => *structures,
            Self::ReceiveBufferOverflows { count } => *count,
            Self::SendingRateNotKept { modes, .. } => *modes,
            _ => 1,
        }
    }

    /// What the text results say.
    pub fn message(&self) -> String {
        match self {
            Self::Counted { what, count } => format!("{count} {}", what.what()),
            Self::TrafficStopped { .. } => {
                "Nothing was received for 1 s, so this end stopped sending".into()
            }
            Self::SendingRateCapped {
                structures,
                highest_kbps,
                cap_kbps,
            } => format!(
                "The Server assigned up to {} Mbps, above the {} Mbps maxBandwidth this Client stated; it sent at most {} Mbps ({structures} Sending Rate Structures capped)",
                mbps_of_kbps(*highest_kbps),
                mbps_of_kbps(*cap_kbps),
                mbps_of_kbps(*cap_kbps),
            ),
            Self::DataPhaseEngineFailed {
                error,
                test_connection: None,
            } => format!("The Data Phase failed, so the results are partial: {error}"),
            Self::DataPhaseEngineFailed {
                error,
                test_connection: Some(index),
            } => format!(
                "Test Connection {index}'s Data Phase failed, so the results are partial: {error}"
            ),
            Self::DataPhaseEnded {
                reason: EndReason::Watchdog,
                test_connection: None,
            } => "The Watchdog ended the Test: nothing received for 3 s".into(),
            Self::DataPhaseEnded {
                reason: EndReason::Watchdog,
                test_connection: Some(index),
            } => format!("The Watchdog ended Test Connection {index}: nothing received for 3 s"),
            Self::DataPhaseEnded {
                reason,
                test_connection: None,
            } => format!(
                "The Test ended without the Server's STOP: {}",
                names::end_reason(*reason)
            ),
            Self::DataPhaseEnded {
                reason,
                test_connection: Some(index),
            } => format!(
                "Test Connection {index} ended without the Server's STOP: {}",
                names::end_reason(*reason)
            ),
            Self::Interrupted => "The Test was interrupted, so the results are partial".into(),
            Self::FirstModeSplitIgnored {
                split,
                sub_intervals,
            } => format!(
                "--first-mode-sub-intervals is {split}, but the Test had {sub_intervals} Sub-Intervals: one Capacity Mode"
            ),
            Self::ReceiveBufferOverflows { count } => format!(
                "{count} datagrams discarded by this host before udpstp read them: its receive buffer overflowed (Receive Buffer Overflows), and a Load Receiver counts them as lost"
            ),
            Self::SocketBuffersShort { requested, limits } => format!(
                "The OS held the socket buffers below the {requested} bytes asked for, so datagrams may have been dropped locally; raise {limits}"
            ),
            Self::UpstreamResultsApproximate { offset_ms } => format!(
                "Upstream results are approximate: each Test Connection's Load Receiver keeps its own Sub-Intervals, summed by number, up to {offset_ms} ms apart"
            ),
            Self::NoValidMaximum => {
                "No Sub-Interval met the Performance Criterion: no valid Maximum IP-Layer Capacity"
                    .into()
            }
            Self::SendingRateNotKept {
                estimated_bps,
                sending_bps,
                ..
            } => format!(
                "The Server sent about {:.2} Mbps in the Sub-Interval of the Maximum IP-Layer Capacity, as estimated here, more than {SENDING_RATE_MARGIN_PERCENT}% below the {:.2} Mbps of its Fixed-Rate row: it may not have kept up, so the result may understate the path",
                *estimated_bps as f64 / 1e6,
                *sending_bps as f64 / 1e6,
            ),
            Self::CongestionNeverConfirmed {
                shortfall,
                direction,
                algorithm_c,
                test_connection,
            } => {
                let search = match test_connection {
                    None => "The search never confirmed Congestion".to_string(),
                    Some(index) => {
                        format!("Test Connection {index}'s search never confirmed Congestion")
                    }
                };
                let why: String = match shortfall {
                    SearchShortfall::MaxBandwidth(mbps) => {
                        let stated = match test_connection {
                            None => "this Client stated (--max-bandwidth),",
                            Some(_) => "it stated, its share of --max-bandwidth,",
                        };
                        // A udpstp Server stops there; an OB-UDPST Server
                        // only in a RATE_LIMITING build run with -B
                        // (conformance: search-capped-at-stated-max-bandwidth).
                        format!(
                            "the IP-Layer Sender Bit Rate reached the {mbps} Mbps maxBandwidth {stated} at which a Server may stop its search, so the result may understate the path"
                        )
                    }
                    SearchShortfall::TableTop => "the IP-Layer Sender Bit Rate reached the top of the Sending Rate Table, so the result may understate the path; more Test Connections (--test-connections) can measure beyond it".into(),
                    SearchShortfall::DelayVariation => "most Trial Intervals were Hold Intervals, delay variation between lowThresh and upperThresh holding the sending rate, so the result may understate the path".into(),
                    SearchShortfall::StillRising => format!(
                        "the IP-Layer Sender Bit Rate was still rising in the last Sub-Interval: the Test ended too early, so the result may understate the path; a longer --test-interval helps{}",
                        if *algorithm_c {
                            ""
                        } else {
                            ", or --algorithm c, which rises faster"
                        }
                    ),
                    // What was checked: no slowAdjThresh Errored Intervals
                    // back to back, and too few of the last ones to have met
                    // the path's limit at the end.
                    SearchShortfall::LoadSender => match direction {
                        Direction::Downstream => "the Server's IP-Layer Sender Bit Rate, as estimated here, stopped rising though few of the last 64 Trial Intervals were Errored Intervals: it may not have kept up, so the result may understate the path".into(),
                        Direction::Upstream => "this host's IP-Layer Sender Bit Rate stopped rising though few of the last 64 Trial Intervals were Errored Intervals: it may not have kept up, so the result may understate the path".into(),
                    },
                };
                format!("{search}: {why}")
            }
            Self::SubIntervalsAfterMaximumMissed {
                capacity_mode,
                maximum_bps,
                faster_before,
                faster,
                slow_last,
                max_loss_ppm,
                direction,
            } => {
                let mode = capacity_mode
                    .map(|mode| format!("In Capacity Mode {mode}, "))
                    .unwrap_or_default();
                let maximum = *maximum_bps as f64 / 1e6;
                let of_maximum = u128::from((*maximum_bps).max(1));
                let tenths = |tenths: u128| format!("{}.{}", tenths / 10, tenths % 10);
                let mut figures: Vec<String> = Vec::new();
                let mut causes: Vec<&str> = Vec::new();
                let mut cause = |cause| {
                    if !causes.contains(&cause) {
                        causes.push(cause);
                    }
                };
                // The first figure names the Maximum in full.
                let named = |figures: &[String]| {
                    if figures.is_empty() {
                        format!("the Maximum IP-Layer Capacity of {maximum:.2} Mbps")
                    } else {
                        "the Maximum".to_string()
                    }
                };
                for (sub_interval, before) in [(faster_before, true), (faster, false)] {
                    let Some(sub_interval) = sub_interval else {
                        continue;
                    };
                    // Up to a tenth, so that just over the margin never
                    // reads as at it.
                    let above = tenths(
                        (u128::from(sub_interval.bps.saturating_sub(*maximum_bps)) * 1000)
                            .div_ceil(of_maximum),
                    );
                    figures.push(format!(
                        "Sub-Interval {}, {} the Maximum's, reached {:.2} Mbps, {above}% above {}, with a Loss Ratio of {:.6}",
                        sub_interval.sub_interval,
                        if before { "before" } else { "after" },
                        sub_interval.bps as f64 / 1e6,
                        named(&figures),
                        sub_interval.loss_ratio(),
                    ));
                    if before {
                        cause("the search may have climbed back slowly after an overshoot");
                        cause("the path may have slowed during the Test");
                        cause("a policer or shaper may have let a burst through at first");
                    } else {
                        cause(
                            "the search may have cycled around a bottleneck whose queue drops more than that",
                        );
                    }
                }
                if let Some(last) = slow_last {
                    // Down to a tenth, so that just under the threshold
                    // never reads as at it.
                    let share = tenths(u128::from(last.bps) * 1000 / of_maximum);
                    figures.push(format!(
                        "Sub-Interval {}, the last, ran at {:.2} Mbps, {share}% of {}, with a Loss Ratio of {:.6}",
                        last.sub_interval,
                        last.bps as f64 / 1e6,
                        named(&figures),
                        last.loss_ratio(),
                    ));
                    cause(
                        "a queue may have kept dropping after it overflowed (such as CAKE with one Test Connection)",
                    );
                    cause("the path may have slowed during the Test");
                }
                // A Server that fell behind can explain the Maximum below an
                // earlier faster one, or a slow last one, though not a
                // faster one after it.
                if *direction == Direction::Downstream
                    && (faster_before.is_some() || slow_last.is_some())
                {
                    cause("the Server may have fallen behind");
                }
                let causes = match causes.split_last() {
                    Some((last, [])) => (*last).to_string(),
                    Some((last, rest)) => format!("{}, or {last}", rest.join(", ")),
                    None => String::new(),
                };
                let consequence = if slow_last.is_some() {
                    "so the Maximum may be well below the path, or no longer what it carries"
                } else {
                    "so the Maximum may be well below the path"
                };
                format!(
                    "{mode}{}, and no Sub-Interval after the Maximum's met the Performance Criterion (--max-loss-ratio {}): {causes}, {consequence}",
                    figures.join("; "),
                    f64::from(*max_loss_ppm) / 1_000_000.0,
                )
            }
        }
    }
}

/// The warnings the Data Phase's events give, in the order they are
/// listed.
pub fn of_data_events(data_events: &[DataEvent]) -> Vec<Warning> {
    let mut counts = [0u64; Counted::ALL.len()];
    let (mut stopped, mut capped, mut highest, mut cap) = (0, 0, 0, 0);
    for data in data_events {
        if let Some((what, n)) = Counted::of(data) {
            let i = Counted::ALL
                .iter()
                .position(|c| *c == what)
                .expect("in ALL");
            counts[i] += u64::from(n);
            continue;
        }
        match *data {
            DataEvent::TrafficStopped { .. } => stopped += 1,
            DataEvent::SendingRateCapped {
                structures,
                highest_kbps,
                cap_kbps,
            } => {
                capped += u64::from(structures);
                highest = highest.max(highest_kbps);
                cap = cap_kbps;
            }
            _ => {}
        }
    }
    let mut warnings: Vec<Warning> = Counted::ALL
        .into_iter()
        .zip(counts)
        .filter(|(_, count)| *count > 0)
        .map(|(what, count)| Warning::Counted { what, count })
        .collect();
    if stopped > 0 {
        warnings.push(Warning::TrafficStopped { times: stopped });
    }
    if capped > 0 {
        warnings.push(Warning::SendingRateCapped {
            structures: capped,
            highest_kbps: highest,
            cap_kbps: cap,
        });
    }
    warnings
}

/// The warning for socket buffers the OS held below the size asked for.
pub fn socket_buffers(buffers: Option<SocketBuffers>, limits: &'static str) -> Option<Warning> {
    buffers
        .filter(SocketBuffers::short)
        .map(|buffers| Warning::SocketBuffersShort {
            requested: buffers.requested,
            limits,
        })
}

/// kbit/s as Mbps, to three decimals at most.
fn mbps_of_kbps(kbps: u64) -> String {
    let text = format!("{}.{:03}", kbps / 1000, kbps % 1000);
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // : a Multi-Connection Test's warnings name the Test Connection a
    // Data Phase's end concerns, each of them, and an interrupt once
    #[test]
    fn a_multi_connection_tests_ends_are_named() {
        let ends = [
            DataPhaseEnd {
                test_connection: Some(0),
                reason: Some(EndReason::Watchdog),
                failure: None,
            },
            DataPhaseEnd {
                test_connection: Some(1),
                reason: Some(EndReason::Completed),
                failure: None,
            },
            DataPhaseEnd {
                test_connection: Some(2),
                reason: None,
                failure: Some("the socket closed"),
            },
        ];
        let sources = |ends, interrupted| Sources {
            data_events: &[],
            ends,
            interrupted,
            first_mode: None,
            sub_intervals: 5,
            engine_facts: EngineFacts::default(),
            socket_buffer_limits: "",
            approximate: None,
        };
        let messages: Vec<String> = of(&sources(&ends, false))
            .iter()
            .map(Warning::message)
            .collect();
        assert_eq!(
            messages,
            [
                "The Watchdog ended Test Connection 0: nothing received for 3 s",
                "Test Connection 2's Data Phase failed, so the results are partial: the socket closed",
            ]
        );
        let stopped = DataPhaseEnd {
            test_connection: Some(0),
            reason: Some(EndReason::Stopped),
            failure: None,
        };
        let both = [
            stopped,
            DataPhaseEnd {
                test_connection: Some(1),
                ..stopped
            },
        ];
        assert_eq!(of(&sources(&both, true)), [Warning::Interrupted]);
        assert_eq!(
            of(&sources(&both, false))
                .iter()
                .map(Warning::message)
                .collect::<Vec<_>>(),
            [
                "Test Connection 0 ended without the Server's STOP: Stopped",
                "Test Connection 1 ended without the Server's STOP: Stopped",
            ]
        );
    }

    // : datagrams the OS discarded before udpstp read them are a
    // warning with their count, under their own code
    #[test]
    fn receive_buffer_overflows_are_a_counted_warning() {
        let warning = Warning::ReceiveBufferOverflows { count: 12 };
        assert_eq!(warning.code(), "ReceiveBufferOverflows");
        assert_eq!(warning.count(), 12);
        assert!(warning.message().starts_with("12 "));
    }

    // RFC 9946 §5 (DF), §10 (MaxHops); ADR 0006; : Path MTU and
    // hop-limit problems are reported, not mistaken for low capacity
    #[test]
    fn path_mtu_and_hop_limit_events_are_warnings() {
        let events = [
            DataEvent::PathMtuExceeded { count: 40 },
            DataEvent::HopLimitExceeded { count: 3 },
            DataEvent::IcmpErrors { count: 1 },
        ];
        let warnings: Vec<(&str, u64)> = of_data_events(&events)
            .iter()
            .map(|w| (w.code(), w.count()))
            .collect();
        assert_eq!(
            warnings,
            [
                ("PathMtuExceeded", 40),
                ("HopLimitExceeded", 3),
                ("IcmpErrors", 1)
            ]
        );
    }

    // docs/spec.md story 9 (the Client says when it kept to its
    // maxBandwidth against a Server that assigned more);
    #[test]
    fn capped_sending_rates_are_a_warning() {
        let events = [
            DataEvent::SendingRateCapped {
                structures: 2,
                highest_kbps: 20_000,
                cap_kbps: 2_500,
            },
            DataEvent::SendingRateCapped {
                structures: 1,
                highest_kbps: 12_345,
                cap_kbps: 2_500,
            },
        ];
        assert_eq!(
            of_data_events(&events),
            [Warning::SendingRateCapped {
                structures: 3,
                highest_kbps: 20_000,
                cap_kbps: 2_500,
            }]
        );
    }

    // : every code is a log event name or one of the warnings' own
    #[test]
    fn every_code_is_a_known_name() {
        let own = names::warning::ALL;
        assert!(own.iter().all(|code| !event::ALL.contains(code)));
        let mut every: Vec<Warning> = Counted::ALL
            .map(|what| Warning::Counted { what, count: 1 })
            .to_vec();
        every.extend([
            Warning::TrafficStopped { times: 1 },
            Warning::SendingRateCapped {
                structures: 1,
                highest_kbps: 1,
                cap_kbps: 1,
            },
            Warning::DataPhaseEngineFailed {
                error: String::new(),
                test_connection: None,
            },
            Warning::DataPhaseEnded {
                reason: EndReason::Watchdog,
                test_connection: Some(1),
            },
            Warning::Interrupted,
            Warning::FirstModeSplitIgnored {
                split: 1,
                sub_intervals: 1,
            },
            Warning::SocketBuffersShort {
                requested: 1,
                limits: "",
            },
            Warning::UpstreamResultsApproximate { offset_ms: 1 },
            Warning::NoValidMaximum,
            Warning::SendingRateNotKept {
                estimated_bps: 1,
                sending_bps: 2,
                modes: 1,
            },
            Warning::CongestionNeverConfirmed {
                shortfall: SearchShortfall::LoadSender,
                direction: Direction::Downstream,
                algorithm_c: false,
                test_connection: None,
            },
            Warning::SubIntervalsAfterMaximumMissed {
                capacity_mode: None,
                maximum_bps: 1,
                faster_before: None,
                faster: None,
                slow_last: Some(MissedSubInterval {
                    sub_interval: 2,
                    bps: 0,
                    lost: 1,
                    sent: 2,
                }),
                max_loss_ppm: 10_000,
                direction: Direction::Downstream,
            },
        ]);
        for warning in every {
            let code = warning.code();
            assert!(
                event::ALL.contains(&code) || own.contains(&code),
                "{warning:?}"
            );
        }
    }
}
