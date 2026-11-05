//! The share of Test Connections held unactivated: Test
//! Connections admitted but not yet activated have a separate share,
//! half the global cap by default. A new Setup beyond it closes the oldest
//! Unactivated Test Connection of the Key Table row holding the most,
//! Unauthenticated Mode counting as one group, instead of being refused,
//! as a SYN cache evicts, so Setup Requests never activated can't fill the
//! global cap. A key flooding authentic ones then evicts its own, and
//! another key's Client keeps its place however long its round trip; a
//! Client of the flooding key does not (docs/security.md). The share never
//! drops below the per-Test cap, so a whole Multi-Connection Test can be
//! set up.

mod harness;

use std::collections::HashMap;
use std::time::Duration;

use harness::requests::{self, Auth, TestConnection};
use harness::{Outcome, Scenario, ScriptClock, ServerConfig};
use udpstp_core::{Direction, Event, SocketId};

const ACTIVATION: [u8; 2] = [0xAC, 0xE2];

fn evicted(outcome: &Outcome) -> usize {
    outcome
        .server_events
        .iter()
        .filter(|e| matches!(e, Event::UnactivatedTestConnectionEvicted { .. }))
        .count()
}

fn refused(outcome: &Outcome) -> usize {
    outcome
        .server_events
        .iter()
        .filter(|e| matches!(e, Event::SetupRequestRejected { .. }))
        .count()
}

/// Tests from one Source, one Test Connection each, starting 10 ms apart.
fn staggered(seed: u64, tests: u64, change: fn(&mut ServerConfig)) -> Scenario {
    let mut scenario = Scenario::new(seed)
        .operator(7, b"an operator key")
        .server_tests_per_source(10)
        .server_config(change);
    for k in 1..tests {
        scenario = scenario
            .add_client_in_another_test_at(Direction::Downstream, Duration::from_millis(10 * k));
    }
    scenario
}

fn four_connections_one_per_test(c: &mut ServerConfig) {
    c.max_test_connections = 4;
    c.test_connections_per_test = 1;
}

// The unactivated share defaults to half the global cap;
// a new Setup beyond it closes the oldest.
#[udpstp_req::req("RFC9946-11-4", "RFC9097-10-4")]
#[test]
fn a_new_setup_evicts_the_oldest_unactivated_test_connection() {
    // The Activation Requests are lost, so every Test Connection stays
    // unactivated: of 4, 2 may wait, and the third Setup closes the first.
    let outcome = staggered(1, 3, four_connections_one_per_test)
        .network_drops(ACTIVATION, Duration::ZERO, None)
        .run_for(Duration::from_millis(100));
    outcome.check(
        evicted(&outcome) == 1
            && refused(&outcome) == 0
            && outcome.server_admitted_count() == 3
            && outcome.server_released_count() == 1,
        &format!("{:?}", outcome.server_events),
    );
}

const CLIENT_KEY: &[u8] = b"the Client's Shared Key";
const FLOOD_KEY: &[u8] = b"the flooding Shared Key";

/// The keyId of the Test Connection admitted on each socket.
fn key_ids(outcome: &Outcome) -> HashMap<SocketId, Option<u8>> {
    outcome
        .server_events
        .iter()
        .filter_map(|e| match e {
            Event::TestConnectionAdmitted { socket, key_id, .. } => Some((*socket, *key_id)),
            _ => None,
        })
        .collect()
}

/// The keyIds of the Test Connections evicted, in order.
fn evicted_key_ids(outcome: &Outcome) -> Vec<Option<u8>> {
    let key_ids = key_ids(outcome);
    outcome
        .server_events
        .iter()
        .filter_map(|e| match e {
            Event::UnactivatedTestConnectionEvicted { socket } => Some(key_ids[socket]),
            _ => None,
        })
        .collect()
}

/// Our Client, 100 ms away each way, runs a Test while Setup Requests
/// signed as `auth` says and never activated arrive every 10 ms: 100 a
/// second, which would close the oldest of a share of 4 after about 40 ms,
/// well before the Client's Activation Request arrives, 200 ms after its
/// Setup Response went out.
fn flooded(scenario: Scenario, auth: Auth) -> Outcome {
    let mut scenario = scenario
        .path_delay(Duration::from_millis(100), Duration::ZERO)
        .server_tests_per_source(100)
        .server_config(|c| {
            c.max_test_connections = 8;
            c.test_connections_per_test = 1;
            c.source_setup_burst = 1000;
            c.source_setups_per_second = 1000;
        });
    for k in 0..50u16 {
        let tc = TestConnection {
            mc_ident: 1000 + k,
            mc_index: 0,
            mc_count: 1,
            max_bandwidth: 0,
        };
        scenario = scenario.add_scripted_client(
            Duration::from_millis(10 * u64::from(k)),
            move |clock: ScriptClock| {
                requests::setup_request(tc, Direction::Downstream, auth, clock.unix_secs as u32)
            },
            |_, _| None,
        );
    }
    scenario.run_for(Duration::from_millis(1000))
}

//  (a key flooding authentic Setup Requests at the share's rate evicts
// its own Unactivated Test Connections, not another key's Client's, which
// activates however long its round trip)
// (conformance: unactivated-test-connection-share)
#[test]
fn another_keys_flood_cannot_evict_a_clients_unactivated_test_connection() {
    let auth = Auth::Mode1 {
        key_id: 7,
        key: FLOOD_KEY,
    };
    let scenario = Scenario::new(11)
        .mode1(9, CLIENT_KEY)
        .server_key(7, FLOOD_KEY);
    let outcome = flooded(scenario, auth);
    let evicted = evicted_key_ids(&outcome);
    outcome.check(
        outcome.client_activated_direction(0).is_some()
            && evicted.len() > 10
            && evicted.iter().all(|&key_id| key_id == Some(7)),
        &format!("{evicted:?} {:?}", outcome.server_events),
    );
}

//  (Unauthenticated Mode's Unactivated Test Connections are one group:
// an unauthenticated flood evicts its own, not a keyed Client's)
// (conformance: unactivated-test-connection-share)
#[test]
fn an_unauthenticated_flood_cannot_evict_a_keyed_clients_unactivated_test_connection() {
    let scenario = Scenario::new(12)
        .mode1(9, CLIENT_KEY)
        .server_allows_unauthenticated();
    let outcome = flooded(scenario, Auth::Unauthenticated);
    let evicted = evicted_key_ids(&outcome);
    outcome.check(
        outcome.client_activated_direction(0).is_some()
            && evicted.len() > 10
            && evicted.iter().all(|&key_id| key_id.is_none()),
        &format!("{evicted:?} {:?}", outcome.server_events),
    );
}

//  (the trust-model limit docs/security.md states: a flood on the
// Client's own key still evicts it before it activates)
#[test]
fn a_flood_on_the_clients_own_key_still_evicts_it() {
    let auth = Auth::Mode1 {
        key_id: 9,
        key: CLIENT_KEY,
    };
    let scenario = Scenario::new(13).mode1(9, CLIENT_KEY);
    let outcome = flooded(scenario, auth);
    outcome.check(
        outcome.client_activated_direction(0).is_none() && evicted(&outcome) > 10,
        &format!("{:?}", outcome.server_events),
    );
}
