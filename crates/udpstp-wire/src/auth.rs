//! HMAC-SHA-256, the key derivation function and the key types
//! (RFC 9946 §5.3, §5.4, §5.4.1).
//!
//! Key types wipe their bytes when dropped and never print them.

use std::fmt;

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::AUTH_DIGEST_LEN;

type HmacSha256 = Hmac<Sha256>;

/// HMAC-SHA-256 (RFC 6234 §6) of `data` under `key`.
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut mac = keyed(key);
    mac.update(data);
    mac.finalize().into_bytes().into()
}

fn keyed(key: &[u8]) -> HmacSha256 {
    HmacSha256::new_from_slice(key).expect("HMAC accepts keys of any length")
}

/// A long-lived Shared Key from the Key Table. It is only ever used as input
/// to the KDF (RFC 9946 §5.4). The bytes are used as they are, including any
/// zero bytes.
#[derive(Clone)]
pub struct SharedKey(Zeroizing<Vec<u8>>);

impl SharedKey {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(Zeroizing::new(bytes))
    }

    /// The Derived Keys for a Test Connection whose first Setup Request
    /// carried `first_setup_auth_unix_time`.
    pub fn derive(&self, first_setup_auth_unix_time: u32) -> DerivedKeys {
        derive_keys(&self.0, first_setup_auth_unix_time)
    }
}

impl fmt::Debug for SharedKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SharedKey(<redacted>)")
    }
}

/// The two keys derived for one Test Connection (RFC 9946 §5.4.1).
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct DerivedKeys {
    client: [u8; 32],
    server: [u8; 32],
}

impl DerivedKeys {
    /// The Client Authentication Key: authenticates PDUs sent by the Client.
    pub fn client(&self) -> &[u8; 32] {
        &self.client
    }

    /// The Server Authentication Key: authenticates PDUs sent by the Server.
    pub fn server(&self) -> &[u8; 32] {
        &self.server
    }
}

impl fmt::Debug for DerivedKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DerivedKeys(<redacted>)")
    }
}

/// The KDF label (RFC 9946 §5.4.1).
const LABEL: &[u8] = b"UDPSTP";

/// Total derived key material in bits: two 256-bit keys (RFC 9946 §5.4.1).
const OUTPUT_BITS: u32 = 512;

/// Derives the Client and Server Authentication Keys from a Shared Key and
/// the authUnixTime of the Test Connection's first Setup Request.
///
/// NIST SP 800-108 KDF in counter mode with HMAC-SHA-256 and a 32-bit
/// counter, producing 512 bits. Each block is
/// `HMAC(key, BE32(i) ‖ "UDPSTP" ‖ 0x00 ‖ decimal(authUnixTime) ‖ BE32(512))`,
/// the layout RFC 9946 Appendix A's OpenSSL call produces (ADR 0001). The
/// Shared Key is used as raw bytes, including any zero bytes.
pub fn derive_keys(shared_key: &[u8], first_setup_auth_unix_time: u32) -> DerivedKeys {
    let context = first_setup_auth_unix_time.to_string();
    let prf = keyed(shared_key);
    let mut keys = DerivedKeys {
        client: [0; 32],
        server: [0; 32],
    };
    // Each block goes straight into the key; the HMAC state (the `zeroize`
    // feature of `hmac`) and the output buffer are wiped.
    for (counter, key) in [(1u32, &mut keys.client), (2, &mut keys.server)] {
        let mut mac = prf.clone();
        mac.update(&counter.to_be_bytes());
        mac.update(LABEL);
        mac.update(&[0x00]);
        mac.update(context.as_bytes());
        mac.update(&OUTPUT_BITS.to_be_bytes());
        let mut block = mac.finalize().into_bytes();
        key.copy_from_slice(&block);
        block.as_mut_slice().zeroize();
    }
    keys
}

/// The authDigest of a PDU (RFC 9946 §5.3.1). `zeroed_pdu` is the whole PDU
/// with its authDigest and checkSum fields set to zero.
pub fn auth_digest(key: &[u8; 32], zeroed_pdu: &[u8]) -> [u8; AUTH_DIGEST_LEN] {
    hmac_sha256(key, zeroed_pdu)
}

/// Checks the authDigest of a PDU as received, in constant time: the HMAC
/// covers the entire PDU (RFC 9946 §5.3.1), reserved fields included, with
/// only authDigest and checkSum zeroed. `digest` and `checksum` are the
/// fields' byte ranges. The PDU is fed to the HMAC in pieces, zeros in
/// place of those two fields, so nothing is copied or allocated: a Data
/// Phase Engine verifies each Status PDU this way (ADR 0008).
pub fn verify_received(
    key: &[u8; 32],
    pdu: &[u8],
    digest: std::ops::Range<usize>,
    checksum: std::ops::Range<usize>,
) -> bool {
    let (Some(received), Some(_)) = (pdu.get(digest.clone()), pdu.get(checksum.clone())) else {
        return false;
    };
    let Ok(received) = <&[u8; AUTH_DIGEST_LEN]>::try_from(received) else {
        return false;
    };
    let zeroed = if digest.start <= checksum.start {
        [digest, checksum]
    } else {
        [checksum, digest]
    };
    const ZEROS: [u8; AUTH_DIGEST_LEN] = [0; AUTH_DIGEST_LEN];
    let mut mac = keyed(key);
    let mut at = 0;
    for field in zeroed {
        // Fields that overlap are zeroed once.
        let (start, end) = (field.start.max(at), field.end.max(at));
        mac.update(&pdu[at..start]);
        for chunk in (start..end).step_by(ZEROS.len()) {
            mac.update(&ZEROS[..ZEROS.len().min(end - chunk)]);
        }
        at = end;
    }
    mac.update(&pdu[at..]);
    mac.verify_slice(received).is_ok()
}

/// Checks a received authDigest in constant time.
pub fn verify_auth_digest(
    key: &[u8; 32],
    zeroed_pdu: &[u8],
    digest: &[u8; AUTH_DIGEST_LEN],
) -> bool {
    let mut mac = keyed(key);
    mac.update(zeroed_pdu);
    mac.verify_slice(digest).is_ok()
}
