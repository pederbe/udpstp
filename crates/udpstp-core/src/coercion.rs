//! Coercion of Activation parameters (RFC 9946 §7.2.1, §7.3).
//!
//! Both ends find the coercions the same way, by comparing the Activation
//! Request with its response, so they always agree on exactly what changed.

use udpstp_wire::ActivationPdu;
use udpstp_wire::activation::modifier;

/// An Activation Request parameter the Server may coerce: the ones RFC 9946
/// §7.2.1 lists, plus srIndexConf and the modifierBitmap bits that change
/// the Test (§7.2 lets the Server modify any test parameter). In the order
/// of the PDU's fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActivationParameter {
    LowThresh,
    UpperThresh,
    TrialInt,
    TestIntTime,
    DscpEcn,
    SrIndexConf,
    UseOwDelVar,
    HighSpeedDelta,
    SlowAdjThresh,
    SeqErrThresh,
    IgnoreOooDup,
    /// CHTA_SRIDX_ISSTART: 1 when srIndexConf is a starting row, 0 for a
    /// Fixed-Rate Test.
    SrIndexIsStart,
    /// CHTA_RAND_PAYLOAD: 1 for random payload content.
    RandomPayload,
    RateAdjAlgo,
    SubIntPeriod,
}

impl ActivationParameter {
    pub const ALL: [Self; 15] = [
        Self::LowThresh,
        Self::UpperThresh,
        Self::TrialInt,
        Self::TestIntTime,
        Self::DscpEcn,
        Self::SrIndexConf,
        Self::UseOwDelVar,
        Self::HighSpeedDelta,
        Self::SlowAdjThresh,
        Self::SeqErrThresh,
        Self::IgnoreOooDup,
        Self::SrIndexIsStart,
        Self::RandomPayload,
        Self::RateAdjAlgo,
        Self::SubIntPeriod,
    ];

    pub(crate) fn value(self, pdu: &ActivationPdu) -> u16 {
        match self {
            Self::LowThresh => pdu.low_thresh,
            Self::UpperThresh => pdu.upper_thresh,
            Self::TrialInt => pdu.trial_int,
            Self::TestIntTime => pdu.test_int_time,
            Self::DscpEcn => pdu.dscp_ecn.into(),
            Self::SrIndexConf => pdu.sr_index_conf,
            Self::UseOwDelVar => pdu.use_ow_del_var.into(),
            Self::HighSpeedDelta => pdu.high_speed_delta.into(),
            Self::SlowAdjThresh => pdu.slow_adj_thresh,
            Self::SeqErrThresh => pdu.seq_err_thresh,
            Self::IgnoreOooDup => pdu.ignore_ooo_dup.into(),
            Self::SrIndexIsStart => bit(pdu, modifier::SR_INDEX_IS_START),
            Self::RandomPayload => bit(pdu, modifier::RANDOM_PAYLOAD),
            Self::RateAdjAlgo => pdu.rate_adj_algo.into(),
            Self::SubIntPeriod => pdu.sub_int_period,
        }
    }
}

impl ActivationParameter {
    /// Sets the parameter in `pdu`; a value too large for a one-octet field
    /// becomes 255.
    pub(crate) fn set(self, pdu: &mut ActivationPdu, value: u16) {
        let octet = u8::try_from(value).unwrap_or(u8::MAX);
        let set_bit = |pdu: &mut ActivationPdu, mask: u8| {
            if value != 0 {
                pdu.modifier_bitmap |= mask;
            } else {
                pdu.modifier_bitmap &= !mask;
            }
        };
        match self {
            Self::LowThresh => pdu.low_thresh = value,
            Self::UpperThresh => pdu.upper_thresh = value,
            Self::TrialInt => pdu.trial_int = value,
            Self::TestIntTime => pdu.test_int_time = value,
            Self::DscpEcn => pdu.dscp_ecn = octet,
            Self::SrIndexConf => pdu.sr_index_conf = value,
            Self::UseOwDelVar => pdu.use_ow_del_var = octet,
            Self::HighSpeedDelta => pdu.high_speed_delta = octet,
            Self::SlowAdjThresh => pdu.slow_adj_thresh = value,
            Self::SeqErrThresh => pdu.seq_err_thresh = value,
            Self::IgnoreOooDup => pdu.ignore_ooo_dup = octet,
            Self::SrIndexIsStart => set_bit(pdu, modifier::SR_INDEX_IS_START),
            Self::RandomPayload => set_bit(pdu, modifier::RANDOM_PAYLOAD),
            Self::RateAdjAlgo => pdu.rate_adj_algo = octet,
            Self::SubIntPeriod => pdu.sub_int_period = value,
        }
    }
}

fn bit(pdu: &ActivationPdu, mask: u8) -> u16 {
    u16::from(pdu.modifier_bitmap & mask != 0)
}

/// A parameter the Server changed: what the Client asked for, and what the
/// Test runs with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Coercion {
    pub parameter: ActivationParameter,
    pub requested: u16,
    pub coerced: u16,
}

/// Every parameter whose value differs between an Activation Request and
/// its response, in field order.
pub(crate) fn coercions<'a>(
    request: &'a ActivationPdu,
    response: &'a ActivationPdu,
) -> impl Iterator<Item = Coercion> + 'a {
    ActivationParameter::ALL
        .into_iter()
        .filter_map(|parameter| {
            let requested = parameter.value(request);
            let coerced = parameter.value(response);
            (requested != coerced).then_some(Coercion {
                parameter,
                requested,
                coerced,
            })
        })
}
