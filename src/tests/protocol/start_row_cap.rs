//! The Server's cap on the Sending Rate Table row an ordinary Client may
//! start its search at (RFC 9946 §7.2.1, §4.1; ): a higher request is
//! coerced to it and the Activation Response says so; Operator Keys are
//! exempt.

use super::harness;

use std::time::Duration;

use harness::{ActivationParameter, Coercion, RowRequest, Scenario};

const KEY: &[u8] = b"a start row key";

fn started_at(scenario: Scenario) -> (Option<u16>, Vec<Coercion>) {
    let outcome = scenario
        .server_config(|c| c.max_start_row = 50)
        .client_requests_row(RowRequest::StartAt(300))
        .run_for(Duration::from_millis(300));
    let row = outcome
        .activation_responses()
        .first()
        .map(|r| r.sr_index_conf);
    (row, outcome.client_coercions())
}

//
#[udpstp_req::req("RFC9946-7.2.1-2", "RFC9946-7.2.1-3")]
#[test]
fn a_consumer_s_start_row_is_coerced_to_the_cap() {
    let (row, coercions) = started_at(Scenario::new(1).mode1(3, KEY));
    assert_eq!(row, Some(50));
    assert!(
        coercions.contains(&Coercion {
            parameter: ActivationParameter::SrIndexConf,
            requested: 300,
            coerced: 50,
        }),
        "{coercions:?}"
    );
}

// Operators may run any row;
#[udpstp_req::req("RFC9946-7.2.1-1")]
#[test]
fn an_operator_key_is_exempt() {
    let (row, coercions) = started_at(Scenario::new(2).operator(3, KEY));
    assert_eq!(row, Some(300));
    assert!(
        coercions
            .iter()
            .all(|c| c.parameter != ActivationParameter::SrIndexConf),
        "{coercions:?}"
    );
}

// : by default a Consumer's search starts no
// higher than row 100, however high it asks, still as a start row
#[udpstp_req::req("RFC9946-7.2.1-2", "RFC9946-7.2.1-3")]
#[test]
fn by_default_a_search_starts_at_row_100_at_most() {
    let outcome = Scenario::new(3)
        .mode1(3, KEY)
        .server_config(|c| c.max_start_row = crate::protocol::DEFAULT_MAX_START_ROW)
        .client_requests_row(RowRequest::StartAt(0xFFFE))
        .run_for(Duration::from_millis(300));
    let response = outcome.activation_responses().first().cloned();
    assert_eq!(response.as_ref().map(|r| r.sr_index_conf), Some(100));
    assert!(response.is_some_and(|r| r.modifier_bitmap
        & crate::wire::activation::modifier::SR_INDEX_IS_START
        != 0));
}

// : the cap bounds where a search starts, not how high it climbs
#[udpstp_req::req("RFC9097-2-6")]
#[test]
fn a_capped_search_still_climbs_past_the_cap() {
    let outcome = Scenario::new(4)
        .mode1(3, KEY)
        .server_config(|c| c.max_start_row = 50)
        .client_direction(crate::protocol::Direction::Upstream)
        .client_requests_row(RowRequest::StartAt(300))
        .run_for(Duration::from_secs(2));
    // The Server's rows, as its Status PDUs carry them, in Mbps.
    let highest = outcome
        .trace
        .iter()
        .filter(|t| t.payload.starts_with(&[0xFE, 0xED]))
        .filter_map(|t| crate::wire::StatusPdu::decode(&t.payload).ok())
        .map(|s| s.sending_rate.burst_size1 * 100 + s.sending_rate.burst_size2 * 10)
        .max()
        .unwrap_or(0);
    outcome.check(
        highest > 50,
        &format!("the search climbed to about {highest} Mbps"),
    );
}
