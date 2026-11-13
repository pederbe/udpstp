//! The harness itself: runs are reproducible from their seed, and failures
//! say how to reproduce them.

use super::harness;

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Duration;

use harness::Scenario;

fn accepted_run(seed: u64) -> harness::Outcome {
    Scenario::new(seed)
        .server_allows_unauthenticated()
        .client_unauthenticated()
        .run_for(Duration::from_secs(1))
}

#[test]
fn the_same_seed_replays_the_same_trace() {
    assert_eq!(accepted_run(7).trace_hash(), accepted_run(7).trace_hash());
    assert_eq!(accepted_run(7).trace, accepted_run(7).trace);
}

#[test]
fn different_seeds_give_different_traces() {
    assert_ne!(accepted_run(7).trace_hash(), accepted_run(8).trace_hash());
}

//  (docs/spec.md, 2026-09-28: the simulator runs the Multi-Connection
// Coordinator that production runs): a Client alone in its Test runs one,
// whose one Test Connection sends from the Client's address.
#[test]
fn a_client_runs_a_multi_connection_coordinator_from_its_address() {
    let outcome = accepted_run(9);
    let first_test_connection = crate::protocol::SocketId(1);
    outcome.check(
        !outcome.client_connection_events.is_empty()
            && outcome
                .client_connection_events
                .iter()
                .all(|(socket, _)| *socket == first_test_connection),
        &format!("{:?}", outcome.client_connection_events),
    );
    outcome.check(
        outcome
            .sent_by(harness::CLIENT_ADDR)
            .first()
            .is_some_and(|setup| setup.payload.starts_with(&[0xAC, 0xE1])),
        "the Setup Request leaves from the Client's address",
    );
}

#[test]
fn a_failed_expectation_reports_the_seed_and_trace_hash() {
    let outcome = accepted_run(42);
    let hash = format!("{:016x}", outcome.trace_hash());

    let panic = catch_unwind(AssertUnwindSafe(|| outcome.check(false, "deliberate")))
        .expect_err("check(false) must fail the test");
    let message = panic
        .downcast_ref::<String>()
        .expect("the failure message is a String");

    assert!(message.contains("deliberate"), "{message}");
    assert!(message.contains("seed: 42"), "{message}");
    assert!(message.contains(&hash), "{message}");
}
