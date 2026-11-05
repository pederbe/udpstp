//! The Server's configuration: built-in defaults, then the TOML file, then
//! command-line overrides, each replacing the one before (docs/spec.md story 66).

use std::fmt;
use std::net::IpAddr;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};

use clap::{Args, ValueEnum};
use serde::{Deserialize, Serialize};
use udpstp_core::{
    DEFAULT_MAX_TEST_CONNECTIONS, DEFAULT_SOURCE_SETUP_BURST, DEFAULT_SOURCE_SETUPS_PER_SECOND,
    DEFAULT_TEST_CONNECTIONS_PER_TEST, DEFAULT_UNVERIFIED_SETUPS_PER_SECOND, DelayVarStatistic,
    DscpSet,
};
use udpstp_runtime::DEFAULT_SOCKET_BUFFER_SIZE;

use crate::log::LogFormat;

/// How often the log reports the Server's counters by default, in seconds.
const DEFAULT_STATISTICS_INTERVAL: u32 = 60;

/// The longest statistics interval: a day.
const MAX_STATISTICS_INTERVAL: u32 = 86_400;

/// The socket buffer sizes accepted, in bytes: at most 256 MiB, since
/// every Test Connection may fill its own.
pub const SOCKET_BUFFER_SIZES: RangeInclusive<usize> = 65_536..=1 << 28;

/// Why a socket buffer size is refused.
pub const SOCKET_BUFFER_SIZE_RANGE: &str = "must be from 65536 to 268435456 bytes";

/// The UDPSTP Standard Port (RFC 9946 §12.1).
const STANDARD_PORT: u16 = 24601;

/// Test Connections' ports by default: the dynamic ports (RFC 6335 §6).
const DEFAULT_EPHEMERAL_PORTS: &str = "49152-65535";

/// The Server's settings, as the TOML file writes them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerSettings {
    /// The addresses the Server listens on for Setup Requests; empty, the
    /// IPv4 and IPv6 wildcards (IPv6 if the host has it). A listener on a
    /// specific address never sees multicast or broadcast Setup Requests.
    pub listen: Vec<IpAddr>,
    /// The UDP port the Server listens on for Setup Requests.
    pub port: u16,
    /// Serve unauthenticated Tests too (not conformant to RFC 9946).
    pub allow_unauthenticated: bool,
    /// Fill the header checksum and verify it when populated (RFC 9946
    /// §5.6).
    pub checksum: bool,
    /// CHSR_JUMBO_STATUS: rows above 1 Gbps may use packets up to 9000
    /// bytes (RFC 9946 §6.1). Clients must ask for the same.
    pub jumbo: bool,
    /// CHSR_TRADITIONAL_MTU: packets may be 1500 bytes, not 1250, up to 1
    /// Gbps too. Clients must ask for the same.
    pub traditional_mtu: bool,
    /// The Server MTU: the largest IP packet the Server sends, in bytes,
    /// 1280 or more (9000 at most is used). Jumbo rows keep their rates
    /// with more, smaller packets below 9000.
    pub server_mtu: u32,
    /// The Ephemeral Ports Test Connections' sockets are bound to, as
    /// `FIRST-LAST`, picked at random: the Server's own range, not the OS's
    /// ephemeral port range. It holds at least `max_test_connections`
    /// plus the Unactivated Test Connection share.
    pub ephemeral_ports: String,
    /// Which statistic of a Trial Interval's one-way delay variation
    /// classifies it for the Load Rate Adjustment Algorithms.
    pub delay_var_statistic: DelayVar,
    /// How many Tests may run at once from one Source (an IPv4 address or
    /// an IPv6 /64). RFC 9946 §4.2 says 1; more, for Clients behind
    /// carrier-grade NAT, departs from it.
    pub tests_per_source: usize,
    /// How many Test Connections activated Tests may reserve at once.
    pub max_test_connections: usize,
    /// How many Test Connections one Test may have over its life.
    pub test_connections_per_test: usize,
    /// How many Test Connections may be admitted but not yet activated at
    /// once, separately from the global cap; absent, half of
    /// max_test_connections, but at least
    /// test_connections_per_test.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_unactivated_test_connections: Option<usize>,
    /// The most maxBandwidth admitted at once in each direction, in Mbps;
    /// no cap when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub admitted_rate_cap: Option<u32>,
    /// The most of `admitted_rate_cap`, in percent, that one Test without
    /// an Operator Key may state, all its Test Connections together.
    pub max_test_share: u8,
    /// Refuse Tests that state no maxBandwidth.
    pub require_max_bandwidth: bool,
    /// Answer authentication failures of Setup Requests with the reason
    /// instead of silence, to debug Clients. Off by default.
    pub troubleshooting: bool,
    /// How many Setup Requests a second may reach authentication; the rest
    /// are dropped silently.
    pub unverified_setups_per_second: u32,
    /// How many authentic Setup Requests one Source may send at once; at
    /// least test_connections_per_test.
    pub source_setup_burst: u32,
    /// How many more a Source may send each second after its burst.
    pub source_setups_per_second: u32,
    /// How many authentic Setup Requests one Key Table row may send at
    /// once, Unauthenticated Mode counting as one; at least
    /// test_connections_per_test.
    pub key_setup_burst: u32,
    /// How many more a Key Table row may send each second after its burst.
    pub key_setups_per_second: u32,
    /// The DSCPs, 0 to 63, a Test without an Operator Key may mark its
    /// traffic with; another is coerced to 0. Only 0 by default, so a
    /// Consumer can't put the load into a priority class.
    pub allowed_dscp: Vec<u8>,
    /// The DSCPs a Test with an Operator Key may mark its traffic with;
    /// absent, all but network control (48 and 56).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operator_allowed_dscp: Option<Vec<u8>>,
    /// The highest Sending Rate Table row a Test without an Operator Key
    /// may start its search at; a higher one is coerced to it. Operator
    /// Keys are exempt.
    pub max_start_row: u16,
    /// The Quota Period, in seconds: what Quotas are counted over.
    pub quota_period: u64,
    /// Each Source's Quota of Tests per Quota Period; absent, none.
    /// Operator Keys are exempt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_tests_per_quota_period: Option<u32>,
    /// Each Source's Quota of load bytes per Quota Period, both
    /// directions; absent, none. Operator Keys are exempt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_bytes_per_quota_period: Option<u64>,
    /// Each Test Connection's receive and send buffer in its Data Phase,
    /// in bytes, within the OS's limits.
    pub socket_buffer_size: usize,
    /// Set the Don't Fragment bit on test traffic (RFC 9946 §5: it SHOULD
    /// be set unless fragmentation is being measured).
    pub dont_fragment: bool,
    /// MaxHops: the IPv4 TTL and IPv6 hop limit of test traffic, 1 to
    /// 255, set from the expected path length (RFC 9946 §10). Absent, 64,
    /// with a warning.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_hops: Option<u8>,
    /// The log's format on stdout: `human` or `json`, one object a line.
    pub log_format: LogFormat,
    /// Where the log goes instead of stdout, with the startup's errors and
    /// warnings: a new file each day, named from this path with the date
    /// before its extension (`server.log` gives `server.2026-10-01.log`),
    /// the last 14 kept. A Windows service has no stdout, so it needs one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_file: Option<PathBuf>,
    /// How the log shows client addresses: `full`, `pseudonymize` or
    /// `truncate` (to the IPv4 /24 or IPv6 /48).
    pub address_privacy: crate::privacy::AddressPrivacy,
    /// The pseudonym key, as hexadecimal digits (16 bytes or more), for
    /// `address_privacy = "pseudonymize"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address_privacy_key_file: Option<std::path::PathBuf>,
    /// How often the log reports the Server's counters, in seconds, up to
    /// a day; 0 never does.
    pub statistics_interval: u32,
    /// The key file: a TOML Key Table if its name ends in `.toml`, else an
    /// OB-UDPST `udpst.keys` file. A relative path is taken from the
    /// current directory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_file: Option<PathBuf>,
}

/// The `delay_var_statistic` setting (docs/spec.md "Load Rate Adjustment Algorithms").
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum DelayVar {
    /// The Trial Interval's average, as OB-UDPST uses.
    Average,
    /// The Trial Interval's maximum: the delay range, which the RFCs judge.
    #[default]
    Maximum,
}

impl From<DelayVar> for DelayVarStatistic {
    fn from(value: DelayVar) -> Self {
        match value {
            DelayVar::Average => Self::Average,
            DelayVar::Maximum => Self::Maximum,
        }
    }
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self {
            listen: Vec::new(),
            port: STANDARD_PORT,
            allow_unauthenticated: false,
            jumbo: true,
            traditional_mtu: false,
            server_mtu: 1500,
            checksum: false,
            ephemeral_ports: DEFAULT_EPHEMERAL_PORTS.into(),
            delay_var_statistic: DelayVar::Maximum,
            tests_per_source: 1,
            max_test_connections: DEFAULT_MAX_TEST_CONNECTIONS,
            test_connections_per_test: DEFAULT_TEST_CONNECTIONS_PER_TEST,
            max_unactivated_test_connections: None,
            admitted_rate_cap: None,
            max_test_share: udpstp_core::DEFAULT_MAX_TEST_SHARE,
            require_max_bandwidth: false,
            troubleshooting: false,
            unverified_setups_per_second: DEFAULT_UNVERIFIED_SETUPS_PER_SECOND,
            source_setup_burst: DEFAULT_SOURCE_SETUP_BURST,
            source_setups_per_second: DEFAULT_SOURCE_SETUPS_PER_SECOND,
            key_setup_burst: udpstp_core::DEFAULT_KEY_SETUP_BURST,
            key_setups_per_second: udpstp_core::DEFAULT_KEY_SETUPS_PER_SECOND,
            allowed_dscp: vec![0],
            max_start_row: udpstp_core::DEFAULT_MAX_START_ROW,
            quota_period: udpstp_core::DEFAULT_QUOTA_PERIOD.as_secs(),
            source_tests_per_quota_period: None,
            source_bytes_per_quota_period: None,
            socket_buffer_size: DEFAULT_SOCKET_BUFFER_SIZE,
            operator_allowed_dscp: None,
            dont_fragment: true,
            max_hops: None,
            log_format: LogFormat::Human,
            log_file: None,
            address_privacy: crate::privacy::AddressPrivacy::Full,
            address_privacy_key_file: None,
            statistics_interval: DEFAULT_STATISTICS_INTERVAL,
            key_file: None,
        }
    }
}

/// Command-line options that override the file's settings.
#[derive(Debug, Default, Args)]
pub struct ServerOverrides {
    /// An address to listen on for Setup Requests, instead of the IPv4 and
    /// IPv6 wildcards. Repeatable.
    #[arg(long, value_name = "ADDRESS")]
    listen: Vec<IpAddr>,
    /// The UDP port to listen on for Setup Requests (the Standard Port
    /// 24601 by default).
    #[arg(long, value_name = "PORT", value_parser = clap::value_parser!(u16).range(1..))]
    port: Option<u16>,
    /// Serve unauthenticated Tests too. Not conformant to RFC 9946; Clients
    /// must opt in as well.
    #[arg(long, value_name = "BOOL", num_args = 0..=1, require_equals = true, default_missing_value = "true")]
    allow_unauthenticated: Option<bool>,
    /// Use the header checksum (RFC 9946 §5.6).
    #[arg(long, value_name = "BOOL", num_args = 0..=1, require_equals = true, default_missing_value = "true")]
    checksum: Option<bool>,
    /// Serve Jumbo rows above 1 Gbps (CHSR_JUMBO_STATUS) [default: true].
    #[arg(long, value_name = "BOOL", num_args = 0..=1, require_equals = true, default_missing_value = "true")]
    jumbo: Option<bool>,
    /// Serve 1500-byte packets (CHSR_TRADITIONAL_MTU) [default: false].
    #[arg(long, value_name = "BOOL", num_args = 0..=1, require_equals = true, default_missing_value = "true")]
    traditional_mtu: Option<bool>,
    /// The Server MTU: the largest IP packet the Server sends, in bytes
    /// [default: 1500].
    #[arg(long, value_name = "BYTES", value_parser = clap::value_parser!(u32).range(1280..))]
    server_mtu: Option<u32>,
    /// Renamed --server-mtu; given, it is refused with that hint.
    #[arg(long = "mtu", value_name = "BYTES", hide = true)]
    renamed_mtu: Option<u32>,
    /// The Server's Ephemeral Ports for Test Connections, as FIRST-LAST, picked at random; its own range, not the OS's [default: 49152-65535].
    #[arg(long, value_name = "FIRST-LAST")]
    ephemeral_ports: Option<String>,
    /// Which statistic of a Trial Interval's one-way delay variation
    /// classifies it [default: maximum].
    #[arg(long, value_name = "STATISTIC")]
    delay_var_statistic: Option<DelayVar>,
    /// Renamed --delay-var-statistic; given, it is refused with that hint.
    #[arg(long = "delay-var", value_name = "STATISTIC", hide = true)]
    renamed_delay_var: Option<DelayVar>,
    /// How many Tests may run at once from one Source [default: 1, as RFC
    /// 9946 §4.2 requires].
    #[arg(long, value_name = "N")]
    tests_per_source: Option<usize>,
    /// How many Test Connections activated Tests may reserve at once [default: 256].
    /// The Ephemeral Port range must hold this plus the unactivated share.
    #[arg(long, value_name = "N")]
    max_test_connections: Option<usize>,
    /// How many Test Connections one Test may have over its life [default: 24].
    #[arg(long, value_name = "N")]
    test_connections_per_test: Option<usize>,
    /// How many Test Connections may be admitted but not yet activated at
    /// once, separately from the global cap [default: half of --max-test-connections, at least
    /// --test-connections-per-test].
    #[arg(long, value_name = "N")]
    max_unactivated_test_connections: Option<usize>,
    /// The Admitted Rate Cap: the most maxBandwidth admitted at once in
    /// each direction, in Mbps [default: no cap]. A Test over it is refused.
    #[arg(long, value_name = "MBPS")]
    admitted_rate_cap: Option<u32>,
    /// The most of the Admitted Rate Cap, in percent, that one Test without
    /// an Operator Key may state [default: 50].
    #[arg(long, value_name = "PERCENT", value_parser = clap::value_parser!(u8).range(1..=100))]
    max_test_share: Option<u8>,
    /// Refuse Tests that state no maxBandwidth.
    #[arg(long, value_name = "BOOL", num_args = 0..=1, require_equals = true, default_missing_value = "true")]
    require_max_bandwidth: Option<bool>,
    /// Answer authentication failures with the reason instead of silence,
    /// to debug Clients; up to 10 a second. Anyone can then learn whether
    /// keys are configured, and spoofed requests get replies.
    #[arg(long, value_name = "BOOL", num_args = 0..=1, require_equals = true, default_missing_value = "true")]
    troubleshooting: Option<bool>,
    /// How many Setup Requests a second may reach authentication; the rest
    /// are dropped silently [default: 20000].
    #[arg(long, value_name = "N")]
    unverified_setups_per_second: Option<u32>,
    /// How many authentic Setup Requests one Source may send at once
    /// [default: 48]; at least --test-connections-per-test.
    #[arg(long, value_name = "N")]
    source_setup_burst: Option<u32>,
    /// How many more a Source may send each second after its burst
    /// [default: 5].
    #[arg(long, value_name = "N")]
    source_setups_per_second: Option<u32>,
    /// How many authentic Setup Requests one Key Table row may send at
    /// once, Unauthenticated Mode counting as one [default: 1000]; at least
    /// --test-connections-per-test.
    #[arg(long, value_name = "N")]
    key_setup_burst: Option<u32>,
    /// How many more a Key Table row may send each second after its burst
    /// [default: 1000].
    #[arg(long, value_name = "N")]
    key_setups_per_second: Option<u32>,
    /// The DSCPs a Test without an Operator Key may mark its traffic with,
    /// comma-separated [default: 0]; another is coerced to 0.
    #[arg(long, value_name = "DSCP,...", value_delimiter = ',', num_args = 1..)]
    allowed_dscp: Option<Vec<u8>>,
    /// The DSCPs a Test with an Operator Key may mark its traffic with,
    /// comma-separated [default: all but network control, 48 and 56].
    #[arg(long, value_name = "DSCP,...", value_delimiter = ',', num_args = 1..)]
    operator_allowed_dscp: Option<Vec<u8>>,
    /// The highest Sending Rate Table row a Test without an Operator Key
    /// may start its search at [default: 100]; Operator Keys are exempt.
    #[arg(long, value_name = "ROW", value_parser = clap::value_parser!(u16).range(0..0xFFFF))]
    max_start_row: Option<u16>,
    /// The Quota Period Quotas are counted over, in seconds [default: 3600].
    #[arg(long, value_name = "SECONDS", value_parser = clap::value_parser!(u64).range(1..=31_536_000))]
    quota_period: Option<u64>,
    /// Each Source's Quota of Tests per Quota Period [default: none];
    /// Operator Keys are exempt.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    source_tests_per_quota_period: Option<u32>,
    /// Each Source's Quota of load bytes per Quota Period, both directions
    /// [default: none]; Operator Keys are exempt.
    #[arg(long, value_name = "BYTES", value_parser = clap::value_parser!(u64).range(1..))]
    source_bytes_per_quota_period: Option<u64>,
    /// Each Test Connection's receive and send buffer in its Data Phase,
    /// in bytes [default: 2 MiB]; the OS's limits may hold it lower.
    #[arg(long, value_name = "BYTES")]
    socket_buffer_size: Option<usize>,
    /// Set the Don't Fragment bit on test traffic [default: true]; false
    /// lets the path fragment it.
    #[arg(long, value_name = "BOOL", num_args = 0..=1, require_equals = true, default_missing_value = "true")]
    dont_fragment: Option<bool>,
    /// MaxHops: the IPv4 TTL and IPv6 hop limit of test traffic, from the
    /// expected path length (RFC 9946 §10 says the operator MUST set it)
    /// [default: 64, with a warning].
    #[arg(long, value_name = "HOPS", value_parser = clap::value_parser!(u8).range(1..))]
    max_hops: Option<u8>,
    /// The log's format on stdout [default: human]; json writes one object
    /// a line.
    #[arg(long, value_name = "FORMAT")]
    pub log_format: Option<LogFormat>,
    /// Where the log goes instead of stdout, with the startup's errors and
    /// warnings: a new file each day, the date before the extension, the
    /// last 14 kept.
    #[arg(long, value_name = "FILE")]
    pub log_file: Option<PathBuf>,
    /// How the log shows client addresses [default: full]: pseudonymize
    /// with a keyed hash, or truncate to the IPv4 /24 or IPv6 /48.
    #[arg(long, value_name = "HOW")]
    address_privacy: Option<crate::privacy::AddressPrivacy>,
    /// The pseudonym key file, hexadecimal digits (16 bytes or more), for
    /// --address-privacy pseudonymize.
    #[arg(long, value_name = "FILE")]
    address_privacy_key_file: Option<std::path::PathBuf>,
    /// How often the log reports the Server's counters, in seconds
    /// [default: 60]; 0 never does.
    #[arg(long, value_name = "SECONDS", value_parser = clap::value_parser!(u32).range(..=MAX_STATISTICS_INTERVAL as i64))]
    statistics_interval: Option<u32>,
    /// The key file: a TOML Key Table if its name ends in .toml, else an
    /// OB-UDPST udpst.keys file. Group and others must not be able to read
    /// it.
    #[arg(long, value_name = "FILE")]
    key_file: Option<PathBuf>,
}

/// Why the configuration can't be used.
#[derive(Debug)]
pub enum ConfigError {
    /// The file can't be read.
    Read { path: String, error: std::io::Error },
    /// The file isn't valid TOML, or has unknown or mistyped settings.
    Parse { path: String, message: String },
    /// A setting has a value outside its range.
    Invalid {
        setting: &'static str,
        message: String,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, error } => write!(f, "{path}: {error}"),
            Self::Parse { path, message } => write!(f, "{path}: {}", message.trim_end()),
            Self::Invalid { setting, message } => write!(f, "{setting}: {message}"),
        }
    }
}

impl ServerSettings {
    /// The defaults, replaced by the file's settings if a file is given,
    /// then by the command-line overrides.
    pub fn load(file: Option<&Path>, overrides: &ServerOverrides) -> Result<Self, ConfigError> {
        let mut settings = match file {
            Some(path) => Self::from_file(path)?,
            None => Self::default(),
        };
        if let Some(port) = overrides.port {
            settings.port = port;
        }
        if let Some(allow) = overrides.allow_unauthenticated {
            settings.allow_unauthenticated = allow;
        }
        if let Some(checksum) = overrides.checksum {
            settings.checksum = checksum;
        }
        if let Some(jumbo) = overrides.jumbo {
            settings.jumbo = jumbo;
        }
        if let Some(traditional) = overrides.traditional_mtu {
            settings.traditional_mtu = traditional;
        }
        if overrides.renamed_mtu.is_some() {
            return Err(ConfigError::Invalid {
                setting: "--mtu",
                message: "renamed --server-mtu (server_mtu in the configuration file)".into(),
            });
        }
        if let Some(mtu) = overrides.server_mtu {
            settings.server_mtu = mtu;
        }
        if let Some(ephemeral_ports) = &overrides.ephemeral_ports {
            settings.ephemeral_ports.clone_from(ephemeral_ports);
        }
        if overrides.renamed_delay_var.is_some() {
            return Err(ConfigError::Invalid {
                setting: "--delay-var",
                message: "renamed --delay-var-statistic (delay_var_statistic in the configuration file); the Client's --delay-var is another choice".into(),
            });
        }
        if let Some(statistic) = overrides.delay_var_statistic {
            settings.delay_var_statistic = statistic;
        }
        if let Some(tests) = overrides.tests_per_source {
            settings.tests_per_source = tests;
        }
        if let Some(connections) = overrides.max_test_connections {
            settings.max_test_connections = connections;
        }
        if let Some(connections) = overrides.test_connections_per_test {
            settings.test_connections_per_test = connections;
        }
        if let Some(connections) = overrides.max_unactivated_test_connections {
            settings.max_unactivated_test_connections = Some(connections);
        }
        if let Some(mbps) = overrides.admitted_rate_cap {
            settings.admitted_rate_cap = Some(mbps);
        }
        if let Some(require) = overrides.require_max_bandwidth {
            settings.require_max_bandwidth = require;
        }
        if let Some(troubleshooting) = overrides.troubleshooting {
            settings.troubleshooting = troubleshooting;
        }
        if let Some(budget) = overrides.unverified_setups_per_second {
            settings.unverified_setups_per_second = budget;
        }
        if let Some(burst) = overrides.source_setup_burst {
            settings.source_setup_burst = burst;
        }
        if let Some(rate) = overrides.source_setups_per_second {
            settings.source_setups_per_second = rate;
        }
        if let Some(burst) = overrides.key_setup_burst {
            settings.key_setup_burst = burst;
        }
        if let Some(rate) = overrides.key_setups_per_second {
            settings.key_setups_per_second = rate;
        }
        if let Some(allowed) = &overrides.allowed_dscp {
            settings.allowed_dscp.clone_from(allowed);
        }
        if let Some(allowed) = &overrides.operator_allowed_dscp {
            settings.operator_allowed_dscp = Some(allowed.clone());
        }
        if let Some(share) = overrides.max_test_share {
            settings.max_test_share = share;
        }
        if let Some(row) = overrides.max_start_row {
            settings.max_start_row = row;
        }
        if let Some(period) = overrides.quota_period {
            settings.quota_period = period;
        }
        if let Some(tests) = overrides.source_tests_per_quota_period {
            settings.source_tests_per_quota_period = Some(tests);
        }
        if let Some(bytes) = overrides.source_bytes_per_quota_period {
            settings.source_bytes_per_quota_period = Some(bytes);
        }
        if let Some(size) = overrides.socket_buffer_size {
            settings.socket_buffer_size = size;
        }
        if let Some(dont_fragment) = overrides.dont_fragment {
            settings.dont_fragment = dont_fragment;
        }
        if let Some(hops) = overrides.max_hops {
            settings.max_hops = Some(hops);
        }
        if let Some(format) = overrides.log_format {
            settings.log_format = format;
        }
        if let Some(path) = &overrides.log_file {
            settings.log_file = Some(path.clone());
        }
        if let Some(privacy) = overrides.address_privacy {
            settings.address_privacy = privacy;
        }
        if let Some(path) = &overrides.address_privacy_key_file {
            settings.address_privacy_key_file = Some(path.clone());
        }
        if !overrides.listen.is_empty() {
            settings.listen = overrides.listen.clone();
        }
        if let Some(interval) = overrides.statistics_interval {
            settings.statistics_interval = interval;
        }
        if let Some(key_file) = &overrides.key_file {
            settings.key_file = Some(key_file.clone());
        }
        settings.validate()?;
        Ok(settings)
    }

    fn from_file(path: &Path) -> Result<Self, ConfigError> {
        let shown = path.display().to_string();
        let text = std::fs::read_to_string(path).map_err(|error| ConfigError::Read {
            path: shown.clone(),
            error,
        })?;
        let parse_error = |error: toml::de::Error| ConfigError::Parse {
            path: shown.clone(),
            message: error.to_string(),
        };
        // Renamed before 0.1.0.
        let lenient: toml::Table = toml::from_str(&text).map_err(parse_error)?;
        if lenient.contains_key("mtu") {
            return Err(ConfigError::Invalid {
                setting: "mtu",
                message: format!("{shown}: renamed server_mtu"),
            });
        }
        if lenient.contains_key("delay_var") {
            return Err(ConfigError::Invalid {
                setting: "delay_var",
                message: format!("{shown}: renamed delay_var_statistic"),
            });
        }
        toml::from_str(&text).map_err(parse_error)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.port == 0 {
            return Err(ConfigError::Invalid {
                setting: "port",
                message: "must be between 1 and 65535".into(),
            });
        }
        let ports = self.ephemeral_port_range()?;
        for (setting, value) in [
            ("tests_per_source", self.tests_per_source),
            ("max_test_connections", self.max_test_connections),
            ("test_connections_per_test", self.test_connections_per_test),
        ] {
            if value == 0 {
                return Err(ConfigError::Invalid {
                    setting,
                    message: "must be at least 1".into(),
                });
            }
        }
        if let Some(unactivated) = self.max_unactivated_test_connections
            && unactivated < self.test_connections_per_test
        {
            return Err(ConfigError::Invalid {
                setting: "max_unactivated_test_connections",
                message: format!(
                    "must be at least test_connections_per_test ({}), so a whole Test can be set up",
                    self.test_connections_per_test
                ),
            });
        }
        // Every admitted Test Connection needs a port of its own, with
        // room for the global cap plus the separate unactivated share.
        // (conformance: unactivated-test-connection-share)
        let port_count = usize::from(*ports.end() - *ports.start()) + 1;
        let unactivated = self
            .max_unactivated_test_connections
            .unwrap_or(self.max_test_connections / 2)
            .max(self.test_connections_per_test);
        if port_count < self.max_test_connections.saturating_add(unactivated) {
            return Err(ConfigError::Invalid {
                setting: "ephemeral_ports",
                message: format!(
                    "{:?} has {port_count} ports, fewer than max_test_connections ({}) plus the Unactivated Test Connection share ({unactivated})",
                    self.ephemeral_ports, self.max_test_connections
                ),
            });
        }
        // Every Test a Source may run at once must fit, whole.
        let needed = self
            .tests_per_source
            .saturating_mul(self.test_connections_per_test);
        if (self.source_setup_burst as usize) < needed {
            return Err(ConfigError::Invalid {
                setting: "source_setup_burst",
                message: format!(
                    "must be at least tests_per_source × test_connections_per_test ({needed}), so every Test a source may run fits"
                ),
            });
        }
        if self.source_setups_per_second == 0 {
            return Err(ConfigError::Invalid {
                setting: "source_setups_per_second",
                message: "must be at least 1".into(),
            });
        }
        if (self.key_setup_burst as usize) < self.test_connections_per_test {
            return Err(ConfigError::Invalid {
                setting: "key_setup_burst",
                message: format!(
                    "must be at least test_connections_per_test ({}), so a whole Test fits",
                    self.test_connections_per_test
                ),
            });
        }
        if self.key_setups_per_second == 0 {
            return Err(ConfigError::Invalid {
                setting: "key_setups_per_second",
                message: "must be at least 1".into(),
            });
        }
        if self.unverified_setups_per_second == 0 {
            return Err(ConfigError::Invalid {
                setting: "unverified_setups_per_second",
                message: "must be at least 1".into(),
            });
        }
        if self.admitted_rate_cap == Some(0) {
            return Err(ConfigError::Invalid {
                setting: "admitted_rate_cap",
                message: "must be at least 1 Mbps; leave it out for no cap".into(),
            });
        }
        for (setting, list) in [
            ("allowed_dscp", Some(&self.allowed_dscp)),
            ("operator_allowed_dscp", self.operator_allowed_dscp.as_ref()),
        ] {
            let Some(list) = list else { continue };
            if list.is_empty() || list.iter().any(|&dscp| dscp > 63) {
                return Err(ConfigError::Invalid {
                    setting,
                    message: "must list DSCPs from 0 to 63; [0] allows only best effort".into(),
                });
            }
        }
        if !SOCKET_BUFFER_SIZES.contains(&self.socket_buffer_size) {
            return Err(ConfigError::Invalid {
                setting: "socket_buffer_size",
                message: SOCKET_BUFFER_SIZE_RANGE.into(),
            });
        }
        if self.max_hops == Some(0) {
            return Err(ConfigError::Invalid {
                setting: "max_hops",
                message: "must be from 1 to 255".into(),
            });
        }
        if !(1..=100).contains(&self.max_test_share) {
            return Err(ConfigError::Invalid {
                setting: "max_test_share",
                message: "must be from 1 to 100 %".into(),
            });
        }
        if self.server_mtu < 1280 {
            return Err(ConfigError::Invalid {
                setting: "server_mtu",
                message: "must be 1280 bytes or more, IPv6's minimum".into(),
            });
        }
        if self.traditional_mtu && self.server_mtu < 1500 {
            return Err(ConfigError::Invalid {
                setting: "server_mtu",
                message: "must be 1500 bytes or more with traditional_mtu".into(),
            });
        }
        if !(1..=31_536_000).contains(&self.quota_period) {
            return Err(ConfigError::Invalid {
                setting: "quota_period",
                message: "must be 1 to 31536000 seconds (a year)".into(),
            });
        }
        if self.source_tests_per_quota_period == Some(0)
            || self.source_bytes_per_quota_period == Some(0)
        {
            return Err(ConfigError::Invalid {
                setting: "source_tests_per_quota_period",
                message: "a Quota must be at least 1; leave it out for none".into(),
            });
        }
        if self.max_start_row == 0xFFFF {
            return Err(ConfigError::Invalid {
                setting: "max_start_row",
                message: "must be a Sending Rate Table row, 0 to 65534".into(),
            });
        }
        if let Some(ip) = self
            .listen
            .iter()
            .find(|ip| ip.is_multicast() || matches!(ip, IpAddr::V4(v4) if v4.is_broadcast()))
        {
            return Err(ConfigError::Invalid {
                setting: "listen",
                message: format!(
                    "{ip} is a multicast or broadcast address, which a Server can't listen on (RFC 9946 §6)"
                ),
            });
        }
        let mut seen = std::collections::HashSet::new();
        if let Some(ip) = self.listen.iter().find(|ip| !seen.insert(**ip)) {
            return Err(ConfigError::Invalid {
                setting: "listen",
                message: format!("{ip} is given twice"),
            });
        }
        if self.statistics_interval > MAX_STATISTICS_INTERVAL {
            return Err(ConfigError::Invalid {
                setting: "statistics_interval",
                message: "must be from 0 to 86400 s".into(),
            });
        }
        if let Some(path) = &self.log_file
            && crate::log::LogFile::new(path).is_none()
        {
            return Err(ConfigError::Invalid {
                setting: "log_file",
                message: "must name a file, with a UTF-8 name".into(),
            });
        }
        if self.key_file.as_ref().is_some_and(|p| p.to_str().is_none()) {
            return Err(ConfigError::Invalid {
                setting: "key_file",
                message: "the path must be UTF-8".into(),
            });
        }
        Ok(())
    }

    /// The `ephemeral_ports` range.
    pub fn ephemeral_port_range(&self) -> Result<RangeInclusive<u16>, ConfigError> {
        let invalid = || ConfigError::Invalid {
            setting: "ephemeral_ports",
            message: format!(
                "{:?} is not FIRST-LAST with 1 <= FIRST <= LAST <= 65535",
                self.ephemeral_ports
            ),
        };
        let (first, last) = self.ephemeral_ports.split_once('-').ok_or_else(invalid)?;
        let first: u16 = first.trim().parse().map_err(|_| invalid())?;
        let last: u16 = last.trim().parse().map_err(|_| invalid())?;
        if first == 0 || first > last {
            return Err(invalid());
        }
        Ok(first..=last)
    }

    /// `allowed_dscp` and `operator_allowed_dscp` as sets.
    pub fn dscp_sets(&self) -> (DscpSet, DscpSet) {
        let set = |list: &[u8]| DscpSet::of(list.iter().copied()).expect("validated");
        (
            set(&self.allowed_dscp),
            self.operator_allowed_dscp
                .as_deref()
                .map_or(DscpSet::ALL_BUT_NETWORK_CONTROL, set),
        )
    }

    /// The settings as TOML, as a configuration file would hold them.
    pub fn to_toml(&self) -> String {
        toml::to_string(self).expect("the settings serialize")
    }
}
