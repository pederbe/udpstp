//! The Activation exchange in Authentication Mode 1 (RFC 9946 §5.3.1, §7.1,
//! §7.2, §7.2.1, §7.3).
//!
//! Scripted peers build their PDUs byte by byte from RFC 9946 Figures 3 and
//! 6, sign them with HMAC-SHA-256 and the KDF (both pinned by independent
//! known-answer vectors), and check what our Client or Server sends
//! back. They don't use our PDU codecs or signing code.

use super::harness;

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use crate::protocol::Direction;
use crate::wire::auth::{derive_keys, hmac_sha256};
use harness::{
    ControlPhaseFailure, ResponseIgnoreReason, Scenario, ScriptClock, SilentRejectionReason,
};

const KEY_ID: u8 = 7;
const SHARED_KEY: &[u8] = b"a shared key\0with a zero byte";
const RUN: Duration = Duration::from_secs(1);
/// The simulated Server's Standard Port (harness::SERVER_ADDR).
const SERVER_PORT: u16 = 24601;

fn client_key(time: u32) -> Vec<u8> {
    derive_keys(SHARED_KEY, time).client().to_vec()
}

fn server_key(time: u32) -> Vec<u8> {
    derive_keys(SHARED_KEY, time).server().to_vec()
}

// ---- Test Setup PDU from RFC 9946 Figure 3 ----

mod setup {
    use std::ops::Range;

    pub const LEN: usize = 56;
    pub const CMD_REQUEST: usize = 8;
    pub const CMD_RESPONSE: usize = 9;
    pub const TEST_PORT: Range<usize> = 12..14;
    pub const AUTH_MODE: usize = 15;
    pub const AUTH_TIME: Range<usize> = 16..20;
    pub const DIGEST: Range<usize> = 20..52;
    pub const KEY_ID: usize = 52;
    pub const CHECKSUM: Range<usize> = 54..56;
}

/// A mode 1 Setup Request: pduId 0xACE1, protocolVer 20, mcCount 1, mcIdent
/// 0x1234, cmdRequest CHSR_CREQ_SETUPREQ, modifierBitmap CHSR_JUMBO_STATUS,
/// authMode AUTHMODE_1, signed with the Client key derived from its own
/// authUnixTime (ADR 0009).
fn setup_request(time: u32) -> Vec<u8> {
    let mut b = vec![0u8; setup::LEN];
    b[0..2].copy_from_slice(&0xACE1u16.to_be_bytes());
    b[2..4].copy_from_slice(&20u16.to_be_bytes());
    b[5] = 1;
    b[6..8].copy_from_slice(&0x1234u16.to_be_bytes());
    b[setup::CMD_REQUEST] = 1;
    b[14] = 0x01;
    b[setup::AUTH_MODE] = 1;
    b[setup::AUTH_TIME].copy_from_slice(&time.to_be_bytes());
    b[setup::KEY_ID] = KEY_ID;
    sign(&mut b, setup::DIGEST, setup::CHECKSUM, &client_key(time));
    b
}

fn is_setup_acceptance(pdu: &[u8]) -> Option<u16> {
    (pdu.len() == setup::LEN
        && pdu[0..2] == [0xAC, 0xE1]
        && pdu[setup::CMD_REQUEST] == 2
        && pdu[setup::CMD_RESPONSE] == 1)
        .then(|| u16::from_be_bytes([pdu[12], pdu[13]]))
}

// ---- Test Activation PDU from RFC 9946 Figure 6 ----

mod act {
    use std::ops::Range;

    pub const LEN: usize = 104;
    pub const CMD_REQUEST: usize = 4;
    pub const CMD_RESPONSE: usize = 5;
    pub const RATE_ADJ_ALGO: usize = 26;
    pub const AUTH_MODE: usize = 63;
    pub const AUTH_TIME: Range<usize> = 64..68;
    pub const DIGEST: Range<usize> = 68..100;
    pub const KEY_ID: usize = 100;
    pub const CHECKSUM: Range<usize> = 102..104;
}

/// An unsigned Upstream Activation Request (CHTA_CREQ_TESTACTUS) with RFC
/// 9946 / TR-471 default parameters, authMode AUTHMODE_1, the given
/// authUnixTime and keyId.
fn activation_request(time: u32, key_id: u8) -> Vec<u8> {
    let mut b = vec![0u8; act::LEN];
    let mut put16 = |at: usize, v: u16| b[at..at + 2].copy_from_slice(&v.to_be_bytes());
    put16(0, 0xACE2);
    put16(2, 20);
    put16(6, 30); // lowThresh
    put16(8, 90); // upperThresh
    put16(10, 50); // trialInt
    put16(12, 10); // testIntTime
    put16(16, 0xFFFF); // srIndexConf
    put16(20, 3); // slowAdjThresh
    put16(22, 10); // seqErrThresh
    put16(56, 1000); // subIntPeriod
    b[act::CMD_REQUEST] = 1;
    b[18] = 1; // useOwDelVar
    b[19] = 10; // highSpeedDelta
    b[24] = 1; // ignoreOooDup
    b[act::AUTH_MODE] = 1;
    b[act::AUTH_TIME].copy_from_slice(&time.to_be_bytes());
    b[act::KEY_ID] = key_id;
    b
}

fn is_activation(pdu: &[u8]) -> bool {
    pdu.len() == act::LEN && pdu[0..2] == [0xAC, 0xE2]
}

fn time_at(pdu: &[u8], at: std::ops::Range<usize>) -> u32 {
    pdu.get(at)
        .and_then(|b| b.try_into().ok())
        .map_or(0, u32::from_be_bytes)
}

/// Bytes RFC 9946 Figure 6 marks reserved: reserved4 (offsets 60-61). A
/// receiver ignores their values, but they are part of the signed PDU.
const RESERVED4: std::ops::Range<usize> = 60..62;

// ---- authDigest (RFC 9946 §5.3.1) ----

/// The PDU with authDigest and checkSum zeroed, the HMAC input.
fn zeroed(pdu: &[u8], digest: std::ops::Range<usize>, checksum: std::ops::Range<usize>) -> Vec<u8> {
    let mut b = pdu.to_vec();
    b[digest].fill(0);
    b[checksum].fill(0);
    b
}

fn sign(
    pdu: &mut [u8],
    digest: std::ops::Range<usize>,
    checksum: std::ops::Range<usize>,
    key: &[u8],
) {
    let mac = hmac_sha256(key, &zeroed(pdu, digest.clone(), checksum));
    pdu[digest].copy_from_slice(&mac);
}

fn digest_verifies(pdu: &[u8], key: &[u8]) -> bool {
    hmac_sha256(key, &zeroed(pdu, act::DIGEST, act::CHECKSUM)) == pdu[act::DIGEST]
}

// ---- Scripted Client ----

/// A scripted mode 1 Client: it sends a Setup Request at its clock's time
/// T, and answers the Setup Response by sending `activation(T, now)` to the
/// testPort.
fn scripted_client(
    seed: u64,
    activation: impl Fn(u32, u32) -> Vec<u8> + 'static,
) -> harness::Outcome {
    let setup_time = Rc::new(Cell::new(0u32));
    let t = setup_time.clone();
    let mut sent = false;
    Scenario::new(seed)
        .server_key(KEY_ID, SHARED_KEY)
        .scripted_client_replying(
            move |clock| {
                t.set(clock.unix_secs as u32);
                setup_request(clock.unix_secs as u32)
            },
            move |clock, pdu| {
                let test_port = is_setup_acceptance(pdu)?;
                if std::mem::replace(&mut sent, true) {
                    return None;
                }
                Some((
                    test_port,
                    activation(setup_time.get(), clock.unix_secs as u32),
                ))
            },
        )
        .run_for(RUN)
}

/// A correctly signed Activation Request: authUnixTime `now + skew`,
/// signed with the Client key of the Setup's authUnixTime.
fn signed_activation(setup_time: u32, now: u32, skew: i64) -> Vec<u8> {
    let mut pdu = activation_request((i64::from(now) + skew) as u32, KEY_ID);
    sign(
        &mut pdu,
        act::DIGEST,
        act::CHECKSUM,
        &client_key(setup_time),
    );
    pdu
}

/// Activation PDUs the Server sent, from any of its ports.
fn activation_responses(outcome: &harness::Outcome) -> Vec<Vec<u8>> {
    let server_ip = harness::SERVER_ADDR
        .parse::<std::net::SocketAddr>()
        .map(|a| a.ip())
        .ok();
    outcome
        .trace
        .iter()
        .filter(|t| Some(t.source.ip()) == server_ip && is_activation(&t.payload))
        .map(|t| t.payload.clone())
        .collect()
}

/// The first Setup Request's authUnixTime, as the Client sent it.
fn sent_setup_time(outcome: &harness::Outcome) -> u32 {
    let sent = outcome.sent_by(harness::CLIENT_ADDR);
    let setup = sent.first().map(|t| t.payload.as_slice());
    outcome.check(setup.is_some(), "the Client sent a Setup Request");
    time_at(setup.unwrap_or_default(), setup::AUTH_TIME)
}

// ---- Our Client against our Server ----

#[udpstp_req::req("RFC9946-5.3-1", "RFC9946-7.3-5")]
#[test]
fn mode1_tests_are_activated_in_both_directions() {
    for (seed, direction) in [(1, Direction::Upstream), (2, Direction::Downstream)] {
        let outcome = Scenario::new(seed)
            .mode1(KEY_ID, SHARED_KEY)
            .client_direction(direction)
            .run_for(RUN);

        outcome.check(
            outcome.client_activated_direction(0) == Some(direction),
            &format!("the Client prepared a {direction:?} Test in mode 1"),
        );
    }
}

// ---- A scripted Client against our Server ----

#[udpstp_req::req(
    "RFC9946-5.3.1-3",
    "RFC9946-5.4.1-6",
    "RFC9946-5.4.1-8",
    "RFC9946-6.1-4",
    "RFC9946-7.2-2",
    "RFC9946-7.2.2-1",
    "RFC9946-5.3.1-8",
    "RFC9946-7.1-L1"
)]
#[test]
fn server_answers_a_signed_activation_with_a_response_signed_with_the_server_key() {
    let outcome = scripted_client(10, |setup_time, now| signed_activation(setup_time, now, 0));

    let responses = activation_responses(&outcome);
    outcome.check(responses.len() == 1, "one Activation Response");
    let response = &responses[0];
    let setup_time = sent_setup_time(&outcome);
    outcome.check(
        response[RESERVED4] == [0, 0],
        "reserved fields are zero in the response",
    );
    outcome.check(response[act::CMD_RESPONSE] == 1, "CHTA_CRSP_ACKOK");
    outcome.check(
        response[act::AUTH_MODE] == 1 && response[act::KEY_ID] == KEY_ID,
        "authMode 1 and the Setup's keyId",
    );
    outcome.check(
        digest_verifies(response, &server_key(setup_time)),
        "signed with the Server key of the Setup's authUnixTime",
    );
    outcome.check(
        !digest_verifies(response, &client_key(setup_time))
            && !digest_verifies(response, SHARED_KEY),
        "not with the Client key or the Shared Key",
    );
    outcome.check(
        time_at(response, act::AUTH_TIME) == setup_time,
        "authUnixTime is the Server's wall clock (no time passes in the simulation)",
    );
}

#[udpstp_req::req(
    "RFC9946-5.3.1-4",
    "RFC9946-5.3.1-5",
    "RFC9946-5.3.1-6",
    "RFC9946-7.2-2",
    "RFC9946-7.2.1-6",
    "RFC9946-7.2.1-7",
    "RFC9946-7.2.1-L3"
)]
#[test]
fn server_silently_drops_activations_that_fail_verification() {
    type Build = fn(u32, u32) -> Vec<u8>;
    let cases: [(&str, Build, SilentRejectionReason); 8] = [
        (
            "signed with the Server key",
            |t, now| {
                let mut pdu = activation_request(now, KEY_ID);
                sign(&mut pdu, act::DIGEST, act::CHECKSUM, &server_key(t));
                pdu
            },
            SilentRejectionReason::AuthenticationFailed,
        ),
        (
            "signed with the Shared Key",
            |_, now| {
                let mut pdu = activation_request(now, KEY_ID);
                sign(&mut pdu, act::DIGEST, act::CHECKSUM, SHARED_KEY);
                pdu
            },
            SilentRejectionReason::AuthenticationFailed,
        ),
        (
            "unauthenticated after a mode 1 Setup",
            |_, _| {
                let mut pdu = activation_request(0, 0);
                pdu[act::AUTH_MODE] = 0;
                pdu
            },
            SilentRejectionReason::AuthModeMismatch,
        ),
        (
            "a keyId other than the Setup's",
            |t, now| {
                let mut pdu = activation_request(now, KEY_ID + 1);
                sign(&mut pdu, act::DIGEST, act::CHECKSUM, &client_key(t));
                pdu
            },
            SilentRejectionReason::KeyIdMismatch,
        ),
        (
            "authUnixTime 6 s ahead",
            |t, now| signed_activation(t, now, 6),
            SilentRejectionReason::AuthTimeOutOfWindow,
        ),
        (
            "authUnixTime 6 s behind",
            |t, now| signed_activation(t, now, -6),
            SilentRejectionReason::AuthTimeOutOfWindow,
        ),
        (
            "the Setup PDU's pduId",
            |t, now| {
                let mut pdu = activation_request(now, KEY_ID);
                pdu[0..2].copy_from_slice(&0xACE1u16.to_be_bytes());
                sign(&mut pdu, act::DIGEST, act::CHECKSUM, &client_key(t));
                pdu
            },
            SilentRejectionReason::Malformed,
        ),
        (
            "one byte short",
            |t, now| {
                let mut pdu = signed_activation(t, now, 0);
                pdu.pop();
                pdu
            },
            SilentRejectionReason::Malformed,
        ),
    ];
    for (seed, (what, build, reason)) in (200..).zip(cases) {
        let outcome = scripted_client(seed, build);

        outcome.check(
            activation_responses(&outcome).is_empty(),
            &format!("{what}: no Activation Response"),
        );
        outcome.check(
            outcome.server_dropped_activation_because(reason),
            &format!("{what}: dropped as {reason:?}"),
        );
    }
}

// The ±5 s window includes its edges.
#[udpstp_req::req("RFC9946-5.3.1-4", "RFC9946-7.2-2")]
#[test]
fn server_accepts_an_activation_time_5_s_off() {
    for (seed, skew) in [(30, 5), (31, -5)] {
        let outcome = scripted_client(seed, move |t, now| signed_activation(t, now, skew));

        let responses = activation_responses(&outcome);
        outcome.check(
            responses.len() == 1 && responses[0][act::CMD_RESPONSE] == 1,
            &format!("accepted at {skew:+} s"),
        );
    }
}

// The digest covers the entire PDU, reserved fields included.
#[udpstp_req::req("RFC9946-5.3.1-5", "RFC9946-6.1-4")]
#[test]
fn server_verifies_the_digest_over_the_bytes_received() {
    // OB-UDPST carries a value in reserved4 when its -i -N option is used,
    // and signs the PDU as sent.
    let outcome = scripted_client(34, |t, now| {
        let mut pdu = activation_request(now, KEY_ID);
        pdu[RESERVED4].copy_from_slice(&3u16.to_be_bytes());
        sign(&mut pdu, act::DIGEST, act::CHECKSUM, &client_key(t));
        pdu
    });

    let responses = activation_responses(&outcome);
    outcome.check(
        responses.len() == 1 && responses[0][act::CMD_RESPONSE] == 1,
        "accepted",
    );
}

#[udpstp_req::req("RFC9946-7.2.1-5", "RFC9946-7.2.1-L3")]
#[test]
fn server_signs_badparam_rejections() {
    type Build = fn(&mut [u8]);
    let cases: [(&str, Build); 2] = [
        ("an unknown rateAdjAlgo", |pdu| pdu[act::RATE_ADJ_ALGO] = 2),
        ("an unknown cmdRequest", |pdu| pdu[act::CMD_REQUEST] = 3),
    ];
    for (seed, (what, change)) in (32..).zip(cases) {
        let outcome = scripted_client(seed, move |t, now| {
            let mut pdu = activation_request(now, KEY_ID);
            change(&mut pdu);
            sign(&mut pdu, act::DIGEST, act::CHECKSUM, &client_key(t));
            pdu
        });

        let responses = activation_responses(&outcome);
        outcome.check(responses.len() == 1, &format!("{what}: one response"));
        outcome.check(
            responses[0][act::CMD_RESPONSE] == 2,
            &format!("{what}: CHTA_CRSP_BADPARAM"),
        );
        outcome.check(
            digest_verifies(&responses[0], &server_key(sent_setup_time(&outcome))),
            &format!("{what}: signed with the Server key"),
        );
    }
}

// Message Verification Procedure: authMode appropriate.
#[udpstp_req::req("RFC9946-7.2-2")]
#[test]
fn an_unauthenticated_test_connection_drops_a_mode1_activation() {
    let outcome = Scenario::new(39)
        .server_allows_unauthenticated()
        .scripted_client_replying(
            |_| {
                let mut b = setup_request(0);
                b[setup::AUTH_MODE] = 0;
                b[setup::AUTH_TIME].fill(0);
                b[setup::DIGEST].fill(0);
                b[setup::KEY_ID] = 0;
                b
            },
            |clock, pdu| {
                let port = is_setup_acceptance(pdu)?;
                let mut act = activation_request(clock.unix_secs as u32, KEY_ID);
                sign(&mut act, act::DIGEST, act::CHECKSUM, &client_key(0));
                Some((port, act))
            },
        )
        .run_for(RUN);

    outcome.check(
        activation_responses(&outcome).is_empty(),
        "no Activation Response",
    );
    outcome.check(
        outcome.server_dropped_activation_because(SilentRejectionReason::AuthModeMismatch),
        "dropped as AuthModeMismatch",
    );
}

// ---- Our Client against a scripted Server ----

/// A scripted mode 1 Server: it verifies nothing, accepts the Setup with
/// testPort = its Standard Port (so the Activation Request reaches the same
/// script), and answers the Activation Request with `respond(request,
/// setup_time, now)`.
fn scripted_server(
    seed: u64,
    respond: impl Fn(&[u8], u32, u32) -> Option<Vec<u8>> + 'static,
) -> harness::Outcome {
    scripted_server_replying_many(seed, move |request, t, now| {
        respond(request, t, now).into_iter().collect()
    })
}

/// Like [`scripted_server`], answering the Activation Request with several
/// datagrams.
fn scripted_server_replying_many(
    seed: u64,
    respond: impl Fn(&[u8], u32, u32) -> Vec<Vec<u8>> + 'static,
) -> harness::Outcome {
    let mut setup_time = 0u32;
    Scenario::new(seed)
        .client_mode1(KEY_ID, SHARED_KEY)
        .client_direction(Direction::Upstream)
        .scripted_server_replying_many(move |clock: ScriptClock, request| {
            let now = clock.unix_secs as u32;
            if request.len() == setup::LEN {
                setup_time = time_at(request, setup::AUTH_TIME);
                let mut pdu = request.to_vec();
                pdu[setup::CMD_REQUEST] = 2;
                pdu[setup::CMD_RESPONSE] = 1;
                pdu[setup::TEST_PORT].copy_from_slice(&SERVER_PORT.to_be_bytes());
                pdu[setup::AUTH_TIME].copy_from_slice(&now.to_be_bytes());
                sign(
                    &mut pdu,
                    setup::DIGEST,
                    setup::CHECKSUM,
                    &server_key(setup_time),
                );
                return vec![pdu];
            }
            if !is_activation(request) {
                return Vec::new();
            }
            respond(request, setup_time, now)
        })
        .run_for(RUN)
}

/// The request answered with `cmd_response` at `now + skew`, signed with `key`.
fn answer(request: &[u8], cmd_response: u8, now: u32, skew: i64, key: &[u8]) -> Vec<u8> {
    let mut pdu = request.to_vec();
    pdu[act::CMD_RESPONSE] = cmd_response;
    pdu[act::AUTH_TIME].copy_from_slice(&((i64::from(now) + skew) as u32).to_be_bytes());
    sign(&mut pdu, act::DIGEST, act::CHECKSUM, key);
    pdu
}

#[udpstp_req::req(
    "RFC9946-5.3.1-3",
    "RFC9946-5.4.1-6",
    "RFC9946-5.4.1-8",
    "RFC9946-5.3.1-L1"
)]
#[test]
fn client_signs_its_activation_request_with_the_client_key() {
    let outcome = scripted_server(40, |_, _, _| None);

    let setup_time = sent_setup_time(&outcome);
    let request = outcome
        .sent_by(harness::CLIENT_ADDR)
        .into_iter()
        .map(|t| t.payload.clone())
        .find(|p| is_activation(p));
    outcome.check(request.is_some(), "the Client sent an Activation Request");
    let request = request.unwrap_or_default();
    outcome.check(
        request[act::AUTH_MODE] == 1 && request[act::KEY_ID] == KEY_ID,
        "authMode 1 and the Setup's keyId",
    );
    outcome.check(
        u64::from(time_at(&request, act::AUTH_TIME)) == harness::WALL_START_UNIX_SECS,
        "authUnixTime is the Client's wall clock",
    );
    outcome.check(
        digest_verifies(&request, &client_key(setup_time)),
        "signed with the Client key of the Setup's authUnixTime",
    );
    outcome.check(
        !digest_verifies(&request, &server_key(setup_time))
            && !digest_verifies(&request, SHARED_KEY),
        "not with the Server key or the Shared Key",
    );
}

#[udpstp_req::req("RFC9946-7.3-1")]
#[test]
fn client_accepts_a_response_signed_with_the_server_key() {
    // With a value in a reserved field, signed as sent: the digest covers
    // the bytes received (RFC 9946 §5.3.1).
    let outcome = scripted_server(41, |request, t, now| {
        let mut request = request.to_vec();
        request[RESERVED4].copy_from_slice(&3u16.to_be_bytes());
        Some(answer(&request, 1, now, 0, &server_key(t)))
    });

    outcome.check(
        outcome.client_activated_direction(0) == Some(Direction::Upstream),
        "the Client prepared the Test",
    );
}

// ADR 0009 (a response failing verification is ignored)
#[udpstp_req::req("RFC9946-5.3.1-4", "RFC9946-5.3.1-5", "RFC9946-7.3-1")]
#[test]
fn client_ignores_responses_that_fail_verification() {
    type Answer = fn(&[u8], u32, u32) -> Vec<u8>;
    let cases: [(&str, Answer, ResponseIgnoreReason); 5] = [
        (
            "signed with the Client key",
            |r, t, now| answer(r, 1, now, 0, &client_key(t)),
            ResponseIgnoreReason::AuthenticationFailed,
        ),
        (
            "signed with the Shared Key",
            |r, _, now| answer(r, 1, now, 0, SHARED_KEY),
            ResponseIgnoreReason::AuthenticationFailed,
        ),
        (
            "authMode 0, though signed with the Server key",
            |r, t, now| {
                let mut request = r.to_vec();
                request[act::AUTH_MODE] = 0;
                answer(&request, 1, now, 0, &server_key(t))
            },
            ResponseIgnoreReason::AuthenticationFailed,
        ),
        (
            "authUnixTime 6 s behind",
            |r, t, now| answer(r, 1, now, -6, &server_key(t)),
            ResponseIgnoreReason::AuthTimeOutOfWindow,
        ),
        (
            "authUnixTime 6 s ahead",
            |r, t, now| answer(r, 1, now, 6, &server_key(t)),
            ResponseIgnoreReason::AuthTimeOutOfWindow,
        ),
    ];
    for (seed, (what, build, reason)) in (50..).zip(cases) {
        let outcome = scripted_server(seed, move |r, t, now| Some(build(r, t, now)));

        outcome.check(
            outcome.client_activated_direction(0).is_none(),
            &format!("{what}: the Client did not prepare the Test"),
        );
        outcome.check(
            outcome.client_dropped_activation_response_because(reason),
            &format!("{what}: ignored as {reason:?}"),
        );
    }
}

#[udpstp_req::req("RFC9946-7.3-2")]
#[test]
fn client_reports_a_signed_badparam() {
    let outcome = scripted_server(60, |request, t, now| {
        Some(answer(request, 2, now, 0, &server_key(t)))
    });

    outcome.check(
        outcome.client_activation_rejected(0),
        "the Client reported the rejection",
    );
}

#[udpstp_req::req("RFC9946-7.3-3", "RFC9946-12.3.9-L1")]
#[test]
fn client_terminates_on_an_unknown_activation_response_code() {
    // A valid acceptance right after it must no longer activate the Test.
    let outcome = scripted_server_replying_many(61, |request, t, now| {
        vec![
            answer(request, 7, now, 0, &server_key(t)),
            answer(request, 1, now, 0, &server_key(t)),
        ]
    });

    outcome.check(
        outcome.client_failed_because(ControlPhaseFailure::UnknownActivationResponse {
            cmd_response: 7,
        }),
        "the Client ended over the unknown code",
    );
    outcome.check(
        outcome.client_activated_direction(0).is_none(),
        "the Client did not prepare the Test",
    );
}
