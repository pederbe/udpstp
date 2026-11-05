#![forbid(unsafe_code)]
//! I/O for udpstp's protocol core: sockets, clocks and the Tokio
//! Control Phase (ADR 0003). It makes no protocol decisions; the core in
//! `udpstp-core` does.
//!
//! [`serve`] runs a Server until an error stops it; [`run_test`] runs one
//! Test as a Client. A Server on the Standard Port, with the core's
//! defaults:
//!
//! ```no_run
//! use udpstp_core::ServerConfig;
//! use udpstp_runtime::{
//!     DEFAULT_SOCKET_BUFFER_SIZE, EngineSettings, IpOptions, ServerNotice, ServerOptions, serve,
//! };
//!
//! # async fn run() -> std::io::Result<()> {
//! let options = ServerOptions {
//!     listen: Vec::new(), // the wildcard addresses
//!     port: 24601, // the Standard Port
//!     ephemeral_ports: 49152..=65535,
//!     ip: IpOptions::default(),
//!     socket_buffer_size: DEFAULT_SOCKET_BUFFER_SIZE,
//!     engine: EngineSettings::default(), // the engine's own choices
//!     core: ServerConfig::default(),
//! };
//! // Completes with its reason to stop the Server: here, never.
//! let stop = std::future::pending::<&'static str>();
//! serve(
//!     options,
//!     |notice| {
//!         if let ServerNotice::Listening(addresses) = notice {
//!             println!("listening on {addresses:?}");
//!         }
//!     },
//!     stop,
//! )
//! .await
//! # }
//! ```

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
#[path = "../../udpstp-core/tests/contract/mod.rs"]
mod contract;
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
