//! The original run loop and the pure schedule consume the same records
//! and clock advances: compare every Run, log line and confinement choice.
mod common;

use bench_rounds::{num::Num, py::Json};
use udpstp_bench::scale::{
    matrix,
    schedule::{Schedule, Step},
};

#[test]
fn the_schedule_matches_the_reference_loop() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/golden/scale-schedule.json"
    );
    let golden: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let pool = common::scale_judge::pool(&golden["values"]);
    for (i, case) in golden["cases"].as_array().unwrap().iter().enumerate() {
        let a: Vec<_> = case["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| &pool[v.as_u64().unwrap() as usize])
            .collect();
        let matrix = matrix::expand(a[0]).unwrap();
        let records = a[1].as_list().unwrap();
        let seconds = a[2].as_list().unwrap();
        let mut now = Num::from_json(a[3]).unwrap().to_f64();
        let deadline = Num::from_json(a[4]).unwrap().to_f64();
        let mut schedule = Schedule::new(&matrix).unwrap();
        let mut taken = Vec::new();
        let mut logs = Vec::new();
        let mut confinement = Json::Null;
        loop {
            let next = schedule.next(now, deadline).unwrap();
            logs.extend(next.logs);
            match next.step {
                Step::Run(run) => {
                    let at = taken.len();
                    let record = records
                        .get(at)
                        .unwrap_or_else(|| panic!("case {i}: unexpected {}", run.label()));
                    assert_eq!(
                        record.item("label").unwrap().as_str(),
                        Some(run.label().as_str()),
                        "case {i}"
                    );
                    now += Num::from_json(&seconds[at]).unwrap().to_f64();
                    taken.push(run.metadata());
                    logs.extend(schedule.record(record.clone()).unwrap());
                }
                Step::SaveConfinement(value) => confinement = value,
                Step::OutOfTime | Step::Stopped(_) | Step::Done => break,
            }
        }
        let actual = common::dict([
            ("taken", Json::List(taken)),
            (
                "logs",
                Json::List(logs.into_iter().map(Json::Str).collect()),
            ),
            ("confined", schedule.confined().clone()),
            ("confinement", confinement),
        ]);
        assert_eq!(
            actual,
            pool[case["python"]["ok"].as_u64().unwrap() as usize],
            "case {i}"
        );
    }
}
