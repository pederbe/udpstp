//! Status PDU layout (RFC 9946 §8.2, Figures 10 and 11): 204 bytes with a
//! packed Sub-Interval statistics block.
//!
//! The expected bytes are written out by hand from the RFC's field layout,
//! not produced by the codec, so the test can disagree with it.

use crate::wire::checksum::internet_checksum;
use crate::wire::status::{self, StatusPdu, SubIntervalStats};
use crate::wire::{DecodeError, SendingRate};

fn sample() -> StatusPdu {
    StatusPdu {
        test_action: 2,
        rx_stopped: 1,
        spdu_seq_no: 0x0102_0304,
        sending_rate: SendingRate {
            tx_interval1: 0x11,
            udp_payload1: 0x12,
            burst_size1: 0x13,
            tx_interval2: 0x14,
            udp_payload2: 0x15,
            burst_size2: 0x16,
            udp_addon2: 0x17,
        },
        sub_int_seq_no: 0x2122_2324,
        sis_sav: SubIntervalStats {
            rx_datagrams: 0x31,
            rx_bytes: 0x3233_3435_3637_3839,
            delta_time: 0x3A,
            seq_err_loss: 0x3B,
            seq_err_ooo: 0x3C,
            seq_err_dup: 0x3D,
            delay_var_min: 0x3E,
            delay_var_max: 0x3F,
            delay_var_sum: 0x40,
            delay_var_cnt: 0x41,
            rtt_var_minimum: 0x42,
            rtt_var_maximum: 0x43,
            accum_time: 0x44,
        },
        seq_err_loss: 0x51,
        seq_err_ooo: 0x52,
        seq_err_dup: 0x53,
        clock_delta_min: 0xFFFF_FFFE,
        delay_var_min: 0x55,
        delay_var_max: 0x56,
        delay_var_sum: 0x57,
        delay_var_cnt: 0x58,
        rtt_minimum: 0x59,
        rtt_var_sample: status::STATUS_NODEL,
        delay_min_upd: 1,
        ti_delta_time: 0x61,
        ti_rx_datagrams: 0x62,
        ti_rx_bytes: 0x63,
        spdu_time: (0x6A00_0001, 0x0000_0002),
        auth_mode: 1,
        auth_unix_time: 0x7172_7374,
        auth_digest: [0xA5; 32],
        key_id: 0x09,
        checksum: 0xBEEF,
    }
}

fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

/// Figure 10 field by field.
fn sample_bytes() -> Vec<u8> {
    let mut b = Vec::new();
    b.extend([0xFE, 0xED]); // pduId
    b.extend([2, 1]); // testAction, rxStopped
    b.extend(be32(0x0102_0304)); // spduSeqNo
    for v in 0x11..=0x17 {
        b.extend(be32(v)); // srStruct
    }
    b.extend(be32(0x2122_2324)); // subIntSeqNo
    b.extend(be32(0x31)); // sisSav.rxDatagrams
    b.extend(0x3233_3435_3637_3839u64.to_be_bytes()); // sisSav.rxBytes
    for v in 0x3A..=0x44 {
        b.extend(be32(v)); // deltaTime .. accumTime
    }
    for v in [
        0x51,
        0x52,
        0x53,
        0xFFFF_FFFE,
        0x55,
        0x56,
        0x57,
        0x58,
        0x59,
        u32::MAX,
    ] {
        b.extend(be32(v)); // seqErrLoss .. rttVarSample
    }
    b.extend([1, 0, 0, 0]); // delayMinUpd, reserved1, reserved2
    for v in [0x61, 0x62, 0x63, 0x6A00_0001, 0x0000_0002] {
        b.extend(be32(v)); // tiDeltaTime .. spduTime_nsec
    }
    b.extend([0, 0, 0, 1]); // reserved3, reserved4, authMode
    b.extend(be32(0x7172_7374)); // authUnixTime
    b.extend([0xA5; 32]); // authDigest
    b.extend([0x09, 0]); // keyId, reservedAuth1
    b.extend([0xBE, 0xEF]); // checkSum
    b
}

#[udpstp_req::req(
    "RFC9946-8.2-5",
    "RFC9946-8.2-11",
    "RFC9097-8.3-3",
    "RFC9946-12.3.1-L1"
)]
#[test]
fn status_pdu_encodes_to_the_rfc_layout() {
    let bytes = sample_bytes();
    assert_eq!(bytes.len(), 204, "the RFC's layout is 204 bytes");
    assert_eq!(sample().encode().to_vec(), bytes);
}

#[udpstp_req::req("RFC9946-8.2-5", "RFC9946-8.2-11")]
#[test]
fn status_pdu_decodes_from_the_rfc_layout() {
    assert_eq!(StatusPdu::decode(&sample_bytes()), Ok(sample()));
}

#[udpstp_req::req("RFC9946-6.1-4")]
#[test]
fn reserved_fields_are_ignored_on_decode() {
    let mut bytes = sample_bytes();
    for at in [137, 138, 139, 160, 161, 162, 201] {
        bytes[at] = 0xFF;
    }
    assert_eq!(StatusPdu::decode(&bytes), Ok(sample()));
}

// A Status PDU is 204 bytes with pduId 0xFEED.
#[udpstp_req::req("RFC9946-8.2-3")]
#[test]
fn decoding_rejects_other_sizes_and_pdu_ids() {
    let bytes = sample_bytes();
    assert_eq!(
        StatusPdu::decode(&bytes[..203]),
        Err(DecodeError::WrongLength {
            expected: 204,
            actual: 203
        })
    );
    let mut other = bytes.clone();
    other[0..2].copy_from_slice(&[0xBE, 0xEF]);
    assert_eq!(
        StatusPdu::decode(&other),
        Err(DecodeError::WrongPduId {
            expected: 0xFEED,
            actual: 0xBEEF
        })
    );
}

// The checksum covers the whole PDU, computed last with the field zeroed.
#[udpstp_req::req("RFC9946-5.6-3", "RFC9946-5.6-4")]
#[test]
fn status_checksum_covers_the_whole_pdu() {
    let mut bytes = sample().encode();
    StatusPdu::fill_checksum(&mut bytes);
    let mut zeroed = bytes;
    zeroed[202..204].fill(0);
    assert_eq!(
        u16::from_be_bytes([bytes[202], bytes[203]]),
        internet_checksum(&zeroed),
        "the Internet checksum (known answers in load_pdu.rs) of all 204 bytes"
    );
    assert!(StatusPdu::checksum_valid(&bytes));
    bytes[100] ^= 0x01;
    assert!(!StatusPdu::checksum_valid(&bytes));
}

// A checkSum of 0 means not populated. pduId FEED and spduSeqNo 0x0112 sum
// to FFFF, so the checksum computes to 0000.
#[udpstp_req::req("RFC9946-5.6-3")]
#[test]
fn a_zero_status_checksum_is_sent_as_all_ones() {
    let mut bytes = [0u8; 204];
    bytes[0..2].copy_from_slice(&[0xFE, 0xED]);
    bytes[6..8].copy_from_slice(&[0x01, 0x12]);
    StatusPdu::fill_checksum(&mut bytes);
    assert_eq!(bytes[202..204], [0xFF, 0xFF]);
    assert!(StatusPdu::checksum_valid(&bytes));
}

#[udpstp_req::req("RFC9946-5.3.2-2", "RFC9946-5.3.2-L2")]
#[test]
fn a_signed_status_pdu_verifies_as_received() {
    let key = [0x42; 32];
    let mut pdu = sample();
    pdu.sign(&key);
    let mut bytes = pdu.encode();
    StatusPdu::fill_checksum(&mut bytes);
    // The checksum, filled after signing, doesn't matter to the digest.
    assert!(StatusPdu::verify_received(&bytes, &key));
    assert!(!StatusPdu::verify_received(&bytes, &[0x43; 32]));
    // Every other byte does, reserved ones too.
    let mut changed = bytes;
    changed[201] ^= 1;
    assert!(!StatusPdu::verify_received(&changed, &key));
    assert!(!StatusPdu::verify_received(&bytes[..203], &key));
}

// RFC 9946 §5.3.1: the digest covers every byte but authDigest and
// checkSum, which count as zero. The expected digest is the HMAC of those
// bytes (hmac_rfc4231.rs checks it against RFC 4231), not our signing;
// each byte is changed in turn.
#[udpstp_req::req("RFC9946-5.3.2-2")]
#[test]
fn verification_covers_every_byte_but_the_digest_and_checksum() {
    let key = [0x42; 32];
    let mut bytes = sample().encode();
    bytes[168..200].fill(0);
    bytes[202..204].fill(0);
    let digest = crate::wire::auth::hmac_sha256(&key, &bytes);
    bytes[168..200].copy_from_slice(&digest);
    bytes[202..204].copy_from_slice(&[0x12, 0x34]);
    assert!(StatusPdu::verify_received(&bytes, &key));
    for at in 0..bytes.len() {
        let mut changed = bytes;
        changed[at] ^= 0x80;
        assert_eq!(
            StatusPdu::verify_received(&changed, &key),
            (202..204).contains(&at),
            "byte {at}"
        );
    }
}

// The same whichever field comes first in a layout: here a 2-byte
// checksum at 4 and the digest at 20.
#[test]
fn verification_takes_the_fields_in_either_order() {
    let key = [0x42; 32];
    let mut bytes: Vec<u8> = (0..64).collect();
    bytes[4..6].fill(0);
    bytes[20..52].fill(0);
    let digest = crate::wire::auth::hmac_sha256(&key, &bytes);
    bytes[20..52].copy_from_slice(&digest);
    bytes[4..6].copy_from_slice(&[0xAB, 0xCD]);
    let verifies = |b: &[u8]| crate::wire::auth::verify_received(&key, b, 20..52, 4..6);
    assert!(verifies(&bytes));
    for at in 0..bytes.len() {
        let mut changed = bytes.clone();
        changed[at] ^= 0x80;
        assert_eq!(verifies(&changed), (4..6).contains(&at), "byte {at}");
    }
}
