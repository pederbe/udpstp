//! Each synthetic Round Run left by the Python suite, analysed without I/O.
mod common;

use std::collections::BTreeMap;
use udpstp_bench::scale::judge::{RunFiles, analyse};

#[test]
#[ignore = "Nightly: complete evidence analysis corpus"]
fn recorded_files_match_the_reference_analysis() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/golden/scale-analysis.json"
    );
    let golden: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let pool = common::scale_judge::pool(&golden["values"]);
    common::for_each_case(golden["cases"].as_array().unwrap(), |i, case| {
        let tree = &pool[case["args"][0].as_u64().unwrap() as usize];
        let mut files: BTreeMap<String, Vec<u8>> = tree
            .get("files")
            .unwrap()
            .as_dict()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap().as_bytes().to_vec()))
            .collect();
        for (key, hex) in tree.get("bytes").unwrap().as_dict().unwrap() {
            let hex = hex.as_str().unwrap();
            let bytes = (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect();
            files.insert(key.clone(), bytes);
        }
        let result = analyse(&RunFiles { files });
        if case["python"].get("raises").is_some() {
            assert!(result.is_err(), "case {i}");
        } else if result.is_ok() || case["port"].as_str() != Some("may-refuse") {
            assert_eq!(
                result.unwrap_or_else(|e| panic!("case {i}: {e}")),
                pool[case["python"]["ok"].as_u64().unwrap() as usize],
                "case {i}"
            );
        }
    });
}
