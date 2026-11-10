//! How the Client and the Server stop early: on Ctrl-C or a signal
//! asking them to stop, and, for the Client, when a write to its standard
//! output fails.
//!
//! The Client:
//! - Unix: SIGINT, SIGTERM and SIGHUP, each unless the Client was started
//!   with it ignored (as `nohup` and `cmd &` do), which it then stays. They
//!   are blocked in every thread and taken by a thread of their own, so the
//!   Test Connections can send their STOP2 and the results so far be
//!   printed; then the Client dies of the same signal, so a shell sees 128
//!   plus its number (130 for Ctrl-C) as for any interrupted program. It
//!   does so even when the signal came once every Test Connection had
//!   ended: the JSON's ErrorStatus then reflects the Test.
//! - Windows: the console's Ctrl-C, Ctrl-Break, close and shutdown events;
//!   the Client then exits with [`crate::exit::INTERRUPTED`].
//!
//! Only while the Test runs is a signal taken this way: before it starts
//! (while a name resolves, say) and once it has ended, a signal ends the
//! Client at once, as it would any program. A second signal ends it at
//! once too.
//!
//! A write to standard output that fails (EPIPE, as when the output is
//! piped into `head` and that exits) is noticed then, not before: it stops
//! a running Test the same way, and the Client exits with
//! [`crate::exit::INTERRUPTED`] instead of a panic.
//! (conformance: client-interrupt)
//!
//! The Server stops on the same signals, and on Windows on the same
//! console events, from before it says it listens: [`ServerStop`].

use std::fmt;
use std::io::Write as _;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::Notify;

#[cfg(unix)]
use crate::names::stop_reason;

/// Set once a write to stdout failed: nothing more is written there.
static STDOUT_CLOSED: AtomicBool = AtomicBool::new(false);

/// Wakes a Test waiting on [`Interrupts::next`] when stdout closes.
static STDOUT_CLOSING: Notify = Notify::const_new();

/// Writes to stdout. A failed write is noted, not a panic, and stops a
/// Test that is running.
pub fn write_out(args: fmt::Arguments<'_>) {
    if STDOUT_CLOSED.load(Ordering::Relaxed) {
        return;
    }
    let mut stdout =
        anstream::AutoStream::new(std::io::stdout(), crate::color::stream_choice()).lock();
    if stdout
        .write_fmt(args)
        .and_then(|()| stdout.flush())
        .is_err()
    {
        STDOUT_CLOSED.store(true, Ordering::Relaxed);
        STDOUT_CLOSING.notify_one();
    }
}

/// Whether a write to stdout has failed.
pub fn stdout_closed() -> bool {
    STDOUT_CLOSED.load(Ordering::Relaxed)
}

/// Writes a line to stderr, ignoring a closed stderr rather than
/// panicking as `eprintln!` would.
pub fn write_err(args: fmt::Arguments<'_>) {
    let text = crate::terminal::wrap(&format!("{args}\n"), crate::terminal::stderr_width());
    let _ = anstream::AutoStream::new(std::io::stderr(), crate::color::stream_choice())
        .lock()
        .write_all(text.as_bytes());
}

/// `print!` through [`write_out`].
macro_rules! out {
    ($($t:tt)*) => {
        $crate::interrupt::write_out(format_args!($($t)*))
    };
}

/// `println!` through [`write_out`].
macro_rules! outln {
    ($($t:tt)*) => {
        $crate::interrupt::write_out(format_args!("{}", $crate::terminal::wrap(
            &format!("{}\n", format_args!($($t)*)), $crate::terminal::stdout_width()
        )))
    };
}

/// `eprintln!` through [`write_err`].
macro_rules! errln {
    ($($t:tt)*) => {
        $crate::interrupt::write_err(format_args!($($t)*))
    };
}

pub(crate) use {errln, out, outln};

/// What stopped the Test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interruption {
    /// Ctrl-C or a signal to stop: on Unix, which.
    #[cfg(unix)]
    Signal(nix::sys::signal::Signal),
    #[cfg(not(unix))]
    Signal,
    /// A write to standard output failed.
    StdoutClosed,
}

impl Interruption {
    /// Ends the process for this interruption, once the results so far are
    /// out: on Unix, by the same signal; otherwise with `status`. Only the
    /// main thread calls it, so flushing stdout can't wait on a lock the
    /// caller holds.
    pub fn exit(self, status: u8) -> ExitCode {
        #[cfg(unix)]
        if let Self::Signal(signal) = self {
            let _ = std::io::stdout().flush();
            die_of(signal);
        }
        ExitCode::from(status)
    }
}

/// Unblocks `signal` in this thread and raises it, whose default action
/// ends the process at once. Nothing is flushed: another thread may hold
/// stdout's lock in a blocked write.
#[cfg(unix)]
fn die_of(signal: nix::sys::signal::Signal) {
    use nix::sys::signal::SigSet;
    let mut set = SigSet::empty();
    set.add(signal);
    let _ = set.thread_unblock();
    let _ = nix::sys::signal::raise(signal);
}

/// Whether `signal` was ignored in the inherited mask.
/// Capture the mask once before installing the Client or Server's handlers.
/// `sigaction` would tell, but only through `unsafe`, which the workspace
/// forbids; Linux reports the ignored signals in `/proc`, and other Unix
/// systems through `ps`. Unknown, it is taken as not ignored.
#[cfg(unix)]
fn ignored(mask: u64, signal: nix::sys::signal::Signal) -> bool {
    let bit = signal as i32 - 1;
    (0..64).contains(&bit) && mask & (1 << bit) != 0
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn ignored_mask() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let hex = status.lines().find_map(|l| l.strip_prefix("SigIgn:"))?;
    u64::from_str_radix(hex.trim(), 16).ok()
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn ignored_mask() -> Option<u64> {
    let output = std::process::Command::new("ps")
        .args(["-o", "sigignore=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    let text = String::from_utf8(output.stdout).ok()?;
    let hex = text.trim().trim_start_matches("0x");
    u64::from_str_radix(hex, 16).ok()
}

/// The signals that stop a Client or Server on Unix, and their names.
#[cfg(unix)]
const SIGNALS: [(nix::sys::signal::Signal, &str); 3] = [
    (nix::sys::signal::Signal::SIGINT, stop_reason::SIGINT),
    (nix::sys::signal::Signal::SIGTERM, stop_reason::SIGTERM),
    (nix::sys::signal::Signal::SIGHUP, stop_reason::SIGHUP),
];

/// How far the Client's signal thread has got.
#[cfg(unix)]
mod state {
    /// A signal ends the Client at once.
    pub const UNARMED: u8 = 0;
    /// The Test runs: the first signal stops it.
    pub const ARMED: u8 = 1;
    /// The first signal came; another ends the Client at once.
    pub const TAKEN: u8 = 2;
}

#[cfg(unix)]
static STATE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(state::UNARMED);

/// The Client's interrupts, from [`listen`].
pub struct Interrupts {
    #[cfg(unix)]
    first: Option<tokio::sync::oneshot::Receiver<nix::sys::signal::Signal>>,
}

/// Starts taking the Client's interrupts. On Unix, call it before any
/// other thread starts, so that every thread leaves the signals to it.
pub fn listen() -> Interrupts {
    #[cfg(unix)]
    {
        use nix::sys::signal::SigSet;
        let mut set = SigSet::empty();
        let ignored_signals = ignored_mask().unwrap_or(0);
        for (signal, _) in SIGNALS {
            if !ignored(ignored_signals, signal) {
                set.add(signal);
            }
        }
        if set.thread_block().is_err() {
            // The signals keep their default action: they end the Client.
            return Interrupts { first: None };
        }
        let (sender, first) = tokio::sync::oneshot::channel();
        let spawned = std::thread::Builder::new()
            .name("udpstp-signals".into())
            .spawn(move || {
                let mut sender = Some(sender);
                while let Ok(signal) = set.wait() {
                    let taken = STATE.compare_exchange(
                        state::ARMED,
                        state::TAKEN,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    );
                    match (taken, sender.take()) {
                        (Ok(_), Some(sender)) => {
                            let _ = sender.send(signal);
                        }
                        // Before the Test, after it, or a second signal.
                        _ => die_of(signal),
                    }
                }
            });
        if spawned.is_err() {
            let _ = set.thread_unblock();
            return Interrupts { first: None };
        }
        Interrupts { first: Some(first) }
    }
    #[cfg(not(unix))]
    {
        Interrupts {}
    }
}

impl Interrupts {
    /// Waits for the first interruption of the running Test; from the
    /// first poll on, a signal stops the Test rather than the Client.
    pub async fn next(&mut self) -> Interruption {
        #[cfg(unix)]
        let _ = STATE.compare_exchange(
            state::UNARMED,
            state::ARMED,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
        let stdout = async {
            if !stdout_closed() {
                STDOUT_CLOSING.notified().await;
            }
            Interruption::StdoutClosed
        };
        tokio::select! {
            interruption = stdout => interruption,
            interruption = self.signal() => interruption,
        }
    }

    /// Once the Test has ended: a signal from now on ends the Client at
    /// once.
    pub fn disarm(&mut self) {
        #[cfg(unix)]
        let _ = STATE.compare_exchange(
            state::ARMED,
            state::UNARMED,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
    }

    #[cfg(unix)]
    async fn signal(&mut self) -> Interruption {
        match self.first.take() {
            Some(first) => match first.await {
                Ok(signal) => Interruption::Signal(signal),
                Err(_) => std::future::pending().await,
            },
            None => std::future::pending().await,
        }
    }

    /// The first console event. Its receivers are dropped with it, so a
    /// second event, or one before or after the Test, gets Windows'
    /// default handling, which ends the Client at once.
    #[cfg(windows)]
    async fn signal(&mut self) -> Interruption {
        match console::ConsoleEvents::new() {
            Some(mut events) => {
                events.recv().await;
                Interruption::Signal
            }
            None => std::future::pending().await,
        }
    }

    #[cfg(not(any(unix, windows)))]
    async fn signal(&mut self) -> Interruption {
        std::future::pending().await
    }
}

/// The console events that stop a Client or Server on Windows.
#[cfg(windows)]
mod console {
    use tokio::signal::windows::{
        CtrlBreak, CtrlC, CtrlClose, CtrlShutdown, ctrl_break, ctrl_c, ctrl_close, ctrl_shutdown,
    };

    use crate::names::stop_reason;

    pub struct ConsoleEvents {
        c: CtrlC,
        b: CtrlBreak,
        close: CtrlClose,
        shutdown: CtrlShutdown,
    }

    impl ConsoleEvents {
        /// Listens for all four; `None` if one can't be.
        pub fn new() -> Option<Self> {
            Some(Self {
                c: ctrl_c().ok()?,
                b: ctrl_break().ok()?,
                close: ctrl_close().ok()?,
                shutdown: ctrl_shutdown().ok()?,
            })
        }

        /// The next event, by name.
        pub async fn recv(&mut self) -> &'static str {
            tokio::select! {
                _ = self.c.recv() => stop_reason::CTRL_C,
                _ = self.b.recv() => stop_reason::CTRL_BREAK,
                _ = self.close.recv() => stop_reason::CTRL_CLOSE,
                _ = self.shutdown.recv() => stop_reason::CTRL_SHUTDOWN,
            }
        }
    }
}

/// What stops the Server, by the name its log's `Stopping` gives it:
/// SIGTERM, SIGINT or SIGHUP (each unless the Server started with it
/// ignored), or on Windows a console event. Handled rather than left to
/// their default because, as PID 1 in a container, a signal without a
/// handler is ignored, and so the Server stops its Tests and
/// reports them.
///
/// Its handlers are installed when it is made, not when the Server first
/// waits on it: made before the Server starts listening, it takes a signal
/// that comes as soon as the Server says it listens.
pub struct ServerStop {
    #[cfg(unix)]
    signals: Vec<(tokio::signal::unix::Signal, &'static str)>,
    #[cfg(windows)]
    events: Option<console::ConsoleEvents>,
}

impl ServerStop {
    /// Installs the handlers: a signal or console event from now on
    /// completes [`ServerStop::stopped`]. Call it in the context of the
    /// Tokio runtime that waits on them (`Runtime::enter`); on Unix, Tokio
    /// panics outside one.
    pub fn install() -> Self {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            let ignored_signals = ignored_mask().unwrap_or(0);
            let signals = SIGNALS
                .into_iter()
                .filter(|&(sig, _)| !ignored(ignored_signals, sig))
                .filter_map(|(sig, name)| {
                    let stream = signal(SignalKind::from_raw(sig as i32)).ok()?;
                    Some((stream, name))
                })
                .collect();
            Self { signals }
        }
        #[cfg(windows)]
        {
            Self {
                events: console::ConsoleEvents::new(),
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            Self {}
        }
    }

    /// The name of the first signal or console event since
    /// [`ServerStop::install`].
    pub async fn stopped(self) -> &'static str {
        #[cfg(unix)]
        {
            let mut signals = self.signals;
            if signals.is_empty() {
                return std::future::pending().await;
            }
            std::future::poll_fn(|cx| {
                for (stream, name) in &mut signals {
                    if stream.poll_recv(cx).is_ready() {
                        return std::task::Poll::Ready(*name);
                    }
                }
                std::task::Poll::Pending
            })
            .await
        }
        #[cfg(windows)]
        {
            match self.events {
                Some(mut events) => events.recv().await,
                None => std::future::pending().await,
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            std::future::pending().await
        }
    }
}

#[cfg(test)]
mod tests {
    // : the Server's console handlers are installed as its stop is
    // made, before it binds its Standard Port and says it listens, not
    // when it first waits on the stop; tokio installs one as it creates
    // its listener. A test can't send a console event without `unsafe`, so
    // on Unix the loopback tests prove the whole path with signals.
    #[cfg(windows)]
    #[test]
    fn the_servers_console_handlers_are_installed_as_its_stop_is_made() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime");
        let stop = {
            let _entered = runtime.enter();
            super::ServerStop::install()
        };
        assert!(stop.events.is_some(), "listening for the console events");
    }
}
