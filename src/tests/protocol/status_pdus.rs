//! Status PDUs from the Load Receiver (RFC 9946 §8.2): sent every Trial
//! Interval once Load PDUs arrive, with Trial Interval and Sub-Interval
//! receive counts.
//!
//! Status PDUs are read by the byte offsets of RFC 9946 Figure 10, not with
//! our codec. Expected counts come from the Load PDUs the trace shows the
//! Client received.

use super::harness;

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use crate::protocol::Direction;
use crate::wire::auth::{derive_keys, hmac_sha256};
use harness::{DataEvent, Outcome, RowRequest, Scenario, TraceEntry};

const MS: Duration = Duration::from_millis(1);
/// The Client's defaults: trialInt 50 ms, subIntPeriod 1000 ms.
const TRIAL: Duration = Duration::from_millis(50);
const SUB_INTERVAL: Duration = Duration::from_millis(1000);
const STATUS_NODEL: u32 = u32::MAX;

// ---- Status PDU, RFC 9946 Figure 10 ----

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn spdu_seq_no(b: &[u8]) -> u32 {
    u32_at(b, 4)
}

fn sub_int_seq_no(b: &[u8]) -> u32 {
    u32_at(b, 36)
}

/// sisSav: rxDatagrams, rxBytes, deltaTime and accumTime.
fn sis_sav(b: &[u8]) -> (u32, u64, u32, u32) {
    let rx_bytes = u64::from_be_bytes(b[44..52].try_into().unwrap_or_default());
    (u32_at(b, 40), rx_bytes, u32_at(b, 52), u32_at(b, 92))
}

/// tiDeltaTime, tiRxDatagrams, tiRxBytes.
fn trial(b: &[u8]) -> (u32, u32, u32) {
    (u32_at(b, 140), u32_at(b, 144), u32_at(b, 148))
}

// ---- Scenarios ----

fn downstream(seed: u64, row: u16) -> Scenario {
    Scenario::new(seed)
        // A Fixed-Rate Test needs an Operator Key (RFC 9946 §4.1).
        .operator(7, b"an operator key")
        .client_direction(Direction::Downstream)
        .client_requests_row(RowRequest::FixedRate(row))
}

fn is_status(t: &TraceEntry) -> bool {
    t.len == 204 && t.payload.starts_with(&[0xFE, 0xED])
}

fn is_load(t: &TraceEntry) -> bool {
    t.payload.starts_with(&[0xBE, 0xEF])
}

/// The trace positions of the Status PDUs the Client sent.
fn statuses(outcome: &Outcome) -> Vec<usize> {
    let client: std::net::SocketAddr = harness::CLIENT_ADDR.parse().unwrap();
    let found: Vec<usize> = (0..outcome.trace.len())
        .filter(|&i| outcome.trace[i].source == client && is_status(&outcome.trace[i]))
        .collect();
    outcome.check(!found.is_empty(), "the Client sent Status PDUs");
    found
}

/// Load PDUs the Client received between two trace positions.
fn loads_between(outcome: &Outcome, from: usize, to: usize) -> Vec<&TraceEntry> {
    let client: std::net::SocketAddr = harness::CLIENT_ADDR.parse().unwrap();
    outcome.trace[from..to]
        .iter()
        .filter(|t| t.destination == client && t.delivered && is_load(t))
        .collect()
}

// docs/spec.md Data Phase (Status PDUs every Trial Interval, only after the
// first Load PDU has arrived)
#[udpstp_req::req(
    "RFC9946-8.2-1",
    "RFC9946-8.2-L2",
    "RFC9946-8.2-9",
    "RFC9946-5.1-5",
    "RFC9097-8.1-L4",
    "RFC9097-8.1-L6"
)]
#[test]
fn status_pdus_follow_the_first_load_pdu_every_trial_interval() {
    let outcome = downstream(1, 5).run_for(Duration::from_millis(500));

    let statuses = statuses(&outcome);
    let first_load = loads_between(&outcome, 0, outcome.trace.len())
        .first()
        .map(|t| t.at);
    outcome.check(first_load.is_some(), "Load PDUs arrived");
    let first_load = first_load.unwrap_or_default();
    outcome.check(
        outcome.trace[statuses[0]].at == first_load + TRIAL,
        "the first Status PDU comes one Trial Interval after the first Load PDU",
    );
    for (k, &i) in statuses.iter().enumerate() {
        let t = &outcome.trace[i];
        outcome.check(
            spdu_seq_no(&t.payload) == k as u32 + 1,
            "spduSeqNo counts from 1",
        );
        outcome.check(
            t.payload[2] == 0 && t.payload[3] == 0,
            "TEST_ACT_TEST, rxStopped clear",
        );
        outcome.check(
            t.payload[8..36].iter().all(|&b| b == 0),
            "a Downstream Test's Status PDU carries no Sending Rate Structure",
        );
        outcome.check(
            k > 0
                || (u32_at(&t.payload, 128) == STATUS_NODEL
                    && u32_at(&t.payload, 132) == STATUS_NODEL),
            "the first Status PDU has no RTT yet: no Load PDU echoed a spduTime",
        );
    }
    for pair in statuses.windows(2) {
        let gap = outcome.trace[pair[1]].at - outcome.trace[pair[0]].at;
        outcome.check(
            gap == TRIAL,
            &format!("one per Trial Interval, not {gap:?}"),
        );
    }
}

// tiDeltaTime, tiRxDatagrams, tiRxBytes for the Trial Interval completed by
// this Status PDU.
#[udpstp_req::req("RFC9946-8.2-6", "RFC9097-8.1-L7")]
#[test]
fn each_status_pdu_counts_its_trial_interval() {
    let outcome = downstream(2, 123).run_for(Duration::from_millis(400));

    let statuses = statuses(&outcome);
    let mut from = 0;
    for (k, &i) in statuses.iter().enumerate() {
        let received = loads_between(&outcome, from, i);
        let bytes: usize = received.iter().map(|t| t.len).sum();
        let (delta, datagrams, rx_bytes) = trial(&outcome.trace[i].payload);
        outcome.check(
            datagrams as usize == received.len() && rx_bytes as usize == bytes,
            &format!(
                "Status PDU {}: {} datagrams, {bytes} bytes received, reported {datagrams}, {rx_bytes}",
                k + 1,
                received.len()
            ),
        );
        outcome.check(
            delta == 50_000,
            &format!("tiDeltaTime is the Trial Interval in µs, not {delta}"),
        );
        from = i;
    }
}

// subIntSeqNo starts at 1 for the last completed Sub-Interval; sisSav;
// accumTime is the sum of deltaTime in ms. SubIntSeqNo 0 and a zero
// sisSav before the first Sub-Interval completes; a Sub-Interval opens at
// the first Status PDU and closes just after the first Status PDU more
// than subIntPeriod - trialInt/2 after it opened)
#[udpstp_req::req("RFC9946-8.2-L3", "RFC9946-8.2-L11", "RFC9946-8.2-6")]
#[test]
fn sub_intervals_are_counted_from_the_first_status_pdu() {
    let outcome = downstream(3, 5).run_for(Duration::from_millis(3300));

    let statuses = statuses(&outcome);
    let opened_at = outcome.trace[statuses[0]].at;
    // With a 50 ms Trial Interval, the one 1000 ms after the opening one
    // closes a Sub-Interval, and the one after it reports it.
    let per_sub_interval = (SUB_INTERVAL.as_millis() / TRIAL.as_millis()) as usize;
    let mut opened = statuses[0];
    let mut completed = 0;
    for (k, &i) in statuses.iter().enumerate() {
        let b = &outcome.trace[i].payload;
        let reports = k > 1 && (k - 1) % per_sub_interval == 0;
        if reports {
            completed += 1;
            let closed = statuses[k - 1];
            let received = loads_between(&outcome, opened, closed);
            let bytes: u64 = received.iter().map(|t| t.len as u64).sum();
            let (datagrams, rx_bytes, delta, accum) = sis_sav(b);
            outcome.check(
                sub_int_seq_no(b) == completed,
                &format!(
                    "Sub-Interval {completed} is reported at {:?}",
                    outcome.trace[i].at - opened_at
                ),
            );
            outcome.check(
                datagrams as usize == received.len() && rx_bytes == bytes,
                &format!("sisSav counts {} datagrams, {bytes} bytes", received.len()),
            );
            outcome.check(
                delta == 1_000_000,
                &format!("deltaTime is 1 s, not {delta} µs"),
            );
            outcome.check(
                accum == 1000 * completed,
                &format!("accumTime adds up the Sub-Intervals: {accum}"),
            );
            opened = closed;
        } else if completed == 0 {
            outcome.check(
                sub_int_seq_no(b) == 0 && b[40..96].iter().all(|&x| x == 0),
                "before the first Sub-Interval completes, subIntSeqNo is 0 and sisSav is zero",
            );
        } else {
            outcome.check(
                sub_int_seq_no(b) == completed,
                "later Status PDUs repeat the last completed Sub-Interval",
            );
        }
    }
    outcome.check(completed >= 3, "three Sub-Intervals completed");
}

// Status PDUs carry the authMode; outside mode 2 keyId, authUnixTime and
// authDigest are zero.
#[udpstp_req::req("RFC9946-5.3.2-6")]
#[test]
fn status_pdus_carry_the_auth_mode_and_zero_auth_fields() {
    let cases = [
        ("mode 1", Scenario::new(4).mode1(7, b"a shared key"), 1u8),
        (
            "unauthenticated",
            Scenario::new(5)
                .server_allows_unauthenticated()
                .client_unauthenticated(),
            0,
        ),
    ];
    for (what, scenario, auth_mode) in cases {
        let outcome = scenario
            .client_direction(Direction::Downstream)
            .run_for(Duration::from_millis(300));

        for i in statuses(&outcome) {
            let b = &outcome.trace[i].payload;
            outcome.check(
                b[163] == auth_mode,
                &format!("{what}: authMode {auth_mode}"),
            );
            outcome.check(
                b[164..201].iter().all(|&x| x == 0),
                &format!("{what}: authUnixTime, authDigest and keyId are zero"),
            );
        }
    }
}

// The checksum covers the whole PDU.
#[udpstp_req::req("RFC9946-5.6-3", "RFC9946-8.2-L1")]
#[test]
fn status_pdus_carry_a_checksum_when_configured() {
    let outcome = downstream(6, 5)
        .checksum()
        .run_for(Duration::from_millis(300));

    for i in statuses(&outcome) {
        let b = &outcome.trace[i].payload;
        let mut sum: u32 = b
            .chunks(2)
            .map(|w| u32::from(u16::from_be_bytes([w[0], w[1]])))
            .sum();
        while sum > 0xFFFF {
            sum = (sum & 0xFFFF) + (sum >> 16);
        }
        outcome.check(
            b[202..204] != [0, 0] && sum == 0xFFFF,
            "the Status PDU sums to 0xFFFF with its checksum",
        );
    }
}

/// A scripted Server that accepts the Setup and Activation Requests and
/// then sends `loads`.
fn server_sending(seed: u64, loads: Vec<Vec<u8>>) -> Scenario {
    Scenario::new(seed)
        .client_unauthenticated()
        .client_direction(Direction::Downstream)
        .scripted_server_replying_many(move |_, request| {
            if request.len() == 56 {
                let mut pdu = request.to_vec();
                pdu[8] = 2;
                pdu[9] = 1;
                pdu[12..14].copy_from_slice(&24601u16.to_be_bytes());
                return vec![pdu];
            }
            if request.len() == 104 && request.starts_with(&[0xAC, 0xE2]) {
                let mut ack = request.to_vec();
                ack[5] = 1;
                return std::iter::once(ack).chain(loads.clone()).collect();
            }
            Vec::new()
        })
}

// RFC 9946 §8.1 (udpPayload "allows" accurate counts under receive
// truncation). Our runtime always knows a datagram's actual length, even
// when it reads only the header, so bytes are counted from that: a peer
// can't inflate the results by stating a larger udpPayload.
#[udpstp_req::req("RFC9946-8.1-L7")]
#[test]
fn bytes_are_counted_from_the_datagram_not_its_udp_payload_field() {
    // A 32-byte Load PDU (header only) that says its UDP payload was 1000
    // bytes.
    let mut load = vec![0u8; 32];
    load[0..2].copy_from_slice(&[0xBE, 0xEF]);
    load[4..8].copy_from_slice(&1u32.to_be_bytes());
    load[8..10].copy_from_slice(&1000u16.to_be_bytes());
    let outcome = server_sending(7, vec![load]).run_for(Duration::from_millis(100));

    let statuses = statuses(&outcome);
    let (_, datagrams, bytes) = trial(&outcome.trace[statuses[0]].payload);
    outcome.check(
        datagrams == 1 && bytes == 32,
        &format!("one datagram of 32 bytes, not {datagrams} of {bytes}"),
    );
}

#[udpstp_req::req("RFC9946-8.2-1", "RFC9946-8.2-2")]
#[test]
fn no_status_pdu_is_sent_before_a_load_pdu_arrives() {
    let client: std::net::SocketAddr = harness::CLIENT_ADDR.parse().unwrap();
    let outcome = server_sending(10, Vec::new()).run_for(Duration::from_millis(500));
    outcome.check(
        outcome.client_activated_direction(0).is_some(),
        "the Test was activated",
    );
    outcome.check(
        !outcome
            .trace
            .iter()
            .any(|t| t.source == client && is_status(t)),
        "no Status PDU without a Load PDU",
    );
}

// ---- The Server as Load Sender receiving Status PDUs ----

const KEY_ID: u8 = 7;
const KEY: &[u8] = b"a shared key";

fn sign(
    pdu: &mut [u8],
    digest: std::ops::Range<usize>,
    checksum: std::ops::Range<usize>,
    key: &[u8],
) {
    let mut zeroed = pdu.to_vec();
    zeroed[digest.clone()].fill(0);
    zeroed[checksum].fill(0);
    pdu[digest].copy_from_slice(&hmac_sha256(key, &zeroed));
}

/// A mode 1 Setup Request (RFC 9946 Figure 3), signed with the Client key.
fn setup_request(time: u32) -> Vec<u8> {
    let mut b = vec![0u8; 56];
    b[0..2].copy_from_slice(&[0xAC, 0xE1]);
    b[2..4].copy_from_slice(&20u16.to_be_bytes());
    b[5] = 1;
    b[6..8].copy_from_slice(&0x1234u16.to_be_bytes());
    b[8] = 1;
    b[14] = 0x01;
    b[15] = 1;
    b[16..20].copy_from_slice(&time.to_be_bytes());
    b[52] = KEY_ID;
    sign(&mut b, 20..52, 54..56, derive_keys(KEY, time).client());
    b
}

/// A mode 1 Downstream Activation Request (RFC 9946 Figure 6) with default
/// parameters, signed with the Client key of the Setup's authUnixTime.
fn activation_request(setup_time: u32, now: u32) -> Vec<u8> {
    let mut b = vec![0u8; 104];
    let mut put16 = |at: usize, v: u16| b[at..at + 2].copy_from_slice(&v.to_be_bytes());
    put16(0, 0xACE2);
    put16(2, 20);
    put16(6, 30);
    put16(8, 90);
    put16(10, 50);
    put16(12, 10);
    put16(16, 0xFFFF);
    put16(20, 3);
    put16(22, 10);
    put16(56, 1000);
    b[4] = 2;
    b[18] = 1;
    b[19] = 10;
    b[24] = 1;
    b[63] = 1;
    b[64..68].copy_from_slice(&now.to_be_bytes());
    b[100] = KEY_ID;
    sign(
        &mut b,
        68..100,
        102..104,
        derive_keys(KEY, setup_time).client(),
    );
    b
}

/// A Status PDU whose authentication fields hold non-zero values, as
/// OB-UDPST's carry ECN counts there.
fn status_with_auth_fields(seq: u32) -> Vec<u8> {
    let mut b = vec![0u8; 204];
    b[0..2].copy_from_slice(&[0xFE, 0xED]);
    b[4..8].copy_from_slice(&seq.to_be_bytes());
    b[163] = 1;
    b[164..168].copy_from_slice(&0x0102_0304u32.to_be_bytes());
    b[168..200].fill(0x5A);
    b[200] = 3;
    b
}

/// The Server's Data Events when a scripted mode 1 Client sends `statuses`,
/// one after each Load PDU it receives.
fn load_sender_receiving(checksum: bool, statuses: Vec<Vec<u8>>) -> (Outcome, Vec<DataEvent>) {
    let setup_time = Rc::new(Cell::new(0u32));
    let t = setup_time.clone();
    let mut test_port = None;
    let mut to_send = statuses.into_iter();
    let mut scenario = Scenario::new(8).server_key(KEY_ID, KEY);
    if checksum {
        scenario = scenario.checksum();
    }
    let outcome = scenario
        .scripted_client_replying(
            move |clock| {
                t.set(clock.unix_secs as u32);
                setup_request(clock.unix_secs as u32)
            },
            move |clock, pdu| {
                if pdu.len() == 56 && pdu[8] == 2 && pdu[9] == 1 {
                    let port = u16::from_be_bytes([pdu[12], pdu[13]]);
                    test_port = Some(port);
                    return Some((
                        port,
                        activation_request(setup_time.get(), clock.unix_secs as u32),
                    ));
                }
                if !pdu.starts_with(&[0xBE, 0xEF]) {
                    return None;
                }
                Some((test_port?, to_send.next()?))
            },
        )
        .run_for(Duration::from_millis(300));
    let events = outcome.server_data_events.iter().map(|(_, e)| *e).collect();
    (outcome, events)
}

fn invalid_count(events: &[DataEvent]) -> u32 {
    events
        .iter()
        .map(|e| match e {
            DataEvent::InvalidStatusPdus { count } => *count,
            _ => 0,
        })
        .sum()
}

/// Fills the checkSum field (RFC 9946 §5.6), computed here, not by the
/// codec.
fn with_checksum(mut b: Vec<u8>) -> Vec<u8> {
    b[202..204].fill(0);
    let mut sum: u32 = b
        .chunks(2)
        .map(|w| u32::from(u16::from_be_bytes([w[0], w[1]])))
        .sum();
    while sum > 0xFFFF {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    let sum = !(sum as u16);
    b[202..204].copy_from_slice(&sum.to_be_bytes());
    b
}

// RFC 9946 §5.3.2 (authentication of Status PDUs is mode 2's); spec
//  (in mode 1 non-zero values in the auth fields are tolerated)
#[test]
fn the_load_sender_tolerates_auth_fields_in_mode1_status_pdus() {
    let (outcome, events) = load_sender_receiving(false, vec![status_with_auth_fields(1)]);
    outcome.check(
        events.contains(&DataEvent::FirstStatusPduReceived) && invalid_count(&events) == 0,
        &format!("the Status PDU with auth fields was accepted: {events:?}"),
    );
}

// A Status PDU is 204 bytes. A datagram with another pduId is not handed to
// the Data Phase as a Status PDU at all.
#[udpstp_req::req("RFC9946-8.2-3")]
#[test]
fn the_load_sender_rejects_status_pdus_of_other_sizes() {
    let mut longer = status_with_auth_fields(2);
    longer.push(0);
    let (outcome, events) = load_sender_receiving(
        false,
        vec![status_with_auth_fields(1)[..203].to_vec(), longer],
    );
    outcome.check(
        !events.contains(&DataEvent::FirstStatusPduReceived) && invalid_count(&events) == 2,
        &format!("both are invalid: {events:?}"),
    );
}

// With checksums configured, a populated checkSum is verified; a zero one
// means not populated.
#[udpstp_req::req("RFC9946-5.6-4", "RFC9946-5.6-5", "RFC9946-8.2-3")]
#[test]
fn the_load_sender_verifies_a_populated_status_checksum() {
    let mut bad = with_checksum(status_with_auth_fields(1));
    bad[100] ^= 1;
    let good = with_checksum(status_with_auth_fields(2));
    let (outcome, events) = load_sender_receiving(true, vec![bad, good]);
    outcome.check(
        events.contains(&DataEvent::FirstStatusPduReceived) && invalid_count(&events) == 1,
        &format!("only the one with a wrong checksum is invalid: {events:?}"),
    );
    let (outcome, events) = load_sender_receiving(true, vec![status_with_auth_fields(1)]);
    outcome.check(
        events.contains(&DataEvent::FirstStatusPduReceived) && invalid_count(&events) == 0,
        &format!("an unpopulated checkSum is accepted: {events:?}"),
    );
}

// A Sub-Interval closes
// after the first Status PDU more than subIntPeriod - trialInt/2 after it
// opened, so a late Status PDU does not move the later ones off the
// Trial Interval grid)
#[test]
fn a_late_status_pdu_does_not_shift_the_next_sub_interval() {
    let baseline = downstream(9, 5).run_for(Duration::from_millis(300));
    let opened_at = baseline.trace[statuses(&baseline)[0]].at;
    // The Client's engine is stalled over the Status PDU that closes the
    // first Sub-Interval, so it goes out 20 ms late. Each Sub-Interval is
    // reported by the Status PDU after the one that closes it.
    let outcome = downstream(9, 5)
        .client_engine_stall(opened_at + SUB_INTERVAL - 10 * MS, 30 * MS)
        .run_for(opened_at + 2 * SUB_INTERVAL + TRIAL + 10 * MS);

    let closing: Vec<(Duration, u32, u32)> = statuses(&outcome)
        .into_iter()
        .map(|i| (&outcome.trace[i], &outcome.trace[i].payload))
        .filter(|(_, b)| sub_int_seq_no(b) > 0)
        .map(|(t, b)| (t.at - opened_at, sis_sav(b).2, sis_sav(b).3))
        .fold(Vec::new(), |mut firsts, (at, delta, accum)| {
            if firsts.last().is_none_or(|&(_, _, a)| a != accum) {
                firsts.push((at, delta, accum));
            }
            firsts
        });
    outcome.check(
        closing
            == vec![
                (SUB_INTERVAL + TRIAL, 1_020_000, 1020),
                (2 * SUB_INTERVAL + TRIAL, 980_000, 2000),
            ],
        &format!("Sub-Intervals close late once, then on the grid: {closing:?}"),
    );
}
