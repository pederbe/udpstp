//! The global budget for unverified Setup Requests (docs/spec.md "Rate limits",
//! story 85): beyond it, Setup Requests whose keyId has a row are dropped
//! silently before any KDF run or HMAC. The Derived Key cache that makes a
//! flood sharing a Key Table row and an authUnixTime cost one KDF run is
//! tested with its module (crates/udpstp-core/src/budget.rs).

mod harness;

use std::time::Duration;

use harness::requests::{self, Auth, TestConnection};
use harness::{ClientSpec, KeyTable, Scenario, ScriptClock, ServerConfig, SilentRejectionReason};
use udpstp_core::{ClientAuth, Direction, Event};

const KEY: &[u8] = b"an operator key";

fn budget_dropped(outcome: &harness::Outcome) -> usize {
    outcome
        .server_events
        .iter()
        .filter(|e| {
            matches!(
                e,
                Event::SetupRequestSilentlyRejected {
                    reason: SilentRejectionReason::UnverifiedBudgetExhausted,
                    ..
                }
            )
        })
        .count()
}

// docs/spec.md story 85 (floods of unverified Setup Requests are dropped
// against a global budget)
#[test]
fn setup_requests_beyond_the_budget_are_dropped_silently() {
    let outcome = Scenario::new(1)
        .operator(7, b"an operator key")
        .server_tests_per_source(3)
        .server_config(|c| c.unverified_setups_per_second = 2)
        .add_client_in_another_test(Direction::Downstream)
        .add_client_in_another_test(Direction::Downstream)
        .run_for(Duration::from_millis(100));
    let responses = outcome
        .trace
        .iter()
        .filter(|t| t.payload.starts_with(&[0xAC, 0xE1]) && t.payload[8] == 2)
        .count();
    outcome.check(
        budget_dropped(&outcome) == 1 && outcome.server_admitted_count() == 2 && responses == 2,
        &format!("{:?}", outcome.server_events),
    );
}

// docs/spec.md (the budget renews every second)
#[test]
fn the_budget_renews_each_second() {
    let outcome = Scenario::new(2)
        .operator(7, b"an operator key")
        .server_tests_per_source(2)
        .server_config(|c| c.unverified_setups_per_second = 1)
        .add_client_in_another_test_at(Direction::Downstream, Duration::from_millis(1000))
        .run_for(Duration::from_millis(1100));
    outcome.check(
        budget_dropped(&outcome) == 0 && outcome.server_admitted_count() == 2,
        &format!("{:?}", outcome.server_events),
    );
}

#[test]
fn the_budget_is_20000_a_second_by_default() {
    assert_eq!(ServerConfig::default().unverified_setups_per_second, 20_000);
}

//  (the budget stays a fixed one-second window because it admits
// Setup Requests that arrive together all together; Conformance Record
// unverified-setup-budget): a Multi-Connection Test's Setup Requests,
// which arrive together, pass whole while the window has room for them
// and fail whole once it is spent, where a budget refilling continuously
// would let some of them through. With a budget of 8, the Coordinator's
// Test of four opens the window, a scripted Test of four fills it 100 ms
// later, and another 100 ms after that finds it spent.
#[test]
fn setup_requests_that_arrive_together_pass_or_fail_together() {
    const FILLS: u16 = 0x0B0B;
    const FINDS_IT_SPENT: u16 = 0x0C0C;
    let auth = Auth::Mode1 {
        key_id: 7,
        key: KEY,
    };
    // Four scripted Clients sending the Setup Requests of Test `mc_ident`
    // together at `at`.
    let scripted_test = |mut scenario: Scenario, mc_ident: u16, at: Duration| {
        for mc_index in 0..4 {
            let tc = TestConnection {
                mc_ident,
                mc_index,
                mc_count: 4,
                max_bandwidth: 0,
            };
            scenario = scenario.add_scripted_client(
                at,
                move |clock: ScriptClock| {
                    requests::setup_request(tc, Direction::Downstream, auth, clock.unix_secs as u32)
                },
                |_, _| None,
            );
        }
        scenario
    };
    let scenario = Scenario::new(5)
        .operator(7, KEY)
        .server_tests_per_source(3)
        .server_config(|c| c.unverified_setups_per_second = 8)
        .coordinated(4, 4);
    let scenario = scripted_test(scenario, FILLS, Duration::from_millis(100));
    let outcome = scripted_test(scenario, FINDS_IT_SPENT, Duration::from_millis(200))
        .run_for(Duration::from_millis(500));
    let admitted = |test: &dyn Fn(u16) -> bool| {
        outcome
            .server_events
            .iter()
            .filter(
                |e| matches!(e, Event::TestConnectionAdmitted { mc_ident, .. } if test(*mc_ident)),
            )
            .count()
    };
    let coordinators = admitted(&|ident| ident != FILLS && ident != FINDS_IT_SPENT);
    let fills = admitted(&|ident| ident == FILLS);
    let finds_it_spent = admitted(&|ident| ident == FINDS_IT_SPENT);
    outcome.check(
        (coordinators, fills, finds_it_spent) == (4, 4, 0) && budget_dropped(&outcome) == 4,
        &format!("{:?}", outcome.server_events),
    );
}

// Requests that fail the checks cheaper than the budget, such as an unknown
// keyId, don't spend it.
#[test]
fn a_request_with_an_unknown_key_id_doesnt_spend_the_budget() {
    let mut unknown = KeyTable::default();
    unknown
        .insert(harness::always_valid(9, b"a key the Server lacks"))
        .expect("one row");
    let outcome = Scenario::new(3)
        .operator(7, b"an operator key")
        .server_tests_per_source(2)
        .server_config(|c| c.unverified_setups_per_second = 1)
        .add_client_with(ClientSpec {
            own_test: true,
            auth: Some(ClientAuth::Mode1 {
                keys: unknown,
                key_id: None,
            }),
            ..ClientSpec::in_same_test(Direction::Downstream)
        })
        .run_for(Duration::from_millis(100));
    outcome.check(
        budget_dropped(&outcome) == 0 && outcome.server_admitted_count() == 1,
        &format!("{:?}", outcome.server_events),
    );
}

// An allowed request in Unauthenticated Mode is admitted without any check,
// so it spends the budget too.
#[test]
fn an_unauthenticated_request_spends_the_budget() {
    let outcome = Scenario::new(4)
        .server_allows_unauthenticated()
        .client_unauthenticated()
        .server_tests_per_source(2)
        .server_config(|c| c.unverified_setups_per_second = 1)
        .add_client_in_another_test(Direction::Downstream)
        .run_for(Duration::from_millis(100));
    outcome.check(
        budget_dropped(&outcome) == 1 && outcome.server_admitted_count() == 1,
        &format!("{:?}", outcome.server_events),
    );
}
