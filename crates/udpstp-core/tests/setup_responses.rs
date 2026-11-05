//! The Client's handling of Setup Responses (RFC 9946 §6.3; docs/spec.md
//! stories 16 and 18): each verified rejection code ends the Setup attempt
//! with its own typed failure, an undefined code ends it too, one with the
//! wrong protocol version, PDU ID or cmdRequest is ignored, and a response
//! that fails verification is ignored, leaving the Test initiation timer to
//! decide, though an unverified rejection's code is reported.
//!
//! A scripted mode 1 Server answers the Client's Setup Request with a
//! response built byte by byte from RFC 9946 Figure 3.

mod harness;

use std::time::Duration;

use harness::{ControlExchange, ControlPhaseFailure, Outcome, ResponseIgnoreReason, Scenario};
use udpstp_core::{Direction, Event, SetupRejectionCode};
use udpstp_wire::auth::{derive_keys, hmac_sha256};

const KEY_ID: u8 = 7;
const SHARED_KEY: &[u8] = b"a shared key";
const RUN: Duration = Duration::from_millis(100);

const DIGEST: std::ops::Range<usize> = 20..52;
const AUTH_TIME: std::ops::Range<usize> = 16..20;

/// A Setup Response to `request` with `cmd_response`, signed with the
/// Server key of the request's authUnixTime when `signed`.
fn response(request: &[u8], cmd_response: u8, protocol_version: u16, signed: bool) -> Vec<u8> {
    let mut b = request.to_vec();
    b[2..4].copy_from_slice(&protocol_version.to_be_bytes());
    b[8] = 2;
    b[9] = cmd_response;
    b[DIGEST].fill(0);
    b[54..56].fill(0);
    if signed {
        let time = u32::from_be_bytes(request[AUTH_TIME].try_into().unwrap());
        let keys = derive_keys(SHARED_KEY, time);
        let digest = hmac_sha256(keys.server(), &b);
        b[DIGEST].copy_from_slice(&digest);
    } else {
        b[AUTH_TIME].fill(0);
        b[52] = 0;
    }
    b
}

/// A Server that answers with `cmd_response`: protocolVer 20, or 21 in a
/// BADVER rejection, which carries the version the Server expects
/// (RFC 9946 §6.2.1).
fn answered(seed: u64, cmd_response: u8, signed: bool, run: Duration) -> Outcome {
    let protocol_version = if cmd_response == 2 { 21 } else { 20 };
    Scenario::new(seed)
        .client_mode1(KEY_ID, SHARED_KEY)
        .scripted_server(move |_, pdu| {
            (pdu.starts_with(&[0xAC, 0xE1]) && pdu[8] == 1)
                .then(|| response(pdu, cmd_response, protocol_version, signed))
        })
        .run_for(run)
}

fn failure(outcome: &Outcome) -> Option<ControlPhaseFailure> {
    outcome.clients_events[0].iter().find_map(|e| match e {
        Event::ControlPhaseFailed(failure) => Some(*failure),
        _ => None,
    })
}

// docs/spec.md story 16 (each rejection code yields a distinct typed failure)
#[udpstp_req::req("RFC9946-6.3-3", "RFC9946-12.3.5-L1")]
#[test]
fn each_rejection_code_ends_the_setup_with_its_own_failure() {
    let codes = [
        (2, SetupRejectionCode::BadVersion { expected: 21 }),
        (3, SetupRejectionCode::JumboMismatch),
        (4, SetupRejectionCode::AuthNotConfigured),
        (5, SetupRejectionCode::AuthRequired),
        (6, SetupRejectionCode::AuthModeInvalid),
        (7, SetupRejectionCode::AuthFailed),
        (8, SetupRejectionCode::AuthTimeInvalid),
        (9, SetupRejectionCode::MaxBandwidthRequired),
        (10, SetupRejectionCode::CapacityExceeded),
        (11, SetupRejectionCode::TraditionalMtuMismatch),
        (12, SetupRejectionCode::MultiConnectionInvalid),
        (13, SetupRejectionCode::ConnectionAllocationFailure),
    ];
    for (cmd_response, code) in codes {
        let outcome = answered(1, cmd_response, true, RUN);
        outcome.check(
            failure(&outcome) == Some(ControlPhaseFailure::SetupRejected(code))
                && code.cmd_response() == cmd_response,
            &format!("{cmd_response}: {:?}", failure(&outcome)),
        );
    }
}

#[udpstp_req::req("RFC9946-6.3-4")]
#[test]
fn an_undefined_cmd_response_ends_the_setup_attempt() {
    for cmd_response in [0, 14, 99, 255] {
        let outcome = answered(2, cmd_response, true, RUN);
        outcome.check(
            failure(&outcome) == Some(ControlPhaseFailure::UnknownSetupResponse { cmd_response }),
            &format!("{cmd_response}: {:?}", failure(&outcome)),
        );
    }
}

/// A change to a Setup Response before it is signed.
type Edit = fn(&mut [u8]);

/// A signed Setup Response to `request` accepting it with `test_port`,
/// changed by `edit` before it is signed.
fn signed_acceptance(request: &[u8], test_port: u16, edit: Edit) -> Vec<u8> {
    let mut b = response(request, 1, 20, false);
    b[AUTH_TIME].copy_from_slice(&request[AUTH_TIME]);
    b[52] = request[52];
    b[12..14].copy_from_slice(&test_port.to_be_bytes());
    edit(&mut b);
    let time = u32::from_be_bytes(request[AUTH_TIME].try_into().unwrap());
    let digest = hmac_sha256(derive_keys(SHARED_KEY, time).server(), &b);
    b[DIGEST].copy_from_slice(&digest);
    b
}

// A Setup Response with another protocol version, PDU ID or cmdRequest is
// not one: the Client ignores it and accepts the valid one that follows.
// Each is signed, so only the field under test is wrong. A BADVER
// rejection carries the Server's version instead, and still counts
// (each_rejection_code_ends_the_setup_with_its_own_failure).
#[udpstp_req::req("RFC9946-6.3-2")]
#[test]
fn a_setup_response_with_the_wrong_version_pdu_id_or_cmd_request_is_ignored() {
    let cases: [(&str, Edit); 5] = [
        ("protocolVer 21", |b| {
            b[2..4].copy_from_slice(&21u16.to_be_bytes())
        }),
        ("protocolVer 19", |b| {
            b[2..4].copy_from_slice(&19u16.to_be_bytes())
        }),
        ("pduId 0xACE2", |b| b[0..2].copy_from_slice(&[0xAC, 0xE2])),
        ("cmdRequest CHSR_CREQ_SETUPREQ", |b| b[8] = 1),
        ("cmdRequest CHSR_CREQ_NONE", |b| b[8] = 0),
    ];
    for (what, edit) in cases {
        let outcome = Scenario::new(8)
            .client_mode1(KEY_ID, SHARED_KEY)
            .scripted_server_replying_many(move |_, pdu| {
                if !(pdu.starts_with(&[0xAC, 0xE1]) && pdu[8] == 1) {
                    return Vec::new();
                }
                vec![
                    signed_acceptance(pdu, 1111, edit),
                    signed_acceptance(pdu, 2222, |_| {}),
                ]
            })
            .run_for(RUN);
        outcome.check(
            outcome.client_accepted_test_port() == Some(2222)
                && outcome.activation_requests_sent_to_port(1111) == 0
                && failure(&outcome).is_none(),
            what,
        );
    }
}

// docs/spec.md (responses failing verification are ignored and the initiation
// timer decides)
#[udpstp_req::req("RFC9946-6.3-1", "RFC9946-6.1-2")]
#[test]
fn an_unverified_rejection_is_reported_but_the_timer_decides() {
    let outcome = answered(3, 7, false, Duration::from_millis(3100));
    let events = &outcome.clients_events[0];
    outcome.check(
        events.contains(&Event::UnverifiedSetupRejection {
            cmd_response: 7,
            protocol_version: 20,
            unsigned: true,
        }) && failure(&outcome)
            == Some(ControlPhaseFailure::TestInitiationTimedOut {
                exchange: ControlExchange::Setup,
                last_ignored: Some(ResponseIgnoreReason::AuthenticationFailed),
            }),
        &format!("{events:?}"),
    );
}

// Conformance Record: setup-response-from-another-address. A verified
// digest does not authorize the unsigned source address as a destination.
#[test]
fn a_verified_setup_response_from_an_unapproved_address_is_ignored() {
    for mode in [1, 2] {
        for ipv6 in [false, true] {
            for connections in [1, 4] {
                let (scenario, alias) = alternate_scenario(mode, ipv6, connections, false);
                let outcome = scenario.run_for(Duration::from_millis(500));
                outcome.check(
                    outcome.client_accepted_test_port().is_none()
                        && outcome.client_activated_direction(0).is_none()
                        && outcome.client_data_events.is_empty()
                        && outcome
                            .trace
                            .iter()
                            .all(|pdu| pdu.destination.ip() != alias),
                    &format!("mode {mode}, IPv6 {ipv6}, {connections} Test Connections"),
                );
            }
        }
    }
}

const ALIAS: &str = "192.0.2.77";

fn alternate_scenario(
    mode: u8,
    ipv6: bool,
    connections: u8,
    authorized: bool,
) -> (Scenario, std::net::IpAddr) {
    let alias = if ipv6 { "2001:db8::77" } else { ALIAS }.parse().unwrap();
    let mut scenario = Scenario::new(8)
        .mode1(KEY_ID, SHARED_KEY)
        .client_direction(Direction::Upstream)
        .coordinated(connections, connections)
        .server_answers_from(alias);
    if mode == 2 {
        scenario = scenario.client_mode2(KEY_ID, SHARED_KEY);
    }
    if ipv6 {
        scenario = scenario.ipv6();
    }
    if authorized {
        scenario = scenario.test_plan(move |plan| {
            plan.alternate_server_addresses
                .push((plan.servers[0], alias));
        });
    }
    (scenario, alias)
}

// Conformance Record: setup-response-from-another-address. The Client
// user authorizes this Server's alternate address before receiving PDUs.
#[test]
fn a_verified_setup_response_from_an_explicitly_authorized_address_is_accepted() {
    for mode in [1, 2] {
        for ipv6 in [false, true] {
            for connections in [1, 4] {
                let (scenario, alias) = alternate_scenario(mode, ipv6, connections, true);
                let outcome = scenario.run_for(Duration::from_millis(500));
                let activated = outcome
                    .client_connection_events
                    .iter()
                    .filter(|(_, event)| {
                        matches!(
                            event,
                            Event::TestActivated {
                                direction: Direction::Upstream,
                                ..
                            }
                        )
                    })
                    .count();
                outcome.check(
                    activated == usize::from(connections)
                        && outcome.trace.iter().any(|pdu| {
                            pdu.destination.ip() == alias && pdu.payload.starts_with(&[0xBE, 0xEF])
                        }),
                    &format!("mode {mode}, IPv6 {ipv6}, {connections} Test Connections"),
                );
            }
        }
    }
}

// Conformance Record: setup-response-from-another-address. Alternates
// of another selected Server cannot authorize this Test Connection.
#[test]
fn alternate_authorization_is_scoped_to_the_selected_server() {
    let other = harness::extra_server_addr(1, false);
    for allow in [false, true] {
        let outcome = Scenario::new(9)
            .mode1(KEY_ID, SHARED_KEY)
            .server_answers_from(other.ip())
            .test_plan(move |plan| {
                plan.servers.push(other);
                if allow {
                    plan.alternate_server_addresses.push((other, other.ip()));
                }
            })
            .run_for(RUN);
        outcome.check(
            outcome.client_accepted_test_port().is_none(),
            "per-Server authorization",
        );
    }
}

// Conformance Record: setup-response-from-another-address. Even explicit
// alternates retain ADR 0009's verification and request correlation.
#[test]
fn an_authorized_alternate_still_needs_a_verified_matching_response() {
    let edits: [Edit; 5] = [
        |pdu| pdu[4] ^= 1, // mcIndex
        |pdu| pdu[6] ^= 1, // mcIdent
        |pdu| pdu[15] = 0, // authMode
        |pdu| pdu[AUTH_TIME].fill(0),
        |pdu| pdu[12..14].fill(0), // no Ephemeral Port
    ];
    for edit in edits {
        let (scenario, _) = alternate_scenario(1, false, 1, true);
        let outcome = scenario
            .scripted_server(move |_, pdu| {
                (pdu.starts_with(&[0xAC, 0xE1]) && pdu[8] == 1)
                    .then(|| signed_acceptance(pdu, 50000, edit))
            })
            .run_for(RUN);
        outcome.check(
            outcome.client_accepted_test_port().is_none(),
            "verification and correlation",
        );
    }
    let (scenario, _) = alternate_scenario(2, true, 1, true);
    let outcome = scenario
        .scripted_server(|_, pdu| {
            (pdu.starts_with(&[0xAC, 0xE1]) && pdu[8] == 1).then(|| {
                let mut response = signed_acceptance(pdu, 50000, |_| {});
                response[20] ^= 1;
                response
            })
        })
        .run_for(RUN);
    outcome.check(
        outcome.client_accepted_test_port().is_none(),
        "invalid digest",
    );
}

// Conformance Record: setup-response-from-another-address. The selected
// IPv6 interface is retained; a response cannot select another interface.
#[test]
fn an_alternate_keeps_the_locally_selected_ipv6_scope() {
    let alias = "2001:db8::77".parse().unwrap();
    for scope in [7, 8] {
        let outcome = Scenario::new(10)
            .ipv6()
            .mode1(KEY_ID, SHARED_KEY)
            .client_direction(Direction::Upstream)
            .client_server_address("[2001:db8::1%7]:24601".parse().unwrap())
            .server_answers_from(alias)
            .server_answers_with_scope(scope)
            .test_plan(move |plan| {
                plan.alternate_server_addresses
                    .push((plan.servers[0], alias));
            })
            .run_for(Duration::from_millis(500));
        outcome.check(
            outcome.client_activated_direction(0).is_some() == (scope == 7),
            "only the locally selected scope is authorized",
        );
        if scope == 7 {
            outcome.check(
                outcome.trace.iter().any(|pdu| {
                    matches!(pdu.destination, std::net::SocketAddr::V6(addr)
                    if addr.ip() == &alias && addr.scope_id() == 7)
                        && pdu.payload.starts_with(&[0xBE, 0xEF])
                }),
                "Load PDUs retain the locally selected scope",
            );
        }
    }
}

// From another address, a response that fails verification is ignored
// without a trace: only the Server's own address can explain a failure.
#[test]
fn an_unverified_setup_response_from_another_address_leaves_no_trace() {
    let outcome = Scenario::new(6)
        .client_mode1(KEY_ID, SHARED_KEY)
        .server_answers_from(ALIAS.parse().unwrap())
        .scripted_server(move |_, pdu| {
            (pdu.starts_with(&[0xAC, 0xE1]) && pdu[8] == 1).then(|| response(pdu, 7, 20, false))
        })
        .run_for(RUN);
    outcome.check(
        outcome.clients_events[0].is_empty(),
        &format!("{:?}", outcome.clients_events[0]),
    );
}

// Only a rejection of this Test Connection's own request (mcIdent and
// mcIndex) is reported.
#[test]
fn an_unverified_rejection_for_another_test_connection_is_not_reported() {
    let outcome = Scenario::new(7)
        .client_mode1(KEY_ID, SHARED_KEY)
        .scripted_server(move |_, pdu| {
            (pdu.starts_with(&[0xAC, 0xE1]) && pdu[8] == 1).then(|| {
                let mut other = response(pdu, 7, 20, false);
                other[4] ^= 1;
                other
            })
        })
        .run_for(RUN);
    let reported = outcome.clients_events[0]
        .iter()
        .any(|e| matches!(e, Event::UnverifiedSetupRejection { .. }));
    outcome.check(!reported, &format!("{:?}", outcome.clients_events[0]));
}

// Unauthenticated, nothing proves where a response came from, so only the
// Server's own address counts.
#[test]
fn an_unauthenticated_setup_response_from_another_address_is_ignored() {
    for ipv6 in [false, true] {
        let (scenario, _) = alternate_scenario(1, ipv6, 4, true);
        let outcome = scenario
            .server_allows_unauthenticated()
            .client_unauthenticated()
            .run_for(RUN);
        outcome.check(
            outcome.client_activated_direction(0).is_none()
                && failure(&outcome).is_none()
                && outcome.clients_events[0].is_empty(),
            &format!("{:?}", outcome.clients_events[0]),
        );
    }
}
