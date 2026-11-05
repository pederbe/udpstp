//! Conformance records: the Deviations, Interpretations and OB-UDPST
//! Differences (GLOSSARY.md, Conformance) kept in TOML files under
//! `docs/conformance/records/`, checked by `cargo xtask req` with the
//! requirements. Each record names the tests that prove it; the report
//! shows whether each one is a test that runs, and flags records with none.
//! Records whose difference from OB-UDPST shows in a Setup, Activation or
//! Null PDU carry masks, from which `cargo xtask req` writes the field
//! list the OB-UDPST interop comparison ignores.

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;

use serde::Deserialize;

use crate::req::SourceFile;

/// Conditions a mask may be limited to, with what each means for an
/// interop run; the harness maps them to its runs. Add one here when a
/// record needs it.
pub const CONDITIONS: &[(&str, &str)] = &[
    ("upstream", "an Upstream Test"),
    ("downstream", "a Downstream Test"),
    ("fixed-rate", "a Fixed-Rate Test"),
    (
        "default-search",
        "a search from row 0: no starting row requested",
    ),
    (
        "operator-key",
        "the Test Connection's key is an Operator Key",
    ),
    (
        "consumer-key",
        "the Test Connection's key isn't an Operator Key",
    ),
    (
        "max-bandwidth-stated",
        "the Setup Request states a maxBandwidth",
    ),
    (
        "no-max-bandwidth-stated",
        "the Setup Request states no maxBandwidth",
    ),
    (
        "row-above-max-bandwidth",
        "a requested starting or Fixed-Rate row above the stated maxBandwidth's row",
    ),
    ("start-row-above-100", "a requested starting row above 100"),
    (
        "row-above-1000",
        "a requested starting or Fixed-Rate row above 1000",
    ),
    (
        "row-beyond-the-top-row",
        "a requested row above the Server's top row (1069 with a 1500-byte MTU)",
    ),
    (
        "test-interval-over-60s",
        "a requested Test Interval above 60 s",
    ),
    (
        "coerced-parameters",
        "Activation parameters udpstp coerces: outside its ranges, lowThresh above upperThresh, subIntPeriod longer than the Test or not a whole number of Trial Intervals, or slowAdjThresh × trialInt above 500 ms",
    ),
    ("dscp-requested", "a nonzero dscpEcn"),
    ("unauthenticated", "Unauthenticated Mode"),
    (
        "troubleshooting-mode",
        "the Server runs in Troubleshooting Mode",
    ),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Deviation,
    Interpretation,
    ObudpstDifference,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Deviation => "Deviation",
            Kind::Interpretation => "Interpretation",
            Kind::ObudpstDifference => "OB-UDPST Difference",
        }
    }
}

/// What kind of departure a Deviation is (docs/spec.md, Conformance records).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeviationKind {
    NonConformance,
    InteropAccommodation,
    PlatformGap,
    UnmetShould,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Default {
    On,
    Off,
}

/// The Control PDUs whose fields can be masked, each named for its sender.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Pdu {
    SetupRequest,
    SetupResponse,
    ActivationRequest,
    ActivationResponse,
    NullRequest,
}

impl Pdu {
    fn name(self) -> &'static str {
        match self {
            Pdu::SetupRequest => "setup-request",
            Pdu::SetupResponse => "setup-response",
            Pdu::ActivationRequest => "activation-request",
            Pdu::ActivationResponse => "activation-response",
            Pdu::NullRequest => "null-request",
        }
    }

    /// The udpstp-wire struct that decodes it.
    fn wire_struct(self) -> &'static str {
        match self {
            Pdu::SetupRequest | Pdu::SetupResponse => "SetupPdu",
            Pdu::ActivationRequest | Pdu::ActivationResponse => "ActivationPdu",
            Pdu::NullRequest => "NullPdu",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mask {
    pub pdu: Pdu,
    /// udpstp-wire field names; a nested one is dotted,
    /// `sending_rate.udp_payload1`.
    pub fields: Vec<String>,
    /// Conditions from `CONDITIONS`, all of which must hold; none means
    /// every run.
    #[serde(default)]
    pub when: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub id: String,
    pub kind: Kind,
    pub deviation: Option<DeviationKind>,
    pub summary: String,
    /// Where the Requirement or the ambiguity is, such as "RFC 9946 §8.1".
    pub rfc: Option<String>,
    /// Requirement IDs the record is about, where they exist yet.
    #[serde(default)]
    pub requirements: Vec<String>,
    /// What OB-UDPST does instead, when it differs.
    pub obudpst: Option<String>,
    pub default: Option<Default>,
    /// The CLI option or TOML setting that controls it.
    pub setting: Option<String>,
    /// What would make it worth revisiting.
    pub revisit: Option<String>,
    /// ADRs (`ADR 0002`, checked), tickets and research notes.
    #[serde(default)]
    pub reference: Vec<String>,
    /// Proving tests, `path/to/file.rs::test_name`.
    #[serde(default)]
    pub tests: Vec<String>,
    #[serde(default)]
    pub masks: Vec<Mask>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    #[serde(default)]
    record: Vec<Record>,
}

/// One records file (one area of behaviour).
pub struct RecordsFile {
    pub name: String,
    pub records: Vec<Record>,
}

impl RecordsFile {
    pub fn parse(name: &str, text: &str) -> Result<Self, String> {
        let raw: RawFile = toml::from_str(text).map_err(|e| format!("{name}: {e}"))?;
        Ok(Self {
            name: name.to_string(),
            records: raw.record,
        })
    }
}

/// The public fields of udpstp-wire's structs, by struct name, with each
/// field's type name.
#[derive(Default)]
pub struct WireStructs(BTreeMap<String, Vec<(String, String)>>);

impl WireStructs {
    /// Reads `pub struct Name {` blocks and their `pub field: Type,` lines
    /// from rustfmt-formatted source.
    pub fn scan(texts: &[&str]) -> Self {
        let mut structs = BTreeMap::new();
        for text in texts {
            let mut current: Option<(String, Vec<(String, String)>)> = None;
            for line in text.lines() {
                if let Some(name) = line
                    .strip_prefix("pub struct ")
                    .and_then(|rest| rest.strip_suffix(" {"))
                {
                    current = Some((name.to_string(), Vec::new()));
                } else if line == "}" {
                    if let Some((name, fields)) = current.take() {
                        structs.insert(name, fields);
                    }
                } else if let Some((_, fields)) = current.as_mut()
                    && let Some((field, ty)) = line
                        .trim()
                        .strip_prefix("pub ")
                        .and_then(|rest| rest.strip_suffix(','))
                        .and_then(|rest| rest.split_once(": "))
                {
                    fields.push((field.to_string(), ty.to_string()));
                }
            }
        }
        Self(structs)
    }

    /// Whether a dotted field path names a field of the struct.
    fn has(&self, wire_struct: &str, path: &str) -> bool {
        let mut current = wire_struct.to_string();
        for (i, part) in path.split('.').enumerate() {
            let Some(fields) = self.0.get(&current) else {
                return false;
            };
            let Some((_, ty)) = fields.iter().find(|(name, _)| name == part) else {
                return false;
            };
            if i + 1 < path.split('.').count() {
                current = ty.clone();
            }
        }
        true
    }
}

/// How a record's proving test was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestStatus {
    /// A test with no `#[ignore]` or `#[cfg]`.
    Runs,
    /// A test compiled only under this `cfg`.
    Cfg(String),
    Ignored,
    /// A function that isn't a test.
    NotATest,
    Missing,
}

impl std::fmt::Display for TestStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TestStatus::Runs => write!(f, "runs"),
            TestStatus::Cfg(cfg) => write!(f, "runs only with cfg({cfg})"),
            TestStatus::Ignored => write!(f, "ignored"),
            TestStatus::NotATest => write!(f, "not a test"),
            TestStatus::Missing => write!(f, "missing"),
        }
    }
}

/// A record problem that fails the check.
#[derive(Debug)]
pub enum RecordProblem {
    DuplicateId {
        id: String,
    },
    SummaryMissing {
        id: String,
    },
    DeviationKindMissing {
        id: String,
    },
    DeviationKindOnOther {
        id: String,
    },
    DefaultMissing {
        id: String,
    },
    RevisitMissing {
        id: String,
    },
    RfcMissing {
        id: String,
    },
    UnknownRequirement {
        id: String,
        requirement: String,
    },
    UnknownAdr {
        id: String,
        reference: String,
    },
    BadTest {
        id: String,
        test: String,
        status: TestStatus,
    },
    MaskWithoutObudpst {
        id: String,
    },
    UnknownField {
        id: String,
        pdu: &'static str,
        field: String,
    },
    UnknownCondition {
        id: String,
        condition: String,
    },
    UnknownSetting {
        id: String,
        setting: String,
    },
    MalformedNote {
        file: String,
        line: String,
    },
    UnknownNote {
        file: String,
        id: String,
    },
    RequirementNotMarked {
        id: String,
        requirement: String,
    },
    RequirementReferenceMismatch {
        requirement: String,
        reference: String,
    },
}

impl std::fmt::Display for RecordProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use RecordProblem::*;
        match self {
            DuplicateId { id } => write!(f, "record {id}: duplicate ID"),
            SummaryMissing { id } => write!(f, "record {id}: the summary is empty"),
            DeviationKindMissing { id } => write!(
                f,
                "record {id}: a Deviation needs `deviation` (non-conformance, interop-accommodation, platform-gap or unmet-should)"
            ),
            DeviationKindOnOther { id } => {
                write!(f, "record {id}: only a Deviation has `deviation`")
            }
            DefaultMissing { id } => write!(
                f,
                "record {id}: a Deviation, or a record with a setting, needs `default`"
            ),
            RevisitMissing { id } => write!(f, "record {id}: a Deviation needs `revisit`"),
            RfcMissing { id } => {
                write!(f, "record {id}: a Deviation or Interpretation needs `rfc`")
            }
            UnknownRequirement { id, requirement } => {
                write!(f, "record {id}: names unknown requirement {requirement}")
            }
            UnknownAdr { id, reference } => {
                write!(
                    f,
                    "record {id}: reference {reference} names no existing ADR"
                )
            }
            BadTest { id, test, status } => write!(f, "record {id}: test {test} is {status}"),
            MaskWithoutObudpst { id } => write!(
                f,
                "record {id}: an OB-UDPST Difference, or a record with masks, needs `obudpst`"
            ),
            UnknownField { id, pdu, field } => {
                write!(f, "record {id}: mask names unknown {pdu} field {field}")
            }
            UnknownCondition { id, condition } => write!(
                f,
                "record {id}: unknown mask condition {condition} (see CONDITIONS in xtask/src/records.rs)"
            ),
            UnknownSetting { id, setting } => write!(
                f,
                "record {id}: setting {setting} is not in crates/udpstp/src"
            ),
            MalformedNote { file, line } => write!(
                f,
                "{file}: a `(conformance: <id>, …)` note must close on its line: {line}"
            ),
            UnknownNote { file, id } => {
                write!(f, "{file}: `conformance: {id}` names no record")
            }
            RequirementNotMarked { id, requirement } => write!(
                f,
                "record {id}: Deviation from {requirement}, whose state isn't deviation with \"{id}\" in its reference"
            ),
            RequirementReferenceMismatch {
                requirement,
                reference,
            } => write!(
                f,
                "{requirement}: reference {reference} is a record of another kind, or one that doesn't list it"
            ),
        }
    }
}

/// A record's line in the report.
pub struct RecordStatus {
    pub file: String,
    pub id: String,
    pub kind: Kind,
    pub deviation: Option<DeviationKind>,
    pub obudpst: bool,
    pub tests: Vec<(String, TestStatus)>,
    /// A section in `rfc` that has requirement IDs, although the record
    /// links none.
    pub unlinked_section: Option<String>,
}

impl RecordStatus {
    /// Whether at least one listed test runs somewhere.
    pub fn proven(&self) -> bool {
        self.tests
            .iter()
            .any(|(_, s)| matches!(s, TestStatus::Runs | TestStatus::Cfg(_)))
    }
}

/// What a requirement says about its state, for cross-checking.
pub struct RequirementView<'a> {
    pub id: &'a str,
    pub deviation: bool,
    pub interpretation: bool,
    /// Its `reference` entries: ADRs or Conformance Record IDs.
    pub references: Vec<&'a str>,
}

pub struct Inputs<'a> {
    pub files: &'a [RecordsFile],
    pub requirements: &'a [RequirementView<'a>],
    pub sources: &'a [SourceFile],
    pub adrs: &'a [&'a str],
    pub wire: &'a WireStructs,
    /// The text of the `udpstp` binary's sources, where settings live.
    pub settings_text: &'a str,
}

pub struct Checked {
    pub records: Vec<RecordStatus>,
    pub problems: Vec<RecordProblem>,
    /// (PDU, field, conditions) → the records that mask it.
    pub masks: BTreeMap<(Pdu, String, Vec<String>), Vec<String>>,
}

pub fn check(inputs: &Inputs<'_>) -> Checked {
    let mut problems = Vec::new();
    let mut statuses = Vec::new();
    let mut masks: BTreeMap<(Pdu, String, Vec<String>), Vec<String>> = BTreeMap::new();
    let mut ids: HashSet<&str> = HashSet::new();
    let requirement_ids: HashSet<&str> = inputs.requirements.iter().map(|r| r.id).collect();
    let by_id: BTreeMap<&str, &Record> = inputs
        .files
        .iter()
        .flat_map(|f| f.records.iter())
        .map(|r| (r.id.as_str(), r))
        .collect();
    for file in inputs.files {
        for record in &file.records {
            let id = record.id.clone();
            if !ids.insert(record.id.as_str()) {
                problems.push(RecordProblem::DuplicateId { id: id.clone() });
            }
            if record.summary.trim().is_empty() {
                problems.push(RecordProblem::SummaryMissing { id: id.clone() });
            }
            let deviation = record.kind == Kind::Deviation;
            match (deviation, record.deviation.is_some()) {
                (true, false) => {
                    problems.push(RecordProblem::DeviationKindMissing { id: id.clone() })
                }
                (false, true) => {
                    problems.push(RecordProblem::DeviationKindOnOther { id: id.clone() })
                }
                _ => {}
            }
            if (deviation || record.setting.is_some()) && record.default.is_none() {
                problems.push(RecordProblem::DefaultMissing { id: id.clone() });
            }
            if deviation && record.revisit.is_none() {
                problems.push(RecordProblem::RevisitMissing { id: id.clone() });
            }
            if record.kind != Kind::ObudpstDifference && record.rfc.is_none() {
                problems.push(RecordProblem::RfcMissing { id: id.clone() });
            }
            for requirement in &record.requirements {
                match inputs.requirements.iter().find(|r| r.id == requirement) {
                    None => problems.push(RecordProblem::UnknownRequirement {
                        id: id.clone(),
                        requirement: requirement.clone(),
                    }),
                    Some(r)
                        if deviation
                            && !(r.deviation && r.references.contains(&record.id.as_str())) =>
                    {
                        problems.push(RecordProblem::RequirementNotMarked {
                            id: id.clone(),
                            requirement: requirement.clone(),
                        })
                    }
                    Some(_) => {}
                }
            }
            for reference in &record.reference {
                if let Some(number) = reference.trim().strip_prefix("ADR ")
                    && !inputs.adrs.contains(&number.trim())
                {
                    problems.push(RecordProblem::UnknownAdr {
                        id: id.clone(),
                        reference: reference.clone(),
                    });
                }
            }
            if let Some(setting) = &record.setting {
                for flag in setting
                    .split(|c: char| c.is_whitespace() || c == ',' || c == '`')
                    .filter_map(|word| word.strip_prefix("--"))
                {
                    let name = flag.split('=').next().unwrap_or(flag).replace('-', "_");
                    if !inputs.settings_text.contains(&format!(" {name}: ")) {
                        problems.push(RecordProblem::UnknownSetting {
                            id: id.clone(),
                            setting: format!("--{flag}"),
                        });
                    }
                }
            }
            let tests: Vec<(String, TestStatus)> = record
                .tests
                .iter()
                .map(|test| (test.clone(), test_status(inputs.sources, test)))
                .collect();
            for (test, status) in &tests {
                if matches!(status, TestStatus::Missing | TestStatus::NotATest) {
                    problems.push(RecordProblem::BadTest {
                        id: id.clone(),
                        test: test.clone(),
                        status: status.clone(),
                    });
                }
            }
            if (!record.masks.is_empty() || record.kind == Kind::ObudpstDifference)
                && record.obudpst.is_none()
            {
                problems.push(RecordProblem::MaskWithoutObudpst { id: id.clone() });
            }
            for mask in &record.masks {
                for condition in &mask.when {
                    if !CONDITIONS.iter().any(|(name, _)| name == condition) {
                        problems.push(RecordProblem::UnknownCondition {
                            id: id.clone(),
                            condition: condition.clone(),
                        });
                    }
                }
                let mut when = mask.when.clone();
                when.sort();
                for field in &mask.fields {
                    if !inputs.wire.has(mask.pdu.wire_struct(), field) {
                        problems.push(RecordProblem::UnknownField {
                            id: id.clone(),
                            pdu: mask.pdu.name(),
                            field: field.clone(),
                        });
                    }
                    let entry = masks
                        .entry((mask.pdu, field.clone(), when.clone()))
                        .or_default();
                    if !entry.contains(&id) {
                        entry.push(id.clone());
                    }
                }
            }
            let unlinked_section = if record.requirements.is_empty() {
                record.rfc.as_deref().and_then(|rfc| {
                    sections(rfc).into_iter().find(|(prefix, _)| {
                        requirement_ids
                            .iter()
                            .any(|r| r.starts_with(prefix.as_str()))
                    })
                })
            } else {
                None
            }
            .map(|(_, label)| label);
            statuses.push(RecordStatus {
                file: file.name.clone(),
                id,
                kind: record.kind,
                deviation: record.deviation,
                obudpst: record.kind == Kind::ObudpstDifference || record.obudpst.is_some(),
                tests,
                unlinked_section,
            });
        }
    }
    for requirement in inputs.requirements {
        for reference in &requirement.references {
            let reference = *reference;
            if reference.trim().starts_with("ADR ") {
                continue;
            }
            let consistent = by_id.get(reference).is_some_and(|record| {
                let kind_matches = match record.kind {
                    Kind::Deviation => requirement.deviation,
                    Kind::Interpretation => requirement.interpretation,
                    Kind::ObudpstDifference => false,
                };
                kind_matches && record.requirements.iter().any(|r| r == requirement.id)
            });
            if !consistent {
                problems.push(RecordProblem::RequirementReferenceMismatch {
                    requirement: requirement.id.to_string(),
                    reference: reference.to_string(),
                });
            }
        }
    }
    for source in inputs.sources {
        for line in &source.malformed_notes {
            problems.push(RecordProblem::MalformedNote {
                file: source.path.clone(),
                line: line.clone(),
            });
        }
        for note in &source.notes {
            if !ids.contains(note.as_str()) {
                problems.push(RecordProblem::UnknownNote {
                    file: source.path.clone(),
                    id: note.clone(),
                });
            }
        }
    }
    Checked {
        records: statuses,
        problems,
        masks,
    }
}

/// Requirement ID prefixes, with a label, for the RFC sections an `rfc`
/// field names: "RFC 9946 §6.2.1" gives ("RFC9946-6.2.1-", …).
fn sections(rfc: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut document = None;
    for word in rfc.split(|c: char| c.is_whitespace() || c == ',' || c == ';' || c == '(') {
        if let Some(number) = word.strip_prefix("RFC") {
            if !number.is_empty() {
                document = Some(number.to_string());
            }
        } else if word.len() == 4 && word.chars().all(|c| c.is_ascii_digit()) {
            document = Some(word.to_string());
        } else if let (Some(section), Some(doc)) = (word.strip_prefix('§'), &document) {
            let section = section.trim_end_matches(['.', ')', ':']);
            if !section.is_empty() {
                out.push((
                    format!("RFC{doc}-{section}-"),
                    format!("RFC {doc} §{section}"),
                ));
            }
        }
    }
    out
}

fn test_status(sources: &[SourceFile], test: &str) -> TestStatus {
    let Some((path, name)) = test.rsplit_once("::") else {
        return TestStatus::Missing;
    };
    let Some(source) = sources.iter().find(|s| s.path == path) else {
        return TestStatus::Missing;
    };
    match source.functions.iter().find(|f| f.name == name) {
        None => TestStatus::Missing,
        Some(f) if !f.test => TestStatus::NotATest,
        Some(f) if f.ignored => TestStatus::Ignored,
        Some(f) => match &f.cfg {
            Some(cfg) => TestStatus::Cfg(cfg.clone()),
            None => TestStatus::Runs,
        },
    }
}

impl Checked {
    pub fn report(&self) -> String {
        let mut out = String::from("Conformance Records\n");
        for record in &self.records {
            let mut kind = record.kind.label().to_string();
            if let Some(deviation) = record.deviation {
                let _ = write!(kind, " ({})", deviation_label(deviation));
            }
            if record.obudpst && record.kind != Kind::ObudpstDifference {
                kind.push_str(", OB-UDPST Difference");
            }
            let _ = writeln!(out, "  {} [{}]: {kind}", record.id, record.file);
            if record.tests.is_empty() {
                out.push_str("    no proving test\n");
            }
            for (test, status) in &record.tests {
                let _ = writeln!(out, "    {test}: {status}");
            }
            if let Some(section) = &record.unlinked_section {
                let _ = writeln!(out, "    links no requirement of {section}");
            }
        }
        out
    }

    /// The records part of summary.md.
    pub fn summary(&self) -> String {
        let count = |pred: &dyn Fn(&RecordStatus) -> bool| {
            let all = self.records.iter().filter(|r| pred(r)).count();
            let unproven = self
                .records
                .iter()
                .filter(|r| pred(r) && !r.proven())
                .count();
            (all, unproven)
        };
        let mut out = String::from(
            "\n## Conformance Records\n\n\
             From [`records/`](records/). A record is unproven when none of its tests runs.\n\n\
             | Kind | Records | Unproven |\n|---|--:|--:|\n",
        );
        for kind in [Kind::Deviation] {
            let (all, unproven) = count(&|r| r.kind == kind);
            let _ = writeln!(out, "| {} | {all} | {unproven} |", kind.label());
            for sub in [
                DeviationKind::NonConformance,
                DeviationKind::InteropAccommodation,
                DeviationKind::PlatformGap,
                DeviationKind::UnmetShould,
            ] {
                let (all, unproven) = count(&|r| r.deviation == Some(sub));
                if all > 0 {
                    let _ = writeln!(
                        out,
                        "| &nbsp;&nbsp;{} | {all} | {unproven} |",
                        deviation_label(sub)
                    );
                }
            }
        }
        let (all, unproven) = count(&|r| r.kind == Kind::Interpretation);
        let _ = writeln!(out, "| Interpretation | {all} | {unproven} |");
        let (all, unproven) = count(&|r| r.kind == Kind::ObudpstDifference);
        let _ = writeln!(out, "| OB-UDPST Difference | {all} | {unproven} |");
        let (all, _) = count(&|r| r.obudpst);
        let _ = writeln!(
            out,
            "\nRecords that are, or include, an OB-UDPST Difference: {all}."
        );
        let unproven: Vec<&str> = self
            .records
            .iter()
            .filter(|r| !r.proven())
            .map(|r| r.id.as_str())
            .collect();
        if !unproven.is_empty() {
            let _ = writeln!(out, "\nUnproven: {}.", unproven.join(", "));
        }
        out
    }

    /// The generated interop mask list.
    pub fn masks_toml(&self) -> String {
        let mut out = String::from(
            "# Generated by `cargo xtask req` from the masks in docs/conformance/records/;\n\
             # don't edit it by hand. Each entry is a field of a Control PDU, named as in\n\
             # udpstp-wire, that the OB-UDPST interop comparison ignores, in runs where\n\
             # every condition in `when` holds (none: every run), because of `records`.\n",
        );
        out.push_str("\n# What each condition means.\n[conditions]\n");
        for (name, meaning) in CONDITIONS {
            let _ = writeln!(out, "{name} = \"{meaning}\"");
        }
        for ((pdu, field, when), records) in &self.masks {
            let quoted = |items: &[String]| {
                items
                    .iter()
                    .map(|i| format!("\"{i}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let _ = write!(
                out,
                "\n[[mask]]\npdu = \"{}\"\nfield = \"{field}\"\nwhen = [{}]\nrecords = [{}]\n",
                pdu.name(),
                quoted(when),
                quoted(records)
            );
        }
        out
    }
}

fn deviation_label(kind: DeviationKind) -> &'static str {
    match kind {
        DeviationKind::NonConformance => "non-conformance",
        DeviationKind::InteropAccommodation => "interop accommodation",
        DeviationKind::PlatformGap => "platform gap",
        DeviationKind::UnmetShould => "unmet SHOULD",
    }
}

#[cfg(test)]
#[path = "records_tests.rs"]
mod tests;
