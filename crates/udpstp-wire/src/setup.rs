//! Test Setup PDU: Setup Request and Setup Response (RFC 9946 §6.1, Figure 3).

use crate::{AUTH_DIGEST_LEN, DecodeError, auth, check_fixed};

/// pduId of the Test Setup PDU.
pub const SETUP_PDU_ID: u16 = 0xACE1;

/// Size of the Test Setup PDU in bytes.
pub const SETUP_PDU_LEN: usize = 56;

/// cmdRequest values (RFC 9946 Figure 3).
pub mod cmd_request {
    pub const NONE: u8 = 0;
    pub const SETUP_REQUEST: u8 = 1;
    pub const SETUP_RESPONSE: u8 = 2;
}

/// cmdResponse values (RFC 9946 §12.3.5).
pub mod cmd_response {
    pub const NONE: u8 = 0;
    pub const ACK_OK: u8 = 1;
    pub const BAD_VERSION: u8 = 2;
    pub const JUMBO_MISMATCH: u8 = 3;
    pub const AUTH_NOT_CONFIGURED: u8 = 4;
    pub const AUTH_REQUIRED: u8 = 5;
    pub const AUTH_MODE_INVALID: u8 = 6;
    pub const AUTH_FAILURE: u8 = 7;
    pub const AUTH_TIME_INVALID: u8 = 8;
    pub const MAX_BANDWIDTH_REQUIRED: u8 = 9;
    pub const CAPACITY_EXCEEDED: u8 = 10;
    pub const TRADITIONAL_MTU_MISMATCH: u8 = 11;
    pub const MULTI_CONNECTION_INVALID: u8 = 12;
    pub const CONNECTION_ALLOCATION_FAILURE: u8 = 13;
}

/// modifierBitmap bits (RFC 9946 §12.3.3).
pub mod modifier {
    pub const JUMBO_STATUS: u8 = 0x01;
    pub const TRADITIONAL_MTU: u8 = 0x02;
}

/// The most significant bit of maxBandwidth marks an Upstream Test (CHSR_USDIR_BIT).
pub const UPSTREAM_DIRECTION_BIT: u16 = 0x8000;

/// A Test Setup PDU. `reservedAuth1` is not represented (see the crate docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupPdu {
    pub protocol_version: u16,
    pub mc_index: u8,
    pub mc_count: u8,
    pub mc_ident: u16,
    pub cmd_request: u8,
    pub cmd_response: u8,
    pub max_bandwidth: u16,
    pub test_port: u16,
    pub modifier_bitmap: u8,
    pub auth_mode: u8,
    pub auth_unix_time: u32,
    pub auth_digest: [u8; AUTH_DIGEST_LEN],
    pub key_id: u8,
    pub checksum: u16,
}

impl SetupPdu {
    pub fn encode(&self) -> [u8; SETUP_PDU_LEN] {
        let mut b = [0u8; SETUP_PDU_LEN];
        b[0..2].copy_from_slice(&SETUP_PDU_ID.to_be_bytes());
        b[2..4].copy_from_slice(&self.protocol_version.to_be_bytes());
        b[4] = self.mc_index;
        b[5] = self.mc_count;
        b[6..8].copy_from_slice(&self.mc_ident.to_be_bytes());
        b[8] = self.cmd_request;
        b[9] = self.cmd_response;
        b[10..12].copy_from_slice(&self.max_bandwidth.to_be_bytes());
        b[12..14].copy_from_slice(&self.test_port.to_be_bytes());
        b[14] = self.modifier_bitmap;
        b[15] = self.auth_mode;
        b[16..20].copy_from_slice(&self.auth_unix_time.to_be_bytes());
        b[20..52].copy_from_slice(&self.auth_digest);
        b[52] = self.key_id;
        // b[53] is reservedAuth1, left zero.
        b[54..56].copy_from_slice(&self.checksum.to_be_bytes());
        b
    }

    /// Sets authDigest to the HMAC of the whole PDU with authDigest and
    /// checkSum zeroed (RFC 9946 §5.3.1). A checksum, if used, must be
    /// computed after this.
    pub fn sign(&mut self, key: &[u8; 32]) {
        self.auth_digest = auth::auth_digest(key, &self.digest_input());
    }

    /// Whether a received Setup PDU's authDigest is the HMAC of its bytes
    /// under `key` (constant time). The bytes as received count, reserved
    /// fields included: a decoded PDU drops those, so it can't be re-encoded
    /// for this.
    pub fn verify_received(bytes: &[u8], key: &[u8; 32]) -> bool {
        bytes.len() == SETUP_PDU_LEN && auth::verify_received(key, bytes, 20..52, 54..56)
    }

    /// The encoded PDU with authDigest and checkSum zeroed.
    fn digest_input(&self) -> [u8; SETUP_PDU_LEN] {
        Self {
            auth_digest: [0; AUTH_DIGEST_LEN],
            checksum: 0,
            ..self.clone()
        }
        .encode()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        check_fixed(bytes, SETUP_PDU_LEN, SETUP_PDU_ID)?;
        let u16_at = |i: usize| u16::from_be_bytes([bytes[i], bytes[i + 1]]);
        let mut auth_digest = [0u8; AUTH_DIGEST_LEN];
        auth_digest.copy_from_slice(&bytes[20..52]);
        Ok(Self {
            protocol_version: u16_at(2),
            mc_index: bytes[4],
            mc_count: bytes[5],
            mc_ident: u16_at(6),
            cmd_request: bytes[8],
            cmd_response: bytes[9],
            max_bandwidth: u16_at(10),
            test_port: u16_at(12),
            modifier_bitmap: bytes[14],
            auth_mode: bytes[15],
            auth_unix_time: u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]),
            auth_digest,
            key_id: bytes[52],
            checksum: u16_at(54),
        })
    }
}
