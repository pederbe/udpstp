//! Pure construction and storage observations for 's local example.
//! Enabled explicitly, outside the stable protocol interface.

use std::mem::{align_of, size_of};

use super::*;

/// Observed inline layouts of the production types.
pub fn layouts() -> [(&'static str, usize, usize); 10] {
    [
        ("DataPhase", size_of::<DataPhase>(), align_of::<DataPhase>()),
        (
            "LoadSender",
            size_of::<LoadSender>(),
            align_of::<LoadSender>(),
        ),
        (
            "LoadReceiver",
            size_of::<LoadReceiver>(),
            align_of::<LoadReceiver>(),
        ),
        ("Lookback", size_of::<Lookback>(), align_of::<Lookback>()),
        (
            "PendingEvents",
            size_of::<PendingEvents>(),
            align_of::<PendingEvents>(),
        ),
        ("SendPlan", size_of::<SendPlan>(), align_of::<SendPlan>()),
        (
            "RxDatagram",
            size_of::<RxDatagram>(),
            align_of::<RxDatagram>(),
        ),
        (
            "Transmitter",
            size_of::<Transmitter>(),
            align_of::<Transmitter>(),
        ),
        (
            "SentEstimate",
            size_of::<SentEstimate>(),
            align_of::<SentEstimate>(),
        ),
        (
            "SubIntervalStats",
            size_of::<SubIntervalStats>(),
            align_of::<SubIntervalStats>(),
        ),
    ]
}

/// Padding beyond the Lookback fields' own sizes.
pub fn lookback_padding() -> usize {
    let lookback = Lookback::new();
    let fields = std::mem::size_of_val(&lookback.expected)
        + std::mem::size_of_val(&lookback.started)
        + std::mem::size_of_val(&lookback.held)
        + std::mem::size_of_val(&lookback.late_run)
        + std::mem::size_of_val(&lookback.entries)
        + std::mem::size_of_val(&lookback.len)
        + std::mem::size_of_val(&lookback.next);
    size_of::<Lookback>() - fields
}

pub fn now(micros: u64) -> Now {
    Now {
        mono: MonoTime::from_duration(Duration::from_micros(micros)),
        wall: WallTime::from_unix(Duration::from_micros(1_790_000_000_000_000 + micros)),
    }
}

pub fn phase(receiver: bool, seed: u64) -> DataPhase {
    let setup = DataPhaseSetup {
        now: now(0),
        peer: "127.0.0.1:24601".parse().unwrap(),
        dscp_ecn: 0,
        seed,
        checksum: false,
        keys: None,
        key_id: 0,
        auth_mode: auth_mode::MODE_1,
        test_interval: Duration::from_secs(10),
        trial_interval: Duration::from_millis(50),
        sub_interval: Duration::from_secs(1),
        this_end: End::Client,
        thresholds: None,
        algorithm: None,
        counter: None,
        max_payload: 1222,
        grid: None,
        max_bandwidth_kbps: None,
    };
    // Row 1000's ordinary IPv4 shape: ten 1222-byte Load PDUs per 100 us.
    let rate = SendingRate {
        tx_interval1: 100,
        udp_payload1: 1222,
        burst_size1: 10,
        ..SendingRate::default()
    };
    if receiver {
        DataPhase::load_receiver(setup, Duration::from_secs(1), rate)
    } else {
        DataPhase::load_sender(setup, &rate, IpVersion::V4, false)
    }
}

pub fn owned_bytes(phase: &DataPhase) -> usize {
    let p = &phase.pending;
    let queues = p.rate_events.capacity() * size_of::<DataEvent>()
        + p.sub_intervals.capacity() * size_of::<SubIntervalMeasurement>()
        + (p.sent.capacity() + p.sent_estimates.capacity()) * size_of::<SentInterval>()
        + phase.assigned.capacity() * size_of::<(MonoTime, f64)>();
    queues
        + match &phase.role {
            Role::LoadReceiver(_) => size_of::<LoadReceiver>(),
            Role::LoadSender(s) => {
                size_of::<LoadSender>() + s.random.as_ref().map_or(0, |r| r.pool.len())
            }
        }
}

/// The Load Receiver's current, not yet reported Trial Interval.
pub fn unreported_datagrams(phase: &DataPhase) -> u64 {
    let Role::LoadReceiver(receiver) = &phase.role else {
        panic!("expected a Load Receiver");
    };
    u64::from(receiver.trial_counts.datagrams)
}
