//! The Client's text results (docs/spec.md "Results and output"; RFC 9097 §9):
//! the Test's parameters, a table of its Sub-Intervals, the Maximum
//! IP-Layer Capacity of each Capacity Mode, the warnings, and how the Test
//! ran. They go to stdout after the Test; stderr carries the notices as
//! they happen, so a warning can appear in both. They render a
//! [`TestResults`], as the JSON results do.

use std::fmt::Write as _;
use std::net::SocketAddr;
use std::time::Duration;

use udpstp_core::{
    Direction, MeasurementResults, PerformanceCriterion, SentInterval, SubIntervalMeasurement,
};
use udpstp_runtime::{EngineFacts, FastPaths};

use crate::color::{Palette, Role};
use crate::test_results::{Block, NoticeOutput, Stream, TestResults, Writer};
use crate::warnings::Warning;

/// The text results of a Test that ran: on stdout, and what went wrong on
/// stderr, in the order written.
pub fn print_with_color(
    results: &TestResults<'_>,
    stdout: Palette,
    stderr: Palette,
) -> Vec<(Stream, String)> {
    print_colored_at(
        results,
        crate::terminal::stdout_width(),
        crate::terminal::stderr_width(),
        stdout,
        stderr,
    )
}

/// Width is an input at the text rendering seam; structured results do
/// not pass through it.
#[cfg(test)]
pub(crate) fn print_at(
    results: &TestResults<'_>,
    stdout: usize,
    stderr: usize,
) -> Vec<(Stream, String)> {
    print_colored_at(results, stdout, stderr, Palette::plain(), Palette::plain())
}

pub(crate) fn print_colored_at(
    results: &TestResults<'_>,
    stdout: usize,
    stderr: usize,
    out_palette: Palette,
    err_palette: Palette,
) -> Vec<(Stream, String)> {
    let mut w = Writer::new();
    let (role, outcome) = results.outcome();
    w.outln(out_palette.rule(stdout));
    let test_connections = results.options.test_connections;
    if results.whole.is_some() {
        w.outln(out_palette.paint(Role::Muted, "Endpoints"));
        for (index, connection) in results.connections.iter().enumerate() {
            let client = connection
                .local
                .filter(|local| !local.ip().is_unspecified())
                .map_or_else(
                    || "unknown".into(),
                    |local| results.options.addresses.show_ip(local.ip()),
                );
            w.outln(out_palette.paint(
                Role::Muted,
                &format!(
                    "  Test Connection {index}: Client {client}; Server {}",
                    connection.server
                ),
            ));
        }
    }
    if test_connections > 1 {
        if let Some(whole) = &results.whole {
            w.outln(format!(
                "{} {test_connections} {}",
                out_palette.paint(Role::Info, "All"),
                out_palette.paint(Role::Info, "Test Connections together:")
            ));
            block(
                &mut w,
                results,
                whole,
                true,
                stdout,
                out_palette,
                err_palette,
            );
        }
        // How each Test Connection ended, with its own Maximum IP-Layer
        // Capacity.
        for (index, connection) in results.connections.iter().enumerate() {
            w.outln(format!(
                "Test Connection {index} to {}: {}",
                connection.server,
                crate::test_results::styled_outcome(
                    out_palette,
                    results.connection_outcome_role(index),
                    &connection.end.description()
                )
            ));
            if let Some(own) = &connection.block {
                block(
                    &mut w,
                    results,
                    own,
                    false,
                    stdout,
                    out_palette,
                    err_palette,
                );
            }
        }
    } else if let Some(whole) = &results.whole {
        block(
            &mut w,
            results,
            whole,
            true,
            stdout,
            out_palette,
            err_palette,
        );
    }
    w.notices(
        &results.notices,
        NoticeOutput::Text,
        out_palette,
        err_palette,
    );
    w.outln(out_palette.rule(stdout));
    w.outln(format!(
        "{}{} {}",
        out_palette.marker(role),
        out_palette.paint(Role::Heading, "Test outcome:"),
        out_palette.paint(role, outcome)
    ));
    w.into_output()
        .into_iter()
        .map(|(stream, text)| {
            let width = match stream {
                Stream::Stdout => stdout,
                Stream::Stderr => stderr,
            };
            (stream, crate::terminal::wrap(&text, width))
        })
        .collect()
}

/// A block of results: all of them, for the Test as a whole, or only the
/// maxima of one Test Connection of several; and a warning on stderr when a
/// Capacity Mode had no Maximum IP-Layer Capacity.
fn block(
    w: &mut Writer,
    results: &TestResults<'_>,
    block: &Block,
    whole: bool,
    width: usize,
    stdout: Palette,
    stderr: Palette,
) {
    let report = Report::of(results, block, whole);
    let (text, invalid) = if whole {
        render_at(&report, width, stdout)
    } else {
        maxima_only_at(&report, width, stdout)
    };
    w.out(text);
    if invalid {
        w.errln(format!(
            "{} the result isn't valid: no Sub-Interval met the Performance Criterion ({})",
            stderr.paint(Role::Warning, "udpstp: warning:"),
            stderr.paint(Role::Command, "--max-loss-ratio"),
        ));
    }
}

/// Everything the results show about one Test.
pub struct Report<'a> {
    /// A retained measurement must not look like an unqualified success
    /// when its Test was partial, interrupted or otherwise qualified.
    pub outcome_role: Role,
    /// Its Sub-Intervals, IP-Layer Sender Bit Rate and Capacity Modes, as
    /// the core computed them.
    pub measurements: &'a MeasurementResults,
    /// When the Client learned each Sub-Interval's result, not when its
    /// packets arrived at the Load Receiver.
    pub received: &'a [Option<Duration>],
    pub server: SocketAddr,
    pub direction: Direction,
    /// A Fixed-Rate Test rather than a Search Test: the Test's Phase (RFC
    /// 9097 §9). (conformance: results-name-the-phase-and-f)
    pub fixed_rate: bool,
    /// When the Test was activated, since the Unix epoch (T0).
    pub started: Option<Duration>,
    pub test_interval: Duration,
    pub sub_interval: Duration,
    pub trial_interval: Duration,
    /// Test Connections in the Test (F).
    pub test_connections: u32,
    pub max_hops: u8,
    pub dont_fragment: bool,
    /// The DSCP the Test's traffic was marked with.
    pub dscp: u8,
    pub criterion: PerformanceCriterion,
    /// Each coercion, as the Client printed it.
    pub coercions: Vec<String>,
    /// What makes the results less than they seem.
    pub warnings: &'a [Warning],
    /// How the Test ran on this host, as its Data Phase Engines reported:
    /// the socket buffers and fast paths the text shows.
    pub engine_facts: EngineFacts,
}

impl<'a> Report<'a> {
    /// What the text shows of `block`, for the Test as a whole, or for one
    /// of its Test Connections.
    fn of(results: &'a TestResults<'_>, block: &'a Block, whole: bool) -> Self {
        let options = results.options;
        let parameters = &block.parameters;
        Self {
            outcome_role: results.outcome().0,
            measurements: &block.measured.results,
            received: &block.measured.times,
            server: block.server,
            direction: block.direction,
            fixed_rate: options.fixed_rate,
            started: results.started,
            test_interval: parameters.test_interval,
            sub_interval: parameters.sub_interval,
            trial_interval: parameters.trial_interval,
            test_connections: if whole {
                options.test_connections.into()
            } else {
                1
            },
            max_hops: options.max_hops,
            dont_fragment: options.dont_fragment,
            dscp: block.dscp,
            criterion: options.criterion,
            coercions: block
                .coercions
                .iter()
                .map(crate::test_results::coercion_text)
                .collect(),
            warnings: &block.warnings,
            engine_facts: block.engine_facts,
        }
    }
}

/// Only the Maximum IP-Layer Capacity of each Capacity Mode, for one Test
/// Connection of several, and whether one had none.
fn maxima_only_at(report: &Report<'_>, width: usize, palette: Palette) -> (String, bool) {
    let mut out = String::new();
    let invalid = maxima(&mut out, report, width, palette);
    (crate::terminal::wrap(&out, width), invalid)
}

/// The results as text, and whether a Capacity Mode had no valid result.
#[cfg(test)]
pub fn render(report: &Report<'_>) -> (String, bool) {
    render_at(report, crate::terminal::UNKNOWN_WIDTH, Palette::plain())
}

fn render_at(report: &Report<'_>, width: usize, palette: Palette) -> (String, bool) {
    let measurements = report.measurements;
    let mut out = String::new();
    parameters(&mut out, report, sub_interval_count(report), palette);
    table(
        &mut out,
        &measurements.sub_intervals,
        &measurements.sender_bit_rates,
        report.test_connections > 1,
        measurements.sender_estimated,
        width,
        palette,
    );
    let invalid = maxima(&mut out, report, width, palette);
    warnings(&mut out, report, palette);
    metadata(&mut out, report, palette);
    (crate::terminal::wrap(&out, width), invalid)
}

/// How many Sub-Intervals the Test Interval holds.
fn sub_interval_count(report: &Report<'_>) -> u32 {
    u32::try_from(report.measurements.sub_intervals.len()).unwrap_or(u32::MAX)
}

fn parameters(out: &mut String, report: &Report<'_>, expected: u32, palette: Palette) {
    let dt = report.sub_interval.as_millis();
    let _ = writeln!(out, "{}", palette.paint(Role::Heading, "Results Summary"));
    let _ = writeln!(
        out,
        "  {} {} Test with {}, started {} (T0)",
        crate::names::direction(report.direction),
        if report.fixed_rate {
            "Fixed-Rate"
        } else {
            "Search"
        },
        report.server,
        palette.paint(
            Role::Muted,
            &report
                .started
                .map_or_else(|| "at an unknown time".into(), utc)
        ),
    );
    let details = format!(
        "Test Interval (I) {} s · {expected} Sub-Intervals (dt) {dt} ms · Trial Interval (FT) and st {} ms\n  {} Test Connection{} (F) · MaxHops {} · DSCP {} agreed",
        report.test_interval.as_secs(),
        report.trial_interval.as_millis(),
        report.test_connections,
        if report.test_connections == 1 {
            ""
        } else {
            "s"
        },
        report.max_hops,
        report.dscp,
    );
    let _ = writeln!(out, "  {}", palette.paint(Role::Muted, &details));
    let criterion = format!(
        "Performance Criterion: loss ≤ {} or one Load PDU · Tmax ≤ {dt} ms\n  Late Load PDUs count in the Sub-Interval where their gap was seen.",
        percent(report.criterion.max_loss_ppm),
    );
    let _ = writeln!(out, "  {}", palette.paint(Role::Muted, &criterion));
    for coercion in &report.coercions {
        let _ = writeln!(out, "  {coercion}");
    }
}

/// One row per Sub-Interval; one never measured says so.
fn table(
    out: &mut String,
    sub_intervals: &[Option<SubIntervalMeasurement>],
    sent: &[Option<SentInterval>],
    multi: bool,
    estimated: bool,
    width: usize,
    palette: Palette,
) {
    let sender = sent.iter().any(Option::is_some);
    let _ = writeln!(out);
    let mut headers = vec![
        "SI",
        "C (Mbps)",
        "Loss Ratio",
        "Lost",
        "Reordered",
        "Duplicates",
        "DV (ms)",
        "RTTV (ms)",
    ];
    if multi {
        headers.push("Active");
    }
    if sender {
        headers.push("B (Mbps)*");
    }
    let mut rows = Vec::new();
    for (index, slot) in sub_intervals.iter().enumerate() {
        let seq = index + 1;
        let Some(m) = slot else {
            let mut row = vec![seq.to_string(), "not measured".into()];
            row.resize(headers.len(), "-".into());
            rows.push(row);
            continue;
        };
        let delay = m
            .delay_variation
            .map_or_else(|| "-".into(), |d| format!("{}/{}/{}", d.min, d.mean, d.max));
        // The mean apart from the range: a Load Sender's is taken from
        // fewer samples and can fall outside it.
        let rtt = match (m.rtt_variation, m.rtt_variation_mean) {
            (Some((low, high)), Some(mean)) => format!("{low}-{high}, {mean}"),
            (Some((low, high)), None) => format!("{low}-{high}, -"),
            _ => "-".into(),
        };
        let mut row = vec![
            seq.to_string(),
            mbps(m.ip_layer_capacity),
            loss_ratio(m),
            m.loss.to_string(),
            m.out_of_order.to_string(),
            m.duplicates.to_string(),
            delay,
            rtt,
        ];
        if multi {
            row.push(m.active_test_connections.to_string());
        }
        if sender {
            row.push(
                sent.get(index)
                    .copied()
                    .flatten()
                    .map_or_else(|| "-".into(), |s| mbps(s.ip_layer_sender_bit_rate)),
            );
        }
        rows.push(row);
    }
    let _ = writeln!(
        out,
        "  {}",
        palette.paint(Role::Heading, "Sub-Interval results")
    );
    if multi {
        let _ = writeln!(
            out,
            "  Active = Test Connections that received in the Sub-Interval."
        );
    }
    let styled: Vec<_> = headers
        .iter()
        .map(|header| palette.paint(Role::Muted, header))
        .collect();
    let headers: Vec<_> = styled.iter().map(String::as_str).collect();
    if crate::terminal::table_fits(&headers, &rows, width) {
        out.push_str(&crate::terminal::table(&headers, &rows, width));
    } else {
        // Keep the Sub-Interval number in every group so that a row never
        // has to be traced across a wrapping line or another table.
        let mut groups = vec![vec![0, 1, 2, 3, 4, 5]];
        let mut rates = vec![0, 6, 7];
        if multi {
            rates.push(8);
        }
        if sender {
            rates.push(if multi { 9 } else { 8 });
        }
        if rates.len() > 1 {
            groups.push(rates);
        }
        for group in groups {
            let selected: Vec<_> = group.iter().map(|&i| headers[i]).collect();
            let data: Vec<_> = rows
                .iter()
                .map(|r| group.iter().map(|&i| r[i].clone()).collect())
                .collect();
            out.push_str(&crate::terminal::table(&selected, &data, width));
            out.push('\n');
        }
    }
    let legend = if sender {
        "SI: Sub-Interval · C: IP-Layer Capacity · B: IP-Layer Sender Bit Rate"
    } else {
        "SI: Sub-Interval · C: IP-Layer Capacity"
    };
    let _ = writeln!(out, "  {}", palette.paint(Role::Muted, legend));
    let _ = writeln!(out, "  {}", palette.paint(Role::Muted,
        "DV: delay variation (min/avg/max) · RTTV: RTT variation (min-max, avg) · -: no measurement"));
    if sender && estimated {
        let _ = writeln!(
            out,
            "  {}", palette.paint(Role::Muted, "* Estimated from the Server's lpduSeqNo span on its st clock.\n  Lost Load PDUs use the size of surrounding Load PDUs.")
        );
    } else if sender && multi {
        let _ = writeln!(
            out,
            "  {}", palette.paint(Role::Muted, "* Approximate sum by st number across Test Connections. Each st grid\n  starts one one-way delay before its Load Receiver's grid.")
        );
    } else if sender {
        let _ = writeln!(
            out,
            "  {}", palette.paint(Role::Muted, "* Local st grid starts one one-way delay before the Load Receiver's.\n  Rows align only to within that delay.")
        );
    }
}

/// Each Capacity Mode's Maximum IP-Layer Capacity; whether one had none.
/// Concise labelled metrics replace the RFC's suggested table columns.
/// (conformance: compact-human-final-metrics)
fn maxima(out: &mut String, report: &Report<'_>, _width: usize, palette: Palette) -> bool {
    let measurements = report.measurements;
    let modes = &measurements.capacity_modes;
    let sent = &measurements.sender_bit_rates;
    let estimated = measurements.sender_estimated;
    let _ = writeln!(out);
    let _ = writeln!(out, "{}", palette.paint(Role::Heading, "Final metrics"));
    let mut invalid = false;
    for (index, mode) in modes.iter().enumerate() {
        if modes.len() > 1 {
            let _ = writeln!(
                out,
                "  {} {} ({} {}-{})",
                palette.paint(Role::Heading, "Capacity Mode"),
                index + 1,
                palette.paint(Role::Muted, "Sub-Intervals"),
                mode.sub_intervals.start(),
                mode.sub_intervals.end()
            );
        }
        let maximum = mode.maximum.as_ref();
        if let Some(max) = maximum {
            let _ = writeln!(
                out,
                "  {} {} {}",
                palette.paint(Role::Muted, "Maximum IP-Layer Capacity:"),
                palette.paint(
                    report.outcome_role,
                    &format!("{} Mbps", mbps(max.ip_layer_capacity))
                ),
                palette.paint(
                    Role::Muted,
                    &format!("(Sub-Interval {})", max.sub_int_seq_no)
                )
            );
        }
        if let Some(max) = maximum {
            let received = max
                .sub_int_seq_no
                .checked_sub(1)
                .and_then(|i| report.received.get(i as usize))
                .copied()
                .flatten();
            let text = format!(
                "Maximum Sub-Interval result received at {}",
                received.map_or_else(|| "an unknown time".into(), utc_precise)
            );
            let _ = writeln!(out, "  {}", palette.paint(Role::Muted, &text));
        }
        let missing = match mode.missing {
            0 => String::new(),
            1 => "; 1 Sub-Interval not measured".into(),
            n => format!("; {n} Sub-Intervals not measured"),
        };
        match (&mode.maximum, &mode.highest) {
            (Some(max), highest) => {
                // The Highest IP-Layer Capacity, what OB-UDPST reports,
                // where another Sub-Interval has it.
                let highest = highest
                    .filter(|h| h.sub_int_seq_no != max.sub_int_seq_no)
                    .map(|h| {
                        format!(
                            "; Highest IP-Layer Capacity {} Mbps in Sub-Interval {} (Loss Ratio {})",
                            mbps(h.ip_layer_capacity),
                            h.sub_int_seq_no,
                            loss_ratio(&h),
                        )
                    })
                    .unwrap_or_default();
                let sender = sent
                    .get(max.sub_int_seq_no as usize - 1)
                    .copied()
                    .flatten()
                    .map(|s| {
                        format!(
                            ", {}IP-Layer Sender Bit Rate {} Mbps",
                            if estimated { "estimated " } else { "" },
                            mbps(s.ip_layer_sender_bit_rate)
                        )
                    })
                    .unwrap_or_default();
                let _ = writeln!(
                    out,
                    "  Loss Ratio {}{}{sender}{highest}{missing}",
                    loss_ratio(max),
                    rtt(max),
                );
            }
            (None, Some(highest)) => {
                invalid = true;
                let _ = writeln!(
                    out,
                    "  {} no Sub-Interval met the Performance Criterion. The Highest IP-Layer Capacity was {} Mbps in Sub-Interval {} (Loss Ratio {}){missing}",
                    palette.paint(Role::Warning, "No Maximum IP-Layer Capacity:"),
                    mbps(highest.ip_layer_capacity),
                    highest.sub_int_seq_no,
                    loss_ratio(highest),
                );
            }
            (None, None) => {
                invalid = true;
                let _ = writeln!(
                    out,
                    "  {} no Sub-Interval was measured{missing}",
                    palette.paint(Role::Warning, "No Maximum IP-Layer Capacity:"),
                );
            }
        }
    }
    // The minimum the RTT variation figures are measured from, and the
    // minimum one-way delay (meaningful only with synchronized clocks).
    let measured = || measurements.sub_intervals.iter().flatten();
    if let Some(minimum) = measured().filter_map(|m| m.rtt_minimum).min() {
        let _ = writeln!(out, "  RTT minimum {minimum} ms");
    }
    if let Some(minimum) = measured().filter_map(|m| m.clock_delta_min).min() {
        let _ = writeln!(
            out,
            "  Minimum one-way delay {minimum} ms (clockDeltaMin; valid only if the clocks are synchronized)"
        );
    }
    invalid
}

fn warnings(out: &mut String, report: &Report<'_>, palette: Palette) {
    if report.warnings.is_empty() {
        return;
    }
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "  {}{}",
        palette.marker(Role::Warning),
        palette.paint(Role::Warning, "Warnings")
    );
    for warning in report.warnings {
        let _ = writeln!(out, "    {}", warning.message());
    }
}

// How the Test ran on this host: its sockets and fast paths.
fn metadata(out: &mut String, report: &Report<'_>, palette: Palette) {
    let _ = writeln!(out);
    let buffers = report.engine_facts.socket_buffers.map_or_else(
        || "unknown".into(),
        |b| format!("receive {} and send {} bytes", b.receive, b.send),
    );
    let text = format!(
        "This host: DF {}, socket buffers {buffers}; portable Data Phase Engine, {}",
        if report.dont_fragment { "set" } else { "clear" },
        fast_paths(report.engine_facts.fast_paths),
    );
    let _ = writeln!(out, "  {}", palette.paint(Role::Muted, &text));
}

/// The fast paths the Data Phase Engine sent Load PDUs, read
/// datagrams and waited with. UDP GSO goes without sendmmsg
/// where each message carried the number of its kernel send timestamp
///
fn fast_paths(paths: FastPaths) -> String {
    let sending = match (paths.sendmmsg, paths.udp_gso) {
        (true, true) => Some("sending with sendmmsg and UDP GSO"),
        (true, false) => Some("sending with sendmmsg"),
        (false, true) => Some("sending with UDP GSO"),
        (false, false) => None,
    };
    let receiving = match (paths.recvmmsg, paths.kernel_rx_timestamps) {
        (true, true) => Some("receiving with recvmmsg and kernel timestamps"),
        (true, false) => Some("receiving with recvmmsg"),
        (false, true) => Some("receiving with kernel timestamps"),
        (false, false) => None,
    };
    let waiting = paths.ppoll.then_some("waking on arrival with ppoll");
    let parts: Vec<&str> = [sending, receiving, waiting]
        .into_iter()
        .flatten()
        .collect();
    if parts.is_empty() {
        "no fast paths".to_owned()
    } else {
        parts.join(", ")
    }
}

/// bit/s as Mbps with two decimals.
fn mbps(bits_per_second: u64) -> String {
    format!("{:.2}", bits_per_second as f64 / 1e6)
}

fn percent(ppm: u32) -> String {
    format!("{}%", f64::from(ppm) / 10_000.0)
}

fn loss_ratio(m: &SubIntervalMeasurement) -> String {
    match m.datagrams_sent() {
        0 => "-".into(),
        sent => format!("{:.6}", f64::from(m.loss) / sent as f64),
    }
}

/// Absolute RTT bounds for the selected maximum. The Sub-Interval table
/// already carries its variation range and mean.
fn rtt(m: &SubIntervalMeasurement) -> String {
    match (m.rtt_variation, m.rtt_minimum) {
        (Some((_, high)), Some(minimum)) => {
            format!(
                ", RTT minimum {minimum} ms, maximum {} ms",
                u64::from(minimum) + u64::from(high)
            )
        }
        (None, Some(minimum)) => {
            format!(", RTT minimum {minimum} ms (maximum not measured)")
        }
        _ => String::new(),
    }
}

/// A time since the Unix epoch as UTC, to the second (RFC 3339).
pub(crate) fn utc(since_epoch: Duration) -> String {
    let secs = since_epoch.as_secs();
    let (days, rest) = (secs / 86_400, secs % 86_400);
    // Days to a civil date (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

fn utc_precise(since_epoch: Duration) -> String {
    let timestamp = utc(since_epoch);
    if since_epoch.subsec_nanos() == 0 {
        timestamp
    } else {
        format!(
            "{}.{:09}Z",
            timestamp.trim_end_matches('Z'),
            since_epoch.subsec_nanos()
        )
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use udpstp_core::{DataEvent, DelayRange, EndReason};
    use udpstp_runtime::SocketBuffers;

    fn measurement(seq: u32, capacity: u64, datagrams: u32, loss: u32) -> SubIntervalMeasurement {
        SubIntervalMeasurement {
            sub_int_seq_no: seq,
            datagrams,
            bytes: 0,
            ip_layer_capacity: capacity,
            loss,
            out_of_order: 1,
            duplicates: 0,
            delay_variation: Some(DelayRange {
                min: 0,
                max: 3,
                mean: 1,
            }),
            delay_variation_sum: 10,
            delay_variation_count: 10,
            rtt_variation: Some((1, 4)),
            rtt_variation_sum: 20,
            rtt_variation_count: 10,
            rtt_variation_mean: Some(2),
            rtt_minimum: Some(10),
            clock_delta_min: Some(5),
            active_test_connections: 1,
            measured_at: udpstp_core::MonoTime::from_duration(Duration::ZERO),
            duration: Duration::from_secs(1),
            accum_time: Duration::from_secs(seq.into()),
        }
    }

    /// Socket buffers Linux's default limits held short.
    const SHORT: Option<SocketBuffers> = Some(SocketBuffers {
        requested: 2_097_152,
        receive: 212_992,
        send: 212_992,
    });

    /// How a fixture's Test ended, and its host's socket buffers.
    struct Ending {
        end: Option<EndReason>,
        failure: Option<&'static str>,
        socket_buffers: Option<SocketBuffers>,
    }

    /// The measurement results and warnings of a 4 s Test that gave
    /// `events` and ended as `ending` says.
    fn measured(
        events: &[DataEvent],
        first_mode: Option<u32>,
        ending: &Ending,
    ) -> (MeasurementResults, Vec<Warning>) {
        let measurements = MeasurementResults::of_test_connection(
            events,
            &udpstp_core::Evaluation {
                test_interval: Duration::from_secs(4),
                sub_interval: Duration::from_secs(1),
                ip_version: udpstp_core::IpVersion::V4,
                criterion: PerformanceCriterion::DEFAULT,
                first_mode,
            },
        );
        let warnings = crate::warnings::of(&crate::warnings::Sources {
            data_events: events,
            ends: &[crate::warnings::DataPhaseEnd {
                test_connection: None,
                reason: ending.end,
                failure: ending.failure,
            }],
            interrupted: false,
            first_mode,
            sub_intervals: 4,
            engine_facts: EngineFacts {
                socket_buffers: ending.socket_buffers,
                receive_buffer_overflows: Some(0),
                ..EngineFacts::default()
            },
            socket_buffer_limits: "the net.core.rmem_max and net.core.wmem_max sysctls",
            approximate: None,
        });
        (measurements, warnings)
    }

    fn report<'a>(
        measurements: &'a MeasurementResults,
        warnings: &'a [Warning],
        coercions: &[String],
        socket_buffers: Option<SocketBuffers>,
    ) -> Report<'a> {
        Report {
            outcome_role: if warnings.is_empty() {
                Role::Success
            } else {
                Role::Warning
            },
            measurements,
            received: &[],
            server: "192.0.2.1:24601".parse().unwrap(),
            direction: Direction::Downstream,
            fixed_rate: false,
            started: Some(Duration::from_secs(1_790_000_000)),
            test_interval: Duration::from_secs(4),
            sub_interval: Duration::from_secs(1),
            trial_interval: Duration::from_millis(50),
            test_connections: 1,
            max_hops: 16,
            dont_fragment: true,
            dscp: 0,
            criterion: PerformanceCriterion::DEFAULT,
            coercions: coercions.to_vec(),
            warnings,
            engine_facts: EngineFacts {
                socket_buffers,
                fast_paths: FastPaths {
                    sendmmsg: true,
                    udp_gso: true,
                    recvmmsg: true,
                    kernel_rx_timestamps: true,
                    ppoll: true,
                    epoll: false,
                },
                ..EngineFacts::default()
            },
        }
    }

    /// Compares with a golden file; `UDPSTP_BLESS=1` rewrites it.
    /// Golden files use `udpstp {version}` for the current package version.
    pub(crate) fn golden(name: &str, text: &str) {
        let version = concat!("udpstp ", env!("CARGO_PKG_VERSION"));
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden")
            .join(name);
        if std::env::var("UDPSTP_BLESS").as_deref() == Ok("1") {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, text.replace(version, "udpstp {version}")).unwrap();
        }
        let want = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{}: {e}; run with UDPSTP_BLESS=1", path.display()))
            .replace("\r\n", "\n")
            .replace("udpstp {version}", version);
        assert_eq!(
            text,
            want,
            "{} differs; run with UDPSTP_BLESS=1 if intended",
            path.display()
        );
    }

    // docs/spec.md story 25;
    #[udpstp_req::req(
        "RFC9097-5.6-1",
        "RFC9097-5.6-2",
        "RFC9097-6.4-L1",
        "RFC9097-6.6-1",
        "RFC9097-6.6-2",
        "RFC9097-9-1",
        "RFC9097-9-2",
        "RFC9097-9-5",
        "RFC9097-9-7",
        "RFC9097-9-8",
        "RFC9097-4-4",
        "RFC9097-5.6-3",
        "RFC9097-9-3",
        "RFC9097-9-4",
        "RFC9097-9-10"
    )]
    #[test]
    fn a_completed_test() {
        let events = [
            DataEvent::SubIntervalMeasured(measurement(1, 90_000_000, 1000, 0)),
            DataEvent::SubIntervalMeasured(measurement(2, 95_000_000, 1000, 50)),
            DataEvent::SubIntervalMeasured(measurement(3, 93_000_000, 1000, 2)),
            DataEvent::BurstsSkipped {
                bursts: 1,
                datagrams: 3,
                bytes: 3000,
            },
            DataEvent::PeerRxStopped { count: 2 },
        ];
        let coercions = ["The Server set testIntTime to 4 (asked for 10)".to_owned()];
        let (measurements, warnings) = measured(
            &events,
            None,
            &Ending {
                end: Some(EndReason::Completed),
                failure: None,
                socket_buffers: SHORT,
            },
        );
        let (text, invalid) = render(&report(&measurements, &warnings, &coercions, SHORT));
        assert!(!invalid);
        golden("completed.txt", &text);
    }

    // A Capacity Mode without a valid maximum; docs/spec.md (Watchdog
    // notifications in the output);
    #[udpstp_req::req("RFC9097-6.3-L1", "RFC9097-6.6-3", "RFC9097-9-6")]
    #[test]
    fn a_watchdog_ended_test_with_two_capacity_modes() {
        let events = [
            DataEvent::SubIntervalMeasured(measurement(1, 90_000_000, 1000, 0)),
            DataEvent::SubIntervalMeasured(measurement(3, 50_000_000, 100, 50)),
            DataEvent::TrafficStopped {
                silent_since: udpstp_core::MonoTime::from_duration(std::time::Duration::ZERO),
            },
        ];
        let (measurements, warnings) = measured(
            &events,
            Some(2),
            &Ending {
                end: Some(EndReason::Watchdog),
                failure: None,
                socket_buffers: SHORT,
            },
        );
        let (text, invalid) = render(&report(&measurements, &warnings, &[], SHORT));
        assert!(invalid);
        golden("watchdog.txt", &text);
    }

    // : each Capacity Mode's Maximum line names the Highest IP-Layer
    // Capacity when another Sub-Interval has it, with its Loss Ratio, as
    // OB-UDPST reports it; not when it is the Maximum's own
    #[test]
    fn the_maximum_line_names_a_highest_elsewhere() {
        let ended = Ending {
            end: Some(EndReason::Completed),
            failure: None,
            socket_buffers: SHORT,
        };
        let text = |events: &[DataEvent], first_mode| {
            let (measurements, warnings) = measured(events, first_mode, &ended);
            render(&report(&measurements, &warnings, &[], SHORT))
                .0
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        };
        let events = [
            DataEvent::SubIntervalMeasured(measurement(1, 60_000_000, 1000, 0)),
            DataEvent::SubIntervalMeasured(measurement(2, 96_000_000, 1000, 40)),
            DataEvent::SubIntervalMeasured(measurement(3, 50_000_000, 1000, 0)),
            DataEvent::SubIntervalMeasured(measurement(4, 45_000_000, 1000, 0)),
        ];
        let one = text(&events, None);
        assert!(
            one.contains("60.00 Mbps (Sub-Interval 1)")
                && one.contains("Loss Ratio 0.000000")
                && one.contains(
                    "Highest IP-Layer Capacity 96.00 Mbps in Sub-Interval 2 (Loss Ratio 0.038462)"
                ),
            "{one}"
        );
        // Split after Sub-Interval 2, the second Capacity Mode's Maximum is
        // its Highest too.
        let two = text(&events, Some(2));
        assert_eq!(two.matches("Highest IP-Layer Capacity").count(), 1, "{two}");
    }

    // : an Upstream Test, where this end is the Load Sender, with gaps
    // in the samples and unknown socket buffers
    #[udpstp_req::req(
        "RFC9097-7.4-1",
        "RFC9097-7.5-1",
        "RFC9097-9-9",
        "RFC9097-9-13",
        "RFC9097-9-14",
        "RFC9097-9-15"
    )]
    #[test]
    fn an_upstream_test() {
        let sent: Vec<DataEvent> = (1..=40)
            .map(|seq| {
                DataEvent::Sent(SentInterval {
                    st_seq_no: seq,
                    datagrams: 500,
                    bytes: 611_000,
                    ip_layer_sender_bit_rate: 100_000_000,
                    duration: Duration::from_millis(50),
                })
            })
            .collect();
        let mut events = vec![
            DataEvent::SubIntervalMeasured(SubIntervalMeasurement {
                delay_variation: None,
                rtt_variation_mean: Some(7),
                ..measurement(1, 99_000_000, 10_000, 0)
            }),
            DataEvent::SubIntervalMeasured(SubIntervalMeasurement {
                rtt_variation: None,
                rtt_variation_mean: None,
                ..measurement(2, 98_000_000, 10_000, 12_345)
            }),
        ];
        events.extend(sent);
        let (measurements, warnings) = measured(
            &events,
            None,
            &Ending {
                end: None,
                failure: Some("the socket closed"),
                socket_buffers: None,
            },
        );
        let mut report = report(&measurements, &warnings, &[], None);
        report.direction = Direction::Upstream;
        let (text, invalid) = render(&report);
        assert!(!invalid);
        golden("upstream.txt", &text);
    }

    // : where each message carried the number of its kernel send
    // timestamp, the Load PDUs went as UDP GSO messages without sendmmsg,
    // and "This host" says so
    #[test]
    fn udp_gso_without_sendmmsg_is_named() {
        let paths = FastPaths {
            udp_gso: true,
            ..FastPaths::default()
        };
        assert_eq!(fast_paths(paths), "sending with UDP GSO");
    }

    // The text rendering seam preserves RFC 9097's reporting context,
    // PM metrics and separate Capacity Modes at ordinary terminal widths.
    #[udpstp_req::req(
        "RFC9097-9-1",
        "RFC9097-9-2",
        "RFC9097-9-3",
        "RFC9097-9-4",
        "RFC9097-9-5",
        "RFC9097-9-6",
        "RFC9097-9-7",
        "RFC9097-9-8"
    )]
    #[test]
    fn terminal_layout_preserves_measurements_context_and_validity() {
        let mut first = measurement(1, 123_450_000, 12345, 12);
        first.out_of_order = 17;
        first.duplicates = 19;
        first.delay_variation = Some(DelayRange {
            min: 2,
            mean: 5,
            max: 9,
        });
        first.rtt_variation = Some((3, 11));
        first.rtt_variation_mean = Some(7);
        first.rtt_minimum = Some(23);
        first.active_test_connections = 3;
        let events = [
            DataEvent::SubIntervalMeasured(first),
            DataEvent::SubIntervalMeasured(measurement(3, 77_880_000, 100, 50)),
        ];
        let (mut measurements, warnings) = measured(
            &events,
            Some(2),
            &Ending {
                end: Some(EndReason::Watchdog),
                failure: None,
                socket_buffers: SHORT,
            },
        );
        measurements.sender_bit_rates[0] = Some(SentInterval {
            st_seq_no: 1,
            datagrams: 12357,
            bytes: 1,
            ip_layer_sender_bit_rate: 125_670_000,
            duration: Duration::from_secs(1),
        });
        for width in [60, 80, 100, 120] {
            for direction in [Direction::Upstream, Direction::Downstream] {
                for count in [1, 3] {
                    let mut report = report(&measurements, &warnings, &[], SHORT);
                    report.server = "[2001:db8:abcd:1234:5678:9999:aaaa:bbbb]:24601"
                        .parse()
                        .unwrap();
                    report.direction = direction;
                    report.test_connections = count;
                    report.fixed_rate = direction == Direction::Upstream;
                    let (text, invalid) = render_at(&report, width, Palette::plain());
                    assert!(invalid, "a Capacity Mode without a valid maximum: {text}");
                    assert!(
                        text.lines()
                            .all(|line| crate::terminal::columns(line) <= width),
                        "width {width}: {text}"
                    );
                    let prose = text.split_whitespace().collect::<Vec<_>>().join(" ");
                    for context in [
                        "[2001:db8:abcd:1234:5678:9999:aaaa:bbbb]:24601",
                        "2026-09-21T14:13:20Z (T0)",
                        "Test Interval (I) 4 s",
                        "(dt) 1000 ms",
                        "Trial Interval (FT) and st 50 ms",
                        "MaxHops 16",
                        "DSCP 0 agreed",
                        "Performance Criterion",
                        "Capacity Mode 1 (Sub-Intervals 1-2)",
                        "Capacity Mode 2 (Sub-Intervals 3-4)",
                        "No Maximum IP-Layer Capacity: no Sub-Interval met the Performance Criterion",
                        "1 Sub-Interval not measured",
                        "Watchdog",
                    ] {
                        assert!(
                            prose.contains(context),
                            "missing {context} at {width}: {text}"
                        );
                    }
                    for value in [
                        "123.45", "125.67", "77.88", "0.000971", "12", "17", "19", "2/5/9",
                        "3-11, 7", "23", "34",
                    ] {
                        assert!(prose.contains(value), "missing {value} at {width}: {text}");
                    }
                    assert!(
                        prose.contains(if report.fixed_rate {
                            "Upstream Fixed-Rate"
                        } else {
                            "Downstream Search"
                        }),
                        "{text}"
                    );
                    if count > 1 {
                        assert!(prose.contains("Active = Test Connections"), "{text}");
                    }
                    if direction == Direction::Downstream && count == 3 {
                        golden(&format!("terminal-{width}.txt"), &text);
                    }
                }
            }
        }
    }

    // The compact-human-final-metrics record preserves the selected
    // maximum's measured RTT minimum even if its variation is unavailable.
    #[udpstp_req::req("RFC9097-9-8")]
    #[test]
    fn maxima_keep_measured_rtt_minimum_without_variation() {
        let mut maximum = measurement(1, 95_000_000, 1000, 0);
        maximum.rtt_minimum = Some(23);
        maximum.rtt_variation = None;
        maximum.rtt_variation_mean = None;
        let (measurements, warnings) = measured(
            &[
                DataEvent::SubIntervalMeasured(maximum),
                DataEvent::SubIntervalMeasured(measurement(2, 90_000_000, 1000, 0)),
            ],
            None,
            &Ending {
                end: Some(EndReason::Completed),
                failure: None,
                socket_buffers: None,
            },
        );
        let (text, _) = render(&report(&measurements, &warnings, &[], None));
        assert!(text.contains("RTT minimum 23 ms"), "{text}");
        assert!(text.contains("RTT minimum 10 ms"), "{text}");
    }

    // docs/spec.md "Results and output": large measurements keep their full
    // values, missing measurements stay distinct from zero, and a Test
    // without a valid Maximum IP-Layer Capacity says so at every width.
    #[test]
    fn huge_values_and_absent_measurements_are_never_clipped() {
        let mut measured = measurement(1, u64::MAX, u32::MAX, u32::MAX);
        measured.out_of_order = u32::MAX;
        measured.duplicates = u32::MAX;
        measured.delay_variation = None;
        measured.rtt_variation = None;
        measured.rtt_variation_mean = None;
        let (measurements, warnings) = self::measured(
            &[DataEvent::SubIntervalMeasured(measured)],
            None,
            &Ending {
                end: Some(EndReason::Stopped),
                failure: None,
                socket_buffers: None,
            },
        );
        for width in [60, 80, 100, 120] {
            let (text, invalid) = render_at(
                &report(&measurements, &warnings, &[], None),
                width,
                Palette::plain(),
            );
            assert!(invalid, "{text}");
            assert!(text.contains(&mbps(u64::MAX)), "{text}");
            assert!(text.matches("4294967295").count() >= 3, "{text}");
            assert!(text.contains("not measured"), "{text}");
            assert!(!text.contains('\u{1b}'));
            let prose = text.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(prose.contains("No Maximum IP-Layer Capacity"), "{text}");
        }
        let (measurements, warnings) = self::measured(
            &[],
            None,
            &Ending {
                end: Some(EndReason::Watchdog),
                failure: None,
                socket_buffers: None,
            },
        );
        let (text, invalid) = render_at(
            &report(&measurements, &warnings, &[], None),
            60,
            Palette::plain(),
        );
        assert!(invalid);
        assert!(
            text.split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .contains("no Sub-Interval was measured"),
            "{text}"
        );
    }

    #[test]
    fn utc_dates() {
        assert_eq!(utc(Duration::ZERO), "1970-01-01T00:00:00Z");
        assert_eq!(
            utc(Duration::from_secs(951_782_400)),
            "2000-02-29T00:00:00Z"
        );
        assert_eq!(
            utc(Duration::from_secs(1_790_000_000)),
            "2026-09-21T14:13:20Z"
        );
    }
}
