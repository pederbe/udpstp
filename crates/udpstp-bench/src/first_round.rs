//! The first Round: udpstp against OB-UDPST on two hosts, and
//! the home line, as `scripts/bench/round.py` runs a first-round matrix
//! and `scripts/bench/report.py` judges it.
//!
//! - [`matrix`]: the matrix, its Round Runs in order, and the checks and
//!   figures a Round's plan makes of them.
//! - [`commands`]: udpstp's and OB-UDPST's Client and Server command
//!   lines.
//! - [`judge`]: each Round Run's Maximum and Highest IP-Layer Capacity,
//!   and the criterion fixed before the first paid Round.
//! - [`report`]: the Round's report, byte for byte the Python's for legacy
//!   records, with separate advisory evidence for explicit Server caps.
//! - [`round`]: the Round's body, which `bench_rounds::round` runs: its
//!   keys, hosts, the path check, the matrix and its Servers.

pub mod capture;
pub mod commands;
mod diagnostics;
pub mod judge;
pub mod matrix;
mod metrics;
pub mod report;
pub mod round;
mod sender_evidence;
