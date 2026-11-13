//! Shared building blocks for the Setup Request flood tools: a
//! private-destination guard, Setup and Activation Request builders on
//! udpstp-wire's encoding, a pool of source sockets and a rate-bounded
//! sender.
//!
//! These are testing tools for one's own Server. The generator
//! (`setup_flood`) and the harness (`flood_cost`) both include this file.
//! Everything here is deliberately safe by default: a destination must be
//! private unless the caller overrides it, and every rate is bounded by
//! [`MAX_RATE_PPS`].

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::wire::activation::{
    self, ACTIVATION_PDU_LEN, ActivationPdu, SR_INDEX_DEFAULT, SendingRate, cmd_request as act_cmd,
    rate_adj_algo,
};
use crate::wire::setup::{SETUP_PDU_LEN, SetupPdu, cmd_request as setup_cmd, modifier};
use crate::wire::{AUTH_DIGEST_LEN, PROTOCOL_VERSION, auth_mode};

/// The most datagrams a second any one flood run may offer, whatever the
/// caller asks for: the flood stays bounded even when misconfigured. About
/// 1 Mpps of 84-byte IPv4 Setup Requests is roughly 670 Mbit/s, far above
/// any rate this first pass measures.
pub const MAX_RATE_PPS: u64 = 1_000_000;

/// The kind of Setup Request a flood sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// authDigest all zero: fails the digest, so the Server drops it after a
    /// KDF run and an HMAC (an off-path attacker without the key).
    ForgedUnsigned,
    /// authDigest present but not the HMAC under any key the Server holds:
    /// the same cost, from a sender that looks like it signed.
    ForgedWrongKey,
    /// Authentic, signed with a real Shared Key, but never activated: each
    /// costs the Server an Ephemeral Port bind, a Setup Response, a Null
    /// Request and the eviction of an Unactivated Test Connection, the
    /// oldest of the Key Table row holding the most
    ///. A key holder can do this from many spoofed Sources.
    KeyHolder,
}

impl Kind {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "forged-unsigned" => Ok(Self::ForgedUnsigned),
            "forged-wrong-key" => Ok(Self::ForgedWrongKey),
            "key-holder" => Ok(Self::KeyHolder),
            other => Err(format!(
                "unknown kind {other}: forged-unsigned, forged-wrong-key or key-holder"
            )),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::ForgedUnsigned => "forged-unsigned",
            Self::ForgedWrongKey => "forged-wrong-key",
            Self::KeyHolder => "key-holder",
        }
    }

    /// Whether the flood needs the Shared Key (only the key-holder flood
    /// signs authentically).
    pub fn needs_key(self) -> bool {
        self == Self::KeyHolder
    }
}

/// A Shared Key and its keyId: the raw bytes, so the Derived Keys can be
/// recomputed as the second (authUnixTime) rolls over.
#[derive(Clone)]
pub struct KeyMaterial {
    pub shared: crate::wire::auth::SharedKey,
    pub key_id: u8,
}

/// Parses a hex string (an even number of hex digits) into bytes.
pub fn parse_hex(hex: &str) -> Result<Vec<u8>, String> {
    let hex = hex.trim();
    if hex.is_empty() || !hex.len().is_multiple_of(2) {
        return Err("expected an even, non-zero number of hex digits".into());
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

/// The current wall-clock second, as authUnixTime (RFC 9946 §5.3.1).
pub fn now_unix() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as u32)
}

/// Whether `ip` is a private or loopback address the flood may target
/// without an override: loopback, RFC 1918, IPv4 link-local, IPv6 ULA
/// (fc00::/7) or IPv6 link-local (fe80::/10).
pub fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            let head = v6.segments()[0];
            v6.is_loopback() || (head & 0xfe00) == 0xfc00 || (head & 0xffc0) == 0xfe80
        }
    }
}

/// Builds a Setup Request of `kind`, made unique by `nonce`: its low 16 bits
/// (forced odd, never zero) become mcIdent, and 15 more bits become
/// maxBandwidth (Downstream). Both are covered by the authDigest, so a
/// distinct `nonce` gives a distinct verified request that the Server's
/// replay cache hasn't seen -- 2^31 of them, enough for any run. maxBandwidth
/// matters only under an Admitted Rate Cap, which the flood-cost Server
/// leaves unset. For [`Kind::KeyHolder`] `client_key` is the Client
/// Authentication Key derived from the Shared Key and `auth_unix_time`; the
/// request is signed with it. Otherwise the digest is left failing.
pub fn encode_setup(
    kind: Kind,
    nonce: u32,
    auth_unix_time: u32,
    key_id: u8,
    client_key: Option<&[u8; 32]>,
) -> [u8; SETUP_PDU_LEN] {
    let mut pdu = SetupPdu {
        protocol_version: PROTOCOL_VERSION,
        mc_index: 0,
        mc_count: 1,
        // Non-zero and varied, so each key-holder request is a fresh Test
        // with a fresh authDigest the replay cache hasn't seen. 15 nonce bits
        // go here (shifted up so the odd, never-zero low bit costs no
        // entropy) and 15 more into maxBandwidth: 2^30 distinct requests,
        // none colliding on the digest (the replay cache keys a Setup on
        // keyId and digest alone, sender excluded).
        mc_ident: (((nonce & 0x7FFF) as u16) << 1) | 1,
        cmd_request: setup_cmd::SETUP_REQUEST,
        cmd_response: 0,
        // More varying bits for the digest; the high bit (Upstream) stays 0.
        max_bandwidth: ((nonce >> 15) as u16) & 0x7FFF,
        test_port: 0,
        // The Server's default serves CHSR_JUMBO_STATUS.
        modifier_bitmap: modifier::JUMBO_STATUS,
        auth_mode: auth_mode::MODE_1,
        auth_unix_time,
        auth_digest: match kind {
            // A digest that is not the HMAC under any key the Server holds.
            Kind::ForgedUnsigned => [0; AUTH_DIGEST_LEN],
            Kind::ForgedWrongKey => [0xA5; AUTH_DIGEST_LEN],
            Kind::KeyHolder => [0; AUTH_DIGEST_LEN],
        },
        key_id,
        checksum: 0,
    };
    if kind == Kind::KeyHolder
        && let Some(key) = client_key
    {
        pdu.sign(key);
    }
    pdu.encode()
}

/// Builds a valid, signed Setup Request for a legitimate probe: a
/// single-connection Test made unique by `nonce`, signed in mode 1.
pub fn probe_setup(
    nonce: u32,
    auth_unix_time: u32,
    client_key: &[u8; 32],
    key_id: u8,
) -> [u8; SETUP_PDU_LEN] {
    encode_setup(
        Kind::KeyHolder,
        nonce,
        auth_unix_time,
        key_id,
        Some(client_key),
    )
}

/// Builds a valid, signed Activation Request for a probe, with the RFC 9946
/// default parameters so the Server accepts it with ACK_OK. `upstream`
/// chooses the direction (Upstream, so the Server sends no load and simply
/// waits, keeps the Data Phase cheap on the host).
pub fn probe_activation(
    upstream: bool,
    auth_unix_time: u32,
    client_key: &[u8; 32],
    key_id: u8,
) -> [u8; ACTIVATION_PDU_LEN] {
    let mut pdu = ActivationPdu {
        protocol_version: PROTOCOL_VERSION,
        cmd_request: if upstream {
            act_cmd::UPSTREAM
        } else {
            act_cmd::DOWNSTREAM
        },
        cmd_response: 0,
        low_thresh: 30,
        upper_thresh: 90,
        trial_int: 50,
        test_int_time: 10,
        dscp_ecn: 0,
        sr_index_conf: SR_INDEX_DEFAULT,
        use_ow_del_var: 1,
        high_speed_delta: 10,
        slow_adj_thresh: 3,
        seq_err_thresh: 10,
        ignore_ooo_dup: 1,
        modifier_bitmap: 0,
        rate_adj_algo: rate_adj_algo::B,
        sending_rate: SendingRate::default(),
        sub_int_period: 1000,
        auth_mode: auth_mode::MODE_1,
        auth_unix_time,
        auth_digest: [0; AUTH_DIGEST_LEN],
        key_id,
        checksum: 0,
    };
    let _ = activation::ACTIVATION_PDU_ID;
    pdu.sign(client_key);
    pdu.encode()
}

/// The loopback addresses the flood's Sources bind, in order: 127.0.0.2
/// upward to 127.255.255.254, skipping a low octet of 0 or 255, which some
/// stacks refuse to bind. The probe's pool (127.210.x) lies far above any
/// flood's.
pub fn source_addresses() -> impl Iterator<Item = Ipv4Addr> {
    (2u32..0x00FF_FFFF)
        .map(|host| Ipv4Addr::from(0x7F00_0000 | host))
        .filter(|ip| !matches!(ip.octets()[3], 0 | 255))
}

/// Binds `count` source sockets, each to a distinct 127.x address from
/// [`source_addresses`] so the Server sees each as its own Source (an IPv4
/// address), and connects each to `target` for fast sending. Only loopback
/// is used, so nothing leaves the host.
///
/// An address the host won't bind is skipped; any other failure ends the
/// binding with an error. Each Source is an open socket, so a `count` above
/// the open-file limit fails here, with a message naming `ulimit -n`, rather
/// than looping over addresses that can never bind.
pub fn bind_sources(count: usize, target: SocketAddr) -> io::Result<Vec<UdpSocket>> {
    let mut sockets = Vec::with_capacity(count);
    for ip in source_addresses() {
        if sockets.len() == count {
            return Ok(sockets);
        }
        let socket = match UdpSocket::bind(SocketAddr::from((ip, 0))) {
            Ok(socket) => socket,
            // A 127.x address the host won't bind: skip it.
            Err(error) if error.kind() == io::ErrorKind::AddrNotAvailable => continue,
            Err(error) => {
                return Err(io::Error::new(
                    error.kind(),
                    format!(
                        "binding Source {} of {count} ({ip}): {error}; each Source is an open socket, so the open-file limit (ulimit -n) must exceed {count}",
                        sockets.len() + 1
                    ),
                ));
            }
        };
        socket.connect(target)?;
        sockets.push(socket);
    }
    if sockets.len() == count {
        Ok(sockets)
    } else {
        Err(io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            format!(
                "only {} of {count} loopback Source addresses would bind",
                sockets.len()
            ),
        ))
    }
}

/// How one flood sender thread runs.
pub struct FloodConfig {
    pub kind: Kind,
    /// The keyId every Setup Request names. Name one the Server holds so a
    /// forged flood reaches the digest check (a KDF run and an HMAC) rather
    /// than being dropped at the keyId scan.
    pub key_id: u8,
    /// The Shared Key a key holder's flood signs with; its keyId must be
    /// `key_id`. `None` for a forged flood.
    pub key: Option<KeyMaterial>,
    /// The datagrams a second the thread aims for, at most [`MAX_RATE_PPS`].
    pub rate_pps: u64,
    /// Offsets the varied mcIdent so several threads don't collide.
    pub mc_seed: u32,
}

/// Runs one flood sender thread until `until`, sending Setup Requests
/// round-robin over `sources` (each a connected socket) as `config` says.
/// Returns how many datagrams it sent (none with no `sources`). Stops early
/// if `stop` is set.
///
/// A forged flood varies authUnixTime into the past, a fresh value each
/// datagram, so the Server's Derived Key cache misses every time and each
/// datagram within the budget costs a KDF run: the worst case. A key
/// holder's flood uses the current second, within the ±5 s window, so its
/// requests verify and admit.
///
/// The rate is a ceiling: the thread never sends faster than `config.rate_pps`
/// (bounded by [`MAX_RATE_PPS`]), and reports how many it actually managed.
pub fn flood_until(
    sources: &[UdpSocket],
    config: &FloodConfig,
    until: Instant,
    stop: &AtomicBool,
) -> u64 {
    if sources.is_empty() {
        return 0;
    }
    let kind = config.kind;
    let key_id = config.key_id;
    let key = config.key.as_ref();
    let rate = config.rate_pps.clamp(1, MAX_RATE_PPS);
    let start = Instant::now();
    let mut sent: u64 = 0;
    // A wide per-datagram nonce (mcIdent plus maxBandwidth bits), disjoint
    // across threads by `mc_seed`, so verified requests never collide.
    let mut mc: u32 = config.mc_seed;
    let mut source = 0usize;

    // A key holder's Derived Keys for the current second, recomputed as it
    // rolls over so the flood keeps hitting the Server's Derived Key cache.
    let mut second = now_unix();
    let mut client_key = key.map(|k| *k.shared.derive(second).client());
    // A forged flood's stale authUnixTime, a fresh value each datagram.
    let mut stale = now_unix().wrapping_sub(3600);

    while Instant::now() < until {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        // How many should have been sent by now to hold the rate.
        let elapsed = start.elapsed().as_secs_f64();
        let target = (elapsed * rate as f64) as u64;
        if sent >= target {
            // Ahead of schedule: wait for the next slot rather than spin.
            let ahead = sent + 1 - target;
            let wait = Duration::from_secs_f64(ahead as f64 / rate as f64);
            std::thread::sleep(wait.min(Duration::from_millis(2)));
            continue;
        }
        // Send a small batch before checking the clock again.
        let batch = (target - sent).min(256);
        for _ in 0..batch {
            let (auth_unix_time, ckey) = if kind == Kind::KeyHolder {
                let now = now_unix();
                if now != second {
                    second = now;
                    client_key = key.map(|k| *k.shared.derive(second).client());
                }
                (second, client_key.as_ref())
            } else {
                stale = stale.wrapping_sub(1);
                (stale, None)
            };
            let datagram = encode_setup(kind, mc, auth_unix_time, key_id, ckey);
            // A connected socket: drop errors (a full send buffer, an
            // unreachable loopback address) and keep the rate honest by
            // counting only what the kernel took.
            if sources[source].send(&datagram).is_ok() {
                sent += 1;
            }
            source = (source + 1) % sources.len();
            mc = mc.wrapping_add(1);
        }
    }
    sent
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_addresses_start_at_127_0_0_2_and_skip_octets_0_and_255() {
        let first: Vec<Ipv4Addr> = source_addresses().take(256).collect();
        assert_eq!(first[0], Ipv4Addr::new(127, 0, 0, 2));
        assert_eq!(first[252], Ipv4Addr::new(127, 0, 0, 254));
        assert_eq!(first[253], Ipv4Addr::new(127, 0, 1, 1));
        assert!(first.iter().all(|ip| !matches!(ip.octets()[3], 0 | 255)));
    }

    #[test]
    fn source_addresses_end_within_127_slash_8() {
        let last = source_addresses().last();
        assert_eq!(last, Some(Ipv4Addr::new(127, 255, 255, 254)));
    }

    #[test]
    fn flood_until_with_no_sources_sends_nothing() {
        let config = FloodConfig {
            kind: Kind::ForgedUnsigned,
            key_id: 1,
            key: None,
            rate_pps: 1_000,
            mc_seed: 0,
        };
        let until = Instant::now() + Duration::from_millis(20);
        assert_eq!(flood_until(&[], &config, until, &AtomicBool::new(false)), 0);
    }
}
