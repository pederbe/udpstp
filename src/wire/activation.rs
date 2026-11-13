//! Test Activation PDU: Activation Request and Activation Response
//! (RFC 9946 §7.1, Figures 6 and 7), and the Sending Rate Structure it
//! carries.

use crate::wire::{AUTH_DIGEST_LEN, DecodeError, auth, check_fixed};

/// pduId of the Test Activation PDU.
pub const ACTIVATION_PDU_ID: u16 = 0xACE2;

/// Size of the Test Activation PDU in bytes.
pub const ACTIVATION_PDU_LEN: usize = 104;

/// Size of the Sending Rate Structure in bytes.
pub const SENDING_RATE_LEN: usize = 28;

/// cmdRequest values (RFC 9946 §12.3.6).
pub mod cmd_request {
    /// Test activation, upstream (Client sends the load).
    pub const UPSTREAM: u8 = 1;
    /// Test activation, downstream (Server sends the load).
    pub const DOWNSTREAM: u8 = 2;
}

/// cmdResponse values (RFC 9946 §12.3.9).
pub mod cmd_response {
    pub const NONE: u8 = 0;
    pub const ACK_OK: u8 = 1;
    pub const BAD_PARAM: u8 = 2;
}

/// modifierBitmap bits (RFC 9946 §12.3.7).
pub mod modifier {
    /// srIndexConf is the starting row of a search, not a fixed rate.
    pub const SR_INDEX_IS_START: u8 = 0x01;
    /// Randomize the payload content beyond the Load PDU header.
    pub const RANDOM_PAYLOAD: u8 = 0x02;
}

/// rateAdjAlgo values (RFC 9946 §12.3.8).
pub mod rate_adj_algo {
    pub const B: u8 = 0;
    pub const C: u8 = 1;
}

/// srIndexConf value requesting the Server's default search (CHTA_SRIDX_DEF).
pub const SR_INDEX_DEFAULT: u16 = u16::MAX;

/// The Sending Rate Structure: one Sending Rate Table row as carried on the
/// wire (RFC 9946 §7.1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SendingRate {
    /// Transmit interval of the first transmitter, in microseconds.
    pub tx_interval1: u32,
    /// UDP payload of the first transmitter, in bytes.
    pub udp_payload1: u32,
    /// Datagrams per interval of the first transmitter.
    pub burst_size1: u32,
    /// Transmit interval of the second transmitter, in microseconds.
    pub tx_interval2: u32,
    /// UDP payload of the second transmitter, in bytes.
    pub udp_payload2: u32,
    /// Datagrams per interval of the second transmitter.
    pub burst_size2: u32,
    /// Size of one extra datagram sent at the end of each second interval.
    pub udp_addon2: u32,
}

impl SendingRate {
    pub(crate) fn write(&self, b: &mut [u8]) {
        let fields = [
            self.tx_interval1,
            self.udp_payload1,
            self.burst_size1,
            self.tx_interval2,
            self.udp_payload2,
            self.burst_size2,
            self.udp_addon2,
        ];
        for (chunk, value) in b.as_chunks_mut::<4>().0.iter_mut().zip(fields) {
            *chunk = value.to_be_bytes();
        }
    }

    pub(crate) fn read(b: &[u8]) -> Self {
        let at = |i: usize| u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
        Self {
            tx_interval1: at(0),
            udp_payload1: at(4),
            burst_size1: at(8),
            tx_interval2: at(12),
            udp_payload2: at(16),
            burst_size2: at(20),
            udp_addon2: at(24),
        }
    }
}

/// A Test Activation PDU. The reserved fields are not represented (see the
/// crate docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivationPdu {
    pub protocol_version: u16,
    pub cmd_request: u8,
    pub cmd_response: u8,
    /// Lower delay variation threshold, in ms.
    pub low_thresh: u16,
    /// Upper delay variation threshold, in ms.
    pub upper_thresh: u16,
    /// Trial Interval (Status PDU interval), in ms.
    pub trial_int: u16,
    /// Test Interval, in seconds.
    pub test_int_time: u16,
    pub dscp_ecn: u8,
    pub sr_index_conf: u16,
    pub use_ow_del_var: u8,
    pub high_speed_delta: u8,
    pub slow_adj_thresh: u16,
    pub seq_err_thresh: u16,
    pub ignore_ooo_dup: u8,
    pub modifier_bitmap: u8,
    pub rate_adj_algo: u8,
    pub sending_rate: SendingRate,
    /// Sub-Interval period, in ms.
    pub sub_int_period: u16,
    pub auth_mode: u8,
    pub auth_unix_time: u32,
    pub auth_digest: [u8; AUTH_DIGEST_LEN],
    pub key_id: u8,
    pub checksum: u16,
}

impl ActivationPdu {
    /// Sets authDigest to the HMAC of the whole PDU with authDigest and
    /// checkSum zeroed (RFC 9946 §5.3.1). A checksum, if used, must be
    /// computed after this.
    pub fn sign(&mut self, key: &[u8; 32]) {
        self.auth_digest = auth::auth_digest(key, &self.digest_input());
    }

    /// Whether a received Activation PDU's authDigest is the HMAC of its
    /// bytes under `key` (constant time). The bytes as received count,
    /// reserved fields included: a decoded PDU drops those, so it can't be
    /// re-encoded for this.
    pub fn verify_received(bytes: &[u8], key: &[u8; 32]) -> bool {
        bytes.len() == ACTIVATION_PDU_LEN && auth::verify_received(key, bytes, 68..100, 102..104)
    }

    /// The encoded PDU with authDigest and checkSum zeroed.
    fn digest_input(&self) -> [u8; ACTIVATION_PDU_LEN] {
        Self {
            auth_digest: [0; AUTH_DIGEST_LEN],
            checksum: 0,
            ..self.clone()
        }
        .encode()
    }

    pub fn encode(&self) -> [u8; ACTIVATION_PDU_LEN] {
        let mut b = [0u8; ACTIVATION_PDU_LEN];
        b[0..2].copy_from_slice(&ACTIVATION_PDU_ID.to_be_bytes());
        b[2..4].copy_from_slice(&self.protocol_version.to_be_bytes());
        b[4] = self.cmd_request;
        b[5] = self.cmd_response;
        b[6..8].copy_from_slice(&self.low_thresh.to_be_bytes());
        b[8..10].copy_from_slice(&self.upper_thresh.to_be_bytes());
        b[10..12].copy_from_slice(&self.trial_int.to_be_bytes());
        b[12..14].copy_from_slice(&self.test_int_time.to_be_bytes());
        // b[14] is reserved1.
        b[15] = self.dscp_ecn;
        b[16..18].copy_from_slice(&self.sr_index_conf.to_be_bytes());
        b[18] = self.use_ow_del_var;
        b[19] = self.high_speed_delta;
        b[20..22].copy_from_slice(&self.slow_adj_thresh.to_be_bytes());
        b[22..24].copy_from_slice(&self.seq_err_thresh.to_be_bytes());
        b[24] = self.ignore_ooo_dup;
        b[25] = self.modifier_bitmap;
        b[26] = self.rate_adj_algo;
        // b[27] is reserved2.
        self.sending_rate.write(&mut b[28..28 + SENDING_RATE_LEN]);
        b[56..58].copy_from_slice(&self.sub_int_period.to_be_bytes());
        // b[58..63] are reserved3, reserved4 and reserved5.
        b[63] = self.auth_mode;
        b[64..68].copy_from_slice(&self.auth_unix_time.to_be_bytes());
        b[68..100].copy_from_slice(&self.auth_digest);
        b[100] = self.key_id;
        // b[101] is reservedAuth1.
        b[102..104].copy_from_slice(&self.checksum.to_be_bytes());
        b
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        check_fixed(bytes, ACTIVATION_PDU_LEN, ACTIVATION_PDU_ID)?;
        let u16_at = |i: usize| u16::from_be_bytes([bytes[i], bytes[i + 1]]);
        let mut auth_digest = [0u8; AUTH_DIGEST_LEN];
        auth_digest.copy_from_slice(&bytes[68..100]);
        Ok(Self {
            protocol_version: u16_at(2),
            cmd_request: bytes[4],
            cmd_response: bytes[5],
            low_thresh: u16_at(6),
            upper_thresh: u16_at(8),
            trial_int: u16_at(10),
            test_int_time: u16_at(12),
            dscp_ecn: bytes[15],
            sr_index_conf: u16_at(16),
            use_ow_del_var: bytes[18],
            high_speed_delta: bytes[19],
            slow_adj_thresh: u16_at(20),
            seq_err_thresh: u16_at(22),
            ignore_ooo_dup: bytes[24],
            modifier_bitmap: bytes[25],
            rate_adj_algo: bytes[26],
            sending_rate: SendingRate::read(&bytes[28..28 + SENDING_RATE_LEN]),
            sub_int_period: u16_at(56),
            auth_mode: bytes[63],
            auth_unix_time: u32::from_be_bytes([bytes[64], bytes[65], bytes[66], bytes[67]]),
            auth_digest,
            key_id: bytes[100],
            checksum: u16_at(102),
        })
    }
}
