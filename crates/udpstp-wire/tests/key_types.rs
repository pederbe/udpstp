//! Key types never print key material (docs/spec.md, Key table: keys are
//! "never logged").

use udpstp_wire::auth::{SharedKey, derive_keys};

/// The first three bytes of a key as `{:?}` prints them in a list.
fn leak(bytes: &[u8]) -> String {
    format!("{:?}", &bytes[..3])
        .trim_matches(['[', ']'])
        .to_string()
}

#[test]
fn shared_key_debug_is_redacted() {
    let key = b"SECRET-KEY-BYTES";
    let printed = format!("{:?}", SharedKey::new(key.to_vec()));

    assert!(printed.contains("redacted"), "{printed}");
    assert!(
        !printed.contains("SECRET") && !printed.contains(&leak(key)),
        "{printed}"
    );
}

#[test]
fn derived_keys_debug_is_redacted() {
    let keys = derive_keys(b"SECRET-KEY-BYTES", 1_790_013_600);
    let printed = format!("{keys:?}");

    assert!(printed.contains("redacted"), "{printed}");
    assert!(!printed.contains(&leak(keys.client())), "{printed}");
    assert!(!printed.contains(&leak(keys.server())), "{printed}");
}
