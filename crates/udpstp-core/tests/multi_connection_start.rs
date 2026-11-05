//! The Client's two-phase start of a Multi-Connection Test (docs/spec.md
//! "Control Phase (Client)", stories 54-58 and 93; ): every Setup
//! first, the Activations together once at least the Minimum Test
//! Connections succeeded, the same parameters from every Server, and Test
//! Connections spread round-robin across the Servers.

mod harness;

use std::net::SocketAddr;
use std::time::Duration;

use harness::{ControlExchange, ControlPhaseFailure, Outcome, Scenario, TraceEntry};
use udpstp_core::{DscpSet, EndReason, Event, SocketId, TestAbort};

const KEY: &[u8] = b"a multi-connection key";
const SETUP: [u8; 2] = [0xAC, 0xE1];
const ACTIVATION: [u8; 2] = [0xAC, 0xE2];

fn test(seed: u64, test_connections: u8, minimum: u8) -> Scenario {
    Scenario::new(seed)
        .mode1(3, KEY)
        .server_config(|c| c.test_connections_per_test = 8)
        .coordinated(test_connections, minimum)
}

/// Requests the Client sent: a Setup Request's cmdRequest (byte 8) is 1,
/// an Activation Request's cmdResponse (byte 5) 0.
fn setup_requests(outcome: &Outcome) -> Vec<&TraceEntry> {
    outcome
        .trace
        .iter()
        .filter(|t| t.payload.starts_with(&SETUP) && t.payload[8] == 1)
        .collect()
}

fn activation_requests(outcome: &Outcome) -> Vec<&TraceEntry> {
    outcome
        .trace
        .iter()
        .filter(|t| t.payload.starts_with(&ACTIVATION) && t.payload[5] == 0)
        .collect()
}

fn activated(outcome: &Outcome) -> Vec<SocketId> {
    outcome
        .client_connection_events
        .iter()
        .filter(|(_, e)| matches!(e, Event::TestActivated { .. }))
        .map(|(socket, _)| *socket)
        .collect()
}

fn aborted(outcome: &Outcome) -> Option<TestAbort> {
    outcome.client_events.iter().find_map(|e| match e {
        Event::TestAborted(reason) => Some(*reason),
        _ => None,
    })
}

// RFC 9946 §6.1 (mcCount the connections the Client attempts); docs/spec.md
// stories 54, 56, 58, 93
#[udpstp_req::req("RFC9946-6.1-L1", "RFC9946-6.1-L3", "RFC9946-4-L4")]
#[test]
fn every_setup_goes_out_before_the_activations_which_go_out_together() {
    let outcome = test(1, 4, 4)
        .path_delay(Duration::from_millis(10), Duration::ZERO)
        .run_for(Duration::from_millis(500));
    let setups = setup_requests(&outcome);
    let activations = activation_requests(&outcome);
    let ident = setups.first().map(|t| &t.payload[6..8]);
    outcome.check(
        setups.len() == 4
            && ident.is_some_and(|i| i != [0, 0])
            && setups.iter().enumerate().all(|(i, t)| {
                t.payload[4] == i as u8 && t.payload[5] == 4 && Some(&t.payload[6..8]) == ident
            }),
        "four Setup Requests with mcIndex 0-3, mcCount 4 and one non-zero mcIdent",
    );
    let last_setup = setups.iter().map(|t| t.at).max().unwrap();
    outcome.check(
        activations.len() == 4
            && activations
                .iter()
                .all(|t| t.at == activations[0].at && t.at > last_setup),
        "the four Activation Requests go out together, after every Setup",
    );
    // One Server answers all four, each with its own testPort (byte
    // 12-13), and each Activation Request goes to one of them.
    let mut ports: Vec<u16> = outcome
        .trace
        .iter()
        .filter(|t| t.payload.starts_with(&SETUP) && t.payload[8] == 2)
        .map(|t| u16::from_be_bytes([t.payload[12], t.payload[13]]))
        .collect();
    ports.sort_unstable();
    ports.dedup();
    let mut activation_ports: Vec<u16> = activations.iter().map(|t| t.destination.port()).collect();
    activation_ports.sort_unstable();
    outcome.check(
        ports.len() == 4 && activation_ports == ports,
        "four distinct testPorts, one per Test Connection",
    );
    outcome.check(
        activated(&outcome).len() == 4,
        "every Test Connection is activated",
    );
}

// docs/spec.md story 55 (Test Connections spread round-robin across the
// Servers)
#[test]
fn test_connections_go_to_the_servers_in_turn() {
    let outcome = test(2, 4, 4)
        .add_server(|c| c.test_connections_per_test = 8)
        .run_for(Duration::from_millis(500));
    let server = |t: &TraceEntry| t.destination.ip();
    let setups = setup_requests(&outcome);
    let first: SocketAddr = harness::SERVER_ADDR.parse().unwrap();
    let second = harness::extra_server_addr(0, false);
    outcome.check(
        setups.len() == 4
            && setups.iter().all(|t| {
                let expected = if t.payload[4] % 2 == 0 { first } else { second };
                server(t) == expected.ip()
            }),
        "mcIndex 0 and 2 go to the first Server, 1 and 3 to the second",
    );
    outcome.check(activated(&outcome).len() == 4, "all four activated");
}

// docs/spec.md story 56 (fewer successful Setups than the minimum abort
// the Test before any Activation Request)
#[udpstp_req::req("RFC9946-4-L1")]
#[test]
fn too_few_setups_abort_the_test_before_any_activation() {
    // The Server takes two Test Connections of a Test.
    let outcome = Scenario::new(3)
        .mode1(3, KEY)
        .server_config(|c| c.test_connections_per_test = 2)
        .coordinated(4, 4)
        .run_for(Duration::from_secs(4));
    outcome.check(
        activation_requests(&outcome).is_empty()
            && aborted(&outcome) == Some(TestAbort::TooFewTestConnections { minimum: 4 }),
        "no Activation Request, and the Test aborted",
    );
    outcome.check(
        outcome.server_admitted_count() == outcome.server_released_count(),
        "the Server released what it admitted",
    );
}

// docs/spec.md story 57 (a minimum below all accepts a partial Test)
#[udpstp_req::req("RFC9946-4-L1")]
#[test]
fn a_lower_minimum_goes_on_with_the_setups_that_succeeded() {
    let outcome = Scenario::new(4)
        .mode1(3, KEY)
        .server_config(|c| c.test_connections_per_test = 2)
        .coordinated(4, 2)
        .run_for(Duration::from_millis(500));
    outcome.check(
        activation_requests(&outcome).len() == 2
            && activated(&outcome).len() == 2
            && aborted(&outcome).is_none(),
        "two Test Connections activated",
    );
}

// docs/spec.md story 57: with enough Setups done, one unanswered is given up in time for the rest to
// activate
#[udpstp_req::req("RFC9946-4-L1", "RFC9946-6.1-2")]
#[test]
fn an_unanswered_setup_is_given_up_once_enough_have_succeeded() {
    // The second Server has no keys, so it drops its Setup Request.
    let outcome = test(5, 2, 1)
        .add_server(|c| c.keys = Default::default())
        .run_for(Duration::from_secs(4));
    let activations = activation_requests(&outcome);
    let abandoned = outcome
        .client_connection_events
        .iter()
        .any(|(socket, e)| *socket == SocketId(2) && *e == Event::TestConnectionAbandoned);
    outcome.check(
        activations.len() == 1
            && activations[0].at == Duration::from_secs(2)
            && abandoned
            && activated(&outcome) == [SocketId(1)],
        "the first Test Connection activates at the 2 s Setup deadline, the second is given up",
    );

    // With both required, the Test waits for it until the time limit.
    let outcome = test(6, 2, 2)
        .add_server(|c| c.keys = Default::default())
        .run_for(Duration::from_secs(4));
    // : each Test Connection's Test initiation timer says where it
    // expired: the second's in the Setup exchange, and the first's, whose
    // Setup Response was accepted and whose Activation Request the
    // Coordinator held, in the Activation exchange.
    let timed_out = |socket: SocketId, exchange: ControlExchange| {
        outcome.client_connection_events.iter().any(|(s, e)| {
            *s == socket
                && *e
                    == Event::ControlPhaseFailed(ControlPhaseFailure::TestInitiationTimedOut {
                        exchange,
                        last_ignored: None,
                    })
        })
    };
    outcome.check(
        activation_requests(&outcome).is_empty()
            && timed_out(SocketId(2), ControlExchange::Setup)
            && timed_out(SocketId(1), ControlExchange::Activation)
            && aborted(&outcome) == Some(TestAbort::TooFewTestConnections { minimum: 2 }),
        "no Activation, and the Test aborted when the Setup timed out",
    );
}

// docs/spec.md story 58 (coerced parameters checked for consistency; the
// Test aborts if they differ)
#[udpstp_req::req("RFC9946-4-L1")]
#[test]
fn servers_that_set_different_parameters_abort_the_test() {
    // The first Server allows DSCP 46; the second coerces it to 0.
    let outcome = Scenario::new(7)
        .mode1(3, KEY)
        .server_config(|c| c.allowed_dscp = DscpSet::ALL)
        .add_server(|_| {})
        .client_config(|c| c.dscp = 46)
        .coordinated(2, 2)
        .run_for(Duration::from_secs(3));
    outcome.check(
        matches!(aborted(&outcome), Some(TestAbort::ParametersDiffer { .. })),
        "the Test aborted",
    );
    // Both Test Connections were activated; both are stopped.
    let stopped = |events: &[(SocketId, harness::DataEvent)]| {
        events
            .iter()
            .any(|(_, e)| *e == harness::DataEvent::Ended(EndReason::StoppedByClient))
    };
    let second_stopped = outcome.other_servers_events[0]
        .iter()
        .any(|e| matches!(e, Event::TestConnectionReleased { .. }));
    outcome.check(
        stopped(&outcome.server_data_events) && second_stopped,
        "both Servers' Test Connections ended",
    );
}

// A maxBandwidth below 1 Mbps a Test Connection can't be shared: the
// Coordinator refuses it rather than state 1 Mbps each, more than was
// asked for (conformance: max-bandwidth-split-across-test-connections)
#[test]
#[should_panic(expected = "a maxBandwidth of at least 1 Mbps a Test Connection")]
fn a_max_bandwidth_below_1_mbps_a_test_connection_is_refused() {
    test(12, 3, 3)
        .client_max_bandwidth(2)
        .run_for(Duration::from_millis(100));
}

// RFC 9946 §6.1 (maxBandwidth per Test Connection); docs/spec.md story 54
// (conformance: max-bandwidth-split-across-test-connections)
#[test]
fn the_max_bandwidth_is_shared_between_the_test_connections() {
    let outcome = test(8, 3, 3)
        .client_max_bandwidth(10)
        .run_for(Duration::from_millis(500));
    let stated: Vec<u16> = setup_requests(&outcome)
        .iter()
        .map(|t| u16::from_be_bytes([t.payload[10], t.payload[11]]))
        .collect();
    outcome.check(
        stated == [3, 3, 3],
        &format!("each states 3 Mbps: {stated:?}"),
    );
}

// RFC 9946 §6.1 (the Test initiation timer); : a shorter time
// limit still leaves the Activations time
#[test]
fn a_short_time_limit_still_activates_every_test_connection() {
    let outcome = test(9, 3, 3)
        .client_time_limit(Duration::from_secs(1))
        .path_delay(Duration::from_millis(100), Duration::ZERO)
        .run_for(Duration::from_secs(2));
    outcome.check(activated(&outcome).len() == 3, "all three activated");
}

// docs/spec.md (a Test with one Test Connection starts as before)
#[test]
fn one_test_connection_activates_as_soon_as_its_setup_succeeds() {
    let outcome = test(10, 1, 1).run_for(Duration::from_millis(500));
    let setup = setup_requests(&outcome)[0].at;
    let activation = activation_requests(&outcome);
    outcome.check(
        activation.len() == 1 && activation[0].at == setup && activated(&outcome).len() == 1,
        "one Test Connection, activated",
    );
}

// docs/spec.md story 56 (an aborted Test sends no stray load); RFC 9946 §9 (the
// Client ends a Test with STOP)
#[udpstp_req::req("RFC9946-4-L1", "RFC9946-7.3-L1")]
#[test]
fn an_acceptance_that_arrives_after_the_abort_is_stopped_at_once() {
    // The first Server coerces DSCP 46 to 0, which the Client refuses: the
    // Test aborts while the second Server's acceptance is on its way.
    let outcome = Scenario::new(11)
        .mode1(3, KEY)
        .add_server(|c| c.allowed_dscp = DscpSet::ALL)
        .client_config(|c| c.dscp = 46)
        .client_config(|c| {
            c.unacceptable_coercions
                .push(harness::ActivationParameter::DscpEcn)
        })
        .coordinated(2, 2)
        .run_for(Duration::from_millis(200));
    outcome.check(
        aborted(&outcome) == Some(TestAbort::TestConnectionEnded { mc_index: 0 })
            && activated(&outcome).is_empty(),
        "the Test aborted, and no Data Phase started",
    );
    let second = &outcome.other_servers_events[0];
    outcome.check(
        second
            .iter()
            .any(|e| matches!(e, Event::TestConnectionActivated { .. }))
            && second
                .iter()
                .any(|e| matches!(e, Event::TestConnectionReleased { .. })),
        "the second Server's Test Connection was activated, then stopped and released",
    );
}
