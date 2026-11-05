//! Server-scale judgments, without host I/O. They preserve the frozen
//! `scale_report.py` rules and the order and number kinds of its records.

mod figures;
pub use figures::*;
mod files;
pub use files::{RunFiles, analyse, tooling_record};
mod experiments;
pub use experiments::*;
mod stats;
pub use stats::lower_p;
mod hold_off;
pub use hold_off::{hold_off_run, interrupt_counts};

use std::cmp::Ordering;

use bench_rounds::num::{self, Num};
use bench_rounds::py::{self, EMPTY_DICT, EMPTY_LIST, Json};
use indexmap::IndexMap;

use super::Error;
use super::matrix::{EXECUTOR_ENV, GROUPING_ENV, STAMPS_ENV, dict, integer, list, string, text};
use super::rules::*;

pub type PointKey = (String, String, String, i64, i64, String);
pub type Verdicts = IndexMap<PointKey, Json>;
pub type Outcomes = (Vec<Json>, Vec<(Json, Vec<Json>)>);

pub(crate) fn number(value: &Json) -> Result<Num, Error> {
    Num::from_json(value)
        .ok_or_else(|| Error(format!("expected a number, not {}", value.type_name())))
}
pub(crate) fn numeric(value: &Json) -> Result<Option<Num>, Error> {
    if value.is_null() {
        Ok(None)
    } else {
        number(value).map(Some)
    }
}
pub(crate) fn num_json(value: Num) -> Json {
    match value {
        Num::Int(n) => Json::Int(n),
        Num::Float(n) => Json::Float(n),
    }
}
pub(crate) fn optional_num(value: Option<Num>) -> Json {
    value.map(num_json).unwrap_or(Json::Null)
}
pub(crate) fn strings(values: impl IntoIterator<Item = String>) -> Json {
    Json::List(values.into_iter().map(Json::Str).collect())
}
pub(crate) fn set(value: &mut Json, key: &str, item: Json) -> Result<(), Error> {
    let Json::Dict(value) = value else {
        return Err(Error("expected a dict".into()));
    };
    value.insert(key.into(), item);
    Ok(())
}
pub(crate) fn extend(value: &mut Json, other: Json) -> Result<(), Error> {
    let Json::Dict(other) = other else {
        return Err(Error("expected a dict".into()));
    };
    for (key, item) in other {
        set(value, &key, item)?;
    }
    Ok(())
}
pub(crate) fn array<'a>(value: &'a Json, key: &str) -> Result<&'a [Json], Error> {
    list(value.get(key)?.or(&EMPTY_LIST))
}
fn default_array<'a>(value: &'a Json, key: &str) -> Result<&'a [Json], Error> {
    list(value.get_or(key, &EMPTY_LIST)?)
}
pub(crate) fn n(value: &Json, key: &str) -> Result<Option<Num>, Error> {
    numeric(value.get(key)?)
}
pub(crate) fn zero(value: &Json, key: &str) -> Result<Num, Error> {
    number(value.get(key)?.or(&Json::Int(0)))
}
pub(crate) fn eq(a: &Json, b: &Json) -> bool {
    match (Num::from_json(a), Num::from_json(b)) {
        (Some(a), Some(b)) => a.py_cmp(b) == Some(Ordering::Equal),
        _ => a == b,
    }
}
pub(crate) fn le(value: Option<Num>, limit: f64) -> bool {
    value.is_some_and(|v| {
        matches!(
            v.py_cmp(Num::Float(limit)),
            Some(Ordering::Less | Ordering::Equal)
        )
    })
}
pub(crate) fn gt(value: Option<Num>, limit: f64) -> bool {
    value.is_some_and(|v| v.py_cmp(Num::Float(limit)) == Some(Ordering::Greater))
}
fn is_zero(value: Option<Num>) -> bool {
    value.is_some_and(|v| v.py_cmp(Num::Int(0)) == Some(Ordering::Equal))
}
fn values(runs: &[&Json], key: &str) -> Result<Vec<Num>, Error> {
    runs.iter().filter_map(|r| n(r, key).transpose()).collect()
}
fn med(runs: &[&Json], key: &str) -> Result<Json, Error> {
    Ok(optional_num(num::median(&values(runs, key)?)))
}
fn maximum(runs: &[&Json], key: &str) -> Result<Json, Error> {
    Ok(optional_num(num::max(&values(runs, key)?)))
}
fn total(runs: &[&Json], key: &str) -> Result<Json, Error> {
    Ok(num_json(num::sum(
        &runs
            .iter()
            .map(|r| zero(r, key))
            .collect::<Result<Vec<_>, _>>()?,
    )))
}
fn med_nested(runs: &[&Json], parent: &str, key: &str) -> Result<Json, Error> {
    let values = runs
        .iter()
        .map(|r| r.get(parent).map(|v| v.or(&EMPTY_DICT)))
        .collect::<Result<Vec<_>, _>>()?;
    med(&values, key)
}
pub(crate) fn sub(a: Num, b: Num) -> Result<Num, Error> {
    match (a, b) {
        (Num::Int(a), Num::Int(b)) => a
            .checked_sub(b)
            .map(Num::Int)
            .ok_or_else(|| Error("integer out of range".into())),
        _ => Ok(Num::Float(a.to_f64() - b.to_f64())),
    }
}

pub fn incomplete(record: &Json) -> Result<Option<Num>, Error> {
    match (n(record, "count")?, n(record, "completed")?) {
        (Some(count), Some(done)) => Ok(Some(sub(sub(count, zero(record, "left_out")?)?, done)?)),
        _ => Ok(None),
    }
}

pub fn only_the_path(record: &Json) -> Result<bool, Error> {
    let reasons = list(record.item("invalid")?)?;
    let reasons = reasons.iter().map(text).collect::<Result<Vec<_>, _>>()?;
    Ok(!reasons.is_empty()
        && reasons
            .iter()
            .all(|r| r.starts_with(PATH_LOST) || r.starts_with(PATH_LEFT_OUT)))
}

pub fn path_loss_text(record: &Json) -> Result<Json, Error> {
    let Some(loss) = n(record, "path_loss")? else {
        return Ok(Json::Null);
    };
    let planned = record.get("planned_pdus")?;
    if !planned.truthy() {
        return Ok(Json::Null);
    }
    let rate = 1e6 * loss.to_f64() / number(planned)?.to_f64();
    Ok(string(&format!(
        "the path lost {} of about {} Load PDUs planned ({} a million)",
        display(&num_json(loss)),
        display(planned),
        num::fixed(rate, 2)
    )))
}

pub(crate) fn display(value: &Json) -> String {
    match value {
        Json::Null => "None".into(),
        Json::Bool(true) => "True".into(),
        Json::Bool(false) => "False".into(),
        Json::Str(s) => s.clone(),
        Json::Int(n) => n.to_string(),
        Json::Float(n) => num::repr(*n),
        other => py::dumps(other),
    }
}

pub fn meets(record: &Json) -> Result<bool, Error> {
    if record.get("kind")?.as_str() == Some("p1") {
        return Ok(is_zero(incomplete(record)?)
            && le(n(record, "p1_p99_us")?, P1_P99 as f64)
            && (record.get("p1_p999_us")?.is_null()
                || le(n(record, "p1_p999_us")?, P1_P999 as f64))
            && le(n(record, "p1_worst_us")?, P1_WORST as f64));
    }
    Ok(is_zero(incomplete(record)?)
        && is_zero(n(record, "missing_sub_intervals")?)
        && (record.get("not_sent_pct")?.is_null() || le(n(record, "not_sent_pct")?, P2B_NOT_SENT))
        && le(n(record, "short_median_pct")?, P2C_SHORT)
        && le(n(record, "over_pct")?, P2A_OVER)
        && le(n(record, "engine_loss")?, LOSS as f64))
}

pub fn judged_like(plan: &Json, case: &Json) -> Result<bool, Error> {
    Ok(case.get_or("judged", &Json::Bool(true))?.truthy()
        || eq(
            case.item("name")?,
            plan.get("grouping")?.or(&EMPTY_DICT).get("arm")?,
        ))
}

pub fn runs_for(plan: &Json, case: &Json, count: i64, engine: &str) -> Result<i64, Error> {
    let elsewhere =
        integer(plan.get_or("runs_elsewhere", plan.get_or("repeats", &Json::Int(1))?)?)?;
    let at_target = judged_like(plan, case)? && integer(case.item("target")?)? == count;
    if case
        .as_dict()
        .ok_or_else(|| Error("expected a case dict".into()))?
        .contains_key("engines")
        && !list(case.item("engines")?)?.contains(&string(engine))
    {
        return Ok(0);
    }
    let target_runs = || {
        integer(
            plan.get_or("runs_at_target", &EMPTY_DICT)?
                .get_or(engine, &Json::Int(elsewhere.into()))?,
        )
    };
    if default_array(plan, "calibration")?.contains(&string(engine)) {
        return if !at_target {
            Ok(0)
        } else if case.get("runs")?.truthy() {
            integer(case.get("runs")?)
        } else {
            target_runs()
        };
    }
    if !default_array(plan, "engines")?.contains(&string(engine)) {
        return Ok(0);
    }
    if case.get("runs")?.truthy() {
        integer(case.get("runs")?)
    } else if at_target {
        target_runs()
    } else {
        Ok(elsewhere)
    }
}

pub fn needed_runs(planned: i64) -> i64 {
    if planned == 0 {
        1
    } else {
        planned.min((MIN_VALID as i64).max((planned as f64 * VALID_SHARE).ceil() as i64))
    }
}

pub fn case_named<'a>(plan: &'a Json, name: &str) -> Result<Option<&'a Json>, Error> {
    for case in list(plan.item("cases")?)? {
        if text(case.item("name")?)? == name {
            return Ok(Some(case));
        }
    }
    Ok(None)
}
pub fn point_key(record: &Json) -> Result<PointKey, Error> {
    Ok((
        text(record.item("kind")?)?.into(),
        text(record.item("case")?)?.into(),
        text(record.item("direction")?)?.into(),
        integer(record.item("row")?)?,
        integer(record.item("count")?)?,
        text(record.item("engine")?)?.into(),
    ))
}
pub fn curve_key(case: &Json, count: i64, engine: &str) -> Result<PointKey, Error> {
    Ok((
        "curve".into(),
        text(case.item("name")?)?.into(),
        text(case.item("direction")?)?.into(),
        integer(case.item("row")?)?,
        count,
        engine.into(),
    ))
}
pub fn key_json(key: &PointKey) -> Json {
    Json::List(vec![
        string(&key.0),
        string(&key.1),
        string(&key.2),
        Json::Int(key.3.into()),
        Json::Int(key.4.into()),
        string(&key.5),
    ])
}
pub fn planned_runs(plan: &Json, key: &PointKey) -> Result<i64, Error> {
    if key.0 == "p1" {
        return integer(plan.item("p1")?.item("repeats")?);
    }
    match case_named(plan, &key.1)? {
        Some(case) => runs_for(plan, case, key.4, &key.5),
        None => Ok(0),
    }
}

pub fn close_calls(valid: &[&Json]) -> Result<Vec<String>, Error> {
    let mut found = Vec::new();
    for (name, key, limit) in [
        ("incomplete", "", 0.0),
        ("missing Sub-Intervals", "missing_sub_intervals", 0.0),
        ("P2b", "not_sent_pct", P2B_NOT_SENT),
        ("P2c", "short_median_pct", P2C_SHORT),
        ("loss", "engine_loss", LOSS as f64),
    ] {
        let mut values = if key.is_empty() {
            valid
                .iter()
                .filter_map(|r| incomplete(r).transpose())
                .collect::<Result<Vec<_>, _>>()?
        } else {
            values(valid, key)?
        };
        values.sort_by(|a, b| a.py_cmp(*b).unwrap_or(Ordering::Equal));
        if values.len() >= 2 {
            let at = |q: f64| (q * (values.len() - 1) as f64 + 0.5) as usize;
            if le(Some(values[at(CLOSE_CALL.0)]), limit)
                && gt(Some(values[at(CLOSE_CALL.1)]), limit)
            {
                found.push(name.into());
            }
        }
    }
    Ok(found)
}

pub fn point_gates(kind: &str, runs: &[&Json]) -> Result<(Json, bool), Error> {
    let incomplete = optional_num(num::median(
        &runs
            .iter()
            .filter_map(|r| incomplete(r).transpose())
            .collect::<Result<Vec<_>, _>>()?,
    ));
    if kind == "p1" {
        // These fields are mandatory in P1 records, even when null.
        for r in runs {
            for key in ["p1_p99_us", "p1_p999_us", "p1_worst_us", "p1_max_us"] {
                r.item(key)?;
            }
        }
        let f = dict([
            ("p99_us", med(runs, "p1_p99_us")?),
            ("p999_us", med(runs, "p1_p999_us")?),
            ("worst_us", med(runs, "p1_worst_us")?),
            ("max_us", maximum(runs, "p1_max_us")?),
            ("incomplete", incomplete),
        ]);
        let ok = le(n(&f, "p99_us")?, P1_P99 as f64)
            && (f.get("p999_us")?.is_null() || le(n(&f, "p999_us")?, P1_P999 as f64))
            && le(n(&f, "worst_us")?, P1_WORST as f64)
            && is_zero(n(&f, "incomplete")?);
        return Ok((f, ok));
    }
    let matrix = matrix_runs(runs)?;
    let f = dict([
        ("not_sent_pct", med(runs, "not_sent_pct")?),
        ("short_median_pct", med(runs, "short_median_pct")?),
        ("over_pct", maximum(&matrix, "over_pct")?),
        ("over_raw_pct", maximum(&matrix, "over_raw_pct")?),
        ("loss", med(runs, "engine_loss")?),
        ("incomplete", incomplete),
        ("left_out", total(runs, "left_out")?),
        ("missing", med(runs, "missing_sub_intervals")?),
        ("delivered_pct", med(runs, "delivered_pct")?),
        ("sender_short_pct", med(runs, "sender_short_pct")?),
        ("server_busy", med_nested(runs, "server_cpu", "busy")?),
        ("other_cpus", med(runs, "other_cpus")?),
        (
            "wakeups_per_s",
            med_nested(runs, "server_threads", "wakeups_per_s")?,
        ),
        ("lateness_p99_us", med(runs, "burst_lateness_p99_us")?),
    ]);
    let ok = is_zero(n(&f, "incomplete")?)
        && is_zero(n(&f, "missing")?)
        && (f.get("not_sent_pct")?.is_null() || le(n(&f, "not_sent_pct")?, P2B_NOT_SENT))
        && le(n(&f, "short_median_pct")?, P2C_SHORT)
        && le(n(&f, "over_pct")?, P2A_OVER)
        && le(n(&f, "loss")?, LOSS as f64);
    Ok((f, ok))
}
fn matrix_runs<'a>(runs: &[&'a Json]) -> Result<Vec<&'a Json>, Error> {
    let mut matrix = Vec::new();
    for r in runs {
        if !r.get("more")?.truthy() {
            matrix.push(*r);
        }
    }
    Ok(if matrix.is_empty() {
        runs.to_vec()
    } else {
        matrix
    })
}
pub fn own_sending(record: &Json) -> Result<bool, Error> {
    Ok(
        matches!(record.get("engine")?.as_str(), Some("udpstp" | "obudpst"))
            && record.get("direction")?.as_str() == Some("downstream")
            && matches!(
                record.get("kind")?.as_str(),
                Some("curve" | "confine" | "probe")
            ),
    )
}
pub fn other_flags(record: &Json) -> Result<Vec<Json>, Error> {
    let room = array(record, "room_flags")?;
    Ok(array(record, "flags")?
        .iter()
        .filter(|flag| !room.contains(flag))
        .cloned()
        .collect())
}
pub fn own_gates_fail(runs: &[&Json]) -> Result<bool, Error> {
    let matrix = matrix_runs(runs)?;
    Ok(gt(numeric(&maximum(&matrix, "over_pct")?)?, P2A_OVER)
        || gt(numeric(&med(runs, "not_sent_pct")?)?, P2B_NOT_SENT)
        || gt(
            numeric(&med(runs, "short_completed_median_pct")?)?,
            P2C_SHORT,
        )
        || gt(numeric(&med(runs, "engine_loss")?)?, LOSS as f64))
}

pub fn verdict(kind: &str, group: &[&Json], planned: i64) -> Result<Json, Error> {
    let needed = needed_runs(planned);
    let mut valid = Vec::new();
    let mut flagged = 0;
    let mut limited = 0;
    for r in group {
        if r.item("valid")?.truthy() {
            valid.push(*r);
            flagged += i128::from(r.get("flags")?.truthy());
        } else {
            limited += usize::from(r.get("platform")?.truthy());
        }
    }
    let mut v = dict([
        ("runs", Json::Int(group.len() as i128)),
        ("valid", Json::Int(valid.len() as i128)),
        ("planned", Json::Int(planned.into())),
        ("needed", Json::Int(needed.into())),
        ("flagged", Json::Int(flagged)),
        ("close_call", EMPTY_LIST.clone()),
    ]);
    let (figures, ok) = point_gates(kind, &valid)?;
    extend(&mut v, figures)?;
    if kind != "p1" {
        set(&mut v, "path_loss", total(group, "path_loss")?)?;
        set(&mut v, "close_call", strings(close_calls(&valid)?))?;
    }
    let answer;
    let mut why = None;
    if (valid.len() as i64) < needed {
        answer = if limited * 2 > group.len() - valid.len() {
            "platform-limited"
        } else {
            "not judged"
        };
    } else if ok {
        answer = "passes";
    } else {
        let mut clean = Vec::new();
        let mut own = Vec::new();
        let mut all_own = true;
        for r in &valid {
            if !r.get("flags")?.truthy() {
                clean.push(*r);
            }
            all_own &= own_sending(r)?;
            if other_flags(r)?.is_empty() {
                own.push(*r);
            }
        }
        if !all_own {
            own.clear();
        }
        if clean.len() >= MIN_VALID && !point_gates(kind, &clean)?.1 {
            answer = "fails";
        } else if own.len() >= MIN_VALID && own_gates_fail(&own)? {
            answer = "fails";
            why = Some("on the Server's own clock and counts, over its runs flagged only for a Client host's room too".into());
        } else {
            answer = "platform-limited";
            let mut reason = if clean.len() >= MIN_VALID {
                "it fails with its flagged runs, not without them".into()
            } else {
                format!(
                    "it fails, and only {} of its runs weren't flagged",
                    clean.len()
                )
            };
            let mut room = false;
            for r in &own {
                room |= r.get("room_flags")?.truthy();
            }
            if own.len() >= MIN_VALID && room {
                reason += "; not on the Server's own clock and counts, where a Client host short of room excuses only Test Connections that didn't complete";
            }
            why = Some(reason);
        }
    }
    set(&mut v, "verdict", string(answer))?;
    if let Some(why) = why {
        set(&mut v, "why", Json::Str(why))?;
    }
    Ok(v)
}

pub fn points(plan: &Json, records: &[Json]) -> Result<Verdicts, Error> {
    let mut groups = IndexMap::<PointKey, Vec<&Json>>::new();
    for r in records {
        if matches!(r.get("kind")?.as_str(), Some("curve" | "p1")) {
            groups.entry(point_key(r)?).or_default().push(r);
        }
    }
    groups
        .into_iter()
        .map(|(key, group)| {
            Ok((
                key.clone(),
                verdict(&key.0, &group, planned_runs(plan, &key)?)?,
            ))
        })
        .collect()
}

pub fn confinement_verdicts(plan: &Json, records: &[Json]) -> Result<Json, Error> {
    let mut groups = IndexMap::<(String, String), Vec<&Json>>::new();
    for r in records {
        if r.get("kind")?.as_str() == Some("confine") {
            groups
                .entry((
                    text(r.item("case")?)?.into(),
                    text(r.get("confine")?)?.into(),
                ))
                .or_default()
                .push(r);
        }
    }
    let runs = integer(
        plan.get_or("confine", &EMPTY_DICT)?
            .get_or("runs", &Json::Int(0))?,
    )?;
    let mut found = IndexMap::<String, Json>::new();
    for ((case, candidate), group) in groups {
        set(
            found.entry(case).or_insert_with(|| EMPTY_DICT.clone()),
            &candidate,
            verdict("confine", &group, runs)?,
        )?;
    }
    Ok(Json::Dict(found))
}

pub fn confinement(plan: &Json, verdicts: &Json) -> Result<Json, Error> {
    let step = plan.get("confine")?.or(&EMPTY_DICT);
    let candidates = default_array(step, "candidates")?;
    let mut chosen = IndexMap::new();
    for case in default_array(plan, "cases")? {
        if !case.get("confined")?.truthy() {
            continue;
        }
        let name = text(case.item("name")?)?;
        let seen = verdicts.or(&EMPTY_DICT).get_or(name, &EMPTY_DICT)?;
        let mut all_seen = true;
        let mut selected = None;
        for candidate in candidates {
            let v = seen.get(text(candidate)?)?;
            if v.is_null() {
                all_seen = false;
                break;
            }
            if v.item("verdict")?.as_str() == Some("fails") {
                selected = Some((
                    candidate.clone(),
                    format!(
                        "udpstp alone fails its point at {} Test Connections there",
                        display(step.get("count")?)
                    ),
                ));
                break;
            }
        }
        if selected.is_none()
            && all_seen
            && let Some(last) = candidates.last()
        {
            selected = Some((
                last.clone(),
                format!(
                    "udpstp alone fails its point at {} Test Connections on none of the candidates, so the last, the smallest",
                    display(step.get("count")?)
                ),
            ));
        }
        if let Some((cpu, why)) = selected {
            chosen.insert(name.into(), Json::List(vec![cpu, Json::Str(why)]));
        }
    }
    Ok(Json::Dict(chosen))
}

fn counts(case: &Json) -> Result<Vec<i64>, Error> {
    list(case.item("counts")?)?.iter().map(integer).collect()
}
pub fn capacity(verdicts: &Verdicts, case: &Json, engine: &str) -> Result<Json, Error> {
    let mut found = false;
    let mut last = 0;
    let mut bound = None;
    let mut inconsistent = false;
    for count in counts(case)? {
        let Some(v) = verdicts.get(&curve_key(case, count, engine)?) else {
            if found && bound.is_none() {
                bound = Some("not run above".into());
            }
            continue;
        };
        found = true;
        let answer = text(v.item("verdict")?)?;
        if bound.is_none() {
            if answer == "passes" {
                last = count;
                continue;
            }
            bound = Some(if answer == "fails" {
                "fails above".into()
            } else {
                format!("{answer} above")
            });
        } else if answer == "passes" {
            inconsistent = true;
        }
    }
    Ok(if !found {
        Json::Null
    } else {
        dict([
            ("count", Json::Int(last.into())),
            (
                "bound",
                Json::Str(bound.unwrap_or_else(|| "top of the grid".into())),
            ),
            ("inconsistent", Json::Bool(inconsistent)),
        ])
    })
}

pub fn carries(verdicts: &Verdicts, case: &Json, engine: &str, need: i64) -> Result<Json, Error> {
    if need <= 0 {
        return Ok(Json::Bool(true));
    }
    let Some(v) = verdicts.get(&curve_key(case, need, engine)?) else {
        return Ok(Json::Null);
    };
    let answer = text(v.item("verdict")?)?;
    if answer == "fails" {
        return Ok(Json::Bool(false));
    }
    if answer != "passes" {
        return Ok(Json::Null);
    }
    if let Some(below) = counts(case)?.into_iter().filter(|c| *c < need).max()
        && let Some(b) = verdicts.get(&curve_key(case, below, engine)?)
        && b.item("verdict")?.as_str() == Some("fails")
    {
        return Ok(string("inconsistent"));
    }
    Ok(Json::Bool(true))
}

pub fn reading(verdicts: &Verdicts, case: &Json, need: i64) -> Result<Json, Error> {
    let ours = carries(verdicts, case, "udpstp", need)?;
    if ours.as_str() == Some("inconsistent") {
        return Ok(string("i"));
    }
    if ours == Json::Bool(true) {
        return Ok(Json::Int(1));
    }
    if ours.is_null() {
        return Ok(Json::Int(0));
    }
    let control = carries(verdicts, case, "prototype-a", need)?;
    let per_core = carries(verdicts, case, "prototype-b", need)?;
    if control == Json::Bool(true) {
        return Ok(Json::Int(4));
    }
    if control.as_str() == Some("inconsistent") || per_core.as_str() == Some("inconsistent") {
        return Ok(string("i"));
    }
    Ok(Json::Int(if per_core == Json::Bool(true) {
        if control == Json::Bool(false) { 2 } else { 0 }
    } else if per_core == Json::Bool(false) {
        3
    } else {
        0
    }))
}

pub fn might_carry(
    verdicts: &Verdicts,
    case: &Json,
    engine: &str,
    cap: &Json,
) -> Result<i64, Error> {
    let counts = counts(case)?;
    let carried = integer(cap.item("count")?)?;
    for (i, &count) in counts.iter().enumerate() {
        if count <= carried {
            continue;
        }
        if let Some(v) = verdicts.get(&curve_key(case, count, engine)?)
            && v.item("verdict")?.as_str() == Some("fails")
        {
            return Ok(if i == 0 { carried } else { counts[i - 1] });
        }
    }
    counts
        .last()
        .copied()
        .ok_or_else(|| Error("empty counts".into()))
}

pub fn floor_stops(case: &Json, count: i64, need: i64) -> Result<String, Error> {
    let stops = counts(case)?
        .into_iter()
        .filter(|c| *c > count)
        .min()
        .ok_or_else(|| Error("no count above the floor".into()))?;
    Ok(if need == stops {
        format!("OB-UDPST's point at {stops} couldn't be judged")
    } else {
        format!("OB-UDPST's curve stops unjudged at {stops} and might reach {need}")
    })
}

pub fn floor_answer(verdicts: &Verdicts, case: &Json) -> Result<Json, Error> {
    let top = counts(case)?
        .into_iter()
        .max()
        .ok_or_else(|| Error("empty counts".into()))?;
    let floor = capacity(verdicts, case, "obudpst")?;
    let count = if floor.is_null() {
        Json::Null
    } else {
        floor.item("count")?.clone()
    };
    let lower = !floor.is_null()
        && !matches!(
            floor.item("bound")?.as_str(),
            Some("fails above" | "top of the grid")
        );
    let inconsistent = !floor.is_null() && floor.item("inconsistent")?.truthy();
    let mut need = Json::Null;
    let mut why = Json::Null;
    let mut answer = Json::Null;
    if floor.is_null() {
        why = string("OB-UDPST didn't run the case");
    } else if inconsistent {
        answer = string("inconsistent");
    } else if count == Json::Int(0) && !lower {
        why = string("the floor didn't bind: OB-UDPST carried none of the grid");
    } else {
        let carried = integer(&count)?;
        let required = if lower {
            might_carry(verdicts, case, "obudpst", &floor)?
        } else {
            carried
        };
        need = Json::Int(required.into());
        let ours = carries(verdicts, case, "udpstp", carried)?;
        if ours != Json::Bool(true) {
            answer = match ours {
                Json::Bool(false) => string("no"),
                Json::Str(ref s) if s == "inconsistent" => string("inconsistent"),
                _ => Json::Null,
            };
            if answer.is_null() {
                why = string(&format!("udpstp's point at {carried} couldn't be judged"));
            }
        } else {
            let at = carries(verdicts, case, "udpstp", required)?;
            answer = match at {
                Json::Bool(true) => string("yes"),
                Json::Str(ref s) if s == "inconsistent" => string("inconsistent"),
                _ => Json::Null,
            };
            if answer.is_null() {
                let there = if at == Json::Bool(false) {
                    format!("udpstp doesn't carry {required}")
                } else {
                    format!("udpstp's point at {required} couldn't be judged")
                };
                why = string(&format!(
                    "the floor didn't bind: {}, and {there}",
                    floor_stops(case, carried, required)?
                ));
            }
        }
    }
    let whole_grid = (answer.is_null()
        || inconsistent
        || (answer.as_str() == Some("yes") && lower && need == Json::Int(top.into())))
        && carries(verdicts, case, "udpstp", top)? == Json::Bool(true);
    if whole_grid {
        answer = string("yes");
        why = Json::Null;
    }
    Ok(dict([
        ("answer", answer),
        ("count", count),
        ("lower_bound", Json::Bool(lower)),
        ("need", need),
        ("why", why),
        ("whole_grid", Json::Bool(whole_grid)),
    ]))
}

pub fn decide(must_have: &str, floor: &Json, target: &Json) -> Json {
    if must_have == "target" {
        target.clone()
    } else if floor.as_str() == Some("inconsistent") {
        string("i")
    } else if floor.is_null() {
        Json::Int(0)
    } else if floor.as_str() == Some("yes") {
        if eq(target, &Json::Int(1)) {
            Json::Int(1)
        } else {
            string("aim")
        }
    } else if eq(target, &Json::Int(2)) {
        Json::Int(2)
    } else {
        Json::Int(4)
    }
}

/// The case lines and the summary's ordered (outcome, case names) pairs.
/// Summary keys are numbers or words, so pairs preserve both their kinds.
pub fn outcomes(plan: &Json, verdicts: &Verdicts) -> Result<Outcomes, Error> {
    let must_have = text(plan.get_or("must_have", &string("obudpst"))?)?.to_owned();
    let mut lines = Vec::new();
    let mut summary: Vec<(Json, Vec<Json>)> = Vec::new();
    for case in list(plan.item("cases")?)? {
        let caps = Json::Dict(
            CURVE_ENGINES
                .iter()
                .map(|e| Ok(((*e).into(), capacity(verdicts, case, e)?)))
                .collect::<Result<_, Error>>()?,
        );
        let mut info = dict([
            ("caps", caps.clone()),
            ("target", Json::Null),
            ("floor", Json::Null),
            ("candidate", Json::Bool(false)),
            ("pinned_only", Json::Bool(false)),
        ]);
        let outcome = if !case.get_or("judged", &Json::Bool(true))?.truthy() {
            string("report")
        } else {
            let need = integer(case.item("target")?)?;
            let target = reading(verdicts, case, need)?;
            let floor = floor_answer(verdicts, case)?;
            let mut answer = floor.item("answer")?.clone();
            if answer.as_str() == Some("no")
                && eq(&target, &Json::Int(1))
                && integer(floor.item("count")?)? <= need
            {
                answer = string("inconsistent");
            }
            let outcome = decide(&must_have, &answer, &target);
            let ours = caps.item("udpstp")?;
            let per_core = caps.item("prototype-b")?;
            if ![Json::Int(1), Json::Int(2), Json::Int(4)].contains(&target)
                && per_core.truthy()
                && !ours.is_null()
                && integer(per_core.item("count")?)? > 0
                && integer(per_core.item("count")?)? as f64
                    >= DESIGN_CANDIDATE * integer(ours.item("count")?)? as f64
            {
                set(&mut info, "candidate", Json::Bool(true))?;
            }
            let pinned = !eq(&target, &Json::Int(1))
                && carries(verdicts, case, "prototype-b-core", need)? == Json::Bool(true)
                && carries(verdicts, case, "prototype-b", need)? != Json::Bool(true);
            set(&mut info, "target", target)?;
            set(&mut info, "floor", floor)?;
            set(&mut info, "pinned_only", Json::Bool(pinned))?;
            outcome
        };
        if let Some((_, names)) = summary.iter_mut().find(|(key, _)| eq(key, &outcome)) {
            names.push(case.item("name")?.clone());
        } else {
            summary.push((outcome.clone(), vec![case.item("name")?.clone()]));
        }
        lines.push(Json::List(vec![case.clone(), info, outcome]));
    }
    Ok((lines, summary))
}

pub fn decision_points(plan: &Json, verdicts: &Verdicts) -> Result<Vec<PointKey>, Error> {
    let mut chosen = Vec::new();
    let calibration = default_array(plan, "calibration")?
        .iter()
        .map(text)
        .collect::<Result<Vec<_>, _>>()?;
    for case in list(plan.item("cases")?)? {
        if !judged_like(plan, case)? {
            continue;
        }
        for engine in CURVE_ENGINES.into_iter().chain(calibration.iter().copied()) {
            let cap = if CURVE_ENGINES.contains(&engine) {
                capacity(verdicts, case, engine)?
            } else {
                Json::Null
            };
            for count in counts(case)? {
                let key = curve_key(case, count, engine)?;
                let Some(v) = verdicts.get(&key) else {
                    continue;
                };
                let shaky = v.get("close_call")?.truthy()
                    || (!cap.is_null()
                        && cap.item("inconsistent")?.truthy()
                        && count > integer(cap.item("count")?)?);
                if shaky {
                    chosen.push((count != integer(case.item("target")?)?, key));
                }
            }
        }
    }
    chosen.sort_by_key(|(other, _)| *other);
    Ok(chosen.into_iter().map(|(_, key)| key).collect())
}

pub fn unusable(record: &Json) -> Result<bool, Error> {
    let void = !record.get("valid")?.truthy() && !only_the_path(record)?;
    if void || !record.get("flags")?.truthy() || record.get("meets")?.truthy() {
        return Ok(void);
    }
    Ok(!(own_sending(record)? && other_flags(record)?.is_empty() && own_gates_fail(&[record])?))
}
pub fn probe_stops(records: &[Json], engines: &[Json], cases: &[Json]) -> Result<bool, Error> {
    let mut probed = Vec::new();
    for r in records {
        if r.get("kind")?.as_str() == Some("probe")
            && engines.contains(r.get("engine")?)
            && cases.contains(r.get("case")?)
        {
            probed.push(r);
        }
    }
    if probed.is_empty() {
        return Ok(false);
    }
    for engine in engines {
        for case in cases {
            let mut found = false;
            for r in &probed {
                found |= r.item("engine")? == engine && r.item("case")? == case;
            }
            if !found {
                return Ok(false);
            }
        }
    }
    for r in probed {
        if !unusable(r)? {
            return Ok(false);
        }
    }
    Ok(true)
}

pub fn startup_settings(meta: &Json) -> Result<Json, Error> {
    let env = meta.get("server_env")?.or(&EMPTY_DICT);
    let meant = meta.get("executor")?.or(env.get(EXECUTOR_ENV)?);
    let env_keys = env
        .as_dict()
        .ok_or_else(|| Error("expected a Server environment dict".into()))?;
    if !meant.truthy()
        && !env_keys.contains_key(THREADS_ENV)
        && !env_keys.contains_key(GROUPING_ENV)
    {
        return Ok(Json::Null);
    }
    let meant = meant.or(&string("pool")).clone();
    if meant.as_str() != Some("pool") {
        return Ok(dict([
            ("executor", meant),
            ("engine_threads", Json::Null),
            ("grouping_us", Json::Int(0)),
        ]));
    }
    let threads = display(env.get_or(THREADS_ENV, &string(""))?);
    let grouping = display(env.get_or(GROUPING_ENV, &string(""))?);
    let digits = |s: &str| {
        let s = py::strip(s);
        // Matrices permit only ASCII environment values. In an edited
        // record, Python's isdigit accepts characters int() cannot read.
        if !s.is_ascii() {
            return Err(Error("non-ASCII numeric environment setting".into()));
        }
        (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse::<i128>())
            .transpose()
            .map_err(|_| Error("integer out of range".into()))
    };
    let cpus = meta.get("server_cpus")?;
    let threads = if let Some(n) = digits(&threads)? {
        Json::Int(n)
    } else if cpus.truthy() {
        Json::Int(list(cpus)?.len() as i128)
    } else {
        Json::Null
    };
    Ok(dict([
        ("executor", meant),
        ("engine_threads", threads),
        ("grouping_us", Json::Int(digits(&grouping)?.unwrap_or(0))),
    ]))
}
pub fn startup_problems(meta: &Json, record: &Json) -> Result<Vec<String>, Error> {
    let meant = startup_settings(meta)?;
    if meant.is_null() {
        return Ok(Vec::new());
    }
    let found = array(record, "engine_startups")?;
    if found.is_empty() {
        return Ok(vec![format!(
            "the tooling failed: the Server logged no startup record ({DATA_PHASE_ENGINES}), which would say how its Data Phase Engines ran"
        )]);
    }
    let said =
        format!("the tooling failed: the Server's startup record ({DATA_PHASE_ENGINES}) says ");
    let mut problems = Vec::new();
    for startup in found {
        if !eq(startup.item("executor")?, meant.item("executor")?) {
            problems.push(format!(
                "{said}{}, where its settings ask for {}",
                executor_name(&display(startup.item("executor")?)),
                executor_name(&display(meant.item("executor")?))
            ));
            continue;
        }
        if !meant.item("engine_threads")?.is_null()
            && !eq(
                startup.item("engine_threads")?,
                meant.item("engine_threads")?,
            )
        {
            problems.push(format!(
                "{said}at most {} Engine Threads, where its settings ask for {}",
                display(startup.item("engine_threads")?),
                display(meant.item("engine_threads")?)
            ));
        }
        if !eq(startup.item("grouping_us")?, meant.item("grouping_us")?) {
            problems.push(format!(
                "{said}a grouping window of {} µs, where its settings ask for {} µs",
                display(startup.item("grouping_us")?),
                display(meant.item("grouping_us")?)
            ));
        }
    }
    let mut unique = Vec::new();
    for problem in problems {
        if !unique.contains(&problem) {
            unique.push(problem);
        }
    }
    Ok(unique)
}
pub fn engine_problems(meta: &Json, record: &Json) -> Result<Vec<String>, Error> {
    if meta.get("engine")?.as_str() != Some("udpstp") || meta.get("kind")?.as_str() == Some("p1") {
        return Ok(Vec::new());
    }
    let mut problems = startup_problems(meta, record)?;
    let meant = meta.get("executor")?;
    let ran = record.get("executor_ran")?;
    if meant.truthy() && ran.is_null() {
        problems.push(format!("the tooling failed: the Server logged no {DATA_PHASE_ENDED} event naming its fast paths, which would say whether it ran {}", executor_name(&display(meant))));
    } else if meant.truthy() && !eq(ran, meant) {
        if !matches!(ran.as_str(), Some("pool" | "per-test-connection" | "both")) {
            return Err(Error("unknown executor in the Server's record".into()));
        }
        problems.push(format!(
            "the tooling failed: the Server ran {}, not {} the run meant",
            executor_name(text(ran)?),
            executor_name(&display(meant))
        ));
    }
    let stamps = meta.get("server_env")?.or(&EMPTY_DICT).get(STAMPS_ENV)?;
    if !stamps.is_null() && !matches!(stamps.as_str(), Some("" | "0")) {
        if record.get(SEND_LAG_P99)?.is_null() {
            problems.push("the tooling failed: the Server logged no send lag, though its kernel send timestamps were asked for".into());
        }
        if record.get(BURST_LATENESS_P99)?.is_null() {
            problems.push("the tooling failed: the Server logged no Burst lateness, though its precision was to be judged (its kernel send timestamps were asked for)".into());
        }
    }
    Ok(problems)
}
