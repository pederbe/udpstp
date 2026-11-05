//! The loopback precision suite. Its per-run gates and output retain the
//! existing suite's contract. Saved raw Client results can be judged again.

use std::fs;
use std::net::UdpSocket;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use bench_rounds::interrupt::Flag;
use bench_rounds::num::{self, Num};
use bench_rounds::process::Runner;
use bench_rounds::py::{self, Json};
use bench_rounds::secrets::SecretDir;
use indexmap::IndexMap;

use crate::scale::matrix::repr;

const KEY_FILE: &str = "[[key]]\nadmin_key_name = \"precision\"\nkey_id = 7\nkey_text = \"the precision suite's operator key\"\noperator = true\n";

#[derive(Debug, clap::Args)]
pub struct Args {
    /// The release-build udpstp binary.
    #[arg(long = "bin", required_unless_present = "replay")]
    pub binary: Option<PathBuf>,
    #[arg(long, default_value_t = 3)]
    pub runs: usize,
    #[arg(long, default_value_t = 5)]
    pub interval: u64,
    #[arg(long, num_args = 0.., default_values = ["10", "25", "50", "100", "500", "1000"])]
    pub rows: Vec<u16>,
    #[arg(long, num_args = 0.., default_values = ["1001", "1006", "1011"])]
    pub extra_rows: Vec<u16>,
    #[arg(long)]
    pub json: Option<PathBuf>,
    #[arg(long)]
    pub kernel_count: bool,
    /// Saved raw results with host, linux, kernel_tx_timestamps and runs.
    #[arg(long)]
    pub replay: Option<PathBuf>,
}

fn get<'a>(value: &'a Json, key: &str) -> &'a Json {
    value
        .as_dict()
        .and_then(|object| object.get(key))
        .unwrap_or(&Json::Null)
}

fn number(value: &Json) -> Result<Num, String> {
    Num::from_json(value).ok_or_else(|| "expected a numeric result field".into())
}

fn list(value: &Json) -> Result<&[Json], String> {
    value
        .as_list()
        .ok_or_else(|| "expected a result list".into())
}

fn numeric(value: Num) -> Json {
    match value {
        Num::Int(value) => Json::Int(value),
        Num::Float(value) => Json::Float(value),
    }
}

fn optional(value: Option<Num>) -> Json {
    value.map_or(Json::Null, numeric)
}

fn rate(row: u16) -> u16 {
    match row {
        1001 => 1100,
        1006 => 1600,
        1011 => 2100,
        _ => row,
    }
}

/// The original suite judges each run separately. The first paid Round
/// has different pre-registered aggregation rules and does not use these gates.
pub fn judge(
    result: &Json,
    row: u16,
    direction: &str,
    gated: bool,
    linux: bool,
) -> Result<(Json, Vec<String>), String> {
    let mut fields = IndexMap::from([
        ("row".into(), Json::Int(row.into())),
        ("direction".into(), Json::Str(direction.into())),
    ]);
    if let Some(error) = result.as_dict().and_then(|object| object.get("error")) {
        fields.insert("error".into(), error.clone());
        return Ok((Json::Dict(fields), vec!["no result".into()]));
    }
    let subs = list(get(get(result, "Output"), "IncrementalResult"))?;
    let caps = subs
        .iter()
        .map(|sub| number(get(sub, "IPLayerCapacity")))
        .collect::<Result<Vec<_>, _>>()?;
    let ours = get(result, "udpstp");
    let mut warnings = IndexMap::new();
    if let Json::List(values) = get(ours, "Warnings") {
        for warning in values {
            let code = get(warning, "Code").as_str().ok_or("warning has no Code")?;
            warnings.insert(code.into(), get(warning, "Count").clone());
        }
    }
    let achieved =
        (!caps.is_empty()).then(|| num::py_round(num::sum(&caps).to_f64() / caps.len() as f64, 3));
    let min = caps
        .iter()
        .copied()
        .min_by(|a, b| a.py_cmp(*b).unwrap_or(std::cmp::Ordering::Equal));
    let loss = subs
        .iter()
        .map(|sub| number(get(sub, "LossCount")))
        .collect::<Result<Vec<_>, _>>()?;
    fields.extend([
        ("mbps".into(), Json::Int(rate(row).into())),
        ("capacity_min".into(), optional(min)),
        ("capacity_max".into(), optional(num::max(&caps))),
        ("achieved".into(), achieved.map_or(Json::Null, Json::Float)),
        ("loss".into(), numeric(num::sum(&loss))),
        ("warnings".into(), Json::Dict(warnings)),
        ("send_lag".into(), get(ours, "KernelSendTimestamps").clone()),
        ("fast_paths".into(), get(ours, "FastPaths").clone()),
    ]);
    let mut failures = Vec::new();
    if gated {
        if let Some(achieved) = achieved {
            if (achieved - f64::from(row)).abs() > f64::from(row) * 0.01 {
                failures.push(format!(
                    "achieved {} Mbps, more than 1 % from {row}",
                    num::repr(achieved)
                ));
            }
        } else {
            failures.push("no Sub-Intervals".into());
        }
        if linux && direction == "upstream" {
            let stamps = get(ours, "KernelSendTimestamps");
            if stamps.is_null() {
                failures.push("no kernel send timestamps".into());
            } else if number(get(stamps, "LagP99Us"))?.to_f64() > 100.0 {
                failures.push(format!(
                    "send lag p99 {} µs over 100",
                    repr(get(stamps, "LagP99Us"))
                ));
            }
        }
    }
    Ok((Json::Dict(fields), failures))
}

fn line(figures: &Json, failures: &[String], run: usize, row: u16, direction: &str) -> String {
    let value = |key| repr(get(figures, key));
    let lag = get(figures, "send_lag");
    let stamp = |key| {
        lag.as_dict()
            .and_then(|lag| lag.get(key))
            .map_or_else(|| "-".into(), repr)
    };
    let warnings = if get(figures, "warnings").is_null() {
        get(figures, "error")
    } else {
        get(figures, "warnings")
    };
    let warnings = match warnings {
        Json::Str(text) => text.clone(),
        value => repr(value),
    };
    format!(
        "run {run} {direction:10} row {row:5} {} Mbps ({}-{}) loss {} lag p50/p99/max {}/{}/{} us {}{}",
        value("achieved"),
        value("capacity_min"),
        value("capacity_max"),
        value("loss"),
        stamp("LagP50Us"),
        stamp("LagP99Us"),
        stamp("LagMaxUs"),
        warnings,
        if failures.is_empty() {
            String::new()
        } else {
            format!("  FAIL: {}", failures.join("; "))
        }
    )
}

fn wait(flag: &Flag, duration: Duration) -> Result<(), String> {
    let until = Instant::now() + duration;
    while Instant::now() < until {
        if flag.raised() {
            return Err("precision interrupted".into());
        }
        std::thread::sleep(
            Duration::from_millis(20).min(until.saturating_duration_since(Instant::now())),
        );
    }
    Ok(())
}

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let until = Instant::now() + Duration::from_secs(2);
        while self.0.try_wait().ok().flatten().is_none() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn host() -> String {
    let output = |program: &str, args: &[&str]| {
        Command::new(program)
            .args(args)
            .output()
            .ok()
            .filter(|done| done.status.success())
            .map(|done| String::from_utf8_lossy(&done.stdout).trim().to_string())
    };
    if cfg!(target_os = "linux") {
        let name = output("uname", &["-s"]).unwrap_or_else(|| "Linux".into());
        let release = output("uname", &["-r"]).unwrap_or_default();
        let machine = output("uname", &["-m"]).unwrap_or_default();
        let libc = output("getconf", &["GNU_LIBC_VERSION"])
            .unwrap_or_default()
            .replace(' ', "");
        return format!("{name}-{release}-{machine}-with-{libc}");
    }
    if cfg!(windows)
        && let Some(version) = output("cmd.exe", &["/d", "/c", "ver"])
    {
        let parts = version
            .split(|c: char| !c.is_ascii_digit() && c != '.')
            .find(|part| part.split('.').count() >= 3)
            .map(|part| part.split('.').take(3).collect::<Vec<_>>());
        if let Some(parts) = parts
            && parts.len() == 3
        {
            let release =
                if parts[0] == "10" && parts[2].parse::<u32>().is_ok_and(|build| build >= 22000) {
                    "11"
                } else {
                    parts[0]
                };
            return format!("Windows-{release}-{}-SP0", parts.join("."));
        }
    }
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// Run the live suite, or replay a saved raw-result list without sockets.
pub fn run(args: &Args, flag: &Flag, out: &mut dyn FnMut(&str)) -> Result<u8, String> {
    let replay = args
        .replay
        .as_ref()
        .map(|path| {
            fs::read_to_string(path)
                .map_err(|e| e.to_string())
                .and_then(|text| Json::parse(&text).map_err(|e| e.to_string()))
        })
        .transpose()?;
    let stamps = if args.kernel_count {
        "kernel-count"
    } else {
        "1"
    };
    let linux = replay.as_ref().map_or(cfg!(target_os = "linux"), |saved| {
        get(saved, "linux") == &Json::Bool(true)
    });
    let mut keys_guard = None;
    let mut server_guard = None;
    let runner = Runner::new(flag.clone());
    let mut cases = Vec::new();
    let mut binary = None;
    let mut key_path = PathBuf::new();
    let mut port = 0;
    if let Some(saved) = &replay {
        cases = list(get(saved, "runs"))?.to_vec();
    } else {
        let path = args
            .binary
            .as_ref()
            .ok_or("give --bin or --replay")?
            .canonicalize()
            .map_err(|e| e.to_string())?;
        let keys = SecretDir::create().map_err(|e| e.to_string())?;
        key_path = keys.path().join("keys.toml");
        fs::write(&key_path, KEY_FILE).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600))
                .map_err(|e| e.to_string())?;
        }
        port = UdpSocket::bind("127.0.0.1:0")
            .and_then(|socket| socket.local_addr())
            .map_err(|e| e.to_string())?
            .port();
        server_guard = Some(Server(
            Command::new(&path)
                .args(["server", "--key-file"])
                .arg(&key_path)
                .args(["--port", &port.to_string(), "--max-hops", "64"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| e.to_string())?,
        ));
        keys_guard = Some(keys);
        wait(flag, Duration::from_secs(1))?;
        binary = Some(path);
        for run in 1..=args.runs {
            for direction in ["downstream", "upstream"] {
                for (rows, gated) in [(&args.rows, true), (&args.extra_rows, false)] {
                    for row in rows {
                        cases.push(Json::Dict(IndexMap::from([
                            ("row".into(), Json::Int((*row).into())),
                            ("direction".into(), Json::Str(direction.into())),
                            ("gated".into(), Json::Bool(gated)),
                            ("run".into(), Json::Int(run as i128)),
                        ])));
                    }
                }
            }
        }
    }
    let (mut results, mut failed, mut missing) = (Vec::new(), 0, false);
    for case in cases {
        let row = u16::try_from(number(get(&case, "row"))?.to_f64() as i128)
            .map_err(|_| "invalid precision row")?;
        let run = usize::try_from(number(get(&case, "run"))?.to_f64() as i128)
            .map_err(|_| "invalid repeat")?;
        let direction = get(&case, "direction")
            .as_str()
            .ok_or("missing direction")?;
        let result = if let Some(binary) = &binary {
            let mut command = Command::new(binary);
            command
                .args(["client", "--key-file"])
                .arg(&key_path)
                .args([
                    "--direction",
                    direction,
                    "--fixed-rate",
                    &row.to_string(),
                    "--test-interval",
                    &args.interval.to_string(),
                    "--format",
                    "json",
                    &format!("127.0.0.1:{port}"),
                ])
                .env("UDPSTP_KERNEL_TX_TIMESTAMPS", stamps);
            let done = runner
                .run(
                    &mut command,
                    None,
                    Duration::from_secs(args.interval.saturating_add(30)),
                )
                .map_err(|error| format!("Client did not run: {error:?}"))?;
            let stdout = done.stdout_text().map_err(|e| e.to_string())?;
            let stderr = done.stderr_text().map_err(|e| e.to_string())?;
            Json::parse(&stdout).unwrap_or_else(|_| {
                Json::Dict(IndexMap::from([(
                    "error".into(),
                    Json::Str(if stderr.trim().is_empty() {
                        format!("exit {}", done.returncode)
                    } else {
                        stderr.trim().into()
                    }),
                )]))
            })
        } else {
            get(&case, "result").clone()
        };
        let (mut figures, failures) = judge(
            &result,
            row,
            direction,
            get(&case, "gated") == &Json::Bool(true),
            linux,
        )?;
        let Json::Dict(fields) = &mut figures else {
            return Err("invalid figures".into());
        };
        fields.insert("run".into(), Json::Int(run as i128));
        missing |= !get(&figures, "error").is_null();
        failed += usize::from(!failures.is_empty());
        out(&line(&figures, &failures, run, row, direction));
        results.push(figures);
        if binary.is_some() {
            wait(flag, Duration::from_secs(1))?;
        }
    }
    drop(server_guard);
    drop(keys_guard);
    if let Some(path) = &args.json {
        let result = Json::Dict(IndexMap::from([
            (
                "host".into(),
                replay
                    .as_ref()
                    .map_or_else(|| Json::Str(host()), |saved| get(saved, "host").clone()),
            ),
            (
                "kernel_tx_timestamps".into(),
                replay.as_ref().map_or_else(
                    || Json::Str(stamps.into()),
                    |saved| get(saved, "kernel_tx_timestamps").clone(),
                ),
            ),
            ("runs".into(), Json::List(results)),
        ]));
        fs::write(path, py::dumps_indent(&result, 1)).map_err(|e| e.to_string())?;
    }
    out(&if failed > 0 {
        format!("{failed} gated run(s) failed")
    } else {
        "every gate held".into()
    });
    Ok(if missing {
        2
    } else if failed > 0 {
        1
    } else {
        0
    })
}
