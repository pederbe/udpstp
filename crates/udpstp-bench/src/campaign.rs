//! Reports of the earlier Server-scale campaign. This format is separate
//! from the pre-registered three-host Round reports.

use bench_rounds::{
    num::{self, Num},
    py::{self, Json},
    stats, utc,
};
use indexmap::IndexMap;
use std::fs;
use std::path::{Path, PathBuf};

const RATE_HEAD: &[&str] = &[
    "Run",
    "Completed",
    "Aggregate / planned Mbps",
    "Slowest TC mean",
    "Over-rate, worst SI",
    "Shortfall: SI median / p99 / worst",
    "SIs > 2 % short",
    "Lost Load PDUs",
    "Server not sent (Sender Bit Rate short of plan)",
    "Clients Skipped + Withheld",
    "RcvbufErrors Server / Clients",
];
const CPU_HEAD: &[&str] = &[
    "Run",
    "Window s",
    "Server CPUs busy (softirq)",
    "Server process",
    "Engine threads",
    "Engine thread CPU % median / max",
    "Wake-ups/s per thread median / max",
    "Wake-ups/s, all",
    "Other Server threads",
    "Clients' CPUs busy",
];

fn get<'a>(value: &'a Json, key: &str) -> &'a Json {
    value
        .as_dict()
        .and_then(|v| v.get(key))
        .unwrap_or(&Json::Null)
}
fn number(value: &Json) -> Result<Num, String> {
    Num::from_json(value).ok_or_else(|| "expected a campaign counter".into())
}
fn float(value: &Json) -> Result<f64, String> {
    Ok(number(value)?.to_f64())
}
fn list(value: &Json) -> Result<&[Json], String> {
    value
        .as_list()
        .ok_or_else(|| "expected a campaign list".into())
}
fn text(value: &Json) -> Result<&str, String> {
    value
        .as_str()
        .ok_or_else(|| "expected campaign text".into())
}
fn numeric(value: Num) -> Json {
    match value {
        Num::Int(v) => Json::Int(v),
        Num::Float(v) => Json::Float(v),
    }
}
fn opt(value: Option<f64>) -> Json {
    value.map_or(Json::Null, Json::Float)
}
fn maximum(values: &[f64]) -> Option<f64> {
    values.iter().copied().reduce(f64::max)
}
fn minimum(values: &[f64]) -> Option<f64> {
    values.iter().copied().reduce(f64::min)
}
fn sum(values: &[f64]) -> f64 {
    num::sum(&values.iter().map(|v| Num::Float(*v)).collect::<Vec<_>>()).to_f64()
}
fn read(path: &Path) -> Result<Json, String> {
    Json::parse(&fs::read_to_string(path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}
fn add(fields: &mut IndexMap<String, Json>, name: &str, value: Json) {
    fields.insert(name.into(), value);
}

fn cpu_list(text: &str) -> Result<Vec<String>, String> {
    let mut values = Vec::new();
    for part in text.split(',').filter(|part| !part.is_empty()) {
        if let Some((a, b)) = part.split_once('-') {
            let a = a.parse::<u32>().map_err(|e| e.to_string())?;
            let b = b.parse::<u32>().map_err(|e| e.to_string())?;
            if b.saturating_sub(a) > 4096 {
                return Err("campaign CPU list is too large".into());
            }
            values.extend((a..=b).map(|cpu| cpu.to_string()));
        } else {
            values.push(part.into());
        }
    }
    Ok(values)
}

fn busy(first: &Json, last: &Json, cpus: &[String]) -> Result<(f64, f64), String> {
    let (mut total, mut soft) = (0.0, 0.0);
    for cpu in cpus {
        let (a, b) = (get(get(first, "cpus"), cpu), get(get(last, "cpus"), cpu));
        if !a.truthy() || !b.truthy() {
            continue;
        }
        let delta = list(a)?
            .iter()
            .zip(list(b)?)
            .map(|(a, b)| Ok(float(b)? - float(a)?))
            .collect::<Result<Vec<_>, String>>()?;
        let span = sum(&delta);
        if span != 0.0 {
            if delta.len() < 7 {
                return Err("short CPU counter record".into());
            }
            total += (span - delta[3] - delta[4]) / span;
            soft += delta[6] / span;
        }
    }
    Ok((total, soft))
}

fn paths(directory: &Path) -> Result<Vec<PathBuf>, String> {
    match fs::read_dir(directory) {
        Ok(entries) => {
            let mut paths = entries
                .map(|e| e.map(|e| e.path()).map_err(|e| e.to_string()))
                .collect::<Result<Vec<_>, _>>()?;
            paths.sort();
            Ok(paths)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.to_string()),
    }
}

fn analyse(run: &Path, server_cpus: &[String], client_cpus: &[String]) -> Result<Json, String> {
    let meta = read(&run.join("meta.json"))?;
    let planned = number(get(&meta, "mbps"))?;
    let planned_float = planned.to_f64();
    let connections = number(get(&meta, "connections"))?;
    let aggregate_planned = match (planned, connections) {
        (Num::Int(a), Num::Int(b)) => {
            Json::Int(a.checked_mul(b).ok_or("campaign planned rate overflow")?)
        }
        _ => Json::Float(planned_float * connections.to_f64()),
    };
    let direction = text(get(&meta, "direction"))?;
    let mut fields = IndexMap::from([
        ("case".into(), get(&meta, "case").clone()),
        (
            "run".into(),
            Json::Str(
                run.file_name()
                    .ok_or("missing run name")?
                    .to_string_lossy()
                    .into(),
            ),
        ),
        ("direction".into(), Json::Str(direction.into())),
    ]);
    let (mut si_rates, mut means, mut over, mut short) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let (mut starts, mut finishes) = (Vec::new(), Vec::new());
    let (mut loss, mut skipped) = (Num::Int(0), Num::Int(0));
    let mut ends = IndexMap::<String, Json>::new();
    for path in paths(&run.join("clients"))?
        .into_iter()
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
    {
        let result = read(&path);
        let end = match &result {
            Ok(result) => get(get(result, "udpstp"), "EndReason")
                .as_str()
                .unwrap_or("?"),
            Err(_) => "no JSON",
        };
        let count = ends
            .get(end)
            .map(number)
            .transpose()?
            .unwrap_or(Num::Int(0))
            .py_add(Num::Int(1));
        ends.insert(end.into(), numeric(count));
        let Ok(result) = result else {
            continue;
        };
        let output = get(&result, "Output");
        if !output.truthy() {
            continue;
        }
        starts.push(utc::timestamp(text(get(output, "BOMTime"))?).map_err(|e| e.to_string())?);
        finishes.push(utc::timestamp(text(get(output, "EOMTime"))?).map_err(|e| e.to_string())?);
        let subs = match get(output, "IncrementalResult") {
            Json::Null => &[][..],
            value => list(value)?,
        };
        let rates = subs
            .iter()
            .map(|s| float(get(s, "IPLayerCapacity")))
            .collect::<Result<Vec<_>, _>>()?;
        for sub in subs {
            loss = loss.py_add(number(get(sub, "LossCount"))?);
        }
        if let Json::List(warnings) = get(get(&result, "udpstp"), "Warnings") {
            for warning in warnings {
                if matches!(
                    get(warning, "Code").as_str(),
                    Some("BurstsSkipped" | "BurstsWithheld")
                ) {
                    skipped = skipped.py_add(if get(warning, "Count").is_null() {
                        Num::Int(0)
                    } else {
                        number(get(warning, "Count"))?
                    });
                }
            }
        }
        if !rates.is_empty() {
            means.push(sum(&rates) / rates.len() as f64);
            over.push(maximum(&rates).unwrap() / planned_float - 1.0);
            short.push(1.0 - minimum(&rates).unwrap() / planned_float);
            si_rates.extend(rates);
        }
    }
    let shortfalls = si_rates
        .iter()
        .map(|r| 100.0 * (1.0 - r / planned_float).max(0.0))
        .collect::<Vec<_>>();
    let packet_bytes = if get(&meta, "packet_bytes").is_null() {
        1250.0
    } else {
        float(get(&meta, "packet_bytes"))?
    };
    let datagrams = planned_float * 1e6 / 8.0 / packet_bytes
        * float(get(&meta, "test_interval"))?
        * connections.to_f64();
    if !datagrams.is_finite() || datagrams <= 0.0 {
        return Err("invalid campaign rate or duration".into());
    }
    fields.extend([
        (
            "completed".into(),
            ends.get("Completed").cloned().unwrap_or(Json::Int(0)),
        ),
        ("ends".into(), Json::Dict(ends)),
        ("tc_mean_min".into(), opt(minimum(&means))),
        ("tc_mean_median".into(), opt(stats::median(&means))),
        (
            "aggregate".into(),
            if means.is_empty() {
                Json::Int(0)
            } else {
                Json::Float(sum(&means))
            },
        ),
        ("aggregate_planned".into(), aggregate_planned),
        ("over_pct".into(), opt(maximum(&over).map(|v| 100.0 * v))),
        (
            "short_worst_pct".into(),
            opt(maximum(&short).map(|v| 100.0 * v)),
        ),
        (
            "short_median_si_pct".into(),
            opt(stats::percentile(&shortfalls, 50.0)),
        ),
        (
            "short_p99_si_pct".into(),
            opt(stats::percentile(&shortfalls, 99.0)),
        ),
        ("sub_intervals".into(), Json::Int(si_rates.len() as i128)),
        (
            "sub_intervals_short_2pct".into(),
            Json::Int(shortfalls.iter().filter(|v| **v > 2.0).count() as i128),
        ),
        ("loss".into(), numeric(loss)),
        (
            "loss_pct".into(),
            Json::Float(100.0 * loss.to_f64() / datagrams),
        ),
    ]);
    let (mut logged, mut summed, mut dropped, mut early) =
        (Num::Int(0), Num::Int(0), Num::Int(0), 0);
    let (mut suppressed, mut sender_rates) = (IndexMap::new(), Vec::new());
    for line in fs::read_to_string(run.join("server.log"))
        .map_err(|e| e.to_string())?
        .lines()
    {
        let Ok(event) = Json::parse(line) else {
            continue;
        };
        let count = |field| {
            if get(&event, field).is_null() {
                Ok(Num::Int(0))
            } else {
                number(get(&event, field))
            }
        };
        match get(&event, "event").as_str() {
            Some("BurstsSkipped" | "BurstsWithheld") => logged = logged.py_add(count("datagrams")?),
            Some("Suppressed") => {
                let key = get(&event, "suppressed_event").as_str().unwrap_or("null");
                let previous = suppressed
                    .get(key)
                    .map(number)
                    .transpose()?
                    .unwrap_or(Num::Int(0));
                suppressed.insert(key.into(), numeric(previous.py_add(count("count")?)));
            }
            Some("DatagramsDropped") => dropped = dropped.py_add(count("count")?),
            Some("DataPhaseEnded") if get(&event, "reason").as_str() != Some("Completed") => {
                early += 1
            }
            Some("TestSummary") => {
                summed = summed
                    .py_add(count("skipped_datagrams")?)
                    .py_add(count("withheld_datagrams")?);
                if !get(&event, "ip_layer_sender_bit_rate").is_null() {
                    sender_rates.push(float(get(&event, "ip_layer_sender_bit_rate"))? / 1e6);
                }
            }
            _ => {}
        }
    }
    fields.extend([
        ("server_logged_not_sent".into(), numeric(logged)),
        ("server_not_sent".into(), numeric(summed)),
        ("server_suppressed_events".into(), Json::Dict(suppressed)),
        ("server_sender_rate_min".into(), opt(minimum(&sender_rates))),
        (
            "not_sent_pct".into(),
            if sender_rates.is_empty() {
                Json::Null
            } else {
                Json::Float(
                    100.0 * (1.0 - sum(&sender_rates) / (planned_float * connections.to_f64())),
                )
            },
        ),
        ("client_not_sent".into(), numeric(skipped)),
        (
            "client_not_sent_pct".into(),
            Json::Float(100.0 * skipped.to_f64() / datagrams),
        ),
        ("server_dropped".into(), numeric(dropped)),
        ("server_early_ends".into(), Json::Int(early)),
    ]);
    let samples = fs::read_to_string(run.join("samples.jsonl"))
        .map_err(|e| e.to_string())?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| Json::parse(line).map_err(|e| e.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    if !starts.is_empty() && !samples.is_empty() {
        let w0 = maximum(&starts).unwrap() + 1.0;
        let mut w1 = minimum(&finishes).unwrap() - 1.0;
        if run.join("pacing.start").exists() {
            let pacing = fs::read_to_string(run.join("pacing.start"))
                .map_err(|e| e.to_string())?
                .trim()
                .parse::<f64>()
                .map_err(|e| e.to_string())?;
            w1 = w1.min(pacing - 0.5);
        }
        let inside = samples
            .iter()
            .map(|s| Ok((s, float(get(s, "t"))?)))
            .collect::<Result<Vec<_>, String>>()?
            .into_iter()
            .filter(|(_, t)| *t >= w0 && *t <= w1)
            .map(|(s, _)| s)
            .collect::<Vec<_>>();
        add(
            &mut fields,
            "window_s",
            if inside.len() > 1 {
                Json::Float(float(get(inside.last().unwrap(), "t"))? - float(get(inside[0], "t"))?)
            } else {
                Json::Int(0)
            },
        );
        if inside.len() > 1 {
            let (a, b) = (inside[0], *inside.last().unwrap());
            let dt = float(get(b, "t"))? - float(get(a, "t"))?;
            add(
                &mut fields,
                "server_process_cpus",
                Json::Float(
                    (float(get(b, "process"))? - float(get(a, "process"))?)
                        / (bench_rounds::samples::USER_HZ as f64)
                        / dt,
                ),
            );
            for (prefix, cpus) in [("server", server_cpus), ("client", client_cpus)] {
                let (total, soft) = busy(a, b, cpus)?;
                add(
                    &mut fields,
                    &format!("{prefix}_cpus_busy"),
                    Json::Float(total),
                );
                add(&mut fields, &format!("{prefix}_softirq"), Json::Float(soft));
            }
            let (mut cpu, mut wake, mut other) = (Vec::new(), Vec::new(), 0.0);
            for (tid, thread) in get(b, "threads")
                .as_dict()
                .ok_or("missing sampled threads")?
            {
                let first = get(get(a, "threads"), tid);
                if first.is_null() {
                    continue;
                }
                let (thread, first) = (list(thread)?, list(first)?);
                if thread.len() < 4 || first.len() < 4 {
                    return Err("short thread record".into());
                }
                let used = (float(&thread[1])? - float(&first[1])?) / 100.0 / dt;
                if text(&thread[0])?.starts_with("udpstp-engine") {
                    cpu.push(100.0 * used);
                    wake.push((float(&thread[2])? - float(&first[2])?) / dt);
                } else {
                    other += used;
                }
            }
            fields.extend([
                ("engine_threads".into(), Json::Int(cpu.len() as i128)),
                (
                    "engine_cpu_pct_median".into(),
                    opt(stats::percentile(&cpu, 50.0)),
                ),
                ("engine_cpu_pct_max".into(), opt(maximum(&cpu))),
                ("engine_cpus_sum".into(), Json::Float(sum(&cpu) / 100.0)),
                (
                    "engine_wakeups_median".into(),
                    opt(stats::percentile(&wake, 50.0)),
                ),
                ("engine_wakeups_max".into(), opt(maximum(&wake))),
                (
                    "engine_wakeups_sum".into(),
                    if wake.is_empty() {
                        Json::Int(0)
                    } else {
                        Json::Float(sum(&wake))
                    },
                ),
                ("other_threads_cpus".into(), Json::Float(other)),
            ]);
        }
        for side in ["server", "clients"] {
            let name = format!("udp_{side}");
            let first = get(&samples[0], &name);
            let last = samples
                .iter()
                .rev()
                .map(|s| get(s, &name))
                .find(|s| s.truthy());
            if first.truthy()
                && let Some(last) = last
            {
                for (label, counter) in [("rcvbuf", "RcvbufErrors"), ("sndbuf", "SndbufErrors")] {
                    let (a, b) = (number(get(first, counter))?, number(get(last, counter))?);
                    add(
                        &mut fields,
                        &format!("{label}_{side}"),
                        match (a, b) {
                            (Num::Int(a), Num::Int(b)) => Json::Int(b - a),
                            _ => Json::Float(b.to_f64() - a.to_f64()),
                        },
                    );
                }
            }
        }
    }
    add(
        &mut fields,
        "pacing",
        if direction == "downstream" {
            pacing(
                &run.join("pacing.pcap"),
                if float(get(&meta, "row"))? < 100.0 {
                    1000.0
                } else {
                    100.0
                },
            )?
        } else {
            Json::Null
        },
    );
    Ok(Json::Dict(fields))
}

fn pacing(path: &Path, interval_us: f64) -> Result<Json, String> {
    let data = match fs::read(path) {
        Ok(data) => data,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Json::Null),
        Err(e) => return Err(e.to_string()),
    };
    if data.len() < 24 {
        return Ok(Json::Null);
    }
    let le = |bytes: &[u8]| u32::from_le_bytes(bytes.try_into().unwrap());
    let be = |bytes: &[u8]| u32::from_be_bytes(bytes.try_into().unwrap());
    let scale = if le(&data[..4]) == 0xa1b23c4d {
        1e-9
    } else {
        1e-6
    };
    let (mut flows, mut at) = (IndexMap::<u16, Vec<(f64, i64, f64)>>::new(), 24usize);
    while at + 16 <= data.len() {
        let (sec, frac, caplen) = (
            le(&data[at..at + 4]),
            le(&data[at + 4..at + 8]),
            le(&data[at + 8..at + 12]) as usize,
        );
        let begin = at + 16;
        let end = begin.saturating_add(caplen).min(data.len());
        at = begin.saturating_add(caplen);
        let Some(ip) = data[begin..end].get(14..) else {
            continue;
        };
        if ip.len() < 20 || ip[0] >> 4 != 4 || ip[9] != 17 {
            continue;
        }
        let Some(udp) = ip.get(usize::from(ip[0] & 15) * 4..) else {
            continue;
        };
        let Some(pdu) = udp.get(8..) else {
            continue;
        };
        if pdu.len() < 28 || pdu[..2] != [0xbe, 0xef] {
            continue;
        }
        let port = u16::from_be_bytes(udp[..2].try_into().unwrap());
        flows.entry(port).or_default().push((
            f64::from(sec) + f64::from(frac) * scale,
            i64::from(be(&pdu[4..8])),
            f64::from(be(&pdu[20..24])) + f64::from(be(&pdu[24..28])) * 1e-9,
        ));
    }
    let (mut late, mut departure, mut worst) = (Vec::new(), Vec::new(), 0.0f64);
    let interval = interval_us * 1e-6;
    for records in flows.values().filter(|r| r.len() >= 10) {
        let mut steps = records
            .windows(2)
            .map(|r| r[1].1 - r[0].1)
            .collect::<Vec<_>>();
        steps.sort();
        let burst = steps[steps.len() / 2].max(1);
        let (t0, s0) = (records[0].0, records[0].1);
        let offsets = records
            .iter()
            .map(|(t, s, _)| t - t0 - ((s - s0).div_euclid(burst) as f64) * interval)
            .collect::<Vec<_>>();
        let window = ((0.05 / interval) as usize).max(10);
        late.extend(offsets.iter().enumerate().map(|(index, o)| {
            (o - minimum(&offsets[index..offsets.len().min(index + window + 1)]).unwrap()) * 1e6
        }));
        let values = records
            .iter()
            .map(|(t, _, l)| (t - l) * 1e6)
            .collect::<Vec<_>>();
        worst = worst.max(stats::percentile(&values, 99.0).unwrap());
        departure.extend(values);
    }
    if late.is_empty() {
        return Ok(Json::Null);
    }
    let mut result = IndexMap::from([
        (
            "flows".into(),
            Json::Int(flows.values().filter(|r| r.len() >= 10).count() as i128),
        ),
        ("messages".into(), Json::Int(late.len() as i128)),
    ]);
    for (prefix, values) in [("departure", &departure), ("late", &late)] {
        for (name, p) in [("p50", 50.0), ("p99", 99.0), ("p999", 99.9)] {
            add(
                &mut result,
                &format!("{prefix}_{name}_us"),
                opt(stats::percentile(values, p)),
            );
        }
        add(
            &mut result,
            &format!("{prefix}_max_us"),
            opt(maximum(values)),
        );
        if prefix == "departure" {
            add(
                &mut result,
                "departure_worst_flow_p99_us",
                Json::Float(worst),
            );
        }
    }
    Ok(Json::Dict(result))
}

fn fmt(value: &Json, digits: usize) -> String {
    num::fmt(Num::from_json(value), digits, "")
}
fn table(out: &mut String, head: &[&str], rows: &[Vec<String>]) {
    out.push_str(&format!(
        "| {} |\n|{}\n",
        head.join(" | "),
        "---|".repeat(head.len())
    ));
    for row in rows {
        out.push_str(&format!("| {} |\n", row.join(" | ")));
    }
    out.push('\n');
}

pub fn report(directory: &Path, server_cpus: &str, client_cpus: &str) -> Result<String, String> {
    let (server_cpus, client_cpus) = (cpu_list(server_cpus)?, cpu_list(client_cpus)?);
    let mut results = Vec::new();
    for case in paths(directory)?.into_iter().filter(|p| p.is_dir()) {
        for run in paths(&case)?.into_iter().filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("run"))
        }) {
            results.push(analyse(&run, &server_cpus, &client_cpus)?);
        }
    }
    fs::write(
        directory.join("summary.json"),
        py::dumps_indent(&Json::List(results.clone()), 1),
    )
    .map_err(|error| error.to_string())?;
    let mut out="Rates and loss (SI: a Test Connection's Sub-Interval; shortfall and over-rate against the row)\n\n".to_string();
    let name = |r: &Json| {
        Ok::<_, String>(format!(
            "{} {}",
            text(get(r, "case"))?,
            text(get(r, "run"))?
        ))
    };
    let mut rows = Vec::new();
    for r in &results {
        let f = |key, digits| fmt(get(r, key), digits);
        let ends = get(r, "ends")
            .as_dict()
            .ok_or("missing end reasons")?
            .values()
            .map(float)
            .collect::<Result<Vec<_>, _>>()?;
        rows.push(vec![
            name(r)?,
            format!("{} of {}", f("completed", 1), sum(&ends) as i128),
            format!(
                "{} / {}",
                f("aggregate", 1),
                crate::scale::matrix::repr(get(r, "aggregate_planned"))
            ),
            f("tc_mean_min", 2),
            format!("{} %", f("over_pct", 2)),
            format!(
                "{} / {} / {} %",
                f("short_median_si_pct", 2),
                f("short_p99_si_pct", 2),
                f("short_worst_pct", 2)
            ),
            format!(
                "{} of {}",
                f("sub_intervals_short_2pct", 1),
                f("sub_intervals", 1)
            ),
            format!("{} ({} %)", f("loss", 1), f("loss_pct", 3)),
            if get(r, "not_sent_pct").is_null() {
                "–".into()
            } else {
                format!("{} %", f("not_sent_pct", 3))
            },
            format!(
                "{} ({} %)",
                f("client_not_sent", 1),
                f("client_not_sent_pct", 3)
            ),
            format!("{} / {}", f("rcvbuf_server", 1), f("rcvbuf_clients", 1)),
        ]);
    }
    table(&mut out, RATE_HEAD, &rows);
    out.push_str("CPU over the window in which every Test Connection ran (logical CPUs; wake-ups are voluntary context switches)\n\n");
    let mut rows = Vec::new();
    for r in &results {
        let f = |key, digits| fmt(get(r, key), digits);
        rows.push(vec![
            name(r)?,
            f("window_s", 1),
            format!("{} ({})", f("server_cpus_busy", 2), f("server_softirq", 2)),
            f("server_process_cpus", 2),
            f("engine_threads", 1),
            format!(
                "{} / {}",
                f("engine_cpu_pct_median", 1),
                f("engine_cpu_pct_max", 1)
            ),
            format!(
                "{} / {}",
                f("engine_wakeups_median", 0),
                f("engine_wakeups_max", 0)
            ),
            f("engine_wakeups_sum", 0),
            f("other_threads_cpus", 2),
            f("client_cpus_busy", 2),
        ]);
    }
    table(&mut out, CPU_HEAD, &rows);
    let mut rows = Vec::new();
    for r in results.iter().filter(|r| get(r, "pacing").truthy()) {
        let p = get(r, "pacing");
        let f = |key| fmt(get(p, key), 1);
        rows.push(vec![
            name(r)?,
            f("flows"),
            f("messages"),
            format!(
                "{} / {} / {} / {}",
                f("departure_p50_us"),
                f("departure_p99_us"),
                f("departure_p999_us"),
                f("departure_max_us")
            ),
            f("departure_worst_flow_p99_us"),
            format!(
                "{} / {} / {} / {}",
                f("late_p50_us"),
                f("late_p99_us"),
                f("late_p999_us"),
                f("late_max_us")
            ),
        ]);
    }
    if !rows.is_empty() {
        out.push_str("Pacing of the Server's Load PDUs, from 1 s captured at its veth (µs)\n\n");
        table(
            &mut out,
            &[
                "Run",
                "Flows",
                "Messages",
                "lpduTime to departure p50 / p99 / p99.9 / max",
                "Worst flow's p99",
                "Lateness to grid p50 / p99 / p99.9 / max",
            ],
            &rows,
        );
    }
    Ok(out)
}
