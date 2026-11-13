//! The per-Test-Connection executor: one Engine Thread per Test Connection
//! that runs the core's [`DataPhase`] on the Test Connection's socket, the
//! Client's on every OS and the Server's on Windows and macOS (ADR 0008; a
//! Linux Server runs the pooled executor, `pool.rs`).
//!
//! Each turn it sends every due Send Plan whole, writing the Load PDU
//! headers just before the send, for at most 1 ms, reading what arrived
//! between the Send Plans of a catch-up (ADR 0012); reads what has arrived,
//! so a Status PDU waiting in the socket counts before any timeout is
//! judged; handles a due timeout; sends due Status PDUs; and passes the
//! Data Phase's events on. That work is the Test Connection's slot's
//! (`slot.rs`), and the thread is the per-Test-Connection executor,
//! which owns one slot, runs its turns and waits between them, so another
//! executor can drive the same slots. Between turns, on Linux, the engine waits in
//! ppoll on its socket until the next deadline or an arrival, with a
//! receive hold-off and a spin guard (`wait.rs`). Elsewhere, and where the kernel refuses ppoll, it sleeps
//! until the next deadline, in slices of at most 1 ms so arrivals wait at
//! most that long. A socket receive timeout would wake on the kernel's tick
//! instead (1-4 ms on Linux, about 15.6 ms on Windows), making 1 ms
//! Transmitters late.
//!
//! How the datagrams go and come is the socket module's (`socket`): on
//! Linux in batches, sendmmsg with UDP GSO and recvmmsg with kernel
//! receive timestamps, each with its fallback, every error
//! classified once (ADR 0006), and the fast paths it used reported
//! ([`FastPaths`]). The engine drives a [`Socket`] over the platform's
//! adapter, or over one that injects faults, given at spawn
//! ([`spawn_with`]).
//!
//! When it exits it reports how the Data Phase ran on this host as one
//! value, [`EngineFacts`], which the Control Phase passes on unread: to the
//! Client's results and the Server's log.
//!
//! A panic on the engine's thread, as its socket opens or in the core,
//! retires only this Test Connection, and its end is always reported, so
//! its admission is released. The engine reports
//! without blocking (ADR 0003). Peer-paced notices share a fixed queue
//! budget, while result, accounting and exit records are retained.
//! The plan-executor contract (`contract_tests.rs`) checks what it
//! puts on the wire against the simulator's executor's.

use std::io;
use std::net::UdpSocket;
use std::num::NonZeroUsize;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use udpstp_core::{DataEvent, DataPhase, EndReason, SocketId};

use crate::buffers::SocketBuffers;
use crate::clock::Clock;
use crate::socket::send_stamps::Numbering;
use crate::socket::{self, Adapter, FastPaths, Open, SendLag, Settings, Socket};

#[cfg(any(target_os = "linux", target_os = "android"))]
pub(crate) mod pool;
#[cfg(all(test, any(target_os = "linux", target_os = "android")))]
mod pool_tests;
// The pooled executor's policy is pure, and tested everywhere; only Linux
// runs it.
pub(crate) mod reports;
#[cfg_attr(not(any(target_os = "linux", target_os = "android")), allow(dead_code))]
pub(crate) mod schedule;
mod slot;
#[cfg(test)]
mod slot_tests;
pub(crate) mod times;

use slot::Slot;
use times::{Fresh, Times};

/// The longest the engine sleeps between reads, and the longest a turn
/// sends for.
const SLICE: Duration = Duration::from_millis(1);

/// How late a missed Burst may be and still be sent (ADR 0012). This
/// engine sleeps until each deadline. On Windows its timer wakes about
/// 400 µs late at p50 and up to about 1 ms at p99 on an idle host, so the
/// budget there is 2 ms, with which every row up to 1 Gbps skips nothing
///; elsewhere 1 ms, the core's default.
const CATCH_UP_BUDGET: Duration = if cfg!(windows) {
    Duration::from_millis(2)
} else {
    udpstp_core::CATCH_UP_BUDGET
};

/// How the Data Phase Engine runs where it may differ from its own
/// choices: experimental settings for measurements, not a stable
/// interface (ADR 0013). The binary reads them from its environment once
/// and passes them to [`serve`](crate::serve) and
/// [`run_test`](crate::run_test), which give them to each engine at spawn
///; the runtime reads no environment variable for them. The
/// default is the engine's own choice in each.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EngineSettings {
    /// How late a missed Burst may be and still be sent (ADR 0012), in
    /// place of 1 ms (2 ms on Windows).
    pub catch_up_budget: Option<Duration>,
    /// A fixed receive hold-off (Linux), zero for none, in place of
    /// the one derived from the receive buffer. It is then taken whatever
    /// is due within it: by a Load Receiver after every turn that
    /// read datagrams, by a Load Sender when flooded.
    pub hold_off: Option<Duration>,
    /// Whether a Load Sender asks the kernel to timestamp what it sends
    /// (Linux), and who numbers its messages.
    pub send_stamps: Option<Numbering>,
    /// How many Engine Threads the pooled executor may start, in place of
    /// the number of CPUs the process may use.
    pub threads: Option<NonZeroUsize>,
}

/// How a Data Phase Engine schedules its Test Connections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Executor {
    /// Engine Threads, as many as the CPUs the process may use, each
    /// serving many Test Connections from one loop: a Linux Server's own.
    Pool,
    /// One thread per Test Connection: the Client's, and every endpoint's
    /// on Windows and macOS.
    PerTestConnection,
}

impl EngineSettings {
    /// The catch-up budget: the one set, or [`CATCH_UP_BUDGET`].
    pub(crate) fn catch_up_budget(&self) -> Duration {
        self.catch_up_budget.unwrap_or(CATCH_UP_BUDGET)
    }
}

/// What an engine reports to the Control Phase that started it.
#[derive(Debug)]
pub(crate) enum FromEngine {
    Event {
        socket: SocketId,
        event: DataEvent,
    },
    /// The engine has exited: the Test Connection is over. `reason` is
    /// `None` when the engine failed (a socket error or a panic). `facts`
    /// say how its Data Phase ran on this host, however it ended.
    Exited {
        socket: SocketId,
        reason: Option<EndReason>,
        error: Option<String>,
        facts: EngineFacts,
    },
}

/// How a Test Connection's Data Phase ran on this host, as its Data Phase
/// Engine reports it when it exits, however it ended: results metadata.
/// The Control Phase passes it on unread, with the Test Connection's end,
/// to the Client's results ([`ClientOutcome`](crate::ClientOutcome)) and
/// the Server's log ([`ServerNotice::DataPhaseEnded`](crate::ServerNotice)),
/// so a fact added here reaches both. A Data Phase that never started has
/// the default: nothing known, no fast path.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EngineFacts {
    /// The socket buffers the Test Connection got as its engine started,
    /// if they could be read: short when the OS's limits held them down.
    pub socket_buffers: Option<SocketBuffers>,
    /// The fast paths the engine sent Load PDUs, read datagrams
    /// and waited between turns with.
    pub fast_paths: FastPaths,
    /// Receive Buffer Overflows at its socket, where the OS counts them
    /// (Linux; ).
    pub receive_buffer_overflows: Option<u64>,
    /// How much later than their lpduTime the kernel sent the Load PDUs,
    /// when [`EngineSettings::send_stamps`] asked it to timestamp them
    /// (Linux, a Load Sender; ): experimental.
    pub send_lag: Option<SendLag>,
    /// How late after their scheduled time the Load Sender wrote its Send
    /// Plans, and so stamped their lpduTime: its Burst lateness,
    /// summed up as the send lag is. Experimental.
    pub burst_lateness: Option<SendLag>,
}

/// A running engine. Dropping it stops the Test Connection's Data Phase;
/// the engine then exits by itself and reports it.
pub(crate) struct Engine {
    stop: Stop,
    buffers: Option<SocketBuffers>,
}

/// How a running engine is told to stop.
enum Stop {
    /// The per-Test-Connection executor's thread sees the flag each turn.
    Flag(Arc<AtomicBool>),
    /// The Test Connection's Engine Thread is sent a command.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    Pool(pool::Stopper),
}

impl Engine {
    pub(crate) fn stop(&self) {
        match &self.stop {
            Stop::Flag(flag) => flag.store(true, Ordering::Relaxed),
            #[cfg(any(target_os = "linux", target_os = "android"))]
            Stop::Pool(stopper) => stopper.stop(),
        }
    }

    /// A Test Connection on an Engine Thread of the pooled executor.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub(crate) fn pooled(stopper: pool::Stopper, buffers: Option<SocketBuffers>) -> Self {
        Self {
            stop: Stop::Pool(stopper),
            buffers,
        }
    }

    /// The socket buffers the Test Connection got, if they could be read:
    /// known at once, where [`EngineFacts`] come only as the engine exits.
    pub(crate) fn buffers(&self) -> Option<SocketBuffers> {
        self.buffers
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Starts the engine for `data_phase` on `socket`, the Test Connection's
/// own socket, which nothing else reads from now on, over the platform's
/// adapter: or, in the `udpstp` binary's tests, over the faults
/// `UDPSTP_FAULTS` asks for. It reads the Control Phase's `clock`,
/// and runs as `settings` say.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn(
    socket_id: SocketId,
    data_phase: DataPhase,
    socket: UdpSocket,
    connected: bool,
    buffer_size: usize,
    clock: Clock,
    settings: EngineSettings,
    report: reports::Sender,
) -> io::Result<Engine> {
    #[cfg(feature = "fault-injection")]
    if let Some(faulty) = socket::faults::from_env() {
        return spawn_with(
            socket_id,
            data_phase,
            socket,
            connected,
            buffer_size,
            clock,
            settings,
            report,
            faulty,
        );
    }
    spawn_with(
        socket_id,
        data_phase,
        socket,
        connected,
        buffer_size,
        clock,
        settings,
        report,
        socket::Platform,
    )
}

/// The adapter recipe a Server's pooled executor opens its Test
/// Connections' sockets with: the platform's own, or, in the `udpstp`
/// binary's tests, one that injects the faults `UDPSTP_FAULTS` asks for,
/// and none when it's unset.
#[cfg(all(
    any(target_os = "linux", target_os = "android"),
    feature = "fault-injection"
))]
pub(crate) type ServerOpen = socket::faults::Faulty<socket::Platform>;
#[cfg(all(
    any(target_os = "linux", target_os = "android"),
    not(feature = "fault-injection")
))]
pub(crate) type ServerOpen = socket::Platform;

/// The recipe [`ServerOpen`] names.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub(crate) fn server_open() -> ServerOpen {
    #[cfg(feature = "fault-injection")]
    return socket::faults::from_env().unwrap_or_else(|| socket::faults::Faulty {
        inner: socket::Platform,
        faults: socket::faults::Faults::default(),
    });
    #[cfg(not(feature = "fault-injection"))]
    socket::Platform
}

/// As [`spawn`], over the adapter `open` opens on the engine's thread.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_with<O: Open>(
    socket_id: SocketId,
    mut data_phase: DataPhase,
    socket: UdpSocket,
    connected: bool,
    buffer_size: usize,
    clock: Clock,
    settings: EngineSettings,
    report: reports::Sender,
    open: O,
) -> io::Result<Engine> {
    data_phase.set_catch_up_budget(settings.catch_up_budget());
    let peer = data_phase.peer();
    let (buffers, connected) =
        socket::prepare(&socket, peer, connected, data_phase.dscp_ecn(), buffer_size)?;
    // What the receive hold-off is derived from.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let receive_buffer = buffers.map_or(buffer_size, |b| b.receive);
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = stop.clone();
    std::thread::Builder::new()
        .name(format!("udpstp-engine-{}", socket_id.0))
        .spawn(move || {
            let mut slot = None;
            // Around everything the thread does before it reports, the
            // socket's opening too, rather than each core call: a panic
            // anywhere retires this Test Connection only, and its end is
            // still reported, so its admission is released (ADR 0003).
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                // Timer slack of 1 ns (ADR 0008), so the kernel doesn't
                // defer this thread's wake-ups by its default 50 µs, half
                // the interval of a 100 µs Transmitter. It is per thread;
                // not getting it is no reason to stop the Test.
                #[cfg(target_os = "linux")]
                let _ = nix::sys::prctl::set_timerslack(1);
                let numbering = settings.send_stamps;
                let opening = Settings {
                    peer,
                    connected,
                    load_receiver: data_phase.is_load_receiver(),
                    rx_timestamps: true,
                    send_stamps: numbering.is_some(),
                    // Unless the settings ask for the kernel's own count
                    //
                    explicit_numbers: numbering != Some(Numbering::Kernel),
                    clock,
                };
                // Opened here: nix's header sets aren't Send.
                let socket = Socket::open(open, socket, &opening);
                #[cfg(any(target_os = "linux", target_os = "android"))]
                let readiness = Some(crate::wait::Readiness::new(
                    data_phase.is_load_receiver(),
                    receive_buffer,
                    settings.hold_off,
                ));
                let slot = slot.insert(Slot::new(socket_id, data_phase, socket, report.clone()));
                PerTestConnection {
                    slot,
                    times: Fresh(clock),
                    report: &report,
                    stop: stopping,
                    #[cfg(any(target_os = "linux", target_os = "android"))]
                    readiness,
                }
                .run()
            }));
            let (reason, error) = match outcome {
                Ok(Ok(())) => (slot.as_ref().and_then(Slot::ended), None),
                Ok(Err(error)) => (None, Some(error.to_string())),
                Err(_) => (None, Some("the Data Phase panicked".into())),
            };
            // What its socket says, unless it never opened.
            let facts = slot.as_ref().map_or(
                EngineFacts {
                    socket_buffers: buffers,
                    ..EngineFacts::default()
                },
                |slot| slot.facts(buffers),
            );
            drop(slot);
            report.send(FromEngine::Exited {
                socket: socket_id,
                reason,
                error,
                facts,
            });
        })?;
    Ok(Engine {
        stop: Stop::Flag(stop),
        buffers,
    })
}

/// The per-Test-Connection executor: a thread that owns one Test
/// Connection's slot, runs its turns and waits between them.
struct PerTestConnection<'a, A> {
    slot: &'a mut Slot<A>,
    /// Every reading a fresh clock pair, as this executor always read the
    /// clock.
    times: Fresh<Clock>,
    report: &'a reports::Sender,
    stop: Arc<AtomicBool>,
    /// Waiting on the socket between turns, while the kernel takes ppoll
    ///
    #[cfg(any(target_os = "linux", target_os = "android"))]
    readiness: Option<crate::wait::Readiness>,
}

impl<A: Adapter> PerTestConnection<'_, A> {
    fn run(&mut self) -> io::Result<()> {
        loop {
            let times = &self.times;
            if self.stop.load(Ordering::Relaxed) {
                self.slot.stop(times);
            }
            // For at most one SLICE (ADR 0012).
            let until = times.now().mono.as_duration() + SLICE;
            self.slot.send_due(times, until)?;
            // What already arrived counts before any timeout is judged, so
            // a late turn doesn't back off for a Status PDU that is waiting
            // in the socket buffer.
            self.slot.read(times)?;
            self.slot.timeouts(times);
            self.slot.transmit_status(times)?;
            if self.slot.forward_events(self.report) {
                return Ok(());
            }
            self.wait();
        }
    }

    /// Waits until the next deadline: on Linux in ppoll on the socket
    ///; elsewhere, and once ppoll turns out not to work, asleep, at
    /// most one slice.
    fn wait(&mut self) {
        let now = self.times.now();
        let deadlines = [self.slot.next_deadline()];
        let turn = self.slot.take_reads();
        #[cfg(any(target_os = "linux", target_os = "android"))]
        if let Some(mut readiness) = self.readiness.take() {
            // Unless ppoll turned out to be unusable: then asleep, now and
            // from now on.
            if self.wait_ready(&mut readiness, turn, now.mono, deadlines) {
                self.readiness = Some(readiness);
                return;
            }
        }
        let _ = turn;
        let wait = crate::wait::timeout(now.mono, deadlines, SLICE);
        if !wait.is_zero() {
            std::thread::sleep(wait);
        }
    }

    /// Waits on the socket until the next deadline, an arrival (unless the
    /// receive hold-off leaves POLLIN out) or a pending error, which it
    /// drains: whether ppoll works here (`wait.rs`).
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn wait_ready(
        &mut self,
        readiness: &mut crate::wait::Readiness,
        turn: crate::socket::Reads,
        now: udpstp_core::MonoTime,
        deadlines: [Option<udpstp_core::MonoTime>; 1],
    ) -> bool {
        use crate::wait::{BACKOFF_SLEEP, IDLE_CAP, timeout};
        let hold_off = readiness.after_turn(turn.datagrams, turn.bytes, now);
        let wait = timeout(now, deadlines, hold_off.unwrap_or(IDLE_CAP));
        if wait.is_zero() {
            return true;
        }
        if readiness.sleep_instead() {
            std::thread::sleep(wait.min(BACKOFF_SLEEP));
            return true;
        }
        let Some((woke, drained)) = self.slot.ready(&self.times, hold_off.is_none(), wait) else {
            return false;
        };
        readiness.woke(woke, drained);
        true
    }
}
