//! Helpers shared by the `udpstp` process tests.

use std::io::Read;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Runs the binary to completion, killing it if it runs for longer than
/// `limit`, so a wrong behaviour (such as starting to serve) fails the test
/// instead of hanging it. The panic shows what the process wrote.
pub fn run_bounded(mut command: Command, limit: Duration) -> Output {
    let program = command.get_program().to_owned();
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the binary runs");
    let readers = [
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    ]
    .map(|pipe| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            bytes
        })
    });
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("the binary is waited for") {
            break Some(status);
        }
        if started.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let [stdout, stderr] = readers.map(|reader| reader.join().expect("the pipe reader finishes"));
    let Some(status) = status else {
        panic!(
            "{} didn't exit within {limit:?}
stdout: {}
stderr: {}",
            program.to_string_lossy(),
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr)
        );
    };
    Output {
        status,
        stdout,
        stderr,
    }
}
