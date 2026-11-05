#!/usr/bin/env bash
# Builds the pinned OB-UDPST v9.1.0 that interop tests run against,
# on Linux or in WSL. Needs git, cmake, a C compiler and OpenSSL 3 headers
# (Debian/Ubuntu: `apt install git cmake build-essential libssl-dev`).
#
#   scripts/obudpst-build.sh [--check] [OUT_DIR]
#
# OUT_DIR defaults to $UDPSTP_OBUDPST_DIR, else target/obudpst. It gets the
# source at the pinned commit, and one build per variant, each with a
# `udpst` binary:
#
#   OUT_DIR/v9.1.0/auth/udpst           authentication, no header checksum
#   OUT_DIR/v9.1.0/auth-checksum/udpst  authentication, header checksum on
#
# The flags are fixed below, every option given explicitly so the build
# doesn't depend on OB-UDPST's defaults; the compiler, CMake and OpenSSL
# are the host's, so a build is reproducible from source and flags, not
# bit for bit. A variant is rebuilt when its flags or those tools change.
# docs/testing.md ("The pinned OB-UDPST build") says why each flag is set
# and lists the OB-UDPST bugs interop tests avoid.
#
# `--check` then runs Tests between the variants on loopback: each
# variant's Client against its own Server, Downstream and Upstream, and a
# checksum Client against the plain Server, all of which must complete
# without an invalid PDU on either side; one with the wrong key, which the
# Server must refuse; and one with no key against a Server without keys
# (Unauthenticated Mode).
set -euo pipefail

# On a host that runs benchmark measurements under measure-lock, build and
# check as a build does, so they never overlap a measurement.
if [ -z "${UDPSTP_MEASURE_LOCKED:-}" ] && command -v measure-lock >/dev/null 2>&1; then
    UDPSTP_MEASURE_LOCKED=1 exec measure-lock build -- "$0" "$@"
fi

REPOSITORY=https://github.com/BroadbandForum/obudpst.git
TAG=v9.1.0
COMMIT=ceb8917d6b8cf1089ff488e19e06c2b52cfbff85

COMMON_FLAGS=(
    -DCMAKE_BUILD_TYPE=Release
    # OB-UDPST's checksum() reads 16-bit header fields through an
    # unsigned int pointer; at -O3 GCC inlines it and loads fields such as
    # udpPayload before they are stored. -g only adds debug information.
    "-DCMAKE_C_FLAGS=-fno-strict-aliasing -g"
    -DDISABLE_INT_TIMER=OFF
    -DHAVE_SENDMMSG=ON
    -DHAVE_RECVMMSG=ON
    -DHAVE_GSO=ON
    -DRATE_LIMITING=OFF
    -DAUTH_IS_OPTIONAL=OFF
    -DSUPP_INVPDU_ALERT=OFF
    -DSUPP_INVPDU_WARN=OFF
)

check=false
if [ "${1:-}" = --check ]; then
    check=true
    shift
fi
root=$(cd "$(dirname "$0")/.." && pwd)
out=${1:-${UDPSTP_OBUDPST_DIR:-$root/target/obudpst}}
src=$out/$TAG/src

# The source, fetched by commit so a moved tag can't change it.
if ! git -C "$src" rev-parse --verify --quiet HEAD >/dev/null 2>&1; then
    rm -rf "$src"
    mkdir -p "$src"
    git -C "$src" init --quiet
    git -C "$src" fetch --quiet --depth 1 "$REPOSITORY" "$COMMIT"
    git -C "$src" -c advice.detachedHead=false checkout --quiet FETCH_HEAD
fi
actual=$(git -C "$src" rev-parse HEAD)
if [ "$actual" != "$COMMIT" ]; then
    echo "$src is at $actual, not the pinned $COMMIT; delete it to fetch it again" >&2
    exit 1
fi
if [ -n "$(git -C "$src" status --porcelain --ignored)" ]; then
    echo "$src has local changes; delete it to fetch it again" >&2
    exit 1
fi

tools="$COMMIT
$(${CC:-cc} --version | head -1)
$(cmake --version | head -1)
$(openssl version)"

build() {
    local variant=$1
    shift
    local dir=$out/$TAG/$variant
    local flags=("${COMMON_FLAGS[@]}" "$@")
    local stamp
    stamp="$(printf '%q ' "${flags[@]}")
$tools"
    if [ -x "$dir/udpst" ] && [ "$(cat "$dir/stamp" 2>/dev/null)" = "$stamp" ]; then
        echo "$dir/udpst is up to date"
        return
    fi
    rm -rf "$dir"
    mkdir -p "$dir"
    cmake -S "$src" -B "$dir" "${flags[@]}" >"$dir/cmake.log"
    cmake --build "$dir" --parallel >"$dir/build.log"
    # Without OpenSSL 3, OB-UDPST silently builds without authentication.
    if ! grep -q -- -DAUTH_KEY_ENABLE "$dir/compile_commands.json"; then
        echo "$variant was built without authentication: install OpenSSL 3 headers" >&2
        exit 1
    fi
    printf '%s\n' "$stamp" >"$dir/stamp"
    echo "Built $dir/udpst"
}

build auth -DADD_HEADER_CSUM=OFF
build auth-checksum -DADD_HEADER_CSUM=ON
echo "With: ${tools//$'\n'/, }"

$check || exit 0

# Ports for the checks, away from the Standard Port and other runs.
port=$((30000 + ($$ % 1000) * 20))
fail() {
    echo "check failed: $*" >&2
    exit 1
}

# One Test on loopback. Arguments: a name, the Server's and the Client's
# variants, the Server's key (none for no key), the Client's key, and the
# Client's direction (-d or -u). The logs are in $out/$TAG/check/<name>.*;
# returns the Client's exit status.
run_test() {
    local name=$1 server_variant=$2 client_variant=$3 server_key=$4 client_key=$5 direction=$6
    local log=$out/$TAG/check/$name
    mkdir -p "$out/$TAG/check"
    port=$((port + 1))
    local server_auth=() client_auth=()
    [ "$server_key" = none ] || server_auth=(-a "$server_key")
    [ "$client_key" = none ] || client_auth=(-a "$client_key")
    "$out/$TAG/$server_variant/udpst" -1 -v "${server_auth[@]}" -p "$port" 127.0.0.1 >"$log.server" 2>&1 &
    local server=$!
    local tries=0
    until grep -q "Awaiting setup requests" "$log.server" 2>/dev/null; do
        kill -0 "$server" 2>/dev/null || fail "$name: the Server exited: $(cat "$log.server")"
        tries=$((tries + 1))
        [ "$tries" -lt 50 ] || fail "$name: the Server didn't start"
        sleep 0.1
    done
    local status=0
    timeout 30 "$out/$TAG/$client_variant/udpst" "$direction" "${client_auth[@]}" -t 5 -s -p "$port" 127.0.0.1 \
        >"$log.client" 2>&1 || status=$?
    kill "$server" 2>/dev/null || true
    wait "$server" 2>/dev/null || true
    return "$status"
}

# A Test that must complete, with no invalid PDU on either side.
completes() {
    local name=$1
    run_test "$@" || fail "$name: the Test didn't complete: $(cat "$out/$TAG/check/$name.client")"
    if grep -q "Received invalid" "$out/$TAG/check/$name".*; then
        fail "$name: invalid PDUs: $(grep -h "Received invalid" "$out/$TAG/check/$name".* | head -3)"
    fi
    echo "$name: passed"
}

for variant in auth auth-checksum; do
    expected=Off
    [ "$variant" = auth-checksum ] && expected=On
    completes "$variant-downstream" "$variant" "$variant" key key -d
    grep -q "Checksum: $expected" "$out/$TAG/check/$variant-downstream.client" ||
        fail "$variant: the banner doesn't say Checksum: $expected"
    completes "$variant-upstream" "$variant" "$variant" key key -u
done
completes checksum-client-plain-server auth auth-checksum key key -d
completes unauthenticated auth auth none none -d
if run_test wrong-key auth auth key wrong -d; then
    fail "wrong-key: a Client with the wrong key completed a Test"
fi
grep -q "Authentication failure" "$out/$TAG/check/wrong-key.server" ||
    fail "wrong-key: the Server didn't refuse the key: $(cat "$out/$TAG/check/wrong-key.server")"
echo "wrong-key: refused"
