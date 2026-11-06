//! Server presentation through the process seams, with redirected
//! console output and the daily file destination used by services.

mod common;

use std::io::{BufRead as _, BufReader, Read as _};
use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const LIMIT: Duration = Duration::from_secs(10);

fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_udpstp"));
    command
        .env_remove("NO_COLOR")
        .env_remove("CLICOLOR_FORCE")
        .env_remove("FORCE_COLOR")
        .env("TERM", "xterm-256color");
    command
}

struct LogDirectory(PathBuf);

impl LogDirectory {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "udpstp-server-colors-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).expect("fresh log directory");
        Self(path)
    }

    fn path(&self) -> PathBuf {
        self.0.join("server.log")
    }

    fn contents(&self) -> String {
        let mut text = String::new();
        for entry in std::fs::read_dir(&self.0).expect("log directory") {
            let path = entry.expect("log entry").path();
            if path.extension().is_some_and(|extension| extension == "log") {
                text.push_str(&std::fs::read_to_string(path).expect("daily log"));
            }
        }
        text
    }
}

impl Drop for LogDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct RunningServer {
    child: Option<Child>,
    stdout: Option<std::thread::JoinHandle<Vec<u8>>>,
    stderr: Option<std::thread::JoinHandle<Vec<u8>>>,
    lines: mpsc::Receiver<String>,
}

impl RunningServer {
    fn start(mode: &str, format: &str, file: Option<&Path>) -> Self {
        let socket = UdpSocket::bind("127.0.0.1:0").expect("free loopback port");
        let port = socket.local_addr().expect("bound address").port();
        drop(socket);
        let mut command = command();
        command.args([
            "--color",
            mode,
            "server",
            "--listen",
            "127.0.0.1",
            "--port",
            &port.to_string(),
            "--log-format",
            format,
        ]);
        if let Some(file) = file {
            command.arg("--log-file").arg(file);
        }
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("Server starts");
        let pipe = child.stdout.take().expect("piped stdout");
        let (sender, lines) = mpsc::channel();
        let stdout = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            for line in BufReader::new(pipe).split(b'\n') {
                let line = line.expect("Server stdout");
                let _ = sender.send(String::from_utf8_lossy(&line).into_owned());
                bytes.extend_from_slice(&line);
                bytes.push(b'\n');
            }
            bytes
        });
        Self {
            child: Some(child),
            stdout: Some(stdout),
            stderr: None,
            lines,
        }
    }

    fn wait_until_listening(&mut self, file: Option<&LogDirectory>) {
        let until = Instant::now() + LIMIT;
        loop {
            if let Some(file) = file {
                if file.contents().contains("Listening") {
                    return;
                }
            } else {
                while let Ok(line) = self.lines.try_recv() {
                    if line.contains("Listening") {
                        return;
                    }
                }
            }
            if let Some(status) = self.child.as_mut().unwrap().try_wait().unwrap() {
                let stderr = if let Some(reader) = self.stderr.take() {
                    reader.join().expect("stderr reader ends")
                } else {
                    let mut bytes = Vec::new();
                    if let Some(mut pipe) = self.child.as_mut().unwrap().stderr.take() {
                        pipe.read_to_end(&mut bytes).expect("Server stderr");
                    }
                    bytes
                };
                panic!(
                    "Server exited before listening ({status})\nstderr: {}",
                    String::from_utf8_lossy(&stderr)
                );
            }
            assert!(
                Instant::now() < until,
                "Server didn't listen within {LIMIT:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn stop(mut self) -> Output {
        let mut child = self.child.take().unwrap();
        let _ = child.kill();
        let mut output = child.wait_with_output().expect("Server ends");
        output.stdout = self
            .stdout
            .take()
            .unwrap()
            .join()
            .expect("stdout reader ends");
        if let Some(stderr) = self.stderr.take() {
            output.stderr = stderr.join().expect("stderr reader ends");
        }
        output
    }
}

impl Drop for RunningServer {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(stdout) = self.stdout.take() {
            let _ = stdout.join();
        }
        if let Some(stderr) = self.stderr.take() {
            let _ = stderr.join();
        }
    }
}

fn text(bytes: &[u8]) -> &str {
    std::str::from_utf8(bytes).expect("UTF-8 output")
}

fn json_records(text: &str) -> Vec<serde_json::Value> {
    text.lines()
        .map(|line| {
            let record: serde_json::Value = serde_json::from_str(line).expect("JSON log record");
            for value in record.as_object().expect("log object").values() {
                assert!(
                    !value.as_str().is_some_and(|text| text.contains('\u{1b}')),
                    "no styling inside structured fields: {line}"
                );
            }
            record
        })
        .collect()
}

// ADR 0013: console log policy belongs to stdout; host warnings belong
// to stderr. Redirected auto output is plain, forced color is explicit.
#[test]
fn redirected_human_console_logs_honor_each_color_mode() {
    for mode in ["auto", "always", "never"] {
        let mut server = RunningServer::start(mode, "human", None);
        server.wait_until_listening(None);
        let output = server.stop();
        let stdout = text(&output.stdout);
        let stderr = text(&output.stderr);
        assert!(stdout.contains("Listening"), "{stdout}");
        assert!(stderr.contains("warning:"), "{stderr}");
        if mode == "always" {
            assert!(
                stdout.contains("\u{1b}[38;2;92;184;255mINFO\u{1b}[0m"),
                "{stdout}"
            );
            assert!(
                stderr.contains("\u{1b}[1m\u{1b}[38;2;243;156;18mwarning:\u{1b}[0m"),
                "{stderr}"
            );
        } else {
            assert!(!stdout.contains('\u{1b}'), "{stdout}");
            assert!(!stderr.contains('\u{1b}'), "{stderr}");
        }
    }
}

#[cfg(unix)]
fn server_with_terminal(terminal_stdout: bool) -> Output {
    use nix::pty::{Winsize, openpty};

    let pty = openpty(
        Some(&Winsize {
            ws_row: 40,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        }),
        None,
    )
    .expect("Server terminal");
    let socket = UdpSocket::bind("127.0.0.1:0").expect("free loopback port");
    let port = socket.local_addr().expect("bound address").port();
    drop(socket);
    let mut command = command();
    command.args([
        "--color=auto",
        "server",
        "--listen",
        "127.0.0.1",
        "--port",
        &port.to_string(),
    ]);
    if terminal_stdout {
        command
            .stdout(Stdio::from(pty.slave.try_clone().unwrap()))
            .stderr(Stdio::piped());
    } else {
        command
            .stdout(Stdio::piped())
            .stderr(Stdio::from(pty.slave.try_clone().unwrap()));
    }
    let mut child = command.spawn().expect("Server starts with a terminal");
    drop(command);
    drop(pty.slave);
    let (stdout, stderr): (Box<dyn std::io::Read + Send>, Box<dyn std::io::Read + Send>) =
        if terminal_stdout {
            (
                Box::new(std::fs::File::from(pty.master)),
                Box::new(child.stderr.take().unwrap()),
            )
        } else {
            (
                Box::new(child.stdout.take().unwrap()),
                Box::new(std::fs::File::from(pty.master)),
            )
        };
    let (sender, lines) = mpsc::channel();
    let stdout = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        // A Unix PTY can end with EIO after its final readable bytes.
        for line in BufReader::new(stdout).split(b'\n').map_while(Result::ok) {
            let _ = sender.send(String::from_utf8_lossy(&line).into_owned());
            bytes.extend_from_slice(&line);
            bytes.push(b'\n');
        }
        bytes
    });
    let stderr = std::thread::spawn(move || {
        let mut stderr = stderr;
        let mut bytes = Vec::new();
        let _ = stderr.read_to_end(&mut bytes);
        bytes
    });
    let mut server = RunningServer {
        child: Some(child),
        stdout: Some(stdout),
        stderr: Some(stderr),
        lines,
    };
    server.wait_until_listening(None);
    server.stop()
}

// ADR 0013: each destination resolves auto color independently from its
// own actual terminal handle, including the Server's ordinary log sink.
#[cfg(unix)]
#[test]
fn human_server_logs_and_startup_warnings_detect_terminals_independently() {
    for terminal_stdout in [true, false] {
        let output = server_with_terminal(terminal_stdout);
        let stdout = text(&output.stdout);
        let stderr = text(&output.stderr);
        assert!(stdout.contains("Listening"), "{stdout}");
        assert!(stderr.contains("warning:"), "{stderr}");
        if terminal_stdout {
            assert!(
                stdout.contains("\u{1b}[38;2;92;184;255mINFO\u{1b}[0m"),
                "{stdout}"
            );
            assert!(!stderr.contains('\u{1b}'), "stderr pipe: {stderr}");
        } else {
            assert!(!stdout.contains('\u{1b}'), "stdout pipe: {stdout}");
            assert!(
                stderr.contains("\u{1b}[1m\u{1b}[38;2;243;156;18mwarning:\u{1b}[0m"),
                "{stderr}"
            );
        }
    }
}

// ADR 0013: JSON console logs retain fields and contain no styling,
// including when the independent stderr warning is forced to be colored.
#[test]
fn forced_color_keeps_json_console_records_plain() {
    let mut server = RunningServer::start("always", "json", None);
    server.wait_until_listening(None);
    let output = server.stop();
    let stdout = text(&output.stdout);
    assert!(!stdout.contains('\u{1b}'), "{stdout}");
    let records = json_records(stdout);
    assert!(records.iter().any(|record| {
        record["event"] == "Listening"
            && record["level"] == "INFO"
            && record["address"]
                .as_str()
                .is_some_and(|address| address.starts_with("127.0.0.1:"))
    }));
    assert!(text(&output.stderr).contains("\u{1b}[1m\u{1b}[38;2;243;156;18mwarning:\u{1b}[0m"));
}

// ADRs 0013 and 0014: the service's daily file destination receives
// ordinary log events and startup warnings in plain human or JSON form.
#[test]
fn forced_color_keeps_human_and_json_daily_logs_and_startup_warnings_plain() {
    for format in ["human", "json"] {
        let directory = LogDirectory::new();
        let mut server = RunningServer::start("always", format, Some(&directory.path()));
        server.wait_until_listening(Some(&directory));
        let output = server.stop();
        assert!(output.stdout.is_empty(), "log file replaces stdout");
        assert!(text(&output.stderr).contains("\u{1b}[1m\u{1b}[38;2;243;156;18mwarning:\u{1b}[0m"));
        let log = directory.contents();
        assert!(!log.contains('\u{1b}'), "{log}");
        assert!(log.contains("StartupWarning"), "{log}");
        assert!(log.contains("Listening"), "{log}");
        if format == "json" {
            let records = json_records(&log);
            assert!(
                records
                    .iter()
                    .any(|record| record["event"] == "StartupWarning"
                        && record["level"] == "WARN"
                        && record["reason"] == "NoTestAdmissible")
            );
        }
    }
}

// ADRs 0013 and 0014: a startup error is plain in the file even when
// its stderr copy uses color, with the same error and exit status.
#[test]
fn forced_color_keeps_human_and_json_startup_errors_plain_in_files() {
    for format in ["human", "json"] {
        let directory = LogDirectory::new();
        let mut command = command();
        command
            .args([
                "--color",
                "always",
                "server",
                "--source-setups-per-second",
                "0",
                "--log-format",
                format,
                "--log-file",
            ])
            .arg(directory.path());
        let output = common::run_bounded(command, LIMIT);
        assert_eq!(output.status.code(), Some(50));
        assert!(output.stdout.is_empty());
        assert!(text(&output.stderr).contains('\u{1b}'));
        let log = directory.contents();
        assert!(!log.contains('\u{1b}'), "{log}");
        assert!(log.contains("StartupError"), "{log}");
        assert!(log.contains("source_setups_per_second"), "{log}");
        if format == "json" {
            let records = json_records(&log);
            assert_eq!(records.len(), 1, "{log}");
            let record = &records[0];
            assert_eq!(record["level"], "ERROR");
            assert_eq!(record["exit_status"], 50);
            assert!(
                record["error"]
                    .as_str()
                    .unwrap()
                    .contains("source_setups_per_second")
            );
        }
    }
}
