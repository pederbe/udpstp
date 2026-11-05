//! Running a program as a Round runs one: in a process group of its own,
//! its standard input the script it is given or nothing, its output
//! captured, within a time limit, and stopped when the Round is
//! interrupted.
//!
//! - **Its own process group** (`CREATE_NEW_PROCESS_GROUP` on Windows): the
//!   terminal's Ctrl-C reaches the tool alone, which raises its flag and
//!   stops what the Round runs itself, and never what its Round Teardown
//!   runs. It is a group, not a session: std's `setsid` is unstable, and
//!   `pre_exec` needs `unsafe`. On Windows a console's Ctrl-Break still
//!   reaches every process on it, the children too: a documented limit,
//!   since Rounds run from WSL or Linux.
//! - **Never the terminal:** standard input is the script, or null, so a
//!   child in a group other than the terminal's can't be stopped by
//!   SIGTTIN.
//! - **Nothing blocks unboundedly:** the child is polled every
//!   [`POLL`] at most, against the time limit and,
//!   until the Round Teardown, the flag. On either, the child's whole group
//!   is killed and the child reaped.
//! - **Readers are detached:** a killed child's output readers get
//!   [`GRACE`] more, then the call returns without them. A grandchild in a
//!   session of its own, such as an ssh ControlPersist master or a command
//!   started with `setsid`, may still hold the pipe; the only other way
//!   past it would be SIGKILL, which skips the Round Teardown. A detached
//!   reader keeps nothing more, and lets go of the pipe at its next read.
//!
//! - **Output is capped:** a call whose output passes [`OUTPUT_CAP`], its
//!   standard output and error together, is killed and fails, where the
//!   Python would hold whatever came (review S1).
//!
//! As Python's `subprocess.run(..., timeout=)` does, a call ends when the
//! child has exited and both its outputs are closed: a grandchild that
//! holds one open keeps the call waiting until the limit.

use std::fmt;
use std::io::{self, Read, Write};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use zeroize::Zeroizing;

use crate::interrupt::{Flag, POLL};

/// How long a killed child's readers, and its reaping, get before the call
/// returns without them.
pub const GRACE: Duration = Duration::from_secs(2);

/// The most output a call may give, its standard output and error
/// together: 1 GiB, far beyond a fetched run's directory, which a hostile
/// or broken host could otherwise make take this machine's memory.
pub const OUTPUT_CAP: u64 = 1 << 30;

/// Runs programs for a Round, stopping them when its [`Flag`] stops it.
/// Clones share the flag.
///
/// A runner is always given its flag: there is no default one, whose flag
/// nothing would raise (review S2).
///
/// ```compile_fail
/// let runner = bench_rounds::process::Runner::default();
/// ```
#[derive(Clone, Debug)]
pub struct Runner {
    flag: Flag,
    output_cap: u64,
}

/// A program that ran to its end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finished {
    /// Python's `returncode`: the exit status, or minus the signal that
    /// ended the program.
    pub returncode: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// A program that ran to its end whose standard output is a secret, such
/// as the credentials `aws configure export-credentials` prints: kept in
/// one buffer, allocated once at its full size and never reallocated, so
/// that no copy is left behind in freed memory, and wiped when dropped
/// (the design's §4). Its `Debug` shows the output's length only.
pub struct FinishedSecret {
    /// Python's `returncode`.
    pub returncode: i32,
    pub stdout: Zeroizing<Vec<u8>>,
    pub stderr: Vec<u8>,
}

impl fmt::Debug for FinishedSecret {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("FinishedSecret")
            .field("returncode", &self.returncode)
            .field(
                "stdout",
                &format_args!("<{} bytes, kept secret>", self.stdout.len()),
            )
            .field("stderr", &String::from_utf8_lossy(&self.stderr))
            .finish()
    }
}

/// Why a program didn't run to its end.
#[derive(Debug)]
pub enum RunError {
    /// The time limit passed: it was killed.
    TimedOut,
    /// The Round was interrupted: it was killed, or not started.
    Interrupted,
    /// It couldn't be started.
    Spawn(io::Error),
    /// Waiting for it failed: it was killed.
    Wait(io::Error),
    /// Its output passed `cap` bytes: it was killed.
    TooMuchOutput { cap: u64 },
}

impl Finished {
    /// Whether it exited with 0.
    pub fn success(&self) -> bool {
        self.returncode == 0
    }

    /// Its standard output as `subprocess.run(..., text=True)` gives it.
    pub fn stdout_text(&self) -> Result<String, std::str::Utf8Error> {
        text(&self.stdout)
    }

    /// Its standard error as `subprocess.run(..., text=True)` gives it.
    pub fn stderr_text(&self) -> Result<String, std::str::Utf8Error> {
        text(&self.stderr)
    }
}

/// Output as Python's `subprocess` gives it in text mode: UTF-8, which
/// Python refuses to decode otherwise (`UnicodeDecodeError`), with every
/// `\r\n` and every lone `\r` made `\n`.
pub fn text(bytes: &[u8]) -> Result<String, std::str::Utf8Error> {
    Ok(std::str::from_utf8(bytes)?
        .replace("\r\n", "\n")
        .replace('\r', "\n"))
}

/// [`text`] for a message: what isn't UTF-8 replaced, as Python's
/// `decode(errors="replace")` does.
pub fn lossy_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .replace("\r\n", "\n")
        .replace('\r', "\n")
}

impl Runner {
    /// A runner the flag stops, whose calls may give [`OUTPUT_CAP`] of
    /// output.
    pub fn new(flag: Flag) -> Runner {
        Runner {
            flag,
            output_cap: OUTPUT_CAP,
        }
    }

    /// The same runner, its calls' output capped at `bytes`.
    pub fn with_output_cap(mut self, bytes: u64) -> Runner {
        self.output_cap = bytes;
        self
    }

    /// The flag that stops what it runs.
    pub fn flag(&self) -> &Flag {
        &self.flag
    }

    /// Runs `command` with `stdin` as its standard input (null if `None`),
    /// its output captured, for at most `limit`. Its standard streams and
    /// process group are set here, whatever `command` had.
    ///
    /// Until the Round Teardown begins ([`Flag::stop_acting`]), a raised
    /// flag stops the call: nothing starts once it is raised, and a running
    /// child is killed within [`POLL`] and a little more.
    pub fn run(
        &self,
        command: &mut Command,
        stdin: Option<&[u8]>,
        limit: Duration,
    ) -> Result<Finished, RunError> {
        let (returncode, mut stdout, mut stderr) = self.call(command, stdin, limit, None)?;
        Ok(Finished {
            returncode,
            stdout: std::mem::take(&mut *stdout),
            stderr: std::mem::take(&mut *stderr),
        })
    }

    /// [`Runner::run`] for a program whose standard output is a secret:
    /// kept in one buffer of `capacity` bytes, allocated before the program
    /// starts and never reallocated, wiped when dropped, as is what the
    /// reader reads it through. More than `capacity` fails the call as
    /// [`RunError::TooMuchOutput`] (review m10).
    pub fn run_secret(
        &self,
        command: &mut Command,
        stdin: Option<&[u8]>,
        limit: Duration,
        capacity: usize,
    ) -> Result<FinishedSecret, RunError> {
        let (returncode, stdout, mut stderr) = self.call(command, stdin, limit, Some(capacity))?;
        Ok(FinishedSecret {
            returncode,
            stdout,
            stderr: std::mem::take(&mut *stderr),
        })
    }

    /// A call: its status, and its outputs as the readers kept them,
    /// standard output in a buffer of `secret` bytes if given.
    fn call(
        &self,
        command: &mut Command,
        stdin: Option<&[u8]>,
        limit: Duration,
        secret: Option<usize>,
    ) -> Result<(i32, Kept, Kept), RunError> {
        if self.flag.stops() {
            return Err(RunError::Interrupted);
        }
        let deadline = Instant::now().checked_add(limit);
        own_group(command);
        command
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(RunError::Spawn)?;
        let (done, readers) = mpsc::channel();
        let counted = Arc::new(AtomicU64::new(0));
        let started = (|| {
            let read = |pipe, fixed| {
                Capture::start(
                    pipe,
                    fixed,
                    done.clone(),
                    Arc::clone(&counted),
                    self.output_cap,
                )
            };
            let stdout = read(
                child
                    .stdout
                    .take()
                    .map(|p| Box::new(p) as Box<dyn Read + Send>),
                secret,
            )?;
            let stderr = read(
                child
                    .stderr
                    .take()
                    .map(|p| Box::new(p) as Box<dyn Read + Send>),
                None,
            )?;
            if let (Some(input), Some(pipe)) = (stdin, child.stdin.take()) {
                feed(pipe, input.to_vec())?;
            }
            Ok((stdout, stderr))
        })();
        drop(done);
        let (stdout, stderr) = match started {
            Ok(captures) => captures,
            Err(e) => {
                stop(&mut child, None);
                return Err(RunError::Spawn(e));
            }
        };

        let mut exited: Option<ExitStatus> = None;
        let mut closed = 0;
        let mut over = None;
        let mut pause = Duration::from_millis(1);
        loop {
            if exited.is_none() {
                match child.try_wait() {
                    Ok(status) => exited = status,
                    Err(e) => {
                        stop(&mut child, Some((&readers, closed)));
                        return Err(RunError::Wait(e));
                    }
                }
            }
            if let (Some(status), 2, None) = (exited, closed, over) {
                return Ok((returncode(status), stdout.take(), stderr.take()));
            }
            let now = Instant::now();
            if let Some(why) = stopping(&self.flag, over, deadline, now) {
                if exited.is_none() {
                    stop(&mut child, Some((&readers, closed)));
                } else {
                    // Reaped: its process group may be gone, and its number
                    // another's. What holds its output is past reach.
                    wait_for_readers(&readers, closed, now + GRACE);
                }
                return Err(why);
            }
            let wait = deadline.map_or(pause, |d| pause.min(d - now));
            if closed == 2 {
                // Both outputs closed: the child is exiting.
                thread::sleep(wait);
            } else {
                match readers.recv_timeout(wait) {
                    Ok(event) => {
                        closed += 1;
                        if let Event::Over(cap) = event {
                            over = Some(cap);
                        }
                        pause = Duration::from_millis(1);
                        continue;
                    }
                    Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {}
                }
            }
            pause = (pause * 2).min(POLL);
        }
    }
}

/// Why a running call must stop at `now`, if it must. An interrupt comes
/// first, so that one raised just before the limit is never reported as a
/// timeout, which a host would take for its own failure (review m1); then
/// output past its cap, `over`; then the limit.
fn stopping(
    flag: &Flag,
    over: Option<u64>,
    deadline: Option<Instant>,
    now: Instant,
) -> Option<RunError> {
    if flag.stops() {
        Some(RunError::Interrupted)
    } else if let Some(cap) = over {
        Some(RunError::TooMuchOutput { cap })
    } else if deadline.is_some_and(|d| now >= d) {
        Some(RunError::TimedOut)
    } else {
        None
    }
}

/// What a reader says when it stops: its pipe closed, or the call's output
/// passed its cap.
enum Event {
    Closed,
    Over(u64),
}

/// Kills `child`'s whole process group (on Windows, the child), reaps it
/// within [`GRACE`], and gives its readers what is left of that.
fn stop(child: &mut Child, readers: Option<(&mpsc::Receiver<Event>, usize)>) {
    let killed = Instant::now();
    kill(child);
    while killed.elapsed() < GRACE {
        match child.try_wait() {
            Ok(None) => thread::sleep(Duration::from_millis(5)),
            // Reaped, or no longer ours to reap.
            Ok(Some(_)) | Err(_) => break,
        }
    }
    if let Some((readers, closed)) = readers {
        wait_for_readers(readers, closed, killed + GRACE);
    }
}

/// Waits until both readers are done or `until`; after that they are
/// detached when the call returns.
fn wait_for_readers(readers: &mpsc::Receiver<Event>, mut closed: usize, until: Instant) {
    while closed < 2 {
        let left = until.saturating_duration_since(Instant::now());
        match readers.recv_timeout(left) {
            Ok(_) => closed += 1,
            Err(_) => return,
        }
    }
}

#[cfg(unix)]
fn own_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(windows)]
fn own_group(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    /// winbase.h's `CREATE_NEW_PROCESS_GROUP`.
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(CREATE_NEW_PROCESS_GROUP);
}

#[cfg(not(any(unix, windows)))]
fn own_group(_: &mut Command) {}

/// SIGKILL to the child's process group, which the child leads: the child
/// and whatever it started that stayed in the group. Called only before the
/// child is reaped, so the group's number is still the child's.
#[cfg(unix)]
fn kill(child: &mut Child) {
    use nix::sys::signal::{Signal, killpg};
    use nix::unistd::Pid;
    let killed = i32::try_from(child.id())
        .is_ok_and(|group| killpg(Pid::from_raw(group), Signal::SIGKILL).is_ok());
    if !killed {
        let _ = child.kill();
    }
}

#[cfg(not(unix))]
fn kill(child: &mut Child) {
    let _ = child.kill();
}

#[cfg(unix)]
fn returncode(status: ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .or_else(|| status.signal().map(|s| -s))
        .unwrap_or(-1)
}

#[cfg(not(unix))]
fn returncode(status: ExitStatus) -> i32 {
    status.code().unwrap_or(-1)
}

/// What a reader keeps of an output, wiped when dropped.
type Kept = Zeroizing<Vec<u8>>;

/// What a reader has read so far. When the call returns and drops it, a
/// reader still running is detached: it keeps nothing more and lets go of
/// the pipe at its next read, so a grandchild still writing to it fails
/// its write rather than filling this machine's memory (review m5).
struct Capture {
    buffer: Arc<Mutex<Kept>>,
    detached: Arc<AtomicBool>,
}

impl Capture {
    /// Reads `pipe` to its end in a thread of its own, then says so on
    /// `done`. It counts what it reads in `counted`, which the call's other
    /// reader shares, and stops, dropping the pipe, once that passes `cap`.
    /// With `fixed`, what it reads is kept in a buffer of that many bytes,
    /// allocated now and never grown: more is refused as too much output.
    fn start(
        pipe: Option<Box<dyn Read + Send>>,
        fixed: Option<usize>,
        done: Sender<Event>,
        counted: Arc<AtomicU64>,
        cap: u64,
    ) -> io::Result<Capture> {
        let buffer = match fixed {
            Some(capacity) => Vec::with_capacity(capacity),
            None => Vec::new(),
        };
        let capture = Capture {
            buffer: Arc::new(Mutex::new(Zeroizing::new(buffer))),
            detached: Arc::new(AtomicBool::new(false)),
        };
        let Some(mut pipe) = pipe else {
            let _ = done.send(Event::Closed);
            return Ok(capture);
        };
        let filled = Arc::clone(&capture.buffer);
        let detached = Arc::clone(&capture.detached);
        thread::Builder::new()
            .name("bench-rounds-output".into())
            .spawn(move || {
                // Wiped when the reader ends, as a secret passes through it.
                let mut chunk = Zeroizing::new([0u8; 16 * 1024]);
                let event = loop {
                    match pipe.read(&mut chunk[..]) {
                        Ok(0) => break Event::Closed,
                        Ok(_) if detached.load(Ordering::SeqCst) => break Event::Closed,
                        Ok(n) => {
                            let total = counted.fetch_add(n as u64, Ordering::SeqCst) + n as u64;
                            if total > cap {
                                break Event::Over(cap);
                            }
                            let mut kept = lock(&filled);
                            if let Some(capacity) = fixed
                                && kept.len() + n > kept.capacity()
                            {
                                break Event::Over(capacity as u64);
                            }
                            kept.extend_from_slice(&chunk[..n]);
                        }
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                        Err(_) => break Event::Closed,
                    }
                };
                drop(pipe);
                let _ = done.send(event);
            })?;
        Ok(capture)
    }

    /// What it kept, moved out, never copied.
    fn take(&self) -> Kept {
        Zeroizing::new(std::mem::take(&mut **lock(&self.buffer)))
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.detached.store(true, Ordering::SeqCst);
        // What a detached reader kept, freed now.
        drop(self.take());
    }
}

fn lock(buffer: &Mutex<Kept>) -> std::sync::MutexGuard<'_, Kept> {
    buffer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Writes `input` to the child's standard input in a thread of its own,
/// then closes it. A child that ends without reading it all fails the
/// write, which is no error; the thread is never waited for.
fn feed(mut pipe: impl Write + Send + 'static, input: Vec<u8>) -> io::Result<()> {
    thread::Builder::new()
        .name("bench-rounds-input".into())
        .spawn(move || {
            let _ = pipe.write_all(&input);
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_python_text_mode() {
        assert_eq!(text(b"a\r\nb\rc\n").unwrap(), "a\nb\nc\n");
        assert_eq!(text(b"\r\r\n").unwrap(), "\n\n");
        assert!(text(b"\xff").is_err());
        assert_eq!(lossy_text(b"x\xffy\r\n"), "x\u{fffd}y\n");
    }

    #[test]
    fn an_interrupt_is_never_reported_as_a_timeout() {
        let now = Instant::now();
        let past = Some(now - Duration::from_millis(1));
        let later = Some(now + Duration::from_secs(1));
        let flag = Flag::new();
        assert!(stopping(&flag, None, later, now).is_none());
        assert!(stopping(&flag, None, None, now).is_none());
        assert!(matches!(
            stopping(&flag, None, past, now),
            Some(RunError::TimedOut)
        ));
        // Too much output ahead of the limit.
        assert!(matches!(
            stopping(&flag, Some(7), past, now),
            Some(RunError::TooMuchOutput { cap: 7 })
        ));
        flag.raise("a test");
        // With either at once: the interrupt.
        assert!(matches!(
            stopping(&flag, None, past, now),
            Some(RunError::Interrupted)
        ));
        assert!(matches!(
            stopping(&flag, Some(7), later, now),
            Some(RunError::Interrupted)
        ));
        assert!(matches!(
            stopping(&flag, None, later, now),
            Some(RunError::Interrupted)
        ));
        // In the Round Teardown the flag stops nothing; the limit still does.
        let _begun = flag.stop_acting();
        assert!(matches!(
            stopping(&flag, None, past, now),
            Some(RunError::TimedOut)
        ));
        assert!(stopping(&flag, None, later, now).is_none());
    }

    #[test]
    fn nothing_starts_once_the_flag_stops_the_round() {
        let flag = Flag::new();
        flag.raise("a test");
        let runner = Runner::new(flag);
        // A program that doesn't exist: had it been started, the error
        // would be Spawn.
        let mut command = Command::new("bench-rounds-no-such-program");
        assert!(matches!(
            runner.run(&mut command, None, Duration::from_secs(1)),
            Err(RunError::Interrupted)
        ));
    }

    #[test]
    fn a_program_that_isnt_there_fails_to_start() {
        let mut command = Command::new("bench-rounds-no-such-program");
        assert!(matches!(
            Runner::new(Flag::new()).run(&mut command, None, Duration::from_secs(1)),
            Err(RunError::Spawn(_))
        ));
    }
}
