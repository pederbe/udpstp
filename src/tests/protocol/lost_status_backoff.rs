//! Lost Status Backoff (RFC 9097 §8.1; docs/spec.md "Load Rate Adjustment
//! Algorithms"; ADR 0005).
//!
//! In a Downstream Search Test, when no Status PDU has arrived for
//! UDRT + (2 + w) × FT since the last one, the Server backs off one row,
//! and w counts the timeouts since then. With the defaults (upperThresh
//! 90 ms, trialInt 50 ms) that is at 190 ms, 240 ms, 290 ms, and so on,
//! until the 1 s Status PDU timeout stops the sending. A Status PDU, stale
//! or not, starts it over. A backoff counts as an Errored Interval (RFC
//! 9097 §8.1 "lost status feedback messages ... can result in rate
//! reduction and congestion confirmation"): the third in a row confirms
//! Congestion, with the one-time drop of 3 × highSpeedDelta rows.

use super::harness;

use std::time::Duration;

use crate::protocol::DataEvent;
use harness::status_script::{DEFAULTS, Report, clean, errored, reported_at};
use harness::{Outcome, Scenario};

const MS: fn(u64) -> Duration = Duration::from_millis;

/// The Server's backoffs as (ms after the first Status PDU arrived, row).
fn backoffs(outcome: &Outcome) -> Vec<(u128, u16)> {
    let first_status = outcome
        .trace
        .iter()
        .find(|t| t.len == 204 && t.payload.starts_with(&[0xFE, 0xED]))
        .map(|t| t.at)
        .unwrap_or_default();
    outcome
        .server_data_events
        .iter()
        .zip(&outcome.server_data_event_times)
        .filter_map(|((_, e), at)| match e {
            DataEvent::LostStatusBackoff { row } => {
                Some(((*at - first_status).as_micros().div_ceil(1000), *row))
            }
            _ => None,
        })
        .collect()
}

fn rows(outcome: &Outcome) -> Vec<u16> {
    outcome
        .server_data_events
        .iter()
        .filter_map(|(_, e)| match e {
            DataEvent::TrialIntervalClassified(c) => c.row,
            DataEvent::LostStatusBackoff { row } => Some(*row),
            _ => None,
        })
        .collect()
}

#[udpstp_req::req(
    "RFC9097-8.1-L18",
    "RFC9097-8.1-L19",
    "RFC9097-8.1-13",
    "RFC9097-8.1-14",
    "RFC9097-8.1-15",
    "RFC9097-8.1-L21",
    "RFC9097-8.1-L22"
)]
#[test]
fn lost_status_pdus_back_the_rate_off_and_confirm_congestion() {
    let outcome = reported_at(
        Scenario::new(1),
        DEFAULTS.starting_at(50),
        vec![(MS(0), clean())],
        MS(1500),
    );
    // At 190 ms, then every 50 ms up to 990 ms: 17 backoffs from row 60,
    // 59 and 58, then 58 - 30 and single steps down.
    let expected: Vec<(u128, u16)> = (0..17)
        .map(|k| {
            let row = if k < 2 { 59 - k } else { 28 - (k - 2) };
            (190 + 50 * u128::from(k), row)
        })
        .collect();
    let got = backoffs(&outcome);
    outcome.check(got == expected, &format!("{got:?}"));

    // The Server really sends slower: 60 Mbit/s before the first backoff,
    // 15 after the 16th.
    let first_status = outcome
        .trace
        .iter()
        .find(|t| t.len == 204 && t.payload.starts_with(&[0xFE, 0xED]))
        .map(|t| t.at)
        .unwrap_or_default();
    let mbps = |from: u64, to: u64| {
        let bytes: usize = outcome
            .trace
            .iter()
            .filter(|t| t.payload.starts_with(&[0xBE, 0xEF]))
            .filter(|t| t.at >= first_status + MS(from) && t.at < first_status + MS(to))
            .map(|t| t.len + 28)
            .sum();
        bytes as f64 * 8.0 / ((to - from) as f64 * 1000.0)
    };
    let (before, after) = (mbps(40, 190), mbps(950, 990));
    outcome.check(
        (before - 60.0).abs() < 0.5 && (after - 15.0).abs() < 0.5,
        &format!("{before} Mbit/s, then {after}"),
    );
}

// docs/spec.md (no Lost Status Backoff in Upstream Tests, where the Server
// receives the Load PDUs)
#[test]
fn an_upstream_server_does_not_back_off() {
    let outcome = Scenario::new(5)
        .server_allows_unauthenticated()
        .client_unauthenticated()
        .client_direction(crate::protocol::Direction::Upstream)
        .client_requests_row(harness::RowRequest::StartAt(20))
        .network_drops([0xBE, 0xEF], MS(300), Some(MS(700)))
        .run_for(MS(1000));
    let got = backoffs(&outcome);
    outcome.check(got.is_empty(), &format!("{got:?}"));
}

// docs/spec.md "Data Phase" (a stale Status PDU resets Lost Status Backoff
// state)
#[udpstp_req::req("RFC9097-8.1-L19")]
#[test]
fn any_status_pdu_starts_the_backoff_over() {
    let stale = Report {
        spdu_seq_no: Some(1),
        ..clean()
    };
    let outcome = reported_at(
        Scenario::new(2),
        DEFAULTS.starting_at(50),
        vec![(MS(0), clean()), (MS(300), stale)],
        MS(1000),
    );
    let got: Vec<u128> = backoffs(&outcome).iter().map(|(at, _)| *at).collect();
    // 190, 240 and 290; the stale Status PDU at 300 ms starts over: 490,
    // 540...
    let expected: Vec<u128> = [190, 240, 290]
        .into_iter()
        .chain((490..=990).step_by(50))
        .collect();
    let near =
        got.len() == expected.len() && got.iter().zip(&expected).all(|(g, e)| g.abs_diff(*e) <= 1);
    outcome.check(near, &format!("{got:?}"));
}

// ADR 0005 as amended
#[udpstp_req::req("RFC9097-8.1-13", "RFC9097-8.1-L21", "RFC9097-8.1-L22")]
#[test]
fn a_backoff_counts_as_an_errored_interval() {
    let outcome = reported_at(
        Scenario::new(3),
        DEFAULTS.starting_at(50),
        vec![
            (MS(0), clean()),
            (MS(400), errored()),
            (MS(450), errored()),
            (MS(500), errored()),
        ],
        MS(560),
    );
    // Row 60; backoffs at 190 and 240 ms take a row off each, the third at
    // 290 ms confirms Congestion (58 - 30), and the ones at 340 and 390 ms
    // and the Errored Intervals after them are single steps.
    let got = rows(&outcome);
    outcome.check(
        got == [60, 59, 58, 28, 27, 26, 25, 24, 23],
        &format!("{got:?}"),
    );
}

// docs/spec.md (Lost Status Backoff only in Downstream Search Tests; no
// backoff in a Fixed-Rate Test, which holds its row)
#[test]
fn a_fixed_rate_test_does_not_back_off() {
    let outcome = Scenario::new(4)
        .operator(7, b"an operator key")
        .client_direction(crate::protocol::Direction::Downstream)
        .client_requests_row(harness::RowRequest::FixedRate(5))
        .network_drops([0xFE, 0xED], MS(200), Some(MS(700)))
        .run_for(MS(1000));
    let got = backoffs(&outcome);
    outcome.check(got.is_empty(), &format!("{got:?}"));
}

// RFC 9097 §8.1 (rows never below row 0); docs/spec.md (Fast Mode goes on from
// the lower row while fewer than slowAdjThresh backoffs came in a row)
#[udpstp_req::req("RFC9097-8.1-L21")]
#[test]
fn backoffs_stop_at_row_0_and_two_in_a_row_leave_fast_mode_on() {
    let outcome = reported_at(
        Scenario::new(6),
        DEFAULTS.starting_at(1),
        vec![(MS(0), clean())],
        MS(1500),
    );
    let got = rows(&outcome);
    // 10 and 9, then the third confirms Congestion: 9 - 30 stops at 0.
    let expected: Vec<u16> = [11, 10, 9].into_iter().chain([0; 15]).collect();
    outcome.check(got == expected, &format!("{got:?}"));

    let outcome = reported_at(
        Scenario::new(7),
        DEFAULTS.starting_at(50),
        vec![(MS(0), clean()), (MS(260), clean())],
        MS(280),
    );
    let got = rows(&outcome);
    outcome.check(got == [60, 59, 58, 68], &format!("{got:?}"));
}

//
#[udpstp_req::req("RFC9097-8.1-L20")]
#[test]
fn a_long_path_gets_no_backoff_before_its_first_status_pdu() {
    // 300 ms each way: the first Status PDU reaches the Server about 650 ms
    // after activation. With w starting at 0 the Server would back off at
    // 190, 240 and 290 ms and confirm Congestion; the Setup Response's
    // round trip (600 ms) starts w at 12, so the first timeout is at
    // 90 + 14 × 50 = 790 ms.
    let outcome = Scenario::new(8)
        .operator(7, b"an operator key")
        .path_delay(MS(300), Duration::ZERO)
        .run_for(MS(2500));
    let backoffs = outcome
        .server_data_events
        .iter()
        .filter(|(_, e)| matches!(e, DataEvent::LostStatusBackoff { .. }))
        .count();
    let got = rows(&outcome);
    outcome.check(
        backoffs == 0 && got.starts_with(&[10, 20, 30]),
        &format!("{backoffs} backoffs; rows {got:?}"),
    );
}
