//! Operator-only Fixed-Rate Tests and srIndexConf handling (RFC 9946 §4.1,
//! §7.1, §7.2.1, §7.2.2).
//!
//! Observed through Upstream Activation Responses, whose Sending Rate
//! Structure is the row the Server chose (§7.2.2). Rows are compared with
//! the row a search starting there gets; the rows' values themselves are
//! proven in sending_rate_table.rs.

mod harness;

use std::time::Duration;

use harness::{ActivationRejection, Outcome, RowRequest, Scenario};
use udpstp_core::{Direction, Event};
use udpstp_wire::ActivationPdu;
use udpstp_wire::activation::cmd_response;

const KEY_ID: u8 = 7;
const KEY: &[u8] = b"a shared key";
/// CHTA_SRIDX_ISSTART in modifierBitmap (RFC 9946 §7.1).
const IS_START: u8 = 0x01;
/// CHTA_SRIDX_DEF.
const DEFAULT_ROW: u16 = 0xFFFF;

enum As {
    Operator,
    Consumer,
    Unauthenticated,
}

fn upstream(seed: u64, who: As, request: RowRequest) -> Outcome {
    test(seed, who, Direction::Upstream, request)
}

fn test(seed: u64, who: As, direction: Direction, request: RowRequest) -> Outcome {
    let scenario = Scenario::new(seed);
    let scenario = match who {
        As::Operator => scenario.operator(KEY_ID, KEY),
        As::Consumer => scenario.mode1(KEY_ID, KEY),
        As::Unauthenticated => scenario
            .server_allows_unauthenticated()
            .client_unauthenticated(),
    };
    scenario
        .client_direction(direction)
        .client_requests_row(request)
        .run_for(Duration::from_secs(1))
}

fn response(outcome: &Outcome) -> ActivationPdu {
    let responses = outcome.activation_responses();
    outcome.check(responses.len() == 1, "one Activation Response");
    responses[0].clone()
}

fn accepted(outcome: &Outcome) -> ActivationPdu {
    outcome.check(
        outcome.client_activated_direction(0) == Some(Direction::Upstream),
        "the Test was activated",
    );
    response(outcome)
}

// docs/spec.md story 48
#[udpstp_req::req("RFC9946-4.1-3", "RFC9946-7.2.1-L2")]
#[test]
fn a_fixed_rate_test_is_refused_without_an_operator_key() {
    for (seed, who, what) in [
        (1, As::Consumer, "an ordinary Shared Key"),
        (2, As::Unauthenticated, "unauthenticated mode"),
    ] {
        let outcome = upstream(seed, who, RowRequest::FixedRate(123));

        outcome.check(
            outcome
                .server_rejected_activation_because(ActivationRejection::FixedRateNeedsOperatorKey),
            &format!("{what}: the Server refuses the Fixed-Rate Test"),
        );
        outcome.check(
            response(&outcome).cmd_response == cmd_response::BAD_PARAM
                && outcome.client_activation_rejected(0),
            &format!("{what}: with CHTA_CRSP_BADPARAM"),
        );
    }
}

// docs/spec.md story 47
#[udpstp_req::req("RFC9946-4.1-L1", "RFC9946-7.2.2-2", "RFC9946-8.1-9")]
#[test]
fn an_operator_runs_a_fixed_rate_test_at_the_requested_row() {
    let outcome = upstream(3, As::Operator, RowRequest::FixedRate(123));
    let fixed = accepted(&outcome);
    let search = accepted(&upstream(4, As::Consumer, RowRequest::StartAt(123)));

    outcome.check(
        fixed.sr_index_conf == 123 && fixed.modifier_bitmap & IS_START == 0,
        "the response keeps it a Fixed-Rate Test at row 123",
    );
    outcome.check(
        fixed.sending_rate == search.sending_rate && fixed.sending_rate != Default::default(),
        "row 123, as a search starting there sends first",
    );
}

// No Load PDU goes out for a refused Downstream request.
#[udpstp_req::req("RFC9946-4.1-3")]
#[test]
fn a_refused_downstream_fixed_rate_test_sends_no_load() {
    let outcome = test(
        13,
        As::Consumer,
        Direction::Downstream,
        RowRequest::FixedRate(123),
    );

    outcome.check(outcome.client_activation_rejected(0), "refused");
    outcome.check(
        !outcome
            .trace
            .iter()
            .any(|t| t.payload.starts_with(&[0xBE, 0xEF])),
        "no Load PDU",
    );
    outcome.check(
        !outcome
            .server_events
            .iter()
            .any(|e| matches!(e, Event::TestConnectionActivated { .. })),
        "no Test Connection was activated",
    );
}

#[udpstp_req::req("RFC9946-7.1-3", "RFC9946-7.1-4", "RFC9946-7.2.1-1", "RFC9946-7.2.2-2")]
#[test]
fn a_consumer_may_choose_the_starting_row_or_the_default() {
    let outcome = upstream(5, As::Consumer, RowRequest::StartAt(123));
    let start = accepted(&outcome);
    outcome.check(start.sr_index_conf == 123, "the starting row");

    // The Client sends CHTA_SRIDX_DEF with CHTA_SRIDX_ISSTART clear: still
    // the default search, not a Fixed-Rate Test.
    let outcome = upstream(6, As::Consumer, RowRequest::ServerDefault);
    let default = accepted(&outcome);
    let row0 = accepted(&upstream(7, As::Consumer, RowRequest::StartAt(0)));
    outcome.check(
        default.sr_index_conf == DEFAULT_ROW && default.sending_rate == row0.sending_rate,
        "the default starts at row 0",
    );
}

// Every srIndexConf gives a row of the table.
// The last row the Server uses is the first at the table's top rate: 1069,
// 12 Gbps with the default 1500-byte MTU; rows after it repeat it.
#[udpstp_req::req("RFC9946-7.2.1-3", "RFC9946-7.2.1-4")]
#[test]
fn a_row_beyond_the_table_is_coerced_to_the_last_row() {
    const LAST: u16 = 1069;
    let last = accepted(&upstream(8, As::Consumer, RowRequest::StartAt(LAST)));
    for (seed, who, request) in [
        (9, As::Consumer, RowRequest::StartAt(LAST + 1)),
        (10, As::Consumer, RowRequest::StartAt(1152)),
        (11, As::Consumer, RowRequest::StartAt(0xFFFE)),
        (12, As::Operator, RowRequest::FixedRate(LAST + 1)),
        (13, As::Operator, RowRequest::FixedRate(0xFFFE)),
    ] {
        let outcome = upstream(seed, who, request);
        let coerced = accepted(&outcome);
        outcome.check(
            coerced.sr_index_conf == LAST && coerced.sending_rate == last.sending_rate,
            &format!("{request:?} is coerced to row {LAST}"),
        );
    }
}
