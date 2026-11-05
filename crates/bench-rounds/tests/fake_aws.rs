//! The `fake-aws` binary around the EC2 model (`bench_rounds::aws::fake`):
//! the state kept in the file `FAKE_AWS_STATE` names as the Python's fake
//! keeps it, the answers printed and the exit statuses, and a paused call.
//! The model itself is checked call for call against the Python's fake in
//! `aws_golden.rs`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use bench_rounds::py::{self, Json};

/// A directory of its own under the system's temporary directory, removed
/// when the test ends.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> TempDir {
        let dir = std::env::temp_dir().join(format!(
            "bench-rounds-fake-aws-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fake(state: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fake-aws"));
    command
        .args(args)
        .args(["--region", "eu-north-1", "--output", "json"])
        .env("FAKE_AWS_STATE", state)
        .stdin(Stdio::null());
    command
}

fn run(state: &Path, args: &[&str]) -> Output {
    fake(state, args).output().expect("fake-aws runs")
}

fn text(bytes: &[u8]) -> &str {
    std::str::from_utf8(bytes).expect("UTF-8")
}

#[test]
fn keeps_its_state_in_the_file_as_the_pythons_fake() {
    let dir = TempDir::new("state");
    let state = dir.0.join("state.json");
    std::fs::write(
        &state,
        r#"{"config": {"expiration": "2099-01-01T00:00:00Z"}}"#,
    )
    .unwrap();
    let done = run(&state, &["sts", "get-caller-identity"]);
    assert_eq!(done.status.code(), Some(0));
    assert_eq!(text(&done.stdout), "{\"Account\": \"123456789012\"}\n");
    assert_eq!(text(&done.stderr), "");
    let done = run(
        &state,
        &[
            "ec2",
            "create-security-group",
            "--tag-specifications",
            "[{\"ResourceType\": \"security-group\", \"Tags\": [{\"Key\": \"udpstp-bench\", \"Value\": \"r1\"}]}]",
        ],
    );
    assert_eq!(text(&done.stdout), "{\"GroupId\": \"sg-1\"}\n");
    // The state as json.dump writes it: the calls in order, the groups.
    assert_eq!(
        std::fs::read_to_string(&state).unwrap(),
        "{\"config\": {\"expiration\": \"2099-01-01T00:00:00Z\"}, \"calls\": [[\"sts\", \"get-caller-identity\", \
         \"--region\", \"eu-north-1\", \"--output\", \"json\"], [\"ec2\", \"create-security-group\", \
         \"--tag-specifications\", \"[{\\\"ResourceType\\\": \\\"security-group\\\", \\\"Tags\\\": [{\\\"Key\\\": \
         \\\"udpstp-bench\\\", \\\"Value\\\": \\\"r1\\\"}]}]\", \"--region\", \"eu-north-1\", \"--output\", \"json\"]], \
         \"instances\": {}, \"groups\": {\"sg-1\": {\"tags\": {\"udpstp-bench\": \"r1\"}, \"rules\": []}}}"
    );
}

#[test]
fn refuses_as_the_cli_refuses() {
    let dir = TempDir::new("refuses");
    let state = dir.0.join("state.json");
    std::fs::write(&state, "{}").unwrap();
    let done = run(&state, &["ec2", "run-instances", "--dry-run"]);
    assert_eq!(done.status.code(), Some(254));
    assert!(text(&done.stderr).contains("(DryRunOperation)"));
    let done = run(
        &state,
        &["configure", "export-credentials", "--format", "process"],
    );
    assert_eq!(
        (done.status.code(), text(&done.stderr)),
        (Some(254), "no credentials to export\n")
    );
    // A call it can't answer: exit 1, and the state as it was.
    let before = std::fs::read(&state).unwrap();
    let done = run(
        &state,
        &["ec2", "terminate-instances", "--instance-ids", "i-9"],
    );
    assert_eq!(done.status.code(), Some(1));
    assert!(text(&done.stderr).starts_with("fake aws: "));
    assert_eq!(std::fs::read(&state).unwrap(), before);
    // Without its state, nothing.
    let done = Command::new(env!("CARGO_BIN_EXE_fake-aws"))
        .args(["sts", "get-caller-identity"])
        .env_remove("FAKE_AWS_STATE")
        .output()
        .unwrap();
    assert_eq!(done.status.code(), Some(1));
}

#[test]
fn a_paused_call_waits_for_its_file_with_the_call_already_made() {
    let dir = TempDir::new("pause");
    let state = dir.0.join("state.json");
    let go = dir.0.join("go");
    let config = Json::Dict(indexmap::IndexMap::from([(
        "pause_call".to_string(),
        Json::parse(&format!(
            "{{\"command\": \"run-instances\", \"n\": 1, \"until\": {}}}",
            py::dumps(&Json::Str(go.to_string_lossy().into_owned()))
        ))
        .unwrap(),
    )]));
    let mut initial = indexmap::IndexMap::new();
    initial.insert("config".to_string(), config);
    std::fs::write(&state, py::dumps(&Json::Dict(initial))).unwrap();
    let mut child = fake(
        &state,
        &[
            "ec2",
            "run-instances",
            "--client-token",
            "r1-server",
            "--tag-specifications",
            "[{\"Tags\": [{\"Key\": \"udpstp-bench\", \"Value\": \"r1\"}]}]",
        ],
    )
    .stdout(Stdio::piped())
    .spawn()
    .unwrap();
    // The call is in the state, the instance made, while it waits.
    let started = Instant::now();
    loop {
        let now = std::fs::read_to_string(&state).unwrap_or_default();
        if now.contains("\"i-1\"") {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "the call never reached the state"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        child.try_wait().unwrap().is_none(),
        "it answered before its file"
    );
    std::fs::write(&go, "").unwrap();
    let done = child.wait_with_output().unwrap();
    assert_eq!(done.status.code(), Some(0));
    assert_eq!(
        text(&done.stdout),
        "{\"Instances\": [{\"InstanceId\": \"i-1\"}]}\n"
    );
}
