#![forbid(unsafe_code)]
//! The `udpstp` binary: the UDPSTP (RFC 9946) Client and Server.

mod color;
mod config;
mod exit;
#[cfg(test)]
mod interfaces;
mod interrupt;
mod json;
mod keyfile;
mod log;
mod names;
mod privacy;
mod results;
// Its Windows glue is Windows-only; its states are tested everywhere.
#[cfg_attr(not(windows), allow(dead_code))]
mod service;
mod start;
mod statistics;
mod terminal;
mod test_connection_end;
mod test_results;
mod warnings;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::error::ErrorKind;
use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use udpstp_core::{ActivationParameter, DataEvent, Event, TransportError};
use udpstp_runtime::{DEFAULT_MAX_HOPS, DEFAULT_SOCKET_BUFFER_SIZE, SOCKET_BUFFER_LIMITS};

use crate::config::{ServerOverrides, ServerSettings};
use crate::interrupt::{errln, out, outln};
use crate::start::client::ClientStart;
use crate::start::server::ServerStart;
use crate::start::{Host, StartError};
use crate::test_results::{Stream, coercion_text};

/// Measure IP-Layer Capacity with UDPSTP (RFC 9946): run a Test as the
/// Client, or serve Tests as the Server.
#[derive(Debug, Parser)]
#[command(
    name = "udpstp",
    version,
    after_help = "\
Examples:
  udpstp server --key-file keys.toml --max-hops 12
  udpstp client --key-file keys.toml --max-hops 12 server.example.net

Documentation: https://udpstp.com/docs/getting-started.html"
)]
struct Cli {
    /// Terminal styling: auto for capable terminals, always for redirected
    /// human output too, or never for plain text.
    #[arg(long, global = true, value_enum, default_value_t = color::Mode::Auto)]
    color: color::Mode,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run a Test against one or more Servers.
    #[command(after_help = "\
Examples:
  udpstp client --key-file keys.toml server.example.net
  udpstp client --key-file keys.toml --direction upstream server.example.net

Write boolean values with '='. For example, --jumbo=false.

Documentation: https://udpstp.com/docs/guides/running-a-test.html")]
    Client(Box<ClientArgs>),
    /// Serve Tests: answer Clients' Setup and Activation Requests and run
    /// their Test Connections.
    // Boxed: its settings make it far larger than the Client's arguments.
    #[command(after_help = "\
Examples:
  udpstp server --key-file keys.toml --max-hops 12
  udpstp server --config /etc/udpstp/server.toml

Write boolean overrides with '='. For example, --jumbo=false.

Documentation: https://udpstp.com/docs/guides/hosting-a-server.html")]
    Server(Box<ServerArgs>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum TestDirection {
    Downstream,
    Upstream,
}

/// A Load Rate Adjustment Algorithm.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum Algorithm {
    B,
    C,
}

/// How the Client prints its results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Format {
    /// Human-readable text.
    Text,
    /// JSON compatible with OB-UDPST's client schema; progress goes to
    /// stderr.
    Json,
}

/// The delay variation the Load Rate Adjustment Algorithm judges by.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum DelayVar {
    /// One-way delay variation (useOwDelVar 1): no clock synchronisation
    /// needed, but it follows clock drift.
    OneWay,
    /// Round-trip time variation (useOwDelVar 0): immune to clock drift.
    Rtt,
}

/// A Loss Ratio from 0 to 1, in parts per million.
fn loss_ratio(value: &str) -> Result<u32, String> {
    value
        .parse::<f64>()
        .ok()
        .filter(|ratio| (0.0..=1.0).contains(ratio))
        .map(|ratio| (ratio * 1e6).round() as u32)
        .ok_or_else(|| "must be a ratio from 0 to 1, such as 0.01".into())
}

/// A socket buffer size within [`config::SOCKET_BUFFER_SIZES`].
fn socket_buffer_size(value: &str) -> Result<usize, String> {
    value
        .parse()
        .ok()
        .filter(|size| config::SOCKET_BUFFER_SIZES.contains(size))
        .ok_or_else(|| config::SOCKET_BUFFER_SIZE_RANGE.into())
}

#[derive(Debug, Args)]
struct ClientArgs {
    /// The Server's host name or address, optionally with a port (the
    /// Standard Port 24601 by default). Several Servers share a Test's Test
    /// Connections in turn; they must all be IPv4 or all IPv6.
    #[arg(required = true, value_name = "SERVER")]
    servers: Vec<String>,
    /// The key file: a TOML Key Table if its name ends in .toml, else an
    /// OB-UDPST udpst.keys file. Group and others must not be able to read
    /// it. Keys are never given on the command line.
    #[arg(long, value_name = "FILE")]
    key_file: Option<PathBuf>,
    /// The direction of the Test: the Server sends the load (downstream)
    /// or the Client does (upstream).
    #[arg(long, value_enum, default_value_t = TestDirection::Downstream)]
    direction: TestDirection,
    /// The Test Interval to ask the Server for, in seconds (5-60; longer
    /// only with an Operator Key that allows it).
    #[arg(long, value_name = "SECONDS", default_value_t = 10, value_parser = clap::value_parser!(u16).range(5..))]
    test_interval: u16,
    /// MaxHops: the IPv4 TTL or IPv6 hop limit of test traffic, from the
    /// expected path length.
    #[arg(long, value_name = "HOPS", default_value_t = DEFAULT_MAX_HOPS, value_parser = clap::value_parser!(u8).range(1..))]
    max_hops: u8,
    /// How to print the results.
    #[arg(long, value_enum, default_value_t = Format::Text)]
    format: Format,
    /// The most the Test is expected to send or receive, in Mbps
    /// (maxBandwidth), shared equally between its Test Connections. The
    /// Server searches no higher, and a Server with an Admitted Rate Cap
    /// may refuse the Test.
    #[arg(long, value_name = "MBPS", value_parser = clap::value_parser!(u16).range(1..=0x7FFF))]
    max_bandwidth: Option<u16>,
    /// How many Test Connections the Test runs, 1 to 255 [default: one per
    /// SERVER]. OB-UDPST Servers accept at most 24.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u8).range(1..))]
    test_connections: Option<u8>,
    /// The Minimum Test Connections: how many must succeed for the Test to
    /// go on [default: all of them]. With all required, one lost Setup
    /// Response aborts the Test.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u8).range(1..))]
    min_test_connections: Option<u8>,
    /// Run the Test unauthenticated. Not conformant to RFC 9946; the Server
    /// must allow it too.
    #[arg(long, conflicts_with = "key_file")]
    unauthenticated: bool,
    /// Send with this keyId's key, for a key file with several keys valid
    /// now.
    #[arg(long, value_name = "KEYID", requires = "key_file")]
    key_id: Option<u8>,
    /// The Authentication Mode with a key file: 2 authenticates the
    /// Control Phase and every Status PDU (RFC 9946 §5.3), 1 the Control
    /// Phase only, for a Server that doesn't support mode 2. The Client
    /// never falls back to mode 1 by itself.
    #[arg(long, value_name = "MODE", default_value_t = 2, value_parser = clap::value_parser!(u8).range(1..=2), conflicts_with = "unauthenticated")]
    auth_mode: u8,
    /// Use mode 1 with this SERVER, written as among the
    /// SERVERs, for one that doesn't support mode 2. Repeatable.
    #[arg(long, value_name = "SERVER", requires = "key_file")]
    mode_1_server: Vec<String>,
    /// Authorize an alternate IP address for this SERVER's authenticated
    /// Setup Responses. SERVER must match a positional SERVER; IP must be
    /// a literal address of the same family. Repeatable. Without this,
    /// only the selected address is accepted.
    #[arg(
        long,
        value_name = "SERVER=IP",
        requires = "key_file",
        conflicts_with = "unauthenticated"
    )]
    allow_server_address: Vec<String>,
    /// Give up sooner than the Test initiation timer's 3 s if the Control
    /// Phase hasn't finished after this many seconds.
    #[arg(long, value_name = "SECONDS", default_value_t = 3, value_parser = clap::value_parser!(u64).range(1..=3))]
    time_limit: u64,
    /// Run a Fixed-Rate Test at this Sending Rate Table row. The Server
    /// allows it only for an Operator Key.
    #[arg(long, value_name = "ROW", value_parser = clap::value_parser!(u16).range(0..=1152))]
    fixed_rate: Option<u16>,
    /// Start the Server's search at this Sending Rate Table row instead of
    /// the first (row N offers N Mbit/s up to 1000).
    #[arg(long, value_name = "ROW", conflicts_with = "fixed_rate", value_parser = clap::value_parser!(u16).range(0..=1152))]
    start_row: Option<u16>,
    /// Ask for random payload content instead of zeros, so compression on
    /// the path can't inflate the result.
    #[arg(long)]
    random_payload: bool,
    /// The delay variation the Load Rate Adjustment Algorithm judges by.
    #[arg(long, value_enum, default_value_t = DelayVar::OneWay)]
    delay_var: DelayVar,
    /// End the Test if the Server coerces this Activation parameter,
    /// named as in RFC 9946 (e.g. testIntTime). Repeatable.
    #[arg(long, value_name = "PARAMETER", value_parser = names::activation_parameter)]
    refuse_coercion: Vec<ActivationParameter>,
    /// Use the header checksum (RFC 9946 §5.6): fill it in every PDU sent,
    /// verify it in every PDU received when populated.
    #[arg(long, value_name = "BOOL", num_args = 0..=1, require_equals = true, default_missing_value = "true", default_value_t = false)]
    checksum: bool,
    /// Ask for Jumbo rows above 1 Gbps (CHSR_JUMBO_STATUS); the Server
    /// must serve the same.
    #[arg(long, value_name = "BOOL", default_value_t = true, num_args = 0..=1, require_equals = true, default_missing_value = "true", action = clap::ArgAction::Set)]
    jumbo: bool,
    /// Ask for 1500-byte packets (CHSR_TRADITIONAL_MTU); the Server must
    /// serve the same.
    #[arg(long, value_name = "BOOL", default_value_t = false, num_args = 0..=1, require_equals = true, default_missing_value = "true", action = clap::ArgAction::Set)]
    traditional_mtu: bool,
    /// The Load Rate Adjustment Algorithm the Server searches with.
    #[arg(long, value_enum, default_value_t = Algorithm::B)]
    algorithm: Algorithm,
    /// The DSCP to mark the Test's traffic with, 0 to 63, so a specific
    /// traffic class is measured. The Server may coerce it to 0.
    #[arg(long, value_name = "DSCP", default_value_t = 0, value_parser = clap::value_parser!(u8).range(0..=63))]
    dscp: u8,
    /// How text and JSON results show this Client's own address: in full,
    /// pseudonymized with a keyed hash, or truncated to the IPv4 /24 or
    /// IPv6 /48.
    #[arg(long, value_enum, default_value_t = privacy::AddressPrivacy::Full)]
    address_privacy: privacy::AddressPrivacy,
    /// The pseudonym key file, hexadecimal digits (16 bytes or more), for
    /// --address-privacy pseudonymize.
    #[arg(long, value_name = "FILE")]
    address_privacy_key_file: Option<std::path::PathBuf>,
    /// With --format json, add udpstp's own results (the Performance
    /// Criterion, the Maximum and Highest IP-Layer Capacity, warnings,
    /// how the Test ran) as a last
    /// "udpstp" object; false leaves OB-UDPST's keys only.
    #[arg(long, value_name = "BOOL", default_value_t = true, num_args = 0..=1, require_equals = true, default_missing_value = "true", action = clap::ArgAction::Set)]
    json_extension: bool,
    /// The Performance Criterion: the most a Sub-Interval may lose, as a
    /// share of the Load PDUs sent, to count towards the Maximum IP-Layer
    /// Capacity (RFC 9097 §6.3). One lost Load PDU always passes; a
    /// Sub-Interval in which none arrived never does.
    #[arg(long, value_name = "RATIO", default_value = "0.01", value_parser = loss_ratio)]
    max_loss_ratio: u32,
    /// Warn when, in a Search Test, a Sub-Interval after the Maximum
    /// IP-Layer Capacity's ran more than this many percent faster and none
    /// after it met the Performance Criterion: the Maximum may then be well
    /// below the path, as a search through a drop-tail queue gives.
    #[arg(long, value_name = "PERCENT", default_value_t = udpstp_core::FASTER_SUB_INTERVAL_MARGIN_PERCENT)]
    faster_sub_interval_margin: u32,
    /// Put the first N Sub-Intervals in a Capacity Mode of their own, with
    /// its own Maximum IP-Layer Capacity, for a path whose capacity changes
    /// on a schedule (RFC 9097 §6.6).
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    first_mode_sub_intervals: Option<u32>,
    /// The Data Phase's receive and send buffer, in bytes; the OS's limits
    /// may hold it lower.
    #[arg(long, value_name = "BYTES", default_value_t = DEFAULT_SOCKET_BUFFER_SIZE, value_parser = socket_buffer_size)]
    socket_buffer_size: usize,
    /// Set the Don't Fragment bit on test traffic; false lets the path
    /// fragment it.
    #[arg(long, value_name = "BOOL", default_value_t = true, num_args = 0..=1, require_equals = true, default_missing_value = "true", action = clap::ArgAction::Set)]
    dont_fragment: bool,
}

#[derive(Debug, Args)]
struct ServerArgs {
    /// The Server's TOML configuration file. Command-line options override
    /// its settings.
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,
    #[command(flatten)]
    overrides: ServerOverrides,
    /// Validate the settings and key file, print effective TOML, and exit.
    #[arg(long)]
    print_config: bool,
    /// With --print-config, preview settings without opening keys or logs.
    #[arg(long, requires = "print_config", conflicts_with = "windows_service")]
    settings_only: bool,
    /// Run as the Windows service the Service Control Manager started,
    /// which stops it, logging to log_file (Windows only).
    #[arg(long)]
    windows_service: bool,
}

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().collect();
    let mode = color::Mode::from_args(&args);
    let cli = match parse_at(
        &args,
        terminal::stdout_width(),
        terminal::stderr_width(),
        mode,
    ) {
        Ok(cli) => cli,
        Err(error) => {
            let stderr = error.use_stderr();
            let width = if stderr {
                terminal::stderr_width()
            } else {
                terminal::stdout_width()
            };
            let palette = if stderr { mode.stderr() } else { mode.stdout() };
            let rendered = error.render();
            let text = if palette.is_colored() {
                rendered.ansi().to_string()
            } else {
                rendered.to_string()
            };
            let text = terminal::wrap(&text, width);
            if stderr {
                errln!("{}", text.trim_end_matches('\n'));
            } else {
                out!("{text}");
            }
            return match error.kind() {
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => ExitCode::SUCCESS,
                _ => ExitCode::from(exit::CONFIGURATION_ERROR),
            };
        }
    };
    match cli.command {
        Command::Client(args) => client(*args, cli.color),
        Command::Server(args) => server(*args, cli.color),
    }
}

/// Clap writes help to stdout and parser diagnostics to stderr. Parse
/// again when the diagnostic's destination has different presentation.
fn parse_at(
    args: &[std::ffi::OsString],
    stdout: usize,
    stderr: usize,
    mode: color::Mode,
) -> Result<Cli, clap::Error> {
    let stdout_palette = mode.stdout();
    let stderr_palette = mode.stderr();
    let parse = |width, palette: color::Palette| {
        let matches = Cli::command()
            .styles(palette.clap_styles())
            .color(if palette.is_colored() {
                clap::ColorChoice::Always
            } else {
                clap::ColorChoice::Never
            })
            .term_width(width)
            .try_get_matches_from(args)?;
        Cli::from_arg_matches(&matches)
    };
    match parse(stdout, stdout_palette) {
        Err(error)
            if error.use_stderr() && (stdout != stderr || stdout_palette != stderr_palette) =>
        {
            parse(stderr, stderr_palette)
        }
        result => result,
    }
}

/// The Control Phase runs on one Tokio thread (ADR 0003).
fn tokio_runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
}

fn client(args: ClientArgs, mode: color::Mode) -> ExitCode {
    let stdout = mode.stdout();
    let stderr = mode.stderr();
    // Before any other thread starts, which leaves the signals to it.
    let mut interrupts = interrupt::listen();
    // With JSON results, stdout holds only the JSON.
    let json = args.format == Format::Json;
    let progress = if json { stderr } else { stdout };
    macro_rules! say {
        ($($t:tt)*) => {
            if json {
                errln!($($t)*)
            } else {
                outln!($($t)*)
            }
        };
    }
    // The Control Phase's runtime, built for the first name to resolve.
    let mut runtime = None;
    let start = ClientStart::new(&args, Host::THIS, |name| {
        let runtime = match &mut runtime {
            Some(runtime) => runtime,
            none => none.insert(tokio_runtime()?),
        };
        runtime.block_on(udpstp_runtime::resolve(name))
    });
    let start = match start {
        Ok(start) => start,
        Err(error) => {
            errln!("{}", stderr.diagnostic(color::Role::Error, &error.message));
            return ExitCode::from(error.status);
        }
    };
    // Every SERVER resolved with it.
    let Some(runtime) = runtime else {
        return ExitCode::from(exit::INITIALIZATION_ERROR);
    };
    for warning in &start.warnings {
        errln!(
            "{}",
            stderr.diagnostic(color::Role::Warning, &warning.message())
        );
    }
    let server = start.plan.servers[0];
    let test_connections = start.plan.test_connections.get();
    let multi = test_connections > 1;
    if multi {
        say!(
            "{}{} {test_connections} {}",
            progress.marker(color::Role::Info),
            progress.paint(color::Role::Info, "Sending"),
            progress.paint(color::Role::Info, "Setup Requests")
        );
    } else {
        say!(
            "{}{} {server}",
            progress.marker(color::Role::Info),
            progress.paint(color::Role::Muted, "Sending the Setup Request to")
        );
    }
    // Which Test Connection a line is about, when there are several.
    let which = |index: Option<u8>| match index {
        Some(index) if multi => format!("Test Connection {index}: "),
        _ => String::new(),
    };
    // When the Test was activated (T0), for the results.
    let mut started = None;
    // What interrupted the Test, if anything did.
    // (conformance: client-interrupt)
    let interruption = std::cell::Cell::new(None);
    let test = runtime.block_on(udpstp_runtime::run_test(
        start.plan.clone(),
        start.config.clone(),
        start.ip,
        start.socket_buffer_size,
        // The experimental settings for the Data Phase Engines, read once.
        crate::start::engine_settings(|name| std::env::var(name).ok()),
        |index, event| match event {
            Event::SetupAccepted { test_port } => {
                say!(
                    "{}{}{} {}",
                    which(index),
                    progress.marker(color::Role::Success),
                    progress.paint(color::Role::Info, "Setup accepted"),
                    progress.paint(color::Role::Muted, &format!("(Ephemeral Port {test_port})"))
                );
            }
            Event::TestActivated { direction, .. } => {
                started = started.or_else(|| {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .ok()
                });
                say!(
                    "{}{}{} {}",
                    which(index),
                    progress.marker(color::Role::Info),
                    progress.paint(color::Role::Info, "Test activated"),
                    progress.paint(color::Role::Muted, &format!("({})", names::direction(*direction)))
                );
            }
            // Coerced, never silently replaced (docs/spec.md).
            Event::ParameterCoerced { coercion } => {
                let message = coercion_text(coercion);
                let label = "The Server set";
                let message = message.strip_prefix(label).map_or_else(
                    || message.clone(),
                    |rest| format!("{}{rest}", progress.paint(color::Role::Warning, label)),
                );
                say!("{}{message}", which(index));
            }
            Event::TestConnectionAbandoned if multi => {
                say!(
                    "{}{}",
                    which(index),
                    progress.paint(color::Role::Error, "abandoned")
                );
            }
            // ADR 0006, : reported once per kind as it happens.
            Event::PathError(TransportError::TimeExceeded) => {
                errln!(
                    "{} {}ICMP time exceeded: --max-hops is too small for the path",
                    stderr.paint(color::Role::Warning, "udpstp: warning:"),
                    which(index)
                );
            }
            Event::PathError(TransportError::MsgSize) => {
                errln!(
                    "{} {}a datagram was too large for the path with DF set (path MTU); drop --traditional-mtu, or ask the Server's administrator to lower its server_mtu",
                    stderr.paint(color::Role::Warning, "udpstp: warning:"),
                    which(index)
                );
            }
            _ => {}
        },
        |index, event| match event {
            DataEvent::StopReceived => {
                let ended = if multi { "Test Connection" } else { "Test" };
                say!(
                    "{}{}",
                    which(Some(index)),
                    progress.paint(color::Role::Info, &format!("The Server ended the {ended}"))
                )
            }
            // Watchdog and non-graceful stops are shown (docs/spec.md).
            DataEvent::TrafficStopped { .. } => errln!(
                "{} {}nothing received from the Server for 1 s; stopped sending",
                stderr.paint(color::Role::Warning, "udpstp: warning:"),
                which(Some(index))
            ),
            _ => {}
        },
        async {
            interruption.set(Some(interrupts.next().await));
        },
    ));
    interrupts.disarm();
    let interrupted = interruption.get();
    // The process ends as the interruption asks; or, if a write to stdout
    // failed, the final results' too, as interrupted; or with the Test's
    // status.
    let exit = |status: exit::Status| match interrupted {
        Some(interruption) => interruption.exit(status.exit_code()),
        None if interrupt::stdout_closed() => ExitCode::from(exit::INTERRUPTED),
        None => ExitCode::from(status.exit_code()),
    };
    let test = match test {
        Ok(test) => test,
        Err(error) => {
            errln!(
                "{} {server}: {error}",
                stderr.paint(color::Role::Error, "udpstp:")
            );
            return ExitCode::from(exit::INITIALIZATION_ERROR);
        }
    };
    let printed =
        test_results::print_with_color(&test, &start.results, started, wall_now(), stdout, stderr);
    for (stream, text) in &printed.output {
        match stream {
            Stream::Stdout => out!("{text}"),
            Stream::Stderr => errln!("{}", text.strip_suffix('\n').unwrap_or(text)),
        }
    }
    exit(printed.status)
}

fn server(args: ServerArgs, mode: color::Mode) -> ExitCode {
    if args.windows_service {
        #[cfg(windows)]
        return service::run(args, mode);
        #[cfg(not(windows))]
        {
            errln!(
                "{}",
                mode.stderr().diagnostic(
                    color::Role::Error,
                    "udpstp: configuration error: --windows-service runs only on Windows"
                )
            );
            return ExitCode::from(exit::CONFIGURATION_ERROR);
        }
    }
    ExitCode::from(run_server(&args, mode, None, || {}))
}

/// What stops a Server run as a Windows service: its control handler's
/// stop or shutdown, by the name its log's `Stopping` gives it.
type ServiceStop = std::pin::Pin<Box<dyn std::future::Future<Output = &'static str>>>;

/// Runs the Server until it stops, returning its exit status. `stop` is a
/// Windows service's stop, in place of the console's signals and events,
/// which a service gets too and mustn't act on; `listening` is
/// called once it listens. With a log file, the startup's errors and
/// warnings go into it as well as to stderr, which a service doesn't have.
fn run_server(
    args: &ServerArgs,
    mode: color::Mode,
    stop: Option<ServiceStop>,
    mut listening: impl FnMut(),
) -> u8 {
    let stdout = mode.stdout();
    let stderr = mode.stderr();
    // A log file named on the command line takes a configuration error too.
    let fail = |message: &str, status: u8, logged: bool| {
        errln!("{}", stderr.diagnostic(color::Role::Error, message));
        if logged {
            log::startup_error(message, status);
        }
        status
    };
    let settings = match ServerSettings::load(args.config.as_deref(), &args.overrides) {
        Ok(settings) => settings,
        Err(error) => {
            let message = format!("udpstp: configuration error: {error}");
            let logged = (!args.settings_only)
                .then(|| {
                    args.overrides
                        .log_file
                        .as_deref()
                        .and_then(log::LogFile::new)
                })
                .flatten();
            let logged = logged.is_some_and(|file| {
                log::init(
                    args.overrides.log_format.unwrap_or_default(),
                    Some(&file),
                    stdout,
                )
                .is_ok()
            });
            return fail(&message, exit::CONFIGURATION_ERROR, logged);
        }
    };
    if args.settings_only {
        out!("{}", settings.to_toml());
        return exit::SUCCESS;
    }
    if stop.is_some() && settings.log_file.is_none() {
        let message = "udpstp: configuration error: a Windows service needs log_file: it has no stdout or stderr";
        return fail(message, exit::CONFIGURATION_ERROR, false);
    }
    // The log file opens before the key file is read, so its errors reach
    // it; never for --print-config, which writes no log.
    let to_file = !args.print_config
        && match settings.log_file.as_deref().and_then(log::LogFile::new) {
            Some(file) => match log::init(settings.log_format, Some(&file), stdout) {
                Ok(()) => true,
                Err(error) => {
                    let message = format!("udpstp: {error}");
                    return fail(&message, exit::INITIALIZATION_ERROR, false);
                }
            },
            None => false,
        };
    let keys = match &settings.key_file {
        Some(path) => match keyfile::load(path) {
            Ok(loaded) => {
                if loaded.format_deprecated {
                    let warning = crate::start::StartupWarning::KeyTableFormatDeprecated {
                        path: path.clone(),
                    };
                    let message = warning.message();
                    errln!("{}", stderr.diagnostic(color::Role::Warning, &message));
                    if to_file {
                        log::startup_warning(warning.name(), &message);
                    }
                }
                loaded.table
            }
            Err(error) => {
                let message = format!("udpstp: key file error: {error}");
                return fail(&message, exit::KEY_FILE_ERROR, to_file);
            }
        },
        None => Default::default(),
    };
    if args.print_config {
        out!("{}", settings.to_toml());
        return exit::SUCCESS;
    }
    // Probing the socket buffers is a question for the OS.
    let socket_buffers = udpstp_runtime::probe_socket_buffers(settings.socket_buffer_size);
    // The experimental settings for the Data Phase Engines, read once.
    let engine = crate::start::engine_settings(|name| std::env::var(name).ok());
    let start = match ServerStart::new(&settings, keys, socket_buffers, Host::THIS, engine) {
        Ok(start) => start,
        Err(error) => return fail(&error.message, error.status, to_file),
    };
    for warning in &start.warnings {
        let message = warning.message();
        errln!("{}", stderr.diagnostic(color::Role::Warning, &message));
        if to_file {
            log::startup_warning(warning.name(), &message);
        }
    }
    let runtime = match tokio_runtime() {
        Ok(runtime) => runtime,
        Err(error) => {
            let error = StartError::runtime(&error);
            return fail(&error.message, error.status, to_file);
        }
    };
    // The signal handlers go in before the Server logs anything, so one
    // that comes as soon as it says it listens stops it cleanly. A
    // service installs none: the Service Control Manager stops it.
    let stop = stop.unwrap_or_else(|| {
        let _entered = runtime.enter();
        Box::pin(interrupt::ServerStop::install().stopped())
    });
    // Startup warnings went to stderr; from here on the log, on stdout.
    if !to_file {
        let _ = log::init(settings.log_format, None, stdout);
    }
    let mut server_log = log::ServerLog::new(
        SOCKET_BUFFER_LIMITS,
        std::time::Duration::from_secs(settings.statistics_interval.into()),
        start.addresses,
    );
    let served = runtime.block_on(udpstp_runtime::serve(
        start.options,
        |notice| {
            if matches!(notice, udpstp_runtime::ServerNotice::Listening(_)) {
                listening();
            }
            server_log.notice(&notice);
        },
        stop,
    ));
    server_log.stopped();
    match served {
        Ok(()) => exit::SUCCESS,
        Err(error) => fail(
            &format!("udpstp: {error}"),
            exit::INITIALIZATION_ERROR,
            to_file,
        ),
    }
}

/// The wall clock since the Unix epoch.
fn wall_now() -> Duration {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
}
