//! The socket error queue on Linux (ADR 0006): with IP_RECVERR or
//! IPV6_RECVERR set, the kernel queues each ICMP error a Test Connection's
//! datagrams provoke, with its type, and reports the error once on the
//! socket. The socket module drains the queue whenever a send or receive
//! fails that way, or a wait finds an error pending, so the Path MTU
//! (fragmentation needed, packet
//! too big) and a MaxHops too small for the path (time exceeded) are told
//! apart from other ICMP errors (`socket::classify::queued`), and the queue
//! can't fill up.

use std::io::IoSliceMut;
use std::mem::size_of;
use std::os::fd::{AsFd, AsRawFd};

use crate::protocol::TransportError;
use nix::libc;
use nix::sys::socket::{
    ControlMessageOwned, MsgFlags, SockaddrStorage, recvmsg, setsockopt, sockopt,
};

use crate::runtime::socket::classify::queued;

/// Room for the control messages one queued entry comes with: with the
/// Data Phase Engine's receive timestamps and TOS on, a queued error
/// carries those too, before the error itself, and a send timestamp its
/// scm_timestamping.
const CONTROL: usize = space(size_of::<libc::sock_extended_err>())
    + space(size_of::<libc::sockaddr_in6>())
    + space(size_of::<[libc::timespec; 3]>())
    + space(size_of::<libc::c_int>())
    + space(size_of::<libc::c_int>())
    + space(size_of::<libc::in6_pktinfo>());

/// The room a control message of `len` bytes of data takes (CMSG_SPACE):
/// its header and its data, each padded to a size_t.
const fn space(len: usize) -> usize {
    let word = size_of::<usize>();
    size_of::<libc::cmsghdr>().next_multiple_of(word) + len.next_multiple_of(word)
}

/// A control buffer on the stack, aligned as struct cmsghdr needs: the
/// queue is drained at every wait while kernel send timestamps are asked
/// for, and the Data Phase Engine's steady state allocates nothing (ADR
/// 0008).
#[repr(C, align(8))]
struct Control([u8; CONTROL]);

/// Asks the kernel to queue ICMP errors on `socket`.
pub(crate) fn enable(socket: &impl AsFd, ipv4: bool) -> std::io::Result<()> {
    if ipv4 {
        setsockopt(socket, sockopt::Ipv4RecvErr, &true)?;
    } else {
        setsockopt(socket, sockopt::Ipv6RecvErr, &true)?;
    }
    Ok(())
}

/// What the socket's error queue held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Queued {
    /// An error about a datagram sent.
    Error(TransportError),
    /// The kernel sent message `key` at `wall`, by its wall clock: a kernel
    /// send timestamp.
    Sent { key: u32, wall: std::time::Duration },
}

/// The errors queued on `socket`, oldest first, until the queue is empty;
/// kernel send timestamps queued among them are dropped.
pub(crate) fn drain(socket: &impl AsFd) -> Vec<TransportError> {
    let mut errors = Vec::new();
    drain_with(socket, |queued| {
        if let Queued::Error(error) = queued {
            errors.push(error);
        }
    });
    errors
}

/// Hands what is queued on `socket` to `each`, oldest first, until the
/// queue is empty: whether anything was.
pub(crate) fn drain_with(socket: &impl AsFd, mut each: impl FnMut(Queued)) -> bool {
    /// sock_extended_err's origin for a timestamp (SO_EE_ORIGIN_TIMESTAMPING).
    const ORIGIN_TIMESTAMPING: u8 = 4;
    let fd = socket.as_fd().as_raw_fd();
    let mut any = false;
    // A queued error carries the start of the datagram that provoked it,
    // which isn't needed.
    let mut data = [0u8; 64];
    let mut control = Control([0; CONTROL]);
    // Bounded, in case errors keep arriving while it drains.
    for _ in 0..64 {
        let mut iov = [IoSliceMut::new(&mut data)];
        let Ok(message) = recvmsg::<SockaddrStorage>(
            fd,
            &mut iov,
            Some(&mut control.0),
            MsgFlags::MSG_ERRQUEUE | MsgFlags::MSG_DONTWAIT,
        ) else {
            break;
        };
        any = true;
        let Ok(cmsgs) = message.cmsgs() else {
            break;
        };
        let (mut wall, mut key) = (None, None);
        for cmsg in cmsgs {
            let (error, ipv6) = match cmsg {
                ControlMessageOwned::ScmTimestampsns(stamps) => {
                    wall = Some(std::time::Duration::from(stamps.system));
                    continue;
                }
                ControlMessageOwned::Ipv4RecvErr(error, _) => (error, false),
                ControlMessageOwned::Ipv6RecvErr(error, _) => (error, true),
                _ => continue,
            };
            if error.ee_origin == ORIGIN_TIMESTAMPING {
                key = Some(error.ee_data);
            } else {
                each(Queued::Error(queued(&error, ipv6)));
            }
        }
        // The timestamp and its message's number come in two control
        // messages, in either order.
        if let (Some(key), Some(wall)) = (key, wall.filter(|w| !w.is_zero())) {
            each(Queued::Sent { key, wall });
        }
    }
    any
}

#[cfg(test)]
mod tests {
    use super::*;

    // ADR 0008: the control buffer on the stack has the room nix's
    // cmsg_space! allocates for the same control messages, by the libc
    // crate's CMSG_SPACE
    #[test]
    fn the_control_buffer_has_cmsg_spaces_room() {
        let room = nix::cmsg_space!(
            libc::sock_extended_err,
            libc::sockaddr_in6,
            [libc::timespec; 3],
            libc::c_int,
            libc::c_int,
            libc::in6_pktinfo
        );
        assert_eq!(CONTROL, room.len());
    }
}
