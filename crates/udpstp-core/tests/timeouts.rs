//! A broken path (RFC 9946 §5.1 items 7-9, §8.1 rxStopped, §9; ADR 0002):
//! after 1 s without receiving, an end stops sending; rxStopped is
//! asserted from half that; the Watchdog ends the Test Connection at 3 s.
//!
//! The network drops Load or Status PDUs from a point in time. Expected
//! times are worked out from the trace: the last PDU an end received, plus
//! the ADR's 1 s and 3 s and this implementation's 0.5 s for rxStopped.

mod harness;

use std::net::SocketAddr;
use std::time::Duration;

use harness::{DataEvent, Outcome, RowRequest, Scenario, TraceEntry};
use udpstp_core::{Direction, EndReason, Event};

const LOAD: [u8; 2] = [0xBE, 0xEF];
const STATUS: [u8; 2] = [0xFE, 0xED];
const ONE_S: Duration = Duration::from_secs(1);
const HALF_S: Duration = Duration::from_millis(500);

fn client() -> SocketAddr {
    harness::CLIENT_ADDR.parse().unwrap()
}

fn test(seed: u64, direction: Direction) -> Scenario {
    Scenario::new(seed)
        // A Fixed-Rate Test needs an Operator Key (RFC 9946 §4.1).
        .operator(7, b"an operator key")
        .client_direction(direction)
        .client_requests_row(RowRequest::FixedRate(35))
}

/// PDUs with `pdu_id` sent to or from the Client.
fn pdus(outcome: &Outcome, pdu_id: [u8; 2], to_client: bool) -> Vec<&TraceEntry> {
    outcome
        .trace
        .iter()
        .filter(|t| t.payload.starts_with(&pdu_id))
        .filter(|t| (t.destination == client()) == to_client)
        .collect()
}

fn last_delivered(pdus: &[&TraceEntry]) -> Duration {
    pdus.iter()
        .filter(|t| t.delivered)
        .map(|t| t.at)
        .max()
        .unwrap_or_default()
}

fn events(events: &[(udpstp_core::SocketId, DataEvent)]) -> Vec<DataEvent> {
    events
        .iter()
        .map(|(_, e)| match e {
            // When the silence began is checked on its own.
            DataEvent::TrafficStopped { .. } => stopped(),
            e => *e,
        })
        .filter(|e| matches!(e, DataEvent::TrafficStopped { .. } | DataEvent::Ended(_)))
        .collect()
}

/// A `TrafficStopped` with its time left out.
fn stopped() -> DataEvent {
    DataEvent::TrafficStopped {
        silent_since: udpstp_core::MonoTime::from_duration(Duration::ZERO),
    }
}

/// Checks an end that stopped receiving at `heard`: what it sent with
/// `pdu_id` carries rxStopped from `heard` + 0.5 s and stops by `heard` +
/// 1 s.
fn check_sender(outcome: &Outcome, sent: &[&TraceEntry], heard: Duration, what: &str) {
    outcome.check(
        sent.iter()
            .all(|t| t.at <= heard + ONE_S && (t.payload[3] == 1) == (t.at >= heard + HALF_S)),
        &format!("{what}: rxStopped from 0.5 s after {heard:?}, nothing from 1 s"),
    );
    outcome.check(
        sent.iter().any(|t| t.payload[3] == 1),
        &format!("{what}: some carry rxStopped"),
    );
}

// ADR 0002 (1 s, then the Watchdog at 3 s); docs/spec.md stories 21, 22
#[udpstp_req::req(
    "RFC9946-5.1-11",
    "RFC9946-5.1-13",
    "RFC9946-5.1-14",
    "RFC9946-5.1-16",
    "RFC9946-5.1-17",
    "RFC9946-8.1-L4",
    "RFC9946-8.1-L5",
    "RFC9946-9-9",
    "RFC9097-8.1-8",
    "RFC9097-8.1-10"
)]
#[test]
fn a_downstream_path_that_breaks_stops_within_1_s_and_ends_at_3_s() {
    let outcome = test(1, Direction::Downstream)
        .network_drops(LOAD, Duration::from_millis(500), None)
        .run_for(Duration::from_secs(6));

    // The Client, as Load Receiver, stops its Status PDUs.
    let loads = pdus(&outcome, LOAD, true);
    let statuses = pdus(&outcome, STATUS, false);
    check_sender(&outcome, &statuses, last_delivered(&loads), "the Client");
    outcome.check(
        events(&outcome.client_data_events) == [stopped(), DataEvent::Ended(EndReason::Watchdog)],
        &format!("the Client: {:?}", outcome.client_data_events),
    );

    // The Server, as Load Sender, then stops its Load PDUs.
    check_sender(&outcome, &loads, last_delivered(&statuses), "the Server");
    outcome.check(
        events(&outcome.server_data_events) == [stopped(), DataEvent::Ended(EndReason::Watchdog)],
        &format!("the Server: {:?}", outcome.server_data_events),
    );
    outcome.check(
        outcome
            .server_events
            .iter()
            .any(|e| matches!(e, Event::TestConnectionReleased { .. })),
        "the Server releases the Test Connection",
    );
}

// ADR 0002
#[udpstp_req::req(
    "RFC9946-5.1-11",
    "RFC9946-5.1-13",
    "RFC9946-5.1-16",
    "RFC9946-5.1-17",
    "RFC9946-8.1-L4",
    "RFC9946-9-9",
    "RFC9097-8.1-8",
    "RFC9097-8.1-12"
)]
#[test]
fn an_upstream_client_stops_when_status_pdus_stop() {
    let outcome = test(2, Direction::Upstream)
        .network_drops(STATUS, Duration::from_millis(500), None)
        .run_for(Duration::from_secs(6));

    let statuses = pdus(&outcome, STATUS, true);
    let loads = pdus(&outcome, LOAD, false);
    check_sender(&outcome, &loads, last_delivered(&statuses), "the Client");
    outcome.check(
        events(&outcome.client_data_events) == [stopped(), DataEvent::Ended(EndReason::Watchdog)],
        &format!("the Client: {:?}", outcome.client_data_events),
    );
    // The Server, as Load Receiver, then stops its Status PDUs and ends.
    check_sender(&outcome, &statuses, last_delivered(&loads), "the Server");
    outcome.check(
        events(&outcome.server_data_events) == [stopped(), DataEvent::Ended(EndReason::Watchdog)],
        &format!("the Server: {:?}", outcome.server_data_events),
    );
    outcome.check(
        outcome
            .server_events
            .iter()
            .any(|e| matches!(e, Event::TestConnectionReleased { .. })),
        "the Server releases the Test Connection",
    );
}

// ADR 0002 (traffic that resumes between 1 s and 3 s does not restart
// sending; the Test Connection is already ending)
// (conformance: receive-timeout-is-final)
#[udpstp_req::req("RFC9097-8.1-10", "RFC9946-8.1-4")]
#[test]
fn traffic_that_comes_back_after_the_timeout_changes_nothing() {
    let outcome = test(3, Direction::Downstream)
        .network_drops(
            LOAD,
            Duration::from_millis(500),
            Some(Duration::from_millis(1700)),
        )
        .run_for(Duration::from_secs(6));

    let loads = pdus(&outcome, LOAD, true);
    let resumed = loads
        .iter()
        .any(|t| t.delivered && t.at >= Duration::from_millis(1700));
    outcome.check(resumed, "Load PDUs arrive again from 1.7 s");
    let statuses = pdus(&outcome, STATUS, false);
    outcome.check(
        statuses.iter().all(|t| t.at < Duration::from_millis(1700)),
        "the Client sends no Status PDU again",
    );
    // The Watchdog still ends it 3 s after the last Load PDU before the
    // break: the ones that came back don't reset it.
    let heard = loads
        .iter()
        .filter(|t| t.delivered && t.at < Duration::from_millis(1700))
        .map(|t| t.at)
        .max()
        .unwrap_or_default();
    let at_watchdog = test(3, Direction::Downstream)
        .network_drops(
            LOAD,
            Duration::from_millis(500),
            Some(Duration::from_millis(1700)),
        )
        .run_for(heard + Duration::from_secs(3));
    at_watchdog.check(
        events(&at_watchdog.client_data_events)
            == [stopped(), DataEvent::Ended(EndReason::Watchdog)],
        &format!(
            "and its Watchdog ends it at {:?}: {:?}",
            heard + Duration::from_secs(3),
            at_watchdog.client_data_events
        ),
    );
}

// rxStopped is 0 while traffic flows.
#[udpstp_req::req("RFC9946-8.1-L4")]
#[test]
fn rx_stopped_is_clear_while_traffic_flows() {
    let outcome = test(4, Direction::Downstream).run_for(Duration::from_secs(2));

    let all: Vec<&TraceEntry> = outcome
        .trace
        .iter()
        .filter(|t| t.payload.starts_with(&LOAD) || t.payload.starts_with(&STATUS))
        .collect();
    outcome.check(
        all.len() > 100 && all.iter().all(|t| t.payload[3] == 0),
        "no Load or Status PDU carries rxStopped",
    );
}

// ADR 0002 (the Watchdog ends the Test Connection 3 s after the last
// PDU received)
#[test]
fn the_watchdog_ends_the_test_connection_3_s_after_the_last_pdu() {
    let broken = || test(5, Direction::Downstream).network_drops(LOAD, HALF_S, None);
    let heard = last_delivered(&pdus(&broken().run_for(Duration::from_secs(6)), LOAD, true));

    let before = broken().run_for(heard + Duration::from_millis(2999));
    let at = broken().run_for(heard + Duration::from_secs(3));
    let ended = |outcome: &Outcome| {
        outcome
            .client_data_events
            .iter()
            .any(|(_, e)| *e == DataEvent::Ended(EndReason::Watchdog))
    };
    before.check(!ended(&before), "not yet 1 ms before 3 s");
    at.check(ended(&at), "at 3 s");
}

#[udpstp_req::req("RFC9946-8.1-12", "RFC9946-5.1-12", "RFC9097-8.1-9")]
#[test]
fn rx_stopped_clears_when_traffic_comes_back_in_time() {
    let outcome = test(6, Direction::Downstream)
        .network_drops(LOAD, HALF_S, Some(Duration::from_millis(1200)))
        .run_for(Duration::from_secs(2));

    let statuses = pdus(&outcome, STATUS, false);
    let during: Vec<u8> = statuses
        .iter()
        .filter(|t| t.at > Duration::from_millis(1050) && t.at < Duration::from_millis(1200))
        .map(|t| t.payload[3])
        .collect();
    let after: Vec<u8> = statuses
        .iter()
        .filter(|t| t.at > Duration::from_millis(1300))
        .map(|t| t.payload[3])
        .collect();
    outcome.check(
        !during.is_empty() && during.iter().all(|&r| r == 1),
        &format!("set while the Load PDUs are missing: {during:?}"),
    );
    outcome.check(
        !after.is_empty() && after.iter().all(|&r| r == 0),
        &format!("clear once they come back: {after:?}"),
    );
    outcome.check(
        events(&outcome.client_data_events).is_empty(),
        "and the timeout never expired",
    );
    // : the Server counts the Status PDUs that said so.
    let reported: u32 = outcome
        .server_data_events
        .iter()
        .filter_map(|(_, e)| match e {
            udpstp_core::DataEvent::PeerRxStopped { count } => Some(*count),
            _ => None,
        })
        .sum();
    outcome.check(
        reported as usize == statuses.iter().filter(|t| t.payload[3] == 1).count(),
        &format!("the Server counted {reported}"),
    );
}

// The mirror of `rx_stopped_clears_when_traffic_comes_back_in_time`: each
// Status PDU resets the Status PDU timeout, so a gap shorter than 1 s
// leaves the Load Sender sending, in either direction.
#[udpstp_req::req("RFC9097-8.1-11", "RFC9946-8.1-12")]
#[test]
fn the_load_sender_keeps_sending_when_status_pdus_come_back_in_time() {
    for (seed, direction) in [(8, Direction::Upstream), (9, Direction::Downstream)] {
        let outcome = test(seed, direction)
            .network_drops(STATUS, HALF_S, Some(Duration::from_millis(1200)))
            .run_for(Duration::from_secs(2));

        // The Load Sender is the Client in an Upstream Test.
        let upstream = direction == Direction::Upstream;
        let statuses = pdus(&outcome, STATUS, upstream);
        let heard = statuses
            .iter()
            .filter(|t| t.delivered && t.at < HALF_S)
            .map(|t| t.at)
            .max()
            .unwrap_or_default();
        let back = statuses
            .iter()
            .filter(|t| t.delivered && t.at >= HALF_S)
            .map(|t| t.at)
            .min();
        outcome.check(
            back.is_some_and(|at| at - heard < ONE_S),
            &format!("{direction:?}: a gap under 1 s, from {heard:?} to {back:?}"),
        );
        let loads = pdus(&outcome, LOAD, !upstream);
        let rx_stopped = |from: u64, to: u64| -> Vec<u8> {
            loads
                .iter()
                .filter(|t| t.at > Duration::from_millis(from) && t.at < Duration::from_millis(to))
                .map(|t| t.payload[3])
                .collect()
        };
        let during = rx_stopped(1050, 1200);
        outcome.check(
            !during.is_empty() && during.iter().all(|&r| r == 1),
            &format!("{direction:?}: rxStopped while the Status PDUs are missing"),
        );
        let after = rx_stopped(1300, 2000);
        outcome.check(
            !after.is_empty() && after.iter().all(|&r| r == 0),
            &format!("{direction:?}: clear once they come back"),
        );
        // Sending carries on past the time 1 s after the last Status PDU
        // before the gap, up to the end of the run.
        outcome.check(
            loads.iter().any(|t| t.at >= heard + ONE_S)
                && loads
                    .last()
                    .is_some_and(|t| t.at >= Duration::from_millis(1990)),
            &format!("{direction:?}: Load PDUs still flow"),
        );
        let sender = if upstream {
            &outcome.client_data_events
        } else {
            &outcome.server_data_events
        };
        outcome.check(
            events(sender).is_empty(),
            &format!("{direction:?}: the timeout never expired: {sender:?}"),
        );
    }
}

// Load PDUs whose header checksum fails are not valid (RFC 9946 §5.6):
// they don't reset the Load PDU timeout or the Watchdog, so the Test
// Connection stops and ends as if they never arrived.
#[udpstp_req::req("RFC9946-8.1-4", "RFC9946-5.1-11")]
#[test]
fn load_pdus_failing_the_checksum_do_not_reset_the_watchdog() {
    // From 0.5 s the network flips a byte of every Load PDU header
    // (spduTime_sec), which the checksum covers.
    let corrupted = || {
        test(10, Direction::Downstream)
            .checksum()
            .network_flips_byte_from(LOAD, 12, HALF_S)
    };
    let outcome = corrupted().run_for(Duration::from_secs(6));

    let loads = pdus(&outcome, LOAD, true);
    let heard = loads
        .iter()
        .filter(|t| t.delivered && t.at < HALF_S)
        .map(|t| t.at)
        .max()
        .unwrap_or_default();
    outcome.check(
        loads
            .iter()
            .any(|t| t.delivered && t.at >= heard + Duration::from_millis(1500)),
        "corrupted Load PDUs keep arriving after the timeout",
    );
    let invalid: u32 = outcome
        .client_data_events
        .iter()
        .map(|(_, e)| match e {
            DataEvent::InvalidLoadPdus { count } => *count,
            _ => 0,
        })
        .sum();
    outcome.check(invalid > 0, "the Client found them invalid");
    // The Client's Status PDUs stop 1 s after the last valid Load PDU.
    let statuses = pdus(&outcome, STATUS, false);
    check_sender(&outcome, &statuses, heard, "the Client");

    let ended = |outcome: &Outcome| {
        events(&outcome.client_data_events) == [stopped(), DataEvent::Ended(EndReason::Watchdog)]
    };
    let before = corrupted().run_for(heard + Duration::from_millis(2999));
    let at = corrupted().run_for(heard + Duration::from_secs(3));
    before.check(
        !before
            .client_data_events
            .iter()
            .any(|(_, e)| matches!(e, DataEvent::Ended(_))),
        "not ended 1 ms before 3 s",
    );
    at.check(
        ended(&at),
        &format!(
            "the Watchdog ends it 3 s after the last valid Load PDU: {:?}",
            at.client_data_events
        ),
    );
}

/// A scripted Server that accepts a Downstream Test and sends `loads` with
/// the Activation Response.
fn server_sending(seed: u64, loads: Vec<Vec<u8>>) -> Outcome {
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
        .run_for(Duration::from_millis(100))
}

// rxStopped is Boolean, 0 or 1; testAction is one of the defined values;
// OB-UDPST drops such PDUs, and so do we.
#[udpstp_req::req("RFC9946-8.1-L4")]
#[test]
fn a_load_pdu_with_undefined_flags_is_invalid() {
    let load = |test_action: u8, rx_stopped: u8| {
        let mut b = vec![0u8; 32];
        b[0..2].copy_from_slice(&LOAD);
        b[2] = test_action;
        b[3] = rx_stopped;
        b[4..8].copy_from_slice(&1u32.to_be_bytes());
        b[8..10].copy_from_slice(&32u16.to_be_bytes());
        b
    };
    let outcome = server_sending(7, vec![load(0, 2), load(3, 0), load(0, 1)]);

    let invalid: u32 = outcome
        .client_data_events
        .iter()
        .map(|(_, e)| match e {
            DataEvent::InvalidLoadPdus { count } => *count,
            _ => 0,
        })
        .sum();
    outcome.check(invalid == 2, &format!("two invalid, not {invalid}"));
}
