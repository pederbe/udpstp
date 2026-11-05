//! Whole synthetic Round reports, byte for byte the Python's.
mod common;
use bench_rounds::py::Json;
use std::collections::BTreeMap;
use udpstp_bench::scale::report::{RoundFiles, report};

fn files(tree: &Json, name: &str) -> RoundFiles {
    let mut files: BTreeMap<_, _> = tree
        .get("files")
        .unwrap()
        .as_dict()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap().as_bytes().to_vec()))
        .collect();
    for (key, value) in tree.get("bytes").unwrap().as_dict().unwrap() {
        let hex = value.as_str().unwrap();
        files.insert(
            key.clone(),
            (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect(),
        );
    }
    for directory in tree.get("dirs").unwrap().as_list().unwrap_or_default() {
        let directory = directory.as_str().unwrap();
        if directory.ends_with("/meta.json") {
            files.insert(directory.into(), Vec::new());
        }
    }
    RoundFiles::from_files(name, files)
}

#[test]
#[ignore = "Nightly: complete report corpus"]
fn reports_match_the_reference_bytes() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/golden/scale-report.json"
    );
    let golden: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let pool = common::scale_judge::pool(&golden["values"]);
    common::for_each_case(golden["cases"].as_array().unwrap(), |i, case| {
        let tree = &pool[case["args"][0].as_u64().unwrap() as usize];
        let out = files(
            &pool[case["args"][0].as_u64().unwrap() as usize],
            "scale-round",
        );
        let first = &pool[case["args"][1].as_u64().unwrap() as usize];
        let first = (!first.is_null()).then(|| files(first, "first-round"));
        let result = report(&out, first.as_ref());
        if i == 0
            || !tree.get("dirs").unwrap().is_null()
            || tree
                .item("files")
                .unwrap()
                .as_dict()
                .unwrap()
                .keys()
                .any(|p| p.starts_with("scale/."))
        {
            let mut tree = tree.as_dict().unwrap().clone();
            tree.insert("name".into(), Json::Str("scale-round".into()));
            let disk = common::Tree::write(&Json::Dict(tree));
            let loaded = RoundFiles::load(&disk.out).unwrap();
            assert_eq!(report(&loaded, first.as_ref()), result, "disk case {i}");
        }
        if case["python"].get("raises").is_some() {
            assert!(result.is_err(), "case {i}");
        } else if result.is_ok() || case["port"].as_str() != Some("may-refuse") {
            let actual = result.unwrap_or_else(|e| panic!("case {i}: {e}"));
            let expected = pool[case["python"]["ok"].as_u64().unwrap() as usize]
                .as_str()
                .unwrap();
            if actual != expected {
                let at = actual
                    .bytes()
                    .zip(expected.bytes())
                    .position(|(a, b)| a != b)
                    .unwrap_or(actual.len().min(expected.len()));
                panic!(
                    "case {i}, byte {at}: Rust {:?}; Python {:?}",
                    actual
                        .get(at..)
                        .unwrap_or_default()
                        .chars()
                        .take(220)
                        .collect::<String>(),
                    expected
                        .get(at..)
                        .unwrap_or_default()
                        .chars()
                        .take(220)
                        .collect::<String>()
                );
            }
        }
    });
}
