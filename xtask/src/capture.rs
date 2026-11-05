//! `cargo xtask capture`: captures the Control PDUs an OB-UDPST Client and
//! Server exchange, for known-answer tests. Linux only; it runs the
//! pinned build from `scripts/obudpst-build.sh`.
//!
//! Capturing packets needs root, so instead the Server listens on
//! 127.0.0.2 and a relay on 127.0.0.1 passes every datagram between it and
//! the Client, recording each. When the Server's Setup Response names its
//! testPort, the relay listens on that port too, so the Client's
//! Activation Request comes through it. Load and Status PDUs pass through
//! unrecorded, at a Fixed-Rate row low enough for the relay to keep up.

use std::fmt::Write as _;
use std::net::{SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const VECTORS: &str = "crates/udpstp-wire/tests/vectors/obudpst-control.txt";

/// The Shared Key, as OB-UDPST writes it in a key file, and its keyId.
const KEY: &str = "udpstp-kat-key";
const KEY_ID: u8 = 5;

/// What in the captures is OB-UDPST's own; the known-answer test checks it.
const FLAGS: &str = "# OB-UDPST-specific content, checked by crates/udpstp-wire/tests/obudpst_control_kat.rs:\n\
         # - protocolVer 20; mcIdent OB-UDPST's random identifier; modifierBitmap CHSR_JUMBO_STATUS;\n\
         # - the Activation Requests carry OB-UDPST's default parameters, srIndexConf 10 with\n\
         #   CHTA_SRIDX_ISSTART clear (a Fixed-Rate Test, which OB-UDPST allows anyone) and\n\
         #   useOwDelVar False;\n\
         # - OB-UDPST's reserved-field extensions are zero in every PDU here.";

const RELAY: &str = "127.0.0.1";
const SERVER: &str = "127.0.0.2";

/// One run: a name, the build variant, and the Client's options.
struct Run {
    name: &'static str,
    variant: &'static str,
    client: &'static [&'static str],
}

const RUNS: &[Run] = &[
    Run {
        name: "downstream",
        variant: "auth",
        client: &["-d", "-I", "10", "-t", "5"],
    },
    Run {
        name: "upstream-max-bandwidth",
        variant: "auth",
        client: &["-u", "-I", "10", "-B", "20", "-t", "5"],
    },
    Run {
        name: "downstream-checksum",
        variant: "auth-checksum",
        client: &["-d", "-I", "10", "-t", "5"],
    },
];

pub fn run() -> Result<(), String> {
    if !cfg!(target_os = "linux") {
        return Err("`cargo xtask capture` runs on Linux (or in WSL) only".into());
    }
    let root = crate::workspace_root();
    let builds = std::env::var_os("UDPSTP_OBUDPST_DIR")
        .map_or_else(|| root.join("target/obudpst"), PathBuf::from)
        .join("v9.1.0");
    let stamp = std::fs::read_to_string(builds.join("auth/stamp")).map_err(|e| {
        format!(
            "no OB-UDPST build in {}: run scripts/obudpst-build.sh ({e})",
            builds.display()
        )
    })?;
    let key_file = std::env::temp_dir().join("udpstp-kat.keys");
    std::fs::write(&key_file, format!("{KEY_ID} {KEY}\n")).map_err(|e| e.to_string())?;

    let mut out = String::new();
    let _ = writeln!(
        out,
        "# Control PDUs captured from OB-UDPST v9.1.0 (commit ceb8917d6b8cf1089ff488e19e06c2b52cfbff85),\n\
         # built by scripts/obudpst-build.sh with:\n#   {}\n\
         # Captured by `cargo xtask capture` (xtask/src/capture.rs) through a UDP relay on\n\
         # loopback; do not edit by hand. Auth mode 1; the Server reads a key file with\n\
         # the line `{KEY_ID} {KEY}`, and the Client runs `udpst -a {KEY} -y {KEY_ID} <options>`.\n\
         # Format: `run <name> <client options>`, then one line per PDU in the order sent:\n\
         # `<setup-request|setup-response|activation-request|activation-response> <hex>`.\n\
         {FLAGS}",
        stamp.lines().collect::<Vec<_>>().join("\n#   ")
    );
    for run in RUNS {
        let pdus = capture(&builds.join(run.variant).join("udpst"), &key_file, run)?;
        let _ = writeln!(out, "\nrun {} {}", run.name, run.client.join(" "));
        for (kind, bytes) in pdus {
            let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            let _ = writeln!(out, "{kind} {hex}");
        }
    }
    let path = root.join(VECTORS);
    std::fs::write(&path, out).map_err(|e| format!("writing {}: {e}", path.display()))?;
    println!("wrote {}", path.display());
    Ok(())
}

/// Kills a child process when dropped.
struct Killed(Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Runs one Test through the relay and returns its Control PDUs.
fn capture(
    udpst: &std::path::Path,
    key_file: &std::path::Path,
    run: &Run,
) -> Result<Vec<(&'static str, Vec<u8>)>, String> {
    let control = UdpSocket::bind((RELAY, 0)).map_err(|e| e.to_string())?;
    let port = control.local_addr().map_err(|e| e.to_string())?.port();
    let upstream = UdpSocket::bind((RELAY, 0)).map_err(|e| e.to_string())?;
    let server = Killed(
        Command::new(udpst)
            .args(["-1", "-K"])
            .arg(key_file)
            .args(["-p", &port.to_string(), SERVER])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("starting {}: {e}", udpst.display()))?,
    );
    std::thread::sleep(Duration::from_millis(500));
    let mut client = Killed(
        Command::new(udpst)
            .args(["-a", KEY, "-y", &KEY_ID.to_string()])
            .args(run.client)
            .args(["-p", &port.to_string(), RELAY])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("starting the Client: {e}"))?,
    );
    for socket in [&control, &upstream] {
        socket.set_nonblocking(true).map_err(|e| e.to_string())?;
    }
    let server_control: SocketAddr = format!("{SERVER}:{port}").parse().unwrap();
    let mut client_addr: Option<SocketAddr> = None;
    // The relay's socket on the Server's testPort, and that port.
    let mut test: Option<(UdpSocket, SocketAddr)> = None;
    let mut pdus = Vec::new();
    let mut buf = vec![0u8; 65536];
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if client.0.try_wait().map_err(|e| e.to_string())?.is_some() {
            break;
        }
        if Instant::now() > deadline {
            return Err(format!("{}: the Client didn't end", run.name));
        }
        let mut idle = true;
        while let Ok((len, from)) = control.recv_from(&mut buf) {
            idle = false;
            client_addr = Some(from);
            record(&mut pdus, &buf[..len], true);
            let _ = upstream.send_to(&buf[..len], server_control);
        }
        if let Some((socket, server_test)) = &test {
            while let Ok((len, from)) = socket.recv_from(&mut buf) {
                idle = false;
                client_addr = Some(from);
                record(&mut pdus, &buf[..len], true);
                let _ = upstream.send_to(&buf[..len], *server_test);
            }
        }
        while let Ok((len, from)) = upstream.recv_from(&mut buf) {
            idle = false;
            let bytes = &buf[..len];
            record(&mut pdus, bytes, false);
            let Some(client_addr) = client_addr else {
                continue;
            };
            if from == server_control {
                if let Some(test_port) = setup_response_port(bytes)
                    && test.is_none()
                {
                    let socket = UdpSocket::bind((RELAY, test_port))
                        .map_err(|e| format!("the relay can't take testPort {test_port}: {e}"))?;
                    socket.set_nonblocking(true).map_err(|e| e.to_string())?;
                    test = Some((socket, SocketAddr::new(from.ip(), test_port)));
                }
                let _ = control.send_to(bytes, client_addr);
            } else if let Some((socket, _)) = &test {
                let _ = socket.send_to(bytes, client_addr);
            }
        }
        if idle {
            std::thread::sleep(Duration::from_micros(200));
        }
    }
    drop(server);
    let kinds: Vec<&str> = pdus.iter().map(|(kind, _)| *kind).collect();
    if kinds
        != [
            "setup-request",
            "setup-response",
            "activation-request",
            "activation-response",
        ]
    {
        return Err(format!("{}: captured {kinds:?}", run.name));
    }
    Ok(pdus)
}

/// The testPort of an accepting Setup Response.
fn setup_response_port(bytes: &[u8]) -> Option<u16> {
    (bytes.len() == 56 && bytes[..2] == [0xAC, 0xE1] && bytes[8] == 2 && bytes[9] == 1)
        .then(|| u16::from_be_bytes([bytes[12], bytes[13]]))
}

/// Keeps a Control PDU: a Setup PDU by its pduId, length and cmdRequest,
/// an Activation PDU by its pduId, length and sender. Load and Status PDUs
/// aren't kept.
fn record(pdus: &mut Vec<(&'static str, Vec<u8>)>, bytes: &[u8], from_client: bool) {
    let kind = match (bytes.get(..2), bytes.len(), bytes.get(8), from_client) {
        (Some([0xAC, 0xE1]), 56, Some(1), true) => "setup-request",
        (Some([0xAC, 0xE1]), 56, Some(2), false) => "setup-response",
        (Some([0xAC, 0xE2]), 104, _, true) => "activation-request",
        (Some([0xAC, 0xE2]), 104, _, false) => "activation-response",
        _ => return,
    };
    pdus.push((kind, bytes.to_vec()));
}
