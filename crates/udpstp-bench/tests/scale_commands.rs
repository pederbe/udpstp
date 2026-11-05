//! Command text must stay byte-identical to scale.py (design §7.1).
mod common;

use bench_rounds::num::Num;
use bench_rounds::py::Json;
use udpstp_bench::scale::{commands, matrix::ScaleRun};

fn text(v: &Json) -> &str {
    v.as_str().unwrap()
}
fn int(v: &Json) -> i64 {
    match v {
        Json::Int(i) => (*i).try_into().unwrap(),
        _ => panic!("integer"),
    }
}
fn ints(v: &Json) -> Vec<i64> {
    v.as_list().unwrap().iter().map(int).collect()
}
fn env(v: &Json) -> std::collections::BTreeMap<String, String> {
    v.as_dict()
        .map(|d| {
            d.iter()
                .map(|(k, v)| (k.clone(), text(v).to_string()))
                .collect()
        })
        .unwrap_or_default()
}
fn run(v: &Json) -> ScaleRun {
    let d = v.as_dict().unwrap();
    ScaleRun {
        index: int(&d["index"]),
        repeat: int(&d["repeat"]),
        case: text(&d["case"]).into(),
        direction: text(&d["direction"]).into(),
        row: int(&d["row"]),
        count: int(&d["count"]),
        engine: text(&d["engine"]).into(),
        test_interval: int(&d["test_interval"]),
        warm_up: int(&d["warm_up"]),
        kind: text(&d["kind"]).into(),
        rerun: int(&d["rerun"]),
        more: int(&d["more"]),
        reserve_s: Num::from_json(&d["reserve_s"]).unwrap().to_f64(),
        cpus: text(&d["cpus"]).into(),
        stagger: Num::from_json(&d["stagger"]).unwrap(),
    }
}

#[test]
fn commands_match_the_python_byte_for_byte() {
    let golden: serde_json::Value =
        serde_json::from_str(include_str!("golden/scale-commands.json")).unwrap();
    for (i, case) in golden["cases"].as_array().unwrap().iter().enumerate() {
        let a: Vec<Json> = case["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(common::untag)
            .collect();
        let actual = match case["fn"].as_str().unwrap() {
            "scale.serve" => commands::serve(
                text(&a[0]),
                int(&a[1]),
                &ints(&a[2]),
                int(&a[3]),
                &env(&a[4]),
            ),
            "scale.background" => commands::background(text(&a[0]), text(&a[1]), text(&a[2])),
            "scale.sampled" => commands::sampled(text(&a[0]), text(&a[1])),
            "scale.clients" => commands::clients(
                &run(&a[0]),
                &commands::Clients {
                    directory: text(&a[1]),
                    servers: text(&a[2]),
                    first: int(&a[3]),
                    count: int(&a[4]),
                    cpus: &ints(&a[5]),
                    stamps: a[6].truthy(),
                    options: text(&a[7]),
                    environment: &env(&a[8]),
                },
            ),
            "scale.obudpst_server" => commands::obudpst_server(
                text(&a[0]),
                &ints(&a[1]),
                &ints(&a[2]),
                if a[3] == Json::Null {
                    None
                } else {
                    Some(int(&a[3]))
                },
            ),
            "scale.prototype" => commands::prototype(
                &run(&a[0]),
                &commands::Prototype {
                    side: text(&a[1]),
                    directory: text(&a[2]),
                    start_wall: Num::from_json(&a[3]).unwrap(),
                    p_address: text(&a[4]),
                    q_addresses: &a[5]
                        .as_list()
                        .unwrap()
                        .iter()
                        .map(|v| text(v).to_string())
                        .collect::<Vec<_>>(),
                    q_index: int(&a[6]),
                    cpus: &ints(&a[7]),
                    per_core: &ints(&a[8]),
                    stamps: a[9].truthy(),
                },
            )
            .unwrap(),
            "scale.prototype_p" => commands::prototype_p(text(&a[0]), text(&a[1])),
            "scale.wait_listening" => commands::wait_listening(text(&a[0])),
            "scale.stop" => commands::stop(text(&a[0]), int(&a[1]), int(&a[2])),
            "scale.alive" => commands::alive(text(&a[0])),
            name => panic!("{name}"),
        };
        assert_eq!(
            Json::Str(actual),
            common::untag(&case["python"]["ok"]),
            "case {i}: {}",
            case["fn"]
        );
    }
}
