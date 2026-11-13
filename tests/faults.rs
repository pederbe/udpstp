//! Seam 2b: faults injected into the Data Phase Engine (docs/spec.md
//! Testing Decisions, "fault injection through a test-only feature"). The
//! tests' build of `udpstp` has the runtime's `fault-injection` feature,
//! so `UDPSTP_FAULTS` makes its engines panic, or fail sends or reads; see
//! src/runtime/socket/faults.rs. What the faults do to an
//! engine is tested in-process, without the binary
//! (src/runtime/fault_tests.rs); these keep what needs its
//! processes: a panic releasing the Server's Admission, and the faults
//! reaching the engines.

mod common;
#[path = "common/key_file.rs"]
mod key_file;

use std::io::{BufRead, BufReader};
use std::net::UdpSocket;
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use common::run_bounded;
use key_file::KeyFile;

fn operator_keys(test: &str) -> KeyFile {
    KeyFile::new(
        test,
        "keys.toml",
        "[[key]]\nadmin_key_name = \"ops\"\nkey_id = 3\nkey_text = \"operator\"\noperator = true\n",
    )
}

/// A free UDP port on IPv4 loopback a moment ago.
fn free_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0")
        .and_then(|s| s.local_addr())
        .expect("a free port")
        .port()
}

/// A Server on IPv4 loopback with a JSON log and `faults`, killed when
/// dropped.
struct Server {
    child: Child,
    port: u16,
    log: mpsc::Receiver<String>,
}

impl Server {
    fn start(keys: &KeyFile, faults: &str) -> Self {
        let port = free_port();
        let mut child = Command::new(env!("CARGO_BIN_EXE_udpstp"))
            .args([
                "server",
                "--listen",
                "127.0.0.1",
                "--port",
                &port.to_string(),
            ])
            .args(["--key-file", keys.path(), "--log-format", "json"])
            .env("UDPSTP_FAULTS", faults)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the Server starts");
        let stdout = child.stdout.take().expect("piped");
        let (lines, log) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = lines.send(line);
            }
        });
        let server = Self { child, port, log };
        server.log_until("listening", Duration::from_secs(10));
        server
    }

    /// The log up to the first line containing `text`, or all within
    /// `limit`.
    fn log_until(&self, text: &str, limit: Duration) -> String {
        let until = std::time::Instant::now() + limit;
        let mut out = String::new();
        while let Some(left) = until.checked_duration_since(std::time::Instant::now()) {
            let Ok(line) = self.log.recv_timeout(left) else {
                break;
            };
            out.push_str(&line);
            out.push('\n');
            if line.contains(text) {
                break;
            }
        }
        out
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A Fixed-Rate Test at row 5 (5 Mbit/s), with JSON results.
fn client(server: &Server, keys: &KeyFile, direction: &str, faults: &str) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_udpstp"));
    command
        .args([
            "client",
            "--key-file",
            keys.path(),
            "--direction",
            direction,
        ])
        .args([
            "--fixed-rate",
            "5",
            "--test-interval",
            "5",
            "--format",
            "json",
        ])
        .arg(format!("127.0.0.1:{}", server.port))
        .env("UDPSTP_FAULTS", faults);
    run_bounded(command, Duration::from_secs(30))
}

// docs/spec.md (the engine must release admission state on every exit path,
// including a panic; ADR 0003);
#[test]
fn an_engine_panic_releases_admission_and_the_server_keeps_serving() {
    let keys = operator_keys("fault-panic");
    let server = Server::start(&keys, "panic_after=20");
    // The first Test's engine panics mid-Test.
    let first = client(&server, &keys, "downstream", "");
    assert!(!first.status.success(), "the first Test fails");
    let log = server.log_until("TestConnectionReleased", Duration::from_secs(10));
    assert!(
        log.lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .any(|e| e["event"] == "DataPhaseEnded"
                && e["reason"] == "DataPhaseEngineFailed"
                && e["error"].is_string()),
        "the panic is logged: {log}"
    );
    // One Test per Source: a second is admitted only if the first was
    // released. It completes.
    let second = client(&server, &keys, "downstream", "");
    assert!(
        second.status.success(),
        "the Server keeps serving: {}",
        String::from_utf8_lossy(&second.stderr)
    );
}

// : UDPSTP_FAULTS reaches both ends' engines (the panic test shows the
// Server's): a Load Sender whose sends fail now and then, as a full send
// buffer fails them, withholds Bursts while its Deferred Datagrams wait
// (ADR 0008), and loses none. The in-process fault tests
// (src/runtime/fault_tests.rs) prove the engine's side; this
// one proves the process's.
#[test]
fn faults_from_the_environment_reach_the_engines() {
    let keys = operator_keys("fault-environment");
    let faults = "would_block_every=3,no_buffers_every=7";
    for direction in ["downstream", "upstream"] {
        // The Load Sender fails sends: the Server Downstream, the Client
        // Upstream.
        let (server_faults, client_faults) = match direction {
            "downstream" => (faults, ""),
            _ => ("", faults),
        };
        let server = Server::start(&keys, server_faults);
        let output = client(&server, &keys, direction, client_faults);
        assert!(
            output.status.success(),
            "{direction}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let results: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("JSON results");
        let lost: Vec<i64> = results["Output"]["IncrementalResult"]
            .as_array()
            .expect("Sub-Intervals")
            .iter()
            .filter_map(|s| s["LossCount"].as_i64())
            .collect();
        assert!(
            !lost.is_empty() && lost.iter().all(|&l| l == 0),
            "{direction}: no Sub-Interval lost a Load PDU: {lost:?}"
        );
        // The faults did hold sends back: the Load Sender withheld Bursts
        // while its Deferred Datagrams waited.
        let withheld = match direction {
            "downstream" => server
                .log_until("TestSummary", Duration::from_secs(10))
                .contains("\"BurstsWithheld\""),
            _ => results["udpstp"]["Warnings"]
                .as_array()
                .is_some_and(|w| w.iter().any(|w| w["Code"] == "BurstsWithheld")),
        };
        assert!(withheld, "{direction}: the injected faults held sends back");
        // : so a Downstream Test's Server sent well below its row, and
        // the Client rightly warns that the result may understate the
        // path. Upstream, the Client sends and says so itself, as above.
        let not_kept = results["udpstp"]["Warnings"]
            .as_array()
            .is_some_and(|w| w.iter().any(|w| w["Code"] == "SendingRateNotKept"));
        assert_eq!(
            not_kept,
            direction == "downstream",
            "{direction}: {}",
            results["udpstp"]
        );
    }
}

//  (ADR 0012): a Load Sender whose host sends slower than its
// schedule still ends the Test with its STOP, reporting what it couldn't
// send as Skipped Bursts; its send turn is bounded, so its timeouts run.
// : the Client warns that the Server didn't keep its row's rate.
#[test]
fn a_host_that_sends_slower_than_its_schedule_still_stops_the_test() {
    let keys = operator_keys("fault-slow");
    // Row 5 sends one Load PDU every 1 ms; each send takes 2 ms.
    let server = Server::start(&keys, "slow_send_us=2000");
    let output = client(&server, &keys, "downstream", "");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let results: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON results");
    assert_eq!(
        results["udpstp"]["EndReason"], "Completed",
        "the Server's STOP arrived: {}",
        results["udpstp"]
    );
    assert!(
        server
            .log_until("TestSummary", Duration::from_secs(10))
            .contains("\"BurstsSkipped\""),
        "the Server reported what it couldn't send as Skipped Bursts"
    );
    assert!(
        results["udpstp"]["Warnings"]
            .as_array()
            .is_some_and(|w| w.iter().any(|w| w["Code"] == "SendingRateNotKept")),
        "the Client warned of the rate the Server didn't keep: {}",
        results["udpstp"]
    );
}
