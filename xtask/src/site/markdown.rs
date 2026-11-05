//! Markdown handling for the site: rewriting links, rendering Mermaid
//! diagrams to figures, and a page's title and description.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

use super::{html_escape, read, write};

/// Where links to repository files go.
pub struct LinkMap<'a> {
    /// Repository path (`docs/guides/README.md`, `docs/adr` for a
    /// directory) to its page in the book (`guides/index.md`).
    pub pages: &'a BTreeMap<String, String>,
    /// The Git ref that links to other repository files pin.
    pub git_ref: &'a str,
    /// The repository root, to tell files from directories.
    pub root: &'a Path,
    /// The files Git tracks: a link to any other file would break on
    /// GitHub, so it is dropped, keeping its text, and recorded in
    /// `untracked`.
    pub tracked: &'a BTreeSet<String>,
    /// `source: target` of each link to an untracked file.
    pub untracked: RefCell<Vec<String>>,
}

/// Where one link in a page goes.
#[derive(Debug, PartialEq, Eq)]
enum Target {
    /// Another page of the book, relative to the linking page.
    Page(String),
    /// A file or directory on GitHub, at the pinned ref.
    GitHub(String),
    /// Nowhere: the link is removed and its text kept. docs/research stays
    /// off the site.
    Unlinked,
    /// Left as it is: a web link, a same-page anchor or a mail address.
    Unchanged,
}

impl LinkMap<'_> {
    fn target(&self, link: &str, source: &str, page: &str) -> Target {
        if link.contains("://") || link.starts_with('#') || link.starts_with("mailto:") {
            return Target::Unchanged;
        }
        let (path, anchor) = match link.split_once('#') {
            Some((path, anchor)) => (path, format!("#{anchor}")),
            None => (link, String::new()),
        };
        let resolved = resolve(source, path);
        if resolved.starts_with("docs/research") {
            return Target::Unlinked;
        }
        let key = resolved.trim_end_matches('/');
        if let Some(book) = self.pages.get(key) {
            return Target::Page(format!("{}{anchor}", relative_to(page, book)));
        }
        let tracked = self.tracked.contains(key)
            || self
                .tracked
                .range(format!("{key}/")..)
                .next()
                .is_some_and(|f| f.starts_with(&format!("{key}/")));
        if !tracked {
            self.untracked
                .borrow_mut()
                .push(format!("{source}: {link}"));
            return Target::Unlinked;
        }
        let kind = if self.root.join(key).is_dir() {
            "tree"
        } else {
            "blob"
        };
        Target::GitHub(format!(
            "{}/{kind}/{}/{key}{anchor}",
            super::REPO,
            self.git_ref
        ))
    }

    /// The Markdown of the page at `page` in the book, from the repository
    /// file `source`, with its relative links pointing at the book's pages
    /// or at GitHub.
    pub fn rewrite(&self, markdown: &str, source: &str, page: &str) -> String {
        rewrite_links(markdown, |link| match self.target(link, source, page) {
            Target::Page(to) | Target::GitHub(to) => Some(Some(to)),
            Target::Unlinked => Some(None),
            Target::Unchanged => None,
        })
    }
}

/// A repository path from a link in the file at `source`.
fn resolve(source: &str, link: &str) -> String {
    let mut parts: Vec<&str> = source.split('/').collect();
    parts.pop();
    for part in link.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    let joined = parts.join("/");
    if link.ends_with('/') {
        format!("{joined}/")
    } else {
        joined
    }
}

/// The relative link from the book page `from` to the book page `to`.
pub fn relative_to(from: &str, to: &str) -> String {
    let depth = from.matches('/').count();
    format!("{}{to}", "../".repeat(depth))
}

/// The prefix that leads from a page back to the book's root.
pub fn root_prefix(page: &str) -> String {
    "../".repeat(page.matches('/').count())
}

/// Rewrites each inline link outside code: `map` returns `None` to leave it,
/// `Some(Some(url))` to point it at `url`, and `Some(None)` to drop the link
/// and keep its text.
pub fn rewrite_links(markdown: &str, map: impl Fn(&str) -> Option<Option<String>>) -> String {
    let mut out = String::with_capacity(markdown.len());
    let mut in_fence = false;
    for line in markdown.split_inclusive('\n') {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            out.push_str(line);
            continue;
        }
        if in_fence {
            out.push_str(line);
            continue;
        }
        out.push_str(&rewrite_line(line, &map));
    }
    out
}

fn rewrite_line(line: &str, map: &impl Fn(&str) -> Option<Option<String>>) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    let mut in_code = false;
    while i < chars.len() {
        let c = chars[i];
        if c == '`' {
            in_code = !in_code;
            out.push(c);
            i += 1;
            continue;
        }
        if in_code || c != '[' {
            out.push(c);
            i += 1;
            continue;
        }
        // `[text](target)`: find the closing bracket, then the parenthesis.
        let Some(close) = matching(&chars, i, '[', ']') else {
            out.push(c);
            i += 1;
            continue;
        };
        if chars.get(close + 1) != Some(&'(') {
            out.push(c);
            i += 1;
            continue;
        }
        let Some(end) = matching(&chars, close + 1, '(', ')') else {
            out.push(c);
            i += 1;
            continue;
        };
        let text: String = chars[i + 1..close].iter().collect();
        let target: String = chars[close + 2..end].iter().collect();
        // The destination, and an optional title after it:
        // `[text](path "title")`.
        let target = target.trim();
        let (destination, title) = match target.split_once(char::is_whitespace) {
            Some((destination, title)) => (destination, format!(" {}", title.trim())),
            None => (target, String::new()),
        };
        let image = out.ends_with('!');
        match map(destination) {
            Some(Some(url)) if !image => {
                let _ = write!(out, "[{text}]({url}{title})");
            }
            Some(None) if !image => out.push_str(&text),
            _ => out.extend(&chars[i..=end]),
        }
        i = end + 1;
    }
    out
}

/// The index of the bracket closing the one at `open`.
fn matching(chars: &[char], open: usize, opener: char, closer: char) -> Option<usize> {
    let mut depth = 0;
    for (index, &c) in chars.iter().enumerate().skip(open) {
        if c == opener {
            depth += 1;
        } else if c == closer {
            depth -= 1;
            if depth == 0 {
                return Some(index);
            }
        }
    }
    None
}

/// The text of a page's first `# ` heading.
pub fn title(markdown: &str) -> Option<String> {
    markdown
        .lines()
        .find_map(|line| line.strip_prefix("# "))
        .map(|t| t.trim().to_string())
}

/// A page's description for search results and link previews: its first
/// paragraph of prose, as plain text, cut at about 160 characters.
pub fn description(markdown: &str) -> String {
    let mut in_fence = false;
    let mut paragraph = Vec::new();
    for line in markdown.lines().chain(std::iter::once("")) {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let prose = !(trimmed.is_empty()
            || trimmed.starts_with('#')
            || trimmed.starts_with('>')
            || trimmed.starts_with('|')
            || trimmed.starts_with('<')
            || trimmed.starts_with("- ")
            || trimmed.starts_with("* "));
        if prose {
            paragraph.push(trimmed);
        } else if !paragraph.is_empty() {
            let text = plain(&paragraph.join(" "));
            if !text.starts_with("Status:") {
                return shorten(&text, 160);
            }
            paragraph.clear();
        }
    }
    String::new()
}

/// Inline Markdown with `escape` applied to the text outside code spans
/// only: a code span shows its content literally, so an entity there would
/// show as `&lt;`. An unclosed backtick leaves the rest as text.
pub fn escape_outside_code(text: &str, escape: impl Fn(&str) -> String) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let Some(open) = rest.find('`') else {
            out.push_str(&escape(rest));
            return out;
        };
        let run = rest[open..].chars().take_while(|&c| c == '`').count();
        let fence = &rest[open..open + run];
        match rest[open + run..].find(fence) {
            Some(close) => {
                out.push_str(&escape(&rest[..open]));
                let end = open + run + close + run;
                out.push_str(&rest[open..end]);
                rest = &rest[end..];
            }
            None => {
                out.push_str(&escape(rest));
                return out;
            }
        }
    }
}

/// Markdown inline text as plain text: links reduced to their text, and
/// emphasis and code marks removed.
pub fn plain(markdown: &str) -> String {
    let unlinked = rewrite_line(markdown, &|_| Some(None));
    unlinked.replace(['*', '`'], "").replace("_Avoid_", "Avoid")
}

/// Text cut to at most `max` characters at a word boundary, with an
/// ellipsis when cut.
pub fn shorten(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out = String::new();
    for word in text.split_whitespace() {
        if out.chars().count() + word.chars().count() + 1 > max - 1 {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    format!("{}…", out.trim_end_matches([',', ';', ':', '.']))
}

/// Renders Mermaid diagrams with mermaid-cli, in a dark and a light
/// variant, and caches them by the hash of their source.
pub struct Diagrams {
    pub mmdc: PathBuf,
    /// `website/diagrams`: the two theme files and Puppeteer's options.
    pub config: PathBuf,
    pub cache: PathBuf,
    /// The rendered SVGs by file name, for copying into the site.
    pub rendered: BTreeMap<String, PathBuf>,
    /// Set by `inputs`, once.
    pub inputs: Option<String>,
}

/// A diagram rendered in both variants.
pub struct Diagram {
    /// The file name stem shared by both variants.
    pub key: String,
    pub width: u32,
    pub height: u32,
}

impl Diagrams {
    /// What a rendering depends on besides the diagram: mermaid-cli's
    /// version, its locked dependencies and the theme and Puppeteer files.
    /// Part of every cache key.
    fn inputs(&mut self) -> Result<&str, String> {
        if self.inputs.is_none() {
            let output = Command::new(&self.mmdc)
                .arg("--version")
                .output()
                .map_err(|e| format!("running {} --version: {e}", self.mmdc.display()))?;
            if !output.status.success() {
                return Err(format!("{} --version failed", self.mmdc.display()));
            }
            let mut inputs = format!("mmdc {}\n", String::from_utf8_lossy(&output.stdout).trim());
            inputs.push_str(&read(&self.config.join("../package-lock.json"))?);
            for name in ["dark.json", "light.json", "puppeteer.json"] {
                inputs.push_str(&read(&self.config.join(name))?);
            }
            self.inputs = Some(inputs);
        }
        Ok(self.inputs.as_deref().unwrap_or_default())
    }

    pub fn render(&mut self, source: &str) -> Result<Diagram, String> {
        let mut hasher = Sha256::new();
        hasher.update(self.inputs()?.as_bytes());
        hasher.update(source.as_bytes());
        let digest = hasher.finalize();
        let key: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
        let mut size = (0, 0);
        for theme in ["dark", "light"] {
            let name = format!("{key}-{theme}.svg");
            let target = self.cache.join(&name);
            if !target.is_file() {
                self.run_mmdc(source, theme, &target)?;
            }
            let svg = read(&target)?;
            size = view_box(&svg).ok_or_else(|| format!("{name}: no viewBox"))?;
            self.rendered.insert(name, target);
        }
        Ok(Diagram {
            key,
            width: size.0,
            height: size.1,
        })
    }

    fn run_mmdc(&self, source: &str, theme: &str, target: &Path) -> Result<(), String> {
        std::fs::create_dir_all(&self.cache).map_err(|e| format!("creating the cache: {e}"))?;
        let input = self.cache.join("input.mmd");
        write(&input, source)?;
        let status = Command::new(&self.mmdc)
            .arg("-q")
            .arg("-i")
            .arg(&input)
            .arg("-o")
            .arg(target)
            .args(["-b", "transparent", "-c"])
            .arg(self.config.join(format!("{theme}.json")))
            .arg("-p")
            .arg(self.config.join("puppeteer.json"))
            .status()
            .map_err(|e| format!("running {}: {e}", self.mmdc.display()))?;
        if !status.success() {
            return Err(format!("mermaid-cli failed ({status}) on:\n{source}"));
        }
        // Mermaid sizes the SVG to its container (width="100%"); an <img>
        // needs the drawing's own size, from the viewBox.
        let svg = read(target)?;
        let (width, height) = view_box(&svg).ok_or("mermaid-cli wrote an SVG without viewBox")?;
        let sized = svg.replacen(
            "width=\"100%\"",
            &format!("width=\"{width}\" height=\"{height}\""),
            1,
        );
        write(target, sized)
    }
}

/// The width and height of an SVG's viewBox, rounded up.
fn view_box(svg: &str) -> Option<(u32, u32)> {
    let start = svg.find("viewBox=\"")? + "viewBox=\"".len();
    let end = start + svg[start..].find('"')?;
    let numbers: Vec<f64> = svg[start..end]
        .split([' ', ','])
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect();
    match numbers[..] {
        [_, _, w, h] => Some((w.ceil() as u32, h.ceil() as u32)),
        _ => None,
    }
}

/// A Mermaid block and the description that must follow it.
pub struct MermaidBlock {
    pub source: String,
    pub description: String,
}

/// Replaces every ```mermaid block, and the `<!-- description: … -->`
/// comment after it, with what `figure` returns.
pub fn replace_mermaid(
    markdown: &str,
    page: &str,
    mut figure: impl FnMut(&MermaidBlock) -> Result<String, String>,
) -> Result<String, String> {
    let lines: Vec<&str> = markdown.lines().collect();
    let mut out = String::with_capacity(markdown.len());
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim() != "```mermaid" {
            out.push_str(lines[i]);
            out.push('\n');
            i += 1;
            continue;
        }
        let mut source = String::new();
        i += 1;
        while i < lines.len() && lines[i].trim() != "```" {
            source.push_str(lines[i]);
            source.push('\n');
            i += 1;
        }
        i += 1;
        while i < lines.len() && lines[i].trim().is_empty() {
            i += 1;
        }
        let description = lines
            .get(i)
            .and_then(|l| l.trim().strip_prefix("<!-- description:"))
            .and_then(|l| l.strip_suffix("-->"))
            .map(|d| d.trim().to_string())
            .ok_or_else(|| {
                format!("{page}: a Mermaid diagram needs a <!-- description: … --> line after it")
            })?;
        i += 1;
        out.push_str(&figure(&MermaidBlock {
            source,
            description,
        })?);
        out.push_str("\n\n");
    }
    Ok(out)
}

/// A diagram's figure: both variants, the site's theme showing one, and its
/// description as the text alternative in the caption.
pub fn figure_html(diagram: &Diagram, root: &str, description: &str) -> String {
    let summary = description.split('.').next().unwrap_or(description);
    let img = |theme: &str| {
        format!(
            "<img class=\"diagram-{theme}\" src=\"{root}diagrams/{key}-{theme}.svg\" alt=\"\" \
             width=\"{w}\" height=\"{h}\" loading=\"lazy\">",
            key = diagram.key,
            w = diagram.width,
            h = diagram.height
        )
    };
    format!(
        "<figure class=\"diagram\"><div class=\"diagram-scroll\" tabindex=\"0\" \
         role=\"region\" aria-label=\"Diagram: {}\">{}{}\
         </div><figcaption>{}</figcaption></figure>",
        html_escape(summary),
        img("dark"),
        img("light"),
        html_escape(description)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map<'a>(pages: &'a BTreeMap<String, String>, tracked: &'a BTreeSet<String>) -> LinkMap<'a> {
        LinkMap {
            pages,
            git_ref: "v0.1.0",
            root: Path::new("."),
            tracked,
            untracked: RefCell::new(Vec::new()),
        }
    }

    #[test]
    fn links_to_pages_become_relative_book_links() {
        let pages = BTreeMap::from([
            ("GLOSSARY.md".to_string(), "glossary.md".to_string()),
            (
                "docs/guides/running-a-test.md".to_string(),
                "guides/running-a-test.md".to_string(),
            ),
        ]);
        let md = "See [terms](../../GLOSSARY.md) and [a Test](running-a-test.md#run-a-test).\n";
        let tracked = BTreeSet::new();
        assert_eq!(
            map(&pages, &tracked).rewrite(
                md,
                "docs/guides/hosting-a-server.md",
                "guides/hosting-a-server.md"
            ),
            "See [terms](../glossary.md) and [a Test](../guides/running-a-test.md#run-a-test).\n"
        );
    }

    #[test]
    fn other_files_go_to_github_and_research_is_unlinked() {
        let pages = BTreeMap::new();
        let tracked = BTreeSet::from(["contrib/systemd/x.service".to_string()]);
        let links = map(&pages, &tracked);
        let md = "[unit](../../contrib/systemd/x.service \"The unit\") and [note](../research/a.md) \
                  and [new](../new.md) and `[c](d.md)`\n";
        assert_eq!(
            links.rewrite(md, "docs/guides/a.md", "guides/a.md"),
            "[unit](https://github.com/pederbe/udpstp/blob/v0.1.0/contrib/systemd/x.service \"The unit\") \
             and note and new and `[c](d.md)`\n"
        );
        assert_eq!(*links.untracked.borrow(), ["docs/guides/a.md: ../new.md"]);
    }

    #[test]
    fn escapes_only_outside_code_spans() {
        let escape = |t: &str| t.replace('<', "&lt;");
        assert_eq!(
            escape_outside_code("a <b> `<c>` ``<d `e`>`` <f", escape),
            "a &lt;b> `<c>` ``<d `e`>`` &lt;f"
        );
        assert_eq!(
            escape_outside_code("x `<unclosed", escape),
            "x `&lt;unclosed"
        );
    }

    #[test]
    fn descriptions_are_the_first_paragraph_as_plain_text() {
        let md =
            "# Title\n\nFor **Client** users: see [the guide](x.md) and `code`.\nMore.\n\nNext.\n";
        assert_eq!(
            description(md),
            "For Client users: see the guide and code. More."
        );
        assert!(description(&format!("# T\n\n{}\n", "word ".repeat(80))).ends_with('…'));
        assert_eq!(
            description(
                "# Decision\n\n**Status: Accepted** (2026-10-01).\nAmended later.\n\nThe Server keeps the assigned rate.\n"
            ),
            "The Server keeps the assigned rate."
        );
        assert_eq!(
            description("# Decision\n\nFinal paragraph without newline."),
            "Final paragraph without newline."
        );
    }

    #[test]
    fn mermaid_blocks_need_a_description() {
        let md = "a\n```mermaid\ngraph LR\n```\n\nb\n";
        assert!(replace_mermaid(md, "p", |_| Ok(String::new())).is_err());
        let md = "a\n```mermaid\ngraph LR\n```\n<!-- description: A to B. -->\nb\n";
        let out = replace_mermaid(md, "p", |block| Ok(format!("[{}]", block.description))).unwrap();
        assert_eq!(out, "a\n[A to B.]\n\nb\n");
    }

    #[test]
    fn view_boxes_give_the_size() {
        assert_eq!(
            view_box("<svg viewBox=\"-8 -8 300.5 120\">"),
            Some((301, 120))
        );
    }
}
