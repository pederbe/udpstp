#!/bin/sh
# Runs every fuzz target (fuzz/) for a while, on Linux with
# nightly Rust and cargo-fuzz (cargo install cargo-fuzz). Each target
# starts from its seed corpus, fuzz/seeds/<target> if it has one, and keeps
# what it finds in fuzz/corpus/<target> (not committed); a crash is saved
# under fuzz/artifacts/<target> and fails the run.
#
# Usage: scripts/fuzz.sh [SECONDS] [TARGET...]
#   SECONDS  how long each target runs (default 60)
#   TARGET   the targets to run (default: all of them)
set -eu

cd "$(dirname "$0")/.."
seconds=${1:-60}
[ $# -gt 0 ] && shift
toolchain=${UDPSTP_FUZZ_TOOLCHAIN:-nightly}
targets=${*:-$(cargo "+$toolchain" fuzz list --fuzz-dir fuzz)}
for target in $targets; do
    mkdir -p "fuzz/corpus/$target"
    echo "Fuzzing $target for ${seconds}s"
    seeds=
    [ -d "fuzz/seeds/$target" ] && seeds="fuzz/seeds/$target"
    # shellcheck disable=SC2086 # without seeds, no argument
    cargo "+$toolchain" fuzz run --fuzz-dir fuzz "$target" \
        "fuzz/corpus/$target" $seeds \
        -- -max_total_time="$seconds" -print_final_stats=1
done
