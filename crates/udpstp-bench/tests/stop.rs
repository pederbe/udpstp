//! Stopping the first Round's Servers when the Round never got to read a
//! Server's PID: an interrupt
//! in the second after a Server starts, before its PID file is read, must
//! still leave no Server running where its host answers. A final sweep
//! also catches starts and stops whose commands failed or were interrupted.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bench_rounds::host::{Completed, Limit, RemotePath, Shell, Stop};
use bench_rounds::interrupt::{Flag, Interrupted};
use bench_rounds::process::Runner;
use bench_rounds::provider::UnixTime;
use bench_rounds::round::{Clock, Log, RoundBody, Stopped};
use bench_rounds::secrets::SecretDir;
use indexmap::IndexMap;
use udpstp_bench::first_round::matrix;
use udpstp_bench::first_round::round::{FirstRound, Obudpst, Options, make_keys};

/// A host that answers as a healthy one would, but that its Servers'
/// starts fail where `start_fails`, and records its calls.
struct Healthy {
    role: String,
    start_fails: bool,
    panic_teardown: bool,
    calls: Arc<Mutex<Vec<String>>>,
}

impl Shell for Healthy {
    fn work(&self) -> &RemotePath {
        static WORK: std::sync::OnceLock<RemotePath> = std::sync::OnceLock::new();

        WORK.get_or_init(|| RemotePath::new("/work").unwrap())
    }
    fn role(&self) -> &str {
        &self.role
    }

    fn address(&self) -> &str {
        "10.0.0.5"
    }

    fn public_address(&self) -> Option<&str> {
        None
    }

    fn destination(&self) -> &str {
        "bench@testhost"
    }

    fn run(&self, script: &str, _: Limit) -> Result<Completed, Stop> {
        self.calls.lock().unwrap().push(script.to_string());
        if self.panic_teardown {
            if script == "kill 4242" {
                panic!("a stop panicked");
            }
            if script == STRAYS {
                if self.role == "server" {
                    panic!("a sweep panicked");
                }
                return Ok(Completed {
                    returncode: 0,
                    stdout: "4242\n".into(),
                    stderr: String::new(),
                });
            }
        }
        let (returncode, stdout) = if script.starts_with("cat server-udpstp.pid") {
            (0, "4242\n")
        } else if script.starts_with("cat server-obudpst.pid") {
            (0, "4343\n")
        } else if script.starts_with("kill -0 ") {
            (0, "alive\n")
        } else if script.contains("&& setsid -f ") && self.start_fails {
            (1, "")
        } else if script.starts_with("dev=$(ip") {
            (0, "9001\n")
        } else {
            (0, "")
        };
        Ok(Completed {
            returncode,
            stdout: stdout.into(),
            stderr: String::new(),
        })
    }

    fn put(&self, _: &Path, _: &RemotePath, _: u32) -> Result<(), Stop> {
        Ok(())
    }

    fn output(&self, _: &str, _: Limit) -> Result<Vec<u8>, Stop> {
        Ok(Vec::new())
    }
}

/// A clock whose `nth` wait of a second (from 1) is interrupted, as a
/// Ctrl-C then would.
struct InterruptedAt {
    flag: Flag,
    nth: usize,
    seconds: Mutex<usize>,
}

impl Clock for InterruptedAt {
    fn now(&self) -> UnixTime {
        1_790_000_000.0
    }

    fn sleep(&self, duration: Duration) -> Result<(), Interrupted> {
        if duration == Duration::from_secs(1) {
            let mut seconds = self.seconds.lock().unwrap();
            *seconds += 1;
            if *seconds == self.nth {
                self.flag.raise("SIGINT");
            }
        }
        self.flag.sleep(Duration::ZERO)
    }

    fn pause(&self, _: Duration) {}
}

/// A Round of `matrix` on healthy hosts, interrupted at the `nth` wait of a
/// second: how its steps ended, and the Round Teardown's calls.
fn stopped(matrix_path: &str, nth: usize, start_fails: bool) -> (Result<(), Stopped>, Vec<String>) {
    let (ended, stopping, _) = stopped_faults(matrix_path, nth, start_fails, false);
    (ended, stopping)
}

fn stopped_faults(
    matrix_path: &str,
    nth: usize,
    start_fails: bool,
    panic_teardown: bool,
) -> (Result<(), Stopped>, Vec<String>, Vec<String>) {
    let dir = std::env::temp_dir().join(format!(
        "udpstp-bench-stop-{nth}-{start_fails}-{panic_teardown}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("out").join("tests")).unwrap();
    let keys = SecretDir::create_in(&dir).unwrap();
    let key = make_keys(&keys).unwrap();
    let text =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(matrix_path)).unwrap();
    let runs = matrix::parse(&text).unwrap();
    let flag = Flag::new();
    let clock = InterruptedAt {
        flag: flag.clone(),
        nth,
        seconds: Mutex::new(0),
    };
    let calls: Arc<Mutex<Vec<String>>> = Arc::default();
    let hosts: IndexMap<String, Box<dyn Shell>> = ["server", "client"]
        .iter()
        .map(|role| {
            let host = Healthy {
                role: role.to_string(),
                start_fails,
                panic_teardown,
                calls: calls.clone(),
            };
            (role.to_string(), Box::new(host) as Box<dyn Shell>)
        })
        .collect();
    let options = Options {
        measurement_tool: None,
        udpstp: "/build/udpstp".into(),
        udpstp_home: None,
        obudpst: Some(Obudpst::Binary("/build/udpst".into())),
        raise_buffers: false,
        udpstp_port: 24601,
        obudpst_port: 25000,
        home_cap_mbps: 250,
        out: dir.join("out"),
        ssh_dir: None,
        obudpst_script: udpstp_bench::obudpst::script(),
    };
    let mut body = FirstRound::new(options, runs, vec![], &clock, Runner::new(flag.clone()));
    let ended = body.steps(
        hosts,
        false,
        &key,
        keys.path(),
        1_790_007_000.0,
        &Log::kept(),
    );
    let before = calls.lock().unwrap().len();
    let log = Log::kept();
    body.stop_servers(&flag.stop_acting(), &log);
    let stopping = calls.lock().unwrap()[before..].to_vec();
    drop(keys);
    std::fs::remove_dir_all(&dir).unwrap();
    (ended, stopping, log.lines())
}

#[test]
fn a_server_whose_pid_was_never_read_is_still_stopped() {
    let (ended, stopping) = stopped("../../scripts/bench/tests/smoke.toml", 1, false);
    assert_eq!(ended, Err(Stopped::Interrupted));
    assert_eq!(
        stopping,
        ["cat server-udpstp.pid", "kill 4242", STRAYS, STRAYS]
    );
}

/// With both engines' Servers, the second's PID unread: the first is
/// stopped by the PID it read, the second by its own file's, never the
/// other's.
#[test]
fn each_server_is_stopped_by_its_own_pid_file() {
    let (ended, stopping) = stopped("../../scripts/bench/tests/smoke-interop.toml", 2, false);
    assert_eq!(ended, Err(Stopped::Interrupted));
    assert_eq!(
        stopping,
        [
            "kill 4343",
            "cat server-udpstp.pid",
            "kill 4242",
            STRAYS,
            STRAYS
        ]
    );
}

/// A failed start can already have launched its remote Server; sweep even
/// where there is no recorded PID.
#[test]
fn a_server_that_didnt_start_isnt_stopped() {
    let (ended, stopping) = stopped("../../scripts/bench/tests/smoke.toml", 1, true);
    assert!(matches!(ended, Err(Stopped::Failed(_))), "{ended:?}");
    assert_eq!(stopping, [STRAYS, STRAYS]);
}

const STRAYS: &str = udpstp_bench::scale::hosts::STRAYS_SCRIPT;

#[test]
fn a_panicking_stop_and_sweep_do_not_skip_the_other_host() {
    let (ended, stopping, logs) =
        stopped_faults("../../scripts/bench/tests/smoke.toml", 1, false, true);
    assert_eq!(ended, Err(Stopped::Interrupted));
    assert_eq!(
        stopping,
        ["cat server-udpstp.pid", "kill 4242", STRAYS, STRAYS]
    );
    assert_eq!(
        logs,
        [
            "server: the stray sweep failed; check its work directory by hand: panicked: a sweep panicked",
            "client: killed what the round left running: ['4242']",
        ]
    );
}
