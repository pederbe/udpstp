#![forbid(unsafe_code)]
#![deny(clippy::exit)]
//! The fake `aws` CLI for the bench tooling's tests: the EC2 model
//! of `bench_rounds::aws::fake`, with its state in the file
//! `FAKE_AWS_STATE` names, as `scripts/bench/tests/fake_aws.py` keeps it,
//! so that either tool's provider can run against it. It never reaches
//! AWS.
//!
//! ```sh
//! FAKE_AWS_STATE=state.json fake-aws ec2 describe-subnets --region eu-north-1 --output json
//! ```
//!
//! Each call is recorded in the state; the state is written back whole,
//! through a temporary file renamed over it, before the answer; a
//! `pause_call` then waits for its file before answering.

use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use bench_rounds::aws::fake;
use bench_rounds::py::{self, Json};

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(message) => {
            let _ = writeln!(std::io::stderr(), "fake aws: {message}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<ExitCode, String> {
    let state_file = std::env::var_os("FAKE_AWS_STATE").ok_or("FAKE_AWS_STATE isn't set")?;
    let state_file = PathBuf::from(state_file);
    let argv = std::env::args_os()
        .skip(1)
        .map(OsString::into_string)
        .collect::<Result<Vec<String>, _>>()
        .map_err(|a| format!("an argument that isn't UTF-8: {a:?}"))?;
    let bytes = fs::read(&state_file).map_err(|e| format!("{}: {e}", state_file.display()))?;
    let mut state =
        Json::parse_bytes(&bytes).map_err(|e| format!("{}: {e}", state_file.display()))?;
    let answer = fake::call(&mut state, &argv).map_err(|e| e.to_string())?;
    save(&state_file, &state).map_err(|e| format!("{}: {e}", state_file.display()))?;
    if let Some(until) = &answer.pause_until {
        // Until a test lets it go on; the call is in the state meanwhile.
        while !Path::new(until).exists() {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    let _ = std::io::stdout().write_all(answer.stdout.as_bytes());
    let _ = std::io::stderr().write_all(answer.stderr.as_bytes());
    Ok(ExitCode::from(answer.status))
}

/// The state as `json.dump(state, f)` writes it, through a temporary
/// file beside it renamed over it, so that a test reading it meanwhile
/// sees the old state or the new, whole.
fn save(path: &Path, state: &Json) -> std::io::Result<()> {
    let name = path
        .file_name()
        .map_or_else(|| "state".into(), |n| n.to_string_lossy().into_owned());
    let temp = path.with_file_name(format!("{name}.{}.tmp", std::process::id()));
    fs::write(&temp, py::dumps(state))?;
    fs::rename(&temp, path)
}
