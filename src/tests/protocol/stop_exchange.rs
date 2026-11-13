//! The STOP exchange (RFC 9946 §9): the Server's Test Interval timer sets
//! STOP1, its next PDUs carry STOP2, the Client confirms and both end. Each
//! end also enforces the Test Interval itself (§9, §11).
//!
//! PDUs are read by their byte offsets (testAction is byte 2 of Load and
//! Status PDUs, Figures 9 and 10). Times come from the trace, and expected
//! times from the Test Interval the Client asks for (10 s) plus the margins
//! this implementation chose (see `DataPhase`).

use super::harness;

use std::net::SocketAddr;
use std::time::Duration;

use crate::protocol::{Direction, EndReason, Event};
use harness::{ActivationParameter, DataEvent, Outcome, RowRequest, Scenario, TraceEntry};

/// The Server sets STOP1 half a second after testIntTime.
const STOP1_AFTER: Duration = Duration::from_millis(10_500);
const STOP2: u8 = 2;
/// The Client asks for 50 ms Trial Intervals.
const TRIAL: Duration = Duration::from_millis(50);
/// Row 35's Bursts go out every millisecond.
const BURST_INTERVAL: Duration = Duration::from_millis(1);

fn client() -> SocketAddr {
    harness::CLIENT_ADDR.parse().unwrap()
}

fn is_load(t: &TraceEntry) -> bool {
    t.payload.starts_with(&[0xBE, 0xEF])
}

fn is_status(t: &TraceEntry) -> bool {
    t.len == 204 && t.payload.starts_with(&[0xFE, 0xED])
}

fn downstream(seed: u64) -> Scenario {
    Scenario::new(seed)
        // A Fixed-Rate Test needs an Operator Key (RFC 9946 §4.1).
        .operator(7, b"an operator key")
        .client_direction(Direction::Downstream)
        // 35 Mbps: Bursts of 3 full-size datagrams plus an add-on.
        .client_requests_row(RowRequest::FixedRate(35))
}

/// When the Server sent the Activation Response.
fn activated_at(outcome: &Outcome) -> Duration {
    let at = outcome
        .trace
        .iter()
        .find(|t| t.destination == client() && t.len == 104 && t.payload.starts_with(&[0xAC, 0xE2]))
        .map(|t| t.at);
    outcome.check(at.is_some(), "the Test was activated");
    at.unwrap_or_default()
}

fn loads_to_client(outcome: &Outcome) -> Vec<&TraceEntry> {
    outcome
        .trace
        .iter()
        .filter(|t| t.destination == client() && is_load(t))
        .collect()
}

fn statuses_from_client(outcome: &Outcome) -> Vec<&TraceEntry> {
    outcome
        .trace
        .iter()
        .filter(|t| t.source == client() && is_status(t))
        .collect()
}

fn ended(events: &[(crate::protocol::SocketId, DataEvent)]) -> Vec<EndReason> {
    events
        .iter()
        .filter_map(|(_, e)| match e {
            DataEvent::Ended(reason) => Some(*reason),
            _ => None,
        })
        .collect()
}

fn server_released(outcome: &Outcome) -> bool {
    outcome
        .server_events
        .iter()
        .any(|e| matches!(e, Event::TestConnectionReleased { .. }))
}

// docs/spec.md story 20
#[udpstp_req::req(
    "RFC9946-9-1",
    "RFC9946-9-2",
    "RFC9946-9-3",
    "RFC9946-9-4",
    "RFC9946-9-5",
    "RFC9946-9-7",
    "RFC9946-9-8",
    "RFC9946-9-6",
    "RFC9097-7.2-2"
)]
#[test]
fn the_test_stops_gracefully_when_the_servers_test_interval_ends() {
    let outcome = downstream(1).run_for(Duration::from_secs(13));
    let activated = activated_at(&outcome);
    let stop1 = activated + STOP1_AFTER;

    let loads = loads_to_client(&outcome);
    let (testing, stopping): (Vec<&&TraceEntry>, Vec<_>) =
        loads.iter().partition(|t| t.payload[2] == 0);
    outcome.check(
        testing.iter().all(|t| t.at < stop1) && !stopping.is_empty(),
        "Load PDUs carry TEST_ACT_TEST until STOP1",
    );
    outcome.check(
        stopping
            .iter()
            .all(|t| t.at >= stop1 && t.payload[2] == STOP2),
        "from STOP1 on every Load PDU carries STOP2",
    );
    let largest_burst = |pdus: &[&&TraceEntry]| {
        let mut sizes = std::collections::HashMap::new();
        for t in pdus {
            *sizes.entry(t.at).or_insert(0) += 1;
        }
        sizes.into_values().max().unwrap_or(0)
    };
    outcome.check(
        largest_burst(&stopping) == 1 && largest_burst(&testing) > 1,
        "Bursts are cut to one datagram in STOP2",
    );

    let first_stop2 = stopping.first().map(|t| t.at).unwrap_or_default();
    outcome.check(
        first_stop2 - stop1 <= BURST_INTERVAL,
        "the next Burst after STOP1 carries STOP2",
    );
    let statuses = statuses_from_client(&outcome);
    let (tested, confirming): (Vec<&&TraceEntry>, Vec<_>) =
        statuses.iter().partition(|t| t.payload[2] == 0);
    outcome.check(
        confirming.len() == 1 && confirming[0].payload[2] == STOP2,
        &format!("one Status PDU confirms, not {}", confirming.len()),
    );
    let confirmed = confirming.first().map(|t| t.at).unwrap_or_default();
    outcome.check(
        tested.iter().all(|t| t.at <= first_stop2)
            && confirmed >= first_stop2
            && confirmed < first_stop2 + Duration::from_millis(50),
        "the Client confirms at its next Status PDU timer",
    );
    outcome.check(
        statuses.last().is_some_and(|t| t.at == confirmed),
        "and sends nothing after it",
    );
    outcome.check(
        loads.last().is_some_and(|t| t.at <= confirmed),
        "the Server sends no Load PDU after the confirmation",
    );

    let client_events: Vec<DataEvent> =
        outcome.client_data_events.iter().map(|(_, e)| *e).collect();
    let stop_received = client_events
        .iter()
        .position(|e| *e == DataEvent::StopReceived);
    let client_ended = client_events
        .iter()
        .position(|e| *e == DataEvent::Ended(EndReason::Completed));
    outcome.check(
        stop_received.is_some() && stop_received < client_ended,
        &format!("the Client finalizes, then ends: {client_events:?}"),
    );
    outcome.check(
        ended(&outcome.server_data_events) == [EndReason::Completed] && server_released(&outcome),
        "the Server ends on the confirmation and releases the Test Connection",
    );
}

// docs/spec.md "Both ends also enforce the Test Interval themselves"
#[udpstp_req::req("RFC9946-9-10", "RFC9946-9-L1")]
#[test]
fn a_cleared_stop_cannot_extend_the_test() {
    let outcome = downstream(2)
        .network_clears_test_action()
        .run_for(Duration::from_secs(15));
    let activated = activated_at(&outcome);

    // The Server waits a Trial Interval plus 1 s for a confirmation after
    // STOP1; the Client ends 1 s past the Server's STOP1.
    let server_end = activated + STOP1_AFTER + TRIAL + Duration::from_secs(1);
    let client_end = activated + STOP1_AFTER + Duration::from_secs(1);
    let last_load = loads_to_client(&outcome).last().map(|t| t.at);
    outcome.check(
        last_load.is_some_and(|at| at < server_end && at >= server_end - BURST_INTERVAL),
        &format!("the Server sends STOP2 until {server_end:?}, not {last_load:?}"),
    );
    let last_status = statuses_from_client(&outcome).last().map(|t| t.at);
    outcome.check(
        last_status.is_some_and(|at| at <= client_end && at > client_end - TRIAL),
        &format!("the Client sends Status PDUs until {client_end:?}, not {last_status:?}"),
    );
    outcome.check(
        ended(&outcome.server_data_events) == [EndReason::Unconfirmed] && server_released(&outcome),
        "the Server ends unconfirmed",
    );
    outcome.check(
        ended(&outcome.client_data_events) == [EndReason::TestIntervalElapsed],
        &format!(
            "the Client ends by its own timer: {:?}",
            outcome.client_data_events
        ),
    );
    outcome.check(
        !outcome
            .client_data_events
            .iter()
            .any(|(_, e)| *e == DataEvent::StopReceived),
        "no STOP2 reached the Client",
    );
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

// Between STOP1 and its next PDU the Server still processes what it
// receives: in an Upstream Test, the Load PDUs that arrive after STOP1 count
// in the Trial Interval its STOP2 Status PDU reports.
#[udpstp_req::req("RFC9946-9-L2")]
#[test]
fn load_pdus_arriving_after_stop1_still_count() {
    let outcome = Scenario::new(7)
        .operator(7, b"an operator key")
        .client_direction(Direction::Upstream)
        .client_requests_row(RowRequest::FixedRate(35))
        // The shortest Test Interval the Server accepts.
        .client_config(|c| c.test_int_time = 5)
        .run_for(Duration::from_secs(7));
    let stop1 = activated_at(&outcome) + Duration::from_millis(5500);

    // The Server's Status PDUs, by their place in the trace.
    let statuses: Vec<usize> = (0..outcome.trace.len())
        .filter(|&i| outcome.trace[i].destination == client() && is_status(&outcome.trace[i]))
        .collect();
    let first_stop2 = statuses
        .iter()
        .position(|&i| outcome.trace[i].payload[2] == STOP2);
    outcome.check(
        first_stop2.is_some_and(|k| k > 0),
        "the Server sent Status PDUs, then one marked STOP2",
    );
    let k = first_stop2.unwrap_or_default();
    let (before, stop2) = (&outcome.trace[statuses[k - 1]], &outcome.trace[statuses[k]]);
    outcome.check(
        before.at < stop1 && stop2.at >= stop1,
        &format!(
            "STOP1 at {stop1:?} falls between the Status PDUs at {:?} and {:?}",
            before.at, stop2.at
        ),
    );

    // The Load PDUs that reached the Server in the Trial Interval the STOP2
    // Status PDU closes: between the two in the trace.
    let arrived: Vec<&TraceEntry> = outcome.trace[statuses[k - 1]..statuses[k]]
        .iter()
        .filter(|t| t.source == client() && t.delivered && is_load(t))
        .collect();
    outcome.check(
        arrived.iter().any(|t| t.at > stop1),
        "some arrived after STOP1",
    );
    let bytes: usize = arrived.iter().map(|t| t.len).sum();
    let (ti_rx_datagrams, ti_rx_bytes) = (u32_at(&stop2.payload, 144), u32_at(&stop2.payload, 148));
    outcome.check(
        ti_rx_datagrams as usize == arrived.len() && ti_rx_bytes as usize == bytes,
        &format!(
            "the STOP2 Status PDU counts all {} ({bytes} bytes), not {ti_rx_datagrams} \
             ({ti_rx_bytes} bytes)",
            arrived.len()
        ),
    );
}

/// A header-only Load PDU (RFC 9946 Figure 9).
fn load(seq_no: u32, test_action: u8) -> Vec<u8> {
    let mut b = vec![0u8; 32];
    b[0..2].copy_from_slice(&[0xBE, 0xEF]);
    b[2] = test_action;
    b[4..8].copy_from_slice(&seq_no.to_be_bytes());
    b[8..10].copy_from_slice(&32u16.to_be_bytes());
    b
}

// TEST_ACT_STOP1 is used locally by the Server; rfc-ambiguities.md §17
#[udpstp_req::req("RFC9946-9-4", "RFC9946-9-5")]
#[test]
fn only_stop2_stops_the_client_and_later_load_pdus_are_ignored() {
    let mut statuses = 0;
    let outcome = Scenario::new(3)
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
                return vec![ack, load(1, 1), load(2, 7)];
            }
            if request.len() == 204 {
                statuses += 1;
                if statuses == 1 {
                    return vec![load(3, STOP2), load(4, 0), load(5, 0)];
                }
            }
            Vec::new()
        })
        .run_for(Duration::from_millis(300));

    let statuses = statuses_from_client(&outcome);
    outcome.check(
        statuses.len() == 2,
        &format!("two Status PDUs, not {}", statuses.len()),
    );
    outcome.check(
        statuses[0].payload[2] == 0,
        "STOP1 and unknown testActions don't stop the Client",
    );
    let ti_rx_datagrams = u32::from_be_bytes(statuses[1].payload[144..148].try_into().unwrap());
    outcome.check(
        statuses[1].payload[2] == STOP2 && ti_rx_datagrams == 0,
        "the confirmation counts neither the STOP2 Load PDU nor the two after \
         it, as OB-UDPST's doesn't",
    );
    outcome.check(
        ended(&outcome.client_data_events) == [EndReason::Completed],
        "the Client ends after confirming",
    );
}

// docs/spec.md  (a Client that ends a Test early sends a PDU marked
// TEST_ACT_STOP2 at once, so the Server ends the Test Connection instead of
// waiting for its watchdog)
#[udpstp_req::req("RFC9946-7.3-L1")]
#[test]
fn a_client_that_refuses_a_coercion_stops_the_server_at_once() {
    let outcome = Scenario::new(4)
        .server_allows_unauthenticated()
        .client_unauthenticated()
        .client_direction(Direction::Downstream)
        // Beyond the table: the Server coerces it to its last row.
        .client_requests_row(RowRequest::StartAt(5000))
        .client_config(|c| {
            c.unacceptable_coercions
                .push(ActivationParameter::SrIndexConf)
        })
        .run_for(Duration::from_secs(1));

    outcome.check(
        outcome.client_refused_coercion().is_some(),
        "the Client refused the coercion",
    );
    let activated = activated_at(&outcome);
    let stop = statuses_from_client(&outcome);
    outcome.check(
        stop.len() == 1 && stop[0].payload[2] == STOP2 && stop[0].at == activated,
        "the Client sends one Status PDU marked STOP2 at once",
    );
    outcome.check(
        ended(&outcome.server_data_events) == [EndReason::StoppedByClient]
            && server_released(&outcome),
        &format!(
            "the Server ends the Test Connection: {:?}",
            outcome.server_data_events
        ),
    );
    outcome.check(
        loads_to_client(&outcome).is_empty(),
        "the Load PDUs, due 5-50 ms after activation, never start",
    );
}

// docs/spec.md  (a Client that ends a Test early sends a PDU marked
// TEST_ACT_STOP2 at once; RFC 9946 §9 defines only a Server-initiated stop)
#[test]
fn a_client_stopping_its_data_phase_tells_the_server_at_once() {
    let stop_at = Duration::from_millis(2000);
    let outcome = downstream(5)
        .client_stops_data_phase_at(stop_at)
        .run_for(Duration::from_secs(4));

    let statuses = statuses_from_client(&outcome);
    let last = statuses.last();
    outcome.check(
        last.is_some_and(|t| t.at == stop_at && t.payload[2] == STOP2),
        "the Client's last Status PDU carries STOP2 and goes out at once",
    );
    outcome.check(
        statuses.iter().rev().skip(1).all(|t| t.payload[2] == 0),
        "the ones before it don't",
    );
    outcome.check(
        ended(&outcome.client_data_events) == [EndReason::Stopped]
            && ended(&outcome.server_data_events) == [EndReason::StoppedByClient]
            && server_released(&outcome),
        "both ends end the Test Connection",
    );
    outcome.check(
        loads_to_client(&outcome)
            .last()
            .is_some_and(|t| t.at <= stop_at),
        "the Server sends no Load PDU afterwards",
    );
}

// rfc-ambiguities.md §17 (the Server takes any testAction but
// TEST_ACT_TEST as the Client's STOP, as OB-UDPST does: an OB-UDPST Client
// ends a Test on its own timer with STOP1)
#[test]
fn the_server_ends_on_a_stop1_from_the_client() {
    let outcome = downstream(6)
        .network_sets_status_test_action_from(Duration::from_millis(500), 1)
        .run_for(Duration::from_secs(1));

    outcome.check(
        ended(&outcome.server_data_events) == [EndReason::StoppedByClient]
            && server_released(&outcome),
        &format!("the Server ended: {:?}", outcome.server_data_events),
    );
}
