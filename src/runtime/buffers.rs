//! Socket buffers for the Data Phase (docs/spec.md story 78): requested at the
//! configured size within the OS's limits, which the process can't raise
//! unprivileged, so a shortfall is reported with the limit to raise.

use std::io;
use std::net::{Ipv4Addr, UdpSocket};

use socket2::SockRef;

/// Each Test Connection's receive and send buffer unless configured: about
/// 16 ms at 1 Gbps, the Sending Rate Table's top row until it goes higher
///; OB-UDPST's sockets end up about as large.
pub const DEFAULT_SOCKET_BUFFER_SIZE: usize = 2 * 1024 * 1024;

/// The buffer sizes a socket got, in bytes, as the OS granted them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SocketBuffers {
    pub requested: usize,
    pub receive: usize,
    pub send: usize,
}

impl SocketBuffers {
    /// Whether the OS's limits held either buffer below the request.
    pub fn short(&self) -> bool {
        self.receive < self.requested || self.send < self.requested
    }
}

/// What to raise when buffers come out short, on this OS.
pub const SOCKET_BUFFER_LIMITS: &str = if cfg!(any(target_os = "linux", target_os = "android")) {
    "the net.core.rmem_max and net.core.wmem_max sysctls"
} else if cfg!(any(target_vendor = "apple", target_os = "freebsd")) {
    "the kern.ipc.maxsockbuf sysctl"
} else {
    "the OS's socket buffer limits"
};

/// Raises both of `socket`'s buffers to `size` where they are smaller,
/// never lowering one an administrator's defaults made larger, and
/// returns what it got. Linux caps a request at its limit; macOS refuses
/// one above it, so a refused request is retried at half the size until
/// one is granted or the buffer is already as large.
pub(crate) fn raise(socket: &SockRef<'_>, size: usize) -> io::Result<SocketBuffers> {
    raise_one(
        size,
        || socket.recv_buffer_size(),
        |s| socket.set_recv_buffer_size(s),
    )?;
    raise_one(
        size,
        || socket.send_buffer_size(),
        |s| socket.set_send_buffer_size(s),
    )?;
    Ok(SocketBuffers {
        requested: size,
        receive: granted(socket.recv_buffer_size()?),
        send: granted(socket.send_buffer_size()?),
    })
}

fn raise_one(
    size: usize,
    get: impl Fn() -> io::Result<usize>,
    set: impl Fn(usize) -> io::Result<()>,
) -> io::Result<()> {
    let current = granted(get()?);
    let mut ask = size;
    while ask > current {
        if set(ask).is_ok() {
            break;
        }
        ask /= 2;
    }
    Ok(())
}

/// Linux reports twice the size set, the rest for its bookkeeping
/// (socket(7)), which also takes some of the size set.
fn granted(reported: usize) -> usize {
    if cfg!(any(target_os = "linux", target_os = "android")) {
        reported / 2
    } else {
        reported
    }
}

/// What a Test Connection's socket would get for `size`, found on a
/// throwaway socket, so a shortfall can be reported at startup.
pub fn probe_socket_buffers(size: usize) -> io::Result<SocketBuffers> {
    let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .or_else(|_| UdpSocket::bind((std::net::Ipv6Addr::LOCALHOST, 0)))?;
    raise(&SockRef::from(&socket), size)
}

#[cfg(test)]
mod tests {
    use super::*;

    // docs/spec.md story 78 (buffers within the limits; shortfalls
    // visible);
    #[test]
    fn a_modest_request_is_granted_and_one_over_the_limit_falls_short() {
        let modest = probe_socket_buffers(64 * 1024).unwrap();
        assert!(!modest.short(), "{modest:?}");
        // 1 GiB is above Linux's and macOS's default limits; Windows has
        // none that low.
        let huge = probe_socket_buffers(1 << 30).unwrap();
        assert_eq!(huge.short(), cfg!(unix), "{huge:?}");
    }

    // : a buffer already larger than asked is left alone.
    #[test]
    fn a_larger_buffer_is_never_lowered() {
        let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let socket = SockRef::from(&socket);
        let before = raise(&socket, 128 * 1024).unwrap();
        let after = raise(&socket, 64 * 1024).unwrap();
        assert_eq!((after.receive, after.send), (before.receive, before.send));
    }
}
