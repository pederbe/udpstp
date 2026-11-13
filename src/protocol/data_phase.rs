//! The Data Phase of one Test Connection: the interface frozen by ADR 0008
//!, which a Data Phase Engine
//! drives. The Control Phase core creates a [`DataPhase`] on activation and
//! the runtime takes it with [`Endpoint::take_data_phase`](crate::protocol::Endpoint).
//!
//! Every `Now` passed in must read the Test Time Base (CLOCK_MONOTONIC_RAW on
//! Linux) as `mono`, and the Control Phase's `Now` must use the same clock:
//! lpduTime is built from it.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::time::Duration;

use crate::wire::AUTH_DIGEST_LEN;
use crate::wire::auth::DerivedKeys;
use crate::wire::load::{self, LOAD_HEADER_LEN, LOAD_PDU_ID, LoadHeader};
use crate::wire::status::{STATUS_NODEL, STATUS_PDU_LEN, StatusPdu, SubIntervalStats};
use crate::wire::{SendingRate, auth_mode};

use crate::protocol::algorithm::RateAdjustment;
use crate::protocol::checksum_use;
use crate::protocol::interval::{Classification, IntervalCounter, Thresholds, TrialIntervalCounts};
use crate::protocol::results::{SentInterval, SubIntervalMeasurement, expected_sub_intervals};
use crate::protocol::{IpVersion, MonoTime, Now, WallTime};

#[cfg(feature = "layout-diagnostics")]
#[doc(hidden)]
pub mod layout_diagnostics;

/// The most runs a [`SendPlan`] holds: Deferred Datagrams, then one new
/// Burst of up to 2 runs (a Burst and an Add-on); a Burst is withheld
/// rather than overflow it.
pub const MAX_RUNS: usize = 8;

/// The default catch-up budget (ADR 0012): a missed Burst less than this
/// late is still sent when the engine wakes. It covers the timer lateness
/// of a portable engine, a few hundred microseconds on Windows, without
/// letting a long stall flood the path: after a wake-up, each Transmitter
/// sends less than this much of its schedule at once.
pub const CATCH_UP_BUDGET: Duration = Duration::from_millis(1);

/// Bytes an IPv6 Load Sender takes off every Sending Rate Structure size:
/// the sizes are IPv4-based (ADR 0004).
const IPV6_EXTRA_HEADER: u16 = 20;

/// A Send Plan's datagram is at least a Load PDU header long; smaller sizes
/// are raised to it.
const MIN_DATAGRAM: u16 = LOAD_HEADER_LEN as u16;

/// Each Transmitter's first Burst goes out at a random offset in this range
/// after activation, so Load PDUs don't overtake the Activation Response and
/// Test Connections activated together don't send in lockstep.
const START_OFFSET: std::ops::RangeInclusive<Duration> =
    Duration::from_millis(5)..=Duration::from_millis(50);

/// The Server's Test Interval timer runs this much past testIntTime, as
/// OB-UDPST's does, so the Client's last Sub-Interval completes before STOP1
/// (Sub-Intervals open at the first Status PDU, just after activation).
/// (conformance: server-test-interval-timer-runs-half-a-second-over)
const STOP_AFTER_TEST_INTERVAL: Duration = Duration::from_millis(500);

/// How long past one Trial Interval the Server keeps sending STOP2 without
/// the Client's confirmation, which comes with the Client's next Status PDU
/// or Burst, before it ends the Test Connection itself.
/// (conformance: stop-confirmation-waits)
const STOP_CONFIRMATION_WAIT: Duration = Duration::from_secs(1);

/// The Load PDU and Status PDU timeouts (RFC 9946 §5.1 items 7-9: "no
/// more than 1 second"): after this long without receiving, an end stops
/// sending (ADR 0002). The Server keeps trialInt at most 250 ms, so
/// Status PDUs come often enough.
const RECEIVE_TIMEOUT: Duration = Duration::from_secs(1);

/// rxStopped is asserted from half the receive timeout, so the PDUs sent
/// before the timeout expires carry it (ADR 0002).
/// (conformance: rx-stopped-from-half-the-timeout)
const RX_STOPPED_AFTER: Duration = Duration::from_millis(500);

/// The Watchdog: after this long without receiving, the Test Connection
/// ends (ADR 0002, RFC 9946 §6.1's 1 s plus 2 s).
/// (conformance: receive-timeouts-stop-sending-at-1s)
const WATCHDOG: Duration = Duration::from_secs(3);

/// The Server stops a Test Connection whose Load Sender sends above this
/// many times its assigned rate for [`OVER_RATE_TRIALS`] Trial Intervals in
/// a row (ADR 0008).
const OVER_RATE_FACTOR: f64 = 1.5;
const OVER_RATE_TRIALS: u32 = 3;

/// How far back over-rate policing looks for the highest rate assigned:
/// longer than a round trip plus the time a bottleneck's queue takes to
/// drain at upperThresh.
/// ADR 0011 amends ADR 0008 so.
const POLICE_WINDOW: Duration = Duration::from_secs(1);

/// How long either end stopping a Test itself waits for its STOP2 PDU to go
/// out before it ends anyway.
const STOP_SEND_WAIT: Duration = Duration::from_secs(1);

/// How long past the Server's STOP1 the Client waits for STOP2 before it
/// ends the Test by its own Test Interval timer (RFC 9946 §9): room for the
/// Server's STOP2 to cross the path.
/// (conformance: stop-confirmation-waits)
const CLIENT_STOP_MARGIN: Duration = Duration::from_secs(1);

/// `count` datagrams of `len` bytes of UDP payload each.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Run {
    pub len: u16,
    pub count: u16,
}

/// A Burst for the engine to send at `scheduled`: runs of equal-sized
/// datagrams back to back, with contiguous sequence numbers from
/// `first_seq`. Heap-free and `Copy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendPlan {
    /// Numbers the Send Plans of a Test Connection in order.
    pub id: u64,
    /// When the Burst is due, on its Transmitter's grid.
    pub scheduled: MonoTime,
    /// The Transmitter's interval; lateness is judged against it.
    pub interval: Duration,
    pub first_seq: u32,
    /// Leading Deferred Datagrams carried over from a partial send.
    pub deferred: u16,
    runs: [Run; MAX_RUNS],
    run_count: u8,
}

impl SendPlan {
    fn new(id: u64, scheduled: MonoTime, interval: Duration, first_seq: u32) -> Self {
        Self {
            id,
            scheduled,
            interval,
            first_seq,
            deferred: 0,
            runs: [Run::default(); MAX_RUNS],
            run_count: 0,
        }
    }

    pub fn runs(&self) -> &[Run] {
        &self.runs[..usize::from(self.run_count)]
    }

    /// Appends a run, merging it into the last one when the sizes match.
    fn push(&mut self, run: Run) {
        if run.count == 0 {
            return;
        }
        let count = usize::from(self.run_count);
        if let Some(last) = self.runs[..count].last_mut()
            && last.len == run.len
        {
            last.count += run.count;
            return;
        }
        assert!(
            count < MAX_RUNS,
            "a Send Plan holds at most {MAX_RUNS} runs"
        );
        self.runs[count] = run;
        self.run_count += 1;
    }

    pub fn datagrams(&self) -> u32 {
        self.runs().iter().map(|r| u32::from(r.count)).sum()
    }

    /// The datagrams after the first `sent`, as a plan of their own.
    fn tail(&self, sent: u32) -> SendPlan {
        let mut tail = SendPlan::new(
            self.id,
            self.scheduled,
            self.interval,
            self.first_seq.wrapping_add(sent),
        );
        let mut skip = sent;
        for run in self.runs() {
            let skipped = skip.min(u32::from(run.count));
            skip -= skipped;
            tail.push(Run {
                len: run.len,
                count: run.count - skipped as u16,
            });
        }
        tail
    }

    /// The UDP payload bytes of every datagram together: the buffer size
    /// [`DataPhase::write_datagrams`] needs.
    pub fn bytes(&self) -> usize {
        self.runs()
            .iter()
            .map(|r| usize::from(r.len) * usize::from(r.count))
            .sum()
    }

    /// Each datagram's offset and length in the send buffer, in order.
    pub fn layout(&self) -> impl Iterator<Item = (usize, usize)> + '_ {
        let mut offset = 0;
        self.runs()
            .iter()
            .flat_map(|r| std::iter::repeat_n(usize::from(r.len), usize::from(r.count)))
            .map(move |len| {
                let at = offset;
                offset += len;
                (at, len)
            })
    }
}

/// A received datagram for the Load Receiver: its first bytes (a
/// header-only read), its real length, and what the kernel reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RxDatagram {
    /// The datagram's first bytes, the most a header-only read takes,
    /// zero-padded if it was shorter: the truncated length is
    /// `actual_len.min(LOAD_HEADER_LEN)`.
    pub header: [u8; LOAD_HEADER_LEN],
    /// The datagram's full length, which a header-only read still reports
    /// (MSG_TRUNC on Linux).
    pub actual_len: usize,
    /// The kernel's wall-clock receive timestamp, where the platform gives one.
    pub kernel_wall: Option<WallTime>,
    /// The received TOS / traffic class, where the platform gives it.
    pub tos: Option<u8>,
}

/// Datagrams from one read, with clock pairs taken just before and just
/// after it, so kernel timestamps can be converted into the Test Time Base.
#[derive(Debug, Clone, Copy)]
pub struct RxBatch<'a> {
    pub before: Now,
    pub after: Now,
    pub datagrams: &'a [RxDatagram],
}

/// A Status PDU, with its kernel receive time.
#[derive(Debug, Clone, Copy)]
pub struct RxPdu<'a> {
    /// A clock pair read just after the read that returned it, which
    /// converts `kernel_wall` into the Test Time Base.
    pub now: Now,
    /// The kernel's wall-clock receive timestamp, where the platform gives
    /// one: rttRespDelay counts from it.
    pub kernel_wall: Option<WallTime>,
    pub pdu: &'a [u8],
}

/// A transport error the runtime saw on the Test Connection's socket
/// (ADR 0006).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportError {
    /// An ICMP-type error (refused, unreachable): counted and ignored.
    Icmp,
    /// EMSGSIZE: a Path MTU event.
    MsgSize,
    /// ICMP time exceeded: MaxHops is too small for the path.
    TimeExceeded,
}

/// Something the Data Phase reports upward, per Test Connection. Counts
/// are coalesced until polled, so reporting never allocates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataEvent {
    /// Locally reported load bytes since the preceding accounting event,
    /// independent of completed measurement intervals or peer statistics.
    /// A runtime sender uses its successful socket sends for Quotas,
    /// separately from logically lost sends counted in measurements.
    LoadBytes { bytes: u64 },
    /// Skipped Bursts were not sent: each was at least the catch-up budget
    /// late when its turn came, and not the most recent Burst due (ADR
    /// 0012).
    BurstsSkipped {
        bursts: u32,
        datagrams: u32,
        /// UDP payload bytes not sent.
        bytes: u64,
    },
    /// Withheld Bursts were not sent: the host took too little of earlier
    /// Send Plans, so at least a Burst's worth of Deferred Datagrams still
    /// waited.
    BurstsWithheld {
        bursts: u32,
        datagrams: u32,
        /// UDP payload bytes not sent.
        bytes: u64,
    },
    /// The first valid Load PDU arrived.
    FirstLoadPduReceived,
    /// Datagrams that failed Load PDU validation were ignored (RFC 9946 §8.1).
    InvalidLoadPdus { count: u32 },
    /// Load PDUs whose lpduSeqNo jumped more than 1024 ahead (a first one:
    /// more than 1024 from 1), and which the next Load PDU didn't continue
    /// from, were ignored: they count as neither received nor lost (a
    /// guard against forged Load PDUs).
    IgnoredLoadPdus { count: u32 },
    /// Mode 2: Status PDUs that failed authentication (the authDigest, the
    /// keyId, authMode or the time window) and were ignored.
    StatusPdusFailedVerification { count: u32 },
    /// Load Sender: Status PDUs that never arrived, from gaps in spduSeqNo
    /// (OB-UDPST's WARN_LOC_STATUS).
    StatusPdusMissing { count: u32 },
    /// Load Receiver: Load PDUs whose spduSeqErr said the peer had missed
    /// Status PDUs (OB-UDPST's WARN_REM_STATUS).
    PeerMissedStatusPdus { count: u32 },
    /// Valid PDUs whose rxStopped said the peer had received nothing for
    /// half the receive timeout (RFC 9946 §8.1): traffic this way was
    /// interrupted.
    PeerRxStopped { count: u32 },
    /// Load PDUs read while the wall clock stepped, so their kernel
    /// timestamps couldn't be trusted: counted, but without a delay
    /// sample.
    DelaySamplesDropped { count: u32 },
    /// Datagrams too large for the path with DF set: a hop's MTU is below
    /// the Test's packet size (ICMP fragmentation needed or packet too big,
    /// or EMSGSIZE). They are lost (ADR 0006).
    PathMtuExceeded { count: u32 },
    /// ICMP time-exceeded errors: the Test's MaxHops is too small for the
    /// path, so its datagrams expire on the way (RFC 9946 §10).
    HopLimitExceeded { count: u32 },
    /// Other ICMP errors the path returned (unreachable, refused): counted
    /// and otherwise ignored (ADR 0006).
    IcmpErrors { count: u32 },
    /// The first valid Status PDU arrived.
    FirstStatusPduReceived,
    /// Datagrams that failed Status PDU validation were ignored (RFC 9946
    /// §8.2: the size, the checksum if used, the PDU ID).
    InvalidStatusPdus { count: u32 },
    /// A Sub-Interval's results: the Load Receiver's as it closes one, the
    /// Load Sender's from the Status PDUs' sisSav, once each.
    SubIntervalMeasured(SubIntervalMeasurement),
    /// Load Sender: what it sent in an st, as each closes while the Test
    /// runs, at most as many as the Test Interval holds.
    Sent(SentInterval),
    /// Load Receiver: an estimate of what the Load Sender sent in an st of
    /// its own clock, from the Load PDUs that arrived, as each closes
    ///. The Load PDUs of an st are those whose lpduTime falls in it,
    /// counted by their lpduSeqNo span, a lost one as big as those around
    /// it; the Load Sender's clock sets the widths, so the offset between
    /// the clocks doesn't matter.
    SentEstimated(SentInterval),
    /// A Client as Load Sender was given Sending Rate Structures above the
    /// maxBandwidth it stated, and sent them at that rate instead: their
    /// intervals lengthened, their sizes and Bursts kept.
    SendingRateCapped {
        /// How many structures were capped.
        structures: u32,
        /// The highest rate one of them asked for after packet-size
        /// normalization, including IP headers, in kbit/s.
        highest_kbps: u64,
        /// The stated maxBandwidth, in kbit/s.
        cap_kbps: u64,
    },
    /// Client: the Server's STOP2 arrived; the Test is complete and its
    /// results are final (RFC 9946 §9). Later PDUs are ignored.
    StopReceived,
    /// Server, Downstream Search Test: no Status PDU arrived for the Lost
    /// Status Backoff timeout (RFC 9097 §8.1), so the Server backed off as
    /// for an Errored Interval; `row` is the row it sends from now.
    LostStatusBackoff { row: u16 },
    /// Status PDUs whose spduSeqNo wasn't greater than the last one
    /// processed were only taken as a sign of life (docs/spec.md).
    StaleStatusPdus { count: u32 },
    /// Server: a Trial Interval was classified, from the Status PDU
    /// received (Downstream) or sent (Upstream), in Fixed-Rate Tests too.
    /// STOP Status PDUs are not classified. At most
    /// [`MAX_PENDING_CLASSIFICATIONS`] wait to be polled; older ones are
    /// dropped.
    TrialIntervalClassified(Classification),
    /// Client: how its Trial Intervals classified, the Status PDUs it sent
    /// (Downstream) or received (Upstream) while the Test ran, by the
    /// accepted thresholds and the maximum delay variation, whether
    /// Congestion was ever confirmed, and how many of the last ones were
    /// errored. Once, as the Data Phase ends, just before
    /// [`DataEvent::Ended`].
    TrialIntervalsCounted(TrialIntervalCounts),
    /// Nothing valid arrived for 1 s (the Load PDU or Status PDU timeout,
    /// RFC 9946 §5.1), so this end has stopped sending; the Watchdog ends
    /// the Test Connection 2 s later (ADR 0002). `silent_since` is when the
    /// last valid PDU arrived: when the silence began.
    TrafficStopped { silent_since: MonoTime },
    /// The Data Phase is over: the runtime closes the Test Connection.
    Ended(EndReason),
}

/// Why a Test Connection's Data Phase ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    /// The STOP exchange completed (RFC 9946 §9): the Server received the
    /// Client's confirmation, or the Client sent it.
    Completed,
    /// Server: no confirmation came within a Trial Interval plus 1 s of
    /// STOP1.
    Unconfirmed,
    /// Client: no STOP2 came, and its own Test Interval timer expired
    /// (RFC 9946 §9: the timers "take precedence").
    TestIntervalElapsed,
    /// Server: a STOP came before the Test Interval ended: the Client ended
    /// the Test Connection, or an on-path attacker set STOP (RFC 9946 §11).
    StoppedByClient,
    /// [`DataPhase::stop`] was called.
    Stopped,
    /// Nothing valid arrived for 3 s: the Watchdog ended the Test
    /// Connection without the STOP exchange (ADR 0002).
    Watchdog,
    /// Server: the Client sent above 1.5 times its assigned rate for 3
    /// Trial Intervals in a row, so the Server stopped the Test Connection
    /// (ADR 0008's
    /// `OverRateStopped`).
    OverRate,
}

impl EndReason {
    /// Whether a Test Connection whose Data Phase ended for this reason
    /// completed: the STOP exchange completed, or the Client's own Test
    /// Interval timer ended it first (RFC 9946 §9: the timers "take
    /// precedence"), which OB-UDPST counts as complete too. Every other end
    /// is early. The Multi-Connection Coordinator goes on by this rule, and
    /// the Client's exit status and results say it.
    pub fn completes(self) -> bool {
        matches!(self, Self::Completed | Self::TestIntervalElapsed)
    }
}

/// Which end of the Test Connection a Data Phase is. Its part in the STOP
/// exchange follows (RFC 9946 §9), and so do the Server's rate decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum End {
    /// Sends STOP2 when the Test Interval is over; decides the rate in both
    /// directions and polices an Upstream Load Sender's.
    Server,
    /// Confirms the Server's STOP2; as Load Sender, sends at the Sending
    /// Rate Structure the Server's Status PDUs carry.
    Client,
}

/// Where the Test Connection is in the STOP exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// TEST_ACT_TEST until `until`: the Server's STOP1, or the Client's own
    /// end of the Test.
    Testing {
        until: MonoTime,
    },
    /// Server: STOP1 is set; every PDU sent carries STOP2 until the Client
    /// confirms, or until `until`.
    Stopping {
        until: MonoTime,
    },
    /// The next PDU sent carries STOP2 and ends the Data Phase with
    /// `reason`; at once when either end stops the Test itself. By `until`
    /// it ends anyway.
    Confirming {
        now: bool,
        until: MonoTime,
        reason: EndReason,
    },
    Ended,
}

/// How a Test Connection's Data Phase is set up.
pub(crate) struct DataPhaseSetup {
    pub now: Now,
    /// The Test Connection's peer: the address its datagrams come from and
    /// go to.
    pub peer: SocketAddr,
    /// dscpEcn from the Activation Response, ECN Not-ECT.
    pub dscp_ecn: u8,
    /// Seeds the start offsets and the random payload; see [`seed`].
    pub seed: u64,
    /// Whether this end fills the Load PDU checksum and verifies a
    /// populated one (RFC 9946 §5.6).
    pub checksum: bool,
    /// The Test Connection's Derived Keys, in an authenticated mode.
    pub keys: Option<DerivedKeys>,
    /// Its keyId, which Status PDUs carry in mode 2.
    pub key_id: u8,
    /// The Test Connection's authMode, which Status PDUs carry.
    pub auth_mode: u8,
    /// testIntTime from the Activation Response.
    pub test_interval: Duration,
    /// trialInt from the Activation Response.
    pub trial_interval: Duration,
    /// subIntPeriod from the Activation Response: the nominal Sub-Interval
    /// its results are rated over.
    pub sub_interval: Duration,
    /// Whether this is the Test Connection's Client or its Server.
    pub this_end: End,
    /// Server: the accepted thresholds that classify each Trial Interval.
    pub thresholds: Option<Thresholds>,
    /// Server: the Load Rate Adjustment Algorithm, unless this is a
    /// Fixed-Rate Test.
    pub algorithm: Option<RateAdjustment>,
    /// Client: counts how its Trial Intervals classify.
    pub counter: Option<IntervalCounter>,
    /// The largest UDP payload, IPv4-based, the Test's Sending Rate Table
    /// variant allows (RFC 9946 §6.1): a Load Sender refuses a Sending Rate
    /// Structure field above it.
    pub max_payload: u32,
    /// A Load Receiver of a Multi-Connection Test: the origin of the grid
    /// its Status PDUs and Sub-Intervals keep to, the same for every Test
    /// Connection of the Test, so their Sub-Intervals can be summed.
    pub grid: Option<MonoTime>,
    /// A Client: the maxBandwidth it stated, in kbit/s, which as Load
    /// Sender it keeps to whatever the Server assigns.
    pub max_bandwidth_kbps: Option<u64>,
}

/// Mode 2: the current wall-clock second, the keyId, then the authDigest
/// over the whole PDU (RFC 9946 §5.3.2).
fn sign_status(pdu: &mut StatusPdu, now: Now, key_id: u8, key: &[u8; 32]) {
    pdu.auth_mode = auth_mode::MODE_2;
    pdu.key_id = key_id;
    pdu.auth_unix_time = crate::protocol::keys::auth_unix_time(now.wall);
    pdu.sign(key);
}

/// A Status PDU marked STOP2 (RFC 9946 §9), for a Client that ends a
/// Downstream Test before its Data Phase starts: the Server then ends the
/// Test Connection at once instead of by its watchdog. In mode 2,
/// `signer` gives the keyId and the Client's key.
pub(crate) fn stop_status_pdu(
    now: Now,
    auth_mode: u8,
    signer: Option<(u8, [u8; 32])>,
    checksum: bool,
) -> [u8; STATUS_PDU_LEN] {
    let wall = now.wall.as_unix();
    let spdu_time = (wall.as_secs() as u32, wall.subsec_nanos());
    // Any non-zero Trial Interval: the timer it advances is thrown away.
    let trial = Duration::from_secs(1);
    let mut pdu = LoadReceiver::new(trial, trial).status(now.mono, spdu_time);
    pdu.test_action = load::test_action::STOP2;
    pdu.auth_mode = auth_mode;
    if let Some((key_id, key)) = signer {
        sign_status(&mut pdu, now, key_id, &key);
    }
    let mut bytes = pdu.encode();
    if checksum {
        StatusPdu::fill_checksum(&mut bytes);
    }
    bytes
}

/// A Load PDU header marked STOP2 (RFC 9946 §9), for a Client that ends
/// an Upstream Test before its Data Phase starts.
pub(crate) fn stop_load_pdu(now: Now, checksum: bool) -> [u8; LOAD_HEADER_LEN] {
    let wall = now.wall.as_unix();
    let header = LoadHeader {
        test_action: load::test_action::STOP2,
        seq_no: 1,
        udp_payload: LOAD_HEADER_LEN as u16,
        lpdu_time: (wall.as_secs() as u32, wall.subsec_nanos()),
        ..LoadHeader::default()
    };
    let mut bytes = [0u8; LOAD_HEADER_LEN];
    header.encode_into(&mut bytes);
    if checksum {
        LoadHeader::fill_checksum(&mut bytes);
    }
    bytes
}

/// A seed for a Data Phase's start offsets and random payload: different
/// for every Test Connection and run, yet the core stays deterministic.
pub(crate) fn seed(now: Now, discriminator: u64) -> u64 {
    let wall = now.wall.as_unix().as_nanos() as u64;
    let mono = now.mono.as_duration().as_nanos() as u64;
    wall ^ mono.rotate_left(32) ^ discriminator.rotate_left(48)
}

/// The Data Phase of one Test Connection.
pub struct DataPhase {
    peer: SocketAddr,
    dscp_ecn: u8,
    /// subIntPeriod: the nominal Sub-Interval.
    sub_interval: Duration,
    /// How many Sub-Intervals the Test Interval holds: results beyond it
    /// aren't reported.
    expected_sub_intervals: u32,
    /// The Test Time Base plus this offset is lpduTime: it reads as wall
    /// time but no wall-clock step or slew moves it. It is fixed when the
    /// Test Connection activates (docs/spec.md, Time model).
    wall_offset_ns: i128,
    checksum: bool,
    auth_mode: u8,
    /// Load Sender or Load Receiver, with its state.
    role: Role,
    /// The Test Connection's Client or its Server.
    this_end: End,
    /// How long the Server waits for the confirmation after STOP1.
    stop_wait: Duration,
    phase: Phase,
    /// Server: Trial Intervals in a row above the over-rate limit, and
    /// whether that stopped the Test.
    over_rate_trials: u32,
    over_rate: bool,
    /// When a valid PDU last arrived (or the Data Phase started).
    last_received: MonoTime,
    /// Whether the timeout has expired: this end sends nothing more, and
    /// traffic that comes back doesn't change that (ADR 0002).
    silenced: bool,
    /// How late a missed Burst may be and still be sent (ADR 0012).
    catch_up_budget: Duration,
    pending: PendingEvents,
    /// Server: classifies each Trial Interval.
    thresholds: Option<Thresholds>,
    /// Server: moves the row by each classified Trial Interval.
    algorithm: Option<RateAdjustment>,
    /// Client: counts how its Trial Intervals classify.
    counter: Option<IntervalCounter>,
    /// Server, Upstream: the rates assigned within the last
    /// [`POLICE_WINDOW`], by when, for over-rate policing.
    assigned: VecDeque<(MonoTime, f64)>,
    /// Mode 2: how Status PDUs are signed and verified.
    status_auth: Option<StatusAuth>,
}

/// Mode 2's Status PDU authentication (RFC 9946 §5.3.2): each end signs
/// with its own Derived Key, the Client's or the Server's (§5.3.1), and
/// verifies with the other's.
struct StatusAuth {
    key_id: u8,
    own: [u8; 32],
    peer: [u8; 32],
}

// The Data Phase Engine moves each `DataPhase` onto an Engine Thread.
const _: () = {
    const fn assert_send<T: Send>() {}
    assert_send::<DataPhase>();
};

enum Role {
    LoadSender(Box<LoadSender>),
    LoadReceiver(Box<LoadReceiver>),
}

impl DataPhase {
    /// The Load Sender of a Test Connection sending at the row `rate`.
    /// `ip_version` is that of the Test Connection's packets.
    pub(crate) fn load_sender(
        setup: DataPhaseSetup,
        rate: &SendingRate,
        ip_version: IpVersion,
        random_payload: bool,
    ) -> Self {
        let mut rng = SplitMix64(setup.seed);
        let mut sender = LoadSender::new(
            rate,
            setup.max_payload,
            setup.max_bandwidth_kbps,
            ip_version,
            random_payload,
            setup.now,
            &mut rng,
        );
        sender.sent_grid = SentGrid::new(
            setup.trial_interval,
            setup.test_interval,
            setup.peer.ip().to_canonical().is_ipv4(),
        );
        let capped = keep_to(sender.assigned, setup.max_bandwidth_kbps, ip_version).1;
        let cap = setup.max_bandwidth_kbps.unwrap_or_default();
        let mut phase = Self::new(setup, Role::LoadSender(Box::new(sender)));
        if let Some(assigned) = capped {
            phase.pending.capped(assigned, cap);
        }
        phase
    }

    /// The Load Receiver of a Test Connection with this Sub-Interval period
    /// (subIntPeriod). Its Status PDUs carry `sending_rate`: the Server's
    /// row in an Upstream Test, zeros from a Client (RFC 9946 §8.2).
    pub(crate) fn load_receiver(
        setup: DataPhaseSetup,
        sub_interval: Duration,
        sending_rate: SendingRate,
    ) -> Self {
        let mut receiver = LoadReceiver::new(setup.trial_interval, sub_interval);
        receiver.sending_rate = sending_rate;
        receiver.grid = setup.grid;
        receiver.sent.most = expected_sub_intervals(setup.test_interval, receiver.sent.st);
        receiver.sent.ipv4 = setup.peer.ip().to_canonical().is_ipv4();
        Self::new(setup, Role::LoadReceiver(Box::new(receiver)))
    }

    fn new(setup: DataPhaseSetup, role: Role) -> Self {
        let wall = setup.now.wall.as_unix().as_nanos() as i128;
        let mono = setup.now.mono.as_duration().as_nanos() as i128;
        let mut until = setup.test_interval + STOP_AFTER_TEST_INTERVAL;
        if setup.this_end == End::Client {
            until += CLIENT_STOP_MARGIN;
        }
        Self {
            peer: setup.peer,
            dscp_ecn: setup.dscp_ecn,
            sub_interval: setup.sub_interval,
            expected_sub_intervals: expected_sub_intervals(setup.test_interval, setup.sub_interval),
            wall_offset_ns: wall - mono,
            checksum: setup.checksum,
            auth_mode: setup.auth_mode,
            role,
            this_end: setup.this_end,
            stop_wait: setup.trial_interval + STOP_CONFIRMATION_WAIT,
            over_rate_trials: 0,
            over_rate: false,
            last_received: setup.now.mono,
            silenced: false,
            catch_up_budget: CATCH_UP_BUDGET,
            phase: Phase::Testing {
                until: MonoTime::from_duration(setup.now.mono.as_duration() + until),
            },
            pending: PendingEvents::default(),
            thresholds: setup.thresholds,
            algorithm: setup.algorithm,
            counter: setup.counter,
            assigned: VecDeque::new(),
            status_auth: setup
                .keys
                .filter(|_| setup.auth_mode == auth_mode::MODE_2)
                .map(|keys| {
                    let (own, peer) = match setup.this_end {
                        End::Client => (*keys.client(), *keys.server()),
                        End::Server => (*keys.server(), *keys.client()),
                    };
                    StatusAuth {
                        key_id: setup.key_id,
                        own,
                        peer,
                    }
                }),
        }
    }

    /// Where this Test Connection's datagrams come from and go to. The
    /// runtime drops datagrams from any other address.
    pub fn peer(&self) -> SocketAddr {
        self.peer
    }

    /// Whether this end is the Load Receiver, which reads Load PDUs; the
    /// Load Sender reads Status PDUs.
    pub fn is_load_receiver(&self) -> bool {
        matches!(self.role, Role::LoadReceiver(_))
    }

    /// The IP header's DS field (IPv4 TOS, IPv6 Traffic Class) for every
    /// datagram this Test Connection sends from now on: the agreed DSCP,
    /// with ECN Not-ECT (RFC 9946 §7.1).
    pub fn dscp_ecn(&self) -> u8 {
        self.dscp_ecn
    }

    /// The next Burst's scheduled time: a precise deadline.
    pub fn next_burst_at(&self) -> Option<MonoTime> {
        match &self.role {
            // Even with no Transmitter, confirmation or a local STOP is due.
            Role::LoadSender(sender)
                if matches!(self.phase, Phase::Confirming { .. })
                    && !self.silenced
                    && sender.next_burst_at().is_none() =>
            {
                Some(MonoTime::from_duration(Duration::ZERO))
            }
            Role::LoadSender(sender) if !self.ended() && !self.silenced => sender.next_burst_at(),
            _ => None,
        }
    }

    /// The next coarse deadline (Status PDU timer, watchdogs, end of Test).
    pub fn poll_timeout(&self) -> Option<MonoTime> {
        let stop = match self.phase {
            Phase::Testing { until }
            | Phase::Stopping { until }
            | Phase::Confirming { until, .. } => Some(until),
            Phase::Ended => return None,
        };
        let status = match &self.role {
            Role::LoadReceiver(receiver) if !self.silenced => receiver.next_status,
            _ => None,
        };
        let after = |d: Duration| MonoTime::from_duration(self.last_received.as_duration() + d);
        let timeout = (!self.silenced).then(|| after(RECEIVE_TIMEOUT));
        let backoff = self.lost_status_backoff_at();
        [stop, status, timeout, Some(after(WATCHDOG)), backoff]
            .into_iter()
            .flatten()
            .min()
    }

    /// When the Server as Load Sender next backs off for lost Status PDUs:
    /// in a Downstream Search Test that is still testing and sending.
    /// Upstream Tests rely on the 1 s timeout (docs/spec.md).
    /// (conformance: no-lost-status-backoff-upstream)
    fn lost_status_backoff_at(&self) -> Option<MonoTime> {
        let algorithm = self.algorithm.as_ref()?;
        let sending = matches!(self.role, Role::LoadSender(_));
        (sending && !self.silenced && matches!(self.phase, Phase::Testing { .. })).then(|| {
            MonoTime::from_duration(
                self.last_received.as_duration() + algorithm.lost_status_timeout(),
            )
        })
    }

    /// A valid PDU arrived: the timeouts start over, unless one has already
    /// expired.
    fn heard(&mut self, now: MonoTime) {
        if !self.silenced {
            self.last_received = now;
        }
    }

    /// rxStopped for a PDU sent now (RFC 9946 §8.1).
    fn rx_stopped(&self, now: MonoTime) -> u8 {
        let quiet = now
            .as_duration()
            .saturating_sub(self.last_received.as_duration());
        u8::from(quiet >= RX_STOPPED_AFTER)
    }

    fn ended(&self) -> bool {
        self.phase == Phase::Ended
    }

    fn end(&mut self, reason: EndReason) {
        if !self.ended() {
            self.phase = Phase::Ended;
            self.pending.trial_interval_counts = self.counter.map(|c| c.counts());
            self.pending.ended = Some(if self.over_rate {
                EndReason::OverRate
            } else {
                reason
            });
        }
    }

    /// Server: polices the Client's rate over the Trial Interval a Status
    /// PDU just closed (ADR 0008), against the highest rate assigned within
    /// [`POLICE_WINDOW`]: after a lower rate is assigned, the Client
    /// switches a round trip later, and what queued at a bottleneck keeps
    /// arriving until the queue drains. Past the limit it starts the STOP
    /// exchange early, so the Client stops sending at once.
    /// (conformance: server-polices-upstream-rate)
    fn police(&mut self, now: Now, received_bytes: u32, trial: Duration, assigned: Option<f64>) {
        let Some(assigned) = assigned else {
            return;
        };
        let since = now.mono.as_duration().saturating_sub(POLICE_WINDOW);
        while self
            .assigned
            .front()
            .is_some_and(|(at, _)| at.as_duration() < since)
        {
            self.assigned.pop_front();
        }
        self.assigned.push_back((now.mono, assigned));
        let bytes_per_second = self
            .assigned
            .iter()
            .map(|&(_, rate)| rate)
            .fold(assigned, f64::max);
        let allowed = bytes_per_second * OVER_RATE_FACTOR * trial.as_secs_f64();
        if f64::from(received_bytes) > allowed {
            self.over_rate_trials += 1;
        } else {
            self.over_rate_trials = 0;
        }
        if self.over_rate_trials >= OVER_RATE_TRIALS && matches!(self.phase, Phase::Testing { .. })
        {
            self.over_rate = true;
            self.phase = Phase::Stopping {
                until: MonoTime::from_duration(now.mono.as_duration() + self.stop_wait),
            };
        }
    }

    /// testAction for a PDU sent now (RFC 9946 §9).
    fn outgoing_test_action(&self) -> u8 {
        match self.phase {
            Phase::Testing { .. } => load::test_action::TEST,
            _ => load::test_action::STOP2,
        }
    }

    /// A received PDU's testAction (RFC 9946 §9).
    /// - The Client acts on STOP2 only: TEST_ACT_STOP1 is the Server's local
    ///   state, so it and unknown values are ignored.
    /// - The Server takes any value but TEST_ACT_TEST as the Client's STOP,
    ///   as OB-UDPST does: an OB-UDPST Client echoes what it received, and
    ///   ends a Test on its own timer with STOP1.
    ///
    /// (conformance: stop-test-action-per-role, client-initiated-stop)
    fn stop_received(&mut self, test_action: u8) {
        let stop = match self.this_end {
            End::Server => test_action != load::test_action::TEST,
            End::Client => test_action == load::test_action::STOP2,
        };
        if !stop {
            return;
        }
        match (self.this_end, self.phase) {
            (End::Server, Phase::Stopping { .. }) => self.end(EndReason::Completed),
            (End::Server, Phase::Testing { .. }) => self.end(EndReason::StoppedByClient),
            // A silenced end can't confirm (RFC 9946 §5.1): it just ends.
            (End::Client, Phase::Testing { .. }) if self.silenced => {
                self.pending.stop_received = true;
                self.end(EndReason::Completed);
            }
            (End::Client, Phase::Testing { until }) => {
                self.phase = Phase::Confirming {
                    now: false,
                    until,
                    reason: EndReason::Completed,
                };
                self.pending.stop_received = true;
            }
            _ => {}
        }
    }

    /// A PDU carrying STOP2 was handed to the runtime: confirmation or a
    /// local stop is done, whether or not the kernel takes it.
    fn sent(&mut self) {
        if let Phase::Confirming { reason, .. } = self.phase {
            self.end(reason);
        }
    }

    /// The Test Time Base plus the wall offset: what lpduTime and spduTime
    /// carry, as seconds and nanoseconds.
    fn wire_time(&self, now: Now) -> (u32, u32) {
        let ns = now.mono.as_duration().as_nanos() as i128 + self.wall_offset_ns;
        ((ns / 1_000_000_000) as u32, (ns % 1_000_000_000) as u32)
    }

    /// Sets the catch-up budget, 1 ms unless the runtime
    /// says otherwise: the engine knows how late its timer wakes.
    pub fn set_catch_up_budget(&mut self, budget: Duration) {
        self.catch_up_budget = budget;
    }

    /// The next Send Plan due at or before `now + horizon`, oldest first.
    /// A missed Burst less than the catch-up budget late is still sent, one
    /// per call; an older one is a Skipped Burst, counted and reported, and
    /// uses no sequence numbers. The most recent missed Burst always goes
    /// (ADR 0008, ADR 0012). `horizon` must be zero while kernel pacing is
    /// off.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub fn poll_burst(&mut self, now: Now, horizon: Duration) -> Option<SendPlan> {
        debug_assert!(horizon.is_zero(), "kernel pacing is not adopted (ADR 0008)");
        if self.ended() || self.silenced {
            return None;
        }
        // While stopping, Bursts are cut to one datagram (RFC 9946 §9 MAY).
        let one = !matches!(self.phase, Phase::Testing { .. });
        let at_once = matches!(self.phase, Phase::Confirming { now: true, .. });
        let Role::LoadSender(sender) = &mut self.role else {
            return None;
        };
        // Either end stopping the Test itself sends its STOP2 at once,
        // even with no Transmitter to carry it.
        if at_once
            || (!matches!(self.phase, Phase::Testing { .. } | Phase::Stopping { .. })
                && sender.next_burst_at().is_none())
        {
            return Some(sender.stop_plan(now));
        }
        sender.poll_burst(now, horizon, one, self.catch_up_budget, &mut self.pending)
    }

    /// Writes every datagram of `plan` into `buf`, which holds at least
    /// `plan.bytes()`: the Load PDU header with lpduTime from `now` (read
    /// just before this call, one value for the whole Burst), then zeros or
    /// random payload content (CHTA_RAND_PAYLOAD).
    /// (conformance: lpdu-time-per-burst)
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub fn write_datagrams(&mut self, plan: &SendPlan, now: Now, buf: &mut [u8]) {
        let lpdu_time = self.wire_time(now);
        let test_action = self.outgoing_test_action();
        let rx_stopped = self.rx_stopped(now.mono);
        let Role::LoadSender(sender) = &mut self.role else {
            return;
        };
        // The latest spduTime, and the ms since it arrived (RFC 9946 §8.1),
        // rounded as OB-UDPST does.
        let spdu_seq_err = sender.spdu_seq_err;
        let (spdu_time, rtt_resp_delay) = sender.last_status.map_or(((0, 0), 0), |(time, at)| {
            let since = mono_ns(now.mono) - mono_ns(at);
            (time, u16::try_from(ms(since)).unwrap_or(u16::MAX))
        });
        for (index, (at, len)) in plan.layout().enumerate() {
            let datagram = &mut buf[at..at + len];
            sender.fill_payload(&mut datagram[LOAD_HEADER_LEN..]);
            let header = LoadHeader {
                test_action,
                rx_stopped,
                seq_no: plan.first_seq.wrapping_add(index as u32),
                udp_payload: len as u16,
                lpdu_time,
                spdu_time,
                rtt_resp_delay,
                spdu_seq_err,
                ..LoadHeader::default()
            };
            let header_bytes = datagram
                .first_chunk_mut::<LOAD_HEADER_LEN>()
                .expect("every Load PDU holds a header");
            header.encode_into(header_bytes);
            if self.checksum {
                LoadHeader::fill_checksum(datagram);
            }
        }
        self.sent();
    }

    /// How many of `plan`'s datagrams (a prefix) reached the kernel; each
    /// Send Plan is reported before the next is polled. The rest are
    /// Deferred Datagrams: they lead the next Send Plan with their sequence
    /// numbers and a fresh lpduTime, so host backpressure never shows as
    /// loss. When there are at least as many as the next Burst holds, that
    /// Burst is a Withheld Burst (`DataEvent::BurstsWithheld`), so they
    /// never grow past about two Bursts.
    /// (conformance: deferred-datagrams-and-withheld-bursts)
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub fn report_sent(&mut self, plan: &SendPlan, sent: u32, now: Now) {
        debug_assert!(sent <= plan.datagrams());
        let Role::LoadSender(sender) = &mut self.role else {
            return;
        };
        debug_assert_eq!(
            plan.id + 1,
            sender.next_plan_id,
            "each Send Plan is reported before the next is polled"
        );
        sender.deferred = (sent < plan.datagrams()).then(|| plan.tail(sent));
        let bytes = match &sender.deferred {
            Some(tail) => plan.bytes() - tail.bytes(),
            None => plan.bytes(),
        };
        self.pending.load_bytes = self.pending.load_bytes.saturating_add(bytes as u64);
        if matches!(self.phase, Phase::Testing { .. }) {
            sender
                .sent_grid
                .add(now.mono, sent, bytes as u64, &mut self.pending.sent);
        }
    }

    /// Load PDUs from one read. A kernel receive timestamp, which is wall
    /// time, becomes Test Time Base with the clock pair around the read
    /// (docs/spec.md Time model). A datagram may have waited in the socket
    /// buffer, so its time may be well before the read: it is only kept
    /// after the read, and no more than the receive timeout before it. If
    /// the pair's wall-to-mono offsets disagree (the wall clock stepped
    /// during the read), the batch's delay and RTT samples are dropped
    /// rather than trusted.
    /// Such Load PDUs are counted ([`DataEvent::DelaySamplesDropped`]).
    /// (conformance: wall-clock-step-drops-delay-samples, kernel-receive-timestamp-clamped)
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub fn handle_load_batch(&mut self, batch: RxBatch<'_>) {
        if !matches!(self.role, Role::LoadReceiver(_)) {
            return;
        }
        let offset = |now: Now| now.wall.as_unix().as_nanos() as i128 - mono_ns(now.mono);
        let consistent =
            (offset(batch.after) - offset(batch.before)).abs() <= CLOCK_PAIR_TOLERANCE_NS;
        let arrived = batch.after.mono;
        let earliest = mono_ns(batch.before.mono) - RECEIVE_TIMEOUT.as_nanos() as i128;
        for datagram in batch.datagrams {
            let received = match datagram.kernel_wall {
                Some(wall) if consistent => {
                    let mono = wall.as_unix().as_nanos() as i128 - offset(batch.after);
                    Some(mono.min(mono_ns(arrived)).max(earliest))
                }
                Some(_) => None,
                None => Some(mono_ns(arrived)),
            }
            .map(|mono| mono + self.wall_offset_ns);
            let valid = load_pdu_valid(datagram, self.checksum);
            if valid {
                // Quota volume includes STOP and every valid datagram
                // already read in this batch, unlike measurement results.
                self.pending.load_bytes = self
                    .pending
                    .load_bytes
                    .saturating_add(datagram.actual_len as u64);
                if !matches!(self.phase, Phase::Testing { .. } | Phase::Stopping { .. }) {
                    continue;
                }
                self.heard(arrived);
                // A STOP ends the counting, as in OB-UDPST: the Load PDU
                // carrying it is not counted, and later ones are ignored.
                // (conformance: stop-ends-counting)
                self.stop_received(datagram.header[2]);
            }
            if !matches!(self.phase, Phase::Testing { .. } | Phase::Stopping { .. }) {
                continue;
            }
            let Role::LoadReceiver(receiver) = &mut self.role else {
                return;
            };
            // Bytes are the datagram's actual length, which a header-only
            // read still reports, not its udpPayload field: a peer can't
            // inflate the results by stating a larger one.
            // (conformance: received-bytes-from-datagram-length)
            if let (true, Ok(mut header)) = (valid, LoadHeader::decode(&datagram.header)) {
                header.udp_payload = u16::try_from(datagram.actual_len).unwrap_or(u16::MAX);
                if received.is_none() {
                    self.pending.delay_samples_dropped += 1;
                }
                if header.rx_stopped == 1 {
                    self.pending.peer_rx_stopped += 1;
                }
                if header.spdu_seq_err != 0 {
                    self.pending.peer_missed_status_pdus += 1;
                }
                receiver.receive(arrived, received, &header, &mut self.pending);
            } else {
                self.pending.invalid_load_pdus += 1;
            }
        }
    }

    /// A Status PDU. In mode 2 it must carry authMode 2 and the Test
    /// Connection's keyId, and its authDigest must verify with the peer's
    /// key and its authUnixTime be within the time window (RFC 9946
    /// §5.3.2); one that fails is ignored and counted, and doesn't count as
    /// heard, so the watchdog ends the Test if they keep failing. Outside
    /// mode 2 the authentication fields aren't checked: OB-UDPST carries
    /// other data in them.
    /// (conformance: status-pdu-auth-fields-ignored-outside-mode-2)
    ///
    /// A stale Status PDU, whose spduSeqNo isn't greater than the last one
    /// processed, only counts as heard (docs/spec.md): it can't change the
    /// rate, deliver STOP or be echoed for RTT, and it is counted.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub fn handle_pdu(&mut self, pdu: RxPdu<'_>) {
        if !matches!(self.phase, Phase::Testing { .. } | Phase::Stopping { .. }) {
            return;
        }
        if !matches!(self.role, Role::LoadSender(_)) {
            return;
        }
        let Some(status) = valid_status_pdu(pdu.pdu, self.checksum) else {
            self.pending.invalid_status_pdus += 1;
            return;
        };
        if let Some(auth) = &self.status_auth
            && !status_authentic(&status, pdu.pdu, auth, pdu.now)
        {
            self.pending.status_pdus_failed_verification += 1;
            return;
        }
        self.heard(pdu.now.mono);
        // Any Status PDU, stale ones too, starts Lost Status Backoff over.
        if let Some(algorithm) = &mut self.algorithm {
            algorithm.status_heard();
        }
        let Role::LoadSender(sender) = &mut self.role else {
            return;
        };
        if !sender.status_received {
            sender.status_received = true;
            self.pending.first_status_pdu = true;
        }
        if status.rx_stopped == 1 {
            self.pending.peer_rx_stopped += 1;
        }
        // The RTT variation sample of each Status PDU that isn't stale, for
        // the Sub-Interval's mean: one a Trial Interval, where the Load
        // Receiver averages every sample.
        let fresh = status
            .spdu_seq_no
            .checked_sub(1)
            .is_some_and(|last| last >= sender.last_spdu_seq_no);
        if fresh && status.rtt_var_sample != STATUS_NODEL {
            sender.rtt_var_sum = sender
                .rtt_var_sum
                .saturating_add(u64::from(status.rtt_var_sample));
            sender.rtt_var_count = sender.rtt_var_count.saturating_add(1);
        }
        // The Load Receiver's latest Sub-Interval, once each, stale Status
        // PDUs included: its results (docs/spec.md).
        if status.sub_int_seq_no > sender.last_sub_int_seq_no
            && status.test_action == load::test_action::TEST
            && status.sub_int_seq_no <= self.expected_sub_intervals
        {
            sender.last_sub_int_seq_no = status.sub_int_seq_no;
            let rtt = (
                std::mem::take(&mut sender.rtt_var_sum),
                std::mem::take(&mut sender.rtt_var_count),
            );
            let measured = SubIntervalMeasurement {
                rtt_variation_sum: rtt.0,
                rtt_variation_count: rtt.1,
                rtt_variation_mean: mean(rtt),
                rtt_minimum: (status.rtt_minimum != STATUS_NODEL).then_some(status.rtt_minimum),
                // clockDeltaMin is signed and always has a value (docs/spec.md
                // Time model).
                clock_delta_min: Some(status.clock_delta_min as i32),
                measured_at: pdu.now.mono,
                ..SubIntervalMeasurement::new(
                    status.sub_int_seq_no,
                    &status.sis_sav,
                    u64::from(status.sis_sav.delay_var_sum),
                    self.sub_interval,
                    self.peer.ip().to_canonical().is_ipv4(),
                )
            };
            self.pending.sub_intervals.push_back(measured);
        }
        let Some(last) = status.spdu_seq_no.checked_sub(1) else {
            self.pending.stale_status_pdus += 1;
            return;
        };
        if last < sender.last_spdu_seq_no {
            self.pending.stale_status_pdus += 1;
            return;
        }
        // The Status PDUs missing before this one, as OB-UDPST counts
        // spduSeqErr, below its 0xFFFF for a reordered one.
        // (conformance: spdu-seq-err-is-the-gap-before-the-latest)
        let missing = last - sender.last_spdu_seq_no;
        self.pending.status_pdus_missing = self.pending.status_pdus_missing.saturating_add(missing);
        sender.spdu_seq_err = u16::try_from(missing)
            .unwrap_or(u16::MAX - 1)
            .min(u16::MAX - 1);
        sender.last_spdu_seq_no = status.spdu_seq_no;
        // rttRespDelay counts from the kernel's receive time, so the
        // time the Status PDU waited for the engine isn't taken for RTT.
        sender.last_status = Some((status.spdu_time, status_received_at(&pdu)));
        let testing = matches!(self.phase, Phase::Testing { .. });
        // A Client executes the Server's Sending Rate Structure (RFC 9946
        // §8.1); a Server sends from its own row.
        // Not from a STOP Status PDU, as OB-UDPST: its srStruct needn't be
        // one to send at.
        if self.this_end == End::Client
            && status.test_action == load::test_action::TEST
            && let Some(assigned) = sender.set_rate(&status.sending_rate, pdu.now)
        {
            let cap = sender.cap_kbps.unwrap_or_default();
            self.pending.capped(assigned, cap);
        }
        // A Client counts the Trial Interval the Server reports, as
        // the Server classifies it.
        if testing
            && status.test_action == load::test_action::TEST
            && let Some(counter) = &mut self.counter
        {
            counter.count(&status);
        }
        // Once the Server has sent STOP1 the rate no longer matters.
        // (conformance: stop-status-pdus-are-not-classified)
        if testing
            && status.test_action == load::test_action::TEST
            && let Some(rate) = classify_trial_interval(
                self.thresholds,
                &mut self.algorithm,
                &mut self.pending,
                &status,
            )
        {
            // At once, not at the next st boundary
            // (conformance: rate-changes-when-a-status-pdu-arrives).
            sender.set_rate(&rate, pdu.now);
        }
        self.stop_received(pdu.pdu[2]);
    }

    /// The timers: the Load PDU or Status PDU timeout and the Watchdog
    /// (ADR 0002), then the Test Interval timers (RFC 9946 §9): the
    /// Server's sets STOP1 and then waits for the confirmation; the
    /// Client's ends a Test whose STOP2 never came.
    pub fn handle_timeout(&mut self, now: Now) {
        let quiet = now
            .mono
            .as_duration()
            .saturating_sub(self.last_received.as_duration());
        if quiet >= RECEIVE_TIMEOUT && !self.silenced && !self.ended() {
            self.silenced = true;
            self.pending.traffic_stopped = Some(self.last_received);
        }
        if quiet >= WATCHDOG {
            return self.end(EndReason::Watchdog);
        }
        if self
            .lost_status_backoff_at()
            .is_some_and(|at| now.mono >= at)
            && let (Some(algorithm), Role::LoadSender(sender)) =
                (&mut self.algorithm, &mut self.role)
        {
            let (row, changed) = algorithm.back_off();
            if let Some(rate) = changed {
                sender.set_rate(&rate, now);
            }
            self.pending
                .rate_event(DataEvent::LostStatusBackoff { row });
        }
        match (self.phase, self.this_end) {
            (Phase::Testing { until }, End::Server) if now.mono >= until => {
                self.phase = Phase::Stopping {
                    until: MonoTime::from_duration(until.as_duration() + self.stop_wait),
                };
            }
            (Phase::Testing { until }, End::Client) if now.mono >= until => {
                self.end(EndReason::TestIntervalElapsed);
            }
            (Phase::Stopping { until }, _) if now.mono >= until => {
                self.end(EndReason::Unconfirmed);
            }
            (Phase::Confirming { until, reason, .. }, _) if now.mono >= until => {
                self.end(reason);
            }
            _ => {}
        }
    }

    /// The next Status PDU to send, written into `buf` (at least 204
    /// bytes); its length. The Load Receiver sends one every Trial
    /// Interval, the first one Trial Interval after the first Load PDU.
    pub fn poll_transmit(&mut self, now: Now, buf: &mut [u8]) -> Option<usize> {
        let spdu_time = self.wire_time(now);
        let test_action = self.outgoing_test_action();
        let rx_stopped = self.rx_stopped(now.mono);
        let at_once = matches!(self.phase, Phase::Confirming { now: true, .. });
        let silenced = self.silenced;
        let Role::LoadReceiver(receiver) = &mut self.role else {
            return None;
        };
        // STOP2 may be the first Load PDU, before any Status PDU timer
        // exists. Confirm it at once, as a Load Sender without a
        // Transmitter does (conformance: stop-ends-counting).
        let confirming_without_timer =
            matches!(self.phase, Phase::Confirming { .. }) && receiver.next_status.is_none();
        let due = receiver.next_status.is_some_and(|at| now.mono >= at) || confirming_without_timer;
        if self.phase == Phase::Ended || silenced || !(due || at_once) {
            return None;
        }
        // Before `status`, which ends the Trial Interval.
        let out = buf.first_chunk_mut::<STATUS_PDU_LEN>()?;
        let closed_before = receiver.sub_int_seq_no;
        let mut pdu = receiver.status(now.mono, spdu_time);
        // A Sub-Interval closed while testing: its results. One closed
        // by a STOP, or beyond the Test Interval, is no measurement.
        if receiver.sub_int_seq_no != closed_before
            && test_action == load::test_action::TEST
            && let Some(seq_no) = receiver.measured_seq_no()
            && seq_no <= self.expected_sub_intervals
        {
            let measured = SubIntervalMeasurement {
                rtt_variation_sum: receiver.saved_rtt.0,
                rtt_variation_count: receiver.saved_rtt.1,
                rtt_variation_mean: mean(receiver.saved_rtt),
                rtt_minimum: receiver.rtt_minimum,
                clock_delta_min: receiver.clock_delta_min,
                measured_at: now.mono,
                ..SubIntervalMeasurement::new(
                    seq_no,
                    &receiver.saved,
                    receiver.saved_delay_sum,
                    receiver.sub_interval,
                    self.peer.ip().to_canonical().is_ipv4(),
                )
            };
            self.pending.sub_intervals.push_back(measured);
        }
        let assigned = udp_bytes_per_second(&receiver.sending_rate);
        pdu.test_action = test_action;
        pdu.rx_stopped = rx_stopped;
        pdu.auth_mode = self.auth_mode;
        // A Client counts the Trial Interval it reports, as the
        // Server classifies it.
        if pdu.test_action == load::test_action::TEST
            && let Some(counter) = &mut self.counter
        {
            counter.count(&pdu);
        }
        // Before encoding, so the rate the algorithm chooses goes out in
        // this Status PDU; after reading the rate in force, which
        // policing judges this Trial Interval by.
        if pdu.test_action == load::test_action::TEST
            && let Some(rate) = classify_trial_interval(
                self.thresholds,
                &mut self.algorithm,
                &mut self.pending,
                &pdu,
            )
        {
            receiver.sending_rate = rate;
            pdu.sending_rate = rate;
        }
        if let Some(auth) = &self.status_auth {
            sign_status(&mut pdu, now, auth.key_id, &auth.own);
        }
        *out = pdu.encode();
        if self.checksum {
            StatusPdu::fill_checksum(out);
        }
        self.sent();
        if self.this_end == End::Server {
            let trial = Duration::from_micros(pdu.ti_delta_time.into());
            self.police(now, pdu.ti_rx_bytes, trial, assigned);
        }
        Some(STATUS_PDU_LEN)
    }

    /// Counts a transport error the runtime saw on the Test Connection's
    /// socket, to be reported as a Data Event (ADR 0006).
    pub fn handle_transport_error(&mut self, _now: Now, error: TransportError) {
        self.pending.transport_error(error);
    }

    /// Ends the Test Connection's Data Phase (Test aborted, shutdown).
    /// Either end first sends STOP2 (a Status PDU, or a final Send Plan
    /// as the Load Sender), so its peer ends the Test Connection
    /// instead of waiting for its Watchdog: the engine calls
    /// [`DataPhase::poll_burst`] and [`DataPhase::poll_transmit`] after
    /// `stop`, and the Data Phase ends by itself within 1 s if it doesn't.
    /// An end already silenced by its receive timeout sends nothing.
    /// (conformance: client-initiated-stop, server-initiated-stop)
    pub fn stop(&mut self, now: Now) {
        match self.phase {
            Phase::Testing { .. } | Phase::Stopping { .. } | Phase::Confirming { .. }
                if !self.silenced =>
            {
                self.phase = Phase::Confirming {
                    now: true,
                    until: MonoTime::from_duration(now.mono.as_duration() + STOP_SEND_WAIT),
                    reason: EndReason::Stopped,
                };
            }
            _ => self.end(EndReason::Stopped),
        }
    }

    pub fn poll_event(&mut self) -> Option<DataEvent> {
        self.pending.take()
    }

    /// Takes actual load bytes not yet returned by `poll_event`, including
    /// an unfinished interval when the engine fails or is dropped.
    pub fn take_load_bytes(&mut self) -> u64 {
        std::mem::take(&mut self.pending.load_bytes)
    }
}

/// Server: classifies the Trial Interval `status` reports, as a Clean,
/// Errored or Hold Interval, and moves the row by it unless the Test holds
/// its row (a Fixed-Rate Test), reporting both; the Sending Rate
/// Structure to switch to, if the row changed. The Server's Status PDUs
/// report the Trial Intervals of an Upstream Test, the Client's those of a
/// Downstream Test; a Client only counts them ([`IntervalCounter`]).
fn classify_trial_interval(
    thresholds: Option<Thresholds>,
    algorithm: &mut Option<RateAdjustment>,
    pending: &mut PendingEvents,
    status: &StatusPdu,
) -> Option<SendingRate> {
    let mut classification = thresholds?.classify(status);
    let mut changed = None;
    if let Some(algorithm) = algorithm {
        let (row, rate) = algorithm.on_interval(classification.class);
        classification.row = Some(row);
        changed = rate;
    }
    pending.classified(classification);
    changed
}

/// Load PDU validation (RFC 9946 §8.1): at least a header long, the
/// checksum where checksum use says (§5.6), the PDU ID 0xBEEF, and
/// testAction and rxStopped.
fn load_pdu_valid(datagram: &RxDatagram, checksum: bool) -> bool {
    if datagram.actual_len < LOAD_HEADER_LEN {
        return false;
    }
    let field = LoadHeader::checksum_field(&datagram.header);
    if !checksum_use::passes(checksum, field, || {
        LoadHeader::checksum_valid(&datagram.header)
    }) {
        return false;
    }
    u16::from_be_bytes([datagram.header[0], datagram.header[1]]) == LOAD_PDU_ID
        && flags_valid(datagram.header[2], datagram.header[3])
}

/// testAction is one RFC 9946 §8.1 defines and rxStopped is Boolean (0 or
/// 1), as OB-UDPST checks.
/// (conformance: undefined-flags-are-invalid)
fn flags_valid(test_action: u8, rx_stopped: u8) -> bool {
    test_action <= load::test_action::STOP2 && rx_stopped <= 1
}

/// Status PDU validation (RFC 9946 §8.2, §5.6): the size and PDU ID, the
/// checksum where checksum use says, and testAction and rxStopped; the PDU,
/// decoded, if it is valid.
fn valid_status_pdu(pdu: &[u8], checksum: bool) -> Option<StatusPdu> {
    let status = StatusPdu::decode(pdu).ok()?;
    let checked =
        checksum_use::passes(checksum, status.checksum, || StatusPdu::checksum_valid(pdu));
    (checked && flags_valid(status.test_action, status.rx_stopped)).then_some(status)
}

/// Mode 2's checks of a Status PDU that is otherwise valid (RFC 9946
/// §5.3.2): authMode, keyId, the digest over `pdu` as received, then
/// authUnixTime.
fn status_authentic(status: &StatusPdu, pdu: &[u8], auth: &StatusAuth, now: Now) -> bool {
    status.auth_mode == auth_mode::MODE_2
        && status.key_id == auth.key_id
        && StatusPdu::verify_received(pdu, &auth.peer)
        && crate::protocol::keys::auth_time_acceptable(status.auth_unix_time, now.wall)
}

/// Counts waiting to be reported as [`DataEvent`]s.
#[derive(Default)]
struct PendingEvents {
    load_bytes: u64,
    first_load_pdu: bool,
    skipped_bursts: u32,
    skipped_datagrams: u32,
    skipped_bytes: u64,
    withheld_bursts: u32,
    withheld_datagrams: u32,
    withheld_bytes: u64,
    invalid_load_pdus: u32,
    ignored_load_pdus: u32,
    delay_samples_dropped: u32,
    path_mtu_exceeded: u32,
    hop_limit_exceeded: u32,
    icmp_errors: u32,
    peer_rx_stopped: u32,
    status_pdus_missing: u32,
    peer_missed_status_pdus: u32,
    status_pdus_failed_verification: u32,
    first_status_pdu: bool,
    invalid_status_pdus: u32,
    stale_status_pdus: u32,
    /// Sending Rate Structures capped at the maxBandwidth: how many, the
    /// highest rate asked for and the cap, in kbit/s.
    capped_structures: u32,
    capped_highest_kbps: u64,
    cap_kbps: u64,
    /// Classifications and Lost Status Backoffs, in the order they
    /// happened.
    rate_events: VecDeque<DataEvent>,
    /// Sub-Interval results: never dropped, at most the expected count.
    sub_intervals: VecDeque<SubIntervalMeasurement>,
    /// st results, likewise.
    sent: VecDeque<SentInterval>,
    /// Estimates of the Load Sender's st, likewise.
    sent_estimates: VecDeque<SentInterval>,
    stop_received: bool,
    traffic_stopped: Option<MonoTime>,
    /// A Client's Trial Intervals, counted as its Data Phase ended.
    trial_interval_counts: Option<TrialIntervalCounts>,
    ended: Option<EndReason>,
}

/// How many classifications and Lost Status Backoffs wait to be polled
/// before the oldest is dropped: a second of the shortest Trial Interval
/// (5 ms) with room.
pub const MAX_PENDING_CLASSIFICATIONS: usize = 256;

impl PendingEvents {
    fn classified(&mut self, classification: Classification) {
        self.rate_event(DataEvent::TrialIntervalClassified(classification));
    }

    fn transport_error(&mut self, error: TransportError) {
        let count = match error {
            TransportError::MsgSize => &mut self.path_mtu_exceeded,
            TransportError::TimeExceeded => &mut self.hop_limit_exceeded,
            TransportError::Icmp => &mut self.icmp_errors,
        };
        *count = count.saturating_add(1);
    }

    fn capped(&mut self, assigned_kbps: u64, cap_kbps: u64) {
        self.capped_structures = self.capped_structures.saturating_add(1);
        self.capped_highest_kbps = self.capped_highest_kbps.max(assigned_kbps);
        self.cap_kbps = cap_kbps;
    }

    fn rate_event(&mut self, event: DataEvent) {
        if self.rate_events.len() == MAX_PENDING_CLASSIFICATIONS {
            self.rate_events.pop_front();
        }
        self.rate_events.push_back(event);
    }

    fn take(&mut self) -> Option<DataEvent> {
        if self.load_bytes > 0 {
            return Some(DataEvent::LoadBytes {
                bytes: std::mem::take(&mut self.load_bytes),
            });
        }
        if std::mem::take(&mut self.first_load_pdu) {
            return Some(DataEvent::FirstLoadPduReceived);
        }
        if let Some(measured) = self.sub_intervals.pop_front() {
            return Some(DataEvent::SubIntervalMeasured(measured));
        }
        if let Some(sent) = self.sent.pop_front() {
            return Some(DataEvent::Sent(sent));
        }
        if let Some(sent) = self.sent_estimates.pop_front() {
            return Some(DataEvent::SentEstimated(sent));
        }
        if self.skipped_bursts > 0 {
            return Some(DataEvent::BurstsSkipped {
                bursts: std::mem::take(&mut self.skipped_bursts),
                datagrams: std::mem::take(&mut self.skipped_datagrams),
                bytes: std::mem::take(&mut self.skipped_bytes),
            });
        }
        if self.capped_structures > 0 {
            return Some(DataEvent::SendingRateCapped {
                structures: std::mem::take(&mut self.capped_structures),
                highest_kbps: std::mem::take(&mut self.capped_highest_kbps),
                cap_kbps: self.cap_kbps,
            });
        }
        if self.withheld_bursts > 0 {
            return Some(DataEvent::BurstsWithheld {
                bursts: std::mem::take(&mut self.withheld_bursts),
                datagrams: std::mem::take(&mut self.withheld_datagrams),
                bytes: std::mem::take(&mut self.withheld_bytes),
            });
        }
        if self.invalid_load_pdus > 0 {
            return Some(DataEvent::InvalidLoadPdus {
                count: std::mem::take(&mut self.invalid_load_pdus),
            });
        }
        if self.ignored_load_pdus > 0 {
            return Some(DataEvent::IgnoredLoadPdus {
                count: std::mem::take(&mut self.ignored_load_pdus),
            });
        }
        if self.status_pdus_failed_verification > 0 {
            return Some(DataEvent::StatusPdusFailedVerification {
                count: std::mem::take(&mut self.status_pdus_failed_verification),
            });
        }
        if self.status_pdus_missing > 0 {
            return Some(DataEvent::StatusPdusMissing {
                count: std::mem::take(&mut self.status_pdus_missing),
            });
        }
        if self.peer_missed_status_pdus > 0 {
            return Some(DataEvent::PeerMissedStatusPdus {
                count: std::mem::take(&mut self.peer_missed_status_pdus),
            });
        }
        if self.peer_rx_stopped > 0 {
            return Some(DataEvent::PeerRxStopped {
                count: std::mem::take(&mut self.peer_rx_stopped),
            });
        }
        if self.delay_samples_dropped > 0 {
            return Some(DataEvent::DelaySamplesDropped {
                count: std::mem::take(&mut self.delay_samples_dropped),
            });
        }
        if self.path_mtu_exceeded > 0 {
            return Some(DataEvent::PathMtuExceeded {
                count: std::mem::take(&mut self.path_mtu_exceeded),
            });
        }
        if self.hop_limit_exceeded > 0 {
            return Some(DataEvent::HopLimitExceeded {
                count: std::mem::take(&mut self.hop_limit_exceeded),
            });
        }
        if self.icmp_errors > 0 {
            return Some(DataEvent::IcmpErrors {
                count: std::mem::take(&mut self.icmp_errors),
            });
        }
        if std::mem::take(&mut self.first_status_pdu) {
            return Some(DataEvent::FirstStatusPduReceived);
        }
        if self.invalid_status_pdus > 0 {
            return Some(DataEvent::InvalidStatusPdus {
                count: std::mem::take(&mut self.invalid_status_pdus),
            });
        }
        if self.stale_status_pdus > 0 {
            return Some(DataEvent::StaleStatusPdus {
                count: std::mem::take(&mut self.stale_status_pdus),
            });
        }
        if let Some(event) = self.rate_events.pop_front() {
            return Some(event);
        }
        if std::mem::take(&mut self.stop_received) {
            return Some(DataEvent::StopReceived);
        }
        if let Some(silent_since) = self.traffic_stopped.take() {
            return Some(DataEvent::TrafficStopped { silent_since });
        }
        if let Some(counts) = self.trial_interval_counts.take() {
            return Some(DataEvent::TrialIntervalsCounted(counts));
        }
        self.ended.take().map(DataEvent::Ended)
    }
}

/// OB-UDPST's random-size flag in a size field of the Sending Rate
/// Structure: the low 31 bits are a maximum, and each size is drawn from
/// [`MIN_RANDOM_SIZE`] up to it. RFC 9946 doesn't define it; OB-UDPST's row 0 sets it on the
/// add-on of every Upstream Search Test.
/// (conformance: random-size-flag)
const RANDOM_SIZE: u32 = 0x8000_0000;

/// The smallest random size, IPv4-based like every size in the Sending Rate
/// Structure: a Load PDU header once an IPv6 sender takes 20 off.
const MIN_RANDOM_SIZE: u16 = MIN_DATAGRAM + IPV6_EXTRA_HEADER;

/// OB-UDPST's sanity limits on a Sending Rate Structure field: a field
/// beyond them is zeroed.
const MAX_TX_INTERVAL_US: u32 = 1_000_000;
const MAX_SIZE: u32 = 8972;
const MAX_BURST: u32 = 100;

/// A Sending Rate Structure with every field beyond OB-UDPST's sanity
/// limits zeroed, as an OB-UDPST Client does with the Server's, so no
/// Server can make the Load Sender send absurd sizes or Bursts; a size
/// above `max_payload`, the largest the Test's table variant allows (RFC
/// 9946 §6.1), is zeroed too.
/// (conformance: sending-rate-structure-sanity-limits)
fn sanitize(rate: &SendingRate, max_payload: u32) -> SendingRate {
    let largest = MAX_SIZE.min(max_payload);
    let interval = |v: u32| if v > MAX_TX_INTERVAL_US { 0 } else { v };
    let size = |v: u32| if v & !RANDOM_SIZE > largest { 0 } else { v };
    let burst = |v: u32| if v > MAX_BURST { 0 } else { v };
    SendingRate {
        tx_interval1: interval(rate.tx_interval1),
        udp_payload1: size(rate.udp_payload1),
        burst_size1: burst(rate.burst_size1),
        tx_interval2: interval(rate.tx_interval2),
        udp_payload2: size(rate.udp_payload2),
        burst_size2: burst(rate.burst_size2),
        udp_addon2: size(rate.udp_addon2),
    }
}

/// The IP-layer rate actually sent after size normalization (ADR 0004),
/// random sizes at their maximum, rounded up to whole kbit/s.
fn ip_kbps(rate: &SendingRate, ip_version: IpVersion) -> u64 {
    let overhead = match ip_version {
        IpVersion::V4 => 28,
        IpVersion::V6 => 48,
    };
    let packets = |payload: u32, count: u32| match payload & !RANDOM_SIZE {
        0 => 0,
        _ => (u64::from(Size::new(payload, ip_version).max) + overhead) * u64::from(count),
    };
    let per = |interval: u32, bytes: u64| match interval {
        0 => 0,
        interval => (bytes * 8000).div_ceil(u64::from(interval)),
    };
    per(
        rate.tx_interval1,
        packets(rate.udp_payload1, rate.burst_size1),
    ) + per(
        rate.tx_interval2,
        packets(rate.udp_payload2, rate.burst_size2) + packets(rate.udp_addon2, 1),
    )
}

/// `rate` kept to a Client's stated maxBandwidth, `cap_kbps`, and the rate
/// it asked for if that was above it. Its intervals are lengthened
/// by the ratio, rounded up, so its sizes and Bursts stay as the Server
/// chose them (conformance: client-keeps-to-its-max-bandwidth).
fn keep_to(
    rate: SendingRate,
    cap_kbps: Option<u64>,
    ip_version: IpVersion,
) -> (SendingRate, Option<u64>) {
    let assigned = ip_kbps(&rate, ip_version);
    let Some(cap) = cap_kbps.filter(|&cap| cap > 0 && assigned > cap) else {
        return (rate, None);
    };
    let longer = |interval: u32| {
        u32::try_from((u64::from(interval) * assigned).div_ceil(cap)).unwrap_or(u32::MAX)
    };
    let kept = SendingRate {
        tx_interval1: longer(rate.tx_interval1),
        tx_interval2: longer(rate.tx_interval2),
        ..rate
    };
    (kept, Some(assigned))
}

/// The UDP payload bytes per second `rate` sends, random sizes at their
/// maximum; `None` if it sends nothing.
fn udp_bytes_per_second(rate: &SendingRate) -> Option<f64> {
    let per_second = |bytes: u32, count: u32, interval_us: u32| {
        if interval_us == 0 {
            0.0
        } else {
            f64::from(bytes & !RANDOM_SIZE) * f64::from(count) * 1e6 / f64::from(interval_us)
        }
    };
    let total = per_second(rate.udp_payload1, rate.burst_size1, rate.tx_interval1)
        + per_second(rate.udp_payload2, rate.burst_size2, rate.tx_interval2)
        + per_second(rate.udp_addon2, 1, rate.tx_interval2);
    (total > 0.0).then_some(total)
}

/// A datagram size of a Transmitter: fixed, or random up to `max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Size {
    max: u16,
    random: bool,
}

impl Size {
    /// A Sending Rate Structure size field for packets of `ip_version`:
    /// IPv4-based, so an IPv6 sender takes 20 bytes off (ADR 0004).
    fn new(field: u32, ip_version: IpVersion) -> Self {
        let to_bytes = |bytes: u32| -> u16 {
            let bytes = u16::try_from(bytes).unwrap_or(u16::MAX);
            let bytes = match ip_version {
                IpVersion::V6 if bytes >= MIN_DATAGRAM + IPV6_EXTRA_HEADER => {
                    bytes - IPV6_EXTRA_HEADER
                }
                _ => bytes,
            };
            bytes.max(MIN_DATAGRAM)
        };
        Self {
            max: to_bytes(field & !RANDOM_SIZE),
            random: field & RANDOM_SIZE != 0,
        }
    }

    /// The size of the next datagrams: `max`, or drawn from the random
    /// range, whose floor is also IPv4-based.
    fn draw(self, ip_version: IpVersion, rng: &mut SplitMix64) -> u16 {
        if !self.random {
            return self.max;
        }
        let min = Size::new(MIN_RANDOM_SIZE.into(), ip_version)
            .max
            .min(self.max);
        min + (rng.next() % u64::from(self.max - min + 1)) as u16
    }
}

/// One of the two Transmitters of a Sending Rate Structure.
#[derive(Debug, Clone, Copy)]
struct Transmitter {
    interval: Duration,
    next: MonoTime,
    /// UDP payload of each Burst datagram.
    payload: Size,
    burst: u16,
    /// UDP payload of the Add-on (transmitter 2 only), if any.
    add_on: Option<Size>,
}

impl Transmitter {
    fn datagrams(&self) -> u32 {
        u32::from(self.burst) + u32::from(self.add_on.is_some())
    }

    /// The UDP payload bytes of one Burst, random sizes at their maximum.
    fn bytes(&self) -> u64 {
        u64::from(self.payload.max) * u64::from(self.burst)
            + self.add_on.map_or(0, |a| u64::from(a.max))
    }
}

/// The Transmitters of `rate` (already sanitized), their first Bursts due
/// at `starts`.
fn transmitters(
    rate: &SendingRate,
    ip_version: IpVersion,
    starts: [MonoTime; 2],
) -> [Option<Transmitter>; 2] {
    let count = |burst: u32| u16::try_from(burst).unwrap_or(u16::MAX);
    let size = |field: u32| Size::new(field, ip_version);
    // A size of 0, random or not, means none.
    let some = |field: u32| field & !RANDOM_SIZE > 0;
    let transmitter1 = (rate.tx_interval1 > 0 && rate.burst_size1 > 0 && some(rate.udp_payload1))
        .then(|| Transmitter {
            interval: Duration::from_micros(rate.tx_interval1.into()),
            next: starts[0],
            payload: size(rate.udp_payload1),
            burst: count(rate.burst_size1),
            add_on: None,
        });
    let burst2 = if some(rate.udp_payload2) {
        count(rate.burst_size2)
    } else {
        0
    };
    let transmitter2 =
        (rate.tx_interval2 > 0 && (burst2 > 0 || some(rate.udp_addon2))).then(|| Transmitter {
            interval: Duration::from_micros(rate.tx_interval2.into()),
            next: starts[1],
            payload: size(rate.udp_payload2),
            burst: burst2,
            add_on: some(rate.udp_addon2).then(|| size(rate.udp_addon2)),
        });
    [transmitter1, transmitter2]
}

struct LoadSender {
    transmitters: [Option<Transmitter>; 2],
    /// The Sending Rate Structure being executed, sanitized and kept to
    /// `cap_kbps`.
    rate: SendingRate,
    /// The structure last assigned, sanitized.
    assigned: SendingRate,
    /// The largest payload the Test's table variant allows.
    max_payload: u32,
    /// A Client's stated maxBandwidth, in kbit/s.
    cap_kbps: Option<u64>,
    ip_version: IpVersion,
    status_received: bool,
    next_plan_id: u64,
    /// The next Load PDU's sequence number; the first is 1 (RFC 9946 §8.1).
    next_seq: u32,
    random: Option<RandomPayload>,
    /// Draws random sizes.
    sizes: SplitMix64,
    /// The latest Status PDU's spduTime and when it arrived, which Load
    /// PDUs echo with rttRespDelay (RFC 9946 §8.1).
    last_status: Option<((u32, u32), MonoTime)>,
    /// The latest subIntSeqNo whose sisSav was reported (0: none yet).
    last_sub_int_seq_no: u32,
    /// RTT variation samples from Status PDUs since the last Sub-Interval
    /// result: sum and count.
    rtt_var_sum: u64,
    rtt_var_count: u32,
    /// What it sent, per st.
    sent_grid: SentGrid,
    /// Deferred Datagrams from the last Send Plan.
    deferred: Option<SendPlan>,
    /// The spduSeqNo of the last Status PDU processed; 0 before the first.
    last_spdu_seq_no: u32,
    /// The Status PDUs missing before the last one processed (spduSeqErr).
    spdu_seq_err: u16,
}

impl LoadSender {
    #[allow(clippy::too_many_arguments)]
    fn new(
        rate: &SendingRate,
        max_payload: u32,
        cap_kbps: Option<u64>,
        ip_version: IpVersion,
        random_payload: bool,
        now: Now,
        rng: &mut SplitMix64,
    ) -> Self {
        let assigned = sanitize(rate, max_payload);
        let rate = keep_to(assigned, cap_kbps, ip_version).0;
        let mut start =
            || MonoTime::from_duration(now.mono.as_duration() + rng.duration_in(START_OFFSET));
        let starts = [start(), start()];
        Self {
            transmitters: transmitters(&rate, ip_version, starts),
            rate,
            assigned,
            max_payload,
            cap_kbps,
            ip_version,
            status_received: false,
            next_plan_id: 0,
            next_seq: 1,
            last_sub_int_seq_no: 0,
            rtt_var_sum: 0,
            rtt_var_count: 0,
            sent_grid: SentGrid::new(Duration::ZERO, Duration::ZERO, true),
            random: random_payload.then(|| RandomPayload::new(rng)),
            sizes: SplitMix64(rng.next()),
            last_status: None,
            deferred: None,
            last_spdu_seq_no: 0,
            spdu_seq_err: 0,
        }
    }

    /// Executes a new Sending Rate Structure from here on (RFC 9946 §8.1).
    /// A Transmitter that goes on keeps its schedule; a new one starts at
    /// once. Returns the rate it asked for, in kbit/s, if it was a new
    /// structure above `cap_kbps`.
    fn set_rate(&mut self, rate: &SendingRate, now: Now) -> Option<u64> {
        let assigned = sanitize(rate, self.max_payload);
        if assigned == self.assigned {
            return None;
        }
        self.assigned = assigned;
        let (rate, capped) = keep_to(assigned, self.cap_kbps, self.ip_version);
        if rate == self.rate {
            return capped;
        }
        let old = self.transmitters;
        let starts = [0, 1].map(|i| old[i].map_or(now.mono, |t| t.next));
        self.transmitters = transmitters(&rate, self.ip_version, starts);
        // A new interval restarts its schedule from now at the latest, so
        // Bursts of the old one are not counted as skipped under the new.
        for (new, old) in self.transmitters.iter_mut().zip(old) {
            if let (Some(new), Some(old)) = (new, old)
                && new.interval != old.interval
            {
                new.next = new.next.max(now.mono);
            }
        }
        self.rate = rate;
        capped
    }

    /// A final STOP2 send now: any Deferred Datagrams, then one more of
    /// the first Transmitter's size, or a header.
    fn stop_plan(&mut self, now: Now) -> SendPlan {
        let len = self
            .transmitters
            .iter()
            .flatten()
            .next()
            .map_or(MIN_DATAGRAM, |t| t.payload.max);
        let interval = Duration::from_secs(1);
        // Deferred Datagrams go first, so none is left behind.
        let deferred = self.deferred.take();
        let first_seq = deferred.map_or(self.next_seq, |d| d.first_seq);
        let mut plan = SendPlan::new(self.next_plan_id, now.mono, interval, first_seq);
        self.next_plan_id += 1;
        if let Some(deferred) = deferred {
            for run in deferred.runs() {
                plan.push(*run);
            }
            plan.deferred = deferred.datagrams() as u16;
        }
        plan.push(Run { len, count: 1 });
        self.next_seq = self.next_seq.wrapping_add(1);
        plan
    }

    fn next_burst_at(&self) -> Option<MonoTime> {
        self.transmitters.iter().flatten().map(|t| t.next).min()
    }

    fn poll_burst(
        &mut self,
        now: Now,
        horizon: Duration,
        one_datagram: bool,
        budget: Duration,
        pending: &mut PendingEvents,
    ) -> Option<SendPlan> {
        // Catch-up by time budget (ADR 0012): a missed Burst is still sent,
        // one per call, while it is less than `budget` late; older ones are
        // Skipped Bursts. The most recent missed Burst always goes
        // (conformance: missed-bursts-catch-up-or-are-skipped).
        for transmitter in self.transmitters.iter_mut().flatten() {
            let late = now
                .mono
                .as_duration()
                .saturating_sub(transmitter.next.as_duration());
            let interval = transmitter.interval.as_nanos();
            let behind = late.as_nanos() / interval;
            let skip = match late.checked_sub(budget) {
                Some(beyond) => (beyond.as_nanos() / interval + 1).min(behind),
                None => 0,
            };
            if skip > 0 {
                let skipped = u32::try_from(skip).unwrap_or(u32::MAX);
                transmitter.next = MonoTime::from_duration(
                    transmitter.next.as_duration() + Duration::from_nanos((interval * skip) as u64),
                );
                pending.skipped_bursts = pending.skipped_bursts.saturating_add(skipped);
                pending.skipped_datagrams = pending
                    .skipped_datagrams
                    .saturating_add(skipped.saturating_mul(transmitter.datagrams()));
                pending.skipped_bytes = pending
                    .skipped_bytes
                    .saturating_add(u64::from(skipped) * transmitter.bytes());
            }
        }
        let transmitter = self
            .transmitters
            .iter_mut()
            .flatten()
            .min_by_key(|t| t.next)?;
        if transmitter.next.as_duration() > now.mono.as_duration() + horizon {
            return None;
        }
        let deferred = self.deferred.take();
        let mut plan = SendPlan::new(
            self.next_plan_id,
            transmitter.next,
            transmitter.interval,
            deferred.map_or(self.next_seq, |d| d.first_seq),
        );
        self.next_plan_id += 1;
        if let Some(deferred) = deferred {
            for run in deferred.runs() {
                plan.push(*run);
            }
            plan.deferred = deferred.datagrams() as u16;
            // At least a Burst's worth still waits, or the plan would hold
            // too many runs: this Burst is withheld.
            if deferred.datagrams() >= transmitter.datagrams()
                || usize::from(plan.run_count) + 2 > MAX_RUNS
            {
                transmitter.next =
                    MonoTime::from_duration(transmitter.next.as_duration() + transmitter.interval);
                pending.withheld_bursts = pending.withheld_bursts.saturating_add(1);
                pending.withheld_datagrams = pending
                    .withheld_datagrams
                    .saturating_add(transmitter.datagrams());
                pending.withheld_bytes = pending.withheld_bytes.saturating_add(transmitter.bytes());
                return Some(plan);
            }
        }
        let count = if one_datagram {
            transmitter.burst.min(1)
        } else {
            transmitter.burst
        };
        // A random size is drawn once per Burst, as OB-UDPST does, so a
        // Burst stays one run.
        plan.push(Run {
            len: transmitter.payload.draw(self.ip_version, &mut self.sizes),
            count,
        });
        if let Some(add_on) = transmitter.add_on
            && !(one_datagram && count > 0)
        {
            plan.push(Run {
                len: add_on.draw(self.ip_version, &mut self.sizes),
                count: 1,
            });
        }
        transmitter.next =
            MonoTime::from_duration(transmitter.next.as_duration() + transmitter.interval);
        self.next_seq = self
            .next_seq
            .wrapping_add(plan.datagrams() - u32::from(plan.deferred));
        Some(plan)
    }

    fn fill_payload(&mut self, content: &mut [u8]) {
        match &mut self.random {
            Some(random) => random.fill(content),
            // The runtime reuses its buffer, so stale bytes are cleared.
            None => content.fill(0),
        }
    }
}

/// Received datagrams, their bytes (from udpPayload), Sequence Errors and
/// delay variation in a Trial Interval or Sub-Interval.
#[derive(Debug, Clone, Copy)]
struct Counts {
    datagrams: u32,
    bytes: u64,
    loss: u32,
    ooo: u32,
    dup: u32,
    /// The next expected lpduSeqNo when the interval opened, if any Load
    /// PDU had arrived: gaps from here on were counted in this interval.
    from: Option<u32>,
    /// One-way delay variation samples, in ms: STATUS_NODEL as the minimum
    /// until the first.
    delay: VariationStats,
    /// RTT variation samples' range, in ms (Sub-Intervals only).
    rtt_min: u32,
    rtt_max: u32,
    /// Their sum and count, for the mean.
    rtt_sum: u64,
    rtt_count: u32,
}

/// Minimum, maximum, sum and count of delay variation samples in ms.
#[derive(Debug, Clone, Copy)]
struct VariationStats {
    min: u32,
    max: u32,
    /// Kept in u64 here; a Status PDU's u32 field saturates.
    sum: u64,
    count: u32,
}

impl Default for VariationStats {
    fn default() -> Self {
        Self {
            min: STATUS_NODEL,
            max: 0,
            sum: 0,
            count: 0,
        }
    }
}

impl VariationStats {
    fn add(&mut self, sample: u32) {
        self.min = self.min.min(sample);
        self.max = self.max.max(sample);
        self.sum = self.sum.saturating_add(u64::from(sample));
        self.count = self.count.saturating_add(1);
    }

    /// The sum as a Status PDU carries it, saturating.
    /// (conformance: status-pdu-sums-saturate)
    fn wire_sum(&self) -> u32 {
        u32::try_from(self.sum).unwrap_or(u32::MAX)
    }
}

impl Default for Counts {
    fn default() -> Self {
        Self {
            datagrams: 0,
            bytes: 0,
            loss: 0,
            ooo: 0,
            dup: 0,
            from: None,
            delay: VariationStats::default(),
            rtt_min: STATUS_NODEL,
            rtt_max: 0,
            rtt_sum: 0,
            rtt_count: 0,
        }
    }
}

impl Counts {
    fn opened(from: Option<u32>) -> Self {
        Self {
            from,
            ..Self::default()
        }
    }

    fn add_rtt_variation(&mut self, sample: u32) {
        self.rtt_min = self.rtt_min.min(sample);
        self.rtt_max = self.rtt_max.max(sample);
        self.rtt_sum = self.rtt_sum.saturating_add(u64::from(sample));
        self.rtt_count = self.rtt_count.saturating_add(1);
    }

    /// Counting moved back to `expected`: gaps from there on are counted
    /// in this interval.
    fn resynced(&mut self, expected: u32) {
        if let Some(from) = self.from
            && is_before(expected, from)
        {
            self.from = Some(expected);
        }
    }

    fn add(&mut self, bytes: u16, arrival: Arrival) {
        self.datagrams = self.datagrams.saturating_add(1);
        self.bytes = self.bytes.saturating_add(u64::from(bytes));
        match arrival {
            Arrival::InOrder { lost } => self.loss = self.loss.saturating_add(lost),
            Arrival::Late { seq_no } => {
                self.ooo = self.ooo.saturating_add(1);
                // It was counted as lost when the gap opened. If that was in
                // an earlier interval, that interval keeps the count and
                // this one's loss is left alone.
                if self.from.is_none_or(|from| !is_before(seq_no, from)) {
                    self.loss = self.loss.saturating_sub(1);
                }
            }
            Arrival::Duplicate => self.dup = self.dup.saturating_add(1),
        }
    }
}

/// Whether lpduSeqNo `a` comes before `b`, modulo 2^32: `b` is less than
/// 2^31 ahead. At exactly 2^31 apart, `a` comes before.
fn is_before(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) < 0
}

/// How a Load PDU's lpduSeqNo compares with those received before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arrival {
    /// At or after the next expected lpduSeqNo; `lost` were skipped.
    InOrder { lost: u32 },
    /// Before the next expected lpduSeqNo and not in the lookback.
    Late { seq_no: u32 },
    /// Before the next expected lpduSeqNo and in the lookback.
    Duplicate,
}

/// Lookback entries (RFC 9946 §8.2: "n=32").
const LOOKBACK: usize = 32;

/// The largest jump counted at once: from the next expected lpduSeqNo, or
/// a first Load PDU's from 1, either way. A larger one waits for the next
/// Load PDU to continue from it.
const MAX_UNCONFIRMED_JUMP: u32 = 1024;

/// Out-of-order Load PDUs in a consecutive run, more than
/// [`MAX_UNCONFIRMED_JUMP`] behind, after which counting takes up from
/// them: the lookback's size.
const RESYNC_RUN: u32 = LOOKBACK as u32;

/// What one Load PDU's lpduSeqNo shows.
struct Checked {
    /// The Load PDUs it lets count, in order: their size in bytes and how
    /// they arrived. A held jump it confirms comes with it; while it is
    /// held itself, it isn't among them.
    counted: [Option<(u16, Arrival)>; 2],
    /// Whether it came in order itself.
    in_order: bool,
    /// A held jump that it didn't continue from, so it is ignored.
    ignored: bool,
    /// The next expected lpduSeqNo, when a run moved it back to follow it.
    resynced: Option<u32>,
}

impl Checked {
    fn count(&mut self, bytes: u16, arrival: Arrival) {
        let free = self.counted.iter_mut().find(|c| c.is_none());
        *free.expect("at most two Load PDUs count at once") = Some((bytes, arrival));
    }
}

/// Sequence error detection (RFC 9946 §8.2).
///
/// - lpduSeqNo starts at 1, so Load PDUs before the first one received
///   count as lost (none if it is 0).
/// - Each higher lpduSeqNo sets the next expected one to lpduSeqNo + 1 and
///   counts the gap as lost, in Load PDUs.
/// - A lower one is a duplicate if it is among the last 32 lpduSeqNos
///   entered in the lookback, and otherwise out of order. Duplicates are
///   not entered, so a duplicate of an older Load PDU counts as out of
///   order.
/// - An out-of-order Load PDU takes one off the loss count, as OB-UDPST
///   does, but only in an
///   interval that counted it as lost. OB-UDPST takes it off whatever
///   interval is open, which can cancel an unrelated loss.
/// - lpduSeqNos are compared modulo 2^32, so the count carries on across
///   the wrap from 4294967295 to 0 (§18 item 7). OB-UDPST doesn't wrap.
///
/// Load PDUs carry no authentication. In OB-UDPST one forged Load PDU up to
/// 2^31 ahead leaves every later one out of order, hiding all loss for the
/// rest of the Test. Here:
/// - A jump of more than 1024 (for a first Load PDU, more than 1024 from 1
///   either way) is held until the next Load PDU continues from it, within
///   1024 on either side; then both count, a first Load PDU's jump as no
///   loss. Otherwise it is ignored, counting as neither received nor lost.
///   A jump of up to 1024 counts at once, so one forged Load PDU can still
///   move counting up to 1024 ahead.
/// - 32 out-of-order Load PDUs in a consecutive run, more than 1024 behind
///   the next expected lpduSeqNo, make it follow them: a jump that two
///   forged Load PDUs confirmed stops hiding loss after the next 32 Load
///   PDUs received in a row. The loss such a jump counted stays counted.
///   Forged Load PDUs can so add loss, which lowers a result as an on-path
///   attacker dropping traffic can, but can't hide it for long.
///
/// The held Load PDU gives no delay or RTT sample, and counts in the
/// interval of the Load PDU that confirms it.
///
/// (conformance: sequence-errors-count-load-pdus, late-load-pdu-decrement-in-its-own-interval, lpdu-seq-no-modulo-2-32, lpdu-seq-no-jumps-held)
struct Lookback {
    expected: u32,
    started: bool,
    /// A jump held for confirmation: its lpduSeqNo and size.
    held: Option<(u32, u16)>,
    /// The last out-of-order lpduSeqNo and how many ran consecutively to
    /// it.
    late_run: (u32, u32),
    entries: [u32; LOOKBACK],
    len: usize,
    next: usize,
}

impl Lookback {
    fn new() -> Self {
        Self {
            expected: 1,
            started: false,
            held: None,
            late_run: (0, 0),
            entries: [0; LOOKBACK],
            len: 0,
            next: 0,
        }
    }

    /// The next expected lpduSeqNo, once a Load PDU has arrived.
    fn next_expected(&self) -> Option<u32> {
        self.started.then_some(self.expected)
    }

    fn check(&mut self, seq_no: u32, bytes: u16) -> Checked {
        let mut checked = Checked {
            counted: [None, None],
            in_order: false,
            ignored: false,
            resynced: None,
        };
        if let Some((held, held_bytes)) = self.held.take() {
            let apart = seq_no.wrapping_sub(held) as i32;
            // A duplicate of it confirms it too, and then counts as one.
            if apart.unsigned_abs() <= MAX_UNCONFIRMED_JUMP {
                if apart < 0 {
                    // Both are past the jump: this one came first.
                    let arrival = self.accept(seq_no);
                    checked.count(bytes, arrival);
                    checked.in_order = true;
                    let arrival = self.accept(held);
                    checked.count(held_bytes, arrival);
                    return checked;
                }
                let arrival = self.accept(held);
                checked.count(held_bytes, arrival);
            } else {
                checked.ignored = true;
            }
        }
        if self.jumps(seq_no) {
            self.held = Some((seq_no, bytes));
            return checked;
        }
        // Nothing received yet cannot be late.
        if !self.started || !is_before(seq_no, self.expected) {
            let lost = self.skipped(seq_no);
            self.enter(seq_no, true);
            checked.count(bytes, Arrival::InOrder { lost });
            checked.in_order = true;
        } else if self.entries[..self.len].contains(&seq_no) {
            checked.count(bytes, Arrival::Duplicate);
        } else {
            let (last, run) = self.late_run;
            let run = if run > 0 && seq_no == last.wrapping_add(1) {
                run + 1
            } else {
                1
            };
            self.late_run = (seq_no, run);
            self.enter(seq_no, false);
            if run >= RESYNC_RUN && self.expected.wrapping_sub(seq_no) > MAX_UNCONFIRMED_JUMP {
                self.expected = seq_no.wrapping_add(1);
                self.late_run = (0, 0);
                checked.resynced = Some(self.expected);
            }
            checked.count(bytes, Arrival::Late { seq_no });
        }
        checked
    }

    /// Whether `seq_no` jumps more than [`MAX_UNCONFIRMED_JUMP`] ahead, or
    /// a first Load PDU that far from 1 either way.
    fn jumps(&self, seq_no: u32) -> bool {
        if self.started {
            !is_before(seq_no, self.expected)
                && seq_no.wrapping_sub(self.expected) > MAX_UNCONFIRMED_JUMP
        } else {
            (seq_no.wrapping_sub(1) as i32).unsigned_abs() > MAX_UNCONFIRMED_JUMP
        }
    }

    /// A held jump, or a Load PDU next to it, taken in order: its loss is
    /// the gap from the next expected lpduSeqNo, none for a first one.
    fn accept(&mut self, seq_no: u32) -> Arrival {
        let lost = if self.started {
            self.skipped(seq_no)
        } else {
            0
        };
        self.enter(seq_no, true);
        Arrival::InOrder { lost }
    }

    /// The Load PDUs skipped before `seq_no`: none if it is behind the next
    /// expected one (a first Load PDU numbered 0).
    fn skipped(&self, seq_no: u32) -> u32 {
        if is_before(seq_no, self.expected) {
            0
        } else {
            seq_no.wrapping_sub(self.expected)
        }
    }

    /// Enters `seq_no` in the lookback, and moves the next expected one on
    /// past it if it came in order.
    fn enter(&mut self, seq_no: u32, in_order: bool) {
        if in_order {
            self.started = true;
            self.expected = seq_no.wrapping_add(1);
            self.late_run = (0, 0);
        }
        self.entries[self.next] = seq_no;
        self.next = (self.next + 1) % LOOKBACK;
        self.len = (self.len + 1).min(LOOKBACK);
    }
}

/// The Load Receiver's statistics and Status PDU timer (RFC 9946 §8.2).
///
/// Where the RFC is silent, OB-UDPST's choices are kept, so both report
/// the same Sub-Intervals:
/// - A Sub-Interval opens when the first Status PDU is sent. It closes just
///   after the first Status PDU sent more than subIntPeriod - trialInt/2
///   (in whole ms) after it opened, so the next Status PDU reports it. Its
///   deltaTime is the measured duration.
/// - Until one has completed, subIntSeqNo is 0 and sisSav is zero.
/// - accumTime adds up each Sub-Interval's duration rounded to ms.
///
/// Unlike OB-UDPST (§18 items 8 and 9), Status PDUs stay on a Trial
/// Interval grid from the first Load PDU, the first tiDeltaTime is measured
/// from the first Load PDU rather than from activation, and tiRxBytes
/// saturates rather than wrapping.
///
/// (conformance: sub-interval-boundaries, status-pdu-grid-from-first-load-pdu, status-pdu-sums-saturate)
/// A Load Receiver's estimate of what the Load Sender sent, st by st on
/// the Load Sender's clock: see [`DataEvent::SentEstimated`].
#[derive(Debug, Clone, Default)]
struct SentEstimate {
    st: Duration,
    /// How many st the Test Interval holds: no more are reported.
    most: u32,
    ipv4: bool,
    /// The first lpduTime, in ns: the grid's origin.
    origin: Option<i128>,
    /// The open st's index, and the highest lpduSeqNo, Load PDUs and bytes
    /// received in it.
    index: u32,
    highest: Option<u32>,
    received: u32,
    bytes: u64,
    /// The highest lpduSeqNo of the st before, where this one's span
    /// starts; the first st's starts at its lowest.
    before: Option<u32>,
    lowest: Option<u32>,
    /// Test-only accounting: fail a work regression before it can hang.
    #[cfg(test)]
    closed_intervals: u32,
    #[cfg(test)]
    close_limit: Option<u32>,
}

impl SentEstimate {
    /// A Load PDU received: `seq_no`, sent at `lpdu_ns`, of `bytes`.
    fn add(&mut self, seq_no: u32, lpdu_ns: i128, bytes: u16, out: &mut VecDeque<SentInterval>) {
        let st = self.st.as_nanos() as i128;
        if st == 0 {
            return;
        }
        let origin = *self.origin.get_or_insert(lpdu_ns);
        let Ok(index) = u32::try_from((lpdu_ns - origin).div_euclid(st)) else {
            // An earlier st, or outside the index range: no estimate.
            return;
        };
        if index < self.index {
            return;
        }
        // Report only the negotiated Test Interval's st. A peer's
        // timestamp must not make us walk arbitrary empty intervals.
        while self.index < index && self.index < self.most {
            self.close(out);
        }
        if self.index < index {
            // The first closure still carries the open st's highest
            // sequence number into `before`. Further empty closures
            // change only the index, so skip them in one step. At most
            // `most + 1` closures are needed for any Load PDU.
            self.close(out);
            self.index = index;
        }
        self.highest = Some(self.highest.map_or(seq_no, |h| h.max(seq_no)));
        if self.before.is_none() {
            self.lowest = Some(self.lowest.map_or(seq_no, |l| l.min(seq_no)));
        }
        self.received = self.received.saturating_add(1);
        self.bytes = self.bytes.saturating_add(u64::from(bytes));
    }

    /// Reports the open st and opens the next.
    fn close(&mut self, out: &mut VecDeque<SentInterval>) {
        #[cfg(test)]
        {
            self.closed_intervals += 1;
            if let Some(limit) = self.close_limit {
                assert!(
                    self.closed_intervals <= limit,
                    "sender estimate exceeded its work bound"
                );
            }
        }
        if self.index < self.most {
            let sent = match (
                self.highest,
                self.before.or(self.lowest.map(|l| l.wrapping_sub(1))),
            ) {
                (Some(highest), Some(before)) => highest.wrapping_sub(before),
                _ => 0,
            };
            // A lost Load PDU as big as the ones that arrived around it in
            // the st: their mean. Loss that falls on one size of Load PDU,
            // as a full tail-drop queue's may, biases it.
            let bytes = match self.received.min(sent) {
                0 => 0,
                _ => self.bytes * u64::from(sent) / u64::from(self.received),
            };
            out.push_back(SentInterval::new(
                self.index + 1,
                sent,
                bytes,
                self.st,
                self.ipv4,
            ));
        }
        if self.highest.is_some() {
            self.before = self.highest;
        }
        self.index += 1;
        self.highest = None;
        self.received = 0;
        self.bytes = 0;
    }
}

struct LoadReceiver {
    trial: Duration,
    sub_interval: Duration,
    /// The Sending Rate Structure its Status PDUs carry.
    sending_rate: SendingRate,
    /// When the first valid Load PDU arrived.
    first_load: Option<MonoTime>,
    /// When the next Status PDU is due, on a grid from the first Load PDU.
    next_status: Option<MonoTime>,
    last_status: Option<MonoTime>,
    trial_counts: Counts,
    lookback: Lookback,
    /// The smallest receive time minus lpduTime so far, in ms: seeded by
    /// the first Load PDU and never reset (RFC 9946 §8.2).
    clock_delta_min: Option<i32>,
    /// The smallest adjusted RTT so far, in ms.
    rtt_minimum: Option<u32>,
    /// The latest RTT variation sample in this Trial Interval.
    rtt_var_sample: Option<u32>,
    /// The spduTime of the last RTT sample: a Load PDU echoing a new one
    /// gives the next sample.
    rtt_spdu_time: (u32, u32),
    /// Whether clockDeltaMin or rttMinimum changed since the last Status
    /// PDU.
    delay_min_upd: bool,
    /// The open Sub-Interval: when it opened and what it has received.
    open: Option<(MonoTime, Counts)>,
    saved: SubIntervalStats,
    /// The closed Sub-Interval's delay variation sum, unsaturated.
    saved_delay_sum: u64,
    /// The closed Sub-Interval's RTT variation samples: sum and count.
    saved_rtt: (u64, u32),
    sub_int_seq_no: u32,
    accum_ms: u32,
    spdu_seq_no: u32,
    /// The common grid's origin in a Multi-Connection Test: Status PDUs go
    /// out on its Trial Intervals, and Sub-Interval `j` runs from
    /// `grid + (j - 1) * subIntPeriod`.
    grid: Option<MonoTime>,
    /// On the grid: the open Sub-Interval's number, and the closed one's
    /// if it is a measurement. A Sub-Interval closed more than half a
    /// Trial Interval late, after a stall, holds more than its time and
    /// isn't; nor is one that ran less than half its time, the first.
    open_bin: i64,
    saved_bin: Option<u32>,
    /// What the Load Sender sent, estimated.
    sent: SentEstimate,
}

impl LoadReceiver {
    fn new(trial: Duration, sub_interval: Duration) -> Self {
        Self {
            // Defensive: a zero trialInt would stop the Status PDU timer
            // from advancing. The Client refuses one
            // (`parameters::unrunnable`) and the Server clamps it.
            trial: trial.max(Duration::from_millis(1)),
            sub_interval,
            first_load: None,
            next_status: None,
            last_status: None,
            trial_counts: Counts::opened(None),
            lookback: Lookback::new(),
            clock_delta_min: None,
            rtt_minimum: None,
            rtt_var_sample: None,
            rtt_spdu_time: (0, 0),
            delay_min_upd: false,
            open: None,
            saved: SubIntervalStats::default(),
            saved_delay_sum: 0,
            saved_rtt: (0, 0),
            sub_int_seq_no: 0,
            accum_ms: 0,
            spdu_seq_no: 0,
            sending_rate: SendingRate::default(),
            grid: None,
            open_bin: 0,
            saved_bin: None,
            sent: SentEstimate {
                st: trial.max(Duration::from_millis(1)),
                ..SentEstimate::default()
            },
        }
    }

    /// On the grid, the number of the Sub-Interval that `at`, a Status
    /// PDU's time, opens: the one it falls in, half a Trial Interval on,
    /// so a Status PDU at a boundary opens the next.
    fn bin(&self, grid: MonoTime, at: MonoTime) -> i64 {
        let from_origin = mono_ns(at) + (self.trial.as_nanos() / 2) as i128 - mono_ns(grid);
        let dt = self.sub_interval.as_nanos().max(1) as i128;
        i64::try_from(from_origin.div_euclid(dt) + 1).unwrap_or(i64::MAX)
    }

    /// The number of the Sub-Interval the last close measured, if it did:
    /// its grid number on a grid, else the count of those closed.
    fn measured_seq_no(&self) -> Option<u32> {
        match self.grid {
            Some(_) => self.saved_bin,
            None => Some(self.sub_int_seq_no),
        }
    }

    /// A valid Load PDU that arrived at `at`, which is `received` in wire
    /// time (the Test Time Base plus the wall offset, in ns) unless its
    /// batch's clocks disagreed.
    fn receive(
        &mut self,
        at: MonoTime,
        received: Option<i128>,
        header: &LoadHeader,
        pending: &mut PendingEvents,
    ) {
        if self.first_load.is_none() {
            self.first_load = Some(at);
            // Status PDUs start only after the first Load PDU (RFC 9946 §8.2):
            // a Trial Interval after it, or on a grid, at its next point.
            let next = match self.grid {
                Some(grid) => {
                    let trial = self.trial.as_nanos().max(1) as i128;
                    let steps = (mono_ns(at) - mono_ns(grid)).div_euclid(trial) + 1;
                    let ns = mono_ns(grid) + steps * trial;
                    MonoTime::from_duration(Duration::from_nanos(u64::try_from(ns).unwrap_or(0)))
                }
                None => MonoTime::from_duration(at.as_duration() + self.trial),
            };
            self.next_status = Some(next);
            // On a grid, the first Sub-Interval counts from here, so it
            // holds all of its time that had load.
            if let Some(grid) = self.grid {
                self.open = Some((at, Counts::opened(self.lookback.next_expected())));
                let dt = self.sub_interval.as_nanos().max(1) as i128;
                let bin = (mono_ns(at) - mono_ns(grid)).div_euclid(dt) + 1;
                self.open_bin = i64::try_from(bin).unwrap_or(i64::MAX);
            }
            pending.first_load_pdu = true;
        }
        let checked = self.lookback.check(header.seq_no, header.udp_payload);
        if checked.ignored {
            pending.ignored_load_pdus += 1;
        } else {
            self.sent.add(
                header.seq_no,
                wire_ns(header.lpdu_time),
                header.udp_payload,
                &mut pending.sent_estimates,
            );
        }
        if let Some(expected) = checked.resynced {
            self.trial_counts.resynced(expected);
            if let Some((_, counts)) = &mut self.open {
                counts.resynced(expected);
            }
        }
        for (bytes, arrival) in checked.counted.into_iter().flatten() {
            self.trial_counts.add(bytes, arrival);
            if let Some((_, counts)) = &mut self.open {
                counts.add(bytes, arrival);
            }
        }
        // Out-of-order and duplicate Load PDUs give no delay samples, as in
        // OB-UDPST, and nor does a held jump.
        if let (true, Some(received)) = (checked.in_order, received) {
            self.rtt(received, header);
            self.delay(received, header.lpdu_time);
        }
    }

    /// One-way delay variation (RFC 9946 §8.2): receive time minus lpduTime,
    /// in ms, less the running clockDeltaMin. The first Load PDU only seeds
    /// clockDeltaMin, as in OB-UDPST.
    fn delay(&mut self, received: i128, lpdu_time: (u32, u32)) {
        let delta = ms_i32(received - wire_ns(lpdu_time));
        let Some(min) = self.clock_delta_min else {
            self.clock_delta_min = Some(delta);
            self.delay_min_upd = true;
            return;
        };
        if delta < min {
            self.clock_delta_min = Some(delta);
            self.delay_min_upd = true;
        }
        let sample = delta.abs_diff(self.clock_delta_min.unwrap_or(delta));
        self.trial_counts.delay.add(sample);
        if let Some((_, counts)) = &mut self.open {
            counts.delay.add(sample);
        }
    }

    /// An RTT sample when the Load PDU echoes a new spduTime (RFC 9946
    /// §8.1, §8.2): receive time minus that spduTime, less rttRespDelay,
    /// in ms. An rttRespDelay one more than the RTT is rounding and gives
    /// 0; a larger one leaves the RTT as it is, as in OB-UDPST. A negative
    /// RTT (a spduTime from the future, which only a forged or corrupted
    /// Load PDU carries) gives no sample.
    /// (conformance: rtt-sample-rules, negative-rtt-gives-no-sample)
    fn rtt(&mut self, received: i128, header: &LoadHeader) {
        // spduTime 0 means the Load Sender has nothing to echo yet.
        if header.spdu_time == self.rtt_spdu_time || header.spdu_time == (0, 0) {
            return;
        }
        self.rtt_spdu_time = header.spdu_time;
        let Ok(raw) = u32::try_from(ms(received - wire_ns(header.spdu_time))) else {
            return;
        };
        let response = u32::from(header.rtt_resp_delay);
        let rtt = if response <= raw {
            raw - response
        } else if response == raw + 1 {
            0
        } else {
            raw
        };
        // 0 stays 0 (conformance: rtt-minimum-may-be-zero).
        let minimum = self.rtt_minimum.map_or(rtt, |m| m.min(rtt));
        if self.rtt_minimum != Some(minimum) {
            self.rtt_minimum = Some(minimum);
            self.delay_min_upd = true;
        }
        let sample = rtt - minimum;
        self.rtt_var_sample = Some(sample);
        if let Some((_, counts)) = &mut self.open {
            counts.add_rtt_variation(sample);
        }
    }

    /// The Status PDU due at `now`, with the authentication fields zero.
    fn status(&mut self, now: MonoTime, spdu_time: (u32, u32)) -> StatusPdu {
        let since = |from: MonoTime| now.as_duration().saturating_sub(from.as_duration());
        let trial_start = self.last_status.or(self.first_load).unwrap_or(now);
        let trial = std::mem::replace(
            &mut self.trial_counts,
            Counts::opened(self.lookback.next_expected()),
        );
        self.last_status = Some(now);
        self.spdu_seq_no = self.spdu_seq_no.wrapping_add(1);
        // The next one on the grid; a late engine sends one now and moves on.
        let mut next = self.next_status.unwrap_or(now);
        while next <= now {
            next = MonoTime::from_duration(next.as_duration() + self.trial);
        }
        self.next_status = Some(next);
        let pdu = StatusPdu {
            test_action: load::test_action::TEST,
            // Set by the Data Phase.
            rx_stopped: 0,
            spdu_seq_no: self.spdu_seq_no,
            // The row in force. The caller replaces it before encoding when
            // the Server's algorithm moves the row, in a Status PDU whose
            // testAction stays TEST.
            sending_rate: self.sending_rate,
            sub_int_seq_no: self.sub_int_seq_no,
            sis_sav: self.saved,
            seq_err_loss: trial.loss,
            seq_err_ooo: trial.ooo,
            seq_err_dup: trial.dup,
            // Two's complement, as OB-UDPST sends it; 0 until the first
            // Load PDU (the first Status PDU follows one).
            // (conformance: clock-delta-min-encoding)
            clock_delta_min: self.clock_delta_min.unwrap_or(0) as u32,
            delay_var_min: trial.delay.min,
            delay_var_max: trial.delay.max,
            delay_var_sum: trial.delay.wire_sum(),
            delay_var_cnt: trial.delay.count,
            rtt_minimum: self.rtt_minimum.unwrap_or(STATUS_NODEL),
            rtt_var_sample: self.rtt_var_sample.take().unwrap_or(STATUS_NODEL),
            delay_min_upd: u8::from(std::mem::take(&mut self.delay_min_upd)),
            ti_delta_time: micros(since(trial_start)),
            ti_rx_datagrams: trial.datagrams,
            ti_rx_bytes: u32::try_from(trial.bytes).unwrap_or(u32::MAX),
            spdu_time,
            // Outside mode 2, keyId, authUnixTime and authDigest are zero
            // (RFC 9946 §5.3.2); authMode is the Test Connection's.
            auth_mode: 0,
            auth_unix_time: 0,
            auth_digest: [0; AUTH_DIGEST_LEN],
            key_id: 0,
            checksum: 0,
        };
        self.sub_interval_step(now);
        pdu
    }

    /// After a Status PDU is sent: opens the first Sub-Interval, or closes
    /// the open one once it has run more than subIntPeriod - trialInt/2 (in
    /// whole ms) and opens the next.
    fn sub_interval_step(&mut self, now: MonoTime) {
        let Some((opened, counts)) = self.open else {
            self.open = Some((now, Counts::opened(self.lookback.next_expected())));
            if let Some(grid) = self.grid {
                self.open_bin = self.bin(grid, now);
            }
            return;
        };
        let duration = now.as_duration().saturating_sub(opened.as_duration());
        let rounded_ms = (duration.as_micros() + 500) / 1000;
        if let Some(grid) = self.grid {
            let bin = self.bin(grid, now);
            if bin <= self.open_bin {
                return;
            }
            // Its end on the grid, and whether it closes late.
            let dt = self.sub_interval.as_nanos() as i128;
            let end = mono_ns(grid) + i128::from(self.open_bin) * dt;
            let late = mono_ns(now) - end > (self.trial.as_nanos() / 2) as i128;
            let short = duration < self.sub_interval / 2;
            self.saved_bin = u32::try_from(self.open_bin)
                .ok()
                .filter(|&j| j >= 1 && !late && !short);
            self.open_bin = bin;
        } else if rounded_ms <= self.sub_interval.saturating_sub(self.trial / 2).as_millis() {
            return;
        }
        self.accum_ms = self
            .accum_ms
            .saturating_add(u32::try_from(rounded_ms).unwrap_or(u32::MAX));
        self.sub_int_seq_no += 1;
        self.saved_delay_sum = counts.delay.sum;
        self.saved_rtt = (counts.rtt_sum, counts.rtt_count);
        self.saved = SubIntervalStats {
            rx_datagrams: counts.datagrams,
            rx_bytes: counts.bytes,
            delta_time: micros(duration),
            delay_var_min: counts.delay.min,
            delay_var_max: counts.delay.max,
            delay_var_sum: counts.delay.wire_sum(),
            delay_var_cnt: counts.delay.count,
            rtt_var_minimum: counts.rtt_min,
            rtt_var_maximum: counts.rtt_max,
            accum_time: self.accum_ms,
            seq_err_loss: counts.loss,
            seq_err_ooo: counts.ooo,
            seq_err_dup: counts.dup,
        };
        self.open = Some((now, Counts::opened(self.lookback.next_expected())));
    }
}

/// What a Load Sender sent, on a grid of st (one Trial Interval) from its
/// first send: each st closes when a send falls past it, empty ones too,
/// up to as many as the Test Interval holds. Those after its last send
/// aren't reported.
#[derive(Debug, Clone, Copy)]
struct SentGrid {
    st: Duration,
    /// How many st the Test Interval holds: no more are reported.
    most: u32,
    ipv4: bool,
    start: Option<MonoTime>,
    /// 0-based index of the open st, and what was sent in it.
    index: u32,
    datagrams: u32,
    bytes: u64,
}

impl SentGrid {
    fn new(st: Duration, test_interval: Duration, ipv4: bool) -> Self {
        Self {
            st,
            most: expected_sub_intervals(test_interval, st),
            ipv4,
            start: None,
            index: 0,
            datagrams: 0,
            bytes: 0,
        }
    }

    fn add(&mut self, now: MonoTime, datagrams: u32, bytes: u64, out: &mut VecDeque<SentInterval>) {
        if self.st.is_zero() {
            return;
        }
        let start = *self.start.get_or_insert(now);
        let elapsed = now.as_duration().saturating_sub(start.as_duration());
        let index = u32::try_from(elapsed.as_nanos() / self.st.as_nanos()).unwrap_or(u32::MAX);
        while self.index < index && self.index < self.most {
            out.push_back(SentInterval::new(
                self.index + 1,
                std::mem::take(&mut self.datagrams),
                std::mem::take(&mut self.bytes),
                self.st,
                self.ipv4,
            ));
            self.index += 1;
        }
        self.index = self.index.max(index);
        self.datagrams = self.datagrams.saturating_add(datagrams);
        self.bytes = self.bytes.saturating_add(bytes);
    }
}

/// The mean of samples given as (sum, count), rounded half up.
fn mean((sum, count): (u64, u32)) -> Option<u32> {
    (count > 0).then(|| {
        let count = u64::from(count);
        u32::try_from((sum * 2 + count) / (2 * count)).unwrap_or(u32::MAX)
    })
}

/// When a Status PDU arrived, in the Test Time Base: its kernel receive
/// timestamp, which is wall time, converted with the clock pair read with
/// it, or the read time where the platform gives no timestamp. As for Load
/// PDUs, it is kept no later than the read and no earlier than the receive
/// timeout before it.
/// (conformance: kernel-receive-timestamp-clamped)
fn status_received_at(pdu: &RxPdu<'_>) -> MonoTime {
    let Some(wall) = pdu.kernel_wall else {
        return pdu.now.mono;
    };
    let read = mono_ns(pdu.now.mono);
    let offset = pdu.now.wall.as_unix().as_nanos() as i128 - read;
    let mono = (wall.as_unix().as_nanos() as i128 - offset)
        .min(read)
        .max(read - RECEIVE_TIMEOUT.as_nanos() as i128)
        .max(0);
    MonoTime::from_duration(Duration::from_nanos(mono as u64))
}

/// How far the wall-to-mono offsets of a read's clock pair may differ
/// before its kernel timestamps aren't trusted.
const CLOCK_PAIR_TOLERANCE_NS: i128 = 1_000_000;

fn mono_ns(mono: MonoTime) -> i128 {
    mono.as_duration().as_nanos() as i128
}

/// A signed span in ns as ms, rounded half up with floor division, as
/// OB-UDPST rounds (its timespecs keep 0 <= tv_nsec < 1 s).
/// (conformance: ms-rounding-half-up)
fn ms(ns: i128) -> i128 {
    (ns + 500_000).div_euclid(1_000_000)
}

/// [`ms`] saturated to an `i32`, as clockDeltaMin is carried.
fn ms_i32(ns: i128) -> i32 {
    i32::try_from(ms(ns)).unwrap_or(if ns < 0 { i32::MIN } else { i32::MAX })
}

/// lpduTime or spduTime (seconds, nanoseconds) in ns.
fn wire_ns((secs, nanos): (u32, u32)) -> i128 {
    i128::from(secs) * 1_000_000_000 + i128::from(nanos)
}

/// A duration in µs, rounded to the nearest.
fn micros(d: Duration) -> u32 {
    u32::try_from((d.as_nanos() + 500) / 1000).unwrap_or(u32::MAX)
}

/// Random payload content cheap enough for 1 Mpps: each datagram
/// copies a pool of pseudo-random bytes from a random offset and XORs it
/// with a fresh 64-bit value, so no byte sequence repeats across datagrams
/// for a compressor to find.
struct RandomPayload {
    pool: Box<[u8]>,
    rng: SplitMix64,
}

/// Offsets into the pool are drawn from `0..POOL_SPAN`.
const POOL_SPAN: usize = 16 * 1024;
/// The largest UDP payload a Sending Rate Structure carries (RFC 9946 §6.1).
const MAX_PAYLOAD: usize = 8972;

impl RandomPayload {
    fn new(rng: &mut SplitMix64) -> Self {
        let mut own = SplitMix64(rng.next());
        let mut pool = vec![0u8; POOL_SPAN + MAX_PAYLOAD].into_boxed_slice();
        for chunk in pool.chunks_mut(8) {
            let bytes = own.next().to_le_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
        }
        Self { pool, rng: own }
    }

    fn fill(&mut self, content: &mut [u8]) {
        let draw = self.rng.next();
        let at = (draw as usize) % POOL_SPAN;
        let len = content.len().min(MAX_PAYLOAD);
        content[..len].copy_from_slice(&self.pool[at..at + len]);
        let mask = self.rng.next();
        let (words, rest) = content.as_chunks_mut::<8>();
        for word in words {
            *word = (u64::from_ne_bytes(*word) ^ mask).to_ne_bytes();
        }
        for (byte, m) in rest.iter_mut().zip(mask.to_ne_bytes()) {
            *byte ^= m;
        }
    }
}

/// SplitMix64: a small, well-known generator; not for security.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn duration_in(&mut self, range: std::ops::RangeInclusive<Duration>) -> Duration {
        let low = range.start().as_nanos() as u64;
        let span = range.end().as_nanos() as u64 - low + 1;
        Duration::from_nanos(low + self.next() % span)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::time::WallTime;

    fn at(ms: u64) -> MonoTime {
        MonoTime::from_duration(Duration::from_millis(ms))
    }

    fn now(ms: u64) -> Now {
        Now {
            mono: at(ms),
            wall: WallTime::from_unix(Duration::from_millis(1000 + ms)),
        }
    }

    fn receiver_phase(test_interval: Duration) -> DataPhase {
        let setup = DataPhaseSetup {
            now: now(0),
            peer: "127.0.0.1:24601".parse().unwrap(),
            dscp_ecn: 0,
            seed: 1,
            checksum: false,
            keys: None,
            key_id: 0,
            auth_mode: auth_mode::MODE_1,
            test_interval,
            trial_interval: Duration::from_millis(50),
            sub_interval: Duration::from_secs(1),
            this_end: End::Server,
            thresholds: None,
            algorithm: None,
            counter: None,
            max_payload: 1222,
            grid: None,
            max_bandwidth_kbps: None,
        };
        DataPhase::load_receiver(setup, Duration::from_secs(1), SendingRate::default())
    }

    fn receive_load(
        phase: &mut DataPhase,
        ms: u64,
        seq_no: u32,
        lpdu_time: (u32, u32),
        bytes: u16,
    ) {
        let header = LoadHeader {
            seq_no,
            lpdu_time,
            udp_payload: bytes,
            ..LoadHeader::default()
        };
        let mut datagram = RxDatagram {
            header: [0; LOAD_HEADER_LEN],
            actual_len: usize::from(bytes),
            kernel_wall: None,
            tos: None,
        };
        header.encode_into(&mut datagram.header);
        phase.handle_load_batch(RxBatch {
            before: now(ms),
            after: now(ms),
            datagrams: &[datagram],
        });
    }

    // Spec story 73. STOP ends measurements, not accounting for the
    // datagrams the runtime already received in that batch.
    #[test]
    fn a_first_stop_and_its_batch_tail_are_charged_without_measurements() {
        let mut phase = receiver_phase(Duration::from_secs(10));
        let datagrams = [(2, 1400), (0, 800)].map(|(test_action, actual_len)| {
            let mut datagram = RxDatagram {
                header: [0; LOAD_HEADER_LEN],
                actual_len,
                kernel_wall: None,
                tos: None,
            };
            LoadHeader {
                seq_no: 1,
                test_action,
                ..LoadHeader::default()
            }
            .encode_into(&mut datagram.header);
            datagram
        });
        phase.handle_load_batch(RxBatch {
            before: now(1),
            after: now(1),
            datagrams: &datagrams,
        });
        // A socket read may split its datagrams into several core batches,
        // including the one-datagram portable fallback after STOP.
        phase.handle_load_batch(RxBatch {
            before: now(1),
            after: now(1),
            datagrams: &datagrams[1..],
        });
        let events: Vec<_> = std::iter::from_fn(|| phase.poll_event()).collect();
        assert_eq!(
            events
                .iter()
                .filter_map(|event| match event {
                    DataEvent::LoadBytes { bytes } => Some(bytes),
                    _ => None,
                })
                .sum::<u64>(),
            3000
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, DataEvent::SubIntervalMeasured(_)))
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, DataEvent::Ended(_)))
        );
    }

    fn status_at(phase: &mut DataPhase, ms: u64) -> StatusPdu {
        let mut buf = [0; STATUS_PDU_LEN];
        let len = phase.poll_transmit(now(ms), &mut buf).unwrap();
        StatusPdu::decode(&buf[..len]).unwrap()
    }

    fn estimate(phase: &mut DataPhase) -> &mut SentEstimate {
        let Role::LoadReceiver(receiver) = &mut phase.role else {
            panic!("a Load Receiver");
        };
        &mut receiver.sent
    }

    // ADR 0008: one Load PDU must return without walking an arbitrary
    // timestamp gap, so other Tests and deadlines can still be served.
    // The work limit makes the unfixed path fail after 202 closures,
    // rather than waiting for the peer's billions of empty st.
    #[udpstp_req::req("RFC9946-8.2-1")]
    #[test]
    fn sender_estimate_work_is_bounded_for_large_forward_timestamps() {
        for timestamp in [(1000, 0), (214_748_365, 750_000_000)] {
            let mut phase = receiver_phase(Duration::from_secs(10));
            estimate(&mut phase).close_limit = Some(201);
            receive_load(&mut phase, 0, 1, (1, 0), 100);
            receive_load(&mut phase, 1, 2, timestamp, 100);
            assert!(estimate(&mut phase).closed_intervals <= 201);
            let events: Vec<_> = std::iter::from_fn(|| phase.poll_event()).collect();
            let estimates: Vec<_> = events
                .iter()
                .filter_map(|e| match e {
                    DataEvent::SentEstimated(s) => Some(s),
                    _ => None,
                })
                .collect();
            assert_eq!(estimates.len(), 200);
            assert_eq!((estimates[0].datagrams, estimates[0].bytes), (1, 100));
            assert!(
                estimates[1..]
                    .iter()
                    .all(|s| s.datagrams == 0 && s.bytes == 0)
            );
            assert_eq!(estimates.last().unwrap().st_seq_no, 200);

            // A genuine timestamp after the jump is earlier on the peer's
            // grid, but its Load PDU still counts and resets the Watchdog.
            receive_load(&mut phase, 2, 3, (1, 60_000_000), 100);
            assert_eq!(phase.poll_timeout(), Some(at(50)));
            let status = status_at(&mut phase, 50);
            assert_eq!((status.ti_rx_datagrams, status.ti_rx_bytes), (3, 300));
            assert_eq!(
                (status.seq_err_loss, status.seq_err_ooo, status.seq_err_dup),
                (0, 0, 0)
            );
            phase.handle_timeout(now(1002));
            assert!(phase.silenced);
            assert_eq!(phase.poll_timeout(), Some(at(3002)));
            phase.handle_timeout(now(3002));
            assert!(
                std::iter::from_fn(|| phase.poll_event())
                    .any(|e| matches!(e, DataEvent::Ended(EndReason::Watchdog)))
            );
        }
    }

    // ADR 0008: once reporting ends, the first closure must still save
    // the open st's sequence boundary before empty intervals are skipped.
    #[test]
    fn sender_estimate_preserves_the_first_closure_beyond_the_horizon() {
        let mut phase = receiver_phase(Duration::from_secs(10));
        receive_load(&mut phase, 0, 1, (1, 0), 100);
        // Exactly at the reporting horizon: index 200 remains open.
        receive_load(&mut phase, 1, 2, (11, 0), 200);
        assert_eq!(estimate(&mut phase).closed_intervals, 200);
        let sent = estimate(&mut phase);
        sent.closed_intervals = 0;
        sent.close_limit = Some(1);
        receive_load(&mut phase, 2, 3, (11, 150_000_000), 100);
        let sent = estimate(&mut phase);
        assert_eq!(sent.closed_intervals, 1);
        assert_eq!(
            (sent.index, sent.before, sent.highest),
            (203, Some(2), Some(3))
        );
        assert_eq!((sent.received, sent.bytes), (1, 100));

        sent.closed_intervals = 0;
        receive_load(&mut phase, 3, 4, (214_748_365, 750_000_000), 150);
        let sent = estimate(&mut phase);
        assert_eq!(sent.closed_intervals, 1);
        assert_eq!(
            (sent.index, sent.before, sent.highest),
            (u32::MAX, Some(3), Some(4))
        );
        assert_eq!((sent.received, sent.bytes), (1, 150));
        let estimates: Vec<_> = std::iter::from_fn(|| phase.poll_event())
            .filter_map(|e| match e {
                DataEvent::SentEstimated(s) => Some(s),
                _ => None,
            })
            .collect();
        assert_eq!(estimates.len(), 200);
        assert_eq!(estimates.last().unwrap().st_seq_no, 200);
    }

    // RFC 9097 §7.4; Sequence Errors are independent of the sender
    // estimate's clock grid. Expected counts and bytes worked by hand.
    #[udpstp_req::req("RFC9097-7.4-1", "RFC9946-8.2-L4", "RFC9946-8.2-L9")]
    #[test]
    fn sender_estimate_keeps_ordinary_gaps_duplicates_and_out_of_order_load() {
        let mut phase = receiver_phase(Duration::from_secs(10));
        for (ms, (seq, timestamp, bytes)) in [
            (1, (1, 0), 100),
            (3, (1, 10_000_000), 200),
            (2, (1, 20_000_000), 150),
            (3, (1, 30_000_000), 200),
            (1, (0, 999_000_000), 100), // earlier than the grid's origin
            (5, (1, 160_000_000), 200),
            (4, (1, 170_000_000), 100),
            (6, (1, 200_000_000), 100),
        ]
        .into_iter()
        .enumerate()
        {
            receive_load(&mut phase, ms as u64, seq, timestamp, bytes);
        }
        let got: Vec<_> = std::iter::from_fn(|| phase.poll_event())
            .filter_map(|e| match e {
                DataEvent::SentEstimated(s) => Some((s.st_seq_no, s.datagrams, s.bytes)),
                _ => None,
            })
            .collect();
        // st 1: span 1..3, four arrivals totalling 650 bytes, so the
        // estimate is 3 * 650 / 4 = 487. The next two st are empty.
        // st 4: span 4..5, two arrivals totalling 300 bytes.
        assert_eq!(got, [(1, 3, 487), (2, 0, 0), (3, 0, 0), (4, 2, 300)]);
        let status = status_at(&mut phase, 50);
        assert_eq!((status.ti_rx_datagrams, status.ti_rx_bytes), (8, 1150));
        assert_eq!(
            (status.seq_err_loss, status.seq_err_ooo, status.seq_err_dup),
            (0, 2, 2)
        );
    }

    // ADR 0008: negative or unrepresentable indices already do no work;
    // keep that behavior, including timestamp fields outside normal form.
    #[test]
    fn sender_estimate_ignores_unrepresentable_timestamps_and_keeps_receiving() {
        let mut phase = receiver_phase(Duration::from_secs(10));
        estimate(&mut phase).close_limit = Some(1);
        receive_load(&mut phase, 0, 1, (1, 0), 100);
        receive_load(&mut phase, 1, 2, (u32::MAX, u32::MAX), 100);
        assert_eq!(estimate(&mut phase).closed_intervals, 0);
        receive_load(&mut phase, 2, 3, (1, 60_000_000), 100);
        assert_eq!(estimate(&mut phase).closed_intervals, 1);
        let events: Vec<_> = std::iter::from_fn(|| phase.poll_event()).collect();
        assert!(events.iter().any(|e| matches!(e,
            DataEvent::SentEstimated(s) if s.st_seq_no == 1 && s.datagrams == 1 && s.bytes == 100
        )));
        let status = status_at(&mut phase, 50);
        assert_eq!((status.ti_rx_datagrams, status.seq_err_loss), (3, 0));
    }

    // RFC 9946 §9 (a Client's own Test Interval timer takes precedence
    // over a STOP2 that doesn't come); the architecture review of
    // 2026-09-30 (the rule stated once): a Data Phase that ended with the
    // STOP exchange, or by the Client's own Test Interval timer, completed
    // its Test Connection, and every other end is early
    #[test]
    fn only_the_stop_exchange_and_the_test_interval_complete() {
        use EndReason::{
            Completed, OverRate, Stopped, StoppedByClient, TestIntervalElapsed, Unconfirmed,
            Watchdog,
        };
        for (reason, completes) in [
            (Completed, true),
            (TestIntervalElapsed, true),
            (Unconfirmed, false),
            (StoppedByClient, false),
            (Stopped, false),
            (Watchdog, false),
            (OverRate, false),
        ] {
            assert_eq!(reason.completes(), completes, "{reason:?}");
        }
    }

    // : a pause in sending shows as empty st, and no more are reported
    // than the Test Interval holds
    #[test]
    fn the_sent_grid_reports_empty_st_and_stops_at_the_test_interval() {
        let mut grid = SentGrid::new(Duration::from_millis(50), Duration::from_millis(200), true);
        let mut out = VecDeque::new();
        grid.add(at(1000), 10, 10_000, &mut out);
        grid.add(at(1010), 10, 10_000, &mut out);
        // Nothing sent from 1050 to 1150.
        grid.add(at(1160), 5, 5_000, &mut out);
        grid.add(at(1400), 1, 1_000, &mut out);
        let got: Vec<(u32, u32, u64)> = out
            .iter()
            .map(|s| (s.st_seq_no, s.datagrams, s.bytes))
            .collect();
        assert_eq!(got, [(1, 20, 20_000), (2, 0, 0), (3, 0, 0), (4, 5, 5_000)]);
        assert_eq!(out[0].ip_layer_sender_bit_rate, (20_000 + 20 * 28) * 8 * 20);
    }

    //
    #[udpstp_req::req("RFC9946-5.3.2-3")]
    #[test]
    fn a_status_pdu_must_pass_every_mode_2_check() {
        let auth = StatusAuth {
            key_id: 7,
            own: [1; 32],
            peer: [2; 32],
        };
        let wall = WallTime::from_unix(Duration::from_secs(1_790_000_000));
        let now = Now { mono: at(0), wall };
        let signed = |edit: &dyn Fn(&mut StatusPdu), key: &[u8; 32]| {
            let mut pdu = LoadReceiver::new(Duration::from_millis(50), Duration::from_secs(1))
                .status(at(0), (0, 0));
            sign_status(&mut pdu, now, 7, key);
            edit(&mut pdu);
            pdu.sign(key);
            pdu.encode()
        };
        let authentic = |bytes: &[u8]| {
            let status = StatusPdu::decode(bytes).expect("a Status PDU");
            status_authentic(&status, bytes, &auth, now)
        };
        let good = signed(&|_| {}, &auth.peer);
        assert!(authentic(&good));
        // Signed with our own key, as a reflected Status PDU would be.
        assert!(!authentic(&signed(&|_| {}, &auth.own)));
        // Another keyId, another authMode.
        assert!(!authentic(&signed(&|p| p.key_id = 8, &auth.peer)));
        assert!(!authentic(&signed(&|p| p.auth_mode = 1, &auth.peer)));
        // Out of the time window, either way.
        let time = crate::protocol::keys::auth_unix_time(wall);
        for off in [time - 6, time + 6] {
            let pdu = signed(&|p| p.auth_unix_time = off, &auth.peer);
            assert!(!authentic(&pdu), "{off}");
        }
        // A changed byte breaks the digest.
        let mut changed = good;
        changed[20] ^= 1;
        assert!(!authentic(&changed));
    }

    // : a Load Sender refuses a size its table variant doesn't allow
    #[udpstp_req::req("RFC9946-6.1-9", "RFC9946-6.1-11")]
    #[test]
    fn a_size_beyond_the_table_variant_is_refused() {
        let rate = SendingRate {
            tx_interval1: 100,
            udp_payload1: 1472,
            burst_size1: 2,
            tx_interval2: 1000,
            udp_addon2: RANDOM_SIZE | 1222,
            ..SendingRate::default()
        };
        let plain = sanitize(&rate, 1222);
        assert_eq!(
            (plain.udp_payload1, plain.udp_addon2),
            (0, RANDOM_SIZE | 1222)
        );
        let traditional = sanitize(&rate, 1472);
        assert_eq!(traditional.udp_payload1, 1472);
    }

    // : a Status PDU's kernel receive time, converted with the clock
    // pair read with it, is kept between the receive timeout before the
    // read and the read; without one, the read time.
    #[test]
    fn status_pdu_receive_times_are_clamped() {
        let wall = |ms: u64| WallTime::from_unix(Duration::from_millis(1_790_000_000_000 + ms));
        let now = Now {
            mono: at(5_000),
            wall: wall(5_000),
        };
        let received = |kernel_wall| {
            status_received_at(&RxPdu {
                now,
                kernel_wall,
                pdu: &[],
            })
        };
        assert_eq!(received(None), at(5_000));
        assert_eq!(received(Some(wall(4_997))), at(4_997), "waited 3 ms");
        assert_eq!(received(Some(wall(5_003))), at(5_000), "after the read");
        assert_eq!(received(Some(wall(1_000))), at(4_000), "a wall step");
    }

    // ADR 0006 (EMSGSIZE is a Path MTU event, time exceeded a
    // hop-limit event, other ICMP errors are counted);
    #[test]
    fn transport_errors_are_counted_and_reported_by_kind() {
        let mut pending = PendingEvents::default();
        for error in [
            TransportError::MsgSize,
            TransportError::MsgSize,
            TransportError::TimeExceeded,
            TransportError::Icmp,
        ] {
            pending.transport_error(error);
        }
        let mut events = Vec::new();
        while let Some(event) = pending.take() {
            events.push(event);
        }
        assert_eq!(
            events,
            [
                DataEvent::PathMtuExceeded { count: 2 },
                DataEvent::HopLimitExceeded { count: 1 },
                DataEvent::IcmpErrors { count: 1 },
            ]
        );
    }
}
