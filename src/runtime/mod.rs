#![forbid(unsafe_code)]
//! Application sockets, clocks and Control Phase loops (ADR 0003).

// Counts the test binary's allocations, for the check that the Data Phase
// Engine allocates nothing in its steady state (ADR 0008): test
// builds only.
#[cfg(test)]
#[global_allocator]
static ALLOCATOR: &stats_alloc::StatsAlloc<std::alloc::System> = &stats_alloc::INSTRUMENTED_SYSTEM;

mod arrival;
mod buffers;
mod client;
mod clock;
// The plan-executor contract's checks, and the in-memory Control Phase that
// gives its executors their Data Phases, shared with the core's tests.
#[cfg(test)]
use crate::tests::protocol::contract;
#[cfg(test)]
mod contract_tests;
#[cfg(all(test, any(target_os = "linux", target_os = "android")))]
mod control_phase_tests;
mod engine;
#[cfg(test)]
mod fault_tests;
mod ip_options;
mod server;
mod socket;
mod transport;
// Only Linux waits in ppoll; its arithmetic is tested everywhere.
#[cfg_attr(not(any(target_os = "linux", target_os = "android")), allow(dead_code))]
mod wait;

pub use buffers::{
    DEFAULT_SOCKET_BUFFER_SIZE, SOCKET_BUFFER_LIMITS, SocketBuffers, probe_socket_buffers,
};
pub use client::{ClientOutcome, ClientResult, TestOutcome, resolve, run_test};
pub use engine::{EngineFacts, EngineSettings, Executor};
pub use ip_options::{DEFAULT_MAX_HOPS, DONT_FRAGMENT_SUPPORTED, DSCP_SUPPORTED, IpOptions};
pub use server::{ServerNotice, ServerOptions, serve};
pub use socket::send_stamps::Numbering as SendStampNumbering;
pub use socket::{FastPaths, SendLag};
