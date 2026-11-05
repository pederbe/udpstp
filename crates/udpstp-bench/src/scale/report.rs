//! The Server-scale report, byte for byte the frozen Python's report.
//! Its file adapter only gathers evidence; all report decisions are pure.

mod capacity;
mod context;
mod experiments;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use bench_rounds::num::{self, Num};
use bench_rounds::py::{self, EMPTY_DICT, EMPTY_LIST, Json};
use indexmap::IndexMap;

use super::Error;
use super::judge::{
    self, RunFiles, Verdicts, array, display, eq, n, number, numeric, optional_num,
};
use super::matrix::{dict, integer, list, string, text};
use super::rules::*;
use capacity::*;
use context::*;
use experiments::*;

/// The Round's evidence, all relative to its output directory.
#[derive(Clone, Debug)]
pub struct RoundFiles {
    pub name: String,
    pub files: BTreeMap<String, Vec<u8>>,
}
impl RoundFiles {
    pub fn from_files(name: &str, files: BTreeMap<String, Vec<u8>>) -> Self {
        Self {
            name: name.into(),
            files,
        }
    }
    pub fn load(out: &Path) -> Result<Self, Error> {
        let name = crate::first_round::report::round_name(
            out.to_str()
                .ok_or_else(|| Error("a Round's path is not UTF-8".into()))?,
        );
        let mut files = BTreeMap::new();
        for path in [
            "binaries.json",
            "env-server.txt",
            "env-client1.txt",
            "env-client2.txt",
            "home-lab.txt",
            "scale/matrix.json",
            "scale/stamps.json",
            "scale/as-found.json",
            "scale/host-state.json",
            "scale/confinement.json",
        ] {
            if let Ok(bytes) = fs::read(out.join(path)) {
                files.insert(path.into(), bytes);
            }
        }
        if let Ok(entries) = fs::read_dir(out.join("scale")) {
            for entry in entries.flatten() {
                if fs::symlink_metadata(entry.path().join("meta.json")).is_err() {
                    continue;
                }
                let label = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| Error("a Round Run's name is not UTF-8".into()))?;
                if label.starts_with('.') {
                    continue;
                }
                for (path, bytes) in RunFiles::load(&entry.path())?.files {
                    files.insert(format!("scale/{label}/{path}"), bytes);
                }
                // glob selects an existing meta.json even if opening it
                // fails. Preserve that Run for the missing-meta judgment.
                files.entry(format!("scale/{label}/meta.json")).or_default();
                for path in [
                    "interrupts.server.before.txt",
                    "interrupts.server.after.txt",
                ] {
                    if let Ok(bytes) = fs::read(entry.path().join(path)) {
                        files.insert(format!("scale/{label}/{path}"), bytes);
                    }
                }
            }
        }
        Ok(Self { name, files })
    }
    fn json(&self, path: &str) -> Result<Json, Error> {
        super::evidence_json(self.files.get(path).map(Vec::as_slice), path)
    }
    fn text(&self, path: &str) -> Result<&str, Error> {
        self.files
            .get(path)
            .map(|b| std::str::from_utf8(b).map_err(|_| Error(format!("{path} is not UTF-8"))))
            .transpose()
            .map(|s| s.unwrap_or(""))
    }
    fn run(&self, label: &str) -> Result<RunFiles, Error> {
        if label.contains(['/', '\\', '\0']) || matches!(label, "." | "..") {
            return Err(Error("invalid Round Run label".into()));
        }
        let prefix = format!("scale/{label}/");
        Ok(RunFiles {
            files: self
                .files
                .iter()
                .filter_map(|(p, b)| p.strip_prefix(&prefix).map(|p| (p.into(), b.clone())))
                .collect(),
        })
    }
    fn analyse(&self) -> Result<(Json, Vec<Json>), Error> {
        let plan = self.json("scale/matrix.json")?;
        if !plan.truthy() {
            return Err(Error(
                "scale/matrix.json is missing: not a Server-scale round".into(),
            ));
        }
        let mut records = Vec::new();
        for path in self.files.keys() {
            if let Some(label) = path
                .strip_prefix("scale/")
                .and_then(|p| p.strip_suffix("/meta.json"))
                && !label.contains('/')
                && !label.starts_with('.')
            {
                records.push(judge::analyse(&self.run(label)?)?);
            }
        }
        Ok((plan, records))
    }
}

fn v(value: &Json, key: &str) -> Result<String, Error> {
    Ok(display(value.item(key)?))
}
fn object(value: &Json) -> Result<&IndexMap<String, Json>, Error> {
    value
        .as_dict()
        .ok_or_else(|| Error("expected a dict".into()))
}
fn required<'a>(value: &'a Json, key: &str) -> Result<&'a [Json], Error> {
    list(value.item(key)?)
}
fn default_list<'a>(value: &'a Json, key: &str) -> Result<&'a [Json], Error> {
    list(value.get_or(key, &EMPTY_LIST)?)
}
fn join(values: &[Json], separator: &str) -> Result<String, Error> {
    Ok(values
        .iter()
        .map(text)
        .collect::<Result<Vec<_>, _>>()?
        .join(separator))
}
fn strs(values: &[&str]) -> Vec<String> {
    values.iter().map(|s| (*s).into()).collect()
}
fn fmt(value: &Json, digits: usize, unit: &str) -> Result<String, Error> {
    Ok(num::fmt(numeric(value)?, digits, unit))
}
fn f(value: &Json, key: &str, digits: usize, unit: &str) -> Result<String, Error> {
    fmt(value.item(key)?, digits, unit)
}
fn field_eq(value: &Json, key: &str, expected: &str) -> Result<bool, Error> {
    Ok(value.get(key)?.as_str() == Some(expected))
}
fn cpus_named(cpus: &Json) -> String {
    let cpus = display(cpus);
    format!(
        "logical CPU{} {cpus}",
        if cpus.contains([',', '-']) { "s" } else { "" }
    )
}
fn chosen_cpus<'a>(chosen: &'a Json, case: &str) -> Result<&'a Json, Error> {
    match chosen.get(case)? {
        Json::Null => Ok(&Json::Null),
        other => Ok(other.at(0)?),
    }
}
fn cpus_or_question(chosen: &Json, case: &str) -> Result<Json, Error> {
    let cpus = chosen_cpus(chosen, case)?;
    Ok(if cpus.is_null() {
        string("?")
    } else {
        cpus.clone()
    })
}
const POINT_HEAD: [&str; 2] = [
    "| Test Connections | Engine | Valid runs | P2b not sent | P2c shortfall | P2a over-rate (receive side) | Loss | Delivered / planned | Incomplete / missing Sub-Intervals | Server CPUs busy | Other processes' CPUs | Wake-ups/s | Burst lateness p99, µs | Verdict |",
    "|---:|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|",
];
fn point_row(count: &Json, engine: &str, value: &Json) -> Result<String, Error> {
    let mut close = if value.get("close_call")?.truthy() {
        format!(
            " (close call: {})",
            join(required(value, "close_call")?, ", ")?
        )
    } else {
        String::new()
    };
    if value.get("why")?.truthy() {
        close += &format!(" ({})", v(value, "why")?);
    }
    let left = if value.get("left_out")?.truthy() {
        format!("; {} left out", v(value, "left_out")?)
    } else {
        String::new()
    };
    Ok(format!(
        "| {} | {} | {}/{} ({} planned, {} needed) | {} | {} | {} ({}) | {} | {} | {} / {}{left} | {} | {} | {} | {} | {}{close} |",
        display(count),
        engine_name(engine),
        v(value, "valid")?,
        v(value, "runs")?,
        v(value, "planned")?,
        v(value, "needed")?,
        f(value, "not_sent_pct", 2, " %")?,
        f(value, "short_median_pct", 2, " %")?,
        f(value, "over_pct", 2, " %")?,
        f(value, "over_raw_pct", 2, " %")?,
        f(value, "loss", 1, "")?,
        f(value, "delivered_pct", 2, " %")?,
        f(value, "incomplete", 1, "")?,
        f(value, "missing", 1, "")?,
        f(value, "server_busy", 2, "")?,
        f(value, "other_cpus", 2, "")?,
        f(value, "wakeups_per_s", 0, "")?,
        fmt(value.get("lateness_p99_us")?, 0, "")?,
        v(value, "verdict")?
    ))
}

pub fn report(out: &RoundFiles, first: Option<&RoundFiles>) -> Result<String, Error> {
    let (plan, records) = out.analyse()?;
    let verdicts = judge::points(&plan, &records)?;
    let mut lines = vec![format!("# Server-scale round {}", out.name), String::new()];
    for role in ["server", "client1", "client2"] {
        let path = format!("env-{role}.txt");
        if out.files.contains_key(&path) {
            let source = out.text(&path)?;
            let env = crate::first_round::report::environment(source)
                .map_err(|e| Error(e.to_string()))?;
            if !env.is_empty() {
                lines.push(format!("- **{role}:** {env}"));
            }
        }
    }
    let made = stamps(out)?;
    if !made.is_empty() {
        lines.extend(strs(&["", "What produced it:", ""]));
        lines.extend(made);
    }
    lines.extend(as_found_lines(out)?);
    lines.extend(host_state_lines(out)?);
    lines.extend(home_lab_lines(out)?);
    lines.extend(client_env_lines(&plan, &records)?);
    let counted = records
        .iter()
        .filter(|r| r.get("kind").is_ok_and(|v| v.as_str() != Some("probe")))
        .collect::<Vec<_>>();
    let mut valid = 0;
    for r in &counted {
        valid += usize::from(r.item("valid")?.truthy());
    }
    lines.extend([String::new(), format!("{} runs, {valid} valid. Gates: P2a over-rate per Sub-Interval ≤ +1 % (worst run; udpstp's and OB-UDPST's Servers Downstream on the Load Sender's clock); P2b Skipped and Withheld Datagrams ≤ 1 %, P2c the median Sub-Interval's shortfall ≤ 2 %, loss the Server host accounts for 0, every Test Connection completed with every Sub-Interval (median run), less those the path left out (their Control Phase got no answer and nothing says the Server saw them); a point needs half its planned runs valid, and at least 3. Flagged runs count whatever they showed; a point that fails only with them is platform-limited, except that where udpstp's or OB-UDPST's Server sends, a Client host short of room excuses only Test Connections that didn't complete: the Server's rates and what it didn't send come from its own clock and counts (the plan's Q0 (b) rule change 1).", counted.len()), String::new()]);
    let (case_lines, summary) = judge::outcomes(&plan, &verdicts)?;
    let chosen = confined_cpus(out, &plan, &records)?;
    lines.extend(if field_eq(&plan, "reading", "home-lab")? {
        home_lab_capacity(&plan, &case_lines, &chosen)?
    } else {
        paid_capacity(&plan, &verdicts, &case_lines, &summary)?
    });
    lines.extend(confinement_lines(&plan, &records, &chosen)?);
    let calibration = default_list(&plan, "calibration")?
        .iter()
        .map(text)
        .collect::<Result<Vec<_>, _>>()?;
    if !calibration.is_empty() {
        lines.extend(strs(&["", "## At the targets: the engines side by side", "", "ADR 0008's comparison on real hosts: the prototype's thread per Test Connection (A, the control) against its per-core engine as ADR 0008 prescribes it (one pinned Engine Thread per core) and as a pool would run it (one unpinned Engine Thread per logical CPU), beside udpstp and OB-UDPST.", ""]));
        lines.extend(strs(&POINT_HEAD));
        for case in required(&plan, "cases")? {
            if !case.get_or("judged", &Json::Bool(true))?.truthy() {
                continue;
            }
            lines.push(format!(
                "| **{}** | | | | | | | | | | | | | |",
                v(case, "name")?
            ));
            for engine in CURVE_ENGINES.into_iter().chain(calibration.iter().copied()) {
                if let Some(value) = verdicts.get(&judge::curve_key(
                    case,
                    integer(case.item("target")?)?,
                    engine,
                )?) {
                    lines.push(point_row(case.item("target")?, engine, value)?);
                }
            }
        }
    }
    lines.extend(hold_off_lines(out, &plan, &records, &verdicts, &chosen)?);
    lines.extend(grouping_lines(
        &plan,
        &records,
        &verdicts,
        &chosen,
        &case_lines,
    )?);
    let first = first.map(RoundFiles::analyse).transpose()?;
    lines.extend(precision_lines(&plan, &records, first.as_ref())?);
    for case in required(&plan, "cases")? {
        let note = if case.get_or("judged", &Json::Bool(true))?.truthy() {
            ""
        } else {
            " (reported, not judged)"
        };
        let name = text(case.item("name")?)?;
        let mut place = if case.get("confined")?.truthy() {
            format!(
                ", on {}, the calibration step's (Server CPUs busy counts theirs)",
                cpus_named(&cpus_or_question(&chosen, name)?)
            )
        } else {
            String::new()
        };
        if case.get("confined_as")?.truthy() {
            let other = text(case.item("confined_as")?)?;
            place = format!(
                ", on {}, the calibration step's for {other} (Server CPUs busy counts theirs)",
                cpus_named(&cpus_or_question(&chosen, other)?)
            );
        }
        if case.get("server_env")?.truthy() {
            place += &format!(
                ", udpstp's Server with {}",
                env_words(case.item("server_env")?)?
            );
        }
        lines.extend([
            String::new(),
            format!(
                "## {name}: {}, row {}{place}{note}",
                v(case, "direction")?,
                v(case, "row")?
            ),
            String::new(),
        ]);
        lines.extend(strs(&POINT_HEAD));
        for count in required(case, "counts")? {
            for engine in CURVE_ENGINES.into_iter().chain(calibration.iter().copied()) {
                if let Some(value) = verdicts.get(&judge::curve_key(case, integer(count)?, engine)?)
                {
                    lines.push(point_row(count, engine, value)?);
                }
            }
        }
        if name == "W5" {
            let mut shown = Vec::new();
            for count in required(case, "counts")? {
                if let Some(value) =
                    verdicts.get(&judge::curve_key(case, integer(count)?, "udpstp")?)
                {
                    shown.push(format!(
                        "{}: {}",
                        display(count),
                        f(value, "server_busy", 2, "")?
                    ));
                }
            }
            lines.extend([String::new(), format!("P5's limit for W5, 6 logical CPUs, reported, not gated; udpstp's Server CPUs busy by count: {}.", if shown.is_empty() { "–".into() } else { shown.join(", ") })]);
        }
    }
    let mut p1: Vec<_> = verdicts.iter().filter(|(k, _)| k.0 == "p1").collect();
    p1.sort_by_key(|(k, _)| *k);
    if !p1.is_empty() {
        lines.extend([String::new(), "## P1: lpduTime to departure".into(), String::new(), format!("At {} Test Connections, the Server host sending; µs, the median run. Gates: p99 ≤ 100, p99.9 ≤ 500, the worst Test Connection's p99 ≤ 200. udpstp's from its kernel send timestamps (the median Test Connection's p99; no p99.9), the prototype's from its own.", p1[0].0.4), String::new()]);
        lines.extend(strs(&["| Row | Engine | Valid runs | p99 | p99.9 | Worst Test Connection's p99 | Max | Verdict |", "|---:|---|---|---:|---:|---:|---:|---|"]));
        for (key, value) in p1 {
            lines.push(format!(
                "| {} | {} | {}/{} | {} | {} | {} | {} | {} |",
                key.3,
                engine_name(&key.5),
                v(value, "valid")?,
                v(value, "runs")?,
                f(value, "p99_us", 1, "")?,
                f(value, "p999_us", 1, "")?,
                f(value, "worst_us", 1, "")?,
                f(value, "max_us", 1, "")?,
                v(value, "verdict")?
            ));
        }
    }
    let mut probe = Vec::new();
    for r in &records {
        if field_eq(r, "kind", "probe")? {
            probe.push(r);
        }
    }
    if !probe.is_empty() {
        lines.extend(strs(&["", "## The early probe", "", "Each case's top count once per engine, right after the path check; not scored. The Load PDUs the path lost that no host accounts for say how often such loss voids a run on these hosts.", ""]));
        for r in probe {
            let state = if r.get("meets")?.truthy() {
                "met"
            } else {
                "failed"
            };
            let flags = if r.get("flags")?.truthy() {
                format!("; {}", join(array(r, "flags")?, "; ")?)
            } else {
                String::new()
            };
            let void = if r.get("invalid")?.truthy() {
                format!("; invalid: {}", join(required(r, "invalid")?, "; ")?)
            } else {
                String::new()
            };
            let lost = judge::path_loss_text(r)?;
            let lost = if lost.truthy() {
                format!("; {}", display(&lost))
            } else {
                String::new()
            };
            let unanswered = if r.get("unanswered")?.truthy() {
                format!(
                    "; {} of {} Test Connections got no answer",
                    v(r, "unanswered")?,
                    v(r, "count")?
                )
            } else {
                String::new()
            };
            lines.push(format!(
                "- `{}`: {state} the gates{flags}{void}{lost}{unanswered}",
                v(r, "label")?
            ));
        }
    }
    lines.extend(unanswered_lines(&records)?);
    lines.extend(throttle_lines(&records)?);
    let mut flagged = Vec::new();
    let mut invalid = Vec::new();
    for r in counted {
        if !r.item("valid")?.truthy() {
            invalid.push(r);
        } else if r.get("flags")?.truthy() {
            flagged.push(r);
        }
    }
    if !flagged.is_empty() {
        lines.extend(strs(&["", "## Flagged runs", "", "Each counted whatever it showed; a point that fails only with them is platform-limited, but where udpstp's or OB-UDPST's Server sends, a Client host short of room excuses only Test Connections that didn't complete:", ""]));
        for r in flagged {
            lines.push(format!(
                "- `{}`: {} the gates; {}",
                v(r, "label")?,
                if r.get("meets")?.truthy() {
                    "met"
                } else {
                    "failed"
                },
                join(required(r, "flags")?, "; ")?
            ));
        }
    }
    if !invalid.is_empty() {
        lines.extend(strs(&[
            "",
            "## Invalid runs",
            "",
            "Not scored; each first run of a point was run again once.",
            "",
        ]));
        for r in invalid {
            lines.push(format!(
                "- `{}`: {}",
                v(r, "label")?,
                join(required(r, "invalid")?, "; ")?
            ));
        }
    }
    Ok(lines.join("\n") + "\n")
}
