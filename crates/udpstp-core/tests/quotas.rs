//! Quotas (RFC 9946 §11 item 6; ): a Server may limit how many Tests,
//! and how many load bytes, one Source or one Key Table row runs per Quota
//! Period. A new Test past its Quota is refused with CONNFAIL at Setup, and
//! with BADPARAM when it first activates if Tests set up alongside it used
//! the Quota first. Operator Keys are exempt. Off by default.

mod harness;

use std::time::Duration;

use harness::{ControlPhaseFailure, Outcome, Scenario};
use udpstp_core::{
    ActivationRejection, Direction, Event, KeyTable, QuotaLimits, SetupRejection,
    SetupRejectionCode,
};

const KEY: &[u8] = b"a consumer key";

fn refused_at_setup(outcome: &Outcome) -> usize {
    outcome
        .server_events
        .iter()
        .filter(|e| {
            matches!(
                e,
                Event::SetupRequestRejected {
                    reason: SetupRejection::QuotaExceeded,
                    ..
                }
            )
        })
        .count()
}

fn connfail(outcome: &Outcome, client: usize) -> bool {
    outcome.clients_events[client].contains(&Event::ControlPhaseFailed(
        ControlPhaseFailure::SetupRejected(SetupRejectionCode::ConnectionAllocationFailure),
    ))
}

/// Two 5 s Tests from one Source, the second after the first ended.
fn one_after_another(seed: u64) -> Scenario {
    Scenario::new(seed)
        .mode1(7, KEY)
        .client_config(|c| c.test_int_time = 5)
        .add_client_in_another_test_at(Direction::Downstream, Duration::from_secs(7))
}

#[udpstp_req::req("RFC9946-11-6")]
#[test]
fn a_source_past_its_quota_of_tests_is_refused_with_connfail() {
    let outcome = one_after_another(1)
        .server_config(|c| {
            c.source_quota = QuotaLimits {
                tests: Some(1),
                bytes: None,
            };
        })
        .run_for(Duration::from_secs(9));
    outcome.check(
        outcome.client_activated_direction(0).is_some()
            && refused_at_setup(&outcome) == 1
            && connfail(&outcome, 1),
        "the second Test is refused with CONNFAIL",
    );
}

// The volume: bytes are charged when the Test ends.
#[udpstp_req::req("RFC9946-11-6")]
#[test]
fn a_source_past_its_quota_of_bytes_is_refused() {
    let outcome = one_after_another(4)
        .server_config(|c| {
            c.source_quota = QuotaLimits {
                tests: None,
                bytes: Some(1000),
            };
        })
        .run_for(Duration::from_secs(9));
    outcome.check(
        outcome.client_activated_direction(0).is_some() && connfail(&outcome, 1),
        "the first Test's load used the byte Quota",
    );
}

// Tests set up together were all within the Quota at Setup: the second
// to activate is refused.
#[udpstp_req::req("RFC9946-11-6")]
#[test]
fn tests_set_up_together_past_the_quota_are_refused_at_activation() {
    let outcome = Scenario::new(5)
        .mode1(7, KEY)
        .server_tests_per_source(2)
        .server_config(|c| {
            c.source_quota = QuotaLimits {
                tests: Some(1),
                bytes: None,
            };
        })
        .add_client_in_another_test(Direction::Downstream)
        .run_for(Duration::from_millis(300));
    outcome.check(
        outcome.server_rejected_activation_because(ActivationRejection::QuotaExceeded)
            && [0, 1]
                .iter()
                .filter(|&&i| outcome.client_activated_direction(i).is_some())
                .count()
                == 1,
        "one Test runs, the other is refused when it activates",
    );
}

// A Key Table row's own Quota, whichever Source uses it.
#[udpstp_req::req("RFC9946-11-6")]
#[test]
fn a_key_past_its_quota_is_refused() {
    let mut row = harness::always_valid(7, KEY);
    row.quota = QuotaLimits {
        tests: Some(1),
        bytes: None,
    };
    let mut keys = KeyTable::default();
    keys.insert(row).expect("one row");
    let outcome = Scenario::new(7)
        .server_keys(keys)
        .client_mode1(7, KEY)
        .client_config(|c| c.test_int_time = 5)
        .add_client_in_another_test_at(Direction::Downstream, Duration::from_secs(7))
        .run_for(Duration::from_secs(9));
    outcome.check(
        outcome.client_activated_direction(0).is_some()
            && refused_at_setup(&outcome) == 1
            && connfail(&outcome, 1),
        "the key's second Test is refused",
    );
}
