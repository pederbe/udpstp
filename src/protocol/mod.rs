#![forbid(unsafe_code)]
//! Sans-IO UDPSTP (RFC 9946) protocol core.
//!
//! The core never performs I/O and never reads a clock. A runtime (or the
//! test simulator) drives each [`Endpoint`]:
//!
//! - it passes in received datagrams with their [`DatagramMeta`], socket
//!   lifecycle notifications, and deadline expiries, always with the current
//!   [`Now`] (monotonic and wall-clock time together);
//! - it drains [`Output`]s: datagrams to send from a given socket, sockets to
//!   open or close, and typed [`Event`]s;
//! - it asks [`Endpoint::poll_timeout`] when to call
//!   [`Endpoint::handle_timeout`] next.
//!
//! A [`Server`] or a [`Coordinator`] (the Client's side of a Test) is an
//! [`Endpoint`]. A runtime's loop looks like this:

mod admission;
mod algorithm;
mod budget;
mod checksum_use;
mod client;
mod coercion;
mod coordinator;
mod data_phase;
#[cfg(feature = "layout-diagnostics")]
#[doc(hidden)]
pub use data_phase::layout_diagnostics;
mod direction;
mod event;
mod interval;
mod io;
mod keys;
mod parameters;
mod quota;
mod replay;
mod results;
mod search;
mod sending_rate_table;
mod server;
mod time;

pub use client::{ClientAuth, ClientConfig, DelayVariation, RateAdjAlgorithm, RowRequest};
pub use coercion::{ActivationParameter, Coercion};
pub use coordinator::{Coordinator, TestPlan, mc_ident_for};
pub use data_phase::{
    CATCH_UP_BUDGET, DataEvent, DataPhase, EndReason, RxBatch, RxDatagram, RxPdu, SendPlan,
    TransportError,
};
pub use direction::Direction;
pub use event::{
    ActivationRejection, AuthenticationFailure, ControlExchange, ControlPhaseFailure, Event,
    MultiConnectionError, ResponseIgnoreReason, SetupIgnoreReason, SetupRejection,
    SetupRejectionCode, SilentRejectionReason, TestAbort, TestParameters,
};
pub use interval::{DelayVarStatistic, TrialIntervalCounts};
pub use io::{DatagramMeta, IpVersion, Output, SocketId, Transmit};
pub use keys::{AlgId, Kdf, KeyEntry, KeyRole, KeyTable, KeyTableError, Lifetime};
pub use parameters::DscpSet;
pub use quota::QuotaLimits;
pub use results::{
    CapacityModeResult, Evaluation, FASTER_SUB_INTERVAL_MARGIN_PERCENT, LoadSender,
    MeasurementResults, PerformanceCriterion, SentInterval, SubIntervalMeasurement,
};
pub use search::{FixedRateTest, SENDING_RATE_MARGIN_PERCENT, SearchShortfall, SearchTest};
pub use sending_rate_table::TableShape;
pub use server::{
    DEFAULT_KEY_SETUP_BURST, DEFAULT_KEY_SETUPS_PER_SECOND, DEFAULT_MAX_START_ROW,
    DEFAULT_MAX_TEST_CONNECTIONS, DEFAULT_MAX_TEST_SHARE, DEFAULT_QUOTA_PERIOD,
    DEFAULT_SOURCE_SETUP_BURST, DEFAULT_SOURCE_SETUPS_PER_SECOND,
    DEFAULT_TEST_CONNECTIONS_PER_TEST, DEFAULT_UNVERIFIED_SETUPS_PER_SECOND, Server, ServerConfig,
};
pub use time::{MonoTime, Now, WallTime};

/// Drivers for the fuzz targets (fuzz/), which reach modules the public
/// interface doesn't expose. The `fuzzing` feature, which only the fuzz
/// targets enable, compiles them; they are no stable interface.
#[cfg(feature = "fuzzing")]
#[doc(hidden)]
pub mod fuzzing {
    pub use crate::protocol::admission::drive as admission;
}

#[cfg(test)]
pub(crate) use interval::{Classification, IntervalClass};
#[cfg(test)]
pub(crate) use results::{DelayRange, SubIntervalsAfterMaximumMissed};

use std::net::SocketAddr;

/// The interface every protocol endpoint (Client or Server) offers its driver.
pub trait Endpoint {
    /// A datagram arrived on one of this endpoint's sockets. `payload` may be
    /// shorter than `meta.actual_len` when the runtime read it truncated.
    fn handle_datagram(&mut self, now: Now, meta: DatagramMeta, payload: &[u8]);

    /// A socket requested with [`Output::OpenSocket`] is now bound at `local`.
    fn handle_socket_opened(&mut self, now: Now, socket: SocketId, local: SocketAddr);

    /// A socket is gone: closed on request ([`Output::CloseSocket`]), never
    /// opened, or failed. Any admission held for it is released.
    ///
    /// A Test Connection's socket whose Data Phase a Data Phase Engine runs
    /// goes when the engine exits, on every exit: its Data Phase ended, or
    /// the engine failed or panicked. The runtime reports it then, after
    /// passing on the engine's last [`DataEvent`] with
    /// [`Endpoint::handle_data_event`], never before; the core never asks
    /// to close such a socket itself. So whatever the engine reported of the
    /// Test Connection comes before its release. For a Data Phase that
    /// ended the call changes nothing on the Client side, and releases the
    /// Test Connection on the Server.
    fn handle_socket_closed(&mut self, now: Now, socket: SocketId);

    /// The deadline from [`Endpoint::poll_timeout`] has been reached.
    fn handle_timeout(&mut self, now: Now);

    /// The next monotonic time at which [`Endpoint::handle_timeout`] must be called.
    fn poll_timeout(&self) -> Option<MonoTime>;

    /// The next output to act on, in order.
    fn poll_output(&mut self) -> Option<Output>;

    /// The Data Phase announced by [`Output::StartDataPhase`] for `socket`,
    /// for the runtime to hand to its Data Phase Engine.
    fn take_data_phase(&mut self, socket: SocketId) -> Option<DataPhase>;

    /// The Data Phase on `socket` reported `event`, which the runtime passes
    /// on in order: the Multi-Connection Coordinator follows each Test
    /// Connection's Data Phase by them, and the Server counts each Test's
    /// load bytes for its Quotas. The engine's exit follows, with
    /// [`Endpoint::handle_socket_closed`].
    fn handle_data_event(&mut self, _now: Now, _socket: SocketId, _event: &DataEvent) {}

    /// The Client user interrupted the Test (Ctrl-C, or a signal to stop).
    /// Only the Client's Multi-Connection Coordinator acts on it: it stops
    /// the Test as [`TestAbort::Interrupted`].
    fn handle_interrupt(&mut self, _now: Now) {}

    /// A socket reported a transport error outside a Data Phase Engine,
    /// during the Control Phase (ADR 0006). Only the Client's
    /// Multi-Connection Coordinator acts on it.
    fn handle_transport_error(&mut self, _now: Now, _socket: SocketId, _error: TransportError) {}
}
