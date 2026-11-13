//! Seam S2a: human presentation controls preserve process contents,
//! destinations and exit statuses (ADR 0013).

mod common;
#[path = "common/styling.rs"]
mod styling;

use styling::plain;

use std::process::{Command, Output};
use std::time::Duration;

fn command(args: &[&str], environment: &[(&str, &str)]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_udpstp"));
    command.args(args).env("TERM", "xterm");
    for name in ["NO_COLOR", "CLICOLOR", "CLICOLOR_FORCE", "CI"] {
        command.env_remove(name);
    }
    command.envs(environment.iter().copied());
    command
}

fn run(args: &[&str], environment: &[(&str, &str)]) -> Output {
    common::run_bounded(command(args, environment), Duration::from_secs(10))
}

fn text(bytes: &[u8]) -> &str {
    std::str::from_utf8(bytes).expect("UTF-8 presentation")
}

// The color policy: explicit modes apply to help before execution begins,
// whether the global option appears before or after a subcommand.
#[test]
fn explicit_help_modes_preserve_contents_and_style_useful_labels() {
    for subcommand in [None, Some("client"), Some("server")] {
        for before in [true, false] {
            let args = |mode| {
                let mut args = Vec::new();
                if before {
                    args.extend(["--color", mode]);
                }
                args.extend(subcommand);
                if !before {
                    args.extend(["--color", mode]);
                }
                args.push("--help");
                args
            };
            let unstyled = run(&args("never"), &[]);
            let styled = run(&args("always"), &[("NO_COLOR", "1"), ("TERM", "dumb")]);
            assert!(unstyled.status.success(), "{}", text(&unstyled.stderr));
            assert!(styled.status.success(), "{}", text(&styled.stderr));
            assert!(unstyled.stderr.is_empty() && styled.stderr.is_empty());
            let unstyled = text(&unstyled.stdout);
            let styled = text(&styled.stdout);
            assert!(!unstyled.contains('\u{1b}'));
            assert!(
                styled.contains("\u{1b}[1m\u{1b}[38;2;189;147;249mUsage:"),
                "{styled}"
            );
            assert!(styled.contains("\u{1b}[1m--color"), "{styled}");
            assert_eq!(plain(styled), unstyled);
        }
    }
}

// Auto always needs a terminal; backend environment force conventions do
// not override that rule or the explicit command-line modes.
#[test]
fn redirected_auto_is_plain_and_explicit_modes_override_environment() {
    for environment in [
        vec![],
        vec![("NO_COLOR", "")],
        vec![("NO_COLOR", "1")],
        vec![("TERM", "dumb")],
        vec![("CLICOLOR_FORCE", "1"), ("CI", "true")],
        vec![("CLICOLOR", "0")],
    ] {
        for mode in ["auto", "never", "always"] {
            let output = run(&["client", "--color", mode, "--help"], &environment);
            assert!(output.status.success(), "{}", text(&output.stderr));
            assert_eq!(text(&output.stdout).contains('\u{1b}'), mode == "always");
        }
    }
}

// ADR 0013: parser and startup errors remain on stderr with status 50,
// retaining identical text while the error label is red.
#[test]
fn parser_and_startup_errors_honor_modes_before_execution() {
    for args in [
        vec!["client", "localhost", "--delay-var", "invalid"],
        vec!["client", "localhost", "--algorithm", "invalid"],
        vec!["client", "localhost"],
        vec!["server", "--config", "missing-color-test-config.toml"],
    ] {
        let mut plain_args = vec!["--color", "never"];
        plain_args.extend_from_slice(&args);
        let mut colored_args = vec!["--color", "always"];
        colored_args.extend_from_slice(&args);
        let unstyled = run(&plain_args, &[]);
        let styled = run(&colored_args, &[("NO_COLOR", "1")]);
        assert_eq!(unstyled.status.code(), Some(50));
        assert_eq!(styled.status, unstyled.status);
        assert!(unstyled.stdout.is_empty() && styled.stdout.is_empty());
        assert!(text(&styled.stderr).contains("\u{1b}[1m\u{1b}[38;2;255;92;87m"));
        assert_eq!(plain(text(&styled.stderr)), text(&unstyled.stderr));
    }
}

// Clap validates the control; early presentation respects the last valid
// occurrence without treating option values or post-`--` tokens as controls.
#[test]
fn invalid_and_duplicate_controls_still_report_parser_errors() {
    for (args, colored) in [
        (vec!["--color=always", "--color=invalid"], true),
        (vec!["--color=always", "--color=never"], false),
        (vec!["--color=never", "--color=always"], true),
        (vec!["--color=invalid"], false),
        (vec!["--color"], false),
        (
            vec!["client", "localhost", "--key-file=--color=always"],
            false,
        ),
        (vec!["client", "--", "--color=always"], false),
    ] {
        let output = run(&args, &[]);
        assert!(!output.status.success(), "{args:?}");
        assert!(output.stdout.is_empty());
        assert_eq!(text(&output.stderr).contains('\u{1b}'), colored, "{args:?}");
    }
    // Help can short-circuit clap before its later global option is read.
    let output = run(&["client", "--help", "--color=always"], &[]);
    assert!(output.status.success());
    assert!(text(&output.stdout).contains('\u{1b}'));
}

// ADR 0013: printed settings are a machine-readable TOML interface.
#[test]
fn printed_configuration_stays_plain_even_with_forced_color() {
    let plain = run(&["--color=never", "server", "--print-config"], &[]);
    let colored = run(&["server", "--print-config", "--color=always"], &[]);
    assert!(plain.status.success() && colored.status.success());
    assert_eq!(plain.stdout, colored.stdout);
    assert_eq!(plain.stderr, colored.stderr);
    assert!(!text(&colored.stdout).contains('\u{1b}'));
    toml::from_str::<toml::Table>(text(&colored.stdout)).unwrap();
}

#[cfg(unix)]
mod terminals {
    use super::*;
    use std::io::Read;
    use std::process::Stdio;
    use std::time::Instant;

    use nix::pty::{Winsize, openpty};
    use nix::sys::termios::{OutputFlags, SetArg, tcgetattr, tcsetattr};

    fn run_terminal(
        args: &[&str],
        width: u16,
        stdout: bool,
        environment: &[(&str, &str)],
    ) -> (Output, String) {
        let pty = openpty(
            Some(&Winsize {
                ws_row: 40,
                ws_col: width,
                ws_xpixel: 0,
                ws_ypixel: 0,
            }),
            None,
        )
        .unwrap();
        // Compare application bytes, without the PTY translating line endings.
        let mut settings = tcgetattr(&pty.slave).unwrap();
        settings.output_flags.remove(OutputFlags::OPOST);
        tcsetattr(&pty.slave, SetArg::TCSANOW, &settings).unwrap();
        let mut command = command(args, environment);
        if stdout {
            command
                .stdout(Stdio::from(pty.slave.try_clone().unwrap()))
                .stderr(Stdio::piped());
        } else {
            command
                .stdout(Stdio::piped())
                .stderr(Stdio::from(pty.slave.try_clone().unwrap()));
        }
        let mut child = command.spawn().unwrap();
        drop(command);
        drop(pty.slave);
        let terminal = std::thread::spawn(move || {
            let mut reader = std::fs::File::from(pty.master);
            let mut bytes = Vec::new();
            let _ = reader.read_to_end(&mut bytes);
            String::from_utf8(bytes).unwrap().replace("\r\n", "\n")
        });
        let pipe: Box<dyn Read + Send> = if stdout {
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
                panic!("terminal child did not finish within 10 s");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let captured = captured.join().unwrap();
        let (stdout, stderr) = if stdout {
            (Vec::new(), captured)
        } else {
            (captured, Vec::new())
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

    // Actual Unix terminal evidence is separate from forced-color pipe
    // captures. stdout and stderr resolve capability independently.
    #[test]
    fn automatic_color_honors_terminal_capability_and_environment() {
        for (args, stdout) in [
            (vec!["client", "--help"], true),
            (vec!["client", "localhost", "--delay-var=invalid"], false),
        ] {
            for (environment, expected) in [
                (vec![], true),
                (vec![("NO_COLOR", "")], true),
                (vec![("NO_COLOR", "1")], false),
                (vec![("TERM", "dumb")], false),
                (vec![("CLICOLOR", "0")], true),
                (vec![("CLICOLOR_FORCE", "1"), ("TERM", "dumb")], false),
            ] {
                let (output, terminal) = run_terminal(&args, 80, stdout, &environment);
                assert_eq!(terminal.contains('\u{1b}'), expected, "{environment:?}");
                assert!(output.stdout.is_empty() && output.stderr.is_empty());
                let (output, terminal) = run_terminal(&args, 80, !stdout, &environment);
                assert!(terminal.is_empty());
                let captured = if stdout { output.stdout } else { output.stderr };
                assert!(!text(&captured).contains('\u{1b}'));
            }
        }
    }

    // Layout counts displayed columns, and styles reset before each line
    // ends. Color never changes alignment or wrapping at supported widths.
    #[test]
    fn colored_help_and_diagnostics_keep_identical_terminal_layouts() {
        for width in [60, 80, 100, 120] {
            for (args, stdout) in [
                (vec!["--help"], true),
                (vec!["client", "--help"], true),
                (vec!["server", "--help"], true),
                (vec!["client", "localhost", "--delay-var=invalid"], false),
                (vec!["client", "localhost"], false),
            ] {
                let mut unstyled = vec!["--color=never"];
                unstyled.extend_from_slice(&args);
                let mut styled = vec!["--color=always"];
                styled.extend_from_slice(&args);
                let (plain_output, unstyled) = run_terminal(&unstyled, width, stdout, &[]);
                let (color_output, styled) = run_terminal(&styled, width, stdout, &[]);
                assert_eq!(plain_output.status, color_output.status);
                assert_eq!(plain(&styled), unstyled, "width {width}: {styled}");
                for line in styled.lines() {
                    if let Some(start) = line.rfind('\u{1b}') {
                        assert!(
                            line[start..].starts_with("\u{1b}[0m"),
                            "a styled line finishes reset: {line}"
                        );
                    }
                    assert!(
                        unicode_width::UnicodeWidthStr::width(plain(line).as_str())
                            <= usize::from(width),
                        "width {width}: {line}"
                    );
                }
            }
        }
    }
}
