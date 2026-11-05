//! The shell text of Server-scale Round Runs, byte for byte scale.py's.
//! Keys stay in the Round's files, never in its command lines. Host paths
//! come from the validated matrix and the Round's own work directory.

use std::collections::BTreeMap;

use bench_rounds::num::{self, Num};
use bench_rounds::quote::{join, quote};

use super::{Error, hosts::cpu_text, matrix::ScaleRun};

fn environment(env: &BTreeMap<String, String>) -> String {
    env.iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Start in a separate session, writing the PID before exec.
pub fn background(command: &str, pidfile: &str, log: &str) -> String {
    let inner = format!("echo $$ > {}; exec {command}", quote(pidfile));
    format!(
        "setsid -f sh -c {} > {} 2>&1 < /dev/null",
        quote(&inner),
        quote(log)
    )
}

pub fn serve(
    directory: &str,
    port: i64,
    cpus: &[i64],
    collector: i64,
    env: &BTreeMap<String, String>,
) -> String {
    let settings = format!(
        "UDPSTP=./udpstp KEY_FILE=keys.toml SERVER_CPUS={} COLLECTOR_CPU={collector} {}",
        cpu_text(cpus),
        environment(env)
    );
    let serve = format!(
        "env {} scripts/server-scale.sh serve {} --port {port}",
        settings.trim_end(),
        quote(directory)
    );
    background(
        &serve,
        &format!("{directory}/serve.pid"),
        &format!("{directory}/serve.out"),
    )
}

/// The host sampler uses the uploaded Rust executable.
pub fn sampled(command: &str, directory: &str) -> String {
    format!(
        "({command} & c=$!; scripts/udpstp-bench sample {} $c & wait $c; s=$?; wait; exit $s)",
        quote(&format!("{directory}/host-samples.jsonl"))
    )
}

/// Placement and options for one host's share of a Round Run's Clients.
pub struct Clients<'a> {
    pub directory: &'a str,
    pub servers: &'a str,
    pub first: i64,
    pub count: i64,
    pub cpus: &'a [i64],
    pub stamps: bool,
    pub options: &'a str,
    pub environment: &'a BTreeMap<String, String>,
}

pub fn clients(run: &ScaleRun, target: &Clients<'_>) -> String {
    let mut settings = format!(
        "UDPSTP=./udpstp KEY_FILE=keys.toml CLIENT_CPUS={} TEST_INTERVAL={} STAGGER={} {}",
        cpu_text(target.cpus),
        run.test_interval,
        general(run.stagger.to_f64()),
        environment(target.environment)
    )
    .trim_end()
    .to_string();
    if !target.options.is_empty() {
        settings.push_str(&format!(" CLIENT_OPTIONS={}", quote(target.options)));
    }
    if target.stamps {
        settings.push_str(" UDPSTP_KERNEL_TX_TIMESTAMPS=1");
    }
    let command = format!(
        "env {settings} scripts/server-scale.sh clients {} {} {}={}:{}:{} {} {}",
        quote(target.directory),
        target.servers,
        run.case,
        run.direction,
        run.count,
        run.row,
        target.first,
        target.count
    );
    format!(
        "mkdir -p {} && {}",
        quote(target.directory),
        sampled(&command, target.directory)
    )
}

pub fn obudpst_server(
    directory: &str,
    ports: &[i64],
    cpus: &[i64],
    collector: Option<i64>,
) -> String {
    let taskset = if cpus.is_empty() {
        String::new()
    } else {
        format!("taskset -c {} ", cpu_text(cpus))
    };
    let sampler = collector
        .map(|c| format!("taskset -c {c} "))
        .unwrap_or_default();
    let starts = ports.iter().map(|port| format!("{taskset}./udpst -K udpst.keys -p {port} > {directory}/server-{port}.log 2>&1 & pids=\"$pids $!\";")).collect::<Vec<_>>().join(" ");
    let inner = format!(
        "pids=; {starts} echo $pids > {directory}/server.pid; {sampler}scripts/udpstp-bench sample {directory}/samples.jsonl $(echo $pids | tr ' ' ,) & wait $pids; wait"
    );
    format!(
        "setsid -f sh -c {} > {directory}/serve.out 2>&1 < /dev/null",
        quote(&inner)
    )
}

/// One side's placement and shared start time for a prototype Round Run.
pub struct Prototype<'a> {
    pub side: &'a str,
    pub directory: &'a str,
    pub start_wall: Num,
    pub p_address: &'a str,
    pub q_addresses: &'a [String],
    pub q_index: i64,
    pub cpus: &'a [i64],
    pub per_core: &'a [i64],
    pub stamps: bool,
}

pub fn prototype(run: &ScaleRun, target: &Prototype<'_>) -> Result<String, Error> {
    let collector = target
        .cpus
        .last()
        .ok_or_else(|| Error("the prototype needs at least one CPU".into()))?;
    let cpus = cpu_text(target.cpus);
    let mut args = vec![
        "./dpproto".into(),
        "run".into(),
        "--side".into(),
        target.side.into(),
    ];
    let mut add = |words: &[&str]| {
        args.extend(words.iter().map(|s| s.to_string()));
    };
    if target.side == "p" {
        match run.engine.as_str() {
            "prototype-a" => add(&["--engine", "a-t", "--cpus", &cpus]),
            "prototype-b" => add(&[
                "--engine", "b-t", "--b-cpus", &cpus, "--b-pin", "none", "--cpus", &cpus,
            ]),
            _ => add(&[
                "--engine",
                "b-t",
                "--b-cpus",
                &cpu_text(target.per_core),
                "--cpus",
                &cpus,
            ]),
        }
    } else {
        let runtimes: Vec<_> = target
            .cpus
            .iter()
            .copied()
            .filter(|c| c != collector)
            .collect();
        let runtimes = if runtimes.is_empty() {
            target.cpus
        } else {
            &runtimes
        };
        add(&[
            "--engine",
            "b-t",
            "--b-cpus",
            &cpu_text(runtimes),
            "--cpus",
            &cpus,
            "--q-index",
            &target.q_index.to_string(),
        ]);
    }
    let direction = if run.direction == "downstream" {
        "pq"
    } else {
        "qp"
    };
    let start = match target.start_wall {
        Num::Int(n) => n.to_string(),
        Num::Float(n) => num::repr(n),
    };
    add(&[
        "--collector-cpu",
        &collector.to_string(),
        "--stat-cpus",
        &cpus,
        "--start-wall",
        &start,
        "--warm",
        &run.warm_up.to_string(),
        "--dur",
        &run.test_interval.to_string(),
        "--conns",
        &format!("{}*row{}:{direction}", run.count, run.row),
        "--random-phase",
        "--ts-one-in",
        if target.stamps && target.side == "p" {
            "1"
        } else {
            "0"
        },
        "--p-addr",
        target.p_address,
        "--q-addr",
        &target.q_addresses.join(","),
        "--label",
        &run.label(),
        "--out",
        &format!("{}/{}.json", target.directory, target.side),
    ]);
    Ok(join(&args))
}

pub fn prototype_p(command: &str, directory: &str) -> String {
    let inner = format!(
        "{command} > {directory}/p.log 2>&1 & p=$!; echo $p > {directory}/p.pid; scripts/udpstp-bench sample {directory}/samples.jsonl $p & wait $p; wait"
    );
    format!(
        "setsid -f sh -c {} > {directory}/serve.out 2>&1 < /dev/null",
        quote(&inner)
    )
}

pub fn wait_listening(directory: &str) -> String {
    format!(
        "for i in $(seq 100); do grep -q '\"Listening\"' {directory}/server.log 2>/dev/null && exit 0; sleep 0.1; done; tail -5 {directory}/server.log; exit 1"
    )
}

pub fn stop(pidfile: &str, wait_s: i64, first_s: i64) -> String {
    let alive = "a=; for p in $ps; do kill -0 $p 2>/dev/null && a=1; done; [ -z \"$a\" ] && exit 0";
    let first = if first_s == 0 {
        String::new()
    } else {
        format!(
            " for _ in $(seq {}); do {alive}; sleep 0.1; done;",
            first_s * 10
        )
    };
    format!(
        "ps=$(cat {pidfile} 2>/dev/null) || exit 0;{first} kill -TERM $ps 2>/dev/null; for _ in $(seq {}); do {alive}; sleep 0.1; done; kill -KILL $ps 2>/dev/null; exit 0",
        wait_s * 10
    )
}

pub fn alive(pidfile: &str) -> String {
    format!(
        "ps=$(cat {pidfile} 2>/dev/null) || {{ echo 'dead: no PID'; exit 0; }}; d=; for p in $ps; do kill -0 $p 2>/dev/null || d=\"$d $p\"; done; [ -z \"$d\" ] && echo alive || echo \"dead$d\""
    )
}

/// Python's default `g` format, six significant digits, for the stagger.
fn general(value: f64) -> String {
    let scientific = format!("{value:.5e}");
    let Some((mantissa, exponent)) = scientific.split_once('e') else {
        return scientific;
    };
    let exponent: i32 = exponent.parse().unwrap_or(0);
    if !(-4..6).contains(&exponent) {
        format!(
            "{}e{exponent:+03}",
            mantissa.trim_end_matches('0').trim_end_matches('.')
        )
    } else {
        let fixed = format!("{value:.digits$}", digits = (5 - exponent).max(0) as usize);
        if fixed.contains('.') {
            fixed
                .trim_end_matches('0')
                .trim_end_matches('.')
                .to_string()
        } else {
            fixed
        }
    }
}
