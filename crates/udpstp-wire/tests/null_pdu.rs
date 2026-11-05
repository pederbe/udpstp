//! Null Request PDU layout (RFC 9946 §6.2.2, Figure 4).
//!
//! The expected bytes are written out by hand from the RFC's field layout,
//! not produced by the codec, so the test can disagree with it.

use udpstp_wire::{DecodeError, NullPdu, null};

fn sample() -> NullPdu {
    NullPdu {
        protocol_version: 20,
        cmd_request: null::cmd_request::NULL_REQUEST,
        cmd_response: null::cmd_response::NONE,
        auth_mode: 1,
        auth_unix_time: 0x6A2B_3C4D,
        auth_digest: [0x5A; 32],
        key_id: 0x07,
        checksum: 0x1234,
    }
}

#[rustfmt::skip]
fn sample_bytes() -> Vec<u8> {
    let mut b = vec![
        0xDE, 0xAD, // pduId = CHNR_ID
        0x00, 0x14, // protocolVer = 20
        0x01,       // cmdRequest = CHNR_CREQ_NULLREQ
        0x00,       // cmdResponse = CHNR_CRSP_NONE
        0x00,       // reserved1
        0x01,       // authMode = 1
        0x6A, 0x2B, 0x3C, 0x4D, // authUnixTime
    ];
    b.extend_from_slice(&[0x5A; 32]); // authDigest
    b.extend_from_slice(&[
        0x07,       // keyId
        0x00,       // reservedAuth1
        0x12, 0x34, // checkSum
    ]);
    b
}

#[udpstp_req::req(
    "RFC9946-6.2.2-7",
    "RFC9946-6.2.2-9",
    "RFC9946-6.2.2-L4",
    "RFC9946-6.2.2-L5"
)]
#[test]
fn null_pdu_encodes_to_the_rfc_layout() {
    assert_eq!(sample().encode().as_slice(), sample_bytes().as_slice());
    assert_eq!(null::NULL_PDU_LEN, 48);
}

#[udpstp_req::req("RFC9946-6.2.2-7")]
#[test]
fn null_pdu_decodes_from_the_rfc_layout() {
    assert_eq!(NullPdu::decode(&sample_bytes()), Ok(sample()));
}

#[udpstp_req::req("RFC9946-6.1-4")]
#[test]
fn reserved_fields_are_ignored_on_receipt() {
    let mut bytes = sample_bytes();
    bytes[6] = 0xFF; // reserved1
    bytes[45] = 0xFF; // reservedAuth1
    assert_eq!(NullPdu::decode(&bytes), Ok(sample()));
}

// Verify the size and the PDU ID.
#[udpstp_req::req("RFC9946-6.2.2-6")]
#[test]
fn wrong_size_or_pdu_id_is_rejected() {
    let bytes = sample_bytes();
    assert_eq!(
        NullPdu::decode(&bytes[..47]),
        Err(DecodeError::WrongLength {
            expected: 48,
            actual: 47
        })
    );
    let mut wrong_id = bytes;
    wrong_id[1] = 0xAE;
    assert_eq!(
        NullPdu::decode(&wrong_id),
        Err(DecodeError::WrongPduId {
            expected: 0xDEAD,
            actual: 0xDEAE
        })
    );
}

// The authentication fields follow the same methodology as the Setup PDUs:
// HMAC over the PDU with authDigest and checkSum zeroed, reserved fields
// included.
#[udpstp_req::req("RFC9946-6.2.2-L3", "RFC9946-5.3.1-L3")]
#[test]
fn a_signed_null_pdu_verifies_as_received() {
    let key = [0x42; 32];
    let mut pdu = sample();
    pdu.checksum = 0;
    pdu.sign(&key);
    let mut bytes = pdu.encode().to_vec();
    assert!(NullPdu::verify_received(&bytes, &key));
    // The digest covers the reserved fields as received.
    bytes[6] = 1;
    assert!(!NullPdu::verify_received(&bytes, &key));
    assert!(!NullPdu::verify_received(&pdu.encode(), &[0x43; 32]));
}
