#[test]
fn signed_null_request_round_trips() {
    use crate::wire::{NullPdu, PROTOCOL_VERSION, auth_mode, checksum, null};

    let key = [7u8; 32]; // a Server Authentication Key
    let mut pdu = NullPdu {
        protocol_version: PROTOCOL_VERSION,
        cmd_request: null::cmd_request::NULL_REQUEST,
        cmd_response: null::cmd_response::NONE,
        auth_mode: auth_mode::MODE_1,
        auth_unix_time: 1_790_000_000,
        auth_digest: [0; 32],
        key_id: 1,
        checksum: 0,
    };
    pdu.sign(&key);
    let mut bytes = pdu.encode();
    checksum::fill_control(&mut bytes);

    assert!(checksum::control_valid(&bytes));
    assert!(NullPdu::verify_received(&bytes, &key));
    let received = NullPdu::decode(&bytes).expect("a Null Request");
    assert_eq!(received.auth_digest, pdu.auth_digest);
    assert_ne!(received.checksum, 0);
}
