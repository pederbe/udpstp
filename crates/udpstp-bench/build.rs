//! Stamp the code that built the benchmark tool, independent of its runtime cwd.
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let root =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo's manifest directory"))
            .join("../..");
    let git = |args: &[&str]| {
        Command::new("git")
            .current_dir(&root)
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_owned())
    };
    if let Some(path) = git(&["rev-parse", "--git-path", "HEAD"]) {
        println!("cargo:rerun-if-changed={}", root.join(path).display());
    }
    if let Some(reference) = git(&["symbolic-ref", "-q", "HEAD"])
        && let Some(path) = git(&["rev-parse", "--git-path", &reference])
    {
        println!("cargo:rerun-if-changed={}", root.join(path).display());
    }
    for path in [
        "Cargo.lock",
        "Cargo.toml",
        "crates/bench-rounds",
        "crates/udpstp-bench",
    ] {
        println!("cargo:rerun-if-changed={}", root.join(path).display());
    }
    let mut commit = git(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into());
    if git(&[
        "status",
        "--porcelain",
        "--untracked-files=normal",
        "--",
        "crates/bench-rounds",
        "crates/udpstp-bench",
        "Cargo.toml",
        "Cargo.lock",
    ])
    .is_some_and(|s| !s.is_empty())
    {
        commit.push_str(" (the benchmark tooling changed since)");
    }
    println!("cargo:rustc-env=UDPSTP_BENCH_COMMIT={commit}");
}
