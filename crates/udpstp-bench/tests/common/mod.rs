//! What the golden test and the differential harness share: the tagged
//! values `scripts/bench/tests/export_goldens.py` exchanges, the first
//! Round's functions as the port computes them under the names the script
//! gives the Python's, and when the port's outcome agrees with the
//! Python's.
//!
//! What the Python wrote, a golden file or the shim's answer, is read with
//! `serde_json::Value`, never with `Json::parse`, which the port's reading
//! uses: a fault in it would misread the expected values as it misreads
//! the inputs, and pass.
#![allow(dead_code)]

pub mod first_steps;
pub mod scale;
pub mod scale_judge;
pub mod scale_steps;
pub mod step_signal;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use bench_rounds::cost::{Prices, WorstCase};
use bench_rounds::num::Num;
use bench_rounds::py::Json;
use bench_rounds::quote;
use indexmap::IndexMap;
use serde_json::Value;
use udpstp_bench::first_round::judge::Unreadable;
use udpstp_bench::first_round::matrix::{self, ClientHost, Direction, Engine, Rate, RoundRun};
use udpstp_bench::first_round::report::{self, OutDir, RoundFiles};
use udpstp_bench::first_round::{commands, judge};

/// Independent golden cases use bounded workers so their full corpus fits
/// the test tier's budget. Each case owns its mutable state and files;
/// all workers join, and any failed assertion fails the enclosing test.
pub fn for_each_case<T: Sync>(cases: &[T], run: impl Fn(usize, &T) + Sync) {
    let workers = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .min(8)
        .min(cases.len())
        .max(1);
    std::thread::scope(|scope| {
        for worker in 0..workers {
            let run = &run;
            scope.spawn(move || {
                for index in (worker..cases.len()).step_by(workers) {
                    run(index, &cases[index]);
                }
            });
        }
    });
}

/// The reference exporter marks its captured originals before expanding
/// the expensive mutation/fault corpus. That expansion runs in the manual
/// Nightly workflow or explicitly with --ignored, retaining every case.
pub fn corpus_cases(golden: &Value, exhaustive: bool) -> &[Value] {
    let cases = golden["cases"].as_array().expect("reference cases");
    let ordinary = golden["ordinary_cases"]
        .as_u64()
        .expect("captured original count") as usize;
    assert!(
        (1..=cases.len()).contains(&ordinary),
        "invalid original count"
    );
    if exhaustive {
        cases
    } else {
        &cases[..ordinary]
    }
}

/// A tagged value as the value it stands for: `{"int": digits}` an int,
/// `{"float": hex}` a float by its bits, `{"dict": pairs}` a dict.
pub fn untag(tagged: &Value) -> Json {
    match tagged {
        Value::Null => Json::Null,
        Value::Bool(b) => Json::Bool(*b),
        Value::String(s) => Json::Str(s.clone()),
        Value::Array(items) => Json::List(items.iter().map(untag).collect()),
        Value::Object(tags) => {
            if let Some(Value::String(digits)) = tags.get("int") {
                Json::Int(digits.parse().expect("an int of 127 bits at most"))
            } else if let Some(Value::String(bits)) = tags.get("float") {
                Json::Float(f64::from_bits(
                    u64::from_str_radix(bits, 16).expect("64 bits in hex"),
                ))
            } else if let Some(Value::Array(pairs)) = tags.get("dict") {
                Json::Dict(
                    pairs
                        .iter()
                        .map(|pair| match pair.as_array().map(Vec::as_slice) {
                            Some([Value::String(key), value]) => (key.clone(), untag(value)),
                            _ => panic!("not a key and a value: {pair}"),
                        })
                        .collect(),
                )
            } else {
                panic!("not a tagged value: {tagged}")
            }
        }
        Value::Number(n) => panic!("a number the script didn't tag: {n}"),
    }
}

/// A value as the tagged JSON text the script reads.
pub fn tag(value: &Json, out: &mut String) {
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Json::Int(i) => {
            let _ = write!(out, r#"{{"int":"{i}"}}"#);
        }
        Json::Float(f) => {
            let _ = write!(out, r#"{{"float":"{:016x}"}}"#, f.to_bits());
        }
        Json::Str(s) => out.push_str(&serde_json::to_string(s).expect("a string")),
        Json::List(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                tag(item, out);
            }
            out.push(']');
        }
        Json::Dict(dict) => {
            out.push_str(r#"{"dict":["#);
            for (i, (key, item)) in dict.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push('[');
                out.push_str(&serde_json::to_string(key).expect("a key"));
                out.push(',');
                tag(item, out);
                out.push(']');
            }
            out.push_str("]}");
        }
    }
}

/// A dict of `(key, value)` pairs, in their order.
pub fn dict<const N: usize>(pairs: [(&str, Json); N]) -> Json {
    Json::Dict(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

fn text(value: &Json) -> &str {
    value
        .as_str()
        .unwrap_or_else(|| panic!("not a string: {value:?}"))
}

fn int(value: &Json) -> i128 {
    match value {
        Json::Int(i) => *i,
        other => panic!("not an int: {other:?}"),
    }
}

fn float(value: &Json) -> f64 {
    Num::from_json(value)
        .unwrap_or_else(|| panic!("not a number: {value:?}"))
        .to_f64()
}

fn strings(items: &[String]) -> Json {
    Json::List(items.iter().map(|s| Json::Str(s.clone())).collect())
}

/// A Round Run as the goldens hold one: `matrix.Run`'s fields in its
/// order, and the label.
pub fn run_json(run: &RoundRun) -> Json {
    let rate = match run.rate {
        Rate::Search => Json::Str("search".into()),
        Rate::FixedRate(row) => Json::Int(row.into()),
    };
    dict([
        ("index", Json::Int(run.index as i128)),
        ("repeat", Json::Int(run.repeat.into())),
        ("name", Json::Str(run.name.clone())),
        ("direction", Json::Str(run.direction.name().into())),
        ("rate", rate),
        ("test_connections", Json::Int(run.test_connections.into())),
        ("client_impl", Json::Str(run.client.name().into())),
        ("server_impl", Json::Str(run.server.name().into())),
        ("client_host", Json::Str(run.client_host.name().into())),
        ("test_interval", Json::Int(run.test_interval.into())),
        ("jumbo", Json::Bool(run.jumbo)),
        (
            "kernel_send_timestamps",
            Json::Bool(run.kernel_send_timestamps),
        ),
        ("label", Json::Str(run.label())),
    ])
}

/// A Round Run from the fields the goldens hold.
pub fn run_from(fields: &Json) -> RoundRun {
    let field = |name: &str| fields.item(name).unwrap_or_else(|e| panic!("{name}: {e}"));
    let engine = |name: &str| {
        let wanted = text(field(name));
        Engine::ALL
            .into_iter()
            .find(|e| e.name() == wanted)
            .unwrap_or_else(|| panic!("no engine {wanted}"))
    };
    let direction = text(field("direction"));
    RoundRun {
        index: usize::try_from(int(field("index"))).expect("an index"),
        repeat: i64::try_from(int(field("repeat"))).expect("a repeat"),
        name: text(field("name")).to_string(),
        direction: Direction::ALL
            .into_iter()
            .find(|d| d.name() == direction)
            .unwrap_or_else(|| panic!("no direction {direction}")),
        rate: match field("rate") {
            Json::Str(s) if s == "search" => Rate::Search,
            row => Rate::FixedRate(u16::try_from(int(row)).expect("a row")),
        },
        test_connections: u8::try_from(int(field("test_connections"))).expect("a count"),
        client: engine("client_impl"),
        server: engine("server_impl"),
        client_host: match text(field("client_host")) {
            "cloud" => ClientHost::Cloud,
            "home" => ClientHost::Home,
            other => panic!("no client host {other}"),
        },
        test_interval: i64::try_from(int(field("test_interval"))).expect("seconds"),
        jumbo: matches!(field("jumbo"), Json::Bool(true)),
        kernel_send_timestamps: matches!(field("kernel_send_timestamps"), Json::Bool(true)),
        host_samples: matches!(
            fields.as_dict().and_then(|f| f.get("host_samples")),
            Some(Json::Bool(true))
        ),
        server_engine_threads: None,
    }
}

fn runs_from(list: &Json) -> Vec<RoundRun> {
    list.as_list().expect("runs").iter().map(run_from).collect()
}

/// The prices besides the hosts' from a dict of them, 0 where it doesn't
/// say, as `prices.get(key, 0.0)`.
pub fn prices_from(prices: &Json) -> Prices {
    let price = |key: &str| {
        prices
            .as_dict()
            .expect("prices")
            .get(key)
            .map_or(0.0, float)
    };
    Prices {
        public_ipv4_per_hour: price("public_ipv4_per_hour"),
        disk_per_host_hour: price("disk_per_host_hour"),
        internet_egress_per_gb: price("internet_egress_per_gb"),
    }
}

/// `first_round_worst_case`: the worst case of a first Round's Round Runs,
/// each role's host at `host_price`.
pub fn worst_case(
    runs: &[RoundRun],
    host_price: &Json,
    prices: &Json,
    max_hours: f64,
    cap: i64,
) -> Json {
    let host_price = Num::from_json(host_price).expect("a price");
    let host_prices = vec![host_price; matrix::roles(runs).len()];
    WorstCase::new(
        &matrix::transfers(runs, cap),
        &host_prices,
        max_hours,
        &prices_from(prices),
        // A first Round's Test brings nothing back, so the share of
        // Server-scale Round Runs run again multiplies nothing.
        0.0,
    )
    .to_json()
}

/// What the port did with a case.
#[derive(Debug)]
pub enum Outcome {
    /// The value it computed.
    Value(Json),
    /// It refused, saying why: where the Python raises, or computes from
    /// what the port calls unreadable (the design's §7.7).
    Refuses(String),
}

/// A function of the port, by the script's name for its Python.
pub fn port(name: &str, args: &[Json]) -> Outcome {
    use Outcome::Value;
    let cap = |i: usize| i64::try_from(int(&args[i])).expect("a cap");
    match name {
        "first_round.expand" => Value(match matrix::parse(text(&args[0])) {
            Ok(runs) => dict([("runs", Json::List(runs.iter().map(run_json).collect()))]),
            Err(e) => dict([("refused", Json::Str(e.message().into()))]),
        }),
        "first_round.check" => Value(
            match matrix::check(&runs_from(&args[0]), float(&args[1]), cap(2)) {
                Ok(()) => Json::Null,
                Err(refusal) => Json::Str(refusal),
            },
        ),
        "first_round.worst_case" => Value(worst_case(
            &runs_from(&args[0]),
            &args[1],
            &args[2],
            float(&args[3]),
            cap(4),
        )),
        "first_round.client_command" => {
            let key_id = u8::try_from(int(&args[2])).expect("a key ID");
            let argv = commands::client(&run_from(&args[0]), text(&args[1]), key_id);
            Value(dict([
                ("argv", strings(&argv)),
                ("script", Json::Str(quote::join(&argv))),
            ]))
        }
        "first_round.server_command" => {
            let engine = Engine::ALL
                .into_iter()
                .find(|e| e.name() == text(&args[0]))
                .expect("an engine");
            let port = u16::try_from(int(&args[1])).expect("a port");
            let argv = commands::server(engine, port, matches!(args[2], Json::Bool(true)));
            Value(dict([
                ("argv", strings(&argv)),
                ("line", Json::Str(argv.join(" "))),
            ]))
        }
        "first_round.report" => match report::report(&MemoryFiles::of(&args[0])) {
            Ok(text) => Value(Json::Str(text)),
            Err(e) => Outcome::Refuses(e.to_string()),
        },
        // The same, from the tree written on disk and read through OutDir.
        "first_round.report_on_disk" => {
            let tree = Tree::write(&args[0]);
            let written = OutDir::new(&tree.out).and_then(|out| report::report(&out));
            match written {
                Ok(text) => Value(Json::Str(text)),
                Err(e) => Outcome::Refuses(e.to_string()),
            }
        }
        "first_round.figures" => match judge::read_figures(Some(text(&args[0]).as_bytes())) {
            Ok(None) => Value(Json::Null),
            Ok(Some(f)) => {
                let json = |n: Option<Num>| n.map_or(Json::Null, number);
                Value(dict([
                    ("maximum", json(f.maximum)),
                    ("highest", number(f.highest)),
                    ("highest_loss", number(f.highest_loss)),
                    ("lag_p99", json(f.lag_p99)),
                ]))
            }
            Err(e) => Outcome::Refuses(e.to_string()),
        },
        "first_round.environment" => match report::environment(text(&args[0])) {
            Ok(line) => Value(Json::Str(line)),
            Err(e) => Outcome::Refuses(e.to_string()),
        },
        "first_round.cell" => {
            let values: Vec<Option<Num>> = args[0]
                .as_list()
                .expect("values")
                .iter()
                .map(Num::from_json)
                .collect();
            let digits = usize::try_from(int(&args[2])).expect("digits");
            Value(Json::Str(report::cell(&values, text(&args[1]), digits)))
        }
        "first_round.round_name" => Value(Json::Str(report::round_name(text(&args[0])))),
        "first_round.round_dir" => Value(round_dir(&args[0], &args[1])),
        other => panic!("no port of {other}"),
    }
}

fn number(n: Num) -> Json {
    match n {
        Num::Int(i) => Json::Int(i),
        Num::Float(f) => Json::Float(f),
    }
}

/// `tests/test_round.py`'s `round_dir`, as a tree: a Round of `cases`,
/// `[label, name, engine, Client host, Test Connections, capacity or
/// null(, rate)]`, each of repeat 1, Upstream, with one Sub-Interval at
/// the capacity or none; and the counter files `counters` gives, `[label,
/// role, when, text]`. Its files come in the Python's order, with the
/// Python's JSON.
pub fn round_dir(cases: &Json, counters: &Json) -> Json {
    let mut files = IndexMap::new();
    let mut summary = String::new();
    for case in cases.as_list().expect("cases") {
        let case = case.as_list().expect("a case");
        let label = text(&case[0]);
        let subs = if case[5].is_null() {
            Json::List(vec![])
        } else {
            Json::List(vec![dict([
                ("IPLayerCapacity", case[5].clone()),
                ("LossRatio", Json::Float(0.0)),
            ])])
        };
        let result = dict([("Output", dict([("IncrementalResult", subs)]))]);
        files.insert(
            format!("tests/{label}.json"),
            Json::Str(bench_rounds::py::dumps(&result)),
        );
        let run = dict([
            ("label", case[0].clone()),
            ("status", Json::Int(0)),
            ("name", case[1].clone()),
            ("direction", Json::Str("upstream".into())),
            (
                "rate",
                case.get(6)
                    .cloned()
                    .unwrap_or_else(|| Json::Str("search".into())),
            ),
            ("test_connections", case[4].clone()),
            ("client_impl", case[2].clone()),
            ("server_impl", case[2].clone()),
            ("client_host", case[3].clone()),
            ("repeat", Json::Int(1)),
        ]);
        summary.push_str(&bench_rounds::py::dumps(&run));
        summary.push('\n');
    }
    for file in counters.as_list().expect("counter files") {
        let file = file.as_list().expect("a counter file");
        files.insert(
            format!(
                "tests/{}.{}.{}.txt",
                text(&file[0]),
                text(&file[1]),
                text(&file[2])
            ),
            file[3].clone(),
        );
    }
    files.insert("summary.jsonl".to_string(), Json::Str(summary));
    dict([
        ("name", Json::Str("round-1".into())),
        ("files", Json::Dict(files)),
    ])
}

/// A Round's tree in memory, read as its directory on disk would be: a
/// path made a directory can't be read, and `tests/` lists what is in it,
/// if anything is.
pub struct MemoryFiles {
    name: String,
    files: BTreeMap<String, Vec<u8>>,
    dirs: Vec<String>,
}

impl MemoryFiles {
    pub fn of(tree: &Json) -> MemoryFiles {
        let field = |name: &str| tree.get(name).expect("a tree");
        let mut files = BTreeMap::new();
        if let Some(texts) = field("files").as_dict() {
            for (path, t) in texts {
                files.insert(path.clone(), text(t).as_bytes().to_vec());
            }
        }
        if let Some(hexes) = field("bytes").as_dict() {
            for (path, h) in hexes {
                files.insert(path.clone(), hex(text(h)));
            }
        }
        let dirs = field("dirs")
            .as_list()
            .unwrap_or_default()
            .iter()
            .map(|d| text(d).to_string())
            .collect();
        MemoryFiles {
            name: text(tree.item("name").expect("a name")).to_string(),
            files,
            dirs,
        }
    }
}

impl RoundFiles for MemoryFiles {
    fn name(&self) -> &str {
        &self.name
    }

    fn read(&self, path: &str) -> Option<Vec<u8>> {
        self.files.get(path).cloned()
    }

    fn tests(&self) -> Option<Result<Vec<String>, Unreadable>> {
        let paths = self.files.keys().chain(&self.dirs);
        let names: BTreeSet<String> = paths
            .filter_map(|p| p.strip_prefix("tests/"))
            .map(|rest| rest.split('/').next().unwrap_or_default().to_string())
            .collect();
        let exists = !names.is_empty() || self.dirs.iter().any(|d| d == "tests");
        exists.then(|| Ok(names.into_iter().collect()))
    }
}

fn hex(h: &str) -> Vec<u8> {
    (0..h.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&h[i..i + 2], 16).expect("hex"))
        .collect()
}

/// A Round's tree, as the goldens hold one (`{"name", "files", "bytes",
/// "dirs"}`), written into a directory of its own, removed on drop.
pub struct Tree {
    root: PathBuf,
    pub out: PathBuf,
}

impl Tree {
    pub fn write(tree: &Json) -> Tree {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "udpstp-bench-tree-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let out = root.join(text(tree.item("name").expect("a name")));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&out).expect("the Round's directory");
        let field = |name: &str| tree.get(name).expect("a tree");
        for dir in field("dirs").as_list().unwrap_or_default() {
            fs::create_dir_all(out.join(text(dir))).expect("a directory");
        }
        let mut contents: Vec<(&String, Vec<u8>)> = Vec::new();
        if let Some(files) = field("files").as_dict() {
            contents.extend(
                files
                    .iter()
                    .map(|(path, t)| (path, text(t).as_bytes().to_vec())),
            );
        }
        if let Some(files) = field("bytes").as_dict() {
            contents.extend(files.iter().map(|(path, h)| (path, hex(text(h)))));
        }
        for (path, bytes) in contents {
            let file = out.join(path);
            fs::create_dir_all(file.parent().expect("a parent")).expect("its directory");
            fs::write(&file, bytes).unwrap_or_else(|e| panic!("{file:?}: {e}"));
        }
        Tree { root, out }
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// The one key of a first-round outcome dict (`{"runs": ...}`,
/// `{"refused": ...}`, `{"crashes": ...}`).
fn outcome_kind(value: &Json) -> Option<&str> {
    value.as_dict()?.keys().next().map(String::as_str)
}

/// Whether the port's outcome agrees with the Python's result, by the
/// case's mark (the script's docstring says what each means).
pub fn agrees(python: &Value, port: &Outcome, mark: Option<&str>) -> bool {
    let result = python.as_object().expect("a result");
    let raised = result.contains_key("raises");
    let expected = result.get("ok").map(untag);
    match (mark, port) {
        (None, Outcome::Value(value)) => expected.as_ref() == Some(value),
        (None, Outcome::Refuses(_)) => raised,
        (Some("words"), Outcome::Value(value)) => {
            let python_refused = expected
                .as_ref()
                .and_then(outcome_kind)
                .is_some_and(|k| k == "refused" || k == "crashes");
            python_refused && outcome_kind(value) == Some("refused")
        }
        (Some("stricter"), Outcome::Value(value)) => {
            expected.as_ref().and_then(outcome_kind) == Some("runs")
                && outcome_kind(value) == Some("refused")
        }
        (Some("toml-1.1"), Outcome::Value(value)) => {
            expected.as_ref().and_then(outcome_kind) == Some("refused")
                && outcome_kind(value) == Some("runs")
        }
        (Some("may-refuse"), Outcome::Value(value)) => expected.as_ref() == Some(value),
        (Some("may-refuse"), Outcome::Refuses(_)) => true,
        _ => false,
    }
}

/// A dict's items as `(key, value)` pairs, for the tests that walk one.
pub fn items(value: &Json) -> &IndexMap<String, Json> {
    value
        .as_dict()
        .unwrap_or_else(|| panic!("not a dict: {value:?}"))
}
