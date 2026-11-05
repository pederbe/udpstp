//! Receiver-based precision gates and ordinary Server sender diagnostics.

use std::collections::HashMap;

use bench_rounds::num::{self, Num};
use bench_rounds::py::Json;
use indexmap::IndexMap;

use super::judge::{Record, Unreadable};
use super::report::{self, RoundFiles};
use super::sender_evidence;

fn field<'a>(value: &'a Json, key: &str) -> Option<&'a Json> {
    value.as_dict()?.get(key)
}

fn read(files: &dyn RoundFiles, name: &str) -> Option<Json> {
    Json::parse(std::str::from_utf8(&files.read(name)?).ok()?).ok()
}

fn number(value: &Json) -> Option<f64> {
    let n = Num::from_json(value)?.to_f64();
    (n.is_finite() && n >= 0.0).then_some(n)
}

fn whole(value: &Json) -> Option<i128> {
    match value {
        Json::Int(n) if *n >= 0 => Some(*n),
        _ => None,
    }
}

fn median(values: &[f64]) -> f64 {
    num::median(&values.iter().copied().map(Num::Float).collect::<Vec<_>>())
        .expect("nonempty finite figures")
        .to_f64()
}

fn worst(values: &[f64]) -> f64 {
    values.iter().copied().fold(0.0, f64::max)
}

// : achieved, lag, absolute error, over-rate, not sent, median shortfall, worst shortfall.
struct PrecisionFigures {
    achieved: f64,
    lag: f64,
    error: f64,
    over: f64,
    not_sent: f64,
    short: f64,
    worst_short: f64,
}

impl PrecisionFigures {
    fn columns(&self) -> [f64; 7] {
        [
            self.achieved,
            self.lag,
            self.error,
            self.over,
            self.not_sent,
            self.short,
            self.worst_short,
        ]
    }
}

fn precision_figures(files: &dyn RoundFiles, run: &Record) -> Option<PrecisionFigures> {
    if run.status != Some(0) {
        return None;
    }
    let interval = run.test_interval.filter(|n| *n > 0)?;
    let rate = run.rate.parse::<i128>().ok()?;
    let result = read(files, &format!("tests/{}.json", run.label))?;
    let (input, output, ours) = (
        field(&result, "Input")?,
        field(&result, "Output")?,
        field(&result, "udpstp")?,
    );
    for (key, expected) in [
        ("TestType", Json::Str("Fixed".into())),
        ("SendingRateIndex", Json::Int(rate)),
        ("NumberOfConnections", Json::Int(1)),
        ("UDPPayloadDefault", Json::Int(1222)),
        ("NumberTestSubIntervals", Json::Int(interval)),
        ("TestSubInterval", Json::Int(1000)),
        (
            "Role",
            Json::Str(
                if run.direction == "upstream" {
                    "Sender"
                } else {
                    "Receiver"
                }
                .into(),
            ),
        ),
    ] {
        if field(input, key) != Some(&expected) {
            return None;
        }
    }
    if field(output, "Status") != Some(&Json::Str("Complete".into()))
        || field(output, "TestInterval") != Some(&Json::Int(interval))
        || field(ours, "EndReason") != Some(&Json::Str("Completed".into()))
        || field(ours, "MissingSubIntervals") != Some(&Json::List(Vec::new()))
    {
        return None;
    }
    let Json::List(subs) = field(output, "IncrementalResult")? else {
        return None;
    };
    if subs.len() as i128 != interval {
        return None;
    }
    let mut caps = Vec::new();
    for (i, sub) in subs.iter().enumerate() {
        if field(sub, "Interval") != Some(&Json::Int(i as i128 + 1)) {
            return None;
        }
        caps.push(number(field(sub, "IPLayerCapacity")?)?);
    }
    let rate = rate as f64;
    let achieved = caps.iter().sum::<f64>() / caps.len() as f64;
    let short: Vec<_> = caps
        .iter()
        .map(|c| (100.0 * (rate - c) / rate).max(0.0))
        .collect();
    let (not_sent, lag) = if run.direction == "downstream" {
        let server = sender_evidence::figures(files, run)?;
        (server.skipped.checked_add(server.withheld)?, server.lag?)
    } else {
        let stamps = field(ours, "KernelSendTimestamps")?;
        if whole(field(stamps, "Samples")?)? == 0 {
            return None;
        }
        let lag = number(field(stamps, "LagP99Us")?)?;
        let Json::List(warnings) = field(ours, "Warnings")? else {
            return None;
        };
        let mut not_sent = 0_i128;
        for warning in warnings {
            warning.as_dict()?;
            if matches!(field(warning, "Code"), Some(Json::Str(s)) if s == "BurstsSkipped" || s == "BurstsWithheld")
            {
                not_sent = not_sent.checked_add(whole(field(warning, "Count")?)?)?;
            }
        }
        (not_sent, lag)
    };
    let over = caps
        .iter()
        .map(|c| (100.0 * (c - rate) / rate).max(0.0))
        .fold(0.0, f64::max);
    let figures = PrecisionFigures {
        achieved,
        lag,
        error: (100.0 * (achieved - rate) / rate).abs(),
        over,
        not_sent: 100.0 * not_sent as f64 / (rate * 100.0 * interval as f64),
        short: median(&short),
        worst_short: worst(&short),
    };
    figures
        .columns()
        .iter()
        .all(|n| n.is_finite())
        .then_some(figures)
}

pub(super) fn metric_lines(
    files: &dyn RoundFiles,
    runs: &[Record],
    labels: &HashMap<&str, usize>,
    listing: Option<&[String]>,
) -> Result<Vec<String>, Unreadable> {
    let mut lines = Vec::new();
    let mut groups: IndexMap<(&str, &str, &str, Option<std::num::NonZeroUsize>), Vec<&Record>> =
        IndexMap::new();
    for run in runs {
        if run.host_samples
            && run.kernel_send_timestamps
            && run.client == "udpstp"
            && run.server == "udpstp"
            && run.test_connections == 1
            && ["100", "500", "1000"].contains(&run.rate.as_str())
        {
            groups
                .entry((
                    &run.name,
                    &run.direction,
                    &run.rate,
                    run.server_engine_threads,
                ))
                .or_default()
                .push(run);
        }
    }
    if !groups.is_empty() {
        lines.extend(["", "## Precision gates", "",
            "Receiver IP-Layer Capacity is used without sender-clock substitution or adjacent-interval leniency. All scheduled non-void Round Runs must have valid evidence; platform-limited Fixed-Rate Round Runs are void. Rate error is absolute before taking its median. Shortfall uses each Round Run's worst Sub-Interval shortfall, then the median across Round Runs, with zero shortfall where the receiver meets or exceeds the planned rate. Worst Sub-Interval shortfall is also shown. Medians average the middle pair.", "",
            "| Test | Direction | Row | Evidence/expected | Voided | Send lag ≤100 µs | Rate error ≤1% | Over-rate ≤1% | Skipped/Withheld ≤1% | Worst Sub-Interval shortfall ≤2% |",
            "|---|---|---:|---:|---:|---|---|---|---|---|"].map(String::from));
        let mut figures_rows = Vec::new();
        for ((_, direction, rate, _), group) in groups {
            let name = group[0].test_name();
            let (mut valid, mut void) = (Vec::new(), 0);
            for run in &group {
                if !report::platform_limits(files, listing, &run.label)?.is_empty() {
                    void += 1;
                    continue;
                }
                if labels.get(run.label.as_str()) == Some(&1)
                    && let Some(figures) = precision_figures(files, run)
                {
                    valid.push(figures);
                }
            }
            let expected = group.len() - void;
            let complete = !valid.is_empty()
                && valid.len() == expected
                && files.read("first-plan.json").is_some();
            let gates: Vec<_> = [
                (
                    valid.iter().map(|f| f.lag).collect::<Vec<_>>(),
                    100.0,
                    false,
                ),
                (valid.iter().map(|f| f.error).collect(), 1.0, false),
                (valid.iter().map(|f| f.over).collect(), 1.0, true),
                (valid.iter().map(|f| f.not_sent).collect(), 1.0, false),
                (valid.iter().map(|f| f.worst_short).collect(), 2.0, false),
            ]
            .iter()
            .map(|(values, cap, use_worst)| {
                if !complete {
                    return "unavailable";
                }
                let judged = if *use_worst {
                    worst(values)
                } else {
                    median(values)
                };
                if judged <= *cap { "met" } else { "not met" }
            })
            .collect();
            lines.push(format!(
                "| {name} | {direction} | {rate} | {}/{expected} | {void} | {} |",
                valid.len(),
                gates.join(" | ")
            ));
            let cells: Vec<_> = (0..7)
                .map(|key| {
                    let values: Vec<_> = valid.iter().map(|f| f.columns()[key]).collect();
                    if values.is_empty() {
                        "unavailable".into()
                    } else {
                        format!(
                            "{} / {}",
                            num::fixed(median(&values), 2),
                            num::fixed(worst(&values), 2)
                        )
                    }
                })
                .collect();
            figures_rows.push(format!("| {name} | {direction} | {} |", cells.join(" | ")));
        }
        lines.extend(["", "Median / worst over valid non-void Round Runs; achieved rate in Mbit/s, lag in µs, others in percent. Worst achieved rate is the highest, while absolute error shows either direction of deviation.", "",
            "| Test | Direction | Achieved rate | Send lag | Absolute rate error | Over-rate | Skipped/Withheld | Median shortfall | Worst Sub-Interval shortfall |",
            "|---|---|---:|---:|---:|---:|---:|---:|---:|"].map(String::from));
        lines.extend(figures_rows);
    }
    let multi: Vec<_> = runs
        .iter()
        .filter(|r| {
            r.host_samples
                && r.server == "udpstp"
                && r.direction == "downstream"
                && r.test_connections > 1
        })
        .collect();
    if !multi.is_empty() {
        lines.extend(["", "## Multi-Connection Server diagnostics", "",
            "Advisory Downstream sender figures from ordinary Server logs. Burst lateness is the median and worst of connection p99s, not pooled percentiles.", "",
            "| Round Run | Skipped Load PDUs | Withheld Load PDUs | Burst lateness median p99 µs | Worst connection p99 µs |",
            "|---|---:|---:|---:|---:|"].map(String::from));
        for run in multi {
            let found = (run.status == Some(0) && labels.get(run.label.as_str()) == Some(&1))
                .then(|| sender_evidence::figures(files, run))
                .flatten();
            let cells = found.map_or_else(
                || vec!["unavailable".to_string(); 4],
                |f| {
                    vec![
                        f.skipped.to_string(),
                        f.withheld.to_string(),
                        num::fixed(f.lateness, 2),
                        num::fixed(f.worst_lateness, 2),
                    ]
                },
            );
            lines.push(format!("| {} | {} |", run.label, cells.join(" | ")));
        }
    }
    Ok(lines)
}
