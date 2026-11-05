//! Signal checks on Linux: signals only raise the flag; a tool
//! started with SIGINT, SIGTERM and SIGHUP blocked or ignored still takes
//! them, and its children start with them unblocked and at their default
//! dispositions; so a child the runner started, and a Server started the
//! Round's way (`setsid -f`, `round.py:359`), end on SIGTERM.
//!
//! A test binary of its own (`harness = false`): its `main` installs the
//! handlers first, as a Round's `main` does, before any thread starts.
//! Started again with `BENCH_ROUNDS_PROBE` set, it is the probe the checks
//! start: a tool that installs its handlers first, whatever it was started
//! with, then does what the check asks. The control probe installs nothing,
//! to show that the checks see the fault the handlers mend.
//!
//! Elsewhere it checks only that the handlers install: the checks read
//! Linux's `/proc`, and a test can't send a console event without `unsafe`.

use std::process::ExitCode;

fn main() -> ExitCode {
    #[cfg(target_os = "linux")]
    {
        linux::main()
    }
    #[cfg(not(target_os = "linux"))]
    {
        match bench_rounds::interrupt::install() {
            Ok(_) => {
                println!("test the_handlers_install ... ok");
                ExitCode::SUCCESS
            }
            Err(e) => {
                println!("test the_handlers_install ... FAILED: {e}");
                ExitCode::FAILURE
            }
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::io::{BufRead, BufReader};
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, ExitCode, Stdio};
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    use bench_rounds::interrupt::{self, Flag};
    use bench_rounds::process::Runner;
    use bench_rounds::quote::quote;
    use nix::sys::signal::{SigSet, SigmaskHow, Signal, kill};
    use nix::unistd::Pid;

    /// SIGHUP (1), SIGINT (2) and SIGTERM (15) in a mask of
    /// `/proc/PID/status`.
    const THREE: u64 = 1 << 0 | 1 << 1 | 1 << 14;

    const PROBE: &str = "BENCH_ROUNDS_PROBE";
    const PROBE_DIR: &str = "BENCH_ROUNDS_PROBE_DIR";

    pub fn main() -> ExitCode {
        let probe = std::env::var(PROBE).ok();
        // First, before any thread starts, as a Round's main; the control
        // probe alone does without.
        let flag = if probe.as_deref() == Some("control") {
            Flag::new()
        } else {
            match interrupt::install() {
                Ok(flag) => flag,
                Err(e) => {
                    eprintln!("installing the handlers: {e}");
                    return ExitCode::FAILURE;
                }
            }
        };
        match probe {
            Some(mode) => probe_main(&mode, flag),
            None => checks(flag),
        }
    }

    // The checks ---------------------------------------------------------

    type Check = fn(&Flag);

    fn checks(flag: Flag) -> ExitCode {
        let filters: Vec<String> = std::env::args()
            .skip(1)
            .filter(|a| !a.starts_with('-'))
            .collect();
        let all: [(&str, Check); 6] = [
            ("signals_only_raise_the_flag", signals_only_raise_the_flag),
            ("a_tool_started_with_the_signals_blocked_takes_them", |_| {
                takes_them(Start::Blocked)
            }),
            ("a_tool_started_with_the_signals_ignored_takes_them", |_| {
                takes_them(Start::Ignored)
            }),
            (
                "a_tool_started_with_the_signals_blocked_and_ignored_takes_them",
                |_| takes_them(Start::BlockedAndIgnored),
            ),
            ("its_runner_child_and_setsid_server_stop_on_sigterm", |_| {
                for start in [Start::Blocked, Start::Ignored] {
                    children_stop(start);
                }
            }),
            ("without_the_handlers_they_would_outlive_sigterm", |_| {
                for start in [Start::Blocked, Start::Ignored] {
                    control(start);
                }
            }),
        ];
        let mut failed = 0;
        for (name, check) in all {
            if !filters.is_empty() && !filters.iter().any(|f| name.contains(f.as_str())) {
                continue;
            }
            let outcome = std::panic::catch_unwind(|| check(&flag));
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

    /// A SIGTERM to the process raises the flag and ends nothing; so do
    /// SIGINT and SIGHUP.
    fn signals_only_raise_the_flag(flag: &Flag) {
        let me = Pid::this();
        for (n, signal) in [Signal::SIGTERM, Signal::SIGINT, Signal::SIGHUP]
            .into_iter()
            .enumerate()
        {
            let before = flag.count();
            kill(me, signal).unwrap();
            let start = Instant::now();
            while flag.count() == before {
                assert!(
                    start.elapsed() < Duration::from_secs(5),
                    "{signal} raised nothing"
                );
                thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(flag.count(), n as u64 + 1);
        }
        assert_eq!(flag.cause(), Some("SIGTERM"));
        // Still here, and still taking them.
        assert!(flag.raised() && flag.stops());
    }

    /// What the tool inherits from what starts it.
    #[derive(Clone, Copy, Debug)]
    enum Start {
        /// The three blocked, as a launcher that blocks them leaves them.
        Blocked,
        /// The three ignored, as `nohup` or `cmd &` leave some of them.
        Ignored,
        BlockedAndIgnored,
    }

    /// The probe installs the handlers first: then nothing is blocked or
    /// ignored in it, all three are caught, and its child starts with the
    /// three unblocked and at their default dispositions. And a SIGTERM
    /// raises its flag.
    fn takes_them(start: Start) {
        let mut probe = Probe::start("report", start);
        let own_line = probe.expect("self ");
        let own = masks(&own_line);
        assert_eq!(own.blocked & THREE, 0, "{start:?}: {own:?}");
        assert_eq!(own.ignored & THREE, 0, "{start:?}: {own:?}");
        assert_eq!(own.caught & THREE, THREE, "{start:?}: {own:?}");
        let child_line = probe.expect("child ");
        println!("  started {start:?}: the probe's {own_line}; its {child_line}");
        let child = masks(&child_line);
        assert_eq!(child.blocked, 0, "{start:?}: {child:?}");
        assert_eq!(child.ignored & THREE, 0, "{start:?}: {child:?}");
        probe.expect("ready");
        kill(probe.pid(), Signal::SIGTERM).unwrap();
        assert_eq!(probe.expect("raised "), "raised 1 SIGTERM");
        assert!(probe.finish().success());
    }

    /// A child the runner started and a Server started the Round's way end
    /// at once on SIGTERM.
    fn children_stop(start: Start) {
        let mut probe = Probe::start("stop", start);
        let (child, server) = pids(&probe.expect("ready "));
        for pid in [child, server] {
            kill(pid, Signal::SIGTERM).unwrap();
        }
        for pid in [child, server] {
            assert!(
                ends_within(pid, Duration::from_secs(2)),
                "{start:?}: {pid} outlived SIGTERM"
            );
        }
        assert_eq!(probe.expect("child-exit "), "child-exit -15", "{start:?}");
        assert!(probe.finish().success());
    }

    /// The same probe without the handlers: the same children outlive
    /// SIGTERM, as the design measured (§3.9), and are killed.
    fn control(start: Start) {
        let mut probe = Probe::start("control", start);
        let (child, server) = pids(&probe.expect("ready "));
        for pid in [child, server] {
            kill(pid, Signal::SIGTERM).unwrap();
        }
        for pid in [child, server] {
            assert!(
                !ends_within(pid, Duration::from_secs(1)),
                "{start:?}: {pid} ended: the check sees nothing"
            );
        }
        for pid in [child, server] {
            kill(pid, Signal::SIGKILL).unwrap();
        }
        assert_eq!(probe.expect("child-exit "), "child-exit -9", "{start:?}");
        assert!(probe.finish().success());
    }

    // The probe ----------------------------------------------------------

    fn probe_main(mode: &str, flag: Flag) -> ExitCode {
        let dir = PathBuf::from(std::env::var_os(PROBE_DIR).expect("the probe's directory"));
        match mode {
            "report" => report(&flag),
            "stop" | "control" => start_children(&dir, &flag),
            _ => panic!("no probe {mode}"),
        }
        ExitCode::SUCCESS
    }

    /// Its own masks, its child's, then waits for a signal.
    fn report(flag: &Flag) {
        println!(
            "self {}",
            status_masks(&std::fs::read_to_string("/proc/self/status").unwrap())
        );
        let mut grep = Command::new("grep");
        grep.args(["^Sig", "/proc/self/status"]);
        let done = Runner::new(flag.clone())
            .run(&mut grep, None, Duration::from_secs(10))
            .unwrap();
        println!(
            "child {}",
            status_masks(&String::from_utf8(done.stdout).unwrap())
        );
        println!("ready");
        let start = Instant::now();
        while !flag.raised() && start.elapsed() < Duration::from_secs(20) {
            thread::sleep(Duration::from_millis(10));
        }
        println!(
            "raised {} {}",
            flag.count(),
            flag.cause().unwrap_or("nothing")
        );
    }

    /// A child through the runner, and a Server the Round's way, through
    /// bash as a local host runs a script; their PIDs; then how the child
    /// ended.
    fn start_children(dir: &Path, flag: &Flag) {
        let bash = |script: String| {
            let mut bash = Command::new("bash");
            bash.args(["-c", &script]);
            bash
        };
        let child_pid = dir.join("child.pid");
        let script = format!(
            "echo $$ > {}; exec sleep 30",
            quote(child_pid.to_str().unwrap())
        );
        let runner = Runner::new(flag.clone());
        let child =
            thread::spawn(move || runner.run(&mut bash(script), None, Duration::from_secs(60)));
        // As round.py:359 starts a Server.
        let server = Runner::new(flag.clone())
            .run(
                &mut bash(format!(
                    "cd {} && setsid -f sh -c 'echo $$ > server.pid; exec sleep 30' > server.log 2>&1 < /dev/null",
                    quote(dir.to_str().unwrap())
                )),
                None,
                Duration::from_secs(30),
            )
            .unwrap();
        assert!(server.success(), "{server:?}");
        let child = (child, read_pid(&child_pid));
        let server = read_pid(&dir.join("server.pid"));
        println!("ready {} {server}", child.1);
        let done = child.0.join().unwrap().unwrap();
        println!("child-exit {}", done.returncode);
    }

    fn read_pid(file: &Path) -> i32 {
        let start = Instant::now();
        loop {
            if let Ok(text) = std::fs::read_to_string(file)
                && let Ok(pid) = text.trim().parse()
            {
                return pid;
            }
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "no PID in {}",
                file.display()
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    // Helpers -------------------------------------------------------------

    /// A probe, and its lines as they come.
    struct Probe {
        child: Child,
        lines: mpsc::Receiver<String>,
        dir: PathBuf,
    }

    impl Probe {
        fn start(mode: &str, start: Start) -> Probe {
            let dir = std::env::temp_dir().join(format!(
                "bench-rounds-interrupt-{}-{mode}-{start:?}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let exe = std::env::current_exe().unwrap();
            let mut command = match start {
                Start::Blocked => Command::new(&exe),
                Start::Ignored | Start::BlockedAndIgnored => {
                    let mut bash = Command::new("bash");
                    bash.args(["-c", "trap '' INT TERM HUP; exec \"$0\""])
                        .arg(&exe);
                    bash
                }
            };
            command
                .env(PROBE, mode)
                .env(PROBE_DIR, &dir)
                .stdin(Stdio::null())
                .stdout(Stdio::piped());
            let blocked = matches!(start, Start::Blocked | Start::BlockedAndIgnored);
            // Blocked in this thread while the probe starts: std passes the
            // mask on (§3.9).
            let mut three = SigSet::empty();
            for signal in [Signal::SIGINT, Signal::SIGTERM, Signal::SIGHUP] {
                three.add(signal);
            }
            let before = blocked.then(|| three.thread_swap_mask(SigmaskHow::SIG_BLOCK).unwrap());
            let spawned = command.spawn();
            if let Some(before) = before {
                before.thread_set_mask().unwrap();
            }
            let mut child = spawned.unwrap();
            let stdout = child.stdout.take().unwrap();
            let (send, lines) = mpsc::channel();
            thread::spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    if send.send(line).is_err() {
                        break;
                    }
                }
            });
            Probe { child, lines, dir }
        }

        fn pid(&self) -> Pid {
            Pid::from_raw(self.child.id() as i32)
        }

        /// The next line, which must start with `prefix`.
        fn expect(&mut self, prefix: &str) -> String {
            let line = self
                .lines
                .recv_timeout(Duration::from_secs(20))
                .unwrap_or_else(|e| panic!("no line starting {prefix:?}: {e}"));
            assert!(line.starts_with(prefix), "{line:?}, not {prefix:?}");
            line
        }

        fn finish(mut self) -> std::process::ExitStatus {
            let start = Instant::now();
            loop {
                if let Some(status) = self.child.try_wait().unwrap() {
                    let _ = std::fs::remove_dir_all(&self.dir);
                    return status;
                }
                assert!(
                    start.elapsed() < Duration::from_secs(20),
                    "the probe didn't end"
                );
                thread::sleep(Duration::from_millis(10));
            }
        }
    }

    impl Drop for Probe {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    #[derive(Debug)]
    struct Masks {
        blocked: u64,
        ignored: u64,
        caught: u64,
    }

    /// `SigBlk`, `SigIgn` and `SigCgt` of `/proc/PID/status`, on one line.
    fn status_masks(status: &str) -> String {
        let field = |name: &str| {
            status
                .lines()
                .find_map(|l| l.strip_prefix(name))
                .map_or("0", str::trim)
                .to_owned()
        };
        format!(
            "SigBlk={} SigIgn={} SigCgt={}",
            field("SigBlk:"),
            field("SigIgn:"),
            field("SigCgt:")
        )
    }

    fn masks(line: &str) -> Masks {
        let field = |name: &str| {
            let hex = line
                .split_whitespace()
                .find_map(|w| w.strip_prefix(name))
                .unwrap_or_else(|| panic!("no {name} in {line:?}"));
            u64::from_str_radix(hex, 16).unwrap()
        };
        Masks {
            blocked: field("SigBlk="),
            ignored: field("SigIgn="),
            caught: field("SigCgt="),
        }
    }

    fn pids(line: &str) -> (Pid, Pid) {
        let mut words = line.split_whitespace().skip(1);
        let mut next = || Pid::from_raw(words.next().unwrap().parse().unwrap());
        (next(), next())
    }

    /// Whether `pid` has ended (gone, or a zombie) within `limit`.
    fn ends_within(pid: Pid, limit: Duration) -> bool {
        let start = Instant::now();
        loop {
            let status = std::fs::read_to_string(format!("/proc/{pid}/status"));
            let ended = match status {
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
}
