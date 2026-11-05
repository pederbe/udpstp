//! What the golden test and the differential harness share: the tagged
//! values `scripts/bench/tests/export_goldens.py` exchanges, each helper as
//! the port computes it under the name the script gives it, and when the
//! port's outcome agrees with the Python's.
//!
//! What the Python wrote, a golden file or the shim's answer, is read with
//! `serde_json::Value`, never with `Json::parse`, the parser under test: a
//! fault in it would misread the expected values as it misreads the
//! inputs, and pass.
#![allow(dead_code)]

use std::fmt::Write;

use bench_rounds::num::{self, Num};
use bench_rounds::py::{self, Json, PyError, PyErrorKind};
use bench_rounds::{quote, redact, stats, utc};
use serde_json::Value;

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

/// What the port did with a case.
#[derive(Debug)]
pub enum Outcome {
    /// The value it computed.
    Value(Json),
    /// It failed where Python raises: the exception's class, where the
    /// port knows which.
    Raises(Option<&'static str>),
    /// It refused what Python computes something from, as the design allows
    /// for what no producer writes (§7.7).
    Refuses,
}

/// A helper of the port, by the script's name for its Python.
pub fn port(name: &str, args: &[Json]) -> Outcome {
    use Outcome::{Raises, Refuses, Value};
    let text = |i: usize| args[i].as_str().expect("a string");
    let int = |i: usize| match args[i] {
        Json::Int(n) => n,
        ref other => panic!("not an int: {other:?}"),
    };
    let number = |i: usize| Num::from_json(&args[i]).expect("a number");
    let numbers = |i: usize| -> Vec<Num> {
        let list = args[i].as_list().expect("a list");
        list.iter()
            .map(|v| Num::from_json(v).expect("a number"))
            .collect()
    };
    let floats = |i: usize| -> Vec<f64> { numbers(i).into_iter().map(Num::to_f64).collect() };
    let doc = |i: usize| Json::parse(text(i)).expect("a document the script parsed");
    let python = |result: Result<&Json, PyError>| match result {
        Ok(value) => Value(value.clone()),
        Err(e) if e.kind() == PyErrorKind::Refused => Refuses,
        Err(e) => Raises(Some(e.kind().name())),
    };
    let strings =
        |items: Vec<&str>| Json::List(items.into_iter().map(|s| Json::Str(s.into())).collect());
    let number_value = |n: Num| match n {
        Num::Int(i) => Json::Int(i),
        Num::Float(f) => Json::Float(f),
    };
    let chars = || (0..=0x10ffff).filter_map(char::from_u32);
    match name {
        "isspace" => Value(Json::List(
            chars()
                .filter(|&c| py::is_space(c))
                .map(|c| Json::Int(c as i128))
                .collect(),
        )),
        "linebreaks" => Value(Json::List(
            chars()
                .filter(|&c| py::splitlines(&format!("x{c}y")).len() == 2)
                .map(|c| Json::Int(c as i128))
                .collect(),
        )),
        "json" => match Json::parse(text(0)) {
            Ok(value) => Value(value),
            Err(_) => Raises(Some("JSONDecodeError")),
        },
        "get" => python(doc(0).get(text(1))),
        "get_or" => {
            let (d, default) = (doc(0), doc(2));
            python(d.get_or(text(1), &default))
        }
        "item" => {
            let d = doc(0);
            match &args[1] {
                Json::Str(key) => python(d.item(key)),
                Json::Int(i) => python(d.at(i64::try_from(*i).expect("an index"))),
                other => panic!("not a key: {other:?}"),
            }
        }
        "or" => {
            let (a, b) = (doc(0), doc(1));
            Value(a.or(&b).clone())
        }
        "truthy" => Value(Json::Bool(doc(0).truthy())),
        "splitlines" => Value(strings(py::splitlines(text(0)))),
        "split" => Value(strings(py::split(text(0)))),
        "strip" => Value(Json::Str(py::strip(text(0)).into())),
        "median" => num::median(&numbers(0)).map_or(Raises(None), |n| Value(number_value(n))),
        "max" => num::max(&numbers(0)).map_or(Raises(None), |n| Value(number_value(n))),
        "sum" => Value(number_value(num::sum(&numbers(0)))),
        "round" => {
            let ndigits = match args[1] {
                Json::Null => None,
                Json::Int(n) => Some(u32::try_from(n).expect("ndigits")),
                ref other => panic!("not ndigits: {other:?}"),
            };
            number(0)
                .round(ndigits)
                .map_or(Raises(None), |n| Value(number_value(n)))
        }
        "fixed" => Value(Json::Str(num::fixed(
            number(0).to_f64(),
            usize::try_from(int(1)).expect("digits"),
        ))),
        "fmt" => {
            let v = (!args[0].is_null()).then(|| number(0));
            let digits = usize::try_from(int(1)).expect("digits");
            Value(Json::Str(num::fmt(v, digits, text(2))))
        }
        "quote" => Value(Json::Str(quote::quote(text(0)).into_owned())),
        "join" => {
            let words: Vec<&str> = args[0]
                .as_list()
                .expect("a list")
                .iter()
                .map(|w| w.as_str().expect("a word"))
                .collect();
            Value(Json::Str(quote::join(&words)))
        }
        "redact" => Value(Json::Str(redact::redact(text(0)))),
        "stats.median" => Value(stats::median(&floats(0)).map_or(Json::Null, Json::Float)),
        "percentile" => {
            let values = floats(0);
            if values.is_empty() {
                // The Python's percentile gives None for no values.
                return Value(Json::Null);
            }
            stats::percentile(&values, number(1).to_f64())
                .map_or(Raises(None), |v| Value(Json::Float(v)))
        }
        "quantile" => stats::quantile(&floats(0), number(1).to_f64())
            .map_or(Raises(None), |v| Value(Json::Float(v))),
        "fromtimestamp" => match utc::Utc::from_timestamp(number(0).to_f64()) {
            Some(t) => Value(strings(vec![
                &t.hms(),
                &t.year_month(),
                &format!("r{}", t.basic()),
                &t.isoformat(),
                &t.to_string(),
            ])),
            None => Raises(None),
        },
        "fromisoformat" => match utc::parse(text(0)) {
            Ok(micros) => Value(Json::List(vec![
                Json::Int(micros.into()),
                Json::Float(utc::seconds(micros)),
            ])),
            Err(_) => Refuses,
        },
        other => stage_b(other, args),
    }
}

/// Stage B's modules: the functions the script names after them.
fn stage_b(name: &str, args: &[Json]) -> Outcome {
    use Outcome::{Raises, Refuses, Value};
    use bench_rounds::aws::plan;
    use bench_rounds::cost::{self, ledger};
    use bench_rounds::{counters, dead_man, host, order, samples};
    use indexmap::IndexMap;
    use std::collections::BTreeMap;

    let text = |i: usize| args[i].as_str().expect("a string");
    let int = |i: usize| match args[i] {
        Json::Int(n) => n,
        ref other => panic!("not an int: {other:?}"),
    };
    let float = |i: usize| Num::from_json(&args[i]).expect("a number").to_f64();
    let list = |i: usize| args[i].as_list().expect("a list");
    let strings = |i: usize| -> Vec<&str> {
        list(i)
            .iter()
            .map(|s| s.as_str().expect("a string"))
            .collect()
    };
    let bools = |value: &Json| -> Vec<bool> {
        value
            .as_list()
            .expect("a list")
            .iter()
            .map(|b| matches!(b, Json::Bool(true)))
            .collect()
    };
    let doc = |i: usize| Json::parse(text(i)).expect("a document the script wrote");
    let python = |result: Result<Json, PyError>| match result {
        Ok(value) => Value(value),
        Err(e) if e.kind() == PyErrorKind::Refused => Refuses,
        Err(e) => Raises(Some(e.kind().name())),
    };
    let counts = |c: &counters::Counters| {
        Json::Dict(c.iter().map(|(k, v)| (k.clone(), Json::Int(*v))).collect())
    };
    let str_list = |items: Vec<String>| Json::List(items.into_iter().map(Json::Str).collect());
    match name {
        "repr" => Value(Json::Str(num::repr(float(0)))),
        "dumps" => Value(Json::Str(py::dumps(&args[0]))),
        "dumps_indent" => Value(Json::Str(py::dumps_indent(&args[0], 1))),
        "file_lines" => Value(str_list(
            py::file_lines(text(0))
                .into_iter()
                .map(str::to_string)
                .collect(),
        )),
        "rotate" => Value(Json::List(order::rotate(list(0), int(1) as i64))),
        "turn" => Value(Json::List(order::turn(list(0), int(1) as i64))),
        "spread" => Value(Json::List(
            order::spread(int(0) as i64, int(1) as i64)
                .into_iter()
                .map(|r| Json::Int(r.into()))
                .collect(),
        )),
        "queue" => {
            let runs = list(0).iter().map(|r| {
                let r = r.as_list().expect("a run");
                (r[0].clone(), r[1].clone())
            });
            let mut queue = order::Queue::new(runs, int(2) as usize);
            let answers = list(1)
                .iter()
                .map(|op| {
                    let op = op.as_list().expect("an operation");
                    match op[0].as_str().expect("its name") {
                        "next" => queue.next().map_or(Json::Null, |(id, _)| id),
                        "rerun" => {
                            queue.rerun((op[1].clone(), op[2].clone()));
                            Json::Null
                        }
                        "left" => Json::Int(queue.left(|(_, point)| point == &op[1]) as i128),
                        "drop" => {
                            queue.drop_runs(|(_, point)| point == &op[1]);
                            Json::Null
                        }
                        _ => Json::Bool(!queue.is_empty()),
                    }
                })
                .collect();
            Value(Json::List(answers))
        }
        "stop_now" => {
            let by_engine: BTreeMap<String, Vec<bool>> = args[2]
                .as_dict()
                .expect("a dict")
                .iter()
                .map(|(engine, history)| (engine.clone(), bools(history)))
                .collect();
            let why = order::stop_now(
                &bools(&args[0]),
                &bools(&args[1]),
                &by_engine,
                int(3) as usize,
                int(4) as usize,
            );
            Value(why.map_or(Json::Null, Json::Str))
        }
        "counters.parse" => counters::parse(text(0)).map_or(Refuses, |c| Value(counts(&c))),
        "counters.missing" => {
            counters::missing(text(0), &strings(1)).map_or(Refuses, |m| Value(str_list(m)))
        }
        "counters.allowance" => counters::allowance(text(0)).map_or(Refuses, |c| Value(counts(&c))),
        "counters.rise" => {
            let read = |i: usize| match &args[i] {
                Json::Null => Ok(None),
                Json::Str(t) => counters::parse(t).map(Some),
                other => panic!("not a counter file: {other:?}"),
            };
            match (read(0), read(1)) {
                (Ok(before), Ok(after)) => Value(counts(&counters::rise(
                    before.as_ref(),
                    after.as_ref(),
                    &strings(2),
                ))),
                _ => Refuses,
            }
        }
        "counters.platform_limits" => {
            let files: BTreeMap<String, Option<String>> = args[1]
                .as_dict()
                .expect("files")
                .iter()
                .map(|(name, t)| (name.clone(), t.as_str().map(str::to_string)))
                .collect();
            counters::platform_limits(text(0), &files).map_or(Refuses, |limits| {
                Value(Json::Dict(
                    limits
                        .iter()
                        .map(|(role, c)| (role.clone(), counts(c)))
                        .collect(),
                ))
            })
        }
        "counters.limits_text" => {
            let limits: IndexMap<String, counters::Counters> = args[0]
                .as_dict()
                .expect("limits")
                .iter()
                .map(|(role, c)| {
                    let c = c
                        .as_dict()
                        .expect("counters")
                        .iter()
                        .map(|(k, v)| match v {
                            Json::Int(v) => (k.clone(), *v),
                            other => panic!("not a count: {other:?}"),
                        })
                        .collect();
                    (role.clone(), c)
                })
                .collect();
            Value(Json::Str(counters::limits_text(&limits)))
        }
        "samples.window" => {
            let s = doc(0);
            let s = s.as_list().expect("samples");
            python(
                samples::window(s, float(1), float(2))
                    .map(|w| w.map_or(Json::Null, |(a, b)| Json::List(vec![a.clone(), b.clone()]))),
            )
        }
        "samples.cpu_use" => {
            let s = doc(0);
            let only: Option<Vec<i128>> = args[3].as_list().map(|o| {
                o.iter()
                    .map(|c| match c {
                        Json::Int(c) => *c,
                        other => panic!("not a logical CPU: {other:?}"),
                    })
                    .collect()
            });
            python(
                samples::cpu_use(
                    s.as_list().expect("samples"),
                    float(1),
                    float(2),
                    only.as_deref(),
                )
                .map(|u| u.map_or(Json::Null, |u| u.to_json())),
            )
        }
        "samples.wakeups" => {
            let s = doc(0);
            python(
                samples::wakeups(s.as_list().expect("samples"), float(1), float(2))
                    .map(|w| w.map_or(Json::Null, |w| w.to_json())),
            )
        }
        "samples.other_cpus" => {
            let s = doc(0);
            python(
                samples::other_cpus(s.as_list().expect("samples"), float(1), float(2))
                    .map(|o| o.map_or(Json::Null, Json::Float)),
            )
        }
        "cost.worst_case" => {
            let runs: Vec<cost::RunTransfer> = doc(0)
                .as_list()
                .expect("runs")
                .iter()
                .map(|run| {
                    let run = run.as_list().expect("a run");
                    let n = |v: &Json| Num::from_json(v).expect("a number").to_f64();
                    cost::RunTransfer {
                        home_downstream: (!run[0].is_null()).then(|| (n(&run[0]), n(&run[1]))),
                        download_gb: n(&run[2]),
                    }
                })
                .collect();
            let host_prices: Vec<Num> = list(1)
                .iter()
                .map(|p| Num::from_json(p).expect("a price"))
                .collect();
            let price = |key: &str| {
                args[3]
                    .as_dict()
                    .expect("prices")
                    .get(key)
                    .map_or(0.0, |p| Num::from_json(p).expect("a price").to_f64())
            };
            let prices = cost::Prices {
                public_ipv4_per_hour: price("public_ipv4_per_hour"),
                disk_per_host_hour: price("disk_per_host_hour"),
                internet_egress_per_gb: price("internet_egress_per_gb"),
            };
            Value(cost::WorstCase::new(&runs, &host_prices, float(2), &prices, float(4)).to_json())
        }
        "cost.month_spent" => python(ledger::month_spent(&doc(0), text(1), float(2)).map(
            |n| match n {
                Num::Int(i) => Json::Int(i),
                Num::Float(f) => Json::Float(f),
            },
        )),
        "ledger.save" => Value(Json::Str(py::dumps_indent(&doc(0), 1))),
        "ledger.add" => {
            let mut l = doc(0);
            let entry =
                ledger::entry(text(1), text(2), float(3), float(4), float(5)).expect("a time");
            python(ledger::add(&mut l, entry).map(|()| Json::Str(py::dumps_indent(&l, 1))))
        }
        "ledger.close" => {
            let mut l = doc(0);
            python(
                ledger::close(&mut l, text(1), text(2), float(3))
                    .map(|()| Json::Str(py::dumps_indent(&l, 1))),
            )
        }
        "dead_man.at_boot" => Value(Json::Str(dead_man::at_boot(
            dead_man::whole_seconds(float(0)).expect("a deadline"),
        ))),
        "dead_man.unit" => Value(Json::Str(dead_man::unit())),
        "dead_man.user_data" => Value(Json::Str(dead_man::user_data(
            int(0) as i64,
            text(1),
            dead_man::whole_seconds(float(2)).expect("a deadline"),
        ))),
        "dead_man.check_script" => Value(Json::Str(dead_man::CHECK_SCRIPT.to_string())),
        "counters.script" => Value(Json::Str(counters::SCRIPT.to_string())),
        "counters.qdisc_dropped" => Value(Json::Str(counters::QDISC_DROPPED.to_string())),
        "counters.throttles_script" => Value(Json::Str(counters::throttles_script(text(0)))),
        "host.environment_script" => Value(Json::Str(host::ENVIRONMENT_SCRIPT.to_string())),
        "host.quiet_script" => Value(Json::Str(host::QUIET_SCRIPT.to_string())),
        "dead_man.verdict" => {
            let status = i32::try_from(int(1)).expect("a status");
            let (said, message) = match dead_man::verdict(text(0), Some(status), text(2)) {
                Ok(m) => ("ok", m),
                Err(m) => ("refused", m),
            };
            Value(Json::List(vec![Json::Str(said.into()), Json::Str(message)]))
        }
        "aws.ingress_rules" => {
            let home_udp = matches!(args[2], Json::Bool(true));
            plan::ingress_rules(text(0), text(1), home_udp).map_or(Refuses, Value)
        }
        other => panic!("no port of {other}"),
    }
}

/// Whether the port's outcome agrees with the Python's result: the same
/// value, or a failure where Python raises. Where Python computes
/// something the port can't hold or refuses (`may_refuse`: a case of what
/// no producer writes), a failure agrees too; a value never does where
/// Python raises.
pub fn agrees(python: &Value, port: &Outcome, may_refuse: bool) -> bool {
    let result = python.as_object().expect("a result");
    if let Some(Value::String(raised)) = result.get("raises") {
        return match port {
            Outcome::Value(_) => false,
            Outcome::Raises(Some(name)) => name == raised,
            Outcome::Raises(None) | Outcome::Refuses => true,
        };
    }
    match (result.get("ok"), port) {
        (Some(value), Outcome::Value(computed)) => *computed == untag(value),
        (Some(_), _) => may_refuse,
        // "read" or "unholdable": the Python read what the port refuses.
        (None, port) => !matches!(port, Outcome::Value(_)),
    }
}
