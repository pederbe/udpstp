//! Trial Interval classification (RFC 9946 §5.1(6), §7.1; docs/spec.md "Load
//! Rate Adjustment Algorithms"): the Server classifies each Trial Interval
//! as clean, errored or neither from the Status PDU that reports it.
//!
//! Downstream, a scripted mode 1 Client sends Status PDUs built from the
//! RFC's Figure 10 offsets. Expected classes follow the spec's rules:
//! clean needs Sequence Errors at or below seqErrThresh and DelayVar
//! strictly below lowThresh; errored is Sequence Errors above seqErrThresh
//! or DelayVar above upperThresh; anything else is a Hold Interval, and no
//! delay sample is never clean.

mod harness;

use std::time::Duration;

use harness::status_script::{DEFAULTS, NODEL, Params, Report, classifications, delay, reported};
use harness::{Outcome, RowRequest, Scenario};
use udpstp_core::{Classification, DelayVarStatistic, Direction, IntervalClass};
use udpstp_wire::StatusPdu;

use IntervalClass::{Clean, Errored, Hold};

fn classified(seed: u64, params: Params, reports: Vec<Report>) -> (Outcome, Vec<Classification>) {
    reported(Scenario::new(seed), params, reports)
}

fn classes(got: &[Classification]) -> Vec<IntervalClass> {
    got.iter().map(|c| c.class).collect()
}

// docs/spec.md (with the average statistic, DelayVar is the Trial Interval's
// average one-way delay variation, rounded; clean needs it strictly below
// lowThresh)
#[udpstp_req::req("RFC9946-5.1-9", "RFC9946-7.1-L4", "RFC9097-8.1-L13", "RFC9946-8.1-L1")]
#[test]
fn the_average_rounds_and_clean_is_strictly_below_the_low_threshold() {
    let (outcome, got) = reported(
        Scenario::new(1).server_delay_var(DelayVarStatistic::Average),
        DEFAULTS,
        vec![
            // Averages 29, 29.5 (rounds to 30), 30, 90, 90.5 (91), 91.
            delay(58, 2),
            delay(59, 2),
            delay(30, 1),
            delay(270, 3),
            delay(181, 2),
            delay(91, 1),
        ],
    );
    let delays: Vec<Option<u32>> = got.iter().map(|c| c.delay_var).collect();
    let numbers: Vec<u32> = got.iter().map(|c| c.spdu_seq_no).collect();
    outcome.check(
        classes(&got) == [Clean, Hold, Hold, Hold, Errored, Errored]
            && delays == [Some(29), Some(30), Some(30), Some(90), Some(91), Some(91)]
            && numbers == [1, 2, 3, 4, 5, 6],
        &format!("{got:#?}"),
    );
}

// docs/spec.md (DelayVar is the Trial Interval's maximum delay variation by
// default, "the delay range" of RFC 9946 §5.1(6); a Server setting can make
// it the average),
#[udpstp_req::req("RFC9946-5.1-9")]
#[test]
fn the_maximum_is_the_default_and_the_average_a_setting() {
    // Averages 10, maxima 29, 30 and 91.
    let spread = |max| Report {
        delay_max: max,
        ..delay(20, 2)
    };
    let (outcome, got) = classified(11, DEFAULTS, vec![spread(29), spread(30), spread(91)]);
    let delays: Vec<Option<u32>> = got.iter().map(|c| c.delay_var).collect();
    outcome.check(
        classes(&got) == [Clean, Hold, Errored] && delays == [Some(29), Some(30), Some(91)],
        &format!("{got:#?}"),
    );

    let (outcome, got) = reported(
        Scenario::new(12).server_delay_var(DelayVarStatistic::Average),
        DEFAULTS,
        vec![spread(91)],
    );
    outcome.check(
        classes(&got) == [Clean],
        &format!("the average when set: {got:#?}"),
    );
}

// docs/spec.md  (an interval with no delay samples means no change, never
// clean); Sequence Errors alone still indicate congestion
#[udpstp_req::req("RFC9946-5.1-9")]
#[test]
fn no_delay_samples_is_never_clean() {
    let none = delay(0, 0);
    let (outcome, got) = classified(2, DEFAULTS, vec![none, Report { loss: 11, ..none }]);
    outcome.check(
        classes(&got) == [Hold, Errored] && got.iter().all(|c| c.delay_var.is_none()),
        &format!("{got:#?}"),
    );
}

// docs/spec.md  (seqErrThresh always applied, 0 honoured; Sequence Errors in
// packets)
#[udpstp_req::req("RFC9946-5.1-9", "RFC9097-8.1-L13")]
#[test]
fn seq_err_thresh_is_always_applied() {
    let quiet = delay(0, 1);
    let (outcome, got) = classified(
        3,
        DEFAULTS,
        vec![Report { loss: 10, ..quiet }, Report { loss: 11, ..quiet }],
    );
    outcome.check(classes(&got) == [Clean, Errored], &format!("10: {got:#?}"));

    let zero = Params {
        seq_err_thresh: 0,
        ..DEFAULTS
    };
    let (outcome, got) = classified(4, zero, vec![quiet, Report { loss: 1, ..quiet }]);
    outcome.check(classes(&got) == [Clean, Errored], &format!("0: {got:#?}"));
}

#[udpstp_req::req("RFC9946-7.1-L5", "RFC9946-7.1-L6")]
#[test]
fn ignore_ooo_dup_decides_what_counts_as_sequence_errors() {
    let report = Report {
        loss: 5,
        ooo: 4,
        dup: 4,
        ..delay(0, 1)
    };
    let (outcome, got) = classified(5, DEFAULTS, vec![report]);
    outcome.check(
        classes(&got) == [Clean] && got[0].seq_errors == 5,
        &format!("ignored: {got:#?}"),
    );

    let counted = Params {
        ignore_ooo_dup: 0,
        ..DEFAULTS
    };
    let (outcome, got) = classified(6, counted, vec![report]);
    outcome.check(
        classes(&got) == [Errored] && got[0].seq_errors == 13,
        &format!("counted: {got:#?}"),
    );
}

// docs/spec.md  (the classification uses the value accepted in the
// Activation exchange)
#[udpstp_req::req("RFC9946-7.2.1-L1")]
#[test]
fn the_accepted_threshold_classifies_not_the_requested_one() {
    // A seqErrThresh of 5000 is coerced to 100, its maximum.
    let asked = Params {
        seq_err_thresh: 5000,
        ..DEFAULTS
    };
    let loss = |loss| Report {
        loss,
        ..delay(0, 1)
    };
    let (outcome, got) = classified(7, asked, vec![loss(100), loss(101)]);
    outcome.check(classes(&got) == [Clean, Errored], &format!("{got:#?}"));
}

// docs/spec.md (RTT uses the latest sample)
#[udpstp_req::req("RFC9946-7.1-L4")]
#[test]
fn without_one_way_delay_the_latest_rtt_sample_is_delay_var() {
    let rtt = Params {
        use_ow_del_var: 0,
        ..DEFAULTS
    };
    // One-way delay averaging 200 ms is ignored.
    let ow = delay(200, 1);
    let (outcome, got) = classified(
        8,
        rtt,
        vec![
            Report {
                rtt_var_sample: 29,
                ..ow
            },
            Report {
                rtt_var_sample: 91,
                ..ow
            },
            Report {
                rtt_var_sample: NODEL,
                ..ow
            },
        ],
    );
    let delays: Vec<Option<u32>> = got.iter().map(|c| c.delay_var).collect();
    outcome.check(
        classes(&got) == [Clean, Errored, Hold] && delays == [Some(29), Some(91), None],
        &format!("{got:#?}"),
    );
}

// docs/spec.md  (a STOP Status PDU reports no Trial Interval to adjust on)
#[test]
fn a_stop_status_pdu_is_not_classified() {
    let stop = Report {
        stop: true,
        ..delay(0, 1)
    };
    let (outcome, got) = classified(9, DEFAULTS, vec![delay(0, 1), stop]);
    outcome.check(classes(&got) == [Clean], &format!("{got:#?}"));
}

// RFC 9946 §7 (in an Upstream Test the Server runs the algorithm on its own
// receive statistics); docs/spec.md  (STOP Status PDUs are not classified)
#[udpstp_req::req("RFC9946-5.1-9")]
#[test]
fn an_upstream_server_classifies_the_status_pdus_it_sends() {
    // A Search Test (not a Fixed-Rate Test) at 20 Mbps, losing 30 ms of
    // Load PDUs.
    let outcome = Scenario::new(10)
        .server_allows_unauthenticated()
        .client_unauthenticated()
        .client_direction(Direction::Upstream)
        .client_requests_row(RowRequest::StartAt(20))
        .network_drops(
            [0xBE, 0xEF],
            Duration::from_millis(300),
            Some(Duration::from_millis(330)),
        )
        .run_for(Duration::from_millis(10_600));

    let (stops, sent): (Vec<StatusPdu>, Vec<StatusPdu>) = outcome
        .trace
        .iter()
        .filter(|t| t.len == 204 && t.payload.starts_with(&[0xFE, 0xED]))
        .filter(|t| t.destination == harness::CLIENT_ADDR.parse().unwrap())
        .filter_map(|t| StatusPdu::decode(&t.payload).ok())
        .partition(|s| s.test_action != 0);
    // Each Status PDU sent, classified by the spec's rules with the default
    // thresholds (30 ms, 90 ms, 10 losses; loss only, the average).
    let expected: Vec<(u32, IntervalClass, u32, Option<u32>)> = sent
        .iter()
        .map(|s| {
            let (sum, cnt) = (u64::from(s.delay_var_sum), u64::from(s.delay_var_cnt));
            let delay = (cnt > 0).then(|| ((sum * 10 / cnt + 5) / 10) as u32);
            let class = if s.seq_err_loss > 10 || delay.is_some_and(|d| d > 90) {
                Errored
            } else if delay.is_some_and(|d| d < 30) {
                Clean
            } else {
                Hold
            };
            (s.spdu_seq_no, class, s.seq_err_loss, delay)
        })
        .collect();
    let got: Vec<(u32, IntervalClass, u32, Option<u32>)> = classifications(&outcome)
        .iter()
        .map(|c| (c.spdu_seq_no, c.class, c.seq_errors, c.delay_var))
        .collect();
    let errored = got.iter().filter(|c| c.1 == Errored).count();
    outcome.check(
        got == expected && !stops.is_empty() && errored >= 1,
        &format!(
            "{} TEST and {} STOP Status PDUs sent; {errored} errored",
            sent.len(),
            stops.len()
        ),
    );
}
