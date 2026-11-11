//! The pre-registered judgments, checked against calls from the Python suite.
mod common;

// ADR 0016: current startup records omit wake-up grouping. Historical
// explicit windows must still match the evidence rather than default to zero.
#[test]
fn current_startups_and_historical_grouping_requests_are_distinguished() {
    use bench_rounds::py::Json;
    use udpstp_bench::scale::judge::{engine_startups, startup_problems};

    let record = |event: &str| {
        let event = Json::parse(event).unwrap();
        Json::Dict(indexmap::indexmap! {
            "engine_startups".into() => engine_startups(&[event]).unwrap()
        })
    };
    let current = record(r#"{"event":"DataPhaseEngines","executor":"pool","engine_threads":2}"#);
    let normal =
        Json::parse(r#"{"server_env":{"UDPSTP_ENGINE_THREADS":"2"},"server_cpus":[0,1]}"#).unwrap();
    assert!(startup_problems(&normal, &current).unwrap().is_empty());
    let grouped = Json::parse(r#"{"server_env":{"UDPSTP_ENGINE_THREADS":"2","UDPSTP_ENGINE_GROUPING_US":"50"},"server_cpus":[0,1]}"#).unwrap();
    assert!(!startup_problems(&grouped, &current).unwrap().is_empty());
    let historical = record(
        r#"{"event":"DataPhaseEngines","executor":"pool","engine_threads":2,"grouping_us":50}"#,
    );
    assert!(startup_problems(&grouped, &historical).unwrap().is_empty());
}

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
