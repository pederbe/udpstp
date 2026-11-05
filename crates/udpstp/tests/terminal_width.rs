//! Observable process output through actual terminal destinations. Help
//! and diagnostics keep ADR 0013's stdout/stderr and exit-status contract.
#![cfg(unix)]

mod common;
#[path = "common/human_output.rs"]
mod human_output;

use std::io::Read;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use nix::pty::{Winsize, openpty};
use nix::sys::termios::{OutputFlags, SetArg, tcgetattr, tcsetattr};
use unicode_width::UnicodeWidthStr;

use common::run_bounded;
use human_output::prose as normalized;

/// Exactly one destination is a terminal; the other is captured through
/// a pipe. COLUMNS deliberately disagrees with the kernel's PTY size.
fn with_terminal(args: &[&str], width: u16, terminal_stdout: bool) -> (Output, String) {
    let pty = openpty(
        Some(&Winsize {
            ws_row: 40,
            ws_col: width,
            ws_xpixel: 0,
            ws_ypixel: 0,
        }),
        None,
    )
    .expect("a terminal");
    // Keep terminal capabilities while capturing the application's line endings.
    let mut settings = tcgetattr(&pty.slave).unwrap();
    settings.output_flags.remove(OutputFlags::OPOST);
    tcsetattr(&pty.slave, SetArg::TCSANOW, &settings).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_udpstp"));
    command
        .args(["--color=never"])
        .args(args)
        .env("COLUMNS", "190");
    if terminal_stdout {
        command
            .stdout(Stdio::from(pty.slave.try_clone().unwrap()))
            .stderr(Stdio::piped());
    } else {
        command
            .stdout(Stdio::piped())
            .stderr(Stdio::from(pty.slave.try_clone().unwrap()));
    }
    let mut child = command.spawn().expect("the binary runs");
    drop(command);
    drop(pty.slave);
    let terminal = std::thread::spawn(move || {
        let mut reader = std::fs::File::from(pty.master);
        let mut bytes = Vec::new();
        // A closed Unix PTY may finish with EIO after its final bytes.
        let _ = reader.read_to_end(&mut bytes);
        String::from_utf8(bytes).unwrap().replace("\r\n", "\n")
    });
    let pipe: Box<dyn Read + Send> = if terminal_stdout {
        Box::new(child.stderr.take().unwrap())
    } else {
        Box::new(child.stdout.take().unwrap())
    };
    let captured = std::thread::spawn(move || {
        let mut pipe = pipe;
        let mut bytes = Vec::new();
        pipe.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if start.elapsed() > Duration::from_secs(10) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("terminal output did not finish within 10 s");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let bytes = captured.join().unwrap();
    let (stdout, stderr) = if terminal_stdout {
        (Vec::new(), bytes)
    } else {
        (bytes, Vec::new())
    };
    (
        Output {
            status,
            stdout,
            stderr,
        },
        terminal.join().unwrap(),
    )
}

fn readable(text: &str, width: usize) {
    assert!(
        text.lines()
            .all(|line| UnicodeWidthStr::width(line) <= width),
        "width {width}: {text}"
    );
    assert!(
        !text.contains('\u{1b}'),
        "no added terminal controls: {text}"
    );
}

// The width policy and ADR 0013: terminal dimensions come from the
// destination, while a redirected destination always renders at 80.
#[test]
fn actual_terminal_help_and_redirected_help_use_their_own_widths() {
    for args in [["client", "--help"], ["server", "--help"]] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_udpstp"));
        command
            .args(["--color=never"])
            .args(args)
            .env("COLUMNS", "190");
        let redirected = run_bounded(command, Duration::from_secs(10));
        let reference = String::from_utf8(redirected.stdout).unwrap();
        assert!(redirected.status.success());
        readable(&reference, 80);
        for width in [60, 80, 100, 120] {
            let (output, help) = with_terminal(&args, width, true);
            assert!(output.status.success());
            assert!(output.stderr.is_empty());
            readable(&help, width.into());
            assert_eq!(
                normalized(&help),
                normalized(&reference),
                "every option and description remains available"
            );
            if width == 80 {
                assert_eq!(help, reference);
            }
            if width == 120 {
                assert!(
                    help.lines().any(|line| line.width() > 80),
                    "wide terminal was detected: {help}"
                );
            }
            let (output, terminal_stderr) = with_terminal(&args, width, false);
            assert!(output.status.success());
            assert!(terminal_stderr.is_empty());
            assert_eq!(
                output.stdout,
                reference.as_bytes(),
                "a stderr terminal cannot widen redirected stdout"
            );
        }
    }
}

// docs/spec.md's configuration-error status and ADR 0013's destinations:
// parser errors use stderr's width independently of stdout.
#[test]
fn actual_terminal_parser_errors_and_redirected_errors_use_their_own_widths() {
    let args = ["client", "--delay-var", "unexpected-value", "localhost"];
    let mut command = Command::new(env!("CARGO_BIN_EXE_udpstp"));
    command
        .args(["--color=never"])
        .args(args)
        .env("COLUMNS", "190");
    let reference = run_bounded(command, Duration::from_secs(10));
    assert_eq!(reference.status.code(), Some(50));
    let reference = String::from_utf8(reference.stderr).unwrap();
    readable(&reference, 80);
    for width in [60, 80, 100, 120] {
        let (output, error) = with_terminal(&args, width, false);
        assert_eq!(output.status.code(), Some(50));
        assert!(output.stdout.is_empty());
        readable(&error, width.into());
        assert_eq!(normalized(&error), normalized(&reference));
        let (output, terminal_stdout) = with_terminal(&args, width, true);
        assert_eq!(output.status.code(), Some(50));
        assert!(terminal_stdout.is_empty());
        assert_eq!(
            output.stderr,
            reference.as_bytes(),
            "a stdout terminal cannot widen redirected stderr"
        );
    }
}

// docs/interfaces.md terminal layout policy: unavailable terminal dimensions
// fall back to the same 80 columns as redirected output, independently by stream.
#[test]
fn zero_column_terminals_use_the_redirected_eighty_column_layout() {
    for (args, terminal_stdout, expected_status) in [
        (vec!["client", "--help"], true, 0),
        (
            vec!["client", "--delay-var", "unexpected-value", "localhost"],
            false,
            50,
        ),
    ] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_udpstp"));
        command
            .args(["--color=never"])
            .args(&args)
            .env("COLUMNS", "190");
        let reference = run_bounded(command, Duration::from_secs(10));
        let expected = if terminal_stdout {
            reference.stdout
        } else {
            reference.stderr
        };
        let (output, text) = with_terminal(&args, 0, terminal_stdout);
        assert_eq!(reference.status.code(), Some(expected_status));
        assert_eq!(output.status.code(), Some(expected_status));
        assert!(output.stdout.is_empty() && output.stderr.is_empty());
        assert_eq!(text.as_bytes(), expected);
        readable(&text, 80);
    }
}

// Human startup diagnostics also use stderr's terminal dimensions. The
// full offending token remains copyable when it is wider than a screen.
#[test]
fn human_diagnostics_keep_long_identifiers_intact() {
    let name = "[2001:db8:1234:5678:abcd:eeee:ffff:9999%long-interface-name]:24601";
    let (output, diagnostic) = with_terminal(&["client", name, "--unauthenticated"], 60, false);
    assert_eq!(output.status.code(), Some(75));
    assert!(diagnostic.contains(name), "{diagnostic}");
    for line in diagnostic.lines().filter(|line| line.width() > 60) {
        assert_eq!(
            line.trim().trim_end_matches(':'),
            name,
            "only the intact identifier may be wider: {diagnostic}"
        );
    }
}
