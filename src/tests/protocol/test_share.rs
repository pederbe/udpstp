//! One Test's share of the Admitted Rate Cap: an activated Test
//! holds its whole maxBandwidth for its whole Test Interval, whatever it
//! sends, so a Test without an Operator Key may state at most
//! `max_test_share` of the cap, all its Test Connections together.

use super::harness;

use std::time::Duration;

use crate::protocol::{Event, SetupRejection};
use harness::{Outcome, Scenario};

const KEY: &[u8] = b"a share key";

fn refused(outcome: &Outcome) -> usize {
    outcome
        .server_events
        .iter()
        .filter(|e| {
            matches!(
                e,
                Event::SetupRequestRejected {
                    reason: SetupRejection::CapacityExceeded,
                    ..
                }
            )
        })
        .count()
}

fn activated(outcome: &Outcome) -> usize {
    (0..outcome.clients_events.len())
        .filter(|&i| outcome.client_activated_direction(i).is_some())
        .count()
}

/// A 100 Mbps cap, half of it the most one Test may hold (the default).
fn capped(scenario: Scenario) -> Scenario {
    scenario.server_config(|c| c.admitted_rate_cap = Some(100))
}

// maxBandwidth is admitted against the Server's capacity;
#[udpstp_req::req("RFC9946-6.1-L7", "RFC9946-11-L2")]
#[test]
fn a_consumer_s_test_may_hold_half_the_cap() {
    let at_half = capped(Scenario::new(1).mode1(3, KEY))
        .client_max_bandwidth(50)
        .run_for(Duration::from_millis(100));
    at_half.check(
        activated(&at_half) == 1 && refused(&at_half) == 0,
        "50 of 100 Mbps: within the share",
    );
    let over = capped(Scenario::new(2).mode1(3, KEY))
        .client_max_bandwidth(60)
        .run_for(Duration::from_millis(100));
    over.check(
        activated(&over) == 0 && refused(&over) == 1,
        "60 of 100 Mbps: refused with CAPEXC",
    );
}
