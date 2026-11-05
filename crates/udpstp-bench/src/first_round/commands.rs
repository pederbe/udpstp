//! The first Round's command lines: udpstp's and OB-UDPST's Clients
//! and Servers, as `scripts/bench/matrix.py` builds them, each run from the
//! host's work directory, where the Round put the binaries and its key
//! files.
//!
//! Both engines send the same packets, so that their results compare: with
//! Jumbo, the default of both, rows above 1 Gbps use IP packets of up to
//! 9000 bytes, which OB-UDPST's Server always sends and udpstp's sends with
//! a Server MTU of 9000; without it, neither uses Jumbo at either end.
//!
//! A Client's command is run as one shell line, the words joined as
//! `shlex.join` joins them ([`bench_rounds::quote::join`]); a Server's is
//! joined with spaces inside the line that starts it in a session of its
//! own.

use super::matrix::{Direction, Engine, Rate, RoundRun};
use std::num::NonZeroUsize;

/// The Round's key files in the host's work directory: udpstp's Key Table
/// and OB-UDPST's keys, both holding the Round's Operator Key.
pub const UDPSTP_KEYS: &str = "keys.toml";
pub const OBUDPST_KEYS: &str = "udpst.keys";

/// The Client's command for `run`, to the Server at `server_address`
/// (`ADDRESS:PORT`), whose Operator Key has the ID `key_id`
/// (`client_command`).
///
/// Both engines read the Operator Key from the Round's key files, never
/// from the command line, which a failed command's error would carry into
/// `--out`: only the key's ID is on it.
pub fn client(run: &RoundRun, server_address: &str, key_id: u8) -> Vec<String> {
    let mut command: Vec<String> = Vec::new();
    let mut words = |words: &[&str]| command.extend(words.iter().map(|w| (*w).to_string()));
    let interval = run.test_interval.to_string();
    let connections = run.test_connections.to_string();
    match run.client {
        Engine::Udpstp => {
            words(&["env"]);
            // udpstp's kernel send timestamps' lag, for M2:
            // experimental, reported only on Linux, and extra work for its
            // Load Sender alone, so only where the matrix asks for it.
            if run.kernel_send_timestamps {
                words(&["UDPSTP_KERNEL_TX_TIMESTAMPS=1"]);
            } else {
                words(&["UDPSTP_KERNEL_TX_TIMESTAMPS=0"]);
            }
            words(&[
                "./udpstp",
                "client",
                "--key-file",
                UDPSTP_KEYS,
                "--format",
                "json",
                "--direction",
                run.direction.name(),
                "--test-interval",
                &interval,
                "--test-connections",
                &connections,
            ]);
            if let Rate::FixedRate(row) = run.rate {
                words(&["--fixed-rate", &row.to_string()]);
            }
            // OB-UDPST's Server doesn't run Authentication Mode 2, udpstp's
            // default.
            if run.server == Engine::Obudpst {
                words(&["--auth-mode", "1"]);
            }
            if !run.jumbo {
                words(&["--jumbo=false"]);
            }
        }
        Engine::Obudpst => {
            let direction = match run.direction {
                Direction::Downstream => "-d",
                Direction::Upstream => "-u",
            };
            words(&[
                "./udpst",
                direction,
                "-K",
                OBUDPST_KEYS,
                "-y",
                &key_id.to_string(),
                "-t",
                &interval,
                "-f",
                "json",
                "-C",
                &connections,
            ]);
            if let Rate::FixedRate(row) = run.rate {
                words(&["-I", &row.to_string()]);
            }
            if !run.jumbo {
                words(&["-j"]);
            }
        }
    }
    words(&[server_address]);
    command
}

/// The Server's command for `engine` on `port`, with or without Jumbo
/// (`server_command`): udpstp's sends the Jumbo rows at OB-UDPST's packet
/// size, not refitted to 1500 bytes, through a Server MTU of 9000.
pub fn server(engine: Engine, port: u16, jumbo: bool) -> Vec<String> {
    server_timestamps(engine, port, jumbo, false)
}

pub(super) fn server_timestamps(
    engine: Engine,
    port: u16,
    jumbo: bool,
    stamps: bool,
) -> Vec<String> {
    server_settings(engine, port, jumbo, stamps, None)
}

pub(super) fn server_settings(
    engine: Engine,
    port: u16,
    jumbo: bool,
    stamps: bool,
    threads: Option<NonZeroUsize>,
) -> Vec<String> {
    let port = port.to_string();
    let threads = threads.map(|n| format!("UDPSTP_ENGINE_THREADS={n}"));
    let words: Vec<&str> = match engine {
        Engine::Udpstp => {
            let mut words = vec![
                "env",
                if stamps {
                    "UDPSTP_KERNEL_TX_TIMESTAMPS=1"
                } else {
                    "UDPSTP_KERNEL_TX_TIMESTAMPS=0"
                },
                "./udpstp",
                "server",
                "--key-file",
                UDPSTP_KEYS,
                "--max-hops",
                "64",
                "--log-format",
                "json",
                "--port",
                &port,
            ];
            if let Some(threads) = &threads {
                words.insert(2, threads);
            }
            if jumbo {
                words.extend(["--server-mtu", "9000"]);
            } else {
                words.push("--jumbo=false");
            }
            words
        }
        Engine::Obudpst => {
            let mut words = vec!["./udpst", "-K", OBUDPST_KEYS, "-p", &port];
            if !jumbo {
                words.push("-j");
            }
            words
        }
    };
    words.into_iter().map(String::from).collect()
}

#[cfg(test)]
mod tests {
    use super::super::matrix::{ClientHost, parse};
    use super::*;

    fn run(direction: Direction, rate: Rate, client: Engine, server: Engine) -> RoundRun {
        RoundRun {
            index: 1,
            repeat: 1,
            name: "t".into(),
            direction,
            rate,
            test_connections: 1,
            client,
            server,
            client_host: ClientHost::Cloud,
            test_interval: 10,
            jumbo: true,
            kernel_send_timestamps: false,
            host_samples: false,
            server_engine_threads: None,
        }
    }

    #[test]
    fn the_clients_commands_are_the_pythons() {
        // tests/test_round.py's test_commands.
        let mut theirs = run(
            Direction::Upstream,
            Rate::FixedRate(500),
            Engine::Obudpst,
            Engine::Udpstp,
        );
        theirs.test_connections = 4;
        assert_eq!(
            client(&theirs, "10.0.0.2:24601", 7),
            [
                "./udpst",
                "-u",
                "-K",
                "udpst.keys",
                "-y",
                "7",
                "-t",
                "10",
                "-f",
                "json",
                "-C",
                "4",
                "-I",
                "500",
                "10.0.0.2:24601"
            ]
        );
        let ours = client(
            &run(
                Direction::Downstream,
                Rate::Search,
                Engine::Udpstp,
                Engine::Udpstp,
            ),
            "10.0.0.2:24601",
            7,
        );
        assert_eq!(
            ours,
            [
                "env",
                "UDPSTP_KERNEL_TX_TIMESTAMPS=0",
                "./udpstp",
                "client",
                "--key-file",
                "keys.toml",
                "--format",
                "json",
                "--direction",
                "downstream",
                "--test-interval",
                "10",
                "--test-connections",
                "1",
                "10.0.0.2:24601"
            ]
        );
        // The experimental send timestamps load udpstp's Load Sender alone,
        // so only the Tests that ask for them turn them on.
        let mut stamped = run(
            Direction::Upstream,
            Rate::FixedRate(500),
            Engine::Udpstp,
            Engine::Udpstp,
        );
        stamped.kernel_send_timestamps = true;
        assert_eq!(
            client(&stamped, "h:1", 7)[1],
            "UDPSTP_KERNEL_TX_TIMESTAMPS=1"
        );
        // udpstp's own mode 2 against itself; mode 1 against OB-UDPST's
        // Server, which doesn't run mode 2.
        let against_theirs = client(
            &run(
                Direction::Downstream,
                Rate::Search,
                Engine::Udpstp,
                Engine::Obudpst,
            ),
            "h:25000",
            7,
        );
        let at = against_theirs.iter().position(|w| w == "--auth-mode");
        assert_eq!(at.map(|i| against_theirs[i + 1].as_str()), Some("1"));
        assert!(!ours.contains(&"--auth-mode".to_string()));
    }

    #[test]
    fn both_engines_send_the_same_packets() {
        // tests/test_round.py's test_both_implementations_send_the_same_packets.
        assert_eq!(
            server(Engine::Udpstp, 24601, true),
            [
                "env",
                "UDPSTP_KERNEL_TX_TIMESTAMPS=0",
                "./udpstp",
                "server",
                "--key-file",
                "keys.toml",
                "--max-hops",
                "64",
                "--log-format",
                "json",
                "--port",
                "24601",
                "--server-mtu",
                "9000"
            ]
        );
        assert_eq!(
            server(Engine::Obudpst, 25000, true),
            ["./udpst", "-K", "udpst.keys", "-p", "25000"]
        );
        let ours = server(Engine::Udpstp, 24601, false);
        assert!(ours.contains(&"--jumbo=false".to_string()));
        assert!(!ours.contains(&"--server-mtu".to_string()));
        assert!(server(Engine::Obudpst, 25000, false).contains(&"-j".to_string()));
        for (jumbo, text) in [
            (false, "[defaults]\njumbo = false\n"),
            (true, "[defaults]\njumbo = true\n"),
        ] {
            let text = format!(
                "{text}[[test]]\nname = \"x\"\npairs = [\"udpstp/udpstp\", \"obudpst/obudpst\"]\n"
            );
            for r in parse(&text).unwrap() {
                let command = client(&r, "h:1", 7);
                let off = match r.client {
                    Engine::Udpstp => "--jumbo=false",
                    Engine::Obudpst => "-j",
                };
                assert_eq!(command.contains(&off.to_string()), !jumbo, "{command:?}");
            }
        }
    }

    #[test]
    fn no_command_carries_a_key() {
        // The design's Â§6: keys never on a command line. Only the key's ID
        // reaches the commands; the key itself stays in the key files.
        let matrices = [
            include_str!("../../../../scripts/bench/first-round.toml"),
            include_str!("../../../../scripts/bench/tests/smoke.toml"),
            include_str!("../../../../scripts/bench/tests/smoke-interop.toml"),
        ];
        for text in matrices {
            for r in parse(text).unwrap() {
                let command = client(&r, "10.0.0.2:24601", 7);
                let key_options = ["-a", "--key", "--auth-key"];
                assert!(
                    command.iter().all(|w| !key_options.contains(&w.as_str())),
                    "{command:?}"
                );
                assert!(
                    command.contains(&UDPSTP_KEYS.to_string())
                        || command.contains(&OBUDPST_KEYS.to_string())
                );
            }
        }
    }
}
