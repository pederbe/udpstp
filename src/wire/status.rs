//! Status PDU (RFC 9946 §8.2, Figures 10 and 11): the Load Receiver's
//! reception statistics, with the packed Sub-Interval statistics block.

use crate::wire::checksum::internet_checksum;
use crate::wire::{AUTH_DIGEST_LEN, DecodeError, SendingRate, auth, check_fixed};

/// pduId of the Status PDU.
pub const STATUS_PDU_ID: u16 = 0xFEED;

/// Size of the Status PDU in bytes.
pub const STATUS_PDU_LEN: usize = 204;

/// "No delay data/value" in the delay and RTT fields (STATUS_NODEL).
pub const STATUS_NODEL: u32 = u32::MAX;

/// Where the checkSum field sits.
const CHECKSUM: std::ops::Range<usize> = 202..204;

/// Sub-Interval statistics (`struct subIntStats`, 56 bytes, packed).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SubIntervalStats {
    pub rx_datagrams: u32,
    pub rx_bytes: u64,
    /// The Sub-Interval's measured duration, in µs.
    pub delta_time: u32,
    pub seq_err_loss: u32,
    pub seq_err_ooo: u32,
    pub seq_err_dup: u32,
    /// Delay variation, in ms.
    pub delay_var_min: u32,
    pub delay_var_max: u32,
    pub delay_var_sum: u32,
    pub delay_var_cnt: u32,
    /// RTT variation, in ms.
    pub rtt_var_minimum: u32,
    pub rtt_var_maximum: u32,
    /// Time of the Test so far, in ms.
    pub accum_time: u32,
}

impl SubIntervalStats {
    fn write(&self, b: &mut [u8]) {
        b[0..4].copy_from_slice(&self.rx_datagrams.to_be_bytes());
        b[4..12].copy_from_slice(&self.rx_bytes.to_be_bytes());
        let rest = [
            self.delta_time,
            self.seq_err_loss,
            self.seq_err_ooo,
            self.seq_err_dup,
            self.delay_var_min,
            self.delay_var_max,
            self.delay_var_sum,
            self.delay_var_cnt,
            self.rtt_var_minimum,
            self.rtt_var_maximum,
            self.accum_time,
        ];
        for (chunk, value) in b[12..56].as_chunks_mut::<4>().0.iter_mut().zip(rest) {
            *chunk = value.to_be_bytes();
        }
    }

    fn read(b: &[u8]) -> Self {
        let at = |i: usize| u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
        let mut rx_bytes = [0u8; 8];
        rx_bytes.copy_from_slice(&b[4..12]);
        Self {
            rx_datagrams: at(0),
            rx_bytes: u64::from_be_bytes(rx_bytes),
            delta_time: at(12),
            seq_err_loss: at(16),
            seq_err_ooo: at(20),
            seq_err_dup: at(24),
            delay_var_min: at(28),
            delay_var_max: at(32),
            delay_var_sum: at(36),
            delay_var_cnt: at(40),
            rtt_var_minimum: at(44),
            rtt_var_maximum: at(48),
            accum_time: at(52),
        }
    }
}

/// A Status PDU. The reserved fields are not represented (see the crate
/// docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusPdu {
    pub test_action: u8,
    pub rx_stopped: u8,
    /// Starts at 1.
    pub spdu_seq_no: u32,
    pub sending_rate: SendingRate,
    /// The Sub-Interval `sis_sav` covers; 0 before any has completed.
    pub sub_int_seq_no: u32,
    pub sis_sav: SubIntervalStats,
    pub seq_err_loss: u32,
    pub seq_err_ooo: u32,
    pub seq_err_dup: u32,
    /// Two's complement, in ms.
    pub clock_delta_min: u32,
    pub delay_var_min: u32,
    pub delay_var_max: u32,
    pub delay_var_sum: u32,
    pub delay_var_cnt: u32,
    pub rtt_minimum: u32,
    pub rtt_var_sample: u32,
    pub delay_min_upd: u8,
    /// The Trial Interval's duration, in µs.
    pub ti_delta_time: u32,
    pub ti_rx_datagrams: u32,
    pub ti_rx_bytes: u32,
    /// spduTime_sec and spduTime_nsec: this Status PDU's send time.
    pub spdu_time: (u32, u32),
    pub auth_mode: u8,
    pub auth_unix_time: u32,
    pub auth_digest: [u8; AUTH_DIGEST_LEN],
    pub key_id: u8,
    pub checksum: u16,
}

impl StatusPdu {
    /// Sets authDigest to the HMAC of the whole PDU with authDigest and
    /// checkSum zeroed (RFC 9946 §5.3.2, §5.3.1). A checksum, if used, must
    /// be computed after this.
    pub fn sign(&mut self, key: &[u8; 32]) {
        let mut input = Self {
            auth_digest: [0; AUTH_DIGEST_LEN],
            checksum: 0,
            ..self.clone()
        }
        .encode();
        input[CHECKSUM].fill(0);
        self.auth_digest = auth::auth_digest(key, &input);
    }

    /// Whether a received Status PDU's authDigest is the HMAC of its bytes
    /// under `key` (constant time), reserved fields included.
    pub fn verify_received(bytes: &[u8], key: &[u8; 32]) -> bool {
        bytes.len() == STATUS_PDU_LEN && auth::verify_received(key, bytes, 168..200, CHECKSUM)
    }

    pub fn encode(&self) -> [u8; STATUS_PDU_LEN] {
        let mut b = [0u8; STATUS_PDU_LEN];
        let mut put = |at: usize, v: u32| b[at..at + 4].copy_from_slice(&v.to_be_bytes());
        put(4, self.spdu_seq_no);
        put(36, self.sub_int_seq_no);
        for (i, v) in [
            self.seq_err_loss,
            self.seq_err_ooo,
            self.seq_err_dup,
            self.clock_delta_min,
            self.delay_var_min,
            self.delay_var_max,
            self.delay_var_sum,
            self.delay_var_cnt,
            self.rtt_minimum,
            self.rtt_var_sample,
        ]
        .into_iter()
        .enumerate()
        {
            put(96 + 4 * i, v);
        }
        for (i, v) in [
            self.ti_delta_time,
            self.ti_rx_datagrams,
            self.ti_rx_bytes,
            self.spdu_time.0,
            self.spdu_time.1,
        ]
        .into_iter()
        .enumerate()
        {
            put(140 + 4 * i, v);
        }
        put(164, self.auth_unix_time);
        b[0..2].copy_from_slice(&STATUS_PDU_ID.to_be_bytes());
        b[2] = self.test_action;
        b[3] = self.rx_stopped;
        self.sending_rate.write(&mut b[8..36]);
        self.sis_sav.write(&mut b[40..96]);
        b[136] = self.delay_min_upd;
        // b[137..140] are reserved1 and reserved2, b[160..163] reserved3
        // and reserved4.
        b[163] = self.auth_mode;
        b[168..200].copy_from_slice(&self.auth_digest);
        b[200] = self.key_id;
        // b[201] is reservedAuth1.
        b[CHECKSUM].copy_from_slice(&self.checksum.to_be_bytes());
        b
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        check_fixed(bytes, STATUS_PDU_LEN, STATUS_PDU_ID)?;
        let at =
            |i: usize| u32::from_be_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
        let mut auth_digest = [0u8; AUTH_DIGEST_LEN];
        auth_digest.copy_from_slice(&bytes[168..200]);
        Ok(Self {
            test_action: bytes[2],
            rx_stopped: bytes[3],
            spdu_seq_no: at(4),
            sending_rate: SendingRate::read(&bytes[8..36]),
            sub_int_seq_no: at(36),
            sis_sav: SubIntervalStats::read(&bytes[40..96]),
            seq_err_loss: at(96),
            seq_err_ooo: at(100),
            seq_err_dup: at(104),
            clock_delta_min: at(108),
            delay_var_min: at(112),
            delay_var_max: at(116),
            delay_var_sum: at(120),
            delay_var_cnt: at(124),
            rtt_minimum: at(128),
            rtt_var_sample: at(132),
            delay_min_upd: bytes[136],
            ti_delta_time: at(140),
            ti_rx_datagrams: at(144),
            ti_rx_bytes: at(148),
            spdu_time: (at(152), at(156)),
            auth_mode: bytes[163],
            auth_unix_time: at(164),
            auth_digest,
            key_id: bytes[200],
            checksum: u16::from_be_bytes([bytes[202], bytes[203]]),
        })
    }

    /// Fills the checkSum field: the Internet checksum of the whole PDU,
    /// computed with the field zeroed (RFC 9946 §5.6). A result of 0x0000
    /// is sent as 0xFFFF, as the Load PDU does.
    pub fn fill_checksum(pdu: &mut [u8; STATUS_PDU_LEN]) {
        pdu[CHECKSUM].fill(0);
        let sum = match internet_checksum(pdu) {
            0 => 0xFFFF,
            sum => sum,
        };
        pdu[CHECKSUM].copy_from_slice(&sum.to_be_bytes());
    }

    /// Whether the checkSum field matches the PDU.
    pub fn checksum_valid(pdu: &[u8]) -> bool {
        pdu.len() == STATUS_PDU_LEN && internet_checksum(pdu) == 0
    }
}
