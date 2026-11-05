use super::*;
use judge::{EXPERIMENT_RUNS, PRECISION_CONFIRMATION, PRECISION_MEASURES, PROXY_BOUND_US};

pub(super) fn hold_off_lines(
    out: &RoundFiles,
    plan: &Json,
    records: &[Json],
    verdicts: &Verdicts,
    chosen: &Json,
) -> Result<Vec<String>, Error> {
    let ab = plan.get("hold_off_ab")?.or(&EMPTY_DICT);
    let case = |arm| -> Result<Option<&Json>, Error> {
        match ab.get(arm)?.as_str() {
            Some(name) => judge::case_named(plan, name),
            None => Ok(None),
        }
    };
    let (Some(fix_case), Some(old_case)) = (case("fix")?, case("old")?) else {
        return Ok(Vec::new());
    };
    let found = out.json("scale/as-found.json")?;
    let queues = found
        .or(&EMPTY_DICT)
        .get("hosts")?
        .or(&EMPTY_DICT)
        .get("server")?
        .or(&EMPTY_DICT)
        .get("interrupts")?
        .or(&EMPTY_DICT);
    let mut queues = object(queues)?
        .iter()
        .map(|(k, v)| {
            Ok((
                k.parse::<i128>()
                    .map_err(|_| Error("invalid NIC IRQ".into()))?,
                k,
                v,
            ))
        })
        .collect::<Result<Vec<_>, Error>>()?;
    queues.sort_by_key(|(n, _, _)| *n);
    let nic_irqs = queues
        .iter()
        .map(|(_, k, _)| (*k).clone())
        .collect::<Vec<_>>();
    let mut arms = Vec::new();
    for case in [fix_case, old_case] {
        let count = required(case, "counts")?
            .last()
            .ok_or_else(|| Error("empty hold-off counts".into()))?;
        let mut runs = Vec::new();
        let mut valid = Vec::new();
        let mut figures = Vec::new();
        for r in records {
            if field_eq(r, "kind", "curve")?
                && eq(r.get("case")?, case.item("name")?)
                && field_eq(r, "engine", "udpstp")?
                && eq(r.get("count")?, count)
            {
                runs.push(r);
                if r.item("valid")?.truthy() {
                    valid.push(r);
                    let figure =
                        judge::hold_off_run(&out.run(text(r.item("label")?)?)?, r, &nic_irqs)?;
                    if figure.truthy() {
                        figures.push(figure);
                    }
                }
            }
        }
        let verdict = verdicts
            .get(&judge::curve_key(case, integer(count)?, "udpstp")?)
            .unwrap_or(&EMPTY_DICT);
        let figures = figures.iter().collect::<Vec<_>>();
        let mut by_cpu = IndexMap::<String, IndexMap<String, Vec<Num>>>::new();
        for figure in &figures {
            for (cpu, row) in object(figure.item("by_cpu")?)? {
                for (name, value) in object(row)? {
                    let values = by_cpu
                        .entry(cpu.clone())
                        .or_default()
                        .entry(name.clone())
                        .or_default();
                    if let Some(value) = numeric(value)? {
                        values.push(value);
                    }
                }
            }
        }
        let by_cpu = Json::Dict(
            by_cpu
                .into_iter()
                .map(|(cpu, row)| {
                    (
                        cpu,
                        Json::Dict(
                            row.into_iter()
                                .map(|(name, values)| (name, optional_num(num::median(&values))))
                                .collect(),
                        ),
                    )
                })
                .collect(),
        );
        let mut host = IndexMap::new();
        for name in judge::HOLD_OFF_ROWS {
            let values = figures
                .iter()
                .filter_map(|f| {
                    f.item("host")
                        .map_err(Error::from)
                        .and_then(|h| n(h, name))
                        .transpose()
                })
                .collect::<Result<Vec<_>, _>>()?;
            host.insert(name.into(), optional_num(num::median(&values)));
        }
        let gates = if verdict.truthy()
            && number(verdict.item("valid")?)?.py_cmp(number(verdict.item("needed")?)?)
                != Some(std::cmp::Ordering::Less)
        {
            judge::gates_met(verdict)?
        } else {
            Json::Null
        };
        arms.push(dict([
            ("case", case.clone()),
            ("count", count.clone()),
            ("runs", Json::List(runs.into_iter().cloned().collect())),
            ("valid", Json::List(valid.into_iter().cloned().collect())),
            ("v", verdict.clone()),
            (
                "cpu",
                Json::List(
                    run_values(&figures, "cpu_per_pdu_us")?
                        .into_iter()
                        .map(judge::num_json)
                        .collect(),
                ),
            ),
            (
                "wakeups",
                Json::List(
                    run_values(&figures, "wakeups_per_pdu")?
                        .into_iter()
                        .map(judge::num_json)
                        .collect(),
                ),
            ),
            ("gates", gates),
            ("host", Json::Dict(host)),
            ("by_cpu", by_cpu),
        ]));
    }
    let (fix, old) = (&arms[0], &arms[1]);
    let old_name = env_words(old_case.get("server_env")?)?;
    let names = [
        "with conditional receive hold-off".to_string(),
        if old_name.is_empty() {
            "without conditional receive hold-off".into()
        } else {
            old_name
        },
    ];
    let shared = fix_case.get("confined_as")?;
    let place = if shared.truthy() {
        format!(
            ", on {}, the calibration step's for {}",
            cpus_named(&cpus_or_question(chosen, text(shared)?)?),
            display(shared)
        )
    } else {
        ", on the whole host".into()
    };
    let clients = env_words(plan.get("client_env")?)?;
    let mut affinity = Vec::new();
    for (_, irq, q) in queues {
        let cpus = default_list(q, "cpus")?
            .iter()
            .map(display)
            .collect::<Vec<_>>()
            .join(",");
        affinity.push(format!(
            "{irq} ({}) on {}",
            display(q.get_or("name", &string("?"))?),
            cpus_named(&string(&cpus))
        ));
    }
    let affinity = if affinity.is_empty() {
        "none found".into()
    } else {
        affinity.join("; ")
    };
    let mut lines = vec![
        String::new(),
        "## conditional receive hold-off's A/B".into(),
        String::new(),
        format!(
            "{}, {} Test Connections of row {}{place}; reported, not judged. udpstp's Server as it ships ({}), whose Load Receiver holds off only where more than one datagram is due within its hold-off (conditional receive hold-off), against the same Server with {} ({}), whose Load Receiver holds off after every turn that read, as before conditional receive hold-off. Their runs went in pairs, one of each, the first of a pair taking turns{}.",
            capitalize(text(fix_case.item("direction")?)?),
            v(fix, "count")?,
            v(fix_case, "row")?,
            v(fix_case, "name")?,
            names[1],
            v(old_case, "name")?,
            if clients.is_empty() {
                String::new()
            } else {
                format!("; udpstp's Clients ran with {clients} in both")
            }
        ),
        String::new(),
        format!(
            "The Server's CPU per Load PDU is its process's CPU time and the whole host's softirq and irq time, over the window in which every Test Connection ran, per Load PDU it received there (those planned, as delivered); where the kernel accounts IRQ time, a softirq's time counts once, as softirq time. Its wake-ups are its threads' voluntary context switches over the same window, per Load PDU: the arms ran the two policies only where they are clearly lower with conditional receive hold-off. The Clients' unsent Load PDUs are the median valid run's, and the worst run's of all; lost ones are those the Server host accounts for and those only the path can have lost, the median valid run's. The softirqs and interrupts are per 1,000 Load PDUs received, over the whole run, the median valid run's: NET_RX from /proc/softirqs; LOC (the local timer), RES (rescheduling) and CAL (function-call) interrupts and the NIC's queues' ({affinity}, as found) from /proc/interrupts."
        ),
        String::new(),
    ];
    lines.extend(strs(&["| Server | Valid runs | Verdict | CPU per Load PDU, µs: median (quarter–three-quarter) | Wake-ups per Load PDU | Clients' unsent: median; worst | Lost at the Server host | Lost on the path | NET_RX | LOC | RES | CAL | NIC's |","|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|"]));
    for (i, a) in arms.iter().enumerate() {
        let cpu = required(a, "cpu")?
            .iter()
            .map(number)
            .collect::<Result<Vec<_>, _>>()?;
        let wakeups = required(a, "wakeups")?
            .iter()
            .map(number)
            .collect::<Result<Vec<_>, _>>()?;
        let runs = required(a, "runs")?.iter().collect::<Vec<_>>();
        let valid = required(a, "valid")?.iter().collect::<Vec<_>>();
        let unsent = run_values(&runs, "clients_not_sent_pct")?;
        let med = |key, digits, unit| -> Result<String, Error> {
            Ok(num::fmt(
                num::median(&run_values(&valid, key)?),
                digits,
                unit,
            ))
        };
        let mut line = format!(
            "| {} | {}/{} | {} | {} | {} | {}; {} | {} | {}",
            names[i],
            valid.len(),
            runs.len(),
            display(a.item("v")?.get_or("verdict", &string("–"))?),
            spread(&cpu, 2),
            num::fmt(num::median(&wakeups), 2, ""),
            med("clients_not_sent_pct", 2, " %")?,
            num::fmt(num::max(&unsent), 2, " %"),
            med("engine_loss", 1, "")?,
            med("path_loss", 1, "")?
        );
        for name in judge::HOLD_OFF_ROWS {
            line += &format!(" | {}", fmt(a.item("host")?.item(name)?, 2, "")?);
        }
        line += " |";
        lines.push(line);
    }
    lines.extend(strs(&["","By logical CPU, the median valid run: busy and softirq as a share of the logical CPU over the window, the rest per 1,000 Load PDUs; the NIC's column shows which logical CPUs took its queues' interrupts. Busy is /proc/stat's, which counts each tick whole to what runs at it: at row 4, whose threads wake as often as the tick, it can be biased either way.","","| Server | Logical CPU | Busy | Softirq | NET_RX | LOC | RES | CAL | NIC's |","|---|---:|---:|---:|---:|---:|---:|---:|---:|"]));
    for (i, a) in arms.iter().enumerate() {
        let mut cpus = object(a.item("by_cpu")?)?
            .iter()
            .map(|(cpu, row)| {
                Ok((
                    cpu.parse::<i128>()
                        .map_err(|_| Error("invalid logical CPU".into()))?,
                    row,
                ))
            })
            .collect::<Result<Vec<_>, Error>>()?;
        cpus.sort_by_key(|(cpu, _)| *cpu);
        for (cpu, row) in cpus {
            let mut line = format!(
                "| {} | {cpu} | {} | {}",
                names[i],
                fmt(row.get("busy")?, 2, "")?,
                fmt(row.get("softirq")?, 2, "")?
            );
            for name in judge::HOLD_OFF_ROWS {
                line += &format!(" | {}", fmt(row.get(name)?, 2, "")?);
            }
            line += " |";
            lines.push(line);
        }
    }
    let reading = judge::hold_off_reading(fix, old)?;
    let meaning = match text(reading.at(0)?)? {
        "stays" => "conditional receive hold-off stays",
        "reverted" => "conditional receive hold-off is worse than the alternative",
        "mixed" => "the result is mixed, and the result needs interpretation",
        "can't tell" => "the A/B can't tell, and the result needs interpretation",
        _ => return Err(Error("unknown hold-off reading".into())),
    };
    lines.extend([String::new(),format!("- **The rule,** pre-registered: conditional receive hold-off is worse than the alternative only if it is worse: with it, the Server's CPU per Load PDU higher than with {} while no gate passes that fails with {}, or about the same while a gate fails that passes with {}. Where the two point different ways, the CPU per Load PDU higher but a gate passing only with conditional receive hold-off, or lower but a gate failing only with conditional receive hold-off, the result is mixed, and the result needs interpretation. Otherwise conditional receive hold-off stays. The A/B can't tell where an arm has too few valid runs, or where the Server's wake-ups per Load PDU aren't clearly lower with conditional receive hold-off. Here {meaning}: {}.",names[1],names[1],names[1],text(reading.at(1)?)?)]);
    Ok(lines)
}
pub(super) fn grouping_lines(
    plan: &Json,
    records: &[Json],
    verdicts: &Verdicts,
    chosen: &Json,
    case_lines: &[Json],
) -> Result<Vec<String>, Error> {
    let found = judge::grouping_answer(plan, verdicts)?;
    if found.is_null() {
        return Ok(Vec::new());
    }
    let (answer, why, as_floor) = (text(found.at(0)?)?, text(found.at(1)?)?, found.at(2)?);
    let grouping = plan.item("grouping")?;
    let case_name = text(grouping.item("case")?)?;
    let arm_name = text(grouping.item("arm")?)?;
    let case =
        judge::case_named(plan, case_name)?.ok_or_else(|| Error("missing grouping case".into()))?;
    let arm =
        judge::case_named(plan, arm_name)?.ok_or_else(|| Error("missing grouping arm".into()))?;
    let cpus = if case.get("confined")?.truthy() {
        cpus_or_question(chosen, case_name)?
    } else {
        Json::Null
    };
    let place = if cpus.truthy() {
        format!("on {}, {case_name}'s", cpus_named(&cpus))
    } else {
        "on the whole host".into()
    };
    let counts = required(arm, "counts")?
        .iter()
        .map(display)
        .collect::<Vec<_>>()
        .join(", ");
    let window = display(
        arm.get("server_env")?
            .or(&EMPTY_DICT)
            .get_or(super::super::matrix::GROUPING_ENV, &string("?"))?,
    );
    let bound = if !window.is_empty() && window.bytes().all(|b| b.is_ascii_digit()) {
        Some(
            window
                .parse::<i128>()
                .ok()
                .and_then(|v| v.checked_add(PROXY_BOUND_US.into()))
                .ok_or_else(|| Error("grouping window out of range".into()))?,
        )
    } else {
        None
    };
    let meaning = match answer {
        "not behind" => "The case reads \"not behind\" at this setting",
        "fewer" => "The case doesn't read \"not behind\" at this setting: the default stays 0",
        "inconsistent" => {
            "A count passes above one that fails: more runs before deciding; until then the default stays 0"
        }
        "can't tell" => {
            "The arm can't tell: the default stays 0 unless it is run again and reads \"not behind\""
        }
        _ => return Err(Error("unknown grouping reading".into())),
    };
    let mut lines = vec![
        String::new(),
        format!("## Wake-up grouping: {arm_name} against {case_name}"),
        String::new(),
        format!(
            "udpstp's Server alone with {} ({arm_name}), {place}, at {counts} Test Connections, by the same gates, against OB-UDPST's curve in {case_name}; its points are in its own table below, close calls marked. Each of its runs is void unless its Server's startup record shows the window.",
            env_words(arm.get("server_env")?)?
        ),
        String::new(),
        format!("- **{case_name} at this setting: {answer}:** {why}. {meaning}."),
    ];
    let stands = judge::grouping_stands(plan, records)?;
    let said = text(stands.at(1)?)?;
    if !stands.at(0)?.truthy() {
        lines.push(format!("- **It doesn't stand for {case_name}:** {said}, so the arm didn't show that it grouped its wake-ups. {case_name} is read at 0 alone, and the default stays 0."));
        return Ok(lines);
    }
    lines.push(format!("- **It stands for {case_name}:** {said}. Its answer, with its close calls, stands for {case_name}'s where the setting becomes the default; {case_name}'s direction's whole host, and the other direction's round, stay as measured at 0."));
    let mut swapped = case_lines.to_vec();
    for line in &mut swapped {
        if line.at(0)?.item("name")?.as_str() == Some(case_name) {
            let mut info = line.at(1)?.clone();
            judge::set(&mut info, "floor", as_floor.clone())?;
            *line = Json::List(vec![line.at(0)?.clone(), info, line.at(2)?.clone()]);
        }
    }
    for (direction, meaning, said) in
        direction_readings(&swapped, chosen, &dict([(case_name, string(arm_name))]))?
    {
        if case.item("direction")?.as_str() == Some(&direction) {
            lines.push(format!("- **{}, with {arm_name}'s answer standing for {case_name}'s:** {said}: **{meaning}**: {}.",capitalize(&direction),home_lab_text(&meaning)?));
        }
    }
    if answer == "not behind" {
        let bound = bound
            .map(|n| format!("{n} µs"))
            .unwrap_or_else(|| format!("the window and {PROXY_BOUND_US} µs"));
        lines.extend([String::new(),format!("The setting becomes the default only if the shared-Engine-Thread loopback proxy agrees, as pre-registered: at 1 and at 2 Engine Threads, against 0, no more Skipped and Withheld Load PDUs and no larger median Sub-Interval shortfall (the precision check's test and margins), its worst Test Connection's Burst lateness p99 within {bound} of 0's, and clearly fewer wake-ups; else the default stays 0.")]);
    }
    Ok(lines)
}

const PRECISION_FAILURE_ROUTE: &str = "stays the lab default through D16's pass, which fixes precision first, then capacity with what remains; what ships is decided by D16's one more check: the pool where its precision check passes there and it carries at least the first check's per-core counts (125 Downstream on logical CPUs 1 and 5, 150 Upstream on 1), else the per-Test-Connection executor, with the first check's per-core numbers as sizing advice";
fn precision_reading_text(reading: &str) -> Result<String, Error> {
    Ok(match reading {
        "passes" => "the pooled executor is no worse for one Test at these rows".into(),
        "fails" => {
            format!("the pooled executor is worse for one Test. It {PRECISION_FAILURE_ROUTE}")
        }
        "can't tell" => format!(
            "some row still has fewer than {EXPERIMENT_RUNS} counted runs of an executor giving a measure. Can't tell counts as not passed and takes a failure's route: the runs that didn't count, and why, are named above, and D16's pass starts from that cause. The pool {PRECISION_FAILURE_ROUTE}"
        ),
        _ => return Err(Error("unknown precision reading".into())),
    })
}
pub(super) fn same_value(a: &Json, b: &Json) -> bool {
    match (a, b) {
        (Json::List(a), Json::List(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same_value(a, b))
        }
        (Json::Dict(a), Json::Dict(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(k, a)| b.get(k).is_some_and(|b| same_value(a, b)))
        }
        _ => eq(a, b),
    }
}
type Pairs = IndexMap<(String, String), [Vec<Json>; 2]>;
fn precision_pairs(plan: &Json) -> Result<Vec<(String, String)>, Error> {
    array(plan.get("precision")?.or(&EMPTY_DICT), "pairs")?
        .iter()
        .map(|p| {
            let p = list(p)?;
            if p.len() != 2 {
                return Err(Error("precision pair must have two cases".into()));
            }
            Ok((text(&p[0])?.into(), text(&p[1])?.into()))
        })
        .collect()
}
fn by_pair(plan: &Json, records: &[Json], pairs: &[(String, String)]) -> Result<Pairs, Error> {
    let mut found = IndexMap::new();
    for (a, b) in pairs {
        if judge::case_named(plan, a)?.is_none() || judge::case_named(plan, b)?.is_none() {
            continue;
        }
        let mut runs = [Vec::new(), Vec::new()];
        for (i, name) in [a, b].into_iter().enumerate() {
            for r in records {
                if field_eq(r, "kind", "curve")? && field_eq(r, "case", name)? {
                    runs[i].push(r.clone());
                }
            }
        }
        found.insert((a.clone(), b.clone()), runs);
    }
    Ok(found)
}
fn precision_rounds(
    plan: &Json,
    records: &[Json],
    first: Option<&(Json, Vec<Json>)>,
) -> Result<(Pairs, Option<Pairs>), Error> {
    let pairs = precision_pairs(plan)?;
    let Some((first_plan, first_records)) = first else {
        return Ok((by_pair(plan, records, &pairs)?, None));
    };
    let mut same = plan
        .get("precision")?
        .or(&EMPTY_DICT)
        .get("confirmation")?
        .truthy()
        && precision_pairs(first_plan)? == pairs;
    for (a, b) in &pairs {
        for name in [a, b] {
            let ours = judge::case_named(plan, name)?
                .ok_or_else(|| Error("missing confirmation case".into()))?;
            let theirs = judge::case_named(first_plan, name)?.unwrap_or(&EMPTY_DICT);
            for key in ["direction", "row", "counts", "server_env", "engines"] {
                same &= same_value(ours.get(key)?, theirs.get(key)?);
            }
        }
    }
    if !same {
        return Err(Error(
            "--first: this round is no confirmation of that one's precision pairs".into(),
        ));
    }
    Ok((
        by_pair(plan, records, &pairs)?,
        Some(by_pair(first_plan, first_records, &pairs)?),
    ))
}
fn precision_refs(values: &[Json]) -> Vec<&Json> {
    values.iter().collect()
}
fn run_values(runs: &[&Json], key: &str) -> Result<Vec<Num>, Error> {
    runs.iter().filter_map(|r| n(r, key).transpose()).collect()
}
fn spread(values: &[Num], digits: usize) -> String {
    if values.is_empty() {
        return "–".into();
    }
    let mut ordered = values.to_vec();
    ordered.sort_by(|a, b| a.py_cmp(*b).unwrap_or(std::cmp::Ordering::Equal));
    let at = |q: f64| Some(ordered[(q * (ordered.len() - 1) as f64 + 0.5) as usize]);
    format!(
        "{} ({}–{})",
        num::fmt(num::median(&ordered), digits, ""),
        num::fmt(at(CLOSE_CALL.0), digits, ""),
        num::fmt(at(CLOSE_CALL.1), digits, "")
    )
}
pub(super) fn precision_lines(
    plan: &Json,
    records: &[Json],
    first: Option<&(Json, Vec<Json>)>,
) -> Result<Vec<String>, Error> {
    if precision_pairs(plan)?.is_empty() {
        return Ok(Vec::new());
    }
    let confirmation = plan.item("precision")?.get("confirmation")?.truthy();
    let (ours, theirs) = precision_rounds(plan, records, first)?;
    let names = ["the pooled executor", "the per-Test-Connection executor"];
    let mut lines = strs(&[
        "",
        "## Single-Test precision",
        "",
        "One Test of one Test Connection, Downstream from udpstp's Server on the whole host, with each Server executor, kernel send timestamps on, the runs in pairs, the first of a pair taking turns. Skipped and Withheld Load PDUs are the Server's TestSummary's; the send lag, lpduTime to departure, and the Burst lateness, each Send Plan's write less its Burst's scheduled time, its DataPhaseEnded record's p99s; the shortfall the median Sub-Interval's, as P2c takes it. Medians of the runs that count (valid, or void only for what the path took or for the measuring side), with their quarter and three-quarter marks; the gates over the same runs. At each row, for each measure, the pooled executor is worse where a one-sided exact Mann-Whitney U test gives p < 0.05 that the per-Test-Connection executor's values lie below its own, and its median exceeds the other's by more than the margin: Skipped and Withheld 0.01 % of the row's planned Load PDUs, the shortfall 0.1 percentage point, the send lag 10 µs, the Burst lateness 100 µs; worse too where its point fails a gate the other's passes. Each executor needs 4 runs giving a measure.",
        "",
    ]);
    lines.push(format!(
        "| Row | Executor | Runs counted | Gates | {} |",
        PRECISION_MEASURES
            .iter()
            .map(|(label, _, _, _)| format!("{label}: median (quarter–three-quarter)"))
            .collect::<Vec<_>>()
            .join(" | ")
    ));
    lines.push(format!(
        "|---:|---|---|---|{}",
        "---:|".repeat(PRECISION_MEASURES.len())
    ));
    let (mut rows, mut said, mut uncounted) = (IndexMap::new(), Vec::new(), Vec::new());
    for (pair, arms_runs) in &ours {
        let arms = [
            judge::case_named(plan, &pair.0)?
                .ok_or_else(|| Error("missing precision case".into()))?,
            judge::case_named(plan, &pair.1)?
                .ok_or_else(|| Error("missing precision case".into()))?,
        ];
        let row = v(arms[0], "row")?;
        let before = theirs.as_ref().and_then(|p| p.get(pair));
        let read = std::array::from_fn::<_, 2, _>(|i| {
            arms_runs[i]
                .iter()
                .chain(before.map(|b| b[i].as_slice()).unwrap_or_default())
                .cloned()
                .collect::<Vec<_>>()
        });
        for i in 0..2 {
            let (name, arm, rs) = (names[i], arms[i], &read[i]);
            let mut counted = Vec::new();
            let mut path = 0;
            for r in rs {
                if judge::precision_counts(r)? {
                    if !r.item("valid")?.truthy() {
                        path += 1;
                    }
                    counted.push(r);
                } else {
                    uncounted.push(format!(
                        "`{}` ({name}): {}",
                        v(r, "label")?,
                        join(required(r, "invalid")?, "; ")?
                    ));
                }
            }
            let gates = judge::point_gates("curve", &counted)?;
            let mut failed = Vec::new();
            if !counted.is_empty() {
                for (g, ok) in object(&judge::gates_met(&gates.0)?)? {
                    if !ok.truthy() {
                        failed.push(g.clone());
                    }
                }
            }
            let mut cells = Vec::new();
            for (_, key, _, digits) in PRECISION_MEASURES {
                cells.push(spread(&run_values(&counted, key)?, digits));
            }
            let path_text = if path > 0 {
                format!(" ({path} void only for the path or the measuring side)")
            } else {
                String::new()
            };
            let verdict_text = if counted.is_empty() {
                "–".into()
            } else if gates.1 {
                "passes".into()
            } else {
                format!("fails ({})", failed.join(", "))
            };
            lines.push(format!(
                "| {row} | {name} ({}) | {}/{}{path_text} | {verdict_text} | {} |",
                v(arm, "name")?,
                counted.len(),
                rs.len(),
                cells.join(" | ")
            ));
        }
        if confirmation && theirs.is_none() {
            continue;
        }
        if let Some(before) = before {
            let kept =
                judge::precision_row(&precision_refs(&before[0]), &precision_refs(&before[1]))?;
            if kept.at(0)?.as_str() == Some("no worse") {
                rows.insert(row.clone(), kept.at(0)?.clone());
                said.push(format!("- **Row {row}: no worse**, in the first round; its confirmation's runs don't change that."));
                continue;
            }
        }
        let reading = judge::precision_row(&precision_refs(&read[0]), &precision_refs(&read[1]))?;
        rows.insert(row.clone(), reading.at(0)?.clone());
        let again = if theirs.is_some() {
            ", read again from both rounds' runs"
        } else {
            ""
        };
        let why = text(reading.at(1)?)?;
        said.push(format!(
            "- **Row {row}{again}: {}{}",
            text(reading.at(0)?)?,
            if why.is_empty() {
                ".**".into()
            } else {
                format!(":** {why}.")
            }
        ));
    }
    lines.push(String::new());
    if confirmation && theirs.is_none() {
        lines.push("These are the confirmation's runs: read them with the first round's, udpstp-bench scale-report OUT --first FIRST_OUT.".into());
        return Ok(lines);
    }
    lines.extend(said);
    if !uncounted.is_empty() {
        lines.push(format!(
            "- **Runs that didn't count:** {}.",
            uncounted.join("; ")
        ));
    }
    let reading = judge::precision_reading(&Json::Dict(rows.clone()), theirs.is_some())?;
    if reading == "confirm" {
        let again = rows
            .iter()
            .filter(|(_, r)| r.as_str() != Some("no worse"))
            .map(|(row, _)| row.as_str())
            .collect::<Vec<_>>();
        let named = if again.len() < 3 {
            again.join(" and ")
        } else {
            format!(
                "{} and {}",
                again[..again.len() - 1].join(", "),
                again[again.len() - 1]
            )
        };
        lines.push(format!("- **The precision check runs row{} {named}'s pairs once more:** from {PRECISION_CONFIRMATION}, 5 runs of each as before; they are read again from all their runs in both rounds (scale_report.py OUT --first FIRST_OUT), and no further runs follow.",if again.len()>1{"s"}else{""}));
    } else {
        lines.push(format!(
            "- **The precision check {reading}:** {}.",
            precision_reading_text(reading)?
        ));
    }
    Ok(lines)
}
