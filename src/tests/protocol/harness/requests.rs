//! Requests a scripted Client sends: Test Setup PDUs (RFC 9946 Figure 3)
//! and Test Activation PDUs (Figure 6), built byte by byte from the RFC
//! text and, in mode 1, signed with HMAC-SHA-256 and the KDF (both pinned
//! by independent known-answer vectors), never with our codecs or our
//! Client. A Test our Multi-Connection Coordinator would never make, such
//! as one with an mcIndex used twice or two directions, or one spread over
//! several Clients, is played by scripted Clients sending these
//! (`Scenario::add_scripted_client`).

use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;

use crate::protocol::Direction;
use crate::wire::auth::{derive_keys, hmac_sha256};

use super::{ClientReplyScript, ClientScript};

/// A scripted Client that plays Test Connection `tc` in `direction`: its
/// Setup Request as it starts, and, once a Setup Response accepts it, its
/// Activation Request to the testPort given, both at its clock's time.
/// For `Scenario::scripted_client_replying` or
/// `Scenario::add_scripted_client`.
pub fn test_connection_script(
    tc: TestConnection,
    direction: Direction,
    auth: Auth,
) -> (ClientScript, ClientReplyScript) {
    // The Activation Request is signed with the keys of the Setup
    // Request's authUnixTime.
    let setup_time = Rc::new(Cell::new(0));
    let started = setup_time.clone();
    (
        Box::new(move |clock| {
            let now = clock.unix_secs as u32;
            started.set(now);
            setup_request(tc, direction, auth, now)
        }),
        Box::new(move |clock, pdu| {
            let port = setup_acceptance(pdu)?;
            let now = clock.unix_secs as u32;
            Some((
                port,
                activation_request(direction, auth, setup_time.get(), now),
            ))
        }),
    )
}

/// Which Test Connection of which Test a Setup Request is for, and the
/// maxBandwidth it states.
#[derive(Debug, Clone, Copy)]
pub struct TestConnection {
    pub mc_ident: u16,
    pub mc_index: u8,
    pub mc_count: u8,
    /// In Mbps, below 0x8000; 0 states none.
    pub max_bandwidth: u16,
}

/// How a scripted Client authenticates.
#[derive(Debug, Clone, Copy)]
pub enum Auth {
    /// Unauthenticated Mode: authMode 0, nothing signed.
    Unauthenticated,
    /// Mode 1 with this keyId's Shared Key.
    Mode1 { key_id: u8, key: &'static [u8] },
}

/// The Test Setup PDU's size and fields (Figure 3).
mod setup {
    use std::ops::Range;

    pub const LEN: usize = 56;
    pub const MC_INDEX: usize = 4;
    pub const MC_COUNT: usize = 5;
    pub const MC_IDENT: Range<usize> = 6..8;
    pub const CMD_REQUEST: usize = 8;
    pub const CMD_RESPONSE: usize = 9;
    pub const MAX_BANDWIDTH: Range<usize> = 10..12;
    pub const TEST_PORT: Range<usize> = 12..14;
    pub const MODIFIER_BITMAP: usize = 14;
    pub const AUTH_MODE: usize = 15;
    pub const AUTH_TIME: Range<usize> = 16..20;
    pub const DIGEST: Range<usize> = 20..52;
    pub const KEY_ID: usize = 52;
    pub const CHECKSUM: Range<usize> = 54..56;
}

/// The Test Activation PDU's size and fields (Figure 6).
mod activation {
    use std::ops::Range;

    pub const LEN: usize = 104;
    pub const CMD_REQUEST: usize = 4;
    pub const CMD_RESPONSE: usize = 5;
    pub const AUTH_MODE: usize = 63;
    pub const AUTH_TIME: Range<usize> = 64..68;
    pub const DIGEST: Range<usize> = 68..100;
    pub const KEY_ID: usize = 100;
    pub const CHECKSUM: Range<usize> = 102..104;
}

/// A Setup Request (pduId 0xACE1, protocolVer 20, cmdRequest
/// CHSR_CREQ_SETUPREQ) for Test Connection `tc`, stating its maxBandwidth
/// with CHSR_USDIR_BIT for an Upstream Test (§6.1), with
/// CHSR_JUMBO_STATUS, at authUnixTime `now` (seconds); in mode 1 signed
/// with the Client Authentication Key derived from its own authUnixTime
/// (§5.4.1, ADR 0009).
pub fn setup_request(tc: TestConnection, direction: Direction, auth: Auth, now: u32) -> Vec<u8> {
    let mut b = vec![0u8; setup::LEN];
    b[0..2].copy_from_slice(&0xACE1u16.to_be_bytes());
    b[2..4].copy_from_slice(&20u16.to_be_bytes());
    b[setup::MC_INDEX] = tc.mc_index;
    b[setup::MC_COUNT] = tc.mc_count;
    b[setup::MC_IDENT].copy_from_slice(&tc.mc_ident.to_be_bytes());
    b[setup::CMD_REQUEST] = 1;
    let upstream = if direction == Direction::Upstream {
        0x8000
    } else {
        0
    };
    b[setup::MAX_BANDWIDTH].copy_from_slice(&(tc.max_bandwidth | upstream).to_be_bytes());
    b[setup::MODIFIER_BITMAP] = 0x01;
    if let Auth::Mode1 { key_id, key } = auth {
        b[setup::AUTH_MODE] = 1;
        b[setup::AUTH_TIME].copy_from_slice(&now.to_be_bytes());
        b[setup::KEY_ID] = key_id;
        let client_key = derive_keys(key, now).client().to_vec();
        sign(&mut b, setup::DIGEST, setup::CHECKSUM, &client_key);
    }
    b
}

/// The testPort of `pdu` if it is a Setup Response accepting its request
/// (cmdRequest CHSR_CREQ_SETUPRSP, cmdResponse CHSR_CRSP_ACKOK).
pub fn setup_acceptance(pdu: &[u8]) -> Option<u16> {
    (pdu.len() == setup::LEN
        && pdu[0..2] == [0xAC, 0xE1]
        && pdu[setup::CMD_REQUEST] == 2
        && pdu[setup::CMD_RESPONSE] == 1)
        .then(|| u16::from_be_bytes([pdu[setup::TEST_PORT][0], pdu[setup::TEST_PORT][1]]))
}

/// The cmdResponse of `pdu` if it is a Setup Response.
pub fn setup_response_code(pdu: &[u8]) -> Option<u8> {
    (pdu.len() == setup::LEN && pdu[0..2] == [0xAC, 0xE1] && pdu[setup::CMD_REQUEST] == 2)
        .then(|| pdu[setup::CMD_RESPONSE])
}

/// The authUnixTime of `pdu` if it is a Setup Request.
pub fn setup_request_time(pdu: &[u8]) -> Option<u32> {
    (pdu.len() == setup::LEN && pdu[0..2] == [0xAC, 0xE1] && pdu[setup::CMD_REQUEST] == 1).then(
        || {
            let time = &pdu[setup::AUTH_TIME];
            u32::from_be_bytes([time[0], time[1], time[2], time[3]])
        },
    )
}

/// Whether `pdu`, a Setup Response, is signed with the Server
/// Authentication Key derived from `key` and `setup_time`, the Setup
/// Request's authUnixTime (§5.4.1): its authDigest is HMAC-SHA-256 over
/// the PDU with authDigest and checkSum zeroed (§5.3.1).
pub fn setup_response_signed(pdu: &[u8], key: &[u8], setup_time: u32) -> bool {
    pdu.len() == setup::LEN && {
        let server_key = derive_keys(key, setup_time).server().to_vec();
        digest(pdu, setup::DIGEST, setup::CHECKSUM, &server_key) == pdu[setup::DIGEST]
    }
}

/// An Activation Request (pduId 0xACE2, protocolVer 20) in `direction`
/// (CHTA_CREQ_TESTACTUS or CHTA_CREQ_TESTACTDS) with RFC 9946 / TR-471
/// default parameters and the Server's default row, at authUnixTime
/// `now`; in mode 1 signed with the Client Authentication Key derived from
/// the Setup Request's authUnixTime, `setup_time` (§5.4.1).
pub fn activation_request(direction: Direction, auth: Auth, setup_time: u32, now: u32) -> Vec<u8> {
    let mut b = vec![0u8; activation::LEN];
    let mut put16 = |at: usize, v: u16| b[at..at + 2].copy_from_slice(&v.to_be_bytes());
    put16(0, 0xACE2);
    put16(2, 20);
    put16(6, 30); // lowThresh
    put16(8, 90); // upperThresh
    put16(10, 50); // trialInt
    put16(12, 10); // testIntTime
    put16(16, 0xFFFF); // srIndexConf: CHTA_SRIDX_DEF
    put16(20, 3); // slowAdjThresh
    put16(22, 10); // seqErrThresh
    put16(56, 1000); // subIntPeriod
    b[activation::CMD_REQUEST] = match direction {
        Direction::Upstream => 1,
        Direction::Downstream => 2,
    };
    b[18] = 1; // useOwDelVar
    b[19] = 10; // highSpeedDelta
    b[24] = 1; // ignoreOooDup
    if let Auth::Mode1 { key_id, key } = auth {
        b[activation::AUTH_MODE] = 1;
        b[activation::AUTH_TIME].copy_from_slice(&now.to_be_bytes());
        b[activation::KEY_ID] = key_id;
        let client_key = derive_keys(key, setup_time).client().to_vec();
        sign(
            &mut b,
            activation::DIGEST,
            activation::CHECKSUM,
            &client_key,
        );
    }
    b
}

/// The cmdResponse of `pdu` if it is an Activation Response, answering a
/// request with cmdResponse 0 (CHTA_CRSP_NONE).
pub fn activation_response_code(pdu: &[u8]) -> Option<u8> {
    (pdu.len() == activation::LEN
        && pdu[0..2] == [0xAC, 0xE2]
        && pdu[activation::CMD_RESPONSE] != 0)
        .then(|| pdu[activation::CMD_RESPONSE])
}

/// Signs `pdu` with HMAC-SHA-256 under `key` over the PDU with authDigest
/// and checkSum zeroed (§5.3.1).
fn sign(pdu: &mut [u8], at: Range<usize>, checksum: Range<usize>, key: &[u8]) {
    let mac = digest(pdu, at.clone(), checksum, key);
    pdu[at].copy_from_slice(&mac);
}

/// HMAC-SHA-256 under `key` over `pdu` with authDigest (`at`) and checkSum
/// zeroed (§5.3.1).
fn digest(pdu: &[u8], at: Range<usize>, checksum: Range<usize>, key: &[u8]) -> Vec<u8> {
    let mut input = pdu.to_vec();
    input[at].fill(0);
    input[checksum].fill(0);
    hmac_sha256(key, &input).to_vec()
}
