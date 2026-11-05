use super::*;

fn outcome_text(value: &Json) -> Result<&'static str, Error> {
    Ok(match display(value).as_str() {
        "1" => "udpstp carries it",
        "2" => {
            "only the per-core prototype carries it, and a thread per Test Connection (A) doesn't"
        }
        "3" => "neither udpstp nor the per-core prototype carries it",
        "4" => {
            "a thread per Test Connection (A) carries it and udpstp doesn't: udpstp's engine costs too much"
        }
        "i" => "inconsistent: more runs",
        "0" => "can't be judged: a curve stops short of the count",
        _ => return Err(Error("unknown target reading".into())),
    })
}
fn decision_text(value: &Json) -> Result<&'static str, Error> {
    Ok(match display(value).as_str() {
        "1" => "udpstp carries what it must, and the target: nothing to build",
        "2" => {
            "udpstp misses what it must, and only the per-core prototype carries the target (A doesn't): the per-core model is what's missing"
        }
        "3" => "neither udpstp nor the per-core prototype carries the target: the documented limit",
        "4" => {
            "udpstp misses what it must, and the prototype doesn't show the per-core model is what's missing: udpstp's engine is what to fix"
        }
        "aim" => {
            "udpstp carries what it must, but isn't shown to carry the target (an aim): a follow-up for the maintainer"
        }
        "i" => "inconsistent: more runs",
        "0" => "can't be judged: what the rule needs couldn't be read on these hosts",
        "report" => "reported, not judged",
        _ => return Err(Error("unknown case outcome".into())),
    })
}
fn rule_text(value: &Json) -> Result<&'static str, Error> {
    Ok(match display(value).as_str() {
        "1" => {
            "udpstp carries what it must in every judged case, and every target: nothing to build; ADR 0008 is amended to the engine udpstp runs; the curves become sizing guidance"
        }
        "2" => {
            "confirm the per-core model on bare metal at the targets, then design a shared engine for both roles (its own ADR and ticket, agreed before any of it is built)"
        }
        "3" => {
            "the measured capacities become the documented limit on this host class (sizing, no target lowered to what udpstp carries, nothing built)"
        }
        "4" => "profile and optimise udpstp's engine (a ticket), not a new one",
        "aim" => {
            "nothing is built from this round; the target's reading goes to the maintainer as a follow-up (2: a design question; 4: an optimisation ticket; 3: the documented limit; inconsistent or can't be judged: what the round showed, since the must-have holds)"
        }
        "i" => {
            "a count passes above one that fails where the rule looks; more runs before deciding"
        }
        "0" => {
            "nothing is decided from them: re-run them where the platform doesn't limit them, and where the floor didn't bind because OB-UDPST carried none of the grid, find out why first"
        }
        _ => return Err(Error("unknown Round outcome".into())),
    })
}
pub(super) fn home_lab_text(meaning: &str) -> Result<&'static str, Error> {
    Ok(match meaning {
        "fewer" => {
            "At this matrix's workload, udpstp carries fewer Test Connections than OB-UDPST in at least one comparison. Inspect the profiles for the limiting cost. This result does not establish capacity for other workloads."
        }
        "not behind" => {
            "At this matrix's workload, udpstp carries at least as many Test Connections per CPU as OB-UDPST. The comparison uses a Load PDU every 1 ms and counts up to the tested grid's limit. A close call needs more evidence before drawing a firm conclusion."
        }
        "inconsistent" => {
            "A count passes above one that fails. More evidence is needed to interpret the capacity boundary."
        }
        "unclear" => {
            "This matrix provides too little valid evidence to compare the per-core capacity. Resolve the missing evidence before interpreting the result."
        }
        _ => return Err(Error("unknown home-lab reading".into())),
    })
}
fn cap_text(cap: &Json) -> Result<String, Error> {
    if cap.is_null() {
        return Ok("–".into());
    }
    Ok(format!(
        "{} ({}{})",
        v(cap, "count")?,
        v(cap, "bound")?,
        if cap.item("inconsistent")?.truthy() {
            "; inconsistent"
        } else {
            ""
        }
    ))
}
fn reading_text(number: &Json, need: &Json) -> Result<String, Error> {
    if number.is_null() {
        return Ok("–".into());
    }
    let at = if need.is_null() {
        String::new()
    } else {
        format!(" at {}", display(need))
    };
    Ok(format!(
        "{}{at}: {}",
        display(number),
        outcome_text(number)?
    ))
}
pub(super) fn floor_text(floor: &Json, case: &Json) -> Result<String, Error> {
    if floor.is_null() {
        return Ok("–".into());
    }
    let count = floor.item("count")?;
    let answer = floor.item("answer")?;
    let need = floor.get("need")?;
    Ok(match answer.as_str() {
        Some("yes") if floor.get("whole_grid")?.truthy() => {
            "yes: udpstp carries the whole grid".into()
        }
        Some("yes") if floor.item("lower_bound")?.truthy() => format!(
            "yes, at {}, the most OB-UDPST might carry: {}",
            display(need),
            judge::floor_stops(case, integer(count)?, integer(need)?)?
        ),
        Some("yes") => format!("yes, at {}", display(count)),
        Some("no") => format!(
            "no: fewer than OB-UDPST's {}{}",
            display(count),
            if floor.item("lower_bound")?.truthy() {
                " (a lower bound: OB-UDPST's curve stops above it unjudged)"
            } else {
                ""
            }
        ),
        Some("inconsistent") => "inconsistent: more runs".into(),
        _ => format!(
            "can't tell: {}",
            display(floor.get("why")?.or(&string("the floor couldn't be read")))
        ),
    })
}
pub(super) fn direction_readings(
    case_lines: &[Json],
    chosen: &Json,
    standing: &Json,
) -> Result<Vec<(String, String, String)>, Error> {
    let mut by = BTreeMap::<String, BTreeMap<&str, (&Json, &Json)>>::new();
    for line in case_lines {
        let case = line.at(0)?;
        let info = line.at(1)?;
        if case.get_or("judged", &Json::Bool(true))?.truthy() && info.get("floor")?.truthy() {
            by.entry(v(case, "direction")?).or_default().insert(
                if case.get("confined")?.truthy() {
                    "confined"
                } else {
                    "whole"
                },
                (case, info),
            );
        }
    }
    let mut readings = Vec::new();
    for direction in ["downstream", "upstream"] {
        let Some(cases) = by.get(direction) else {
            continue;
        };
        let whole = cases.get("whole");
        let per_core = cases.get("confined");
        let answer = |pair: Option<&(&Json, &Json)>| -> Result<Json, Error> {
            match pair {
                Some((_, info)) => Ok(info.item("floor")?.item("answer")?.clone()),
                None => Ok(Json::Null),
            }
        };
        let w = answer(whole)?;
        let p = answer(per_core)?;
        let meaning = if w.as_str() == Some("no") || p.as_str() == Some("no") {
            "fewer"
        } else if w.as_str() == Some("inconsistent") || p.as_str() == Some("inconsistent") {
            "inconsistent"
        } else if p.as_str() == Some("yes") {
            "not behind"
        } else {
            "unclear"
        };
        let mut said = Vec::new();
        if let Some((case, info)) = whole {
            said.push(format!(
                "on the whole host ({}): {}",
                v(case, "name")?,
                floor_text(info.item("floor")?, case)?
            ));
        }
        if let Some((case, info)) = per_core {
            let name = v(case, "name")?;
            let cpus = cpus_or_question(chosen, &name)?;
            let named = if !standing.get(&name)?.is_null() {
                format!("{} for {name}", display(standing.get(&name)?))
            } else {
                name
            };
            said.push(format!(
                "per core ({named}, {}): {}",
                cpus_named(&cpus),
                floor_text(info.item("floor")?, case)?
            ));
        }
        readings.push((
            direction.into(),
            meaning.into(),
            if said.is_empty() {
                "not run".into()
            } else {
                said.join("; ")
            },
        ));
    }
    Ok(readings)
}
fn home_lab_reading(case_lines: &[Json], chosen: &Json) -> Result<Vec<String>, Error> {
    let mut lines = Vec::new();
    let mut meanings = Vec::new();
    for (direction, meaning, said) in direction_readings(case_lines, chosen, &EMPTY_DICT)? {
        lines.push(format!(
            "- **{}**, {said}: **{meaning}**: {}.",
            capitalize(&direction),
            home_lab_text(&meaning)?
        ));
        meanings.push(meaning);
    }
    if !meanings.is_empty() {
        let overall = if meanings.iter().any(|m| m == "fewer") {
            "fewer"
        } else if meanings.iter().all(|m| m == "not behind") {
            "not behind"
        } else if meanings.iter().any(|m| m == "inconsistent") {
            "inconsistent"
        } else {
            "unclear"
        };
        lines.push(format!("- **The check's answer: {overall}.**"));
    }
    Ok(lines)
}
pub(super) fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase())
        .unwrap_or_default()
}

pub(super) fn home_lab_capacity(
    plan: &Json,
    case_lines: &[Json],
    chosen: &Json,
) -> Result<Vec<String>, Error> {
    let mut lines = strs(&[
        "## Capacity",
        "",
        "The most Test Connections of each shape each Server carried with every gate met, and at every smaller count, on the whole host and on the same few logical CPUs; and whether udpstp carries OB-UDPST's count there (the floor: where OB-UDPST's curve stops unjudged, as many as it might carry; where it carried none, the floor doesn't bind; \"carries\" means the count passes and the count below it doesn't fail).",
        "",
        "| Case | Row | Direction | Logical CPUs | udpstp | OB-UDPST | udpstp carries OB-UDPST's count |",
        "|---|---:|---|---|---|---|---|",
    ]);
    let mut ab = object(plan.get("hold_off_ab")?.or(&EMPTY_DICT))?
        .values()
        .cloned()
        .collect::<Vec<_>>();
    ab.push(plan.get("grouping")?.or(&EMPTY_DICT).get("arm")?.clone());
    for pair in array(plan.get("precision")?.or(&EMPTY_DICT), "pairs")? {
        ab.extend(list(pair)?.iter().cloned());
    }
    for line in case_lines {
        let case = line.at(0)?;
        let info = line.at(1)?;
        let outcome = line.at(2)?;
        if ab.contains(case.item("name")?) {
            continue;
        }
        let caps = info.item("caps")?;
        let cpus = if case.get("confined")?.truthy() {
            format!(
                "{} (the calibration step's)",
                display(&cpus_or_question(chosen, text(case.item("name")?)?)?)
            )
        } else {
            "the whole host".into()
        };
        let answer = if info.get("floor")?.truthy() {
            floor_text(info.item("floor")?, case)?
        } else {
            decision_text(outcome)?.into()
        };
        lines.push(format!(
            "| {} | {} | {} | {cpus} | {} | {} | {answer} |",
            v(case, "name")?,
            v(case, "row")?,
            v(case, "direction")?,
            cap_text(caps.item("udpstp")?)?,
            cap_text(caps.item("obudpst")?)?
        ));
    }
    lines.extend(strs(&["","## What it means","","For this Server-scale matrix: in each direction the per-core comparison decides, and the whole host is a sanity check, where both carrying the grid's top isn't discriminating; udpstp carrying fewer than OB-UDPST in either decides alone.",""]));
    let read = home_lab_reading(case_lines, chosen)?;
    lines.extend(if read.is_empty() {
        strs(&["- Nothing judged."])
    } else {
        read
    });
    Ok(lines)
}
pub(super) fn paid_capacity(
    plan: &Json,
    verdicts: &Verdicts,
    case_lines: &[Json],
    summary: &[(Json, Vec<Json>)],
) -> Result<Vec<String>, Error> {
    let what = if plan.get_or("must_have", &string("obudpst"))?.as_str() == Some("obudpst") {
        "as many Test Connections as OB-UDPST's Server on the same host; ADR 0008's counts, the targets, are aims"
    } else {
        "ADR 0008's counts, the targets; OB-UDPST's capacity is reported"
    };
    let mut lines = vec![
        "## Capacity".into(),
        String::new(),
        format!(
            "The most Test Connections of each shape the Server carried with every gate met, and at every smaller count. Each judged case is read at OB-UDPST's capacity (the floor: does udpstp carry as many? Where OB-UDPST's curve stops unjudged, as many as it might carry; where it carried none, the floor doesn't bind) and at its target (what carries ADR 0008's count); \"carries\" means the count passes and the count below it doesn't fail. What one Server must carry: {what}."
        ),
        String::new(),
    ];
    lines.extend(strs(&["| Case | Row | Direction | Target | udpstp | Prototype B | OB-UDPST (floor) | udpstp carries OB-UDPST's count | Target reading | Outcome |","|---|---:|---|---:|---|---|---|---|---|---|"]));
    let mut targets = IndexMap::new();
    let mut candidates = Vec::new();
    let mut pinned = Vec::new();
    for line in case_lines {
        let case = line.at(0)?;
        let info = line.at(1)?;
        let outcome = line.at(2)?;
        let caps = info.item("caps")?;
        targets.insert(v(case, "name")?, info.item("target")?.clone());
        let mut note = String::new();
        if info.item("candidate")?.truthy() {
            note +=
                "; the per-core prototype carried at least 1.5 times as many: a design candidate";
            candidates.push(v(case, "name")?);
        }
        if info.item("pinned_only")?.truthy() {
            note +=
                "; only ADR 0008's pinned B carried the target: reported, nothing built from it";
            pinned.push(v(case, "name")?);
        }
        lines.push(format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {}: {}{note} |",
            v(case, "name")?,
            v(case, "row")?,
            v(case, "direction")?,
            v(case, "target")?,
            cap_text(caps.item("udpstp")?)?,
            cap_text(caps.item("prototype-b")?)?,
            cap_text(caps.item("obudpst")?)?,
            floor_text(info.item("floor")?, case)?,
            reading_text(info.item("target")?, case.item("target")?)?,
            display(outcome),
            decision_text(outcome)?
        ));
    }
    lines.extend(strs(&["", "## Outcome", ""]));
    let mut p1_failed = Vec::new();
    let mut ours_hold = true;
    for (k, value) in verdicts {
        if k.0 == "p1" && !field_eq(value, "verdict", "passes")? {
            p1_failed.push(format!("{} row {}", k.5, k.3));
            if k.5 == "udpstp" {
                ours_hold = false;
            }
        }
    }
    p1_failed.sort();
    let judged = summary
        .iter()
        .filter(|(k, _)| k.as_str() != Some("report"))
        .collect::<Vec<_>>();
    if !judged.is_empty() && judged.iter().all(|(k, _)| eq(k, &Json::Int(1))) {
        lines.push(if ours_hold{format!("- **1:** {}.",rule_text(&Json::Int(1))?)}else{"- **1** only if P1 holds, and udpstp's didn't at every row: back to the maintainer with the data.".into()});
    } else {
        for number in [
            Json::Int(2),
            Json::Int(4),
            Json::Int(3),
            string("aim"),
            string("i"),
            Json::Int(0),
            Json::Int(1),
        ] {
            if let Some((_, names)) = judged.iter().find(|(k, _)| eq(k, &number)) {
                let named = names
                    .iter()
                    .map(|name| {
                        if number.as_str() == Some("aim") {
                            Ok(format!(
                                "{} (target reading {})",
                                display(name),
                                reading_text(
                                    targets
                                        .get(text(name)?)
                                        .ok_or_else(|| Error("missing case target".into()))?,
                                    &Json::Null
                                )?
                            ))
                        } else {
                            Ok(display(name))
                        }
                    })
                    .collect::<Result<Vec<_>, Error>>()?
                    .join(", ");
                let rule = if number == Json::Int(1) {
                    "nothing to build for it"
                } else {
                    rule_text(&number)?
                };
                lines.push(format!(
                    "- **{}** ({named}): {}: {rule}.",
                    display(&number),
                    decision_text(&number)?
                ));
            }
        }
    }
    if !candidates.is_empty() {
        lines.push(format!("- The per-core prototype carried at least 1.5 times udpstp's count ({}): back to the maintainer as a design candidate.",candidates.join(", ")));
    }
    if !pinned.is_empty() {
        lines.push(format!("- Only ADR 0008's pinned B carried the target ({}): reported; it runs as a reference, and nothing is built from it.",pinned.join(", ")));
    }
    if let Some((_, names)) = summary.iter().find(|(k, _)| k.as_str() == Some("report")) {
        lines.push(format!("- Reported, not judged: {}.", join(names, ", ")?));
    }
    if !p1_failed.is_empty() {
        lines.push(format!(
            "- P1 didn't pass ({}): back to the maintainer with the data.",
            p1_failed.join(", ")
        ));
    }
    Ok(lines)
}
