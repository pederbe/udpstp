//! `cargo xtask glossary`: checks that the docs and the user-visible strings
//! use the glossary's terms. It reads every term of `GLOSSARY.md` and
//! its _Avoid_ list, and reports, with the term to use:
//!
//! - an avoided term, in any case, as a whole word or phrase, plural too;
//! - a term written in the wrong case, such as "Key table" for **Key
//!   Table**. Only terms of several words, each after the first
//!   capitalized, are checked, and the first word's case is free, so that
//!   "the highest IP-Layer Capacity" and a lowercase "catch-up budget" pass.
//!
//! It scans `GLOSSARY.md`, `README.md`, `CONTRIBUTING.md`, `CHANGELOG.md`,
//! `SECURITY.md`, the crates' READMEs, the site's pages (`website/pages`)
//! and the Markdown under `docs/` (not `docs/research` or `docs/rfc`); the string
//! literals of the `udpstp` binary and `udpstp-runtime`; the doc comments
//! clap turns into `--help`; the requirement notes, never their RFC text;
//! and the Conformance Records' summaries, OB-UDPST lines and revisit notes.
//! Quotations are exempt: blockquotes and text in double quotes. So are an
//! avoided term inside a glossary term ("rate table" in "Sending Rate
//! Table") and what `xtask/glossary-allow.toml` lists, each with a reason.

use std::path::{Path, PathBuf};

use serde::Deserialize;

const GLOSSARY: &str = "GLOSSARY.md";

const ALLOW_LIST: &str = "xtask/glossary-allow.toml";

/// Markdown files scanned besides those under `docs/` and
/// `WEBSITE_PAGES`: the ones users read, the crates' READMEs too, which
/// crates.io shows, and those of the bench tooling's crates.
const MARKDOWN: &[&str] = &[
    "GLOSSARY.md",
    "README.md",
    "CONTRIBUTING.md",
    "CHANGELOG.md",
    "SECURITY.md",
    "crates/udpstp/README.md",
    "crates/udpstp-core/README.md",
    "crates/udpstp-runtime/README.md",
    "crates/udpstp-wire/README.md",
    "crates/bench-rounds/README.md",
    "crates/udpstp-bench/README.md",
];

const DOCS_DIR: &str = "docs";

/// The site's own pages, beside the docs it publishes.
const WEBSITE_PAGES: &str = "website/pages";

/// Research notes and the RFCs quote other wording by design.
const SKIPPED_DOCS: &[&str] = &["docs/research", "docs/rfc"];

/// Crates whose string literals users see.
const SOURCE_DIRS: &[&str] = &["crates/udpstp/src", "crates/udpstp-runtime/src"];

/// clap derives whose items' doc comments become `--help` text.
const CLAP_DERIVES: &[&str] = &["Parser", "Args", "Subcommand", "ValueEnum"];

const REQUIREMENTS_DIR: &str = "docs/conformance/requirements";

const RECORDS_DIR: &str = "docs/conformance/records";

pub fn run() -> Result<(), String> {
    let root = crate::workspace_root();
    let glossary = Glossary::parse(&read(&root.join(GLOSSARY))?);
    let allow = AllowList::parse(&read(&root.join(ALLOW_LIST))?)?;
    let texts = texts(&root)?;
    let checked = check(&glossary, &allow, &texts);
    println!(
        "Checked {} texts for {} avoided terms ({} unchecked) and the case of {} terms",
        texts.len(),
        glossary.avoided.len(),
        allow.unchecked.len(),
        glossary.cased_terms().count(),
    );
    let mut problems: Vec<String> = checked.findings.iter().map(|f| format!("  {f}")).collect();
    problems.extend(checked.unused.iter().map(|&i| {
        let entry = &allow.allow[i];
        format!(
            "  {ALLOW_LIST}: the entry for \"{}\" in {} exempts nothing; remove it",
            entry.pattern, entry.path
        )
    }));
    problems.extend(checked.unknown_unchecked.iter().map(|term| {
        format!("  {ALLOW_LIST}: \"{term}\" is on no _Avoid_ list in {GLOSSARY}; remove it")
    }));
    if problems.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} glossary problem(s); use the glossary's term, or allow-list a true exception \
             with its reason in {ALLOW_LIST}:\n{}",
            problems.len(),
            problems.join("\n")
        ))
    }
}

fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))
}

/// The glossary's terms and their _Avoid_ lists.
#[derive(Debug)]
pub struct Glossary {
    /// Every term, as its heading writes it.
    pub terms: Vec<String>,
    pub avoided: Vec<Avoided>,
}

/// One avoided term, and the term (or terms, for a paired heading) to use.
#[derive(Debug)]
pub struct Avoided {
    pub phrase: String,
    pub canonical: String,
}

impl Glossary {
    /// Reads the `**Term**:` headings, and the `_Avoid_:` line under each.
    /// An avoided term's qualifier is dropped, "(unqualified)" or "(for the
    /// setting)" alike: where the qualified use is right, the allow-list
    /// says so, or lists the term as unchecked. Alternatives such as
    /// "Src/Dst" are checked apart. An avoided term that is itself a term,
    /// such as Deviation, is left out.
    pub fn parse(context: &str) -> Glossary {
        let mut terms = Vec::new();
        let mut avoided = Vec::new();
        let mut current = String::new();
        for line in context.lines() {
            if let Some(heading) = line.strip_prefix("**").and_then(|l| l.strip_suffix("**:")) {
                let names: Vec<String> = heading.split("** / **").map(str::to_string).collect();
                current = names.join(" / ");
                terms.extend(names);
            } else if let Some(list) = line.strip_prefix("_Avoid_:") {
                for item in top_level_items(list) {
                    let bare = item.split('(').next().unwrap_or("").trim();
                    let bare = bare.strip_suffix(" unqualified").unwrap_or(bare);
                    for phrase in bare.split('/').map(str::trim).filter(|p| !p.is_empty()) {
                        avoided.push(Avoided {
                            phrase: phrase.to_string(),
                            canonical: current.clone(),
                        });
                    }
                }
            }
        }
        avoided.retain(|a| !terms.iter().any(|t| t.eq_ignore_ascii_case(&a.phrase)));
        Glossary { terms, avoided }
    }

    /// The terms whose case is checked: several words, each after the
    /// first capitalized. "Setup exchange" and "Add-on" are not, as their
    /// lowercase words would be ambiguous in a sentence-case heading.
    pub fn cased_terms(&self) -> impl Iterator<Item = &str> {
        self.terms.iter().map(String::as_str).filter(|term| {
            let words: Vec<&str> = term.split([' ', '-']).collect();
            words.len() > 1
                && words[1..]
                    .iter()
                    .all(|w| w.starts_with(|c: char| c.is_ascii_uppercase()))
        })
    }
}

/// The comma-separated items of an _Avoid_ list, keeping commas inside
/// parentheses.
fn top_level_items(list: &str) -> Vec<&str> {
    let mut items = Vec::new();
    let (mut depth, mut start) = (0usize, 0);
    for (i, c) in list.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                items.push(list[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    items.push(list[start..].trim());
    items.retain(|i| !i.is_empty());
    items
}

/// `xtask/glossary-allow.toml`: the true exceptions, each with its reason.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AllowList {
    /// Findings exempt where their text is a name the glossary doesn't
    /// govern, such as a flag or a field.
    #[serde(default)]
    pub allow: Vec<Allow>,
    /// Avoided terms not checked anywhere, being ordinary words in senses
    /// the glossary doesn't avoid.
    #[serde(default)]
    pub unchecked: Vec<Unchecked>,
}

/// A finding is exempt in a file `path` matches (a glob: `*` within a path
/// segment, `**` across segments) when an occurrence of `pattern` in its
/// line covers it. The pattern is case-sensitive, and `*` in it stands for
/// any run of characters.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Allow {
    pub path: String,
    pub pattern: String,
    #[allow(dead_code, reason = "for the reader of the allow-list")]
    pub reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Unchecked {
    pub term: String,
    #[allow(dead_code, reason = "for the reader of the allow-list")]
    pub reason: String,
}

impl AllowList {
    pub fn parse(text: &str) -> Result<AllowList, String> {
        toml::from_str(text).map_err(|e| format!("{ALLOW_LIST}: {e}"))
    }
}

/// A text to scan: its lines are the lines of `path` from `line` on.
#[derive(Debug)]
pub struct Text {
    pub path: String,
    pub line: usize,
    pub text: String,
}

#[derive(Debug, PartialEq)]
pub struct Finding {
    pub path: String,
    pub line: usize,
    /// The text found, its whitespace collapsed.
    pub found: String,
    pub kind: Kind,
}

#[derive(Debug, PartialEq)]
pub enum Kind {
    /// On the _Avoid_ list of this term.
    Avoided(String),
    /// This term, in the wrong case.
    Case(String),
}

impl std::fmt::Display for Finding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (path, line, found) = (&self.path, self.line, &self.found);
        match &self.kind {
            Kind::Avoided(term) => write!(f, "{path}:{line}: \"{found}\": avoid it for {term}"),
            Kind::Case(term) => write!(f, "{path}:{line}: \"{found}\": write it {term}"),
        }
    }
}

#[derive(Debug, Default)]
pub struct Checked {
    pub findings: Vec<Finding>,
    /// Allow-list entries that exempted nothing, by index.
    pub unused: Vec<usize>,
    /// Unchecked terms that are on no _Avoid_ list.
    pub unknown_unchecked: Vec<String>,
}

pub fn check(glossary: &Glossary, allow: &AllowList, texts: &[Text]) -> Checked {
    let is_unchecked = |phrase: &str| {
        allow
            .unchecked
            .iter()
            .any(|u| u.term.eq_ignore_ascii_case(phrase))
    };
    let avoided: Vec<&Avoided> = glossary
        .avoided
        .iter()
        .filter(|a| !is_unchecked(&a.phrase))
        .collect();
    let cased: Vec<&str> = glossary.cased_terms().collect();
    let mut used = vec![false; allow.allow.len()];
    let mut findings = Vec::new();
    for text in texts {
        let lower = text.text.to_ascii_lowercase();
        let quoted = quotations(&text.text);
        let terms: Vec<(usize, usize)> = glossary
            .terms
            .iter()
            .flat_map(|t| occurrences(&lower, t))
            .collect();
        let mut found = Vec::new();
        for a in &avoided {
            for (start, end) in occurrences(&lower, &a.phrase) {
                if !terms.iter().any(|&(s, e)| s <= start && end <= e) {
                    found.push((start, end, Kind::Avoided(a.canonical.clone())));
                }
            }
        }
        for term in &cased {
            for (start, end) in occurrences(&lower, term) {
                let written = collapse(&text.text[start..end]);
                let head = term
                    .find([' ', '-'])
                    .expect("a cased term has several words");
                if written[head..term.len()] != term[head..] {
                    found.push((start, end, Kind::Case(term.to_string())));
                }
            }
        }
        found.sort_by_key(|&(start, end, _)| (start, end));
        for (start, end, kind) in found {
            if quoted.iter().any(|&(s, e)| s <= start && start < e) {
                continue;
            }
            let line_start = text.text[..start].rfind('\n').map_or(0, |i| i + 1);
            let line_end = text.text[start..]
                .find('\n')
                .map_or(text.text.len(), |i| start + i);
            let line = &text.text[line_start..line_end];
            let (from, to) = (start - line_start, end.min(line_end) - line_start);
            let exempt = allow.allow.iter().enumerate().find(|(_, entry)| {
                glob(&entry.path, &text.path) && covers(&entry.pattern, line, from, to)
            });
            if let Some((i, _)) = exempt {
                used[i] = true;
                continue;
            }
            findings.push(Finding {
                path: text.path.clone(),
                line: text.line + text.text[..start].matches('\n').count(),
                found: collapse(&text.text[start..end]),
                kind,
            });
        }
    }
    Checked {
        findings,
        unused: (0..used.len()).filter(|&i| !used[i]).collect(),
        unknown_unchecked: allow
            .unchecked
            .iter()
            .filter(|u| {
                !glossary
                    .avoided
                    .iter()
                    .any(|a| a.phrase.eq_ignore_ascii_case(&u.term))
            })
            .map(|u| u.term.clone())
            .collect(),
    }
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A hyphen joins a compound, so that neither a flag such as
/// `--first-mode-sub-intervals` nor "Fixed-Rate row" holds a phrase.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-'
}

/// Where `phrase` occurs in `lower` (a text in ASCII lowercase) as a whole
/// word or phrase, as byte ranges: its words may be apart by any run of
/// whitespace, line breaks included, and the last may take a plural `s` or
/// `es`. A hyphenated compound is one word, so "test port" occurs in
/// neither "test-port" nor "--test-port".
fn occurrences(lower: &str, phrase: &str) -> Vec<(usize, usize)> {
    let phrase = phrase.to_ascii_lowercase();
    let words: Vec<&str> = phrase.split_whitespace().collect();
    let Some((first, rest)) = words.split_first() else {
        return Vec::new();
    };
    let boundary = |i: usize| !lower[i..].chars().next().is_some_and(is_word_char);
    let mut out = Vec::new();
    'start: for (start, _) in lower.match_indices(first) {
        if lower[..start].chars().next_back().is_some_and(is_word_char) {
            continue;
        }
        let mut end = start + first.len();
        for word in rest {
            let gap: usize = lower[end..]
                .chars()
                .take_while(|c| c.is_whitespace())
                .map(char::len_utf8)
                .sum();
            if gap == 0 || !lower[end + gap..].starts_with(word) {
                continue 'start;
            }
            end += gap + word.len();
        }
        if let Some(plural) = ["es", "s"]
            .iter()
            .find(|s| lower[end..].starts_with(**s) && boundary(end + s.len()))
        {
            end += plural.len();
        }
        if boundary(end) {
            out.push((start, end));
        }
    }
    out
}

/// The byte ranges of quotations: text between paired double quotes,
/// straight or curly, within a line.
fn quotations(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut offset = 0;
    for line in text.split('\n') {
        let mut open = None;
        for (i, c) in line.char_indices() {
            match (c, open) {
                ('"' | '\u{201C}', None) => open = Some(i),
                ('"' | '\u{201D}', Some(start)) => {
                    out.push((offset + start, offset + i + c.len_utf8()));
                    open = None;
                }
                _ => {}
            }
        }
        offset += line.len() + 1;
    }
    out
}

/// Whether `path` matches `pattern`, a glob over `/`-separated segments.
fn glob(pattern: &str, path: &str) -> bool {
    fn segments(pattern: &[&str], path: &[&str]) -> bool {
        match pattern.split_first() {
            None => path.is_empty(),
            Some((&"**", rest)) => (0..=path.len()).any(|i| segments(rest, &path[i..])),
            Some((first, rest)) => path
                .split_first()
                .is_some_and(|(p, path)| wildcard(first, p) && segments(rest, path)),
        }
    }
    let pattern: Vec<&str> = pattern.split('/').collect();
    let path: Vec<&str> = path.split('/').collect();
    segments(&pattern, &path)
}

/// Whether all of `text` matches `pattern`, where `*` stands for any run of
/// characters.
fn wildcard(pattern: &str, text: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == text,
        Some((head, tail)) => {
            text.starts_with(head)
                && (head.len()..=text.len())
                    .any(|i| text.is_char_boundary(i) && wildcard(tail, &text[i..]))
        }
    }
}

/// Whether an occurrence of `pattern` in `line`, where `*` stands for any
/// run of characters, covers `start..end`.
fn covers(pattern: &str, line: &str, start: usize, end: usize) -> bool {
    let pieces: Vec<&str> = pattern.split('*').collect();
    let (first, last) = (pieces[0], pieces[pieces.len() - 1]);
    'candidate: for (from, _) in line.match_indices(first) {
        if from > start {
            break;
        }
        let mut pos = from + first.len();
        if pieces.len() > 1 {
            for piece in &pieces[1..pieces.len() - 1] {
                let Some(i) = line[pos..].find(piece) else {
                    continue 'candidate;
                };
                pos += i + piece.len();
            }
            // The longest occurrence: its last piece as far on as it goes.
            pos = match line[pos..].rfind(last) {
                Some(i) => pos + i + last.len(),
                None => continue 'candidate,
            };
        }
        if pos >= end {
            return true;
        }
    }
    false
}

/// Every text the check scans.
fn texts(root: &Path) -> Result<Vec<Text>, String> {
    let mut texts = Vec::new();
    let mut markdown: Vec<PathBuf> = MARKDOWN.iter().map(|m| root.join(m)).collect();
    markdown_files(root, &root.join(DOCS_DIR), &mut markdown)?;
    markdown_files(root, &root.join(WEBSITE_PAGES), &mut markdown)?;
    for path in markdown {
        texts.push(Text {
            path: relative(root, &path),
            line: 1,
            text: markdown_prose(&read(&path)?),
        });
    }
    for dir in SOURCE_DIRS {
        for path in crate::req::rust_files(&root.join(dir))? {
            texts.extend(rust_texts(&relative(root, &path), &read(&path)?));
        }
    }
    for path in toml_files(root, REQUIREMENTS_DIR)? {
        texts.extend(requirement_notes(&relative(root, &path), &read(&path)?)?);
    }
    for path in toml_files(root, RECORDS_DIR)? {
        texts.extend(record_texts(&relative(root, &path), &read(&path)?)?);
    }
    Ok(texts)
}

/// The TOML files of a directory, in name order.
fn toml_files(root: &Path, dir: &str) -> Result<Vec<PathBuf>, String> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(root.join(dir))
        .map_err(|e| format!("reading {dir}: {e}"))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "toml"))
        .collect();
    files.sort();
    Ok(files)
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn markdown_files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("reading {}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            if !SKIPPED_DOCS.contains(&relative(root, &path).as_str()) {
                markdown_files(root, &path, out)?;
            }
        } else if path.extension().is_some_and(|e| e == "md") {
            out.push(path);
        }
    }
    Ok(())
}

/// A Markdown file with its blockquotes blanked and its link targets
/// dropped, line for line.
fn markdown_prose(markdown: &str) -> String {
    let lines: Vec<String> = markdown
        .lines()
        .map(|line| {
            if line.trim_start().starts_with('>') {
                return String::new();
            }
            let mut out = String::new();
            let mut rest = line;
            while let Some(i) = rest.find("](") {
                out.push_str(&rest[..i + 2]);
                rest = &rest[i + 2..];
                rest = rest.find(')').map_or("", |end| &rest[end..]);
            }
            out.push_str(rest);
            out
        })
        .collect();
    lines.join("\n")
}

/// A Rust file's string literals, and the doc comments of its clap items,
/// consecutive lines together.
pub fn rust_texts(path: &str, source: &str) -> Vec<Text> {
    let lexed = lex(source);
    let mut texts: Vec<Text> = lexed
        .strings
        .into_iter()
        .map(|(line, text)| Text {
            path: path.to_string(),
            line,
            text,
        })
        .collect();
    let help = clap_items(source, &lexed.braces);
    let mut docs = lexed
        .docs
        .into_iter()
        .filter(|(line, _)| help.iter().any(|(from, to)| from <= line && line <= to))
        .peekable();
    while let Some((line, first)) = docs.next() {
        let mut text = first;
        let mut last = line;
        while let Some((_, next)) = docs.next_if(|(l, _)| *l == last + 1) {
            text.push('\n');
            text.push_str(&next);
            last += 1;
        }
        texts.push(Text {
            path: path.to_string(),
            line,
            text,
        });
    }
    texts
}

/// The line ranges of the items clap derives help from, each with the doc
/// comments and attributes above it.
fn clap_items(source: &str, braces: &[(usize, char)]) -> Vec<(usize, usize)> {
    let lines: Vec<&str> = source.lines().collect();
    let mut items = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let line = line.trim_start();
        let is_clap = line.starts_with("#[derive(")
            && line
                .split(|c: char| !is_word_char(c))
                .any(|word| CLAP_DERIVES.contains(&word));
        if !is_clap {
            continue;
        }
        let annotated = |line: &str| {
            let line = line.trim_start();
            line.starts_with("///") || line.starts_with("#[")
        };
        let mut first = i;
        while first > 0 && annotated(lines[first - 1]) {
            first -= 1;
        }
        // Its body: from the first brace after the derive to its match.
        let mut depth = 0;
        let body = braces.iter().skip_while(|(line, _)| *line < i + 1);
        for &(line, brace) in body {
            depth += if brace == '{' { 1 } else { -1 };
            if depth == 0 {
                items.push((first + 1, line));
                break;
            }
        }
    }
    items
}

/// What `lex` finds in a Rust file, by 1-based line.
#[derive(Debug, Default)]
struct Lexed {
    /// String literals: the line each starts on, and its content with
    /// escapes resolved enough to read (a line break kept as one).
    strings: Vec<(usize, String)>,
    /// Outer doc comments (`///`), without the marker.
    docs: Vec<(usize, String)>,
    /// Braces outside comments and literals.
    braces: Vec<(usize, char)>,
}

/// Finds a Rust file's string literals, doc comments and braces, skipping
/// comments and character literals.
fn lex(source: &str) -> Lexed {
    let bytes = source.as_bytes();
    let breaks: Vec<usize> = source.match_indices('\n').map(|(i, _)| i).collect();
    let line_at = |i: usize| breaks.partition_point(|&b| b < i) + 1;
    let mut lexed = Lexed::default();
    let mut i = 0;
    while i < bytes.len() {
        let rest = &source[i..];
        if rest.starts_with("//") {
            let end = rest.find('\n').map_or(source.len(), |n| i + n);
            let comment = &source[i..end];
            if let Some(doc) = comment.strip_prefix("///")
                && !doc.starts_with('/')
            {
                let doc = doc.strip_prefix(' ').unwrap_or(doc).trim_end();
                lexed.docs.push((line_at(i), doc.to_string()));
            }
            i = end;
        } else if rest.starts_with("/*") {
            let mut depth = 0;
            while i < bytes.len() {
                if source[i..].starts_with("/*") {
                    depth += 1;
                    i += 2;
                } else if source[i..].starts_with("*/") {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
        } else if bytes[i] == b'"' {
            let mut end = i + 1;
            while end < bytes.len() && bytes[end] != b'"' {
                end += if bytes[end] == b'\\' { 2 } else { 1 };
            }
            let end = end.min(bytes.len());
            lexed
                .strings
                .push((line_at(i), unescape(&source[i + 1..end])));
            i = end + 1;
        } else if bytes[i] == b'\'' {
            // A character literal, or else a lifetime or label.
            if rest[1..].starts_with('\\') {
                i += 3 + rest[3..].find('\'').unwrap_or(0) + 1;
            } else {
                let width = rest[1..].chars().next().map_or(0, char::len_utf8);
                i += if rest[1 + width..].starts_with('\'') {
                    2 + width
                } else {
                    1
                };
            }
        } else if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
            let word_len = rest
                .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            let after = &rest[word_len..];
            let hashes = after.len() - after.trim_start_matches('#').len();
            if matches!(&rest[..word_len], "r" | "br" | "cr") && after[hashes..].starts_with('"') {
                let open = i + word_len + hashes + 1;
                let close = format!("\"{}", "#".repeat(hashes));
                let end = source[open..]
                    .find(&close)
                    .map_or(source.len(), |n| open + n);
                lexed
                    .strings
                    .push((line_at(open), source[open..end].to_string()));
                i = (end + close.len()).min(source.len());
            } else {
                i += word_len;
            }
        } else {
            if bytes[i] == b'{' || bytes[i] == b'}' {
                lexed.braces.push((line_at(i), bytes[i] as char));
            }
            i += rest.chars().next().map_or(1, char::len_utf8);
        }
    }
    lexed
}

/// A string literal's content with its escapes resolved: quotes and
/// backslashes as themselves, other escapes as a space, and a continued
/// line as a line break, so that its lines stay the source's.
fn unescape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some(c @ ('"' | '\'' | '\\')) => out.push(c),
            Some('\n') => out.push_str(" \n"),
            Some('u') => {
                chars.by_ref().find(|&c| c == '}');
                out.push(' ');
            }
            Some('x') => {
                chars.nth(1);
                out.push(' ');
            }
            Some('\r') => {}
            _ => out.push(' '),
        }
    }
    out
}

#[derive(Deserialize)]
struct RequirementsFile {
    requirement: Vec<Requirement>,
}

#[derive(Deserialize)]
struct Requirement {
    note: Option<toml::Spanned<String>>,
}

/// A requirements file's notes. Their RFC text is never scanned.
fn requirement_notes(path: &str, source: &str) -> Result<Vec<Text>, String> {
    let file: RequirementsFile = toml::from_str(source).map_err(|e| format!("{path}: {e}"))?;
    Ok(file
        .requirement
        .into_iter()
        .filter_map(|r| r.note)
        .map(|note| toml_text(path, source, note))
        .collect())
}

#[derive(Deserialize)]
struct RecordsFile {
    #[serde(default)]
    record: Vec<RecordTexts>,
}

/// What the site shows of a Conformance Record as prose.
#[derive(Deserialize)]
struct RecordTexts {
    summary: toml::Spanned<String>,
    obudpst: Option<toml::Spanned<String>>,
    revisit: Option<toml::Spanned<String>>,
}

/// A records file's prose: each record's summary, OB-UDPST line and what
/// would make it worth revisiting.
fn record_texts(path: &str, source: &str) -> Result<Vec<Text>, String> {
    let file: RecordsFile = toml::from_str(source).map_err(|e| format!("{path}: {e}"))?;
    Ok(file
        .record
        .into_iter()
        .flat_map(|r| [Some(r.summary), r.obudpst, r.revisit])
        .flatten()
        .map(|text| toml_text(path, source, text))
        .collect())
}

/// A TOML string as a text to scan, from the line it starts on.
fn toml_text(path: &str, source: &str, text: toml::Spanned<String>) -> Text {
    Text {
        path: path.to_string(),
        line: source[..text.span().start].matches('\n').count() + 1,
        text: text.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GLOSSARY: &str = "\
**Ephemeral Port**:
The Server UDP port allocated to one Test Connection.
_Avoid_: Test port, data port

**Key Table**:
The table of Shared Keys.
_Avoid_: Key store, keyring

**Sending Rate Table**:
The Server's ordered table of rows.
_Avoid_: Rate table (unqualified), rate ladder

**Client**:
The endpoint that starts every Test.
_Avoid_: Initiator, controller

**Unauthenticated Mode**:
OB-UDPST's authMode 0.
_Avoid_: Mode 0, open mode

**Setup exchange**:
The Setup Request and the Setup Response.

**Deviation**:
A recorded departure from a Requirement.

**OB-UDPST Difference**:
Behaviour in which udpstp and OB-UDPST differ.
_Avoid_: Deviation (from OB-UDPST)
";

    fn text(path: &str, text: &str) -> Text {
        Text {
            path: path.to_string(),
            line: 1,
            text: text.to_string(),
        }
    }

    fn findings(allow: &str, texts: &[Text]) -> Vec<String> {
        let glossary = Glossary::parse(GLOSSARY);
        let allow = AllowList::parse(allow).unwrap();
        check(&glossary, &allow, texts)
            .findings
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn reads_terms_and_avoided_terms() {
        let glossary = Glossary::parse(GLOSSARY);
        assert!(glossary.terms.contains(&"Key Table".to_string()));
        let avoided: Vec<&str> = glossary.avoided.iter().map(|a| a.phrase.as_str()).collect();
        // The qualifier goes, and so does Deviation, a term of its own.
        assert_eq!(
            avoided,
            [
                "Test port",
                "data port",
                "Key store",
                "keyring",
                "Rate table",
                "rate ladder",
                "Initiator",
                "controller",
                "Mode 0",
                "open mode"
            ]
        );
        let cased: Vec<&str> = glossary.cased_terms().collect();
        assert!(cased.contains(&"OB-UDPST Difference"));
        assert!(!cased.contains(&"Setup exchange"));
    }

    #[test]
    fn flags_an_avoided_term_across_case_plural_and_line_break() {
        let found = findings(
            "",
            &[text(
                "docs/guide.md",
                "Open the Test\nports.\nA keyring. Not test-port, --test-port or protest ports.",
            )],
        );
        assert_eq!(
            found,
            [
                "docs/guide.md:1: \"Test ports\": avoid it for Ephemeral Port",
                "docs/guide.md:3: \"keyring\": avoid it for Key Table",
            ]
        );
    }

    #[test]
    fn skips_an_avoided_term_inside_a_term() {
        assert!(findings("", &[text("README.md", "Each Sending Rate Table row.")]).is_empty());
    }

    #[test]
    fn exempts_quotations() {
        let markdown =
            markdown_prose("> RFC 9946 says the test port.\n\nIt calls it \"the data port\".\n");
        assert!(findings("", &[text("docs/a.md", &markdown)]).is_empty());
        // A Rust string's escaped quotes quote too.
        let texts = rust_texts("src/main.rs", r#"let s = "the RFC's \"test port\"";"#);
        assert!(findings("", &texts).is_empty());
    }

    #[test]
    fn exempts_what_the_allow_list_covers_and_reports_unused_entries() {
        let allow = r#"
            [[allow]]
            path = "docs/**/*.md"
            pattern = "StopRole::Initiator"
            reason = "a Rust identifier"

            [[allow]]
            path = "GLOSSARY.md"
            pattern = "_Avoid_: *"
            reason = "the lists themselves"

            [[allow]]
            path = "docs/*.md"
            pattern = "--auth-mode 0"
            reason = "used nowhere"
        "#;
        let texts = [
            text(
                "docs/architecture.md",
                "The Server plays `StopRole::Initiator`.",
            ),
            text("GLOSSARY.md", "_Avoid_: Key store, keyring"),
            text("docs/x.md", "the open mode"),
            text("docs/architecture.md", "but the Initiator here"),
        ];
        let glossary = Glossary::parse(GLOSSARY);
        let allow = AllowList::parse(allow).unwrap();
        let checked = check(&glossary, &allow, &texts);
        let found: Vec<String> = checked.findings.iter().map(ToString::to_string).collect();
        assert_eq!(
            found,
            [
                "docs/x.md:1: \"open mode\": avoid it for Unauthenticated Mode",
                "docs/architecture.md:1: \"Initiator\": avoid it for Client",
            ]
        );
        assert_eq!(checked.unused, [2]);
    }

    #[test]
    fn unchecked_terms_are_skipped_and_must_be_avoided_terms() {
        let allow = r#"
            [[unchecked]]
            term = "keyring"
            reason = "an OS facility"

            [[unchecked]]
            term = "flow"
            reason = "on no list"
        "#;
        let glossary = Glossary::parse(GLOSSARY);
        let allow = AllowList::parse(allow).unwrap();
        let checked = check(&glossary, &allow, &[text("README.md", "the keyring")]);
        assert!(checked.findings.is_empty());
        assert_eq!(checked.unknown_unchecked, ["flow"]);
    }

    #[test]
    fn flags_a_term_in_the_wrong_case() {
        let found = findings(
            "",
            &[text(
                "crates/udpstp/src/keyfile.rs",
                "a TOML key table; Key tables; the Key Table; key Table; an ob-udpst Difference",
            )],
        );
        assert_eq!(
            found,
            [
                "crates/udpstp/src/keyfile.rs:1: \"key table\": write it Key Table",
                "crates/udpstp/src/keyfile.rs:1: \"Key tables\": write it Key Table",
                "crates/udpstp/src/keyfile.rs:1: \"ob-udpst Difference\": write it OB-UDPST Difference",
            ]
        );
        // Only terms whose later words are capitalized are checked.
        assert!(findings("", &[text("README.md", "the Setup Exchange")]).is_empty());
    }

    #[test]
    fn reads_string_literals_and_clap_help() {
        let source = r####"
// a "test port" in a comment
/// Not help: no clap derive.
struct Plain;
let c = '"'; let l: &'static str = r#"raw "test port""#;
/// The Client's arguments.
#[derive(Debug, Args)]
struct ClientArgs {
    /// Which data
    /// port to use.
    #[arg(long, help = "x")]
    port: u16,
}
/// After the item.
fn f() {}
"####;
        let texts = rust_texts("src/main.rs", source);
        let found: Vec<(usize, &str)> = texts.iter().map(|t| (t.line, t.text.as_str())).collect();
        assert_eq!(
            found,
            [
                (5, "raw \"test port\""),
                (11, "x"),
                (6, "The Client's arguments."),
                (9, "Which data\nport to use."),
            ]
        );
        assert_eq!(
            findings("", &texts),
            ["src/main.rs:9: \"data port\": avoid it for Ephemeral Port"]
        );
    }

    #[test]
    fn reads_the_prose_of_conformance_records() {
        let source = "\
[[record]]
id = \"a\"
kind = \"interpretation\"
summary = \"Answered in troubleshooting mode.\"
rfc = \"RFC 9946 §6.2\"
obudpst = \"OB-UDPST differs.\"
revisit = \"If mode 0 goes.\"
tests = [\"x.rs::y\"]

[[record]]
id = \"b\"
kind = \"deviation\"
summary = \"Only a summary.\"
setting = \"`--test-port`\"
";
        let path = "docs/conformance/records/x.toml";
        let texts = record_texts(path, source).unwrap();
        let found: Vec<(usize, &str)> = texts.iter().map(|t| (t.line, t.text.as_str())).collect();
        assert_eq!(
            found,
            [
                (4, "Answered in troubleshooting mode."),
                (6, "OB-UDPST differs."),
                (7, "If mode 0 goes."),
                (13, "Only a summary."),
            ]
        );
        assert_eq!(
            findings("", &texts),
            ["docs/conformance/records/x.toml:7: \"mode 0\": avoid it for Unauthenticated Mode"]
        );
    }

    #[test]
    fn globs_match_paths_by_segment() {
        assert!(glob("docs/**/*.md", "docs/a.md"));
        assert!(glob("docs/**/*.md", "docs/guides/a.md"));
        assert!(!glob("docs/*.md", "docs/guides/a.md"));
        assert!(glob("GLOSSARY.md", "GLOSSARY.md"));
        assert!(covers("_Avoid_: *", "_Avoid_: Test port", 9, 18));
        assert!(!covers("--auth-mode", "--auth-mode 0", 7, 13));
    }
}
