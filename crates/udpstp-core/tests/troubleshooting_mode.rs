//! Troubleshooting mode (RFC 9946 §6.2.1 "server administrators are
//! permitted to send a Setup Response to support operations and
//! troubleshooting"; docs/spec.md stories 76 and 77): off by default, when
//! authentication failures are silent; on, each is answered with a Setup
//! Response carrying its code, unsigned unless the digest was valid
//! (AUTHTIME), at most 10 a second, and the Server warns every minute.
//! Activation Requests that fail verification stay silent even then.

mod harness;

use std::cell::Cell;
use std::net::SocketAddr;
use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

use harness::{Outcome, Scenario, SilentRejectionReason};
use udpstp_core::{AuthenticationFailure, Event, SetupRejection};
use udpstp_wire::auth::{derive_keys, hmac_sha256};

const AUTHNC: u8 = 4;
const AUTHREQ: u8 = 5;
const AUTHINV: u8 = 6;
const AUTHFAIL: u8 = 7;
const AUTHTIME: u8 = 8;
const RUN: Duration = Duration::from_millis(100);

fn troubleshooting(scenario: Scenario) -> Scenario {
    scenario.server_config(|c| c.troubleshooting = true)
}

/// The cmdResponse of each Setup Response the Server sent, and whether all
/// were unsigned (authDigest zero).
fn responses(outcome: &Outcome) -> (Vec<u8>, bool) {
    let sent: Vec<&Vec<u8>> = outcome
        .trace
        .iter()
        .filter(|t| t.payload.starts_with(&[0xAC, 0xE1]) && t.payload.get(8) == Some(&2))
        .map(|t| &t.payload)
        .collect();
    let codes = sent.iter().map(|p| p[9]).collect();
    let unsigned = sent.iter().all(|p| p[20..52].iter().all(|&b| b == 0));
    (codes, unsigned)
}

fn check(outcome: &Outcome, failure: AuthenticationFailure, code: u8) {
    let (codes, unsigned) = responses(outcome);
    let reported = outcome.server_events.iter().any(|e| {
        matches!(e, Event::SetupRequestRejected {
            reason: SetupRejection::Authentication(f),
            ..
        } if *f == failure)
    });
    outcome.check(
        codes == [code] && unsigned == (code != AUTHTIME) && reported,
        &format!("expected one {code} ({failure:?}): {codes:?}, unsigned {unsigned}"),
    );
}

#[udpstp_req::req("RFC9946-6.2.1-L1")]
#[test]
fn a_wrong_key_is_answered_with_authfail() {
    let outcome = troubleshooting(Scenario::new(1))
        .server_key(7, b"the Server's key")
        .client_mode1(7, b"another key")
        .run_for(RUN);
    check(&outcome, AuthenticationFailure::DigestFailed, AUTHFAIL);
}

// docs/spec.md story 76
#[udpstp_req::req("RFC9946-6.2.1-L1", "RFC9946-5.3.1-L4")]
#[test]
fn an_unknown_key_id_is_answered_with_authfail() {
    let outcome = troubleshooting(Scenario::new(2))
        .server_key(7, b"a key")
        .client_mode1(8, b"a key")
        .run_for(RUN);
    check(&outcome, AuthenticationFailure::UnknownKeyId, AUTHFAIL);
}

// docs/spec.md story 76; the digest is valid, so the response is signed.
#[udpstp_req::req("RFC9946-6.2.1-L1", "RFC9946-5.3.1-L4", "RFC9946-6.2.1-3")]
#[test]
fn a_time_outside_the_window_is_answered_with_a_signed_authtime() {
    let outcome = troubleshooting(Scenario::new(3))
        .mode1(7, b"a key")
        .client_clock_offset(10)
        .run_for(RUN);
    check(&outcome, AuthenticationFailure::TimeOutOfWindow, AUTHTIME);
}

// docs/spec.md story 76
#[udpstp_req::req("RFC9946-6.2.1-L1", "RFC9946-5.3.1-L4")]
#[test]
fn an_unauthenticated_request_is_answered_with_authreq() {
    let outcome = troubleshooting(Scenario::new(4))
        .server_key(7, b"a key")
        .client_unauthenticated()
        .run_for(RUN);
    check(&outcome, AuthenticationFailure::Required, AUTHREQ);
}

// docs/spec.md story 76
#[udpstp_req::req("RFC9946-6.2.1-L1", "RFC9946-5.3.1-L4", "RFC9946-12.3.5-L2")]
#[test]
fn an_authenticated_request_to_a_server_without_keys_is_answered_with_authnc() {
    let outcome = troubleshooting(Scenario::new(5))
        .client_mode1(7, b"a key")
        .run_for(RUN);
    check(&outcome, AuthenticationFailure::NotConfigured, AUTHNC);
    // Also when unauthenticated Tests are allowed.
    let outcome = troubleshooting(Scenario::new(5))
        .server_allows_unauthenticated()
        .client_mode1(7, b"a key")
        .run_for(RUN);
    check(&outcome, AuthenticationFailure::NotConfigured, AUTHNC);
}

// docs/spec.md story 76
#[udpstp_req::req("RFC9946-6.2.1-L1", "RFC9946-5.3.1-L4", "RFC9946-12.3.5-L3")]
#[test]
fn an_unsupported_mode_is_answered_with_authinv() {
    let outcome = troubleshooting(Scenario::new(6))
        .mode1(7, b"a key")
        .network_writes_setup(0, 15, &[3])
        .run_for(RUN);
    check(&outcome, AuthenticationFailure::ModeInvalid, AUTHINV);
}

// docs/spec.md story 77 (authentication failures are silent by default)
#[udpstp_req::req("RFC9946-6.2.1-4", "RFC9946-5.3.1-6")]
#[test]
fn by_default_authentication_failures_are_silent() {
    let outcome = Scenario::new(7)
        .server_key(7, b"the Server's key")
        .client_mode1(7, b"another key")
        .run_for(RUN);
    let (codes, _) = responses(&outcome);
    outcome.check(
        codes.is_empty()
            && !outcome
                .server_events
                .contains(&Event::TroubleshootingModeOn),
        &format!("{codes:?}"),
    );
}

// docs/spec.md (while on, the Server emits a warning event periodically)
#[test]
fn while_on_the_server_warns_every_minute() {
    for (run, expected) in [(59_999, 1), (60_000, 2), (120_000, 3)] {
        let outcome = troubleshooting(Scenario::new(8))
            .server_key(7, b"the Server's key")
            .client_mode1(7, b"another key")
            .run_for(Duration::from_millis(run));
        let warnings = outcome
            .server_events
            .iter()
            .filter(|e| **e == Event::TroubleshootingModeOn)
            .count();
        outcome.check(
            warnings == expected,
            &format!("{warnings} warnings in {run} ms"),
        );
    }
}

// docs/spec.md (troubleshooting replies are bounded, since a spoofed
// request gets its reply)
#[udpstp_req::req("RFC9946-7.2.1-L4")]
#[test]
fn at_most_10_failures_a_second_are_answered() {
    let mut scenario = troubleshooting(Scenario::new(9))
        .server_key(7, b"the Server's key")
        .client_mode1(7, b"another key")
        .server_tests_per_source(20);
    for _ in 0..11 {
        scenario = scenario.add_client_in_another_test(udpstp_core::Direction::Downstream);
    }
    let outcome = scenario.run_for(RUN);
    let (codes, _) = responses(&outcome);
    outcome.check(codes.len() == 10, &format!("{} replies", codes.len()));
}

// docs/spec.md story 84 (a replay stays silent, troubleshooting or not)
#[udpstp_req::req("RFC9946-7.2.1-L4")]
#[test]
fn a_replay_stays_silent() {
    let outcome = troubleshooting(Scenario::new(10))
        .mode1(7, b"a key")
        .network_duplicates_setup_requests()
        .run_for(RUN);
    let (codes, _) = responses(&outcome);
    outcome.check(codes == [1], &format!("{codes:?}"));
}

// ---- Activation Requests: a scripted mode 1 Client, whose PDUs are built
// from RFC 9946 Figures 3 and 7 and signed with HMAC-SHA-256 and the KDF
// (both pinned by known-answer vectors), not with our codecs ----

const KEY_ID: u8 = 7;
const SHARED_KEY: &[u8] = b"a key";
/// A second row of the Server's Key Table, which the Setup doesn't use.
const OTHER_KEY_ID: u8 = 8;
const OTHER_KEY: &[u8] = b"another key";
/// authDigest and checkSum in a Test Activation PDU (Figure 7).
const ACT_DIGEST: Range<usize> = 68..100;
const ACT_CHECKSUM: Range<usize> = 102..104;

/// The Client Authentication Key of `key` at the Setup's authUnixTime
/// (RFC 9946 §5.4.1).
fn client_key(key: &[u8], setup_time: u32) -> Vec<u8> {
    derive_keys(key, setup_time).client().to_vec()
}

/// Fills authDigest with the HMAC-SHA-256 of the whole PDU under `key`,
/// authDigest and checkSum zeroed (RFC 9946 §5.3.1).
fn sign(pdu: &mut [u8], digest: Range<usize>, checksum: Range<usize>, key: &[u8]) {
    let mut zeroed = pdu.to_vec();
    zeroed[digest.clone()].fill(0);
    zeroed[checksum].fill(0);
    pdu[digest].copy_from_slice(&hmac_sha256(key, &zeroed));
}

/// A mode 1 Setup Request (Figure 3): protocolVer 20, mcCount 1, mcIdent
/// 0x1234, CHSR_CREQ_SETUPREQ, CHSR_JUMBO_STATUS, AUTHMODE_1, keyId 7,
/// signed with the Client key of its own authUnixTime.
fn setup_request(time: u32) -> Vec<u8> {
    let mut b = vec![0u8; 56];
    b[0..2].copy_from_slice(&0xACE1u16.to_be_bytes());
    b[2..4].copy_from_slice(&20u16.to_be_bytes());
    b[5] = 1;
    b[6..8].copy_from_slice(&0x1234u16.to_be_bytes());
    b[8] = 1;
    b[14] = 0x01;
    b[15] = 1;
    b[16..20].copy_from_slice(&time.to_be_bytes());
    b[52] = KEY_ID;
    sign(&mut b, 20..52, 54..56, &client_key(SHARED_KEY, time));
    b
}

/// The testPort of a Setup Response that accepts (CHSR_CREQ_SETUPRSP,
/// CHSR_CRSP_ACKOK).
fn accepted_test_port(pdu: &[u8]) -> Option<u16> {
    (pdu.len() == 56 && pdu[0..2] == [0xAC, 0xE1] && pdu[8] == 2 && pdu[9] == 1)
        .then(|| u16::from_be_bytes([pdu[12], pdu[13]]))
}

/// An unsigned Upstream Activation Request (Figure 7, CHTA_CREQ_TESTACTUS)
/// with the RFC 9097 and TR-471 default parameters and the Server's
/// default search (CHTA_SRIDX_DEF), in authMode `mode` with keyId `key_id`
/// and authUnixTime `time`.
fn activation_request(mode: u8, key_id: u8, time: u32) -> Vec<u8> {
    let mut b = vec![0u8; 104];
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
    b[4] = 1;
    b[18] = 1; // useOwDelVar
    b[19] = 10; // highSpeedDelta
    b[24] = 1; // ignoreOooDup
    b[63] = mode;
    b[64..68].copy_from_slice(&time.to_be_bytes());
    b[100] = key_id;
    b
}

/// The Activation Request the Setup's Test Connection must send: mode 1,
/// keyId 7, signed with the Client key of the Setup's authUnixTime, with
/// authUnixTime `skew` seconds off the Client's clock.
fn authentic(setup_time: u32, now: u32, skew: i64) -> Vec<u8> {
    let mut pdu = activation_request(1, KEY_ID, (i64::from(now) + skew) as u32);
    sign(
        &mut pdu,
        ACT_DIGEST,
        ACT_CHECKSUM,
        &client_key(SHARED_KEY, setup_time),
    );
    pdu
}

/// A Server in Troubleshooting Mode with two Shared Keys, and a scripted
/// Client whose Setup Request, with the first, it accepts. The Client
/// answers the Setup Response once, with `activation(the Setup's
/// authUnixTime, its clock)`.
fn activate(seed: u64, activation: fn(u32, u32) -> Vec<u8>) -> Outcome {
    let setup_time = Rc::new(Cell::new(0u32));
    let set = setup_time.clone();
    let mut sent = false;
    troubleshooting(Scenario::new(seed))
        .server_key(KEY_ID, SHARED_KEY)
        .server_key(OTHER_KEY_ID, OTHER_KEY)
        .scripted_client_replying(
            move |clock| {
                set.set(clock.unix_secs as u32);
                setup_request(clock.unix_secs as u32)
            },
            move |clock, pdu| {
                let port = accepted_test_port(pdu)?;
                let first = !std::mem::replace(&mut sent, true);
                first.then(|| (port, activation(setup_time.get(), clock.unix_secs as u32)))
            },
        )
        .run_for(Duration::from_secs(1))
}

/// Every datagram the Server sent, from any of its ports.
fn sent_by_server(outcome: &Outcome) -> Vec<&[u8]> {
    let server = harness::SERVER_ADDR
        .parse::<SocketAddr>()
        .expect("an address")
        .ip();
    outcome
        .trace
        .iter()
        .filter(|t| t.source.ip() == server)
        .map(|t| t.payload.as_slice())
        .collect()
}

// The Interpretation that Troubleshooting Mode answers Setup Requests
// only: RFC 9946 §7.2.1 says an Activation Request whose authDigest is
// invalid SHOULD fail silently, and only permits an answer
// (conformance: troubleshooting-replies-rate-limited). §6.2's Message
// Verification Procedure, which §7.2 applies, decides the reason: authMode
// and keyId must be the Setup's (step 3, ADR 0009), then the digest (step
// 4), then authUnixTime within ±5 s (step 5, §5.3.1).
#[udpstp_req::req(
    "RFC9946-7.2-2",
    "RFC9946-7.2.1-6",
    "RFC9946-7.2.1-L4",
    "RFC9946-5.3.1-6"
)]
#[test]
fn a_failed_activation_request_stays_silent_in_troubleshooting_mode() {
    // The same exchange with an authentic Activation Request is answered.
    let outcome = activate(20, |t, now| authentic(t, now, 0));
    let answers: Vec<u8> = sent_by_server(&outcome)
        .iter()
        .filter(|p| p.starts_with(&[0xAC, 0xE2]))
        .map(|p| p[5])
        .collect();
    outcome.check(answers == [1], &format!("one ACKOK: {answers:?}"));

    type Build = fn(u32, u32) -> Vec<u8>;
    let cases: [(&str, Build, SilentRejectionReason); 7] = [
        (
            "signed with the Shared Key, not the Client key",
            |_, now| {
                let mut pdu = activation_request(1, KEY_ID, now);
                sign(&mut pdu, ACT_DIGEST, ACT_CHECKSUM, SHARED_KEY);
                pdu
            },
            SilentRejectionReason::AuthenticationFailed,
        ),
        (
            "a digest byte changed",
            |t, now| {
                let mut pdu = authentic(t, now, 0);
                pdu[ACT_DIGEST.start] ^= 0x01;
                pdu
            },
            SilentRejectionReason::AuthenticationFailed,
        ),
        (
            "the other key's keyId, signed with its Client key",
            |t, now| {
                let mut pdu = activation_request(1, OTHER_KEY_ID, now);
                sign(
                    &mut pdu,
                    ACT_DIGEST,
                    ACT_CHECKSUM,
                    &client_key(OTHER_KEY, t),
                );
                pdu
            },
            SilentRejectionReason::KeyIdMismatch,
        ),
        (
            "authMode 2 after a mode 1 Setup, otherwise authentic",
            |t, now| {
                let mut pdu = activation_request(2, KEY_ID, now);
                sign(
                    &mut pdu,
                    ACT_DIGEST,
                    ACT_CHECKSUM,
                    &client_key(SHARED_KEY, t),
                );
                pdu
            },
            SilentRejectionReason::AuthModeMismatch,
        ),
        (
            "unauthenticated after a mode 1 Setup",
            |_, _| activation_request(0, 0, 0),
            SilentRejectionReason::AuthModeMismatch,
        ),
        (
            "authUnixTime 6 s ahead",
            |t, now| authentic(t, now, 6),
            SilentRejectionReason::AuthTimeOutOfWindow,
        ),
        (
            "authUnixTime 6 s behind",
            |t, now| authentic(t, now, -6),
            SilentRejectionReason::AuthTimeOutOfWindow,
        ),
    ];
    for (seed, (what, build, reason)) in (21..).zip(cases) {
        let outcome = activate(seed, build);
        outcome.check(
            outcome
                .server_events
                .contains(&Event::TroubleshootingModeOn),
            &format!("{what}: Troubleshooting Mode is on"),
        );
        // The Setup Response accepting the Setup and the Null Request, and
        // nothing after them: no Activation Response, nor any other answer.
        let sent = sent_by_server(&outcome);
        let setup_exchange_only = sent.len() == 2
            && sent.iter().any(|p| accepted_test_port(p).is_some())
            && sent.iter().any(|p| p.starts_with(&[0xDE, 0xAD]));
        outcome.check(
            setup_exchange_only,
            &format!(
                "{what}: the Server sent only the Setup exchange ({} datagrams)",
                sent.len()
            ),
        );
        outcome.check(
            outcome.server_dropped_activation_because(reason),
            &format!("{what}: reported as a Silent Rejection, {reason:?}"),
        );
    }
}
