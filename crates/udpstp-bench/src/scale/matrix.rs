//! Server-scale matrices and their ordered Round Runs (design §3.3).

mod alphanumeric;

use std::collections::{BTreeMap, BTreeSet};

use bench_rounds::num::{self, Num};
use bench_rounds::order;
use bench_rounds::py::{EMPTY_DICT, EMPTY_LIST, Json};
use indexmap::IndexMap;

use super::Error;
use crate::first_round::matrix::repr_str;

pub const ENGINES: [&str; 5] = [
    "udpstp",
    "prototype-a",
    "prototype-b",
    "prototype-b-core",
    "obudpst",
];
pub const EXECUTOR_ENV: &str = "UDPSTP_ENGINE_EXECUTOR";
pub const GROUPING_ENV: &str = "UDPSTP_ENGINE_GROUPING_US";
pub const STAMPS_ENV: &str = "UDPSTP_KERNEL_TX_TIMESTAMPS";
pub const STAGGER_S: f64 = 0.005;
pub const RERUN_SHARE: f64 = 0.15;

/// One engine at one count of a case, with the same metadata as scale.py.
#[derive(Clone, Debug, PartialEq)]
pub struct ScaleRun {
    pub index: i64,
    pub repeat: i64,
    pub case: String,
    pub direction: String,
    pub row: i64,
    pub count: i64,
    pub engine: String,
    pub test_interval: i64,
    pub warm_up: i64,
    pub kind: String,
    pub rerun: i64,
    pub more: i64,
    pub reserve_s: f64,
    pub cpus: String,
    pub stagger: Num,
}

impl ScaleRun {
    pub fn label(&self) -> String {
        let cpus = if self.kind == "confine" {
            format!("-cpu{}", self.cpus.replace(',', "+"))
        } else {
            String::new()
        };
        let again = if self.rerun != 0 {
            format!("-again{}", self.rerun)
        } else {
            String::new()
        };
        let more = if self.more != 0 {
            format!("-more{}", self.more)
        } else {
            String::new()
        };
        format!(
            "{:04}-{}-{}-{}-row{}-n{}-{}{}-r{}{}{}",
            self.index,
            self.kind,
            self.case,
            self.direction,
            self.row,
            self.count,
            self.engine,
            cpus,
            self.repeat,
            again,
            more
        )
    }

    pub fn point(&self) -> (String, String, String, i64, i64, String) {
        (
            self.kind.clone(),
            self.case.clone(),
            self.direction.clone(),
            self.row,
            self.count,
            self.engine.clone(),
        )
    }

    pub fn seconds(&self) -> f64 {
        if self.engine.starts_with("prototype-") {
            // Keep Python's addition order, including its first integer sum.
            (4 + self.warm_up + self.test_interval) as f64 + 0.4 + 6.0 + 1.5
        } else {
            self.test_interval as f64
                + self.count as f64 / 2.0 * self.stagger.to_f64()
                + 3.0
                + 6.0
                + 1.5
        }
    }

    pub fn download_gb(&self) -> f64 {
        (0.5 + 0.02 * self.count as f64) / 1000.0
    }

    /// The fields a Round writes in its metadata, in Python's field order.
    pub fn metadata(&self) -> Json {
        dict([
            ("index", int(self.index)),
            ("repeat", int(self.repeat)),
            ("case", string(&self.case)),
            ("direction", string(&self.direction)),
            ("row", int(self.row)),
            ("count", int(self.count)),
            ("engine", string(&self.engine)),
            ("test_interval", int(self.test_interval)),
            ("warm_up", int(self.warm_up)),
            ("kind", string(&self.kind)),
            ("client_host", string("cloud")),
            ("rerun", int(self.rerun)),
            ("more", int(self.more)),
            ("reserve_s", Json::Float(self.reserve_s)),
            ("cpus", string(&self.cpus)),
            (
                "stagger",
                match self.stagger {
                    Num::Int(n) => Json::Int(n),
                    Num::Float(n) => Json::Float(n),
                },
            ),
        ])
    }
}

/// The validated settings, report plan and ordered Round Runs of a matrix.
#[derive(Clone, Debug)]
pub struct Matrix {
    pub settings: Json,
    pub plan: Json,
    pub runs: Vec<ScaleRun>,
}

/// CPython's compensated sums, with the re-runs and close-call reserve.
pub fn duration_s(runs: &[ScaleRun]) -> f64 {
    let seconds = num::sum(
        &runs
            .iter()
            .map(|r| Num::Float(r.seconds()))
            .collect::<Vec<_>>(),
    )
    .to_f64();
    let reserve = num::sum(
        &runs
            .iter()
            .map(|r| Num::Float(r.reserve_s))
            .collect::<Vec<_>>(),
    )
    .to_f64();
    seconds * (1.0 + RERUN_SHARE) + reserve
}

pub(crate) fn dict<const N: usize>(items: [(&str, Json); N]) -> Json {
    Json::Dict(items.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}
pub(crate) fn string(value: &str) -> Json {
    Json::Str(value.to_owned())
}
pub(crate) fn int(value: i64) -> Json {
    Json::Int(value.into())
}
pub(crate) fn get<'a>(value: &'a Json, key: &str) -> &'a Json {
    value
        .as_dict()
        .and_then(|d| d.get(key))
        .unwrap_or(&Json::Null)
}
fn default<'a>(value: &'a Json, key: &str, otherwise: &'a Json) -> &'a Json {
    value
        .as_dict()
        .and_then(|d| d.get(key))
        .unwrap_or(otherwise)
}
fn has(value: &Json, key: &str) -> bool {
    value.as_dict().is_some_and(|d| d.contains_key(key))
}
pub(crate) fn text(value: &Json) -> Result<&str, Error> {
    if let Json::Str(s) = value {
        Ok(s)
    } else {
        Err(Error(format!(
            "expected a string, not {}",
            value.type_name()
        )))
    }
}
pub(crate) fn integer(value: &Json) -> Result<i64, Error> {
    if let Json::Int(n) = value {
        i64::try_from(*n).map_err(|_| Error("integer out of range".into()))
    } else {
        Err(Error(format!(
            "expected a whole number, not {}",
            value.type_name()
        )))
    }
}
pub(crate) fn list(value: &Json) -> Result<&[Json], Error> {
    if let Json::List(v) = value {
        Ok(v)
    } else {
        Err(Error(format!("expected a list, not {}", value.type_name())))
    }
}
fn array<'a>(value: &'a Json, key: &str) -> Result<&'a [Json], Error> {
    list(default(value, key, &EMPTY_LIST))
}
fn object(value: &Json) -> Result<&IndexMap<String, Json>, Error> {
    value
        .as_dict()
        .ok_or_else(|| Error(format!("expected a table, not {}", value.type_name())))
}

fn only_keys(value: &Json, keys: &[&str], where_: &str) -> Result<(), Error> {
    for key in object(value)?.keys() {
        require(
            keys.contains(&key.as_str()),
            format!("{where_}: unknown key {}", repr_str(key)),
        )?;
    }
    Ok(())
}
fn number(value: &Json) -> Result<f64, Error> {
    Num::from_json(value)
        .map(Num::to_f64)
        .ok_or_else(|| Error("expected a number".into()))
}
pub(crate) fn repr(value: &Json) -> String {
    match value {
        Json::Null => "None".into(),
        Json::Bool(true) => "True".into(),
        Json::Bool(false) => "False".into(),
        Json::Int(n) => n.to_string(),
        Json::Float(n) => num::repr(*n),
        Json::Str(s) => repr_str(s),
        Json::List(v) => format!("[{}]", v.iter().map(repr).collect::<Vec<_>>().join(", ")),
        Json::Dict(d) => format!(
            "{{{}}}",
            d.iter()
                .map(|(k, v)| format!("{}: {}", repr_str(k), repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}
fn require(ok: bool, why: impl Into<String>) -> Result<(), Error> {
    if ok { Ok(()) } else { Err(Error(why.into())) }
}
fn between(value: &Json, lo: i64, hi: i64) -> bool {
    integer(value).is_ok_and(|n| (lo..=hi).contains(&n))
}
fn engines(value: &Json, where_: &str) -> Result<Vec<String>, Error> {
    let mut names = Vec::new();
    for name in list(value)? {
        let name = text(name)?;
        require(
            ENGINES.contains(&name),
            format!(
                "{where_}: unknown engine {}; one of {}",
                repr_str(name),
                ENGINES.join(", ")
            ),
        )?;
        names.push(name.to_owned());
    }
    require(
        names.iter().collect::<BTreeSet<_>>().len() == names.len(),
        format!("{where_}: an engine is named twice"),
    )?;
    Ok(names)
}

/// A command's environment, checked before any command is built.
pub fn environment(value: &Json, where_: &str) -> Result<Json, Error> {
    let env = value
        .as_dict()
        .ok_or_else(|| Error(format!("{where_} is a table of NAME = value")))?;
    let mut found = IndexMap::new();
    for (name, value) in env {
        let value_text = match value {
            Json::Int(n) => Some(n.to_string()),
            Json::Str(s) => Some(s.clone()),
            _ => None,
        };
        let valid_name = !name.is_empty()
            && name
                .bytes()
                .enumerate()
                .all(|(i, b)| b == b'_' || b.is_ascii_uppercase() || i > 0 && b.is_ascii_digit());
        let valid_value = value_text.as_ref().is_some_and(|v| {
            v.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.,:/+=-".contains(&b))
        });
        let shown = repr(value);
        require(
            valid_name && valid_value,
            format!(
                "{where_}: {} = {shown} isn't NAME = a plain value",
                repr_str(name)
            ),
        )?;
        found.insert(
            name.clone(),
            string(value_text.as_deref().unwrap_or_default()),
        );
    }
    Ok(Json::Dict(found))
}

fn stagger(value: &Json, where_: &str) -> Result<(), Error> {
    require(
        number(value).is_ok_and(|n| (0.0..=0.1).contains(&n)),
        format!("{where_} is 0-0.1 s between the Clients' starts"),
    )
}

/// Whether a matrix selects the Server-scale Round.
pub fn is_scale(data: &Json) -> bool {
    get(get(data, "round"), "kind") == &string("server-scale")
}

/// Parse directly into the order-preserving JSON type shared by the tooling.
pub fn parse(source: &str) -> Result<Matrix, Error> {
    let data: Json = toml::from_str(source).map_err(|e| Error(format!("{e}")))?;
    expand(&data)
}

/// Validate before expanding. Unknown fields and non-integral numbers are
/// refused where the accepted design permits stricter input checks (§7.7).
pub fn expand(data: &Json) -> Result<Matrix, Error> {
    require(
        is_scale(data),
        "not a Server-scale matrix: [round] kind = \"server-scale\"",
    )?;
    let settings = get(data, "round");
    only_keys(
        data,
        &[
            "round",
            "case",
            "p1",
            "confine",
            "hold_off_ab",
            "grouping",
            "precision",
        ],
        "matrix",
    )?;
    only_keys(
        settings,
        &[
            "kind",
            "server_type",
            "client_type",
            "test_interval",
            "warm_up",
            "engines",
            "calibration",
            "runs_at_target",
            "runs_elsewhere",
            "repeats",
            "close_call_runs",
            "close_call_most",
            "obudpst_pinned",
            "obudpst_instances",
            "must_have",
            "reading",
            "executor",
            "keep_server_binaries",
            "stagger",
            "client_env",
            "probe_stop_engines",
            "probe_stop_cases",
        ],
        "[round]",
    )?;
    if has(data, "p1") {
        only_keys(
            get(data, "p1"),
            &["engines", "test_connections", "repeats", "rows"],
            "[p1]",
        )?;
    }
    if has(data, "confine") {
        only_keys(
            get(data, "confine"),
            &["candidates", "count", "runs"],
            "[confine]",
        )?;
    }
    let interval = integer(default(settings, "test_interval", &Json::Int(10)));
    let warm_up = integer(default(settings, "warm_up", &Json::Int(2)));
    let measured = engines(
        default(settings, "engines", &Json::List(vec![string("udpstp")])),
        "engines",
    )?;
    let calibration = engines(
        default(settings, "calibration", &Json::List(Vec::new())),
        "calibration",
    )?;
    require(
        !measured.iter().any(|e| calibration.contains(e)),
        "an engine is both measured and calibration",
    )?;
    require(
        interval.as_ref().is_ok_and(|n| (5..=60).contains(n))
            && warm_up.as_ref().is_ok_and(|n| (1..=10).contains(n)),
        "test_interval 5-60 s, warm_up 1-10 s",
    )?;
    let (interval, warm_up) = (interval?, warm_up?);
    let at_target = object(default(settings, "runs_at_target", &EMPTY_DICT))?;
    for (engine, n) in at_target {
        require(
            measured.contains(engine) || calibration.contains(engine),
            format!(
                "runs_at_target: {} is neither measured nor calibration",
                repr_str(engine)
            ),
        )?;
        require(
            between(n, 1, 30),
            format!("runs_at_target: {engine}'s runs are 1-30"),
        )?;
    }
    let elsewhere = default(
        settings,
        "runs_elsewhere",
        default(settings, "repeats", &Json::Int(1)),
    );
    require(between(elsewhere, 1, 30), "runs_elsewhere is 1-30")?;
    for name in ["close_call_runs", "close_call_most"] {
        require(
            between(default(settings, name, &Json::Int(0)), 0, i64::MAX),
            format!("{name} is a count"),
        )?;
    }
    require(
        matches!(
            default(settings, "obudpst_pinned", &Json::Bool(false)),
            Json::Bool(_)
        ),
        "obudpst_pinned is true or false",
    )?;
    let instances = get(settings, "obudpst_instances");
    require(
        instances == &Json::Null || between(instances, 1, 64),
        "obudpst_instances is 1-64; without it, one per core of the Server host",
    )?;
    require(
        [string("obudpst"), string("target")].contains(default(
            settings,
            "must_have",
            &string("obudpst"),
        )),
        "must_have is one of obudpst, target",
    )?;
    require(
        [Json::Null, string("home-lab")].contains(get(settings, "reading")),
        "reading is \"home-lab\", or absent for the paid round's",
    )?;
    require(
        [string("pool"), string("per-test-connection")].contains(default(
            settings,
            "executor",
            &string("pool"),
        )),
        "executor is one of pool, per-test-connection, or absent for none checked",
    )?;
    require(
        matches!(
            default(settings, "keep_server_binaries", &Json::Bool(false)),
            Json::Bool(_)
        ),
        "keep_server_binaries is true or false",
    )?;
    stagger(
        default(settings, "stagger", &Json::Float(STAGGER_S)),
        "stagger",
    )?;
    let cases = array(data, "case")?;
    require(!cases.is_empty(), "the matrix has no [[case]]")?;
    let mut names = BTreeSet::new();
    for case in cases {
        object(case)?;
        only_keys(
            case,
            &[
                "name",
                "direction",
                "row",
                "counts",
                "target",
                "judged",
                "confined",
                "runs",
                "stagger",
                "engines",
                "server_env",
                "confined_as",
            ],
            "[[case]]",
        )?;
        let name = text(get(case, "name"))?;
        let stripped = name.replace('_', "");
        require(
            names.insert(name)
                && !stripped.is_empty()
                && stripped.chars().all(alphanumeric::contains),
            format!(
                "{}: case names are unique, letters and digits",
                repr_str(name)
            ),
        )?;
        require(
            [string("downstream"), string("upstream")].contains(get(case, "direction")),
            format!("{name}: direction is downstream or upstream"),
        )?;
        require(
            between(get(case, "row"), 1, 1002),
            format!("{name}: row 1-1002, whose packets fit a 1500-byte Server MTU"),
        )?;
        let counts = array(case, "counts")?;
        require(
            !counts.is_empty()
                && counts.iter().all(|n| between(n, 1, 250))
                && counts
                    .windows(2)
                    .all(|w| integer(&w[0]).ok() < integer(&w[1]).ok()),
            format!("{name}: counts rise, 1-250 Test Connections"),
        )?;
        require(
            matches!(get(case, "target"), Json::Int(_)) && counts.contains(get(case, "target")),
            format!("{name}: the target is one of the counts"),
        )?;
        for flag in ["judged", "confined"] {
            require(
                matches!(
                    default(case, flag, &Json::Bool(flag == "judged")),
                    Json::Bool(_)
                ),
                format!("{name}: {flag} is true or false"),
            )?;
        }
        if has(case, "runs") {
            require(
                between(get(case, "runs"), 1, 30),
                format!("{name}: runs is 1-30"),
            )?;
        }
        if has(case, "stagger") {
            stagger(get(case, "stagger"), &format!("{name}: stagger"))?;
        }
        let own = if has(case, "engines") {
            let own = engines(get(case, "engines"), &format!("{name}: engines"))?;
            require(
                !own.is_empty() && own.iter().all(|e| measured.contains(e)),
                format!("{name}: engines are some of the round's"),
            )?;
            own
        } else {
            measured.clone()
        };
        if get(case, "server_env").truthy() {
            let env = environment(get(case, "server_env"), &format!("{name}: server_env"))?;
            require(
                own.iter().any(|s| s == "udpstp"),
                format!("{name}: server_env is udpstp's Server's, which the case doesn't run"),
            )?;
            require(
                [string("pool"), string("per-test-connection")].contains(default(
                    &env,
                    EXECUTOR_ENV,
                    &string("pool"),
                )),
                format!("{name}: server_env's {EXECUTOR_ENV} is one of pool, per-test-connection"),
            )?;
        }
    }
    environment(default(settings, "client_env", &EMPTY_DICT), "client_env")?;
    let by_name: BTreeMap<&str, &Json> = cases
        .iter()
        .map(|c| Ok((text(get(c, "name"))?, c)))
        .collect::<Result<_, Error>>()?;
    for case in cases {
        if has(case, "confined_as") {
            let other = get(case, "confined_as")
                .as_str()
                .and_then(|name| by_name.get(name));
            require(
                !get(case, "confined").truthy()
                    && other.is_some_and(|c| get(c, "confined").truthy()),
                format!(
                    "{}: confined_as names a confined case, and the case isn't one",
                    text(get(case, "name"))?
                ),
            )?;
        }
    }
    let (pairs, grouping) = paired_cases(data, cases, &by_name)?;
    let names_used: Vec<&str> = pairs.iter().flatten().copied().chain(grouping).collect();
    require(
        names_used.iter().collect::<BTreeSet<_>>().len() == names_used.len(),
        "a case is in more than one of [hold_off_ab], [grouping] and [precision]",
    )?;
    for engine in engines(
        default(settings, "probe_stop_engines", &Json::List(Vec::new())),
        "probe_stop_engines",
    )? {
        require(
            measured.contains(&engine),
            format!("probe_stop_engines: {engine} has no curve"),
        )?;
    }
    for name in array(settings, "probe_stop_cases")? {
        let name = text(name)?;
        require(
            names.contains(name),
            format!("probe_stop_cases: no case {}", repr_str(name)),
        )?;
    }
    let confined: Vec<_> = cases
        .iter()
        .filter(|c| get(c, "confined").truthy())
        .collect();
    let confine = get(data, "confine");
    require(
        confined.is_empty() || confine.truthy(),
        "a confined case needs [confine], the calibration step that chooses its CPUs",
    )?;
    if confine.truthy() {
        check_confine(confine, &confined, &measured)?;
    }
    let plan = plan(data)?;
    let mut runs = Vec::new();
    let mut add = |repeat: i64,
                   case: &Json,
                   count: i64,
                   engine: &str,
                   kind: &str,
                   cpus: Option<&str>|
     -> Result<f64, Error> {
        let where_ = cpus.unwrap_or(
            if get(case, "confined").truthy() || get(case, "confined_as").truthy() {
                "confined"
            } else {
                ""
            },
        );
        let run = ScaleRun {
            index: runs.len() as i64 + 1,
            repeat,
            case: text(get(case, "name"))?.into(),
            direction: text(get(case, "direction"))?.into(),
            row: integer(get(case, "row"))?,
            count,
            engine: engine.into(),
            test_interval: interval,
            warm_up,
            kind: kind.into(),
            rerun: 0,
            more: 0,
            reserve_s: 0.0,
            cpus: where_.into(),
            stagger: Num::from_json(default(
                case,
                "stagger",
                default(settings, "stagger", &Json::Float(STAGGER_S)),
            ))
            .ok_or_else(|| Error("stagger must be a number".into()))?,
        };
        let seconds = run.seconds();
        runs.push(run);
        Ok(seconds)
    };
    if confine.truthy() {
        for candidate in array(confine, "candidates")? {
            for repeat in 1..=integer(default(confine, "runs", &Json::Int(3)))? {
                for case in &confined {
                    add(
                        repeat,
                        case,
                        integer(get(confine, "count"))?,
                        "udpstp",
                        "confine",
                        Some(text(candidate)?),
                    )?;
                }
            }
        }
    }
    for (k, case) in cases.iter().enumerate() {
        if names_used.contains(&text(get(case, "name"))?) {
            continue;
        }
        let own = if has(case, "engines") {
            engines(get(case, "engines"), "engines")?
        } else {
            measured.clone()
        };
        for engine in order::rotate(&own, k as i64) {
            let count = array(case, "counts")?
                .last()
                .ok_or_else(|| Error("no counts".into()))?;
            add(0, case, integer(count)?, &engine, "probe", None)?;
        }
    }
    let p1 = get(data, "p1");
    if p1.truthy() {
        let own = engines(
            default(p1, "engines", &Json::List(Vec::new())),
            "p1 engines",
        )?;
        let n = default(p1, "test_connections", &Json::Int(6));
        require(
            between(n, 1, 6),
            "p1: at most 6 Test Connections (their send timestamps load the engine)",
        )?;
        let repeats = integer(default(p1, "repeats", &Json::Int(1)))
            .map_err(|_| Error("p1: repeats is a whole number".into()))?;
        for repeat in 1..=repeats {
            for row in array(p1, "rows")? {
                require(
                    between(row, 1, 1002),
                    format!("p1: row {} isn't 1-1002", repr(row)),
                )?;
                for engine in order::turn(&own, repeat - 1) {
                    require(
                        engine != "obudpst",
                        "p1: OB-UDPST reports no send timestamps",
                    )?;
                    let case = dict([
                        ("name", string("P1")),
                        (
                            "direction",
                            string(if engine == "udpstp" {
                                "upstream"
                            } else {
                                "downstream"
                            }),
                        ),
                        ("row", row.clone()),
                    ]);
                    add(repeat, &case, integer(n)?, &engine, "p1", None)?;
                }
            }
        }
    }
    let all: Vec<_> = measured.iter().chain(&calibration).cloned().collect();
    let mut planned = BTreeMap::new();
    for case in cases {
        for count in array(case, "counts")? {
            for engine in &all {
                planned.insert(
                    (text(get(case, "name"))?, integer(count)?, engine.as_str()),
                    runs_for(&plan, case, integer(count)?, engine)?,
                );
            }
        }
    }
    let total = planned.values().copied().max().unwrap_or(0);
    let when: BTreeMap<_, _> = planned
        .iter()
        .filter(|(_, n)| **n != 0)
        .map(|(key, n)| (*key, order::spread(*n, total)))
        .collect();
    let mut times = BTreeMap::new();
    let mut pair_times = vec![0; pairs.len()];
    let mut total_s = 0.0;
    for repeat in 1..=total {
        let mut ordered: Vec<_> = cases.iter().collect();
        for (p, [first, _]) in pairs.iter().enumerate() {
            let first_case = by_name[first];
            if array(first_case, "counts")?.iter().any(|n| {
                when.get(&(*first, integer(n).unwrap_or(0), "udpstp"))
                    .is_some_and(|v| v.contains(&repeat))
            }) {
                pair_times[p] += 1;
                if pair_times[p] % 2 == 0
                    && let Some(i) = ordered
                        .iter()
                        .position(|c| get(c, "name") == &string(first))
                {
                    ordered.swap(i, i + 1);
                }
            }
        }
        for case in ordered {
            let name = text(get(case, "name"))?;
            for count in array(case, "counts")? {
                let count = integer(count)?;
                let present: Vec<_> = all
                    .iter()
                    .filter(|e| {
                        when.get(&(name, count, e.as_str()))
                            .is_some_and(|v| v.contains(&repeat))
                    })
                    .cloned()
                    .collect();
                if present.is_empty() {
                    continue;
                }
                let t = times.entry((name, count, present.clone())).or_insert(0);
                for engine in order::turn(&present, *t) {
                    total_s += add(repeat, case, count, &engine, "curve", None)?;
                }
                *t += 1;
            }
        }
    }
    let most = if get(settings, "close_call_runs").truthy() {
        integer(default(settings, "close_call_most", &Json::Int(0)))?
    } else {
        0
    };
    let curve_count = runs.iter().filter(|r| r.kind == "curve").count();
    if curve_count != 0
        && most != 0
        && let Some(first) = runs.first_mut()
    {
        first.reserve_s = most as f64 * total_s / curve_count as f64;
    }
    Ok(Matrix {
        settings: settings.clone(),
        plan,
        runs,
    })
}

/// How many Round Runs a point needs, including grouping's judged case.
pub fn runs_for(plan: &Json, case: &Json, count: i64, engine: &str) -> Result<i64, Error> {
    let elsewhere = integer(default(
        plan,
        "runs_elsewhere",
        default(plan, "repeats", &Json::Int(1)),
    ))?;
    let at_target = (default(case, "judged", &Json::Bool(true)).truthy()
        || get(case, "name") == get(get(plan, "grouping"), "arm"))
        && count == integer(get(case, "target"))?;
    if has(case, "engines") && !array(case, "engines")?.contains(&string(engine)) {
        return Ok(0);
    }
    let at = || {
        integer(get(case, "runs").or(default(
            get(plan, "runs_at_target"),
            engine,
            &int(elsewhere),
        )))
    };
    if array(plan, "calibration")?.contains(&string(engine)) {
        return if at_target { at() } else { Ok(0) };
    }
    if !array(plan, "engines")?.contains(&string(engine)) {
        return Ok(0);
    }
    if get(case, "runs").truthy() {
        return integer(get(case, "runs"));
    }
    if at_target { at() } else { Ok(elsewhere) }
}

fn plan(data: &Json) -> Result<Json, Error> {
    let settings = get(data, "round");
    let p1 = get(data, "p1");
    let confine = get(data, "confine");
    let cases = array(data, "case")?
        .iter()
        .map(|case| {
            let mut case = object(case)?.clone();
            case.entry("judged".into()).or_insert(Json::Bool(true));
            case.entry("confined".into()).or_insert(Json::Bool(false));
            Ok(Json::Dict(case))
        })
        .collect::<Result<_, Error>>()?;
    let mut precision = dict([(
        "pairs",
        default(get(data, "precision"), "pairs", &Json::List(Vec::new())).clone(),
    )]);
    if let Json::Dict(p) = &mut precision
        && has(get(data, "precision"), "confirmation")
    {
        p.insert(
            "confirmation".into(),
            get(get(data, "precision"), "confirmation").clone(),
        );
    }
    Ok(dict([
        ("cases", Json::List(cases)),
        (
            "confine",
            if confine.truthy() {
                dict([
                    ("candidates", get(confine, "candidates").clone()),
                    ("count", get(confine, "count").clone()),
                    ("runs", default(confine, "runs", &Json::Int(3)).clone()),
                ])
            } else {
                EMPTY_DICT.clone()
            },
        ),
        ("reading", get(settings, "reading").clone()),
        (
            "stagger",
            default(settings, "stagger", &Json::Float(STAGGER_S)).clone(),
        ),
        (
            "engines",
            default(settings, "engines", &Json::List(Vec::new())).clone(),
        ),
        (
            "calibration",
            default(settings, "calibration", &Json::List(Vec::new())).clone(),
        ),
        (
            "obudpst_pinned",
            default(settings, "obudpst_pinned", &Json::Bool(false)).clone(),
        ),
        (
            "obudpst_instances",
            get(settings, "obudpst_instances").clone(),
        ),
        (
            "must_have",
            default(settings, "must_have", &string("obudpst")).clone(),
        ),
        (
            "runs_at_target",
            default(settings, "runs_at_target", &EMPTY_DICT).clone(),
        ),
        (
            "runs_elsewhere",
            default(
                settings,
                "runs_elsewhere",
                default(settings, "repeats", &Json::Int(1)),
            )
            .clone(),
        ),
        (
            "probe",
            dict([
                (
                    "stop_engines",
                    default(settings, "probe_stop_engines", &Json::List(Vec::new())).clone(),
                ),
                (
                    "stop_cases",
                    default(settings, "probe_stop_cases", &Json::List(Vec::new())).clone(),
                ),
            ]),
        ),
        (
            "close_call",
            dict([
                (
                    "runs",
                    default(settings, "close_call_runs", &Json::Int(0)).clone(),
                ),
                (
                    "most",
                    default(settings, "close_call_most", &Json::Int(0)).clone(),
                ),
            ]),
        ),
        (
            "p1",
            dict([
                (
                    "engines",
                    default(p1, "engines", &Json::List(Vec::new())).clone(),
                ),
                ("rows", default(p1, "rows", &Json::List(Vec::new())).clone()),
                (
                    "repeats",
                    if p1.truthy() {
                        default(p1, "repeats", &Json::Int(1)).clone()
                    } else {
                        Json::Int(0)
                    },
                ),
            ]),
        ),
        (
            "client_env",
            environment(default(settings, "client_env", &EMPTY_DICT), "client_env")?,
        ),
        (
            "hold_off_ab",
            default(data, "hold_off_ab", &EMPTY_DICT).clone(),
        ),
        ("executor", get(settings, "executor").clone()),
        (
            "keep_server_binaries",
            default(settings, "keep_server_binaries", &Json::Bool(false)).clone(),
        ),
        ("grouping", default(data, "grouping", &EMPTY_DICT).clone()),
        ("precision", precision),
    ]))
}

fn check_confine(confine: &Json, confined: &[&Json], measured: &[String]) -> Result<(), Error> {
    require(!confined.is_empty(), "[confine] without a confined case")?;
    require(
        measured.iter().any(|e| e == "udpstp"),
        "[confine]: the calibration step runs udpstp, which the matrix doesn't",
    )?;
    let candidates = get(confine, "candidates");
    let valid = list(candidates).is_ok_and(|c| {
        !c.is_empty()
            && c.iter().all(|v| v.as_str().is_some_and(is_cpu_list))
            && c.iter()
                .filter_map(Json::as_str)
                .collect::<BTreeSet<_>>()
                .len()
                == c.len()
    });
    require(
        valid,
        "[confine]: candidates are distinct lists of logical CPUs, such as \"1,5\"",
    )?;
    require(
        between(get(confine, "count"), 1, 250),
        "[confine]: count is 1-250 Test Connections",
    )?;
    require(
        between(default(confine, "runs", &Json::Int(3)), 1, 10),
        "[confine]: runs is 1-10",
    )
}

fn is_cpu_list(s: &str) -> bool {
    !s.is_empty()
        && s.split(',').all(|part| {
            let ends: Vec<_> = part.split('-').collect();
            ends.len() <= 2
                && ends
                    .iter()
                    .all(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
        })
}

type Pairs<'a> = (Vec<[&'a str; 2]>, Option<&'a str>);

fn paired_cases<'a>(
    data: &'a Json,
    cases: &'a [Json],
    by_name: &BTreeMap<&str, &'a Json>,
) -> Result<Pairs<'a>, Error> {
    let lookup = |name: &Json| name.as_str().and_then(|s| by_name.get(s)).copied();
    // Compatibility of two arms uses Python's numeric equality. The
    // documents still keep kinds and signs for byte-identical metadata.
    let same = |a: &Json, b: &Json, keys: &[&str]| {
        keys.iter().all(|k| {
            let (a, b) = (get(a, k), get(b, k));
            if let Some((a, b)) = Num::from_json(a).zip(Num::from_json(b)) {
                a.py_cmp(b) == Some(std::cmp::Ordering::Equal)
            } else {
                a == b
            }
        })
    };
    let own = |c: &Json| get(c, "engines") == &Json::List(vec![string("udpstp")]);
    let reported = |c: &Json| {
        !default(c, "judged", &Json::Bool(true)).truthy() && !get(c, "confined").truthy() && own(c)
    };
    let adjacent = |a: &Json, b: &Json| {
        cases
            .iter()
            .position(|c| std::ptr::eq(c, a))
            .zip(cases.iter().position(|c| std::ptr::eq(c, b)))
            .is_some_and(|(i, j)| j == i + 1)
    };
    let mut pairs = Vec::new();
    if has(data, "hold_off_ab") {
        let ab = get(data, "hold_off_ab");
        let arms = lookup(get(ab, "fix")).zip(lookup(get(ab, "old")));
        let ok = object(ab)
            .is_ok_and(|d| d.len() == 2 && d.contains_key("fix") && d.contains_key("old"))
            && arms.is_some_and(|(a, b)| {
                same(
                    a,
                    b,
                    &[
                        "direction",
                        "row",
                        "counts",
                        "target",
                        "runs",
                        "stagger",
                        "confined_as",
                    ],
                ) && reported(a)
                    && reported(b)
                    && !get(a, "server_env").truthy()
                    && get(b, "server_env").truthy()
                    && adjacent(a, b)
            });
        require(
            ok,
            "[hold_off_ab]: fix and old name two cases of udpstp alone, reported, not judged, one after the other, alike but for old's server_env",
        )?;
        pairs.push([text(get(ab, "fix"))?, text(get(ab, "old"))?]);
    }
    let mut grouping_name = None;
    if has(data, "grouping") {
        let grouping = get(data, "grouping");
        let arms = lookup(get(grouping, "case")).zip(lookup(get(grouping, "arm")));
        let ok = object(grouping)
            .is_ok_and(|d| d.len() == 2 && d.contains_key("case") && d.contains_key("arm"))
            && arms.is_some_and(|(case, arm)| {
                let env = get(arm, "server_env");
                let window = match get(env, GROUPING_ENV) {
                    Json::Str(s) if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) => {
                        s.parse::<u64>().ok()
                    }
                    Json::Int(n) => u64::try_from(*n).ok(),
                    _ => None,
                };
                let counts = array(arm, "counts")
                    .ok()
                    .zip(array(case, "counts").ok())
                    .is_some_and(|(a, b)| a.iter().all(|n| b.contains(n)));
                default(case, "judged", &Json::Bool(true)).truthy()
                    && reported(arm)
                    && same(case, arm, &["direction", "row", "target"])
                    && counts
                    && get(arm, "confined_as")
                        == if get(case, "confined").truthy() {
                            get(case, "name")
                        } else {
                            &Json::Null
                        }
                    && window.is_some_and(|n| n > 0)
            });
        require(
            ok,
            format!(
                "[grouping]: case names a judged case and arm another of udpstp alone, reported, not judged, alike in direction, row and target, at some of its counts, confined_as it where it is confined, with {GROUPING_ENV} a positive number of µs in its server_env"
            ),
        )?;
        grouping_name = Some(text(get(grouping, "arm"))?);
    }
    if has(data, "precision") {
        let precision = get(data, "precision");
        let ps = get(precision, "pairs");
        let ok = list(ps).is_ok_and(|l| !l.is_empty())
            && object(precision)
                .is_ok_and(|d| d.keys().all(|k| k == "pairs" || k == "confirmation"))
            && matches!(
                default(precision, "confirmation", &Json::Bool(false)),
                Json::Bool(_)
            );
        require(
            ok,
            "[precision]: pairs is a list of [pooled case, per-Test-Connection case], and confirmation true or false",
        )?;
        let mut named = BTreeSet::new();
        for pair in list(ps)? {
            let pair_values = list(pair)?;
            let arms = if pair_values.len() == 2 {
                lookup(&pair_values[0]).zip(lookup(&pair_values[1]))
            } else {
                None
            };
            let ok = arms.is_some_and(|(a, b)| {
                let valid_arm = |c: &Json| {
                    reported(c)
                        && !get(c, "confined_as").truthy()
                        && get(c, "direction") == &string("downstream")
                        && get(c, "counts") == &Json::List(vec![Json::Int(1)])
                        && has(c, "runs")
                };
                let envs = object(get(a, "server_env"))
                    .ok()
                    .zip(object(get(b, "server_env")).ok());
                let valid_envs = envs.is_some_and(|(a, b)| {
                    let (mut a, mut b) = (a.clone(), b.clone());
                    a.shift_remove(EXECUTOR_ENV) == Some(string("pool"))
                        && b.shift_remove(EXECUTOR_ENV) == Some(string("per-test-connection"))
                        && a == b
                        && a.get(STAMPS_ENV) == Some(&string("1"))
                });
                valid_arm(a)
                    && valid_arm(b)
                    && adjacent(a, b)
                    && same(a, b, &["row", "runs", "stagger"])
                    && valid_envs
                    && !named.contains(get(a, "name").as_str().unwrap_or_default())
                    && !named.contains(get(b, "name").as_str().unwrap_or_default())
            });
            let shown = repr(pair);
            require(
                ok,
                format!(
                    "[precision]: {shown}: two cases of udpstp alone, one Test Connection Downstream on the whole host, reported, not judged, with their runs, one after the other, alike but for {EXECUTOR_ENV}, pool then per-test-connection, with {STAMPS_ENV} = \"1\""
                ),
            )?;
            let p = [text(&pair_values[0])?, text(&pair_values[1])?];
            named.extend(p);
            pairs.push(p);
        }
    }
    Ok((pairs, grouping_name))
}

/// A row's IP-Layer rate in Mbit/s, through the matrix's last row, 1002.
pub fn row_mbps(row: i64) -> i64 {
    if row <= 1000 {
        row
    } else {
        1100 + 100 * (row - 1001)
    }
}

/// Load PDUs per second in the Sending Rate Table's two Transmitters.
pub fn row_datagrams_per_s(row: i64) -> i64 {
    if row > 1000 {
        return 100_000;
    }
    let (hundreds, tens, units) = if row == 1000 {
        (10, 0, 0)
    } else {
        (row / 100, row / 10 % 10, row % 10)
    };
    10_000 * hundreds + 1000 * tens + if units != 0 { 1000 } else { 0 }
}

/// Mean IP packet size, keeping an integral size an int as Python does.
pub fn row_packet_bytes(row: i64) -> Num {
    let size = row_mbps(row) as f64 * 1e6 / 8.0 / row_datagrams_per_s(row) as f64;
    if size == size.trunc() {
        Num::Int(size as i128)
    } else {
        Num::Float(size)
    }
}
