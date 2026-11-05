#![forbid(unsafe_code)]
//! Project automation (`cargo xtask <command>`).

mod capture;
mod glossary;
mod kat;
mod links;
mod records;
mod release_smoke;
mod req;
mod site;

#[cfg(test)]
mod check_selection_tests;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "usage: cargo xtask <command>

commands:
  capture
         capture OB-UDPST's Control PDUs into known-answer vectors (Linux;
         needs scripts/obudpst-build.sh first)
  kat    regenerate known-answer vectors from independent implementations
         (needs `openssl` 3.x on PATH)
  glossary
         check the docs, user-visible strings, --help and requirement notes
         for terms GLOSSARY.md avoids or writes in another case
  links  check that relative links in the Markdown files resolve
  req    check requirement traceability and the conformance records, print
         coverage per section and each record's proving tests, and update
         docs/conformance/summary.md and docs/conformance/interop-masks.toml
  req hash
         print the text hash of a requirement read from stdin
  release-smoke BIN VERSION DIRECTORY [RUNNER]
         check an installed release with authenticated loopback Tests
  site <prepare|finish|check|contrast>
         build the udpstp.com website's pages and check them (run by
         scripts/build-site.sh, which also runs mdBook and mermaid-cli)";

fn main() -> ExitCode {
    let command = std::env::args().nth(1);
    let result = match command.as_deref() {
        Some("capture") => capture::run(),
        Some("glossary") => glossary::run(),
        Some("kat") => kat::run(),
        Some("links") => links::run(),
        Some("req") => match std::env::args().nth(2).as_deref() {
            None => req::run(),
            Some("hash") => req::print_hash(),
            Some(other) => Err(format!("unknown req subcommand: {other}\n\n{USAGE}")),
        },
        Some("site") => site::run(&std::env::args().skip(2).collect::<Vec<_>>()),
        Some("release-smoke") => release_smoke::run(&std::env::args().skip(2).collect::<Vec<_>>()),
        None | Some("help") => {
            println!("{USAGE}");
            Ok(())
        }
        Some(other) => Err(format!("unknown xtask command: {other}\n\n{USAGE}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

/// The workspace root (the parent of this crate).
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives in the workspace root")
        .to_path_buf()
}
