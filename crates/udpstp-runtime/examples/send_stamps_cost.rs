//! What kernel send timestamps cost a Load Sender, and what each way of
//! matching them to Load PDUs needs from the kernel.
//!
//! With `UDPSTP_KERNEL_TX_TIMESTAMPS=1` a Linux Load Sender asks for a
//! software send timestamp of each message it sends, which the kernel
//! queues on the socket's error queue; each is matched to the Send
//! Plan whose lpduTime it measures. Three ways of matching are compared
//! here, with no timestamps as the baseline:
//!
//! - **count**: the kernel numbers the messages (SOF_TIMESTAMPING_OPT_ID)
//!   and queues the timestamp alone (OPT_TSONLY); the Load Sender counts
//!   the messages the kernel took. A send the kernel numbers and then
//!   refuses shifts the count.
//! - **explicit**: each message carries its own number in an SCM_TS_OPT_ID
//!   control message (Linux 6.13), which nothing can shift; OPT_TSONLY.
//!   nix's sendmmsg shares one set of control messages across a call, so
//!   each message goes with a sendmsg of its own (socket2's).
//! - **looped**: without OPT_TSONLY the kernel queues the message itself
//!   with its timestamp, and its Load PDU header carries its lpduSeqNo.
//!
//! On a connected pair of UDP sockets over loopback it reports:
//!
//! 1. **The kernel**: whether it takes SCM_TS_OPT_ID and returns the number
//!    set; what a looped message holds, for one datagram and for a UDP GSO
//!    message, over loopback and over `--via`'s route if given; and each
//!    design's numbers after a send the kernel numbers and then refuses (a
//!    GSO message of more segments than UDP_MAX_SEGMENTS, EINVAL from
//!    udp_send_skb), checked against the Load PDUs sent.
//! 2. **The error queue's memory**: what a queued timestamp takes from the
//!    socket's receive buffer, and how many fit in it, for the Bursts of
//!    rows 1000 and above, with udpstp's 2 MiB request and with the
//!    kernel's default buffer.
//! 3. **The cost**: the time per Burst, sending it and draining its
//!    timestamps, as the Data Phase Engine drains them at its next wait.
//!    Each design gets `--runs` runs of `--bursts` Bursts sent back to
//!    back, interleaved with the other designs' so a change in the host's
//!    load touches them all; the median run is reported, with the fastest
//!    and the slowest.
//!
//! ```text
//! cargo run --release -p udpstp-runtime --example send_stamps_cost -- [--runs N] [--bursts N] [--via ADDR:PORT]
//! ```
//!

use std::process::ExitCode;

#[cfg(target_os = "linux")]
fn main() -> ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() -> ExitCode {
    eprintln!("send_stamps_cost: kernel send timestamps are Linux's alone");
    ExitCode::FAILURE
}

#[cfg(target_os = "linux")]
mod linux {
    use std::io::{self, IoSlice, IoSliceMut};
    use std::net::{SocketAddr, UdpSocket};
    use std::os::fd::AsRawFd;
    use std::process::ExitCode;
    use std::time::{Duration, Instant};

    use nix::libc;
    use nix::sys::socket::{
        ControlMessage, ControlMessageOwned, MsgFlags, MultiHeaders, SockaddrStorage,
        TimestampingFlag, recvmsg, sendmmsg, setsockopt, sockopt,
    };
    use socket2::{MsgHdr, SockRef};

    /// SCM_TS_OPT_ID (Linux 6.13): 81 in include/uapi/asm-generic/socket.h
    /// and on MIPS and Alpha; SPARC numbers it 0x5a, which this harness
    /// doesn't cover.
    const SCM_TS_OPT_ID: libc::c_int = 81;

    /// The most messages one sendmmsg call carries, as udpstp's.
    const BATCH: usize = 64;

    /// udpstp's socket buffer request (`DEFAULT_SOCKET_BUFFER_SIZE`).
    const UDPSTP_BUFFER: usize = 2 * 1024 * 1024;

    /// The kernel's default receive buffer (net.core.rmem_default), which
    /// is also its default limit (net.core.rmem_max).
    const DEFAULT_BUFFER: usize = 212_992;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Design {
        Off,
        Count,
        Explicit,
        Looped,
    }

    impl Design {
        const ALL: [Design; 4] = [Design::Off, Design::Count, Design::Explicit, Design::Looped];

        fn name(self) -> &'static str {
            match self {
                Design::Off => "no timestamps",
                Design::Count => "count",
                Design::Explicit => "explicit",
                Design::Looped => "looped",
            }
        }

        fn flags(self) -> Option<TimestampingFlag> {
            let stamps = TimestampingFlag::SOF_TIMESTAMPING_TX_SOFTWARE
                | TimestampingFlag::SOF_TIMESTAMPING_SOFTWARE
                | TimestampingFlag::SOF_TIMESTAMPING_OPT_ID;
            match self {
                Design::Off => None,
                Design::Count | Design::Explicit => {
                    Some(stamps | TimestampingFlag::SOF_TIMESTAMPING_OPT_TSONLY)
                }
                Design::Looped => Some(stamps),
            }
        }
    }

    /// A row's Burst: `per_burst` Load PDUs of `size` bytes, as UDP GSO
    /// messages or one message each.
    struct Shape {
        name: &'static str,
        size: usize,
        per_burst: usize,
        gso: bool,
    }

    /// Rows 1000 (1 Gbps), 1011 (2.1 Gbps) and 1062 (7.2 Gbps) of the
    /// jumbo table, a Burst every 100 µs, and row 1000 where the kernel
    /// refuses UDP GSO.
    const SHAPES: [Shape; 4] = [
        Shape {
            name: "row 1000, 10 x 1222 B, GSO",
            size: 1222,
            per_burst: 10,
            gso: true,
        },
        Shape {
            name: "row 1011, 10 x 2597 B, GSO",
            size: 2597,
            per_burst: 10,
            gso: true,
        },
        Shape {
            name: "row 1062, 10 x 8972 B, GSO",
            size: 8972,
            per_burst: 10,
            gso: true,
        },
        Shape {
            name: "row 1000, 10 x 1222 B, no GSO",
            size: 1222,
            per_burst: 10,
            gso: false,
        },
    ];

    /// Load PDUs that go to the kernel as one message: from `at` in the
    /// Burst's buffer, `bytes` long.
    #[derive(Debug, Clone, Copy)]
    struct Message {
        at: usize,
        bytes: usize,
    }

    impl Shape {
        /// The Burst's messages, grouped as udpstp groups them: at most 64
        /// Load PDUs and 65,507 bytes each with GSO, one each without.
        fn messages(&self) -> Vec<Message> {
            let per = if self.gso {
                64.min(65_507 / self.size)
            } else {
                1
            };
            let mut messages = Vec::new();
            let mut first = 0;
            while first < self.per_burst {
                let pdus = per.min(self.per_burst - first);
                messages.push(Message {
                    at: first * self.size,
                    bytes: pdus * self.size,
                });
                first += pdus;
            }
            messages
        }
    }

    /// Writes Load PDU headers into `buf`, one per `size` bytes: the pduId
    /// and the lpduSeqNo, from `first_seq`.
    fn number_pdus(buf: &mut [u8], size: usize, first_seq: u32) {
        for (index, pdu) in buf.chunks_mut(size).enumerate() {
            pdu[..2].copy_from_slice(&[0xBE, 0xEF]);
            pdu[4..8].copy_from_slice(&(first_seq + index as u32).to_be_bytes());
        }
    }

    /// A connected sender, and the loopback socket it sends to, which never
    /// reads: what it can't hold, it drops.
    struct Link {
        sender: UdpSocket,
        _receiver: Option<UdpSocket>,
        /// The sender's port and the peer's, as a UDP header carries them.
        ports: [u8; 4],
        plain: MultiHeaders<()>,
        gso: MultiHeaders<()>,
    }

    impl Link {
        fn open(design: Design, via: Option<SocketAddr>, buffer: usize) -> io::Result<Self> {
            let (receiver, peer) = match via {
                Some(via) => (None, via),
                None => {
                    let receiver = UdpSocket::bind("127.0.0.1:0")?;
                    let _ = SockRef::from(&receiver).set_recv_buffer_size(4096);
                    let peer = receiver.local_addr()?;
                    (Some(receiver), peer)
                }
            };
            let sender = UdpSocket::bind(if via.is_some() {
                "0.0.0.0:0"
            } else {
                "127.0.0.1:0"
            })?;
            sender.connect(peer)?;
            sender.set_nonblocking(true)?;
            SockRef::from(&sender).set_recv_buffer_size(buffer)?;
            // As a Test Connection's socket (ADR 0006).
            setsockopt(&sender, sockopt::Ipv4RecvErr, &true)?;
            if let Some(flags) = design.flags() {
                setsockopt(&sender, sockopt::Timestamping, &flags)?;
            }
            let local = sender.local_addr()?.port().to_be_bytes();
            let remote = peer.port().to_be_bytes();
            Ok(Self {
                sender,
                _receiver: receiver,
                ports: [local[0], local[1], remote[0], remote[1]],
                plain: MultiHeaders::preallocate(BATCH, None),
                gso: MultiHeaders::preallocate(BATCH, Some(nix::cmsg_space!(u16))),
            })
        }

        /// Sends `messages` from `buf` as `design` does, with UDP GSO
        /// segments of `segment` bytes when given: how many the kernel took.
        /// `key` is the number the first carries, for `Design::Explicit`.
        fn send(
            &mut self,
            design: Design,
            buf: &[u8],
            messages: &[Message],
            segment: Option<usize>,
            key: u32,
        ) -> io::Result<usize> {
            if design == Design::Explicit {
                return self.send_each(buf, messages, segment, key);
            }
            let mut slices = [[IoSlice::new(&[])]; BATCH];
            for (slice, message) in slices.iter_mut().zip(messages) {
                *slice = [IoSlice::new(&buf[message.at..message.at + message.bytes])];
            }
            let addresses = [None::<()>; BATCH];
            let count = messages.len().min(BATCH);
            let size = segment.map_or(0, |s| s as u16);
            let control = [ControlMessage::UdpGsoSegments(&size)];
            let (headers, control): (_, &[ControlMessage]) = match segment {
                Some(_) => (&mut self.gso, &control),
                None => (&mut self.plain, &[]),
            };
            let sent = sendmmsg(
                self.sender.as_raw_fd(),
                headers,
                &slices[..count],
                &addresses[..count],
                control,
                MsgFlags::empty(),
            )?;
            Ok(sent.count())
        }

        /// Sends each message with a sendmsg of its own, numbered from `key`
        /// by SCM_TS_OPT_ID: how many the kernel took.
        fn send_each(
            &self,
            buf: &[u8],
            messages: &[Message],
            segment: Option<usize>,
            key: u32,
        ) -> io::Result<usize> {
            let socket = SockRef::from(&self.sender);
            let mut control = [0u8; 64];
            for (index, message) in messages.iter().enumerate() {
                let number = key.wrapping_add(index as u32).to_ne_bytes();
                let mut len =
                    control_message(&mut control, 0, libc::SOL_SOCKET, SCM_TS_OPT_ID, &number);
                if let Some(segment) = segment {
                    let size = (segment as u16).to_ne_bytes();
                    len =
                        control_message(&mut control, len, libc::SOL_UDP, libc::UDP_SEGMENT, &size);
                }
                let slices = [IoSlice::new(&buf[message.at..message.at + message.bytes])];
                let header = MsgHdr::new()
                    .with_buffers(&slices)
                    .with_control(&control[..len]);
                if let Err(error) = socket.sendmsg(&header, 0) {
                    return if index == 0 { Err(error) } else { Ok(index) };
                }
            }
            Ok(messages.len())
        }

        /// Hands each timestamp queued on the socket to `each`, oldest first,
        /// until the queue is empty, reading at most `data.len()` bytes of a
        /// looped message: how many there were.
        fn drain(&self, data: &mut [u8], mut each: impl FnMut(Stamp)) -> usize {
            let mut control = nix::cmsg_space!(
                libc::sock_extended_err,
                libc::sockaddr_in6,
                [libc::timespec; 3]
            );
            let mut count = 0;
            loop {
                let (bytes, key, wall) = {
                    let mut iov = [IoSliceMut::new(data)];
                    let Ok(message) = recvmsg::<SockaddrStorage>(
                        self.sender.as_raw_fd(),
                        &mut iov,
                        Some(&mut control),
                        MsgFlags::MSG_ERRQUEUE | MsgFlags::MSG_DONTWAIT,
                    ) else {
                        return count;
                    };
                    let (mut key, mut wall) = (None, None);
                    for cmsg in message.cmsgs().into_iter().flatten() {
                        match cmsg {
                            ControlMessageOwned::ScmTimestampsns(stamps) => {
                                wall = Some(Duration::from(stamps.system));
                            }
                            ControlMessageOwned::Ipv4RecvErr(error, _)
                            | ControlMessageOwned::Ipv6RecvErr(error, _)
                                if error.ee_origin == libc::SO_EE_ORIGIN_TIMESTAMPING =>
                            {
                                key = Some(error.ee_data);
                            }
                            _ => {}
                        }
                    }
                    (message.bytes, key, wall)
                };
                let (Some(key), Some(_)) = (key, wall) else {
                    continue;
                };
                count += 1;
                let looped = looped_pdu(&data[..bytes], self.ports);
                each(Stamp {
                    key,
                    looped_bytes: bytes,
                    pdu_at: looped.map(|(at, _)| at),
                    seq: looped.map(|(_, seq)| seq),
                });
            }
        }

        /// The bytes the socket's receive buffer holds (`rx_queue` in the
        /// process's net/udp table), and its size as the kernel reports it.
        fn receive_buffer(&self) -> (Option<u64>, usize) {
            let size = SockRef::from(&self.sender).recv_buffer_size().unwrap_or(0);
            (rx_queue(&self.sender), size)
        }
    }

    /// Appends one control message to `buf` at `at`, laid out as the
    /// kernel's struct cmsghdr (a size_t length, the level and the type)
    /// and its data, padded to the next: where the next one goes.
    fn control_message(
        buf: &mut [u8],
        at: usize,
        level: libc::c_int,
        kind: libc::c_int,
        data: &[u8],
    ) -> usize {
        let word = std::mem::size_of::<usize>();
        let header = std::mem::size_of::<libc::cmsghdr>();
        let space = header + data.len().next_multiple_of(word);
        buf[at..at + space].fill(0);
        buf[at..at + word].copy_from_slice(&(header + data.len()).to_ne_bytes());
        buf[at + word..at + word + 4].copy_from_slice(&level.to_ne_bytes());
        buf[at + word + 4..at + word + 8].copy_from_slice(&kind.to_ne_bytes());
        buf[at + header..at + header + data.len()].copy_from_slice(data);
        at + space
    }

    /// Where the Load PDU of a looped message starts, after its link-layer,
    /// IP and UDP headers, and its lpduSeqNo: found by the UDP header's
    /// ports and the pduId after it.
    fn looped_pdu(data: &[u8], ports: [u8; 4]) -> Option<(usize, u32)> {
        (0..data.len().saturating_sub(15)).find_map(|at| {
            (data[at..at + 4] == ports && data[at + 8..at + 10] == [0xBE, 0xEF]).then(|| {
                let seq = &data[at + 12..at + 16];
                (at + 8, u32::from_be_bytes([seq[0], seq[1], seq[2], seq[3]]))
            })
        })
    }

    /// A timestamp from the error queue: the number the kernel gave it,
    /// and what a looped message held.
    #[derive(Debug)]
    struct Stamp {
        key: u32,
        looped_bytes: usize,
        pdu_at: Option<usize>,
        seq: Option<u32>,
    }

    /// The `rx_queue` of `socket`'s row in /proc/self/net/udp, found by its
    /// inode: the bytes charged to its receive buffer, its error queue's
    /// too.
    fn rx_queue(socket: &UdpSocket) -> Option<u64> {
        let link = std::fs::read_link(format!("/proc/self/fd/{}", socket.as_raw_fd())).ok()?;
        let inode = link
            .to_str()?
            .strip_prefix("socket:[")?
            .strip_suffix(']')?
            .to_owned();
        let table = std::fs::read_to_string("/proc/self/net/udp").ok()?;
        table.lines().skip(1).find_map(|line| {
            let columns: Vec<&str> = line.split_whitespace().collect();
            (columns.len() > 9 && columns[9] == inode)
                .then(|| u64::from_str_radix(columns[4].split(':').nth(1)?, 16).ok())
                .flatten()
        })
    }

    struct Args {
        runs: usize,
        bursts: u32,
        via: Option<SocketAddr>,
    }

    fn args() -> Result<Args, String> {
        let mut args = Args {
            runs: 7,
            bursts: 20_000,
            via: None,
        };
        let mut given = std::env::args().skip(1);
        while let Some(arg) = given.next() {
            let value = given.next().ok_or(format!("{arg} needs a value"))?;
            match arg.as_str() {
                "--runs" => args.runs = value.parse().map_err(|_| "--runs N")?,
                "--bursts" => args.bursts = value.parse().map_err(|_| "--bursts N")?,
                "--via" => args.via = Some(value.parse().map_err(|_| "--via ADDR:PORT")?),
                _ => return Err(format!("unknown argument {arg}")),
            }
        }
        Ok(args)
    }

    pub(super) fn main() -> ExitCode {
        let args = match args() {
            Ok(args) => args,
            Err(error) => {
                eprintln!("send_stamps_cost: {error}");
                return ExitCode::FAILURE;
            }
        };
        println!(
            "kernel {}",
            std::fs::read_to_string("/proc/sys/kernel/osrelease")
                .unwrap_or_default()
                .trim()
        );
        if let Err(error) = the_kernel(args.via) {
            eprintln!("send_stamps_cost: {error}");
            return ExitCode::FAILURE;
        }
        if let Err(error) = memory() {
            eprintln!("send_stamps_cost: {error}");
            return ExitCode::FAILURE;
        }
        if let Err(error) = cost(args.runs, args.bursts) {
            eprintln!("send_stamps_cost: {error}");
            return ExitCode::FAILURE;
        }
        ExitCode::SUCCESS
    }

    /// Drains `link` until `expected` timestamps came, for at most 100 ms:
    /// the kernel queues a loopback message's timestamp before its send
    /// returns, but a real device's later.
    fn collect(link: &Link, data: &mut [u8], expected: usize) -> Vec<Stamp> {
        let mut stamps = Vec::new();
        let until = Instant::now() + Duration::from_millis(100);
        while stamps.len() < expected && Instant::now() < until {
            link.drain(data, |stamp| stamps.push(stamp));
        }
        stamps
    }

    /// Part 1: what the kernel does.
    fn the_kernel(via: Option<SocketAddr>) -> io::Result<()> {
        println!("\n1. The kernel");
        let mut data = vec![0u8; 65_536];
        // SCM_TS_OPT_ID: taken, and the number returned?
        let mut link = Link::open(Design::Explicit, None, UDPSTP_BUFFER)?;
        let mut buf = vec![0u8; 1222];
        number_pdus(&mut buf, 1222, 1);
        let one = [Message { at: 0, bytes: 1222 }];
        match link.send(Design::Explicit, &buf, &one, None, 0x5EED_0001) {
            Ok(_) => {
                let keys: Vec<u32> = collect(&link, &mut data, 1).iter().map(|s| s.key).collect();
                println!(
                    "   SCM_TS_OPT_ID: taken; the timestamp came back numbered {keys:x?} (set 0x5eed0001)"
                );
            }
            Err(error) => println!("   SCM_TS_OPT_ID: refused ({error})"),
        }
        // What a looped message holds.
        let routes: Vec<(&str, Option<SocketAddr>)> = std::iter::once(("loopback", None))
            .chain(via.map(|via| ("--via", Some(via))))
            .collect();
        for (route, via) in routes {
            for gso in [false, true] {
                let mut link = Link::open(Design::Looped, via, UDPSTP_BUFFER)?;
                let shape = Shape {
                    name: "",
                    size: 1222,
                    per_burst: if gso { 10 } else { 1 },
                    gso,
                };
                let mut buf = vec![0u8; shape.size * shape.per_burst];
                number_pdus(&mut buf, shape.size, 1);
                let messages = shape.messages();
                link.send(Design::Looped, &buf, &messages, gso.then_some(1222), 0)?;
                for stamp in collect(&link, &mut data, 1) {
                    println!(
                        "   looped, {route}, {}: {} bytes, the Load PDU at byte {:?} (lpduSeqNo {:?})",
                        if gso {
                            "a GSO message of 10 x 1222 B"
                        } else {
                            "one 1222-byte datagram"
                        },
                        stamp.looped_bytes,
                        stamp.pdu_at,
                        stamp.seq,
                    );
                }
            }
        }
        // A send the kernel numbers and then refuses: the second of four.
        println!(
            "   a send the kernel refuses after numbering it (GSO over UDP_MAX_SEGMENTS), between Load PDUs 1 and 2:"
        );
        for design in [Design::Count, Design::Explicit, Design::Looped] {
            let mut link = Link::open(design, None, UDPSTP_BUFFER)?;
            let mut buf = vec![0u8; 3 * 1222];
            number_pdus(&mut buf, 1222, 1);
            let pdu = |seq: usize| {
                [Message {
                    at: (seq - 1) * 1222,
                    bytes: 1222,
                }]
            };
            // The Load Sender's own count of the messages the kernel took.
            let mut taken = 0u32;
            taken += link.send(design, &buf, &pdu(1), None, taken)? as u32;
            let refused = vec![0u8; 2000];
            let over = [Message { at: 0, bytes: 2000 }];
            let error = link
                .send(design, &refused, &over, Some(10), taken)
                .expect_err("200 segments are more than UDP_MAX_SEGMENTS");
            for seq in [2, 3] {
                taken += link.send(design, &buf, &pdu(seq), None, taken)? as u32;
            }
            let stamps = collect(&link, &mut data, 3);
            let got: Vec<String> = stamps
                .iter()
                .map(|s| match s.seq {
                    Some(seq) => format!("{} (lpduSeqNo {seq})", s.key),
                    None => s.key.to_string(),
                })
                .collect();
            println!(
                "     {:9} refused with {error}; numbers of Load PDUs 1, 2, 3: {}",
                design.name(),
                got.join(", ")
            );
        }
        Ok(())
    }

    /// Part 2: what queued timestamps take from the receive buffer.
    fn memory() -> io::Result<()> {
        println!("\n2. The error queue's memory, nothing drained");
        println!(
            "   {:32} {:9} {:>9} {:>11} {:>9} {:>10}",
            "Burst", "design", "buffer", "held", "queued", "per stamp"
        );
        let mut data = vec![0u8; 256];
        for shape in &SHAPES {
            let messages = shape.messages();
            let segment = shape.gso.then_some(shape.size);
            let mut buf = vec![0u8; shape.size * shape.per_burst];
            for design in [Design::Count, Design::Looped] {
                for buffer in [UDPSTP_BUFFER, DEFAULT_BUFFER] {
                    let mut link = Link::open(design, None, buffer)?;
                    // Enough messages to fill the larger buffer.
                    let bursts = 16_000 / messages.len() as u32;
                    for burst in 0..bursts {
                        number_pdus(&mut buf, shape.size, burst * shape.per_burst as u32 + 1);
                        link.send(design, &buf, &messages, segment, 0)?;
                    }
                    let (held, size) = link.receive_buffer();
                    let queued = link.drain(&mut data, |_| {});
                    let held = held.unwrap_or(0);
                    println!(
                        "   {:32} {:9} {:>9} {:>11} {:>9} {:>10}",
                        shape.name,
                        design.name(),
                        size,
                        held,
                        format!("{queued}/{}", bursts as usize * messages.len()),
                        held / queued.max(1) as u64,
                    );
                }
            }
        }
        Ok(())
    }

    /// Part 3: the time per Burst.
    fn cost(runs: usize, bursts: u32) -> io::Result<()> {
        println!("\n3. The time per Burst, sent and drained ({runs} runs of {bursts} Bursts each)");
        let mut data = vec![0u8; 256];
        for shape in &SHAPES {
            let messages = shape.messages();
            let segment = shape.gso.then_some(shape.size);
            let mut buf = vec![0u8; shape.size * shape.per_burst];
            let mut times: Vec<Vec<Duration>> = vec![Vec::new(); Design::ALL.len()];
            let mut wrong = [0u64; 4];
            let mut stamped = [0u64; 4];
            for _ in 0..runs {
                for (index, &design) in Design::ALL.iter().enumerate() {
                    let mut link = Link::open(design, None, UDPSTP_BUFFER)?;
                    let mut taken = 0u32;
                    let started = Instant::now();
                    for burst in 0..bursts {
                        let first_seq = burst * shape.per_burst as u32 + 1;
                        number_pdus(&mut buf, shape.size, first_seq);
                        let sent = link.send(design, &buf, &messages, segment, taken)?;
                        assert_eq!(sent, messages.len(), "loopback takes every message");
                        let first_key = taken;
                        taken += sent as u32;
                        let mut next = 0usize;
                        link.drain(&mut data, |stamp| {
                            stamped[index] += 1;
                            let message = messages.get(next);
                            let right = match design {
                                Design::Looped => {
                                    stamp.seq
                                        == message.map(|m| first_seq + (m.at / shape.size) as u32)
                                }
                                _ => stamp.key == first_key + next as u32,
                            };
                            wrong[index] += u64::from(!right);
                            next += 1;
                        });
                    }
                    times[index].push(started.elapsed() / bursts);
                }
            }
            println!("   {}:", shape.name);
            for (index, design) in Design::ALL.iter().enumerate() {
                let mut run = times[index].clone();
                run.sort();
                let sent = u64::from(bursts) * runs as u64 * messages.len() as u64;
                println!(
                    "     {:14} {:>7.2} µs a Burst (runs {:.2}-{:.2}){}",
                    design.name(),
                    run[run.len() / 2].as_secs_f64() * 1e6,
                    run[0].as_secs_f64() * 1e6,
                    run[run.len() - 1].as_secs_f64() * 1e6,
                    match design {
                        Design::Off => String::new(),
                        _ => format!(
                            "; {} of {sent} messages stamped, {} matched wrongly",
                            stamped[index], wrong[index]
                        ),
                    }
                );
            }
        }
        Ok(())
    }
}
