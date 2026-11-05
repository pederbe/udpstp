#!/usr/bin/env bash
# The simulator against a real impaired network: runs
# crates/udpstp/tests/crosscheck.rs in a rootless network namespace, as
# scripts/netns-test.sh does, but alone, once (no retry: a disagreement
# must show), and in release. Its tolerances hold on a quiet host; on a
# shared one, run it under the host's measurement lock.
#
#   scripts/crosscheck.sh [--out FILE] [--repeats N] [--only CASE]
#
# Linux only; needs iproute2 (ip, tc) and unprivileged user namespaces.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
export UDPSTP_CROSSCHECK=1
while [ $# -gt 0 ]; do
    case "$1" in
        --out) export UDPSTP_CROSSCHECK_OUT=$2; shift 2 ;;
        --repeats) export UDPSTP_CROSSCHECK_REPEATS=$2; shift 2 ;;
        --only) export UDPSTP_CROSSCHECK_ONLY=$2; shift 2 ;;
        *) echo "crosscheck: unknown argument $1" >&2; exit 2 ;;
    esac
done
if ! unshare --user --map-root-user --net true 2>/dev/null; then
    echo "crosscheck: unprivileged user namespaces are unavailable here" >&2
    exit 1
fi
export UDPSTP_NETNS_REQUIRED=1
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER="$root/scripts/netns-runner.sh"
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUNNER="$root/scripts/netns-runner.sh"
cd "$root"
exec cargo test --release --locked -p udpstp --test crosscheck -- --nocapture
