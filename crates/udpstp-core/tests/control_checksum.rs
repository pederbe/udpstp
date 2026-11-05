//! The optional checksum on the Setup, Null Request and Activation PDUs
//! (RFC 9946 §5.6): with checksum use configured, the sender fills it last,
//! after signing, with the field zeroed; the receiver verifies it first,
//! after the size, when the field is populated, and silently discards a
//! PDU that fails. An unpopulated field passes. The RFC 1071 vector is
//! pinned in crates/udpstp-wire/tests/load_pdu.rs.

mod harness;

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use harness::{Outcome, ResponseIgnoreReason, Scenario, SilentRejectionReason, TraceEntry};
use udpstp_core::Event;
use udpstp_wire::checksum::internet_checksum;

const KEY_ID: u8 = 7;
const KEY: &[u8] = b"a shared key";

fn server_ip() -> IpAddr {
    harness::SERVER_ADDR.parse::<SocketAddr>().unwrap().ip()
}

fn client_ip() -> IpAddr {
    harness::CLIENT_ADDR.parse::<SocketAddr>().unwrap().ip()
}

fn mode1(seed: u64) -> Scenario {
    Scenario::new(seed)
        .server_key(KEY_ID, KEY)
        .client_mode1(KEY_ID, KEY)
}

fn control_pdus(outcome: &Outcome) -> Vec<&TraceEntry> {
    outcome
        .trace
        .iter()
        .filter(|t| {
            t.payload.starts_with(&[0xAC, 0xE1])
                || t.payload.starts_with(&[0xAC, 0xE2])
                || t.payload.starts_with(&[0xDE, 0xAD])
        })
        .collect()
}

fn run(scenario: Scenario) -> Outcome {
    scenario.run_for(Duration::from_millis(50))
}

#[udpstp_req::req(
    "RFC9946-5.6-3",
    "RFC9946-6.1-L14",
    "RFC9946-6.1-L15",
    "RFC9946-7.1-L1"
)]
#[test]
fn every_control_pdu_carries_a_checksum_that_verifies() {
    let outcome = run(mode1(1).checksum());
    let pdus = control_pdus(&outcome);
    // Setup Request and Response, Null Request, Activation Request and
    // Response.
    let ok = pdus.len() == 5
        && pdus.iter().all(|t| {
            let at = t.len - 2;
            t.payload[at..] != [0, 0] && internet_checksum(&t.payload) == 0
        });
    outcome.check(
        ok && outcome.client_activated_direction(0).is_some(),
        &format!("{pdus:?}"),
    );
}

// The checksum is verified before authentication.
#[udpstp_req::req("RFC9946-5.6-4", "RFC9946-5.6-5", "RFC9946-6.2-1")]
#[test]
fn a_corrupted_setup_request_is_dropped_before_authentication() {
    // mcIdent flipped on the way to the Server.
    let outcome = run(mode1(2)
        .checksum()
        .network_flips_byte_to(server_ip(), [0xAC, 0xE1], 6));
    outcome.check(
        outcome.server_dropped_setup_because(SilentRejectionReason::ChecksumFailed)
            && !outcome.server_dropped_setup_because(SilentRejectionReason::AuthenticationFailed)
            && outcome.client_accepted_test_port().is_none(),
        &format!("{:?}", outcome.server_events),
    );
}

#[udpstp_req::req("RFC9946-5.6-4", "RFC9946-5.6-5", "RFC9946-6.3-1")]
#[test]
fn a_corrupted_setup_response_is_dropped() {
    let outcome = run(mode1(3)
        .checksum()
        .network_flips_byte_to(client_ip(), [0xAC, 0xE1], 6));
    outcome.check(
        outcome.client_dropped_setup_response_because(ResponseIgnoreReason::ChecksumFailed)
            && outcome.client_accepted_test_port().is_none(),
        &format!("{:?}", outcome.client_events),
    );
}

#[udpstp_req::req("RFC9946-5.6-4", "RFC9946-5.6-5", "RFC9946-7.2-2")]
#[test]
fn a_corrupted_activation_request_is_dropped() {
    let outcome = run(mode1(4)
        .checksum()
        .network_flips_byte_to(server_ip(), [0xAC, 0xE2], 6));
    outcome.check(
        outcome.server_dropped_activation_because(SilentRejectionReason::ChecksumFailed)
            && outcome.activation_responses().is_empty(),
        &format!("{:?}", outcome.server_events),
    );
}

#[udpstp_req::req("RFC9946-5.6-4", "RFC9946-5.6-5", "RFC9946-6.2.2-6", "RFC9946-7.3-1")]
#[test]
fn a_corrupted_activation_response_or_null_request_is_dropped() {
    let outcome = run(mode1(5)
        .checksum()
        .network_flips_byte_to(client_ip(), [0xAC, 0xE2], 6));
    outcome.check(
        outcome
            .client_events
            .contains(&Event::ActivationResponseIgnored {
                reason: ResponseIgnoreReason::ChecksumFailed,
            })
            && outcome.client_activated_direction(0).is_none(),
        &format!("{:?}", outcome.client_events),
    );

    let outcome = run(mode1(6)
        .checksum()
        .network_flips_byte_to(client_ip(), [0xDE, 0xAD], 6));
    outcome.check(
        outcome.client_events.contains(&Event::NullRequestIgnored {
            reason: ResponseIgnoreReason::ChecksumFailed,
        }) && outcome.client_activated_direction(0).is_some(),
        &format!("{:?}", outcome.client_events),
    );
}

// Verified only when checksum processing is configured and the field is
// populated.
#[udpstp_req::req("RFC9946-5.6-4")]
#[test]
fn only_a_configured_receiver_verifies_and_only_a_populated_field() {
    // The Client fills it, and it is corrupted, but the Server doesn't use
    // checksums: the Server's Setup exchange goes on unhindered in
    // Unauthenticated Mode.
    let outcome = run(Scenario::new(7)
        .server_allows_unauthenticated()
        .client_unauthenticated()
        .client_config(|c| c.checksum = true)
        .network_flips_byte_to(server_ip(), [0xAC, 0xE1], 54));
    outcome.check(
        outcome.client_activated_direction(0).is_some(),
        &format!("unconfigured Server: {:?}", outcome.server_events),
    );

    // The Server uses checksums but the Client doesn't: unpopulated fields
    // pass both ways.
    let outcome = run(mode1(8).server_checksum());
    outcome.check(
        outcome.client_activated_direction(0).is_some(),
        &format!("unpopulated: {:?}", outcome.server_events),
    );
}
