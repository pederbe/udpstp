//! The real pooled turn, observed through scripted socket operations and
//! time. Real sockets provide epoll registrations only. No traffic, clock
//! reads, waits or worker threads determine the expected operations.

use std::net::{SocketAddr, UdpSocket};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize};

use crate::protocol::{DataEvent, DataPhase, Direction, Now, SocketId, TransportError, WallTime};
use nix::sys::epoll::EpollCreateFlags;
use nix::sys::eventfd::{EfdFlags, EventFd};

use super::*;
use crate::runtime::clock::Reading;
use crate::runtime::contract::activate;
use crate::runtime::engine::FromEngine;
use crate::runtime::engine::times::Fresh;
use crate::runtime::socket::{Offers, Woke, batch, batch_read};

#[derive(Debug, PartialEq, Eq)]
enum Operation {
    Send,
    Read(usize),
}

/// Finite traffic that exceeds two turns' receive budgets. The core
/// rejects these datagrams, whether they come from the peer or elsewhere.
struct Wire {
    operations: Arc<Mutex<Vec<Operation>>>,
    remaining: usize,
    foreign_source: bool,
    peer: SocketAddr,
    socket: Option<UdpSocket>,
}

impl Open for Wire {
    type Adapter = Self;

    fn open(mut self, socket: UdpSocket, settings: &Settings) -> Self {
        self.socket = Some(socket);
        self.peer = settings.peer;
        self
    }
}

impl Adapter for Wire {
    fn send(&mut self, datagram: &[u8], _: Option<MonoTime>) -> io::Result<usize> {
        assert!(datagram.starts_with(&[0xBE, 0xEF]), "a Load PDU is due");
        self.operations.lock().unwrap().push(Operation::Send);
        Ok(datagram.len())
    }

    fn recv(&mut self, _: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        unreachable!("the scripted socket supports batched reads")
    }

    fn offers(&self) -> Offers {
        Offers::default()
    }

    fn send_batch(
        &mut self,
        _: &[u8],
        _: &[batch::Message],
        _: Option<usize>,
        _: Option<MonoTime>,
    ) -> io::Result<usize> {
        unreachable!("the scripted socket sends individual datagrams")
    }

    fn read_batch(&mut self, max: usize) -> io::Result<usize> {
        if self.remaining == 0 {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let count = self.remaining.min(max);
        self.remaining -= count;
        self.operations.lock().unwrap().push(Operation::Read(count));
        Ok(count)
    }

    fn batch_datagram(&self, _: usize) -> (&[u8], batch_read::Received) {
        (
            &[0x5A; 32],
            batch_read::Received {
                len: 1_000,
                source: Some(if self.foreign_source {
                    "127.0.0.2:40003".parse().unwrap()
                } else {
                    self.peer
                }),
                ..Default::default()
            },
        )
    }

    fn ppoll(&mut self, _: bool, _: Duration) -> io::Result<Woke> {
        unreachable!("a scripted turn never waits")
    }

    fn drain_queue(&mut self, _: &mut impl FnMut(TransportError)) -> (bool, bool) {
        (false, false)
    }

    fn pending_error(&mut self) -> Option<io::Error> {
        None
    }

    fn socket(&self) -> &UdpSocket {
        self.socket.as_ref().unwrap()
    }
}

#[derive(Clone, Copy)]
struct Fixed(Now);

impl Reading for Fixed {
    fn now(&self) -> Now {
        self.0
    }

    fn mono(&self) -> MonoTime {
        self.0.mono
    }
}

fn context() -> (Context<Wire>, reports::Receiver) {
    let (_, starts) = mpsc::channel();
    let (_, stops) = mpsc::channel();
    let (report, received) = reports::channel();
    let context = Context {
        shared: Arc::new(Shared {
            wake: EventFd::from_flags(EfdFlags::EFD_CLOEXEC | EfdFlags::EFD_NONBLOCK).unwrap(),
            serving: AtomicUsize::new(2),
            alive: AtomicBool::new(true),
            shutdown: AtomicBool::new(false),
            tid: AtomicI32::new(0),
        }),
        epoll: Epoll::new(EpollCreateFlags::EPOLL_CLOEXEC).unwrap(),
        starts,
        stops,
        clock: Clock::new(),
        settings: EngineSettings::default(),
        report,
        overflows: None,
        hooks: Hooks::default(),
    };
    (context, received)
}

fn open(
    thread: &mut EngineThread<Wire>,
    context: &Context<Wire>,
    data_phase: DataPhase,
    id: u32,
    wire: Wire,
) {
    thread.open(
        context,
        Handover {
            socket_id: SocketId(id),
            key: u64::from(id),
            data_phase,
            socket: UdpSocket::bind("127.0.0.1:0").unwrap(),
            connected: true,
            buffers: None,
            receive_buffer: 2 << 20,
            open: wire,
            reporter: ExitReporter::new(context.report.clone(), SocketId(id), None),
        },
    );
}

// ADR 0008's fairness contract: sends precede reads, each ready socket
// gets at most 256 datagrams per turn, and a socket with more to read is
// served on the next turn without another arrival. ADR 0012 permits
// Skipped Bursts after a host pause; that must not change these rules.
#[test]
fn a_flooded_socket_yields_to_sends_on_each_turn_even_after_a_host_pause() {
    for foreign_source in [false, true] {
        let operations = Arc::new(Mutex::new(Vec::new()));
        let now = Now {
            mono: MonoTime::from_duration(Duration::from_secs(1)),
            wall: WallTime::from_unix(Duration::from_secs(1_800_000_000)),
        };
        let peer = "127.0.0.1:40001".parse().unwrap();
        let server = "127.0.0.1:40002".parse().unwrap();
        let (_, sender, _) = activate(|| now, Direction::Downstream, 5, peer, server);
        let (_, flooded, _) = activate(|| now, Direction::Upstream, 5, peer, server);
        let first = sender.next_burst_at().unwrap();
        let wire = |remaining| Wire {
            operations: operations.clone(),
            remaining,
            foreign_source,
            peer,
            socket: None,
        };
        let (context, mut received) = context();
        let mut thread = EngineThread::new(&context);
        open(&mut thread, &context, sender, 1, wire(0));
        open(&mut thread, &context, flooded, 2, wire(513));

        // Row 5 sends one Load PDU every millisecond. Only scripted time
        // advances: the third turn follows a 50 ms host pause.
        for (millis, reads) in [(0, 256), (1, 256), (51, 1)] {
            let elapsed =
                first.as_duration() - now.mono.as_duration() + Duration::from_millis(millis);
            let at = Now {
                mono: MonoTime::from_duration(now.mono.as_duration() + elapsed),
                wall: WallTime::from_unix(now.wall.as_unix() + elapsed),
            };
            thread.turn(&context, &Fresh(Fixed(at)));
            let made = std::mem::take(&mut *operations.lock().unwrap());
            assert_eq!(made.first(), Some(&Operation::Send), "{made:?}");
            assert_eq!(
                made.iter().filter(|op| **op == Operation::Send).count(),
                1,
                "one due Load PDU: {made:?}"
            );
            let read: usize = made
                .iter()
                .filter_map(|op| match op {
                    Operation::Read(count) => Some(count),
                    Operation::Send => None,
                })
                .sum();
            assert_eq!(read, reads, "each turn's receive budget: {made:?}");
        }

        let mut skipped = 0;
        while let Ok(report) = received.try_recv() {
            if let FromEngine::Event {
                socket: SocketId(1),
                event: DataEvent::BurstsSkipped { datagrams, .. },
            } = report
            {
                skipped += datagrams;
            }
        }
        assert_eq!(skipped, 49, "the pause skips the 49 overdue Bursts");
    }
}
