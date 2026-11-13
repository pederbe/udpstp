//! The IP header fields test traffic is sent with: the Don't Fragment bit
//! (RFC 9946 §5: it SHOULD be set) and MaxHops, the IPv4 TTL or IPv6 hop
//! limit (RFC 9946 §10: the testing operator MUST set it).

use std::io;

use socket2::Socket;

/// MaxHops unless configured (docs/spec.md).
pub const DEFAULT_MAX_HOPS: u8 = 64;

/// How a socket's datagrams leave this host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IpOptions {
    /// Set DF on IPv4, and on IPv6 don't fragment locally. A datagram too
    /// large for the path is then lost rather than fragmented, and one too
    /// large for this host's interface fails to send and is counted as
    /// lost.
    pub dont_fragment: bool,
    /// The IPv4 TTL or IPv6 hop limit, 1 to 255.
    pub max_hops: u8,
}

impl Default for IpOptions {
    fn default() -> Self {
        Self {
            dont_fragment: true,
            max_hops: DEFAULT_MAX_HOPS,
        }
    }
}

/// Whether this OS lets the Don't Fragment setting be applied without
/// `unsafe`. On Windows it can't yet, and datagrams leave with the OS
/// default: fragmentation allowed.
pub const DONT_FRAGMENT_SUPPORTED: bool = cfg!(any(
    target_os = "linux",
    target_os = "android",
    target_vendor = "apple"
));

/// Whether this OS lets the DSCP be set. Windows documents `IP_TOS` as not
/// to be used, in favour of its QoS API, so test traffic there goes
/// unmarked.
pub const DSCP_SUPPORTED: bool = cfg!(unix);

/// Sets the DS field (IPv4 TOS, IPv6 Traffic Class) of an IPv4 or
/// IPv6-only `socket`'s datagrams to `dscp_ecn`, which the core gives with
/// ECN Not-ECT (RFC 9946 §7.1).
#[cfg(unix)]
pub(crate) fn set_ds_field(socket: &Socket, ipv4: bool, dscp_ecn: u8) -> io::Result<()> {
    if ipv4 {
        socket.set_tos_v4(dscp_ecn.into())
    } else {
        socket.set_tclass_v6(dscp_ecn.into())
    }
}

// Where the DSCP can't be set, test traffic goes unmarked and only DSCP 0
// is agreed. (conformance: windows-dscp-coerced-to-0)
#[cfg(not(unix))]
pub(crate) fn set_ds_field(_: &Socket, _: bool, _: u8) -> io::Result<()> {
    Ok(())
}

/// Applies `options` to an IPv4 or IPv6-only `socket`. On Linux it also
/// asks for ICMP errors on the socket's error queue (ADR 0006).
pub(crate) fn apply(socket: &Socket, ipv4: bool, options: IpOptions) -> io::Result<()> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    crate::runtime::socket::errqueue::enable(socket, ipv4)?;
    if ipv4 {
        socket.set_ttl_v4(options.max_hops.into())?;
    } else {
        socket.set_unicast_hops_v6(options.max_hops.into())?;
    }
    set_dont_fragment(socket, ipv4, options.dont_fragment)
}

/// The options `socket` has, as the OS reports them; `dont_fragment` is
/// `None` where it can't be read.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn read(socket: &Socket, ipv4: bool) -> io::Result<(Option<bool>, u8)> {
    let hops = if ipv4 {
        socket.ttl_v4()?
    } else {
        socket.unicast_hops_v6()?
    };
    let hops = u8::try_from(hops).map_err(|_| io::Error::other("hop limit out of range"))?;
    Ok((dont_fragment(socket, ipv4)?, hops))
}

// Linux: PROBE sets DF but ignores the cached Path MTU, so a real or
// spoofed ICMP "fragmentation needed" doesn't lower what may be sent, as it
// would with DO; datagrams too large for the path are lost on it, and the
// ICMP errors are reported from the error queue as Path MTU events.
// DONT neither sets DF nor refuses a send.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn set_dont_fragment(socket: &Socket, ipv4: bool, on: bool) -> io::Result<()> {
    use rustix::net::sockopt::{
        Ipv4PathMtuDiscovery, Ipv6PathMtuDiscovery, set_ip_mtu_discover, set_ipv6_mtu_discover,
    };
    if ipv4 {
        let value = if on {
            Ipv4PathMtuDiscovery::PROBE
        } else {
            Ipv4PathMtuDiscovery::DONT
        };
        set_ip_mtu_discover(socket, value)?;
    } else {
        let value = if on {
            Ipv6PathMtuDiscovery::PROBE
        } else {
            Ipv6PathMtuDiscovery::DONT
        };
        set_ipv6_mtu_discover(socket, value)?;
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn dont_fragment(socket: &Socket, ipv4: bool) -> io::Result<Option<bool>> {
    use rustix::net::sockopt::{
        Ipv4PathMtuDiscovery, Ipv6PathMtuDiscovery, ip_mtu_discover, ipv6_mtu_discover,
    };
    // WANT, the default, sets DF only on datagrams it needn't fragment.
    Ok(Some(if ipv4 {
        matches!(
            ip_mtu_discover(socket)?,
            Ipv4PathMtuDiscovery::DO | Ipv4PathMtuDiscovery::PROBE
        )
    } else {
        matches!(
            ipv6_mtu_discover(socket)?,
            Ipv6PathMtuDiscovery::DO | Ipv6PathMtuDiscovery::PROBE
        )
    }))
}

#[cfg(target_vendor = "apple")]
fn set_dont_fragment(socket: &Socket, ipv4: bool, on: bool) -> io::Result<()> {
    use nix::sys::socket::{setsockopt, sockopt};
    if ipv4 {
        setsockopt(socket, sockopt::IpDontFrag, &on)?;
    } else {
        setsockopt(socket, sockopt::Ipv6DontFrag, &on)?;
    }
    Ok(())
}

#[cfg(target_vendor = "apple")]
fn dont_fragment(socket: &Socket, ipv4: bool) -> io::Result<Option<bool>> {
    use nix::sys::socket::{getsockopt, sockopt};
    Ok(Some(if ipv4 {
        getsockopt(socket, sockopt::IpDontFrag)?
    } else {
        getsockopt(socket, sockopt::Ipv6DontFrag)?
    }))
}

// TODO(): set DF on Windows; no crate we allow offers it without
// `unsafe`. Until then datagrams leave without DF.
// (conformance: windows-dont-fragment-unset)
#[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
fn set_dont_fragment(_: &Socket, _: bool, _: bool) -> io::Result<()> {
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
fn dont_fragment(_: &Socket, _: bool) -> io::Result<Option<bool>> {
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    use crate::runtime::clock::SplitMix64;
    use crate::runtime::server::{bind_ephemeral_port, bind_std};

    #[udpstp_req::req("RFC9946-5-1", "RFC9946-10-3")]
    #[test]
    fn test_traffic_carries_df_and_max_hops() {
        for ip in [
            IpAddr::from(Ipv4Addr::LOCALHOST),
            IpAddr::from(Ipv6Addr::LOCALHOST),
        ] {
            // Neither is the OS default: Linux's is DF only where not
            // fragmenting, and 64.
            for options in [
                IpOptions {
                    dont_fragment: true,
                    max_hops: 9,
                },
                IpOptions {
                    dont_fragment: false,
                    max_hops: 7,
                },
            ] {
                let socket = match bind_std(ip, 0, options) {
                    Ok(socket) => socket,
                    // A host without IPv6.
                    Err(_) if ip.is_ipv6() => continue,
                    Err(error) => panic!("{ip}: {error}"),
                };
                let (df, hops) = read(&socket, ip.is_ipv4()).unwrap();
                assert_eq!(hops, options.max_hops, "{ip}");
                let expected = DONT_FRAGMENT_SUPPORTED.then_some(options.dont_fragment);
                assert_eq!(df, expected, "{ip}");
            }
        }
    }

    // The options reach a Test Connection's socket, and the Data Phase
    // Engine's handle to it: test traffic carries them.
    #[tokio::test]
    async fn a_test_connections_engine_socket_carries_them() {
        let options = IpOptions {
            dont_fragment: false,
            max_hops: 5,
        };
        let engine = bind_ephemeral_port(
            (Ipv4Addr::LOCALHOST, 0).into(),
            &(49152..=65535),
            options,
            &mut SplitMix64(1),
        )
        .unwrap()
        .engine;
        let (df, hops) = read(&socket2::SockRef::from(&engine), true).unwrap();
        assert_eq!(hops, 5);
        assert_eq!(df, DONT_FRAGMENT_SUPPORTED.then_some(false));
    }

    //
    #[udpstp_req::req("RFC9946-7.1-2")]
    #[cfg(unix)]
    #[test]
    fn the_ds_field_reads_back() {
        for ip in [
            IpAddr::from(Ipv4Addr::LOCALHOST),
            IpAddr::from(Ipv6Addr::LOCALHOST),
        ] {
            let Ok(socket) = bind_std(ip, 0, IpOptions::default()) else {
                assert!(ip.is_ipv6(), "{ip}");
                continue;
            };
            set_ds_field(&socket, ip.is_ipv4(), 46 << 2).unwrap();
            let read = if ip.is_ipv4() {
                socket.tos_v4().unwrap()
            } else {
                socket.tclass_v6().unwrap()
            };
            assert_eq!(read, 46 << 2, "{ip}");
        }
    }
}
