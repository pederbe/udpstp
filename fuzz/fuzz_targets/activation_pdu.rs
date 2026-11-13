//! Any datagram, as a Activation PDU: decoding, authDigest verification and
//! checksum verification never panic, and a decoded PDU encodes to bytes
//! that decode to it again and carry a valid checksum once it is filled.
#![no_main]


#[path = "../../src/wire/mod.rs"]
#[allow(dead_code, unused_imports)]
mod wire;
use libfuzzer_sys::fuzz_target;
use crate::wire::{ActivationPdu, checksum};

fuzz_target!(|datagram: &[u8]| {
    let _ = ActivationPdu::verify_received(datagram, &[0x5a; 32]);
    let _ = checksum::control_valid(datagram);
    if let Ok(pdu) = ActivationPdu::decode(datagram) {
        let mut bytes = pdu.encode();
        assert_eq!(ActivationPdu::decode(&bytes), Ok(pdu));
        checksum::fill_control(&mut bytes);
        assert!(checksum::control_valid(&bytes));
    }
});
