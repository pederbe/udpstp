//! The first Round's judging: each Round Run's Maximum and
//! Highest IP-Layer Capacity from its JSON result, and the criterion fixed
//! before the first paid Round, as `scripts/bench/report.py` judges them.
//!
//! Both engines are judged the same way, from their JSON's Sub-Intervals,
//! since their own maxima differ: udpstp's `AtMax` meets the Performance
//! Criterion, OB-UDPST's is its highest Sub-Interval whatever the loss
//! (docs/conformance: `json-at-max-is-the-maximum`). For each Round Run:
//!
//! - its Maximum: the highest IP-Layer Capacity of a Sub-Interval whose
//!   Loss Ratio is at most 1 % (RFC 9097 §6.3's criterion, udpstp's
//!   default);
//! - its Highest: the highest of any Sub-Interval, with its Loss Ratio.
//!
//! The criterion: for each single-connection Search Test between the
//! Round's hosts, in each direction, udpstp's median Maximum is within 5 %
//! of OB-UDPST's, both at the same packet sizes. Tests from home cross the
//! internet, and the two engines sum a Multi-Connection Test's Test
//! Connections differently, so those are shown, not judged.
//!
//! The figures keep the kind of number the files hold ([`Num`]), as the
//! Python keeps an int or a float, for reports that come out byte for
//! byte the Python's.
//!
//! # Unreadable files
//!
//! A report is the Python's, byte for byte, or none ([`Unreadable`]): none
//! where the Python refuses it too, in the same words where it can, and
//! none where the Python would compute from what no Round writes, such as
//! `true` where a number belongs, which the design lets the Rust tool call
//! unreadable (§7.7). A result the Python can't read at all it counts as
//! a Round Run without figures, and so does this.

use std::cmp::Ordering;
use std::fmt;
use std::num::NonZeroUsize;

use bench_rounds::num::{self, Num};
use bench_rounds::py::{self, Json};
use indexmap::IndexMap;

/// The Performance Criterion's Loss Ratio: a Sub-Interval losing more has
/// no Maximum (`MAX_LOSS`).
pub const MAX_LOSS: f64 = 0.01;

/// The most udpstp's median Maximum may differ from OB-UDPST's
/// (`MAX_DIFFERENCE`).
pub const MAX_DIFFERENCE: f64 = 0.05;

/// Why a Round's report can't be written: which file, and what in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unreadable(String);

impl Unreadable {
    pub fn new(why: impl Into<String>) -> Unreadable {
        Unreadable(why.into())
    }

    /// The same, said of `file`.
    pub fn at(self, file: &str) -> Unreadable {
        Unreadable(format!("{file}: {}", self.0))
    }
}

impl fmt::Display for Unreadable {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Unreadable {}

/// A Round Run of a repeat, from the Round's `summary.jsonl` (the path
/// checks, of repeat 0, are left out, as the Python's `load` leaves them).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// Its label, which names its files in `tests/`.
    pub label: String,
    /// The Client's exit status, or `None` where the host failed the call.
    pub status: Option<i128>,
    pub name: String,
    pub direction: String,
    /// `search`, or the row's number: `str(run["rate"])`.
    pub rate: String,
    pub test_connections: i128,
    pub client_host: String,
    /// The engines of its Client and Server.
    pub client: String,
    pub server: String,
    pub kernel_send_timestamps: bool,
    pub host_samples: bool,
    pub test_interval: Option<i128>,
    pub repeat: i128,
    /// Requested maximum pool size, separate from startup and sampled evidence.
    pub server_engine_threads: Option<NonZeroUsize>,
}

impl Record {
    pub(super) fn test_name(&self) -> String {
        match self.server_engine_threads {
            Some(cap) => format!("{} (Server cap {cap})", self.name),
            None => self.name.clone(),
        }
    }
    /// `Client/Server`, as the report shows the engines.
    pub fn pair(&self) -> String {
        format!("{}/{}", self.client, self.server)
    }

    /// The Round Run's line of `summary.jsonl`, read as a Round writes
    /// one: strings, an int or `search` for the rate, an int for the Test
    /// Connections, an int or `null` for the status.
    fn read(run: &IndexMap<String, Json>) -> Result<Record, Unreadable> {
        let field = |key: &str| {
            run.get(key)
                .ok_or_else(|| Unreadable(format!("it has no {key}")))
        };
        let odd = |key: &str, value: &Json| {
            Unreadable(format!(
                "its {key} is of type {}, which no Round writes there",
                value.type_name()
            ))
        };
        let text = |key: &str| match field(key)? {
            Json::Str(s) => Ok(s.clone()),
            other => Err(odd(key, other)),
        };
        let label = text("label")?;
        if label.contains(['/', '\\', '\0']) {
            return Err(Unreadable(format!(
                "its label {label:?} isn't a file's name"
            )));
        }
        let status = match field("status")? {
            Json::Int(status) => Some(*status),
            Json::Null => None,
            other => return Err(odd("status", other)),
        };
        let rate = match field("rate")? {
            Json::Str(rate) if rate == "search" => rate.clone(),
            Json::Int(row) => row.to_string(),
            other => return Err(odd("rate", other)),
        };
        let test_connections = match field("test_connections")? {
            Json::Int(n) => *n,
            other => return Err(odd("test_connections", other)),
        };
        let name = text("name")?;
        let direction = text("direction")?;
        let client_host = text("client_host")?;
        let client = text("client_impl")?;
        let server = text("server_impl")?;
        let server_engine_threads = match run.get("server_engine_threads") {
            None => None,
            Some(Json::Int(n)) if server == "udpstp" => Some(
                usize::try_from(*n)
                    .ok()
                    .and_then(NonZeroUsize::new)
                    .ok_or_else(|| {
                        Unreadable::new("its server_engine_threads is not a positive usize")
                    })?,
            ),
            Some(_) => {
                return Err(Unreadable::new(
                    "its server_engine_threads is not a positive usize for a udpstp Server",
                ));
            }
        };
        Ok(Record {
            label,
            status,
            name,
            direction,
            rate,
            test_connections,
            client_host,
            client,
            server,
            server_engine_threads,
            kernel_send_timestamps: matches!(
                run.get("kernel_send_timestamps"),
                Some(Json::Bool(true))
            ),
            host_samples: matches!(run.get("host_samples"), Some(Json::Bool(true))),
            test_interval: match run.get("test_interval") {
                Some(Json::Int(n)) => Some(*n),
                _ => None,
            },
            repeat: match run.get("repeat") {
                Some(Json::Int(n)) => *n,
                _ => 0,
            },
        })
    }
}

/// The Round Runs of the repeats in a Round's `summary.jsonl`, as the
/// Python's `load` reads them: a JSON object a line, blank lines passed
/// over, those of repeat 0 or none left out.
pub fn read_summary(text: &str) -> Result<Vec<Record>, Unreadable> {
    let mut runs = Vec::new();
    for (n, line) in py::file_lines(text).into_iter().enumerate() {
        if py::strip(line).is_empty() {
            continue;
        }
        let at = |why: Unreadable| why.at(&format!("summary.jsonl, line {}", n + 1));
        let value = Json::parse(line).map_err(|e| at(Unreadable(e.to_string())))?;
        let Json::Dict(run) = &value else {
            return Err(at(Unreadable::new("not a JSON object")));
        };
        let repeat = match run.get("repeat") {
            None => 0,
            Some(Json::Int(repeat)) => *repeat,
            Some(other) => {
                return Err(at(Unreadable(format!(
                    "its repeat is of type {}, which no Round writes there",
                    other.type_name()
                ))));
            }
        };
        if repeat > 0 {
            runs.push(Record::read(run).map_err(at)?);
        }
    }
    Ok(runs)
}

/// A Round Run's figures, from its JSON result (`figures`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Figures {
    /// The Maximum IP-Layer Capacity, Mbit/s: none where no Sub-Interval
    /// lost at most [`MAX_LOSS`].
    pub maximum: Option<Num>,
    /// The Highest IP-Layer Capacity, Mbit/s.
    pub highest: Num,
    /// The Loss Ratio of the Sub-Interval of the Highest (of the one with
    /// the most loss, of those as high).
    pub highest_loss: Num,
    /// udpstp's kernel send timestamps' lag, 99th percentile, µs, where it
    /// reported one.
    pub lag_p99: Option<Num>,
}

/// A Round Run's figures from its result file's bytes, or none: no file, or
/// one that isn't UTF-8 or JSON, which the Python passes over as it does.
///
/// Text only the Python reads as JSON, or fails on otherwise
/// ([`py::python_also_refuses`]), is unreadable.
pub fn read_figures(result: Option<&[u8]>) -> Result<Option<Figures>, Unreadable> {
    let Some(text) = result.and_then(|bytes| std::str::from_utf8(bytes).ok()) else {
        return Ok(None);
    };
    match Json::parse(text) {
        Ok(result) => figures(&result),
        Err(_) if !py::python_also_refuses(text) => Err(Unreadable::new(
            "JSON that Python reads and this doesn't: a NaN, an infinity, an integer beyond 64 \
             bits, a lone surrogate or deep nesting, which no Client writes",
        )),
        Err(_) => Ok(None),
    }
}

/// A Round Run's figures from its JSON result, as the Python's `figures`
/// takes them: none where it has no Sub-Intervals.
pub fn figures(result: &Json) -> Result<Option<Figures>, Unreadable> {
    let Json::Dict(result) = result else {
        return Err(Unreadable::new("the result isn't a JSON object"));
    };
    let subs = match result.get("Output") {
        None => return Ok(None),
        Some(Json::Dict(output)) => output.get("IncrementalResult"),
        Some(_) => return Err(Unreadable::new("its Output isn't an object")),
    };
    let subs = match subs {
        Some(Json::List(subs)) if !subs.is_empty() => subs,
        None => return Ok(None),
        Some(subs) if !subs.truthy() => return Ok(None),
        Some(_) => return Err(Unreadable::new("its IncrementalResult isn't a list")),
    };
    let mut caps = Vec::with_capacity(subs.len());
    for sub in subs {
        let Json::Dict(sub) = sub else {
            return Err(Unreadable::new("a Sub-Interval of it isn't an object"));
        };
        let number = |key: &str| match sub.get(key) {
            None => Ok(Num::Float(0.0)),
            Some(value) => Num::from_json(value)
                .ok_or_else(|| Unreadable(format!("a Sub-Interval's {key} isn't a number"))),
        };
        caps.push((number("IPLayerCapacity")?, number("LossRatio")?));
    }
    let valid: Vec<Num> = caps
        .iter()
        .filter(|(_, loss)| {
            matches!(
                loss.py_cmp(Num::Float(MAX_LOSS)),
                Some(Ordering::Less | Ordering::Equal)
            )
        })
        .map(|&(cap, _)| cap)
        .collect();
    // max() of the (capacity, loss) pairs: the highest capacity, of equals
    // the most loss, of those the first.
    let mut highest = caps[0];
    for &pair in &caps[1..] {
        if pair_greater(pair, highest) {
            highest = pair;
        }
    }
    let lag_p99 = match result.get("udpstp") {
        None => None,
        Some(Json::Dict(ours)) => match ours.get("KernelSendTimestamps") {
            None => None,
            Some(stamps) if !stamps.truthy() => None,
            Some(Json::Dict(stamps)) => match stamps.get("LagP99Us") {
                None | Some(Json::Null) => None,
                Some(value) => Some(Num::from_json(value).ok_or_else(|| {
                    Unreadable::new("its kernel send timestamps' LagP99Us isn't a number")
                })?),
            },
            Some(_) => {
                return Err(Unreadable::new("its KernelSendTimestamps isn't an object"));
            }
        },
        Some(_) => return Err(Unreadable::new("its udpstp extension isn't an object")),
    };
    Ok(Some(Figures {
        maximum: num::max(&valid),
        highest: highest.0,
        highest_loss: highest.1,
        lag_p99,
    }))
}

/// Python's `a > b` of two tuples of numbers: at the first place where
/// they aren't equal, whether `a`'s is greater.
fn pair_greater(a: (Num, Num), b: (Num, Num)) -> bool {
    for (x, y) in [(a.0, b.0), (a.1, b.1)] {
        if x.py_cmp(y) != Some(Ordering::Equal) {
            return x.py_cmp(y) == Some(Ordering::Greater);
        }
    }
    false
}

/// A Round Run as the criterion counts it: its Maximum, if it had one, and
/// whether the platform limited it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Counted {
    pub maximum: Option<Num>,
    pub flagged: bool,
}

/// The criterion's verdict on a Test both engines ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// From home: shown, not judged.
    AcrossTheInternet,
    /// More than one Test Connection: shown, not judged.
    MultiConnection,
    /// A udpstp Round Run the platform didn't limit failed or found no
    /// Maximum.
    NotMet,
    /// A udpstp Round Run the platform limited failed or found no Maximum.
    OursLimitedFailed,
    /// An OB-UDPST Round Run failed or found no Maximum.
    TheirsFailed,
    /// Within [`MAX_DIFFERENCE`], or outside it; the Round Runs the platform
    /// limited count, and the verdict says how many there were.
    Within {
        ours_limited: usize,
        theirs_limited: usize,
    },
    Outside {
        ours_limited: usize,
        theirs_limited: usize,
    },
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let limited = |f: &mut fmt::Formatter, ours: usize, theirs: usize| {
            let count = ours + theirs;
            if count == 0 {
                return Ok(());
            }
            let s = if count == 1 { "" } else { "s" };
            write!(
                f,
                " (the platform limited {count} run{s}, counted: udpstp's {ours}, OB-UDPST's {theirs})"
            )
        };
        match *self {
            Verdict::AcrossTheInternet => f.write_str("shown, not judged: across the internet"),
            Verdict::MultiConnection => {
                f.write_str("shown, not judged: the two sum Test Connections differently")
            }
            Verdict::NotMet => f.write_str("**not met**: a udpstp run failed or found no Maximum"),
            Verdict::OursLimitedFailed => f.write_str(
                "can't be judged: a udpstp run that the platform limited failed or found no Maximum",
            ),
            Verdict::TheirsFailed => {
                f.write_str("can't be judged: an OB-UDPST run failed or found no Maximum")
            }
            Verdict::Within {
                ours_limited,
                theirs_limited,
            } => {
                f.write_str("within 5%")?;
                limited(f, ours_limited, theirs_limited)
            }
            Verdict::Outside {
                ours_limited,
                theirs_limited,
            } => {
                f.write_str("**outside 5%**: investigate")?;
                limited(f, ours_limited, theirs_limited)
            }
        }
    }
}

/// udpstp against OB-UDPST on one Test (`comparison`'s loop).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Comparison {
    /// udpstp's Round Runs with a Maximum, and all it counted.
    pub ours: (usize, usize),
    /// OB-UDPST's.
    pub theirs: (usize, usize),
    pub ours_median: Option<Num>,
    pub theirs_median: Option<Num>,
    /// udpstp's median Maximum over OB-UDPST's, less 1.
    pub difference: Option<f64>,
    pub verdict: Verdict,
}

/// The criterion on a Test both engines ran, from the Round Runs each
/// counted, where the Client was (`cloud` or `home`) and how many Test
/// Connections the Test had.
///
/// A Round Run that failed counts against the engine it ran: a udpstp
/// Round Run that failed without the platform limiting it makes the
/// criterion not met, whatever else happened; one the platform limited, or
/// an OB-UDPST one that failed, means the Test can't be judged. Round Runs
/// the platform limited otherwise count, and the verdict says how many:
/// the shaper is part of the path both engines measure.
pub fn compare(
    ours: &[Counted],
    theirs: &[Counted],
    client_host: &str,
    test_connections: i128,
) -> Result<Comparison, Unreadable> {
    let maxima = |runs: &[Counted]| -> Vec<Num> { runs.iter().filter_map(|r| r.maximum).collect() };
    let (our_maxima, their_maxima) = (maxima(ours), maxima(theirs));
    let ours_median = num::median(&our_maxima);
    let theirs_median = num::median(&their_maxima);
    // `ours / theirs - 1` where OB-UDPST's median is there and not 0.
    let difference = match (ours_median, theirs_median) {
        (Some(o), Some(t)) if t.py_cmp(Num::Int(0)) != Some(Ordering::Equal) => Some(
            o.py_div(t)
                .ok_or_else(|| Unreadable::new("a Maximum beyond 2^53 Mbit/s"))?
                - 1.0,
        ),
        _ => None,
    };
    let limited = |runs: &[Counted]| runs.iter().filter(|r| r.flagged).count();
    let (ours_limited, theirs_limited) = (limited(ours), limited(theirs));
    let ours_failed: Vec<&Counted> = ours.iter().filter(|r| r.maximum.is_none()).collect();
    let verdict = if client_host != "cloud" {
        Verdict::AcrossTheInternet
    } else if test_connections > 1 {
        Verdict::MultiConnection
    } else if ours_failed.iter().any(|r| !r.flagged) {
        Verdict::NotMet
    } else if !ours_failed.is_empty() {
        Verdict::OursLimitedFailed
    } else {
        match difference {
            Some(d) if their_maxima.len() == theirs.len() => {
                if d.abs() <= MAX_DIFFERENCE {
                    Verdict::Within {
                        ours_limited,
                        theirs_limited,
                    }
                } else {
                    Verdict::Outside {
                        ours_limited,
                        theirs_limited,
                    }
                }
            }
            _ => Verdict::TheirsFailed,
        }
    };
    Ok(Comparison {
        ours: (our_maxima.len(), ours.len()),
        theirs: (their_maxima.len(), theirs.len()),
        ours_median,
        theirs_median,
        difference,
        verdict,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(subs: &[(f64, f64)]) -> Json {
        let subs: Vec<String> = subs
            .iter()
            .map(|(c, l)| format!(r#"{{"IPLayerCapacity": {c:?}, "LossRatio": {l:?}}}"#))
            .collect();
        Json::parse(&format!(
            r#"{{"Output": {{"IncrementalResult": [{}]}}}}"#,
            subs.join(", ")
        ))
        .unwrap()
    }

    #[test]
    fn both_engines_are_judged_by_the_same_criterion() {
        // tests/test_round.py's Report: OB-UDPST's highest Sub-Interval lost
        // 3 %; its Maximum is lower.
        let theirs = figures(&result(&[(900.0, 0.0), (950.0, 0.03), (920.0, 0.005)]))
            .unwrap()
            .unwrap();
        assert_eq!(theirs.maximum, Some(Num::Float(920.0)));
        assert_eq!(theirs.highest, Num::Float(950.0));
        assert_eq!(theirs.highest_loss, Num::Float(0.03));
        // Of equal capacities, the one with the most loss.
        let tied = figures(&result(&[(900.0, 0.0), (900.0, 0.02), (900.0, 0.01)]))
            .unwrap()
            .unwrap();
        assert_eq!(tied.highest_loss, Num::Float(0.02));
        assert_eq!(tied.maximum, Some(Num::Float(900.0)));
    }

    #[test]
    fn a_result_without_sub_intervals_has_no_figures() {
        for text in [
            "{}",
            r#"{"Output": {}}"#,
            r#"{"Output": {"IncrementalResult": []}}"#,
            r#"{"Output": {"IncrementalResult": null}}"#,
            r#"{"Output": {"IncrementalResult": 0}}"#,
            r#"{"ErrorStatus": 200, "ErrorMessage": "gone"}"#,
        ] {
            assert_eq!(figures(&Json::parse(text).unwrap()), Ok(None), "{text}");
        }
        // Nor does a result that isn't JSON, or isn't UTF-8.
        assert_eq!(read_figures(None), Ok(None));
        assert_eq!(read_figures(Some(b"{\"Output\": {")), Ok(None));
        assert_eq!(read_figures(Some(b"\xff\xfe")), Ok(None));
    }

    #[test]
    fn a_result_the_python_fails_on_or_computes_oddly_from_is_unreadable() {
        for text in [
            "[]",
            "null",
            r#"{"Output": []}"#,
            r#"{"Output": null}"#,
            r#"{"Output": {"IncrementalResult": "abc"}}"#,
            r#"{"Output": {"IncrementalResult": {"a": 1}}}"#,
            r#"{"Output": {"IncrementalResult": [5]}}"#,
            r#"{"Output": {"IncrementalResult": [{"LossRatio": null}]}}"#,
            r#"{"Output": {"IncrementalResult": [{"IPLayerCapacity": true}]}}"#,
            r#"{"Output": {"IncrementalResult": [{"IPLayerCapacity": 1.0}]}, "udpstp": null}"#,
            r#"{"Output": {"IncrementalResult": [{"IPLayerCapacity": 1.0}]}, "udpstp": {"KernelSendTimestamps": [1]}}"#,
        ] {
            assert!(figures(&Json::parse(text).unwrap()).is_err(), "{text}");
        }
        // JSON only Python reads.
        assert!(
            read_figures(Some(
                br#"{"Output": {"IncrementalResult": [{"LossRatio": NaN}]}}"#
            ))
            .is_err()
        );
    }

    #[test]
    fn missing_fields_are_the_pythons_defaults() {
        let f = figures(
            &Json::parse(
                r#"{"Output": {"IncrementalResult": [{"LossRatio": 0.5}, {"IPLayerCapacity": 3}]},
                    "udpstp": {"KernelSendTimestamps": {"LagP99Us": 17}}}"#,
            )
            .unwrap(),
        )
        .unwrap()
        .unwrap();
        // The first lost half its Load PDUs; the second, as written, an int.
        assert_eq!(f.maximum, Some(Num::Int(3)));
        assert_eq!(f.highest, Num::Int(3));
        assert_eq!(f.highest_loss, Num::Float(0.0));
        assert_eq!(f.lag_p99, Some(Num::Int(17)));
    }

    fn counted(maximum: Option<f64>, flagged: bool) -> Counted {
        Counted {
            maximum: maximum.map(Num::Float),
            flagged,
        }
    }

    #[test]
    fn the_criterion_judges_only_complete_single_connection_cloud_tests() {
        let verdict = |ours: &[Counted], theirs: &[Counted], host: &str, n: i128| {
            compare(ours, theirs, host, n).unwrap().verdict.to_string()
        };
        let ours = [counted(Some(800.0), false)];
        let theirs = [counted(Some(900.0), false)];
        assert_eq!(
            verdict(&ours, &theirs, "cloud", 1),
            "**outside 5%**: investigate"
        );
        assert_eq!(
            compare(&ours, &theirs, "cloud", 1).unwrap().difference,
            Some(800.0 / 900.0 - 1.0)
        );
        assert_eq!(
            verdict(&[counted(Some(905.0), false)], &theirs, "cloud", 1),
            "within 5%"
        );
        assert_eq!(
            verdict(&ours, &[counted(None, false)], "cloud", 1),
            "can't be judged: an OB-UDPST run failed or found no Maximum"
        );
        assert_eq!(
            verdict(&ours, &theirs, "cloud", 4),
            "shown, not judged: the two sum Test Connections differently"
        );
        assert_eq!(
            verdict(&ours, &theirs, "home", 1),
            "shown, not judged: across the internet"
        );
        // A udpstp run that failed: not met unless the platform limited it.
        let failed = [counted(None, false), counted(Some(930.0), false)];
        assert_eq!(
            verdict(&failed, &theirs, "cloud", 1),
            "**not met**: a udpstp run failed or found no Maximum"
        );
        assert_eq!(
            verdict(&[counted(None, true)], &theirs, "cloud", 1),
            "can't be judged: a udpstp run that the platform limited failed or found no Maximum"
        );
        // Limited runs count, and the verdict says how many.
        assert_eq!(
            verdict(
                &[counted(Some(905.0), false)],
                &[counted(Some(900.0), true)],
                "cloud",
                1
            ),
            "within 5% (the platform limited 1 run, counted: udpstp's 0, OB-UDPST's 1)"
        );
        // OB-UDPST's median of 0: no difference, so it can't be judged.
        assert_eq!(
            verdict(&ours, &[counted(Some(0.0), false)], "cloud", 1),
            "can't be judged: an OB-UDPST run failed or found no Maximum"
        );
    }

    #[test]
    fn the_summary_keeps_the_repeats_runs() {
        let line = |repeat: &str| {
            format!(
                r#"{{"label": "001-a", "status": 0, "name": "a", "direction": "upstream", "rate": "search", "test_connections": 1, "client_impl": "udpstp", "server_impl": "udpstp", "client_host": "cloud"{repeat}}}"#
            )
        };
        let text = [
            line(", \"repeat\": 1"),
            String::new(),
            " \t".into(),
            line(", \"repeat\": 0"),
            line(""),
            line(", \"repeat\": -2"),
        ]
        .join("\r\n");
        let runs = read_summary(&text).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].pair(), "udpstp/udpstp");
        // What the Python refuses too, and what no Round writes.
        for bad in [
            "[1]".to_string(),
            "{\"repeat\": null}".into(),
            "{\"repeat\": 1}".into(),
            line(", \"repeat\": 1.0"),
            line(", \"repeat\": 1").replace("\"search\"", "true"),
            line(", \"repeat\": 1").replace("\"status\": 0", "\"status\": false"),
            line(", \"repeat\": 1").replace("001-a", "../x"),
            line(", \"repeat\": 1") + "{",
        ] {
            assert!(read_summary(&bad).is_err(), "{bad}");
        }
    }
}
