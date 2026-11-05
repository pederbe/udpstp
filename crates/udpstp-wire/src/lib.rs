#![forbid(unsafe_code)]
//! UDPSTP (RFC 9946) PDU layouts and codecs.
//!
//! Codec convention, shared by every PDU type:
//! - All fields are big-endian, at the offsets of the RFC's figures.
//! - Fields "reserved for alignment" are not represented in the PDU structs:
//!   they are written as zero and ignored when decoding (RFC 9946 §6.1).
//! - The `checksum` field is carried as a plain value. Encoding writes it
//!   unchanged and decoding returns it unchanged; computing and verifying it
//!   is left to the caller, because it must be done last on send and first on
//!   receive (RFC 9946 §5.6).
//! - Decoding checks only the size and the PDU ID. Every other field is
//!   returned as received, so the protocol layer decides what is valid.
//!
//! A Null Request signed in mode 1, with the checksum, and
//! read back:
//!
//! ```
//! use udpstp_wire::{NullPdu, PROTOCOL_VERSION, auth_mode, checksum, null};
//!
//! let key = [7u8; 32]; // a Server Authentication Key
//! let mut pdu = NullPdu {
//!     protocol_version: PROTOCOL_VERSION,
//!     cmd_request: null::cmd_request::NULL_REQUEST,
//!     cmd_response: null::cmd_response::NONE,
//!     auth_mode: auth_mode::MODE_1,
//!     auth_unix_time: 1_790_000_000,
//!     auth_digest: [0; 32],
//!     key_id: 1,
//!     checksum: 0,
//! };
//! pdu.sign(&key);
//! let mut bytes = pdu.encode();
//! checksum::fill_control(&mut bytes);
//!
//! assert!(checksum::control_valid(&bytes));
//! assert!(NullPdu::verify_received(&bytes, &key));
//! let received = NullPdu::decode(&bytes).expect("a Null Request");
//! assert_eq!(received.auth_digest, pdu.auth_digest);
//! assert_ne!(received.checksum, 0);
//! ```

pub mod activation;
pub mod auth;
pub mod checksum;
pub mod load;
pub mod null;
pub mod setup;
pub mod status;

pub use activation::{ActivationPdu, SendingRate};
pub use load::LoadHeader;
pub use null::NullPdu;
pub use setup::SetupPdu;
pub use status::StatusPdu;

/// The UDPSTP protocol version defined by RFC 9946 (§12.3.2).
pub const PROTOCOL_VERSION: u16 = 20;

/// The standard UDP port for the Setup exchange (RFC 9946 §12.1).
pub const STANDARD_PORT: u16 = 24601;

/// Length of the HMAC-SHA-256 authDigest field.
pub const AUTH_DIGEST_LEN: usize = 32;

/// authMode values (RFC 9946 §12.3.4).
pub mod auth_mode {
    /// Not used by RFC 9946; only available as an explicit, non-conformant opt-in.
    pub const UNAUTHENTICATED: u8 = 0;
    /// Authenticated Control Phase.
    pub const MODE_1: u8 = 1;
    /// Authenticated Control Phase and Status PDUs.
    pub const MODE_2: u8 = 2;
}

/// Why a datagram could not be decoded as the requested PDU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// The datagram is not exactly the size of the PDU.
    WrongLength { expected: usize, actual: usize },
    /// The datagram is shorter than the PDU's header.
    TooShort { minimum: usize, actual: usize },
    /// The pduId field names a different PDU type.
    WrongPduId { expected: u16, actual: u16 },
}

/// Checks a datagram's size and PDU ID before a fixed-size PDU is decoded.
pub(crate) fn check_fixed(bytes: &[u8], len: usize, pdu_id: u16) -> Result<(), DecodeError> {
    if bytes.len() != len {
        return Err(DecodeError::WrongLength {
            expected: len,
            actual: bytes.len(),
        });
    }
    let actual = u16::from_be_bytes([bytes[0], bytes[1]]);
    if actual != pdu_id {
        return Err(DecodeError::WrongPduId {
            expected: pdu_id,
            actual,
        });
    }
    Ok(())
}
