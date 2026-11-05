//! Client colors at S2a and S2b: the process keeps human progress and
//! diagnostics on their established destinations and JSON unstyled.

mod common;
#[path = "common/styling.rs"]
mod styling;

use std::io::{BufRead, BufReader};
use std::net::UdpSocket;
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use common::run_bounded;
use styling::plain;

fn unanswered(format: &str, color: &str) -> Output {
    // A bound socket cannot reply or be taken by another process.
    let server = UdpSocket::bind("127.0.0.1:0").expect("unused Server address");
    let mut command = Command::new(env!("CARGO_BIN_EXE_udpstp"));
    command
        .args([
            "client",
            "--unauthenticated",
            "--time-limit",
            "1",
            "--format",
            format,
            "--color",
            color,
            &server.local_addr().unwrap().to_string(),
        ])
        .env_remove("NO_COLOR")
        .env_remove("CLICOLOR")
        .env_remove("CLICOLOR_FORCE");
    run_bounded(command, Duration::from_secs(10))
}

// The Client color policy preserves ADR 0013's streams: human progress
// stays on stdout, failures on stderr, and a failed outcome is red.
#[test]
fn a_timeout_colors_progress_and_failure_on_their_existing_streams() {
    let output = unanswered("text", "always");
    assert_eq!(output.status.code(), Some(200));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stdout.contains("\x1b[38;2;92;184;255m"), "{stdout}");
    assert!(plain(&stdout).contains("▶ Sending"), "{stdout}");
    assert!(plain(&stdout).contains("Setup Request"), "{stdout}");
    assert!(stderr.contains("\x1b[1m\x1b[38;2;255;92;87m"), "{stderr}");
    assert!(stderr.contains("no answer from"), "{stderr}");
    assert!(!stdout.contains("no answer from"), "{stdout}");
}

// ADR 0013 and the Client color policy: JSON alone remains on stdout,
// even with forced colors; progress and the failed outcome use stderr.
#[test]
fn json_is_unstyled_while_its_progress_and_diagnostics_are_colored() {
    let output = unanswered("json", "always");
    assert_eq!(output.status.code(), Some(200));
    assert!(!output.stdout.contains(&0x1b));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["ErrorStatus"], 200);
    assert_eq!(json["udpstp"]["EndReason"], "ControlPhaseFailed");
    assert_eq!(
        json["udpstp"]["ControlPhaseFailure"],
        "TestInitiationTimedOut"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("\x1b[38;2;92;184;255m"), "{stderr}");
    assert!(stderr.contains("\x1b[1m\x1b[38;2;255;92;87m"), "{stderr}");
    assert!(stderr.contains("no answer from"), "{stderr}");
}

// The shared color policy disables styles when output is redirected in
// auto mode, and when the Client user explicitly selects never.
#[test]
fn redirected_auto_and_explicit_never_keep_client_output_plain() {
    for color in ["auto", "never"] {
        let output = unanswered("text", color);
        assert_eq!(output.status.code(), Some(200));
        assert!(!output.stdout.contains(&0x1b), "{color}");
        assert!(!output.stderr.contains(&0x1b), "{color}");
        assert!(
            !String::from_utf8_lossy(&output.stdout).contains('▶'),
            "{color}"
        );
    }
}

struct Server {
    child: Child,
    address: String,
}

impl Server {
    fn start() -> Self {
        let port = UdpSocket::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let mut child = Command::new(env!("CARGO_BIN_EXE_udpstp"))
            .args([
                "server",
                "--listen",
                "127.0.0.1",
                "--port",
                &port.to_string(),
                "--allow-unauthenticated",
                "--color",
                "never",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the Server starts");
        let (sender, lines) = mpsc::channel();
        let stdout = child.stdout.take().unwrap();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = sender.send(line);
            }
        });
        let server = Self {
            child,
            address: format!("127.0.0.1:{port}"),
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut received = String::new();
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            let Ok(line) = lines.recv_timeout(left) else {
                break;
            };
            received.push_str(&line);
            received.push('\n');
            if line.contains("listening") {
                return server;
            }
        }
        panic!("the Server did not listen within 10 s: {received}");
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// S2b and the Client color policy: a Test that completes after giving up
// one Test Connection has a qualified aggregate outcome even with exit 0.
#[test]
fn a_partial_multi_connection_test_keeps_aggregate_and_own_outcomes_distinct() {
    let server = Server::start();
    let unanswered = UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_udpstp"));
    command
        .args([
            "client",
            "--unauthenticated",
            "--time-limit",
            "1",
            "--test-interval",
            "5",
            "--min-test-connections",
            "1",
            "--max-bandwidth",
            "2",
            "--socket-buffer-size",
            "65536",
            "--color",
            "always",
            &server.address,
            &unanswered.local_addr().unwrap().to_string(),
        ])
        .env_remove("NO_COLOR");
    let output = run_bounded(command, Duration::from_secs(15));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let plain = plain(&stdout);
    assert!(
        plain.contains("Test outcome: partially completed"),
        "{stdout}"
    );
    assert!(
        stdout.contains("\x1b[1m\x1b[38;2;243;156;18mpartially\x1b[0m"),
        "{stdout}"
    );
    assert!(
        plain.contains("Test Connection 0 to") && plain.contains("Completed"),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!(
            "Test Connection 1 to {}: \x1b[1m\x1b[38;2;255;92;87mabandoned\x1b[0m",
            unanswered.local_addr().unwrap()
        )),
        "{stdout}"
    );
    assert!(
        !stdout.contains("\x1b[1m\x1b[38;2;90;247;142mpartially"),
        "{stdout}"
    );
}
