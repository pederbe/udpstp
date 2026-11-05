//! The Null Request (RFC 9946 §6.2.2, Figure 4): right after the Setup
//! Response, the Server sends one from the Test Connection's Ephemeral Port
//! to the Client, so its own firewall lets the Client's traffic in. In mode
//! 1 it is signed like the Setup Response. The Client verifies it if it
//! arrives and never answers it; a missing one changes nothing.
//!
//! Expected values come from Figure 4 and the Setup PDU's methodology; the
//! signature is checked here with the Server Authentication Key derived
//! from the Setup Request's authUnixTime.

mod harness;

use std::net::SocketAddr;
use std::time::Duration;

use harness::{Outcome, ResponseIgnoreReason, Scenario, TraceEntry};
use udpstp_core::Event;
use udpstp_wire::auth::{derive_keys, hmac_sha256};

const KEY_ID: u8 = 7;
const KEY: &[u8] = b"a shared key";

fn server() -> SocketAddr {
    harness::SERVER_ADDR.parse().unwrap()
}

fn client() -> SocketAddr {
    harness::CLIENT_ADDR.parse().unwrap()
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// What the Server sent the Client during the Setup exchange, in order.
fn from_server(outcome: &Outcome) -> Vec<&TraceEntry> {
    outcome
        .trace
        .iter()
        .filter(|t| t.source.ip() == server().ip() && t.destination == client())
        .filter(|t| t.payload.starts_with(&[0xAC, 0xE1]) || t.payload.starts_with(&[0xDE, 0xAD]))
        .collect()
}

fn unauthenticated(seed: u64) -> Scenario {
    Scenario::new(seed)
        .server_allows_unauthenticated()
        .client_unauthenticated()
}

fn mode1(seed: u64) -> Scenario {
    Scenario::new(seed)
        .server_key(KEY_ID, KEY)
        .client_mode1(KEY_ID, KEY)
}

// Unauthenticated, the Null Request's authentication fields are zero.
#[udpstp_req::req(
    "RFC9946-6.2.2-4",
    "RFC9946-6.2.2-5",
    "RFC9946-6.2.2-L4",
    "RFC9946-6.2.2-L5",
    "RFC9946-5.5-2",
    "RFC9946-6.1-L6"
)]
#[test]
fn the_server_sends_a_null_request_after_the_setup_response() {
    let outcome = unauthenticated(1).run_for(Duration::from_millis(50));
    outcome.check(
        null_received(&outcome),
        "an unauthenticated Client takes it",
    );
    let sent = from_server(&outcome);
    let ok = sent.len() == 2 && {
        let (response, null) = (&sent[0].payload, &sent[1].payload);
        let test_port = u16::from_be_bytes([response[12], response[13]]);
        response.len() == 56
            && sent[0].source == server()
            && null.len() == 48
            && null[0..2] == [0xDE, 0xAD]
            && sent[1].source.port() == test_port
            && null[2..4] == 20u16.to_be_bytes()
            && null[4] == 1
            && null[5] == 0
            && null[7..48].iter().all(|&b| b == 0)
    };
    outcome.check(ok, &format!("{sent:?}"));
}

// Mode 1, the current authUnixTime, the Setup's keyId, and the digest with
// the Server Authentication Key.
#[udpstp_req::req("RFC9946-6.2.2-L3", "RFC9946-5.3.1-8")]
#[test]
fn a_mode_1_null_request_is_signed_with_the_server_key() {
    let outcome = mode1(2).run_for(Duration::from_millis(50));
    let setup_time = outcome
        .trace
        .iter()
        .find(|t| t.source == client() && t.payload.starts_with(&[0xAC, 0xE1]))
        .map(|t| u32_at(&t.payload, 16))
        .unwrap();
    let sent = from_server(&outcome);
    let null = sent.get(1).map(|t| t.payload.clone()).unwrap_or_default();
    let mut zeroed = null.clone();
    if zeroed.len() == 48 {
        zeroed[12..44].fill(0);
        zeroed[46..48].fill(0);
    }
    let digest = hmac_sha256(derive_keys(KEY, setup_time).server(), &zeroed);
    outcome.check(
        null.len() == 48
            && null[7] == 1
            && null[44] == KEY_ID
            && u32_at(&null, 8).abs_diff(setup_time) <= 1
            && null[12..44] == digest,
        &format!("{null:02x?}"),
    );
}

/// The datagrams the Client sent.
fn from_client(outcome: &Outcome) -> Vec<&TraceEntry> {
    outcome
        .trace
        .iter()
        .filter(|t| t.source == client())
        .collect()
}

fn null_received(outcome: &Outcome) -> bool {
    outcome.client_events.contains(&Event::NullRequestReceived)
}

#[udpstp_req::req("RFC9946-6.2.2-6", "RFC9946-6.2.2-L2")]
#[test]
fn the_client_verifies_the_null_request_and_does_not_answer() {
    let outcome = mode1(3).run_for(Duration::from_millis(50));
    // Everything but the Data Phase's Status PDUs and Load PDUs.
    let sent: Vec<[u8; 2]> = from_client(&outcome)
        .iter()
        .filter_map(|t| t.payload.first_chunk::<2>().copied())
        .filter(|id| *id != [0xFE, 0xED] && *id != [0xBE, 0xEF])
        .collect();
    outcome.check(
        null_received(&outcome)
            && sent == [[0xAC, 0xE1], [0xAC, 0xE2]]
            && outcome.client_activated_direction(0).is_some(),
        &format!("{:?}; sent {sent:02x?}", outcome.client_events),
    );
}

// Without a Null Request, or with a forged one, the Test goes on.
#[udpstp_req::req("RFC9946-6.2.2-6", "RFC9946-6.2-2", "RFC9946-5.5-L2")]
#[test]
fn a_forged_or_missing_null_request_changes_nothing() {
    // A byte of its authDigest flipped on the way.
    let outcome = mode1(4)
        .network_flips_byte([0xDE, 0xAD], 20)
        .run_for(Duration::from_millis(50));
    outcome.check(
        outcome.client_events.contains(&Event::NullRequestIgnored {
            reason: ResponseIgnoreReason::AuthenticationFailed,
        }) && !null_received(&outcome)
            && outcome.client_activated_direction(0).is_some(),
        &format!("forged: {:?}", outcome.client_events),
    );

    let outcome = mode1(5)
        .network_drops([0xDE, 0xAD], Duration::ZERO, None)
        .run_for(Duration::from_millis(50));
    outcome.check(
        !null_received(&outcome) && outcome.client_activated_direction(0).is_some(),
        &format!("missing: {:?}", outcome.client_events),
    );
}

// A mode 1 Client drops an unauthenticated Null Request, an unauthenticated
// Client a signed one.
#[udpstp_req::req("RFC9946-6.2-1", "RFC9946-6.2-2", "RFC9946-6.2.2-6")]
#[test]
fn a_null_request_in_the_wrong_mode_is_dropped() {
    // authMode flipped to 0xFE on the way, from 1 or from 0.
    for (seed, scenario) in [(6, mode1(6)), (7, unauthenticated(7))] {
        let outcome = scenario
            .network_flips_byte([0xDE, 0xAD], 7)
            .run_for(Duration::from_millis(50));
        outcome.check(
            outcome.client_events.contains(&Event::NullRequestIgnored {
                reason: ResponseIgnoreReason::AuthenticationFailed,
            }) && outcome.client_activated_direction(0).is_some(),
            &format!("seed {seed}: {:?}", outcome.client_events),
        );
    }
}
