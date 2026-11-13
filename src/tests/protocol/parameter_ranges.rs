//! Activation parameter ranges and defaults (RFC 9946 §7.1, §7.2.1, §7.3;
//! docs/spec.md stories 6 and 67).
//!
//! Expected ranges and defaults are written out here from their sources
//! (Y.1540 Amd 2 Tables B.1 and B.2, RFC 9946 §5.1 and §7.1), and PDUs are
//! read and written by their RFC 9946 Figure 6 offsets.

use super::harness;

use std::time::Duration;

use crate::protocol::Direction;
use harness::{ActivationParameter, ControlPhaseFailure, Outcome, Scenario};

/// A parameter's Figure 6 offset and width, and its range from the sources.
struct Field {
    parameter: ActivationParameter,
    at: usize,
    wide: bool,
    min: u16,
    max: u16,
}

const fn field(parameter: ActivationParameter, at: usize, wide: bool, min: u16, max: u16) -> Field {
    Field {
        parameter,
        at,
        wide,
        min,
        max,
    }
}

/// Y.1540 Amd 2 Table B.1 (thresholds, highSpeedDelta ≥ 2, slowAdjThresh >
/// 1, seqErrThresh 0-100) and Table B.2 (StatusFeedbackInterval 5-250 ms,
/// t 5-60 s, dt 0.1-10 s); Booleans 0-1 (RFC 9946 §7.1).
const FIELDS: [Field; 10] = [
    field(ActivationParameter::LowThresh, 6, true, 5, 250),
    field(ActivationParameter::UpperThresh, 8, true, 5, 250),
    field(ActivationParameter::TrialInt, 10, true, 5, 250),
    field(ActivationParameter::TestIntTime, 12, true, 5, 60),
    field(ActivationParameter::UseOwDelVar, 18, false, 0, 1),
    field(ActivationParameter::HighSpeedDelta, 19, false, 2, 255),
    field(ActivationParameter::SlowAdjThresh, 20, true, 2, u16::MAX),
    field(ActivationParameter::SeqErrThresh, 22, true, 0, 100),
    field(ActivationParameter::IgnoreOooDup, 24, false, 0, 1),
    field(ActivationParameter::SubIntPeriod, 56, true, 100, 10_000),
];

fn get(b: &[u8], f: &Field) -> u16 {
    if f.wide {
        u16::from_be_bytes([b[f.at], b[f.at + 1]])
    } else {
        b[f.at].into()
    }
}

/// An unauthenticated Setup Request (Figure 3).
fn setup_request() -> Vec<u8> {
    let mut b = vec![0u8; 56];
    b[0..2].copy_from_slice(&[0xAC, 0xE1]);
    b[2..4].copy_from_slice(&20u16.to_be_bytes());
    b[5] = 1;
    b[6..8].copy_from_slice(&0x1234u16.to_be_bytes());
    b[8] = 1;
    b[14] = 0x01;
    b
}

/// An unauthenticated Downstream Activation Request with the defaults,
/// changed by `edit`.
fn activation_request(edit: &dyn Fn(&mut Vec<u8>)) -> Vec<u8> {
    let mut b = vec![0u8; 104];
    for (at, v) in [
        (0, 0xACE2u16),
        (2, 20),
        (6, 30),
        (8, 90),
        (10, 50),
        (12, 10),
        (16, 0xFFFF),
        (20, 3),
        (22, 10),
        (56, 1000),
    ] {
        b[at..at + 2].copy_from_slice(&v.to_be_bytes());
    }
    b[4] = 2;
    b[18] = 1;
    b[19] = 10;
    b[24] = 1;
    edit(&mut b);
    b
}

/// The Server's Activation Response to a request made by `edit`.
fn response_to(seed: u64, edit: impl Fn(&mut Vec<u8>) + 'static) -> Vec<u8> {
    let mut test_port = None;
    let outcome = Scenario::new(seed)
        .server_allows_unauthenticated()
        .scripted_client_replying(
            |_| setup_request(),
            move |_, pdu| {
                if pdu.len() == 56 && pdu[8] == 2 && pdu[9] == 1 && test_port.is_none() {
                    let port = u16::from_be_bytes([pdu[12], pdu[13]]);
                    test_port = Some(port);
                    return Some((port, activation_request(&edit)));
                }
                None
            },
        )
        .run_for(Duration::from_millis(100));
    let response = outcome
        .trace
        .iter()
        .find(|t| t.len == 104 && t.payload.starts_with(&[0xAC, 0xE2]) && t.payload[5] != 0)
        .map(|t| t.payload.clone());
    outcome.check(response.is_some(), "an Activation Response");
    response.unwrap_or_default()
}

// docs/spec.md story 67 (each clamped to the nearest limit of its range)
#[udpstp_req::req("RFC9946-7.2.1-L1", "RFC9946-7.2.1-4")]
#[test]
fn out_of_range_values_are_clamped_to_the_nearest_limit() {
    for (i, f) in FIELDS.iter().enumerate() {
        let below = f.min.checked_sub(1);
        let above = f
            .max
            .checked_add(1)
            .filter(|&v| f.wide || v <= u16::from(u8::MAX));
        for (value, expected) in [(below, f.min), (above, f.max)] {
            let Some(value) = value else { continue };
            // Keep the other constraints satisfied while testing one field.
            let (at, wide) = (f.at, f.wide);
            let response = response_to(i as u64 * 2 + u64::from(value > f.min), move |b| {
                if wide {
                    b[at..at + 2].copy_from_slice(&value.to_be_bytes());
                } else {
                    b[at] = value as u8;
                }
                if at == 6 {
                    b[8..10].copy_from_slice(&250u16.to_be_bytes());
                }
                if at == 12 {
                    b[56..58].copy_from_slice(&100u16.to_be_bytes());
                }
                if at == 56 {
                    b[12..14].copy_from_slice(&60u16.to_be_bytes());
                }
            });
            assert_eq!(response[5], 1, "{:?} {value}: CHTA_CRSP_ACKOK", f.parameter);
            assert_eq!(
                get(&response, f),
                expected,
                "{:?} {value} is coerced to {expected}",
                f.parameter
            );
        }
    }
}

// docs/spec.md (: if LowThresh ends above UpperThresh, Low is set to
// Upper); rate-adjustment-algorithms.md A12
#[udpstp_req::req("RFC9946-7.2.1-L1")]
#[test]
fn a_low_threshold_above_the_upper_one_is_lowered_to_it() {
    let response = response_to(40, |b| {
        b[6..8].copy_from_slice(&200u16.to_be_bytes());
        b[8..10].copy_from_slice(&100u16.to_be_bytes());
    });
    assert_eq!(
        (get(&response, &FIELDS[0]), get(&response, &FIELDS[1])),
        (100, 100)
    );
}

// subIntPeriod is a slice of the Test Interval: one longer than the Test
// Interval is shortened to it
#[udpstp_req::req("RFC9946-7.2.1-L1")]
#[test]
fn a_sub_interval_longer_than_the_test_is_shortened() {
    let response = response_to(41, |b| {
        b[12..14].copy_from_slice(&5u16.to_be_bytes());
        b[56..58].copy_from_slice(&9000u16.to_be_bytes());
    });
    assert_eq!(get(&response, &FIELDS[9]), 5000);
}

// CHTA_CREQ_NONE asks for neither direction, so a verified Activation
// Request carrying it is refused with CHTA_CRSP_BADPARAM, not activated.
#[udpstp_req::req("RFC9946-7.1-L3")]
#[test]
fn an_activation_request_with_cmd_request_none_is_refused() {
    let response = response_to(43, |b| b[4] = 0);
    assert_eq!(response[5], 2, "CHTA_CRSP_BADPARAM");
}

// docs/spec.md ( Client defaults)
#[udpstp_req::req(
    "RFC9946-5.1-6",
    "RFC9946-5.1-10",
    "RFC9946-6.1-7",
    "RFC9946-7.1-3",
    "RFC9946-7.1-L4",
    "RFC9946-7.1-L7",
    "RFC9946-7.1-L10",
    "RFC9946-8.1-7",
    "RFC9097-8.1-7",
    "RFC9097-2-5",
    "RFC9097-4-L1",
    "RFC9097-4-L2",
    "RFC9097-4-L3",
    "RFC9946-6.1-10",
    "RFC9946-6.1-L4",
    "RFC9946-6.1-L6",
    "RFC9946-6.1-L9"
)]
#[test]
fn the_client_asks_for_the_defaults() {
    let outcome = Scenario::new(42)
        .server_allows_unauthenticated()
        .client_unauthenticated()
        .client_direction(Direction::Downstream)
        .run_for(Duration::from_millis(100));

    let request = outcome
        .trace
        .iter()
        .find(|t| t.len == 104 && t.payload.starts_with(&[0xAC, 0xE2]) && t.payload[5] == 0)
        .map(|t| t.payload.clone())
        .unwrap_or_default();
    let expected: [(usize, u16); 10] = [
        (0, 30),   // lowThresh
        (1, 90),   // upperThresh
        (2, 50),   // trialInt
        (3, 10),   // testIntTime
        (4, 1),    // useOwDelVar
        (5, 10),   // highSpeedDelta
        (6, 3),    // slowAdjThresh
        (7, 10),   // seqErrThresh
        (8, 1),    // ignoreOooDup
        (9, 1000), // subIntPeriod
    ];
    for (i, value) in expected {
        assert_eq!(
            get(&request, &FIELDS[i]),
            value,
            "{:?}",
            FIELDS[i].parameter
        );
    }
    assert_eq!(
        u16::from_be_bytes([request[16], request[17]]),
        0xFFFF,
        "srIndexConf"
    );
    assert_eq!(request[26], 0, "rateAdjAlgo B");
    let client = harness::CLIENT_ADDR.parse().unwrap();
    let setup = outcome
        .trace
        .iter()
        .find(|t| t.source == client && t.len == 56 && t.payload.starts_with(&[0xAC, 0xE1]))
        .map(|t| t.payload.clone())
        .unwrap_or_default();
    assert_eq!(setup[8], 1, "cmdRequest CHSR_CREQ_SETUPREQ");
    assert_eq!(setup[9], 0, "cmdResponse CHSR_CRSP_NONE");
    assert_eq!(setup[12..14], [0, 0], "testPort zero");
    assert_eq!(setup[14] & 0x01, 0x01, "CHSR_JUMBO_STATUS set");
    assert_eq!(setup[14] & 0x02, 0, "CHSR_TRADITIONAL_MTU not set");
}

/// A scripted Server that answers the Client's Activation Request with
/// `edit` applied.
fn client_told(seed: u64, edit: impl Fn(&mut Vec<u8>) + 'static) -> Outcome {
    Scenario::new(seed)
        .client_unauthenticated()
        .client_direction(Direction::Downstream)
        .scripted_server_replying_many(move |_, request| {
            if request.len() == 56 {
                let mut pdu = request.to_vec();
                pdu[8] = 2;
                pdu[9] = 1;
                pdu[12..14].copy_from_slice(&24601u16.to_be_bytes());
                return vec![pdu];
            }
            if request.len() == 104 && request.starts_with(&[0xAC, 0xE2]) && request[5] == 0 {
                let mut ack = request.to_vec();
                ack[5] = 1;
                edit(&mut ack);
                return vec![ack];
            }
            Vec::new()
        })
        .run_for(Duration::from_millis(100))
}

// A value outside the range table, thresholds out of order, or an unknown
// algorithm.
#[udpstp_req::req("RFC9946-7.3-L1")]
#[test]
fn the_client_ends_a_test_it_cannot_run() {
    for (seed, edit, parameter, value) in [
        (
            50,
            Box::new(|b: &mut Vec<u8>| b[10..12].copy_from_slice(&0u16.to_be_bytes()))
                as Box<dyn Fn(&mut Vec<u8>)>,
            ActivationParameter::TrialInt,
            0,
        ),
        (
            51,
            Box::new(|b: &mut Vec<u8>| b[6..8].copy_from_slice(&100u16.to_be_bytes())),
            ActivationParameter::LowThresh,
            100,
        ),
        (
            52,
            Box::new(|b: &mut Vec<u8>| b[26] = 9),
            ActivationParameter::RateAdjAlgo,
            9,
        ),
    ] {
        let outcome = client_told(seed, edit);
        outcome.check(
            outcome.client_failed_because(ControlPhaseFailure::UnrunnableParameter {
                parameter,
                value,
            }),
            &format!("{parameter:?} {value} ends the Test"),
        );
        let stop = outcome
            .trace
            .iter()
            .filter(|t| t.len == 204 && t.payload.starts_with(&[0xFE, 0xED]))
            .collect::<Vec<_>>();
        outcome.check(
            stop.len() == 1 && stop[0].payload[2] == 2,
            "and tells the Server with a STOP2 Status PDU",
        );
    }
}
