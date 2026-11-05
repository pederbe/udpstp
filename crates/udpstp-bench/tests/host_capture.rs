//! First-Round host evidence through the capture helper's process interface.
//! The accepted migration preserves its sampler lifecycle until replacement.
#![cfg(target_os = "linux")]

use std::path::Path;
use std::process::{Child, Command};
use std::time::Duration;

use bench_rounds::secrets::SecretDir;
use serde_json::Value;

fn setup(work: &Path) {
    use std::os::unix::fs::PermissionsExt;
    bench_rounds::host::stage_executable(
        Path::new(env!("CARGO_BIN_EXE_udpstp-bench")),
        &work.join("udpstp-bench"),
    )
    .unwrap();
    std::fs::write(
        work.join("first-capture"),
        include_str!("../../../scripts/first-capture.sh"),
    )
    .unwrap();
    std::fs::set_permissions(
        work.join("first-capture"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
}

fn capture(work: &Path, args: &[&str]) -> Value {
    let output = Command::new("timeout")
        .arg("5")
        .arg("./first-capture")
        .args(args)
        .current_dir(work)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

struct Running {
    work: SecretDir,
    target: Child,
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = Command::new("timeout")
            .arg("5")
            .arg(env!("CARGO_BIN_EXE_udpstp-bench"))
            .args(["capture", "stop", "vm-001"])
            .current_dir(self.work.path())
            .output();
        let _ = self.target.kill();
        let _ = self.target.wait();
    }
}

#[test]
fn stopping_an_owned_sampler_retains_evidence_and_is_idempotent() {
    // First-Round diagnostics require completed captures before the next Test.
    let work = SecretDir::create().unwrap();
    setup(work.path());
    let running = Running {
        work,
        target: Command::new("sleep").arg("20").spawn().unwrap(),
    };
    let pid = running.target.id().to_string();
    assert_eq!(
        capture(running.work.path(), &["start", "vm-001", &pid, "15", "-"])["status"],
        0
    );
    std::thread::sleep(Duration::from_millis(1100));
    let stopped = capture(running.work.path(), &["stop", "vm-001"]);
    assert_eq!(stopped["stopped"], true);
    assert_eq!(stopped["status"], 0);
    assert!(stopped["samples"].as_str().unwrap().lines().count() >= 2);
    assert!(stopped["window"][1].as_f64().unwrap() > stopped["window"][0].as_f64().unwrap());
    assert!(stopped["after"].as_str().unwrap().contains("NET_RX"));
    assert_eq!(
        capture(running.work.path(), &["stop", "vm-001"])["stopped"],
        true
    );
}

#[test]
fn missing_samples_preserve_the_window_and_mark_evidence_unavailable() {
    // Measurement migration  preserves the reference's partial evidence.
    let work = SecretDir::create().unwrap();
    setup(work.path());
    std::fs::write(
        work.path().join("vm-001.start.json"),
        r#"{"pid":"self","window_start":1.0,"before":"saved interrupts","log":"-","offset":null}"#,
    )
    .unwrap();
    std::fs::write(
        work.path().join("vm-001.pid.json"),
        r#"{"pid":2147483647,"identity":null}"#,
    )
    .unwrap();
    let stopped = capture(work.path(), &["stop", "vm-001"]);
    assert_eq!(stopped["stopped"], true);
    assert_eq!(stopped["status"], Value::Null);
    assert_eq!(stopped["pid"], "self");
    assert_eq!(stopped["window"][0], 1.0);
    assert_eq!(stopped["before"], "saved interrupts");
    assert_eq!(stopped["samples"], "");
}
