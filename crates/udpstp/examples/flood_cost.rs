//! What Setup Request floods cost a whole `udpstp` Server.
//!
//! Unlike the core-only measurement in `unverified_setup_cost`, this
//! drives a real `udpstp server` subprocess over loopback and measures the
//! whole Server under a flood: the control thread's CPU, the datagrams its
//! runtime drops before the core (the shared 1024-datagram queue), and,
//! from a legitimate probe run alongside, the Activation latency of new
//! Tests and the rate at which they stop starting. The probe signs with a
//! Key Table row of its own, apart from the flood's, so it stands for
//! another key's Clients.
//!
//! Two floods (`--kind`): forged Setup Requests (`forged-unsigned` or
//! `forged-wrong-key`), and a key holder's authentic ones that never
//! activate (`key-holder`). It ramps through `--rates` and prints a row for
//! each.
//!
//! ```text
//! cargo build --release -p udpstp                 # build the Server
//! cargo run --release -p udpstp --example flood_cost -- \
//!     --server target/release/udpstp --kind forged-unsigned \
//!     --rates 0,5000,20000,50000,100000,200000 --seconds 4
//! ```
//!
//! The CPU figure comes from /proc on Linux and from `Get-Process` on
//! Windows; elsewhere it is left out, and the rest runs anywhere. Run it
//! under the host's measurement lock, and on Windows from a fixed build
//! directory, so that the firewall asks about each program once.

#[path = "flood_support/mod.rs"]
#[allow(dead_code)]
mod flood_common;

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use udpstp_wire::activation::{ACTIVATION_PDU_LEN, ActivationPdu, cmd_response as act_resp};
use udpstp_wire::setup::{
    SETUP_PDU_LEN, SetupPdu, cmd_request as setup_cmd, cmd_response as setup_resp,
};

use flood_common::{
    FloodConfig, KeyMaterial, Kind, MAX_RATE_PPS, bind_sources, flood_until, now_unix,
    probe_activation, probe_setup,
};

/// A throwaway 32-byte Shared Key for the Server under test; it never
/// leaves loopback. Written to a mode-0600 key file the Server reads.
const KEY_HEX: &str = "5c5c5c5c5c5c5c5c5c5c5c5c5c5c5c5c5c5c5c5c5c5c5c5c5c5c5c5c5c5c5c5c";
const KEY_ID: u8 = 1;

/// The probe's own Key Table row, apart from the flood's: the Server evicts
/// the oldest Unactivated Test Connection of the row holding the most, so a
/// probe signing with the flood's key would measure only that accepted
/// same-key limit, not what a flood costs another key's Clients.
const PROBE_KEY_HEX: &str = "a3a3a3a3a3a3a3a3a3a3a3a3a3a3a3a3a3a3a3a3a3a3a3a3a3a3a3a3a3a3a3a3";
const PROBE_KEY_ID: u8 = 2;

/// The pause after each step before its log counts are read and the next
/// step starts: longer than the Server log limiter's 10 s window, so every
/// DatagramsDropped the step caused has been reported (in a Suppressed
/// rollup at the latest), plus the once-a-second tick that closes it.
const COOL_DOWN: Duration = Duration::from_secs(12);

struct Options {
    server: String,
    kind: Kind,
    rates: Vec<u64>,
    seconds: u64,
    sources: usize,
    flood_threads: usize,
    probe_interval: Duration,
    port: u16,
}

fn usage() -> String {
    "usage: flood_cost --server PATH --kind forged-unsigned|forged-wrong-key|key-holder \
[--rates a,b,c] [--seconds N] [--sources N] [--flood-threads N] [--probe-interval-ms N] [--port N]"
        .into()
}

fn parse() -> Result<Options, String> {
    let mut server: Option<String> = None;
    let mut kind: Option<Kind> = None;
    let mut rates: Vec<u64> = vec![0, 5_000, 20_000, 50_000, 100_000, 200_000];
    let mut seconds = 4u64;
    let mut sources = 2_000usize;
    let mut flood_threads = 3usize;
    let mut probe_interval = Duration::from_millis(100);
    let mut port = 0u16;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--server" => server = Some(value()?),
            "--kind" => kind = Some(Kind::parse(&value()?)?),
            "--rates" => {
                rates = value()?
                    .split(',')
                    .map(|s| s.trim().parse::<u64>().map_err(|e| format!("--rates: {e}")))
                    .collect::<Result<_, _>>()?;
            }
            "--seconds" => seconds = value()?.parse().map_err(|e| format!("--seconds: {e}"))?,
            "--sources" => sources = value()?.parse().map_err(|e| format!("--sources: {e}"))?,
            "--flood-threads" => {
                flood_threads = value()?
                    .parse()
                    .map_err(|e| format!("--flood-threads: {e}"))?;
            }
            "--probe-interval-ms" => {
                let ms: u64 = value()?
                    .parse()
                    .map_err(|e| format!("--probe-interval-ms: {e}"))?;
                probe_interval = Duration::from_millis(ms);
            }
            "--port" => port = value()?.parse().map_err(|e| format!("--port: {e}"))?,
            "-h" | "--help" => return Err(usage()),
            other => return Err(format!("unknown argument {other}\n{}", usage())),
        }
    }
    let server = server.ok_or("--server PATH to the udpstp binary is required")?;
    let kind = kind.ok_or("--kind is required")?;
    if let Some(&bad) = rates.iter().find(|&&r| r > MAX_RATE_PPS) {
        return Err(format!(
            "--rates: {bad} exceeds the {MAX_RATE_PPS} pps ceiling"
        ));
    }
    if seconds == 0 || seconds > 120 {
        return Err("--seconds must be 1..=120".into());
    }
    Ok(Options {
        server,
        kind,
        rates,
        seconds,
        sources: sources.max(1),
        flood_threads: flood_threads.clamp(1, 32),
        probe_interval,
        port,
    })
}

/// The cumulative counters the harness reads from the Server's JSON log.
#[derive(Default, Clone)]
struct LogState {
    listening: bool,
    /// Total datagrams the runtime dropped before the core (the 1024 queue),
    /// summed from `DatagramsDropped` events and their `Suppressed` rollups.
    drops: u64,
    /// From the latest `Statistics` event (cumulative since start).
    test_connections: u64,
    rate_limited: u64,
    authentication_failures: u64,
    setup_requests_ignored: u64,
    /// Unactivated Test Connections evicted for a new Setup.
    evicted: u64,
    /// From `StatisticsRefusals` (cumulative), keyed by reason.
    refusals: BTreeMap<String, u64>,
}

/// Reads the Server's JSON log, updating `state`.
fn read_log(reader: impl Read, state: Arc<Mutex<LogState>>) {
    let reader = BufReader::new(reader);
    for line in reader.lines().map_while(Result::ok) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let event = value.get("event").and_then(|v| v.as_str()).unwrap_or("");
        let u64_at = |key: &str| value.get(key).and_then(serde_json::Value::as_u64);
        let mut s = state.lock().expect("log state");
        match event {
            "Listening" => s.listening = true,
            "DatagramsDropped" => s.drops += u64_at("count").unwrap_or(0),
            "Suppressed" => {
                if value.get("suppressed_event").and_then(|v| v.as_str())
                    == Some("DatagramsDropped")
                {
                    s.drops += u64_at("suppressed_count").unwrap_or(0);
                }
            }
            "Statistics" => {
                s.test_connections = u64_at("test_connections").unwrap_or(s.test_connections);
                s.rate_limited = u64_at("rate_limited").unwrap_or(s.rate_limited);
                s.authentication_failures =
                    u64_at("authentication_failures").unwrap_or(s.authentication_failures);
                s.setup_requests_ignored =
                    u64_at("setup_requests_ignored").unwrap_or(s.setup_requests_ignored);
                s.evicted = u64_at("unactivated_test_connections_evicted").unwrap_or(s.evicted);
            }
            "StatisticsRefusals" => {
                if let (Some(reason), Some(count)) = (
                    value.get("reason").and_then(|v| v.as_str()),
                    u64_at("count"),
                ) {
                    s.refusals.insert(reason.to_owned(), count);
                }
            }
            _ => {}
        }
    }
}

/// The Server control thread's CPU seconds so far. The control thread is the
/// process's main thread (it runs `block_on(serve)`), whose tid equals the
/// pid, so /proc/<pid>/task/<pid>/stat isolates it from the Engine Threads.
#[cfg(target_os = "linux")]
fn control_cpu_seconds(pid: u32, clk_tck: f64) -> Option<f64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/stat")).ok()?;
    // The comm field is parenthesised and may hold spaces or ')': the fields
    // after it start past the last ')'.
    let after = &stat[stat.rfind(')')? + 1..];
    let fields: Vec<&str> = after.split_whitespace().collect();
    // `after` begins at field 3 (state), so utime (field 14) is index 11 and
    // stime (field 15) is index 12.
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some((utime + stime) as f64 / clk_tck)
}

/// The same on Windows, which has no /proc: the process's first thread,
/// its main one, as `Get-Process` sees it. The workspace forbids `unsafe`,
/// so this asks Windows PowerShell rather than calling GetThreadTimes. The
/// PowerShell takes a few hundred milliseconds, so a reading at the end of a
/// step's window lands that much after it. Its PSModulePath is
/// cleared, so that one inherited from PowerShell 7 can't hide its own
/// modules; the time comes back in 100 ns ticks, which no locale formats.
#[cfg(windows)]
fn control_cpu_seconds(pid: u32, _clk_tck: f64) -> Option<f64> {
    let script = format!(
        "((Get-Process -Id {pid}).Threads | Sort-Object StartTime | Select-Object -First 1).TotalProcessorTime.Ticks"
    );
    let out = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .env_remove("PSModulePath")
        .output()
        .ok()?;
    let ticks: u64 = String::from_utf8(out.stdout).ok()?.trim().parse().ok()?;
    Some(ticks as f64 / 1e7)
}

#[cfg(not(any(target_os = "linux", windows)))]
fn control_cpu_seconds(_pid: u32, _clk_tck: f64) -> Option<f64> {
    None
}

/// The clock ticks a second (_SC_CLK_TCK); 100 on Linux virtually always.
fn clk_tck() -> f64 {
    #[cfg(target_os = "linux")]
    if let Ok(out) = Command::new("getconf").arg("CLK_TCK").output()
        && let Ok(text) = String::from_utf8(out.stdout)
        && let Ok(hz) = text.trim().parse::<f64>()
        && hz > 0.0
    {
        return hz;
    }
    100.0
}

/// What one probe (a legitimate new Test) managed against the Server.
struct ProbeResult {
    /// The Setup was answered with ACK_OK.
    setup_ok: bool,
    /// The Test was activated (Setup and Activation both accepted).
    activated: bool,
    /// Time from sending the Setup Request to the Activation Response.
    latency: Option<Duration>,
}

/// Runs one probe from a fresh loopback Source: a signed Setup, then, on a
/// Setup Response, a signed Activation Request, timing the whole Control
/// Phase. `host` picks the 127.x source address so each probe is its own
/// Source with a full budget.
///
/// An error is the harness's own failure (its socket would not bind or
/// send), never the Server's: the caller stops rather than count it as a
/// Test that didn't start.
fn probe(
    target: SocketAddr,
    client_key: &[u8; 32],
    host: u32,
    mc_ident: u16,
) -> std::io::Result<ProbeResult> {
    let mut fail = ProbeResult {
        setup_ok: false,
        activated: false,
        latency: None,
    };
    let octets = host.to_be_bytes();
    let source = IpAddr::V4(Ipv4Addr::new(127, octets[1], octets[2], octets[3].max(2)));
    let socket = UdpSocket::bind(SocketAddr::from((source, 0)))?;
    socket.set_read_timeout(Some(Duration::from_millis(200)))?;
    let auth_unix_time = now_unix();
    let started = Instant::now();
    let deadline = started + Duration::from_millis(1000);
    let setup = probe_setup(
        u32::from(mc_ident),
        auth_unix_time,
        client_key,
        PROBE_KEY_ID,
    );
    socket.send_to(&setup, target)?;

    // Await the Setup Response (from the Standard Port); ignore the Null
    // Request that arrives from the Ephemeral Port.
    let mut buf = [0u8; 2048];
    let mut test_port = None;
    while Instant::now() < deadline {
        let Ok((len, _from)) = socket.recv_from(&mut buf) else {
            continue;
        };
        if len == SETUP_PDU_LEN
            && let Ok(pdu) = SetupPdu::decode(&buf[..len])
            && pdu.cmd_request == setup_cmd::SETUP_RESPONSE
        {
            if pdu.cmd_response == setup_resp::ACK_OK {
                test_port = Some(pdu.test_port);
                fail.setup_ok = true;
            }
            break;
        }
    }
    let Some(test_port) = test_port else {
        return Ok(fail);
    };

    // The Activation Request to the Ephemeral Port, then its response.
    let ephemeral = SocketAddr::new(target.ip(), test_port);
    let activation = probe_activation(true, auth_unix_time, client_key, PROBE_KEY_ID);
    socket.send_to(&activation, ephemeral)?;
    while Instant::now() < deadline {
        let Ok((len, _from)) = socket.recv_from(&mut buf) else {
            continue;
        };
        if len == ACTIVATION_PDU_LEN
            && let Ok(pdu) = ActivationPdu::decode(&buf[..len])
        {
            fail.activated = pdu.cmd_response == act_resp::ACK_OK;
            fail.latency = Some(started.elapsed());
            break;
        }
    }
    Ok(fail)
}

/// What a step's probes managed.
#[derive(Default)]
struct Probes {
    attempts: u32,
    /// Setups answered with ACK_OK, whether or not the Test then activated.
    setup_ok: u32,
    activated: u32,
    latencies: Vec<Duration>,
    /// The harness's own failure that stopped the probes, if any.
    error: Option<std::io::Error>,
}

/// A free UDP port on 127.0.0.1.
fn free_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0")
        .and_then(|s| s.local_addr())
        .map(|a| a.port())
        .unwrap_or(24601)
}

fn write_key_file(path: &std::path::Path) -> std::io::Result<()> {
    let toml = format!(
        "[[key]]\nadmin_key_name = \"flood\"\nkey_id = {KEY_ID}\nkey_hex = \"{KEY_HEX}\"\n\
         [[key]]\nadmin_key_name = \"probe\"\nkey_id = {PROBE_KEY_ID}\nkey_hex = \"{PROBE_KEY_HEX}\"\n"
    );
    std::fs::write(path, toml)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn percentile(sorted: &[Duration], p: f64) -> Option<Duration> {
    if sorted.is_empty() {
        return None;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted.get(idx).copied()
}

/// A step's flood figures.
struct StepFigures {
    /// Datagrams the flood sent a second.
    offered_pps: f64,
    /// The same in Mbit/s of IPv4 packets (a 56-byte Setup PDU plus 28
    /// bytes of IPv4 and UDP headers).
    mbps: f64,
    /// The control thread's share of one core; NaN without a CPU reading.
    cpu_pct: f64,
}

/// A step's figures over `window`, the span the flood threads send in (the
/// step's `--seconds`): `sent` datagrams, and the control thread's CPU
/// seconds read at the window's start and end. Not over the time the step
/// took: its last probe may run up to a second past the window, and dividing
/// by that understated every rate and CPU share.
fn step_figures(sent: u64, window: Duration, cpu: (Option<f64>, Option<f64>)) -> StepFigures {
    let seconds = window.as_secs_f64();
    let offered_pps = sent as f64 / seconds;
    StepFigures {
        offered_pps,
        mbps: offered_pps * (56.0 + 28.0) * 8.0 / 1e6,
        cpu_pct: match cpu {
            (Some(a), Some(b)) => (b - a) / seconds * 100.0,
            _ => f64::NAN,
        },
    }
}

fn main() -> std::process::ExitCode {
    let options = match parse() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            return std::process::ExitCode::from(2);
        }
    };

    let port = if options.port == 0 {
        free_port()
    } else {
        options.port
    };
    let target = SocketAddr::from((Ipv4Addr::LOCALHOST, port));

    // A temp directory for the key file, cleaned up at the end.
    let dir = std::env::temp_dir().join(format!("udpstp-flood-{}", std::process::id()));
    if let Err(error) = std::fs::create_dir_all(&dir) {
        eprintln!("flood_cost: temp dir: {error}");
        return std::process::ExitCode::FAILURE;
    }
    let key_file = dir.join("keys.toml");
    if let Err(error) = write_key_file(&key_file) {
        eprintln!("flood_cost: key file: {error}");
        let _ = std::fs::remove_dir_all(&dir);
        return std::process::ExitCode::FAILURE;
    }

    // Start the Server.
    let mut child = match Command::new(&options.server)
        .args(["server", "--key-file"])
        .arg(&key_file)
        .args([
            "--listen",
            "127.0.0.1",
            "--port",
            &port.to_string(),
            "--log-format",
            "json",
            "--statistics-interval",
            "1",
            "--max-hops",
            "12",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            eprintln!("flood_cost: could not start {}: {error}", options.server);
            let _ = std::fs::remove_dir_all(&dir);
            return std::process::ExitCode::FAILURE;
        }
    };
    let pid = child.id();

    let state = Arc::new(Mutex::new(LogState::default()));
    if let Some(stdout) = child.stdout.take() {
        let state = state.clone();
        std::thread::spawn(move || read_log(stdout, state));
    }
    // Drain stderr so the Server never blocks on a full pipe; echo it.
    if let Some(stderr) = child.stderr.take() {
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                eprintln!("server: {line}");
            }
        });
    }

    // Wait for the Server to listen.
    let ready_by = Instant::now() + Duration::from_secs(5);
    while Instant::now() < ready_by {
        if state.lock().expect("log state").listening {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if !state.lock().expect("log state").listening {
        eprintln!("flood_cost: the Server did not report listening within 5 s");
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);
        return std::process::ExitCode::FAILURE;
    }

    let hz = clk_tck();
    let shared = flood_common::parse_hex(KEY_HEX).expect("valid key hex");
    let probe_shared = flood_common::parse_hex(PROBE_KEY_HEX).expect("valid key hex");
    let key = KeyMaterial {
        shared: shared.clone(),
        key_id: KEY_ID,
    };
    let sources = match bind_sources(options.sources, target) {
        Ok(sources) => sources,
        Err(error) => {
            eprintln!("flood_cost: bind sources: {error}");
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_dir_all(&dir);
            return std::process::ExitCode::FAILURE;
        }
    };

    println!(
        "# flood_cost: kind={} seconds={} sources={} flood_threads={} probe_every={}ms",
        options.kind.name(),
        options.seconds,
        sources.len(),
        options.flood_threads,
        options.probe_interval.as_millis()
    );
    println!(
        "# CPU is the Server control thread's share of one core ({}).",
        if cfg!(windows) {
            "Windows Get-Process".to_string()
        } else {
            format!("Linux /proc, CLK_TCK={hz}")
        }
    );
    println!(
        "{:>10}  {:>10}  {:>9}  {:>7}  {:>10}  {:>10}  {:>10}  {:>6}  {:>7}  {:>7}  {:>7}  {:>7}",
        "target",
        "offered",
        "Mbit/s",
        "ctrlCPU",
        "queuedrop",
        "admitted",
        "budgetdr",
        "probes",
        "setup%",
        "probe%",
        "p50ms",
        "p95ms"
    );

    let key_holder = options.kind.needs_key();
    let mut probe_mc: u16 = 1;
    let mut probe_host: u32 = 0x7f_d2_00_02; // 127.210.0.2, apart from the flood pool

    for &rate in &options.rates {
        // Snapshot before.
        let cpu0 = control_cpu_seconds(pid, hz);
        let (drops0, adm0, rl0) = {
            let s = state.lock().expect("log state");
            (s.drops, s.test_connections, s.rate_limited)
        };
        let t0 = Instant::now();
        let until = t0 + Duration::from_secs(options.seconds);
        let stop = AtomicBool::new(false);

        // Flood threads share the bound sockets, each sending from its own
        // chunk of them: no socket is cloned, so a step opens no descriptors
        // beyond the probe's one.
        let step = std::thread::scope(|scope| {
            let mut handles = Vec::new();
            if rate > 0 {
                let threads = options.flood_threads.min(sources.len());
                for (t, chunk) in sources.chunks(sources.len().div_ceil(threads)).enumerate() {
                    let per_thread =
                        rate / threads as u64 + u64::from((rate % threads as u64) > t as u64);
                    let config = FloodConfig {
                        kind: options.kind,
                        key_id: KEY_ID,
                        key: key_holder.then(|| key.clone()),
                        rate_pps: per_thread,
                        mc_seed: (t as u32).wrapping_mul(0x9E37_79B9),
                    };
                    let stop = &stop;
                    handles.push(scope.spawn(move || flood_until(chunk, &config, until, stop)));
                }
            }

            // The control thread's CPU at the window's end, read on its own
            // thread so the step's last probe, which may run on past the
            // window, doesn't delay it. On Windows the reading lands the few
            // hundred milliseconds PowerShell takes after the window.
            let cpu_at_end = scope.spawn(move || {
                std::thread::sleep(until.saturating_duration_since(Instant::now()));
                control_cpu_seconds(pid, hz)
            });

            // Probes on this thread until the step ends.
            let mut probes = Probes::default();
            while Instant::now() < until {
                let second = now_unix();
                let ckey = *udpstp_wire::auth::derive_keys(&probe_shared, second).client();
                match probe(target, &ckey, probe_host, probe_mc) {
                    Ok(result) => {
                        probes.attempts += 1;
                        probes.setup_ok += u32::from(result.setup_ok);
                        if result.activated {
                            probes.activated += 1;
                            if let Some(latency) = result.latency {
                                probes.latencies.push(latency);
                            }
                        }
                    }
                    Err(error) => {
                        probes.error = Some(error);
                        break;
                    }
                }
                probe_mc = probe_mc.wrapping_add(1).max(1);
                probe_host = probe_host.wrapping_add(1);
                std::thread::sleep(options.probe_interval);
            }

            // Stop the flood.
            stop.store(true, Ordering::Relaxed);
            let mut sent = 0u64;
            let mut flood_failed = false;
            for handle in handles {
                match handle.join() {
                    Ok(count) => sent += count,
                    Err(_) => flood_failed = true,
                }
            }
            let cpu1 = cpu_at_end.join().unwrap_or(None);
            (sent, flood_failed, probes, cpu1)
        });
        let (sent, flood_failed, probes, cpu1) = step;
        if flood_failed || probes.error.is_some() {
            match &probes.error {
                Some(error) => eprintln!("flood_cost: a probe could not run: {error}"),
                None => eprintln!("flood_cost: a flood thread failed"),
            }
            eprintln!("flood_cost: stopping: this step's figures would measure the harness");
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_dir_all(&dir);
            return std::process::ExitCode::FAILURE;
        }
        let Probes {
            mut latencies,
            attempts,
            setup_ok,
            activated,
            ..
        } = probes;

        // Let watchdogs clear admitted-but-unactivated Test Connections, so
        // the next step starts from a quiet Server, and let the Server's log
        // report the step whole before its counts are read: the Statistics
        // come each second, and the log limiter holds DatagramsDropped back
        // for up to its 10 s window before a Suppressed rollup counts them.
        std::thread::sleep(COOL_DOWN);
        let (drops1, adm1, rl1) = {
            let s = state.lock().expect("log state");
            (s.drops, s.test_connections, s.rate_limited)
        };

        let StepFigures {
            offered_pps,
            mbps,
            cpu_pct,
        } = step_figures(sent, until - t0, (cpu0, cpu1));
        latencies.sort();
        let p50 = percentile(&latencies, 0.50);
        let p95 = percentile(&latencies, 0.95);
        // A share of the probes; NaN when none ran.
        let pct = |n: u32| n as f64 / attempts as f64 * 100.0;
        let ms = |d: Option<Duration>| d.map_or(f64::NAN, |d| d.as_secs_f64() * 1000.0);

        println!(
            "{:>10}  {:>10.0}  {:>9.1}  {:>6.1}%  {:>10}  {:>10}  {:>10}  {:>6}  {:>6.0}%  {:>6.0}%  {:>7.1}  {:>7.1}",
            rate,
            offered_pps,
            mbps,
            cpu_pct,
            drops1.saturating_sub(drops0),
            adm1.saturating_sub(adm0),
            rl1.saturating_sub(rl0),
            attempts,
            pct(setup_ok),
            pct(activated),
            ms(p50),
            ms(p95),
        );
    }

    // Print the final refusal tally by reason, for context.
    {
        let s = state.lock().expect("log state");
        if !s.refusals.is_empty() {
            println!("# cumulative refusals by reason:");
            for (reason, count) in &s.refusals {
                println!("#   {reason}: {count}");
            }
        }
        println!(
            "# cumulative: admitted={} evicted={} rate_limited={} auth_failures={} setup_ignored={} queue_drops={}",
            s.test_connections,
            s.evicted,
            s.rate_limited,
            s.authentication_failures,
            s.setup_requests_ignored,
            s.drops
        );
    }

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);
    std::process::ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    //  (review finding F7): a step's rates and CPU share are over the
    // flood's own window, however long the step's last probe runs past it.
    #[test]
    fn step_figures_are_over_the_flood_window() {
        let figures = step_figures(80_000, Duration::from_secs(4), (Some(10.0), Some(12.0)));
        assert!((figures.offered_pps - 20_000.0).abs() < 1e-9);
        assert!((figures.mbps - 13.44).abs() < 1e-9);
        assert!((figures.cpu_pct - 50.0).abs() < 1e-9);
    }

    #[test]
    fn step_figures_without_a_cpu_reading_leave_the_cpu_share_out() {
        let figures = step_figures(4_000, Duration::from_secs(4), (Some(1.0), None));
        assert!((figures.offered_pps - 1_000.0).abs() < 1e-9);
        assert!(figures.cpu_pct.is_nan());
    }
}
