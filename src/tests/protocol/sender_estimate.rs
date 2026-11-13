//! The Load Receiver's estimate of the IP-Layer Sender Bit Rate: in
//! a Downstream Test the Client can't measure it at the Source, so it
//! estimates what the Server sent in each st of the Server's clock, from
//! the lpduSeqNo span of the Load PDUs that arrived, a lost one as big as
//! those around it.

use super::harness;

use std::time::Duration;

use crate::protocol::SentInterval;
use harness::{DataEvent, Scenario};

fn sent(events: &[(crate::protocol::SocketId, DataEvent)], estimated: bool) -> Vec<SentInterval> {
    events
        .iter()
        .filter_map(|(_, e)| match (e, estimated) {
            (DataEvent::Sent(s), false) | (DataEvent::SentEstimated(s), true) => Some(*s),
            _ => None,
        })
        .collect()
}

// docs/spec.md ( item 4: the Client estimates the IP-Layer Sender Bit Rate
// in a Downstream Test, labelled as such)
#[udpstp_req::req("RFC9097-7.4-1")]
#[test]
fn the_estimate_follows_what_the_server_sent_despite_loss() {
    // The path loses every Load PDU for 20 ms at a time, three times:
    // loss of every size alike. (A full tail-drop queue that loses one size
    // of Load PDU more than others biases the estimate.)
    let mut scenario = Scenario::new(1)
        .mode1(3, b"an estimate key")
        .client_config(|c| c.test_int_time = 5)
        .path_delay(Duration::from_millis(10), Duration::ZERO);
    for at in [1500, 2730, 3910] {
        scenario = scenario.network_drops(
            [0xBE, 0xEF],
            Duration::from_millis(at),
            Some(Duration::from_millis(at + 20)),
        );
    }
    let outcome = scenario.run_for(Duration::from_secs(8));
    let lost = outcome
        .trace
        .iter()
        .filter(|t| !t.delivered && t.payload.starts_with(&[0xBE, 0xEF]))
        .count();
    outcome.check(lost > 50, &format!("{lost} Load PDUs lost"));
    let measured = sent(&outcome.server_data_events, false);
    let estimated = sent(&outcome.client_data_events, true);
    // The whole Test: bytes the Server sent, and the Client's estimate,
    // over the st both report.
    let common = measured.len().min(estimated.len());
    let total = |s: &[SentInterval]| s[..common].iter().map(|s| s.bytes).sum::<u64>();
    let (sent_bytes, estimated_bytes) = (total(&measured), total(&estimated));
    outcome.check(
        common >= 80 && estimated_bytes.abs_diff(sent_bytes) * 100 <= sent_bytes * 3,
        &format!("within 3 % over {common} st: sent {sent_bytes}, estimated {estimated_bytes}"),
    );
    // st by st, most within 10 %: the two grids start one one-way delay
    // apart at most, and a Load PDU late across an st boundary shifts one.
    let close = measured[..common]
        .iter()
        .zip(&estimated[..common])
        .filter(|(m, e)| {
            m.ip_layer_sender_bit_rate
                .abs_diff(e.ip_layer_sender_bit_rate)
                * 10
                <= m.ip_layer_sender_bit_rate.max(1)
        })
        .count();
    outcome.check(
        close * 10 >= common * 9,
        &format!("{close} of {common} st within 10 %"),
    );
}
