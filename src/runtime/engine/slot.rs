//! A Test Connection slot: one Test Connection's Data Phase, its socket
//! and what its turns need, with the protocol work of a turn as methods
//!. An executor schedules the slots it owns and waits
//! between turns: today the per-Test-Connection executor, a thread that
//! owns one slot (`engine.rs`). Each method reads the clock through the
//! scheme its executor gives it (`times.rs`), which decides which reading
//! each core call gets.

use std::io;
use std::time::Duration;

use crate::protocol::{
    DataEvent, DataPhase, EndReason, MonoTime, Now, RxBatch, RxPdu, SendPlan, SocketId,
    TransportError,
};

use super::reports;
use super::times::Times;
use super::{EngineFacts, FromEngine};
use crate::runtime::buffers::SocketBuffers;
use crate::runtime::socket::send_stamps::Micros;
use crate::runtime::socket::{Adapter, Reads, Sink, Socket};

/// Room for any Status PDU the Data Phase writes.
const STATUS_BUF: usize = 2048;

/// The core calls a slot makes on its Test Connection's Data Phase: the
/// frozen Data Phase interface's own, which [`DataPhase`] answers, and in
/// tests a recording of them that says which reading each call got.
pub(crate) trait Core {
    fn is_load_receiver(&self) -> bool;
    fn take_load_bytes(&mut self) -> u64;
    fn stop(&mut self, now: Now);
    fn poll_burst(&mut self, now: Now, horizon: Duration) -> Option<SendPlan>;
    fn write_datagrams(&mut self, plan: &SendPlan, now: Now, buf: &mut [u8]);
    fn report_sent(&mut self, plan: &SendPlan, sent: u32, now: Now);
    fn handle_load_batch(&mut self, batch: RxBatch<'_>);
    fn handle_pdu(&mut self, pdu: RxPdu<'_>);
    fn handle_transport_error(&mut self, now: Now, error: TransportError);
    fn handle_timeout(&mut self, now: Now);
    fn poll_transmit(&mut self, now: Now, buf: &mut [u8]) -> Option<usize>;
    fn poll_event(&mut self) -> Option<DataEvent>;
    fn next_burst_at(&self) -> Option<MonoTime>;
    fn poll_timeout(&self) -> Option<MonoTime>;
}

impl Core for DataPhase {
    fn is_load_receiver(&self) -> bool {
        DataPhase::is_load_receiver(self)
    }
    fn take_load_bytes(&mut self) -> u64 {
        DataPhase::take_load_bytes(self)
    }
    fn stop(&mut self, now: Now) {
        DataPhase::stop(self, now);
    }

    fn poll_burst(&mut self, now: Now, horizon: Duration) -> Option<SendPlan> {
        DataPhase::poll_burst(self, now, horizon)
    }

    fn write_datagrams(&mut self, plan: &SendPlan, now: Now, buf: &mut [u8]) {
        DataPhase::write_datagrams(self, plan, now, buf);
    }

    fn report_sent(&mut self, plan: &SendPlan, sent: u32, now: Now) {
        DataPhase::report_sent(self, plan, sent, now);
    }

    fn handle_load_batch(&mut self, batch: RxBatch<'_>) {
        DataPhase::handle_load_batch(self, batch);
    }

    fn handle_pdu(&mut self, pdu: RxPdu<'_>) {
        DataPhase::handle_pdu(self, pdu);
    }

    fn handle_transport_error(&mut self, now: Now, error: TransportError) {
        DataPhase::handle_transport_error(self, now, error);
    }

    fn handle_timeout(&mut self, now: Now) {
        DataPhase::handle_timeout(self, now);
    }

    fn poll_transmit(&mut self, now: Now, buf: &mut [u8]) -> Option<usize> {
        DataPhase::poll_transmit(self, now, buf)
    }

    fn poll_event(&mut self) -> Option<DataEvent> {
        DataPhase::poll_event(self)
    }

    fn next_burst_at(&self) -> Option<MonoTime> {
        DataPhase::next_burst_at(self)
    }

    fn poll_timeout(&self) -> Option<MonoTime> {
        DataPhase::poll_timeout(self)
    }
}

/// One Test Connection's Data Phase on its socket.
pub(crate) struct Slot<A, D: Core = DataPhase> {
    report: reports::Sender,
    load_bytes: u64,
    load_receiver: bool,
    reported_load_bytes: u64,
    counters: reports::Counters,
    socket_id: SocketId,
    data_phase: D,
    socket: Socket<A>,
    /// Whether its Data Phase has been told to stop.
    stop_sent: bool,
    ended: Option<EndReason>,
    /// What it read since the executor last took it, for the next wait.
    reads: Reads,
    /// The Send Plans' datagrams, written just before they are sent: as
    /// large as the largest Send Plan so far.
    send_buf: Vec<u8>,
    status_buf: [u8; STATUS_BUF],
    /// How late each Send Plan was written, after its scheduled time
    ///: its Burst lateness.
    lateness: Micros,
}

/// What the socket hands on goes to the Test Connection's Data Phase.
struct ToDataPhase<'a, D, T> {
    data_phase: &'a mut D,
    times: &'a T,
}

impl<D: Core, T: Times> Sink for ToDataPhase<'_, D, T> {
    fn status_pdu(&mut self, pdu: RxPdu<'_>) {
        self.data_phase.handle_pdu(pdu);
    }

    fn load_pdus(&mut self, batch: RxBatch<'_>) {
        self.data_phase.handle_load_batch(batch);
    }

    fn path_error(&mut self, error: TransportError) {
        self.data_phase
            .handle_transport_error(self.times.now(), error);
    }
}

impl<A: Adapter, D: Core> Slot<A, D> {
    /// The slot of Test Connection `socket_id`, whose Data Phase runs on
    /// `socket`, opened.
    pub(crate) fn new(
        socket_id: SocketId,
        data_phase: D,
        socket: Socket<A>,
        report: reports::Sender,
    ) -> Self {
        Self {
            report,
            load_bytes: 0,
            load_receiver: data_phase.is_load_receiver(),
            reported_load_bytes: 0,
            counters: reports::Counters::default(),
            socket_id,
            data_phase,
            socket,
            stop_sent: false,
            ended: None,
            reads: Reads::default(),
            send_buf: Vec::new(),
            status_buf: [0; STATUS_BUF],
            lateness: Micros::default(),
        }
    }

    /// Tells its Data Phase to stop, once: the STOP exchange follows.
    pub(crate) fn stop(&mut self, times: &impl Times) {
        if !self.stop_sent {
            self.stop_sent = true;
            self.data_phase.stop(times.now());
        }
    }

    /// Every due Burst, whole, its headers written just before sending,
    /// until `until` on the Test Time Base: a host that can't send as fast
    /// as the schedule still gets to its timeouts and its STOP, and the
    /// Bursts it didn't reach become Skipped Bursts. Between the Bursts of
    /// a catch-up (ADR 0012) it reads what arrived, so a Status PDU's
    /// receive time isn't held back by the catch-up and the RTT sample
    /// stays true. Lateness is judged from a fresh reading, and each Send
    /// Plan is stamped with one.
    pub(crate) fn send_due(&mut self, times: &impl Times, until: Duration) -> io::Result<()> {
        let mut now = times.now();
        while let Some(plan) = self.data_phase.poll_burst(now, Duration::ZERO) {
            if self.send_buf.len() < plan.bytes() {
                self.send_buf.resize(plan.bytes(), 0);
            }
            let stamped = times.now();
            self.lateness.record(
                stamped
                    .mono
                    .as_duration()
                    .saturating_sub(plan.scheduled.as_duration()),
            );
            self.data_phase
                .write_datagrams(&plan, stamped, &mut self.send_buf);
            let mut sink = ToDataPhase {
                data_phase: &mut self.data_phase,
                times,
            };
            let sent = self
                .socket
                .send_plan(&plan, &self.send_buf, stamped.mono, &mut sink)?;
            self.data_phase.report_sent(&plan, sent, times.now());
            // The send buffer is full: the rest waits for the next turn.
            if sent < plan.datagrams() {
                break;
            }
            let at = times.latest().mono;
            if at.as_duration() >= until {
                break;
            }
            if self
                .data_phase
                .next_burst_at()
                .is_some_and(|next| next <= at)
            {
                self.read(times)?;
            }
            now = times.latest();
        }
        Ok(())
    }

    /// Hands every datagram from the peer that has arrived, up to the
    /// socket's budget, to the Data Phase, and counts what it read: what
    /// this read read.
    pub(crate) fn read(&mut self, times: &impl Times) -> io::Result<Reads> {
        let mut sink = ToDataPhase {
            data_phase: &mut self.data_phase,
            times,
        };
        let reads = self.socket.receive(times, &mut sink)?;
        self.reads.datagrams += reads.datagrams;
        self.reads.bytes += reads.bytes;
        Ok(reads)
    }

    /// Drains the errors its Engine Thread's epoll set reported pending on
    /// its socket (EPOLLERR), handing them to its Data Phase: whether there
    /// were any (Linux).
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub(crate) fn drain_errors(&mut self, times: &impl Times) -> bool {
        let mut sink = ToDataPhase {
            data_phase: &mut self.data_phase,
            times,
        };
        self.socket.drain_pending(&mut sink)
    }

    /// Handles the Data Phase's timeout, if it is due.
    pub(crate) fn timeouts(&mut self, times: &impl Times) {
        if let Some(now) = times.due(self.data_phase.poll_timeout()) {
            self.data_phase.handle_timeout(now);
        }
    }

    /// Sends every Status PDU that is due.
    pub(crate) fn transmit_status(&mut self, times: &impl Times) -> io::Result<()> {
        while let Some(len) = self
            .data_phase
            .poll_transmit(times.now(), &mut self.status_buf)
        {
            let mut sink = ToDataPhase {
                data_phase: &mut self.data_phase,
                times,
            };
            self.socket.send(&self.status_buf[..len], &mut sink)?;
        }
        Ok(())
    }

    /// Passes the Data Phase's events on to `report`; whether it has
    /// ended.
    pub(crate) fn forward_events(&mut self, report: &reports::Sender) -> bool {
        while let Some(event) = self.data_phase.poll_event() {
            if let DataEvent::LoadBytes { bytes } = event {
                if self.load_receiver {
                    self.load_bytes = self.load_bytes.saturating_add(bytes);
                }
                continue;
            }
            if let DataEvent::Ended(reason) = event {
                self.ended = Some(reason);
                self.report_load_bytes();
            }
            if !report.send(FromEngine::Event {
                socket: self.socket_id,
                event,
            }) {
                self.counters.retain(event);
            }
        }
        self.ended.is_some()
    }

    /// How its Data Phase ended, once it has.
    pub(crate) fn ended(&self) -> Option<EndReason> {
        self.ended
    }

    /// When it next has work: its next Burst or its Data Phase's next
    /// timeout, whichever comes first.
    pub(crate) fn next_deadline(&self) -> Option<MonoTime> {
        [
            self.data_phase.next_burst_at(),
            self.data_phase.poll_timeout(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Whether its next Burst is due at `at`.
    #[cfg_attr(not(any(target_os = "linux", target_os = "android")), allow(dead_code))]
    pub(crate) fn burst_due(&self, at: MonoTime) -> bool {
        self.data_phase
            .next_burst_at()
            .is_some_and(|next| next <= at)
    }

    /// Whether its Data Phase's timeout is due at `at`.
    #[cfg_attr(not(any(target_os = "linux", target_os = "android")), allow(dead_code))]
    pub(crate) fn timeout_due(&self, at: MonoTime) -> bool {
        self.data_phase
            .poll_timeout()
            .is_some_and(|next| next <= at)
    }

    /// Its socket joined its Engine Thread's epoll set (Linux).
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub(crate) fn joined_epoll(&mut self) {
        self.socket.joined_epoll();
    }

    /// Its socket itself (Linux).
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub(crate) fn socket(&self) -> &std::net::UdpSocket {
        self.socket.handle()
    }

    /// What it read since this was last asked.
    pub(crate) fn take_reads(&mut self) -> Reads {
        std::mem::take(&mut self.reads)
    }

    /// Waits on its socket, as [`Socket::ready`] does, handing a drained
    /// error to its Data Phase (Linux).
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub(crate) fn ready(
        &mut self,
        times: &impl Times,
        readable: bool,
        timeout: Duration,
    ) -> Option<(crate::runtime::socket::Woke, bool)> {
        let mut sink = ToDataPhase {
            data_phase: &mut self.data_phase,
            times,
        };
        self.socket.ready(readable, timeout, &mut sink)
    }

    /// How its Data Phase ran on this host, with the socket buffers it got
    /// as it started: what its socket says.
    pub(crate) fn facts(&self, socket_buffers: Option<SocketBuffers>) -> EngineFacts {
        EngineFacts {
            receive_buffer_overflows: self.socket.receive_buffer_overflows(),
            ..self.facts_but_overflows(socket_buffers)
        }
    }

    /// As [`Slot::facts`], but for its Receive Buffer Overflows, which are
    /// said apart, and never read from the process's UDP tables, which
    /// would stall every other Test Connection of its Engine Thread
    /// (Linux).
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub(crate) fn facts_on_engine_thread(
        &self,
        socket_buffers: Option<SocketBuffers>,
    ) -> (EngineFacts, crate::runtime::socket::OverflowCount) {
        (
            self.facts_but_overflows(socket_buffers),
            self.socket.overflow_count(),
        )
    }

    fn facts_but_overflows(&self, socket_buffers: Option<SocketBuffers>) -> EngineFacts {
        EngineFacts {
            socket_buffers,
            fast_paths: self.socket.fast_paths(),
            receive_buffer_overflows: None,
            send_lag: self.socket.send_lag(),
            burst_lateness: self.lateness.summary(),
        }
    }

    /// Its Data Phase, for a test to read what it recorded.
    #[cfg(test)]
    pub(crate) fn data_phase(&self) -> &D {
        &self.data_phase
    }
}

impl<A, D: Core> Slot<A, D> {
    fn report_load_bytes(&mut self) {
        self.counters.finish(&self.report, self.socket_id);
        let pending = self.data_phase.take_load_bytes();
        let total = if self.load_receiver {
            self.load_bytes = self.load_bytes.saturating_add(pending);
            self.load_bytes
        } else {
            self.socket.load_bytes_sent()
        };
        let bytes = total.saturating_sub(self.reported_load_bytes);
        self.reported_load_bytes = total;
        if bytes > 0 {
            self.report.send(FromEngine::Event {
                socket: self.socket_id,
                event: DataEvent::LoadBytes { bytes },
            });
        }
    }
}

// Before the exit report releases Admission, even when a socket error or
// panic drops the slot before its next normal event-forwarding turn.
impl<A, D: Core> Drop for Slot<A, D> {
    fn drop(&mut self) {
        self.report_load_bytes();
    }
}
