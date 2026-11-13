//! Downstream Multi-Connection aggregation (docs/spec.md stories 61 and 63;
//! ): the Client's Load Receivers keep their Sub-Intervals to one grid,
//! so the Sub-Intervals of the same number cover the same time, and their
//! sum never inflates the capacity even when the Test Connections start
//! apart.

use super::harness;

use std::time::Duration;

use crate::protocol::{
    Direction, Evaluation, IpVersion, MeasurementResults, PerformanceCriterion, SocketId,
    SubIntervalMeasurement,
};
use harness::{DataEvent, Outcome, Scenario};

const KEY: &[u8] = b"a multi-connection key";
const BOTTLENECK: u64 = 10_000_000;

/// Two Test Connections, one to each of two Servers, sharing the Client's
/// 10 Mbps access link; the first Server's engine starts 400 ms late, so
/// the first Test Connection's load arrives that much after the second's.
fn test(seed: u64) -> Scenario {
    Scenario::new(seed)
        .mode1(3, KEY)
        .client_config(|c| c.test_int_time = 5)
        .add_server(|_| {})
        .path_delay(Duration::from_millis(10), Duration::ZERO)
        .bottleneck(BOTTLENECK, 60_000)
        .bottleneck_per_receiver()
        .server_engine_stall(Duration::ZERO, Duration::from_millis(400))
        .coordinated(2, 2)
}

fn measured(outcome: &Outcome, socket: u32) -> Vec<SubIntervalMeasurement> {
    outcome
        .client_data_events
        .iter()
        .filter_map(|(s, e)| match e {
            DataEvent::SubIntervalMeasured(m) if *s == SocketId(socket) => Some(*m),
            _ => None,
        })
        .collect()
}

/// The Client's results of both Test Connections together: their
/// Sub-Intervals summed by number.
fn together(outcome: &Outcome, direction: Direction) -> MeasurementResults {
    let each: Vec<Vec<DataEvent>> = [1, 2]
        .map(|socket| {
            outcome
                .client_data_events
                .iter()
                .filter(|(s, _)| *s == SocketId(socket))
                .map(|(_, e)| *e)
                .collect()
        })
        .into();
    let each: Vec<&[DataEvent]> = each.iter().map(Vec::as_slice).collect();
    MeasurementResults::of_test(
        &each,
        direction,
        false,
        &Evaluation {
            test_interval: Duration::from_secs(5),
            sub_interval: Duration::from_secs(1),
            ip_version: IpVersion::V4,
            criterion: PerformanceCriterion::DEFAULT,
            first_mode: None,
        },
    )
}

/// The summed Sub-Intervals that were measured.
fn summed(results: &MeasurementResults) -> Vec<SubIntervalMeasurement> {
    results.sub_intervals.iter().flatten().copied().collect()
}

// docs/spec.md story 61 (Downstream results aggregated on one common timeline,
// so misaligned Sub-Intervals never inflate the summed capacity); RFC 9097
// §5.3 (Sub-Intervals of equal duration dt)
#[udpstp_req::req("RFC9946-4-L1")]
#[test]
fn the_summed_capacity_never_exceeds_the_shared_bottleneck() {
    let outcome = test(1).run_for(Duration::from_secs(8));
    let (first, second) = (measured(&outcome, 1), measured(&outcome, 2));
    outcome.check(
        !first.is_empty()
            && !second.is_empty()
            && first
                .iter()
                .chain(&second)
                .all(|m| (1..=5).contains(&m.sub_int_seq_no)),
        &format!("both measured, on the grid's numbers: {first:?} {second:?}"),
    );
    let summed = summed(&together(&outcome, Direction::Downstream));
    // A Sub-Interval may hold a Trial Interval more than its time: 5 %.
    let limit = BOTTLENECK * 105 / 100;
    outcome.check(
        summed.iter().all(|m| m.ip_layer_capacity <= limit),
        &format!(
            "at most the bottleneck: {:?}",
            summed
                .iter()
                .map(|m| (
                    m.sub_int_seq_no,
                    m.ip_layer_capacity,
                    m.active_test_connections
                ))
                .collect::<Vec<_>>()
        ),
    );
    // docs/spec.md story 63 (each Sub-Interval shows its active count)
    outcome.check(
        summed.iter().any(|m| m.active_test_connections == 2),
        "both Test Connections active together",
    );
}

// docs/spec.md story 61; : a Sub-Interval an engine stall stretched
// past its end holds more than its time, and is no measurement
#[test]
fn a_stalled_sub_interval_is_not_measured() {
    let outcome = test(2)
        .client_engine_stall(Duration::from_millis(2200), Duration::from_millis(1300))
        .run_for(Duration::from_secs(8));
    let summed = summed(&together(&outcome, Direction::Downstream));
    let limit = BOTTLENECK * 105 / 100;
    outcome.check(
        summed.iter().all(|m| m.ip_layer_capacity <= limit),
        &format!(
            "no Sub-Interval inflated by the stall: {:?}",
            summed
                .iter()
                .map(|m| (m.sub_int_seq_no, m.ip_layer_capacity))
                .collect::<Vec<_>>()
        ),
    );
}

// docs/spec.md story 62 (Upstream Multi-Connection results are marked
// approximate, with the largest offset between Test Connections shown);
//
#[udpstp_req::req("RFC9946-4-L1")]
#[test]
fn upstream_sub_intervals_are_summed_by_number_and_their_offset_measured() {
    // The first Server, the Load Receiver, starts 400 ms late: its
    // Sub-Intervals lie that much after the second's.
    let outcome = test(3)
        .client_direction(crate::protocol::Direction::Upstream)
        .run_for(Duration::from_secs(8));
    let per_connection = [measured(&outcome, 1), measured(&outcome, 2)];
    outcome.check(
        per_connection.iter().all(|m| !m.is_empty()),
        "both Servers reported Sub-Intervals",
    );
    let results = together(&outcome, Direction::Upstream);
    let offset = results.approximate.unwrap_or_default();
    outcome.check(
        offset >= Duration::from_millis(200) && offset <= Duration::from_millis(600),
        &format!("the offset shows the late start: {offset:?}"),
    );
    let summed = summed(&results);
    outcome.check(
        summed.iter().any(|m| m.active_test_connections == 2),
        "each Sub-Interval shows its active count",
    );
}
