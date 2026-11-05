//! The pre-registered judgments, checked against calls from the Python suite.
mod common;

#[test]
fn judgments_match_the_reference() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/scale-judge.json");
    let golden: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let pool = common::scale_judge::pool(&golden["values"]);
    common::for_each_case(golden["cases"].as_array().unwrap(), |i, case| {
        let name = case["fn"].as_str().unwrap();
        let args: Vec<_> = case["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| pool[v.as_u64().unwrap() as usize].clone())
            .collect();
        let result = common::scale_judge::call(name, &args);
        if case["python"].get("raises").is_some() {
            assert!(result.is_err(), "case {i}: {name}");
        } else {
            assert_eq!(
                result.unwrap_or_else(|e| panic!("case {i}: {name}: {e}")),
                pool[case["python"]["ok"].as_u64().unwrap() as usize],
                "case {i}: {name}"
            );
        }
    });
}
