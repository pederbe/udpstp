//! Per-Sub-Interval results (RFC 9097 §4; RFC 9946 §8.2; docs/spec.md ): the
//! Load Receiver reports each Sub-Interval as it closes, and the Load
//! Sender reports each from the Status PDUs' sisSav, once per
//! subIntSeqNo. The IP-layer rate includes the IP and UDP headers and is
//! taken over the nominal Sub-Interval.

use super::harness;

use std::time::Duration;

use crate::protocol::{
    DataEvent, Direction, Evaluation, IpVersion, MeasurementResults, PerformanceCriterion,
    SubIntervalMeasurement,
};
use harness::{Outcome, RowRequest, Scenario};

/// The Client's results of a 5 s Test of 1 s Sub-Intervals over IPv4.
fn results(events: &[(crate::protocol::SocketId, DataEvent)]) -> MeasurementResults {
    let events: Vec<DataEvent> = events.iter().map(|(_, e)| *e).collect();
    MeasurementResults::of_test_connection(
        &events,
        &Evaluation {
            test_interval: Duration::from_secs(5),
            sub_interval: Duration::from_secs(1),
            ip_version: IpVersion::V4,
            criterion: PerformanceCriterion::DEFAULT,
            first_mode: None,
        },
    )
}

fn measured(events: &[(crate::protocol::SocketId, DataEvent)]) -> Vec<SubIntervalMeasurement> {
    events
        .iter()
        .filter_map(|(_, e)| match e {
            DataEvent::SubIntervalMeasured(m) => Some(*m),
            _ => None,
        })
        .collect()
}

/// A 5 s Fixed-Rate Test at row 5: 5 Mbit/s at the IP layer.
fn fixed_rate(seed: u64, direction: Direction) -> Outcome {
    Scenario::new(seed)
        .operator(7, b"an operator key")
        .client_direction(direction)
        .client_requests_row(RowRequest::FixedRate(5))
        .client_config(|c| c.test_int_time = 5)
        .run_for(Duration::from_millis(6500))
}

fn near(rate: u64, mbps: f64) -> bool {
    (rate as f64 / 1e6 - mbps).abs() <= mbps * 0.05
}

// The IP-layer rate per Sub-Interval, headers included; docs/spec.md
#[udpstp_req::req("RFC9097-5.3-L1")]
#[test]
fn a_downstream_client_reports_each_sub_interval_it_received() {
    let outcome = fixed_rate(1, Direction::Downstream);
    let got = measured(&outcome.client_data_events);
    let seqs: Vec<u32> = got.iter().map(|m| m.sub_int_seq_no).collect();
    // Sub-Intervals 2 to 4 are whole; the first starts with the first
    // Status PDU and the last ends with the Test.
    outcome.check(
        seqs.starts_with(&[1, 2, 3, 4])
            && got[1..4]
                .iter()
                .all(|m| near(m.ip_layer_capacity, 5.0) && m.loss == 0),
        &format!("{got:#?}"),
    );
}

// docs/spec.md  (in Upstream Tests the Client builds results from
// sisSav, de-duplicated by subIntSeqNo)
#[test]
fn an_upstream_client_reports_each_sub_interval_from_sis_sav_once() {
    let outcome = fixed_rate(2, Direction::Upstream);
    let got = measured(&outcome.client_data_events);
    let seqs: Vec<u32> = got.iter().map(|m| m.sub_int_seq_no).collect();
    let consecutive = seqs.windows(2).all(|w| w[1] == w[0] + 1);
    outcome.check(
        seqs.first() == Some(&1)
            && seqs.len() >= 4
            && consecutive
            && got[1..4].iter().all(|m| near(m.ip_layer_capacity, 5.0)),
        &format!("{seqs:?}"),
    );
}

// docs/spec.md  (the IP-layer rate over IPv6 includes 48 header bytes a
// datagram)
#[udpstp_req::req("RFC9097-5.3-L1", "RFC9097-8.3-L2")]
#[test]
fn over_ipv6_the_headers_are_48_bytes() {
    let outcome = Scenario::new(3)
        .ipv6()
        .operator(7, b"an operator key")
        .client_requests_row(RowRequest::FixedRate(5))
        .client_config(|c| c.test_int_time = 5)
        .run_for(Duration::from_millis(3500));
    let got = measured(&outcome.client_data_events);
    let whole = got.get(1).copied();
    outcome.check(
        whole.is_some_and(|m| m.ip_layer_capacity == (m.bytes + u64::from(m.datagrams) * 48) * 8),
        &format!("{whole:#?}"),
    );
}

// The Server makes subIntPeriod a whole number of Trial Intervals, as a
// Sub-Interval closes at a Status PDU, and a whole number of those make
// the Test Interval (I = m * dt) when any can: with the 10 s default, 3 s
// becomes 2.5 s (4 of them), 4 s becomes 5 s (the nearer), and with
// 150 ms Trial Intervals 3.4 s becomes 3.45 s, as no multiple of 150 ms
// divides 10 s.
#[udpstp_req::req("RFC9097-5.3-6", "RFC9097-6.3-7", "RFC9097-5.3-2", "RFC9097-6.3-2")]
#[test]
fn the_sub_interval_becomes_a_whole_number_of_trial_intervals() {
    use harness::status_script::{DEFAULTS, Params, reported};
    for (trial_int, asked, coerced) in [
        (60, 100, 120),
        (250, 100, 250),
        (50, 1000, 1000),
        (150, 1000, 1050),
        (50, 3000, 2500),
        (100, 3000, 2500),
        (50, 4000, 5000),
        (150, 3400, 3450),
    ] {
        let params = Params {
            trial_int,
            sub_int_period: asked,
            ..DEFAULTS
        };
        let (outcome, _) = reported(Scenario::new(4), params, vec![]);
        let response = outcome.activation_responses().first().cloned();
        outcome.check(
            response
                .as_ref()
                .is_some_and(|r| r.sub_int_period == coerced),
            &format!("{trial_int}/{asked}: {response:?}"),
        );
    }
}

// docs/spec.md  (only Sub-Intervals of the Test Interval, closed while
// the Test ran, are results)
#[test]
fn no_more_sub_intervals_are_reported_than_the_test_interval_holds() {
    for direction in [Direction::Downstream, Direction::Upstream] {
        let outcome = fixed_rate(5, direction);
        let seqs: Vec<u32> = measured(&outcome.client_data_events)
            .iter()
            .map(|m| m.sub_int_seq_no)
            .collect();
        outcome.check(
            seqs.len() <= 5 && seqs.iter().all(|&s| (1..=5).contains(&s)),
            &format!("{direction:?}: {seqs:?}"),
        );
    }
}

// Bytes are the datagrams' actual lengths: a peer can't inflate the
// results by stating a larger udpPayload.
#[udpstp_req::req("RFC9946-8.1-L7")]
#[test]
fn a_forged_udp_payload_field_doesnt_inflate_the_capacity() {
    let outcome = Scenario::new(6)
        .operator(7, b"an operator key")
        .client_requests_row(RowRequest::FixedRate(5))
        .client_config(|c| c.test_int_time = 5)
        .network_flips_byte([0xBE, 0xEF], 8)
        .run_for(Duration::from_millis(3500));
    let got = measured(&outcome.client_data_events);
    outcome.check(
        got.get(1).is_some_and(|m| near(m.ip_layer_capacity, 5.0)),
        &format!("{got:#?}"),
    );
}

// docs/spec.md  (a missing final Sub-Interval is reported as missing,
// never zero)
#[test]
fn a_missing_final_sub_interval_is_missing() {
    // Upstream, the Client learns each Sub-Interval from the next Status
    // PDU; those after 4.9 s are lost.
    let outcome = Scenario::new(7)
        .operator(7, b"an operator key")
        .client_direction(Direction::Upstream)
        .client_requests_row(RowRequest::FixedRate(5))
        .client_config(|c| c.test_int_time = 5)
        .network_drops([0xFE, 0xED], Duration::from_millis(4900), None)
        .run_for(Duration::from_millis(6500));
    let slots = results(&outcome.client_data_events).sub_intervals;
    outcome.check(
        slots.len() == 5 && slots[..3].iter().all(Option::is_some) && slots[4].is_none(),
        &format!("{slots:#?}"),
    );
}

// Loss per Sub-Interval; duplicates and reordering counted apart from it.
#[udpstp_req::req("RFC9097-5.4-2", "RFC9097-5.4-3", "RFC9097-6.4-1")]
#[test]
fn duplicates_and_reordering_are_reported_apart_from_loss() {
    let outcome = Scenario::new(8)
        .operator(7, b"an operator key")
        .client_requests_row(RowRequest::FixedRate(5))
        .client_config(|c| c.test_int_time = 5)
        .duplicate_load_pdus(50)
        .reorder_load_pdus(77, 3)
        .run_for(Duration::from_millis(3500));
    let got = measured(&outcome.client_data_events);
    let whole = got.get(1).copied();
    outcome.check(
        whole.is_some_and(|m| m.duplicates > 0 && m.out_of_order > 0 && m.loss == 0),
        &format!("{whole:#?}"),
    );
}

// RTT at the maximum, reported with the minimum it is measured from;
#[udpstp_req::req("RFC9097-5.3-3", "RFC9097-5.3-5", "RFC9097-5.4-1")]
#[test]
fn each_sub_interval_carries_the_rtt_and_one_way_delay_minimums() {
    for direction in [Direction::Downstream, Direction::Upstream] {
        let outcome = Scenario::new(20)
            .operator(7, b"an operator key")
            .client_direction(direction)
            .client_requests_row(RowRequest::FixedRate(5))
            .client_config(|c| c.test_int_time = 5)
            .path_delay(Duration::from_millis(3), Duration::ZERO)
            .run_for(Duration::from_millis(6500));
        let got = measured(&outcome.client_data_events);
        outcome.check(
            got.len() >= 4
                && got.iter().all(|m| {
                    // 3 ms each way.
                    m.rtt_minimum.is_some_and(|rtt| (5..=7).contains(&rtt))
                        // A steady path: little variation.
                        && m.rtt_variation_mean.is_some_and(|mean| mean <= 1)
                        // The clocks agree: the one-way delay, 3 ms.
                        && m.clock_delta_min.is_some_and(|d| (2..=4).contains(&d))
                }),
            &format!("{direction:?}: {got:#?}"),
        );
    }
}

// The IP-Layer Sender Bit Rate over st, headers included;
#[udpstp_req::req("RFC9097-7.3-2", "RFC9097-7.2-L1", "RFC9097-7.4-1", "RFC9097-7.2-3")]
#[test]
fn an_upstream_client_reports_what_it_sent_per_st() {
    let outcome = fixed_rate(21, Direction::Upstream);
    let sent: Vec<crate::protocol::SentInterval> = outcome
        .client_data_events
        .iter()
        .filter_map(|(_, e)| match e {
            DataEvent::Sent(s) => Some(*s),
            _ => None,
        })
        .collect();
    let seqs: Vec<u32> = sent.iter().map(|s| s.st_seq_no).collect();
    // A 5 s Test Interval of 50 ms st: at most 100, numbered in order.
    outcome.check(
        sent.len() >= 90
            && sent.len() <= 100
            && seqs.iter().copied().eq(1..=sent.len() as u32)
            && sent[1..sent.len() - 1].iter().all(|s| {
                s.duration == Duration::from_millis(50) && near(s.ip_layer_sender_bit_rate, 5.0)
            }),
        &format!("{sent:#?}"),
    );
    // Per Sub-Interval: 20 st each, the Load Sender's own count.
    let results = results(&outcome.client_data_events);
    let per_sub = results.sender_bit_rates;
    outcome.check(!results.sender_estimated, "measured, not estimated");
    outcome.check(
        per_sub[1..4]
            .iter()
            .all(|s| s.is_some_and(|s| near(s.ip_layer_sender_bit_rate, 5.0))),
        &format!("{per_sub:#?}"),
    );
    // The Load Receiver reports none.
    outcome.check(
        !outcome
            .server_data_events
            .iter()
            .any(|(_, e)| matches!(e, DataEvent::Sent(_))),
        "the Server received",
    );
}
