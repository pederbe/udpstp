//! Stale Status PDUs and spduSeqErr (RFC 9946 §8.1, §8.2; docs/spec.md "Data
//! Phase": one stale-PDU rule in both roles).
//!
//! A Status PDU whose spduSeqNo isn't greater than the last one processed is
//! stale: it isn't fed to the Load Rate Adjustment Algorithm, can't change
//! the Sending Rate Structure or deliver STOP, and isn't echoed for RTT, but
//! it still resets the Watchdog and is counted. The Load Sender reports
//! spduSeqErr: the Status PDUs missing before the latest one processed, as
//! OB-UDPST counts them.

mod harness;

use std::time::Duration;

use harness::status_script::{DEFAULTS, Report, clean, errored, reported};
use harness::{Scenario, TraceEntry};
use udpstp_core::{DataEvent, Direction, EndReason, SocketId};

fn stale_count(events: &[(SocketId, DataEvent)]) -> u32 {
    events
        .iter()
        .map(|(_, e)| match e {
            DataEvent::StaleStatusPdus { count } => *count,
            _ => 0,
        })
        .sum()
}

fn numbered(seq: u32, report: Report) -> Report {
    Report {
        spdu_seq_no: Some(seq),
        ..report
    }
}

// docs/spec.md "Data Phase" (a Status PDU whose spduSeqNo isn't greater
// than the last one processed isn't fed to the algorithm, and is counted);
// story 92
#[test]
fn a_stale_status_pdu_does_not_steer_the_server() {
    // Numbers 0 (never valid: they start at 1), 1, 2, 2 (a duplicate), 1
    // (a replay), 3, 5.
    let (outcome, got) = reported(
        Scenario::new(1),
        DEFAULTS,
        vec![
            numbered(0, clean()),
            numbered(1, clean()),
            numbered(2, clean()),
            numbered(2, clean()),
            numbered(1, clean()),
            numbered(3, clean()),
            numbered(5, clean()),
        ],
    );
    let classified: Vec<(u32, Option<u16>)> = got.iter().map(|c| (c.spdu_seq_no, c.row)).collect();
    outcome.check(
        classified == [(1, Some(10)), (2, Some(20)), (3, Some(30)), (5, Some(40))]
            && stale_count(&outcome.server_data_events) == 3,
        &format!("{classified:?}, events {:?}", outcome.server_data_events),
    );
}

// spduSeqErr is the Status PDU loss count as seen by the Load Sender; spec
//   (OB-UDPST's gap before the latest Status PDU)
#[udpstp_req::req("RFC9946-8.1-L8", "RFC9097-8.3-3")]
#[test]
fn spdu_seq_err_reports_the_status_pdus_missing_before_the_latest() {
    let (outcome, _) = reported(
        Scenario::new(2),
        DEFAULTS,
        vec![
            numbered(1, errored()),
            numbered(4, errored()),
            numbered(4, errored()),
            numbered(5, errored()),
        ],
    );
    // spduSeqErr is Load PDU bytes 10-11 (Figure 9). With Errored Intervals
    // the row stays at 0: one Load PDU per 50 ms, each answered.
    let reported: Vec<u16> = outcome
        .trace
        .iter()
        .filter(|t: &&TraceEntry| t.payload.starts_with(&[0xBE, 0xEF]))
        .filter(|t| t.destination == harness::CLIENT_ADDR.parse().unwrap())
        .map(|t| u16::from_be_bytes([t.payload[10], t.payload[11]]))
        .collect();
    // Before any Status PDU: 0. After 1: 0. After 4: 2 missing. A stale 4
    // changes nothing. After 5: 0.
    let mut distinct = reported.clone();
    distinct.dedup();
    outcome.check(
        distinct == [0, 2, 0],
        &format!("spduSeqErr in the Server's Load PDUs: {reported:?}"),
    );
}

// docs/spec.md "Data Phase" (a stale Status PDU cannot deliver STOP);
// story 92
#[test]
fn a_stale_stop_does_not_end_the_test() {
    let stop = |seq| Report {
        stop: true,
        ..numbered(seq, clean())
    };
    let (outcome, _) = reported(
        Scenario::new(3),
        DEFAULTS,
        vec![
            numbered(1, errored()),
            numbered(2, errored()),
            stop(2),
            stop(1),
        ],
    );
    let ended: Vec<&DataEvent> = outcome
        .server_data_events
        .iter()
        .map(|(_, e)| e)
        .filter(|e| matches!(e, DataEvent::Ended(_)))
        .collect();
    outcome.check(ended.is_empty(), &format!("{ended:?}"));

    let (outcome, _) = reported(
        Scenario::new(4),
        DEFAULTS,
        vec![numbered(1, errored()), stop(2)],
    );
    outcome.check(
        outcome
            .server_data_events
            .iter()
            .any(|(_, e)| matches!(e, DataEvent::Ended(EndReason::StoppedByClient))),
        &format!("a fresh STOP ends it: {:?}", outcome.server_data_events),
    );
}

// docs/spec.md "Data Phase" (a stale Status PDU still resets the Watchdog); ADR
// 0002
#[udpstp_req::req("RFC9946-5.1-15", "RFC9946-8.2-4")]
#[test]
fn a_stale_status_pdu_still_counts_as_heard() {
    // After one fresh Status PDU, 40 stale ones, one per 50 ms: 2 s with no
    // fresh one, twice the Status PDU timeout.
    let mut reports = vec![numbered(1, errored())];
    reports.extend(vec![numbered(1, errored()); 40]);
    let (outcome, _) = reported(Scenario::new(5), DEFAULTS, reports);
    outcome.check(
        !outcome
            .server_data_events
            .iter()
            .any(|(_, e)| matches!(e, DataEvent::TrafficStopped { .. })),
        &format!("{:?}", outcome.server_data_events),
    );
}

/// A scripted Server running an Upstream Test that answers the Client's
/// first Load PDUs with these Status PDUs: (spduSeqNo, testAction, the
/// payload size its srStruct asks for).
fn server_sending(seed: u64, statuses: Vec<(u32, u8, u32)>) -> harness::Outcome {
    let mut statuses = statuses.into_iter();
    Scenario::new(seed)
        .client_unauthenticated()
        .client_direction(Direction::Upstream)
        .scripted_server_replying_many(move |_, request| {
            if request.len() == 56 {
                let mut pdu = request.to_vec();
                pdu[8] = 2;
                pdu[9] = 1;
                pdu[12..14].copy_from_slice(&24601u16.to_be_bytes());
                return vec![pdu];
            }
            let status = |seq: u32, action: u8, size: u32| {
                let mut b = vec![0u8; 204];
                b[0..2].copy_from_slice(&[0xFE, 0xED]);
                b[2] = action;
                b[4..8].copy_from_slice(&seq.to_be_bytes());
                // srStruct (Figure 7): the second transmitter, one datagram
                // of `size` every 10 ms.
                b[20..24].copy_from_slice(&10_000u32.to_be_bytes());
                b[24..28].copy_from_slice(&size.to_be_bytes());
                b[28..32].copy_from_slice(&1u32.to_be_bytes());
                b
            };
            if request.len() == 104 && request.starts_with(&[0xAC, 0xE2]) {
                let mut ack = request.to_vec();
                ack[5] = 1;
                ack[28..56].copy_from_slice(&status(0, 0, 500)[8..36]);
                return vec![ack];
            }
            if request.starts_with(&[0xBE, 0xEF]) {
                return statuses
                    .next()
                    .map(|(seq, action, size)| vec![status(seq, action, size)])
                    .unwrap_or_default();
            }
            Vec::new()
        })
        .run_for(Duration::from_millis(200))
}

// docs/spec.md "Data Phase" (the same rule for the Client as Load Sender:
// a stale Status PDU can't change its Sending Rate Structure or deliver
// STOP2); story 92
#[test]
fn a_stale_status_pdu_does_not_steer_the_client() {
    const STOP2: u8 = 2;
    let outcome = server_sending(
        6,
        vec![(1, 0, 600), (1, 0, 700), (1, STOP2, 800), (2, 0, 900)],
    );
    let sizes: Vec<usize> = outcome
        .trace
        .iter()
        .filter(|t| t.source == harness::CLIENT_ADDR.parse().unwrap())
        .filter(|t| t.payload.starts_with(&[0xBE, 0xEF]))
        .map(|t| t.len)
        .collect();
    let mut distinct = sizes.clone();
    distinct.dedup();
    outcome.check(
        distinct == [500, 600, 900] && stale_count(&outcome.client_data_events) == 2,
        &format!("sizes {sizes:?}, events {:?}", outcome.client_data_events),
    );
}
