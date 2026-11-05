//! The runner with real processes, on Linux: its own process group, nothing on standard
//! input but the script, the time limit and the flag, the whole group
//! killed, and a killed child's readers detached after about 2 s, so that
//! a grandchild holding stdout can't hang an interrupted call.
#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use bench_rounds::interrupt::Flag;
use bench_rounds::process::{GRACE, RunError, Runner};
use bench_rounds::quote::quote;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

const LIMIT: Duration = Duration::from_secs(30);

fn bash(script: &str) -> Command {
    let mut command = Command::new("bash");
    command.args(["-c", script]);
    command
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "bench-rounds-process-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn quoted(path: &Path) -> String {
    quote(path.to_str().unwrap()).into_owned()
}

fn read_pid(file: &Path) -> Pid {
    let start = Instant::now();
    loop {
        if let Ok(text) = std::fs::read_to_string(file)
            && let Ok(pid) = text.trim().parse()
        {
            return Pid::from_raw(pid);
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "no PID in {}",
            file.display()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

/// Whether `pid` has ended (gone, or a zombie) within `limit`.
fn ends_within(pid: Pid, limit: Duration) -> bool {
    let start = Instant::now();
    loop {
        let ended = match std::fs::read_to_string(format!("/proc/{pid}/status")) {
            Err(_) => true,
            Ok(text) => text
                .lines()
                .find_map(|l| l.strip_prefix("State:"))
                .is_some_and(|s| s.trim_start().starts_with('Z')),
        };
        if ended {
            return true;
        }
        if start.elapsed() >= limit {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// Raises `flag` after `after`, from another thread.
fn raise_after(flag: &Flag, after: Duration) -> thread::JoinHandle<()> {
    let flag = flag.clone();
    thread::spawn(move || {
        thread::sleep(after);
        flag.raise("a test");
    })
}

#[test]
fn output_status_and_input_come_back_as_python_gives_them() {
    let runner = Runner::new(Flag::new());
    let done = runner
        .run(&mut bash("echo out; echo err >&2; exit 3"), None, LIMIT)
        .unwrap();
    assert_eq!(
        (
            done.stdout.as_slice(),
            done.stderr.as_slice(),
            done.returncode
        ),
        (&b"out\n"[..], &b"err\n"[..], 3)
    );
    assert!(!done.success());
    let mut shell = Command::new("bash");
    shell.arg("-s");
    let done = runner
        .run(&mut shell, Some(b"echo from stdin; exit 4"), LIMIT)
        .unwrap();
    assert_eq!(
        (done.stdout.as_slice(), done.returncode),
        (&b"from stdin\n"[..], 4)
    );
    // Ended by a signal: Python's returncode is minus its number.
    let done = runner.run(&mut bash("kill -9 $$"), None, LIMIT).unwrap();
    assert_eq!(done.returncode, -9);
}

/// No child reads the terminal (§6): its standard input is `/dev/null`.
/// Where this test's own standard input is `/dev/null` too, as under
/// measure-lock, inheriting it would look the same: tests/runner.rs gives
/// the tool a line on its standard input to tell them apart (review m4).
#[test]
fn a_child_reads_nothing_but_its_script() {
    let done = Runner::new(Flag::new())
        .run(
            &mut bash(
                r#"readlink /proc/self/fd/0; if read -r line; then echo "read $line"; else echo eof; fi"#,
            ),
            None,
            LIMIT,
        )
        .unwrap();
    assert_eq!(done.stdout, b"/dev/null\neof\n");
}

#[test]
fn a_child_leads_a_process_group_of_its_own() {
    let done = Runner::new(Flag::new())
        .run(
            &mut bash("echo $$; awk '{ print $5 }' /proc/$$/stat"),
            None,
            LIMIT,
        )
        .unwrap();
    let text = String::from_utf8(done.stdout).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], lines[1], "its group is its own: {text:?}");
    let ours = std::fs::read_to_string("/proc/self/stat").unwrap();
    let our_group = ours
        .rsplit(')')
        .next()
        .unwrap()
        .split_whitespace()
        .nth(2)
        .unwrap();
    assert_ne!(lines[1], our_group);
}

#[test]
fn much_output_and_input_pass_whole() {
    let input = vec![b'x'; 1 << 20];
    let done = Runner::new(Flag::new())
        .run(
            &mut bash("wc -c; head -c 3000000 /dev/zero; head -c 200000 /dev/zero >&2"),
            Some(&input),
            LIMIT,
        )
        .unwrap();
    assert!(done.stdout.starts_with(b"1048576\n"));
    assert_eq!(done.stdout.len(), 8 + 3_000_000);
    assert_eq!(done.stderr.len(), 200_000);
}

#[test]
fn the_limit_kills_the_whole_group() {
    let dir = scratch("limit");
    let background = dir.join("background.pid");
    let script = format!("sleep 30 & echo $! > {}; wait", quoted(&background));
    let start = Instant::now();
    let error = Runner::new(Flag::new())
        .run(&mut bash(&script), None, Duration::from_millis(300))
        .unwrap_err();
    assert!(matches!(error, RunError::TimedOut), "{error:?}");
    // Its group killed, nothing held its output: no wait for the readers.
    let took = start.elapsed();
    assert!(
        took < Duration::from_millis(300) + GRACE / 2,
        "took {took:?}"
    );
    assert!(
        ends_within(read_pid(&background), Duration::from_secs(1)),
        "the background sleep outlived its group"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn an_interrupt_stops_a_command_at_once() {
    let flag = Flag::new();
    let runner = Runner::new(flag.clone());
    let raiser = raise_after(&flag, Duration::from_millis(200));
    let start = Instant::now();
    let error = runner.run(&mut bash("sleep 30"), None, LIMIT).unwrap_err();
    assert!(matches!(error, RunError::Interrupted), "{error:?}");
    let took = start.elapsed();
    assert!(
        took < Duration::from_millis(200) + GRACE / 2,
        "took {took:?}"
    );
    raiser.join().unwrap();
}

/// A grandchild in a session of its own (as an ssh ControlPersist master,
/// or a command under `setsid`) keeps stdout open after its parent is
/// killed; the call goes on without its readers about 2 s later (review
/// N1).
#[test]
fn a_grandchild_holding_stdout_doesnt_hang_an_interrupted_call() {
    for (name, waits) in [("running", "wait"), ("exited", "echo started")] {
        let dir = scratch(name);
        let holder = dir.join("holder.pid");
        let script = format!(
            "setsid sh -c 'echo $$ > {}; exec sleep 30' & {waits}",
            quoted(&holder)
        );
        let flag = Flag::new();
        let runner = Runner::new(flag.clone());
        let raiser = raise_after(&flag, Duration::from_millis(300));
        let start = Instant::now();
        let error = runner.run(&mut bash(&script), None, LIMIT).unwrap_err();
        let took = start.elapsed();
        assert!(matches!(error, RunError::Interrupted), "{name}: {error:?}");
        assert!(
            took >= GRACE,
            "{name}: gave up on the readers early: {took:?}"
        );
        assert!(
            took < Duration::from_millis(300) + GRACE + Duration::from_secs(2),
            "{name}: took {took:?}"
        );
        raiser.join().unwrap();
        let holder = read_pid(&holder);
        assert!(
            !ends_within(holder, Duration::ZERO),
            "{name}: the holder was reachable after all"
        );
        kill(holder, Signal::SIGKILL).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

/// The same holder, and the limit rather than the flag.
#[test]
fn a_grandchild_holding_stdout_doesnt_hang_a_call_past_its_limit() {
    let dir = scratch("limit-holder");
    let holder = dir.join("holder.pid");
    let script = format!(
        "setsid sh -c 'echo $$ > {}; exec sleep 30' & echo started",
        quoted(&holder)
    );
    let start = Instant::now();
    let error = Runner::new(Flag::new())
        .run(&mut bash(&script), None, Duration::from_millis(300))
        .unwrap_err();
    let took = start.elapsed();
    assert!(matches!(error, RunError::TimedOut), "{error:?}");
    assert!(
        took < Duration::from_millis(300) + GRACE + Duration::from_secs(2),
        "took {took:?}"
    );
    kill(read_pid(&holder), Signal::SIGKILL).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Output past the call's cap kills the whole group and fails the call promptly.
#[test]
fn output_past_the_cap_kills_the_call() {
    let dir = scratch("cap");
    let after = dir.join("after.pid");
    let script = format!(
        "echo $$ > {}; head -c 3000000 /dev/zero; sleep 30",
        quoted(&after)
    );
    let start = Instant::now();
    let error = Runner::new(Flag::new())
        .with_output_cap(1 << 20)
        .run(&mut bash(&script), None, LIMIT)
        .unwrap_err();
    let took = start.elapsed();
    assert!(
        matches!(error, RunError::TooMuchOutput { cap } if cap == 1 << 20),
        "{error:?}"
    );
    assert!(took < GRACE, "took {took:?}");
    // The PID is complete before the output can trip the cap.
    assert!(ends_within(read_pid(&after), Duration::from_secs(1)));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The review's open question 1: a child that opens the terminal
/// (`/dev/tty`) and reads it, from a process group other than the
/// terminal's. Ignored, since it needs a controlling terminal: run it in
/// tmux, `cargo test -p bench-rounds --test process -- --ignored
/// --nocapture reads_the_terminal`, and under `env
/// --default-signal=TTIN,TTOU` too. It prints what it saw.
///
/// Seen in WSL on 2026-09-29, tmux 3.6: tmux starts its panes with SIGTTIN
/// and SIGTTOU ignored, which the child inherits, so its read fails at
/// once with EIO. With both at their default, as an interactive shell
/// starts a job, SIGTTIN stops the child (`T`) until the call's limit
/// kills it: a stall as long as the limit, or until an interrupt, never a
/// hang. Either way the call ends within its limit.
#[test]
#[ignore = "needs a controlling terminal: run it in tmux"]
fn a_child_that_reads_the_terminal() {
    assert!(
        std::fs::File::open("/dev/tty").is_ok(),
        "no controlling terminal: run it in tmux"
    );
    let own = std::fs::read_to_string("/proc/self/status").unwrap();
    for line in own
        .lines()
        .filter(|l| l.starts_with("SigIgn") || l.starts_with("SigBlk"))
    {
        println!("this test's {line}");
    }
    let dir = scratch("tty");
    let pid_file = dir.join("pid");
    let script = format!(
        "echo $$ > {}; grep -E '^Sig(Blk|Ign)' /proc/$$/status >&2; \
         ps -o pid=,ppid=,pgid=,sid=,tpgid=,stat= -p $$ -p $PPID >&2; \
         exec 3</dev/tty; read -r x <&3; echo \"read: $?\"",
        quoted(&pid_file)
    );
    let started = Instant::now();
    let call = thread::spawn(move || {
        Runner::new(Flag::new()).run(&mut bash(&script), None, Duration::from_secs(5))
    });
    let pid = read_pid(&pid_file);
    thread::sleep(Duration::from_millis(500));
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
    let state = stat
        .rsplit(')')
        .next()
        .unwrap_or("")
        .split_whitespace()
        .next();
    println!("the child's state 0.5 s in: {state:?}");
    let outcome = call.join().unwrap();
    let took = started.elapsed();
    match &outcome {
        Ok(done) => println!(
            "the call finished after {took:?}: exit {}\n{}{}",
            done.returncode,
            String::from_utf8_lossy(&done.stdout),
            String::from_utf8_lossy(&done.stderr)
        ),
        Err(e) => println!("the call failed after {took:?}: {e:?}"),
    }
    assert!(
        took < Duration::from_secs(5) + GRACE,
        "the call outlived its limit: {took:?}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// In the Round Teardown the flag stops nothing, and the limit still
/// holds.
#[test]
fn the_round_teardowns_commands_run_whatever_the_flag_says() {
    let flag = Flag::new();
    flag.raise("a test");
    let _begun = flag.stop_acting();
    let runner = Runner::new(flag);
    let done = runner
        .run(&mut bash("sleep 0.3; echo done"), None, LIMIT)
        .unwrap();
    assert_eq!(done.stdout, b"done\n");
    let error = runner
        .run(&mut bash("sleep 30"), None, Duration::from_millis(300))
        .unwrap_err();
    assert!(matches!(error, RunError::TimedOut), "{error:?}");
}
