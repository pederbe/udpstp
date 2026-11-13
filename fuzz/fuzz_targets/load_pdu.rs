//! Any datagram, as a Load PDU: decoding its header and checksum
//! verification never panic, and a decoded header encodes to bytes that
//! decode to it again and carry a valid checksum once it is filled.
#![no_main]


#[path = "../../src/wire/mod.rs"]
#[allow(dead_code, unused_imports)]
mod wire;
use libfuzzer_sys::fuzz_target;
use crate::wire::load::LoadHeader;
use crate::wire::load::LOAD_HEADER_LEN;

fuzz_target!(|datagram: &[u8]| {
    let _ = LoadHeader::checksum_valid(datagram);
    if let Ok(header) = LoadHeader::decode(datagram) {
        // The payload content after the header is kept as it came.
        let mut bytes = datagram.to_vec();
        let encoded = bytes.first_chunk_mut::<LOAD_HEADER_LEN>().expect("decoded");
        header.encode_into(encoded);
        assert_eq!(LoadHeader::decode(&bytes), Ok(header));
        LoadHeader::fill_checksum(&mut bytes);
        assert!(LoadHeader::checksum_valid(&bytes));
    }
});
