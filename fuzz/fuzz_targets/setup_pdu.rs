//! Any datagram, as a Setup PDU: decoding, authDigest verification and
//! checksum verification never panic, and a decoded PDU encodes to bytes
//! that decode to it again and carry a valid checksum once it is filled.
#![no_main]


#[path = "../../src/wire/mod.rs"]
#[allow(dead_code, unused_imports)]
mod wire;
use libfuzzer_sys::fuzz_target;
use crate::wire::{SetupPdu, checksum};

fuzz_target!(|datagram: &[u8]| {
    let _ = SetupPdu::verify_received(datagram, &[0x5a; 32]);
    let _ = checksum::control_valid(datagram);
    if let Ok(pdu) = SetupPdu::decode(datagram) {
        let mut bytes = pdu.encode();
        assert_eq!(SetupPdu::decode(&bytes), Ok(pdu));
        checksum::fill_control(&mut bytes);
        assert!(checksum::control_valid(&bytes));
    }
});
