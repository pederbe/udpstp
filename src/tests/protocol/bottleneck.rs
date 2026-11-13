//! The simulator's bottleneck, and checks of the simulator itself against
//! closed-form expectations (docs/spec.md "Validating the simulator").
//!
//! A bottleneck of rate C with a drop-tail buffer of B bytes, fed at a fixed
//! rate R, gives these RFC 9097 IP-layer results in steady state: a received
//! rate, counting IP and UDP headers, of min(R, C); a Loss Ratio of
//! 1 - C/R when R > C, else 0; and, once the buffer is full, a queueing
//! delay of B/C, less the one datagram that is always being sent. Row N of
//! the Sending Rate Table sends N Mbps at the IP layer in 1250-byte IPv4
//! datagrams (1270 bytes on IPv6, whose payload is the same).

use super::harness;

use std::collections::{BTreeMap, HashSet};
use std::net::IpAddr;
use std::time::Duration;

use crate::protocol::Direction;
use crate::wire::StatusPdu;
use crate::wire::status::{STATUS_PDU_LEN, SubIntervalStats};
use harness::{Outcome, RowRequest, Scenario, TraceEntry};

const MBPS: u64 = 1_000_000;

/// IP and UDP header bytes.
fn headers(ipv6: bool) -> u64 {
    if ipv6 { 48 } else { 28 }
}

fn server_ips() -> [IpAddr; 2] {
    let ip = |addr: &str| addr.parse::<std::net::SocketAddr>().unwrap().ip();
    [ip(harness::SERVER_ADDR), ip(harness::SERVER_ADDR_V6)]
}

fn is_load(t: &TraceEntry) -> bool {
    t.payload.starts_with(&[0xBE, 0xEF])
}

/// A Downstream Fixed-Rate Test at `row` Mbps through a bottleneck.
fn through_bottleneck(seed: u64, row: u16, rate_bps: u64, buffer: u64) -> Scenario {
    Scenario::new(seed)
        .operator(7, b"an operator key")
        .client_direction(Direction::Downstream)
        .client_requests_row(RowRequest::FixedRate(row))
        .bottleneck(rate_bps, buffer)
        .path_delay(Duration::from_millis(5), Duration::ZERO)
}

/// The Status PDUs the Clients sent, with when they reached the Server,
/// decoded.
fn statuses(outcome: &Outcome) -> Vec<(Duration, StatusPdu)> {
    outcome
        .trace
        .iter()
        .filter(|t| server_ips().contains(&t.destination.ip()))
        .filter(|t| t.len == STATUS_PDU_LEN && t.payload.starts_with(&[0xFE, 0xED]))
        .filter_map(|t| Some((t.at, StatusPdu::decode(&t.payload).ok()?)))
        .collect()
}

/// Each completed Sub-Interval from the second on (the first holds the
/// buffer filling up), by its number.
fn steady_sub_intervals(outcome: &Outcome) -> BTreeMap<u32, SubIntervalStats> {
    statuses(outcome)
        .into_iter()
        .filter(|(_, s)| s.sub_int_seq_no >= 2)
        .map(|(_, s)| (s.sub_int_seq_no, s.sis_sav))
        .collect()
}

/// RFC 9097 IP-layer capacity of a Sub-Interval, in bit/s: udpPayload
/// bytes plus the IP and UDP headers of each datagram.
fn ip_layer_bps(sis: &SubIntervalStats, ipv6: bool) -> f64 {
    let bytes = sis.rx_bytes + headers(ipv6) * u64::from(sis.rx_datagrams);
    bytes as f64 * 8.0 / (f64::from(sis.delta_time) / 1e6)
}

fn within_1_percent(got: f64, expected: f64) -> bool {
    (got - expected).abs() <= expected / 100.0
}

// docs/spec.md "Validating the simulator" (datagram conservation: every
// datagram the bottleneck drops is one the Load Receiver counts as lost)
#[test]
fn every_datagram_the_bottleneck_drops_is_counted_as_lost() {
    let outcome = through_bottleneck(1, 40, 20 * MBPS, 25_000)
        .path_delay(Duration::from_millis(5), Duration::from_millis(2))
        .run_for(Duration::from_millis(1500));

    let net = outcome.network;
    let undelivered = outcome.trace.iter().filter(|t| !t.delivered).count() as u64;
    outcome.check(
        net.sent == outcome.trace.len() as u64 + net.in_transit
            && undelivered == net.bottleneck_drops,
        &format!("{net:?}, {undelivered} undelivered"),
    );

    // Loss shows once a later Load PDU arrives, within the path delay,
    // jitter and queue (under 20 ms), so the last Status PDU counts every
    // drop up to 20 ms before it left, and none after.
    let statuses = statuses(&outcome);
    let reported: u64 = statuses
        .iter()
        .map(|(_, s)| u64::from(s.seq_err_loss))
        .sum();
    let last_sent =
        statuses.last().map(|(at, _)| *at).unwrap_or_default() - Duration::from_millis(5);
    let dropped_before = |at: Duration| {
        outcome
            .trace
            .iter()
            .filter(|t| !t.delivered && is_load(t) && t.at < at)
            .count() as u64
    };
    let (surely, at_most) = (
        dropped_before(last_sent - Duration::from_millis(20)),
        dropped_before(last_sent),
    );
    outcome.check(
        surely > 1000 && (surely..=at_most).contains(&reported),
        &format!("{reported} lost reported; {surely} to {at_most} dropped"),
    );
}

// docs/spec.md "Validating the simulator" (the delivered rate matches the
// configured bottleneck within 1%, including header overhead)
#[test]
fn the_delivered_rate_matches_the_bottleneck() {
    for (seed, ipv6) in [(2, false), (3, true)] {
        let mut scenario = through_bottleneck(seed, 40, 20 * MBPS, 25_000);
        if ipv6 {
            scenario = scenario.ipv6();
        }
        let outcome = scenario.run_for(Duration::from_millis(2100));

        let (from, to) = (Duration::from_millis(1000), Duration::from_millis(2000));
        let bits: u64 = outcome
            .trace
            .iter()
            .filter(|t| t.delivered && is_load(t) && t.at >= from && t.at < to)
            .map(|t| (t.len as u64 + headers(ipv6)) * 8)
            .sum();
        let bps = bits as f64 / (to - from).as_secs_f64();
        outcome.check(
            within_1_percent(bps, 20e6),
            &format!("IPv6 {ipv6}: {bps} bit/s delivered through 20 Mbit/s"),
        );
    }
}

// docs/spec.md "Validating the simulator" (the Test Connections between
// two hosts share one bottleneck)
#[test]
fn test_connections_share_the_bottleneck() {
    // Two 10 Mbit/s Test Connections into 15 Mbit/s: 15 in all, where
    // a bottleneck each would let 20 through.
    let outcome = through_bottleneck(11, 10, 15 * MBPS, 25_000)
        .coordinated(2, 2)
        .run_for(Duration::from_millis(2100));

    let (from, to) = (Duration::from_millis(1000), Duration::from_millis(2000));
    let delivered: Vec<&TraceEntry> = outcome
        .trace
        .iter()
        .filter(|t| t.delivered && is_load(t) && t.at >= from && t.at < to)
        .collect();
    let connections: HashSet<u16> = delivered.iter().map(|t| t.destination.port()).collect();
    let bits: u64 = delivered.iter().map(|t| (t.len as u64 + 28) * 8).sum();
    let bps = bits as f64 / (to - from).as_secs_f64();
    outcome.check(
        connections.len() == 2 && within_1_percent(bps, 15e6),
        &format!("{} Test Connections: {bps} bit/s in all", connections.len()),
    );
}

// docs/spec.md "Validating the simulator" (the same seed gives the same
// trace hash)
#[test]
fn the_same_seed_gives_the_same_trace() {
    let run = |seed| {
        through_bottleneck(seed, 40, 20 * MBPS, 25_000)
            .path_delay(Duration::from_millis(5), Duration::from_millis(3))
            .run_for(Duration::from_millis(600))
    };
    let (a, b, other) = (run(4), run(4), run(5));
    a.check(a.trace_hash() == b.trace_hash(), "seed 4 twice");
    a.check(a.trace_hash() != other.trace_hash(), "seeds 4 and 5 differ");
}

// IP-layer capacity counts the IP and UDP headers of the address family;
// docs/spec.md "Validating the simulator" (closed-form RFC 9097 expectations)
#[udpstp_req::req("RFC9097-5.3-L1")]
#[test]
fn a_fixed_rate_over_the_bottleneck_measures_its_capacity_and_loss() {
    // 30 Mbit/s into 20: a third is lost.
    for (seed, ipv6) in [(6, false), (12, true)] {
        let mut scenario = through_bottleneck(seed, 30, 20 * MBPS, 25_000);
        if ipv6 {
            scenario = scenario.ipv6();
        }
        let outcome = scenario.run_for(Duration::from_millis(3200));

        let sub_intervals = steady_sub_intervals(&outcome);
        outcome.check(sub_intervals.len() >= 2, "two steady Sub-Intervals");
        for (n, sis) in &sub_intervals {
            let bps = ip_layer_bps(sis, ipv6);
            let sent = sis.rx_datagrams + sis.seq_err_loss;
            let loss = f64::from(sis.seq_err_loss) / f64::from(sent);
            outcome.check(
                within_1_percent(bps, 20e6) && (loss - 1.0 / 3.0).abs() <= 0.01,
                &format!("IPv6 {ipv6}, Sub-Interval {n}: {bps} bit/s, loss ratio {loss}: {sis:?}"),
            );
        }
    }
}

// docs/spec.md "Validating the simulator" (below the bottleneck, the offered
// rate arrives whole)
#[udpstp_req::req("RFC9097-5.3-L1")]
#[test]
fn a_fixed_rate_under_the_bottleneck_arrives_whole() {
    let outcome = through_bottleneck(7, 10, 20 * MBPS, 25_000).run_for(Duration::from_millis(3200));

    let sub_intervals = steady_sub_intervals(&outcome);
    outcome.check(sub_intervals.len() >= 2, "two steady Sub-Intervals");
    for (n, sis) in &sub_intervals {
        let bps = ip_layer_bps(sis, false);
        outcome.check(
            within_1_percent(bps, 10e6) && sis.seq_err_loss == 0 && sis.delay_var_max <= 1,
            &format!("Sub-Interval {n}: {bps} bit/s: {sis:?}"),
        );
    }
}

// docs/spec.md "Validating the simulator" (a full buffer of B bytes at rate C
// adds B/C of delay)
#[udpstp_req::req("RFC9946-8.2-L10")]
#[test]
fn a_full_buffer_shows_as_its_drain_time_in_delay_variation() {
    // 25 000 bytes at 20 Mbit/s drain in 10 ms. The first datagram waited
    // only for its own 0.5 ms of serialization; one that finds the buffer
    // full waits 9.5 ms to 10 ms more, which rounds to 9 or 10.
    let outcome = through_bottleneck(8, 40, 20 * MBPS, 25_000).run_for(Duration::from_millis(1200));

    let later: Vec<u32> = statuses(&outcome)
        .iter()
        .skip(5)
        .map(|(_, s)| s.delay_var_max)
        .collect();
    outcome.check(
        !later.is_empty() && later.iter().all(|max| (9..=10).contains(max)),
        &format!("delayVarMax per Trial Interval: {later:?}"),
    );
}

// docs/spec.md "Validating the simulator" (a drop-tail buffer holds at
// most its size, the datagram being sent included)
#[test]
fn no_datagram_waits_longer_than_the_buffer_takes_to_drain() {
    let outcome = through_bottleneck(9, 40, 20 * MBPS, 25_000).run_for(Duration::from_millis(600));

    // A Load PDU's lpduTime is when it left the Server, whose wall clock
    // starts at WALL_START_UNIX_SECS.
    let start = Duration::from_secs(harness::WALL_START_UNIX_SECS);
    let longest = outcome
        .trace
        .iter()
        .filter(|t| t.delivered && is_load(t))
        .map(|t| {
            let b = &t.payload;
            let secs = u32::from_be_bytes([b[20], b[21], b[22], b[23]]);
            let nanos = u32::from_be_bytes([b[24], b[25], b[26], b[27]]);
            t.at + start - Duration::new(secs.into(), nanos)
        })
        .max()
        .unwrap_or_default();
    // 5 ms of path, then at most 25 000 bytes at 20 Mbit/s: 10 ms. One
    // more 1250-byte datagram would add 0.5 ms.
    let (drained, datagram) = (Duration::from_millis(15), Duration::from_micros(500));
    outcome.check(
        longest <= drained && longest > drained - datagram,
        &format!("the longest wait was {longest:?}"),
    );
}
