//! Server-scale planning against the Python reference (design §7.3).

mod common;

use udpstp_bench::scale::matrix::{row_datagrams_per_s, row_mbps, row_packet_bytes};

#[test]
fn rows_keep_the_pre_registered_rate_and_packet_shape() {
    // scale.py's row helpers and test_scale.py's Matrix fixture.
    for (row, mbps, datagrams, bytes) in [
        (1, 1, 1_000, 125.0),
        (4, 4, 1_000, 500.0),
        (9, 9, 1_000, 1125.0),
        (15, 15, 2_000, 937.5),
        (50, 50, 5_000, 1250.0),
        (1000, 1000, 100_000, 1250.0),
        (1001, 1100, 100_000, 1375.0),
        (1002, 1200, 100_000, 1500.0),
    ] {
        assert_eq!(row_mbps(row), mbps);
        assert_eq!(row_datagrams_per_s(row), datagrams);
        assert_eq!(row_packet_bytes(row).to_f64(), bytes);
    }
}

#[test]
fn committed_matrices_and_refusals_match_the_python() {
    use bench_rounds::py::Json;
    use udpstp_bench::scale::matrix;
    let golden: serde_json::Value =
        serde_json::from_str(include_str!("golden/scale-matrix.json")).unwrap();
    for (index, case) in golden["cases"].as_array().unwrap().iter().enumerate() {
        let input = common::untag(&case["data"]);
        let expected = common::untag(&case["python"]);
        let actual = match matrix::expand(&input) {
            Ok(matrix) => common::scale::snapshot(&matrix),
            Err(e) => common::dict([("error", Json::Str(e.to_string()))]),
        };
        assert_eq!(actual, expected, "case {index}: {}", case["name"]);
        if let Some(path) = case["path"].as_str() {
            let saved = matrix::parse(case["text"].as_str().unwrap()).unwrap();
            assert_eq!(
                common::scale::snapshot(&saved),
                expected,
                "saved TOML case {index}"
            );
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(path);
            let text = std::fs::read_to_string(path).unwrap().replace("\r\n", "\n");
            let parsed = matrix::parse(&text).unwrap();
            if matrix::check_execution(&saved).is_err() {
                // ADR 0016: saved grouping plans stay readable, while
                // their maintained replacements no longer request it.
                matrix::check_execution(&parsed).unwrap();
            } else {
                assert_eq!(
                    text,
                    case["text"].as_str().unwrap(),
                    "regenerate the Python golden"
                );
                assert_eq!(
                    common::scale::snapshot(&parsed),
                    expected,
                    "TOML case {index}"
                );
            }
        }
    }
}

#[test]
fn unknown_keys_and_non_integral_counts_are_refused_before_expansion() {
    // The design's accepted stricter matrices (§7.7), beyond Python's
    // coercions and ignored keys. No committed matrix uses these.
    let good = "[round]\nkind = 'server-scale'\nengines = ['udpstp']\n[[case]]\nname = 'C'\ndirection = 'downstream'\nrow = 50\ncounts = [1]\ntarget = 1\n";
    assert!(udpstp_bench::scale::matrix::parse(good).is_ok());
    for bad in [
        good.replace("[round]", "unexpected = 1\n[round]"),
        good.replace("engines =", "unrecognised = 1\nengines ="),
        format!("{good}unknown = 1\n"),
        good.replace("counts = [1]", "counts = [1.5]"),
        good.replace("target = 1", "target = '1'"),
        format!("{good}[p1]\nwrong = 1\n"),
    ] {
        assert!(udpstp_bench::scale::matrix::parse(&bad).is_err(), "{bad}");
    }
}

#[test]
fn case_names_use_the_references_unicode_character_property() {
    // Reference-generated range boundaries also exercise the lookup gaps.
    let data: serde_json::Value =
        serde_json::from_str(include_str!("golden/scale-names.json")).unwrap();
    for case in data["cases"].as_array().unwrap() {
        let point = case[0].as_u64().unwrap() as u32;
        let character = char::from_u32(point).unwrap();
        // JSON string escaping is also valid TOML basic string syntax
        // for these boundary points; no lone surrogate is generated.
        let name = serde_json::to_string(&format!("A{character}")).unwrap();
        let matrix = format!(
            "[round]\nkind = 'server-scale'\nengines = ['udpstp']\n[[case]]\nname = {name}\ndirection = 'downstream'\nrow = 1\ncounts = [1]\ntarget = 1\n"
        );
        let accepted = udpstp_bench::scale::matrix::parse(&matrix).is_ok();
        assert_eq!(
            accepted,
            case[1].as_bool().unwrap() || character == '_',
            "U+{point:04X}"
        );
    }
}
