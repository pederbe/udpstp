//! What an unverified Setup Request costs the Server core.
//!
//! Drives a [`Server`] with Setup Requests whose authDigest is wrong, as a
//! flood's would be (the sender doesn't hold the Shared Key), and times
//! `handle_datagram` and the draining of its outputs, per request, in four
//! cases:
//!
//! - **cache miss**: a valid keyId with a new authUnixTime in every
//!   request. The Derived Keys of a time outside the ±5 s window are derived
//!   but not kept, and the window is checked after the digest (RFC 9946
//!   §6.2), so each request costs a KDF run and an HMAC before its Silent
//!   Rejection: the worst case the global budget for unverified Setup
//!   Requests lets through.
//! - **cache hit**: the same authUnixTime, within the window, in every
//!   request, its Derived Keys cached: the HMAC alone.
//! - **budget spent**: the budget is spent, so the request is dropped
//!   before any KDF run or HMAC: what a flood beyond the budget costs.
//! - **unknown keyId**: no Key Table row has the keyId, so the request is
//!   dropped before it can spend the budget.
//!
//! Each case's work is checked, not assumed: the Server's KDF counter and
//! the reason of every Silent Rejection must show it. The KDF and the HMAC
//! are also timed alone, through udpstp-wire, for comparison.
//!
//! ```text
//! cargo run --release -p udpstp --example unverified_setup_cost -- [--runs N] [--rows N]
//! ```
//!
//! Each case gets `--runs` timed runs (11 by default), interleaved with the
//! other cases' so that a change in the host's load or clock speed touches
//! them all; the median run is reported, with the fastest and the slowest.
//! `--rows` (1 by default, at most 255) fills the Key Table with that many
//! rows, one per keyId, and the requests use the last row's keyId, so the
//! lookup passes them all. The last lines give the share of one core the
//! default budget takes on this host at the cache-miss cost, and the budget
//! each share would allow.

#[path = "../src/protocol/mod.rs"]
#[allow(dead_code, unused_imports)]
mod protocol;
#[path = "../src/wire/mod.rs"]
#[allow(dead_code, unused_imports)]
mod wire;
use std::hint::black_box;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use crate::protocol::{
    DEFAULT_UNVERIFIED_SETUPS_PER_SECOND, DatagramMeta, Endpoint, Event, IpVersion, KeyEntry,
    KeyTable, Lifetime, MonoTime, Now, Output, Server, ServerConfig, SilentRejectionReason,
    SocketId, WallTime,
};
use crate::wire::setup::{SETUP_PDU_LEN, cmd_request, modifier};
use crate::wire::{AUTH_DIGEST_LEN, PROTOCOL_VERSION, SetupPdu, auth::derive_keys, auth_mode};

/// The Server's wall clock, in Unix seconds. Both its clocks stand still:
/// only the budget reads the monotonic one, and a time that stood still
/// keeps the cache hit's authUnixTime within the window.
const NOW_UNIX: u32 = 1_790_000_000;

const NOW: Now = Now {
    mono: MonoTime::from_duration(Duration::from_secs(1)),
    wall: WallTime::from_unix(Duration::from_secs(NOW_UNIX as u64)),
};

/// Every request arrives on the Standard Port from the same address: the
/// Setup path keeps nothing per Source before a request is verified.
const META: DatagramMeta = DatagramMeta {
    socket: SocketId::PRIMARY,
    source: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 1), 40_000)),
    local: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(198, 51, 100, 1), 24_601)),
    ip_version: IpVersion::V4,
    tos: None,
    actual_len: SETUP_PDU_LEN,
};

/// Every row's Shared Key: 32 bytes, the length docs/security.md
/// recommends. A key longer than SHA-256's 64-byte block would be hashed
/// first, so each KDF run would cost more.
const SHARED_KEY: [u8; 32] = [0x5C; 32];

/// The keyId no row has.
const UNKNOWN_KEY_ID: u8 = 255;

/// The first cache-miss authUnixTime: an hour before the Server's clock,
/// far outside the window. Each request takes the second before the last.
const FIRST_STALE_TIME: u32 = NOW_UNIX - 3600;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Case {
    CacheMiss,
    CacheHit,
    BudgetSpent,
    UnknownKeyId,
    /// The wire crate's KDF alone.
    KdfAlone,
    /// The wire crate's HMAC check of a received Setup Request alone.
    HmacAlone,
}

impl Case {
    const ALL: [Case; 6] = [
        Case::CacheMiss,
        Case::CacheHit,
        Case::BudgetSpent,
        Case::UnknownKeyId,
        Case::KdfAlone,
        Case::HmacAlone,
    ];

    fn name(self) -> &'static str {
        match self {
            Case::CacheMiss => "cache miss",
            Case::CacheHit => "cache hit",
            Case::BudgetSpent => "budget spent",
            Case::UnknownKeyId => "unknown keyId",
            Case::KdfAlone => "KDF alone (udpstp-wire)",
            Case::HmacAlone => "HMAC alone (udpstp-wire)",
        }
    }

    /// Requests in one timed run: a few tenths of a second each on a 2017
    /// laptop core without SHA extensions.
    fn requests(self) -> u32 {
        match self {
            Case::CacheMiss | Case::KdfAlone => 100_000,
            Case::CacheHit | Case::HmacAlone => 200_000,
            Case::BudgetSpent | Case::UnknownKeyId => 2_000_000,
        }
    }

    /// Why the Server must silently reject every request of the case.
    fn reason(self) -> Option<SilentRejectionReason> {
        match self {
            Case::CacheMiss | Case::CacheHit => Some(SilentRejectionReason::AuthenticationFailed),
            Case::BudgetSpent => Some(SilentRejectionReason::UnverifiedBudgetExhausted),
            Case::UnknownKeyId => Some(SilentRejectionReason::UnknownKeyId),
            Case::KdfAlone | Case::HmacAlone => None,
        }
    }

    /// KDF runs each request must cost the Server.
    fn kdf_runs_per_request(self) -> u64 {
        u64::from(self == Case::CacheMiss)
    }
}

/// A case's Server, its request, and the next cache-miss authUnixTime.
struct Bench {
    case: Case,
    server: Server,
    request: [u8; SETUP_PDU_LEN],
    stale_time: u32,
    /// Nanoseconds per request, one entry per timed run.
    runs: Vec<f64>,
}

impl Bench {
    fn new(case: Case, rows: u8) -> Self {
        let mut keys = KeyTable::default();
        for key_id in 0..rows {
            let row = KeyEntry::new(
                &format!("row {key_id}"),
                key_id,
                SHARED_KEY.to_vec(),
                Lifetime::ALWAYS,
                Lifetime::ALWAYS,
            );
            keys.insert(row).expect("one row per keyId");
        }
        // High enough never to drop a request, except where the case is
        // the drop: then one, which the warm-up spends.
        let unverified_setups_per_second = match case {
            Case::BudgetSpent => 1,
            _ => u32::MAX,
        };
        let server = Server::new(ServerConfig {
            keys,
            unverified_setups_per_second,
            ..ServerConfig::default()
        });
        let key_id = match case {
            Case::UnknownKeyId => UNKNOWN_KEY_ID,
            _ => rows - 1,
        };
        let request = SetupPdu {
            protocol_version: PROTOCOL_VERSION,
            mc_index: 0,
            mc_count: 1,
            mc_ident: 0x0201,
            cmd_request: cmd_request::SETUP_REQUEST,
            cmd_response: 0,
            max_bandwidth: 0,
            test_port: 0,
            modifier_bitmap: modifier::JUMBO_STATUS,
            auth_mode: auth_mode::MODE_1,
            auth_unix_time: NOW_UNIX,
            // Not the HMAC of the request under any key the Server holds.
            auth_digest: [0xA5; AUTH_DIGEST_LEN],
            key_id,
            checksum: 0,
        }
        .encode();
        Self {
            case,
            server,
            request,
            stale_time: FIRST_STALE_TIME,
            runs: Vec::new(),
        }
    }

    /// Sends `requests` requests, and how long they took. Panics unless the
    /// Server silently rejected each for the case's reason, with the case's
    /// KDF runs and nothing else.
    fn run(&mut self, requests: u32) -> Duration {
        match self.case {
            Case::KdfAlone => return self.kdf_alone(requests),
            Case::HmacAlone => return self.hmac_alone(requests),
            _ => {}
        }
        let reason = self.case.reason();
        let kdf_runs = self.server.kdf_runs();
        let (mut rejected, mut other) = (0u32, 0u32);
        let start = Instant::now();
        for _ in 0..requests {
            if self.case == Case::CacheMiss {
                self.stale_time -= 1;
                self.request[16..20].copy_from_slice(&self.stale_time.to_be_bytes());
            }
            self.server
                .handle_datagram(NOW, META, black_box(&self.request[..]));
            while let Some(output) = self.server.poll_output() {
                match output {
                    Output::Event(Event::SetupRequestSilentlyRejected { reason: r, .. })
                        if Some(r) == reason =>
                    {
                        rejected += 1
                    }
                    _ => other += 1,
                }
            }
        }
        let elapsed = start.elapsed();
        assert_eq!(
            (rejected, other),
            (requests, 0),
            "{}: every request rejected silently as {reason:?}, nothing else",
            self.case.name()
        );
        assert_eq!(
            self.server.kdf_runs() - kdf_runs,
            u64::from(requests) * self.case.kdf_runs_per_request(),
            "{}: KDF runs",
            self.case.name()
        );
        elapsed
    }

    /// The cache miss's KDF runs alone: the same Shared Key, the same
    /// authUnixTimes.
    fn kdf_alone(&mut self, requests: u32) -> Duration {
        let mut sum = 0u8;
        let start = Instant::now();
        for _ in 0..requests {
            self.stale_time -= 1;
            let keys = derive_keys(black_box(&SHARED_KEY), self.stale_time);
            sum = sum.wrapping_add(keys.client()[0]);
        }
        let elapsed = start.elapsed();
        black_box(sum);
        elapsed
    }

    /// The HMAC check of each request alone, with a Client Authentication
    /// Key the request wasn't signed with.
    fn hmac_alone(&mut self, requests: u32) -> Duration {
        let key = derive_keys(&SHARED_KEY, NOW_UNIX);
        let mut verified = 0u32;
        let start = Instant::now();
        for _ in 0..requests {
            verified += u32::from(SetupPdu::verify_received(
                black_box(&self.request),
                key.client(),
            ));
        }
        let elapsed = start.elapsed();
        assert_eq!(verified, 0, "no request verifies");
        elapsed
    }

    fn median(&self) -> f64 {
        let mut runs = self.runs.clone();
        runs.sort_by(f64::total_cmp);
        runs[runs.len() / 2]
    }

    fn min(&self) -> f64 {
        self.runs.iter().copied().fold(f64::INFINITY, f64::min)
    }

    fn max(&self) -> f64 {
        self.runs.iter().copied().fold(0.0, f64::max)
    }
}

struct Options {
    runs: usize,
    rows: u8,
}

fn options() -> Result<Options, String> {
    let mut options = Options { runs: 11, rows: 1 };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |name: &str| {
            args.next()
                .ok_or_else(|| format!("{name} needs a value"))
                .and_then(|v| v.parse::<usize>().map_err(|e| format!("{name} {v}: {e}")))
        };
        match arg.as_str() {
            "--runs" => options.runs = value("--runs")?.max(1),
            "--rows" => {
                options.rows = match value("--rows")? {
                    rows @ 1..=255 => rows as u8,
                    rows => return Err(format!("--rows {rows}: from 1 to 255")),
                }
            }
            "-h" | "--help" => {
                return Err("usage: unverified_setup_cost [--runs N] [--rows N]".into());
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(options)
}

/// Whether this CPU has the SHA-256 instructions sha2 uses when present.
fn sha_extensions() -> &'static str {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    let present =
        std::arch::is_x86_feature_detected!("sha") && std::arch::is_x86_feature_detected!("sse4.1");
    #[cfg(target_arch = "aarch64")]
    let present = std::arch::is_aarch64_feature_detected!("sha2");
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
    let present = false;
    if present { "yes" } else { "no" }
}

fn main() -> ExitCode {
    let options = match options() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    if cfg!(debug_assertions) {
        eprintln!("warning: a debug build; measure with --release");
    }
    let mut benches: Vec<Bench> = Case::ALL
        .iter()
        .map(|&case| Bench::new(case, options.rows))
        .collect();
    // Untimed: one request (a KDF run and an HMAC) caches the cache hit's
    // Derived Keys and spends the budget-spent case's budget, then a short
    // run of each case brings the core up to speed.
    for bench in &mut benches {
        if matches!(bench.case, Case::CacheHit | Case::BudgetSpent) {
            bench.server.handle_datagram(NOW, META, &bench.request);
            while bench.server.poll_output().is_some() {}
        }
        bench.run(bench.case.requests() / 10);
    }
    for _ in 0..options.runs {
        for bench in &mut benches {
            let requests = bench.case.requests();
            let elapsed = bench.run(requests);
            bench
                .runs
                .push(elapsed.as_nanos() as f64 / f64::from(requests));
        }
    }

    println!("The cost of an unverified Setup Request to the Server core");
    println!(
        "{} {}, SHA extensions: {}; Key Table rows: {}; median of {} runs",
        std::env::consts::OS,
        std::env::consts::ARCH,
        sha_extensions(),
        options.rows,
        options.runs
    );
    println!();
    println!(
        "{:<26} {:>12} {:>9} {:>9} {:>12}",
        "case", "ns/request", "min", "max", "requests/run"
    );
    for bench in &benches {
        println!(
            "{:<26} {:>12.1} {:>9.1} {:>9.1} {:>12}",
            bench.case.name(),
            bench.median(),
            bench.min(),
            bench.max(),
            bench.case.requests()
        );
    }
    let cost = |case: Case| {
        benches
            .iter()
            .find(|b| b.case == case)
            .map_or(f64::NAN, Bench::median)
    };
    let default = f64::from(DEFAULT_UNVERIFIED_SETUPS_PER_SECOND);
    let share = |ns: f64, per_second: f64| ns * per_second / 1e9 * 100.0;
    println!();
    println!(
        "The default budget, {DEFAULT_UNVERIFIED_SETUPS_PER_SECOND} a second, takes {:.1} % of one core at the cache-miss cost ({:.1} % at the cache hit's).",
        share(cost(Case::CacheMiss), default),
        share(cost(Case::CacheHit), default),
    );
    println!(
        "A million requests a second beyond the budget take {:.1} % of one core.",
        share(cost(Case::BudgetSpent), 1e6),
    );
    let budgets: Vec<String> = [5.0, 10.0, 25.0, 50.0]
        .iter()
        .map(|percent| {
            let per_second = percent / 100.0 * 1e9 / cost(Case::CacheMiss);
            format!("{percent} %: {per_second:.0}")
        })
        .collect();
    println!(
        "The budget a share of one core allows at the cache-miss cost: {}",
        budgets.join(", ")
    );
    ExitCode::SUCCESS
}
