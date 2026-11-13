//! One-way delay variation, clockDeltaMin and RTT (RFC 9946 §8.1, §8.2).
//!
//! The hand-computed vectors come from a scripted Server that stamps each
//! Load PDU's lpduTime a chosen span before the Client receives it (the
//! simulator's path has no delay unless a test gives one). Expected values
//! follow the RFC's definitions and OB-UDPST's rounding: a span in ms is
//! rounded half up, with floor division for negative spans. Status PDUs are
//! read by their Figure 10 offsets.

use super::harness;

use std::time::Duration;

use crate::protocol::Direction;
use harness::{Outcome, RowRequest, Scenario, TraceEntry};

const NODEL: u32 = u32::MAX;
const MS: i64 = 1_000_000;

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn client() -> std::net::SocketAddr {
    harness::CLIENT_ADDR.parse().unwrap()
}

/// A Status PDU's delay fields (Figure 10).
#[derive(Debug, PartialEq, Eq)]
struct Delay {
    clock_delta_min: u32,
    var: [u32; 4],
    rtt_minimum: u32,
    rtt_var_sample: u32,
    delay_min_upd: u8,
}

fn delay(b: &[u8]) -> Delay {
    Delay {
        clock_delta_min: u32_at(b, 108),
        var: [
            u32_at(b, 112),
            u32_at(b, 116),
            u32_at(b, 120),
            u32_at(b, 124),
        ],
        rtt_minimum: u32_at(b, 128),
        rtt_var_sample: u32_at(b, 132),
        delay_min_upd: b[136],
    }
}

fn statuses(outcome: &Outcome) -> Vec<&TraceEntry> {
    outcome
        .trace
        .iter()
        .filter(|t| t.source == client() && t.len == 204 && t.payload.starts_with(&[0xFE, 0xED]))
        .collect()
}

/// A Load PDU that left `span_ns` before the scripted Server's `now_ns`,
/// which is when the Client receives it.
fn load(seq_no: u32, now_ns: u128, span_ns: i64) -> Vec<u8> {
    let sent = (now_ns as i128 - i128::from(span_ns)) as u128;
    let mut b = vec![0u8; 32];
    b[0..2].copy_from_slice(&[0xBE, 0xEF]);
    b[4..8].copy_from_slice(&seq_no.to_be_bytes());
    b[8..10].copy_from_slice(&32u16.to_be_bytes());
    b[20..24].copy_from_slice(&((sent / 1_000_000_000) as u32).to_be_bytes());
    b[24..28].copy_from_slice(&((sent % 1_000_000_000) as u32).to_be_bytes());
    b
}

/// A scripted Server that sends Load PDUs with these spans: `batches[0]`
/// with the Activation Response, `batches[k]` after the kth Status PDU.
fn server_sending(seed: u64, batches: Vec<Vec<i64>>) -> Outcome {
    server_sending_to(Scenario::new(seed), batches)
}

/// [`server_sending`] in `scenario`.
fn server_sending_to(scenario: Scenario, batches: Vec<Vec<i64>>) -> Outcome {
    let mut statuses = 0;
    let mut seq = 0;
    scenario
        .client_unauthenticated()
        .client_direction(Direction::Downstream)
        .scripted_server_replying_many(move |clock, request| {
            let mut batch = |k: usize| -> Vec<Vec<u8>> {
                batches
                    .get(k)
                    .into_iter()
                    .flatten()
                    .map(|&span| {
                        seq += 1;
                        load(seq, clock.unix_nanos, span)
                    })
                    .collect()
            };
            if request.len() == 56 {
                let mut pdu = request.to_vec();
                pdu[8] = 2;
                pdu[9] = 1;
                pdu[12..14].copy_from_slice(&24601u16.to_be_bytes());
                return vec![pdu];
            }
            if request.len() == 104 && request.starts_with(&[0xAC, 0xE2]) {
                let mut ack = request.to_vec();
                ack[5] = 1;
                return std::iter::once(ack).chain(batch(0)).collect();
            }
            if request.len() == 204 {
                statuses += 1;
                return batch(statuses);
            }
            Vec::new()
        })
        .run_for(Duration::from_millis(170))
}

// docs/spec.md  (OB-exact rounding: half up, floor division, per PDU before
// the minimum and the subtraction)
#[udpstp_req::req(
    "RFC9946-8.2-9",
    "RFC9946-8.2-L10",
    "RFC9946-8.2-L12",
    "RFC9946-8.2-L13",
    "RFC9946-8.2-L18"
)]
#[test]
fn delay_variation_is_rounded_per_load_pdu() {
    let outcome = server_sending(
        1,
        vec![
            // 10.5 ms rounds to 11 and only seeds clockDeltaMin; 10.4999 ms
            // rounds to 10, a new minimum (sample 0); 12 ms gives 2.
            vec![10 * MS + MS / 2, 10 * MS + MS / 2 - 100, 12 * MS],
            // -0.5 ms rounds up to 0 (sample 0 - 10 < 0: a new minimum,
            // sample 0); -1.5 ms rounds to -1, a new minimum (sample 0);
            // 1 ms gives 2.
            vec![-MS / 2, -MS - MS / 2, MS],
            // -0.5000001 ms rounds to -1: sample 0, no new minimum.
            vec![-MS / 2 - 100],
        ],
    );

    let statuses = statuses(&outcome);
    let got: Vec<Delay> = statuses.iter().take(3).map(|t| delay(&t.payload)).collect();
    let no_rtt = |clock_delta_min: u32, var: [u32; 4], delay_min_upd: u8| Delay {
        clock_delta_min,
        var,
        rtt_minimum: NODEL,
        rtt_var_sample: NODEL,
        delay_min_upd,
    };
    let expected = vec![
        no_rtt(10, [0, 2, 2, 2], 1),
        // clockDeltaMin -1 is 0xFFFFFFFF in two's complement, not "no value".
        no_rtt(-1i32 as u32, [0, 2, 2, 3], 1),
        no_rtt(-1i32 as u32, [0, 0, 0, 1], 0),
    ];
    outcome.check(got == expected, &format!("{got:#?}"));
}

// docs/spec.md  (clockDeltaMin saturates outside 32 bits)
#[test]
fn clock_delta_min_saturates() {
    // About 25 days either way: beyond i32 ms.
    let far = 3_000_000_000 * MS;
    for (seed, span, expected) in [(2, -far, i32::MIN), (22, far, i32::MAX)] {
        let outcome = server_sending(seed, vec![vec![span]]);
        let got = u32_at(&statuses(&outcome)[0].payload, 108);
        outcome.check(got == expected as u32, &format!("{expected} ms, not {got}"));
    }
}

// RFC 9946 §8.2 (the Trial Interval's delayVarMin is "no value" when no
// sample came, as OB-UDPST sends it)
#[test]
fn a_trial_interval_without_samples_reports_no_value() {
    let outcome = server_sending(3, vec![vec![5 * MS, 6 * MS], vec![]]);

    let statuses = statuses(&outcome);
    let second = delay(&statuses[1].payload);
    outcome.check(
        second.var == [NODEL, 0, 0, 0],
        &format!("no samples in the second Trial Interval: {second:?}"),
    );
}

fn operator_downstream(seed: u64) -> Scenario {
    Scenario::new(seed)
        .operator(7, b"an operator key")
        .client_direction(Direction::Downstream)
        .client_requests_row(RowRequest::FixedRate(5))
}

fn sub_interval_delay(outcome: &Outcome) -> Vec<[u32; 4]> {
    statuses(outcome)
        .iter()
        .map(|t| {
            let b = &t.payload;
            [u32_at(b, 68), u32_at(b, 72), u32_at(b, 76), u32_at(b, 80)]
        })
        .collect()
}

// docs/spec.md  (a wall-clock step or slew during a Test does not
// change measured delay variation); docs/spec.md Time model
#[test]
fn wall_clock_steps_and_slew_do_not_change_delay_variation() {
    let run = |scenario: Scenario| {
        let outcome = scenario.run_for(Duration::from_millis(2500));
        let trial: Vec<Delay> = statuses(&outcome)
            .iter()
            .map(|t| delay(&t.payload))
            .collect();
        (trial, sub_interval_delay(&outcome))
    };
    let base =
        || operator_downstream(4).path_delay(Duration::from_millis(20), Duration::from_millis(4));
    let baseline = run(base());

    for (what, changed) in [
        (
            "a Client wall step",
            base().client_wall_step(Duration::from_millis(900), 7),
        ),
        (
            "a Server wall step",
            base().server_wall_step(Duration::from_millis(1300), -3),
        ),
        (
            "a Client wall slew",
            base().client_wall_slew(Duration::from_millis(500), 500),
        ),
    ] {
        let got = run(changed);
        assert!(got == baseline, "{what} changed the delay measurements");
    }
    assert!(
        baseline.0.iter().any(|d| d.var[1] > 0),
        "the jitter shows as delay variation"
    );
}

// docs/spec.md  (the simulator's clock offset and drift between hosts)
#[udpstp_req::req("RFC9946-8.2-L12", "RFC9946-8.2-L14")]
#[test]
fn clock_offset_and_drift_show_in_clock_delta_min_only() {
    let outcome = operator_downstream(5)
        .path_delay(Duration::from_millis(20), Duration::ZERO)
        .client_clock_offset(-3)
        .run_for(Duration::from_millis(500));
    let first = delay(&statuses(&outcome)[0].payload);
    outcome.check(
        first.clock_delta_min as i32 == 20 - 3000 && first.var[1] == 0,
        &format!("clockDeltaMin is the delay plus the offset, and nothing varies: {first:?}"),
    );

    // 1000 ppm fast for 2 s is a 2 ms drift: delay variation grows by it.
    let outcome = operator_downstream(6)
        .path_delay(Duration::from_millis(20), Duration::ZERO)
        .client_clock_drift_ppm(1000)
        .run_for(Duration::from_millis(2100));
    let last = statuses(&outcome).last().map(|t| delay(&t.payload).var[1]);
    outcome.check(
        last.is_some_and(|max| (1..=3).contains(&max)),
        &format!("drift shows as a slowly growing delay variation: {last:?}"),
    );
}

#[udpstp_req::req(
    "RFC9946-8.1-L9",
    "RFC9946-8.1-L12",
    "RFC9946-8.2-7",
    "RFC9946-8.2-L15",
    "RFC9946-8.2-L16",
    "RFC9946-8.2-L17"
)]
#[test]
fn rtt_is_measured_from_echoed_status_pdus() {
    let outcome = operator_downstream(7)
        .path_delay(Duration::from_millis(15), Duration::ZERO)
        .run_for(Duration::from_millis(600));

    // Each Load PDU echoes the latest Status PDU's spduTime (Figure 9,
    // bytes 12-19) and rttRespDelay (bytes 28-29).
    let server_loads: Vec<&TraceEntry> = outcome
        .trace
        .iter()
        .filter(|t| t.destination == client() && t.payload.starts_with(&[0xBE, 0xEF]))
        .collect();
    let echoing = server_loads
        .iter()
        .filter(|t| u32_at(&t.payload, 12) != 0)
        .count();
    outcome.check(echoing > 10, "Load PDUs echo a spduTime");

    // With a 15 ms path each way and rttRespDelay taken off, the adjusted
    // RTT is 30 ms, give or take the rounding of the two ms values.
    let later: Vec<Delay> = statuses(&outcome)
        .iter()
        .skip(2)
        .map(|t| delay(&t.payload))
        .collect();
    outcome.check(
        !later.is_empty()
            && later
                .iter()
                .all(|d| (29..=31).contains(&d.rtt_minimum) && d.rtt_var_sample <= 2),
        &format!("rttMinimum about 30 ms: {later:#?}"),
    );
}

// OB-UDPST's special case: an rttRespDelay one more than the raw RTT gives
// 0, a larger one leaves the raw RTT
#[udpstp_req::req("RFC9946-8.1-L12", "RFC9946-8.2-L15", "RFC9946-8.2-L18")]
#[test]
fn rtt_resp_delay_adjusts_the_rtt() {
    // The raw RTT of a Load PDU sent the moment a Status PDU arrives is
    // twice the path, rounded half up: 4 ms for 2 ms, 4.5 ms (5) for 2.25
    // ms, 4.4 ms (4) for 2.2 ms.
    for (seed, path_us, resp_delay_ms, expected) in [
        (8, 2000, 3u16, 1u32),
        (9, 2000, 4, 0),
        (10, 2000, 5, 0),
        (11, 2000, 9, 4),
        (15, 2250, 0, 5),
        (16, 2250, 1, 4),
        (17, 2200, 0, 4),
    ] {
        let mut seq = 1;
        let outcome = Scenario::new(seed)
            .path_delay(Duration::from_micros(path_us), Duration::ZERO)
            .client_unauthenticated()
            .client_direction(Direction::Downstream)
            .scripted_server_replying_many(move |clock, request| {
                if request.len() == 56 {
                    let mut pdu = request.to_vec();
                    pdu[8] = 2;
                    pdu[9] = 1;
                    pdu[12..14].copy_from_slice(&24601u16.to_be_bytes());
                    return vec![pdu];
                }
                if request.len() == 104 && request.starts_with(&[0xAC, 0xE2]) {
                    let mut ack = request.to_vec();
                    ack[5] = 1;
                    return vec![ack, load(1, clock.unix_nanos, 0)];
                }
                if request.len() == 204 {
                    // Echo this Status PDU's spduTime (bytes 152-159) at
                    // once, with the given rttRespDelay.
                    seq += 1;
                    let mut l = load(seq, clock.unix_nanos, 0);
                    l[12..20].copy_from_slice(&request[152..160]);
                    l[28..30].copy_from_slice(&resp_delay_ms.to_be_bytes());
                    return vec![l];
                }
                Vec::new()
            })
            .run_for(Duration::from_millis(140));

        let statuses = statuses(&outcome);
        let second = statuses.get(1).map(|t| delay(&t.payload));
        outcome.check(
            second.as_ref().is_some_and(|d| {
                d.rtt_minimum == expected && d.rtt_var_sample == 0 && d.delay_min_upd == 1
            }),
            &format!("rttRespDelay {resp_delay_ms} ms: RTT {expected} ms, not {second:?}"),
        );
    }
}

// docs/spec.md  (out-of-order and duplicate Load PDUs give no delay
// samples, as in OB-UDPST)
#[test]
fn late_and_duplicate_load_pdus_give_no_delay_sample() {
    let mut statuses_seen = 0;
    let outcome = Scenario::new(12)
        .client_unauthenticated()
        .client_direction(Direction::Downstream)
        .scripted_server_replying_many(move |clock, request| {
            if request.len() == 56 {
                let mut pdu = request.to_vec();
                pdu[8] = 2;
                pdu[9] = 1;
                pdu[12..14].copy_from_slice(&24601u16.to_be_bytes());
                return vec![pdu];
            }
            if request.len() == 104 && request.starts_with(&[0xAC, 0xE2]) {
                let mut ack = request.to_vec();
                ack[5] = 1;
                let now = clock.unix_nanos;
                return vec![ack, load(1, now, 5 * MS), load(3, now, 5 * MS)];
            }
            if request.len() == 204 {
                statuses_seen += 1;
                if statuses_seen == 1 {
                    // Load PDU 2 arrives late, and a duplicate of 3, both
                    // 50 ms after they left.
                    let now = clock.unix_nanos;
                    return vec![load(2, now, 50 * MS), load(3, now, 50 * MS)];
                }
            }
            Vec::new()
        })
        .run_for(Duration::from_millis(120));

    let statuses = statuses(&outcome);
    let second = delay(&statuses[1].payload);
    outcome.check(
        second.var == [NODEL, 0, 0, 0],
        &format!("no sample from the late or duplicate Load PDU: {second:?}"),
    );
}

#[udpstp_req::req("RFC9946-8.2-7", "RFC9946-8.2-10", "RFC9946-8.2-L17")]
#[test]
fn rtt_var_sample_has_no_value_without_a_new_sample() {
    let mut seq = 1;
    let outcome = Scenario::new(13)
        .path_delay(Duration::from_millis(2), Duration::ZERO)
        .client_unauthenticated()
        .client_direction(Direction::Downstream)
        .scripted_server_replying_many(move |clock, request| {
            if request.len() == 56 {
                let mut pdu = request.to_vec();
                pdu[8] = 2;
                pdu[9] = 1;
                pdu[12..14].copy_from_slice(&24601u16.to_be_bytes());
                return vec![pdu];
            }
            if request.len() == 104 && request.starts_with(&[0xAC, 0xE2]) {
                let mut ack = request.to_vec();
                ack[5] = 1;
                return vec![ack, load(1, clock.unix_nanos, 0)];
            }
            if request.len() == 204 {
                // Echo the first Status PDU's spduTime, then none (0).
                seq += 1;
                let mut l = load(seq, clock.unix_nanos, 0);
                if seq == 2 {
                    l[12..20].copy_from_slice(&request[152..160]);
                }
                return vec![l];
            }
            Vec::new()
        })
        .run_for(Duration::from_millis(170));

    let statuses = statuses(&outcome);
    let (second, third) = (delay(&statuses[1].payload), delay(&statuses[2].payload));
    outcome.check(
        second.rtt_var_sample == 0 && third.rtt_var_sample == NODEL && third.rtt_minimum == 4,
        &format!("a sample, then none; the minimum stays: {second:?} {third:?}"),
    );
}

// delayVar and rttVar statistics for the Sub-Interval too.
#[udpstp_req::req("RFC9946-8.2-6", "RFC9097-6.4-1")]
#[test]
fn sub_intervals_collect_the_delay_samples_of_their_trial_intervals() {
    let outcome = operator_downstream(14)
        .path_delay(Duration::from_millis(20), Duration::from_millis(6))
        .run_for(Duration::from_millis(1300));

    let statuses = statuses(&outcome);
    let trials: Vec<Delay> = statuses.iter().map(|t| delay(&t.payload)).collect();
    // The first Sub-Interval opens after Status PDU 0 and closes after
    // Status PDU 20; Status PDU 21 reports it.
    let within = &trials[1..=20];
    let expected = [
        within.iter().map(|d| d.var[0]).min().unwrap_or(NODEL),
        within.iter().map(|d| d.var[1]).max().unwrap_or(0),
        within.iter().map(|d| d.var[2]).sum(),
        within.iter().map(|d| d.var[3]).sum(),
    ];
    let reported = sub_interval_delay(&outcome)[21];
    outcome.check(
        reported == expected && expected[1] > 0,
        &format!("sisSav delay {reported:?}, trials {expected:?}"),
    );
    let b = &statuses[21].payload;
    let rtt_samples: Vec<u32> = within
        .iter()
        .map(|d| d.rtt_var_sample)
        .filter(|&s| s != NODEL)
        .collect();
    outcome.check(
        !rtt_samples.is_empty()
            && u32_at(b, 84) <= *rtt_samples.iter().min().unwrap_or(&0)
            && u32_at(b, 88) >= *rtt_samples.iter().max().unwrap_or(&0),
        &format!("sisSav rttVar range covers the samples {rtt_samples:?}"),
    );
}

// docs/spec.md Time model (a kernel receive timestamp is converted with
// the clock pair around the read; a datagram that waited in the socket
// buffer keeps its earlier time)
#[test]
fn kernel_receive_timestamps_give_the_receive_time() {
    // Each Load PDU left 10 ms before the read; the kernel timestamp says
    // when it arrived.
    for (seed, shift_ns, expected) in [
        // Waited 3 ms in the socket buffer: 7 ms.
        (30, -3 * MS, 7),
        // A timestamp after the read is a clock error: the read time.
        (31, 3 * MS, 10),
        // Over the receive timeout before the read: kept at that timeout.
        (32, -2_000 * MS, 10 - 1000),
    ] {
        let outcome = server_sending_to(
            Scenario::new(seed).client_kernel_timestamp_shift(shift_ns),
            vec![vec![10 * MS]],
        );
        let got = u32_at(&statuses(&outcome)[0].payload, 108) as i32;
        outcome.check(
            got == expected,
            &format!("shift {shift_ns} ns: clockDeltaMin {expected} ms, not {got}"),
        );
    }
}

// docs/spec.md Time model (a read whose clock pair disagrees gives no
// delay samples)
#[test]
fn a_wall_clock_step_during_the_read_gives_no_samples() {
    let outcome = server_sending_to(
        Scenario::new(33).client_wall_steps_during_reads(),
        vec![vec![10 * MS, 12 * MS]],
    );
    let first = delay(&statuses(&outcome)[0].payload);
    let expected = Delay {
        clock_delta_min: 0,
        var: [NODEL, 0, 0, 0],
        rtt_minimum: NODEL,
        rtt_var_sample: NODEL,
        delay_min_upd: 0,
    };
    outcome.check(first == expected, &format!("{first:?}"));
    // : and says how many Load PDUs had their sample dropped.
    let dropped: u32 = outcome
        .client_data_events
        .iter()
        .filter_map(|(_, e)| match e {
            crate::protocol::DataEvent::DelaySamplesDropped { count } => Some(*count),
            _ => None,
        })
        .sum();
    outcome.check(dropped >= 2, &format!("{dropped} counted"));
}

// docs/spec.md  (rounded half up, as OB-UDPST)
#[udpstp_req::req("RFC9946-8.1-L9", "RFC9946-8.1-L11")]
#[test]
fn rtt_resp_delay_is_the_time_since_the_status_pdu_arrived() {
    check_rtt_resp_delay(Scenario::new(34), 0);
}

// : rttRespDelay counts from the Status PDU's kernel receive time, not
// from the read: here each Status PDU waited 1 ms in the socket buffer, which
// the RTT then leaves out.
#[udpstp_req::req("RFC9946-8.1-L11")]
#[test]
fn rtt_resp_delay_counts_from_the_kernel_receive_time() {
    check_rtt_resp_delay(
        Scenario::new(36).client_kernel_timestamp_shift(-1_000_000),
        1_000_000,
    );
}

/// Checks each Upstream Load PDU's rttRespDelay: the time since the
/// latest Status PDU arrived, which the Client's kernel timestamped
/// `waited_ns` before it read it, rounded half up.
fn check_rtt_resp_delay(scenario: Scenario, waited_ns: u128) {
    // The Server sends each Status PDU as a Load PDU arrives, so with a
    // 0.2 ms path every Load PDU leaves x.6 ms after one arrives: rounded up.
    let path = Duration::from_micros(200);
    let outcome = scenario
        .operator(7, b"an operator key")
        .client_direction(Direction::Upstream)
        .client_requests_row(RowRequest::FixedRate(5))
        .path_delay(path, Duration::ZERO)
        .run_for(Duration::from_millis(400));

    // Arrivals at the Client, and the Client's Load PDUs by send time.
    let statuses: Vec<&TraceEntry> = outcome
        .trace
        .iter()
        .filter(|t| {
            t.destination == client() && t.len == 204 && t.payload.starts_with(&[0xFE, 0xED])
        })
        .collect();
    let loads = outcome
        .trace
        .iter()
        .filter(|t| t.source == client() && t.payload.starts_with(&[0xBE, 0xEF]));
    let (mut checked, mut rounded_up) = (0, 0);
    for load in loads {
        let sent = load.at - path;
        let echoed = &load.payload[12..20];
        let resp_delay = u16::from_be_bytes([load.payload[28], load.payload[29]]);
        let Some(status) = statuses.iter().rev().find(|s| s.at < sent) else {
            outcome.check(echoed == [0; 8] && resp_delay == 0, "nothing to echo yet");
            continue;
        };
        if statuses.iter().any(|s| s.at == sent) {
            continue;
        }
        let since = (sent - status.at).as_nanos() + waited_ns;
        let expected = (since + 500_000) / 1_000_000;
        outcome.check(
            echoed == &status.payload[152..160] && u128::from(resp_delay) == expected,
            &format!("{since} ns since the Status PDU: rttRespDelay {expected}, not {resp_delay}"),
        );
        checked += 1;
        rounded_up += usize::from(since % 1_000_000 >= 500_000);
    }
    outcome.check(
        checked > 50 && rounded_up > 0,
        &format!("{checked} Load PDUs checked, {rounded_up} rounded up"),
    );
}

// docs/spec.md  (an echoed spduTime from the future gives no RTT
// sample; OB-UDPST casts the negative RTT to unsigned, a huge sample)
#[test]
fn an_rtt_from_a_future_spdu_time_gives_no_sample() {
    let mut seq = 1;
    let outcome = Scenario::new(35)
        .path_delay(Duration::from_millis(2), Duration::ZERO)
        .client_unauthenticated()
        .client_direction(Direction::Downstream)
        .scripted_server_replying_many(move |clock, request| {
            if request.len() == 56 {
                let mut pdu = request.to_vec();
                pdu[8] = 2;
                pdu[9] = 1;
                pdu[12..14].copy_from_slice(&24601u16.to_be_bytes());
                return vec![pdu];
            }
            if request.len() == 104 && request.starts_with(&[0xAC, 0xE2]) {
                let mut ack = request.to_vec();
                ack[5] = 1;
                return vec![ack, load(1, clock.unix_nanos, 0)];
            }
            if request.len() == 204 {
                // Echo a spduTime a second after this Status PDU's.
                seq += 1;
                let mut l = load(seq, clock.unix_nanos, 0);
                let secs = u32_at(request, 152) + 1;
                l[12..16].copy_from_slice(&secs.to_be_bytes());
                l[16..20].copy_from_slice(&request[156..160]);
                return vec![l];
            }
            Vec::new()
        })
        .run_for(Duration::from_millis(140));

    let second = delay(&statuses(&outcome)[1].payload);
    outcome.check(
        second.rtt_minimum == NODEL && second.rtt_var_sample == NODEL,
        &format!("no RTT sample: {second:?}"),
    );
}
