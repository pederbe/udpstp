//! The per-key budget: each Key Table row has a budget of Setup
//! Requests, charged only once a request passed the HMAC and the replay
//! check, Unauthenticated Mode counting as one group. A key holder's flood
//! of authentic Setup Requests then costs the Server at most its key's
//! budget in admissions, the rest dropped silently as cheaply as a forged
//! one, so it can't saturate the control thread and keep other keys'
//! Clients out.

mod harness;

use std::time::Duration;

use harness::requests::{self, Auth, TestConnection};
use harness::{Outcome, Scenario, ScriptClock, SilentRejectionReason};
use udpstp_core::{Direction, Event};

const CLIENT_KEY: &[u8] = b"the Client's Shared Key";
const FLOOD_KEY: &[u8] = b"the flooding Shared Key";
const SETUP: [u8; 2] = [0xAC, 0xE1];

fn dropped_over_key_budget(outcome: &Outcome) -> usize {
    outcome
        .server_events
        .iter()
        .filter(|e| {
            matches!(
                e,
                Event::SetupRequestSilentlyRejected {
                    reason: SilentRejectionReason::KeyBudgetExhausted,
                    ..
                }
            )
        })
        .count()
}

/// 50 Setup Requests signed as `auth` says, never activated, one every
/// 10 ms from Sources whose own budgets don't limit them, beside our
/// Client's Test; each key holds a burst of 3 and gains 10 a second.
fn flooded(scenario: Scenario, auth: Auth) -> Outcome {
    let mut scenario = scenario.server_tests_per_source(100).server_config(|c| {
        c.test_connections_per_test = 1;
        c.source_setup_burst = 1000;
        c.source_setups_per_second = 1000;
        c.key_setup_burst = 3;
        c.key_setups_per_second = 10;
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

//  (a key's authentic Setup Requests beyond its budget are dropped
// silently, before admission)
// (conformance: key-setup-budget)
#[test]
fn a_keys_setup_requests_over_its_budget_are_dropped_silently() {
    let auth = Auth::Mode1 {
        key_id: 7,
        key: FLOOD_KEY,
    };
    let scenario = Scenario::new(21)
        .mode1(9, CLIENT_KEY)
        .server_key(7, FLOOD_KEY);
    let outcome = flooded(scenario, auth);
    // Over the flood's 0.49 s: the burst of 3 and 4 more, and our
    // Client's own Test Connection, on its own key's budget.
    let admitted = outcome.server_admitted_count();
    outcome.check(
        (7..=9).contains(&admitted) && dropped_over_key_budget(&outcome) >= 40,
        &format!("{admitted} {:?}", outcome.server_events),
    );
}

//  (Operator Keys aren't exempt: an Operator Key's flood spends its
// row's budget too)
// (conformance: key-setup-budget)
#[test]
fn an_operator_keys_setup_requests_spend_its_budget_too() {
    let auth = Auth::Mode1 {
        key_id: 7,
        key: FLOOD_KEY,
    };
    let scenario = Scenario::new(25)
        .mode1(9, CLIENT_KEY)
        .server_operator_key(7, FLOOD_KEY, None);
    let outcome = flooded(scenario, auth);
    outcome.check(
        dropped_over_key_budget(&outcome) >= 40,
        &format!("{:?}", outcome.server_events),
    );
}

//  (each key's budget is its own: a flood on one key leaves another
// key's Client its budget, and it starts its Test)
// (conformance: key-setup-budget)
#[test]
fn a_flood_on_one_key_leaves_another_keys_budget_alone() {
    let auth = Auth::Mode1 {
        key_id: 7,
        key: FLOOD_KEY,
    };
    let scenario = Scenario::new(22)
        .mode1(9, CLIENT_KEY)
        .server_key(7, FLOOD_KEY);
    let outcome = flooded(scenario, auth);
    outcome.check(
        outcome.client_activated_direction(0).is_some(),
        &format!("{:?}", outcome.server_events),
    );
}

//  (Unauthenticated Mode's Setup Requests share one budget, as they
// share one group in eviction)
// (conformance: key-setup-budget)
#[test]
fn unauthenticated_setup_requests_share_one_budget() {
    let scenario = Scenario::new(23)
        .mode1(9, CLIENT_KEY)
        .server_allows_unauthenticated();
    let outcome = flooded(scenario, Auth::Unauthenticated);
    outcome.check(
        dropped_over_key_budget(&outcome) >= 40 && outcome.client_activated_direction(0).is_some(),
        &format!("{:?}", outcome.server_events),
    );
}

//  (a replay is dropped before it reaches its key's budget, so a
// replayer, who holds no key, can't spend a key's budget)
// (conformance: key-setup-budget)
#[test]
fn a_replayed_setup_request_spends_no_keys_budget() {
    // The Client's Setup Request spends its key's only token, which takes
    // a second to come back; its copy, half a second later, is dropped as
    // a replay, not over the key's budget.
    let outcome = Scenario::new(24)
        .operator(7, b"an operator key")
        .server_config(|c| {
            c.test_connections_per_test = 1;
            c.key_setup_burst = 1;
            c.key_setups_per_second = 1;
        })
        .network_replays(SETUP, 0, Duration::from_millis(500))
        .run_for(Duration::from_millis(600));
    outcome.check(
        outcome.server_dropped_setup_because(SilentRejectionReason::Replayed)
            && dropped_over_key_budget(&outcome) == 0,
        &format!("{:?}", outcome.server_events),
    );
}
