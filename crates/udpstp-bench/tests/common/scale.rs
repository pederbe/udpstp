pub fn snapshot(matrix: &udpstp_bench::scale::matrix::Matrix) -> bench_rounds::py::Json {
    use bench_rounds::py::Json;
    use udpstp_bench::scale::matrix::{duration_s, row_mbps};
    let runs: Vec<Json> = matrix
        .runs
        .iter()
        .map(|r| {
            let Json::Dict(mut fields) = r.metadata() else {
                panic!("metadata is an object")
            };
            fields.insert("label".into(), Json::Str(r.label()));
            fields.insert(
                "point".into(),
                Json::List(vec![
                    Json::Str(r.kind.clone()),
                    Json::Str(r.case.clone()),
                    Json::Str(r.direction.clone()),
                    Json::Int(r.row.into()),
                    Json::Int(r.count.into()),
                    Json::Str(r.engine.clone()),
                ]),
            );
            fields.insert("mbps".into(), Json::Int(row_mbps(r.row).into()));
            fields.insert("seconds".into(), Json::Float(r.seconds()));
            fields.insert("download_gb".into(), Json::Float(r.download_gb()));
            fields.insert("shape".into(), Json::Str(format!("row{}", r.row)));
            Json::Dict(fields)
        })
        .collect();
    let fields = runs
        .first()
        .and_then(Json::as_dict)
        .map(|row| row.keys().map(|s| Json::Str(s.clone())).collect())
        .unwrap_or_default();
    let rows = runs
        .into_iter()
        .map(|row| {
            let Json::Dict(row) = row else { unreachable!() };
            Json::List(row.into_values().collect())
        })
        .collect();
    let table = super::dict([("fields", Json::List(fields)), ("rows", Json::List(rows))]);
    super::dict([
        ("settings", matrix.settings.clone()),
        ("plan", matrix.plan.clone()),
        ("runs", table),
        ("duration_s", Json::Float(duration_s(&matrix.runs))),
    ])
}
