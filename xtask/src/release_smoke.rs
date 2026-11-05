//! Exercise the installed, retained release binary, without rebuilding it.

use std::fs::{self, File};
use std::net::UdpSocket;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Process(Child);

impl Drop for Process {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

fn checked(command: &mut Command) -> Result<(), String> {
    let mut process = Process(
        command
            .spawn()
            .map_err(|e| format!("start {command:?}: {e}"))?,
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = process.0.try_wait().map_err(|e| e.to_string())? {
            return if status.success() {
                Ok(())
            } else {
                Err(format!("{command:?} exited with {status}"))
            };
        }
        if Instant::now() >= deadline {
            return Err(format!("{command:?} exceeded 30 seconds"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn command(binary: &str, runner: Option<&String>) -> Command {
    let mut command = if let Some(runner) = runner {
        let mut command = Command::new(runner);
        command.arg(binary);
        command
    } else {
        Command::new(binary)
    };
    command.stdin(Stdio::null());
    command
}

fn report(json: &serde_json::Value) -> Result<(), String> {
    if json["Output"]["Status"] != "Complete"
        || !json["ErrorStatus"]
            .as_u64()
            .is_some_and(|status| status < 50)
        || !json["Output"]["IncrementalResult"]
            .as_array()
            .is_some_and(|results| {
                results.iter().any(|result| {
                    result["IPLayerCapacity"]
                        .as_f64()
                        .is_some_and(|rate| rate > 0.0)
                })
            })
    {
        return Err("the installed binary did not report a completed Test with results".into());
    }
    Ok(())
}

struct KeyFile(std::path::PathBuf);

impl Drop for KeyFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn key_file(directory: &Path) -> Result<KeyFile, String> {
    let path = directory.join("keys.toml");
    let file = KeyFile(path);
    fs::write(
        &file.0,
        "[[key]]\nadmin_key_name = \"release-check\"\nkey_id = 1\nkey_text = \"loopback-release-check-only\"\n",
    )
    .map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&file.0, fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
    }
    #[cfg(windows)]
    {
        let user = std::env::var("USERNAME").map_err(|e| e.to_string())?;
        checked(Command::new("icacls").arg(&file.0).args([
            "/inheritance:r",
            "/grant:r",
            &format!("{user}:F"),
        ]))?;
    }
    Ok(file)
}

pub(crate) fn run(args: &[String]) -> Result<(), String> {
    let [binary, version, directory, rest @ ..] = args else {
        return Err("usage: cargo xtask release-smoke BIN VERSION DIRECTORY [RUNNER]".into());
    };
    if rest.len() > 1 {
        return Err("at most one executable runner is supported".into());
    }
    let runner = rest.first();
    let directory = Path::new(directory);
    fs::create_dir(directory).map_err(|e| e.to_string())?;
    let version_path = directory.join("version.log");
    checked(
        command(binary, runner)
            .arg("--version")
            .stdout(File::create(&version_path).map_err(|e| e.to_string())?),
    )?;
    let actual = fs::read_to_string(&version_path).map_err(|e| e.to_string())?;
    if actual.trim() != format!("udpstp {version}") {
        return Err(format!("expected udpstp {version}, received {actual:?}"));
    }
    let key = key_file(directory)?;
    let socket = UdpSocket::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let port = socket.local_addr().map_err(|e| e.to_string())?.port();
    drop(socket);
    let mut server = command(binary, runner);
    server
        .args([
            "server",
            "--listen",
            "127.0.0.1",
            "--port",
            &port.to_string(),
        ])
        .args(["--max-hops", "8", "--key-file"])
        .arg(&key.0)
        .stdout(File::create(directory.join("server-stdout.log")).map_err(|e| e.to_string())?)
        .stderr(File::create(directory.join("server-stderr.log")).map_err(|e| e.to_string())?);
    let mut server = Process(server.spawn().map_err(|e| e.to_string())?);
    std::thread::sleep(Duration::from_secs(1));
    if let Some(status) = server.0.try_wait().map_err(|e| e.to_string())? {
        return Err(format!(
            "installed Server stopped before the Test: {status}"
        ));
    }
    for direction in ["downstream", "upstream"] {
        let result = directory.join(format!("{direction}.json"));
        let mut client = command(binary, runner);
        client
            .args(["client", "--key-file"])
            .arg(&key.0)
            .args([
                "--direction",
                direction,
                "--test-interval",
                "5",
                "--max-bandwidth",
                "10",
                "--max-hops",
                "8",
                "--format",
                "json",
                &format!("127.0.0.1:{port}"),
            ])
            .stdout(File::create(&result).map_err(|e| e.to_string())?)
            .stderr(
                File::create(directory.join(format!("{direction}.log")))
                    .map_err(|e| e.to_string())?,
            );
        checked(&mut client)?;
        let json: serde_json::Value =
            serde_json::from_slice(&fs::read(result).map_err(|e| e.to_string())?)
                .map_err(|e| format!("installed {direction} output is not JSON: {e}"))?;
        report(&json)?;
        println!("udpstp {version}: installed authenticated {direction} Test passed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::report;
    use serde_json::json;

    #[test]
    fn completion_accepts_qualified_results_and_rejects_errors_or_empty_results() {
        let valid = json!({"Output":{"Status":"Complete","IncrementalResult":[{"IPLayerCapacity":1.0}]},"ErrorStatus":49});
        assert!(report(&valid).is_ok());
        for invalid in [
            json!({"Output":{"Status":"Incomplete","IncrementalResult":[{"IPLayerCapacity":1.0}]},"ErrorStatus":0}),
            json!({"Output":{"Status":"Complete","IncrementalResult":[]},"ErrorStatus":0}),
            json!({"Output":{"Status":"Complete","IncrementalResult":[{"IPLayerCapacity":0.0}]},"ErrorStatus":0}),
            json!({"Output":{"Status":"Complete","IncrementalResult":[{"IPLayerCapacity":1.0}]},"ErrorStatus":50}),
            json!({}),
        ] {
            assert!(report(&invalid).is_err());
        }
    }
}
