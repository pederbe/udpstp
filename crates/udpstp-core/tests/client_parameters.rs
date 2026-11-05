//! The Client's Test parameters in its Activation Request (RFC 9946 §7.1;
//! docs/spec.md stories 8 and 10-12): what the Client user chose reaches the
//! wire.

mod harness;

use std::time::Duration;

use harness::{RowRequest, Scenario};
use udpstp_core::{DelayVariation, Direction};

/// The Activation Requests the Client sent (RFC 9946 Figure 6).
fn activation_requests(outcome: &harness::Outcome) -> Vec<Vec<u8>> {
    outcome
        .trace
        .iter()
        .filter(|t| t.payload.starts_with(&[0xAC, 0xE2]) && t.payload[4] != 0 && t.payload[5] == 0)
        .map(|t| t.payload.clone())
        .collect()
}

// docs/spec.md story 8 (one-way delay variation or RTT variation as the
// algorithm's delay input)
#[udpstp_req::req("RFC9946-7.1-L4")]
#[test]
fn the_client_asks_for_the_delay_variation_chosen() {
    for (delay_variation, octet) in [(DelayVariation::OneWay, 1), (DelayVariation::RoundTrip, 0)] {
        let outcome = Scenario::new(1)
            .operator(7, b"an operator key")
            .client_config(move |c| c.delay_variation = delay_variation)
            .run_for(Duration::from_millis(100));
        let sent = activation_requests(&outcome);
        outcome.check(
            sent.len() == 1
                && sent[0][18] == octet
                && outcome.client_activated_direction(0) == Some(Direction::Downstream),
            &format!("{delay_variation:?}: {sent:?}"),
        );
    }
}

// docs/spec.md story 10 (a starting row, CHTA_SRIDX_ISSTART set)
#[udpstp_req::req("RFC9097-8.1-L1")]
#[test]
fn the_client_asks_for_the_start_row_chosen() {
    let outcome = Scenario::new(2)
        .operator(7, b"an operator key")
        .client_requests_row(RowRequest::StartAt(40))
        .run_for(Duration::from_millis(100));
    let sent = activation_requests(&outcome);
    outcome.check(
        sent.len() == 1 && sent[0][16..18] == 40u16.to_be_bytes() && sent[0][25] & 0x01 != 0,
        &format!("{sent:?}"),
    );
}
