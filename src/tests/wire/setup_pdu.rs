//! Setup PDU layout (RFC 9946 §6.1, Figure 2 and Figure 3).
//!
//! The expected bytes are written out by hand from the RFC's field layout,
//! not produced by the codec, so the test can disagree with it.

use crate::wire::{DecodeError, SetupPdu, setup};

/// A Setup Response with every field set to a distinct, recognisable value.
fn sample() -> SetupPdu {
    SetupPdu {
        protocol_version: 20,
        mc_index: 0x03,
        mc_count: 0x04,
        mc_ident: 0xBEAD,
        cmd_request: setup::cmd_request::SETUP_RESPONSE,
        cmd_response: setup::cmd_response::ACK_OK,
        max_bandwidth: 0x8123,
        test_port: 0xC350,
        modifier_bitmap: setup::modifier::JUMBO_STATUS,
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
        0xAC, 0xE1, // pduId
        0x00, 0x14, // protocolVer = 20
        0x03,       // mcIndex
        0x04,       // mcCount
        0xBE, 0xAD, // mcIdent
        0x02,       // cmdRequest = CHSR_CREQ_SETUPRSP
        0x01,       // cmdResponse = CHSR_CRSP_ACKOK
        0x81, 0x23, // maxBandwidth (upstream bit set)
        0xC3, 0x50, // testPort = 50000
        0x01,       // modifierBitmap = CHSR_JUMBO_STATUS
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

// The sample bytes carry reservedAuth1 = 0, so encoding also shows reserved
// fields are sent as zero.
#[udpstp_req::req(
    "RFC9946-6.1-4",
    "RFC9946-6.1-5",
    "RFC9946-6.1-13",
    "RFC9946-6.2.1-7",
    "RFC9946-12.3.1-L1",
    "RFC9946-12.3.2-L1"
)]
#[test]
fn setup_pdu_encodes_to_the_rfc_layout() {
    assert_eq!(sample().encode().as_slice(), sample_bytes().as_slice());
    assert_eq!(setup::SETUP_PDU_LEN, 56);
}

#[udpstp_req::req("RFC9946-6.1-5", "RFC9946-6.2.1-7")]
#[test]
fn setup_pdu_decodes_from_the_rfc_layout() {
    assert_eq!(SetupPdu::decode(&sample_bytes()), Ok(sample()));
}

#[udpstp_req::req("RFC9946-6.1-4", "RFC9946-6.1-13")]
#[test]
fn reserved_field_is_ignored_on_receipt() {
    let mut bytes = sample_bytes();
    bytes[53] = 0xFF; // reservedAuth1
    assert_eq!(SetupPdu::decode(&bytes), Ok(sample()));
}

#[udpstp_req::req("RFC9946-6.2-1", "RFC9946-6.2.1-1")]
#[test]
fn wrong_size_or_pdu_id_is_rejected() {
    let bytes = sample_bytes();
    assert_eq!(
        SetupPdu::decode(&bytes[..55]),
        Err(DecodeError::WrongLength {
            expected: 56,
            actual: 55
        })
    );
    let mut longer = bytes.clone();
    longer.push(0);
    assert_eq!(
        SetupPdu::decode(&longer),
        Err(DecodeError::WrongLength {
            expected: 56,
            actual: 57
        })
    );
    let mut wrong_id = bytes;
    wrong_id[1] = 0xE2;
    assert_eq!(
        SetupPdu::decode(&wrong_id),
        Err(DecodeError::WrongPduId {
            expected: 0xACE1,
            actual: 0xACE2
        })
    );
}
