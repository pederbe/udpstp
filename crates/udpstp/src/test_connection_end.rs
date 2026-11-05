//! How a Test Connection ended, said once (the architecture review of
//! 2026-09-30). From its outcome this module answers everything the
//! Client reports of one Test Connection's end: its ErrorStatus and
//! whether it completed, its names in the JSON results, how its Data Phase
//! ended, and what is said of it, alone or, named by its mcIndex, as one of
//! a Multi-Connection Test's. Its ErrorStatus and its words both follow one
//! private account of why it ended ([`Cause`]), each by an exhaustive match
//! with no catch-all, so a new way to end can't get a status without words
//! or words without a status. `exit.rs` combines a Test's statuses as
//! OB-UDPST does; `test_results.rs`, `json.rs` and `results.rs` ask
//! this module of each Test Connection.

use std::net::SocketAddr;

use udpstp_core::{
    ActivationParameter, Coercion, ControlExchange, ControlPhaseFailure, EndReason, Event,
    ResponseIgnoreReason, SetupRejectionCode,
};
use udpstp_runtime::{ClientOutcome, ClientResult};

use crate::exit::{self, Status};
use crate::names;
use crate::test_results::ResultOptions;
use crate::warnings::DataPhaseEnd;

/// How one Test Connection ended, as the Client reports it.
#[derive(Debug, Clone)]
pub struct TestConnectionEnd {
    server: SocketAddr,
    /// The runtime's account of how it ended, which its JSON names, its
    /// description and its Data Phase's end render.
    result: ClientResult,
    /// Why it ended, told apart once: its ErrorStatus and what is said of
    /// it both follow.
    cause: Cause,
    /// The most severe warning its Data Phase had, as OB-UDPST ranks them.
    warning: Option<u8>,
}

/// Why a Test Connection ended, as far as its ErrorStatus and what is said
/// of it tell endings apart.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Cause {
    /// Its Data Phase ended as one that completes does
    /// (`EndReason::completes`): with the STOP exchange, or by the Client's
    /// own Test Interval timer.
    Completed(EndReason),
    /// The Watchdog ended its Data Phase.
    Watchdog,
    /// The Multi-Connection Coordinator stopped its Data Phase.
    Stopped,
    /// Its Data Phase ended early otherwise, for this reason: a Server's
    /// own, which a Client's Data Phase doesn't report.
    EndedEarly(EndReason),
    /// Its Data Phase Engine failed, for this reason.
    EngineFailed(String),
    /// The Server rejected its Activation Request (CHTA_CRSP_BADPARAM).
    ActivationRejected,
    /// The Multi-Connection Coordinator gave it up before its Data Phase.
    Abandoned,
    /// A local socket error ended it.
    SocketError(String),
    /// The Client's settings can't run it.
    Configuration(Configuration),
    /// A verified Setup Response refused it.
    SetupRejected(SetupRejectionCode),
    /// A verified Setup Response carried this cmdResponse, which RFC 9946
    /// doesn't define.
    UnknownSetupResponse(u8),
    /// An Activation Response carried this cmdResponse, which RFC 9946
    /// doesn't define.
    UnknownActivationResponse(u8),
    /// Its Test initiation timer expired in `exchange` (RFC 9946 §6.1), for
    /// `why`, as the core reports it. A troubleshooting Server may have
    /// refused it in a Setup Response that couldn't be verified, which
    /// `unverified` says why; `mode_2_unsupported` when it refused mode 2
    /// so.
    TimedOut {
        exchange: ControlExchange,
        why: Unanswered,
        unverified: Option<SetupRejectionCode>,
        mode_2_unsupported: bool,
    },
    /// The Server coerced a parameter the Client was told to refuse.
    UnacceptableCoercion(Coercion),
    /// The Server set a parameter to a value the Client can't run with.
    UnrunnableParameter {
        parameter: ActivationParameter,
        value: u16,
    },
    /// Its socket couldn't be opened, or failed before its Data Phase.
    SocketUnavailable,
}

/// Which of the Client's settings can't run a Test Connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Configuration {
    /// No authentication, and not Unauthenticated Mode either.
    NoAuthentication,
    /// No key may be sent with now, or none with the keyId given.
    NoKey,
    /// Several keys may be sent with now.
    AmbiguousKey,
    /// Its Server's address is a multicast or broadcast address.
    MulticastServer,
}

/// Why a Test initiation timer expired: the last response ignored in the
/// exchange it expired in, if one was (RFC 9946 §6.1), as the core
/// reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unanswered {
    /// None was ignored there: no response came.
    NoAnswer,
    /// It failed verification.
    Unverified,
    /// Its authUnixTime was more than 5 s from this host's clock.
    Late,
    /// It was a Setup Response to a broadcast or multicast address.
    Broadcast,
    /// It failed its header checksum.
    Corrupted,
}

impl Cause {
    /// Why `outcome`'s Test Connection ended.
    fn of(outcome: &ClientOutcome) -> Self {
        use ControlPhaseFailure as F;
        match &outcome.result {
            ClientResult::Finished {
                reason: Some(reason),
                ..
            } => match reason {
                reason if reason.completes() => Self::Completed(*reason),
                EndReason::Watchdog => Self::Watchdog,
                EndReason::Stopped => Self::Stopped,
                reason => Self::EndedEarly(*reason),
            },
            ClientResult::Finished {
                reason: None,
                error,
                ..
            } => Self::EngineFailed(error.clone().unwrap_or_default()),
            ClientResult::ActivationRejected => Self::ActivationRejected,
            ClientResult::Abandoned => Self::Abandoned,
            ClientResult::SocketError(error) => Self::SocketError(error.clone()),
            ClientResult::Failed(failure) => match *failure {
                F::NoAuthenticationConfigured => {
                    Self::Configuration(Configuration::NoAuthentication)
                }
                F::NoKeyForSending => Self::Configuration(Configuration::NoKey),
                F::AmbiguousKeyForSending => Self::Configuration(Configuration::AmbiguousKey),
                F::MulticastOrBroadcastServer => {
                    Self::Configuration(Configuration::MulticastServer)
                }
                F::SetupRejected(code) => Self::SetupRejected(code),
                F::UnknownSetupResponse { cmd_response } => {
                    Self::UnknownSetupResponse(cmd_response)
                }
                F::UnknownActivationResponse { cmd_response } => {
                    Self::UnknownActivationResponse(cmd_response)
                }
                F::TestInitiationTimedOut {
                    exchange,
                    last_ignored,
                } => Self::TimedOut {
                    exchange,
                    why: Unanswered::of(last_ignored),
                    // A troubleshooting Server's unsigned rejection can't be
                    // verified, but may say why.
                    unverified: outcome.events.iter().find_map(|e| match e {
                        Event::UnverifiedSetupRejection {
                            cmd_response,
                            protocol_version,
                            ..
                        } => SetupRejectionCode::from_response(*cmd_response, *protocol_version),
                        _ => None,
                    }),
                    mode_2_unsupported: outcome.events.contains(&Event::AuthMode2Unsupported),
                },
                F::UnacceptableCoercion(coercion) => Self::UnacceptableCoercion(coercion),
                F::UnrunnableParameter { parameter, value } => {
                    Self::UnrunnableParameter { parameter, value }
                }
                F::SocketUnavailable => Self::SocketUnavailable,
            },
        }
    }
}

impl Unanswered {
    /// Why a Test initiation timer expired, from why the last response
    /// ignored in its exchange was, if one was: `last_ignored`.
    fn of(last_ignored: Option<ResponseIgnoreReason>) -> Self {
        match last_ignored {
            None => Self::NoAnswer,
            Some(ResponseIgnoreReason::AuthenticationFailed) => Self::Unverified,
            Some(ResponseIgnoreReason::AuthTimeOutOfWindow) => Self::Late,
            Some(ResponseIgnoreReason::MulticastOrBroadcast) => Self::Broadcast,
            Some(ResponseIgnoreReason::ChecksumFailed) => Self::Corrupted,
        }
    }

    /// The ErrorStatus of a Test initiation timer that expired in
    /// `exchange` for this: no usable answer in time. A response that
    /// failed verification says why, at that exchange, as OB-UDPST reports;
    /// otherwise the Server didn't answer, which ends as 200 as in
    /// OB-UDPST.
    fn status(self, exchange: ControlExchange) -> u8 {
        let base = match exchange {
            ControlExchange::Setup => exit::SETUP_ERROR_BASE,
            ControlExchange::Activation => exit::ACTIVATION_ERROR_BASE,
        };
        match self {
            Self::Unverified => base + exit::AUTH_FAILURE,
            Self::Late => base + exit::AUTH_TIME,
            Self::NoAnswer | Self::Broadcast | Self::Corrupted => exit::CONNECTIONS_UNAVAILABLE,
        }
    }
}

impl TestConnectionEnd {
    /// How `outcome`'s Test Connection ended.
    pub fn of(outcome: &ClientOutcome) -> Self {
        Self {
            server: outcome.server,
            result: outcome.result.clone(),
            cause: Cause::of(outcome),
            warning: exit::warning(&outcome.data_events),
        }
    }

    /// Its ErrorStatus, and whether it completed (the table in `exit.rs`).
    /// (conformance: exit-own-error-statuses, exit-most-severe-warning)
    pub fn status(&self) -> Status {
        let failed = Status::failed;
        match &self.cause {
            // Completed, with the STOP exchange or when the Client's own
            // Test Interval timer ended it without STOP2 (as OB-UDPST): the
            // most severe warning, if any.
            Cause::Completed(_) => Status {
                error_status: self.warning.unwrap_or(exit::SUCCESS),
                completed: true,
            },
            // Ended early: the Test didn't complete.
            Cause::Watchdog | Cause::Abandoned => failed(exit::CONNECTIONS_UNAVAILABLE),
            Cause::Stopped
            | Cause::EndedEarly(_)
            | Cause::EngineFailed(_)
            | Cause::SocketError(_) => failed(exit::GENERIC_ERROR),
            Cause::ActivationRejected => failed(exit::ACTIVATION_ERROR_BASE + exit::BAD_PARAMETER),
            Cause::Configuration(_) => failed(exit::CONFIGURATION_ERROR),
            Cause::SetupRejected(code) => failed(exit::setup_rejected(code.cmd_response())),
            Cause::UnknownSetupResponse(_) => failed(exit::SETUP_ERROR_BASE),
            Cause::UnknownActivationResponse(_) => failed(exit::ACTIVATION_ERROR_BASE),
            Cause::TimedOut { exchange, why, .. } => failed(why.status(*exchange)),
            Cause::UnacceptableCoercion(_) => failed(exit::COERCION_REFUSED),
            Cause::UnrunnableParameter { .. } => failed(exit::UNRUNNABLE_PARAMETER),
            Cause::SocketUnavailable => failed(exit::INITIALIZATION_ERROR),
        }
    }

    /// Whether it completed: its Data Phase ended as one that completes
    /// does (`EndReason::completes`), as its status says.
    pub fn completed(&self) -> bool {
        self.status().completed
    }

    /// The most severe warning its Data Phase had, if any, whether or not
    /// it completed: a Multi-Connection Test that completed reports the
    /// most severe of all its Test Connections'.
    pub fn warning(&self) -> Option<u8> {
        self.warning
    }

    /// Whether the Multi-Connection Coordinator ended it for the Test's
    /// sake rather than for a failure of its own: stopped its Data Phase,
    /// or gave it up before one.
    pub fn ended_by_the_coordinator(&self) -> bool {
        matches!(self.cause, Cause::Stopped | Cause::Abandoned)
    }

    /// Whether its Data Phase Engine ran until its Data Phase ended,
    /// however it ended, rather than failing.
    pub fn data_phase_ended(&self) -> bool {
        matches!(
            self.cause,
            Cause::Completed(_) | Cause::Watchdog | Cause::Stopped | Cause::EndedEarly(_)
        )
    }

    /// How its Data Phase ended, if it ran one, for the warnings; named by
    /// its mcIndex `test_connection` in a Multi-Connection Test.
    pub fn data_phase_end(&self, test_connection: Option<u8>) -> Option<DataPhaseEnd<'_>> {
        match &self.result {
            ClientResult::Finished { reason, error, .. } => Some(DataPhaseEnd {
                test_connection,
                reason: *reason,
                failure: error.as_deref(),
            }),
            _ => None,
        }
    }

    /// How it ended, by name, as the JSON's `EndReason` gives it: an
    /// [`EndReason`]'s, or how its Control Phase or Data Phase Engine ended
    /// it.
    pub fn end_name(&self) -> &'static str {
        match &self.result {
            ClientResult::Finished {
                reason: Some(reason),
                ..
            } => names::end_reason(*reason),
            ClientResult::Finished { reason: None, .. } => names::outcome::DATA_PHASE_ENGINE_FAILED,
            ClientResult::ActivationRejected => names::outcome::ACTIVATION_REJECTED,
            ClientResult::Failed(_) => names::outcome::CONTROL_PHASE_FAILED,
            ClientResult::Abandoned => names::outcome::ABANDONED,
            ClientResult::SocketError(_) => names::outcome::SOCKET_ERROR,
        }
    }

    /// Why its Control Phase failed, if it did, by name, as the JSON's
    /// `ControlPhaseFailure` gives it.
    pub fn control_phase_failure_name(&self) -> Option<&'static str> {
        match &self.result {
            ClientResult::Failed(failure) => Some(names::control_phase_failure(*failure)),
            _ => None,
        }
    }

    /// How it ended, in a few words.
    pub fn description(&self) -> String {
        match &self.result {
            ClientResult::Finished {
                reason: Some(reason),
                ..
            } => names::end_reason(*reason).into(),
            ClientResult::Finished { error, .. } => {
                format!(
                    "the Data Phase failed: {}",
                    error.clone().unwrap_or_default()
                )
            }
            ClientResult::ActivationRejected => "the Activation Request was rejected".into(),
            ClientResult::Failed(failure) => names::control_phase_failure(*failure).into(),
            ClientResult::Abandoned => "abandoned".into(),
            ClientResult::SocketError(error) => format!("socket error: {error}"),
        }
    }

    /// What is said of how it ended, line by line: in a Test of this one
    /// Test Connection, or among a Multi-Connection Test's, naming it by its
    /// mcIndex `test_connection`. Nothing for one that completed, which the
    /// Test says; nor for one stopped because the Client user `interrupted`
    /// the Test, nor, among several, for one the Multi-Connection
    /// Coordinator stopped or gave up, which the Test's own notice
    /// explains. What a timed-out one says follows the cause its ErrorStatus
    /// comes from, as the core reports it: the exchange the Test initiation
    /// timer expired in, and the last response ignored there.
    pub fn said(
        &self,
        options: &ResultOptions,
        test_connection: Option<u8>,
        interrupted: bool,
    ) -> Vec<Line> {
        if test_connection.is_some() && self.ended_by_the_coordinator() {
            return Vec::new();
        }
        let server = self.server;
        let time_limit = options.time_limit;
        // A line about this Test Connection alone, which a Multi-Connection
        // Test says with its mcIndex; one about its Server with both.
        let about_it = |text: String| {
            own(match test_connection {
                None => text,
                Some(i) => format!("Test Connection {i}: {text}"),
            })
        };
        let at_server = |text: String| {
            own(match test_connection {
                None => format!("{server}: {text}"),
                Some(i) => format!("Test Connection {i} to {server}: {text}"),
            })
        };
        // A Data Phase that ended early for `reason`.
        let without_stop = |reason: EndReason| {
            let reason = names::end_reason(reason);
            own(match test_connection {
                None => format!("warning: the Test ended without the Server's STOP: {reason}"),
                Some(i) => {
                    format!(
                        "warning: Test Connection {i} ended without the Server's STOP: {reason}"
                    )
                }
            })
        };
        let mut said = Vec::new();
        match &self.cause {
            Cause::Completed(_) => {}
            Cause::Watchdog => said.push(own(match test_connection {
                None => "warning: the Watchdog ended the Test: nothing received for 3 s".into(),
                Some(i) => format!(
                    "warning: the Watchdog ended Test Connection {i}: nothing received for 3 s"
                ),
            })),
            // Stopped by the interrupt, which the Test says.
            Cause::Stopped if interrupted => {}
            Cause::Stopped => said.push(without_stop(EndReason::Stopped)),
            Cause::EndedEarly(reason) => said.push(without_stop(*reason)),
            Cause::EngineFailed(error) => said.push(own(match test_connection {
                None => format!("the Data Phase failed: {error}"),
                Some(i) => format!("Test Connection {i}'s Data Phase failed: {error}"),
            })),
            // The Server doesn't say why; this is the likely reason.
            Cause::ActivationRejected if options.fixed_rate => said.push(whole(
                "the Server rejected the Activation Request; only an Operator Key may run a Fixed-Rate Test (RFC 9946 §4.1), so check the key, or the Server's log",
            )),
            Cause::ActivationRejected => said.push(own(match test_connection {
                None => "the Server rejected the Activation Request".into(),
                Some(i) => format!("{server} rejected Test Connection {i}'s Activation Request"),
            })),
            Cause::Abandoned => {
                said.push(own("the Test was abandoned before it started".into()));
            }
            Cause::SocketError(error) => said.push(at_server(error.clone())),
            Cause::Configuration(Configuration::AmbiguousKey) => said.push(whole(
                "configuration error: several keys are valid now; choose one with --key-id",
            )),
            Cause::Configuration(Configuration::NoAuthentication) => said.push(whole(
                "configuration error: no key to authenticate with; give one with --key-file, or --unauthenticated for a Server that allows Unauthenticated Mode",
            )),
            Cause::Configuration(Configuration::NoKey) => said.push(whole(
                "configuration error: no key in the key file may be sent with now (its send lifetime), or none with the --key-id given",
            )),
            Cause::Configuration(Configuration::MulticastServer) => said.push(whole(format!(
                "configuration error: {server} is a multicast or broadcast address; the Server address must be unicast (RFC 9946 §6)"
            ))),
            Cause::SetupRejected(code) => {
                let (why, hint) = rejection(*code);
                let line = own(match test_connection {
                    None => format!("{server} refused the Test: {why}"),
                    Some(i) => format!("{server} refused Test Connection {i}: {why}"),
                });
                said.extend(refused(line, hint, test_connection));
            }
            Cause::UnknownSetupResponse(cmd_response) => said.push(own(match test_connection {
                None => format!(
                    "{server} answered the Setup Request with cmdResponse {cmd_response}, which RFC 9946 doesn't define; the Test was not started"
                ),
                Some(i) => format!(
                    "{server} answered Test Connection {i}'s Setup Request with cmdResponse {cmd_response}, which RFC 9946 doesn't define; it was not started"
                ),
            })),
            Cause::UnknownActivationResponse(cmd_response) => {
                let name =
                    names::control_phase_failure(ControlPhaseFailure::UnknownActivationResponse {
                        cmd_response: *cmd_response,
                    });
                said.push(own(match test_connection {
                    None => format!("the Test ended: {name}"),
                    Some(i) => format!("Test Connection {i} ended: {name}"),
                }));
            }
            // What is said follows the cause the exit status comes from.
            Cause::TimedOut {
                exchange,
                why,
                unverified,
                mode_2_unsupported,
            } => {
                if *mode_2_unsupported {
                    said.push(whole(format!(
                        "{server} probably doesn't support mode 2: it rejected it, unverified; use --auth-mode 1 (the Client never falls back by itself)"
                    )));
                }
                let stage = match exchange {
                    ControlExchange::Setup => "Setup",
                    ControlExchange::Activation => "Activation",
                };
                match (exchange, why) {
                    (ControlExchange::Setup, Unanswered::NoAnswer) => said.push(about_it(format!(
                        "no answer from {server} within {time_limit} s"
                    ))),
                    _ => said.push(at_server(format!(
                        "the {stage} exchange didn't complete within {time_limit} s"
                    ))),
                }
                if *why == Unanswered::Late {
                    said.push(whole(
                        "a response was more than 5 s from this host's clock; check both clocks",
                    ));
                }
                match (unverified, why) {
                    (Some(code), _) => {
                        let (why, hint) = rejection(*code);
                        let line = about_it(format!("an unverified Setup Response said: {why}"));
                        said.extend(refused(line, hint, test_connection));
                    }
                    (None, Unanswered::Broadcast) => said.push(about_it(
                        "a Setup Response came to a broadcast or multicast address and was ignored (RFC 9946 §6)"
                            .into(),
                    )),
                    (None, Unanswered::Unverified) => {
                        said.push(whole("a response failed verification; check the key"));
                    }
                    // No key signs the header checksum.
                    (None, Unanswered::Corrupted) => said.push(about_it(
                        "a response failed its header checksum (--checksum): it was corrupted on the path, or the Server computes the checksum otherwise"
                            .into(),
                    )),
                    (None, Unanswered::Late | Unanswered::NoAnswer) => {}
                }
            }
            Cause::UnacceptableCoercion(coercion) => said.push(about_it(format!(
                "{server} set {} to {} (asked for {}), which --refuse-coercion doesn't accept; the Test ended",
                names::parameter(coercion.parameter),
                coercion.coerced,
                coercion.requested
            ))),
            Cause::UnrunnableParameter { parameter, value } => said.push(about_it(format!(
                "the Server set {} to {value}, which the Client can't run with; the Test ended",
                names::parameter(*parameter)
            ))),
            Cause::SocketUnavailable => said.push(own(match test_connection {
                None => "the Test Connection's socket couldn't be opened".into(),
                Some(i) => format!("Test Connection {i}'s socket couldn't be opened"),
            })),
        }
        said
    }
}

/// One thing said of how a Test Connection ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    /// It concerns the whole Test, as this host's clock, the key and the
    /// Client's settings do, rather than this Test Connection alone: a
    /// Multi-Connection Test says it once, naming the Test Connections it
    /// was said of.
    pub whole_test: bool,
}

/// A line about one Test Connection alone.
fn own(text: String) -> Line {
    Line {
        text,
        whole_test: false,
    }
}

/// A line that concerns the whole Test.
fn whole(text: impl Into<String>) -> Line {
    Line {
        text: text.into(),
        whole_test: true,
    }
}

/// Test Connections by mcIndex, to name what is said of them all:
/// "Test Connection 0", "Test Connections 0 and 2", "Test Connections 0,
/// 1 and 3".
pub fn named(mc_indexes: &[u8]) -> String {
    match mc_indexes {
        [] => String::new(),
        [one] => format!("Test Connection {one}"),
        [rest @ .., last] => {
            let rest: Vec<String> = rest.iter().map(u8::to_string).collect();
            format!("Test Connections {} and {last}", rest.join(", "))
        }
    }
}

/// Why a Server refused the Test, for a Client user (docs/spec.md story 16),
/// and what to do about it where that concerns the whole Test: this host's
/// clock, the key, the Client's options, or trying again later.
fn rejection(code: SetupRejectionCode) -> (String, Option<&'static str>) {
    match code {
        SetupRejectionCode::BadVersion { expected } => {
            (format!("it expects protocol version {expected}"), None)
        }
        SetupRejectionCode::JumboMismatch | SetupRejectionCode::TraditionalMtuMismatch => {
            ("its Jumbo or Traditional MTU setting differs".into(), None)
        }
        // Its administrator, and whether it allows Unauthenticated Mode,
        // are this Server's.
        SetupRejectionCode::AuthNotConfigured => (
            "it has no authentication configured; ask its administrator, or use --unauthenticated if it allows that".into(),
            None,
        ),
        SetupRejectionCode::AuthRequired => {
            ("it requires authentication".into(), Some("give a --key-file"))
        }
        SetupRejectionCode::AuthModeInvalid => {
            ("it doesn't run this Authentication Mode".into(), None)
        }
        SetupRejectionCode::AuthFailed => (
            "authentication failed".into(),
            Some("check the key and keyId"),
        ),
        SetupRejectionCode::AuthTimeInvalid => (
            "the request's time was more than 5 s from its clock".into(),
            Some("check both clocks"),
        ),
        SetupRejectionCode::MaxBandwidthRequired => (
            "it requires a maxBandwidth".into(),
            Some("set --max-bandwidth"),
        ),
        SetupRejectionCode::CapacityExceeded => (
            "it has no room for the --max-bandwidth asked for".into(),
            Some("retry later or ask for less"),
        ),
        SetupRejectionCode::MultiConnectionInvalid => (
            "its Multi-Connection fields (mcIndex, mcCount, mcIdent, authMode, keyId) are invalid or disagree with the Test's other Test Connections".into(),
            None,
        ),
        SetupRejectionCode::ConnectionAllocationFailure => (
            "it runs as many Tests or Test Connections as it allows, or one from this address or network, or this address or key used its quota of Tests".into(),
            Some("retry later"),
        ),
    }
}

/// What is said of a refusal: `line`, of this Test Connection, and
/// `hint`, which concerns the whole Test. In a Test of one Test Connection
/// they are one line, the hint after the reason; among several, the hint
/// is a line of its own, which the Test says once for all it concerns.
fn refused(line: Line, hint: Option<&'static str>, test_connection: Option<u8>) -> Vec<Line> {
    match (hint, test_connection) {
        (None, _) => vec![line],
        (Some(hint), None) => vec![own(format!("{}; {hint}", line.text))],
        (Some(hint), Some(_)) => vec![line, whole(hint)],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use udpstp_core::{ActivationParameter, Coercion, DataEvent, Direction};

    fn outcome(result: ClientResult, events: Vec<Event>) -> ClientOutcome {
        ClientOutcome {
            server: "192.0.2.1:24601".parse().unwrap(),
            events,
            data_events: Vec::new(),
            data_event_times: Vec::new(),
            local: None,
            mc_ident: 1,
            result,
            engine_facts: udpstp_runtime::EngineFacts::default(),
        }
    }

    fn with_data(result: ClientResult, data_events: Vec<DataEvent>) -> ClientOutcome {
        ClientOutcome {
            data_events,
            ..outcome(result, vec![])
        }
    }

    fn finished(direction: Direction, reason: Option<EndReason>) -> ClientResult {
        ClientResult::Finished {
            direction,
            reason,
            error: None,
        }
    }

    fn failed(failure: ControlPhaseFailure) -> ClientResult {
        ClientResult::Failed(failure)
    }

    /// The Test initiation timer expired in `exchange`, the last response
    /// ignored there for `last_ignored`, as the core reports.
    fn timer_expired(
        exchange: ControlExchange,
        last_ignored: Option<ResponseIgnoreReason>,
    ) -> ClientResult {
        failed(ControlPhaseFailure::TestInitiationTimedOut {
            exchange,
            last_ignored,
        })
    }

    // docs/spec.md (the exit status equals ErrorStatus, which matches
    // OB-UDPST's; a completed Test with only warnings exits 0; our own codes
    // use numbers OB-UDPST leaves unused). Seam S2a's exit-code table (spec
    //  "Testing Decisions"); the loopback tests check some codes end to
    // end. Which response a timed-out Test's cause names is the core's
    // rule (crates/udpstp-core/tests/control_timers.rs).
    #[test]
    fn every_exit_status() {
        use ControlExchange::{Activation, Setup};
        use ControlPhaseFailure as F;
        use Direction::{Downstream, Upstream};
        use ResponseIgnoreReason::{
            AuthTimeOutOfWindow, AuthenticationFailed, ChecksumFailed, MulticastOrBroadcast,
        };
        let coercion = Coercion {
            parameter: ActivationParameter::TestIntTime,
            requested: 120,
            coerced: 60,
        };
        let table: Vec<(&str, ClientOutcome, u8, u8)> = vec![
            (
                "completed",
                outcome(finished(Downstream, Some(EndReason::Completed)), vec![]),
                0,
                0,
            ),
            (
                "the Client's Test Interval timer ended it",
                outcome(
                    finished(Downstream, Some(EndReason::TestIntervalElapsed)),
                    vec![],
                ),
                0,
                0,
            ),
            (
                "completed, with invalid PDUs and missed Status PDUs",
                with_data(
                    finished(Upstream, Some(EndReason::Completed)),
                    vec![
                        DataEvent::InvalidLoadPdus { count: 2 },
                        DataEvent::StatusPdusMissing { count: 1 },
                    ],
                ),
                2,
                0,
            ),
            (
                "completed after a 1 s gap and the peer's rxStopped",
                with_data(
                    finished(Downstream, Some(EndReason::Completed)),
                    vec![
                        DataEvent::PeerRxStopped { count: 3 },
                        DataEvent::TrafficStopped {
                            silent_since: udpstp_core::MonoTime::from_duration(
                                std::time::Duration::ZERO,
                            ),
                        },
                        DataEvent::InvalidStatusPdus { count: 1 },
                    ],
                ),
                4,
                0,
            ),
            (
                "Load PDUs stopped",
                outcome(finished(Downstream, Some(EndReason::Watchdog)), vec![]),
                200,
                200,
            ),
            (
                "Status PDUs stopped",
                outcome(finished(Upstream, Some(EndReason::Watchdog)), vec![]),
                200,
                200,
            ),
            (
                "Data Phase failed",
                outcome(finished(Upstream, None), vec![]),
                255,
                255,
            ),
            (
                "stopped",
                outcome(finished(Upstream, Some(EndReason::Stopped)), vec![]),
                255,
                255,
            ),
            (
                "no answer",
                outcome(timer_expired(Setup, None), vec![]),
                200,
                200,
            ),
            (
                "given up by the Multi-Connection Coordinator",
                outcome(ClientResult::Abandoned, vec![]),
                200,
                200,
            ),
            (
                "Setup Response failed verification",
                outcome(timer_expired(Setup, Some(AuthenticationFailed)), vec![]),
                107,
                107,
            ),
            (
                "Setup Response out of time",
                outcome(timer_expired(Setup, Some(AuthTimeOutOfWindow)), vec![]),
                108,
                108,
            ),
            (
                "Setup Response failed its header checksum",
                outcome(timer_expired(Setup, Some(ChecksumFailed)), vec![]),
                200,
                200,
            ),
            (
                "Setup Response to a broadcast address",
                outcome(timer_expired(Setup, Some(MulticastOrBroadcast)), vec![]),
                200,
                200,
            ),
            (
                "Activation Response failed verification",
                outcome(
                    timer_expired(Activation, Some(AuthenticationFailed)),
                    vec![],
                ),
                157,
                157,
            ),
            (
                "Activation Response out of time",
                outcome(timer_expired(Activation, Some(AuthTimeOutOfWindow)), vec![]),
                158,
                158,
            ),
            (
                "Activation Response failed its header checksum",
                outcome(timer_expired(Activation, Some(ChecksumFailed)), vec![]),
                200,
                200,
            ),
            (
                "no Activation Response",
                outcome(timer_expired(Activation, None), vec![]),
                200,
                200,
            ),
            (
                "CONNFAIL",
                outcome(
                    failed(F::SetupRejected(
                        SetupRejectionCode::ConnectionAllocationFailure,
                    )),
                    vec![],
                ),
                113,
                113,
            ),
            (
                "BADVER",
                outcome(
                    failed(F::SetupRejected(SetupRejectionCode::BadVersion {
                        expected: 20,
                    })),
                    vec![],
                ),
                102,
                102,
            ),
            (
                "undefined Setup code",
                outcome(failed(F::UnknownSetupResponse { cmd_response: 99 }), vec![]),
                100,
                100,
            ),
            (
                "BADPARAM",
                outcome(ClientResult::ActivationRejected, vec![]),
                152,
                152,
            ),
            (
                "undefined Activation code",
                outcome(
                    failed(F::UnknownActivationResponse { cmd_response: 9 }),
                    vec![],
                ),
                150,
                150,
            ),
            (
                "coercion refused",
                outcome(failed(F::UnacceptableCoercion(coercion)), vec![]),
                201,
                201,
            ),
            (
                "unrunnable parameter",
                outcome(
                    failed(F::UnrunnableParameter {
                        parameter: ActivationParameter::TestIntTime,
                        value: 1,
                    }),
                    vec![],
                ),
                202,
                202,
            ),
            (
                "no authentication",
                outcome(failed(F::NoAuthenticationConfigured), vec![]),
                50,
                50,
            ),
            (
                "multicast Server",
                outcome(failed(F::MulticastOrBroadcastServer), vec![]),
                50,
                50,
            ),
            (
                "no key now",
                outcome(failed(F::NoKeyForSending), vec![]),
                50,
                50,
            ),
            (
                "several keys",
                outcome(failed(F::AmbiguousKeyForSending), vec![]),
                50,
                50,
            ),
            (
                "socket error",
                outcome(ClientResult::SocketError("x".into()), vec![]),
                255,
                255,
            ),
        ];
        for (what, outcome, error_status, exit_code) in table {
            let status = TestConnectionEnd::of(&outcome).status();
            assert_eq!(
                (status.error_status, status.exit_code()),
                (error_status, exit_code),
                "{what}"
            );
        }
    }

    // : what a timed-out Test Connection says follows the cause its
    // exit status comes from, which the core reports with the failure: the
    // exchange the timer expired in and why the last response of that
    // exchange ignored was (which response that is, the core's tests
    // prove: crates/udpstp-core/tests/control_timers.rs). So a clock is
    // blamed only for 108 or 158, and the key only for 107 or 157; a
    // header checksum, which no key signs, points at the path or
    // --checksum. Each cause the core can report, once.
    #[test]
    fn what_a_timed_out_test_connection_says_follows_its_exit_status() {
        use ControlExchange::{Activation, Setup};
        use ResponseIgnoreReason::{
            AuthTimeOutOfWindow as Late, AuthenticationFailed as Unverified,
            ChecksumFailed as Corrupted, MulticastOrBroadcast as Broadcast,
        };
        // The cause the core reported, the exit status and what is said.
        type Case<'a> = (
            (ControlExchange, Option<ResponseIgnoreReason>),
            u8,
            &'a [&'a str],
        );
        let cases: Vec<Case<'_>> = vec![
            ((Setup, None), 200, &["no answer from"]),
            (
                (Setup, Some(Late)),
                108,
                &["the Setup exchange didn't complete"],
            ),
            (
                (Setup, Some(Unverified)),
                107,
                &["the Setup exchange didn't complete"],
            ),
            ((Setup, Some(Broadcast)), 200, &["broadcast or multicast"]),
            (
                (Setup, Some(Corrupted)),
                200,
                &["the Setup exchange didn't complete", "header checksum"],
            ),
            (
                (Activation, None),
                200,
                &["the Activation exchange didn't complete"],
            ),
            (
                (Activation, Some(Unverified)),
                157,
                &["the Activation exchange didn't complete"],
            ),
            (
                (Activation, Some(Late)),
                158,
                &["the Activation exchange didn't complete"],
            ),
            (
                (Activation, Some(Corrupted)),
                200,
                &["the Activation exchange didn't complete", "header checksum"],
            ),
        ];
        let options = crate::test_results::tests::options(false, &["192.0.2.1:24601"]);
        for ((exchange, last_ignored), status, says) in cases {
            let what = format!("{exchange:?}, {last_ignored:?}");
            let end =
                TestConnectionEnd::of(&outcome(timer_expired(exchange, last_ignored), vec![]));
            assert_eq!(end.status().error_status, status, "{what}");
            let said: Vec<String> = end
                .said(&options, None, false)
                .into_iter()
                .map(|line| line.text)
                .collect();
            for phrase in says {
                assert!(said.iter().any(|s| s.contains(phrase)), "{what}: {said:?}");
            }
            assert_eq!(
                said.iter().any(|s| s.contains("check both clocks")),
                matches!(status, 108 | 158),
                "{what}: {said:?}"
            );
            assert_eq!(
                said.iter().any(|s| s.contains("check the key")),
                matches!(status, 107 | 157),
                "{what}: {said:?}"
            );
        }
    }

    // The architecture review of 2026-09-30 (F5): among a Multi-Connection
    // Test's Test Connections, each says what one alone would, naming
    // itself; what concerns the whole Test (the clock, the key, the Client's
    // settings) is marked so the Test says it once; and one the
    // Multi-Connection Coordinator stopped or gave up says nothing, which
    // the Test's own notice explains
    #[test]
    fn among_several_a_test_connection_names_itself() {
        use ControlExchange::Setup;
        let options =
            crate::test_results::tests::options(false, &["192.0.2.1:24601", "192.0.2.2:24601"]);
        let said = |result: ClientResult| {
            TestConnectionEnd::of(&outcome(result, vec![])).said(&options, Some(1), false)
        };
        let late = said(timer_expired(
            Setup,
            Some(ResponseIgnoreReason::AuthTimeOutOfWindow),
        ));
        assert_eq!(
            late,
            [
                own("Test Connection 1 to 192.0.2.1:24601: the Setup exchange didn't complete within 3 s".into()),
                whole("a response was more than 5 s from this host's clock; check both clocks"),
            ]
        );
        assert_eq!(
            said(finished(Direction::Downstream, Some(EndReason::Watchdog))),
            [own(
                "warning: the Watchdog ended Test Connection 1: nothing received for 3 s".into()
            )]
        );
        // Why a Server refused it is its own; what to do about it, here
        // the clocks, concerns the whole Test.
        assert_eq!(
            said(failed(ControlPhaseFailure::SetupRejected(
                SetupRejectionCode::AuthTimeInvalid
            ))),
            [
                own("192.0.2.1:24601 refused Test Connection 1: the request's time was more than 5 s from its clock".into()),
                whole("check both clocks"),
            ]
        );
        for ended_by_the_coordinator in [
            ClientResult::Abandoned,
            finished(Direction::Downstream, Some(EndReason::Stopped)),
        ] {
            assert_eq!(said(ended_by_the_coordinator), []);
        }
    }

    #[test]
    fn test_connections_are_named_by_their_mc_index() {
        assert_eq!(named(&[0]), "Test Connection 0");
        assert_eq!(named(&[0, 2]), "Test Connections 0 and 2");
        assert_eq!(named(&[0, 1, 3]), "Test Connections 0, 1 and 3");
    }
}
