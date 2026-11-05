//! The conformance pages, generated from the same data and checks as
//! `cargo xtask req`: coverage per section, every Requirement with its
//! state and proving tests, and the Conformance Records by kind.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use super::html_escape;
use crate::records::{DeviationKind, Kind, Record, TestStatus};
use crate::req::{Loaded, Status, section_label};

/// The states in the order the pages count them.
pub const STATES: [Status; 7] = [
    Status::Tested,
    Status::Nightly,
    Status::Pending,
    Status::Deviation,
    Status::Interpretation,
    Status::Operational,
    Status::NotApplicable,
];

/// One section's coverage.
pub struct Section {
    /// `rfc9946-06`.
    pub file: String,
    /// `RFC 9946 §6`.
    pub label: String,
    /// The section's title, from its file's first comment.
    pub title: String,
    /// Requirement count per state, in `STATES` order.
    pub counts: [usize; 7],
}

impl Section {
    pub fn total(&self) -> usize {
        self.counts.iter().sum()
    }
    pub fn page(&self) -> String {
        format!("conformance/{}.md", self.file)
    }
}

/// What the landing page and the pages report.
pub struct Facts {
    pub sections: Vec<Section>,
    /// Record counts by kind: Deviation, Interpretation, OB-UDPST Difference.
    pub deviations: usize,
    pub interpretations: usize,
    pub obudpst_differences: usize,
    /// Records that are, or include, an OB-UDPST Difference.
    pub with_obudpst: usize,
}

impl Facts {
    pub fn total(&self) -> usize {
        self.sections.iter().map(Section::total).sum()
    }
    pub fn count(&self, state: Status) -> usize {
        let index = STATES.iter().position(|s| *s == state).unwrap_or(0);
        self.sections.iter().map(|s| s.counts[index]).sum()
    }
    pub fn records(&self) -> usize {
        self.deviations + self.interpretations + self.obudpst_differences
    }
}

/// The pages of the Conformance part, and the record pages by record ID.
pub struct Output {
    pub pages: Vec<(String, String)>,
    pub record_pages: BTreeMap<String, String>,
    pub facts: Facts,
    /// How many references to docs/research notes the record pages left
    /// out, for the build log.
    pub left_out_research: usize,
}

fn kind_page(kind: Kind) -> &'static str {
    match kind {
        Kind::Deviation => "conformance/deviations.md",
        Kind::Interpretation => "conformance/interpretations.md",
        Kind::ObudpstDifference => "conformance/obudpst-differences.md",
    }
}

fn deviation_label(kind: DeviationKind) -> &'static str {
    match kind {
        DeviationKind::NonConformance => "Non-conformances",
        DeviationKind::InteropAccommodation => "Interop accommodations",
        DeviationKind::PlatformGap => "Platform gaps",
        DeviationKind::UnmetShould => "Unmet SHOULDs",
    }
}

/// A test path `crates/x/tests/y.rs::name` linked to its file on GitHub.
fn test_link(test: &str, git_ref: &str) -> String {
    let file = test.split("::").next().unwrap_or(test);
    format!("[`{}`]({}/blob/{git_ref}/{file})", test, super::REPO)
}

/// A record's references, linked: ADRs to their page, tickets to GitHub.
/// Research notes stay off the site, so they are left out.
fn references(
    references: &[String],
    adrs: &BTreeMap<String, String>,
    from: &str,
    left_out: &mut usize,
) -> Vec<String> {
    let mut out = Vec::new();
    for reference in references {
        let reference = reference.trim();
        if let Some(number) = reference.strip_prefix("ADR ") {
            match adrs.get(number.trim()) {
                Some(page) => out.push(format!(
                    "[{reference}]({})",
                    super::markdown::relative_to(from, page)
                )),
                None => out.push(reference.to_string()),
            }
        } else if let Some(number) = reference.strip_prefix('#') {
            out.push(format!("[{reference}]({}/issues/{number})", super::REPO));
        } else if reference.starts_with("docs/research") {
            *left_out += 1;
        } else {
            out.push(reference.to_string());
        }
    }
    out
}

/// Generates the Conformance part. `adrs` maps ADR numbers to their pages.
pub fn generate(
    loaded: &Loaded,
    adrs: &BTreeMap<String, String>,
    git_ref: &str,
) -> Result<Output, String> {
    // Tests by the Requirement they tag, with whether they run on the gating
    // platform.
    let mut tests: BTreeMap<&str, Vec<(String, bool)>> = BTreeMap::new();
    for source in &loaded.sources {
        for function in &source.functions {
            if !function.test {
                continue;
            }
            let runs = function.runs_on_gating_platform(source.cfg.as_deref());
            for tag in &function.tags {
                tests
                    .entry(tag.as_str())
                    .or_default()
                    .push((format!("{}::{}", source.path, function.name), runs));
            }
        }
    }
    let status_of: BTreeMap<&str, Status> = loaded
        .checked
        .files
        .iter()
        .flat_map(|(_, statuses)| statuses.iter())
        .map(|(id, status)| (id.as_str(), *status))
        .collect();

    // Where each record is shown, for links from the Requirements.
    let records: Vec<&Record> = loaded
        .record_files
        .iter()
        .flat_map(|f| f.records.iter())
        .collect();
    let record_pages: BTreeMap<String, String> = records
        .iter()
        .map(|r| (r.id.clone(), kind_page(r.kind).to_string()))
        .collect();
    let record_link = |id: &str, from: &str| match record_pages.get(id) {
        Some(page) => format!("[{id}]({}#{id})", super::markdown::relative_to(from, page)),
        None => id.to_string(),
    };
    let reference_link = |reference: &str, from: &str| {
        let reference = reference.trim();
        match reference.strip_prefix("ADR ") {
            Some(number) => match adrs.get(number.trim()) {
                Some(page) => format!(
                    "[{reference}]({})",
                    super::markdown::relative_to(from, page)
                ),
                None => reference.to_string(),
            },
            None => record_link(reference, from),
        }
    };

    // Requirements, one page per RFC section.
    let mut pages = Vec::new();
    let mut sections = Vec::new();
    let mut requirement_pages: BTreeMap<String, String> = BTreeMap::new();
    for file in &loaded.files {
        let stem = file.name.trim_end_matches(".toml").to_string();
        for requirement in &file.requirements {
            requirement_pages.insert(requirement.id.clone(), format!("conformance/{stem}.md"));
        }
    }
    for file in &loaded.files {
        let stem = file.name.trim_end_matches(".toml").to_string();
        let page = format!("conformance/{stem}.md");
        let label = section_label(&file.name);
        let text = super::read(
            &crate::workspace_root()
                .join("docs/conformance/requirements")
                .join(&file.name),
        )?;
        let title = text
            .lines()
            .next()
            .and_then(|l| l.split_once(": "))
            .map(|(_, t)| t.trim().trim_end_matches('.').to_string())
            .unwrap_or_default();
        let mut counts = [0usize; 7];
        for requirement in &file.requirements {
            let status = status_of
                .get(requirement.id.as_str())
                .copied()
                .unwrap_or(Status::Pending);
            let index = STATES.iter().position(|s| *s == status).unwrap_or(0);
            counts[index] += 1;
        }
        let mut out = format!("# {label}: {title}\n\n");
        let _ = writeln!(
            out,
            "The {} Requirements of {label}, each quoted from the RFC with its state and the \
             tests that prove it. [Conformance](index.md) explains the states.\n",
            file.requirements.len()
        );
        out.push_str("| State | Requirements |\n|---|--:|\n");
        for (state, count) in STATES.iter().zip(counts) {
            if count > 0 {
                let _ = writeln!(out, "| {} | {count} |", state.label());
            }
        }
        out.push('\n');
        for requirement in &file.requirements {
            let status = status_of
                .get(requirement.id.as_str())
                .copied()
                .unwrap_or(Status::Pending);
            let id = &requirement.id;
            let _ = writeln!(out, "## {id} {{#{id}}}\n");
            let _ = writeln!(out, "**{}**\n", status.label());
            let quoted = requirement
                .text
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let _ = writeln!(out, "> {}\n", markdown_text(&quoted));
            if let Some(note) = &requirement.note {
                let _ = writeln!(out, "Note: {}\n", note.trim());
            }
            let refs: Vec<String> = requirement
                .reference
                .iter()
                .map(|r| reference_link(r, &page))
                .collect();
            if !refs.is_empty() {
                let _ = writeln!(out, "Recorded in {}.\n", refs.join(", "));
            }
            let mut linked: Vec<String> = records
                .iter()
                .filter(|r| r.requirements.iter().any(|q| q == id))
                .filter(|r| !requirement.reference.iter().any(|x| x.trim() == r.id))
                .map(|r| record_link(&r.id, &page))
                .collect();
            linked.dedup();
            if !linked.is_empty() {
                let _ = writeln!(out, "Conformance Records: {}.\n", linked.join(", "));
            }
            match tests.get(id.as_str()) {
                Some(list) if !list.is_empty() => {
                    out.push_str("Proving tests:\n\n");
                    for (test, runs) in list {
                        let where_ = if *runs {
                            ""
                        } else {
                            " (nightly or not on Linux)"
                        };
                        let _ = writeln!(out, "- {}{where_}", test_link(test, git_ref));
                    }
                    out.push('\n');
                }
                _ => {}
            }
        }
        sections.push(Section {
            file: stem,
            label,
            title,
            counts,
        });
        pages.push((page, out));
    }

    // Records, one page per kind.
    let statuses: BTreeMap<&str, &crate::records::RecordStatus> = loaded
        .records
        .records
        .iter()
        .map(|r| (r.id.as_str(), r))
        .collect();
    let mut left_out_research = 0;
    let render = |record: &Record, page: &str, left_out: &mut usize| -> String {
        let mut out = String::new();
        let id = &record.id;
        let _ = writeln!(out, "### {id} {{#{id}}}\n");
        let _ = writeln!(out, "{}\n", markdown_text(&record.summary));
        let mut facts = Vec::new();
        if record.kind == Kind::Interpretation && record.obudpst.is_some() {
            facts.push("**Also an OB-UDPST Difference.**".to_string());
        }
        if let Some(rfc) = &record.rfc {
            facts.push(format!("**RFC:** {rfc}"));
        }
        if !record.requirements.is_empty() {
            let links: Vec<String> = record
                .requirements
                .iter()
                .map(|q| match requirement_pages.get(q) {
                    Some(to) => format!("[{q}]({}#{q})", super::markdown::relative_to(page, to)),
                    None => q.clone(),
                })
                .collect();
            facts.push(format!("**Requirements:** {}", links.join(", ")));
        }
        if let Some(text) = &record.obudpst {
            facts.push(format!("**OB-UDPST:** {}", markdown_text(text)));
        }
        if let Some(default) = record.default {
            let default = match default {
                crate::records::Default::On => "on",
                crate::records::Default::Off => "off",
            };
            facts.push(format!("**By default:** {default}"));
        }
        if let Some(setting) = &record.setting {
            facts.push(format!("**Setting:** {setting}"));
        }
        if let Some(revisit) = &record.revisit {
            facts.push(format!("**Revisit:** {}", markdown_text(revisit)));
        }
        let refs = references(&record.reference, adrs, page, left_out);
        if !refs.is_empty() {
            facts.push(format!("**References:** {}", refs.join(", ")));
        }
        for fact in facts {
            let _ = writeln!(out, "{fact}\\");
        }
        if out.ends_with("\\\n") {
            out.truncate(out.len() - 2);
            out.push('\n');
        }
        out.push('\n');
        let status = statuses.get(id.as_str());
        if record.tests.is_empty() {
            out.push_str("No proving test.\n\n");
        } else {
            let unproven = status.is_some_and(|s| !s.proven());
            out.push_str(if unproven {
                "Proving tests (none runs yet, so the record is unproven):\n\n"
            } else {
                "Proving tests:\n\n"
            });
            for test in &record.tests {
                let state = status
                    .and_then(|s| s.tests.iter().find(|(t, _)| t == test))
                    .map(|(_, s)| match s {
                        TestStatus::Runs => String::new(),
                        other => format!(" ({other})"),
                    })
                    .unwrap_or_default();
                let _ = writeln!(out, "- {}{state}", test_link(test, git_ref));
            }
            out.push('\n');
        }
        out
    };

    let count = |kind: Kind| records.iter().filter(|r| r.kind == kind).count();
    let deviations = count(Kind::Deviation);
    let interpretations = count(Kind::Interpretation);
    let obudpst_differences = count(Kind::ObudpstDifference);
    let with_obudpst = records
        .iter()
        .filter(|r| r.kind == Kind::ObudpstDifference || r.obudpst.is_some())
        .count();

    // Deviations, by the kind of departure.
    let page = kind_page(Kind::Deviation);
    let mut out = format!(
        "# Deviations\n\n\
         A Deviation is a recorded departure from a Requirement, with its reason. udpstp has \
         {deviations}. Each says whether it applies by default, which setting controls it, and \
         what would make it worth revisiting.\n\n"
    );
    for sub in [
        DeviationKind::NonConformance,
        DeviationKind::InteropAccommodation,
        DeviationKind::PlatformGap,
        DeviationKind::UnmetShould,
    ] {
        let group: Vec<&&Record> = records
            .iter()
            .filter(|r| r.kind == Kind::Deviation && r.deviation == Some(sub))
            .collect();
        if group.is_empty() {
            continue;
        }
        let _ = writeln!(out, "## {} ({})\n", deviation_label(sub), group.len());
        for record in group {
            out.push_str(&render(record, page, &mut left_out_research));
        }
    }
    pages.push((page.to_string(), out));

    for (kind, title, intro) in [
        (
            Kind::Interpretation,
            "Interpretations",
            format!(
                "An Interpretation is a recorded reading of a Requirement that is ambiguous, \
                 silent on a case or in conflict with another, or a choice the RFC leaves to the \
                 implementation; udpstp still conforms. There are {interpretations}. Those marked \
                 as also an OB-UDPST Difference are readings OB-UDPST doesn't share.\n\n"
            ),
        ),
        (
            Kind::ObudpstDifference,
            "OB-UDPST Differences",
            format!(
                "An OB-UDPST Difference is behaviour in which udpstp and OB-UDPST differ while \
                 udpstp conforms to the RFC. {obudpst_differences} records are OB-UDPST \
                 Differences only; with the [Interpretations](interpretations.md) OB-UDPST \
                 doesn't share, {with_obudpst} records describe a difference from OB-UDPST.\n\n"
            ),
        ),
    ] {
        let page = kind_page(kind);
        let mut out = format!("# {title}\n\n{intro}");
        // Grouped by the records file, the area of behaviour.
        for file in &loaded.record_files {
            let group: Vec<&Record> = file.records.iter().filter(|r| r.kind == kind).collect();
            if group.is_empty() {
                continue;
            }
            let area = area_title(&file.name);
            let _ = writeln!(out, "## {area} ({})\n", group.len());
            for record in group {
                out.push_str(&render(record, page, &mut left_out_research));
            }
        }
        pages.push((page.to_string(), out));
    }

    let facts = Facts {
        sections,
        deviations,
        interpretations,
        obudpst_differences,
        with_obudpst,
    };
    pages.insert(0, ("conformance/index.md".into(), overview(&facts)));
    Ok(Output {
        pages,
        record_pages,
        facts,
        left_out_research,
    })
}

/// A records file's area of behaviour, from its name.
fn area_title(file: &str) -> String {
    match file.trim_end_matches(".toml") {
        "algorithms" => "Load Rate Adjustment Algorithms".into(),
        "client" => "The Client".into(),
        "control" => "The Control Phase and Admission".into(),
        "data-phase" => "The Data Phase".into(),
        "keys" => "Keys and authentication".into(),
        other => other.to_string(),
    }
}

/// Text from TOML shown as Markdown: `<` and `>` escaped outside code
/// spans, the rest kept.
fn markdown_text(text: &str) -> String {
    super::markdown::escape_outside_code(text, |t| t.replace('<', "&lt;").replace('>', "&gt;"))
}

fn overview(facts: &Facts) -> String {
    let total = facts.total();
    let tested = facts.count(Status::Tested) + facts.count(Status::Nightly);
    let mut out = format!(
        "# Conformance\n\n\
         Every normative statement of RFC 9946, and of the RFC 9097 sections it uses, is a \
         Requirement with a stable ID: {total} in all. A Requirement counts as met when a test \
         that runs on Linux proves it, unless it is recorded otherwise. These pages are \
         generated by `cargo xtask req`'s checks when the site is built.\n\n\
         **{tested} of {total}** Requirements are proven by tests. Of the rest, \
         {deviation} are Deviations, each recorded with its reason, {operational} are \
         operational (they concern how udpstp is deployed or used rather than its code), and \
         {na} don't apply to udpstp.\n\n",
        deviation = facts.count(Status::Deviation),
        operational = facts.count(Status::Operational),
        na = facts.count(Status::NotApplicable),
    );
    out.push_str("## Coverage by section\n\n");
    let shown: Vec<(usize, Status)> = STATES
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, s)| facts.count(*s) > 0)
        .collect();
    out.push_str("| Section | Requirements |");
    for (_, state) in &shown {
        let _ = write!(out, " {} |", state.label());
    }
    out.push_str("\n|---|--:|");
    for _ in &shown {
        out.push_str("--:|");
    }
    out.push('\n');
    for section in &facts.sections {
        let _ = write!(
            out,
            "| [{}: {}]({}.md) | {} |",
            section.label,
            html_escape(&section.title),
            section.file,
            section.total()
        );
        for (index, _) in &shown {
            let _ = write!(out, " {} |", section.counts[*index]);
        }
        out.push('\n');
    }
    let _ = write!(out, "| **Total** | **{total}** |");
    for (_, state) in &shown {
        let _ = write!(out, " **{}** |", facts.count(*state));
    }
    out.push_str("\n\n");
    let _ = writeln!(
        out,
        "## Conformance Records\n\n\
         Where udpstp departs from a Requirement, reads an ambiguous one, or differs from \
         OB-UDPST, a Conformance Record says what it does, why, and which tests prove it.\n\n\
         | Kind | Records |\n|---|--:|\n\
         | [Deviations](deviations.md) | {} |\n\
         | [Interpretations](interpretations.md) | {} |\n\
         | [OB-UDPST Differences](obudpst-differences.md) | {} |\n\n\
         [How the records work](format.md) describes the files these pages come from.",
        facts.deviations, facts.interpretations, facts.obudpst_differences
    );
    out.push_str("\n## States\n\n");
    for (state, meaning) in [
        (
            Status::Tested,
            "a test that runs on Linux, in CI and before every push, proves it",
        ),
        (
            Status::Nightly,
            "only tests in the nightly workflow prove it",
        ),
        (Status::Pending, "no test proves it yet"),
        (
            Status::Deviation,
            "udpstp departs from it, as a Deviation records",
        ),
        (
            Status::Interpretation,
            "udpstp meets it as an Interpretation reads it",
        ),
        (
            Status::Operational,
            "it concerns deployment or use, not udpstp's code",
        ),
        (Status::NotApplicable, "it doesn't apply to udpstp"),
    ] {
        let _ = writeln!(out, "- **{}**: {meaning}.", state.label());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_text_escapes_outside_code_only() {
        assert_eq!(markdown_text("a <b> and `<c>`"), "a &lt;b&gt; and `<c>`");
    }

    #[test]
    fn test_links_point_at_the_file_at_the_ref() {
        assert_eq!(
            test_link("crates/x/tests/y.rs::a_test", "v0.1.0"),
            "[`crates/x/tests/y.rs::a_test`](https://github.com/pederbe/udpstp/blob/v0.1.0/crates/x/tests/y.rs)"
        );
    }

    /// The pages from the repository's own requirements and records: every
    /// section has its page, the counts add up, and every record's OB-UDPST
    /// line is shown.
    #[test]
    fn the_real_conformance_data_makes_consistent_pages() {
        let loaded = crate::req::load(&crate::workspace_root()).unwrap();
        let adrs = BTreeMap::from([("0009".to_string(), "decisions/0009.md".to_string())]);
        let output = generate(&loaded, &adrs, "v0.1.0").unwrap();
        let page = |path: &str| {
            output
                .pages
                .iter()
                .find(|(p, _)| p == path)
                .map(|(_, markdown)| markdown.as_str())
                .unwrap_or_else(|| panic!("no page {path}"))
        };
        assert_eq!(output.facts.sections.len(), loaded.files.len());
        for section in &output.facts.sections {
            page(&section.page());
        }
        let requirements: usize = loaded.files.iter().map(|f| f.requirements.len()).sum();
        assert_eq!(output.facts.total(), requirements);
        let records = loaded
            .record_files
            .iter()
            .map(|f| f.records.len())
            .sum::<usize>();
        assert_eq!(output.facts.records(), records);
        assert!(page("conformance/index.md").contains(&format!("{requirements} in all")));

        let all: String = output.pages.iter().map(|(_, m)| m.as_str()).collect();
        let shown = all.matches("**OB-UDPST:**").count();
        let with_line = loaded
            .record_files
            .iter()
            .flat_map(|f| &f.records)
            .filter(|r| r.obudpst.is_some())
            .count();
        assert_eq!(shown, with_line);
    }
}
