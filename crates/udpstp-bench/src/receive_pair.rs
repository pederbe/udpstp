//! One finite Receive probe pair on existing hosts. Physical assessment and
//! paid-account resource management remain with the separately frozen plan.

use std::cell::Cell;
use std::fs;
use std::net::SocketAddrV4;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use bench_rounds::host::{Host, Limit, RemotePath, Shell};
use bench_rounds::interrupt::{Flag, RoundTeardown};
use bench_rounds::process::Runner;
use bench_rounds::provider::{Access, Leftover, Provider, ProviderError, RoundId, UnixTime};
use bench_rounds::round::{self, Bracket, Clock, Log, RoundBody, Start, Stopped, WallClock};
use bench_rounds::static_hosts::StaticHosts;
use clap::Args as ClapArgs;
use indexmap::IndexMap;
use serde::Serialize;

use crate::receive_probe::{self, ProbeResult, Readiness, ReceiveMode, Workload};

mod scripts;
#[cfg(test)]
mod tests;

const CALL: Limit = Limit::secs(15);
const STOP: Limit = Limit::secs(45);
const ROLES: [&str; 2] = ["sender", "receiver"];

#[derive(Clone, Debug, ClapArgs, Serialize)]
pub struct Args {
    /// SSH destination or local. Access uses the existing SSH configuration.
    #[arg(long)]
    pub sender_host: String,
    #[arg(long)]
    pub receiver_host: String,
    /// Explicit private IPv4 address and port for each endpoint.
    #[arg(long)]
    pub sender: SocketAddrV4,
    #[arg(long)]
    pub receiver: SocketAddrV4,
    #[arg(long)]
    pub sender_cpu: usize,
    #[arg(long)]
    pub receiver_cpu: usize,
    /// One sampler CPU outside the receiver's physical core.
    #[arg(long)]
    pub sampler_cpu: usize,
    /// Separately built Linux executable, which also supplies capture commands.
    #[arg(long)]
    pub measurement_tool: PathBuf,
    #[arg(long)]
    pub endpoint_sha256: String,
    /// Source revision of the endpoint payload, independent of this coordinator.
    #[arg(long)]
    pub endpoint_source: String,
    /// Descriptive build feature identity, retained separately from the hash.
    #[arg(long, default_value = "default")]
    pub endpoint_features: String,
    #[arg(long, default_value_t = false)]
    pub endpoint_profiling: bool,
    #[arg(long)]
    pub run_id: u64,
    #[arg(long)]
    pub count: u64,
    #[arg(long, default_value_t = 1_000_000)]
    pub pps: u64,
    #[arg(long)]
    pub deadline_ms: u64,
    #[arg(long, default_value_t = 500)]
    pub drain_ms: u64,
    #[arg(long, default_value_t = 100)]
    pub start_delay_ms: u64,
    #[arg(long, default_value_t = 16_777_216)]
    pub socket_buffer: usize,
    #[arg(long, value_enum)]
    pub mode: ReceiveMode,
    /// Maximum receiver readiness wait, within its endpoint deadline.
    #[arg(long, default_value_t = 15_000)]
    pub readiness_ms: u64,
    /// New private output directory. Existing evidence is never replaced.
    #[arg(long)]
    pub out: PathBuf,
    #[arg(long)]
    pub ssh_dir: Option<PathBuf>,
}

impl Args {
    fn workload(&self, role: &str) -> Workload {
        let receiver = role == "receiver";
        Workload {
            bind: if receiver { self.receiver } else { self.sender },
            peer: if receiver { self.sender } else { self.receiver },
            run_id: self.run_id,
            source: self.endpoint_source.clone(),
            count: self.count,
            pps: self.pps,
            deadline_ms: self.deadline_ms,
            drain_ms: self.drain_ms,
            start_delay_ms: self.start_delay_ms,
            cpu: if receiver {
                self.receiver_cpu
            } else {
                self.sender_cpu
            },
            socket_buffer: self.socket_buffer,
            out: PathBuf::from(format!("{role}/result.json")),
        }
    }

    fn validate(&self) -> Result<(), String> {
        self.workload("sender").validate()?;
        self.workload("receiver").validate()?;
        if self.sender_host.is_empty()
            || self.receiver_host.is_empty()
            || self.sender_host.starts_with('-')
            || self.receiver_host.starts_with('-')
            || self.endpoint_sha256.len() != 64
            || !self.endpoint_sha256.bytes().all(|b| b.is_ascii_hexdigit())
            || self.endpoint_features.is_empty()
            || self.endpoint_features.len() > 256
            || self.endpoint_features.chars().any(char::is_control)
            || !(1..=30_000).contains(&self.readiness_ms)
            || self.readiness_ms >= self.deadline_ms
            || self.receiver_cpu == self.sampler_cpu
        {
            return Err("explicit hosts, payload identity, distinct sampler CPU and finite readiness bound are required".into());
        }
        if receive_probe::hash_file(&self.measurement_tool)? != self.endpoint_sha256 {
            return Err("endpoint executable SHA256 differs from --endpoint-sha256".into());
        }
        Ok(())
    }
}

#[derive(Serialize, Default)]
struct Outcome {
    version: u32,
    operation_complete: bool,
    physical_acceptance: bool,
    endpoints_stopped: bool,
    sampler_stopped: bool,
    export_verified: bool,
    errors: Vec<String>,
    workspaces: Vec<(String, String, String)>,
}

/// Run exactly one pair. Exit 3 means retained workspaces need recovery.
pub fn run(args: &Args, flag: &Flag) -> Result<u8, String> {
    args.validate()?;
    fs::create_dir(&args.out).map_err(|e| format!("{}: {e}", args.out.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&args.out, fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
    }
    receive_probe::write_json(&args.out.join("plan.json"), args)?;
    let coordinator = std::env::current_exe().map_err(|e| e.to_string())?;
    receive_probe::write_json(
        &args.out.join("identities.json"),
        &serde_json::json!({
            "coordinator_source": env!("UDPSTP_BENCH_COMMIT"),
            "coordinator_sha256": receive_probe::hash_file(&coordinator)?,
            "coordinator_features": {"hotpath": cfg!(feature="hotpath"), "hotpath_alloc": cfg!(feature="hotpath-alloc")},
            "endpoint_source": args.endpoint_source,
            "endpoint_sha256": args.endpoint_sha256,
            "endpoint_features_declared": args.endpoint_features,
            "endpoint_profiling_declared": args.endpoint_profiling,
            "static_bookkeeping_usd": 0,
            "paid_account_enforcement": false,
        }),
    )?;
    let runner = Runner::new(flag.clone()).with_output_cap(64 * 1024 * 1024);
    let work = StaticHosts::work_dir(std::process::id()).map_err(|e| e.to_string())?;
    let mut hosts = Vec::new();
    for (role, destination, address) in [
        ("sender", &args.sender_host, args.sender.ip()),
        ("receiver", &args.receiver_host, args.receiver.ip()),
    ] {
        let mut h = if destination == "local" {
            Host::local(role, address.to_string(), work.clone(), runner.clone())
        } else {
            Host::ssh(
                role,
                destination,
                address.to_string(),
                work.clone(),
                runner.clone(),
            )
            .map_err(|e| e.to_string())?
        };
        if let Some(dir) = &args.ssh_dir {
            h = h.with_ssh_dir(dir);
        }
        hosts.push(h);
    }
    let safe_to_remove = Rc::new(Cell::new(false));
    let mut provider = RetainedHosts {
        inner: StaticHosts::new(hosts),
        safe_to_remove: safe_to_remove.clone(),
    };
    provider
        .check(1.0, round::wall_clock())
        .map_err(|e| e.to_string())?;
    let clock = WallClock { flag: flag.clone() };
    let log = Log::new(&args.out);
    let mut body = Pair::new(args, flag, &clock, safe_to_remove);
    let status = round::execute(
        &mut body,
        &mut provider,
        Bracket {
            ledger: &args.out.join("static-ledger.json"),
            worst_case: 0.0,
            max_hours: 1.0,
            round: None,
            flag,
            clock: &clock,
            log: &log,
            keys_in: None,
            alert: &mut round::alert,
        },
    );
    Ok(
        if status == round::Status::Done && !body.outcome.operation_complete {
            1
        } else {
            status.code()
        },
    )
}

struct Pair<'a> {
    args: &'a Args,
    flag: &'a Flag,
    clock: &'a dyn Clock,
    hosts: IndexMap<String, Box<dyn Shell>>,
    safe_to_remove: Rc<Cell<bool>>,
    outcome: Outcome,
    call: usize,
    receiver_ready: Option<Readiness>,
}

impl<'a> Pair<'a> {
    fn new(args: &'a Args, flag: &'a Flag, clock: &'a dyn Clock, safe: Rc<Cell<bool>>) -> Self {
        Self {
            args,
            flag,
            clock,
            hosts: IndexMap::new(),
            safe_to_remove: safe,
            outcome: Outcome {
                version: 1,
                ..Outcome::default()
            },
            call: 0,
            receiver_ready: None,
        }
    }

    fn command(
        &mut self,
        role: &str,
        label: &str,
        script: &str,
        limit: Limit,
    ) -> Result<String, Stopped> {
        self.call += 1;
        let stem = self
            .args
            .out
            .join(format!("command-{:03}-{role}-{label}", self.call));
        // The intended command survives an interrupted or failed transport.
        let command_record = fs::write(stem.with_extension("sh"), script);
        let result = self.hosts[role].run(script, limit);
        match result {
            Ok(done) => {
                command_record.map_err(failed)?;
                fs::write(stem.with_extension("stdout"), &done.stdout).map_err(failed)?;
                fs::write(stem.with_extension("stderr"), &done.stderr).map_err(failed)?;
                fs::write(stem.with_extension("status"), done.returncode.to_string())
                    .map_err(failed)?;
                if done.success() {
                    Ok(done.stdout)
                } else {
                    Err(failed(format!("{role} {label}: exit {}", done.returncode)))
                }
            }
            Err(e) => {
                let _ = fs::write(stem.with_extension("error"), e.to_string());
                Err(e.into())
            }
        }
    }

    fn prepare(&mut self) -> Result<(), Stopped> {
        let mut staged = Vec::new();
        for role in ROLES {
            let host = &self.hosts[role];
            let identity = (host.destination().to_owned(), host.work().clone());
            if !staged.contains(&identity) {
                let transfer = self.args.out.join(format!("stage-{role}.json"));
                receive_probe::write_json(
                    &transfer,
                    &serde_json::json!({
                        "source": self.args.measurement_tool, "sha256": self.args.endpoint_sha256,
                        "destination": host.destination(), "workspace": host.work().as_str(),
                        "name": "endpoint", "mode": "0755", "state": "intended",
                    }),
                )
                .map_err(failed)?;
                let placed = host.put(&self.args.measurement_tool, &remote("endpoint")?, 0o755);
                fs::write(
                    self.args.out.join(format!("stage-{role}.status")),
                    match &placed {
                        Ok(()) => "completed".into(),
                        Err(e) => e.to_string(),
                    },
                )
                .map_err(failed)?;
                placed?;
                staged.push(identity);
            }
            self.command(
                role,
                "prepare",
                &scripts::prepare(role, &self.args.endpoint_sha256),
                CALL,
            )?;
        }
        let placement = self.command(
            "receiver",
            "placement",
            &scripts::placement(self.args.receiver_cpu, self.args.sampler_cpu),
            CALL,
        )?;
        validate_placement(self.args, &placement)?;
        Ok(())
    }

    fn launch(&mut self, role: &str) -> Result<(), Stopped> {
        self.command(
            role,
            "launch",
            &scripts::launch(
                role,
                &scripts::endpoint_args(self.args, role),
                self.args.deadline_ms,
            ),
            CALL,
        )?;
        Ok(())
    }

    fn ready(&mut self) -> Result<(), Stopped> {
        let until = Instant::now() + Duration::from_millis(self.args.readiness_ms);
        loop {
            if self.flag.stops() {
                return Err(Stopped::Interrupted);
            }
            let text = self.command("receiver", "readiness", &scripts::readiness(), CALL)?;
            if text.starts_with("pending\n") {
                if Instant::now() >= until {
                    return Err(failed("receiver readiness timed out"));
                }
                self.clock.sleep(Duration::from_millis(50))?;
                continue;
            }
            let (proof, json) = text
                .split_once("\n---ready---\n")
                .ok_or_else(|| failed("malformed receiver readiness envelope"))?;
            let ready: Readiness = serde_json::from_str(json).map_err(failed)?;
            validate_ready(self.args, &ready, proof)?;
            self.receiver_ready = Some(ready);
            return Ok(());
        }
    }

    fn wait_endpoints(&mut self) -> Result<(), Stopped> {
        let until =
            Instant::now() + Duration::from_millis(self.args.deadline_ms) + Duration::from_secs(80);
        let mut done = [false; 2];
        loop {
            for (at, role) in ROLES.iter().enumerate() {
                if done[at] {
                    continue;
                }
                let output = self.command(role, "wait", &scripts::status(role), CALL)?;
                if output.trim() == "pending" {
                    continue;
                }
                let status: i32 = output.trim().parse().map_err(failed)?;
                if status != 0 {
                    return Err(failed(format!("{role} endpoint exited {status}")));
                }
                done[at] = true;
            }
            if done.into_iter().all(|v| v) {
                return Ok(());
            }
            if Instant::now() >= until {
                return Err(failed("endpoint lifetime exceeded its finite bound"));
            }
            self.clock.sleep(Duration::from_millis(100))?;
        }
    }

    fn finish(&mut self) -> Result<(), Stopped> {
        // Each role and phase is tried even if an earlier one failed or panicked.
        let mut stopped = true;
        for role in ROLES {
            if !self.hosts.contains_key(role) {
                continue;
            }
            let result =
                crate::cleanup::guarded(|| self.command(role, "stop", &scripts::stop(role), STOP));
            if !matches!(result, Ok(Ok(_))) {
                stopped = false;
                self.outcome
                    .errors
                    .push(format!("{role} workload stop unconfirmed: {result:?}"));
            }
        }
        self.outcome.endpoints_stopped = stopped;
        let sampler = if self.hosts.contains_key("receiver") {
            crate::cleanup::guarded(|| {
                self.command("receiver", "capture-stop", scripts::CAPTURE_STOP, CALL)
            })
        } else {
            Ok(Ok(String::new()))
        };
        self.outcome.sampler_stopped = matches!(&sampler, Ok(Ok(text)) if text.is_empty() || serde_json::from_str::<serde_json::Value>(text).ok().is_some_and(|v| v["stopped"] == true && v["status"] == 0));
        if !self.outcome.sampler_stopped {
            self.outcome
                .errors
                .push(format!("sampler stop unconfirmed: {sampler:?}"));
        }
        let mut exported = true;
        for role in ROLES {
            if !self.hosts.contains_key(role) {
                continue;
            }
            let result = crate::cleanup::guarded(|| self.export(role));
            if !matches!(result, Ok(Ok(()))) {
                exported = false;
                self.outcome
                    .errors
                    .push(format!("{role} export unconfirmed: {result:?}"));
            }
        }
        self.outcome.export_verified = exported;
        if self.outcome.operation_complete && exported {
            if let Err(e) = self.evaluate_results() {
                self.outcome.operation_complete = false;
                self.outcome
                    .errors
                    .push(format!("saved endpoint identity/evaluation failed: {e}"));
            }
        } else if !self.outcome.operation_complete {
            self.outcome.errors.push(
                "pair stopped before both endpoints ended; see round.log and command receipts"
                    .into(),
            );
        }
        let safe = stopped && self.outcome.sampler_stopped && exported;
        receive_probe::write_json(&self.args.out.join("outcome.json"), &self.outcome)
            .map_err(failed)?;
        self.safe_to_remove.set(safe);
        if safe {
            Ok(())
        } else {
            Err(failed(
                "owned processes or evidence need recovery in retained workspaces",
            ))
        }
    }

    fn evaluate_results(&self) -> Result<(), String> {
        let sender: ProbeResult =
            receive_probe::read_json(&self.args.out.join("sender/result.json"))?;
        let receiver: ProbeResult =
            receive_probe::read_json(&self.args.out.join("receiver/result.json"))?;
        for (role, result) in [("sender", &sender), ("receiver", &receiver)] {
            let mut expected = self.args.workload(role);
            expected.out = PathBuf::from("result.json");
            if result.version != receive_probe::VERSION
                || result.role != role
                || result.binary_sha256 != self.args.endpoint_sha256
                || result.profiling_enabled != self.args.endpoint_profiling
                || serde_json::to_value(&result.workload).map_err(|e| e.to_string())?
                    != serde_json::to_value(expected).map_err(|e| e.to_string())?
                || result.mode
                    != if role == "receiver" {
                        Some(self.args.mode)
                    } else {
                        None
                    }
            {
                return Err(format!(
                    "{role} saved identity differs from the pinned pair"
                ));
            }
        }
        if self
            .receiver_ready
            .as_ref()
            .is_none_or(|ready| ready.result_id != receiver.result_id)
        {
            return Err("receiver result does not match its admitted readiness".into());
        }
        receive_probe::write_json(
            &self.args.out.join("evaluation.json"),
            &receive_probe::evaluate(&sender, &receiver, None),
        )
    }

    fn export(&mut self, role: &str) -> Result<(), Stopped> {
        // A stopped workspace has no writers. Verify every regular evidence file
        // after the maintained bounded archive fetch and before authorizing removal.
        self.command(role, "manifest", &scripts::manifest(role), CALL)?;
        self.hosts[role].fetch_dir(&remote(role)?, &self.args.out.join(role))?;
        verify_manifest(&self.args.out.join(role))?;
        Ok(())
    }
}

impl RoundBody for Pair<'_> {
    fn run(&mut self, provider: &mut dyn Provider, start: &Start<'_>) -> Result<(), Stopped> {
        self.hosts = provider.create(start.round, start.expiry, &ROLES, &Access::default())?;
        self.outcome.workspaces = self
            .hosts
            .iter()
            .map(|(role, host)| {
                (
                    role.clone(),
                    host.destination().into(),
                    host.work().to_string(),
                )
            })
            .collect();
        receive_probe::write_json(
            &self.args.out.join("workspaces.json"),
            &self.outcome.workspaces,
        )
        .map_err(failed)?;
        self.prepare()?;
        self.launch("receiver")?;
        self.ready()?;
        let pid = self.receiver_ready.as_ref().expect("ready checked").pid;
        self.command(
            "receiver",
            "capture-start",
            &scripts::capture_start(self.args, pid),
            CALL,
        )?;
        // Re-read immediately before sender admission, after sampler startup.
        self.ready()?;
        self.launch("sender")?;
        self.wait_endpoints()?;
        self.outcome.operation_complete = true;
        Ok(())
    }
    fn stop_servers(&mut self, _: &RoundTeardown, log: &Log) {
        if let Err(e) = self.finish() {
            log.line(&e.to_string());
        }
    }
    fn made(&self) -> String {
        "one Receive probe pair; physical acceptance not assessed".into()
    }
}

fn validate_placement(args: &Args, text: &str) -> Result<(), Stopped> {
    let parse = |line: Option<&str>, role: &str, cpu: usize| -> Option<(i32, i32)> {
        let mut words = line?.split_whitespace();
        if words.next()? != format!("{role}_cpu={cpu}") {
            return None;
        }
        let package = words
            .next()?
            .strip_prefix("package=")?
            .parse::<i32>()
            .ok()?;
        let core = words.next()?.strip_prefix("core=")?.parse::<i32>().ok()?;
        (package >= 0 && core >= 0 && words.next().is_none()).then_some((package, core))
    };
    let mut lines = text.lines();
    let receiver = parse(lines.next(), "receiver", args.receiver_cpu);
    let sampler = parse(lines.next(), "sampler", args.sampler_cpu);
    if receiver.is_none() || sampler.is_none() || receiver == sampler {
        return Err(failed(
            "receiver and sampler require known nonnegative package/core IDs on different physical cores",
        ));
    }
    Ok(())
}

fn owns_udp_socket(fds: &str, table: &str, local: SocketAddrV4, peer: SocketAddrV4) -> bool {
    // Linux's IPv4 /proc table prints each address as a little-endian hex
    // word on the supported Linux endpoint architectures, followed by its port.
    let address = |a: SocketAddrV4| {
        format!(
            "{:08X}:{:04X}",
            u32::from_le_bytes(a.ip().octets()),
            a.port()
        )
    };
    let local = address(local);
    let peer = address(peer);
    table.lines().any(|line| {
        let columns = line.split_whitespace().take(10).collect::<Vec<_>>();
        columns.len() == 10
            && columns[1] == local
            && columns[2] == peer
            && columns[3] == "01"
            && fds.lines().any(|fd| {
                fd.strip_prefix("socket:[")
                    .and_then(|s| s.strip_suffix(']'))
                    .is_some_and(|inode| {
                        inode.parse::<u64>().is_ok_and(|n| n > 0) && inode == columns[9]
                    })
            })
    })
}

fn validate_ready(args: &Args, ready: &Readiness, proof: &str) -> Result<(), Stopped> {
    let (identity, sockets) = proof
        .split_once("\n---fds---\n")
        .ok_or_else(|| failed("missing receiver socket ownership receipt"))?;
    let (fds, udp) = sockets
        .split_once("\n---udp---\n")
        .ok_or_else(|| failed("missing receiver UDP table receipt"))?;
    let expected_args = scripts::argv_hex(&scripts::endpoint_args(args, "receiver"));
    let expected = format!("{}\n{}\n{}", ready.pid, args.endpoint_sha256, expected_args);
    if ready.version != receive_probe::VERSION
        || ready.pid == 0
        || identity != expected
        || !owns_udp_socket(fds, udp, args.receiver, args.sender)
        || !ready
            .result_id
            .starts_with(&format!("{}-{}-r-", args.run_id, ready.pid))
        || ready.socket.local != args.receiver.to_string()
        || ready.socket.peer != args.sender.to_string()
        || ready.socket.family != "IPv4"
        || ready.socket.payload_bytes != receive_probe::PAYLOAD
        || ready.socket.ipv4_bytes != receive_probe::IP_BYTES
        || ready.socket.df != Some(true)
        || ready.socket.udp_gso != Some(0)
        || ready.socket.udp_gro != Some(args.mode == ReceiveMode::Gro)
    {
        return Err(failed(
            "receiver readiness does not match the live owned endpoint/socket, source, run, shape, peers or mode",
        ));
    }
    Ok(())
}

fn verify_manifest(directory: &Path) -> Result<(), Stopped> {
    let text = fs::read_to_string(directory.join("SHA256SUMS")).map_err(failed)?;
    if text.is_empty() {
        return Err(failed("empty evidence manifest"));
    }
    for line in text.lines() {
        let (hash, file) = line
            .split_once("  ./")
            .ok_or_else(|| failed("malformed evidence manifest"))?;
        if Path::new(file)
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
            || hash.len() != 64
            || receive_probe::hash_file(&directory.join(file)).map_err(failed)? != hash
        {
            return Err(failed("exported evidence SHA256 mismatch"));
        }
    }
    Ok(())
}

fn remote(s: &str) -> Result<RemotePath, Stopped> {
    RemotePath::new(s).map_err(failed)
}
fn failed(e: impl std::fmt::Display) -> Stopped {
    Stopped::Failed(e.to_string())
}

// StaticHosts owns allocation/removal. This adapter adds only the diagnostic's
// prerequisite that stopped writers and verified exports precede removal.
struct RetainedHosts {
    inner: StaticHosts,
    safe_to_remove: Rc<Cell<bool>>,
}
impl Provider for RetainedHosts {
    fn name(&self) -> &'static str {
        self.inner.name()
    }
    fn rented(&self) -> bool {
        false
    }
    fn price_per_hour(&self, t: &str) -> Result<f64, ProviderError> {
        self.inner.price_per_hour(t)
    }
    fn check(&mut self, h: f64, now: UnixTime) -> Result<(), ProviderError> {
        self.inner.check(h, now)
    }
    fn reap(&mut self, now: UnixTime) -> Result<Vec<RoundId>, ProviderError> {
        self.inner.reap(now)
    }
    fn create(
        &mut self,
        r: &RoundId,
        e: UnixTime,
        roles: &[&str],
        a: &Access,
    ) -> Result<IndexMap<String, Box<dyn Shell>>, ProviderError> {
        self.inner.create(r, e, roles, a)
    }
    fn destroy(&mut self, r: &RoundId) -> Result<Vec<Leftover>, ProviderError> {
        if self.safe_to_remove.get() {
            self.inner.destroy(r)
        } else {
            Ok(self.retained())
        }
    }
    fn leftovers(&mut self, r: &RoundId) -> Result<Vec<Leftover>, ProviderError> {
        if self.safe_to_remove.get() {
            self.inner.leftovers(r)
        } else {
            Ok(self.retained())
        }
    }
    fn recheck_after(&self) -> Option<Duration> {
        None
    }
    fn manual_cleanup(&self, _: &RoundId) -> Vec<String> {
        vec!["Retain the workspaces until owned processes are stopped and their evidence is exported and verified.".into()]
    }
}
impl RetainedHosts {
    fn retained(&self) -> Vec<Leftover> {
        self.inner
            .hosts()
            .iter()
            .map(|h| {
                Leftover::new(
                    "unconfirmed workspace",
                    format!("{}:{}", h.destination(), h.work()),
                )
            })
            .collect()
    }
}
