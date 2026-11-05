//! The Downstream Load stream: the Server sends Load PDUs at a fixed row and
//! the Client receives them (RFC 9946 §7.2.2, §8.1).
//!
//! Load PDUs are read by the byte offsets of RFC 9946 Figure 8, not with our
//! codec. Expected rows come from the Sending Rate Table's documented
//! mapping (row 5: one 597-byte datagram every 1 ms; row 123: one 1222-byte
//! datagram every 100 µs, plus two 1222-byte datagrams and a 347-byte Add-on
//! every 1 ms).

mod harness;

use std::time::Duration;

use harness::{DataEvent, Outcome, RowRequest, Scenario, TraceEntry};
use udpstp_core::Direction;

const RUN: Duration = Duration::from_millis(200);
const MS: Duration = Duration::from_millis(1);

// ---- Load PDU header, RFC 9946 Figure 8 ----

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn test_action(b: &[u8]) -> u8 {
    b[2]
}

fn rx_stopped(b: &[u8]) -> u8 {
    b[3]
}

fn seq_no(b: &[u8]) -> u32 {
    u32_at(b, 4)
}

fn udp_payload(b: &[u8]) -> u16 {
    u16_at(b, 8)
}

/// lpduTime_sec and lpduTime_nsec as one duration since the Unix epoch.
fn lpdu_time(b: &[u8]) -> Duration {
    Duration::new(u64::from(u32_at(b, 20)), u32_at(b, 24))
}

fn content(b: &[u8]) -> &[u8] {
    &b[32..]
}

// ---- Scenarios ----

fn downstream(seed: u64, row: u16) -> Scenario {
    Scenario::new(seed)
        // A Fixed-Rate Test needs an Operator Key (RFC 9946 §4.1).
        .operator(7, b"an operator key")
        .client_direction(Direction::Downstream)
        .client_requests_row(RowRequest::FixedRate(row))
}

/// When the Server sent its Activation Response.
fn activated_at(outcome: &Outcome) -> Duration {
    let server_ip = harness::SERVER_ADDR
        .parse::<std::net::SocketAddr>()
        .map(|a| a.ip())
        .ok();
    let response = outcome
        .trace
        .iter()
        .find(|t| Some(t.source.ip()) == server_ip && t.payload.starts_with(&[0xAC, 0xE2]));
    outcome.check(response.is_some(), "the Server sent an Activation Response");
    response.map_or(Duration::ZERO, |t| t.at)
}

fn loads(outcome: &Outcome) -> Vec<&TraceEntry> {
    let loads = outcome.load_pdus_to_client();
    outcome.check(!loads.is_empty(), "the Client received Load PDUs");
    loads
}

#[udpstp_req::req("RFC9946-7.2.2-5", "RFC9946-8.1-5", "RFC9946-8.1-L6")]
#[test]
fn a_fixed_row_is_sent_as_load_pdus() {
    let outcome = downstream(1, 5).run_for(RUN);

    let loads = loads(&outcome);
    for (index, load) in loads.iter().enumerate() {
        let b = &load.payload;
        outcome.check(load.len == 597, "row 5 sends 597-byte UDP payloads");
        outcome.check(
            usize::from(udp_payload(b)) == load.len,
            "udpPayload is the datagram's UDP payload size",
        );
        outcome.check(
            seq_no(b) == index as u32 + 1,
            "lpduSeqNo counts from 1 without gaps",
        );
        outcome.check(
            test_action(b) == 0 && rx_stopped(b) == 0,
            "TEST_ACT_TEST, rxStopped clear",
        );
    }
    for pair in loads.windows(2) {
        outcome.check(pair[1].at - pair[0].at == MS, "one Burst every 1 ms");
    }
    let first = loads[0].at - activated_at(&outcome);
    outcome.check(
        (5 * MS..=50 * MS).contains(&first),
        &format!("the first Burst leaves 5-50 ms after activation, not {first:?}"),
    );
}

// srStruct: two transmitters with their own interval, and one Add-on at
// the end of every txInterval2.
#[udpstp_req::req("RFC9946-7.1-L9", "RFC9946-7.2.2-5")]
#[test]
fn both_transmitters_and_the_add_on_are_sent() {
    let outcome = downstream(2, 123).run_for(RUN);

    let loads = loads(&outcome);
    let add_ons: Vec<&&TraceEntry> = loads.iter().filter(|t| t.len == 347).collect();
    outcome.check(add_ons.len() > 10, "Add-ons were sent");
    for pair in add_ons.windows(2) {
        outcome.check(
            pair[1].at - pair[0].at == MS,
            "one Add-on every txInterval2 (1 ms)",
        );
    }
    for add_on in &add_ons {
        let index = loads
            .iter()
            .position(|t| std::ptr::eq(*t, **add_on))
            .unwrap_or(0);
        outcome.check(
            index >= 2
                && loads[index - 2..index]
                    .iter()
                    .all(|t| t.len == 1222 && t.at == add_on.at),
            "each Add-on ends a Burst of two 1222-byte datagrams",
        );
    }
    // Each Transmitter starts at its own offset of up to 50 ms, so count
    // over the last 10 ms.
    let last = add_ons.len() - 1;
    let window = add_ons[last - 10].at..add_ons[last].at;
    let full_size = loads
        .iter()
        .filter(|t| window.contains(&t.at) && t.len == 1222)
        .count();
    outcome.check(
        full_size == 10 * (10 + 2),
        &format!("each 1 ms carries 10 datagrams of transmitter 1 and 2 of transmitter 2, not {full_size} in 10 ms"),
    );
}

// docs/spec.md Time model (lpduTime is the Test Time Base plus a wall offset
// fixed at the start, one value per Burst)
#[udpstp_req::req("RFC9946-8.1-L10")]
#[test]
fn lpdu_time_is_the_send_time_and_ignores_wall_clock_steps() {
    let step_at = 100 * MS;
    // Row 123's Bursts on transmitter 2 hold three datagrams.
    let outcome = downstream(3, 123)
        .server_wall_step(step_at, 3600)
        .run_for(RUN);

    let wall_start = Duration::from_secs(harness::WALL_START_UNIX_SECS);
    let loads = loads(&outcome);
    outcome.check(
        loads.iter().any(|t| t.at > step_at),
        "Load PDUs were sent after the step",
    );
    for load in loads {
        outcome.check(
            lpdu_time(&load.payload) == wall_start + load.at,
            &format!(
                "lpduTime at {:?} is the send time, not {:?}",
                load.at,
                lpdu_time(&load.payload)
            ),
        );
    }
}

// RFC 9946 §7.1 (CHTA_RAND_PAYLOAD clear by default)
#[udpstp_req::req("RFC9946-8.1-L15")]
#[test]
fn payload_content_is_zeros_by_default() {
    let outcome = downstream(4, 5).keep_load_content().run_for(RUN);

    // The simulator's send buffer starts filled with 0xA5, so zeros show
    // the core wrote them.
    for load in loads(&outcome) {
        outcome.check(
            content(&load.payload).iter().all(|&b| b == 0),
            "the payload content is all zeros",
        );
    }
}

// RFC 9946 §7.1 (CHTA_RAND_PAYLOAD: randomize the payload content beyond
// the Load PDU header)
#[udpstp_req::req("RFC9946-8.1-L15", "RFC9097-4-3", "RFC9946-12.3.7-L1")]
#[test]
fn payload_content_is_random_when_asked_for() {
    let outcome = downstream(5, 123)
        .client_config(|c| c.random_payload = true)
        .keep_load_content()
        .run_for(RUN);

    let loads = loads(&outcome);
    let contents: Vec<&[u8]> = loads.iter().map(|t| content(&t.payload)).collect();
    for c in &contents {
        let zeros = c.iter().filter(|&&b| b == 0).count();
        outcome.check(
            zeros * 32 < c.len(),
            "random content is mostly non-zero bytes",
        );
    }
    let mut firsts: Vec<&[u8]> = contents.iter().map(|c| &c[..64]).collect();
    firsts.sort();
    firsts.dedup();
    outcome.check(
        firsts.len() == contents.len(),
        "no two datagrams start with the same content",
    );
    let blocks: Vec<&[u8; 64]> = contents
        .iter()
        .flat_map(|c| c.as_chunks::<64>().0)
        .collect();
    let mut unique = blocks.clone();
    unique.sort();
    unique.dedup();
    outcome.check(
        unique.len() == blocks.len(),
        "no 64-byte block repeats anywhere, so a compressor finds nothing",
    );
}

// ADR 0008, as amended by ADR 0012 (a Transmitter whose interval is the
// catch-up budget or longer sends only its most recent missed Burst;
// older ones are Skipped Bursts, counted and reported)
#[test]
fn a_stalled_engine_skips_missed_bursts_and_reports_them() {
    let stall_at = 100 * MS;
    let stall = Duration::from_micros(5_500);
    let outcome = downstream(6, 5)
        .server_engine_stall(stall_at, stall)
        .run_for(RUN);

    let loads = loads(&outcome);
    let resume = stall_at + stall;
    outcome.check(
        !loads.iter().any(|t| t.at >= stall_at && t.at < resume),
        "nothing is sent during the stall",
    );
    // The grid: the first Burst's time plus whole milliseconds.
    let first = loads[0].at;
    let due_in_stall: Vec<Duration> = (0..200)
        .map(|k| first + k * MS)
        .filter(|t| *t >= stall_at && *t <= resume)
        .collect();
    outcome.check(
        due_in_stall.len() > 1,
        "several Bursts fell due during the stall",
    );
    let skipped = due_in_stall.len().saturating_sub(1) as u32;
    let late = loads.iter().find(|t| t.at >= resume);
    outcome.check(
        late.is_some_and(|t| t.at == resume),
        "the most recent missed Burst goes out when the engine resumes",
    );
    let after: Vec<&&TraceEntry> = loads.iter().filter(|t| t.at > resume).collect();
    outcome.check(
        after
            .first()
            .is_some_and(|t| t.at == *due_in_stall.last().unwrap_or(&resume) + MS),
        "then the grid continues",
    );
    for (index, load) in loads.iter().enumerate() {
        outcome.check(
            seq_no(&load.payload) == index as u32 + 1,
            "Skipped Bursts use no sequence numbers",
        );
    }
    outcome.check(
        outcome.server_data_events.iter().any(|(_, e)| {
            *e == DataEvent::BurstsSkipped {
                bursts: skipped,
                datagrams: skipped,
                bytes: u64::from(skipped) * 597,
            }
        }),
        &format!(
            "{skipped} Skipped Bursts reported, events: {:?}",
            outcome.server_data_events
        ),
    );
}

/// The catch-up budget the Data Phase Engine uses (ADR 0012).
const BUDGET: Duration = MS;

/// What a Server stalled at 100 ms for `stall` does at `row`, whose only
/// Transmitter sends one Burst every `interval`: the Bursts it sends the
/// moment it resumes, the Skipped Bursts it reports, and what ADR 0012
/// expects of both, from the grid of the first Burst's time plus whole
/// intervals.
struct CatchUp {
    at_resume: usize,
    skipped: u32,
    expected_sent: usize,
    expected_skipped: u32,
    outcome: Outcome,
}

fn catch_up_after(seed: u64, row: u16, interval: Duration, stall: Duration) -> CatchUp {
    let stall_at = 100 * MS;
    let outcome = downstream(seed, row)
        .server_engine_stall(stall_at, stall)
        .run_for(RUN);
    let resume = stall_at + stall;
    let loads = loads(&outcome);
    let at_resume = loads.iter().filter(|t| t.at == resume).count();
    let first = loads[0].at;
    let due: Vec<Duration> = (0..)
        .map(|k| first + interval * k)
        .skip_while(|t| *t < stall_at)
        .take_while(|t| *t <= resume)
        .collect();
    let expected_sent = due.iter().filter(|d| resume - **d < BUDGET).count();
    let skipped = outcome
        .server_data_events
        .iter()
        .map(|(_, e)| match e {
            DataEvent::BurstsSkipped { bursts, .. } => *bursts,
            _ => 0,
        })
        .sum();
    CatchUp {
        at_resume,
        skipped,
        expected_sent,
        expected_skipped: (due.len() - expected_sent) as u32,
        outcome,
    }
}

impl CatchUp {
    fn check(&self, sent: usize, skipped: u32) {
        let outcome = &self.outcome;
        outcome.check(
            self.expected_sent == sent && self.expected_skipped == skipped,
            &format!(
                "the grid gives {} to send and {} to skip",
                self.expected_sent, self.expected_skipped
            ),
        );
        outcome.check(
            self.at_resume == sent,
            &format!("{sent} Bursts go out on waking, not {}", self.at_resume),
        );
        outcome.check(
            self.skipped == skipped,
            &format!("{skipped} Bursts are skipped, not {}", self.skipped),
        );
        for (index, load) in outcome.load_pdus_to_client().iter().enumerate() {
            outcome.check(
                seq_no(&load.payload) == index as u32 + 1,
                "Skipped Bursts use no sequence numbers",
            );
        }
    }
}

/// A stall ending exactly `lateness` after the first Burst of row
/// `row` that falls due in it, found from a run with the same seed.
fn stall_until(seed: u64, row: u16, interval: Duration, lateness: Duration) -> Duration {
    let stall_at = 100 * MS;
    let outcome = downstream(seed, row).run_for(RUN);
    let first = loads(&outcome)[0].at;
    let due = (0..)
        .map(|k| first + interval * k)
        .find(|t| *t >= stall_at)
        .unwrap_or(stall_at);
    due + lateness - stall_at
}

const TX1: Duration = Duration::from_micros(100);

//  (catch-up by time budget; ADR 0012): a Burst missed by less than
// 1 ms is still sent when the engine wakes, so a timer that wakes a few
// hundred microseconds late, as Windows' does, costs nothing.
#[test]
fn bursts_missed_by_less_than_the_catch_up_budget_are_all_sent() {
    let catch_up = catch_up_after(7, 100, TX1, Duration::from_micros(450));
    // 4 or 5 Bursts fall due in 450 µs, by the grid's phase.
    let sent = catch_up.expected_sent;
    catch_up
        .outcome
        .check((4..=5).contains(&sent), "several Bursts fell due");
    catch_up.check(sent, 0);
}

// : Bursts missed by the budget or more are skipped, and the ones
// less late still go out: each Transmitter sends less than 1 ms of its
// schedule at once, so a long stall can't flood a Trial Interval.
#[test]
fn bursts_missed_by_more_than_the_catch_up_budget_are_skipped() {
    let catch_up = catch_up_after(8, 100, TX1, Duration::from_micros(3_050));
    catch_up.check(10, catch_up.expected_skipped);
    catch_up
        .outcome
        .check(catch_up.expected_skipped >= 20, "about 20 are older");
}

// : the budget is exclusive. A Burst exactly 1 ms late is skipped,
// one a nanosecond less late is sent.
#[test]
fn a_burst_exactly_the_catch_up_budget_late_is_skipped() {
    let stall = stall_until(9, 100, TX1, BUDGET);
    catch_up_after(9, 100, TX1, stall).check(10, 1);
    let stall = stall_until(9, 100, TX1, BUDGET - Duration::from_nanos(1));
    catch_up_after(9, 100, TX1, stall).check(10, 0);
}

// : with an interval of 1 ms, the budget changes nothing: only the
// most recent missed Burst goes, as under ADR 0008.
#[test]
fn a_one_millisecond_transmitter_catches_up_as_before() {
    let stall = stall_until(11, 5, MS, BUDGET);
    catch_up_after(11, 5, MS, stall).check(1, 1);
}

// ---- The Client as Load Receiver, against a scripted Server ----

/// A scripted Server that accepts the Setup with testPort = its standard
/// port and answers the Activation Request with an acceptance followed by
/// the given datagrams.
fn scripted_load(seed: u64, datagrams: Vec<Vec<u8>>) -> Outcome {
    Scenario::new(seed)
        .client_unauthenticated()
        .client_direction(Direction::Downstream)
        .scripted_server_replying_many(move |_, request| {
            if request.len() == 56 {
                let mut pdu = request.to_vec();
                pdu[8] = 2; // CHSR_CREQ_SETUPRSP
                pdu[9] = 1; // CHSR_CRSP_ACKOK
                pdu[12..14].copy_from_slice(&24601u16.to_be_bytes());
                return vec![pdu];
            }
            if request.len() == 104 && request.starts_with(&[0xAC, 0xE2]) {
                let mut ack = request.to_vec();
                ack[5] = 1; // CHTA_CRSP_ACKOK
                let mut out = vec![ack];
                out.extend(datagrams.iter().cloned());
                return out;
            }
            Vec::new()
        })
        .run_for(RUN)
}

/// A valid Load PDU of `len` bytes (RFC 9946 Figure 8).
fn load_pdu(seq: u32, len: usize) -> Vec<u8> {
    let mut b = vec![0u8; len];
    b[0..2].copy_from_slice(&[0xBE, 0xEF]);
    b[4..8].copy_from_slice(&seq.to_be_bytes());
    b[8..10].copy_from_slice(&(len as u16).to_be_bytes());
    b
}

fn invalid_reported(outcome: &Outcome) -> u32 {
    outcome
        .client_data_events
        .iter()
        .map(|(_, e)| match e {
            DataEvent::InvalidLoadPdus { count } => *count,
            _ => 0,
        })
        .sum()
}

#[udpstp_req::req("RFC9946-8.1-2", "RFC9946-8.1-3")]
#[test]
fn the_client_ignores_invalid_load_pdus() {
    let short = load_pdu(1, 31);
    let mut wrong_id = load_pdu(2, 100);
    wrong_id[0..2].copy_from_slice(&[0xFE, 0xEF]);
    let outcome = scripted_load(10, vec![short, wrong_id]);

    outcome.check(
        outcome.client_activated_direction(0) == Some(Direction::Downstream),
        "the Client prepared the Downstream Test",
    );
    outcome.check(
        invalid_reported(&outcome) == 2,
        "two invalid Load PDUs reported",
    );
    outcome.check(
        !outcome
            .client_data_events
            .iter()
            .any(|(_, e)| *e == DataEvent::FirstLoadPduReceived),
        "no Load PDU counted as received",
    );
}

// A valid Load PDU is at least a header long.
#[udpstp_req::req("RFC9946-8.1-2", "RFC9946-8.1-3")]
#[test]
fn the_client_receives_a_valid_load_pdu_after_invalid_ones() {
    let outcome = scripted_load(11, vec![load_pdu(1, 31), load_pdu(1, 32), load_pdu(2, 597)]);

    let events: Vec<DataEvent> = outcome.client_data_events.iter().map(|(_, e)| *e).collect();
    outcome.check(
        invalid_reported(&outcome) == 1,
        "one invalid Load PDU reported",
    );
    outcome.check(
        events
            .iter()
            .filter(|e| **e == DataEvent::FirstLoadPduReceived)
            .count()
            == 1,
        &format!("the first valid Load PDU was reported once: {events:?}"),
    );
}

// ---- Checksum (RFC 9946 §5.6) ----

/// The Internet checksum of a Load PDU header: RFC 1071, computed here from
/// the RFC text rather than with our codec.
fn header_sum(header: &[u8]) -> u16 {
    let mut sum: u32 = header[..32]
        .chunks(2)
        .map(|w| u32::from(u16::from_be_bytes([w[0], w[1]])))
        .sum();
    while sum > 0xFFFF {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    sum as u16
}

#[udpstp_req::req("RFC9946-5.6-3", "RFC9946-8.1-L13", "RFC9946-8.1-L14", "RFC9946-5.6-7")]
#[test]
fn the_server_fills_the_header_checksum_when_configured() {
    let outcome = downstream(20, 123).checksum().run_for(RUN);

    for load in loads(&outcome) {
        outcome.check(
            u16_at(&load.payload, 30) != 0 && header_sum(&load.payload) == 0xFFFF,
            "every Load PDU header sums to 0xFFFF with its checksum",
        );
    }
}

/// `pdu` with its header checksum filled (RFC 1071).
fn with_checksum(mut pdu: Vec<u8>) -> Vec<u8> {
    pdu[30..32].fill(0);
    let sum = !header_sum(&pdu);
    pdu[30..32].copy_from_slice(&sum.to_be_bytes());
    pdu
}

fn scripted_load_with_checksum(seed: u64, datagrams: Vec<Vec<u8>>) -> Outcome {
    Scenario::new(seed)
        .client_unauthenticated()
        .client_direction(Direction::Downstream)
        .checksum()
        .scripted_server_replying_many(move |_, request| {
            // The echoed requests' checksums are cleared: an unpopulated
            // field passes.
            if request.len() == 56 {
                let mut pdu = request.to_vec();
                pdu[8] = 2;
                pdu[9] = 1;
                pdu[12..14].copy_from_slice(&24601u16.to_be_bytes());
                pdu[54..56].fill(0);
                return vec![pdu];
            }
            if request.len() == 104 && request.starts_with(&[0xAC, 0xE2]) {
                let mut ack = request.to_vec();
                ack[5] = 1;
                ack[102..104].fill(0);
                let mut out = vec![ack];
                out.extend(datagrams.iter().cloned());
                return out;
            }
            Vec::new()
        })
        .run_for(RUN)
}

#[udpstp_req::req("RFC9946-5.6-4", "RFC9946-5.6-5", "RFC9946-8.1-2")]
#[test]
fn the_client_verifies_a_populated_checksum_when_configured() {
    let mut corrupt = with_checksum(load_pdu(1, 100));
    corrupt[5] ^= 0x01;
    let outcome = scripted_load_with_checksum(21, vec![corrupt]);

    outcome.check(
        invalid_reported(&outcome) == 1,
        "the corrupted header is ignored",
    );

    let unpopulated = load_pdu(1, 100);
    let outcome =
        scripted_load_with_checksum(22, vec![unpopulated, with_checksum(load_pdu(2, 100))]);
    outcome.check(
        invalid_reported(&outcome) == 0,
        "an unpopulated field and a correct checksum are both accepted",
    );
    outcome.check(
        outcome
            .client_data_events
            .iter()
            .any(|(_, e)| *e == DataEvent::FirstLoadPduReceived),
        "the Load PDUs were received",
    );
}
