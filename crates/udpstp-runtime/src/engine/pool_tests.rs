//! The Linux Server's pooled executor on loopback (seam 2b): Tests
//! set up in memory (the plan-executor contract's `activate`) whose Data
//! Phases run on an [`EnginePool`], the Server's end or both ends, over
//! real sockets, the Clients' ends otherwise on the per-Test-Connection
//! executor. The pool's recipe is the fault adapter's (`socket::faults`),
//! which injects nothing when given no faults, and whose ppoll faults and
//! counters reach the pooled wait. The pure policy is `schedule.rs`'s; the
//! fault cases shared with the per-Test-Connection executor run on the
//! pool in `fault_tests.rs`.

use std::collections::BTreeMap;
use std::net::UdpSocket;
use std::time::{Duration, Instant, SystemTime};

use tokio::sync::mpsc;
use udpstp_core::{
    DataEvent, Direction, EndReason, Evaluation, IpVersion, MeasurementResults,
    PerformanceCriterion, SocketId,
};

use super::pool::EnginePool;
use super::{Engine, EngineFacts, EngineSettings, FromEngine};
use crate::DEFAULT_SOCKET_BUFFER_SIZE;
use crate::clock::Clock;
use crate::contract::{self, Observed, Schedule, Tolerance, activate, activate_lasting};
use crate::socket::faults::{Faults, Faulty};
use crate::socket::{FastPaths, Platform};

/// The recipe the pool's sockets open with.
pub(crate) type Recipe = Faulty<Platform>;

/// A recipe injecting the faults `spec` spells, as `UDPSTP_FAULTS` does.
pub(crate) fn recipe(spec: &str) -> Recipe {
    Faulty {
        inner: Platform,
        faults: Faults::parse(spec),
    }
}

/// How one end of a Test ran, as its engine reported.
#[derive(Debug, Default)]
pub(crate) struct End {
    pub(crate) events: Vec<DataEvent>,
    pub(crate) reason: Option<EndReason>,
    pub(crate) error: Option<String>,
    pub(crate) facts: EngineFacts,
    /// Its ends reported: exactly one, however it ended.
    pub(crate) exits: u32,
    /// When its end arrived.
    pub(crate) exited_at: Option<Instant>,
}

impl End {
    pub(crate) fn reported(&self, what: impl Fn(&DataEvent) -> bool) -> bool {
        self.events.iter().any(what)
    }

    /// The Load PDUs it counted in its Sub-Intervals: received, and lost.
    pub(crate) fn load_pdus(&self) -> (u64, u64) {
        self.sub_intervals()
            .fold((0, 0), |(received, lost), (r, l)| (received + r, lost + l))
    }

    /// Each Sub-Interval's Load PDUs, received and lost.
    pub(crate) fn sub_intervals(&self) -> impl Iterator<Item = (u64, u64)> + '_ {
        self.events.iter().filter_map(|event| match event {
            DataEvent::SubIntervalMeasured(measured) => {
                Some((u64::from(measured.datagrams), u64::from(measured.loss)))
            }
            _ => None,
        })
    }

    /// The Load PDUs it didn't send: in Skipped Bursts, and in Withheld
    /// ones.
    pub(crate) fn unsent(&self) -> (u64, u64) {
        self.events
            .iter()
            .fold((0, 0), |(skipped, withheld), event| match event {
                DataEvent::BurstsSkipped { datagrams, .. } => {
                    (skipped + u64::from(*datagrams), withheld)
                }
                DataEvent::BurstsWithheld { datagrams, .. } => {
                    (skipped, withheld + u64::from(*datagrams))
                }
                _ => (skipped, withheld),
            })
    }
}

/// Tests whose ends report through one channel: the Servers' ends on a
/// pool, the Clients' on it too or on threads of their own.
pub(crate) struct Harness {
    pub(crate) clock: Clock,
    report: mpsc::UnboundedSender<FromEngine>,
    reports: mpsc::UnboundedReceiver<FromEngine>,
    pub(crate) pool: EnginePool<Recipe>,
    pub(crate) ends: BTreeMap<u32, End>,
    /// Each end's engine, by its SocketId, kept until the harness goes:
    /// dropping one stops it.
    pub(crate) engines: BTreeMap<u32, Engine>,
    next: u32,
    /// How long each Test it starts lasts: its testIntTime, in seconds.
    seconds: u16,
}

/// Where a Test's Client runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Client {
    /// On a thread of its own, as the Client runs.
    Thread,
    /// On the pool too.
    Pool,
    /// Nowhere: its socket is the test's.
    Absent,
}

/// A Test started: its ends' SocketIds, and the Client's socket when the
/// test keeps it.
pub(crate) struct Started {
    pub(crate) client: u32,
    pub(crate) server: u32,
    pub(crate) client_socket: Option<UdpSocket>,
}

impl Harness {
    /// A harness whose pool may start `threads` Engine Threads, with
    /// `settings`, its recipe `prototype`.
    pub(crate) fn new(threads: usize, settings: EngineSettings, prototype: Recipe) -> Self {
        let clock = Clock::new();
        let (report, reports) = mpsc::unbounded_channel();
        let settings = EngineSettings {
            threads: std::num::NonZeroUsize::new(threads),
            ..settings
        };
        let (pool, allowed) = EnginePool::new(clock, settings, report.clone(), prototype);
        assert_eq!(allowed.unwrap(), threads);
        Self {
            clock,
            report,
            reports,
            pool,
            ends: BTreeMap::new(),
            engines: BTreeMap::new(),
            next: 1,
            seconds: 5,
        }
    }

    /// Its Tests lasting `seconds` each, rather than 5.
    pub(crate) fn lasting(self, seconds: u16) -> Self {
        Self { seconds, ..self }
    }

    /// Starts a Fixed-Rate Test at `row` in `direction` over loopback: its
    /// Server's end on the pool, its socket opening with `server`, and its
    /// Client where `client` says.
    pub(crate) fn test(
        &mut self,
        direction: Direction,
        row: u16,
        server: Recipe,
        client: Client,
    ) -> Started {
        let (client_id, server_id) = (self.next, self.next + 1);
        self.next += 2;
        let client_socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let server_socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let client_addr = client_socket.local_addr().unwrap();
        let test_port = server_socket.local_addr().unwrap();
        let clock = self.clock;
        let (client_dp, server_dp, _) = activate_lasting(
            || clock.now(),
            direction,
            row,
            self.seconds,
            client_addr,
            test_port,
        );
        // As the Server's Control Phase connects it on activation.
        server_socket.connect(client_addr).unwrap();
        let buffers = DEFAULT_SOCKET_BUFFER_SIZE;
        let engine = self
            .pool
            .start_with(
                SocketId(server_id),
                server_dp,
                server_socket,
                true,
                buffers,
                server,
            )
            .expect("the Server's end starts");
        self.engines.insert(server_id, engine);
        self.ends.insert(server_id, End::default());
        let mut kept = None;
        match client {
            Client::Thread => {
                let engine = super::spawn_with(
                    SocketId(client_id),
                    client_dp,
                    client_socket,
                    false,
                    buffers,
                    clock,
                    EngineSettings::default(),
                    self.report.clone(),
                    Platform,
                )
                .expect("the Client's end starts");
                self.engines.insert(client_id, engine);
                self.ends.insert(client_id, End::default());
            }
            Client::Pool => {
                let engine = self
                    .pool
                    .start_with(
                        SocketId(client_id),
                        client_dp,
                        client_socket,
                        false,
                        buffers,
                        recipe(""),
                    )
                    .expect("the Client's end starts");
                self.engines.insert(client_id, engine);
                self.ends.insert(client_id, End::default());
            }
            Client::Absent => {
                client_socket.connect(test_port).unwrap();
                kept = Some(client_socket);
            }
        }
        Started {
            client: client_id,
            server: server_id,
            client_socket: kept,
        }
    }

    /// Takes the reports waiting.
    pub(crate) fn collect(&mut self) {
        while let Ok(report) = self.reports.try_recv() {
            self.take([report]);
        }
    }

    /// Takes `reports`, taken from the channel already.
    fn take(&mut self, reports: impl IntoIterator<Item = FromEngine>) {
        for report in reports {
            match report {
                FromEngine::Event { socket, event } => {
                    self.ends.entry(socket.0).or_default().events.push(event);
                }
                FromEngine::Exited {
                    socket,
                    reason,
                    error,
                    facts,
                } => {
                    let end = self.ends.entry(socket.0).or_default();
                    end.reason = reason;
                    end.error = error;
                    end.facts = facts;
                    end.exits += 1;
                    end.exited_at.get_or_insert_with(Instant::now);
                }
            }
        }
    }

    /// Takes reports until every end has reported its end, or `limit`
    /// passes: whether they all did.
    pub(crate) fn run(&mut self, limit: Duration) -> bool {
        self.run_until(limit, |harness| {
            harness.ends.values().all(|end| end.exits > 0)
        })
    }

    /// Takes reports until `done`, or `limit` passes: whether `done`.
    pub(crate) fn run_until(&mut self, limit: Duration, done: impl Fn(&Self) -> bool) -> bool {
        let until = Instant::now() + limit;
        loop {
            self.collect();
            if done(self) {
                return true;
            }
            if Instant::now() >= until {
                return false;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    pub(crate) fn end(&self, id: u32) -> &End {
        &self.ends[&id]
    }
}

/// A Test completed at both ends without loss: the Client with the STOP
/// exchange or its own Test Interval timer, the Server by any end but a
/// failure, and no Sub-Interval of the Load Receiver's lost a Load PDU.
pub(crate) fn completed_without_loss(harness: &Harness, started: &Started, direction: Direction) {
    let client = harness.end(started.client);
    let server = harness.end(started.server);
    assert!(
        matches!(
            (client.reason, &client.error),
            (
                Some(EndReason::Completed | EndReason::TestIntervalElapsed),
                None
            )
        ),
        "the Client's end: {:?} {:?}",
        client.reason,
        client.error
    );
    assert!(
        matches!((server.reason, &server.error), (Some(_), None)),
        "the Server's end: {:?} {:?}",
        server.reason,
        server.error
    );
    let receiver = match direction {
        Direction::Downstream => client,
        Direction::Upstream => server,
    };
    let losses: Vec<u64> = receiver.sub_intervals().map(|(_, lost)| lost).collect();
    assert!(
        !losses.is_empty() && losses.iter().all(|&lost| lost == 0),
        "no Sub-Interval lost a Load PDU: {losses:?}"
    );
}

/// The fast paths a pooled Load Sender uses on loopback, where the kernel
/// offers them all.
const POOLED_SENDING: FastPaths = FastPaths {
    sendmmsg: true,
    udp_gso: true,
    recvmmsg: true,
    kernel_rx_timestamps: true,
    ppoll: false,
    epoll: true,
};

fn unix_time() -> Duration {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
}

/// Runs `count` Upstream Load Senders of row 40 on one Engine Thread, each
/// sending to a plain socket that records what arrives, and checks each
/// against the plan-executor contract, as the per-Test-Connection
/// executor's run is (`contract_tests.rs`).
fn meets_the_contract(count: usize) {
    let mut harness = Harness::new(1, EngineSettings::default(), recipe(""));
    let mut runs = Vec::new();
    for index in 0..count {
        let observer = UdpSocket::bind("127.0.0.1:0").unwrap();
        // Room for the whole run, so the observer loses nothing.
        let _ = socket2::SockRef::from(&observer).set_recv_buffer_size(8 << 20);
        observer
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let (client_addr, test_port) =
            (socket.local_addr().unwrap(), observer.local_addr().unwrap());
        let clock = harness.clock;
        let (data_phase, _, activated_at) = activate(
            || clock.now(),
            Direction::Upstream,
            40,
            client_addr,
            test_port,
        );
        let first_slot = data_phase.next_burst_at().map(|at| {
            at.as_duration() + activated_at.wall.as_unix() - activated_at.mono.as_duration()
        });
        socket.connect(test_port).unwrap();
        runs.push((1 + index as u32, observer, first_slot, data_phase, socket));
    }
    let observed: Vec<(u32, Vec<Observed>, Option<Duration>)> = std::thread::scope(|scope| {
        // Each observer reads before its Load Sender starts, so what it
        // records is timed as it arrives.
        let mut starting = Vec::new();
        let observers: Vec<_> = runs
            .into_iter()
            .map(|(id, observer, first_slot, data_phase, socket)| {
                starting.push((id, data_phase, socket));
                scope.spawn(move || {
                    let mut observed = Vec::new();
                    let mut buf = [0u8; 2048];
                    let started = Instant::now();
                    while started.elapsed() < Duration::from_millis(1_500) {
                        if let Ok(len) = observer.recv(&mut buf) {
                            observed.extend(Observed::load_pdu(unix_time(), &buf[..len], len));
                        }
                    }
                    (id, observed, first_slot)
                })
            })
            .collect();
        for (id, data_phase, socket) in starting {
            let engine = harness
                .pool
                .start_with(
                    SocketId(id),
                    data_phase,
                    socket,
                    true,
                    DEFAULT_SOCKET_BUFFER_SIZE,
                    recipe(""),
                )
                .expect("the engine starts");
            harness.engines.insert(id, engine);
            harness.ends.insert(id, End::default());
        }
        assert_eq!(harness.pool.started(), 1, "on one Engine Thread");
        // Stopped before the 1 s without Status PDUs would stop them.
        std::thread::sleep(Duration::from_millis(600));
        for engine in harness.engines.values() {
            engine.stop();
        }
        assert!(harness.run(Duration::from_secs(5)), "every engine ended");
        observers
            .into_iter()
            .map(|observer| observer.join().unwrap())
            .collect()
    });
    for (id, observed, first_slot) in observed {
        let end = harness.end(id);
        let (mut datagrams, mut bytes, mut per_st) = (0, 0, 0);
        let mut skipped_bursts = 0u64;
        for event in &end.events {
            match event {
                DataEvent::Sent(sent) => {
                    datagrams += u64::from(sent.datagrams);
                    bytes += sent.bytes;
                    per_st = per_st.max(u64::from(sent.datagrams));
                }
                DataEvent::BurstsSkipped { bursts, .. } => skipped_bursts += u64::from(*bursts),
                _ => {}
            }
        }
        assert!(
            observed
                .windows(2)
                .any(|pair| pair[1].lpdu_time() > pair[0].lpdu_time()),
            "Test Connection {id}: fewer than two Send Plans"
        );
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
        let failures = contract::check(&observed, schedule, tolerance);
        assert!(
            failures.is_empty(),
            "Test Connection {id}: {failures:#?}\n{} Load PDUs observed",
            observed.len()
        );
        assert_eq!(end.exits, 1, "Test Connection {id}: one end");
        assert!(end.facts.fast_paths.epoll, "{:?}", end.facts.fast_paths);
    }
}

// The pooled executor meets the plan-executor
// contract with one Test Connection: Bursts every 1 ms (row 40) at their
// slots, in order, reported as sent, stamped with the wall clock.
#[udpstp_req::req("RFC9097-8.1-L2")]
#[test]
fn the_pooled_executor_meets_the_contract() {
    meets_the_contract(1);
}

// So does each of 32 Test Connections of row 40 sharing
// one Engine Thread.
#[test]
fn thirty_two_test_connections_on_one_engine_thread_each_meet_the_contract() {
    meets_the_contract(32);
}

//  (design §2.1, D9): Engine Threads start lazily, one per Test
// Connection until as many as allowed serve one each; then each new one
// goes to the Engine Thread serving the fewest. Every Test completes
// without loss, and each Server's end reports the fast path `epoll`, which
// the re-check's tooling reads to know the pooled executor ran (review
// M7), and not `ppoll`.
#[test]
fn tests_spread_over_lazily_started_engine_threads_and_complete() {
    let mut harness = Harness::new(2, EngineSettings::default(), recipe(""));
    assert_eq!(harness.pool.max(), 2);
    assert_eq!(harness.pool.started(), 0, "none before the first");
    let mut tests = Vec::new();
    for (index, direction) in [
        Direction::Downstream,
        Direction::Upstream,
        Direction::Downstream,
    ]
    .into_iter()
    .enumerate()
    {
        tests.push((
            harness.test(direction, 25, recipe(""), Client::Thread),
            direction,
        ));
        assert_eq!(harness.pool.started(), (index + 1).min(2));
    }
    assert!(harness.run(Duration::from_secs(30)), "every end reported");
    for (started, direction) in &tests {
        completed_without_loss(&harness, started, *direction);
        let server = harness.end(started.server);
        assert_eq!(server.exits, 1);
        let paths = server.facts.fast_paths;
        assert!(paths.epoll && !paths.ppoll, "{direction:?}: {paths:?}");
        if *direction == Direction::Downstream {
            assert_eq!(paths, POOLED_SENDING, "{direction:?}");
        }
        assert!(
            server.facts.socket_buffers.is_some(),
            "the socket buffers it got"
        );
    }
}

// docs/spec.md (the engine must release admission state on every exit path,
// including a panic; ADR 0003),  (design §4; the  prototype's F1):
// a panic in one of 8 Test Connections sharing an Engine Thread retires
// that one only, reporting its end once, which is what releases its
// Admission; the other 7 complete without loss.
#[test]
fn a_panic_in_one_of_eight_test_connections_on_an_engine_thread_ends_it_alone() {
    let mut harness = Harness::new(1, EngineSettings::default(), recipe(""));
    let mut tests = Vec::new();
    for index in 0..8 {
        let server = if index == 3 {
            recipe("panic_after=20")
        } else {
            recipe("")
        };
        tests.push(harness.test(Direction::Downstream, 5, server, Client::Thread));
    }
    assert!(harness.run(Duration::from_secs(30)), "every end reported");
    for (index, started) in tests.iter().enumerate() {
        let server = harness.end(started.server);
        assert_eq!(server.exits, 1, "Test {index}: one end");
        if index == 3 {
            assert_eq!(server.reason, None, "the one that panicked");
            assert_eq!(server.error.as_deref(), Some("the Data Phase panicked"));
            assert!(server.facts.socket_buffers.is_some());
        } else {
            completed_without_loss(&harness, started, Direction::Downstream);
        }
    }
}

// docs/spec.md, ADR 0003,  (design §4): a socket that panics as it opens
// on an Engine Thread that serves another Test Connection has its end
// reported, with the socket buffers it got; the other completes.
#[test]
fn a_panic_as_a_shared_engine_thread_opens_a_socket_ends_that_one_alone() {
    let mut harness = Harness::new(1, EngineSettings::default(), recipe(""));
    let running = harness.test(Direction::Downstream, 5, recipe(""), Client::Thread);
    std::thread::sleep(Duration::from_millis(300));
    let panicking = harness.test(
        Direction::Downstream,
        5,
        recipe("panic_on_open=1"),
        Client::Absent,
    );
    assert!(harness.run(Duration::from_secs(30)), "every end reported");
    let failed = harness.end(panicking.server);
    assert_eq!(failed.exits, 1);
    assert_eq!(failed.error.as_deref(), Some("the Data Phase panicked"));
    assert!(failed.facts.socket_buffers.is_some());
    assert_eq!(failed.facts.fast_paths, FastPaths::default());
    completed_without_loss(&harness, &running, Direction::Downstream);
}

// docs/spec.md, ADR 0003,  (design §4, every handover reports its end):
// an Engine Thread that panics in its own loop, outside every Test
// Connection's work, reports the end of each Test Connection it served and
// of each handed to it and not yet taken, once each. A handover sent after
// it died goes to another Engine Thread, and so does the next Test
// Connection, which completes. (Which of the four it had taken when it
// panicked is the scheduler's: at least the two it opened; one that went
// to another Engine Thread ends there, its Client silent, by the
// Watchdog.)
#[test]
fn a_panic_in_an_engine_threads_own_loop_reports_every_end() {
    let prototype = recipe("engine_thread_panic_after_opens=2");
    let mut harness = Harness::new(1, EngineSettings::default(), prototype);
    let lost: Vec<Started> = (0..4)
        .map(|_| harness.test(Direction::Downstream, 5, recipe(""), Client::Absent))
        .collect();
    let every_lost_end = |harness: &Harness| {
        lost.iter()
            .all(|started| harness.end(started.server).exits > 0)
    };
    assert!(
        harness.run_until(Duration::from_secs(10), every_lost_end),
        "every end reported"
    );
    let mut failed = 0;
    for started in &lost {
        let end = harness.end(started.server);
        assert_eq!(end.exits, 1, "one end each");
        if end.reason.is_none() {
            assert!(end.error.is_some());
            failed += 1;
        }
    }
    assert!(failed >= 2, "{failed} failed with their Engine Thread");
    // The dead Engine Thread takes no more; another does.
    let next = harness.test(Direction::Downstream, 5, recipe(""), Client::Thread);
    assert!(harness.pool.started() >= 2, "another Engine Thread started");
    assert!(harness.run(Duration::from_secs(30)));
    completed_without_loss(&harness, &next, Direction::Downstream);
    for started in &lost {
        assert_eq!(harness.end(started.server).exits, 1, "still one end each");
    }
}

//  (design §3.3, fairness): a peer flooding its Test Connection's
// socket as fast as it can still allows another Test Connection on its
// Engine Thread to send Load PDUs and complete without loss. The scripted
// turn test pins the receive budget and sends-before-reads ordering. A
// shared host can legitimately skip overdue Bursts (ADR 0012), so this
// loopback check does not impose a pacing-precision threshold.
#[test]
fn a_flooded_socket_allows_another_test_connection_to_complete() {
    let mut harness = Harness::new(1, EngineSettings::default(), recipe(""));
    let sender = harness.test(Direction::Downstream, 5, recipe(""), Client::Thread);
    let flooded = harness.test(Direction::Upstream, 5, recipe(""), Client::Absent);
    let flooder = flooded.client_socket.expect("the flooding peer's socket");
    flooder.set_nonblocking(true).unwrap();
    let stop = std::sync::atomic::AtomicBool::new(false);
    let (completed, flood_sent) = std::thread::scope(|scope| {
        let flood = scope.spawn(|| {
            let datagram = [0x5Au8; 1_000];
            let mut sent = 0u64;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                if flooder.send(&datagram).is_ok() {
                    sent += 1;
                }
            }
            sent
        });
        let done = |harness: &Harness| harness.end(sender.client).exits > 0;
        let completed = harness.run_until(Duration::from_secs(30), done);
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        (completed, flood.join().unwrap())
    });
    assert!(completed, "the Client completed while its peer was flooded");
    assert!(flood_sent > 0, "the peer sent its flood");
    assert!(harness.run_until(Duration::from_secs(10), |harness| {
        harness.end(sender.server).exits > 0
    }));
    completed_without_loss(&harness, &sender, Direction::Downstream);
    let (received, _) = harness.end(sender.client).load_pdus();
    assert!(received > 0, "the other Test Connection carried Load PDUs");
}

//  (design §7, a saturated Engine Thread): an Engine Thread whose
// every send takes longer than its schedule allows runs its turns back to
// back, never waiting; it still reads Status PDUs (no Load Sender's
// traffic stops for their silence), takes a new Test Connection, and
// lands a stop at once.
#[test]
fn a_saturated_engine_thread_still_reads_takes_new_test_connections_and_stops() {
    let mut harness = Harness::new(1, EngineSettings::default(), recipe(""));
    // Four Load Senders of row 5, a Burst every 1 ms each, each send 400 µs.
    let slow = || recipe("slow_send_us=400");
    let saturating: Vec<Started> = (0..4)
        .map(|_| harness.test(Direction::Downstream, 5, slow(), Client::Thread))
        .collect();
    std::thread::sleep(Duration::from_millis(1_500));
    let late = harness.test(Direction::Downstream, 5, slow(), Client::Thread);
    let stopped = &saturating[0];
    let stop_at = Instant::now();
    harness.engines[&stopped.server].stop();
    assert!(harness.run_until(Duration::from_secs(2), |harness| {
        harness.end(stopped.server).exits > 0
    }));
    let took = harness.end(stopped.server).exited_at.unwrap() - stop_at;
    assert!(took < Duration::from_millis(100), "the stop took {took:?}");
    assert_eq!(harness.end(stopped.server).reason, Some(EndReason::Stopped));
    assert!(harness.run(Duration::from_secs(30)));
    for started in saturating.iter().skip(1).chain([&late]) {
        let server = harness.end(started.server);
        assert!(
            server.reported(|e| matches!(e, DataEvent::FirstStatusPduReceived)),
            "Status PDUs were read"
        );
        assert!(
            !server.reported(|e| matches!(e, DataEvent::TrafficStopped { .. })),
            "no Load Sender's traffic stopped for silence"
        );
        assert!(
            server.reported(|e| matches!(e, DataEvent::BurstsSkipped { .. })),
            "saturated: it skipped Bursts"
        );
    }
    assert!(
        harness
            .end(late.server)
            .reported(|e| matches!(e, DataEvent::Sent(_))),
        "the new Test Connection ran"
    );
}

//  (design §4, stop): a stop reaches a waiting Engine Thread at once,
// through its eventfd, where the per-Test-Connection executor sees its flag
// within 10 ms: a Server's Load Receiver whose Client sends nothing has no
// Status PDU due (the first comes a Trial Interval after the first Load
// PDU), so it waits for its 1 s receive timeout, and ends within a few ms
// of its stop.
#[test]
fn a_stop_reaches_a_waiting_engine_thread_within_one_wait() {
    let mut harness = Harness::new(1, EngineSettings::default(), recipe(""));
    let quiet = harness.test(Direction::Upstream, 5, recipe(""), Client::Absent);
    std::thread::sleep(Duration::from_millis(200));
    let mut took = Vec::new();
    let stop_at = Instant::now();
    harness.engines[&quiet.server].stop();
    assert!(harness.run_until(Duration::from_secs(2), |harness| {
        harness.end(quiet.server).exits > 0
    }));
    took.push(harness.end(quiet.server).exited_at.unwrap() - stop_at);
    assert_eq!(harness.end(quiet.server).reason, Some(EndReason::Stopped));
    assert!(
        took[0] < Duration::from_millis(20),
        "the stop took {took:?}"
    );
}

//  (design §3.6, §7): a measurement run by hand, which gates nothing:
// a fast Load Receiver sharing an Engine Thread with Load Senders, and its
// Receive Buffer Overflows, which it asserts are none. At row 1000 and
// 2 MiB buffers its share of the hold-off is several ms, so the hold-off
// stays 250 µs: this shows that a 250 µs hold-off and short sends leave a
// 1 Gbit/s Load Receiver whole, not that the hold-off is cut to a buffer
// that holds little; the share counts payload bytes, which can't see how
// much more loopback and veth charge a GSO datagram's memory
//, so it wouldn't be cut there
// either. In an unoptimized build above row 1000, a Load Receiver alone on
// either executor can't read as fast as loopback delivers and overflows
// (testhost, row 1036: about 7 % on both). `UDPSTP_MIXED_ROW` sets the
// row. Run it optimized and alone, under the host's measurement lock:
//
//     cargo test --release -p udpstp-runtime --lib a_fast_load_receiver -- --ignored --nocapture
#[test]
#[ignore = "a measurement run by hand, optimized and alone on the host: see its comment"]
fn a_fast_load_receiver_sharing_an_engine_thread_with_load_senders_overflows_nothing() {
    let row: u16 = std::env::var("UDPSTP_MIXED_ROW")
        .ok()
        .and_then(|row| row.parse().ok())
        .unwrap_or(1000);
    let mut harness = Harness::new(1, EngineSettings::default(), recipe(""));
    let receiver = harness.test(Direction::Upstream, row, recipe(""), Client::Thread);
    let senders: Vec<Started> = (0..4)
        .map(|_| harness.test(Direction::Downstream, 5, recipe(""), Client::Thread))
        .collect();
    assert!(harness.run(Duration::from_secs(30)));
    let end = harness.end(receiver.server);
    let (received, lost) = end.load_pdus();
    eprintln!(
        "row {row}: {received} Load PDUs received, {lost} lost, Receive Buffer Overflows {:?}",
        end.facts.receive_buffer_overflows
    );
    assert_eq!(end.facts.receive_buffer_overflows, Some(0));
    for started in &senders {
        completed_without_loss(&harness, started, Direction::Downstream);
    }
}

//  (design §3.4): where the kernel refuses ppoll, an Engine Thread
// sleeps until its next deadline in slices of at most 1 ms and harvests
// after each: Tests complete without loss both ways, their sockets still
// in its epoll set.
#[test]
fn an_engine_thread_sleeps_and_harvests_when_ppoll_is_refused() {
    let prototype = recipe("ppoll_unsupported=1");
    let polls = prototype.faults.clone();
    let mut harness = Harness::new(1, EngineSettings::default(), prototype);
    let tests: Vec<(Started, Direction)> = [Direction::Downstream, Direction::Upstream]
        .into_iter()
        .map(|direction| {
            (
                harness.test(direction, 25, recipe(""), Client::Thread),
                direction,
            )
        })
        .collect();
    assert!(harness.run(Duration::from_secs(30)));
    for (started, direction) in &tests {
        completed_without_loss(&harness, started, *direction);
        assert!(harness.end(started.server).facts.fast_paths.epoll);
    }
    assert_eq!(polls.ppoll_calls(), 1, "refused once, then asleep");
}

// , : an epoll set that stays readable with nothing to harvest
// makes every ppoll call return at once; the Engine Thread's spin guard
// backs it off to short sleeps rather than spinning, and its Tests
// complete without loss. It made a few thousand waits a second, where a
// spinning one makes hundreds of thousands.
#[test]
fn a_stuck_epoll_set_does_not_spin_the_engine_thread() {
    let prototype = recipe("pollerr_stuck=1");
    let counted = prototype.faults.clone();
    let mut harness = Harness::new(1, EngineSettings::default(), prototype);
    let started_at = Instant::now();
    let tests: Vec<(Started, Direction)> = [Direction::Downstream, Direction::Upstream]
        .into_iter()
        .map(|direction| {
            (
                harness.test(direction, 25, recipe(""), Client::Thread),
                direction,
            )
        })
        .collect();
    assert!(harness.run(Duration::from_secs(30)));
    let seconds = started_at.elapsed().as_secs_f64();
    for (started, direction) in &tests {
        completed_without_loss(&harness, started, *direction);
    }
    let polls = counted.ppoll_calls() as f64 / seconds;
    eprintln!("{polls:.0} ppoll calls a second");
    assert!(polls >= 1.0, "the stuck set was waited on");
    assert!(polls < 20_000.0, "{polls:.0} ppoll calls a second");
}

// ADR 0008 (no allocation in the steady state),  (D11): 8 Tests, both
// ends on one Engine Thread, run 1.8 s of their steady state, past their
// first Sub-Interval, allocating nothing in the whole process, counted by
// a counting global allocator (`stats_alloc`, test builds only): none at
// all. It runs alone, in a process of its own (the process's allocations
// are counted, and the harness's other tests allocate): this spawns it.
#[test]
fn the_pooled_executor_allocates_nothing_in_the_steady_state() {
    const INNER: &str = "engine::pool_tests::steady_state_allocations";
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", INNER, "--nocapture", "--test-threads=1"])
        .env(STEADY_STATE, "1")
        .output()
        .expect("the test binary runs");
    let out = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && out.contains("1 passed"),
        "{out}{err}"
    );
}

/// Makes this thread's Tokio parker, which Tokio makes on a thread's first
/// use and keeps: `try_recv` makes it when it finds a sender part-way
/// through a send, a race that load makes likely. Made in the counted
/// window, it was the test's own allocation, of 40 bytes, not the Engine
/// Thread's: under the whole suite's load, or with 10 busy loops on
/// testhost's 8 logical CPUs, where about half the runs counted it, a
/// tracing allocator showed its stack. With it made beforehand,
/// 20 such runs counted nothing.
fn park_this_thread_once() {
    let (send, mut receive) = mpsc::unbounded_channel::<()>();
    send.send(()).expect("the receiver is here");
    receive.blocking_recv().expect("the value sent");
}

/// Set for [`steady_state_allocations`] to run: in its own process.
const STEADY_STATE: &str = "UDPSTP_TEST_STEADY_STATE";

#[test]
fn steady_state_allocations() {
    if std::env::var_os(STEADY_STATE).is_none() {
        return;
    }
    let mut harness = Harness::new(1, EngineSettings::default(), recipe(""));
    let tests: Vec<Started> = (0..8)
        .map(|_| harness.test(Direction::Downstream, 40, recipe(""), Client::Pool))
        .collect();
    // Past the first Bursts, Status PDUs and Sub-Intervals, whose buffers
    // grow once, the first at 1 s.
    std::thread::sleep(Duration::from_millis(2_200));
    harness.collect();
    // The reports are taken as they come, so the channel reuses its
    // blocks, into room made beforehand.
    let mut taken = Vec::with_capacity(1 << 16);
    park_this_thread_once();
    let region = stats_alloc::Region::new(crate::ALLOCATOR);
    let until = Instant::now() + Duration::from_millis(1_800);
    while Instant::now() < until {
        while taken.len() < taken.capacity()
            && let Ok(report) = harness.reports.try_recv()
        {
            taken.push(report);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let change = region.change();
    harness.take(taken);
    assert!(harness.run(Duration::from_secs(30)));
    for started in &tests {
        completed_without_loss(&harness, started, Direction::Downstream);
    }
    eprintln!("in the steady state: {change:?}");
    assert!(
        change.allocations == 0 && change.reallocations == 0,
        "allocated in the steady state: {change:?}"
    );
}

//  (D5 (i), design §7): the shared-Engine-Thread precision proxy that
// Diagnostic loopback proxy.
// pre-registers, run by hand, which gates nothing: Downstream Tests from
// one Server at once over loopback, on the pooled executor with 1 and 2
// Engine Threads, with and without wake-up grouping, each pair of windows
// back to back, the first of a pair taking turns. Its parameters come from
// the environment, each defaulting to the pre-registration's fixed value
// or its first candidate; the feasibility run sets
// `UDPSTP_PROXY_THREADS=1`, `UDPSTP_PROXY_GROUPING_US=0` and
// `UDPSTP_PROXY_RUNS=3`:
//
// - `UDPSTP_PROXY_TESTS`: the Tests at once (8);
// - `UDPSTP_PROXY_ROW`: each one's row, at most 1000 (125);
// - `UDPSTP_PROXY_SECONDS`: each Test's length, its testIntTime (10);
// - `UDPSTP_PROXY_THREADS`: the pooled executor's Engine Threads, comma-
//   separated (1,2); 0 runs each Server's end on a thread of its own, the
//   per-Test-Connection executor, for comparison only;
// - `UDPSTP_PROXY_GROUPING_US`: the grouping windows, comma-separated
//   (0,50);
// - `UDPSTP_PROXY_RUNS`: the rounds of all of them (5).
//
// Each run prints one line, `PROXY` and tab-separated `key=value` fields,
// from which the pre-registration's reading takes one value a run:
//
// - `run`, `executor` (`pool` or `per-test-connection`), `engine_threads`,
//   `grouping_us`, `pair_position` (1 or 2), `tests`, `row`, `seconds`;
// - `completed`: `yes` where every Test completed at both ends; a run
//   whose Tests didn't gives the reading no figures;
// - `planned`: the Load PDUs the Tests were to send;
// - `skipped`, `withheld`, `not_sent` (their sum) and `not_sent_pct` (of
//   `planned`): the Server's Load PDUs in Skipped and in Withheld Bursts;
// - `shortfall_median_pct`: the median Sub-Interval's shortfall against
//   the plan, over every Test's Sub-Intervals, as P2c takes it
//   ([`p2c_shortfalls`]);
// - `lateness_p99_us`: the worst Test Connection's Burst lateness p99, the
//   time each Send Plan was written less its Burst's scheduled time;
// - `wakeups_per_s`: the Engine Threads' wake-ups a second of the Test,
//   all of them together: their voluntary context switches, as Linux's
//   /proc counts them and the home lab's samplers read them; `-` for a
//   thread per Test Connection, whose threads' names don't tell the
//   Server's ends apart;
// - `waits_per_s`: the Servers' ppoll calls a second of the Test;
// - `cpu_pct`: each Engine Thread's CPU time as a share of the Test's
//   length, comma-separated;
// - `feasible`: `yes` where the Server skipped no Burst and no Engine
//   Thread's CPU time exceeded 70 % of the Test's, the feasibility run's
//   rule; `-` for a thread per Test Connection.
//
// Run it optimized and alone on the host, under its measurement lock:
//
//     cargo test --release -p udpstp-runtime --lib shared_engine_thread_precision -- --ignored --nocapture
#[test]
#[ignore = "a measurement run by hand, optimized and alone on the host: see its comment"]
fn shared_engine_thread_precision() {
    let number = |name: &str, default: &str| -> Vec<u64> {
        std::env::var(name)
            .unwrap_or_else(|_| default.into())
            .split(',')
            .map(|value| value.trim().parse().expect(name))
            .collect()
    };
    let tests = number("UDPSTP_PROXY_TESTS", "8")[0] as usize;
    let row = u16::try_from(number("UDPSTP_PROXY_ROW", "125")[0]).expect("a row");
    let seconds = u16::try_from(number("UDPSTP_PROXY_SECONDS", "10")[0]).expect("seconds");
    let threads = number("UDPSTP_PROXY_THREADS", "1,2");
    let groupings = number("UDPSTP_PROXY_GROUPING_US", "0,50");
    let runs = number("UDPSTP_PROXY_RUNS", "5")[0];
    for run in 1..=runs {
        for &threads in &threads {
            // Each pair back to back, the first of a pair taking turns.
            let mut order = groupings.clone();
            if run % 2 == 0 {
                order.reverse();
            }
            for (position, &grouping) in order.iter().enumerate() {
                if threads == 0 && grouping > 0 {
                    continue;
                }
                let settings = EngineSettings {
                    grouping: Duration::from_micros(grouping),
                    ..EngineSettings::default()
                };
                let figures = proxy_run(tests, row, seconds, threads as usize, settings);
                let executor = if threads == 0 {
                    "per-test-connection"
                } else {
                    "pool"
                };
                println!(
                    "PROXY\trun={run}\texecutor={executor}\tengine_threads={threads}\t\
                     grouping_us={grouping}\tpair_position={}\ttests={tests}\trow={row}\t\
                     seconds={seconds}\t{}",
                    position + 1,
                    figures.fields(threads > 0)
                );
            }
        }
    }
}

/// What one run of the precision proxy gave.
#[derive(Debug)]
struct ProxyFigures {
    completed: bool,
    planned: u64,
    skipped: u64,
    withheld: u64,
    shortfall_median_pct: f64,
    lateness_p99: Duration,
    /// The Test's length.
    seconds: f64,
    /// The Servers' ppoll calls.
    waits: u64,
    engine_threads: Vec<ThreadUsage>,
}

impl ProxyFigures {
    /// Its tab-separated `key=value` fields; `pooled` where the Server ran
    /// on the pooled executor, whose Engine Threads it counted.
    fn fields(&self, pooled: bool) -> String {
        let not_sent = self.skipped + self.withheld;
        let per_second = |count: u64| format!("{:.0}", count as f64 / self.seconds);
        let shares: Vec<f64> = self
            .engine_threads
            .iter()
            .map(|thread| 100.0 * thread.cpu.as_secs_f64() / self.seconds)
            .collect();
        let (wake_ups, cpu, feasible) = if pooled {
            let feasible = self.skipped == 0
                && !shares.is_empty()
                && shares.iter().all(|&share| share <= FEASIBLE_CPU_PCT);
            (
                per_second(self.engine_threads.iter().map(|t| t.wake_ups).sum()),
                shares
                    .iter()
                    .map(|share| format!("{share:.1}"))
                    .collect::<Vec<_>>()
                    .join(","),
                if feasible { "yes" } else { "no" }.to_owned(),
            )
        } else {
            ("-".into(), "-".into(), "-".into())
        };
        format!(
            "completed={}\tplanned={}\tskipped={}\twithheld={}\tnot_sent={not_sent}\t\
             not_sent_pct={:.4}\tshortfall_median_pct={:.3}\tlateness_p99_us={}\t\
             wakeups_per_s={wake_ups}\twaits_per_s={}\tcpu_pct={cpu}\tfeasible={feasible}",
            if self.completed { "yes" } else { "no" },
            self.planned,
            self.skipped,
            self.withheld,
            100.0 * not_sent as f64 / self.planned.max(1) as f64,
            self.shortfall_median_pct,
            self.lateness_p99.as_micros(),
            per_second(self.waits),
        )
    }
}

/// The feasibility run's limit on each Engine Thread's CPU time, as a
/// share of the Test's length, in %.
const FEASIBLE_CPU_PCT: f64 = 70.0;

/// One run of the precision proxy: `tests` Downstream Tests of `row`, each
/// `seconds` long, their Servers' ends on a pool of `threads` Engine
/// Threads, or on a thread each where `threads` is 0.
fn proxy_run(
    tests: usize,
    row: u16,
    seconds: u16,
    threads: usize,
    settings: EngineSettings,
) -> ProxyFigures {
    // Counts the Servers' waits: the Engine Threads', or each thread's.
    let counted = recipe("");
    let mut harness = Harness::new(threads.max(1), settings, counted.clone()).lasting(seconds);
    let started: Vec<Started> = (0..tests)
        .map(|_| {
            if threads == 0 {
                harness.per_test_connection_test(Direction::Downstream, row, counted.clone())
            } else {
                harness.test(Direction::Downstream, row, recipe(""), Client::Thread)
            }
        })
        .collect();
    let ended = harness.run(Duration::from_secs(u64::from(seconds) + 30));
    // Read before the pool, and its Engine Threads, go with the harness.
    let engine_threads = engine_threads_usage(&harness.pool.thread_ids());
    let evaluation = Evaluation {
        test_interval: Duration::from_secs(u64::from(seconds)),
        // The Server's subIntPeriod, the default (RFC 9946 §7.1).
        sub_interval: Duration::from_secs(1),
        ip_version: IpVersion::V4,
        criterion: PerformanceCriterion::DEFAULT,
        first_mode: None,
    };
    // Rows up to 1000 are that many Mbit/s at the IP layer, over IPv4.
    let planned_bps = u64::from(row) * 1_000_000;
    let mut figures = ProxyFigures {
        completed: ended,
        planned: row_load_pdus_per_second(row) * u64::from(seconds) * tests as u64,
        skipped: 0,
        withheld: 0,
        shortfall_median_pct: 100.0,
        lateness_p99: Duration::ZERO,
        seconds: f64::from(seconds),
        waits: counted.faults.ppoll_calls(),
        engine_threads,
    };
    let mut shortfalls = Vec::new();
    for test in &started {
        let (client, server) = (harness.end(test.client), harness.end(test.server));
        figures.completed &= matches!(
            (client.reason, &client.error),
            (
                Some(EndReason::Completed | EndReason::TestIntervalElapsed),
                None
            )
        ) && matches!((server.reason, &server.error), (Some(_), None));
        if let Some(late) = server.facts.burst_lateness {
            figures.lateness_p99 = figures.lateness_p99.max(late.p99);
        }
        let (skipped, withheld) = server.unsent();
        figures.skipped += skipped;
        figures.withheld += withheld;
        let results = MeasurementResults::of_test_connection(&client.events, &evaluation);
        let sent: Vec<Option<u64>> = results
            .sender_bit_rates
            .iter()
            .map(|sent| sent.map(|sent| sent.ip_layer_sender_bit_rate))
            .collect();
        let received: Vec<Option<u64>> = results
            .sub_intervals
            .iter()
            .map(|measured| measured.map(|measured| measured.ip_layer_capacity))
            .collect();
        shortfalls.extend(p2c_shortfalls(&sent, &received, planned_bps));
    }
    shortfalls.sort_by(f64::total_cmp);
    if let Some(&median) = shortfalls.get(shortfalls.len() / 2) {
        figures.shortfall_median_pct = median;
    }
    figures
}

/// Each Sub-Interval's shortfall against the plan, `planned_bps`, in %,
/// as the home lab's P2c takes it Downstream (`scripts/bench/
/// scale_report.py`, `client_figures` and `rates_figures`): on the Load
/// Sender's clock where the Load Receiver estimated what it sent in the
/// Sub-Interval (`sent`, its IPLayerSenderBitRateEstimate, by lpduTime),
/// else at the rate it received (`received`), and 100 % where it measured
/// neither; never below 0. The run's is their median over every Test
/// Connection's Sub-Intervals, the one at half their count once sorted.
fn p2c_shortfalls(sent: &[Option<u64>], received: &[Option<u64>], planned_bps: u64) -> Vec<f64> {
    let planned = planned_bps.max(1) as f64;
    (0..sent.len().max(received.len()))
        .map(|i| {
            let rate = sent
                .get(i)
                .copied()
                .flatten()
                .or_else(|| received.get(i).copied().flatten());
            rate.map_or(100.0, |rate| 100.0 * (1.0 - rate as f64 / planned).max(0.0))
        })
        .collect()
}

/// A row's Load PDUs a second, as the Sending Rate Table builds the rows up
/// to 1000 (`scripts/bench/scale.py`, `row_datagrams_per_s`): its hundreds
/// of Mbit/s as 1250-byte datagrams every 100 µs, its tens as 1250-byte
/// datagrams every 1 ms, and its units as one smaller add-on every 1 ms.
fn row_load_pdus_per_second(row: u16) -> u64 {
    assert!((1..=1000).contains(&row), "rows up to 1000: {row}");
    let (hundreds, tens, units) = if row == 1000 {
        (10, 0, 0)
    } else {
        (row / 100, row / 10 % 10, row % 10)
    };
    10_000 * u64::from(hundreds) + 1_000 * u64::from(tens) + if units > 0 { 1_000 } else { 0 }
}

/// One thread of this process, as Linux's /proc shows it.
#[derive(Debug)]
struct ThreadUsage {
    name: String,
    /// Its time on a CPU so far (`schedstat`).
    cpu: Duration,
    /// Its voluntary context switches so far: its wake-ups.
    wake_ups: u64,
}

/// The Engine Threads, `udpstp-eng-<index>`, of thread ids `tids`, by
/// name: a pool's, read while it runs.
fn engine_threads_usage(tids: &[i32]) -> Vec<ThreadUsage> {
    let mut found: Vec<ThreadUsage> = tids
        .iter()
        .filter_map(|tid| {
            let path = std::path::PathBuf::from(format!("/proc/self/task/{tid}"));
            let name = std::fs::read_to_string(path.join("comm")).ok()?;
            let name = name.trim();
            if !name.starts_with("udpstp-eng-") {
                return None;
            }
            let schedstat = std::fs::read_to_string(path.join("schedstat")).ok()?;
            let cpu = schedstat.split_whitespace().next()?.parse().ok()?;
            let status = std::fs::read_to_string(path.join("status")).ok()?;
            let wake_ups = status
                .lines()
                .find_map(|line| line.strip_prefix("voluntary_ctxt_switches:"))?
                .trim()
                .parse()
                .ok()?;
            Some(ThreadUsage {
                name: name.to_owned(),
                cpu: Duration::from_nanos(cpu),
                wake_ups,
            })
        })
        .collect();
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

// The proxy's figures: P2c's shortfall of each Sub-Interval, on the
// Load Sender's clock where estimated, else as received, else 100 %, and
// never below 0.
#[test]
fn the_proxy_takes_each_sub_intervals_shortfall_as_p2c_does() {
    let shortfalls = p2c_shortfalls(
        &[Some(90_000_000), None, Some(101_000_000), None],
        &[Some(50_000_000), Some(80_000_000), Some(0), None],
        100_000_000,
    );
    let rounded: Vec<f64> = shortfalls.iter().map(|s| s.round()).collect();
    assert_eq!(rounded, [10.0, 20.0, 0.0, 100.0]);
}

// The proxy's planned Load PDUs, as scripts/bench/scale.py counts a
// row's.
#[test]
fn the_proxy_counts_a_rows_load_pdus_as_the_home_lab_does() {
    assert_eq!(row_load_pdus_per_second(4), 1_000);
    assert_eq!(row_load_pdus_per_second(63), 7_000);
    assert_eq!(row_load_pdus_per_second(125), 13_000);
    assert_eq!(row_load_pdus_per_second(250), 25_000);
    assert_eq!(row_load_pdus_per_second(1000), 100_000);
}

// The proxy: a harness's Tests last as long as it asks, here 6 s
// rather than its usual 5 (testIntTime is at least 5), and its Engine
// Threads' CPU time and wake-ups are read from /proc while the pool runs,
// its own alone, whatever other pools the process runs.
#[test]
fn a_harness_runs_tests_as_long_as_asked_and_reads_its_engine_threads() {
    let mut harness = Harness::new(2, EngineSettings::default(), recipe("")).lasting(6);
    let tests: Vec<Started> = (0..2)
        .map(|_| harness.test(Direction::Downstream, 5, recipe(""), Client::Thread))
        .collect();
    assert!(harness.run(Duration::from_secs(30)));
    for started in &tests {
        completed_without_loss(&harness, started, Direction::Downstream);
        let measured = harness.end(started.client).sub_intervals().count();
        assert_eq!(measured, 6, "a 6 s Test's Sub-Intervals");
    }
    let threads = engine_threads_usage(&harness.pool.thread_ids());
    let names: Vec<&str> = threads.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["udpstp-eng-0", "udpstp-eng-1"]);
    for thread in &threads {
        assert!(thread.cpu > Duration::ZERO, "{thread:?}");
        assert!(thread.wake_ups > 0, "{thread:?}");
    }
}

impl Harness {
    /// As [`Harness::test`], the Server's end on a thread of its own, as
    /// the per-Test-Connection executor runs it: for comparisons.
    fn per_test_connection_test(
        &mut self,
        direction: Direction,
        row: u16,
        server: Recipe,
    ) -> Started {
        let (client_id, server_id) = (self.next, self.next + 1);
        self.next += 2;
        let client_socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let server_socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let client_addr = client_socket.local_addr().unwrap();
        let test_port = server_socket.local_addr().unwrap();
        let clock = self.clock;
        let (client_dp, server_dp, _) = activate_lasting(
            || clock.now(),
            direction,
            row,
            self.seconds,
            client_addr,
            test_port,
        );
        server_socket.connect(client_addr).unwrap();
        for (id, data_phase, socket, connected, open) in [
            (server_id, server_dp, server_socket, true, server),
            (client_id, client_dp, client_socket, false, recipe("")),
        ] {
            let engine = super::spawn_with(
                SocketId(id),
                data_phase,
                socket,
                connected,
                DEFAULT_SOCKET_BUFFER_SIZE,
                clock,
                EngineSettings::default(),
                self.report.clone(),
                open,
            )
            .expect("the end starts");
            self.engines.insert(id, engine);
            self.ends.insert(id, End::default());
        }
        Started {
            client: client_id,
            server: server_id,
            client_socket: None,
        }
    }
}
