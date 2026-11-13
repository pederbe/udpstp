#!/bin/sh
# Runs CI's checks against a clean checkout of a commit, so the result is
# what CI would see: nothing uncommitted, nothing left over from earlier
# builds but the cached target directory. Works on Linux and on Windows
# through Git Bash.
#
# Usage: scripts/check-commit.sh [REPO] [REV] [BASE]
#   REPO  the repository to check out from (default: the one this script is in)
#   REV   the commit to check (default: HEAD)
#   BASE  the commit REV replaces, such as the remote branch's tip. When
#         every file changed since BASE is documentation, only the
#         documentation checks run.
set -eu

repo=$(cd "${1:-$(dirname "$0")/..}" && pwd)
rev=${2:-HEAD}
base=${3:-}
platform=$(uname -s)
work=${UDPSTP_CHECK_DIR:-$HOME/.cache/udpstp-check-$platform}
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$HOME/.cache/udpstp-target-$platform}
export CARGO_TERM_COLOR=${CARGO_TERM_COLOR:-always}
# Checks never reuse incremental state or need full debug info, which
# would take gigabytes of disk.
export CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG=${CARGO_PROFILE_DEV_DEBUG:-line-tables-only}
if [ -f "$HOME/.cargo/env" ]; then
    . "$HOME/.cargo/env"
fi

commit=$(git -C "$repo" rev-parse "$rev")
if [ ! -d "$work/.git" ]; then
    git clone --quiet --no-checkout "$repo" "$work"
fi
cd "$work"
git fetch --quiet "$repo" "$commit"
git checkout --quiet --force "$commit"
git clean --quiet -fdx
echo "Checking $(git log -1 --format='%h %s') on $platform"

# True when every file changed since BASE is documentation that nothing but
# the link and glossary checks reads. Tests read docs/conformance/,
# docs/guides/windows.md, and `cargo xtask req` reads docs/conformance/,
# docs/rfc/ and docs/adr/. Packaging and installers read contrib/; checks and
# their fixtures remain full inputs even when named Markdown. Add any new reader
# here. Any other path, or a
# BASE this repository doesn't have, such as a new branch's zeros, runs
# every check. Renames list both paths.
docs_only() {
    [ -n "$base" ] || return 1
    changed=$(git -C "$repo" diff --no-renames --name-only "$base" "$commit" 2>/dev/null) || return 1
    [ -n "$changed" ] || return 1
    while IFS= read -r path; do
        case $path in
        src/* | crates/* | scripts/* | xtask/* | fuzz/* | tools/* | tests/* | fixtures/* | examples/* | contrib/* | .github/* | docs/adr/* | docs/conformance/* | docs/rfc/* | docs/guides/windows.md) return 1 ;;
        *.md | docs/* | website/*) ;;
        *) return 1 ;;
        esac
    done <<EOF
$changed
EOF
}
if docs_only; then
    rustup toolchain install
    cargo run --locked --package xtask -- links
    cargo run --locked --package xtask -- glossary
    echo "Documentation checks passed for $(git log -1 --format=%h) on $platform"
    exit 0
fi

rustup toolchain install
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
# Compile macOS-only code from Windows as an additional local check.
case $platform in
MINGW* | MSYS* | CYGWIN*)
    rustup target add aarch64-apple-darwin
    cargo clippy --locked --target aarch64-apple-darwin --workspace --all-targets -- -D warnings
    ;;
esac
cargo test --locked --workspace
cargo test --locked -p udpstp --features fault-injection --test faults
cargo test --locked -p udpstp --example setup_flood
cargo run --locked --package xtask -- req
if [ -n "$(git status --porcelain -- docs/conformance/summary.md docs/conformance/interop-masks.toml)" ]; then
    git diff -- docs/conformance/summary.md docs/conformance/interop-masks.toml
    echo "docs/conformance/summary.md or interop-masks.toml is stale: run cargo xtask req and commit them"
    exit 1
fi
cargo run --locked --package xtask -- links
cargo run --locked --package xtask -- glossary
# The install scripts, against a fake release served from
# 127.0.0.1: a second or so on Linux, a few on Windows.
case $platform in
MINGW* | MSYS* | CYGWIN*)
    powershell -NoProfile -ExecutionPolicy Bypass -File scripts/install-test.ps1
    ;;
*)
    sh scripts/install-test.sh
    ;;
esac
# The dependency audit (deny.toml), where cargo-deny is installed:
# once, on the Windows side, as it checks every platform's dependencies.
case $platform in
MINGW* | MSYS* | CYGWIN*)
    if command -v cargo-deny >/dev/null 2>&1; then
        cargo deny --locked check
    else
        echo "cargo-deny isn't installed, so the dependency audit is skipped (cargo install cargo-deny)"
    fi
    ;;
esac
# The crates as crates.io would get them, once, on the Linux side:
# rustdoc without warnings, and a dry-run publish, which packages and
# builds every published crate.
case $platform in
Linux)
    RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps
    # Verify the application's packaged source in a disposable build directory.
    rm -rf "$CARGO_TARGET_DIR/publish"
    CARGO_TARGET_DIR="$CARGO_TARGET_DIR/publish" cargo publish --locked -p udpstp --dry-run
    rm -rf "$CARGO_TARGET_DIR/publish"
    # The fuzz targets include application source modules and
    # run only with nightly Rust (scripts/fuzz.sh).
    cargo clippy --locked --manifest-path fuzz/Cargo.toml --all-targets -- -D warnings
    ;;
esac
# On Linux only: the pinned OB-UDPST build interop tests run against, kept
# outside the clean checkout so it is rebuilt only when it changes, and the
# network-namespace tier.
case $platform in
Linux)
    export UDPSTP_OBUDPST_DIR=${UDPSTP_OBUDPST_DIR:-$HOME/.cache/udpstp-obudpst}
    scripts/obudpst-build.sh --check "$UDPSTP_OBUDPST_DIR"
    # The network-namespace tier, with a loose capacity bound on a shared
    # machine; CI checks it strictly.
    scripts/netns-test.sh
    # The example systemd unit, where systemd is: verified, its
    # exposure checked, and run as a user service serving a Test.
    if command -v systemd-analyze >/dev/null 2>&1; then
        scripts/systemd-check.sh "$CARGO_TARGET_DIR/debug/udpstp"
    fi
    ;;
esac
echo "Checks passed for $(git log -1 --format=%h) on $platform"
