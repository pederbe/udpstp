//! Any datagram, as a Null Request PDU: decoding, authDigest verification and
//! checksum verification never panic, and a decoded PDU encodes to bytes
//! that decode to it again and carry a valid checksum once it is filled.
#![no_main]

use libfuzzer_sys::fuzz_target;
use udpstp_wire::{NullPdu, checksum};

fuzz_target!(|datagram: &[u8]| {
    let _ = NullPdu::verify_received(datagram, &[0x5a; 32]);
    let _ = checksum::control_valid(datagram);
    if let Ok(pdu) = NullPdu::decode(datagram) {
        let mut bytes = pdu.encode();
        assert_eq!(NullPdu::decode(&bytes), Ok(pdu));
        checksum::fill_control(&mut bytes);
        assert!(checksum::control_valid(&bytes));
    }
});
