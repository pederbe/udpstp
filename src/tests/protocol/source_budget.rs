//! The per-Source budget (docs/spec.md "Rate limits", story 86): each Source
//! has a budget of Setup Requests, charged only once a request passed the
//! HMAC, so a spoofed Source can't spend another's. Its burst holds at
//! least a Multi-Connection Test's Test Connections, and an authenticated
//! request over it gets a signed CONNFAIL instead of silence.

use super::harness;

use std::time::Duration;

use crate::protocol::{ClientAuth, Direction, Event, SetupRejection};
use crate::wire::auth::{derive_keys, hmac_sha256};
use harness::{ClientSpec, KeyTable, Outcome, Scenario, ServerConfig, SilentRejectionReason};

const CONNFAIL: u8 = 13;
const KEY: &[u8] = b"an operator key";

fn over_budget(outcome: &Outcome) -> usize {
    outcome
        .server_events
        .iter()
        .filter(|e| {
            matches!(
                e,
                Event::SetupRequestRejected {
                    reason: SetupRejection::SourceBudgetExhausted,
                    ..
                }
            )
        })
        .count()
}

/// `tests` Tests from one Source, each of one Test Connection, at `at`.
fn tests_from_one_source(seed: u64, tests: usize, at: Duration) -> Scenario {
    let mut scenario = Scenario::new(seed)
        .operator(7, KEY)
        .server_tests_per_source(tests + 1);
    for _ in 1..tests {
        scenario = scenario.add_client_in_another_test_at(Direction::Downstream, at);
    }
    scenario
}

// docs/spec.md story 86 (an authenticated request over its Source's budget
// gets a signed rejection)
#[udpstp_req::req("RFC9946-6.2.1-3")]
#[test]
fn a_request_over_its_sources_budget_gets_a_signed_connfail() {
    let outcome = tests_from_one_source(1, 3, Duration::ZERO)
        .server_config(|c| {
            c.test_connections_per_test = 2;
            c.source_setup_burst = 2;
            c.source_setups_per_second = 1;
        })
        .run_for(Duration::from_millis(100));
    // The refused request's response, signed with the Server key of its
    // own authUnixTime.
    let refusal = outcome
        .trace
        .iter()
        .find(|t| {
            t.payload.starts_with(&[0xAC, 0xE1]) && t.payload[8] == 2 && t.payload[9] == CONNFAIL
        })
        .map(|t| t.payload.clone());
    let signed = refusal.as_ref().is_some_and(|r| {
        let time = u32::from_be_bytes(r[16..20].try_into().unwrap());
        let mut zeroed = r.clone();
        zeroed[20..52].fill(0);
        zeroed[54..56].fill(0);
        hmac_sha256(derive_keys(KEY, time).server(), &zeroed) == r[20..52]
    });
    outcome.check(
        over_budget(&outcome) == 1 && outcome.server_admitted_count() == 2 && signed,
        &format!("{:?}", outcome.server_events),
    );
}

// docs/spec.md story 86 (the burst allowance is at least the per-Test
// connection cap, so a Multi-Connection Test isn't blocked)
#[test]
fn a_whole_multi_connection_test_fits_the_default_burst() {
    let outcome = Scenario::new(2)
        .operator(7, KEY)
        .coordinated(24, 24)
        .run_for(Duration::from_millis(100));
    outcome.check(
        over_budget(&outcome) == 0 && outcome.server_admitted_count() == 24,
        &format!("{:?}", outcome.server_events),
    );
    let defaults = ServerConfig::default();
    assert!(defaults.source_setup_burst as usize >= defaults.test_connections_per_test);
}

// docs/spec.md (per-Source budgets are charged only after the HMAC check:
// requests that fail it, as a spoofed Source's would, spend nothing)
#[test]
fn requests_that_fail_the_hmac_dont_spend_the_budget() {
    // Five requests with the wrong key come first; the right one 10 ms
    // later.
    let mut wrong = KeyTable::default();
    wrong
        .insert(harness::always_valid(7, b"not the key"))
        .expect("one row");
    let mut right = KeyTable::default();
    right
        .insert(harness::always_valid(7, KEY))
        .expect("one row");
    let mut scenario = Scenario::new(3)
        .operator(7, KEY)
        .client_keys(wrong, None)
        .server_tests_per_source(10)
        .server_config(|c| {
            c.test_connections_per_test = 1;
            c.source_setup_burst = 1;
        });
    for _ in 0..4 {
        scenario = scenario.add_client_in_another_test(Direction::Downstream);
    }
    let outcome = scenario
        .add_client_with(ClientSpec {
            own_test: true,
            start_at: Duration::from_millis(10),
            auth: Some(ClientAuth::Mode1 {
                keys: right,
                key_id: None,
            }),
            ..ClientSpec::in_same_test(Direction::Downstream)
        })
        .run_for(Duration::from_millis(100));
    outcome.check(
        over_budget(&outcome) == 0 && outcome.client_activated_direction(5).is_some(),
        &format!("{:?}", outcome.server_events),
    );
}

// docs/spec.md story 84 (a replay, dropped by the replay cache, spends
// nothing)
#[test]
fn a_replayed_request_doesnt_spend_the_budget() {
    let outcome = tests_from_one_source(6, 2, Duration::from_millis(10))
        .server_config(|c| {
            c.test_connections_per_test = 1;
            c.source_setup_burst = 2;
            c.source_setups_per_second = 1;
        })
        .network_duplicates_setup_requests()
        .run_for(Duration::from_millis(100));
    outcome.check(
        over_budget(&outcome) == 0 && outcome.server_admitted_count() == 2,
        &format!("{:?}", outcome.server_events),
    );
}

// Only the first refusal since the Source last spent a token is answered,
// so a key holder can't make the Server reflect a signed rejection for
// each request.
#[test]
fn only_the_first_refusal_is_answered() {
    let outcome = tests_from_one_source(7, 4, Duration::ZERO)
        .server_config(|c| {
            c.test_connections_per_test = 1;
            c.source_setup_burst = 1;
            c.source_setups_per_second = 1;
        })
        .run_for(Duration::from_millis(100));
    let silent = outcome
        .server_events
        .iter()
        .filter(|e| {
            matches!(
                e,
                Event::SetupRequestSilentlyRejected {
                    reason: SilentRejectionReason::SourceBudgetExhausted,
                    ..
                }
            )
        })
        .count();
    outcome.check(
        over_budget(&outcome) == 1 && silent == 2 && outcome.server_admitted_count() == 1,
        &format!("{:?}", outcome.server_events),
    );
}

// docs/spec.md (the budget refills over time)
#[test]
fn the_budget_refills() {
    let run = |second_at| {
        tests_from_one_source(4, 2, second_at)
            .server_config(|c| {
                c.test_connections_per_test = 1;
                c.source_setup_burst = 1;
                c.source_setups_per_second = 1;
            })
            .run_for(second_at + Duration::from_millis(100))
    };
    let early = run(Duration::from_millis(500));
    early.check(
        over_budget(&early) == 1,
        &format!("{:?}", early.server_events),
    );
    let late = run(Duration::from_millis(1000));
    late.check(
        over_budget(&late) == 0,
        &format!("{:?}", late.server_events),
    );
}

// Unauthenticated requests have no HMAC, so they don't spend a Source's
// budget: a spoofed one could spend another's.
#[test]
fn unauthenticated_requests_dont_spend_the_budget() {
    let outcome = Scenario::new(5)
        .server_allows_unauthenticated()
        .client_unauthenticated()
        .server_tests_per_source(3)
        .server_config(|c| {
            c.test_connections_per_test = 1;
            c.source_setup_burst = 1;
        })
        .add_client_in_another_test(Direction::Downstream)
        .add_client_in_another_test(Direction::Downstream)
        .run_for(Duration::from_millis(100));
    outcome.check(
        over_budget(&outcome) == 0 && outcome.server_admitted_count() == 3,
        &format!("{:?}", outcome.server_events),
    );
}
