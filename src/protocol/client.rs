//! The Client role.

use std::collections::VecDeque;
use std::net::{IpAddr, SocketAddr};
use std::num::NonZeroU16;
use std::time::Duration;

use crate::wire::activation::{self, SR_INDEX_DEFAULT, rate_adj_algo};
use crate::wire::auth::DerivedKeys;
use crate::wire::checksum;
use crate::wire::null::{self, NULL_PDU_ID, NullPdu};
use crate::wire::setup::{self, modifier};
use crate::wire::{
    AUTH_DIGEST_LEN, ActivationPdu, PROTOCOL_VERSION, SendingRate, SetupPdu, auth_mode,
};

use crate::protocol::checksum_use;
use crate::protocol::coercion::coercions;
use crate::protocol::data_phase::{self, DataPhase, DataPhaseSetup, End};
use crate::protocol::event::{ControlExchange, SetupRejectionCode, TestParameters};
use crate::protocol::interval::IntervalCounter;
use crate::protocol::io::is_multicast_or_broadcast;
use crate::protocol::keys::{SendKeyError, auth_time_acceptable, auth_unix_time};
use crate::protocol::parameters;
use crate::protocol::server::CONTROL_WATCHDOG;
use crate::protocol::{
    ActivationParameter, ControlPhaseFailure, DatagramMeta, Direction, Endpoint, Event, IpVersion,
    KeyTable, MonoTime, Now, Output, ResponseIgnoreReason, SocketId, Transmit,
};

/// How the Client authenticates. `None` in [`ClientConfig::auth`] means
/// nothing is configured, so no Test can start.
#[derive(Debug, Clone)]
pub enum ClientAuth {
    /// Explicit opt-in to Unauthenticated Mode (not conformant to RFC 9946).
    Unauthenticated,
    /// Authentication Mode 1 (RFC 9946 §5.3.1) with the Key Table's row
    /// valid for sending now (RFC 7210 §3), limited to keyId `key_id` when
    /// it is given.
    Mode1 { keys: KeyTable, key_id: Option<u8> },
    /// Authentication Mode 2 (RFC 9946 §5.3.2): mode 1, and every Status
    /// PDU authenticated too.
    Mode2 { keys: KeyTable, key_id: Option<u8> },
}

/// The Sending Rate Table row the Client asks for (srIndexConf, RFC 9946
/// §7.1). Rows are the Server's: the Client never reads a table (§8.1).
/// A row is 0 to 1152, the Sending Rate Table's rows, as the Client's
/// options take it: 0xFFFF would be CHTA_SRIDX_DEF on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowRequest {
    /// The Server's default: the Load Rate Adjustment Algorithm starts at
    /// the first row (CHTA_SRIDX_DEF).
    ServerDefault,
    /// The Load Rate Adjustment Algorithm starts at this row
    /// (CHTA_SRIDX_ISSTART set).
    StartAt(u16),
    /// A Fixed-Rate Test at this row (CHTA_SRIDX_ISSTART clear).
    FixedRate(u16),
}

/// The Client's settings for a Test, which every Test Connection of it
/// shares: what the Client user chose. With the Test's
/// [`TestPlan`](crate::protocol::TestPlan) and its mcIdent, what the Multi-Connection
/// Coordinator runs the Test with; it decides the rest for each Test
/// Connection itself: its Server, mcIndex and mcCount, its share of the
/// plan's maxBandwidth, its time limit, and mode 1 with a Server the plan
/// names.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub direction: Direction,
    pub auth: Option<ClientAuth>,
    /// The Sending Rate Table row to start at, or to hold.
    pub row_request: RowRequest,
    /// Parameters the Server must not coerce: if it does, the Client ends
    /// the Test (RFC 9946 §7.3). Empty by default: every coercion is
    /// accepted.
    pub unacceptable_coercions: Vec<ActivationParameter>,
    /// Ask for random payload content instead of zeros (CHTA_RAND_PAYLOAD),
    /// so compression on the path cannot inflate the result.
    pub random_payload: bool,
    /// The delay variation the Load Rate Adjustment Algorithm judges by
    /// (useOwDelVar).
    pub delay_variation: DelayVariation,
    /// Checksum use (RFC 9946 §5.6): fill the optional checkSum field of
    /// every PDU sent, and verify it in every PDU received when populated.
    pub checksum: bool,
    /// CHSR_JUMBO_STATUS and CHSR_TRADITIONAL_MTU, which must match the
    /// Server's (RFC 9946 §6.1).
    pub jumbo: bool,
    pub traditional_mtu: bool,
    /// The Test Interval to ask for (testIntTime, seconds; 10 by default;
    /// the binary's `--test-interval` asks for 5 or more). The Client
    /// refuses an Activation Response whose Test Interval is below 5 s or
    /// above the longer of 60 s and this (`parameters::unrunnable`).
    pub test_int_time: u16,
    /// The Load Rate Adjustment Algorithm to ask for (rateAdjAlgo).
    pub algorithm: RateAdjAlgorithm,
    /// The DSCP to mark the Test's traffic with (dscpEcn), 0 to 63; the
    /// Server may coerce it to 0.
    pub dscp: u8,
}

/// One Test Connection's Client settings: the Test's, and what the
/// Multi-Connection Coordinator decides for it.
#[derive(Debug, Clone)]
pub(crate) struct TestConnectionConfig {
    /// The Test's settings, with mode 1 where the plan names the Server.
    pub(crate) test: ClientConfig,
    /// Identifies the Test's Test Connections (RFC 9946 §6.1).
    pub(crate) mc_ident: NonZeroU16,
    /// The Server's standard-port address.
    pub(crate) server: SocketAddr,
    /// This Server's locally authorized alternate IP addresses.
    pub(crate) alternate_server_addresses: Vec<IpAddr>,
    /// This Test Connection's index within the Test.
    pub(crate) mc_index: u8,
    /// How many Test Connections the Test has.
    pub(crate) mc_count: u8,
    /// The most this Test Connection expects to send or receive, in Mbps
    /// (maxBandwidth, RFC 9946 §6.1), at most 32767; 0 states none.
    pub(crate) max_bandwidth: u16,
    /// How long the Setup and Activation exchanges may take: the Test
    /// initiation timer, at most its 3 s (RFC 9946 §6.1).
    pub(crate) time_limit: Duration,
}

/// Which delay variation the Load Rate Adjustment Algorithm judges by
/// (useOwDelVar, RFC 9946 §7.1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DelayVariation {
    /// One-way delay variation, which needs no clock synchronisation but
    /// follows clock drift (the RFC's default).
    #[default]
    OneWay,
    /// Round-trip time variation, immune to clock drift.
    RoundTrip,
}

/// A Load Rate Adjustment Algorithm (RFC 9946 §12.3.8).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RateAdjAlgorithm {
    /// Algorithm B, the default (Y.1540 Amd 2 Annex B).
    #[default]
    B,
    /// Algorithm C (Y.1540 Amd 2 Annex B.4).
    C,
}

enum State {
    AwaitingSetupResponse,
    /// The Setup succeeded; the Multi-Connection Coordinator sends the
    /// Activation Request with the Test's others.
    SetupAccepted {
        test_port_addr: SocketAddr,
    },
    AwaitingActivationResponse {
        test_port_addr: SocketAddr,
        /// The Activation Request as sent, to find what the Server coerced.
        request: Box<ActivationPdu>,
    },
    Activated {
        /// The Test's parameters: the Activation Response's, including any
        /// the Server coerced (RFC 9946 §7.3).
        /// The Data Phase was set up from these.
        #[allow(dead_code)]
        parameters: Box<ActivationPdu>,
    },
    /// The Test was aborted while the Activation Request was on its way:
    /// an accepting Activation Response, until the Test initiation timer,
    /// is answered with a stop, so the Server sends no load.
    Aborting {
        test_port_addr: SocketAddr,
    },
    Ended,
}

/// What the Multi-Connection Coordinator must do for a Test Connection it
/// aborts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Aborted {
    /// Nothing: it has ended.
    Ended,
    /// It waits for its Activation Response, to stop the Test Connection.
    Aborting,
    /// Its Data Phase runs: stop it.
    StopDataPhase,
}

/// One Test Connection's Client side: its Setup and Activation exchanges
/// and the Data Phase they start. Only the Multi-Connection Coordinator
/// drives it, one per Test Connection, on the Test Connection's socket; the
/// Coordinator is the Client role's one endpoint.
pub(crate) struct Client {
    config: TestConnectionConfig,
    state: State,
    /// When the Test initiation timer expires (RFC 9946 §6.1).
    initiation_deadline: Option<MonoTime>,
    /// Why the last Setup Response and the last Activation Response the
    /// Client ignored were: the one of the exchange the Test initiation
    /// timer expires in explains it.
    setup_response_ignored: Option<ResponseIgnoreReason>,
    activation_response_ignored: Option<ResponseIgnoreReason>,
    /// The Derived Keys of this Test Connection in mode 1, and the keyId
    /// of the Shared Key they came from.
    keys: Option<(u8, DerivedKeys)>,
    outputs: VecDeque<Output>,
    /// The Data Phase announced but not yet taken by the runtime.
    data_phase: Option<DataPhase>,
    /// Hold the Activation Request until [`Client::activate`].
    held: bool,
    /// The Multi-Connection Test's common grid, for a Load Receiver.
    grid: Option<MonoTime>,
}

impl Client {
    /// Creates a Client and queues its first outputs (the Setup Request).
    /// # Panics
    ///
    /// If `config.test.dscp` is above 63.
    fn new(config: TestConnectionConfig, now: Now) -> Self {
        assert!(
            config.test.dscp < 64,
            "a DSCP is 0 to 63, not {}",
            config.test.dscp
        );
        let mut client = Self {
            config,
            state: State::AwaitingSetupResponse,
            initiation_deadline: None,
            setup_response_ignored: None,
            activation_response_ignored: None,
            keys: None,
            outputs: VecDeque::new(),
            data_phase: None,
            held: false,
            grid: None,
        };
        client.start(now);
        client
    }

    /// A Client whose Activation Request waits for [`Client::activate`],
    /// for the Multi-Connection Coordinator.
    pub(crate) fn held(config: TestConnectionConfig, now: Now) -> Self {
        let mut client = Self::new(config, now);
        client.held = true;
        client
    }

    /// Sends the Activation Request held since the Setup succeeded; a Load
    /// Receiver keeps its Sub-Intervals to `grid`, if given.
    pub(crate) fn activate(&mut self, now: Now, grid: Option<MonoTime>) {
        self.grid = grid;
        if let State::SetupAccepted { test_port_addr } = self.state {
            self.send_activation_request(now, test_port_addr);
        }
    }

    /// Gives the Test Connection up, and says what's left to do.
    pub(crate) fn abort(&mut self) -> Aborted {
        match self.state {
            State::AwaitingActivationResponse { test_port_addr, .. } => {
                self.state = State::Aborting { test_port_addr };
                Aborted::Aborting
            }
            State::Aborting { .. } => Aborted::Aborting,
            State::Activated { .. } => {
                self.state = State::Ended;
                Aborted::StopDataPhase
            }
            State::AwaitingSetupResponse | State::SetupAccepted { .. } | State::Ended => {
                self.state = State::Ended;
                Aborted::Ended
            }
        }
    }

    fn start(&mut self, now: Now) {
        // Started with the Setup Request, stopped by the Data Phase (RFC
        // 9946 §6.1); it reuses the Test termination timeout, 3 s.
        self.initiation_deadline = Some(MonoTime::from_duration(
            now.mono.as_duration() + self.config.time_limit.min(CONTROL_WATCHDOG),
        ));
        if is_multicast_or_broadcast(self.config.server.ip()) {
            self.state = State::Ended;
            return self.emit(Event::ControlPhaseFailed(
                ControlPhaseFailure::MulticastOrBroadcastServer,
            ));
        }
        // Above 32767 Mbps would set CHSR_USDIR_BIT: stated as the most the
        // field holds.
        let max_bandwidth = self
            .config
            .max_bandwidth
            .min(!setup::UPSTREAM_DIRECTION_BIT);
        let mut request = SetupPdu {
            protocol_version: PROTOCOL_VERSION,
            mc_index: self.config.mc_index,
            mc_count: self.config.mc_count,
            mc_ident: self.config.mc_ident.get(),
            cmd_request: setup::cmd_request::SETUP_REQUEST,
            cmd_response: setup::cmd_response::NONE,
            // CHSR_USDIR_BIT marks an Upstream Test whether or not a
            // maxBandwidth is stated (RFC 9946 §6.1).
            max_bandwidth: match self.config.test.direction {
                Direction::Upstream => max_bandwidth | setup::UPSTREAM_DIRECTION_BIT,
                Direction::Downstream => max_bandwidth,
            },
            test_port: 0,
            modifier_bitmap: if self.config.test.jumbo {
                modifier::JUMBO_STATUS
            } else {
                0
            } | if self.config.test.traditional_mtu {
                modifier::TRADITIONAL_MTU
            } else {
                0
            },
            auth_mode: auth_mode::UNAUTHENTICATED,
            auth_unix_time: 0,
            auth_digest: [0; AUTH_DIGEST_LEN],
            key_id: 0,
            checksum: 0,
        };
        match &self.config.test.auth {
            None => {
                self.state = State::Ended;
                self.emit(Event::ControlPhaseFailed(
                    ControlPhaseFailure::NoAuthenticationConfigured,
                ));
                return;
            }
            Some(ClientAuth::Unauthenticated) => {}
            Some(
                ClientAuth::Mode1 {
                    keys: table,
                    key_id,
                }
                | ClientAuth::Mode2 {
                    keys: table,
                    key_id,
                },
            ) => {
                let mode = match self.config.test.auth {
                    Some(ClientAuth::Mode2 { .. }) => auth_mode::MODE_2,
                    _ => auth_mode::MODE_1,
                };
                let entry = match table.sending(now.wall, *key_id) {
                    Ok(entry) => entry,
                    Err(error) => {
                        self.state = State::Ended;
                        self.emit(Event::ControlPhaseFailed(match error {
                            SendKeyError::None => ControlPhaseFailure::NoKeyForSending,
                            SendKeyError::Ambiguous => ControlPhaseFailure::AmbiguousKeyForSending,
                        }));
                        return;
                    }
                };
                // The keys are derived before sending, from the authUnixTime
                // this first Setup Request carries (RFC 9946 §5.4.1); the
                // Client Authentication Key signs it.
                let time = auth_unix_time(now.wall);
                let keys = entry.key.derive(time);
                request.auth_mode = mode;
                request.key_id = entry.key_id;
                request.auth_unix_time = time;
                request.sign(keys.client());
                self.keys = Some((entry.key_id, keys));
            }
        }
        self.send(self.config.server, request.encode().to_vec());
    }

    /// The Activation Request for this Client's Test, signed in mode 1.
    /// Apart from the row, the Test parameters are RFC 9946 / TR-471
    /// defaults for now; the ticket that adds the parameter table and Client
    /// options makes them configurable.
    fn activation_request(&self, now: Now) -> ActivationPdu {
        let (sr_index_conf, modifier_bitmap) = match self.config.test.row_request {
            RowRequest::ServerDefault => (SR_INDEX_DEFAULT, 0),
            RowRequest::StartAt(row) => (row, activation::modifier::SR_INDEX_IS_START),
            RowRequest::FixedRate(row) => (row, 0),
        };
        let modifier_bitmap = if self.config.test.random_payload {
            modifier_bitmap | activation::modifier::RANDOM_PAYLOAD
        } else {
            modifier_bitmap
        };
        let mut request = ActivationPdu {
            protocol_version: PROTOCOL_VERSION,
            cmd_request: self.config.test.direction.activation_cmd_request(),
            cmd_response: activation::cmd_response::NONE,
            low_thresh: parameters::LOW_THRESH.default,
            upper_thresh: parameters::UPPER_THRESH.default,
            trial_int: parameters::TRIAL_INT.default,
            test_int_time: self.config.test.test_int_time,
            dscp_ecn: self.config.test.dscp << 2,
            sr_index_conf,
            use_ow_del_var: match self.config.test.delay_variation {
                DelayVariation::OneWay => 1,
                DelayVariation::RoundTrip => 0,
            },
            high_speed_delta: octet(parameters::HIGH_SPEED_DELTA.default),
            slow_adj_thresh: parameters::SLOW_ADJ_THRESH.default,
            seq_err_thresh: parameters::SEQ_ERR_THRESH.default,
            ignore_ooo_dup: octet(parameters::IGNORE_OOO_DUP.default),
            modifier_bitmap,
            rate_adj_algo: match self.config.test.algorithm {
                RateAdjAlgorithm::B => rate_adj_algo::B,
                RateAdjAlgorithm::C => rate_adj_algo::C,
            },
            // Zero in a request (RFC 9946 §7.1).
            sending_rate: SendingRate::default(),
            sub_int_period: parameters::SUB_INT_PERIOD.default,
            auth_mode: auth_mode::UNAUTHENTICATED,
            auth_unix_time: 0,
            auth_digest: [0; AUTH_DIGEST_LEN],
            key_id: 0,
            checksum: 0,
        };
        if let Some((key_id, keys)) = &self.keys {
            // The Setup's authMode and keyId, the current time, and the
            // Client Authentication Key (RFC 9946 §5.4.1).
            request.auth_mode = self.auth_mode();
            request.key_id = *key_id;
            request.auth_unix_time = auth_unix_time(now.wall);
            request.sign(keys.client());
        }
        request
    }

    /// Message Verification Procedure steps 3 to 5 for a response in mode 1
    /// (RFC 9946 §6.2): authMode, the digest with the Server Authentication
    /// Key, then the time window. A response that fails is ignored, since it
    /// may not be the Server's.
    fn response_ignore_reason(
        &self,
        auth_mode: u8,
        verifies: impl FnOnce(&[u8; 32]) -> bool,
        auth_unix_time: u32,
        now: Now,
    ) -> Option<ResponseIgnoreReason> {
        // Unauthenticated, only authMode 0 is appropriate (RFC 9946 §6.2).
        let Some((_, keys)) = self.keys.as_ref() else {
            return (auth_mode != auth_mode::UNAUTHENTICATED)
                .then_some(ResponseIgnoreReason::AuthenticationFailed);
        };
        // The authMode the Setup Request stated (RFC 9946 §6.2).
        if auth_mode != self.auth_mode() || !verifies(keys.server()) {
            Some(ResponseIgnoreReason::AuthenticationFailed)
        } else if !auth_time_acceptable(auth_unix_time, now.wall) {
            Some(ResponseIgnoreReason::AuthTimeOutOfWindow)
        } else {
            None
        }
    }

    /// A datagram on the primary socket from `source` while waiting for the
    /// Setup Response. Only the Server's own address is trusted without a
    /// verified digest: anything else that fails verification is ignored
    /// without a trace.
    fn handle_setup_response(&mut self, now: Now, payload: &[u8], source: SocketAddr) {
        let from_server = source == self.config.server;
        let Ok(response) = SetupPdu::decode(payload) else {
            return;
        };
        // The protocol version, PDU ID (checked by decoding) and cmdRequest
        // come first (RFC 9946 §6.3). A BADVER rejection carries the
        // Server's version instead of ours (§6.2.1), so its version is
        // whatever the Server expects. (conformance: setup-response-protocol-version)
        let version_ok = response.protocol_version == PROTOCOL_VERSION
            || response.cmd_response == setup::cmd_response::BAD_VERSION;
        let ours = version_ok
            && response.cmd_request == setup::cmd_request::SETUP_RESPONSE
            && response.mc_ident == self.config.mc_ident.get()
            && response.mc_index == self.config.mc_index;
        // Message Verification Procedure (RFC 9946 §6.3), the checksum
        // first (§5.6).
        if !checksum_use::passes(self.config.test.checksum, response.checksum, || {
            checksum::control_valid(payload)
        }) {
            if !from_server {
                return;
            }
            return self.emit(Event::SetupResponseIgnored {
                reason: ResponseIgnoreReason::ChecksumFailed,
            });
        }
        let reason = self.response_ignore_reason(
            response.auth_mode,
            |key| SetupPdu::verify_received(payload, key),
            response.auth_unix_time,
            now,
        );
        if let Some(reason) = reason {
            // From elsewhere, only a response whose digest verified counts;
            // one that is merely out of time still says the clocks differ.
            if !from_server && reason != ResponseIgnoreReason::AuthTimeOutOfWindow {
                return;
            }
            self.emit(Event::SetupResponseIgnored { reason });
            // Ignored, but its code may explain the failure, as a
            // troubleshooting Server's unsigned rejection does.
            // (conformance: unverified-setup-rejection-hint)
            if ours && from_server && response.cmd_response != setup::cmd_response::ACK_OK {
                self.emit(Event::UnverifiedSetupRejection {
                    cmd_response: response.cmd_response,
                    protocol_version: response.protocol_version,
                    unsigned: response.auth_digest == [0; AUTH_DIGEST_LEN],
                });
                // A Server without mode 2 can't derive the keys to sign its
                // AUTHINV (OB-UDPST signs it with keys it never derived):
                // say so, but don't fall back to mode 1 (docs/spec.md story 37).
                if response.cmd_response == setup::cmd_response::AUTH_MODE_INVALID
                    && self.auth_mode() == auth_mode::MODE_2
                {
                    self.emit(Event::AuthMode2Unsupported);
                }
            }
            return;
        }
        // An authentic rejection ends the Setup attempt (RFC 9946 §6.3),
        // and so does a code §12.3.5 doesn't define.
        // (conformance: undefined-setup-cmd-response)
        if ours && response.cmd_response != setup::cmd_response::ACK_OK {
            self.state = State::Ended;
            let failure = match SetupRejectionCode::from_response(
                response.cmd_response,
                response.protocol_version,
            ) {
                Some(code) => ControlPhaseFailure::SetupRejected(code),
                None => ControlPhaseFailure::UnknownSetupResponse {
                    cmd_response: response.cmd_response,
                },
            };
            return self.emit(Event::ControlPhaseFailed(failure));
        }
        if ours && response.cmd_response == setup::cmd_response::ACK_OK && response.test_port != 0 {
            self.emit(Event::SetupAccepted {
                test_port: response.test_port,
            });
            // The Activation Request goes to the Test Connection's ephemeral
            // port, not the Standard Port (RFC 9946 §7.1), on an address
            // authorized before receiving the response. Preserve the
            // locally selected IPv6 scope, never response metadata.
            // (conformance: setup-response-from-another-address)
            let mut test_port_addr = self.config.server;
            test_port_addr.set_ip(source.ip());
            test_port_addr.set_port(response.test_port);
            if self.held {
                self.state = State::SetupAccepted { test_port_addr };
            } else {
                self.send_activation_request(now, test_port_addr);
            }
        }
    }

    /// Authorization is independent of the response's contents. An
    /// alternate keeps the selected Server's port, family and IPv6 scope.
    fn setup_source_authorized(&self, source: SocketAddr) -> bool {
        if source == self.config.server {
            return true;
        }
        let mut expected = self.config.server;
        expected.set_ip(source.ip());
        self.keys.is_some()
            && source.is_ipv4() == self.config.server.is_ipv4()
            && source == expected
            && !source.ip().is_unspecified()
            && self
                .config
                .alternate_server_addresses
                .contains(&source.ip())
    }

    fn send_activation_request(&mut self, now: Now, test_port_addr: SocketAddr) {
        let request = self.activation_request(now);
        self.send(test_port_addr, request.encode().to_vec());
        self.state = State::AwaitingActivationResponse {
            test_port_addr,
            request: Box::new(request),
        };
    }

    /// An Activation Response to an aborted Test: an authentic acceptance
    /// is stopped at once.
    fn handle_aborted_activation_response(
        &mut self,
        now: Now,
        test_port_addr: SocketAddr,
        payload: &[u8],
    ) {
        let Ok(response) = ActivationPdu::decode(payload) else {
            return;
        };
        if !checksum_use::passes(self.config.test.checksum, response.checksum, || {
            checksum::control_valid(payload)
        }) {
            return;
        }
        let authentic = self
            .response_ignore_reason(
                response.auth_mode,
                |key| ActivationPdu::verify_received(payload, key),
                response.auth_unix_time,
                now,
            )
            .is_none();
        if authentic
            && response.cmd_request == self.config.test.direction.activation_cmd_request()
            && response.cmd_response == activation::cmd_response::ACK_OK
        {
            self.stop_at_once(now, test_port_addr);
            self.state = State::Ended;
        }
    }

    /// A Null Request from the Test Connection's port: verified if it
    /// arrives (RFC 9946 §6.2.2), never answered, and nothing depends on
    /// it. One that arrives before the Setup Response can't be told from
    /// stray traffic, and is ignored.
    fn handle_null_request(&mut self, now: Now, payload: &[u8]) {
        let Ok(null) = NullPdu::decode(payload) else {
            return;
        };
        if !checksum_use::passes(self.config.test.checksum, null.checksum, || {
            checksum::control_valid(payload)
        }) {
            return self.emit(Event::NullRequestIgnored {
                reason: ResponseIgnoreReason::ChecksumFailed,
            });
        }
        let reason = self.response_ignore_reason(
            null.auth_mode,
            |key| NullPdu::verify_received(payload, key),
            null.auth_unix_time,
            now,
        );
        match reason {
            Some(reason) => self.emit(Event::NullRequestIgnored { reason }),
            None if null.cmd_request == null::cmd_request::NULL_REQUEST => {
                self.emit(Event::NullRequestReceived);
            }
            None => {}
        }
    }

    fn handle_activation_response(
        &mut self,
        now: Now,
        test_port_addr: SocketAddr,
        request: &ActivationPdu,
        payload: &[u8],
    ) {
        let Ok(response) = ActivationPdu::decode(payload) else {
            return;
        };
        // Message Verification Procedure (RFC 9946 §7.3), the checksum
        // first (§5.6).
        if !checksum_use::passes(self.config.test.checksum, response.checksum, || {
            checksum::control_valid(payload)
        }) {
            return self.emit(Event::ActivationResponseIgnored {
                reason: ResponseIgnoreReason::ChecksumFailed,
            });
        }
        let reason = self.response_ignore_reason(
            response.auth_mode,
            |key| ActivationPdu::verify_received(payload, key),
            response.auth_unix_time,
            now,
        );
        if let Some(reason) = reason {
            return self.emit(Event::ActivationResponseIgnored { reason });
        }
        if response.cmd_request != self.config.test.direction.activation_cmd_request() {
            return;
        }
        match response.cmd_response {
            activation::cmd_response::ACK_OK => {
                let mut refused = None;
                for coercion in coercions(request, &response) {
                    self.emit(Event::ParameterCoerced { coercion });
                    if self
                        .config
                        .test
                        .unacceptable_coercions
                        .contains(&coercion.parameter)
                    {
                        refused.get_or_insert(coercion);
                    }
                }
                if let Some(parameter) = parameters::unrunnable(&response, request) {
                    self.stop_at_once(now, test_port_addr);
                    self.state = State::Ended;
                    return self.emit(Event::ControlPhaseFailed(
                        ControlPhaseFailure::UnrunnableParameter {
                            parameter,
                            value: parameter.value(&response),
                        },
                    ));
                }
                if let Some(coercion) = refused {
                    self.stop_at_once(now, test_port_addr);
                    self.state = State::Ended;
                    return self.emit(Event::ControlPhaseFailed(
                        ControlPhaseFailure::UnacceptableCoercion(coercion),
                    ));
                }
                self.emit(Event::TestActivated {
                    direction: self.config.test.direction,
                    parameters: TestParameters::of(&response),
                });
                self.start_data_phase(now, test_port_addr, &response);
                self.state = State::Activated {
                    parameters: Box::new(response),
                };
            }
            activation::cmd_response::BAD_PARAM => {
                self.state = State::Ended;
                self.emit(Event::ActivationRejected);
            }
            // A code RFC 9946 §12.3.9 does not define: the Client MUST
            // terminate the connection (§7.3).
            cmd_response => {
                self.state = State::Ended;
                self.emit(Event::ControlPhaseFailed(
                    ControlPhaseFailure::UnknownActivationResponse { cmd_response },
                ));
            }
        }
    }

    /// Ends an activated Test before its Data Phase starts: a PDU marked
    /// STOP2 tells the Server at once (RFC 9946 §9 defines only a
    /// Server-initiated stop; see `DataPhase::stop`).
    fn stop_at_once(&mut self, now: Now, test_port_addr: SocketAddr) {
        match self.config.test.direction {
            Direction::Downstream => {
                let signer = self
                    .keys
                    .as_ref()
                    .filter(|_| self.auth_mode() == auth_mode::MODE_2)
                    .map(|(key_id, keys)| (*key_id, *keys.client()));
                let pdu = data_phase::stop_status_pdu(
                    now,
                    self.auth_mode(),
                    signer,
                    self.config.test.checksum,
                );
                self.send(test_port_addr, pdu.to_vec());
            }
            Direction::Upstream => {
                let pdu = data_phase::stop_load_pdu(now, self.config.test.checksum);
                self.send(test_port_addr, pdu.to_vec());
            }
        }
    }

    /// The Test Connection's authMode.
    fn auth_mode(&self) -> u8 {
        match (&self.config.test.auth, &self.keys) {
            (Some(ClientAuth::Mode2 { .. }), Some(_)) => auth_mode::MODE_2,
            (_, Some(_)) => auth_mode::MODE_1,
            _ => auth_mode::UNAUTHENTICATED,
        }
    }

    /// Hands the Test Connection to the Data Phase, with the parameters of
    /// the Activation Response: in a Downstream Test the Client is the Load
    /// Receiver, in an Upstream Test the Load Sender, executing the Server's
    /// Sending Rate Structure (RFC 9946 §8.1).
    fn start_data_phase(&mut self, now: Now, peer: SocketAddr, response: &ActivationPdu) {
        let setup = DataPhaseSetup {
            now,
            peer,
            // The sender SHALL set ECN to Not-ECT (RFC 9946 §7.1), whatever
            // the Server answered; the coercion event shows its answer.
            dscp_ecn: response.dscp_ecn & !0b11,
            seed: data_phase::seed(
                now,
                u64::from(self.config.mc_ident.get()) << 8 | u64::from(self.config.mc_index),
            ),
            checksum: self.config.test.checksum,
            keys: self.keys.as_ref().map(|(_, keys)| keys.clone()),
            key_id: self.keys.as_ref().map_or(0, |(key_id, _)| *key_id),
            auth_mode: self.auth_mode(),
            test_interval: Duration::from_secs(response.test_int_time.into()),
            trial_interval: Duration::from_millis(response.trial_int.into()),
            sub_interval: Duration::from_millis(response.sub_int_period.into()),
            this_end: End::Client,
            thresholds: None,
            algorithm: None,
            // Whether the search ever confirmed Congestion.
            counter: Some(IntervalCounter::accepted(response)),
            grid: self.grid,
            max_payload: match (self.config.test.jumbo, self.config.test.traditional_mtu) {
                (true, _) => crate::protocol::sending_rate_table::MAX_JUMBO_IP_PACKET - 28,
                (false, true) => 1472,
                (false, false) => 1222,
            },
            max_bandwidth_kbps: (self.config.max_bandwidth > 0)
                .then(|| u64::from(self.config.max_bandwidth) * 1000),
        };
        let data_phase = match self.config.test.direction {
            Direction::Downstream => DataPhase::load_receiver(
                setup,
                Duration::from_millis(response.sub_int_period.into()),
                // Zeros from a Client (RFC 9946 §8.2).
                SendingRate::default(),
            ),
            Direction::Upstream => {
                let random_payload =
                    response.modifier_bitmap & activation::modifier::RANDOM_PAYLOAD != 0;
                DataPhase::load_sender(
                    setup,
                    &response.sending_rate,
                    IpVersion::of(peer),
                    random_payload,
                )
            }
        };
        self.data_phase = Some(data_phase);
        self.outputs.push_back(Output::StartDataPhase {
            socket: SocketId::PRIMARY,
            peer,
        });
    }

    /// Sends a control PDU, its checkSum filled last when checksum use is
    /// configured (RFC 9946 §5.6).
    fn send(&mut self, destination: SocketAddr, mut payload: Vec<u8>) {
        if self.config.test.checksum {
            checksum::fill_control(&mut payload);
        }
        self.outputs.push_back(Output::Transmit(Transmit {
            socket: SocketId::PRIMARY,
            destination,
            payload,
            source: None,
        }));
    }

    fn emit(&mut self, event: Event) {
        // The last response of each kind ignored, for the Test initiation
        // timer.
        match event {
            Event::SetupResponseIgnored { reason } => self.setup_response_ignored = Some(reason),
            Event::ActivationResponseIgnored { reason } => {
                self.activation_response_ignored = Some(reason);
            }
            _ => {}
        }
        self.outputs.push_back(Output::Event(event));
    }
}

/// A one-octet parameter's default.
fn octet(value: u16) -> u8 {
    u8::try_from(value).expect("one-octet defaults fit an octet")
}

impl Endpoint for Client {
    fn handle_datagram(&mut self, now: Now, meta: DatagramMeta, payload: &[u8]) {
        if meta.socket != SocketId::PRIMARY {
            return;
        }
        match &self.state {
            // Authentication covers the PDU, not its source address. Only
            // a locally selected Server may choose the Ephemeral Port.
            // (conformance: setup-response-from-another-address)
            State::AwaitingSetupResponse if self.setup_source_authorized(meta.source) => {
                // Dropped silently, before its contents are looked at (RFC
                // 9946 §6). The runtime gives where it arrived where the OS
                // says (Linux, macOS; ), else the bound wildcard
                // address, and then only the source is checked.
                if is_multicast_or_broadcast(meta.local.ip())
                    || is_multicast_or_broadcast(meta.source.ip())
                {
                    if meta.source != self.config.server {
                        return;
                    }
                    return self.emit(Event::SetupResponseIgnored {
                        reason: ResponseIgnoreReason::MulticastOrBroadcast,
                    });
                }
                self.handle_setup_response(now, payload, meta.source);
            }
            State::AwaitingActivationResponse { test_port_addr, .. }
                if meta.source == *test_port_addr
                    && payload.starts_with(&NULL_PDU_ID.to_be_bytes()) =>
            {
                self.handle_null_request(now, payload);
            }
            State::AwaitingActivationResponse {
                test_port_addr,
                request,
            } if meta.source == *test_port_addr => {
                let (test_port_addr, request) = (*test_port_addr, request.clone());
                self.handle_activation_response(now, test_port_addr, &request, payload);
            }
            State::Aborting { test_port_addr } if meta.source == *test_port_addr => {
                let test_port_addr = *test_port_addr;
                self.handle_aborted_activation_response(now, test_port_addr, payload);
            }
            _ => {}
        }
    }

    fn handle_socket_opened(&mut self, _now: Now, _socket: SocketId, _local: SocketAddr) {}

    fn handle_socket_closed(&mut self, _now: Now, _socket: SocketId) {}

    /// The Test initiation timer (RFC 9946 §6.1).
    fn handle_timeout(&mut self, now: Now) {
        let due = self.initiation_deadline.is_some_and(|at| at <= now.mono);
        if due && matches!(self.state, State::Aborting { .. }) {
            self.state = State::Ended;
            self.initiation_deadline = None;
            return;
        }
        // The exchange the Control Phase is in, and the last response of
        // its kind ignored: a Setup Response accepted starts the Activation
        // exchange, even while the Multi-Connection Coordinator holds the
        // Activation Request, and a Setup Response ignored no longer
        // explains the timer.
        let (exchange, last_ignored) = match self.state {
            State::AwaitingSetupResponse => (ControlExchange::Setup, self.setup_response_ignored),
            State::SetupAccepted { .. } | State::AwaitingActivationResponse { .. } => (
                ControlExchange::Activation,
                self.activation_response_ignored,
            ),
            State::Activated { .. } | State::Aborting { .. } | State::Ended => return,
        };
        if due {
            self.state = State::Ended;
            self.initiation_deadline = None;
            self.emit(Event::ControlPhaseFailed(
                ControlPhaseFailure::TestInitiationTimedOut {
                    exchange,
                    last_ignored,
                },
            ));
        }
    }

    fn poll_timeout(&self) -> Option<MonoTime> {
        match self.state {
            State::AwaitingSetupResponse
            | State::SetupAccepted { .. }
            | State::AwaitingActivationResponse { .. }
            | State::Aborting { .. } => self.initiation_deadline,
            _ => None,
        }
    }

    fn poll_output(&mut self) -> Option<Output> {
        self.outputs.pop_front()
    }

    fn take_data_phase(&mut self, socket: SocketId) -> Option<DataPhase> {
        (socket == SocketId::PRIMARY)
            .then(|| self.data_phase.take())
            .flatten()
    }
}
