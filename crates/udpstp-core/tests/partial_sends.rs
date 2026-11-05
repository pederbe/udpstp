//! Partial sends (docs/spec.md "Data Phase"; ADR 0008; GLOSSARY.md Deferred
//! Datagram, Withheld Burst): when the host takes only part of a Send Plan,
//! the rest are Deferred Datagrams that lead the next Send Plan with their
//! sequence numbers and a fresh lpduTime, so host backpressure never shows
//! as loss. While a whole Burst is still deferred, the next new Burst is
//! withheld and reported as a Withheld Burst.

mod harness;

use std::time::Duration;

use harness::{Outcome, RowRequest, Scenario, TraceEntry};
use udpstp_core::{DataEvent, Direction};
use udpstp_wire::StatusPdu;

/// A Downstream Fixed-Rate Test at 40 Mbit/s: four datagrams every 1 ms.
fn downstream(seed: u64) -> Scenario {
    Scenario::new(seed)
        .operator(7, b"an operator key")
        .client_direction(Direction::Downstream)
        .client_requests_row(RowRequest::FixedRate(40))
}

fn loads(outcome: &Outcome) -> Vec<&TraceEntry> {
    outcome
        .trace
        .iter()
        .filter(|t| t.payload.starts_with(&[0xBE, 0xEF]))
        .collect()
}

fn seq(t: &TraceEntry) -> u32 {
    u32::from_be_bytes([t.payload[4], t.payload[5], t.payload[6], t.payload[7]])
}

/// The loss, out-of-order and duplicate counts all Status PDUs reported.
fn reported_errors(outcome: &Outcome) -> (u32, u32, u32) {
    outcome
        .trace
        .iter()
        .filter(|t| t.len == 204 && t.payload.starts_with(&[0xFE, 0xED]))
        .filter_map(|t| StatusPdu::decode(&t.payload).ok())
        .fold((0, 0, 0), |(l, o, d), s| {
            (l + s.seq_err_loss, o + s.seq_err_ooo, d + s.seq_err_dup)
        })
}

/// Bursts withheld, and skipped, by the Server.
fn withheld_and_skipped(outcome: &Outcome) -> (u32, u32) {
    outcome
        .server_data_events
        .iter()
        .fold((0, 0), |(w, s), (_, e)| match e {
            DataEvent::BurstsWithheld { bursts, .. } => (w + bursts, s),
            DataEvent::BurstsSkipped { bursts, .. } => (w, s + bursts),
            _ => (w, s),
        })
}

/// Whether every Load PDU from `from` came once, in order, from 1.
fn contiguous(outcome: &Outcome, from: std::net::SocketAddr) -> bool {
    let seqs: Vec<u32> = loads(outcome)
        .iter()
        .filter(|t| t.source == from)
        .map(|t| seq(t))
        .collect();
    !seqs.is_empty() && seqs[0] == 1 && seqs.windows(2).all(|w| w[1] == w[0] + 1)
}

// docs/spec.md  (unsent datagrams go out in the next Burst with their
// sequence numbers; the receiver sees no loss caused by partial sends)
#[test]
fn deferred_datagrams_keep_their_sequence_numbers_and_count_no_loss() {
    // Every 10th Send Plan, the host takes one datagram of four.
    let outcome = downstream(1)
        .partial_sends(10, 1)
        .run_for(Duration::from_millis(600));

    let seqs: Vec<u32> = loads(&outcome).iter().map(|t| seq(t)).collect();
    let in_order = seqs.windows(2).all(|w| w[1] == w[0] + 1);
    let errors = reported_errors(&outcome);
    outcome.check(
        seqs.len() > 1000 && seqs.first() == Some(&1) && in_order && errors == (0, 0, 0),
        &format!(
            "{} Load PDUs, contiguous: {in_order}, errors {errors:?}",
            seqs.len()
        ),
    );
    outcome.check(withheld_and_skipped(&outcome) == (0, 0), "nothing withheld");
}

// docs/spec.md  (Deferred Datagrams get a fresh lpduTime: every Load
// PDU's lpduTime is when it was actually sent)
#[test]
fn deferred_datagrams_carry_the_time_they_are_sent() {
    let outcome = downstream(2)
        .partial_sends(10, 1)
        .run_for(Duration::from_millis(300));

    // The simulated path has no delay, and the Server's wall clock starts
    // at WALL_START_UNIX_SECS.
    let start = Duration::from_secs(harness::WALL_START_UNIX_SECS);
    let stale: Vec<u32> = loads(&outcome)
        .iter()
        .filter(|t| {
            let b = &t.payload;
            let secs = u32::from_be_bytes([b[20], b[21], b[22], b[23]]);
            let nanos = u32::from_be_bytes([b[24], b[25], b[26], b[27]]);
            Duration::new(secs.into(), nanos) != start + t.at
        })
        .map(|t| seq(t))
        .collect();
    outcome.check(stale.is_empty(), &format!("stale lpduTime: {stale:?}"));
}

// docs/spec.md ; GLOSSARY.md Withheld Burst (while a whole Burst is
// deferred, the next is withheld and reported as one)
#[test]
fn a_whole_deferred_burst_withholds_the_next() {
    // Every 10th Send Plan, the host takes none of it.
    let outcome = downstream(3)
        .partial_sends(10, 0)
        .run_for(Duration::from_millis(600));

    let seqs: Vec<u32> = loads(&outcome).iter().map(|t| seq(t)).collect();
    let in_order = seqs.windows(2).all(|w| w[1] == w[0] + 1);
    let errors = reported_errors(&outcome);
    let (withheld, skipped) = withheld_and_skipped(&outcome);
    outcome.check(
        in_order && errors == (0, 0, 0) && (40..=70).contains(&withheld) && skipped == 0,
        &format!("contiguous: {in_order}, errors {errors:?}, {withheld} withheld"),
    );
}

// docs/spec.md  (a host that takes 3 of every plan: the Deferred
// Datagrams grow until they fill a Burst, which is then withheld, and no
// more; nothing is lost)
#[test]
fn a_host_that_always_takes_too_little_withholds_a_burst_in_four() {
    let outcome = downstream(4)
        .partial_sends(1, 3)
        .run_for(Duration::from_millis(600));
    let server = harness::SERVER_ADDR
        .parse::<std::net::SocketAddr>()
        .unwrap();
    let from_server = loads(&outcome)
        .iter()
        .find(|t| t.source.ip() == server.ip())
        .map(|t| t.source)
        .unwrap();
    let errors = reported_errors(&outcome);
    let (withheld, _) = withheld_and_skipped(&outcome);
    // Plans of 4, 5, 6 and 7 datagrams send 3 each; the 4 left withhold
    // the next Burst: one Burst in 4 is withheld, 3 datagrams go per ms.
    let sent = loads(&outcome).len();
    outcome.check(
        contiguous(&outcome, from_server)
            && errors == (0, 0, 0)
            && (110..=160).contains(&withheld)
            && (1500..=1800).contains(&sent),
        &format!("errors {errors:?}, {withheld} withheld, {sent} sent"),
    );
}

// docs/spec.md  (a host that takes nothing: the Deferred Datagrams
// stay one Burst, and every later Burst is withheld)
#[test]
fn a_host_that_takes_nothing_withholds_every_burst() {
    let outcome = downstream(5)
        .partial_sends(1, 0)
        .run_for(Duration::from_millis(300));
    let (withheld, skipped) = withheld_and_skipped(&outcome);
    outcome.check(
        loads(&outcome).is_empty() && withheld > 200 && skipped == 0,
        &format!("{withheld} withheld, {skipped} skipped"),
    );
}

// docs/spec.md  (two Transmitters and an Add-on: Deferred Datagrams of
// one lead the other's plan, and nothing is lost)
#[test]
fn deferred_datagrams_lead_the_other_transmitters_plans_too() {
    // Row 145: one 1250-byte datagram every 100 µs, and four plus an
    // Add-on every 1 ms.
    let outcome = Scenario::new(6)
        .operator(7, b"an operator key")
        .client_direction(Direction::Downstream)
        .client_requests_row(RowRequest::FixedRate(145))
        .partial_sends(7, 1)
        .run_for(Duration::from_millis(400));
    let errors = reported_errors(&outcome);
    let seqs: Vec<u32> = loads(&outcome).iter().map(|t| seq(t)).collect();
    let in_order = seqs.windows(2).all(|w| w[1] == w[0] + 1);
    outcome.check(
        in_order && seqs.len() > 3000 && errors == (0, 0, 0),
        &format!(
            "contiguous: {in_order}, {} sent, errors {errors:?}",
            seqs.len()
        ),
    );
}

// docs/spec.md  (a Client that stops the Test sends its Deferred
// Datagrams before its STOP2, so none is lost)
#[test]
fn a_stop_sends_the_deferred_datagrams_first() {
    let outcome = Scenario::new(7)
        .operator(7, b"an operator key")
        .client_direction(Direction::Upstream)
        .client_requests_row(RowRequest::FixedRate(40))
        .partial_sends(1, 3)
        .client_stops_data_phase_at(Duration::from_millis(300))
        .run_for(Duration::from_millis(600));
    let client = harness::CLIENT_ADDR.parse().unwrap();
    let from_client: Vec<&TraceEntry> = loads(&outcome)
        .into_iter()
        .filter(|t| t.source == client)
        .collect();
    let last_is_stop = from_client.last().is_some_and(|t| t.payload[2] == 2);
    outcome.check(
        contiguous(&outcome, client) && last_is_stop,
        &format!(
            "{} Load PDUs, the last a STOP2: {last_is_stop}",
            from_client.len()
        ),
    );
}
