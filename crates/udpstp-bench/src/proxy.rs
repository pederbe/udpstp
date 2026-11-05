//! The loopback proxy's registered reading and feasibility check.
//! The rules are those of `proxy_report.py`, described in the home-lab
//! pre-registration. This module reads saved evidence and performs no I/O.

use std::cmp::Ordering;

use bench_rounds::{
    num::{self, Num},
    py::{self, Json},
};
use indexmap::IndexMap;

use crate::scale::{
    Error,
    judge::{display, eq, general, lower_p, num_json, number, sub},
};

const P: f64 = 0.05;
const NOT_SENT_SHARE: f64 = 0.0001;
const SHORTFALL_PP: f64 = 0.1;
const BOUND_US: i128 = 10;
const WAKE_SHARE: f64 = 0.75;
const RUNS: usize = 4;
const FEASIBILITY_RUNS: usize = 3;
const CANDIDATES: [(i128, i128); 3] = [(8, 125), (6, 125), (4, 125)];

/// The PROXY lines, with numbers retaining their Python integer or float
/// kind. Values outside the port's numeric domain are unreadable evidence.
pub fn parse(lines: &[&str]) -> Result<Vec<Json>, Error> {
    let mut runs = Vec::new();
    for text in lines {
        let Some(at) = text.find("PROXY\t") else {
            continue;
        };
        let mut fields = IndexMap::new();
        for item in text[at..].trim_end_matches('\n').split('\t').skip(1) {
            let (key, value) = item.split_once('=').unwrap_or((item, ""));
            fields.insert(key.into(), parsed_value(value)?);
        }
        runs.push(Json::Dict(fields));
    }
    Ok(runs)
}

fn parsed_value(value: &str) -> Result<Json, Error> {
    // Numeric conversion excludes the four ASCII information separators
    // which str.strip() otherwise treats as whitespace.
    let trimmed = value.trim_matches(|c| py::is_space(c) && !('\u{1c}'..='\u{1f}').contains(&c));
    // Python accepts underscores only between decimal digits, including
    // those in an exponent. A malformed numeric-looking value stays text.
    let bytes = trimmed.as_bytes();
    let underscores = bytes.iter().enumerate().all(|(i, b)| {
        *b != b'_'
            || (i > 0
                && bytes[i - 1].is_ascii_digit()
                && bytes.get(i + 1).is_some_and(u8::is_ascii_digit))
    });
    let plain = trimmed.replace('_', "");
    let unsigned = plain.strip_prefix(['+', '-']).unwrap_or(&plain);
    if underscores && !unsigned.is_empty() && unsigned.bytes().all(|b| b.is_ascii_digit()) {
        if unsigned.len() > 4300 {
            return Err(Error(
                "proxy integer exceeds the reference's digit limit".into(),
            ));
        }
        return plain
            .parse()
            .map(Json::Int)
            .map_err(|_| Error("proxy integer out of range".into()));
    }
    if underscores && let Ok(number) = plain.parse::<f64>() {
        return if number.is_finite() {
            Ok(Json::Float(number))
        } else {
            Err(Error("non-finite proxy number".into()))
        };
    }
    if trimmed.chars().any(|c| !c.is_ascii() && c.is_numeric()) {
        return Err(Error("proxy numbers must use ASCII digits".into()));
    }
    Ok(Json::Str(value.into()))
}

fn median(values: &[Num]) -> Result<Num, Error> {
    num::median(values).ok_or_else(|| Error("no proxy figures".into()))
}
fn figures(runs: &[&Json], key: &str) -> Result<Vec<Num>, Error> {
    runs.iter().map(|r| number(r.item(key)?)).collect()
}
fn g(n: Num) -> String {
    general(n.to_f64(), 6)
}
fn greater(a: Num, b: Num) -> bool {
    a.py_cmp(b) == Some(Ordering::Greater)
}
fn yes(run: &Json, key: &str) -> Result<bool, Error> {
    Ok(run.get(key)?.as_str() == Some("yes"))
}

/// A one-sided exact test together with the registered practical margin.
pub fn worse_by(xs: &[Num], ys: &[Num], margin: Num) -> Result<(bool, f64), Error> {
    let p = lower_p(xs, ys)?;
    Ok((p < P && greater(sub(median(ys)?, median(xs)?)?, margin), p))
}

/// The comparison at one number of Engine Threads.
pub fn thread_reading(
    zero: &[Json],
    fifty: &[Json],
    window: &Json,
) -> Result<(bool, String), Error> {
    fn completed(runs: &[Json]) -> Result<Vec<&Json>, Error> {
        runs.iter()
            .filter_map(|r| match yes(r, "completed") {
                Ok(true) => Some(Ok(r)),
                Ok(false) => None,
                Err(e) => Some(Err(e)),
            })
            .collect::<Result<Vec<_>, _>>()
    }
    let done0 = completed(zero)?;
    let done50 = completed(fifty)?;
    let mut worse = Vec::new();
    let w = display(window);
    if !zero.is_empty() && done0.len() == zero.len() && done50.len() < fifty.len() {
        worse.push(
            "one of its runs' Tests didn't complete, while every W = 0 run's did".to_string(),
        );
    }
    if done0.len().min(done50.len()) < RUNS {
        return Ok((
            false,
            if worse.is_empty() {
                format!(
                    "fewer than {RUNS} completed runs of a window ({} at W = 0, {} at W = {w} µs): it doesn't agree",
                    done0.len(),
                    done50.len()
                )
            } else {
                format!("W = {w} µs is worse: {}", worse.join("; "))
            },
        ));
    }
    let all: Vec<_> = done0.iter().chain(&done50).copied().collect();
    let planned = median(&figures(&all, "planned")?)?;
    for (label, key, margin) in [
        (
            "more Skipped and Withheld Load PDUs",
            "not_sent",
            NOT_SENT_SHARE * planned.to_f64(),
        ),
        (
            "a larger median Sub-Interval shortfall",
            "shortfall_median_pct",
            SHORTFALL_PP,
        ),
    ] {
        let (xs, ys) = (figures(&done0, key)?, figures(&done50, key)?);
        let (bad, p) = worse_by(&xs, &ys, Num::Float(margin))?;
        if bad {
            worse.push(format!(
                "{label} ({} against {}, one-sided p = {}, margin {})",
                g(median(&ys)?),
                g(median(&xs)?),
                general(p, 2),
                general(margin, 6)
            ));
        }
    }
    let late0 = median(&figures(&done0, "lateness_p99_us")?)?;
    let late50 = median(&figures(&done50, "lateness_p99_us")?)?;
    let window = number(window)?;
    let bound = match window {
        Num::Int(n) => Num::Int(
            n.checked_add(BOUND_US)
                .ok_or_else(|| Error("proxy window out of range".into()))?,
        ),
        Num::Float(n) => Num::Float(n + BOUND_US as f64),
    };
    if greater(sub(late50, late0)?, bound) {
        worse.push(format!("it breaks its bound: its Burst lateness p99's median, {} µs, is more than {} µs above W = 0's, {} µs", g(late50), display(&num_json(bound)), g(late0)));
    }
    let wakeups = |runs: &[&Json]| {
        runs.iter()
            .filter_map(|r| match r.get("wakeups_per_s") {
                Ok(Json::Bool(_)) => Some(Err(Error("a boolean proxy count".into()))),
                Ok(v) => Num::from_json(v).map(Ok),
                Err(e) => Some(Err(e.into())),
            })
            .collect::<Result<Vec<_>, Error>>()
    };
    let (woke0, woke50) = (wakeups(&done0)?, wakeups(&done50)?);
    if woke0.len().min(woke50.len()) < RUNS {
        worse.push(format!(
            "its wake-ups were read in fewer than {RUNS} runs of a window"
        ));
    } else {
        let p = lower_p(&woke50, &woke0)?;
        if !(p < P
            && !greater(
                median(&woke50)?,
                Num::Float(WAKE_SHARE * median(&woke0)?.to_f64()),
            ))
        {
            worse.push(format!("it doesn't wake clearly less often ({} a second against {}, one-sided p = {}): the proxy couldn't show grouping at work", g(median(&woke50)?), g(median(&woke0)?), general(p, 2)));
        }
    }
    Ok(if worse.is_empty() {
        (
            true,
            format!(
                "W = {w} µs is no worse, keeps its bound ({} µs against {}) and wakes clearly less often ({} a second against {})",
                g(late50),
                g(late0),
                g(median(&woke50)?),
                g(median(&woke0)?)
            ),
        )
    } else {
        (false, format!("W = {w} µs is worse: {}", worse.join("; ")))
    })
}

fn pooled(runs: &[Json]) -> Result<Vec<&Json>, Error> {
    runs.iter()
        .filter_map(|r| match r.get("executor") {
            Ok(v) if v.as_str() == Some("pool") => Some(Ok(r)),
            Ok(_) => None,
            Err(e) => Some(Err(e.into())),
        })
        .collect()
}
fn scalar(value: &Json) -> Result<(), Error> {
    match value {
        Json::Null | Json::Str(_) | Json::Int(_) => Ok(()),
        Json::Float(n) if n.is_finite() => Ok(()),
        _ => Err(Error("not a proxy scalar".into())),
    }
}
fn compare(a: &Json, b: &Json) -> Result<Ordering, Error> {
    if eq(a, b) {
        return Ok(Ordering::Equal);
    }
    match (a, b) {
        (Json::Str(a), Json::Str(b)) => Ok(a.cmp(b)),
        _ => number(a)?
            .py_cmp(number(b)?)
            .ok_or_else(|| Error("incomparable proxy values".into())),
    }
}
fn ordered_unique(values: impl IntoIterator<Item = Json>) -> Result<Vec<Json>, Error> {
    let mut result = Vec::new();
    for value in values {
        scalar(&value)?;
        if result.iter().any(|v| eq(v, &value)) {
            continue;
        }
        let mut at = result.len();
        for (i, found) in result.iter().enumerate() {
            if compare(&value, found)? == Ordering::Less {
                at = i;
                break;
            }
        }
        result.insert(at, value);
    }
    Ok(result)
}

/// The judged runs' complete Markdown reading, in report-line order.
pub fn reading(runs: &[Json]) -> Result<(bool, Vec<String>), Error> {
    let pooled = pooled(runs)?;
    let windows = ordered_unique(
        pooled
            .iter()
            .map(|r| r.get("grouping_us").cloned())
            .collect::<Result<Vec<_>, _>>()?,
    )?;
    let others: Vec<_> = windows.iter().filter(|w| !eq(w, &Json::Int(0))).collect();
    let mut lines = vec!["## The loopback proxy".into(), String::new()];
    if others.len() != 1 || !windows.iter().any(|w| eq(w, &Json::Int(0))) {
        let found = if windows.is_empty() {
            "none".into()
        } else {
            windows.iter().map(display).collect::<Vec<_>>().join(", ")
        };
        lines.push(format!(
            "- **It can't be read:** it needs runs at W = 0 and at one other window; found {found}."
        ));
        lines.push("- **The proxy doesn't agree:** the default stays 0.".into());
        return Ok((false, lines));
    }
    let window = others[0];
    let mut agrees = true;
    for threads in [1, 2] {
        let mut at = Vec::new();
        for r in &pooled {
            let n = r.get("engine_threads")?;
            if matches!(n, Json::Bool(_)) {
                return Err(Error("a boolean thread count".into()));
            }
            if eq(n, &Json::Int(threads)) {
                at.push(*r)
            }
        }
        let name = if threads == 1 {
            "1 Engine Thread"
        } else {
            "2 Engine Threads"
        };
        if at.is_empty() {
            agrees = false;
            lines.push(format!("- **{name}:** not run."));
            continue;
        }
        let group = |w: &Json| {
            at.iter()
                .filter_map(|r| match r.item("grouping_us") {
                    Ok(v) if eq(v, w) => Some(Ok((*r).clone())),
                    Ok(_) => None,
                    Err(e) => Some(Err(e)),
                })
                .collect::<Result<Vec<_>, _>>()
        };
        let (ok, why) = thread_reading(&group(&Json::Int(0))?, &group(window)?, window)?;
        agrees &= ok;
        lines.push(format!("- **{name}:** {why}."));
    }
    let w = display(window);
    lines.push(if agrees {
        format!("- **The proxy agrees with W = {w} µs:** the default may become {w} µs, where the grouping arm also stands and H4c reads \"not behind\" there.")
    } else { format!("- **The proxy doesn't agree with W = {w} µs:** the default stays 0.") });
    Ok((agrees, lines))
}

pub type Shape = [Json; 3];
pub type Feasibility = (Option<Shape>, Vec<String>);
fn shape(run: &Json) -> Result<Shape, Error> {
    let fields = [
        run.get("tests")?.clone(),
        run.get("row")?.clone(),
        run.get("seconds")?.clone(),
    ];
    for field in &fields {
        scalar(field)?;
    }
    Ok(fields)
}
fn same_shape(a: &Shape, b: &Shape) -> bool {
    a.iter().zip(b).all(|(a, b)| eq(a, b))
}
fn candidate(shape: &Shape) -> usize {
    CANDIDATES
        .iter()
        .position(|(n, row)| eq(&shape[0], &Json::Int(*n)) && eq(&shape[1], &Json::Int(*row)))
        .unwrap_or(CANDIDATES.len())
}
fn shape_cmp(a: &Shape, b: &Shape) -> Result<Ordering, Error> {
    let order = candidate(a).cmp(&candidate(b));
    if order != Ordering::Equal {
        return Ok(order);
    }
    for (a, b) in a.iter().zip(b) {
        let order = compare(a, b)?;
        if order != Ordering::Equal {
            return Ok(order);
        }
    }
    Ok(Ordering::Equal)
}

/// The first feasible candidate in the registered order, and its report.
pub fn feasibility(runs: &[Json]) -> Result<Feasibility, Error> {
    let pooled = pooled(runs)?;
    let mut lines = vec![
        "## The loopback proxy's feasibility run".into(),
        String::new(),
    ];
    for r in &pooled {
        let window = r.get("grouping_us")?;
        if matches!(window, Json::Bool(_)) {
            return Err(Error("a boolean proxy window".into()));
        }
        if !eq(window, &Json::Int(0)) {
            lines.push("- **It can't be read:** a run at W = 50 µs: the feasibility run is at W = 0 only, so that no result at 50 is in view.".into());
            return Ok((None, lines));
        }
    }
    let mut shapes: Vec<Shape> = Vec::new();
    for r in &pooled {
        let s = shape(r)?;
        if shapes.iter().any(|p| same_shape(p, &s)) {
            continue;
        }
        let mut at = shapes.len();
        for (i, old) in shapes.iter().enumerate() {
            if shape_cmp(&s, old)? == Ordering::Less {
                at = i;
                break;
            }
        }
        shapes.insert(at, s);
    }
    let mut chosen = None;
    for s in shapes {
        let mut at = Vec::new();
        let mut off = Vec::new();
        for r in &pooled {
            if same_shape(&shape(r)?, &s) {
                at.push(*r);
                let threads = r.get("engine_threads")?;
                if matches!(threads, Json::Bool(_)) {
                    return Err(Error("a boolean thread count".into()));
                }
                if !eq(threads, &Json::Int(1)) || !yes(r, "completed")? || !yes(r, "feasible")? {
                    off.push(*r);
                }
            }
        }
        let named = format!(
            "{} Test Connections of row {}, {} s",
            display(&s[0]),
            display(&s[1]),
            display(&s[2])
        );
        if at.len() != FEASIBILITY_RUNS {
            lines.push(format!(
                "- {named}: not feasible ({} runs, not {FEASIBILITY_RUNS})",
                at.len()
            ));
        } else if !off.is_empty() {
            let runs = off
                .iter()
                .map(|r| {
                    let value = r.get("run")?;
                    scalar(value)?;
                    Ok(display(value))
                })
                .collect::<Result<Vec<_>, Error>>()?
                .join(", ");
            lines.push(format!("- {named}: not feasible (run {runs} isn't)"));
        } else {
            lines.push(format!("- {named}: feasible"));
            if chosen.is_none() && candidate(&s) < CANDIDATES.len() {
                chosen = Some(s);
            }
        }
    }
    lines.push(String::new());
    lines.push(if let Some(s) = &chosen {
        format!("**The first feasible candidate: {} Test Connections of row {}, {} s**, in the pre-registered order; commit it to the pre-registration before the judged runs.", display(&s[0]), display(&s[1]), display(&s[2]))
    } else { "**No candidate is feasible:** the proxy doesn't run, the shared case stays an accepted gap, and the default stays 0.".into() });
    Ok((chosen, lines))
}
