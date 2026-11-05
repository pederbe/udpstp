//! Owned first-Round sampling. Registration survives an interrupted start,
//! and every signal checks the recorded process identity and work directory.

use bench_rounds::{interrupt::Flag, py::Json};
#[cfg(target_os = "linux")]
use bench_rounds::{num, py, sampler};
#[cfg(target_os = "linux")]
use indexmap::IndexMap;
#[cfg(target_os = "linux")]
use std::fs::{self, File};
use std::io;
#[cfg(target_os = "linux")]
use std::io::{Read, Seek, SeekFrom};
#[cfg(target_os = "linux")]
use std::path::Path;
#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, clap::Args)]
pub struct Args {
    #[command(subcommand)]
    operation: Operation,
}
#[derive(Debug, clap::Subcommand)]
enum Operation {
    Start {
        stem: String,
        target: String,
        seconds: f64,
        log: String,
    },
    Collect {
        stem: String,
        target: String,
        seconds: f64,
    },
    Stop {
        stem: String,
    },
}

fn error(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why)
}
#[cfg(target_os = "linux")]
fn now() -> io::Result<f64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_secs_f64())
}
#[cfg(target_os = "linux")]
fn get<'a>(value: &'a Json, key: &str) -> &'a Json {
    value
        .as_dict()
        .and_then(|v| v.get(key))
        .unwrap_or(&Json::Null)
}
#[cfg(target_os = "linux")]
fn read(path: &str) -> io::Result<Json> {
    Json::parse(&fs::read_to_string(path)?).map_err(io::Error::other)
}
#[cfg(target_os = "linux")]
fn write(path: &str, value: &Json) -> io::Result<()> {
    let temporary = format!("{path}.tmp");
    fs::write(&temporary, py::dumps(value))?;
    fs::rename(temporary, path)
}

#[cfg(target_os = "linux")]
fn identity(pid: i32) -> io::Result<Option<Json>> {
    use std::os::unix::fs::MetadataExt;
    let root = format!("/proc/{pid}");
    let collect = || -> io::Result<Json> {
        let stat = fs::read_to_string(format!("{root}/stat"))?;
        let end = stat
            .rfind(')')
            .ok_or_else(|| error("invalid process stat"))?;
        let fields = stat[end + 1..].split_whitespace().collect::<Vec<_>>();
        let start = fields.get(19).ok_or_else(|| error("short process stat"))?;
        let state = fields
            .first()
            .ok_or_else(|| error("missing process state"))?;
        Ok(Json::Dict(IndexMap::from([
            ("start".into(), Json::Str((*start).into())),
            ("uid".into(), Json::Int(fs::metadata(&root)?.uid().into())),
            (
                "cwd".into(),
                Json::Str(
                    fs::read_link(format!("{root}/cwd"))?
                        .to_string_lossy()
                        .into(),
                ),
            ),
            ("state".into(), Json::Str((*state).into())),
        ])))
    };
    match collect() {
        Ok(value) => Ok(Some(value)),
        Err(e) if matches!(e.kind(), io::ErrorKind::NotFound) => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(target_os = "linux")]
fn interrupts() -> io::Result<String> {
    let mut text = String::new();
    for path in ["/proc/softirqs", "/proc/interrupts"] {
        text.push_str(&format!("--- {path}\n"));
        text.push_str(&fs::read_to_string(path)?);
    }
    Ok(text)
}

#[cfg(target_os = "linux")]
fn collect(stem: &str, target: &str, seconds: f64, flag: &Flag) -> io::Result<()> {
    let pid = std::process::id();
    let target = if target == "self" {
        pid.to_string()
    } else {
        target.into()
    };
    // Handlers only raise a flag until registration is durably written.
    write(
        &format!("{stem}.pid.json"),
        &Json::Dict(IndexMap::from([
            ("pid".into(), Json::Int(pid.into())),
            (
                "identity".into(),
                identity(pid as i32)?.unwrap_or(Json::Null),
            ),
        ])),
    )?;
    sampler::sample(
        Path::new(&format!("{stem}.jsonl")),
        &target,
        None,
        Some(seconds),
        flag,
    )
}

#[cfg(target_os = "linux")]
fn start(stem: &str, target: &str, seconds: f64, log: &str) -> io::Result<Json> {
    if Path::new(&format!("{stem}.start.json")).exists() {
        return Err(error("capture already exists"));
    }
    if !seconds.is_finite() || seconds <= 0.0 {
        return Err(error("sample duration must be finite and positive"));
    }
    let offset = if log == "-" {
        Json::Null
    } else {
        Json::Int(fs::metadata(log)?.len().into())
    };
    write(
        &format!("{stem}.start.json"),
        &Json::Dict(IndexMap::from([
            ("pid".into(), Json::Str(target.into())),
            ("window_start".into(), Json::Float(now()?)),
            ("before".into(), Json::Str(interrupts()?)),
            ("log".into(), Json::Str(log.into())),
            ("offset".into(), offset),
        ])),
    )?;
    let stderr = File::create(format!("{stem}.stderr"))?;
    let mut child = Command::new("setsid")
        .arg("-f")
        .arg(std::env::current_exe()?)
        .args(["capture", "collect", stem, target, &num::repr(seconds)])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr)
        .spawn()?;
    // setsid -f exits after detaching the collector, so it owns no inherited pipe.
    let until = Instant::now() + Duration::from_secs(1);
    while !Path::new(&format!("{stem}.pid.json")).exists() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = child.try_wait();
    if !Path::new(&format!("{stem}.pid.json")).exists() {
        return Err(error("sampler did not register"));
    }
    Ok(Json::Dict(IndexMap::from([(
        "status".into(),
        Json::Int(0),
    )])))
}

#[cfg(target_os = "linux")]
fn stop(stem: &str) -> io::Result<Json> {
    use nix::{
        sys::signal::{Signal, kill},
        unistd::Pid,
    };
    use std::os::unix::fs::MetadataExt;
    let end = now()?;
    let after = interrupts().unwrap_or_default();
    let mut stopped = false;
    if Path::new(&format!("{stem}.pid.json")).exists() {
        let saved = read(&format!("{stem}.pid.json"))?;
        let pid = match get(&saved, "pid") {
            Json::Int(pid) => i32::try_from(*pid).ok().filter(|pid| *pid > 0),
            _ => None,
        }
        .ok_or_else(|| error("invalid saved process ID"))?;
        let old = get(&saved, "identity");
        let uid = Json::Int(fs::metadata("/proc/self")?.uid().into());
        let cwd = Json::Str(std::env::current_dir()?.to_string_lossy().into());
        let same = |value: &Json| {
            ["start", "uid", "cwd"]
                .iter()
                .all(|key| get(value, key) == get(old, key))
                && get(value, "uid") == &uid
                && get(value, "cwd") == &cwd
        };
        let ended = |value: &Option<Json>| {
            value
                .as_ref()
                .is_none_or(|value| get(value, "state").as_str() == Some("Z"))
        };
        let current = identity(pid)?;
        if ended(&current) {
            stopped = true;
        } else if current.as_ref().is_some_and(same) {
            for signal in [Signal::SIGTERM, Signal::SIGKILL] {
                let current = identity(pid)?;
                if ended(&current) {
                    stopped = true;
                    break;
                }
                if !current.as_ref().is_some_and(same) {
                    break;
                }
                match kill(Pid::from_raw(pid), signal) {
                    Ok(()) => {}
                    Err(nix::errno::Errno::ESRCH) => {
                        stopped = true;
                        break;
                    }
                    Err(e) => return Err(io::Error::other(e)),
                }
                let until = Instant::now() + Duration::from_millis(400);
                while Instant::now() < until {
                    if ended(&identity(pid)?) {
                        stopped = true;
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                if stopped {
                    break;
                }
            }
        }
    } else {
        stopped = !Path::new(&format!("{stem}.start.json")).exists();
    }
    let mut capture = IndexMap::from([
        (
            "status".into(),
            if stopped { Json::Int(0) } else { Json::Null },
        ),
        ("stopped".into(), Json::Bool(stopped)),
    ]);
    let evidence = || -> io::Result<()> {
        let begun = read(&format!("{stem}.start.json"))?;
        capture.extend([
            ("pid".into(), get(&begun, "pid").clone()),
            (
                "window".into(),
                Json::List(vec![get(&begun, "window_start").clone(), Json::Float(end)]),
            ),
            ("before".into(), get(&begun, "before").clone()),
            ("after".into(), Json::Str(after)),
            ("samples".into(), Json::Str(String::new())),
        ]);
        if stopped {
            capture.insert(
                "samples".into(),
                Json::Str(fs::read_to_string(format!("{stem}.jsonl"))?),
            );
        }
        let log = get(&begun, "log")
            .as_str()
            .ok_or_else(|| error("missing capture log"))?;
        if log != "-" {
            std::thread::sleep(Duration::from_millis(500));
            let offset = match get(&begun, "offset") {
                Json::Int(offset) => u64::try_from(*offset).map_err(io::Error::other)?,
                _ => return Err(error("invalid log offset")),
            };
            let mut file = File::open(log)?;
            if file.metadata()?.len() < offset {
                return Err(error("Server log was truncated"));
            }
            file.seek(SeekFrom::Start(offset))?;
            let mut text = String::new();
            file.read_to_string(&mut text)?;
            capture.insert("log".into(), Json::Str(text));
        }
        Ok(())
    };
    if evidence().is_err() {
        capture.insert("status".into(), Json::Null);
    }

    Ok(Json::Dict(capture))
}

pub fn run(args: &Args, flag: &Flag) -> io::Result<Option<Json>> {
    let stem = match &args.operation {
        Operation::Start { stem, .. }
        | Operation::Collect { stem, .. }
        | Operation::Stop { stem } => stem,
    };
    if !stem.starts_with("vm-")
        || !stem
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return Err(error("invalid capture stem"));
    }
    #[cfg(target_os = "linux")]
    {
        match &args.operation {
            Operation::Start {
                stem,
                target,
                seconds,
                log,
            } => start(stem, target, *seconds, log).map(Some),
            Operation::Collect {
                stem,
                target,
                seconds,
            } => collect(stem, target, *seconds, flag).map(|()| None),
            Operation::Stop { stem } => stop(stem).map(Some),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = flag;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "capture requires Linux",
        ))
    }
}
