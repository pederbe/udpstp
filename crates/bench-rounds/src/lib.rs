#![forbid(unsafe_code)]
#![deny(clippy::exit)]
//! The general machinery of a benchmark Round, which knows nothing
//! of udpstp: `udpstp-bench` holds udpstp's own Rounds on top of it.
//!
//! A Round is one pass of the benchmark tooling over a matrix on one set
//! of hosts: priced and planned, then provisioned. A Round makes its Round
//! Runs and ends with its Round Teardown, whatever happened.
//!
//! The Python tooling it replaces (`scripts/bench/`) is the reference
//! implementation of rules written down before each Round, so the port
//! reproduces the Python's results exactly, down to how Python reads JSON,
//! adds floats and rounds.
//!
//! Its adapters do the Round's I/O and decide nothing: [`interrupt`] takes
//! the signals, [`process`] runs programs, [`host`] reaches the hosts,
//! [`unpack`] takes their results, [`secrets`] keeps the keys, and a
//! [`provider`], such as [`static_hosts`], or AWS's through the `aws` CLI
//! ([`aws::cli`]), gives the hosts, whose resources [`round_teardown`]
//! destroys.
//!
//! Its modules that decide do so as the Python does: [`order`] puts the
//! Round Runs in order, [`cost`] prices a Round and keeps the ledger both
//! tools share, [`counters`] and [`samples`] read what a host measured,
//! [`dead_man`] writes a rented host's dead-man switch, and [`aws`] plans
//! AWS's calls and models EC2, with the private records of a Round's real
//! calls and their replay against that model.

pub mod aws;
pub mod cost;
pub mod counters;
pub mod dead_man;
pub mod host;
pub mod interrupt;
pub mod num;
pub mod order;
pub mod process;
pub mod provider;
pub mod py;
pub mod quote;
pub mod redact;
pub mod round;
pub mod round_teardown;
pub mod sampler;
pub mod samples;
pub mod secrets;
pub mod static_hosts;
pub mod stats;
pub mod unpack;
pub mod utc;
