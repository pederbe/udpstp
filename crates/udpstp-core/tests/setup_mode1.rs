//! The Setup exchange in Authentication Mode 1 (RFC 9946 §5.3.1, §5.4.1,
//! §6.2, §6.3).
//!
//! Scripted peers build their PDUs byte by byte from RFC 9946 Figure 3, sign
//! them with HMAC-SHA-256 and the KDF (both pinned by independent known-answer
//! vectors), and check what our Client or Server sends back. They don't
//! use our Setup PDU codec or signing code.

mod harness;

use std::time::Duration;

use harness::{Scenario, ScriptClock};
use udpstp_core::{ResponseIgnoreReason, SilentRejectionReason};
use udpstp_wire::auth::{derive_keys, hmac_sha256};

const KEY_ID: u8 = 7;
const SHARED_KEY: &[u8] = b"a shared key\0with a zero byte";
const RUN: Duration = Duration::from_secs(1);

// ---- Test Setup PDU from RFC 9946 Figure 3 ----

const LEN: usize = 56;
const CMD_REQUEST: usize = 8;
const CMD_RESPONSE: usize = 9;
const TEST_PORT: std::ops::Range<usize> = 12..14;
const AUTH_MODE: usize = 15;
const AUTH_TIME: std::ops::Range<usize> = 16..20;
const DIGEST: std::ops::Range<usize> = 20..52;
const KEY_ID_AT: usize = 52;
const CHECKSUM: std::ops::Range<usize> = 54..56;

/// A mode 1 Setup Request, unsigned: pduId 0xACE1, protocolVer 20, mcIndex 0,
/// mcCount 1, mcIdent 0x1234, cmdRequest CHSR_CREQ_SETUPREQ, modifierBitmap
/// CHSR_JUMBO_STATUS, authMode AUTHMODE_1.
fn setup_request(auth_unix_time: u32, key_id: u8) -> [u8; LEN] {
    let mut b = [0u8; LEN];
    b[0..2].copy_from_slice(&0xACE1u16.to_be_bytes());
    b[2..4].copy_from_slice(&20u16.to_be_bytes());
    b[5] = 1;
    b[6..8].copy_from_slice(&0x1234u16.to_be_bytes());
    b[CMD_REQUEST] = 1;
    b[14] = 0x01;
    b[AUTH_MODE] = 1;
    b[AUTH_TIME].copy_from_slice(&auth_unix_time.to_be_bytes());
    b[KEY_ID_AT] = key_id;
    b
}

/// The PDU with authDigest and checkSum zeroed, as the HMAC input (§5.3.1).
fn zeroed(pdu: &[u8]) -> Vec<u8> {
    let mut b = pdu.to_vec();
    b[DIGEST].fill(0);
    b[CHECKSUM].fill(0);
    b
}

/// Fills authDigest: HMAC-SHA-256 of the whole PDU with authDigest and
/// checkSum preset to zero (§5.3.1).
fn sign(pdu: &mut [u8], key: &[u8]) {
    let digest = hmac_sha256(key, &zeroed(pdu));
    pdu[DIGEST].copy_from_slice(&digest);
}

fn digest_verifies(pdu: &[u8], key: &[u8]) -> bool {
    hmac_sha256(key, &zeroed(pdu)) == pdu[DIGEST]
}

fn auth_unix_time(pdu: &[u8]) -> u32 {
    u32::from_be_bytes(pdu[AUTH_TIME].try_into().unwrap())
}

/// The RFC 1071 Internet checksum, written into checkSum as the last step (§5.6).
fn fill_checksum(pdu: &mut [u8]) {
    pdu[CHECKSUM].fill(0);
    let mut sum: u32 = pdu
        .chunks(2)
        .map(|w| u32::from(u16::from_be_bytes([w[0], w[1]])))
        .sum();
    while sum > 0xFFFF {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    pdu[CHECKSUM].copy_from_slice(&(!(sum as u16)).to_be_bytes());
}

/// A scripted Client's Setup Request, signed with `key`.
fn scripted_request(clock: ScriptClock, key: impl Fn(u32) -> Vec<u8>) -> Vec<u8> {
    let time = clock.unix_secs as u32;
    let mut pdu = setup_request(time, KEY_ID);
    sign(&mut pdu, &key(time));
    pdu.to_vec()
}

fn client_key(time: u32) -> Vec<u8> {
    derive_keys(SHARED_KEY, time).client().to_vec()
}

fn server_key(time: u32) -> Vec<u8> {
    derive_keys(SHARED_KEY, time).server().to_vec()
}

/// A scripted Server's acceptance: the request with cmdRequest
/// CHSR_CREQ_SETUPRSP, cmdResponse CHSR_CRSP_ACKOK, a testPort, the current
/// authUnixTime (plus `time_skew`), signed with `key` (§6.2.2).
fn scripted_acceptance(clock: ScriptClock, request: &[u8], time_skew: i64, key: &[u8]) -> Vec<u8> {
    let mut pdu = request.to_vec();
    pdu[CMD_REQUEST] = 2;
    pdu[CMD_RESPONSE] = 1;
    pdu[TEST_PORT].copy_from_slice(&40001u16.to_be_bytes());
    pdu[AUTH_TIME].copy_from_slice(&((clock.unix_secs as i64 + time_skew) as u32).to_be_bytes());
    sign(&mut pdu, key);
    pdu
}

// ---- Our Client against our Server ----

#[udpstp_req::req(
    "RFC9946-5.3-1",
    "RFC9946-5.3.1-7",
    "RFC9946-5.3.1-8",
    "RFC9946-12.3.4-L1"
)]
#[test]
fn mode1_setup_between_our_cores_is_accepted() {
    let outcome = Scenario::new(10).mode1(KEY_ID, SHARED_KEY).run_for(RUN);

    outcome.check(
        outcome.client_accepted_test_port().is_some(),
        "the Client's mode 1 Setup was accepted",
    );
}

#[udpstp_req::req("RFC9946-5.3.1-6", "RFC9946-6.2.1-4", "RFC9946-5.4.1-7")]
#[test]
fn server_silently_drops_a_setup_signed_from_a_different_shared_key() {
    let outcome = Scenario::new(11)
        .server_key(KEY_ID, SHARED_KEY)
        .client_mode1(KEY_ID, b"another key")
        .run_for(RUN);

    outcome.check(
        outcome.sent_by(harness::SERVER_ADDR).is_empty(),
        "the Server sent nothing",
    );
    outcome.check(
        outcome.server_dropped_setup_because(SilentRejectionReason::AuthenticationFailed),
        "the Server reported an authentication failure",
    );
}

#[udpstp_req::req("RFC9946-5.4-4", "RFC9946-5.3.1-6")]
#[test]
fn server_silently_drops_a_setup_whose_key_id_it_does_not_have() {
    let outcome = Scenario::new(12)
        .server_key(1, SHARED_KEY)
        .client_mode1(2, SHARED_KEY)
        .run_for(RUN);

    outcome.check(
        outcome.sent_by(harness::SERVER_ADDR).is_empty(),
        "the Server sent nothing",
    );
    outcome.check(
        outcome.server_dropped_setup_because(SilentRejectionReason::UnknownKeyId),
        "the Server reported an unknown keyId",
    );
}

// Interpretation (ADR 0009): the edges of the ±5 s window are inclusive, in
// whole seconds.
#[udpstp_req::req("RFC9946-5.3.1-4")]
#[test]
fn clocks_five_seconds_apart_are_within_the_window() {
    for offset in [5, -5] {
        let outcome = Scenario::new(13)
            .mode1(KEY_ID, SHARED_KEY)
            .client_clock_offset(offset)
            .run_for(RUN);
        outcome.check(
            outcome.client_accepted_test_port().is_some(),
            &format!("accepted with the Client {offset} s off"),
        );
    }
}

#[udpstp_req::req("RFC9946-5.3.1-4", "RFC9946-6.2-1", "RFC9946-5.3.1-6")]
#[test]
fn server_silently_drops_a_setup_more_than_five_seconds_off() {
    for offset in [6, -6] {
        let outcome = Scenario::new(14)
            .mode1(KEY_ID, SHARED_KEY)
            .client_clock_offset(offset)
            .run_for(RUN);
        outcome.check(
            outcome.sent_by(harness::SERVER_ADDR).is_empty(),
            &format!("the Server sent nothing with the Client {offset} s off"),
        );
        outcome.check(
            outcome.server_dropped_setup_because(SilentRejectionReason::AuthTimeOutOfWindow),
            "the Server reported authUnixTime out of the window",
        );
    }
}

// authUnixTime is a 32-bit count of seconds since the epoch: it wraps on 2106-02-07 06:28:16 UTC. The Client's clock reads
// 2^32 - 2 (field 0xFFFFFFFE) and the Server's 2^32 + 1 (field 1).
#[udpstp_req::req("RFC9946-6.1-L12", "RFC9946-5.3.1-4")]
#[test]
fn setup_is_accepted_across_the_2106_wraparound() {
    let outcome = Scenario::new(15)
        .mode1(KEY_ID, SHARED_KEY)
        .wall_start((1u64 << 32) + 1)
        .client_clock_offset(-3)
        .run_for(RUN);

    let request = &outcome.sent_by(harness::CLIENT_ADDR)[0].payload;
    outcome.check(
        auth_unix_time(request) == 0xFFFF_FFFE,
        "the Client sent authUnixTime 0xFFFFFFFE",
    );
    outcome.check(
        outcome.client_accepted_test_port().is_some(),
        "the Setup was accepted across the wraparound",
    );
}

// ---- A scripted Client against our Server ----

// The Client key authenticates messages sent by the client, the Server key
// those sent by the server, both derived from the first Setup Request's
// authUnixTime.
#[udpstp_req::req(
    "RFC9946-5.4.1-3",
    "RFC9946-5.4.1-6",
    "RFC9946-5.4.1-8",
    "RFC9946-5.3.1-3",
    "RFC9946-6.2.2-3"
)]
#[test]
fn server_accepts_a_request_signed_with_the_client_key_and_answers_with_the_server_key() {
    let outcome = Scenario::new(20)
        .server_key(KEY_ID, SHARED_KEY)
        .scripted_client(|clock| scripted_request(clock, client_key))
        // The Client's clock is 3 s behind, so an echoed authUnixTime shows.
        .client_clock_offset(-3)
        .run_for(RUN);

    let request = &outcome.sent_by(harness::CLIENT_ADDR)[0].payload;
    let time = auth_unix_time(request);
    let replies = outcome.sent_by(harness::SERVER_ADDR);
    outcome.check(replies.len() == 1, "the Server sent one Setup Response");
    let response = &replies[0].payload;
    outcome.check(
        response[CMD_RESPONSE] == 1,
        "the response is CHSR_CRSP_ACKOK",
    );
    outcome.check(
        digest_verifies(response, &server_key(time)),
        "the response is signed with the Server Authentication Key",
    );
    outcome.check(
        !digest_verifies(response, &client_key(time)) && !digest_verifies(response, SHARED_KEY),
        "and not with the Client key or the Shared Key",
    );
    outcome.check(
        auth_unix_time(response) == time + 3,
        "the response's authUnixTime is the Server's current time, not the request's",
    );
}

#[udpstp_req::req("RFC9946-5.4-2", "RFC9946-5.3.1-2")]
#[test]
fn server_drops_a_request_signed_with_the_shared_key_itself() {
    let outcome = Scenario::new(21)
        .server_key(KEY_ID, SHARED_KEY)
        .scripted_client(|clock| scripted_request(clock, |_| SHARED_KEY.to_vec()))
        .run_for(RUN);

    outcome.check(
        outcome.sent_by(harness::SERVER_ADDR).is_empty(),
        "the Server sent nothing",
    );
    outcome.check(
        outcome.server_dropped_setup_because(SilentRejectionReason::AuthenticationFailed),
        "the Server reported an authentication failure",
    );
}

#[udpstp_req::req("RFC9946-5.4.1-6")]
#[test]
fn server_drops_a_request_signed_with_the_server_key() {
    let outcome = Scenario::new(22)
        .server_key(KEY_ID, SHARED_KEY)
        .scripted_client(|clock| scripted_request(clock, server_key))
        .run_for(RUN);

    outcome.check(
        outcome.sent_by(harness::SERVER_ADDR).is_empty(),
        "the Server sent nothing",
    );
}

#[udpstp_req::req("RFC9946-5.4.1-3")]
#[test]
fn server_drops_a_request_whose_keys_come_from_another_auth_unix_time() {
    let outcome = Scenario::new(23)
        .server_key(KEY_ID, SHARED_KEY)
        .scripted_client(|clock| scripted_request(clock, |time| client_key(time - 1)))
        .run_for(RUN);

    outcome.check(
        outcome.sent_by(harness::SERVER_ADDR).is_empty(),
        "the Server sent nothing",
    );
}

// The digest covers the entire PDU as sent.
#[udpstp_req::req("RFC9946-6.1-4", "RFC9946-5.3.1-L3")]
#[test]
fn server_verifies_the_digest_over_the_bytes_received() {
    let outcome = Scenario::new(25)
        .server_key(KEY_ID, SHARED_KEY)
        .scripted_client(|clock| {
            let time = clock.unix_secs as u32;
            let mut pdu = setup_request(time, KEY_ID);
            // reservedAuth1: its value is ignored upon receipt, but the
            // sender signed it.
            pdu[KEY_ID_AT + 1] = 0x5A;
            sign(&mut pdu, &client_key(time));
            pdu.to_vec()
        })
        .run_for(RUN);

    outcome.check(
        outcome.server_admitted_count() == 1,
        "the Server admitted the Test Connection",
    );
}

// The digest is computed with checkSum preset to zero and the checksum last:
// a populated checksum doesn't break the digest.
#[udpstp_req::req("RFC9946-5.3.1-L3", "RFC9946-6.2-1")]
#[test]
fn server_accepts_a_request_whose_checksum_was_filled_after_the_digest() {
    let outcome = Scenario::new(24)
        .server_key(KEY_ID, SHARED_KEY)
        .scripted_client(|clock| {
            let mut pdu = scripted_request(clock, client_key);
            fill_checksum(&mut pdu);
            pdu
        })
        .run_for(RUN);

    let request = &outcome.sent_by(harness::CLIENT_ADDR)[0].payload;
    outcome.check(
        request[CHECKSUM] != [0, 0],
        "the scripted checksum is populated",
    );
    outcome.check(
        outcome.sent_by(harness::SERVER_ADDR).len() == 1,
        "the Server answered",
    );
}

// ---- Our Client against a scripted Server ----

// Without checksum use, the digest verifies over every other byte as sent,
// so no field was set after it: it was the last step of building the PDU.
#[udpstp_req::req(
    "RFC9946-5.4.1-3",
    "RFC9946-5.4.1-6",
    "RFC9946-5.3.1-3",
    "RFC9946-5.3.1-L1",
    "RFC9946-6.1-L13"
)]
#[test]
fn client_signs_its_setup_request_with_the_client_key_from_its_own_auth_unix_time() {
    let outcome = Scenario::new(30)
        .client_mode1(KEY_ID, SHARED_KEY)
        .scripted_server(|_, _| None)
        .run_for(RUN);

    let request = &outcome.sent_by(harness::CLIENT_ADDR)[0].payload;
    let time = auth_unix_time(request);
    outcome.check(
        time == 1_790_013_600,
        "authUnixTime is the Client's wall clock in seconds",
    );
    outcome.check(
        request[AUTH_MODE] == 1 && request[KEY_ID_AT] == KEY_ID,
        "authMode 1 and the keyId",
    );
    outcome.check(
        digest_verifies(request, &client_key(time)) && request[CHECKSUM] == [0, 0],
        "signed with the Client key, with no checksum after the digest",
    );
    outcome.check(
        !digest_verifies(request, SHARED_KEY) && !digest_verifies(request, &server_key(time)),
        "not with the Shared Key or the Server key",
    );
}

#[udpstp_req::req("RFC9946-6.3-1")]
#[test]
fn client_accepts_a_response_signed_with_the_server_key() {
    let outcome = Scenario::new(31)
        .client_mode1(KEY_ID, SHARED_KEY)
        .scripted_server(|clock, request| {
            Some(scripted_acceptance(
                clock,
                request,
                0,
                &server_key(auth_unix_time(request)),
            ))
        })
        .run_for(RUN);

    outcome.check(
        outcome.client_accepted_test_port() == Some(40001),
        "the Client accepted",
    );
}

#[udpstp_req::req("RFC9946-5.4.1-6", "RFC9946-6.3-1")]
#[test]
fn client_ignores_a_response_signed_with_the_client_key() {
    let outcome = Scenario::new(32)
        .client_mode1(KEY_ID, SHARED_KEY)
        .scripted_server(|clock, request| {
            Some(scripted_acceptance(
                clock,
                request,
                0,
                &client_key(auth_unix_time(request)),
            ))
        })
        .run_for(RUN);

    outcome.check(
        outcome.client_accepted_test_port().is_none(),
        "the Client did not accept",
    );
    outcome.check(
        outcome.client_dropped_setup_response_because(ResponseIgnoreReason::AuthenticationFailed),
        "the Client reported an authentication failure",
    );
}

#[udpstp_req::req("RFC9946-5.3.1-4", "RFC9946-6.3-1")]
#[test]
fn client_ignores_a_response_more_than_five_seconds_off() {
    let outcome = Scenario::new(33)
        .client_mode1(KEY_ID, SHARED_KEY)
        .scripted_server(|clock, request| {
            Some(scripted_acceptance(
                clock,
                request,
                6,
                &server_key(auth_unix_time(request)),
            ))
        })
        .run_for(RUN);

    outcome.check(
        outcome.client_accepted_test_port().is_none(),
        "the Client did not accept",
    );
    outcome.check(
        outcome.client_dropped_setup_response_because(ResponseIgnoreReason::AuthTimeOutOfWindow),
        "the Client reported authUnixTime out of the window",
    );
}

// A mode 1 Client doesn't accept an unauthenticated (authMode 0, unsigned)
// response.
#[udpstp_req::req("RFC9946-6.3-1", "RFC9946-6.2-1")]
#[test]
fn client_ignores_an_unauthenticated_response_to_a_mode1_request() {
    let outcome = Scenario::new(34)
        .client_mode1(KEY_ID, SHARED_KEY)
        .scripted_server(|clock, request| {
            let mut pdu =
                scripted_acceptance(clock, request, 0, &server_key(auth_unix_time(request)));
            pdu[AUTH_MODE] = 0;
            pdu[AUTH_TIME].fill(0);
            pdu[DIGEST].fill(0);
            pdu[KEY_ID_AT] = 0;
            Some(pdu)
        })
        .run_for(RUN);

    outcome.check(
        outcome.client_accepted_test_port().is_none(),
        "the Client did not accept",
    );
    outcome.check(
        outcome.client_dropped_setup_response_because(ResponseIgnoreReason::AuthenticationFailed),
        "the Client reported an authentication failure",
    );
}
