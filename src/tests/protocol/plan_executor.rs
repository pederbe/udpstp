//! The plan-executor contract against the simulator's executor (docs/spec.md
//! story 119; S1). `contract/mod.rs` says what it checks; the real
//! Data Phase Engine meets the same contract on loopback
//! (`src/runtime/contract_tests.rs`).

use super::contract;
use super::harness;

use std::time::Duration;

use crate::protocol::{DataEvent, Direction};
use contract::{Observed, Schedule, Tolerance};
use harness::{Outcome, RowRequest, Scenario};

/// A Downstream Fixed-Rate Test at `row`, whose Server is the Load Sender.
fn fixed_rate(seed: u64, row: u16) -> Scenario {
    Scenario::new(seed)
        .operator(7, b"an operator key")
        .client_direction(Direction::Downstream)
        .client_requests_row(RowRequest::FixedRate(row))
}

/// The Server's Load PDUs as the Client saw them: the simulated path has
/// no delay, and the wall clocks start at WALL_START_UNIX_SECS.
fn observed(outcome: &Outcome) -> Vec<Observed> {
    let start = Duration::from_secs(harness::WALL_START_UNIX_SECS);
    outcome
        .trace
        .iter()
        .filter(|t| t.delivered)
        .filter_map(|t| Observed::load_pdu(start + t.at, &t.payload, t.len))
        .collect()
}

/// The schedule of a Transmitter every `interval`, with what the Server
/// reported sent.
fn schedule(outcome: &Outcome, interval: Duration) -> Schedule {
    let sts: Vec<_> = outcome
        .server_data_events
        .iter()
        .filter_map(|(_, e)| match e {
            DataEvent::Sent(sent) => Some(*sent),
            _ => None,
        })
        .collect();
    Schedule {
        interval,
        first_slot: None,
        reported_datagrams: sts.iter().map(|s| u64::from(s.datagrams)).sum(),
        reported_bytes: sts.iter().map(|s| s.bytes).sum(),
        per_st: sts
            .iter()
            .map(|s| u64::from(s.datagrams))
            .max()
            .unwrap_or(0),
        skipped_bursts: 0,
    }
}

fn meets(outcome: &Outcome, interval: Duration, tolerance: Tolerance) {
    let failures = contract::check(&observed(outcome), schedule(outcome, interval), tolerance);
    outcome.check(failures.is_empty(), &format!("{failures:#?}"));
}

// Bursts every 1 ms (row 40: four Load PDUs a Burst) and every 100 µs
// (row 500: five), each sent at its slot
#[udpstp_req::req("RFC9097-8.1-L2", "RFC9946-7.1-L8")]
#[test]
fn the_simulators_executor_meets_the_contract() {
    for (row, interval) in [(40, 1000), (500, 100)] {
        let outcome = fixed_rate(1, row).run_for(Duration::from_millis(400));
        meets(&outcome, Duration::from_micros(interval), Tolerance::EXACT);
    }
}

// Partial sends leave Deferred Datagrams, which lead the next Send
// Plan in order, and are reported as sent only once they go
#[test]
fn the_simulators_executor_meets_the_contract_with_partial_sends() {
    let outcome = fixed_rate(2, 40)
        .partial_sends(10, 1)
        .run_for(Duration::from_millis(400));
    meets(&outcome, Duration::from_millis(1), Tolerance::EXACT);
    // A host that takes none of every 10th Send Plan: a Withheld Burst for
    // each, whose slot goes without a Send Plan.
    let outcome = fixed_rate(3, 40)
        .partial_sends(10, 0)
        .run_for(Duration::from_millis(400));
    let tolerance = Tolerance {
        missing: 0.15,
        ..Tolerance::EXACT
    };
    meets(&outcome, Duration::from_millis(1), tolerance);
}
