//! Admission limits (RFC 9946 §6.1, §11 items 4 and 5; docs/spec.md "Server
//! Control Phase"): the Server admits a Test Connection within its caps on
//! admitted bandwidth and on Test Connections, globally and per Test, and
//! releases the capacity when the Test Connection ends.

mod harness;

use std::time::Duration;

use harness::{ControlPhaseFailure, Outcome, Scenario, ServerConfig};
use udpstp_core::{ActivationRejection, Direction, Event, SetupRejection};

const NOMAXBW: u8 = 9;
const CAPEXC: u8 = 10;
const CONNFAIL: u8 = 13;
const SETUP: [u8; 2] = [0xAC, 0xE1];

/// The maxBandwidth field of each Setup Request sent.
fn setup_request_max_bandwidths(outcome: &Outcome) -> Vec<u16> {
    outcome
        .trace
        .iter()
        .filter(|t| t.payload.starts_with(&SETUP) && t.payload[8] == 1)
        .map(|t| u16::from_be_bytes([t.payload[10], t.payload[11]]))
        .collect()
}

/// The Clients whose Setup Request was refused with `cmd_response`.
fn refused_with(outcome: &Outcome, cmd_response: u8) -> usize {
    outcome
        .clients_events
        .iter()
        .filter(|events| {
            events.iter().any(|e| {
                matches!(e, Event::ControlPhaseFailed(ControlPhaseFailure::SetupRejected(code))
                    if code.cmd_response() == cmd_response)
            })
        })
        .count()
}

fn activated(outcome: &Outcome) -> usize {
    (0..outcome.clients_events.len())
        .filter(|&i| outcome.client_activated_direction(i).is_some())
        .count()
}

fn server_rejected(outcome: &Outcome, reason: SetupRejection) -> usize {
    outcome
        .server_events
        .iter()
        .filter(|e| matches!(e, Event::SetupRequestRejected { reason: r, .. } if *r == reason))
        .count()
}

/// Two Tests from one Source (the harness's Clients share an address), so
/// the per-source limit doesn't decide.
fn two_tests(seed: u64, first: Direction, second: Direction) -> Scenario {
    Scenario::new(seed)
        .operator(7, b"an operator key")
        .server_tests_per_source(3)
        .client_direction(first)
        .add_client_in_another_test(second)
}

#[udpstp_req::req("RFC9946-6.1-L8")]
#[test]
fn the_client_states_its_max_bandwidth_and_sets_usdir_for_upstream() {
    for (direction, mbps, field) in [
        (Direction::Downstream, 100, 100),
        (Direction::Upstream, 100, 0x8000 | 100),
        (Direction::Upstream, 0, 0x8000),
        (Direction::Downstream, 0, 0),
    ] {
        let outcome = Scenario::new(1)
            .operator(7, b"an operator key")
            .client_max_bandwidth(mbps)
            .client_direction(direction)
            .run_for(Duration::from_millis(50));
        outcome.check(
            setup_request_max_bandwidths(&outcome) == [field],
            &format!(
                "{direction:?} {mbps}: {:?}",
                setup_request_max_bandwidths(&outcome)
            ),
        );
    }
}

// docs/spec.md (the Admitted Rate Cap rejects with CAPEXC)
#[udpstp_req::req("RFC9946-6.1-L7")]
#[test]
fn a_test_connection_over_the_bandwidth_cap_is_refused_with_capexc() {
    // The second comes once the first is activated.
    let outcome = Scenario::new(2)
        .operator(7, b"an operator key")
        .server_tests_per_source(3)
        .add_client_in_another_test_at(Direction::Downstream, Duration::from_millis(10))
        .server_config(|c| c.admitted_rate_cap = Some(150))
        .client_max_bandwidth(100)
        .run_for(Duration::from_millis(100));
    outcome.check(
        activated(&outcome) == 1
            && refused_with(&outcome, CAPEXC) == 1
            && server_rejected(&outcome, SetupRejection::CapacityExceeded) == 1,
        &format!("{:?}", outcome.clients_events),
    );
}

// docs/spec.md (a setting, off by default, requires maxBandwidth and
// rejects its absence with NOMAXBW); the combinations are Admission's own
// tests (src/admission.rs)
#[test]
fn a_server_that_requires_max_bandwidth_refuses_its_absence_with_nomaxbw() {
    let absent = Scenario::new(4)
        .operator(7, b"an operator key")
        .server_config(|c| c.require_max_bandwidth = true)
        .client_direction(Direction::Upstream)
        .run_for(Duration::from_millis(100));
    absent.check(
        refused_with(&absent, NOMAXBW) == 1
            && server_rejected(&absent, SetupRejection::MaxBandwidthRequired) == 1,
        &format!("{:?}", absent.clients_events),
    );
}

// docs/spec.md (a per-Test cap of 24 by default, CONNFAIL)
#[udpstp_req::req("RFC9946-6.1-L2", "RFC9946-11-4")]
#[test]
fn a_test_connection_over_the_per_test_cap_is_refused_with_connfail() {
    // A Test of three Test Connections, two of which must succeed.
    let outcome = Scenario::new(5)
        .operator(7, b"an operator key")
        .server_config(|c| c.test_connections_per_test = 2)
        .coordinated(3, 2)
        .run_for(Duration::from_millis(100));
    let count = |wanted: fn(&Event) -> bool| {
        outcome
            .client_connection_events
            .iter()
            .filter(|(_, e)| wanted(e))
            .count()
    };
    let activated = count(|e| matches!(e, Event::TestActivated { .. }));
    let refused = count(|e| {
        matches!(e, Event::ControlPhaseFailed(ControlPhaseFailure::SetupRejected(code))
            if code.cmd_response() == CONNFAIL)
    });
    outcome.check(
        activated == 2
            && refused == 1
            && server_rejected(&outcome, SetupRejection::TestConnectionsPerTest) == 1,
        &format!("{:?}", outcome.client_connection_events),
    );
}

#[test]
fn the_per_test_cap_is_24_by_default() {
    assert_eq!(ServerConfig::default().test_connections_per_test, 24);
}

// docs/spec.md (a global Test Connection cap, CONNFAIL)
#[udpstp_req::req("RFC9946-11-4")]
#[test]
fn a_test_connection_over_the_global_cap_is_refused_with_connfail() {
    // The third comes once the first two are activated: unactivated ones
    // would be evicted to make room instead.
    let outcome = two_tests(6, Direction::Downstream, Direction::Upstream)
        .add_client_in_another_test_at(Direction::Downstream, Duration::from_millis(10))
        .server_config(|c| c.max_test_connections = 2)
        .run_for(Duration::from_millis(100));
    outcome.check(
        activated(&outcome) == 2
            && refused_with(&outcome, CONNFAIL) == 1
            && server_rejected(&outcome, SetupRejection::TestConnections) == 1,
        &format!("{:?}", outcome.clients_events),
    );
}

// docs/spec.md (Admission holds capacity until the Test Connection ends)
#[test]
fn admission_is_released_when_a_test_connection_ends() {
    // The first Test runs its 5 s Test Interval and ends at about 5.5 s; a
    // second Test is refused before then and admitted after.
    let run = |second_at| {
        Scenario::new(7)
            .operator(7, b"an operator key")
            .client_config(|c| c.test_int_time = 5)
            .server_tests_per_source(2)
            .server_config(|c| c.max_test_connections = 1)
            .add_client_in_another_test_at(Direction::Downstream, second_at)
            .run_for(second_at + Duration::from_millis(100))
    };
    let early = run(Duration::from_millis(3500));
    early.check(
        refused_with(&early, CONNFAIL) == 1,
        &format!("{:?}", early.clients_events),
    );
    let late = run(Duration::from_millis(6500));
    late.check(
        late.server_released_count() >= 1 && late.client_activated_direction(1).is_some(),
        &format!("{:?}", late.clients_events),
    );
}

// CHSR_USDIR_BIT marks the direction the stated maxBandwidth was admitted
// for.
#[udpstp_req::req("RFC9946-6.1-L8")]
#[test]
fn an_activation_in_the_other_direction_than_the_setup_stated_is_refused() {
    // The network sets CHSR_USDIR_BIT in a Downstream Test's Setup Request.
    // Unauthenticated, so the change isn't caught by the digest.
    let outcome = Scenario::new(8)
        .server_config(|c| c.admitted_rate_cap = Some(80_000))
        .server_allows_unauthenticated()
        .client_unauthenticated()
        .client_max_bandwidth(100)
        .network_flips_byte(SETUP, 10)
        .client_direction(Direction::Downstream)
        .run_for(Duration::from_millis(100));
    outcome.check(
        outcome.client_activation_rejected(0)
            && outcome.server_events.iter().any(|e| {
                matches!(
                    e,
                    Event::ActivationRequestRejected {
                        reason: ActivationRejection::MaxBandwidthDirection,
                        ..
                    }
                )
            }),
        &format!("{:?}", outcome.server_events),
    );
}

// docs/spec.md (Admission is released on every exit path: here, a socket
// that can't be opened, which is refused with CONNFAIL as OB-UDPST does)
#[test]
fn a_setup_whose_socket_cant_be_opened_is_refused_and_released() {
    let outcome = two_tests(10, Direction::Downstream, Direction::Downstream)
        .server_config(|c| c.max_test_connections = 1)
        .server_socket_opens_fail(1)
        .run_for(Duration::from_millis(100));
    outcome.check(
        activated(&outcome) == 1
            && refused_with(&outcome, CONNFAIL) == 1
            && server_rejected(&outcome, SetupRejection::SocketUnavailable) == 1,
        &format!("{:?}", outcome.clients_events),
    );
}

//  (Setups admitted together past the Admitted Rate Cap: the first
// to activate keeps its rate, the other's Activation is refused)
#[test]
fn of_two_setups_racing_past_the_cap_the_first_activated_keeps_it() {
    let outcome = two_tests(21, Direction::Downstream, Direction::Downstream)
        .server_config(|c| c.admitted_rate_cap = Some(150))
        .client_max_bandwidth(100)
        .run_for(Duration::from_millis(100));
    let refused_at_activation = outcome.server_events.iter().any(|e| {
        matches!(
            e,
            Event::ActivationRequestRejected {
                reason: ActivationRejection::CapacityExceeded,
                ..
            }
        )
    });
    outcome.check(
        activated(&outcome) == 1 && refused_at_activation,
        &format!("{:?}", outcome.server_events),
    );
}

// The global cap is reserved at activation, as a whole Test's places.
// (conformance: unactivated-test-connection-share)
#[test]
fn of_two_setups_racing_for_one_place_only_one_activates() {
    let outcome = two_tests(22, Direction::Downstream, Direction::Downstream)
        .server_config(|c| c.max_test_connections = 1)
        .run_for(Duration::from_millis(100));
    outcome.check(
        activated(&outcome) == 1
            && outcome.server_admitted_count() == 2
            && outcome.server_released_count() == 1
            && outcome.server_rejected_activation_because(ActivationRejection::TestConnections)
            && (0..2)
                .filter(|&i| outcome.client_activation_rejected(i))
                .count()
                == 1,
        &format!("{:?} {:?}", outcome.server_events, outcome.clients_events),
    );
}
