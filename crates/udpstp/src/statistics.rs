//! The Server's counters and per-Test summaries (docs/spec.md story 81),
//! kept from every notice, whether the log's limiter lets it through or
//! not. The counters run from the Server's start; the log reports them
//! every `statistics_interval`. A Test's summary is complete when its last
//! Test Connection is released, as the core forgets the Test then too.

use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use udpstp_core::{
    AuthenticationFailure, DataEvent, Direction, EndReason, Event, SetupRejection,
    SilentRejectionReason, SocketId, TestParameters,
};
use udpstp_runtime::ServerNotice;

use crate::names::{self, event};

/// The Server's counters since it started.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Counters {
    /// Tests admitted: those with a Test Connection admitted.
    pub tests: u64,
    pub test_connections: u64,
    /// Test Connections admitted and not yet released.
    pub active_test_connections: u64,
    /// Setup and Activation Requests refused, with a response or without,
    /// by the event that logs them and its reason.
    pub refusals: BTreeMap<(&'static str, &'static str), u64>,
    /// Requests and Status PDUs that failed authentication. The requests
    /// are counted in `refusals` too.
    pub authentication_failures: u64,
    /// Setup Requests over the unverified or a Source's budget, counted in
    /// `refusals` too.
    pub rate_limited: u64,
    /// Status PDUs ignored as stale.
    pub stale_status_pdus: u64,
    /// Setup Requests ignored, not refused: duplicates, and those whose
    /// arrival address was unknown.
    pub setup_requests_ignored: u64,
    /// Unactivated Test Connections closed to make room for a new Setup
    ///
    pub unactivated_test_connections_evicted: u64,
}

/// What the log reports of a Test once it has ended.
#[derive(Debug, Clone, PartialEq)]
pub struct TestSummary {
    /// Its first Test Connection's Client address.
    pub client: SocketAddr,
    pub mc_ident: u16,
    /// The keyId, in an authenticated mode.
    pub key_id: Option<u8>,
    pub test_connections: u32,
    /// Once a Test Connection was activated.
    pub direction: Option<Direction>,
    pub parameters: Option<TestParameters>,
    /// Load PDUs' UDP payload bytes the Server sent, as Load Sender, and
    /// the Load Receiver received, as the Server measured them or the
    /// Client's Status PDUs reported them.
    pub bytes_sent: u64,
    pub bytes_received: u64,
    /// The IP-Layer Sender Bit Rate the Server measured as Load Sender over
    /// the Test, all its Test Connections together, in bit/s: the
    /// bits they sent over the longest time one of them spent sending, as
    /// they run at once.
    pub ip_layer_sender_bit_rate: Option<u64>,
    /// What the Server didn't send as Load Sender, all its Test
    /// Connections together: Skipped Bursts, for falling behind,
    /// and Withheld Bursts, for a full send buffer, whether the log
    /// reported their events or suppressed them.
    pub skipped: NotSent,
    pub withheld: NotSent,
    /// Receive Buffer Overflows at its Test Connections' sockets, summed
    /// over those whose OS counts them.
    pub receive_buffer_overflows: Option<u64>,
    /// `Completed`; `ServerStopping`, for a Test cut short when the Server
    /// stopped; how the first Test Connection that didn't complete ended:
    /// an [`EndReason`] name, `DataPhaseEngineFailed`, or the event that
    /// ended it; or `NotActivated`.
    pub result: &'static str,
    /// The rejection's reason, when `result` is `ActivationRequestRejected`.
    pub reason: Option<&'static str>,
}

/// Bursts not sent, their Load PDUs and their UDP payload bytes.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct NotSent {
    pub bursts: u64,
    pub datagrams: u64,
    pub bytes: u64,
}

impl NotSent {
    fn add(&mut self, bursts: u32, datagrams: u32, bytes: u64) {
        self.bursts += u64::from(bursts);
        self.datagrams += u64::from(datagrams);
        self.bytes += bytes;
    }
}

impl TestSummary {
    /// Whether an attacker could have caused it: a Test none of whose Test
    /// Connections was activated, which a spoofed source can bring about
    /// as fast as the Server admits Setups. Only these are limited.
    pub fn unverified(&self) -> bool {
        self.direction.is_none()
    }
}

/// A Test as the core identifies it: the Client's IP address and mcIdent.
type TestKey = (IpAddr, u16);

#[derive(Debug)]
struct OpenTest {
    summary: TestSummary,
    open: u32,
    completed: bool,
    /// How the first Test Connection that didn't complete ended, and why.
    failure: Option<(&'static str, Option<&'static str>)>,
    /// The IP-layer bits sent, and the longest time a Test Connection
    /// spent sending.
    sent_bits: u128,
    sending: Duration,
}

/// The counters, and the Tests still open.
#[derive(Debug, Default)]
pub struct Statistics {
    counters: Counters,
    tests: HashMap<TestKey, OpenTest>,
    /// Each open Test Connection's Test and time spent sending.
    sockets: HashMap<SocketId, (TestKey, Duration)>,
}

impl Statistics {
    pub fn counters(&self) -> &Counters {
        &self.counters
    }

    /// Counts `notice`; returns the summary of the Test it ends, if any.
    pub fn observe(&mut self, notice: &ServerNotice) -> Option<TestSummary> {
        match notice {
            ServerNotice::Core(event) => return self.core(event),
            ServerNotice::Data { socket, event } => self.data(*socket, event),
            // Every Test still open is cut short.
            ServerNotice::Stopping { .. } => {
                for test in self.tests.values_mut() {
                    test.failure
                        .get_or_insert((names::summary::SERVER_STOPPING, None));
                }
            }
            ServerNotice::DataPhaseEnded {
                socket,
                reason,
                engine_facts,
                ..
            } => {
                if let (Some(n), Some(test)) =
                    (engine_facts.receive_buffer_overflows, self.test(*socket))
                {
                    *test.summary.receive_buffer_overflows.get_or_insert(0) += n;
                }
                match reason {
                    Some(EndReason::Completed) => {
                        if let Some(test) = self.test(*socket) {
                            test.completed = true;
                        }
                    }
                    Some(reason) => self.fail(*socket, names::end_reason(*reason), None),
                    None => self.fail(*socket, names::outcome::DATA_PHASE_ENGINE_FAILED, None),
                }
            }
            _ => {}
        }
        None
    }

    fn core(&mut self, notice: &Event) -> Option<TestSummary> {
        let c = &mut self.counters;
        match notice {
            Event::TestConnectionAdmitted {
                socket,
                client,
                mc_ident,
                key_id,
            } => {
                c.test_connections += 1;
                c.active_test_connections += 1;
                let key = (client.ip(), *mc_ident);
                let test = self.tests.entry(key).or_insert_with(|| {
                    c.tests += 1;
                    OpenTest {
                        summary: TestSummary {
                            client: *client,
                            mc_ident: *mc_ident,
                            key_id: *key_id,
                            test_connections: 0,
                            direction: None,
                            parameters: None,
                            bytes_sent: 0,
                            bytes_received: 0,
                            ip_layer_sender_bit_rate: None,
                            skipped: NotSent::default(),
                            withheld: NotSent::default(),
                            receive_buffer_overflows: None,
                            result: names::summary::NOT_ACTIVATED,
                            reason: None,
                        },
                        open: 0,
                        completed: false,
                        failure: None,
                        sent_bits: 0,
                        sending: Duration::ZERO,
                    }
                });
                test.open += 1;
                test.summary.test_connections += 1;
                self.sockets.insert(*socket, (key, Duration::ZERO));
            }
            Event::TestConnectionActivated {
                socket,
                direction,
                parameters,
            } => {
                if let Some(test) = self.test(*socket) {
                    test.summary.direction = Some(*direction);
                    test.summary.parameters.get_or_insert(*parameters);
                }
            }
            Event::TestConnectionReleased { socket } => return self.release(*socket),
            Event::ActivationWatchdogExpired { socket } => {
                self.fail(*socket, event::ACTIVATION_WATCHDOG_EXPIRED, None);
            }
            Event::UnactivatedTestConnectionEvicted { socket } => {
                c.unactivated_test_connections_evicted += 1;
                self.fail(*socket, event::UNACTIVATED_TEST_CONNECTION_EVICTED, None);
            }
            Event::SetupRequestRejected { reason, .. } => {
                match reason {
                    SetupRejection::SourceBudgetExhausted
                    | SetupRejection::Authentication(AuthenticationFailure::BudgetExhausted) => {
                        c.rate_limited += 1;
                    }
                    SetupRejection::Authentication(_) => c.authentication_failures += 1,
                    _ => {}
                }
                let key = (
                    event::SETUP_REQUEST_REJECTED,
                    names::setup_rejection(*reason),
                );
                *c.refusals.entry(key).or_default() += 1;
            }
            Event::SetupRequestIgnored { .. } => c.setup_requests_ignored += 1,
            Event::ActivationRequestRejected { socket, reason } => {
                let reason = names::activation_rejection(*reason);
                *c.refusals
                    .entry((event::ACTIVATION_REQUEST_REJECTED, reason))
                    .or_default() += 1;
                self.fail(*socket, event::ACTIVATION_REQUEST_REJECTED, Some(reason));
            }
            Event::SetupRequestSilentlyRejected { reason, .. }
            | Event::ActivationRequestSilentlyRejected { reason, .. } => {
                match reason {
                    SilentRejectionReason::UnverifiedBudgetExhausted
                    | SilentRejectionReason::SourceBudgetExhausted
                    | SilentRejectionReason::KeyBudgetExhausted => {
                        c.rate_limited += 1;
                    }
                    SilentRejectionReason::UnauthenticatedNotAllowed
                    | SilentRejectionReason::UnsupportedAuthMode
                    | SilentRejectionReason::NoKeysConfigured
                    | SilentRejectionReason::UnknownKeyId
                    | SilentRejectionReason::KeyOutsideLifetime
                    | SilentRejectionReason::AuthenticationFailed
                    | SilentRejectionReason::AuthTimeOutOfWindow
                    | SilentRejectionReason::AuthModeMismatch
                    | SilentRejectionReason::KeyIdMismatch => c.authentication_failures += 1,
                    _ => {}
                }
                let kind = match notice {
                    Event::SetupRequestSilentlyRejected { .. } => {
                        event::SETUP_REQUEST_SILENTLY_REJECTED
                    }
                    _ => event::ACTIVATION_REQUEST_SILENTLY_REJECTED,
                };
                *c.refusals
                    .entry((kind, names::silent_rejection_reason(*reason)))
                    .or_default() += 1;
            }
            _ => {}
        }
        None
    }

    fn data(&mut self, socket: SocketId, event: &DataEvent) {
        match event {
            DataEvent::StatusPdusFailedVerification { count } => {
                self.counters.authentication_failures += u64::from(*count);
            }
            DataEvent::StaleStatusPdus { count } => {
                self.counters.stale_status_pdus += u64::from(*count);
            }
            DataEvent::Sent(sent) => {
                let Some((key, sending)) = self.sockets.get_mut(&socket) else {
                    return;
                };
                *sending += sent.duration;
                let (key, sending) = (*key, *sending);
                if let Some(test) = self.tests.get_mut(&key) {
                    test.summary.bytes_sent += sent.bytes;
                    test.sent_bits += u128::from(sent.ip_layer_sender_bit_rate)
                        * sent.duration.as_nanos()
                        / 1_000_000_000;
                    test.sending = test.sending.max(sending);
                }
            }
            DataEvent::SubIntervalMeasured(measured) => {
                if let Some(test) = self.test(socket) {
                    test.summary.bytes_received += measured.bytes;
                }
            }
            DataEvent::BurstsSkipped {
                bursts,
                datagrams,
                bytes,
            } => {
                if let Some(test) = self.test(socket) {
                    test.summary.skipped.add(*bursts, *datagrams, *bytes);
                }
            }
            DataEvent::BurstsWithheld {
                bursts,
                datagrams,
                bytes,
            } => {
                if let Some(test) = self.test(socket) {
                    test.summary.withheld.add(*bursts, *datagrams, *bytes);
                }
            }
            _ => {}
        }
    }

    fn test(&mut self, socket: SocketId) -> Option<&mut OpenTest> {
        let (key, _) = self.sockets.get(&socket)?;
        self.tests.get_mut(key)
    }

    /// Notes how a Test Connection ended, and why, if it's the first not to
    /// complete.
    fn fail(&mut self, socket: SocketId, how: &'static str, why: Option<&'static str>) {
        if let Some(test) = self.test(socket) {
            test.failure.get_or_insert((how, why));
        }
    }

    fn release(&mut self, socket: SocketId) -> Option<TestSummary> {
        let (key, _) = self.sockets.remove(&socket)?;
        self.counters.active_test_connections -= 1;
        let test = self.tests.get_mut(&key)?;
        test.open -= 1;
        if test.open > 0 {
            return None;
        }
        let mut test = self.tests.remove(&key)?;
        let completed = (names::end_reason(EndReason::Completed), None);
        (test.summary.result, test.summary.reason) = match test.failure.take() {
            Some(failure) => failure,
            None if test.completed => completed,
            None => (names::summary::NOT_ACTIVATED, None),
        };
        if !test.sending.is_zero() {
            let rate = test.sent_bits * 1_000_000_000 / test.sending.as_nanos();
            test.summary.ip_layer_sender_bit_rate = Some(rate.try_into().unwrap_or(u64::MAX));
        }
        Some(test.summary)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use udpstp_core::{ActivationRejection, SentInterval};

    use super::*;

    fn admitted(socket: u32, port: u16, mc_ident: u16) -> ServerNotice {
        ServerNotice::Core(Event::TestConnectionAdmitted {
            socket: SocketId(socket),
            client: SocketAddr::from(([192, 0, 2, 7], port)),
            mc_ident,
            key_id: Some(4),
        })
    }

    fn released(socket: u32) -> ServerNotice {
        ServerNotice::Core(Event::TestConnectionReleased {
            socket: SocketId(socket),
        })
    }

    fn ended(socket: u32, reason: EndReason) -> ServerNotice {
        ServerNotice::DataPhaseEnded {
            socket: SocketId(socket),
            reason: Some(reason),
            error: None,
            engine_facts: udpstp_runtime::EngineFacts {
                receive_buffer_overflows: Some(2),
                ..udpstp_runtime::EngineFacts::default()
            },
        }
    }

    /// An st of 1 s at `rate` bit/s.
    fn sent(socket: u32, rate: u64) -> ServerNotice {
        ServerNotice::Data {
            socket: SocketId(socket),
            event: DataEvent::Sent(SentInterval {
                st_seq_no: 1,
                datagrams: 1,
                bytes: 1000,
                ip_layer_sender_bit_rate: rate,
                duration: Duration::from_secs(1),
            }),
        }
    }

    // docs/spec.md story 81 (a summary record per Test);
    #[test]
    fn a_multi_connection_test_is_summarized_once_its_last_connection_is_released() {
        let mut stats = Statistics::default();
        let notices = [
            admitted(1, 5000, 9),
            admitted(2, 5001, 9),
            // Another Test from the same address.
            admitted(3, 5002, 10),
            sent(1, 3_000_000),
            sent(1, 3_000_000),
            sent(2, 2_000_000),
            ended(1, EndReason::Completed),
            ended(2, EndReason::Watchdog),
            released(1),
        ];
        for notice in &notices {
            assert_eq!(stats.observe(notice), None, "{notice:?}");
        }
        let summary = stats.observe(&released(2)).expect("the Test ended");
        assert_eq!(summary.client, SocketAddr::from(([192, 0, 2, 7], 5000)));
        assert_eq!((summary.mc_ident, summary.key_id), (9, Some(4)));
        assert_eq!(summary.test_connections, 2);
        assert_eq!(summary.bytes_sent, 3000);
        // 8 Mbit over the longest Test Connection's 2 s.
        assert_eq!(summary.ip_layer_sender_bit_rate, Some(4_000_000));
        // : both Test Connections' Receive Buffer Overflows.
        assert_eq!(summary.receive_buffer_overflows, Some(4));
        // One Test Connection didn't complete, so neither did the Test.
        assert_eq!((summary.result, summary.reason), ("Watchdog", None));
        assert_eq!(stats.counters().tests, 2);
        assert_eq!(stats.counters().test_connections, 3);
        assert_eq!(stats.counters().active_test_connections, 1);
        // A release seen twice is counted once.
        assert_eq!(stats.observe(&released(2)), None);
        let summary = stats.observe(&released(3)).unwrap();
        assert_eq!(summary.result, "NotActivated");
        assert_eq!(summary.ip_layer_sender_bit_rate, None);
        assert_eq!(stats.counters().active_test_connections, 0);
    }

    // docs/spec.md story 90 (events an attacker can trigger are
    // rate-limited);
    #[test]
    fn only_a_test_never_activated_is_unverified() {
        let mut stats = Statistics::default();
        stats.observe(&admitted(1, 5000, 9));
        stats.observe(&ServerNotice::Core(
            Event::UnactivatedTestConnectionEvicted {
                socket: SocketId(1),
            },
        ));
        let evicted = stats.observe(&released(1)).unwrap();
        assert_eq!(evicted.result, "UnactivatedTestConnectionEvicted");
        assert!(evicted.unverified());
        stats.observe(&admitted(2, 5000, 9));
        stats.observe(&ServerNotice::Core(Event::TestConnectionActivated {
            socket: SocketId(2),
            direction: Direction::Upstream,
            parameters: parameters(),
        }));
        stats.observe(&ended(2, EndReason::Completed));
        let ran = stats.observe(&released(2)).unwrap();
        assert_eq!(ran.result, "Completed");
        assert!(!ran.unverified());
    }

    //  (Statistics counts the Unactivated Test Connections evicted, so
    // an eviction storm shows even where the log limits their records)
    #[test]
    fn evictions_are_counted() {
        let mut stats = Statistics::default();
        for socket in 1..=3 {
            stats.observe(&admitted(socket, 5000, socket as u16));
            stats.observe(&ServerNotice::Core(
                Event::UnactivatedTestConnectionEvicted {
                    socket: SocketId(socket),
                },
            ));
            stats.observe(&released(socket));
        }
        assert_eq!(stats.counters().unactivated_test_connections_evicted, 3);
    }

    // : a rejection's reason is a field of its own, not part of the
    // result
    #[test]
    fn a_rejected_activation_is_the_result_with_its_reason() {
        let mut stats = Statistics::default();
        stats.observe(&admitted(1, 5000, 9));
        stats.observe(&ServerNotice::Core(Event::ActivationRequestRejected {
            socket: SocketId(1),
            reason: ActivationRejection::QuotaExceeded,
        }));
        let summary = stats.observe(&released(1)).unwrap();
        assert_eq!(
            (summary.result, summary.reason),
            ("ActivationRequestRejected", Some("QuotaExceeded"))
        );
    }

    pub(crate) fn parameters() -> TestParameters {
        TestParameters {
            test_interval: Duration::from_secs(10),
            sub_interval: Duration::from_secs(1),
            trial_interval: Duration::from_millis(50),
            low_thresh: 30,
            upper_thresh: 90,
            seq_err_thresh: 10,
            slow_adj_thresh: 3,
            high_speed_delta: 10,
            ignore_ooo_dup: true,
            one_way_delay_variation: false,
            random_payload: false,
            rate_adj_algo: 0,
            dscp: 0,
            sr_index_conf: 0,
            sr_index_is_start: false,
        }
    }

    // docs/spec.md story 81 (counters of rejections by reason, auth
    // failures, rate-limit drops and stale PDUs);
    #[test]
    fn refusals_are_counted_by_reason_and_kind() {
        let mut stats = Statistics::default();
        let from = SocketAddr::from(([192, 0, 2, 7], 5000));
        let drop =
            |reason| ServerNotice::Core(Event::SetupRequestSilentlyRejected { from, reason });
        let reject = |reason| ServerNotice::Core(Event::SetupRequestRejected { from, reason });
        let data = |event| ServerNotice::Data {
            socket: SocketId(1),
            event,
        };
        for notice in [
            drop(SilentRejectionReason::AuthenticationFailed),
            drop(SilentRejectionReason::AuthenticationFailed),
            drop(SilentRejectionReason::UnverifiedBudgetExhausted),
            drop(SilentRejectionReason::Malformed),
            reject(SetupRejection::Authentication(
                AuthenticationFailure::DigestFailed,
            )),
            reject(SetupRejection::SourceBudgetExhausted),
            reject(SetupRejection::TestConnections),
            ServerNotice::Core(Event::ActivationRequestSilentlyRejected {
                socket: SocketId(1),
                reason: SilentRejectionReason::KeyIdMismatch,
            }),
            data(DataEvent::StatusPdusFailedVerification { count: 3 }),
            data(DataEvent::StaleStatusPdus { count: 2 }),
            ServerNotice::Core(Event::ActivationRequestRejected {
                socket: SocketId(1),
                reason: ActivationRejection::CapacityExceeded,
            }),
        ] {
            stats.observe(&notice);
        }
        let c = stats.counters();
        assert_eq!(c.authentication_failures, 2 + 1 + 1 + 3);
        assert_eq!(c.rate_limited, 2);
        assert_eq!(c.stale_status_pdus, 2);
        let refusals: Vec<(&str, &str, u64)> = c
            .refusals
            .iter()
            .map(|(&(kind, reason), &count)| (kind, reason, count))
            .collect();
        // Authentication failures have one name, answered or not.
        assert_eq!(
            refusals,
            [
                ("ActivationRequestRejected", "CapacityExceeded", 1),
                ("ActivationRequestSilentlyRejected", "KeyIdMismatch", 1),
                ("SetupRequestRejected", "AuthenticationDigestFailed", 1),
                ("SetupRequestRejected", "SourceBudgetExhausted", 1),
                ("SetupRequestRejected", "TestConnections", 1),
                (
                    "SetupRequestSilentlyRejected",
                    "AuthenticationDigestFailed",
                    2
                ),
                ("SetupRequestSilentlyRejected", "Malformed", 1),
                (
                    "SetupRequestSilentlyRejected",
                    "UnverifiedBudgetExhausted",
                    1
                ),
            ]
        );
    }
}
