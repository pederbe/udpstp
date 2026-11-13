#!/usr/bin/env bash
# Runs the network-namespace tier (tests/netns.rs and
# the OB-UDPST interop tests in interop.rs) on
# Linux, without root: cargo runs the test binary inside a new user,
# network, mount and PID namespace, where the tests build their topology
# with `ip netns`, veth pairs and qdiscs. Everything they create, processes
# included, ends with that namespace.
#
#   scripts/netns-test.sh [--strict] [CARGO TEST ARGS...]
#
# A failed test is run once more (docs/testing.md, "Flaky tests").
#
# --strict checks capacity to the tier's 5% (CI, or a quiet host);
# otherwise the bound is loose, for the pre-push hook on shared machines.
# The tests fail rather than skip if they can't run in the namespace.
#
# Needs iproute2 (`ip`, `tc`), tcpdump, nft (fetched without root on
# Debian and Ubuntu when missing) and unprivileged user namespaces. Ubuntu
# 24.04 restricts those for unconfined programs; CI lifts that with
# `sysctl kernel.apparmor_restrict_unprivileged_userns=0`.
set -euo pipefail

if [ "${1:-}" = --strict ]; then
    export UDPSTP_NETNS_STRICT=1
    shift
fi
root=$(cd "$(dirname "$0")/.." && pwd)
if ! unshare --user --map-root-user --net true 2>/dev/null; then
    echo "netns-test: unprivileged user namespaces are unavailable here" >&2
    exit 1
fi
export UDPSTP_NETNS_REQUIRED=1
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER="$root/scripts/netns-runner.sh"
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUNNER="$root/scripts/netns-runner.sh"
# The NAT tests need nft. Where it isn't installed, fetch it without root
# from the distribution's packages (Debian and Ubuntu), into a cache.
if [ -z "${UDPSTP_NFT:-}" ]; then
    if command -v nft >/dev/null 2>&1; then
        UDPSTP_NFT=$(command -v nft)
    else
        cache=${UDPSTP_NFT_DIR:-$HOME/.cache/udpstp-nft}
        if [ ! -x "$cache/nft" ]; then
            mkdir -p "$cache/debs"
            (cd "$cache/debs" && apt-get download -q nftables libnftables1 libnftnl11 libjansson4 >/dev/null)
            for deb in "$cache"/debs/*.deb; do dpkg -x "$deb" "$cache/root"; done
            libs=$(dirname "$(find "$cache/root" -name 'libnftables.so.1' | head -1)")
            printf '#!/bin/sh\nLD_LIBRARY_PATH=%s exec %s "$@"\n' "$libs" "$cache/root/usr/sbin/nft" >"$cache/nft"
            chmod +x "$cache/nft"
        fi
        UDPSTP_NFT=$cache/nft
    fi
fi
export UDPSTP_NFT
# The interop tests need the pinned OB-UDPST build.
export UDPSTP_OBUDPST_DIR=${UDPSTP_OBUDPST_DIR:-$root/target/obudpst}
"$root/scripts/obudpst-build.sh" "$UDPSTP_OBUDPST_DIR" >/dev/null
# A test that fails is run once more: these tiers share the host
# with everything else, and a flaky test shouldn't stop a push. One that
# fails again fails the run. docs/testing.md ("Flaky tests") says when a
# test that needed its retry is quarantined.
log=$(mktemp)
trap 'rm -f "$log"' EXIT
if cargo test --locked -p udpstp --test netns --test interop "$@" 2>&1 | tee "$log"; then
    exit 0
fi
# The failed tests' names: the indented list after the last "failures:".
failed=$(awk '/^failures:$/ { names = ""; listing = 1; next }
    listing && /^    [A-Za-z0-9_:]+$/ { names = names " " $1; next }
    listing && NF { listing = 0 }
    END { print names }' "$log")
if [ -z "$failed" ]; then
    echo "netns-test: the run failed without a list of failed tests, so nothing is retried" >&2
    exit 1
fi
echo "netns-test: retrying once:$failed" >&2
# The test binaries' own arguments follow the first "--", if one was given.
separator=--
for arg in "$@"; do
    if [ "$arg" = -- ]; then
        separator=
    fi
done
# shellcheck disable=SC2086 # one argument per test name; no separator twice
cargo test --locked -p udpstp --test netns --test interop "$@" $separator --exact $failed
echo "netns-test: FLAKY, passed on the retry:$failed (see docs/testing.md, \"Flaky tests\")" >&2
