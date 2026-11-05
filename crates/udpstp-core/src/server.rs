//! The Server role.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use udpstp_wire::activation::{self, SR_INDEX_DEFAULT, rate_adj_algo};
use udpstp_wire::auth::DerivedKeys;
use udpstp_wire::setup::modifier;
use udpstp_wire::setup::{cmd_request, cmd_response};
use udpstp_wire::{
    AUTH_DIGEST_LEN, ActivationPdu, NullPdu, PROTOCOL_VERSION, SendingRate, SetupPdu, auth_mode,
    null,
};

use crate::DataEvent;
use crate::admission::{Admission, Admitted, Closed, Key, Refusal, Setup, Source};
use crate::algorithm::RateAdjustment;
use crate::budget::{Budgets, DerivedKeyCache, PerSecond, Spend};
use crate::checksum_use;
use crate::coercion::coercions;
use crate::data_phase::{self, DataPhase, DataPhaseSetup, End};
use crate::event::{AuthenticationFailure, SetupRejection, TestParameters};
use crate::interval::{DelayVarStatistic, Thresholds};
use crate::io::is_multicast_or_broadcast;
use crate::keys::{KeyEntry, KeyLookupError, KeyRole, auth_time_acceptable, auth_unix_time};
use crate::parameters::{self, Allowance, DscpSet};
use crate::quota::QuotaLimits;
use crate::replay::{self, ReplayCache, Sight};
use crate::sending_rate_table::{SendingRateTable, TableShape};
use crate::{
    ActivationParameter, ActivationRejection, DatagramMeta, Direction, Endpoint, Event, KeyTable,
    MonoTime, Now, Output, SetupIgnoreReason, SilentRejectionReason, SocketId, Transmit,
};
use udpstp_wire::checksum;

#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Explicit opt-in to unauthenticated Tests (not conformant to RFC 9946).
    pub allow_unauthenticated: bool,
    /// Shared Keys for authenticated Tests.
    pub keys: KeyTable,
    /// Checksum use (RFC 9946 §5.6): fill the optional checkSum field of
    /// every PDU sent, and verify it in every PDU received when populated.
    pub checksum: bool,
    /// How many Tests may run at once from one Source (an IPv4 address,
    /// or an IPv6 /64). RFC 9946 §4.2 says one; more is a Deviation, for
    /// Clients behind carrier-grade NAT.
    pub tests_per_source: usize,
    /// How many Test Connections activated Tests may reserve at once
    /// (RFC 9946 §11 item 4), including their unactivated siblings. The
    /// Ephemeral Port range must hold this plus the unactivated share.
    pub max_test_connections: usize,
    /// How many Test Connections one Test may have over its life.
    pub test_connections_per_test: usize,
    /// How many Test Connections may be admitted but not yet activated at
    /// once; a new Setup beyond it closes the oldest of the Key Table row
    /// holding the most, so Setup Requests never activated can't
    /// fill `max_test_connections`. Separate from that cap; `None`: half
    /// of it. Never below `test_connections_per_test`, so a whole Test
    /// can be set up.
    pub max_unactivated_test_connections: Option<usize>,
    /// The most maxBandwidth admitted at once in each direction, in Mbps,
    /// summed over the Test Connections that state one (RFC 9946 §6.1);
    /// `None` for no cap.
    pub admitted_rate_cap: Option<u32>,
    /// The most of the Admitted Rate Cap, in percent, that one Test
    /// without an Operator Key may state, summed over its Test
    /// Connections: an activated Test holds its whole maxBandwidth for its
    /// whole Test Interval, whatever it sends, so one Test mustn't hold it
    /// all. A Setup past it is refused with CHSR_CRSP_CAPEXC.
    pub max_test_share: u8,
    /// Refuse a Setup Request that states no maxBandwidth, with
    /// CHSR_CRSP_NOMAXBW.
    pub require_max_bandwidth: bool,
    /// How many Setup Requests a second may reach authentication, counted
    /// once the keyId's row is found; the rest are dropped silently, before
    /// any KDF run or HMAC.
    pub unverified_setups_per_second: u32,
    /// How many authentic Setup Requests one Source may send at once: at
    /// least `test_connections_per_test`, so a Multi-Connection Test fits.
    pub source_setup_burst: u32,
    /// How many more a Source may send each second after its burst.
    pub source_setups_per_second: u32,
    /// How many authentic Setup Requests one Key Table row may send at
    /// once, Unauthenticated Mode counting as one row: at least
    /// `test_connections_per_test`. Spent after the HMAC and the replay
    /// check, so a key holder's flood costs at most this many admissions,
    /// and the rest no more than a forged request.
    pub key_setup_burst: u32,
    /// How many more a Key Table row may send each second after its burst.
    pub key_setups_per_second: u32,
    /// Troubleshooting mode (RFC 9946 §6.2.1; docs/spec.md story 76): answer
    /// authentication failures of Setup Requests with an unsigned Setup
    /// Response carrying the reason, instead of silence. Off by default;
    /// while on, the Server warns every minute.
    pub troubleshooting: bool,
    /// Which statistic of one-way delay variation classifies a Trial
    /// Interval.
    pub delay_var: DelayVarStatistic,
    /// The DSCPs a Test without an Operator Key may mark its traffic with;
    /// another is coerced to 0 (RFC 9946 §7.2.1). Only 0 by default: a
    /// Consumer mustn't put a Test's load into a priority class. A runtime
    /// that can't mark traffic allows only 0 here and below, so the
    /// Activation Response never promises a marking that isn't applied.
    pub allowed_dscp: DscpSet,
    /// The DSCPs a Test with an Operator Key may mark its traffic with:
    /// all but network control (CS6, CS7) by default.
    pub operator_allowed_dscp: DscpSet,
    /// The highest Sending Rate Table row a Test without an Operator Key
    /// may start its search at (srIndexConf with CHTA_SRIDX_ISSTART); one
    /// above is coerced to it (RFC 9946 §7.2.1; ). Operator Keys are
    /// exempt, as they may hold any row in a Fixed-Rate Test (§4.1).
    pub max_start_row: u16,
    /// The Setup modifiers this Server serves, CHSR_JUMBO_STATUS and
    /// CHSR_TRADITIONAL_MTU, and its Server MTU: they shape its Sending Rate
    /// Table, and a Setup Request with other modifiers is refused (RFC
    /// 9946 §6.1, §6.2.1; ).
    pub table: TableShape,
    /// The period Quotas are counted over.
    pub quota_period: Duration,
    /// Each Source's Quota: Tests and load bytes per Quota Period; none by
    /// default. A Key Table row can have its own (`KeyEntry::quota`).
    /// Operator Keys are exempt (RFC 9946 §11 item 6).
    pub source_quota: QuotaLimits,
}

/// The Quota Period by default: an hour.
pub const DEFAULT_QUOTA_PERIOD: Duration = Duration::from_secs(3600);

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            allow_unauthenticated: false,
            keys: KeyTable::default(),
            checksum: false,
            delay_var: DelayVarStatistic::default(),
            tests_per_source: 1,
            max_test_connections: DEFAULT_MAX_TEST_CONNECTIONS,
            test_connections_per_test: DEFAULT_TEST_CONNECTIONS_PER_TEST,
            max_unactivated_test_connections: None,
            admitted_rate_cap: None,
            max_test_share: DEFAULT_MAX_TEST_SHARE,
            require_max_bandwidth: false,
            troubleshooting: false,
            unverified_setups_per_second: DEFAULT_UNVERIFIED_SETUPS_PER_SECOND,
            source_setup_burst: DEFAULT_SOURCE_SETUP_BURST,
            source_setups_per_second: DEFAULT_SOURCE_SETUPS_PER_SECOND,
            key_setup_burst: DEFAULT_KEY_SETUP_BURST,
            key_setups_per_second: DEFAULT_KEY_SETUPS_PER_SECOND,
            allowed_dscp: DscpSet::DEFAULT_ONLY,
            operator_allowed_dscp: DscpSet::ALL_BUT_NETWORK_CONTROL,
            max_start_row: DEFAULT_MAX_START_ROW,
            table: TableShape::default(),
            quota_period: DEFAULT_QUOTA_PERIOD,
            source_quota: QuotaLimits::default(),
        }
    }
}

/// The share of the Admitted Rate Cap one Test may hold by default, in
/// percent: half, so at least two Tests can run whatever one states
/// (conformance: max-test-share-of-the-rate-cap).
pub const DEFAULT_MAX_TEST_SHARE: u8 = 50;

/// The highest row a Test without an Operator Key may start its search at
/// by default: 100 Mbps. Below the gigabit row the algorithms keep Fast
/// Mode and their large step down, so a search started too high on a
/// slow link drains in about 3 s; from row 1000 or above Algorithm B
/// would step down one row a Trial Interval, some 45 s from 1 Gbps to 100
/// Mbps, flooding the link for the whole Test (RFC 9946 §4.2, §11 item 5).
/// Starting at 100 saves little over row 0: Algorithm C reaches 1 Gbps in
/// about 1 s (conformance: consumer-start-row-capped).
pub const DEFAULT_MAX_START_ROW: u16 = 100;

/// A Source's burst by default: two whole Multi-Connection Tests of the
/// default per-Test cap.
pub const DEFAULT_SOURCE_SETUP_BURST: u32 = 2 * DEFAULT_TEST_CONNECTIONS_PER_TEST as u32;

/// A Source's refill by default: a new 24-connection Test about every 5 s,
/// beyond the burst.
pub const DEFAULT_SOURCE_SETUPS_PER_SECOND: u32 = 5;

/// A Key Table row's burst by default: a second's worth of its refill.
pub const DEFAULT_KEY_SETUP_BURST: u32 = DEFAULT_KEY_SETUPS_PER_SECOND;

/// A Key Table row's refill by default: about 40 Tests of the default
/// per-Test cap a second, far above a key's Clients' own use, and about a
/// twentieth of the admissions a 2017 laptop CPU's control thread can do,
/// so one key's flood leaves it room.
pub const DEFAULT_KEY_SETUPS_PER_SECOND: u32 = 1000;

/// The global budget for unverified Setup Requests by default, which the
/// maintainer chose to hold the checking work of forged Setup
/// Requests to at most about 10 % of one slow core. Measured
///: a request with a new
/// authUnixTime outside the window, the worst case, costs the core 3.2 µs
/// on an Intel Core i7-8550U without SHA extensions and 0.5 µs on an AMD
/// Ryzen 7 9800X3D with them, so that work takes at most 6.4 % and 1 % of
/// one core, leaving room for a host about one and a half times slower
/// than the i7-8550U at its turbo clock. About 13 Mbit/s of forged
/// requests exceeds it, so it bounds the CPU rather than keeping Clients
/// in during a flood.
pub const DEFAULT_UNVERIFIED_SETUPS_PER_SECOND: u32 = 20_000;

/// How often the Server warns that Troubleshooting Mode is on.
const TROUBLESHOOTING_WARNING_EVERY: Duration = Duration::from_secs(60);

/// The most authentication failures Troubleshooting Mode answers a second;
/// the rest stay silent.
const TROUBLESHOOTING_REPLIES_PER_SECOND: u32 = 10;

/// The global Test Connection cap by default, as in OB-UDPST.
pub const DEFAULT_MAX_TEST_CONNECTIONS: usize = 256;

/// The per-Test cap by default (docs/spec.md), OB-UDPST's largest mcCount.
pub const DEFAULT_TEST_CONNECTIONS_PER_TEST: usize = 24;

/// The Test initiation timer and the Activation Watchdog: the watchdog's
/// 1 s plus the 2 s to termination (RFC 9946 §6), the same on both ends
/// (§6.2.2), as in OB-UDPST.
pub(crate) const CONTROL_WATCHDOG: Duration = Duration::from_secs(3);

/// The Server's own data about an admitted Test Connection, which
/// [`Admission`] keeps with its place.
struct Connection {
    /// The address its Setup Request arrived on, which its socket is bound
    /// to.
    local: IpAddr,
    /// Its Setup Request.
    request: SetupPdu,
    /// How the Setup was authenticated; `None` in Unauthenticated Mode.
    auth: Option<ConnectionAuth>,
    /// Whether its socket was connected to the Client, on the first
    /// Activation Request that passed verification.
    connected: bool,
    /// When the Activation Watchdog closes it unless a valid Activation
    /// Request has come: from its Setup Response on; `None` before, once
    /// one has come, or once it expired.
    watchdog: Option<MonoTime>,
}

/// An authenticated Test Connection's authMode, keyId and Derived Keys,
/// fixed by its Setup Request and kept for the rest of the Control Phase
/// (RFC 9946 §5.4.1), and what its Key Table row allows. A row whose
/// lifetime ends later doesn't affect the Test Connection (RFC 7210 §3).
struct ConnectionAuth {
    auth_mode: u8,
    key_id: u8,
    keys: DerivedKeys,
    /// Whether the row is an Operator Key, with its own maximum Test
    /// Interval.
    role: KeyRole,
    /// The row's position in the Key Table, which has its Quota.
    row: usize,
}

impl ConnectionAuth {
    fn is_operator(&self) -> bool {
        matches!(self.role, KeyRole::Operator { .. })
    }

    /// What the key allows beyond the ordinary parameter ranges.
    fn allowance(&self) -> Allowance {
        match self.role {
            KeyRole::Operator { max_test_interval } => Allowance {
                max_test_interval: max_test_interval
                    .map_or(Allowance::CONSUMER.max_test_interval, |d| {
                        u16::try_from(d.as_secs()).unwrap_or(u16::MAX)
                    }),
                operator: true,
                // Set from the Server's configuration.
                dscp: DscpSet::DEFAULT_ONLY,
            },
            KeyRole::Consumer => Allowance::CONSUMER,
        }
    }

    fn new(request: &SetupPdu, row: usize, entry: &KeyEntry, keys: DerivedKeys) -> Self {
        Self {
            auth_mode: request.auth_mode,
            key_id: request.key_id,
            keys,
            role: entry.role,
            row,
        }
    }
}

/// How full one of the Server's tables is: its entries, and the most it
/// may hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableUse {
    pub entries: usize,
    pub limit: usize,
}

impl TableUse {
    fn new((entries, limit): (usize, usize)) -> Self {
        Self { entries, limit }
    }
}

/// The Server's state, table by table. However hostile its input, each
/// stays within its limit, and once every Test Connection has ended none
/// holds Admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerFootprint {
    /// Admitted Test Connections, waiting for their socket or set up.
    pub test_connections: TableUse,
    /// Tests with a set-up Test Connection.
    pub tests: TableUse,
    /// Data Phases announced but not yet taken by the runtime.
    pub data_phases: TableUse,
    /// Verified requests remembered to refuse their replays.
    pub replay_cache: TableUse,
    /// Sources with a budget of Setup Requests.
    pub source_budgets: TableUse,
    /// Derived Keys cached by Key Table row and authUnixTime.
    pub derived_keys: TableUse,
    /// Sources with Quota use.
    pub source_quotas: TableUse,
    /// Key Table rows with Quota use.
    pub key_quotas: TableUse,
}

impl ServerFootprint {
    /// Every table, by name.
    pub fn tables(&self) -> [(&'static str, TableUse); 8] {
        [
            ("test connections", self.test_connections),
            ("tests", self.tests),
            ("data phases", self.data_phases),
            ("replay cache", self.replay_cache),
            ("source budgets", self.source_budgets),
            ("derived keys", self.derived_keys),
            ("source quotas", self.source_quotas),
            ("key quotas", self.key_quotas),
        ]
    }
}

pub struct Server {
    config: ServerConfig,
    sending_rates: Arc<SendingRateTable>,
    outputs: VecDeque<Output>,
    /// The admitted Test Connections, their Tests and the Quotas.
    admission: Admission<Connection>,
    /// Data Phases announced but not yet taken by the runtime.
    data_phases: HashMap<SocketId, DataPhase>,
    /// Verified requests' keyId and authDigest, shared by every socket.
    replays: ReplayCache,
    /// When Troubleshooting Mode's next warning is due; `None` when off.
    troubleshooting_warning: Option<MonoTime>,
    /// Troubleshooting replies this second.
    troubleshooting_replies: PerSecond,
    /// Setup Requests reaching authentication this second.
    unverified_setups: PerSecond,
    /// Each Source's budget of authentic Setup Requests.
    source_budgets: Budgets<Source>,
    /// Each Key Table row's budget of authentic Setup Requests, `None`
    /// for Unauthenticated Mode.
    key_budgets: Budgets<Option<usize>>,
    key_cache: DerivedKeyCache,
    /// Stopping, after [`Server::handle_shutdown`]: no more Admissions.
    stopping: bool,
}

impl Server {
    pub fn new(config: ServerConfig) -> Self {
        // Due at once: the first warning comes with the first timeout.
        let troubleshooting_warning = config
            .troubleshooting
            .then(|| MonoTime::from_duration(Duration::ZERO));
        let unverified_setups = PerSecond::new(config.unverified_setups_per_second);
        let replays = ReplayCache::new(replay::capacity(
            config.unverified_setups_per_second,
            config.max_test_connections,
        ));
        let source_budgets =
            Budgets::new(config.source_setup_burst, config.source_setups_per_second);
        let key_budgets = Budgets::new(config.key_setup_burst, config.key_setups_per_second);
        let sending_rates = Arc::new(SendingRateTable::new(config.table));
        let admission = Admission::new(&config);
        Self {
            config,
            sending_rates,
            outputs: VecDeque::new(),
            admission,
            data_phases: HashMap::new(),
            replays,
            troubleshooting_warning,
            troubleshooting_replies: PerSecond::new(TROUBLESHOOTING_REPLIES_PER_SECOND),
            unverified_setups,
            source_budgets,
            key_budgets,
            key_cache: DerivedKeyCache::default(),
            stopping: false,
        }
    }

    /// The Server's operator stopped it: every Test Connection is stopped,
    /// one in its Data Phase through [`Output::StopDataPhase`] and any
    /// other closed, and from now on each Setup Request is ignored
    /// ([`SetupIgnoreReason::Stopping`]). The runtime reports each as it
    /// ends, as ever; it decides how long to wait for them.
    pub fn handle_shutdown(&mut self, _now: Now) {
        if self.stopping {
            return;
        }
        self.stopping = true;
        for (socket, connection) in self.admission.set_up_mut() {
            // No Activation Watchdog expires on the way out.
            connection.data.watchdog = None;
            self.outputs.push_back(if connection.activated() {
                Output::StopDataPhase(socket)
            } else {
                Output::CloseSocket(socket)
            });
        }
    }

    fn emit(&mut self, event: Event) {
        self.outputs.push_back(Output::Event(event));
    }

    /// Sends a control PDU, its checkSum filled last when checksum use is
    /// configured (RFC 9946 §5.6).
    fn send(&mut self, socket: SocketId, destination: SocketAddr, payload: Vec<u8>) {
        self.send_from(socket, None, destination, payload);
    }

    /// As [`Self::send`], from the local address `source` of a socket
    /// bound to a wildcard address.
    fn send_from(
        &mut self,
        socket: SocketId,
        source: Option<IpAddr>,
        destination: SocketAddr,
        mut payload: Vec<u8>,
    ) {
        if self.config.checksum {
            checksum::fill_control(&mut payload);
        }
        self.outputs.push_back(Output::Transmit(Transmit {
            socket,
            destination,
            payload,
            source,
        }));
    }

    /// What Admission knows of the Key Table row that authenticated a
    /// Test Connection.
    fn key(&self, auth: &ConnectionAuth) -> Key {
        Key {
            row: auth.row,
            operator: auth.is_operator(),
            quota: self.row_quota(auth.row),
        }
    }

    /// A Key Table row's Quota.
    fn row_quota(&self, row: usize) -> QuotaLimits {
        self.config
            .keys
            .entries()
            .get(row)
            .map_or_else(QuotaLimits::default, |entry| entry.quota)
    }

    /// Answers an authentic Setup Request with a rejection: the request
    /// echoed as a Setup Response with the reason's cmdResponse, signed with
    /// the Server key when the request was authenticated (RFC 9946 §6.2.1).
    fn reject_setup(
        &mut self,
        now: Now,
        (client, local): (SocketAddr, IpAddr),
        request: SetupPdu,
        auth: Option<&ConnectionAuth>,
        reason: SetupRejection,
    ) {
        let mut response = SetupPdu {
            cmd_request: cmd_request::SETUP_RESPONSE,
            cmd_response: reason.cmd_response(),
            // BADVER tells the Client the version expected (§6.2.1).
            protocol_version: match reason {
                SetupRejection::BadVersion => PROTOCOL_VERSION,
                _ => request.protocol_version,
            },
            test_port: 0,
            auth_unix_time: 0,
            auth_digest: [0; AUTH_DIGEST_LEN],
            key_id: 0,
            checksum: 0,
            ..request
        };
        if let Some(auth) = auth {
            response.auth_unix_time = auth_unix_time(now.wall);
            response.key_id = request.key_id;
            response.sign(auth.keys.server());
        }
        // From the address the request arrived on (RFC 9946 §6.2.2).
        self.send_from(
            SocketId::PRIMARY,
            Some(local),
            client,
            response.encode().to_vec(),
        );
        self.emit(Event::SetupRequestRejected {
            from: client,
            reason,
        });
    }

    /// Whether Troubleshooting Mode answers an authentication failure now:
    /// at most TROUBLESHOOTING_REPLIES_PER_SECOND, since a spoofed request
    /// gets its reply.
    fn troubleshooting_reply(&mut self, now: MonoTime) -> bool {
        self.config.troubleshooting && self.troubleshooting_replies.take(now)
    }

    /// How full each of the Server's tables is.
    pub fn footprint(&self) -> ServerFootprint {
        let admission = self.admission.usage();
        ServerFootprint {
            test_connections: TableUse::new(admission.test_connections),
            tests: TableUse::new(admission.tests),
            data_phases: TableUse::new((self.data_phases.len(), self.config.max_test_connections)),
            replay_cache: TableUse::new(self.replays.usage()),
            source_budgets: TableUse::new(self.source_budgets.usage()),
            derived_keys: TableUse::new(self.key_cache.usage()),
            source_quotas: TableUse::new(admission.source_quotas),
            key_quotas: TableUse::new(admission.key_quotas),
        }
    }

    /// KDF runs so far: a flood of Setup Requests sharing a Key Table row
    /// and an authUnixTime costs one.
    pub fn kdf_runs(&self) -> u64 {
        self.key_cache.kdf_runs()
    }

    fn silently_reject_setup(&mut self, from: SocketAddr, reason: SilentRejectionReason) {
        self.emit(Event::SetupRequestSilentlyRejected { from, reason });
    }

    /// Spends a token of the budget of the Key Table row at `row`, `None`
    /// for Unauthenticated Mode, once a request is authentic and no replay
    ///. Over it the request is dropped silently, never answered: a
    /// key holder's flood gets no reflection, and costs no admission
    /// (conformance: key-setup-budget).
    fn spend_key_budget(&mut self, row: Option<usize>, now: MonoTime) -> bool {
        self.key_budgets.spend(row, now) == Spend::Spent
    }

    /// Message Verification Procedure (RFC 9946 §6.2) for an authenticated
    /// Setup Request: authMode, then authDigest, then authUnixTime. The row
    /// for the keyId must be within its accept lifetime on this Server's
    /// clock (RFC 7210 §3). Returns the Test Connection's authentication, or
    /// why the request isn't authentic, with the authentication when only
    /// its time failed: its digest is valid, so its keys are too.
    fn authenticate_setup(
        &mut self,
        request: &SetupPdu,
        payload: &[u8],
        now: Now,
    ) -> Result<ConnectionAuth, (AuthenticationFailure, Option<ConnectionAuth>)> {
        let (row, entry) = self
            .config
            .keys
            .accepting(request.key_id, now.wall)
            .map_err(|e| match e {
                KeyLookupError::Unknown => (AuthenticationFailure::UnknownKeyId, None),
                KeyLookupError::OutsideLifetime => {
                    (AuthenticationFailure::KeyOutsideLifetime, None)
                }
            })?;
        // A global budget, spent once a key row is found and before any KDF
        // or HMAC, so a flood is dropped cheaply (docs/spec.md story 85); per
        // source only after the HMAC, so a spoofed source can't spend
        // another's. Requests that fail the cheaper checks above don't
        // spend it.
        if !self.unverified_setups.take(now.mono) {
            return Err((AuthenticationFailure::BudgetExhausted, None));
        }
        // Keys come from this first Setup Request's own authUnixTime (RFC
        // 9946 §5.4.1), from the cache, so a flood sharing a row and an
        // authUnixTime costs one KDF run and then an HMAC per PDU. A cached
        // copy stays when the digest fails: it depends only on the row and
        // the time, not on this request.
        let keys = self
            .key_cache
            .keys(row, entry, request.auth_unix_time, now.wall);
        if !SetupPdu::verify_received(payload, keys.client()) {
            return Err((AuthenticationFailure::DigestFailed, None));
        }
        let auth = ConnectionAuth::new(request, row, entry, keys);
        if !auth_time_acceptable(request.auth_unix_time, now.wall) {
            return Err((AuthenticationFailure::TimeOutOfWindow, Some(auth)));
        }
        Ok(auth)
    }

    fn handle_setup_request(&mut self, now: Now, meta: &DatagramMeta, payload: &[u8]) {
        if self.stopping {
            return self.emit(Event::SetupRequestIgnored {
                from: meta.source,
                reason: SetupIgnoreReason::Stopping,
            });
        }
        // Dropped silently, before its contents are looked at (RFC 9946
        // §6). The destination is where the datagram arrived, which the
        // runtime's listeners report (ADR 0007).
        if is_multicast_or_broadcast(meta.source.ip()) || is_multicast_or_broadcast(meta.local.ip())
        {
            return self
                .silently_reject_setup(meta.source, SilentRejectionReason::MulticastOrBroadcast);
        }
        // Replies and the Test Connection's socket use the arrival address
        // (RFC 9946 §6.2.2), so without one there is nothing to answer from.
        if meta.local.ip().is_unspecified() {
            return self.emit(Event::SetupRequestIgnored {
                from: meta.source,
                reason: SetupIgnoreReason::ArrivalAddressUnknown,
            });
        }
        let request = match SetupPdu::decode(payload) {
            Ok(pdu) if pdu.cmd_request == cmd_request::SETUP_REQUEST => pdu,
            _ => return self.silently_reject_setup(meta.source, SilentRejectionReason::Malformed),
        };
        // After the size, before authentication (RFC 9946 §5.6, §6.2).
        if !checksum_use::passes(self.config.checksum, request.checksum, || {
            checksum::control_valid(payload)
        }) {
            return self.silently_reject_setup(meta.source, SilentRejectionReason::ChecksumFailed);
        }
        // Every authentication failure is silent, the RFC's default (§5.3.1,
        // §6.2.1; docs/spec.md story 77), unless Troubleshooting Mode answers it.
        let failure = match request.auth_mode {
            // Admitted without any check, so it spends the budget too.
            auth_mode::UNAUTHENTICATED if self.config.allow_unauthenticated => {
                if !self.unverified_setups.take(now.mono) {
                    let reason = SilentRejectionReason::UnverifiedBudgetExhausted;
                    return self.silently_reject_setup(meta.source, reason);
                }
                if !self.spend_key_budget(None, now.mono) {
                    let reason = SilentRejectionReason::KeyBudgetExhausted;
                    return self.silently_reject_setup(meta.source, reason);
                }
                Ok(None)
            }
            auth_mode::UNAUTHENTICATED => Err((AuthenticationFailure::Required, None)),
            _ if self.config.keys.entries().is_empty() => {
                Err((AuthenticationFailure::NotConfigured, None))
            }
            auth_mode::MODE_1 | auth_mode::MODE_2 => {
                self.authenticate_setup(&request, payload, now).map(Some)
            }
            _ => Err((AuthenticationFailure::ModeInvalid, None)),
        };
        let auth = match failure {
            Ok(auth) => auth,
            Err((AuthenticationFailure::BudgetExhausted, _)) => {
                let reason = SilentRejectionReason::UnverifiedBudgetExhausted;
                return self.silently_reject_setup(meta.source, reason);
            }
            Err((failure, auth)) if self.troubleshooting_reply(now.mono) => {
                // Unsigned unless the digest was valid (AUTHTIME), whose
                // keys are then valid too (ADR 0009).
                let rejection = SetupRejection::Authentication(failure);
                return self.reject_setup(
                    now,
                    (meta.source, meta.local.ip()),
                    request,
                    auth.as_ref(),
                    rejection,
                );
            }
            Err((failure, _)) => return self.silently_reject_setup(meta.source, failure.into()),
        };
        // After the digest and time checks, so only verified requests are
        // remembered (docs/spec.md story 84).
        if auth.is_some() {
            if self
                .replays
                .seen(request.key_id, request.auth_digest, None, now.mono)
            {
                return self.silently_reject_setup(meta.source, SilentRejectionReason::Replayed);
            }
            // Each Source's budget, spent only once the request is authentic,
            // so only a key holder can spend another Source's (docs/spec.md
            // story 86). Requests in Unauthenticated Mode have no HMAC, so
            // they don't spend it. Only the first refusal since the Source last spent a token
            // is answered, so a key holder can't make the Server reflect a
            // signed rejection for each request (docs/spec.md)
            // (conformance: source-budget-spent-after-the-hmac).
            match self
                .source_budgets
                .spend(Source::of(meta.source.ip()), now.mono)
            {
                Spend::Spent => {}
                Spend::FirstRefusal => {
                    let reason = SetupRejection::SourceBudgetExhausted;
                    return self.reject_setup(
                        now,
                        (meta.source, meta.local.ip()),
                        request,
                        auth.as_ref(),
                        reason,
                    );
                }
                Spend::Refused => {
                    return self.silently_reject_setup(
                        meta.source,
                        SilentRejectionReason::SourceBudgetExhausted,
                    );
                }
            }
            if !self.spend_key_budget(auth.as_ref().map(|auth| auth.row), now.mono) {
                let reason = SilentRejectionReason::KeyBudgetExhausted;
                return self.silently_reject_setup(meta.source, reason);
            }
            // Remembered only now, within its Source's budget, so a key
            // holder's flood can't fill the cache and lock everyone out: it
            // can't fill faster than the budgets let requests through. A
            // request refused by its budget isn't remembered, so a copy may
            // come back once the budget refills.
            if !self
                .replays
                .remember(request.key_id, request.auth_digest, None, now.mono)
            {
                return self
                    .silently_reject_setup(meta.source, SilentRejectionReason::ReplayCacheFull);
            }
        }
        // The other fields (§6.2.1): an authentic request (or an allowed mode
        // 0 one) whose options don't match this Server's configuration gets
        // a coded rejection, after the replay cache so a replay stays silent.
        // Legacy protocolVer 11-19, which OB-UDPST serves, gets BADVER, and
        // MCINVPAR is checked after BADJS and BADTMTU.
        // (conformance: legacy-protocol-versions-get-badver, setup-check-order)
        if let Some(reason) = unsupported_option(&request, self.config.table) {
            return self.reject_setup(
                now,
                (meta.source, meta.local.ip()),
                request,
                auth.as_ref(),
                reason,
            );
        }
        // Then the Multi-Connection fields (MCINVPAR) and the Server's
        // limits, Admission's in its order.
        let setup = Setup::of(&request, meta.source, auth.as_ref().map(|a| self.key(a)));
        let connection = Connection {
            local: meta.local.ip(),
            request,
            auth,
            connected: false,
            watchdog: None,
        };
        let socket = match self.admission.admit(setup, connection, now.mono) {
            Ok(Admitted { socket, evicted }) => {
                if let Some(evicted) = evicted {
                    self.emit(Event::UnactivatedTestConnectionEvicted { socket: evicted });
                    self.data_phases.remove(&evicted);
                    self.emit(Event::TestConnectionReleased { socket: evicted });
                    self.outputs.push_back(Output::CloseSocket(evicted));
                }
                socket
            }
            Err((Refusal::Duplicate, _)) => {
                return self.emit(Event::SetupRequestIgnored {
                    from: meta.source,
                    reason: SetupIgnoreReason::DuplicateSetupRequest,
                });
            }
            Err((Refusal::Rejected(reason), connection)) => {
                return self.reject_setup(
                    now,
                    (meta.source, meta.local.ip()),
                    connection.request,
                    connection.auth.as_ref(),
                    reason,
                );
            }
        };
        // The Test Connection's socket is bound to the address the request
        // arrived on, so replies come from the address the Client used.
        self.outputs.push_back(Output::OpenSocket {
            socket,
            local_ip: meta.local.ip(),
            scope_id: match meta.local {
                SocketAddr::V6(local) => local.scope_id(),
                SocketAddr::V4(_) => 0,
            },
        });
    }

    fn handle_activation_request(&mut self, now: Now, meta: &DatagramMeta, payload: &[u8]) {
        // Neither answered nor sent Load PDUs: a spoofed group address would
        // turn the Test into a multicast stream (docs/spec.md story 89).
        if is_multicast_or_broadcast(meta.source.ip()) {
            return self.emit(Event::ActivationRequestSilentlyRejected {
                socket: meta.socket,
                reason: SilentRejectionReason::MulticastOrBroadcast,
            });
        }
        let socket = meta.socket;
        let Some(connection) = self.admission.get(socket) else {
            return;
        };
        if connection.activated() {
            return;
        }
        // A Test Connection is locked to its Client's IP address; the port
        // may differ, for NATs that change it (docs/spec.md).
        if meta.source.ip() != connection.setup().client.ip() {
            let reason = SilentRejectionReason::NotFromClient;
            return self.emit(Event::ActivationRequestSilentlyRejected { socket, reason });
        }
        let max_bandwidth = connection.setup().max_bandwidth_mbps();
        // The Setup Response's round trip, the only one before any Status
        // PDU: it also holds the Client's own delay, which the cap on the
        // initial w bounds (RFC 9097 §8.1).
        let rtt = connection.answered_at().map_or(Duration::ZERO, |answered| {
            now.mono
                .as_duration()
                .saturating_sub(answered.as_duration())
        });
        let auth = connection.data.auth.as_ref();
        let operator = auth.is_some_and(ConnectionAuth::is_operator);
        let allowance = auth.map_or(Allowance::CONSUMER, ConnectionAuth::allowance);
        let allowance = Allowance {
            dscp: if allowance.operator {
                self.config.operator_allowed_dscp
            } else {
                self.config.allowed_dscp
            },
            ..allowance
        };
        // Message Verification Procedure (RFC 9946 §7.2, §6.2): the size and
        // PDU ID, then authMode and keyId, then authDigest, then
        // authUnixTime, with the checksum before authentication when checksum
        // use is configured (§5.6). Every failure is silent (§7.2.1),
        // Troubleshooting Mode or not: it answers Setup Requests only
        // (conformance: troubleshooting-replies-rate-limited).
        let checksum = self.config.checksum;
        let verified = match ActivationPdu::decode(payload) {
            Ok(request)
                if !checksum_use::passes(checksum, request.checksum, || {
                    checksum::control_valid(payload)
                }) =>
            {
                Err(SilentRejectionReason::ChecksumFailed)
            }
            Ok(request) => verify_activation(auth, &request, payload, now).map(|()| request),
            Err(_) => Err(SilentRejectionReason::Malformed),
        };
        let authenticated = auth.is_some();
        let request = match verified {
            Ok(request) => request,
            Err(reason) => {
                return self.emit(Event::ActivationRequestSilentlyRejected { socket, reason });
            }
        };
        // One cache for every socket, so a copy sent to a sibling Test
        // Connection's Ephemeral Port is caught too; keyed on the sender as
        // well, since siblings' own requests can be identical.
        if authenticated {
            let sight = self.replays.sight(
                request.key_id,
                request.auth_digest,
                Some(meta.source),
                now.mono,
            );
            if let Some(reason) = unseen(sight) {
                return self.emit(Event::ActivationRequestSilentlyRejected { socket, reason });
            }
        }
        // The first verified Activation Request locks the Test Connection
        // to its address and port, NAT's choice included (docs/spec.md, story
        // 88), before any answer goes out. In Unauthenticated Mode that is
        // the first that decodes (conformance: first-verified-activation-connects).
        if let Some(connection) = self.admission.get_mut(socket)
            && !connection.data.connected
        {
            connection.data.connected = true;
            self.outputs.push_back(Output::ConnectSocket {
                socket,
                peer: meta.source,
            });
        }
        let Some(direction) = Direction::from_activation_cmd_request(request.cmd_request) else {
            return self.reject_activation(now, meta, request, ActivationRejection::InvalidCommand);
        };
        if let Some(parameter) = unacceptable_parameter(&request) {
            let reason = ActivationRejection::UnacceptableParameter(parameter);
            return self.reject_activation(now, meta, request, reason);
        }
        // A consumer MUST NOT be able to initiate a Fixed-Rate Test (RFC 9946
        // §4.1): only an Operator Key may. OB-UDPST lets any Client run one
        // (conformance: fixed-rate-needs-operator-key).
        if is_fixed_rate(&request) && !operator {
            let reason = ActivationRejection::FixedRateNeedsOperatorKey;
            return self.reject_activation(now, meta, request, reason);
        }
        // Then the limits: Admission's, in its order.
        if let Err(reason) = self.admission.activate(socket, direction, now.mono) {
            return self.reject_activation(now, meta, request, reason);
        }
        if let Some(connection) = self.admission.get_mut(socket) {
            connection.data.watchdog = None;
        }
        // The Server never attempts a rate above the Client's stated
        // maxBandwidth (RFC 9946 §6.1, §11 item 5; docs/spec.md story 9), in
        // either direction and with or without an admission cap, where
        // OB-UDPST caps only in a test build with a cap configured
        // (conformance: search-capped-at-stated-max-bandwidth).
        let max_row = match max_bandwidth {
            0 => self.sending_rates.last_index(),
            mbps => self.sending_rates.row_for_mbps(mbps),
        };
        // A Consumer's search starts no higher than the Server allows; it
        // may climb past that from there, up to `max_row`.
        let max_start = if operator {
            max_row
        } else {
            max_row.min(self.config.max_start_row)
        };
        let (row, sr_index_conf) = self.requested_row(&request, max_start);
        let row_rate = self
            .sending_rates
            .row(row)
            .expect("requested_row stays within the table");
        let sending_rate = match direction {
            Direction::Upstream => row_rate,
            // All octets are zero for a Downstream Test (RFC 9946 §7.2.2).
            Direction::Downstream => SendingRate::default(),
        };
        // Every ranged parameter within its range (RFC 9946 §7.2.1).
        let response = ActivationPdu {
            cmd_response: activation::cmd_response::ACK_OK,
            sr_index_conf,
            sending_rate,
            ..response_to(parameters::clamp(&request, allowance))
        };
        let response = self.authenticate_response(socket, response, now);
        self.send(socket, meta.source, response.encode().to_vec());
        for coercion in coercions(&request, &response) {
            self.emit(Event::ActivationParameterCoerced { socket, coercion });
        }
        self.emit(Event::TestConnectionActivated {
            socket,
            direction,
            parameters: TestParameters::of(&response),
        });
        let auth = self
            .admission
            .get(socket)
            .and_then(|c| c.data.auth.as_ref());
        let setup = DataPhaseSetup {
            now,
            peer: meta.source,
            dscp_ecn: response.dscp_ecn,
            seed: data_phase::seed(now, socket.0.into()),
            checksum: self.config.checksum,
            keys: auth.map(|auth| auth.keys.clone()),
            key_id: auth.map_or(0, |a| a.key_id),
            auth_mode: auth.map_or(auth_mode::UNAUTHENTICATED, |a| a.auth_mode),
            test_interval: Duration::from_secs(response.test_int_time.into()),
            trial_interval: Duration::from_millis(response.trial_int.into()),
            sub_interval: Duration::from_millis(response.sub_int_period.into()),
            this_end: End::Server,
            thresholds: Some(Thresholds::accepted(&response, self.config.delay_var)),
            // A Fixed-Rate Test holds its row.
            algorithm: (!is_fixed_rate(&request)).then(|| {
                RateAdjustment::new(row, &response, self.sending_rates.clone(), max_row, rtt)
            }),
            // Its classifications drive the algorithm: nothing to count.
            counter: None,
            grid: None,
            max_payload: crate::sending_rate_table::MAX_JUMBO_IP_PACKET - 28,
            max_bandwidth_kbps: None,
        };
        let data_phase = match direction {
            // The Server is the Load Sender and sends from the row itself
            // (RFC 9946 §7.2.2); the algorithm moves it.
            Direction::Downstream => {
                let random_payload =
                    response.modifier_bitmap & activation::modifier::RANDOM_PAYLOAD != 0;
                DataPhase::load_sender(setup, &row_rate, meta.ip_version, random_payload)
            }
            // The Server is the Load Receiver; its Status PDUs carry the row
            // the Client sends at (RFC 9946 §8.2), which the algorithm
            // moves.
            Direction::Upstream => DataPhase::load_receiver(
                setup,
                Duration::from_millis(response.sub_int_period.into()),
                row_rate,
            ),
        };
        self.data_phases.insert(socket, data_phase);
        self.outputs.push_back(Output::StartDataPhase {
            socket,
            peer: meta.source,
        });
    }

    /// The Test's Sending Rate Table row and the srIndexConf the Activation
    /// Response reports: the first row for CHTA_SRIDX_DEF, otherwise the
    /// srIndexConf row, whether the algorithm starts there
    /// (CHTA_SRIDX_ISSTART) or it is a fixed rate (RFC 9946 §7.2.2).
    ///
    /// A row above `max_row` (the table's last row, or the row matching the
    /// stated maxBandwidth) is coerced to it, and the response says so. For
    /// a row beyond the table: §7.2.1 coerces to "the configured maximum", and the index
    /// is never outside the table. OB-UDPST instead coerces such a request
    /// to CHTA_SRIDX_DEF, which would turn a Fixed-Rate Test into a Search Test
    /// (conformance: sr-index-conf-beyond-the-top-row-coerced, row-above-max-bandwidth-coerced).
    fn requested_row(&self, request: &ActivationPdu, max_row: u16) -> (u16, u16) {
        match request.sr_index_conf {
            SR_INDEX_DEFAULT => (0, SR_INDEX_DEFAULT),
            row => {
                let row = row.min(max_row);
                (row, row)
            }
        }
    }

    /// Fills the authentication fields of an Activation Response on a mode 1
    /// Test Connection: the current authUnixTime, the Setup's keyId, and the
    /// digest with the Server Authentication Key (RFC 9946 §5.4.1).
    fn authenticate_response(
        &self,
        socket: SocketId,
        mut response: ActivationPdu,
        now: Now,
    ) -> ActivationPdu {
        let auth = self
            .admission
            .get(socket)
            .and_then(|c| c.data.auth.as_ref());
        if let Some(auth) = auth {
            response.auth_mode = auth.auth_mode;
            response.auth_unix_time = auth_unix_time(now.wall);
            response.key_id = auth.key_id;
            response.sign(auth.keys.server());
        }
        response
    }

    fn reject_activation(
        &mut self,
        now: Now,
        meta: &DatagramMeta,
        request: ActivationPdu,
        reason: ActivationRejection,
    ) {
        let response = ActivationPdu {
            cmd_response: activation::cmd_response::BAD_PARAM,
            ..response_to(request)
        };
        // The request's authDigest was valid, so the rejection is answered
        // (RFC 9946 §7.2.1).
        let response = self.authenticate_response(meta.socket, response, now);
        self.send(meta.socket, meta.source, response.encode().to_vec());
        self.emit(Event::ActivationRequestRejected {
            socket: meta.socket,
            reason,
        });
        // The rejected Test Connection ends here; closing its socket
        // releases what was admitted for it (handle_socket_closed).
        if let Some(connection) = self.admission.get_mut(meta.socket) {
            connection.data.watchdog = None;
        }
        self.outputs.push_back(Output::CloseSocket(meta.socket));
    }
}

/// Whether an Activation Request asks for a Fixed-Rate Test: an srIndexConf
/// row with CHTA_SRIDX_ISSTART clear (RFC 9946 §7.1). CHTA_SRIDX_DEF is the
/// Server's default search whatever the bit says.
fn is_fixed_rate(request: &ActivationPdu) -> bool {
    request.sr_index_conf != SR_INDEX_DEFAULT
        && request.modifier_bitmap & activation::modifier::SR_INDEX_IS_START == 0
}

/// The first parameter of an Activation Request that the Server can neither
/// accept nor coerce, which gets the request rejected with
/// CHTA_CRSP_BADPARAM (RFC 9946 §7.2.1): an algorithm it doesn't implement,
/// which has no value to coerce to. A ranged parameter outside its range is
/// coerced to the range's limits instead (`parameters::clamp`).
fn unacceptable_parameter(request: &ActivationPdu) -> Option<ActivationParameter> {
    // An algorithm this Server doesn't implement: running the Test with
    // another one would measure by a method the Client didn't ask for.
    if !matches!(request.rate_adj_algo, rate_adj_algo::B | rate_adj_algo::C) {
        return Some(ActivationParameter::RateAdjAlgo);
    }
    None
}

/// Message Verification Procedure steps 3 to 5 for an Activation Request
/// (RFC 9946 §6.2): authMode and keyId must match the Setup's (ADR 0009),
/// then the digest over the received bytes must verify with the Client
/// Authentication Key, then authUnixTime must be within ±5 s.
fn verify_activation(
    auth: Option<&ConnectionAuth>,
    request: &ActivationPdu,
    payload: &[u8],
    now: Now,
) -> Result<(), SilentRejectionReason> {
    let Some(auth) = auth else {
        return match request.auth_mode {
            auth_mode::UNAUTHENTICATED => Ok(()),
            _ => Err(SilentRejectionReason::AuthModeMismatch),
        };
    };
    if request.auth_mode != auth.auth_mode {
        return Err(SilentRejectionReason::AuthModeMismatch);
    }
    if request.key_id != auth.key_id {
        return Err(SilentRejectionReason::KeyIdMismatch);
    }
    let signed = ActivationPdu::verify_received(payload, auth.keys.client());
    check_signed(signed, request.auth_unix_time, now)
}

/// Message Verification Procedure steps 4 and 5 (RFC 9946 §6.2), shared by
/// every authenticated request: the digest, then the ±5 s window.
fn check_signed(signed: bool, auth_unix_time: u32, now: Now) -> Result<(), SilentRejectionReason> {
    if !signed {
        return Err(SilentRejectionReason::AuthenticationFailed);
    }
    if !auth_time_acceptable(auth_unix_time, now.wall) {
        return Err(SilentRejectionReason::AuthTimeOutOfWindow);
    }
    Ok(())
}

/// The Setup Request option this Server can't serve, if any (RFC 9946
/// §6.2.1): another protocol version, or CHSR_JUMBO_STATUS or
/// CHSR_TRADITIONAL_MTU other than the Server's own (§6.1: the Server MUST
/// reject a mismatch).
fn unsupported_option(request: &SetupPdu, table: TableShape) -> Option<SetupRejection> {
    let flag = |bit: u8| request.modifier_bitmap & bit != 0;
    if request.protocol_version != PROTOCOL_VERSION {
        Some(SetupRejection::BadVersion)
    } else if flag(modifier::JUMBO_STATUS) != table.jumbo {
        Some(SetupRejection::JumboMismatch)
    } else if flag(modifier::TRADITIONAL_MTU) != table.traditional_mtu {
        Some(SetupRejection::TraditionalMtuMismatch)
    } else {
        None
    }
}

/// Why the replay cache drops a verified request, if it does.
fn unseen(sight: Sight) -> Option<SilentRejectionReason> {
    match sight {
        Sight::First => None,
        Sight::Replay => Some(SilentRejectionReason::Replayed),
        Sight::Full => Some(SilentRejectionReason::ReplayCacheFull),
    }
}

/// The Setup Response accepting `connection`'s Setup Request, whose socket
/// is bound at `local`, and the Null Request that follows it (RFC 9946
/// §6.2.2), encoded.
fn setup_response(connection: &Connection, local: SocketAddr, now: Now) -> (Vec<u8>, Vec<u8>) {
    let request = &connection.request;
    // The response keeps the request's values except for the fields RFC
    // 9946 §6.2.2 says to change. The authentication fields and the
    // checksum are never echoed: in Unauthenticated Mode they are zero, and
    // in authenticated modes authUnixTime is the current time and the
    // digest is recomputed with the Server Authentication Key.
    let mut response = SetupPdu {
        cmd_request: cmd_request::SETUP_RESPONSE,
        cmd_response: cmd_response::ACK_OK,
        test_port: local.port(),
        auth_unix_time: 0,
        auth_digest: [0; AUTH_DIGEST_LEN],
        key_id: 0,
        checksum: 0,
        ..request.clone()
    };
    // OB-UDPST sends the Null Request only for protocolVer 20 or later;
    // only 20 gets this far, so it always goes.
    // (conformance: legacy-protocol-versions-get-badver)
    let mut null = NullPdu {
        protocol_version: request.protocol_version,
        cmd_request: null::cmd_request::NULL_REQUEST,
        cmd_response: null::cmd_response::NONE,
        auth_mode: request.auth_mode,
        auth_unix_time: 0,
        auth_digest: [0; AUTH_DIGEST_LEN],
        key_id: 0,
        checksum: 0,
    };
    if let Some(auth) = &connection.auth {
        response.auth_unix_time = auth_unix_time(now.wall);
        response.key_id = request.key_id;
        response.sign(auth.keys.server());
        null.auth_unix_time = auth_unix_time(now.wall);
        null.key_id = request.key_id;
        null.sign(auth.keys.server());
    }
    (response.encode().to_vec(), null.encode().to_vec())
}

/// A response built from a request, before its authentication fields are
/// filled: they and the checksum are never echoed.
fn response_to(request: ActivationPdu) -> ActivationPdu {
    ActivationPdu {
        auth_unix_time: 0,
        auth_digest: [0; AUTH_DIGEST_LEN],
        key_id: 0,
        checksum: 0,
        ..request
    }
}

impl Endpoint for Server {
    fn handle_datagram(&mut self, now: Now, meta: DatagramMeta, payload: &[u8]) {
        if meta.socket == SocketId::PRIMARY {
            self.handle_setup_request(now, &meta, payload);
        } else {
            self.handle_activation_request(now, &meta, payload);
        }
    }

    fn handle_socket_opened(&mut self, now: Now, socket: SocketId, local: SocketAddr) {
        // A Setup accepted just before the Server began to stop is closed
        // unanswered.
        if self.stopping {
            if self.admission.is_waiting(socket) {
                self.admission.close(socket, now.mono);
                self.outputs.push_back(Output::CloseSocket(socket));
            }
            return;
        }
        let Some(connection) = self.admission.open(socket, now.mono) else {
            return;
        };
        connection.data.watchdog = Some(MonoTime::from_duration(
            now.mono.as_duration() + CONTROL_WATCHDOG,
        ));
        let client = connection.setup().client;
        let mc_ident = connection.setup().mc_ident;
        let key_id = connection.data.auth.as_ref().map(|a| a.key_id);
        let (response, null) = setup_response(&connection.data, local, now);
        // From the Test Connection's socket's address, the one the request
        // arrived on (RFC 9946 §6.2.2).
        self.send_from(SocketId::PRIMARY, Some(local.ip()), client, response);
        // At once, from the new socket, so this host's firewall lets the
        // Client's datagrams to it in (RFC 9946 §6.2.2). No response comes.
        self.send(socket, client, null);
        self.emit(Event::TestConnectionAdmitted {
            socket,
            client,
            mc_ident,
            key_id,
        });
    }

    /// Adds a Test Connection's load bytes, sent and received, to its
    /// Test's, for the Quotas it is charged to.
    fn handle_data_event(&mut self, _now: Now, socket: SocketId, event: &DataEvent) {
        let bytes = match event {
            DataEvent::Sent(sent) => sent.bytes,
            DataEvent::SubIntervalMeasured(measured) => measured.bytes,
            _ => return,
        };
        self.admission.add_load_bytes(socket, bytes);
    }

    fn handle_socket_closed(&mut self, now: Now, socket: SocketId) {
        self.data_phases.remove(&socket);
        match self.admission.close(socket, now.mono) {
            // A Setup whose socket couldn't be opened is refused, as
            // OB-UDPST answers a failed allocation.
            Closed::Waiting(setup, connection) => {
                let reason = SetupRejection::SocketUnavailable;
                self.reject_setup(
                    now,
                    (setup.client, connection.local),
                    connection.request,
                    connection.auth.as_ref(),
                    reason,
                );
            }
            Closed::Released => self.emit(Event::TestConnectionReleased { socket }),
            Closed::Unknown => {}
        }
    }

    /// The Activation Watchdog of each Test Connection still waiting for
    /// its Activation Request (RFC 9946 §6.2.2).
    fn handle_timeout(&mut self, now: Now) {
        if let Some(due) = self.troubleshooting_warning
            && due <= now.mono
        {
            self.troubleshooting_warning = Some(MonoTime::from_duration(
                now.mono.as_duration() + TROUBLESHOOTING_WARNING_EVERY,
            ));
            self.emit(Event::TroubleshootingModeOn);
        }
        let expired: Vec<SocketId> = self
            .admission
            .set_up()
            .filter(|(_, c)| c.data.watchdog.is_some_and(|at| at <= now.mono))
            .map(|(socket, _)| socket)
            .collect();
        for socket in expired {
            if let Some(connection) = self.admission.get_mut(socket) {
                connection.data.watchdog = None;
            }
            self.emit(Event::ActivationWatchdogExpired { socket });
            self.outputs.push_back(Output::CloseSocket(socket));
        }
    }

    fn poll_timeout(&self) -> Option<MonoTime> {
        self.admission
            .set_up()
            .filter_map(|(_, c)| c.data.watchdog)
            .chain(self.troubleshooting_warning)
            .min()
    }

    fn poll_output(&mut self) -> Option<Output> {
        self.outputs.pop_front()
    }

    fn take_data_phase(&mut self, socket: SocketId) -> Option<DataPhase> {
        self.data_phases.remove(&socket)
    }
}
