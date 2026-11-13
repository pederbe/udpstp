//! The Sending Rate Table up to 1 Gbps, observed through the Sending Rate
//! Structure of Upstream Activation Responses (RFC 9946 §6.1, §7.2.2).
//!
//! Expected values come from the RFC, OB-UDPST's documented index-to-rate
//! mapping and its Client's sanity limits, and docs/spec.md ("Sending Rate Table"), never from our
//! table.

use super::harness;

use std::time::Duration;

use crate::protocol::Direction;
use crate::wire::{ActivationPdu, SendingRate};
use harness::{Outcome, RowRequest, Scenario};

/// IPv4 and UDP headers: srStruct sizes are IPv4-based (ADR 0004).
const IPV4_UDP_HEADERS: u32 = 28;
/// The random-size flag OB-UDPST puts in the top bit of a size field.
const RANDOM_SIZE_FLAG: u32 = 0x8000_0000;

/// Runs an Upstream Test's Activation exchange for `request` and returns the
/// outcome with the Server's one Activation Response.
fn upstream_activation(seed: u64, request: RowRequest) -> (Outcome, ActivationPdu) {
    let scenario = match request {
        // A Fixed-Rate Test needs an Operator Key (RFC 9946 §4.1).
        RowRequest::FixedRate(_) => Scenario::new(seed).operator(7, b"an operator key"),
        _ => Scenario::new(seed)
            .server_allows_unauthenticated()
            .client_unauthenticated(),
    };
    let outcome = scenario
        .client_direction(Direction::Upstream)
        .client_requests_row(request)
        .run_for(Duration::from_millis(50));
    let responses = outcome.activation_responses();
    outcome.check(responses.len() == 1, "one Activation Response was sent");
    outcome.check(
        outcome.client_activated_direction(0) == Some(Direction::Upstream),
        "the Upstream Test was activated",
    );
    let response = responses[0].clone();
    (outcome, response)
}

/// Every row from 0 to 1000, each requested as the starting row.
fn rows_up_to_1_gbps() -> impl Iterator<Item = (u16, Outcome, SendingRate)> {
    (0..=1000).map(|row| {
        let (outcome, response) = upstream_activation(u64::from(row), RowRequest::StartAt(row));
        (row, outcome, response.sending_rate)
    })
}

/// IP-layer bits per second of a Sending Rate Structure: each transmitter
/// sends `burst` datagrams per interval, and the add-on is one more datagram
/// at the end of every second interval (RFC 9946 §7.1).
fn ip_layer_bps(sr: &SendingRate) -> f64 {
    let per_second = |bytes: u32, count: u32, interval_us: u32| {
        if bytes == 0 || count == 0 || interval_us == 0 {
            return 0.0;
        }
        f64::from((bytes + IPV4_UDP_HEADERS) * 8 * count) * 1e6 / f64::from(interval_us)
    };
    per_second(sr.udp_payload1, sr.burst_size1, sr.tx_interval1)
        + per_second(sr.udp_payload2, sr.burst_size2, sr.tx_interval2)
        + per_second(sr.udp_addon2, 1, sr.tx_interval2)
}

// docs/spec.md "Sending Rate Table" (row N is N Mbps up to row 1000);
// TR-471 Table 1 HighSpeedDelta / HSpeedThresh assume 1 Mbps per row
#[udpstp_req::req("RFC9097-8.1-2", "RFC9946-11-L1")]
#[test]
fn row_n_is_n_mbps_up_to_1_gbps() {
    for (row, outcome, sr) in rows_up_to_1_gbps().skip(1) {
        let bps = ip_layer_bps(&sr);
        outcome.check(
            (bps - f64::from(row) * 1e6).abs() < 1.0,
            &format!("row {row} is {row} Mbit/s, not {bps} bit/s: {sr:?}"),
        );
    }
}

// ADR 0004 (IPv4-based sizes); docs/spec.md (limits OB-UDPST Clients enforce on
// srStruct); research note §2.1 (OB-UDPST's smallest payload, 52 bytes,
// below which it skips the IPv6 adjustment)
#[udpstp_req::req("RFC9946-6.1-8", "RFC9097-8.1-L2", "RFC9946-7.1-L8")]
#[test]
fn every_row_up_to_1_gbps_fits_the_rfc_sizes_and_ob_udpst_client_limits() {
    for (row, outcome, sr) in rows_up_to_1_gbps() {
        let check = |ok: bool, what: String| outcome.check(ok, &format!("row {row}: {what}"));
        for size in [sr.udp_payload1, sr.udp_payload2, sr.udp_addon2] {
            if size == 0 {
                continue;
            }
            check(
                size + IPV4_UDP_HEADERS <= 1250,
                format!("a {size}-byte payload makes an IP packet over 1250 bytes"),
            );
            check(size <= 8972, format!("payload {size} is over 8972"));
            check(size >= 52, format!("payload {size} is under 52"));
        }
        for burst in [sr.burst_size1, sr.burst_size2] {
            check(burst <= 100, format!("burst {burst} is over 100"));
        }
        for interval in [sr.tx_interval1, sr.tx_interval2] {
            check(
                interval == 0 || (75..=1_000_000).contains(&interval),
                format!("interval {interval} µs is outside 75 µs..1 s"),
            );
        }
        if sr.burst_size2 != 0 {
            check(
                sr.udp_addon2 <= sr.udp_payload2,
                format!(
                    "add-on {} is larger than the burst payload {}",
                    sr.udp_addon2, sr.udp_payload2
                ),
            );
        }
        check(ip_layer_bps(&sr) > 0.0, format!("sends nothing: {sr:?}"));
    }
}

// docs/spec.md "Sending Rate Table" (row 0 is a fixed datagram size near
// 0.1 Mbps with no random-size flag)
#[udpstp_req::req("RFC9097-8.1-6")]
#[test]
fn row_zero_sends_fixed_size_datagrams_near_0_1_mbps() {
    let (outcome, response) = upstream_activation(1, RowRequest::StartAt(0));
    let sr = response.sending_rate;

    for size in [sr.udp_payload1, sr.udp_payload2, sr.udp_addon2] {
        outcome.check(
            size & RANDOM_SIZE_FLAG == 0,
            &format!("no random-size flag: {sr:?}"),
        );
    }
    let bps = ip_layer_bps(&sr);
    outcome.check(
        (80_000.0..=120_000.0).contains(&bps),
        &format!("row 0 is about 0.1 Mbit/s, not {bps} bit/s: {sr:?}"),
    );
}

#[udpstp_req::req("RFC9946-7.1-4", "RFC9946-7.2.2-2")]
#[test]
fn the_server_default_starts_at_row_zero() {
    let (outcome, default) = upstream_activation(2, RowRequest::ServerDefault);
    let (_, row_zero) = upstream_activation(3, RowRequest::StartAt(0));

    outcome.check(
        default.sending_rate != SendingRate::default(),
        "the first row is not empty",
    );
    outcome.check(
        default.sending_rate == row_zero.sending_rate,
        "the default is row 0",
    );
    outcome.check(
        default.sr_index_conf == 0xFFFF,
        "the response keeps CHTA_SRIDX_DEF",
    );
}

#[udpstp_req::req("RFC9946-7.2.2-2", "RFC9097-8.1-L1", "RFC9946-12.3.7-L1")]
#[test]
fn a_requested_starting_row_is_sent() {
    // Row 123 = one 1222-byte datagram every 100 µs (100 Mbps), two every
    // 1 ms (20 Mbps), and a 3 Mbps add-on of 375 - 28 bytes every 1 ms.
    let expected = SendingRate {
        tx_interval1: 100,
        udp_payload1: 1222,
        burst_size1: 1,
        tx_interval2: 1000,
        udp_payload2: 1222,
        burst_size2: 2,
        udp_addon2: 347,
    };

    let (outcome, response) = upstream_activation(4, RowRequest::StartAt(123));
    outcome.check(
        response.sending_rate == expected,
        &format!("row 123 is {expected:?}, not {:?}", response.sending_rate),
    );
}

// A Fixed-Rate Test, requested with an Operator Key (RFC 9946 §4.1).
#[udpstp_req::req("RFC9946-7.2.2-2")]
#[test]
fn a_requested_fixed_rate_row_is_sent() {
    // Row 1000 = ten 1222-byte datagrams every 100 µs, nothing else.
    let expected = SendingRate {
        tx_interval1: 100,
        udp_payload1: 1222,
        burst_size1: 10,
        ..SendingRate::default()
    };

    let (outcome, response) = upstream_activation(5, RowRequest::FixedRate(1000));
    outcome.check(
        response.sending_rate == expected,
        &format!("row 1000 is {expected:?}, not {:?}", response.sending_rate),
    );
}

// Rows 1-9 are add-on only.
#[udpstp_req::req("RFC9946-7.2.2-2")]
#[test]
fn a_low_row_is_an_add_on_alone() {
    // Row 5 = one 625-byte IP packet every 1 ms.
    let expected = SendingRate {
        tx_interval2: 1000,
        udp_addon2: 597,
        ..SendingRate::default()
    };

    let (outcome, response) = upstream_activation(6, RowRequest::StartAt(5));
    outcome.check(
        response.sending_rate == expected,
        &format!("row 5 is {expected:?}, not {:?}", response.sending_rate),
    );
}

// srIndexConf is 0xFFFF for the Server default; otherwise the row, with
// CHTA_SRIDX_ISSTART (0x01) set for a starting row and clear for a fixed
// rate.
#[udpstp_req::req("RFC9946-7.1-3", "RFC9946-8.1-10")]
#[test]
fn the_client_asks_for_the_row_in_its_activation_request() {
    let cases = [
        (RowRequest::ServerDefault, 0xFFFF, false),
        (RowRequest::StartAt(42), 42, true),
        (RowRequest::FixedRate(875), 875, false),
    ];
    for (seed, (request, sr_index_conf, is_start)) in (7..).zip(cases) {
        let outcome = Scenario::new(seed)
            .server_allows_unauthenticated()
            .client_unauthenticated()
            .client_direction(Direction::Upstream)
            .client_requests_row(request)
            .run_for(Duration::from_secs(1));

        let requests = outcome.activation_requests();
        outcome.check(requests.len() == 1, "one Activation Request was sent");
        outcome.check(
            requests[0].sr_index_conf == sr_index_conf,
            &format!("{request:?} is sent as srIndexConf {sr_index_conf}"),
        );
        outcome.check(
            (requests[0].modifier_bitmap & 0x01 != 0) == is_start,
            &format!("{request:?} sets CHTA_SRIDX_ISSTART: {is_start}"),
        );
    }
}
