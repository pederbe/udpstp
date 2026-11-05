//! The AWS provider and the fake CLI against the Python's, call for call
//!: `tests/golden/aws.json`
//! holds what `scripts/bench/tests/export_goldens.py` saw the Python's
//! provider do on the Python's fake CLI in each scenario (the tests' of
//! `test_round.py` and `test_scale.py`, and a few more): each action's
//! result or error, each call's argv, the fake's answer and the state it
//! left, the waits, and the state at the end.
//!
//! - The fake: every call, replayed against [`fake::call`] from the state
//!   before it, must be answered and leave the state as the Python's fake
//!   did, byte for byte.
//! - The provider: [`AwsProvider`], on the fake in-process with the same
//!   clock, must make the same calls in the same order (the final state
//!   holds them all), return or refuse the same, and wait as long.
//!
//! The goldens are read with serde_json, not the parser under test.

mod common;

use std::path::Path;

use bench_rounds::aws::{AwsError, AwsProvider, CallError, Launched, fake};
use bench_rounds::num::Num;
use bench_rounds::py::{self, Json};
use common::untag;
use indexmap::IndexMap;
use serde_json::Value;

fn scenarios() -> Vec<Value> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/aws.json");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
    let mut golden: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path:?}: {e}"));
    match golden["scenarios"].take() {
        Value::Array(scenarios) if !scenarios.is_empty() => scenarios,
        _ => panic!("aws.json has no scenarios"),
    }
}

fn string(value: &Value) -> &str {
    value
        .as_str()
        .unwrap_or_else(|| panic!("not a string: {value}"))
}

/// A state's text with its calls replaced by how many there are, as the
/// golden writes each call's state.
fn counted(state: &Json) -> String {
    let mut state = state.clone();
    if let Json::Dict(top) = &mut state
        && let Some(calls) = top.get_mut("calls")
    {
        let n = calls.as_list().map_or(0, <[Json]>::len);
        *calls = Json::Int(n as i128);
    }
    py::dumps(&state)
}

#[test]
fn the_fake_answers_every_call_as_the_pythons_fake() {
    for scenario in scenarios() {
        let name = string(&scenario["name"]);
        let mut state = Json::parse(string(&scenario["state"])).expect("a state");
        let mut seen = Vec::new();
        for (i, call) in scenario["calls"]
            .as_array()
            .expect("calls")
            .iter()
            .enumerate()
        {
            let at = format!("{name}, call {i}");
            if let Some(before) = call["before"].as_str() {
                // A test changed the state: its calls are those so far.
                state = Json::parse(before).expect("a state");
                if let Json::Dict(top) = &mut state
                    && let Some(calls) = top.get_mut("calls")
                {
                    assert_eq!(calls, &Json::Int(seen.len() as i128), "{at}");
                    *calls = Json::List(seen.clone());
                }
            }
            let argv: Vec<String> = call["argv"]
                .as_array()
                .expect("argv")
                .iter()
                .map(|a| string(a).to_string())
                .collect();
            let answer = fake::call(&mut state, &argv).unwrap_or_else(|e| panic!("{at}: {e}"));
            assert_eq!(answer.stdout, string(&call["stdout"]), "{at}: stdout");
            assert_eq!(answer.stderr, string(&call["stderr"]), "{at}: stderr");
            assert_eq!(
                i64::from(answer.status),
                call["status"].as_i64().expect("a status"),
                "{at}: status"
            );
            seen.push(Json::List(
                argv.iter().map(|a| Json::Str(a.clone())).collect(),
            ));
            assert_eq!(
                counted(&state),
                string(&call["after"]),
                "{at}: the state after"
            );
        }
        assert_eq!(
            py::dumps(&state),
            string(&scenario["final"]),
            "{name}: the final state"
        );
    }
}

/// The Python's `Host` of a launched host, as the golden has it: `[ssh,
/// address, public_address]`.
fn host(launched: &Launched) -> Json {
    let or_null = |a: &Option<String>| a.clone().map_or(Json::Null, Json::Str);
    Json::List(vec![
        Json::Str(format!(
            "ubuntu@{}",
            launched.public_address.as_deref().unwrap_or("None")
        )),
        or_null(&launched.private_address),
        or_null(&launched.public_address),
    ])
}

fn number(n: Num) -> Json {
    match n {
        Num::Int(i) => Json::Int(i),
        Num::Float(f) => Json::Float(f),
    }
}

fn pairs(left: Vec<(String, String)>) -> Json {
    Json::List(
        left.into_iter()
            .map(|(kind, id)| Json::List(vec![Json::Str(kind), Json::Str(id)]))
            .collect(),
    )
}

type Provider = AwsProvider<fake::Local>;

/// One of the scenario's actions, as the exporter's `perform` does it.
fn perform(
    provider: &mut Provider,
    action: &[Json],
    make: &dyn Fn(fake::Local) -> Provider,
) -> Result<Json, CallError> {
    let text = |i: usize| action[i].as_str().expect("a string").to_string();
    let float = |i: usize| Num::from_json(&action[i]).expect("a number").to_f64();
    match action[0].as_str().expect("an action") {
        "check" => provider.check(float(1)),
        "check_permissions" => provider.check_permissions(&text(1)).map(|verdicts| {
            Json::Dict(
                verdicts
                    .into_iter()
                    .map(|(action, verdict)| (action, Json::Str(verdict)))
                    .collect(),
            )
        }),
        "price" => provider
            .price_per_hour(&text(1))
            .map(number)
            .map_err(CallError::Aws),
        "create" => {
            let roles: Vec<String> = action[3]
                .as_list()
                .expect("roles")
                .iter()
                .map(|r| r.as_str().expect("a role").to_string())
                .collect();
            provider
                .create(&text(1), float(2), &roles, &text(4))
                .map(|hosts| {
                    Json::Dict(
                        hosts
                            .iter()
                            .map(|(role, launched)| (role.clone(), host(launched)))
                            .collect(),
                    )
                })
        }
        "destroy" => provider.destroy(&text(1)).map(pairs),
        "leftovers" => provider.leftovers(&text(1)).map(pairs),
        "reap" => provider
            .reap(float(1))
            .map(|reaped| Json::List(reaped.into_iter().map(Json::Str).collect())),
        "manual_cleanup" => Ok(Json::List(
            provider
                .manual_cleanup(&text(1))
                .into_iter()
                .map(Json::Str)
                .collect(),
        )),
        "configure" => {
            // `state.setdefault("config", {}).update(changes)`.
            let Json::Dict(top) = &mut provider.calls.state else {
                panic!("the state isn't a dict");
            };
            let config = top
                .entry("config".to_string())
                .or_insert_with(|| Json::Dict(IndexMap::new()));
            let Json::Dict(config) = config else {
                panic!("the config isn't a dict");
            };
            for (key, value) in action[1].as_dict().expect("changes") {
                config.insert(key.clone(), value.clone());
            }
            Ok(Json::Null)
        }
        "fresh" => {
            *provider = make(provider.calls.clone());
            Ok(Json::Null)
        }
        other => panic!("no action {other}"),
    }
}

#[test]
fn the_provider_makes_the_pythons_calls_and_decides_as_it_does() {
    for scenario in scenarios() {
        let name = string(&scenario["name"]);
        let now = Num::from_json(&untag(&scenario["now"]))
            .expect("a time")
            .to_f64();
        let host_types: IndexMap<String, String> = scenario["host_types"]
            .as_object()
            .expect("host types")
            .iter()
            .map(|(role, t)| (role.clone(), string(t).to_string()))
            .collect();
        let prices: IndexMap<String, Num> = untag(&scenario["prices"])
            .as_dict()
            .expect("prices")
            .iter()
            .map(|(t, p)| (t.clone(), Num::from_json(p).expect("a price")))
            .collect();
        let home_cidr = string(&scenario["home_cidr"]).to_string();
        let home_udp = scenario["home_udp"].as_bool().expect("home_udp");
        let make = |calls: fake::Local| {
            AwsProvider::new(
                calls,
                "eu-north-1",
                "eu-north-1a",
                host_types.clone(),
                prices.clone(),
                &home_cidr,
                home_udp,
            )
        };
        let state = Json::parse(string(&scenario["state"])).expect("a state");
        let mut provider = make(fake::Local::new(state, "eu-north-1", now));
        let actions = untag(&scenario["actions"]);
        let results = scenario["results"].as_array().expect("results");
        for (i, (action, expected)) in actions
            .as_list()
            .expect("actions")
            .iter()
            .zip(results)
            .enumerate()
        {
            let action = action.as_list().expect("an action");
            let got = perform(&mut provider, action, &make);
            let at = format!("{name}, action {i} {:?}", action[0]);
            match (expected.get("ok"), got) {
                (Some(value), Ok(json)) => assert_eq!(json, untag(value), "{at}"),
                (None, Err(CallError::Aws(AwsError(message)))) => {
                    assert_eq!(string(&expected["raises"]), "AwsError", "{at}");
                    assert_eq!(message, string(&expected["message"]), "{at}");
                }
                (_, got) => panic!("{at}: the Python {expected}, the port {got:?}"),
            }
        }
        let slept: Vec<u64> = scenario["slept"]
            .as_array()
            .expect("waits")
            .iter()
            .map(|s| s.as_u64().expect("seconds"))
            .collect();
        assert_eq!(provider.calls.slept, slept, "{name}: the waits");
        assert_eq!(
            py::dumps(&provider.calls.state),
            string(&scenario["final"]),
            "{name}: the calls and the final state"
        );
    }
}
