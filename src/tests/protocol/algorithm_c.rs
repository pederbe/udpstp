//! Load Rate Adjustment Algorithm C (RFC 9946 §7, §12.3.8; Y.1540 Amd 2
//! Annex B.4 Figure B.2; docs/spec.md "Load Rate Adjustment Algorithms"; ADR
//! 0005).
//!
//! The row traces are derived by hand from the flowchart text and the spec:
//! - Fast Mode (below the 1 Gbps row, before Congestion Confirmed): each
//!   Clean Interval toggles Update and doubles the row when Update was set,
//!   so the row doubles on every second one; row 0 goes to row 1 first.
//!   When doubling would pass the 1 Gbps row, the row jumps to it without
//!   toggling Update.
//! - Errored Intervals and Congestion Confirmed as in algorithm B: one row
//!   off, and the slowAdjThresh one in a row takes 3 × highSpeedDelta off.
//! - Every single step, up or down, counts toward RetryThresh (5 at
//!   first). At RetryThresh a Fast-Mode Retry clears Congestion Confirmed.
//!   A retry from a step up raises RetryThresh by 5; from a step down it
//!   stays. Update persists across a retry.

use super::harness;

use std::time::Duration;

use crate::protocol::{Classification, DataEvent, Direction, EndReason, RateAdjAlgorithm};
use crate::wire::StatusPdu;
use harness::status_script::{
    DEFAULTS, Params, Report, classifications, clean, errored, hold, reported_then_stopped,
};
use harness::{Outcome, Scenario, TraceEntry};

const C: Params = Params {
    rate_adj_algo: 1,
    ..DEFAULTS
};

fn check_rows(seed: u64, params: Params, reports: Vec<Report>, expected: &[u16]) {
    let (outcome, got) = reported_then_stopped(Scenario::new(seed), params, reports);
    let got: Vec<Option<u16>> = got.iter().map(|c| c.row).collect();
    let expected: Vec<Option<u16>> = expected.iter().copied().map(Some).collect();
    outcome.check(
        got == expected,
        &format!("rows {got:?}, expected {expected:?}"),
    );
}

fn times<T: Clone>(n: usize, item: T) -> Vec<T> {
    vec![item; n]
}

// docs/spec.md (Fast Mode doubles the row index on every second clean
// interval; row 0 goes to row 1 on the first fast pass)
#[test]
fn fast_mode_doubles_the_row_on_every_second_clean_interval() {
    check_rows(1, C, times(6, clean()), &[1, 2, 2, 4, 4, 8]);
}

// docs/spec.md (when doubling would pass the 1 Gbps row, it jumps to that
// row without toggling Update; Update persists across a retry)
#[test]
fn the_jump_to_1_gbps_leaves_update_as_it_was() {
    // highSpeedDelta 255: the confirming drop is 765 rows.
    let params = Params {
        high_speed_delta: 255,
        ..C.starting_at(300)
    };
    let mut reports = times(3, clean());
    reports.extend(times(3, errored()));
    reports.extend(times(5, clean()));
    // 300 (Update set), 600 (cleared), 1000 (the jump; Update stays
    // cleared). 999 and 998 (steps 1 and 2), then Congestion: 233. Steps
    // 3-5 up to 236 retry. Fast Mode again from Update cleared: 236 (set),
    // then 472.
    check_rows(
        2,
        params,
        reports,
        &[300, 600, 1000, 999, 998, 233, 234, 235, 236, 236, 472],
    );
}

// docs/spec.md (Fast-Mode Retry: RetryThresh starts at 5 and grows by 5; a
// retry clears Congestion Confirmed)
#[test]
fn retries_come_after_5_then_10_then_15_single_steps_up() {
    let mut reports = times(3, errored());
    reports.extend(times(5, clean()));
    reports.extend(times(3, errored()));
    reports.extend(times(10, clean()));
    reports.extend(times(3, errored()));
    reports.extend(times(15, clean()));
    let mut expected = vec![49, 48, 18];
    // Steps 3-5 retry at 21 (RetryThresh 10); Fast Mode: 21, 42.
    expected.extend([19, 20, 21, 21, 42]);
    // Steps 1-2, then Congestion again (the retry cleared it): 10.
    expected.extend([41, 40, 10]);
    // Steps 3-10 retry at 18 (RetryThresh 15); Fast Mode: 18, 36.
    expected.extend([11, 12, 13, 14, 15, 16, 17, 18, 18, 36]);
    // Steps 1-2, Congestion: 4; steps 3-15 retry at 17; Fast Mode: 17, 34.
    expected.extend([35, 34, 4]);
    expected.extend((5..=17).chain([17, 34]));
    check_rows(3, C.starting_at(50), reports, &expected);
}

// docs/spec.md (Update persists across a retry)
#[test]
fn update_persists_across_a_retry() {
    let mut reports = vec![clean()];
    reports.extend(times(3, errored()));
    reports.extend(times(4, clean()));
    // 50 with Update set; 49, 48, Congestion 18; 19, 20, 21 (steps 3-5:
    // a retry); Fast Mode doubles at once, Update being still set: 42.
    check_rows(
        9,
        C.starting_at(50),
        reports,
        &[50, 49, 48, 18, 19, 20, 21, 42],
    );
}

// docs/spec.md (a Hold Interval changes nothing: it neither toggles Update
// nor counts toward RetryThresh)
#[test]
fn a_hold_interval_changes_nothing() {
    let mut reports = vec![clean(), hold(), clean()];
    reports.extend(times(3, errored()));
    reports.extend([clean(), hold(), clean(), clean(), clean(), clean()]);
    // 50 (Update set), 50, 100; 99, 98 (steps 1-2), Congestion 68; 69,
    // 69, 70, 71 (steps 3-5: a retry), then 71 (Update set), 142.
    check_rows(
        10,
        C.starting_at(50),
        reports,
        &[50, 50, 100, 99, 98, 68, 69, 69, 70, 71, 71, 142],
    );
}

/// The default table's highest row: 12 Gbps as bursts of 100 1500-byte
/// packets, the Server's default MTU.
const LAST_ROW: u16 = 1069;

/// Algorithm C as the flowchart (Y.1540 Amd 2 Figure B.2, research §2.3)
/// and docs/spec.md describe it, written here independently of the Server.
fn reference(start: u16, classes: &[u8]) -> Vec<u16> {
    let (mut row, mut run, mut confirmed) = (start, 0u32, false);
    let (mut count, mut thresh, mut update) = (0u32, 5u32, false);
    let mut rows = Vec::new();
    for &class in classes {
        match class {
            // Clean.
            0 => {
                run = 0;
                if !confirmed && row < 1000 {
                    if row * 2 > 1000 {
                        row = 1000;
                    } else {
                        row = row.max(1);
                        if update {
                            row *= 2;
                        }
                        update = !update;
                    }
                } else if row < LAST_ROW {
                    row += 1;
                    count += 1;
                    if count >= thresh {
                        (count, confirmed, run) = (0, false, 0);
                        thresh += 5;
                    }
                }
            }
            // Errored.
            1 => {
                run += 1;
                if !confirmed && run >= 3 && row < 1000 {
                    confirmed = true;
                    row = row.saturating_sub(30);
                } else {
                    confirmed |= run >= 3;
                    if row > 0 {
                        row -= 1;
                        count += 1;
                        if count >= thresh {
                            (count, confirmed, run) = (0, false, 0);
                        }
                    }
                }
            }
            // Hold.
            _ => run = 0,
        }
        rows.push(row);
    }
    rows
}

// docs/spec.md  (a property test: the rule matches OB-UDPST's; below
// the 1 Gbps row every table variant doubles the rate with the index, as
// the table's own tests show)
#[test]
fn random_interval_sequences_follow_the_flowchart() {
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    };
    for case in 0..24 {
        let start = (next() % 1001) as u16;
        // Mostly clean, so Fast Mode and retries come round.
        let classes: Vec<u8> = (0..40)
            .map(|_| match next() % 10 {
                0..=5 => 0,
                6..=8 => 1,
                _ => 2,
            })
            .collect();
        let reports = classes
            .iter()
            .map(|c| match c {
                0 => clean(),
                1 => errored(),
                _ => hold(),
            })
            .collect();
        let expected = reference(start, &classes);
        check_rows(100 + case, C.starting_at(start), reports, &expected);
    }
}

// docs/spec.md (a retry from a single step down keeps RetryThresh)
#[test]
fn a_retry_from_a_step_down_keeps_retry_thresh() {
    let mut reports = times(9, errored());
    reports.extend(times(5, clean()));
    // 49, 48 (steps 1-2), Congestion 18; 17, 16, 15 (steps 3-5: a retry,
    // RetryThresh stays 5); 14, 13 (steps 1-2), Congestion again: 0.
    // 1, 2, 3 (steps 3-5: a retry, now RetryThresh 10); Fast Mode: 3, 6.
    check_rows(
        4,
        C.starting_at(50),
        reports,
        &[49, 48, 18, 17, 16, 15, 14, 13, 0, 1, 2, 3, 3, 6],
    );
}

const MBPS: u64 = 1_000_000;

/// IP-layer bit/s of the Sending Rate Structure a Status PDU carries.
fn ip_layer_bps(s: &StatusPdu) -> u64 {
    let sr = &s.sending_rate;
    let per_second = |bytes: u32, count: u32, interval_us: u32| {
        if bytes == 0 || count == 0 || interval_us == 0 {
            return 0;
        }
        u64::from((bytes + 28) * 8 * count) * 1_000_000 / u64::from(interval_us)
    };
    per_second(sr.udp_payload1, sr.burst_size1, sr.tx_interval1)
        + per_second(sr.udp_payload2, sr.burst_size2, sr.tx_interval2)
        + per_second(sr.udp_addon2, 1, sr.tx_interval2)
}

fn search(seed: u64, direction: Direction) -> Outcome {
    let scenario = Scenario::new(seed)
        .server_allows_unauthenticated()
        .client_unauthenticated()
        .client_direction(direction)
        .client_config(|c| c.algorithm = RateAdjAlgorithm::C)
        .bottleneck(50 * MBPS, 62_500)
        .path_delay(Duration::from_millis(5), Duration::ZERO);
    scenario.run_for(Duration::from_secs(6))
}

// docs/spec.md  (a search with algorithm C converges near a known
// bottleneck, in both directions)
#[udpstp_req::req("RFC9946-8.1-8")]
#[test]
fn a_search_with_algorithm_c_converges_near_the_bottleneck() {
    for (seed, direction) in [(5, Direction::Downstream), (6, Direction::Upstream)] {
        let outcome = search(seed, direction);
        let rows: Vec<u16> = classifications(&outcome)
            .iter()
            .skip(60)
            .filter_map(|c| c.row)
            .collect();
        let ended = outcome
            .server_data_events
            .iter()
            .any(|(_, e)| matches!(e, DataEvent::Ended(r) if *r != EndReason::Completed));
        // Algorithm C keeps retrying Fast Mode, so it saws around the
        // bottleneck rather than settling: within about twice it, and
        // centred on it.
        let mut sorted = rows.clone();
        sorted.sort_unstable();
        let median = sorted.get(sorted.len() / 2).copied().unwrap_or_default();
        // Unlike B, C confirms Congestion again after each retry.
        let all: Vec<u16> = classifications(&outcome)
            .iter()
            .filter_map(|c| c.row)
            .collect();
        let drops = all.windows(2).filter(|w| w[1] + 10 < w[0]).count();
        outcome.check(
            !ended
                && rows.len() > 40
                && rows.iter().all(|row| (15..=110).contains(row))
                && (40..=60).contains(&median)
                && drops > 1,
            &format!("{direction:?}: median {median}, {drops} drops, rows after 3 s {rows:?}"),
        );
    }
}

// docs/spec.md  (doubling the row index doubles the rate: the rule
// matches OB-UDPST's index doubling on our table)
#[test]
fn doubling_the_row_doubles_the_rate() {
    let outcome = search(7, Direction::Upstream);
    let got: Vec<Classification> = classifications(&outcome);
    let sent: Vec<StatusPdu> = outcome
        .trace
        .iter()
        .filter(|t: &&TraceEntry| t.len == 204 && t.payload.starts_with(&[0xFE, 0xED]))
        .filter(|t| t.destination == harness::CLIENT_ADDR.parse().unwrap())
        .filter_map(|t| StatusPdu::decode(&t.payload).ok())
        .filter(|s| s.test_action == 0)
        .collect();
    let rows: Vec<(u16, u64)> = got
        .iter()
        .zip(&sent)
        .filter(|(c, s)| c.spdu_seq_no == s.spdu_seq_no)
        .filter_map(|(c, s)| Some((c.row?, ip_layer_bps(s))))
        .collect();
    let doublings: Vec<_> = rows
        .windows(2)
        .filter(|w| w[0].0 > 0 && w[1].0 == 2 * w[0].0)
        .collect();
    outcome.check(
        doublings.len() >= 4 && doublings.iter().all(|w| w[1].1 == 2 * w[0].1),
        &format!("{doublings:?}"),
    );
}

// docs/spec.md (only a single step counts toward RetryThresh: a Clean
// Interval at the table's last row takes none)
#[test]
fn clean_intervals_at_the_last_row_are_not_steps() {
    let mut reports = times(5, clean());
    reports.extend(times(3, errored()));
    reports.extend(times(4, clean()));
    // The last row five times (no steps); 1068, 1067 (steps 1-2), then
    // Congestion above the 1 Gbps row takes one row: 1066 (step 3); 1067,
    // 1068 (steps 4-5: a retry); 1069; then the last row again, no step.
    check_rows(
        8,
        C.starting_at(LAST_ROW),
        reports,
        &[
            1069, 1069, 1069, 1069, 1069, 1068, 1067, 1066, 1067, 1068, 1069, 1069,
        ],
    );
}

// docs/spec.md  (at or above the 1 Gbps row, the Errored Interval that
// confirms Congestion can also be the step that reaches RetryThresh: the
// latch is set and cleared in one interval);
#[test]
fn a_confirming_step_above_the_gigabit_row_can_retry() {
    let mut reports = times(2, clean());
    reports.extend(times(3, errored()));
    reports.extend([clean(), errored()]);
    // 1011, 1012 (steps 1-2); 1011, 1010 (steps 3-4); the third Errored
    // Interval confirms and steps to 1009, step 5: a retry, which clears
    // Congestion and the run; 1010; one Errored Interval confirms nothing.
    check_rows(
        9,
        C.starting_at(1010),
        reports,
        &[1011, 1012, 1011, 1010, 1009, 1010, 1009],
    );
}
