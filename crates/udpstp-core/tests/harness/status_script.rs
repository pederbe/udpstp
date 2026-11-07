//! A scripted mode 1 Client in a Downstream Test that answers each Load PDU
//! with a Status PDU reporting a chosen Trial Interval, built from RFC 9946
//! Figures 3, 6 and 10, so tests can drive the Server's interval
//! classification and Load Rate Adjustment Algorithm directly.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use udpstp_core::{Classification, DataEvent};
use udpstp_wire::auth::{derive_keys, hmac_sha256};

use super::{Outcome, Scenario};

const KEY_ID: u8 = 7;
const KEY: &[u8] = b"a shared key";
pub const NODEL: u32 = u32::MAX;

fn sign(
    pdu: &mut [u8],
    digest: std::ops::Range<usize>,
    checksum: std::ops::Range<usize>,
    key: &[u8],
) {
    let mut zeroed = pdu.to_vec();
    zeroed[digest.clone()].fill(0);
    zeroed[checksum].fill(0);
    pdu[digest].copy_from_slice(&hmac_sha256(key, &zeroed));
}

/// A mode 1 Setup Request (RFC 9946 Figure 3), signed with the Client key.
fn setup_request(time: u32, max_bandwidth: u16) -> Vec<u8> {
    let mut b = vec![0u8; 56];
    b[0..2].copy_from_slice(&[0xAC, 0xE1]);
    b[2..4].copy_from_slice(&20u16.to_be_bytes());
    b[5] = 1;
    b[6..8].copy_from_slice(&0x1234u16.to_be_bytes());
    b[8] = 1;
    b[10..12].copy_from_slice(&max_bandwidth.to_be_bytes());
    b[14] = 0x01;
    b[15] = 1;
    b[16..20].copy_from_slice(&time.to_be_bytes());
    b[52] = KEY_ID;
    sign(&mut b, 20..52, 54..56, derive_keys(KEY, time).client());
    b
}

/// The Activation Request's parameters.
#[derive(Clone, Copy)]
pub struct Params {
    pub low: u16,
    pub upper: u16,
    pub use_ow_del_var: u8,
    pub seq_err_thresh: u16,
    pub ignore_ooo_dup: u8,
    /// 0xFFFF (CHTA_SRIDX_DEF) for the Server's default search.
    pub sr_index_conf: u16,
    pub modifier_bitmap: u8,
    pub high_speed_delta: u8,
    pub slow_adj_thresh: u16,
    pub rate_adj_algo: u8,
    pub trial_int: u16,
    pub test_int_time: u16,
    /// The key is an Operator Key, allowing Tests up to this many seconds.
    pub operator: Option<Option<u64>>,
    /// The Setup Request's maxBandwidth field.
    pub max_bandwidth: u16,
    /// subIntPeriod, ms.
    pub sub_int_period: u16,
}

/// The RFC 9946 defaults, with algorithm B searching from the first row.
pub const DEFAULTS: Params = Params {
    low: 30,
    upper: 90,
    use_ow_del_var: 1,
    seq_err_thresh: 10,
    ignore_ooo_dup: 1,
    sr_index_conf: 0xFFFF,
    modifier_bitmap: 0,
    high_speed_delta: 10,
    slow_adj_thresh: 3,
    rate_adj_algo: 0,
    trial_int: 50,
    test_int_time: 10,
    operator: None,
    max_bandwidth: 0,
    sub_int_period: 1000,
};

impl Params {
    /// Algorithm B starting at `row` (CHTA_SRIDX_ISSTART).
    pub fn starting_at(self, row: u16) -> Self {
        Self {
            sr_index_conf: row,
            modifier_bitmap: self.modifier_bitmap | 0x01,
            ..self
        }
    }
}

/// A mode 1 Downstream Activation Request (RFC 9946 Figure 6), signed with
/// the Client key of the Setup's authUnixTime.
fn activation_request(setup_time: u32, now: u32, p: Params) -> Vec<u8> {
    let mut b = vec![0u8; 104];
    let mut put16 = |at: usize, v: u16| b[at..at + 2].copy_from_slice(&v.to_be_bytes());
    put16(0, 0xACE2);
    put16(2, 20);
    put16(6, p.low);
    put16(8, p.upper);
    put16(10, p.trial_int);
    put16(12, p.test_int_time);
    put16(16, p.sr_index_conf);
    put16(20, p.slow_adj_thresh);
    put16(22, p.seq_err_thresh);
    put16(56, p.sub_int_period);
    b[4] = 2;
    b[18] = p.use_ow_del_var;
    b[19] = p.high_speed_delta;
    b[24] = p.ignore_ooo_dup;
    b[25] = p.modifier_bitmap;
    b[26] = p.rate_adj_algo;
    b[63] = 1;
    b[64..68].copy_from_slice(&now.to_be_bytes());
    b[100] = KEY_ID;
    sign(
        &mut b,
        68..100,
        102..104,
        derive_keys(KEY, setup_time).client(),
    );
    b
}

/// What a Status PDU reports about its Trial Interval.
#[derive(Clone, Copy, Default)]
pub struct Report {
    pub loss: u32,
    pub ooo: u32,
    pub dup: u32,
    pub delay_sum: u32,
    pub delay_cnt: u32,
    pub delay_max: u32,
    pub rtt_var_sample: u32,
    pub stop: bool,
    /// spduSeqNo, if not the next in turn.
    pub spdu_seq_no: Option<u32>,
}

/// A Report with `cnt` one-way delay samples of `sum / cnt` ms each, so
/// the average and the maximum agree.
pub fn delay(sum: u32, cnt: u32) -> Report {
    Report {
        delay_sum: sum,
        delay_cnt: cnt,
        delay_max: sum.checked_div(cnt).unwrap_or(0),
        rtt_var_sample: NODEL,
        ..Report::default()
    }
}

/// A Clean Interval with the default thresholds.
pub fn clean() -> Report {
    delay(0, 1)
}

/// An Errored Interval with the default thresholds: 11 losses.
pub fn errored() -> Report {
    Report {
        loss: 11,
        ..delay(0, 1)
    }
}

/// A Hold Interval with the default thresholds: DelayVar 50 ms.
pub fn hold() -> Report {
    delay(50, 1)
}

/// A Status PDU (RFC 9946 Figure 10) with this report.
fn status(seq: u32, r: Report) -> Vec<u8> {
    let mut b = vec![0u8; 204];
    b[0..2].copy_from_slice(&[0xFE, 0xED]);
    // A Client confirms a STOP with STOP2.
    b[2] = if r.stop { 2 } else { 0 };
    b[4..8].copy_from_slice(&seq.to_be_bytes());
    let mut put = |at: usize, v: u32| b[at..at + 4].copy_from_slice(&v.to_be_bytes());
    put(96, r.loss);
    put(100, r.ooo);
    put(104, r.dup);
    put(112, if r.delay_cnt > 0 { 0 } else { NODEL });
    put(116, r.delay_max);
    put(120, r.delay_sum);
    put(124, r.delay_cnt);
    put(128, NODEL);
    put(132, r.rtt_var_sample);
    b
}

/// The Server's classifications when a scripted mode 1 Client asks for
/// `params` and sends one Status PDU per report, each after a Load PDU.
pub fn reported(
    scenario: Scenario,
    params: Params,
    reports: Vec<Report>,
) -> (Outcome, Vec<Classification>) {
    reported_with_stop(scenario, params, reports, false)
}

/// As [`reported`], but sends a fresh STOP after the finite report sequence.
/// For row and classification checks only: callers observing silence, later
/// Load PDUs or watchdog expiry must use [`reported`] or [`reported_at`].
pub fn reported_then_stopped(
    scenario: Scenario,
    params: Params,
    reports: Vec<Report>,
) -> (Outcome, Vec<Classification>) {
    reported_with_stop(scenario, params, reports, true)
}

fn reported_with_stop(
    scenario: Scenario,
    params: Params,
    reports: Vec<Report>,
    stop_after_reports: bool,
) -> (Outcome, Vec<Classification>) {
    let setup_time = Rc::new(Cell::new(0u32));
    let t = setup_time.clone();
    let mut test_port = None;
    // The first row sends a Load PDU every 50 ms.
    let run = Duration::from_millis(50 * (reports.len() as u64 + 2));
    let mut stop = stop_after_reports.then(|| {
        let last = reports
            .iter()
            .enumerate()
            .map(|(i, r)| r.spdu_seq_no.unwrap_or(i as u32 + 1))
            .max()
            .unwrap_or(0);
        status(
            last.wrapping_add(1),
            Report {
                stop: true,
                ..delay(0, 1)
            },
        )
    });
    let mut to_send = reports
        .into_iter()
        .enumerate()
        .map(|(i, r)| status(r.spdu_seq_no.unwrap_or(i as u32 + 1), r));
    let scenario = match params.operator {
        Some(max) => scenario.server_operator_key(KEY_ID, KEY, max),
        None => scenario.server_key(KEY_ID, KEY),
    };
    let outcome = scenario
        .scripted_client_replying(
            move |clock| {
                t.set(clock.unix_secs as u32);
                setup_request(clock.unix_secs as u32, params.max_bandwidth)
            },
            move |clock, pdu| {
                if pdu.len() == 56 && pdu[8] == 2 && pdu[9] == 1 {
                    let port = u16::from_be_bytes([pdu[12], pdu[13]]);
                    test_port = Some(port);
                    return Some((
                        port,
                        activation_request(setup_time.get(), clock.unix_secs as u32, params),
                    ));
                }
                if !pdu.starts_with(&[0xBE, 0xEF]) {
                    return None;
                }
                Some((test_port?, to_send.next().or_else(|| stop.take())?))
            },
        )
        .run_for(run);
    let got = classifications(&outcome);
    (outcome, got)
}

/// The Outcome when a scripted mode 1 Client asks for `params` and sends
/// each report with the first Load PDU received at or after its time,
/// counted from the first Load PDU; the run lasts `run`.
pub fn reported_at(
    scenario: Scenario,
    params: Params,
    reports: Vec<(Duration, Report)>,
    run: Duration,
) -> Outcome {
    let setup_time = Rc::new(Cell::new(0u32));
    let t = setup_time.clone();
    let mut test_port = None;
    let mut first_load: Option<u128> = None;
    let mut to_send = reports
        .into_iter()
        .enumerate()
        .map(|(i, (at, r))| (at, status(r.spdu_seq_no.unwrap_or(i as u32 + 1), r)))
        .peekable();
    let scenario = match params.operator {
        Some(max) => scenario.server_operator_key(KEY_ID, KEY, max),
        None => scenario.server_key(KEY_ID, KEY),
    };
    scenario
        .scripted_client_replying(
            move |clock| {
                t.set(clock.unix_secs as u32);
                setup_request(clock.unix_secs as u32, params.max_bandwidth)
            },
            move |clock, pdu| {
                if pdu.len() == 56 && pdu[8] == 2 && pdu[9] == 1 {
                    let port = u16::from_be_bytes([pdu[12], pdu[13]]);
                    test_port = Some(port);
                    return Some((
                        port,
                        activation_request(setup_time.get(), clock.unix_secs as u32, params),
                    ));
                }
                if !pdu.starts_with(&[0xBE, 0xEF]) {
                    return None;
                }
                let since = clock.unix_nanos - *first_load.get_or_insert(clock.unix_nanos);
                let due = to_send.peek().is_some_and(|(at, _)| at.as_nanos() <= since);
                let (_, pdu) = due.then(|| to_send.next()).flatten()?;
                Some((test_port?, pdu))
            },
        )
        .run_for(run)
}

/// A scripted Server running an Upstream Test in Unauthenticated Mode,
/// whose Status PDUs report `reports` in turn to our Client, one for each
/// Load PDU it sends, and then a STOP2 (RFC 9946 Figures 3, 6, 7 and 10).
/// The Activation Response accepts the request as it came, so the Test has
/// the Client's thresholds, the defaults; every Status PDU asks for one
/// 500-byte datagram every 10 ms.
pub fn reported_to_client(scenario: Scenario, reports: Vec<Report>) -> Outcome {
    let run = Duration::from_millis(200 + 10 * reports.len() as u64);
    let next = reports.len() as u32 + 1;
    let mut to_send = reports
        .into_iter()
        .enumerate()
        .map(|(i, r)| (r.spdu_seq_no.unwrap_or(i as u32 + 1), r))
        .chain(std::iter::once((
            next,
            Report {
                stop: true,
                ..clean()
            },
        )));
    // srStruct (Figure 7): the second transmitter, one 500-byte datagram
    // every 10 ms.
    let sending = |pdu: &mut [u8]| {
        pdu[20..24].copy_from_slice(&10_000u32.to_be_bytes());
        pdu[24..28].copy_from_slice(&500u32.to_be_bytes());
        pdu[28..32].copy_from_slice(&1u32.to_be_bytes());
    };
    scenario
        .client_unauthenticated()
        .client_direction(super::Direction::Upstream)
        .scripted_server_replying_many(move |_, request| {
            // The Setup Response, testPort the Standard Port.
            if request.len() == 56 {
                let mut pdu = request.to_vec();
                pdu[8] = 2;
                pdu[9] = 1;
                pdu[12..14].copy_from_slice(&24601u16.to_be_bytes());
                return vec![pdu];
            }
            // The Activation Response: ACKOK, the request's parameters.
            if request.len() == 104 && request.starts_with(&[0xAC, 0xE2]) {
                let mut ack = request.to_vec();
                ack[5] = 1;
                sending(&mut ack[20..]);
                return vec![ack];
            }
            if request.starts_with(&[0xBE, 0xEF]) {
                return to_send
                    .next()
                    .map(|(seq, report)| {
                        let mut pdu = status(seq, report);
                        sending(&mut pdu);
                        vec![pdu]
                    })
                    .unwrap_or_default();
            }
            Vec::new()
        })
        .run_for(run)
}

/// The Server's classifications, in order.
pub fn classifications(outcome: &Outcome) -> Vec<Classification> {
    outcome
        .server_data_events
        .iter()
        .filter_map(|(_, e)| match e {
            DataEvent::TrialIntervalClassified(c) => Some(*c),
            _ => None,
        })
        .collect()
}
