//! A Setup Request flood generator, for testing one's own Server's
//! resilience to Control Phase floods.
//!
//! It sends Setup Requests to a Server at a bounded rate: forged
//! (unsigned or wrongly signed), or a key holder's authentic requests that
//! never activate. It is safe by default:
//!
//! - it sends only to the `--target` given, never a default;
//! - it refuses a non-private destination unless `--allow-public` is given,
//!   so it can't be pointed at a stranger;
//! - its rate is bounded (`--rate`, at most `MAX_RATE_PPS`).
//!
//! ```text
//! cargo run --release -p udpstp --example setup_flood -- \
//!     --target 127.0.0.1:24601 --kind forged-unsigned --rate 20000 --seconds 5
//!
//! cargo run --release -p udpstp --example setup_flood -- \
//!     --target 127.0.0.1:24601 --kind key-holder --key-hex <64 hex> --key-id 1 \
//!     --rate 20000 --seconds 5 --sources 2000
//! ```
//!
//! Only send this to a Server you run.

#[path = "flood_support/mod.rs"]
#[allow(dead_code)]
mod flood_common;

use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use flood_common::{
    FloodConfig, KeyMaterial, Kind, MAX_RATE_PPS, bind_sources, flood_until, is_private, parse_hex,
};

/// The longest a flood may run in one invocation.
const MAX_SECONDS: u64 = 600;

struct Options {
    target: SocketAddr,
    kind: Kind,
    rate: u64,
    seconds: u64,
    sources: usize,
    threads: usize,
    key: Option<KeyMaterial>,
    key_id: u8,
    allow_public: bool,
}

fn usage() -> String {
    "usage: setup_flood --target ADDR:PORT --kind forged-unsigned|forged-wrong-key|key-holder \
--rate PPS --seconds N [--sources N] [--threads N] [--key-hex HEX --key-id ID] [--allow-public]"
        .into()
}

fn parse() -> Result<Options, String> {
    let mut target: Option<SocketAddr> = None;
    let mut kind: Option<Kind> = None;
    let mut rate: Option<u64> = None;
    let mut seconds: Option<u64> = None;
    let mut sources = 1usize;
    let mut threads = 2usize;
    let mut key_hex: Option<String> = None;
    let mut key_id: Option<u8> = None;
    let mut allow_public = false;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--target" => {
                target = Some(value()?.parse().map_err(|e| format!("--target: {e}"))?);
            }
            "--kind" => kind = Some(Kind::parse(&value()?)?),
            "--rate" => rate = Some(value()?.parse().map_err(|e| format!("--rate: {e}"))?),
            "--seconds" => seconds = Some(value()?.parse().map_err(|e| format!("--seconds: {e}"))?),
            "--sources" => sources = value()?.parse().map_err(|e| format!("--sources: {e}"))?,
            "--threads" => threads = value()?.parse().map_err(|e| format!("--threads: {e}"))?,
            "--key-hex" => key_hex = Some(value()?),
            "--key-id" => key_id = Some(value()?.parse().map_err(|e| format!("--key-id: {e}"))?),
            "--allow-public" => allow_public = true,
            "-h" | "--help" => return Err(usage()),
            other => return Err(format!("unknown argument {other}\n{}", usage())),
        }
    }

    let target = target.ok_or("--target is required (no default, for safety)")?;
    let kind = kind.ok_or("--kind is required")?;
    let rate = rate.ok_or("--rate is required (the flood is rate-bounded)")?;
    let seconds = seconds.ok_or("--seconds is required")?;
    if rate == 0 || rate > MAX_RATE_PPS {
        return Err(format!("--rate must be 1..={MAX_RATE_PPS}"));
    }
    if seconds == 0 || seconds > MAX_SECONDS {
        return Err(format!("--seconds must be 1..={MAX_SECONDS}"));
    }
    let threads = threads.clamp(1, 64);
    let sources = sources.max(1);
    if !allow_public && !is_private(target.ip()) {
        return Err(format!(
            "{} is not a private/loopback address; refusing (pass --allow-public to override, \
only for a Server you run)",
            target.ip()
        ));
    }
    let (key, flood_key_id) = if kind.needs_key() {
        let hex = key_hex.ok_or("--kind key-holder needs --key-hex")?;
        let shared = parse_hex(&hex)?;
        let id = key_id.ok_or("--kind key-holder needs --key-id")?;
        (Some(KeyMaterial { shared, key_id: id }), id)
    } else {
        if key_hex.is_some() {
            return Err("--key-hex is only for --kind key-holder".into());
        }
        // For a forged flood --key-id names the keyId to stamp: name one the
        // Server holds so the request reaches the digest check (a KDF run and
        // an HMAC), else it is dropped cheaply at the keyId scan. Defaults 0.
        (None, key_id.unwrap_or(0))
    };

    Ok(Options {
        target,
        kind,
        rate,
        seconds,
        sources,
        threads,
        key,
        key_id: flood_key_id,
        allow_public,
    })
}

fn main() -> ExitCode {
    let options = match parse() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    if options.allow_public {
        eprintln!(
            "setup_flood: WARNING sending to a non-private destination {} because --allow-public was given",
            options.target.ip()
        );
    }

    // One source pool, split across threads. Each source is a distinct
    // loopback address (a distinct Server-side Source) where possible.
    let sources = match bind_sources(options.sources, options.target) {
        Ok(sources) => sources,
        Err(error) => {
            eprintln!("setup_flood: could not bind source sockets: {error}");
            return ExitCode::FAILURE;
        }
    };
    let bound = sources.len();

    let threads = options.threads.min(bound);
    let until = Instant::now() + Duration::from_secs(options.seconds);
    let stop = AtomicBool::new(false);
    eprintln!(
        "setup_flood: {} to {} at up to {} pps for {} s, {} threads, {} source socket(s)",
        options.kind.name(),
        options.target,
        options.rate,
        options.seconds,
        threads,
        bound
    );

    let start = Instant::now();
    // Each thread sends from its own chunk of the shared sockets, with a
    // share of the rate; no socket is cloned, so the flood needs one open
    // file per Source.
    let sent: u64 = std::thread::scope(|scope| {
        let handles: Vec<_> = sources
            .chunks(bound.div_ceil(threads))
            .enumerate()
            .map(|(t, chunk)| {
                let per_thread = options.rate / threads as u64
                    + u64::from((options.rate % threads as u64) > t as u64);
                let config = FloodConfig {
                    kind: options.kind,
                    key_id: options.key_id,
                    key: options.key.clone(),
                    rate_pps: per_thread,
                    mc_seed: (t as u32).wrapping_mul(0x9E37_79B9),
                };
                let stop = &stop;
                scope.spawn(move || flood_until(chunk, &config, until, stop))
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("a flood thread failed"))
            .sum()
    });
    let elapsed = start.elapsed().as_secs_f64();
    let pps = sent as f64 / elapsed;
    // 56-byte UDP payload + 28 bytes IPv4/UDP header on the wire.
    let mbps = pps * (56.0 + 28.0) * 8.0 / 1e6;
    eprintln!(
        "setup_flood: sent {sent} datagrams in {elapsed:.2} s = {pps:.0} pps (~{mbps:.1} Mbit/s IPv4)"
    );
    ExitCode::SUCCESS
}
