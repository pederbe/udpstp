//! Typed events the core emits for the runtime to log or show.

use std::net::SocketAddr;

use crate::wire::activation;
use crate::wire::setup::cmd_response;

use crate::protocol::{ActivationParameter, Coercion, Direction, SocketId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Client: the Server accepted the Setup Request.
    SetupAccepted { test_port: u16 },
    /// Client: a Setup Response failed verification and was ignored.
    SetupResponseIgnored { reason: ResponseIgnoreReason },
    /// Client: the Server's Null Request arrived from the Test Connection's
    /// port and verified (RFC 9946 §6.2.2): that port pair works. It gets
    /// no response.
    NullRequestReceived,
    /// Client: a Null Request failed verification and was ignored.
    NullRequestIgnored { reason: ResponseIgnoreReason },
    /// Client: the Control Phase cannot proceed.
    ControlPhaseFailed(ControlPhaseFailure),
    /// Server: a Setup Request was accepted and a Test Connection opened.
    /// With the Client's IP address, mcIdent identifies its Test.
    TestConnectionAdmitted {
        socket: SocketId,
        client: SocketAddr,
        mc_ident: u16,
        /// The keyId, in an authenticated mode.
        key_id: Option<u8>,
    },
    /// Server: the Activation Watchdog expired, no valid Activation Request
    /// having come within 3 s of the Setup Response (RFC 9946 §6.2.2): the
    /// Server closes the Test Connection, which releases it.
    ActivationWatchdogExpired { socket: SocketId },
    /// Server: the oldest Unactivated Test Connection of the Key Table row
    /// holding the most, Unauthenticated Mode's counting as one, was closed
    /// to make room for a new Setup, the unactivated ones having filled
    /// their share.
    UnactivatedTestConnectionEvicted { socket: SocketId },
    /// Server: Troubleshooting Mode is on, so authentication failures are
    /// answered (docs/spec.md story 76); emitted at start and every minute.
    TroubleshootingModeOn,
    /// Server: a Test Connection ended and its admission was released.
    TestConnectionReleased { socket: SocketId },
    /// Server: an authentic Setup Request was refused with a Setup Response
    /// carrying the reason (RFC 9946 §6.2.1).
    SetupRequestRejected {
        from: SocketAddr,
        reason: SetupRejection,
    },
    /// Client: a Setup Response rejecting the Setup Request failed
    /// verification, so it was ignored, as troubleshooting responses are in
    /// an authenticated mode; the Test initiation timer decides. Its
    /// cmdResponse may still explain a failure.
    UnverifiedSetupRejection {
        cmd_response: u8,
        protocol_version: u16,
        /// Its authDigest is all zero, as a Server that can't or won't sign
        /// sends it.
        unsigned: bool,
    },
    /// Server: a Setup Request was refused without a response: a Silent
    /// Rejection.
    SetupRequestSilentlyRejected {
        from: SocketAddr,
        reason: SilentRejectionReason,
    },
    /// Server: a Setup Request was ignored without a response, though it
    /// isn't refused: it was answered already, or can't be answered yet.
    SetupRequestIgnored {
        from: SocketAddr,
        reason: SetupIgnoreReason,
    },
    /// Client: the Server accepted the Activation Request; the Client is
    /// ready for the Data Phase in this direction, with the parameters the
    /// Activation Response gives.
    TestActivated {
        direction: Direction,
        parameters: TestParameters,
    },
    /// Client: the Server rejected the Activation Request
    /// (CHTA_CRSP_BADPARAM, the only rejection code RFC 9946 defines).
    ActivationRejected,
    /// Server: a Test Connection's Activation Request was accepted, with
    /// the parameters its Activation Response gives.
    TestConnectionActivated {
        socket: SocketId,
        direction: Direction,
        parameters: TestParameters,
    },
    /// Server: the Activation Response changes a parameter of the request
    /// (with CHTA_CRSP_ACKOK).
    ActivationParameterCoerced {
        socket: SocketId,
        coercion: Coercion,
    },
    /// Client: the Server changed a parameter of the Activation Request;
    /// the Test runs with the coerced value.
    ParameterCoerced { coercion: Coercion },
    /// Server: an Activation Request was refused without a response: a
    /// Silent Rejection.
    ActivationRequestSilentlyRejected {
        socket: SocketId,
        reason: SilentRejectionReason,
    },
    /// Client: an Activation Response failed verification and was ignored.
    ActivationResponseIgnored { reason: ResponseIgnoreReason },
    /// Server: an Activation Request was rejected with CHTA_CRSP_BADPARAM.
    ActivationRequestRejected {
        socket: SocketId,
        reason: ActivationRejection,
    },
    /// Client: in Authentication Mode 2, a Setup Response from the Server's
    /// address rejected the mode (AUTHINV) but failed verification: the
    /// Server probably doesn't support mode 2, and mode 1 may work. The
    /// Client doesn't fall back by itself (docs/spec.md stories 17 and 37).
    AuthMode2Unsupported,
    /// Client: the Multi-Connection Coordinator gave up on a Test
    /// Connection that hadn't ended: its Setup went unanswered while the
    /// others went on, or the Test was aborted before its Data Phase.
    TestConnectionAbandoned,
    /// Client: the Multi-Connection Coordinator aborted the Test. Test
    /// Connections in their Data Phase are stopped.
    TestAborted(TestAbort),
    /// Client: a Test Connection's socket reported a Path MTU or ICMP
    /// time-exceeded error during the Control Phase, reported once per kind
    /// (ADR 0006). Time exceeded means the Test's MaxHops is too small
    /// for the path, so its Control PDUs expire too.
    PathError(crate::protocol::TransportError),
}

/// Why the Client's Multi-Connection Coordinator aborted a Test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestAbort {
    /// Fewer Test Connections than the Minimum Test Connections could
    /// still succeed.
    TooFewTestConnections { minimum: u8 },
    /// The Server gave the Test Connection with this mcIndex other
    /// parameters than the first Test Connection activated.
    ParametersDiffer { mc_index: u8 },
    /// The Test Connection with this mcIndex ended the Test: the Server
    /// coerced a parameter it refuses a change of, or one it can't run with.
    TestConnectionEnded { mc_index: u8 },
    /// Two or more Test Connections stopped hearing their peers together: a
    /// path failure.
    TestConnectionsTimedOut,
    /// The Client user interrupted the Test (Ctrl-C, or a signal to stop):
    /// each Test Connection in its Data Phase sends its STOP2 and ends.
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationRejection {
    /// cmdRequest is neither an upstream nor a downstream activation.
    InvalidCommand,
    /// Another Test Connection of the same Test was activated in the other
    /// direction; a Test has exactly one direction.
    MixedDirection,
    /// A parameter has a value the Server can neither accept nor coerce.
    UnacceptableParameter(ActivationParameter),
    /// A Fixed-Rate Test without an Operator Key (RFC 9946 §4.1), refused
    /// with CHTA_CRSP_BADPARAM (§7.2.1).
    FixedRateNeedsOperatorKey,
    /// Another Test from the same source was activated first (RFC 9946
    /// §4.2), refused with CHTA_CRSP_BADPARAM, the only rejection code.
    TestsPerSource,
    /// The Test's admitted Test Connections no longer all fit the global
    /// cap; refused with CHTA_CRSP_BADPARAM, the only rejection code.
    TestConnections,
    /// The Setup Request's CHSR_USDIR_BIT marked the other direction, so
    /// its maxBandwidth was admitted against that direction's cap (RFC
    /// 9946 §6.1); refused with CHTA_CRSP_BADPARAM.
    MaxBandwidthDirection,
    /// The Test's maxBandwidth no longer fits the Admitted Rate Cap:
    /// another Test admitted with it activated first; refused with
    /// CHTA_CRSP_BADPARAM, the only rejection code.
    CapacityExceeded,
    /// The Test is past its Source's or key's Quota: another Test set up
    /// with it activated first; refused with CHTA_CRSP_BADPARAM.
    QuotaExceeded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlPhaseFailure {
    /// No authentication is configured and Unauthenticated Mode is not enabled.
    NoAuthenticationConfigured,
    /// No row of the Client's Key Table has a send lifetime that includes
    /// the current time (RFC 7210 §3), with the pinned keyId if any.
    NoKeyForSending,
    /// Several rows tie as the newest valid key for sending; pin a keyId.
    AmbiguousKeyForSending,
    /// The Server coerced a parameter the Client was configured not to
    /// accept a change of, so the Client ended the Test.
    UnacceptableCoercion(Coercion),
    /// The Activation Response carried a cmdResponse RFC 9946 §12.3.9 does
    /// not define, so the Client terminated the Test Connection (§7.3).
    UnknownActivationResponse { cmd_response: u8 },
    /// The Server's address is a multicast or broadcast address, which a
    /// Test Setup Request must not go to (RFC 9946 §6).
    MulticastOrBroadcastServer,
    /// The Server refused the Setup Request with a verified Setup Response
    /// (RFC 9946 §6.3, §12.3.5).
    SetupRejected(SetupRejectionCode),
    /// A verified Setup Response carried a cmdResponse RFC 9946 §12.3.5
    /// doesn't define, which ends the Setup attempt.
    UnknownSetupResponse { cmd_response: u8 },
    /// The Test initiation timer expired: the Setup and Activation
    /// exchanges didn't complete within 3 s (RFC 9946 §6.1). `exchange`
    /// is the one it expired in: the Activation exchange once a Setup
    /// Response was accepted, whether or not the Null Request came, since
    /// nothing waits for it. `last_ignored` is why the last response of
    /// that exchange the Client ignored was, if it ignored one: a Setup
    /// Response ignored no longer explains the timer once a Setup Response
    /// was accepted.
    TestInitiationTimedOut {
        exchange: ControlExchange,
        last_ignored: Option<ResponseIgnoreReason>,
    },
    /// The Activation Response set a parameter to a value the Client can't
    /// run with (outside its range, thresholds out of order, an unknown
    /// algorithm), so the Client ended the Test (§7.3).
    UnrunnableParameter {
        parameter: ActivationParameter,
        value: u16,
    },
    /// The Test Connection's socket couldn't be opened, or failed before
    /// the Data Phase.
    SocketUnavailable,
}

/// One of the Control Phase's two exchanges (not the STOP exchange, which
/// ends a Data Phase).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlExchange {
    /// The Setup exchange: the Setup Request, the Setup Response and, from
    /// the Ephemeral Port, the Null Request (RFC 9946 §6).
    Setup,
    /// The Activation exchange: the Activation Request and the Activation
    /// Response (RFC 9946 §7).
    Activation,
}

/// A rejection cmdResponse of a Setup Response (RFC 9946 §12.3.5), as the
/// Client reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupRejectionCode {
    /// CHSR_CRSP_BADVER, with the protocolVer the Server expects.
    BadVersion { expected: u16 },
    /// CHSR_CRSP_BADJS: the Jumbo setting differs.
    JumboMismatch,
    /// CHSR_CRSP_AUTHNC: the Server has no authentication configured.
    AuthNotConfigured,
    /// CHSR_CRSP_AUTHREQ: the Server requires authentication.
    AuthRequired,
    /// CHSR_CRSP_AUTHINV: the Server doesn't run this authMode.
    AuthModeInvalid,
    /// CHSR_CRSP_AUTHFAIL: the key didn't verify.
    AuthFailed,
    /// CHSR_CRSP_AUTHTIME: authUnixTime was outside the Server's window.
    AuthTimeInvalid,
    /// CHSR_CRSP_NOMAXBW: the Server requires a maxBandwidth.
    MaxBandwidthRequired,
    /// CHSR_CRSP_CAPEXC: the Server lacks room for the maxBandwidth.
    CapacityExceeded,
    /// CHSR_CRSP_BADTMTU: the Traditional MTU setting differs.
    TraditionalMtuMismatch,
    /// CHSR_CRSP_MCINVPAR: the Multi-Connection fields are invalid.
    MultiConnectionInvalid,
    /// CHSR_CRSP_CONNFAIL: the Server couldn't allocate the Test
    /// Connection.
    ConnectionAllocationFailure,
}

impl SetupRejectionCode {
    /// The code a Setup Response carries, or `None` for one RFC 9946
    /// §12.3.5 doesn't define as a rejection.
    pub fn from_response(cmd_response: u8, protocol_version: u16) -> Option<Self> {
        Some(match cmd_response {
            cmd_response::BAD_VERSION => Self::BadVersion {
                expected: protocol_version,
            },
            cmd_response::JUMBO_MISMATCH => Self::JumboMismatch,
            cmd_response::AUTH_NOT_CONFIGURED => Self::AuthNotConfigured,
            cmd_response::AUTH_REQUIRED => Self::AuthRequired,
            cmd_response::AUTH_MODE_INVALID => Self::AuthModeInvalid,
            cmd_response::AUTH_FAILURE => Self::AuthFailed,
            cmd_response::AUTH_TIME_INVALID => Self::AuthTimeInvalid,
            cmd_response::MAX_BANDWIDTH_REQUIRED => Self::MaxBandwidthRequired,
            cmd_response::CAPACITY_EXCEEDED => Self::CapacityExceeded,
            cmd_response::TRADITIONAL_MTU_MISMATCH => Self::TraditionalMtuMismatch,
            cmd_response::MULTI_CONNECTION_INVALID => Self::MultiConnectionInvalid,
            cmd_response::CONNECTION_ALLOCATION_FAILURE => Self::ConnectionAllocationFailure,
            _ => return None,
        })
    }

    /// The cmdResponse value.
    pub fn cmd_response(self) -> u8 {
        match self {
            Self::BadVersion { .. } => cmd_response::BAD_VERSION,
            Self::JumboMismatch => cmd_response::JUMBO_MISMATCH,
            Self::AuthNotConfigured => cmd_response::AUTH_NOT_CONFIGURED,
            Self::AuthRequired => cmd_response::AUTH_REQUIRED,
            Self::AuthModeInvalid => cmd_response::AUTH_MODE_INVALID,
            Self::AuthFailed => cmd_response::AUTH_FAILURE,
            Self::AuthTimeInvalid => cmd_response::AUTH_TIME_INVALID,
            Self::MaxBandwidthRequired => cmd_response::MAX_BANDWIDTH_REQUIRED,
            Self::CapacityExceeded => cmd_response::CAPACITY_EXCEEDED,
            Self::TraditionalMtuMismatch => cmd_response::TRADITIONAL_MTU_MISMATCH,
            Self::MultiConnectionInvalid => cmd_response::MULTI_CONNECTION_INVALID,
            Self::ConnectionAllocationFailure => cmd_response::CONNECTION_ALLOCATION_FAILURE,
        }
    }
}

/// Why the Server refused a Setup Request with a Setup Response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupRejection {
    /// The source already runs as many Tests as the Server allows at once
    /// (CHSR_CRSP_CONNFAIL).
    TestsPerSource,
    /// The Test already has as many Test Connections as the Server allows
    /// one Test (CHSR_CRSP_CONNFAIL).
    TestConnectionsPerTest,
    /// The Server has admitted as many Test Connections as it allows at
    /// once (CHSR_CRSP_CONNFAIL).
    TestConnections,
    /// The Server requires a maxBandwidth and none was stated
    /// (CHSR_CRSP_NOMAXBW).
    MaxBandwidthRequired,
    /// The stated maxBandwidth would take the maxBandwidth admitted in
    /// its direction over the Server's cap (CHSR_CRSP_CAPEXC).
    CapacityExceeded,
    /// The Test Connection's socket couldn't be opened
    /// (CHSR_CRSP_CONNFAIL).
    SocketUnavailable,
    /// A new Test past its Source's or key's Quota of Tests or bytes in
    /// the Quota Period (RFC 9946 §11 item 6; CHSR_CRSP_CONNFAIL).
    QuotaExceeded,
    /// The Source has spent its budget of authentic Setup Requests
    /// (CHSR_CRSP_CONNFAIL).
    SourceBudgetExhausted,
    /// The Multi-Connection fields are invalid, or disagree with the
    /// Test's other Test Connections (CHSR_CRSP_MCINVPAR).
    MultiConnection(MultiConnectionError),
    /// Another protocolVer than this Server's (CHSR_CRSP_BADVER); the
    /// response carries the expected one.
    BadVersion,
    /// CHSR_JUMBO_STATUS doesn't match this Server's setting
    /// (CHSR_CRSP_BADJS).
    JumboMismatch,
    /// CHSR_TRADITIONAL_MTU doesn't match this Server's setting
    /// (CHSR_CRSP_BADTMTU).
    TraditionalMtuMismatch,
    /// Troubleshooting mode: an authentication failure, answered unsigned,
    /// or signed for AUTHTIME, whose digest is valid.
    Authentication(AuthenticationFailure),
}

/// Why a Setup Request failed authentication (RFC 9946 §6.2 steps 3-5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthenticationFailure {
    /// Unauthenticated, but this Server hasn't opted in (AUTHREQ).
    Required,
    /// Authenticated, but this Server has no keys (AUTHNC).
    NotConfigured,
    /// An authMode this Server doesn't run (AUTHINV).
    ModeInvalid,
    /// No Key Table row has the keyId (AUTHFAIL).
    UnknownKeyId,
    /// The keyId's rows are all outside their accept lifetimes (AUTHFAIL).
    KeyOutsideLifetime,
    /// The authDigest didn't verify (AUTHFAIL).
    DigestFailed,
    /// authUnixTime is more than 5 s from this Server's clock (AUTHTIME).
    TimeOutOfWindow,
    /// The global budget for unverified Setup Requests is spent this
    /// second; always silent.
    BudgetExhausted,
}

impl AuthenticationFailure {
    /// Troubleshooting mode's Setup Response cmdResponse (RFC 9946
    /// §12.3.5).
    pub fn cmd_response(self) -> u8 {
        match self {
            Self::Required => cmd_response::AUTH_REQUIRED,
            Self::NotConfigured => cmd_response::AUTH_NOT_CONFIGURED,
            Self::ModeInvalid => cmd_response::AUTH_MODE_INVALID,
            Self::UnknownKeyId | Self::KeyOutsideLifetime | Self::DigestFailed => {
                cmd_response::AUTH_FAILURE
            }
            Self::TimeOutOfWindow => cmd_response::AUTH_TIME_INVALID,
            // Never answered; AUTHFAIL for completeness.
            Self::BudgetExhausted => cmd_response::AUTH_FAILURE,
        }
    }
}

impl From<AuthenticationFailure> for SilentRejectionReason {
    fn from(failure: AuthenticationFailure) -> Self {
        match failure {
            AuthenticationFailure::Required => Self::UnauthenticatedNotAllowed,
            AuthenticationFailure::NotConfigured => Self::NoKeysConfigured,
            AuthenticationFailure::ModeInvalid => Self::UnsupportedAuthMode,
            AuthenticationFailure::UnknownKeyId => Self::UnknownKeyId,
            AuthenticationFailure::KeyOutsideLifetime => Self::KeyOutsideLifetime,
            AuthenticationFailure::DigestFailed => Self::AuthenticationFailed,
            AuthenticationFailure::TimeOutOfWindow => Self::AuthTimeOutOfWindow,
            AuthenticationFailure::BudgetExhausted => Self::UnverifiedBudgetExhausted,
        }
    }
}

/// What is wrong with a Setup Request's Multi-Connection fields (RFC 9946
/// §6.1; docs/spec.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MultiConnectionError {
    /// mcIdent is zero; it must be non-zero.
    IdentZero,
    /// mcIndex isn't below mcCount (or mcCount is zero).
    IndexNotBelowCount,
    /// Another Test Connection of the Test has this mcIndex.
    IndexInUse,
    /// The Test's other Test Connections stated another mcCount.
    CountDiffers,
    /// The Test's other Test Connections use another authMode.
    AuthModeDiffers,
    /// The Test's other Test Connections use another keyId, in an
    /// authenticated mode.
    KeyIdDiffers,
}

impl SetupRejection {
    /// The Setup Response's cmdResponse (RFC 9946 §12.3.5).
    pub fn cmd_response(self) -> u8 {
        match self {
            Self::TestsPerSource
            | Self::TestConnectionsPerTest
            | Self::TestConnections
            | Self::SocketUnavailable
            | Self::QuotaExceeded
            | Self::SourceBudgetExhausted => cmd_response::CONNECTION_ALLOCATION_FAILURE,
            Self::MaxBandwidthRequired => cmd_response::MAX_BANDWIDTH_REQUIRED,
            Self::MultiConnection(_) => cmd_response::MULTI_CONNECTION_INVALID,
            Self::BadVersion => cmd_response::BAD_VERSION,
            Self::JumboMismatch => cmd_response::JUMBO_MISMATCH,
            Self::TraditionalMtuMismatch => cmd_response::TRADITIONAL_MTU_MISMATCH,
            Self::Authentication(failure) => failure.cmd_response(),
            Self::CapacityExceeded => cmd_response::CAPACITY_EXCEEDED,
        }
    }
}

/// Why the Server refused a Setup or Activation Request without a
/// response: a Silent Rejection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SilentRejectionReason {
    /// Not a well-formed request of the expected kind (size or PDU ID).
    Malformed,
    /// An unauthenticated request, but this Server has not opted in.
    UnauthenticatedNotAllowed,
    /// An Authentication Mode this Server does not support yet.
    UnsupportedAuthMode,
    /// An authenticated request, but this Server has no keys.
    NoKeysConfigured,
    /// More Setup Requests this second than the global budget for
    /// unverified traffic allows (docs/spec.md story 85).
    UnverifiedBudgetExhausted,
    /// An authentic request from a Source whose budget is spent, refused
    /// before: only the first refusal is answered.
    SourceBudgetExhausted,
    /// An authentic request, or one in Unauthenticated Mode, over its Key
    /// Table row's budget; never answered.
    KeyBudgetExhausted,
    /// No Shared Key in the Key Table for the request's keyId.
    UnknownKeyId,
    /// The keyId's rows are all outside their accept lifetimes now; treated
    /// as an authentication failure (RFC 7210 §3, ADR 0010).
    KeyOutsideLifetime,
    /// The authDigest did not verify with the Client Authentication Key.
    AuthenticationFailed,
    /// authUnixTime is more than 5 s from this host's wall clock.
    AuthTimeOutOfWindow,
    /// An Activation Request's authMode differs from its Setup's.
    AuthModeMismatch,
    /// An Activation Request's keyId differs from its Setup's.
    KeyIdMismatch,
    /// A Setup Request from or to a multicast or broadcast address (RFC
    /// 9946 §6), or an Activation Request from one.
    MulticastOrBroadcast,
    /// Checksum use is configured and the populated checkSum didn't verify
    /// (RFC 9946 §5.6).
    ChecksumFailed,
    /// A verified request with the keyId and authDigest of one already
    /// seen while it could pass the time window: a replay (docs/spec.md story
    /// 84).
    Replayed,
    /// The replay cache is full, so a verified request can't be checked
    /// and is refused.
    ReplayCacheFull,
    /// An Activation Request from another IP address than its Setup
    /// Request's Client: a Test Connection is locked to its Client.
    NotFromClient,
}

/// Why the Server ignored a Setup Request it doesn't refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupIgnoreReason {
    /// Its arrival address the runtime couldn't tell: the reply couldn't
    /// leave from it (ADR 0007). The Client retries.
    ArrivalAddressUnknown,
    /// It came from the same Client address, with the same mcIdent and
    /// mcIndex, as an admitted Test Connection: a duplicate, already
    /// answered.
    DuplicateSetupRequest,
    /// The Server is stopping: it admits no more Test Connections
    /// ([`crate::protocol::Server::handle_shutdown`]).
    Stopping,
}

/// Why the Client ignored a response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseIgnoreReason {
    /// The authDigest did not verify with the Server Authentication Key.
    AuthenticationFailed,
    /// authUnixTime is more than 5 s from this host's wall clock.
    AuthTimeOutOfWindow,
    /// A Setup Response from or to a multicast or broadcast address (RFC
    /// 9946 §6).
    MulticastOrBroadcast,
    /// Checksum use is configured and the populated checkSum didn't verify
    /// (RFC 9946 §5.6).
    ChecksumFailed,
}

/// The Test parameters an Activation Response gives (RFC 9946 §7.1), as the
/// Client runs with them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TestParameters {
    /// testIntTime.
    pub test_interval: std::time::Duration,
    /// subIntPeriod.
    pub sub_interval: std::time::Duration,
    /// trialInt: also st, the IP-Layer Sender Bit Rate's interval.
    pub trial_interval: std::time::Duration,
    /// lowThresh and upperThresh, in ms.
    pub low_thresh: u16,
    pub upper_thresh: u16,
    pub seq_err_thresh: u16,
    pub slow_adj_thresh: u16,
    pub high_speed_delta: u8,
    /// ignoreOooDup.
    pub ignore_ooo_dup: bool,
    /// useOwDelVar: one-way delay variation rather than RTT variation.
    pub one_way_delay_variation: bool,
    /// CHTA_RAND_PAYLOAD.
    pub random_payload: bool,
    /// rateAdjAlgo: 0 for B, 1 for C.
    pub rate_adj_algo: u8,
    /// The DSCP of dscpEcn.
    pub dscp: u8,
    /// srIndexConf, and whether it is a start row (CHTA_SRIDX_ISSTART);
    /// CHTA_SRIDX_DEF asks for the Server's default.
    pub sr_index_conf: u16,
    pub sr_index_is_start: bool,
}

impl TestParameters {
    /// Whether a Test Connection activated with `other` runs the same Test
    /// as one activated with these: every parameter the same but the
    /// Sending Rate Table row, which each Server picks from its own table
    /// and limits by each Test Connection's maxBandwidth. Whether the row
    /// starts a search or is held must still agree.
    pub fn same_test(&self, other: &Self) -> bool {
        Self {
            sr_index_conf: other.sr_index_conf,
            ..*self
        } == *other
    }

    /// The parameters an accepting Activation Response gives.
    pub(crate) fn of(response: &activation::ActivationPdu) -> Self {
        Self {
            test_interval: std::time::Duration::from_secs(response.test_int_time.into()),
            sub_interval: std::time::Duration::from_millis(response.sub_int_period.into()),
            trial_interval: std::time::Duration::from_millis(response.trial_int.into()),
            low_thresh: response.low_thresh,
            upper_thresh: response.upper_thresh,
            seq_err_thresh: response.seq_err_thresh,
            slow_adj_thresh: response.slow_adj_thresh,
            high_speed_delta: response.high_speed_delta,
            ignore_ooo_dup: response.ignore_ooo_dup != 0,
            one_way_delay_variation: response.use_ow_del_var != 0,
            random_payload: response.modifier_bitmap & activation::modifier::RANDOM_PAYLOAD != 0,
            rate_adj_algo: response.rate_adj_algo,
            dscp: response.dscp_ecn >> 2,
            sr_index_conf: response.sr_index_conf,
            sr_index_is_start: response.modifier_bitmap & activation::modifier::SR_INDEX_IS_START
                != 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // docs/spec.md story 58 (coerced parameters checked for consistency
    // across Test Connections);
    #[test]
    fn the_same_test_but_for_the_row() {
        let first = TestParameters {
            test_interval: std::time::Duration::from_secs(10),
            sub_interval: std::time::Duration::from_secs(1),
            trial_interval: std::time::Duration::from_millis(50),
            low_thresh: 30,
            upper_thresh: 90,
            seq_err_thresh: 10,
            slow_adj_thresh: 3,
            high_speed_delta: 10,
            ignore_ooo_dup: true,
            one_way_delay_variation: true,
            random_payload: false,
            rate_adj_algo: 0,
            dscp: 0,
            sr_index_conf: 5,
            sr_index_is_start: true,
        };
        assert!(first.same_test(&TestParameters {
            sr_index_conf: 3,
            ..first
        }));
        assert!(!first.same_test(&TestParameters {
            sr_index_is_start: false,
            ..first
        }));
        assert!(!first.same_test(&TestParameters { dscp: 46, ..first }));
        assert!(!first.same_test(&TestParameters {
            sub_interval: std::time::Duration::from_millis(500),
            ..first
        }));
    }
}
