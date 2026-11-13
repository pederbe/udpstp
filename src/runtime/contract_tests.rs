//! The plan-executor contract against the real Data Phase Engine on
//! loopback (docs/spec.md story 119; S2b). The simulator's executor meets
//! the same contract (`src/tests/protocol/plan_executor.rs`), whose
//! checks this includes.
//!
//! A Client core and a Server core set up an Upstream Fixed-Rate Test in
//! memory (the contract's `activate`); the Client's Data Phase then runs on
//! the real engine, on a loopback socket, and a plain socket in the
//! Server's place records what arrives. Nothing answers with Status PDUs,
//! so the Load Sender is stopped before its 1 s timeout would stop it.
//! Pacing precision on a quiet host uses the loopback precision suite.
//! This runs in the gating tiers, on busy hosts too. Reported Skipped
//! Bursts account for unsent slots, without hiding unexplained gaps.
//!
//! The socket adapter records the engine's independently supplied stamping
//! time on the Test Time Base. An activation clock pair projects that time
//! into the sender's wall-clock domain. The received header is checked
//! against this witness, rather than a post-receive clock read that also
//! includes socket buffering and observer scheduling. This is a send-domain
//! witness, neither a kernel arrival timestamp nor measured send latency.

use std::collections::BTreeMap;
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[cfg(any(target_os = "linux", target_os = "android"))]
use crate::protocol::TransportError;
use crate::protocol::{DataEvent, Direction, MonoTime, Now, SocketId};

use crate::runtime::clock::Clock;
use crate::runtime::contract::{self, Observed, Schedule, Tolerance};
use crate::runtime::engine::{self, EngineSettings, FromEngine};
use crate::runtime::socket::{self, Adapter, Open};

/// Witnesses from the socket send boundary, independent of lpduTime.
struct SendWitnesses {
    activation: Now,
    sent: BTreeMap<u32, Option<Duration>>,
}

impl SendWitnesses {
    fn new(activation: Now) -> Self {
        Self {
            activation,
            sent: BTreeMap::new(),
        }
    }

    fn record(&mut self, payload: &[u8], stamped: Option<MonoTime>) {
        let Some(pdu) = Observed::load_pdu(Duration::ZERO, payload, payload.len()) else {
            return;
        };
        let wall = stamped
            .and_then(|at| {
                at.as_duration()
                    .checked_sub(self.activation.mono.as_duration())
            })
            .and_then(|since| self.activation.wall.as_unix().checked_add(since));
        // A repeated successful send of the same sequence number has no
        // unique witness. A failed send never reaches this recording.
        self.sent
            .entry(pdu.seq_no())
            .and_modify(|old| *old = None)
            .or_insert(wall);
    }

    fn check(
        &self,
        observed: &[Observed],
        schedule: Schedule,
        tolerance: Tolerance,
    ) -> Vec<String> {
        let mut witnessed = observed.to_vec();
        for pdu in &mut witnessed {
            let Some(Some(wall)) = self.sent.get(&pdu.seq_no()) else {
                return vec![format!(
                    "no unique sender clock witness for Load PDU {}",
                    pdu.seq_no()
                )];
            };
            pdu.wall = *wall;
        }
        contract::check(&witnessed, schedule, tolerance)
    }
}

struct WitnessOpen<O> {
    inner: O,
    witnesses: Arc<Mutex<SendWitnesses>>,
}

impl<O: Open> Open for WitnessOpen<O> {
    type Adapter = WitnessAdapter<O::Adapter>;

    fn open(self, socket: UdpSocket, settings: &socket::Settings) -> Self::Adapter {
        WitnessAdapter {
            inner: self.inner.open(socket, settings),
            witnesses: self.witnesses,
        }
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn pool_hooks(&self) -> crate::runtime::engine::pool::Hooks {
        self.inner.pool_hooks()
    }
}

/// Forwards every platform operation unchanged and records only sends
/// accepted by it. No lock is held during I/O.
struct WitnessAdapter<A> {
    inner: A,
    witnesses: Arc<Mutex<SendWitnesses>>,
}

fn load_pdu(seq: u32, at: Duration) -> [u8; 32] {
    let mut payload = [0u8; 32];
    payload[..2].copy_from_slice(&[0xBE, 0xEF]);
    payload[4..8].copy_from_slice(&seq.to_be_bytes());
    payload[8..10].copy_from_slice(&32u16.to_be_bytes());
    payload[20..24].copy_from_slice(&u32::try_from(at.as_secs()).unwrap().to_be_bytes());
    payload[24..28].copy_from_slice(&at.subsec_nanos().to_be_bytes());
    payload
}

/// Three correctly stamped Send Plans, at independently specified wire
/// times and socket stamping inputs, one millisecond apart.
fn witnessed_plans() -> (SendWitnesses, Vec<Observed>, Schedule, Tolerance) {
    witnessed_plans_at([1_000, 2_000, 3_000])
}

fn witnessed_plans_at(micros: [u64; 3]) -> (SendWitnesses, Vec<Observed>, Schedule, Tolerance) {
    let epoch = Duration::from_secs(1_700_000_000);
    let mut witnesses = SendWitnesses::new(Now {
        mono: MonoTime::from_duration(Duration::from_secs(5)),
        wall: crate::protocol::WallTime::from_unix(epoch),
    });
    let observed = micros
        .into_iter()
        .enumerate()
        .map(|(index, micros)| {
            let seq = u32::try_from(index + 1).unwrap();
            let payload = load_pdu(seq, epoch + Duration::from_micros(micros));
            witnesses.record(
                &payload,
                Some(MonoTime::from_duration(
                    Duration::from_secs(5) + Duration::from_micros(micros),
                )),
            );
            Observed::load_pdu(Duration::ZERO, &payload, payload.len()).unwrap()
        })
        .collect();
    (
        witnesses,
        observed,
        Schedule {
            interval: Duration::from_millis(1),
            first_slot: Some(epoch + Duration::from_micros(micros[0])),
            reported_datagrams: 3,
            reported_bytes: 96,
            per_st: 0,
            skipped_bursts: 0,
        },
        Tolerance {
            domain: Duration::from_millis(20),
            ..Tolerance::EXACT
        },
    )
}

// Spec story 119 and ADR 0012: initial Skipped Bursts cannot conceal a
// later unexplained gap. Slots 0, 1 and 3 are absent from this wire trace.
#[test]
fn skipped_bursts_account_for_slots_from_the_known_schedule_start() {
    let (witnesses, observed, mut schedule, tolerance) = witnessed_plans_at([2_000, 4_000, 5_000]);
    schedule.first_slot = Some(witnesses.activation.wall.as_unix());
    schedule.skipped_bursts = 2;
    let failures = witnesses.check(&observed, schedule, tolerance);
    assert!(
        failures.iter().any(|reason| reason.contains("fewer than")),
        "{failures:#?}"
    );
    schedule.skipped_bursts = 3;
    let failures = witnesses.check(&observed, schedule, tolerance);
    assert!(failures.is_empty(), "{failures:#?}");
}

// Spec story 119: a late immediate STOP2 does not extend periodic coverage.
#[test]
fn a_delayed_stop2_keeps_its_wire_checks_without_extending_the_periodic_schedule() {
    let (witnesses, mut observed, schedule, tolerance) = witnessed_plans_at([0, 1_000, 5_000]);
    let failures = witnesses.check(&observed, schedule, tolerance);
    assert!(
        failures.iter().any(|reason| reason.contains("fewer than")),
        "{failures:#?}"
    );
    observed[2].header[2] = 2;
    let failures = witnesses.check(&observed, schedule, tolerance);
    assert!(failures.is_empty(), "{failures:#?}");
    let failures = witnesses.check(
        &observed,
        Schedule {
            reported_datagrams: 4,
            ..schedule
        },
        tolerance,
    );
    assert!(
        failures
            .iter()
            .any(|reason| reason.contains("reported sent")),
        "{failures:#?}"
    );
}

// Spec story 119: an immediate STOP2 can precede the next periodic slot.
#[test]
fn an_immediate_stop2_can_precede_the_next_periodic_slot() {
    let (witnesses, mut observed, schedule, tolerance) = witnessed_plans_at([0, 1_000, 1_500]);
    let failures = witnesses.check(&observed, schedule, tolerance);
    assert!(
        failures
            .iter()
            .any(|reason| reason.contains("before its slot")),
        "{failures:#?}"
    );
    observed[2].header[2] = 2;
    let failures = witnesses.check(&observed, schedule, tolerance);
    assert!(failures.is_empty(), "{failures:#?}");
}

// Spec story 119: STOP2 retains the independent clock-domain and monotonicity checks.
#[test]
fn stop2_keeps_the_timestamp_domain_and_order_checks() {
    let (witnesses, mut observed, schedule, tolerance) = witnessed_plans_at([0, 1_000, 5_000]);
    observed[2].header[2] = 2;
    let mut backwards = observed.clone();
    backwards[2].header[20..28].copy_from_slice(&observed[0].header[20..28]);
    let failures = witnesses.check(&backwards, schedule, tolerance);
    assert!(
        failures.iter().any(|reason| reason.contains("went back")),
        "{failures:#?}"
    );
    let payload = load_pdu(
        3,
        witnesses.activation.wall.as_unix() + Duration::from_secs(1),
    );
    observed[2] = Observed::load_pdu(Duration::ZERO, &payload, payload.len()).unwrap();
    observed[2].header[2] = 2;
    let failures = witnesses.check(&observed, schedule, tolerance);
    assert!(
        failures
            .iter()
            .any(|reason| reason.contains("clock reference")),
        "{failures:#?}"
    );
}

// The plan-executor contract (docs/spec.md story 119): a sender clock witness
// must still distinguish a wrong epoch or a clock offset above 20 ms.
#[test]
fn the_sender_clock_witness_rejects_wrong_timestamp_domains() {
    let (witnesses, observed, schedule, tolerance) = witnessed_plans();
    assert!(witnesses.check(&observed, schedule, tolerance).is_empty());
    for wrong in [
        Duration::from_millis(2),
        Duration::from_secs(1_700_000_000) + Duration::from_millis(27),
        Duration::from_secs(1_700_000_000) - Duration::from_millis(23),
    ] {
        let mut corrupted = observed.clone();
        corrupted[1].header = load_pdu(2, wrong);
        let failures = witnesses.check(&corrupted, schedule, tolerance);
        assert!(
            failures
                .iter()
                .any(|reason| reason.starts_with("Load PDU 2 stamped")),
            "wrong timestamp {wrong:?} escaped the clock-domain check: {failures:#?}"
        );
    }
    // The domain tolerance remains inclusive at 20 ms. Shifting only wire
    // stamps leaves an inconsistent same-domain schedule start.
    let boundary: Vec<_> = observed
        .iter()
        .map(|pdu| Observed {
            header: load_pdu(pdu.seq_no(), pdu.lpdu_time() + Duration::from_millis(20)),
            ..*pdu
        })
        .collect();
    let failures = witnesses.check(&boundary, schedule, tolerance);
    assert!(
        failures
            .iter()
            .all(|reason| !reason.starts_with("Load PDU ")),
        "{failures:#?}"
    );
    assert!(
        failures.iter().any(|reason| reason.contains("fewer than")),
        "{failures:#?}"
    );
    let shifted = Schedule {
        first_slot: schedule.first_slot.map(|at| at + Duration::from_millis(20)),
        ..schedule
    };
    let failures = witnesses.check(&boundary, shifted, tolerance);
    assert!(failures.is_empty(), "{failures:#?}");
}

// The wire remains authoritative for all of the contract's other checks.
#[test]
fn the_sender_clock_witness_preserves_order_spacing_and_sent_counts() {
    let (witnesses, observed, schedule, tolerance) = witnessed_plans();
    let mut backwards = observed.clone();
    backwards[1].header = load_pdu(2, Duration::from_secs(1_700_000_000));
    let mut early = observed.clone();
    early[0].header = load_pdu(1, Duration::from_secs(1_700_000_000));
    let mut sparse = observed.clone();
    sparse[2].header = load_pdu(
        3,
        Duration::from_secs(1_700_000_000) + Duration::from_millis(6),
    );
    let mut gap = observed.clone();
    gap.remove(1);
    for (wire, expected) in [
        (backwards, "lpduTime went back"),
        (early, "Send Plan 0 went"),
        (sparse, "3 Send Plans"),
        (gap, "lpduSeqNo 1 followed by 3"),
    ] {
        let failures = witnesses.check(&wire, schedule, tolerance);
        assert!(
            failures.iter().any(|reason| reason.starts_with(expected)),
            "{expected} escaped the wire contract: {failures:#?}"
        );
    }
    for (schedule, expected) in [
        (
            Schedule {
                reported_datagrams: 4,
                ..schedule
            },
            "4 Load PDUs reported sent",
        ),
        (
            Schedule {
                reported_bytes: 97,
                ..schedule
            },
            "97 bytes reported sent",
        ),
    ] {
        let failures = witnesses.check(&observed, schedule, tolerance);
        assert!(
            failures.iter().any(|reason| reason.starts_with(expected)),
            "{expected} escaped the wire contract: {failures:#?}"
        );
    }
}

// Missing or ambiguous stamping metadata cannot substitute the received
// lpduTime for an independent witness and silently satisfy the contract.
#[test]
fn every_received_load_pdu_requires_one_usable_sender_clock_witness() {
    let (mut witnesses, observed, schedule, tolerance) = witnessed_plans();
    witnesses.sent.remove(&2);
    assert_eq!(
        witnesses.check(&observed, schedule, tolerance),
        ["no unique sender clock witness for Load PDU 2"]
    );
    witnesses.record(&observed[1].header, None);
    assert_eq!(
        witnesses.check(&observed, schedule, tolerance),
        ["no unique sender clock witness for Load PDU 2"]
    );
    let (mut witnesses, observed, schedule, tolerance) = witnessed_plans();
    witnesses.record(
        &observed[1].header,
        Some(MonoTime::from_duration(Duration::from_millis(5002))),
    );
    assert_eq!(
        witnesses.check(&observed, schedule, tolerance),
        ["no unique sender clock witness for Load PDU 2"]
    );
}

impl<A: Adapter> Adapter for WitnessAdapter<A> {
    fn send(&mut self, datagram: &[u8], stamped: Option<MonoTime>) -> io::Result<usize> {
        let len = self.inner.send(datagram, stamped)?;
        if len == datagram.len() {
            self.witnesses.lock().unwrap().record(datagram, stamped);
        }
        Ok(len)
    }

    fn recv(&mut self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        self.inner.recv(buf)
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn offers(&self) -> socket::Offers {
        self.inner.offers()
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn send_batch(
        &mut self,
        buf: &[u8],
        messages: &[socket::batch::Message],
        segment: Option<usize>,
        stamped: Option<MonoTime>,
    ) -> io::Result<usize> {
        let sent = self.inner.send_batch(buf, messages, segment, stamped)?;
        let mut witnesses = self.witnesses.lock().unwrap();
        for message in &messages[..sent] {
            for datagram in buf[message.at..message.at + message.bytes].chunks(message.segment) {
                witnesses.record(datagram, stamped);
            }
        }
        Ok(sent)
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn read_batch(&mut self, max: usize) -> io::Result<usize> {
        self.inner.read_batch(max)
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn batch_datagram(&self, index: usize) -> (&[u8], socket::batch_read::Received) {
        self.inner.batch_datagram(index)
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn ppoll(&mut self, readable: bool, timeout: Duration) -> io::Result<socket::Woke> {
        self.inner.ppoll(readable, timeout)
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn drain_queue(&mut self, each: &mut impl FnMut(TransportError)) -> (bool, bool) {
        self.inner.drain_queue(each)
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn pending_error(&mut self) -> Option<io::Error> {
        self.inner.pending_error()
    }

    fn receive_buffer_overflows(&self) -> Option<u64> {
        self.inner.receive_buffer_overflows()
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn socket(&self) -> &UdpSocket {
        self.inner.socket()
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn overflow_count(&self) -> socket::OverflowCount {
        self.inner.overflow_count()
    }

    fn send_lag(&self) -> Option<socket::SendLag> {
        self.inner.send_lag()
    }
}

// The plan-executor contract (docs/spec.md story 119) judges the sender's
// timestamp domain. A receiver that is not scheduled cannot timestamp a
// datagram's arrival with a clock read after it eventually drains the socket.
#[test]
fn the_contract_allows_load_pdus_buffered_before_the_observer_runs() {
    let observer = UdpSocket::bind("127.0.0.1:0").unwrap();
    observer
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
    let peer = observer.local_addr().unwrap();
    sender.connect(peer).unwrap();
    let clock = Clock::new();
    let witnesses = Arc::new(Mutex::new(SendWitnesses::new(clock.now())));
    let mut sender = WitnessOpen {
        inner: socket::Platform,
        witnesses: witnesses.clone(),
    }
    .open(
        sender,
        &socket::Settings {
            peer,
            connected: true,
            load_receiver: false,
            rx_timestamps: false,
            send_stamps: false,
            explicit_numbers: false,
            clock,
        },
    );
    let stamped = clock.now();
    for seq in 1u32..=2 {
        let payload = load_pdu(seq, stamped.wall.as_unix());
        sender.send(&payload, Some(stamped.mono)).unwrap();
    }
    // Both sends completed before the observer was allowed to run. This
    // isolates receiver scheduling/buffering from sender pacing and clocks.
    std::thread::sleep(Duration::from_millis(40));
    let mut payload = [0u8; 32];
    let observed: Vec<_> = (0..2)
        .map(|_| {
            let len = observer.recv(&mut payload).unwrap();
            Observed::load_pdu(Duration::ZERO, &payload[..len], len).unwrap()
        })
        .collect();
    let failures = witnesses.lock().unwrap().check(
        &observed,
        Schedule {
            interval: Duration::from_millis(1),
            first_slot: Some(stamped.wall.as_unix()),
            reported_datagrams: 2,
            reported_bytes: 64,
            per_st: 0,
            skipped_bursts: 0,
        },
        Tolerance {
            domain: Duration::from_millis(20),
            ..Tolerance::EXACT
        },
    );
    assert!(failures.is_empty(), "{failures:#?}");
}

// The real engine sends Bursts every 1 ms (row 40: four Load PDUs a
// Burst) at their slots, in order, reports what it sent, and stamps them
// with the wall clock
#[udpstp_req::req("RFC9097-8.1-L2")]
#[test]
fn the_data_phase_engine_meets_the_contract() {
    let observer = UdpSocket::bind("127.0.0.1:0").unwrap();
    // Room for the whole run, so the observer loses nothing.
    let _ = socket2::SockRef::from(&observer).set_recv_buffer_size(8 << 20);
    observer
        .set_read_timeout(Some(Duration::from_millis(20)))
        .unwrap();
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let (client_addr, test_port) = (socket.local_addr().unwrap(), observer.local_addr().unwrap());
    let clock = Clock::new();
    let now = || clock.now();
    let (data_phase, _, activated_at) =
        contract::activate(now, Direction::Upstream, 40, client_addr, test_port);
    // Read the independent reference pair immediately after activation,
    // before the engine starts or any wire timestamp is observed.
    let activation = clock.now();
    let witnesses = Arc::new(Mutex::new(SendWitnesses::new(activation)));
    // Its first slot by the wall clock, as lpduTime reads it.
    let first_slot = data_phase
        .next_burst_at()
        .map(|at| at.as_duration() + activated_at.wall.as_unix() - activated_at.mono.as_duration());
    socket.connect(test_port).unwrap();
    let (report, mut reports) = crate::runtime::engine::reports::channel();
    let engine = engine::spawn_with(
        SocketId(1),
        data_phase,
        socket,
        true,
        crate::runtime::DEFAULT_SOCKET_BUFFER_SIZE,
        clock,
        EngineSettings::default(),
        report,
        WitnessOpen {
            inner: socket::Platform,
            witnesses: witnesses.clone(),
        },
    )
    .expect("the engine starts");
    let mut observed = Vec::new();
    let mut buf = [0u8; 2048];
    let started = Instant::now();
    let mut stopped = None;
    let (mut datagrams, mut bytes, mut per_st) = (0, 0, 0);
    let mut skipped_bursts = 0u64;
    let mut exit = None;
    let mut exited = false;
    while !exited && started.elapsed() < Duration::from_secs(5) {
        if let Ok(len) = observer.recv(&mut buf) {
            observed.extend(Observed::load_pdu(Duration::ZERO, &buf[..len], len));
        }
        // Stopped before the 1 s without Status PDUs would stop it.
        if stopped.is_none() && started.elapsed() > Duration::from_millis(600) {
            engine.stop();
            stopped = Some(Instant::now());
        }
        while let Ok(report) = reports.try_recv() {
            match report {
                FromEngine::Event {
                    event: DataEvent::Sent(sent),
                    ..
                } => {
                    datagrams += u64::from(sent.datagrams);
                    bytes += sent.bytes;
                    per_st = per_st.max(u64::from(sent.datagrams));
                }
                FromEngine::Event {
                    event: DataEvent::BurstsSkipped { bursts, .. },
                    ..
                } => skipped_bursts += u64::from(bursts),
                FromEngine::Exited {
                    reason,
                    error,
                    facts,
                    ..
                } => {
                    exit = Some((reason, error, facts));
                    exited = true;
                }
                FromEngine::Event { .. } => {}
            }
        }
    }
    assert!(exited, "the engine didn't exit");
    // What was sent last may still be on its way.
    while let Ok(len) = observer.recv(&mut buf) {
        observed.extend(Observed::load_pdu(Duration::ZERO, &buf[..len], len));
    }
    let schedule = Schedule {
        interval: Duration::from_millis(1),
        first_slot,
        reported_datagrams: datagrams,
        reported_bytes: bytes,
        per_st,
        skipped_bursts,
    };
    let tolerance = Tolerance {
        missing: 0.0,
        domain: Duration::from_millis(20),
        late: EngineSettings::default().catch_up_budget(),
    };
    let witnesses = witnesses.lock().unwrap();
    let failures = witnesses.check(&observed, schedule, tolerance);
    let stamps: Vec<_> = witnesses.sent.values().flatten().copied().collect();
    assert!(
        contract::periodic_plans(&observed).len() >= 2,
        "the engine produced fewer than two Send Plans"
    );
    let largest_sender_gap = stamps
        .windows(2)
        .map(|pair| pair[1].saturating_sub(pair[0]))
        .max();
    assert!(
        failures.is_empty(),
        "{failures:#?}\n{} Load PDUs observed, {skipped_bursts} Bursts skipped, largest sender gap {largest_sender_gap:?}, exit {exit:#?}",
        observed.len()
    );
}
