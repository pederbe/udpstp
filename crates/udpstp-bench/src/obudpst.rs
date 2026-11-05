//! OB-UDPST built on a host of a Round, as `scripts/bench/round.py`'s
//! `build_obudpst` builds it, for either Round (`--obudpst build`).
//!
//! The pinned OB-UDPST is built from source on the host itself, with the
//! host's own C library, which a build elsewhere may not match:
//! `scripts/obudpst-build.sh` fetches the source at its pinned commit and
//! builds it with its fixed flags (docs/testing.md, "The pinned OB-UDPST
//! build"). The tools come from the distribution where they are missing
//! (Debian or Ubuntu, `sudo` without a password), waiting up to 10 minutes
//! for dpkg's lock. The binary ends up as `udpst` in the host's work
//! directory, where a given one would be copied.
//!
//! It runs on the host through [`Shell`], and says what failed as the
//! Python's refusals say it: a failure is the host's error, which stops the
//! Round before its Round Runs, as the Python's `Refused` stops it; an
//! interrupt stays one.

use std::path::{Path, PathBuf};

use bench_rounds::host::{HostError, Limit, RUN_LIMIT, RemotePath, Shell, Stop};
use bench_rounds::py;

/// The build script's name in the host's work directory.
pub const SCRIPT_NAME: &str = "obudpst-build.sh";

/// The binary's name in the host's work directory, as a Round Run's
/// command runs it (`./udpst`).
pub const BINARY: &str = "udpst";

/// Whether the host has the build tools, else their installation.
pub const TOOLS_SCRIPT: &str = concat!(
    "command -v cmake >/dev/null && command -v cc >/dev/null && command -v git >/dev/null",
    " && [ -e /usr/include/openssl/hmac.h ]",
    " || (sudo -n env DEBIAN_FRONTEND=noninteractive apt-get -qq -o DPkg::Lock::Timeout=600",
    " update && sudo -n env DEBIAN_FRONTEND=noninteractive apt-get -qq -o",
    " DPkg::Lock::Timeout=600 install -y cmake build-essential libssl-dev git)",
);

/// How long the tools' installation may take: dpkg's lock, then apt.
pub const TOOLS_LIMIT: Limit = Limit::secs(1500);

/// The build, its output in a log, then the binary where a Round Run's
/// command finds it.
pub const BUILD_SCRIPT: &str = "./obudpst-build.sh ./obudpst > obudpst-build.log 2>&1 \
     && cp obudpst/v9.1.0/auth/udpst ./udpst";

/// How long the build may take.
pub const BUILD_LIMIT: Limit = Limit::secs(900);

/// The end of the build's log, for a build that failed.
pub const LOG_SCRIPT: &str = "tail -20 obudpst-build.log";

/// How much of the tools' installation's error output a refusal quotes, in
/// characters.
const QUOTED: usize = 300;

/// The build script in the checkout this tool was built from, as the
/// Python finds it beside itself.
pub fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/obudpst-build.sh")
}

/// Builds the pinned OB-UDPST on `host`, from the build script at `script`
/// ([`script`], or a test's): the script copied in, the tools installed
/// where missing, the build run and its binary put where a Round Run's
/// command finds it.
///
/// Where the tools can't be installed or the build fails, the host's error
/// says so in the Python's words, with the end of what the installation
/// said or of the build's log.
pub fn build(host: &dyn Shell, script: &Path) -> Result<(), Stop> {
    let name = RemotePath::new(SCRIPT_NAME).expect("a plain name");
    host.put(script, &name, 0o755)?;
    let tools = host.run(TOOLS_SCRIPT, TOOLS_LIMIT)?;
    if !tools.success() {
        return Err(HostError::new(format!(
            "{}: OB-UDPST's build tools are missing and couldn't be installed: {}",
            host.role(),
            last_chars(py::strip(&tools.stderr), QUOTED)
        ))
        .into());
    }
    let built = host.run(BUILD_SCRIPT, BUILD_LIMIT)?;
    if !built.success() {
        let log = host.run(LOG_SCRIPT, RUN_LIMIT)?.stdout;
        return Err(
            HostError::new(format!("{}: OB-UDPST didn't build: {log}", host.role())).into(),
        );
    }
    Ok(())
}

/// `text[-n:]`: its last `n` characters.
fn last_chars(text: &str, n: usize) -> &str {
    let skip = text.chars().count().saturating_sub(n);
    text.char_indices()
        .nth(skip)
        .map_or("", |(at, _)| &text[at..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_quotes_the_last_characters() {
        assert_eq!(last_chars("abc", 300), "abc");
        assert_eq!(last_chars("", 3), "");
        assert_eq!(last_chars("abcdef", 3), "def");
        assert_eq!(last_chars("éé日😀", 2), "日😀");
    }

    #[test]
    fn the_script_is_the_checkouts() {
        let text = std::fs::read_to_string(script()).unwrap();
        assert!(text.contains("TAG=v9.1.0"), "the pinned version");
        // The binary the build copies is the one the script builds.
        assert!(text.contains("OUT_DIR/v9.1.0/auth/udpst"));
        assert!(BUILD_SCRIPT.contains("obudpst/v9.1.0/auth/udpst"));
    }
}
