//! The ledger of Rounds, which the Rust and the Python tools share
//! (`scripts/bench/round.py:176-204, 743-816`): one file, by default
//! `$XDG_STATE_HOME/udpstp-bench/ledger.json`, holding `{"rounds": [...]}`,
//! an entry a Round (`id`, `provider`, `started_utc`, `expiry`,
//! `worst_case`, `state`, later `hours`), so that each tool's month cap
//! counts the other's Rounds.
//!
//! Both read and write it alike: the whole file as `json.dump(ledger, f,
//! indent=1)` writes it, byte for byte, fields the tool doesn't know kept
//! where they are. Where the Python writes a fixed `ledger.json.tmp`
//! before renaming it over the ledger, this writes a temporary file of a
//! name of its own, and reads the ledger again right before each write,
//! so that it never writes back a ledger older than the file (the
//! design's N10). Two Rounds at once on one ledger still race, as in the
//! Python: the runbooks keep to one at a time.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use indexmap::IndexMap;

use crate::num::{self, Num};
use crate::py::{self, Json, NotJson, PyError};
use crate::utc::Utc;

/// What went wrong with the ledger.
#[derive(Debug)]
pub enum LedgerError {
    /// It couldn't be read or written.
    Io(PathBuf, io::Error),
    /// It isn't JSON.
    NotJson(PathBuf, NotJson),
    /// It is JSON of a shape the Python would raise on.
    Shape(PathBuf, PyError),
}

impl fmt::Display for LedgerError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            LedgerError::Io(path, e) => write!(f, "the ledger {}: {e}", path.display()),
            LedgerError::NotJson(path, e) => write!(f, "the ledger {}: {e}", path.display()),
            LedgerError::Shape(path, e) => write!(f, "the ledger {}: {e}", path.display()),
        }
    }
}

impl std::error::Error for LedgerError {}

/// `ledger_path(arg)`: `--ledger`'s path, else
/// `$XDG_STATE_HOME/udpstp-bench/ledger.json`, with `~/.local/state` for
/// an unset `XDG_STATE_HOME`. As in the Python, a set but empty one makes
/// the path relative. `home` is the user's home directory, where `~` goes.
pub fn path(arg: Option<&Path>, xdg_state_home: Option<&str>, home: &Path) -> PathBuf {
    if let Some(arg) = arg {
        return arg.to_path_buf();
    }
    let base = match xdg_state_home {
        Some(base) => PathBuf::from(base),
        None => home.join(".local").join("state"),
    };
    base.join("udpstp-bench").join("ledger.json")
}

/// `load_ledger(path)`: the ledger, or `{"rounds": []}` where there is
/// none yet.
pub fn load(path: &Path) -> Result<Json, LedgerError> {
    match fs::read(path) {
        Ok(bytes) => Json::parse_bytes(&bytes).map_err(|e| LedgerError::NotJson(path.into(), e)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(empty()),
        Err(e) => Err(LedgerError::Io(path.into(), e)),
    }
}

fn empty() -> Json {
    Json::Dict(IndexMap::from([(
        "rounds".to_string(),
        Json::List(Vec::new()),
    )]))
}

/// The ledger read again right now, `change` applied, and written back
/// through a temporary file of its own, renamed over the ledger.
pub fn update(
    path: &Path,
    change: impl FnOnce(&mut Json) -> Result<(), PyError>,
) -> Result<(), LedgerError> {
    let mut ledger = load(path)?;
    change(&mut ledger).map_err(|e| LedgerError::Shape(path.into(), e))?;
    write(path, &ledger)
}

/// A Round's entry as `execute` writes it before creating anything: its
/// ID, the provider, when it started (`isoformat()` of `now`), its
/// expiry, the worst case's total, and `"state": "started"`. `None` for a
/// `now` beyond `datetime`'s years, which Python refuses too.
pub fn entry(id: &str, provider: &str, now: f64, expiry: f64, worst_case: f64) -> Option<Json> {
    let started = Utc::from_timestamp(now)?.isoformat();
    Some(Json::Dict(IndexMap::from([
        ("id".to_string(), Json::Str(id.to_string())),
        ("provider".to_string(), Json::Str(provider.to_string())),
        ("started_utc".to_string(), Json::Str(started)),
        ("expiry".to_string(), Json::Float(expiry)),
        ("worst_case".to_string(), Json::Float(worst_case)),
        ("state".to_string(), Json::Str("started".to_string())),
    ])))
}

/// Adds `entry` to the ledger file's Rounds: the ledger first, so that a
/// crash from here on leaves a record.
pub fn start(path: &Path, entry: Json) -> Result<(), LedgerError> {
    update(path, |ledger| add(ledger, entry))
}

/// Closes the entries of Round `id` in the ledger file at its end
/// ([`close`]).
pub fn finish(path: &Path, id: &str, state: &str, hours: f64) -> Result<(), LedgerError> {
    update(path, |ledger| close(ledger, id, state, hours))
}

/// `ledger["rounds"].append(entry)`.
pub fn add(ledger: &mut Json, entry: Json) -> Result<(), PyError> {
    rounds_mut(ledger)?.push(entry);
    Ok(())
}

/// Closes the entries of Round `id` at its end, as `execute` does: `state`
/// (`"torn down"`, or `"left resources"`) and `hours`, how long it took,
/// rounded to three decimals as the Python rounds.
pub fn close(ledger: &mut Json, id: &str, state: &str, hours: f64) -> Result<(), PyError> {
    for round in rounds_mut(ledger)?.iter_mut() {
        if round.item("id")? == &Json::Str(id.to_string()) {
            let Json::Dict(round) = round else {
                unreachable!("item() found a key, so it is a dict");
            };
            round.insert("state".to_string(), Json::Str(state.to_string()));
            round.insert("hours".to_string(), Json::Float(num::py_round(hours, 3)));
        }
    }
    Ok(())
}

/// `ledger["rounds"]`, to append to: a dict's list, else what Python
/// raises.
fn rounds_mut(ledger: &mut Json) -> Result<&mut Vec<Json>, PyError> {
    let found = ledger.item("rounds")?;
    if found.as_list().is_none() {
        return Err(PyError::new(
            py::PyErrorKind::AttributeError,
            format!("'{}' object has no attribute 'append'", found.type_name()),
        ));
    }
    match ledger {
        Json::Dict(d) => match d.get_mut("rounds") {
            Some(Json::List(rounds)) => Ok(rounds),
            _ => unreachable!("checked above"),
        },
        _ => unreachable!("item() found a key, so it is a dict"),
    }
}

/// `month_spent(ledger, provider, now)`: what `provider`'s Rounds started
/// in `now`'s month (UTC) cost, each its `cost` where the ledger has one,
/// else its worst case; added as `sum()` adds, so none is the int 0.
pub fn month_spent(ledger: &Json, provider: &str, now: f64) -> Result<Num, PyError> {
    let month = Utc::from_timestamp(now)
        .map(|t| t.year_month())
        .ok_or_else(|| PyError::new(py::PyErrorKind::Refused, format!("the time {now}")))?;
    let mut costs = Vec::new();
    let rounds = ledger.item("rounds")?;
    let Some(rounds) = rounds.as_list() else {
        return Err(PyError::new(
            py::PyErrorKind::Refused,
            format!("the ledger's rounds are a {}", rounds.type_name()),
        ));
    };
    for round in rounds {
        // `r["provider"] == provider and r["started_utc"].startswith(month)`,
        // the second only where the first holds.
        if round.item("provider")? != &Json::Str(provider.to_string()) {
            continue;
        }
        let started = round.item("started_utc")?;
        let Some(started) = started.as_str() else {
            return Err(PyError::new(
                py::PyErrorKind::AttributeError,
                format!(
                    "'{}' object has no attribute 'startswith'",
                    started.type_name()
                ),
            ));
        };
        if !started.starts_with(&month) {
            continue;
        }
        // `r.get("cost", r["worst_case"])`: the default is read first.
        let worst = round.item("worst_case")?;
        let cost = round.get_or("cost", worst)?;
        costs.push(match cost {
            Json::Int(i) => Num::Int(*i),
            Json::Float(f) => Num::Float(*f),
            other => {
                return Err(PyError::new(
                    py::PyErrorKind::Refused,
                    format!("a Round's cost is a {}", other.type_name()),
                ));
            }
        });
    }
    Ok(num::sum(&costs))
}

/// Writes the ledger as `json.dump(ledger, f, indent=1)` does, through a
/// temporary file of a name of its own in the ledger's directory, renamed
/// over the ledger.
fn write(path: &Path, ledger: &Json) -> Result<(), LedgerError> {
    let io = |e| LedgerError::Io(path.into(), e);
    let directory = path.parent().unwrap_or(Path::new(""));
    fs::create_dir_all(directory).map_err(io)?;
    let text = py::dumps_indent(ledger, 1);
    let (temp, mut file) = temp_file(path).map_err(io)?;
    let written = file
        .write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
        .and_then(|()| {
            drop(file);
            fs::rename(&temp, path)
        });
    if let Err(e) = written {
        let _ = fs::remove_file(&temp);
        return Err(io(e));
    }
    Ok(())
}

/// A new file beside `path`, named for this process and a count
/// ([`temp_name`]), never one that exists: never the Python's
/// `ledger.json.tmp` either, which a Python writing at the same time would
/// truncate under it.
fn temp_file(path: &Path) -> io::Result<(PathBuf, File)> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    // A few names taken by files a crash left behind are passed over; so
    // many that no name is free is an error, never a wait.
    for _ in 0..1000 {
        let temp = temp_name(path, NEXT.fetch_add(1, Ordering::Relaxed));
        match OpenOptions::new().write(true).create_new(true).open(&temp) {
            Ok(file) => return Ok((temp, file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!(
            "no free name for a temporary file beside {}",
            path.display()
        ),
    ))
}

/// The `n`-th temporary name of this process beside `path`:
/// `ledger.json.<process>-<n>.tmp`.
fn temp_name(path: &Path, n: u64) -> PathBuf {
    let name = path.file_name().map_or_else(
        || "ledger.json".into(),
        |n| n.to_string_lossy().into_owned(),
    );
    path.with_file_name(format!("{name}.{}-{n}.tmp", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of its own under the system's temporary directory,
    /// removed when the test ends.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> TempDir {
            let dir = std::env::temp_dir()
                .join(format!("bench-rounds-ledger-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_path_is_the_pythons() {
        let home = Path::new("/home/u");
        assert_eq!(
            path(None, None, home),
            Path::new("/home/u/.local/state/udpstp-bench/ledger.json")
        );
        assert_eq!(
            path(None, Some("/s"), home),
            Path::new("/s/udpstp-bench/ledger.json")
        );
        assert_eq!(
            path(None, Some(""), home),
            Path::new("udpstp-bench/ledger.json")
        );
        assert_eq!(
            path(Some(Path::new("l.json")), Some("/s"), home),
            Path::new("l.json")
        );
    }

    #[test]
    fn a_round_is_entered_first_and_closed_at_its_end() {
        let dir = TempDir::new("round");
        let file = dir.0.join("state").join("ledger.json");
        assert_eq!(load(&file).unwrap(), empty());
        let now = 1_790_000_000.25;
        start(&file, entry("r1", "aws", now, now + 7200.0, 0.64).unwrap()).unwrap();
        finish(&file, "r1", "torn down", 0.123_456_7).unwrap();
        let text = fs::read_to_string(&file).unwrap();
        assert_eq!(
            text,
            "{\n \"rounds\": [\n  {\n   \"id\": \"r1\",\n   \"provider\": \"aws\",\n   \
             \"started_utc\": \"2026-09-21T14:13:20.250000+00:00\",\n   \"expiry\": 1790007200.25,\n   \
             \"worst_case\": 0.64,\n   \"state\": \"torn down\",\n   \"hours\": 0.123\n  }\n ]\n}"
        );
        // No temporary file is left beside it.
        let names: Vec<_> = fs::read_dir(file.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["ledger.json"]);
    }

    #[test]
    fn what_the_tool_doesnt_know_is_kept_where_it_is() {
        let dir = TempDir::new("unknown");
        let file = dir.0.join("ledger.json");
        let text = "{\n \"note\": \"by hand\",\n \"rounds\": [\n  {\n   \"id\": \"r0\",\n   \
                    \"provider\": \"aws\",\n   \"cost\": 0.3,\n   \"extra\": [\n    1,\n    2.0\n   ]\n  }\n ]\n}";
        fs::write(&file, text).unwrap();
        update(&file, |_| Ok(())).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), text);
    }

    #[test]
    fn each_write_has_a_temporary_file_of_its_own() {
        // Never the Python's fixed ledger.json.tmp: a Python and a Rust
        // tool writing at once would truncate each other's file.
        let path = Path::new("state").join("ledger.json");
        let names: Vec<PathBuf> = (0..3).map(|n| temp_name(&path, n)).collect();
        let pid = std::process::id().to_string();
        for name in &names {
            assert_eq!(name.parent(), path.parent());
            let file = name.file_name().unwrap().to_string_lossy().into_owned();
            assert_ne!(file, "ledger.json.tmp");
            assert!(
                file.starts_with("ledger.json.") && file.contains(&pid),
                "{file}"
            );
        }
        assert!(names[0] != names[1] && names[1] != names[2] && names[0] != names[2]);
        // Two at once in one process get two files.
        let dir = TempDir::new("temp");
        let file = dir.0.join("ledger.json");
        let (a, _) = temp_file(&file).unwrap();
        let (b, _) = temp_file(&file).unwrap();
        assert_ne!(a, b);
        std::fs::remove_file(&a).unwrap();
        std::fs::remove_file(&b).unwrap();
        // A Python's temporary file beside the ledger is left as it is.
        let python = dir.0.join("ledger.json.tmp");
        fs::write(&python, "the Python's").unwrap();
        start(&file, entry("r1", "aws", 1e9, 1e9, 1.0).unwrap()).unwrap();
        assert_eq!(fs::read_to_string(&python).unwrap(), "the Python's");
        assert_eq!(
            load(&file)
                .unwrap()
                .item("rounds")
                .unwrap()
                .as_list()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn each_write_reads_the_ledger_again_first() {
        let dir = TempDir::new("reread");
        let file = dir.0.join("ledger.json");
        start(&file, entry("r1", "aws", 1e9, 1e9, 1.0).unwrap()).unwrap();
        // Another tool adds a Round meanwhile.
        let mut other = load(&file).unwrap();
        rounds_mut(&mut other)
            .unwrap()
            .push(entry("r2", "static", 1e9, 1e9, 0.0).unwrap());
        fs::write(&file, py::dumps_indent(&other, 1)).unwrap();
        finish(&file, "r1", "torn down", 1.0).unwrap();
        let ledger = load(&file).unwrap();
        let ids: Vec<&Json> = ledger
            .item("rounds")
            .unwrap()
            .as_list()
            .unwrap()
            .iter()
            .map(|r| r.item("id").unwrap())
            .collect();
        assert_eq!(ids, [&Json::Str("r1".into()), &Json::Str("r2".into())]);
    }

    #[test]
    fn the_month_counts_this_months_rounds() {
        // tests/test_round.py's Cli.test_the_month_counts_this_months_rounds.
        let ledger = Json::parse(
            r#"{"rounds": [
            {"provider": "aws", "started_utc": "2026-09-01T10:00:00+00:00", "worst_case": 3.0, "cost": 0.4},
            {"provider": "aws", "started_utc": "2026-09-20T10:00:00+00:00", "worst_case": 2.5},
            {"provider": "aws", "started_utc": "2026-08-31T10:00:00+00:00", "worst_case": 9.0},
            {"provider": "static", "started_utc": "2026-09-02T10:00:00+00:00", "worst_case": 0.0},
            {"provider": "other"}
        ]}"#,
        )
        .unwrap();
        let september = 1_790_000_000.0;
        assert_eq!(month_spent(&ledger, "aws", september), Ok(Num::Float(2.9)));
        // None is the int 0, which the plan prints as 0.
        assert_eq!(month_spent(&ledger, "gcp", september), Ok(Num::Int(0)));
        // The worst case is read even where a cost is given.
        let ledger = Json::parse(
            r#"{"rounds": [{"provider": "aws", "started_utc": "2026-09-01", "cost": 1.0}]}"#,
        )
        .unwrap();
        assert!(month_spent(&ledger, "aws", september).is_err());
    }
}
