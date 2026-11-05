//! Base64 as Python's `base64.b64encode` writes it (RFC 4648's standard
//! alphabet, `=` padding), for the instances' user data; and its strict
//! inverse, for reading user data back (the tests). A dozen lines where
//! the design keeps the `base64` crate out.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// `base64.b64encode(bytes).decode()`.
pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[(n >> (18 - 6 * i) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The bytes `text` encodes, where it is base64 in [`encode`]'s form:
/// the standard alphabet, whole groups of four, `=` only as padding at
/// the end.
pub fn decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for (g, group) in bytes.chunks(4).enumerate() {
        let last = g + 1 == bytes.len() / 4;
        let padding = group.iter().rev().take_while(|&&b| b == b'=').count();
        if padding > 2 || (padding > 0 && !last) {
            return None;
        }
        let mut n = 0u32;
        for (i, &b) in group[..4 - padding].iter().enumerate() {
            let value = ALPHABET.iter().position(|&a| a == b)? as u32;
            n |= value << (18 - 6 * i);
        }
        let take = 3 - padding;
        out.extend((0..take).map(|i| (n >> (16 - 8 * i)) as u8));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_as_python_does() {
        // RFC 4648's test vectors.
        for (text, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(encode(text.as_bytes()), encoded);
            assert_eq!(decode(encoded).unwrap(), text.as_bytes());
        }
        let all: Vec<u8> = (0..=255).collect();
        assert_eq!(decode(&encode(&all)).unwrap(), all);
        assert!(encode(&[0xfb, 0xff]).contains('+') && encode(&[0xfb, 0xff]).contains('/'));
    }

    #[test]
    fn decodes_only_its_own_form() {
        for bad in ["Zg", "Zg=", "Z===", "Zg==Zg==", "Zm9v!A==", "Zm 9"] {
            assert_eq!(decode(bad), None, "{bad}");
        }
    }
}
