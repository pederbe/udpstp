//! Tests for the traceability checker. The inputs are small in-memory
//! requirement files, RFC text and Rust sources.

use super::*;

const RFC: &str = "\
6.1.  Client Generates Test Setup Request

   The client SHALL begin the Control phase exchange by sending a Test
   Setup Request message to the server's (standard) control port.

   Note: Any field labeled as 'reserved for alignment', in any PDU, MUST
   be set to 0 and MUST be ignored upon receipt.

6.2.  Server Test Setup Request Processing and Response Generation

   The server SHALL first perform the following.
";

fn requirement_toml(id: &str, text: &str, hash: &str, state: &str) -> String {
    format!(
        "[[requirement]]\nid = \"{id}\"\ntext = \"{text}\"\ntext_sha256 = \"{hash}\"\nstate = \"{state}\"\n"
    )
}

const BEGIN_TEXT: &str = "The client SHALL begin the Control phase exchange by sending a Test Setup Request message to the server's (standard) control port.";

fn begin_requirement(state: &str) -> String {
    requirement_toml("RFC9946-6.1-1", BEGIN_TEXT, &text_hash(BEGIN_TEXT), state)
}

fn check_one(toml: &str, sources: &[(&str, &str)]) -> Checked {
    let file = RequirementsFile::parse("rfc9946-06.toml", toml).expect("valid TOML");
    let sources: Vec<SourceFile> = sources
        .iter()
        .map(|(path, text)| SourceFile::scan(path, text))
        .collect();
    let mut checked = check(&[file], &[("RFC9946", RFC)], &sources, &["0002"]);
    // One requirement leaves the fixture's other keywords unrecorded; the
    // completeness tests below check those problems.
    checked
        .problems
        .retain(|p| !matches!(p, Problem::KeywordOutsideRequirements { .. }));
    checked
}

#[test]
fn a_requirement_with_a_tag_is_tested_and_one_without_is_pending() {
    let untested = format!("{}ticket = 24\n", begin_requirement("pending"));
    let checked = check_one(&untested, &[]);
    assert!(checked.problems.is_empty(), "{:?}", checked.problems);
    assert_eq!(checked.status("RFC9946-6.1-1"), Some(Status::Pending));

    let tagged = "#[req(\"RFC9946-6.1-1\")]\n#[test]\nfn begins() {}\n";
    let checked = check_one(&begin_requirement("pending"), &[("tests/a.rs", tagged)]);
    assert!(checked.problems.is_empty(), "{:?}", checked.problems);
    assert_eq!(checked.status("RFC9946-6.1-1"), Some(Status::Tested));
}

#[test]
fn deviation_interpretation_and_operational_states_come_from_the_file() {
    for (state, expected) in [
        ("deviation", Status::Deviation),
        ("interpretation", Status::Interpretation),
        ("operational", Status::Operational),
    ] {
        let toml = format!("{}reference = \"ADR 0002\"\n", begin_requirement(state));
        let checked = check_one(&toml, &[]);
        assert!(
            checked.problems.is_empty(),
            "{state}: {:?}",
            checked.problems
        );
        assert_eq!(checked.status("RFC9946-6.1-1"), Some(expected), "{state}");
    }
}

#[test]
fn deviations_and_interpretations_must_name_their_reference() {
    let checked = check_one(&begin_requirement("interpretation"), &[]);
    assert!(
        checked
            .problems
            .iter()
            .any(|p| matches!(p, Problem::MissingReference { id } if id == "RFC9946-6.1-1")),
        "{:?}",
        checked.problems
    );
}

#[test]
fn a_tag_naming_an_unknown_id_is_a_problem() {
    let tagged = "#[req(\"RFC9946-6.1-9\")]\n#[test]\nfn nope() {}\n";
    let checked = check_one(&begin_requirement("pending"), &[("tests/a.rs", tagged)]);
    assert!(
        checked.problems.iter().any(
            |p| matches!(p, Problem::UnknownTag { id, file } if id == "RFC9946-6.1-9" && file == "tests/a.rs")
        ),
        "{:?}",
        checked.problems
    );
}

#[test]
fn a_text_whose_hash_no_longer_matches_is_a_problem() {
    let toml = requirement_toml("RFC9946-6.1-1", BEGIN_TEXT, "0000000000000000", "pending");
    let checked = check_one(&toml, &[]);
    assert!(
        checked
            .problems
            .iter()
            .any(|p| matches!(p, Problem::HashMismatch { id } if id == "RFC9946-6.1-1")),
        "{:?}",
        checked.problems
    );
}

#[test]
fn a_text_not_found_in_the_rfc_is_a_problem() {
    let text = "The client SHALL do something the RFC never says.";
    let checked = check_one(
        &requirement_toml("RFC9946-6.1-2", text, &text_hash(text), "pending"),
        &[],
    );
    assert!(
        checked
            .problems
            .iter()
            .any(|p| matches!(p, Problem::TextNotInRfc { id } if id == "RFC9946-6.1-2")),
        "{:?}",
        checked.problems
    );
}

#[test]
fn text_matches_across_the_rfcs_line_wrapping() {
    // BEGIN_TEXT is one line; in the RFC it wraps over two indented lines.
    let checked = check_one(&begin_requirement("pending"), &[]);
    assert!(
        !checked
            .problems
            .iter()
            .any(|p| matches!(p, Problem::TextNotInRfc { .. })),
        "{:?}",
        checked.problems
    );
}

#[test]
fn duplicate_ids_are_a_problem() {
    let toml = format!(
        "{}{}",
        begin_requirement("pending"),
        begin_requirement("pending")
    );
    let checked = check_one(&toml, &[]);
    assert!(
        checked
            .problems
            .iter()
            .any(|p| matches!(p, Problem::DuplicateId { id } if id == "RFC9946-6.1-1")),
        "{:?}",
        checked.problems
    );
}

#[test]
fn tags_with_several_ids_and_section_markers_are_found() {
    let source = "\
// req: RFC 9946 §6.2 (verify the size)
#[test]
fn a() {}

#[req(\"RFC9946-6.1-1\", \"RFC9946-6.1-2\")]
#[test]
fn b() {}

#[udpstp_req::req(\"RFC9946-6.1-3\")]
#[test]
fn c() {}
";
    let scanned = SourceFile::scan("tests/x.rs", source);
    assert_eq!(
        scanned.tags,
        vec!["RFC9946-6.1-1", "RFC9946-6.1-2", "RFC9946-6.1-3"]
    );
    assert_eq!(
        scanned.section_markers,
        vec!["RFC 9946 §6.2 (verify the size)"]
    );
}

#[test]
fn tags_wrapped_over_several_lines_are_found() {
    let source = "\
#[udpstp_req::req(
    \"RFC9946-6.1-1\",
    \"RFC9946-6.1-2\"
)]
#[test]
fn wrapped() {}
";
    let scanned = SourceFile::scan("tests/x.rs", source);
    assert_eq!(scanned.tags, vec!["RFC9946-6.1-1", "RFC9946-6.1-2"]);
}

#[test]
fn section_markers_keep_their_continuation_lines() {
    let source = "// req: RFC 9946 §7.1 (Activation Request sent to the testPort\n// from the Setup Response)\n#[test]\nfn a() {}\n";
    let scanned = SourceFile::scan("tests/x.rs", source);
    assert_eq!(
        scanned.section_markers,
        vec!["RFC 9946 §7.1 (Activation Request sent to the testPort from the Setup Response)"]
    );
}

#[test]
fn text_from_another_subsection_is_a_problem() {
    // BEGIN_TEXT is in §6.1 of the fixture, not §6.2.
    let toml = requirement_toml(
        "RFC9946-6.2-1",
        BEGIN_TEXT,
        &text_hash(BEGIN_TEXT),
        "pending",
    );
    let checked = check_one(&toml, &[]);
    assert!(
        checked
            .problems
            .iter()
            .any(|p| matches!(p, Problem::TextNotInRfc { id } if id == "RFC9946-6.2-1")),
        "{:?}",
        checked.problems
    );
}

#[test]
fn a_reference_to_a_missing_adr_is_a_problem() {
    let toml = format!(
        "{}reference = \"ADR 0099\"\n",
        begin_requirement("interpretation")
    );
    let file = RequirementsFile::parse("rfc9946-06.toml", &toml).unwrap();
    let checked = check(&[file], &[("RFC9946", RFC)], &[], &["0001", "0002"]);
    assert!(
        checked
            .problems
            .iter()
            .any(|p| matches!(p, Problem::UnknownReference { id, .. } if id == "RFC9946-6.1-1")),
        "{:?}",
        checked.problems
    );
}

#[test]
fn the_report_lists_coverage_per_section_and_the_section_markers() {
    let tagged = "#[req(\"RFC9946-6.1-1\")]\n#[test]\nfn begins() {}\n// req: RFC 9946 §6.2\n";
    let checked = check_one(&begin_requirement("pending"), &[("tests/a.rs", tagged)]);
    let report = checked.report();
    assert!(report.contains("rfc9946-06.toml"), "{report}");
    assert!(report.contains("1 tested"), "{report}");
    assert!(report.contains("RFC 9946 §6.2"), "{report}");
}

#[test]
fn the_summary_counts_each_state_per_section_and_in_total() {
    let tagged = "#[req(\"RFC9946-6.1-1\")]\n#[test]\nfn begins() {}\n// req: RFC 9946 §6.2\n";
    let checked = check_one(&begin_requirement("pending"), &[("tests/a.rs", tagged)]);
    let summary = checked.summary();
    assert!(
        summary.contains("| RFC 9946 §6 | 1 | 1 | 0 | 0 | 0 | 0 | 0 | 0 |"),
        "{summary}"
    );
    assert!(
        summary.contains("| **Total** | **1** | **1** | **0** |"),
        "{summary}"
    );
    assert!(summary.contains("section marker, written before their requirement had an ID: 1."));
}

#[test]
fn section_labels_name_the_rfc_and_section() {
    assert_eq!(section_label("rfc9946-06.toml"), "RFC 9946 §6");
    assert_eq!(section_label("rfc9097-10.toml"), "RFC 9097 §10");
    assert_eq!(section_label("appendix.toml"), "appendix.toml");
}

#[test]
fn a_reference_may_be_a_list_and_each_adr_in_it_must_exist() {
    let toml = format!(
        "{}reference = [\"ADR 0002\", \"some-record\", \"ADR 0099\"]\n",
        begin_requirement("deviation")
    );
    let file = RequirementsFile::parse("rfc9946-06.toml", &toml).unwrap();
    let views: Vec<_> = file.views().collect();
    assert_eq!(views[0].references, ["ADR 0002", "some-record", "ADR 0099"]);
    let checked = check(&[file], &[("RFC9946", RFC)], &[], &["0002"]);
    let unknown: Vec<&Problem> = checked
        .problems
        .iter()
        .filter(|p| matches!(p, Problem::UnknownReference { .. }))
        .collect();
    assert!(
        matches!(unknown[..], [Problem::UnknownReference { reference, .. }] if reference == "ADR 0099"),
        "{:?}",
        checked.problems
    );
}

fn has(checked: &Checked, problem: impl Fn(&Problem) -> bool) -> bool {
    checked.problems.iter().any(problem)
}

#[test]
fn a_pending_requirement_must_name_a_ticket_and_the_summary_lists_it() {
    let checked = check_one(&begin_requirement("pending"), &[]);
    assert!(
        has(
            &checked,
            |p| matches!(p, Problem::Unclassified { id } if id == "RFC9946-6.1-1")
        ),
        "{:?}",
        checked.problems
    );

    let toml = format!("{}ticket = [24, 67]\n", begin_requirement("pending"));
    let checked = check_one(&toml, &[]);
    assert!(checked.problems.is_empty(), "{:?}", checked.problems);
    let summary = checked.summary();
    assert!(
        summary.contains("- [#24](https://github.com/pederbe/udpstp/issues/24): RFC9946-6.1-1"),
        "{summary}"
    );
    assert!(summary.contains("[#67]"), "{summary}");
}

#[test]
fn a_tested_requirement_still_naming_a_ticket_is_a_problem() {
    let toml = format!("{}ticket = 24\n", begin_requirement("pending"));
    let tagged = "#[req(\"RFC9946-6.1-1\")]\n#[test]\nfn begins() {}\n";
    let checked = check_one(&toml, &[("tests/a.rs", tagged)]);
    assert!(
        has(&checked, |p| matches!(p, Problem::StaleTicket { .. })),
        "{:?}",
        checked.problems
    );
}

#[test]
fn a_requirement_whose_only_tests_dont_run_on_linux_is_a_problem() {
    let toml = format!("{}ticket = 24\n", begin_requirement("pending"));
    for (what, source) in [
        (
            "ignored",
            "#[req(\"RFC9946-6.1-1\")]\n#[test]\n#[ignore]\nfn begins() {}\n",
        ),
        (
            "Windows only",
            "#[req(\"RFC9946-6.1-1\")]\n#[cfg(windows)]\n#[test]\nfn begins() {}\n",
        ),
        (
            "macOS only, for the file",
            "#![cfg(target_os = \"macos\")]\n#[req(\"RFC9946-6.1-1\")]\n#[test]\nfn begins() {}\n",
        ),
    ] {
        let checked = check_one(&toml, &[("tests/a.rs", source)]);
        assert!(
            has(&checked, |p| matches!(p, Problem::NotGating { .. })),
            "{what}: {:?}",
            checked.problems
        );
        assert_eq!(
            checked.status("RFC9946-6.1-1"),
            Some(Status::Pending),
            "{what}"
        );
    }
}

#[test]
fn tests_compiled_on_linux_count() {
    for source in [
        "#![cfg(target_os = \"linux\")]\n#[req(\"RFC9946-6.1-1\")]\n#[test]\nfn begins() {}\n",
        "#[req(\"RFC9946-6.1-1\")]\n#[cfg(unix)]\n#[test]\nfn begins() {}\n",
        "#[req(\"RFC9946-6.1-1\")]\n#[cfg(any(windows, target_os = \"linux\"))]\n#[test]\nfn begins() {}\n",
        "#[req(\"RFC9946-6.1-1\")]\n#[cfg(not(target_vendor = \"apple\"))]\n#[test]\nfn begins() {}\n",
    ] {
        let checked = check_one(&begin_requirement("pending"), &[("tests/a.rs", source)]);
        assert!(
            checked.problems.is_empty(),
            "{source}: {:?}",
            checked.problems
        );
        assert_eq!(
            checked.status("RFC9946-6.1-1"),
            Some(Status::Tested),
            "{source}"
        );
    }
}

#[test]
fn a_requirement_tested_only_nightly_is_classified_and_counted_apart() {
    let toml = format!("{}nightly = true\n", begin_requirement("pending"));
    let ignored = "#[req(\"RFC9946-6.1-1\")]\n#[test]\n#[ignore = \"nightly\"]\nfn begins() {}\n";
    let checked = check_one(&toml, &[("tests/a.rs", ignored)]);
    assert!(checked.problems.is_empty(), "{:?}", checked.problems);
    assert_eq!(checked.status("RFC9946-6.1-1"), Some(Status::Nightly));
    assert!(checked.report().contains("1 tested nightly"));

    // Once a gating test covers it, the classification is stale.
    let gating = "#[req(\"RFC9946-6.1-1\")]\n#[test]\nfn begins() {}\n";
    let checked = check_one(&toml, &[("tests/a.rs", gating)]);
    assert!(
        has(&checked, |p| matches!(p, Problem::StaleNightly { .. })),
        "{:?}",
        checked.problems
    );
}

#[test]
fn a_tag_on_a_function_that_isnt_a_test_is_a_problem() {
    let source = "#[req(\"RFC9946-6.1-1\")]\nfn helper() {}\n";
    let checked = check_one(&begin_requirement("pending"), &[("tests/a.rs", source)]);
    assert!(
        has(
            &checked,
            |p| matches!(p, Problem::TagOnNonTest { function, .. } if function == "helper")
        ),
        "{:?}",
        checked.problems
    );
}

const RESERVED_TEXT: &str = "Note: Any field labeled as 'reserved for alignment', in any PDU, MUST be set to 0 and MUST be ignored upon receipt.";
const FIRST_TEXT: &str = "The server SHALL first perform the following.";

fn complete_check(toml: &str) -> Vec<Problem> {
    let file = RequirementsFile::parse("rfc9946-06.toml", toml).expect("valid TOML");
    check(&[file], &[("RFC9946", RFC)], &[], &["0002"]).problems
}

fn requirement(id: &str, text: &str) -> String {
    format!(
        "{}ticket = 163\n",
        requirement_toml(id, text, &text_hash(text), "pending")
    )
}

#[test]
fn every_keyword_of_a_covered_section_must_be_in_a_requirement() {
    let all = [
        requirement("RFC9946-6.1-1", BEGIN_TEXT),
        requirement("RFC9946-6.1-2", RESERVED_TEXT),
        requirement("RFC9946-6.2-1", FIRST_TEXT),
    ]
    .concat();
    let problems = complete_check(&all);
    assert!(problems.is_empty(), "{problems:?}");

    // Without §6.2's sentence, its SHALL is outside every requirement.
    let missing = [
        requirement("RFC9946-6.1-1", BEGIN_TEXT),
        requirement("RFC9946-6.1-2", RESERVED_TEXT),
    ]
    .concat();
    let problems = complete_check(&missing);
    assert!(
        matches!(&problems[..], [Problem::KeywordOutsideRequirements { section, keyword, .. }] if section == "RFC9946 §6.2" && keyword == "SHALL"),
        "{problems:?}"
    );
}

#[test]
fn keyword_ordinals_follow_the_text() {
    let swapped = [
        requirement("RFC9946-6.1-2", BEGIN_TEXT),
        requirement("RFC9946-6.1-1", RESERVED_TEXT),
        requirement("RFC9946-6.2-1", FIRST_TEXT),
    ]
    .concat();
    let problems = complete_check(&swapped);
    assert!(
        matches!(&problems[..], [Problem::OrdinalOutOfOrder { id, after }] if id == "RFC9946-6.1-2" && after == "RFC9946-6.1-1"),
        "{problems:?}"
    );

    // A lowercase statement found later takes the next free number.
    let lowercase = [
        requirement("RFC9946-6.1-L2", BEGIN_TEXT),
        requirement("RFC9946-6.1-L1", RESERVED_TEXT),
        requirement("RFC9946-6.2-1", FIRST_TEXT),
    ]
    .concat();
    let problems = complete_check(&lowercase);
    assert!(problems.is_empty(), "{problems:?}");
}

#[test]
fn keywords_are_whole_words_and_tables_and_code_are_left_out() {
    assert_eq!(
        keywords("MUST NOT and MAYBE, MUSTARD, SHALL."),
        [(0, "MUST NOT"), (29, "SHALL")]
    );
    let body = "   Text that MAY hold.\n   | a table MUST |\n   +------+\n   <CODE BEGINS>\n   code MUST\n   <CODE ENDS>\n";
    assert_eq!(normalize(&prose(body)), "Text that MAY hold.");
}
