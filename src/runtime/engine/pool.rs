//! The Linux Server's pooled executor: Engine Threads, as many as
//! the CPUs the process may use, each serving many Test Connections from
//! one loop (`pool/thread.rs`), where the per-Test-Connection executor
//! runs a thread for each (`engine.rs`). Both drive the same Test
//! Connection slots (`slot.rs`).
//!
//! **How many, and where.** At most `EngineSettings::threads` Engine
//! Threads, else `std::thread::available_parallelism()`, read once as the
//! pool is made: on Linux the CPUs of the process's affinity mask, within a
//! cgroup CPU quota, so a Server confined with `taskset`, systemd's
//! `CPUAffinity=` or a container's CPU limit follows it. None is pinned
//! (the example systemd unit denies `sched_setaffinity`). They start
//! lazily: the first with the first Test Connection, the next only when
//! every one started serves at least one, so a Server with fewer Test
//! Connections than CPUs runs each alone. A new Test Connection goes to
//! the Engine Thread serving the fewest, and stays there
//! (`schedule::place`). Each is named `udpstp-eng-<index>`.
//!
//! **Handing over.** [`EnginePool::start`] makes the socket ready on the
//! Control Phase's thread, as the per-Test-Connection executor does, and
//! sends the Test Connection to its Engine Thread over a channel, writing
//! the Engine Thread's eventfd to wake it; stops go the same way. Nothing
//! the Control Phase does blocks an Engine Thread (ADR 0003).
//!
//! **Every end reported.** Each handover carries an [`ExitReporter`] from
//! the moment it is made: in the channel, then in the Engine Thread's slot.
//! Its `Drop` reports the Test Connection's end unless it was reported
//! already, so whatever happens (a panic in the Engine Thread's own loop, a
//! handover still queued when it dies, one sent to an Engine Thread that
//! has died) every Test Connection's end reaches the Control Phase, which
//! then releases its Admission (ADR 0003). A handover the channel refuses,
//! its Engine Thread having died, goes to another.
//!
//! **Receive Buffer Overflows** are read, where the process can read its
//! own UDP tables, off the Engine Threads: parsing `/proc/self/net/udp`
//! and `udp6` as a Test Connection ends would stall every other Test
//! Connection of its Engine Thread. The Engine Thread hands the ending
//! Test Connection's socket, out of its epoll set, to one thread of the
//! pool's, which reads the tables once for each batch of ends waiting,
//! and reports each end: after its last Data Phase event, as always.

use std::io;
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::protocol::{DataPhase, EndReason, SocketId};
use crate::runtime::engine::reports;
use nix::sys::epoll::{Epoll, EpollCreateFlags};
use nix::sys::eventfd::{EfdFlags, EventFd};

use super::schedule::{self, Load, Place};
use super::{Engine, EngineFacts, EngineSettings, FromEngine};
use crate::runtime::buffers::SocketBuffers;
use crate::runtime::clock::Clock;
use crate::runtime::socket::{self, Open};

mod thread;

/// What a Test Connection's end says when its Engine Thread failed before
/// it could report it.
const FAILED: &str = "the Data Phase Engine failed";

/// How long dropping the pool waits for its Engine Threads to end.
const JOIN_WAIT: Duration = crate::runtime::server::STOP_WAIT;

/// The pooled executor: the Engine Threads it started, and how it starts
/// more.
pub(crate) struct EnginePool<O> {
    threads: Vec<Started<O>>,
    /// The most that may run.
    max: usize,
    clock: Clock,
    settings: EngineSettings,
    report: reports::Sender,
    /// The adapter recipe each Test Connection's socket opens with.
    open: O,
    /// The key of the next Test Connection handed over.
    next_key: u64,
    /// The thread that reads Receive Buffer Overflows, once started.
    overflows: Option<(mpsc::Sender<Counting>, JoinHandle<()>)>,
}

/// An Engine Thread started.
struct Started<O> {
    starts: mpsc::Sender<Handover<O>>,
    stops: mpsc::Sender<u64>,
    shared: Arc<Shared>,
    join: Option<JoinHandle<()>>,
}

/// What the pool and one Engine Thread share.
pub(crate) struct Shared {
    /// Wakes the Engine Thread when it waits.
    wake: EventFd,
    /// The Test Connections handed to it and not yet ended.
    serving: AtomicUsize,
    /// Whether its thread still runs.
    alive: AtomicBool,
    /// Set as the pool is dropped: the Engine Thread returns.
    shutdown: AtomicBool,
    /// Its thread's id, once it runs, for tests to find it in /proc.
    #[cfg(test)]
    pub(crate) tid: std::sync::atomic::AtomicI32,
}

impl Shared {
    /// Wakes the Engine Thread, if it waits.
    fn wake(&self) {
        let _ = self.wake.write(1);
    }
}

/// Tells a Test Connection's Engine Thread to stop it, once.
pub(crate) struct Stopper {
    key: u64,
    stops: mpsc::Sender<u64>,
    shared: Arc<Shared>,
    sent: AtomicBool,
}

impl Stopper {
    pub(crate) fn stop(&self) {
        if !self.sent.swap(true, Ordering::Relaxed) && self.stops.send(self.key).is_ok() {
            self.shared.wake();
        }
    }
}

/// A Test Connection handed to an Engine Thread, with what opens it there.
pub(crate) struct Handover<O> {
    socket_id: SocketId,
    /// Tells it apart from every other Test Connection of the pool, for
    /// its stop.
    key: u64,
    data_phase: DataPhase,
    socket: UdpSocket,
    connected: bool,
    buffers: Option<SocketBuffers>,
    /// The receive buffer it got, which its share of a hold-off is judged
    /// by.
    receive_buffer: usize,
    open: O,
    reporter: ExitReporter,
}

/// Reports a Test Connection's end: as it ends, or, if it never gets to,
/// as it is dropped.
pub(crate) struct ExitReporter {
    report: reports::Sender,
    socket: SocketId,
    buffers: Option<SocketBuffers>,
    armed: bool,
}

impl ExitReporter {
    fn new(report: reports::Sender, socket: SocketId, buffers: Option<SocketBuffers>) -> Self {
        Self {
            report,
            socket,
            buffers,
            armed: true,
        }
    }

    /// Reports the end.
    pub(crate) fn exited(
        mut self,
        reason: Option<EndReason>,
        error: Option<String>,
        facts: EngineFacts,
    ) {
        self.armed = false;
        self.report.send(FromEngine::Exited {
            socket: self.socket,
            reason,
            error,
            facts,
        });
    }

    /// Reports nothing: the end goes another way.
    fn disarm(mut self) {
        self.armed = false;
    }

    /// The socket buffers it got, and nothing else known.
    pub(crate) fn plain_facts(&self) -> EngineFacts {
        EngineFacts {
            socket_buffers: self.buffers,
            ..EngineFacts::default()
        }
    }
}

impl Drop for ExitReporter {
    fn drop(&mut self) {
        // Reads nothing but its own plain fields: this may run as a panic
        // unwinds, where a second panic would abort the process.
        if self.armed {
            self.report.send(FromEngine::Exited {
                socket: self.socket,
                reason: None,
                error: Some(FAILED.into()),
                facts: EngineFacts {
                    socket_buffers: self.buffers,
                    ..EngineFacts::default()
                },
            });
        }
    }
}

/// An ending Test Connection whose Receive Buffer Overflows are read from
/// the process's UDP tables, off its Engine Thread.
pub(crate) struct Counting {
    reporter: ExitReporter,
    reason: Option<EndReason>,
    error: Option<String>,
    facts: EngineFacts,
    /// Its socket, out of its Engine Thread's epoll set: its row stays in
    /// the tables until this closes.
    socket: UdpSocket,
}

impl Counting {
    /// Reports the end with `overflows`.
    fn report(self, overflows: Option<u64>) {
        let facts = EngineFacts {
            receive_buffer_overflows: overflows,
            ..self.facts
        };
        self.reporter.exited(self.reason, self.error, facts);
    }
}

/// Test-only hooks into an Engine Thread's own wait and loop, which the
/// fault adapter's recipe gives (`socket::faults`): none elsewhere.
#[derive(Debug, Clone, Default)]
pub(crate) struct Hooks {
    /// Every ppoll call fails with ENOSYS.
    pub(crate) ppoll_refused: bool,
    /// Every ppoll call returns at once with the epoll set readable, and
    /// nothing to harvest.
    pub(crate) epoll_stuck: bool,
    /// Counts the ppoll calls made, injected or not.
    pub(crate) polls: Option<Arc<AtomicU64>>,
    /// The first Engine Thread to have opened this many Test Connections
    /// panics in its own loop, outside every Test Connection's work, once:
    /// the flag says whether one has.
    pub(crate) panic_after_opens: Option<(u64, Arc<AtomicBool>)>,
}

impl<O: Open + Clone> EnginePool<O> {
    /// A pool whose Test Connections read `clock`, run as `settings` say,
    /// report through `report`, and open their sockets with `open`: the
    /// number of Engine Threads it may start, or why it couldn't find the
    /// CPUs the process may use, when it then starts at most one.
    pub(crate) fn new(
        clock: Clock,
        settings: EngineSettings,
        report: reports::Sender,
        open: O,
    ) -> (Self, Result<usize, io::Error>) {
        let allowed = settings
            .threads
            .map_or_else(std::thread::available_parallelism, Ok);
        let max = allowed.as_ref().map_or(1, |n| n.get());
        let pool = Self {
            threads: Vec::new(),
            max,
            clock,
            settings,
            report,
            open,
            next_key: 0,
            overflows: None,
        };
        (pool, allowed.map(|n| n.get()))
    }

    /// Hands Test Connection `socket_id`'s Data Phase to an Engine Thread,
    /// on `socket`, its own socket, which nothing else reads from now on.
    /// The socket is made ready here, as the per-Test-Connection executor
    /// makes it (`socket::prepare`), whose failure this returns; from then
    /// on the Test Connection's end is reported through the report channel,
    /// however it ends.
    pub(crate) fn start(
        &mut self,
        socket_id: SocketId,
        data_phase: DataPhase,
        socket: UdpSocket,
        connected: bool,
        buffer_size: usize,
    ) -> io::Result<Engine> {
        let open = self.open.clone();
        self.start_with(socket_id, data_phase, socket, connected, buffer_size, open)
    }

    /// As [`EnginePool::start`], its socket opening with `open`.
    pub(crate) fn start_with(
        &mut self,
        socket_id: SocketId,
        mut data_phase: DataPhase,
        socket: UdpSocket,
        connected: bool,
        buffer_size: usize,
        open: O,
    ) -> io::Result<Engine> {
        data_phase.set_catch_up_budget(self.settings.catch_up_budget());
        let peer = data_phase.peer();
        let (buffers, connected) =
            socket::prepare(&socket, peer, connected, data_phase.dscp_ecn(), buffer_size)?;
        let key = self.next_key;
        self.next_key += 1;
        let mut handover = Handover {
            socket_id,
            key,
            data_phase,
            socket,
            connected,
            buffers,
            receive_buffer: buffers.map_or(buffer_size, |b| b.receive),
            open,
            reporter: ExitReporter::new(self.report.clone(), socket_id, buffers),
        };
        loop {
            let loads: Vec<Load> = self
                .threads
                .iter()
                .map(|started| Load {
                    serving: started.shared.serving.load(Ordering::Relaxed),
                    alive: started.shared.alive.load(Ordering::Relaxed),
                })
                .collect();
            let index = match schedule::place(&loads, self.max) {
                Place::On(index) => index,
                Place::Start => match self.spawn() {
                    Ok(index) => index,
                    Err(error) => {
                        // Reported as the per-Test-Connection executor's
                        // failure to start is: by the caller.
                        handover.reporter.disarm();
                        return Err(error);
                    }
                },
            };
            let started = &self.threads[index];
            started.shared.serving.fetch_add(1, Ordering::Relaxed);
            match started.starts.send(handover) {
                Ok(()) => {
                    started.shared.wake();
                    let stopper = Stopper {
                        key,
                        stops: started.stops.clone(),
                        shared: started.shared.clone(),
                        sent: AtomicBool::new(false),
                    };
                    return Ok(Engine::pooled(stopper, buffers));
                }
                // It has died: to another.
                Err(mpsc::SendError(back)) => {
                    started.shared.serving.fetch_sub(1, Ordering::Relaxed);
                    started.shared.alive.store(false, Ordering::Relaxed);
                    handover = back;
                }
            }
        }
    }

    /// Starts an Engine Thread: its index.
    fn spawn(&mut self) -> io::Result<usize> {
        let index = self.threads.len();
        let epoll = Epoll::new(EpollCreateFlags::EPOLL_CLOEXEC).map_err(io::Error::from)?;
        let wake = EventFd::from_flags(EfdFlags::EFD_CLOEXEC | EfdFlags::EFD_NONBLOCK)
            .map_err(io::Error::from)?;
        let shared = Arc::new(Shared {
            wake,
            serving: AtomicUsize::new(0),
            alive: AtomicBool::new(true),
            shutdown: AtomicBool::new(false),
            #[cfg(test)]
            tid: std::sync::atomic::AtomicI32::new(0),
        });
        let (starts, started) = mpsc::channel();
        let (stops, stopped) = mpsc::channel();
        let context = thread::Context {
            shared: shared.clone(),
            epoll,
            starts: started,
            stops: stopped,
            clock: self.clock,
            settings: self.settings,
            report: self.report.clone(),
            overflows: self.overflow_reader(),
            hooks: self.open.pool_hooks(),
        };
        let join = std::thread::Builder::new()
            .name(format!("udpstp-eng-{index}"))
            .spawn(move || thread::run(context))?;
        self.threads.push(Started {
            starts,
            stops,
            shared,
            join: Some(join),
        });
        Ok(index)
    }

    /// Where Engine Threads send the Test Connections whose Receive Buffer
    /// Overflows are read from the process's UDP tables: none if its
    /// thread couldn't start, when they read them themselves.
    fn overflow_reader(&mut self) -> Option<mpsc::Sender<Counting>> {
        if self.overflows.is_none() {
            let (jobs, waiting) = mpsc::channel();
            let join = std::thread::Builder::new()
                .name("udpstp-overflow".into())
                .spawn(move || count_overflows(&waiting))
                .ok()?;
            self.overflows = Some((jobs, join));
        }
        self.overflows.as_ref().map(|(jobs, _)| jobs.clone())
    }

    /// The Engine Threads started, for tests.
    #[cfg(test)]
    pub(crate) fn started(&self) -> usize {
        self.threads.len()
    }

    /// The thread ids of the Engine Threads started and running, for tests.
    #[cfg(test)]
    pub(crate) fn thread_ids(&self) -> Vec<i32> {
        self.threads
            .iter()
            .map(|started| started.shared.tid.load(Ordering::Relaxed))
            .filter(|&tid| tid != 0)
            .collect()
    }

    /// How many Engine Threads it may start.
    #[cfg(test)]
    pub(crate) fn max(&self) -> usize {
        self.max
    }
}

impl<O> Drop for EnginePool<O> {
    /// Tells every Engine Thread to return, and waits for them, 2 s at
    /// most: one that hasn't returned by then is left behind, as a
    /// per-Test-Connection executor's thread is.
    fn drop(&mut self) {
        for started in &self.threads {
            started.shared.shutdown.store(true, Ordering::Relaxed);
            started.shared.wake();
        }
        let until = Instant::now() + JOIN_WAIT;
        for started in &mut self.threads {
            if let Some(join) = started.join.take() {
                while !join.is_finished() && Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(1));
                }
                if join.is_finished() {
                    let _ = join.join();
                }
            }
        }
        // The overflow reader returns once the Engine Threads' senders and
        // this one are gone, having reported what it was given.
        self.overflows = None;
    }
}

/// The overflow reader: each batch of ends waiting, the process's UDP
/// tables read once, each end reported with its count.
fn count_overflows(waiting: &mpsc::Receiver<Counting>) {
    let mut batch = Vec::new();
    while let Ok(first) = waiting.recv() {
        batch.push(first);
        batch.extend(waiting.try_iter());
        let counts = socket::listed_overflows(batch.iter().map(|counting| &counting.socket));
        for (counting, count) in batch.drain(..).zip(counts) {
            counting.report(count);
        }
    }
}
