//! What the Client and the Server start with: their options turned
//! into what the runtime runs, with the warnings to show before it runs
//! and the errors that stop it, each with its exit status, as data.
//! `main.rs` prints them and runs; one module per role: [`client`] and
//! [`server`].
//!
//! Reading the files the options name happens here, once: the Client's key
//! file and pseudonym key file, and the Server's pseudonym key
//! file. What the host allows (marking a DSCP, setting DF, the socket
//! buffers it grants) comes in as [`Host`] and the Server's probe of its
//! socket buffers, and the Client's name resolution as a function, so the
//! rules are tested the same way on every OS, without a network. So are the
//! experimental environment variables for the Data Phase Engines, looked
//! up through a function in [`engine_settings`], once for the process
//!

pub mod client;
pub mod server;

use std::io;
use std::time::Duration;

use udpstp_runtime::{
    DEFAULT_MAX_HOPS, DONT_FRAGMENT_SUPPORTED, DSCP_SUPPORTED, EngineSettings, SendStampNumbering,
    SocketBuffers,
};

use crate::exit;

/// What this host lets test traffic carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Host {
    /// The OS lets test traffic be marked with a DSCP.
    pub dscp: bool,
    /// The OS lets DF be set on test traffic.
    pub dont_fragment: bool,
}

impl Host {
    /// This host, as the runtime reports it.
    pub const THIS: Self = Self {
        dscp: DSCP_SUPPORTED,
        dont_fragment: DONT_FRAGMENT_SUPPORTED,
    };
}

/// An error that stops the Client or the Server before it starts, most of
/// them in its configuration: the line it prints on stderr, and its exit
/// status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartError {
    pub status: u8,
    pub message: String,
}

impl StartError {
    fn new(status: u8, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    /// The runtime the Control Phase runs on couldn't be made (ADR 0003):
    /// the OS refused it `error`.
    pub fn runtime(error: &io::Error) -> Self {
        Self::new(
            exit::INITIALIZATION_ERROR,
            format!("udpstp: can't start the Control Phase's runtime: {error}"),
        )
    }
}

/// A warning shown on stderr before the Client or the Server starts: a
/// setting, or a limit of this host, that weakens what it does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupWarning {
    /// This host can't mark test traffic with a DSCP: the Client asks for
    /// DSCP 0 instead of the one it was given.
    DscpAskedAsZero,
    /// This host can't mark test traffic with a DSCP: the Server allows
    /// only 0, so every Test's DSCP is coerced to it.
    DscpCoercedToZero,
    /// This host can't set DF on test traffic.
    DontFragmentUnsupported,
    /// The Server caps the maxBandwidth it admits but doesn't require one,
    /// so Tests stating none are admitted without counting.
    AdmittedRateCapWithoutMaxBandwidth,
    /// The Server has no key file and doesn't allow Unauthenticated Mode,
    /// so it can admit no Test.
    NoTestAdmissible,
    /// Troubleshooting Mode answers authentication failures.
    TroubleshootingModeOn,
    /// The OS holds a Test Connection's socket buffers below the size
    /// asked for.
    SocketBuffersShort(SocketBuffers),
    /// The socket buffers the OS would grant couldn't be found out.
    SocketBuffersUnknown { error: String },
    /// MaxHops wasn't set, so test traffic has the default TTL or hop
    /// limit (RFC 9946 §10).
    MaxHopsNotSet,
    /// More than one Test at a time per Source (RFC 9946 §4.2).
    TestsPerSource { tests_per_source: usize },
}

/// Where DF can't be set yet.
const DONT_FRAGMENT_UNSUPPORTED: &str = "udpstp: warning: the Don't Fragment bit can't be set on this OS yet, so test traffic may be fragmented (RFC 9946 §5 says it SHOULD be set)";

/// Where the DSCP can't be set.
const DSCP_UNSUPPORTED: &str = "udpstp: warning: this OS doesn't let test traffic be marked with a DSCP (Windows reserves that for its QoS API)";

impl StartupWarning {
    /// Its name, as a Server logging to a file logs it.
    pub fn name(&self) -> &'static str {
        use crate::names::startup_warning as name;
        match self {
            Self::DscpAskedAsZero => name::DSCP_ASKED_AS_ZERO,
            Self::DscpCoercedToZero => name::DSCP_COERCED_TO_ZERO,
            Self::DontFragmentUnsupported => name::DONT_FRAGMENT_UNSUPPORTED,
            Self::AdmittedRateCapWithoutMaxBandwidth => {
                name::ADMITTED_RATE_CAP_WITHOUT_MAX_BANDWIDTH
            }
            Self::NoTestAdmissible => name::NO_TEST_ADMISSIBLE,
            Self::TroubleshootingModeOn => name::TROUBLESHOOTING_MODE_ON,
            Self::SocketBuffersShort(_) => name::SOCKET_BUFFERS_SHORT,
            Self::SocketBuffersUnknown { .. } => name::SOCKET_BUFFERS_UNKNOWN,
            Self::MaxHopsNotSet => name::MAX_HOPS_NOT_SET,
            Self::TestsPerSource { .. } => name::TESTS_PER_SOURCE,
        }
    }

    /// The line it prints on stderr.
    pub fn message(&self) -> String {
        match self {
            Self::DscpAskedAsZero => format!("{DSCP_UNSUPPORTED}; asking for DSCP 0"),
            Self::DscpCoercedToZero => {
                format!("{DSCP_UNSUPPORTED}; every Test's DSCP is coerced to 0")
            }
            Self::DontFragmentUnsupported => DONT_FRAGMENT_UNSUPPORTED.into(),
            Self::AdmittedRateCapWithoutMaxBandwidth => "udpstp: warning: admitted_rate_cap is set but require_max_bandwidth is off: Tests that state no maxBandwidth are admitted without counting".into(),
            Self::NoTestAdmissible => "udpstp: warning: no key file and Unauthenticated Mode is off, so the Server can admit no Test; give one with --key-file, or allow unauthenticated Tests".into(),
            Self::TroubleshootingModeOn => "udpstp: warning: Troubleshooting Mode is on: authentication failures are answered, telling anyone whether keys are configured and unauthenticated Tests allowed, and replying to spoofed sources; turn it off after debugging".into(),
            Self::SocketBuffersShort(buffers) => format!(
                "udpstp: warning: the OS holds socket buffers below the {} bytes asked for (receive {}, send {}); at high rates datagrams may be dropped locally, so raise {}",
                buffers.requested,
                buffers.receive,
                buffers.send,
                udpstp_runtime::SOCKET_BUFFER_LIMITS
            ),
            Self::SocketBuffersUnknown { error } => {
                format!("udpstp: warning: can't check socket buffer sizes: {error}")
            }
            Self::MaxHopsNotSet => format!(
                "udpstp: warning: max_hops (--max-hops) is not set, so test traffic has a TTL or hop limit of {DEFAULT_MAX_HOPS}; RFC 9946 §10 says the operator MUST set it from the expected path length"
            ),
            Self::TestsPerSource { tests_per_source } => format!(
                "udpstp: warning: tests_per_source is {tests_per_source}: RFC 9946 §4.2 allows one Test at a time per path"
            ),
        }
    }
}

/// Kernel send timestamps whose messages carry their own numbers where
/// the kernel takes them.
const SEND_STAMPS: &str = "UDPSTP_KERNEL_TX_TIMESTAMPS";
/// The catch-up budget, in µs.
const CATCH_UP: &str = "UDPSTP_ENGINE_CATCH_UP_US";
/// A fixed receive hold-off, in µs.
const HOLD_OFF: &str = "UDPSTP_ENGINE_HOLD_OFF_US";
/// The most Engine Threads the pooled executor starts.
const THREADS: &str = "UDPSTP_ENGINE_THREADS";
/// The pooled executor's wake-up grouping window, in µs.
const GROUPING: &str = "UDPSTP_ENGINE_GROUPING_US";

/// How the Data Phase Engines run, from the experimental environment
/// variables `variable` looks up (not a stable interface; ADR 0013,
/// `docs/interfaces.md`), read here once, for the whole process:
///
/// - `UDPSTP_KERNEL_TX_TIMESTAMPS`: `1` asks for kernel send timestamps
///   whose messages carry their own numbers where the kernel takes them,
///   `kernel-count` for the kernel's own count on every kernel;
///   any other value for none;
/// - `UDPSTP_ENGINE_CATCH_UP_US`: the catch-up budget in µs (ADR 0012);
/// - `UDPSTP_ENGINE_HOLD_OFF_US`: a fixed receive hold-off in µs, 0 for
///   none;
/// - `UDPSTP_ENGINE_THREADS`: the most Engine Threads the pooled executor
///   starts, in place of the CPUs the process may use;
/// - `UDPSTP_ENGINE_GROUPING_US`: the pooled executor's wake-up grouping
///   window in µs, 0 for none, the default (no measurement has shown
///   a window worth making the default: ADR 0008).
///
/// A value that isn't a whole number of µs or a positive count leaves the
/// engine's own choice.
pub fn engine_settings(variable: impl Fn(&str) -> Option<String>) -> EngineSettings {
    let value = |name: &str| variable(name).map(|value| value.trim().to_owned());
    let micros = |name: &str| value(name)?.parse().ok().map(Duration::from_micros);
    EngineSettings {
        catch_up_budget: micros(CATCH_UP),
        hold_off: micros(HOLD_OFF),
        send_stamps: match value(SEND_STAMPS).as_deref() {
            Some("1") => Some(SendStampNumbering::Explicit),
            Some("kernel-count") => Some(SendStampNumbering::Kernel),
            _ => None,
        },
        threads: value(THREADS).and_then(|threads| threads.parse().ok()),
        grouping: micros(GROUPING).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from(variables: &[(&str, &str)]) -> EngineSettings {
        engine_settings(|name| {
            variables
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, value)| (*value).to_string())
        })
    }

    // : the experimental variables are read in the binary
    // and given to the engines; none set is the engine's own choice.
    #[test]
    fn no_variable_leaves_the_engines_their_own_choices() {
        assert_eq!(from(&[]), EngineSettings::default());
    }

    // ADR 0012, , : the catch-up budget and the receive hold-off
    // in µs, the hold-off 0 for none; anything but a whole number of µs is
    // ignored.
    #[test]
    fn the_catch_up_budget_and_the_hold_off_are_whole_microseconds() {
        let settings = from(&[(CATCH_UP, " 1500\n"), (HOLD_OFF, "0")]);
        assert_eq!(settings.catch_up_budget, Some(Duration::from_micros(1500)));
        assert_eq!(settings.hold_off, Some(Duration::ZERO));
        for value in ["", "1.5", "-1", "1ms"] {
            let settings = from(&[(CATCH_UP, value), (HOLD_OFF, value)]);
            assert_eq!(settings, EngineSettings::default(), "{value:?}");
        }
    }

    //  (D3, D4): `UDPSTP_ENGINE_THREADS` caps the pool's Engine
    // Threads, a positive count, and `UDPSTP_ENGINE_GROUPING_US` sets its
    // grouping window in µs, 0 (none) by default. Anything else is
    // ignored.
    #[test]
    fn the_engine_threads_and_the_grouping_window() {
        let settings = from(&[(THREADS, " 2\n"), (GROUPING, "50")]);
        assert_eq!(settings.threads.map(|n| n.get()), Some(2));
        assert_eq!(settings.grouping, Duration::from_micros(50));
        assert_eq!(EngineSettings::default().grouping, Duration::ZERO);
        for value in ["", "0", "-1", "1.5", "two"] {
            let settings = from(&[(THREADS, value), (GROUPING, value)]);
            assert_eq!(settings, EngineSettings::default(), "{value:?}");
        }
    }

    // : the Linux Server's executor was a setting,
    // `UDPSTP_ENGINE_EXECUTOR`, only while the pool was measured against a
    // thread per Test Connection; the pool is now its only one (ADR 0008),
    // and the variable changes nothing.
    #[test]
    fn the_executor_is_not_a_setting() {
        let settings = from(&[("UDPSTP_ENGINE_EXECUTOR", "per-test-connection")]);
        assert_eq!(settings, EngineSettings::default());
    }

    // : `UDPSTP_KERNEL_TX_TIMESTAMPS=kernel-count` asks for kernel send
    // timestamps matched by the kernel's own count, `1` for numbers the
    // messages carry where the kernel takes them, and any other value, or
    // none, for no timestamps. The kernel's count is tested through the
    // runtime's fault adapter's `ts_opt_id_unsupported`.
    #[test]
    fn the_variable_chooses_who_numbers_the_messages() {
        let asked = |value: &str| from(&[(SEND_STAMPS, value)]).send_stamps;
        assert_eq!(asked("kernel-count"), Some(SendStampNumbering::Kernel));
        assert_eq!(asked(" kernel-count\n"), Some(SendStampNumbering::Kernel));
        assert_eq!(asked("1"), Some(SendStampNumbering::Explicit));
        for value in ["", "0", "true", "kernel"] {
            assert_eq!(asked(value), None, "{value:?}");
        }
    }
}
