//! Trial Interval classification for the Load Rate Adjustment Algorithms
//! (RFC 9946 §5.1(6), §7.1; docs/spec.md "Load Rate Adjustment Algorithms").
//!
//! The Server classifies each Trial Interval from the Status PDU that
//! reports it: the one it receives in a Downstream Test, the one it sends in
//! an Upstream Test. A Client classifies them too, the Status PDUs it sends
//! in a Downstream Test and those it receives in an Upstream one, only to
//! count them: whether its search ever confirmed Congestion.

use crate::wire::status::STATUS_NODEL;
use crate::wire::{ActivationPdu, StatusPdu};

/// How a Trial Interval is classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntervalClass {
    /// A Clean Interval: Sequence Errors at or below seqErrThresh and
    /// DelayVar strictly below lowThresh.
    Clean,
    /// An Errored Interval: Sequence Errors above seqErrThresh or DelayVar
    /// above upperThresh.
    Errored,
    /// A Hold Interval: neither clean nor errored, including one with no
    /// delay sample and Sequence Errors at or below the threshold.
    Hold,
}

/// Which statistic of a Trial Interval's one-way delay variation is its
/// DelayVar (docs/spec.md: the maximum by default, a Server setting may choose
/// the average). RTT variation has one sample per Trial Interval, so this
/// doesn't apply to it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DelayVarStatistic {
    /// delayVarSum / delayVarCnt, rounded half up, as OB-UDPST uses.
    Average,
    /// delayVarMax: the range above the minimum, which RFC 9946 §5.1(6) and
    /// RFC 9097 §8.1 judge ("the delay range"), so the default.
    #[default]
    Maximum,
}

/// A classified Trial Interval, with the inputs that classified it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Classification {
    /// spduSeqNo of the Status PDU reporting the Trial Interval.
    pub spdu_seq_no: u32,
    pub class: IntervalClass,
    /// Sequence Errors, in Load PDUs: seqErrLoss, plus seqErrOoo and
    /// seqErrDup unless ignoreOooDup is set (RFC 9946 §7.1).
    pub seq_errors: u32,
    /// DelayVar in ms: the Trial Interval's one-way delay variation (see
    /// [`DelayVarStatistic`]), or its RTT variation sample, as useOwDelVar
    /// says; `None` without a sample.
    pub delay_var: Option<u32>,
    /// The Sending Rate Table row the Load Rate Adjustment Algorithm chose
    /// from this Trial Interval; `None` when no algorithm runs (a
    /// Fixed-Rate Test).
    pub row: Option<u16>,
}

/// The accepted Activation parameters the classification uses.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Thresholds {
    low: u16,
    upper: u16,
    seq_errors: u16,
    use_ow_del_var: bool,
    ignore_ooo_dup: bool,
    statistic: DelayVarStatistic,
}

impl Thresholds {
    /// From the Activation Response, so a Server's coercion counts.
    pub(crate) fn accepted(response: &ActivationPdu, statistic: DelayVarStatistic) -> Self {
        Self {
            low: response.low_thresh,
            upper: response.upper_thresh,
            seq_errors: response.seq_err_thresh,
            use_ow_del_var: response.use_ow_del_var != 0,
            ignore_ooo_dup: response.ignore_ooo_dup != 0,
            statistic,
        }
    }

    /// Classifies the Trial Interval `status` reports. seqErrThresh always
    /// applies, so 0 means any Sequence Error is errored; clean needs
    /// DelayVar strictly below lowThresh, as OB-UDPST and RFC 9097's
    /// pseudocode have it. A Trial Interval in which no Load PDU arrived
    /// has no delay sample and shows no loss until one does, so it is a
    /// Hold Interval, as in OB-UDPST.
    /// (conformance: delay-var-is-the-maximum-by-default, clean-needs-delay-var-below-low-thresh, seq-err-thresh-always-applied, no-delay-sample-is-never-clean)
    pub(crate) fn classify(&self, status: &StatusPdu) -> Classification {
        let mut seq_errors = status.seq_err_loss;
        if !self.ignore_ooo_dup {
            seq_errors = seq_errors
                .saturating_add(status.seq_err_ooo)
                .saturating_add(status.seq_err_dup);
        }
        let delay_var = if !self.use_ow_del_var {
            (status.rtt_var_sample != STATUS_NODEL).then_some(status.rtt_var_sample)
        } else if status.delay_var_cnt == 0 {
            None
        } else {
            Some(match self.statistic {
                DelayVarStatistic::Average => {
                    let sum = u64::from(status.delay_var_sum);
                    let count = u64::from(status.delay_var_cnt);
                    ((2 * sum + count) / (2 * count)) as u32
                }
                DelayVarStatistic::Maximum => status.delay_var_max,
            })
        };
        let errored = seq_errors > u32::from(self.seq_errors)
            || delay_var.is_some_and(|d| d > u32::from(self.upper));
        let class = if errored {
            IntervalClass::Errored
        } else if delay_var.is_some_and(|d| d < u32::from(self.low)) {
            IntervalClass::Clean
        } else {
            IntervalClass::Hold
        };
        Classification {
            spdu_seq_no: status.spdu_seq_no,
            class,
            seq_errors,
            delay_var,
            row: None,
        }
    }
}

/// How many of a Test Connection's latest Trial Intervals its Client also
/// keeps, one bit each: a search that never confirmed Congestion is
/// judged at its end by these.
pub const LAST_TRIAL_INTERVALS: u32 = 64;

/// How a Test Connection's Trial Intervals classified, as its Client
/// counted them: how many were Clean, Errored and Hold Intervals,
/// whether Congestion was ever confirmed, and how the last
/// [`LAST_TRIAL_INTERVALS`] went.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TrialIntervalCounts {
    pub clean: u32,
    pub errored: u32,
    pub hold: u32,
    /// Of `hold`, those with no delay variation sample: no Load PDU
    /// arrived in order in them (with useOwDelVar off, no RTT sample), as
    /// when the Load Sender stalls or the path drops everything for a
    /// while. No delay variation held them.
    pub hold_without_sample: u32,
    /// slowAdjThresh Errored Intervals came back to back at least once:
    /// Congestion, as the Load Rate Adjustment Algorithm infers it.
    pub congestion_confirmed: bool,
    /// Of the last [`LAST_TRIAL_INTERVALS`] Trial Intervals, the Errored
    /// Intervals.
    pub last_errored: u32,
    /// Of the last [`LAST_TRIAL_INTERVALS`] Trial Intervals, those from the
    /// first Errored Interval among them to the latest, both included,
    /// Clean and Hold Intervals of both kinds too; 0 with no Errored
    /// Interval among them.
    pub last_from_first_errored: u32,
}

impl TrialIntervalCounts {
    /// Every Trial Interval counted.
    pub fn trial_intervals(&self) -> u32 {
        self.clean
            .saturating_add(self.errored)
            .saturating_add(self.hold)
    }
}

/// A Client's count of its Test Connection's Trial Intervals: each
/// Status PDU of the Test that reports one, the Client's own in a
/// Downstream Test and the Server's in an Upstream one, classified with the
/// accepted thresholds as the Server's algorithm classifies it, by the
/// maximum delay variation, the delay range the RFCs judge, whatever
/// statistic the Server uses; the run of Errored Intervals back to back,
/// any other interval ending it, which confirms Congestion at
/// slowAdjThresh (ADR 0005); and the last [`LAST_TRIAL_INTERVALS`], a bit
/// each. The Client can't see the Server's Lost Status Backoffs, which
/// count as Errored Intervals there. Counters and a register only, so a
/// Data Phase allocates nothing for it (ADR 0008).
/// (conformance: latched-congestion-confirmed)
#[derive(Debug, Clone, Copy)]
pub(crate) struct IntervalCounter {
    thresholds: Thresholds,
    slow_adj_thresh: u32,
    /// Errored Intervals back to back.
    errored_run: u32,
    last: LastTrialIntervals,
    counts: TrialIntervalCounts,
}

/// A Test Connection's last [`LAST_TRIAL_INTERVALS`] Trial Intervals, a bit
/// each, the latest lowest: set for an Errored Interval.
#[derive(Debug, Clone, Copy, Default)]
struct LastTrialIntervals(u64);

// A bit each.
const _: () = assert!(LAST_TRIAL_INTERVALS == u64::BITS);

impl LastTrialIntervals {
    /// Adds the latest Trial Interval; the oldest falls out.
    fn push(&mut self, errored: bool) {
        self.0 = (self.0 << 1) | u64::from(errored);
    }

    /// The Errored Intervals among them.
    fn errored(self) -> u32 {
        self.0.count_ones()
    }

    /// Those from the first Errored Interval among them to the latest,
    /// both included: from the highest bit set; 0 with none set.
    fn since_first_errored(self) -> u32 {
        match self.0 {
            0 => 0,
            bits => LAST_TRIAL_INTERVALS - bits.leading_zeros(),
        }
    }
}

impl IntervalCounter {
    /// From the Activation Response, so a Server's coercion counts.
    pub(crate) fn accepted(response: &ActivationPdu) -> Self {
        Self {
            thresholds: Thresholds::accepted(response, DelayVarStatistic::Maximum),
            slow_adj_thresh: response.slow_adj_thresh.into(),
            errored_run: 0,
            last: LastTrialIntervals::default(),
            counts: TrialIntervalCounts::default(),
        }
    }

    /// Counts the Trial Interval `status` reports.
    pub(crate) fn count(&mut self, status: &StatusPdu) {
        let counts = &mut self.counts;
        let classification = self.thresholds.classify(status);
        self.last
            .push(classification.class == IntervalClass::Errored);
        match classification.class {
            IntervalClass::Clean => {
                counts.clean = counts.clean.saturating_add(1);
                self.errored_run = 0;
            }
            IntervalClass::Hold => {
                counts.hold = counts.hold.saturating_add(1);
                if classification.delay_var.is_none() {
                    counts.hold_without_sample = counts.hold_without_sample.saturating_add(1);
                }
                self.errored_run = 0;
            }
            IntervalClass::Errored => {
                counts.errored = counts.errored.saturating_add(1);
                self.errored_run = self.errored_run.saturating_add(1);
                counts.congestion_confirmed |= self.errored_run >= self.slow_adj_thresh;
            }
        }
    }

    /// The counts so far.
    pub(crate) fn counts(&self) -> TrialIntervalCounts {
        TrialIntervalCounts {
            last_errored: self.last.errored(),
            last_from_first_errored: self.last.since_first_errored(),
            ..self.counts
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // : the register keeps a Test Connection's last 64 Trial Intervals:
    // how many of them were Errored Intervals, and how many there are from
    // the first of those to the latest, as a list of the last 64 says,
    // before 64 were counted, while Errored Intervals come and go, and once
    // a stretch longer than 64 had none
    #[test]
    fn the_register_keeps_what_a_list_of_the_last_64_keeps() {
        let mut register = LastTrialIntervals::default();
        let mut list: Vec<bool> = Vec::new();
        let mut x: u64 = 0x2545_F491_4F6C_DD1D;
        for step in 0..2000 {
            // Errored about one time in five, but not in the last 100
            // steps of every 300.
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let errored = step % 300 < 200 && x.is_multiple_of(5);
            register.push(errored);
            list.push(errored);
            let last = &list[list.len().saturating_sub(64)..];
            let errored_last = last.iter().filter(|&&e| e).count() as u32;
            let from_first = last
                .iter()
                .position(|&e| e)
                .map_or(0, |first| (last.len() - first) as u32);
            assert_eq!(
                (register.errored(), register.since_first_errored()),
                (errored_last, from_first),
                "after {} Trial Intervals",
                step + 1
            );
        }
    }
}
