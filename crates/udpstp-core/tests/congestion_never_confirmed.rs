//! A Search Test whose search never confirmed Congestion, in
//! simulated Tests: the cases of the adversarial reviews the maintainer's
//! decisions on  (2026-09-29) followed. Below the 1 Gbps row a search
//! that rises past the path's limit confirms Congestion at its first
//! overshoot, slowAdjThresh Errored Intervals back to back, whatever the
//! queue or the algorithm. Above it the search steps a row at a time, and
//! through a shallow queue its Errored Intervals can come one or two at a
//! time, too few back to back to confirm Congestion: it met the limit when
//! its last Trial Intervals were often errored. A
//! search that did either gets no warning. One that did neither fell short
//! of the path, and the Client names why, the first that fits: the
//! maxBandwidth it stated, the top of the Sending Rate Table, delay
//! variation holding it in Hold Intervals, a Test that ended while it
//! still rose, or a Load Sender that stopped rising though few of its last
//! Trial Intervals were errored.
//!
//! What the Client judges comes from its own Data Phase events, the count
//! of its Trial Intervals and the IP-Layer Sender Bit Rate
//! (`SearchTest::shortfalls`, whose rules `src/search.rs` tests on
//! hand-built events); the expectations come from the scenario: where the
//! bottleneck is, and what holds the Server back.

mod harness;

use std::time::Duration;

use harness::{Outcome, Scenario};
use udpstp_core::{
    DataEvent, DelayVarStatistic, Direction, Event, IpVersion, RateAdjAlgorithm, SearchShortfall,
    SearchTest, SocketId, TableShape, TrialIntervalCounts,
};

const MBPS: u64 = 1_000_000;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// A Search Test of 5 s from the first Client, in `direction`, 5 ms each
/// way.
fn search(seed: u64, direction: Direction) -> Scenario {
    Scenario::new(seed)
        .server_allows_unauthenticated()
        .client_unauthenticated()
        .client_direction(direction)
        .path_delay(ms(5), Duration::ZERO)
        .client_config(|c| c.test_int_time = 5)
}

/// Why the first Client's searches fell short of the path, as it judges
/// them from its Test Connections' Data Phase events, in mcIndex order:
/// none when one confirmed Congestion. The Test stated `max_bandwidth` over
/// `test_connections`, as the scenario set them.
fn shortfalls(outcome: &Outcome, max_bandwidth: u16, test_connections: u8) -> Vec<SearchShortfall> {
    let parameters = outcome
        .client_events
        .iter()
        .find_map(|e| match e {
            Event::TestActivated { parameters, .. } => Some(*parameters),
            _ => None,
        })
        .expect("the Test was activated");
    let mut sockets: Vec<SocketId> = outcome.client_data_events.iter().map(|(s, _)| *s).collect();
    sockets.sort_by_key(|s| s.0);
    sockets.dedup();
    let each: Vec<Vec<DataEvent>> = sockets
        .iter()
        .map(|socket| {
            outcome
                .client_data_events
                .iter()
                .filter(|(s, _)| s == socket)
                .map(|(_, e)| *e)
                .collect()
        })
        .collect();
    // Each Data Phase ended, so each counted its Trial Intervals.
    let ended = each.len() == usize::from(test_connections)
        && each
            .iter()
            .all(|events| matches!(events.last(), Some(DataEvent::Ended(_))));
    outcome.check(ended, "every Test Connection's Data Phase ended");
    let each: Vec<&[DataEvent]> = each.iter().map(Vec::as_slice).collect();
    SearchTest {
        parameters,
        ip_version: IpVersion::V4,
        max_bandwidth,
        test_connections,
        jumbo: true,
        traditional_mtu: false,
    }
    .shortfalls(&each)
}

/// How the first Client counted its Test Connections' Trial Intervals.
fn counted(outcome: &Outcome) -> Vec<TrialIntervalCounts> {
    outcome
        .client_data_events
        .iter()
        .filter_map(|(_, e)| match e {
            DataEvent::TrialIntervalsCounted(counts) => Some(*counts),
            _ => None,
        })
        .collect()
}

/// What a case adds to its Test.
type Path = fn(Scenario) -> Scenario;

/// Runs a 5 s Test.
fn run(scenario: Scenario) -> Outcome {
    scenario.run_for(ms(6_500))
}

/// A Search Test of 10 s, which a search needs to pass the 1 Gbps row, 5 s
/// into it in Fast Mode.
fn long(seed: u64, direction: Direction) -> Scenario {
    search(seed, direction).client_config(|c| c.test_int_time = 10)
}

/// Runs a 10 s Test.
fn run_long(scenario: Scenario) -> Outcome {
    scenario.run_for(ms(11_500))
}

// : through a bottleneck the search rises past, Congestion is
// confirmed at the first overshoot and no warning is given: a short, a
// shallow (20 ms and tbf's 32 kB burst, around which it then alternates)
// and a deep queue (200 ms, whose delay variation errs first), a faster
// bottleneck, algorithm C, a Server that classifies by the average delay
// variation, an Upstream Test, and a Server that can't keep up behind a
// slower bottleneck, which the path still limits
// (conformance: shallow-queue-search-alternates-at-the-bottleneck)
#[test]
fn a_search_through_a_bottleneck_confirms_congestion() {
    let cases: [(&str, u64, Direction, Path); 8] = [
        ("50 Mbit/s, 10 ms", 11, Direction::Downstream, |s| {
            s.bottleneck(50 * MBPS, 62_500)
        }),
        ("50 Mbit/s, shallow", 21, Direction::Downstream, |s| {
            s.bottleneck(50 * MBPS, 125_000 + 32_768)
        }),
        ("50 Mbit/s, 200 ms", 31, Direction::Downstream, |s| {
            s.bottleneck(50 * MBPS, 1_250_000)
        }),
        ("300 Mbit/s, 20 ms", 41, Direction::Downstream, |s| {
            s.bottleneck(300 * MBPS, 750_000)
        }),
        ("algorithm C", 51, Direction::Downstream, |s| {
            s.client_config(|c| c.algorithm = RateAdjAlgorithm::C)
                .bottleneck(50 * MBPS, 62_500)
        }),
        ("the Server's average", 61, Direction::Downstream, |s| {
            s.server_delay_var(DelayVarStatistic::Average)
                .bottleneck(50 * MBPS, 62_500)
        }),
        ("Upstream", 12, Direction::Upstream, |s| {
            s.bottleneck(50 * MBPS, 62_500)
        }),
        (
            "a busy Server behind 300 Mbit/s",
            151,
            Direction::Downstream,
            |s| s.partial_sends(1, 3).bottleneck(300 * MBPS, 750_000),
        ),
    ];
    for (what, seed, direction, path) in cases {
        let outcome = run(path(search(seed, direction)));
        let got = shortfalls(&outcome, 0, 1);
        outcome.check(got.is_empty(), &format!("{what}: {got:?}"));
    }
}

// : a 5 s Test whose search reaches a 480 Mbit/s bottleneck in its
// third second confirms Congestion there: no warning
#[test]
fn a_short_test_that_reaches_the_bottleneck_confirms_congestion() {
    let outcome = run(search(121, Direction::Downstream).bottleneck(480 * MBPS, 600_000));
    let got = shortfalls(&outcome, 0, 1);
    outcome.check(got.is_empty(), &format!("{got:?}"));
}

/// A Server whose 9000-byte MTU takes its Sending Rate Table to 72 Gbps,
/// in Load PDUs of up to 9000 bytes above the 1 Gbps row: fewer to
/// simulate than a Server's default 1500 bytes send at the same rate.
fn jumbo_server(scenario: Scenario) -> Scenario {
    scenario.server_config(|c| {
        c.table = TableShape {
            jumbo: true,
            traditional_mtu: false,
            mtu: 9000,
        };
    })
}

/// Runs a 10 s Test whose search alternates around a bottleneck above the
/// 1 Gbps row: never confirmed, often errored at the end, and no warning.
fn alternates(what: &str, scenario: Scenario) {
    let outcome = run_long(scenario);
    let counts = counted(&outcome);
    outcome.check(
        matches!(counts[..], [one] if !one.congestion_confirmed && one.last_errored >= 4),
        &format!("{what}: never confirmed, often errored at the end: {counts:?}"),
    );
    let got = shortfalls(&outcome, 0, 1);
    outcome.check(got.is_empty(), &format!("{what}: {got:?}"));
}

// : above the 1 Gbps row the search steps a row at a time, so through
// a shallow queue its first overshoot is one row, and here its Errored
// Intervals come one or two at a time, never slowAdjThresh back to back: it
// never confirms Congestion, but about half its last Trial Intervals are
// errored, from the first of them on, so it met the path's limit and gets
// no warning (the maintainer's decision on , 2026-09-29; the review's
// case): 2.5 Gbit/s through 1 ms of queue. Behind a Server with a 9000-byte
// MTU, whose Load PDUs there are 3125 bytes, to keep within the core
// tests' time budget; at the default 1500 bytes it alternates the same way
// (conformance: shallow-queue-search-alternates-at-the-bottleneck)
#[test]
fn a_search_alternating_above_the_gigabit_row_met_the_limit() {
    alternates(
        "2.5 Gbit/s",
        jumbo_server(long(171, Direction::Downstream)).bottleneck(2_500 * MBPS, 312_500),
    );
}

// : the same at 11.8 Gbit/s through 0.5 ms of queue, behind a Server
// whose 9000-byte MTU takes its table to 72 Gbps: the search, peaking at
// 11.52 Gbps, spends its first 5 s below the 1 Gbps row and the next 3.4
// climbing, so fewer than 1 in 10 of all its Trial Intervals are errored,
// but about half of its last ones (the review's case)
// (conformance: shallow-queue-search-alternates-at-the-bottleneck)
#[test]
fn a_search_alternating_near_12_gbit_s_met_the_limit() {
    alternates(
        "11.8 Gbit/s",
        jumbo_server(long(172, Direction::Downstream)).bottleneck(11_800 * MBPS, 737_500),
    );
}

/// A Server sending only 20 datagrams of each Send Plan, which tops out
/// near 2.5 Gbit/s: a Load Sender that can't keep up above the 1 Gbps row,
/// as a udpstp Server's engine does at 2 to 5 Gbit/s.
fn topping_out(seed: u64) -> Scenario {
    long(seed, Direction::Downstream).partial_sends(1, 20)
}

// : a Load Sender that tops out above the 1 Gbps row, with no Errored
// Interval, is named (the review's case)
#[test]
fn a_load_sender_topping_out_above_the_gigabit_row_is_named() {
    let outcome = run_long(topping_out(181));
    let got = shortfalls(&outcome, 0, 1);
    outcome.check(got == [SearchShortfall::LoadSender], &format!("{got:?}"));
}

// : so is one with two 2 ms loss windows a second, each an Errored
// Interval or two, which leave its last Trial Intervals errored too rarely
// to have met the path's limit (the review's case)
#[test]
fn a_load_sender_topping_out_through_loss_windows_is_named() {
    let mut lossy = topping_out(182);
    for window in 0..21 {
        let from = ms(300 + 500 * window);
        lossy = lossy.network_drops([0xBE, 0xEF], from, Some(from + ms(2)));
    }
    let outcome = run_long(lossy);
    let counts = counted(&outcome);
    let got = shortfalls(&outcome, 0, 1);
    outcome.check(
        matches!(counts[..], [one] if one.last_errored > 0) && got == [SearchShortfall::LoadSender],
        &format!("{got:?} from {counts:?}"),
    );
}

// : a Server that can't keep up, sending only 3 datagrams of each Send
// Plan, on a path with no bottleneck: its Skipped and Withheld Bursts take
// no lpduSeqNo, so the search sees no loss and keeps raising the row while
// what the Server sends stops rising; so too with a 30 ms Load PDU loss
// burst, one Errored Interval, and a 150 ms outage, which a Hold Interval
// or two follow. It is named, and so is a Client that can't keep up in an
// Upstream Test
#[test]
fn a_load_sender_that_falls_behind_is_named() {
    let cases: [(&str, u64, Direction, Path); 4] = [
        ("a clean path", 71, Direction::Downstream, |s| s),
        ("a 30 ms loss burst", 81, Direction::Downstream, |s| {
            s.network_drops([0xBE, 0xEF], ms(3_000), Some(ms(3_030)))
        }),
        ("a 150 ms outage", 131, Direction::Downstream, |s| {
            s.network_drops([0xBE, 0xEF], ms(3_000), Some(ms(3_150)))
        }),
        ("Upstream", 72, Direction::Upstream, |s| s),
    ];
    for (what, seed, direction, path) in cases {
        let outcome = run(path(search(seed, direction).partial_sends(1, 3)));
        let got = shortfalls(&outcome, 0, 1);
        outcome.check(
            got == [SearchShortfall::LoadSender],
            &format!("{what}: {got:?}"),
        );
    }
}

// : delay variation between lowThresh (30 ms) and upperThresh (90 ms),
// from up to 40 ms or 100 ms of jitter on a path with no bottleneck, makes
// nearly every Trial Interval a Hold Interval, on which the algorithm
// changes nothing: the search stays low, and it is named
#[test]
fn jitter_that_holds_the_search_is_named() {
    for (seed, jitter) in [(91, 40), (101, 100)] {
        let outcome = run(search(seed, Direction::Downstream).path_delay(ms(5), ms(jitter)));
        let got = shortfalls(&outcome, 0, 1);
        outcome.check(
            got == [SearchShortfall::DelayVariation],
            &format!("{jitter} ms of jitter: {got:?}"),
        );
    }
}

// : a udpstp Server never searches above the row of the maxBandwidth a
// Test Connection stated, 200 Mbps here, so a search capped there on
// a faster path names it; in a Multi-Connection Test each Test Connection
// stated an equal share, each named
// (conformance: max-bandwidth-split-across-test-connections, search-capped-at-stated-max-bandwidth)
#[test]
fn a_max_bandwidth_that_caps_the_search_is_named() {
    let outcome = run(search(111, Direction::Downstream).client_max_bandwidth(200));
    let got = shortfalls(&outcome, 200, 1);
    outcome.check(
        got == [SearchShortfall::MaxBandwidth(200)],
        &format!("{got:?}"),
    );
    let outcome = run(search(112, Direction::Downstream)
        .client_max_bandwidth(400)
        .coordinated(2, 2));
    let got = shortfalls(&outcome, 400, 2);
    outcome.check(
        got == [SearchShortfall::MaxBandwidth(200); 2],
        &format!("two Test Connections: {got:?}"),
    );
    // Between two rows above 1 Gbps: at 1190 Mbps the search stops at row
    // 1001, 1100 Mbps, the highest at or below it, which takes a 10 s Test
    // to reach.
    let outcome = search(113, Direction::Downstream)
        .client_config(|c| c.test_int_time = 10)
        .client_max_bandwidth(1190)
        .run_for(ms(11_500));
    let got = shortfalls(&outcome, 1190, 1);
    outcome.check(
        got == [SearchShortfall::MaxBandwidth(1190)],
        &format!("1190 Mbps: {got:?}"),
    );
}

// : a 5 s Test on a path faster than Fast Mode reaches in it, the
// search still rising 200 Mbps a second at its end, names the Test's
// length
#[test]
fn a_test_that_ends_while_the_search_rises_is_named() {
    let outcome = run(search(141, Direction::Downstream));
    let got = shortfalls(&outcome, 0, 1);
    outcome.check(got == [SearchShortfall::StillRising], &format!("{got:?}"));
}

// : in a Multi-Connection Test the warning waits for no Test
// Connection to confirm Congestion: two through one 50 Mbit/s bottleneck
// both confirm it
#[test]
fn a_multi_connection_test_through_a_bottleneck_confirms_congestion() {
    let outcome = run(search(161, Direction::Downstream)
        .bottleneck(50 * MBPS, 62_500)
        .coordinated(2, 2));
    let got = shortfalls(&outcome, 0, 2);
    outcome.check(got.is_empty(), &format!("{got:?}"));
}
