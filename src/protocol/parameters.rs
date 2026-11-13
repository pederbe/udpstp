//! The Activation parameters' ranges and defaults (RFC 9946 §7.1, §7.2.1;
//! docs/spec.md stories 6 and 67).
//!
//! The Server clamps each requested value into its range: out-of-range
//! values go to the nearest limit, and the Activation Response reports the
//! coercion (§7.2.1). The Client asks for the defaults and ends a Test whose
//! response it can't run: a value outside the range, or thresholds out of
//! order (§7.3).
//!
//! Sources, most specific first:
//! - Y.1540 Amd 2 Tables B.1 and B.2, the definition of Algorithm B;
//! - RFC 9946 §7.1 for the defaults it states;
//! - TR-471 Tables 1 and 3, where they agree or are advisory.
//!

use crate::wire::ActivationPdu;
use crate::wire::activation::rate_adj_algo;

use crate::protocol::ActivationParameter;

/// A parameter's default and its inclusive range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Range {
    pub default: u16,
    pub min: u16,
    pub max: u16,
}

impl Range {
    fn clamp(&self, value: u16) -> u16 {
        value.clamp(self.min, self.max)
    }

    fn contains(&self, value: u16) -> bool {
        (self.min..=self.max).contains(&value)
    }
}

/// lowThresh, ms: Y.1540 Amd 2 Table B.1, 5-250; default 30 (RFC 9946 §5.1
/// item 6).
pub(crate) const LOW_THRESH: Range = Range {
    default: 30,
    min: 5,
    max: 250,
};

/// upperThresh, ms: Y.1540 Amd 2 Table B.1, 5-250; default 90.
pub(crate) const UPPER_THRESH: Range = Range {
    default: 90,
    min: 5,
    max: 250,
};

/// trialInt, ms: Y.1540 Amd 2 Table B.2 StatusFeedbackInterval, 5-250
/// (TR-471 Table 1 advises 20-250); default 50.
pub(crate) const TRIAL_INT: Range = Range {
    default: 50,
    min: 5,
    max: 250,
};

/// testIntTime, s: Y.1540 Amd 2 Table B.2 and TR-471 Table 1 t, 5-60;
/// default 10 (RFC 9946 §5.1 item 4: at most 10). An Operator Key may
/// allow more ([`Allowance`]).
pub(crate) const TEST_INT_TIME: Range = Range {
    default: 10,
    min: 5,
    max: 60,
};

/// subIntPeriod, ms: Y.1540 Amd 2 Table B.2 dt, 0.1-10 s (TR-471 advises
/// 100-6000 ms); default 1000 (RFC 9946 §7.1). Also at most the Test
/// Interval.
pub(crate) const SUB_INT_PERIOD: Range = Range {
    default: 1000,
    min: 100,
    max: 10_000,
};

/// useOwDelVar, Boolean: default True as RFC 9946 §7.1 says, although
/// TR-471's EnableIPDV and OB-UDPST default to False (RTT)
/// (conformance: use-ow-del-var-defaults-to-true).
pub(crate) const USE_OW_DEL_VAR: Range = Range {
    default: 1,
    min: 0,
    max: 1,
};

/// highSpeedDelta, rows: Y.1540 Amd 2 Table B.1, at least 2; default 10.
pub(crate) const HIGH_SPEED_DELTA: Range = Range {
    default: 10,
    min: 2,
    max: u8::MAX as u16,
};

/// slowAdjThresh: Y.1540 Amd 2 Table B.1, more than 1; default 3.
pub(crate) const SLOW_ADJ_THRESH: Range = Range {
    default: 3,
    min: 2,
    max: u16::MAX,
};

/// seqErrThresh: Y.1540 Amd 2 Table B.1, 0-100; default 10 (RFC 9946 §5.1
/// item 6).
pub(crate) const SEQ_ERR_THRESH: Range = Range {
    default: 10,
    min: 0,
    max: 100,
};

/// ignoreOooDup, Boolean: default True (RFC 9946 §7.1).
pub(crate) const IGNORE_OOO_DUP: Range = Range {
    default: 1,
    min: 0,
    max: 1,
};

/// The ranged parameters, in the PDU's field order. An out-of-range value
/// is clamped to the nearest limit; the number of Sub-Intervals isn't
/// bounded (conformance: parameter-ranges-follow-y1540, out-of-range-parameters-clamped).
/// dscpEcn is the key's DSCPs, never ECN-capable (below). srIndexConf is
/// the Server's table's, and rateAdjAlgo is refused unless it is B or
/// C.
const RANGED: [(ActivationParameter, Range); 10] = [
    (ActivationParameter::LowThresh, LOW_THRESH),
    (ActivationParameter::UpperThresh, UPPER_THRESH),
    (ActivationParameter::TrialInt, TRIAL_INT),
    (ActivationParameter::TestIntTime, TEST_INT_TIME),
    (ActivationParameter::UseOwDelVar, USE_OW_DEL_VAR),
    (ActivationParameter::HighSpeedDelta, HIGH_SPEED_DELTA),
    (ActivationParameter::SlowAdjThresh, SLOW_ADJ_THRESH),
    (ActivationParameter::SeqErrThresh, SEQ_ERR_THRESH),
    (ActivationParameter::IgnoreOooDup, IGNORE_OOO_DUP),
    (ActivationParameter::SubIntPeriod, SUB_INT_PERIOD),
];

/// The most time a Test may take to infer congestion,
/// slowAdjThresh × trialInt, in ms (RFC 9946 §5.1 item 5).
const CONGESTION_INFERENCE_MS: u16 = 500;

/// A set of DSCP values (RFC 2474), 0 to 63.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DscpSet(u64);

impl DscpSet {
    /// Every DSCP.
    #[cfg(any(test, feature = "fuzzing"))]
    pub const ALL: Self = Self(u64::MAX);
    /// Only 0, the default Per-Hop Behavior (best effort).
    pub const DEFAULT_ONLY: Self = Self(1);
    /// Every DSCP but CS6 and CS7 (48 and 56), which networks keep for
    /// their own control traffic (RFC 4594 §3.1, §3.2).
    pub const ALL_BUT_NETWORK_CONTROL: Self = Self(!(1 << 48 | 1 << 56));

    /// The set of `values`; `None` if one is above 63.
    pub fn of(values: impl IntoIterator<Item = u8>) -> Option<Self> {
        values.into_iter().try_fold(Self(0), |set, dscp| {
            (dscp < 64).then(|| Self(set.0 | 1 << dscp))
        })
    }

    pub fn contains(self, dscp: u8) -> bool {
        dscp < 64 && self.0 & 1 << dscp != 0
    }
}

/// The ECN field's bits in dscpEcn.
const ECN_MASK: u8 = 0b11;

/// What a Test Connection's key allows beyond the ordinary ranges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Allowance {
    /// The longest Test Interval, in s: 60, or an Operator Key's own.
    pub max_test_interval: u16,
    /// An Operator Key: exempt from the 500 ms congestion inference.
    pub operator: bool,
    /// The DSCPs the Test may mark its traffic with.
    pub dscp: DscpSet,
}

impl Allowance {
    /// An ordinary Shared Key, or no key, on a Server that allows only the
    /// default DSCP.
    pub(crate) const CONSUMER: Self = Self {
        max_test_interval: TEST_INT_TIME.max,
        operator: false,
        dscp: DscpSet::DEFAULT_ONLY,
    };
}

/// The request with every ranged parameter clamped to the nearest limit;
/// LowThresh above UpperThresh becomes UpperThresh, and a Sub-Interval
/// longer than the Test Interval becomes the Test Interval. Then what the
/// key allows (RFC 9946 §5.1 items 4 and 5):
/// - the Test Interval up to 60 s, or up to an Operator Key's own maximum;
/// - above 60 s, RTT variation in place of one-way delay variation, which
///   clock drift would distort;
/// - slowAdjThresh lowered until slowAdjThresh × trialInt is at most
///   500 ms, except for an Operator Key;
/// - dscpEcn's DSCP, or 0 if the key doesn't allow it, and its ECN field
///   always Not-ECT: UDPSTP isn't an ECN-capable transport (RFC 9946
///   §7.1).
///
/// The 500 ms rule lowers slowAdjThresh, not trialInt
/// (conformance: test-interval-over-60-s, congestion-inference-within-500-ms).
pub(crate) fn clamp(request: &ActivationPdu, allowance: Allowance) -> ActivationPdu {
    let mut pdu = request.clone();
    for (parameter, range) in RANGED {
        let value = parameter.value(&pdu);
        parameter.set(&mut pdu, range.clamp(value));
    }
    pdu.test_int_time = request.test_int_time.clamp(
        TEST_INT_TIME.min,
        allowance.max_test_interval.max(TEST_INT_TIME.max),
    );
    if pdu.test_int_time > TEST_INT_TIME.max {
        pdu.use_ow_del_var = 0;
    }
    // Only the Server can apply this: a Client doesn't know its key's role.
    // With trialInt at most 250 ms, the floor of 2 keeps it within 500 ms.
    if !allowance.operator {
        let most = (CONGESTION_INFERENCE_MS / pdu.trial_int).max(SLOW_ADJ_THRESH.min);
        pdu.slow_adj_thresh = pdu.slow_adj_thresh.min(most);
    }
    // A DSCP from the key's set, and Not-ECT
    // (conformance: dscp-from-an-allowed-set-never-ect).
    let dscp = pdu.dscp_ecn >> 2;
    pdu.dscp_ecn = if allowance.dscp.contains(dscp) {
        pdu.dscp_ecn & !ECN_MASK
    } else {
        0
    };
    // Thresholds in order (conformance: low-thresh-above-upper-lowered).
    pdu.low_thresh = pdu.low_thresh.min(pdu.upper_thresh);
    // Within the Test Interval (conformance: sub-interval-shortened-to-test-interval).
    let test_ms = pdu.test_int_time.saturating_mul(1000);
    pdu.sub_int_period = pdu.sub_int_period.min(test_ms);
    // A Sub-Interval closes at a Status PDU, one a Trial Interval, so it
    // runs a whole number of Trial Intervals: subIntPeriod becomes the
    // nearest such number, within its range and the Test Interval, and
    // every Sub-Interval then runs its nominal length (RFC 9097 §5.3: all
    // of equal duration) (conformance: sub-interval-whole-trial-intervals).
    let trial = pdu.trial_int.max(1);
    let most = SUB_INT_PERIOD.max.min(test_ms) / trial;
    let trials = ((pdu.sub_int_period + trial / 2) / trial).clamp(1, most.max(1));
    // And a whole number of them makes the Test Interval (RFC 9097 §5.3,
    // §6.3: I = m * dt): the nearest such subIntPeriod, the shorter on a
    // tie. When none does, the Trial Interval doesn't divide the Test
    // Interval, and the nearest whole number of Trial Intervals stays.
    // In u32: an Operator's Test Interval can be longer than u16 ms.
    let test_ms_whole = u32::from(pdu.test_int_time) * 1000;
    let divides = |k: u16| test_ms_whole % (u32::from(k) * u32::from(trial)) == 0;
    let nearest = (1..=most.max(1))
        .filter(|&k| divides(k))
        .min_by_key(|&k| (k.abs_diff(trials), k));
    pdu.sub_int_period = nearest.unwrap_or(trials) * trial;
    pdu
}

/// The first parameter of an Activation Response the Client can't run
/// with: outside its range, thresholds out of order, a Sub-Interval longer
/// than the Test Interval, or an algorithm it doesn't know.
pub(crate) fn unrunnable(
    response: &ActivationPdu,
    request: &ActivationPdu,
) -> Option<ActivationParameter> {
    // An Operator Key may allow a Test Interval above the range's 60 s, and
    // then only as long as asked: a Server can't make a Client run longer
    // than 60 s unless it asked to.
    let longest = request.test_int_time.max(TEST_INT_TIME.max);
    let outside = RANGED
        .iter()
        .find(|&&(parameter, range)| match parameter {
            ActivationParameter::TestIntTime => {
                !(range.min..=longest).contains(&response.test_int_time)
            }
            _ => !range.contains(parameter.value(response)),
        })
        .map(|&(parameter, _)| parameter);
    if outside.is_some() {
        return outside;
    }
    if response.low_thresh > response.upper_thresh {
        return Some(ActivationParameter::LowThresh);
    }
    if u32::from(response.sub_int_period) > u32::from(response.test_int_time) * 1000 {
        return Some(ActivationParameter::SubIntPeriod);
    }
    if !matches!(response.rate_adj_algo, rate_adj_algo::B | rate_adj_algo::C) {
        return Some(ActivationParameter::RateAdjAlgo);
    }
    // A Server may lower the marking to 0, not change it to another DSCP:
    // the Client's traffic goes only into the class it asked for, or best
    // effort. ECN bits in the answer are ignored; the Client sends Not-ECT
    // (conformance: client-refuses-a-changed-dscp).
    let dscp = response.dscp_ecn >> 2;
    if dscp != 0 && dscp != request.dscp_ecn >> 2 {
        return Some(ActivationParameter::DscpEcn);
    }
    None
}
