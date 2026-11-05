//! The first Round's port against the Python's own answers: every case of
//! `tests/golden/*.json`, which `scripts/bench/tests/export_goldens.py`
//! wrote from the Python tooling, its reference implementation
//!. A golden is never rewritten
//! from the Rust's output: a changed one is a changed Python.
//!
//! The files are read with `serde_json::Value`, not with `Json::parse`: a
//! fault in it would misread the expected values as it misreads the
//! inputs, and pass.

mod common;

use std::path::Path;

use bench_rounds::num;
use bench_rounds::py::Json;
use common::{Outcome, agrees, dict, port, run_json, untag, worst_case};
use serde_json::Value;
use udpstp_bench::first_round::matrix::{self, ClientHost};

fn golden(file: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(file);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path:?}: {e}"))
}

/// Every case of a file of cases, as the port computes it, against the
/// Python's result and the case's mark.
fn check(file: &str) {
    check_cases(file, |name, _| Some(name.to_string()));
}

/// The cases `pick` takes, as the port computes them under the name it
/// gives (the case's own, or another way to the same answer).
fn check_cases(file: &str, pick: impl Fn(&str, &[Json]) -> Option<String>) {
    let golden = golden(file);
    let cases = golden["cases"].as_array().expect("cases");
    assert!(!cases.is_empty(), "{file} has no cases");
    let (mut checked, mut disagree) = (0, Vec::new());
    for (i, case) in cases.iter().enumerate() {
        let name = case["fn"].as_str().expect("a name");
        let args: Vec<Json> = case["args"]
            .as_array()
            .expect("arguments")
            .iter()
            .map(untag)
            .collect();
        let Some(name) = pick(name, &args) else {
            continue;
        };
        let name = name.as_str();
        checked += 1;
        let mark = case.get("port").and_then(Value::as_str);
        let outcome = port(name, &args);
        if !agrees(&case["python"], &outcome, mark) {
            // A Round's tree is long: its case's number says which.
            let short = |text: String| text.chars().take(600).collect::<String>();
            disagree.push(format!(
                "case {i}, {name}{} ({mark:?}):\n  the Python {}\n  the port {}",
                short(format!("{args:?}")),
                short(case["python"].to_string()),
                short(format!("{outcome:?}"))
            ));
        }
    }
    assert!(checked > 0, "{file}: no case was checked");
    assert!(
        disagree.is_empty(),
        "{} of {checked} cases of {file} disagree, the first:\n{}",
        disagree.len(),
        disagree[..disagree.len().min(10)].join("\n")
    );
}

#[test]
fn a_matrix_is_read_or_refused_as_the_python_reads_or_refuses_it() {
    check("matrix.json");
}

#[test]
fn the_commands_are_the_pythons_byte_for_byte() {
    check("commands.json");
}

#[test]
fn the_report_is_the_pythons_byte_for_byte_or_none() {
    // report.py's report of Rounds built on disk: tests/test_round.py's
    // fixtures, each built by the port of its round_dir and compared with
    // the Python's tree first (§7.3 item 1); Rounds of the committed
    // matrices as a Round leaves them; and the capped corpus of damage
    // (§7.3 item 2). And the report's parts: the environment line, the
    // cells, the Round's name. The Rounds are read from memory, as they
    // would be from disk.
    check("report.json");
}

#[test]
fn the_report_reads_a_round_on_disk_as_the_python_does() {
    // The same, through OutDir, for the Rounds small enough to write out
    // quickly: the fixtures and the damage, directories in place of files
    // and files that aren't UTF-8 among them.
    check_cases("report.json", |name, args| {
        let small = args
            .first()
            .and_then(|tree| tree.get("files").ok()?.as_dict())
            .is_some_and(|files| files.len() <= 100);
        (name == "first_round.report" && small).then(|| "first_round.report_on_disk".to_string())
    });
}

#[test]
fn every_committed_matrix_is_planned_as_the_python_plans_it() {
    // Each committed first-round matrix: its Round Runs, every field to the
    // label; and with and without the Tests from home, the labels kept,
    // their duration and hours, the roles, the checks at several
    // --max-hours and --home-cap-mbps, and the worst case at the pinned
    // prices (§7.1 "Run lists", §8.1 C1).
    let golden = golden("matrices.json");
    let matrices = golden["matrices"].as_array().expect("matrices");
    assert_eq!(matrices.len(), 3);
    for entry in matrices {
        let entry = untag(entry);
        let field = |name: &str| entry.item(name).unwrap_or_else(|e| panic!("{name}: {e}"));
        let path = field("path").as_str().expect("a path");
        let file = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(path);
        let text = std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("{file:?}: {e}"));
        assert_eq!(
            text,
            field("toml").as_str().unwrap(),
            "{path} changed from the independent reference: review and amend the pre-registration and its fixture"
        );
        let runs = matrix::parse(&text).unwrap_or_else(|e| panic!("{path}: {e}"));
        assert_eq!(
            Json::List(runs.iter().map(run_json).collect()),
            *field("runs"),
            "{path}"
        );
        for plan in field("plans").as_list().expect("plans") {
            let plan_field = |name: &str| plan.item(name).unwrap_or_else(|e| panic!("{name}: {e}"));
            let home_client = matches!(plan_field("home_client"), Json::Bool(true));
            let at = format!("{path}, home client {home_client}");
            let kept: Vec<_> = runs
                .iter()
                .filter(|r| home_client || r.client_host != ClientHost::Home)
                .cloned()
                .collect();
            let labels: Vec<Json> = kept.iter().map(|r| Json::Str(r.label())).collect();
            assert_eq!(Json::List(labels), *plan_field("labels"), "{at}");
            assert_eq!(Json::Int(kept.len() as i128), *plan_field("tests"), "{at}");
            assert_eq!(
                Json::Int(matrix::duration_s(&kept)),
                *plan_field("duration_s"),
                "{at}"
            );
            assert_eq!(
                Json::Float(matrix::matrix_hours(&kept)),
                *plan_field("matrix_hours"),
                "{at}"
            );
            let roles: Vec<Json> = matrix::roles(&kept)
                .into_iter()
                .map(|r| Json::Str(r.into()))
                .collect();
            assert_eq!(Json::List(roles), *plan_field("roles"), "{at}");
            for check in plan_field("checks").as_list().expect("checks") {
                let check = check.as_list().expect("a check");
                let (max_hours, cap) = (float(&check[0]), int(&check[1]));
                let port = match matrix::check(&kept, max_hours, cap) {
                    Ok(()) => Json::Null,
                    Err(refusal) => Json::Str(refusal),
                };
                assert_eq!(
                    port, check[2],
                    "{at}, --max-hours {max_hours} --home-cap-mbps {cap}"
                );
            }
            for cost in plan_field("worst_cases").as_list().expect("worst cases") {
                let cost = cost.as_list().expect("a worst case");
                let (max_hours, cap) = (float(&cost[2]), int(&cost[3]));
                assert_eq!(
                    worst_case(&kept, &cost[0], &cost[1], max_hours, cap),
                    cost[4],
                    "{at}, {:?} and {:?} at {max_hours} h, --home-cap-mbps {cap}",
                    cost[0],
                    cost[1]
                );
            }
        }
    }
}

fn float(value: &Json) -> f64 {
    num::Num::from_json(value).expect("a number").to_f64()
}

fn int(value: &Json) -> i64 {
    match value {
        Json::Int(i) => i64::try_from(*i).expect("an i64"),
        other => panic!("not an int: {other:?}"),
    }
}

#[test]
fn the_marks_hold_what_they_say() {
    // A case the port refuses where the Python reads it is marked so; an
    // unmarked refusal agrees only with a refusal.
    let refused = serde_json::json!({"ok": {"dict": [["refused", "x"]]}});
    let read = serde_json::json!({"ok": {"dict": [["runs", []]]}});
    let port_refused = Outcome::Value(dict([("refused", Json::Str("y".into()))]));
    assert!(!agrees(&refused, &port_refused, None));
    assert!(agrees(&refused, &port_refused, Some("words")));
    assert!(agrees(&read, &port_refused, Some("stricter")));
    assert!(!agrees(&refused, &port_refused, Some("stricter")));
}
