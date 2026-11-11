//! Figures taken from engine results, before the Round's validity rules.

use super::*;

pub fn server_receive_drops(drops: &Json, nic: &str) -> Result<Num, Error> {
    let udp = num::max(&[
        number(drops.get_or("RcvbufErrors", &Json::Int(0))?)?,
        number(drops.get_or("InErrors", &Json::Int(0))?)?,
    ])
    .unwrap();
    Ok(udp
        .py_add(number(drops.get_or("softnet_dropped", &Json::Int(0))?)?)
        .py_add(number(drops.get_or(nic, &Json::Int(0))?)?))
}
pub fn server_accounts(record: &Json, drops: &Json, sends: bool, nic: &str) -> Result<Num, Error> {
    if sends {
        Ok(num::sum(
            &SERVER_SEND_DROPS
                .iter()
                .map(|k| number(drops.get_or(k, &Json::Int(0))?))
                .collect::<Result<Vec<_>, Error>>()?,
        ))
    } else {
        Ok(num::max(&[
            server_receive_drops(drops, nic)?,
            zero(record, "server_overflows")?,
        ])
        .unwrap())
    }
}
pub fn planned_datagrams(meta: &Json, count: Option<Num>) -> Result<f64, Error> {
    let count = match count {
        Some(count) => count,
        None => number(meta.item("count")?)?,
    };
    if is_zero(Some(number(meta.item("packet_bytes")?)?)) {
        return Err(Error("division by zero".into()));
    }
    Ok((number(meta.item("mbps")?)?.to_f64() * 1e6
        / 8.0
        / number(meta.item("packet_bytes")?)?.to_f64())
        * number(meta.item("test_interval")?)?.to_f64()
        * count.to_f64())
}

pub fn counted_over(sis: &[Json], mbps: f64) -> Result<Option<f64>, Error> {
    for s in sis.iter().filter(|s| !s.is_null()) {
        if list(s)?.len() != 2 {
            return Err(Error(
                "a Sub-Interval must hold its rate and leniency".into(),
            ));
        }
        if mbps == 0.0 {
            return Err(Error("division by zero".into()));
        }
    }
    let values = sis
        .iter()
        .map(|s| {
            if s.is_null() {
                Ok(None)
            } else {
                Ok(Some(number(s.at(0)?)?.to_f64()))
            }
        })
        .collect::<Result<Vec<_>, Error>>()?;
    let mut worst: Option<f64> = None;
    for (i, s) in sis.iter().enumerate() {
        let Some(rate) = values[i] else {
            continue;
        };
        let mut over = 100.0 * (rate / mbps - 1.0);
        if s.at(1)?.truthy() && over > P2A_OVER {
            let beside = [
                i.checked_sub(1),
                i.checked_add(1).filter(|j| *j < values.len()),
            ];
            if beside
                .into_iter()
                .flatten()
                .filter_map(|j| values[j])
                .any(|n| mbps - n >= rate - mbps * (1.0 + P2A_OVER / 100.0))
            {
                over = P2A_OVER;
            }
        }
        worst = Some(worst.map_or(over, |w| num::max2(w, over)));
    }
    // The sum keeps the input kinds until division, as Python's sum does.
    let present = sis
        .iter()
        .filter(|s| !s.is_null())
        .map(|s| number(s.at(0)?))
        .collect::<Result<Vec<_>, Error>>()?;
    if !present.is_empty() {
        let whole = 100.0 * (num::sum(&present).to_f64() / (mbps * present.len() as f64) - 1.0);
        worst = Some(num::max2(worst.unwrap(), whole));
    }
    Ok(worst)
}

pub fn rates_figures(tcs: &[Json], mbps: f64, expected: i64, count: i64) -> Result<Json, Error> {
    if count != 0 && (mbps == 0.0 || expected == 0) {
        return Err(Error("division by zero".into()));
    }
    let mut shortfalls = Vec::new();
    let mut completed_shortfalls = Vec::new();
    let mut overs = Vec::new();
    let mut raw = Vec::new();
    let mut present = 0i128;
    let mut missing = 0i128;
    let mut delivered = 0.0;
    for tc in tcs {
        if list(tc)?.len() != 3 {
            return Err(Error(
                "a Test Connection needs judged and received rates and completion".into(),
            ));
        }
        let judged = list(tc.at(0)?)?;
        let received = list(tc.at(1)?)?;
        let completed = tc.at(2)?.truthy();
        for s in judged {
            let short = if s.is_null() {
                100.0
            } else {
                100.0 * num::max2(0.0, 1.0 - number(s.at(0)?)?.to_f64() / mbps)
            };
            shortfalls.push(short);
            if completed && !s.is_null() {
                completed_shortfalls.push(short);
            }
        }
        if judged.iter().any(Json::truthy)
            && let Some(over) = counted_over(judged, mbps)?
        {
            overs.push(Num::Float(over));
        }
        let found = judged.iter().filter(|s| !s.is_null()).count() as i128;
        present += found;
        if completed {
            missing += i128::from(expected) - found;
        }
        let received = received
            .iter()
            .filter_map(|r| numeric(r).transpose())
            .collect::<Result<Vec<_>, _>>()?;
        if mbps == 0.0 && !received.is_empty() {
            return Err(Error("division by zero".into()));
        }
        raw.extend(
            received
                .iter()
                .map(|r| Num::Float(100.0 * (r.to_f64() / mbps - 1.0))),
        );
        delivered += num::sum(&received).to_f64();
    }
    let percentile = |xs: &[f64], p| {
        bench_rounds::stats::percentile(xs, p)
            .map(Json::Float)
            .unwrap_or(Json::Null)
    };
    Ok(dict([
        ("over_pct", optional_num(num::max(&overs))),
        ("over_raw_pct", optional_num(num::max(&raw))),
        ("short_median_pct", percentile(&shortfalls, 50.0)),
        ("short_p99_pct", percentile(&shortfalls, 99.0)),
        (
            "short_worst_pct",
            optional_num(num::max(
                &shortfalls
                    .iter()
                    .copied()
                    .map(Num::Float)
                    .collect::<Vec<_>>(),
            )),
        ),
        (
            "short_completed_median_pct",
            percentile(&completed_shortfalls, 50.0),
        ),
        ("sub_intervals", Json::Int(present)),
        ("missing_sub_intervals", Json::Int(missing)),
        (
            "delivered_pct",
            if count == 0 {
                Json::Null
            } else {
                Json::Float(100.0 * delivered / (mbps * count as f64 * expected as f64))
            },
        ),
    ]))
}

pub fn unanswered(result: &Json) -> Result<Json, Error> {
    let ours = result.get("udpstp")?.or(&EMPTY_DICT);
    if ours.get("EndReason")?.as_str() != Some("ControlPhaseFailed")
        || ours.get("ControlPhaseFailure")?.as_str() != Some("TestInitiationTimedOut")
        || !eq(result.get("ErrorStatus")?, &Json::Int(200))
    {
        return Ok(Json::Null);
    }
    let mut messages: Vec<_> = result
        .as_dict()
        .ok_or_else(|| Error("expected a result dict".into()))?
        .iter()
        .filter(|(k, _)| k.starts_with("ErrorMessage"))
        .collect();
    messages.sort_by_key(|(k, _)| *k);
    let said = messages
        .into_iter()
        .map(|(_, v)| display(v))
        .collect::<Vec<_>>()
        .join(" ");
    Ok(string(if said.contains("Activation exchange") {
        "Activation"
    } else {
        "Setup"
    }))
}
pub fn silent_rejections(events: &[Json]) -> Result<Num, Error> {
    let mut logged = Num::Int(0);
    let mut suppressed = Vec::new();
    let mut reports = Vec::new();
    for e in events {
        let event = e.get("event")?.as_str();
        if event.is_some_and(|s| SILENT_REJECTIONS.contains(&s)) {
            logged = logged.py_add(Num::Int(1));
        }
        if event == Some("Suppressed")
            && e.get("suppressed_event")?
                .as_str()
                .is_some_and(|s| SILENT_REJECTIONS.contains(&s))
        {
            suppressed.push(number(e.get_or("count", &Json::Int(0))?)?);
        }
        if event == Some("StatisticsRefusals") {
            reports.push(e);
        }
    }
    logged = logged.py_add(num::sum(&suppressed));
    let last = num::max(
        &reports
            .iter()
            .map(|e| number(e.get_or("report", &Json::Int(0))?))
            .collect::<Result<Vec<_>, Error>>()?,
    );
    let mut counted = Vec::new();
    for e in reports {
        let same_report = match (numeric(e.get("report")?)?, last) {
            (Some(a), Some(b)) => a.py_cmp(b) == Some(Ordering::Equal),
            (None, None) => true,
            _ => false,
        };
        if same_report
            && e.get("kind")?
                .as_str()
                .is_some_and(|s| SILENT_REJECTIONS.contains(&s))
        {
            counted.push(number(e.get_or("count", &Json::Int(0))?)?);
        }
    }
    Ok(num::max(&[logged, num::sum(&counted)]).unwrap())
}
pub fn engine_ends(events: &[Json]) -> Result<Json, Error> {
    let mut pooled = Vec::new();
    let mut lag = Vec::new();
    let mut lateness = Vec::new();
    for e in events {
        if e.get("event")?.as_str() != Some(DATA_PHASE_ENDED) {
            continue;
        }
        let paths = e.get("fast_paths")?;
        let paths = if paths.truthy() { text(paths)? } else { "" };
        let paths = paths
            .split(',')
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>();
        if !paths.is_empty() {
            pooled.push(paths.contains(&POOL_FAST_PATH));
        }
        for (key, figures) in [
            (SEND_LAG_P99, &mut lag),
            (BURST_LATENESS_P99, &mut lateness),
        ] {
            if let Some(value) = Num::from_json(e.get(key)?) {
                figures.push(value);
            }
        }
    }
    let ran = if pooled.is_empty() {
        Json::Null
    } else {
        string(if pooled.iter().all(|b| *b) {
            "pool"
        } else if pooled.iter().all(|b| !b) {
            "per-test-connection"
        } else {
            "both"
        })
    };
    Ok(Json::List(vec![
        ran,
        optional_num(num::max(&lag)),
        optional_num(num::max(&lateness)),
    ]))
}
pub fn engine_startups(events: &[Json]) -> Result<Json, Error> {
    let mut found = Vec::new();
    for e in events {
        if e.get("event")?.as_str() != Some(DATA_PHASE_ENGINES) {
            continue;
        }
        if matches!(e.get("executor")?, Json::List(_) | Json::Dict(_)) {
            return Err(Error("an executor name is unhashable".into()));
        }
        let executor = match e.get("executor")?.as_str() {
            Some("per_test_connection") => string("per-test-connection"),
            _ => e.get("executor")?.clone(),
        };
        found.push(dict([
            ("executor", executor),
            ("engine_threads", e.get("engine_threads")?.clone()),
            // Current Servers omit the retired setting. Historical events
            // still provide their actual window.
            (
                "grouping_us",
                e.get_or("grouping_us", &Json::Int(0))?.clone(),
            ),
        ]));
    }
    Ok(Json::List(found))
}
