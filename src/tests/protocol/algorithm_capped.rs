//! The algorithm capped at maxBandwidth (RFC 9946 §6.1 "This field MAY be
//! used to rate-limit the maximum rate the server should attempt", §11 item
//! 5; docs/spec.md story 9): the Server never picks a Sending Rate Table row
//! above the one matching the Client's stated maxBandwidth, with
//! CHSR_USDIR_BIT masked. Row N offers N Mbps up to the 1 Gbps row, so the
//! row matching M Mbps is row M. A maxBandwidth of 0 states no cap.

use super::harness;

use crate::protocol::Event;
use harness::status_script::{DEFAULTS, Params, Report, clean, errored, reported_then_stopped};
use harness::{ActivationParameter, Outcome, Scenario};

fn rows(seed: u64, params: Params, reports: Vec<Report>) -> (Outcome, Vec<Option<u16>>) {
    let (outcome, got) = reported_then_stopped(Scenario::new(seed), params, reports);
    let rows = got.iter().map(|c| c.row).collect();
    (outcome, rows)
}

fn check_rows(seed: u64, params: Params, reports: Vec<Report>, expected: &[u16]) -> Outcome {
    let (outcome, got) = rows(seed, params, reports);
    let expected: Vec<Option<u16>> = expected.iter().copied().map(Some).collect();
    outcome.check(
        got == expected,
        &format!("rows {got:?}, expected {expected:?}"),
    );
    outcome
}

fn capped(max_bandwidth: u16) -> Params {
    Params {
        max_bandwidth,
        ..DEFAULTS
    }
}

/// The srIndexConf coercion the Server reported, as (requested, coerced).
fn row_coercion(outcome: &Outcome) -> Option<(u16, u16)> {
    outcome.server_events.iter().find_map(|e| match e {
        Event::ActivationParameterCoerced { coercion, .. }
            if coercion.parameter == ActivationParameter::SrIndexConf =>
        {
            Some((coercion.requested, coercion.coerced))
        }
        _ => None,
    })
}

// docs/spec.md story 9 (the Server doesn't attempt rates beyond the
// Client's maxBandwidth)
#[udpstp_req::req("RFC9946-6.1-6", "RFC9946-11-L2", "RFC9097-10-5")]
#[test]
fn fast_mode_stops_at_the_row_matching_max_bandwidth() {
    check_rows(1, capped(25), vec![clean(); 4], &[10, 20, 25, 25]);
}

// docs/spec.md story 9 (CHSR_USDIR_BIT isn't part of the rate)
#[udpstp_req::req("RFC9946-6.1-6", "RFC9946-11-L2")]
#[test]
fn the_cap_masks_the_direction_bit() {
    check_rows(2, capped(0x8000 | 25), vec![clean(); 4], &[10, 20, 25, 25]);
}

// docs/spec.md story 9 (Single-Step Mode stops at the cap as well)
#[udpstp_req::req("RFC9946-6.1-6", "RFC9946-11-L2")]
#[test]
fn single_steps_stop_at_the_cap() {
    // From row 2: two Errored Intervals take a row off each, the third
    // confirms Congestion (30 rows off, stopping at row 0), and single
    // steps then climb to the cap and stay.
    check_rows(
        3,
        capped(2).starting_at(2),
        vec![errored(), errored(), errored(), clean(), clean(), clean()],
        &[1, 0, 0, 1, 2, 2],
    );
}

// docs/spec.md story 9 (a requested start row above the cap is coerced to
// it, and the response says so)
#[udpstp_req::req("RFC9946-6.1-6", "RFC9946-11-L2")]
#[test]
fn a_start_row_above_the_cap_is_coerced_to_it() {
    let outcome = check_rows(4, capped(100).starting_at(500), vec![clean()], &[100]);
    outcome.check(
        row_coercion(&outcome) == Some((500, 100)),
        &format!("{:?}", row_coercion(&outcome)),
    );
}

// docs/spec.md story 9 (a Fixed-Rate Test above the cap runs at it)
#[udpstp_req::req("RFC9946-6.1-6", "RFC9946-11-L2")]
#[test]
fn a_fixed_rate_above_the_cap_is_coerced_to_it() {
    let fixed = Params {
        sr_index_conf: 700,
        operator: Some(None),
        max_bandwidth: 300,
        ..DEFAULTS
    };
    let (outcome, _) = rows(5, fixed, vec![clean()]);
    outcome.check(
        row_coercion(&outcome) == Some((700, 300)),
        &format!("{:?}", row_coercion(&outcome)),
    );
}

// docs/spec.md (maxBandwidth 0 means no Client cap)
#[test]
fn a_max_bandwidth_of_0_states_no_cap() {
    check_rows(
        6,
        capped(0).starting_at(985),
        vec![clean(); 2],
        &[995, 1000],
    );
}

// A cap above the table's top leaves the table's bound in place: the first
// row at the top rate, 1069, 12 Gbps with the default 1500-byte MTU.
#[test]
fn a_cap_above_the_table_stops_at_its_last_row() {
    check_rows(
        7,
        capped(32_767).starting_at(1068),
        vec![clean(); 3],
        &[1069, 1069, 1069],
    );
}

// Never above maxBandwidth; : above 1 Gbps the cap is the highest row
// whose rate is within it, 1500 Mbps row 1005
#[udpstp_req::req("RFC9946-6.1-6", "RFC9946-11-5", "RFC9946-11-L2")]
#[test]
fn a_cap_above_1_gbps_stops_at_the_row_within_it() {
    check_rows(
        10,
        capped(1500).starting_at(1003),
        vec![clean(); 3],
        &[1004, 1005, 1005],
    );
}

// A start row at the cap is no coercion.
#[test]
fn a_start_row_at_the_cap_is_not_coerced() {
    let outcome = check_rows(8, capped(100).starting_at(100), vec![clean()], &[100]);
    outcome.check(
        row_coercion(&outcome).is_none(),
        &format!("{:?}", row_coercion(&outcome)),
    );
}
