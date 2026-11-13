#![forbid(unsafe_code)]
//! The Client's results as JSON compatible with OB-UDPST v9.1.0's client
//! schema (docs/spec.md story 24): the same
//! keys, in the same order and printed forms, as OB-UDPST's formatted
//! output (`-f jsonf`), so TR-471 tooling keeps working. Fields udpstp
//! never measures carry OB-UDPST's "feature off" values: no ECN (CE counts
//! 0), no interface rates. Results OB-UDPST lacks go in one last top-level
//! object, `"udpstp"`, which `--json-extension=false` leaves out.
//!
//! Where udpstp differs, as decided on :
//! - `AtMax` and `ModalResult` describe the Maximum IP-Layer Capacity, which
//!   meets the Performance Criterion; the Highest IP-Layer Capacity, what
//!   OB-UDPST reports there, is in the extension. A Capacity Mode with no
//!   maximum has no `AtMax` (or `ModalResult`) and ErrorStatus 49.
//!   (conformance: json-at-max-is-the-maximum)
//! - Delay fields of a Sub-Interval without samples are 0, as OB-UDPST
//!   writes, and listed in the extension; `Summary` uses real samples only.
//!   (conformance: json-summary-delays-from-samples)
//! - A Sub-Interval never measured is left out of `IncrementalResult`, and
//!   listed in the extension; `Interval` is its subIntSeqNo.
//!   (conformance: json-missing-sub-intervals)
//! - `IPLayerCapacity` is over the nominal Sub-Interval (RFC 9097 §5.3; conformance: sub-interval-whole-trial-intervals).
//! - A Test that didn't complete (ErrorStatus 200 and up) has only the
//!   `Output` header and trailer, as in OB-UDPST; its results are in the
//!   extension's `SubIntervals` and `MinRTT`.
//!   (conformance: json-incomplete-test-results-in-extension)
//!
//! `docs/json-output.md` lists the fields and their fixed values.
//! `ClientIPAddress` follows `--address-privacy`.

use std::fmt::Write as _;
use std::net::SocketAddr;
use std::time::Duration;

use crate::protocol::{
    CapacityModeResult, Coercion, Direction, MeasurementResults, PerformanceCriterion,
    SubIntervalMeasurement, TestParameters,
};
use crate::runtime::EngineFacts;

use crate::color::Palette;
use crate::test_results::{Notice, NoticeOutput, Said, Stream, TestResults, Writer};
use crate::warnings::Warning;

/// The JSON results of a Test that ran: what went wrong on stderr, then
/// the JSON alone on stdout.
pub fn print(results: &TestResults<'_>, stderr: Palette) -> Vec<(Stream, String)> {
    let mut w = Writer::new();
    w.notices(
        &results.notices,
        NoticeOutput::Json,
        Palette::plain(),
        stderr,
    );
    let messages: Vec<String> = results
        .notices
        .iter()
        .filter(|notice| notice.said != Said::Progress)
        .map(Notice::message)
        .collect();
    let options = results.options;
    let first = &results.connections[results.first];
    let whole = results.whole.as_ref();
    let mut warnings = whole.map(|w| w.warnings.clone()).unwrap_or_default();
    if results.no_valid_maximum() {
        warnings.push(Warning::NoValidMaximum);
    }
    let connections: Vec<TestConnectionResult> = if options.test_connections > 1 {
        results
            .connections
            .iter()
            .enumerate()
            .map(|(index, c)| TestConnectionResult {
                mc_index: index as u8,
                server: c.server,
                end_reason: c.end.end_name(),
                control_phase_failure: c.end.control_phase_failure_name(),
                capacity_modes: c
                    .block
                    .as_ref()
                    .map(|b| b.measured.results.capacity_modes.clone())
                    .unwrap_or_default(),
            })
            .collect()
    } else {
        Vec::new()
    };
    let engine_facts = whole.map_or_else(EngineFacts::default, |w| w.engine_facts);
    let coercions = results.coercions();
    w.out(render(&JsonReport {
        addresses: &options.addresses,
        server: first.server,
        host: &options.hosts[0].0,
        local: first.local,
        mc_ident: results.mc_ident,
        test_port: first.test_port,
        direction: options.direction,
        // The DSCP asked for.
        parameters: whole.map(|w| TestParameters {
            dscp: options.dscp,
            ..w.parameters
        }),
        fixed_rate: options.fixed_rate,
        jumbo: options.jumbo,
        traditional_mtu: options.traditional_mtu,
        test_connections: options.test_connections,
        min_test_connections: options.minimum,
        hosts: &options.hosts,
        max_bandwidth: options.max_bandwidth,
        max_test_connections: u32::try_from(crate::protocol::DEFAULT_TEST_CONNECTIONS_PER_TEST)
            .unwrap_or(u32::MAX),
        first_mode: options.first_mode,
        criterion: options.criterion,
        measurements: whole.map(|w| &w.measured.results),
        sub_interval_times: whole.map_or(&[], |w| &w.measured.times),
        started: results.started,
        ended: results.ended,
        error_status: results.error_status(),
        completed: results.status.completed,
        notices: &messages,
        warnings: &warnings,
        end_reason: Some(results.end_name().into()),
        control_phase_failure: results.control_phase_failure_name(),
        coercions: &coercions,
        test_connection_results: &connections,
        dont_fragment: options.dont_fragment,
        engine_facts,
        extension: options.json_extension,
    }));
    w.into_output()
}

/// OB-UDPST's `Metrics`: we measure all five.
const METRICS: &str = "IPLR,Sampled_RTT,IPDV,IPRR,RIPR";

/// OB-UDPST's fixed timers, which its Input and Output echo: the 1 s
/// traffic-stopped timer (Tmax in its terms) and the 3 s Watchdog.
const TMAX_MS: i64 = 1000;
const TMAX_RTT_MS: i64 = 3000;

/// The Fast Mode ceiling, bit/s (Sending Rate Table row 1000).
const HIGH_SPEED_THRESHOLD: i64 = 1_000_000_000;

/// Everything the JSON results need.
pub struct JsonReport<'a> {
    /// How this Client's own address is shown.
    pub addresses: &'a crate::privacy::Addresses,
    pub server: SocketAddr,
    /// The Server as the user typed it.
    pub host: &'a str,
    pub local: Option<SocketAddr>,
    pub mc_ident: u16,
    /// The Test Connection's Ephemeral Port on the Server.
    pub test_port: Option<u16>,
    pub direction: Direction,
    /// The Activation Response's parameters, if the Test was activated.
    pub parameters: Option<TestParameters>,
    pub fixed_rate: bool,
    /// The Sending Rate Table variant asked for (RFC 9946 §6.1).
    pub jumbo: bool,
    pub traditional_mtu: bool,
    /// The Test's Test Connections and Minimum Test Connections, and its
    /// Servers as typed, with their addresses.
    pub test_connections: u8,
    pub min_test_connections: u8,
    pub hosts: &'a [(String, SocketAddr)],
    pub max_bandwidth: u16,
    pub max_test_connections: u32,
    pub first_mode: Option<u32>,
    pub criterion: PerformanceCriterion,
    /// The Test's measurement results, once it was activated.
    pub measurements: Option<&'a MeasurementResults>,
    /// When this host learned each Sub-Interval's results, by
    /// subIntSeqNo from 1, since the Unix epoch.
    pub sub_interval_times: &'a [Option<Duration>],
    /// T0 and the end, since the Unix epoch.
    pub started: Option<Duration>,
    pub ended: Duration,
    pub error_status: u8,
    /// Whether the Test completed: one that didn't has no results in
    /// Output, as in OB-UDPST; they are in the extension.
    pub completed: bool,
    /// The notices, oldest first: the last two become ErrorMessage2 and
    /// ErrorMessage.
    pub notices: &'a [String],
    /// Extension fields.
    pub warnings: &'a [Warning],
    pub end_reason: Option<String>,
    /// Why the Control Phase failed, if it did.
    pub control_phase_failure: Option<&'static str>,
    /// Every Test Connection's coercions, by mcIndex.
    pub coercions: &'a [(u8, Coercion)],
    /// A Multi-Connection Test's Test Connections, each on its own; empty
    /// for a single one.
    pub test_connection_results: &'a [TestConnectionResult],
    pub dont_fragment: bool,
    /// How the Test ran on this host, as its Data Phase Engines reported:
    /// the socket buffers, the fast paths and, when asked for, the
    /// kernel send timestamps' lag behind lpduTime (experimental).
    pub engine_facts: EngineFacts,
    pub extension: bool,
}

/// How one Test Connection of a Multi-Connection Test ended, with its own
/// Capacity Modes: what the text results show of each.
pub struct TestConnectionResult {
    pub mc_index: u8,
    pub server: SocketAddr,
    /// How it ended, as `EndReason` names it.
    pub end_reason: &'static str,
    pub control_phase_failure: Option<&'static str>,
    /// Empty if it was never activated.
    pub capacity_modes: Vec<CapacityModeResult>,
}

/// A JSON value, printed the way OB-UDPST's cJSON prints it.
enum V {
    /// Printed as is: a number already in its form.
    Raw(String),
    Str(String),
    Obj(Vec<(&'static str, V)>),
    Arr(Vec<V>),
}

fn int(n: impl Into<i64>) -> V {
    V::Raw(n.into().to_string())
}

fn str_or_null(s: Option<&str>) -> V {
    s.map_or_else(|| V::Raw("null".into()), |s| V::Str(s.into()))
}

/// A count that may exceed i64, printed as is.
fn count(n: u64) -> V {
    V::Raw(n.to_string())
}

/// A number with `places` decimals, `0.0` when zero (cJSON's rule).
fn fixed(x: f64, places: usize) -> V {
    if x == 0.0 {
        V::Raw("0.0".into())
    } else {
        V::Raw(format!("{x:.places$}"))
    }
}

/// Whole milliseconds as seconds, printed exactly: 11 ms is `0.011`,
/// 1000 ms is `1.0` (cJSON's `%.9g`, with `.0` added to a whole number).
fn seconds(ms: i64) -> V {
    let sign = if ms < 0 { "-" } else { "" };
    let (whole, frac) = (ms.unsigned_abs() / 1000, ms.unsigned_abs() % 1000);
    let frac = format!("{frac:03}");
    let frac = frac.trim_end_matches('0');
    V::Raw(if frac.is_empty() {
        format!("{sign}{whole}.0")
    } else {
        format!("{sign}{whole}.{frac}")
    })
}

fn mbps(bits_per_second: u64) -> V {
    fixed(bits_per_second as f64 / 1e6, 2)
}

fn ratio(part: u64, whole: u64) -> V {
    if whole == 0 {
        fixed(0.0, 9)
    } else {
        fixed(part as f64 / whole as f64, 9)
    }
}

fn percent(part: u64, whole: u64) -> V {
    if whole == 0 {
        fixed(0.0, 2)
    } else {
        fixed(part as f64 * 100.0 / whole as f64, 2)
    }
}

/// A time as UTC with microseconds, as OB-UDPST writes every time.
fn time(since_epoch: Duration) -> V {
    let second = crate::results::utc(Duration::from_secs(since_epoch.as_secs()));
    let micros = since_epoch.subsec_micros();
    V::Str(second.replace('Z', &format!(".{micros:06}Z")))
}

fn write(value: &V, depth: usize, out: &mut String) {
    match value {
        V::Raw(raw) => out.push_str(raw),
        V::Str(s) => out.push_str(&serde_json::to_string(s).expect("a string serializes")),
        V::Obj(entries) => {
            out.push_str("{\n");
            for (index, (key, value)) in entries.iter().enumerate() {
                out.push_str(&"\t".repeat(depth + 1));
                let _ = write!(out, "\"{key}\":\t");
                write(value, depth + 1, out);
                out.push_str(if index + 1 < entries.len() {
                    ",\n"
                } else {
                    "\n"
                });
            }
            out.push_str(&"\t".repeat(depth));
            out.push('}');
        }
        V::Arr(values) => {
            out.push('[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                write(value, depth + 1, out);
            }
            out.push(']');
        }
    }
}

/// The results as OB-UDPST-compatible JSON, with a final newline.
pub fn render(report: &JsonReport<'_>) -> String {
    let mut top = vec![
        ("IPLayerMaxConnections", int(report.max_test_connections)),
        // The most Sub-Intervals any Test Interval can hold: 65535 s of
        // 100 ms.
        ("IPLayerMaxIncrementalResult", int(655_350)),
        (
            "IPLayerCapSupported",
            V::Obj(vec![
                (
                    "SoftwareVersion",
                    V::Str(format!("udpstp {}", env!("CARGO_PKG_VERSION"))),
                ),
                ("ControlProtocolVersion", int(20)),
                ("Metrics", V::Str(METRICS.into())),
            ]),
        ),
    ];
    let mut extension = Vec::new();
    if let (Some(parameters), Some(measurements)) = (&report.parameters, report.measurements) {
        top.push(("Input", input(report, parameters)));
        let (output, ext) = output(report, parameters, measurements);
        top.push(("Output", output));
        extension = ext;
    }
    let messages = report.notices;
    let last = |back: usize| {
        messages
            .len()
            .checked_sub(back)
            .map_or_else(String::new, |i| messages[i].clone())
    };
    top.push(("ErrorStatus", int(report.error_status)));
    top.push(("ErrorMessage", V::Str(last(1))));
    top.push(("ErrorMessage2", V::Str(last(2))));
    if report.extension {
        let mut ours = vec![
            (
                "Warnings",
                V::Arr(
                    report
                        .warnings
                        .iter()
                        .map(|w| {
                            V::Obj(vec![
                                ("Code", V::Str(w.code().into())),
                                ("Count", count(w.count())),
                                ("Message", V::Str(w.message())),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("EndReason", str_or_null(report.end_reason.as_deref())),
            (
                "ControlPhaseFailure",
                str_or_null(report.control_phase_failure),
            ),
            (
                "Coercions",
                V::Arr(
                    report
                        .coercions
                        .iter()
                        .map(|(mc_index, c)| {
                            V::Obj(vec![
                                ("McIndex", int(*mc_index)),
                                (
                                    "Parameter",
                                    V::Str(crate::names::parameter(c.parameter).into()),
                                ),
                                ("Requested", int(c.requested)),
                                ("Coerced", int(c.coerced)),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("DontFragment", V::Raw(report.dont_fragment.to_string())),
            (
                "SocketBuffers",
                report.engine_facts.socket_buffers.map_or_else(
                    || V::Raw("null".into()),
                    |b| {
                        V::Obj(vec![
                            ("Requested", int(b.requested as i64)),
                            ("Receive", int(b.receive as i64)),
                            ("Send", int(b.send as i64)),
                        ])
                    },
                ),
            ),
            (
                "DataPhaseEngine",
                V::Str(crate::names::data_phase_engine::PORTABLE.into()),
            ),
            (
                "FastPaths",
                V::Arr(
                    crate::names::fast_paths(report.engine_facts.fast_paths)
                        .into_iter()
                        .map(|name| V::Str(name.into()))
                        .collect(),
                ),
            ),
        ];
        // Only with UDPSTP_KERNEL_TX_TIMESTAMPS set to 1 or kernel-count:
        // experimental, not a stable interface (ADR 0013).
        if let Some(lag) = report.engine_facts.send_lag {
            let us = |d: std::time::Duration| int(i64::try_from(d.as_micros()).unwrap_or(i64::MAX));
            ours.push((
                "KernelSendTimestamps",
                V::Obj(vec![
                    (
                        "Samples",
                        int(i64::try_from(lag.samples).unwrap_or(i64::MAX)),
                    ),
                    ("LagP50Us", us(lag.p50)),
                    ("LagP99Us", us(lag.p99)),
                    ("LagMaxUs", us(lag.max)),
                ]),
            ));
        }
        // Where the Client was the Load Sender: experimental, not a stable
        // interface.
        if let Some(late) = report.engine_facts.burst_lateness {
            let us = |d: std::time::Duration| int(i64::try_from(d.as_micros()).unwrap_or(i64::MAX));
            ours.push((
                "BurstLateness",
                V::Obj(vec![
                    (
                        "Samples",
                        int(i64::try_from(late.samples).unwrap_or(i64::MAX)),
                    ),
                    ("P50Us", us(late.p50)),
                    ("P99Us", us(late.p99)),
                    ("MaxUs", us(late.max)),
                ]),
            ));
        }
        ours.extend(extension);
        if !report.test_connection_results.is_empty() {
            ours.push((
                "TestConnections",
                V::Arr(
                    report
                        .test_connection_results
                        .iter()
                        .map(|c| {
                            V::Obj(vec![
                                ("McIndex", int(c.mc_index)),
                                ("Server", V::Str(c.server.to_string())),
                                ("EndReason", V::Str(c.end_reason.into())),
                                ("ControlPhaseFailure", str_or_null(c.control_phase_failure)),
                                ("CapacityModes", capacity_modes_json(&c.capacity_modes)),
                            ])
                        })
                        .collect(),
                ),
            ));
        }
        top.push(("udpstp", V::Obj(ours)));
    }
    let mut out = String::new();
    write(&V::Obj(top), 0, &mut out);
    out.push('\n');
    out
}

/// The Server as typed, without a port: OB-UDPST's `Host`.
fn host_name(typed: &str) -> String {
    if let Ok(addr) = typed.parse::<SocketAddr>() {
        return addr.ip().to_string();
    }
    let bare = typed.trim_start_matches('[').trim_end_matches(']');
    if bare.parse::<std::net::IpAddr>().is_ok() {
        return bare.to_owned();
    }
    match typed.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') && port.parse::<u16>().is_ok() => host.to_owned(),
        _ => typed.to_owned(),
    }
}

fn input(report: &JsonReport<'_>, p: &TestParameters) -> V {
    let ipv4 = report.server.ip().to_canonical().is_ipv4();
    // The largest UDP payload the variant allows, IPv4-based.
    let max_payload: i64 = match (report.jumbo, report.traditional_mtu) {
        (true, _) => 8972,
        (false, true) => 1472,
        (false, false) => 1222,
    };
    let less = if ipv4 { 0 } else { 20 };
    let host = V::Str(host_name(report.host));
    let host_ip = V::Str(report.server.ip().to_string());
    let (start, index) = match (report.fixed_rate, p.sr_index_is_start) {
        (true, _) => (i64::from(p.sr_index_conf), i64::from(p.sr_index_conf)),
        (false, true) => (i64::from(p.sr_index_conf), -1),
        (false, false) => (0, -1),
    };
    let expected = report
        .measurements
        .map_or(0, |m| m.sub_intervals.len() as i64);
    V::Obj(vec![
        ("Interface", V::Str(String::new())),
        (
            "Role",
            V::Str(
                match report.direction {
                    Direction::Upstream => "Sender",
                    Direction::Downstream => "Receiver",
                }
                .into(),
            ),
        ),
        ("ID", int(report.mc_ident)),
        ("Host", host),
        ("HostIPAddress", host_ip),
        ("Port", int(report.test_port.unwrap_or(0))),
        ("NumberOfHosts", int(report.hosts.len() as i64)),
        (
            "HostList",
            V::Arr(
                report
                    .hosts
                    .iter()
                    .map(|(host, addr)| {
                        V::Obj(vec![
                            ("Host", V::Str(host_name(host))),
                            ("HostIPAddress", V::Str(addr.ip().to_string())),
                            ("ControlPort", int(addr.port())),
                        ])
                    })
                    .collect(),
            ),
        ),
        // The Client's socket is bound to a wildcard address, which says
        // nothing: then empty.
        (
            "ClientIPAddress",
            V::Str(
                report
                    .local
                    .filter(|l| !l.ip().is_unspecified())
                    .map_or_else(String::new, |l| report.addresses.show_ip(l.ip())),
            ),
        ),
        ("ClientPort", int(report.local.map_or(0, |l| l.port()))),
        ("JumboFramesPermitted", int(i64::from(report.jumbo))),
        ("NumberOfConnections", int(report.test_connections)),
        ("MinNumOfConnections", int(report.min_test_connections)),
        ("DSCP", int(p.dscp)),
        ("ECN", int(0)),
        ("CEThreshold", int(0)),
        ("ProtocolVersion", V::Str("Any".into())),
        // A Load PDU header plus the 20 bytes an IPv6 sender takes off.
        ("UDPPayloadMin", int(52 - less)),
        ("UDPPayloadMax", int(max_payload - less)),
        (
            "UDPPayloadDefault",
            int(if report.traditional_mtu { 1472 } else { 1222 } - less),
        ),
        (
            "UDPPayloadContent",
            V::Str(if p.random_payload { "random" } else { "zeroes" }.into()),
        ),
        (
            "TestType",
            V::Str(if report.fixed_rate { "Fixed" } else { "Search" }.into()),
        ),
        ("IPDVEnable", int(i64::from(p.one_way_delay_variation))),
        ("IPRREnable", int(1)),
        ("RIPREnable", int(1)),
        ("PreambleDuration", int(0)),
        ("MaxRequiredBandwidth", int(report.max_bandwidth)),
        ("StartSendingRateIndex", int(start)),
        ("SendingRateIndex", int(index)),
        ("NumberTestSubIntervals", int(expected)),
        (
            "NumberFirstModeTestSubIntervals",
            int(report.first_mode.unwrap_or(0)),
        ),
        ("NumberFirstModeSuppSubIntervals", int(0)),
        ("TestSubInterval", int(p.sub_interval.as_millis() as i64)),
        (
            "StatusFeedbackInterval",
            int(p.trial_interval.as_millis() as i64),
        ),
        ("TimeoutNoTestTraffic", int(TMAX_MS)),
        ("TimeoutNoStatusMessage", int(TMAX_MS)),
        ("Tmax", int(TMAX_MS)),
        ("TmaxRTT", int(TMAX_RTT_MS)),
        ("TimestampResolution", int(1)),
        ("SeqErrThresh", int(p.seq_err_thresh)),
        ("ReordDupIgnoreEnable", int(i64::from(p.ignore_ooo_dup))),
        ("LowerThresh", int(p.low_thresh)),
        ("UpperThresh", int(p.upper_thresh)),
        ("HighSpeedDelta", int(p.high_speed_delta)),
        ("SlowAdjThresh", int(p.slow_adj_thresh)),
        ("HSpeedThresh", int(HIGH_SPEED_THRESHOLD)),
        (
            "RateAdjAlgorithm",
            V::Str(if p.rate_adj_algo == 1 { "C" } else { "B" }.into()),
        ),
        ("InterfaceDeterminesMax", int(0)),
    ])
}

/// The Output object, and the extension's result fields.
fn output(
    report: &JsonReport<'_>,
    p: &TestParameters,
    measurements: &MeasurementResults,
) -> (V, Vec<(&'static str, V)>) {
    let ipv4 = report.server.ip().to_canonical().is_ipv4();
    let modes = &measurements.capacity_modes;
    let at = |seq: u32| {
        (seq as usize)
            .checked_sub(1)
            .and_then(|i| report.sub_interval_times.get(i).copied().flatten())
            .unwrap_or(report.ended)
    };
    let estimated = measurements.sender_estimated;
    let mut fields = vec![
        ("BOMTime", time(report.started.unwrap_or(report.ended))),
        ("TmaxUsed", int(TMAX_MS)),
        ("TestInterval", int(p.test_interval.as_secs() as i64)),
        ("TmaxRTTUsed", int(TMAX_RTT_MS)),
        ("TimestampResolutionUsed", int(1)),
    ];
    let slots = &measurements.sub_intervals;
    let incremental = || {
        V::Arr(
            slots
                .iter()
                .flatten()
                .map(|m| sub_interval(m, at(m.sub_int_seq_no), report.active(Some(m))))
                .collect(),
        )
    };
    let mut no_samples = Vec::new();
    // A Capacity Mode with nothing measured, such as a second one the Test
    // ended before: OB-UDPST then leaves out the modal results.
    let modal = modes
        .iter()
        .skip(1)
        .all(|mode| (mode.sub_intervals.end() - mode.sub_intervals.start() + 1) > mode.missing);
    if report.completed && slots.iter().any(Option::is_some) {
        fields.push(("IncrementalResult", incremental()));
        let summaries: Vec<V> = modes
            .iter()
            .enumerate()
            .map(|(i, mode)| {
                let last = slots
                    .iter()
                    .flatten()
                    .rfind(|m| mode.sub_intervals.contains(&m.sub_int_seq_no));
                summary(i + 1, mode, slots, report.active(last))
            })
            .collect();
        let mut summaries = summaries.into_iter();
        fields.push((
            "Summary",
            summaries.next().expect("one Capacity Mode at least"),
        ));
        if modal {
            fields.push(("ModalSummary", V::Arr(summaries.collect())));
        }
        // The Maximum IP-Layer Capacity of each Capacity Mode; one without
        // leaves AtMax, or ModalResult, out.
        let maxima: Vec<Option<V>> = modes
            .iter()
            .enumerate()
            .map(|(i, mode)| {
                mode.maximum.map(|m| {
                    at_max(
                        i + 1,
                        mode,
                        &m,
                        at(m.sub_int_seq_no),
                        report.active(Some(&m)),
                        p,
                        ipv4,
                    )
                })
            })
            .collect();
        let mut maxima = maxima.into_iter();
        if let Some(Some(first)) = maxima.next() {
            fields.push(("AtMax", first));
        }
        let rest: Vec<Option<V>> = maxima.collect();
        if modal {
            if rest.iter().all(Option::is_some) {
                fields.push(("ModalResult", V::Arr(rest.into_iter().flatten().collect())));
            }
            fields.push(("ECNBleachingDetected", int(0)));
        }
    }
    fields.push(("EOMTime", time(report.ended)));
    fields.push((
        "Status",
        V::Str(
            if report.error_status == 0 {
                "Complete"
            } else {
                "Error_Other"
            }
            .into(),
        ),
    ));
    // The extension's results.
    let sent_per_sub = &measurements.sender_bit_rates;
    for m in slots.iter().flatten() {
        if m.delay_variation.is_none() || m.rtt_variation.is_none() {
            no_samples.push(int(m.sub_int_seq_no));
        }
    }
    let missing: Vec<V> = slots
        .iter()
        .enumerate()
        .filter(|(_, s)| s.is_none())
        .map(|(i, _)| int(i as i64 + 1))
        .collect();
    let sender: Vec<V> = sent_per_sub
        .iter()
        .enumerate()
        .filter_map(|(i, s)| {
            s.map(|s| {
                V::Obj(vec![
                    ("SubInterval", int(i as i64 + 1)),
                    (
                        // A Load Receiver's is an estimate.
                        if estimated {
                            "IPLayerSenderBitRateEstimate"
                        } else {
                            "IPLayerSenderBitRate"
                        },
                        mbps(s.ip_layer_sender_bit_rate),
                    ),
                ])
            })
        })
        .collect();
    let mut extension = vec![
        (
            "PerformanceCriterion",
            V::Obj(vec![(
                "MaxLossRatio",
                fixed(f64::from(report.criterion.max_loss_ppm) / 1e6, 9),
            )]),
        ),
        ("CapacityModes", capacity_modes_json(modes)),
        ("MissingSubIntervals", V::Arr(missing)),
        ("NoSamples", V::Arr(no_samples)),
        ("IPLayerSenderBitRate", V::Arr(sender)),
    ];
    if !report.completed {
        // Preserve the measurements without changing OB-UDPST's incomplete
        // Output (conformance: json-incomplete-test-results-in-extension).
        extension.push(("SubIntervals", incremental()));
        extension.push((
            "MinRTT",
            slots
                .iter()
                .flatten()
                .filter_map(|m| m.rtt_minimum)
                .min()
                .map_or_else(|| V::Raw("null".into()), |ms| seconds(ms.into())),
        ));
    }
    (V::Obj(fields), extension)
}

/// Each Capacity Mode's Sub-Intervals, whether it has a valid result, and
/// its Maximum and Highest IP-Layer Capacity.
fn capacity_modes_json(modes: &[CapacityModeResult]) -> V {
    let pick = |m: &Option<SubIntervalMeasurement>| {
        m.map_or_else(
            || V::Raw("null".into()),
            |m| {
                V::Obj(vec![
                    ("SubInterval", int(m.sub_int_seq_no)),
                    ("IPLayerCapacity", mbps(m.ip_layer_capacity)),
                    ("LossRatio", ratio(m.loss.into(), m.datagrams_sent())),
                ])
            },
        )
    };
    V::Arr(
        modes
            .iter()
            .enumerate()
            .map(|(i, mode)| {
                V::Obj(vec![
                    ("Mode", int(i as i64 + 1)),
                    (
                        "SubIntervals",
                        V::Arr(vec![
                            int(*mode.sub_intervals.start()),
                            int(*mode.sub_intervals.end()),
                        ]),
                    ),
                    ("Valid", V::Raw(mode.maximum.is_some().to_string())),
                    ("MaximumIPLayerCapacity", pick(&mode.maximum)),
                    ("HighestIPLayerCapacity", pick(&mode.highest)),
                ])
            })
            .collect(),
    )
}

impl JsonReport<'_> {
    /// Test Connections active in a Sub-Interval: always 1 with one
    /// Test Connection, as OB-UDPST; in a Multi-Connection Test, those
    /// that received in it (for a Summary, in its mode's last).
    fn active(&self, m: Option<&SubIntervalMeasurement>) -> i64 {
        match self.test_connections {
            1 => 1,
            _ => m.map_or(0, |m| m.active_test_connections.into()),
        }
    }
}

fn sub_interval(m: &SubIntervalMeasurement, at: Duration, active: i64) -> V {
    let sent = m.datagrams_sent();
    let (pdv_min, pdv_avg, pdv_max) = m
        .delay_variation
        .map_or((0, 0, 0), |d| (d.min.into(), d.mean.into(), d.max.into()));
    let (rtt_min, rtt_max) = m
        .rtt_variation
        .map_or((0, 0), |(l, h)| (l.into(), h.into()));
    V::Obj(vec![
        ("Interval", int(m.sub_int_seq_no)),
        ("Seconds", fixed(m.accum_time.as_secs_f64(), 1)),
        ("TimeOfSubInterval", time(at)),
        ("ActiveConnections", int(active)),
        ("DeliveredPercent", percent(m.datagrams.into(), sent)),
        ("LossRatio", ratio(m.loss.into(), sent)),
        ("ReorderedRatio", ratio(m.out_of_order.into(), sent)),
        ("ReplicatedRatio", ratio(m.duplicates.into(), sent)),
        ("CEPercentOfDelivered", fixed(0.0, 2)),
        ("LossCount", int(m.loss)),
        ("ReorderedCount", int(m.out_of_order)),
        ("ReplicatedCount", int(m.duplicates)),
        ("CECountOfDelivered", int(0)),
        ("PDVMin", seconds(pdv_min)),
        ("PDVAvg", seconds(pdv_avg)),
        ("PDVMax", seconds(pdv_max)),
        ("PDVRange", seconds(pdv_max - pdv_min)),
        ("RTTMin", seconds(rtt_min)),
        ("RTTAvg", seconds(m.rtt_variation_mean.map_or(0, i64::from))),
        ("RTTMax", seconds(rtt_max)),
        ("RTTRange", seconds(rtt_max - rtt_min)),
        ("IPLayerCapacity", mbps(m.ip_layer_capacity)),
        ("InterfaceEthMbps", fixed(0.0, 2)),
        (
            "MinOnewayDelay",
            seconds(i64::from(m.clock_delta_min.unwrap_or(0)) + pdv_min),
        ),
    ])
}

/// A Capacity Mode's totals, from the Sub-Intervals it measured; delay
/// figures from real samples only.
fn summary(
    mode_number: usize,
    mode: &CapacityModeResult,
    slots: &[Option<SubIntervalMeasurement>],
    active: i64,
) -> V {
    let start = *mode.sub_intervals.start() as usize;
    let end = *mode.sub_intervals.end() as usize;
    let ms: Vec<&SubIntervalMeasurement> = slots
        .get(start.saturating_sub(1)..end)
        .unwrap_or_default()
        .iter()
        .flatten()
        .collect();
    let sum = |f: fn(&SubIntervalMeasurement) -> u64| ms.iter().map(|m| f(m)).sum::<u64>();
    let (received, lost, ooo, dup) = (
        sum(|m| m.datagrams.into()),
        sum(|m| m.loss.into()),
        sum(|m| m.out_of_order.into()),
        sum(|m| m.duplicates.into()),
    );
    let sent = received + lost;
    let delays: Vec<_> = ms.iter().filter_map(|m| m.delay_variation).collect();
    let pdv_min = delays.iter().map(|d| i64::from(d.min)).min().unwrap_or(0);
    let pdv_max = delays.iter().map(|d| i64::from(d.max)).max().unwrap_or(0);
    let pdv_avg = if delays.is_empty() {
        0
    } else {
        let total: i64 = delays.iter().map(|d| i64::from(d.mean)).sum();
        (total * 2 + delays.len() as i64) / (2 * delays.len() as i64)
    };
    let rtts: Vec<(u32, u32)> = ms.iter().filter_map(|m| m.rtt_variation).collect();
    let rtt_min = rtts.iter().map(|r| i64::from(r.0)).min().unwrap_or(0);
    let rtt_max = rtts.iter().map(|r| i64::from(r.1)).max().unwrap_or(0);
    let (rtt_sum, rtt_count) = ms.iter().fold((0u64, 0u64), |(s, c), m| {
        (
            s + m.rtt_variation_sum,
            c + u64::from(m.rtt_variation_count),
        )
    });
    let rtt_avg = if rtt_count == 0 {
        0
    } else {
        ((rtt_sum * 2 + rtt_count) / (2 * rtt_count)) as i64
    };
    let capacity = if ms.is_empty() {
        0
    } else {
        ms.iter().map(|m| m.ip_layer_capacity).sum::<u64>() / ms.len() as u64
    };
    let min_one_way = ms
        .iter()
        .filter_map(|m| m.clock_delta_min)
        .min()
        .unwrap_or(0);
    let min_rtt = ms.iter().filter_map(|m| m.rtt_minimum).min().unwrap_or(0);
    V::Obj(vec![
        ("Mode", int(mode_number as i64)),
        ("Intervals", int(ms.len() as i64)),
        ("ActiveConnections", int(active)),
        ("DeliveredPercent", percent(received, sent)),
        ("LossRatioSummary", ratio(lost, sent)),
        ("ReorderedRatioSummary", ratio(ooo, sent)),
        ("ReplicatedRatioSummary", ratio(dup, sent)),
        ("CEPercentOfDelivered", fixed(0.0, 2)),
        ("LossCount", int(lost as i64)),
        ("ReorderedCount", int(ooo as i64)),
        ("ReplicatedCount", int(dup as i64)),
        ("CECountOfDelivered", int(0)),
        ("PDVMin", seconds(pdv_min)),
        ("PDVAvg", seconds(pdv_avg)),
        ("PDVMax", seconds(pdv_max)),
        ("PDVRangeSummary", seconds(pdv_max - pdv_min)),
        ("RTTMin", seconds(rtt_min)),
        ("RTTAvg", seconds(rtt_avg)),
        ("RTTMax", seconds(rtt_max)),
        ("RTTRangeSummary", seconds(rtt_max - rtt_min)),
        ("IPLayerCapacitySummary", mbps(capacity)),
        ("InterfaceEthMbps", fixed(0.0, 2)),
        ("MinOnewayDelaySummary", seconds(min_one_way.into())),
        ("MinRTTSummary", seconds(min_rtt.into())),
    ])
}

/// The Sub-Interval of a Capacity Mode's Maximum IP-Layer Capacity.
fn at_max(
    mode_number: usize,
    mode: &CapacityModeResult,
    m: &SubIntervalMeasurement,
    at: Duration,
    active: i64,
    p: &TestParameters,
    ipv4: bool,
) -> V {
    let sent = m.datagrams_sent();
    let (pdv_min, pdv_avg, pdv_max) = m
        .delay_variation
        .map_or((0, 0, 0), |d| (d.min.into(), d.mean.into(), d.max.into()));
    let (rtt_min, rtt_max) = m
        .rtt_variation
        .map_or((0, 0), |(l, h)| (l.into(), h.into()));
    // The Ethernet rates add a header per datagram (14, with FCS 18, with a
    // VLAN tag 22), over the nominal Sub-Interval as the IP-Layer Capacity.
    let header = if ipv4 { 28 } else { 48 };
    let ethernet = |extra: u64| {
        let bytes = m.bytes + u64::from(m.datagrams) * (header + extra);
        let nanos = p.sub_interval.as_nanos().max(1);
        u64::try_from(u128::from(bytes) * 8 * 1_000_000_000 / nanos).unwrap_or(u64::MAX)
    };
    let intervals = (mode.sub_intervals.end() - mode.sub_intervals.start() + 1) - mode.missing;
    V::Obj(vec![
        ("Mode", int(mode_number as i64)),
        ("Intervals", int(intervals)),
        ("TimeOfMax", time(at)),
        ("ActiveConnections", int(active)),
        ("DeliveredPercent", percent(m.datagrams.into(), sent)),
        ("LossRatioAtMax", ratio(m.loss.into(), sent)),
        ("ReorderedRatioAtMax", ratio(m.out_of_order.into(), sent)),
        ("ReplicatedRatioAtMax", ratio(m.duplicates.into(), sent)),
        ("CEPercentOfDelivered", fixed(0.0, 2)),
        ("LossCount", int(m.loss)),
        ("ReorderedCount", int(m.out_of_order)),
        ("ReplicatedCount", int(m.duplicates)),
        ("CECountOfDelivered", int(0)),
        ("PDVMin", seconds(pdv_min)),
        ("PDVAvg", seconds(pdv_avg)),
        ("PDVMax", seconds(pdv_max)),
        ("PDVRangeAtMax", seconds(pdv_max - pdv_min)),
        ("RTTMin", seconds(rtt_min)),
        ("RTTAvg", seconds(m.rtt_variation_mean.map_or(0, i64::from))),
        ("RTTMax", seconds(rtt_max)),
        ("RTTRangeAtMax", seconds(rtt_max - rtt_min)),
        ("MaxIPLayerCapacity", mbps(m.ip_layer_capacity)),
        ("InterfaceEthMbps", fixed(0.0, 2)),
        ("MaxETHCapacityNoFCS", mbps(ethernet(14))),
        ("MaxETHCapacityWithFCS", mbps(ethernet(18))),
        ("MaxETHCapacityWithFCSVLAN", mbps(ethernet(22))),
        (
            "MinOnewayDelayAtMax",
            seconds(i64::from(m.clock_delta_min.unwrap_or(0)) + pdv_min),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{DataEvent, DelayRange};
    use std::collections::{BTreeMap, BTreeSet};

    #[test]
    fn numbers_print_as_obudpst_prints_them() {
        let text = |v: V| {
            let mut out = String::new();
            write(&v, 0, &mut out);
            out
        };
        assert_eq!(text(seconds(11)), "0.011");
        assert_eq!(text(seconds(10)), "0.01");
        assert_eq!(text(seconds(1000)), "1.0");
        assert_eq!(text(seconds(0)), "0.0");
        assert_eq!(text(seconds(-5)), "-0.005");
        assert_eq!(text(fixed(0.0, 9)), "0.0");
        assert_eq!(text(ratio(1, 3)), "0.333333333");
        assert_eq!(text(percent(1, 1)), "100.00");
        assert_eq!(text(mbps(951_250_000)), "951.25");
        assert_eq!(
            text(time(Duration::new(1_790_000_000, 712_759_123))),
            "\"2026-09-21T14:13:20.712759Z\""
        );
    }

    fn measurement(seq: u32, capacity: u64, loss: u32) -> SubIntervalMeasurement {
        SubIntervalMeasurement {
            sub_int_seq_no: seq,
            datagrams: 1000,
            // The UDP payload that makes `capacity` over 1 s on IPv4.
            bytes: capacity / 8 - 1000 * 28,
            ip_layer_capacity: capacity,
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
            measured_at: crate::protocol::MonoTime::from_duration(Duration::ZERO),
            duration: Duration::from_secs(1),
            accum_time: Duration::from_secs(seq.into()),
        }
    }

    fn parameters(test_interval: u64) -> TestParameters {
        TestParameters {
            test_interval: Duration::from_secs(test_interval),
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

    struct Case<'a> {
        events: &'a [DataEvent],
        times: &'a [Duration],
        parameters: Option<TestParameters>,
        first_mode: Option<u32>,
        error_status: u8,
        notices: &'a [String],
        extension: bool,
        warnings: &'a [Warning],
        coercions: &'a [(u8, Coercion)],
        connections: &'a [TestConnectionResult],
    }

    fn render_case(case: &Case<'_>) -> String {
        render_case_with(case, &crate::privacy::Addresses::full())
    }

    fn render_case_with(case: &Case<'_>, addresses: &crate::privacy::Addresses) -> String {
        render_case_lagging(case, addresses, None)
    }

    /// A Test of these parameters, over IPv4, judged as by default.
    fn evaluation(p: &TestParameters, first_mode: Option<u32>) -> crate::protocol::Evaluation {
        crate::protocol::Evaluation {
            test_interval: p.test_interval,
            sub_interval: p.sub_interval,
            ip_version: crate::protocol::IpVersion::V4,
            criterion: PerformanceCriterion::DEFAULT,
            first_mode,
        }
    }

    fn render_case_lagging(
        case: &Case<'_>,
        addresses: &crate::privacy::Addresses,
        send_lag: Option<crate::runtime::SendLag>,
    ) -> String {
        let measurements = case.parameters.map(|p| {
            MeasurementResults::of_test_connection(case.events, &evaluation(&p, case.first_mode))
        });
        let times = crate::test_results::sub_interval_times(
            &[(case.events, case.times)],
            measurements.as_ref().map_or(0, |m| m.sub_intervals.len()),
        );
        render(&JsonReport {
            addresses,
            server: "192.0.2.1:24601".parse().unwrap(),
            host: "test-server1.example",
            test_connections: 1,
            min_test_connections: 1,
            hosts: &[(
                "test-server1.example".into(),
                "192.0.2.1:24601".parse().unwrap(),
            )],
            local: Some("198.51.100.2:40000".parse().unwrap()),
            mc_ident: 45366,
            test_port: Some(51646),
            direction: Direction::Upstream,
            parameters: case.parameters,
            fixed_rate: false,
            jumbo: true,
            traditional_mtu: false,
            max_bandwidth: 0,
            max_test_connections: 24,
            first_mode: case.first_mode,
            criterion: PerformanceCriterion::DEFAULT,
            measurements: measurements.as_ref(),
            sub_interval_times: &times,
            started: Some(Duration::from_secs(1_790_000_000)),
            ended: Duration::from_secs(1_790_000_016),
            error_status: case.error_status,
            completed: case.error_status < 50,
            notices: case.notices,
            warnings: case.warnings,
            end_reason: Some("Completed".into()),
            control_phase_failure: None,
            coercions: case.coercions,
            test_connection_results: case.connections,
            dont_fragment: true,
            engine_facts: EngineFacts {
                fast_paths: crate::runtime::FastPaths {
                    sendmmsg: true,
                    udp_gso: true,
                    recvmmsg: true,
                    kernel_rx_timestamps: true,
                    ppoll: true,
                    epoll: false,
                },
                send_lag,
                // Measured alike, for the tests here.
                burst_lateness: send_lag,
                ..EngineFacts::default()
            },
            extension: case.extension,
        })
    }

    /// Each key's path (its ancestors' keys) in order, with the shape of
    /// its value: a string, a time, a container, an integer, zero, or a
    /// number with so many decimals.
    fn shapes(json: &str) -> Vec<(String, String)> {
        // (indentation, key) of the containers open above each line.
        let mut open: Vec<(usize, String)> = Vec::new();
        let mut out = Vec::new();
        for line in json.lines() {
            let depth = line.len() - line.trim_start_matches('\t').len();
            let body = line.trim_start_matches('\t');
            let Some(rest) = body.strip_prefix('"') else {
                continue;
            };
            let Some((key, value)) = rest.split_once("\":\t") else {
                continue;
            };
            open.retain(|(d, _)| *d < depth);
            let path: Vec<&str> = open.iter().map(|(_, k)| k.as_str()).collect();
            let path = format!("/{}", [path, vec![key]].concat().join("/"));
            let value = value.trim_end_matches(',');
            let shape = if value.starts_with('"') {
                if value.ends_with("Z\"") && value.contains('T') {
                    "time".to_owned()
                } else {
                    "string".to_owned()
                }
            } else if value.starts_with('{') || value.starts_with('[') {
                "container".to_owned()
            } else if value == "0.0" {
                "zero".to_owned()
            } else if let Some((_, decimals)) = value.split_once('.') {
                format!("decimals{}", decimals.len())
            } else {
                "integer".to_owned()
            };
            out.push((path, shape));
            if value.starts_with('{') || value.starts_with('[') {
                open.push((depth, key.to_owned()));
            }
        }
        out
    }

    fn example(name: &str) -> String {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/testdata/obudpst-9.1.0/json_examples/"
        );
        std::fs::read_to_string(format!("{path}{name}"))
            .unwrap()
            .replace("\r\n", "\n")
    }

    /// Ours has OB-UDPST's keys in its order (each path once, since arrays
    /// repeat them), and every value has a shape OB-UDPST writes for that
    /// key. Zero prints as `0.0` in every number form, and a delay's
    /// decimals vary with its value.
    fn assert_matches_example(ours: &str, name: &str) {
        let theirs = shapes(&example(name));
        let ours: Vec<(String, String)> = shapes(ours)
            .into_iter()
            .filter(|(path, _)| !path.starts_with("/udpstp"))
            .collect();
        let order = |shapes: &[(String, String)]| {
            let mut seen: Vec<String> = Vec::new();
            for (path, _) in shapes {
                if !seen.contains(path) {
                    seen.push(path.clone());
                }
            }
            seen
        };
        assert_eq!(order(&ours), order(&theirs), "{name}: key order");
        let mut allowed: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for (path, shape) in &theirs {
            allowed.entry(path).or_default().insert(shape);
        }
        let number = |s: &str| s.starts_with("decimals") || s == "zero";
        for (path, shape) in &ours {
            let theirs = &allowed[path.as_str()];
            let decimals: BTreeSet<_> = theirs
                .iter()
                .filter(|s| s.starts_with("decimals"))
                .collect();
            let ok = theirs.contains(shape.as_str())
                || (shape == "zero" && theirs.iter().any(|s| number(s)))
                // A field OB-UDPST's example only shows as zero.
                || (number(shape) && theirs.iter().all(|s| *s == "zero"))
                // A delay, printed with as many decimals as its value needs.
                || (number(shape) && (decimals.len() > 1 || is_delay(path)));
            assert!(ok, "{name}: {path} is {shape}, OB-UDPST writes {theirs:?}");
        }
    }

    /// A delay field: seconds printed with as many decimals as needed.
    fn is_delay(path: &str) -> bool {
        let key = path.rsplit('/').next().unwrap_or_default();
        ["PDV", "RTT", "MinOneway", "MinRTT"]
            .iter()
            .any(|prefix| key.starts_with(prefix))
    }

    fn upstream_events(count: u32) -> (Vec<DataEvent>, Vec<Duration>) {
        let events: Vec<DataEvent> = (1..=count)
            .map(|seq| {
                DataEvent::SubIntervalMeasured(measurement(
                    seq,
                    900_000_000 + u64::from(seq) * 1_000_000,
                    if seq == 4 { 200 } else { 0 },
                ))
            })
            .collect();
        let times = (1..=u64::from(count))
            .map(|s| Duration::new(1_790_000_000 + s, 816_995_000))
            .collect();
        (events, times)
    }

    // docs/spec.md story 24 (JSON compatible with OB-UDPST's schema);
    #[test]
    fn a_completed_test_has_obudpst_keys_and_forms() {
        let (events, times) = upstream_events(10);
        let case = Case {
            events: &events,
            times: &times,
            parameters: Some(parameters(10)),
            first_mode: None,
            error_status: 0,
            notices: &[],
            extension: false,
            warnings: &[],
            coercions: &[],
            connections: &[],
        };
        // docs/spec.md story 82; : the Client's own address as configured.
        let truncated =
            crate::privacy::Addresses::new(crate::privacy::AddressPrivacy::Truncate, None)
                .expect("no key needed");
        let value: serde_json::Value =
            serde_json::from_str(&render_case_with(&case, &truncated)).unwrap();
        assert_eq!(value["Input"]["ClientIPAddress"], "198.51.100.0/24");
        let text = render_case(&case);
        assert_matches_example(&text, "client_example_01.json");
        serde_json::from_str::<serde_json::Value>(&text).unwrap();
        crate::results::tests::golden("completed.json", &text);
    }

    //
    #[udpstp_req::req("RFC9097-6.6-3", "RFC9097-9-6")]
    #[test]
    fn a_bimodal_test_has_obudpst_keys_and_forms() {
        let (events, times) = upstream_events(15);
        let text = render_case(&Case {
            events: &events,
            times: &times,
            parameters: Some(parameters(15)),
            first_mode: Some(5),
            error_status: 0,
            notices: &[],
            extension: true,
            warnings: &[],
            coercions: &[],
            connections: &[],
        });
        assert_matches_example(&text, "client_example_06.json");
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            value["udpstp"]["CapacityModes"].as_array().unwrap().len(),
            2
        );
        crate::results::tests::golden("bimodal.json", &text);
    }

    //  (ErrorStatus 200 and the notices when nothing was
    // activated)
    #[test]
    fn a_test_that_never_started_has_only_the_error() {
        let notices = [
            "no answer from 192.0.2.1:24601 within 3 s".to_owned(),
            "ERROR: the Test didn't complete: fewer than 1 Test Connection".to_owned(),
        ];
        let text = render_case(&Case {
            events: &[],
            times: &[],
            parameters: None,
            first_mode: None,
            error_status: 200,
            notices: &notices,
            extension: false,
            warnings: &[],
            coercions: &[],
            connections: &[],
        });
        assert_matches_example(&text, "client_example_12.json");
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["ErrorMessage"], notices[1]);
        assert_eq!(value["ErrorMessage2"], notices[0]);
    }

    //  (no valid Maximum IP-Layer Capacity: AtMax is left out,
    // never 0)
    #[test]
    fn without_a_maximum_at_max_is_left_out() {
        let events: Vec<DataEvent> = (1..=3)
            .map(|seq| DataEvent::SubIntervalMeasured(measurement(seq, 900_000_000, 500)))
            .collect();
        let text = render_case(&Case {
            events: &events,
            times: &[],
            parameters: Some(parameters(3)),
            first_mode: None,
            error_status: 49,
            notices: &[],
            extension: true,
            warnings: &[],
            coercions: &[],
            connections: &[],
        });
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(value["Output"].get("AtMax").is_none(), "{text}");
        assert!(value["Output"].get("Summary").is_some());
        assert_eq!(value["udpstp"]["CapacityModes"][0]["Valid"], false);
    }

    //  (a Test that didn't complete: only Output's header and
    // trailer, as OB-UDPST writes)
    #[test]
    fn a_test_that_failed_midway_has_no_results_in_output() {
        let (events, times) = upstream_events(3);
        let notices = [
            "WARNING: the Watchdog ended the Test".to_owned(),
            "ERROR: the Test didn't complete".to_owned(),
        ];
        let text = render_case(&Case {
            events: &events,
            times: &times,
            parameters: Some(parameters(10)),
            first_mode: None,
            error_status: 200,
            notices: &notices,
            extension: false,
            warnings: &[],
            coercions: &[],
            connections: &[],
        });
        assert_matches_example(&text, "client_example_11.json");
    }

    /// Each key of a JSON object, in order of name, with its value's type.
    fn keys(value: &serde_json::Value) -> Vec<String> {
        value
            .as_object()
            .unwrap_or_else(|| panic!("an object: {value}"))
            .iter()
            .map(|(key, value)| {
                let kind = match value {
                    serde_json::Value::Null => "null",
                    serde_json::Value::Bool(_) => "bool",
                    serde_json::Value::Number(n) if n.is_u64() => "integer",
                    serde_json::Value::Number(_) => "number",
                    serde_json::Value::String(_) => "string",
                    serde_json::Value::Array(_) => "array",
                    serde_json::Value::Object(_) => "object",
                };
                format!("{key}:{kind}")
            })
            .collect()
    }

    // : kernel send timestamps' lag, only when asked for, in the
    // extension (experimental, ADR 0013)
    #[test]
    fn kernel_send_timestamps_appear_only_when_measured() {
        let case = Case {
            events: &[],
            times: &[],
            parameters: None,
            first_mode: None,
            error_status: 0,
            notices: &[],
            extension: true,
            warnings: &[],
            coercions: &[],
            connections: &[],
        };
        let full = crate::privacy::Addresses::full();
        assert!(!render_case_lagging(&case, &full, None).contains("KernelSendTimestamps"));
        let lag = crate::runtime::SendLag {
            samples: 4000,
            p50: Duration::from_micros(12),
            p99: Duration::from_micros(41),
            max: Duration::from_micros(230),
        };
        let json: serde_json::Value =
            serde_json::from_str(&render_case_lagging(&case, &full, Some(lag))).unwrap();
        let stamps = &json["udpstp"]["KernelSendTimestamps"];
        assert_eq!(
            (
                &stamps["Samples"],
                &stamps["LagP50Us"],
                &stamps["LagP99Us"],
                &stamps["LagMaxUs"]
            ),
            (&4000.into(), &12.into(), &41.into(), &230.into())
        );
        // : the Load Sender's Burst lateness, when it has one.
        let late = &json["udpstp"]["BurstLateness"];
        assert_eq!(
            (
                &late["Samples"],
                &late["P50Us"],
                &late["P99Us"],
                &late["MaxUs"]
            ),
            (&4000.into(), &12.into(), &41.into(), &230.into())
        );
    }

    //  (the JSON results are a strict superset of the text results:
    // warnings as codes, coercions, each Test Connection of several)
    #[test]
    fn the_extension_has_warnings_coercions_and_test_connections() {
        let (events, times) = upstream_events(3);
        let warnings = [
            Warning::Counted {
                what: crate::warnings::Counted::BurstsSkipped,
                count: 3,
            },
            Warning::NoValidMaximum,
        ];
        let coercions = [(
            1,
            Coercion {
                parameter: crate::protocol::ActivationParameter::TestIntTime,
                requested: 120,
                coerced: 60,
            },
        )];
        let connections = [
            TestConnectionResult {
                mc_index: 0,
                server: "192.0.2.1:24601".parse().unwrap(),
                end_reason: "Completed",
                control_phase_failure: None,
                capacity_modes: MeasurementResults::of_test_connection(
                    &events,
                    &evaluation(&parameters(3), None),
                )
                .capacity_modes,
            },
            TestConnectionResult {
                mc_index: 1,
                server: "192.0.2.2:24601".parse().unwrap(),
                end_reason: "ControlPhaseFailed",
                control_phase_failure: Some("TestInitiationTimedOut"),
                capacity_modes: Vec::new(),
            },
        ];
        let text = render_case(&Case {
            events: &events,
            times: &times,
            parameters: Some(parameters(3)),
            first_mode: None,
            error_status: 0,
            notices: &[],
            extension: true,
            warnings: &warnings,
            coercions: &coercions,
            connections: &connections,
        });
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        let ours = &value["udpstp"];
        assert_eq!(
            ours["Warnings"]
                .as_array()
                .unwrap()
                .iter()
                .map(|w| (keys(w), w["Code"].as_str().unwrap()))
                .collect::<Vec<_>>(),
            [
                (
                    vec!["Code:string", "Count:integer", "Message:string"]
                        .into_iter()
                        .map(String::from)
                        .collect::<Vec<_>>(),
                    "BurstsSkipped"
                ),
                (
                    ["Code:string", "Count:integer", "Message:string"]
                        .into_iter()
                        .map(String::from)
                        .collect(),
                    "NoValidMaximum"
                ),
            ]
        );
        assert_eq!(ours["Warnings"][0]["Count"], 3);
        assert_eq!(
            keys(&ours["Coercions"][0]),
            [
                "Coerced:integer",
                "McIndex:integer",
                "Parameter:string",
                "Requested:integer"
            ]
        );
        assert_eq!(ours["Coercions"][0]["Parameter"], "testIntTime");
        assert_eq!(ours["ControlPhaseFailure"], serde_json::Value::Null);
        let each = ours["TestConnections"].as_array().unwrap();
        assert_eq!(each.len(), 2);
        assert_eq!(
            keys(&each[1]),
            [
                "CapacityModes:array",
                "ControlPhaseFailure:string",
                "EndReason:string",
                "McIndex:integer",
                "Server:string"
            ]
        );
        assert_eq!(each[0]["CapacityModes"][0]["Valid"], true);
        assert_eq!(each[1]["ControlPhaseFailure"], "TestInitiationTimedOut");
        // OB-UDPST's keys are unchanged.
        assert_matches_example(&text, "client_example_01.json");
        //  (the extension's keys, types and enumerated values are
        // stable), with every list populated.
        crate::results::tests::golden("extension.json", &text);
    }

    #[test]
    fn the_host_is_named_without_its_port() {
        assert_eq!(host_name("server.example:24601"), "server.example");
        assert_eq!(host_name("server.example"), "server.example");
        assert_eq!(host_name("[2001:db8::1]:24601"), "2001:db8::1");
        assert_eq!(host_name("2001:db8::1"), "2001:db8::1");
        assert_eq!(host_name("192.0.2.1:9"), "192.0.2.1");
    }
}
