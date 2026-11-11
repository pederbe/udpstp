//! The `udpstp-bench` command line (stage C6): `udpstp-bench round`
//! with `scripts/bench/round.py`'s options, names and defaults, so that the
//! runbooks change only the program's name; and `udpstp-bench report OUT`,
//! `report.py`'s report on standard output.
//!
//! `round` plans a Round as `round.py`'s `run` does: the matrix read and
//! checked, the provider built, the worst case priced against the caps and
//! the ledger, the plan printed, the provider's preflight (and its reaper
//! of expired Rounds); then, with `--yes`, the Round, which
//! `bench_rounds::round::execute` runs around the first Round's body. Its
//! exit status: 0 done, 1 stopped, 2 refused before anything was created,
//! 3 something left to delete by hand.
//!
//! Server-scale matrices use their own body inside the same Round bracket.
//! - **A real account needs `--live`.** Without it `--provider aws` is
//!   refused before anything is built, and only [`build_provider`] with
//!   `--live` makes the real CLI ([`Cli::live`]); a test hands in a
//!   factory instead, which makes its provider on the fake. No test passes
//!   the option, which `tests/cli.rs` checks by scanning both crates'
//!   sources.
//! - **Paid Rounds run from Linux or WSL:** `--provider aws --yes` is
//!   refused on Windows itself, where closing the console ends the process
//!   before its Round Teardown has ended the hosts it rented (§3.9).
//! - **A paid Round's calls are recorded where `--record-calls` asks**
//!   (§4; for the Rust tool's first AWS Round), privately, outside the
//!   checkout and `--out`: the Round's ID is made before its preflight, so
//!   that the records hold every call, and a Round whose calls can't be
//!   recorded is refused before anything is created. Where recording ends
//!   during the Round, the Round goes on and says why, in red, at its end.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use bench_rounds::aws::cli::{Cli, HostsRunner};
use bench_rounds::aws::fake;
use bench_rounds::aws::record::{Recorder, calls_directory};
use bench_rounds::aws::{AwsProvider, Calls};
use bench_rounds::cost::{Prices, RunTransfer, WorstCase, check_caps, ledger};
use bench_rounds::host::{Host, Shell};
use bench_rounds::interrupt::Flag;
use bench_rounds::num::{self, Num};
use bench_rounds::process::Runner;
use bench_rounds::provider::{Provider, ProviderError, RoundId};
use bench_rounds::py::{self, Json};
use bench_rounds::round::{self, Bracket, Clock, Log, RoundBody};
use bench_rounds::secrets;
use bench_rounds::static_hosts::StaticHosts;
use clap::{Parser, Subcommand};
use indexmap::IndexMap;

use crate::first_round::matrix::{self, ClientHost, RoundRun};
use crate::first_round::report::{self, OutDir};
use crate::first_round::round::{FirstRound, Obudpst, Options};
use crate::obudpst;
use crate::{proxy, scale};

/// The command line.
#[derive(Debug, Parser)]
#[command(name = "udpstp-bench", about = "udpstp's benchmark Rounds")]
pub struct Command {
    #[command(subcommand)]
    what: What,
}

#[derive(Debug, Subcommand)]
enum What {
    /// Start, collect or stop an owned first-Round host capture.
    Capture(crate::first_round::capture::Args),
    /// Report a saved earlier Server-scale campaign.
    CampaignReport {
        out: PathBuf,
        server_cpus: String,
        client_cpus: String,
    },
    /// Run the loopback precision suite or replay saved raw results.
    Precision(crate::precision::Args),
    /// Run a finite Linux receive workload or evaluate its saved evidence.
    ReceiveProbe(crate::receive_probe::Args),
    /// Append Linux CPU, thread and UDP samples until the targets exit.
    Sample {
        out: PathBuf,
        servers: String,
        client_ns: Option<String>,
        duration: Option<f64>,
    },
    /// One benchmark Round: provision, run a matrix of Tests, tear down.
    /// Without --yes it checks everything it can without creating
    /// anything, prints the plan and its worst-case cost, and stops.
    Round(Box<RoundArgs>),
    /// The first Round's report of the Round in OUT, on standard output.
    Report {
        /// The Round's --out.
        out: PathBuf,
    },
    /// The Server-scale Round's report, on standard output.
    ScaleReport {
        out: PathBuf,
        /// The first Round used for comparison.
        #[arg(long)]
        first: Option<PathBuf>,
    },
    /// Read a loopback proxy's recorded output.
    ProxyReport {
        out: PathBuf,
        #[arg(long)]
        feasibility: bool,
    },
}

/// `round.py`'s options, names and defaults (`round.py:566-613`).
#[derive(Clone, Debug, clap::Args)]
pub struct RoundArgs {
    #[arg(long, value_parser = ["static", "aws"])]
    pub provider: String,
    /// Allow a real cloud account.
    #[arg(long)]
    pub live: bool,
    /// Create and run; else plan only.
    #[arg(long)]
    pub yes: bool,
    #[arg(long)]
    pub matrix: PathBuf,
    #[arg(long)]
    pub out: PathBuf,
    /// The udpstp binary for the hosts.
    #[arg(long)]
    pub udpstp: PathBuf,
    /// The udpstp binary for this machine, if other.
    #[arg(long)]
    pub udpstp_home: Option<PathBuf>,
    /// Linux udpstp-bench built for the hosts' architecture and libc.
    /// Defaults to this executable. The local home role uses this executable.
    #[arg(long)]
    pub measurement_tool: Option<PathBuf>,
    /// OB-UDPST's udpst binary for the hosts, or "build" to build it on
    /// each host from the pinned source.
    #[arg(long)]
    pub obudpst: Option<String>,
    /// Server-scale Round: the  prototype's dpproto binary for the
    /// hosts.
    #[arg(long)]
    pub prototype: Option<PathBuf>,
    /// Static: ROLE=SSH,ADDRESS.
    #[arg(long)]
    pub host: Vec<String>,
    #[arg(long, default_value = "eu-north-1")]
    pub region: String,
    #[arg(long, default_value = "eu-north-1a")]
    pub zone: String,
    /// The Server's instance type [c6in.large].
    #[arg(long)]
    pub server_type: Option<String>,
    /// The Client hosts' instance type [as --server-type].
    #[arg(long)]
    pub client_type: Option<String>,
    /// AWS: this machine's public IPv4 address/32.
    #[arg(long)]
    pub home_cidr: Option<String>,
    /// Run the matrix's home Tests.
    #[arg(long)]
    pub home_client: bool,
    #[arg(long, default_value_t = 250)]
    pub home_cap_mbps: i64,
    #[arg(long, default_value_t = 2.0)]
    pub max_hours: f64,
    /// $.
    #[arg(long, default_value_t = 3.0)]
    pub round_cap: f64,
    /// $.
    #[arg(long, default_value_t = 20.0)]
    pub month_cap: f64,
    #[arg(long)]
    pub ledger: Option<PathBuf>,
    /// The pinned prices [scripts/bench/prices.toml].
    #[arg(long)]
    pub prices: Option<PathBuf>,
    /// The directory of the ssh and scp to use.
    #[arg(long)]
    pub ssh_dir: Option<PathBuf>,
    /// AWS: ask EC2 (--dry-run) whether this identity may run a Round,
    /// and stop.
    #[arg(long)]
    pub check_permissions: bool,
    /// The udpstp Server's port (the Standard Port by default).
    #[arg(long, default_value_t = 24601)]
    pub udpstp_port: u16,
    /// OB-UDPST's Server port (its default).
    #[arg(long, default_value_t = 25000)]
    pub obudpst_port: u16,
    /// Raise the hosts' socket-buffer limits (sudo -n).
    #[arg(long)]
    pub raise_buffers: bool,
    /// Server-scale Round: hold the NIC's queue interrupts where the plan
    /// says.
    #[arg(long)]
    pub hold_irqs: bool,
    /// Server-scale Round on hosts that exist: take them as found.
    #[arg(long)]
    pub as_found: bool,
    /// A paid Round (--provider aws --live --yes): keep the raw records of
    /// the Round's calls to AWS, privately, for the fake's replay.
    #[arg(long)]
    pub record_calls: bool,
}

/// A provider a Round rents its hosts from: one that can also say whether
/// this identity may run a Round, and whether its calls' recording ended
/// early.
pub trait Rented: Provider {
    /// `check_permissions()`: each action's verdict, `allowed` or AWS's
    /// error.
    fn check_permissions(&mut self) -> Result<IndexMap<String, String>, ProviderError>;

    /// Why the recording of its calls ended early, if it did.
    fn unrecorded(&self) -> Option<String>;
}

/// What the AWS provider's calls go through: the `aws` CLI, or a test's
/// model of EC2.
pub trait RoundCalls: Calls + HostsRunner {
    /// Why the recording of the calls ended early, if it did.
    fn unrecorded(&self) -> Option<String> {
        None
    }
}

impl RoundCalls for Cli {
    fn unrecorded(&self) -> Option<String> {
        Cli::unrecorded(self).map(ToString::to_string)
    }
}

/// The in-process model, for tests.
impl RoundCalls for fake::Local {}

impl<C: RoundCalls> Rented for AwsProvider<C> {
    fn check_permissions(&mut self) -> Result<IndexMap<String, String>, ProviderError> {
        Ok(AwsProvider::check_permissions(self, "permission-check")?)
    }

    fn unrecorded(&self) -> Option<String> {
        self.calls.unrecorded()
    }
}

/// The Round's provider, as built.
pub enum Built {
    Static(StaticHosts),
    Rented(Box<dyn Rented>),
}

impl Built {
    fn provider(&mut self) -> &mut dyn Provider {
        match self {
            Built::Static(hosts) => hosts,
            Built::Rented(rented) => rented.as_mut(),
        }
    }
}

/// What a provider is built from, besides the options.
pub struct Setup<'a> {
    /// The pinned prices, `prices.toml`.
    pub prices: &'a toml::Table,
    /// Each role's instance type.
    pub types: &'a IndexMap<String, String>,
    /// Whether the Round has Tests from home, whose UDP a rented host's
    /// security group admits.
    pub home_udp: bool,
    /// What runs the Round's commands.
    pub runner: &'a Runner,
    /// The Round's ID, where its calls are to be recorded (`--live --yes
    /// --record-calls`).
    pub recorded_as: Option<&'a RoundId>,
    /// `$XDG_STATE_HOME`, and the home directory: where the records go.
    pub xdg_state_home: Option<&'a str>,
    pub home: &'a Path,
}

/// Builds the Round's provider: a test's, which makes it on the fake.
pub type Factory<'a> = &'a dyn Fn(&RoundArgs, &Setup) -> Result<Built, String>;

/// What the command line runs in: the flag the signals raise, the clock,
/// the terminal, and where the tools keep their state.
pub struct Env<'a> {
    pub flag: Flag,
    pub clock: &'a dyn Clock,
    /// Standard output, a line at a time.
    pub out: &'a mut dyn FnMut(&str),
    /// Standard error, a line at a time.
    pub err: &'a mut dyn FnMut(&str),
    /// A test's provider, in [`build_provider`]'s place; `None` in a
    /// Round, where only `--live` reaches a real account.
    pub factory: Option<Factory<'a>>,
    /// Whether this runs on Windows itself.
    pub native_windows: bool,
    /// `$XDG_STATE_HOME`, and the home directory: where the ledger and a
    /// paid Round's records go.
    pub xdg_state_home: Option<String>,
    pub home: PathBuf,
    /// This process's ID, which names a static Round's work directory.
    pub pid: u32,
    /// Where the Round's key directory goes: the system's temporary
    /// directory where `None`.
    pub keys_in: Option<PathBuf>,
    /// A test's: the Round ID's four hex digits, which are random in a
    /// Round.
    pub round_suffix: Option<String>,
    /// A test's: the rented hosts' SSH key, which `ssh-keygen` makes in a
    /// Round.
    pub ssh_key: Option<&'a MakeKey>,
}

/// Makes a rented host's SSH key in the Round's key directory.
pub use crate::first_round::round::MakeKey;

/// Why a Round is refused before it is created: exit status 2.
#[derive(Debug)]
struct Refused(String);

impl From<String> for Refused {
    fn from(why: String) -> Refused {
        Refused(why)
    }
}

impl From<&str> for Refused {
    fn from(why: &str) -> Refused {
        Refused(why.to_string())
    }
}

/// `argv` run, `argv[0]` the program: the exit status.
pub fn main(argv: Vec<OsString>, env: &mut Env) -> u8 {
    let command = match Command::try_parse_from(argv) {
        Ok(command) => command,
        Err(e) => {
            let code = e.exit_code();
            (env.err)(e.render().to_string().trim_end());
            return u8::try_from(code).unwrap_or(2);
        }
    };
    match command.what {
        What::Capture(args) => match crate::first_round::capture::run(&args, &env.flag) {
            Ok(Some(value)) => {
                (env.out)(&py::dumps(&value));
                0
            }
            Ok(None) => 0,
            Err(error) => {
                (env.err)(&format!("udpstp-bench capture: {error}"));
                1
            }
        },
        What::CampaignReport {
            out,
            server_cpus,
            client_cpus,
        } => print_report(
            "campaign-report",
            crate::campaign::report(&out, &server_cpus, &client_cpus).map_err(scale::Error),
            env,
        ),
        What::Precision(args) => match crate::precision::run(&args, &env.flag, env.out) {
            Ok(code) => code,
            Err(error) => {
                (env.err)(&format!("udpstp-bench precision: {error}"));
                2
            }
        },
        What::ReceiveProbe(args) => match match &args.operation {
            crate::receive_probe::Operation::Pair(pair) => {
                crate::receive_pair::run(pair, &env.flag)
            }
            _ => crate::receive_probe::run(&args, &env.flag).map(|()| 0),
        } {
            Ok(code) => code,
            Err(error) => {
                (env.err)(&format!("udpstp-bench receive-probe: {error}"));
                2
            }
        },
        What::Sample {
            out,
            servers,
            client_ns,
            duration,
        } => {
            match bench_rounds::sampler::sample(
                &out,
                &servers,
                client_ns.as_deref(),
                duration,
                &env.flag,
            ) {
                Ok(()) => 0,
                Err(error) => {
                    (env.err)(&format!("udpstp-bench sample: {error}"));
                    1
                }
            }
        }
        What::Round(args) => match run(&args, env) {
            Ok(code) => code,
            Err(Refused(why)) => {
                (env.err)(&format!("udpstp-bench round: refused: {why}"));
                2
            }
        },
        What::Report { out } => {
            let made = OutDir::new(&out).and_then(|files| report::report(&files));
            match made {
                Ok(text) => {
                    (env.out)(text.strip_suffix('\n').unwrap_or(&text));
                    0
                }
                Err(e) => {
                    (env.err)(&format!("udpstp-bench report: no report: {e}"));
                    1
                }
            }
        }
        What::ScaleReport { out, first } => {
            let made = (|| {
                let out = scale::report::RoundFiles::load(&out)?;
                let first = first
                    .as_deref()
                    .map(scale::report::RoundFiles::load)
                    .transpose()?;
                scale::report::report(&out, first.as_ref())
            })();
            print_report("scale-report", made, env)
        }
        What::ProxyReport { out, feasibility } => {
            let made = (|| {
                let bytes = fs::read(&out).map_err(|e| scale::Error(e.to_string()))?;
                // Python opens this one with errors="replace" and universal newlines.
                let source = String::from_utf8_lossy(&bytes)
                    .replace("\r\n", "\n")
                    .replace('\r', "\n");
                let runs = proxy::parse(&py::file_lines(&source))?;
                let lines = if feasibility {
                    proxy::feasibility(&runs)?.1
                } else {
                    proxy::reading(&runs)?.1
                };
                Ok(lines.join("\n") + "\n")
            })();
            print_report("proxy-report", made, env)
        }
    }
}

fn print_report(command: &str, result: Result<String, scale::Error>, env: &mut Env) -> u8 {
    match result {
        Ok(text) => {
            (env.out)(text.strip_suffix('\n').unwrap_or(&text));
            0
        }
        Err(error) => {
            (env.err)(&format!("udpstp-bench {command}: no report: {error}"));
            1
        }
    }
}

/// The pinned prices beside the tool's source, as the Python finds them
/// beside itself.
pub fn default_prices() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/bench/prices.toml")
}

enum Loaded {
    First(Vec<RoundRun>),
    Scale(Box<scale::matrix::Matrix>),
}

impl Loaded {
    fn scale(&self) -> Option<&scale::matrix::Matrix> {
        match self {
            Self::Scale(matrix) => Some(matrix),
            Self::First(_) => None,
        }
    }
    fn len(&self) -> usize {
        match self {
            Self::First(runs) => runs.len(),
            Self::Scale(m) => m.runs.len(),
        }
    }
    fn home_udp(&self) -> bool {
        matches!(self, Self::First(runs) if runs.iter().any(|r| r.client_host == ClientHost::Home))
    }
    fn cloud_client(&self) -> bool {
        matches!(self, Self::First(runs) if runs.iter().any(|r| r.client_host == ClientHost::Cloud))
    }
    fn roles(&self) -> Vec<String> {
        match self {
            Self::First(runs) => matrix::roles(runs).into_iter().map(String::from).collect(),
            Self::Scale(_) => scale::round::ROLES.into_iter().map(String::from).collect(),
        }
    }
    fn hours(&self) -> f64 {
        match self {
            Self::First(runs) => matrix::matrix_hours(runs),
            Self::Scale(m) => num::py_round(scale::matrix::duration_s(&m.runs) / 3600.0, 2),
        }
    }
    fn check(&self, args: &RoundArgs) -> Result<(), Refused> {
        match self {
            Self::First(runs) => Ok(matrix::check(runs, args.max_hours, args.home_cap_mbps)?),
            Self::Scale(m) => {
                scale::matrix::check_execution(m).map_err(|error| error.to_string())?;
                let needed = scale::matrix::duration_s(&m.runs) + matrix::SETUP_S as f64;
                if needed > args.max_hours * 3600.0 {
                    return Err(Refused(format!(
                        "the matrix takes about {} h with setup, more than --max-hours {}",
                        num::fixed(needed / 3600.0, 2),
                        num::repr(args.max_hours)
                    )));
                }
                Ok(())
            }
        }
    }
    fn transfers(&self, cap: i64) -> Vec<RunTransfer> {
        match self {
            Self::First(runs) => matrix::transfers(runs, cap),
            Self::Scale(m) => m
                .runs
                .iter()
                .map(|r| RunTransfer {
                    home_downstream: None,
                    download_gb: r.download_gb(),
                })
                .collect(),
        }
    }
}

fn load_matrix(path: &Path) -> Result<Loaded, Refused> {
    let refused = |why: String| Refused(format!("{}: {why}", path.display()));
    let text = fs::read_to_string(path).map_err(|e| refused(e.to_string()))?;
    let server_scale = toml::from_str::<toml::Table>(&text)
        .ok()
        .and_then(|table| table.get("round").cloned())
        .and_then(|round| round.as_table().and_then(|r| r.get("kind").cloned()))
        .is_some_and(|kind| kind.as_str() == Some("server-scale"));
    if server_scale {
        return scale::matrix::parse(&text)
            .map(|m| Loaded::Scale(Box::new(m)))
            .map_err(|e| refused(e.to_string()));
    }
    matrix::parse(&text)
        .map(Loaded::First)
        .map_err(|e| refused(e.to_string()))
}

/// Each role's instance type: `--server-type` and `--client-type`, else
/// `c6in.large` (`host_types`).
fn host_types(
    roles: &[String],
    args: &RoundArgs,
    scale: Option<&scale::matrix::Matrix>,
) -> IndexMap<String, String> {
    let setting = |key| scale.and_then(|m| scale::matrix::get(&m.settings, key).as_str());
    let server = args
        .server_type
        .as_deref()
        .or_else(|| setting("server_type"))
        .unwrap_or("c6in.large");
    let client = args
        .client_type
        .as_deref()
        .or_else(|| setting("client_type"))
        .unwrap_or("c6in.large");
    roles
        .iter()
        .map(|role| {
            let t = if role == "server" { server } else { client };
            (role.clone(), t.to_string())
        })
        .collect()
}

fn scale_options(args: &RoundArgs, matrix: &scale::matrix::Matrix) -> Result<(), Refused> {
    if args.as_found {
        if args.provider != "static" {
            return Err(Refused::from(
                "--as-found is for hosts that exist (--provider static): rented hosts take the plan's settings",
            ));
        }
        if args.hold_irqs || args.raise_buffers {
            return Err(Refused::from(
                "--as-found changes nothing on the hosts: give it without --hold-irqs and --raise-buffers",
            ));
        }
    }
    if scale::matrix::get(&matrix.settings, "keep_server_binaries").truthy()
        && args.provider != "static"
    {
        return Err(Refused::from(
            "keep_server_binaries is for hosts that exist (--provider static): a rented host goes with its binaries",
        ));
    }
    let mut missing = Vec::new();
    if matrix
        .runs
        .iter()
        .any(|r| r.engine.starts_with("prototype-"))
        && args.prototype.is_none()
    {
        missing.push("--prototype PATH (dpproto, built for the hosts)");
    }
    if matrix.runs.iter().any(|r| r.engine == "obudpst")
        && args.obudpst.as_deref().is_none_or(str::is_empty)
    {
        missing.push("--obudpst PATH, or --obudpst build");
    }
    if !args.as_found {
        if !args.hold_irqs {
            missing.push("--hold-irqs");
        }
        if !args.raise_buffers {
            missing.push("--raise-buffers");
        }
    }
    if !missing.is_empty() {
        let instead = if args.provider == "static" && missing.contains(&"--hold-irqs") {
            "; on hosts that exist, --as-found takes them as they are instead of --hold-irqs and --raise-buffers"
        } else {
            ""
        };
        return Err(Refused(format!(
            "the Server-scale round uses its defined matrix: give {}{instead}",
            missing.join(", ")
        )));
    }
    Ok(())
}

/// The Round's provider (`build_provider`): the static hosts `--host`
/// names, or AWS's through the real `aws` CLI, which only `--live` makes.
pub fn build_provider(args: &RoundArgs, setup: &Setup, pid: u32) -> Result<Built, String> {
    match args.provider.as_str() {
        "static" => {
            let work = StaticHosts::work_dir(pid).map_err(|e| format!("workspace name: {e}"))?;
            let mut hosts = Vec::new();
            for spec in &args.host {
                let (role, rest) = spec.split_once('=').unwrap_or((spec, ""));
                let (ssh, address) = rest.split_once(',').unwrap_or((rest, ""));
                let host = if ssh == "local" {
                    Host::local(role, address, work.clone(), setup.runner.clone())
                } else {
                    let mut host =
                        Host::ssh(role, ssh, address, work.clone(), setup.runner.clone())
                            .map_err(|e| e.to_string())?;
                    if let Some(dir) = &args.ssh_dir {
                        host = host.with_ssh_dir(dir);
                    }
                    host
                };
                hosts.push(host);
            }
            let hosts = StaticHosts::new(hosts);
            if hosts.host("server").is_none() {
                return Err("--provider static needs --host server=SSH,ADDRESS".to_string());
            }
            Ok(Built::Static(hosts))
        }
        _ => {
            if !args.live {
                return Err(
                    "--provider aws with the real aws CLI reaches a real account: add --live"
                        .to_string(),
                );
            }
            let mut cli = Cli::live("aws", &args.region, setup.runner.clone());
            if let Some(round) = setup.recorded_as {
                cli = cli.with_recorder(recorder(round, &args.out, setup)?);
            }
            aws_provider(cli, args, setup).map(|p| Built::Rented(Box::new(p)))
        }
    }
}

/// Whether the Round's calls are recorded: only a paid Round's, which
/// reaches a real account (`--provider aws --live --yes`), and only where
/// `--record-calls` asks.
pub fn recorded(args: &RoundArgs) -> bool {
    args.record_calls && args.provider == "aws" && args.live && args.yes
}

/// Where a paid Round's calls are recorded: refused where they can't be,
/// before anything is created (§4).
pub fn recorder(round: &RoundId, out: &Path, setup: &Setup) -> Result<Recorder, String> {
    Recorder::create(
        &calls_directory(setup.xdg_state_home, setup.home),
        round.as_str(),
        &[out],
    )
    .map_err(|e| {
        format!(
            "a paid Round's calls to AWS are recorded, and these can't be: {e}; nothing was \
             created. Keep them on Linux: XDG_STATE_HOME (or HOME) on a Linux filesystem, \
             outside any repository and --out; or leave out --record-calls"
        )
    })
}

/// The AWS provider over `calls`: the pinned prices of `--region`, and
/// `--home-cidr` required.
pub fn aws_provider<C: RoundCalls>(
    calls: C,
    args: &RoundArgs,
    setup: &Setup,
) -> Result<AwsProvider<C>, String> {
    let Some(home_cidr) = &args.home_cidr else {
        return Err("--provider aws needs --home-cidr, this machine's public address/32".into());
    };
    let prices = setup
        .prices
        .get("aws")
        .and_then(|aws| aws.get(&args.region))
        .and_then(toml::Value::as_table)
        .ok_or_else(|| format!("the prices have no [aws.{}]", args.region))?;
    let prices: IndexMap<String, Num> = prices
        .iter()
        .filter_map(|(t, price)| match price {
            toml::Value::Float(f) => Some((t.clone(), Num::Float(*f))),
            toml::Value::Integer(i) => Some((t.clone(), Num::Int((*i).into()))),
            _ => None,
        })
        .collect();
    Ok(AwsProvider::new(
        calls,
        &args.region,
        &args.zone,
        setup.types.clone(),
        prices,
        home_cidr,
        setup.home_udp,
    ))
}

/// The prices besides the hosts', `prices["aws"]["common"]` for AWS, none
/// for hosts that exist.
fn cost_prices(prices: &toml::Table, provider: &str) -> Prices {
    let common = (provider == "aws")
        .then(|| prices.get("aws").and_then(|aws| aws.get("common")))
        .flatten();
    let price = |key: &str| {
        common
            .and_then(|c| c.get(key))
            .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
            .unwrap_or(0.0)
    };
    Prices {
        public_ipv4_per_hour: price("public_ipv4_per_hour"),
        disk_per_host_hour: price("disk_per_host_hour"),
        internet_egress_per_gb: price("internet_egress_per_gb"),
    }
}

/// `round.py`'s `run`: checks, plans and prices the Round, and runs it with
/// `--yes`: its exit status, or why it is refused.
fn run(args: &RoundArgs, env: &mut Env) -> Result<u8, Refused> {
    if args.provider == "aws" && args.yes && env.native_windows {
        return Err(Refused::from(
            "a paid Round runs from Linux or WSL, not from Windows itself: closing a Windows \
             console ends this process before its Round Teardown has ended the hosts it rented \
            ",
        ));
    }
    if env.factory.is_none() && args.provider == "aws" && !args.live {
        return Err(Refused::from(
            "--provider aws reaches a real account: add --live",
        ));
    }
    if args.record_calls && args.provider != "aws" {
        return Err(Refused::from(
            "--record-calls is for a paid Round, on --provider aws",
        ));
    }
    if args.yes
        && let Some(tool) = &args.measurement_tool
        && (!tool.is_file() || fs::File::open(tool).is_err())
    {
        return Err(Refused(format!(
            "measurement tool must be a readable executable file: {}",
            tool.display()
        )));
    }
    let prices_path = args.prices.clone().unwrap_or_else(default_prices);
    let prices: toml::Table = fs::read_to_string(&prices_path)
        .map_err(|e| e.to_string())
        .and_then(|text| toml::from_str(&text).map_err(|e| e.to_string()))
        .map_err(|e| Refused(format!("{}: {e}", prices_path.display())))?;
    let mut runs = load_matrix(&args.matrix)?;
    let needs_sampling = match &runs {
        Loaded::Scale(_) => true,
        Loaded::First(runs) => runs.iter().any(|run| run.host_samples),
    };
    if args.yes && needs_sampling && env.native_windows && args.measurement_tool.is_none() {
        return Err(Refused(
            "host sampling needs --measurement-tool built for Linux".into(),
        ));
    }
    if !args.home_client
        && let Loaded::First(runs) = &mut runs
    {
        runs.retain(|run| run.client_host != ClientHost::Home);
    }
    if runs.len() == 0 {
        return Err(Refused::from("the matrix has no Tests to run"));
    }
    let existing = args.out.join("scale");
    if runs.scale().is_some()
        && existing.is_dir()
        && fs::read_dir(&existing)
            .map_err(|e| e.to_string())?
            .next()
            .is_some()
    {
        return Err(Refused(format!(
            "{} already holds a Server-scale round: give an empty --out",
            existing.display()
        )));
    }
    if args.home_cap_mbps < 0 {
        return Err(Refused::from("--home-cap-mbps can't be below 0"));
    }
    // NaN passes every cap, an infinity makes no deadline, and no Round
    // runs in no time or for no money.
    for (option, value) in [
        ("--max-hours", args.max_hours),
        ("--round-cap", args.round_cap),
        ("--month-cap", args.month_cap),
    ] {
        if !(value.is_finite() && value > 0.0) {
            return Err(Refused(format!(
                "{option} must be a number above 0, not {value}"
            )));
        }
    }
    if args.ledger.is_none() && env.xdg_state_home.is_none() && env.home.as_os_str().is_empty() {
        return Err(Refused::from(
            "no home directory to keep the ledger in: set HOME or XDG_STATE_HOME, or give --ledger",
        ));
    }
    runs.check(args)?;
    let mut roles = runs.roles();
    let types = host_types(&roles, args, runs.scale());
    let runner = Runner::new(env.flag.clone());
    let now = env.clock.now();
    // A recorded Round's ID is made before its preflight, so that the
    // records of its calls begin with the first; one that can't be made is
    // refused, never run unrecorded. Otherwise the Round makes its ID as
    // it starts, as the Python does.
    let recorded_as = if recorded(args) {
        let suffix = secrets::random_hex(2)
            .map_err(|e| format!("the Round's ID, whose calls are to be recorded: {e}"))?;
        Some(RoundId::new(now, &suffix).ok_or_else(|| format!("the clock says {now}"))?)
    } else {
        None
    };
    let round_id = match &env.round_suffix {
        Some(suffix) => RoundId::new(now, suffix),
        None => recorded_as.clone(),
    };
    let home_udp = runs.home_udp();
    let setup = Setup {
        prices: &prices,
        types: &types,
        home_udp,
        runner: &runner,
        recorded_as: recorded_as.as_ref(),
        xdg_state_home: env.xdg_state_home.as_deref(),
        home: &env.home,
    };
    let mut built = match env.factory {
        Some(factory) => factory(args, &setup),
        None => build_provider(args, &setup, env.pid),
    }?;
    if args.check_permissions {
        let Built::Rented(rented) = &mut built else {
            return Err(Refused::from("--check-permissions is for --provider aws"));
        };
        let verdicts = rented.check_permissions().map_err(|e| e.to_string())?;
        let all = verdicts.values().all(|v| v == "allowed");
        let json = Json::Dict(
            verdicts
                .into_iter()
                .map(|(k, v)| (k, Json::Str(v)))
                .collect(),
        );
        (env.out)(&py::dumps_indent(&json, 1));
        return Ok(if all { 0 } else { 1 });
    }
    if let Some(matrix) = runs.scale() {
        scale_options(args, matrix)?;
    } else if args.as_found {
        return Err(Refused::from("--as-found is for a Server-scale Round"));
    }
    if let Built::Static(hosts) = &built {
        if runs.scale().is_some() {
            let missing: Vec<_> = roles
                .iter()
                .filter(|role| hosts.host(role).is_none())
                .map(|role| format!("{role}=SSH,ADDRESS"))
                .collect();
            if !missing.is_empty() {
                return Err(Refused(format!(
                    "the Server-scale round runs on three hosts: give --host {}",
                    missing.join(" ")
                )));
            }
        }
        roles.retain(|role| hosts.host(role).is_some());
        if runs.cloud_client() && hosts.host("client").is_none() {
            return Err(Refused::from(
                "the matrix has Tests between hosts: give --host client=SSH,ADDRESS",
            ));
        }
    }
    let provider = built.provider();
    let mut host_prices = Vec::new();
    for role in &roles {
        let price = provider
            .price_per_hour(&types[role])
            .map_err(|e| e.to_string())?;
        host_prices.push(Num::Float(price));
    }
    let cost = WorstCase::new(
        &runs.transfers(args.home_cap_mbps),
        &host_prices,
        args.max_hours,
        &cost_prices(&prices, provider.name()),
        if runs.scale().is_some() {
            scale::matrix::RERUN_SHARE
        } else {
            0.0
        },
    );
    let ledger_file = ledger::path(
        args.ledger.as_deref(),
        env.xdg_state_home.as_deref(),
        &env.home,
    );
    let book = ledger::load(&ledger_file).map_err(|e| e.to_string())?;
    let spent = ledger::month_spent(&book, provider.name(), now)
        .map_err(|e| format!("the ledger {}: {e}", ledger_file.display()))?;
    let rented = provider.rented();
    let role_values: IndexMap<String, Json> = roles
        .iter()
        .map(|role| {
            let said = if rented {
                types[role].clone()
            } else {
                match &built {
                    Built::Static(hosts) => hosts
                        .host(role)
                        .map(|h| h.destination().to_string())
                        .unwrap_or_default(),
                    Built::Rented(_) => String::new(),
                }
            };
            (role.clone(), Json::Str(said))
        })
        .collect();
    let provider = built.provider();
    let plan: IndexMap<String, Json> = IndexMap::from([
        ("provider".to_string(), Json::Str(args.provider.clone())),
        (
            "region".to_string(),
            if rented {
                Json::Str(args.region.clone())
            } else {
                Json::Null
            },
        ),
        (
            "kind".to_string(),
            Json::Str(
                if runs.scale().is_some() {
                    "server-scale"
                } else {
                    "tests"
                }
                .into(),
            ),
        ),
        ("roles".to_string(), Json::Dict(role_values)),
        ("tests".to_string(), Json::Int(runs.len() as i128)),
        ("matrix_hours".to_string(), Json::Float(runs.hours())),
        ("max_hours".to_string(), Json::Float(args.max_hours)),
        ("worst_case".to_string(), cost.to_json()),
        ("round_cap".to_string(), Json::Float(args.round_cap)),
        (
            "month_spent".to_string(),
            match spent.round(Some(2)) {
                Some(Num::Int(i)) => Json::Int(i),
                Some(Num::Float(f)) => Json::Float(f),
                None => Json::Float(spent.to_f64()),
            },
        ),
        ("month_cap".to_string(), Json::Float(args.month_cap)),
    ]);
    (env.out)(&py::dumps_indent(&Json::Dict(plan), 1));
    check_caps(cost.total, args.round_cap, spent, args.month_cap)?;
    // Preflight against the provider: credentials, image, leftovers. It
    // creates nothing, but ends what earlier Rounds left past their
    // expiry, with or without --yes.
    let refused = |e: ProviderError| match e {
        ProviderError::Failed(why) => Refused(why),
        ProviderError::Interrupted => Refused::from("interrupted"),
    };
    provider.check(args.max_hours, now).map_err(refused)?;
    let reaped = provider.reap(now).map_err(refused)?;
    if !reaped.is_empty() {
        let ids: Vec<String> = reaped.iter().map(ToString::to_string).collect();
        (env.out)(&format!("reaped expired Rounds: {}", ids.join(", ")));
    }
    if !args.yes {
        let also = if reaped.is_empty() {
            ""
        } else {
            ", but expired Rounds were reaped"
        };
        (env.out)(&format!(
            "plan only: nothing was created{also}. Add --yes to run the Round."
        ));
        if args.record_calls {
            (env.out)("--record-calls records a Round run with --yes: this plan recorded nothing.");
        }
        return Ok(0);
    }
    // The preflight was the first call recorded: a Round asked to record
    // whose recording has already ended is refused before anything is
    // created, as one whose calls can't be recorded at all is.
    if args.record_calls
        && let Built::Rented(rented) = &built
        && let Some(why) = rented.unrecorded()
    {
        return Err(Refused(format!(
            "recording the Round's calls to AWS ended in its plan: {why}; nothing was created"
        )));
    }
    // A signal during the plan ends it here, before anything is created,
    // as it ends the Python there.
    if env.flag.raised() {
        return Err(Refused::from("interrupted"));
    }
    fs::create_dir_all(args.out.join("tests"))
        .map_err(|e| format!("{}: {e}", args.out.display()))?;
    let options = Options {
        measurement_tool: args.measurement_tool.clone(),
        udpstp: args.udpstp.clone(),
        udpstp_home: args.udpstp_home.clone(),
        obudpst: args.obudpst.as_deref().map(|o| {
            if o == "build" {
                Obudpst::Build
            } else {
                Obudpst::Binary(o.into())
            }
        }),
        raise_buffers: args.raise_buffers,
        udpstp_port: args.udpstp_port,
        obudpst_port: args.obudpst_port,
        home_cap_mbps: args.home_cap_mbps,
        out: args.out.clone(),
        ssh_dir: args.ssh_dir.clone(),
        obudpst_script: obudpst::script(),
    };
    let mut body: Box<dyn RoundBody + '_> = match runs {
        Loaded::First(runs) => {
            let mut body = FirstRound::new(options, runs, roles, env.clock, runner);
            if let Some(make) = env.ssh_key {
                body = body.with_ssh_key(make);
            }
            Box::new(body)
        }
        Loaded::Scale(matrix) => {
            let options = scale::round::Options {
                measurement_tool: args.measurement_tool.clone(),
                udpstp: options.udpstp,
                prototype: args.prototype.clone(),
                obudpst: options.obudpst,
                raise_buffers: args.raise_buffers,
                hold_irqs: args.hold_irqs,
                as_found: args.as_found,
                udpstp_port: args.udpstp_port,
                obudpst_port: args.obudpst_port,
                out: args.out.clone(),
                ssh_dir: args.ssh_dir.clone(),
                matrix: args.matrix.clone(),
                scripts: Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts"),
                obudpst_script: options.obudpst_script,
            };
            let mut body = scale::round::ScaleRound::new(options, *matrix, env.clock, runner)
                .map_err(|e| e.to_string())?;
            if let Some(make) = env.ssh_key {
                body = body.with_ssh_key(make);
            }
            Box::new(body)
        }
    };
    let log = Log::new(&args.out);
    let status = round::execute(
        body.as_mut(),
        built.provider(),
        Bracket {
            ledger: &ledger_file,
            worst_case: cost.total,
            max_hours: args.max_hours,
            round: round_id,
            flag: &env.flag,
            clock: env.clock,
            log: &log,
            keys_in: env.keys_in.as_deref(),
            alert: &mut *env.err,
        },
    );
    if let Built::Rented(rented) = &built
        && let Some(why) = rented.unrecorded()
    {
        (env.err)(&format!(
            "\x1b[31mrecording the Round's calls to AWS ended early: {why}; the replay covers \
             only the calls before it\x1b[0m"
        ));
    }
    Ok(status.code())
}
