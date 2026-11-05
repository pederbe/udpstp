//! Advisory host figures, kept separate from the first Round's gates.

use std::collections::{HashMap, HashSet};

use bench_rounds::num::{self, Num};
use bench_rounds::py::Json;
use bench_rounds::samples;
use indexmap::IndexMap;

use super::judge::Record;
use super::report::RoundFiles;

pub(super) fn planned_runs(
    files: &dyn RoundFiles,
    attempted: Vec<Record>,
) -> Result<Vec<Record>, super::judge::Unreadable> {
    let Some(bytes) = files.read("first-plan.json") else {
        return Ok(attempted);
    };
    let read = || -> Option<Vec<Record>> {
        let plan = Json::parse(std::str::from_utf8(&bytes).ok()?).ok()?;
        if !matches!(field(&plan, "version"), Some(Json::Int(1 | 2))) {
            return None;
        }
        let Json::List(planned) = field(&plan, "runs")? else {
            return None;
        };
        if planned.is_empty() {
            return None;
        }
        let text = planned
            .iter()
            .map(bench_rounds::py::dumps)
            .collect::<Vec<_>>()
            .join("\n");
        let mut runs = super::judge::read_summary(&text).ok()?;
        if runs.len() != planned.len() {
            return None;
        }
        let mut labels = HashSet::new();
        for run in &runs {
            if !labels.insert(&run.label)
                || run.status.is_some()
                || run.test_interval.is_none_or(|n| n <= 0)
            {
                return None;
            }
        }
        let mut seen = HashSet::new();
        for run in &attempted {
            if !seen.insert(&run.label) {
                return None;
            }
            let expected = runs.iter_mut().find(|r| r.label == run.label)?;
            let mut normalized = run.clone();
            normalized.status = None;
            if &normalized != expected {
                return None;
            }
            *expected = run.clone();
        }
        Some(runs)
    };
    read().ok_or_else(|| {
        super::judge::Unreadable::new("first-plan.json: invalid schedule or mismatched Round Runs")
    })
}

fn number(value: &Json) -> Option<f64> {
    let n = Num::from_json(value)?.to_f64();
    (n.is_finite() && n >= 0.0).then_some(n)
}

fn field<'a>(value: &'a Json, key: &str) -> Option<&'a Json> {
    value.as_dict()?.get(key)
}

fn time(value: &Json) -> Option<f64> {
    number(field(value, "t")?)
}

fn cpu_counters(value: &Json) -> Option<Vec<i128>> {
    let Json::List(values) = value else {
        return None;
    };
    if values.len() != 8 {
        return None;
    }
    values
        .iter()
        .map(|v| match v {
            Json::Int(n) if *n >= 0 => Some(*n),
            _ => None,
        })
        .collect()
}

struct HostFigures {
    guest: f64,
    steal: f64,
    softirq: f64,
    net_rx: Option<f64>,
    switches: Option<f64>,
    duration: f64,
    engine_duration: Option<f64>,
    engine_threads: Option<usize>,
}

fn read_host(capture: &Json, engines: bool) -> Option<HostFigures> {
    if field(capture, "status") != Some(&Json::Int(0)) {
        return None;
    }
    let Json::List(span) = field(capture, "window")? else {
        return None;
    };
    if span.len() != 2 {
        return None;
    }
    let (t0, t1) = (number(&span[0])?, number(&span[1])?);
    if t1 <= t0 {
        return None;
    }
    let Json::Str(raw) = field(capture, "samples")? else {
        return None;
    };
    let records: Vec<Json> = raw
        .split('\n')
        .filter(|line| !line.trim().is_empty())
        .map(Json::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    let mut previous = None;
    for record in &records {
        let t = time(record)?;
        if previous.is_some_and(|p| t <= p) {
            return None;
        }
        previous = Some(t);
        let cpus = field(record, "cpus")?.as_dict()?;
        if cpus.is_empty() || cpus.values().any(|v| cpu_counters(v).is_none()) {
            return None;
        }
    }
    let (first, last) = samples::window(&records, t0, t1).ok()??;
    let first_cpus = field(first, "cpus")?.as_dict()?;
    let last_cpus = field(last, "cpus")?.as_dict()?;
    let inside: Vec<_> = records
        .iter()
        .filter(|s| {
            time(s).is_some_and(|t| t >= time(first).unwrap_or(t0) && t <= time(last).unwrap_or(t1))
        })
        .collect();
    for pair in inside.windows(2) {
        let (a, b) = (
            field(pair[0], "cpus")?.as_dict()?,
            field(pair[1], "cpus")?.as_dict()?,
        );
        if a.len() != b.len() {
            return None;
        }
        for (cpu, v) in a {
            if cpu_counters(v)?
                .iter()
                .zip(cpu_counters(b.get(cpu)?)?)
                .any(|(a, b)| b < *a)
            {
                return None;
            }
        }
    }
    if first_cpus.len() != last_cpus.len() {
        return None;
    }
    let mut steal = 0.0;
    for (cpu, a) in first_cpus {
        let (a, b) = (cpu_counters(a)?, cpu_counters(last_cpus.get(cpu)?)?);
        let delta: Vec<i128> = a.iter().zip(b).map(|(a, b)| b - a).collect();
        let total: i128 = delta.iter().try_fold(0_i128, |n, v| n.checked_add(*v))?;
        if total <= 0 || delta.iter().any(|n| *n < 0) {
            return None;
        }
        steal += delta[7] as f64 / total as f64;
    }
    let use_cpu = samples::cpu_use(&records, t0, t1, None).ok()??;
    let interrupts = |key| {
        let Json::Str(raw) = field(capture, key)? else {
            return None;
        };
        crate::scale::judge::interrupt_counts(raw).ok()
    };
    let rx = (|| {
        let (before, after) = (interrupts("before")?, interrupts("after")?);
        let a = field(&before, "NET_RX")?.as_dict()?;
        let b = field(&after, "NET_RX")?.as_dict()?;
        if a.is_empty() || a.len() != b.len() {
            return None;
        }
        let mut rise = 0.0;
        for (cpu, n) in a {
            let delta = number(b.get(cpu)?)? - number(n)?;
            if delta < 0.0 {
                return None;
            }
            rise += delta;
        }
        Some(rise / (t1 - t0))
    })();
    let (switches, engine_duration, engine_threads) = if engines {
        engine_switches(&records, time(first)?, time(last)?)
    } else {
        (None, None, None)
    };
    Some(HostFigures {
        guest: use_cpu.busy - steal,
        steal,
        softirq: use_cpu.softirq,
        net_rx: rx,
        switches,
        duration: time(last)? - time(first)?,
        engine_duration,
        engine_threads,
    })
}

fn engine_switches(
    records: &[Json],
    t0: f64,
    t1: f64,
) -> (Option<f64>, Option<f64>, Option<usize>) {
    fn read(records: &[Json], t0: f64, t1: f64) -> Option<(f64, f64, usize)> {
        let inside: Vec<&Json> = records
            .iter()
            .filter(|s| time(s).is_some_and(|t| t >= t0 && t <= t1))
            .collect();
        let mut ids = HashSet::new();
        for record in &inside {
            for (tid, values) in field(record, "threads")?.as_dict()? {
                if let Json::List(v) = values
                    && v.len() == 4
                    && matches!(&v[0], Json::Str(name) if name.starts_with("udpstp-eng-"))
                {
                    if v[1..].iter().any(|n| !matches!(n, Json::Int(n) if *n >= 0)) {
                        return None;
                    }
                    ids.insert(tid.clone());
                }
            }
        }
        if ids.is_empty() {
            return None;
        }
        let mut seen = HashSet::new();
        let mut previous_values: HashMap<String, Vec<Json>> = HashMap::new();
        for record in &inside {
            let threads = field(record, "threads")?.as_dict()?;
            if seen.iter().any(|id| !threads.contains_key(id)) {
                return None;
            }
            for id in &ids {
                if let Some(Json::List(value)) = threads.get(id) {
                    if let Some(old) = previous_values.get(id) {
                        if value.len() != 4 || value[0] != old[0] {
                            return None;
                        }
                        for (a, b) in old[1..].iter().zip(&value[1..]) {
                            if !matches!((a, b), (Json::Int(a), Json::Int(b)) if b >= a) {
                                return None;
                            }
                        }
                    }
                    if matches!(value.first(), Some(Json::Str(name)) if name.starts_with("udpstp-eng-"))
                    {
                        previous_values.insert(id.clone(), value.clone());
                        seen.insert(id.clone());
                    }
                } else if seen.contains(id) {
                    return None;
                }
            }
        }
        let start = inside.iter().position(|s| {
            field(s, "threads")
                .and_then(Json::as_dict)
                .is_some_and(|threads| ids.iter().all(|id| threads.contains_key(id)))
        })?;
        let selected = &inside[start..];
        if selected.len() < 2 {
            return None;
        }
        let mut filtered = Vec::new();
        let mut previous: HashMap<String, (String, i128)> = HashMap::new();
        for record in selected {
            let threads = field(record, "threads")?.as_dict()?;
            let mut kept = IndexMap::new();
            for id in &ids {
                let value = threads.get(id)?;
                let Json::List(v) = value else { return None };
                let (Some(Json::Str(name)), Some(Json::Int(switches))) = (v.first(), v.get(2))
                else {
                    return None;
                };
                if !name.starts_with("udpstp-eng-") || *switches < 0 {
                    return None;
                }
                if let Some((old_name, old_switches)) = previous.get(id)
                    && (name != old_name || switches < old_switches)
                {
                    return None;
                }
                previous.insert(id.clone(), (name.clone(), *switches));
                kept.insert(id.clone(), value.clone());
            }
            let mut sample = record.as_dict()?.clone();
            sample.insert("threads".into(), Json::Dict(kept));
            filtered.push(Json::Dict(sample));
        }
        let (a, b) = (time(filtered.first()?)?, time(filtered.last()?)?);
        Some((
            samples::wakeups(&filtered, a, b).ok()??.wakeups_per_s?,
            b - a,
            ids.len(),
        ))
    }
    read(records, t0, t1).map_or((None, None, None), |(s, t, n)| (Some(s), Some(t), Some(n)))
}

/// Only a complete isolated Server capture supplies its allowed pool capacity.
fn allowed_threads(capture: &Json) -> Option<usize> {
    if field(capture, "status") != Some(&Json::Int(0)) {
        return None;
    }
    let log = field(capture, "log")?.as_str()?;
    if !log.ends_with('\n') {
        return None;
    }
    let mut found = None;
    for line in log.lines().filter(|line| !line.trim().is_empty()) {
        let event = Json::parse(line).ok()?;
        let fields = event.as_dict()?;
        // Retain the lowercase spelling used by earlier first-Round fixtures.
        if matches!(
            fields.get("event").and_then(Json::as_str),
            Some("DataPhaseEngines" | "data_phase_engines")
        ) {
            if found.is_some()
                || fields.get("executor").and_then(Json::as_str) != Some("pool")
                || fields.get("warning") == Some(&Json::Bool(true))
                || !matches!(fields.get("error"), None | Some(Json::Null))
            {
                return None;
            }
            let Json::Int(n) = fields.get("engine_threads")? else {
                return None;
            };
            found = usize::try_from(*n).ok().filter(|n| *n > 0);
            found?;
        }
    }
    found
}

fn cap_lines(
    files: &dyn RoundFiles,
    runs: &[Record],
    labels: &HashMap<&str, usize>,
) -> Vec<String> {
    let selected: Vec<_> = runs
        .iter()
        .filter(|r| r.server_engine_threads.is_some())
        .collect();
    if selected.is_empty() {
        return Vec::new();
    }
    let mut lines = ["", "## Server Engine Thread caps", "",
        "Advisory experimental arms, excluded from the default OB-UDPST comparison criterion. Requested maximum is the matrix setting. Allowed maximum comes from the isolated Server's pool startup event. Sampled threads count the Engine Threads observed over their common sampled lifetime, not their allowed capacity or simultaneous CPU activity. Missing or invalid readback remains unavailable.", "",
        "| Round Run | Requested maximum | Allowed maximum | Sampled Engine Threads |",
        "|---|---:|---:|---:|"].map(String::from).to_vec();
    for run in selected {
        let capture = |suffix| {
            files
                .read(&format!("tests/{}.{suffix}.json", run.label))
                .and_then(|bytes| String::from_utf8(bytes).ok())
                .and_then(|text| Json::parse(&text).ok())
                .filter(|c| {
                    field(c, "label") == Some(&Json::Str(run.label.clone()))
                        && labels.get(run.label.as_str()) == Some(&1)
                })
        };
        let allowed = capture("server").as_ref().and_then(allowed_threads);
        let sampled = capture("vm")
            .as_ref()
            .and_then(|c| field(c, "hosts"))
            .and_then(|h| field(h, "server"))
            .and_then(|h| read_host(h, true))
            .and_then(|h| h.engine_threads);
        let show = |n: Option<usize>| n.map_or_else(|| "unavailable".into(), |n| n.to_string());
        lines.push(format!(
            "| {} | {} | {} | {} |",
            run.label,
            run.server_engine_threads.expect("selected cap"),
            show(allowed),
            show(sampled)
        ));
    }
    lines
}

pub(super) fn host_lines(
    files: &dyn RoundFiles,
    runs: &[Record],
    labels: &HashMap<&str, usize>,
) -> Vec<String> {
    let selected: Vec<_> = runs.iter().filter(|r| r.host_samples).collect();
    let mut lines = cap_lines(files, runs, labels);
    if selected.is_empty() {
        return lines;
    }
    lines.extend(["", "## Host diagnostics", "",
        "Advisory figures over each host's collection window, including the Control Phase. CPU figures count logical CPUs; guest execution excludes steal time. Softirq time covers all softirqs; NET_RX counts receive softirq events. Engine Thread voluntary context switches are a wake-up proxy over their common sampled lifetime. Missing figures are unavailable and add no rule for voiding a Round Run.", "",
        "| Round Run | Host | Guest CPUs busy | Steal CPUs | Softirq CPUs | NET_RX/s | Engine Thread switches/s | Sampled seconds | Engine seconds |",
        "|---|---|---:|---:|---:|---:|---:|---:|---:|"].map(String::from));
    let mut missing = Vec::new();
    for run in selected {
        let capture = files
            .read(&format!("tests/{}.vm.json", run.label))
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .and_then(|text| Json::parse(&text).ok())
            .filter(|c| {
                field(c, "label") == Some(&Json::Str(run.label.clone()))
                    && labels.get(run.label.as_str()) == Some(&1)
            });
        let client = if run.client_host == "home" {
            "home"
        } else {
            "client"
        };
        for role in ["server", client] {
            let engines = role == "server" && run.server == "udpstp";
            let found = capture
                .as_ref()
                .and_then(|c| field(c, "hosts"))
                .and_then(|h| field(h, role))
                .and_then(|h| read_host(h, engines));
            let cells = if let Some(f) = found {
                let show = |n: Option<f64>, digits| {
                    n.map_or_else(|| "unavailable".into(), |v| num::fixed(v, digits))
                };
                vec![
                    show(Some(f.guest), 2),
                    show(Some(f.steal), 2),
                    show(Some(f.softirq), 2),
                    show(f.net_rx, 0),
                    if engines {
                        show(f.switches, 0)
                    } else {
                        "not applicable".into()
                    },
                    show(Some(f.duration), 2),
                    if engines {
                        show(f.engine_duration, 2)
                    } else {
                        "not applicable".into()
                    },
                ]
            } else {
                missing.push(format!(
                    "- `{}` ({role}): host evidence missing or invalid",
                    run.label
                ));
                vec!["unavailable".to_string(); 7]
            };
            lines.push(format!(
                "| {} | {role} | {} |",
                run.label,
                cells.join(" | ")
            ));
        }
    }
    if !missing.is_empty() {
        lines.push(String::new());
        lines.extend(missing);
    }
    lines
}
