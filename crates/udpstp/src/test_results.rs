//! What the Client says once its Test has run (docs/spec.md "Results and
//! output"; ): the Test's results, computed once from the runtime's
//! [`TestOutcome`] and the options that shape them, in [`TestResults`].
//! The text results (`results.rs`) and the JSON results (`json.rs`) only
//! render it. [`print_with_color()`] is a pure function of the Test, so every Test's
//! output is tested without a network.

use std::net::SocketAddr;
use std::time::Duration;

use udpstp_core::{
    ActivationParameter, Coercion, DataEvent, Direction, EndReason, Evaluation, Event,
    FixedRateTest, IpVersion, LoadSender, MeasurementResults, PerformanceCriterion, SearchTest,
    SubIntervalMeasurement, TestAbort, TestParameters,
};
use udpstp_runtime::{
    ClientOutcome, EngineFacts, FastPaths, SOCKET_BUFFER_LIMITS, SendLag, SocketBuffers,
    TestOutcome,
};

use crate::color::{Palette, Role};
use crate::test_connection_end::{self, TestConnectionEnd};
use crate::warnings::{self, DataPhaseEnd, MissedSubInterval, Warning};
use crate::{exit, json, names, privacy, results};

/// What shapes a Test's results, from the Client's options.
pub struct ResultOptions {
    /// JSON results (`--format json`): stdout then holds only the JSON.
    pub json: bool,
    /// The JSON results' `udpstp` object (`--json-extension`).
    pub json_extension: bool,
    /// How the JSON results show this Client's own address, its
    /// pseudonym key read at the start.
    pub addresses: privacy::Addresses,
    /// The SERVERs as typed, with their addresses.
    pub hosts: Vec<(String, SocketAddr)>,
    pub direction: Direction,
    /// A Fixed-Rate Test rather than a Search Test.
    pub fixed_rate: bool,
    /// The Test Connections and the Minimum Test Connections.
    pub test_connections: u8,
    pub minimum: u8,
    pub criterion: PerformanceCriterion,
    /// `--faster-sub-interval-margin`, in percent.
    pub faster_sub_interval_margin: u32,
    /// `--first-mode-sub-intervals`.
    pub first_mode: Option<u32>,
    /// The DSCP asked for.
    pub dscp: u8,
    pub max_hops: u8,
    /// DF set on test traffic: asked for, and possible on this host.
    pub dont_fragment: bool,
    /// The Sending Rate Table variant asked for.
    pub jumbo: bool,
    pub traditional_mtu: bool,
    /// maxBandwidth asked for, in Mbps; 0 for none.
    pub max_bandwidth: u16,
    /// `--time-limit`, in seconds.
    pub time_limit: u64,
}

impl ResultOptions {
    /// The first SERVER's address.
    fn server(&self) -> SocketAddr {
        self.hosts[0].1
    }
}

/// Which stream a text goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// What the Client prints once its Test has run, and its status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Printed {
    /// Each text as written, with its line breaks, in order.
    pub output: Vec<(Stream, String)>,
    pub status: exit::Status,
}

/// Collects what is printed, in order.
pub(crate) struct Writer {
    output: Vec<(Stream, String)>,
}

/// The renderer determines where final Notices are said.
#[derive(Clone, Copy)]
pub(crate) enum NoticeOutput {
    Text,
    Json,
}

impl Writer {
    pub(crate) fn new() -> Self {
        Self { output: Vec::new() }
    }

    /// Text on stdout, as it is.
    pub(crate) fn out(&mut self, text: String) {
        self.output.push((Stream::Stdout, text));
    }

    /// A line on stdout.
    pub(crate) fn outln(&mut self, text: String) {
        self.output.push((Stream::Stdout, format!("{text}\n")));
    }

    /// A line on stderr.
    pub(crate) fn errln(&mut self, text: String) {
        self.output.push((Stream::Stderr, format!("{text}\n")));
    }

    /// Emit final Notices in list order, styled for their destination.
    pub(crate) fn notices(
        &mut self,
        notices: &[Notice],
        output: NoticeOutput,
        _stdout: Palette,
        stderr: Palette,
    ) {
        for notice in notices {
            match (notice.said, output) {
                // The human report ends with its aggregate typed outcome.
                // Keep the compatible progress notice for JSON only.
                (Said::Progress, NoticeOutput::Text) => {}
                (Said::Always, NoticeOutput::Text) => self.errln(notice.render(stderr, false)),
                (Said::Always | Said::WithJson | Said::Progress, NoticeOutput::Json) => {
                    self.errln(notice.render(stderr, true));
                }
                (Said::WithJson | Said::Never, NoticeOutput::Text)
                | (Said::Never, NoticeOutput::Json) => {}
            }
        }
    }

    pub(crate) fn into_output(self) -> Vec<(Stream, String)> {
        self.output
    }
}

/// What the Client prints once `test` has run, activated at `started`
/// (T0) and ended at `ended`, both since the Unix epoch: its results as
/// text or JSON, and its status.
#[cfg(test)]
pub fn print(
    test: &TestOutcome,
    options: &ResultOptions,
    started: Option<Duration>,
    ended: Duration,
) -> Printed {
    print_with_color(
        test,
        options,
        started,
        ended,
        Palette::plain(),
        Palette::plain(),
    )
}

/// Human styles belong to each destination. Structured stdout always
/// keeps the JSON renderer's bytes, including when styles are forced.
pub fn print_with_color(
    test: &TestOutcome,
    options: &ResultOptions,
    started: Option<Duration>,
    ended: Duration,
    stdout: Palette,
    stderr: Palette,
) -> Printed {
    let results = TestResults::new(test, options, started, ended);
    let output = if options.json {
        json::print(&results, stderr)
    } else {
        results::print_with_color(&results, stdout, stderr)
    };
    Printed {
        output,
        status: results.status,
    }
}

/// Everything the Client says of a Test once it has run, built once from
/// the runtime's [`TestOutcome`] and the options.
pub struct TestResults<'o> {
    pub options: &'o ResultOptions,
    /// T0 and the end, since the Unix epoch.
    pub started: Option<Duration>,
    pub ended: Duration,
    /// Its ErrorStatus, and whether it completed (`exit.rs`).
    pub status: exit::Status,
    /// Its mcIdent.
    pub mc_ident: u16,
    /// The Client user interrupted it.
    pub interrupted: bool,
    /// Why the Multi-Connection Coordinator aborted it, if it did.
    pub abort: Option<TestAbort>,
    /// Each Test Connection, by mcIndex.
    pub connections: Vec<ConnectionResults>,
    /// The Test Connection that stands for the Test where the results name
    /// one, as the JSON's `Input` does: the first activated, else the
    /// first.
    pub first: usize,
    /// The Test's results as a whole, once a Test Connection was
    /// activated: its one Test Connection's, or several's summed.
    pub whole: Option<Block>,
    /// What is said as the Test ends, and what went wrong, in order.
    pub notices: Vec<Notice>,
}

/// One Test Connection, as the results show it.
pub struct ConnectionResults {
    pub server: SocketAddr,
    /// Its socket's local address.
    pub local: Option<SocketAddr>,
    /// The Ephemeral Port its Setup Response gave.
    pub test_port: Option<u16>,
    /// How it ended: its status, its names and what is said of it.
    pub end: TestConnectionEnd,
    pub coercions: Vec<Coercion>,
    /// Its own results, once it was activated.
    pub block: Option<Block>,
}

/// A Test Connection's results, or a Test's as a whole, once activated.
#[derive(Clone)]
pub struct Block {
    /// The Test Connection's Server; for a Test, that of the Test
    /// Connection that stands for it.
    pub server: SocketAddr,
    pub direction: Direction,
    /// As the Activation Response gave them.
    pub parameters: TestParameters,
    /// The DSCP its traffic carried: as asked, unless the Server coerced
    /// it.
    pub dscp: u8,
    /// The coercions its text lists.
    pub coercions: Vec<Coercion>,
    pub measured: Measured,
    /// How its Data Phase ran on this host, as its Data Phase Engine
    /// reported; for a Test of several Test Connections, as
    /// `engine_facts_of_test` combines theirs.
    pub engine_facts: EngineFacts,
    /// What makes its results less than they seem, in order.
    pub warnings: Vec<Warning>,
}

impl Block {
    fn qualified(&self) -> bool {
        !self.warnings.is_empty() || self.measured.results.lacks_a_maximum()
    }
}

/// A Test's measurement results, or a Test Connection's, as the core
/// computed them, and when this host learned each Sub-Interval's: a
/// runtime fact, which the JSON results show.
#[derive(Clone)]
pub struct Measured {
    pub results: MeasurementResults,
    /// By subIntSeqNo from 1, since the Unix epoch.
    pub times: Vec<Option<Duration>>,
}

/// How a Multi-Connection Test ran on this host, from what each of its
/// activated Test Connections' Data Phase Engines reported, with how it
/// ended: the one place a fact the engine adds is combined.
/// - the fast paths every one that completed used, so a fallback on any
///   shows; if none completed, every one whose Data Phase Engine ran until
///   its Data Phase ended. One whose engine failed or never ran reports
///   none, and one that ended early may not have needed them all (UDP GSO
///   only for a Burst of several Load PDUs): neither says what this host
///   could do;
/// - the smallest socket buffers the OS granted one, which is where
///   datagrams would be dropped;
/// - their Receive Buffer Overflows summed;
/// - when measured, their kernel send timestamps, and their Burst
///   lateness, each counted together, with
///   the largest of their lags, since percentiles don't combine.
fn engine_facts_of_test<'a>(
    each: impl IntoIterator<Item = (&'a EngineFacts, &'a TestConnectionEnd)>,
) -> EngineFacts {
    let each: Vec<(&EngineFacts, &TestConnectionEnd)> = each.into_iter().collect();
    let fast_paths_of = |ran: fn(&TestConnectionEnd) -> bool| {
        each.iter()
            .filter(|(_, end)| ran(end))
            .map(|(facts, _)| facts.fast_paths)
            .reduce(|a, b| FastPaths {
                sendmmsg: a.sendmmsg && b.sendmmsg,
                udp_gso: a.udp_gso && b.udp_gso,
                recvmmsg: a.recvmmsg && b.recvmmsg,
                kernel_rx_timestamps: a.kernel_rx_timestamps && b.kernel_rx_timestamps,
                ppoll: a.ppoll && b.ppoll,
                epoll: a.epoll && b.epoll,
            })
    };
    let fast_paths = fast_paths_of(TestConnectionEnd::completed)
        .or_else(|| fast_paths_of(TestConnectionEnd::data_phase_ended))
        .unwrap_or_default();
    let each: Vec<&EngineFacts> = each.iter().map(|(facts, _)| *facts).collect();
    let socket_buffers =
        each.iter()
            .filter_map(|f| f.socket_buffers)
            .reduce(|a, b| SocketBuffers {
                requested: a.requested.max(b.requested),
                receive: a.receive.min(b.receive),
                send: a.send.min(b.send),
            });
    let receive_buffer_overflows = each
        .iter()
        .filter_map(|f| f.receive_buffer_overflows)
        .reduce(|a, b| a + b);
    // Counted together, with the largest of their percentiles.
    let merged = |lags: Vec<SendLag>| {
        lags.into_iter().reduce(|a, b| SendLag {
            samples: a.samples + b.samples,
            p50: a.p50.max(b.p50),
            p99: a.p99.max(b.p99),
            max: a.max.max(b.max),
        })
    };
    EngineFacts {
        socket_buffers,
        fast_paths,
        receive_buffer_overflows,
        send_lag: merged(each.iter().filter_map(|f| f.send_lag).collect()),
        burst_lateness: merged(each.iter().filter_map(|f| f.burst_lateness).collect()),
    }
}

/// Something said as a Test ends. The JSON's `ErrorMessage` and
/// `ErrorMessage2` are the last two, progress aside.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub level: Level,
    /// The human severity, independent of the JSON message's prefix.
    pub role: Role,
    pub text: String,
    pub said: Said,
    /// A typed Test Connection outcome at the end of this notice's text.
    outcome_start: Option<usize>,
}

/// How a notice starts in the JSON's error messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Plain,
    Warning,
    Error,
}

/// Where a notice is said, besides the JSON's error messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Said {
    /// On stderr, with text or JSON results.
    Always,
    /// On stderr with JSON results; the text results show it otherwise.
    WithJson,
    /// Nowhere else: it was said as it happened.
    Never,
    /// Progress, not a notice: on stdout with text results, stderr with
    /// JSON, and never an error message.
    Progress,
}

impl Notice {
    fn new(level: Level, text: impl Into<String>, said: Said) -> Self {
        Self {
            level,
            role: match level {
                Level::Plain => Role::Info,
                Level::Warning => Role::Warning,
                Level::Error => Role::Error,
            },
            text: text.into(),
            said,
            outcome_start: None,
        }
    }

    fn with_role(mut self, role: Role) -> Self {
        self.role = role;
        self
    }

    fn with_outcome(mut self, description: &str) -> Self {
        self.outcome_start = Some(self.text.len() - description.len());
        self
    }

    /// Style labels while addresses, measurements and diagnostic values
    /// retain the default foreground. Progress here is a Test outcome.
    pub(crate) fn render(&self, palette: Palette, json: bool) -> String {
        if self.said == Said::Progress {
            return format!(
                "{}{}",
                palette.marker(self.role),
                palette.paint(self.role, &self.text)
            );
        }
        let text = self.outcome_start.map_or_else(
            || self.text.clone(),
            |start| {
                let (prefix, outcome) = self.text.split_at(start);
                format!("{prefix}{}", styled_outcome(palette, self.role, outcome))
            },
        );
        let message = if json {
            match self.level {
                Level::Plain => text,
                Level::Warning => format!("{} {text}", palette.paint(Role::Warning, "WARNING:")),
                Level::Error => format!("{} {text}", palette.paint(Role::Error, "ERROR:")),
            }
        } else {
            text
        };
        format!("{} {message}", palette.paint(self.role, "udpstp:"))
    }

    /// As the JSON's error messages and its stderr give it.
    pub fn message(&self) -> String {
        match self.level {
            Level::Plain => self.text.clone(),
            Level::Warning => format!("WARNING: {}", self.text),
            Level::Error => format!("ERROR: {}", self.text),
        }
    }
}

/// Descriptions can end with a socket or engine error. Only their outcome
/// label is styled; the diagnostic's addresses and values stay neutral.
pub(crate) fn styled_outcome(palette: Palette, role: Role, description: &str) -> String {
    match description.split_once(':') {
        Some((label, detail)) => format!("{}{detail}", palette.paint(role, &format!("{label}:"))),
        None => palette.paint(role, description),
    }
}

impl<'o> TestResults<'o> {
    /// The results of `test`, activated at `started` (T0) and ended at
    /// `ended`.
    pub fn new(
        test: &TestOutcome,
        options: &'o ResultOptions,
        started: Option<Duration>,
        ended: Duration,
    ) -> Self {
        let interrupted = test
            .events
            .contains(&Event::TestAborted(TestAbort::Interrupted));
        let connections: Vec<ConnectionResults> = test
            .connections
            .iter()
            .map(|c| ConnectionResults::new(c, options, interrupted))
            .collect();
        let first = connections
            .iter()
            .position(|c| c.block.is_some())
            .unwrap_or(0);
        let multi = options.test_connections > 1;
        let mut whole = if multi {
            summed(test, options, &connections, first, interrupted)
        } else {
            connections[0].block.clone()
        };
        // What may understate the path is judged only when every activated
        // Test Connection completed, since one that ended otherwise
        // measured less for another reason.
        if let Some(whole) = &mut whole
            && every_activated_completed(&connections)
        {
            let not_kept = sending_rate_not_kept(options, whole, &connections);
            whole.warnings.extend(not_kept);
            let never = congestion_never_confirmed(options, whole, &connections, test);
            whole.warnings.extend(never);
            let missed = sub_intervals_after_maximum_missed(options, whole, &connections, test);
            whole.warnings.extend(missed);
        }
        let mut results = Self {
            options,
            started,
            ended,
            status: exit::test_status(test),
            mc_ident: test.mc_ident,
            interrupted,
            abort: test.events.iter().find_map(|event| match event {
                Event::TestAborted(reason) => Some(*reason),
                _ => None,
            }),
            connections,
            first,
            whole,
            notices: Vec::new(),
        };
        results.notices = if multi {
            results.multi_connection_notices()
        } else {
            results.single_notices(&test.connections[0])
        };
        if results.no_valid_maximum() {
            results.notices.push(Notice::new(
                Level::Warning,
                Warning::NoValidMaximum.message(),
                Said::Never,
            ));
        }
        results
    }

    /// Whether a completed Test has a Capacity Mode without a Maximum
    /// IP-Layer Capacity: the most severe warning, ErrorStatus 49, which the
    /// JSON reports.
    pub fn no_valid_maximum(&self) -> bool {
        self.status.completed
            && self
                .whole
                .as_ref()
                .is_some_and(|whole| whole.measured.results.lacks_a_maximum())
    }

    /// A Test's human outcome accounts for partial completion and result
    /// qualifications separately from its compatible process exit status.
    pub(crate) fn outcome(&self) -> (Role, &'static str) {
        if self.interrupted {
            (Role::Error, "interrupted")
        } else if self.abort.is_some() {
            (Role::Error, "aborted")
        } else if !self.status.completed {
            (Role::Error, "failed")
        } else if self.connections.iter().any(|c| !c.end.completed()) {
            (Role::Warning, "partially completed")
        } else if self.status.error_status != exit::SUCCESS
            || self
                .connections
                .iter()
                .any(|c| c.outcome_role() != Role::Success)
            || self.whole.as_ref().is_none_or(Block::qualified)
        {
            (
                Role::Warning,
                "completed with warnings or qualified results",
            )
        } else {
            (Role::Success, "completed")
        }
    }

    /// Include qualifications assembled for the Test as a whole when
    /// their typed attribution names this Test Connection.
    pub(crate) fn connection_outcome_role(&self, index: usize) -> Role {
        let role = self.connections[index].outcome_role();
        if role != Role::Success {
            return role;
        }
        let attributed = self.whole.as_ref().is_some_and(|whole| {
            whole.warnings.iter().any(|warning| match warning {
                Warning::DataPhaseEngineFailed {
                    test_connection, ..
                }
                | Warning::DataPhaseEnded {
                    test_connection, ..
                }
                | Warning::CongestionNeverConfirmed {
                    test_connection, ..
                } => test_connection.is_some_and(|named| usize::from(named) == index),
                _ => false,
            })
        });
        if attributed { Role::Warning } else { role }
    }

    /// The JSON's ErrorStatus: the Test's, or 49 for a completed Test with
    /// no valid Maximum IP-Layer Capacity.
    pub fn error_status(&self) -> u8 {
        if self.no_valid_maximum() {
            exit::NO_VALID_MAXIMUM
        } else {
            self.status.error_status
        }
    }

    /// Every Test Connection's coercions, by mcIndex.
    pub fn coercions(&self) -> Vec<(u8, Coercion)> {
        self.connections
            .iter()
            .enumerate()
            .flat_map(|(index, c)| {
                c.coercions
                    .iter()
                    .map(move |coercion| (index as u8, *coercion))
            })
            .collect()
    }

    /// How the Test ended, by name, as the JSON's `EndReason` gives it:
    /// with one Test Connection, how it ended; with several, the Test's
    /// own end: `Completed`, why the Multi-Connection Coordinator
    /// aborted it, or that no Test Connection completed. Each Test
    /// Connection's own is in its results.
    pub fn end_name(&self) -> &'static str {
        if self.options.test_connections == 1 {
            return self.connections[0].end.end_name();
        }
        match self.abort {
            Some(reason) => names::test_abort(reason),
            None if self.status.completed => names::end_reason(EndReason::Completed),
            None => names::test_end::NO_TEST_CONNECTION_COMPLETED,
        }
    }

    /// Why the Test's Control Phase failed, by name, as the JSON's
    /// `ControlPhaseFailure` gives it: with one Test Connection, if its
    /// failed; with several, never, each Test Connection's being its own
    ///
    pub fn control_phase_failure_name(&self) -> Option<&'static str> {
        if self.options.test_connections == 1 {
            self.connections[0].end.control_phase_failure_name()
        } else {
            None
        }
    }

    /// What is said of a Multi-Connection Test as it ends: each Test
    /// Connection that didn't complete, and on stderr what a Test of it
    /// alone would say of how it ended, naming it; then, once, what of that
    /// concerns the whole Test, naming the Test Connections it was said of;
    /// then why the Test was aborted, or that none completed.
    fn multi_connection_notices(&self) -> Vec<Notice> {
        let mut notices: Vec<Notice> = Vec::new();
        let mut whole_test: Vec<(String, Vec<u8>)> = Vec::new();
        for (index, c) in self.connections.iter().enumerate() {
            if c.end.completed() {
                continue;
            }
            notices.push(
                Notice::new(
                    Level::Warning,
                    format!(
                        "Test Connection {index} to {}: {}",
                        c.server,
                        c.end.description()
                    ),
                    Said::WithJson,
                )
                .with_role(Role::Error)
                .with_outcome(&c.end.description()),
            );
            let mc_index = index as u8;
            for line in c.end.said(self.options, Some(mc_index), self.interrupted) {
                if !line.whole_test {
                    notices.push(
                        Notice::new(Level::Plain, line.text, Said::Always).with_role(Role::Error),
                    );
                } else if let Some((_, named)) =
                    whole_test.iter_mut().find(|(text, _)| *text == line.text)
                {
                    named.push(mc_index);
                } else {
                    whole_test.push((line.text, vec![mc_index]));
                }
            }
        }
        for (text, said_of) in whole_test {
            notices.push(
                Notice::new(
                    Level::Plain,
                    format!("{}: {text}", test_connection_end::named(&said_of)),
                    Said::Always,
                )
                .with_role(Role::Error),
            );
        }
        match self.abort {
            Some(reason) => notices.push(Notice::new(
                Level::Error,
                format!("the Test was aborted: {}", abort_reason(reason)),
                Said::Always,
            )),
            None if self.status.error_status == exit::CONNECTIONS_UNAVAILABLE => {
                notices.push(Notice::new(
                    Level::Error,
                    "the Test didn't complete: no Test Connection completed",
                    Said::WithJson,
                ));
            }
            None => {}
        }
        notices
    }

    /// What is said of a Test of one Test Connection as it ends.
    fn single_notices(&self, outcome: &ClientOutcome) -> Vec<Notice> {
        let end = &self.connections[0].end;
        let mut notices: Vec<Notice> = Vec::new();
        let aborted = self.interrupted;
        if aborted {
            notices.push(
                Notice::new(Level::Plain, "the Test was interrupted", Said::Always)
                    .with_role(Role::Error),
            );
        }
        // The 1 s warning came first, as it happened.
        if outcome
            .data_events
            .iter()
            .any(|e| matches!(e, DataEvent::TrafficStopped { .. }))
        {
            notices.push(Notice::new(
                Level::Warning,
                "nothing received from the Server for 1 s; stopped sending",
                Said::Never,
            ));
        }
        if end.completed() {
            notices.push(
                Notice::new(Level::Plain, "Test completed", Said::Progress)
                    .with_role(self.outcome().0),
            );
        }
        // How it ended, said on stderr as the Test ends.
        notices.extend(
            end.said(self.options, None, aborted)
                .into_iter()
                .map(|line| {
                    Notice::new(Level::Plain, line.text, Said::Always).with_role(Role::Error)
                }),
        );
        // A Test that didn't complete ends with the error last, as
        // OB-UDPST's ErrorMessage, the cause before it.
        if self.status.error_status == exit::CONNECTIONS_UNAVAILABLE {
            notices.push(Notice::new(
                Level::Error,
                "the Test didn't complete: fewer than 1 Test Connection remained",
                Said::Never,
            ));
        }
        notices
    }
}

impl ConnectionResults {
    pub(crate) fn outcome_role(&self) -> Role {
        if !self.end.completed() {
            Role::Error
        } else if self.end.warning().is_some() || self.block.as_ref().is_none_or(Block::qualified) {
            Role::Warning
        } else {
            Role::Success
        }
    }

    /// A Test Connection's `outcome`, of a Test the Client user
    /// `interrupted` or not.
    fn new(outcome: &ClientOutcome, options: &ResultOptions, interrupted: bool) -> Self {
        let end = TestConnectionEnd::of(outcome);
        let coercions: Vec<Coercion> = outcome
            .events
            .iter()
            .filter_map(|e| match e {
                Event::ParameterCoerced { coercion } => Some(*coercion),
                _ => None,
            })
            .collect();
        let block = activated(outcome).map(|(direction, parameters)| {
            let results = MeasurementResults::of_test_connection(
                &outcome.data_events,
                &evaluation(&parameters, outcome.server, options),
            );
            let times = sub_interval_times(
                &[(&outcome.data_events, &outcome.data_event_times)],
                results.sub_intervals.len(),
            );
            let engine_facts = outcome.engine_facts;
            let ends: Vec<DataPhaseEnd<'_>> = end.data_phase_end(None).into_iter().collect();
            let warnings = block_warnings(
                &outcome.data_events,
                &ends,
                interrupted,
                options,
                &results,
                engine_facts,
            );
            Block {
                server: outcome.server,
                direction,
                parameters,
                dscp: carried(&coercions, options.dscp),
                coercions: coercions.clone(),
                measured: Measured { results, times },
                engine_facts,
                warnings,
            }
        });
        Self {
            server: outcome.server,
            local: outcome.local,
            test_port: outcome.events.iter().find_map(|e| match e {
                Event::SetupAccepted { test_port } => Some(*test_port),
                _ => None,
            }),
            end,
            coercions,
            block,
        }
    }
}

/// A Multi-Connection Test's results as a whole: the core's sum of its
/// Test Connections', each Sub-Interval timed by the last of
/// them to report it, and every other Data Phase event of theirs. The rest
/// is that of the Test Connection `first` that stands for the Test.
/// `None` if none was activated.
fn summed(
    test: &TestOutcome,
    options: &ResultOptions,
    connections: &[ConnectionResults],
    first: usize,
    interrupted: bool,
) -> Option<Block> {
    let base = connections[first].block.as_ref()?;
    let one_server = options.hosts.iter().all(|(_, s)| *s == options.server());
    let each: Vec<&[DataEvent]> = test
        .connections
        .iter()
        .map(|c| c.data_events.as_slice())
        .collect();
    let results = MeasurementResults::of_test(
        &each,
        options.direction,
        one_server,
        &evaluation(&base.parameters, base.server, options),
    );
    let reported: Vec<(&[DataEvent], &[Duration])> = test
        .connections
        .iter()
        .map(|c| (c.data_events.as_slice(), c.data_event_times.as_slice()))
        .collect();
    let times = sub_interval_times(&reported, results.sub_intervals.len());
    // Every Test Connection's other Data Phase events.
    let data_events: Vec<DataEvent> = test
        .connections
        .iter()
        .flat_map(|c| &c.data_events)
        .filter(|event| {
            !matches!(
                event,
                DataEvent::SubIntervalMeasured(_)
                    | DataEvent::Sent(_)
                    | DataEvent::SentEstimated(_)
            )
        })
        .copied()
        .collect();
    // How every Test Connection's Data Phase ran here.
    let engine_facts = engine_facts_of_test(
        connections
            .iter()
            .filter_map(|c| c.block.as_ref().map(|block| (&block.engine_facts, &c.end))),
    );
    // Each Test Connection's end, named: the Test's own is its abort, if
    // any.
    let ends: Vec<DataPhaseEnd<'_>> = connections
        .iter()
        .enumerate()
        .filter_map(|(index, c)| c.end.data_phase_end(Some(index as u8)))
        .collect();
    let warnings = block_warnings(
        &data_events,
        &ends,
        interrupted,
        options,
        &results,
        engine_facts,
    );
    Some(Block {
        measured: Measured { results, times },
        engine_facts,
        warnings,
        ..base.clone()
    })
}

/// Whether a Downstream Fixed-Rate Test's Server sent well below its
/// sending rate where the result was measured, as the core judges it
/// (`FixedRateTest`): the warning, if so. Only a Downstream Test's
/// Client estimates what its Server sent, and a Search Test's
/// Server tells it no sending rate: its Load PDUs carry none.
fn sending_rate_not_kept(
    options: &ResultOptions,
    whole: &Block,
    connections: &[ConnectionResults],
) -> Option<Warning> {
    if options.direction != Direction::Downstream || !options.fixed_rate {
        return None;
    }
    let rows: Vec<u16> = connections
        .iter()
        .filter_map(|c| c.block.as_ref())
        .map(|block| block.parameters.sr_index_conf)
        .collect();
    let test = FixedRateTest {
        jumbo: options.jumbo,
        traditional_mtu: options.traditional_mtu,
    };
    let not_kept = test.sending_rate_not_kept(&rows, &whole.measured.results)?;
    Some(Warning::SendingRateNotKept {
        estimated_bps: not_kept.estimated_bps,
        sending_bps: not_kept.sending_bps,
        modes: not_kept.modes,
    })
}

/// Whether no Test Connection of a Search Test found the path's limit, as
/// this Client counted their Trial Intervals, neither confirming
/// Congestion nor meeting it at its end: then one warning for each, saying
/// why its search fell short, as the core judges it (`SearchTest`),
/// naming it in a Multi-Connection Test.
fn congestion_never_confirmed(
    options: &ResultOptions,
    whole: &Block,
    connections: &[ConnectionResults],
    test: &TestOutcome,
) -> Vec<Warning> {
    if options.fixed_rate {
        return Vec::new();
    }
    let activated: Vec<(usize, &ClientOutcome)> = connections
        .iter()
        .zip(&test.connections)
        .enumerate()
        .filter(|(_, (c, _))| c.block.is_some())
        .map(|(index, (_, outcome))| (index, outcome))
        .collect();
    let each: Vec<&[DataEvent]> = activated
        .iter()
        .map(|(_, outcome)| outcome.data_events.as_slice())
        .collect();
    let search = SearchTest {
        parameters: whole.parameters,
        ip_version: IpVersion::of(whole.server),
        max_bandwidth: options.max_bandwidth,
        test_connections: options.test_connections,
        jumbo: options.jumbo,
        traditional_mtu: options.traditional_mtu,
    };
    let multi = options.test_connections > 1;
    search
        .shortfalls(&each)
        .into_iter()
        .zip(&activated)
        .map(
            |(shortfall, (index, _))| Warning::CongestionNeverConfirmed {
                shortfall,
                direction: whole.direction,
                // rateAdjAlgo 1 (RFC 9946 §12.3.8).
                algorithm_c: whole.parameters.rate_adj_algo == 1,
                test_connection: multi.then_some(*index as u8),
            },
        )
        .collect()
}

/// Whether no Sub-Interval after a Search Test's Maximum IP-Layer
/// Capacity's met the Performance Criterion, with one that missed it,
/// before the Maximum's or after it, more than
/// `--faster-sub-interval-margin` faster, or the last one far slower, as
/// the core judges it from what this host knows of the Load Sender: a warning for each Capacity Mode, naming it when there
/// are two. A Fixed-Rate Test's rate doesn't cycle around the bottleneck,
/// so its later Sub-Intervals lose for other reasons.
fn sub_intervals_after_maximum_missed(
    options: &ResultOptions,
    whole: &Block,
    connections: &[ConnectionResults],
    test: &TestOutcome,
) -> Vec<Warning> {
    if options.fixed_rate {
        return Vec::new();
    }
    let each: Vec<&[DataEvent]> = connections
        .iter()
        .zip(&test.connections)
        .filter(|(c, _)| c.block.is_some())
        .map(|(_, outcome)| outcome.data_events.as_slice())
        .collect();
    let results = &whole.measured.results;
    let two = results.capacity_modes.len() > 1;
    let named = |m: SubIntervalMeasurement| MissedSubInterval {
        sub_interval: m.sub_int_seq_no,
        bps: m.ip_layer_capacity,
        lost: u64::from(m.loss),
        sent: m.datagrams_sent(),
    };
    results
        .sub_intervals_after_maximum_missed(
            options.criterion,
            options.faster_sub_interval_margin,
            LoadSender::of(whole.direction, &each),
        )
        .into_iter()
        .map(|missed| Warning::SubIntervalsAfterMaximumMissed {
            capacity_mode: two.then_some(missed.capacity_mode + 1),
            maximum_bps: missed.maximum.ip_layer_capacity,
            faster_before: missed.faster_before.map(named),
            faster: missed.faster.map(named),
            slow_last: missed.slow_last.map(named),
            max_loss_ppm: options.criterion.max_loss_ppm,
            direction: whole.direction,
        })
        .collect()
}

/// Whether every activated Test Connection completed: the gate on the
/// warnings that the result may understate the path.
fn every_activated_completed(connections: &[ConnectionResults]) -> bool {
    connections
        .iter()
        .all(|c| c.block.is_none() || c.end.completed())
}

/// What makes a block's results less than they seem.
fn block_warnings(
    data_events: &[DataEvent],
    ends: &[DataPhaseEnd<'_>],
    interrupted: bool,
    options: &ResultOptions,
    results: &MeasurementResults,
    engine_facts: EngineFacts,
) -> Vec<Warning> {
    warnings::of(&warnings::Sources {
        data_events,
        ends,
        interrupted,
        first_mode: options.first_mode,
        sub_intervals: u32::try_from(results.sub_intervals.len()).unwrap_or(u32::MAX),
        engine_facts,
        socket_buffer_limits: SOCKET_BUFFER_LIMITS,
        approximate: results.approximate,
    })
}

/// A Test Connection's direction and parameters, once it was activated.
fn activated(outcome: &ClientOutcome) -> Option<(Direction, TestParameters)> {
    outcome.events.iter().find_map(|e| match e {
        Event::TestActivated {
            direction,
            parameters,
        } => Some((*direction, *parameters)),
        _ => None,
    })
}

/// How the results of a Test Connection activated with `parameters`, to
/// `server`, are evaluated.
fn evaluation(
    parameters: &TestParameters,
    server: SocketAddr,
    options: &ResultOptions,
) -> Evaluation {
    Evaluation {
        test_interval: parameters.test_interval,
        sub_interval: parameters.sub_interval,
        ip_version: IpVersion::of(server),
        criterion: options.criterion,
        first_mode: options.first_mode,
    }
}

/// The DSCP the traffic carried: `asked`, unless the Server coerced it.
fn carried(coercions: &[Coercion], asked: u8) -> u8 {
    coercions
        .iter()
        .find(|c| c.parameter == ActivationParameter::DscpEcn)
        .map_or(asked, |c| (c.coerced >> 2) as u8)
}

/// When this host learned each of `expected` Sub-Intervals' results, by
/// subIntSeqNo from 1: when a Test Connection's first report of it reached
/// the runtime, or for several, the last of theirs.
pub(crate) fn sub_interval_times(
    connections: &[(&[DataEvent], &[Duration])],
    expected: usize,
) -> Vec<Option<Duration>> {
    let mut times: Vec<Option<Duration>> = vec![None; expected];
    for (events, at) in connections {
        let mut seen = vec![false; expected];
        for (event, at) in events.iter().zip(at.iter()) {
            let DataEvent::SubIntervalMeasured(m) = event else {
                continue;
            };
            let Some(i) = (m.sub_int_seq_no as usize)
                .checked_sub(1)
                .filter(|&i| i < expected && !seen[i])
            else {
                continue;
            };
            seen[i] = true;
            times[i] = Some(times[i].map_or(*at, |t| t.max(*at)));
        }
    }
    times
}

/// Why the Multi-Connection Coordinator aborted a Test, in words.
fn abort_reason(reason: TestAbort) -> String {
    match reason {
        TestAbort::TooFewTestConnections { minimum } => {
            format!("fewer than {minimum} Test Connections (--min-test-connections) could run")
        }
        TestAbort::ParametersDiffer { mc_index } => {
            format!("the Server gave Test Connection {mc_index} other parameters than the first")
        }
        TestAbort::TestConnectionEnded { mc_index } => {
            format!("Test Connection {mc_index} ended it")
        }
        TestAbort::TestConnectionsTimedOut => {
            "several Test Connections stopped hearing their Servers together".into()
        }
        TestAbort::Interrupted => "it was interrupted".into(),
    }
}

/// A coercion as the Client reports it: dscpEcn in the units --dscp
/// takes, the rest by their RFC 9946 names.
pub fn coercion_text(coercion: &Coercion) -> String {
    if coercion.parameter == ActivationParameter::DscpEcn {
        return format!(
            "The Server set the DSCP to {} (asked for {})",
            coercion.coerced >> 2,
            coercion.requested >> 2
        );
    }
    format!(
        "The Server set {} to {} (asked for {})",
        names::parameter(coercion.parameter),
        coercion.coerced,
        coercion.requested
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use udpstp_core::{
        ControlExchange, ControlPhaseFailure, DelayRange, MonoTime, ResponseIgnoreReason,
        SentInterval, SetupRejectionCode, SubIntervalMeasurement, TrialIntervalCounts,
    };
    use udpstp_runtime::{ClientResult, FastPaths, SendLag, SocketBuffers};

    /// T0: when the fixtures' Tests were activated, since the Unix epoch.
    const T0: u64 = 1_790_000_000;

    /// `ms` after T0.
    fn at(ms: u64) -> Duration {
        Duration::from_secs(T0) + Duration::from_millis(ms)
    }

    fn address(text: &str) -> SocketAddr {
        text.parse().unwrap()
    }

    fn parameters() -> TestParameters {
        TestParameters {
            test_interval: Duration::from_secs(5),
            sub_interval: Duration::from_secs(1),
            trial_interval: Duration::from_millis(50),
            low_thresh: 30,
            upper_thresh: 90,
            seq_err_thresh: 10,
            slow_adj_thresh: 3,
            high_speed_delta: 10,
            ignore_ooo_dup: true,
            one_way_delay_variation: true,
            random_payload: false,
            rate_adj_algo: 0,
            dscp: 0,
            sr_index_conf: u16::MAX,
            sr_index_is_start: false,
        }
    }

    /// Sub-Interval `seq`, at `mbps`, with `loss` of its Load PDUs lost.
    fn measurement(seq: u32, mbps: u64, loss: u32) -> SubIntervalMeasurement {
        let datagrams = 1000;
        SubIntervalMeasurement {
            sub_int_seq_no: seq,
            datagrams,
            // The UDP payload that makes `mbps` over 1 s on IPv4.
            bytes: mbps * 1_000_000 / 8 - u64::from(datagrams) * 28,
            ip_layer_capacity: mbps * 1_000_000,
            loss,
            out_of_order: 0,
            duplicates: 0,
            delay_variation: Some(DelayRange {
                min: 0,
                max: 12,
                mean: 7,
            }),
            delay_variation_sum: 70,
            delay_variation_count: 10,
            rtt_variation: Some((0, 12)),
            rtt_variation_sum: 80,
            rtt_variation_count: 10,
            rtt_variation_mean: Some(8),
            rtt_minimum: Some(1),
            clock_delta_min: Some(0),
            active_test_connections: 1,
            measured_at: MonoTime::from_duration(Duration::from_secs(seq.into())),
            duration: Duration::from_secs(1),
            accum_time: Duration::from_secs(seq.into()),
        }
    }

    /// A Downstream Data Phase's events, `offset` ms into each Sub-Interval:
    /// Sub-Intervals 1 to `sub_intervals` at about `mbps` (the fourth
    /// loses 200 Load PDUs), the st the Load Receiver estimated, two
    /// invalid Load PDUs, and `end`.
    fn downstream(
        sub_intervals: u32,
        mbps: u64,
        offset: u64,
        end: EndReason,
    ) -> Vec<(DataEvent, Duration)> {
        let mut events = vec![(DataEvent::FirstLoadPduReceived, at(offset))];
        for seq in 1..=sub_intervals {
            for st in 1..=20 {
                let st_seq_no = (seq - 1) * 20 + st;
                events.push((
                    DataEvent::SentEstimated(SentInterval {
                        st_seq_no,
                        datagrams: 50,
                        bytes: mbps * 1_000_000 / 8 / 20 - 50 * 28,
                        ip_layer_sender_bit_rate: mbps * 1_000_000,
                        duration: Duration::from_millis(50),
                    }),
                    at(u64::from(st_seq_no) * 50 + offset),
                ));
            }
            let loss = if seq == 4 { 200 } else { 0 };
            events.push((
                DataEvent::SubIntervalMeasured(measurement(seq, mbps + u64::from(seq), loss)),
                at(u64::from(seq) * 1000 + offset),
            ));
        }
        events.push((DataEvent::InvalidLoadPdus { count: 2 }, at(5_200 + offset)));
        events.push((DataEvent::Ended(end), at(5_600 + offset)));
        events
    }

    /// A Test Connection to `server` that was never activated.
    fn connection(server: &str, events: Vec<Event>, result: ClientResult) -> ClientOutcome {
        ClientOutcome {
            server: address(server),
            events,
            data_events: Vec::new(),
            data_event_times: Vec::new(),
            local: Some(address("0.0.0.0:40000")),
            mc_ident: 45366,
            result,
            engine_facts: EngineFacts::default(),
        }
    }

    /// A Test Connection to `server` whose Test initiation timer expired
    /// in `exchange`, the last response ignored there for `last_ignored`,
    /// after `events`: its failure as the core reports it, last.
    fn timed_out(
        server: &str,
        mut events: Vec<Event>,
        exchange: ControlExchange,
        last_ignored: Option<ResponseIgnoreReason>,
    ) -> ClientOutcome {
        let failure = ControlPhaseFailure::TestInitiationTimedOut {
            exchange,
            last_ignored,
        };
        events.push(Event::ControlPhaseFailed(failure));
        connection(server, events, ClientResult::Failed(failure))
    }

    /// A Downstream Test Connection to `server`, activated with
    /// `parameters` after the Server coerced testIntTime, whose Data Phase
    /// gave `data` and ended with `reason`.
    fn activated(
        server: &str,
        test_port: u16,
        parameters: TestParameters,
        data: Vec<(DataEvent, Duration)>,
        reason: EndReason,
    ) -> ClientOutcome {
        let (data_events, data_event_times) = data.into_iter().unzip();
        ClientOutcome {
            data_events,
            data_event_times,
            engine_facts: EngineFacts {
                socket_buffers: Some(SocketBuffers {
                    requested: 2_097_152,
                    receive: 4_194_304,
                    send: 4_194_304,
                }),
                receive_buffer_overflows: Some(0),
                ..EngineFacts::default()
            },
            ..connection(
                server,
                vec![
                    Event::SetupAccepted { test_port },
                    Event::ParameterCoerced {
                        coercion: Coercion {
                            parameter: ActivationParameter::TestIntTime,
                            requested: 10,
                            coerced: 5,
                        },
                    },
                    Event::TestActivated {
                        direction: Direction::Downstream,
                        parameters,
                    },
                ],
                ClientResult::Finished {
                    direction: Direction::Downstream,
                    reason: Some(reason),
                    error: None,
                },
            )
        }
    }

    /// The options of a Downstream Search Test to `hosts`, one Test
    /// Connection to each, all of them needed, with JSON results or text.
    pub(crate) fn options(json: bool, hosts: &[&str]) -> ResultOptions {
        ResultOptions {
            json,
            json_extension: true,
            addresses: privacy::Addresses::full(),
            hosts: hosts
                .iter()
                .map(|host| ((*host).to_owned(), address(host)))
                .collect(),
            direction: Direction::Downstream,
            fixed_rate: false,
            test_connections: hosts.len() as u8,
            minimum: hosts.len() as u8,
            criterion: PerformanceCriterion::DEFAULT,
            faster_sub_interval_margin: udpstp_core::FASTER_SUB_INTERVAL_MARGIN_PERCENT,
            first_mode: None,
            dscp: 0,
            max_hops: 16,
            dont_fragment: true,
            jumbo: true,
            traditional_mtu: false,
            max_bandwidth: 0,
            time_limit: 3,
        }
    }

    const FIRST: &str = "192.0.2.1:24601";
    const SECOND: &str = "192.0.2.2:24601";

    /// One Test Connection's Test, as each characterization case ends.
    fn single(end: &str) -> TestOutcome {
        let (events, connection) = match end {
            "completed" => (
                vec![],
                activated(
                    FIRST,
                    51646,
                    parameters(),
                    downstream(5, 90, 17, EndReason::Completed),
                    EndReason::Completed,
                ),
            ),
            // Its first Setup Response came outside the time window, a
            // second was accepted, and its Activation Response failed
            // verification.
            "timed-out" => (
                vec![],
                timed_out(
                    FIRST,
                    vec![
                        Event::SetupResponseIgnored {
                            reason: ResponseIgnoreReason::AuthTimeOutOfWindow,
                        },
                        Event::SetupAccepted { test_port: 51646 },
                        Event::ActivationResponseIgnored {
                            reason: ResponseIgnoreReason::AuthenticationFailed,
                        },
                    ],
                    ControlExchange::Activation,
                    Some(ResponseIgnoreReason::AuthenticationFailed),
                ),
            ),
            "refused" => {
                let refused =
                    ControlPhaseFailure::SetupRejected(SetupRejectionCode::CapacityExceeded);
                (
                    vec![],
                    connection(
                        FIRST,
                        vec![Event::ControlPhaseFailed(refused)],
                        ClientResult::Failed(refused),
                    ),
                )
            }
            // The Client user interrupted it after two Sub-Intervals.
            "aborted" => (
                vec![Event::TestAborted(TestAbort::Interrupted)],
                activated(
                    FIRST,
                    51646,
                    parameters(),
                    downstream(2, 90, 17, EndReason::Stopped),
                    EndReason::Stopped,
                ),
            ),
            _ => unreachable!("{end}"),
        };
        TestOutcome {
            mc_ident: 45366,
            events,
            connections: vec![connection],
        }
    }

    /// The first Test Connection's host ran differently from the second's:
    /// other fast paths and socket buffers, and Receive Buffer Overflows.
    fn with_other_host_facts(mut c: ClientOutcome) -> ClientOutcome {
        c.engine_facts = EngineFacts {
            socket_buffers: Some(SocketBuffers {
                requested: 2_097_152,
                receive: 8_388_608,
                send: 8_388_608,
            }),
            fast_paths: FastPaths {
                sendmmsg: false,
                udp_gso: false,
                recvmmsg: true,
                kernel_rx_timestamps: true,
                ppoll: true,
                epoll: false,
            },
            receive_buffer_overflows: Some(3),
            ..EngineFacts::default()
        };
        c
    }

    /// A Multi-Connection Test to two Servers, as each characterization
    /// case ends.
    fn multi(end: &str) -> TestOutcome {
        let too_few = Event::TestAborted(TestAbort::TooFewTestConnections { minimum: 2 });
        let abandoned = || {
            connection(
                SECOND,
                vec![
                    Event::SetupAccepted { test_port: 51647 },
                    Event::TestConnectionAbandoned,
                ],
                ClientResult::Abandoned,
            )
        };
        let (events, connections) = match end {
            "completed" => (
                vec![],
                vec![
                    with_other_host_facts(activated(
                        FIRST,
                        51646,
                        parameters(),
                        downstream(5, 60, 17, EndReason::Completed),
                        EndReason::Completed,
                    )),
                    activated(
                        SECOND,
                        51647,
                        parameters(),
                        downstream(5, 30, 23, EndReason::Completed),
                        EndReason::Completed,
                    ),
                ],
            ),
            "timed-out" => (
                vec![too_few],
                vec![
                    timed_out(FIRST, vec![], ControlExchange::Setup, None),
                    abandoned(),
                ],
            ),
            "refused" => {
                let refused = ControlPhaseFailure::SetupRejected(
                    SetupRejectionCode::ConnectionAllocationFailure,
                );
                (
                    vec![too_few],
                    vec![
                        connection(
                            FIRST,
                            vec![Event::ControlPhaseFailed(refused)],
                            ClientResult::Failed(refused),
                        ),
                        abandoned(),
                    ],
                )
            }
            // The second Server gave other parameters: the Test stopped
            // both after two Sub-Intervals.
            "aborted" => (
                vec![Event::TestAborted(TestAbort::ParametersDiffer {
                    mc_index: 1,
                })],
                vec![
                    with_other_host_facts(activated(
                        FIRST,
                        51646,
                        parameters(),
                        downstream(2, 60, 17, EndReason::Stopped),
                        EndReason::Stopped,
                    )),
                    activated(
                        SECOND,
                        51647,
                        TestParameters {
                            trial_interval: Duration::from_millis(100),
                            ..parameters()
                        },
                        downstream(2, 30, 23, EndReason::Stopped),
                        EndReason::Stopped,
                    ),
                ],
            ),
            _ => unreachable!("{end}"),
        };
        TestOutcome {
            mc_ident: 45366,
            events,
            connections,
        }
    }

    /// An activated Test Connection's Test, made Upstream.
    fn upstream(mut c: ClientOutcome) -> ClientOutcome {
        for event in &mut c.events {
            if let Event::TestActivated { direction, .. } = event {
                *direction = Direction::Upstream;
            }
        }
        if let ClientResult::Finished { direction, .. } = &mut c.result {
            *direction = Direction::Upstream;
        }
        c
    }

    /// An Upstream Data Phase's events: the Client sent at `mbps`, counting
    /// each st itself, and the Server measured Sub-Intervals 1 to
    /// `sub_intervals`, whose results reached this host in its Status PDUs
    /// `lag` ms after each ended.
    fn upstream_data(
        sub_intervals: u32,
        mbps: u64,
        lag: u64,
        end: EndReason,
    ) -> Vec<(DataEvent, Duration)> {
        let mut events = vec![(DataEvent::FirstStatusPduReceived, at(lag))];
        for seq in 1..=sub_intervals {
            for st in 1..=20 {
                let st_seq_no = (seq - 1) * 20 + st;
                events.push((
                    DataEvent::Sent(SentInterval {
                        st_seq_no,
                        datagrams: 50,
                        bytes: mbps * 1_000_000 / 8 / 20 - 50 * 28,
                        ip_layer_sender_bit_rate: mbps * 1_000_000,
                        duration: Duration::from_millis(50),
                    }),
                    at(u64::from(st_seq_no) * 50),
                ));
            }
            let learned = u64::from(seq) * 1000 + lag;
            events.push((
                DataEvent::SubIntervalMeasured(SubIntervalMeasurement {
                    measured_at: MonoTime::from_duration(Duration::from_millis(learned)),
                    ..measurement(seq, mbps - u64::from(seq), 0)
                }),
                at(learned),
            ));
        }
        events.push((DataEvent::Ended(end), at(5_600 + lag)));
        events
    }

    /// A Test the first sixteen transcripts miss: its name, the Test, and
    /// its options, with text or JSON results.
    type Case = (&'static str, TestOutcome, fn(bool) -> ResultOptions);

    /// What the first sixteen transcripts miss: ErrorStatus 49 with
    /// the Sub-Intervals split into two Capacity Modes; an Upstream
    /// Multi-Connection Test, with the sender rate the Client counted and
    /// sums of Sub-Intervals learned apart; a Fixed-Rate Test that the
    /// Watchdog ended after 1 s of silence; an interrupted Multi-Connection
    /// Test, one of whose Test Connections failed at its socket; a Data
    /// Phase Engine that failed, with the JSON's extension off; Tests that
    /// timed out for other causes, several at once; a Search Test that
    /// never confirmed Congestion, its Server's sending having stopped
    /// rising; and Downstream Fixed-Rate Tests whose Servers sent
    /// well below their rows' sending rate, as the Client estimated it,
    /// with one Test Connection and with two.
    fn more_cases() -> Vec<Case> {
        let passing_first_mode_only = {
            let mut data = downstream(5, 90, 17, EndReason::Completed);
            for (event, _) in &mut data {
                if let DataEvent::SubIntervalMeasured(m) = event
                    && m.sub_int_seq_no > 2
                {
                    m.loss = 250;
                }
            }
            activated(FIRST, 51646, parameters(), data, EndReason::Completed)
        };
        let fast = FastPaths {
            sendmmsg: true,
            udp_gso: true,
            recvmmsg: true,
            kernel_rx_timestamps: true,
            ppoll: true,
            epoll: false,
        };
        let mut upstream_first = upstream(activated(
            FIRST,
            51646,
            parameters(),
            upstream_data(5, 60, 17, EndReason::Completed),
            EndReason::Completed,
        ));
        upstream_first.engine_facts.fast_paths = fast;
        let mut upstream_second = upstream(activated(
            FIRST,
            51647,
            parameters(),
            upstream_data(5, 30, 317, EndReason::Completed),
            EndReason::Completed,
        ));
        upstream_second.engine_facts.fast_paths = FastPaths {
            udp_gso: false,
            ..fast
        };
        let watchdog = {
            let mut data = downstream(3, 90, 17, EndReason::Watchdog);
            let end = data.pop().expect("an end");
            data.push((
                DataEvent::TrafficStopped {
                    silent_since: MonoTime::from_duration(Duration::from_millis(3_017)),
                },
                at(4_017),
            ));
            data.push(end);
            activated(
                FIRST,
                51646,
                TestParameters {
                    sr_index_conf: 90,
                    ..parameters()
                },
                data,
                EndReason::Watchdog,
            )
        };
        let engine_failed = {
            let mut c = activated(
                FIRST,
                51646,
                parameters(),
                downstream(2, 90, 17, EndReason::Completed),
                EndReason::Completed,
            );
            // A failed engine reports no end.
            c.data_events.pop();
            c.data_event_times.pop();
            c.result = ClientResult::Finished {
                direction: Direction::Downstream,
                reason: None,
                error: Some("the Data Phase panicked".into()),
            };
            c
        };
        let one = |connection| TestOutcome {
            mc_ident: 45366,
            events: vec![],
            connections: vec![connection],
        };
        vec![
            (
                "single-first-mode-no-maximum",
                one(passing_first_mode_only),
                |json| ResultOptions {
                    first_mode: Some(2),
                    ..options(json, &[FIRST])
                },
            ),
            (
                "multi-upstream",
                TestOutcome {
                    mc_ident: 45366,
                    events: vec![],
                    connections: vec![upstream_first, upstream_second],
                },
                |json| ResultOptions {
                    direction: Direction::Upstream,
                    test_connections: 2,
                    minimum: 2,
                    ..options(json, &[FIRST])
                },
            ),
            ("single-fixed-rate-watchdog", one(watchdog), |json| {
                ResultOptions {
                    fixed_rate: true,
                    ..options(json, &[FIRST])
                }
            }),
            (
                "multi-interrupted",
                TestOutcome {
                    mc_ident: 45366,
                    events: vec![Event::TestAborted(TestAbort::Interrupted)],
                    connections: vec![
                        activated(
                            FIRST,
                            51646,
                            parameters(),
                            downstream(2, 60, 17, EndReason::Stopped),
                            EndReason::Stopped,
                        ),
                        connection(
                            SECOND,
                            vec![Event::SetupAccepted { test_port: 51647 }],
                            ClientResult::SocketError(
                                "Network is unreachable (os error 101)".into(),
                            ),
                        ),
                    ],
                },
                |json| ResultOptions {
                    minimum: 1,
                    ..options(json, &[FIRST, SECOND])
                },
            ),
            ("single-engine-failed", one(engine_failed), |json| {
                ResultOptions {
                    json_extension: false,
                    ..options(json, &[FIRST])
                }
            }),
            // Mode 2 to a Server that rejected it unverified.
            (
                "single-timed-out-mode-2",
                one(timed_out(
                    FIRST,
                    vec![
                        Event::SetupResponseIgnored {
                            reason: ResponseIgnoreReason::AuthenticationFailed,
                        },
                        Event::UnverifiedSetupRejection {
                            cmd_response: udpstp_wire::setup::cmd_response::AUTH_MODE_INVALID,
                            protocol_version: 20,
                            unsigned: false,
                        },
                        Event::AuthMode2Unsupported,
                    ],
                    ControlExchange::Setup,
                    Some(ResponseIgnoreReason::AuthenticationFailed),
                )),
                |json| options(json, &[FIRST]),
            ),
            // One Test Connection's Setup Response came late, the other's
            // Activation Response failed verification.
            (
                "multi-timed-out-apart",
                TestOutcome {
                    mc_ident: 45366,
                    events: vec![Event::TestAborted(TestAbort::TooFewTestConnections {
                        minimum: 2,
                    })],
                    connections: vec![
                        timed_out(
                            FIRST,
                            vec![Event::SetupResponseIgnored {
                                reason: ResponseIgnoreReason::AuthTimeOutOfWindow,
                            }],
                            ControlExchange::Setup,
                            Some(ResponseIgnoreReason::AuthTimeOutOfWindow),
                        ),
                        timed_out(
                            SECOND,
                            vec![
                                Event::SetupAccepted { test_port: 51647 },
                                Event::ActivationResponseIgnored {
                                    reason: ResponseIgnoreReason::AuthenticationFailed,
                                },
                            ],
                            ControlExchange::Activation,
                            Some(ResponseIgnoreReason::AuthenticationFailed),
                        ),
                    ],
                },
                |json| options(json, &[FIRST, SECOND]),
            ),
            (
                "single-congestion-never-confirmed",
                one(activated(
                    FIRST,
                    51646,
                    parameters(),
                    counted(
                        downstream(5, 90, 17, EndReason::Completed),
                        trial_intervals(109, 0, 0, false),
                    ),
                    EndReason::Completed,
                )),
                |json| options(json, &[FIRST]),
            ),
            // A search through a drop-tail queue.
            (
                "single-after-maximum-faster",
                one(activated(
                    FIRST,
                    51646,
                    parameters(),
                    cliff(),
                    EndReason::Completed,
                )),
                |json| options(json, &[FIRST]),
            ),
            // A queue that kept dropping after it overflowed.
            (
                "single-after-maximum-slower",
                one(activated(
                    FIRST,
                    51646,
                    parameters(),
                    kept_dropping(61),
                    EndReason::Completed,
                )),
                |json| options(json, &[FIRST]),
            ),
            // A search that climbed back slowly after its overshoot.
            (
                "single-before-maximum-faster",
                one(activated(
                    FIRST,
                    51646,
                    parameters(),
                    slow_climb_back(),
                    EndReason::Completed,
                )),
                |json| options(json, &[FIRST]),
            ),
            // Row 90, 90 Mbps, from a Server that sent about 72.
            (
                "single-fixed-rate-sending-rate-not-kept",
                one(activated(
                    FIRST,
                    51646,
                    TestParameters {
                        sr_index_conf: 90,
                        ..parameters()
                    },
                    downstream(5, 72, 17, EndReason::Completed),
                    EndReason::Completed,
                )),
                |json| ResultOptions {
                    fixed_rate: true,
                    ..options(json, &[FIRST])
                },
            ),
            // Row 45 each, 90 Mbps together, from a Server that kept it and
            // one that sent about 20 Mbps, on hosts with other facts, whose
            // warnings come first.
            (
                "multi-fixed-rate-sending-rate-not-kept",
                TestOutcome {
                    mc_ident: 45366,
                    events: vec![],
                    connections: vec![
                        with_other_host_facts(activated(
                            FIRST,
                            51646,
                            TestParameters {
                                sr_index_conf: 45,
                                ..parameters()
                            },
                            downstream(5, 45, 17, EndReason::Completed),
                            EndReason::Completed,
                        )),
                        activated(
                            SECOND,
                            51647,
                            TestParameters {
                                sr_index_conf: 45,
                                ..parameters()
                            },
                            downstream(5, 20, 23, EndReason::Completed),
                            EndReason::Completed,
                        ),
                    ],
                },
                |json| ResultOptions {
                    fixed_rate: true,
                    ..options(json, &[FIRST, SECOND])
                },
            ),
        ]
    }

    /// What was printed, as a transcript: the status, then each stream's
    /// text in the order written. The OS's socket buffer limits read the
    /// same on every OS.
    fn transcript(printed: &Printed) -> String {
        let status = printed.status;
        let mut text = format!(
            "ErrorStatus {}, {}: exit {}\n",
            status.error_status,
            if status.completed {
                "completed"
            } else {
                "not completed"
            },
            status.exit_code()
        );
        let mut current = None;
        for (stream, chunk) in &printed.output {
            if current != Some(*stream) {
                text.push_str(match stream {
                    Stream::Stdout => "--- stdout\n",
                    Stream::Stderr => "--- stderr\n",
                });
                current = Some(*stream);
            }
            text.push_str(chunk);
        }
        text.replace(SOCKET_BUFFER_LIMITS, "<the OS's socket buffer limits>")
    }

    // The Client color policy: result sections and table headings use
    // the informational palette while measurements keep their meaning.
    #[test]
    fn colored_result_sections_and_table_headings_leave_measurements_neutral() {
        let test = single("completed");
        let options = options(false, &[FIRST]);
        let results = TestResults::new(&test, &options, Some(at(0)), at(16_000));
        let output =
            results::print_colored_at(&results, 120, 120, Palette::colored(), Palette::colored());
        let stdout: String = output
            .iter()
            .filter(|(stream, _)| *stream == Stream::Stdout)
            .map(|(_, text)| text.as_str())
            .collect();
        assert!(
            stdout.contains("\x1b[38;2;112;120;140mEndpoints\x1b[0m"),
            "{stdout}"
        );
        assert!(
            stdout.contains("\x1b[1m\x1b[38;2;243;156;18mWarnings\x1b[0m"),
            "{stdout}"
        );
        assert!(
            stdout.contains("\x1b[38;2;112;120;140mSI\x1b[0m"),
            "{stdout}"
        );
        assert!(
            stdout
                .lines()
                .any(|line| line.contains("91.00") && !line.contains('\x1b')),
            "{stdout}"
        );
        assert!(
            !stdout.contains("\x1b[1m\x1b[38;2;90;247;142m91.00"),
            "{stdout}"
        );
        assert!(
            !stdout.contains("\x1b[38;2;92;184;255m192.0.2.1"),
            "{stdout}"
        );
    }

    fn clean_single() -> TestOutcome {
        let mut test = single("completed");
        for connection in &mut test.connections {
            let (events, times): (Vec<_>, Vec<_>) = connection
                .data_events
                .iter()
                .copied()
                .zip(connection.data_event_times.iter().copied())
                .filter(|(event, _)| !matches!(event, DataEvent::InvalidLoadPdus { .. }))
                .unzip();
            connection.data_events = events;
            connection.data_event_times = times;
        }
        test
    }

    // The CLI styling handover: a wide report consolidates Sub-Intervals,
    // narrow output groups metrics, and both retain one qualified headline.
    #[test]
    fn report_layout_keeps_one_capacity_headline_and_one_aggregate_outcome() {
        let test = clean_single();
        let options = options(false, &[FIRST]);
        let results = TestResults::new(&test, &options, Some(at(0)), at(16_000));
        for (width, tables) in [(120, 1), (60, 2)] {
            let output = results::print_colored_at(
                &results,
                width,
                width,
                Palette::colored(),
                Palette::colored(),
            );
            let text: String = output.into_iter().map(|(_, text)| text).collect();
            let (visible, _) = unstyled(&text);
            assert_eq!(
                visible
                    .lines()
                    .filter(|line| line.split_whitespace().next() == Some("SI"))
                    .count(),
                tables
            );
            assert_eq!(visible.matches("Maximum IP-Layer Capacity:").count(), 1);
            assert_eq!(visible.matches("Test outcome:").count(), 1);
            assert!(!visible.contains("Test completed"));
            assert!(visible.contains("95.00 Mbps"));
            assert!(visible.contains("0.166667"));
            assert!(
                visible
                    .lines()
                    .all(|line| crate::terminal::columns(line) <= width)
            );
        }
    }

    // The Client color policy styles existing typed outcomes and result
    // validity, including a completed partial Test with exit status zero
    // and an explicit abort whose initiating Test Connection completed.
    #[test]
    fn outcome_colors_distinguish_clean_qualified_partial_and_failed_tests() {
        let clean = clean_single();
        let mut invalid = clean.clone();
        for event in &mut invalid.connections[0].data_events {
            if let DataEvent::SubIntervalMeasured(measured) = event {
                measured.loss = 250;
            }
        }
        let mut partial = clean.clone();
        partial
            .connections
            .push(single("refused").connections.remove(0));
        let mut aborted = partial.clone();
        aborted
            .events
            .push(Event::TestAborted(TestAbort::TestConnectionEnded {
                mc_index: 0,
            }));
        let cases = [
            (
                "clean",
                clean,
                "\x1b[1m\x1b[38;2;90;247;142mcompleted\x1b[0m",
            ),
            (
                "warnings",
                single("completed"),
                "\x1b[1m\x1b[38;2;243;156;18mcompleted\x1b[0m",
            ),
            (
                "invalid",
                invalid,
                "\x1b[1m\x1b[38;2;243;156;18mcompleted\x1b[0m",
            ),
            (
                "partial",
                partial,
                "\x1b[1m\x1b[38;2;243;156;18mpartially\x1b[0m",
            ),
            (
                "explicit-abort",
                aborted,
                "\x1b[1m\x1b[38;2;255;92;87maborted\x1b[0m",
            ),
            (
                "rejected",
                single("refused"),
                "\x1b[1m\x1b[38;2;255;92;87mfailed\x1b[0m",
            ),
            (
                "timeout",
                single("timed-out"),
                "\x1b[1m\x1b[38;2;255;92;87mfailed\x1b[0m",
            ),
            (
                "interrupted",
                single("aborted"),
                "\x1b[1m\x1b[38;2;255;92;87minterrupted\x1b[0m",
            ),
        ];
        for (name, test, outcome) in cases {
            let hosts = vec![FIRST; test.connections.len()];
            let options = options(false, &hosts);
            let results = TestResults::new(&test, &options, Some(at(0)), at(16_000));
            let printed =
                results::print_colored_at(&results, 80, 80, Palette::colored(), Palette::colored());
            let text: String = printed.iter().map(|(_, text)| text.as_str()).collect();
            assert!(text.contains(outcome), "{name}: {text}");
            if name == "clean" {
                assert!(
                    text.contains("\x1b[1m\x1b[38;2;90;247;142mMbps\x1b[0m"),
                    "{text}"
                );
                assert!(text.contains("✔"), "{text}");
            } else {
                assert!(
                    !text.contains("\x1b[1m\x1b[38;2;90;247;142mMbps\x1b[0m"),
                    "{name}: {text}"
                );
            }
            if name == "partial" {
                assert_eq!(results.status.exit_code(), 0);
                let text: String = printed.iter().map(|(_, text)| text.as_str()).collect();
                assert!(
                    text.contains("\x1b[1m\x1b[38;2;90;247;142mCompleted\x1b[0m"),
                    "{text}"
                );
                assert!(
                    text.contains(
                        "\x1b[1m\x1b[38;2;255;92;87mSetupRejectedCapacityExceeded\x1b[0m"
                    ),
                    "{text}"
                );
            }
            if name == "explicit-abort" {
                assert!(
                    results.status.completed,
                    "the compatible status alone cannot style this abort"
                );
            }
        }
    }

    // The Client color policy: a warning explicitly attributed to one
    // Test Connection qualifies its own outcome, while group-only
    // qualifications do not change another connection's clean completion.
    #[test]
    fn attributed_whole_warnings_qualify_only_the_named_connection() {
        let mut test = clean_single();
        test.connections.push(test.connections[0].clone());
        test.connections[1].server = address(SECOND);
        let options = options(false, &[FIRST, SECOND]);
        for named in [Some(0), Some(1), None] {
            let mut results = TestResults::new(&test, &options, Some(at(0)), at(16_000));
            results
                .whole
                .as_mut()
                .unwrap()
                .warnings
                .push(Warning::CongestionNeverConfirmed {
                    shortfall: udpstp_core::SearchShortfall::TableTop,
                    direction: Direction::Downstream,
                    algorithm_c: false,
                    test_connection: named,
                });
            results
                .whole
                .as_mut()
                .unwrap()
                .warnings
                .push(Warning::NoValidMaximum);
            let output = results::print_colored_at(
                &results,
                120,
                120,
                Palette::colored(),
                Palette::colored(),
            );
            let text: String = output.iter().map(|(_, text)| text.as_str()).collect();
            for (index, server) in [FIRST, SECOND].into_iter().enumerate() {
                let color = if named == Some(index as u8) {
                    "38;2;243;156;18"
                } else {
                    "38;2;90;247;142"
                };
                assert!(
                    text.contains(&format!(
                        "Test Connection {index} to {server}: \x1b[1m\x1b[{color}mCompleted\x1b[0m"
                    )),
                    "{text}"
                );
            }
            let plain = results::print_at(&results, 120, 120);
            let plain: String = plain.iter().map(|(_, text)| text.as_str()).collect();
            assert_eq!(content(&unstyled(&text).0), content(&plain));
        }
    }

    /// Independently strip the small SGR palette, retaining every visible
    /// byte and collecting spans so values cannot gain styles unnoticed.
    fn unstyled(text: &str) -> (String, Vec<String>) {
        let mut output = String::new();
        let mut spans = Vec::new();
        let mut remaining = text;
        while let Some(start) = remaining.find('\x1b') {
            output.push_str(&remaining[..start]);
            remaining = &remaining[start..];
            let end = remaining.find('m').expect("a complete SGR sequence");
            let sequence = &remaining[..=end];
            assert!(
                [
                    "\x1b[0m",
                    "\x1b[1m",
                    "\x1b[38;2;112;120;140m",
                    "\x1b[38;2;255;92;87m",
                    "\x1b[38;2;90;247;142m",
                    "\x1b[38;2;243;156;18m",
                    "\x1b[38;2;92;184;255m",
                    "\x1b[38;2;189;147;249m"
                ]
                .contains(&sequence),
                "unexpected terminal control: {sequence:?}"
            );
            remaining = &remaining[end + 1..];
            if sequence != "\x1b[0m" {
                let next = remaining.find('\x1b').expect("a styled span resets");
                if next == 0 {
                    continue; // anstyle writes bold before its foreground color.
                }
                let span = &remaining[..next];
                assert!(
                    !span.chars().any(char::is_whitespace),
                    "a style crossed a layout break: {span:?}"
                );
                assert!(
                    remaining[next..].starts_with("\x1b[0m"),
                    "the span resets before another style"
                );
                spans.push(span.to_owned());
            }
        }
        output.push_str(remaining);
        (output, spans)
    }

    fn content(text: &str) -> Vec<&str> {
        text.split_whitespace()
            .filter(|word| !["✔", "⚠", "✗", "▶"].contains(word))
            .filter(|word| !word.chars().all(|ch| ch == '-' || ch == '─'))
            .collect()
    }

    // Visual fixture export at the existing rendering seam. The handover's
    // preview uses these exact bytes, with explicit wide and narrow widths.
    #[test]
    #[ignore = "writes visual fixtures to UDPSTP_PREVIEW_DIR"]
    fn export_terminal_preview() {
        use clap::CommandFactory as _;
        let directory = std::path::PathBuf::from(std::env::var_os("UDPSTP_PREVIEW_DIR").unwrap());
        std::fs::create_dir_all(&directory).unwrap();
        for (name, test, width) in [
            ("clean", clean_single(), 120),
            ("qualified", single("completed"), 120),
            ("narrow", clean_single(), 60),
        ] {
            let options = options(false, &[FIRST]);
            let results = TestResults::new(&test, &options, Some(at(0)), at(16_000));
            let text: String = results::print_colored_at(
                &results,
                width,
                width,
                Palette::colored(),
                Palette::colored(),
            )
            .into_iter()
            .filter(|(stream, _)| *stream == Stream::Stdout)
            .map(|(_, text)| text)
            .collect();
            std::fs::write(directory.join(format!("{name}.ansi")), text).unwrap();
        }
        let command = || {
            crate::Cli::command()
                .styles(Palette::colored().clap_styles())
                .term_width(120)
        };
        std::fs::write(
            directory.join("help.ansi"),
            command().render_help().ansi().to_string(),
        )
        .unwrap();
        let mut command = command();
        command.build();
        let client = command.find_subcommand_mut("client").unwrap();
        std::fs::write(
            directory.join("client-help.ansi"),
            client.render_help().ansi().to_string(),
        )
        .unwrap();
        let error = command
            .clone()
            .try_get_matches_from(["udpstp", "client", "--delay-var", "invalid"])
            .unwrap_err();
        std::fs::write(
            directory.join("error.ansi"),
            error.render().ansi().to_string(),
        )
        .unwrap();
    }

    // The approved rich-terminal presentation adds status symbols and
    // emphasis while preserving every reported word/value and destination.
    #[test]
    fn rich_and_plain_ending_fixtures_preserve_content_and_streams() {
        for ending in endings() {
            for (test, options) in [single_ending(&ending, false), multi_ending(&ending, false)] {
                let results = TestResults::new(&test, &options, Some(at(0)), at(16_000));
                for width in [60, 80, 100, 120] {
                    let plain = results::print_at(&results, width, width);
                    let colored = results::print_colored_at(
                        &results,
                        width,
                        width,
                        Palette::colored(),
                        Palette::colored(),
                    );
                    assert_eq!(plain.len(), colored.len());
                    for ((stream, plain), (colored_stream, colored)) in plain.iter().zip(&colored) {
                        let (visible, _) = unstyled(colored);
                        assert_eq!(stream, colored_stream);
                        assert_eq!(
                            content(plain),
                            content(&visible),
                            "{} at {width}",
                            ending.name
                        );
                    }
                }
            }
        }
    }

    // ADR 0013: forced human styles never change JSON bytes, message
    // wording, exit status or the stream of any emitted notice.
    #[test]
    fn colored_json_ending_fixtures_preserve_json_bytes_and_streams() {
        for ending in endings() {
            for (test, options) in [single_ending(&ending, true), multi_ending(&ending, true)] {
                let plain = print(&test, &options, Some(at(0)), at(16_000));
                let colored = print_with_color(
                    &test,
                    &options,
                    Some(at(0)),
                    at(16_000),
                    Palette::colored(),
                    Palette::colored(),
                );
                assert_eq!(plain.status, colored.status);
                assert_eq!(plain.output.len(), colored.output.len());
                for ((stream, plain), (colored_stream, colored)) in
                    plain.output.iter().zip(&colored.output)
                {
                    assert_eq!(stream, colored_stream);
                    if *stream == Stream::Stdout {
                        assert_eq!(plain, colored, "{}", ending.name);
                        assert!(!colored.contains('\x1b'));
                    } else {
                        assert_eq!(
                            content(plain),
                            content(&unstyled(colored).0),
                            "{}",
                            ending.name
                        );
                    }
                }
            }
        }
    }

    // The Client color policy: an oversized diagnostic identifier remains
    // neutral, intact and on the same continuation line as plain output.
    #[test]
    fn colored_diagnostics_keep_oversized_identifiers_copyable() {
        let identifier = "[2001:db8:1234:5678:abcd:eeee:ffff:9999%long-interface-name]:24601";
        let test = TestOutcome {
            mc_ident: 45366,
            events: vec![],
            connections: vec![connection(
                FIRST,
                vec![],
                ClientResult::SocketError(format!("cannot use {identifier} for the Test")),
            )],
        };
        let options = options(false, &[FIRST]);
        let results = TestResults::new(&test, &options, None, at(3_000));
        for width in [60, 80, 100, 120] {
            let plain = results::print_at(&results, width, width);
            let colored = results::print_colored_at(
                &results,
                width,
                width,
                Palette::colored(),
                Palette::colored(),
            );
            for ((stream, plain), (colored_stream, colored)) in plain.iter().zip(&colored) {
                let (visible, spans) = unstyled(colored);
                assert_eq!(stream, colored_stream);
                assert_eq!(content(plain), content(&visible));
                assert!(spans.iter().all(|span| !span.contains("2001:db8")));
                if *stream == Stream::Stderr {
                    assert!(colored.contains(identifier), "{width}: {colored}");
                    if width == 60 {
                        assert!(
                            colored.lines().any(|line| line.trim() == identifier),
                            "{colored}"
                        );
                    }
                }
            }
        }
    }

    // RFC 9097 reporting context: endpoint identities use the same
    // privacy rules as JSON. Wildcard bindings identify no Client.
    #[udpstp_req::req("RFC9097-9-1")]
    #[test]
    fn human_context_preserves_private_identities_and_received_times() {
        struct KeyFile(std::path::PathBuf);
        impl Drop for KeyFile {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let key = KeyFile(
            std::env::temp_dir().join(format!("udpstp-human-privacy-{}.key", std::process::id())),
        );
        std::fs::write(&key.0, "00112233445566778899aabbccddeeff").unwrap();
        for privacy in [
            privacy::AddressPrivacy::Full,
            privacy::AddressPrivacy::Truncate,
            privacy::AddressPrivacy::Pseudonymize,
        ] {
            for local in [
                None,
                Some(address("0.0.0.0:40000")),
                Some(address("[::]:40000")),
                Some(address("192.0.2.77:40000")),
                Some(address("[2001:db8:1234:5678::99]:40000")),
            ] {
                let mut test = multi("completed");
                test.connections[0].local = local;
                test.connections[1].local = Some(address("198.51.100.88:40001"));
                let mut options = options(false, &[FIRST, SECOND]);
                options.addresses = privacy::Addresses::new(
                    privacy,
                    (privacy == privacy::AddressPrivacy::Pseudonymize).then_some(key.0.as_path()),
                )
                .unwrap();
                let expected = local
                    .filter(|a| !a.ip().is_unspecified())
                    .map_or_else(|| "unknown".into(), |a| options.addresses.show_ip(a.ip()));
                let second = options
                    .addresses
                    .show_ip(address("198.51.100.88:40001").ip());
                let results = TestResults::new(&test, &options, Some(at(0)), at(16_000));
                for width in [60, 80, 100, 120] {
                    let output = results::print_at(&results, width, width);
                    let text = output
                        .iter()
                        .filter(|(stream, _)| *stream == Stream::Stdout)
                        .map(|(_, text)| text.as_str())
                        .collect::<String>();
                    let prose = text.split_whitespace().collect::<Vec<_>>().join(" ");
                    assert!(
                        prose.contains(&format!(
                            "Test Connection 0: Client {expected}; Server {FIRST}"
                        )),
                        "{prose}"
                    );
                    assert!(
                        prose.contains(&format!(
                            "Test Connection 1: Client {second}; Server {SECOND}"
                        )),
                        "{prose}"
                    );
                    assert!(
                        prose.contains(
                            "Maximum Sub-Interval result received at 2026-09-21T14:13:25.023000000Z"
                        ),
                        "the aggregate uses the latest first report: {prose}"
                    );
                    assert!(
                        prose.contains(
                            "Maximum Sub-Interval result received at 2026-09-21T14:13:25.017000000Z"
                        ),
                        "the first Test Connection's own report: {prose}"
                    );
                    assert!(
                        !prose.contains("2026-09-21T14:13:36Z"),
                        "the Test end is never a fallback: {prose}"
                    );
                    assert!(!prose.contains("00112233445566778899aabbccddeeff"));
                    if privacy != privacy::AddressPrivacy::Full {
                        assert!(
                            !prose.contains("198.51.100.88")
                                && !prose.contains("192.0.2.77")
                                && !prose.contains("2001:db8:1234:5678::99"),
                            "{prose}"
                        );
                    }
                    assert!(
                        text.lines()
                            .all(|line| crate::terminal::columns(line) <= width),
                        "{text}"
                    );
                }
                for connection in &mut test.connections {
                    connection.data_event_times.clear();
                }
                let results = TestResults::new(&test, &options, Some(at(0)), at(16_000));
                let output = results::print_at(&results, 60, 60);
                let text = output
                    .iter()
                    .map(|(_, text)| text.as_str())
                    .collect::<String>();
                assert!(
                    text.split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                        .contains("Maximum Sub-Interval result received at an unknown time"),
                    "{text}"
                );
            }
        }
    }

    /// Standard output, as JSON.
    fn stdout_json(printed: &Printed) -> serde_json::Value {
        let stdout: String = printed
            .output
            .iter()
            .filter(|(stream, _)| *stream == Stream::Stdout)
            .map(|(_, text)| text.as_str())
            .collect();
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("{e}: {stdout}"))
    }

    // An interrupted Test keeps every measured Sub-Interval in the extension,
    // including loss and delay figures (conformance: json-incomplete-test-results-in-extension).
    #[test]
    fn an_interrupted_tests_json_keeps_its_measured_sub_intervals() {
        let mut test = single("aborted");
        for event in &mut test.connections[0].data_events {
            if let DataEvent::SubIntervalMeasured(m) = event {
                m.rtt_minimum = Some(17);
                if m.sub_int_seq_no == 2 {
                    m.loss = 10;
                    m.out_of_order = 2;
                    m.duplicates = 1;
                    m.rtt_minimum = Some(13);
                }
            }
        }
        let printed = print(&test, &options(true, &[FIRST]), Some(at(0)), at(16_000));
        let json = stdout_json(&printed);
        assert_eq!(json["ErrorStatus"], 204);
        assert_eq!(json["udpstp"]["EndReason"], "Stopped");
        assert!(json["Output"].get("IncrementalResult").is_none());
        assert!(json["Output"].get("Summary").is_none());
        let intervals = json["udpstp"]["SubIntervals"]
            .as_array()
            .expect("the measured Sub-Intervals");
        assert_eq!(intervals.len(), 2);
        assert_eq!(intervals[0]["Interval"], 1);
        assert_eq!(
            intervals[1],
            serde_json::json!({
                "Interval": 2,
                "Seconds": 2.0,
                "TimeOfSubInterval": "2026-09-21T14:13:22.017000Z",
                "ActiveConnections": 1,
                "DeliveredPercent": 99.01,
                "LossRatio": 0.009900990,
                "ReorderedRatio": 0.001980198,
                "ReplicatedRatio": 0.000990099,
                "CEPercentOfDelivered": 0.0,
                "LossCount": 10,
                "ReorderedCount": 2,
                "ReplicatedCount": 1,
                "CECountOfDelivered": 0,
                "PDVMin": 0.0,
                "PDVAvg": 0.007,
                "PDVMax": 0.012,
                "PDVRange": 0.012,
                "RTTMin": 0.0,
                "RTTAvg": 0.008,
                "RTTMax": 0.012,
                "RTTRange": 0.012,
                "IPLayerCapacity": 92.0,
                "InterfaceEthMbps": 0.0,
                "MinOnewayDelay": 0.0
            })
        );
        assert_eq!(json["udpstp"]["MinRTT"], 0.013);
        assert_eq!(
            json["udpstp"]["MissingSubIntervals"],
            serde_json::json!([3, 4, 5])
        );
        assert!(codes(&printed).iter().any(|code| code == "Interrupted"));
    }

    // Missing Sub-Intervals and unavailable samples stay explicit in an
    // incomplete Test (conformance: json-incomplete-test-results-in-extension).
    #[test]
    fn an_incomplete_tests_json_keeps_missing_measurements_explicit() {
        let mut test = single("aborted");
        for event in &mut test.connections[0].data_events {
            if let DataEvent::SubIntervalMeasured(m) = event {
                if m.sub_int_seq_no == 2 {
                    m.sub_int_seq_no = 3;
                }
                m.delay_variation = None;
                m.rtt_variation = None;
                m.rtt_variation_mean = None;
                m.rtt_minimum = None;
            }
        }
        let mut options = options(true, &[FIRST]);
        options.first_mode = Some(2);
        let render = |test: &TestOutcome, options: &ResultOptions| {
            stdout_json(&print(test, options, Some(at(0)), at(16_000)))
        };
        let json = render(&test, &options);
        let intervals = json["udpstp"]["SubIntervals"].as_array().unwrap();
        assert_eq!(intervals.len(), 2);
        assert_eq!(intervals[0]["Interval"], 1);
        assert_eq!(intervals[1]["Interval"], 3);
        assert_eq!(intervals[1]["PDVAvg"], 0.0);
        assert_eq!(intervals[1]["RTTAvg"], 0.0);
        assert!(json["udpstp"].get("MinRTT").unwrap().is_null());
        assert_eq!(
            json["udpstp"]["MissingSubIntervals"],
            serde_json::json!([2, 4, 5])
        );
        assert_eq!(json["udpstp"]["NoSamples"], serde_json::json!([1, 3]));

        test.connections[0]
            .data_events
            .retain(|e| !matches!(e, DataEvent::SubIntervalMeasured(_)));
        let json = render(&test, &options);
        assert_eq!(json["udpstp"]["SubIntervals"], serde_json::json!([]));
        assert!(json["udpstp"].get("MinRTT").unwrap().is_null());
        options.json_extension = false;
        let json = render(&test, &options);
        assert!(json.get("udpstp").is_none());
        assert!(json["Output"].get("IncrementalResult").is_none());
    }

    /// The warning codes the JSON lists.
    fn codes(printed: &Printed) -> Vec<String> {
        stdout_json(printed)["udpstp"]["Warnings"]
            .as_array()
            .map(|warnings| {
                warnings
                    .iter()
                    .filter_map(|w| w["Code"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }

    // : a Downstream Fixed-Rate Test from a Server that skipped or
    // withheld Bursts, whose Load PDUs take no lpduSeqNo, so that what the
    // Client estimates it sent falls more than 5% short of its row's rate
    // where the result was measured, warns that the Server may not have kept
    // up; one that kept its rate doesn't, nor does a Search Test, whose
    // sending rate the Client can't know, nor one a Test Connection of which
    // didn't complete, since it sent less for another reason, whether alone
    // or with another that did
    #[test]
    fn a_server_that_did_not_keep_its_sending_rate_is_warned_of() {
        use EndReason::{Completed, Watchdog};
        // Row 90: 90 Mbps.
        let row_90 = TestParameters {
            sr_index_conf: 90,
            ..parameters()
        };
        // Each Test Connection's row, the Mbps its Server sent, and how its
        // Data Phase ended: the Watchdog's 2 s after nothing came for 1 s,
        // after 3 Sub-Intervals.
        type Sent = Vec<(u16, u64, EndReason)>;
        let cases: Vec<(&str, bool, Sent, bool)> = vec![
            ("kept its rate", true, vec![(90, 90, Completed)], false),
            ("within the margin", true, vec![(90, 86, Completed)], false),
            ("skipped a fifth", true, vec![(90, 72, Completed)], true),
            ("a Search Test", false, vec![(90, 72, Completed)], false),
            (
                "two Test Connections, kept",
                true,
                vec![(45, 45, Completed), (45, 45, Completed)],
                false,
            ),
            (
                "two Test Connections, one fell behind",
                true,
                vec![(45, 45, Completed), (45, 20, Completed)],
                true,
            ),
            (
                "the Watchdog ended it",
                true,
                vec![(90, 72, Watchdog)],
                false,
            ),
            (
                "two Test Connections, the Watchdog ended one",
                true,
                vec![(45, 45, Completed), (45, 20, Watchdog)],
                false,
            ),
        ];
        for (what, fixed_rate, rows, warned) in cases {
            let hosts = [FIRST, SECOND];
            let hosts = &hosts[..rows.len()];
            let connections = rows
                .iter()
                .zip(hosts)
                .zip([17, 23])
                .map(|(((row, sent_mbps, end), host), offset)| {
                    let data = match end {
                        Watchdog => {
                            let mut data = downstream(3, *sent_mbps, offset, Watchdog);
                            let end = data.pop().expect("an end");
                            data.push((
                                DataEvent::TrafficStopped {
                                    silent_since: MonoTime::from_duration(Duration::from_millis(
                                        2_000 + offset,
                                    )),
                                },
                                at(3_000 + offset),
                            ));
                            data.push(end);
                            data
                        }
                        _ => downstream(5, *sent_mbps, offset, *end),
                    };
                    activated(
                        host,
                        51646,
                        TestParameters {
                            sr_index_conf: *row,
                            ..row_90
                        },
                        data,
                        *end,
                    )
                })
                .collect();
            let test = TestOutcome {
                mc_ident: 45366,
                events: vec![],
                connections,
            };
            // One Test Connection may fail, so the Test goes on.
            let options = ResultOptions {
                fixed_rate,
                minimum: 1,
                ..options(true, hosts)
            };
            let printed = print(&test, &options, Some(at(0)), at(16_000));
            assert_eq!(
                codes(&printed).iter().any(|c| c == "SendingRateNotKept"),
                warned,
                "{what}: {:?}",
                codes(&printed)
            );
            // A warning, not an error: the Test completed if one of its
            // Test Connections did.
            let completed = rows.iter().any(|(_, _, end)| *end == Completed);
            assert_eq!(printed.status.completed, completed, "{what}");
        }
        // The text says it too, with the estimate and the rate.
        let test = TestOutcome {
            mc_ident: 45366,
            events: vec![],
            connections: vec![activated(
                FIRST,
                51646,
                row_90,
                downstream(5, 72, 17, EndReason::Completed),
                EndReason::Completed,
            )],
        };
        let options = ResultOptions {
            fixed_rate: true,
            ..options(false, &[FIRST])
        };
        let said: String = print(&test, &options, Some(at(0)), at(16_000))
            .output
            .iter()
            .map(|(_, text)| text.as_str())
            .collect();
        let said = said.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            said.contains("The Server sent about 72.00 Mbps")
                && said.contains("the 90.00 Mbps of its Fixed-Rate row"),
            "{said}"
        );
    }

    /// A Downstream Data Phase through a drop-tail queue, 17 ms into
    /// each Sub-Interval: the search's climb meets the Performance
    /// Criterion up to 60 Mbps in Sub-Interval 2, and Sub-Intervals 3-5,
    /// cycling around the bottleneck at 94-96 Mbps, lose 2-4% of their
    /// Load PDUs.
    fn cliff() -> Vec<(DataEvent, Duration)> {
        let mut events = vec![(DataEvent::FirstLoadPduReceived, at(17))];
        for (seq, (mbps, loss)) in (1..).zip([(30, 0), (60, 0), (94, 30), (96, 40), (95, 20)]) {
            events.push((
                DataEvent::SubIntervalMeasured(measurement(seq, mbps, loss)),
                at(u64::from(seq) * 1000 + 17),
            ));
        }
        events.push((DataEvent::Ended(EndReason::Completed), at(5_617)));
        events
    }

    /// A Downstream Data Phase through a queue that kept dropping after
    /// it overflowed, as CAKE does with one Test Connection, 17 ms
    /// into each Sub-Interval: the search's climb meets the Performance
    /// Criterion up to 60 Mbps in Sub-Interval 2, its overshoot to
    /// `overshoot` Mbps in Sub-Interval 3 loses 8%, and Sub-Intervals 4
    /// and 5 run at under half the Maximum, still losing 8%.
    fn kept_dropping(overshoot: u64) -> Vec<(DataEvent, Duration)> {
        let mut events = vec![(DataEvent::FirstLoadPduReceived, at(17))];
        for (seq, (mbps, loss)) in
            (1..).zip([(40, 0), (60, 0), (overshoot, 87), (28, 90), (27, 90)])
        {
            events.push((
                DataEvent::SubIntervalMeasured(measurement(seq, mbps, loss)),
                at(u64::from(seq) * 1000 + 17),
            ));
        }
        events.push((DataEvent::Ended(EndReason::Completed), at(5_617)));
        events
    }

    // : a Search Test whose Maximum IP-Layer Capacity came before
    // Sub-Intervals that all missed the Performance Criterion, one of them
    // more than the margin faster, warns with what that one measured, per
    // Capacity Mode; a Fixed-Rate Test, whose rate doesn't cycle, doesn't,
    // nor does one within a wider margin
    #[test]
    fn a_maximum_before_faster_sub_intervals_that_missed_the_criterion_is_warned_of() {
        let test = TestOutcome {
            mc_ident: 45366,
            events: vec![],
            connections: vec![activated(
                FIRST,
                51646,
                parameters(),
                cliff(),
                EndReason::Completed,
            )],
        };
        let warned = |options: &ResultOptions| {
            codes(&print(&test, options, Some(at(0)), at(16_000)))
                .iter()
                .any(|c| c == "SubIntervalsAfterMaximumMissed")
        };
        assert!(warned(&options(true, &[FIRST])));
        assert!(!warned(&ResultOptions {
            fixed_rate: true,
            ..options(true, &[FIRST])
        }));
        // Nor one that ended otherwise, which lost for another reason.
        let watchdog = TestOutcome {
            connections: vec![activated(
                FIRST,
                51646,
                parameters(),
                cliff(),
                EndReason::Watchdog,
            )],
            ..test.clone()
        };
        assert!(
            !codes(&print(
                &watchdog,
                &options(true, &[FIRST]),
                Some(at(0)),
                at(16_000)
            ))
            .iter()
            .any(|c| c == "SubIntervalsAfterMaximumMissed")
        );
        // 96 of 60 Mbps is 60% faster.
        assert!(!warned(&ResultOptions {
            faster_sub_interval_margin: 60,
            ..options(true, &[FIRST])
        }));
        // The text names the fastest, its Loss Ratio and the criterion,
        // and the Capacity Mode when there are two.
        let said = |options: &ResultOptions| -> String {
            print(&test, options, Some(at(0)), at(16_000))
                .output
                .iter()
                .map(|(_, text)| text.as_str())
                .collect::<String>()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        };
        let one_mode = said(&options(false, &[FIRST]));
        assert!(
            one_mode.contains("Sub-Interval 4, after the Maximum's, reached 96.00 Mbps, 60.0% above the Maximum IP-Layer Capacity of 60.00 Mbps, with a Loss Ratio of 0.038462")
                && one_mode.contains("(--max-loss-ratio 0.01)")
                && !one_mode.contains("Capacity Mode"),
            "{one_mode}"
        );
        let two_modes = said(&ResultOptions {
            first_mode: Some(1),
            ..options(false, &[FIRST])
        });
        assert!(
            two_modes.contains("In Capacity Mode 2, Sub-Interval 4, after the Maximum's, reached"),
            "{two_modes}"
        );
    }

    /// `data` with its Client's count of its Trial Intervals just before
    /// its end, as a Data Phase reports it.
    // : a Search Test whose last Sub-Interval ran under 60% of the
    // Maximum IP-Layer Capacity, after Sub-Intervals that all missed the
    // Performance Criterion, warns with what it measured, naming a Server
    // that fell behind among the causes Downstream; with a faster one as
    // well, in one warning. Upstream, a host that left more than 1% of its
    // Load PDUs unsent isn't judged on the slow one: its Skipped Bursts
    // say so
    #[test]
    fn a_slow_last_sub_interval_after_the_maximum_is_warned_of() {
        let one = |data, upstream_test: bool| {
            let connection = activated(FIRST, 51646, parameters(), data, EndReason::Completed);
            TestOutcome {
                mc_ident: 45366,
                events: vec![],
                connections: vec![if upstream_test {
                    upstream(connection)
                } else {
                    connection
                }],
            }
        };
        let said = |test: &TestOutcome, options: fn(bool) -> ResultOptions| {
            let json = print(test, &options(true), Some(at(0)), at(16_000));
            let text: String = print(test, &options(false), Some(at(0)), at(16_000))
                .output
                .iter()
                .map(|(_, text)| text.as_str())
                .collect();
            (
                codes(&json),
                text.split_whitespace().collect::<Vec<_>>().join(" "),
            )
        };
        let code = "SubIntervalsAfterMaximumMissed".to_string();
        let downstream_options: fn(bool) -> ResultOptions = |json| options(json, &[FIRST]);
        let (codes_down, text) = said(&one(kept_dropping(61), false), downstream_options);
        assert_eq!(
            codes_down.iter().filter(|c| **c == code).count(),
            1,
            "{codes_down:?}"
        );
        assert!(
            text.contains("Sub-Interval 5, the last, ran at 27.00 Mbps, 45.0% of the Maximum IP-Layer Capacity of 60.00 Mbps, with a Loss Ratio of 0.082569, and no Sub-Interval after the Maximum's met the Performance Criterion (--max-loss-ratio 0.01): a queue may have kept dropping after it overflowed (such as CAKE with one Test Connection), the path may have slowed during the Test, or the Server may have fallen behind, so the Maximum may be well below the path, or no longer what it carries"),
            "{text}"
        );
        // Both at once: one warning that says both.
        let (codes_both, both) = said(&one(kept_dropping(70), false), downstream_options);
        assert_eq!(codes_both.iter().filter(|c| **c == code).count(), 1);
        assert!(
            both.contains("Sub-Interval 3, after the Maximum's, reached 70.00 Mbps, 16.7% above the Maximum IP-Layer Capacity of 60.00 Mbps, with a Loss Ratio of 0.080037; Sub-Interval 5, the last, ran at 27.00 Mbps, 45.0% of the Maximum, with a Loss Ratio of 0.082569, and no Sub-Interval after the Maximum's met the Performance Criterion (--max-loss-ratio 0.01): the search may have cycled around a bottleneck whose queue drops more than that, a queue may have kept dropping after it overflowed"),
            "{both}"
        );
        // Upstream: this host sent 5000 Load PDUs; judged when it left 50
        // unsent (1%), not when it left 51, and never naming the Server.
        let upstream_options: fn(bool) -> ResultOptions = |json| ResultOptions {
            direction: Direction::Upstream,
            ..options(json, &[FIRST])
        };
        let sent_with = |unsent: u32| one(sent_leaving(kept_dropping(61), unsent), true);
        let (kept_up, text) = said(&sent_with(50), upstream_options);
        assert!(kept_up.contains(&code), "{kept_up:?}");
        assert!(
            text.contains("or the path may have slowed during the Test, so the Maximum")
                && !text.contains("Server may have fallen behind"),
            "{text}"
        );
        let (fell_behind, _) = said(&sent_with(51), upstream_options);
        assert!(
            !fell_behind.contains(&code) && fell_behind.contains(&"BurstsSkipped".to_string()),
            "{fell_behind:?}"
        );
    }

    /// `data` from an Upstream Test's Client that sent 5000 Load PDUs and
    /// left `unsent` in Skipped Bursts.
    fn sent_leaving(
        mut data: Vec<(DataEvent, Duration)>,
        unsent: u32,
    ) -> Vec<(DataEvent, Duration)> {
        for st_seq_no in 1..=100 {
            data.push((
                DataEvent::Sent(SentInterval {
                    st_seq_no,
                    datagrams: 50,
                    bytes: 50_000,
                    ip_layer_sender_bit_rate: 8_000_000,
                    duration: Duration::from_millis(50),
                }),
                at(u64::from(st_seq_no) * 50),
            ));
        }
        data.push((
            DataEvent::BurstsSkipped {
                bursts: 1,
                datagrams: unsent,
                bytes: u64::from(unsent) * 1000,
            },
            at(3_000),
        ));
        data.sort_by_key(|(_, time)| *time);
        data
    }

    /// A Downstream Data Phase whose search climbed back slowly after its
    /// overshoot, as CAKE gives with two Test Connections at 2.5 Gbit/s
    ///, 17 ms into each Sub-Interval: the climb meets the
    /// Performance Criterion up to 50 Mbps in Sub-Interval 2, the overshoot
    /// to 70 Mbps in Sub-Interval 3 loses 8%, Sub-Interval 4 at 52 Mbps
    /// still loses 3%, and Sub-Interval 5, the last, meets it at 55 Mbps.
    fn slow_climb_back() -> Vec<(DataEvent, Duration)> {
        let mut events = vec![(DataEvent::FirstLoadPduReceived, at(17))];
        for (seq, (mbps, loss)) in (1..).zip([(30, 0), (50, 0), (70, 87), (52, 30), (55, 0)]) {
            events.push((
                DataEvent::SubIntervalMeasured(measurement(seq, mbps, loss)),
                at(u64::from(seq) * 1000 + 17),
            ));
        }
        events.push((DataEvent::Ended(EndReason::Completed), at(5_617)));
        events
    }

    // : a Search Test whose Maximum IP-Layer Capacity came after a
    // Sub-Interval that missed the Performance Criterion more than the
    // margin faster, with none after it that met the criterion, warns with
    // what that one measured, naming a Server that fell behind among the
    // causes Downstream. Upstream, a host that left more than 1% of its
    // Load PDUs unsent isn't judged on it: its Skipped Bursts say so
    #[test]
    fn a_faster_sub_interval_before_the_maximum_is_warned_of() {
        let one = |data, upstream_test: bool| {
            let connection = activated(FIRST, 51646, parameters(), data, EndReason::Completed);
            TestOutcome {
                mc_ident: 45366,
                events: vec![],
                connections: vec![if upstream_test {
                    upstream(connection)
                } else {
                    connection
                }],
            }
        };
        let said = |test: &TestOutcome, options: fn(bool) -> ResultOptions| {
            let json = print(test, &options(true), Some(at(0)), at(16_000));
            let text: String = print(test, &options(false), Some(at(0)), at(16_000))
                .output
                .iter()
                .map(|(_, text)| text.as_str())
                .collect();
            (
                codes(&json),
                text.split_whitespace().collect::<Vec<_>>().join(" "),
            )
        };
        let code = "SubIntervalsAfterMaximumMissed".to_string();
        let downstream_options: fn(bool) -> ResultOptions = |json| options(json, &[FIRST]);
        let (codes_down, text) = said(&one(slow_climb_back(), false), downstream_options);
        assert_eq!(
            codes_down.iter().filter(|c| **c == code).count(),
            1,
            "{codes_down:?}"
        );
        assert!(
            text.contains("Sub-Interval 3, before the Maximum's, reached 70.00 Mbps, 27.3% above the Maximum IP-Layer Capacity of 55.00 Mbps, with a Loss Ratio of 0.080037, and no Sub-Interval after the Maximum's met the Performance Criterion (--max-loss-ratio 0.01): the search may have climbed back slowly after an overshoot, the path may have slowed during the Test, a policer or shaper may have let a burst through at first, or the Server may have fallen behind, so the Maximum may be well below the path"),
            "{text}"
        );
        // Upstream: judged when this host left 50 of 5000 unsent (1%), not
        // when it left 51, and never naming the Server.
        let upstream_options: fn(bool) -> ResultOptions = |json| ResultOptions {
            direction: Direction::Upstream,
            ..options(json, &[FIRST])
        };
        let sent_with = |unsent: u32| one(sent_leaving(slow_climb_back(), unsent), true);
        let (kept_up, text) = said(&sent_with(50), upstream_options);
        assert!(kept_up.contains(&code), "{kept_up:?}");
        assert!(
            text.contains(
                "or a policer or shaper may have let a burst through at first, so the Maximum"
            ) && !text.contains("Server may have fallen behind"),
            "{text}"
        );
        let (fell_behind, _) = said(&sent_with(51), upstream_options);
        assert!(
            !fell_behind.contains(&code) && fell_behind.contains(&"BurstsSkipped".to_string()),
            "{fell_behind:?}"
        );
    }

    fn counted(
        mut data: Vec<(DataEvent, Duration)>,
        counts: TrialIntervalCounts,
    ) -> Vec<(DataEvent, Duration)> {
        let (end, at) = data.pop().expect("an end");
        data.push((DataEvent::TrialIntervalsCounted(counts), at));
        data.push((end, at));
        data
    }

    /// `data` with the st of each Sub-Interval estimated at its rate of
    /// `mbps`, in order.
    fn with_rates(
        mut data: Vec<(DataEvent, Duration)>,
        mbps: &[u64],
    ) -> Vec<(DataEvent, Duration)> {
        for (event, _) in &mut data {
            if let DataEvent::SentEstimated(sent) = event {
                let rate = mbps[((sent.st_seq_no - 1) / 20) as usize];
                sent.bytes = rate * 1_000_000 / 8 / 20 - 50 * 28;
                sent.ip_layer_sender_bit_rate = rate * 1_000_000;
            }
        }
        data
    }

    /// Trial Intervals as a Client counts them: `clean`, `errored` and
    /// `hold` of each, every Hold Interval with a delay sample, and whether
    /// Congestion was confirmed; none of the last ones errored.
    fn trial_intervals(
        clean: u32,
        errored: u32,
        hold: u32,
        confirmed: bool,
    ) -> TrialIntervalCounts {
        TrialIntervalCounts {
            clean,
            errored,
            hold,
            hold_without_sample: 0,
            congestion_confirmed: confirmed,
            last_errored: 0,
            last_from_first_errored: 0,
        }
    }

    // : a completed Search Test none of whose searches confirmed
    // Congestion, as the Client counted its Trial Intervals, warns that the
    // result may understate the path, saying why, in text and JSON under
    // the code CongestionNeverConfirmed; in a Multi-Connection Test each
    // Test Connection's warning names it, as 's do. One that confirmed
    // it doesn't, nor a Fixed-Rate Test, nor one that ended otherwise, nor
    // one whose Trial Intervals weren't counted
    #[test]
    fn a_search_that_never_confirmed_congestion_is_warned_of() {
        let clean = trial_intervals(109, 0, 0, false);
        let confirmed = trial_intervals(62, 46, 1, true);
        let held = trial_intervals(3, 0, 106, false);
        let flat = |counts| counted(downstream(5, 90, 17, EndReason::Completed), counts);
        let rising = with_rates(flat(clean), &[83, 283, 483, 683, 883]);
        let one = |data, parameters| TestOutcome {
            mc_ident: 45366,
            events: vec![],
            connections: vec![activated(
                FIRST,
                51646,
                parameters,
                data,
                EndReason::Completed,
            )],
        };
        let algorithm_c = TestParameters {
            rate_adj_algo: 1,
            ..parameters()
        };
        let watchdog = TestOutcome {
            mc_ident: 45366,
            events: vec![],
            connections: vec![activated(
                FIRST,
                51646,
                parameters(),
                counted(downstream(3, 90, 17, EndReason::Watchdog), clean),
                EndReason::Watchdog,
            )],
        };
        let up = TestOutcome {
            mc_ident: 45366,
            events: vec![],
            connections: vec![upstream(activated(
                FIRST,
                51646,
                parameters(),
                counted(upstream_data(5, 60, 17, EndReason::Completed), clean),
                EndReason::Completed,
            ))],
        };
        // Two Test Connections at 60 and 30 Mbps.
        let two = |second| TestOutcome {
            mc_ident: 45366,
            events: vec![],
            connections: vec![
                activated(
                    FIRST,
                    51646,
                    parameters(),
                    counted(downstream(5, 60, 17, EndReason::Completed), clean),
                    EndReason::Completed,
                ),
                activated(
                    SECOND,
                    51647,
                    parameters(),
                    counted(downstream(5, 30, 23, EndReason::Completed), second),
                    EndReason::Completed,
                ),
            ],
        };
        let single = |change: fn(&mut ResultOptions)| {
            move |json| {
                let mut options = options(json, &[FIRST]);
                change(&mut options);
                options
            }
        };
        let pair = |change: fn(&mut ResultOptions)| {
            move |json| {
                let mut options = options(json, &[FIRST, SECOND]);
                change(&mut options);
                options
            }
        };
        let search: fn(&mut ResultOptions) = |_| {};
        // The Test, its options, and what each of its warnings says.
        type Case = (
            &'static str,
            TestOutcome,
            Box<dyn Fn(bool) -> ResultOptions>,
            Vec<&'static str>,
        );
        let cases: Vec<Case> = vec![
            (
                "a Server that stopped rising",
                one(flat(clean), parameters()),
                Box::new(single(search)),
                vec![
                    "The search never confirmed Congestion: the Server's IP-Layer Sender Bit Rate, as estimated here, stopped rising though few of the last 64 Trial Intervals were Errored Intervals: it may not have kept up, so the result may understate the path",
                ],
            ),
            (
                "confirmed",
                one(flat(confirmed), parameters()),
                Box::new(single(search)),
                vec![],
            ),
            (
                "a Fixed-Rate Test",
                one(flat(clean), parameters()),
                Box::new(single(|o| o.fixed_rate = true)),
                vec![],
            ),
            (
                "the Watchdog ended it",
                watchdog,
                Box::new(single(search)),
                vec![],
            ),
            (
                "not counted",
                one(downstream(5, 90, 17, EndReason::Completed), parameters()),
                Box::new(single(search)),
                vec![],
            ),
            (
                "the maxBandwidth",
                one(flat(clean), parameters()),
                Box::new(single(|o| o.max_bandwidth = 90)),
                vec![
                    "The search never confirmed Congestion: the IP-Layer Sender Bit Rate reached the 90 Mbps maxBandwidth this Client stated (--max-bandwidth), at which a Server may stop its search, so the result may understate the path",
                ],
            ),
            (
                "Hold Intervals",
                one(flat(held), parameters()),
                Box::new(single(search)),
                vec![
                    "The search never confirmed Congestion: most Trial Intervals were Hold Intervals, delay variation between lowThresh and upperThresh holding the sending rate, so the result may understate the path",
                ],
            ),
            (
                "still rising",
                one(rising.clone(), parameters()),
                Box::new(single(search)),
                vec![
                    "The search never confirmed Congestion: the IP-Layer Sender Bit Rate was still rising in the last Sub-Interval: the Test ended too early, so the result may understate the path; a longer --test-interval helps, or --algorithm c, which rises faster",
                ],
            ),
            (
                "still rising with algorithm C",
                one(rising, algorithm_c),
                Box::new(single(search)),
                vec![
                    "The search never confirmed Congestion: the IP-Layer Sender Bit Rate was still rising in the last Sub-Interval: the Test ended too early, so the result may understate the path; a longer --test-interval helps",
                ],
            ),
            (
                "Upstream",
                up,
                Box::new(single(|o| o.direction = Direction::Upstream)),
                vec![
                    "The search never confirmed Congestion: this host's IP-Layer Sender Bit Rate stopped rising though few of the last 64 Trial Intervals were Errored Intervals: it may not have kept up, so the result may understate the path",
                ],
            ),
            (
                "two Test Connections, 120 Mbps between them",
                two(clean),
                Box::new(pair(|o| o.max_bandwidth = 120)),
                vec![
                    "Test Connection 0's search never confirmed Congestion: the IP-Layer Sender Bit Rate reached the 60 Mbps maxBandwidth it stated, its share of --max-bandwidth, at which a Server may stop its search, so the result may understate the path",
                    "Test Connection 1's search never confirmed Congestion: the Server's IP-Layer Sender Bit Rate, as estimated here, stopped rising though few of the last 64 Trial Intervals were Errored Intervals: it may not have kept up, so the result may understate the path",
                ],
            ),
            (
                "two Test Connections, one confirmed",
                two(confirmed),
                Box::new(pair(search)),
                vec![],
            ),
        ];
        for (what, test, options, says) in cases {
            let json = print(&test, &options(true), Some(at(0)), at(16_000));
            let ours = &stdout_json(&json)["udpstp"]["Warnings"];
            let listed: Vec<(&str, u64)> = ours
                .as_array()
                .into_iter()
                .flatten()
                .filter(|w| w["Code"] == "CongestionNeverConfirmed")
                .map(|w| {
                    (
                        w["Message"].as_str().unwrap_or_default(),
                        w["Count"].as_u64().unwrap_or_default(),
                    )
                })
                .collect();
            let want: Vec<(&str, u64)> = says.iter().map(|say| (*say, 1)).collect();
            assert_eq!(listed, want, "{what}: JSON {ours:#}");
            assert!(
                says.is_empty() || json.status == exit::test_status(&test),
                "{what}: a warning, which leaves the status as it was"
            );
            let text: String = print(&test, &options(false), Some(at(0)), at(16_000))
                .output
                .iter()
                .map(|(_, text)| text.as_str())
                .collect();
            let prose = text.split_whitespace().collect::<Vec<_>>().join(" ");
            assert_eq!(
                prose.matches("never confirmed Congestion").count(),
                says.len(),
                "{what}: text {text}"
            );
            for message in says {
                assert!(prose.contains(message), "{what}: missing {message}: {text}");
            }
        }
    }

    // : a Test whose only Sub-Interval that would meet the Performance
    // Criterion had nothing received in it (a stall), with nothing lost or
    // one Load PDU lost, within the allowance, has no Maximum IP-Layer
    // Capacity: ErrorStatus 49 and no AtMax in the JSON, and the text says
    // so, where it used to report 0.00 Mbps
    // (conformance: empty-sub-interval-meets-no-criterion)
    #[test]
    fn a_test_whose_only_passing_sub_interval_is_empty_has_no_maximum() {
        for lost in [0, 1] {
            let mut data: Vec<(DataEvent, Duration)> = (1..=5)
                .map(|seq| {
                    // A fifth of every Sub-Interval's Load PDUs lost, but for
                    // the third, in which nothing arrived.
                    let m = if seq == 3 {
                        SubIntervalMeasurement {
                            datagrams: 0,
                            bytes: 0,
                            ip_layer_capacity: 0,
                            loss: lost,
                            delay_variation: None,
                            rtt_variation: None,
                            active_test_connections: 0,
                            ..measurement(seq, 90, 0)
                        }
                    } else {
                        measurement(seq, 90, 250)
                    };
                    (DataEvent::SubIntervalMeasured(m), at(u64::from(seq) * 1000))
                })
                .collect();
            data.push((DataEvent::Ended(EndReason::Completed), at(5_600)));
            let test = TestOutcome {
                mc_ident: 45366,
                events: vec![],
                connections: vec![activated(
                    FIRST,
                    51646,
                    parameters(),
                    data,
                    EndReason::Completed,
                )],
            };
            let json = print(&test, &options(true, &[FIRST]), Some(at(0)), at(16_000));
            // It completed: exit 0, the missing maximum a warning in the JSON.
            assert_eq!(json.status.exit_code(), 0, "{lost} lost");
            let value = stdout_json(&json);
            assert_eq!(value["ErrorStatus"], 49, "{lost} lost: {value:#}");
            assert!(
                value["Output"].get("AtMax").is_none(),
                "{lost} lost: {value:#}"
            );
            assert_eq!(value["udpstp"]["CapacityModes"][0]["Valid"], false);
            let text = print(&test, &options(false, &[FIRST]), Some(at(0)), at(16_000));
            let said: String = text.output.iter().map(|(_, t)| t.as_str()).collect();
            assert!(
                said.contains(
                    "No Maximum IP-Layer Capacity: no Sub-Interval met the Performance Criterion"
                ) && said.contains("the result isn't valid"),
                "{lost} lost: {said}"
            );
        }
    }

    // : a Multi-Connection Test that completed reports Completed as the
    // Test's end though its first Test Connection hit the Watchdog, which
    // that Test Connection's own end and a warning naming it report
    #[test]
    fn a_multi_connection_tests_end_is_the_tests_own() {
        let mut silent = downstream(2, 60, 17, EndReason::Watchdog);
        let end = silent.pop().expect("an end");
        silent.push((
            DataEvent::TrafficStopped {
                silent_since: MonoTime::from_duration(Duration::from_secs(2)),
            },
            at(3_017),
        ));
        silent.push(end);
        let test = TestOutcome {
            mc_ident: 45366,
            events: vec![],
            connections: vec![
                activated(FIRST, 51646, parameters(), silent, EndReason::Watchdog),
                activated(
                    SECOND,
                    51647,
                    parameters(),
                    downstream(5, 30, 23, EndReason::Completed),
                    EndReason::Completed,
                ),
            ],
        };
        for (format, json) in [("text", false), ("json", true)] {
            // One Test Connection may fail, so the Test goes on.
            let options = ResultOptions {
                minimum: 1,
                ..options(json, &[FIRST, SECOND])
            };
            let printed = print(&test, &options, Some(at(0)), at(16_000));
            assert_eq!(
                printed.status,
                exit::Status {
                    error_status: exit::TRAFFIC_STOPPED,
                    completed: true
                }
            );
            crate::results::tests::golden(
                &format!("client/multi-completed-despite-a-watchdog-{format}.txt"),
                &transcript(&printed),
            );
            if json {
                let ours = &stdout_json(&printed)["udpstp"];
                assert_eq!(ours["EndReason"], "Completed");
                assert_eq!(ours["ControlPhaseFailure"], serde_json::Value::Null);
                assert_eq!(ours["TestConnections"][0]["EndReason"], "Watchdog");
                assert_eq!(ours["TestConnections"][1]["EndReason"], "Completed");
                let named = ours["Warnings"].as_array().is_some_and(|warnings| {
                    warnings.iter().any(|w| {
                        w["Code"] == "DataPhaseEnded"
                            && w["Message"]
                                .as_str()
                                .is_some_and(|m| m.contains("Test Connection 0"))
                    })
                });
                assert!(named, "{ours:#}");
            }
        }
    }

    // : an aborted Multi-Connection Test's end is why it was aborted,
    // and one whose Test Connections all ended otherwise says none
    // completed
    #[test]
    fn a_multi_connection_tests_end_names_its_abort() {
        for (end, want) in [
            ("timed-out", "TooFewTestConnections"),
            ("refused", "TooFewTestConnections"),
            ("aborted", "ParametersDiffer"),
            ("completed", "Completed"),
        ] {
            let printed = print(
                &multi(end),
                &options(true, &[FIRST, SECOND]),
                Some(at(0)),
                at(16_000),
            );
            let ours = &stdout_json(&printed)["udpstp"];
            assert_eq!(ours["EndReason"], want, "{end}");
            assert_eq!(
                ours["ControlPhaseFailure"],
                serde_json::Value::Null,
                "{end}"
            );
        }
        let mut test = multi("completed");
        for connection in &mut test.connections {
            connection.result = ClientResult::Finished {
                direction: Direction::Downstream,
                reason: Some(EndReason::Watchdog),
                error: None,
            };
        }
        let printed = print(
            &test,
            &options(true, &[FIRST, SECOND]),
            Some(at(0)),
            at(16_000),
        );
        assert_eq!(
            stdout_json(&printed)["udpstp"]["EndReason"],
            "NoTestConnectionCompleted"
        );
    }

    /// How a Test Connection to FIRST that ended with `result` ended.
    fn ended_as(result: ClientResult) -> TestConnectionEnd {
        TestConnectionEnd::of(&connection(FIRST, vec![], result))
    }

    // : how a Multi-Connection Test ran on this host is how all its
    // Test Connections' engines did, not its first's: the fast paths all
    // of them used, the smallest socket buffers, the overflows summed, the
    // send timestamps counted together with the largest lags
    #[test]
    fn a_multi_connection_test_ran_as_all_its_test_connections_did() {
        let completed = ended_as(ClientResult::Finished {
            direction: Direction::Upstream,
            reason: Some(EndReason::Completed),
            error: None,
        });
        let lag = |samples, ms: u64| SendLag {
            samples,
            p50: Duration::from_micros(ms),
            p99: Duration::from_micros(ms * 3),
            max: Duration::from_micros(ms * 10),
        };
        let first = EngineFacts {
            socket_buffers: Some(SocketBuffers {
                requested: 2_097_152,
                receive: 8_388_608,
                send: 212_992,
            }),
            fast_paths: FastPaths {
                sendmmsg: true,
                udp_gso: true,
                recvmmsg: true,
                kernel_rx_timestamps: true,
                ppoll: true,
                epoll: false,
            },
            receive_buffer_overflows: Some(3),
            send_lag: Some(lag(100, 10)),
            burst_lateness: Some(lag(100, 2)),
        };
        let second = EngineFacts {
            socket_buffers: Some(SocketBuffers {
                requested: 2_097_152,
                receive: 4_194_304,
                send: 4_194_304,
            }),
            fast_paths: FastPaths {
                udp_gso: false,
                ..first.fast_paths
            },
            receive_buffer_overflows: Some(4),
            send_lag: Some(lag(50, 20)),
            burst_lateness: None,
        };
        assert_eq!(
            engine_facts_of_test([(&first, &completed), (&second, &completed)]),
            EngineFacts {
                socket_buffers: Some(SocketBuffers {
                    requested: 2_097_152,
                    receive: 4_194_304,
                    send: 212_992,
                }),
                fast_paths: FastPaths {
                    udp_gso: false,
                    ..first.fast_paths
                },
                receive_buffer_overflows: Some(7),
                send_lag: Some(SendLag {
                    samples: 150,
                    p50: Duration::from_micros(20),
                    p99: Duration::from_micros(60),
                    max: Duration::from_micros(200),
                }),
                burst_lateness: Some(lag(100, 2)),
            }
        );
        // One Test Connection's alone are its own.
        assert_eq!(engine_facts_of_test([(&first, &completed)]), first);
    }

    // : a Multi-Connection Test's fast paths are those every Test
    // Connection that completed used, or, when none did, every one whose
    // Data Phase Engine ran until its Data Phase ended: one whose engine
    // never ran, or failed, or that ended early, empties no fast path
    #[test]
    fn a_multi_connection_tests_fast_paths_are_those_of_the_test_connections_that_ran() {
        let ended = |reason| {
            ended_as(ClientResult::Finished {
                direction: Direction::Upstream,
                reason,
                error: None,
            })
        };
        let completed = ended(Some(EndReason::Completed));
        let stopped = ended(Some(EndReason::Stopped));
        let watchdog = ended(Some(EndReason::Watchdog));
        let failed = ended(None);
        let all = EngineFacts {
            fast_paths: FastPaths {
                sendmmsg: true,
                udp_gso: true,
                recvmmsg: true,
                kernel_rx_timestamps: true,
                ppoll: true,
                epoll: false,
            },
            ..EngineFacts::default()
        };
        // Stopped early, before a Burst held more than one Load PDU.
        let no_gso = EngineFacts {
            fast_paths: FastPaths {
                udp_gso: false,
                ..all.fast_paths
            },
            ..EngineFacts::default()
        };
        // An engine that never ran, or failed before it said, reports none.
        let none = EngineFacts::default();
        let never_ran = ended_as(ClientResult::SocketError(
            "Too many open files (os error 24)".into(),
        ));
        // What each Test Connection's engine reported, with how it ended.
        type Ran<'a> = Vec<(&'a EngineFacts, &'a TestConnectionEnd)>;
        let cases: Vec<(&str, Ran<'_>, FastPaths)> = vec![
            (
                "an engine that never ran",
                vec![(&all, &completed), (&none, &never_ran)],
                all.fast_paths,
            ),
            (
                "an engine that failed",
                vec![(&all, &completed), (&none, &failed)],
                all.fast_paths,
            ),
            (
                "one the Watchdog ended early",
                vec![(&all, &completed), (&no_gso, &watchdog)],
                all.fast_paths,
            ),
            (
                "a fallback on one that completed",
                vec![(&all, &completed), (&no_gso, &completed)],
                no_gso.fast_paths,
            ),
            (
                "none completed: those that ran to their end",
                vec![(&all, &stopped), (&no_gso, &stopped), (&none, &failed)],
                no_gso.fast_paths,
            ),
            (
                "no engine ran",
                vec![(&none, &never_ran), (&none, &failed)],
                FastPaths::default(),
            ),
        ];
        for (what, each, want) in cases {
            assert_eq!(engine_facts_of_test(each).fast_paths, want, "{what}");
        }
    }

    /// A kind of Test, by the number of its Test Connections.
    type Kind = (
        &'static str,
        fn(&str) -> TestOutcome,
        &'static [&'static str],
    );

    // : what the Client prints once a Test has run, and its exit
    // status, for one Test Connection and for several, as text and as
    // JSON, completed, timed out, refused and aborted, and the Tests of
    // `more_cases` (characterization: the checked-in transcripts in
    // tests/golden/client/ change only on purpose)
    #[test]
    fn every_test_prints_as_it_did() {
        let kinds: [Kind; 2] = [
            ("single", single, &[FIRST]),
            ("multi", multi, &[FIRST, SECOND]),
        ];
        for (kind, test, hosts) in kinds {
            for end in ["completed", "timed-out", "refused", "aborted"] {
                for (format, json) in [("text", false), ("json", true)] {
                    let printed = print(&test(end), &options(json, hosts), Some(at(0)), at(16_000));
                    crate::results::tests::golden(
                        &format!("client/{kind}-{end}-{format}.txt"),
                        &transcript(&printed),
                    );
                }
            }
        }
        for (name, test, options) in more_cases() {
            for (format, json) in [("text", false), ("json", true)] {
                let printed = print(&test, &options(json), Some(at(0)), at(16_000));
                crate::results::tests::golden(
                    &format!("client/{name}-{format}.txt"),
                    &transcript(&printed),
                );
            }
        }
    }

    /// A multicast Server address, which the Client refuses to send to.
    const MULTICAST: &str = "224.0.0.1:24601";

    /// How the first Test Connection of a characterization case ended, and
    /// how its Test ran: a Test Connection to FIRST unless it names another
    /// Server, in a Test the Client user interrupted or ran Fixed-Rate.
    struct Ending {
        name: &'static str,
        outcome: ClientOutcome,
        interrupted: bool,
        fixed_rate: bool,
    }

    impl Ending {
        fn new(name: &'static str, outcome: ClientOutcome) -> Self {
            Self {
                name,
                outcome,
                interrupted: false,
                fixed_rate: false,
            }
        }

        fn interrupted(self) -> Self {
            Self {
                interrupted: true,
                ..self
            }
        }

        fn fixed_rate(self) -> Self {
            Self {
                fixed_rate: true,
                ..self
            }
        }
    }

    /// A Downstream Test Connection to FIRST whose Data Phase ended for
    /// `reason` after `sub_intervals` Sub-Intervals: the Watchdog's 2 s
    /// after nothing came for 1 s.
    fn ran_until(reason: EndReason, sub_intervals: u32) -> ClientOutcome {
        let mut data = downstream(sub_intervals, 90, 17, reason);
        if reason == EndReason::Watchdog {
            let end = data.pop().expect("an end");
            data.push((
                DataEvent::TrafficStopped {
                    silent_since: MonoTime::from_duration(Duration::from_millis(2_017)),
                },
                at(3_017),
            ));
            data.push(end);
        }
        activated(FIRST, 51646, parameters(), data, reason)
    }

    /// A Test Connection to FIRST whose Control Phase failed with
    /// `failure` after `events`, as the core reports it.
    fn failed_with(mut events: Vec<Event>, failure: ControlPhaseFailure) -> ClientOutcome {
        events.push(Event::ControlPhaseFailed(failure));
        connection(FIRST, events, ClientResult::Failed(failure))
    }

    /// Every way a Test Connection can end, as the Client tells them apart
    /// (the architecture review of 2026-09-30, finding F1): each EndReason
    /// its Data Phase can end with, and its Data Phase Engine's failure;
    /// every other ClientResult; each Control Phase failure; and each cause
    /// the core reports for a Test initiation timer that expired.
    fn endings() -> Vec<Ending> {
        use ControlExchange::{Activation, Setup};
        use ControlPhaseFailure as F;
        use ResponseIgnoreReason::{
            AuthTimeOutOfWindow, AuthenticationFailed, ChecksumFailed, MulticastOrBroadcast,
        };
        let accepted = || vec![Event::SetupAccepted { test_port: 51646 }];
        let engine_failed = {
            let mut c = ran_until(EndReason::Completed, 2);
            // A failed engine reports no end.
            c.data_events.pop();
            c.data_event_times.pop();
            c.result = ClientResult::Finished {
                direction: Direction::Downstream,
                reason: None,
                error: Some("the Data Phase panicked".into()),
            };
            c
        };
        let rejected = || {
            let mut events = accepted();
            events.push(Event::ActivationRejected);
            connection(FIRST, events, ClientResult::ActivationRejected)
        };
        // The Server coerced each, the Client told to refuse the first and
        // unable to run with the second.
        let refused = Coercion {
            parameter: ActivationParameter::DscpEcn,
            requested: 184,
            coerced: 0,
        };
        let unrunnable = Coercion {
            parameter: ActivationParameter::TestIntTime,
            requested: 10,
            coerced: 1,
        };
        let coerced = |coercion| {
            let mut events = accepted();
            events.push(Event::ParameterCoerced { coercion });
            events
        };
        let multicast = {
            let failure = F::MulticastOrBroadcastServer;
            connection(
                MULTICAST,
                vec![Event::ControlPhaseFailed(failure)],
                ClientResult::Failed(failure),
            )
        };
        let mut all = vec![
            Ending::new("completed", ran_until(EndReason::Completed, 5)),
            // It completed, as its exit status says, though its Data Phase
            // ended without the STOP exchange, which a warning says.
            Ending::new(
                "test-interval-elapsed",
                ran_until(EndReason::TestIntervalElapsed, 5),
            ),
            Ending::new("watchdog", ran_until(EndReason::Watchdog, 2)),
            Ending::new("stopped-interrupted", ran_until(EndReason::Stopped, 2)).interrupted(),
            // Stopped, though not by the Client user.
            Ending::new("stopped", ran_until(EndReason::Stopped, 2)),
            // A Server's own ends, which a Client's Data Phase doesn't report.
            Ending::new("unconfirmed", ran_until(EndReason::Unconfirmed, 2)),
            Ending::new(
                "stopped-by-client",
                ran_until(EndReason::StoppedByClient, 2),
            ),
            Ending::new("over-rate", ran_until(EndReason::OverRate, 2)),
            Ending::new("data-phase-engine-failed", engine_failed),
            Ending::new("activation-rejected", rejected()),
            Ending::new("activation-rejected-fixed-rate", rejected()).fixed_rate(),
            Ending::new(
                "unknown-activation-response",
                failed_with(accepted(), F::UnknownActivationResponse { cmd_response: 9 }),
            ),
            Ending::new(
                "unacceptable-coercion",
                failed_with(coerced(refused), F::UnacceptableCoercion(refused)),
            ),
            Ending::new(
                "unrunnable-parameter",
                failed_with(
                    coerced(unrunnable),
                    F::UnrunnableParameter {
                        parameter: unrunnable.parameter,
                        value: unrunnable.coerced,
                    },
                ),
            ),
            Ending::new(
                "setup-rejected",
                failed_with(
                    vec![],
                    F::SetupRejected(SetupRejectionCode::AuthTimeInvalid),
                ),
            ),
            Ending::new(
                "unknown-setup-response",
                failed_with(vec![], F::UnknownSetupResponse { cmd_response: 99 }),
            ),
            Ending::new(
                "no-authentication",
                failed_with(vec![], F::NoAuthenticationConfigured),
            ),
            Ending::new("no-key", failed_with(vec![], F::NoKeyForSending)),
            Ending::new(
                "ambiguous-key",
                failed_with(vec![], F::AmbiguousKeyForSending),
            ),
            Ending::new("multicast-server", multicast),
            Ending::new(
                "socket-unavailable",
                failed_with(vec![], F::SocketUnavailable),
            ),
            // Its socket failed as it opened: the runtime's result, which
            // the Coordinator's failure after it doesn't replace.
            Ending::new(
                "socket-error",
                connection(
                    FIRST,
                    vec![Event::ControlPhaseFailed(F::SocketUnavailable)],
                    ClientResult::SocketError("Too many open files (os error 24)".into()),
                ),
            ),
            // Given up before its Data Phase, as a Test of one Test
            // Connection is only when the Client user interrupts it.
            Ending::new(
                "abandoned",
                connection(
                    FIRST,
                    vec![Event::TestConnectionAbandoned],
                    ClientResult::Abandoned,
                ),
            )
            .interrupted(),
        ];
        // The Test initiation timer, by the exchange it expired in and why
        // the last response ignored there was.
        let causes = [
            ("timed-out-setup-unanswered", Setup, None),
            ("timed-out-setup-late", Setup, Some(AuthTimeOutOfWindow)),
            (
                "timed-out-setup-unverified",
                Setup,
                Some(AuthenticationFailed),
            ),
            (
                "timed-out-setup-broadcast",
                Setup,
                Some(MulticastOrBroadcast),
            ),
            ("timed-out-setup-corrupted", Setup, Some(ChecksumFailed)),
            ("timed-out-activation-unanswered", Activation, None),
            (
                "timed-out-activation-late",
                Activation,
                Some(AuthTimeOutOfWindow),
            ),
            (
                "timed-out-activation-unverified",
                Activation,
                Some(AuthenticationFailed),
            ),
            (
                "timed-out-activation-corrupted",
                Activation,
                Some(ChecksumFailed),
            ),
        ];
        for (name, exchange, last_ignored) in causes {
            let mut events = match exchange {
                Setup => vec![],
                Activation => accepted(),
            };
            events.extend(last_ignored.map(|reason| match exchange {
                Setup => Event::SetupResponseIgnored { reason },
                Activation => Event::ActivationResponseIgnored { reason },
            }));
            all.push(Ending::new(
                name,
                timed_out(FIRST, events, exchange, last_ignored),
            ));
        }
        all
    }

    /// A Test of `ending`'s one Test Connection, and its options.
    fn single_ending(ending: &Ending, json: bool) -> (TestOutcome, ResultOptions) {
        let server = ending.outcome.server.to_string();
        let test = TestOutcome {
            mc_ident: 45366,
            events: if ending.interrupted {
                vec![Event::TestAborted(TestAbort::Interrupted)]
            } else {
                vec![]
            },
            connections: vec![ending.outcome.clone()],
        };
        let options = ResultOptions {
            fixed_rate: ending.fixed_rate,
            ..options(json, &[&server])
        };
        (test, options)
    }

    /// A Multi-Connection Test to two Servers whose first Test Connection
    /// ended as `ending` says, as the Multi-Connection Coordinator would
    /// have run it, and its options:
    /// - one whose Data Phase ran went on alone once the second's Setup
    ///   Request went unanswered, as a Minimum Test Connections of 1 lets
    ///   it;
    /// - one the Client user interrupted before its Data Phase gave up the
    ///   second then too;
    /// - a refused coercion or an unrunnable parameter ended the whole Test,
    ///   the second given up then;
    /// - any other failure left too few of all two, the second given up
    ///   then.
    fn multi_ending(ending: &Ending, json: bool) -> (TestOutcome, ResultOptions) {
        let first = ending.outcome.clone();
        let interrupted = Event::TestAborted(TestAbort::Interrupted);
        let unanswered = connection(
            SECOND,
            vec![Event::TestConnectionAbandoned],
            ClientResult::Abandoned,
        );
        let set_up = connection(
            SECOND,
            vec![
                Event::SetupAccepted { test_port: 51647 },
                Event::TestConnectionAbandoned,
            ],
            ClientResult::Abandoned,
        );
        let (events, second, minimum) = match &first.result {
            ClientResult::Finished { .. } => (
                if ending.interrupted {
                    vec![interrupted]
                } else {
                    vec![]
                },
                unanswered,
                1,
            ),
            _ if ending.interrupted => (vec![interrupted], unanswered, 2),
            ClientResult::Failed(
                ControlPhaseFailure::UnacceptableCoercion(_)
                | ControlPhaseFailure::UnrunnableParameter { .. },
            ) => (
                vec![Event::TestAborted(TestAbort::TestConnectionEnded {
                    mc_index: 0,
                })],
                set_up,
                2,
            ),
            _ => (
                vec![Event::TestAborted(TestAbort::TooFewTestConnections {
                    minimum: 2,
                })],
                set_up,
                2,
            ),
        };
        let server = first.server.to_string();
        let options = ResultOptions {
            fixed_rate: ending.fixed_rate,
            minimum,
            ..options(json, &[&server, SECOND])
        };
        let test = TestOutcome {
            mc_ident: 45366,
            events,
            connections: vec![first, second],
        };
        (test, options)
    }

    /// Multi-Connection Tests whose two Test Connections ended alike, for
    /// what is said of both: their name, the Test and its options.
    fn multi_alike(json: bool) -> Vec<(&'static str, TestOutcome, ResultOptions)> {
        let late = ResponseIgnoreReason::AuthTimeOutOfWindow;
        let timed_out_late = |server| {
            timed_out(
                server,
                vec![Event::SetupResponseIgnored { reason: late }],
                ControlExchange::Setup,
                Some(late),
            )
        };
        let rejected = |server, test_port| {
            connection(
                server,
                vec![
                    Event::SetupAccepted { test_port },
                    Event::ActivationRejected,
                ],
                ClientResult::ActivationRejected,
            )
        };
        // A Setup Request refused by `server` for its time, outside the
        // Server's window.
        let refused_late = |server| {
            let refused = ControlPhaseFailure::SetupRejected(SetupRejectionCode::AuthTimeInvalid);
            connection(
                server,
                vec![Event::ControlPhaseFailed(refused)],
                ClientResult::Failed(refused),
            )
        };
        // A Setup Request in mode 2 that the Server refused, unverified, for
        // its Authentication Mode.
        let mode_2_refused = || {
            let unverified = ResponseIgnoreReason::AuthenticationFailed;
            timed_out(
                FIRST,
                vec![
                    Event::SetupResponseIgnored { reason: unverified },
                    Event::UnverifiedSetupRejection {
                        cmd_response: udpstp_wire::setup::cmd_response::AUTH_MODE_INVALID,
                        protocol_version: 20,
                        unsigned: false,
                    },
                    Event::AuthMode2Unsupported,
                ],
                ControlExchange::Setup,
                Some(unverified),
            )
        };
        vec![
            // Both Setup Responses came outside the time window, and both
            // Test initiation timers expired together.
            (
                "both-timed-out-setup-late",
                TestOutcome {
                    mc_ident: 45366,
                    events: vec![Event::TestAborted(TestAbort::TooFewTestConnections {
                        minimum: 2,
                    })],
                    connections: vec![timed_out_late(FIRST), timed_out_late(SECOND)],
                },
                options(json, &[FIRST, SECOND]),
            ),
            // Both Servers refused a Fixed-Rate Test's Activation Request,
            // the Test going on after the first as a Minimum Test
            // Connections of 1 lets it.
            (
                "both-activation-rejected-fixed-rate",
                TestOutcome {
                    mc_ident: 45366,
                    events: vec![],
                    connections: vec![rejected(FIRST, 51646), rejected(SECOND, 51647)],
                },
                ResultOptions {
                    fixed_rate: true,
                    minimum: 1,
                    ..options(json, &[FIRST, SECOND])
                },
            ),
            // Two Servers refused four Test Connections, two each, for a
            // time more than 5 s from their clocks, the Test going on after
            // each as a Minimum Test Connections of 1 lets it, until none
            // was left: why each was refused is its own, and to check the
            // clocks is said once.
            (
                "four-refused-late",
                TestOutcome {
                    mc_ident: 45366,
                    events: vec![Event::TestAborted(TestAbort::TooFewTestConnections {
                        minimum: 1,
                    })],
                    connections: vec![
                        refused_late(FIRST),
                        refused_late(SECOND),
                        refused_late(FIRST),
                        refused_late(SECOND),
                    ],
                },
                ResultOptions {
                    test_connections: 4,
                    minimum: 1,
                    ..options(json, &[FIRST, SECOND])
                },
            ),
            // Both Test Connections went to one Server without mode 2,
            // which refused both, unverified, and both Test initiation
            // timers expired together: what each unverified refusal said
            // is its own, and that the Server doesn't run mode 2 is said
            // once.
            (
                "both-refused-mode-2-unverified",
                TestOutcome {
                    mc_ident: 45366,
                    events: vec![Event::TestAborted(TestAbort::TooFewTestConnections {
                        minimum: 2,
                    })],
                    connections: vec![mode_2_refused(), mode_2_refused()],
                },
                ResultOptions {
                    test_connections: 2,
                    minimum: 2,
                    ..options(json, &[FIRST])
                },
            ),
        ]
    }

    // The architecture review of 2026-09-30 (F1): what the Client prints,
    // and its exit status, for every way a Test Connection can end, in a
    // Test of that one Test Connection and as the first of a
    // Multi-Connection Test's two, and for two that ended alike, as text
    // and as JSON (characterization: the checked-in transcripts
    // tests/golden/client/test-connection-ends-*.txt change only on
    // purpose)
    #[test]
    fn every_test_connection_end_prints_as_it_did() {
        for (format, json) in [("text", false), ("json", true)] {
            let mut single = Vec::new();
            let mut multi = Vec::new();
            for ending in endings() {
                for (kind, (test, options)) in [
                    (&mut single, single_ending(&ending, json)),
                    (&mut multi, multi_ending(&ending, json)),
                ] {
                    kind.push((ending.name, print(&test, &options, Some(at(0)), at(16_000))));
                }
            }
            for (name, test, options) in multi_alike(json) {
                multi.push((name, print(&test, &options, Some(at(0)), at(16_000))));
            }
            for (kind, printed) in [("single", single), ("multi", multi)] {
                let each: Vec<String> = printed
                    .iter()
                    .map(|(name, printed)| format!("=== {name}\n{}", transcript(printed)))
                    .collect();
                crate::results::tests::golden(
                    &format!("client/test-connection-ends-{kind}-{format}.txt"),
                    &each.join("\n"),
                );
            }
        }
    }

    // The architecture review of 2026-09-30 (F5): a Multi-Connection Test
    // says on stderr what a Test of each Test Connection that didn't
    // complete would say of it, naming it, before the Test's own error,
    // which stays the JSON's ErrorMessage; what concerns the whole Test
    // once, naming the Test Connections it was said of; and each line said
    // before keeps its stream
    #[test]
    fn a_multi_connection_test_says_why_each_test_connection_ended() {
        let stream_of = |printed: &Printed, stream: Stream| -> Vec<String> {
            printed
                .output
                .iter()
                .filter(|(s, _)| *s == stream)
                .map(|(_, text)| text.split_whitespace().collect::<Vec<_>>().join(" "))
                .collect()
        };
        let (_, test, text_options) = multi_alike(false)
            .into_iter()
            .find(|(name, ..)| *name == "both-timed-out-setup-late")
            .expect("the case");
        let text = print(&test, &text_options, None, at(3_000));
        assert_eq!(
            stream_of(&text, Stream::Stdout),
            [
                "--------------------------------------------------------------------------------",
                "Test Connection 0 to 192.0.2.1:24601: TestInitiationTimedOut",
                "Test Connection 1 to 192.0.2.2:24601: TestInitiationTimedOut",
                "--------------------------------------------------------------------------------",
                "Test outcome: aborted",
            ]
        );
        assert_eq!(
            stream_of(&text, Stream::Stderr),
            [
                "udpstp: Test Connection 0 to 192.0.2.1:24601: the Setup exchange didn't complete within 3 s",
                "udpstp: Test Connection 1 to 192.0.2.2:24601: the Setup exchange didn't complete within 3 s",
                "udpstp: Test Connections 0 and 1: a response was more than 5 s from this host's clock; check both clocks",
                "udpstp: the Test was aborted: fewer than 2 Test Connections (--min-test-connections) could run",
            ]
        );
        let json_options = ResultOptions {
            json: true,
            ..text_options
        };
        let json = print(&test, &json_options, None, at(3_000));
        let value = stdout_json(&json);
        assert_eq!(
            value["ErrorMessage"],
            "ERROR: the Test was aborted: fewer than 2 Test Connections (--min-test-connections) could run"
        );
        assert_eq!(
            value["ErrorMessage2"],
            "Test Connections 0 and 1: a response was more than 5 s from this host's clock; check both clocks"
        );
        let stderr = stream_of(&json, Stream::Stderr);
        assert_eq!(
            stderr.first().map(String::as_str),
            Some("udpstp: WARNING: Test Connection 0 to 192.0.2.1:24601: TestInitiationTimedOut")
        );
        assert_eq!(
            stderr
                .iter()
                .filter(|line| line.contains("check both clocks"))
                .count(),
            1,
            "{stderr:#?}"
        );
        assert!(
            stderr
                .last()
                .is_some_and(|line| line.starts_with("udpstp: ERROR: ")),
            "{stderr:#?}"
        );
    }

    // The architecture review of 2026-09-30 (F4): the pre-registered
    // judging of the Server-scale Rounds tells a Control Phase that got no
    // answer in the Activation exchange from one that got none in the
    // Setup exchange by "Activation exchange" in ErrorMessage or
    // ErrorMessage2, at ErrorStatus 200 with the names ControlPhaseFailed
    // and TestInitiationTimedOut (scripts/bench/scale_report.py,
    // unanswered()). ADR 0013 leaves the words free to change, so this
    // pins them.
    #[test]
    fn an_unanswered_exchange_is_named_where_the_judging_reads_it() {
        for (exchange, named) in [
            (ControlExchange::Activation, true),
            (ControlExchange::Setup, false),
        ] {
            let events = match exchange {
                ControlExchange::Setup => vec![],
                ControlExchange::Activation => vec![Event::SetupAccepted { test_port: 51646 }],
            };
            let test = TestOutcome {
                mc_ident: 45366,
                events: vec![],
                connections: vec![timed_out(FIRST, events, exchange, None)],
            };
            let printed = print(&test, &options(true, &[FIRST]), None, at(3_000));
            let json = stdout_json(&printed);
            assert_eq!(json["ErrorStatus"], 200, "{exchange:?}: {json:#}");
            assert_eq!(json["udpstp"]["EndReason"], "ControlPhaseFailed");
            assert_eq!(
                json["udpstp"]["ControlPhaseFailure"],
                "TestInitiationTimedOut"
            );
            let said = ["ErrorMessage", "ErrorMessage2"]
                .map(|key| json[key].as_str().unwrap_or_default().to_owned())
                .join(" ");
            assert_eq!(
                said.contains("Activation exchange"),
                named,
                "{exchange:?}: {said}"
            );
        }
    }
}
