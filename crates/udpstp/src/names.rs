//! The stable names udpstp writes in the Server's log, its statistics and
//! the JSON results: every event name, and a name for each variant
//! of the core's enums that appears as a value. Each comes from an
//! exhaustive match here, never from Rust's `Debug` output, so renaming a
//! core variant can't change what users read. A test compares every name
//! with the checked-in list `tests/golden/names.txt`.
//!
//! A variant with a payload is flattened into one name
//! (`MultiConnectionIdentZero`), and an authentication failure has one name
//! whether the Server answered it (Troubleshooting Mode) or rejected it
//! silently: only the event differs.

use udpstp_core::{
    ActivationParameter, ActivationRejection, AuthenticationFailure, ControlPhaseFailure,
    Direction, EndReason, MultiConnectionError, SetupIgnoreReason, SetupRejection,
    SetupRejectionCode, SilentRejectionReason, TestAbort,
};

/// The Server log's event names, and the warning codes of the Client's
/// JSON results, which reuse them.
pub mod event {
    pub const LISTENING: &str = "Listening";
    pub const NO_IPV6_LISTENER: &str = "NoIpv6Listener";
    pub const TEST_CONNECTION_ADMITTED: &str = "TestConnectionAdmitted";
    pub const ACTIVATION_WATCHDOG_EXPIRED: &str = "ActivationWatchdogExpired";
    pub const UNACTIVATED_TEST_CONNECTION_EVICTED: &str = "UnactivatedTestConnectionEvicted";
    pub const TROUBLESHOOTING_MODE_ON: &str = "TroubleshootingModeOn";
    pub const TEST_CONNECTION_RELEASED: &str = "TestConnectionReleased";
    pub const SETUP_REQUEST_REJECTED: &str = "SetupRequestRejected";
    pub const SETUP_REQUEST_SILENTLY_REJECTED: &str = "SetupRequestSilentlyRejected";
    pub const SETUP_REQUEST_IGNORED: &str = "SetupRequestIgnored";
    pub const TEST_CONNECTION_ACTIVATED: &str = "TestConnectionActivated";
    pub const ACTIVATION_PARAMETER_COERCED: &str = "ActivationParameterCoerced";
    pub const ACTIVATION_REQUEST_SILENTLY_REJECTED: &str = "ActivationRequestSilentlyRejected";
    pub const ACTIVATION_REQUEST_REJECTED: &str = "ActivationRequestRejected";
    pub const DATA_PHASE_ENDED: &str = "DataPhaseEnded";
    pub const NO_EPHEMERAL_PORT: &str = "NoEphemeralPort";
    pub const SOCKET_ERROR: &str = "SocketError";
    pub const SOCKET_BUFFERS_SHORT: &str = "SocketBuffersShort";
    pub const DATAGRAMS_DROPPED: &str = "DatagramsDropped";
    pub const BURSTS_SKIPPED: &str = "BurstsSkipped";
    pub const BURSTS_WITHHELD: &str = "BurstsWithheld";
    pub const FIRST_LOAD_PDU_RECEIVED: &str = "FirstLoadPduReceived";
    pub const FIRST_STATUS_PDU_RECEIVED: &str = "FirstStatusPduReceived";
    pub const INVALID_LOAD_PDUS: &str = "InvalidLoadPdus";
    pub const IGNORED_LOAD_PDUS: &str = "IgnoredLoadPdus";
    pub const STATUS_PDUS_FAILED_VERIFICATION: &str = "StatusPdusFailedVerification";
    pub const STATUS_PDUS_MISSING: &str = "StatusPdusMissing";
    pub const PEER_MISSED_STATUS_PDUS: &str = "PeerMissedStatusPdus";
    pub const PEER_RX_STOPPED: &str = "PeerRxStopped";
    pub const DELAY_SAMPLES_DROPPED: &str = "DelaySamplesDropped";
    pub const INVALID_STATUS_PDUS: &str = "InvalidStatusPdus";
    pub const PATH_MTU_EXCEEDED: &str = "PathMtuExceeded";
    pub const HOP_LIMIT_EXCEEDED: &str = "HopLimitExceeded";
    pub const ICMP_ERRORS: &str = "IcmpErrors";
    pub const STALE_STATUS_PDUS: &str = "StaleStatusPdus";
    pub const STOP_RECEIVED: &str = "StopReceived";
    pub const LOST_STATUS_BACKOFF: &str = "LostStatusBackoff";
    pub const TRAFFIC_STOPPED: &str = "TrafficStopped";
    pub const SUPPRESSED: &str = "Suppressed";
    pub const TEST_SUMMARY: &str = "TestSummary";
    pub const STATISTICS: &str = "Statistics";
    pub const STATISTICS_REFUSALS: &str = "StatisticsRefusals";
    pub const STOPPING: &str = "Stopping";
    pub const DATA_PHASE_ENGINES: &str = "DataPhaseEngines";
    pub const STARTUP_ERROR: &str = "StartupError";
    pub const STARTUP_WARNING: &str = "StartupWarning";

    /// Every event the Server's log writes.
    pub const ALL: [&str; 46] = [
        LISTENING,
        NO_IPV6_LISTENER,
        TEST_CONNECTION_ADMITTED,
        ACTIVATION_WATCHDOG_EXPIRED,
        UNACTIVATED_TEST_CONNECTION_EVICTED,
        TROUBLESHOOTING_MODE_ON,
        TEST_CONNECTION_RELEASED,
        SETUP_REQUEST_REJECTED,
        SETUP_REQUEST_SILENTLY_REJECTED,
        SETUP_REQUEST_IGNORED,
        TEST_CONNECTION_ACTIVATED,
        ACTIVATION_PARAMETER_COERCED,
        ACTIVATION_REQUEST_SILENTLY_REJECTED,
        ACTIVATION_REQUEST_REJECTED,
        DATA_PHASE_ENDED,
        NO_EPHEMERAL_PORT,
        SOCKET_ERROR,
        SOCKET_BUFFERS_SHORT,
        DATAGRAMS_DROPPED,
        BURSTS_SKIPPED,
        BURSTS_WITHHELD,
        FIRST_LOAD_PDU_RECEIVED,
        FIRST_STATUS_PDU_RECEIVED,
        INVALID_LOAD_PDUS,
        IGNORED_LOAD_PDUS,
        STATUS_PDUS_FAILED_VERIFICATION,
        STATUS_PDUS_MISSING,
        PEER_MISSED_STATUS_PDUS,
        PEER_RX_STOPPED,
        DELAY_SAMPLES_DROPPED,
        INVALID_STATUS_PDUS,
        PATH_MTU_EXCEEDED,
        HOP_LIMIT_EXCEEDED,
        ICMP_ERRORS,
        STALE_STATUS_PDUS,
        STOP_RECEIVED,
        LOST_STATUS_BACKOFF,
        TRAFFIC_STOPPED,
        SUPPRESSED,
        TEST_SUMMARY,
        STATISTICS,
        STATISTICS_REFUSALS,
        STOPPING,
        DATA_PHASE_ENGINES,
        STARTUP_ERROR,
        STARTUP_WARNING,
    ];
}

/// A startup warning, as a Server logging to a file names it in its
/// `StartupWarning` event's `reason`.
pub mod startup_warning {
    pub const DSCP_ASKED_AS_ZERO: &str = "DscpAskedAsZero";
    pub const DSCP_COERCED_TO_ZERO: &str = "DscpCoercedToZero";
    pub const DONT_FRAGMENT_UNSUPPORTED: &str = "DontFragmentUnsupported";
    pub const ADMITTED_RATE_CAP_WITHOUT_MAX_BANDWIDTH: &str = "AdmittedRateCapWithoutMaxBandwidth";
    pub const NO_TEST_ADMISSIBLE: &str = "NoTestAdmissible";
    pub const TROUBLESHOOTING_MODE_ON: &str = "TroubleshootingModeOn";
    pub const SOCKET_BUFFERS_SHORT: &str = "SocketBuffersShort";
    pub const SOCKET_BUFFERS_UNKNOWN: &str = "SocketBuffersUnknown";
    pub const MAX_HOPS_NOT_SET: &str = "MaxHopsNotSet";
    pub const TESTS_PER_SOURCE: &str = "TestsPerSource";
    pub const KEY_TABLE_FORMAT_DEPRECATED: &str = "KeyTableFormatDeprecated";

    // The names test lists them all.
    #[allow(dead_code)]
    pub const ALL: [&str; 11] = [
        DSCP_ASKED_AS_ZERO,
        DSCP_COERCED_TO_ZERO,
        DONT_FRAGMENT_UNSUPPORTED,
        ADMITTED_RATE_CAP_WITHOUT_MAX_BANDWIDTH,
        NO_TEST_ADMISSIBLE,
        TROUBLESHOOTING_MODE_ON,
        SOCKET_BUFFERS_SHORT,
        SOCKET_BUFFERS_UNKNOWN,
        MAX_HOPS_NOT_SET,
        TESTS_PER_SOURCE,
        KEY_TABLE_FORMAT_DEPRECATED,
    ];
}

/// What stopped the Server, as its log's `Stopping` names it.
// Each platform uses only its own; the test lists them all.
#[allow(dead_code)]
pub mod stop_reason {
    pub const SIGINT: &str = "SIGINT";
    pub const SIGTERM: &str = "SIGTERM";
    pub const SIGHUP: &str = "SIGHUP";
    pub const CTRL_C: &str = "CtrlC";
    pub const CTRL_BREAK: &str = "CtrlBreak";
    pub const CTRL_CLOSE: &str = "CtrlClose";
    pub const CTRL_SHUTDOWN: &str = "CtrlShutdown";
    pub const SERVICE_STOP: &str = "ServiceStop";
    pub const SERVICE_SHUTDOWN: &str = "ServiceShutdown";

    pub const ALL: [&str; 9] = [
        SIGINT,
        SIGTERM,
        SIGHUP,
        CTRL_C,
        CTRL_BREAK,
        CTRL_CLOSE,
        CTRL_SHUTDOWN,
        SERVICE_STOP,
        SERVICE_SHUTDOWN,
    ];
}

/// The `result` of a `TestSummary` that isn't an end's name.
pub mod summary {
    /// None of the Test's Test Connections was activated.
    pub const NOT_ACTIVATED: &str = "NotActivated";
    /// The Server cut the Test short because it was stopping; also why it
    /// ignored a Setup Request then.
    pub const SERVER_STOPPING: &str = "ServerStopping";

    #[cfg(test)]
    pub const ALL: [&str; 2] = [NOT_ACTIVATED, SERVER_STOPPING];
}

/// How a Test Connection ended, in the JSON's `EndReason`, when not by an
/// [`EndReason`] of its Data Phase.
pub mod outcome {
    /// Its Data Phase Engine failed; also a `DataPhaseEnded` reason.
    pub const DATA_PHASE_ENGINE_FAILED: &str = "DataPhaseEngineFailed";
    pub const ACTIVATION_REJECTED: &str = "ActivationRejected";
    pub const CONTROL_PHASE_FAILED: &str = "ControlPhaseFailed";
    /// The Multi-Connection Coordinator gave it up.
    pub const ABANDONED: &str = "Abandoned";
    pub const SOCKET_ERROR: &str = "SocketError";

    #[cfg(test)]
    pub const ALL: [&str; 5] = [
        DATA_PHASE_ENGINE_FAILED,
        ACTIVATION_REJECTED,
        CONTROL_PHASE_FAILED,
        ABANDONED,
        SOCKET_ERROR,
    ];
}

/// How a Multi-Connection Test ended, in the JSON's `EndReason`, when it
/// neither completed nor was aborted; an aborted one has
/// [`test_abort`]'s name.
pub mod test_end {
    /// Every Test Connection ended without completing, though the
    /// Multi-Connection Coordinator didn't abort the Test.
    pub const NO_TEST_CONNECTION_COMPLETED: &str = "NoTestConnectionCompleted";

    #[cfg(test)]
    pub const ALL: [&str; 1] = [NO_TEST_CONNECTION_COMPLETED];
}

/// Why the Multi-Connection Coordinator aborted a Test: a Multi-Connection
/// Test's `EndReason` in the JSON.
pub fn test_abort(reason: TestAbort) -> &'static str {
    match reason {
        TestAbort::TooFewTestConnections { .. } => "TooFewTestConnections",
        TestAbort::ParametersDiffer { .. } => "ParametersDiffer",
        TestAbort::TestConnectionEnded { .. } => "TestConnectionEnded",
        TestAbort::TestConnectionsTimedOut => "TestConnectionsTimedOut",
        TestAbort::Interrupted => "Interrupted",
    }
}

/// The codes of the Client's warnings that no Server log event names; the
/// others are [`event`] names.
pub mod warning {
    pub const SENDING_RATE_CAPPED: &str = "SendingRateCapped";
    pub const FIRST_MODE_SPLIT_IGNORED: &str = "FirstModeSplitIgnored";
    pub const UPSTREAM_RESULTS_APPROXIMATE: &str = "UpstreamResultsApproximate";
    pub const NO_VALID_MAXIMUM: &str = "NoValidMaximum";
    pub const INTERRUPTED: &str = "Interrupted";
    /// Datagrams the OS discarded at a socket before udpstp read them
    /// (Receive Buffer Overflows).
    pub const RECEIVE_BUFFER_OVERFLOWS: &str = "ReceiveBufferOverflows";
    /// A Downstream Fixed-Rate Test's Server sent well below its row's
    /// sending rate where the result was measured: it may not have kept
    /// up.
    pub const SENDING_RATE_NOT_KEPT: &str = "SendingRateNotKept";
    /// A Search Test none of whose searches found the path's limit, neither
    /// confirming Congestion nor meeting it at its end.
    pub const CONGESTION_NEVER_CONFIRMED: &str = "CongestionNeverConfirmed";
    /// A Search Test's Maximum IP-Layer Capacity came before Sub-Intervals
    /// that all missed the Performance Criterion, one of them well faster:
    /// it may be well below the path.
    pub const SUB_INTERVALS_AFTER_MAXIMUM_MISSED: &str = "SubIntervalsAfterMaximumMissed";

    #[cfg(test)]
    pub const ALL: [&str; 9] = [
        SENDING_RATE_CAPPED,
        FIRST_MODE_SPLIT_IGNORED,
        UPSTREAM_RESULTS_APPROXIMATE,
        NO_VALID_MAXIMUM,
        INTERRUPTED,
        RECEIVE_BUFFER_OVERFLOWS,
        SENDING_RATE_NOT_KEPT,
        CONGESTION_NEVER_CONFIRMED,
        SUB_INTERVALS_AFTER_MAXIMUM_MISSED,
    ];
}

/// The Data Phase Engine a host ran, as the JSON's `DataPhaseEngine`
/// names it.
pub mod data_phase_engine {
    /// One Engine Thread per Test Connection: the per-Test-Connection
    /// executor, the Client's on every OS (ADR 0008).
    pub const PORTABLE: &str = "portable";

    #[cfg(test)]
    pub const ALL: [&str; 1] = [PORTABLE];
}

/// The fast paths a Data Phase Engine sent Load PDUs, read
/// datagrams and waited with, as the JSON's `FastPaths` and
/// the log's `fast_paths` name them.
pub mod fast_path {
    /// Load PDUs went to the kernel in batches.
    pub const SENDMMSG: &str = "sendmmsg";
    /// Runs of equal-sized Load PDUs went as one message each, which the
    /// kernel split into datagrams.
    pub const UDP_GSO: &str = "UdpGso";
    /// Datagrams were read in batches, a Load Receiver's header-only.
    pub const RECVMMSG: &str = "recvmmsg";
    /// Datagrams were timed by the kernel's receive timestamps.
    pub const KERNEL_RX_TIMESTAMPS: &str = "KernelRxTimestamps";
    /// Between turns the engine waited in ppoll on its socket.
    pub const PPOLL: &str = "ppoll";
    /// The socket was in its Engine Thread's epoll set: the Linux Server's
    /// pooled executor.
    pub const EPOLL: &str = "epoll";

    #[cfg(test)]
    pub const ALL: [&str; 6] = [
        SENDMMSG,
        UDP_GSO,
        RECVMMSG,
        KERNEL_RX_TIMESTAMPS,
        PPOLL,
        EPOLL,
    ];
}

/// The executor a Server's Data Phase Engine runs on, as the log's
/// `DataPhaseEngines` names it.
pub mod executor {
    /// Engine Threads, each serving many Test Connections: a Linux
    /// Server's only executor (ADR 0008).
    pub const POOL: &str = "pool";
    /// One Engine Thread per Test Connection: a Server's on Windows and
    /// macOS.
    pub const PER_TEST_CONNECTION: &str = "per_test_connection";

    #[cfg(test)]
    pub const ALL: [&str; 2] = [POOL, PER_TEST_CONNECTION];
}

/// The name of `executor`.
pub fn executor(executor: udpstp_runtime::Executor) -> &'static str {
    match executor {
        udpstp_runtime::Executor::Pool => executor::POOL,
        udpstp_runtime::Executor::PerTestConnection => executor::PER_TEST_CONNECTION,
    }
}

/// The names of the fast paths in `paths`, in [`fast_path`]'s order.
pub fn fast_paths(paths: udpstp_runtime::FastPaths) -> Vec<&'static str> {
    [
        (paths.sendmmsg, fast_path::SENDMMSG),
        (paths.udp_gso, fast_path::UDP_GSO),
        (paths.recvmmsg, fast_path::RECVMMSG),
        (paths.kernel_rx_timestamps, fast_path::KERNEL_RX_TIMESTAMPS),
        (paths.ppoll, fast_path::PPOLL),
        (paths.epoll, fast_path::EPOLL),
    ]
    .into_iter()
    .filter_map(|(on, name)| on.then_some(name))
    .collect()
}

/// A parameter's RFC 9946 name: its field, or for the modifierBitmap
/// flags, the flag's constant. `--refuse-coercion` takes these names.
pub fn parameter(parameter: ActivationParameter) -> &'static str {
    match parameter {
        ActivationParameter::LowThresh => "lowThresh",
        ActivationParameter::UpperThresh => "upperThresh",
        ActivationParameter::TrialInt => "trialInt",
        ActivationParameter::TestIntTime => "testIntTime",
        ActivationParameter::DscpEcn => "dscpEcn",
        ActivationParameter::SrIndexConf => "srIndexConf",
        ActivationParameter::UseOwDelVar => "useOwDelVar",
        ActivationParameter::HighSpeedDelta => "highSpeedDelta",
        ActivationParameter::SlowAdjThresh => "slowAdjThresh",
        ActivationParameter::SeqErrThresh => "seqErrThresh",
        ActivationParameter::IgnoreOooDup => "ignoreOooDup",
        ActivationParameter::SrIndexIsStart => "CHTA_SRIDX_ISSTART",
        ActivationParameter::RandomPayload => "CHTA_RAND_PAYLOAD",
        ActivationParameter::RateAdjAlgo => "rateAdjAlgo",
        ActivationParameter::SubIntPeriod => "subIntPeriod",
    }
}

/// An Activation parameter by its RFC 9946 name, in any case, for
/// `--refuse-coercion`.
pub fn activation_parameter(name: &str) -> Result<ActivationParameter, String> {
    ActivationParameter::ALL
        .into_iter()
        .find(|p| parameter(*p).eq_ignore_ascii_case(name))
        .ok_or_else(|| {
            let names: Vec<&str> = ActivationParameter::ALL.map(parameter).to_vec();
            format!(
                "not an RFC 9946 Activation parameter; one of {}",
                names.join(", ")
            )
        })
}

pub fn direction(direction: Direction) -> &'static str {
    match direction {
        Direction::Upstream => "Upstream",
        Direction::Downstream => "Downstream",
    }
}

/// rateAdjAlgo as the Load Rate Adjustment Algorithm's letter.
pub fn rate_adj_algo(rate_adj_algo: u8) -> &'static str {
    match rate_adj_algo {
        0 => "B",
        1 => "C",
        _ => "Unknown",
    }
}

pub fn end_reason(reason: EndReason) -> &'static str {
    match reason {
        EndReason::Completed => "Completed",
        EndReason::Unconfirmed => "Unconfirmed",
        EndReason::TestIntervalElapsed => "TestIntervalElapsed",
        EndReason::StoppedByClient => "StoppedByClient",
        EndReason::Stopped => "Stopped",
        EndReason::Watchdog => "Watchdog",
        EndReason::OverRate => "OverRate",
    }
}

/// An authentication failure, answered or not.
pub fn authentication_failure(failure: AuthenticationFailure) -> &'static str {
    match failure {
        AuthenticationFailure::Required => "AuthenticationRequired",
        AuthenticationFailure::NotConfigured => "AuthenticationNotConfigured",
        AuthenticationFailure::ModeInvalid => "AuthenticationModeInvalid",
        AuthenticationFailure::UnknownKeyId => "AuthenticationUnknownKeyId",
        AuthenticationFailure::KeyOutsideLifetime => "AuthenticationKeyOutsideLifetime",
        AuthenticationFailure::DigestFailed => "AuthenticationDigestFailed",
        AuthenticationFailure::TimeOutOfWindow => "AuthenticationTimeOutOfWindow",
        AuthenticationFailure::BudgetExhausted => "UnverifiedBudgetExhausted",
    }
}

fn multi_connection_error(error: MultiConnectionError) -> &'static str {
    match error {
        MultiConnectionError::IdentZero => "MultiConnectionIdentZero",
        MultiConnectionError::IndexNotBelowCount => "MultiConnectionIndexNotBelowCount",
        MultiConnectionError::IndexInUse => "MultiConnectionIndexInUse",
        MultiConnectionError::CountDiffers => "MultiConnectionCountDiffers",
        MultiConnectionError::AuthModeDiffers => "MultiConnectionAuthModeDiffers",
        MultiConnectionError::KeyIdDiffers => "MultiConnectionKeyIdDiffers",
    }
}

pub fn setup_rejection(rejection: SetupRejection) -> &'static str {
    match rejection {
        SetupRejection::TestsPerSource => "TestsPerSource",
        SetupRejection::TestConnectionsPerTest => "TestConnectionsPerTest",
        SetupRejection::TestConnections => "TestConnections",
        SetupRejection::MaxBandwidthRequired => "MaxBandwidthRequired",
        SetupRejection::CapacityExceeded => "CapacityExceeded",
        SetupRejection::SocketUnavailable => "SocketUnavailable",
        SetupRejection::QuotaExceeded => "QuotaExceeded",
        SetupRejection::SourceBudgetExhausted => "SourceBudgetExhausted",
        SetupRejection::MultiConnection(error) => multi_connection_error(error),
        SetupRejection::BadVersion => "BadVersion",
        SetupRejection::JumboMismatch => "JumboMismatch",
        SetupRejection::TraditionalMtuMismatch => "TraditionalMtuMismatch",
        SetupRejection::Authentication(failure) => authentication_failure(failure),
    }
}

/// The reason an Activation Request was rejected; the parameter of
/// `UnacceptableParameter` is a field of its own.
pub fn activation_rejection(rejection: ActivationRejection) -> &'static str {
    match rejection {
        ActivationRejection::InvalidCommand => "InvalidCommand",
        ActivationRejection::MixedDirection => "MixedDirection",
        ActivationRejection::UnacceptableParameter(_) => "UnacceptableParameter",
        ActivationRejection::FixedRateNeedsOperatorKey => "FixedRateNeedsOperatorKey",
        ActivationRejection::TestsPerSource => "TestsPerSource",
        ActivationRejection::TestConnections => "TestConnections",
        ActivationRejection::MaxBandwidthDirection => "MaxBandwidthDirection",
        ActivationRejection::CapacityExceeded => "CapacityExceeded",
        ActivationRejection::QuotaExceeded => "QuotaExceeded",
    }
}

/// Why a request was rejected silently; an authentication failure has
/// the name [`authentication_failure`] gives it.
pub fn silent_rejection_reason(reason: SilentRejectionReason) -> &'static str {
    match reason {
        SilentRejectionReason::Malformed => "Malformed",
        SilentRejectionReason::UnauthenticatedNotAllowed => {
            authentication_failure(AuthenticationFailure::Required)
        }
        SilentRejectionReason::UnsupportedAuthMode => {
            authentication_failure(AuthenticationFailure::ModeInvalid)
        }
        SilentRejectionReason::NoKeysConfigured => {
            authentication_failure(AuthenticationFailure::NotConfigured)
        }
        SilentRejectionReason::UnverifiedBudgetExhausted => {
            authentication_failure(AuthenticationFailure::BudgetExhausted)
        }
        SilentRejectionReason::SourceBudgetExhausted => "SourceBudgetExhausted",
        SilentRejectionReason::KeyBudgetExhausted => "KeyBudgetExhausted",
        SilentRejectionReason::UnknownKeyId => {
            authentication_failure(AuthenticationFailure::UnknownKeyId)
        }
        SilentRejectionReason::KeyOutsideLifetime => {
            authentication_failure(AuthenticationFailure::KeyOutsideLifetime)
        }
        SilentRejectionReason::AuthenticationFailed => {
            authentication_failure(AuthenticationFailure::DigestFailed)
        }
        SilentRejectionReason::AuthTimeOutOfWindow => {
            authentication_failure(AuthenticationFailure::TimeOutOfWindow)
        }
        SilentRejectionReason::AuthModeMismatch => "AuthModeMismatch",
        SilentRejectionReason::KeyIdMismatch => "KeyIdMismatch",
        SilentRejectionReason::MulticastOrBroadcast => "MulticastOrBroadcast",
        SilentRejectionReason::ChecksumFailed => "ChecksumFailed",
        SilentRejectionReason::Replayed => "Replayed",
        SilentRejectionReason::ReplayCacheFull => "ReplayCacheFull",
        SilentRejectionReason::NotFromClient => "NotFromClient",
    }
}

/// Why a Setup Request was ignored, not refused.
pub fn setup_ignore_reason(reason: SetupIgnoreReason) -> &'static str {
    match reason {
        SetupIgnoreReason::ArrivalAddressUnknown => "ArrivalAddressUnknown",
        SetupIgnoreReason::DuplicateSetupRequest => "DuplicateSetupRequest",
        SetupIgnoreReason::Stopping => summary::SERVER_STOPPING,
    }
}

fn setup_rejection_code(code: SetupRejectionCode) -> &'static str {
    match code {
        SetupRejectionCode::BadVersion { .. } => "SetupRejectedBadVersion",
        SetupRejectionCode::JumboMismatch => "SetupRejectedJumboMismatch",
        SetupRejectionCode::AuthNotConfigured => "SetupRejectedAuthNotConfigured",
        SetupRejectionCode::AuthRequired => "SetupRejectedAuthRequired",
        SetupRejectionCode::AuthModeInvalid => "SetupRejectedAuthModeInvalid",
        SetupRejectionCode::AuthFailed => "SetupRejectedAuthFailed",
        SetupRejectionCode::AuthTimeInvalid => "SetupRejectedAuthTimeInvalid",
        SetupRejectionCode::MaxBandwidthRequired => "SetupRejectedMaxBandwidthRequired",
        SetupRejectionCode::CapacityExceeded => "SetupRejectedCapacityExceeded",
        SetupRejectionCode::TraditionalMtuMismatch => "SetupRejectedTraditionalMtuMismatch",
        SetupRejectionCode::MultiConnectionInvalid => "SetupRejectedMultiConnectionInvalid",
        SetupRejectionCode::ConnectionAllocationFailure => {
            "SetupRejectedConnectionAllocationFailure"
        }
    }
}

/// Why the Client's Control Phase failed; a Setup Response's rejection is
/// flattened into one name with its code (`SetupRejectedCapacityExceeded`).
pub fn control_phase_failure(failure: ControlPhaseFailure) -> &'static str {
    match failure {
        ControlPhaseFailure::NoAuthenticationConfigured => "NoAuthenticationConfigured",
        ControlPhaseFailure::NoKeyForSending => "NoKeyForSending",
        ControlPhaseFailure::AmbiguousKeyForSending => "AmbiguousKeyForSending",
        ControlPhaseFailure::UnacceptableCoercion(_) => "UnacceptableCoercion",
        ControlPhaseFailure::UnknownActivationResponse { .. } => "UnknownActivationResponse",
        ControlPhaseFailure::MulticastOrBroadcastServer => "MulticastOrBroadcastServer",
        ControlPhaseFailure::SetupRejected(code) => setup_rejection_code(code),
        ControlPhaseFailure::UnknownSetupResponse { .. } => "UnknownSetupResponse",
        ControlPhaseFailure::TestInitiationTimedOut { .. } => "TestInitiationTimedOut",
        ControlPhaseFailure::UnrunnableParameter { .. } => "UnrunnableParameter",
        ControlPhaseFailure::SocketUnavailable => "SocketUnavailable",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use udpstp_core::Coercion;

    // Each enum's every variant, with a payload for those that have one.
    // Next to each list, a match on the enum that fails to compile when a
    // variant is added, so that the list, and the checked-in names, gain
    // it too.

    const DIRECTIONS: [Direction; 2] = [Direction::Upstream, Direction::Downstream];
    fn _directions(d: Direction) {
        match d {
            Direction::Upstream | Direction::Downstream => {}
        }
    }

    const END_REASONS: [EndReason; 7] = [
        EndReason::Completed,
        EndReason::Unconfirmed,
        EndReason::TestIntervalElapsed,
        EndReason::StoppedByClient,
        EndReason::Stopped,
        EndReason::Watchdog,
        EndReason::OverRate,
    ];
    fn _end_reasons(r: EndReason) {
        match r {
            EndReason::Completed
            | EndReason::Unconfirmed
            | EndReason::TestIntervalElapsed
            | EndReason::StoppedByClient
            | EndReason::Stopped
            | EndReason::Watchdog
            | EndReason::OverRate => {}
        }
    }

    // The list is the core's ActivationParameter::ALL.
    fn _parameters(p: ActivationParameter) {
        use ActivationParameter as P;
        match p {
            P::LowThresh
            | P::UpperThresh
            | P::TrialInt
            | P::TestIntTime
            | P::DscpEcn
            | P::SrIndexConf
            | P::UseOwDelVar
            | P::HighSpeedDelta
            | P::SlowAdjThresh
            | P::SeqErrThresh
            | P::IgnoreOooDup
            | P::SrIndexIsStart
            | P::RandomPayload
            | P::RateAdjAlgo
            | P::SubIntPeriod => {}
        }
    }

    const AUTHENTICATION_FAILURES: [AuthenticationFailure; 8] = [
        AuthenticationFailure::Required,
        AuthenticationFailure::NotConfigured,
        AuthenticationFailure::ModeInvalid,
        AuthenticationFailure::UnknownKeyId,
        AuthenticationFailure::KeyOutsideLifetime,
        AuthenticationFailure::DigestFailed,
        AuthenticationFailure::TimeOutOfWindow,
        AuthenticationFailure::BudgetExhausted,
    ];
    fn _authentication_failures(f: AuthenticationFailure) {
        use AuthenticationFailure as F;
        match f {
            F::Required
            | F::NotConfigured
            | F::ModeInvalid
            | F::UnknownKeyId
            | F::KeyOutsideLifetime
            | F::DigestFailed
            | F::TimeOutOfWindow
            | F::BudgetExhausted => {}
        }
    }

    const MULTI_CONNECTION_ERRORS: [MultiConnectionError; 6] = [
        MultiConnectionError::IdentZero,
        MultiConnectionError::IndexNotBelowCount,
        MultiConnectionError::IndexInUse,
        MultiConnectionError::CountDiffers,
        MultiConnectionError::AuthModeDiffers,
        MultiConnectionError::KeyIdDiffers,
    ];
    fn _multi_connection_errors(e: MultiConnectionError) {
        use MultiConnectionError as E;
        match e {
            E::IdentZero
            | E::IndexNotBelowCount
            | E::IndexInUse
            | E::CountDiffers
            | E::AuthModeDiffers
            | E::KeyIdDiffers => {}
        }
    }

    /// Without the payload variants, which take every value above.
    const SETUP_REJECTIONS: [SetupRejection; 11] = [
        SetupRejection::TestsPerSource,
        SetupRejection::TestConnectionsPerTest,
        SetupRejection::TestConnections,
        SetupRejection::MaxBandwidthRequired,
        SetupRejection::CapacityExceeded,
        SetupRejection::SocketUnavailable,
        SetupRejection::QuotaExceeded,
        SetupRejection::SourceBudgetExhausted,
        SetupRejection::BadVersion,
        SetupRejection::JumboMismatch,
        SetupRejection::TraditionalMtuMismatch,
    ];
    fn _setup_rejections(r: SetupRejection) {
        use SetupRejection as R;
        match r {
            R::TestsPerSource
            | R::TestConnectionsPerTest
            | R::TestConnections
            | R::MaxBandwidthRequired
            | R::CapacityExceeded
            | R::SocketUnavailable
            | R::QuotaExceeded
            | R::SourceBudgetExhausted
            | R::BadVersion
            | R::JumboMismatch
            | R::TraditionalMtuMismatch
            | R::MultiConnection(_)
            | R::Authentication(_) => {}
        }
    }

    const ACTIVATION_REJECTIONS: [ActivationRejection; 9] = [
        ActivationRejection::InvalidCommand,
        ActivationRejection::MixedDirection,
        ActivationRejection::UnacceptableParameter(ActivationParameter::TestIntTime),
        ActivationRejection::FixedRateNeedsOperatorKey,
        ActivationRejection::TestsPerSource,
        ActivationRejection::TestConnections,
        ActivationRejection::MaxBandwidthDirection,
        ActivationRejection::CapacityExceeded,
        ActivationRejection::QuotaExceeded,
    ];
    fn _activation_rejections(r: ActivationRejection) {
        use ActivationRejection as R;
        match r {
            R::InvalidCommand
            | R::MixedDirection
            | R::UnacceptableParameter(_)
            | R::FixedRateNeedsOperatorKey
            | R::TestsPerSource
            | R::TestConnections
            | R::MaxBandwidthDirection
            | R::CapacityExceeded
            | R::QuotaExceeded => {}
        }
    }

    const SILENT_REJECTION_REASONS: [SilentRejectionReason; 18] = [
        SilentRejectionReason::Malformed,
        SilentRejectionReason::UnauthenticatedNotAllowed,
        SilentRejectionReason::UnsupportedAuthMode,
        SilentRejectionReason::NoKeysConfigured,
        SilentRejectionReason::UnverifiedBudgetExhausted,
        SilentRejectionReason::SourceBudgetExhausted,
        SilentRejectionReason::KeyBudgetExhausted,
        SilentRejectionReason::UnknownKeyId,
        SilentRejectionReason::KeyOutsideLifetime,
        SilentRejectionReason::AuthenticationFailed,
        SilentRejectionReason::AuthTimeOutOfWindow,
        SilentRejectionReason::AuthModeMismatch,
        SilentRejectionReason::KeyIdMismatch,
        SilentRejectionReason::MulticastOrBroadcast,
        SilentRejectionReason::ChecksumFailed,
        SilentRejectionReason::Replayed,
        SilentRejectionReason::ReplayCacheFull,
        SilentRejectionReason::NotFromClient,
    ];
    fn _silent_rejection_reasons(r: SilentRejectionReason) {
        use SilentRejectionReason as R;
        match r {
            R::Malformed
            | R::UnauthenticatedNotAllowed
            | R::UnsupportedAuthMode
            | R::NoKeysConfigured
            | R::UnverifiedBudgetExhausted
            | R::SourceBudgetExhausted
            | R::KeyBudgetExhausted
            | R::UnknownKeyId
            | R::KeyOutsideLifetime
            | R::AuthenticationFailed
            | R::AuthTimeOutOfWindow
            | R::AuthModeMismatch
            | R::KeyIdMismatch
            | R::MulticastOrBroadcast
            | R::ChecksumFailed
            | R::Replayed
            | R::ReplayCacheFull
            | R::NotFromClient => {}
        }
    }

    const SETUP_IGNORE_REASONS: [SetupIgnoreReason; 3] = [
        SetupIgnoreReason::ArrivalAddressUnknown,
        SetupIgnoreReason::DuplicateSetupRequest,
        SetupIgnoreReason::Stopping,
    ];
    fn _setup_ignore_reasons(r: SetupIgnoreReason) {
        use SetupIgnoreReason as R;
        match r {
            R::ArrivalAddressUnknown | R::DuplicateSetupRequest | R::Stopping => {}
        }
    }

    const SETUP_REJECTION_CODES: [SetupRejectionCode; 12] = [
        SetupRejectionCode::BadVersion { expected: 20 },
        SetupRejectionCode::JumboMismatch,
        SetupRejectionCode::AuthNotConfigured,
        SetupRejectionCode::AuthRequired,
        SetupRejectionCode::AuthModeInvalid,
        SetupRejectionCode::AuthFailed,
        SetupRejectionCode::AuthTimeInvalid,
        SetupRejectionCode::MaxBandwidthRequired,
        SetupRejectionCode::CapacityExceeded,
        SetupRejectionCode::TraditionalMtuMismatch,
        SetupRejectionCode::MultiConnectionInvalid,
        SetupRejectionCode::ConnectionAllocationFailure,
    ];
    fn _setup_rejection_codes(c: SetupRejectionCode) {
        use SetupRejectionCode as C;
        match c {
            C::BadVersion { .. }
            | C::JumboMismatch
            | C::AuthNotConfigured
            | C::AuthRequired
            | C::AuthModeInvalid
            | C::AuthFailed
            | C::AuthTimeInvalid
            | C::MaxBandwidthRequired
            | C::CapacityExceeded
            | C::TraditionalMtuMismatch
            | C::MultiConnectionInvalid
            | C::ConnectionAllocationFailure => {}
        }
    }

    const TEST_ABORTS: [TestAbort; 5] = [
        TestAbort::TooFewTestConnections { minimum: 2 },
        TestAbort::ParametersDiffer { mc_index: 1 },
        TestAbort::TestConnectionEnded { mc_index: 1 },
        TestAbort::TestConnectionsTimedOut,
        TestAbort::Interrupted,
    ];
    fn _test_aborts(a: TestAbort) {
        match a {
            TestAbort::TooFewTestConnections { .. }
            | TestAbort::ParametersDiffer { .. }
            | TestAbort::TestConnectionEnded { .. }
            | TestAbort::TestConnectionsTimedOut
            | TestAbort::Interrupted => {}
        }
    }

    /// Without `SetupRejected`, which takes every code above.
    const CONTROL_PHASE_FAILURES: [ControlPhaseFailure; 10] = [
        ControlPhaseFailure::NoAuthenticationConfigured,
        ControlPhaseFailure::NoKeyForSending,
        ControlPhaseFailure::AmbiguousKeyForSending,
        ControlPhaseFailure::UnacceptableCoercion(Coercion {
            parameter: ActivationParameter::TestIntTime,
            requested: 120,
            coerced: 60,
        }),
        ControlPhaseFailure::UnknownActivationResponse { cmd_response: 9 },
        ControlPhaseFailure::MulticastOrBroadcastServer,
        ControlPhaseFailure::UnknownSetupResponse { cmd_response: 99 },
        ControlPhaseFailure::TestInitiationTimedOut {
            exchange: udpstp_core::ControlExchange::Setup,
            last_ignored: None,
        },
        ControlPhaseFailure::UnrunnableParameter {
            parameter: ActivationParameter::TestIntTime,
            value: 1,
        },
        ControlPhaseFailure::SocketUnavailable,
    ];
    fn _control_phase_failures(f: ControlPhaseFailure) {
        use ControlPhaseFailure as F;
        match f {
            F::NoAuthenticationConfigured
            | F::NoKeyForSending
            | F::AmbiguousKeyForSending
            | F::UnacceptableCoercion(_)
            | F::UnknownActivationResponse { .. }
            | F::MulticastOrBroadcastServer
            | F::SetupRejected(_)
            | F::UnknownSetupResponse { .. }
            | F::TestInitiationTimedOut { .. }
            | F::UnrunnableParameter { .. }
            | F::SocketUnavailable => {}
        }
    }

    /// Every name, with the enum and variant it names.
    fn every_name() -> Vec<(&'static str, String, &'static str)> {
        let mut rows: Vec<(&'static str, String, &'static str)> = Vec::new();
        let mut add = |kind: &'static str, variant: String, name: &'static str| {
            rows.push((kind, variant, name));
        };
        for d in DIRECTIONS {
            add("Direction", format!("{d:?}"), direction(d));
        }
        for r in END_REASONS {
            add("EndReason", format!("{r:?}"), end_reason(r));
        }
        for p in ActivationParameter::ALL {
            add("ActivationParameter", format!("{p:?}"), parameter(p));
        }
        let setups = SETUP_REJECTIONS
            .into_iter()
            .chain(MULTI_CONNECTION_ERRORS.map(SetupRejection::MultiConnection))
            .chain(AUTHENTICATION_FAILURES.map(SetupRejection::Authentication));
        for s in setups {
            add("SetupRejection", format!("{s:?}"), setup_rejection(s));
        }
        for a in ACTIVATION_REJECTIONS {
            let name = activation_rejection(a);
            add("ActivationRejection", format!("{a:?}"), name);
        }
        for r in SILENT_REJECTION_REASONS {
            let name = silent_rejection_reason(r);
            add("SilentRejectionReason", format!("{r:?}"), name);
        }
        for r in SETUP_IGNORE_REASONS {
            add(
                "SetupIgnoreReason",
                format!("{r:?}"),
                setup_ignore_reason(r),
            );
        }
        let failures = CONTROL_PHASE_FAILURES
            .into_iter()
            .chain(SETUP_REJECTION_CODES.map(ControlPhaseFailure::SetupRejected));
        for f in failures {
            let name = control_phase_failure(f);
            add("ControlPhaseFailure", format!("{f:?}"), name);
        }
        for a in TEST_ABORTS {
            add("TestAbort", format!("{a:?}"), test_abort(a));
        }
        for (kind, names) in [
            ("LogEvent", &event::ALL[..]),
            ("StopReason", &stop_reason::ALL[..]),
            ("StartupWarning", &startup_warning::ALL[..]),
            ("TestSummaryResult", &summary::ALL[..]),
            ("Outcome", &outcome::ALL[..]),
            ("TestEnd", &test_end::ALL[..]),
            ("WarningCode", &warning::ALL[..]),
            ("DataPhaseEngine", &data_phase_engine::ALL[..]),
            ("FastPath", &fast_path::ALL[..]),
            ("Executor", &executor::ALL[..]),
        ] {
            for name in names {
                add(kind, (*name).to_owned(), name);
            }
        }
        rows
    }

    //  (names come from explicit tables; a checked-in list guards
    // against renames)
    #[test]
    fn every_name_is_in_the_checked_in_list() {
        let rows = every_name();
        let text: String = rows
            .iter()
            .map(|(kind, variant, name)| format!("{kind}\t{variant}\t{name}\n"))
            .collect();
        crate::results::tests::golden("names.txt", &text);
        // A name is a word: letters and digits, or a parameter's RFC name.
        for (kind, variant, name) in &rows {
            assert!(
                !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
                "{kind} {variant}: {name:?}"
            );
        }
        let events: std::collections::BTreeSet<_> = event::ALL.into_iter().collect();
        assert_eq!(events.len(), event::ALL.len(), "an event is listed twice");
        // --refuse-coercion takes each parameter's name, in any case.
        for p in ActivationParameter::ALL {
            assert_eq!(activation_parameter(&parameter(p).to_uppercase()), Ok(p));
        }
    }
}
