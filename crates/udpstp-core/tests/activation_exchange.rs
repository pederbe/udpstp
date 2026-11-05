//! The Activation exchange after an unauthenticated Setup (RFC 9946 §7).

mod harness;

use std::net::SocketAddr;
use std::time::Duration;

use harness::requests::{self, Auth, TestConnection};
use harness::{ActivationRejection, ClientReplyScript, ClientScript, Scenario};
use udpstp_core::{Direction, Event};

fn activated(direction: Direction) -> harness::Outcome {
    Scenario::new(10)
        .server_allows_unauthenticated()
        .client_unauthenticated()
        .client_direction(direction)
        .run_for(Duration::from_secs(1))
}

/// The cmdRequest and cmdResponse (Figure 6 offsets 4 and 5) of the
/// Activation Requests the Client sent.
fn activation_request_commands(outcome: &harness::Outcome) -> Vec<(u8, u8)> {
    let client: SocketAddr = harness::CLIENT_ADDR.parse().unwrap();
    outcome
        .trace
        .iter()
        .filter(|t| t.source == client && t.payload.starts_with(&[0xAC, 0xE2]))
        .map(|t| (t.payload[4], t.payload[5]))
        .collect()
}

#[udpstp_req::req(
    "RFC9946-7.1-1",
    "RFC9946-7.3-5",
    "RFC9946-6.1-L10",
    "RFC9946-6.3-5",
    "RFC9946-12.3.6-L1",
    "RFC9946-7.1-L2",
    "RFC9946-6.1-L6"
)]
#[test]
fn downstream_test_is_activated_on_the_test_port() {
    let outcome = activated(Direction::Downstream);

    let test_port = outcome.client_accepted_test_port();
    outcome.check(test_port.is_some(), "the Setup was accepted");
    outcome.check(
        outcome.activation_requests_sent_to_port(test_port.unwrap()) == 1,
        "one Activation Request went to the testPort",
    );
    outcome.check(
        activation_request_commands(&outcome) == [(2, 0)],
        "cmdRequest CHTA_CREQ_TESTACTDS, cmdResponse CHTA_CRSP_NONE",
    );
    outcome.check(
        outcome.client_activated_direction(0) == Some(Direction::Downstream),
        "the Client prepared a Downstream Test",
    );
}

#[udpstp_req::req(
    "RFC9946-7.2.2-2",
    "RFC9946-7.3-5",
    "RFC9946-12.3.6-L1",
    "RFC9946-7.1-L2",
    "RFC9946-6.1-L6"
)]
#[test]
fn upstream_test_is_activated() {
    let outcome = activated(Direction::Upstream);

    outcome.check(
        activation_request_commands(&outcome) == [(1, 0)],
        "cmdRequest CHTA_CREQ_TESTACTUS, cmdResponse CHTA_CRSP_NONE",
    );

    outcome.check(
        outcome.client_activated_direction(0) == Some(Direction::Upstream),
        "the Client prepared an Upstream Test",
    );
    let responses = outcome.activation_responses();
    outcome.check(
        responses.len() == 1 && responses[0].sending_rate != Default::default(),
        "the Activation Response carries a Sending Rate Structure",
    );
}

#[udpstp_req::req("RFC9946-7.1-6", "RFC9946-7.2.2-3")]
#[test]
fn downstream_activation_response_carries_a_zero_sending_rate() {
    let outcome = activated(Direction::Downstream);

    let responses = outcome.activation_responses();
    outcome.check(responses.len() == 1, "one Activation Response was sent");
    outcome.check(
        responses[0].sending_rate == Default::default(),
        "its Sending Rate Structure is all zeros",
    );
}

/// A scripted Client that plays Test Connection `mc_index` of an
/// unauthenticated Test of two in `direction`.
fn scripted(mc_index: u8, direction: Direction) -> (ClientScript, ClientReplyScript) {
    let tc = TestConnection {
        mc_ident: 0x1234,
        mc_index,
        mc_count: 2,
        max_bandwidth: 0,
    };
    requests::test_connection_script(tc, direction, Auth::Unauthenticated)
}

// docs/spec.md (a Test has exactly one direction: the Server rejects Test
// Connections of one Test that ask for different directions;
// conformance: test-has-one-direction). Our Multi-Connection Coordinator
// never asks for two, so two scripted Clients play the Test: its first
// Test Connection Downstream, then its second Upstream.
#[test]
fn a_test_connection_asking_for_the_other_direction_is_rejected() {
    let (first_setup, first_activation) = scripted(0, Direction::Downstream);
    let (second_setup, second_activation) = scripted(1, Direction::Upstream);
    let outcome = Scenario::new(11)
        .server_allows_unauthenticated()
        .scripted_client_replying(first_setup, first_activation)
        .add_scripted_client(Duration::from_millis(10), second_setup, second_activation)
        .run_for(Duration::from_secs(1));

    // What each Client's Activation Request was answered with.
    let answers = |client: SocketAddr| -> Vec<u8> {
        outcome
            .trace
            .iter()
            .filter(|t| t.destination == client)
            .filter_map(|t| requests::activation_response_code(&t.payload))
            .collect()
    };
    let first: SocketAddr = harness::CLIENT_ADDR.parse().unwrap();
    let second = SocketAddr::new(first.ip(), first.port() + 1);
    let activated: Vec<Direction> = outcome
        .server_events
        .iter()
        .filter_map(|e| match e {
            Event::TestConnectionActivated { direction, .. } => Some(*direction),
            _ => None,
        })
        .collect();
    outcome.check(
        activated == [Direction::Downstream] && answers(first) == [ACK_OK],
        "the first Test Connection was activated",
    );
    outcome.check(
        outcome.server_events.iter().any(|e| {
            matches!(
                e,
                Event::ActivationRequestRejected {
                    reason: ActivationRejection::MixedDirection,
                    ..
                }
            )
        }) && answers(second) == [BAD_PARAM],
        "the second was told its Activation Request was rejected (CHTA_CRSP_BADPARAM)",
    );
    outcome.check(
        outcome.server_released_count() == 1,
        "the Server released the rejected Test Connection",
    );
}

/// cmdResponse CHTA_CRSP_ACKOK and CHTA_CRSP_BADPARAM (RFC 9946 §12.3.9).
const ACK_OK: u8 = 1;
const BAD_PARAM: u8 = 2;

// docs/spec.md (a Test has exactly one direction): Test Connections of one
// Test that ask for the same direction are all activated.
#[test]
fn test_connections_of_one_test_with_the_same_direction_are_activated() {
    let outcome = Scenario::new(12)
        .server_allows_unauthenticated()
        .client_unauthenticated()
        .client_direction(Direction::Upstream)
        .coordinated(2, 2)
        .run_for(Duration::from_secs(1));

    let activated: Vec<_> = outcome
        .client_connection_events
        .iter()
        .filter(|(_, e)| {
            matches!(
                e,
                udpstp_core::Event::TestActivated {
                    direction: Direction::Upstream,
                    ..
                }
            )
        })
        .map(|(socket, _)| *socket)
        .collect();
    outcome.check(
        activated.len() == 2 && activated[0] != activated[1],
        &format!("both Test Connections were activated: {activated:?}"),
    );
}
