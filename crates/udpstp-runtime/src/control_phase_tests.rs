//! Seam 2b: the Control Phase's thread while a Data Phase runs. A
//! Server ([`serve`]) and a Client ([`run_test`]) run a Fixed-Rate Test
//! over loopback, each Control Phase on a Tokio thread of its own, named
//! so its wake-ups can be counted: its voluntary context switches, in
//! Linux's `/proc`.
//!
//! Each Test Connection's socket is the Control Phase's until its Data
//! Phase starts, and then its Data Phase Engine's alone. Tokio registers
//! a socket with its reactor for reading and writing, edge-triggered, and
//! a registration belongs to the socket, not to the handle it was made
//! through, so while the Control Phase keeps a Tokio handle, every
//! datagram that arrives and every send the kernel completes can wake the
//! Control Phase's thread, which then finds nothing to do.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};

use tokio::sync::oneshot;
use udpstp_core::{Direction, Event};

use crate::contract::{fixed_rate_test, server_config};
use crate::{
    DEFAULT_SOCKET_BUFFER_SIZE, EngineSettings, IpOptions, ServerNotice, ServerOptions, run_test,
    serve,
};

/// The Server's Control Phase thread's name, as `/proc` shows it.
const SERVER_THREAD: &str = "control-server";
/// The Client's.
const CLIENT_THREAD: &str = "control-client";

/// The voluntary context switches so far of this process's thread named
/// `name`: its wake-ups. `None` if it can't be found or read.
pub(crate) fn voluntary_switches(name: &str) -> Option<u64> {
    let task = std::fs::read_dir("/proc/self/task")
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|task| {
            std::fs::read_to_string(task.join("comm")).is_ok_and(|comm| comm.trim() == name)
        })?;
    let status = std::fs::read_to_string(task.join("status")).ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("voluntary_ctxt_switches:"))?
        .trim()
        .parse()
        .ok()
}

/// A current-thread Tokio runtime, as the binary runs each Control Phase
/// on (ADR 0003).
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a Tokio runtime")
}

/// Runs a Fixed-Rate Test at `row` in `direction` from a Client to a
/// Server over loopback: the wake-ups a second of the Server's and the
/// Client's Control Phase threads over 2 s in the middle of its Data
/// Phase.
fn control_phase_wake_ups(direction: Direction, row: u16) -> (f64, f64) {
    let (listening, server_address) = std_mpsc::channel();
    let (stop, stopped) = oneshot::channel::<()>();
    let server = std::thread::Builder::new()
        .name(SERVER_THREAD.into())
        .spawn(move || {
            let options = ServerOptions {
                listen: vec![Ipv4Addr::LOCALHOST.into()],
                port: 0,
                ephemeral_ports: 49152..=65535,
                ip: IpOptions::default(),
                socket_buffer_size: DEFAULT_SOCKET_BUFFER_SIZE,
                engine: EngineSettings::default(),
                core: server_config(),
            };
            let notify = move |notice| {
                if let ServerNotice::Listening(addresses) = notice {
                    let _ = listening.send(addresses[0]);
                }
            };
            let shutdown = async {
                let _ = stopped.await;
                "the test is over"
            };
            runtime().block_on(serve(options, notify, shutdown))
        })
        .expect("the Server's thread starts");
    let standard: SocketAddr = server_address
        .recv_timeout(Duration::from_secs(10))
        .expect("the Server listens");
    let (activated, data_phase) = std_mpsc::channel();
    let client = std::thread::Builder::new()
        .name(CLIENT_THREAD.into())
        .spawn(move || {
            let (plan, config) = fixed_rate_test(standard, direction, row);
            let notify = move |_: Option<u8>, event: &Event| {
                if matches!(event, Event::TestActivated { .. }) {
                    let _ = activated.send(Instant::now());
                }
            };
            runtime().block_on(run_test(
                plan,
                config,
                IpOptions::default(),
                DEFAULT_SOCKET_BUFFER_SIZE,
                EngineSettings::default(),
                notify,
                |_, _| {},
                std::future::pending(),
            ))
        })
        .expect("the Client's thread starts");
    let started = data_phase
        .recv_timeout(Duration::from_secs(10))
        .expect("the Test is activated");
    // The Data Phase runs for the Test Interval, 5 s: sampled from 1 s in.
    std::thread::sleep(
        (started + Duration::from_secs(1)).saturating_duration_since(Instant::now()),
    );
    let first = (
        voluntary_switches(SERVER_THREAD),
        voluntary_switches(CLIENT_THREAD),
    );
    let from = Instant::now();
    std::thread::sleep(Duration::from_secs(2));
    let last = (
        voluntary_switches(SERVER_THREAD),
        voluntary_switches(CLIENT_THREAD),
    );
    let seconds = from.elapsed().as_secs_f64();
    let outcome = client
        .join()
        .expect("the Client ran")
        .expect("the Test ran");
    let _ = stop.send(());
    server
        .join()
        .expect("the Server ran")
        .expect("the Server stopped cleanly");
    let ended = &outcome.connections[0].result;
    assert!(
        matches!(
            ended,
            crate::ClientResult::Finished {
                reason: Some(_),
                error: None,
                ..
            }
        ),
        "{direction:?}: the Test ran: {ended:?}"
    );
    let rate = |first: Option<u64>, last: Option<u64>| {
        let (first, last) = (
            first.expect("a thread's /proc"),
            last.expect("a thread's /proc"),
        );
        last.saturating_sub(first) as f64 / seconds
    };
    (rate(first.0, last.0), rate(first.1, last.1))
}

// : once a Test Connection's Data Phase starts, its socket
// leaves the Control Phase's Tokio reactor, so neither the datagrams that
// arrive on it nor the sends the kernel completes wake the Control Phase's
// thread, on the Server or the Client, in either direction. Row 40 sends
// 4,000 Load PDUs a second; what does wake the Control Phase is the Data
// Phase's events (tens a second), its timers and its 1 s tick.
#[test]
fn data_phase_traffic_does_not_wake_the_control_phase() {
    for direction in [Direction::Upstream, Direction::Downstream] {
        let (server, client) = control_phase_wake_ups(direction, 40);
        eprintln!(
            "{direction:?}: the Server's Control Phase woke {server:.0} times a second, the Client's {client:.0}"
        );
        assert!(
            server < 400.0 && client < 400.0,
            "{direction:?}: the Server's Control Phase woke {server:.0} times a second, \
             the Client's {client:.0}, with 4,000 Load PDUs a second in its Data Phase"
        );
    }
}
