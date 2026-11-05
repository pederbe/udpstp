//! The first Round's report: per Test, udpstp's and
//! OB-UDPST's figures side by side over the Round's repeats, the
//! criterion's verdicts, and the Round Runs the platform limited, as
//! Markdown, byte for byte `scripts/bench/report.py`'s for legacy records.
//! Explicit Server caps remain separate experimental arms, with advisory
//! requested, allowed and sampled thread counts and no default OB criterion.
//!
//! Each cell is the median over the repeats, with the range. Tests that
//! didn't complete are counted, not averaged in. A Round Run during which
//! the platform limited a host is flagged: the Round reads each host's
//! counters before and after every Test, and on AWS the ENA's
//! `*_allowance_exceeded` counters count the packets the platform queued
//! or dropped because the instance went over an allowance. A Search
//! Test's flagged Round Runs still count, and the verdict says how many
//! there were; a Fixed-Rate Test's are void, since the precision gates
//! judge the pacing, which a shaper queueing the packets distorts.
//!
//! The report reads the Round's files through [`RoundFiles`]: its `--out`
//! on disk ([`OutDir`]), or a test's own. It reads the files the Python
//! reads, when the Python reads them, and none else; where it can't write
//! the Python's report byte for byte, it writes none ([`Unreadable`]).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use bench_rounds::counters::{self, Counters};
use bench_rounds::num::{self, Num};
use bench_rounds::py;
use indexmap::IndexMap;

use super::judge::{self, Counted, Figures, Record, Unreadable};
use super::sender_evidence;

/// What a Round left in its `--out`, as the report reads it.
pub trait RoundFiles {
    /// The Round's name: its `--out`'s last part ([`round_name`]).
    fn name(&self) -> &str;

    /// A file of the Round's, by its path in `--out` (`summary.jsonl`,
    /// `tests/NAME`): its bytes, or `None` where it can't be read, as
    /// Python's `open` fails with an `OSError`.
    fn read(&self, path: &str) -> Option<Vec<u8>>;

    /// The names in `tests/`, in order, or `None` where it can't be listed.
    fn tests(&self) -> Option<Result<Vec<String>, Unreadable>>;
}

/// A Round's `--out` on disk.
#[derive(Clone, Debug)]
pub struct OutDir {
    path: PathBuf,
    name: String,
}

impl OutDir {
    /// The Round's `--out` at `out`, named as the Python names it.
    pub fn new(out: &Path) -> Result<OutDir, Unreadable> {
        let text = out.to_str().ok_or_else(|| {
            Unreadable::new(format!("{}: a path that isn't UTF-8", out.display()))
        })?;
        Ok(OutDir {
            path: out.to_path_buf(),
            name: round_name(text),
        })
    }
}

impl RoundFiles for OutDir {
    fn name(&self) -> &str {
        &self.name
    }

    fn read(&self, path: &str) -> Option<Vec<u8>> {
        fs::read(self.path.join(path)).ok()
    }

    fn tests(&self) -> Option<Result<Vec<String>, Unreadable>> {
        let mut names = Vec::new();
        for entry in fs::read_dir(self.path.join("tests")).ok()? {
            let name = entry.ok()?.file_name();
            match name.into_string() {
                Ok(name) => names.push(name),
                Err(name) => {
                    return Some(Err(Unreadable::new(format!(
                        "tests/{}: a name that isn't UTF-8",
                        name.to_string_lossy()
                    ))));
                }
            }
        }
        names.sort();
        Some(Ok(names))
    }
}

/// The Round's name, as the report's title gives it:
/// `os.path.basename(os.path.normpath(out))` on Linux. On Windows, `\`
/// separates too.
pub fn round_name(out: &str) -> String {
    #[cfg(windows)]
    let out = &out.replace('\\', "/");
    let normal = normpath(out);
    normal.rsplit('/').next().unwrap_or_default().to_string()
}

/// `posixpath.normpath`: `.` and empty parts dropped, `..` taking the part
/// before it where there is one, one or two leading slashes kept (three or
/// more are one), `.` for nothing.
fn normpath(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let initial = if path.starts_with("//") && !path.starts_with("///") {
        2
    } else {
        usize::from(path.starts_with('/'))
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part != ".." || (initial == 0 && parts.is_empty()) || parts.last() == Some(&"..") {
            parts.push(part);
        } else if !parts.is_empty() {
            parts.pop();
        }
    }
    let path = "/".repeat(initial) + &parts.join("/");
    if path.is_empty() {
        ".".to_string()
    } else {
        path
    }
}

/// A host's environment line from its `env-ROLE.txt` (`environment`): its
/// CPU's model, its kernel, and the MTU of the device it routes out of,
/// from `lscpu`, `uname -a` and `ip link show`.
pub fn environment(text: &str) -> Result<String, Unreadable> {
    let lines = py::splitlines(text);
    let kernel = if py::strip(text).is_empty() {
        "?"
    } else {
        py::split(lines[0]).get(2).copied().ok_or_else(|| {
            Unreadable::new("its first line, uname -a's, has fewer than three words")
        })?
    };
    let model = match lines.iter().find(|line| line.starts_with("Model name")) {
        None => "?",
        Some(line) => py::strip(
            line.split_once(':')
                .ok_or_else(|| Unreadable::new("its Model name line has no colon"))?
                .1,
        ),
    };
    // The word after the first `mtu` of the first line with a word after it.
    let mtu = lines.iter().find_map(|line| {
        let words = py::split(line);
        let at = words.iter().position(|w| *w == "mtu")?;
        words.get(at + 1).copied()
    });
    let mtu = mtu.map_or(String::new(), |mtu| format!(", MTU {mtu}"));
    Ok(format!("{model}, Linux {kernel}{mtu}"))
}

/// A cell of the report (`cell`): the median of the values given, to
/// `digits` decimals, with `unit`, and their range where there are
/// several; `–` for none.
pub fn cell(values: &[Option<Num>], unit: &str, digits: usize) -> String {
    let values: Vec<Num> = values.iter().flatten().copied().collect();
    let fixed = |n: Num| num::fixed(n.to_f64(), digits);
    let (Some(median), Some(least), Some(most)) =
        (num::median(&values), min(&values), num::max(&values))
    else {
        return "–".to_string();
    };
    if values.len() == 1 {
        format!("{}{unit}", fixed(median))
    } else {
        format!("{}{unit} ({}–{})", fixed(median), fixed(least), fixed(most))
    }
}

/// `min(values)`: the first of the smallest.
fn min(values: &[Num]) -> Option<Num> {
    let (first, rest) = values.split_first()?;
    let mut least = *first;
    for &v in rest {
        if v.py_cmp(least) == Some(std::cmp::Ordering::Less) {
            least = v;
        }
    }
    Some(least)
}

/// `100 * x`, of the kind Python's gives.
fn percent_of(x: Num) -> Num {
    match x {
        Num::Int(i) => Num::Int(i.saturating_mul(100)),
        Num::Float(f) => Num::Float(100.0 * f),
    }
}

/// `format(x, ".0%")`.
fn percent(x: f64) -> String {
    format!("{}%", num::fixed(x * 100.0, 0))
}

/// `format(x, f"+.{digits}f")`: a sign always, `+nan` for a NaN whatever
/// its sign, as Python writes one.
fn signed_fixed(x: f64, digits: usize) -> String {
    if x.is_nan() {
        return "+nan".to_string();
    }
    let sign = if x.is_sign_negative() { '-' } else { '+' };
    format!("{sign}{}", num::fixed(x.abs(), digits))
}

/// A file's bytes as text, which the Python reads as UTF-8 or refuses.
fn decoded(bytes: &[u8], path: &str) -> Result<String, Unreadable> {
    String::from_utf8(bytes.to_vec()).map_err(|_| Unreadable::new(format!("{path}: not UTF-8")))
}

/// What the report groups a Round's Round Runs by: one Test, in one
/// direction, run by one pair of engines.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Key {
    name: String,
    direction: String,
    rate: String,
    test_connections: i128,
    client_host: String,
    pair: String,
    server_engine_threads: Option<std::num::NonZeroUsize>,
}

impl Key {
    fn of(run: &Record) -> Key {
        Key {
            name: run.test_name(),
            direction: run.direction.clone(),
            rate: run.rate.clone(),
            test_connections: run.test_connections,
            client_host: run.client_host.clone(),
            pair: run.pair(),
            server_engine_threads: run.server_engine_threads,
        }
    }
}

/// A Round Run as a row of the report counts it.
struct Judged<'a> {
    run: &'a Record,
    figures: Option<Figures>,
    flagged: bool,
}

const TABLE: [&str; 2] = [
    "| Test | Direction | Rate | Test Connections | Client | Client/Server | Runs | Maximum | Highest | Loss Ratio at Highest | Send lag p99 | Platform-limited runs |",
    "|---|---|---|---:|---|---|---:|---|---|---|---|---:|",
];

/// The Round's report, retaining the Python's Markdown for legacy records,
/// or why it can't be written.
pub fn report(files: &dyn RoundFiles) -> Result<String, Unreadable> {
    let summary = files
        .read("summary.jsonl")
        .ok_or_else(|| Unreadable::new("summary.jsonl can't be read"))?;
    let runs = super::diagnostics::planned_runs(
        files,
        judge::read_summary(&decoded(&summary, "summary.jsonl")?)?,
    )?;
    let mut labels = std::collections::HashMap::new();
    for run in &runs {
        *labels.entry(run.label.as_str()).or_insert(0) += 1;
    }
    let mut lines = vec![format!("# Round {}", files.name()), String::new()];
    for role in ["server", "client"] {
        let file = format!("env-{role}.txt");
        if let Some(bytes) = files.read(&file) {
            let env = environment(&decoded(&bytes, &file)?).map_err(|e| e.at(&file))?;
            let role = role[..1].to_uppercase() + &role[1..];
            lines.push(format!("- **{role}:** {env}"));
        }
    }
    lines.push(String::new());
    lines.push(format!(
        "Maximum: the highest IP-Layer Capacity of a Sub-Interval losing at most {}; Highest: \
         of any Sub-Interval, with its Loss Ratio. Median over the repeats, range in brackets, \
         Mbit/s.",
        percent(judge::MAX_LOSS)
    ));
    lines.push(String::new());
    // The groups in order of their first label, as the Python sorts them:
    // stable, so ties stay in the order they came.
    let mut groups: IndexMap<Key, Vec<&Record>> = IndexMap::new();
    for run in &runs {
        groups.entry(Key::of(run)).or_default().push(run);
    }
    let mut groups: Vec<(Key, Vec<&Record>)> = groups.into_iter().collect();
    fn first_label<'a>(group: &[&'a Record]) -> Option<&'a str> {
        group.iter().map(|r| r.label.as_str()).min()
    }
    groups.sort_by(|a, b| first_label(&a.1).cmp(&first_label(&b.1)));
    lines.extend(TABLE.map(String::from));
    // `tests/`, as the Python lists it for each Round Run's counters.
    let listing = if runs.is_empty() {
        None
    } else {
        files.tests().transpose()?
    };
    let mut judged: IndexMap<Key, Vec<Counted>> = IndexMap::new();
    let mut limited = Vec::new();
    let (mut precision_rows, mut precision_missing) = (Vec::new(), Vec::new());
    for (key, group) in &groups {
        let mut each = Vec::with_capacity(group.len());
        for &run in group {
            let found = platform_limits(files, listing.as_deref(), &run.label)?;
            if !found.is_empty() {
                limited.push(format!(
                    "- `{}`: {}",
                    run.label,
                    counters::limits_text(&found)
                ));
            }
            let figures = if run.status == Some(0) {
                let file = format!("tests/{}.json", run.label);
                judge::read_figures(files.read(&file).as_deref()).map_err(|e| e.at(&file))?
            } else {
                None
            };
            each.push(Judged {
                run,
                figures,
                flagged: !found.is_empty(),
            });
        }
        lines.push(row(key, &each, &mut judged));
        if let Some(row) = precision_row(files, key, &each, &labels, &mut precision_missing) {
            precision_rows.push(row);
        }
    }
    if !precision_rows.is_empty() {
        lines.extend([
            "", "## Server precision", "",
            "Server sender measurements apply to Downstream precision Tests. Upstream uses the Client's send lag in the table above. Platform-limited Fixed-Rate Tests are excluded. Values are medians of connection p99s within each Round Run, then medians over Round Runs; they are not pooled percentiles. Runs with timing counts eligible evidence against all requested Runs.",
            "", "| Test | Direction | Rate | Test Connections | Client | Client/Server | Runs with timing | Server send lag p99 | Server Burst lateness p99 |",
            "|---|---|---|---:|---|---|---:|---|---|",
        ].map(String::from));
        lines.extend(precision_rows);
        if !precision_missing.is_empty() {
            lines.push(String::new());
            lines.extend(precision_missing);
        }
    }
    lines.extend(comparison(&groups, &judged)?);
    if !limited.is_empty() {
        lines.extend(
            [
                "",
                "## Runs the platform limited",
                "",
                "The platform's allowance counters that rose during the run, on each host (AWS \
                 queues, then drops, what goes over an instance's allowances). A Search Test's \
                 still count; a Fixed-Rate Test's are void:",
                "",
            ]
            .map(String::from),
        );
        lines.extend(limited);
    }
    lines.extend(super::diagnostics::host_lines(files, &runs, &labels));
    lines.extend(super::metrics::metric_lines(
        files,
        &runs,
        &labels,
        listing.as_deref(),
    )?);
    Ok(lines.join("\n") + "\n")
}

fn precision_row(
    files: &dyn RoundFiles,
    key: &Key,
    each: &[Judged],
    labels: &std::collections::HashMap<&str, usize>,
    missing: &mut Vec<String>,
) -> Option<String> {
    let requested: Vec<_> = each
        .iter()
        .filter(|e| e.run.kernel_send_timestamps && e.run.server == "udpstp")
        .collect();
    if requested.is_empty() {
        return None;
    }
    let prefix = format!(
        "| {} | {} | {} | {} | {} | {} |",
        key.name, key.direction, key.rate, key.test_connections, key.client_host, key.pair
    );
    if key.direction != "downstream" {
        return Some(prefix + " not applicable | not applicable | not applicable |");
    }
    let (mut lags, mut lateness) = (Vec::new(), Vec::new());
    for e in &requested {
        let run = e.run;
        if key.rate != "search" && e.flagged {
            missing.push(format!("- `{}`: void: the platform limited it", run.label));
            continue;
        }
        let timing = if labels.get(run.label.as_str()) != Some(&1) {
            Err("Server timing unavailable: duplicate Round Run label")
        } else if run.status != Some(0) {
            Err("Server timing unavailable: Client failed")
        } else {
            sender_evidence::timing(
                files
                    .read(&format!("tests/{}.server.json", run.label))
                    .as_deref(),
                run,
            )
        };
        match timing {
            Ok((lag, burst)) => {
                lags.push(Some(lag));
                lateness.push(Some(burst));
            }
            Err(why) => missing.push(format!("- `{}`: {why}", run.label)),
        }
    }
    let shown = if lags.is_empty() {
        ("unavailable".into(), "unavailable".into())
    } else {
        (cell(&lags, " µs", 0), cell(&lateness, " µs", 0))
    };
    Some(format!(
        "{prefix} {}/{} | {} | {} |",
        lags.len(),
        requested.len(),
        shown.0,
        shown.1
    ))
}

/// A Test's row, and the Round Runs the criterion counts of it into
/// `judged`.
fn row(key: &Key, each: &[Judged], judged: &mut IndexMap<Key, Vec<Counted>>) -> String {
    // A Fixed-Rate Test's flagged Round Runs are void: the pacing it
    // judges is what a shaper queueing the packets distorts. A Search
    // Test's count: the shaper is part of the path both engines measure.
    let counted: Vec<&Judged> = each
        .iter()
        .filter(|e| key.rate == "search" || !e.flagged)
        .collect();
    let void = each.len() - counted.len();
    judged.insert(
        key.clone(),
        counted
            .iter()
            .map(|e| Counted {
                maximum: e.figures.and_then(|f| f.maximum),
                flagged: e.flagged,
            })
            .collect(),
    );
    let done: Vec<&&Judged> = counted.iter().filter(|e| e.run.status == Some(0)).collect();
    let got: Vec<Figures> = done.iter().filter_map(|e| e.figures).collect();
    let failed = counted.len() - done.len();
    let mut runs = done.len().to_string();
    if failed > 0 {
        runs.push_str(&format!(" ({failed} failed)"));
    }
    if void > 0 {
        let them = if void > 1 { "them" } else { "it" };
        runs.push_str(&format!(" ({void} void: the platform limited {them})"));
    }
    let flagged = each.iter().filter(|e| e.flagged).count();
    let flagged = if flagged == 0 {
        "–".to_string()
    } else {
        flagged.to_string()
    };
    let column =
        |f: fn(&Figures) -> Option<Num>| -> Vec<Option<Num>> { got.iter().map(f).collect() };
    format!(
        "| {} | {} | {} | {} | {} | {} | {runs} | {} | {} | {} | {} | {flagged} |",
        key.name,
        key.direction,
        key.rate,
        key.test_connections,
        key.client_host,
        key.pair,
        cell(&column(|f| f.maximum), "", 1),
        cell(&column(|f| Some(f.highest)), "", 1),
        cell(&column(|f| Some(percent_of(f.highest_loss))), " %", 2),
        cell(&column(|f| f.lag_p99), " µs", 0),
    )
}

/// udpstp against OB-UDPST (`comparison`): each Search Test run by both
/// at both ends, their median Maxima and the criterion's verdict; nothing
/// where there is no such Test.
fn comparison(
    groups: &[(Key, Vec<&Record>)],
    judged: &IndexMap<Key, Vec<Counted>>,
) -> Result<Vec<String>, Unreadable> {
    let mut rows = Vec::new();
    for (key, _) in groups {
        if key.pair != "udpstp/udpstp"
            || key.rate != "search"
            || key.server_engine_threads.is_some()
        {
            continue;
        }
        let theirs_key = Key {
            pair: "obudpst/obudpst".to_string(),
            ..key.clone()
        };
        let (Some(ours), Some(theirs)) = (judged.get(key), judged.get(&theirs_key)) else {
            continue;
        };
        let c = judge::compare(ours, theirs, &key.client_host, key.test_connections)?;
        let shown = |v: Option<Num>| v.map_or("–".to_string(), |v| num::fixed(v.to_f64(), 1));
        let difference = c.difference.map_or("–".to_string(), |d| {
            format!("{} %", signed_fixed(100.0 * d, 1))
        });
        rows.push(format!(
            "| {} | {} | {} | {} | {}/{} · {}/{} | {} | {} | {difference} | {} |",
            key.name,
            key.direction,
            key.test_connections,
            key.client_host,
            c.ours.0,
            c.ours.1,
            c.theirs.0,
            c.theirs.1,
            shown(c.ours_median),
            shown(c.theirs_median),
            c.verdict,
        ));
    }
    if rows.is_empty() {
        return Ok(rows);
    }
    let mut lines: Vec<String> = ["", "## udpstp against OB-UDPST", ""]
        .map(String::from)
        .to_vec();
    lines.push(format!(
        "Median Maximum over the repeats, Mbit/s; runs with a Maximum, udpstp · OB-UDPST. The \
         criterion: within {} of OB-UDPST's for each single-connection Search Test between the \
         cloud hosts.",
        percent(judge::MAX_DIFFERENCE)
    ));
    lines.extend(
        [
            "",
            "| Test | Direction | Test Connections | Client | Runs | udpstp | OB-UDPST | Difference | Criterion |",
            "|---|---|---:|---|---|---:|---:|---:|---|",
        ]
        .map(String::from),
    );
    lines.extend(rows);
    Ok(lines)
}

/// How far each of the platform's allowance counters rose on each host
/// during the Round Run labelled `label` (`platform_limits`), from its
/// counter files in `tests/`, `LABEL.ROLE.before.txt` and `.after.txt`:
/// the files the Python reads, read as it reads them.
pub(super) fn platform_limits(
    files: &dyn RoundFiles,
    listing: Option<&[String]>,
    label: &str,
) -> Result<IndexMap<String, Counters>, Unreadable> {
    let Some(names) = listing else {
        return Ok(IndexMap::new());
    };
    let prefix = format!("{label}.");
    let mut texts: BTreeMap<String, Option<String>> = BTreeMap::new();
    for name in names {
        let Some(role) = name
            .strip_prefix(&prefix)
            .and_then(|rest| rest.strip_suffix(".before.txt"))
        else {
            continue;
        };
        if role.is_empty() || role.contains('.') {
            continue;
        }
        for file in [name.clone(), format!("{label}.{role}.after.txt")] {
            if texts.contains_key(&file) || names.binary_search(&file).is_err() {
                continue;
            }
            let path = format!("tests/{file}");
            let text = files
                .read(&path)
                .map(|bytes| decoded(&bytes, &path))
                .transpose()?;
            texts.insert(file, text);
        }
    }
    counters::platform_limits(label, &texts)
        .map_err(|e| Unreadable::new(e.to_string()).at(&format!("tests/{label}.*.txt")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Round's files in memory.
    #[derive(Default)]
    struct Files {
        files: BTreeMap<String, Vec<u8>>,
    }

    impl Files {
        fn with(self, path: &str, text: &str) -> Files {
            self.with_bytes(path, text.as_bytes())
        }

        fn with_bytes(mut self, path: &str, bytes: &[u8]) -> Files {
            self.files.insert(path.to_string(), bytes.to_vec());
            self
        }
    }

    impl RoundFiles for Files {
        fn name(&self) -> &str {
            "round-1"
        }

        fn read(&self, path: &str) -> Option<Vec<u8>> {
            self.files.get(path).cloned()
        }

        fn tests(&self) -> Option<Result<Vec<String>, Unreadable>> {
            let names: Vec<String> = self
                .files
                .keys()
                .filter_map(|p| p.strip_prefix("tests/"))
                .map(String::from)
                .collect();
            Some(Ok(names))
        }
    }

    #[test]
    fn downstream_precision_reports_the_servers_sender_timing() {
        // : Server sender evidence belongs to its individual Round Run.
        let files = Files::default()
            .with("summary.jsonl", r#"{"label":"001","status":0,"name":"M2","direction":"downstream","rate":500,"test_connections":1,"client_impl":"udpstp","server_impl":"udpstp","client_host":"cloud","repeat":1,"kernel_send_timestamps":true}"#)
            .with("tests/001.server.json", r#"{"label":"001","status":0,"log":"{\"event\":\"DataPhaseEnded\",\"test_connection_id\":1,\"reason\":\"Completed\",\"send_lag_samples\":100,\"send_lag_p99_us\":37,\"burst_lateness_p99_us\":82}\n"}"#);
        assert!(report(&files).unwrap().contains(
            "| M2 | downstream | 500 | 1 | cloud | udpstp/udpstp | 1/1 | 37 µs | 82 µs |"
        ));
    }

    #[test]
    fn capped_arms_keep_separate_medians_and_skip_the_default_ob_criterion() {
        // 's explicit caps are experimental arms, not default M1 acceptance.
        let mut files = round(&[
            ("001", "arm", "udpstp", "cloud", 1, Some(100.0), "search"),
            ("002", "arm", "udpstp", "cloud", 1, Some(200.0), "search"),
            ("003", "arm", "obudpst", "cloud", 1, Some(100.0), "search"),
        ]);
        let summary = String::from_utf8(files.read("summary.jsonl").unwrap()).unwrap();
        let changed = summary
            .lines()
            .enumerate()
            .map(|(i, line)| {
                let mut run: serde_json::Value = serde_json::from_str(line).unwrap();
                if i < 2 {
                    run["server_engine_threads"] = serde_json::json!([2, 4][i]);
                }
                run.to_string()
            })
            .collect::<Vec<_>>()
            .join("\n");
        files = files.with("summary.jsonl", &changed);
        let text = report(&files).unwrap();
        assert!(
            text.contains(
                "| arm (Server cap 2) | upstream | search | 1 | cloud | udpstp/udpstp | 1 | 100.0 |"
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "| arm (Server cap 4) | upstream | search | 1 | cloud | udpstp/udpstp | 1 | 200.0 |"
            ),
            "{text}"
        );
        assert!(!text.contains("## udpstp against OB-UDPST"), "{text}");
        let precision = changed
            .lines()
            .map(|line| {
                let mut run: serde_json::Value = serde_json::from_str(line).unwrap();
                run["rate"] = serde_json::json!(500);
                run["host_samples"] = serde_json::json!(true);
                run["kernel_send_timestamps"] = serde_json::json!(true);
                run.to_string()
            })
            .collect::<Vec<_>>()
            .join("\n");
        let text = report(&files.with("summary.jsonl", &precision)).unwrap();
        for cap in [2, 4] {
            assert!(
                text.contains(&format!(
                    "| arm (Server cap {cap}) | upstream | 500 | 0/1 | 0 | unavailable |"
                )),
                "{text}"
            );
        }
    }

    #[test]
    fn capped_server_reports_the_canonical_pool_startup_event() {
        // : the production Server log, named by ADR 0013, supplies readback.
        let summary = r#"{"label":"001","status":0,"name":"cap","direction":"downstream","rate":1065,"test_connections":4,"client_impl":"udpstp","server_impl":"udpstp","client_host":"cloud","repeat":1,"server_engine_threads":1}"#;
        let log = concat!(
            r#"{"timestamp":"2026-10-13T20:00:00Z","level":"INFO","message":"the Data Phase Engine runs on at most 1 Engine Threads","event":"DataPhaseEngines","executor":"pool","engine_threads":1,"grouping_us":0}"#,
            "\n"
        );
        let capture = serde_json::json!({"label":"001","status":0,"log":log});
        let files = Files::default()
            .with("summary.jsonl", summary)
            .with("tests/001.server.json", &capture.to_string());
        let text = report(&files).unwrap();
        assert!(text.contains("| 001 | 1 | 1 | unavailable |"), "{text}");
    }

    #[test]
    fn canonical_pool_startup_readback_refuses_invalid_or_duplicate_events() {
        // : canonical readback has the same evidence safeguards as old logs.
        let summary = r#"{"label":"001","status":0,"name":"cap","direction":"downstream","rate":1065,"test_connections":4,"client_impl":"udpstp","server_impl":"udpstp","client_host":"cloud","repeat":1,"server_engine_threads":1}"#;
        let startup = serde_json::json!({"event":"DataPhaseEngines",
            "executor":"pool","engine_threads":1,"grouping_us":0});
        let valid = format!("{startup}\n");
        let changed = |field: &str, value| {
            let mut event = startup.clone();
            event[field] = value;
            format!("{event}\n")
        };
        let mut legacy = startup.clone();
        legacy["event"] = serde_json::json!("data_phase_engines");
        for log in [
            changed("executor", serde_json::json!("per_test_connection")),
            changed("warning", serde_json::json!(true)),
            changed("error", serde_json::json!("CPU allowance unavailable")),
            changed("engine_threads", serde_json::json!(0)),
            changed("engine_threads", serde_json::json!(true)),
            startup.to_string(),
            format!("{valid}not JSON\n"),
            format!("{valid}{valid}"),
            format!("{valid}{legacy}\n"),
        ] {
            let capture = serde_json::json!({"label":"001","status":0,"log":log});
            let files = Files::default()
                .with("summary.jsonl", summary)
                .with("tests/001.server.json", &capture.to_string());
            let text = report(&files).unwrap();
            assert!(
                text.contains("| 001 | 1 | unavailable | unavailable |"),
                "{text}"
            );
        }
    }

    #[test]
    fn requested_allowed_and_sampled_engine_threads_are_distinct_evidence() {
        // : the requested maximum is never substituted for host readback.
        let summary = r#"{"label":"001","status":0,"name":"cap","direction":"upstream","rate":500,"test_connections":4,"client_impl":"udpstp","server_impl":"udpstp","client_host":"cloud","repeat":1,"host_samples":true,"server_engine_threads":2}"#;
        let samples = [1, 2]
            .map(|t| {
                serde_json::json!({"t":t,
            "cpus":{"0":[100+t*10,0,0,100+t*90,0,0,0,0]},
            "threads":{"42":["udpstp-eng-0",t*10,t*10,0]},"process":t*10})
                .to_string()
            })
            .join("\n");
        let vm = serde_json::json!({"label":"001","hosts":{"server":{
            "status":0,"window":[1,2],"samples":samples}}})
        .to_string();
        let base = || {
            Files::default()
                .with("summary.jsonl", summary)
                .with("tests/001.vm.json", &vm)
        };
        let startup = serde_json::json!({"label":"001","status":0,
            "log":"{\"event\":\"data_phase_engines\",\"executor\":\"pool\",\"engine_threads\":3,\"grouping_us\":0}\n"}).to_string();
        let text = report(&base().with("tests/001.server.json", &startup)).unwrap();
        assert!(text.contains("| 001 | 2 | 3 | 1 |"), "{text}");
        let missing = report(&base()).unwrap();
        assert!(
            missing.contains("| 001 | 2 | unavailable | 1 |"),
            "{missing}"
        );
        for log in [
            "{\"event\":\"data_phase_engines\",\"executor\":\"per-connection\",\"engine_threads\":3}\n",
            "{\"event\":\"data_phase_engines\",\"executor\":\"pool\",\"engine_threads\":3,\"warning\":true}\n",
            "{\"event\":\"data_phase_engines\",\"executor\":\"pool\",\"engine_threads\":3}",
            "not JSON\n",
        ] {
            let startup = serde_json::json!({"label":"001","status":0,"log":log}).to_string();
            let text = report(&base().with("tests/001.server.json", &startup)).unwrap();
            assert!(text.contains("| 001 | 2 | unavailable | 1 |"), "{text}");
        }
    }

    #[test]
    fn cap_plans_reconcile_the_requested_setting_and_refuse_unknown_versions() {
        // Version 2 makes older reporters refuse a schedule they would misgroup.
        let summary: serde_json::Value = serde_json::json!({"label":"001","status":0,
            "name":"cap","direction":"upstream","rate":500,"test_connections":4,
            "client_impl":"udpstp","server_impl":"udpstp","client_host":"cloud",
            "repeat":1,"test_interval":10,"server_engine_threads":2});
        let mut planned = summary.clone();
        planned["status"] = serde_json::Value::Null;
        let fixture = |version: serde_json::Value, cap| {
            let mut run = planned.clone();
            run["server_engine_threads"] = serde_json::json!(cap);
            Files::default()
                .with("summary.jsonl", &summary.to_string())
                .with(
                    "first-plan.json",
                    &serde_json::json!({"version":version,"runs":[run]}).to_string(),
                )
        };
        assert!(report(&fixture(serde_json::json!(2), 2)).is_ok());
        assert!(report(&fixture(serde_json::json!(2), 4)).is_err());
        for version in [
            serde_json::json!(0),
            serde_json::json!(3),
            serde_json::json!(true),
            serde_json::json!("2"),
        ] {
            assert!(report(&fixture(version, 2)).is_err());
        }
    }

    fn sender_files(log: &str, connections: u8) -> Files {
        Files::default()
            .with("summary.jsonl", &format!(r#"{{"label":"001","status":0,"name":"M2","direction":"downstream","rate":500,"test_connections":{connections},"test_interval":10,"client_impl":"udpstp","server_impl":"udpstp","client_host":"cloud","repeat":1,"host_samples":true,"kernel_send_timestamps":true}}"#))
            .with("tests/001.server.json", &serde_json::json!({"label": "001", "status": 0, "log": log}).to_string())
    }

    #[test]
    fn sender_timing_keeps_the_first_event_error() {
        // The port design §7.7 retains diagnostic precedence, even when a
        // later line is malformed. Parsing the whole log first changes it.
        let summary = r#"{"event":"TestSummary","result":"Timeout"}"#;
        for (log, why) in [
            (format!("{summary}\nnot JSON"), "mismatched Test summary"),
            (
                format!("not JSON\n{summary}"),
                "capture missing or unreadable",
            ),
            (
                format!("[]\n{summary}"),
                "invalid or incomplete connection evidence",
            ),
        ] {
            let text = report(&sender_files(&log, 1)).unwrap();
            assert!(
                text.contains(&format!("- `001`: Server timing unavailable: {why}")),
                "{text}"
            );
        }
    }

    #[test]
    fn connection_only_timing_does_not_supply_complete_sender_figures() {
        // The cloud testing plan's sender diagnostics require the complete
        // Test log. The timing section also accepts connection-only evidence.
        let log = [1, 2].map(|id| format!(r#"{{"event":"DataPhaseEnded","test_connection_id":{id},"reason":"Completed","send_lag_samples":100,"send_lag_p99_us":37,"burst_lateness_p99_us":82}}"#)).join("\n");
        for ending in ["", "\n"] {
            let text = report(&sender_files(&format!("{log}{ending}"), 2)).unwrap();
            assert!(
                text.contains(
                    "| M2 | downstream | 500 | 2 | cloud | udpstp/udpstp | 1/1 | 37 µs | 82 µs |"
                ),
                "{text}"
            );
            assert!(
                text.contains("| 001 | unavailable | unavailable | unavailable | unavailable |"),
                "{text}"
            );
        }
    }

    #[test]
    fn sender_readers_keep_their_numeric_conversion_points() {
        // The port design §7.2 retains integer arithmetic for timing medians.
        // Ordinary diagnostics convert each connection measurement to float.
        let mut log = String::new();
        for (id, value) in [(1, 9_007_199_254_740_993_i128), (2, 9_007_199_254_740_994)] {
            log.push_str(&format!(r#"{{"event":"TestConnectionActivated","test_connection_id":{id}}}
{{"event":"DataPhaseEnded","test_connection_id":{id},"reason":"Completed","send_lag_samples":100,"send_lag_p99_us":{value},"burst_lateness_p99_us":{value}}}
"#));
        }
        log.push_str("{\"event\":\"TestSummary\",\"result\":\"Completed\",\"direction\":\"Downstream\",\"test_connections\":2,\"test_interval\":10,\"sr_index_conf\":500,\"skipped_datagrams\":0,\"withheld_datagrams\":0}\n");
        let text = report(&sender_files(&log, 2)).unwrap();
        assert!(
            text.contains("| 1/1 | 9007199254740994 µs | 9007199254740994 µs |"),
            "{text}"
        );
        assert!(
            text.contains("| 001 | 0 | 0 | 9007199254740992.00 | 9007199254740994.00 |"),
            "{text}"
        );
    }

    /// A Round Run of `round`: its label, name, engine, Client host, Test
    /// Connections, capacity, and rate.
    type Case<'a> = (
        &'a str,
        &'a str,
        &'a str,
        &'a str,
        i32,
        Option<f64>,
        &'a str,
    );

    /// `tests/test_round.py`'s `round_dir`: each case a Round Run of
    /// repeat 1, Upstream, with one Sub-Interval at `capacity`, or none.
    fn round(cases: &[Case]) -> Files {
        let mut files = Files::default();
        let mut summary = String::new();
        for (label, name, engine, host, connections, capacity, rate) in cases {
            let subs = capacity.map_or(String::new(), |c| {
                format!(r#"{{"IPLayerCapacity": {c:?}, "LossRatio": 0.0}}"#)
            });
            files = files.with(
                &format!("tests/{label}.json"),
                &format!(r#"{{"Output": {{"IncrementalResult": [{subs}]}}}}"#),
            );
            let rate = if *rate == "search" {
                "\"search\"".to_string()
            } else {
                rate.to_string()
            };
            summary.push_str(&format!(
                r#"{{"label": "{label}", "status": 0, "name": "{name}", "direction": "upstream", "rate": {rate}, "test_connections": {connections}, "client_impl": "{engine}", "server_impl": "{engine}", "client_host": "{host}", "repeat": 1}}"#
            ));
            summary.push('\n');
        }
        files.with("summary.jsonl", &summary)
    }

    /// A counter file as the counter script writes it on an AWS host
    /// (`tests/test_round.py`'s `counters`).
    fn counters(pps: u32, bw_out: u32) -> String {
        format!(
            "Udp: InDatagrams NoPorts InErrors OutDatagrams RcvbufErrors SndbufErrors InCsumErrors IgnoredMulti MemErrors\n\
             Udp: 1000 0 0 2000 0 0 0 0 0\nsoftnet_dropped: 0\nrx_drops: 0\ntx_drops: 0\n\
             \x20    bw_in_allowance_exceeded: 0\n     bw_out_allowance_exceeded: {bw_out}\n\
             \x20    pps_allowance_exceeded: {pps}\n     conntrack_allowance_available: 40000\n"
        )
    }

    fn limited(files: Files, label: &str, role: &str, pps: u32) -> Files {
        files
            .with(&format!("tests/{label}.{role}.before.txt"), &counters(0, 0))
            .with(
                &format!("tests/{label}.{role}.after.txt"),
                &counters(pps, 0),
            )
    }

    fn line_with<'a>(text: &'a str, start: &str) -> &'a str {
        text.lines()
            .find(|l| l.starts_with(start))
            .unwrap_or_else(|| panic!("no line {start:?} in\n{text}"))
    }

    #[test]
    fn a_fixed_rate_tests_limited_runs_are_void() {
        let files = round(&[
            ("001", "p", "udpstp", "cloud", 1, Some(500.0), "500"),
            ("002", "p", "udpstp", "cloud", 1, Some(400.0), "500"),
            ("003", "p", "udpstp", "cloud", 1, Some(498.0), "500"),
        ]);
        let text = report(&limited(files, "002", "server", 9)).unwrap();
        let line = line_with(&text, "| p | upstream | 500 |");
        // The limited Round Run's 400 is left out of the median and the range.
        assert!(
            line.contains("| 2 (1 void: the platform limited it) | 499.0 (498.0–500.0) |"),
            "{line}"
        );
        assert!(line.ends_with("| 1 |"), "{line}");
    }

    #[test]
    fn the_report_counts_and_lists_the_limited_runs() {
        let files = round(&[
            ("001", "a", "udpstp", "cloud", 1, Some(905.0), "search"),
            ("002", "a", "udpstp", "cloud", 1, Some(900.0), "search"),
            ("003", "a", "obudpst", "cloud", 1, Some(900.0), "search"),
        ]);
        let text = report(&limited(
            limited(files, "001", "server", 3),
            "002",
            "server",
            3,
        ))
        .unwrap();
        let ours = line_with(&text, "| a | upstream | search | 1 | cloud | udpstp/udpstp");
        let theirs = line_with(
            &text,
            "| a | upstream | search | 1 | cloud | obudpst/obudpst",
        );
        // A Search Test's limited Round Runs still count in its medians.
        assert!(ours.contains("| 2 | 902.5 (900.0–905.0) |"), "{ours}");
        assert!(ours.ends_with("| 2 |"), "{ours}");
        assert!(theirs.ends_with("| – |"), "{theirs}");
        assert!(text.contains("\n## Runs the platform limited\n"));
        assert!(text.contains("\n- `001`: server pps_allowance_exceeded +3\n"));
        assert!(text.ends_with("- `002`: server pps_allowance_exceeded +3\n"));
        let verdict = line_with(&text, "| a | upstream | 1 | cloud |");
        assert!(
            verdict.ends_with(
                "| 2/2 · 1/1 | 902.5 | 900.0 | +0.3 % | within 5% (the platform limited 2 runs, \
                 counted: udpstp's 2, OB-UDPST's 0) |"
            ),
            "{verdict}"
        );
    }

    #[test]
    fn the_environment_says_the_cpu_the_kernel_and_the_mtu() {
        let text = "Linux testhost 6.18.33-1-cachyos #1 SMP x86_64 GNU/Linux\n\n\
                    Architecture:  x86_64\nModel name:  AMD Ryzen 7 5800U  \n\
                    dev eth0\n2: eth0: <BROADCAST,UP> mtu 9001 qdisc mq state UP\n";
        assert_eq!(
            environment(text).unwrap(),
            "AMD Ryzen 7 5800U, Linux 6.18.33-1-cachyos, MTU 9001"
        );
        assert_eq!(environment("").unwrap(), "?, Linux ?");
        assert_eq!(environment("a b c\nmtu\n").unwrap(), "?, Linux c");
        // Where the Python refuses it too: a first line of fewer than three
        // words, a Model name without a colon.
        assert!(environment("\nLinux x y\n").is_err());
        assert!(environment("a b c\nModel name\n").is_err());
    }

    #[test]
    fn cells_keep_the_pythons_numbers() {
        let cells = |values: &[Option<Num>]| cell(values, "", 1);
        assert_eq!(cells(&[]), "–");
        assert_eq!(cells(&[None]), "–");
        assert_eq!(cells(&[Some(Num::Float(930.0))]), "930.0");
        assert_eq!(
            cells(&[Some(Num::Int(3)), None, Some(Num::Int(4))]),
            "3.5 (3.0–4.0)"
        );
        // Half to even on the binary value, as Python formats.
        assert_eq!(
            cell(&[Some(Num::Int(2)), Some(Num::Int(3))], " µs", 0),
            "2 µs (2–3)"
        );
        assert_eq!(signed_fixed(-0.04, 1), "-0.0");
        assert_eq!(signed_fixed(0.0, 1), "+0.0");
        assert_eq!(signed_fixed(f64::NAN, 1), "+nan");
        assert_eq!(signed_fixed(f64::NEG_INFINITY, 1), "-inf");
        assert_eq!(percent(judge::MAX_LOSS), "1%");
        assert_eq!(percent(judge::MAX_DIFFERENCE), "5%");
    }

    #[test]
    fn the_round_is_named_as_the_python_names_it() {
        for (out, name) in [
            ("results/round-1", "round-1"),
            ("results/round-1/", "round-1"),
            (".", "."),
            ("/", ""),
            ("", "."),
            ("a/b/..", "a"),
            ("//x//y/", "y"),
            ("a/./b", "b"),
            ("../x", "x"),
            ("/..", ""),
            ("a/b/../..", "."),
            ("../..", ".."),
        ] {
            assert_eq!(round_name(out), name, "{out:?}");
        }
    }

    #[test]
    fn no_summary_or_a_broken_one_is_no_report() {
        assert!(report(&Files::default()).is_err());
        let files = Files::default().with("summary.jsonl", "{\"repeat\": 1, \"label\"");
        assert!(report(&files).is_err());
        // An empty Round still has a report: its title and the tables' heads,
        // with two blank lines between where no host's environment came.
        let text = report(&Files::default().with("summary.jsonl", "")).unwrap();
        assert!(text.starts_with("# Round round-1\n\n\nMaximum: "), "{text}");
        assert!(text.ends_with("|---|---|---|---:|---|---|---:|---|---|---|---|---:|\n"));
    }

    #[test]
    fn what_the_python_refuses_too_is_refused_in_its_words() {
        // Each ended the Python's report with a traceback. It refuses them
        // in these words too since pyfix-2 (scripts/bench/tests/
        // test_round.py's Report).
        let one = || round(&[("001", "a", "udpstp", "cloud", 1, Some(900.0), "search")]);
        let refused = |files: Files| report(&files).expect_err("refused").to_string();
        assert_eq!(refused(Files::default()), "summary.jsonl can't be read");
        let line = r#"{"label": "001", "status": 0, "name": "a", "direction": "upstream", "rate": "search", "test_connections": 1, "client_impl": "udpstp", "server_impl": "udpstp", "client_host": "cloud", "repeat": 1}"#;
        let summary = |text: &str| one().with("summary.jsonl", &format!("{text}\n"));
        for (files, why) in [
            (
                one().with_bytes("summary.jsonl", b"{\"label\": \"\xff\"}\n"),
                "summary.jsonl: not UTF-8",
            ),
            (
                summary("[1, 2]"),
                "summary.jsonl, line 1: not a JSON object",
            ),
            (
                summary(&line.replace("\"repeat\": 1", "\"repeat\": null")),
                "summary.jsonl, line 1: its repeat is of type NoneType, which no Round writes there",
            ),
            (
                summary(&line.replace("\"label\": \"001\", ", "")),
                "summary.jsonl, line 1: it has no label",
            ),
            (
                summary(&line.replace("\"label\": \"001\"", "\"label\": 5")),
                "summary.jsonl, line 1: its label is of type int, which no Round writes there",
            ),
            (
                summary(&line.replace("\"name\": \"a\"", "\"name\": [\"a\"]")),
                "summary.jsonl, line 1: its name is of type list, which no Round writes there",
            ),
            (
                one().with_bytes("env-server.txt", b"Linux h 6.8 \xff\n"),
                "env-server.txt: not UTF-8",
            ),
            (
                one().with_bytes(
                    "tests/001.server.before.txt",
                    b"     pps_allowance_exceeded: 5\xff\n",
                ),
                "tests/001.server.before.txt: not UTF-8",
            ),
        ] {
            assert_eq!(refused(files), why);
        }
        let subs =
            r#"{"Output": {"IncrementalResult": [{"IPLayerCapacity": 900.0, "LossRatio": 0.0}]}"#;
        for (result, why) in [
            ("[1]".to_string(), "the result isn't a JSON object"),
            (
                "\"udpst: connection refused\"".into(),
                "the result isn't a JSON object",
            ),
            ("{\"Output\": null}".into(), "its Output isn't an object"),
            ("{\"Output\": [1]}".into(), "its Output isn't an object"),
            (
                r#"{"Output": {"IncrementalResult": "none"}}"#.into(),
                "its IncrementalResult isn't a list",
            ),
            (
                r#"{"Output": {"IncrementalResult": [5]}}"#.into(),
                "a Sub-Interval of it isn't an object",
            ),
            (
                r#"{"Output": {"IncrementalResult": [{"IPLayerCapacity": 900.0, "LossRatio": null}]}}"#
                    .into(),
                "a Sub-Interval's LossRatio isn't a number",
            ),
            (
                r#"{"Output": {"IncrementalResult": [{"IPLayerCapacity": null}, {"IPLayerCapacity": 900.0}]}}"#
                    .into(),
                "a Sub-Interval's IPLayerCapacity isn't a number",
            ),
            (
                r#"{"Output": {"IncrementalResult": [{"IPLayerCapacity": "900"}]}}"#.into(),
                "a Sub-Interval's IPLayerCapacity isn't a number",
            ),
            (
                format!("{subs}, \"udpstp\": null}}"),
                "its udpstp extension isn't an object",
            ),
            (
                format!("{subs}, \"udpstp\": {{\"KernelSendTimestamps\": [1]}}}}"),
                "its KernelSendTimestamps isn't an object",
            ),
            (
                format!("{subs}, \"udpstp\": {{\"KernelSendTimestamps\": {{\"LagP99Us\": \"5\"}}}}}}"),
                "its kernel send timestamps' LagP99Us isn't a number",
            ),
        ] {
            assert_eq!(
                refused(one().with("tests/001.json", &result)),
                format!("tests/001.json: {why}"),
                "{result}"
            );
        }
    }
}
