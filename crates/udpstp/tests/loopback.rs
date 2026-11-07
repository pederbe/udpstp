//! Seam 2b: the `udpstp` process over loopback sockets. A real Client and
//! Server complete the Control Phase (docs/spec.md Testing Decisions; RFC 9946
//! §6, §7).
//!
//! Most tests run unauthenticated, which the Server allows; the Setup and
//! Activation exchanges are otherwise the same. The key file tests use auth
//! mode 1.

mod common;
#[path = "common/human_output.rs"]
mod human_output;
#[path = "common/key_file.rs"]
mod key_file;

use std::io::{BufRead, BufReader, Read as _};
use std::net::{Ipv6Addr, SocketAddr, UdpSocket};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use common::run_bounded;
use human_output::prose;
use key_file::KeyFile;

/// OB-UDPST's ErrorStatus for configuration errors (docs/spec.md).
const CONFIGURATION_ERROR: i32 = 50;

/// Asks a Linux Load Sender for kernel send timestamps, which change how it
/// sends, and so the fast paths it reports: the processes these
/// tests start go without it, whatever the environment says, unless a test
/// asks for it.
const KERNEL_TX_TIMESTAMPS: &str = "UDPSTP_KERNEL_TX_TIMESTAMPS";

fn udpstp() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_udpstp"));
    command.env_remove(KERNEL_TX_TIMESTAMPS);
    command
}

/// A UDP port that was free on both IPv4 and IPv6 loopback a moment ago.
fn free_port() -> u16 {
    loop {
        let v4 = UdpSocket::bind("127.0.0.1:0").expect("IPv4 loopback");
        let port = v4.local_addr().expect("bound").port();
        if UdpSocket::bind((Ipv6Addr::LOCALHOST, port)).is_ok() || !ipv6_loopback() {
            return port;
        }
    }
}

/// The first of `len` consecutive UDP ports in 20000-30000 that are free
/// on IPv4 and IPv6 loopback right now.
fn free_block(len: u16) -> u16 {
    let free = |port: u16| {
        UdpSocket::bind(("127.0.0.1", port)).is_ok()
            && (!ipv6_loopback() || UdpSocket::bind((Ipv6Addr::LOCALHOST, port)).is_ok())
    };
    (20000..30000)
        .step_by(usize::from(len))
        .find(|&first| (first..first + len).all(free))
        .expect("a free block of ports in 20000-30000")
}

fn ipv6_loopback() -> bool {
    UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).is_ok()
}

/// A running `udpstp server`, killed when dropped.
struct Server {
    child: Child,
    port: u16,
    /// The Server's output lines after "listening".
    output: mpsc::Receiver<String>,
}

impl Server {
    /// Ordinary tests listen on loopback unless they give their own addresses
    ///. Wildcard listeners have their own tests below.
    fn start(extra: &[&str]) -> Self {
        let mut args = extra.to_vec();
        if !args.contains(&"--listen") {
            args.extend(["--listen", "127.0.0.1"]);
            if ipv6_loopback() {
                args.extend(["--listen", "::1"]);
            }
        }
        Self::start_with_listeners(&args)
    }

    /// Starts a Server that allows unauthenticated Tests, and waits until it
    /// reports that it listens. A free port can be taken in the meantime, so
    /// a Server that exits at once is retried on another port. Without
    /// `--listen`, this exercises the Server's default wildcard listeners.
    fn start_with_listeners(extra: &[&str]) -> Self {
        let mut failures = Vec::new();
        for _ in 0..3 {
            match Self::try_start(extra) {
                Ok(server) => return server,
                Err(failure) => failures.push(failure),
            }
        }
        panic!("the Server didn't start: {failures:#?}");
    }

    fn try_start(extra: &[&str]) -> Result<Self, String> {
        let port = free_port();
        let mut child = udpstp()
            .args([
                "server",
                "--allow-unauthenticated",
                "--port",
                &port.to_string(),
            ])
            .args(extra)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the Server starts");
        let stdout = child.stdout.take().expect("piped");
        let (lines, received) = mpsc::channel();
        // Keep draining the Server's output for as long as it runs.
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = lines.send(line);
            }
        });
        loop {
            match received.recv_timeout(Duration::from_secs(10)) {
                Ok(line) if line.contains("listening") => break,
                Ok(_) => continue,
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let mut stderr = String::new();
                    if let Some(mut pipe) = child.stderr.take() {
                        let _ = pipe.read_to_string(&mut stderr);
                    }
                    return Err(format!("port {port}: {stderr}"));
                }
            }
        }
        Ok(Self {
            child,
            port,
            output: received,
        })
    }

    /// The Server's output up to the first line containing `text`, or
    /// everything within `limit` if none does.
    fn output_until(&self, text: &str, limit: Duration) -> String {
        let until = std::time::Instant::now() + limit;
        let mut output = String::new();
        while let Some(left) = until.checked_duration_since(std::time::Instant::now()) {
            let Ok(line) = self.output.recv_timeout(left) else {
                break;
            };
            output.push_str(&line);
            output.push('\n');
            if line.contains(text) {
                break;
            }
        }
        output
    }

    /// Waits until the Server logs the end of the Test it was running: its
    /// TestSummary, which it logs once it has released every Test
    /// Connection of it. Only then does a Server with the default one Test
    /// per Source admit another Test from this host.
    fn wait_for_the_test_summary(&self) {
        let log = self.output_until("TestSummary", Duration::from_secs(10));
        assert!(
            log.contains("TestSummary"),
            "the Server ended the Test:\n{log}"
        );
    }

    /// Stops the Server and returns what it wrote to stderr.
    fn stop(mut self) -> String {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let mut stderr = String::new();
        if let Some(mut pipe) = self.child.stderr.take() {
            let _ = pipe.read_to_string(&mut stderr);
        }
        stderr
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Runs the Client with a 3 s time limit and the shortest Test Interval,
/// 5 s, unless the test gives them. A Test then ends in about 5.6 s (the
/// Server stops it 0.5 s after its Test Interval); the process is killed
/// at 15 s, leaving room for a slow CI runner, if the Client doesn't stop
/// by itself.
fn client(args: &[&str]) -> Output {
    let mut command = udpstp();
    command.arg("client");
    if !args.contains(&"--time-limit") {
        command.args(["--time-limit", "3"]);
    }
    if !args.contains(&"--test-interval") {
        command.args(["--test-interval", "5"]);
    }
    command.args(args);
    let output = run_bounded(command, Duration::from_secs(15));
    // The Client sends its Setup Request once. Tests running alongside can
    // load loopback enough for the OS to drop it, so a Client that got no
    // answer at all is run once more ('s one retry).
    if output.status.code() == Some(200) && stderr(&output).contains("no answer from") {
        eprintln!("retrying a Client that got no answer: {}", stderr(&output));
        let mut again = udpstp();
        again.arg("client");
        if !args.contains(&"--time-limit") {
            again.args(["--time-limit", "3"]);
        }
        if !args.contains(&"--test-interval") {
            again.args(["--test-interval", "5"]);
        }
        again.args(args);
        return run_bounded(again, Duration::from_secs(15));
    }
    output
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The testPort the Client reported from the Setup Response.
fn test_port(output: &Output) -> Option<u16> {
    stdout(output)
        .lines()
        .find_map(|line| line.strip_prefix("Setup accepted (Ephemeral Port "))
        .and_then(|port| port.trim_end_matches(')').trim().parse().ok())
}

fn assert_activated(output: &Output, what: &str) {
    assert!(
        output.status.success(),
        "{what}: status {:?}\nstdout: {}\nstderr: {}",
        output.status,
        stdout(output),
        stderr(output)
    );
    assert!(
        stdout(output).contains("Test activated"),
        "{what}: {}",
        stdout(output)
    );
}

// Conformance Record: setup-response-from-another-address. Isolated
// loopback peers send fresh matching authenticated PDUs from an alternate
// address. A signed Null Request cannot authorize it either.
#[test]
fn only_locally_authorized_alternates_receive_activation_and_load() {
    use udpstp_wire::auth::{derive_keys, hmac_sha256};

    // RFC 9946 Figures 3, 4 and 6: construct peer bytes directly, without
    // the PDU codecs used by the Client under test.
    fn sign_control(
        pdu: &mut [u8],
        digest: std::ops::Range<usize>,
        checksum: std::ops::Range<usize>,
        key: &[u8; 32],
    ) {
        pdu[digest.clone()].fill(0);
        pdu[checksum].fill(0);
        let signature = hmac_sha256(key, pdu);
        pdu[digest].copy_from_slice(&signature);
    }

    // As in replies_leave_from_the_address_the_setup_request_arrived_on:
    // macOS needs an explicitly configured second loopback address.
    if UdpSocket::bind("127.0.0.2:0").is_err() {
        eprintln!("skipped: 127.0.0.2 isn't a local address here");
        return;
    }

    let keys = KeyFile::new(
        "alternate-destination",
        "keys.toml",
        "[[key]]\nadmin_key_name = \"fixture\"\nkey_id = 7\nkey_text = \"a shared key\"\n",
    );
    for mode in ["1", "2"] {
        for (authorized, correct_port) in [(false, true), (true, true), (true, false)] {
            let expected_traffic = authorized && correct_port;
            let selected = UdpSocket::bind("127.0.0.1:0").unwrap();
            let selected_addr = selected.local_addr().unwrap();
            let alternate = if correct_port {
                UdpSocket::bind(("127.0.0.2", selected_addr.port())).unwrap()
            } else {
                loop {
                    let socket = UdpSocket::bind("127.0.0.2:0").unwrap();
                    if socket.local_addr().unwrap().port() != selected_addr.port() {
                        break socket;
                    }
                }
            };
            let ephemeral = UdpSocket::bind("127.0.0.2:0").unwrap();
            selected
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            ephemeral
                .set_read_timeout(Some(Duration::from_millis(750)))
                .unwrap();
            let peer = std::thread::spawn(move || {
                let mut bytes = [0u8; 2048];
                let (len, client) = selected.recv_from(&mut bytes).unwrap();
                assert_eq!(len, 56);
                assert_eq!(&bytes[..2], &[0xAC, 0xE1]);
                let mut response = bytes[..len].to_vec();
                let time = u32::from_be_bytes(response[16..20].try_into().unwrap());
                let derived = derive_keys(b"a shared key", time);
                let mut request = response.clone();
                sign_control(&mut request, 20..52, 54..56, derived.client());
                assert_eq!(request, response, "authenticated Setup Request");
                let mut null = [0u8; 48];
                null[..2].copy_from_slice(&[0xDE, 0xAD]);
                null[2..4].copy_from_slice(&response[2..4]);
                null[4] = 1;
                null[7] = response[15];
                null[8..12].copy_from_slice(&response[16..20]);
                null[44] = response[52];
                sign_control(&mut null, 12..44, 46..48, derived.server());
                ephemeral.send_to(&null, client).unwrap();
                response[8] = 2;
                response[9] = 1;
                response[12..14]
                    .copy_from_slice(&ephemeral.local_addr().unwrap().port().to_be_bytes());
                sign_control(&mut response, 20..52, 54..56, derived.server());
                alternate.send_to(&response, client).unwrap();
                let received = ephemeral.recv_from(&mut bytes);
                if !expected_traffic {
                    assert!(
                        matches!(received, Err(ref error) if matches!(
                            error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        )),
                        "unauthorized destination received traffic: {received:?}"
                    );
                    return;
                }
                let (len, source) = received.unwrap();
                assert_eq!(source, client);
                assert_eq!(len, 104);
                let mut activation = bytes[..len].to_vec();
                let mut request = activation.clone();
                sign_control(&mut request, 68..100, 102..104, derived.client());
                assert_eq!(request, activation, "authenticated Activation Request");
                activation[5] = 1;
                activation[28..56].fill(0);
                activation[28..32].copy_from_slice(&10_000u32.to_be_bytes());
                activation[32..36].copy_from_slice(&256u32.to_be_bytes());
                activation[36..40].copy_from_slice(&1u32.to_be_bytes());
                sign_control(&mut activation, 68..100, 102..104, derived.server());
                ephemeral.send_to(&activation, client).unwrap();
                let (len, source) = ephemeral.recv_from(&mut bytes).unwrap();
                assert_eq!(source, client);
                assert!(bytes[..len].starts_with(&[0xBE, 0xEF]), "expected Load PDU");
            });
            let target = selected_addr.to_string();
            let authorization = format!("{target}=127.0.0.2");
            let mut command = udpstp();
            command.args([
                "client",
                "--key-file",
                keys.path(),
                "--auth-mode",
                mode,
                "--direction",
                "upstream",
                "--time-limit",
                "1",
                "--test-interval",
                "5",
            ]);
            if authorized {
                command.args(["--allow-server-address", &authorization]);
            }
            command.arg(&target);
            let output = run_bounded(command, Duration::from_secs(10));
            peer.join().unwrap();
            assert_eq!(
                stdout(&output).contains("Test activated"),
                expected_traffic,
                "mode {mode}, authorized {authorized}, correct port {correct_port}: {}\n{}",
                stdout(&output),
                stderr(&output)
            );
            assert_eq!(test_port(&output).is_some(), expected_traffic);
        }
    }
}

// docs/spec.md (the Server listens with Tokio, on IPv4 and IPv6-only sockets,
// ADR 0007)
#[udpstp_req::req(
    "RFC9946-6.1-1",
    "RFC9946-6.2.2-1",
    "RFC9946-7.1-1",
    "RFC9097-4-L5",
    "RFC9097-5.3-8",
    "RFC9097-6.3-9",
    "RFC9097-7.3-3",
    "RFC9946-4-1"
)]
#[test]
fn a_client_completes_the_control_phase_over_ipv4_loopback() {
    let server = Server::start_with_listeners(&[]);

    let output = client(&["--unauthenticated", &format!("127.0.0.1:{}", server.port)]);

    assert_activated(&output, "IPv4");
    assert!(test_port(&output).is_some_and(|p| p != server.port));
    // F is 1 unless the Client user asks for more.
    assert!(
        prose(&stdout(&output)).contains("1 Test Connection (F)"),
        "{}",
        stdout(&output)
    );
}

// The Server's startup warnings reach its stderr, and a Test runs with the
// MaxHops and DF it was given; . Which warnings a Server gives, the
// one for an unset MaxHops (RFC 9097 §8.3) among them, is tested at
// `start::server`'s interface.
#[test]
fn the_servers_startup_warnings_reach_stderr() {
    let unset = Server::start(&[]).stop();
    assert!(
        prose(&unset).contains("max_hops (--max-hops) is not set"),
        "{unset}"
    );

    let server = Server::start(&["--max-hops", "8", "--dont-fragment=false"]);
    let output = client(&[
        "--unauthenticated",
        "--max-hops",
        "8",
        "--dont-fragment=false",
        &format!("127.0.0.1:{}", server.port),
    ]);
    assert_activated(&output, "MaxHops 8, fragmentation allowed");
    let set = server.stop();
    assert!(!set.contains("max_hops"), "{set}");
}

// The Setup Response leaves from the address the Setup Request was sent
// to; ADR 0007;
#[udpstp_req::req("RFC9097-5.3-9")]
#[test]
fn replies_leave_from_the_address_the_setup_request_arrived_on() {
    // A second loopback address: Linux and Windows answer on all of
    // 127.0.0.0/8; macOS only where one was added, so a host without it
    // skips the test.
    if std::net::UdpSocket::bind("127.0.0.2:0").is_err() {
        eprintln!("skipped: 127.0.0.2 isn't a local address here");
        return;
    }
    let server = Server::start_with_listeners(&[]);
    // The Client drops a Setup Response from any address but the one it
    // sent to, and sends its Activation Request to that address and the
    // Test Connection's port.
    let output = client(&["--unauthenticated", &format!("127.0.0.2:{}", server.port)]);
    if !output.status.success() {
        // It has failed only under the pre-push hook's load: say
        // what the Server saw.
        let seen = server.output_until("\u{0}", Duration::from_secs(1));
        let stderr = server.stop();
        eprintln!("the Server's output:\n{seen}\nits stderr:\n{stderr}");
    }
    assert_activated(&output, "via 127.0.0.2");
}

// docs/spec.md story 24 (JSON compatible with OB-UDPST's schema);
#[test]
fn the_client_prints_json_results() {
    let server = Server::start(&[]);
    let output = client(&[
        "--unauthenticated",
        "--direction",
        "upstream",
        "--format",
        "json",
        &format!("127.0.0.1:{}", server.port),
    ]);
    assert!(output.status.success(), "{}", stderr(&output));
    // Only JSON on stdout; progress went to stderr.
    let json: serde_json::Value = serde_json::from_str(&stdout(&output))
        .unwrap_or_else(|e| panic!("{e}: {}", stdout(&output)));
    assert_eq!(json["Input"]["Role"], "Sender");
    assert_eq!(json["Output"]["Status"], "Complete");
    assert!(
        json["Output"]["IncrementalResult"]
            .as_array()
            .is_some_and(|r| !r.is_empty())
    );
    assert!(json["udpstp"]["CapacityModes"].is_array());
    assert!(prose(&stderr(&output)).contains("Sending the Setup Request"));
    // : the socket buffers the Test Connection got at its start, which
    // its Data Phase Engine reports with the rest of how it ran as it exits:
    // the default size asked for, and what the OS granted.
    let buffers = &json["udpstp"]["SocketBuffers"];
    assert_eq!(buffers["Requested"], 2 * 1024 * 1024, "{buffers}");
    assert!(
        ["Receive", "Send"]
            .iter()
            .all(|size| buffers[size].as_u64().is_some_and(|bytes| bytes > 0)),
        "{buffers}"
    );

    // A Test that gets no answer still prints its ErrorStatus.
    let port = free_port();
    let output = client(&[
        "--unauthenticated",
        "--time-limit",
        "1",
        "--format",
        "json",
        "--json-extension=false",
        &format!("127.0.0.1:{port}"),
    ]);
    assert_eq!(output.status.code(), Some(200));
    let json: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(json["ErrorStatus"], 200);
    assert!(json.get("Output").is_none() && json.get("udpstp").is_none());
    assert!(json["ErrorMessage"].as_str().is_some_and(|m| !m.is_empty()));
}

// docs/spec.md story 82 (client addresses in the log in full, pseudonymized
// or truncated);
#[test]
fn the_server_log_shows_client_addresses_as_configured() {
    let key = KeyFile::new("privacy", "pseudonym.key", &"5a".repeat(16));
    for (args, shown, hidden) in [
        (vec![], "127.0.0.1:", "anon-"),
        (
            vec!["--address-privacy", "truncate"],
            "127.0.0.0/24",
            "127.0.0.1",
        ),
        (
            vec![
                "--address-privacy",
                "pseudonymize",
                "--address-privacy-key-file",
                key.path(),
            ],
            "anon-",
            "127.0.0.1",
        ),
    ] {
        let mut server_args = vec!["--log-format", "json"];
        server_args.extend(args.iter().copied());
        let server = Server::start(&server_args);
        let output = client(&["--unauthenticated", &format!("127.0.0.1:{}", server.port)]);
        assert_activated(&output, "with address privacy");
        let log = server.output_until("TestSummary", Duration::from_secs(10));
        let clients: Vec<String> = log
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter_map(|e| e["client"].as_str().map(str::to_owned))
            .collect();
        assert!(
            !clients.is_empty()
                && clients
                    .iter()
                    .all(|c| c.starts_with(shown) && !c.contains(hidden)),
            "{args:?}: {clients:?}"
        );
    }
    // Pseudonyms need a key.
    let mut no_key = udpstp();
    no_key.args(["server", "--port", &free_port().to_string()]);
    no_key.args(["--address-privacy", "pseudonymize"]);
    let output = run_bounded(no_key, Duration::from_secs(10));
    assert_eq!(
        output.status.code(),
        Some(CONFIGURATION_ERROR),
        "{}",
        stderr(&output)
    );
}

// : the Client reads its pseudonym key once, at the start, so a key
// file removed while the Test runs changes nothing: the results come out
// with the Test's exit status, where the Client used to read the file
// again for them and panic (exit 101)
#[test]
fn a_pseudonym_key_file_removed_during_the_test_changes_nothing() {
    let server = Server::start(&[]);
    let key = KeyFile::new("vanishing", "pseudonym.key", &"5a".repeat(16));
    let mut child = udpstp()
        .args(["client", "--unauthenticated", "--format", "json"])
        .args(["--time-limit", "3", "--test-interval", "5"])
        .args(["--address-privacy", "pseudonymize"])
        .args(["--address-privacy-key-file", key.path()])
        .arg(format!("127.0.0.1:{}", server.port))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the Client starts");
    let mut stdout = child.stdout.take().expect("piped");
    let printed = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        text
    });
    // With JSON results, the progress lines go to stderr.
    let (lines, received) = mpsc::channel();
    let stderr = child.stderr.take().expect("piped");
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = lines.send(line);
        }
    });
    let mut progress = String::new();
    while !progress.contains("Test activated") {
        let Ok(line) = received.recv_timeout(Duration::from_secs(10)) else {
            let _ = child.kill();
            panic!("the Test wasn't activated: {progress}");
        };
        progress.push_str(&line);
        progress.push('\n');
    }
    drop(key);
    let until = std::time::Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.try_wait().expect("waitable") {
            break status;
        }
        if std::time::Instant::now() > until {
            let _ = child.kill();
            panic!("the Client didn't end: {progress}");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let printed = printed.join().expect("stdout read");
    let rest: String = received.try_iter().collect::<Vec<_>>().join("\n");
    assert!(status.success(), "{status:?}: {progress}{rest}");
    let json: serde_json::Value =
        serde_json::from_str(&printed).unwrap_or_else(|e| panic!("{e}: {printed}"));
    // Completed, perhaps with a warning.
    assert!(
        json["ErrorStatus"]
            .as_u64()
            .is_some_and(|status| status < 50),
        "{json:#}"
    );
}

// docs/spec.md stories 83 and 90 (structured logs in JSON; events an
// attacker can trigger are rate-limited);
#[test]
fn the_server_logs_json_and_limits_a_flood() {
    let server = Server::start(&["--log-format", "json"]);
    let output = client(&["--unauthenticated", &format!("127.0.0.1:{}", server.port)]);
    assert_activated(&output, "with a JSON log");
    // A flood of malformed Setup Requests.
    let flood = UdpSocket::bind("127.0.0.1:0").unwrap();
    for _ in 0..40 {
        flood
            .send_to(b"not a Setup PDU", ("127.0.0.1", server.port))
            .unwrap();
    }
    let log = server.output_until("never logged", Duration::from_secs(2));
    let events: Vec<serde_json::Value> = log
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {line}")))
        .collect();
    let named = |name: &str| events.iter().filter(|e| e["event"] == name).count();
    assert!(named("TestConnectionActivated") == 1, "{log}");
    // Loopback loses none of the 40: exactly a burst is logged.
    let dropped = named("SetupRequestSilentlyRejected");
    assert_eq!(dropped, 10, "{log}");
}

//
#[udpstp_req::req("RFC9946-12.3.8-L1", "RFC9946-8.1-8")]
#[test]
fn a_client_can_ask_for_algorithm_c() {
    let server = Server::start(&[]);

    let output = client(&[
        "--unauthenticated",
        "--algorithm",
        "c",
        &format!("127.0.0.1:{}", server.port),
    ]);

    assert_activated(&output, "algorithm C");
}

// docs/spec.md story 11;
#[udpstp_req::req("RFC9946-7.2-1", "RFC9946-7.2.1-L1")]
#[test]
fn a_client_asks_for_a_dscp_the_server_allows() {
    // Two Tests per Source: the second Client's Test is admitted however
    // soon the Server releases the first's.
    let server = Server::start(&["--allowed-dscp", "0,46", "--tests-per-source", "2"]);
    let allowed = client(&[
        "--unauthenticated",
        "--dscp",
        "46",
        &format!("127.0.0.1:{}", server.port),
    ]);
    assert_activated(&allowed, "DSCP 46");
    // Not coerced, and the results say what the traffic carried.
    let marked = if cfg!(unix) {
        "DSCP 46 agreed"
    } else {
        "DSCP 0 agreed"
    };
    assert!(
        !prose(&stdout(&allowed)).contains("The Server set the DSCP")
            && prose(&stdout(&allowed)).contains(marked),
        "{}",
        stdout(&allowed)
    );

    let coerced = client(&[
        "--unauthenticated",
        "--dscp",
        "10",
        &format!("127.0.0.1:{}", server.port),
    ]);
    assert_activated(&coerced, "DSCP 10, coerced");
    // Where traffic can't be marked, the Client asks for 0 instead.
    let (said, output) = if cfg!(unix) {
        (
            "The Server set the DSCP to 0 (asked for 10)",
            stdout(&coerced),
        )
    } else {
        ("asking for DSCP 0", stderr(&coerced))
    };
    assert!(prose(&output).contains(said), "{output}");
}

// Both ends use the optional checksum;
#[udpstp_req::req("RFC9946-5.6-6")]
#[test]
fn a_test_activates_with_checksums_on_both_ends() {
    let server = Server::start(&["--checksum"]);

    let output = client(&[
        "--unauthenticated",
        "--checksum",
        &format!("127.0.0.1:{}", server.port),
    ]);

    assert_activated(&output, "checksums");
}

// ADR 0007 (an IPv6-only listener beside the IPv4 one)
#[test]
fn a_client_completes_the_control_phase_over_ipv6_loopback() {
    if !ipv6_loopback() {
        eprintln!("skipped: this host has no IPv6 loopback");
        return;
    }
    let server = Server::start_with_listeners(&[]);

    let output = client(&["--unauthenticated", &format!("[::1]:{}", server.port)]);

    assert_activated(&output, "IPv6");
}

// docs/spec.md story 4 (a host name resolves to the first address the
// resolver returns)
#[udpstp_req::req("RFC9946-4-L3")]
#[test]
fn a_client_resolves_a_host_name() {
    let server = Server::start(&[]);

    let output = client(&["--unauthenticated", &format!("localhost:{}", server.port)]);

    assert_activated(&output, "localhost");
}

// docs/spec.md story 74 (the Ephemeral Port range is configurable)
#[udpstp_req::req("RFC9097-10-2", "RFC9946-11-2")]
#[test]
fn test_connections_use_ports_from_the_configured_range() {
    // Below every OS's ephemeral range (Linux starts at 32768, Windows and
    // macOS at 49152), so a port the OS picked itself can't land in it.
    let first = free_block(21);
    let range = format!("{first}-{}", first.saturating_add(20));
    // The range holds the cap of 14 plus the unactivated share of 7.
    let server = Server::start(&[
        "--ephemeral-ports",
        &range,
        "--max-test-connections",
        "14",
        "--test-connections-per-test",
        "7",
    ]);

    let output = client(&["--unauthenticated", &format!("127.0.0.1:{}", server.port)]);

    assert_activated(&output, "range");
    let port = test_port(&output);
    assert!(
        port.is_some_and(|p| (first..=first.saturating_add(20)).contains(&p)),
        "Ephemeral Port {port:?} is in {range}"
    );
}

// docs/spec.md (Unauthenticated Mode only when both ends opt in; the
// Client otherwise needs keys)
#[test]
fn a_client_without_authentication_configured_does_not_start() {
    let server = Server::start(&[]);

    let output = client(&[&format!("127.0.0.1:{}", server.port)]);

    assert_eq!(
        output.status.code(),
        Some(CONFIGURATION_ERROR),
        "{}",
        stderr(&output)
    );
}

#[udpstp_req::req("RFC9946-6.1-1", "RFC9946-12.1-L1")]
#[test]
fn the_client_uses_the_standard_port_by_default() {
    let server: SocketAddr = "127.0.0.1:24601".parse().unwrap();
    // Nothing needs to listen: the Client reports where it sends the Setup
    // Request, then gives up.
    let output = client(&["--unauthenticated", "--time-limit", "1", "127.0.0.1"]);

    assert!(
        stdout(&output).contains(&format!("Sending the Setup Request to {server}")),
        "{}",
        stdout(&output)
    );
}

// docs/spec.md (ErrorStatus 200 when no answer came, as OB-UDPST reports it)
#[test]
fn a_client_that_gets_no_answer_gives_up_at_its_time_limit() {
    let port = free_port();

    let output = client(&[
        "--unauthenticated",
        "--time-limit",
        "1",
        &format!("127.0.0.1:{port}"),
    ]);

    // ErrorStatus 200: no answer from the Server, so the Test didn't
    // complete, as OB-UDPST ends up reporting.
    assert_eq!(output.status.code(), Some(200), "{}", stderr(&output));
}

// ADR 0006 (only local errors stop a socket's reader); docs/spec.md story
// 85 (floods of unverified traffic can't stop the Server)
#[test]
fn an_oversized_datagram_does_not_stop_the_server() {
    let server = Server::start(&[]);
    let sender = UdpSocket::bind("127.0.0.1:0").expect("a socket");
    sender
        .send_to(&[0xAB; 3000], ("127.0.0.1", server.port))
        .expect("sent");
    // Larger ones where the OS sends them: macOS caps outgoing UDP at 9216
    // bytes by default (net.inet.udp.maxdgram).
    for len in [9000, 65_507] {
        let _ = sender.send_to(&vec![0xAB; len], ("127.0.0.1", server.port));
    }

    let output = client(&["--unauthenticated", &format!("127.0.0.1:{}", server.port)]);

    assert_activated(&output, "after oversized datagrams");
}

// docs/spec.md "Key table" (TOML Key Tables with lifetimes, OB-UDPST key files
// loaded directly)
#[udpstp_req::req("RFC9946-5.3-1", "RFC9946-5.3.1-1")]
#[test]
fn a_mode1_test_runs_with_a_toml_key_table_and_an_obudpst_key_file() {
    // The same key written both ways: "shared" is 736861726564 in hex.
    let toml = KeyFile::new(
        "loopback-toml",
        "keys.toml",
        "[[key]]\nadmin_key_name = \"lab\"\nkey_id = 4\nkey_hex = \"736861726564\"\n\
         send_lifetime_start = \"20200101000000Z\"\naccept_lifetime_start = \"20200101000000Z\"\n",
    );
    // A CR, a comment and what follows a NUL byte end the key, as in
    // OB-UDPST; keyId 4 is written in hexadecimal.
    let obudpst = KeyFile::new(
        "loopback-ob",
        "udpst.keys",
        "1,other\n0x4,shared\0tail\r\n5,more # comment\n",
    );
    for (what, server_keys, client_keys, extra) in [
        (
            "Server TOML, Client OB-UDPST",
            &toml,
            &obudpst,
            vec!["--key-id", "4"],
        ),
        ("Server OB-UDPST, Client TOML", &obudpst, &toml, vec![]),
    ] {
        let server = Server::start(&["--key-file", server_keys.path()]);
        let target = format!("127.0.0.1:{}", server.port);
        let mut args = vec!["--key-file", client_keys.path()];
        args.extend(extra);
        args.push(&target);

        let output = client(&args);

        assert_activated(&output, what);
    }
}

//
#[udpstp_req::req("RFC9946-5.3-2", "RFC9946-5.3.2-1", "RFC9946-5.3.2-2", "RFC9946-5.4-1")]
#[test]
fn a_mode2_test_completes_both_ways() {
    let keys = KeyFile::new(
        "loopback-mode2",
        "keys.toml",
        "[[key]]
admin_key_name = \"ops\"
key_id = 3
key_text = \"operator\"
operator = true
",
    );
    for direction in ["downstream", "upstream"] {
        let server = Server::start(&["--key-file", keys.path()]);
        let output = client(&[
            "--key-file",
            keys.path(),
            "--auth-mode",
            "2",
            "--direction",
            direction,
            "--fixed-rate",
            "5",
            &format!("127.0.0.1:{}", server.port),
        ]);
        assert!(
            output.status.success() && stdout(&output).contains("Test outcome: completed"),
            "{direction}: {:?}
{}
{}",
            output.status,
            stdout(&output),
            stderr(&output)
        );
        assert!(
            !prose(&stdout(&output)).contains("failed authentication"),
            "{direction}: {}",
            stdout(&output)
        );
    }
}

// docs/spec.md "Key table" (OB-UDPST key files load directly: their keys
// are bytes, not necessarily UTF-8)
#[test]
fn an_obudpst_key_need_not_be_utf8() {
    let server_keys = KeyFile::new("loopback-latin1", "udpst.keys", "");
    std::fs::write(server_keys.path(), b"2,caf\xe9\n").expect("written");
    server_keys.set_mode(0o600);
    let client_keys = KeyFile::new(
        "loopback-latin1-client",
        "keys.toml",
        "[[key]]\nadmin_key_name = \"latin1\"\nkey_id = 2\nkey_hex = \"636166e9\"\n",
    );
    let server = Server::start(&["--key-file", server_keys.path()]);

    let output = client(&[
        "--key-file",
        client_keys.path(),
        &format!("127.0.0.1:{}", server.port),
    ]);

    assert_activated(&output, "a Latin-1 key");
}

// RFC 7210 §3 (a key is accepted only within its accept lifetime);
// ADR 0010 (outside it the Setup Request is dropped silently)
#[test]
fn the_server_ignores_a_key_past_its_accept_lifetime() {
    let client_keys = KeyFile::new("loopback-client", "udpst.keys", "4,shared\n");
    let expired = KeyFile::new(
        "loopback-expired",
        "keys.toml",
        "[[key]]\nadmin_key_name = \"old\"\nkey_id = 4\nkey_text = \"shared\"\n\
         accept_lifetime_end = \"20200101000000Z\"\n",
    );
    let server = Server::start(&["--key-file", expired.path()]);

    let output = client(&[
        "--key-file",
        client_keys.path(),
        "--time-limit",
        "1",
        &format!("127.0.0.1:{}", server.port),
    ]);

    assert_eq!(output.status.code(), Some(200), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("no answer") && test_port(&output).is_none(),
        "no Setup Response: {}",
        stderr(&output)
    );
    // The same key without the ended lifetime is accepted.
    let current = KeyFile::new("loopback-current", "udpst.keys", "4,shared\n");
    let server = Server::start(&["--key-file", current.path()]);
    let output = client(&[
        "--key-file",
        client_keys.path(),
        &format!("127.0.0.1:{}", server.port),
    ]);
    assert_activated(&output, "the same key, still valid");
}

// docs/spec.md  (Downstream and Upstream Fixed-Rate Tests with an
// Operator Key complete over loopback; admission is released when the
// engine exits); RFC 9946 §4.1, §9
#[udpstp_req::req("RFC9946-5.1-18", "RFC9097-2-4", "RFC9097-7.4-1", "RFC9097-8-1")]
#[test]
fn fixed_rate_tests_complete_over_loopback() {
    let keys = KeyFile::new(
        "loopback-operator",
        "keys.toml",
        "[[key]]\nadmin_key_name = \"ops\"\nkey_id = 3\nkey_text = \"operator\"\noperator = true\n",
    );
    for direction in ["downstream", "upstream"] {
        let server = Server::start(&[
            "--key-file",
            keys.path(),
            "--log-format",
            "json",
            "--statistics-interval",
            "1",
        ]);

        let output = client(&[
            "--key-file",
            keys.path(),
            "--direction",
            direction,
            "--fixed-rate",
            "5",
            &format!("127.0.0.1:{}", server.port),
        ]);

        assert!(
            output.status.success(),
            "{direction}: status {:?}\nstdout: {}\nstderr: {}",
            output.status,
            stdout(&output),
            stderr(&output)
        );
        assert!(
            prose(&stdout(&output)).contains("The Server ended the Test")
                && prose(&stdout(&output)).contains("Test outcome: completed"),
            "{direction}: the STOP exchange completed: {}",
            stdout(&output)
        );
        // : the headline result, at the fixed 5 Mbps, with the Sender
        // Bit Rate: measured where the Client sent, else estimated.
        let text = prose(&stdout(&output));
        let headline = text
            .split_once("Maximum IP-Layer Capacity: ")
            .map(|(_, rest)| rest.split("Minimum one-way delay ").next().unwrap_or(rest))
            .unwrap_or_default();
        assert!(
            headline.contains(" Mbps (Sub-Interval ")
                && headline.contains("IP-Layer Sender Bit Rate")
                && headline.contains("estimated IP-Layer Sender Bit Rate")
                    == (direction == "downstream"),
            "{direction}: {}",
            stdout(&output)
        );
        // At row 5, 5 Mbps: no rate above it. A busy host can lose Load
        // PDUs even on loopback, so a Sub-Interval may miss the criterion
        // and the maximum be one that ran lower.
        let rates: Vec<f64> = headline
            .split(" Mbps")
            .filter_map(|part| part.rsplit(' ').next()?.parse().ok())
            .collect();
        assert!(
            !rates.is_empty() && rates.iter().all(|r| (0.5..=5.5).contains(r)),
            "{direction}: {}",
            stdout(&output)
        );
        let printed = server.output_until("TestSummary", Duration::from_secs(10));
        let events: Vec<serde_json::Value> = printed
            .lines()
            .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {line}")))
            .collect();
        let event = |name: &str| events.iter().find(|e| e["event"] == name);
        let received = match direction {
            "downstream" => "FirstStatusPduReceived",
            _ => "FirstLoadPduReceived",
        };
        assert!(
            event(received).is_some()
                && event("DataPhaseEnded").is_some_and(|e| e["reason"] == "Completed")
                && event("TestConnectionReleased").is_some(),
            "{direction}: the Server's engine completed the Test and released the Test \
             Connection:\n{printed}"
        );
        // docs/spec.md story 81 (a summary record per Test);
        let summary = event("TestSummary").expect("a TestSummary");
        let rate = summary["ip_layer_sender_bit_rate"].as_u64();
        let sent = summary["bytes_sent"].as_u64().unwrap_or_default();
        let received = summary["bytes_received"].as_u64().unwrap_or_default();
        // : a Server that kept its row's rate, leaving less than a
        // hundredth of its Load PDUs unsent in Skipped or Withheld Bursts,
        // draws no warning that it may not have; nor does an Upstream Test,
        // whose Client sends. (A busy host can make the Server fall further
        // behind, and then the warning may rightly come.)
        let not_sent: u64 = ["skipped_bytes", "withheld_bytes"]
            .iter()
            .filter_map(|key| summary[*key].as_u64())
            .sum();
        if direction == "upstream" || not_sent * 100 < sent {
            assert!(
                !prose(&stdout(&output)).contains("may not have kept up"),
                "{direction}: {not_sent} of {sent} bytes not sent: {}",
                stdout(&output)
            );
        }
        let (expected, measured) = match direction {
            // The Server sent at no more than 5 Mbps: its Sender Bit Rate
            // says so. On a loaded host it may fall behind; pacing
            // precision is the nightly tier's.
            "downstream" => (
                "Downstream",
                sent > 0 && received > 0 && rate.is_some_and(|r| (1..=6_000_000).contains(&r)),
            ),
            _ => ("Upstream", sent == 0 && received > 0 && rate.is_none()),
        };
        assert!(
            summary["result"] == "Completed"
                && summary["key_id"] == 3
                && summary["test_connections"] == 1
                && summary["direction"] == expected
                && summary["test_interval"].as_u64().is_some()
                && summary["dscp"] == 0
                && measured,
            "{direction}: {summary}"
        );
        // docs/spec.md story 81 (periodic counters);
        let counters =
            server.output_until(r#""active_test_connections":0"#, Duration::from_secs(10));
        assert!(
            counters
                .lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .any(|e| e["event"] == "Statistics"
                    && e["tests"] == 1
                    && e["active_test_connections"] == 0),
            "{direction}: {printed}{counters}"
        );
    }
}

/// The fast paths a Linux Data Phase Engine sends Load PDUs, reads
/// Status PDUs and waits with over loopback, which offloads
/// UDP GSO; none elsewhere.
const LOAD_SENDER_FAST_PATHS: &[&str] = if cfg!(target_os = "linux") {
    &[
        "sendmmsg",
        "UdpGso",
        "recvmmsg",
        "KernelRxTimestamps",
        "ppoll",
    ]
} else {
    &[]
};

/// The fast paths a Linux Data Phase Engine reads Load PDUs and
/// waits with; none elsewhere.
const LOAD_RECEIVER_FAST_PATHS: &[&str] = if cfg!(target_os = "linux") {
    &["recvmmsg", "KernelRxTimestamps", "ppoll"]
} else {
    &[]
};

// : asked with UDPSTP_KERNEL_TX_TIMESTAMPS=1, a Linux Load Sender
// reports how much later than their lpduTime the kernel sent its Load PDUs
// (experimental; the nightly precision test holds it to 100 µs at p99).
// Loopback on a busy host is no measure of that, so this only asks that
// every stamped Load PDU went within 100 ms. : asked with
// `kernel-count`, it reports them too, matched by the kernel's own count
// on every kernel, so its batches go in sendmmsg calls, which `FastPaths`
// names; with `1`, from Linux 6.13 each message goes with a sendmsg call
// of its own.
#[cfg(target_os = "linux")]
#[test]
fn kernel_send_timestamps_are_reported_when_asked_for() {
    let keys = KeyFile::new(
        "loopback-tx-timestamps",
        "keys.toml",
        "[[key]]
admin_key_name = \"ops\"
key_id = 3
key_text = \"operator\"
operator = true
",
    );
    let server = Server::start(&["--key-file", keys.path()]);
    let address = format!("127.0.0.1:{}", server.port);
    for asked in ["1", "kernel-count"] {
        let mut command = udpstp();
        command
            .env(KERNEL_TX_TIMESTAMPS, asked)
            .args(["client", "--time-limit", "3", "--test-interval", "5"])
            .args(["--key-file", keys.path(), "--direction", "upstream"])
            .args(["--fixed-rate", "50", "--format", "json", &address]);
        let output = run_bounded(command, Duration::from_secs(15));
        assert!(output.status.success(), "{asked}: {}", stderr(&output));
        let results: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON");
        let stamps = &results["udpstp"]["KernelSendTimestamps"];
        // Row 50: 5 Load PDUs a 1 ms Burst, one GSO message each, for 5 s.
        let samples = stamps["Samples"].as_u64().unwrap_or(0);
        assert!(samples > 2500, "{asked}: {stamps}");
        assert!(stamps["LagP50Us"].as_u64() <= stamps["LagP99Us"].as_u64());
        assert!(
            stamps["LagMaxUs"].as_u64().is_some_and(|us| us < 100_000),
            "{asked}: {stamps}"
        );
        // The kernel's count sends batches in sendmmsg calls on every
        // kernel.
        if asked == "kernel-count" {
            let paths = &results["udpstp"]["FastPaths"];
            assert!(
                paths
                    .as_array()
                    .is_some_and(|paths| paths.iter().any(|path| path == "sendmmsg")),
                "{paths}"
            );
        }
    }
    // Not asked for: not reported.
    let output = client(
        &["--key-file", keys.path(), "--direction", "upstream"]
            .into_iter()
            .chain(["--fixed-rate", "50", "--format", "json", &address])
            .collect::<Vec<_>>(),
    );
    let results: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON");
    assert!(results["udpstp"]["KernelSendTimestamps"].is_null());
}

//  (docs/spec.md S2b: batched sending and GSO over real sockets): at row 25
// each 1 ms Burst is two 1222-byte Load PDUs and a 597-byte Add-on
// (Transmitter 2's burstSize2 2 of udpPayload2 1222, and udpAddon2 597),
// which Linux sends as one UDP GSO message. In both directions:
// - every Load PDU arrives once and in order, from lpduSeqNo 1: none lost,
//   reordered or duplicated in any Sub-Interval, and none invalid;
// - each goes at its size, the Add-on at its own: the Load PDUs' mean
//   size, their bytes over their count, is the row's (2 × 1222 + 597) / 3,
//   where a missing or full-size Add-on makes 1222 and a smaller one less.
//   Downstream it is what the Server sent, Upstream what the Server
//   received and counted;
// - the Load Receiver counts each at its full length, reading header-only
//   on Linux: over its Sub-Intervals it counted the bytes the Load
//   Sender sent over those 5 s;
// - no Load Sender sends above the row's 25 Mbps;
// - the results and the Server's log name the fast paths used.
//
// None of it depends on the host keeping the row's schedule: a
// loaded Load Sender skips or withholds whole Bursts, which leaves the
// mean size as it was, and both ends count the same Load PDUs. That it
// keeps the row's rate is the precision suite's check
// (scripts/precision.py, row 25).
#[udpstp_req::req("RFC9946-7.1-L9", "RFC9946-8.1-L6", "RFC9946-8.1-L7")]
#[test]
fn batched_load_pdus_arrive_whole_in_order_and_at_their_sizes() {
    // Mbps: the row's 25, with room for Bursts sent late into the next
    // Sub-Interval.
    const ROW_MBPS: f64 = 25.5;
    // The row's mean Load PDU size, UDP payload bytes: each Burst's two
    // 1222-byte Load PDUs and its 597-byte Add-on.
    const ROW_MEAN_BYTES: f64 = (2.0 * 1222.0 + 597.0) / 3.0;
    // Each Load PDU's IPv4 and UDP headers, which the IP-layer rates count
    // and the byte counts don't.
    const HEADER_BYTES: f64 = 28.0;
    // What the Load Receiver counted over its Sub-Intervals against what
    // the Load Sender sent over the Test Interval, 5 s from its first send,
    // within 10%: a Load PDU still unread when the last Sub-Interval closed
    // falls outside (a loaded Load Receiver reads behind: 2% at most under
    // 16 busy loops in WSL), and the Sub-Intervals end later than 5 s by
    // each Trial Interval a late Status PDU moved them on. Counting only the
    // headers a header-only read took would be 3%, and counting a GSO
    // message's whole length for each of its datagrams 300%.
    let counted_what_was_sent =
        |received: f64, sent: f64| sent > 0.0 && received >= 0.9 * sent && received <= 1.1 * sent;
    let keys = KeyFile::new(
        "loopback-batched",
        "keys.toml",
        "[[key]]\nadmin_key_name = \"ops\"\nkey_id = 3\nkey_text = \"operator\"\noperator = true\n",
    );
    for direction in ["downstream", "upstream"] {
        let server = Server::start(&["--key-file", keys.path(), "--log-format", "json"]);
        let output = client(&[
            "--key-file",
            keys.path(),
            "--direction",
            direction,
            "--fixed-rate",
            "25",
            "--format",
            "json",
            &format!("127.0.0.1:{}", server.port),
        ]);
        assert!(
            output.status.success(),
            "{direction}: status {:?}\nstdout: {}\nstderr: {}",
            output.status,
            stdout(&output),
            stderr(&output)
        );
        let results: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("JSON results");
        let sub_intervals = results["Output"]["IncrementalResult"]
            .as_array()
            .expect("Sub-Intervals");
        assert!(!sub_intervals.is_empty(), "{direction}: {results}");
        for s in sub_intervals {
            assert!(
                s["LossCount"] == 0 && s["ReorderedCount"] == 0 && s["ReplicatedCount"] == 0,
                "{direction}: every Load PDU arrived, in order, once: {s}"
            );
        }
        let log = server.output_until("TestSummary", Duration::from_secs(10));
        let events: Vec<serde_json::Value> = log
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        let summary = events
            .iter()
            .find(|e| e["event"] == "TestSummary")
            .unwrap_or_else(|| panic!("{direction}: a TestSummary: {log}"));
        let field = |name: &str| summary[name].as_f64().unwrap_or(0.0);
        if direction == "downstream" {
            // The Server sent; its log has what it sent and, from the
            // Client's Status PDUs, what the Client counted: UDP payload
            // bytes.
            let (sent, received) = (field("bytes_sent"), field("bytes_received"));
            assert!(
                counted_what_was_sent(received, sent),
                "{direction}: the Client counted {received} bytes of the {sent} sent: {summary}"
            );
            let rate = field("ip_layer_sender_bit_rate") / 1e6;
            assert!(
                rate > 0.0 && rate <= ROW_MBPS,
                "{direction}: the Server sent {rate} Mbps, no more than the row: {summary}"
            );
            // The mean size of what the Server sent over the Test
            // Interval's st, the IP-Layer Sender Bit Rate's grid, all of
            // which it fills by sending until the STOP exchange: the Load
            // PDUs' bytes, and at the IP-Layer Sender Bit Rate the same
            // Load PDUs with their headers, so their count.
            let ip_bytes = field("ip_layer_sender_bit_rate") * field("test_interval") / 8.0;
            let load_pdus = (ip_bytes - sent) / HEADER_BYTES;
            let mean = sent / load_pdus;
            assert!(
                load_pdus > 0.0 && (mean / ROW_MEAN_BYTES - 1.0).abs() <= 0.01,
                "{direction}: the Server's Load PDUs were {mean:.1} bytes on average, \
                 the row's {ROW_MEAN_BYTES:.1}: {summary}"
            );
        } else {
            // The Client sent; its results have the IP-Layer Sender Bit
            // Rate it measured and, from the Server's Status PDUs, the
            // IP-Layer Capacity the Server measured, Sub-Interval by
            // Sub-Interval.
            let sender = results["udpstp"]["IPLayerSenderBitRate"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let sent_in = |interval: &serde_json::Value| {
                sender
                    .iter()
                    .find(|s| s["SubInterval"] == *interval)
                    .and_then(|s| s["IPLayerSenderBitRate"].as_f64())
            };
            let pairs: Vec<(f64, f64)> = sub_intervals
                .iter()
                .filter_map(|s| Some((s["IPLayerCapacity"].as_f64()?, sent_in(&s["Interval"])?)))
                .collect();
            let received: f64 = pairs.iter().map(|&(c, _)| c).sum();
            let sent: f64 = pairs.iter().map(|&(_, b)| b).sum();
            assert!(
                pairs.len() >= 3 && counted_what_was_sent(received, sent),
                "{direction}: the Server counted {received} of the {sent} Mbps sent: {pairs:?}"
            );
            assert!(
                pairs.iter().all(|&(_, b)| b <= ROW_MBPS),
                "{direction}: the Client sent no more than the row: {pairs:?}"
            );
            // The mean size of what the Server received and counted over
            // its Sub-Intervals: the Load PDUs' bytes, in its log, and
            // their IP-Layer Capacity, with their headers, in the Client's
            // results, so their count. The Client learns every Sub-Interval
            // the Server closes, from its Status PDUs, unless the Client's
            // STOP2 was lost and the Server went on (it then ends its Data
            // Phase Unconfirmed, and may close one more). A capacity is
            // given to 0.01 Mbps, so a Sub-Interval's bytes to 625 in a
            // second, which the tolerance allows for.
            let confirmed = events
                .iter()
                .any(|e| e["event"] == "DataPhaseEnded" && e["reason"] == "Completed");
            if confirmed {
                let seconds = results["Input"]["TestSubInterval"].as_f64().unwrap_or(0.0) / 1000.0;
                let ip_bytes: f64 = sub_intervals
                    .iter()
                    .filter_map(|s| s["IPLayerCapacity"].as_f64())
                    .map(|mbps| mbps * 1e6 * seconds / 8.0)
                    .sum();
                let received = field("bytes_received");
                let load_pdus = (ip_bytes - received) / HEADER_BYTES;
                let rounding = sub_intervals.len() as f64 * 0.005e6 * seconds / 8.0 / HEADER_BYTES;
                let mean = received / load_pdus;
                assert!(
                    load_pdus > 0.0
                        && (mean / ROW_MEAN_BYTES - 1.0).abs() <= 0.01 + rounding / load_pdus,
                    "{direction}: the Server's Load PDUs were {mean:.1} bytes on average, \
                     the row's {ROW_MEAN_BYTES:.1}: {received} bytes, {ip_bytes} with \
                     their headers\n{summary}\n{results}"
                );
            } else {
                eprintln!(
                    "{direction}: the STOP exchange went unconfirmed, so the mean size isn't checked"
                );
            }
        }
        let warnings = results["udpstp"]["Warnings"].as_array().cloned();
        assert!(
            !events.iter().any(|e| e["event"] == "InvalidLoadPdus")
                && !warnings
                    .unwrap_or_default()
                    .iter()
                    .any(|w| w["Code"] == "InvalidLoadPdus"),
            "{direction}: no Load PDU was invalid:\n{log}\n{results}"
        );
        // Each end names its fast paths: the Server in its log, the
        // Client in its results.
        let (server_paths, client_paths) = match direction {
            "downstream" => (LOAD_SENDER_FAST_PATHS, LOAD_RECEIVER_FAST_PATHS),
            _ => (LOAD_RECEIVER_FAST_PATHS, LOAD_SENDER_FAST_PATHS),
        };
        assert_eq!(
            results["udpstp"]["FastPaths"],
            serde_json::json!(client_paths),
            "{direction}"
        );
        // The Linux Server's Engine Threads wait on their epoll sets, not
        // on each socket.
        let server_paths = server_paths
            .iter()
            .map(|&path| if path == "ppoll" { "epoll" } else { path })
            .collect::<Vec<_>>()
            .join(",");
        assert!(
            events
                .iter()
                .any(|e| e["event"] == "DataPhaseEnded" && e["fast_paths"] == server_paths),
            "{direction}: {log}"
        );
    }
}

// : Clients on one host, starting in the same second with the same
// key, draw different mcIdents, so the Server neither merges their Tests
// (MultiConnectionIndexInUse) nor drops a Setup Request as a replay of
// another's
#[test]
fn many_clients_on_one_host_starting_together_all_run() {
    const CLIENTS: usize = 12;
    let keys = KeyFile::new(
        "loopback-many-clients",
        "keys.toml",
        "[[key]]
admin_key_name = \"ops\"
key_id = 3
key_text = \"operator\"
operator = true
",
    );
    let tests = CLIENTS.to_string();
    let server = Server::start(&[
        "--key-file",
        keys.path(),
        "--tests-per-source",
        &tests,
        "--test-connections-per-test",
        "1",
    ]);
    let address = format!("127.0.0.1:{}", server.port);
    let outputs: Vec<Output> = std::thread::scope(|scope| {
        let clients: Vec<_> = (0..CLIENTS)
            .map(|_| {
                scope.spawn(|| client(&["--key-file", keys.path(), "--fixed-rate", "1", &address]))
            })
            .collect();
        clients.into_iter().map(|c| c.join().unwrap()).collect()
    });
    for (i, output) in outputs.iter().enumerate() {
        assert!(
            output.status.success() && stdout(output).contains("Test outcome: completed"),
            "Client {i}: {:?}
{}
{}",
            output.status,
            stdout(output),
            stderr(output)
        );
    }
}

// docs/spec.md stories 54 and 55 (a Multi-Connection Test, its Test
// Connections spread across Servers); . The Servers keep the default
// one Test per Source, so each Test's Test Connections must count as one
// Test, and each Test starts only once the Servers the one before it used
// have logged its end, however long they take to release it.
#[test]
fn a_multi_connection_test_runs_against_one_server_and_against_two() {
    let first = Server::start(&[]);
    let second = Server::start(&[]);
    // One live text round. Exact multi-Server text layout and Upstream
    // approximation wording are covered by test_results.rs transcripts.
    {
        let server = format!("127.0.0.1:{}", first.port);
        let output = client(&["--unauthenticated", "--test-connections", "2", &server]);
        let printed = stdout(&output);
        let messages = prose(&printed);
        assert!(
            output.status.success()
                && messages.contains("Test Connection 0: Test activated")
                && messages.contains("Test Connection 1: Test activated")
                && messages.matches(": Completed").count() == 2
                // : the Test's, summed, then each Test Connection's.
                && messages.contains("All 2 Test Connections together:")
                && printed.contains("Active")
                && printed.lines().filter(|line| line.trim_start().starts_with("Maximum IP-Layer Capacity: ")).count() == 3,
            "{server}: status {:?}
stdout: {printed}
stderr: {}",
            output.status,
            stderr(&output)
        );
        assert!(
            printed.contains(&format!("Test Connection 1 to {server}")),
            "the second Test Connection went to {server}: {printed}"
        );
        first.wait_for_the_test_summary();
    }
    let servers = [
        format!("127.0.0.1:{}", first.port),
        format!("127.0.0.1:{}", second.port),
    ];
    let assert_connections = |json: &serde_json::Value| {
        let each = json["udpstp"]["TestConnections"]
            .as_array()
            .unwrap_or_else(|| panic!("missing Test Connections: {json:#}"));
        assert_eq!(each.len(), 2, "{json:#}");
        for (index, server) in servers.iter().enumerate() {
            let connection = each
                .iter()
                .find(|c| c["McIndex"] == index)
                .unwrap_or_else(|| panic!("missing Test Connection {index}: {json:#}"));
            assert!(
                connection["Server"] == *server
                    && connection["EndReason"] == "Completed"
                    && connection["CapacityModes"].is_array(),
                "Test Connection {index} to {server}: {json:#}"
            );
        }
        assert!(
            json["Output"]["IncrementalResult"]
                .as_array()
                .is_some_and(|r| r.iter().any(|s| s["ActiveConnections"] == 2))
                && json["udpstp"]["EndReason"] == "Completed",
            "both Test Connections were active and completed: {json:#}"
        );
    };
    // docs/spec.md (: JSON of a Downstream Multi-Connection Test, with
    // OB-UDPST's connection counts)
    let output = client(&[
        "--unauthenticated",
        "--test-connections",
        "2",
        "--format",
        "json",
        &servers[0],
        &servers[1],
    ]);
    let json: serde_json::Value = serde_json::from_str(&stdout(&output))
        .unwrap_or_else(|e| panic!("{e}: {}", stdout(&output)));
    assert!(
        output.status.success()
            && json["Input"]["NumberOfConnections"] == 2
            && json["Input"]["NumberOfHosts"] == 2,
        "{json:#}"
    );
    assert_connections(&json);
    first.wait_for_the_test_summary();
    second.wait_for_the_test_summary();
    // docs/spec.md story 62: Upstream results summed, with the
    // approximation warning in the JSON extension, not an error.
    let output = client(&[
        "--unauthenticated",
        "--test-connections",
        "2",
        "--direction",
        "upstream",
        "--format",
        "json",
        &servers[0],
        &servers[1],
    ]);
    let json: serde_json::Value = serde_json::from_str(&stdout(&output))
        .unwrap_or_else(|e| panic!("{e}: {}", stdout(&output)));
    let warned = json["udpstp"]["Warnings"]
        .as_array()
        .is_some_and(|w| w.iter().any(|w| w["Code"] == "UpstreamResultsApproximate"));
    assert_connections(&json);
    assert!(
        output.status.success()
            && warned
            && !json["ErrorMessage"]
                .as_str()
                .unwrap_or_default()
                .contains("approximate"),
        "{json:#}"
    );
}

// : a Server listening on one address answers there only
#[test]
fn a_server_listens_only_on_the_addresses_given() {
    let server = Server::start(&["--listen", "127.0.0.1"]);
    let output = client(&["--unauthenticated", &format!("127.0.0.1:{}", server.port)]);
    assert_activated(&output, "on the address it listens on");
    let output = client(&[
        "--unauthenticated",
        "--time-limit",
        "1",
        &format!("127.0.0.2:{}", server.port),
    ]);
    assert!(
        test_port(&output).is_none() && !output.status.success(),
        "no answer on another address: {}",
        stdout(&output)
    );
}

// , where the host delivers limited broadcasts to itself
#[udpstp_req::req("RFC9946-6-2")]
#[test]
fn a_setup_response_sent_to_a_broadcast_address_is_ignored() {
    // Windows doesn't tell the Client where a datagram arrived (ADR 0007).
    if cfg!(windows) {
        return;
    }
    // A fake Server answers every Setup Request by broadcast.
    let server = UdpSocket::bind("127.0.0.1:0").unwrap();
    server.set_broadcast(true).unwrap();
    // Does this host deliver a limited broadcast to its own sockets?
    let probe = UdpSocket::bind("0.0.0.0:0").unwrap();
    probe
        .set_read_timeout(Some(Duration::from_millis(300)))
        .unwrap();
    let probe_port = probe.local_addr().unwrap().port();
    if server
        .send_to(b"probe", ("255.255.255.255", probe_port))
        .is_err()
        || probe.recv_from(&mut [0u8; 8]).is_err()
    {
        eprintln!("skipped: this host doesn't deliver broadcasts to itself");
        return;
    }
    let port = server.local_addr().unwrap().port();
    let answering = std::thread::spawn(move || {
        server
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut buf = [0u8; 2048];
        if let Ok((len, client)) = server.recv_from(&mut buf) {
            let mut response = buf[..len].to_vec();
            response[8] = 2; // Setup Response
            response[9] = 1; // ACK
            response[12..14].copy_from_slice(&50_000u16.to_be_bytes());
            let _ = server.send_to(&response, ("255.255.255.255", client.port()));
        }
    });
    let output = client(&[
        "--unauthenticated",
        "--time-limit",
        "1",
        &format!("127.0.0.1:{port}"),
    ]);
    answering.join().unwrap();
    assert!(
        test_port(&output).is_none()
            && prose(&stderr(&output)).contains("broadcast or multicast address and was ignored"),
        "stdout: {}
stderr: {}",
        stdout(&output),
        stderr(&output)
    );
}

//
#[udpstp_req::req("RFC9946-6.2.1-2")]
#[test]
fn the_jumbo_setting_must_match() {
    let server = Server::start(&["--jumbo=false"]);
    let refused = client(&["--unauthenticated", &format!("127.0.0.1:{}", server.port)]);
    assert_eq!(refused.status.code(), Some(103), "{}", stderr(&refused));
    let output = client(&[
        "--unauthenticated",
        "--jumbo=false",
        &format!("127.0.0.1:{}", server.port),
    ]);
    assert_activated(&output, "without jumbo on both ends");
}

// docs/spec.md (watchdog and non-graceful stops are shown in the Client's
// output); ADR 0002
#[udpstp_req::req("RFC9946-5.1-11", "RFC9946-5.1-14", "RFC9946-7.3-6")]
#[test]
fn the_client_reports_a_server_that_goes_away() {
    let server = Server::start(&[]);
    let mut child = udpstp()
        .args([
            "client",
            "--unauthenticated",
            "--test-interval",
            "10",
            &format!("127.0.0.1:{}", server.port),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the Client starts");
    // The Server goes away once the Data Phase runs.
    let mut stdout = BufReader::new(child.stdout.take().expect("piped"));
    let mut printed = String::new();
    let mut line = String::new();
    while stdout.read_line(&mut line).unwrap_or(0) > 0 {
        printed.push_str(&line);
        if line.contains("Test activated") {
            break;
        }
        line.clear();
    }
    assert!(printed.contains("Test activated"), "{printed}");
    drop(server);

    // Nothing is received for 1 s, then 3 s: the Client ends by itself
    // well within the 10 s Test Interval.
    let started = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("waited for") {
            break status;
        }
        if started.elapsed() > Duration::from_secs(8) {
            let _ = child.kill();
            panic!("the Client didn't end by its Watchdog");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    // ErrorStatus 200: Load PDUs stopped arriving and the Test didn't
    // complete, as OB-UDPST ends up reporting.
    assert_eq!(status.code(), Some(200), "{status:?}: {stderr}");
    assert!(
        prose(&stderr).contains("nothing received from the Server for 1 s")
            && stderr.contains("Watchdog"),
        "{stderr}"
    );
}

// docs/spec.md stories 8, 10, 11 and 15 (the Client's Test options reach the
// Server; a refused coercion ends the Test)
#[udpstp_req::req("RFC9946-7.2-1", "RFC9946-7.3-L1")]
#[test]
fn client_test_options_reach_the_server() {
    // Two Tests per Source: the second Client's Test is admitted however
    // soon the Server releases the first's.
    let server = Server::start(&["--tests-per-source", "2"]);

    let output = client(&[
        "--unauthenticated",
        "--start-row",
        "20",
        "--random-payload",
        "--delay-var",
        "rtt",
        &format!("127.0.0.1:{}", server.port),
    ]);
    assert_activated(&output, "options");
    assert!(
        !prose(&stdout(&output)).contains("The Server set"),
        "nothing coerced: {}",
        stdout(&output)
    );

    // Asked for a 120 s Test, the Server coerces it to 60 s, which the
    // Client was told to refuse.
    let refused = client(&[
        "--unauthenticated",
        "--test-interval",
        "120",
        "--refuse-coercion",
        "testIntTime",
        &format!("127.0.0.1:{}", server.port),
    ]);
    assert!(
        !refused.status.success() && stderr(&refused).contains("--refuse-coercion"),
        "status {:?}\nstderr: {}",
        refused.status,
        stderr(&refused)
    );
}

// : a Server with no key file and Unauthenticated Mode off drops
// every Setup Request, so it says so when it starts, and still starts.
// Which warnings a Server gives is tested at `start::server`'s interface
//; this checks the one a real Server gives.
#[test]
fn a_server_that_can_admit_no_test_says_so() {
    let mut child = udpstp()
        .args([
            "server",
            "--listen",
            "127.0.0.1",
            "--port",
            &free_port().to_string(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the Server starts");
    let stdout = child.stdout.take().expect("piped");
    let listening = BufReader::new(stdout)
        .lines()
        .map_while(Result::ok)
        .any(|line| line.contains("listening"));
    let _ = child.kill();
    let _ = child.wait();
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    assert!(listening, "{stderr}");
    assert!(
        prose(&stderr).contains("the Server can admit no Test"),
        "{stderr}"
    );
}

// : the Server doesn't say why it refuses an Activation Request; for
// a Fixed-Rate Test the likely reason is a key that isn't an Operator's.
#[test]
fn a_refused_fixed_rate_test_names_the_likely_reason() {
    let server = Server::start(&[]);
    let output = client(&[
        "--unauthenticated",
        "--fixed-rate",
        "10",
        &format!("127.0.0.1:{}", server.port),
    ]);
    assert_eq!(output.status.code(), Some(152), "{}", stderr(&output));
    assert!(
        prose(&stderr(&output)).contains("only an Operator Key may run a Fixed-Rate Test"),
        "{}",
        stderr(&output)
    );
}

// : the Server stops on SIGTERM, which it handles itself: as PID 1 in
// a container a signal without a handler would be ignored.
#[cfg(unix)]
#[test]
fn the_server_stops_on_sigterm() {
    let mut server = Server::start(&["--log-format", "json"]);
    let pid = server.child.id().to_string();
    let killed = Command::new("kill")
        .args(["-TERM", &pid])
        .status()
        .expect("kill runs");
    assert!(killed.success());
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = server.child.try_wait().expect("waits") {
            break status;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "still running 5 s after SIGTERM"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success(), "{status:?}");
    // It said why in its log.
    let log = server.output_until("Stopping", Duration::from_secs(1));
    assert!(
        log.lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .any(|e| e["event"] == "Stopping" && e["reason"] == "SIGTERM"),
        "{log}"
    );
}

/// Waits up to `limit` for `child` to exit, killing it after.
#[cfg(unix)]
fn wait_bounded(mut child: Child, limit: Duration) -> Output {
    let until = std::time::Instant::now() + limit;
    while child.try_wait().expect("waitable").is_none() {
        if std::time::Instant::now() > until {
            let _ = child.kill();
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    child.wait_with_output().expect("its output")
}

/// Sends `signal` to `child`.
#[cfg(unix)]
fn signal(child: &Child, signal: nix::sys::signal::Signal) {
    let pid = nix::unistd::Pid::from_raw(i32::try_from(child.id()).expect("a pid"));
    nix::sys::signal::kill(pid, signal).expect("the signal is sent");
}

/// A pipe whose buffer is already full of newlines: a child given its
/// write end as stdout blocks in its first write until the read end is
/// drained.
#[cfg(target_os = "linux")]
fn full_pipe() -> (std::io::PipeReader, std::io::PipeWriter) {
    use nix::fcntl::{FcntlArg, OFlag, fcntl};
    use std::io::{ErrorKind, Write as _};
    let (reader, mut writer) = std::io::pipe().expect("a pipe");
    let flags = OFlag::from_bits_truncate(fcntl(&writer, FcntlArg::F_GETFL).expect("its flags"));
    fcntl(&writer, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK)).expect("non-blocking");
    // Whole pages, then single bytes for anything left.
    for chunk in [&[b'\n'; 4096][..], b"\n"] {
        loop {
            match writer.write(chunk) {
                Ok(_) => {}
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) => panic!("filling the pipe: {e}"),
            }
        }
    }
    // Blocking again: the child shares this open file description.
    fcntl(&writer, FcntlArg::F_SETFL(flags)).expect("blocking");
    (reader, writer)
}

/// Whether `child` holds a UDP socket bound to the IPv4 wildcard address
/// at `port`: one of its open files is a socket `/proc/net/udp` lists
/// there, matched by inode, so another process's socket doesn't count.
#[cfg(target_os = "linux")]
fn holds_the_ipv4_wildcard(child: &Child, port: u16) -> bool {
    let local = format!("00000000:{port:04X}");
    let table = std::fs::read_to_string("/proc/net/udp").unwrap_or_default();
    // Each line's second field is its local address, its tenth its inode.
    let sockets: Vec<String> = table
        .lines()
        .skip(1)
        .map(|line| line.split_whitespace().collect::<Vec<_>>())
        .filter(|fields| fields.get(1) == Some(&local.as_str()))
        .filter_map(|fields| Some(format!("socket:[{}]", fields.get(9)?)))
        .collect();
    std::fs::read_dir(format!("/proc/{}/fd", child.id()))
        .into_iter()
        .flatten()
        .filter_map(|fd| std::fs::read_link(fd.ok()?.path()).ok())
        .any(|target| sockets.iter().any(|s| target.as_os_str() == s.as_str()))
}

/// A command for `udpstp` that starts it with SIGINT, SIGHUP and SIGTERM at
/// their default action. A test process started with one ignored, as under
/// `nohup` or by a shell's `cmd &`, passes the ignore on to its children,
/// and a Client or Server keeps a signal it started with ignored, so
/// a signal the test sent would do nothing. A shell can't reset an ignore
/// it started with (POSIX `trap`); `env --default-signal` can (GNU
/// coreutils from 8.31, and uutils). Elsewhere, `udpstp` as it is.
#[cfg(unix)]
fn udpstp_with_default_signals() -> Command {
    if cfg!(target_os = "linux") {
        let mut command = Command::new("env");
        command.env_remove(KERNEL_TX_TIMESTAMPS).args([
            "--default-signal=INT,HUP,TERM",
            env!("CARGO_BIN_EXE_udpstp"),
        ]);
        command
    } else {
        udpstp()
    }
}

/// A Server whose stdout is a pipe already full, so it can't get past its
/// first log line until the pipe is read, once it holds its Standard Port,
/// which it binds just before that line; and the pipe's read end. A free
/// port can be taken meanwhile: the Server then exits, and another is
/// tried.
#[cfg(target_os = "linux")]
fn server_held_at_its_first_log_line() -> (Child, std::io::PipeReader) {
    let mut failures = Vec::new();
    for _ in 0..3 {
        let port = free_port();
        let (reader, writer) = full_pipe();
        let mut command = udpstp_with_default_signals();
        command
            .args(["server", "--allow-unauthenticated", "--log-format", "json"])
            .args(["--port", &port.to_string()])
            .stdout(writer)
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("the Server starts");
        // Without the test's copy of the write end, the log ends with the
        // Server.
        drop(command);
        let until = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if holds_the_ipv4_wildcard(&child, port) {
                return (child, reader);
            }
            let running = child.try_wait().expect("waitable").is_none();
            if !running || std::time::Instant::now() > until {
                let _ = child.kill();
                let output = child.wait_with_output().expect("its output");
                failures.push(format!("port {port}: {}", stderr(&output)));
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    panic!("the Server didn't bind its Standard Port: {failures:#?}");
}

// : the Server installs its signal handlers before it logs that it
// listens, so a signal that comes as it says so stops it cleanly, with
// its log's Stopping and last Statistics, not by the signal's default
// action. No timing is involved: the Server's stdout is a full pipe, so it
// can't get past its first log line until the test reads, and the signal
// goes once the Server holds its Standard Port, which it binds just before
// that line. A Server without its handlers by then dies of the signal
// every time.
#[cfg(target_os = "linux")]
#[test]
fn a_signal_as_the_server_says_it_listens_stops_it_cleanly() {
    use nix::sys::signal::Signal;
    for (sent, name) in [
        (Signal::SIGTERM, "SIGTERM"),
        (Signal::SIGINT, "SIGINT"),
        (Signal::SIGHUP, "SIGHUP"),
    ] {
        let (child, reader) = server_held_at_its_first_log_line();
        signal(&child, sent);
        // Only now is the log read: the filler, then the Server's lines.
        let log = std::thread::spawn(move || {
            let mut log = String::new();
            let _ = BufReader::new(reader).read_to_string(&mut log);
            log
        });
        let output = wait_bounded(child, Duration::from_secs(10));
        let log = log.join().expect("the log is read");
        let events: Vec<serde_json::Value> = log
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        let event = |event: &str| events.iter().find(|e| e["event"] == event);
        assert!(
            output.status.success(),
            "{name}: {:?}\n{log}{}",
            output.status,
            stderr(&output)
        );
        assert!(
            event("Listening").is_some()
                && event("Stopping").is_some_and(|e| e["reason"] == name)
                && event("Statistics").is_some(),
            "{name}: {log}"
        );
    }
}

//  (the Client on Ctrl-C: it stops the Test with its STOP2, reports
// what it measured with ErrorStatus 204, and dies of the signal, as a
// shell expects of an interrupted program) (conformance: client-interrupt)
#[cfg(unix)]
#[test]
fn an_interrupted_client_stops_the_test_and_reports_it() {
    use std::os::unix::process::ExitStatusExt as _;
    let server = Server::start(&["--log-format", "json"]);
    // SIGINT at its default action, whatever this test process started with.
    let client = udpstp_with_default_signals()
        .args(["client", "--unauthenticated", "--format", "json"])
        .args(["--test-interval", "10"])
        .arg(format!("127.0.0.1:{}", server.port))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the Client starts");
    let activated = server.output_until("TestConnectionActivated", Duration::from_secs(10));
    assert!(activated.contains("TestConnectionActivated"), "{activated}");
    // A Sub-Interval or so measured.
    std::thread::sleep(Duration::from_millis(1500));
    signal(&client, nix::sys::signal::Signal::SIGINT);
    let output = wait_bounded(client, Duration::from_secs(15));
    assert_eq!(
        output.status.signal(),
        Some(nix::sys::signal::Signal::SIGINT as i32),
        "{:?}: {}",
        output.status,
        stderr(&output)
    );
    let json: serde_json::Value = serde_json::from_str(&stdout(&output))
        .unwrap_or_else(|e| panic!("{e}: {}", stdout(&output)));
    assert!(
        json["ErrorStatus"] == 204
            && json["udpstp"]["EndReason"] == "Stopped"
            && json["udpstp"]["Warnings"]
                .as_array()
                .is_some_and(|w| w.iter().any(|w| w["Code"] == "Interrupted")
                    && !w.iter().any(|w| w["Code"] == "DataPhaseEnded")),
        "{json:#}"
    );
    // The measurements survive the signal in JSON too
    // (conformance: json-incomplete-test-results-in-extension).
    let intervals = json["udpstp"]["SubIntervals"]
        .as_array()
        .expect("measured Sub-Intervals in the extension");
    let sender = json["udpstp"]["IPLayerSenderBitRate"].as_array().unwrap();
    assert_eq!(intervals.len(), sender.len(), "{json:#}");
    for interval in intervals {
        for key in [
            "LossRatio",
            "LossCount",
            "PDVAvg",
            "RTTAvg",
            "IPLayerCapacity",
        ] {
            assert!(interval[key].is_number(), "{interval:#}");
        }
    }
    assert!(json["udpstp"].get("MinRTT").is_some(), "{json:#}");
    // The STOP2 reached the Server, which ended its Test Connection at once
    // rather than by its Watchdog.
    let log = server.output_until("TestSummary", Duration::from_secs(10));
    let ended = log
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|e| e["event"] == "DataPhaseEnded");
    assert!(
        ended.is_some_and(|e| e["reason"] == "StoppedByClient"),
        "{log}"
    );
}

// ADR 0014: the shared shutdown path sends STOP to active Tests and logs
// their ends, summaries and counters before the Server exits.
#[cfg(unix)]
#[test]
fn a_server_stopped_by_a_signal_ends_its_tests_and_reports_them() {
    for direction in ["downstream", "upstream"] {
        let mut server = Server::start(&["--log-format", "json", "--statistics-interval", "3600"]);
        let client = udpstp()
            .args(["client", "--unauthenticated", "--format", "json"])
            .args(["--test-interval", "60"])
            .args(["--direction", direction])
            .arg(format!("127.0.0.1:{}", server.port))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the Client starts");
        let activated = server.output_until("TestConnectionActivated", Duration::from_secs(10));
        assert!(activated.contains("TestConnectionActivated"), "{activated}");
        signal(&server.child, nix::sys::signal::Signal::SIGTERM);
        let log = server.output_until(r#""event":"Statistics""#, Duration::from_secs(10));
        let events: Vec<serde_json::Value> = log
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        let event = |name: &str| events.iter().find(|e| e["event"] == name);
        assert!(
            event("Stopping").is_some_and(|e| e["reason"] == "SIGTERM")
                && event("DataPhaseEnded").is_some_and(|e| e["reason"] == "Stopped")
                && event("TestConnectionReleased").is_some()
                && event("TestSummary").is_some_and(|e| e["result"] == "ServerStopping")
                && event("Statistics").is_some_and(|e| e["tests"] == 1),
            "{log}"
        );
        let until = std::time::Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = server.child.try_wait().expect("waitable") {
                break status;
            }
            assert!(std::time::Instant::now() < until, "the Server exits");
            std::thread::sleep(Duration::from_millis(50));
        };
        assert!(status.success(), "{status:?}");
        let output = wait_bounded(client, Duration::from_secs(10));
        assert!(output.status.success(), "{}", stderr(&output));
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).expect("Client JSON");
        assert_eq!(json["udpstp"]["EndReason"], "Completed", "{json:#}");
    }
}

//  (a closed standard output, as when piped into `head`, ends the
// Client cleanly with ErrorStatus 204 rather than a panic's 101)
#[test]
fn a_client_whose_stdout_closes_stops_cleanly() {
    let server = Server::start(&[]);
    let mut client = udpstp()
        .args(["client", "--unauthenticated", "--test-interval", "10"])
        .arg(format!("127.0.0.1:{}", server.port))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the Client starts");
    drop(client.stdout.take());
    let until = std::time::Instant::now() + Duration::from_secs(15);
    while client.try_wait().expect("waitable").is_none() {
        if std::time::Instant::now() > until {
            let _ = client.kill();
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let output = client.wait_with_output().expect("its output");
    assert_eq!(output.status.code(), Some(204), "{}", stderr(&output));
}

//  (a write to a closed standard output is noticed when it fails, and
// stops the running Test: exit 204)
#[test]
fn a_client_whose_stdout_closes_after_activation_stops_cleanly() {
    let server = Server::start(&[]);
    let mut client = udpstp()
        .args(["client", "--unauthenticated", "--test-interval", "5"])
        .arg(format!("127.0.0.1:{}", server.port))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the Client starts");
    let mut lines = BufReader::new(client.stdout.take().expect("piped")).lines();
    let activated = lines
        .by_ref()
        .map_while(Result::ok)
        .any(|line| line.contains("Test activated"));
    assert!(activated, "the Test was activated");
    drop(lines);
    let until = std::time::Instant::now() + Duration::from_secs(20);
    while client.try_wait().expect("waitable").is_none() {
        if std::time::Instant::now() > until {
            let _ = client.kill();
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let output = client.wait_with_output().expect("its output");
    assert_eq!(output.status.code(), Some(204), "{}", stderr(&output));
}

// : a Client started with SIGINT ignored (`nohup`, `cmd &`) keeps it
// ignored, and runs its Test to the end
#[cfg(unix)]
#[test]
fn a_client_started_with_sigint_ignored_keeps_running() {
    let server = Server::start(&[]);
    let client = Command::new("sh")
        .env_remove(KERNEL_TX_TIMESTAMPS)
        .arg("-c")
        .arg(format!(
            "trap '' INT; exec '{}' client --unauthenticated --test-interval 5 127.0.0.1:{}",
            env!("CARGO_BIN_EXE_udpstp"),
            server.port
        ))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the Client starts");
    let activated = server.output_until("TestConnectionActivated", Duration::from_secs(10));
    assert!(activated.contains("TestConnectionActivated"), "{activated}");
    signal(&client, nix::sys::signal::Signal::SIGINT);
    let output = wait_bounded(client, Duration::from_secs(20));
    assert!(
        output.status.success(),
        "{:?}: {}",
        output.status,
        stderr(&output)
    );
}

//  (a Server stopped while a Test Connection waits for its Activation
// Request closes it, reports it as cut short, and exits well before the
// Activation Watchdog)
#[cfg(unix)]
#[test]
fn a_server_stopped_mid_activation_releases_the_test_connection_at_once() {
    use udpstp_wire::SetupPdu;
    use udpstp_wire::setup::{cmd_request, modifier};
    let mut server = Server::start(&["--log-format", "json", "--statistics-interval", "3600"]);
    let request = SetupPdu {
        protocol_version: 20,
        mc_index: 0,
        mc_count: 1,
        mc_ident: 7,
        cmd_request: cmd_request::SETUP_REQUEST,
        cmd_response: 0,
        max_bandwidth: 0,
        test_port: 0,
        modifier_bitmap: modifier::JUMBO_STATUS,
        auth_mode: 0,
        auth_unix_time: 0,
        auth_digest: [0; 32],
        key_id: 0,
        checksum: 0,
    };
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket
        .send_to(&request.encode(), ("127.0.0.1", server.port))
        .unwrap();
    let admitted = server.output_until("TestConnectionAdmitted", Duration::from_secs(10));
    assert!(admitted.contains("TestConnectionAdmitted"), "{admitted}");
    let stopped_at = std::time::Instant::now();
    signal(&server.child, nix::sys::signal::Signal::SIGTERM);
    let log = server.output_until(r#""event":"Statistics""#, Duration::from_secs(5));
    let status = loop {
        if let Some(status) = server.child.try_wait().expect("waitable") {
            break status;
        }
        assert!(
            stopped_at.elapsed() < Duration::from_secs(3),
            "the Server exits within 3 s"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(status.success(), "{status:?}");
    let events: Vec<serde_json::Value> = log
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    let event = |name: &str| events.iter().find(|e| e["event"] == name);
    assert!(
        event("Stopping").is_some()
            && event("TestConnectionReleased").is_some()
            && event("ActivationWatchdogExpired").is_none()
            && event("TestSummary").is_some_and(|e| e["result"] == "ServerStopping"),
        "{log}"
    );
}
