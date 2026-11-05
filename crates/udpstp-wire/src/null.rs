//! Null Request PDU (RFC 9946 §6.2.2, Figure 4): sent by the Server from a
//! Test Connection's Ephemeral Port right after the Setup Response, so its
//! own firewall lets the Client's traffic in. There is no response.

use crate::{AUTH_DIGEST_LEN, DecodeError, auth, check_fixed};

/// pduId of the Null Request PDU (CHNR_ID).
pub const NULL_PDU_ID: u16 = 0xDEAD;

/// Size of the Null Request PDU in bytes.
pub const NULL_PDU_LEN: usize = 48;

/// cmdRequest values.
pub mod cmd_request {
    pub const NONE: u8 = 0;
    /// CHNR_CREQ_NULLREQ.
    pub const NULL_REQUEST: u8 = 1;
}

/// cmdResponse values.
pub mod cmd_response {
    /// CHNR_CRSP_NONE.
    pub const NONE: u8 = 0;
}

/// A Null Request PDU. `reserved1` and `reservedAuth1` are not represented
/// (see the crate docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NullPdu {
    pub protocol_version: u16,
    pub cmd_request: u8,
    pub cmd_response: u8,
    pub auth_mode: u8,
    pub auth_unix_time: u32,
    pub auth_digest: [u8; AUTH_DIGEST_LEN],
    pub key_id: u8,
    pub checksum: u16,
}

impl NullPdu {
    pub fn encode(&self) -> [u8; NULL_PDU_LEN] {
        let mut b = [0u8; NULL_PDU_LEN];
        b[0..2].copy_from_slice(&NULL_PDU_ID.to_be_bytes());
        b[2..4].copy_from_slice(&self.protocol_version.to_be_bytes());
        b[4] = self.cmd_request;
        b[5] = self.cmd_response;
        // b[6] is reserved1, left zero.
        b[7] = self.auth_mode;
        b[8..12].copy_from_slice(&self.auth_unix_time.to_be_bytes());
        b[12..44].copy_from_slice(&self.auth_digest);
        b[44] = self.key_id;
        // b[45] is reservedAuth1, left zero.
        b[46..48].copy_from_slice(&self.checksum.to_be_bytes());
        b
    }

    /// Sets authDigest to the HMAC of the whole PDU with authDigest and
    /// checkSum zeroed (RFC 9946 §5.3.1). A checksum, if used, must be
    /// computed after this.
    pub fn sign(&mut self, key: &[u8; 32]) {
        let input = Self {
            auth_digest: [0; AUTH_DIGEST_LEN],
            checksum: 0,
            ..self.clone()
        }
        .encode();
        self.auth_digest = auth::auth_digest(key, &input);
    }

    /// Whether a received Null Request's authDigest is the HMAC of its
    /// bytes under `key` (constant time), reserved fields included.
    pub fn verify_received(bytes: &[u8], key: &[u8; 32]) -> bool {
        bytes.len() == NULL_PDU_LEN && auth::verify_received(key, bytes, 12..44, 46..48)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        check_fixed(bytes, NULL_PDU_LEN, NULL_PDU_ID)?;
        let mut auth_digest = [0u8; AUTH_DIGEST_LEN];
        auth_digest.copy_from_slice(&bytes[12..44]);
        Ok(Self {
            protocol_version: u16::from_be_bytes([bytes[2], bytes[3]]),
            cmd_request: bytes[4],
            cmd_response: bytes[5],
            auth_mode: bytes[7],
            auth_unix_time: u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            auth_digest,
            key_id: bytes[44],
            checksum: u16::from_be_bytes([bytes[46], bytes[47]]),
        })
    }
}
