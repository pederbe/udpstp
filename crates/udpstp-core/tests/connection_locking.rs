//! Connection locking (docs/spec.md story 88): a Test Connection takes
//! Activation Requests only from its Client's IP address, and the first one
//! that passes verification connects its socket to that address and port,
//! a NAT's port included, before the Activation Response goes out.

mod harness;

use std::net::SocketAddr;
use std::time::Duration;

use harness::Scenario;

fn client() -> SocketAddr {
    harness::CLIENT_ADDR.parse().unwrap()
}

// docs/spec.md (on the first authenticated Activation Request the
// ephemeral socket connects to that address and port)
#[test]
fn the_first_verified_activation_request_connects_the_socket() {
    let outcome = Scenario::new(1)
        .operator(7, b"an operator key")
        .run_for(Duration::from_millis(100));
    outcome.check(
        outcome.client_activated_direction(0).is_some(),
        "the Test was activated",
    );
    let connects = &outcome.server_connects;
    outcome.check(
        connects.len() == 1 && connects[0].1 == client(),
        &format!("connected once, to the Client: {connects:?}"),
    );
}

// docs/spec.md (before Activation the ephemeral socket accepts only the
// Client's IP address)
#[test]
fn a_request_from_another_ip_address_connects_nothing() {
    let outcome = Scenario::new(2)
        .operator(7, b"an operator key")
        .activation_pdus_from("203.0.113.9".parse().unwrap())
        .run_for(Duration::from_millis(100));
    outcome.check(
        outcome.server_connects.is_empty(),
        &format!("{:?}", outcome.server_connects),
    );
}

// docs/spec.md (only an authenticated request locks the Test Connection)
#[test]
fn a_request_failing_verification_connects_nothing() {
    // A flipped byte in the authDigest (bytes 68-99 of the Activation PDU).
    let outcome = Scenario::new(3)
        .operator(7, b"an operator key")
        .network_flips_byte([0xAC, 0xE2], 70)
        .run_for(Duration::from_millis(100));
    outcome.check(
        outcome.server_connects.is_empty() && outcome.client_activated_direction(0).is_none(),
        &format!("{:?}", outcome.server_events),
    );
}

// docs/spec.md story 88 (the Test Connection is locked to the Activation
// Request's address and port, which a NAT may have changed since the
// Setup)
#[test]
fn a_nat_that_changes_the_port_is_followed() {
    let outcome = Scenario::new(4)
        .operator(7, b"an operator key")
        .client_direction(udpstp_core::Direction::Upstream)
        .client_requests_row(harness::RowRequest::FixedRate(5))
        .client_nat_rebinds_to(50_000)
        .run_for(Duration::from_millis(600));
    let mapped = SocketAddr::new(client().ip(), 50_000);
    outcome.check(
        outcome.server_connects.len() == 1 && outcome.server_connects[0].1 == mapped,
        &format!("connected to the NAT's port: {:?}", outcome.server_connects),
    );
    // The Data Phase runs through it: the Server hears the Client's load.
    outcome.check(
        outcome
            .server_data_events
            .iter()
            .any(|(_, e)| matches!(e, udpstp_core::DataEvent::FirstLoadPduReceived)),
        "the Server received Load PDUs",
    );
}
