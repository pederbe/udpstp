//! Any datagram, as a Status PDU: decoding, authDigest verification and
//! checksum verification never panic, and a decoded PDU encodes to bytes
//! that decode to it again and carry a valid checksum once it is filled.
#![no_main]

use libfuzzer_sys::fuzz_target;
use udpstp_wire::StatusPdu;

fuzz_target!(|datagram: &[u8]| {
    let _ = StatusPdu::verify_received(datagram, &[0x5a; 32]);
    let _ = StatusPdu::checksum_valid(datagram);
    if let Ok(pdu) = StatusPdu::decode(datagram) {
        let mut bytes = pdu.encode();
        assert_eq!(StatusPdu::decode(&bytes), Ok(pdu));
        StatusPdu::fill_checksum(&mut bytes);
        assert!(StatusPdu::checksum_valid(&bytes));
    }
});
