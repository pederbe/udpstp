#!/usr/bin/env bash
# The Server-scale measurement of the Data Phase Engine: many Test
# Connections at once against one Server confined to a few CPUs, with the
# Server's CPU, its engine threads' CPU and wake-ups, the Skipped and
# Withheld Datagrams, loss, RcvbufErrors and the pacing of its Bursts.
#
#   scripts/server-scale.sh netns [--runs N] [--test-interval S] [--out DIR] CASE...
#   scripts/server-scale.sh serve DIR [SERVER OPTIONS...]     (real hosts: on the Server)
#   scripts/server-scale.sh clients DIR SERVER[,SERVER...] CASE [FIRST [COUNT]]
#                                                              (real hosts: on a Client host)
#   scripts/server-scale.sh report DIR
#
# CASE is NAME=DIRECTION:CONNECTIONS:ROW, e.g. w3=downstream:200:50: that
# many Test Connections, each its own Client process and its own Fixed-Rate
# Test at Sending Rate Table ROW (rows up to 1000 are that many Mbps; 1001
# and 1002 are 1100 and 1200 Mbps, in packets that fit the default Server
# MTU). `clients` runs Clients FIRST to FIRST+COUNT-1 of them (all by
# default), so that several Client hosts can share a case
# (udpstp-bench round does, for 's round). Given several SERVERs,
# Client i tests with the ((i-1) mod n)-th of the n, so that the Clients
# spread over several Server instances (OB-UDPST's, one per core, as its
# README recommends).
#
# `netns` runs everything on this Linux host without root: the Server in
# one network namespace and the Clients in another, joined by a veth pair,
# inside a user namespace (as scripts/netns-test.sh does). The cases run
# interleaved, run after run. Each run is DIR/NAME/runN; `report` is run at
# the end. Where measure-lock is installed, the whole campaign runs under it.
# Without root there is no RPS, so a datagram is received on the sending
# side's CPUs: Downstream, the Server's CPUs also carry the Clients'
# receive work; Upstream, the Clients' CPUs carry the Server's.
#
# On real hosts, with a run directory OUT/NAME/runN: on the
# Server host, `serve OUT/NAME/runN` starts the Server with the same
# settings and samples it until it exits (stop it with Ctrl-C once the
# Clients are done); on a Client host, `clients OUT/NAME/runN SERVER CASE`
# runs the case's Clients. Copy the Client host's clients/ and meta.json
# into the Server's run directory, and run `report OUT`.
# KEY_FILE must then name a key file with an Operator Key, mode 600, the
# same on both hosts. Pacing is captured only in `netns`.
#
# Environment:
#   BENCH          prebuilt udpstp-bench [uploaded scripts/udpstp-bench, or target/release/udpstp-bench]
#   UDPSTP         the udpstp binary [target/release/udpstp]
#   SERVER_CPUS    the Server's CPUs (taskset) [2,4,6,8: 4 physical cores]
#   CLIENT_CPUS    the Clients' CPUs [10-15]
#   COLLECTOR_CPU  the sampler's and tcpdump's CPU [1]
#   KEY_FILE       the key file; `netns` writes one with an Operator Key
#   STAGGER        seconds between Client starts [0.005]
#   CLIENT_OPTIONS more options for each Client, such as "--auth-mode 1"
#                  against OB-UDPST's Server, which doesn't run mode 2
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
here=$root/scripts
BENCH=${BENCH:-$here/udpstp-bench}
if [ ! -x "$BENCH" ] && [ "$BENCH" = "$here/udpstp-bench" ]; then BENCH="$here/../target/release/udpstp-bench"; fi
UDPSTP=${UDPSTP:-$root/target/release/udpstp}
SERVER_CPUS=${SERVER_CPUS:-2,4,6,8}
CLIENT_CPUS=${CLIENT_CPUS:-10-15}
COLLECTOR_CPU=${COLLECTOR_CPU:-1}
STAGGER=${STAGGER:-0.005}
SERVER_ADDRESS=10.77.0.1
CLIENT_ADDRESS=10.77.0.2
PORTS_FIRST=50000
PORTS_LAST=50511

die() {
    echo "server-scale: $*" >&2
    exit 1
}

# The Server's settings: enough room for hundreds of Test Connections from
# one address. tests_per_source departs from RFC 9946 §4.2 (one Test per
# Source), which is why it is set only here.
write_config() {
    local file=$1 key_file=$2
    cat >"$file" <<EOF
key_file = "$key_file"
max_hops = 64
tests_per_source = 256
source_setup_burst = 6144
max_test_connections = 256
max_unactivated_test_connections = 256
ephemeral_ports = "$PORTS_FIRST-$PORTS_LAST"
EOF
}

write_key_file() {
    local file=$1
    printf '[[key]]\nadmin_key_name = "scale"\nkey_id = 7\nkey_hex = "%s"\noperator = true\n' \
        "$(od -An -tx1 -N32 /dev/urandom | tr -d ' \n')" >"$file"
    chmod 600 "$file"
}

parse_case() {
    local spec=$1
    case_name=${spec%%=*}
    local rest=${spec#*=}
    IFS=: read -r case_direction case_connections case_row <<<"$rest"
    [[ $case_name != "$spec" && $case_direction =~ ^(downstream|upstream)$ &&
        $case_connections =~ ^[0-9]+$ && $case_row =~ ^[0-9]+$ ]] ||
        die "a CASE is NAME=DIRECTION:CONNECTIONS:ROW, not $spec"
    # Rows above 1002 would be refitted to the Server MTU, in packets other
    # implementations don't send.
    ((case_row >= 1 && case_row <= 1002)) ||
        die "rows 1 to 1002 only (1 to 1000 Mbps, then 1100 and 1200), not $case_row"
    # The row's IP-layer rate and packet size: 1250 bytes up to 1000 Mbps,
    # then the Jumbo rows' 1375 and 1500.
    if ((case_row <= 1000)); then
        case_mbps=$case_row
        case_packet=1250
    else
        case_mbps=$((1100 + 100 * (case_row - 1001)))
        case_packet=$((1375 + 125 * (case_row - 1001)))
    fi
}

write_meta() {
    local dir=$1 interval=$2
    cat >"$dir/meta.json" <<EOF
{"case": "$case_name", "direction": "$case_direction", "connections": $case_connections,
 "row": $case_row, "mbps": $case_mbps, "packet_bytes": $case_packet, "test_interval": $interval,
 "server_cpus": "$SERVER_CPUS", "client_cpus": "$CLIENT_CPUS"}
EOF
}

# Starts Clients FIRST to LAST of the case, one Test Connection each, and
# waits for them. SERVERS is one Server or several, comma-separated: Client
# i tests with the ((i-1) mod n)-th. PREFIX runs each (a namespace and
# taskset).
run_clients() {
    local dir=$1 servers=$2 interval=$3 key_file=$4 first=$5 last=$6
    shift 6
    mkdir -p "$dir/clients"
    local i list extra
    IFS=, read -r -a list <<<"$servers"
    read -r -a extra <<<"${CLIENT_OPTIONS:-}"
    for ((i = first; i <= last; i++)); do
        "$@" "$UDPSTP" client --key-file "$key_file" --direction "$case_direction" \
            --fixed-rate "$case_row" --test-interval "$interval" --max-hops 64 \
            --format json ${extra[@]+"${extra[@]}"} "${list[$(((i - 1) % ${#list[@]}))]}" \
            >"$dir/clients/$i.json" 2>"$dir/clients/$i.err" &
        sleep "$STAGGER"
    done
    wait
}

netns_campaign() {
    local runs=3 interval=20 out=
    while (($#)); do
        case $1 in
        --runs) runs=$2; shift 2 ;;
        --test-interval) interval=$2; shift 2 ;;
        --out) out=$2; shift 2 ;;
        *) break ;;
        esac
    done
    (($#)) || die "no CASE given"
    for spec in "$@"; do parse_case "$spec"; done
    [ -x "$UDPSTP" ] || die "no udpstp binary at $UDPSTP (cargo build --release, or set UDPSTP)"
    out=${out:-$root/target/server-scale/$(date +%Y%m%d-%H%M%S)}
    mkdir -p "$out"
    out=$(cd "$out" && pwd)
    if [ -z "${SERVER_SCALE_INSIDE:-}" ]; then
        unshare --user --map-root-user --net true 2>/dev/null ||
            die "unprivileged user namespaces are unavailable here"
        local wrap=()
        if [ -z "${UDPSTP_MEASURE_LOCKED:-}" ] && command -v measure-lock >/dev/null 2>&1; then
            wrap=(measure-lock run --reason "udpstp server-scale" --max $((runs * $# * (interval + 30) + 300)) --)
        fi
        UDPSTP_MEASURE_LOCKED=1 SERVER_SCALE_INSIDE=1 exec "${wrap[@]}" \
            unshare --user --map-root-user --net --mount --fork --kill-child \
            "$0" netns --runs "$runs" --test-interval "$interval" --out "$out" "$@"
    fi
    # Inside the namespaces: /run for `ip netns`, two namespaces, one veth.
    mount -t tmpfs none /run
    ip netns add srv
    ip netns add cli
    ip link add vs netns srv type veth peer vc netns cli
    ip -n srv addr add "$SERVER_ADDRESS/24" dev vs
    ip -n cli addr add "$CLIENT_ADDRESS/24" dev vc
    for ns in srv cli; do ip -n $ns link set lo up; done
    ip -n srv link set vs up
    ip -n cli link set vc up
    local key_file=${KEY_FILE:-$out/keys.toml}
    [ -n "${KEY_FILE:-}" ] || write_key_file "$key_file"
    write_config "$out/server.toml" "$key_file"
    {
        echo "commit $(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)"
        echo "binary $UDPSTP"
        echo "kernel $(uname -r)"
        echo "cpus server $SERVER_CPUS clients $CLIENT_CPUS collector $COLLECTOR_CPU"
        echo "veth $(ip netns exec srv ethtool -k vs 2>/dev/null | grep -E '^(tx-udp-segmentation|generic-receive-offload):' | tr '\n' ' ')"
        echo "rmem_max $(cat /proc/sys/net/core/rmem_max) wmem_max $(cat /proc/sys/net/core/wmem_max)"
        echo "runs $runs test_interval $interval cases $*"
    } >"$out/environment.txt"
    # A process that lives in the Clients' namespace, whose UDP counters
    # the sampler reads.
    ip netns exec cli sleep infinity &
    local holder=$!
    local run spec
    for ((run = 1; run <= runs; run++)); do
        for spec in "$@"; do
            parse_case "$spec"
            netns_run "$out/$case_name/run$run" "$interval" "$key_file" "$holder"
            echo "server-scale: $case_name run $run done" >&2
        done
    done
    kill "$holder" 2>/dev/null || true
    report "$out" | tee "$out/report.md"
}

netns_run() {
    local dir=$1 interval=$2 key_file=$3 holder=$4
    rm -rf "$dir"
    mkdir -p "$dir"
    write_meta "$dir" "$interval"
    ip netns exec srv taskset -c "$SERVER_CPUS" "$UDPSTP" server --config "$dir/../../server.toml" \
        --listen "$SERVER_ADDRESS" --log-format json >"$dir/server.log" 2>&1 &
    local server=$!
    local tries=0
    until grep -q '"Listening"' "$dir/server.log" 2>/dev/null; do
        ((++tries < 50)) || die "the Server didn't start: $(cat "$dir/server.log")"
        sleep 0.1
    done
    taskset -c "$COLLECTOR_CPU" "$BENCH" sample "$dir/samples.jsonl" "$server" "$holder" &
    local sampler=$!
    run_clients "$dir" "$SERVER_ADDRESS" "$interval" "$key_file" 1 "$case_connections" \
        ip netns exec cli taskset -c "$CLIENT_CPUS" &
    local clients=$!
    # Downstream, capture 1 s of the Server's sends near the end, after
    # the CPU window: the Ephemeral Ports of about 6 Test Connections.
    if [ "$case_direction" = downstream ]; then
        local ramp span=$(((PORTS_LAST - PORTS_FIRST + 1) * 6 / case_connections))
        ramp=$(awk -v n="$case_connections" -v s="$STAGGER" 'BEGIN { print n * s + 1 }')
        sleep "$(awk -v r="$ramp" -v i="$interval" 'BEGIN { print r + i - 4 }')"
        ((span >= 1)) || span=1
        ((span <= PORTS_LAST - PORTS_FIRST + 1)) || span=$((PORTS_LAST - PORTS_FIRST + 1))
        date +%s.%N >"$dir/pacing.start"
        ip netns exec srv taskset -c "$COLLECTOR_CPU" timeout 1 tcpdump -Z root -i vs -s 96 -B 65536 \
            --time-stamp-precision=nano -w "$dir/pacing.pcap" \
            "udp and src portrange $PORTS_FIRST-$((PORTS_FIRST + span - 1))" 2>"$dir/pacing.err" || true
    fi
    wait "$clients"
    kill -TERM "$server" 2>/dev/null || true
    wait "$server" 2>/dev/null || true
    wait "$sampler" 2>/dev/null || true
}

serve() {
    local dir=$1
    shift
    [ -n "${KEY_FILE:-}" ] || die "set KEY_FILE to a key file with an Operator Key"
    mkdir -p "$dir"
    write_config "$dir/server.toml" "$(cd "$(dirname "$KEY_FILE")" && pwd)/$(basename "$KEY_FILE")"
    taskset -c "$SERVER_CPUS" "$UDPSTP" server --config "$dir/server.toml" --log-format json "$@" \
        >"$dir/server.log" 2>&1 &
    local server=$!
    trap 'kill -TERM $server 2>/dev/null' INT TERM
    taskset -c "$COLLECTOR_CPU" "$BENCH" sample "$dir/samples.jsonl" "$server" &
    wait "$server" || true
    wait
}

clients() {
    local dir=$1 server=$2
    parse_case "$3"
    local first=${4:-1}
    [[ $first =~ ^[0-9]+$ ]] || die "FIRST is a number, not $first"
    local count=${5:-$((case_connections - first + 1))}
    if ! [[ $count =~ ^[0-9]+$ ]] || ((first < 1 || first + count - 1 > case_connections)); then
        die "FIRST and COUNT must pick Clients within the case's $case_connections"
    fi
    [ -n "${KEY_FILE:-}" ] || die "set KEY_FILE to a key file with an Operator Key"
    mkdir -p "$dir"
    write_meta "$dir" "${TEST_INTERVAL:-20}"
    run_clients "$dir" "$server" "${TEST_INTERVAL:-20}" "$KEY_FILE" "$first" $((first + count - 1)) \
        taskset -c "$CLIENT_CPUS"
}

report() {
    "$BENCH" campaign-report "$1" "$SERVER_CPUS" "$CLIENT_CPUS"
}

command=${1:-}
shift || true
case $command in
netns) netns_campaign "$@" ;;
serve) serve "$@" ;;
clients) clients "$@" ;;
report) report "$@" ;;
*) sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//' >&2; exit 2 ;;
esac
