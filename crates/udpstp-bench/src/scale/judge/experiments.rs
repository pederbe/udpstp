//! The frozen A/B, grouping, and precision readings. These report the
//! historical experiments; they do not authorize another benchmark round.

use super::*;

pub const HOLD_OFF_ROWS: [&str; 5] = ["NET_RX", "LOC", "RES", "CAL", "NIC"];
pub const HOLD_OFF_GATES: [&str; 6] = [
    "incomplete",
    "missing Sub-Intervals",
    "P2b",
    "P2c",
    "P2a",
    "loss",
];
pub const EXPERIMENT_P: f64 = 0.05;
pub const EXPERIMENT_RUNS: usize = 4;
pub const WAKE_SHARE: f64 = 0.75;
pub const PROXY_BOUND_US: i64 = 10;
pub const PRECISION_NOT_SENT_SHARE: f64 = 0.0001;
pub const PRECISION_CONFIRMATION: &str = "scripts/bench/home-lab-recheck-precision.toml";
pub const PRECISION_MEASURES: [(&str, &str, Option<Num>, usize); 4] = [
    ("Skipped and Withheld Load PDUs", "server_not_sent", None, 0),
    (
        "median Sub-Interval shortfall, %",
        "short_median_pct",
        Some(Num::Float(0.1)),
        2,
    ),
    ("send lag p99, µs", "send_lag_p99_us", Some(Num::Int(10)), 0),
    (
        "Burst lateness p99, µs",
        "burst_lateness_p99_us",
        Some(Num::Int(100)),
        0,
    ),
];

pub fn gates_met(value: &Json) -> Result<Json, Error> {
    Ok(dict([
        (
            "incomplete",
            Json::Bool(eq(value.get("incomplete")?, &Json::Int(0))),
        ),
        (
            "missing Sub-Intervals",
            Json::Bool(eq(value.get("missing")?, &Json::Int(0))),
        ),
        (
            "P2b",
            Json::Bool(
                value.get("not_sent_pct")?.is_null() || le(n(value, "not_sent_pct")?, P2B_NOT_SENT),
            ),
        ),
        (
            "P2c",
            Json::Bool(le(n(value, "short_median_pct")?, P2C_SHORT)),
        ),
        ("P2a", Json::Bool(le(n(value, "over_pct")?, P2A_OVER))),
        ("loss", Json::Bool(le(n(value, "loss")?, LOSS as f64))),
    ]))
}

/// Python's general float format, at a given number of significant digits.
pub(crate) fn general(value: f64, digits: usize) -> String {
    if !value.is_finite() {
        return num::repr(value);
    }
    let scientific = format!("{value:.precision$e}", precision = digits - 1);
    let (mantissa, exponent) = scientific.split_once('e').expect("scientific format");
    let exponent: i32 = exponent.parse().expect("formatted exponent");
    let trim = |s: String| {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s
        }
    };
    if exponent < -4 || exponent >= digits as i32 {
        format!("{}e{exponent:+03}", trim(mantissa.into()))
    } else {
        trim(num::fixed(
            value,
            (digits as i32 - 1 - exponent).max(0) as usize,
        ))
    }
}

fn numbers(value: &Json, key: &str) -> Result<Vec<Num>, Error> {
    list(value.item(key)?)?.iter().map(number).collect()
}

pub fn hold_off_reading(fix: &Json, old: &Json) -> Result<Json, Error> {
    let answer = |reading: &str, why: String| Json::List(vec![string(reading), Json::Str(why)]);
    if fix.item("gates")?.is_null() || old.item("gates")?.is_null() {
        return Ok(answer(
            "can't tell",
            "an arm had too few valid runs to judge its gates".into(),
        ));
    }
    let (fc, oc) = (numbers(fix, "cpu")?, numbers(old, "cpu")?);
    if fc.len().min(oc.len()) < EXPERIMENT_RUNS {
        return Ok(answer(
            "can't tell",
            format!("an arm's CPU per Load PDU was read in fewer than {EXPERIMENT_RUNS} runs"),
        ));
    }
    let (fw, ow) = (numbers(fix, "wakeups")?, numbers(old, "wakeups")?);
    if fw.len().min(ow.len()) < EXPERIMENT_RUNS {
        return Ok(answer(
            "can't tell",
            format!(
                "an arm's wake-ups per Load PDU were read in fewer than {EXPERIMENT_RUNS} runs"
            ),
        ));
    }
    let fm = num::median(&fw).expect("four runs").to_f64();
    let om = num::median(&ow).expect("four runs").to_f64();
    let woke = format!(
        "{} with conditional receive hold-off against {}",
        num::fixed(fm, 2),
        num::fixed(om, 2)
    );
    if lower_p(&fw, &ow)? >= EXPERIMENT_P || fm > WAKE_SHARE * om {
        return Ok(answer(
            "can't tell",
            format!(
                "the Server's wake-ups per Load PDU, {woke}, aren't clearly lower with conditional receive hold-off, so the arms didn't run the two policies: the binary may lack conditional receive hold-off, or the setting didn't reach the Server"
            ),
        ));
    }
    let (lower, higher) = (lower_p(&fc, &oc)?, lower_p(&oc, &fc)?);
    let cpu = if lower < EXPERIMENT_P {
        "lower"
    } else if higher < EXPERIMENT_P {
        "higher"
    } else {
        "about the same"
    };
    let mut worse = Vec::new();
    let mut better = Vec::new();
    for gate in HOLD_OFF_GATES {
        let f = fix.item("gates")?.item(gate)?.truthy();
        let o = old.item("gates")?.item(gate)?.truthy();
        if o && !f {
            worse.push(gate);
        }
        if f && !o {
            better.push(gate);
        }
    }
    let mut said = format!(
        "with conditional receive hold-off the Server's CPU per Load PDU is {cpu} (median {} µs against {}; one-sided p = {} for lower, {} for higher), its wake-ups per Load PDU {woke}",
        num::fixed(num::median(&fc).expect("four runs").to_f64(), 2),
        num::fixed(num::median(&oc).expect("four runs").to_f64(), 2),
        general(lower, 2),
        general(higher, 2)
    );
    if !worse.is_empty() {
        said += &format!(
            "; with conditional receive hold-off {} fail{}, passing without it",
            worse.join(", "),
            if worse.len() == 1 { "s" } else { "" }
        );
    }
    if !better.is_empty() {
        said += &format!(
            "; with conditional receive hold-off {} pass{}, failing without it",
            better.join(", "),
            if better.len() == 1 { "es" } else { "" }
        );
    }
    if worse.is_empty() && better.is_empty() {
        said += "; the gates come out alike";
    }
    let reading = if cpu == "higher" {
        if better.is_empty() {
            "reverted"
        } else {
            "mixed"
        }
    } else if !worse.is_empty() {
        if cpu == "lower" { "mixed" } else { "reverted" }
    } else {
        "stays"
    };
    Ok(answer(reading, said))
}

pub fn grouping_answer(plan: &Json, verdicts: &Verdicts) -> Result<Json, Error> {
    let grouping = plan.get("grouping")?.or(&EMPTY_DICT);
    let Some(case_name) = grouping.get("case")?.as_str() else {
        return Ok(Json::Null);
    };
    let Some(arm_name) = grouping.get("arm")?.as_str() else {
        return Ok(Json::Null);
    };
    let (Some(case), Some(arm)) = (case_named(plan, case_name)?, case_named(plan, arm_name)?)
    else {
        return Ok(Json::Null);
    };
    let floor = floor_answer(verdicts, case)?;
    let (need, why) = if floor.get("need")?.is_null() {
        let need = counts(case)?
            .into_iter()
            .max()
            .ok_or_else(|| Error("empty case counts".into()))?;
        (
            need,
            format!(
                "the grid's top, since OB-UDPST's curve in {case_name} binds nothing ({})",
                display(floor.get("why")?.or(floor.item("answer")?))
            ),
        )
    } else {
        let need = integer(floor.item("need")?)?;
        (
            need,
            if eq(floor.item("need")?, floor.item("count")?) {
                format!("OB-UDPST's capacity in {case_name}")
            } else {
                format!("as many as OB-UDPST's curve in {case_name} might carry")
            },
        )
    };
    let ours = carries(verdicts, arm, "udpstp", need)?;
    let (answer, said, as_answer) = match ours {
        Json::Bool(true) => ("not behind", "carries", string("yes")),
        Json::Bool(false) => ("fewer", "doesn't carry", string("no")),
        Json::Str(ref s) if s == "inconsistent" => (
            "inconsistent",
            "carries, with the count below it failing,",
            string("inconsistent"),
        ),
        _ => ("can't tell", "can't be judged at", Json::Null),
    };
    let as_floor = dict([
        ("answer", as_answer),
        ("count", Json::Int(need.into())),
        ("lower_bound", Json::Bool(false)),
        ("need", Json::Int(need.into())),
        ("whole_grid", Json::Bool(false)),
        (
            "why",
            if answer == "can't tell" {
                Json::Str(format!("udpstp's point at {need} couldn't be judged"))
            } else {
                Json::Null
            },
        ),
    ]);
    Ok(Json::List(vec![
        string(answer),
        Json::Str(format!(
            "at this setting udpstp {said} {need} Test Connections, {why}"
        )),
        as_floor,
    ]))
}

pub fn grouping_stands(plan: &Json, records: &[Json]) -> Result<Json, Error> {
    let grouping = plan.item("grouping")?;
    let case = text(grouping.item("case")?)?;
    let arm = text(grouping.item("arm")?)?;
    let arm_case = case_named(plan, arm)?.ok_or_else(|| Error("missing grouping arm".into()))?;
    let count = counts(arm_case)?
        .into_iter()
        .min()
        .ok_or_else(|| Error("empty grouping counts".into()))?;
    let woke = |name: &str| -> Result<Vec<Num>, Error> {
        let mut values = Vec::new();
        for r in records {
            if r.get("kind")?.as_str() == Some("curve")
                && r.get("case")?.as_str() == Some(name)
                && eq(r.get("count")?, &Json::Int(count.into()))
                && r.get("engine")?.as_str() == Some("udpstp")
                && r.item("valid")?.truthy()
                && let Some(value) = n(r.get("server_threads")?.or(&EMPTY_DICT), "wakeups_per_s")?
            {
                values.push(value);
            }
        }
        Ok(values)
    };
    let (ours, theirs) = (woke(arm)?, woke(case)?);
    if ours.len().min(theirs.len()) < EXPERIMENT_RUNS {
        return Ok(Json::List(vec![
            Json::Bool(false),
            Json::Str(format!(
                "the wake-ups at {count} Test Connections were read in fewer than {EXPERIMENT_RUNS} valid runs of each"
            )),
        ]));
    }
    let p = lower_p(&ours, &theirs)?;
    let (ours, theirs) = (num::median(&ours), num::median(&theirs));
    let clearly = p < EXPERIMENT_P && le(ours, WAKE_SHARE * theirs.expect("four runs").to_f64());
    Ok(Json::List(vec![
        Json::Bool(clearly),
        Json::Str(format!(
            "at {count} Test Connections udpstp's Server woke {} times a second at this setting against {} at 0 (one-sided p = {}): {}",
            num::fmt(ours, 0, ""),
            num::fmt(theirs, 0, ""),
            general(p, 2),
            if clearly {
                "clearly less often"
            } else {
                "not clearly less often"
            }
        )),
    ]))
}

pub fn precision_counts(record: &Json) -> Result<bool, Error> {
    if record.item("valid")?.truthy() {
        return Ok(true);
    }
    let mut allowed = vec![
        PATH_LOST.to_string(),
        PATH_LEFT_OUT.to_string(),
        "the Clients' sockets overflowed".into(),
        "the Clients didn't send".into(),
    ];
    for role in CLIENT_ROLES {
        for what in ["'s host discarded datagrams", " had no room"] {
            allowed.push(format!("{role}{what}"));
        }
    }
    for reason in list(record.item("invalid")?)? {
        if !allowed
            .iter()
            .any(|p| text(reason).is_ok_and(|r| r.starts_with(p)))
        {
            text(reason)?;
            return Ok(false);
        }
    }
    Ok(true)
}
pub fn precision_margin(margin: Option<Num>, runs: &[&Json]) -> Result<Option<Num>, Error> {
    if margin.is_some() {
        return Ok(margin);
    }
    Ok(num::median(&values(runs, "planned_pdus")?)
        .map(|p| Num::Float(PRECISION_NOT_SENT_SHARE * p.to_f64())))
}
pub fn precision_measure(
    pool: &[&Json],
    per_test_connection: &[&Json],
    key: &str,
    margin: Option<Num>,
) -> Result<Json, Error> {
    let (xs, ys) = (values(pool, key)?, values(per_test_connection, key)?);
    let (ours, theirs) = (num::median(&xs), num::median(&ys));
    if xs.len() < EXPERIMENT_RUNS || ys.len() < EXPERIMENT_RUNS || margin.is_none() {
        return Ok(Json::List(vec![
            string("unreadable"),
            optional_num(ours),
            optional_num(theirs),
            Json::Null,
        ]));
    }
    let p = lower_p(&ys, &xs)?;
    let worse = p < EXPERIMENT_P
        && sub(ours.expect("four runs"), theirs.expect("four runs"))?
            .py_cmp(margin.expect("checked margin"))
            == Some(Ordering::Greater);
    Ok(Json::List(vec![
        string(if worse { "worse" } else { "no worse" }),
        optional_num(ours),
        optional_num(theirs),
        Json::Float(p),
    ]))
}
pub fn precision_row(pool: &[&Json], per_test_connection: &[&Json]) -> Result<Json, Error> {
    fn counted<'a>(runs: &[&'a Json]) -> Result<Vec<&'a Json>, Error> {
        runs.iter()
            .filter_map(|r| match precision_counts(r) {
                Ok(true) => Some(Ok(*r)),
                Ok(false) => None,
                Err(e) => Some(Err(e)),
            })
            .collect()
    }
    let (pool, other) = (counted(pool)?, counted(per_test_connection)?);
    let all = pool.iter().chain(&other).copied().collect::<Vec<_>>();
    let (mut worse, mut unreadable) = (Vec::new(), Vec::new());
    for (label, key, margin, digits) in PRECISION_MEASURES {
        let margin = precision_margin(margin, &all)?;
        let reading = precision_measure(&pool, &other, key, margin)?;
        match reading.at(0)?.as_str() {
            Some("worse") => worse.push(format!(
                "{label}: {} against {} (one-sided p = {}, margin {})",
                num::fmt(numeric(reading.at(1)?)?, digits, ""),
                num::fmt(numeric(reading.at(2)?)?, digits, ""),
                general(number(reading.at(3)?)?.to_f64(), 2),
                general(margin.expect("readable measure").to_f64(), 6)
            )),
            Some("unreadable") => unreadable.push(format!(
                "{label}: fewer than {EXPERIMENT_RUNS} runs of an executor give it"
            )),
            _ => {}
        }
    }
    if pool.len().min(other.len()) >= EXPERIMENT_RUNS {
        let ours = gates_met(&point_gates("curve", &pool)?.0)?;
        let theirs = gates_met(&point_gates("curve", &other)?.0)?;
        let mut failed = Vec::new();
        for gate in HOLD_OFF_GATES {
            if theirs.item(gate)?.truthy() && !ours.item(gate)?.truthy() {
                failed.push(gate);
            }
        }
        if !failed.is_empty() {
            worse.push(format!(
                "its point fails {}, which the per-Test-Connection executor's passes",
                failed.join(", ")
            ));
        }
    } else {
        unreadable.push(format!(
            "the gates: fewer than {EXPERIMENT_RUNS} runs of an executor count"
        ));
    }
    let (reading, why) = if !worse.is_empty() {
        ("worse", worse.join("; "))
    } else if !unreadable.is_empty() {
        ("unreadable", unreadable.join("; "))
    } else {
        ("no worse", String::new())
    };
    Ok(Json::List(vec![string(reading), Json::Str(why)]))
}
pub fn precision_reading(rows: &Json, confirmed: bool) -> Result<&'static str, Error> {
    let rows = rows
        .as_dict()
        .ok_or_else(|| Error("expected precision rows".into()))?;
    if !rows.is_empty() && rows.values().all(|r| r.as_str() == Some("no worse")) {
        return Ok("passes");
    }
    if !confirmed {
        return Ok("confirm");
    }
    Ok(if rows.values().any(|r| r.as_str() == Some("worse")) {
        "fails"
    } else {
        "can't tell"
    })
}
