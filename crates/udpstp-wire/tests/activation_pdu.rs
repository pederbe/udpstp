//! Test Activation PDU layout (RFC 9946 §7.1, Figures 6 and 7).
//!
//! The expected bytes are written out by hand from the RFC's field layout,
//! not produced by the codec, so the test can disagree with it.

use udpstp_wire::DecodeError;
use udpstp_wire::activation::{self, ActivationPdu, SendingRate};

fn sample() -> ActivationPdu {
    ActivationPdu {
        protocol_version: 20,
        cmd_request: activation::cmd_request::UPSTREAM,
        cmd_response: activation::cmd_response::ACK_OK,
        low_thresh: 0x001E,
        upper_thresh: 0x005A,
        trial_int: 0x0032,
        test_int_time: 0x000A,
        dscp_ecn: 0xB8,
        sr_index_conf: 0x1234,
        use_ow_del_var: 1,
        high_speed_delta: 10,
        slow_adj_thresh: 0x0003,
        seq_err_thresh: 0x000A,
        ignore_ooo_dup: 1,
        modifier_bitmap: activation::modifier::SR_INDEX_IS_START,
        rate_adj_algo: activation::rate_adj_algo::C,
        sending_rate: SendingRate {
            tx_interval1: 0x0000_0064,
            udp_payload1: 0x0000_04C6,
            burst_size1: 0x0000_0001,
            tx_interval2: 0x0000_03E8,
            udp_payload2: 0x0000_04C6,
            burst_size2: 0x0000_0002,
            udp_addon2: 0x0000_0100,
        },
        sub_int_period: 0x03E8,
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
        0xAC, 0xE2, // pduId
        0x00, 0x14, // protocolVer = 20
        0x01,       // cmdRequest = CHTA_CREQ_TESTACTUS
        0x01,       // cmdResponse = CHTA_CRSP_ACKOK
        0x00, 0x1E, // lowThresh = 30
        0x00, 0x5A, // upperThresh = 90
        0x00, 0x32, // trialInt = 50
        0x00, 0x0A, // testIntTime = 10
        0x00,       // reserved1
        0xB8,       // dscpEcn
        0x12, 0x34, // srIndexConf
        0x01,       // useOwDelVar
        0x0A,       // highSpeedDelta
        0x00, 0x03, // slowAdjThresh
        0x00, 0x0A, // seqErrThresh
        0x01,       // ignoreOooDup
        0x01,       // modifierBitmap = CHTA_SRIDX_ISSTART
        0x01,       // rateAdjAlgo = CHTA_RA_ALGO_C
        0x00,       // reserved2
        // srStruct
        0x00, 0x00, 0x00, 0x64, // txInterval1
        0x00, 0x00, 0x04, 0xC6, // udpPayload1
        0x00, 0x00, 0x00, 0x01, // burstSize1
        0x00, 0x00, 0x03, 0xE8, // txInterval2
        0x00, 0x00, 0x04, 0xC6, // udpPayload2
        0x00, 0x00, 0x00, 0x02, // burstSize2
        0x00, 0x00, 0x01, 0x00, // udpAddon2
        0x03, 0xE8, // subIntPeriod = 1000
        0x00, 0x00, // reserved3
        0x00, 0x00, // reserved4
        0x00,       // reserved5
        0x01,       // authMode
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

// Test Activation PDU layout (RFC 9946 Figures 6 and 7). The sample bytes
// carry every reserved field as 0.
#[udpstp_req::req("RFC9946-6.1-4", "RFC9946-7.1-7")]
#[test]
fn activation_pdu_encodes_to_the_rfc_layout() {
    assert_eq!(activation::ACTIVATION_PDU_LEN, 104);
    assert_eq!(sample().encode().as_slice(), sample_bytes().as_slice());
}

#[udpstp_req::req("RFC9946-7.1-7")]
#[test]
fn activation_pdu_decodes_from_the_rfc_layout() {
    assert_eq!(ActivationPdu::decode(&sample_bytes()), Ok(sample()));
}

// Applied to reserved1-5 and reservedAuth1 of the Test Activation PDU.
#[udpstp_req::req("RFC9946-6.1-4")]
#[test]
fn reserved_fields_are_ignored_on_receipt() {
    let mut bytes = sample_bytes();
    for offset in [14, 27, 58, 59, 60, 61, 62, 101] {
        bytes[offset] = 0xFF;
    }
    assert_eq!(ActivationPdu::decode(&bytes), Ok(sample()));
}

// The size must equal controlHdrTA; the PDU ID must be 0xACE2.
#[udpstp_req::req("RFC9946-7.2-2")]
#[test]
fn wrong_size_or_pdu_id_is_rejected() {
    let bytes = sample_bytes();
    assert_eq!(
        ActivationPdu::decode(&bytes[..103]),
        Err(DecodeError::WrongLength {
            expected: 104,
            actual: 103
        })
    );
    let mut wrong_id = bytes;
    wrong_id[1] = 0xE1;
    assert_eq!(
        ActivationPdu::decode(&wrong_id),
        Err(DecodeError::WrongPduId {
            expected: 0xACE2,
            actual: 0xACE1
        })
    );
}
