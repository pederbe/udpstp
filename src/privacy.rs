#![forbid(unsafe_code)]
//! How client addresses are shown in the Server's log and the Client's
//! text and JSON results (docs/spec.md story 82): in full, which is the default,
//! pseudonymized with a keyed hash, or truncated to their network.

use std::net::{IpAddr, SocketAddr};
use std::path::Path;

use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

/// How client addresses are shown.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AddressPrivacy {
    /// The address and port as they are.
    #[default]
    Full,
    /// `anon-` and the first 8 bytes of HMAC-SHA-256 over the IP address,
    /// in hex, keyed with the pseudonym key: one client keeps one pseudonym
    /// while the key is kept, and no one without it can reverse it.
    Pseudonymize,
    /// The IPv4 /24 or IPv6 /48 the address is in, without the port.
    Truncate,
}

/// The shortest pseudonym key, in bytes.
const MIN_KEY: usize = 16;

/// Shows addresses as configured.
pub struct Addresses {
    privacy: AddressPrivacy,
    key: Option<Zeroizing<Vec<u8>>>,
}

/// The key never shows.
impl std::fmt::Debug for Addresses {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Addresses")
            .field("privacy", &self.privacy)
            .finish_non_exhaustive()
    }
}

impl Addresses {
    /// Addresses in full.
    #[cfg(test)]
    pub fn full() -> Self {
        Self {
            privacy: AddressPrivacy::Full,
            key: None,
        }
    }

    /// Addresses shown as `privacy` says; pseudonyms need a key, read from
    /// `key_file` as hexadecimal digits (at least 16 bytes). A key isn't
    /// taken from the command line, where other users could see it.
    pub fn new(privacy: AddressPrivacy, key_file: Option<&Path>) -> Result<Self, String> {
        let key = match (privacy, key_file) {
            (AddressPrivacy::Pseudonymize, None) => {
                return Err(
                    "--address-privacy pseudonymize needs --address-privacy-key-file FILE".into(),
                );
            }
            (AddressPrivacy::Pseudonymize, Some(path)) => Some(read_key(path)?),
            (_, Some(_)) => {
                return Err(
                    "--address-privacy-key-file requires --address-privacy pseudonymize. Set it or remove --address-privacy-key-file".into(),
                );
            }
            (_, None) => None,
        };
        Ok(Self { privacy, key })
    }

    /// A client's address, as configured.
    pub fn show(&self, address: SocketAddr) -> String {
        match self.privacy {
            AddressPrivacy::Full => address.to_string(),
            _ => self.show_ip(address.ip()),
        }
    }

    /// A client's IP address, as configured.
    pub fn show_ip(&self, ip: IpAddr) -> String {
        let ip = ip.to_canonical();
        match (self.privacy, &self.key) {
            (AddressPrivacy::Full, _) => ip.to_string(),
            (AddressPrivacy::Truncate, _) => match ip {
                IpAddr::V4(v4) => {
                    let [a, b, c, _] = v4.octets();
                    format!("{a}.{b}.{c}.0/24")
                }
                IpAddr::V6(v6) => {
                    let prefix = u128::from(v6) & !((1u128 << 80) - 1);
                    format!("{}/48", std::net::Ipv6Addr::from(prefix))
                }
            },
            (AddressPrivacy::Pseudonymize, key) => {
                let octets = match ip {
                    IpAddr::V4(v4) => v4.octets().to_vec(),
                    IpAddr::V6(v6) => v6.octets().to_vec(),
                };
                let key = key.as_deref().map_or(&[][..], Vec::as_slice);
                let digest = crate::wire::auth::hmac_sha256(key, &octets);
                let hex: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
                format!("anon-{hex}")
            }
        }
    }
}

/// The pseudonym key file: hexadecimal digits, with whitespace around them
/// and nothing else. Every file valid now is read the same way by later
/// versions.
fn read_key(path: &Path) -> Result<Zeroizing<Vec<u8>>, String> {
    let text = Zeroizing::new(
        std::fs::read_to_string(path)
            .map_err(|e| format!("pseudonym key file {}: {e}", path.display()))?,
    );
    let hex = text.trim();
    if !hex.len().is_multiple_of(2) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!(
            "pseudonym key file {}: not hexadecimal digits",
            path.display()
        ));
    }
    let key: Zeroizing<Vec<u8>> = Zeroizing::new(
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("checked hex"))
            .collect(),
    );
    if key.len() < MIN_KEY {
        return Err(format!(
            "pseudonym key file {}: at least {MIN_KEY} bytes ({} hex digits)",
            path.display(),
            MIN_KEY * 2
        ));
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_key(privacy: AddressPrivacy, key: &[u8]) -> Addresses {
        Addresses {
            privacy,
            key: Some(Zeroizing::new(key.to_vec())),
        }
    }

    // docs/spec.md story 82: full, truncated or pseudonymized
    #[test]
    fn addresses_are_shown_as_configured() {
        let v4: SocketAddr = "192.0.2.77:40000".parse().unwrap();
        let v6: SocketAddr = "[2001:db8:1234:5678::9]:40000".parse().unwrap();
        let mapped: SocketAddr = "[::ffff:192.0.2.77]:40000".parse().unwrap();
        let full = Addresses::full();
        assert_eq!(full.show(v4), "192.0.2.77:40000");
        let truncate = Addresses {
            privacy: AddressPrivacy::Truncate,
            key: None,
        };
        assert_eq!(truncate.show(v4), "192.0.2.0/24");
        assert_eq!(truncate.show(v6), "2001:db8:1234::/48");
        assert_eq!(truncate.show(mapped), "192.0.2.0/24");
        let pseudonyms = with_key(AddressPrivacy::Pseudonymize, &[7; 16]);
        let p = pseudonyms.show(v4);
        assert!(p.starts_with("anon-") && p.len() == 21, "{p}");
        assert_eq!(
            p,
            pseudonyms.show("192.0.2.77:1".parse().unwrap()),
            "one client, one pseudonym"
        );
        assert_eq!(
            p,
            pseudonyms.show(mapped),
            "an IPv4-mapped address is the IPv4 one"
        );
        assert_ne!(p, pseudonyms.show("192.0.2.78:40000".parse().unwrap()));
        assert_ne!(
            p,
            with_key(AddressPrivacy::Pseudonymize, &[8; 16]).show(v4),
            "keyed"
        );
    }

    // , ADR 0013 (the pseudonym algorithm is stable: one key gives one
    // Client the same pseudonym in every version): known answers, computed
    // independently as HMAC-SHA-256(key, the address's octets)[..8].
    #[test]
    fn pseudonyms_keep_their_values() {
        let pseudonyms = with_key(AddressPrivacy::Pseudonymize, &[7; 16]);
        assert_eq!(
            pseudonyms.show("192.0.2.77:40000".parse().unwrap()),
            "anon-f092c12ea76e0f51"
        );
        assert_eq!(
            pseudonyms.show("[2001:db8:1234:5678::9]:40000".parse().unwrap()),
            "anon-e91ce0ee977d6b04"
        );
    }

    #[test]
    fn pseudonyms_need_a_key_of_16_bytes_in_hex() {
        let dir = std::env::temp_dir().join(format!("udpstp-privacy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, text: &str| {
            let path = dir.join(name);
            std::fs::write(&path, text).unwrap();
            path
        };
        assert!(Addresses::new(AddressPrivacy::Pseudonymize, None).is_err());
        let short = write("short", "00112233");
        assert!(Addresses::new(AddressPrivacy::Pseudonymize, Some(&short)).is_err());
        let not_hex = write("not-hex", &"zz".repeat(16));
        assert!(Addresses::new(AddressPrivacy::Pseudonymize, Some(&not_hex)).is_err());
        let good = write("good", &format!("{}\n", "ab".repeat(16)));
        assert!(Addresses::new(AddressPrivacy::Pseudonymize, Some(&good)).is_ok());
        assert!(Addresses::new(AddressPrivacy::Truncate, Some(&good)).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
