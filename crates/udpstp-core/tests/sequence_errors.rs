//! Sequence errors in Status PDUs (RFC 9946 §8.2): loss, out-of-order and
//! duplicate totals from lpduSeqNo, with a lookback of 32.
//!
//! A scripted Server sends Load PDUs in batches: the first on activation,
//! then one after each Status PDU, so the Client's Status PDU k reports
//! batch k. Expected counts are worked out by hand from the RFC's rules
//! and the interpretations recorded in the Conformance Records.

mod harness;

use std::collections::HashSet;
use std::time::Duration;

use harness::{Outcome, RowRequest, Scenario, TraceEntry};
use udpstp_core::Direction;

/// seqErrLoss, seqErrOoo, seqErrDup.
type SeqErrors = (u32, u32, u32);

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// A header-only Load PDU (RFC 9946 Figure 9) with this lpduSeqNo.
fn load(seq_no: u32) -> Vec<u8> {
    let mut b = vec![0u8; 32];
    b[0..2].copy_from_slice(&[0xBE, 0xEF]);
    b[4..8].copy_from_slice(&seq_no.to_be_bytes());
    b[8..10].copy_from_slice(&32u16.to_be_bytes());
    b
}

/// A scripted Server that accepts the Setup and Activation Requests, sends
/// `batches[0]` with the Activation Response and `batches[k]` after the
/// Client's kth Status PDU.
fn server_sending(seed: u64, batches: Vec<Vec<u32>>) -> Scenario {
    let mut statuses = 0;
    Scenario::new(seed)
        .client_unauthenticated()
        .client_direction(Direction::Downstream)
        .scripted_server_replying_many(move |_, request| {
            let batch = |k: usize| -> Vec<Vec<u8>> {
                batches
                    .get(k)
                    .into_iter()
                    .flatten()
                    .map(|&s| load(s))
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
            if request.len() == 204 && request.starts_with(&[0xFE, 0xED]) {
                statuses += 1;
                return batch(statuses);
            }
            Vec::new()
        })
}

fn client() -> std::net::SocketAddr {
    harness::CLIENT_ADDR.parse().unwrap()
}

fn is_status(t: &TraceEntry) -> bool {
    t.source == client() && t.len == 204 && t.payload.starts_with(&[0xFE, 0xED])
}

/// The Client's Status PDUs, in order.
fn statuses(outcome: &Outcome) -> Vec<&[u8]> {
    outcome
        .trace
        .iter()
        .filter(|t| is_status(t))
        .map(|t| t.payload.as_slice())
        .collect()
}

/// The Trial Interval Sequence Errors of each Status PDU (Figure 10).
fn trial_errors(outcome: &Outcome) -> Vec<SeqErrors> {
    statuses(outcome)
        .into_iter()
        .map(|b| (u32_at(b, 96), u32_at(b, 100), u32_at(b, 104)))
        .collect()
}

/// subIntSeqNo and sisSav's Sequence Errors of each Status PDU.
fn sub_interval_errors(outcome: &Outcome) -> Vec<(u32, SeqErrors)> {
    statuses(outcome)
        .into_iter()
        .map(|b| (u32_at(b, 36), (u32_at(b, 56), u32_at(b, 60), u32_at(b, 64))))
        .collect()
}

/// Runs `batches` and checks the Trial Interval counts reported for each.
fn check_batches(seed: u64, batches: Vec<Vec<u32>>, expected: &[SeqErrors], what: &str) {
    let n = batches.len();
    let outcome = server_sending(seed, batches).run_for(Duration::from_millis(60 * n as u64));
    let reported = trial_errors(&outcome);
    outcome.check(
        reported.len() >= n && reported[..n] == *expected,
        &format!("{what}: expected {expected:?}, reported {reported:?}"),
    );
}

// RFC 9946 §8.2's example: after 93, 94, 95, 100, 96, 97, 101, 98, 99, 102,
// 103, the Load PDUs 96-99 are out of order and do not move the
// next-expected seqno; rfc-ambiguities.md §18 item 1 (each late Load PDU
// takes one off the loss the gap caused)
#[udpstp_req::req("RFC9946-8.2-L4", "RFC9946-8.2-L9")]
#[test]
fn the_rfc_example_counts_four_out_of_order() {
    check_batches(
        1,
        vec![
            (1..=92).collect(),
            vec![93, 94, 95, 100, 96, 97, 101, 98, 99, 102, 103],
            vec![104],
        ],
        &[(0, 0, 0), (0, 4, 0), (0, 0, 0)],
        "the RFC's sequence",
    );
}

// rfc-ambiguities.md §18 item 1 (loss counts Load PDUs, not gaps)
#[udpstp_req::req("RFC9946-8.1-L6", "RFC9097-8.1-L5")]
#[test]
fn loss_counts_every_missing_load_pdu() {
    check_batches(
        2,
        vec![vec![4, 5], vec![6, 10, 11, 20], vec![21]],
        &[(3, 0, 0), (3 + 8, 0, 0), (0, 0, 0)],
        "Load PDUs 1-3, then 7-9 and 12-19, never arrive",
    );
}

#[udpstp_req::req("RFC9946-8.2-L4", "RFC9946-8.2-L5", "RFC9946-8.2-L6")]
#[test]
fn a_repeated_load_pdu_is_a_duplicate() {
    check_batches(
        3,
        vec![vec![1, 2, 3], vec![3, 2, 4, 4], vec![5]],
        &[(0, 0, 0), (0, 0, 3), (0, 0, 0)],
        "3, 2 and 4 again",
    );
}

// rfc-ambiguities.md §18 item 1 (a Load PDU no longer in the lookback
// counts as out of order)
#[udpstp_req::req("RFC9946-8.2-L5", "RFC9946-8.2-L8", "RFC9946-8.2-L7")]
#[test]
fn the_lookback_holds_32_load_pdus() {
    // After 1-40 the lookback holds 9-40: 9 is found, 8 is not. The
    // duplicate 40 is not entered, so it doesn't push 9 out.
    check_batches(
        4,
        vec![(1..=40).collect(), vec![40, 9, 8]],
        &[(0, 0, 0), (0, 1, 2)],
        "40 and 9 are duplicates, 8 is out of order",
    );
}

// rfc-ambiguities.md §18 item 1 (a late Load PDU takes one off the
// loss of the interval it arrives in, which does not go below 0)
#[test]
fn a_late_load_pdu_in_the_next_trial_interval_leaves_the_loss_counted() {
    check_batches(
        5,
        vec![vec![1, 2, 4], vec![3, 5], vec![7, 6]],
        &[(1, 0, 0), (0, 1, 0), (0, 1, 0)],
        "3 arrives one Trial Interval after 4; 6 right after 7",
    );
}

// rfc-ambiguities.md §18 item 1 (a late Load PDU takes one off the
// loss only in an interval that counted it as lost)
#[test]
fn a_late_load_pdu_does_not_cancel_a_later_loss() {
    check_batches(
        9,
        vec![vec![1, 2, 4], vec![6, 3]],
        &[(1, 0, 0), (1, 1, 0)],
        "5 is lost in the interval where 3 arrives late",
    );
}

#[udpstp_req::req("RFC9946-8.2-L6")]
#[test]
fn a_late_load_pdu_enters_the_lookback() {
    check_batches(
        10,
        vec![vec![1, 3, 2], vec![2]],
        &[(0, 1, 0), (0, 0, 1)],
        "2 arrives late in the first Trial Interval, then again",
    );
}

#[udpstp_req::req("RFC9946-8.1-L6")]
#[test]
fn a_first_load_pdu_numbered_0_counts_no_loss() {
    check_batches(
        11,
        vec![vec![0, 1], vec![3]],
        &[(0, 0, 0), (1, 0, 0)],
        "0 and 1 arrive, then 3",
    );
}

// rfc-ambiguities.md §18 item 7 (an lpduSeqNo exactly 2^31 ahead of the
// next expected one counts as out of order)
#[test]
fn half_the_sequence_space_ahead_counts_as_late() {
    check_batches(
        12,
        vec![vec![1], vec![2 + (1 << 31), 2]],
        &[(0, 0, 0), (0, 1, 0)],
        "2 + 2^31 is behind 2",
    );
}

// RFC 9946 §8.2 (lpduSeqNo is 32 bits); rfc-ambiguities.md §18 item 7
// (compared modulo 2^32, so counting carries on across the wrap)
#[test]
fn sequence_errors_are_counted_across_the_wrap() {
    let max = u32::MAX;
    check_batches(
        6,
        vec![
            vec![max - 2],
            // max skips max - 1, and 1 then skips 0; both arrive late.
            vec![max, 1, max - 1, 0],
            // 2 follows 1; 1 again.
            vec![2, 1],
        ],
        &[(0, 0, 0), (0, 2, 0), (0, 0, 1)],
        "across the wrap (a first Load PDU that far from 1 counts no loss)",
    );
}

#[udpstp_req::req("RFC9946-8.2-6")]
#[test]
fn sub_intervals_count_sequence_errors_too() {
    // With a 50 ms Trial Interval and a 1 s Sub-Interval, the 22nd Status
    // PDU reports the first Sub-Interval: batches 1-20.
    let mut batches: Vec<Vec<u32>> = (0..23u32).map(|k| vec![10 * k + 1]).collect();
    // Batch 1 skips 12 and 13; 13 arrives in batch 2, a Trial Interval
    // later, so the Sub-Interval takes it off its loss and batch 2's
    // Trial Interval can't.
    batches[1] = vec![11, 14];
    batches[2] = vec![13, 21];
    // Batch 3 repeats 21.
    batches[3] = vec![21, 31];
    let outcome = server_sending(7, batches).run_for(Duration::from_millis(1200));

    // Loss per batch: 9 and 2 in batch 1, 6 in batch 2 (13 arrives late
    // and takes the Sub-Interval's count from 11 to 10 first), then 9 in
    // each of batches 3-20. Of lpduSeqNos 2-201, only 11, 13, 14 and
    // 21, 31, ... 201 arrive: 200 - 22 = 178 lost.
    let trial = trial_errors(&outcome);
    outcome.check(
        trial.len() > 3 && trial[1..4] == [(11, 0, 0), (6, 1, 0), (9, 0, 1)],
        &format!("the Trial Intervals of batches 1-3: {:?}", trial.get(1..4)),
    );
    let reported = sub_interval_errors(&outcome);
    outcome.check(
        reported.len() > 21 && reported[21] == (1, (178, 1, 1)),
        &format!("Sub-Interval 1: {:?}", reported.get(21)),
    );
}

/// subIntSeqNo and sisSav's rxDatagrams and Sequence Errors of each
/// Status PDU.
fn sub_intervals(outcome: &Outcome) -> Vec<(u32, u32, SeqErrors)> {
    statuses(outcome)
        .into_iter()
        .map(|b| {
            let errors = (u32_at(b, 56), u32_at(b, 60), u32_at(b, 64));
            (u32_at(b, 36), u32_at(b, 40), errors)
        })
        .collect()
}

// Tmax is the rest of the Sub-Interval whose gap counted a Load PDU as
// lost (conformance: tmax-within-the-sub-interval): one that arrives
// after that Sub-Interval closed stays lost there, and counts as received
// and out of order in the Sub-Interval it arrives in, without taking off
// that one's own loss.
#[udpstp_req::req("RFC9097-4-L4", "RFC9097-5.3-4", "RFC9097-6.3-5")]
#[test]
fn a_load_pdu_later_than_its_sub_interval_stays_lost() {
    // As in `sub_intervals_count_sequence_errors_too`, Sub-Interval 1
    // holds batches 1-20 and Sub-Interval 2 batches 21-40. One Load PDU a
    // batch, in order: batch k carries k + 1.
    let mut batches: Vec<Vec<u32>> = (0..20u32).map(|k| vec![k + 1]).collect();
    // The last batch of Sub-Interval 1 skips 21.
    batches.push(vec![22]);
    // The first batch of Sub-Interval 2 skips 23, then 21 arrives: taking
    // one off the loss of the Sub-Interval open now would cancel 23's.
    batches.push(vec![24, 21]);
    batches.extend((22..44u32).map(|k| vec![k + 3]));
    let outcome = server_sending(28, batches).run_for(Duration::from_millis(2200));

    // Sub-Interval 1: 2-20 and 22 arrived, 21 lost. Sub-Interval 2: 24,
    // 21 and 25-43 arrived (21 of them), 23 lost, 21 out of order.
    // Each Status PDU repeats the last Sub-Interval closed.
    let mut closed: Vec<(u32, u32, SeqErrors)> = sub_intervals(&outcome)
        .into_iter()
        .filter(|(seq, ..)| (1..=2).contains(seq))
        .collect();
    closed.dedup();
    outcome.check(
        closed == [(1, 20, (1, 0, 0)), (2, 21, (1, 1, 0))],
        &format!("Sub-Intervals 1 and 2: {closed:?}"),
    );
}

// ---- The simulator's reordering and duplication ----

#[udpstp_req::req("RFC9946-8.2-L4", "RFC9097-8.1-L5")]
#[test]
fn reordered_and_duplicated_load_pdus_are_counted() {
    let outcome = Scenario::new(8)
        // A Fixed-Rate Test needs an Operator Key (RFC 9946 §4.1).
        .operator(7, b"an operator key")
        .client_direction(Direction::Downstream)
        .client_requests_row(RowRequest::FixedRate(20))
        .reorder_load_pdus(25, 2)
        .duplicate_load_pdus(10)
        .run_for(Duration::from_millis(600));

    // What the trace shows the Client received up to its last Status PDU:
    // repeats, and first arrivals below the highest lpduSeqNo so far.
    let last_status = outcome
        .trace
        .iter()
        .rposition(is_status)
        .unwrap_or_default();
    let mut seen = HashSet::new();
    let mut highest = 0;
    let (mut ooo, mut dup) = (0, 0);
    for t in &outcome.trace[..last_status] {
        if t.destination != client() || !t.delivered || !t.payload.starts_with(&[0xBE, 0xEF]) {
            continue;
        }
        let seq_no = u32_at(&t.payload, 4);
        if !seen.insert(seq_no) {
            dup += 1;
        } else if seq_no < highest {
            ooo += 1;
        }
        highest = highest.max(seq_no);
    }
    outcome.check(ooo > 5 && dup > 5, "the network reordered and duplicated");
    let reported = trial_errors(&outcome);
    let sum = |f: fn(&SeqErrors) -> u32| reported.iter().map(f).sum::<u32>();
    outcome.check(
        sum(|e| e.1) == ooo && sum(|e| e.2) == dup,
        &format!(
            "{ooo} out of order and {dup} duplicates, reported {} and {}",
            sum(|e| e.1),
            sum(|e| e.2)
        ),
    );
}

fn ignored(outcome: &Outcome) -> u32 {
    outcome
        .client_data_events
        .iter()
        .map(|(_, e)| match e {
            udpstp_core::DataEvent::IgnoredLoadPdus { count } => *count,
            _ => 0,
        })
        .sum()
}

//  (a jump of up to 1024 counts at once; a larger one counts once
// the next Load PDU continues from it)
#[test]
fn a_jump_over_1024_waits_for_the_next_load_pdu() {
    check_batches(
        14,
        vec![vec![1], vec![1026]],
        &[(0, 0, 0), (1024, 0, 0)],
        "1024 at once",
    );
    check_batches(
        15,
        vec![vec![1], vec![1027, 1028]],
        &[(0, 0, 0), (1025, 0, 0)],
        "1025, confirmed by 1028",
    );
}

//  (a Load PDU far ahead that the next doesn't continue from is
// ignored: no loss, and loss detection carries on)
#[test]
fn a_far_ahead_load_pdu_does_not_switch_loss_detection_off() {
    let outcome = server_sending(16, vec![vec![1, 2], vec![10_000_002, 3, 5]])
        .run_for(Duration::from_millis(120));
    let reported = trial_errors(&outcome);
    outcome.check(
        reported.get(1) == Some(&(1, 0, 0)) && ignored(&outcome) == 1,
        &format!("lpduSeqNo 4 lost, the far one ignored: {reported:?}"),
    );
}

//  (lpduSeqNo starts at 1: a forged first Load PDU far ahead doesn't
// take counting with it)
#[udpstp_req::req("RFC9946-8.1-L6")]
#[test]
fn a_forged_first_load_pdu_is_ignored() {
    let outcome = server_sending(17, vec![vec![2_000_000_000, 1, 2], vec![4]])
        .run_for(Duration::from_millis(120));
    let reported = trial_errors(&outcome);
    outcome.check(
        reported.get(..2) == Some(&[(0, 0, 0), (1, 0, 0)][..]) && ignored(&outcome) == 1,
        &format!("lpduSeqNo 3 lost: {reported:?}"),
    );
}

//  (a first Load PDU far from 1 that the next continues from
// counts no loss, and counting starts from it)
#[test]
fn a_far_first_load_pdu_counts_no_loss() {
    check_batches(
        18,
        vec![vec![5_000_000, 5_000_001], vec![5_000_003]],
        &[(0, 0, 0), (1, 0, 0)],
        "5 000 000 first",
    );
}

//  (after two forged Load PDUs confirm a jump, 32 out-of-order
// Load PDUs in a run bring counting back to them)
#[test]
fn counting_follows_a_run_of_32_out_of_order_load_pdus() {
    let mut forged = vec![3_000_000, 3_000_001];
    forged.extend(3..=34);
    let outcome =
        server_sending(19, vec![vec![1, 2], forged, vec![36]]).run_for(Duration::from_millis(180));
    let reported = trial_errors(&outcome);
    outcome.check(
        reported.get(2) == Some(&(1, 0, 0)),
        &format!("lpduSeqNo 35 lost after the run: {reported:?}"),
    );
}

// ; rfc-ambiguities.md §18 item 7 (a held jump across the wrap)
#[test]
fn a_held_jump_counts_across_the_wrap() {
    let max = u32::MAX;
    check_batches(
        20,
        vec![vec![max - 1], vec![1500, 1501]],
        &[(0, 0, 0), (1501, 0, 0)],
        "from 2^32 - 1 to 1500",
    );
}

//  (only a consecutive run of out-of-order Load PDUs moves
// counting, and an in-order Load PDU ends a run)
#[test]
fn scattered_out_of_order_load_pdus_do_not_move_counting() {
    // 32 out of order, but every other lpduSeqNo: 100 is still behind.
    let mut forged = vec![3_000_000, 3_000_001];
    forged.extend((3..=65).step_by(2));
    let outcome =
        server_sending(21, vec![vec![1, 2], forged, vec![100]]).run_for(Duration::from_millis(180));
    let reported = trial_errors(&outcome);
    outcome.check(
        reported.get(2) == Some(&(0, 1, 0)),
        &format!("100 is out of order: {reported:?}"),
    );

    // Two runs of 16 split by an in-order Load PDU: 3000 counts 999 lost
    // after 2000.
    let mut split = vec![1, 1000];
    split.extend(2..=17);
    split.push(2000);
    split.extend(18..=33);
    check_batches(
        22,
        vec![split, vec![3000]],
        &[(998 + 999 - 32, 32, 0), (999, 0, 0)],
        "no run of 32",
    );
}

//  (a forged first Load PDU at 2^31 or more from 1 is held like
// any far one, not taken as a first Load PDU numbered before 1)
#[test]
fn a_forged_first_load_pdu_beyond_half_the_space_is_ignored() {
    let outcome = server_sending(23, vec![vec![3_000_000_000, 1, 2], vec![4]])
        .run_for(Duration::from_millis(120));
    let reported = trial_errors(&outcome);
    outcome.check(
        reported.get(..2) == Some(&[(0, 0, 0), (1, 0, 0)][..]) && ignored(&outcome) == 1,
        &format!("lpduSeqNo 3 lost: {reported:?}"),
    );
}

//  (heavy reordering within 1024 doesn't move counting: a block
// that overtakes another counts no loss)
#[test]
fn a_block_that_overtakes_another_counts_no_loss() {
    let mut reordered: Vec<u32> = (100..=199).collect();
    reordered.extend(50..=99);
    check_batches(
        24,
        vec![(1..=49).collect(), reordered, vec![200]],
        &[(0, 0, 0), (0, 50, 0), (0, 0, 0)],
        "100-199, then 50-99",
    );
}

//  (after an outage of more than 1024, reordering or a duplicate
// confirms the held jump too)
#[test]
fn reordering_right_after_an_outage_confirms_the_jump() {
    check_batches(
        25,
        vec![vec![1], vec![3000, 2999, 3001]],
        &[(0, 0, 0), (2997, 0, 0)],
        "2999 before 3000",
    );
    check_batches(
        26,
        vec![vec![1], vec![3000, 3000, 3001]],
        &[(0, 0, 0), (2998, 0, 1)],
        "3000 twice",
    );
}

//  (after a resync, an out-of-order Load PDU whose gap was counted
// in the same interval takes it off again)
#[test]
fn a_resync_lets_the_interval_take_back_its_own_loss() {
    let mut run: Vec<u32> = (3..=34).collect();
    run.extend([36, 35]);
    let outcome = server_sending(27, vec![vec![1, 2], vec![3_000_000, 3_000_001], run])
        .run_for(Duration::from_millis(180));
    let reported = trial_errors(&outcome);
    outcome.check(
        reported.get(2) == Some(&(0, 33, 0)),
        &format!("35 was lost, then came: {reported:?}"),
    );
}
