//! Short Linux execution/proc receipts. Admission and failure decisions live
//! in the Rust coordinator, rather than a second remote experiment driver.

use bench_rounds::quote::{join, quote};

use super::Args;

pub(super) fn endpoint_args(a: &Args, role: &str) -> Vec<String> {
    let w = a.workload(role);
    let mut args: Vec<String> = [
        "../endpoint",
        "receive-probe",
        if role == "receiver" {
            "receive"
        } else {
            "send"
        },
    ]
    .into_iter()
    .map(String::from)
    .collect();
    for (key, value) in [
        ("--bind", w.bind.to_string()),
        ("--peer", w.peer.to_string()),
        ("--run-id", w.run_id.to_string()),
        ("--source", w.source),
        ("--count", w.count.to_string()),
        ("--pps", w.pps.to_string()),
        ("--deadline-ms", w.deadline_ms.to_string()),
        ("--drain-ms", w.drain_ms.to_string()),
        ("--start-delay-ms", w.start_delay_ms.to_string()),
        ("--cpu", w.cpu.to_string()),
        ("--socket-buffer", w.socket_buffer.to_string()),
        ("--out", "result.json".into()),
    ] {
        args.extend([key.into(), value]);
    }
    if role == "receiver" {
        args.extend([
            "--mode".into(),
            match a.mode {
                crate::receive_probe::ReceiveMode::HeaderOnly => "header-only",
                crate::receive_probe::ReceiveMode::Gro => "gro",
            }
            .into(),
        ]);
    }
    args
}

pub(super) fn argv_hex(args: &[String]) -> String {
    args.iter()
        .flat_map(|s| s.bytes().chain([0]))
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub(super) fn prepare(role: &str, hash: &str) -> String {
    format!(
        "# pair-prepare\nset -eu\n[ \"$(sha256sum endpoint | cut -d' ' -f1)\" = {} ]\numask 077\nmkdir {}\n",
        quote(hash),
        quote(role)
    )
}

pub(super) fn placement(receiver: usize, sampler: usize) -> String {
    placement_from(receiver, sampler, "/sys/devices/system/cpu")
}

pub(super) fn placement_from(receiver: usize, sampler: usize, root: &str) -> String {
    let root = quote(root);
    format!(
        r#"# pair-placement
set -eu
r={root}/cpu{receiver}/topology
s={root}/cpu{sampler}/topology
rp=$(cat "$r/physical_package_id"); rc=$(cat "$r/core_id")
sp=$(cat "$s/physical_package_id"); sc=$(cat "$s/core_id")
printf 'receiver_cpu={receiver} package=%s core=%s\nsampler_cpu={sampler} package=%s core=%s\n' "$rp" "$rc" "$sp" "$sc" > receiver/placement.txt
cat {root}/online >> receiver/placement.txt
cat "$r/thread_siblings_list" >> receiver/placement.txt
cat "$s/thread_siblings_list" >> receiver/placement.txt
cat receiver/placement.txt
"#
    )
}

// stat's comm may contain spaces or ')'. Remove through the last ')', then
// starttime is field 20 of the remainder, with state at field 1.
const IDENTITY: &str = r#"
identity() {
    local pid=$1 stat rest
    stat=$(cat "/proc/$pid/stat") || return 1
    rest=${stat##*) }; set -- $rest
    [[ ${20} =~ ^[0-9]+$ ]] || return 1
    printf '%s %s %s\n' "$pid" "${20}" "$(stat -c %u "/proc/$pid")"
}
same() {
    local pid start uid now
    read -r pid start uid < "$1" || return 1
    [[ $pid =~ ^[1-9][0-9]*$ && $start =~ ^[0-9]+$ && $uid == "$EUID" ]] || return 1
    [[ $(readlink "/proc/$pid/cwd") == "$PWD" ]] || return 1
    now=$(identity "$pid") || return 1
    [[ "$now" == "$pid $start $uid" ]]
}
"#;

pub(super) fn launch(role: &str, args: &[String], deadline_ms: u64) -> String {
    let endpoint = join(args);
    let worker = format!(
        "{IDENTITY}\nidentity $$ > workload.identity.tmp && mv workload.identity.tmp workload.identity\nexec {endpoint}"
    );
    // Endpoint snapshots have their own 30-second limits before and after the
    // finite socket lifetime. The outer timeout bounds all of that and writers.
    let seconds = deadline_ms.div_ceil(1000) + 75;
    let supervisor = format!(
        r#"set -u
cd {role} || exit 1
{IDENTITY}
identity $$ > supervisor.identity.tmp && mv supervisor.identity.tmp supervisor.identity || exit 1
timeout --signal=TERM --kill-after=3s {seconds}s bash -c {worker} > endpoint.stdout 2> endpoint.stderr &
p=$!
identity "$p" > timeout.identity.tmp && mv timeout.identity.tmp timeout.identity
wait "$p"; status=$?
printf '%s\n' "$status" > exit.status.tmp
mv exit.status.tmp exit.status
"#,
        role = quote(role),
        worker = quote(&worker)
    );
    format!(
        "# pair-launch-{role}\nset -eu\n(set -o noclobber; : > {role}/launch.intent)\nsetsid -f bash -c {} > {role}/supervisor.stdout 2> {role}/supervisor.stderr < /dev/null\n",
        quote(&supervisor)
    )
}

pub(super) fn readiness() -> String {
    format!(
        r#"# pair-readiness
set -eu
cd receiver
{IDENTITY}
if [ -f exit.status ]; then echo 'receiver exited before admission' >&2; exit 1; fi
if [ ! -f result.ready.json ] || [ ! -f workload.identity ]; then printf 'pending\n'; exit 0; fi
same workload.identity || exit 1
read -r pid rest < workload.identity
state=$(sed 's/.*) //' /proc/$pid/stat | cut -d' ' -f1)
[ "$state" != Z ] && [ "$state" != X ]
printf '%s\n' "$pid"
sha256sum /proc/$pid/exe | cut -d' ' -f1
od -An -v -tx1 /proc/$pid/cmdline | tr -d ' \n'
printf '\n---fds---\n'
for fd in /proc/$pid/fd/*; do readlink "$fd" 2>/dev/null || true; done
printf '\n---udp---\n'
cat /proc/$pid/net/udp
printf '\n---ready---\n'
cat result.ready.json
"#
    )
}

pub(super) fn status(role: &str) -> String {
    format!(
        "# pair-wait-{role}\nif [ -f {role}/exit.status ]; then cat {role}/exit.status; else echo pending; fi\n"
    )
}

pub(super) fn capture_start(a: &Args, pid: u32) -> String {
    let seconds = a.deadline_ms.div_ceil(1000) + 75;
    format!(
        "# pair-capture-start\nset -eu\ncd receiver\ntaskset -c {} ../endpoint capture start vm-probe {pid} {seconds} - > capture.start.json\ncat capture.start.json\n",
        a.sampler_cpu
    )
}

pub(super) const CAPTURE_STOP: &str = "# pair-capture-stop\nset -eu\nif [ ! -d receiver ]; then exit 0; fi\ncd receiver\nif [ ! -e vm-probe.start.json ] && [ ! -e vm-probe.pid.json ]; then exit 0; fi\n../endpoint capture stop vm-probe > capture.stop.json\ncat capture.stop.json\n";

pub(super) fn stop(role: &str) -> String {
    format!(
        r#"# pair-stop-{role}
set -eu
if [ ! -d {role} ]; then exit 0; fi
cd {role}
if [ ! -f launch.intent ]; then exit 0; fi
{IDENTITY}
# An interrupted launch may not yet have registered. Never treat that as
# absence. Keep its workspace for recovery rather than erase the intent.
[ -f supervisor.identity ] || exit 1
# Stop only receipts whose start time, UID and private working directory
# still match. A reused live PID is unconfirmed, never a signal target.
ended() {{
    local pid start uid stat rest
    read -r pid start uid < "$1" || return 1
    [[ $pid =~ ^[1-9][0-9]*$ ]] || return 1
    [ -d /proc/$pid ] || return 0
    stat=$(cat /proc/$pid/stat) || return 1
    rest=${{stat##*) }}; set -- $rest
    [ "$1" = Z ] || [ "$1" = X ]
}}
for receipt in workload.identity timeout.identity supervisor.identity; do
    if [ ! -f "$receipt" ]; then
        [ -f exit.status ] || exit 1
        continue
    fi
    if ended "$receipt"; then continue; fi
    same "$receipt" || exit 1
    read -r pid rest < "$receipt"
    kill -TERM "$pid" 2>/dev/null || true
    for attempt in {{1..20}}; do ended "$receipt" && break; sleep 0.1; done
    if ! ended "$receipt"; then
        same "$receipt" || exit 1
        kill -KILL "$pid" 2>/dev/null || true
        for attempt in {{1..20}}; do ended "$receipt" && break; sleep 0.1; done
    fi
    ended "$receipt" || exit 1
done
# Snapshot commands can have their own bounded process groups. Check every
# remaining process in this private role directory, excluding this control
# shell and its ancestors. This uses UID/start-time/cwd identity, never names.
ancestors=" $$ "
p=$PPID
while [[ $p =~ ^[1-9][0-9]*$ && $p -gt 1 ]]; do
    ancestors+="$p "
    p=$(sed 's/.*) //' /proc/$p/stat 2>/dev/null | cut -d' ' -f2) || break
done
for proc in /proc/[0-9]*/cwd; do
    p=${{proc#/proc/}}; p=${{p%/cwd}}
    [[ $ancestors != *" $p "* ]] || continue
    [[ $(readlink "$proc" 2>/dev/null || true) == "$PWD" ]] || continue
    [ "$(stat -c %u /proc/$p)" = "$EUID" ] || exit 1
    identity "$p" > cleanup.identity
    same cleanup.identity || exit 1
    kill -TERM "$p" 2>/dev/null || true
    for attempt in {{1..20}}; do ended cleanup.identity && break; sleep 0.1; done
    if ! ended cleanup.identity; then
        same cleanup.identity || exit 1
        kill -KILL "$p" 2>/dev/null || true
        for attempt in {{1..20}}; do ended cleanup.identity && break; sleep 0.1; done
    fi
    ended cleanup.identity || exit 1
done
for proc in /proc/[0-9]*/cwd; do
    p=${{proc#/proc/}}; p=${{p%/cwd}}
    [[ $ancestors != *" $p "* ]] || continue
    [[ $(readlink "$proc" 2>/dev/null || true) != "$PWD" ]] || exit 1
done
printf 'owned workload processes stopped\n'
"#,
        role = quote(role)
    )
}

pub(super) fn manifest(role: &str) -> String {
    format!(
        "# pair-manifest-{role}\nset -eu\ncd {role}\nfind . -type f ! -name SHA256SUMS -print0 | sort -z | xargs -0 -r sha256sum > SHA256SUMS\n[ -s SHA256SUMS ]\n"
    )
}
