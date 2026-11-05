//! The Test Interval and 500 ms limits (RFC 9946 §5.1 items 4 and 5; spec
//! ; ADR 0010): the Server coerces testIntTime to at most 60 s, or to an
//! Operator Key's own maximum; above 60 s, one-way delay variation to RTT
//! variation; and, except for an Operator Key, slowAdjThresh until
//! slowAdjThresh × trialInt is at most 500 ms.

mod harness;

use std::time::Duration;

use harness::status_script::{DEFAULTS, Params, reported};
use harness::{ActivationParameter, Outcome, Scenario};
use udpstp_wire::ActivationPdu;

/// The Activation Response the Server sent for `params`.
fn response(seed: u64, params: Params) -> (Outcome, ActivationPdu) {
    let (outcome, _) = reported(Scenario::new(seed), params, vec![]);
    let response = outcome
        .activation_responses()
        .first()
        .cloned()
        .expect("an Activation Response");
    (outcome, response)
}

fn operator(max: Option<u64>) -> Params {
    Params {
        operator: Some(max),
        ..DEFAULTS
    }
}

// The Test Interval is bounded; docs/spec.md (60 s unless an Operator Key
// allows more)
#[udpstp_req::req("RFC9946-10-1", "RFC9946-11-6", "RFC9097-8-2", "RFC9097-10-6")]
#[test]
fn an_ordinary_key_gets_at_most_60_s() {
    let (outcome, r) = response(
        1,
        Params {
            test_int_time: 3600,
            ..DEFAULTS
        },
    );
    outcome.check(
        r.test_int_time == 60
            && r.use_ow_del_var == 1
            && outcome
                .server_coercions()
                .iter()
                .any(|c| c.parameter == ActivationParameter::TestIntTime),
        &format!("{r:?}"),
    );
}

// ADR 0010 (an Operator Key allows Tests up to its own maximum); spec
//  (above 60 s, RTT variation)
#[test]
fn an_operator_key_allows_its_own_maximum_with_rtt_variation() {
    for (seed, max, asked, granted, one_way) in [
        (2, Some(3600), 3600, 3600, 0),
        (3, Some(3600), 7200, 3600, 0),
        (4, Some(3600), 120, 120, 0),
        (5, Some(3600), 60, 60, 1),
        (6, None, 3600, 60, 1),
    ] {
        let (outcome, r) = response(
            seed,
            Params {
                test_int_time: asked,
                ..operator(max)
            },
        );
        outcome.check(
            r.test_int_time == granted && r.use_ow_del_var == one_way,
            &format!("max {max:?}, asked {asked}: {r:?}"),
        );
    }
}

// Congestion inferred within 500 ms: slowAdjThresh × trialInt; docs/spec.md
// (Operator Keys exempt)
#[udpstp_req::req("RFC9946-5.1-8")]
#[test]
fn slow_adj_thresh_times_trial_int_is_at_most_500_ms() {
    for (seed, trial_int, asked, expected, operator_key) in [
        (7, 250, 5, 2, false),
        (8, 50, 20, 10, false),
        (9, 5, 50, 50, false),
        (10, 50, 10, 10, false),
        (11, 250, 5, 5, true),
    ] {
        let params = Params {
            trial_int,
            slow_adj_thresh: asked,
            operator: operator_key.then_some(None),
            ..DEFAULTS
        };
        let (outcome, r) = response(seed, params);
        outcome.check(
            r.slow_adj_thresh == expected && r.trial_int == trial_int,
            &format!("trialInt {trial_int}, asked {asked}: {r:?}"),
        );
    }
}

// The Client runs with the Server's values: a Test Interval above 60 s that
// an Operator Key allows is runnable.
#[udpstp_req::req("RFC9946-7.3-4")]
#[test]
fn the_client_runs_a_long_test_its_operator_key_allows() {
    let outcome = Scenario::new(12)
        .server_operator_key(7, b"an operator key", Some(120))
        .client_mode1(7, b"an operator key")
        .client_config(|c| c.test_int_time = 120)
        .run_for(Duration::from_millis(300));
    let granted = outcome
        .activation_responses()
        .first()
        .map(|r| r.test_int_time);
    outcome.check(
        granted == Some(120) && outcome.client_activated_direction(0).is_some(),
        &format!("{granted:?}: {:?}", outcome.client_events),
    );
}

// The Client ends a Test whose response it can't run: a Server can't make it
// run longer than it asked, or under 5 s.
#[udpstp_req::req("RFC9946-7.3-L1")]
#[test]
fn the_client_refuses_a_longer_test_than_it_asked_for() {
    for (seed, granted) in [(13u64, 61u16), (14, 3600), (15, 4)] {
        let outcome = Scenario::new(seed)
            .client_unauthenticated()
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
                    ack[12..14].copy_from_slice(&granted.to_be_bytes());
                    return vec![ack];
                }
                Vec::new()
            })
            .run_for(Duration::from_millis(100));
        outcome.check(
            outcome.client_failed_because(harness::ControlPhaseFailure::UnrunnableParameter {
                parameter: ActivationParameter::TestIntTime,
                value: granted,
            }),
            &format!("{granted} s: {:?}", outcome.client_events),
        );
    }
}
