//! Transport errors, classified once (ADR 0006): given the call that
//! failed on a socket and the error the OS returned, what it means and what
//! the caller does about it. Every error number an OS returns that the
//! runtime acts on is named here and nowhere else, in one classifier per
//! OS, each with a table test.
//!
//! The same number can mean different things on different calls:
//!
//! - EINVAL is a socket used wrongly on a plain send or read, which ends
//!   it; on Linux's batched calls and on ppoll it is the kernel refusing
//!   the call itself, which the Data Phase Engine then stops making; on a
//!   send whose messages carry the numbers of their kernel send timestamps
//!   it may be the kernel refusing the numbers.
//! - EMSGSIZE on a UDP GSO send says GSO can't be used on this path; on any
//!   other send, that the datagram is larger than may leave with DF set, a
//!   Path MTU event.
//! - An ICMP-type error (refused, unreachable) is about an earlier
//!   datagram: reported, and on a send the call is made once more, since
//!   the failed call consumed the error.
//!
//! An error without a number this OS's classifier names, such as one a
//! test makes from an [`io::ErrorKind`] alone, is classified by its kind.

use std::io;

use udpstp_core::TransportError;

/// A call on a socket that can fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Call {
    /// A Data Phase datagram sent on its own: a Load PDU or a Status PDU.
    Send,
    /// A sendmmsg call, with UDP GSO messages when `gso` (Linux).
    #[cfg(any(target_os = "linux", target_os = "android"))]
    SendBatch { gso: bool },
    /// A send, single or batched, whose messages carry the numbers of their
    /// kernel send timestamps (SCM_TS_OPT_ID), as the Linux adapter
    /// first makes it: EINVAL may be the kernel refusing the numbers, and
    /// otherwise as a single send. The adapter acts only on the numbers'
    /// refusal, and passes any other error on to the socket, which
    /// classifies it by the call it asked for.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    NumberedSend,
    /// A Data Phase datagram read on its own.
    Read,
    /// A recvmmsg call (Linux).
    #[cfg(any(target_os = "linux", target_os = "android"))]
    ReadBatch,
    /// A wait on the socket in ppoll between turns (Linux).
    #[cfg(any(target_os = "linux", target_os = "android"))]
    Wait,
    /// A Control Phase datagram sent.
    ControlSend,
    /// A Control Phase datagram read.
    ControlRead,
}

/// A fast path the Data Phase Engine stops using when the kernel refuses
/// it (Linux).
#[cfg_attr(not(any(target_os = "linux", target_os = "android")), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FastPath {
    /// UDP GSO: sendmmsg goes on without it.
    UdpGso,
    /// sendmmsg: one send per datagram instead.
    Sendmmsg,
    /// recvmmsg: one read per datagram instead.
    Recvmmsg,
    /// ppoll: the engine sleeps between turns instead.
    Ppoll,
    /// The numbers messages carry for their kernel send timestamps:
    /// the send is made again without them, and if that goes, messages go
    /// without them from then on and no more send lags are counted.
    CarriedNumbers,
}

/// What a failed call means, and what its caller does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    /// Make the call again: it was interrupted, or, on a read or a wait,
    /// failed for a reason of its own that the next call needn't share.
    Retry,
    /// The socket takes nothing more for now: on a send a full send buffer
    /// (EAGAIN, ENOBUFS), so what is left waits for the next turn (ADR
    /// 0008's backpressure); on a read, nothing more has arrived.
    Blocked,
    /// The path returned an ICMP-type error about an earlier datagram:
    /// report it as this kind, or as the socket's error queue says where
    /// there is one (Linux), and go on. A send is made once more, and only
    /// once, since the failed call consumed the error.
    PathError(TransportError),
    /// The datagram is larger than may leave this host with DF set
    /// (EMSGSIZE): a Path MTU event. It will never go, so it counts as
    /// sent, and is lost. Not told apart on the Unixes udpstp isn't built
    /// for.
    #[cfg_attr(
        not(any(
            target_os = "linux",
            target_os = "android",
            target_vendor = "apple",
            windows
        )),
        allow(dead_code)
    )]
    TooLarge,
    /// The kernel refuses this fast path: stop using it, now and from now
    /// on.
    #[cfg_attr(not(any(target_os = "linux", target_os = "android")), allow(dead_code))]
    Unsupported(FastPath),
    /// The socket is unusable, or used wrongly: give it up. Sending with an
    /// address on a connected socket (EISCONN) would otherwise look like
    /// backpressure and stall the Test silently.
    Fatal,
}

/// What `error`, returned by `call`, means (ADR 0006).
pub(crate) fn classify(call: Call, error: &io::Error) -> Action {
    os::classify(call, error)
}

/// A call interrupted before it did anything is made again, except a
/// single send: its datagram waits for the next turn, as when the send
/// buffer is full.
fn interrupted(call: Call) -> Action {
    match call {
        Call::Send | Call::ControlSend => Action::Blocked,
        #[cfg(any(target_os = "linux", target_os = "android"))]
        Call::NumberedSend => Action::Blocked,
        _ => Action::Retry,
    }
}

/// An error that means nothing more particular to `call`: a datagram that
/// didn't go waits for the next turn; a read or a wait that failed is made
/// again.
fn otherwise(call: Call) -> Action {
    match call {
        Call::Send | Call::ControlSend => Action::Blocked,
        #[cfg(any(target_os = "linux", target_os = "android"))]
        Call::SendBatch { .. } | Call::NumberedSend => Action::Blocked,
        _ => Action::Retry,
    }
}

/// EMSGSIZE, where it isn't a refused UDP GSO send: a datagram too large
/// to send, and on a read the read's own failure.
#[cfg_attr(
    not(any(
        target_os = "linux",
        target_os = "android",
        target_vendor = "apple",
        windows
    )),
    allow(dead_code)
)]
fn too_large(call: Call) -> Action {
    match call {
        Call::Send | Call::ControlSend => Action::TooLarge,
        #[cfg(any(target_os = "linux", target_os = "android"))]
        Call::SendBatch { .. } | Call::NumberedSend => Action::TooLarge,
        _ => Action::Retry,
    }
}

/// An error by its kind alone, as the standard library maps the OS's codes
/// to kinds.
fn by_kind(call: Call, error: &io::Error) -> Action {
    match error.kind() {
        io::ErrorKind::Interrupted => interrupted(call),
        io::ErrorKind::WouldBlock => Action::Blocked,
        io::ErrorKind::InvalidInput => Action::Fatal,
        io::ErrorKind::ConnectionRefused
        | io::ErrorKind::ConnectionReset
        | io::ErrorKind::HostUnreachable
        | io::ErrorKind::NetworkUnreachable => Action::PathError(TransportError::Icmp),
        _ => otherwise(call),
    }
}

/// Linux and macOS, by the codes' names: their numbers differ.
#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
mod os {
    use std::io;

    use nix::errno::Errno;
    use udpstp_core::TransportError;

    #[cfg(any(target_os = "linux", target_os = "android"))]
    use super::FastPath;
    use super::{Action, Call, by_kind, interrupted, otherwise, too_large};

    pub(super) fn classify(call: Call, error: &io::Error) -> Action {
        let errno = error.raw_os_error().map(Errno::from_raw);
        // The codes by which the kernel refuses a fast path: its own
        // calls, on Linux.
        #[cfg(any(target_os = "linux", target_os = "android"))]
        match call {
            // Any other failure is this wait's alone: a wake-up without
            // progress.
            Call::Wait => {
                return match errno {
                    Some(Errno::ENOSYS | Errno::EPERM | Errno::EINVAL) => {
                        Action::Unsupported(FastPath::Ppoll)
                    }
                    _ => Action::Retry,
                };
            }
            // The device can't offload the checksum (EIO), or the kernel or
            // the path refuses the segment size (EINVAL, EMSGSIZE, as when
            // the datagrams would need IP fragmentation), or doesn't know
            // GSO; without sendmmsg, nothing batched goes at all.
            Call::SendBatch { gso: true } => match errno {
                Some(
                    Errno::EIO
                    | Errno::EINVAL
                    | Errno::EMSGSIZE
                    | Errno::EOPNOTSUPP
                    | Errno::ENOPROTOOPT,
                ) => return Action::Unsupported(FastPath::UdpGso),
                Some(Errno::ENOSYS) => return Action::Unsupported(FastPath::Sendmmsg),
                _ => {}
            },
            // An old kernel or a sandbox without it (ENOSYS), or a socket
            // that refuses it.
            Call::SendBatch { gso: false } => {
                if let Some(Errno::ENOSYS | Errno::EINVAL | Errno::EIO | Errno::EOPNOTSUPP) = errno
                {
                    return Action::Unsupported(FastPath::Sendmmsg);
                }
            }
            Call::ReadBatch => {
                if let Some(Errno::ENOSYS | Errno::EPERM | Errno::EINVAL | Errno::EOPNOTSUPP) =
                    errno
                {
                    return Action::Unsupported(FastPath::Recvmmsg);
                }
            }
            // How the kernel answers a control message it doesn't take
            // (SCM_TS_OPT_ID before Linux 6.13, `__sock_cmsg_send`).
            Call::NumberedSend => {
                if let Some(Errno::EINVAL) = errno {
                    return Action::Unsupported(FastPath::CarriedNumbers);
                }
            }
            _ => {}
        }
        match errno {
            Some(Errno::EINTR) => interrupted(call),
            // EWOULDBLOCK is EAGAIN.
            Some(Errno::EAGAIN) => Action::Blocked,
            Some(Errno::ENOBUFS) => otherwise(call),
            Some(
                Errno::ECONNREFUSED | Errno::ECONNRESET | Errno::EHOSTUNREACH | Errno::ENETUNREACH,
            ) => Action::PathError(TransportError::Icmp),
            Some(Errno::EMSGSIZE) => too_large(call),
            Some(Errno::EINVAL | Errno::EBADF | Errno::EISCONN) => Action::Fatal,
            _ => by_kind(call, error),
        }
    }

    /// A full send buffer's error, as a test injects it.
    #[cfg(any(test, feature = "fault-injection"))]
    pub(super) const NO_BUFFERS: i32 = Errno::ENOBUFS as i32;
}

/// Windows: Winsock's codes.
#[cfg(windows)]
mod os {
    use std::io;

    use udpstp_core::TransportError;

    use super::{Action, Call, by_kind, interrupted, otherwise, too_large};

    const WSAEINTR: i32 = 10004;
    const WSAEINVAL: i32 = 10022;
    const WSAEWOULDBLOCK: i32 = 10035;
    const WSAENOTSOCK: i32 = 10038;
    const WSAEMSGSIZE: i32 = 10040;
    const WSAENETUNREACH: i32 = 10051;
    const WSAENETRESET: i32 = 10052;
    const WSAECONNRESET: i32 = 10054;
    const WSAENOBUFS: i32 = 10055;
    const WSAEISCONN: i32 = 10056;
    const WSAECONNREFUSED: i32 = 10061;
    const WSAEHOSTUNREACH: i32 = 10065;

    pub(super) fn classify(call: Call, error: &io::Error) -> Action {
        match error.raw_os_error() {
            Some(WSAEINTR) => interrupted(call),
            Some(WSAEWOULDBLOCK) => Action::Blocked,
            Some(WSAENOBUFS) => otherwise(call),
            // WSAECONNRESET is a port unreachable, on a UDP socket.
            Some(WSAECONNREFUSED | WSAECONNRESET | WSAEHOSTUNREACH | WSAENETUNREACH) => {
                Action::PathError(TransportError::Icmp)
            }
            // Windows' only report of an ICMP time exceeded.
            Some(WSAENETRESET) => Action::PathError(TransportError::TimeExceeded),
            Some(WSAEMSGSIZE) => too_large(call),
            Some(WSAEINVAL | WSAENOTSOCK | WSAEISCONN) => Action::Fatal,
            _ => by_kind(call, error),
        }
    }

    /// A full send buffer's error, as a test injects it.
    #[cfg(any(test, feature = "fault-injection"))]
    pub(super) const NO_BUFFERS: i32 = WSAENOBUFS;
}

/// Elsewhere: the Unixes udpstp isn't built for.
#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_vendor = "apple",
    windows
)))]
mod os {
    use std::io;

    use super::{Action, Call};

    pub(super) fn classify(call: Call, error: &io::Error) -> Action {
        super::elsewhere(call, error)
    }

    /// ENOBUFS as the BSDs number it.
    #[cfg(any(test, feature = "fault-injection"))]
    pub(super) const NO_BUFFERS: i32 = 55;
}

/// On a Unix udpstp isn't built for: by the errors' kinds, and by the
/// numbers the BSDs give EBADF and EISCONN, which the kinds don't name,
/// so that a socket used wrongly still ends rather than stalls, as before
/// the classifier. Compiled everywhere, so that its table test runs.
#[cfg_attr(
    any(
        target_os = "linux",
        target_os = "android",
        target_vendor = "apple",
        windows
    ),
    allow(dead_code)
)]
fn elsewhere(call: Call, error: &io::Error) -> Action {
    const EBADF: i32 = 9;
    const EISCONN: i32 = 56;
    match error.raw_os_error() {
        Some(EBADF | EISCONN) => Action::Fatal,
        _ => by_kind(call, error),
    }
}

/// What an error the socket's error queue held means (Linux, ADR 0006):
/// ICMP fragmentation needed and packet too big, and a local EMSGSIZE, are
/// Path MTU events; time exceeded a hop-limit event; anything else an ICMP
/// error.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub(crate) fn queued(error: &nix::libc::sock_extended_err, ipv6: bool) -> TransportError {
    const ORIGIN_LOCAL: u8 = 1;
    const ORIGIN_ICMP: u8 = 2;
    const ORIGIN_ICMP6: u8 = 3;
    // ICMP: destination unreachable, fragmentation needed; time exceeded.
    const ICMP_UNREACH: u8 = 3;
    const ICMP_FRAG_NEEDED: u8 = 4;
    const ICMP_TIME_EXCEEDED: u8 = 11;
    // ICMPv6: packet too big; time exceeded.
    const ICMP6_PACKET_TOO_BIG: u8 = 2;
    const ICMP6_TIME_EXCEEDED: u8 = 3;
    let local_too_large = error.ee_errno == nix::errno::Errno::EMSGSIZE as u32;
    match (error.ee_origin, error.ee_type, error.ee_code) {
        (ORIGIN_ICMP, ICMP_UNREACH, ICMP_FRAG_NEEDED) if !ipv6 => TransportError::MsgSize,
        (ORIGIN_ICMP, ICMP_TIME_EXCEEDED, _) if !ipv6 => TransportError::TimeExceeded,
        (ORIGIN_ICMP6, ICMP6_PACKET_TOO_BIG, _) => TransportError::MsgSize,
        (ORIGIN_ICMP6, ICMP6_TIME_EXCEEDED, _) => TransportError::TimeExceeded,
        (ORIGIN_LOCAL, _, _) if local_too_large => TransportError::MsgSize,
        _ => TransportError::Icmp,
    }
}

/// Errors as this OS returns them, for the faults tests inject.
#[cfg(any(test, feature = "fault-injection"))]
pub(crate) mod inject {
    use std::io;

    /// A full send buffer, which the queue discipline reports (ENOBUFS).
    pub(crate) fn no_buffers() -> io::Error {
        io::Error::from_raw_os_error(super::os::NO_BUFFERS)
    }

    /// A call the kernel doesn't have (ENOSYS).
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub(crate) fn not_implemented() -> io::Error {
        nix::errno::Errno::ENOSYS.into()
    }

    /// A device that can't offload a UDP GSO send's checksum (EIO).
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub(crate) fn device_error() -> io::Error {
        nix::errno::Errno::EIO.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ICMP: Action = Action::PathError(TransportError::Icmp);

    fn check<const N: usize>(calls: [Call; N], table: &[(i32, &str, [Action; N])]) {
        for (code, name, actions) in table {
            for (call, expected) in calls.iter().zip(actions) {
                let error = io::Error::from_raw_os_error(*code);
                assert_eq!(classify(*call, &error), *expected, "{name} on {call:?}");
            }
        }
    }

    // ADR 0006 (would-block and interrupted are retried, ICMP-type errors
    // reported and a send retried once, EMSGSIZE a Path MTU event, only
    // local errors fatal); , ,  (the codes that refuse GSO,
    // sendmmsg, recvmmsg and ppoll);  (the code by which the kernel
    // refuses the numbers messages carry): what each code means on each
    // call, by the codes' Linux numbers, written out rather than taken from
    // the classifier's names
    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn linux_errors_are_classified_by_call() {
        use Action::{Blocked, Fatal, Retry, TooLarge, Unsupported};
        use FastPath::{CarriedNumbers, Ppoll, Recvmmsg, Sendmmsg, UdpGso};
        const GSO: Action = Unsupported(UdpGso);
        const MMSG: Action = Unsupported(Sendmmsg);
        const RMMSG: Action = Unsupported(Recvmmsg);
        const PPOLL: Action = Unsupported(Ppoll);
        const NUMS: Action = Unsupported(CarriedNumbers);
        let calls = [
            Call::Send,
            Call::SendBatch { gso: false },
            Call::SendBatch { gso: true },
            Call::NumberedSend,
            Call::Read,
            Call::ReadBatch,
            Call::Wait,
            Call::ControlSend,
            Call::ControlRead,
        ];
        #[rustfmt::skip]
        let table = [
            //                   send     batch    GSO      numbered read     rbatch   wait     ctl send ctl read
            (4, "EINTR",        [Blocked, Retry,   Retry,   Blocked, Retry,   Retry,   Retry,   Blocked, Retry]),
            (11, "EAGAIN",      [Blocked, Blocked, Blocked, Blocked, Blocked, Blocked, Retry,   Blocked, Blocked]),
            (105, "ENOBUFS",    [Blocked, Blocked, Blocked, Blocked, Retry,   Retry,   Retry,   Blocked, Retry]),
            (111, "ECONNREFUSED", [ICMP,  ICMP,    ICMP,    ICMP,    ICMP,    ICMP,    Retry,   ICMP,    ICMP]),
            (104, "ECONNRESET", [ICMP,    ICMP,    ICMP,    ICMP,    ICMP,    ICMP,    Retry,   ICMP,    ICMP]),
            (113, "EHOSTUNREACH", [ICMP,  ICMP,    ICMP,    ICMP,    ICMP,    ICMP,    Retry,   ICMP,    ICMP]),
            (101, "ENETUNREACH", [ICMP,   ICMP,    ICMP,    ICMP,    ICMP,    ICMP,    Retry,   ICMP,    ICMP]),
            (90, "EMSGSIZE",    [TooLarge, TooLarge, GSO,   TooLarge, Retry,  Retry,   Retry,   TooLarge, Retry]),
            (22, "EINVAL",      [Fatal,   MMSG,    GSO,     NUMS,    Fatal,   RMMSG,   PPOLL,   Fatal,   Fatal]),
            (5, "EIO",          [Blocked, MMSG,    GSO,     Blocked, Retry,   Retry,   Retry,   Blocked, Retry]),
            (95, "EOPNOTSUPP",  [Blocked, MMSG,    GSO,     Blocked, Retry,   RMMSG,   Retry,   Blocked, Retry]),
            (92, "ENOPROTOOPT", [Blocked, Blocked, GSO,     Blocked, Retry,   Retry,   Retry,   Blocked, Retry]),
            (38, "ENOSYS",      [Blocked, MMSG,    MMSG,    Blocked, Retry,   RMMSG,   PPOLL,   Blocked, Retry]),
            (1, "EPERM",        [Blocked, Blocked, Blocked, Blocked, Retry,   RMMSG,   PPOLL,   Blocked, Retry]),
            (9, "EBADF",        [Fatal,   Fatal,   Fatal,   Fatal,   Fatal,   Fatal,   Retry,   Fatal,   Fatal]),
            (106, "EISCONN",    [Fatal,   Fatal,   Fatal,   Fatal,   Fatal,   Fatal,   Retry,   Fatal,   Fatal]),
            (100, "ENETDOWN",   [Blocked, Blocked, Blocked, Blocked, Retry,   Retry,   Retry,   Blocked, Retry]),
        ];
        check(calls, &table);
    }

    // ADR 0006, as above, by the codes' macOS numbers
    #[cfg(target_vendor = "apple")]
    #[test]
    fn macos_errors_are_classified_by_call() {
        use Action::{Blocked, Fatal, Retry, TooLarge};
        let calls = [Call::Send, Call::Read, Call::ControlSend, Call::ControlRead];
        #[rustfmt::skip]
        let table = [
            //                     send      read     ctl send  ctl read
            (4, "EINTR",          [Blocked,  Retry,   Blocked,  Retry]),
            (35, "EAGAIN",        [Blocked,  Blocked, Blocked,  Blocked]),
            (55, "ENOBUFS",       [Blocked,  Retry,   Blocked,  Retry]),
            (61, "ECONNREFUSED",  [ICMP,     ICMP,    ICMP,     ICMP]),
            (54, "ECONNRESET",    [ICMP,     ICMP,    ICMP,     ICMP]),
            (65, "EHOSTUNREACH",  [ICMP,     ICMP,    ICMP,     ICMP]),
            (51, "ENETUNREACH",   [ICMP,     ICMP,    ICMP,     ICMP]),
            (40, "EMSGSIZE",      [TooLarge, Retry,   TooLarge, Retry]),
            (22, "EINVAL",        [Fatal,    Fatal,   Fatal,    Fatal]),
            (9, "EBADF",          [Fatal,    Fatal,   Fatal,    Fatal]),
            (56, "EISCONN",       [Fatal,    Fatal,   Fatal,    Fatal]),
            (5, "EIO",            [Blocked,  Retry,   Blocked,  Retry]),
            (50, "ENETDOWN",      [Blocked,  Retry,   Blocked,  Retry]),
        ];
        check(calls, &table);
    }

    // ADR 0006, as above, by Winsock's codes: WSAENETRESET is Windows'
    // report of a time exceeded
    #[cfg(windows)]
    #[test]
    fn windows_errors_are_classified_by_call() {
        use Action::{Blocked, Fatal, Retry, TooLarge};
        const TIME: Action = Action::PathError(TransportError::TimeExceeded);
        let calls = [Call::Send, Call::Read, Call::ControlSend, Call::ControlRead];
        #[rustfmt::skip]
        let table = [
            //                          send      read     ctl send  ctl read
            (10004, "WSAEINTR",        [Blocked,  Retry,   Blocked,  Retry]),
            (10035, "WSAEWOULDBLOCK",  [Blocked,  Blocked, Blocked,  Blocked]),
            (10055, "WSAENOBUFS",      [Blocked,  Retry,   Blocked,  Retry]),
            (10061, "WSAECONNREFUSED", [ICMP,     ICMP,    ICMP,     ICMP]),
            (10054, "WSAECONNRESET",   [ICMP,     ICMP,    ICMP,     ICMP]),
            (10065, "WSAEHOSTUNREACH", [ICMP,     ICMP,    ICMP,     ICMP]),
            (10051, "WSAENETUNREACH",  [ICMP,     ICMP,    ICMP,     ICMP]),
            (10052, "WSAENETRESET",    [TIME,     TIME,    TIME,     TIME]),
            (10040, "WSAEMSGSIZE",     [TooLarge, Retry,   TooLarge, Retry]),
            (10022, "WSAEINVAL",       [Fatal,    Fatal,   Fatal,    Fatal]),
            (10038, "WSAENOTSOCK",     [Fatal,    Fatal,   Fatal,    Fatal]),
            (10056, "WSAEISCONN",      [Fatal,    Fatal,   Fatal,    Fatal]),
            (10050, "WSAENETDOWN",     [Blocked,  Retry,   Blocked,  Retry]),
        ];
        check(calls, &table);
    }

    // ADR 0006: an error with no code, as a test makes one, by its kind
    #[test]
    fn an_error_without_a_code_is_classified_by_its_kind() {
        use io::ErrorKind;
        let cases = [
            (ErrorKind::WouldBlock, Call::Send, Action::Blocked),
            (ErrorKind::WouldBlock, Call::Read, Action::Blocked),
            (ErrorKind::Interrupted, Call::Send, Action::Blocked),
            (ErrorKind::Interrupted, Call::Read, Action::Retry),
            (ErrorKind::InvalidInput, Call::Send, Action::Fatal),
            (ErrorKind::ConnectionRefused, Call::Read, ICMP),
            (ErrorKind::Other, Call::Send, Action::Blocked),
            (ErrorKind::Other, Call::Read, Action::Retry),
        ];
        for (kind, call, expected) in cases {
            assert_eq!(
                classify(call, &io::Error::from(kind)),
                expected,
                "{kind:?} on {call:?}"
            );
        }
    }

    // ADR 0006 (only local errors are fatal), on a Unix udpstp isn't built
    // for: a bad socket (EBADF, 9) and a send with an address on a
    // connected one (EISCONN, 56 on the BSDs) end it, as before the
    // classifier; the rest go by their kinds, as the standard library maps
    // them there
    #[test]
    fn elsewhere_a_socket_used_wrongly_ends_it() {
        use Action::{Blocked, Fatal, Retry};
        let by_number = [
            (9, "EBADF", [Fatal, Fatal, Fatal, Fatal]),
            (56, "EISCONN", [Fatal, Fatal, Fatal, Fatal]),
        ];
        let calls = [Call::Send, Call::Read, Call::ControlSend, Call::ControlRead];
        for (code, name, actions) in by_number {
            for (call, expected) in calls.iter().zip(actions) {
                let error = io::Error::from_raw_os_error(code);
                assert_eq!(elsewhere(*call, &error), expected, "{name} on {call:?}");
            }
        }
        let by_kind = [
            (io::ErrorKind::InvalidInput, [Fatal, Fatal, Fatal, Fatal]),
            (
                io::ErrorKind::WouldBlock,
                [Blocked, Blocked, Blocked, Blocked],
            ),
            (io::ErrorKind::ConnectionRefused, [ICMP, ICMP, ICMP, ICMP]),
            (io::ErrorKind::Other, [Blocked, Retry, Blocked, Retry]),
        ];
        for (kind, actions) in by_kind {
            for (call, expected) in calls.iter().zip(actions) {
                let error = io::Error::from(kind);
                assert_eq!(elsewhere(*call, &error), expected, "{kind:?} on {call:?}");
            }
        }
    }

    // ADR 0006: a datagram over the UDP maximum fails on every OS with the
    // error a DF datagram over the interface MTU gets, which is a Path MTU
    // event on a send
    #[test]
    fn an_oversized_send_is_too_large() {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let peer = socket.local_addr().unwrap();
        let error = socket.send_to(&vec![0; 70_000], peer).unwrap_err();
        assert_eq!(classify(Call::Send, &error), Action::TooLarge, "{error:?}");
        assert_eq!(classify(Call::ControlSend, &error), Action::TooLarge);
    }

    // ADR 0006 (EMSGSIZE and packet too big are Path MTU events, time
    // exceeded a hop-limit event, anything else an ICMP error)
    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn queued_errors_are_classified_by_icmp_type() {
        let entry = |origin, kind, code| nix::libc::sock_extended_err {
            ee_errno: 0,
            ee_origin: origin,
            ee_type: kind,
            ee_code: code,
            ee_pad: 0,
            ee_info: 0,
            ee_data: 0,
        };
        assert_eq!(queued(&entry(2, 3, 4), false), TransportError::MsgSize);
        assert_eq!(
            queued(&entry(2, 11, 0), false),
            TransportError::TimeExceeded
        );
        assert_eq!(queued(&entry(2, 3, 3), false), TransportError::Icmp);
        assert_eq!(queued(&entry(3, 2, 0), true), TransportError::MsgSize);
        assert_eq!(queued(&entry(3, 3, 0), true), TransportError::TimeExceeded);
        assert_eq!(queued(&entry(3, 1, 4), true), TransportError::Icmp);
        let mut local = entry(1, 0, 0);
        local.ee_errno = 90; // EMSGSIZE
        assert_eq!(queued(&local, false), TransportError::MsgSize);
    }
}
