//! The Setup exchange between a Client and a Server (RFC 9946 §6), and the
//! Server's silence on a datagram that isn't a Setup Request (§6.2.1): the
//! wrong size, another PDU ID or another cmdRequest.

mod harness;

use std::net::SocketAddr;
use std::time::Duration;

use harness::{Outcome, Scenario, SilentRejectionReason};

// Acceptance: ACKOK with the ephemeral testPort.
#[udpstp_req::req("RFC9946-6.1-1", "RFC9097-10-2", "RFC9946-11-2", "RFC9946-12.3.4-L1")]
#[test]
fn unauthenticated_setup_ends_in_ack_with_a_test_port_when_both_ends_opt_in() {
    let outcome = Scenario::new(1)
        .server_allows_unauthenticated()
        .client_unauthenticated()
        .run_for(Duration::from_secs(1));

    let test_port = outcome.client_accepted_test_port();
    outcome.check(test_port.is_some(), "the Client's Setup was accepted");
    outcome.check(test_port != Some(0), "the accepted testPort is non-zero");
}

// docs/spec.md Wire format and authentication (Unauthenticated Mode only when
// explicitly enabled on both ends): the Server is off by default.
#[test]
fn server_without_opt_in_does_not_accept_an_unauthenticated_setup() {
    let outcome = Scenario::new(2)
        .client_unauthenticated()
        .run_for(Duration::from_secs(1));

    outcome.check(
        !outcome.sent_by(harness::CLIENT_ADDR).is_empty(),
        "the Client sent a Setup Request",
    );
    outcome.check(
        outcome.client_accepted_test_port().is_none(),
        "the Setup was not accepted",
    );
    outcome.check(
        outcome.sent_by(harness::SERVER_ADDR).is_empty(),
        "the Server sent nothing back",
    );
    outcome.check(
        outcome.server_dropped_unauthenticated_setup(),
        "the Server reported dropping an unauthenticated Setup Request",
    );
}

// docs/spec.md Wire format and authentication (Unauthenticated Mode only when
// explicitly enabled on both ends): the Client is off by default, so with no
// authentication configured no Test starts.
#[test]
fn client_without_opt_in_sends_nothing_and_reports_why() {
    let outcome = Scenario::new(3)
        .server_allows_unauthenticated()
        .run_for(Duration::from_secs(1));

    outcome.check(outcome.trace.is_empty(), "no datagram was sent");
    outcome.check(
        outcome.client_reported_no_authentication(),
        "the Client reported that no authentication is configured",
    );
}

// docs/spec.md Control Phase and admission (admission released on every exit
// path of a Test Connection): the runtime reports the socket gone, and the
// Server releases what it held for it.
#[test]
fn server_releases_a_test_connection_when_its_socket_closes() {
    let outcome = Scenario::new(4)
        .server_allows_unauthenticated()
        .client_unauthenticated()
        .runtime_closes_test_connections()
        .run_for(Duration::from_secs(1));

    outcome.check(
        outcome.server_admitted_count() == 1,
        "one Test Connection was admitted",
    );
    outcome.check(
        outcome.server_released_count() == 1,
        "that Test Connection was released",
    );
}

/// An unauthenticated Setup Request (RFC 9946 Figure 3): pduId 0xACE1,
/// protocolVer 20, mcCount 1, mcIdent 0x1234, cmdRequest
/// CHSR_CREQ_SETUPREQ, modifierBitmap CHSR_JUMBO_STATUS; 56 bytes.
fn setup_request() -> Vec<u8> {
    let mut b = vec![0u8; 56];
    b[0..2].copy_from_slice(&[0xAC, 0xE1]);
    b[2..4].copy_from_slice(&20u16.to_be_bytes());
    b[5] = 1;
    b[6..8].copy_from_slice(&0x1234u16.to_be_bytes());
    b[8] = 1;
    b[14] = 0x01;
    b
}

/// A Server that allows Unauthenticated Mode, sent `request` by a scripted
/// Client.
fn sent(seed: u64, request: Vec<u8>) -> Outcome {
    Scenario::new(seed)
        .server_allows_unauthenticated()
        .scripted_client(move |_| request)
        .run_for(Duration::from_millis(100))
}

/// Whether the Server sent nothing at all, from any port, and reported
/// the request as malformed.
fn silent(outcome: &Outcome) -> bool {
    let server: SocketAddr = harness::SERVER_ADDR.parse().unwrap();
    outcome.trace.iter().all(|t| t.source.ip() != server.ip())
        && outcome.server_admitted_count() == 0
        && outcome.server_dropped_setup_because(SilentRejectionReason::Malformed)
}

// The scripted request itself is answered, so each change below is what
// makes the Server silent.
#[test]
fn the_scripted_setup_request_is_answered() {
    let outcome = sent(5, setup_request());
    outcome.check(
        outcome.sent_by(harness::SERVER_ADDR).len() == 1 && outcome.server_admitted_count() == 1,
        "the Server answered",
    );
}

#[udpstp_req::req("RFC9946-6.2.1-5")]
#[test]
fn a_setup_request_of_the_wrong_size_gets_no_reply() {
    let short = setup_request()[..55].to_vec();
    let mut long = setup_request();
    long.push(0);
    let mut padded = setup_request();
    padded.resize(104, 0);
    for (seed, request) in [(6, short), (7, long), (8, padded)] {
        let len = request.len();
        let outcome = sent(seed, request);
        outcome.check(silent(&outcome), &format!("{len} bytes"));
    }
}

#[udpstp_req::req("RFC9946-6.2.1-5")]
#[test]
fn a_setup_request_with_another_pdu_id_gets_no_reply() {
    for (seed, pdu_id) in [(9, [0xAC, 0xE2]), (10, [0xDE, 0xAD]), (11, [0xE1, 0xAC])] {
        let mut request = setup_request();
        request[0..2].copy_from_slice(&pdu_id);
        let outcome = sent(seed, request);
        outcome.check(silent(&outcome), &format!("pduId {pdu_id:02x?}"));
    }
}

// CHSR_CREQ_NONE is unused, so a request carrying it is not a Setup
// Request; neither is one carrying CHSR_CREQ_SETUPRSP.
#[udpstp_req::req("RFC9946-6.1-L5")]
#[test]
fn a_setup_pdu_whose_cmd_request_is_not_setup_request_gets_no_reply() {
    for (seed, cmd_request) in [(12, 0), (13, 2)] {
        let mut request = setup_request();
        request[8] = cmd_request;
        let outcome = sent(seed, request);
        outcome.check(silent(&outcome), &format!("cmdRequest {cmd_request}"));
    }
}
