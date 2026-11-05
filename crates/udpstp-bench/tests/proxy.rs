//! The loopback proxy's registered reading, compared with its reference.
mod common;

use bench_rounds::{num::Num, py::Json};
use udpstp_bench::proxy;

fn lines(lines: Vec<String>) -> Json {
    Json::List(lines.into_iter().map(Json::Str).collect())
}

#[test]
fn proxy_readings_match_the_reference() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("golden/proxy.json")).unwrap();
    let pool = common::scale_judge::pool(&fixture["values"]);
    for (i, case) in fixture["cases"].as_array().unwrap().iter().enumerate() {
        let a: Vec<_> = case["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| &pool[v.as_u64().unwrap() as usize])
            .collect();
        let numbers = |v: &Json| {
            v.as_list()
                .unwrap()
                .iter()
                .map(|v| Num::from_json(v).unwrap())
                .collect::<Vec<_>>()
        };
        let result = match case["fn"].as_str().unwrap() {
            "proxy.parse" => proxy::parse(
                &a[0]
                    .as_list()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap())
                    .collect::<Vec<_>>(),
            )
            .map(Json::List),
            "proxy.worse_by" => proxy::worse_by(
                &numbers(a[0]),
                &numbers(a[1]),
                Num::from_json(a[2]).unwrap(),
            )
            .map(|(bad, p)| Json::List(vec![Json::Bool(bad), Json::Float(p)])),
            "proxy.thread_reading" => {
                proxy::thread_reading(a[0].as_list().unwrap(), a[1].as_list().unwrap(), a[2])
                    .map(|(ok, why)| Json::List(vec![Json::Bool(ok), Json::Str(why)]))
            }
            "proxy.proxy_reading" => proxy::reading(a[0].as_list().unwrap())
                .map(|(ok, text)| Json::List(vec![Json::Bool(ok), lines(text)])),
            "proxy.feasibility" => {
                proxy::feasibility(a[0].as_list().unwrap()).map(|(chosen, text)| {
                    Json::List(vec![
                        chosen.map(|s| Json::List(s.into())).unwrap_or(Json::Null),
                        lines(text),
                    ])
                })
            }
            name => panic!("{name}"),
        };
        if case["python"].get("raises").is_some() {
            assert!(result.is_err(), "case {i}: Python refuses, Rust {result:?}");
        } else if !(result.is_err() && case["port"] == "may-refuse") {
            assert_eq!(
                result.unwrap_or_else(|e| panic!("case {i}: {e}")),
                pool[case["python"]["ok"].as_u64().unwrap() as usize],
                "case {i}"
            );
        }
    }
}
