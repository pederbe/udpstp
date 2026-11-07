//! Load Rate Adjustment Algorithm B (RFC 9946 §7; Y.1540 Amd 2 Annex B
//! Figure B.1; docs/spec.md "Load Rate Adjustment Algorithms"; ADR 0005).
//!
//! The row traces are derived by hand from the flowchart text and the spec,
//! not from our model: a scripted Client reports chosen Trial Intervals and
//! the Server's classifications carry the row it chose. With the defaults,
//! highSpeedDelta is 10 rows and slowAdjThresh 3:
//! - Fast Mode (below the 1 Gbps row, before Congestion Confirmed): a Clean
//!   Interval adds 10 rows, capped at the 1 Gbps row;
//! - an Errored Interval takes a row off; the third back to back confirms
//!   Congestion and, below the 1 Gbps row, takes 3 x 10 rows off instead;
//! - any interval that isn't errored ends the run of Errored Intervals;
//! - after Congestion Confirmed, every change is one row;
//! - a Hold Interval changes nothing;
//! - rows stay between 0 and the table's last row, 1000.

mod harness;

use std::time::Duration;

use harness::status_script::{
    DEFAULTS, Params, Report, classifications, clean, errored, hold, reported_then_stopped,
};
use harness::{Outcome, RowRequest, Scenario, TraceEntry};
use udpstp_core::{Classification, DataEvent, Direction, EndReason, IntervalClass};
use udpstp_wire::StatusPdu;

fn rows(seed: u64, params: Params, reports: Vec<Report>) -> (Outcome, Vec<Option<u16>>) {
    let (outcome, got) = reported_then_stopped(Scenario::new(seed), params, reports);
    let rows = got.iter().map(|c| c.row).collect();
    (outcome, rows)
}

fn check_rows(seed: u64, params: Params, reports: Vec<Report>, expected: &[u16]) {
    let (outcome, got) = rows(seed, params, reports);
    let expected: Vec<Option<u16>> = expected.iter().copied().map(Some).collect();
    outcome.check(
        got == expected,
        &format!("rows {got:?}, expected {expected:?}"),
    );
}

// Fast Mode: highSpeedDelta rows per Clean Interval; docs/spec.md (from the
// first row by default)
#[udpstp_req::req("RFC9097-8.1-L9", "RFC9097-8.1-L10", "RFC9097-10-L1")]
#[test]
fn in_fast_mode_each_clean_interval_adds_high_speed_delta_rows() {
    check_rows(1, DEFAULTS, vec![clean(); 3], &[10, 20, 30]);
    let wider = Params {
        high_speed_delta: 25,
        ..DEFAULTS
    };
    check_rows(2, wider, vec![clean(); 2], &[25, 50]);
}

// Rows are bounded at 0 and the table's maximum.
// (conformance: fast-mode-stops-at-the-gigabit-row)
#[udpstp_req::req("RFC9097-8.1-L12", "RFC9097-10-5")]
#[test]
fn rows_stay_within_the_table_and_fast_mode_stops_at_1_gbps() {
    // Fast Mode stops at the 1 Gbps row; above it each Clean Interval
    // adds one row ( gave the table rows beyond it).
    check_rows(
        3,
        DEFAULTS.starting_at(985),
        vec![clean(); 3],
        &[995, 1000, 1001],
    );
    check_rows(4, DEFAULTS, vec![errored(); 2], &[0, 0]);
}

// ADR 0005; docs/spec.md (Congestion Confirmed drops 3 x highSpeedDelta rows
// once, then Single-Step Mode)
#[udpstp_req::req(
    "RFC9946-5.1-L1",
    "RFC9946-8.1-L2",
    "RFC9097-8.1-L11",
    "RFC9097-8.1-L14",
    "RFC9097-8.1-L15",
    "RFC9097-8.1-L17",
    "RFC9097-8.1-L13",
    "RFC9946-5.1-7",
    "RFC9946-8.1-L1"
)]
#[test]
fn congestion_is_confirmed_by_back_to_back_errored_intervals() {
    check_rows(
        5,
        DEFAULTS.starting_at(50),
        vec![errored(), errored(), errored(), clean(), clean(), errored()],
        &[49, 48, 18, 19, 20, 19],
    );
}

// ADR 0005 (a non-errored interval resets the count, so intermittent
// errors don't confirm Congestion)
#[udpstp_req::req(
    "RFC9946-5.1-L1",
    "RFC9946-8.1-L2",
    "RFC9097-8.1-L16",
    "RFC9097-8.1-L17",
    "RFC9946-8.1-L1"
)]
#[test]
fn intermittent_errored_intervals_do_not_confirm_congestion() {
    check_rows(
        6,
        DEFAULTS.starting_at(50),
        vec![errored(), hold(), errored(), hold(), errored(), clean()],
        &[49, 49, 48, 48, 47, 57],
    );
    check_rows(
        7,
        DEFAULTS.starting_at(50),
        vec![errored(), clean(), errored(), errored(), errored()],
        &[49, 59, 58, 57, 27],
    );
}

// ADR 0005 (algorithm B never clears the latch)
#[udpstp_req::req("RFC9946-8.1-L3", "RFC9097-8.1-L11")]
#[test]
fn algorithm_b_never_leaves_single_step_mode() {
    let mut reports = vec![errored(); 3];
    reports.extend(vec![clean(); 5]);
    reports.extend(vec![errored(); 3]);
    reports.push(clean());
    check_rows(
        8,
        DEFAULTS.starting_at(50),
        reports,
        &[49, 48, 18, 19, 20, 21, 22, 23, 22, 21, 20, 21],
    );
}

#[udpstp_req::req(
    "RFC9946-7.1-3",
    "RFC9946-7.1-4",
    "RFC9946-7.2.2-5",
    "RFC9097-2-5",
    "RFC9097-2-6",
    "RFC9097-8.1-L4",
    "RFC9097-10-L1"
)]
#[test]
fn the_default_search_starts_at_row_0() {
    check_rows(13, DEFAULTS, vec![clean(); 2], &[10, 20]);
    check_rows(14, DEFAULTS.starting_at(0), vec![clean(); 2], &[10, 20]);
}

// docs/spec.md (rows are bounded at 0; the confirming drop happens below the
// 1 Gbps row, whatever the rows before it)
#[udpstp_req::req("RFC9097-8.1-L14", "RFC9946-5.1-7")]
#[test]
fn the_confirming_drop_stops_at_row_0_and_follows_the_row_it_starts_from() {
    check_rows(
        15,
        DEFAULTS.starting_at(20),
        vec![errored(); 3],
        &[19, 18, 0],
    );
    let mut reports = vec![clean()];
    reports.extend(vec![errored(); 3]);
    check_rows(
        16,
        DEFAULTS.starting_at(1000),
        reports,
        &[1001, 1000, 999, 969],
    );
}

// docs/spec.md (above the 1 Gbps row the algorithm moves one row at a time: a
// Clean Interval adds one, and a Congestion confirmation takes one off,
// not 3 x highSpeedDelta); ,
#[udpstp_req::req("RFC9097-8.1-L12")]
#[test]
fn above_the_gigabit_row_every_step_is_one_row() {
    check_rows(
        17,
        DEFAULTS.starting_at(1010),
        vec![clean(), clean(), errored(), errored(), errored(), clean()],
        &[1011, 1012, 1011, 1010, 1009, 1010],
    );
}

// RFC 9946 §7.1 (highSpeedDelta and slowAdjThresh from the Activation
// Request)
#[udpstp_req::req("RFC9946-5.1-8", "RFC9946-8.1-7", "RFC9097-8.1-L8")]
#[test]
fn the_accepted_parameters_drive_the_algorithm() {
    let params = Params {
        high_speed_delta: 5,
        slow_adj_thresh: 2,
        ..DEFAULTS.starting_at(50)
    };
    check_rows(
        9,
        params,
        vec![clean(), errored(), errored()],
        &[55, 54, 39],
    );
}

#[udpstp_req::req("RFC9946-4.1-L1", "RFC9946-7.2.2-5")]
#[test]
fn a_fixed_rate_test_holds_its_row() {
    let outcome = Scenario::new(10)
        .operator(7, b"an operator key")
        .client_direction(Direction::Downstream)
        .client_requests_row(RowRequest::FixedRate(5))
        .run_for(Duration::from_millis(500));
    let got = classifications(&outcome);
    outcome.check(
        got.len() > 5 && got.iter().all(|c| c.row.is_none()),
        &format!("{got:?}"),
    );
}

const MBPS: u64 = 1_000_000;

/// The rows the Server chose in the Trial Intervals after `from`.
fn rows_after(outcome: &Outcome, from: usize) -> Vec<u16> {
    classifications(outcome)
        .iter()
        .skip(from)
        .filter_map(|c| c.row)
        .collect()
}

// docs/spec.md  (a Downstream Search Test in the simulator converges near
// a known bottleneck)
#[udpstp_req::req("RFC9097-8.1-L14")]
#[test]
fn a_downstream_search_converges_near_the_bottleneck() {
    // 50 Mbit/s with 10 ms of buffer. Row N is N Mbit/s.
    let outcome = Scenario::new(11)
        .server_allows_unauthenticated()
        .client_unauthenticated()
        .client_direction(Direction::Downstream)
        .bottleneck(50 * MBPS, 62_500)
        .path_delay(Duration::from_millis(5), Duration::ZERO)
        .run_for(Duration::from_secs(6));

    // After 3 s (60 Trial Intervals) the search has found the bottleneck.
    let later = rows_after(&outcome, 60);
    outcome.check(
        later.len() > 40 && later.iter().all(|row| (40..=55).contains(row)),
        &format!("rows after 3 s: {later:?}"),
    );
    // Congestion was confirmed once, with one 3 x highSpeedDelta drop.
    let all = rows_after(&outcome, 0);
    let drops: Vec<i32> = all
        .windows(2)
        .map(|w| i32::from(w[1]) - i32::from(w[0]))
        .filter(|d| *d < -1)
        .collect();
    outcome.check(drops == [-30], &format!("drops {drops:?} in {all:?}"));
}

// ADR 0008 (over-rate policing); docs/spec.md  (an Upstream search
// through a deep buffer is not stopped: what queued before a lower rate
// was assigned keeps arriving until the queue drains)
#[test]
fn an_upstream_search_through_a_deep_buffer_is_not_stopped_over_rate() {
    for (seed, rate, buffer, one_way) in [(17, 20 * MBPS, 500_000, 5), (18, 5 * MBPS, 125_000, 50)]
    {
        let outcome = Scenario::new(seed)
            .server_allows_unauthenticated()
            .client_unauthenticated()
            .client_direction(Direction::Upstream)
            .bottleneck(rate, buffer)
            .path_delay(Duration::from_millis(one_way), Duration::ZERO)
            .run_for(Duration::from_secs(12));
        let ended: Vec<&DataEvent> = outcome
            .server_data_events
            .iter()
            .map(|(_, e)| e)
            .filter(|e| matches!(e, DataEvent::Ended(_)))
            .collect();
        outcome.check(
            ended == [&DataEvent::Ended(EndReason::Completed)],
            &format!("{rate} bit/s, {buffer} bytes, {one_way} ms: {ended:?}"),
        );
    }
}

/// IP-layer bit/s of a Sending Rate Structure: each transmitter sends
/// `burst` datagrams per interval, plus the add-on per second interval.
fn ip_layer_bps(s: &StatusPdu) -> u64 {
    let sr = &s.sending_rate;
    let per_second = |bytes: u32, count: u32, interval_us: u32| {
        if bytes == 0 || count == 0 || interval_us == 0 {
            return 0;
        }
        u64::from((bytes + 28) * 8 * count) * 1_000_000 / u64::from(interval_us)
    };
    per_second(sr.udp_payload1, sr.burst_size1, sr.tx_interval1)
        + per_second(sr.udp_payload2, sr.burst_size2, sr.tx_interval2)
        + per_second(sr.udp_addon2, 1, sr.tx_interval2)
}

// docs/spec.md  (Upstream, the algorithm runs before each Status PDU is
// built and its row goes out in that Status PDU; the search converges near
// a known bottleneck)
#[udpstp_req::req("RFC9946-8.1-1")]
#[test]
fn an_upstream_search_converges_near_the_bottleneck() {
    let outcome = Scenario::new(12)
        .server_allows_unauthenticated()
        .client_unauthenticated()
        .client_direction(Direction::Upstream)
        .bottleneck(50 * MBPS, 62_500)
        .path_delay(Duration::from_millis(5), Duration::ZERO)
        .run_for(Duration::from_secs(6));

    let got: Vec<Classification> = classifications(&outcome);
    let sent: Vec<StatusPdu> = outcome
        .trace
        .iter()
        .filter(|t: &&TraceEntry| t.len == 204 && t.payload.starts_with(&[0xFE, 0xED]))
        .filter(|t| t.destination == harness::CLIENT_ADDR.parse().unwrap())
        .filter_map(|t| StatusPdu::decode(&t.payload).ok())
        .filter(|s| s.test_action == 0)
        .collect();
    // Each Status PDU carries the row chosen from the Trial Interval it
    // reports: row N is N Mbit/s at the IP layer.
    let carried = sent.iter().zip(&got).all(|(s, c)| {
        c.spdu_seq_no == s.spdu_seq_no
            && c.row
                .is_some_and(|row| row == 0 || ip_layer_bps(s) == u64::from(row) * MBPS)
    });
    let later = rows_after(&outcome, 60);
    outcome.check(
        carried
            && sent.len() == got.len()
            && later.len() > 40
            && later.iter().all(|row| (40..=55).contains(row)),
        &format!("rows after 3 s: {later:?}"),
    );
}

// Each Test Connection of a Multi-Connection Test runs its own algorithm:
// a bottleneck on one Test Connection's path lowers only its rows, while
// the other's keep climbing in Fast Mode as a lone Test Connection's would.
#[udpstp_req::req("RFC9097-8.3-L1", "RFC9946-4-L2")]
#[test]
fn each_test_connection_runs_its_own_algorithm() {
    // A coordinated Client's Test Connections send from its address and
    // the ports after it: mcIndex 0 from 40000, mcIndex 1 from 40001.
    let ip = harness::CLIENT_ADDR
        .parse::<std::net::SocketAddr>()
        .unwrap()
        .ip();
    let client = |port: u16| std::net::SocketAddr::new(ip, port);
    let outcome = Scenario::new(40)
        .mode1(3, b"a multi-connection key")
        .client_direction(Direction::Downstream)
        .path_delay(Duration::from_millis(5), Duration::ZERO)
        // 20 Mbit/s on the path to the second Test Connection only.
        .bottleneck(20_000_000, 25_000)
        .bottleneck_only_to(client(40001))
        .coordinated(2, 2)
        .run_for(Duration::from_millis(2000));

    let rows = |port: u16| -> Vec<(IntervalClass, u16)> {
        let socket = outcome
            .server_connects
            .iter()
            .find(|(_, peer)| *peer == client(port))
            .map(|(socket, _)| *socket);
        outcome
            .server_data_events
            .iter()
            .filter(|(s, _)| Some(*s) == socket)
            .filter_map(|(_, e)| match e {
                DataEvent::TrialIntervalClassified(c) => Some((c.class, c.row?)),
                _ => None,
            })
            .collect()
    };
    let (free, impaired) = (rows(40000), rows(40001));
    outcome.check(
        free.len() > 20 && impaired.len() > 20,
        &format!(
            "both Test Connections ran: {} and {}",
            free.len(),
            impaired.len()
        ),
    );
    let highest = |r: &[(IntervalClass, u16)]| r.iter().map(|&(_, row)| row).max().unwrap_or(0);
    let lowered = |r: &[(IntervalClass, u16)]| r.windows(2).any(|w| w[1].1 < w[0].1);
    // The free one sees no Errored Interval: in Fast Mode each Clean
    // Interval adds highSpeedDelta (10) rows and a Hold Interval none,
    // whatever the other Test Connection meets.
    let fast_mode = free.windows(2).all(|w| match w[1].0 {
        IntervalClass::Clean => w[1].1 == w[0].1 + 10,
        IntervalClass::Hold => w[1].1 == w[0].1,
        IntervalClass::Errored => false,
    });
    outcome.check(
        fast_mode && highest(&free) >= 200,
        &format!("the free Test Connection climbs: {free:?}"),
    );
    // The impaired one meets Errored Intervals once it sends above 20
    // Mbit/s (row 20), is lowered, and ends up searching around row 20.
    let last = &impaired[impaired.len().saturating_sub(10)..];
    outcome.check(
        impaired
            .iter()
            .any(|&(class, _)| class == IntervalClass::Errored)
            && lowered(&impaired)
            && highest(&impaired) <= 40
            && last.iter().all(|&(_, row)| (15..=25).contains(&row)),
        &format!("the impaired Test Connection is lowered: {impaired:?}"),
    );
}

// ,  (conformance: shallow-queue-search-alternates-at-the-bottleneck):
// through a queue too shallow for delay variation to reach upperThresh,
// the search confirms Congestion at its first overshoot, Fast Mode's jump
// past the bottleneck's rate bringing slowAdjThresh Errored Intervals back
// to back, and takes 3 x highSpeedDelta rows off once; in Single-Step Mode
// from then on it settles into a cycle a few rows around the bottleneck's
// rate, each Errored Interval taking one row off and each Clean one adding
// one back (the feedback lags by a Trial Interval or two), so every
// Sub-Interval loses Load PDUs; OB-UDPST does the same through the same
// queue
#[test]
fn a_search_through_a_shallow_queue_alternates_around_the_bottleneck() {
    // 50 Mbit/s with 20 ms of queue plus tbf's 32 kB burst, as the netns
    // tier's shallow case maps to the simulator.
    let outcome = Scenario::new(21)
        .server_allows_unauthenticated()
        .client_unauthenticated()
        .client_direction(Direction::Downstream)
        .bottleneck(50 * MBPS, 125_000 + 32_768)
        .path_delay(Duration::from_millis(5), Duration::ZERO)
        .run_for(Duration::from_millis(11_500));
    // Congestion was confirmed at the first overshoot: Fast Mode climbed
    // highSpeedDelta (10) rows a Clean Interval past row 50, the Errored
    // Intervals that followed took a row off each, and the third back to
    // back took 3 x 10 off, the only drop of more than a row. Every change
    // after it is a single step.
    let all = rows_after(&outcome, 0);
    let changes: Vec<i32> = all
        .windows(2)
        .map(|w| i32::from(w[1]) - i32::from(w[0]))
        .filter(|&d| d != 0)
        .collect();
    let confirming = changes.iter().position(|&d| d < -1);
    let overshoot = confirming.is_some_and(|at| {
        changes[at] == -30
            && changes[..at].iter().all(|&d| d == 10 || d == -1)
            && changes[at + 1..].iter().all(|&d| d.abs() == 1)
    });
    outcome.check(
        overshoot && all.iter().any(|&row| row > 50),
        &format!("the changes of row {changes:?} in {all:?}"),
    );
    let later = rows_after(&outcome, 60);
    outcome.check(
        later.len() > 100 && later.iter().all(|row| (48..=52).contains(row)),
        &format!("rows after 3 s: {later:?}"),
    );
    // The row keeps moving rather than settling on a lossless one.
    let changes = later.windows(2).filter(|w| w[0] != w[1]).count();
    outcome.check(
        changes > later.len() / 4,
        &format!("{changes} changes in {later:?}"),
    );
    // Every Sub-Interval after the first loses more than 1 %.
    let lossy: Vec<(u32, u32)> = outcome
        .client_data_events
        .iter()
        .filter_map(|(_, e)| match e {
            DataEvent::SubIntervalMeasured(m) => Some((m.loss, m.datagrams)),
            _ => None,
        })
        .skip(1)
        .collect();
    outcome.check(
        lossy.len() >= 9
            && lossy
                .iter()
                .all(|&(loss, got)| loss * 100 > loss.max(1) + got),
        &format!("Sub-Intervals' (loss, received): {lossy:?}"),
    );
}
