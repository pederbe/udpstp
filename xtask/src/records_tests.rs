//! Tests for the conformance records check. The inputs are small in-memory
//! record files, wire structs and Rust sources.

use super::*;

const WIRE: &str = "\
pub struct SendingRate {
    pub udp_payload1: u32,
}

pub struct ActivationPdu {
    pub use_ow_del_var: u8,
    pub sending_rate: SendingRate,
}

pub struct SetupPdu {
    pub mc_count: u8,
}
";

const TESTS: &str = "\
#[test]
fn proves_it() {}

#[cfg(windows)]
#[test]
fn proves_it_on_windows() {}

#[test]
#[ignore = \"slow\"]
fn proves_it_slowly() {}

/// A helper.
fn helper() {}

#[udpstp_req::req(
    \"RFC9946-6.1-1\"
)]
#[tokio::test]
async fn proves_it_async() {}

// The rule (conformance: lpdu-time-per-burst).
";

struct Setup {
    requirements: Vec<(&'static str, bool, bool, Option<&'static str>)>,
    settings: &'static str,
}

const SETTINGS: &str = "struct Args {\n    max_start_row: Option<u16>,\n}\n";

impl std::default::Default for Setup {
    fn default() -> Self {
        Self {
            requirements: vec![("RFC9946-6.1-6", false, false, None)],
            settings: SETTINGS,
        }
    }
}

fn run_with(setup: &Setup, toml: &str) -> Checked {
    let file = RecordsFile::parse("data-phase.toml", toml).expect("valid TOML");
    let sources = [SourceFile::scan("crates/c/tests/t.rs", TESTS)];
    let wire = WireStructs::scan(&[WIRE]);
    let views: Vec<RequirementView<'_>> = setup
        .requirements
        .iter()
        .map(
            |(id, deviation, interpretation, reference)| RequirementView {
                id,
                deviation: *deviation,
                interpretation: *interpretation,
                references: reference.iter().copied().collect(),
            },
        )
        .collect();
    check(&Inputs {
        files: &[file],
        requirements: &views,
        sources: &sources,
        adrs: &["0002"],
        wire: &wire,
        settings_text: setup.settings,
    })
}

fn run(toml: &str) -> Checked {
    run_with(&Setup::default(), toml)
}

const INTERPRETATION: &str = "\
[[record]]
id = \"lpdu-time-per-burst\"
kind = \"interpretation\"
summary = \"Every datagram of a Burst carries the same lpduTime.\"
rfc = \"RFC 9946 §8.1\"
reference = [\"ADR 0002\", \"#57\"]
tests = [\"crates/c/tests/t.rs::proves_it\"]
";

fn problems(checked: &Checked) -> Vec<String> {
    checked.problems.iter().map(ToString::to_string).collect()
}

#[test]
fn a_complete_interpretation_passes_and_its_test_runs() {
    let checked = run(INTERPRETATION);
    assert!(checked.problems.is_empty(), "{:?}", problems(&checked));
    let record = &checked.records[0];
    assert_eq!(
        record.tests,
        vec![(
            "crates/c/tests/t.rs::proves_it".to_string(),
            TestStatus::Runs
        )]
    );
    assert!(record.proven());
}

#[test]
fn test_status_tells_cfg_ignored_async_helpers_and_missing_apart() {
    let toml = INTERPRETATION.replace(
        "tests = [\"crates/c/tests/t.rs::proves_it\"]",
        "tests = [\"crates/c/tests/t.rs::proves_it_on_windows\", \"crates/c/tests/t.rs::proves_it_slowly\", \"crates/c/tests/t.rs::proves_it_async\", \"crates/c/tests/t.rs::helper\", \"crates/c/tests/t.rs::gone\", \"crates/c/tests/other.rs::proves_it\"]",
    );
    let checked = run(&toml);
    let statuses: Vec<TestStatus> = checked.records[0]
        .tests
        .iter()
        .map(|(_, s)| s.clone())
        .collect();
    assert_eq!(
        statuses,
        vec![
            TestStatus::Cfg("windows".into()),
            TestStatus::Ignored,
            TestStatus::Runs,
            TestStatus::NotATest,
            TestStatus::Missing,
            TestStatus::Missing,
        ]
    );
    // A helper or a missing test fails the check; cfg and ignore don't.
    assert_eq!(checked.problems.len(), 3, "{:?}", problems(&checked));
}

#[test]
fn a_record_without_a_running_test_is_unproven_and_listed() {
    let toml = INTERPRETATION.replace(
        "tests = [\"crates/c/tests/t.rs::proves_it\"]",
        "tests = [\"crates/c/tests/t.rs::proves_it_slowly\"]",
    );
    let checked = run(&toml);
    assert!(!checked.records[0].proven());
    assert!(checked.summary().contains("Unproven: lpdu-time-per-burst."));
    let checked =
        run(&INTERPRETATION.replace("tests = [\"crates/c/tests/t.rs::proves_it\"]\n", ""));
    assert!(checked.report().contains("no proving test"));
    assert!(checked.problems.is_empty(), "{:?}", problems(&checked));
}

#[test]
fn a_deviation_needs_its_kind_default_and_revisit_trigger() {
    let toml = INTERPRETATION.replace("\"interpretation\"", "\"deviation\"");
    let found = problems(&run(&toml));
    for expected in ["needs `deviation`", "needs `default`", "needs `revisit`"] {
        assert!(
            found.iter().any(|p| p.contains(expected)),
            "{expected}: {found:?}"
        );
    }
    let toml = toml.replace(
        "kind = \"deviation\"",
        "kind = \"deviation\"\ndeviation = \"non-conformance\"\ndefault = \"off\"\nrevisit = \"Never.\"",
    );
    let checked = run(&toml);
    assert!(checked.problems.is_empty(), "{:?}", problems(&checked));
}

#[test]
fn only_a_deviation_has_a_deviation_kind_and_only_an_obudpst_difference_may_omit_rfc() {
    let toml = INTERPRETATION.replace(
        "kind = \"interpretation\"",
        "kind = \"interpretation\"\ndeviation = \"platform-gap\"",
    );
    assert!(
        problems(&run(&toml))
            .iter()
            .any(|p| p.contains("only a Deviation"))
    );
    let without_rfc = INTERPRETATION.replace("rfc = \"RFC 9946 §8.1\"\n", "");
    assert!(
        problems(&run(&without_rfc))
            .iter()
            .any(|p| p.contains("needs `rfc`"))
    );
    let difference = without_rfc.replace("\"interpretation\"", "\"obudpst-difference\"");
    assert!(
        problems(&run(&difference))
            .iter()
            .any(|p| p.contains("needs `obudpst`"))
    );
    let checked = run(&format!("{difference}obudpst = \"Differs.\"\n"));
    assert!(checked.problems.is_empty(), "{:?}", problems(&checked));
}

#[test]
fn a_setting_needs_a_default_and_must_exist() {
    let toml = format!("{INTERPRETATION}setting = \"--max-start-row\"\n");
    assert!(
        problems(&run(&toml))
            .iter()
            .any(|p| p.contains("needs `default`"))
    );
    let toml = format!("{toml}default = \"on\"\n");
    let checked = run(&toml);
    assert!(checked.problems.is_empty(), "{:?}", problems(&checked));
    let toml = toml.replace("--max-start-row", "--max-start-rows-gone");
    assert!(
        problems(&run(&toml))
            .iter()
            .any(|p| p.contains("setting --max-start-rows-gone"))
    );
}

#[test]
fn unknown_requirements_adrs_and_duplicate_ids_are_problems() {
    let toml = INTERPRETATION.replace(
        "reference = [\"ADR 0002\", \"#57\"]",
        "reference = [\"ADR 0099\"]\nrequirements = [\"RFC9946-6.1-99\"]",
    );
    let found = problems(&run(&format!("{toml}{INTERPRETATION}")));
    for expected in ["ADR 0099", "RFC9946-6.1-99", "duplicate ID"] {
        assert!(
            found.iter().any(|p| p.contains(expected)),
            "{expected}: {found:?}"
        );
    }
}

#[test]
fn a_deviation_and_its_requirement_must_point_at_each_other() {
    let deviation = INTERPRETATION
        .replace(
            "kind = \"interpretation\"",
            "kind = \"deviation\"\ndeviation = \"non-conformance\"\ndefault = \"off\"\nrevisit = \"Never.\"\nrequirements = [\"RFC9946-6.1-6\"]",
        )
        .replace("lpdu-time-per-burst", "no-rate-limit")
        + INTERPRETATION;
    // The requirement is still pending.
    let found = problems(&run(&deviation));
    assert!(
        found
            .iter()
            .any(|p| p.contains("whose state isn't deviation")),
        "{found:?}"
    );
    let marked = Setup {
        requirements: vec![("RFC9946-6.1-6", true, false, Some("no-rate-limit"))],
        ..Setup::default()
    };
    let checked = run_with(&marked, &deviation);
    assert!(checked.problems.is_empty(), "{:?}", problems(&checked));
    // A requirement pointing at a record that doesn't list it.
    let stray = Setup {
        requirements: vec![("RFC9946-6.1-6", false, true, Some("lpdu-time-per-burst"))],
        ..Setup::default()
    };
    let found = problems(&run_with(&stray, INTERPRETATION));
    assert!(
        found
            .iter()
            .any(|p| p.starts_with("RFC9946-6.1-6: reference")),
        "{found:?}"
    );
}

#[test]
fn a_record_whose_section_has_requirements_but_links_none_is_reported() {
    let toml = INTERPRETATION.replace("RFC 9946 §8.1", "RFC 9946 §6.1 and RFC 9097 §8");
    let checked = run(&toml);
    assert_eq!(
        checked.records[0].unlinked_section.as_deref(),
        Some("RFC 9946 §6.1")
    );
    assert!(
        checked
            .report()
            .contains("links no requirement of RFC 9946 §6.1")
    );
}

#[test]
fn masks_name_wire_fields_and_known_conditions_and_need_the_obudpst_behaviour() {
    let masked = format!(
        "{INTERPRETATION}masks = [{{ pdu = \"activation-response\", fields = [\"use_ow_del_var\", \"sending_rate.udp_payload1\"], when = [\"upstream\"] }}]\n"
    );
    assert!(
        problems(&run(&masked))
            .iter()
            .any(|p| p.contains("needs `obudpst`"))
    );
    let masked = format!("{masked}obudpst = \"Resets it to False.\"\n");
    let checked = run(&masked);
    assert!(checked.problems.is_empty(), "{:?}", problems(&checked));
    assert!(checked.records[0].obudpst);
    let bad = masked
        .replace(
            "\"use_ow_del_var\"",
            "\"use_ow_del_var\", \"sending_rate.nope\", \"mc_count\"",
        )
        .replace("\"upstream\"", "\"sideways\"");
    let found = problems(&run(&bad));
    for expected in [
        "field sending_rate.nope",
        "field mc_count",
        "condition sideways",
    ] {
        assert!(
            found.iter().any(|p| p.contains(expected)),
            "{expected}: {found:?}"
        );
    }
}

#[test]
fn the_mask_list_merges_records_per_field_and_condition() {
    let first = format!(
        "{INTERPRETATION}obudpst = \"x\"\nmasks = [{{ pdu = \"activation-response\", fields = [\"use_ow_del_var\"] }}, {{ pdu = \"setup-response\", fields = [\"mc_count\"], when = [\"upstream\", \"fixed-rate\"] }}]\n"
    );
    let second = first.replace("lpdu-time-per-burst", "another");
    let checked = run(&format!("{first}{second}"));
    assert!(checked.problems.is_empty(), "{:?}", problems(&checked));
    let toml = checked.masks_toml();
    assert!(toml.contains(
        "[[mask]]\npdu = \"setup-response\"\nfield = \"mc_count\"\nwhen = [\"fixed-rate\", \"upstream\"]\nrecords = [\"lpdu-time-per-burst\", \"another\"]\n"
    ), "{toml}");
    // In the order the PDUs are exchanged.
    assert!(toml.find("setup-response") < toml.find("activation-response"));
    let parsed: toml::Value = toml::from_str(&toml).expect("valid TOML");
    assert_eq!(parsed["mask"].as_array().map(Vec::len), Some(2));
}

#[test]
fn code_notes_must_name_existing_records() {
    let checked = run(INTERPRETATION);
    assert!(checked.problems.is_empty(), "{:?}", problems(&checked));
    let renamed = INTERPRETATION.replace("lpdu-time-per-burst", "renamed");
    let found = problems(&run(&renamed));
    assert!(
        found
            .iter()
            .any(|p| p.contains("`conformance: lpdu-time-per-burst` names no record")),
        "{found:?}"
    );
}

#[test]
fn the_summary_counts_records_by_kind() {
    let deviation = INTERPRETATION
        .replace(
            "kind = \"interpretation\"",
            "kind = \"deviation\"\ndeviation = \"platform-gap\"\ndefault = \"on\"\nrevisit = \"x\"",
        )
        .replace("lpdu-time-per-burst", "d");
    let difference = INTERPRETATION
        .replace("\"interpretation\"", "\"obudpst-difference\"")
        .replace("lpdu-time-per-burst", "o")
        .replace(
            "tests = [\"crates/c/tests/t.rs::proves_it\"]\n",
            "obudpst = \"x\"\n",
        );
    let summary = run(&format!("{INTERPRETATION}{deviation}{difference}")).summary();
    for row in [
        "| Deviation | 1 | 0 |",
        "| &nbsp;&nbsp;platform gap | 1 | 0 |",
        "| Interpretation | 1 | 0 |",
        "| OB-UDPST Difference | 1 | 1 |",
        "Unproven: o.",
    ] {
        assert!(summary.contains(row), "{row}: {summary}");
    }
}

#[test]
fn a_record_needs_a_summary() {
    let toml = INTERPRETATION.replace(
        "summary = \"Every datagram of a Burst carries the same lpduTime.\"",
        "summary = \" \"",
    );
    assert!(
        problems(&run(&toml))
            .iter()
            .any(|p| p.contains("summary is empty"))
    );
}

#[test]
fn a_setting_must_name_an_option_not_just_a_word() {
    // "row" is in the sources, but no option is called that.
    let toml = format!("{INTERPRETATION}setting = \"--row\"\ndefault = \"on\"\n");
    assert!(
        problems(&run(&toml))
            .iter()
            .any(|p| p.contains("setting --row"))
    );
}

#[test]
fn a_note_that_doesnt_close_on_its_line_is_a_problem() {
    let source = SourceFile::scan(
        "a.rs",
        "// Fine (conformance: a, b).\n// Wrapped (conformance: a,\n// b).\n",
    );
    assert_eq!(source.notes, ["a", "b"]);
    assert_eq!(source.malformed_notes, ["// Wrapped (conformance: a,"]);
}

#[test]
fn the_mask_list_explains_its_conditions() {
    let masked = format!(
        "{INTERPRETATION}obudpst = \"x\"\nmasks = [{{ pdu = \"setup-request\", fields = [\"mc_count\"], when = [\"upstream\"] }}]\n"
    );
    let toml = run(&masked).masks_toml();
    let parsed: toml::Value = toml::from_str(&toml).expect("valid TOML");
    assert_eq!(
        parsed["conditions"]["upstream"].as_str(),
        Some("an Upstream Test")
    );
}
