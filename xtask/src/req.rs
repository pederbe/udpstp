//! `cargo xtask req`: the traceability check.
//!
//! Requirements live in TOML files under `docs/conformance/requirements/`,
//! one file per RFC section. Each requirement quotes its RFC text and stores
//! a hash of it. Whether a pending requirement is tested is never stored: it
//! is derived from `#[req("ID")]` tags on tests. The check fails on tags
//! naming unknown IDs, on text whose hash no longer matches, on text that is
//! not in the RFC, on duplicate IDs, and on deviations or interpretations
//! without a reference. A pending requirement no test covers must name the
//! ticket expected to test it, and a tagged test must run on the gating
//! platform unless the requirement is classified as tested nightly.
//! It also writes the coverage table to `docs/conformance/summary.md`, which
//! CI checks is committed up to date. The conformance records
//! (`records.rs`) are checked in the same run.

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

const REQUIREMENTS_DIR: &str = "docs/conformance/requirements";

const RECORDS_DIR: &str = "docs/conformance/records";

/// Where the wire structs that masks name are defined.
const WIRE_DIR: &str = "crates/udpstp-wire/src";

/// Where the `udpstp` binary's options and settings are defined.
const SETTINGS_DIR: &str = "crates/udpstp/src";

/// The generated interop mask list.
const MASKS_PATH: &str = "docs/conformance/interop-masks.toml";

/// RFC texts requirements are checked against, by the ID prefix they use.
const DOCUMENTS: &[(&str, &str)] = &[
    ("RFC9946", "docs/rfc/rfc9946.txt"),
    ("RFC9097", "docs/rfc/rfc9097.txt"),
];

const ADR_DIR: &str = "docs/adr";

/// The generated coverage table, linked from the README.
const SUMMARY_PATH: &str = "docs/conformance/summary.md";

/// Prints the hash `text_sha256` must hold for text read from stdin.
pub fn print_hash() -> Result<(), String> {
    let mut text = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)
        .map_err(|e| format!("reading stdin: {e}"))?;
    println!("{}", text_hash(&text));
    Ok(())
}

pub fn run() -> Result<(), String> {
    let root = crate::workspace_root();
    let loaded = load(&root)?;
    let (checked, records) = (&loaded.checked, &loaded.records);
    print!("{}{}", checked.report(), records.report());
    let problems: Vec<String> = checked
        .problems
        .iter()
        .map(ToString::to_string)
        .chain(records.problems.iter().map(ToString::to_string))
        .collect();
    if !records.problems.is_empty() {
        println!("Record problems: {}", records.problems.len());
        for problem in &records.problems {
            println!("  {problem}");
        }
    }
    let summary = checked.summary() + &records.summary();
    write_generated(&root, SUMMARY_PATH, &summary)?;
    write_generated(&root, MASKS_PATH, &records.masks_toml())?;
    if problems.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "traceability check failed with {} problem(s)",
            problems.len()
        ))
    }
}

/// Everything the traceability check reads, and its results: shared by
/// `cargo xtask req` and the website's conformance pages (`site.rs`).
pub struct Loaded {
    pub files: Vec<RequirementsFile>,
    pub record_files: Vec<crate::records::RecordsFile>,
    pub sources: Vec<SourceFile>,
    pub checked: Checked,
    pub records: crate::records::Checked,
}

/// Reads the requirements, records and test sources, and checks them.
pub fn load(root: &Path) -> Result<Loaded, String> {
    let files = read_requirement_files(&root.join(REQUIREMENTS_DIR))?;
    let record_files =
        read_toml_files(&root.join(RECORDS_DIR), crate::records::RecordsFile::parse)?;
    let mut texts: Vec<(&str, String)> = Vec::new();
    for (name, path) in DOCUMENTS {
        let text =
            std::fs::read_to_string(root.join(path)).map_err(|e| format!("reading {path}: {e}"))?;
        texts.push((name, text));
    }
    let rfcs: Vec<(&str, &str)> = texts.iter().map(|(n, t)| (*n, t.as_str())).collect();
    let adrs = adr_numbers(&root.join(ADR_DIR))?;
    let adrs: Vec<&str> = adrs.iter().map(String::as_str).collect();
    let mut sources = Vec::new();
    for path in rust_files(&root.join("crates"))? {
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("reading {}: {e}", path.display()))?;
        let relative = path.strip_prefix(root).unwrap_or(&path);
        sources.push(SourceFile::scan(
            &relative.to_string_lossy().replace('\\', "/"),
            &text,
        ));
    }
    let checked = check(&files, &rfcs, &sources, &adrs);
    let wire_texts = rust_files(&root.join(WIRE_DIR))?
        .iter()
        .map(|p| std::fs::read_to_string(p).map_err(|e| format!("reading {}: {e}", p.display())))
        .collect::<Result<Vec<_>, _>>()?;
    let wire_texts: Vec<&str> = wire_texts.iter().map(String::as_str).collect();
    let wire = crate::records::WireStructs::scan(&wire_texts);
    let settings_text = rust_files(&root.join(SETTINGS_DIR))?
        .iter()
        .map(|p| std::fs::read_to_string(p).map_err(|e| format!("reading {}: {e}", p.display())))
        .collect::<Result<Vec<_>, _>>()?
        .join(
            "
",
        );
    let views: Vec<crate::records::RequirementView<'_>> =
        files.iter().flat_map(RequirementsFile::views).collect();
    let records = crate::records::check(&crate::records::Inputs {
        files: &record_files,
        requirements: &views,
        sources: &sources,
        adrs: &adrs,
        wire: &wire,
        settings_text: &settings_text,
    });
    drop(views);
    Ok(Loaded {
        files,
        record_files,
        sources,
        checked,
        records,
    })
}

/// Writes a generated file if its content changed.
fn write_generated(root: &Path, relative: &str, content: &str) -> Result<(), String> {
    let path = root.join(relative);
    if std::fs::read_to_string(&path).ok().as_deref() != Some(content) {
        std::fs::write(&path, content).map_err(|e| format!("writing {relative}: {e}"))?;
        println!("Updated {relative}");
    }
    Ok(())
}

fn read_requirement_files(dir: &Path) -> Result<Vec<RequirementsFile>, String> {
    read_toml_files(dir, RequirementsFile::parse)
}

/// Parses every `.toml` file in a directory, in name order.
fn read_toml_files<T>(
    dir: &Path,
    parse: impl Fn(&str, &str) -> Result<T, String>,
) -> Result<Vec<T>, String> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("reading {}: {e}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "toml"))
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|path| {
            let text = std::fs::read_to_string(path)
                .map_err(|e| format!("reading {}: {e}", path.display()))?;
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            parse(&name, &text)
        })
        .collect()
}

pub fn rust_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("reading {}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    entries.sort();
    let mut out = Vec::new();
    for path in entries {
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            out.extend(rust_files(&path)?);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
    Ok(out)
}

/// The four-digit numbers of the ADRs in `docs/adr/`.
fn adr_numbers(dir: &Path) -> Result<Vec<String>, String> {
    Ok(std::fs::read_dir(dir)
        .map_err(|e| format!("reading {}: {e}", dir.display()))?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let number = name.get(..4)?;
            number
                .chars()
                .all(|c| c.is_ascii_digit())
                .then(|| number.to_string())
        })
        .collect())
}

/// How a requirement is classified in its file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum State {
    Pending,
    Deviation,
    Interpretation,
    Operational,
    NotApplicable,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Requirement {
    pub(crate) id: String,
    pub(crate) text: String,
    pub(crate) text_sha256: String,
    pub(crate) state: State,
    /// The ADRs or Conformance Records a deviation or interpretation rests
    /// on: one, or a list.
    #[serde(default)]
    pub(crate) reference: References,
    /// The tickets expected to test a pending requirement: one, or a list.
    #[serde(default)]
    pub(crate) ticket: Tickets,
    /// Proved only by tests that run nightly, not on the gating platform.
    #[serde(default)]
    pub(crate) nightly: bool,
    pub(crate) note: Option<String>,
}

/// A requirement's `ticket`: none, one issue number, or a list.
#[derive(Debug, Default, Deserialize)]
#[serde(untagged)]
pub(crate) enum Tickets {
    #[default]
    None,
    One(u32),
    Many(Vec<u32>),
}

impl Tickets {
    fn list(&self) -> Vec<u32> {
        match self {
            Tickets::None => Vec::new(),
            Tickets::One(one) => vec![*one],
            Tickets::Many(many) => many.clone(),
        }
    }
}

/// A requirement's `reference`: none, one, or a list.
#[derive(Debug, Default, Deserialize)]
#[serde(untagged)]
pub(crate) enum References {
    #[default]
    None,
    One(String),
    Many(Vec<String>),
}

impl References {
    pub(crate) fn iter(&self) -> impl Iterator<Item = &str> {
        let items: &[String] = match self {
            References::None => &[],
            References::One(one) => std::slice::from_ref(one),
            References::Many(many) => many,
        };
        items
            .iter()
            .map(String::as_str)
            .filter(|r| !r.trim().is_empty())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    #[serde(default)]
    requirement: Vec<Requirement>,
}

/// One requirements file (one RFC section).
pub struct RequirementsFile {
    pub(crate) name: String,
    pub(crate) requirements: Vec<Requirement>,
}

impl RequirementsFile {
    pub fn parse(name: &str, text: &str) -> Result<Self, String> {
        let raw: RawFile = toml::from_str(text).map_err(|e| format!("{name}: {e}"))?;
        Ok(Self {
            name: name.to_string(),
            requirements: raw.requirement,
        })
    }

    /// What the records check needs to know of each requirement.
    pub fn views(&self) -> impl Iterator<Item = crate::records::RequirementView<'_>> {
        self.requirements
            .iter()
            .map(|r| crate::records::RequirementView {
                id: &r.id,
                deviation: r.state == State::Deviation,
                interpretation: r.state == State::Interpretation,
                references: r.reference.iter().collect(),
            })
    }
}

/// Collapses runs of whitespace, so quoted text matches across the RFC's
/// line wrapping.
fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The first 16 hex digits of SHA-256 over the whitespace-normalized text.
pub fn text_hash(text: &str) -> String {
    let digest = Sha256::digest(normalize(text).as_bytes());
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// The requirement tags, section markers, functions and record notes
/// found in one Rust source file.
pub struct SourceFile {
    pub path: String,
    pub tags: Vec<String>,
    /// The file's own `#![cfg(...)]`, if any.
    pub cfg: Option<String>,
    pub section_markers: Vec<String>,
    pub functions: Vec<Function>,
    /// Record IDs named in `conformance: <id>, <id>` comments.
    pub notes: Vec<String>,
    /// Comment lines whose `(conformance: …` note doesn't close.
    pub malformed_notes: Vec<String>,
}

/// A function, and whether its attributes make it a test.
pub struct Function {
    pub name: String,
    pub test: bool,
    pub ignored: bool,
    /// The `cfg(...)` it is compiled under, if any.
    pub cfg: Option<String>,
    /// The requirement IDs its `#[req]` attribute names.
    pub tags: Vec<String>,
}

impl Function {
    fn new(name: &str, attributes: &[String]) -> Self {
        let head = |a: &String| {
            a.split(['(', ']', ' ', '='])
                .next()
                .unwrap_or_default()
                .trim_start_matches("#[")
                .to_string()
        };
        Self {
            name: name.to_string(),
            test: attributes.iter().any(|a| {
                let head = head(a);
                head == "test" || head.ends_with("::test")
            }),
            ignored: attributes.iter().any(|a| head(a) == "ignore"),
            cfg: attributes.iter().find_map(|a| {
                a.strip_prefix("#[cfg(")
                    .and_then(|rest| rest.strip_suffix(")]"))
                    .map(str::to_string)
            }),
            tags: attributes.iter().flat_map(|a| req_ids(a)).collect(),
        }
    }

    /// Whether it is a test that runs on the gating platform, in a file
    /// compiled under `file_cfg`: not ignored, and not compiled out there.
    pub fn runs_on_gating_platform(&self, file_cfg: Option<&str>) -> bool {
        self.test
            && !self.ignored
            && [file_cfg, self.cfg.as_deref()]
                .into_iter()
                .flatten()
                .all(cfg_holds_on_gating_platform)
    }
}

/// The attribute openers of `#[req(...)]`.
const REQ_OPENERS: [&str; 3] = ["#[req(", "#[udpstp_req::req(", "#[::udpstp_req::req("];

/// The requirement IDs a `#[req(...)]` attribute names; none for any other
/// attribute.
fn req_ids(attribute: &str) -> Vec<String> {
    REQ_OPENERS
        .iter()
        .find_map(|o| attribute.strip_prefix(o))
        .map(|args| {
            args.split('"')
                .skip(1)
                .step_by(2)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The platform whose test run gates a change: Linux, which CI always
/// runs and the pre-push hook runs in WSL.
const GATING_OS: &str = "linux";

/// Whether a `cfg` predicate holds on the gating platform, for test
/// builds with every feature a test enables. Predicates it doesn't know
/// are taken to hold.
fn cfg_holds_on_gating_platform(cfg: &str) -> bool {
    let cfg = cfg.trim();
    let (name, args) = match cfg.split_once('(') {
        Some((name, rest)) if rest.ends_with(')') => (name.trim(), &rest[..rest.len() - 1]),
        _ => (cfg, ""),
    };
    match name {
        "all" => cfg_args(args)
            .iter()
            .all(|a| cfg_holds_on_gating_platform(a)),
        "any" => cfg_args(args)
            .iter()
            .any(|a| cfg_holds_on_gating_platform(a)),
        "not" => !cfg_holds_on_gating_platform(args),
        "unix" => true,
        "windows" => false,
        _ => match cfg.split_once('=') {
            Some((key, value)) => {
                let value = value.trim().trim_matches('"');
                match key.trim() {
                    "target_os" => value == GATING_OS,
                    "target_family" => value == "unix",
                    "target_vendor" => value == "unknown",
                    _ => true,
                }
            }
            None => true,
        },
    }
}

/// The comma-separated arguments of `all(...)` or `any(...)`, split at the
/// top level.
fn cfg_args(args: &str) -> Vec<&str> {
    let (mut depth, mut start, mut out) = (0, 0, Vec::new());
    for (i, c) in args.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                out.push(&args[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&args[start..]);
    out.into_iter().filter(|a| !a.trim().is_empty()).collect()
}

/// The name of the function a line declares, if it declares one.
fn function_name(line: &str) -> Option<&str> {
    let mut rest = line;
    for prefix in ["pub(crate) ", "pub(super) ", "pub ", "const ", "async "] {
        rest = rest.strip_prefix(prefix).unwrap_or(rest);
    }
    let rest = rest.strip_prefix("fn ")?;
    let end = rest.find(['(', '<'])?;
    Some(&rest[..end])
}

/// The record IDs a comment names after `conformance:`, or `None` when
/// a note opened with `(conformance:` doesn't close on the line.
fn record_notes(comment: &str) -> Option<Vec<String>> {
    let Some((before, rest)) = comment.split_once("conformance:") else {
        return Some(Vec::new());
    };
    let rest = rest.trim_start();
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == ',' || c == ' '))
        .unwrap_or(rest.len());
    if before.ends_with('(') && !rest[end..].starts_with(')') {
        return None;
    }
    Some(
        rest[..end]
            .split(',')
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

impl SourceFile {
    /// Finds `#[req("ID", ...)]` attributes (also written
    /// `#[udpstp_req::req(...)]`, and possibly wrapped over several lines)
    /// and `// req: <section>` markers, including their continuation
    /// comment lines.
    pub fn scan(path: &str, text: &str) -> Self {
        let mut file_cfg = None;
        let mut tags = Vec::new();
        let mut section_markers: Vec<String> = Vec::new();
        let mut attribute: Option<String> = None;
        let mut in_marker = false;
        let mut functions = Vec::new();
        let mut notes = Vec::new();
        let mut malformed_notes = Vec::new();
        // Attributes seen since the last item, and one still open.
        let mut attributes: Vec<String> = Vec::new();
        let mut open_attribute: Option<String> = None;
        for line in text.lines() {
            let line = line.trim();
            if let Some(cfg) = line
                .strip_prefix("#![cfg(")
                .and_then(|rest| rest.strip_suffix(")]"))
            {
                file_cfg = Some(cfg.to_string());
                continue;
            }
            if let Some(open) = open_attribute.as_mut() {
                open.push_str(line);
            } else if line.starts_with("#[") {
                open_attribute = Some(line.to_string());
            } else if line.starts_with("//") {
                match record_notes(line) {
                    Some(ids) => notes.extend(ids),
                    None => malformed_notes.push(line.to_string()),
                }
            } else if let Some(name) = function_name(line) {
                functions.push(Function::new(name, &attributes));
                attributes.clear();
            } else if !line.is_empty() {
                attributes.clear();
            }
            if let Some(open) = open_attribute.take_if(|a| {
                a.chars().filter(|&c| c == '[').count() == a.chars().filter(|&c| c == ']').count()
            }) {
                attributes.push(open);
            }
            if let Some(open) = attribute.as_mut() {
                open.push_str(line);
            } else if let Some(args) = REQ_OPENERS.iter().find_map(|o| line.strip_prefix(o)) {
                attribute = Some(args.to_string());
            }
            if let Some(args) = attribute.as_ref().filter(|a| a.contains(")]")) {
                tags.extend(args.split('"').skip(1).step_by(2).map(str::to_string));
                attribute = None;
                in_marker = false;
                continue;
            }
            if let Some(marker) = line.strip_prefix("// req:") {
                section_markers.push(marker.trim().to_string());
                in_marker = true;
            } else if let Some(more) = line.strip_prefix("//").filter(|_| in_marker) {
                let last = section_markers.last_mut().expect("in a marker");
                last.push(' ');
                last.push_str(more.trim());
            } else {
                in_marker = false;
            }
        }
        Self {
            path: path.to_string(),
            tags,
            cfg: file_cfg,
            section_markers,
            functions,
            notes,
            malformed_notes,
        }
    }
}

/// A requirement's status in the report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Pending,
    Tested,
    /// Tested only by tests that run nightly.
    Nightly,
    Deviation,
    Interpretation,
    Operational,
    NotApplicable,
}

impl Status {
    /// How the website's conformance pages name it.
    pub fn label(self) -> &'static str {
        match self {
            Status::Pending => "Pending",
            Status::Tested => "Tested",
            Status::Nightly => "Tested nightly",
            Status::Deviation => "Deviation",
            Status::Interpretation => "Interpretation",
            Status::Operational => "Operational",
            Status::NotApplicable => "Not applicable",
        }
    }
}

#[derive(Debug)]
pub enum Problem {
    UnknownTag {
        id: String,
        file: String,
    },
    HashMismatch {
        id: String,
    },
    TextNotInRfc {
        id: String,
    },
    UnknownDocument {
        id: String,
    },
    DuplicateId {
        id: String,
    },
    MissingReference {
        id: String,
    },
    UnknownReference {
        id: String,
        reference: String,
    },
    /// Pending, with no ticket expected to test it.
    Unclassified {
        id: String,
    },
    /// Its only tests don't run on the gating platform, and it isn't
    /// classified as nightly.
    NotGating {
        id: String,
    },
    /// Classified as nightly, but tested on the gating platform, or by no
    /// test at all.
    StaleNightly {
        id: String,
    },
    /// Tested, yet still naming the tickets expected to test it.
    StaleTicket {
        id: String,
    },
    /// A `#[req]` attribute on a function that isn't a test.
    TagOnNonTest {
        id: String,
        file: String,
        function: String,
    },
    /// A BCP 14 keyword in a subsection whose section has a requirements
    /// file, outside every requirement's text.
    KeywordOutsideRequirements {
        section: String,
        keyword: String,
        context: String,
    },
    /// A requirement whose ordinal is lower than an earlier one's in the
    /// same series, by position in the subsection.
    OrdinalOutOfOrder {
        id: String,
        after: String,
    },
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Problem::UnknownTag { id, file } => {
                write!(f, "{file}: tag names unknown requirement {id}")
            }
            Problem::HashMismatch { id } => write!(
                f,
                "{id}: text_sha256 does not match the text (run `cargo xtask req hash`)"
            ),
            Problem::TextNotInRfc { id } => write!(f, "{id}: text is not in the RFC"),
            Problem::UnknownDocument { id } => {
                write!(f, "{id}: no RFC text for this ID's document")
            }
            Problem::DuplicateId { id } => write!(f, "{id}: duplicate ID"),
            Problem::MissingReference { id } => {
                write!(f, "{id}: a deviation or interpretation needs a reference")
            }
            Problem::UnknownReference { id, reference } => {
                write!(f, "{id}: reference {reference} names no existing ADR")
            }
            Problem::Unclassified { id } => {
                write!(f, "{id}: pending, with no `ticket` expected to test it")
            }
            Problem::NotGating { id } => write!(
                f,
                "{id}: its only tests don't run on the gating platform (Linux); run one there, or classify it `nightly = true`"
            ),
            Problem::StaleNightly { id } => {
                write!(
                    f,
                    "{id}: `nightly = true`, but no test runs it only nightly"
                )
            }
            Problem::StaleTicket { id } => write!(f, "{id}: tested, so remove its `ticket`"),
            Problem::TagOnNonTest { id, file, function } => {
                write!(f, "{file}: {function} tags {id} but isn't a test")
            }
            Problem::KeywordOutsideRequirements {
                section,
                keyword,
                context,
            } => write!(
                f,
                "{section}: {keyword} outside every requirement's text: \"{context}\""
            ),
            Problem::OrdinalOutOfOrder { id, after } => {
                write!(f, "{id}: its text comes before {after}'s in the RFC")
            }
        }
    }
}

pub struct Checked {
    /// Per requirements file: each requirement's ID and status.
    pub(crate) files: Vec<(String, Vec<(String, Status)>)>,
    /// Pending requirements' IDs, by the ticket expected to test them.
    tickets: BTreeMap<u32, Vec<String>>,
    /// (source file, marker text) for tests tagged with section markers.
    markers: Vec<(String, String)>,
    pub problems: Vec<Problem>,
}

pub fn check(
    files: &[RequirementsFile],
    rfcs: &[(&str, &str)],
    sources: &[SourceFile],
    adrs: &[&str],
) -> Checked {
    let rfcs: BTreeMap<&str, &str> = rfcs.iter().copied().collect();
    // Requirements with a test on the gating platform, and those with a
    // test that runs elsewhere or only when asked (ignored).
    let mut gating: HashSet<&str> = HashSet::new();
    let mut elsewhere: HashSet<&str> = HashSet::new();
    let mut problems = Vec::new();
    for source in sources {
        for function in &source.functions {
            for tag in &function.tags {
                if !function.test {
                    problems.push(Problem::TagOnNonTest {
                        id: tag.clone(),
                        file: source.path.clone(),
                        function: function.name.clone(),
                    });
                } else if function.runs_on_gating_platform(source.cfg.as_deref()) {
                    gating.insert(tag);
                } else {
                    elsewhere.insert(tag);
                }
            }
        }
    }
    let mut tickets: BTreeMap<u32, Vec<String>> = BTreeMap::new();
    let mut known = HashSet::new();
    let mut out_files = Vec::new();
    for file in files {
        let mut statuses = Vec::new();
        for req in &file.requirements {
            let id = req.id.clone();
            if !known.insert(req.id.as_str()) {
                problems.push(Problem::DuplicateId { id: id.clone() });
            }
            if text_hash(&req.text) != req.text_sha256 {
                problems.push(Problem::HashMismatch { id: id.clone() });
            }
            let mut parts = req.id.split('-');
            let (document, section) = (parts.next().unwrap_or_default(), parts.next());
            match rfcs.get(document) {
                Some(rfc) => {
                    let found = section
                        .and_then(|s| subsection(rfc, s))
                        .is_some_and(|text| normalize(text).contains(&normalize(&req.text)));
                    if !found {
                        problems.push(Problem::TextNotInRfc { id: id.clone() });
                    }
                }
                None => problems.push(Problem::UnknownDocument { id: id.clone() }),
            }
            for reference in req.reference.iter() {
                if let Some(number) = reference.trim().strip_prefix("ADR ")
                    && !adrs.contains(&number.trim())
                {
                    problems.push(Problem::UnknownReference {
                        id: id.clone(),
                        reference: reference.to_string(),
                    });
                }
            }
            if matches!(req.state, State::Deviation | State::Interpretation)
                && req.reference.iter().next().is_none()
            {
                problems.push(Problem::MissingReference { id: id.clone() });
            }
            let tested = |set: &HashSet<&str>| set.contains(req.id.as_str());
            let status = match req.state {
                State::Pending if tested(&gating) => Status::Tested,
                State::Pending if tested(&elsewhere) && req.nightly => Status::Nightly,
                State::Pending if tested(&elsewhere) => {
                    problems.push(Problem::NotGating { id: id.clone() });
                    Status::Pending
                }
                State::Pending => Status::Pending,
                State::Deviation => Status::Deviation,
                State::Interpretation => Status::Interpretation,
                State::Operational => Status::Operational,
                State::NotApplicable => Status::NotApplicable,
            };
            if req.nightly && status != Status::Nightly {
                problems.push(Problem::StaleNightly { id: id.clone() });
            }
            let expected = req.ticket.list();
            match status {
                Status::Pending if expected.is_empty() => {
                    problems.push(Problem::Unclassified { id: id.clone() });
                }
                Status::Pending => {
                    for ticket in expected {
                        tickets.entry(ticket).or_default().push(id.clone());
                    }
                }
                _ if !expected.is_empty() => {
                    problems.push(Problem::StaleTicket { id: id.clone() });
                }
                _ => {}
            }
            statuses.push((id, status));
        }
        out_files.push((file.name.clone(), statuses));
    }
    problems.extend(completeness(files, &rfcs));
    let mut markers = Vec::new();
    for source in sources {
        for tag in &source.tags {
            if !known.contains(tag.as_str()) {
                problems.push(Problem::UnknownTag {
                    id: tag.clone(),
                    file: source.path.clone(),
                });
            }
        }
        for marker in &source.section_markers {
            markers.push((source.path.clone(), marker.clone()));
        }
    }
    Checked {
        files: out_files,
        tickets,
        markers,
        problems,
    }
}

/// The BCP 14 keywords, longest first so "MUST NOT" isn't read as "MUST".
const KEYWORDS: [&str; 11] = [
    "NOT RECOMMENDED",
    "MUST NOT",
    "SHALL NOT",
    "SHOULD NOT",
    "RECOMMENDED",
    "REQUIRED",
    "OPTIONAL",
    "MUST",
    "SHALL",
    "SHOULD",
    "MAY",
];

/// Whether each subsection of every section with a requirements file is
/// extracted completely: every BCP 14 keyword lies in some requirement's
/// text (code and tables aside), and each ordinal series follows the text
///. A text the RFC repeats covers every place it appears, and
/// requirements sharing one text take its places in order. The lowercase
/// (L) series isn't checked for order: one found later takes the next
/// free number.
fn completeness(files: &[RequirementsFile], rfcs: &BTreeMap<&str, &str>) -> Vec<Problem> {
    let mut problems = Vec::new();
    // The requirements by document and subsection, in file order.
    let mut by_section: BTreeMap<(&str, &str), Vec<&Requirement>> = BTreeMap::new();
    let mut tops: BTreeMap<&str, HashSet<&str>> = BTreeMap::new();
    for req in files.iter().flat_map(|f| &f.requirements) {
        let mut parts = req.id.split('-');
        let (Some(document), Some(section)) = (parts.next(), parts.next()) else {
            continue;
        };
        by_section.entry((document, section)).or_default().push(req);
        let top = section.split('.').next().unwrap_or(section);
        tops.entry(document).or_default().insert(top);
    }
    for (document, rfc) in rfcs {
        let Some(tops) = tops.get(document) else {
            continue;
        };
        for (section, body) in subsections(rfc) {
            if !tops.contains(section.split('.').next().unwrap_or(section)) {
                continue;
            }
            let text = normalize(&prose(body));
            let reqs = by_section
                .get(&(*document, section))
                .map_or(&[][..], Vec::as_slice);
            // Where each requirement's text is: every place for coverage;
            // for its position, the places no longer requirement's text
            // contains (the RFC restates some sentences inside others),
            // the next unused one for requirements sharing a text.
            let quoted: Vec<(&Requirement, String)> = reqs
                .iter()
                .map(|r| (*r, normalize(&r.text)))
                .filter(|(_, q)| !q.is_empty())
                .collect();
            let places =
                |q: &str| -> Vec<usize> { text.match_indices(q).map(|(i, _)| i).collect() };
            let covered: Vec<(usize, usize)> = quoted
                .iter()
                .flat_map(|(_, q)| places(q).into_iter().map(|i| (i, i + q.len())))
                .collect();
            let mut positions: Vec<(&Requirement, usize)> = Vec::new();
            let mut used: BTreeMap<&str, usize> = BTreeMap::new();
            for (req, q) in &quoted {
                let own: Vec<usize> = places(q)
                    .into_iter()
                    .filter(|&i| {
                        !quoted.iter().any(|(_, other)| {
                            other.len() > q.len()
                                && places(other)
                                    .into_iter()
                                    .any(|o| o <= i && i + q.len() <= o + other.len())
                        })
                    })
                    .collect();
                let nth = used.entry(q.as_str()).or_insert(0);
                if let Some(&at) = own.get(*nth).or(own.last()) {
                    positions.push((req, at));
                }
                *nth += 1;
            }
            for (at, keyword) in keywords(&text) {
                if !covered.iter().any(|&(a, b)| a <= at && at < b) {
                    let from = text[..at]
                        .char_indices()
                        .rev()
                        .nth(60)
                        .map_or(0, |(i, _)| i);
                    let to = text[at..]
                        .char_indices()
                        .nth(keyword.len() + 40)
                        .map_or(text.len(), |(i, _)| at + i);
                    problems.push(Problem::KeywordOutsideRequirements {
                        section: format!("{document} §{section}"),
                        keyword: keyword.to_string(),
                        context: text[from..to].to_string(),
                    });
                }
            }
            // The keyword series only: a lowercase statement found later
            // takes the next free L number wherever it is.
            let mut series: Vec<(u32, usize, &str)> = positions
                .iter()
                .filter_map(|(req, at)| {
                    let ordinal = req.id.rsplit('-').next()?;
                    Some((ordinal.parse().ok()?, *at, req.id.as_str()))
                })
                .collect();
            series.sort();
            for pair in series.windows(2) {
                if pair[1].1 < pair[0].1 {
                    problems.push(Problem::OrdinalOutOfOrder {
                        id: pair[1].2.to_string(),
                        after: pair[0].2.to_string(),
                    });
                }
            }
        }
    }
    problems
}

/// Where each BCP 14 keyword starts in `text`, as whole words.
fn keywords(text: &str) -> Vec<(usize, &'static str)> {
    let mut found = Vec::new();
    let mut at = 0;
    while at < text.len() {
        let word_start = at == 0 || !text[..at].ends_with(|c: char| c.is_alphanumeric());
        let hit = KEYWORDS.iter().find(|k| {
            text[at..].starts_with(*k)
                && !text[at + k.len()..].starts_with(|c: char| c.is_alphanumeric())
        });
        match hit {
            Some(k) if word_start => {
                found.push((at, *k));
                at += k.len();
            }
            _ => at += text[at..].chars().next().map_or(1, char::len_utf8),
        }
    }
    found
}

/// A subsection's prose: its code blocks and table or figure lines left out.
fn prose(body: &str) -> String {
    let mut out = String::new();
    let mut in_code = false;
    for line in body.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("<CODE BEGINS>") {
            in_code = true;
        }
        if !in_code && !trimmed.starts_with(['|', '+']) {
            out.push_str(line);
            out.push('\n');
        }
        if trimmed.starts_with("<CODE ENDS>") {
            in_code = false;
        }
    }
    out
}

/// Every subsection of an RFC, by number: from its heading (`6.1.  …` at
/// column 0) to the next line that starts at column 0.
fn subsections(rfc: &str) -> Vec<(&str, &str)> {
    let mut out = Vec::new();
    let mut current: Option<(&str, usize)> = None;
    let mut offset = 0;
    for line in rfc.split_inclusive('\n') {
        if line.chars().next().is_some_and(|c| !c.is_whitespace()) {
            if let Some((section, begin)) = current.take() {
                out.push((section, &rfc[begin..offset]));
            }
            current = line
                .split_once(".  ")
                .map(|(number, _)| number)
                .filter(|n| {
                    !n.is_empty()
                        && n.split('.')
                            .all(|p| p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty())
                })
                .map(|n| (n, offset + line.len()));
        }
        offset += line.len();
    }
    if let Some((section, begin)) = current {
        out.push((section, &rfc[begin..]));
    }
    out
}

/// The text of one subsection, from its heading (`6.1.  …` at column 0)
/// to the next line that starts at column 0. RFC body text is indented.
fn subsection<'a>(rfc: &'a str, section: &str) -> Option<&'a str> {
    let heading = format!("{section}.  ");
    let mut offset = 0;
    let mut start = None;
    for line in rfc.split_inclusive('\n') {
        let at_column_0 = line.chars().next().is_some_and(|c| !c.is_whitespace());
        match start {
            None if line.starts_with(&heading) => start = Some(offset + line.len()),
            Some(begin) if at_column_0 => return Some(&rfc[begin..offset]),
            _ => {}
        }
        offset += line.len();
    }
    start.map(|begin| &rfc[begin..])
}

/// A requirements file's heading in the summary: `rfc9946-06.toml` is
/// "RFC 9946 §6". Other names are shown as they are.
pub(crate) fn section_label(file: &str) -> String {
    let parsed = file
        .strip_prefix("rfc")
        .and_then(|rest| rest.strip_suffix(".toml"))
        .and_then(|rest| rest.split_once('-'))
        .filter(|(rfc, _)| rfc.chars().all(|c| c.is_ascii_digit()));
    match parsed {
        Some((rfc, section)) => format!("RFC {rfc} §{}", section.trim_start_matches('0')),
        None => file.to_string(),
    }
}

impl Checked {
    /// A requirement's status, for tests.
    #[cfg(test)]
    pub fn status(&self, id: &str) -> Option<Status> {
        self.files
            .iter()
            .flat_map(|(_, statuses)| statuses.iter())
            .find(|(req, _)| req == id)
            .map(|(_, status)| *status)
    }

    /// The Markdown coverage table written to `SUMMARY_PATH`. It holds no
    /// dates or commit hashes, so it changes only when coverage does.
    pub fn summary(&self) -> String {
        const STATES: [Status; 7] = [
            Status::Tested,
            Status::Nightly,
            Status::Pending,
            Status::Deviation,
            Status::Interpretation,
            Status::Operational,
            Status::NotApplicable,
        ];
        let mut out = String::from(
            "# Conformance summary\n\n\
             Generated by `cargo xtask req` from the requirement records in \
             [`requirements/`](requirements/) and the `#[req]` tags on tests; \
             don't edit it by hand. [README.md](README.md) explains the states.\n\n\
             | Section | Requirements | Tested | Tested nightly | Pending | Deviation | Interpretation | Operational | Not applicable |\n\
             |---|--:|--:|--:|--:|--:|--:|--:|--:|\n",
        );
        let mut totals = [0usize; 7];
        for (name, statuses) in &self.files {
            let _ = write!(out, "| {} | {} |", section_label(name), statuses.len());
            for (total, state) in totals.iter_mut().zip(STATES) {
                let count = statuses.iter().filter(|(_, s)| *s == state).count();
                *total += count;
                let _ = write!(out, " {count} |");
            }
            out.push('\n');
        }
        let all: usize = self.files.iter().map(|(_, s)| s.len()).sum();
        let _ = write!(out, "| **Total** | **{all}** |");
        for total in totals {
            let _ = write!(out, " **{total}** |");
        }
        let _ = writeln!(
            out,
            "\n\nTests tagged only with a section marker, written before their requirement had an ID: {}.",
            self.markers.len()
        );
        out.push_str("\n## Pending, by the ticket expected to test them\n\n");
        if self.tickets.is_empty() {
            out.push_str("None.\n");
        }
        for (ticket, ids) in &self.tickets {
            let _ = writeln!(
                out,
                "- [#{ticket}](https://github.com/pederbe/udpstp/issues/{ticket}): {}",
                ids.join(", ")
            );
        }
        out
    }

    pub fn report(&self) -> String {
        let mut out = String::from("Requirements coverage\n");
        for (name, statuses) in &self.files {
            let count = |s: Status| statuses.iter().filter(|(_, st)| *st == s).count();
            let _ = writeln!(
                out,
                "  {name}: {} requirements: {} tested, {} tested nightly, {} pending, {} deviation, {} interpretation, {} operational, {} not applicable",
                statuses.len(),
                count(Status::Tested),
                count(Status::Nightly),
                count(Status::Pending),
                count(Status::Deviation),
                count(Status::Interpretation),
                count(Status::Operational),
                count(Status::NotApplicable),
            );
        }
        let _ = writeln!(
            out,
            "Section markers (tests written before requirement IDs existed): {}",
            self.markers.len()
        );
        for (file, marker) in &self.markers {
            let _ = writeln!(out, "  {file}: {marker}");
        }
        if self.problems.is_empty() {
            out.push_str("Problems: none\n");
        } else {
            let _ = writeln!(out, "Problems: {}", self.problems.len());
            for problem in &self.problems {
                let _ = writeln!(out, "  {problem}");
            }
        }
        out
    }
}

#[cfg(test)]
#[path = "req_tests.rs"]
mod tests;
