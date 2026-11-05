//! Linux CPU, thread and UDP sampling. Records retain the measurement
//! scripts' JSON stream interface. The source root also accepts saved proc trees.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use indexmap::IndexMap;

use crate::interrupt::Flag;
use crate::py::{self, Json};

fn invalid(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why)
}

fn integer(text: &str) -> io::Result<i128> {
    text.parse().map_err(|_| invalid("invalid proc counter"))
}

fn stat(text: &str) -> io::Result<(&str, Vec<&str>)> {
    let begin = text
        .find('(')
        .ok_or_else(|| invalid("missing process name"))?;
    let end = text
        .rfind(')')
        .filter(|end| *end > begin)
        .ok_or_else(|| invalid("invalid process name"))?;
    Ok((
        &text[begin + 1..end],
        text[end + 1..].split_whitespace().collect(),
    ))
}

fn ticks(fields: &[&str]) -> io::Result<i128> {
    Ok(integer(
        fields
            .get(11)
            .ok_or_else(|| invalid("short process stat"))?,
    )? + integer(
        fields
            .get(12)
            .ok_or_else(|| invalid("short process stat"))?,
    )?)
}

fn udp(root: &Path, pid: &str) -> Option<Json> {
    let text = fs::read_to_string(root.join(pid).join("net/snmp")).ok()?;
    let mut lines = text
        .lines()
        .filter(|line| line.starts_with("Udp:"))
        .map(|line| line.split_whitespace().skip(1));
    let names = lines.next()?;
    let values = lines.next()?;
    let fields = names
        .zip(values)
        .map(|(name, value)| Some((name.into(), Json::Int(integer(value).ok()?))))
        .collect::<Option<IndexMap<_, _>>>()?;
    Some(Json::Dict(fields))
}

/// One complete sample, from a live or recorded Linux proc tree.
/// A vanished thread is omitted. No surviving process means no record.
pub fn snapshot(
    root: &Path,
    pids: &[String],
    client_ns: Option<&str>,
    now: f64,
) -> io::Result<Option<Json>> {
    if !pids.iter().any(|pid| root.join(pid).exists()) {
        return Ok(None);
    }
    let mut cpus = IndexMap::new();
    for line in fs::read_to_string(root.join("stat"))?.lines() {
        let mut parts = line.split_whitespace();
        let Some(name) = parts.next().and_then(|name| name.strip_prefix("cpu")) else {
            continue;
        };
        if !name.starts_with(|c: char| c.is_ascii_digit()) {
            continue;
        }
        let values = parts
            .take(8)
            .map(|value| integer(value).map(Json::Int))
            .collect::<io::Result<Vec<_>>>()?;
        cpus.insert(name.into(), Json::List(values));
    }
    let mut threads = IndexMap::new();
    let mut process = 0;
    let mut alive = Vec::new();
    for pid in pids {
        let tasks = match fs::read_dir(root.join(pid).join("task")) {
            Ok(tasks) => tasks,
            Err(_) => continue,
        };
        for task in tasks.flatten() {
            let read = || -> io::Result<Json> {
                let text = fs::read_to_string(task.path().join("stat"))?;
                let (name, fields) = stat(&text)?;
                let mut voluntary = 0;
                let mut involuntary = 0;
                for line in fs::read_to_string(task.path().join("status"))?.lines() {
                    let mut fields = line.split_whitespace();
                    match fields.next() {
                        Some("voluntary_ctxt_switches:") => {
                            voluntary = integer(fields.next().unwrap_or(""))?
                        }
                        Some("nonvoluntary_ctxt_switches:") => {
                            involuntary = integer(fields.next().unwrap_or(""))?
                        }
                        _ => {}
                    }
                }
                Ok(Json::List(vec![
                    Json::Str(name.into()),
                    Json::Int(ticks(&fields)?),
                    Json::Int(voluntary),
                    Json::Int(involuntary),
                ]))
            };
            if let Ok(value) = read() {
                let tid = task.file_name().to_string_lossy().into_owned();
                threads.insert(
                    if pids.len() > 1 {
                        format!("{pid}/{tid}")
                    } else {
                        tid
                    },
                    value,
                );
            }
        }
        let text = match fs::read_to_string(root.join(pid).join("stat")) {
            Ok(text) => text,
            Err(_) => continue,
        };
        process += ticks(&stat(&text)?.1)?;
        alive.push(pid);
    }
    let Some(first) = alive.first() else {
        return Ok(None);
    };
    let mut record = IndexMap::from([
        ("t".into(), Json::Float(now)),
        ("cpus".into(), Json::Dict(cpus)),
        ("process".into(), Json::Int(process)),
        ("threads".into(), Json::Dict(threads)),
        ("udp_server".into(), udp(root, first).unwrap_or(Json::Null)),
    ]);
    if let Some(pid) = client_ns.filter(|pid| !pid.is_empty()) {
        record.insert("udp_clients".into(), udp(root, pid).unwrap_or(Json::Null));
    }
    Ok(Some(Json::Dict(record)))
}

/// Append samples every half second until the targets exit, the duration
/// expires, or an installed signal handler raises the flag.
pub fn sample(
    out: &Path,
    servers: &str,
    client_ns: Option<&str>,
    duration: Option<f64>,
    flag: &Flag,
) -> io::Result<()> {
    if !cfg!(target_os = "linux") {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "sampling requires Linux",
        ));
    }
    if duration.is_some_and(|seconds| !seconds.is_finite() || seconds <= 0.0) {
        return Err(invalid("sample duration must be finite and positive"));
    }
    let limit = duration
        .map(|seconds| {
            Duration::try_from_secs_f64(seconds)
                .map_err(|_| invalid("sample duration is too large"))
        })
        .transpose()?;
    let deadline = limit.and_then(|limit| Instant::now().checked_add(limit));
    if duration.is_some() && deadline.is_none() {
        return Err(invalid("sample duration is too large"));
    }
    let pids: Vec<_> = servers
        .split(',')
        .filter(|pid| !pid.is_empty())
        .map(str::to_owned)
        .collect();
    if pids
        .iter()
        .map(String::as_str)
        .chain(client_ns.into_iter().filter(|pid| !pid.is_empty()))
        .any(|pid| !pid.bytes().all(|c| c.is_ascii_digit()))
    {
        return Err(invalid("sample target must be a process ID"));
    }
    let mut file = OpenOptions::new().create(true).append(true).open(out)?;
    while !flag.raised() && deadline.is_none_or(|deadline| Instant::now() < deadline) {
        let tick = Instant::now();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_secs_f64();
        let Some(record) = snapshot(Path::new("/proc"), &pids, client_ns, now)? else {
            break;
        };
        writeln!(file, "{}", py::dumps(&record))?;
        file.flush()?;
        let pause = if duration.is_none() {
            let elapsed = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(io::Error::other)?
                .as_secs_f64()
                - now;
            Duration::try_from_secs_f64((0.5 - elapsed).max(0.0)).map_err(io::Error::other)?
        } else {
            Duration::from_millis(500).saturating_sub(tick.elapsed())
        };
        let pause = deadline.map_or(pause, |deadline| {
            pause.min(deadline.saturating_duration_since(Instant::now()))
        });
        let until = Instant::now() + pause;
        while !flag.raised() && Instant::now() < until {
            std::thread::sleep(
                Duration::from_millis(20).min(until.saturating_duration_since(Instant::now())),
            );
        }
    }
    Ok(())
}
