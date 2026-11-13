//! The Maximum IP-Layer Capacity of a Search Test through a drop-tail
//! bottleneck, in simulated Tests. Through a queue whose delay
//! variation stays below upperThresh, only loss lowers the row, and it
//! comes back a Trial Interval late: after its first overshoot the search
//! cycles a row or two around the bottleneck's rate, on average above it,
//! and every Sub-Interval there loses a share of its Load PDUs that grows
//! with the row's step against the rate. Where that share is above the
//! default Performance Criterion (a Loss Ratio of 1%), no Sub-Interval at
//! the bottleneck's rate meets it, and the Maximum IP-Layer Capacity falls
//! to the last Sub-Interval of the search's climb that did, at the latest
//! the one in which it first overshot: a cliff, not a gradual bias. Above
//! the 1 Gbps row, where the rows are 100 Mbps apart, that is common, and
//! at the points tested the share grew with the path's delay: 1885 Mbit/s
//! of 2.5 Gbit/s through 20 ms, and Fast Mode's last Sub-Interval,
//! 893 Mbit/s, at 1 to 2 Gbit/s. Below it the rows are 1 Mbps apart and,
//! from a few hundred Mbit/s up, the share is near 1%, which the path's
//! delay moves either way: at 500 Mbit/s through 20 ms, 5 ms each way
//! leaves 0.70-0.92% and the bottleneck's rate, 20 ms each way 1.04% and
//! 287 Mbit/s, and 40 ms each way 0.83-0.92% and the rate again. After the
//! first overshoot, a delay variation between lowThresh and upperThresh
//! can hold the search just below the bottleneck with Hold Intervals, with
//! nothing lost. The netns tier measured the same through tbf on testhost,
//! 1960 Mbit/s of 2.5 Gbit/s.
//!
//! `through_drop_tail_queues_sweep` prints the Maximum and the Loss Ratios
//! over a sweep of rates, queue depths and path delays, the note's tables;
//! it is ignored, so run it by hand:
//!
//! ```text
//! cargo test --release -p udpstp --bin udpstp tests::protocol::maximum_through_drop_tail_queues -- --ignored --nocapture
//! ```

use super::harness;

use std::time::Duration;

use crate::protocol::{
    DataEvent, Direction, Evaluation, FASTER_SUB_INTERVAL_MARGIN_PERCENT, IntervalClass, IpVersion,
    LoadSender, MeasurementResults, PerformanceCriterion, SubIntervalMeasurement,
    SubIntervalsAfterMaximumMissed, TableShape,
};
use harness::status_script::classifications;
use harness::{Outcome, Scenario};

const MBPS: u64 = 1_000_000;

/// A Search Test of 10 s through a bottleneck of `rate_mbps` whose queue
/// holds `queue_ms` of it plus a token bucket's 32 kB burst, as the netns
/// tier's tbf does, `one_way_ms` each way. Above the 1 Gbps row the
/// Server's MTU is 9000 bytes, so its Load PDUs are 3125 bytes at 2.5
/// Gbit/s rather than 1500, which the core tests' time budget needs; the
/// search moves through the same rows either way.
fn search(rate_mbps: u64, queue_ms: u64, one_way_ms: u64, direction: Direction) -> Outcome {
    let buffer = rate_mbps * MBPS / 8 * queue_ms / 1000 + 32_768;
    let scenario = Scenario::new(215)
        .server_allows_unauthenticated()
        .client_unauthenticated()
        .client_direction(direction)
        .client_config(|c| c.test_int_time = 10)
        .bottleneck(rate_mbps * MBPS, buffer)
        .path_delay(Duration::from_millis(one_way_ms), Duration::ZERO);
    if rate_mbps > 1000 {
        scenario.server_config(|c| {
            c.table = TableShape {
                jumbo: true,
                traditional_mtu: false,
                mtu: 9000,
            };
        })
    } else {
        scenario
    }
    .run_for(Duration::from_millis(11_500))
}

/// The Sub-Intervals the Client measured.
fn sub_intervals(outcome: &Outcome) -> Vec<SubIntervalMeasurement> {
    outcome
        .client_data_events
        .iter()
        .filter_map(|(_, e)| match e {
            DataEvent::SubIntervalMeasured(m) => Some(*m),
            _ => None,
        })
        .collect()
}

fn loss_ratio(m: &SubIntervalMeasurement) -> f64 {
    f64::from(m.loss) / m.datagrams_sent().max(1) as f64
}

fn mbps(m: &SubIntervalMeasurement) -> f64 {
    m.ip_layer_capacity as f64 / MBPS as f64
}

/// The Sub-Interval of the Maximum IP-Layer Capacity at the default
/// Performance Criterion, if any met it: the earliest of equal ones, as
/// the Client's results take it (`max_by_key` keeps the last, so the
/// Sub-Intervals are searched from the end).
fn maximum(subs: &[SubIntervalMeasurement]) -> Option<SubIntervalMeasurement> {
    subs.iter()
        .rev()
        .filter(|m| PerformanceCriterion::DEFAULT.is_met_by(m))
        .max_by_key(|m| m.ip_layer_capacity)
        .copied()
}

/// What the Client's results find of Sub-Intervals after the Maximum's
/// that all missed the default Performance Criterion, at the default 5 %
/// margin: a faster one, or a slow last one.
fn after_maximum(outcome: &Outcome, direction: Direction) -> Vec<SubIntervalsAfterMaximumMissed> {
    let events: Vec<DataEvent> = outcome.client_data_events.iter().map(|(_, e)| *e).collect();
    let evaluation = Evaluation {
        test_interval: Duration::from_secs(10),
        sub_interval: Duration::from_secs(1),
        ip_version: IpVersion::V4,
        criterion: PerformanceCriterion::DEFAULT,
        first_mode: None,
    };
    MeasurementResults::of_test_connection(&events, &evaluation).sub_intervals_after_maximum_missed(
        PerformanceCriterion::DEFAULT,
        FASTER_SUB_INTERVAL_MARGIN_PERCENT,
        LoadSender::of(direction, &[&events]),
    )
}

fn show(subs: &[SubIntervalMeasurement]) -> String {
    subs.iter()
        .map(|m| format!("{:.0}/{:.2}%", mbps(m), 100.0 * loss_ratio(m)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Checks that the Maximum IP-Layer Capacity at the default Performance
/// Criterion is the bottleneck's rate, within 1%.
fn maximum_is_the_rate(rate: u64, queue_ms: u64) {
    let outcome = search(rate, queue_ms, 5, Direction::Downstream);
    let subs = sub_intervals(&outcome);
    outcome.check(
        maximum(&subs).is_some_and(|m| mbps(&m) >= 0.99 * rate as f64)
            && after_maximum(&outcome, Direction::Downstream).is_empty(),
        &format!("{rate} Mbit/s, {queue_ms} ms: {}", show(&subs)),
    );
}

//  (conformance: shallow-queue-search-alternates-at-the-bottleneck):
// at 2.5 Gbit/s through 20 ms of queue, above the 1 Gbps row, the search
// cycles a row or two around the bottleneck and every Sub-Interval at its
// rate loses more than 1%, so the Maximum IP-Layer Capacity comes from a
// Sub-Interval of the climb, under 80% of the bottleneck's rate
#[test]
fn above_the_gigabit_row_a_20_ms_queue_leaves_the_maximum_on_the_climb() {
    let outcome = search(2500, 20, 5, Direction::Downstream);
    let subs = sub_intervals(&outcome);
    let at_rate: Vec<&SubIntervalMeasurement> =
        subs.iter().filter(|m| mbps(m) >= 0.99 * 2500.0).collect();
    let max = maximum(&subs);
    let first_at_rate = at_rate.first().map(|m| m.sub_int_seq_no);
    outcome.check(
        at_rate.len() >= 3
            && at_rate.iter().all(|m| loss_ratio(m) > 0.01)
            && max
                .is_some_and(|m| mbps(&m) < 0.8 * 2500.0 && Some(m.sub_int_seq_no) < first_at_rate)
            // : and the Client's results warn of it, of the faster
            // Sub-Intervals after the Maximum's, not of one before it, all
            // of the climb, nor of a slow last one: the cycle stays at the
            // rate.
            && after_maximum(&outcome, Direction::Downstream)
                .iter()
                .all(|found| {
                    found.faster.is_some()
                        && found.faster_before.is_none()
                        && found.slow_last.is_none()
                })
            && !after_maximum(&outcome, Direction::Downstream).is_empty(),
        &format!("2.5 Gbit/s, 20 ms: {}", show(&subs)),
    );
    // A cycle a row or two around the bottleneck, row 1015 (2.5 Gbps),
    // with Errored Intervals in it: after 8 s, rows 1013 to 1018 only.
    let late: Vec<_> = classifications(&outcome).into_iter().skip(160).collect();
    outcome.check(
        late.iter()
            .all(|c| c.row.is_some_and(|row| (1013..=1018).contains(&row)))
            && late.iter().any(|c| c.class == IntervalClass::Errored),
        &format!(
            "rows after 8 s: {:?}",
            late.iter().map(|c| c.row).collect::<Vec<_>>()
        ),
    );
}

// : at 2.5 Gbit/s through 1 ms of queue, over a 10 ms round trip, the
// loss comes back soon enough that the cycle loses under 1%, and a
// Sub-Interval at the bottleneck's rate meets the Performance Criterion
// (over a 40 ms round trip it doesn't)
#[test]
fn through_1_ms_of_queue_over_a_short_path_the_maximum_is_the_bottlenecks_rate() {
    maximum_is_the_rate(2500, 1);
}

// : below the 1 Gbps row, 500 Mbit/s through 20 ms over a 10 ms round
// trip, the rows are 1 Mbps apart, the cycle loses under 1%, and a
// Sub-Interval at the bottleneck's rate meets the Performance Criterion
// (over a 40 ms round trip it loses 1.04% and doesn't)
#[test]
fn below_the_gigabit_row_over_a_short_path_the_maximum_is_the_bottlenecks_rate() {
    maximum_is_the_rate(500, 20);
}

// : the Maximum IP-Layer Capacity at the default Performance
// Criterion and the Loss Ratios at the bottleneck's rate over a sweep of
// rates through 20 ms of queue, of queue depths at 2.5 Gbit/s, of path
// delays at 1.2 and 2.5 Gbit/s, and at points between those lines, where
// the rate, the depth and the delay each change the result: the research
// note's tables (a measurement run by hand; see the module docs)
#[test]
#[ignore = "a manual diagnostic sweep: see the module docs"]
fn through_drop_tail_queues_sweep() {
    let line = |rate: u64, queue_ms: u64, one_way_ms: u64, direction: Direction| {
        let outcome = search(rate, queue_ms, one_way_ms, direction);
        let subs = sub_intervals(&outcome);
        let highest = subs.iter().map(mbps).fold(0.0, f64::max);
        let at_rate: Vec<String> = subs
            .iter()
            .filter(|m| mbps(m) >= 0.99 * rate as f64)
            .map(|m| format!("{:.2}%", 100.0 * loss_ratio(m)))
            .collect();
        let rows: Vec<u16> = classifications(&outcome)
            .into_iter()
            .skip(160)
            .filter_map(|c| c.row)
            .collect();
        let max = maximum(&subs);
        let found = after_maximum(&outcome, direction);
        // The Sub-Intervals after the Maximum's, each of which lost too
        // much to meet the criterion.
        let after: Vec<String> = subs
            .iter()
            .filter(|m| max.is_some_and(|max| m.sub_int_seq_no > max.sub_int_seq_no))
            .map(|m| format!("{:.0}/{:.2}%", mbps(m), 100.0 * loss_ratio(m)))
            .collect();
        let maximum = max.map_or_else(
            || "none".to_string(),
            |m| {
                format!(
                    "{:.0} Mbit/s in Sub-Interval {} ({:.0}%)",
                    mbps(&m),
                    m.sub_int_seq_no,
                    100.0 * mbps(&m) / rate as f64
                )
            },
        );
        eprintln!(
            "SWEEP {rate} Mbit/s, {queue_ms} ms of queue, {one_way_ms} ms each way, {direction:?}: Maximum {maximum}; SubIntervalsAfterMaximumMissed faster before {} faster {} slow last {}; Highest {highest:.0} Mbit/s; Loss Ratios at the rate [{}]; after the Maximum's Sub-Interval [{}]; rows after 8 s {}-{}; every Sub-Interval [{}]",
            found.iter().any(|f| f.faster_before.is_some()),
            found.iter().any(|f| f.faster.is_some()),
            found.iter().any(|f| f.slow_last.is_some()),
            at_rate.join(" "),
            after.join(" "),
            rows.iter().min().copied().unwrap_or(0),
            rows.iter().max().copied().unwrap_or(0),
            show(&subs),
        );
    };
    for rate in [
        500, 800, 1000, 1100, 1200, 1500, 2000, 2500, 3000, 4000, 5000,
    ] {
        line(rate, 20, 5, Direction::Downstream);
    }
    for queue_ms in [1, 2, 5, 10, 25, 28, 35, 50, 80, 100, 200] {
        line(2500, queue_ms, 5, Direction::Downstream);
    }
    for one_way_ms in [1, 2, 10, 20, 40] {
        line(2500, 20, one_way_ms, Direction::Downstream);
        line(1200, 20, one_way_ms, Direction::Downstream);
    }
    for (rate, queue_ms) in [
        (1000, 2),
        (1000, 50),
        (1200, 1),
        (1200, 2),
        (1200, 50),
        (2000, 2),
        (2000, 50),
        (4000, 2),
        (4000, 50),
    ] {
        line(rate, queue_ms, 5, Direction::Downstream);
    }
    for one_way_ms in [20, 40] {
        for (rate, queue_ms) in [(500, 20), (800, 20), (2500, 1)] {
            line(rate, queue_ms, one_way_ms, Direction::Downstream);
        }
    }
    line(2500, 20, 5, Direction::Upstream);
}
