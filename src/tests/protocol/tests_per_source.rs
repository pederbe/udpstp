//! One Test per Source (RFC 9946 §4.2; docs/spec.md "Server Control Phase"):
//! the Server runs one concurrent Test per Source, an IPv4 address or an
//! IPv6 /64, and refuses a second one with a signed CONNFAIL Setup Response.
//! Test Connections of the same Test (the same mcIdent) are one Test.

use super::harness;

use std::time::Duration;

use crate::protocol::{ActivationRejection, Direction, Event, SetupRejection, SetupRejectionCode};
use harness::{ControlPhaseFailure, Outcome, Scenario};

const CONNFAIL: u8 = 13;

fn two_tests(seed: u64) -> Scenario {
    Scenario::new(seed)
        .operator(7, b"an operator key")
        .add_client_in_another_test(Direction::Downstream)
}

fn server_refused(outcome: &Outcome) -> usize {
    outcome
        .server_events
        .iter()
        .filter(|e| {
            matches!(
                e,
                Event::SetupRequestRejected {
                    reason: SetupRejection::TestsPerSource,
                    ..
                } | Event::ActivationRequestRejected {
                    reason: ActivationRejection::TestsPerSource,
                    ..
                }
            )
        })
        .count()
}

// An authentic rejection is answered.
#[udpstp_req::req("RFC9946-4.2-1", "RFC9946-10-4", "RFC9097-8.3-6", "RFC9946-6.2.1-3")]
#[test]
fn a_second_test_from_the_same_source_is_refused_at_setup() {
    // The second Test starts once the first is activated.
    let outcome = Scenario::new(1)
        .operator(7, b"an operator key")
        .add_client_in_another_test_at(Direction::Downstream, Duration::from_millis(50))
        .run_for(Duration::from_millis(150));
    let responses: Vec<u8> = outcome
        .trace
        .iter()
        .filter(|t| t.payload.starts_with(&[0xAC, 0xE1]) && t.payload[8] == 2)
        .map(|t| t.payload[9])
        .collect();
    outcome.check(
        outcome.client_activated_direction(0).is_some()
            && server_refused(&outcome) == 1
            && outcome.clients_events[1].contains(&Event::ControlPhaseFailed(
                ControlPhaseFailure::SetupRejected(SetupRejectionCode::ConnectionAllocationFailure),
            ))
            && responses == [1, CONNFAIL],
        &format!("responses {responses:?}; {:?}", outcome.clients_events),
    );
}

// docs/spec.md (a Setup that isn't activated doesn't hold
// a Source's place: of two set up together, the first activated wins)
#[udpstp_req::req("RFC9946-4.2-1")]
#[test]
fn of_two_tests_set_up_together_the_second_is_refused_at_activation() {
    let outcome = two_tests(4).run_for(Duration::from_millis(100));
    let activated = (0..2)
        .filter(|&i| outcome.client_activated_direction(i).is_some())
        .count();
    let rejected = (0..2)
        .filter(|&i| outcome.client_activation_rejected(i))
        .count();
    outcome.check(
        activated == 1 && rejected == 1 && server_refused(&outcome) == 1,
        &format!("{:?}", outcome.clients_events),
    );
}
