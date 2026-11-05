//! Coercion of Activation parameters, and rejection of parameters that
//! cannot be coerced (RFC 9946 §7.2, §7.2.1, §7.3).
//!
//! Scripted peers build Activation PDUs byte by byte from RFC 9946 Figure 6,
//! so a check doesn't rely on our own Client or Server getting the other side
//! right.

mod harness;

use std::time::Duration;

use harness::{ActivationParameter, ActivationRejection, Coercion, RowRequest, Scenario};
use udpstp_core::Direction;

const RUN: Duration = Duration::from_secs(1);

/// The simulated Server's Standard Port (harness::SERVER_ADDR).
const SERVER_PORT: u16 = 24601;

// ---- Test Setup PDU from RFC 9946 Figure 3 ----

const SETUP_LEN: usize = 56;

/// An unauthenticated Setup Request: pduId 0xACE1, protocolVer 20, mcIndex
/// 0, mcCount 1, mcIdent 0x1234, cmdRequest CHSR_CREQ_SETUPREQ,
/// modifierBitmap CHSR_JUMBO_STATUS.
fn setup_request() -> Vec<u8> {
    let mut b = vec![0u8; SETUP_LEN];
    b[0..2].copy_from_slice(&0xACE1u16.to_be_bytes());
    b[2..4].copy_from_slice(&20u16.to_be_bytes());
    b[5] = 1;
    b[6..8].copy_from_slice(&0x1234u16.to_be_bytes());
    b[8] = 1;
    b[14] = 0x01;
    b
}

/// A Setup Response accepting `request` with the given testPort
/// (CHSR_CREQ_SETUPRSP, CHSR_CRSP_ACKOK; §6.2.2).
fn setup_acceptance(request: &[u8], test_port: u16) -> Vec<u8> {
    let mut b = request.to_vec();
    b[8] = 2;
    b[9] = 1;
    b[12..14].copy_from_slice(&test_port.to_be_bytes());
    b
}

fn is_setup_acceptance(pdu: &[u8]) -> Option<u16> {
    (pdu.len() == SETUP_LEN && pdu[0..2] == [0xAC, 0xE1] && pdu[8] == 2 && pdu[9] == 1)
        .then(|| u16::from_be_bytes([pdu[12], pdu[13]]))
}

// ---- Test Activation PDU from RFC 9946 Figure 6 ----

const ACTIVATION_LEN: usize = 104;
const CMD_RESPONSE: usize = 5;
const LOW_THRESH: usize = 6;
const TRIAL_INT: usize = 10;
const TEST_INT_TIME: usize = 12;
const DSCP_ECN: usize = 15;
const SR_INDEX_CONF: usize = 16;
const RATE_ADJ_ALGO: usize = 26;

/// An unauthenticated Upstream Activation Request (CHTA_CREQ_TESTACTUS)
/// with RFC 9946 / TR-471 default parameters and the given rateAdjAlgo.
fn activation_request(rate_adj_algo: u8) -> Vec<u8> {
    let mut b = vec![0u8; ACTIVATION_LEN];
    let mut put16 = |at: usize, v: u16| b[at..at + 2].copy_from_slice(&v.to_be_bytes());
    put16(0, 0xACE2);
    put16(2, 20);
    put16(LOW_THRESH, 30);
    put16(8, 90); // upperThresh
    put16(TRIAL_INT, 50);
    put16(TEST_INT_TIME, 10);
    put16(SR_INDEX_CONF, 0xFFFF);
    put16(20, 3); // slowAdjThresh
    put16(22, 10); // seqErrThresh
    put16(56, 1000); // subIntPeriod
    b[4] = 1;
    b[18] = 1; // useOwDelVar
    b[19] = 10; // highSpeedDelta
    b[24] = 1; // ignoreOooDup
    b[RATE_ADJ_ALGO] = rate_adj_algo;
    b
}

fn is_activation(pdu: &[u8]) -> bool {
    pdu.len() == ACTIVATION_LEN && pdu[0..2] == [0xAC, 0xE2]
}

/// The last Activation PDU the first Client sent (`from_client`) or received.
fn last_activation(outcome: &harness::Outcome, from_client: bool) -> Vec<u8> {
    let client: std::net::SocketAddr = harness::CLIENT_ADDR.parse().unwrap();
    let found = outcome
        .trace
        .iter()
        .rev()
        .find(|t| is_activation(&t.payload) && (t.source == client) == from_client)
        .map(|t| t.payload.clone());
    outcome.check(found.is_some(), "an Activation PDU was exchanged");
    found.unwrap_or_default()
}

/// Runs a scripted Client that completes the Setup exchange and sends
/// `activation` to the testPort.
fn scripted_activation(seed: u64, activation: Vec<u8>) -> harness::Outcome {
    let mut activation = Some(activation);
    Scenario::new(seed)
        .server_allows_unauthenticated()
        .scripted_client_replying(
            |_| setup_request(),
            move |_, pdu| {
                let test_port = is_setup_acceptance(pdu)?;
                Some((test_port, activation.take()?))
            },
        )
        .run_for(RUN)
}

/// A scripted Server that accepts the Setup (testPort = its Standard Port,
/// so the Activation Request reaches the same script) and answers the
/// Activation Request with ACKOK after `coerce` has edited it.
fn coercing_server(seed: u64, coerce: impl Fn(&mut [u8]) + 'static) -> Scenario {
    Scenario::new(seed)
        .client_unauthenticated()
        .client_direction(Direction::Upstream)
        .scripted_server(move |_, request| {
            if request.len() == SETUP_LEN {
                return Some(setup_acceptance(request, SERVER_PORT));
            }
            if !is_activation(request) {
                return None;
            }
            let mut response = request.to_vec();
            response[CMD_RESPONSE] = 1;
            coerce(&mut response);
            Some(response)
        })
}

// docs/spec.md (every coercion is reported); a starting row beyond the table
// is coerced to its top row, which the harness's Server allows a Consumer
// to start at (conformance: sr-index-conf-beyond-the-top-row-coerced)
#[udpstp_req::req(
    "RFC9946-7.2-1",
    "RFC9946-7.2.1-3",
    "RFC9946-7.2.1-4",
    "RFC9946-7.2-L1"
)]
#[test]
fn a_coerced_row_is_reported_at_both_ends() {
    let outcome = Scenario::new(1)
        .server_allows_unauthenticated()
        .client_unauthenticated()
        .client_direction(Direction::Upstream)
        .client_requests_row(RowRequest::StartAt(2000))
        .run_for(RUN);

    // The first row at the default table's top rate: 12 Gbps as bursts of
    // 100 1500-byte packets, the default MTU.
    let expected = Coercion {
        parameter: ActivationParameter::SrIndexConf,
        requested: 2000,
        coerced: 1069,
    };
    let responses = outcome.activation_responses();
    outcome.check(
        responses.len() == 1 && responses[0].cmd_response == 1,
        "the Activation Response is CHTA_CRSP_ACKOK",
    );
    outcome.check(
        responses[0].sr_index_conf == 1069,
        "it carries the coerced value",
    );
    outcome.check(
        outcome.server_coercions() == vec![expected],
        "the Server reported the one coercion",
    );
    outcome.check(
        outcome.client_coercions() == vec![expected],
        "the Client reported the one coercion",
    );
    outcome.check(
        outcome.client_activated_direction(0) == Some(Direction::Upstream),
        "the Client accepted the coerced Test",
    );
}

#[udpstp_req::req("RFC9946-7.2-1", "RFC9946-7.2-L1")]
#[test]
fn an_acceptable_request_is_echoed_without_coercions() {
    let outcome = Scenario::new(2)
        .server_allows_unauthenticated()
        .client_unauthenticated()
        .client_direction(Direction::Downstream)
        .run_for(RUN);

    let request = last_activation(&outcome, true);
    let response = last_activation(&outcome, false);
    outcome.check(
        response[CMD_RESPONSE] == 1,
        "an Activation Request was accepted",
    );
    // Everything from lowThresh to rateAdjAlgo (with modifierBitmap), and
    // subIntPeriod.
    outcome.check(
        request[6..27] == response[6..27] && request[56..58] == response[56..58],
        "the response carries the requested parameters",
    );
    outcome.check(
        outcome.server_coercions().is_empty() && outcome.client_coercions().is_empty(),
        "no coercion is reported at either end",
    );
}

// docs/spec.md story 14 (every coerced parameter is shown)
#[udpstp_req::req("RFC9946-7.3-4")]
#[test]
fn the_client_reports_each_parameter_the_server_coerced() {
    let outcome = coercing_server(3, |response| {
        response[TEST_INT_TIME..TEST_INT_TIME + 2].copy_from_slice(&5u16.to_be_bytes());
        // ECT(1): the Client reports it, and still sends Not-ECT.
        response[DSCP_ECN] = 0b01;
    })
    .run_for(RUN);

    outcome.check(
        outcome.client_coercions()
            == vec![
                Coercion {
                    parameter: ActivationParameter::TestIntTime,
                    requested: 10,
                    coerced: 5,
                },
                Coercion {
                    parameter: ActivationParameter::DscpEcn,
                    requested: 0,
                    coerced: 0b01,
                },
            ],
        "the Client reported both coercions, in field order",
    );
    outcome.check(
        outcome.client_activated_direction(0) == Some(Direction::Upstream),
        "the Client accepted the coerced Test",
    );
}

// docs/spec.md story 15
#[udpstp_req::req("RFC9946-7.3-L1")]
#[test]
fn the_client_ends_the_test_when_a_coercion_is_unacceptable() {
    let outcome = coercing_server(4, |response| {
        response[TRIAL_INT..TRIAL_INT + 2].copy_from_slice(&100u16.to_be_bytes());
        response[TEST_INT_TIME..TEST_INT_TIME + 2].copy_from_slice(&5u16.to_be_bytes());
    })
    .client_config(|c| {
        c.unacceptable_coercions
            .push(ActivationParameter::TestIntTime)
    })
    .run_for(RUN);

    outcome.check(
        outcome.client_refused_coercion()
            == Some(Coercion {
                parameter: ActivationParameter::TestIntTime,
                requested: 10,
                coerced: 5,
            }),
        "the Client ended the Test over the testIntTime coercion",
    );
    outcome.check(
        outcome.client_activated_direction(0).is_none(),
        "the Client did not prepare the Test",
    );
}

// docs/spec.md story 15
#[udpstp_req::req("RFC9946-7.3-L1")]
#[test]
fn the_first_refused_coercion_in_field_order_is_reported() {
    let outcome = coercing_server(8, |response| {
        response[TRIAL_INT..TRIAL_INT + 2].copy_from_slice(&100u16.to_be_bytes());
        response[TEST_INT_TIME..TEST_INT_TIME + 2].copy_from_slice(&5u16.to_be_bytes());
    })
    .client_config(|c| {
        c.unacceptable_coercions
            .push(ActivationParameter::TestIntTime)
    })
    .client_config(|c| c.unacceptable_coercions.push(ActivationParameter::TrialInt))
    .run_for(RUN);

    outcome.check(
        outcome.client_refused_coercion().map(|c| c.parameter)
            == Some(ActivationParameter::TrialInt),
        "trialInt comes before testIntTime in the PDU",
    );
}

/// Every parameter RFC 9946 §7.2.1 lets the Server coerce, plus srIndexConf,
/// with its offset and width in Figure 6.
const COERCIBLE: [(ActivationParameter, usize, usize); 13] = [
    (ActivationParameter::LowThresh, 6, 2),
    (ActivationParameter::UpperThresh, 8, 2),
    (ActivationParameter::TrialInt, 10, 2),
    (ActivationParameter::TestIntTime, 12, 2),
    (ActivationParameter::DscpEcn, 15, 1),
    (ActivationParameter::SrIndexConf, 16, 2),
    (ActivationParameter::UseOwDelVar, 18, 1),
    (ActivationParameter::HighSpeedDelta, 19, 1),
    (ActivationParameter::SlowAdjThresh, 20, 2),
    (ActivationParameter::SeqErrThresh, 22, 2),
    (ActivationParameter::IgnoreOooDup, 24, 1),
    (ActivationParameter::RateAdjAlgo, 26, 1),
    (ActivationParameter::SubIntPeriod, 56, 2),
];

fn field(pdu: &[u8], at: usize, width: usize) -> u16 {
    match width {
        1 => pdu[at].into(),
        _ => u16::from_be_bytes([pdu[at], pdu[at + 1]]),
    }
}

// Every parameter RFC 9946 §7.2.1 lets the Server coerce.
#[udpstp_req::req("RFC9946-7.3-4")]
#[test]
fn the_client_reports_a_coercion_of_every_coercible_parameter() {
    let outcome = coercing_server(9, |response| {
        // A different step per field, so no two fields can stand in for each other.
        for (step, (_, at, width)) in (1..).zip(COERCIBLE) {
            let bumped = field(response, at, width).wrapping_add(step);
            match width {
                1 => response[at] = bumped as u8,
                _ => response[at..at + 2].copy_from_slice(&bumped.to_be_bytes()),
            }
        }
    })
    .run_for(RUN);

    let request = last_activation(&outcome, true);
    let expected: Vec<Coercion> = COERCIBLE
        .iter()
        .zip(1..)
        .map(|(&(parameter, at, width), step)| {
            let requested = field(&request, at, width);
            let coerced = match width {
                1 => u16::from((requested as u8).wrapping_add(step as u8)),
                _ => requested.wrapping_add(step),
            };
            Coercion {
                parameter,
                requested,
                coerced,
            }
        })
        .collect();
    outcome.check(
        outcome.client_coercions() == expected,
        &format!(
            "the Client reported {expected:?}, not {:?}",
            outcome.client_coercions()
        ),
    );
}

const MODIFIER_BITMAP: usize = 25;

// RFC 9946 §7.1 modifierBitmap (CHTA_SRIDX_ISSTART = 0x01,
// CHTA_RAND_PAYLOAD = 0x02)
#[udpstp_req::req("RFC9946-7.3-4", "RFC9946-7.3-L1")]
#[test]
fn the_client_reports_changed_modifier_bits() {
    let outcome = coercing_server(12, |response| {
        // A starting row becomes a fixed rate, with random payload added.
        response[MODIFIER_BITMAP] = (response[MODIFIER_BITMAP] & !0x01) | 0x02;
    })
    .client_requests_row(RowRequest::StartAt(5))
    .client_config(|c| {
        c.unacceptable_coercions
            .push(ActivationParameter::SrIndexIsStart)
    })
    .run_for(RUN);

    let is_start = Coercion {
        parameter: ActivationParameter::SrIndexIsStart,
        requested: 1,
        coerced: 0,
    };
    outcome.check(
        outcome.client_coercions()
            == vec![
                is_start,
                Coercion {
                    parameter: ActivationParameter::RandomPayload,
                    requested: 0,
                    coerced: 1,
                },
            ],
        "the Client reported both modifier bits",
    );
    outcome.check(
        outcome.client_refused_coercion() == Some(is_start),
        "the Client refused to run a Fixed-Rate Test it didn't ask for",
    );
}

// docs/spec.md story 15 (only the coercions the user refused end the Test)
#[udpstp_req::req("RFC9946-7.3-4")]
#[test]
fn the_client_accepts_coercions_it_did_not_refuse() {
    let outcome = coercing_server(5, |response| {
        response[TRIAL_INT..TRIAL_INT + 2].copy_from_slice(&100u16.to_be_bytes());
    })
    .client_config(|c| {
        c.unacceptable_coercions
            .push(ActivationParameter::TestIntTime)
    })
    .run_for(RUN);

    outcome.check(
        outcome.client_refused_coercion().is_none(),
        "no coercion was refused",
    );
    outcome.check(
        outcome.client_activated_direction(0) == Some(Direction::Upstream),
        "the Client prepared the Test",
    );
}

// RFC 9946 §12.3.8 (rateAdjAlgo values: B = 0, C = 1)
#[udpstp_req::req(
    "RFC9946-7.2.1-L2",
    "RFC9946-12.3.8-L1",
    "RFC9946-7.2-L1",
    "RFC9946-12.3.9-L1"
)]
#[test]
fn an_unknown_rate_adjustment_algorithm_is_rejected_with_badparam() {
    let outcome = scripted_activation(6, activation_request(2));

    let responses: Vec<_> = outcome
        .trace
        .iter()
        .filter(|t| is_activation(&t.payload) && t.payload[CMD_RESPONSE] != 0)
        .collect();
    outcome.check(responses.len() == 1, "one Activation Response was sent");
    outcome.check(
        responses[0].payload[CMD_RESPONSE] == 2,
        "it is CHTA_CRSP_BADPARAM",
    );
    outcome.check(
        outcome.server_rejected_activation_because(ActivationRejection::UnacceptableParameter(
            ActivationParameter::RateAdjAlgo,
        )),
        "the Server reported rejecting rateAdjAlgo",
    );
    outcome.check(
        outcome.server_released_count() == 1,
        "the Server released the rejected Test Connection",
    );
}

#[udpstp_req::req("RFC9946-12.3.8-L1", "RFC9946-8.1-8")]
#[test]
fn algorithm_c_is_accepted() {
    let outcome = scripted_activation(7, activation_request(1));

    let accepted = outcome
        .trace
        .iter()
        .find(|t| is_activation(&t.payload) && t.payload[CMD_RESPONSE] != 0);
    outcome.check(
        accepted.is_some_and(|t| t.payload[CMD_RESPONSE] == 1 && t.payload[RATE_ADJ_ALGO] == 1),
        "the Activation Request was accepted with algorithm C",
    );
}

#[udpstp_req::req("RFC9946-7.1-2", "RFC9946-7.2.1-L1")]
#[test]
fn the_server_clears_a_requested_ecn_codepoint() {
    let mut request = activation_request(0);
    // DSCP 0 with CE.
    request[DSCP_ECN] = 0b11;
    let outcome = scripted_activation(20, request);

    let response = last_activation(&outcome, false);
    outcome.check(
        response[CMD_RESPONSE] == 1 && response[DSCP_ECN] == 0,
        "the Test is accepted with ECN Not-ECT",
    );
    outcome.check(
        outcome.server_coercions()
            == vec![Coercion {
                parameter: ActivationParameter::DscpEcn,
                requested: 0b11,
                coerced: 0,
            }],
        "the Server reported the coercion",
    );
    outcome.check(outcome.dscp_ecn == [(0, 0)], "the Server sends Not-ECT");
}

#[udpstp_req::req("RFC9946-7.1-2")]
#[test]
fn the_client_sends_not_ect_whatever_the_server_answers() {
    // DSCP 8 with ECT(1).
    let outcome = coercing_server(21, |response| response[DSCP_ECN] = 8 << 2 | 0b01)
        .client_config(|c| c.dscp = 8)
        .run_for(RUN);

    outcome.check(
        outcome.client_activated_direction(0) == Some(Direction::Upstream),
        "the Client accepted the Test",
    );
    outcome.check(
        outcome.dscp_ecn == [(1, 8 << 2)],
        "the Client marks DSCP 8, Not-ECT",
    );
}

//  (a Server may lower the marking to 0, not raise it)
#[udpstp_req::req("RFC9946-7.3-L1")]
#[test]
fn the_client_refuses_a_dscp_it_did_not_ask_for() {
    // Asked for 0, told EF.
    let outcome = coercing_server(22, |response| response[DSCP_ECN] = 46 << 2).run_for(RUN);

    outcome.check(
        outcome.client_activated_direction(0).is_none(),
        "the Client didn't start the Test",
    );
    outcome.check(outcome.dscp_ecn.is_empty(), "nothing was marked");
}
