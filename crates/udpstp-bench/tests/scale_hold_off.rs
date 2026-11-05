//! Each captured A/B window and its interrupt evidence, compared to the
//! reference figures before their medians or report formatting can hide it.
mod common;

use udpstp_bench::scale::judge::{RunFiles, hold_off_run};

#[test]
fn hold_off_windows_match_the_reference() {
    let golden: serde_json::Value = serde_json::from_slice(
        &std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/golden/scale-hold-off.json"
        ))
        .unwrap(),
    )
    .unwrap();
    let pool = common::scale_judge::pool(&golden["values"]);
    for (i, case) in golden["cases"].as_array().unwrap().iter().enumerate() {
        let a = case["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| &pool[v.as_u64().unwrap() as usize])
            .collect::<Vec<_>>();
        let run = RunFiles {
            files: a[0]
                .item("files")
                .unwrap()
                .as_dict()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap().as_bytes().to_vec()))
                .collect(),
        };
        let irqs = a[2]
            .as_list()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        let result = hold_off_run(&run, a[1], &irqs);
        if case["python"].get("raises").is_some() {
            assert!(result.is_err(), "case {i}");
        } else {
            assert_eq!(
                result.unwrap_or_else(|e| panic!("case {i}: {e}")),
                pool[case["python"]["ok"].as_u64().unwrap() as usize],
                "case {i}"
            );
        }
    }
}
