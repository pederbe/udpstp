#![forbid(unsafe_code)]
//! The Server's log (docs/spec.md "Observability", stories 83 and 90; ):
//! the core's typed events and the runtime's notices as `tracing` events,
//! human-readable or one JSON object a line, on stdout. Each has an
//! `event` field naming what happened, from the fixed set in
//! [`names::event`], and typed fields for its data, each with one meaning:
//! `test_connection_id`, `client`, `reason` (a name from [`names`]),
//! `count` (only ever a count), `error` (an OS error's text, not stable),
//! and the few an event has of its own. Nothing is logged per PDU: the
//! Data Phase reports counts.
//!
//! An attacker can make most events happen, so every kind is limited but
//! the few in [`UNLIMITED`]: at most [`BURST`] of a kind, and [`TOTAL`] of
//! all kinds together, in each [`WINDOW`]. What was held back is logged as
//! a `Suppressed` count per kind when the window ends.
//!
//! The log also reports the Server's counters every `statistics_interval`
//! (`Statistics`, then a `StatisticsRefusals` per reason) and each Test
//! once it ends (`TestSummary`), from [`Statistics`], which counts every
//! notice, logged or not. Only the summaries of Tests never
//! activated, which an attacker can cause, are limited.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use crate::protocol::{ActivationRejection, DataEvent, EndReason, Event, SocketId};
use crate::runtime::ServerNotice;
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::names::{self, event};
use crate::statistics::{self, Statistics, TestSummary};

/// The most notices of one kind logged in a [`WINDOW`].
const BURST: u32 = 10;

/// The most notices of all kinds together logged in a [`WINDOW`].
const TOTAL: u32 = 100;

/// The window over which notices are limited.
const WINDOW: Duration = Duration::from_secs(10);

/// Events no one outside can cause, never limited.
const UNLIMITED: [&str; 5] = [
    event::NO_IPV6_LISTENER,
    event::TROUBLESHOOTING_MODE_ON,
    event::SUPPRESSED,
    event::STOPPING,
    event::DATA_PHASE_ENGINES,
];

/// The log's format.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    /// Readable events, with indented continuation lines when needed.
    #[default]
    Human,
    /// One JSON object a line.
    Json,
}

/// How many of the log file's daily files are kept.
const LOG_FILES_KEPT: usize = 14;

/// The `log_file` setting taken apart: the directory its daily files go
/// in, and the name around the date, `server.log` giving
/// `server.2026-10-01.log`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogFile {
    directory: std::path::PathBuf,
    stem: String,
    extension: Option<String>,
}

impl LogFile {
    /// `None` where the path names no file, or its name isn't UTF-8.
    pub fn new(path: &std::path::Path) -> Option<Self> {
        let stem = path.file_stem()?.to_str()?.to_owned();
        let extension = match path.extension() {
            Some(extension) => Some(extension.to_str()?.to_owned()),
            None => None,
        };
        let directory = match path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
            _ => std::path::PathBuf::from("."),
        };
        Some(Self {
            directory,
            stem,
            extension,
        })
    }

    /// The appender writing a new file each day, keeping the last
    /// [`LOG_FILES_KEPT`]; an error where the directory can't be made or
    /// written.
    fn appender(
        &self,
    ) -> Result<tracing_appender::rolling::RollingFileAppender, tracing_appender::rolling::InitError>
    {
        let mut builder = tracing_appender::rolling::RollingFileAppender::builder()
            .rotation(tracing_appender::rolling::Rotation::DAILY)
            .filename_prefix(&self.stem)
            .max_log_files(LOG_FILES_KEPT);
        if let Some(extension) = &self.extension {
            builder = builder.filename_suffix(extension);
        }
        builder.build(&self.directory)
    }
}

/// Starts logging in `format`, to stdout or to `file`; an error where the
/// file can't be opened, when nothing is logged.
pub fn init(
    format: LogFormat,
    file: Option<&LogFile>,
    palette: crate::color::Palette,
) -> Result<(), String> {
    let subscriber = match file {
        None => subscriber(
            format,
            // Keep native color adaptation and any partial writes under
            // the same stdout lock for the whole event.
            || anstream::AutoStream::new(std::io::stdout(), crate::color::stream_choice()).lock(),
            palette,
        ),
        Some(file) => {
            let appender = file.appender().map_err(|error| {
                format!(
                    "the log file in {} can't be written: {error}",
                    file.directory.display()
                )
            })?;
            subscriber_at(
                format,
                appender,
                Some(crate::terminal::UNKNOWN_WIDTH),
                crate::color::Palette::plain(),
            )
        }
    };
    let _ = tracing::subscriber::set_global_default(subscriber);
    Ok(())
}

/// A startup error in the log, as stderr gets it, with the exit status it
/// ends the Server with.
pub fn startup_error(message: &str, exit_status: u8) {
    tracing::error!(
        event = event::STARTUP_ERROR,
        error = message,
        exit_status,
        "{message}"
    );
}

/// A startup warning in the log, as stderr gets it, by its name.
pub fn startup_warning(reason: &'static str, message: &str) {
    warn!(event = event::STARTUP_WARNING, reason, "{message}");
}

/// The log's subscriber, writing to `writer`.
fn subscriber<W>(
    format: LogFormat,
    writer: W,
    palette: crate::color::Palette,
) -> Box<dyn tracing::Subscriber + Send + Sync>
where
    W: for<'w> tracing_subscriber::fmt::MakeWriter<'w> + Send + Sync + 'static,
{
    subscriber_at(format, writer, None, palette)
}

fn subscriber_at<W>(
    format: LogFormat,
    writer: W,
    width: Option<usize>,
    palette: crate::color::Palette,
) -> Box<dyn tracing::Subscriber + Send + Sync>
where
    W: for<'w> tracing_subscriber::fmt::MakeWriter<'w> + Send + Sync + 'static,
{
    let builder = tracing_subscriber::fmt()
        .with_writer(writer)
        .with_target(false)
        .with_ansi(false);
    match format {
        LogFormat::Human => Box::new(
            builder
                .event_format(HumanFormat { width, palette })
                .finish(),
        ),
        LogFormat::Json => Box::new(builder.json().flatten_event(true).finish()),
    }
}

/// Format one event as a whole before wrapping it, so its continuations
/// stay together under a single write and cannot resemble a new event.
struct HumanFormat {
    width: Option<usize>,
    palette: crate::color::Palette,
}

impl<S, N> tracing_subscriber::fmt::format::FormatEvent<S, N> for HumanFormat
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    N: for<'a> tracing_subscriber::fmt::format::FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        context: &tracing_subscriber::fmt::FmtContext<'_, S, N>,
        mut writer: tracing_subscriber::fmt::format::Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> std::fmt::Result {
        use tracing_subscriber::fmt::format::Writer;
        let mut text = String::new();
        tracing_subscriber::fmt::format()
            .with_target(false)
            .format_event(context, Writer::new(&mut text), event)?;
        let text = crate::terminal::log_entry(
            &text,
            self.width.unwrap_or_else(crate::terminal::stdout_width),
        );
        writer.write_str(&styled_level(text, *event.metadata().level(), self.palette))
    }
}

/// Style the level token and mute the timestamp after layout. Message
/// values remain neutral; structured event fields use the metadata color.
fn styled_level(mut text: String, level: tracing::Level, palette: crate::color::Palette) -> String {
    use crate::color::Role;
    let role = match level {
        tracing::Level::INFO => Role::Info,
        tracing::Level::WARN => Role::Warning,
        tracing::Level::ERROR => Role::Error,
        _ => return text,
    };
    if !palette.is_colored() {
        return text;
    }
    if let Some(start) = text.find(" event=").map(|start| start + 1) {
        let fields = palette.paint(Role::Muted, &text[start..]);
        text.replace_range(start.., &fields);
    }
    let Some(time_end) = text.find(char::is_whitespace) else {
        return text;
    };
    let after_time = text[time_end..].trim_start();
    let token = after_time.strip_prefix("| ").unwrap_or(after_time);
    let label = level.as_str();
    if token
        .strip_prefix(label)
        .is_some_and(|suffix| suffix.starts_with(char::is_whitespace))
    {
        let start = text.len() - token.len();
        text.replace_range(start..start + label.len(), &palette.paint(role, label));
        text.replace_range(..time_end, &palette.paint(Role::Muted, &text[..time_end]));
    }
    text
}

/// What notices held back carried, summed: the fields a `Suppressed`
/// record adds to its count of them.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Carried {
    pub bursts: u64,
    pub datagrams: u64,
    pub bytes: u64,
    /// Their own `count`s.
    pub count: u64,
}

impl Carried {
    /// What `record` carries.
    fn of(record: &Record) -> Self {
        Self {
            bursts: record.bursts.map_or(0, u64::from),
            datagrams: record.datagrams.map_or(0, u64::from),
            bytes: record.bytes.unwrap_or(0),
            count: record.count.unwrap_or(0),
        }
    }

    fn add(&mut self, other: Self) {
        self.bursts += other.bursts;
        self.datagrams += other.datagrams;
        self.bytes += other.bytes;
        self.count += other.count;
    }
}

/// Notices of one kind held back in a window: how many, and what they
/// carried.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Held {
    pub events: u64,
    pub carried: Carried,
}

/// Counts notices of each kind within the current window.
#[derive(Debug, Default)]
pub struct Limiter {
    started: Option<Instant>,
    total: u32,
    /// Per kind: logged and held back in this window.
    counts: BTreeMap<&'static str, (u32, Held)>,
}

impl Limiter {
    /// Whether a notice of `kind` at `now` may be logged.
    pub fn allow(&mut self, kind: &'static str, now: Instant) -> bool {
        self.allow_carrying(kind, Carried::default(), now)
    }

    /// Whether a notice of `kind` carrying `carried` at `now` may be
    /// logged; if not, what it carried is added to what is held back.
    pub fn allow_carrying(&mut self, kind: &'static str, carried: Carried, now: Instant) -> bool {
        self.started.get_or_insert(now);
        let (logged, held) = self.counts.entry(kind).or_default();
        if *logged < BURST && self.total < TOTAL {
            *logged += 1;
            self.total += 1;
            true
        } else {
            held.events += 1;
            held.carried.add(carried);
            false
        }
    }

    /// Once the window has ended at `now`: what it held back, per kind; the
    /// next window then starts.
    pub fn close(&mut self, now: Instant) -> Vec<(&'static str, Held)> {
        let Some(started) = self.started else {
            return Vec::new();
        };
        if now.duration_since(started) < WINDOW {
            return Vec::new();
        }
        let held = self
            .counts
            .iter()
            .filter(|(_, (_, held))| held.events > 0)
            .map(|(kind, (_, held))| (*kind, *held))
            .collect();
        *self = Self::default();
        held
    }
}

/// A notice as the log writes it: its event, and each field it has.
#[derive(Debug, Default, PartialEq)]
pub struct Record {
    pub event: &'static str,
    pub warning: bool,
    /// The Test Connection; none for the Standard Port.
    pub test_connection_id: Option<u32>,
    pub client: Option<SocketAddr>,
    pub mc_ident: Option<u16>,
    /// The keyId of the Key Table row that authenticated it; none in
    /// Unauthenticated Mode.
    pub key_id: Option<u8>,
    /// A name from [`names`]: why it happened.
    pub reason: Option<&'static str>,
    /// An Activation parameter's RFC 9946 name, and the values asked for
    /// and given.
    pub parameter: Option<&'static str>,
    pub requested: Option<u16>,
    pub coerced: Option<u16>,
    pub direction: Option<&'static str>,
    /// An OS error's text: free, not stable.
    pub error: Option<String>,
    /// How many times it happened.
    pub count: Option<u64>,
    /// A Sending Rate Table row.
    pub row: Option<u16>,
    /// Socket buffer sizes, in bytes.
    pub requested_bytes: Option<u64>,
    pub receive_bytes: Option<u64>,
    pub send_bytes: Option<u64>,
    /// Bursts not sent, their Load PDUs and their UDP payload bytes.
    pub bursts: Option<u32>,
    pub datagrams: Option<u32>,
    pub bytes: Option<u64>,
    /// The fast paths a Data Phase Engine sent Load PDUs with, by their
    /// names, comma-separated; empty for none.
    pub fast_paths: Option<String>,
    /// Receive Buffer Overflows at a Test Connection's socket, where the
    /// OS counts them.
    pub receive_buffer_overflows: Option<u64>,
    /// With kernel send timestamps asked for (experimental): the
    /// Load PDUs stamped, and how much later than their lpduTime the
    /// kernel sent them, in µs.
    pub send_lag_samples: Option<u64>,
    pub send_lag_p50_us: Option<u64>,
    pub send_lag_p99_us: Option<u64>,
    pub send_lag_max_us: Option<u64>,
    /// How late after their scheduled time a Load Sender's Send Plans
    /// were written, its Burst lateness, 99th percentile, in µs
    /// (experimental).
    pub burst_lateness_p99_us: Option<u64>,
    /// The Server's executor, by its name in [`mod@names::executor`], the
    /// Engine Threads the pooled one may start.
    pub executor: Option<&'static str>,
    pub engine_threads: Option<u64>,
    pub message: String,
}

impl Record {
    fn new(event: &'static str, message: impl Into<String>) -> Self {
        Self {
            event,
            message: message.into(),
            ..Self::default()
        }
    }
}

/// A duration in whole µs.
fn micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

/// A Test Connection's id, or none for the Standard Port's socket.
fn test_connection_id(socket: SocketId) -> Option<u32> {
    (socket != SocketId::PRIMARY).then_some(socket.0)
}

/// What the log writes for `notice`, if anything: never the per-Trial
/// Interval, Sub-Interval or st results, which the Client reports.
pub fn record(notice: &ServerNotice, buffer_limits: &str) -> Option<Record> {
    Some(match notice {
        ServerNotice::Listening(_) | ServerNotice::Tick => return None,
        ServerNotice::DataPhaseEngines {
            executor,
            engine_threads,
            error,
        } => Record {
            // The CPUs the process may use weren't found: one Engine
            // Thread.
            warning: error.is_some(),
            executor: Some(names::executor(*executor)),
            engine_threads: engine_threads.map(|threads| threads as u64),
            error: error.clone(),
            ..Record::new(
                event::DATA_PHASE_ENGINES,
                match engine_threads {
                    Some(threads) => {
                        format!("the Data Phase Engine runs on at most {threads} Engine Threads")
                    }
                    None => "the Data Phase Engine runs a thread per Test Connection".into(),
                },
            )
        },
        ServerNotice::Stopping { reason } => Record {
            reason: Some(reason),
            ..Record::new(
                event::STOPPING,
                format!("stopping on {reason}: ending every Test Connection"),
            )
        },
        ServerNotice::NoIpv6Listener(error) => Record {
            warning: true,
            error: Some(error.clone()),
            ..Record::new(event::NO_IPV6_LISTENER, "no IPv6 listener")
        },
        ServerNotice::Core(event) => core_record(event)?,
        ServerNotice::Data { socket, event } => Record {
            test_connection_id: test_connection_id(*socket),
            ..data_record(event)?
        },
        ServerNotice::DataPhaseEnded {
            socket,
            reason,
            error,
            engine_facts,
        } => Record {
            test_connection_id: test_connection_id(*socket),
            fast_paths: Some(names::fast_paths(engine_facts.fast_paths).join(",")),
            receive_buffer_overflows: engine_facts.receive_buffer_overflows,
            send_lag_samples: engine_facts.send_lag.map(|lag| lag.samples),
            send_lag_p50_us: engine_facts.send_lag.map(|lag| micros(lag.p50)),
            send_lag_p99_us: engine_facts.send_lag.map(|lag| micros(lag.p99)),
            send_lag_max_us: engine_facts.send_lag.map(|lag| micros(lag.max)),
            burst_lateness_p99_us: engine_facts.burst_lateness.map(|late| micros(late.p99)),
            // RFC 9946 §9: a watchdog expiry is notified.
            warning: *reason != Some(EndReason::Completed),
            reason: Some(
                reason.map_or(names::outcome::DATA_PHASE_ENGINE_FAILED, names::end_reason),
            ),
            error: error.clone(),
            ..Record::new(
                event::DATA_PHASE_ENDED,
                match reason {
                    Some(EndReason::Watchdog) => {
                        "the Watchdog ended the Test Connection: nothing received for 3 s"
                    }
                    Some(EndReason::Completed) => "the Test Connection's Data Phase completed",
                    Some(_) => "the Test Connection's Data Phase ended early",
                    None => "the Data Phase Engine failed",
                },
            )
        },
        ServerNotice::NoEphemeralPort { socket, error } => Record {
            test_connection_id: test_connection_id(*socket),
            warning: true,
            error: Some(error.clone()),
            ..Record::new(
                event::NO_EPHEMERAL_PORT,
                "no port in the Ephemeral Port range could be bound",
            )
        },
        ServerNotice::SocketError { socket, error } => Record {
            test_connection_id: test_connection_id(*socket),
            warning: true,
            error: Some(error.clone()),
            ..Record::new(event::SOCKET_ERROR, "a socket error")
        },
        ServerNotice::SocketBuffersShort { socket, buffers } => Record {
            test_connection_id: test_connection_id(*socket),
            warning: true,
            requested_bytes: Some(buffers.requested as u64),
            receive_bytes: Some(buffers.receive as u64),
            send_bytes: Some(buffers.send as u64),
            ..Record::new(
                event::SOCKET_BUFFERS_SHORT,
                format!(
                    "socket buffers held below {} bytes (receive {}, send {}); raise {buffer_limits}",
                    buffers.requested, buffers.receive, buffers.send
                ),
            )
        },
        ServerNotice::DatagramsDropped { socket, count } => Record {
            test_connection_id: test_connection_id(*socket),
            warning: true,
            count: Some(*count),
            ..Record::new(
                event::DATAGRAMS_DROPPED,
                "datagrams dropped: the Server fell behind",
            )
        },
    })
}

fn core_record(notice: &Event) -> Option<Record> {
    let at = |event: &'static str, socket: SocketId, message: &str| Record {
        test_connection_id: test_connection_id(socket),
        ..Record::new(event, message)
    };
    Some(match notice {
        Event::TestConnectionAdmitted {
            socket,
            client,
            mc_ident,
            key_id,
        } => Record {
            client: Some(*client),
            mc_ident: Some(*mc_ident),
            key_id: *key_id,
            ..at(
                event::TEST_CONNECTION_ADMITTED,
                *socket,
                "Test Connection admitted",
            )
        },
        Event::ActivationWatchdogExpired { socket } => Record {
            warning: true,
            ..at(
                event::ACTIVATION_WATCHDOG_EXPIRED,
                *socket,
                "no valid Activation Request within 3 s: Test Connection closed",
            )
        },
        Event::UnactivatedTestConnectionEvicted { socket } => at(
            event::UNACTIVATED_TEST_CONNECTION_EVICTED,
            *socket,
            "the oldest Unactivated Test Connection of the Key Table row, or Unauthenticated Mode, holding the most was closed for a new Setup",
        ),
        Event::TroubleshootingModeOn => Record {
            warning: true,
            ..Record::new(
                event::TROUBLESHOOTING_MODE_ON,
                "Troubleshooting Mode is still on",
            )
        },
        Event::TestConnectionReleased { socket } => at(
            event::TEST_CONNECTION_RELEASED,
            *socket,
            "Test Connection released",
        ),
        Event::SetupRequestRejected { from, reason } => Record {
            client: Some(*from),
            reason: Some(names::setup_rejection(*reason)),
            ..Record::new(event::SETUP_REQUEST_REJECTED, "Setup Request refused")
        },
        Event::SetupRequestSilentlyRejected { from, reason } => Record {
            client: Some(*from),
            reason: Some(names::silent_rejection_reason(*reason)),
            ..Record::new(
                event::SETUP_REQUEST_SILENTLY_REJECTED,
                "Setup Request refused silently",
            )
        },
        Event::SetupRequestIgnored { from, reason } => Record {
            client: Some(*from),
            reason: Some(names::setup_ignore_reason(*reason)),
            ..Record::new(event::SETUP_REQUEST_IGNORED, "Setup Request ignored")
        },
        Event::TestConnectionActivated {
            socket, direction, ..
        } => Record {
            direction: Some(names::direction(*direction)),
            ..at(
                event::TEST_CONNECTION_ACTIVATED,
                *socket,
                "Test Connection activated",
            )
        },
        Event::ActivationParameterCoerced { socket, coercion } => Record {
            parameter: Some(names::parameter(coercion.parameter)),
            requested: Some(coercion.requested),
            coerced: Some(coercion.coerced),
            ..at(
                event::ACTIVATION_PARAMETER_COERCED,
                *socket,
                "Activation parameter coerced",
            )
        },
        Event::ActivationRequestSilentlyRejected { socket, reason } => Record {
            reason: Some(names::silent_rejection_reason(*reason)),
            ..at(
                event::ACTIVATION_REQUEST_SILENTLY_REJECTED,
                *socket,
                "Activation Request refused silently",
            )
        },
        Event::ActivationRequestRejected { socket, reason } => Record {
            reason: Some(names::activation_rejection(*reason)),
            parameter: match reason {
                ActivationRejection::UnacceptableParameter(parameter) => {
                    Some(names::parameter(*parameter))
                }
                _ => None,
            },
            ..at(
                event::ACTIVATION_REQUEST_REJECTED,
                *socket,
                "Activation Request refused",
            )
        },
        // The Client's events.
        Event::SetupAccepted { .. }
        | Event::SetupResponseIgnored { .. }
        | Event::NullRequestReceived
        | Event::NullRequestIgnored { .. }
        | Event::ControlPhaseFailed(_)
        | Event::UnverifiedSetupRejection { .. }
        | Event::TestActivated { .. }
        | Event::ActivationRejected
        | Event::ParameterCoerced { .. }
        | Event::ActivationResponseIgnored { .. }
        | Event::AuthMode2Unsupported
        | Event::TestConnectionAbandoned
        | Event::TestAborted(_)
        | Event::PathError(_) => return None,
    })
}

fn data_record(data: &DataEvent) -> Option<Record> {
    let counted = |event: &'static str, count: u32, message: &str| Record {
        count: Some(count.into()),
        ..Record::new(event, message)
    };
    let not_sent = |event: &'static str, bursts: u32, datagrams: u32, bytes: u64, message| Record {
        bursts: Some(bursts),
        datagrams: Some(datagrams),
        bytes: Some(bytes),
        ..Record::new(event, message)
    };
    Some(match data {
        // Per Trial Interval, Sub-Interval or st; DataPhaseEnded reports
        // the end with its reason.
        DataEvent::TrialIntervalClassified(_)
        | DataEvent::SubIntervalMeasured(_)
        | DataEvent::Sent(_)
        | DataEvent::SentEstimated(_)
        | DataEvent::Ended(_)
        | DataEvent::LoadBytes { .. } => return None,
        // Only a Client caps its sending rate, or counts its Trial
        // Intervals.
        DataEvent::SendingRateCapped { .. } | DataEvent::TrialIntervalsCounted(_) => return None,
        DataEvent::BurstsSkipped {
            bursts,
            datagrams,
            bytes,
        } => not_sent(
            event::BURSTS_SKIPPED,
            *bursts,
            *datagrams,
            *bytes,
            "Load PDUs not sent: this host fell behind",
        ),
        DataEvent::BurstsWithheld {
            bursts,
            datagrams,
            bytes,
        } => not_sent(
            event::BURSTS_WITHHELD,
            *bursts,
            *datagrams,
            *bytes,
            "Load PDUs not sent: the send buffer was full",
        ),
        DataEvent::FirstLoadPduReceived => {
            Record::new(event::FIRST_LOAD_PDU_RECEIVED, "first Load PDU")
        }
        DataEvent::FirstStatusPduReceived => {
            Record::new(event::FIRST_STATUS_PDU_RECEIVED, "first Status PDU")
        }
        DataEvent::InvalidLoadPdus { count } => counted(
            event::INVALID_LOAD_PDUS,
            *count,
            "invalid Load PDUs ignored",
        ),
        DataEvent::IgnoredLoadPdus { count } => counted(
            event::IGNORED_LOAD_PDUS,
            *count,
            "Load PDUs ignored for an implausible lpduSeqNo jump",
        ),
        DataEvent::StatusPdusFailedVerification { count } => counted(
            event::STATUS_PDUS_FAILED_VERIFICATION,
            *count,
            "Status PDUs failed authentication",
        ),
        DataEvent::StatusPdusMissing { count } => counted(
            event::STATUS_PDUS_MISSING,
            *count,
            "Status PDUs never arrived",
        ),
        DataEvent::PeerMissedStatusPdus { count } => counted(
            event::PEER_MISSED_STATUS_PDUS,
            *count,
            "the Client missed Status PDUs",
        ),
        DataEvent::PeerRxStopped { count } => counted(
            event::PEER_RX_STOPPED,
            *count,
            "the Client said it had stopped receiving",
        ),
        DataEvent::DelaySamplesDropped { count } => counted(
            event::DELAY_SAMPLES_DROPPED,
            *count,
            "delay samples dropped: the wall clock stepped",
        ),
        DataEvent::InvalidStatusPdus { count } => counted(
            event::INVALID_STATUS_PDUS,
            *count,
            "invalid Status PDUs ignored",
        ),
        DataEvent::PathMtuExceeded { count } => Record {
            warning: true,
            ..counted(
                event::PATH_MTU_EXCEEDED,
                *count,
                "datagrams too large for the path with DF set: a hop's MTU is below the Test's packet size",
            )
        },
        DataEvent::HopLimitExceeded { count } => Record {
            warning: true,
            ..counted(
                event::HOP_LIMIT_EXCEEDED,
                *count,
                "ICMP time exceeded: max_hops is too small for the path",
            )
        },
        DataEvent::IcmpErrors { count } => counted(
            event::ICMP_ERRORS,
            *count,
            "ICMP errors from the path ignored",
        ),
        DataEvent::StaleStatusPdus { count } => counted(
            event::STALE_STATUS_PDUS,
            *count,
            "stale Status PDUs, not used for the sending rate",
        ),
        DataEvent::StopReceived => Record::new(event::STOP_RECEIVED, "STOP received"),
        DataEvent::LostStatusBackoff { row } => Record {
            row: Some(*row),
            ..Record::new(
                event::LOST_STATUS_BACKOFF,
                "no Status PDU in time: backed off as for an Errored Interval",
            )
        },
        DataEvent::TrafficStopped { .. } => Record {
            warning: true,
            ..Record::new(
                event::TRAFFIC_STOPPED,
                "nothing received for 1 s: stopped sending",
            )
        },
    })
}

/// The Server's log.
#[derive(Debug)]
pub struct ServerLog {
    limiter: Limiter,
    buffer_limits: &'static str,
    statistics: Statistics,
    /// How often the counters are reported; zero, never.
    statistics_interval: Duration,
    reported_at: Option<Instant>,
    /// The counters' reports so far: each report's number ties its
    /// `StatisticsRefusals` to its `Statistics`.
    reports: u64,
    /// How client addresses are shown.
    addresses: crate::privacy::Addresses,
}

impl ServerLog {
    /// A log whose buffer warnings say to raise `buffer_limits`, reporting
    /// the counters every `statistics_interval` unless it is zero.
    pub fn new(
        buffer_limits: &'static str,
        statistics_interval: Duration,
        addresses: crate::privacy::Addresses,
    ) -> Self {
        Self {
            addresses,
            limiter: Limiter::default(),
            buffer_limits,
            statistics: Statistics::default(),
            statistics_interval,
            reported_at: None,
            reports: 0,
        }
    }

    /// Logs `notice`, unless too many of its kind came lately.
    pub fn notice(&mut self, notice: &ServerNotice) {
        self.notice_at(notice, Instant::now());
    }

    /// Once the Server has stopped: what the limiter held back, and the
    /// counters a last time, unless they are never reported.
    pub fn stopped(&mut self) {
        self.stopped_at(Instant::now());
    }

    fn stopped_at(&mut self, now: Instant) {
        for (kind, held) in self.limiter.close(now + WINDOW) {
            suppressed(kind, held);
        }
        if !self.statistics_interval.is_zero() {
            self.reports += 1;
            log_counters(self.statistics.counters(), self.reports);
        }
    }

    fn notice_at(&mut self, notice: &ServerNotice, now: Instant) {
        // Every notice, the once-a-second tick too, closes an ended window.
        for (kind, held) in self.limiter.close(now) {
            suppressed(kind, held);
        }
        let summary = self.statistics.observe(notice);
        self.log(notice, now);
        // After the release that ends its Test.
        if let Some(summary) = summary
            && (!summary.unverified() || self.limiter.allow(event::TEST_SUMMARY, now))
        {
            log_summary(&summary, &self.addresses);
        }
        let reported_at = *self.reported_at.get_or_insert(now);
        if !self.statistics_interval.is_zero()
            && now.duration_since(reported_at) >= self.statistics_interval
        {
            self.reported_at = Some(now);
            self.reports += 1;
            log_counters(self.statistics.counters(), self.reports);
        }
    }

    fn log(&mut self, notice: &ServerNotice, now: Instant) {
        if let ServerNotice::Listening(addresses) = notice {
            for address in addresses {
                info!(event = event::LISTENING, %address, "listening on {address}");
            }
            return;
        }
        let Some(record) = record(notice, self.buffer_limits) else {
            return;
        };
        debug_assert!(event::ALL.contains(&record.event), "{}", record.event);
        if !UNLIMITED.contains(&record.event)
            && !self
                .limiter
                .allow_carrying(record.event, Carried::of(&record), now)
        {
            return;
        }
        let client = record.client.map(|c| self.addresses.show(c));
        let Record {
            event,
            warning,
            test_connection_id,
            mc_ident,
            key_id,
            reason,
            parameter,
            requested,
            coerced,
            direction,
            count,
            row,
            requested_bytes,
            receive_bytes,
            send_bytes,
            bursts,
            datagrams,
            bytes,
            send_lag_samples,
            send_lag_p50_us,
            send_lag_p99_us,
            send_lag_max_us,
            burst_lateness_p99_us,
            executor,
            engine_threads,
            ..
        } = record;
        let (client, error) = (client.as_deref(), record.error.as_deref());
        let fast_paths = record.fast_paths.as_deref();
        let receive_buffer_overflows = record.receive_buffer_overflows;
        let message = &record.message;
        macro_rules! emit {
            ($level:ident) => {
                $level!(
                    event,
                    test_connection_id,
                    client,
                    mc_ident,
                    key_id,
                    reason,
                    parameter,
                    requested,
                    coerced,
                    direction,
                    error,
                    count,
                    row,
                    requested_bytes,
                    receive_bytes,
                    send_bytes,
                    bursts,
                    datagrams,
                    bytes,
                    fast_paths,
                    receive_buffer_overflows,
                    send_lag_samples,
                    send_lag_p50_us,
                    send_lag_p99_us,
                    send_lag_max_us,
                    burst_lateness_p99_us,
                    executor,
                    engine_threads,
                    "{message}"
                )
            };
        }
        if warning {
            emit!(warn);
        } else {
            emit!(info);
        }
    }
}

/// Logs what the limiter held back of `kind`: how many, and the sums of
/// what they carried, where they carried it.
fn suppressed(kind: &str, held: Held) {
    let Held { events, carried } = held;
    let some = |n: u64| (n > 0).then_some(n);
    let (bursts, datagrams) = (some(carried.bursts), some(carried.datagrams));
    let (bytes, suppressed_count) = (some(carried.bytes), some(carried.count));
    warn!(
        event = event::SUPPRESSED,
        suppressed_event = kind,
        count = events,
        bursts,
        datagrams,
        bytes,
        suppressed_count,
        "{events} more {kind} not logged"
    );
}

fn log_summary(summary: &TestSummary, addresses: &crate::privacy::Addresses) {
    let TestSummary {
        client,
        mc_ident,
        key_id,
        test_connections,
        direction,
        parameters,
        bytes_sent,
        bytes_received,
        ip_layer_sender_bit_rate,
        skipped,
        withheld,
        receive_buffer_overflows,
        result,
        reason,
    } = summary;
    let client = addresses.show(*client);
    let direction = direction.map(names::direction);
    let p = parameters.as_ref();
    info!(
        event = event::TEST_SUMMARY,
        %client,
        mc_ident,
        key_id,
        test_connections,
        direction,
        test_interval = p.map(|p| p.test_interval.as_secs()),
        sub_interval = p.map(|p| p.sub_interval.as_millis() as u64),
        trial_interval = p.map(|p| p.trial_interval.as_millis() as u64),
        low_thresh = p.map(|p| p.low_thresh),
        upper_thresh = p.map(|p| p.upper_thresh),
        seq_err_thresh = p.map(|p| p.seq_err_thresh),
        slow_adj_thresh = p.map(|p| p.slow_adj_thresh),
        high_speed_delta = p.map(|p| p.high_speed_delta),
        ignore_ooo_dup = p.map(|p| p.ignore_ooo_dup),
        one_way_delay_variation = p.map(|p| p.one_way_delay_variation),
        random_payload = p.map(|p| p.random_payload),
        rate_adj_algo = p.map(|p| names::rate_adj_algo(p.rate_adj_algo)),
        dscp = p.map(|p| p.dscp),
        sr_index_conf = p.map(|p| p.sr_index_conf),
        sr_index_is_start = p.map(|p| p.sr_index_is_start),
        bytes_sent,
        bytes_received,
        ip_layer_sender_bit_rate,
        skipped_bursts = skipped.bursts,
        skipped_datagrams = skipped.datagrams,
        skipped_bytes = skipped.bytes,
        withheld_bursts = withheld.bursts,
        withheld_datagrams = withheld.datagrams,
        withheld_bytes = withheld.bytes,
        receive_buffer_overflows,
        result = *result,
        reason = *reason,
        "Test ended: {result}"
    );
}

fn log_counters(counters: &statistics::Counters, report: u64) {
    info!(
        event = event::STATISTICS,
        report,
        tests = counters.tests,
        test_connections = counters.test_connections,
        active_test_connections = counters.active_test_connections,
        authentication_failures = counters.authentication_failures,
        rate_limited = counters.rate_limited,
        stale_status_pdus = counters.stale_status_pdus,
        setup_requests_ignored = counters.setup_requests_ignored,
        unactivated_test_connections_evicted = counters.unactivated_test_connections_evicted,
        "since start: {} Tests, {} Test Connections, {} active",
        counters.tests,
        counters.test_connections,
        counters.active_test_connections
    );
    for (&(kind, reason), &count) in &counters.refusals {
        info!(
            event = event::STATISTICS_REFUSALS,
            report, kind, reason, count, "since start: {count} {kind} for {reason}"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::protocol::{
        ActivationParameter, Coercion, Direction, MonoTime, SetupRejection, SilentRejectionReason,
    };
    use crate::runtime::SocketBuffers;

    // docs/spec.md story 90 (events an attacker can trigger are
    // rate-limited or aggregated);
    #[test]
    fn a_flood_is_logged_a_burst_at_a_time_then_counted() {
        let mut limiter = Limiter::default();
        let start = Instant::now();
        let allowed = (0..25)
            .filter(|_| limiter.allow("SetupRequestSilentlyRejected", start))
            .count();
        assert_eq!(allowed, BURST as usize);
        // Another kind has a burst of its own.
        assert!(limiter.allow("SocketError", start));
        // Nothing is reported before the window ends; then the count is.
        assert!(limiter.close(start + WINDOW / 2).is_empty());
        let held = Held {
            events: 15,
            carried: Carried::default(),
        };
        assert_eq!(
            limiter.close(start + WINDOW),
            [("SetupRequestSilentlyRejected", held)]
        );
        assert!(limiter.allow("SetupRequestSilentlyRejected", start + WINDOW));
    }

    // : a flood across many kinds is capped as a whole
    #[test]
    fn all_kinds_together_are_capped() {
        let mut limiter = Limiter::default();
        let start = Instant::now();
        let kinds = [
            "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o",
        ];
        let allowed = kinds
            .iter()
            .flat_map(|kind| std::iter::repeat_n(*kind, 20))
            .filter(|kind| limiter.allow(kind, start))
            .count();
        assert_eq!(allowed, TOTAL as usize);
    }

    #[test]
    fn a_dropped_request_is_recorded_with_its_client_and_reason() {
        let notice = ServerNotice::Core(Event::SetupRequestSilentlyRejected {
            from: "192.0.2.9:4000".parse().unwrap(),
            reason: SilentRejectionReason::Malformed,
        });
        let dropped = record(&notice, "").unwrap();
        assert_eq!(dropped.event, "SetupRequestSilentlyRejected");
        assert_eq!(dropped.client, Some("192.0.2.9:4000".parse().unwrap()));
        assert_eq!(dropped.reason, Some("Malformed"));
        // The 1 s traffic stop is a warning; per-st results aren't logged.
        let stopped = ServerNotice::Data {
            socket: SocketId(1),
            event: DataEvent::TrafficStopped {
                silent_since: MonoTime::from_duration(Duration::ZERO),
            },
        };
        assert!(record(&stopped, "").is_some_and(|r| r.warning));
        let tick = ServerNotice::Tick;
        assert!(record(&tick, "").is_none());
    }

    /// A shared buffer the test subscriber writes the log to.
    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Captured {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    // The shared human palette colors level labels while retaining
    // neutral event fields and text (ADR 0013).
    #[test]
    fn human_log_labels_use_the_shared_severity_colors() {
        let captured = Captured::default();
        let subscriber = subscriber(
            LogFormat::Human,
            {
                let captured = captured.clone();
                move || captured.clone()
            },
            crate::color::Palette::colored(),
        );
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                event = "Listening",
                client = "192.0.2.9:24601",
                "INFO in the message"
            );
            tracing::warn!(
                event = "StartupWarning",
                reason = "SocketUnavailable",
                "WARN in the message"
            );
            tracing::error!(
                event = "StartupError",
                exit_status = 75_u8,
                "ERROR in the message"
            );
        });
        let text = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        for label in [
            "\u{1b}[38;2;92;184;255mINFO\u{1b}[0m",
            "\u{1b}[1m\u{1b}[38;2;243;156;18mWARN\u{1b}[0m",
            "\u{1b}[1m\u{1b}[38;2;255;92;87mERROR\u{1b}[0m",
        ] {
            assert!(text.contains(label), "{text}");
        }
        assert!(
            text.starts_with("\u{1b}[38;2;112;120;140m"),
            "muted timestamp: {text}"
        );
        assert!(text.contains("INFO in the message"), "{text}");
        assert!(text.contains("WARN in the message"), "{text}");
        assert!(text.contains("ERROR in the message"), "{text}");
        assert!(text.contains("192.0.2.9:24601"), "{text}");
    }

    /// One item per write lets the formatter's caller observe whether
    /// wrapped lines are kept together.
    #[derive(Clone, Default)]
    struct EventWrites(Arc<Mutex<Vec<Vec<u8>>>>);

    impl std::io::Write for EventWrites {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().push(bytes.to_vec());
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    #[ignore = "writes visual fixtures to UDPSTP_PREVIEW_DIR"]
    fn export_server_preview() {
        let captured = Captured::default();
        let subscriber = subscriber_at(
            LogFormat::Human,
            {
                let captured = captured.clone();
                move || captured.clone()
            },
            Some(120),
            crate::color::Palette::colored(),
        );
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                event = "Listening",
                address = "127.0.0.1:24601",
                "listening on 127.0.0.1:24601"
            );
            tracing::info!(
                event = "TestConnectionAdmitted",
                test_connection_id = 1,
                client = "192.0.2.1:41355",
                "Test Connection admitted"
            );
            tracing::info!(
                event = "DataPhaseEnded",
                test_connection_id = 1,
                reason = "Completed",
                "the Test Connection's Data Phase completed"
            );
            tracing::warn!(
                event = "StartupWarning",
                reason = "SocketBuffersShort",
                "the OS limited the socket buffer size"
            );
        });
        let directory = std::path::PathBuf::from(std::env::var_os("UDPSTP_PREVIEW_DIR").unwrap());
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("server.ansi"), &*captured.0.lock().unwrap()).unwrap();
    }

    // ADR 0013: color changes no visible content, line breaks or field
    // meaning, and each wrapped event remains one write.
    #[test]
    fn colored_and_plain_human_events_have_identical_layout_and_write_grouping() {
        let diagnostic = "the Server could not open its socket for [2001:db8:1234:5678:abcd:eeee:ffff:9999]:24601 after checking all available interfaces; measurements and café remain neutral";
        for width in [60, 80, 100, 120] {
            let capture = |palette| {
                let writes = EventWrites::default();
                let subscriber = subscriber_at(
                    LogFormat::Human,
                    {
                        let writes = writes.clone();
                        move || writes.clone()
                    },
                    Some(width),
                    palette,
                );
                tracing::subscriber::with_default(subscriber, || {
                    tracing::warn!(
                        event = "StartupWarning",
                        reason = "SocketUnavailable",
                        "{diagnostic}"
                    );
                    tracing::error!(event = "StartupError", exit_status = 75_u8, "{diagnostic}");
                    tracing::info!(
                        event = "Stopping",
                        reason = "SIGTERM",
                        "The Server is stopping"
                    );
                    tracing::debug!(event = "Filtered", "debug stays filtered");
                    tracing::trace!(event = "Filtered", "trace stays filtered");
                });
                writes
                    .0
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|bytes| String::from_utf8(bytes.clone()).unwrap())
                    .collect::<Vec<_>>()
            };
            let plain = capture(crate::color::Palette::plain());
            let colored = capture(crate::color::Palette::colored());
            assert_eq!(plain.len(), 3, "one write per event; filtering unchanged");
            assert_eq!(colored.len(), 3, "one write per event; filtering unchanged");
            for (plain, colored) in plain.iter().zip(&colored) {
                assert!(!plain.contains('\u{1b}'));
                let visible = anstream::adapter::strip_str(colored).to_string();
                // Capture times differ; compare every byte after them.
                assert_eq!(
                    visible.split_once(' ').unwrap().1,
                    plain.split_once(' ').unwrap().1
                );
                assert!(
                    visible
                        .lines()
                        .all(|line| crate::terminal::columns(line) <= width),
                    "{visible}"
                );
                assert!(
                    colored.starts_with("\u{1b}[38;2;112;120;140m"),
                    "muted timestamp: {colored}"
                );
                assert!(
                    colored.contains("\u{1b}[38;2;112;120;140mevent="),
                    "structured metadata recedes: {colored}"
                );
                assert!(colored.ends_with('\n'));
            }
            assert!(colored[0].contains("\n  | "));
            assert!(colored[1].contains("\n  | "));
            assert!(
                colored[2].contains("\u{1b}[38;2;92;184;255mINFO\u{1b}[0m The Server is stopping")
            );
        }
    }

    // ADR 0013: only human events wrap. A visible continuation marker
    // keeps adjacent records distinguishable; JSON remains one object
    // per line with exactly the same fields and free text.
    #[test]
    fn wrapped_human_events_remain_distinct_and_json_is_unchanged() {
        let diagnostic = "the Server could not open its socket for [2001:db8:1234:5678:abcd:eeee:ffff:9999]:24601 after checking all available interfaces";
        for width in [60, 80, 100, 120] {
            let captured = Captured::default();
            let subscriber = subscriber_at(
                LogFormat::Human,
                {
                    let captured = captured.clone();
                    move || captured.clone()
                },
                Some(width),
                crate::color::Palette::plain(),
            );
            tracing::subscriber::with_default(subscriber, || {
                tracing::warn!(
                    event = "StartupWarning",
                    reason = "SocketUnavailable",
                    "{diagnostic}"
                );
                tracing::info!(
                    event = "Stopping",
                    reason = "SIGTERM",
                    "The Server is stopping"
                );
            });
            let text = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
            let mut starts = 0;
            for line in text.lines() {
                if !line.starts_with("  | ") {
                    starts += 1;
                }
                assert!(crate::terminal::columns(line) <= width, "{text}");
            }
            assert_eq!(starts, 2, "each event starts once: {text}");
            assert!(text.lines().any(|line| line.starts_with("  | ")), "{text}");
            assert!(!text.contains('\u{1b}'));
            let prose = text.replace("\n  | ", " ");
            assert!(prose.contains(diagnostic), "{text}");

            let captured = Captured::default();
            let subscriber = subscriber_at(
                LogFormat::Json,
                {
                    let captured = captured.clone();
                    move || captured.clone()
                },
                Some(width),
                crate::color::Palette::colored(),
            );
            tracing::subscriber::with_default(subscriber, || {
                tracing::warn!(
                    event = "StartupWarning",
                    reason = "SocketUnavailable",
                    "{diagnostic}"
                );
                tracing::info!(
                    event = "Stopping",
                    reason = "SIGTERM",
                    "The Server is stopping"
                );
            });
            let text = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
            let records: Vec<serde_json::Value> = text
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(records.len(), 2);
            assert_eq!(records[0]["message"], diagnostic);
            assert_eq!(records[0]["event"], "StartupWarning");
            assert_eq!(records[1]["event"], "Stopping");
        }
    }

    /// A notice of every kind the log writes, once each way it can.
    fn every_notice() -> Vec<ServerNotice> {
        let client: SocketAddr = "192.0.2.9:4000".parse().unwrap();
        let socket = SocketId(1);
        let core = ServerNotice::Core;
        let data = |event| ServerNotice::Data { socket, event };
        let parameters = crate::statistics::tests::parameters();
        let mut notices = vec![
            ServerNotice::DataPhaseEngines {
                executor: crate::runtime::Executor::Pool,
                engine_threads: Some(2),
                error: None,
            },
            ServerNotice::DataPhaseEngines {
                executor: crate::runtime::Executor::Pool,
                engine_threads: Some(1),
                error: Some("unsupported".into()),
            },
            ServerNotice::DataPhaseEngines {
                executor: crate::runtime::Executor::PerTestConnection,
                engine_threads: None,
                error: None,
            },
            ServerNotice::Listening(vec!["0.0.0.0:24601".parse().unwrap()]),
            ServerNotice::NoIpv6Listener("unsupported".into()),
            core(Event::TroubleshootingModeOn),
            core(Event::SetupRequestRejected {
                from: client,
                reason: SetupRejection::CapacityExceeded,
            }),
            core(Event::SetupRequestSilentlyRejected {
                from: client,
                reason: SilentRejectionReason::AuthenticationFailed,
            }),
            core(Event::TestConnectionAdmitted {
                socket,
                client,
                mc_ident: 9,
                key_id: Some(3),
            }),
            core(Event::ActivationParameterCoerced {
                socket,
                coercion: Coercion {
                    parameter: ActivationParameter::TestIntTime,
                    requested: 120,
                    coerced: 60,
                },
            }),
            core(Event::SetupRequestIgnored {
                from: client,
                reason: crate::protocol::SetupIgnoreReason::DuplicateSetupRequest,
            }),
            core(Event::ActivationRequestSilentlyRejected {
                socket,
                reason: SilentRejectionReason::KeyIdMismatch,
            }),
            core(Event::ActivationRequestRejected {
                socket,
                reason: ActivationRejection::MixedDirection,
            }),
            core(Event::ActivationRequestRejected {
                socket,
                reason: ActivationRejection::UnacceptableParameter(ActivationParameter::TrialInt),
            }),
            core(Event::TestConnectionActivated {
                socket,
                direction: Direction::Downstream,
                parameters,
            }),
            ServerNotice::SocketBuffersShort {
                socket,
                buffers: SocketBuffers {
                    requested: 2_097_152,
                    receive: 212_992,
                    send: 212_992,
                },
            },
            ServerNotice::DatagramsDropped { socket, count: 3 },
            ServerNotice::SocketError {
                socket: SocketId::PRIMARY,
                error: "refused".into(),
            },
            ServerNotice::SocketError {
                socket,
                error: "refused".into(),
            },
            ServerNotice::NoEphemeralPort {
                socket: SocketId(2),
                error: "in use".into(),
            },
            data(DataEvent::BurstsSkipped {
                bursts: 1,
                datagrams: 3,
                bytes: 3000,
            }),
            data(DataEvent::BurstsWithheld {
                bursts: 1,
                datagrams: 3,
                bytes: 3000,
            }),
            data(DataEvent::FirstLoadPduReceived),
            data(DataEvent::FirstStatusPduReceived),
            data(DataEvent::StopReceived),
            data(DataEvent::LostStatusBackoff { row: 42 }),
            data(DataEvent::Sent(crate::protocol::SentInterval {
                st_seq_no: 1,
                datagrams: 10,
                bytes: 12_000,
                ip_layer_sender_bit_rate: 2_000_000,
                duration: Duration::from_millis(50),
            })),
            data(DataEvent::TrafficStopped {
                silent_since: MonoTime::from_duration(Duration::ZERO),
            }),
        ];
        for counted in [
            DataEvent::InvalidLoadPdus { count: 1 },
            DataEvent::IgnoredLoadPdus { count: 1 },
            DataEvent::StatusPdusFailedVerification { count: 1 },
            DataEvent::StatusPdusMissing { count: 1 },
            DataEvent::PeerMissedStatusPdus { count: 1 },
            DataEvent::PeerRxStopped { count: 1 },
            DataEvent::DelaySamplesDropped { count: 1 },
            DataEvent::InvalidStatusPdus { count: 1 },
            DataEvent::PathMtuExceeded { count: 1 },
            DataEvent::HopLimitExceeded { count: 1 },
            DataEvent::IcmpErrors { count: 1 },
            DataEvent::StaleStatusPdus { count: 1 },
        ] {
            notices.push(data(counted));
        }
        notices.extend([
            ServerNotice::DataPhaseEnded {
                socket,
                reason: Some(EndReason::Completed),
                error: None,
                engine_facts: crate::runtime::EngineFacts {
                    fast_paths: crate::runtime::FastPaths {
                        sendmmsg: true,
                        udp_gso: true,
                        recvmmsg: true,
                        kernel_rx_timestamps: true,
                        ppoll: true,
                        epoll: false,
                    },
                    receive_buffer_overflows: Some(3),
                    ..crate::runtime::EngineFacts::default()
                },
            },
            ServerNotice::DataPhaseEnded {
                socket: SocketId(2),
                reason: None,
                error: Some("the thread panicked".into()),
                engine_facts: crate::runtime::EngineFacts::default(),
            },
            // A pooled Load Sender's, with kernel send timestamps.
            ServerNotice::DataPhaseEnded {
                socket: SocketId(4),
                reason: Some(EndReason::Completed),
                error: None,
                engine_facts: crate::runtime::EngineFacts {
                    fast_paths: crate::runtime::FastPaths {
                        sendmmsg: true,
                        udp_gso: true,
                        recvmmsg: true,
                        kernel_rx_timestamps: true,
                        ppoll: false,
                        epoll: true,
                    },
                    receive_buffer_overflows: Some(0),
                    send_lag: Some(crate::runtime::SendLag {
                        samples: 5_000,
                        p50: Duration::from_micros(12),
                        p99: Duration::from_micros(40),
                        max: Duration::from_micros(310),
                    }),
                    burst_lateness: Some(crate::runtime::SendLag {
                        samples: 5_000,
                        p50: Duration::from_micros(3),
                        p99: Duration::from_micros(21),
                        max: Duration::from_micros(95),
                    }),
                    ..crate::runtime::EngineFacts::default()
                },
            },
            core(Event::ActivationWatchdogExpired {
                socket: SocketId(2),
            }),
            core(Event::UnactivatedTestConnectionEvicted {
                socket: SocketId(2),
            }),
            core(Event::TestConnectionReleased { socket }),
        ]);
        // A Test never activated.
        notices.extend([
            core(Event::TestConnectionAdmitted {
                socket: SocketId(3),
                client,
                mc_ident: 10,
                key_id: None,
            }),
            core(Event::TestConnectionReleased {
                socket: SocketId(3),
            }),
        ]);
        notices
    }

    /// A JSON value's type, as the log's reader sees it.
    fn kind(value: &serde_json::Value) -> &'static str {
        match value {
            serde_json::Value::Null => "null",
            serde_json::Value::Bool(_) => "bool",
            serde_json::Value::Number(n) if n.is_u64() || n.is_i64() => "integer",
            serde_json::Value::Number(_) => "number",
            serde_json::Value::String(_) => "string",
            serde_json::Value::Array(_) => "array",
            serde_json::Value::Object(_) => "object",
        }
    }

    /// A Skipped Burst of 4 Load PDUs on Test Connection `socket`.
    fn skipped(socket: u32) -> ServerNotice {
        ServerNotice::Data {
            socket: SocketId(socket),
            event: DataEvent::BurstsSkipped {
                bursts: 1,
                datagrams: 4,
                bytes: 5000,
            },
        }
    }

    /// The JSON log lines `notices` bring about, all at one instant, then
    /// once the limiter's window has ended.
    fn json_lines(notices: &[ServerNotice]) -> Vec<serde_json::Value> {
        let captured = Captured::default();
        let writer = captured.clone();
        let subscriber = subscriber(
            LogFormat::Json,
            move || writer.clone(),
            crate::color::Palette::plain(),
        );
        tracing::subscriber::with_default(subscriber, || {
            let mut log =
                ServerLog::new("limits", Duration::ZERO, crate::privacy::Addresses::full());
            let start = Instant::now();
            for notice in notices {
                log.notice_at(notice, start);
            }
            log.notice_at(&ServerNotice::Tick, start + WINDOW);
        });
        let text = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        text.lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    // : a busy Server's summary says how much it didn't send, though
    // the limiter suppressed most of the events that said so
    #[test]
    fn a_tests_summary_totals_its_skipped_and_withheld_bursts_whatever_was_suppressed() {
        let client: SocketAddr = "192.0.2.9:4000".parse().unwrap();
        let admitted = |socket| {
            ServerNotice::Core(Event::TestConnectionAdmitted {
                socket: SocketId(socket),
                client,
                mc_ident: 9,
                key_id: None,
            })
        };
        let mut notices = vec![admitted(1), admitted(2)];
        // 30 from each Test Connection: all but BURST are suppressed.
        for _ in 0..30 {
            notices.extend([skipped(1), skipped(2)]);
        }
        notices.push(ServerNotice::Data {
            socket: SocketId(2),
            event: DataEvent::BurstsWithheld {
                bursts: 2,
                datagrams: 3,
                bytes: 4000,
            },
        });
        notices.extend([1, 2].map(|socket| {
            ServerNotice::Core(Event::TestConnectionReleased {
                socket: SocketId(socket),
            })
        }));
        let lines = json_lines(&notices);
        let find = |name: &str| {
            lines
                .iter()
                .find(|line| line["event"] == name)
                .unwrap_or_else(|| panic!("no {name}"))
        };
        let summary = find("TestSummary");
        assert_eq!(summary["skipped_bursts"], 60);
        assert_eq!(summary["skipped_datagrams"], 240);
        assert_eq!(summary["skipped_bytes"], 300_000);
        assert_eq!(summary["withheld_bursts"], 2);
        assert_eq!(summary["withheld_datagrams"], 3);
        assert_eq!(summary["withheld_bytes"], 4000);
        // What the limiter held back is reported with what it carried.
        let suppressed = find("Suppressed");
        assert_eq!(suppressed["suppressed_event"], "BurstsSkipped");
        let held = 60 - u64::from(BURST);
        assert_eq!(suppressed["count"], held);
        assert_eq!(suppressed["bursts"], held);
        assert_eq!(suppressed["datagrams"], 4 * held);
        assert_eq!(suppressed["bytes"], 5000 * held);
    }

    // docs/spec.md story 83 (structured logs, asserted on structure, never
    // text);  (one meaning per field; every event from the const
    // table)
    #[test]
    fn every_json_log_line_has_its_events_keys_and_types() {
        let captured = Captured::default();
        let writer = captured.clone();
        let subscriber = subscriber(
            LogFormat::Json,
            move || writer.clone(),
            crate::color::Palette::plain(),
        );
        tracing::subscriber::with_default(subscriber, || {
            // What a Server logging to a file reports as it starts.
            startup_warning(
                names::startup_warning::DONT_FRAGMENT_UNSUPPORTED,
                "a warning",
            );
            startup_error("a configuration error", 50);
            let mut log = ServerLog::new(
                "limits",
                Duration::from_secs(1),
                crate::privacy::Addresses::full(),
            );
            let start = Instant::now();
            for notice in every_notice() {
                log.notice_at(&notice, start);
            }
            // Floods, suppressed; then the window and the statistics
            // interval end.
            for _ in 0..=BURST {
                log.notice_at(
                    &ServerNotice::DatagramsDropped {
                        socket: SocketId(1),
                        count: 1,
                    },
                    start,
                );
                log.notice_at(&skipped(1), start);
            }
            log.notice_at(&ServerNotice::Tick, start + WINDOW);
            log.notice_at(
                &ServerNotice::Stopping { reason: "SIGTERM" },
                start + WINDOW,
            );
            log.stopped_at(start + WINDOW);
        });
        let text = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        let mut shapes = BTreeSet::new();
        let mut seen = BTreeSet::new();
        for line in text.lines() {
            let value: serde_json::Value =
                serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {line}"));
            let object = value.as_object().expect("an object a line");
            let name = object["event"].as_str().expect("an event name");
            assert!(event::ALL.contains(&name), "{name} isn't in the table");
            seen.insert(name.to_owned());
            let keys: Vec<String> = object
                .iter()
                .filter(|(key, _)| key.as_str() != "event")
                .map(|(key, value)| format!("{key}:{}", kind(value)))
                .collect();
            shapes.insert(format!("{name}\t{}\n", keys.join(" ")));
        }
        crate::results::tests::golden("log-fields.txt", &shapes.into_iter().collect::<String>());
        let unseen: Vec<&str> = event::ALL
            .into_iter()
            .filter(|name| !seen.contains(*name))
            .collect();
        assert!(unseen.is_empty(), "never logged: {unseen:?}");
    }
}
