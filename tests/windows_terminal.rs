//! Native Windows console evidence through ConPTY, including the actual
//! handles inherited by a child when its other destination is a pipe.

#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
fn main() {
    windows::run();
}

#[cfg(windows)]
mod windows {
    use std::io::{IsTerminal as _, Read, Write as _};
    use std::net::UdpSocket;
    use std::num::NonZeroU16;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::{Duration, Instant};

    use portable_pty::{CommandBuilder, PtySize, native_pty_system};

    const ROWS: u16 = 500;
    const BEGIN: &str = "UDPSTP-TERMINAL-BEGIN";
    const END: &str = "UDPSTP-TERMINAL-END";

    #[derive(serde::Serialize, serde::Deserialize)]
    struct Capture {
        code: Option<i32>,
        pipe: String,
        stdout_terminal: bool,
        stderr_terminal: bool,
    }

    struct Presented {
        contents: String,
        screen: vt100::Screen,
        capture: Capture,
    }

    enum Input {
        CursorPosition(Vec<u8>),
        Close,
    }

    #[derive(Default)]
    struct TerminalTrace {
        bytes: Vec<u8>,
        cursor_requests: usize,
        cursor_queued: usize,
        cursor_replies: usize,
        cursor_reply_write_error: Option<String>,
    }

    struct CursorReplies {
        input: mpsc::Sender<Input>,
        trace: Arc<Mutex<TerminalTrace>>,
    }

    impl vt100::Callbacks for CursorReplies {
        fn unhandled_csi(
            &mut self,
            screen: &mut vt100::Screen,
            first: Option<u8>,
            second: Option<u8>,
            params: &[&[u16]],
            command: char,
        ) {
            if first.is_none() && second.is_none() && params == [&[6][..]] && command == 'n' {
                self.trace.lock().unwrap().cursor_requests += 1;
                let (row, column) = screen.cursor_position();
                let reply = format!("\u{1b}[{};{}R", row + 1, column + 1).into_bytes();
                if self.input.send(Input::CursorPosition(reply)).is_ok() {
                    self.trace.lock().unwrap().cursor_queued += 1;
                }
            }
        }
    }

    fn retain_trace(
        name: &str,
        details: &serde_json::Value,
        trace: &TerminalTrace,
        phase: &str,
        capture: Option<&Capture>,
    ) {
        if let Some(directory) = std::env::var_os("UDPSTP_TERMINAL_EVIDENCE_DIR") {
            let directory = PathBuf::from(directory);
            std::fs::create_dir_all(&directory).unwrap();
            let next = std::fs::read_dir(&directory).unwrap().count();
            let stem = format!("{next:04}-{name}");
            std::fs::write(directory.join(format!("{stem}.ansi")), &trace.bytes).unwrap();
            let mut metadata = details.clone();
            let fields = metadata.as_object_mut().unwrap();
            fields.insert("phase".into(), phase.into());
            fields.insert("cursor_requests".into(), trace.cursor_requests.into());
            fields.insert("cursor_queued".into(), trace.cursor_queued.into());
            fields.insert("cursor_replies".into(), trace.cursor_replies.into());
            fields.insert(
                "cursor_reply_write_error".into(),
                serde_json::to_value(&trace.cursor_reply_write_error).unwrap(),
            );
            fields.insert("capture".into(), serde_json::to_value(capture).unwrap());
            std::fs::write(
                directory.join(format!("{stem}.json")),
                serde_json::to_vec_pretty(&metadata).unwrap(),
            )
            .unwrap();
        }
    }

    struct ProbeFiles(PathBuf);

    impl Drop for ProbeFiles {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
            let _ = std::fs::remove_file(self.0.with_extension("stop"));
        }
    }

    fn arguments(surface: &str) -> &[&str] {
        match surface {
            "help" => &["--help"],
            "client" => &["client", "--help"],
            "server" => &["server", "--help"],
            "parser" => &["client", "localhost", "--delay-var=invalid"],
            "startup" => &["client", "localhost"],
            _ => panic!("unknown surface {surface}"),
        }
    }

    fn probe(args: &[String], mode: &str) {
        let stdout_terminal = std::io::stdout().is_terminal();
        let stderr_terminal = std::io::stderr().is_terminal();
        assert!(
            stdout_terminal && stderr_terminal,
            "actual ConPTY console handles"
        );
        let surface = &args[2];
        let stdout = args[3] == "stdout";
        let path = PathBuf::from(&args[4]);
        let server = surface == "server-log";
        let client = surface == "client-timeout";
        let unanswered = client.then(|| UdpSocket::bind("127.0.0.1:0").unwrap());
        let mut command = Command::new(env!("CARGO_BIN_EXE_udpstp"));
        command.arg(format!("--color={mode}"));
        if server {
            let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
            let port = socket.local_addr().unwrap().port();
            drop(socket);
            command.args([
                "server",
                "--listen",
                "127.0.0.1",
                "--port",
                &port.to_string(),
            ]);
        } else if let Some(unanswered) = &unanswered {
            command.args([
                "client",
                "--unauthenticated",
                "--time-limit",
                "1",
                &unanswered.local_addr().unwrap().to_string(),
            ]);
        } else {
            command.args(arguments(surface));
        }
        if stdout {
            command.stdout(Stdio::inherit()).stderr(Stdio::piped());
        } else {
            command.stdout(Stdio::piped()).stderr(Stdio::inherit());
        }
        println!("{BEGIN}");
        std::io::stdout().flush().unwrap();
        let mut child = command.spawn().expect("native udpstp process");
        let pipe: Box<dyn Read + Send> = if stdout {
            Box::new(child.stderr.take().unwrap())
        } else {
            Box::new(child.stdout.take().unwrap())
        };
        let (sender, receiver) = std::sync::mpsc::channel();
        let stop = path.with_extension("stop");
        let pipe_stop = stop.clone();
        std::thread::spawn(move || {
            let mut pipe = pipe;
            let mut bytes = Vec::new();
            let result = (|| {
                let mut buffer = [0; 4096];
                loop {
                    let count = pipe.read(&mut buffer)?;
                    if count == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buffer[..count]);
                    if server && bytes.windows(9).any(|word| word == b"Listening") {
                        std::fs::write(&pipe_stop, b"ready")?;
                    }
                }
                Ok::<_, std::io::Error>(bytes)
            })();
            let _ = sender.send(result);
        });
        let started = Instant::now();
        let status = loop {
            if server && stop.exists() {
                let _ = child.kill();
            }
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if started.elapsed() > Duration::from_secs(10) {
                let _ = child.kill();
                let _ = child.wait();
                panic!("native presentation exceeded 10 s");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let pipe = receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("pipe closes after process exit")
            .unwrap();
        println!("{END}");
        std::io::stdout().flush().unwrap();
        std::fs::write(
            path,
            serde_json::to_vec(&Capture {
                code: status.code(),
                pipe: String::from_utf8(pipe).unwrap(),
                stdout_terminal,
                stderr_terminal,
            })
            .unwrap(),
        )
        .unwrap();
    }

    fn present(
        surface: &str,
        width: u16,
        stdout: bool,
        mode: &str,
        environment: &[(&str, &str)],
    ) -> Presented {
        let path = std::env::temp_dir().join(format!(
            "udpstp-native-terminal-{}-{surface}-{width}-{stdout}-{mode}.json",
            std::process::id()
        ));
        assert!(!path.exists(), "fresh capture {}", path.display());
        let files = ProbeFiles(path.clone());
        assert!(!path.with_extension("stop").exists(), "fresh stop signal");
        let evidence_name = format!("{surface}-{width}-{stdout}-{mode}");
        let details = serde_json::json!({
            "surface": surface,
            "width": width,
            "terminal_stdout": stdout,
            "mode": mode,
            "environment": environment,
        });
        let system = native_pty_system();
        let pair = system
            .openpty(PtySize {
                rows: ROWS,
                cols: width,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("native Windows ConPTY");
        let mut command = CommandBuilder::new(std::env::current_exe().unwrap());
        command.args(["--probe", surface, if stdout { "stdout" } else { "stderr" }]);
        command.arg(&path);
        command.env("TERM", "xterm");
        for name in ["NO_COLOR", "CLICOLOR", "CLICOLOR_FORCE", "CI"] {
            command.env_remove(name);
        }
        command.env("UDPSTP_TERMINAL_MODE", mode);
        for &(name, value) in environment {
            command.env(name, value);
        }
        let mut reader = pair.master.try_clone_reader().unwrap();
        // portable-pty creates ConPTY with PSEUDOCONSOLE_INHERIT_CURSOR.
        // Reply to its cursor query on input while output remains drained,
        // and retain the responder throughout pseudoconsole teardown.
        let mut writer = pair.master.take_writer().unwrap();
        let trace = Arc::new(Mutex::new(TerminalTrace::default()));
        let (input, requests) = mpsc::channel();
        let (input_done, input_finished) = mpsc::channel();
        let input_trace = Arc::clone(&trace);
        let input_thread = std::thread::spawn(move || {
            let result = (|| {
                while let Ok(request) = requests.recv() {
                    match request {
                        Input::CursorPosition(reply) => {
                            writer.write_all(&reply)?;
                            writer.flush()?;
                            input_trace.lock().unwrap().cursor_replies += 1;
                        }
                        Input::Close => break,
                    }
                }
                Ok::<_, std::io::Error>(())
            })();
            if let Err(error) = &result {
                input_trace.lock().unwrap().cursor_reply_write_error = Some(error.to_string());
            }
            drop(writer);
            let _ = input_done.send(result);
        });
        let (sender, receiver) = std::sync::mpsc::channel();
        let terminal_stop = path.with_extension("stop");
        let server = surface == "server-log";
        let output_trace = Arc::clone(&trace);
        let replies = input.clone();
        let output_thread = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let mut parser = vt100::Parser::new_with_callbacks(
                NonZeroU16::new(ROWS).unwrap(),
                NonZeroU16::new(width).unwrap(),
                0,
                CursorReplies {
                    input: replies,
                    trace: Arc::clone(&output_trace),
                },
            );
            let result = (|| {
                let mut buffer = [0; 4096];
                loop {
                    let count = reader.read(&mut buffer)?;
                    if count == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buffer[..count]);
                    output_trace
                        .lock()
                        .unwrap()
                        .bytes
                        .extend_from_slice(&buffer[..count]);
                    parser.process(&buffer[..count]);
                    if server && bytes.windows(9).any(|word| word == b"Listening") {
                        std::fs::write(&terminal_stop, b"ready")?;
                    }
                }
                Ok::<_, std::io::Error>(bytes)
            })();
            let _ = sender.send(result);
        });
        // Start both channels before CreateProcess can wait for the cursor
        // response. The slave's Arc is released before delivering the child.
        let slave = pair.slave;
        let (spawned, process) = mpsc::channel();
        let spawn_thread = std::thread::spawn(move || {
            let result = slave.spawn_command(command);
            drop(slave);
            let _ = spawned.send(result);
        });
        let mut child = match process.recv_timeout(Duration::from_secs(15)) {
            Ok(Ok(child)) => child,
            result => {
                retain_trace(
                    &evidence_name,
                    &details,
                    &trace.lock().unwrap(),
                    "spawn-failed",
                    None,
                );
                panic!(
                    "ConPTY spawn did not complete: {}",
                    match result {
                        Ok(Err(error)) => error.to_string(),
                        Err(error) => error.to_string(),
                        Ok(Ok(_)) => unreachable!(),
                    }
                );
            }
        };
        spawn_thread.join().unwrap();
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break Some(status);
            }
            if started.elapsed() > Duration::from_secs(15) {
                let trace = trace.lock().unwrap();
                retain_trace(&evidence_name, &details, &trace, "child-timeout", None);
                drop(trace);
                let _ = child.kill();
                break None;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        // ClosePseudoConsole is synchronous and can issue another cursor
        // query. Bound it independently while both communication threads
        // remain alive; neither FileDescriptor owns the console's Arc.
        retain_trace(
            &evidence_name,
            &details,
            &trace.lock().unwrap(),
            "before-teardown",
            None,
        );
        let master = pair.master;
        let (closed, teardown) = mpsc::channel();
        let teardown_thread = std::thread::spawn(move || {
            drop(master);
            let _ = closed.send(());
        });
        if let Err(error) = teardown.recv_timeout(Duration::from_secs(2)) {
            let trace = trace.lock().unwrap();
            retain_trace(&evidence_name, &details, &trace, "teardown-timeout", None);
            panic!(
                "ConPTY teardown did not finish: {error}; bytes={}; cursor requests={}, queued={}, written={}; partial={}",
                trace.bytes.len(),
                trace.cursor_requests,
                trace.cursor_queued,
                trace.cursor_replies,
                String::from_utf8_lossy(&trace.bytes)
                    .escape_debug()
                    .take(2000)
                    .collect::<String>()
            );
        }
        teardown_thread.join().unwrap();
        let bytes = match receiver.recv_timeout(Duration::from_secs(2)) {
            Ok(result) => result.unwrap(),
            Err(error) => {
                let trace = trace.lock().unwrap();
                retain_trace(&evidence_name, &details, &trace, "output-timeout", None);
                panic!(
                    "ConPTY reader did not finish: {error}; bytes={}; cursor requests={}, replies={}",
                    trace.bytes.len(),
                    trace.cursor_requests,
                    trace.cursor_replies
                );
            }
        };
        output_thread.join().unwrap();
        let _ = input.send(Input::Close);
        let written = input_finished.recv_timeout(Duration::from_secs(2));
        if !matches!(written, Ok(Ok(()))) {
            retain_trace(
                &evidence_name,
                &details,
                &trace.lock().unwrap(),
                "input-failed",
                None,
            );
            panic!("ConPTY input writer did not finish: {written:?}");
        }
        input_thread.join().unwrap();
        let Some(status) = status else {
            let trace = trace.lock().unwrap();
            panic!(
                "ConPTY child exceeded 15 s: {surface} {width} {mode}; cursor requests={}, queued={}, written={}; partial={}",
                trace.cursor_requests,
                trace.cursor_queued,
                trace.cursor_replies,
                String::from_utf8_lossy(&trace.bytes)
                    .escape_debug()
                    .take(2000)
                    .collect::<String>()
            );
        };
        assert!(
            status.success(),
            "probe failed: {}",
            String::from_utf8_lossy(&bytes)
        );
        let capture: Capture = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        retain_trace(
            &evidence_name,
            &details,
            &trace.lock().unwrap(),
            "complete",
            Some(&capture),
        );
        drop(files);
        let mut parser = vt100::Parser::new(
            NonZeroU16::new(ROWS).unwrap(),
            NonZeroU16::new(width).unwrap(),
            0,
        );
        parser.process(&bytes);
        let screen = parser.screen().clone();
        let contents = screen.contents();
        let contents = contents
            .split_once(BEGIN)
            .and_then(|(_, rest)| rest.split_once(END))
            .map(|(contents, _)| contents.trim_matches('\n').to_owned())
            .expect("complete console transcript markers");
        Presented {
            contents,
            screen,
            capture,
        }
    }

    fn has_foreground(screen: &vt100::Screen, color: vt100::Color) -> bool {
        let (rows, columns) = screen.size();
        (0..rows.get()).any(|row| {
            (0..columns.get()).any(|column| {
                let cell = screen.cell(row, column).unwrap();
                !cell.contents().trim().is_empty() && cell.fgcolor() == color
            })
        })
    }

    // The approved terminal palette: purple help, blue progress/INFO,
    // and red/amber errors/warnings, observed through ConPTY.
    fn foreground(surface: &str, stdout: bool) -> vt100::Color {
        match (surface, stdout) {
            ("client-timeout" | "server-log", true) => vt100::Color::Rgb(92, 184, 255),
            ("server-log", false) => vt100::Color::Rgb(243, 156, 18),
            (_, true) => vt100::Color::Rgb(189, 147, 249),
            (_, false) => vt100::Color::Rgb(255, 92, 87),
        }
    }

    pub fn run() {
        let args: Vec<String> = std::env::args().collect();
        if args.get(1).is_some_and(|arg| arg == "--probe") {
            // The mode travels independently of libtest or a shell's parsing.
            let mode = std::env::var("UDPSTP_TERMINAL_MODE").unwrap();
            probe(&args, &mode);
            return;
        }
        if !args.get(1).is_some_and(|arg| arg == "--suite") {
            // This independent process remains the watchdog through spawn,
            // panic unwinding, synchronous teardown and thread completion.
            let mut suite = Command::new(std::env::current_exe().unwrap())
                .arg("--suite")
                .stdin(Stdio::null())
                .spawn()
                .unwrap();
            let started = Instant::now();
            loop {
                if let Some(status) = suite.try_wait().unwrap() {
                    assert!(status.success(), "native terminal suite failed: {status}");
                    return;
                }
                if started.elapsed() > Duration::from_secs(180) {
                    let _ = suite.kill();
                    let _ = suite.wait();
                    panic!("native terminal suite exceeded its 180 s watchdog");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        for width in [60, 80, 100, 120] {
            for surface in ["help", "client", "server", "parser", "startup"] {
                let stdout = matches!(surface, "help" | "client" | "server");
                let plain = present(surface, width, stdout, "never", &[]);
                for mode in ["auto", "always"] {
                    let styled = present(surface, width, stdout, mode, &[]);
                    assert!(styled.capture.stdout_terminal && styled.capture.stderr_terminal);
                    assert_eq!(styled.capture.code, plain.capture.code);
                    assert_eq!(styled.capture.pipe, plain.capture.pipe);
                    assert_eq!(styled.contents, plain.contents, "{surface} {width} {mode}");
                    assert!(has_foreground(&styled.screen, foreground(surface, stdout)));
                }
                assert!(!has_foreground(&plain.screen, foreground(surface, stdout)));
                let redirected = present(surface, width, !stdout, "auto", &[]);
                let reference = present(surface, 80, !stdout, "never", &[]);
                assert!(redirected.contents.is_empty());
                assert_eq!(
                    redirected.capture.pipe, reference.capture.pipe,
                    "redirected output keeps its own 80-column layout: {surface} {width}"
                );
                assert!(!redirected.capture.pipe.contains('\u{1b}'));
            }
        }
        for surface in ["client", "parser"] {
            let stdout = surface == "client";
            for (environment, colored) in [
                (vec![("NO_COLOR", "")], true),
                (vec![("NO_COLOR", "1")], false),
                (vec![("TERM", "dumb")], false),
                (vec![("CLICOLOR", "0")], true),
                (vec![("TERM", "dumb"), ("CLICOLOR_FORCE", "1")], false),
            ] {
                let terminal = present(surface, 80, stdout, "auto", &environment);
                assert_eq!(
                    has_foreground(&terminal.screen, foreground(surface, stdout)),
                    colored
                );
                let pipe = present(surface, 80, !stdout, "auto", &environment);
                assert!(pipe.contents.is_empty());
                assert!(!pipe.capture.pipe.contains('\u{1b}'));
            }
            let environment = [("NO_COLOR", "1"), ("TERM", "dumb")];
            let forced = present(surface, 80, stdout, "always", &environment);
            assert!(
                has_foreground(&forced.screen, foreground(surface, stdout)),
                "forced color overrides NO_COLOR and TERM=dumb on {surface}"
            );
            let suppressed = present(surface, 80, stdout, "never", &environment);
            assert!(!has_foreground(
                &suppressed.screen,
                foreground(surface, stdout)
            ));
        }
        for stdout in [true, false] {
            let client = present("client-timeout", 80, stdout, "auto", &[]);
            assert_eq!(client.capture.code, Some(200));
            assert!(has_foreground(
                &client.screen,
                foreground("client-timeout", stdout)
            ));
            assert!(
                client
                    .contents
                    .contains(if stdout { "Setup Request" } else { "no answer" })
            );
            assert!(!client.capture.pipe.contains('\u{1b}'));
            let server = present("server-log", 80, stdout, "auto", &[]);
            assert!(has_foreground(
                &server.screen,
                foreground("server-log", stdout)
            ));
            assert!(
                server
                    .contents
                    .contains(if stdout { "INFO" } else { "warning:" })
            );
            assert!(!server.capture.pipe.contains('\u{1b}'));
            assert!(
                server
                    .capture
                    .pipe
                    .contains(if stdout { "warning:" } else { "Listening" })
            );
        }
        println!("native Windows console color and layout checks passed");
    }
}
