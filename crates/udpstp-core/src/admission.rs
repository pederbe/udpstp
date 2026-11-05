//! Admission (GLOSSARY.md): the Server's acceptance of a Setup Request
//! within its limits, which gives the Test Connection an Ephemeral Port and
//! a place among the Test Connections the Server serves until it ends.
//!
//! [`Admission`] keeps those places: each admitted Test Connection, from
//! its Setup Request's acceptance, while it waits for its socket, until its
//! release; the Tests they make up and the Sources those come from; and the
//! Quotas of each Source and each Key Table row. It judges a verified Setup
//! Request against the Server's limits in a set order
//! (conformance: setup-check-order), checks a Test's first activation
//! again and takes the Test's rate and Quota then, and releases a Test
//! Connection when its socket goes (ADR 0003). The Server keeps the rest:
//! authentication and the budgets before Admission, the PDUs, the
//! Activation Watchdog, the Data Phases and the events.
//!
//! Only activated Tests reserve places in the global cap and hold what
//! they state: an unactivated Test holds no rate and uses no Quota, so a
//! spoofed or abandoned Setup Request can't keep other Tests out. When
//! the Unactivated Test Connections fill their share, the Key Table row
//! holding the most gives up its oldest (see `eviction_candidate`).

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use udpstp_wire::auth_mode;
use udpstp_wire::setup::{SetupPdu, UPSTREAM_DIRECTION_BIT};

use crate::event::{MultiConnectionError, SetupRejection};
use crate::quota::{QuotaLimits, Quotas};
use crate::{ActivationRejection, Direction, MonoTime, ServerConfig, SocketId};

/// A maxBandwidth field's direction and Mbps (RFC 9946 §6.1): the
/// CHSR_USDIR_BIT marks an Upstream Test, and is masked from the Mbps.
fn stated_bandwidth(max_bandwidth: u16) -> (bool, u32) {
    (
        max_bandwidth & UPSTREAM_DIRECTION_BIT != 0,
        u32::from(max_bandwidth & !UPSTREAM_DIRECTION_BIT),
    )
}

/// Identifies a Test: its Test Connections come from one Client address and
/// share an mcIdent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct TestKey {
    client_ip: IpAddr,
    mc_ident: u16,
}

/// The Source a Test comes from: an IPv4 address, or an IPv6 /64, with
/// IPv4-mapped addresses taken as IPv4 (docs/spec.md). RFC 9946 §4.2 limits
/// concurrent Tests of a path to one; the Source approximates the path
/// (conformance: path-approximated-by-source, tests-per-source-above-one).
/// The Server's budgets for Setup Requests are kept per Source too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Source {
    V4(Ipv4Addr),
    V6Prefix(u64),
}

impl Source {
    pub(crate) fn of(ip: IpAddr) -> Self {
        match ip {
            IpAddr::V4(ip) => Self::V4(ip),
            IpAddr::V6(ip) => match ip.to_ipv4_mapped() {
                Some(ip) => Self::V4(ip),
                None => Self::V6Prefix((u128::from(ip) >> 64) as u64),
            },
        }
    }
}

/// What Admission judges a verified Setup Request by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Setup {
    /// The Client's address it came from.
    pub(crate) client: SocketAddr,
    pub(crate) mc_ident: u16,
    pub(crate) mc_index: u8,
    pub(crate) mc_count: u8,
    pub(crate) auth_mode: u8,
    pub(crate) key_id: u8,
    /// maxBandwidth as the field carries it, CHSR_USDIR_BIT included.
    pub(crate) max_bandwidth: u16,
    /// The Key Table row that authenticated it; `None` in Unauthenticated
    /// Mode.
    pub(crate) key: Option<Key>,
}

impl Setup {
    /// What `request`, from `client` and authenticated with `key`, asks
    /// Admission for.
    pub(crate) fn of(request: &SetupPdu, client: SocketAddr, key: Option<Key>) -> Self {
        Self {
            client,
            mc_ident: request.mc_ident,
            mc_index: request.mc_index,
            mc_count: request.mc_count,
            auth_mode: request.auth_mode,
            key_id: request.key_id,
            max_bandwidth: request.max_bandwidth,
            key,
        }
    }

    /// The maxBandwidth it states, in Mbps.
    pub(crate) fn max_bandwidth_mbps(&self) -> u32 {
        stated_bandwidth(self.max_bandwidth).1
    }

    fn test(&self) -> TestKey {
        TestKey {
            client_ip: self.client.ip(),
            mc_ident: self.mc_ident,
        }
    }

    fn operator(&self) -> bool {
        self.key.is_some_and(|key| key.operator)
    }
}

/// The Key Table row a Setup Request was authenticated with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Key {
    /// Its place in the Key Table, by which its Quota's use is kept.
    pub(crate) row: usize,
    /// Whether it is an Operator Key: exempt from the Quotas and from a
    /// Test's share of the Admitted Rate Cap.
    pub(crate) operator: bool,
    /// Its own Quota.
    pub(crate) quota: QuotaLimits,
}

/// An admitted Test Connection: its Setup, how far it got, and the
/// Server's own data about it.
pub(crate) struct TestConnection<T> {
    setup: Setup,
    /// When its socket opened and its Setup Response went out; `None`
    /// while it waits for its socket.
    answered_at: Option<MonoTime>,
    activated: bool,
    pub(crate) data: T,
}

impl<T> TestConnection<T> {
    pub(crate) fn setup(&self) -> &Setup {
        &self.setup
    }

    /// When its Setup Response went out, once its socket opened.
    pub(crate) fn answered_at(&self) -> Option<MonoTime> {
        self.answered_at
    }

    /// Whether its Activation Request was accepted.
    pub(crate) fn activated(&self) -> bool {
        self.activated
    }
}

/// A Test the Server is running Test Connections for: from when its first
/// Test Connection's socket opens until none of its Test Connections is
/// admitted any more, set up or waiting for its socket.
#[derive(Default)]
struct Test {
    /// Fixed by the Test's first accepted Activation Request.
    direction: Option<Direction>,
    /// Its whole Test could not reserve places in the global cap. Keep
    /// refusing its siblings as rejected sockets close, until it ends.
    connections_refused: bool,
    /// Its Test Connections that were set up and have closed: they count
    /// against the per-Test cap for the rest of its life, so a Test kept
    /// alive by one can't start new measurements under it.
    closed: usize,
    /// Whose Quotas it was charged to when it first activated; its bytes
    /// are charged to the same when it ends.
    charged: Option<Charged>,
    /// Load bytes its Test Connections sent and received so far.
    bytes: u64,
}

/// The Quotas a Test is charged to: its Source's and its Key Table row's.
#[derive(Debug, Clone, Copy)]
struct Charged {
    source: Source,
    row: Option<(usize, QuotaLimits)>,
}

/// What one group, a Key Table row or Unauthenticated Mode, holds of the
/// Unactivated Test Connections, when choosing one to evict.
#[derive(Default)]
struct Holding {
    /// Its Unactivated Test Connections, those waiting for their socket
    /// included.
    held: usize,
    /// The oldest of them by its Setup Response, and its socket; `None`
    /// while all are waiting for their socket.
    oldest: Option<(MonoTime, SocketId)>,
}

/// The Server's limits Admission keeps to.
#[derive(Debug, Clone, Copy)]
struct Limits {
    tests_per_source: usize,
    max_test_connections: usize,
    test_connections_per_test: usize,
    /// The most Test Connections admitted but not yet activated at once:
    /// at least the per-Test cap, so a whole Test can be set up. Separate
    /// from the global cap on the places reserved by activated Tests.
    max_unactivated: usize,
    admitted_rate_cap: Option<u32>,
    max_test_share: u8,
    require_max_bandwidth: bool,
    source_quota: QuotaLimits,
}

impl Limits {
    fn of(config: &ServerConfig) -> Self {
        let cap = config.max_test_connections;
        let per_test = config.test_connections_per_test;
        Self {
            tests_per_source: config.tests_per_source,
            max_test_connections: cap,
            test_connections_per_test: per_test,
            max_unactivated: config
                .max_unactivated_test_connections
                .unwrap_or(cap / 2)
                .max(per_test),
            admitted_rate_cap: config.admitted_rate_cap,
            max_test_share: config.max_test_share,
            require_max_bandwidth: config.require_max_bandwidth,
            source_quota: config.source_quota,
        }
    }
}

/// Why [`Admission::admit`] turned a Setup down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// A copy of an admitted Setup Request from the same address, which
    /// the network duplicated: answered once already, so ignored, since
    /// refusing it would end the Client's Test.
    Duplicate,
    /// Refused with a Setup Response carrying the reason.
    Rejected(SetupRejection),
}

/// An admitted Setup: the socket to open for it, and that of the
/// Unactivated Test Connection released to make room for it, which
/// must close.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Admitted {
    pub(crate) socket: SocketId,
    pub(crate) evicted: Option<SocketId>,
}

/// What a socket's closing ended.
pub(crate) enum Closed<T> {
    /// An admitted Setup whose socket never opened: what it asked for, and
    /// the Server's data.
    Waiting(Setup, T),
    /// A set-up Test Connection, now released.
    Released,
    /// Nothing Admission held.
    Unknown,
}

/// How full Admission's tables are, each as its entries and its limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Usage {
    /// Admitted Test Connections, waiting for their socket or set up.
    pub(crate) test_connections: (usize, usize),
    /// Tests with a set-up Test Connection.
    pub(crate) tests: (usize, usize),
    pub(crate) source_quotas: (usize, usize),
    pub(crate) key_quotas: (usize, usize),
}

/// The Server's admitted Test Connections, their Tests and Sources, and the
/// Quotas; `T` is the Server's own data about each Test Connection.
pub(crate) struct Admission<T> {
    limits: Limits,
    /// The socket the next admitted Setup gets.
    next_socket: u32,
    /// Every admitted Test Connection, by socket: in the order the Setups
    /// were admitted, until the numbers wrap.
    test_connections: BTreeMap<SocketId, TestConnection<T>>,
    tests: HashMap<TestKey, Test>,
    /// Each Source's and each Key Table row's Quota use.
    source_quotas: Quotas<Source>,
    key_quotas: Quotas<usize>,
}

impl<T> Admission<T> {
    pub(crate) fn new(config: &ServerConfig) -> Self {
        Self {
            limits: Limits::of(config),
            next_socket: 1,
            test_connections: BTreeMap::new(),
            tests: HashMap::new(),
            source_quotas: Quotas::new(config.quota_period),
            key_quotas: Quotas::new(config.quota_period),
        }
    }

    /// Judges a verified Setup Request, in order: a copy of one admitted is
    /// ignored; then the Multi-Connection checks (MCINVPAR); the Tests per
    /// Source and the Quotas, for a new Test; then the limits: a missing
    /// maxBandwidth where one is required (NOMAXBW), the per-Test and the
    /// global Test Connection caps (CONNFAIL), the Admitted Rate Cap and a
    /// Test's share of it (CAPEXC). When the Unactivated Test Connections
    /// fill their share, admitting it closes the oldest of the Key Table
    /// row holding the most of them, and only then. An
    /// admitted Setup holds a place in the unactivated share. Its Test's
    /// first activation reserves all its admitted places in the global
    /// cap, its rate and its Quota; `data` comes back with a refusal.
    pub(crate) fn admit(
        &mut self,
        setup: Setup,
        data: T,
        now: MonoTime,
    ) -> Result<Admitted, (Refusal, T)> {
        let test = setup.test();
        let source = Source::of(setup.client.ip());
        // A copy of an admitted request, which the network duplicated, is
        // answered once: refusing it would end the Client's Test.
        if self.test_connections.values().any(|c| {
            c.setup.test() == test
                && c.setup.client == setup.client
                && c.setup.mc_index == setup.mc_index
        }) {
            return Err((Refusal::Duplicate, data));
        }
        match self.refusal(test, source, &setup, now) {
            Ok(evicted) => {
                let evicted = evicted.filter(|&socket| self.release(socket, now).is_some());
                let socket = SocketId(self.next_socket);
                // Never SocketId::PRIMARY, even after 2^32 Setups.
                self.next_socket = self.next_socket.checked_add(1).unwrap_or(1);
                self.test_connections.insert(
                    socket,
                    TestConnection {
                        setup,
                        answered_at: None,
                        activated: false,
                        data,
                    },
                );
                Ok(Admitted { socket, evicted })
            }
            Err(reason) => Err((Refusal::Rejected(reason), data)),
        }
    }

    /// Why a Setup of `test` from `source` is refused, or the Test
    /// Connection to evict to admit it.
    fn refusal(
        &self,
        test: TestKey,
        source: Source,
        setup: &Setup,
        now: MonoTime,
    ) -> Result<Option<SocketId>, SetupRejection> {
        // Stricter than RFC 9946 §4, which lets a Server treat each Test
        // Connection as if from a separate Client, and than OB-UDPST, which
        // checks mcIndex against mcCount only (docs/spec.md)
        // (conformance: multi-connection-agreement-checks).
        if let Some(error) = self.multi_connection_error(test, setup) {
            return Err(SetupRejection::MultiConnection(error));
        }
        let new_test = !self.tests.contains_key(&test);
        // One concurrent Test per Source by default (RFC 9946 §4.2): a new
        // Test from a Source that already runs as many is refused.
        if new_test && self.tests_from(source, test) >= self.limits.tests_per_source {
            return Err(SetupRejection::TestsPerSource);
        }
        // A new Test past its Source's or key's Quota (RFC 9946 §11 item
        // 6; ).
        if new_test && !self.within_quota(source, setup.key, now) {
            return Err(SetupRejection::QuotaExceeded);
        }
        // Judge the limits as if the candidate were released. Eviction
        // frees a global place only if its Test has activated; an
        // unrelated Unactivated Test Connection cannot free one.
        // (conformance: unactivated-test-connection-share)
        let evicted = self.eviction_candidate();
        match self.limit_refusal(test, setup, evicted) {
            Some(reason) => Err(reason),
            None => Ok(evicted),
        }
    }

    /// What is wrong with `setup` as a Test Connection of `test`, if
    /// anything (RFC 9946 §6.1; docs/spec.md "Multi-Connection checks"). The
    /// Test's other mcIndexes are never waited for: a Test may span
    /// several Servers.
    fn multi_connection_error(&self, test: TestKey, setup: &Setup) -> Option<MultiConnectionError> {
        if setup.mc_ident == 0 {
            return Some(MultiConnectionError::IdentZero);
        }
        if setup.mc_index >= setup.mc_count {
            return Some(MultiConnectionError::IndexNotBelowCount);
        }
        self.test_connections
            .values()
            .map(|c| &c.setup)
            .filter(|other| other.test() == test)
            .find_map(|other| {
                if other.mc_index == setup.mc_index {
                    Some(MultiConnectionError::IndexInUse)
                } else if other.mc_count != setup.mc_count {
                    Some(MultiConnectionError::CountDiffers)
                } else if other.auth_mode != setup.auth_mode {
                    Some(MultiConnectionError::AuthModeDiffers)
                } else if setup.auth_mode != auth_mode::UNAUTHENTICATED
                    && other.key_id != setup.key_id
                {
                    Some(MultiConnectionError::KeyIdDiffers)
                } else {
                    None
                }
            })
    }

    /// The activated Tests from `source` other than `test`. A Test that is
    /// only set up doesn't count, so a spoofed Setup Request can't keep a
    /// Source from testing; activation checks again.
    fn tests_from(&self, source: Source, test: TestKey) -> usize {
        self.tests
            .iter()
            .filter(|(key, t)| {
                **key != test && t.direction.is_some() && Source::of(key.client_ip) == source
            })
            .count()
    }

    /// Whether a new Test from `source`, keyed with `key`, is within its
    /// Quotas. Operator Keys are exempt.
    fn within_quota(&self, source: Source, key: Option<Key>, now: MonoTime) -> bool {
        if key.is_some_and(|key| key.operator) {
            return true;
        }
        self.source_quotas
            .allows(source, self.limits.source_quota, now)
            && key.is_none_or(|key| self.key_quotas.allows(key.row, key.quota, now))
    }

    /// The limit that refuses another Test Connection of `test` for
    /// `setup`, if any (docs/spec.md "Test limits").
    fn limit_refusal(
        &self,
        test: TestKey,
        setup: &Setup,
        evicted: Option<SocketId>,
    ) -> Option<SetupRejection> {
        if self.limits.require_max_bandwidth && setup.max_bandwidth_mbps() == 0 {
            return Some(SetupRejection::MaxBandwidthRequired);
        }
        let of_test = self
            .test_connections
            .values()
            .filter(|c| c.setup.test() == test)
            .count();
        // Over the Test's whole life, not only at once. A Test ends
        // when its last Test Connection closes; one from the same address
        // and mcIdent after that is a new Test.
        // Refused with CONNFAIL, not MCINVPAR.
        // (conformance: per-test-cap-over-the-tests-life, server-limits-refuse-with-connfail)
        let closed = self.tests.get(&test).map_or(0, |t| t.closed);
        if of_test + closed >= self.limits.test_connections_per_test {
            return Some(SetupRejection::TestConnectionsPerTest);
        }
        if self.reserved_connections(None, evicted) >= self.limits.max_test_connections {
            return Some(SetupRejection::TestConnections);
        }
        // Waiting sockets occupy the share too, but cannot be evicted.
        // Keep the bound when the largest holder has none open yet.
        if evicted.is_none()
            && self
                .test_connections
                .values()
                .filter(|c| !c.activated)
                .count()
                >= self.limits.max_unactivated
        {
            return Some(SetupRejection::TestConnections);
        }
        self.rate_refusal(test, setup, evicted)
    }

    /// Places held by activated Tests, including siblings waiting for
    /// their socket or Activation Request. `including` also counts a
    /// Test whose first activation is being judged; `evicted` is omitted
    /// when judging a Setup that would release it.
    // (conformance: unactivated-test-connection-share)
    fn reserved_connections(&self, including: Option<TestKey>, evicted: Option<SocketId>) -> usize {
        self.test_connections
            .iter()
            .filter(|&(socket, c)| {
                Some(*socket) != evicted
                    && (Some(c.setup.test()) == including
                        || self
                            .tests
                            .get(&c.setup.test())
                            .is_some_and(|t| t.direction.is_some()))
            })
            .count()
    }

    /// The Admitted Rate Cap's refusal of another Test Connection of `test`
    /// for `setup`, or its Test's share's, if any, as if the set-up Test
    /// Connection `evicted` were released first.
    fn rate_refusal(
        &self,
        test: TestKey,
        setup: &Setup,
        evicted: Option<SocketId>,
    ) -> Option<SetupRejection> {
        let (upstream, mbps) = stated_bandwidth(setup.max_bandwidth);
        let cap = self.limits.admitted_rate_cap?;
        if self.rate_in_use(upstream, None, evicted) + u64::from(mbps) > u64::from(cap) {
            return Some(SetupRejection::CapacityExceeded);
        }
        // One Test's share of the cap, all its Test Connections together
        // (conformance: max-test-share-of-the-rate-cap).
        if !setup.operator()
            && (self.test_rate(test, upstream, evicted) + u64::from(mbps)) * 100
                > u64::from(cap) * u64::from(self.limits.max_test_share)
        {
            return Some(SetupRejection::CapacityExceeded);
        }
        None
    }

    /// The maxBandwidth reserved in one direction: that of every admitted
    /// Test Connection of the activated Tests, those waiting for their
    /// socket included, other than those of `except` and than
    /// `evicted`. Each direction has the Admitted Rate Cap to itself, as in
    /// OB-UDPST: a link carries its rate both ways. A Test reserves its rate
    /// when its first Test Connection activates, so a Setup never activated
    /// holds none and can't keep other Tests out of the cap.
    /// (conformance: admitted-rate-cap-per-direction, admitted-rate-reserved-at-activation)
    fn rate_in_use(
        &self,
        upstream: bool,
        except: Option<TestKey>,
        evicted: Option<SocketId>,
    ) -> u64 {
        self.test_connections
            .iter()
            .filter(|&(socket, c)| {
                Some(c.setup.test()) != except
                    && Some(*socket) != evicted
                    && self
                        .tests
                        .get(&c.setup.test())
                        .is_some_and(|t| t.direction.is_some())
            })
            .map(|(_, c)| stated_bandwidth(c.setup.max_bandwidth))
            .filter(|&(up, _)| up == upstream)
            .map(|(_, mbps)| u64::from(mbps))
            .sum()
    }

    /// The maxBandwidth a Test's admitted Test Connections state in one
    /// direction, those waiting for their socket included, other than
    /// `evicted`.
    fn test_rate(&self, test: TestKey, upstream: bool, evicted: Option<SocketId>) -> u64 {
        self.test_connections
            .iter()
            .filter(|&(socket, c)| c.setup.test() == test && Some(*socket) != evicted)
            .map(|(_, c)| stated_bandwidth(c.setup.max_bandwidth))
            .filter(|&(up, _)| up == upstream)
            .map(|(_, mbps)| u64::from(mbps))
            .sum()
    }

    /// The Test Connection a new Setup would evict: when the
    /// Unactivated Test Connections fill their share, one is closed, as a
    /// SYN cache evicts, rather than the new Setup refused, so Setup
    /// Requests never activated can't fill the global cap. It is the
    /// oldest, by its Setup Response, of the Key Table row holding the most
    /// Unactivated Test Connections, those of Unauthenticated Mode counting
    /// as one group; of groups holding as many, the one whose oldest is
    /// oldest gives it up, so with one group the oldest of all goes.
    ///
    /// Evicting the oldest of all would let authentic Setup Requests
    /// admitted at R a second close any Client's Unactivated Test
    /// Connection about share/R seconds after it was admitted, before a
    /// Client with a longer round trip activates it. A flood on one key
    /// instead holds the most and evicts its own, so a Client of another
    /// key keeps its place until it activates, a Multi-Connection Test's
    /// Test Connections too, which are admitted one by one and activated
    /// together, as long as that key holds fewer than half the share at
    /// once. It is a fair share, not a guarantee: a flood split over k keys
    /// holds the share in about k + 1 equal parts, so another key holding
    /// more than about share/(k + 1) at once loses some. Clients of a
    /// flooding key are not protected, nor, in Unauthenticated Mode,
    /// unauthenticated ones: a trust-model limit (docs/security.md).
    ///
    /// Setups still waiting for their socket count toward the share and
    /// their group's holding but aren't evicted: a group holding the most
    /// only in those evicts no one, and the new Setup is refused until
    /// Test Connections open, activate or close. The
    /// runtime opens a socket before it reads the next datagram, so no
    /// Setup is waiting when the next is judged there.
    /// (conformance: unactivated-test-connection-share)
    fn eviction_candidate(&self) -> Option<SocketId> {
        let unactivated = self
            .test_connections
            .values()
            .filter(|c| !c.activated)
            .count();
        if unactivated < self.limits.max_unactivated {
            return None;
        }
        // Each group's Unactivated Test Connections: by Key Table row, `None`
        // for Unauthenticated Mode.
        let mut groups: BTreeMap<Option<usize>, Holding> = BTreeMap::new();
        for (socket, c) in self.test_connections.iter().filter(|(_, c)| !c.activated) {
            let group = groups.entry(c.setup.key.map(|key| key.row)).or_default();
            group.held += 1;
            if let Some(answered_at) = c.answered_at {
                let this = (answered_at, *socket);
                group.oldest = Some(group.oldest.map_or(this, |oldest| oldest.min(this)));
            }
        }
        // The most held; then the oldest answered, a group with none last.
        groups
            .into_values()
            .max_by_key(|group| (group.held, group.oldest.map(Reverse)))
            .and_then(|group| group.oldest)
            .map(|(_, socket)| socket)
    }

    /// Whether `socket` is an admitted Setup's that is still waiting for
    /// its socket.
    pub(crate) fn is_waiting(&self, socket: SocketId) -> bool {
        self.test_connections
            .get(&socket)
            .is_some_and(|c| c.answered_at.is_none())
    }

    /// The socket of an admitted Setup is bound, and its Setup Response
    /// goes out now: it is a set-up Test Connection of its Test, which
    /// begins with the first.
    pub(crate) fn open(
        &mut self,
        socket: SocketId,
        now: MonoTime,
    ) -> Option<&mut TestConnection<T>> {
        let connection = self
            .test_connections
            .get_mut(&socket)
            .filter(|c| c.answered_at.is_none())?;
        connection.answered_at = Some(now);
        self.tests.entry(connection.setup.test()).or_default();
        Some(connection)
    }

    /// A set-up Test Connection.
    pub(crate) fn get(&self, socket: SocketId) -> Option<&TestConnection<T>> {
        self.test_connections
            .get(&socket)
            .filter(|c| c.answered_at.is_some())
    }

    /// A set-up Test Connection, whose data the Server may change.
    pub(crate) fn get_mut(&mut self, socket: SocketId) -> Option<&mut TestConnection<T>> {
        self.test_connections
            .get_mut(&socket)
            .filter(|c| c.answered_at.is_some())
    }

    /// The set-up Test Connections, in socket order.
    pub(crate) fn set_up(&self) -> impl Iterator<Item = (SocketId, &TestConnection<T>)> + '_ {
        self.test_connections
            .iter()
            .filter(|(_, c)| c.answered_at.is_some())
            .map(|(socket, c)| (*socket, c))
    }

    /// The set-up Test Connections, in socket order, whose data the Server
    /// may change.
    pub(crate) fn set_up_mut(
        &mut self,
    ) -> impl Iterator<Item = (SocketId, &mut TestConnection<T>)> + '_ {
        self.test_connections
            .iter_mut()
            .filter(|(_, c)| c.answered_at.is_some())
            .map(|(socket, c)| (*socket, c))
    }

    /// The set-up Test Connection on `socket` has its first valid
    /// Activation Request accepted, for `direction`, unless the limits
    /// refuse it, checked in order: the direction its maxBandwidth was
    /// admitted for, when an Admitted Rate Cap is set; then, for the first
    /// Test Connection of its Test to activate, the Tests per Source, the
    /// global Test Connection cap, the Admitted Rate Cap, both of which
    /// the whole Test then reserves, and the
    /// Quotas, which it then uses; then the direction of the Test's others.
    ///
    /// # Panics
    ///
    /// If `socket` isn't a set-up Test Connection's.
    pub(crate) fn activate(
        &mut self,
        socket: SocketId,
        direction: Direction,
        now: MonoTime,
    ) -> Result<(), ActivationRejection> {
        let connection = self.get(socket).expect("a set-up Test Connection");
        let test = connection.setup.test();
        let key = connection.setup.key;
        let (upstream, mbps) = stated_bandwidth(connection.setup.max_bandwidth);
        // Bandwidth admitted against the cap for one direction can't be
        // used in the other. A Setup Request that states none may leave
        // CHSR_USDIR_BIT clear, as OB-UDPST's Client does, and without a
        // cap the bit decides nothing.
        // (conformance: usdir-must-match-activation-direction)
        if mbps > 0
            && self.limits.admitted_rate_cap.is_some()
            && upstream != (direction == Direction::Upstream)
        {
            return Err(ActivationRejection::MaxBandwidthDirection);
        }
        let first = self
            .tests
            .get(&test)
            .expect("every set-up Test Connection belongs to a Test")
            .direction
            .is_none();
        let source = Source::of(test.client_ip);
        // The first Test Connection to activate a Test checks the Source's
        // limit again: Tests set up at the same time were all admitted.
        if first && self.tests_from(source, test) >= self.limits.tests_per_source {
            return Err(ActivationRejection::TestsPerSource);
        }
        // Reserve every admitted sibling on the first activation, as for
        // the Admitted Rate Cap. A refusal lasts until the whole Test ends:
        // closing a rejected socket must not let the rest activate.
        // (conformance: unactivated-test-connection-share)
        if first
            && (self.tests[&test].connections_refused
                || self.reserved_connections(Some(test), None) > self.limits.max_test_connections)
        {
            self.tests
                .get_mut(&test)
                .expect("every set-up Test Connection belongs to a Test")
                .connections_refused = true;
            return Err(ActivationRejection::TestConnections);
        }
        // Tests admitted together past the Admitted Rate Cap: the first to
        // activate reserves its whole Test's rate, in each direction its
        // Test Connections state, and a later one that no longer fits is
        // refused whole, never left half running.
        if first
            && let Some(cap) = self.limits.admitted_rate_cap
            && [false, true].into_iter().any(|upstream| {
                self.rate_in_use(upstream, Some(test), None) + self.test_rate(test, upstream, None)
                    > u64::from(cap)
            })
        {
            return Err(ActivationRejection::CapacityExceeded);
        }
        // Tests set up together were all within the Quota at Setup: the
        // first Test Connection to activate a Test checks it again, and
        // charges it.
        if first && !self.within_quota(source, key, now) {
            return Err(ActivationRejection::QuotaExceeded);
        }
        let charged = first.then(|| quota_subjects(source, key)).flatten();
        if let Some(charged) = charged {
            self.charge_test(charged, now);
        }
        let test = self
            .tests
            .get_mut(&test)
            .expect("every set-up Test Connection belongs to a Test");
        if charged.is_some() {
            test.charged = charged;
        }
        match test.direction {
            // A Test has exactly one direction, fixed by its first accepted
            // Activation Request (conformance: test-has-one-direction).
            Some(existing) if existing != direction => {
                return Err(ActivationRejection::MixedDirection);
            }
            _ => test.direction = Some(direction),
        }
        if let Some(connection) = self.test_connections.get_mut(&socket) {
            connection.activated = true;
        }
        Ok(())
    }

    /// Load bytes the Data Phase on `socket` sent or received, which its
    /// Test's Quotas are charged when it ends.
    pub(crate) fn add_load_bytes(&mut self, socket: SocketId, bytes: u64) {
        let Some(connection) = self.get(socket) else {
            return;
        };
        if let Some(test) = self.tests.get_mut(&connection.setup.test())
            && test.charged.is_some()
        {
            test.bytes = test.bytes.saturating_add(bytes);
        }
    }

    /// `socket` is gone. A set-up Test Connection is released: its place
    /// is free, and it counts as closed for the rest of its Test's life.
    /// A Test ends once none of its Test Connections is admitted, set up or
    /// waiting for its socket; its Quotas are charged its bytes
    /// then.
    pub(crate) fn close(&mut self, socket: SocketId, now: MonoTime) -> Closed<T> {
        match self.test_connections.get(&socket).map(|c| c.answered_at) {
            None => Closed::Unknown,
            Some(None) => match self.test_connections.remove(&socket) {
                Some(connection) => {
                    self.end_test_if_done(connection.setup.test(), now);
                    Closed::Waiting(connection.setup, connection.data)
                }
                None => Closed::Unknown,
            },
            Some(Some(_)) => match self.release(socket, now) {
                Some(_) => Closed::Released,
                None => Closed::Unknown,
            },
        }
    }

    /// Forgets the set-up Test Connection on `socket`, and ends its Test
    /// if it was the last admitted.
    fn release(&mut self, socket: SocketId, now: MonoTime) -> Option<T> {
        let connection = self.test_connections.remove(&socket)?;
        let key = connection.setup.test();
        if let Some(test) = self.tests.get_mut(&key) {
            test.closed += 1;
        }
        self.end_test_if_done(key, now);
        Some(connection.data)
    }

    /// Ends Test `key` if none of its Test Connections is admitted any
    /// more, charging its Quotas its bytes.
    fn end_test_if_done(&mut self, key: TestKey, now: MonoTime) {
        if self
            .test_connections
            .values()
            .any(|c| c.setup.test() == key)
        {
            return;
        }
        if let Some(Test {
            charged: Some(charged),
            bytes,
            ..
        }) = self.tests.remove(&key)
        {
            self.charge_bytes(charged, bytes, now);
        }
    }

    fn charge_test(&mut self, charged: Charged, now: MonoTime) {
        self.source_quotas
            .charge_test(charged.source, self.limits.source_quota, now);
        if let Some((row, limits)) = charged.row {
            self.key_quotas.charge_test(row, limits, now);
        }
    }

    fn charge_bytes(&mut self, charged: Charged, bytes: u64, now: MonoTime) {
        self.source_quotas
            .charge_bytes(charged.source, self.limits.source_quota, bytes, now);
        if let Some((row, limits)) = charged.row {
            self.key_quotas.charge_bytes(row, limits, bytes, now);
        }
    }

    /// How full each table is.
    pub(crate) fn usage(&self) -> Usage {
        let cap = self
            .limits
            .max_test_connections
            .saturating_add(self.limits.max_unactivated);
        Usage {
            test_connections: (self.test_connections.len(), cap),
            tests: (self.tests.len(), cap),
            source_quotas: self.source_quotas.usage(),
            key_quotas: self.key_quotas.usage(),
        }
    }
}

/// The Quotas a Test from `source`, keyed with `key`, is charged to; none
/// for an Operator Key.
fn quota_subjects(source: Source, key: Option<Key>) -> Option<Charged> {
    if key.is_some_and(|key| key.operator) {
        return None;
    }
    Some(Charged {
        source,
        row: key.map(|key| (key.row, key.quota)),
    })
}

/// Drives the Server's Admission through the steps `data` encodes, as a
/// Server would: Setups from a few Client addresses, so Sources and Tests
/// collide, sockets opening and closing, activations, load bytes and time
/// passing. It keeps its own account of each Test from Admission's answers
/// alone, and after every step holds Admission to it: each table within
/// its limit; the rate held in each direction, by every admitted Test
/// Connection of the activated Tests, within the Admitted Rate Cap (
/// ); each Source's activated Tests within the Tests per Source; and
/// each Test's Test Connections over its life, the closed ones included,
/// within the per-Test cap. Once every socket has closed,
/// nothing may be left. The fuzz target `admission` (fuzz/)
/// feeds it; a test here runs it on pseudorandom input.
///
/// # Panics
///
/// When one of those fails.
#[cfg(any(test, feature = "fuzzing"))]
pub fn drive(data: &[u8]) {
    use std::net::Ipv6Addr;
    use std::time::Duration;

    /// The next byte of the input, 0 once it is used up.
    struct Input<'a> {
        data: &'a [u8],
        at: usize,
    }

    impl Input<'_> {
        fn byte(&mut self) -> u8 {
            let byte = self.data.get(self.at).copied().unwrap_or(0);
            self.at += 1;
            byte
        }

        fn pick<T: Copy>(&mut self, from: &[T]) -> Option<T> {
            let at = usize::from(self.byte());
            (!from.is_empty()).then(|| from[at % from.len()])
        }
    }

    let mut input = Input { data, at: 0 };
    let config = ServerConfig {
        tests_per_source: usize::from(input.byte() % 3) + 1,
        max_test_connections: usize::from(input.byte() % 8) + 1,
        test_connections_per_test: usize::from(input.byte() % 4) + 1,
        max_unactivated_test_connections: match input.byte() % 5 {
            0 => None,
            n => Some(usize::from(n)),
        },
        admitted_rate_cap: match input.byte() % 4 {
            0 => None,
            n => Some(u32::from(n) * 50),
        },
        max_test_share: input.byte() % 101,
        require_max_bandwidth: input.byte() % 2 == 1,
        quota_period: Duration::from_secs(u64::from(input.byte() % 60) + 1),
        source_quota: QuotaLimits {
            tests: match input.byte() % 3 {
                0 => None,
                n => Some(u32::from(n)),
            },
            bytes: match input.byte() % 3 {
                0 => None,
                n => Some(u64::from(n) * 1000),
            },
        },
        ..ServerConfig::default()
    };
    // Two IPv4 Sources, one of them also as an IPv4-mapped address, and
    // two IPv6 /64s.
    let clients: [IpAddr; 6] = [
        IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)),
        IpAddr::V4(Ipv4Addr::new(192, 0, 2, 2)),
        IpAddr::V6(Ipv4Addr::new(192, 0, 2, 1).to_ipv6_mapped()),
        IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)),
        IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 2)),
        IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 1, 0, 0, 0, 1)),
    ];
    let keys = [
        None,
        Some(Key {
            row: 0,
            operator: false,
            quota: QuotaLimits {
                tests: Some(2),
                bytes: None,
            },
        }),
        Some(Key {
            row: 1,
            operator: true,
            quota: QuotaLimits::default(),
        }),
    ];
    let mut admission: Admission<()> = Admission::new(&config);
    let mut account = Account::default();
    let mut now = Duration::from_secs(1);
    while input.at < input.data.len() {
        let at = MonoTime::from_duration(now);
        let sockets: Vec<SocketId> = account.admitted.keys().copied().collect();
        match input.byte() % 6 {
            0 => {
                let ip = input.pick(&clients).expect("some");
                let key = input.pick(&keys).expect("some");
                // Now and then an mcIdent of zero, or an mcIndex not below
                // mcCount.
                let mc_ident = match input.byte() % 16 {
                    0 => 0,
                    n => u16::from(n % 3) + 1,
                };
                let mc_count = input.byte() % 4 + 1;
                let setup = Setup {
                    client: SocketAddr::new(ip, 40000 + u16::from(input.byte() % 3)),
                    mc_ident,
                    mc_index: input.byte() % (mc_count + 1),
                    mc_count,
                    auth_mode: input.byte() % 3,
                    key_id: input.byte() % 2,
                    max_bandwidth: u16::from(input.byte() % 100)
                        | (u16::from(input.byte() % 2) * UPSTREAM_DIRECTION_BIT),
                    key,
                };
                if let Ok(admitted) = admission.admit(setup, (), at) {
                    // The Server closes the evicted one's socket, whose
                    // closing then finds nothing.
                    if let Some(evicted) = admitted.evicted {
                        account.check_eviction(evicted);
                        account.close(evicted);
                        assert!(matches!(admission.close(evicted, at), Closed::Unknown));
                    }
                    account.admit(admitted.socket, &setup);
                }
            }
            1 => {
                if let Some(socket) = input.pick(&sockets)
                    && admission.open(socket, at).is_some()
                {
                    account.open(socket);
                }
            }
            2 => {
                let direction = if input.byte() % 2 == 0 {
                    Direction::Downstream
                } else {
                    Direction::Upstream
                };
                if let Some(socket) = input.pick(&sockets)
                    && admission.get(socket).is_some_and(|c| !c.activated())
                {
                    match admission.activate(socket, direction, at) {
                        Ok(()) => account.activate(socket),
                        // Refused: the Server closes its socket.
                        Err(_) => {
                            assert!(matches!(admission.close(socket, at), Closed::Released));
                            account.close(socket);
                        }
                    }
                }
            }
            3 => {
                if let Some(socket) = input.pick(&sockets) {
                    assert!(!matches!(admission.close(socket, at), Closed::Unknown));
                    account.close(socket);
                }
            }
            4 => {
                if let Some(socket) = input.pick(&sockets) {
                    admission.add_load_bytes(socket, u64::from(input.byte()) * 100);
                }
            }
            _ => now += Duration::from_millis(u64::from(input.byte()) * 100),
        }
        account.check(&config, admission.usage());
    }
    let at = MonoTime::from_duration(now);
    let sockets: Vec<SocketId> = account.admitted.keys().copied().collect();
    for socket in sockets {
        assert!(!matches!(admission.close(socket, at), Closed::Unknown));
        account.close(socket);
    }
    let usage = admission.usage();
    assert_eq!(usage.test_connections.0, 0, "{usage:?}");
    assert_eq!(usage.tests.0, 0, "{usage:?}");
    assert!(account.tests.is_empty());
}

/// The fuzz driver's own account of the Tests, kept from Admission's
/// answers alone, to hold Admission to.
#[cfg(any(test, feature = "fuzzing"))]
#[derive(Default)]
struct Account {
    /// Each admitted Test Connection's Test and the rate it states.
    admitted: BTreeMap<SocketId, Accounted>,
    /// Each Test with an admitted Test Connection.
    tests: HashMap<TestKey, Life>,
}

/// An admitted Test Connection, as the fuzz driver saw it admitted.
#[cfg(any(test, feature = "fuzzing"))]
struct Accounted {
    test: TestKey,
    upstream: bool,
    mbps: u32,
    /// Its socket opened.
    set_up: bool,
    /// Its own Activation Request was accepted.
    activated: bool,
    /// The Key Table row it was authenticated with; `None` in
    /// Unauthenticated Mode.
    row: Option<usize>,
}

/// A Test, as the fuzz driver saw it: from its first admitted Test
/// Connection until none is left.
#[cfg(any(test, feature = "fuzzing"))]
#[derive(Default)]
struct Life {
    admitted: usize,
    /// Its Test Connections that were set up and have closed.
    closed: usize,
    /// One of its Test Connections activated.
    activated: bool,
}

#[cfg(any(test, feature = "fuzzing"))]
impl Account {
    fn admit(&mut self, socket: SocketId, setup: &Setup) {
        let (upstream, mbps) = stated_bandwidth(setup.max_bandwidth);
        let test = setup.test();
        self.admitted.insert(
            socket,
            Accounted {
                test,
                upstream,
                mbps,
                set_up: false,
                activated: false,
                row: setup.key.map(|key| key.row),
            },
        );
        self.tests.entry(test).or_default().admitted += 1;
    }

    /// Fails unless `evicted` was a set-up Unactivated Test Connection of
    /// a group holding the most Unactivated Test Connections.
    fn check_eviction(&self, evicted: SocketId) {
        let connection = &self.admitted[&evicted];
        assert!(connection.set_up && !connection.activated);
        let mut held: HashMap<Option<usize>, usize> = HashMap::new();
        for c in self.admitted.values().filter(|c| !c.activated) {
            *held.entry(c.row).or_default() += 1;
        }
        let most = held.values().copied().max().unwrap_or(0);
        assert_eq!(held[&connection.row], most, "{held:?}");
    }

    fn open(&mut self, socket: SocketId) {
        if let Some(connection) = self.admitted.get_mut(&socket) {
            connection.set_up = true;
        }
    }

    fn activate(&mut self, socket: SocketId) {
        let connection = self.admitted.get_mut(&socket).expect("admitted");
        connection.activated = true;
        let test = connection.test;
        self.tests.get_mut(&test).expect("admitted").activated = true;
    }

    fn close(&mut self, socket: SocketId) {
        let connection = self.admitted.remove(&socket).expect("admitted");
        let life = self.tests.get_mut(&connection.test).expect("admitted");
        life.admitted -= 1;
        life.closed += usize::from(connection.set_up);
        if life.admitted == 0 {
            self.tests.remove(&connection.test);
        }
    }

    /// Fails unless Admission, whose tables are as full as `usage` says,
    /// kept to `config`'s limits by this account.
    fn check(&self, config: &ServerConfig, usage: Usage) {
        for (entries, limit) in [
            usage.test_connections,
            usage.tests,
            usage.source_quotas,
            usage.key_quotas,
        ] {
            assert!(entries <= limit, "{usage:?}");
        }
        assert_eq!(usage.test_connections.0, self.admitted.len(), "{usage:?}");
        let reserved: usize = self
            .tests
            .values()
            .filter(|test| test.activated)
            .map(|test| test.admitted)
            .sum();
        assert!(
            reserved <= config.max_test_connections,
            "{reserved} reserved"
        );
        let unactivated = self.admitted.values().filter(|c| !c.activated).count();
        let share = config
            .max_unactivated_test_connections
            .unwrap_or(config.max_test_connections / 2)
            .max(config.test_connections_per_test);
        assert!(
            unactivated <= share,
            "{unactivated} unactivated, share {share}"
        );
        if let Some(cap) = config.admitted_rate_cap {
            for upstream in [false, true] {
                let held: u64 = self
                    .admitted
                    .values()
                    .filter(|c| c.upstream == upstream && self.tests[&c.test].activated)
                    .map(|c| u64::from(c.mbps))
                    .sum();
                assert!(
                    held <= u64::from(cap),
                    "{held} Mbps held upstream={upstream}, over the Admitted Rate Cap of {cap}"
                );
            }
        }
        let mut activated: HashMap<Source, usize> = HashMap::new();
        for (test, life) in &self.tests {
            assert!(
                life.admitted + life.closed <= config.test_connections_per_test,
                "{test:?}: {} admitted and {} closed, past the per-Test cap of {}",
                life.admitted,
                life.closed,
                config.test_connections_per_test
            );
            if life.activated {
                *activated.entry(Source::of(test.client_ip)).or_default() += 1;
            }
        }
        assert!(
            activated
                .values()
                .all(|&tests| tests <= config.tests_per_source),
            "{activated:?}"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv6Addr;
    use std::time::Duration;

    use super::*;

    /// The Client's address, with another port for its other sockets.
    fn client(port: u16) -> SocketAddr {
        SocketAddr::from((Ipv4Addr::new(192, 0, 2, 1), port))
    }

    fn at(ms: u64) -> MonoTime {
        MonoTime::from_duration(Duration::from_millis(ms))
    }

    /// Test Connection 0 of 1 of Test `mc_ident`, from the Client's
    /// address, unauthenticated, stating no maxBandwidth.
    fn setup(mc_ident: u16) -> Setup {
        Setup {
            client: client(40000),
            mc_ident,
            mc_index: 0,
            mc_count: 1,
            auth_mode: auth_mode::UNAUTHENTICATED,
            key_id: 0,
            max_bandwidth: 0,
            key: None,
        }
    }

    /// Test Connection `mc_index` of `mc_count` of Test `mc_ident`, from
    /// its own port of the Client's address.
    fn connection(mc_ident: u16, mc_index: u8, mc_count: u8) -> Setup {
        Setup {
            client: client(40000 + u16::from(mc_index)),
            mc_index,
            mc_count,
            ..setup(mc_ident)
        }
    }

    const OPERATOR: Option<Key> = Some(Key {
        row: 0,
        operator: true,
        quota: QuotaLimits {
            tests: None,
            bytes: None,
        },
    });

    fn admission_with(change: impl FnOnce(&mut ServerConfig)) -> Admission<()> {
        let mut config = ServerConfig::default();
        change(&mut config);
        Admission::new(&config)
    }

    fn admit(admission: &mut Admission<()>, setup: Setup, now: MonoTime) -> Admitted {
        admission
            .admit(setup, (), now)
            .unwrap_or_else(|(refusal, ())| panic!("{setup:?}: {refusal:?}"))
    }

    /// Admits `setup` and opens its socket.
    fn set_up(admission: &mut Admission<()>, setup: Setup, now: MonoTime) -> SocketId {
        let socket = admit(admission, setup, now).socket;
        admission.open(socket, now).expect("waiting for its socket");
        socket
    }

    /// Sets `setup` up and activates it.
    fn activated(
        admission: &mut Admission<()>,
        setup: Setup,
        direction: Direction,
        now: MonoTime,
    ) -> SocketId {
        let socket = set_up(admission, setup, now);
        admission
            .activate(socket, direction, now)
            .unwrap_or_else(|reason| panic!("{setup:?}: {reason:?}"));
        socket
    }

    fn refusal(admission: &mut Admission<()>, setup: Setup, now: MonoTime) -> Option<Refusal> {
        admission
            .admit(setup, (), now)
            .err()
            .map(|(refusal, ())| refusal)
    }

    fn rejected(reason: SetupRejection) -> Option<Refusal> {
        Some(Refusal::Rejected(reason))
    }

    fn mcinvpar(error: MultiConnectionError) -> Option<Refusal> {
        rejected(SetupRejection::MultiConnection(error))
    }

    const DOWN: Direction = Direction::Downstream;
    const UP: Direction = Direction::Upstream;

    /// maxBandwidth `mbps` for an Upstream Test: CHSR_USDIR_BIT set.
    fn upstream(mbps: u16) -> u16 {
        mbps | UPSTREAM_DIRECTION_BIT
    }

    // A copy of an admitted Setup Request from the same address, waiting
    // for its socket or set up, is ignored rather than refused; from
    // another port its mcIndex is in use.
    #[test]
    fn a_copy_of_an_admitted_setup_is_ignored() {
        let mut admission = admission_with(|_| {});
        admit(&mut admission, setup(1), at(0));
        assert_eq!(
            refusal(&mut admission, setup(1), at(0)),
            Some(Refusal::Duplicate)
        );
        let socket = set_up(&mut admission, setup(2), at(0));
        assert!(admission.get(socket).is_some());
        assert_eq!(
            refusal(&mut admission, setup(2), at(1)),
            Some(Refusal::Duplicate)
        );
        let other_port = Setup {
            client: client(40009),
            ..setup(2)
        };
        assert_eq!(
            refusal(&mut admission, other_port, at(1)),
            mcinvpar(MultiConnectionError::IndexInUse)
        );
    }

    #[test]
    fn a_zero_mc_ident_is_refused() {
        let mut admission = admission_with(|_| {});
        assert_eq!(
            refusal(&mut admission, setup(0), at(0)),
            mcinvpar(MultiConnectionError::IdentZero)
        );
    }

    #[udpstp_req::req("RFC9946-6.1-L1")]
    #[test]
    fn an_mc_index_not_below_mc_count_is_refused() {
        for (index, count) in [(2, 2), (0, 0), (255, 1)] {
            let mut admission = admission_with(|_| {});
            assert_eq!(
                refusal(&mut admission, connection(1, index, count), at(0)),
                mcinvpar(MultiConnectionError::IndexNotBelowCount),
                "{index}/{count}"
            );
        }
    }

    // docs/spec.md (the Test Connections of a Test agree on mcCount)
    #[test]
    fn a_test_connection_with_another_mc_count_is_refused() {
        let mut admission = admission_with(|_| {});
        set_up(&mut admission, connection(1, 0, 2), at(0));
        assert_eq!(
            refusal(&mut admission, connection(1, 1, 3), at(0)),
            mcinvpar(MultiConnectionError::CountDiffers)
        );
    }

    // ADR 0013: the reason names the Server log's MCINVPAR refusal, a
    // stable interface, so a request at odds with several of its Test's
    // Test Connections in different ways gets the oldest one's reason:
    // here the older one's other mcCount, not the newer one's mcIndex.
    #[test]
    fn a_request_wrong_in_several_ways_gets_the_oldest_test_connections_reason() {
        let mut admission = admission_with(|_| {});
        set_up(&mut admission, connection(1, 0, 2), at(0));
        set_up(&mut admission, connection(1, 1, 2), at(1));
        // mcIndex 1, which the newer holds, and mcCount 3, from another
        // port, so it isn't the newer's copy.
        let other = Setup {
            client: client(40009),
            ..connection(1, 1, 3)
        };
        assert_eq!(
            refusal(&mut admission, other, at(2)),
            mcinvpar(MultiConnectionError::CountDiffers)
        );
    }

    // docs/spec.md (the Test Connections of a Test agree on authMode): an
    // unauthenticated Test Connection can't join an authenticated Test
    #[test]
    fn a_test_connection_with_another_auth_mode_is_refused() {
        let mut admission = admission_with(|_| {});
        let keyed = Setup {
            auth_mode: auth_mode::MODE_1,
            key_id: 7,
            ..connection(1, 0, 2)
        };
        set_up(&mut admission, keyed, at(0));
        assert_eq!(
            refusal(&mut admission, connection(1, 1, 2), at(0)),
            mcinvpar(MultiConnectionError::AuthModeDiffers)
        );
    }

    // docs/spec.md (the Test Connections of a Test agree on keyId)
    #[test]
    fn a_test_connection_with_another_key_id_is_refused() {
        let mut admission = admission_with(|_| {});
        let keyed = |mc_index, key_id| Setup {
            auth_mode: auth_mode::MODE_1,
            key_id,
            ..connection(1, mc_index, 2)
        };
        set_up(&mut admission, keyed(0, 7), at(0));
        assert_eq!(
            refusal(&mut admission, keyed(1, 8), at(0)),
            mcinvpar(MultiConnectionError::KeyIdDiffers)
        );
    }

    // keyId is only compared in an authenticated mode: in Unauthenticated
    // Mode it carries nothing.
    #[test]
    fn in_unauthenticated_mode_the_key_id_is_not_compared() {
        let mut admission = admission_with(|_| {});
        set_up(&mut admission, connection(1, 0, 2), at(0));
        let other_key_id = Setup {
            key_id: 9,
            ..connection(1, 1, 2)
        };
        assert_eq!(refusal(&mut admission, other_key_id, at(0)), None);
    }

    // docs/spec.md ("Completeness is never required, since a Test may span
    // several Servers"): indexes 0 and 1 of 3 run
    #[test]
    fn a_test_runs_without_every_mc_index() {
        let mut admission = admission_with(|_| {});
        activated(&mut admission, connection(1, 0, 3), DOWN, at(0));
        activated(&mut admission, connection(1, 1, 3), DOWN, at(0));
    }

    // docs/spec.md (mcIndex is unique within a Test, not across Tests)
    #[test]
    fn a_different_test_may_reuse_an_mc_index() {
        let mut admission = admission_with(|c| c.tests_per_source = 2);
        activated(&mut admission, setup(1), DOWN, at(0));
        activated(&mut admission, setup(2), DOWN, at(0));
    }

    // docs/spec.md (mcIndex unique among the Test's admitted Test
    // Connections, including one still waiting for its socket)
    #[test]
    fn an_mc_index_held_by_a_setup_waiting_for_its_socket_is_in_use() {
        let mut admission = admission_with(|_| {});
        admit(&mut admission, connection(1, 0, 2), at(0));
        let same_index = Setup {
            client: client(40009),
            ..connection(1, 0, 2)
        };
        assert_eq!(
            refusal(&mut admission, same_index, at(0)),
            mcinvpar(MultiConnectionError::IndexInUse)
        );
    }

    // RFC 9946 §4.2, docs/spec.md: only activated Tests count against their
    // Source, at Setup; the first Test Connection to activate checks again
    #[test]
    fn only_activated_tests_count_against_their_source() {
        let mut admission = admission_with(|_| {});
        let first = set_up(&mut admission, setup(1), at(0));
        let second = set_up(&mut admission, setup(2), at(0));
        assert_eq!(admission.activate(first, DOWN, at(1)), Ok(()));
        assert_eq!(
            admission.activate(second, DOWN, at(1)),
            Err(ActivationRejection::TestsPerSource)
        );
        assert_eq!(
            refusal(&mut admission, setup(3), at(2)),
            rejected(SetupRejection::TestsPerSource)
        );
    }

    #[test]
    fn a_higher_per_source_cap_admits_both() {
        let mut admission = admission_with(|c| c.tests_per_source = 2);
        activated(&mut admission, setup(1), DOWN, at(0));
        activated(&mut admission, setup(2), DOWN, at(0));
    }

    // "Regardless of the number of flows": the Test Connections of one
    // Test count once.
    #[udpstp_req::req("RFC9946-4.2-1")]
    #[test]
    fn the_test_connections_of_one_test_are_one_test() {
        let mut admission = admission_with(|_| {});
        activated(&mut admission, connection(1, 0, 2), DOWN, at(0));
        activated(&mut admission, connection(1, 1, 2), DOWN, at(0));
    }

    // docs/spec.md: a Source is an IPv4 address, IPv4-mapped ones included,
    // or an IPv6 /64 (conformance: path-approximated-by-source)
    #[test]
    fn a_source_is_an_ipv4_address_or_an_ipv6_64() {
        let from = |ip: IpAddr, mc_ident| Setup {
            client: SocketAddr::new(ip, 40000),
            ..setup(mc_ident)
        };
        let v4 = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1));
        let mapped = IpAddr::V6(Ipv4Addr::new(192, 0, 2, 1).to_ipv6_mapped());
        let one = IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1));
        let same_64 = IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0xffff, 0, 0, 2));
        let other_64 = IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 1, 0, 0, 0, 1));
        let mut admission = admission_with(|_| {});
        activated(&mut admission, from(v4, 1), DOWN, at(0));
        activated(&mut admission, from(one, 1), DOWN, at(0));
        for ip in [mapped, same_64] {
            assert_eq!(
                refusal(&mut admission, from(ip, 2), at(0)),
                rejected(SetupRejection::TestsPerSource),
                "{ip}"
            );
        }
        assert_eq!(refusal(&mut admission, from(other_64, 2), at(0)), None);
    }

    /// A Quota of `tests` Tests per 10 s.
    fn tests_quota(tests: u32) -> impl FnOnce(&mut ServerConfig) {
        move |c: &mut ServerConfig| {
            c.quota_period = Duration::from_secs(10);
            c.source_quota = QuotaLimits {
                tests: Some(tests),
                bytes: None,
            };
        }
    }

    // One Test per 10 s: the second, 7 s after the first, is refused; a
    // third, 12 s after, runs.
    #[udpstp_req::req("RFC9946-11-6")]
    #[test]
    fn the_quota_refills_over_the_quota_period() {
        let mut admission = admission_with(tests_quota(1));
        let first = activated(&mut admission, setup(1), DOWN, at(0));
        admission.close(first, at(5_000));
        assert_eq!(
            refusal(&mut admission, setup(2), at(7_000)),
            rejected(SetupRejection::QuotaExceeded)
        );
        activated(&mut admission, setup(3), DOWN, at(12_000));
    }

    // RFC 9946 §11 item 6 concerns subscribers: an Operator configures the
    // measurement.
    #[test]
    fn operator_keys_are_exempt() {
        let mut admission = admission_with(|c| {
            c.source_quota = QuotaLimits {
                tests: Some(1),
                bytes: Some(1),
            };
        });
        let operator = |mc_ident| Setup {
            key: OPERATOR,
            ..setup(mc_ident)
        };
        let first = activated(&mut admission, operator(1), DOWN, at(0));
        admission.add_load_bytes(first, 1_000_000);
        admission.close(first, at(1));
        activated(&mut admission, operator(2), DOWN, at(2));
    }

    // : a Test counts once, when its first Test Connection activates
    #[test]
    fn a_test_uses_its_quota_once_when_it_first_activates() {
        let mut admission = admission_with(|c| {
            tests_quota(1)(c);
            c.tests_per_source = 2;
        });
        activated(&mut admission, connection(1, 0, 2), DOWN, at(0));
        activated(&mut admission, connection(1, 1, 2), DOWN, at(0));
        assert_eq!(
            refusal(&mut admission, setup(2), at(1)),
            rejected(SetupRejection::QuotaExceeded)
        );
    }

    // : a Test's load bytes are charged when it ends, so they may
    // overdraw its Quota
    #[test]
    fn a_tests_bytes_are_charged_when_it_ends() {
        let mut admission = admission_with(|c| {
            c.tests_per_source = 3;
            c.source_quota = QuotaLimits {
                tests: None,
                bytes: Some(1000),
            };
        });
        let first = activated(&mut admission, setup(1), DOWN, at(0));
        admission.add_load_bytes(first, 2000);
        activated(&mut admission, setup(2), DOWN, at(1));
        admission.close(first, at(2));
        assert_eq!(
            refusal(&mut admission, setup(3), at(3)),
            rejected(SetupRejection::QuotaExceeded)
        );
    }

    // : a Key Table row's Quota holds whichever Source uses it
    #[test]
    fn a_key_rows_quota_holds_across_sources() {
        let mut admission = admission_with(|_| {});
        let keyed = |ip: Ipv4Addr| Setup {
            client: SocketAddr::from((ip, 40000)),
            auth_mode: auth_mode::MODE_1,
            key: Some(Key {
                row: 3,
                operator: false,
                quota: QuotaLimits {
                    tests: Some(1),
                    bytes: None,
                },
            }),
            ..setup(1)
        };
        activated(
            &mut admission,
            keyed(Ipv4Addr::new(192, 0, 2, 1)),
            DOWN,
            at(0),
        );
        assert_eq!(
            refusal(&mut admission, keyed(Ipv4Addr::new(198, 51, 100, 7)), at(1)),
            rejected(SetupRejection::QuotaExceeded)
        );
    }

    // docs/spec.md (a setting, off by default, requires maxBandwidth and
    // rejects its absence with NOMAXBW)
    #[test]
    fn nomaxbw_only_where_a_max_bandwidth_is_required() {
        let mut requiring = admission_with(|c| c.require_max_bandwidth = true);
        assert_eq!(
            refusal(&mut requiring, setup(1), at(0)),
            rejected(SetupRejection::MaxBandwidthRequired)
        );
        let stated = Setup {
            max_bandwidth: upstream(10),
            ..setup(2)
        };
        assert_eq!(refusal(&mut requiring, stated, at(0)), None);
        assert_eq!(refusal(&mut admission_with(|_| {}), setup(1), at(0)), None);
    }

    //  (a Test admits at most test_connections_per_test Test
    // Connections over its whole life, not only at once)
    #[test]
    fn a_test_kept_alive_cannot_start_new_test_connections_past_its_cap() {
        let mut admission = admission_with(|c| c.test_connections_per_test = 2);
        let first = activated(&mut admission, connection(1, 0, 3), DOWN, at(0));
        let second = activated(&mut admission, connection(1, 1, 3), DOWN, at(0));
        admission.close(second, at(5_000));
        assert_eq!(
            refusal(&mut admission, connection(1, 2, 3), at(8_000)),
            rejected(SetupRejection::TestConnectionsPerTest)
        );
        // Once the Test ends, the same address and mcIdent start a new one.
        admission.close(first, at(10_000));
        set_up(&mut admission, connection(1, 2, 3), at(11_000));
    }

    // Admission holds a place in the Unactivated Test Connection share
    // before its socket opens (conformance: unactivated-test-connection-share).
    #[test]
    fn a_setup_waiting_for_its_socket_holds_its_admission() {
        let mut admission = admission_with(|c| {
            c.max_test_connections = 1;
            c.test_connections_per_test = 1;
            c.tests_per_source = 2;
        });
        admit(&mut admission, setup(1), at(0));
        assert_eq!(
            refusal(&mut admission, setup(2), at(0)),
            rejected(SetupRejection::TestConnections)
        );
    }

    // docs/spec.md (maxBandwidth counted per Test Connection, with
    // CHSR_USDIR_BIT masked, against each direction's cap)
    #[udpstp_req::req("RFC9946-6.1-L7", "RFC9946-6.1-L8")]
    #[test]
    fn the_bandwidth_cap_holds_per_direction_with_usdir_masked() {
        let mut admission = admission_with(|c| {
            c.admitted_rate_cap = Some(100);
            c.tests_per_source = 3;
        });
        let operator = |mc_ident, max_bandwidth| Setup {
            max_bandwidth,
            key: OPERATOR,
            ..setup(mc_ident)
        };
        // 100 Mbps each way fills a 100 Mbps cap exactly: the Upstream
        // Test's field reads 0x8064, which counts as 100.
        activated(&mut admission, operator(1, upstream(100)), UP, at(0));
        activated(&mut admission, operator(2, 100), DOWN, at(0));
        assert_eq!(
            refusal(&mut admission, operator(3, 1), at(0)),
            rejected(SetupRejection::CapacityExceeded)
        );
    }

    //  (a Setup never activated holds no share of the Admitted Rate
    // Cap, so it can't keep other Tests out of it; the first to activate keeps
    // the rate)
    #[test]
    fn an_unactivated_setup_holds_no_rate() {
        let mut admission = admission_with(|c| {
            c.admitted_rate_cap = Some(100);
            c.tests_per_source = 3;
        });
        let operator = |mc_ident| Setup {
            max_bandwidth: 100,
            key: OPERATOR,
            ..setup(mc_ident)
        };
        let idle = set_up(&mut admission, operator(1), at(0));
        activated(&mut admission, operator(2), DOWN, at(10));
        assert_eq!(
            admission.activate(idle, DOWN, at(20)),
            Err(ActivationRejection::CapacityExceeded)
        );
    }

    //  (a Test reserves its whole rate when its first Test Connection
    // activates, so of two Multi-Connection Tests racing past the
    // Admitted Rate Cap one runs whole and the other is refused whole)
    #[test]
    fn a_multi_connection_test_is_admitted_or_refused_whole() {
        let mut admission = admission_with(|c| {
            c.admitted_rate_cap = Some(100);
            c.tests_per_source = 3;
        });
        // Tests 1 and 2, 50 Mbps each Test Connection; their Activations
        // interleave.
        let operator = |mc_ident, mc_index| Setup {
            max_bandwidth: 50,
            key: OPERATOR,
            ..connection(mc_ident, mc_index, 2)
        };
        let a0 = set_up(&mut admission, operator(1, 0), at(0));
        let b0 = set_up(&mut admission, operator(2, 0), at(0));
        let a1 = set_up(&mut admission, operator(1, 1), at(0));
        let b1 = set_up(&mut admission, operator(2, 1), at(0));
        let refused = Err(ActivationRejection::CapacityExceeded);
        assert_eq!(admission.activate(a0, DOWN, at(1)), Ok(()));
        assert_eq!(admission.activate(b0, DOWN, at(1)), refused);
        assert_eq!(admission.activate(a1, DOWN, at(1)), Ok(()));
        assert_eq!(admission.activate(b1, DOWN, at(1)), refused);
    }

    //  (the same in the Upstream direction; the refused Test
    // Connection is released)
    #[test]
    fn an_upstream_test_that_no_longer_fits_is_refused_at_activation_and_released() {
        let mut admission = admission_with(|c| {
            c.admitted_rate_cap = Some(150);
            c.tests_per_source = 3;
        });
        let operator = |mc_ident| Setup {
            max_bandwidth: upstream(100),
            key: OPERATOR,
            ..setup(mc_ident)
        };
        let first = set_up(&mut admission, operator(1), at(0));
        let second = set_up(&mut admission, operator(2), at(0));
        assert_eq!(admission.activate(first, UP, at(1)), Ok(()));
        assert_eq!(
            admission.activate(second, UP, at(1)),
            Err(ActivationRejection::CapacityExceeded)
        );
        assert!(matches!(admission.close(second, at(1)), Closed::Released));
        assert_eq!(admission.usage().test_connections.0, 1);
    }

    // : the share counts all of a Test's Test Connections
    #[test]
    fn the_share_counts_every_test_connection_of_the_test() {
        let mut admission = admission_with(|c| c.admitted_rate_cap = Some(100));
        let consumer = |mc_index| Setup {
            max_bandwidth: 30,
            ..connection(1, mc_index, 2)
        };
        set_up(&mut admission, consumer(0), at(0));
        assert_eq!(
            refusal(&mut admission, consumer(1), at(0)),
            rejected(SetupRejection::CapacityExceeded)
        );
    }

    // : Operator Keys are exempt, bound by the cap alone
    #[test]
    fn an_operator_key_may_hold_the_whole_cap() {
        let mut admission = admission_with(|c| c.admitted_rate_cap = Some(100));
        let whole = Setup {
            max_bandwidth: 100,
            key: OPERATOR,
            ..setup(1)
        };
        activated(&mut admission, whole, DOWN, at(0));
    }

    // Without a cap the direction bit decides nothing, so a Client that
    // sets it wrongly still runs.
    #[test]
    fn without_a_bandwidth_cap_the_direction_bit_is_not_checked() {
        let mut admission = admission_with(|_| {});
        let marked = Setup {
            max_bandwidth: upstream(100),
            ..setup(1)
        };
        activated(&mut admission, marked, DOWN, at(0));
    }

    // OB-UDPST's Client sets CHSR_USDIR_BIT only when it states a
    // maxBandwidth, so an Upstream Test that states none may leave it clear.
    #[udpstp_req::req("RFC9946-6.1-L8")]
    #[test]
    fn an_upstream_test_stating_no_max_bandwidth_may_leave_usdir_clear() {
        let mut admission = admission_with(|c| c.admitted_rate_cap = Some(100));
        activated(&mut admission, setup(1), UP, at(0));
    }

    // docs/spec.md: a Test has one direction, fixed by its first accepted
    // Activation Request (conformance: test-has-one-direction)
    #[test]
    fn a_test_has_one_direction() {
        let mut admission = admission_with(|_| {});
        activated(&mut admission, connection(1, 0, 2), DOWN, at(0));
        let other = set_up(&mut admission, connection(1, 1, 2), at(0));
        assert_eq!(
            admission.activate(other, UP, at(1)),
            Err(ActivationRejection::MixedDirection)
        );
    }

    /// A cap of 4 Test Connections, 1 per Test, 10 Tests per Source.
    fn four_connections_one_per_test(c: &mut ServerConfig) {
        c.max_test_connections = 4;
        c.test_connections_per_test = 1;
        c.tests_per_source = 10;
    }

    // The unactivated share defaults to half the global cap;
    // a new Setup beyond it closes the oldest.
    // (conformance: unactivated-test-connection-share)
    #[test]
    fn a_new_setup_evicts_the_oldest_unactivated_test_connection() {
        let mut admission = admission_with(four_connections_one_per_test);
        // Answered in the other order than admitted: the older answer goes.
        let first = admit(&mut admission, setup(1), at(0)).socket;
        let second = admit(&mut admission, setup(2), at(1)).socket;
        admission.open(second, at(2));
        admission.open(first, at(3));
        let admitted = admit(&mut admission, setup(3), at(4));
        assert_eq!(admitted.evicted, Some(second));
        assert!(admission.get(second).is_none() && admission.get(first).is_some());
    }

    //  (activated Test Connections don't count)
    #[test]
    fn activated_test_connections_dont_count() {
        let mut admission = admission_with(four_connections_one_per_test);
        for mc_ident in 1..=4 {
            let socket = admit(&mut admission, setup(mc_ident), at(0)).socket;
            admission.open(socket, at(0));
            assert_eq!(admission.activate(socket, DOWN, at(0)), Ok(()));
        }
    }

    // The global cap still refuses when activated Tests fill it.
    #[udpstp_req::req("RFC9946-11-4", "RFC9097-10-4")]
    #[test]
    fn a_cap_full_of_activated_tests_still_refuses() {
        let mut admission = admission_with(|c| {
            c.max_test_connections = 2;
            c.test_connections_per_test = 1;
            c.tests_per_source = 10;
        });
        activated(&mut admission, setup(1), DOWN, at(0));
        activated(&mut admission, setup(2), DOWN, at(0));
        assert_eq!(
            refusal(&mut admission, setup(3), at(0)),
            rejected(SetupRejection::TestConnections)
        );
    }

    //  (the share is configurable)
    #[test]
    fn the_share_is_configurable() {
        let mut admission = admission_with(|c| {
            four_connections_one_per_test(c);
            c.max_unactivated_test_connections = Some(1);
        });
        let first = set_up(&mut admission, setup(1), at(0));
        let second = admit(&mut admission, setup(2), at(1));
        assert_eq!(second.evicted, Some(first));
        admission.open(second.socket, at(1));
        assert_eq!(
            admit(&mut admission, setup(3), at(2)).evicted,
            Some(second.socket)
        );
    }

    //  (a whole Multi-Connection Test fits: the share is at least the
    // per-Test cap)
    #[test]
    fn a_whole_multi_connection_test_fits_the_share() {
        let mut admission = admission_with(|c| {
            c.max_test_connections = 4;
            c.test_connections_per_test = 4;
        });
        for mc_index in 0..4 {
            let socket = admit(&mut admission, connection(1, mc_index, 4), at(0));
            assert_eq!(socket.evicted, None);
            admission.open(socket.socket, at(0));
        }
    }

    //  (a Setup still waiting for its socket counts toward the share,
    // but isn't evicted)
    #[test]
    fn setups_waiting_for_their_socket_count() {
        let mut admission = admission_with(four_connections_one_per_test);
        let first = set_up(&mut admission, setup(1), at(0));
        let waiting = admit(&mut admission, setup(2), at(10)).socket;
        assert_eq!(admit(&mut admission, setup(3), at(10)).evicted, Some(first));
        assert!(admission.is_waiting(waiting));
    }

    /// Test `mc_ident`'s only Test Connection, from the Client's address,
    /// in mode 1 with Key Table row `row` (keyId `row`), a Consumer's.
    fn keyed(mc_ident: u16, row: usize) -> Setup {
        Setup {
            auth_mode: auth_mode::MODE_1,
            key_id: row as u8,
            key: Some(Key {
                row,
                operator: false,
                quota: QuotaLimits::default(),
            }),
            ..setup(mc_ident)
        }
    }

    /// Test `mc_ident`'s only Test Connection, unauthenticated, from its
    /// own Source.
    fn unauthenticated_from(mc_ident: u16, host: u8) -> Setup {
        Setup {
            client: SocketAddr::from((Ipv4Addr::new(198, 51, 100, host), 40000)),
            ..setup(mc_ident)
        }
    }

    /// A cap of 8 Test Connections, 1 per Test, 10 Tests per Source: a
    /// share of 4.
    fn eight_connections_one_per_test(c: &mut ServerConfig) {
        c.max_test_connections = 8;
        c.test_connections_per_test = 1;
        c.tests_per_source = 10;
    }

    // The Unactivated Test Connections have their own share, even with
    // only one activated Test Connection's place left in the global cap.
    // (conformance: unactivated-test-connection-share)
    #[test]
    fn a_busy_server_still_evicts_a_flooding_keys_unactivated_test_connections() {
        let mut admission = admission_with(eight_connections_one_per_test);
        for mc_ident in 1..=7 {
            activated(&mut admission, keyed(mc_ident, 1), DOWN, at(0));
        }
        for mc_ident in 8..=11 {
            set_up(&mut admission, keyed(mc_ident, 0), at(u64::from(mc_ident)));
        }
        let client = set_up(&mut admission, keyed(12, 1), at(12));
        for mc_ident in 13..=24 {
            let admitted = admit(&mut admission, keyed(mc_ident, 0), at(u64::from(mc_ident)));
            assert!(admitted.evicted.is_some());
            assert_ne!(admitted.evicted, Some(client));
            admission.open(admitted.socket, at(u64::from(mc_ident)));
        }
        assert_eq!(admission.activate(client, DOWN, at(1_000)), Ok(()));
    }

    // A Test whose whole reservation no longer fits stays refused even
    // after the runtime closes its first refused Test Connection.
    // (conformance: unactivated-test-connection-share)
    #[test]
    fn a_test_that_no_longer_fits_cannot_activate_a_remaining_sibling() {
        let mut admission = admission_with(|c| {
            c.max_test_connections = 2;
            c.test_connections_per_test = 2;
            c.tests_per_source = 3;
            c.max_unactivated_test_connections = Some(4);
        });
        let first = set_up(&mut admission, connection(2, 0, 2), at(0));
        let second = set_up(&mut admission, connection(2, 1, 2), at(0));
        let occupied = activated(&mut admission, setup(1), DOWN, at(1));
        let refused = admission.activate(first, DOWN, at(2));
        assert_eq!(refused, Err(ActivationRejection::TestConnections));
        admission.close(first, at(3));
        admission.close(occupied, at(3));
        assert_eq!(admission.activate(second, DOWN, at(4)), refused);
        admission.close(second, at(5));
        // Once every Test Connection closes, the same mcIdent may start
        // a new Test, and its reservation is released on close.
        let restarted = activated(&mut admission, connection(2, 0, 2), DOWN, at(6));
        admission.close(restarted, at(7));
        assert_eq!(admission.usage().test_connections.0, 0);
    }

    // The whole Test reserves places, including a sibling whose socket
    // has not opened yet (conformance: unactivated-test-connection-share).
    #[test]
    fn the_first_activation_reserves_siblings_waiting_for_their_sockets() {
        let mut admission = admission_with(|c| {
            c.max_test_connections = 2;
            c.test_connections_per_test = 3;
            c.tests_per_source = 3;
        });
        let first = set_up(&mut admission, connection(1, 0, 3), at(0));
        let waiting = admit(&mut admission, connection(1, 1, 3), at(1)).socket;
        let competing = set_up(&mut admission, setup(2), at(1));
        assert_eq!(admission.activate(first, DOWN, at(2)), Ok(()));
        assert_eq!(
            admission.activate(competing, DOWN, at(3)),
            Err(ActivationRejection::TestConnections)
        );
        // A later Setup for the activated Test also needs a free place;
        // evicting an unrelated unactivated one cannot provide it.
        assert_eq!(
            refusal(&mut admission, connection(1, 2, 3), at(3)),
            rejected(SetupRejection::TestConnections)
        );
        admission.close(first, at(4));
        admission.open(waiting, at(5));
        assert_eq!(admission.activate(waiting, DOWN, at(6)), Ok(()));
        admission.close(competing, at(7));
        admission.close(waiting, at(7));
        activated(&mut admission, setup(3), DOWN, at(8));
    }

    // A configured share can exceed the activated Test Connection cap.
    // (conformance: unactivated-test-connection-share)
    #[test]
    fn the_unactivated_share_can_exceed_the_global_cap() {
        let mut admission = admission_with(|c| {
            c.max_test_connections = 2;
            c.test_connections_per_test = 1;
            c.max_unactivated_test_connections = Some(4);
        });
        let oldest = set_up(&mut admission, setup(1), at(0));
        for mc_ident in 2..=4 {
            let admitted = admit(&mut admission, setup(mc_ident), at(u64::from(mc_ident)));
            assert_eq!(admitted.evicted, None);
            admission.open(admitted.socket, at(u64::from(mc_ident)));
        }
        assert_eq!(admit(&mut admission, setup(5), at(5)).evicted, Some(oldest));
    }

    //  (when the share is full, the Key Table row holding the most
    // Unactivated Test Connections gives up its oldest: a key flooding at
    // the share's rate evicts its own, and another key's Client keeps its
    // place until it activates, however long its round trip)
    // (conformance: unactivated-test-connection-share)
    #[test]
    fn a_flooding_key_evicts_its_own_unactivated_test_connections() {
        let mut admission = admission_with(eight_connections_one_per_test);
        // The other key's Client is the oldest of all.
        let client = set_up(&mut admission, keyed(1, 1), at(0));
        let mut flood = Vec::new();
        for mc_ident in 2..=4 {
            flood.push(set_up(
                &mut admission,
                keyed(mc_ident, 0),
                at(u64::from(mc_ident)),
            ));
        }
        // Many times the share, each evicting the flood's own oldest.
        for mc_ident in 5..=40 {
            let admitted = admit(&mut admission, keyed(mc_ident, 0), at(u64::from(mc_ident)));
            assert_eq!(admitted.evicted, Some(flood.remove(0)), "{mc_ident}");
            admission.open(admitted.socket, at(u64::from(mc_ident)));
            flood.push(admitted.socket);
        }
        assert_eq!(admission.activate(client, DOWN, at(1_000)), Ok(()));
    }

    //  (the Unactivated Test Connections of Unauthenticated Mode are one
    // group, whatever their Sources)
    // (conformance: unactivated-test-connection-share)
    #[test]
    fn unauthenticated_test_connections_are_one_group() {
        let mut admission = admission_with(eight_connections_one_per_test);
        let client = set_up(&mut admission, keyed(1, 0), at(0));
        let first = set_up(&mut admission, unauthenticated_from(2, 1), at(1));
        set_up(&mut admission, unauthenticated_from(3, 2), at(2));
        set_up(&mut admission, unauthenticated_from(4, 3), at(3));
        let admitted = admit(&mut admission, unauthenticated_from(5, 4), at(4));
        assert_eq!(admitted.evicted, Some(first));
        assert!(admission.get(client).is_some());
        // An unauthenticated Client is protected from a keyed flood alike.
        let mut admission = admission_with(eight_connections_one_per_test);
        let client = set_up(&mut admission, unauthenticated_from(1, 1), at(0));
        let first = set_up(&mut admission, keyed(2, 0), at(1));
        set_up(&mut admission, keyed(3, 0), at(2));
        set_up(&mut admission, keyed(4, 0), at(3));
        assert_eq!(
            admit(&mut admission, keyed(5, 0), at(4)).evicted,
            Some(first)
        );
        assert!(admission.get(client).is_some());
    }

    //  (a Multi-Connection Test's Test Connections, admitted one by one
    // and activated together, aren't evicted by its own later Setups nor by
    // another key flooding)
    // (conformance: unactivated-test-connection-share)
    #[test]
    fn a_multi_connection_test_survives_another_keys_flood() {
        let mut admission = admission_with(|c| {
            c.max_test_connections = 20;
            c.test_connections_per_test = 4;
            c.tests_per_source = 10;
        });
        let flood_setup = |mc_ident: u16| Setup {
            client: SocketAddr::from((Ipv4Addr::new(203, 0, 113, 1), 40000)),
            ..keyed(mc_ident, 0)
        };
        // The flood fills the share of 10 first.
        for mc_ident in 1..=10 {
            set_up(
                &mut admission,
                flood_setup(mc_ident),
                at(u64::from(mc_ident)),
            );
        }
        let mut test = Vec::new();
        for mc_index in 0..4 {
            let setup = Setup {
                auth_mode: auth_mode::MODE_1,
                key_id: 1,
                key: keyed(0, 1).key,
                ..connection(100, mc_index, 4)
            };
            let now = at(20 + u64::from(mc_index));
            let admitted = admit(&mut admission, setup, now);
            assert!(!test.contains(&admitted.evicted.expect("the share is full")));
            admission.open(admitted.socket, now);
            test.push(admitted.socket);
        }
        for mc_ident in 11..=60 {
            let now = at(30 + u64::from(mc_ident));
            let admitted = admit(&mut admission, flood_setup(mc_ident), now);
            assert!(!test.contains(&admitted.evicted.expect("the share is full")));
            admission.open(admitted.socket, now);
        }
        for socket in test {
            assert_eq!(admission.activate(socket, DOWN, at(1_000)), Ok(()));
        }
    }

    //  (of two groups holding as many, the one whose oldest is oldest
    // gives it up: with one group, the oldest of all goes, as before)
    #[test]
    fn of_two_groups_holding_as_many_the_older_gives_up_its_oldest() {
        let mut admission = admission_with(eight_connections_one_per_test);
        let oldest = set_up(&mut admission, keyed(1, 1), at(0));
        set_up(&mut admission, keyed(2, 0), at(1));
        set_up(&mut admission, keyed(3, 0), at(2));
        set_up(&mut admission, keyed(4, 1), at(3));
        assert_eq!(
            admit(&mut admission, keyed(5, 2), at(4)).evicted,
            Some(oldest)
        );
        // Answered in another order than admitted: the older answer goes.
        let mut admission = admission_with(eight_connections_one_per_test);
        let first = admit(&mut admission, keyed(1, 1), at(0)).socket;
        let second = admit(&mut admission, keyed(2, 0), at(1)).socket;
        admission.open(second, at(2));
        admission.open(first, at(3));
        set_up(&mut admission, keyed(3, 1), at(4));
        set_up(&mut admission, keyed(4, 0), at(5));
        assert_eq!(
            admit(&mut admission, keyed(5, 2), at(6)).evicted,
            Some(second)
        );
    }

    //  (Setups waiting for their socket count toward their group's
    // holding but aren't evicted: a group holding the most only in Setups
    // still waiting evicts no one else's)
    #[test]
    fn a_group_holding_the_most_only_in_waiting_setups_evicts_no_one_else() {
        let mut admission = admission_with(eight_connections_one_per_test);
        let client = set_up(&mut admission, keyed(1, 1), at(0));
        for mc_ident in 2..=4 {
            admit(&mut admission, keyed(mc_ident, 0), at(u64::from(mc_ident)));
        }
        assert_eq!(
            refusal(&mut admission, keyed(5, 0), at(5)),
            rejected(SetupRejection::TestConnections)
        );
        assert!(admission.get(client).is_some());
        assert_eq!(admission.usage().test_connections.0, 4);
    }

    // ADR 0003 (an eviction releases the evicted Test Connection's
    // Admission: its place is free, it counts as closed for its Test, and
    // its Test ends when it was the last)
    #[test]
    fn an_eviction_releases_the_evicted_test_connections_admission() {
        let mut admission = admission_with(eight_connections_one_per_test);
        let client = set_up(&mut admission, keyed(1, 1), at(0));
        for mc_ident in 2..=4 {
            set_up(&mut admission, keyed(mc_ident, 0), at(u64::from(mc_ident)));
        }
        assert_eq!(admission.usage().test_connections.0, 4);
        assert_eq!(admission.usage().tests.0, 4);
        let admitted = admit(&mut admission, keyed(5, 0), at(5));
        let evicted = admitted.evicted.expect("the share is full");
        assert_ne!(evicted, client);
        // One in, one out; the evicted one's Test ended with it, and the
        // new one's begins when its socket opens.
        assert_eq!(admission.usage().test_connections.0, 4);
        assert_eq!(admission.usage().tests.0, 3);
        assert!(matches!(admission.close(evicted, at(6)), Closed::Unknown));
        admission.open(admitted.socket, at(6));
        assert_eq!(admission.usage().tests.0, 4);
    }

    // No one is evicted for a Setup refused anyway.
    #[test]
    fn a_setup_refused_anyway_evicts_no_one() {
        let mut admission = admission_with(|c| {
            four_connections_one_per_test(c);
            c.max_unactivated_test_connections = Some(1);
            c.admitted_rate_cap = Some(100);
        });
        let idle = set_up(&mut admission, setup(1), at(0));
        let over = Setup {
            max_bandwidth: 200,
            ..setup(2)
        };
        assert_eq!(
            refusal(&mut admission, over, at(10)),
            rejected(SetupRejection::CapacityExceeded)
        );
        assert!(admission.get(idle).is_some());
    }

    // Nor for a Setup that an eviction would let past the global cap but
    // the Admitted Rate Cap still refuses: only "a Setup that would
    // otherwise be admitted" evicts.
    // (conformance: unactivated-test-connection-share)
    #[test]
    fn a_setup_the_rate_cap_refuses_evicts_no_one_from_a_full_cap() {
        let mut admission = admission_with(|c| {
            c.max_test_connections = 2;
            c.test_connections_per_test = 1;
            c.max_unactivated_test_connections = Some(1);
            c.admitted_rate_cap = Some(100);
            c.tests_per_source = 10;
        });
        let whole = Setup {
            max_bandwidth: 100,
            key: OPERATOR,
            ..setup(1)
        };
        activated(&mut admission, whole, DOWN, at(0));
        let idle = set_up(&mut admission, setup(2), at(10));
        let more = Setup {
            max_bandwidth: 10,
            key: OPERATOR,
            ..setup(3)
        };
        assert_eq!(
            refusal(&mut admission, more, at(20)),
            rejected(SetupRejection::CapacityExceeded)
        );
        assert!(admission.get(idle).is_some());
    }

    // The Setup checks come in their recorded order: a Setup failing two
    // gets the first (conformance: setup-check-order).
    #[test]
    fn a_setup_is_refused_by_the_first_check_it_fails() {
        // A copy is ignored before its fields are judged.
        let mut admission = admission_with(|_| {});
        set_up(&mut admission, connection(1, 0, 2), at(0));
        let copy = Setup {
            mc_count: 3,
            ..connection(1, 0, 2)
        };
        assert_eq!(
            refusal(&mut admission, copy, at(0)),
            Some(Refusal::Duplicate)
        );
        // MCINVPAR before the Tests per Source.
        let mut admission = admission_with(|_| {});
        activated(&mut admission, setup(1), DOWN, at(0));
        assert_eq!(
            refusal(&mut admission, setup(0), at(0)),
            mcinvpar(MultiConnectionError::IdentZero)
        );
        // The Tests per Source before the Quota.
        let mut admission = admission_with(tests_quota(1));
        activated(&mut admission, setup(1), DOWN, at(0));
        assert_eq!(
            refusal(&mut admission, setup(2), at(0)),
            rejected(SetupRejection::TestsPerSource)
        );
        // The Quota before NOMAXBW.
        let mut admission = admission_with(|c| {
            tests_quota(1)(c);
            c.require_max_bandwidth = true;
        });
        let stated = Setup {
            max_bandwidth: 10,
            ..setup(1)
        };
        let first = activated(&mut admission, stated, DOWN, at(0));
        admission.close(first, at(1));
        assert_eq!(
            refusal(&mut admission, setup(2), at(2)),
            rejected(SetupRejection::QuotaExceeded)
        );
        // NOMAXBW before the per-Test cap.
        let mut admission = admission_with(|c| {
            c.require_max_bandwidth = true;
            c.test_connections_per_test = 1;
        });
        set_up(
            &mut admission,
            Setup {
                max_bandwidth: 10,
                ..connection(1, 0, 2)
            },
            at(0),
        );
        assert_eq!(
            refusal(&mut admission, connection(1, 1, 2), at(0)),
            rejected(SetupRejection::MaxBandwidthRequired)
        );
        // The per-Test cap before the global one.
        let mut admission = admission_with(|c| {
            c.max_test_connections = 1;
            c.test_connections_per_test = 1;
        });
        activated(&mut admission, connection(1, 0, 2), DOWN, at(0));
        assert_eq!(
            refusal(&mut admission, connection(1, 1, 2), at(0)),
            rejected(SetupRejection::TestConnectionsPerTest)
        );
        // The global cap before the Admitted Rate Cap.
        let mut admission = admission_with(|c| {
            c.max_test_connections = 1;
            c.admitted_rate_cap = Some(100);
            c.tests_per_source = 2;
        });
        let whole = Setup {
            max_bandwidth: 100,
            key: OPERATOR,
            ..setup(1)
        };
        activated(&mut admission, whole, DOWN, at(0));
        assert_eq!(
            refusal(
                &mut admission,
                Setup {
                    mc_ident: 2,
                    ..whole
                },
                at(0)
            ),
            rejected(SetupRejection::TestConnections)
        );
    }

    // At activation the direction the maxBandwidth was admitted for comes
    // first, then the first activation's Tests per Source, Admitted Rate
    // Cap and Quota, in that order.
    #[test]
    fn an_activation_is_refused_by_the_first_check_it_fails() {
        // The direction before the Tests per Source.
        let mut admission = admission_with(|c| c.admitted_rate_cap = Some(100));
        let marked = Setup {
            max_bandwidth: upstream(10),
            ..setup(2)
        };
        let idle = set_up(&mut admission, marked, at(0));
        activated(&mut admission, setup(1), DOWN, at(0));
        assert_eq!(
            admission.activate(idle, DOWN, at(1)),
            Err(ActivationRejection::MaxBandwidthDirection)
        );
        // The Tests per Source before the Admitted Rate Cap.
        let mut admission = admission_with(|c| c.admitted_rate_cap = Some(100));
        let whole = |mc_ident| Setup {
            max_bandwidth: 100,
            key: OPERATOR,
            ..setup(mc_ident)
        };
        let first = set_up(&mut admission, whole(1), at(0));
        let second = set_up(&mut admission, whole(2), at(0));
        assert_eq!(admission.activate(first, DOWN, at(1)), Ok(()));
        assert_eq!(
            admission.activate(second, DOWN, at(1)),
            Err(ActivationRejection::TestsPerSource)
        );
        // The Admitted Rate Cap before the Quota: the first two use both.
        let mut admission = admission_with(|c| {
            tests_quota(2)(c);
            c.admitted_rate_cap = Some(100);
            c.tests_per_source = 3;
        });
        let consumer = |mc_ident| Setup {
            max_bandwidth: 50,
            ..setup(mc_ident)
        };
        let first = set_up(&mut admission, consumer(1), at(0));
        let second = set_up(&mut admission, consumer(2), at(0));
        let third = set_up(&mut admission, consumer(3), at(0));
        assert_eq!(admission.activate(first, DOWN, at(1)), Ok(()));
        assert_eq!(admission.activate(second, DOWN, at(1)), Ok(()));
        assert_eq!(
            admission.activate(third, DOWN, at(1)),
            Err(ActivationRejection::CapacityExceeded)
        );
    }

    // : an activated Test's Test Connections hold their rate from their
    // admission, before their sockets open, so two admitted together can't
    // take a Test past the Admitted Rate Cap (record
    // admitted-rate-reserved-at-activation: a Test holds its whole rate)
    #[test]
    fn an_activated_tests_test_connections_hold_their_rate_before_their_sockets_open() {
        let mut admission = admission_with(|c| c.admitted_rate_cap = Some(100));
        let operator = |mc_index| Setup {
            max_bandwidth: 50,
            key: OPERATOR,
            ..connection(1, mc_index, 3)
        };
        activated(&mut admission, operator(0), DOWN, at(0));
        admit(&mut admission, operator(1), at(1));
        assert_eq!(
            refusal(&mut admission, operator(2), at(2)),
            rejected(SetupRejection::CapacityExceeded)
        );
    }

    // , : a Test's first activation reserves its rate in every
    // direction its Test Connections state, so one that states both can't
    // take the other direction past the Admitted Rate Cap
    #[test]
    fn a_first_activation_reserves_each_direction_its_test_states() {
        let mut admission = admission_with(|c| {
            c.admitted_rate_cap = Some(100);
            c.tests_per_source = 2;
        });
        let operator = |mc_ident, mc_index, max_bandwidth| Setup {
            max_bandwidth,
            key: OPERATOR,
            ..connection(mc_ident, mc_index, 2)
        };
        // Test 1 states 10 Mbps Downstream and 60 Upstream; Test 2, 60
        // Upstream, activates first.
        let downstream = set_up(&mut admission, operator(1, 0, 10), at(0));
        set_up(&mut admission, operator(1, 1, upstream(60)), at(0));
        activated(&mut admission, operator(2, 0, upstream(60)), UP, at(1));
        assert_eq!(
            admission.activate(downstream, DOWN, at(2)),
            Err(ActivationRejection::CapacityExceeded)
        );
    }

    // : a Test lives while any of its Test Connections is admitted, one
    // waiting for its socket included, so the next to activate after its
    // last set-up one closed isn't the Test's first: it uses no Quota again
    #[test]
    fn a_test_lives_on_in_a_test_connection_waiting_for_its_socket() {
        let mut admission = admission_with(tests_quota(1));
        let first = activated(&mut admission, connection(1, 0, 2), DOWN, at(0));
        let waiting = admit(&mut admission, connection(1, 1, 2), at(1)).socket;
        admission.close(first, at(2));
        admission
            .open(waiting, at(3))
            .expect("waiting for its socket");
        assert_eq!(admission.activate(waiting, DOWN, at(4)), Ok(()));
    }

    // , : and its closed Test Connections still count against the
    // per-Test cap
    #[test]
    fn a_test_waiting_on_one_test_connection_keeps_its_closed_count() {
        let mut admission = admission_with(|c| c.test_connections_per_test = 2);
        let first = activated(&mut admission, connection(1, 0, 3), DOWN, at(0));
        let waiting = admit(&mut admission, connection(1, 1, 3), at(1)).socket;
        admission.close(first, at(2));
        admission
            .open(waiting, at(3))
            .expect("waiting for its socket");
        assert_eq!(
            refusal(&mut admission, connection(1, 2, 3), at(4)),
            rejected(SetupRejection::TestConnectionsPerTest)
        );
    }

    // ADR 0003: a socket's closing releases its Test Connection's place;
    // one never opened is returned for the Server to refuse.
    #[test]
    fn closing_a_socket_releases_its_place() {
        let mut admission = admission_with(|c| {
            c.max_test_connections = 1;
            c.tests_per_source = 2;
        });
        let first = activated(&mut admission, setup(1), DOWN, at(0));
        assert_eq!(
            refusal(&mut admission, setup(2), at(1)),
            rejected(SetupRejection::TestConnections)
        );
        assert!(matches!(admission.close(first, at(2)), Closed::Released));
        assert!(matches!(admission.close(first, at(2)), Closed::Unknown));
        let waiting = admit(&mut admission, setup(2), at(3)).socket;
        match admission.close(waiting, at(4)) {
            Closed::Waiting(setup, ()) => assert_eq!(setup.mc_ident, 2),
            _ => panic!("the Setup waited for its socket"),
        }
        assert_eq!(
            admission.usage(),
            Usage {
                test_connections: (0, 25),
                tests: (0, 25),
                source_quotas: (0, 4096),
                key_quotas: (0, 4096),
            }
        );
    }

    // : the fuzz target's driver keeps Admission's invariants on
    // pseudorandom input.
    #[test]
    fn the_fuzz_driver_keeps_its_invariants() {
        let mut state = 0x5EED_u64;
        for _ in 0..500 {
            let data: Vec<u8> = (0..256)
                .map(|_| {
                    // SplitMix64.
                    state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
                    let mut z = state;
                    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
                    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
                    (z ^ (z >> 31)) as u8
                })
                .collect();
            drive(&data);
        }
    }
}
