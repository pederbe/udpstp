//! A Client's count of its Trial Intervals: each Status PDU of the
//! Test that reports one, classified with the accepted thresholds as the
//! Server's algorithm classifies it (RFC 9946 §5.1(6)), by the maximum
//! delay variation, the RFCs' "delay range", whatever the Server's own
//! statistic; whether slowAdjThresh Errored Intervals ever came back to
//! back, any other interval ending the run: Congestion (GLOSSARY.md, ADR
//! 0005); and, of the last 64, the Errored Intervals and those from the
//! first of them on. The Client reports the counts once, as its Data Phase
//! ends.
//!
//! A scripted Server reports chosen Trial Intervals to our Client in an
//! Upstream Test (`status_script::reported_to_client`), so the expected
//! counts follow from the reports and the default thresholds (lowThresh
//! 30 ms, upperThresh 90 ms, seqErrThresh 10, slowAdjThresh 3), not from
//! our classification. Downstream, the Client counts the Status PDUs it
//! sends, which the Server classifies as they arrive: its counts are
//! checked against the Server's classifications and rows.

use super::harness;

use std::time::Duration;

use crate::protocol::{DataEvent, Direction, EndReason, IntervalClass, TrialIntervalCounts};
use harness::Scenario;
use harness::status_script::{
    Report, classifications, clean, delay, errored, hold, reported_to_client,
};

/// The counts the Client reported, and whether its Data Phase then ended.
fn counted(seed: u64, reports: Vec<Report>) -> (harness::Outcome, Vec<TrialIntervalCounts>) {
    let outcome = reported_to_client(Scenario::new(seed), reports);
    let events: Vec<DataEvent> = outcome.client_data_events.iter().map(|(_, e)| *e).collect();
    let counts: Vec<TrialIntervalCounts> = events
        .iter()
        .filter_map(|e| match e {
            DataEvent::TrialIntervalsCounted(counts) => Some(*counts),
            _ => None,
        })
        .collect();
    // Once, just before the end.
    let last_two: Vec<&DataEvent> = events.iter().rev().take(2).collect();
    outcome.check(
        matches!(
            last_two.as_slice(),
            [
                DataEvent::Ended(EndReason::Completed),
                DataEvent::TrialIntervalsCounted(_)
            ]
        ),
        &format!("the counts come once, just before the end: {events:?}"),
    );
    (outcome, counts)
}

/// Counts with every Hold Interval's delay sampled, and none of the last
/// Trial Intervals errored.
fn counts(clean: u32, errored: u32, hold: u32, congestion_confirmed: bool) -> TrialIntervalCounts {
    TrialIntervalCounts {
        clean,
        errored,
        hold,
        hold_without_sample: 0,
        congestion_confirmed,
        last_errored: 0,
        last_from_first_errored: 0,
    }
}

/// `counts` whose last Trial Intervals held `errored` Errored Intervals,
/// `from_first` of them from the first of those on.
fn ending(counts: TrialIntervalCounts, errored: u32, from_first: u32) -> TrialIntervalCounts {
    TrialIntervalCounts {
        last_errored: errored,
        last_from_first_errored: from_first,
        ..counts
    }
}

/// A script of Trial Intervals, a letter each: C clean, E errored, H a Hold
/// Interval with a delay sample, h one without.
fn script(pattern: &str) -> Vec<Report> {
    pattern
        .chars()
        .map(|interval| match interval {
            'C' => clean(),
            'E' => errored(),
            'H' => hold(),
            'h' => delay(0, 0),
            other => panic!("no Trial Interval {other:?}"),
        })
        .collect()
}

// ; ADR 0005 (slowAdjThresh Errored Intervals back to back confirm
// Congestion; any other interval ends the run)
#[test]
fn back_to_back_errored_intervals_confirm_congestion() {
    let (outcome, got) = counted(1, vec![clean(), errored(), errored(), errored(), clean()]);
    outcome.check(
        got == [ending(counts(2, 3, 0, true), 3, 4)],
        &format!("counted {got:?}"),
    );
    // Two, then a clean one and a hold one between: the run starts over.
    let (outcome, got) = counted(
        2,
        vec![
            errored(),
            errored(),
            clean(),
            errored(),
            errored(),
            hold(),
            errored(),
            errored(),
        ],
    );
    outcome.check(
        got == [ending(counts(1, 6, 1, false), 6, 8)],
        &format!("counted {got:?}"),
    );
}

// ; ADR 0005 (errored, hold, errored, hold, errored doesn't confirm
// Congestion, where OB-UDPST's single SlowAdjCount would)
#[test]
fn intermittent_errored_intervals_confirm_nothing() {
    let (outcome, got) = counted(
        3,
        vec![errored(), hold(), errored(), hold(), errored(), clean()],
    );
    outcome.check(
        got == [ending(counts(1, 3, 2, false), 3, 6)],
        &format!("counted {got:?}"),
    );
}

// : a Client also keeps its last 64 Trial Intervals, and counts how
// many of them were Errored Intervals and how many there are from the first
// of those to the latest, Clean Intervals and Hold Intervals of both kinds
// included: a search that never confirmed Congestion is judged at its end
// by them (the maintainer's decision on , 2026-09-29)
#[test]
fn the_last_trial_intervals_count_from_the_first_errored_one() {
    let last = |seed: u64, pattern: &str| {
        let (outcome, got) = counted(seed, script(pattern));
        let [counts] = got[..] else {
            panic!("counted once: {got:?}");
        };
        outcome.check(
            counts.trial_intervals() as usize == pattern.len(),
            &format!("every Trial Interval of {pattern} counted: {counts:?}"),
        );
        (counts.last_errored, counts.last_from_first_errored)
    };
    // Fewer than 64: seven from the first Errored Interval on.
    assert_eq!(last(12, "CCECHhECE"), (3, 7));
    assert_eq!(last(13, "CCHhC"), (0, 0));
    // Of 100, the last 64 keep the Errored Intervals 60, 30 and 1 back,
    // 60 Trial Intervals from the first of them, and not those 100, 90
    // and 70 back.
    let mut long = vec!['C'; 100];
    for errored in [0, 10, 30, 40, 70, 99] {
        long[errored] = 'E';
    }
    long[50] = 'H';
    long[60] = 'h';
    let long: String = long.into_iter().collect();
    assert_eq!(last(14, &long), (3, 60));
    // 64 back is among them, 65 back isn't.
    assert_eq!(last(15, &format!("E{}", "C".repeat(63))), (1, 64));
    assert_eq!(last(16, &format!("E{}", "C".repeat(64))), (0, 0));
}

// ; RFC 9946 §5.1(6) and RFC 9097 §8.1 judge the delay range: a
// Trial Interval whose largest delay variation is above upperThresh is
// errored though its average is low, one between the thresholds is a Hold
// Interval, and one without a delay sample is never clean, a Hold Interval
// counted apart, since no Load PDU arrived in order in it: no delay
// variation held it
// (conformance: delay-var-is-the-maximum-by-default, no-delay-sample-is-never-clean)
#[test]
fn delay_variation_counts_by_its_maximum() {
    // Ten samples each: one of 91 ms and nine of 0 average 9 ms.
    let spike = |max: u32| Report {
        delay_max: max,
        ..delay(max, 10)
    };
    let none = delay(0, 0);
    let (outcome, got) = counted(4, vec![spike(91), spike(91), spike(91), spike(40), none]);
    let want = TrialIntervalCounts {
        hold_without_sample: 1,
        ..ending(counts(0, 3, 2, true), 3, 5)
    };
    outcome.check(got == [want], &format!("counted {got:?}"));
    // At lowThresh a Trial Interval isn't clean yet; just below it, it is.
    let (outcome, got) = counted(5, vec![spike(30), spike(29), spike(90)]);
    outcome.check(got == [counts(1, 0, 2, false)], &format!("counted {got:?}"));
}

//  (the Status PDUs the Server classifies, and only those, are
// counted: stale ones and STOP ones aren't)
// (conformance: stale-status-pdus-count-only-as-heard, stop-status-pdus-are-not-classified)
#[test]
fn stale_and_stop_status_pdus_are_not_counted() {
    let numbered = |seq, report: Report| Report {
        spdu_seq_no: Some(seq),
        ..report
    };
    // 1 and 2 errored, a stale 2 and 1 errored too, then 3 errored as the
    // STOP: two back to back counted, no Congestion.
    let stop = Report {
        stop: true,
        ..numbered(3, errored())
    };
    let (outcome, got) = counted(
        6,
        vec![
            numbered(1, errored()),
            numbered(2, errored()),
            numbered(2, errored()),
            numbered(1, errored()),
            stop,
        ],
    );
    outcome.check(
        got == [ending(counts(0, 2, 0, false), 2, 2)],
        &format!("counted {got:?}"),
    );
}

/// A header-only Load PDU (RFC 9946 Figure 9) with this lpduSeqNo and
/// testAction.
fn load(seq_no: u32, test_action: u8) -> Vec<u8> {
    let mut b = vec![0u8; 32];
    b[0..2].copy_from_slice(&[0xBE, 0xEF]);
    b[2] = test_action;
    b[4..8].copy_from_slice(&seq_no.to_be_bytes());
    b[8..10].copy_from_slice(&32u16.to_be_bytes());
    b
}

// : Downstream, the Client counts the Status PDUs it sends marked
// TEST, and not the one marked STOP2 that confirms the Server's STOP (RFC
// 9946 §9), which the Server classifies no more than a Client's STOP
// Status PDU: as many as the Client's TEST Status PDUs on the wire. A
// scripted Server sends a Load PDU for each Status PDU, the fifth answered
// with STOP2 (conformance: stop-status-pdus-are-not-classified)
#[test]
fn the_status_pdu_confirming_stop2_is_not_counted() {
    const STOP2: u8 = 2;
    let mut statuses = 0u32;
    let outcome = Scenario::new(11)
        .client_unauthenticated()
        .client_direction(Direction::Downstream)
        .scripted_server_replying_many(move |_, request| {
            if request.len() == 56 {
                let mut pdu = request.to_vec();
                pdu[8] = 2;
                pdu[9] = 1;
                pdu[12..14].copy_from_slice(&24601u16.to_be_bytes());
                return vec![pdu];
            }
            if request.len() == 104 && request.starts_with(&[0xAC, 0xE2]) {
                let mut ack = request.to_vec();
                ack[5] = 1;
                return vec![ack, load(1, 0)];
            }
            if request.len() == 204 && request.starts_with(&[0xFE, 0xED]) {
                statuses += 1;
                let action = if statuses >= 5 { STOP2 } else { 0 };
                return vec![load(statuses + 1, action)];
            }
            Vec::new()
        })
        .run_for(Duration::from_millis(600));
    let client: std::net::SocketAddr = harness::CLIENT_ADDR.parse().unwrap();
    let actions: Vec<u8> = outcome
        .trace
        .iter()
        .filter(|t| t.source == client && t.len == 204 && t.payload.starts_with(&[0xFE, 0xED]))
        .map(|t| t.payload[2])
        .collect();
    let marked = |action: u8| actions.iter().filter(|&&a| a == action).count() as u32;
    let counted: Vec<TrialIntervalCounts> = outcome
        .client_data_events
        .iter()
        .filter_map(|(_, e)| match e {
            DataEvent::TrialIntervalsCounted(counts) => Some(*counts),
            _ => None,
        })
        .collect();
    outcome.check(
        marked(0) >= 5
            && marked(STOP2) == 1
            && actions.last() == Some(&STOP2)
            && matches!(counted[..], [one] if one.trial_intervals() == marked(0)),
        &format!("the Client's Status PDUs by testAction {actions:?}, counted {counted:?}"),
    );
}

const MBPS: u64 = 1_000_000;

// : Downstream, the Client counts the Status PDUs it sends, and the
// Server classifies them as they arrive, by the same maximum: the same
// counts, but for the one or two the Client sent after the Server's STOP1,
// which the Server no longer classifies; and the same Congestion, which
// the Server's rows show by their one drop of 3 x highSpeedDelta. Through
// a bottleneck with a short queue, a deep one, whose delay variation makes
// Hold Intervals too, and with a Server that sends only 3 datagrams of
// each Send Plan, on a clean path, whose search confirms nothing, once
// with a 150 ms outage of Load PDUs, whose Trial Intervals are Hold
// Intervals with no delay sample, counted apart.
#[test]
fn downstream_the_client_counts_what_the_server_classifies() {
    /// What a case adds to the scenario.
    type Path = fn(Scenario) -> Scenario;
    let run = |seed: u64, path: Path| {
        path(
            Scenario::new(seed)
                .server_allows_unauthenticated()
                .client_unauthenticated()
                .client_direction(Direction::Downstream)
                .path_delay(Duration::from_millis(5), Duration::ZERO)
                .client_config(|c| c.test_int_time = 5),
        )
        .run_for(Duration::from_millis(6_500))
    };
    // What, the seed, the path, whether Congestion is confirmed, and
    // whether Load PDUs stop arriving for a while, for two Trial Intervals
    // or more.
    let cases: [(&str, u64, Path, bool, bool); 4] = [
        (
            "10 ms of queue",
            7,
            |s| s.bottleneck(50 * MBPS, 62_500),
            true,
            false,
        ),
        (
            "200 ms of queue",
            8,
            |s| s.bottleneck(50 * MBPS, 1_250_000),
            true,
            false,
        ),
        (
            "a Server that falls behind",
            9,
            |s| s.partial_sends(1, 3),
            false,
            false,
        ),
        (
            "a Server that falls behind, and an outage",
            10,
            |s| {
                s.partial_sends(1, 3).network_drops(
                    [0xBE, 0xEF],
                    Duration::from_millis(3_000),
                    Some(Duration::from_millis(3_150)),
                )
            },
            false,
            true,
        ),
    ];
    for (what, seed, path, confirmed, outage) in cases {
        let outcome = run(seed, path);
        let server = classifications(&outcome);
        let of = |class| server.iter().filter(|c| c.class == class).count() as u32;
        let unsampled = server
            .iter()
            .filter(|c| c.class == IntervalClass::Hold && c.delay_var.is_none())
            .count() as u32;
        let rows: Vec<u16> = server.iter().filter_map(|c| c.row).collect();
        let drops = rows
            .windows(2)
            .filter(|w| i32::from(w[1]) - i32::from(w[0]) == -30)
            .count();
        let client: Vec<TrialIntervalCounts> = outcome
            .client_data_events
            .iter()
            .filter_map(|(_, e)| match e {
                DataEvent::TrialIntervalsCounted(counts) => Some(*counts),
                _ => None,
            })
            .collect();
        let [counted] = client[..] else {
            panic!("{what}: counted once: {client:?}");
        };
        let classified =
            of(IntervalClass::Clean) + of(IntervalClass::Errored) + of(IntervalClass::Hold);
        outcome.check(
            counted.clean >= of(IntervalClass::Clean)
                && counted.errored >= of(IntervalClass::Errored)
                && counted.hold >= of(IntervalClass::Hold)
                && counted.trial_intervals() <= classified + 2
                && counted.hold_without_sample >= unsampled
                && counted.hold_without_sample <= counted.hold
                && (!outage || unsampled >= 2)
                && counted.congestion_confirmed == confirmed
                && drops == usize::from(confirmed),
            &format!(
                "{what}: the Client counted {counted:?}; the Server classified {} Clean, {} Errored and {} Hold Intervals, {unsampled} of them with no delay sample, its rows dropping by 30 {drops} times",
                of(IntervalClass::Clean),
                of(IntervalClass::Errored),
                of(IntervalClass::Hold)
            ),
        );
    }
}
