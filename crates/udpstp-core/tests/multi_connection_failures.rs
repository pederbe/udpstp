//! A Multi-Connection Test survives isolated failures and stops on path
//! failures (docs/spec.md stories 59 and 60; ): a Test Connection whose
//! Data Phase stops hearing its peer has failed; the Test goes on while
//! the Minimum Test Connections remain, and two falling silent together
//! stop it. Whatever is stopped releases its Server's admission.

mod harness;

use std::net::SocketAddr;
use std::time::Duration;

use harness::{DataEvent, Outcome, Scenario};
use udpstp_core::{EndReason, Event, SocketId, TestAbort};

const KEY: &[u8] = b"a multi-connection key";

/// A Test of `n` Test Connections, `minimum` of which must remain, over
/// `servers` Servers (1 to 3), with a 5 s Test Interval.
fn test(seed: u64, n: u8, minimum: u8, servers: usize) -> Scenario {
    let mut scenario = Scenario::new(seed)
        .mode1(3, KEY)
        .client_config(|c| c.test_int_time = 5)
        .coordinated(n, minimum);
    for _ in 1..servers {
        scenario = scenario.add_server(|_| {});
    }
    scenario
}

fn server(k: usize) -> SocketAddr {
    match k {
        0 => harness::SERVER_ADDR.parse().unwrap(),
        k => harness::extra_server_addr(k - 1, false),
    }
}

fn aborted(outcome: &Outcome) -> Option<TestAbort> {
    outcome.client_events.iter().find_map(|e| match e {
        Event::TestAborted(reason) => Some(*reason),
        _ => None,
    })
}

/// How the Client's Data Phase on `socket` ended.
fn ended(outcome: &Outcome, socket: u32) -> Option<EndReason> {
    outcome
        .client_data_events
        .iter()
        .find_map(|(s, e)| match e {
            DataEvent::Ended(reason) if *s == SocketId(socket) => Some(*reason),
            _ => None,
        })
}

/// Every Server released every Test Connection it admitted.
fn all_released(outcome: &Outcome) -> bool {
    let count = |events: &[Event], released: bool| {
        events
            .iter()
            .filter(|e| {
                if released {
                    matches!(e, Event::TestConnectionReleased { .. })
                } else {
                    matches!(e, Event::TestConnectionAdmitted { .. })
                }
            })
            .count()
    };
    std::iter::once(&outcome.server_events)
        .chain(&outcome.other_servers_events)
        .all(|events| count(events, false) == count(events, true))
}

// docs/spec.md story 59 (a Test Connection's silence alone, with none
// healthy left to stop, leaves its own end to tell: the Client's Test
// Interval timer completes it)
#[test]
fn a_silence_at_the_end_with_nothing_left_to_stop_is_no_abort() {
    let outcome = test(6, 1, 1, 1)
        .network_cuts_off(server(0), Duration::from_millis(4500))
        .run_for(Duration::from_secs(10));
    outcome.check(
        aborted(&outcome).is_none() && ended(&outcome, 1) == Some(EndReason::TestIntervalElapsed),
        "no abort, and the Test Interval timer ended the Test",
    );
}

// The architecture review of 2026-09-30 (the rule for a completed Test
// Connection stated once, EndReason::completes): one that the Client's own
// Test Interval timer ended completed, so the Test goes on while another
// still runs, though every Test Connection is required. Here the network
// clears every STOP2, so each Test Connection ends by its own timer, one
// after the other
#[test]
fn a_test_connection_its_own_test_interval_ended_completed() {
    let outcome = test(9, 2, 2, 1)
        .network_clears_test_action()
        .run_for(Duration::from_secs(12));
    outcome.check(aborted(&outcome).is_none(), "the Test wasn't aborted");
    outcome.check(
        ended(&outcome, 1) == Some(EndReason::TestIntervalElapsed)
            && ended(&outcome, 2) == Some(EndReason::TestIntervalElapsed),
        "each Test Connection's own timer ended it",
    );
    outcome.check(all_released(&outcome), "the Server released both");
}

// docs/spec.md story 59 (the Test continues when a connection fails and
// at least the minimum remain)
#[udpstp_req::req("RFC9946-4-L1")]
#[test]
fn the_test_goes_on_when_one_fails_and_enough_remain() {
    let outcome = test(1, 2, 1, 2)
        .network_cuts_off(server(1), Duration::from_secs(2))
        .run_for(Duration::from_secs(10));
    outcome.check(aborted(&outcome).is_none(), "the Test wasn't aborted");
    outcome.check(
        ended(&outcome, 1) == Some(EndReason::Completed)
            && ended(&outcome, 2) == Some(EndReason::Watchdog),
        "the first Test Connection completed, the second timed out",
    );
    outcome.check(all_released(&outcome), "both Servers released");
}

// docs/spec.md story 59 (below the minimum, the Test stops, and what
// remains is cleaned up and releases its admission)
#[udpstp_req::req("RFC9946-4-L1")]
#[test]
fn below_the_minimum_the_rest_are_stopped_and_released() {
    let outcome = test(2, 2, 2, 2)
        .network_cuts_off(server(1), Duration::from_secs(2))
        .run_for(Duration::from_secs(10));
    outcome.check(
        aborted(&outcome) == Some(TestAbort::TooFewTestConnections { minimum: 2 }),
        "the Test aborted when the second fell silent",
    );
    let stopped = outcome
        .server_data_events
        .iter()
        .any(|(_, e)| *e == DataEvent::Ended(EndReason::StoppedByClient));
    outcome.check(
        stopped && ended(&outcome, 1) == Some(EndReason::Stopped),
        "the first Test Connection was stopped with the STOP exchange, well before its end",
    );
    outcome.check(all_released(&outcome), "both Servers released");
}

//  (the Client on Ctrl-C: the interrupt is a core input that stops
// every Test Connection with its STOP2, so each Server ends its own at
// once and releases it)
#[test]
fn an_interrupted_test_stops_every_test_connection_with_the_stop_exchange() {
    let outcome = test(7, 2, 2, 1)
        .client_interrupted_at(Duration::from_secs(2))
        .run_for(Duration::from_secs(10));
    outcome.check(
        aborted(&outcome) == Some(TestAbort::Interrupted),
        "the Test was aborted as interrupted",
    );
    let stopped_by_client = outcome
        .server_data_events
        .iter()
        .filter(|(_, e)| *e == DataEvent::Ended(EndReason::StoppedByClient))
        .count();
    outcome.check(
        stopped_by_client == 2
            && ended(&outcome, 1) == Some(EndReason::Stopped)
            && ended(&outcome, 2) == Some(EndReason::Stopped),
        "each Test Connection sent its STOP2, and each Server ended its own at once",
    );
    outcome.check(all_released(&outcome), "both Servers released");
}

// : an interrupt once every Test Connection has ended changes nothing
#[test]
fn an_interrupt_after_the_test_ended_is_no_abort() {
    let outcome = test(8, 1, 1, 1)
        .client_interrupted_at(Duration::from_secs(9))
        .run_for(Duration::from_secs(10));
    outcome.check(
        aborted(&outcome).is_none() && ended(&outcome, 1) == Some(EndReason::Completed),
        "the Test completed, and the interrupt came too late to abort it",
    );
}

// docs/spec.md story 60 (several connections timing out together stop the
// Test, as a path failure)
#[udpstp_req::req("RFC9946-4-L1", "RFC9946-4.2-L5")]
#[test]
fn test_connections_that_fall_silent_together_stop_the_test() {
    // Two of four go to the second Server, whose path dies; one would be
    // enough to go on.
    let outcome = test(3, 4, 1, 2)
        .network_cuts_off(server(1), Duration::from_secs(2))
        .run_for(Duration::from_secs(10));
    outcome.check(
        aborted(&outcome) == Some(TestAbort::TestConnectionsTimedOut),
        "a path failure stopped the Test",
    );
    outcome.check(
        ended(&outcome, 1) == Some(EndReason::Stopped)
            && ended(&outcome, 3) == Some(EndReason::Stopped),
        "the first Server's Test Connections were stopped",
    );
    outcome.check(all_released(&outcome), "both Servers released");
}

// docs/spec.md stories 59 and 60 (each failure counts, but apart they are
// isolated ones)
#[udpstp_req::req("RFC9946-4-L1")]
#[test]
fn failures_are_counted_until_too_few_remain() {
    // Two of three must remain: the first failure is survived, the second
    // stops the Test.
    let outcome = test(5, 3, 2, 3)
        .network_cuts_off(server(1), Duration::from_secs(1))
        .run_for(Duration::from_millis(3500));
    outcome.check(aborted(&outcome).is_none(), "one failure is survived");
    let outcome = test(5, 3, 2, 3)
        .network_cuts_off(server(1), Duration::from_secs(1))
        .network_cuts_off(server(2), Duration::from_secs(3))
        .run_for(Duration::from_secs(10));
    outcome.check(
        aborted(&outcome) == Some(TestAbort::TooFewTestConnections { minimum: 2 })
            && ended(&outcome, 1) == Some(EndReason::Stopped),
        "the second failure stopped the first Test Connection",
    );
    outcome.check(all_released(&outcome), "every Server released");
}

// docs/spec.md stories 59 and 60 (failures apart are isolated ones)
#[udpstp_req::req("RFC9946-4-L1")]
#[test]
fn failures_apart_are_isolated_ones() {
    let outcome = test(4, 3, 1, 3)
        .network_cuts_off(server(1), Duration::from_secs(1))
        .network_cuts_off(server(2), Duration::from_secs(3))
        .run_for(Duration::from_secs(10));
    outcome.check(aborted(&outcome).is_none(), "the Test went on");
    outcome.check(
        ended(&outcome, 1) == Some(EndReason::Completed),
        "the first Test Connection completed",
    );
    outcome.check(all_released(&outcome), "every Server released");
}
