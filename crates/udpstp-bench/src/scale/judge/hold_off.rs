use bench_rounds::samples;

use super::*;

/// `/proc/softirqs` and `/proc/interrupts`, in the order the files name
/// their rows and logical CPUs. Numeric keys use their decimal spelling.
pub fn interrupt_counts(source: &str) -> Result<Json, Error> {
    let mut counts = IndexMap::new();
    let mut cpus: Option<Vec<i128>> = None;
    for line in py::splitlines(source) {
        let words = py::split(line);
        if line.starts_with("---") {
            cpus = None;
        } else if !words.is_empty()
            && words.iter().all(|w| {
                w.starts_with("CPU") && !w[3..].is_empty() && w[3..].chars().all(char::is_numeric)
            })
        {
            cpus = Some(
                words
                    .iter()
                    .map(|w| {
                        w[3..]
                            .parse()
                            .map_err(|_| Error("invalid interrupt CPU number".into()))
                    })
                    .collect::<Result<_, _>>()?,
            );
        } else if !words.is_empty()
            && words[0].ends_with(':')
            && let Some(cpus) = &cpus
        {
            let mut values = IndexMap::new();
            for (cpu, word) in cpus.iter().zip(&words[1..]) {
                if word.is_empty() || !word.chars().all(char::is_numeric) {
                    break;
                }
                let value = word
                    .parse::<i128>()
                    .map_err(|_| Error("invalid interrupt count".into()))?;
                values.insert(cpu.to_string(), Json::Int(value));
            }
            if !values.is_empty() {
                counts.insert(words[0][..words[0].len() - 1].into(), Json::Dict(values));
            }
        }
    }
    Ok(Json::Dict(counts))
}
fn difference(a: &Json, b: &Json) -> Result<Num, Error> {
    sub(number(a)?, number(b)?)
}
fn divide(a: Num, b: Num) -> Result<f64, Error> {
    a.py_div(b)
        .ok_or_else(|| Error("invalid or out-of-range divisor".into()))
}
fn sum_row(row: &Json) -> Result<Num, Error> {
    let row = row
        .as_dict()
        .ok_or_else(|| Error("invalid interrupt row".into()))?;
    Ok(num::sum(
        &row.values().map(number).collect::<Result<Vec<_>, _>>()?,
    ))
}

/// The Server's CPU and wake-ups per received Load PDU, and its host's
/// interrupt rises. `run` already contains the evidence for this record.
pub fn hold_off_run(run: &RunFiles, record: &Json, nic_irqs: &[String]) -> Result<Json, Error> {
    let span = record.get("window")?;
    if !span.truthy()
        || !record.get("planned_pdus")?.truthy()
        || !record.get("delivered_pct")?.truthy()
        || !record.get("test_interval")?.truthy()
    {
        return Ok(Json::Null);
    }
    let span = list(span)?;
    if span.len() != 2 {
        return Err(Error("invalid run window".into()));
    }
    let (t0, t1) = (number(&span[0])?.to_f64(), number(&span[1])?.to_f64());
    let found = run.lines("server/samples.jsonl")?;
    let pair = samples::window(&found, t0, t1)?;
    let use_ = samples::cpu_use(&found, t0, t1, None)?;
    let (Some((first, last)), Some(use_)) = (pair, use_) else {
        return Ok(Json::Null);
    };
    if !first.as_dict().is_some_and(|d| d.contains_key("process"))
        || !last.as_dict().is_some_and(|d| d.contains_key("process"))
    {
        return Ok(Json::Null);
    }
    let per_s = divide(
        number(record.item("planned_pdus")?)?,
        number(record.item("test_interval")?)?,
    )? * number(record.item("delivered_pct")?)?.to_f64()
        / 100.0;
    if number(last.item("t")?)?.py_cmp(number(first.item("t")?)?) != Some(Ordering::Greater)
        || per_s <= 0.0
    {
        return Ok(Json::Null);
    }
    let process = divide(
        difference(last.item("process")?, first.item("process")?)?,
        Num::Int(samples::USER_HZ),
    )? / difference(last.item("t")?, first.item("t")?)?.to_f64();
    let before = interrupt_counts(run.text("interrupts.server.before.txt")?.unwrap_or(""))?;
    let after = interrupt_counts(run.text("interrupts.server.after.txt")?.unwrap_or(""))?;
    let thousands = per_s * number(record.item("test_interval")?)?.to_f64() / 1000.0;
    if thousands == 0.0 {
        return Err(Error("zero received Load PDUs".into()));
    }
    let rows = [
        ("NET_RX", vec!["NET_RX".into()]),
        ("LOC", vec!["LOC".into()]),
        ("RES", vec!["RES".into()]),
        ("CAL", vec!["CAL".into()]),
        ("NIC", nic_irqs.to_vec()),
    ];
    let rise = |names: &[String], cpu: Option<&str>| -> Result<Json, Error> {
        if names.is_empty() {
            return Ok(Json::Null);
        }
        let mut found = Vec::new();
        for name in names {
            if !before.as_dict().is_some_and(|d| d.contains_key(name))
                || !after.as_dict().is_some_and(|d| d.contains_key(name))
            {
                return Ok(Json::Null);
            }
            let (a, b) = (after.item(name)?, before.item(name)?);
            found.push(if let Some(cpu) = cpu {
                difference(a.get_or(cpu, &Json::Int(0))?, b.get_or(cpu, &Json::Int(0))?)?
            } else {
                sub(sum_row(a)?, sum_row(b)?)?
            });
        }
        Ok(Json::Float(num::sum(&found).to_f64() / thousands))
    };
    let mut by_cpu = IndexMap::new();
    let cpus = first
        .item("cpus")?
        .as_dict()
        .ok_or_else(|| Error("invalid CPU samples".into()))?;
    for (cpu, a) in cpus {
        let b = last.item("cpus")?.get(cpu)?;
        if !b.truthy() || cpu.is_empty() || !cpu.chars().all(char::is_numeric) {
            continue;
        }
        let cpu = cpu
            .parse::<i128>()
            .map_err(|_| Error("invalid logical CPU".into()))?
            .to_string();
        let d = list(a)?
            .iter()
            .zip(list(b)?)
            .map(|(a, b)| difference(b, a))
            .collect::<Result<Vec<_>, _>>()?;
        let sum = num::sum(&d);
        if sum.py_cmp(Num::Int(0)) == Some(Ordering::Equal) {
            continue;
        }
        if d.len() < 7 {
            return Err(Error("incomplete CPU counters".into()));
        }
        let mut row = IndexMap::from([
            (
                "busy".into(),
                Json::Float(divide(sub(sub(sum, d[3])?, d[4])?, sum)?),
            ),
            ("softirq".into(), Json::Float(divide(d[6], sum)?)),
        ]);
        for (name, names) in &rows {
            row.insert((*name).into(), rise(names, Some(&cpu))?);
        }
        by_cpu.insert(cpu, Json::Dict(row));
    }
    let woke = n(
        record.get("server_threads")?.or(&EMPTY_DICT),
        "wakeups_per_s",
    )?;
    let host = rows
        .iter()
        .map(|(name, names)| Ok(((*name).into(), rise(names, None)?)))
        .collect::<Result<_, Error>>()?;
    Ok(dict([
        (
            "cpu_per_pdu_us",
            Json::Float(1e6 * (process + use_.softirq + use_.irq) / per_s),
        ),
        (
            "wakeups_per_pdu",
            woke.map(|v| Json::Float(v.to_f64() / per_s))
                .unwrap_or(Json::Null),
        ),
        ("by_cpu", Json::Dict(by_cpu)),
        ("host", Json::Dict(host)),
    ]))
}
