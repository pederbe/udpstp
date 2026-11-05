//! Multi-Connection checks (RFC 9946 §4, §6.1; docs/spec.md "Control Phase and
//! admission (Server)"): the Server refuses with MCINVPAR a Setup Request
//! whose mcIdent is zero, whose mcIndex isn't below its mcCount or is
//! already in use in the Test, or that disagrees with the Test's other Test
//! Connections on mcCount, authMode or keyId. It never waits for every
//! mcIndex: a Test may span several Servers.

mod harness;

use std::net::SocketAddr;
use std::time::Duration;

use harness::requests::{self, Auth, TestConnection};
use harness::{Outcome, Scenario, ScriptClock, SilentRejectionReason};
use udpstp_core::{Direction, Event, MultiConnectionError, SetupRejection};

const MCINVPAR: u8 = 12;
const RUN: Duration = Duration::from_millis(100);
const OPERATOR_KEY: &[u8] = b"an operator key";

fn activated(outcome: &Outcome) -> usize {
    (0..outcome.clients_events.len())
        .filter(|&i| outcome.client_activated_direction(i).is_some())
        .count()
}

fn operator() -> Scenario {
    Scenario::new(1).operator(7, OPERATOR_KEY)
}

/// Unauthenticated: no replay cache.
fn unauthenticated() -> Scenario {
    Scenario::new(1)
        .server_allows_unauthenticated()
        .client_unauthenticated()
}

#[udpstp_req::req("RFC9946-6.1-L3")]
#[test]
fn a_zero_mc_ident_is_refused() {
    // Unauthenticated, so the network's change isn't caught by the digest.
    let outcome = Scenario::new(2)
        .server_allows_unauthenticated()
        .client_unauthenticated()
        .network_writes_setup(0, 6, &[0, 0])
        .run_for(RUN);
    // The response echoes mcIdent 0, which isn't the Client's own, so only
    // the Server's side is seen.
    outcome.check(
        outcome
            .server_events
            .contains(&Event::SetupRequestRejected {
                from: harness::CLIENT_ADDR.parse().unwrap(),
                reason: SetupRejection::MultiConnection(MultiConnectionError::IdentZero),
            })
            && outcome
                .trace
                .iter()
                .any(|t| t.payload.starts_with(&[0xAC, 0xE1]) && t.payload[9] == MCINVPAR),
        &format!("{:?}", outcome.server_events),
    );
}

// docs/spec.md (mcIndex unique): the Server refuses, with a signed MCINVPAR, a
// Setup Request for an mcIndex another Test Connection of the Test uses,
// from another port. Our Multi-Connection Coordinator never sends one, so
// two scripted Clients play the Test in mode 1, both as mcIndex 0; the
// second states a maxBandwidth, else its Setup Request would be identical
// to the first's and the replay cache would drop it first. Our Client's
// handling of MCINVPAR is setup_responses.rs's, and the other
// Multi-Connection checks are Admission's own tests (src/admission.rs)
#[test]
fn an_mc_index_already_in_use_in_the_test_is_refused() {
    let auth = Auth::Mode1 {
        key_id: 7,
        key: OPERATOR_KEY,
    };
    let setup = |max_bandwidth| {
        let tc = TestConnection {
            mc_ident: 0x1234,
            mc_index: 0,
            mc_count: 2,
            max_bandwidth,
        };
        move |clock: ScriptClock| {
            requests::setup_request(tc, Direction::Downstream, auth, clock.unix_secs as u32)
        }
    };
    let outcome = operator()
        .scripted_client(setup(0))
        .add_scripted_client(Duration::from_millis(10), setup(5), |_, _| None)
        .run_for(RUN);
    // What each Client's Setup Request was answered with.
    let answers = |client: SocketAddr| -> Vec<u8> {
        outcome
            .trace
            .iter()
            .filter(|t| t.destination == client)
            .filter_map(|t| requests::setup_response_code(&t.payload))
            .collect()
    };
    let first: SocketAddr = harness::CLIENT_ADDR.parse().unwrap();
    let second = SocketAddr::new(first.ip(), first.port() + 1);
    let refused = outcome
        .server_events
        .iter()
        .filter(|e| {
            **e == Event::SetupRequestRejected {
                from: second,
                reason: SetupRejection::MultiConnection(MultiConnectionError::IndexInUse),
            }
        })
        .count();
    outcome.check(
        refused == 1 && answers(first) == [ACK_OK] && answers(second) == [MCINVPAR],
        &format!("{:?}", outcome.server_events),
    );
    // The refusal is signed with the Server Authentication Key of the
    // second's own Setup Request: the Shared Key and its authUnixTime.
    let sent_time = outcome
        .trace
        .iter()
        .filter(|t| t.source == second)
        .find_map(|t| requests::setup_request_time(&t.payload));
    let refusal = outcome
        .trace
        .iter()
        .find(|t| t.destination == second && requests::setup_response_code(&t.payload).is_some());
    outcome.check(
        sent_time.zip(refusal).is_some_and(|(time, refusal)| {
            requests::setup_response_signed(&refusal.payload, OPERATOR_KEY, time)
        }),
        "the MCINVPAR is signed with the Server Authentication Key",
    );
}

/// cmdResponse CHSR_CRSP_ACKOK (RFC 9946 §12.3.5).
const ACK_OK: u8 = 1;

// A Setup Request the network duplicates is answered once, not refused as
// an mcIndex in use, which would end the Client's Test. In mode 1 the
// replay cache drops the copy; in Unauthenticated Mode the duplicate
// rule does.
#[test]
fn a_duplicated_setup_request_is_answered_once() {
    for (scenario, replay) in [(unauthenticated(), false), (operator(), true)] {
        let outcome = scenario.network_duplicates_setup_requests().run_for(RUN);
        let dropped = if replay {
            outcome.server_dropped_setup_because(SilentRejectionReason::Replayed)
        } else {
            outcome.server_ignored_setup_because(harness::SetupIgnoreReason::DuplicateSetupRequest)
        };
        outcome.check(
            dropped && outcome.server_admitted_count() == 1 && activated(&outcome) == 1,
            &format!("replay {replay}: {:?}", outcome.server_events),
        );
    }
}
