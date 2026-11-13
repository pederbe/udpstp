//! Upstream Tests (RFC 9946 §7.2.2, §8.1, §8.2): the Client is the Load
//! Sender and executes the Server's Sending Rate Structure; the Server is
//! the Load Receiver and sends Status PDUs carrying it.
//!
//! Sending Rate Structures are read from the PDUs by their Figure 6 and
//! Figure 10 offsets, and expected Load PDU sizes come from those fields,
//! not from our table.

mod harness;

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::time::Duration;

use harness::{ActivationParameter, DataEvent, Outcome, RowRequest, Scenario, TraceEntry};
use udpstp_core::{Direction, EndReason};

/// A Sending Rate Structure's seven fields (RFC 9946 Figure 7).
type Rate = [u32; 7];
const STOP2: u8 = 2;
/// The random-size flag OB-UDPST sets in a size field.
const RANDOM_SIZE: u32 = 0x8000_0000;

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn rate_at(b: &[u8], at: usize) -> Rate {
    std::array::from_fn(|i| u32_at(b, at + 4 * i))
}

fn put_rate(b: &mut [u8], at: usize, rate: Rate) {
    for (i, v) in rate.iter().enumerate() {
        b[at + 4 * i..at + 4 * i + 4].copy_from_slice(&v.to_be_bytes());
    }
}

fn is_load(t: &TraceEntry) -> bool {
    t.payload.starts_with(&[0xBE, 0xEF])
}

fn is_status(t: &TraceEntry) -> bool {
    t.len == 204 && t.payload.starts_with(&[0xFE, 0xED])
}

fn upstream(seed: u64, row: u16) -> Scenario {
    Scenario::new(seed)
        // A Fixed-Rate Test needs an Operator Key (RFC 9946 §4.1).
        .operator(7, b"an operator key")
        .client_direction(Direction::Upstream)
        .client_requests_row(RowRequest::FixedRate(row))
}

/// The Server's Activation Response's Sending Rate Structure (Figure 6:
/// srStruct at byte 28).
fn activation_rate(outcome: &Outcome) -> Rate {
    let response = outcome.trace.iter().find(|t| {
        t.len == 104 && t.payload.starts_with(&[0xAC, 0xE2]) && t.destination.port() == 40000
    });
    outcome.check(response.is_some(), "an Activation Response");
    rate_at(&response.map(|t| t.payload.clone()).unwrap_or_default(), 28)
}

/// The Load PDUs a node sent, grouped by send time: one group per Burst.
fn bursts(outcome: &Outcome, from: SocketAddr) -> BTreeMap<Duration, Vec<usize>> {
    let mut bursts: BTreeMap<Duration, Vec<usize>> = BTreeMap::new();
    for t in outcome
        .trace
        .iter()
        .filter(|t| t.source == from && is_load(t))
    {
        bursts.entry(t.at).or_default().push(t.len);
    }
    bursts
}

fn client() -> SocketAddr {
    harness::CLIENT_ADDR.parse().unwrap()
}

// srStruct: two Transmitters, the add-on at the end of the second's Burst.
#[udpstp_req::req("RFC9946-8.1-6", "RFC9946-7.1-5", "RFC9946-7.1-L9", "RFC9946-7.3-5")]
#[test]
fn the_client_sends_the_servers_sending_rate_structure() {
    // Row 35: Bursts of 3 datagrams plus an add-on every millisecond.
    let outcome = upstream(1, 35).run_for(Duration::from_millis(300));
    let [_, _, _, interval2, payload2, burst2, addon2] = activation_rate(&outcome);
    outcome.check(burst2 > 1 && addon2 > 0, "the row has Bursts and an add-on");

    let bursts = bursts(&outcome, client());
    let expected: Vec<usize> = std::iter::repeat_n(payload2 as usize, burst2 as usize)
        .chain([addon2 as usize])
        .collect();
    outcome.check(
        bursts.len() > 200 && bursts.values().all(|sizes| *sizes == expected),
        &format!("every Burst is {expected:?}"),
    );
    let starts: Vec<Duration> = bursts.keys().copied().collect();
    outcome.check(
        starts
            .windows(2)
            .all(|w| w[1] - w[0] == Duration::from_micros(interval2.into())),
        "one Burst per txInterval2",
    );
}

// ADR 0004 (srStruct sizes are IPv4-based; an IPv6 Load Sender takes
// 20 bytes off each)
#[test]
fn an_ipv6_client_sends_20_bytes_less() {
    let outcome = upstream(2, 35).ipv6().run_for(Duration::from_millis(100));
    let [_, _, _, _, payload2, burst2, addon2] = activation_rate(&outcome);
    let client: SocketAddr = harness::CLIENT_ADDR_V6.parse().unwrap();

    let expected: Vec<usize> = std::iter::repeat_n(payload2 as usize - 20, burst2 as usize)
        .chain([addon2 as usize - 20])
        .collect();
    let bursts = bursts(&outcome, client);
    outcome.check(
        !bursts.is_empty() && bursts.values().all(|sizes| *sizes == expected),
        &format!("every Burst is {expected:?}"),
    );
}

// The Status PDUs carry authMode, and zero auth fields outside mode 2; spec
//  (the Server sends Status PDUs in an Upstream Test)
#[udpstp_req::req("RFC9946-8.1-1", "RFC9946-5.3.2-6", "RFC9946-7.2.2-4", "RFC9946-8.2-1")]
#[test]
fn the_server_sends_status_pdus_with_its_row() {
    let outcome = upstream(3, 35).run_for(Duration::from_millis(400));
    let rate = activation_rate(&outcome);
    let server: SocketAddr = harness::SERVER_ADDR.parse().unwrap();

    let statuses: Vec<&TraceEntry> = outcome
        .trace
        .iter()
        .filter(|t| t.destination == client() && t.source != server && is_status(t))
        .collect();
    outcome.check(statuses.len() >= 6, "a Status PDU every Trial Interval");
    for t in &statuses {
        let b = &t.payload;
        outcome.check(rate_at(b, 8) == rate, "srStruct is the Server's row");
        outcome.check(
            b[163] == 1 && b[164..201].iter().all(|&x| x == 0),
            "authMode 1, auth fields zero",
        );
    }
    let gaps: BTreeSet<Duration> = statuses.windows(2).map(|w| w[1].at - w[0].at).collect();
    outcome.check(
        gaps == BTreeSet::from([Duration::from_millis(50)]),
        &format!("every 50 ms: {gaps:?}"),
    );
}

/// A scripted Server that accepts an Upstream Test with `activation` as
/// its Sending Rate Structure, and answers the first Load PDU with a Status
/// PDU carrying `later`.
fn scripted(seed: u64, activation: Rate, later: Option<Rate>) -> Outcome {
    scripted_on(
        Scenario::new(seed),
        activation,
        later,
        Duration::from_millis(100),
    )
}

/// A Scenario's Client against a scripted Server that assigns `activation`
/// in its Activation Response, then `later` in its first Status PDU.
fn scripted_on(
    scenario: Scenario,
    activation: Rate,
    later: Option<Rate>,
    run: Duration,
) -> Outcome {
    let mut answered = false;
    scenario
        .client_unauthenticated()
        .client_direction(Direction::Upstream)
        .scripted_server_replying_many(move |_, request| {
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
                put_rate(&mut ack, 28, activation);
                return vec![ack];
            }
            if request.starts_with(&[0xBE, 0xEF])
                && !answered
                && let Some(later) = later
            {
                answered = true;
                let mut status = vec![0u8; 204];
                status[0..2].copy_from_slice(&[0xFE, 0xED]);
                status[4..8].copy_from_slice(&1u32.to_be_bytes());
                put_rate(&mut status, 8, later);
                return vec![status];
            }
            Vec::new()
        })
        .run_for(run)
}

fn sizes(outcome: &Outcome) -> Vec<(Duration, usize)> {
    outcome
        .trace
        .iter()
        .filter(|t| t.source == client() && is_load(t))
        .map(|t| (t.at, t.len))
        .collect()
}

#[udpstp_req::req("RFC9946-8.1-1", "RFC9946-8.1-6")]
#[test]
fn the_client_follows_the_srstruct_of_later_status_pdus() {
    let first = [0, 0, 0, 1000, 500, 1, 0];
    let later = [0, 0, 0, 1000, 700, 2, 0];
    let outcome = scripted(4, first, Some(later));

    let sent = sizes(&outcome);
    let switch = sent.iter().position(|&(_, len)| len == 700);
    outcome.check(
        switch.is_some_and(|i| {
            sent[..i].iter().all(|&(_, len)| len == 500)
                && sent[i..].iter().all(|&(_, len)| len == 700)
        }),
        &format!("500-byte Load PDUs, then 700-byte ones: {sent:?}"),
    );
    let first_700 = switch.map(|i| sent[i].0).unwrap_or_default();
    outcome.check(
        sent.iter().filter(|&&(at, _)| at == first_700).count() == 2,
        "two per Burst after the change",
    );
}

// docs/spec.md  (srStruct values beyond OB-UDPST's sanity limits are
// not sent: an interval above 1 s, a size above 8972 bytes or a Burst above
// 100 datagrams is zeroed).
#[test]
fn srstruct_values_beyond_the_sanity_limits_are_not_sent() {
    for (seed, rate, what) in [
        (5, [1000, 500, 101, 1000, 600, 1, 0], "a Burst of 101"),
        (6, [1000, 9000, 1, 1000, 600, 1, 0], "a 9000-byte payload"),
        (
            7,
            [1_000_001, 500, 1, 1000, 600, 1, 0],
            "an interval above 1 s",
        ),
    ] {
        let outcome = scripted(seed, rate, None);
        let sent = sizes(&outcome);
        outcome.check(
            !sent.is_empty() && sent.iter().all(|&(_, len)| len == 600),
            &format!("{what}: only the second Transmitter sends"),
        );
    }
}

// docs/spec.md  (OB-UDPST's random-size flag and row-0 add-on are
// accepted); sizes are random from
// 52 bytes up to the low 31 bits)
#[test]
fn a_random_size_add_on_is_sent_at_random_sizes() {
    // OB-UDPST's row 0: an add-on of up to 1222 bytes every 50 ms, with
    // the random-size flag; here every millisecond, to collect sizes.
    let outcome = scripted(8, [0, 0, 0, 1000, 0, 0, RANDOM_SIZE | 1222], None);

    let lens: BTreeSet<usize> = sizes(&outcome).into_iter().map(|(_, len)| len).collect();
    outcome.check(
        lens.len() > 20 && lens.iter().all(|&len| (52..=1222).contains(&len)),
        &format!("random sizes within 52-1222: {lens:?}"),
    );
}

// In an Upstream Test the Server's Status PDUs carry STOP2 and the Client
// confirms with its next Load PDUs (RFC 9946 Figure 1).
#[udpstp_req::req("RFC9946-9-2", "RFC9946-9-4", "RFC9946-9-5", "RFC9946-9-8")]
#[test]
fn an_upstream_test_stops_gracefully() {
    let outcome = upstream(9, 35).run_for(Duration::from_secs(13));
    let server: SocketAddr = harness::SERVER_ADDR.parse().unwrap();

    let first_stop = outcome
        .trace
        .iter()
        .find(|t| {
            t.source != server && t.destination == client() && is_status(t) && t.payload[2] == STOP2
        })
        .map(|t| t.at);
    outcome.check(first_stop.is_some(), "the Server's Status PDUs carry STOP2");
    let loads: Vec<&TraceEntry> = outcome
        .trace
        .iter()
        .filter(|t| t.source == client() && is_load(t))
        .collect();
    let confirming: Vec<&&TraceEntry> = loads.iter().filter(|t| t.payload[2] == STOP2).collect();
    outcome.check(
        confirming.len() == 1 && loads.last().is_some_and(|t| t.payload[2] == STOP2),
        "one Load PDU confirms, and it is the Client's last",
    );
    let ended = |events: &[(udpstp_core::SocketId, DataEvent)]| {
        events
            .iter()
            .filter_map(|(_, e)| match e {
                DataEvent::Ended(reason) => Some(*reason),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    outcome.check(
        ended(&outcome.client_data_events) == [EndReason::Completed]
            && ended(&outcome.server_data_events) == [EndReason::Completed],
        "both end gracefully",
    );
}

// docs/spec.md  (a Client that ends a Test early sends a PDU marked
// TEST_ACT_STOP2 at once): in an Upstream Test, a Load PDU
#[test]
fn an_upstream_client_that_stops_early_sends_a_stop2_load_pdu() {
    let stop_at = Duration::from_millis(500);
    let outcome = upstream(10, 35)
        .client_stops_data_phase_at(stop_at)
        .run_for(Duration::from_secs(2));

    let loads: Vec<&TraceEntry> = outcome
        .trace
        .iter()
        .filter(|t| t.source == client() && is_load(t))
        .collect();
    outcome.check(
        loads
            .last()
            .is_some_and(|t| t.at == stop_at && t.payload[2] == STOP2),
        "the Client's last Load PDU carries STOP2 and goes out at once",
    );
    outcome.check(
        outcome
            .server_data_events
            .iter()
            .any(|(_, e)| *e == DataEvent::Ended(EndReason::StoppedByClient)),
        "the Server ends the Test Connection",
    );

    // Refusing a coercion, before any Data Phase: a STOP2 Load PDU too.
    let outcome = Scenario::new(11)
        .server_allows_unauthenticated()
        .client_unauthenticated()
        .client_direction(Direction::Upstream)
        .client_requests_row(RowRequest::StartAt(5000))
        .client_config(|c| {
            c.unacceptable_coercions
                .push(ActivationParameter::SrIndexConf)
        })
        .run_for(Duration::from_secs(1));
    let loads: Vec<&TraceEntry> = outcome
        .trace
        .iter()
        .filter(|t| t.source == client() && is_load(t))
        .collect();
    outcome.check(
        loads.len() == 1 && loads[0].payload[2] == STOP2 && loads[0].len == 32,
        "one header-only Load PDU marked STOP2",
    );
    outcome.check(
        outcome
            .server_data_events
            .iter()
            .any(|(_, e)| *e == DataEvent::Ended(EndReason::StoppedByClient)),
        "the Server ends the Test Connection",
    );
}

/// A scripted Server that accepts an Upstream Test with `rate`, and sends
/// a Status PDU marked STOP2 with an all-zero srStruct: at once when
/// `with_ack`, else after the first Load PDU.
fn scripted_stop(seed: u64, rate: Rate, with_ack: bool) -> Outcome {
    let mut stopped = false;
    let stop = || {
        let mut status = vec![0u8; 204];
        status[0..2].copy_from_slice(&[0xFE, 0xED]);
        status[2] = STOP2;
        status[4..8].copy_from_slice(&1u32.to_be_bytes());
        status
    };
    Scenario::new(seed)
        .client_unauthenticated()
        .client_direction(Direction::Upstream)
        .scripted_server_replying_many(move |_, request| {
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
                put_rate(&mut ack, 28, rate);
                return if with_ack {
                    vec![ack, stop()]
                } else {
                    vec![ack]
                };
            }
            if request.starts_with(&[0xBE, 0xEF]) && !with_ack && !stopped {
                stopped = true;
                return vec![stop()];
            }
            Vec::new()
        })
        .run_for(Duration::from_millis(100))
}

fn loads_from_client(outcome: &Outcome) -> Vec<&TraceEntry> {
    outcome
        .trace
        .iter()
        .filter(|t| t.source == client() && is_load(t))
        .collect()
}

// OB-UDPST takes srStruct only from TEST Status PDUs: a STOP2 Status PDU's
// zeroed srStruct doesn't silence the confirmation.
#[udpstp_req::req("RFC9946-9-5")]
#[test]
fn a_stop_status_pdu_does_not_change_the_rate() {
    let outcome = scripted_stop(12, [0, 0, 0, 1000, 500, 1, 0], false);

    let loads = loads_from_client(&outcome);
    outcome.check(
        loads
            .last()
            .is_some_and(|t| t.payload[2] == STOP2 && t.len == 500),
        "the confirmation goes out at the same rate",
    );
}

// The Client confirms STOP2 with its next Load PDU, even when the Server's
// srStruct gives it nothing to send.
#[udpstp_req::req("RFC9946-9-5")]
#[test]
fn a_client_with_nothing_to_send_still_confirms_a_stop() {
    let outcome = scripted_stop(13, [0; 7], true);

    let loads = loads_from_client(&outcome);
    outcome.check(
        loads.len() == 1 && loads[0].payload[2] == STOP2 && loads[0].len == 32,
        &format!("one header-only Load PDU confirms: {} sent", loads.len()),
    );
}

// docs/spec.md  (srStruct values within OB-UDPST's sanity limits are
// kept, those beyond are zeroed, in the Activation Response and in Status
// PDUs alike)
#[test]
fn the_sanity_limits_keep_their_edges() {
    let outcome = scripted(14, [1_000_000, 8972, 100, 0, 0, 0, 0], None);
    let sent = sizes(&outcome);
    outcome.check(
        sent.len() == 100 && sent.iter().all(|&(_, len)| len == 8972),
        &format!("a Burst of 100 at 8972 bytes: {}", sent.len()),
    );

    for (add_on, what) in [
        (RANDOM_SIZE | 9000, "a random add-on above 8972 is zeroed"),
        (RANDOM_SIZE, "a random add-on of at most 0 bytes is none"),
    ] {
        let outcome = scripted(15, [0, 0, 0, 1000, 600, 1, add_on], None);
        let sent = sizes(&outcome);
        outcome.check(
            !sent.is_empty() && sent.iter().all(|&(_, len)| len == 600),
            what,
        );
    }

    let outcome = scripted(
        16,
        [0, 0, 0, 1000, 500, 1, 0],
        Some([0, 0, 0, 1000, 500, 101, 0]),
    );
    let sent = sizes(&outcome);
    outcome.check(
        sent.iter().all(|&(_, len)| len == 500) && sent.len() < 10,
        &format!("a Status PDU's Burst of 101 is zeroed: {} sent", sent.len()),
    );
}

// A new Sending Rate Structure can switch Transmitters on and off.
#[udpstp_req::req("RFC9946-8.1-6")]
#[test]
fn a_new_srstruct_can_swap_transmitters() {
    let outcome = scripted(
        17,
        [0, 0, 0, 1000, 500, 1, 0],
        Some([2000, 300, 2, 0, 0, 0, 0]),
    );

    let sent = sizes(&outcome);
    let switch = sent.iter().position(|&(_, len)| len == 300);
    outcome.check(
        switch.is_some_and(|i| sent[i..].iter().all(|&(_, len)| len == 300)),
        &format!("Transmitter 2 stops and Transmitter 1 starts: {sent:?}"),
    );
    let starts: Vec<Duration> = sent
        .iter()
        .filter(|&&(_, len)| len == 300)
        .map(|&(at, _)| at)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    outcome.check(
        starts.len() > 10
            && starts
                .windows(2)
                .all(|w| w[1] - w[0] == Duration::from_millis(2)),
        "a Burst of two every 2 ms",
    );
}

// ADR 0004 (an IPv6 sender's
// random floor is 20 bytes lower too: 32)
#[test]
fn an_ipv6_client_draws_random_sizes_from_32() {
    let outcome = Scenario::new(18)
        .ipv6()
        .client_unauthenticated()
        .client_direction(Direction::Upstream)
        .scripted_server_replying_many(move |_, request| {
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
                put_rate(&mut ack, 28, [0, 0, 0, 1000, 0, 0, RANDOM_SIZE | 60]);
                return vec![ack];
            }
            Vec::new()
        })
        .run_for(Duration::from_millis(300));

    let client: SocketAddr = harness::CLIENT_ADDR_V6.parse().unwrap();
    let lens: BTreeSet<usize> = outcome
        .trace
        .iter()
        .filter(|t| t.source == client && is_load(t))
        .map(|t| t.len)
        .collect();
    outcome.check(
        lens.first() == Some(&32) && lens.last() == Some(&40),
        &format!("sizes 32-40 (52-60 less 20): {lens:?}"),
    );
}

/// An unauthenticated Setup Request (RFC 9946 Figure 3).
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

/// An unauthenticated Upstream Activation Request for the Server's default
/// row (RFC 9946 Figure 6).
fn upstream_activation_request() -> Vec<u8> {
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
    b[4] = 1;
    b[18] = 1;
    b[19] = 10;
    b[24] = 1;
    b
}

// ADR 0008 (the Server stops a Test Connection whose Load Sender runs
// above 1.5 times its assigned rate for 3 Trial Intervals); RFC 9946 §9
#[test]
fn the_server_stops_a_client_sending_over_its_rate() {
    let mut test_port = None;
    let mut seq = 0u32;
    let outcome = Scenario::new(19)
        .server_allows_unauthenticated()
        .scripted_client_replying(
            |_| setup_request(),
            move |_, pdu| {
                if pdu.len() == 56 && pdu[8] == 2 && pdu[9] == 1 {
                    let port = u16::from_be_bytes([pdu[12], pdu[13]]);
                    test_port = Some(port);
                    return Some((port, upstream_activation_request()));
                }
                let port = test_port?;
                // Each Activation Response and TEST Status PDU is answered
                // with one 1400-byte Load PDU: 224 kbit/s, over 1.5 times
                // row 0's 100 kbit/s. Each skips 19 lpduSeqNos, so every
                // Trial Interval is errored and algorithm B keeps the row
                // at 0. (Bytes are counted from the datagram's length, not
                // its udpPayload field.)
                if (pdu.len() == 104 || pdu.len() == 204) && pdu[2] == 0 {
                    seq += 20;
                    let mut load = vec![0u8; 1400];
                    load[0..2].copy_from_slice(&[0xBE, 0xEF]);
                    load[4..8].copy_from_slice(&seq.to_be_bytes());
                    load[8..10].copy_from_slice(&1400u16.to_be_bytes());
                    return Some((port, load));
                }
                None
            },
        )
        .run_for(Duration::from_secs(2));

    let server_statuses: Vec<&TraceEntry> = outcome
        .trace
        .iter()
        .filter(|t| t.destination == client() && is_status(t))
        .collect();
    let stop = server_statuses.iter().position(|t| t.payload[2] == STOP2);
    outcome.check(
        stop == Some(3),
        &format!(
            "the fourth Status PDU, after three Trial Intervals over rate, carries STOP2: {stop:?}"
        ),
    );
    outcome.check(
        outcome
            .server_data_events
            .iter()
            .any(|(_, e)| *e == DataEvent::Ended(EndReason::OverRate)),
        &format!(
            "the Server ends over rate: {:?}",
            outcome.server_data_events
        ),
    );
}

/// The IP-layer rate, with IPv4 headers, of the Load PDUs the Client sent
/// from `from` on, in bit/s.
fn client_ip_rate(outcome: &Outcome, from: Duration) -> f64 {
    let loads: Vec<(Duration, usize)> = sizes(outcome)
        .into_iter()
        .filter(|&(at, _)| at >= from)
        .collect();
    let (Some(first), Some(last)) = (loads.first(), loads.last()) else {
        return 0.0;
    };
    // The last Burst's bits go out after the span ends.
    let bits: usize = loads
        .iter()
        .filter(|&&(at, _)| at < last.0)
        .map(|&(_, len)| (len + 28) * 8)
        .sum();
    bits as f64 / (last.0 - first.0).as_secs_f64()
}

fn capped(outcome: &Outcome) -> Vec<DataEvent> {
    outcome
        .client_data_events
        .iter()
        .map(|(_, e)| *e)
        .filter(|e| matches!(e, DataEvent::SendingRateCapped { .. }))
        .collect()
}

// Spec story 9 and client-keeps-to-its-max-bandwidth. Use emitted packet
// lengths, including the actual IP headers, rather than remote size fields.
#[udpstp_req::req("RFC9946-11-5")]
#[test]
fn short_and_random_payloads_keep_the_actual_ip_rate_within_the_cap() {
    for ipv6 in [false, true] {
        for size in [1, 31, 32, 51, 52, RANDOM_SIZE | 1, RANDOM_SIZE | 51] {
            for later in [false, true] {
                let tiny = [1000, size, 100, 1000, size, 100, size];
                let ordinary = [1000, 1222, 1, 0, 0, 0, 0];
                let mut scenario = Scenario::new(40).client_max_bandwidth(1);
                if ipv6 {
                    scenario = scenario.ipv6();
                }
                let outcome = scripted_on(
                    scenario,
                    if later { ordinary } else { tiny },
                    later.then_some(tiny),
                    Duration::from_secs(2),
                );
                let source: SocketAddr = if ipv6 {
                    harness::CLIENT_ADDR_V6
                } else {
                    harness::CLIENT_ADDR
                }
                .parse()
                .unwrap();
                let groups = bursts(&outcome, source);
                // Each Transmitter has its own start offset. Measure its
                // complete Bursts between matching boundaries, then add
                // the rates without mixing partial windows.
                let mut rate = 0.0;
                for count in [100, 101] {
                    let loads: Vec<_> = groups
                        .iter()
                        .filter(|(at, sizes)| {
                            **at >= Duration::from_millis(200) && sizes.len() == count
                        })
                        .collect();
                    outcome.check(loads.len() >= 2, "each Transmitter sends repeated Bursts");
                    let first = *loads.first().unwrap().0;
                    let last = *loads.last().unwrap().0;
                    let bits: usize = loads
                        .iter()
                        .filter(|(at, _)| **at < last)
                        .flat_map(|(_, sizes)| sizes.iter())
                        .map(|len| (len + if ipv6 { 48 } else { 28 }) * 8)
                        .sum();
                    rate += bits as f64 / (last - first).as_secs_f64();
                }
                outcome.check(
                    rate <= 1_000_001.0,
                    &format!("actual IP rate {rate}, size {size}, IPv6 {ipv6}, update {later}"),
                );
            }
        }
    }
}

// maxBandwidth: the most the Client wants to be sent or to send; docs/spec.md
// story 9 (conformance: client-keeps-to-its-max-bandwidth)
#[udpstp_req::req("RFC9946-11-5")]
#[test]
fn a_client_keeps_to_its_max_bandwidth_against_a_server_that_assigns_more() {
    // 1250-byte packets every millisecond: 10 Mbps, then 20.
    let ten = [1000, 1222, 1, 0, 0, 0, 0];
    let twenty = [1000, 1222, 2, 0, 0, 0, 0];
    let outcome = scripted_on(
        Scenario::new(21).client_max_bandwidth(2),
        ten,
        Some(twenty),
        Duration::from_secs(2),
    );
    let rate = client_ip_rate(&outcome, Duration::from_millis(500));
    outcome.check(
        (1_900_000.0..=2_000_000.0).contains(&rate),
        &format!("the Client sends at its 2 Mbps: {rate} bit/s"),
    );
    outcome.check(
        sizes(&outcome).iter().all(|&(_, len)| len == 1222),
        "packet sizes unchanged",
    );
    let capped = capped(&outcome);
    let structures: u32 = capped
        .iter()
        .map(|e| match e {
            DataEvent::SendingRateCapped { structures, .. } => *structures,
            _ => 0,
        })
        .sum();
    outcome.check(
        structures == 2
            && capped.iter().all(|e| {
                matches!(
                    e,
                    DataEvent::SendingRateCapped {
                        cap_kbps: 2_000,
                        ..
                    }
                )
            })
            && capped.iter().any(|e| {
                matches!(
                    e,
                    DataEvent::SendingRateCapped {
                        highest_kbps: 20_000,
                        ..
                    }
                )
            }),
        &format!("both structures reported as capped at 2 Mbps: {capped:?}"),
    );
}

// docs/spec.md story 9 (a Client that states no maxBandwidth, or one the
// Server's structure is within, sends the structure as given)
#[test]
fn a_structure_within_the_max_bandwidth_is_sent_as_given() {
    let ten = [1000, 1222, 1, 0, 0, 0, 0];
    for (seed, mbps) in [(22, 0), (23, 10)] {
        let outcome = scripted_on(
            Scenario::new(seed).client_max_bandwidth(mbps),
            ten,
            None,
            Duration::from_secs(1),
        );
        let rate = client_ip_rate(&outcome, Duration::from_millis(200));
        outcome.check(
            (9_900_000.0..=10_100_000.0).contains(&rate),
            &format!("maxBandwidth {mbps}: 10 Mbps sent: {rate} bit/s"),
        );
        outcome.check(capped(&outcome).is_empty(), "nothing capped");
    }
}

// docs/spec.md story 9 (with our own Server, which never assigns more than
// the stated maxBandwidth, nothing is capped)
#[test]
fn our_server_never_makes_the_client_cap_its_rate() {
    let outcome = Scenario::new(24)
        .client_unauthenticated()
        .server_allows_unauthenticated()
        .client_direction(Direction::Upstream)
        .client_max_bandwidth(20)
        .run_for(Duration::from_secs(6));
    outcome.check(
        outcome
            .client_data_events
            .iter()
            .any(|(_, e)| matches!(e, DataEvent::Sent(_))),
        "the Test ran",
    );
    outcome.check(capped(&outcome).is_empty(), "nothing capped");
}
