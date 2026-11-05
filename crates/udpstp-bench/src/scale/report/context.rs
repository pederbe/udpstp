use super::*;

pub(super) fn stamps(out: &RoundFiles) -> Result<Vec<String>, Error> {
    let mut lines = Vec::new();
    for (path, suffix) in [("scale/stamps.json", ""), ("binaries.json", " SHA-256")] {
        let found = out.json(path)?;
        let mut items = object(found.or(&EMPTY_DICT))?.iter().collect::<Vec<_>>();
        items.sort_by_key(|(k, _)| *k);
        for (name, value) in items {
            lines.push(format!("- {name}{suffix}: `{}`", display(value)));
        }
    }
    Ok(lines)
}
fn host_order(hosts: &IndexMap<String, Json>) -> Vec<&String> {
    let mut roles = hosts.keys().collect::<Vec<_>>();
    roles.sort_by_key(|r| (*r != "server", *r));
    roles
}
pub(super) fn as_found_lines(out: &RoundFiles) -> Result<Vec<String>, Error> {
    let found = out.json("scale/as-found.json")?;
    if !found.truthy() {
        return Ok(Vec::new());
    }
    let mut lines = strs(&[
        "",
        "**The hosts as found** (`--as-found`): the round changed neither the NIC's interrupts nor the socket-buffer limits, which the plan sets on rented hosts (irqbalance stopped, the NIC's queue interrupts placed, the limits raised to 16 MiB); it ran with them as they were:",
        "",
    ]);
    let hosts = object(found.get("hosts")?.or(&EMPTY_DICT))?;
    for role in host_order(hosts) {
        let host = &hosts[role];
        let mut irqs = object(host.get_or("interrupts", &EMPTY_DICT)?)?
            .iter()
            .map(|(irq, value)| {
                Ok((
                    irq.parse::<i128>()
                        .map_err(|_| Error("invalid IRQ number".into()))?,
                    irq,
                    value,
                ))
            })
            .collect::<Result<Vec<_>, Error>>()?;
        irqs.sort_by_key(|(n, _, _)| *n);
        let irqs = irqs
            .into_iter()
            .map(|(_, irq, value)| {
                let cpus = required(value, "cpus")?
                    .iter()
                    .map(display)
                    .collect::<Vec<_>>()
                    .join(",");
                Ok(format!(
                    "{irq} ({}) on logical CPU {}",
                    v(value, "name")?,
                    if cpus.is_empty() { "?" } else { &cpus }
                ))
            })
            .collect::<Result<Vec<_>, Error>>()?
            .join(", ");
        lines.push(format!("- {role}: socket-buffer limits {} bytes to receive, {} to send; irqbalance {}; the NIC's interrupts: {}",fmt(host.get("rmem_max")?,1,"")?,fmt(host.get("wmem_max")?,1,"")?,display(host.get("irqbalance")?.or(&string("unknown"))),if irqs.is_empty(){"none found"}else{&irqs}));
    }
    if found.get("obudpst_cpus")?.truthy() {
        lines.push(format!("- OB-UDPST's instances ran on logical CPUs {} (taskset): those the Server's NIC's interrupts don't take, as found.",required(&found,"obudpst_cpus")?.iter().map(display).collect::<Vec<_>>().join(",")));
    }
    Ok(lines)
}
fn state_items(state: &Json) -> Result<Vec<(&'static str, Json)>, Error> {
    let mut powers = Vec::new();
    for power in array(state, "power")? {
        let p = list(power)?;
        if p.len() != 4 {
            return Err(Error("a power reading has four fields".into()));
        }
        let status = display(&p[3]);
        powers.push(format!(
            "{} ({}) online {}{}",
            display(&p[0]),
            display(&p[1]),
            display(&p[2]),
            if matches!(status.as_str(), "-" | "") {
                String::new()
            } else {
                format!(" {status}")
            }
        ));
    }
    let turbo = state.get("no_turbo")?;
    if matches!(turbo, Json::List(_) | Json::Dict(_)) {
        return Err(Error("unhashable turbo setting".into()));
    }
    let turbo = match turbo.as_str() {
        Some("0") => string("on"),
        Some("1") => string("off"),
        _ => turbo.clone(),
    };
    Ok(vec![
        ("power", Json::Str(powers.join(", "))),
        ("governor", state.get("governor")?.clone()),
        ("driver", state.get("driver")?.clone()),
        ("energy preference", state.get("epp")?.clone()),
        ("turbo", turbo),
        ("idle driver", state.get("idle_driver")?.clone()),
        ("idle governor", state.get("idle_governor")?.clone()),
        ("kernel", state.get("kernel")?.clone()),
        ("preemption", state.get("preempt")?.clone()),
        ("HZ", state.get("hz")?.clone()),
        ("scheduler", state.get("scheduler")?.clone()),
        ("sched_ext", state.get("sched_ext")?.clone()),
        ("interfaces", Json::Str(join(array(state, "links")?, "; ")?)),
        ("Wi-Fi radio", state.get("wifi")?.clone()),
    ])
}
fn state_text(state: &Json, before: Option<&Json>) -> Result<String, Error> {
    let old = before
        .map(state_items)
        .transpose()?
        .unwrap_or_default()
        .into_iter()
        .collect::<BTreeMap<_, _>>();
    let mut parts = Vec::new();
    for (what, value) in state_items(state)? {
        if value.truthy()
            && (before.is_none() || !same_value(old.get(what).unwrap_or(&Json::Null), &value))
        {
            parts.push(format!("{what} {}", display(&value)));
        }
    }
    for name in THROTTLES {
        if let Some(value) = object(state)?.get(name) {
            let rose = before
                .filter(|b| b.truthy())
                .and_then(|b| b.as_dict())
                .and_then(|b| b.get(name))
                .map(|b| judge::sub(number(value)?, number(b)?))
                .transpose()?;
            let changed =
                rose.is_some_and(|n| n.py_cmp(Num::Int(0)) != Some(std::cmp::Ordering::Equal));
            if before.is_none() || changed {
                let rise = rose
                    .map(judge::num_json)
                    .map(|v| display(&v))
                    .unwrap_or_default();
                parts.push(format!(
                    "{}s {}{}",
                    name.replace('_', " "),
                    display(value),
                    if changed {
                        format!(" (+{rise})")
                    } else {
                        String::new()
                    }
                ));
            }
        }
    }
    Ok(parts.join("; "))
}
pub(super) fn host_state_lines(out: &RoundFiles) -> Result<Vec<String>, Error> {
    let states = out.json("scale/host-state.json")?;
    let states = states.or(&EMPTY_DICT);
    let start = states.get("start")?.or(&EMPTY_DICT);
    let end = states.get("end")?.or(&EMPTY_DICT);
    if !start.truthy() {
        return Ok(Vec::new());
    }
    let mut lines = strs(&[
        "",
        "**The hosts' state** at the round's start, and what had changed by its end:",
        "",
    ]);
    for role in host_order(object(start)?) {
        let changed = if let Some(e) = object(end)?.get(role) {
            state_text(e, Some(start.item(role)?))?
        } else {
            "not read".into()
        };
        let before = state_text(start.item(role)?, None)?;
        lines.push(format!(
            "- {role}: {}; at the end: {}",
            if before.is_empty() {
                "nothing read"
            } else {
                &before
            },
            if changed.is_empty() {
                "unchanged"
            } else {
                &changed
            }
        ));
    }
    Ok(lines)
}
pub(super) fn home_lab_lines(out: &RoundFiles) -> Result<Vec<String>, Error> {
    if !out.files.contains_key("home-lab.txt") {
        return Ok(Vec::new());
    }
    let mut lines = strs(&["", "**Recorded by `scripts/bench/home-lab.sh`:**", ""]);
    for line in py::file_lines(out.text("home-lab.txt")?) {
        if !py::strip(line).is_empty() {
            lines.push(format!("- {}", line.trim_end_matches(py::is_space)));
        }
    }
    Ok(lines)
}
pub(super) fn env_words(env: &Json) -> Result<String, Error> {
    let mut items = object(env.or(&EMPTY_DICT))?.iter().collect::<Vec<_>>();
    items.sort_by_key(|(k, _)| *k);
    Ok(items
        .into_iter()
        .map(|(name, value)| format!("`{name}={}`", display(value)))
        .collect::<Vec<_>>()
        .join(", "))
}
pub(super) fn client_env_lines(plan: &Json, records: &[Json]) -> Result<Vec<String>, Error> {
    let env = plan.get("client_env")?.or(&EMPTY_DICT);
    let mut other = Vec::new();
    for r in records {
        if matches!(r.get("engine")?.as_str(), Some("udpstp" | "obudpst"))
            && !field_eq(r, "kind", "p1")?
            && !same_value(r.get("client_env")?.or(&EMPTY_DICT), env)
        {
            other.push(format!("`{}`", v(r, "label")?));
        }
    }
    if !env.truthy() && other.is_empty() {
        return Ok(Vec::new());
    }
    let why = if let Some(value) = object(env)?.get("UDPSTP_ENGINE_HOLD_OFF_US") {
        format!(
            ": as Load Receivers they hold off {} µs after every turn that read, as before conditional receive hold-off, so that the measuring side works as it did when the dry runs and the stagger measurement tried it",
            display(value)
        )
    } else {
        String::new()
    };
    let words = env_words(env)?;
    let mut lines = vec![
        String::new(),
        format!(
            "**udpstp's Clients**, which measure both Servers, ran with {} on the Client hosts, as each run's meta.json records{why}.",
            if words.is_empty() {
                "nothing set"
            } else {
                &words
            }
        ),
    ];
    if !other.is_empty() {
        lines.push(format!(
            "Runs whose meta.json says otherwise: {}.",
            other.join(", ")
        ));
    }
    Ok(lines)
}
pub(super) fn confined_cpus(
    out: &RoundFiles,
    plan: &Json,
    records: &[Json],
) -> Result<Json, Error> {
    let found = out.json("scale/confinement.json")?;
    if found.truthy() {
        return Ok(Json::Dict(
            object(&found)?
                .iter()
                .map(|(case, value)| {
                    Ok((
                        case.clone(),
                        Json::List(vec![
                            value.item("cpus")?.clone(),
                            value.get_or("why", &string(""))?.clone(),
                        ]),
                    ))
                })
                .collect::<Result<_, Error>>()?,
        ));
    }
    judge::confinement(plan, &judge::confinement_verdicts(plan, records)?)
}
pub(super) fn confinement_lines(
    plan: &Json,
    records: &[Json],
    chosen: &Json,
) -> Result<Vec<String>, Error> {
    let step = plan.get("confine")?.or(&EMPTY_DICT);
    let verdicts = judge::confinement_verdicts(plan, records)?;
    if !step.truthy() {
        return Ok(Vec::new());
    }
    let runs = step.get("runs")?;
    let mut lines = vec![
        String::new(),
        "## The calibration step".into(),
        String::new(),
        format!(
            "udpstp alone at {} Test Connections on each candidate set of logical CPUs in turn ({}), {} run{} each, before any other engine ran. Each confined case runs, every engine alike, on the first candidate where udpstp fails its point, or on the last, the smallest, where it fails on none; the NIC's interrupts and the sampler stay outside. Server CPUs busy counts the candidate's.",
            display(step.get("count")?),
            join(default_list(step, "candidates")?, "; ")?,
            display(runs),
            if eq(runs, &Json::Int(1)) { "" } else { "s" }
        ),
        String::new(),
    ];
    lines.extend(strs(&POINT_HEAD));
    for case in required(plan, "cases")? {
        for candidate in default_list(step, "candidates")? {
            let value = verdicts
                .get_or(text(case.item("name")?)?, &EMPTY_DICT)?
                .get(text(candidate)?)?;
            if value.truthy() {
                lines.push(point_row(
                    step.get("count")?,
                    &format!("udpstp, {}, {}", v(case, "name")?, cpus_named(candidate)),
                    value,
                )?);
            }
        }
    }
    lines.push(String::new());
    for case in required(plan, "cases")? {
        let name = v(case, "name")?;
        if case.get("confined")?.truthy() {
            let found = chosen.get(&name)?;
            let (cpus, why) = if found.is_null() {
                (Json::Null, string("the calibration step didn't end"))
            } else {
                (found.at(0)?.clone(), found.at(1)?.clone())
            };
            lines.push(if cpus.truthy() {
                format!("- {name} ran on {}: {}.", cpus_named(&cpus), display(&why))
            } else {
                format!("- {name}: no logical CPUs chosen: {}.", display(&why))
            });
        } else if case.get("confined_as")?.truthy() {
            let other = v(case, "confined_as")?;
            let cpus = chosen_cpus(chosen, &other)?;
            lines.push(if cpus.truthy() {
                format!("- {name} ran on {}, {other}'s.", cpus_named(cpus))
            } else {
                format!("- {name}: no logical CPUs chosen: {other} has none.")
            });
        }
    }
    Ok(lines)
}
pub(super) fn unanswered_lines(records: &[Json]) -> Result<Vec<String>, Error> {
    let mut runs = Vec::new();
    let mut ran = BTreeMap::<(String, String), (i64, i64, i64, BTreeMap<String, i64>)>::new();
    for r in records {
        if r.get("unanswered")?.truthy() {
            runs.push(r);
        }
        if r.get("count")?.truthy()
            && matches!(r.get("engine")?.as_str(), Some("udpstp" | "obudpst"))
            && !field_eq(r, "kind", "p1")?
        {
            let total = ran.entry((v(r, "case")?, v(r, "engine")?)).or_default();
            total.0 += 1;
            total.1 += integer(r.item("count")?)?;
            total.2 += integer(r.get("unanswered")?.or(&Json::Int(0)))?;
            for (k, value) in object(r.get("unanswered_by")?.or(&EMPTY_DICT))? {
                *total.3.entry(k.clone()).or_default() += integer(value)?;
            }
        }
    }
    if runs.is_empty() {
        return Ok(Vec::new());
    }
    let mut lines = strs(&[
        "",
        "## Test Connections that got no answer",
        "",
        "A Test Connection whose Control Phase got no answer, while the Server host's receive counters didn't rise and udpstp's Server logged no Silent Rejection (OB-UDPST's log isn't read), never reached the Server, or its answer never reached the Client: the path's, left out of its run's gates. Otherwise it counts against the Server, as incomplete. A run that leaves out more than 5 % of its Test Connections is void. Over each case's runs, the probe's and the calibration step's among them:",
        "",
    ]);
    for ((case, engine), (n, count, lost, by)) in ran {
        let exchanges = by
            .iter()
            .map(|(k, v)| format!("{k} {v}"))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!(
            "- {case}, {}: {lost} of {count} Test Connections over {n} runs ({} %){}",
            engine_name(&engine),
            num::fixed(100.0 * lost as f64 / count as f64, 2),
            if exchanges.is_empty() {
                String::new()
            } else {
                format!(": {exchanges}")
            }
        ));
    }
    lines.extend(strs(&["", "In each run:", ""]));
    for r in runs {
        let mut by = object(r.get("unanswered_by")?.or(&EMPTY_DICT))?
            .iter()
            .collect::<Vec<_>>();
        by.sort_by_key(|(k, _)| *k);
        let exchanges = by
            .into_iter()
            .map(|(k, v)| format!("{k} {}", display(v)))
            .collect::<Vec<_>>()
            .join(", ");
        let how = if r.get("left_out")?.truthy() {
            "left out".into()
        } else if r.get("server_receive_drops")?.truthy() {
            format!(
                "counted against the Server: its host's receive counters rose by {}",
                v(r, "server_receive_drops")?
            )
        } else if r.get("silent_rejections")?.truthy() {
            format!(
                "counted against the Server: it logged {} Silent Rejections",
                v(r, "silent_rejections")?
            )
        } else {
            "counted against the Server".into()
        };
        lines.push(format!(
            "- `{}`: {} of {} ({exchanges}): {how}",
            v(r, "label")?,
            v(r, "unanswered")?,
            v(r, "count")?
        ));
    }
    Ok(lines)
}
pub(super) fn throttle_lines(records: &[Json]) -> Result<Vec<String>, Error> {
    let mut hit = BTreeMap::<String, [i128; 4]>::new();
    for r in records {
        if r.get("engine")?.truthy()
            && matches!(r.get("kind")?.as_str(), Some("curve" | "probe" | "confine"))
        {
            let seen = hit.entry(v(r, "engine")?).or_default();
            seen[0] += 1;
            let rose = r.get("server_throttles")?.or(&EMPTY_DICT);
            if rose.truthy() {
                seen[1] += 1;
                seen[2] += i128::from(integer(rose.get_or("core_throttle", &Json::Int(0))?)?);
                seen[3] += i128::from(integer(rose.get_or("package_throttle", &Json::Int(0))?)?);
            }
        }
    }
    if !hit.values().any(|seen| seen[1] != 0) {
        return Ok(Vec::new());
    }
    let mut lines = strs(&[
        "",
        "## The Server host's CPUs throttled",
        "",
        "Runs during which the Server host's thermal throttling counted (round.py's THROTTLES), by engine: recorded, not judged.",
        "",
    ]);
    for (engine, seen) in hit {
        lines.push(format!(
            "- {}: {} of {} runs; core throttles +{}, package throttles +{} in all",
            engine_name(&engine),
            seen[1],
            seen[0],
            seen[2],
            seen[3]
        ));
    }
    Ok(lines)
}
