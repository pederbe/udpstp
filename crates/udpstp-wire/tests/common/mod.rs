//! Helpers shared by the wire known-answer tests.

/// Decodes a hex string (no separators) into bytes.
pub fn unhex(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2), "odd-length hex");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex"))
        .collect()
}
