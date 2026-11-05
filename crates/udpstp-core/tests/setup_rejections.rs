//! Coded Setup rejections (RFC 9946 §6.2.1; docs/spec.md "Control Phase and
//! admission (Server)"): an authentic Setup Request that doesn't match the
//! Server's protocol version, Jumbo or Traditional MTU setting or
//! Authentication Mode is answered with a Setup Response carrying the
//! reason, signed with the Server key. Requests that aren't authentic stay
//! silent, and so, by default, do the other authentication failures (spec
//!  story 77): a valid digest outside the time window, and a mode this
//! Server doesn't run.
//!
//! A scripted Client builds and signs its Setup Request byte by byte from
//! RFC 9946 Figure 3.

mod harness;

use std::time::Duration;

use harness::{Outcome, Scenario};
use udpstp_wire::auth::{derive_keys, hmac_sha256};

const KEY_ID: u8 = 7;
const SHARED_KEY: &[u8] = b"a shared key";
const RUN: Duration = Duration::from_millis(100);

const BADVER: u8 = 2;
const BADJS: u8 = 3;
const BADTMTU: u8 = 11;

const PROTOCOL_VER: std::ops::Range<usize> = 2..4;
const MC_INDEX: usize = 4;
const MC_IDENT: std::ops::Range<usize> = 6..8;
const MODIFIER_BITMAP: usize = 14;
const AUTH_MODE: usize = 15;
const AUTH_TIME: std::ops::Range<usize> = 16..20;
const DIGEST: std::ops::Range<usize> = 20..52;

/// A mode 1 Setup Request (Figure 3), changed by `edit`, then signed.
fn signed_request(time: u32, edit: impl Fn(&mut [u8])) -> Vec<u8> {
    let mut b = vec![0u8; 56];
    b[0..2].copy_from_slice(&0xACE1u16.to_be_bytes());
    b[PROTOCOL_VER].copy_from_slice(&20u16.to_be_bytes());
    b[5] = 1;
    b[6..8].copy_from_slice(&0x1234u16.to_be_bytes());
    b[8] = 1;
    b[MODIFIER_BITMAP] = 0x01;
    b[AUTH_MODE] = 1;
    b[AUTH_TIME].copy_from_slice(&time.to_be_bytes());
    b[52] = KEY_ID;
    edit(&mut b);
    let keys = derive_keys(SHARED_KEY, time);
    let digest = hmac_sha256(keys.client(), &b);
    b[DIGEST].copy_from_slice(&digest);
    b
}

/// The Setup Responses the Server sent.
fn responses(outcome: &Outcome) -> Vec<Vec<u8>> {
    outcome
        .trace
        .iter()
        .filter(|t| t.payload.starts_with(&[0xAC, 0xE1]) && t.payload.get(8) == Some(&2))
        .map(|t| t.payload.clone())
        .collect()
}

/// Whether `response` carries the digest of the Server Authentication Key
/// derived from the request's authUnixTime (§5.4.1).
fn signed_by_server(response: &[u8], request_time: u32) -> bool {
    let mut zeroed = response.to_vec();
    zeroed[DIGEST].fill(0);
    zeroed[54..56].fill(0);
    let keys = derive_keys(SHARED_KEY, request_time);
    hmac_sha256(keys.server(), &zeroed) == response[DIGEST]
}

/// Runs a scripted Client sending the Setup Request `edit` makes, stamped
/// `skew` seconds from the Server's clock, and returns the Server's
/// responses and the request's authUnixTime.
fn run(seed: u64, skew: i64, edit: impl Fn(&mut [u8]) + 'static) -> (Outcome, Vec<Vec<u8>>, u32) {
    run_signed(seed, skew, edit, |_| {})
}

/// As [`run`], with `after` applied once the request is signed.
fn run_signed(
    seed: u64,
    skew: i64,
    edit: impl Fn(&mut [u8]) + 'static,
    after: impl Fn(&mut [u8]) + 'static,
) -> (Outcome, Vec<Vec<u8>>, u32) {
    let time = std::rc::Rc::new(std::cell::Cell::new(0u32));
    let t = time.clone();
    let outcome = Scenario::new(seed)
        .server_key(KEY_ID, SHARED_KEY)
        .scripted_client(move |clock| {
            let stamped = (clock.unix_secs as i64 + skew) as u32;
            t.set(stamped);
            let mut request = signed_request(stamped, &edit);
            after(&mut request);
            request
        })
        .run_for(RUN);
    let got = responses(&outcome);
    (outcome, got, time.get())
}

fn check_rejected(seed: u64, skew: i64, edit: impl Fn(&mut [u8]) + 'static, code: u8) -> Vec<u8> {
    let (outcome, got, time) = run(seed, skew, edit);
    outcome.check(
        got.len() == 1 && got[0][9] == code && signed_by_server(&got[0], time),
        &format!("expected a signed code {code}: {got:?}"),
    );
    got[0].clone()
}

// BADVER, with protocolVer updated to the expected version.
#[udpstp_req::req(
    "RFC9946-6.2.1-1",
    "RFC9946-6.2.1-3",
    "RFC9946-6.2.1-6",
    "RFC9946-12.3.2-L1"
)]
#[test]
fn a_bad_protocol_version_is_rejected_with_the_expected_version() {
    let response = check_rejected(
        1,
        0,
        |b| b[PROTOCOL_VER].copy_from_slice(&19u16.to_be_bytes()),
        BADVER,
    );
    assert_eq!(response[PROTOCOL_VER], 20u16.to_be_bytes());
    let (_, got, time) = run(1, -3, |b| {
        b[PROTOCOL_VER].copy_from_slice(&19u16.to_be_bytes())
    });
    let stamped = u32::from_be_bytes(got[0][AUTH_TIME].try_into().unwrap());
    assert_eq!(
        stamped,
        time + 3,
        "the response carries the Server's own time"
    );
}

#[udpstp_req::req("RFC9946-6.2.1-2", "RFC9946-6.2.1-3", "RFC9946-12.3.3-L1")]
#[test]
fn a_jumbo_setting_the_server_doesnt_use_is_rejected() {
    check_rejected(2, 0, |b| b[MODIFIER_BITMAP] = 0, BADJS);
}

#[udpstp_req::req("RFC9946-6.2.1-2")]
#[test]
fn a_traditional_mtu_setting_the_server_doesnt_use_is_rejected() {
    check_rejected(3, 0, |b| b[MODIFIER_BITMAP] = 0x03, BADTMTU);
}

// Mode 2 incorporates mode 1: its Setup is accepted and answered in mode 2;
//
#[udpstp_req::req("RFC9946-6.2.2-3", "RFC9946-12.3.4-L1")]
#[test]
fn a_mode_2_request_is_accepted_in_mode_2() {
    let (outcome, got, _) = run(4, 0, |b| b[AUTH_MODE] = 2);
    outcome.check(
        got.len() == 1 && got[0][9] == 1 && got[0][AUTH_MODE] == 2,
        &format!("{got:?}"),
    );
}

// A valid digest outside the time window stays silent (docs/spec.md story 77:
// authentication failures are silent unless Troubleshooting Mode is on).
#[udpstp_req::req("RFC9946-6.2-2")]
#[test]
fn an_authentic_request_outside_the_time_window_gets_no_response() {
    let (outcome, got, _) = run(5, -10, |_| {});
    outcome.check(got.is_empty(), &format!("{got:?}"));
}

// Both flags wrong: the Jumbo mismatch is reported.
#[test]
fn traditional_mtu_without_jumbo_is_rejected_as_a_jumbo_mismatch() {
    check_rejected(9, 0, |b| b[MODIFIER_BITMAP] = 0x02, BADJS);
}

// The options come before the Multi-Connection fields: a request wrong in
// both gets the option's code, where OB-UDPST checks MCINVPAR right after
// BADVER, before BADJS and BADTMTU (conformance: setup-check-order).
#[test]
fn the_options_are_judged_before_the_multi_connection_fields() {
    // modifierBitmap 0 and mcIdent 0.
    check_rejected(
        11,
        0,
        |b| {
            b[MODIFIER_BITMAP] = 0;
            b[MC_IDENT].fill(0);
        },
        BADJS,
    );
    // Traditional MTU, which the Server doesn't use, and mcIndex 1 of 1.
    check_rejected(
        12,
        0,
        |b| {
            b[MODIFIER_BITMAP] = 0x03;
            b[MC_INDEX] = 1;
        },
        BADTMTU,
    );
    // protocolVer 19 and mcIdent 0.
    check_rejected(
        13,
        0,
        |b| {
            b[PROTOCOL_VER].copy_from_slice(&19u16.to_be_bytes());
            b[MC_IDENT].fill(0);
        },
        BADVER,
    );
}

// Authentication comes before any field: an unauthenticated request to a
// Server that doesn't allow one is refused silently, however wrong its
// options, where OB-UDPST judges the fields first and answers BADVER
// (conformance: setup-check-order).
#[test]
fn a_disallowed_unauthenticated_request_is_silent_whatever_its_options() {
    let (outcome, got, _) = run_signed(
        14,
        0,
        |b| {
            b[AUTH_MODE] = 0;
            b[PROTOCOL_VER].copy_from_slice(&19u16.to_be_bytes());
            b[MODIFIER_BITMAP] = 0;
        },
        |b| b[DIGEST].fill(0),
    );
    outcome.check(
        got.is_empty()
            && outcome.server_dropped_setup_because(
                harness::SilentRejectionReason::UnauthenticatedNotAllowed,
            ),
        &format!("{got:?}; {:?}", outcome.server_events),
    );
}

// A replay of a request that was rejected stays silent: the rejection comes
// after the replay cache.
#[test]
fn a_replayed_rejected_request_gets_one_response() {
    let time = std::rc::Rc::new(std::cell::Cell::new(0u32));
    let t = time.clone();
    let outcome = Scenario::new(10)
        .server_key(KEY_ID, SHARED_KEY)
        .scripted_client(move |clock| {
            t.set(clock.unix_secs as u32);
            signed_request(clock.unix_secs as u32, |b| b[MODIFIER_BITMAP] = 0)
        })
        .network_replays([0xAC, 0xE1], 0, Duration::from_secs(1))
        .run_for(Duration::from_millis(1100));
    let got = responses(&outcome);
    outcome.check(got.len() == 1 && got[0][9] == BADJS, &format!("{got:?}"));
}

#[udpstp_req::req("RFC9946-6.2.1-4")]
#[test]
fn a_request_whose_digest_fails_gets_no_response() {
    let (outcome, got, _) = run_signed(6, 0, |_| {}, |b| b[DIGEST.start] ^= 0xFF);
    outcome.check(got.is_empty(), &format!("{got:?}"));
}

// In Unauthenticated Mode the rejection is unsigned.
#[udpstp_req::req("RFC9946-6.2.1-2")]
#[test]
fn an_unauthenticated_request_gets_unsigned_rejections() {
    for (at, value, code) in [
        (2, &19u16.to_be_bytes()[..], BADVER),
        (MODIFIER_BITMAP, &[0][..], BADJS),
        (MODIFIER_BITMAP, &[0x03][..], BADTMTU),
    ] {
        let outcome = Scenario::new(7)
            .server_allows_unauthenticated()
            .client_unauthenticated()
            .network_writes_setup(0, at, value)
            .run_for(RUN);
        let got = responses(&outcome);
        outcome.check(
            got.len() == 1
                && got[0][9] == code
                && got[0][AUTH_MODE] == 0
                && got[0][DIGEST].iter().all(|&b| b == 0),
            &format!("{code}: {got:?}"),
        );
    }
}
