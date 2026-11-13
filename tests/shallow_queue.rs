//! 's investigation, kept to reproduce it: Search Tests through a
//! shallow queue (20 ms) and a deep one (200 ms), at 50 and 48 Mbit/s,
//! udpstp's and OB-UDPST's side by side on the same netns bottleneck, each
//! run's Sub-Intervals printed. With `UDPSTP_SHALLOW_Q` (a list of
//! seqErrThresh values) it runs OB-UDPST alone through the shallow queue
//! with each. It gates nothing: the proving test is algorithm_b.rs's
//! a_search_through_a_shallow_queue_alternates_around_the_bottleneck, and
//! the resulting trace is diagnostic. Runs only with
//! UDPSTP_SHALLOW set, in the netns tier's namespace, with
//! UDPSTP_OBUDPST_DIR pointing at scripts/obudpst-build.sh's build.
#![cfg(target_os = "linux")]

mod common;
#[path = "common/key_file.rs"]
mod key_file;
#[path = "common/netns.rs"]
mod netns;

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::time::Duration;

use common::run_bounded;
use key_file::KeyFile;
use netns::{Node, Spec, Topology, addr};

const UDPSTP: &str = env!("CARGO_BIN_EXE_udpstp");

struct Process(Child);

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start(topology: &Topology, program: &str, args: &[&str], ready: &str) -> Process {
    let mut child = topology
        .command(Node::Server, program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("starts");
    let stdout = child.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    loop {
        match lines.next() {
            Some(Ok(line)) if line.contains(ready) => break,
            Some(Ok(_)) => {}
            _ => panic!("{program} didn't start"),
        }
    }
    std::thread::spawn(move || for _ in lines {});
    Process(child)
}

fn show(what: &str, stdout: &[u8]) {
    let Ok(results) = serde_json::from_slice::<serde_json::Value>(stdout) else {
        eprintln!("{what}: no JSON: {}", String::from_utf8_lossy(stdout));
        return;
    };
    let subs = results["Output"]["IncrementalResult"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let caps: Vec<f64> = subs
        .iter()
        .filter_map(|s| s["IPLayerCapacity"].as_f64())
        .collect();
    let loss: Vec<u64> = subs
        .iter()
        .filter_map(|s| s["LossCount"].as_u64())
        .collect();
    eprintln!(
        "{what}: caps {caps:?}\n  loss {loss:?}\n  AtMax {} warnings {}",
        results["Output"]["AtMax"]["MaxIPLayerCapacity"], results["udpstp"]["Warnings"]
    );
}

#[test]
fn a_search_through_a_shallow_queue() {
    if std::env::var_os("UDPSTP_SHALLOW").is_none() || !netns::available("shallow") {
        return;
    }
    let udpst = PathBuf::from(std::env::var("UDPSTP_OBUDPST_DIR").expect("OB-UDPST"))
        .join("v9.1.0/auth/udpst");
    let keys = KeyFile::new("shallow", "udpst.keys", "5 shallow-key\n");
    let only_q: Vec<String> = std::env::var("UDPSTP_SHALLOW_Q")
        .map(|q| q.split(',').map(String::from).collect())
        .unwrap_or_default();
    for queue_ms in [20, 200] {
        for rate_mbit in [50, 48] {
            if !only_q.is_empty() {
                if queue_ms != 20 || rate_mbit != 50 {
                    continue;
                }
                let spec = Spec {
                    rate_mbit,
                    queue_ms,
                    ..Spec::default()
                };
                let topology = Topology::new("shallowq", spec);
                let _server = start(
                    &topology,
                    udpst.to_str().unwrap(),
                    &["-v", "-K", keys.path(), addr::SERVER_V4],
                    "Awaiting setup requests",
                );
                for q in &only_q {
                    let mut client = topology.command(Node::Client, udpst.to_str().unwrap());
                    client.args([
                        "-d",
                        "-a",
                        "shallow-key",
                        "-y",
                        "5",
                        "-t",
                        "10",
                        "-f",
                        "json",
                        "-q",
                        q,
                    ]);
                    client.arg(addr::SERVER_V4);
                    let out = run_bounded(client, Duration::from_secs(40));
                    show(
                        &format!("OB-UDPST -q {q}, {rate_mbit} Mbit/s, {queue_ms} ms"),
                        &out.stdout,
                    );
                }
                continue;
            }
            let spec = Spec {
                rate_mbit,
                queue_ms,
                ..Spec::default()
            };
            let topology = Topology::new("shallow", spec);
            {
                let _server = start(
                    &topology,
                    UDPSTP,
                    &["server", "--key-file", keys.path()],
                    "listening",
                );
                let mut client = topology.command(Node::Client, UDPSTP);
                client.args(["client", "--key-file", keys.path(), "--auth-mode", "1"]);
                client.args(["--format", "json", "--test-interval", "10", addr::SERVER_V4]);
                let out = run_bounded(client, Duration::from_secs(40));
                show(
                    &format!("udpstp {rate_mbit} Mbit/s, {queue_ms} ms"),
                    &out.stdout,
                );
            }
            {
                let _server = start(
                    &topology,
                    udpst.to_str().unwrap(),
                    &["-v", "-K", keys.path(), addr::SERVER_V4],
                    "Awaiting setup requests",
                );
                let mut client = topology.command(Node::Client, udpst.to_str().unwrap());
                client.args([
                    "-d",
                    "-a",
                    "shallow-key",
                    "-y",
                    "5",
                    "-t",
                    "10",
                    "-f",
                    "json",
                ]);
                client.arg(addr::SERVER_V4);
                let out = run_bounded(client, Duration::from_secs(40));
                show(
                    &format!("OB-UDPST {rate_mbit} Mbit/s, {queue_ms} ms"),
                    &out.stdout,
                );
            }
        }
    }
}
