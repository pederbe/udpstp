//! Server shutdown: once its administrator stops it, the Server
//! stops every Test Connection, one in its Data Phase through that Data
//! Phase and any other at once, releases each when it is gone (ADR 0003),
//! and ignores new Setup Requests. How long the runtime waits for them is
//! the runtime's (S2b's signal tests).

use super::harness;

use std::time::Duration;

use crate::protocol::{Direction, EndReason, Event, SocketId};
use harness::{DataEvent, Outcome, Scenario, SetupIgnoreReason};

const ACTIVATION: [u8; 2] = [0xAC, 0xE2];
const SHUTDOWN: Duration = Duration::from_secs(2);

fn unauthenticated(seed: u64) -> Scenario {
    Scenario::new(seed)
        .server_allows_unauthenticated()
        .client_unauthenticated()
        .server_shuts_down_at(SHUTDOWN)
}

/// When the Server's Data Phases ended, and why.
fn server_data_phase_ends(outcome: &Outcome) -> Vec<(Duration, EndReason)> {
    outcome
        .server_data_events
        .iter()
        .zip(&outcome.server_data_event_times)
        .filter_map(|((_, event), at)| match event {
            DataEvent::Ended(reason) => Some((*at, *reason)),
            _ => None,
        })
        .collect()
}

/// Whether the Server released the Test Connection on `socket` only after
/// its Data Phase's end: the order its log shows them in (ADR 0003,
/// `Endpoint::handle_socket_closed`).
fn released_after_its_end(outcome: &Outcome, socket: SocketId) -> bool {
    let ended = outcome
        .server_data_events
        .iter()
        .position(|(s, e)| *s == socket && matches!(e, DataEvent::Ended(_)));
    let released = outcome
        .server_events
        .iter()
        .position(|e| *e == Event::TestConnectionReleased { socket });
    match (ended, released) {
        (Some(ended), Some(released)) => outcome.server_event_data_marks[released] > ended,
        _ => false,
    }
}

// ADR 0014, The Server as a Windows service: stopping the Server sends
// STOP to each active Test Connection before releasing it.
#[test]
fn a_stopping_server_stops_each_data_phase_and_then_releases_it() {
    for direction in [Direction::Downstream, Direction::Upstream] {
        let outcome = unauthenticated(1)
            .client_direction(direction)
            .run_for(SHUTDOWN + Duration::from_millis(100));
        // On its engine's next turn, within a Trial Interval.
        let stopped = |&(at, reason): &(Duration, EndReason)| {
            reason == EndReason::Stopped
                && (SHUTDOWN..SHUTDOWN + Duration::from_millis(50)).contains(&at)
        };
        let ends = server_data_phase_ends(&outcome);
        outcome.check(
            ends.len() == 1
                && ends.iter().all(stopped)
                && outcome.server_released_count() == 1
                && released_after_its_end(&outcome, SocketId(1)),
            &format!(
                "{direction:?}: {:?}; {:?}",
                server_data_phase_ends(&outcome),
                outcome.server_events
            ),
        );
        outcome.check(
            outcome
                .client_data_events
                .iter()
                .any(|(_, event)| *event == DataEvent::Ended(EndReason::Completed)),
            &format!("{direction:?}: the Client receives STOP, not silence"),
        );
        let stop_pdus: Vec<_> = outcome
            .trace
            .iter()
            .filter(|pdu| {
                pdu.destination == harness::CLIENT_ADDR.parse().unwrap()
                    && (pdu.payload.starts_with(&[0xBE, 0xEF])
                        || pdu.payload.starts_with(&[0xFE, 0xED]))
                    && pdu.payload[2] == 2
            })
            .collect();
        outcome.check(
            stop_pdus.len() == 1
                && (SHUTDOWN..SHUTDOWN + Duration::from_millis(50)).contains(&stop_pdus[0].at),
            &format!("{direction:?}: the Server sends one STOP2 on its next engine turn"),
        );
    }
}

// ADR 0014: if shutdown makes STOP2 the first Downstream Load PDU, the
// Client must still confirm it and complete, without a Status PDU timer.
#[test]
fn shutdown_completes_when_stop_is_the_first_downstream_load_pdu() {
    let outcome = unauthenticated(6)
        .server_shuts_down_at(Duration::from_millis(1))
        .run_for(Duration::from_millis(100));
    let first_load = outcome
        .trace
        .iter()
        .find(|pdu| pdu.payload.starts_with(&[0xBE, 0xEF]));
    outcome.check(
        first_load.is_some_and(|pdu| pdu.payload[2] == 2)
            && outcome.trace.iter().any(|pdu| {
                pdu.source == harness::CLIENT_ADDR.parse().unwrap()
                    && pdu.payload.starts_with(&[0xFE, 0xED])
                    && pdu.payload[2] == 2
            })
            && outcome
                .client_data_events
                .iter()
                .any(|(_, event)| *event == DataEvent::Ended(EndReason::Completed))
            && outcome.server_released_count() == 1,
        "the first Load PDU is STOP2 and the Client confirms without waiting for its Watchdog",
    );
}

// ADR 0014: an Upstream Test must receive STOP even before the first Load
// PDU has reached the Server and started its Status PDU timer.
#[test]
fn shutdown_sends_stop_before_the_first_upstream_load_pdu() {
    let shutdown = Duration::from_millis(100);
    let outcome = unauthenticated(4)
        .client_direction(Direction::Upstream)
        .server_shuts_down_at(shutdown)
        .network_drops([0xBE, 0xEF], Duration::ZERO, None)
        .run_for(shutdown + Duration::from_millis(100));
    outcome.check(
        outcome
            .client_data_events
            .iter()
            .any(|(_, event)| *event == DataEvent::Ended(EndReason::Completed))
            && server_data_phase_ends(&outcome)
                .iter()
                .any(|(_, reason)| *reason == EndReason::Stopped)
            && outcome.server_released_count() == 1,
        "the Server's STOP reaches the Client without a Status PDU timer",
    );
}

// ADR 0002's receive timeout still forbids sending after silence, including
// an administrator's stop (conformance: server-initiated-stop).
#[test]
fn shutdown_does_not_send_stop_after_the_receive_timeout() {
    for direction in [Direction::Downstream, Direction::Upstream] {
        let outcome = unauthenticated(5)
            .client_direction(direction)
            .network_drops([0xBE, 0xEF], Duration::from_millis(500), None)
            .network_drops([0xFE, 0xED], Duration::from_millis(500), None)
            .run_for(SHUTDOWN + Duration::from_millis(100));
        outcome.check(
            outcome
                .server_data_events
                .iter()
                .any(|(_, event)| matches!(event, DataEvent::TrafficStopped { .. }))
                && server_data_phase_ends(&outcome) == [(SHUTDOWN, EndReason::Stopped)]
                && outcome.server_released_count() == 1
                && !outcome.trace.iter().any(|pdu| {
                    pdu.at >= SHUTDOWN
                        && pdu.destination == harness::CLIENT_ADDR.parse().unwrap()
                }),
            &format!(
                "{direction:?}: a silenced Server releases the Test Connection without sending; ends={:?}, released={}",
                server_data_phase_ends(&outcome),
                outcome.server_released_count(),
            ),
        );
    }
}

//  (a Test Connection waiting for its Activation Request is closed at
// once, not by its Activation Watchdog, which was due 3 s after its Setup
// Response)
#[test]
fn a_stopping_server_closes_a_test_connection_waiting_for_its_activation() {
    let outcome = unauthenticated(2)
        .network_drops(ACTIVATION, Duration::ZERO, None)
        .run_for(Duration::from_millis(3500));
    outcome.check(
        outcome.server_admitted_count() == 1
            && outcome.server_released_count() == 1
            && !outcome
                .server_events
                .iter()
                .any(|e| matches!(e, Event::ActivationWatchdogExpired { .. })),
        &format!("{:?}", outcome.server_events),
    );
}

//  (a stopping Server admits no more Test Connections, and doesn't
// answer)
#[test]
fn a_stopping_server_ignores_new_setup_requests() {
    let outcome = unauthenticated(3)
        .server_tests_per_source(2)
        .add_client_in_another_test_at(Direction::Downstream, SHUTDOWN + Duration::from_millis(10))
        .run_for(SHUTDOWN + Duration::from_millis(100));
    outcome.check(
        outcome.server_ignored_setup_because(SetupIgnoreReason::Stopping)
            && outcome.server_admitted_count() == 1
            && !outcome.clients_events[1]
                .iter()
                .any(|e| matches!(e, Event::SetupAccepted { .. })),
        &format!("{:?}", outcome.server_events),
    );
}
