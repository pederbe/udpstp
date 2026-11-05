//! The runner with real processes on every platform (review m9): the test
//! binary starts itself again as the child, `BENCH_ROUNDS_CHILD` saying
//! what the child does, so the checks need neither bash nor a socket, and
//! run on Windows as on Linux. `tests/process.rs` holds the checks that
//! need a Unix shell: process groups, grandchildren, `setsid`.
//!
//! A test binary of its own (`harness = false`), since libtest has no way
//! to start one test's body as a child.

use std::io::{Read, Write};
use std::process::{Command, ExitCode};
use std::thread;
use std::time::{Duration, Instant};

use bench_rounds::interrupt::Flag;
use bench_rounds::process::{GRACE, RunError, Runner};

const CHILD: &str = "BENCH_ROUNDS_CHILD";
const MARK: &str = "BENCH_ROUNDS_MARK";
const SECRET: &[u8] = br#"{"Version": 1, "AccessKeyId": "ASIATESTTESTTESTTEST", "SecretAccessKey": "test/test/test", "SessionToken": "test", "Expiration": "2099-01-01T00:00:00+00:00"}"#;
const LIMIT: Duration = Duration::from_secs(30);

fn main() -> ExitCode {
    match std::env::var(CHILD) {
        Ok(what) => child(&what),
        Err(_) => checks(),
    }
}

// The child ------------------------------------------------------------------

fn child(what: &str) -> ExitCode {
    let mut out = std::io::stdout();
    match what {
        "output" => {
            let _ = out.write_all(b"out\r\nmore");
            let _ = std::io::stderr().write_all(b"err");
            let _ = out.flush();
            ExitCode::from(3)
        }
        // How many bytes its standard input held.
        "stdin" => {
            let mut input = Vec::new();
            let _ = std::io::stdin().read_to_end(&mut input);
            let _ = write!(out, "{}", input.len());
            let _ = out.flush();
            ExitCode::SUCCESS
        }
        "sleep" => {
            thread::sleep(Duration::from_secs(30));
            ExitCode::SUCCESS
        }
        // Given a line on its own standard input, runs the child that counts
        // its input through the runner, which gives it nothing, and says what
        // that child read; on Linux, also what its standard input is.
        "outer" => {
            let runner = Runner::new(Flag::new());
            let read = runner.run(&mut me("stdin"), None, LIMIT);
            let read = read.map(|done| String::from_utf8_lossy(&done.stdout).into_owned());
            let _ = writeln!(
                out,
                "inner read {}",
                read.unwrap_or_else(|e| format!("{e:?}"))
            );
            #[cfg(target_os = "linux")]
            {
                let fd0 = runner.run(&mut me("fd0"), None, LIMIT);
                let fd0 = fd0.map(|done| String::from_utf8_lossy(&done.stdout).into_owned());
                let _ = writeln!(
                    out,
                    "inner fd0 {}",
                    fd0.unwrap_or_else(|e| format!("{e:?}"))
                );
            }
            let _ = out.flush();
            ExitCode::SUCCESS
        }
        // What its standard input is.
        "fd0" => {
            let target = std::fs::read_link("/proc/self/fd/0");
            let _ = write!(
                out,
                "{}",
                target.map_or("?".into(), |t| t.display().to_string())
            );
            let _ = out.flush();
            ExitCode::SUCCESS
        }
        // Credentials, as `aws configure export-credentials` prints them.
        "secret" => {
            let _ = out.write_all(SECRET);
            let _ = out.flush();
            ExitCode::SUCCESS
        }
        // 3 MiB on standard output, then an end.
        "flood" => {
            let _ = out.write_all(&vec![b'x'; 3 << 20]);
            let _ = out.flush();
            ExitCode::SUCCESS
        }
        // A grandchild writing on the same output, out of the kill's reach
        // (a process group of its own on Unix; on Windows the kill takes
        // the child alone), then a wait.
        "hold" => {
            let mut writer = me("writer");
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                writer.process_group(0);
            }
            let _ = writer.spawn();
            thread::sleep(Duration::from_secs(30));
            ExitCode::SUCCESS
        }
        // A line every 100 ms until a write fails, which it marks in the
        // file MARK names; an end of itself after 30 s.
        "writer" => {
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(30) {
                if out.write_all(b"x\n").and_then(|()| out.flush()).is_err() {
                    if let Some(mark) = std::env::var_os(MARK) {
                        let _ = std::fs::write(mark, "the pipe closed");
                    }
                    return ExitCode::from(3);
                }
                thread::sleep(Duration::from_millis(100));
            }
            ExitCode::SUCCESS
        }
        _ => ExitCode::from(99),
    }
}

/// This binary, as the child that does `what`.
fn me(what: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().expect("this binary"));
    command.env(CHILD, what);
    command
}

// The checks -----------------------------------------------------------------

type Check = fn();

fn checks() -> ExitCode {
    let filters: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| !a.starts_with('-'))
        .collect();
    let all: [(&str, Check); 8] = [
        (
            "output_status_and_input_come_back",
            output_status_and_input_come_back,
        ),
        ("the_limit_ends_a_call", the_limit_ends_a_call),
        ("an_interrupt_ends_a_call", an_interrupt_ends_a_call),
        (
            "the_round_teardowns_calls_run_whatever_the_flag_says",
            the_round_teardowns_calls_run_whatever_the_flag_says,
        ),
        (
            "output_past_the_cap_ends_a_call",
            output_past_the_cap_ends_a_call,
        ),
        (
            "a_detached_reader_lets_go_of_the_pipe",
            a_detached_reader_lets_go_of_the_pipe,
        ),
        (
            "a_secret_is_kept_in_one_buffer_never_grown",
            a_secret_is_kept_in_one_buffer_never_grown,
        ),
        (
            "a_child_reads_nothing_it_isnt_given",
            a_child_reads_nothing_it_isnt_given,
        ),
    ];
    let mut failed = 0;
    for (name, check) in all {
        if !filters.is_empty() && !filters.iter().any(|f| name.contains(f.as_str())) {
            continue;
        }
        let outcome = std::panic::catch_unwind(check);
        println!(
            "test {name} ... {}",
            if outcome.is_ok() { "ok" } else { "FAILED" }
        );
        failed += usize::from(outcome.is_err());
    }
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        println!("{failed} failed");
        ExitCode::FAILURE
    }
}

fn output_status_and_input_come_back() {
    let runner = Runner::new(Flag::new());
    let done = runner.run(&mut me("output"), None, LIMIT).unwrap();
    assert_eq!(done.returncode, 3);
    assert_eq!(done.stdout, b"out\r\nmore");
    assert_eq!(done.stdout_text().unwrap(), "out\nmore");
    assert_eq!(done.stderr, b"err");
    let input = vec![b'x'; 1 << 20];
    let done = runner.run(&mut me("stdin"), Some(&input), LIMIT).unwrap();
    assert_eq!(
        (done.returncode, done.stdout.as_slice()),
        (0, &b"1048576"[..])
    );
}

fn the_limit_ends_a_call() {
    let start = Instant::now();
    let error = Runner::new(Flag::new())
        .run(&mut me("sleep"), None, Duration::from_millis(300))
        .unwrap_err();
    let took = start.elapsed();
    assert!(matches!(error, RunError::TimedOut), "{error:?}");
    // Killed, nothing holding its output: no wait for the readers.
    assert!(
        took < Duration::from_millis(300) + GRACE / 2,
        "took {took:?}"
    );
}

fn an_interrupt_ends_a_call() {
    let flag = Flag::new();
    let raiser = {
        let flag = flag.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(200));
            flag.raise("a test");
        })
    };
    let start = Instant::now();
    let error = Runner::new(flag)
        .run(&mut me("sleep"), None, LIMIT)
        .unwrap_err();
    let took = start.elapsed();
    assert!(matches!(error, RunError::Interrupted), "{error:?}");
    assert!(
        took < Duration::from_millis(200) + GRACE / 2,
        "took {took:?}"
    );
    raiser.join().unwrap();
}

/// Review S1: a call's output is capped, where the Python holds whatever
/// comes; the child ends of itself, and the call still fails.
fn output_past_the_cap_ends_a_call() {
    let capped = Runner::new(Flag::new()).with_output_cap(1 << 20);
    let error = capped.run(&mut me("flood"), None, LIMIT).unwrap_err();
    assert!(
        matches!(error, RunError::TooMuchOutput { cap } if cap == 1 << 20),
        "{error:?}"
    );
    // Under the cap, all of it.
    let roomy = Runner::new(Flag::new()).with_output_cap(4 << 20);
    let done = roomy.run(&mut me("flood"), None, LIMIT).unwrap();
    assert_eq!(done.stdout.len(), 3 << 20);
    assert_eq!(bench_rounds::process::OUTPUT_CAP, 1 << 30);
}

/// Review m5: a grandchild out of the kill's reach keeps writing to the
/// output after the call has let its readers go; they keep nothing more
/// and drop the pipe, so its next write fails, at once rather than never.
fn a_detached_reader_lets_go_of_the_pipe() {
    let dir = std::env::temp_dir().join(format!("bench-rounds-runner-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mark = dir.join("the-writer-failed");
    let flag = Flag::new();
    let raiser = {
        let flag = flag.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(300));
            flag.raise("a test");
        })
    };
    let mut hold = me("hold");
    hold.env(MARK, &mark);
    let start = Instant::now();
    let error = Runner::new(flag).run(&mut hold, None, LIMIT).unwrap_err();
    let took = start.elapsed();
    assert!(matches!(error, RunError::Interrupted), "{error:?}");
    // The writer held the output, so the readers had their grace.
    assert!(took >= GRACE, "took {took:?}");
    raiser.join().unwrap();
    let waiting = Instant::now();
    while !mark.exists() {
        assert!(
            waiting.elapsed() < Duration::from_secs(3),
            "the writer is still writing, {:?} after the call returned",
            waiting.elapsed()
        );
        thread::sleep(Duration::from_millis(20));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Review m10: a secret's output is kept in the one buffer allocated before
/// the program started, never grown, so no copy of it is left behind; more
/// than it holds fails the call; and its `Debug` doesn't show it.
fn a_secret_is_kept_in_one_buffer_never_grown() {
    let runner = Runner::new(Flag::new());
    let done = runner
        .run_secret(&mut me("secret"), None, LIMIT, 4096)
        .unwrap();
    assert_eq!(done.returncode, 0);
    assert_eq!(&done.stdout[..], SECRET);
    assert_eq!(done.stdout.capacity(), 4096, "reallocated");
    let shown = format!("{done:?}");
    assert!(!shown.contains("ASIATEST"), "{shown}");
    assert!(
        shown.contains(&format!("<{} bytes, kept secret>", SECRET.len())),
        "{shown}"
    );
    for (what, capacity) in [("secret", 16), ("flood", 4096)] {
        let error = runner
            .run_secret(&mut me(what), None, LIMIT, capacity)
            .unwrap_err();
        assert!(
            matches!(error, RunError::TooMuchOutput { cap } if cap == capacity as u64),
            "{what}: {error:?}"
        );
    }
}

/// Review m4: a child the runner starts without a script reads nothing,
/// even where the tool's own standard input holds something. The outer
/// child is given a line, so that inheriting would show: the inner child
/// would read it, and on Linux its standard input would be a pipe, not
/// `/dev/null`.
fn a_child_reads_nothing_it_isnt_given() {
    let mut outer = me("outer")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut given = outer.stdin.take().unwrap();
    given.write_all(b"a line\n").unwrap();
    drop(given);
    let mut said = String::new();
    outer
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut said)
        .unwrap();
    assert!(outer.wait().unwrap().success());
    assert!(said.lines().any(|l| l == "inner read 0"), "{said:?}");
    if cfg!(target_os = "linux") {
        assert!(said.lines().any(|l| l == "inner fd0 /dev/null"), "{said:?}");
    }
}

fn the_round_teardowns_calls_run_whatever_the_flag_says() {
    let flag = Flag::new();
    flag.raise("a test");
    let runner = Runner::new(flag.clone());
    assert!(matches!(
        runner.run(&mut me("output"), None, LIMIT),
        Err(RunError::Interrupted)
    ));
    let _begun = flag.stop_acting();
    let done = runner.run(&mut me("output"), None, LIMIT).unwrap();
    assert_eq!(done.returncode, 3);
    // Its limit still holds.
    assert!(matches!(
        runner.run(&mut me("sleep"), None, Duration::from_millis(300)),
        Err(RunError::TimedOut)
    ));
}
