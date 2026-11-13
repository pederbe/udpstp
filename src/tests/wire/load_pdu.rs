//! Load PDU header layout (RFC 9946 §8.1, Figures 8 and 9) and the Internet
//! checksum (RFC 1071, RFC 9946 §5.6).
//!
//! The expected bytes are written out by hand from the RFC's field layout,
//! not produced by the codec, so the test can disagree with it.

use crate::wire::DecodeError;
use crate::wire::checksum::internet_checksum;
use crate::wire::load::{self, LoadHeader};

fn sample() -> LoadHeader {
    LoadHeader {
        test_action: load::test_action::STOP2,
        rx_stopped: 1,
        seq_no: 0x0102_0304,
        udp_payload: 0x04C6,
        spdu_seq_err: 0x0007,
        spdu_time: (0x1122_3344, 0x0555_6677),
        lpdu_time: (0x6AB0_1234, 0x0000_0FA0),
        rtt_resp_delay: 0x0009,
        checksum: 0xABCD,
    }
}

#[rustfmt::skip]
const SAMPLE_BYTES: [u8; 32] = [
    0xBE, 0xEF,             // pduId
    0x02,                   // testAction: TEST_ACT_STOP2
    0x01,                   // rxStopped
    0x01, 0x02, 0x03, 0x04, // lpduSeqNo
    0x04, 0xC6,             // udpPayload
    0x00, 0x07,             // spduSeqErr
    0x11, 0x22, 0x33, 0x44, // spduTime_sec
    0x05, 0x55, 0x66, 0x77, // spduTime_nsec
    0x6A, 0xB0, 0x12, 0x34, // lpduTime_sec
    0x00, 0x00, 0x0F, 0xA0, // lpduTime_nsec
    0x00, 0x09,             // rttRespDelay
    0xAB, 0xCD,             // checkSum
];

#[udpstp_req::req(
    "RFC9946-8.1-11",
    "RFC9946-8.1-13",
    "RFC9097-8.3-3",
    "RFC9946-12.3.1-L1"
)]
#[test]
fn header_encodes_to_the_rfc_layout() {
    let mut buf = [0u8; load::LOAD_HEADER_LEN];
    sample().encode_into(&mut buf);
    assert_eq!(buf, SAMPLE_BYTES);
}

#[udpstp_req::req("RFC9946-8.1-11", "RFC9946-8.1-13")]
#[test]
fn header_decodes_from_the_rfc_layout_with_any_payload_after_it() {
    let mut datagram = SAMPLE_BYTES.to_vec();
    datagram.extend_from_slice(&[0x5A; 100]);
    assert_eq!(LoadHeader::decode(&datagram), Ok(sample()));
    assert_eq!(LoadHeader::decode(&SAMPLE_BYTES), Ok(sample()));
}

#[udpstp_req::req("RFC9946-8.1-2", "RFC9946-8.1-3")]
#[test]
fn decoding_rejects_short_datagrams_and_other_pdu_ids() {
    assert_eq!(
        LoadHeader::decode(&SAMPLE_BYTES[..31]),
        Err(DecodeError::TooShort {
            minimum: 32,
            actual: 31
        })
    );
    let mut other = SAMPLE_BYTES;
    other[0..2].copy_from_slice(&[0xAC, 0xE2]);
    assert_eq!(
        LoadHeader::decode(&other),
        Err(DecodeError::WrongPduId {
            expected: 0xBEEF,
            actual: 0xACE2
        })
    );
}

// RFC 1071 §3 (numerical example: bytes 00 01 f2 03 f4 f5 f6 f7 sum to
// ddf2, so the checksum, the one's complement of the sum, is 220d)
#[udpstp_req::req("RFC9946-5.6-L2")]
#[test]
fn internet_checksum_matches_the_rfc_1071_example() {
    let bytes = [0x00, 0x01, 0xF2, 0x03, 0xF4, 0xF5, 0xF6, 0xF7];
    assert_eq!(internet_checksum(&bytes), !0xDDF2);
}

// RFC 1071 §1 (an odd byte count is padded with a zero byte)
#[udpstp_req::req("RFC9946-5.6-L2")]
#[test]
fn internet_checksum_pads_an_odd_length() {
    let bytes = [0x00, 0x01, 0xF2, 0x03, 0xF4, 0xF5, 0xF6];
    let padded = [0x00, 0x01, 0xF2, 0x03, 0xF4, 0xF5, 0xF6, 0x00];
    assert_eq!(internet_checksum(&bytes), internet_checksum(&padded));
}

// Computed last with the field zeroed and verified by the receiver; the
// checksum covers only the Load PDU header.
#[udpstp_req::req(
    "RFC9946-5.6-3",
    "RFC9946-5.6-4",
    "RFC9946-5.6-L1",
    "RFC9946-8.1-L13",
    "RFC9946-8.1-L14"
)]
#[test]
fn load_header_checksum_covers_the_header_only() {
    let mut datagram = SAMPLE_BYTES.to_vec();
    datagram.extend_from_slice(&[0x5A; 16]);
    LoadHeader::fill_checksum(&mut datagram);

    let mut zeroed = SAMPLE_BYTES;
    zeroed[30..32].fill(0);
    let expected = internet_checksum(&zeroed);
    assert_eq!(datagram[30..32], expected.to_be_bytes());
    assert!(LoadHeader::checksum_valid(&datagram));

    datagram[40] ^= 0xFF;
    assert!(
        LoadHeader::checksum_valid(&datagram),
        "a payload change keeps it valid"
    );
    datagram[5] ^= 0x01;
    assert!(
        !LoadHeader::checksum_valid(&datagram),
        "a header change makes it invalid"
    );
}

// RFC 1071 §1 (end-around carry: carries are added back until none
// is left). FFFF + FFFF + 0001 = 1FFFF; folding once gives 10000, which
// carries again to 0001, so the checksum is FFFE.
#[udpstp_req::req("RFC9946-5.6-L2")]
#[test]
fn internet_checksum_folds_every_carry() {
    assert_eq!(
        internet_checksum(&[0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x01]),
        0xFFFE
    );
}

// A zero field means "not populated" (RFC 9946 §5.6), and a computed zero
// is sent as all ones (RFC 768). pduId BEEF plus rttRespDelay 4110 sum to
// FFFF, so the checksum computes to 0000.
#[udpstp_req::req("RFC9946-5.6-3")]
#[test]
fn a_zero_checksum_is_sent_as_all_ones() {
    let mut header = [0u8; 32];
    header[0..2].copy_from_slice(&[0xBE, 0xEF]);
    header[28..30].copy_from_slice(&[0x41, 0x10]);
    LoadHeader::fill_checksum(&mut header);

    assert_eq!(header[30..32], [0xFF, 0xFF]);
    assert!(LoadHeader::checksum_valid(&header));
}
