//! Load PDU header (RFC 9946 §8.1, Figures 8 and 9). The payload content
//! after the header is not part of the codec.

use crate::wire::DecodeError;
use crate::wire::checksum::internet_checksum;

/// pduId of the Load PDU.
pub const LOAD_PDU_ID: u16 = 0xBEEF;

/// Size of the Load PDU header (`struct loadHdr`) in bytes.
pub const LOAD_HEADER_LEN: usize = 32;

/// Where the checkSum field sits in the header.
const CHECKSUM: std::ops::Range<usize> = 30..32;

/// testAction values (RFC 9946 §8.1).
pub mod test_action {
    /// Test active.
    pub const TEST: u8 = 0;
    /// First phase of graceful termination, used locally by the Server.
    #[allow(dead_code)] // Receivers handle either nonzero graceful-stop phase.
    pub const STOP1: u8 = 1;
    /// Second phase of graceful termination, exchanged with the Client.
    pub const STOP2: u8 = 2;
}

/// A Load PDU header.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LoadHeader {
    pub test_action: u8,
    pub rx_stopped: u8,
    /// Load PDU sequence number, starting at 1.
    pub seq_no: u32,
    /// The whole UDP payload's size (header and payload content), in bytes.
    pub udp_payload: u16,
    pub spdu_seq_err: u16,
    /// spduTime_sec and spduTime_nsec from the last Status PDU received.
    pub spdu_time: (u32, u32),
    /// lpduTime_sec and lpduTime_nsec: this Load PDU's send time.
    pub lpdu_time: (u32, u32),
    /// Milliseconds from that Status PDU's arrival to this send.
    pub rtt_resp_delay: u16,
    pub checksum: u16,
}

impl LoadHeader {
    pub fn encode_into(&self, b: &mut [u8; LOAD_HEADER_LEN]) {
        b[0..2].copy_from_slice(&LOAD_PDU_ID.to_be_bytes());
        b[2] = self.test_action;
        b[3] = self.rx_stopped;
        b[4..8].copy_from_slice(&self.seq_no.to_be_bytes());
        b[8..10].copy_from_slice(&self.udp_payload.to_be_bytes());
        b[10..12].copy_from_slice(&self.spdu_seq_err.to_be_bytes());
        b[12..16].copy_from_slice(&self.spdu_time.0.to_be_bytes());
        b[16..20].copy_from_slice(&self.spdu_time.1.to_be_bytes());
        b[20..24].copy_from_slice(&self.lpdu_time.0.to_be_bytes());
        b[24..28].copy_from_slice(&self.lpdu_time.1.to_be_bytes());
        b[28..30].copy_from_slice(&self.rtt_resp_delay.to_be_bytes());
        b[CHECKSUM].copy_from_slice(&self.checksum.to_be_bytes());
    }

    /// Decodes the header at the start of a datagram, which may carry any
    /// payload content after it (RFC 9946 §8.1: the size must be at least
    /// the header's, and the PDU ID 0xBEEF).
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let Some(b) = bytes.first_chunk::<LOAD_HEADER_LEN>() else {
            return Err(DecodeError::TooShort {
                minimum: LOAD_HEADER_LEN,
                actual: bytes.len(),
            });
        };
        let u16_at = |i: usize| u16::from_be_bytes([b[i], b[i + 1]]);
        let u32_at = |i: usize| u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
        if u16_at(0) != LOAD_PDU_ID {
            return Err(DecodeError::WrongPduId {
                expected: LOAD_PDU_ID,
                actual: u16_at(0),
            });
        }
        Ok(Self {
            test_action: b[2],
            rx_stopped: b[3],
            seq_no: u32_at(4),
            udp_payload: u16_at(8),
            spdu_seq_err: u16_at(10),
            spdu_time: (u32_at(12), u32_at(16)),
            lpdu_time: (u32_at(20), u32_at(24)),
            rtt_resp_delay: u16_at(28),
            checksum: u16_at(30),
        })
    }

    /// Fills the checkSum field of the Load PDU at the start of `datagram`:
    /// the Internet checksum of the header only, computed with the field
    /// zeroed (RFC 9946 §5.6, §8.1). Call it after the header is complete.
    /// A result of 0x0000 is sent as 0xFFFF, its one's complement
    /// equivalent, because a zero field means "not populated" (§5.6) and a
    /// receiver would skip it; UDP does the same (RFC 768).
    pub fn fill_checksum(datagram: &mut [u8]) {
        let header = &mut datagram[..LOAD_HEADER_LEN];
        header[CHECKSUM].fill(0);
        let sum = match internet_checksum(header) {
            0 => 0xFFFF,
            sum => sum,
        };
        header[CHECKSUM].copy_from_slice(&sum.to_be_bytes());
    }

    /// Whether the checkSum field matches the header at the start of
    /// `datagram`. A header whose checksum was filled sums to zero.
    pub fn checksum_valid(datagram: &[u8]) -> bool {
        datagram
            .get(..LOAD_HEADER_LEN)
            .is_some_and(|header| internet_checksum(header) == 0)
    }

    /// The checkSum field of a Load PDU `header`, read without decoding
    /// the rest.
    pub fn checksum_field(header: &[u8; LOAD_HEADER_LEN]) -> u16 {
        u16::from_be_bytes([header[CHECKSUM.start], header[CHECKSUM.start + 1]])
    }
}
