//! Measurement migration , through the complete sampler record interface.
//! Expected records come from the existing Python on the same saved proc trees.

use bench_rounds::{py, sampler, secrets::SecretDir};
use serde_json::Value;
use std::path::Path;

fn tree(root: &Path, input: &Value) {
    for (path, text) in input["files"].as_object().unwrap() {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text.as_str().unwrap()).unwrap();
    }
}

#[test]
fn saved_proc_trees_produce_the_independent_reference_records() {
    let cases: Value = serde_json::from_str(include_str!("golden/sampler-recorded.json")).unwrap();
    for input in cases.as_array().unwrap() {
        let root = SecretDir::create().unwrap();
        tree(root.path(), input);
        let pids = input["pids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|pid| pid.as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        let actual = sampler::snapshot(
            root.path(),
            &pids,
            input["client_ns"].as_str(),
            input["t"].as_f64().unwrap(),
        )
        .unwrap();
        let actual = actual
            .map(|record| py::dumps(&record) + "\n")
            .unwrap_or_default();
        assert_eq!(actual, input["python"].as_str().unwrap());
    }
}
