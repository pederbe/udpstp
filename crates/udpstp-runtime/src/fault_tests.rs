//! Seam 2b: faults injected into a Test Connection's socket, in-process
//! (docs/spec.md Testing Decisions, "fault injection through a test-only
//! feature"). A Client core and a Server core set up a Fixed-Rate Test in
//! memory (the plan-executor contract's `activate`); each end's Data Phase
//! then runs on a real engine over loopback, one end's socket over the
//! fault adapter given at spawn (`socket::faults`), the faults spelled as
//! `UDPSTP_FAULTS` spells them. The fault adapter also counts its socket's
//! waits and reads, injecting nothing when given no faults, which the
//! tests of the wait between turns read. The `udpstp`
//! binary's own fault tests (`crates/udpstp/tests/faults.rs`) keep what
//! needs its processes: a panic releasing the Server's Admission (ADR
//! 0003), and `UDPSTP_FAULTS` reaching the engines.

use std::net::UdpSocket;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use udpstp_core::{DataEvent, Direction, EndReason, SocketId};

use crate::DEFAULT_SOCKET_BUFFER_SIZE;
use crate::clock::Clock;
use crate::contract::activate;
use crate::engine::{self, EngineSettings, Executor, FromEngine};
use crate::socket::faults::{Faults, Faulty};
use crate::socket::send_stamps::Numbering;
use crate::socket::{FastPaths, Platform, SendLag};

/// Which end's socket the faults are injected into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum End {
    Client,
    Server,
}

/// How one end's engine ended, and what its Data Phase reported.
#[derive(Debug, Default)]
struct Ended {
    events: Vec<DataEvent>,
    reason: Option<EndReason>,
    error: Option<String>,
    fast_paths: FastPaths,
    #[cfg_attr(not(any(target_os = "linux", target_os = "android")), allow(dead_code))]
    send_lag: Option<SendLag>,
    #[cfg_attr(not(any(target_os = "linux", target_os = "android")), allow(dead_code))]
    burst_lateness: Option<SendLag>,
    /// Receive Buffer Overflows at its socket, where the OS counts them.
    #[cfg_attr(not(any(target_os = "linux", target_os = "android")), allow(dead_code))]
    overflows: Option<u64>,
}

impl Ended {
    fn reported(&self, what: impl Fn(&DataEvent) -> bool) -> bool {
        self.events.iter().any(what)
    }

    /// The Load PDUs it counted in its Sub-Intervals, received or lost.
    #[cfg_attr(not(any(target_os = "linux", target_os = "android")), allow(dead_code))]
    fn load_pdus(&self) -> (u64, u64) {
        self.events
            .iter()
            .filter_map(|event| match event {
                DataEvent::SubIntervalMeasured(measured) => {
                    Some((u64::from(measured.datagrams), u64::from(measured.loss)))
                }
                _ => None,
            })
            .fold((0, 0), |(received, lost), (r, l)| (received + r, lost + l))
    }
}

/// Both ends of a Test, and how long it ran.
#[derive(Debug)]
struct Test {
    direction: Direction,
    client: Ended,
    server: Ended,
    #[cfg_attr(not(any(target_os = "linux", target_os = "android")), allow(dead_code))]
    ran: Duration,
    /// The executor the Server's end ran on.
    #[cfg_attr(not(any(target_os = "linux", target_os = "android")), allow(dead_code))]
    server_on: Executor,
}

impl Test {
    fn load_sender(&self) -> &Ended {
        match self.direction {
            Direction::Upstream => &self.client,
            Direction::Downstream => &self.server,
        }
    }

    fn load_receiver(&self) -> &Ended {
        match self.direction {
            Direction::Upstream => &self.server,
            Direction::Downstream => &self.client,
        }
    }

    /// The Load PDUs the Load Receiver counted lost, in each Sub-Interval.
    fn losses(&self) -> Vec<u32> {
        self.load_receiver()
            .events
            .iter()
            .filter_map(|event| match event {
                DataEvent::SubIntervalMeasured(measured) => Some(measured.loss),
                _ => None,
            })
            .collect()
    }

    /// The Client's Test completed, as its exit status says: with the STOP
    /// exchange, or by its own Test Interval timer. Neither engine failed,
    /// and no Sub-Interval lost a Load PDU. The Server may end unconfirmed:
    /// a Client is done once its STOP2 PDU is handed over, whether or not
    /// the kernel takes it, which a fault may refuse.
    fn completes_without_loss(&self, what: &str) {
        let client = (self.client.reason, &self.client.error);
        assert!(
            matches!(
                client,
                (
                    Some(EndReason::Completed | EndReason::TestIntervalElapsed),
                    None
                )
            ),
            "{what}: the Client's end: {client:?}"
        );
        let server = (self.server.reason, &self.server.error);
        assert!(
            matches!(server, (Some(_), None)),
            "{what}: the Server's end: {server:?}"
        );
        let lost = self.losses();
        assert!(
            !lost.is_empty() && lost.iter().all(|&l| l == 0),
            "{what}: no Sub-Interval lost a Load PDU: {lost:?}"
        );
    }
}

/// Runs a Fixed-Rate Test at `row` in `direction` over loopback, `faulty`'s
/// socket with `faults`: what each end's engine reported.
fn run_test(direction: Direction, row: u16, faulty: End, faults: &Faults) -> Test {
    run_test_on(
        Executor::PerTestConnection,
        direction,
        row,
        faulty,
        faults,
        false,
    )
}

/// The executors a Server's end can run on here: a thread per Test
/// Connection, and on Linux the pooled executor too.
fn executors() -> Vec<Executor> {
    let mut executors = vec![Executor::PerTestConnection];
    if cfg!(any(target_os = "linux", target_os = "android")) {
        executors.push(Executor::Pool);
    }
    executors
}

/// As [`run_test`], `faulty`'s socket asking for kernel send timestamps
/// when `send_stamps`.
#[cfg_attr(not(any(target_os = "linux", target_os = "android")), allow(dead_code))]
fn run_test_asking(
    direction: Direction,
    row: u16,
    faulty: End,
    faults: &Faults,
    send_stamps: bool,
) -> Test {
    run_test_on(
        Executor::PerTestConnection,
        direction,
        row,
        faulty,
        faults,
        send_stamps,
    )
}

/// As [`run_test_asking`], the Server's end on `server_on`: on Linux the
/// pooled executor, on an Engine Thread of its own, which the
/// faults reach through its recipe's hooks when they are the Server's.
fn run_test_on(
    server_on: Executor,
    direction: Direction,
    row: u16,
    faulty: End,
    faults: &Faults,
    send_stamps: bool,
) -> Test {
    const CLIENT: SocketId = SocketId(1);
    const SERVER: SocketId = SocketId(2);
    let client_socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let server_socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let client_addr = client_socket.local_addr().unwrap();
    let test_port = server_socket.local_addr().unwrap();
    let clock = Clock::new();
    let (client, server, _) = activate(|| clock.now(), direction, row, client_addr, test_port);
    // As the Server's Control Phase connects it on activation.
    server_socket.connect(client_addr).unwrap();
    let (report, mut reports) = mpsc::unbounded_channel();
    let buffers = DEFAULT_SOCKET_BUFFER_SIZE;
    let faulty_socket = || Faulty {
        inner: Platform,
        faults: faults.clone(),
    };
    // Kernel send timestamps for the faulty end's socket, their messages
    // carrying their own numbers where the kernel takes them; none
    // for the other end.
    let faulty_settings = EngineSettings {
        send_stamps: send_stamps.then_some(Numbering::Explicit),
        ..EngineSettings::default()
    };
    let plain = EngineSettings::default();
    // The pooled executor's, started with the Server's end.
    let mut pool = None;
    let started = Instant::now();
    // Kept until both have exited: dropping one stops its engine.
    let engines = match faulty {
        End::Client => (
            engine::spawn_with(
                CLIENT,
                client,
                client_socket,
                false,
                buffers,
                clock,
                faulty_settings,
                report.clone(),
                faulty_socket(),
            )
            .expect("the Client's engine starts"),
            start_on(
                server_on,
                &mut pool,
                SERVER,
                server,
                server_socket,
                clock,
                plain,
                report,
                Faulty {
                    inner: Platform,
                    faults: Faults::default(),
                },
            ),
        ),
        End::Server => (
            engine::spawn_with(
                CLIENT,
                client,
                client_socket,
                false,
                buffers,
                clock,
                plain,
                report.clone(),
                Platform,
            )
            .expect("the Client's engine starts"),
            start_on(
                server_on,
                &mut pool,
                SERVER,
                server,
                server_socket,
                clock,
                faulty_settings,
                report,
                faulty_socket(),
            ),
        ),
    };
    // The socket buffers each engine got as it started, which it reports
    // again as it exits.
    let buffers = [engines.0.buffers(), engines.1.buffers()];
    let mut ends = [Ended::default(), Ended::default()];
    let mut exited = [false, false];
    let until = started + Duration::from_secs(30);
    while exited.contains(&false) && Instant::now() < until {
        let Ok(report) = reports.try_recv() else {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        };
        match report {
            FromEngine::Event { socket, event } => ends[socket.0 as usize - 1].events.push(event),
            FromEngine::Exited {
                socket,
                reason,
                error,
                facts,
            } => {
                let at = socket.0 as usize - 1;
                assert!(
                    facts.socket_buffers.is_some() && facts.socket_buffers == buffers[at],
                    "{socket:?} exited with the socket buffers it got as it started: {:?}, {:?}",
                    facts.socket_buffers,
                    buffers[at]
                );
                let end = &mut ends[at];
                (end.reason, end.error, end.fast_paths) = (reason, error, facts.fast_paths);
                end.send_lag = facts.send_lag;
                end.burst_lateness = facts.burst_lateness;
                end.overflows = facts.receive_buffer_overflows;
                exited[at] = true;
            }
        }
    }
    assert!(!exited.contains(&false), "the engines didn't exit");
    let [client, server] = ends;
    Test {
        direction,
        client,
        server,
        ran: started.elapsed(),
        server_on,
    }
}

/// The pooled executor, where there is one.
#[cfg(any(target_os = "linux", target_os = "android"))]
type Pool = crate::engine::pool::EnginePool<Faulty<Platform>>;
#[cfg(not(any(target_os = "linux", target_os = "android")))]
type Pool = ();

/// Starts the Server's end, `data_phase` on `socket`, on `executor`: the
/// pooled executor's in `pool`, of one Engine Thread, started here.
#[allow(clippy::too_many_arguments)]
fn start_on(
    executor: Executor,
    pool: &mut Option<Pool>,
    socket_id: SocketId,
    data_phase: udpstp_core::DataPhase,
    socket: UdpSocket,
    clock: Clock,
    settings: EngineSettings,
    report: mpsc::UnboundedSender<FromEngine>,
    open: Faulty<Platform>,
) -> engine::Engine {
    let buffers = DEFAULT_SOCKET_BUFFER_SIZE;
    #[cfg(any(target_os = "linux", target_os = "android"))]
    if executor == Executor::Pool {
        let pool = pool.get_or_insert_with(|| {
            let settings = EngineSettings {
                threads: std::num::NonZeroUsize::new(1),
                ..settings
            };
            crate::engine::pool::EnginePool::new(clock, settings, report, open.clone()).0
        });
        return pool
            .start_with(socket_id, data_phase, socket, true, buffers, open)
            .expect("the Server's engine starts");
    }
    let _ = (executor, pool);
    engine::spawn_with(
        socket_id, data_phase, socket, true, buffers, clock, settings, report, open,
    )
    .expect("the Server's engine starts")
}

/// Runs each of `tests` at once, each on a thread of its own: what each
/// run's Test reported, in order.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn run_tests(tests: &[(Direction, u16, End, Faults)]) -> Vec<Test> {
    run_tests_on(Executor::PerTestConnection, tests)
}

/// As [`run_tests`], the Servers' ends on `executor`.
fn run_tests_on(executor: Executor, tests: &[(Direction, u16, End, Faults)]) -> Vec<Test> {
    std::thread::scope(|scope| {
        let runs: Vec<_> = tests
            .iter()
            .map(|(direction, row, faulty, faults)| {
                scope.spawn(move || run_test_on(executor, *direction, *row, *faulty, faults, false))
            })
            .collect();
        runs.into_iter()
            .map(|run| run.join().expect("the Test ran"))
            .collect()
    })
}

/// The fast paths an engine that falls back from none of them uses: a
/// Load Receiver sends no Load PDUs.
#[cfg(any(target_os = "linux", target_os = "android"))]
const RECEIVING: FastPaths = FastPaths {
    sendmmsg: false,
    udp_gso: false,
    recvmmsg: true,
    kernel_rx_timestamps: true,
    ppoll: true,
    epoll: false,
};

// docs/spec.md (the engine must release admission state on every exit path,
// including a panic; ADR 0003): a socket that panics as it opens, on the
// engine's thread before its first turn, still has the engine report its
// end, which is what makes the Control Phase release the Test
// Connection's Admission, with the socket buffers it got as it started
// and no fast path. The binary's fault test shows the release after a
// panic mid-Test (crates/udpstp/tests/faults.rs).
#[test]
fn a_panic_as_the_socket_opens_still_reports_the_end() {
    let client_socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let server_socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let client_addr = client_socket.local_addr().unwrap();
    let test_port = server_socket.local_addr().unwrap();
    let clock = Clock::new();
    let (_, server, _) = activate(
        || clock.now(),
        Direction::Downstream,
        5,
        client_addr,
        test_port,
    );
    let (report, mut reports) = mpsc::unbounded_channel();
    let faulty = Faulty {
        inner: Platform,
        faults: Faults::parse("panic_on_open=1"),
    };
    let engine = engine::spawn_with(
        SocketId(2),
        server,
        server_socket,
        false,
        DEFAULT_SOCKET_BUFFER_SIZE,
        clock,
        EngineSettings::default(),
        report,
        faulty,
    )
    .expect("the engine starts");
    let until = Instant::now() + Duration::from_secs(10);
    while Instant::now() < until {
        match reports.try_recv() {
            Ok(FromEngine::Exited {
                reason,
                error,
                facts,
                ..
            }) => {
                assert_eq!(reason, None, "the engine failed");
                assert!(error.is_some(), "with an error to log");
                assert_eq!(facts.fast_paths, FastPaths::default(), "having used none");
                assert!(
                    facts.socket_buffers.is_some() && facts.socket_buffers == engine.buffers(),
                    "with the socket buffers it got as it started: {:?}, {:?}",
                    facts.socket_buffers,
                    engine.buffers()
                );
                return;
            }
            Ok(FromEngine::Event { event, .. }) => panic!("the Data Phase ran: {event:?}"),
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    panic!("the engine reported no end");
}

// docs/spec.md (EAGAIN/ENOBUFS and partial sends); ADR 0008: a send the host
// didn't take is sent later as a Deferred Datagram, never counted lost;
// ; on either executor of the Server's
#[test]
fn injected_partial_sends_produce_no_reported_loss() {
    let faults = Faults::parse("would_block_every=3,no_buffers_every=7");
    // The Load Sender fails sends: the Server Downstream, the Client
    // Upstream.
    let tests = executors().into_iter().flat_map(|executor| {
        run_tests_on(
            executor,
            &[
                (Direction::Downstream, 5, End::Server, faults.clone()),
                (Direction::Upstream, 5, End::Client, faults.clone()),
            ],
        )
    });
    for test in tests {
        let what = format!("{:?} on {:?}", test.direction, test.server_on);
        test.completes_without_loss(&what);
        // The faults did hold sends back: the Load Sender withheld Bursts
        // while its Deferred Datagrams waited.
        assert!(
            test.load_sender()
                .reported(|e| matches!(e, DataEvent::BurstsWithheld { .. })),
            "{what}: the injected faults held sends back"
        );
    }
}

//  (ADR 0012): a Load Sender whose host sends slower than its
// schedule still ends the Test with its STOP, reporting what it couldn't
// send as Skipped Bursts; its send turn is bounded, so its timeouts run;
// on either executor of the Server's, whose turn is bounded too.
#[test]
fn a_host_that_sends_slower_than_its_schedule_still_stops_the_test() {
    // Row 5 sends one Load PDU every 1 ms; each send takes 2 ms.
    let faults = Faults::parse("slow_send_us=2000");
    for executor in executors() {
        let test = run_test_on(
            executor,
            Direction::Downstream,
            5,
            End::Server,
            &faults,
            false,
        );
        assert_eq!(
            test.client.reason,
            Some(EndReason::Completed),
            "{executor:?}: the Server's STOP arrived"
        );
        assert!(
            test.server
                .reported(|e| matches!(e, DataEvent::BurstsSkipped { .. })),
            "{executor:?}: the Server reported what it couldn't send as Skipped Bursts"
        );
    }
}

// docs/spec.md (partial sends are reported to the core); : a batch the
// kernel takes only part of, the rest refused with EAGAIN, is reported
// exactly: the Load PDUs it didn't take are sent later as Deferred
// Datagrams and none is counted lost. Row 25's Bursts are two Load PDUs
// and an Add-on, sent without GSO here so that each batch holds several
// messages, of which the kernel takes one; the Deferred Datagrams pile up
// until Bursts are withheld.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn partial_batched_sends_produce_no_reported_loss() {
    let faults = Faults::parse("gso_unsupported=1,partial_batch_every=1");
    let test = run_test(Direction::Upstream, 25, End::Client, &faults);
    test.completes_without_loss("partial sendmmsg calls");
    // Only Linux sends in batches.
    if cfg!(any(target_os = "linux", target_os = "android")) {
        let paths = FastPaths {
            sendmmsg: true,
            ..RECEIVING
        };
        assert_eq!(test.client.fast_paths, paths);
        assert!(
            test.client
                .reported(|e| matches!(e, DataEvent::BurstsWithheld { .. })),
            "the partial sendmmsg calls held sends back"
        );
    }
}

// : where the kernel or the device refuses UDP GSO, the engine sends
// with sendmmsg alone; where it refuses sendmmsg, one Load PDU at a time.
// Either way the Test completes without loss, and the engine reports the
// fast paths it used.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn sending_falls_back_when_gso_or_sendmmsg_is_refused() {
    let cases = [
        (
            "gso_unsupported=1",
            FastPaths {
                sendmmsg: true,
                ..RECEIVING
            },
        ),
        ("sendmmsg_unsupported=1", RECEIVING),
    ];
    let tests = run_tests(
        &cases.map(|(faults, _)| (Direction::Upstream, 25, End::Client, Faults::parse(faults))),
    );
    for ((faults, linux_paths), test) in cases.iter().zip(tests) {
        test.completes_without_loss(faults);
        assert_eq!(test.client.fast_paths, *linux_paths, "{faults}");
    }
}

// : where the kernel refuses recvmmsg, the engine reads one datagram at
// a time, timed by the read; where it refuses kernel receive timestamps,
// it reads in batches without them. Either way the Test completes without
// loss, as Load Receiver (Downstream) and as Load Sender (Upstream), and
// the engine reports the fast paths it used.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn receiving_falls_back_when_recvmmsg_or_timestamps_are_refused() {
    let only_ppoll = FastPaths {
        ppoll: true,
        ..FastPaths::default()
    };
    let sending = FastPaths {
        sendmmsg: true,
        udp_gso: true,
        ..only_ppoll
    };
    let cases = [
        (Direction::Downstream, "recvmmsg_unsupported=1", only_ppoll),
        (
            Direction::Downstream,
            "rx_timestamps_unsupported=1",
            FastPaths {
                recvmmsg: true,
                ..only_ppoll
            },
        ),
        (Direction::Upstream, "recvmmsg_unsupported=1", sending),
        (
            Direction::Upstream,
            "rx_timestamps_unsupported=1",
            FastPaths {
                recvmmsg: true,
                ..sending
            },
        ),
    ];
    let tests = run_tests(
        &cases.map(|(direction, faults, _)| (direction, 25, End::Client, Faults::parse(faults))),
    );
    for ((direction, faults, linux_paths), test) in cases.iter().zip(tests) {
        let what = format!("{direction:?} {faults}");
        test.completes_without_loss(&what);
        assert_eq!(test.client.fast_paths, *linux_paths, "{what}");
    }
}

// : an error that stays pending on the socket makes every ppoll call
// return at once. The engine drains what it can, and its spin guard backs
// it off to short sleeps rather than spinning: the Test completes without
// loss, as Load Sender (Downstream) and as Load Receiver (Upstream). On
// Linux the Server's engine made a few thousand turns a second, counted by
// the reads it makes at least once a turn, however it waited between them,
// where a spinning one makes hundreds of thousands. On the pooled
// executor the fault keeps its Engine Thread's epoll set readable
// with nothing to harvest, and its ppoll calls, one a wait, are bounded
// alike (its turns read only sockets that signalled).
#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn a_stuck_pending_error_does_not_spin_the_engine() {
    for executor in executors() {
        let faults = [
            Faults::parse("pollerr_stuck=1"),
            Faults::parse("pollerr_stuck=1"),
        ];
        let tests = run_tests_on(
            executor,
            &[
                (Direction::Downstream, 25, End::Server, faults[0].clone()),
                (Direction::Upstream, 25, End::Server, faults[1].clone()),
            ],
        );
        for (test, _faults) in tests.iter().zip(&faults) {
            let what = format!("{:?} on {executor:?}", test.direction);
            test.completes_without_loss(&what);
            #[cfg(any(target_os = "linux", target_os = "android"))]
            {
                let seconds = test.ran.as_secs_f64();
                let polls = _faults.ppoll_calls() as f64 / seconds;
                let turns = _faults.read_calls() as f64 / seconds;
                eprintln!("{what}: {turns:.0} turns a second, {polls:.0} ppoll calls");
                assert!(polls >= 1.0, "{what}: the stuck error was never waited on");
                let busiest = if executor == Executor::Pool {
                    polls
                } else {
                    turns
                };
                assert!(
                    busiest < 20_000.0,
                    "{what}: the Server's engine made {turns:.0} turns a second, \
                     {polls:.0} ppoll calls"
                );
            }
        }
    }
}

// : where the kernel refuses ppoll, the engine sleeps between turns as
// before: the Test completes without loss, as Load Receiver (Downstream)
// and as Load Sender (Upstream), and the engine doesn't report ppoll.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn waiting_falls_back_to_sleeping_when_ppoll_is_refused() {
    let faults = Faults::parse("ppoll_unsupported=1");
    let tests = run_tests(&[
        (Direction::Downstream, 25, End::Client, faults.clone()),
        (Direction::Upstream, 25, End::Client, faults),
    ]);
    let sending = FastPaths {
        sendmmsg: true,
        udp_gso: true,
        ppoll: false,
        ..RECEIVING
    };
    let expected = [
        FastPaths {
            ppoll: false,
            ..RECEIVING
        },
        sending,
    ];
    for (test, linux_paths) in tests.iter().zip(expected) {
        let what = format!("{:?}", test.direction);
        test.completes_without_loss(&what);
        assert_eq!(test.client.fast_paths, linux_paths, "{what}");
    }
}

//  (kernel send timestamps matched to the right Load PDUs after a
// failed send): mid-Test drops of messages the kernel numbered, as a queue
// discipline's (`qdisc_drop_every`, a real refusal), don't stop an
// Upstream Test's send timestamps where the Load Sender's messages carry
// their own numbers (Linux 6.13): they keep coming from the Send Plans
// sent after the drops. Where the kernel numbers them (the fallback,
// forced here), the count stops at the first drop. Either way each lag is
// its own Send Plan's: one matched to a later plan would read 0. The
// socket's own test (`socket::linux`) shows each timestamp's plan. The
// Load Sender reports the calls its Load PDUs went with: a sendmsg call
// each where they carried their numbers, and sendmmsg calls where the
// kernel numbered them. With UDP GSO each batch is one message, which a
// drop takes whole; without, three, of which it takes the second, after
// the first went, the call answering 1 as sendmmsg does.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn send_timestamps_go_on_after_mid_test_drops_of_numbered_messages() {
    // Row 25 sends a Send Plan every 1 ms: two Load PDUs and an Add-on,
    // one UDP GSO message or three messages; every 50th batch loses one.
    let cases = [
        ("qdisc_drop_every=50", true, 1),
        ("qdisc_drop_every=50,ts_opt_id_unsupported=1", false, 1),
        ("qdisc_drop_every=50,gso_unsupported=1", true, 3),
        (
            "qdisc_drop_every=50,gso_unsupported=1,ts_opt_id_unsupported=1",
            false,
            3,
        ),
    ];
    let tests: Vec<Test> = std::thread::scope(|scope| {
        let runs: Vec<_> = cases
            .iter()
            .map(|(faults, _, _)| {
                let faults = Faults::parse(faults);
                scope.spawn(move || {
                    run_test_asking(Direction::Upstream, 25, End::Client, &faults, true)
                })
            })
            .collect();
        runs.into_iter()
            .map(|run| run.join().expect("the Test ran"))
            .collect()
    });
    let numbered = crate::socket::send_stamps::kernel_takes_numbers();
    for ((faults, numbers, messages), test) in cases.iter().zip(&tests) {
        test.completes_without_loss(faults);
        let lag = test.client.send_lag.expect("the Load Sender's send lag");
        assert!(lag.p50 >= Duration::from_micros(1), "{faults}: {lag:?}");
        let carried = numbered && *numbers;
        if carried {
            assert!(
                lag.samples >= 1000,
                "{faults}: counted past the drops: {lag:?}"
            );
        } else {
            assert!(
                lag.samples < 50 * messages,
                "{faults}: the count stopped at the first drop: {lag:?}"
            );
        }
        let paths = FastPaths {
            sendmmsg: !carried,
            udp_gso: *messages == 1,
            ..RECEIVING
        };
        assert_eq!(test.client.fast_paths, paths, "{faults}");
    }
}

// , : the Linux Server's pooled executor, as the re-check's
// precision arm runs it, asked for kernel send timestamps: its Load
// Sender, Downstream, drains them from the error queue its epoll set
// reports (EPOLLERR), matches them, and reports their send lag, with the
// fast path `epoll` and its Burst lateness; the Test completes without
// loss.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn a_pooled_load_sender_reports_its_send_lag_when_asked() {
    let test = run_test_on(
        Executor::Pool,
        Direction::Downstream,
        25,
        End::Server,
        &Faults::default(),
        true,
    );
    test.completes_without_loss("pooled, with kernel send timestamps");
    let lag = test.server.send_lag.expect("the Load Sender's send lag");
    assert!(lag.samples >= 1000, "{lag:?}");
    assert!(test.server.fast_paths.epoll, "{:?}", test.server.fast_paths);
    let late = test.server.burst_lateness.expect("its Burst lateness");
    assert!(late.samples >= 1000, "{late:?}");
}

//  (at row 4 a Load Receiver wakes about once per Load PDU): row 4
// sends a Load PDU every 1 ms against a receive hold-off of 250 µs, so a
// Load Receiver that held off after every read waited twice per Load PDU:
// once until the hold-off ended, with nothing to read, and once for the
// next arrival. Holding off only when more than one datagram is due, it
// waits about once per Load PDU, as Load Receiver Upstream (the Server)
// and Downstream (the Client), counted by its ppoll calls, one a wait.
// They are counted over its whole Data Phase and the Load PDUs over the
// Test Interval, so its waits after the Test Interval, until the STOP
// exchange ends, add about a tenth, and its Status PDUs and its first
// 10 ms a few more; a busy host makes fewer, several Load PDUs read at a
// wake-up. The Server's pooled executor holds off per Engine
// Thread by the same rule, and waits about once per Load PDU too.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn a_load_receiver_at_row_4_waits_about_once_per_load_pdu() {
    let counted = [Faults::default(), Faults::default(), Faults::default()];
    let mut tests = run_tests(&[
        (Direction::Upstream, 4, End::Server, counted[0].clone()),
        (Direction::Downstream, 4, End::Client, counted[1].clone()),
    ]);
    tests.extend(run_tests_on(
        Executor::Pool,
        &[(Direction::Upstream, 4, End::Server, counted[2].clone())],
    ));
    for (test, counted) in tests.iter().zip(&counted) {
        let what = format!("{:?} on {:?}", test.direction, test.server_on);
        test.completes_without_loss(&what);
        let paths = test.load_receiver().fast_paths;
        assert!(paths.ppoll || paths.epoll, "{what}: it waited on arrivals");
        let (received, _) = test.load_receiver().load_pdus();
        let waits = counted.ppoll_calls();
        eprintln!("{what}: {waits} waits for {received} Load PDUs");
        assert!(received >= 2_500, "{what}: 5 s of row 4: {received}");
        assert!(
            2 * waits < 3 * received,
            "{what}: {waits} waits for {received} Load PDUs, not fewer than 1.5 each"
        );
    }
}

/// The voluntary context switches a second of this process's thread named
/// `name`, its wake-ups, over `span` starting `after` from now; `None` if
/// the thread can't be found or read (Linux's /proc).
#[cfg(any(target_os = "linux", target_os = "android"))]
fn wake_ups_a_second(name: &str, after: Duration, span: Duration) -> Option<f64> {
    use crate::control_phase_tests::voluntary_switches;
    std::thread::sleep(after);
    let switches = || voluntary_switches(name);
    let (first, started) = (switches()?, Instant::now());
    std::thread::sleep(span);
    let last = switches()?;
    Some(last.saturating_sub(first) as f64 / started.elapsed().as_secs_f64())
}

// : a measurement run by hand, which gates nothing: a Load Receiver's
// waits and wake-ups, and its Receive Buffer Overflows, over loopback, at
// row 4 and at rows above 1 Gbps, Downstream and Upstream, one Fixed-Rate
// Test of 5 s at a time. Its waits are its ppoll calls over its whole
// Data Phase, as the fault adapter counts them (injecting nothing), the
// STOP exchange after the Test Interval included, so per Load PDU of the
// Test Interval they read about a tenth above its wake-ups: its engine
// thread's voluntary context switches over 3 s in the middle of the
// Test. Run it optimized and alone on the host, under its measurement
// lock where it has one; it prints a line a Test:
//
//     cargo test --release -p udpstp-runtime --lib load_receiver_wake_ups -- --ignored --nocapture
//
// `UDPSTP_WAKE_UPS_ROWS` sets the rows, comma-separated (4,1000,1036,1062).
#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
#[ignore = "a measurement run by hand, optimized and alone on the host: see its comment"]
fn load_receiver_wake_ups() {
    let rows: Vec<u16> = std::env::var("UDPSTP_WAKE_UPS_ROWS")
        .unwrap_or_else(|_| "4,1000,1036,1062".into())
        .split(',')
        .map(|row| {
            row.trim()
                .parse()
                .expect("UDPSTP_WAKE_UPS_ROWS: rows, comma-separated")
        })
        .collect();
    let ends = [
        (Direction::Downstream, End::Client, "udpstp-engine-1"),
        (Direction::Upstream, End::Server, "udpstp-engine-2"),
        (Direction::Upstream, End::Server, "udpstp-eng-0"),
    ];
    for row in rows {
        for (direction, receiver, thread) in ends {
            // The Server's Engine Thread of the pooled executor.
            let executor = if thread.starts_with("udpstp-eng-") {
                Executor::Pool
            } else {
                Executor::PerTestConnection
            };
            let counted = Faults::default();
            let (test, wake_ups) = std::thread::scope(|scope| {
                let (after, span) = (Duration::from_millis(1_500), Duration::from_secs(3));
                let sampled = scope.spawn(move || wake_ups_a_second(thread, after, span));
                let test = run_test_on(executor, direction, row, receiver, &counted, false);
                (test, sampled.join().expect("the sampler ran"))
            });
            let (received, lost) = test.load_receiver().load_pdus();
            let skipped: u64 = test
                .load_sender()
                .events
                .iter()
                .filter_map(|event| match event {
                    DataEvent::BurstsSkipped { datagrams, .. } => Some(u64::from(*datagrams)),
                    _ => None,
                })
                .sum();
            // The Sub-Intervals' Load PDUs came in the Test Interval, 5 s.
            let per_second = received as f64 / 5.0;
            let waits = counted.ppoll_calls() as f64;
            let wake_ups = wake_ups.unwrap_or(f64::NAN);
            println!(
                "WAKE-UPS {direction:?} on {executor:?} row {row}: {received} Load PDUs received, {lost} lost, \
                 {skipped} skipped by the Load Sender, Receive Buffer Overflows {:?}; \
                 {:.0} waits a second over the Test ({:.2} a Load PDU), \
                 {wake_ups:.0} wake-ups a second mid-Test ({:.2} a Load PDU)",
                test.load_receiver().overflows,
                waits / test.ran.as_secs_f64(),
                waits / received as f64,
                wake_ups / per_second,
            );
            let end = (test.load_receiver().reason, &test.load_receiver().error);
            assert!(
                received > 0 && matches!(end, (Some(_), None)),
                "{direction:?} row {row}: the Test ran: {end:?}"
            );
        }
    }
}
