//! The Internet checksum (RFC 1071), used by UDPSTP's optional checkSum
//! field (RFC 9946 §5.6).

/// The one's complement of the one's complement sum of `data` as big-endian
/// 16-bit words, an odd final byte padded with zero (RFC 1071 §1).
pub fn internet_checksum(data: &[u8]) -> u16 {
    let (words, rest) = data.as_chunks::<2>();
    let mut sum: u32 = words
        .iter()
        .map(|w| u32::from(u16::from_be_bytes(*w)))
        .sum();
    if let [last] = rest {
        sum += u32::from(*last) << 8;
    }
    while sum > 0xFFFF {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

/// Where a control PDU's checkSum field is, by its pduId: the Setup (0xACE1),
/// Activation (0xACE2) and Null Request (0xDEAD) PDUs.
fn control_checksum_at(pdu: &[u8]) -> Option<usize> {
    match pdu.first_chunk::<2>().map(|id| u16::from_be_bytes(*id))? {
        crate::setup::SETUP_PDU_ID => Some(54),
        crate::activation::ACTIVATION_PDU_ID => Some(102),
        crate::null::NULL_PDU_ID => Some(46),
        _ => None,
    }
    .filter(|at| pdu.len() == at + 2)
}

/// Fills a control PDU's checkSum field as the last step of building it
/// (RFC 9946 §5.6), after signing: the checksum over the PDU with the field
/// zeroed, 0xFFFF for a sum of 0 so the field reads as populated. Other
/// PDUs are left as they are.
/// (conformance: zero-checksum-sent-as-ffff)
pub fn fill_control(pdu: &mut [u8]) {
    let Some(at) = control_checksum_at(pdu) else {
        return;
    };
    pdu[at..at + 2].fill(0);
    let sum = match internet_checksum(pdu) {
        0 => 0xFFFF,
        sum => sum,
    };
    pdu[at..at + 2].copy_from_slice(&sum.to_be_bytes());
}

/// Whether a control PDU's checkSum field matches it: the Internet
/// checksum over the whole PDU, the field included, is zero. Anything that
/// isn't a Setup, Activation or Null Request PDU of its size fails. Which
/// fields a receiver verifies at all is the protocol's to say (RFC 9946
/// §5.6: populated ones, where checksum processing is configured).
pub fn control_valid(pdu: &[u8]) -> bool {
    control_checksum_at(pdu).is_some() && internet_checksum(pdu) == 0
}
