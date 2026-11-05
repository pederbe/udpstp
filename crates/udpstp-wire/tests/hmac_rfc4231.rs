//! HMAC-SHA-256 against the test cases of RFC 4231 §4.2-§4.8.
//!
//! `cargo xtask kat` extracts the vectors from the RFC text (they are not
//! computed), so they are an independent check. Case 5 specifies an output truncated to
//! 128 bits, so only its first 16 octets are compared.

mod common;

use common::unhex;
use udpstp_wire::auth::hmac_sha256;

const VECTORS: &str = include_str!("vectors/hmac-rfc4231.txt");

// The 32-octet HMAC-SHA-256 per RFC 6234 §6.
#[udpstp_req::req("RFC9946-5.3.1-3")]
#[test]
fn hmac_sha256_matches_every_rfc_4231_test_case() {
    let cases: Vec<Vec<&str>> = VECTORS
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| line.split_whitespace().collect())
        .collect();
    assert_eq!(cases.len(), 7, "RFC 4231 has seven test cases");
    for fields in cases {
        let [case, key, data, expected] = fields[..] else {
            panic!("malformed vector line: {fields:?}");
        };
        let mac = hmac_sha256(&unhex(key), &unhex(data));
        let expected = unhex(expected);
        assert_eq!(
            &mac[..expected.len()],
            expected.as_slice(),
            "RFC 4231 test case {case}"
        );
    }
}
