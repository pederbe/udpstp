//! The port against the Python's own answers: every case of
//! `tests/golden/*.json`, which `scripts/bench/tests/export_goldens.py`
//! wrote from the Python tooling, its reference implementation
//!. A golden is never rewritten
//! from the Rust's output: a changed one is a changed Python.
//!
//! The files are read with `serde_json::Value`, not with `Json::parse`,
//! which is under test: a fault in it would misread the expected values as
//! it misreads the inputs, and pass.

mod common;

use std::path::Path;

use bench_rounds::py::Json;
use serde_json::Value;

fn check(file: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(file);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
    let golden: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path:?}: {e}"));
    let cases = golden["cases"].as_array().expect("cases");
    assert!(!cases.is_empty(), "{file} has no cases");
    let mut disagree = Vec::new();
    for case in cases {
        let field = |name: &str| {
            case.get(name)
                .unwrap_or_else(|| panic!("no {name}: {case}"))
        };
        let name = field("fn").as_str().expect("a name");
        let args: Vec<Json> = field("args")
            .as_array()
            .expect("arguments")
            .iter()
            .map(common::untag)
            .collect();
        let python = field("python");
        // "refuses": the Python read what the port refuses; "may-refuse":
        // a garbled input, which the port may refuse or must read as the
        // Python did.
        let may_refuse = matches!(
            case.get("port").and_then(Value::as_str),
            Some("refuses" | "may-refuse")
        );
        let port = common::port(name, &args);
        if !common::agrees(python, &port, may_refuse) {
            disagree.push(format!(
                "{name}{args:?}: the Python {python}, the port {port:?}"
            ));
        }
    }
    assert!(
        disagree.is_empty(),
        "{} of {} cases of {file} disagree, the first:\n{}",
        disagree.len(),
        cases.len(),
        disagree[..disagree.len().min(15)].join("\n")
    );
}

#[test]
fn py_reads_json_and_text_as_the_python_does() {
    check("py.json");
}

#[test]
fn num_computes_as_the_python_does() {
    check("num.json");
}

#[test]
fn quote_quotes_as_shlex_does() {
    check("quote.json");
}

#[test]
fn redact_redacts_as_the_python_does() {
    check("redact.json");
}

#[test]
fn stats_computes_as_the_rules_do() {
    check("stats.json");
}

#[test]
fn utc_writes_and_reads_times_as_the_python_does() {
    check("utc.json");
}

#[test]
fn repr_and_dumps_write_as_python_writes() {
    check("dumps.json");
}

#[test]
fn order_takes_turns_spreads_and_queues_as_the_python_does() {
    check("order.json");
}

#[test]
fn counters_are_read_as_the_python_reads_them() {
    check("counters.json");
}

#[test]
fn the_scripts_every_round_runs_are_the_pythons_byte_for_byte() {
    check("scripts.json");
}

#[test]
fn samples_are_judged_as_the_python_judges_them() {
    check("samples.json");
}

#[test]
fn cost_and_the_ledger_come_out_as_the_pythons() {
    check("cost.json");
}

#[test]
fn the_dead_man_switch_is_the_pythons_byte_for_byte() {
    check("dead_man.json");
}

#[test]
fn the_security_groups_rules_are_the_pythons() {
    check("plan.json");
}
