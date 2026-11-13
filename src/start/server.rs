//! The Server's start: its settings turned into what the runtime serves,
//! with the warnings to show first, or the error that stops it.

use std::io;
use std::time::Duration;

use crate::protocol::{DscpSet, KeyTable, QuotaLimits, ServerConfig, TableShape};
use crate::runtime::{DEFAULT_MAX_HOPS, EngineSettings, IpOptions, ServerOptions, SocketBuffers};

use super::{Host, StartError, StartupWarning};
use crate::config::ServerSettings;
use crate::{exit, privacy};

/// What a Server serves with, and the warnings to show before it does.
pub struct ServerStart {
    pub options: ServerOptions,
    /// How its log shows client addresses, the pseudonym key read here,
    /// once.
    pub addresses: privacy::Addresses,
    /// In the order they are shown.
    pub warnings: Vec<StartupWarning>,
}

impl ServerStart {
    /// The Server `settings` describe (validated by
    /// [`ServerSettings::load`]), with the Key Table loaded from their key
    /// file, on `host`, whose socket buffers came out as `socket_buffers`
    /// when probed at the settings' size, its Data Phase Engines running as
    /// `engine` says ([`engine_settings`](super::engine_settings)); or,
    /// when its pseudonym key file can't be read or isn't wanted, the error
    /// that stops it (50).
    pub fn new(
        settings: &ServerSettings,
        keys: KeyTable,
        socket_buffers: io::Result<SocketBuffers>,
        host: Host,
        engine: EngineSettings,
    ) -> Result<Self, StartError> {
        let addresses = privacy::Addresses::new(
            settings.address_privacy,
            settings.address_privacy_key_file.as_deref(),
        )
        .map_err(|message| {
            StartError::new(exit::CONFIGURATION_ERROR, format!("udpstp: {message}"))
        })?;
        let (allowed_dscp, operator_allowed_dscp) = settings.dscp_sets();
        // Where traffic can't be marked, only 0 is allowed, so the
        // Activation Response says what is sent.
        let dscp = |allowed: DscpSet| {
            if host.dscp {
                allowed
            } else {
                DscpSet::DEFAULT_ONLY
            }
        };
        let options = ServerOptions {
            listen: settings.listen.clone(),
            port: settings.port,
            ephemeral_ports: settings.ephemeral_port_range().expect("validated"),
            ip: IpOptions {
                dont_fragment: settings.dont_fragment,
                max_hops: settings.max_hops.unwrap_or(DEFAULT_MAX_HOPS),
            },
            socket_buffer_size: settings.socket_buffer_size,
            engine,
            core: ServerConfig {
                allow_unauthenticated: settings.allow_unauthenticated,
                keys,
                checksum: settings.checksum,
                delay_var: settings.delay_var_statistic.into(),
                tests_per_source: settings.tests_per_source,
                max_test_connections: settings.max_test_connections,
                test_connections_per_test: settings.test_connections_per_test,
                max_unactivated_test_connections: settings.max_unactivated_test_connections,
                admitted_rate_cap: settings.admitted_rate_cap,
                max_test_share: settings.max_test_share,
                require_max_bandwidth: settings.require_max_bandwidth,
                troubleshooting: settings.troubleshooting,
                unverified_setups_per_second: settings.unverified_setups_per_second,
                source_setup_burst: settings.source_setup_burst,
                source_setups_per_second: settings.source_setups_per_second,
                key_setup_burst: settings.key_setup_burst,
                key_setups_per_second: settings.key_setups_per_second,
                max_start_row: settings.max_start_row,
                quota_period: Duration::from_secs(settings.quota_period),
                source_quota: QuotaLimits {
                    tests: settings.source_tests_per_quota_period,
                    bytes: settings.source_bytes_per_quota_period,
                },
                table: TableShape {
                    jumbo: settings.jumbo,
                    traditional_mtu: settings.traditional_mtu,
                    mtu: settings.server_mtu,
                },
                allowed_dscp: dscp(allowed_dscp),
                operator_allowed_dscp: dscp(operator_allowed_dscp),
            },
        };
        Ok(Self {
            options,
            addresses,
            warnings: warnings(settings, socket_buffers, host),
        })
    }
}

/// The Server's startup warnings, in the order they are shown.
fn warnings(
    settings: &ServerSettings,
    socket_buffers: io::Result<SocketBuffers>,
    host: Host,
) -> Vec<StartupWarning> {
    let mut warnings = Vec::new();
    // A Test that states no maxBandwidth adds nothing to the sum, so the
    // cap alone admits every such Test (OB-UDPST requires one with a cap).
    if settings.admitted_rate_cap.is_some() && !settings.require_max_bandwidth {
        warnings.push(StartupWarning::AdmittedRateCapWithoutMaxBandwidth);
    }
    if settings.key_file.is_none() && !settings.allow_unauthenticated {
        warnings.push(StartupWarning::NoTestAdmissible);
    }
    if settings.troubleshooting {
        warnings.push(StartupWarning::TroubleshootingModeOn);
    }
    match socket_buffers {
        Ok(buffers) if buffers.short() => {
            warnings.push(StartupWarning::SocketBuffersShort(buffers));
        }
        Ok(_) => {}
        Err(error) => warnings.push(StartupWarning::SocketBuffersUnknown {
            error: error.to_string(),
        }),
    }
    if settings.max_hops.is_none() {
        warnings.push(StartupWarning::MaxHopsNotSet);
    }
    if settings.dont_fragment && !host.dont_fragment {
        warnings.push(StartupWarning::DontFragmentUnsupported);
    }
    if !host.dscp {
        warnings.push(StartupWarning::DscpCoercedToZero);
    }
    if settings.tests_per_source > 1 {
        warnings.push(StartupWarning::TestsPerSource {
            tests_per_source: settings.tests_per_source,
        });
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVERYTHING: Host = Host {
        dscp: true,
        dont_fragment: true,
    };

    fn granted(size: usize) -> io::Result<SocketBuffers> {
        Ok(SocketBuffers {
            requested: size,
            receive: size,
            send: size,
        })
    }

    fn start(settings: &ServerSettings, host: Host) -> ServerStart {
        ServerStart::new(
            settings,
            KeyTable::default(),
            granted(settings.socket_buffer_size),
            host,
            EngineSettings::default(),
        )
        .unwrap_or_else(|error| panic!("{error:?}"))
    }

    /// A case: its name, the settings, the host and its probed socket
    /// buffers, and the warnings they give.
    type Case = (
        &'static str,
        ServerSettings,
        Host,
        io::Result<SocketBuffers>,
        Vec<StartupWarning>,
    );

    /// Settings a careful Server administrator gives: no warning.
    fn careful() -> ServerSettings {
        ServerSettings {
            allow_unauthenticated: true,
            max_hops: Some(8),
            ..ServerSettings::default()
        }
    }

    // RFC 9946 §10 has the operator set MaxHops (an operational
    // Requirement) and §5 the DF bit; docs/spec.md (the Server warns when
    // MaxHops was not set explicitly);
    #[udpstp_req::req("RFC9097-8.3-2")]
    #[test]
    fn the_server_warns_when_max_hops_is_not_set() {
        let unset = start(
            &ServerSettings {
                max_hops: None,
                ..careful()
            },
            EVERYTHING,
        );
        assert_eq!(unset.warnings, [StartupWarning::MaxHopsNotSet]);
        assert_eq!(unset.options.ip.max_hops, DEFAULT_MAX_HOPS);
        assert!(
            unset.warnings[0]
                .message()
                .contains("max_hops (--max-hops)"),
            "names the setting"
        );
        let set = start(&careful(), EVERYTHING);
        assert!(set.warnings.is_empty(), "{:?}", set.warnings);
        assert_eq!(set.options.ip.max_hops, 8);
    }

    // : the Server's startup warnings, as data (they replaced greps
    // of a real Server's stderr); docs/spec.md story 78 (a warning names the
    // limit to raise), ;  (a Server that can admit no Test says
    // so)
    #[test]
    fn the_server_warns_of_what_limits_or_weakens_it() {
        let short = SocketBuffers {
            requested: 1 << 28,
            receive: 212_992,
            send: 212_992,
        };
        let cases: Vec<Case> = vec![
            ("careful", careful(), EVERYTHING, granted(1 << 21), vec![]),
            (
                "no key file, Unauthenticated Mode off",
                ServerSettings {
                    allow_unauthenticated: false,
                    ..careful()
                },
                EVERYTHING,
                granted(1 << 21),
                vec![StartupWarning::NoTestAdmissible],
            ),
            (
                "a key file",
                ServerSettings {
                    allow_unauthenticated: false,
                    key_file: Some("keys.toml".into()),
                    ..careful()
                },
                EVERYTHING,
                granted(1 << 21),
                vec![],
            ),
            (
                "a cap without a required maxBandwidth",
                ServerSettings {
                    admitted_rate_cap: Some(1000),
                    ..careful()
                },
                EVERYTHING,
                granted(1 << 21),
                vec![StartupWarning::AdmittedRateCapWithoutMaxBandwidth],
            ),
            (
                "a cap with a required maxBandwidth",
                ServerSettings {
                    admitted_rate_cap: Some(1000),
                    require_max_bandwidth: true,
                    ..careful()
                },
                EVERYTHING,
                granted(1 << 21),
                vec![],
            ),
            (
                "Troubleshooting Mode",
                ServerSettings {
                    troubleshooting: true,
                    ..careful()
                },
                EVERYTHING,
                granted(1 << 21),
                vec![StartupWarning::TroubleshootingModeOn],
            ),
            (
                "short socket buffers",
                careful(),
                EVERYTHING,
                Ok(short),
                vec![StartupWarning::SocketBuffersShort(short)],
            ),
            (
                "socket buffers that can't be probed",
                careful(),
                EVERYTHING,
                Err(io::Error::other("no socket")),
                vec![StartupWarning::SocketBuffersUnknown {
                    error: "no socket".into(),
                }],
            ),
            (
                "a host that can't set DF",
                careful(),
                Host {
                    dont_fragment: false,
                    ..EVERYTHING
                },
                granted(1 << 21),
                vec![StartupWarning::DontFragmentUnsupported],
            ),
            (
                "a host that can't set DF, DF off",
                ServerSettings {
                    dont_fragment: false,
                    ..careful()
                },
                Host {
                    dont_fragment: false,
                    ..EVERYTHING
                },
                granted(1 << 21),
                vec![],
            ),
            (
                "a host that can't mark a DSCP",
                careful(),
                Host {
                    dscp: false,
                    ..EVERYTHING
                },
                granted(1 << 21),
                vec![StartupWarning::DscpCoercedToZero],
            ),
            (
                "more than one Test per Source",
                ServerSettings {
                    tests_per_source: 2,
                    source_setup_burst: 48,
                    ..careful()
                },
                EVERYTHING,
                granted(1 << 21),
                vec![StartupWarning::TestsPerSource {
                    tests_per_source: 2,
                }],
            ),
            (
                "everything, in the order shown",
                ServerSettings {
                    admitted_rate_cap: Some(1000),
                    allow_unauthenticated: false,
                    troubleshooting: true,
                    max_hops: None,
                    tests_per_source: 2,
                    ..careful()
                },
                Host {
                    dscp: false,
                    dont_fragment: false,
                },
                Ok(short),
                vec![
                    StartupWarning::AdmittedRateCapWithoutMaxBandwidth,
                    StartupWarning::NoTestAdmissible,
                    StartupWarning::TroubleshootingModeOn,
                    StartupWarning::SocketBuffersShort(short),
                    StartupWarning::MaxHopsNotSet,
                    StartupWarning::DontFragmentUnsupported,
                    StartupWarning::DscpCoercedToZero,
                    StartupWarning::TestsPerSource {
                        tests_per_source: 2,
                    },
                ],
            ),
        ];
        for (what, settings, host, buffers, want) in cases {
            let start = ServerStart::new(
                &settings,
                KeyTable::default(),
                buffers,
                host,
                EngineSettings::default(),
            )
            .unwrap_or_else(|error| panic!("{what}: {error:?}"));
            assert_eq!(start.warnings, want, "{what}");
        }
        // The line a short buffer prints names the limit to raise.
        assert!(
            StartupWarning::SocketBuffersShort(short)
                .message()
                .contains(crate::runtime::SOCKET_BUFFER_LIMITS)
        );
    }

    // , : the Server reads its pseudonym key once, at the start,
    // and one it can't read, or has without pseudonyms, stops it with a
    // configuration error (50) before any warning; a runtime the OS
    // refuses stops it as one that couldn't start (75)
    #[test]
    fn a_server_whose_pseudonym_key_cant_be_read_doesnt_start() {
        use crate::privacy::AddressPrivacy::{Full, Pseudonymize};
        let cases: Vec<(&str, _, Option<&str>, &str)> = vec![
            (
                "pseudonyms without a key",
                Pseudonymize,
                None,
                "udpstp: --address-privacy pseudonymize needs --address-privacy-key-file FILE",
            ),
            (
                "a key file that isn't there",
                Pseudonymize,
                Some("no-such-pseudonym.key"),
                "udpstp: pseudonym key file no-such-pseudonym.key: ",
            ),
            (
                "a key file without pseudonyms",
                Full,
                Some("no-such-pseudonym.key"),
                "udpstp: --address-privacy-key-file requires --address-privacy pseudonymize",
            ),
        ];
        for (what, privacy, key_file, says) in cases {
            let settings = ServerSettings {
                address_privacy: privacy,
                address_privacy_key_file: key_file.map(Into::into),
                // Would warn, had the Server started.
                max_hops: None,
                ..careful()
            };
            let Err(error) = ServerStart::new(
                &settings,
                KeyTable::default(),
                granted(settings.socket_buffer_size),
                EVERYTHING,
                EngineSettings::default(),
            ) else {
                panic!("{what}: started");
            };
            assert_eq!(error.status, exit::CONFIGURATION_ERROR, "{what}");
            assert!(error.message.starts_with(says), "{what}: {}", error.message);
        }
        let refused = StartError::runtime(&io::Error::other("no threads"));
        assert_eq!(refused.status, exit::INITIALIZATION_ERROR);
        assert!(
            refused.message.ends_with("no threads"),
            "{}",
            refused.message
        );
    }

    // : where traffic can't be marked, the Server allows only DSCP 0,
    // so its Activation Responses say what is sent
    #[test]
    fn where_no_dscp_can_be_marked_only_zero_is_allowed() {
        let settings = ServerSettings {
            allowed_dscp: vec![0, 46],
            ..careful()
        };
        let marking = start(&settings, EVERYTHING);
        assert_eq!(
            marking.options.core.allowed_dscp,
            DscpSet::of([0, 46]).unwrap()
        );
        assert_eq!(
            marking.options.core.operator_allowed_dscp,
            DscpSet::ALL_BUT_NETWORK_CONTROL
        );
        let unmarked = start(
            &settings,
            Host {
                dscp: false,
                ..EVERYTHING
            },
        );
        assert_eq!(unmarked.options.core.allowed_dscp, DscpSet::DEFAULT_ONLY);
        assert_eq!(
            unmarked.options.core.operator_allowed_dscp,
            DscpSet::DEFAULT_ONLY
        );
    }

    // docs/spec.md story 66: every setting reaches what the runtime serves
    #[test]
    fn the_settings_become_what_the_runtime_serves() {
        let settings = ServerSettings {
            listen: vec!["192.0.2.1".parse().unwrap()],
            port: 24602,
            ephemeral_ports: "40000-40999".into(),
            dont_fragment: false,
            socket_buffer_size: 1 << 20,
            quota_period: 60,
            source_tests_per_quota_period: Some(3),
            source_bytes_per_quota_period: Some(1 << 30),
            server_mtu: 9000,
            traditional_mtu: true,
            ..careful()
        };
        let options = start(&settings, EVERYTHING).options;
        assert_eq!(options.listen, settings.listen);
        assert_eq!(options.port, 24602);
        assert_eq!(options.ephemeral_ports, 40000..=40999);
        assert_eq!(
            options.ip,
            IpOptions {
                dont_fragment: false,
                max_hops: 8
            }
        );
        assert_eq!(options.socket_buffer_size, 1 << 20);
        let core = &options.core;
        assert!(core.allow_unauthenticated);
        assert_eq!(core.quota_period, Duration::from_secs(60));
        assert_eq!(
            core.source_quota,
            QuotaLimits {
                tests: Some(3),
                bytes: Some(1 << 30)
            }
        );
        assert_eq!(
            core.table,
            TableShape {
                jumbo: true,
                traditional_mtu: true,
                mtu: 9000
            }
        );
    }
}
