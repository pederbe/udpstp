//! Receive Buffer Overflows on Linux: datagrams the kernel
//! discarded at a Test Connection's socket before the Data Phase Engine
//! read them, mostly because its receive buffer was full. The path didn't
//! lose them, but a Load Receiver counts them as lost, so the results and
//! the Server's log report them beside the loss.
//!
//! The kernel counts them per socket from its opening (sk_drops). A
//! process reads the count, without privilege or `unsafe`, in one of two
//! ways, which [`Counter`] chooses as the socket opens:
//!
//! - **Listed:** the `drops` column of the process's own UDP tables,
//!   `/proc/self/net/udp` and `udp6`, found by the socket's inode, which
//!   `/proc/self/fd` names, read when the Data Phase ends. Only this
//!   process's network namespace is listed there, which is the one its
//!   sockets are in. They are read as the process's own, not as
//!   `/proc/net/udp{,6}`, the link to them: the hardened systemd unit's
//!   `ProcSubset=pid` mounts a `/proc` that hides everything not a
//!   process's, `/proc/net` too, but keeps `/proc/self`.
//! - **Attached,** where the process can't read those tables: since Linux
//!   7.2 a `/proc` mounted with `subset=pid` hides `/proc/self/net` too,
//!   unless whoever mounted it holds CAP_NET_ADMIN over the network
//!   namespace (kernel commit a2a5eb6323a7), which the root of a user
//!   namespace in the host's network namespace doesn't. The socket
//!   then asks the kernel to attach the count to each datagram it queues
//!   (SO_RXQ_OVFL), and the batched reads keep the highest
//!   (`batch_read.rs`).
//!
//! Listed is chosen where it can be: it is exact when the Data Phase ends,
//! where the attached count misses the overflows after the last datagram
//! read, and asking for the attached count costs the datagrams of the
//! first read after the first overflow their timestamps (`batch_read.rs`).
//! The attached count is unknown while the last datagram read lost it
//! that way, and from the first datagram read on its own, where recvmmsg
//! is refused, as a single read comes without it. An exact count at any
//! time is also sock_diag's, over netlink, which the hardened unit's
//! `RestrictAddressFamilies` doesn't allow, and SO_MEMINFO's, a getsockopt
//! that no crate the runtime uses binds.

use std::net::UdpSocket;
use std::os::fd::AsRawFd;

use nix::sys::socket::{setsockopt, sockopt};

/// The process's own UDP socket tables, IPv4 and IPv6.
const TABLES: [&str; 2] = ["/proc/self/net/udp", "/proc/self/net/udp6"];

/// How a socket's Receive Buffer Overflows are counted, chosen as it
/// opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Counter {
    /// Read from the process's own UDP tables when the Data Phase ends.
    Listed,
    /// Attached by the kernel to each datagram read in batches.
    Attached,
    /// Not known: the process can't read its UDP tables, and the kernel
    /// refused to attach the count, or a datagram was read on its own.
    Unknown,
}

impl Counter {
    /// Chooses how the Receive Buffer Overflows at `socket` are counted:
    /// from the process's own UDP tables where it can read them, and
    /// otherwise from the count the kernel attaches to each datagram,
    /// which this asks for.
    pub(crate) fn open(socket: &UdpSocket) -> Self {
        if std::fs::metadata(TABLES[0]).is_ok() {
            Self::Listed
        } else if setsockopt(socket, sockopt::RxqOvfl, &1).is_ok() {
            Self::Attached
        } else {
            Self::Unknown
        }
    }

    /// A datagram was read on its own, where recvmmsg is refused: it came
    /// without the count attached, so an attached count is unknown from
    /// now on.
    pub(crate) fn read_alone(&mut self) {
        if *self == Self::Attached {
            *self = Self::Unknown;
        }
    }

    /// The Receive Buffer Overflows at `socket` since it opened, as its
    /// Data Phase ends; `attached` is the highest count the kernel attached
    /// to a datagram read in batches, if known. None if they can't be
    /// counted.
    pub(crate) fn count(self, socket: &impl AsRawFd, attached: Option<u32>) -> Option<u64> {
        match self {
            Self::Listed => listed(socket),
            Self::Attached => attached.map(u64::from),
            Self::Unknown => None,
        }
    }
}

/// The overflows the process's own UDP tables list for `socket`; none if
/// they can't be read.
fn listed(socket: &impl AsRawFd) -> Option<u64> {
    listed_each(std::iter::once(socket)).pop().flatten()
}

/// The overflows the process's own UDP tables list for each of `sockets`,
/// the tables read once: none for one they don't list, or if they can't be
/// read. The pooled executor reads a batch of ending Test Connections'
/// this way, off its Engine Threads.
pub(crate) fn listed_each<'a, S: AsRawFd + 'a>(
    sockets: impl Iterator<Item = &'a S>,
) -> Vec<Option<u64>> {
    let tables: Vec<String> = TABLES
        .into_iter()
        .filter_map(|table| std::fs::read_to_string(table).ok())
        .collect();
    sockets
        .map(|socket| {
            let inode = inode(socket)?;
            tables.iter().find_map(|table| drops_in(table, &inode))
        })
        .collect()
}

/// The inode of `socket`, as `/proc/self/fd` names it.
fn inode(socket: &impl AsRawFd) -> Option<String> {
    let link = std::fs::read_link(format!("/proc/self/fd/{}", socket.as_raw_fd())).ok()?;
    Some(
        link.to_str()?
            .strip_prefix("socket:[")?
            .strip_suffix(']')?
            .to_owned(),
    )
}

/// The `drops` of the socket with `inode` in a `net/udp` table: the 10th
/// column is the inode and the 13th, the last, the drops.
fn drops_in(table: &str, inode: &str) -> Option<u64> {
    table.lines().skip(1).find_map(|line| {
        let columns: Vec<&str> = line.split_whitespace().collect();
        (columns.len() >= 13 && columns[9] == inode)
            .then(|| columns[12].parse().ok())
            .flatten()
    })
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::net::UdpSocket;
    use std::time::{Duration, Instant};

    use crate::protocol::{RxBatch, RxPdu, TransportError};

    use super::super::batch_read::{self, BATCH, BatchReader, HEADER_SLOT, Received};
    use super::super::{Platform, Settings, Sink, Socket};
    use super::*;
    use crate::runtime::clock::Clock;
    use crate::runtime::engine::times::Fresh;

    #[test]
    fn a_sockets_drops_are_read_by_its_inode() {
        let table = "   sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode ref pointer drops
  110: 0100007F:A5C1 00000000:0000 07 00000000:00000000 00:00000000 00000000  1000        0 424242 2 0000000000000000 0
  111: 0100007F:A5C2 00000000:0000 07 00000000:00004400 00:00000000 00000000  1000        0 424243 2 0000000000000000 17
";
        assert_eq!(drops_in(table, "424243"), Some(17));
        assert_eq!(drops_in(table, "424242"), Some(0));
        assert_eq!(drops_in(table, "4242"), None);
    }

    /// A non-blocking receiver with a receive buffer of a few datagrams,
    /// and a sender connected to it.
    fn small_receiver() -> (UdpSocket, UdpSocket) {
        let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
        receiver.set_nonblocking(true).unwrap();
        let _ = socket2::SockRef::from(&receiver).set_recv_buffer_size(4096);
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        sender.connect(receiver.local_addr().unwrap()).unwrap();
        (sender, receiver)
    }

    /// Sends 200 datagrams of 1000 bytes, far more than the receiver's
    /// buffer holds, and calls `read` to read what arrives and count its
    /// Receive Buffer Overflows, sending one more datagram at a time, until
    /// they are as many as the datagrams sent and not read: the kernel
    /// attaches the count only to a datagram queued after them, and on a
    /// busy host handles the datagrams a moment after they are sent, so
    /// this waits, up to 5 s. Returns the count, and the datagrams sent and
    /// not read.
    fn flood(
        sender: &UdpSocket,
        mut read: impl FnMut() -> (u64, Option<u64>),
    ) -> (Option<u64>, u64) {
        let mut sent = 0u64;
        for _ in 0..200 {
            sent += u64::from(sender.send(&[0; 1000]).is_ok());
        }
        let mut received = 0u64;
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            let (read, counted) = read();
            received += read;
            if counted == Some(sent - received) || Instant::now() >= until {
                return (counted, sent - received);
            }
            sent += u64::from(sender.send(&[1; 100]).is_ok());
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Reads, in batches, whatever has arrived at `receiver`: what the
    /// kernel reported about each datagram.
    fn read_waiting(reader: &mut BatchReader, receiver: &UdpSocket) -> Vec<Received> {
        let mut read = Vec::new();
        loop {
            match reader.read(receiver, BATCH) {
                Ok(0) => return read,
                Ok(count) => read.extend((0..count).map(|index| reader.datagram(index).1)),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return read,
                Err(error) => panic!("{error}"),
            }
        }
    }

    /// Sets `receiver` up as the Linux adapter sets up a Load Receiver's
    /// socket where the process can't read its UDP tables, and floods it
    /// until the count arrived: its reader, and the datagrams sent and not
    /// read, which the count equals.
    fn flooded(sender: &UdpSocket, receiver: &UdpSocket) -> (BatchReader, u64) {
        batch_read::enable(receiver, true);
        setsockopt(receiver, sockopt::RxqOvfl, &1).unwrap();
        let mut reader = BatchReader::new(HEADER_SLOT);
        let (attached, lost) = flood(sender, || {
            let read = read_waiting(&mut reader, receiver).len() as u64;
            (read, reader.overflows().map(u64::from))
        });
        assert!(lost > 100, "{lost} not read");
        assert_eq!(attached, Some(lost));
        (reader, lost)
    }

    /// Reads, in batches, the `count` datagrams `sender` sends next.
    fn next(
        count: usize,
        sender: &UdpSocket,
        receiver: &UdpSocket,
        reader: &mut BatchReader,
    ) -> Vec<Received> {
        for _ in 0..count {
            sender.send(&[2; 100]).unwrap();
        }
        let mut read = Vec::new();
        let until = Instant::now() + Duration::from_secs(2);
        while read.len() < count && Instant::now() < until {
            read.extend(read_waiting(reader, receiver));
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(read.len(), count);
        read
    }

    // : asked to (SO_RXQ_OVFL), the kernel attaches a socket's Receive
    // Buffer Overflows to each datagram queued after them, and the batched
    // reads keep the highest: every datagram sent to the socket and not
    // read. From the read after the one whose control messages the first
    // overflow truncated on, every datagram carries its timestamp and TOS
    // again: the headers were built anew once, with room for the count.
    #[test]
    fn the_kernel_attaches_the_overflows_to_the_datagrams_queued_after_them() {
        let (sender, receiver) = small_receiver();
        let (mut reader, lost) = flooded(&sender, &receiver);
        for received in next(3, &sender, &receiver, &mut reader) {
            assert!(
                received.kernel_wall.is_some() && received.tos == Some(0),
                "{received:?}"
            );
        }
        assert_eq!(reader.overflows().map(u64::from), Some(lost));
    }

    // : a datagram whose control messages didn't fit loses them all,
    // and the count it carried may have been higher than any read before
    // it, so the count is unknown until a datagram's arrive whole again,
    // rather than too low.
    #[test]
    fn the_count_is_unknown_while_the_last_datagram_read_lost_its_control_messages() {
        let (sender, receiver) = small_receiver();
        let (mut reader, lost) = flooded(&sender, &receiver);
        // Without the TOS, a datagram carries fewer control messages, and
        // its header's room shrinks to them.
        setsockopt(&receiver, sockopt::IpRecvTos, &false).unwrap();
        next(1, &sender, &receiver, &mut reader);
        assert_eq!(reader.overflows().map(u64::from), Some(lost));
        // With the TOS again, the next one's don't fit in that room.
        setsockopt(&receiver, sockopt::IpRecvTos, &true).unwrap();
        let truncated = next(1, &sender, &receiver, &mut reader)[0];
        assert_eq!((truncated.kernel_wall, truncated.tos), (None, None));
        assert_eq!(reader.overflows(), None);
        // The headers built anew, the one after arrives whole.
        next(1, &sender, &receiver, &mut reader);
        assert_eq!(reader.overflows().map(u64::from), Some(lost));
    }

    /// A sink that takes whatever the socket hands it.
    struct Taken;

    impl Sink for Taken {
        fn status_pdu(&mut self, _: RxPdu<'_>) {}

        fn load_pdus(&mut self, _: RxBatch<'_>) {}

        fn path_error(&mut self, _: TransportError) {}
    }

    /// Set by a_full_receive_buffer_is_counted_where_proc_shows_only_processes
    /// for a namespace where it knows how the count is read: the
    /// [`Counter`] expected, by name.
    const EXPECTED_COUNTER: &str = "UDPSTP_TEST_OVERFLOW_COUNTER";

    // , , : a Test Connection's socket, over the Linux adapter
    // as the Data Phase Engine opens it, counts its Receive Buffer
    // Overflows: every datagram sent to it, with a small receive buffer,
    // and not read.
    #[test]
    fn a_full_receive_buffer_is_counted() {
        if let Ok(expected) = std::env::var(EXPECTED_COUNTER) {
            let probe = UdpSocket::bind("127.0.0.1:0").unwrap();
            assert_eq!(format!("{:?}", Counter::open(&probe)), expected);
        }
        let (sender, receiver) = small_receiver();
        let settings = Settings {
            peer: sender.local_addr().unwrap(),
            connected: false,
            load_receiver: true,
            rx_timestamps: true,
            send_stamps: false,
            explicit_numbers: true,
            clock: Clock::new(),
        };
        let mut socket = Socket::open(Platform, receiver, &settings);
        assert_eq!(socket.receive_buffer_overflows(), Some(0));
        let (counted, lost) = flood(&sender, || {
            let reads = socket.receive(&Fresh(settings.clock), &mut Taken).unwrap();
            (reads.datagrams as u64, socket.receive_buffer_overflows())
        });
        assert!(lost > 100, "{lost} not read");
        assert_eq!(counted, Some(lost));
    }

    // : a datagram read on its own, where recvmmsg is refused, comes
    // without the count the kernel attaches, so from then on an attached
    // count is unknown; a listed one doesn't depend on the reads.
    #[test]
    fn a_datagram_read_on_its_own_leaves_an_attached_count_unknown() {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut attached = Counter::Attached;
        assert_eq!(attached.count(&socket, Some(3)), Some(3));
        attached.read_alone();
        assert_eq!(attached.count(&socket, Some(3)), None);
        let mut listed = Counter::Listed;
        listed.read_alone();
        assert_eq!(listed, Counter::Listed);
    }

    // , : the hardened systemd unit (ProtectProc=invisible,
    // ProcSubset=pid) mounts a /proc that shows only processes, without
    // /proc/net. The count is read under such a /proc all the same:
    // rootless, this runs a_full_receive_buffer_is_counted again in a user,
    // mount and PID namespace whose /proc is mounted with the unit's
    // options, three ways:
    // - in the host's network namespace, over which the namespace's root
    //   lacks CAP_NET_ADMIN, so since Linux 7.2 the process's own UDP
    //   tables are hidden too, and the count the kernel attaches is read,
    //   where earlier kernels list them;
    // - in a network namespace of its own, over which its root holds
    //   CAP_NET_ADMIN, as the host's root does over the host's, so the
    //   tables are listed on every kernel;
    // - with the tables hidden under a tmpfs, so the count the kernel
    //   attaches is read on every kernel.
    // A way the host doesn't allow is skipped.
    #[test]
    fn a_full_receive_buffer_is_counted_where_proc_shows_only_processes() {
        const TEST: &str = "runtime::socket::overflows::tests::a_full_receive_buffer_is_counted";
        const PROC: &str =
            "mount -t proc -o subset=pid,hidepid=invisible proc /proc && ! test -e /proc/net";
        let exe = std::env::current_exe().unwrap();
        let ways = [
            (
                "the host's network namespace",
                "-Urmpf",
                PROC.to_owned(),
                None,
            ),
            (
                "a network namespace of its own",
                "-Urmpfn",
                format!("ip link set lo up && {PROC} && test -e /proc/self/net/udp"),
                Some("Listed"),
            ),
            (
                "the tables hidden",
                "-Urmpf",
                format!(
                    "{PROC} && mount -t tmpfs tmpfs /proc/$$/net && ! test -e /proc/self/net/udp"
                ),
                Some("Attached"),
            ),
        ];
        for (way, namespaces, setup, counter) in ways {
            let mut unshare = std::process::Command::new("unshare");
            unshare
                .args([namespaces, "--mount-proc", "sh", "-c"])
                .arg(format!("{setup} && exec \"$0\" --exact \"$1\""))
                .arg(&exe)
                .arg(TEST);
            if let Some(counter) = counter {
                unshare.env(EXPECTED_COUNTER, counter);
            }
            let Ok(output) = unshare.output() else {
                eprintln!("skipped: no unshare here");
                return;
            };
            let out = String::from_utf8_lossy(&output.stdout);
            if !out.contains("running 1 test") {
                eprintln!(
                    "skipped, {way}: not allowed here: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                continue;
            }
            assert!(
                output.status.success() && out.contains("1 passed"),
                "{way}: {out}{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
