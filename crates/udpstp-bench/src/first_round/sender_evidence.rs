//! Server Load Sender evidence. Timing accepts completed connection evidence
//! alone; gates and ordinary diagnostics require the complete Test's log.

use std::collections::HashSet;

use bench_rounds::num::{self, Num};
use bench_rounds::py::Json;
use indexmap::IndexMap;

use super::judge::Record;
use super::report::RoundFiles;

const UNREADABLE: &str = "Server timing unavailable: capture missing or unreadable";
const INVALID: &str = "Server timing unavailable: invalid or incomplete connection evidence";
const MISMATCH: &str = "Server timing unavailable: capture does not match the Round Run";

type Fields = IndexMap<String, Json>;

fn field<'a>(value: &'a Json, key: &str) -> Option<&'a Json> {
    value.as_dict()?.get(key)
}

fn capture(bytes: Option<&[u8]>) -> Option<Json> {
    Json::parse(std::str::from_utf8(bytes?).ok()?).ok()
}

fn log(capture: &Json) -> Result<&str, &'static str> {
    if field(capture, "status") != Some(&Json::Int(0)) {
        return Err("Server timing unavailable: capture failed");
    }
    field(capture, "log")
        .and_then(Json::as_str)
        .ok_or(UNREADABLE)
}

/// Decode lazily so timing retains the first event's diagnostic. Figures
/// collect the whole log before checking whether the Test is complete.
fn events(log: &str) -> impl Iterator<Item = Result<Fields, &'static str>> + '_ {
    log.split('\n')
        .filter(|line| !line.trim().is_empty())
        .map(|line| match Json::parse(line).map_err(|_| UNREADABLE)? {
            Json::Dict(event) => Ok(event),
            _ => Err(INVALID),
        })
}

fn number(fields: &Fields, key: &str) -> Option<Num> {
    let n = Num::from_json(fields.get(key)?)?;
    (n.to_f64().is_finite() && n.to_f64() >= 0.0).then_some(n)
}

fn whole(value: &Json) -> Option<i128> {
    match value {
        Json::Int(n) if *n >= 0 => Some(*n),
        _ => None,
    }
}

/// The measurements of a completed Test Connection. Retain their numeric
/// kinds until each reader's established aggregation step.
struct ConnectionEnd<'a> {
    id: i128,
    lateness: Num,
    fields: &'a Fields,
}

impl<'a> ConnectionEnd<'a> {
    fn read(fields: &'a Fields) -> Option<Self> {
        let id = whole(fields.get("test_connection_id")?)?;
        if id == 0 || fields.get("reason")?.as_str()? != "Completed" {
            return None;
        }
        Some(Self {
            id,
            lateness: number(fields, "burst_lateness_p99_us")?,
            fields,
        })
    }

    fn lag(&self) -> Option<Num> {
        if whole(self.fields.get("send_lag_samples")?)? == 0 {
            return None;
        }
        number(self.fields, "send_lag_p99_us")
    }
}

/// The median of connection p99s, not a percentile of pooled samples.
/// A summary is optional, but must match when present.
pub(super) fn timing(bytes: Option<&[u8]>, run: &Record) -> Result<(Num, Num), &'static str> {
    let capture = capture(bytes).ok_or(UNREADABLE)?;
    if field(&capture, "label") != Some(&Json::Str(run.label.clone())) {
        return Err(MISMATCH);
    }
    let log = log(&capture)?;
    let mut ids = HashSet::new();
    let (mut lags, mut lateness) = (Vec::new(), Vec::new());
    let mut summaries = 0;
    for event in events(log) {
        let event = event?;
        match event.get("event").and_then(Json::as_str) {
            Some("DataPhaseEnded") => {
                let end = ConnectionEnd::read(&event).ok_or(INVALID)?;
                if !ids.insert(end.id) {
                    return Err(INVALID);
                }
                lags.push(end.lag().ok_or(INVALID)?);
                lateness.push(end.lateness);
            }
            Some("TestSummary") => {
                summaries += 1;
                let row_matches = run.rate == "search"
                    || matches!(event.get("sr_index_conf"), Some(Json::Int(row)) if row.to_string() == run.rate);
                if summaries > 1
                    || event.get("result") != Some(&Json::Str("Completed".into()))
                    || event.get("direction") != Some(&Json::Str("Downstream".into()))
                    || event.get("test_connections") != Some(&Json::Int(run.test_connections))
                    || !row_matches
                {
                    return Err("Server timing unavailable: mismatched Test summary");
                }
            }
            _ => {}
        }
    }
    if ids.len() as i128 != run.test_connections {
        return Err(INVALID);
    }
    Ok((
        num::median(&lags).ok_or(INVALID)?,
        num::median(&lateness).ok_or(INVALID)?,
    ))
}

pub(super) struct ServerFigures {
    pub skipped: i128,
    pub withheld: i128,
    pub lateness: f64,
    pub worst_lateness: f64,
    pub lag: Option<f64>,
}

/// Complete evidence for precision gates and ordinary sender diagnostics.
/// Matching activations, one summary and its totals are required here.
pub(super) fn figures(files: &dyn RoundFiles, run: &Record) -> Option<ServerFigures> {
    if run.direction != "downstream" || run.server != "udpstp" {
        return None;
    }
    let capture = if run.kernel_send_timestamps {
        let capture = capture(
            files
                .read(&format!("tests/{}.server.json", run.label))
                .as_deref(),
        )?;
        if field(&capture, "label") != Some(&Json::Str(run.label.clone())) {
            return None;
        }
        capture
    } else {
        let envelope = capture(
            files
                .read(&format!("tests/{}.vm.json", run.label))
                .as_deref(),
        )?;
        if field(&envelope, "label") != Some(&Json::Str(run.label.clone())) {
            return None;
        }
        field(field(&envelope, "hosts")?, "server")?.clone()
    };
    let log = log(&capture).ok()?;
    if !log.ends_with('\n') {
        return None;
    }
    let events: Vec<_> = events(log).collect::<Result<_, _>>().ok()?;
    let named = |e: &&Fields, name| e.get("event").and_then(Json::as_str) == Some(name);
    let ends: Vec<_> = events
        .iter()
        .filter(|e| named(e, "DataPhaseEnded"))
        .map(ConnectionEnd::read)
        .collect::<Option<_>>()?;
    let summaries: Vec<_> = events.iter().filter(|e| named(e, "TestSummary")).collect();
    let ids: HashSet<_> = ends.iter().map(|end| end.id).collect();
    if summaries.len() != 1 || ends.len() as i128 != run.test_connections || ids.len() != ends.len()
    {
        return None;
    }
    let activated: Vec<_> = events
        .iter()
        .filter(|e| named(e, "TestConnectionActivated"))
        .map(|e| whole(e.get("test_connection_id")?))
        .collect::<Option<_>>()?;
    if activated.len() != ids.len() || activated.iter().copied().collect::<HashSet<_>>() != ids {
        return None;
    }
    let summary = summaries[0];
    if summary.get("result") != Some(&Json::Str("Completed".into()))
        || summary.get("direction") != Some(&Json::Str("Downstream".into()))
        || summary.get("test_connections") != Some(&Json::Int(run.test_connections))
        || whole(summary.get("test_interval")?) != run.test_interval
        || (run.rate != "search" && whole(summary.get("sr_index_conf")?)?.to_string() != run.rate)
    {
        return None;
    }
    let (mut lateness, mut lags) = (Vec::new(), Vec::new());
    for end in ends {
        // These figures convert each value before taking the median, unlike timing.
        lateness.push(end.lateness.to_f64());
        if run.kernel_send_timestamps {
            lags.push(end.lag()?.to_f64());
        }
    }
    let median = |values: &[f64]| {
        num::median(&values.iter().copied().map(Num::Float).collect::<Vec<_>>())
            .expect("nonempty finite figures")
            .to_f64()
    };
    Some(ServerFigures {
        skipped: whole(summary.get("skipped_datagrams")?)?,
        withheld: whole(summary.get("withheld_datagrams")?)?,
        lateness: median(&lateness),
        worst_lateness: lateness.iter().copied().fold(0.0, f64::max),
        lag: (!lags.is_empty()).then(|| median(&lags)),
    })
}
