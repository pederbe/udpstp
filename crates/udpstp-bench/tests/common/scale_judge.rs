use bench_rounds::py::Json;
use udpstp_bench::scale::{Error, judge};

pub fn pool(values: &serde_json::Value) -> Vec<Json> {
    let mut pool: Vec<Json> = Vec::new();
    for value in values.as_array().unwrap() {
        let read = |v: &serde_json::Value| pool[v.as_u64().unwrap() as usize].clone();
        let value = if let Some(items) = value.get("list") {
            Json::List(items.as_array().unwrap().iter().map(read).collect())
        } else if let Some(items) = value.get("dict") {
            Json::Dict(
                items
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|entry| (entry[0].as_str().unwrap().into(), read(&entry[1])))
                    .collect(),
            )
        } else {
            super::untag(value)
        };
        pool.push(value);
    }
    pool
}

fn key(value: &Json) -> judge::PointKey {
    let a = value.as_list().unwrap();
    (
        s(&a[0]).into(),
        s(&a[1]).into(),
        s(&a[2]).into(),
        i(&a[3]),
        i(&a[4]),
        s(&a[5]).into(),
    )
}
fn s(value: &Json) -> &str {
    value.as_str().unwrap()
}
fn i(value: &Json) -> i64 {
    match value {
        Json::Int(n) => (*n).try_into().unwrap(),
        _ => panic!("int"),
    }
}
fn list(value: &Json) -> &[Json] {
    value.as_list().unwrap()
}
fn refs(value: &Json) -> Vec<&Json> {
    list(value).iter().collect()
}
fn strings(values: Vec<String>) -> Json {
    Json::List(values.into_iter().map(Json::Str).collect())
}
fn optional(value: Option<bench_rounds::num::Num>) -> Json {
    use bench_rounds::num::Num;
    match value {
        Some(Num::Int(n)) => Json::Int(n),
        Some(Num::Float(n)) => Json::Float(n),
        None => Json::Null,
    }
}
fn verdicts(value: &Json) -> judge::Verdicts {
    list(value)
        .iter()
        .map(|v| {
            let v = list(v);
            (key(&v[0]), v[1].clone())
        })
        .collect()
}
fn point_pairs(values: judge::Verdicts) -> Json {
    Json::List(
        values
            .into_iter()
            .map(|(k, v)| Json::List(vec![judge::key_json(&k), v]))
            .collect(),
    )
}

pub fn call(name: &str, a: &[Json]) -> Result<Json, Error> {
    Ok(match name.strip_prefix("scale.judge.").unwrap() {
        "gates_met" => judge::gates_met(&a[0])?,
        "lower_p" => {
            let numbers = |v: &Json| {
                list(v)
                    .iter()
                    .map(|v| {
                        bench_rounds::num::Num::from_json(v).ok_or_else(|| {
                            Error::from(bench_rounds::py::PyError::new(
                                bench_rounds::py::PyErrorKind::Refused,
                                "expected number".into(),
                            ))
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
            };
            Json::Float(judge::lower_p(&numbers(&a[0])?, &numbers(&a[1])?)?)
        }
        "hold_off_reading" => judge::hold_off_reading(&a[0], &a[1])?,
        "grouping_answer" => judge::grouping_answer(&a[0], &verdicts(&a[1]))?,
        "grouping_stands" => judge::grouping_stands(&a[0], list(&a[1]))?,
        "precision_counts" => Json::Bool(judge::precision_counts(&a[0])?),
        "precision_margin" => optional(judge::precision_margin(
            bench_rounds::num::Num::from_json(&a[1]),
            &refs(&a[2]),
        )?),
        "precision_measure" => judge::precision_measure(
            &refs(&a[0]),
            &refs(&a[1]),
            s(&a[2]),
            bench_rounds::num::Num::from_json(&a[3]),
        )?,
        "precision_row" => judge::precision_row(&refs(&a[0]), &refs(&a[1]))?,
        "precision_reading" => Json::Str(judge::precision_reading(&a[0], a[1].truthy())?.into()),
        "interrupt_counts" => judge::interrupt_counts(s(&a[0]))?,
        "server_receive_drops" => optional(Some(judge::server_receive_drops(&a[0], s(&a[1]))?)),
        "server_accounts" => optional(Some(judge::server_accounts(
            &a[0],
            &a[1],
            a[2].truthy(),
            s(&a[3]),
        )?)),
        "counted_over" => judge::counted_over(
            list(&a[0]),
            bench_rounds::num::Num::from_json(&a[1]).unwrap().to_f64(),
        )?
        .map(Json::Float)
        .unwrap_or(Json::Null),
        "rates_figures" => judge::rates_figures(
            list(&a[0]),
            bench_rounds::num::Num::from_json(&a[1]).unwrap().to_f64(),
            i(&a[2]),
            i(&a[3]),
        )?,
        "planned_datagrams" => Json::Float(judge::planned_datagrams(
            &a[0],
            bench_rounds::num::Num::from_json(&a[1]),
        )?),
        "unanswered" => judge::unanswered(&a[0])?,
        "silent_rejections" => optional(Some(judge::silent_rejections(list(&a[0]))?)),
        "engine_ends" => judge::engine_ends(list(&a[0]))?,
        "engine_startups" => judge::engine_startups(list(&a[0]))?,
        "engine_problems" => strings(judge::engine_problems(&a[0], &a[1])?),
        "startup_settings" => judge::startup_settings(&a[0])?,
        "startup_problems" => strings(judge::startup_problems(&a[0], &a[1])?),
        "only_the_path" => Json::Bool(judge::only_the_path(&a[0])?),
        "incomplete" => optional(judge::incomplete(&a[0])?),
        "path_loss_text" => judge::path_loss_text(&a[0])?,
        "meets" => Json::Bool(judge::meets(&a[0])?),
        "runs_for" => Json::Int(judge::runs_for(&a[0], &a[1], i(&a[2]), s(&a[3]))?.into()),
        "judged_like" => Json::Bool(judge::judged_like(&a[0], &a[1])?),
        "needed_runs" => Json::Int(judge::needed_runs(i(&a[0])).into()),
        "point_key" => judge::key_json(&judge::point_key(&a[0])?),
        "curve_key" => judge::key_json(&judge::curve_key(&a[0], i(&a[1]), s(&a[2]))?),
        "planned_runs" => Json::Int(judge::planned_runs(&a[0], &key(&a[1]))?.into()),
        "close_calls" => strings(judge::close_calls(&refs(&a[0]))?),
        "point_gates" => {
            let (figures, ok) = judge::point_gates(s(&a[0]), &refs(&a[1]))?;
            Json::List(vec![figures, Json::Bool(ok)])
        }
        "own_sending" => Json::Bool(judge::own_sending(&a[0])?),
        "other_flags" => Json::List(judge::other_flags(&a[0])?),
        "own_gates_fail" => Json::Bool(judge::own_gates_fail(&refs(&a[0]))?),
        "points" => point_pairs(judge::points(&a[0], list(&a[1]))?),
        "verdict" => judge::verdict(s(&a[0]), &refs(&a[1]), i(&a[2]))?,
        "confinement_verdicts" => judge::confinement_verdicts(&a[0], list(&a[1]))?,
        "confinement" => judge::confinement(&a[0], &a[1])?,
        "capacity" => judge::capacity(&verdicts(&a[0]), &a[1], s(&a[2]))?,
        "carries" => judge::carries(&verdicts(&a[0]), &a[1], s(&a[2]), i(&a[3]))?,
        "reading" => judge::reading(&verdicts(&a[0]), &a[1], i(&a[2]))?,
        "might_carry" => {
            Json::Int(judge::might_carry(&verdicts(&a[0]), &a[1], s(&a[2]), &a[3])?.into())
        }
        "floor_answer" => judge::floor_answer(&verdicts(&a[0]), &a[1])?,
        "floor_stops" => Json::Str(judge::floor_stops(&a[0], i(&a[1]), i(&a[2]))?),
        "decide" => judge::decide(s(&a[0]), &a[1], &a[2]),
        "outcomes" => {
            let (lines, summary) = judge::outcomes(&a[0], &verdicts(&a[1]))?;
            Json::List(vec![
                Json::List(lines),
                Json::List(
                    summary
                        .into_iter()
                        .map(|(k, v)| Json::List(vec![k, Json::List(v)]))
                        .collect(),
                ),
            ])
        }
        "decision_points" => Json::List(
            judge::decision_points(&a[0], &verdicts(&a[1]))?
                .iter()
                .map(judge::key_json)
                .collect(),
        ),
        "unusable" => Json::Bool(judge::unusable(&a[0])?),
        "probe_stops" => Json::Bool(judge::probe_stops(list(&a[0]), list(&a[1]), list(&a[2]))?),
        name => panic!("unimplemented {name}"),
    })
}
