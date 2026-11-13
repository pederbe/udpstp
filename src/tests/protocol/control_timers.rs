//! Control Phase timers (RFC 9946 §6, §6.1, §6.2.2): neither end waits
//! forever. The watchdog's 1 s plus 2 s to termination is 3 s. The Client's
//! Test initiation timer, started with its Setup Request, ends a Control
//! Phase that hasn't reached the Data Phase by then, saying why: the
//! exchange it expired in and the last response ignored there. The Server
//! closes a Test Connection that got no valid Activation Request within
//! 3 s of its Setup Response, releasing it.
//!
//! A scripted mode 1 Server answers with responses built byte by byte from
//! RFC 9946 Figures 3 and 6, signed with HMAC-SHA-256 and the KDF (both
//! pinned by independent known-answer vectors).

use super::harness;

use std::net::SocketAddr;
use std::time::Duration;

use crate::protocol::Event;
use crate::wire::auth::{derive_keys, hmac_sha256};
use harness::{ControlExchange, ControlPhaseFailure, Outcome, ResponseIgnoreReason, Scenario};

const MS: fn(u64) -> Duration = Duration::from_millis;

const KEY_ID: u8 = 7;
const SHARED_KEY: &[u8] = b"a shared key";

/// An unauthenticated Setup Request (RFC 9946 Figure 3).
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

/// Why the first Client's Test initiation timer expired, if it did: the
/// exchange it expired in, and why the last response ignored there was.
type Cause = (ControlExchange, Option<ResponseIgnoreReason>);

fn timer_cause(outcome: &Outcome) -> Option<Cause> {
    outcome.client_events.iter().find_map(|e| match e {
        Event::ControlPhaseFailed(ControlPhaseFailure::TestInitiationTimedOut {
            exchange,
            last_ignored,
        }) => Some((*exchange, *last_ignored)),
        _ => None,
    })
}

fn timed_out(outcome: &Outcome) -> bool {
    timer_cause(outcome).is_some()
}

/// A Client whose Server answers the Setup Request (or not) and never the
/// Activation Request, run for `run`.
fn unanswered(seed: u64, answer_setup: bool, run: Duration) -> Outcome {
    Scenario::new(seed)
        .client_unauthenticated()
        .scripted_server(move |_, request| {
            (answer_setup && request.len() == 56).then(|| {
                let mut pdu = request.to_vec();
                pdu[8] = 2;
                pdu[9] = 1;
                pdu[12..14].copy_from_slice(&24601u16.to_be_bytes());
                pdu
            })
        })
        .run_for(run)
}

#[udpstp_req::req("RFC9946-6.1-2", "RFC9946-6.1-3")]
#[test]
fn a_client_gives_up_3_s_after_its_setup_request() {
    // : in the exchange it reached, with no response ignored.
    for (seed, answer_setup, exchange) in [
        (1, false, ControlExchange::Setup),
        (2, true, ControlExchange::Activation),
    ] {
        let early = unanswered(seed, answer_setup, MS(2990));
        early.check(!timed_out(&early), "not before 3 s");
        let late = unanswered(seed, answer_setup, MS(3010));
        late.check(
            timer_cause(&late) == Some((exchange, None)),
            &format!("Setup answered: {answer_setup}: {:?}", late.client_events),
        );
    }
}

/// How a scripted mode 1 Server answers a request: accepting it, signed
/// with the Server Authentication Key, or with a response the Client must
/// ignore: 6 s ahead of its clock, outside the ±5 s window, or signed with
/// the Client Authentication Key.
#[derive(Debug, Clone, Copy)]
enum Answer {
    Accepted,
    Late,
    Unverified,
}

/// `request`, a Setup Request (RFC 9946 Figure 3) or an Activation Request
/// (Figure 6), answered at `now` as `answer` says with the keys derived
/// from the Setup Request's authUnixTime, `setup_time`: ACKOK, for a Setup
/// Request with the Standard Port as testPort, so the Activation Request
/// comes to the same script; authUnixTime; then authDigest over the PDU
/// with authDigest and checkSum zeroed (§5.3.1).
fn answer(request: &[u8], answer: Answer, setup_time: u32, now: u32) -> Vec<u8> {
    let mut pdu = request.to_vec();
    let (time, digest, checksum) = if pdu.len() == 56 {
        pdu[8] = 2; // cmdRequest CHSR_CREQ_SETUPRSP
        pdu[9] = 1; // cmdResponse CHSR_CRSP_ACKOK
        pdu[12..14].copy_from_slice(&24601u16.to_be_bytes());
        (16..20, 20..52, 54..56)
    } else {
        pdu[5] = 1; // cmdResponse CHTA_CRSP_ACKOK
        (64..68, 68..100, 102..104)
    };
    let late = matches!(answer, Answer::Late);
    pdu[time].copy_from_slice(&(now + if late { 6 } else { 0 }).to_be_bytes());
    let keys = derive_keys(SHARED_KEY, setup_time);
    let key = match answer {
        Answer::Unverified => keys.client(),
        Answer::Accepted | Answer::Late => keys.server(),
    };
    pdu[digest.clone()].fill(0);
    pdu[checksum].fill(0);
    let mac = hmac_sha256(key, &pdu);
    pdu[digest].copy_from_slice(&mac);
    pdu
}

/// A mode 1 Client whose scripted Server answers its Setup Request with
/// `setups` and its Activation Request with `activations`, in order, run
/// until just after its Test initiation timer.
fn answered(seed: u64, setups: Vec<Answer>, activations: Vec<Answer>) -> Outcome {
    let mut setup_time = 0;
    Scenario::new(seed)
        .client_mode1(KEY_ID, SHARED_KEY)
        .scripted_server_replying_many(move |clock, request| {
            let now = clock.unix_secs as u32;
            let answers = match (request.len(), request.get(..2)) {
                (56, Some([0xAC, 0xE1])) => {
                    setup_time = u32::from_be_bytes(request[16..20].try_into().unwrap());
                    &setups
                }
                (104, Some([0xAC, 0xE2])) => &activations,
                _ => return Vec::new(),
            };
            answers
                .iter()
                .map(|&a| answer(request, a, setup_time, now))
                .collect()
        })
        .run_for(MS(3010))
}

// : the Test initiation timer names the exchange it expired in and
// why the last response ignored there was, the cause of the Client's exit
// status (ADR 0013: 107 or 108 in the Setup exchange, 157 or 158 in the
// Activation exchange, else 200) and of what it says. A response ignored in
// the Setup exchange no longer explains the timer once a Setup Response
// was accepted.
#[test]
fn the_timer_names_its_exchange_and_the_last_response_ignored_there() {
    use Answer::{Accepted, Late, Unverified};
    use ControlExchange::{Activation, Setup};
    use ResponseIgnoreReason::{AuthTimeOutOfWindow, AuthenticationFailed};
    let cases: Vec<(&str, Vec<Answer>, Vec<Answer>, Cause)> = vec![
        (
            "a Setup Response signed with the wrong key",
            vec![Unverified],
            vec![],
            (Setup, Some(AuthenticationFailed)),
        ),
        (
            "a late Setup Response",
            vec![Late],
            vec![],
            (Setup, Some(AuthTimeOutOfWindow)),
        ),
        (
            "a late Setup Response, then one signed with the wrong key",
            vec![Late, Unverified],
            vec![],
            (Setup, Some(AuthenticationFailed)),
        ),
        (
            "one signed with the wrong key, then a late one",
            vec![Unverified, Late],
            vec![],
            (Setup, Some(AuthTimeOutOfWindow)),
        ),
        (
            "an unverified Setup Response, then one accepted, and no Activation Response",
            vec![Unverified, Accepted],
            vec![],
            (Activation, None),
        ),
        (
            "a late Setup Response, then one accepted, and an unverified Activation Response",
            vec![Late, Accepted],
            vec![Unverified],
            (Activation, Some(AuthenticationFailed)),
        ),
        (
            "a late Activation Response",
            vec![Accepted],
            vec![Late],
            (Activation, Some(AuthTimeOutOfWindow)),
        ),
        (
            "an unverified Activation Response, then a late one",
            vec![Accepted],
            vec![Unverified, Late],
            (Activation, Some(AuthTimeOutOfWindow)),
        ),
    ];
    for (seed, (what, setups, activations, cause)) in (20..).zip(cases) {
        let outcome = answered(seed, setups, activations);
        outcome.check(
            timer_cause(&outcome) == Some(cause),
            &format!("{what}: {:?}", outcome.client_events),
        );
    }
}

// : a response ignored for its header checksum (RFC 9946 §5.6), or
// sent to a broadcast address (§6), explains the timer too
#[test]
fn a_corrupted_or_broadcast_response_explains_the_timer() {
    let client = harness::CLIENT_ADDR.parse::<SocketAddr>().unwrap().ip();
    for (seed, pdu_id, exchange) in [
        (30, [0xAC, 0xE1], ControlExchange::Setup),
        (31, [0xAC, 0xE2], ControlExchange::Activation),
    ] {
        let outcome = Scenario::new(seed)
            .mode1(KEY_ID, SHARED_KEY)
            .checksum()
            .network_flips_byte_to(client, pdu_id, 6)
            .run_for(MS(3010));
        outcome.check(
            timer_cause(&outcome) == Some((exchange, Some(ResponseIgnoreReason::ChecksumFailed))),
            &format!("{exchange:?}: {:?}", outcome.client_events),
        );
    }
    let outcome = Scenario::new(32)
        .client_unauthenticated()
        .setup_pdus_to("255.255.255.255".parse().unwrap())
        .scripted_server(|_, request| {
            (request.len() == 56).then(|| {
                let mut pdu = request.to_vec();
                pdu[8] = 2;
                pdu[9] = 1;
                pdu[12..14].copy_from_slice(&24601u16.to_be_bytes());
                pdu
            })
        })
        .run_for(MS(3010));
    outcome.check(
        timer_cause(&outcome)
            == Some((
                ControlExchange::Setup,
                Some(ResponseIgnoreReason::MulticastOrBroadcast),
            )),
        &format!("{:?}", outcome.client_events),
    );
}

#[udpstp_req::req("RFC9946-7.3-6")]
#[test]
fn an_activated_client_does_not_time_out() {
    let outcome = Scenario::new(3)
        .server_allows_unauthenticated()
        .client_unauthenticated()
        .run_for(MS(4000));
    outcome.check(
        outcome.client_activated_direction(0).is_some() && !timed_out(&outcome),
        &format!("{:?}", outcome.client_events),
    );
}

/// A Server whose Client sends a Setup Request and nothing more.
fn silent_client(seed: u64, run: Duration) -> Outcome {
    Scenario::new(seed)
        .server_allows_unauthenticated()
        .scripted_client(|_| setup_request())
        .run_for(run)
}

fn watchdog_expired(outcome: &Outcome) -> bool {
    outcome
        .server_events
        .iter()
        .any(|e| matches!(e, Event::ActivationWatchdogExpired { .. }))
}

#[udpstp_req::req("RFC9946-6.2.2-2", "RFC9946-6.2.2-8", "RFC9946-6.2.2-L1")]
#[test]
fn a_server_releases_a_test_connection_never_activated() {
    let early = silent_client(4, MS(2990));
    early.check(
        early.server_admitted_count() == 1 && early.server_released_count() == 0,
        "held before 3 s",
    );
    let late = silent_client(4, MS(3010));
    late.check(
        watchdog_expired(&late) && late.server_released_count() == 1,
        &format!("{:?}", late.server_events),
    );
}

#[udpstp_req::req("RFC9946-7.2.2-6")]
#[test]
fn an_activated_test_connection_is_not_closed_by_the_watchdog() {
    let outcome = Scenario::new(5)
        .server_allows_unauthenticated()
        .client_unauthenticated()
        .run_for(MS(4000));
    outcome.check(
        !watchdog_expired(&outcome) && outcome.server_released_count() == 0,
        &format!("{:?}", outcome.server_events),
    );
}

// ADR 0009 (a Setup Response that fails verification is ignored, and the
// Test initiation timer decides the outcome)
#[udpstp_req::req("RFC9946-6.1-2", "RFC9946-6.3-1")]
#[test]
fn a_forged_setup_response_leaves_the_decision_to_the_timer() {
    let outcome = Scenario::new(6)
        .client_mode1(7, b"a shared key")
        .scripted_server(|_, request| {
            // Answered, but signed with no key at all.
            (request.len() == 56).then(|| {
                let mut pdu = request.to_vec();
                pdu[8] = 2;
                pdu[9] = 1;
                pdu[12..14].copy_from_slice(&24601u16.to_be_bytes());
                pdu[20..52].fill(0xAB);
                pdu
            })
        })
        .run_for(MS(3010));
    outcome.check(
        outcome.client_dropped_setup_response_because(ResponseIgnoreReason::AuthenticationFailed)
            && timer_cause(&outcome)
                == Some((
                    ControlExchange::Setup,
                    Some(ResponseIgnoreReason::AuthenticationFailed),
                )),
        &format!("{:?}", outcome.client_events),
    );
}

// RFC 9946 §7.2.1 (a rejected Activation Request ends the Test
// Connection at once: released once, with no Activation Watchdog after)
#[test]
fn a_rejected_activation_is_released_once() {
    let outcome = Scenario::new(7)
        .server_allows_unauthenticated()
        .client_unauthenticated()
        .client_requests_row(harness::RowRequest::FixedRate(5))
        .run_for(MS(4000));
    outcome.check(
        outcome.client_activation_rejected(0)
            && outcome.server_released_count() == 1
            && !watchdog_expired(&outcome),
        &format!("{:?}", outcome.server_events),
    );
}
