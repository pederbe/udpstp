//! What the Key Table's keyId scan costs against its size.
//!
//! `KeyTable::accepting` scans the rows for a keyId (`crates/udpstp-core/
//! src/keys.rs`), and it runs before the global budget for unverified Setup
//! Requests: until 's keyId index, a forged flood naming an absent
//! keyId paid a scan of the whole table per datagram, whatever the budget.
//! The Key Table's size is bounded only by the 1 MiB key file, so this
//! times a request whose keyId no row holds (dropped straight after the
//! scan, `UnknownKeyId`), the worst case for a scan of the whole table,
//! against a rising row count, through the public `Server` so no private
//! API is exercised.
//!
//! ```text
//! cargo run --release -p udpstp --example key_scan_cost -- [--runs N] [--max-rows N]
//! ```
//!
//! The per-row cost is the slope: (cost at N rows − cost at 1 row) ÷ (N − 1).

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
    DatagramMeta, Endpoint, Event, IpVersion, KeyEntry, KeyTable, Lifetime, MonoTime, Now, Output,
    Server, ServerConfig, SilentRejectionReason, SocketId, WallTime,
};
use crate::wire::setup::{SETUP_PDU_LEN, cmd_request, modifier};
use crate::wire::{AUTH_DIGEST_LEN, PROTOCOL_VERSION, SetupPdu, auth_mode};

const NOW_UNIX: u32 = 1_790_000_000;
const NOW: Now = Now {
    mono: MonoTime::from_duration(Duration::from_secs(1)),
    wall: WallTime::from_unix(Duration::from_secs(NOW_UNIX as u64)),
};

/// Every request arrives on the Standard Port from the same address.
const META: DatagramMeta = DatagramMeta {
    socket: SocketId::PRIMARY,
    source: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 1), 40_000)),
    local: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(198, 51, 100, 1), 24_601)),
    ip_version: IpVersion::V4,
    tos: None,
    actual_len: SETUP_PDU_LEN,
};

/// A keyId no row holds, the worst case for a scan of the whole table; the
/// request is then dropped as `UnknownKeyId`. The rows use keyIds 0..=254,
/// so 255 is absent.
const UNKNOWN_KEY_ID: u8 = 255;

/// A rough lower bound on a TOML Key Table row's size in bytes: the
/// `[[key]]` header, an `admin_key_name`, a `key_id`, a 32-byte `key_hex`
/// and a short accept lifetime. A 1 MiB file (the cap `docs/security.md`
/// sets) holds about 1_048_576 / this many rows.
const MIN_TOML_ROW_BYTES: usize = 140;

/// Builds a Key Table of `rows` rows with keyIds cycling 0..=254, each with
/// a disjoint one-second lifetime so rows sharing a keyId are accepted (ADR
/// 0010). The lifetimes don't matter to the scan: an absent keyId matches
/// no row.
fn table(rows: usize) -> KeyTable {
    let mut keys = KeyTable::default();
    for i in 0..rows {
        let t = 1_000_000_000 + i as u64; // disjoint per row, so per keyId too
        let lifetime = Lifetime::new(Some(t), Some(t));
        let row = KeyEntry::new(
            &format!("row-{i}"),
            (i % 255) as u8,
            vec![0x5C; 32],
            lifetime,
            lifetime,
        );
        keys.insert(row).expect("disjoint lifetimes per keyId");
    }
    keys
}

fn request() -> [u8; SETUP_PDU_LEN] {
    SetupPdu {
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
        auth_digest: [0xA5; AUTH_DIGEST_LEN],
        key_id: UNKNOWN_KEY_ID,
        checksum: 0,
    }
    .encode()
}

/// Sends `requests` unknown-keyId Setup Requests and returns the time taken.
/// Panics unless the Server dropped every one as `UnknownKeyId` (so the scan
/// is what was measured, not some other path).
fn run(server: &mut Server, request: &[u8], requests: u32) -> Duration {
    let (mut unknown, mut other) = (0u32, 0u32);
    let start = Instant::now();
    for _ in 0..requests {
        server.handle_datagram(NOW, META, black_box(request));
        while let Some(output) = server.poll_output() {
            match output {
                Output::Event(Event::SetupRequestSilentlyRejected {
                    reason: SilentRejectionReason::UnknownKeyId,
                    ..
                }) => unknown += 1,
                _ => other += 1,
            }
        }
    }
    let elapsed = start.elapsed();
    assert_eq!(
        (unknown, other),
        (requests, 0),
        "every request must be dropped as UnknownKeyId"
    );
    elapsed
}

/// Requests per timed run: fewer as the scan grows, so each run stays short
/// but the total scanned rows stay high enough to time steadily.
fn requests_for(rows: usize) -> u32 {
    (20_000_000 / rows.max(1)).clamp(2_000, 500_000) as u32
}

struct Options {
    runs: usize,
    max_rows: usize,
}

fn options() -> Result<Options, String> {
    let mut options = Options {
        runs: 7,
        max_rows: 8_192,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |name: &str| {
            args.next()
                .ok_or_else(|| format!("{name} needs a value"))
                .and_then(|v| v.parse::<usize>().map_err(|e| format!("{name} {v}: {e}")))
        };
        match arg.as_str() {
            "--runs" => options.runs = value("--runs")?.max(1),
            "--max-rows" => options.max_rows = value("--max-rows")?.max(1),
            "-h" | "--help" => return Err("usage: key_scan_cost [--runs N] [--max-rows N]".into()),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(options)
}

fn median(runs: &mut [f64]) -> f64 {
    runs.sort_by(f64::total_cmp);
    runs[runs.len() / 2]
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

    let mut sizes = vec![1usize, 8, 64, 255, 1_024, 4_096];
    sizes.retain(|&n| n <= options.max_rows);
    if *sizes.last().unwrap_or(&0) != options.max_rows {
        sizes.push(options.max_rows);
    }
    let request = request();

    println!("The Key Table keyId scan against its size");
    println!(
        "{} {}, median of {} runs; an absent keyId, the worst case for a scan of the whole table",
        std::env::consts::OS,
        std::env::consts::ARCH,
        options.runs
    );
    println!(
        "A 1 MiB key file holds about {} rows (>= {MIN_TOML_ROW_BYTES} bytes a TOML row).",
        1_048_576 / MIN_TOML_ROW_BYTES
    );
    println!();
    println!(
        "{:>8}  {:>12}  {:>12}  {:>12}",
        "rows", "ns/request", "ns/row", "requests/run"
    );

    let mut base = f64::NAN;
    for &rows in &sizes {
        let mut server = Server::new(ServerConfig {
            keys: table(rows),
            // High, so the budget never drops a request; an unknown keyId is
            // dropped before the budget anyway.
            unverified_setups_per_second: u32::MAX,
            ..ServerConfig::default()
        });
        let requests = requests_for(rows);
        // Warm-up.
        run(&mut server, &request, requests / 10 + 1);
        let mut times: Vec<f64> = (0..options.runs)
            .map(|_| run(&mut server, &request, requests).as_nanos() as f64 / f64::from(requests))
            .collect();
        let ns = median(&mut times);
        if rows == 1 {
            base = ns;
        }
        let per_row = if rows > 1 && base.is_finite() {
            (ns - base) / (rows as f64 - 1.0)
        } else {
            f64::NAN
        };
        println!("{rows:>8}  {ns:>12.1}  {per_row:>12.3}  {requests:>12}");
    }
    ExitCode::SUCCESS
}
