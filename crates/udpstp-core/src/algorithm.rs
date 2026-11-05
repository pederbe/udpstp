//! Load Rate Adjustment Algorithms B and C (RFC 9946 §7; Y.1540 Amd 2
//! Annex B, Figures B.1 and B.2; docs/spec.md "Load Rate Adjustment
//! Algorithms"; ADR 0005), defined on Sending Rate Table rows.
//!
//! Each classified Trial Interval moves the row:
//! - In Fast Mode, below the 1 Gbps row and before Congestion Confirmed, a
//!   Clean Interval adds highSpeedDelta rows (algorithm B), or doubles the
//!   row on every second one (algorithm C, which takes row 0 to row 1
//!   first); either is capped at the 1 Gbps row.
//! - In Single-Step Mode every change is one row.
//! - An Errored Interval takes one row off, unless it is the slowAdjThresh
//!   one in a row that confirms Congestion below the 1 Gbps row: that takes
//!   3 × highSpeedDelta rows off.
//! - A Hold Interval changes nothing.
//!
//! Any interval that isn't errored resets the count of back-to-back Errored
//! Intervals, and Congestion Confirmed is latched (ADR 0005). Algorithm B
//! never clears it, so it never leaves Single-Step Mode. Algorithm C counts
//! its single steps and, at RetryThresh of them, clears it in a Fast-Mode
//! Retry; RetryThresh starts at 5 and grows by 5 after a retry from a
//! single step up. Rows stay between 0 and the Test's highest row: the
//! table's last, or the one matching the Client's stated maxBandwidth.

use std::sync::Arc;
use std::time::Duration;

use udpstp_wire::activation::rate_adj_algo;
use udpstp_wire::{ActivationPdu, SendingRate};

use crate::interval::IntervalClass;
use crate::sending_rate_table::{GIGABIT_ROW, SendingRateTable};

/// The Server's rate adjustment for one Test Connection: the algorithm,
/// the table its rows come from, and Lost Status Backoff (RFC 9097 §8.1).
#[derive(Debug, Clone)]
pub(crate) struct RateAdjustment {
    algorithm: Algorithm,
    table: Arc<SendingRateTable>,
    /// upperThresh (UDRT).
    upper: Duration,
    /// trialInt (FT).
    trial: Duration,
    /// Lost Status Backoff timeouts since a Status PDU last arrived (w).
    backoffs: u32,
}

impl RateAdjustment {
    /// The accepted algorithm from `row`, bounded by `max_row`, which is
    /// within the table.
    /// `rtt` estimates the path's round-trip time before any Status PDU.
    pub(crate) fn new(
        row: u16,
        response: &ActivationPdu,
        table: Arc<SendingRateTable>,
        max_row: u16,
        rtt: Duration,
    ) -> Self {
        debug_assert!(max_row <= table.last_index());
        let algorithm = Algorithm::new(row, response, max_row);
        let upper = Duration::from_millis(response.upper_thresh.into());
        let trial = Duration::from_millis(response.trial_int.into());
        Self {
            algorithm,
            table,
            upper,
            trial,
            backoffs: initial_backoffs(rtt, upper, trial),
        }
    }

    /// How long after the last Status PDU the next Lost Status Backoff
    /// falls due: UDRT + (2 + w) × FT (RFC 9097 §8.1), w starting from the
    /// path's round-trip time (see [`initial_backoffs`]).
    pub(crate) fn lost_status_timeout(&self) -> Duration {
        self.upper + self.trial * 2u32.saturating_add(self.backoffs)
    }

    /// A Status PDU arrived: w starts over at 0, the round trip being past.
    pub(crate) fn status_heard(&mut self) {
        self.backoffs = 0;
    }

    /// The Lost Status Backoff timeout expired: the rate decreases "following
    /// the same process" as for errored feedback, "with the same load rate
    /// adjustment algorithm variables in their current state" (RFC 9097
    /// §8.1), so it counts as an Errored Interval: it adds to the run of
    /// Errored Intervals and can confirm Congestion, with the one-time
    /// larger drop (ADR 0005, as amended). Returns the row, and its rate if
    /// it changed (conformance: lost-status-backoff-is-an-errored-interval).
    pub(crate) fn back_off(&mut self) -> (u16, Option<SendingRate>) {
        self.backoffs = self.backoffs.saturating_add(1);
        self.on_interval(IntervalClass::Errored)
    }

    /// The row chosen from a classified Trial Interval, and its rate if the
    /// row changed.
    pub(crate) fn on_interval(&mut self, class: IntervalClass) -> (u16, Option<SendingRate>) {
        let before = self.algorithm.row();
        let row = self.algorithm.adjust(class);
        (row, (row != before).then(|| self.rate(row)))
    }

    fn rate(&self, row: u16) -> SendingRate {
        self.table
            .row(row)
            .expect("the algorithm keeps its row within the table")
    }
}

/// The Status PDU timeout, after which the Load Sender stops (RFC 9946
/// §5.1 item 9).
const STATUS_PDU_TIMEOUT: Duration = Duration::from_secs(1);

/// The initial w of Lost Status Backoff for a path whose round-trip time
/// is about `rtt` (RFC 9097 §8.1: "A test with an RTT longer than FT is a
/// valid reason to increase the initial value of w appropriately"). The
/// first Status PDU arrives about one RTT plus one FT after activation,
/// so w starts at RTT / FT, rounded up; with UDRT that leaves at least a
/// Trial Interval to spare. It is capped so the first backoff still comes
/// before the Status PDU timeout stops the sending.
/// (conformance: lost-status-backoff-initial-w)
fn initial_backoffs(rtt: Duration, upper: Duration, trial: Duration) -> u32 {
    let trial = trial.as_nanos().max(1);
    let wanted = rtt.as_nanos().div_ceil(trial);
    // The largest w with UDRT + (2 + w) × FT still under the timeout.
    let room = STATUS_PDU_TIMEOUT.saturating_sub(upper).as_nanos();
    let most = (room.saturating_sub(1) / trial).saturating_sub(2);
    u32::try_from(wanted.min(most)).unwrap_or(0)
}

/// RetryThresh at the start, and its growth after a retry from a single
/// step up (RETRY_THRESH_ALGOC in the flowchart and OB-UDPST).
const RETRY_THRESH_STEP: u32 = 5;

/// Algorithm C's own state.
#[derive(Debug, Clone)]
struct FastModeRetry {
    /// Single steps since the last Fast-Mode Retry (RetryCount).
    count: u32,
    /// RetryThresh.
    thresh: u32,
    /// Whether the last Fast Mode pass held the row, so the next doubles it
    /// (Update). It toggles on each pass and persists across a retry.
    update: bool,
}

/// The algorithm's state for one Test Connection.
#[derive(Debug, Clone)]
pub(crate) struct Algorithm {
    row: u16,
    /// The highest row: the table's last, or the one matching the stated
    /// maxBandwidth.
    max_row: u16,
    high_speed_delta: u16,
    slow_adj_thresh: u32,
    /// Errored Intervals back to back.
    errored_run: u32,
    congestion_confirmed: bool,
    /// Algorithm C's Fast-Mode Retry state; `None` for algorithm B.
    fast_mode_retry: Option<FastModeRetry>,
}

impl Algorithm {
    /// The accepted algorithm, starting at `row`, with the accepted
    /// Activation parameters, up to `max_row`.
    pub(crate) fn new(row: u16, response: &ActivationPdu, max_row: u16) -> Self {
        Self {
            row: row.min(max_row),
            max_row,
            high_speed_delta: response.high_speed_delta.into(),
            slow_adj_thresh: response.slow_adj_thresh.into(),
            errored_run: 0,
            congestion_confirmed: false,
            fast_mode_retry: (response.rate_adj_algo == rate_adj_algo::C).then_some(
                FastModeRetry {
                    count: 0,
                    thresh: RETRY_THRESH_STEP,
                    update: false,
                },
            ),
        }
    }

    /// Algorithm C: counts a single step toward RetryThresh, and at it
    /// retries Fast Mode, clearing Congestion Confirmed and the run of
    /// Errored Intervals (OB-UDPST's SlowAdjCount = 0). After a retry from
    /// a step up RetryThresh grows by 5; after one from a step down it
    /// stays, as in the flowchart footer and OB-UDPST. Only a step that
    /// moved the row counts, and Update is left as it is.
    /// (conformance: retry-thresh-grows-by-5-after-a-step-up, retry-count-counts-actual-single-steps, fast-mode-retry-resets-the-errored-run, update-persists-across-a-fast-mode-retry)
    fn count_step(&mut self, stepped: bool, up: bool) {
        let Some(retry) = &mut self.fast_mode_retry else {
            return;
        };
        if !stepped {
            return;
        }
        retry.count += 1;
        if retry.count >= retry.thresh {
            retry.count = 0;
            if up {
                retry.thresh += RETRY_THRESH_STEP;
            }
            self.congestion_confirmed = false;
            self.errored_run = 0;
        }
    }

    /// Algorithm C's Fast Mode pass from `row`: double it on every second
    /// pass, starting from row 1, or jump to the 1 Gbps row when doubling
    /// would pass it, without toggling Update.
    /// (conformance: algorithm-c-doubles-the-row-index, fast-mode-stops-at-the-gigabit-row)
    fn multiplied(row: u16, retry: &mut FastModeRetry) -> u16 {
        if u32::from(row) * 2 > u32::from(GIGABIT_ROW) {
            return GIGABIT_ROW;
        }
        let row = row.max(1);
        retry.update = !retry.update;
        if retry.update { row } else { row * 2 }
    }

    /// The row to send at.
    pub(crate) fn row(&self) -> u16 {
        self.row
    }

    /// Adjusts the row for a classified Trial Interval, and returns it.
    /// Fast Mode stops at the 1 Gbps row; slowAdjThresh Errored Intervals
    /// back to back latch Congestion Confirmed, and the confirming one takes
    /// 3 × highSpeedDelta rows off only below the 1 Gbps row.
    /// (conformance: fast-mode-stops-at-the-gigabit-row, latched-congestion-confirmed, confirming-drop-below-the-gigabit-row-only)
    pub(crate) fn adjust(&mut self, class: IntervalClass) -> u16 {
        let fast = !self.congestion_confirmed && self.row < GIGABIT_ROW;
        match class {
            IntervalClass::Clean => {
                self.errored_run = 0;
                if fast {
                    let row = match &mut self.fast_mode_retry {
                        Some(retry) => Self::multiplied(self.row, retry),
                        None => self
                            .row
                            .saturating_add(self.high_speed_delta)
                            .min(GIGABIT_ROW),
                    };
                    self.row = row.min(self.max_row);
                } else {
                    let stepped = self.row < self.max_row;
                    if stepped {
                        self.row += 1;
                    }
                    self.count_step(stepped, true);
                }
            }
            IntervalClass::Hold => self.errored_run = 0,
            IntervalClass::Errored => {
                self.errored_run = self.errored_run.saturating_add(1);
                let confirms =
                    !self.congestion_confirmed && self.errored_run >= self.slow_adj_thresh;
                self.congestion_confirmed |= confirms;
                if confirms && self.row < GIGABIT_ROW {
                    self.row = self
                        .row
                        .saturating_sub(self.high_speed_delta.saturating_mul(3));
                } else if self.row > 0 {
                    self.row -= 1;
                    self.count_step(true, false);
                }
            }
        }
        self.row
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: fn(u64) -> Duration = Duration::from_millis;

    #[test]
    fn the_initial_w_covers_the_round_trip() {
        // RTT / FT rounded up.
        assert_eq!(initial_backoffs(MS(0), MS(90), MS(50)), 0);
        assert_eq!(initial_backoffs(MS(40), MS(90), MS(50)), 1);
        assert_eq!(initial_backoffs(MS(600), MS(90), MS(50)), 12);
    }

    #[test]
    fn the_initial_w_keeps_the_first_backoff_before_the_status_pdu_timeout() {
        // 90 + (2 + 16) × 50 = 990 ms; 17 would be 1040.
        assert_eq!(initial_backoffs(MS(3000), MS(90), MS(50)), 16);
        // An upperThresh near the timeout leaves no room.
        assert_eq!(initial_backoffs(MS(3000), MS(990), MS(50)), 0);
    }
}
