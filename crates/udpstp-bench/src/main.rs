#![forbid(unsafe_code)]
#![deny(clippy::exit)]
//! `udpstp-bench`: udpstp's benchmark Rounds. See
//! [`udpstp_bench::cli`].

use std::process::ExitCode;

use bench_rounds::interrupt;
use bench_rounds::round::{self, WallClock};
use udpstp_bench::cli::{self, Env};

#[cfg(feature = "hotpath-alloc")]
#[global_allocator]
static ALLOCATOR: hotpath::CountingAllocator = hotpath::CountingAllocator::new();

fn main() -> ExitCode {
    // First, before any thread: SIGINT, SIGTERM and SIGHUP (the console's
    // events on Windows) only raise the flag from here on.
    let flag = match interrupt::install() {
        Ok(flag) => flag,
        Err(e) => {
            round::alert(&format!(
                "udpstp-bench: the signal handlers couldn't be installed: {e}"
            ));
            return ExitCode::from(1);
        }
    };
    let clock = WallClock { flag: flag.clone() };
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(Into::into)
        .unwrap_or_default();
    let code = cli::main(
        std::env::args_os().collect(),
        &mut Env {
            flag,
            clock: &clock,
            out: &mut |line| round::say(line),
            err: &mut |line| round::alert(line),
            factory: None,
            native_windows: cfg!(windows),
            xdg_state_home: std::env::var("XDG_STATE_HOME").ok(),
            home,
            pid: std::process::id(),
            keys_in: None,
            round_suffix: None,
            ssh_key: None,
        },
    );
    ExitCode::from(code)
}
