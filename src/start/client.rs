//! The Client's start: its options turned into the Test the runtime runs,
//! with the warnings to show first, or the configuration error that stops
//! it.

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::num::NonZeroU8;
use std::time::Duration;

use crate::protocol::{
    ClientAuth, ClientConfig, DelayVariation, Direction, PerformanceCriterion, RateAdjAlgorithm,
    RowRequest, TestPlan,
};
use crate::runtime::IpOptions;

use super::{Host, StartError, StartupWarning};
use crate::test_results::ResultOptions;
use crate::{Algorithm, ClientArgs, DelayVar, Format, TestDirection, exit, keyfile, privacy};

/// What a Client runs: its Test, as the runtime takes it, and the
/// warnings to show before it starts.
pub struct ClientStart {
    /// Where the Test Connections go, how many must succeed, the time
    /// limit, the maxBandwidth and the Servers that get mode 1.
    pub plan: TestPlan,
    /// DF and MaxHops on test traffic.
    pub ip: IpOptions,
    /// Each Data Phase's receive and send buffer, in bytes.
    pub socket_buffer_size: usize,
    /// What shapes the Test's results. Among them, the DSCP the Test asks
    /// for: the option's, or 0 where this host can't mark traffic, since a
    /// Server would mark its side alone; and how the JSON results show
    /// this Client's own address, whose pseudonym key is read here,
    /// once, so a key file removed during the Test changes nothing.
    pub results: ResultOptions,
    /// In the order they are shown.
    pub warnings: Vec<StartupWarning>,
    /// The Client's settings for the Test, which every Test Connection
    /// shares; the runtime draws the Test's mcIdent.
    pub config: ClientConfig,
}

impl ClientStart {
    /// The Test `args` ask for on `host`, with each SERVER resolved by
    /// `resolve`, once the options are known to fit together; or the
    /// configuration error that stops it: the first one found, in this
    /// order (exit status in brackets):
    ///
    /// 1. the address privacy and its pseudonym key file (50);
    /// 2. `--mode-1-server` naming no SERVER (50);
    /// 3. the key file, and `--key-id` naming none of its keys (51);
    /// 4. no key and no `--unauthenticated` (50);
    /// 5. the Test Connection counts and the maxBandwidth shares (50);
    /// 6. a SERVER that doesn't resolve (75);
    /// 7. SERVERs of both IP versions (50);
    /// 8. an invalid `--allow-server-address` (50).
    pub fn new(
        args: &ClientArgs,
        host: Host,
        mut resolve: impl FnMut(&str) -> io::Result<SocketAddr>,
    ) -> Result<Self, StartError> {
        let configuration = |message: &str| {
            StartError::new(
                exit::CONFIGURATION_ERROR,
                format!("udpstp: configuration error: {message}"),
            )
        };
        let addresses = privacy::Addresses::new(
            args.address_privacy,
            args.address_privacy_key_file.as_deref(),
        )
        .map_err(|message| {
            StartError::new(exit::CONFIGURATION_ERROR, format!("udpstp: {message}"))
        })?;
        if let Some(name) = args
            .mode_1_server
            .iter()
            .find(|name| !args.servers.contains(name))
        {
            return Err(configuration(&format!(
                "--mode-1-server {name} isn't one of the SERVERs"
            )));
        }
        let mut warnings = Vec::new();
        let auth = match &args.key_file {
            Some(path) => {
                let loaded = keyfile::load(path).map_err(|error| {
                    StartError::new(
                        exit::KEY_FILE_ERROR,
                        format!("udpstp: key file error: {error}"),
                    )
                })?;
                if loaded.format_deprecated {
                    warnings.push(StartupWarning::KeyTableFormatDeprecated { path: path.clone() });
                }
                let keys = loaded.table;
                if let Some(id) = args.key_id
                    && !keys.entries().iter().any(|entry| entry.key_id == id)
                {
                    return Err(StartError::new(
                        exit::KEY_FILE_ERROR,
                        format!(
                            "udpstp: key file error: {}: no key has keyId {id}",
                            path.display()
                        ),
                    ));
                }
                Some(match args.auth_mode {
                    2 => ClientAuth::Mode2 {
                        keys,
                        key_id: args.key_id,
                    },
                    _ => ClientAuth::Mode1 {
                        keys,
                        key_id: args.key_id,
                    },
                })
            }
            None => args.unauthenticated.then_some(ClientAuth::Unauthenticated),
        };
        // Said before anything is sent; the core would refuse too.
        if auth.is_none() {
            return Err(configuration(
                "no key to authenticate with; give one with --key-file, or --unauthenticated for a Server that allows Unauthenticated Mode",
            ));
        }
        let (test_connections, minimum) =
            test_connection_counts(args).map_err(|message| configuration(&message))?;
        let mut servers = Vec::with_capacity(args.servers.len());
        for name in &args.servers {
            let server = resolve(name).map_err(|error| {
                StartError::new(
                    exit::INITIALIZATION_ERROR,
                    format!("udpstp: {name}: {error}"),
                )
            })?;
            servers.push(server);
        }
        let first = servers[0];
        // A Test uses one IP version; IPv4 and IPv6 together are left for
        // future work (RFC 9946 §4).
        if servers.iter().any(|s| s.is_ipv4() != first.is_ipv4()) {
            return Err(configuration(
                "the Servers' addresses mix IPv4 and IPv6; a Test uses one address family",
            ));
        }
        let mut alternate_server_addresses = Vec::new();
        for entry in &args.allow_server_address {
            let (name, address) = entry
                .split_once('=')
                .ok_or_else(|| configuration("--allow-server-address needs SERVER=IP"))?;
            let index = args
                .servers
                .iter()
                .position(|server| server == name)
                .ok_or_else(|| {
                    configuration(&format!(
                        "--allow-server-address {entry}: {name} isn't one of the SERVERs"
                    ))
                })?;
            let ip = address
                .parse::<IpAddr>()
                .map_err(|_| configuration("--allow-server-address needs a literal IP address"))?
                .to_canonical();
            if ip.is_unspecified() || ip.is_multicast() || ip == IpAddr::V4(Ipv4Addr::BROADCAST) {
                return Err(configuration(
                    "--allow-server-address needs a unicast address",
                ));
            }
            if ip.is_ipv4() != servers[index].is_ipv4() {
                return Err(configuration(
                    "--allow-server-address must use the SERVER's address family",
                ));
            }
            alternate_server_addresses.push((servers[index], ip));
        }
        // Ask only for what can be sent: a Server would mark its side alone.
        let dscp = if args.dscp != 0 && !host.dscp {
            warnings.push(StartupWarning::DscpAskedAsZero);
            0
        } else {
            args.dscp
        };
        if args.dont_fragment && !host.dont_fragment {
            warnings.push(StartupWarning::DontFragmentUnsupported);
        }
        let auth_mode_1 = args
            .mode_1_server
            .iter()
            .filter_map(|name| args.servers.iter().position(|s| s == name))
            .map(|index| servers[index])
            .collect();
        let direction = match args.direction {
            TestDirection::Downstream => Direction::Downstream,
            TestDirection::Upstream => Direction::Upstream,
        };
        let results = ResultOptions {
            json: args.format == Format::Json,
            json_extension: args.json_extension,
            addresses,
            hosts: args.servers.iter().cloned().zip(servers.clone()).collect(),
            direction,
            fixed_rate: args.fixed_rate.is_some(),
            test_connections: test_connections.get(),
            minimum: minimum.get(),
            criterion: PerformanceCriterion {
                max_loss_ppm: args.max_loss_ratio,
            },
            faster_sub_interval_margin: args.faster_sub_interval_margin,
            first_mode: args.first_mode_sub_intervals,
            dscp,
            max_hops: args.max_hops,
            dont_fragment: args.dont_fragment && host.dont_fragment,
            jumbo: args.jumbo,
            traditional_mtu: args.traditional_mtu,
            max_bandwidth: args.max_bandwidth.unwrap_or(0),
            time_limit: args.time_limit,
        };
        let config = ClientConfig {
            direction,
            auth,
            row_request: match (args.fixed_rate, args.start_row) {
                (Some(row), _) => RowRequest::FixedRate(row),
                (None, Some(row)) => RowRequest::StartAt(row),
                (None, None) => RowRequest::ServerDefault,
            },
            unacceptable_coercions: args.refuse_coercion.clone(),
            random_payload: args.random_payload,
            delay_variation: match args.delay_var {
                DelayVar::OneWay => DelayVariation::OneWay,
                DelayVar::Rtt => DelayVariation::RoundTrip,
            },
            checksum: args.checksum,
            jumbo: args.jumbo,
            traditional_mtu: args.traditional_mtu,
            test_int_time: args.test_interval,
            algorithm: match args.algorithm {
                Algorithm::B => RateAdjAlgorithm::B,
                Algorithm::C => RateAdjAlgorithm::C,
            },
            dscp,
        };
        Ok(Self {
            plan: TestPlan {
                servers,
                alternate_server_addresses,
                test_connections,
                minimum,
                time_limit: Duration::from_secs(args.time_limit),
                max_bandwidth: args.max_bandwidth.unwrap_or(0),
                auth_mode_1,
            },
            ip: IpOptions {
                dont_fragment: args.dont_fragment,
                max_hops: args.max_hops,
            },
            socket_buffer_size: args.socket_buffer_size,
            results,
            warnings,
            config,
        })
    }
}

/// The Test Connections and the Minimum Test Connections the options ask
/// for, or why they can't be.
fn test_connection_counts(args: &ClientArgs) -> Result<(NonZeroU8, NonZeroU8), String> {
    let servers = args.servers.len();
    let default = u8::try_from(servers).map_err(|_| {
        format!("at most 255 Servers are allowed (got {servers}). Give fewer SERVERs")
    })?;
    let test_connections = args.test_connections.unwrap_or(default);
    if usize::from(test_connections) < servers {
        return Err(format!(
            "--test-connections is {test_connections}. Set it to at least {servers} for {servers} Servers, or give fewer SERVERs"
        ));
    }
    let minimum = args.min_test_connections.unwrap_or(test_connections);
    if minimum > test_connections {
        return Err(format!(
            "--min-test-connections is {minimum}, but --test-connections is {test_connections}. Lower --min-test-connections to {test_connections} or less, or increase --test-connections"
        ));
    }
    // The Test's maxBandwidth is split equally between its Test
    // Connections, each stating its share rounded down: below 1 Mbps each
    // there is no share to state, since 0 states none and 1 each would
    // state more than the Client user asked for.
    // (conformance: max-bandwidth-split-across-test-connections)
    if let Some(total) = args.max_bandwidth
        && total < u16::from(test_connections)
    {
        return Err(format!(
            "--max-bandwidth is {total} Mbps for {test_connections} Test Connections. Use at least {test_connections} Mbps (1 Mbps each), or reduce --test-connections"
        ));
    }
    // clap takes both from 1, and SERVER is required.
    Ok((
        NonZeroU8::new(test_connections).expect("at least one SERVER"),
        NonZeroU8::new(minimum).expect("at least 1"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;

    const EVERYTHING: Host = Host {
        dscp: true,
        dont_fragment: true,
    };

    /// The Client's options, as `udpstp client` takes them.
    fn args(line: &[&str]) -> ClientArgs {
        let cli = crate::Cli::try_parse_from(["udpstp", "client"].iter().chain(line))
            .unwrap_or_else(|e| panic!("{line:?}: {e}"));
        match cli.command {
            crate::Command::Client(args) => *args,
            crate::Command::Server(_) => unreachable!("a client command line"),
        }
    }

    /// Resolves only addresses, the Standard Port when none is given, as a
    /// resolver without a network would.
    fn literal(name: &str) -> io::Result<SocketAddr> {
        name.parse()
            .or_else(|_| {
                name.trim_matches(['[', ']'])
                    .parse()
                    .map(|ip| SocketAddr::new(ip, 24601))
            })
            .map_err(|_| io::Error::new(io::ErrorKind::NotFound, format!("{name} has no address")))
    }

    /// What a case says: its name, its command line, and what should come
    /// of it.
    type Case<'a, T> = (&'a str, Vec<&'a str>, T);

    fn start(line: &[&str], host: Host) -> Result<ClientStart, StartError> {
        ClientStart::new(&args(line), host, literal)
    }

    /// A Key Table only its owner can read, in a fresh directory.
    struct KeyFile(std::path::PathBuf);

    impl KeyFile {
        fn new(test: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("udpstp-client-start-{test}-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("keys.toml");
            std::fs::write(
                &path,
                "[[key]]\nadmin_key_name = \"lab\"\nkey_id = 4\nkey_hex = \"736861726564\"\n\
                 send_lifetime_start = \"20200101000000Z\"\naccept_lifetime_start = \"20200101000000Z\"\n",
            )
            .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
            Self(path)
        }

        fn path(&self) -> &str {
            self.0.to_str().unwrap()
        }
    }

    impl Drop for KeyFile {
        fn drop(&mut self) {
            if let Some(dir) = self.0.parent() {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
    }

    // Conformance Record: setup-response-from-another-address. Alternate
    // authorization is local and belongs only to the selected SERVER.
    #[test]
    fn alternate_server_addresses_are_authorized_before_the_test_starts() {
        let key = KeyFile::new("alternate-addresses");
        let start = start(
            &[
                "--key-file",
                key.path(),
                "--allow-server-address",
                "192.0.2.1=192.0.2.77",
                "--allow-server-address",
                "192.0.2.2:24602=::ffff:192.0.2.78",
                "192.0.2.1",
                "192.0.2.2:24602",
            ],
            EVERYTHING,
        )
        .unwrap();
        assert_eq!(
            start.plan.alternate_server_addresses,
            vec![
                (
                    "192.0.2.1:24601".parse().unwrap(),
                    "192.0.2.77".parse().unwrap()
                ),
                (
                    "192.0.2.2:24602".parse().unwrap(),
                    "192.0.2.78".parse().unwrap()
                ),
            ]
        );
    }

    // Conformance Record: setup-response-from-another-address. Invalid
    // local authorization is refused whole, with configuration status 50.
    #[test]
    fn invalid_alternate_authorization_is_refused() {
        let key = KeyFile::new("invalid-alternates");
        for entry in [
            "192.0.2.1",
            "other=192.0.2.77",
            "192.0.2.1=other",
            "192.0.2.1=192.0.2.77:24601",
            "192.0.2.1=0.0.0.0",
            "192.0.2.1=255.255.255.255",
            "192.0.2.1=224.0.0.1",
            "192.0.2.1=::ffff:224.0.0.1",
            "192.0.2.1=2001:db8::77",
        ] {
            let error = start(
                &[
                    "--key-file",
                    key.path(),
                    "--allow-server-address",
                    entry,
                    "192.0.2.1",
                ],
                EVERYTHING,
            )
            .err()
            .expect("invalid authorization");
            assert_eq!(
                error.status,
                exit::CONFIGURATION_ERROR,
                "{entry}: {error:?}"
            );
        }
        let start = start(
            &[
                "--key-file",
                key.path(),
                "--allow-server-address",
                "[2001:db8::1]:24602=2001:db8::77",
                "[2001:db8::1]:24602",
            ],
            EVERYTHING,
        )
        .unwrap();
        assert_eq!(
            start.plan.alternate_server_addresses,
            vec![(
                "[2001:db8::1]:24602".parse().unwrap(),
                "2001:db8::77".parse().unwrap()
            )]
        );
    }

    // : the Client's configuration errors, as data, each with its exit
    // status, the first found in the documented order; docs/spec.md (ErrorStatus
    // 50 for configuration errors, 51 for key file errors, 75 for a Client
    // that can't start);  (Test Connection counts),  (mode 1 for one
    // of the SERVERs),  (a pseudonym needs a key),  (no key)
    #[test]
    fn options_that_cant_run_are_refused_with_their_exit_status() {
        let key = KeyFile::new("refused");
        let many: Vec<String> = (0..256)
            .map(|i| format!("192.0.2.{}:{}", i % 250, 20000 + i))
            .collect();
        let mut too_many: Vec<&str> = vec!["--unauthenticated"];
        too_many.extend(many.iter().map(String::as_str));
        let cases: Vec<(&str, Vec<&str>, u8, &str)> = vec![
            (
                "a pseudonym without a key",
                vec![
                    "--unauthenticated",
                    "--address-privacy",
                    "pseudonymize",
                    "192.0.2.1",
                ],
                50,
                "--address-privacy pseudonymize needs --address-privacy-key-file FILE",
            ),
            (
                "a pseudonym key file that isn't there",
                vec![
                    "--unauthenticated",
                    "--address-privacy",
                    "pseudonymize",
                    "--address-privacy-key-file",
                    "no-such-pseudonym.key",
                    "192.0.2.1",
                ],
                50,
                "pseudonym key file no-such-pseudonym.key",
            ),
            (
                "mode 1 for no SERVER, before the key file is read",
                vec![
                    "--key-file",
                    "no-such.keys",
                    "--mode-1-server",
                    "192.0.2.9",
                    "192.0.2.1",
                ],
                50,
                "--mode-1-server 192.0.2.9 isn't one of the SERVERs",
            ),
            (
                "a key file that isn't there",
                vec!["--key-file", "no-such.keys", "192.0.2.1"],
                51,
                "udpstp: key file error: no-such.keys",
            ),
            (
                "a keyId the key file lacks",
                vec!["--key-file", key.path(), "--key-id", "2", "192.0.2.1"],
                51,
                "no key has keyId 2",
            ),
            (
                "no key",
                vec!["192.0.2.1"],
                50,
                "no key to authenticate with",
            ),
            (
                "more than 255 SERVERs",
                too_many,
                50,
                "at most 255 Servers are allowed (got 256). Give fewer SERVERs",
            ),
            (
                "fewer Test Connections than SERVERs",
                vec![
                    "--unauthenticated",
                    "--test-connections",
                    "1",
                    "192.0.2.1",
                    "192.0.2.2",
                ],
                50,
                "--test-connections is 1. Set it to at least 2 for 2 Servers",
            ),
            (
                "a minimum above the Test Connections",
                vec![
                    "--unauthenticated",
                    "--test-connections",
                    "2",
                    "--min-test-connections",
                    "3",
                    "192.0.2.1",
                ],
                50,
                "--min-test-connections is 3, but --test-connections is 2. Lower --min-test-connections to 2 or less",
            ),
            (
                "a minimum above the default Test Connections",
                vec![
                    "--unauthenticated",
                    "--min-test-connections",
                    "2",
                    "192.0.2.1",
                ],
                50,
                "--min-test-connections is 2, but --test-connections is 1. Lower --min-test-connections to 1 or less",
            ),
            (
                "less than 1 Mbps each",
                vec![
                    "--unauthenticated",
                    "--test-connections",
                    "3",
                    "--max-bandwidth",
                    "2",
                    "192.0.2.1",
                ],
                50,
                "--max-bandwidth is 2 Mbps for 3 Test Connections. Use at least 3 Mbps (1 Mbps each)",
            ),
            (
                "a SERVER that doesn't resolve",
                vec!["--unauthenticated", "server.invalid"],
                75,
                "udpstp: server.invalid: server.invalid has no address",
            ),
            (
                "IPv4 and IPv6 together",
                vec!["--unauthenticated", "192.0.2.1", "[2001:db8::1]:24601"],
                50,
                "mix IPv4 and IPv6",
            ),
        ];
        for (what, line, status, says) in cases {
            match start(&line, EVERYTHING) {
                Ok(_) => panic!("{what}: started"),
                Err(error) => {
                    assert_eq!(error.status, status, "{what}: {}", error.message);
                    assert!(
                        error.message.starts_with("udpstp: ") && error.message.contains(says),
                        "{what}: {}",
                        error.message
                    );
                }
            }
        }
    }

    // : the Client's startup warnings, as data;  (where traffic
    // can't be marked, the Client asks for DSCP 0); RFC 9946 §5 (DF)
    #[test]
    fn the_client_warns_of_what_this_host_cant_do() {
        let no_dscp = Host {
            dscp: false,
            ..EVERYTHING
        };
        let no_df = Host {
            dont_fragment: false,
            ..EVERYTHING
        };
        let cases: Vec<Case<(Host, u8, Vec<StartupWarning>)>> = vec![
            ("DSCP 46", vec!["--dscp", "46"], (EVERYTHING, 46, vec![])),
            (
                "DSCP 46, unmarkable",
                vec!["--dscp", "46"],
                (no_dscp, 0, vec![StartupWarning::DscpAskedAsZero]),
            ),
            ("DSCP 0, unmarkable", vec![], (no_dscp, 0, vec![])),
            (
                "DF, unsupported",
                vec![],
                (no_df, 0, vec![StartupWarning::DontFragmentUnsupported]),
            ),
            (
                "no DF asked for",
                vec!["--dont-fragment=false"],
                (no_df, 0, vec![]),
            ),
            (
                "neither, in the order shown",
                vec!["--dscp", "10"],
                (
                    Host {
                        dscp: false,
                        dont_fragment: false,
                    },
                    0,
                    vec![
                        StartupWarning::DscpAskedAsZero,
                        StartupWarning::DontFragmentUnsupported,
                    ],
                ),
            ),
        ];
        for (what, mut line, (host, dscp, warnings)) in cases {
            line.extend(["--unauthenticated", "192.0.2.1"]);
            let start = start(&line, host).unwrap_or_else(|e| panic!("{what}: {e:?}"));
            assert_eq!(
                (start.results.dscp, &start.warnings),
                (dscp, &warnings),
                "{what}"
            );
            assert_eq!(start.config.dscp, dscp, "{what}: the DSCP asked for");
        }
    }

    // docs/spec.md stories 8, 10, 11 and 54-58 (the Client's Test options reach
    // the Test it runs);  (mode 1 for the SERVERs named, by address)
    #[test]
    fn the_options_become_the_test_the_runtime_runs() {
        let key = KeyFile::new("options");
        let start = start(
            &[
                "--key-file",
                key.path(),
                "--mode-1-server",
                "192.0.2.2:24602",
                "--test-connections",
                "3",
                "--min-test-connections",
                "2",
                "--max-bandwidth",
                "300",
                "--time-limit",
                "2",
                "--direction",
                "upstream",
                "--start-row",
                "20",
                "--algorithm",
                "c",
                "--delay-var",
                "rtt",
                "--random-payload",
                "--refuse-coercion",
                "testIntTime",
                "--checksum",
                "--jumbo=false",
                "--traditional-mtu",
                "--test-interval",
                "20",
                "--max-hops",
                "12",
                "--socket-buffer-size",
                "1048576",
                "192.0.2.1",
                "192.0.2.2:24602",
            ],
            EVERYTHING,
        )
        .unwrap();
        let first: SocketAddr = "192.0.2.1:24601".parse().unwrap();
        let second: SocketAddr = "192.0.2.2:24602".parse().unwrap();
        assert_eq!(
            start.plan,
            TestPlan {
                servers: vec![first, second],
                alternate_server_addresses: Vec::new(),
                test_connections: NonZeroU8::new(3).unwrap(),
                minimum: NonZeroU8::new(2).unwrap(),
                time_limit: Duration::from_secs(2),
                max_bandwidth: 300,
                auth_mode_1: vec![second],
            }
        );
        assert_eq!(
            start.ip,
            IpOptions {
                dont_fragment: true,
                max_hops: 12
            }
        );
        assert_eq!(start.socket_buffer_size, 1 << 20);
        // What shapes the results: the SERVERs as typed, with their
        // addresses.
        let results = &start.results;
        assert_eq!(
            results.hosts,
            [
                ("192.0.2.1".to_owned(), first),
                ("192.0.2.2:24602".to_owned(), second)
            ]
        );
        assert_eq!(
            (
                results.direction,
                results.fixed_rate,
                results.test_connections,
                results.minimum,
                results.max_bandwidth,
                results.time_limit,
                results.max_hops,
                results.dont_fragment,
                results.json,
            ),
            (Direction::Upstream, false, 3, 2, 300, 2, 12, true, false)
        );
        let config = &start.config;
        assert!(matches!(
            config.auth,
            Some(ClientAuth::Mode2 { key_id: None, .. })
        ));
        assert_eq!(
            (
                config.direction,
                config.row_request,
                config.algorithm,
                config.delay_variation,
                config.test_int_time,
            ),
            (
                Direction::Upstream,
                RowRequest::StartAt(20),
                RateAdjAlgorithm::C,
                DelayVariation::RoundTrip,
                20,
            )
        );
        assert!(
            config.random_payload && config.checksum && config.traditional_mtu && !config.jumbo
        );
        assert_eq!(
            config.unacceptable_coercions,
            [crate::protocol::ActivationParameter::TestIntTime]
        );
        // A Fixed-Rate Test holds its row; --auth-mode 1 uses mode 1.
        let fixed = self::start(
            &[
                "--key-file",
                key.path(),
                "--auth-mode",
                "1",
                "--fixed-rate",
                "30",
                "192.0.2.1",
            ],
            EVERYTHING,
        )
        .unwrap()
        .config;
        assert_eq!(fixed.row_request, RowRequest::FixedRate(30));
        assert!(matches!(fixed.auth, Some(ClientAuth::Mode1 { .. })));
    }
}
