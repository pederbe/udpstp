//! The checkSum field of the control PDUs (RFC 9946 §5.6): filled last with
//! the field zeroed, and matched against the PDU.

use udpstp_wire::checksum::{control_valid, fill_control, internet_checksum};

/// A 56-byte Setup PDU whose words sum to 0xFFFF, so its checksum computes
/// to 0: 0xACE1 + 0x531E = 0xFFFF.
fn summing_to_ffff() -> Vec<u8> {
    let mut b = vec![0u8; 56];
    b[0..2].copy_from_slice(&[0xAC, 0xE1]);
    b[2..4].copy_from_slice(&0x531Eu16.to_be_bytes());
    b
}

// A populated checkSum: a sum of 0 is sent as 0xFFFF, its one's complement
// equal, so the field never reads as unpopulated.
#[udpstp_req::req("RFC9946-5.6-3")]
#[test]
fn a_zero_checksum_is_sent_as_ffff() {
    let mut pdu = summing_to_ffff();
    assert_eq!(internet_checksum(&pdu), 0);
    fill_control(&mut pdu);
    assert_eq!(pdu[54..56], [0xFF, 0xFF]);
    assert!(control_valid(&pdu));
}

// A filled field matches its PDU and a corrupted PDU doesn't; an
// unpopulated one matches only by chance. Which fields a receiver verifies
// is the core's rule (RFC 9946 §5.6; crates/udpstp-core/tests/
// control_checksum.rs).
#[test]
fn a_filled_field_matches_and_a_corrupted_pdu_does_not() {
    let mut pdu = summing_to_ffff();
    pdu[10] = 1;
    assert!(!control_valid(&pdu), "unpopulated, and not summing to zero");
    fill_control(&mut pdu);
    assert!(control_valid(&pdu));
    pdu[11] ^= 1;
    assert!(!control_valid(&pdu), "corrupted");
    // Activation (0xACE2, 104 bytes) and Null Request (0xDEAD, 48 bytes)
    // PDUs have the field at their ends.
    for (id, len) in [([0xAC, 0xE2], 104), ([0xDE, 0xAD], 48)] {
        let mut pdu = vec![0u8; len];
        pdu[..2].copy_from_slice(&id);
        pdu[7] = 9;
        fill_control(&mut pdu);
        assert_ne!(pdu[len - 2..], [0, 0]);
        assert_eq!(internet_checksum(&pdu), 0);
    }
}
