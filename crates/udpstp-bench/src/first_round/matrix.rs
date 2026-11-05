//! The first Round's matrix: the Tests a Round runs, read from its
//! TOML file, and its Round Runs in the order a Round makes them, each
//! with its label; how long they take, the checks a Round makes of them
//! before it is priced, and what they add to its worst case.
//!
//! Without the optional Server Engine Thread cap, a matrix is read as
//! `scripts/bench/matrix.py`'s `expand` reads one,
//! and one it refuses is refused in its words, so that a Round planned by
//! either tool makes the same Round Runs:
//!
//! - each repeat runs every Test, direction and pair once, in the file's
//!   order, so the engines interleave rather than run in blocks;
//! - every other repeat reverses the pairs, so neither engine always goes
//!   first while the hosts' burst allowance drains;
//! - `jumbo` is round-wide and has no default, since a path that can't
//!   carry 9000-byte packets would fail only udpstp's Tests (it sets DF).
//!
//! # Where it is stricter than the Python
//!
//! What no committed matrix holds and the Python reads anyway is refused
//! (the design's §7.7, decided as fixes): a key it doesn't know, and a
//! fraction or a numeric string where `int()` took one. Such a refusal is
//! in words of its own, in the Python's style.
//!
//! The Python refuses the rest of what the port was first stricter about,
//! since `c1-pyfix`, in the same words: `true` or `false` where a number
//! belongs (Python's `bool` is an `int`), a name that isn't a string or
//! that couldn't name a file (with `/`, `\` or NUL, which the report
//! refuses in a label), and directions or pairs that aren't a list.
//! Since `pyfix-2` it also refuses, in the same words, a `[defaults]` that
//! isn't a table and an infinity where a whole number belongs, where it
//! ended with a traceback.
//!
//! The TOML itself is read by the `toml` crate, which reads TOML 1.1 where
//! CPython 3.14's `tomllib` reads 1.0: a matrix using 1.1's additions
//! (newlines and a trailing comma in an inline table, `\e` and `\x`
//! escapes, a time without seconds) is read here and refused by the
//! Python, and TOML that neither reads is refused in other words.
//!
//! The explicit `server_engine_threads` extension runs udpstp Server cases
//! with the settled experimental pool cap, isolating each process and
//! preserving requested and observed evidence. Legacy matrices stay unchanged.

use std::cmp::Ordering;
use std::fmt;
use std::num::NonZeroUsize;

use bench_rounds::cost::RunTransfer;
use bench_rounds::num::{self, Num};
use serde::Deserialize;
use toml::Value;

/// Setup, activation and the STOP exchange around each Test Interval, s
/// (`matrix.py`'s `OVERHEAD_S`).
pub const OVERHEAD_S: i64 = 15;
/// Isolated Server overhead: two stops (15 s each), two starts
/// (3 x 10 s + 3 s each), capture (15 s), rounded up.
pub const SERVER_PRECISION_OVERHEAD_S: i64 = 120;

/// Provisioning the hosts, preparing them and checking the path, and the
/// Round Teardown, s (`round.py`'s `SETUP_S`).
pub const SETUP_S: i64 = 20 * 60;

/// The Sending Rate Table's last row.
pub const LAST_ROW: u16 = 1152;

/// The most Test Connections a Test of the matrix may have.
pub const MOST_TEST_CONNECTIONS: u8 = 24;

/// The fastest a Test from home may be, Mbit/s: faster rows use packets
/// over 1500 bytes with Jumbo, which the internet path to home doesn't
/// carry (1002 still fits; this keeps a margin).
pub const FASTEST_FROM_HOME_WITH_JUMBO: i64 = 1000;

/// An engine of the first Round, whose Client or Server a Round Run runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Engine {
    Udpstp,
    Obudpst,
}

impl Engine {
    /// Both, in the Python's order (`IMPLEMENTATIONS`).
    pub const ALL: [Engine; 2] = [Engine::Udpstp, Engine::Obudpst];

    /// Its name in a matrix's pairs, in labels and in the Round's files.
    /// The Python sorts engines by it (`obudpst` before `udpstp`), so the
    /// type has no order of its own.
    pub fn name(self) -> &'static str {
        match self {
            Engine::Udpstp => "udpstp",
            Engine::Obudpst => "obudpst",
        }
    }

    fn named(name: &str) -> Option<Engine> {
        Engine::ALL.into_iter().find(|e| e.name() == name)
    }
}

/// Which way a Test's Load PDUs go.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    Downstream,
    Upstream,
}

impl Direction {
    /// Both, in the Python's order (`DIRECTIONS`): a Test's directions
    /// unless it names them.
    pub const ALL: [Direction; 2] = [Direction::Downstream, Direction::Upstream];

    pub fn name(self) -> &'static str {
        match self {
            Direction::Downstream => "downstream",
            Direction::Upstream => "upstream",
        }
    }

    fn named(name: &str) -> Option<Direction> {
        Direction::ALL.into_iter().find(|d| d.name() == name)
    }
}

/// What sets a Test's sending rate: the Load Rate Adjustment Algorithm (a
/// Search Test), or one row of the Sending Rate Table (a Fixed-Rate Test).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Rate {
    Search,
    FixedRate(u16),
}

/// `str(run.rate)`: `search`, or the row's number, as the Round's
/// `summary.jsonl` and report give it.
impl fmt::Display for Rate {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Rate::Search => f.write_str("search"),
            Rate::FixedRate(row) => write!(f, "{row}"),
        }
    }
}

/// Where a Test's Client runs: a host of the Round, or this machine at
/// home, across the internet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ClientHost {
    Cloud,
    Home,
}

impl ClientHost {
    pub fn name(self) -> &'static str {
        match self {
            ClientHost::Cloud => "cloud",
            ClientHost::Home => "home",
        }
    }
}

/// One Test of the matrix, as a Round runs it (`matrix.py`'s `Run`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoundRun {
    /// Its place in the matrix, from 1; 0 for the path check.
    pub index: usize,
    /// The repeat it belongs to, from 1; 0 for the path check.
    pub repeat: i64,
    /// The Test's name in the matrix.
    pub name: String,
    pub direction: Direction,
    pub rate: Rate,
    pub test_connections: u8,
    /// The engine of its Client.
    pub client: Engine,
    /// The engine of its Server.
    pub server: Engine,
    pub client_host: ClientHost,
    /// Its Test Interval, s, as the matrix gives it.
    pub test_interval: i64,
    /// Round-wide: whether the path carries the Jumbo rows' 9000-byte
    /// packets.
    pub jumbo: bool,
    /// Whether udpstp's Client reports its kernel send timestamps' lag
    /// (M2), which loads its Load Sender, so only where a Test asks.
    pub kernel_send_timestamps: bool,
    /// Advisory host captures for every Round Run.
    pub host_samples: bool,
    /// An explicit cap for udpstp's pooled Server, not its active thread count.
    pub server_engine_threads: Option<NonZeroUsize>,
}

impl RoundRun {
    pub fn server_precision(&self) -> bool {
        self.kernel_send_timestamps
            && self.direction == Direction::Downstream
            && self.server == Engine::Udpstp
    }

    pub fn isolated_server(&self) -> bool {
        self.server == Engine::Udpstp
            && (self.server_precision() || self.server_engine_threads.is_some())
    }

    pub fn overhead_s(&self) -> i64 {
        OVERHEAD_S
            + if self.host_samples { 15 } else { 0 }
            + if self.isolated_server() {
                SERVER_PRECISION_OVERHEAD_S
            } else {
                0
            }
    }
    /// Its label, which names its files in the Round's `--out`:
    /// `NNN-name-direction-rate-cC-client-to-server-host-rR`, the name's
    /// spaces made `_` (`Run.label`).
    pub fn label(&self) -> String {
        let rate = match self.rate {
            Rate::Search => "search".to_string(),
            Rate::FixedRate(row) => format!("row{row}"),
        };
        format!(
            "{:03}-{}-{}-{rate}-c{}-{}-to-{}-{}-r{}",
            self.index,
            self.name.replace(' ', "_"),
            self.direction.name(),
            self.test_connections,
            self.client.name(),
            self.server.name(),
            self.client_host.name(),
            self.repeat
        )
    }

    /// The most it can send, Mbit/s: its row, or for a Search Test to home
    /// the home line's cap; `None` for a Search Test between the Round's
    /// hosts (`Run.mbps_bound`).
    pub fn mbps_bound(&self, home_cap_mbps: i64) -> Option<i64> {
        match self.rate {
            Rate::FixedRate(row) => Some(row.into()),
            Rate::Search => (self.client_host == ClientHost::Home).then_some(home_cap_mbps),
        }
    }
}

/// Why a matrix is refused: in the Python's words where it refuses the
/// same, which `round.py` prints after the matrix's path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatrixError(String);

impl MatrixError {
    fn new(message: impl Into<String>) -> MatrixError {
        MatrixError(message.into())
    }

    /// TOML that can't be read, where it went wrong as `tomllib` says it.
    fn toml(error: &toml::de::Error, text: &str) -> MatrixError {
        let at = error
            .span()
            .and_then(|span| text.get(..span.start))
            .map(|before| {
                let line = before.matches('\n').count() + 1;
                let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
                format!(" (at line {line}, column {column})")
            })
            .unwrap_or_default();
        MatrixError(format!("{}{at}", error.message()))
    }

    pub fn message(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MatrixError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for MatrixError {}

/// A matrix file: `[defaults]` and its `[[test]]`s, and nothing else.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    defaults: Option<Defaults>,
    test: Option<Vec<Test>>,
}

/// The values are TOML's own, checked by hand in the Python's order and
/// words.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Defaults {
    test_interval: Option<Value>,
    repeats: Option<Value>,
    jumbo: Option<Value>,
    host_samples: Option<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Test {
    name: Option<Value>,
    directions: Option<Value>,
    rate: Option<Value>,
    test_connections: Option<Value>,
    client: Option<Value>,
    test_interval: Option<Value>,
    pairs: Option<Value>,
    kernel_send_timestamps: Option<Value>,
    server_engine_threads: Option<Value>,
    /// Round-wide: named here only to be refused in the Python's words.
    jumbo: Option<Value>,
}

/// A matrix file's text, read as the Python's `expand` reads it: its Round
/// Runs, in the order a Round makes them.
///
/// It is checked as the Python checks it, a Test at a time on the first
/// repeat, so a matrix with the same first mistake is refused in the same
/// words; one with no repeats (`repeats` 0 or less) makes no Round Runs
/// and is checked no further, which a Round then refuses.
pub fn parse(text: &str) -> Result<Vec<RoundRun>, MatrixError> {
    let unreadable = |e: toml::de::Error| MatrixError::toml(&e, text);
    // `[defaults]` first, as the Python checks it, and in its words, where
    // reading `File` would refuse another value in serde's.
    let table: toml::Table = toml::from_str(text).map_err(unreadable)?;
    if let Some(defaults) = table.get("defaults").filter(|v| !v.is_table()) {
        return Err(MatrixError(format!(
            "[defaults] must be a table, not {}",
            repr(defaults)
        )));
    }
    let file: File = toml::from_str(text).map_err(unreadable)?;
    let defaults = file.defaults.unwrap_or_default();
    let tests = file.test.unwrap_or_default();
    if tests.is_empty() {
        return Err(MatrixError::new("the matrix has no [[test]]"));
    }
    let repeats = match &defaults.repeats {
        None => 1,
        Some(value) => whole(value).ok_or_else(|| {
            MatrixError(format!(
                "repeats must be a whole number, not {}",
                repr(value)
            ))
        })?,
    };
    let jumbo = match &defaults.jumbo {
        None => {
            return Err(MatrixError::new(
                "set jumbo under [defaults]: true if the path between the hosts carries \
                 9000-byte IP packets, false if not",
            ));
        }
        Some(Value::Boolean(jumbo)) => *jumbo,
        Some(other) => {
            return Err(MatrixError(format!(
                "jumbo must be true or false, not {}",
                repr(other)
            )));
        }
    };
    let mut runs = Vec::new();
    if !matches!(defaults.host_samples, None | Some(Value::Boolean(_))) {
        return Err(MatrixError::new("host_samples must be true or false"));
    }
    for repeat in 1..=repeats {
        for test in &tests {
            expand_test(test, &defaults, repeat, jumbo, &mut runs)?;
        }
    }
    Ok(runs)
}

/// One Test's Round Runs in one repeat, added to `runs`.
fn expand_test(
    test: &Test,
    defaults: &Defaults,
    repeat: i64,
    jumbo: bool,
    runs: &mut Vec<RoundRun>,
) -> Result<(), MatrixError> {
    let refused = |message: String| Err(MatrixError(message));
    let name = match &test.name {
        // `test["name"]`'s KeyError, as `round.py` prints it.
        None => return refused("'name'".to_string()),
        Some(Value::String(name)) => name,
        Some(other) => {
            return refused(format!(
                "a [[test]]'s name must be a string, not {}",
                repr(other)
            ));
        }
    };
    // A name is part of each label, which names the Test's files.
    if name.contains(['/', '\\', '\0']) {
        return refused(format!(
            "{}: a name can't hold /, \\ or NUL, since it names the Test's files",
            repr_str(name)
        ));
    }
    if test.jumbo.is_some() {
        return refused(format!(
            "{name}: jumbo is round-wide: set it under [defaults]"
        ));
    }
    let kernel_send_timestamps = match &test.kernel_send_timestamps {
        None => false,
        Some(Value::Boolean(stamps)) => *stamps,
        Some(_) => {
            return refused(format!(
                "{name}: kernel_send_timestamps must be true or false"
            ));
        }
    };
    let server_engine_threads = match &test.server_engine_threads {
        None => None,
        Some(Value::Integer(n)) => Some(
            usize::try_from(*n)
                .ok()
                .and_then(NonZeroUsize::new)
                .ok_or_else(|| {
                    MatrixError::new(format!(
                        "{name}: server_engine_threads must be a positive integer that fits usize"
                    ))
                })?,
        ),
        Some(_) => {
            return refused(format!(
                "{name}: server_engine_threads must be a positive integer that fits usize"
            ));
        }
    };
    let rate = match &test.rate {
        None => Rate::Search,
        Some(Value::String(rate)) if rate == "search" => Rate::Search,
        Some(Value::Integer(row)) if (0..=i64::from(LAST_ROW)).contains(row) => {
            Rate::FixedRate(u16::try_from(*row).expect("a row in the table"))
        }
        Some(other) => {
            return refused(format!(
                "{name}: rate must be \"search\" or a row 0-{LAST_ROW}, not {}",
                repr(other)
            ));
        }
    };
    let test_connections = match &test.test_connections {
        None => 1,
        Some(Value::Integer(n)) => match u8::try_from(*n) {
            Ok(n) if (1..=MOST_TEST_CONNECTIONS).contains(&n) => n,
            _ => {
                return refused(format!(
                    "{name}: test_connections must be 1-{MOST_TEST_CONNECTIONS}"
                ));
            }
        },
        Some(other) => {
            return refused(format!(
                "{name}: test_connections must be a whole number 1-{MOST_TEST_CONNECTIONS}, not {}",
                repr(other)
            ));
        }
    };
    let client_host = match &test.client {
        None => ClientHost::Cloud,
        Some(Value::String(host)) if host == "cloud" => ClientHost::Cloud,
        Some(Value::String(host)) if host == "home" => ClientHost::Home,
        Some(_) => return refused(format!("{name}: client must be \"cloud\" or \"home\"")),
    };
    // The Test's own, else the defaults', else 10 s: only the one used is
    // read, as the Python reads it.
    let test_interval = match test
        .test_interval
        .as_ref()
        .or(defaults.test_interval.as_ref())
    {
        None => 10,
        Some(value) => match whole(value) {
            Some(seconds) => seconds,
            None => {
                return refused(format!(
                    "{name}: test_interval must be a whole number of seconds, not {}",
                    repr(value)
                ));
            }
        },
    };
    let default_pairs = [Value::String("udpstp/udpstp".into())];
    let mut pairs: Vec<&Value> = match &test.pairs {
        None => default_pairs.iter().collect(),
        Some(Value::Array(pairs)) => pairs.iter().collect(),
        Some(other) => {
            return refused(format!("{name}: pairs must be a list, not {}", repr(other)));
        }
    };
    // Every other repeat reverses the pairs: A, B, then B, A.
    if repeat % 2 == 0 {
        pairs.reverse();
    }
    let default_directions = Direction::ALL.map(|d| Value::String(d.name().into()));
    let directions: Vec<&Value> = match &test.directions {
        None => default_directions.iter().collect(),
        Some(Value::Array(directions)) => directions.iter().collect(),
        Some(other) => {
            return refused(format!(
                "{name}: directions must be a list, not {}",
                repr(other)
            ));
        }
    };
    let first_run = runs.len();
    for value in directions {
        let Some(direction) = value.as_str().and_then(Direction::named) else {
            return refused(format!("{name}: unknown direction {}", repr(value)));
        };
        for &pair in &pairs {
            let engines = pair.as_str().and_then(|pair| {
                let (client, server) = pair.split_once('/')?;
                Some((Engine::named(client)?, Engine::named(server)?))
            });
            let Some((client, server)) = engines else {
                return refused(format!("{name}: unknown pair {}", repr(pair)));
            };
            runs.push(RoundRun {
                index: runs.len() + 1,
                repeat,
                name: name.clone(),
                direction,
                rate,
                test_connections,
                client,
                server,
                client_host,
                test_interval,
                jumbo,
                kernel_send_timestamps,
                host_samples: matches!(defaults.host_samples, Some(Value::Boolean(true))),
                server_engine_threads: server_engine_threads.filter(|_| server == Engine::Udpstp),
            });
        }
    }
    if server_engine_threads.is_some()
        && !runs[first_run..].iter().any(|r| r.server == Engine::Udpstp)
    {
        return refused(format!(
            "{name}: server_engine_threads needs a udpstp Server pair"
        ));
    }
    Ok(())
}

/// A whole number where the matrix holds one: a TOML integer, never a
/// bool, which the Python refuses too, nor a float or a string, which the
/// Python's `int()` takes.
fn whole(value: &Value) -> Option<i64> {
    match value {
        Value::Integer(n) => Some(*n),
        _ => None,
    }
}

/// How long the Round Runs take, s: each Test Interval plus its overhead
/// (`duration_s`).
pub fn duration_s(runs: &[RoundRun]) -> i128 {
    runs.iter()
        .map(|run| i128::from(run.test_interval) + i128::from(run.overhead_s()))
        .sum()
}

/// The hours the plan prints: the duration's, to two decimals
/// (`round(duration_s(runs) / 3600, 2)`).
pub fn matrix_hours(runs: &[RoundRun]) -> f64 {
    num::py_round(hours(duration_s(runs)), 2)
}

/// `seconds / 3600`, Python's true division.
fn hours(seconds: i128) -> f64 {
    Num::Int(seconds)
        .py_div(Num::Int(3600))
        // Beyond 2^53 s, which no matrix reaches.
        .unwrap_or(seconds as f64 / 3600.0)
}

/// The checks `round.py` makes of the Round Runs a Round would make
/// (without `--home-client`, none from home) before it prices the Round
/// (`check_matrix`), with its refusal: a row to home above the home line's
/// cap, a Test from home as fast as the Jumbo rows, or a matrix that takes
/// longer than `--max-hours` with setup.
pub fn check(runs: &[RoundRun], max_hours: f64, home_cap_mbps: i64) -> Result<(), String> {
    for run in runs.iter().filter(|r| r.client_host == ClientHost::Home) {
        if let Rate::FixedRate(row) = run.rate
            && i64::from(row) > home_cap_mbps
        {
            return Err(format!(
                "{}: row {row} to home is above the home cap ({home_cap_mbps} Mbit/s): the rest \
                 would be paid egress, then lost",
                run.name
            ));
        }
        if run.jumbo
            && run
                .mbps_bound(home_cap_mbps)
                .is_some_and(|mbps| mbps > FASTEST_FROM_HOME_WITH_JUMBO)
        {
            return Err(format!(
                "{}: with jumbo = true, a Test from home must stay below the Jumbo rows: lower \
                 --home-cap-mbps to 1000 or less",
                run.name
            ));
        }
    }
    let needed = duration_s(runs) + i128::from(SETUP_S);
    if Num::Int(needed).py_cmp(Num::Float(max_hours * 3600.0)) == Some(Ordering::Greater) {
        return Err(format!(
            "the matrix takes about {} h with setup, more than --max-hours {}",
            num::fixed(hours(needed), 2),
            num::repr(max_hours)
        ));
    }
    Ok(())
}

/// The roles a Round of these Round Runs needs: the Server's host, and a
/// Client host where a Test runs between the Round's hosts (`roles_of`).
pub fn roles(runs: &[RoundRun]) -> Vec<&'static str> {
    let mut roles = vec!["server"];
    if runs.iter().any(|r| r.client_host == ClientHost::Cloud) {
        roles.push("client");
    }
    roles
}

/// What each Round Run adds to the worst case's transfer: a Downstream
/// Test to home leaves the provider as paid egress, at most its
/// [`RoundRun::mbps_bound`]; a first Round's Test brings nothing back
/// (`worst_case`'s loop).
pub fn transfers(runs: &[RoundRun], home_cap_mbps: i64) -> Vec<RunTransfer> {
    runs.iter()
        .map(|run| {
            let home_downstream =
                run.client_host == ClientHost::Home && run.direction == Direction::Downstream;
            RunTransfer {
                home_downstream: home_downstream
                    .then(|| run.mbps_bound(home_cap_mbps))
                    .flatten()
                    .map(|mbps| (mbps as f64, run.test_interval as f64)),
                download_gb: 0.0,
            }
        })
        .collect()
}

/// Python's `repr` of what `tomllib` reads a TOML value as, for the
/// Python's messages. Two differ, in values no matrix holds where these
/// are shown: a table's keys come in order of name, where Python keeps the
/// file's order, and a date or a time is written as TOML writes it.
pub fn repr(value: &Value) -> String {
    match value {
        Value::String(s) => repr_str(s),
        Value::Integer(i) => i.to_string(),
        Value::Float(f) => num::repr(*f),
        Value::Boolean(b) => if *b { "True" } else { "False" }.to_string(),
        Value::Datetime(d) => d.to_string(),
        Value::Array(items) => {
            let items: Vec<String> = items.iter().map(repr).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Table(table) => {
            let items: Vec<String> = table
                .iter()
                .map(|(key, value)| format!("{}: {}", repr_str(key), repr(value)))
                .collect();
            format!("{{{}}}", items.join(", "))
        }
    }
}

/// Python's `repr` of a string: in single quotes, or double quotes where
/// it holds a single quote and no double one; the quote and `\` escaped,
/// `\t`, `\n` and `\r` as such, other control characters as `\xNN`, and
/// what Python doesn't print as `\xNN`, `\uNNNN` or `\UNNNNNNNN`.
pub fn repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            c if c == quote || c == '\\' => {
                out.push('\\');
                out.push(c);
            }
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            ' '..='~' => out.push(c),
            c if c.is_ascii() => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if printable(c) => out.push(c),
            c if (c as u32) <= 0xff => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if (c as u32) <= 0xffff => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push_str(&format!("\\U{:08x}", c as u32)),
        }
    }
    out.push(quote);
    out
}

/// Whether Python prints a character beyond ASCII as it is (`isprintable`):
/// not a control, format or private-use character, a separator, or a
/// noncharacter. Python also escapes a code point Unicode hasn't assigned,
/// which this, without Unicode's tables, prints.
fn printable(c: char) -> bool {
    /// Unicode 16's format characters (Cf), which CPython 3.14 uses.
    const FORMAT: &[(u32, u32)] = &[
        (0xad, 0xad),
        (0x600, 0x605),
        (0x61c, 0x61c),
        (0x6dd, 0x6dd),
        (0x70f, 0x70f),
        (0x890, 0x891),
        (0x8e2, 0x8e2),
        (0x180e, 0x180e),
        (0x200b, 0x200f),
        (0x202a, 0x202e),
        (0x2060, 0x2064),
        (0x2066, 0x206f),
        (0xfeff, 0xfeff),
        (0xfff9, 0xfffb),
        (0x110bd, 0x110bd),
        (0x110cd, 0x110cd),
        (0x13430, 0x1343f),
        (0x1bca0, 0x1bca3),
        (0x1d173, 0x1d17a),
        (0xe0001, 0xe0001),
        (0xe0020, 0xe007f),
    ];
    let n = c as u32;
    let format = FORMAT.iter().any(|&(lo, hi)| (lo..=hi).contains(&n));
    let private_use =
        (0xe000..=0xf8ff).contains(&n) || (0xf0000..=0xffffd).contains(&n) || n >= 0x100000;
    let noncharacter = (0xfdd0..=0xfdef).contains(&n) || n & 0xfffe == 0xfffe;
    !(c.is_control() || c.is_whitespace() || format || private_use || noncharacter)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refusal(text: &str) -> String {
        parse(text).expect_err("refused").message().to_string()
    }

    fn one_test(test: &str) -> String {
        format!("[defaults]\njumbo = false\n\n[[test]]\nname = \"x\"\n{test}\n")
    }

    #[test]
    fn repeats_interleave_the_engines_and_alternate_who_goes_first() {
        // tests/test_round.py's first Matrix test.
        let runs = parse(
            "[defaults]\nrepeats = 2\ntest_interval = 10\njumbo = true\n\n[[test]]\nname = \"a\"\n\
             rate = \"search\"\ndirections = [\"downstream\"]\npairs = [\"udpstp/udpstp\", \"obudpst/obudpst\"]\n",
        )
        .unwrap();
        let order: Vec<(i64, Engine)> = runs.iter().map(|r| (r.repeat, r.client)).collect();
        assert_eq!(
            order,
            [
                (1, Engine::Udpstp),
                (1, Engine::Obudpst),
                (2, Engine::Obudpst),
                (2, Engine::Udpstp)
            ]
        );
        assert_eq!(duration_s(&runs), 4 * (10 + 15));
        assert_eq!(
            runs[3].label(),
            "004-a-downstream-search-c1-udpstp-to-udpstp-cloud-r2"
        );
    }

    #[test]
    fn a_bad_matrix_is_refused_in_the_pythons_words() {
        // tests/test_round.py's second Matrix test, with the words.
        for (test, words) in [
            (
                "rate = 2000",
                "x: rate must be \"search\" or a row 0-1152, not 2000",
            ),
            (
                "pairs = [\"udpstp/iperf\"]",
                "x: unknown pair 'udpstp/iperf'",
            ),
            (
                "client = \"moon\"",
                "x: client must be \"cloud\" or \"home\"",
            ),
            (
                "directions = [\"sideways\"]",
                "x: unknown direction 'sideways'",
            ),
            (
                "jumbo = false",
                "x: jumbo is round-wide: set it under [defaults]",
            ),
            (
                "kernel_send_timestamps = 1",
                "x: kernel_send_timestamps must be true or false",
            ),
            ("test_connections = 25", "x: test_connections must be 1-24"),
            (
                "rate = 100.0",
                "x: rate must be \"search\" or a row 0-1152, not 100.0",
            ),
            (
                "rate = \"fast\"",
                "x: rate must be \"search\" or a row 0-1152, not 'fast'",
            ),
        ] {
            assert_eq!(refusal(&one_test(test)), words, "{test}");
        }
        // jumbo has no default, since a wrong guess fails only udpstp's Tests.
        assert_eq!(
            refusal("[[test]]\nname = \"x\"\n"),
            "set jumbo under [defaults]: true if the path between the hosts carries 9000-byte \
             IP packets, false if not"
        );
        assert_eq!(
            refusal("[defaults]\njumbo = \"yes\"\n[[test]]\nname = \"x\"\n"),
            "jumbo must be true or false, not 'yes'"
        );
        assert_eq!(
            refusal("[defaults]\njumbo = true\n"),
            "the matrix has no [[test]]"
        );
        assert_eq!(
            refusal("[defaults]\njumbo = true\n[[test]]\nrate = 5\n"),
            "'name'"
        );
    }

    #[test]
    fn what_the_python_reads_but_no_matrix_holds_is_refused() {
        // The design's §7.7: fractions and numeric strings where int()
        // took them, unknown keys.
        for test in [
            "test_connections = 4.5",
            "test_connections = \"4\"",
            "test_interval = 10.0",
            "test_interval = \"10\"",
            "colour = \"blue\"",
        ] {
            assert!(parse(&one_test(test)).is_err(), "{test}");
        }
        for defaults in ["repeats = 2.0", "repeats = \"2\"", "note = 1"] {
            let text = format!("[defaults]\njumbo = true\n{defaults}\n[[test]]\nname = \"x\"\n");
            assert!(parse(&text).is_err(), "{defaults}");
        }
        assert!(parse("jumbo = true\n[[test]]\nname = \"x\"\n").is_err());
        assert_eq!(
            refusal(&one_test("test_connections = 4.5")),
            "x: test_connections must be a whole number 1-24, not 4.5"
        );
    }

    #[test]
    fn what_both_refuse_since_the_pythons_fix_is_refused_in_its_words() {
        // `true` where a number belongs, a name that couldn't name the
        // Test's files, a list given as a string: the Python refuses them
        // too since c1-pyfix (scripts/bench/tests/test_round.py's Matrix).
        for (test, words) in [
            (
                "rate = true",
                "x: rate must be \"search\" or a row 0-1152, not True",
            ),
            (
                "test_connections = false",
                "x: test_connections must be a whole number 1-24, not False",
            ),
            (
                "test_interval = true",
                "x: test_interval must be a whole number of seconds, not True",
            ),
            (
                "directions = \"downstream\"",
                "x: directions must be a list, not 'downstream'",
            ),
            ("pairs = \"\"", "x: pairs must be a list, not ''"),
            ("pairs = [5]", "x: unknown pair 5"),
            // Since pyfix-2, where int() ended the Python with a traceback.
            (
                "test_connections = inf",
                "x: test_connections must be a whole number 1-24, not inf",
            ),
            (
                "test_interval = -inf",
                "x: test_interval must be a whole number of seconds, not -inf",
            ),
        ] {
            assert_eq!(refusal(&one_test(test)), words, "{test}");
        }
        assert_eq!(
            refusal("[defaults]\njumbo = true\nrepeats = true\n[[test]]\nname = \"x\"\n"),
            "repeats must be a whole number, not True"
        );
        assert_eq!(
            refusal("[defaults]\njumbo = true\nrepeats = inf\n[[test]]\nname = \"x\"\n"),
            "repeats must be a whole number, not inf"
        );
        assert_eq!(
            refusal("[defaults]\njumbo = true\n[[test]]\nname = 5\n"),
            "a [[test]]'s name must be a string, not 5"
        );
        for (name, shown) in [
            ("a/b", "'a/b'"),
            ("a\\\\b", "'a\\\\b'"),
            ("a\\u0000b", "'a\\x00b'"),
        ] {
            let text = format!("[defaults]\njumbo = true\n[[test]]\nname = \"{name}\"\n");
            assert_eq!(
                refusal(&text),
                format!("{shown}: a name can't hold /, \\ or NUL, since it names the Test's files")
            );
        }
    }

    #[test]
    fn defaults_that_arent_a_table_are_refused_first_in_the_pythons_words() {
        // The Python refuses them too since pyfix-2, before it looks for
        // Tests (scripts/bench/tests/test_round.py's Matrix).
        for (defaults, shown) in [("5", "5"), ("\"x\"", "'x'")] {
            for tests in ["[[test]]\nname = \"x\"\n", ""] {
                assert_eq!(
                    refusal(&format!("defaults = {defaults}\n{tests}")),
                    format!("[defaults] must be a table, not {shown}")
                );
            }
        }
        assert_eq!(
            refusal("[[defaults]]\njumbo = true\n[[test]]\nname = \"x\"\n"),
            "[defaults] must be a table, not [{'jumbo': True}]"
        );
    }

    #[test]
    fn a_matrix_is_checked_as_far_as_the_python_checks_it() {
        // No repeats: no Round Runs, and the Tests unchecked.
        let text = "[defaults]\njumbo = true\nrepeats = 0\n[[test]]\nname = \"x\"\nrate = 5000\n";
        assert_eq!(parse(text), Ok(vec![]));
        // No directions: the pairs unchecked; the defaults' Test Interval
        // unread where each Test has its own.
        let text = "[defaults]\njumbo = true\ntest_interval = \"ten\"\n[[test]]\nname = \"x\"\n\
                    directions = []\npairs = [\"x/y\"]\ntest_interval = 5\n";
        assert_eq!(parse(text), Ok(vec![]));
        // The first mistake in the Python's order is the one said.
        let text = one_test("rate = 5000\ntest_connections = 0\nclient = \"moon\"");
        assert_eq!(
            refusal(&text),
            "x: rate must be \"search\" or a row 0-1152, not 5000"
        );
    }

    #[test]
    fn toml_that_cant_be_read_says_where() {
        let message = refusal("[defaults]\njumbo = \n");
        assert!(message.contains(" (at line 2, column "), "{message}");
        assert!(!message.contains('\n'), "{message}");
    }

    #[test]
    fn the_first_round_fits_its_two_hours() {
        let text = include_str!("../../../../scripts/bench/first-round.toml");
        let runs = parse(text).unwrap();
        assert_eq!(runs.len(), 96);
        assert_eq!(check(&runs, 2.0, 250), Ok(()));
        // An even number of repeats, so each engine goes first as often.
        assert_eq!(runs.iter().filter(|r| r.repeat % 2 == 0).count(), 48);
        let stamped: std::collections::BTreeSet<&str> = runs
            .iter()
            .filter(|r| r.kernel_send_timestamps)
            .map(|r| r.name.as_str())
            .collect();
        assert_eq!(stamped, ["M2 row 100", "M2 row 1000", "M2 row 500"].into());
        assert!(runs.iter().all(|r| r.jumbo));
    }

    #[test]
    fn a_case_can_request_a_udpstp_server_engine_thread_cap() {
        //  measures the settled pooled executor with an explicit cap.
        let text = one_test("server_engine_threads = 2");
        let runs = parse(&text).unwrap();
        assert!(
            runs.iter()
                .all(|r| r.server_engine_threads.unwrap().get() == 2)
        );
        assert!(runs.iter().all(RoundRun::isolated_server));
        let mut stamped = runs[0].clone();
        stamped.kernel_send_timestamps = true;
        assert_eq!(
            stamped.overhead_s(),
            OVERHEAD_S + SERVER_PRECISION_OVERHEAD_S
        );
    }

    #[test]
    fn server_caps_are_positive_and_apply_only_to_udpstp_server_pairs() {
        // The executable's experimental setting is a positive usize.
        for value in ["0", "-1", "true", "2.5", "\"2\""] {
            let text = one_test(&format!("server_engine_threads = {value}"));
            assert_eq!(
                refusal(&text),
                "x: server_engine_threads must be a positive integer that fits usize"
            );
        }
        let runs = parse(&one_test("server_engine_threads = 2\npairs = [\"udpstp/udpstp\", \"obudpst/udpstp\", \"udpstp/obudpst\", \"obudpst/obudpst\"]")).unwrap();
        for run in runs {
            assert_eq!(
                run.server_engine_threads.is_some(),
                run.server == Engine::Udpstp
            );
        }
        assert_eq!(
            refusal(&one_test(
                "server_engine_threads = 2\npairs = [\"obudpst/obudpst\"]"
            )),
            "x: server_engine_threads needs a udpstp Server pair"
        );
    }

    fn run(name: &str, direction: Direction, rate: Rate, host: ClientHost) -> RoundRun {
        RoundRun {
            index: 1,
            repeat: 1,
            name: name.into(),
            direction,
            rate,
            test_connections: 1,
            client: Engine::Udpstp,
            server: Engine::Udpstp,
            client_host: host,
            test_interval: 10,
            jumbo: true,
            kernel_send_timestamps: false,
            host_samples: false,
            server_engine_threads: None,
        }
    }

    #[test]
    fn jumbo_rows_never_go_to_home_nor_rows_above_its_cap() {
        let home = run("h", Direction::Downstream, Rate::Search, ClientHost::Home);
        assert_eq!(check(std::slice::from_ref(&home), 2.0, 250), Ok(()));
        assert_eq!(
            check(std::slice::from_ref(&home), 2.0, 2000),
            Err(
                "h: with jumbo = true, a Test from home must stay below the Jumbo rows: lower \
                 --home-cap-mbps to 1000 or less"
                    .to_string()
            )
        );
        let row = run(
            "r",
            Direction::Downstream,
            Rate::FixedRate(1000),
            ClientHost::Home,
        );
        assert_eq!(
            check(&[row], 2.0, 250),
            Err(
                "r: row 1000 to home is above the home cap (250 Mbit/s): the rest would be \
                 paid egress, then lost"
                    .to_string()
            )
        );
    }

    #[test]
    fn a_matrix_longer_than_the_round_is_refused() {
        let mut runs = vec![
            run(
                "x",
                Direction::Downstream,
                Rate::FixedRate(100),
                ClientHost::Cloud
            );
            100
        ];
        for r in &mut runs {
            r.test_interval = 60;
        }
        // 100 × (60 + 15) s and 20 minutes of setup.
        assert_eq!(
            check(&runs, 1.0, 250),
            Err("the matrix takes about 2.42 h with setup, more than --max-hours 1.0".to_string())
        );
        assert_eq!(check(&runs, 2.6, 250), Ok(()));
        assert_eq!(matrix_hours(&runs), 2.08);
    }

    #[test]
    fn only_a_downstream_test_to_home_is_paid_egress() {
        let runs = [
            run("h", Direction::Downstream, Rate::Search, ClientHost::Home),
            run("h", Direction::Upstream, Rate::Search, ClientHost::Home),
            run(
                "c",
                Direction::Downstream,
                Rate::FixedRate(1000),
                ClientHost::Cloud,
            ),
        ];
        let transfers = transfers(&runs, 250);
        assert_eq!(transfers[0].home_downstream, Some((250.0, 10.0)));
        assert_eq!(transfers[1].home_downstream, None);
        assert_eq!(transfers[2].home_downstream, None);
        assert_eq!(roles(&runs), ["server", "client"]);
        assert_eq!(roles(&runs[..2]), ["server"]);
    }

    #[test]
    fn repr_writes_values_as_python_does() {
        let value = |text: &str| -> Value {
            let table: toml::Table = toml::from_str(&format!("v = {text}")).unwrap();
            table["v"].clone()
        };
        for (text, python) in [
            ("'a'", "'a'"),
            ("\"it's\"", "\"it's\""),
            ("'say \"hi\"'", "'say \"hi\"'"),
            ("\"both ' and \\\"\"", "'both \\' and \"'"),
            (
                "\"tab\\tnl\\n\\u0000\\u007f\\u001f\"",
                "'tab\\tnl\\n\\x00\\x7f\\x1f'",
            ),
            (
                "\"\\u00e9\\u00a0\\u200b\\u2028\\ue000\\U0001F600\\ufffe\\u00ad\"",
                "'é\\xa0\\u200b\\u2028\\ue000😀\\ufffe\\xad'",
            ),
            ("1e16", "1e+16"),
            ("-0.0", "-0.0"),
            ("nan", "nan"),
            ("[1, 'a', 2.5, true]", "[1, 'a', 2.5, True]"),
            ("{a = [2], b = 1}", "{'a': [2], 'b': 1}"),
        ] {
            assert_eq!(repr(&value(text)), python, "{text}");
        }
    }
}
