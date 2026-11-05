#![forbid(unsafe_code)]
#![deny(clippy::exit)]
//! udpstp's benchmark Rounds, on `bench-rounds`: the first Round,
//! which compares udpstp with OB-UDPST on rented hosts, and the
//! Server-scale Round, which finds how many Test Connections one Server
//! carries. Its `udpstp-bench` binary plans and runs either Round type
//! and writes the reports, preserving the independent reference fixtures.
//!
//! It reads udpstp's JSON results and the Server's JSON log as any
//! consumer does, never through udpstp's crates, so the judging stays
//! independent of the code it judges. It holds the first Round
//! ([`first_round`]), the pinned OB-UDPST built on a host ([`obudpst`]),
//! the Server-scale modules ([`scale`]), and the command line
//! ([`cli`]).

pub mod campaign;
mod cleanup;
pub mod cli;
pub mod first_round;
pub mod obudpst;
pub mod precision;
pub mod proxy;
pub mod receive_pair;
pub mod receive_probe;
pub mod scale;
