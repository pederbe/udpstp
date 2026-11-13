//! Authentication Mode 2 (RFC 9946 §5.3.2): every Status PDU is
//! authenticated. The Status PDU sender, the Client in a Downstream Test and
//! the Server in an Upstream one, signs it with its own Derived Key; the
//! receiver ignores one that fails, and its watchdog ends the Test if they
//! keep failing. The digests are recomputed here from RFC 9946's text,
//! independently of the code under test.

use super::harness;

use std::time::Duration;

use crate::protocol::{Direction, EndReason};
use crate::wire::auth::{derive_keys, hmac_sha256};
use harness::{DataEvent, Outcome, RowRequest, Scenario, TraceEntry};

const KEY: &[u8] = b"a mode 2 key";
const STATUS: [u8; 2] = [0xFE, 0xED];

fn test(seed: u64, direction: Direction) -> Scenario {
    Scenario::new(seed)
        .operator_mode2(7, KEY)
        .client_direction(direction)
        .client_requests_row(RowRequest::FixedRate(5))
}

fn statuses(outcome: &Outcome) -> Vec<&TraceEntry> {
    outcome
        .trace
        .iter()
        .filter(|t| t.payload.starts_with(&STATUS) && t.len == 204)
        .collect()
}

/// The Client's and the Server's Derived Keys, from the first Setup
/// Request's authUnixTime (RFC 9946 §5.4.1).
fn keys(outcome: &Outcome) -> ([u8; 32], [u8; 32]) {
    let setup = outcome
        .trace
        .iter()
        .find(|t| t.payload.starts_with(&[0xAC, 0xE1]) && t.payload[8] == 1)
        .expect("a Setup Request");
    let time = u32::from_be_bytes(setup.payload[16..20].try_into().unwrap());
    let keys = derive_keys(KEY, time);
    (*keys.client(), *keys.server())
}

/// The authDigest recomputed: HMAC-SHA-256 of the whole PDU with the
/// authDigest (168-199) and checkSum (202-203) zeroed.
fn digest_is(pdu: &[u8], key: &[u8; 32]) -> bool {
    let mut zeroed = pdu.to_vec();
    zeroed[168..200].fill(0);
    zeroed[202..204].fill(0);
    hmac_sha256(key, &zeroed)[..] == pdu[168..200]
}

// The Client signs Status PDUs in a Downstream Test, the Server in an
// Upstream one.
#[udpstp_req::req(
    "RFC9946-5.3.2-1",
    "RFC9946-5.3.2-2",
    "RFC9946-5.3.2-3",
    "RFC9946-5.4.1-6",
    "RFC9097-10-3",
    "RFC9946-11-3",
    "RFC9946-5.3.2-L1",
    "RFC9946-5.4-1",
    "RFC9946-8.2-L1"
)]
#[test]
fn each_status_pdu_is_signed_with_its_sender_s_key() {
    for (seed, direction) in [(1, Direction::Downstream), (2, Direction::Upstream)] {
        let outcome = test(seed, direction).run_for(Duration::from_millis(600));
        let (client, server) = keys(&outcome);
        let sender = match direction {
            Direction::Downstream => client,
            Direction::Upstream => server,
        };
        let statuses = statuses(&outcome);
        outcome.check(
            !statuses.is_empty()
                && statuses.iter().all(|t| {
                    t.payload[163] == 2
                        && t.payload[200] == 7
                        && t.payload[164..168] != [0; 4]
                        && digest_is(&t.payload, &sender)
                        && !digest_is(&t.payload, if sender == client { &server } else { &client })
                }),
            &format!("{direction:?}: {} Status PDUs", statuses.len()),
        );
        let failed = outcome
            .server_data_events
            .iter()
            .chain(&outcome.client_data_events)
            .any(|(_, e)| matches!(e, DataEvent::StatusPdusFailedVerification { .. }));
        outcome.check(
            outcome.client_activated_direction(0) == Some(direction) && !failed,
            "the Test ran, and every Status PDU verified",
        );
    }
}

// RFC 9946 §9 (a Client that ends a Test sends STOP2); in mode 2 that
// Status PDU is authenticated too, so the Server accepts it
#[udpstp_req::req("RFC9946-5.3.2-1")]
#[test]
fn a_client_s_early_stop_is_authenticated() {
    // The Server coerces CS6 to 0 even for an Operator Key, which this
    // Client refuses.
    let outcome = test(6, Direction::Downstream)
        .client_config(|c| c.dscp = 48)
        .client_config(|c| {
            c.unacceptable_coercions
                .push(harness::ActivationParameter::DscpEcn)
        })
        .run_for(Duration::from_millis(600));
    let stopped = outcome
        .server_data_events
        .iter()
        .any(|(_, e)| *e == DataEvent::Ended(EndReason::StoppedByClient));
    outcome.check(stopped, &format!("{:?}", outcome.server_data_events));
}

#[udpstp_req::req("RFC9946-5.3.2-4", "RFC9946-5.3.2-5")]
#[test]
fn failing_status_pdus_are_ignored_until_the_watchdog_ends_the_test() {
    // Every Status PDU arrives with one bit of its digest flipped.
    for (seed, direction) in [(3, Direction::Downstream), (4, Direction::Upstream)] {
        let outcome = test(seed, direction)
            .network_flips_byte(STATUS, 180)
            .run_for(Duration::from_secs(5));
        let (receiver_events, receiver_ended) = match direction {
            // The Server receives the Client's Status PDUs.
            Direction::Downstream => (&outcome.server_data_events, "Server"),
            Direction::Upstream => (&outcome.client_data_events, "Client"),
        };
        let failed: u32 = receiver_events
            .iter()
            .filter_map(|(_, e)| match e {
                DataEvent::StatusPdusFailedVerification { count } => Some(*count),
                _ => None,
            })
            .sum();
        let watchdog = receiver_events
            .iter()
            .any(|(_, e)| *e == DataEvent::Ended(EndReason::Watchdog));
        outcome.check(
            failed > 0 && watchdog,
            &format!("{direction:?}: the {receiver_ended} ignored {failed} and its watchdog ended it: {watchdog}"),
        );
    }
}

#[udpstp_req::req("RFC9946-5.3.2-6")]
#[test]
fn in_mode_1_the_status_pdu_s_authentication_fields_are_zero() {
    let outcome = Scenario::new(5)
        .operator(7, KEY)
        .client_requests_row(RowRequest::FixedRate(5))
        .run_for(Duration::from_millis(600));
    let statuses = statuses(&outcome);
    outcome.check(
        !statuses.is_empty()
            && statuses.iter().all(|t| {
                t.payload[163] == 1
                    && t.payload[164..168] == [0; 4]
                    && t.payload[168..200] == [0; 32]
                    && t.payload[200] == 0
            }),
        &format!("{} Status PDUs", statuses.len()),
    );
}
