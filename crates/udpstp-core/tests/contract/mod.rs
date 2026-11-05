//! The plan-executor contract (docs/spec.md story 119): what any Data
//! Phase executor must do with the Send Plans a Load Sender's `DataPhase`
//! hands it, judged only from the Load PDUs it puts on the wire. One suite
//! runs against the simulator's executor (`tests/plan_executor.rs`, S1)
//! and against the real Data Phase Engine on loopback
//! (`crates/udpstp-runtime/src/contract_tests.rs`, S2b), which includes
//! this file.
//!
//! - **Ordering.** Every Load PDU goes once, in order, numbered from 1:
//!   partial sends leave Deferred Datagrams, never gaps (GLOSSARY.md
//!   Deferred Datagram).
//! - **Burst spacing.** A Send Plan's Load PDUs share one lpduTime, stamped
//!   as it goes. The n-th Send Plan never goes before the n-th slot of the
//!   Transmitter's schedule, since a Burst is never sent early, nor are
//!   there more Send Plans than slots; and it keeps up with the schedule
//!   within the executor's tolerance, accounting for reported Skipped
//!   Bursts (RFC 9946 §7.1; RFC 9097 §8.1).
//! - **Partial-send reporting.** What the executor reports sent (the
//!   `Sent` sts) is what reached the wire: never more, and less only by
//!   the st still open.
//! - **Timestamp domain.** lpduTime reads as the wall clock when the Load
//!   PDU was sent: the Test Time Base plus the offset fixed at activation.
//!
//! An executor that runs outside a simulated Test, such as the real
//! engine, gets its Data Phases from [`activate`]: the Client's
//! Multi-Connection Coordinator and a Server core, as production runs
//! them, run the Control Phase of a Fixed-Rate Test in memory.

#![allow(dead_code)]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::num::{NonZeroU8, NonZeroU16};
use std::time::Duration;

use udpstp_core::{
    ClientAuth, ClientConfig, Coordinator, DataPhase, DatagramMeta, DelayVariation, Direction,
    Endpoint, IpVersion, KeyEntry, KeyRole, KeyTable, Lifetime, Now, Output, RateAdjAlgorithm,
    RowRequest, Server, ServerConfig, SocketId, TestPlan,
};

/// pduId of the Load PDU.
const LOAD_PDU_ID: [u8; 2] = [0xBE, 0xEF];

/// A Load PDU as the observer saw it.
#[derive(Debug, Clone, Copy)]
pub struct Observed {
    /// An independent wall-clock reference (since the Unix epoch): its
    /// arrival where that is known, or a witness of the executor's
    /// stamping time projected through an activation clock pair.
    pub wall: Duration,
    /// Its first 32 bytes: the Load PDU header.
    pub header: [u8; 32],
    /// Its UDP payload length.
    pub len: usize,
}

impl Observed {
    /// The datagram as a Load PDU, if it is one. The timestamp reference
    /// used when checking it must be independent of lpduTime.
    pub fn load_pdu(wall: Duration, payload: &[u8], len: usize) -> Option<Self> {
        if payload.len() < 32 || payload[..2] != LOAD_PDU_ID {
            return None;
        }
        let mut header = [0; 32];
        header.copy_from_slice(&payload[..32]);
        Some(Self { wall, header, len })
    }

    pub fn seq_no(&self) -> u32 {
        u32::from_be_bytes(self.header[4..8].try_into().unwrap())
    }

    /// lpduTime, since the Unix epoch.
    pub fn lpdu_time(&self) -> Duration {
        let secs = u32::from_be_bytes(self.header[20..24].try_into().unwrap());
        let nanos = u32::from_be_bytes(self.header[24..28].try_into().unwrap());
        Duration::new(secs.into(), nanos)
    }

    pub fn is_stop2(&self) -> bool {
        // testAction, RFC 9946 §8.1. STOP2 can be sent immediately (§9).
        self.header[2] == 2
    }
}

/// Periodic Send Plans, excluding immediate terminal STOP2 traffic.
pub fn periodic_plans(observed: &[Observed]) -> Vec<Duration> {
    let mut plans = Vec::new();
    for pdu in observed.iter().filter(|pdu| !pdu.is_stop2()) {
        let at = pdu.lpdu_time();
        if plans.last() != Some(&at) {
            plans.push(at);
        }
    }
    plans
}

/// What an executor must meet: exact for the simulator, looser for a real
/// host.
#[derive(Debug, Clone, Copy)]
pub struct Tolerance {
    /// How much of the schedule may be missing beyond reported Skipped
    /// Bursts, as a share, such as deliberately Withheld Bursts.
    pub missing: f64,
    /// How far lpduTime may be from the independent wall-clock reference:
    /// clock read-out, and path delay when arrival is the reference.
    pub domain: Duration,
    /// How late after its slot a Send Plan may go out: a real engine's
    /// catch-up budget (ADR 0012).
    pub late: Duration,
}

impl Tolerance {
    /// A simulator: no delay, nothing late.
    pub const EXACT: Self = Self {
        missing: 0.0,
        domain: Duration::ZERO,
        late: Duration::ZERO,
    };
}

/// What the Load Sender was to do, and what it reported.
#[derive(Debug, Clone, Copy)]
pub struct Schedule {
    /// The Transmitter's interval: 1 ms or 100 µs. The row must use one
    /// Transmitter only (rows 1-99 in tens, or 100-1000 in hundreds).
    pub interval: Duration,
    /// Its first slot, by the Load Sender's wall clock; unknown, the first
    /// Send Plan's lpduTime.
    pub first_slot: Option<Duration>,
    /// Load PDUs and their UDP payload bytes the executor reported sent,
    /// summed over the `Sent` sts.
    pub reported_datagrams: u64,
    pub reported_bytes: u64,
    /// The most Load PDUs of one st: an st's worth may not be reported yet.
    pub per_st: u64,
    /// Skipped Bursts reported by the executor over this schedule.
    pub skipped_bursts: u64,
}

/// Checks `observed`, in arrival order, against the contract: the reasons
/// it fails, if it does.
pub fn check(observed: &[Observed], schedule: Schedule, tolerance: Tolerance) -> Vec<String> {
    let mut failures = Vec::new();
    if observed.len() < 2 {
        return vec![format!("only {} Load PDUs observed", observed.len())];
    }
    // Ordering.
    let seqs: Vec<u32> = observed.iter().map(Observed::seq_no).collect();
    if seqs[0] != 1 {
        failures.push(format!("the first Load PDU is numbered {}", seqs[0]));
    }
    if let Some(w) = seqs.windows(2).find(|w| w[1] != w[0] + 1) {
        failures.push(format!("lpduSeqNo {} followed by {}", w[0], w[1]));
    }
    // Burst spacing: a Send Plan is a run of one lpduTime.
    let mut plans: Vec<Duration> = Vec::new();
    for o in observed {
        if plans.last() != Some(&o.lpdu_time()) {
            plans.push(o.lpdu_time());
        }
    }
    if let Some(w) = plans.windows(2).find(|w| w[1] < w[0]) {
        failures.push(format!("lpduTime went back from {:?} to {:?}", w[0], w[1]));
    }
    let periodic = periodic_plans(observed);
    if let Some(last) = periodic.last() {
        let first = schedule.first_slot.unwrap_or(periodic[0]);
        let early = periodic.iter().enumerate().find(|&(n, &at)| {
            let slot = first + schedule.interval * u32::try_from(n).unwrap();
            at < slot
        });
        if let Some((n, at)) = early {
            failures.push(format!(
                "Send Plan {n} went {:?} before its slot",
                first + schedule.interval * u32::try_from(n).unwrap() - *at
            ));
        }
        let span = last.saturating_sub(first);
        let slots = (span.as_nanos() / schedule.interval.as_nanos()) as f64 + 1.0;
        let least = slots * (1.0 - tolerance.missing);
        let covered = periodic.len() as f64 + schedule.skipped_bursts as f64;
        if covered < least.floor() {
            failures.push(format!(
                "{} Send Plans plus {} Skipped Bursts in {span:?}, fewer than {least:.0} of {slots:.0} slots",
                periodic.len(), schedule.skipped_bursts
            ));
        }
    }
    // Partial-send reporting. The st open when the executor stopped isn't
    // reported. On a real host it may hold more than any st that was:
    // the Send Plans sent late into it from the st before (catch-up), and
    // one more, whose Deferred Datagrams a partial send carried over.
    let (datagrams, bytes) = (
        observed.len() as u64,
        observed.iter().map(|o| o.len as u64).sum::<u64>(),
    );
    let largest_plan = observed
        .chunk_by(|a, b| a.lpdu_time() == b.lpdu_time())
        .map(|plan| plan.len() as u64)
        .max()
        .unwrap_or(0);
    let carried = if tolerance.late.is_zero() {
        0
    } else {
        let late_plans = tolerance.late.as_nanos() / schedule.interval.as_nanos();
        (u64::try_from(late_plans).unwrap_or(u64::MAX) + 1).saturating_mul(largest_plan)
    };
    let unreported = schedule.per_st + carried;
    if schedule.reported_datagrams > datagrams
        || datagrams > schedule.reported_datagrams + unreported
    {
        failures.push(format!(
            "{} Load PDUs reported sent, {datagrams} observed (an st is {}, {carried} more may be carried into the last)",
            schedule.reported_datagrams, schedule.per_st
        ));
    }
    if schedule.reported_bytes > bytes {
        failures.push(format!(
            "{} bytes reported sent, {bytes} observed",
            schedule.reported_bytes
        ));
    }
    // Timestamp domain.
    if let Some(o) = observed
        .iter()
        .find(|o| o.lpdu_time().abs_diff(o.wall) > tolerance.domain)
    {
        failures.push(format!(
            "Load PDU {} stamped {:?} had clock reference {:?}",
            o.seq_no(),
            o.lpdu_time(),
            o.wall
        ));
    }
    failures
}

/// The Server's Standard Port: only a name, since the Control Phase runs
/// in memory.
const STANDARD: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1);

/// A Key Table of one Operator Key, which a Fixed-Rate Test needs.
fn operator_keys() -> KeyTable {
    let mut row = KeyEntry::new(
        "operator",
        7,
        b"0123456789abcdef".to_vec(),
        Lifetime::new(None, None),
        Lifetime::new(None, None),
    );
    row.role = KeyRole::Operator {
        max_test_interval: None,
    };
    let mut keys = KeyTable::default();
    keys.insert(row).expect("one row");
    keys
}

fn meta(socket: SocketId, source: SocketAddr, local: SocketAddr, len: usize) -> DatagramMeta {
    DatagramMeta {
        socket,
        source,
        local,
        ip_version: IpVersion::V4,
        tos: None,
        actual_len: len,
    }
}

/// The Server's configuration for a Fixed-Rate Test: its defaults, with
/// one Operator Key.
pub fn server_config() -> ServerConfig {
    ServerConfig {
        keys: operator_keys(),
        ..ServerConfig::default()
    }
}

/// A Fixed-Rate Test at `row` in `direction` of one Test Connection to
/// `server`, in mode 1 with the Operator Key of [`server_config`], for
/// 5 s: its plan and the Client's configuration.
pub fn fixed_rate_test(
    server: SocketAddr,
    direction: Direction,
    row: u16,
) -> (TestPlan, ClientConfig) {
    let plan = TestPlan {
        servers: vec![server],
        alternate_server_addresses: Vec::new(),
        test_connections: NonZeroU8::MIN,
        minimum: NonZeroU8::MIN,
        time_limit: Duration::from_secs(3),
        max_bandwidth: 0,
        auth_mode_1: Vec::new(),
    };
    let config = ClientConfig {
        direction,
        auth: Some(ClientAuth::Mode1 {
            keys: operator_keys(),
            key_id: None,
        }),
        row_request: RowRequest::FixedRate(row),
        unacceptable_coercions: Vec::new(),
        random_payload: false,
        delay_variation: DelayVariation::default(),
        checksum: false,
        jumbo: true,
        traditional_mtu: false,
        test_int_time: 5,
        algorithm: RateAdjAlgorithm::B,
        dscp: 0,
    };
    (plan, config)
}

/// Runs the Control Phase of a Fixed-Rate Test at `row` in `direction` in
/// memory, in mode 1 with an Operator Key, the Client's one Test
/// Connection at `client_addr` and the Server's Ephemeral Port at
/// `test_port`, reading the time from `now`: the Client's Data Phase, then
/// the Server's, and the Client's activation clock pair.
pub fn activate(
    now: impl Fn() -> Now,
    direction: Direction,
    row: u16,
    client_addr: SocketAddr,
    test_port: SocketAddr,
) -> (DataPhase, DataPhase, Now) {
    activate_lasting(now, direction, row, 5, client_addr, test_port)
}

/// As [`activate`], the Test lasting `seconds`: its testIntTime.
pub fn activate_lasting(
    now: impl Fn() -> Now,
    direction: Direction,
    row: u16,
    seconds: u16,
    client_addr: SocketAddr,
    test_port: SocketAddr,
) -> (DataPhase, DataPhase, Now) {
    let mut server = Server::new(server_config());
    let (plan, mut config) = fixed_rate_test(STANDARD, direction, row);
    config.test_int_time = seconds;
    let mut client = Coordinator::new(plan, NonZeroU16::new(9).unwrap(), config, now());
    // The Client's one Test Connection's socket, once opened.
    let mut connection = None;
    let mut ephemeral = None;
    let mut server_data_phase = None;
    let mut client_activation = None;
    for _ in 0..100 {
        while let Some(output) = client.poll_output() {
            match output {
                Output::OpenSocket { socket, .. } => {
                    connection = Some(socket);
                    client.handle_socket_opened(now(), socket, client_addr);
                }
                Output::Transmit(t) => {
                    let socket = if t.destination == STANDARD {
                        SocketId::PRIMARY
                    } else {
                        ephemeral.expect("a Setup Response first")
                    };
                    let meta = meta(socket, client_addr, t.destination, t.payload.len());
                    server.handle_datagram(now(), meta, &t.payload);
                }
                Output::StartDataPhase { socket, peer } => {
                    assert_eq!(peer, test_port);
                    let data_phase = client.take_data_phase(socket).expect("announced");
                    let server = server_data_phase.expect("the Server activated first");
                    return (
                        data_phase,
                        server,
                        client_activation.expect("Activation Response delivered"),
                    );
                }
                _ => {}
            }
        }
        while let Some(output) = server.poll_output() {
            match output {
                Output::Transmit(t) => {
                    let source = if t.socket == SocketId::PRIMARY {
                        STANDARD
                    } else {
                        test_port
                    };
                    let socket = connection.expect("the Client's socket sent the request");
                    let meta = meta(socket, source, client_addr, t.payload.len());
                    let delivered_at = now();
                    client.handle_datagram(delivered_at, meta, &t.payload);
                    if source == test_port {
                        client_activation = Some(delivered_at);
                    }
                }
                Output::OpenSocket { socket, .. } => {
                    ephemeral = Some(socket);
                    server.handle_socket_opened(now(), socket, test_port);
                }
                Output::StartDataPhase { socket, .. } => {
                    server_data_phase = server.take_data_phase(socket);
                }
                _ => {}
            }
        }
    }
    panic!("the Test wasn't activated");
}
