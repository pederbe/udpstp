//! Process exit statuses (docs/spec.md, "CLI, configuration, output and exit
//! codes"): the ErrorStatus OB-UDPST reports (`udpst.h`), except that a
//! Test that completed with only warnings exits 0.
//!
//! | ErrorStatus | Meaning |
//! |---|---|
//! | 0 | The Test completed, with the STOP exchange or when its Test Interval ran out |
//! | 49 | Ours, JSON only, the last warning code, where OB-UDPST won't add one: the Test completed without a valid Maximum IP-Layer Capacity (no Sub-Interval met the Performance Criterion); it exits 0 |
//! | 2-6 | The Test completed with a warning (JSON only; it exits 0), in order: 4 nothing received for 1 s at some point (WARN_LOC_STOPPED), 5 the peer said it had stopped receiving (WARN_REM_STOPPED), 3 the peer missed Status PDUs (WARN_REM_STATUS), 2 Status PDUs never arrived (WARN_LOC_STATUS), 6 invalid PDUs (WARN_RX_INVPDU); the first of these that applies |
//! | 50 | The configuration or command line is invalid |
//! | 51 | The key file can't be read or is invalid |
//! | 75 | The Client or Server couldn't start |
//! | 100 + code | The Server refused the Setup Request with that cmdResponse; 100 alone for a code RFC 9946 doesn't define, 107 or 108 for a Setup Response that failed verification or the time window |
//! | 150 + code | The same for the Activation exchange: 152 for BADPARAM, 150 for an undefined code, 157 or 158 for a response that failed verification |
//! | 200 | The Test didn't complete: no answer from the Server in time, or the Watchdog ended it (ERROR_CONN_MIN, as OB-UDPST ends up reporting both) |
//! | 201 | Ours: the Server coerced a parameter the Client was told to refuse |
//! | 202 | Ours: the Server set a parameter the Client can't run with |
//! | 203 | Ours: the Servers gave a Multi-Connection Test's Test Connections different parameters |
//! | 204 | Ours: the Client user interrupted the running Test (Ctrl-C, or a signal to stop), or a write to its standard output failed, while the Test ran or with its final results. On Unix a Client stopped by a signal then dies of it, so a shell sees 128 plus the signal (130 for SIGINT, 143 for SIGTERM, 129 for SIGHUP); 204 is then the JSON ErrorStatus, unless every Test Connection had already ended, when it reflects the Test. On Windows, and for a failed write, the Client exits 204 |
//! | 255 | Any other error |
//!
//! A Multi-Connection Test reports as OB-UDPST does: once any Test
//! Connection completed, success with the most severe warning of all; if
//! none did, the first Test Connection's error, by mcIndex, or 200 when
//! none had one of its own (ERROR_CONN_MIN, which keeps an earlier error).
//!
//! Our own codes sit in OB-UDPST's connection-error range (200 plus an
//! offset), which uses only 200, away from the ranges that add an IANA
//! registered cmdResponse. Codes above 128 read like a shell's "killed by
//! signal" status; that comes with matching OB-UDPST. OB-UDPST keeps the last
//! warning and lets a warning replace an error; we keep the most severe.
//! (conformance: exit-zero-with-warnings, exit-own-error-statuses, exit-most-severe-warning, client-interrupt)

#![forbid(unsafe_code)]

use crate::protocol::{DataEvent, Event, TestAbort};
use crate::runtime::TestOutcome;

use crate::test_connection_end::TestConnectionEnd;

/// The Test completed.
pub const SUCCESS: u8 = 0;

/// Warnings a completed Test can have (OB-UDPST's WARN_*), most severe
/// first: nothing received for 1 s at some point; the peer said it had
/// stopped receiving; the peer missed Status PDUs; Status PDUs never
/// arrived here; invalid PDUs.
pub const TRAFFIC_STOPPED: u8 = 4;
pub const PEER_STOPPED_RECEIVING: u8 = 5;
pub const PEER_MISSED_STATUS_PDUS: u8 = 3;
pub const STATUS_PDUS_MISSING: u8 = 2;
pub const INVALID_PDUS: u8 = 6;

/// Ours, in OB-UDPST's warning range: a completed Test with no valid
/// Maximum IP-Layer Capacity, because no Sub-Interval of a Capacity Mode
/// met the Performance Criterion. The most severe warning.
pub const NO_VALID_MAXIMUM: u8 = 49;

/// The Test didn't complete: fewer Test Connections than it needs, which
/// is what OB-UDPST ends up reporting after a Server timeout or a Watchdog
/// end (ERROR_CONN_MIN).
pub const CONNECTIONS_UNAVAILABLE: u8 = 200;

/// The last warning status (OB-UDPST STATUS_WARNMAX).
const WARNING_MAX: u8 = 49;

/// The configuration or command line is invalid (OB-UDPST
/// ERROR_CONF_GENERIC).
pub const CONFIGURATION_ERROR: u8 = 50;

/// The key file can't be read, is readable by others, or is invalid
/// (OB-UDPST ERROR_CONF_KEYFILE). A key file with no key to send with now,
/// or several, is a configuration error (50).
pub const KEY_FILE_ERROR: u8 = CONFIGURATION_ERROR + 1;

/// The Server or Client couldn't start: a socket couldn't be bound, or a
/// host name couldn't be resolved.
pub const INITIALIZATION_ERROR: u8 = 75;

/// Setup exchange errors: plus the Setup Response's cmdResponse.
pub const SETUP_ERROR_BASE: u8 = 100;

/// Activation exchange errors: plus the Activation Response's cmdResponse.
pub const ACTIVATION_ERROR_BASE: u8 = 150;

/// CHTA_CRSP_BADPARAM.
pub const BAD_PARAMETER: u8 = 2;

/// CHSR_CRSP_AUTHFAIL and CHSR_CRSP_AUTHTIME, which OB-UDPST adds to either
/// base for a response that failed verification.
pub const AUTH_FAILURE: u8 = 7;
pub const AUTH_TIME: u8 = 8;

/// Ours: the Server coerced a parameter the Client was told to refuse.
pub const COERCION_REFUSED: u8 = 201;

/// Ours: the Server set a parameter the Client can't run with.
pub const UNRUNNABLE_PARAMETER: u8 = 202;

/// Ours: the Servers gave a Multi-Connection Test's Test Connections
/// different parameters, so the Client aborted it.
pub const PARAMETERS_DIFFER: u8 = 203;

/// Ours: the Client user interrupted the Test, or its standard output
/// closed.
pub const INTERRUPTED: u8 = 204;

/// Any error without a more specific status.
pub const GENERIC_ERROR: u8 = 255;

/// How a Test ended, or one of its Test Connections: its ErrorStatus, and
/// whether it completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    pub error_status: u8,
    pub completed: bool,
}

impl Status {
    /// It didn't complete, for `error_status`.
    pub fn failed(error_status: u8) -> Self {
        Self {
            error_status,
            completed: false,
        }
    }

    /// The process exit status: the ErrorStatus, but 0 for a Test that
    /// completed with only warnings (docs/spec.md).
    pub fn exit_code(self) -> u8 {
        if self.completed && self.error_status <= WARNING_MAX {
            SUCCESS
        } else {
            self.error_status
        }
    }
}

/// The ErrorStatus of a Setup Response's `cmd_response`: 100 plus it, or
/// 100 alone for a code RFC 9946 §12.3.5 doesn't define, as OB-UDPST does.
pub fn setup_rejected(cmd_response: u8) -> u8 {
    match cmd_response {
        2..=13 => SETUP_ERROR_BASE + cmd_response,
        _ => SETUP_ERROR_BASE,
    }
}

/// The most severe warning a Data Phase's events gave, if any.
pub fn warning(data_events: &[DataEvent]) -> Option<u8> {
    let had = |status: u8| {
        data_events.iter().any(|event| match event {
            DataEvent::TrafficStopped { .. } => status == TRAFFIC_STOPPED,
            DataEvent::PeerRxStopped { .. } => status == PEER_STOPPED_RECEIVING,
            DataEvent::PeerMissedStatusPdus { .. } => status == PEER_MISSED_STATUS_PDUS,
            DataEvent::StatusPdusMissing { .. } | DataEvent::StaleStatusPdus { .. } => {
                status == STATUS_PDUS_MISSING
            }
            DataEvent::InvalidLoadPdus { .. }
            | DataEvent::InvalidStatusPdus { .. }
            | DataEvent::StatusPdusFailedVerification { .. } => status == INVALID_PDUS,
            _ => false,
        })
    };
    [
        TRAFFIC_STOPPED,
        PEER_STOPPED_RECEIVING,
        PEER_MISSED_STATUS_PDUS,
        STATUS_PDUS_MISSING,
        INVALID_PDUS,
    ]
    .into_iter()
    .find(|&status| had(status))
}

/// A Test's Status, from each Test Connection's
/// ([`TestConnectionEnd::status`]): see the module's table for a
/// Multi-Connection Test.
pub fn test_status(test: &TestOutcome) -> Status {
    let abort = test.events.iter().find_map(|e| match e {
        Event::TestAborted(reason) => Some(*reason),
        _ => None,
    });
    let ends: Vec<TestConnectionEnd> = test.connections.iter().map(TestConnectionEnd::of).collect();
    match abort {
        Some(TestAbort::ParametersDiffer { .. }) => return Status::failed(PARAMETERS_DIFFER),
        Some(TestAbort::Interrupted) => return Status::failed(INTERRUPTED),
        Some(TestAbort::TestConnectionEnded { mc_index }) => {
            if let Some(end) = ends.get(usize::from(mc_index)) {
                return end.status();
            }
        }
        Some(TestAbort::TooFewTestConnections { .. } | TestAbort::TestConnectionsTimedOut)
        | None => {}
    }
    if abort.is_none() && ends.iter().any(TestConnectionEnd::completed) {
        let warning = ends
            .iter()
            .filter_map(TestConnectionEnd::warning)
            .min_by_key(|w| severity(*w));
        return Status {
            error_status: warning.unwrap_or(SUCCESS),
            completed: true,
        };
    }
    // An error of its own: not 200, which ERROR_CONN_MIN gives anyway, nor
    // the Multi-Connection Coordinator's doing.
    ends.iter()
        .map(|end| (end.status(), end))
        .find(|(status, end)| {
            status.error_status != CONNECTIONS_UNAVAILABLE && !end.ended_by_the_coordinator()
        })
        .map_or(Status::failed(CONNECTIONS_UNAVAILABLE), |(status, _)| {
            Status::failed(status.error_status)
        })
}

/// A warning's place in [`warning`]'s order, the most severe first. No
/// valid Maximum IP-Layer Capacity (49) is more severe than all of them,
/// and only the JSON's ErrorStatus has it: see
/// `TestResults::error_status`.
fn severity(status: u8) -> usize {
    [
        TRAFFIC_STOPPED,
        PEER_STOPPED_RECEIVING,
        PEER_MISSED_STATUS_PDUS,
        STATUS_PDUS_MISSING,
        INVALID_PDUS,
    ]
    .iter()
    .position(|&s| s == status)
    .unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{
        ActivationParameter, Coercion, ControlPhaseFailure, Direction, EndReason,
        SetupRejectionCode,
    };
    use crate::runtime::{ClientOutcome, ClientResult};

    fn outcome(result: ClientResult, events: Vec<Event>) -> ClientOutcome {
        ClientOutcome {
            server: "192.0.2.1:24601".parse().unwrap(),
            events,
            data_events: Vec::new(),
            data_event_times: Vec::new(),
            local: None,
            mc_ident: 1,
            result,
            engine_facts: crate::runtime::EngineFacts::default(),
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

    // , ADR 0013 (exit statuses are stable: new values may be added,
    // an assigned one never changes meaning). Every assigned ErrorStatus,
    // with the process exit status of a Test that completed with it.
    #[test]
    fn every_assigned_exit_status_is_in_the_checked_in_list() {
        let mut named: Vec<(u8, String)> = [
            (SUCCESS, "SUCCESS"),
            (STATUS_PDUS_MISSING, "STATUS_PDUS_MISSING"),
            (PEER_MISSED_STATUS_PDUS, "PEER_MISSED_STATUS_PDUS"),
            (TRAFFIC_STOPPED, "TRAFFIC_STOPPED"),
            (PEER_STOPPED_RECEIVING, "PEER_STOPPED_RECEIVING"),
            (INVALID_PDUS, "INVALID_PDUS"),
            (NO_VALID_MAXIMUM, "NO_VALID_MAXIMUM"),
            (CONFIGURATION_ERROR, "CONFIGURATION_ERROR"),
            (KEY_FILE_ERROR, "KEY_FILE_ERROR"),
            (INITIALIZATION_ERROR, "INITIALIZATION_ERROR"),
            (
                SETUP_ERROR_BASE,
                "SETUP_ERROR_BASE (an undefined cmdResponse)",
            ),
            (
                ACTIVATION_ERROR_BASE,
                "ACTIVATION_ERROR_BASE (an undefined cmdResponse)",
            ),
            (ACTIVATION_ERROR_BASE + BAD_PARAMETER, "Activation BADPARAM"),
            (
                ACTIVATION_ERROR_BASE + AUTH_FAILURE,
                "Activation Response failed verification",
            ),
            (
                ACTIVATION_ERROR_BASE + AUTH_TIME,
                "Activation Response outside the time window",
            ),
            (CONNECTIONS_UNAVAILABLE, "CONNECTIONS_UNAVAILABLE"),
            (COERCION_REFUSED, "COERCION_REFUSED"),
            (UNRUNNABLE_PARAMETER, "UNRUNNABLE_PARAMETER"),
            (PARAMETERS_DIFFER, "PARAMETERS_DIFFER"),
            (INTERRUPTED, "INTERRUPTED"),
            (GENERIC_ERROR, "GENERIC_ERROR"),
        ]
        .into_iter()
        .map(|(status, name)| (status, name.to_owned()))
        .collect();
        // The Setup exchange: 100 plus each cmdResponse RFC 9946 defines,
        // the verification failures among them.
        for code in 2..=13 {
            named.push((setup_rejected(code), format!("Setup cmdResponse {code}")));
        }
        named.sort();
        let mut seen = std::collections::BTreeSet::new();
        let mut text = String::new();
        for (status, name) in &named {
            assert!(seen.insert(*status), "{status} has two meanings");
            let completed = Status {
                error_status: *status,
                completed: true,
            };
            let exit = if *status <= WARNING_MAX {
                format!("completed: exit {}", completed.exit_code())
            } else {
                format!("exit {}", Status::failed(*status).exit_code())
            };
            text.push_str(&format!("{status}\t{name}\t{exit}\n"));
        }
        crate::results::tests::golden("exit-statuses.txt", &text);
    }

    #[test]
    fn every_defined_setup_code_adds_to_100() {
        for code in 2..=13 {
            assert_eq!(setup_rejected(code), 100 + code);
        }
        for code in [0, 1, 14, 200, 255] {
            assert_eq!(setup_rejected(code), 100);
        }
    }

    fn test(events: Vec<Event>, connections: Vec<ClientOutcome>) -> TestOutcome {
        TestOutcome {
            mc_ident: 1,
            events,
            connections,
        }
    }

    // docs/spec.md (a Multi-Connection Test reports as OB-UDPST does:
    // success with the most severe warning once any Test Connection
    // completed, else the first error, else 200);
    #[test]
    fn a_tests_status_from_its_test_connections() {
        use ControlPhaseFailure as F;
        use Direction::Downstream;
        let completed = || outcome(finished(Downstream, Some(EndReason::Completed)), vec![]);
        let rejected = || {
            outcome(
                failed(F::SetupRejected(SetupRejectionCode::CapacityExceeded)),
                vec![],
            )
        };
        let stopped = || outcome(finished(Downstream, Some(EndReason::Stopped)), vec![]);
        let too_few = Event::TestAborted(TestAbort::TooFewTestConnections { minimum: 2 });
        let cases: Vec<(&str, TestOutcome, u8, bool)> = vec![
            (
                "interrupted, one Test Connection stopped",
                test(
                    vec![Event::TestAborted(TestAbort::Interrupted)],
                    vec![stopped()],
                ),
                INTERRUPTED,
                false,
            ),
            (
                "interrupted after one Test Connection completed",
                test(
                    vec![Event::TestAborted(TestAbort::Interrupted)],
                    vec![completed(), stopped()],
                ),
                INTERRUPTED,
                false,
            ),
            (
                "one Test Connection, completed",
                test(vec![], vec![completed()]),
                0,
                true,
            ),
            (
                "one completed, one refused: a partial Test succeeds",
                test(vec![], vec![rejected(), completed()]),
                0,
                true,
            ),
            (
                "the most severe warning of all",
                test(
                    vec![],
                    vec![
                        with_data(
                            finished(Downstream, Some(EndReason::Completed)),
                            vec![DataEvent::StatusPdusMissing { count: 1 }],
                        ),
                        with_data(
                            finished(Downstream, Some(EndReason::Completed)),
                            vec![DataEvent::TrafficStopped {
                                silent_since: crate::protocol::MonoTime::from_duration(
                                    std::time::Duration::ZERO,
                                ),
                            }],
                        ),
                    ],
                ),
                TRAFFIC_STOPPED,
                true,
            ),
            (
                "too few: the first error, not 200",
                test(
                    vec![too_few.clone()],
                    vec![
                        outcome(ClientResult::Abandoned, vec![]),
                        rejected(),
                        rejected(),
                    ],
                ),
                setup_rejected(SetupRejectionCode::CapacityExceeded.cmd_response()),
                false,
            ),
            (
                "too few, without an error of their own",
                test(
                    vec![too_few.clone()],
                    vec![outcome(ClientResult::Abandoned, vec![]), stopped()],
                ),
                CONNECTIONS_UNAVAILABLE,
                false,
            ),
            (
                "the Servers disagreed",
                test(
                    vec![Event::TestAborted(TestAbort::ParametersDiffer {
                        mc_index: 1,
                    })],
                    vec![stopped(), stopped()],
                ),
                PARAMETERS_DIFFER,
                false,
            ),
            (
                "a Test Connection refused a coercion",
                test(
                    vec![Event::TestAborted(TestAbort::TestConnectionEnded {
                        mc_index: 1,
                    })],
                    vec![
                        stopped(),
                        outcome(
                            failed(F::UnacceptableCoercion(Coercion {
                                parameter: ActivationParameter::DscpEcn,
                                requested: 184,
                                coerced: 0,
                            })),
                            vec![],
                        ),
                    ],
                ),
                COERCION_REFUSED,
                false,
            ),
        ];
        for (what, test, error_status, completed) in cases {
            assert_eq!(
                test_status(&test),
                Status {
                    error_status,
                    completed
                },
                "{what}"
            );
        }
    }
}
