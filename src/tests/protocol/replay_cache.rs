//! The replay cache (RFC 9946 §5.3.1; docs/spec.md story 84): a verified Setup
//! or Activation Request seen again while its authUnixTime still passes the
//! ±5 s window is dropped, whichever socket the copy reaches, including a
//! sibling Test Connection's Ephemeral Port. Unauthenticated Mode has no
//! digest and no replay protection.

use super::harness;

use std::net::SocketAddr;
use std::time::Duration;

use crate::protocol::{Direction, Event};
use harness::requests::{self, Auth, TestConnection};
use harness::{ClientReplyScript, ClientScript, Outcome, Scenario, SilentRejectionReason};

const SETUP: [u8; 2] = [0xAC, 0xE1];
const ACTIVATION: [u8; 2] = [0xAC, 0xE2];

fn setup_dropped(outcome: &Outcome, why: SilentRejectionReason) -> bool {
    outcome
        .server_events
        .iter()
        .any(|e| matches!(e, Event::SetupRequestSilentlyRejected { reason, .. } if *reason == why))
}

// docs/spec.md story 84 (captured PDUs can't start Tests); RFC 9946 §5.3.1
// (the time window alone lets a replay through within ±5 s)
#[test]
fn a_replayed_setup_request_is_dropped() {
    let outcome = Scenario::new(1)
        .operator(7, b"an operator key")
        .network_replays(SETUP, 0, Duration::from_secs(2))
        .run_for(Duration::from_millis(2100));
    outcome.check(
        setup_dropped(&outcome, SilentRejectionReason::Replayed)
            && outcome.server_admitted_count() == 1,
        &format!("{:?}", outcome.server_events),
    );
}

const OPERATOR_KEY: &[u8] = b"an operator key";

/// A scripted Client that plays Test Connection `mc_index` of a Downstream
/// Test of two in mode 1, with the Operator Key.
fn scripted(mc_index: u8) -> (ClientScript, ClientReplyScript) {
    let tc = TestConnection {
        mc_ident: 0x1234,
        mc_index,
        mc_count: 2,
        max_bandwidth: 0,
    };
    let auth = Auth::Mode1 {
        key_id: 7,
        key: OPERATOR_KEY,
    };
    requests::test_connection_script(tc, Direction::Downstream, auth)
}

// docs/spec.md story 84 ("including replays across sibling connections")
#[test]
fn a_replayed_activation_request_is_dropped_at_a_sibling_test_connection() {
    // Both Test Connections, each played by a scripted Client, are set up
    // in the same second, so they derive the same keys and their Activation
    // Requests are identical: the first one's would verify on the second's
    // port. The copy, from the first Client's address, reaches that port
    // before the second Client's own request, which is still accepted.
    let (first_setup, first_activation) = scripted(0);
    let (second_setup, second_activation) = scripted(1);
    let outcome = Scenario::new(2)
        .operator(7, OPERATOR_KEY)
        .path_delay(Duration::from_millis(20), Duration::ZERO)
        .scripted_client_replying(first_setup, first_activation)
        .add_scripted_client(Duration::from_millis(300), second_setup, second_activation)
        .network_replays_to_sibling(ACTIVATION, 0, Duration::from_millis(330))
        .run_for(Duration::from_millis(500));
    let dropped = outcome.server_events.iter().any(|e| {
        matches!(
            e,
            Event::ActivationRequestSilentlyRejected {
                reason: SilentRejectionReason::Replayed,
                ..
            }
        )
    });
    let activated = outcome
        .server_events
        .iter()
        .filter(|e| matches!(e, Event::TestConnectionActivated { .. }))
        .count();
    // The premise: the two Activation Requests are byte for byte the same,
    // so only the sender tells the copy from the second's own.
    let first: SocketAddr = harness::CLIENT_ADDR.parse().unwrap();
    let second = SocketAddr::new(first.ip(), first.port() + 1);
    let sent_by = |client: SocketAddr| -> Vec<&[u8]> {
        outcome
            .trace
            .iter()
            .filter(|t| t.source == client && t.payload.starts_with(&ACTIVATION))
            .map(|t| t.payload.as_slice())
            .collect()
    };
    let (firsts, seconds) = (sent_by(first), sent_by(second));
    outcome.check(
        !firsts.is_empty() && firsts.iter().chain(&seconds).all(|&pdu| pdu == firsts[0]),
        &format!("identical Activation Requests: {firsts:?} {seconds:?}"),
    );
    outcome.check(
        dropped && activated == 2 && seconds.len() == 1,
        &format!("{:?}", outcome.server_events),
    );
}

// docs/spec.md (Unauthenticated Mode bypasses the replay cache; a recorded Deviation)
#[test]
fn unauthenticated_requests_bypass_the_cache() {
    let outcome = Scenario::new(3)
        .server_allows_unauthenticated()
        .client_unauthenticated()
        .network_replays(SETUP, 0, Duration::from_secs(1))
        .run_for(Duration::from_millis(1100));
    // Caught only as a duplicate of the admitted Test Connection.
    outcome.check(
        !setup_dropped(&outcome, SilentRejectionReason::Replayed)
            && outcome
                .server_ignored_setup_because(harness::SetupIgnoreReason::DuplicateSetupRequest),
        &format!("{:?}", outcome.server_events),
    );
}

// docs/spec.md story 84 (a replay is dropped even once the Test
// Connection it copies has ended, while its authUnixTime still passes the
// window)
#[test]
fn a_setup_request_replayed_after_its_test_connection_ended_is_dropped() {
    // The Activation Request is lost, so the Activation Watchdog closes the
    // Test Connection about 3 s in; the copy comes at 4 s.
    let outcome = Scenario::new(4)
        .operator(7, b"an operator key")
        .network_drops(ACTIVATION, Duration::ZERO, Some(Duration::from_secs(1)))
        .network_replays(SETUP, 0, Duration::from_secs(4))
        .run_for(Duration::from_millis(4100));
    outcome.check(
        outcome.server_released_count() == 1
            && setup_dropped(&outcome, SilentRejectionReason::Replayed)
            && outcome.server_admitted_count() == 1,
        &format!("{:?}", outcome.server_events),
    );
}

// docs/spec.md (a Test Connection is locked to its Client's IP address)
#[test]
fn an_activation_request_from_another_ip_address_is_dropped() {
    let outcome = Scenario::new(5)
        .operator(7, b"an operator key")
        .activation_pdus_from("203.0.113.9".parse().unwrap())
        .run_for(Duration::from_millis(100));
    let dropped = outcome.server_events.iter().any(|e| {
        matches!(
            e,
            Event::ActivationRequestSilentlyRejected {
                reason: SilentRejectionReason::NotFromClient,
                ..
            }
        )
    });
    outcome.check(
        dropped && outcome.client_activated_direction(0).is_none(),
        &format!("{:?}", outcome.server_events),
    );
}

// A request is remembered only once it is within its Source's budget, so a
// key holder's flood of refused requests can't fill the cache and lock
// every Client out. The price: a refused request's copy is judged afresh
// once the budget refills.
#[test]
fn a_request_refused_by_its_sources_budget_isnt_remembered() {
    let outcome = Scenario::new(6)
        .operator(7, b"an operator key")
        .server_tests_per_source(2)
        .server_config(|c| {
            c.test_connections_per_test = 1;
            c.source_setup_burst = 1;
            c.source_setups_per_second = 1;
        })
        .add_client_in_another_test_at(Direction::Downstream, Duration::from_millis(10))
        .network_replays(SETUP, 1, Duration::from_millis(1500))
        .run_for(Duration::from_millis(1600));
    outcome.check(
        !setup_dropped(&outcome, SilentRejectionReason::Replayed)
            && outcome.server_admitted_count() == 2,
        &format!("{:?}", outcome.server_events),
    );
}
