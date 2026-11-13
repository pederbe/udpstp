//! Packet marking (RFC 9946 §7.1, §7.2.1; docs/spec.md story 11): the
//! Client asks for a DSCP in dscpEcn, the Server allows it or coerces it to
//! 0, and both ends mark their Test Connection's traffic with the agreed
//! DSCP, ECN always Not-ECT.

use super::harness;

use std::time::Duration;

use crate::protocol::{Direction, DscpSet};
use harness::{ActivationParameter, Coercion, Scenario};

const RUN: Duration = Duration::from_secs(1);

/// DSCP 46, Expedited Forwarding: a priority class.
const EF: u8 = 46;

fn dscp_ecn_coercion(requested: u8, coerced: u8) -> Coercion {
    Coercion {
        parameter: ActivationParameter::DscpEcn,
        requested: requested.into(),
        coerced: coerced.into(),
    }
}

// docs/spec.md story 11
#[udpstp_req::req("RFC9946-7.2.1-L1", "RFC9946-7.3-4")]
#[test]
fn a_server_allowing_only_the_default_coerces_a_priority_dscp_to_0() {
    for direction in [Direction::Downstream, Direction::Upstream] {
        let outcome = Scenario::new(1)
            .server_allows_unauthenticated()
            .client_unauthenticated()
            .client_direction(direction)
            .client_config(|c| c.dscp = EF)
            .run_for(RUN);

        let expected = vec![dscp_ecn_coercion(EF << 2, 0)];
        outcome.check(
            outcome.server_coercions() == expected && outcome.client_coercions() == expected,
            "both ends report dscpEcn coerced to 0",
        );
        outcome.check(
            outcome.dscp_ecn == [(1, 0), (0, 0)] || outcome.dscp_ecn == [(0, 0), (1, 0)],
            "both ends send unmarked",
        );
    }
}

// RFC 9946 §7.1 (dscpEcn); docs/spec.md story 11 (a DSCP for the load)
#[udpstp_req::req("RFC9946-7.2-1")]
#[test]
fn an_allowed_dscp_marks_both_ends_traffic() {
    let outcome = Scenario::new(2)
        .server_allows_unauthenticated()
        .server_config(|config| config.allowed_dscp = DscpSet::of([0, EF]).unwrap())
        .client_unauthenticated()
        .client_config(|c| c.dscp = EF)
        .run_for(RUN);

    outcome.check(
        outcome.server_coercions().is_empty() && outcome.client_coercions().is_empty(),
        "no coercion",
    );
    let mut marked = outcome.dscp_ecn.clone();
    marked.sort();
    outcome.check(marked == [(0, EF << 2), (1, EF << 2)], "both ends mark EF");
}

// docs/spec.md (Operator Keys aren't held to the Server's DSCP policy)
#[test]
fn an_operator_key_may_ask_for_any_dscp() {
    let outcome = Scenario::new(3)
        .operator(1, b"operator key")
        .client_config(|c| c.dscp = EF)
        .run_for(RUN);

    outcome.check(outcome.server_coercions().is_empty(), "no coercion");
    let mut marked = outcome.dscp_ecn.clone();
    marked.sort();
    outcome.check(marked == [(0, EF << 2), (1, EF << 2)], "both ends mark EF");
}

/// CS6: network control.
const CS6: u8 = 48;

// docs/spec.md (Operator Keys are bounded too: network control stays the
// network's)
#[test]
fn an_operator_key_may_not_ask_for_network_control_by_default() {
    let outcome = Scenario::new(4)
        .operator(1, b"operator key")
        .client_config(|c| c.dscp = CS6)
        .run_for(RUN);

    outcome.check(
        outcome.server_coercions() == vec![dscp_ecn_coercion(CS6 << 2, 0)],
        "CS6 is coerced to 0",
    );
}

//  (a Server that can't mark traffic allows only 0, so its answer is
// what it sends)
#[udpstp_req::req("RFC9946-7.2.1-L1")]
#[test]
fn a_server_that_cant_mark_allows_only_0_even_to_operators() {
    let outcome = Scenario::new(5)
        .operator(1, b"operator key")
        .server_config(|config| config.operator_allowed_dscp = DscpSet::DEFAULT_ONLY)
        .client_config(|c| c.dscp = EF)
        .run_for(RUN);

    outcome.check(
        outcome.server_coercions() == vec![dscp_ecn_coercion(EF << 2, 0)],
        "EF is coerced to 0",
    );
    outcome.check(
        outcome.dscp_ecn.iter().all(|&(_, marked)| marked == 0),
        "nothing is marked",
    );
}
