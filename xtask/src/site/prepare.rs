//! `cargo xtask site prepare`: the book's sources and everything served
//! beside the book.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::conformance::{self, Facts};
use super::markdown::{self, Diagrams, LinkMap};
use super::{DOCS, Paths, SITE, copy, docs_url, html_escape, read, slug, write};
use crate::req::Status;

/// One page of the book.
pub struct Page {
    /// Its path in the book, `guides/running-a-test.md`.
    pub book: String,
    /// Its entry in the chapter list.
    pub label: String,
    /// The part of the chapter list it is in; empty before the first part.
    pub part: &'static str,
    pub nested: bool,
    /// The Markdown served beside the page, and read by agents.
    pub markdown: String,
}

/// A repository page on the site: (repository path, book path, part, label,
/// or the file's heading when empty). `run` sets the order.
pub(super) const SOURCES: &[(&str, &str, &str, &str)] = &[
    // The book's front page, before the first part.
    ("website/pages/index.md", "index.md", "", ""),
    (
        "docs/guides/installing-udpstp.md",
        "guides/installing-udpstp.md",
        "Getting started",
        "Installation",
    ),
    (
        "docs/guides/building-udpstp.md",
        "guides/building-udpstp.md",
        "Getting started",
        "Building from source",
    ),
    (
        "docs/getting-started.md",
        "getting-started.md",
        "Getting started",
        "",
    ),
    (
        "docs/guides/README.md",
        "guides/index.md",
        "Guides",
        "Overview",
    ),
    (
        "docs/guides/running-a-test.md",
        "guides/running-a-test.md",
        "Guides",
        "Running a Test",
    ),
    (
        "docs/guides/hosting-a-server.md",
        "guides/hosting-a-server.md",
        "Guides",
        "",
    ),
    ("docs/guides/windows.md", "guides/windows.md", "Guides", ""),
    (
        "docs/guides/operator-features.md",
        "guides/operator-features.md",
        "Guides",
        "",
    ),
    (
        "docs/understanding-results.md",
        "understanding-results.md",
        "Explanation",
        "",
    ),
    ("docs/security.md", "security.md", "Explanation", ""),
    (
        "docs/interfaces.md",
        "reference/interfaces.md",
        "Reference",
        "",
    ),
    (
        "docs/server-log.md",
        "reference/server-log.md",
        "Reference",
        "",
    ),
    (
        "docs/json-output.md",
        "reference/json-output.md",
        "Reference",
        "",
    ),
    (
        "docs/key-files.md",
        "reference/key-files.md",
        "Reference",
        "",
    ),
    (
        "docs/platforms.md",
        "reference/platforms.md",
        "Reference",
        "",
    ),
    ("GLOSSARY.md", "glossary.md", "Reference", "Glossary"),
    ("docs/troubleshooting.md", "troubleshooting.md", "Help", ""),
    (
        "docs/conformance/README.md",
        "conformance/format.md",
        "Project",
        "How the records work",
    ),
    (
        "SECURITY.md",
        "security-policy.md",
        "Project",
        "Reporting a vulnerability",
    ),
    ("docs/architecture.md", "architecture.md", "Project", ""),
    ("docs/technology.md", "technology.md", "Project", ""),
    (
        "CONTRIBUTING.md",
        "contributing.md",
        "Project",
        "Contributing",
    ),
    ("website/pages/about.md", "about.md", "Project", ""),
];

/// Repository paths that aren't pages themselves but have one.
const ALIASES: &[(&str, &str)] = &[
    // The options' and the settings' own documentation, which the pages
    // are generated from.
    ("crates/udpstp/src/main.rs", "reference/cli.md"),
    ("crates/udpstp/src/config.rs", "reference/configuration.md"),
    ("docs/conformance/summary.md", "conformance/index.md"),
    ("docs/conformance", "conformance/index.md"),
    ("docs/guides", "guides/index.md"),
];

pub fn run(paths: &Paths) -> Result<(), String> {
    if cfg!(windows) {
        return Err(
            "cargo xtask site prepare runs on Linux or macOS (on Windows, in WSL): it runs \
             mermaid-cli and the udpstp binary with Unix paths; see docs/website.md"
                .into(),
        );
    }
    let root = &paths.root;
    let binary = PathBuf::from(
        std::env::var_os("UDPSTP_BIN")
            .ok_or("set UDPSTP_BIN to the built udpstp binary (scripts/build-site.sh does)")?,
    );
    // Absolute: it runs in a temporary directory.
    let binary = std::fs::canonicalize(&binary)
        .map_err(|e| format!("UDPSTP_BIN {}: {e}", binary.display()))?;
    let mmdc = std::env::var_os("UDPSTP_MMDC")
        .map(PathBuf::from)
        .unwrap_or_else(|| paths.website.join("node_modules/.bin/mmdc"));
    let version = super::version(root)?;
    let release = release(root, &version)?;
    let git_ref = release.git_ref.clone();
    for dir in [paths.book(), paths.extra()] {
        if dir.exists() {
            std::fs::remove_dir_all(&dir)
                .map_err(|e| format!("clearing {}: {e}", dir.display()))?;
        }
    }
    let mut diagrams = Diagrams {
        mmdc,
        config: paths.website.join("diagrams"),
        cache: paths.diagram_cache(),
        rendered: BTreeMap::new(),
        inputs: None,
    };

    // Where each repository file is published.
    let mut map: BTreeMap<String, String> = SOURCES
        .iter()
        .map(|(source, book, ..)| (source.to_string(), book.to_string()))
        .collect();
    for (source, book) in ALIASES {
        map.insert(source.to_string(), book.to_string());
    }
    // The ADRs stay in the repository: a record names one by its number
    // and links to the file there.
    let adr_links: BTreeMap<String, String> = adr_files(root)?
        .into_iter()
        .map(|(number, file)| {
            (
                number,
                format!("{}/blob/{git_ref}/docs/adr/{file}", super::REPO),
            )
        })
        .collect();
    let links = LinkMap {
        pages: &map,
        git_ref: &git_ref,
        root,
        tracked: &release.tracked,
        untracked: RefCell::new(Vec::new()),
    };

    let loaded = crate::req::load(root)?;
    let conformance = conformance::generate(&loaded, &adr_links, &git_ref)?;
    let reference = super::reference::pages(root, &binary, &conformance.record_pages)?;

    let mut pages: Vec<Page> = Vec::new();
    let from_repo = |source: &str, book: &str, part, nested, label: &str| -> Result<Page, String> {
        let text = read(&root.join(source))?;
        let text = if source == "GLOSSARY.md" {
            glossary(&text)
        } else {
            text
        };
        let markdown = links.rewrite(&text, source, book);
        let label = if label.is_empty() {
            markdown::title(&markdown).ok_or_else(|| format!("{source}: no # heading"))?
        } else {
            label.to_string()
        };
        Ok(Page {
            book: book.to_string(),
            label,
            part,
            nested,
            markdown,
        })
    };
    let generated = |book: &str, part, nested, markdown: String| Page {
        label: markdown::title(&markdown).unwrap_or_default(),
        book: book.to_string(),
        part,
        nested,
        markdown,
    };
    let repo = |source: &str, nested: bool| -> Result<Page, String> {
        let &(source, book, part, label) = SOURCES
            .iter()
            .find(|s| s.0 == source)
            .ok_or_else(|| format!("{source} isn't in SOURCES"))?;
        from_repo(source, book, part, nested, label)
    };
    let reference_page = |path: &str| -> Result<Page, String> {
        let (_, markdown) = reference
            .iter()
            .find(|(p, _)| p == path)
            .ok_or_else(|| format!("no generated page {path}"))?;
        Ok(generated(path, "Reference", false, markdown.clone()))
    };
    // In the chapter list's order.
    pages.push(repo("website/pages/index.md", false)?);
    pages.push(repo("docs/guides/installing-udpstp.md", false)?);
    pages.push(repo("docs/guides/building-udpstp.md", true)?);
    pages.push(repo("docs/getting-started.md", false)?);
    for source in [
        "docs/guides/README.md",
        "docs/guides/running-a-test.md",
        "docs/guides/hosting-a-server.md",
        "docs/guides/windows.md",
        "docs/guides/operator-features.md",
    ] {
        pages.push(repo(source, false)?);
    }
    pages.push(reference_page("reference/cli.md")?);
    pages.push(reference_page("reference/configuration.md")?);
    pages.push(reference_page("reference/exit-statuses.md")?);
    pages.push(repo("docs/json-output.md", false)?);
    pages.push(repo("docs/server-log.md", false)?);
    // The key file format, then the recorded choices from keyfile.rs.
    let mut key_files = repo("docs/key-files.md", false)?;
    key_files
        .markdown
        .push_str(&reference_page("reference/key-files.md")?.markdown);
    pages.push(key_files);
    pages.push(repo("docs/platforms.md", false)?);
    pages.push(repo("docs/interfaces.md", false)?);
    pages.push(repo("GLOSSARY.md", false)?);
    pages.push(repo("docs/understanding-results.md", false)?);
    pages.push(repo("docs/security.md", false)?);
    for (index, (path, markdown)) in conformance.pages.iter().enumerate() {
        pages.push(generated(path, "Conformance", index > 0, markdown.clone()));
    }
    pages.push(repo("docs/troubleshooting.md", false)?);
    pages.push(repo("CONTRIBUTING.md", false)?);
    pages.push(repo("docs/conformance/README.md", true)?);
    pages.push(repo("docs/architecture.md", false)?);
    pages.push(repo("docs/technology.md", true)?);
    pages.push(repo("SECURITY.md", false)?);
    pages.push(generated("citation.md", "Project", false, citation(root)?));
    pages.push(repo("website/pages/about.md", false)?);

    // The book: SUMMARY, pages with their diagrams, 404, theme and config.
    let src = paths.src();
    // Links to files Git doesn't track would break on GitHub; they were
    // dropped, keeping their text.
    let untracked = links.untracked.borrow();
    if !untracked.is_empty() {
        let list = untracked.join("\n  ");
        if release.dev {
            eprintln!(
                "warning (UDPSTP_SITE_DEV): links to untracked files, left unlinked:\n  {list}"
            );
        } else {
            return Err(format!("links to files Git doesn't track:\n  {list}"));
        }
    }
    drop(untracked);

    let mut summary = String::from("# Summary\n\n");
    let mut part = "";
    for page in &pages {
        if page.part != part {
            part = page.part;
            let _ = writeln!(summary, "\n# {part}\n");
        }
        let indent = if page.nested { "    " } else { "" };
        let bullet = if page.part.is_empty() { "" } else { "- " };
        let _ = writeln!(summary, "{indent}{bullet}[{}]({})", page.label, page.book);
    }
    write(&src.join("SUMMARY.md"), summary)?;
    let mut descriptions = String::new();
    for page in &pages {
        let root_prefix = markdown::root_prefix(&page.book);
        let book_markdown = markdown::replace_mermaid(&page.markdown, &page.book, |block| {
            let diagram = diagrams.render(&block.source)?;
            Ok(markdown::figure_html(
                &diagram,
                &root_prefix,
                &block.description,
            ))
        })?;
        write(&src.join(&page.book), book_markdown)?;
        // The Markdown copy, whose links go to the other copies.
        write(&paths.extra().join(DOCS).join(&page.book), &page.markdown)?;
        let title = markdown::title(&page.markdown).unwrap_or_else(|| page.label.clone());
        let _ = writeln!(
            descriptions,
            "{}\t{}\t{}\t{}",
            page.book,
            title.replace('\t', " "),
            page_description(page).replace('\t', " "),
            if page.part.is_empty() {
                "Docs"
            } else {
                page.part
            }
        );
    }
    write(&paths.pages_list(), descriptions)?;
    write(&src.join("404.md"), NOT_FOUND)?;
    for (name, file) in &diagrams.rendered {
        copy(file, &src.join("diagrams").join(name))?;
    }
    let theme = paths.book().join("theme");
    for name in ["head.hbs", "udpstp.css", "footer.js"] {
        copy(&paths.website.join("theme").join(name), &theme.join(name))?;
    }
    copy(
        &root.join("docs/assets/logo/udpstp-icon.svg"),
        &theme.join("favicon.svg"),
    )?;
    let rendered = paths.rendered();
    copy(&rendered.join("favicon-32.png"), &theme.join("favicon.png"))?;
    copy(
        &paths.website.join("book.toml"),
        &paths.book().join("book.toml"),
    )?;

    // Beside the book: the landing page and its files.
    let extra = paths.extra();
    let readme = read(&root.join("README.md"))?;
    let mut sequence = None;
    markdown::replace_mermaid(&insert_description(&readme), "README.md", |block| {
        sequence = Some(diagrams.render(&block.source)?);
        Ok(String::new())
    })?;
    let sequence = sequence.ok_or("README.md has no Mermaid diagram of a Test")?;
    for theme in ["dark", "light"] {
        let name = format!("{}-{theme}.svg", sequence.key);
        copy(
            &diagrams.cache.join(&name),
            &extra.join("diagrams").join(&name),
        )?;
    }
    let landing = landing(paths, &release, &conformance.facts, &sequence)?;
    write(&extra.join("index.html"), landing)?;
    for name in ["landing.css", "landing.js", "_headers", "_redirects"] {
        copy(&paths.website.join(name), &extra.join(name))?;
    }
    // The install scripts, as udpstp.com/install.sh and
    // /install.ps1, from the release tag the site is built from.
    for name in ["install.sh", "install.ps1"] {
        copy(&root.join("install").join(name), &extra.join(name))?;
    }
    for name in ["udpstp-display-bold.woff2", "OFL.txt"] {
        copy(
            &paths.website.join("fonts").join(name),
            &extra.join("fonts").join(name),
        )?;
    }
    copy(
        &root.join("docs/assets/logo/udpstp-mark-dark.svg"),
        &extra.join("logo-dark.svg"),
    )?;
    copy(
        &root.join("docs/assets/logo/udpstp-mark-light.svg"),
        &extra.join("logo-light.svg"),
    )?;
    for name in [
        "og.png",
        "apple-touch-icon.png",
        "icon-192.png",
        "icon-512.png",
    ] {
        copy(&rendered.join(name), &extra.join(name))?;
    }
    // mdBook fingerprints its own copies; the landing page and the manifest
    // need stable names.
    copy(&rendered.join("favicon-32.png"), &extra.join("favicon.png"))?;
    copy(
        &root.join("docs/assets/logo/udpstp-icon.svg"),
        &extra.join("favicon.svg"),
    )?;
    write(&extra.join("site.webmanifest"), MANIFEST)?;
    write(
        &extra.join("robots.txt"),
        format!("User-agent: *\nAllow: /\n\nSitemap: {SITE}/sitemap.xml\n"),
    )?;
    write(&extra.join(".well-known/security.txt"), security_txt())?;
    write(&extra.join("sitemap.xml"), sitemap(&pages))?;
    write(
        &extra.join("llms.txt"),
        llms_txt(&pages, &version, release.released, &conformance.facts),
    )?;
    write(&extra.join("llms-full.txt"), llms_full(&pages))?;

    println!(
        "site prepared: {} pages, {} diagrams, links pinned to {git_ref}",
        pages.len(),
        diagrams.rendered.len() / 2
    );
    if conformance.left_out_research > 0 {
        println!(
            "  left {} references to docs/research notes out of the record pages",
            conformance.left_out_research
        );
    }
    Ok(())
}

/// What the site is built from.
pub struct Release {
    /// The Git ref that links to repository files pin.
    pub git_ref: String,
    /// Whether `v<version>` is tagged: the pages then drop their
    /// pre-release wording.
    pub released: bool,
    /// A local preview (`UDPSTP_SITE_DEV=1`): a dirty tree or a missing tag
    /// is allowed, and links to untracked files are dropped with a warning
    /// rather than failing the build.
    pub dev: bool,
    /// The files Git tracks, the only ones links may point at on GitHub.
    pub tracked: BTreeSet<String>,
}

fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|e| format!("running git {}: {e}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// The ref links pin, from the release tag `v<version>`, or the ref in
/// `UDPSTP_SITE_REF`. Without either, or from a tree with uncommitted
/// changes, or with the tag elsewhere than the checked-out commit, the build
/// fails unless `UDPSTP_SITE_DEV=1` asks for a local preview.
fn release(root: &Path, version: &str) -> Result<Release, String> {
    let dev = std::env::var("UDPSTP_SITE_DEV").is_ok_and(|v| v == "1");
    let tag = format!("v{version}");
    let head = git(root, &["rev-parse", "HEAD"])?;
    let tagged = git(
        root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/tags/{tag}^{{commit}}"),
        ],
    )
    .ok();
    let chosen = std::env::var("UDPSTP_SITE_REF")
        .ok()
        .filter(|r| !r.is_empty());
    let mut problems = Vec::new();
    let (git_ref, released) = match (&tagged, chosen) {
        (_, Some(chosen)) => (chosen, tagged.is_some()),
        (Some(commit), None) => {
            if *commit != head {
                problems.push(format!("the checked-out commit isn't the tag {tag}"));
            }
            (tag.clone(), true)
        }
        (None, None) => {
            problems.push(format!(
                "the tag {tag} doesn't exist; set UDPSTP_SITE_REF to the ref links should pin"
            ));
            (head.clone(), false)
        }
    };
    let status = git(root, &["status", "--porcelain", "--untracked-files=no"])?;
    if !status.is_empty() {
        problems.push("the working tree has uncommitted changes".to_string());
    }
    if !problems.is_empty() {
        if !dev {
            return Err(format!(
                "not building the site: {}. For a local preview, set UDPSTP_SITE_DEV=1.",
                problems.join("; ")
            ));
        }
        for problem in &problems {
            eprintln!("warning (UDPSTP_SITE_DEV): {problem}");
        }
    }
    let tracked = git(root, &["ls-files"])?
        .lines()
        .map(str::to_string)
        .collect();
    Ok(Release {
        git_ref,
        released,
        dev,
        tracked,
    })
}

/// Keeps the `<!-- pre-release -->` or the `<!-- released -->` blocks of a
/// template, whichever applies, and removes the others with their markers.
fn release_blocks(page: &str, released: bool) -> Result<String, String> {
    let mut out = page.to_string();
    for (name, keep) in [("pre-release", !released), ("released", released)] {
        let (open, close) = (format!("<!-- {name} -->"), format!("<!-- /{name} -->"));
        while let Some(start) = out.find(&open) {
            let end = out[start..]
                .find(&close)
                .map(|i| start + i)
                .ok_or_else(|| format!("{open} without {close}"))?;
            let inner = out[start + open.len()..end].to_string();
            let kept = if keep { inner } else { String::new() };
            out.replace_range(start..end + close.len(), &kept);
        }
        if out.contains(&close) {
            return Err(format!("{close} without {open}"));
        }
    }
    Ok(out)
}

/// The ADRs as (number, file name), in order.
fn adr_files(root: &Path) -> Result<Vec<(String, String)>, String> {
    let mut out: Vec<(String, String)> = super::files_under(&root.join("docs/adr"))?
        .iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
        .filter(|n| {
            n.ends_with(".md")
                && n.get(..4)
                    .is_some_and(|d| d.chars().all(|c| c.is_ascii_digit()))
        })
        .map(|n| (n[..4].to_string(), n))
        .collect();
    out.sort();
    Ok(out)
}

fn citation(root: &Path) -> Result<String, String> {
    let cff = read(&root.join("CITATION.cff"))?;
    Ok(format!(
        "# Citing udpstp\n\n\
         If you use udpstp in research or a publication, please cite it. The repository's \
         `CITATION.cff` holds the citation, which GitHub also offers as APA and BibTeX:\n\n\
         ```yaml\n{}\n```\n",
        cff.trim_end()
    ))
}

/// GLOSSARY.md as the glossary page: each term a heading with its own anchor,
/// and no term linked automatically anywhere else.
fn glossary(context: &str) -> String {
    let mut out = String::from(
        "# Glossary\n\n\
         Press **Ctrl+F** (**Command+F** on macOS) to find a term on this page.\n\n\
         udpstp uses these terms the same way in its output, documentation, code and issues. \
         They come from the repository's `GLOSSARY.md`. Each term has its own anchor, so you \
         can link to it. _Avoid_ lists words udpstp doesn't use for it.\n",
    );
    let mut lines = context.lines().peekable();
    // Skip the title and the introduction up to the first group of terms.
    while let Some(line) = lines.peek() {
        if line.starts_with("### ") {
            break;
        }
        lines.next();
    }
    for line in lines {
        // The last group names the bench tooling's own vocabulary, which
        // readers of the site never meet.
        if line == "### Benchmark rounds" {
            break;
        }
        if let Some(group) = line.strip_prefix("### ") {
            let _ = writeln!(out, "\n## {group}");
        } else if let Some(terms) = line.strip_prefix("**").and_then(|l| l.strip_suffix("**:")) {
            let names: Vec<&str> = terms.split("** / **").collect();
            out.push('\n');
            for extra in names.iter().skip(1) {
                let _ = writeln!(out, "<a id=\"{}\"></a>", slug(extra));
            }
            let _ = writeln!(out, "### {} {{#{}}}\n", names.join(" / "), slug(names[0]));
        } else if let Some(avoid) = line.strip_prefix("_Avoid_:") {
            let _ = writeln!(out, "\n_Avoid:_{avoid}");
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// README.md's diagram of a Test has no description line; the landing page
/// writes its own.
fn insert_description(readme: &str) -> String {
    let mut out = String::new();
    let mut in_mermaid = false;
    for line in readme.lines() {
        out.push_str(line);
        out.push('\n');
        if line.trim() == "```mermaid" {
            in_mermaid = true;
        } else if in_mermaid && line.trim() == "```" {
            in_mermaid = false;
            out.push_str("<!-- description: landing -->\n");
        }
    }
    out
}

const NOT_FOUND: &str = "# Page not found\n\n\
The page you asked for isn't here. It may have moved when the docs were reorganised.\n\n\
- [udpstp.com](https://udpstp.com/), the home page\n\
- [Documentation](index.md)\n\
- [Quickstart](getting-started.md)\n\
- [Guides](guides/index.md)\n\
- [Troubleshooting](troubleshooting.md)\n\
- [Conformance](conformance/index.md)\n\n\
Use the search (the magnifying glass above) to find a topic.\n";

const MANIFEST: &str = r##"{
  "name": "udpstp",
  "short_name": "udpstp",
  "description": "A Rust implementation of UDPSTP (RFC 9946), which measures the Maximum IP-Layer Capacity of a network path.",
  "start_url": "/",
  "display": "browser",
  "background_color": "#121214",
  "theme_color": "#121214",
  "icons": [
    { "src": "/favicon.svg", "sizes": "any", "type": "image/svg+xml" },
    { "src": "/icon-192.png", "sizes": "192x192", "type": "image/png" },
    { "src": "/icon-512.png", "sizes": "512x512", "type": "image/png" }
  ]
}
"##;

/// security.txt (RFC 9116), pointing to SECURITY.md's contact, valid for a
/// year from the build.
fn security_txt() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let (y, m, d) = civil_date(now / 86_400 + 365);
    format!(
        "# udpstp's security contact. The policy is SECURITY.md.\n\
         Contact: {repo}/security/advisories/new\n\
         Expires: {y:04}-{m:02}-{d:02}T00:00:00Z\n\
         Preferred-Languages: en\n\
         Policy: {policy}\n\
         Canonical: {SITE}/.well-known/security.txt\n",
        repo = super::REPO,
        policy = docs_url("security-policy.md")
    )
}

/// The Gregorian (year, month, day) of a day count since 1970-01-01.
fn civil_date(days: u64) -> (i64, u32, u32) {
    // Howard Hinnant's days_from_civil, inverted.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

fn html_path(book: &str) -> String {
    book.trim_end_matches(".md").to_string() + ".html"
}

/// Search summaries of reference and navigation pages describe their contents;
/// articles take their summary from their own prose.
fn page_description(page: &Page) -> String {
    let summary = match page.book.as_str() {
        "index.md" => {
            "Documentation for udpstp: install it, run a first Test, host a Server, read the results and look up every option, field and exit status."
        }
        "getting-started.md" => {
            "Share a key, connect a Client to a Server, and measure Maximum IP-Layer Capacity with authenticated UDPSTP Tests."
        }
        "guides/index.md" => {
            "Task guides for running UDPSTP Tests, reading capacity results, hosting a Server and using Operator features."
        }
        "guides/installing-udpstp.md" => {
            "Install udpstp release binaries on Linux, macOS and Windows with a package manager or the install script, and update or remove them."
        }
        "guides/running-a-test.md" => {
            "Run Upstream and Downstream Tests, choose Client options, and save JSON results."
        }
        "guides/windows.md" => {
            "Set up udpstp on Windows: private key files, firewall rules, a trial Server in a window and the Windows service."
        }
        "understanding-results.md" => {
            "What udpstp's results mean: the Maximum IP-Layer Capacity, each line of the text results, the warnings, and why a search can fall short."
        }
        "guides/hosting-a-server.md" => {
            "Configure and host a UDPSTP Server: keys, the firewall, limits, MaxHops, socket buffers, address privacy, services and the log."
        }
        "reference/server-log.md" => {
            "Every event, field and reason in the udpstp Server's human and JSON log, and where the log is written."
        }
        "reference/platforms.md" => {
            "What differs between udpstp on Linux, macOS and Windows: DF, DSCP, key file checks, Data Phase Engines, fast paths and services."
        }
        "guides/operator-features.md" => {
            "Use Operator Keys and Fixed-Rate Tests to measure paths within your network, with explicit sending rates and packet settings."
        }
        "guides/building-udpstp.md" => {
            "Build udpstp from source with Rust, run the binary, and prepare a development checkout for contributions."
        }
        "reference/interfaces.md" => {
            "The compatibility policy for udpstp's command line, configuration, JSON results and Server logs, and how a stable interface changes."
        }
        "reference/cli.md" => {
            "Commands, options and defaults for udpstp client and udpstp server, generated from the binary's built-in help."
        }
        "reference/configuration.md" => {
            "Server settings, defaults and TOML configuration for authentication, Admission limits, Quotas, socket settings and logging."
        }
        "reference/json-output.md" => {
            "Field reference for udpstp's JSON Test results, OB-UDPST compatibility, schema versioning and optional result extensions."
        }
        "reference/exit-statuses.md" => {
            "udpstp's process exit statuses: completed Tests, invalid configuration, authentication failures, interrupted Tests and incomplete results."
        }
        "reference/key-files.md" => {
            "Authentication key-file formats, Key Table fields, key lifetimes and private-file permissions for udpstp Clients and Servers."
        }
        "conformance/index.md" => {
            "RFC 9946 and RFC 9097 Requirement coverage, proving tests, recorded Deviations, operational Requirements and Requirements that do not apply."
        }
        "conformance/format.md" => {
            "How udpstp records Deviations, Interpretations and OB-UDPST Differences, with RFC references, reasons and proving tests."
        }
        "glossary.md" => {
            "Definitions of udpstp's protocol roles, Tests, PDUs, measurement intervals, capacity metrics, authentication and conformance terms."
        }
        "troubleshooting.md" => {
            "Fix udpstp problems by symptom: no answer from the Server, refused Tests, key file errors, result warnings, socket buffers and Windows firewall rules."
        }
        "contributing.md" => {
            "Build and test udpstp, follow the Rust and protocol conventions, and prepare contributions with Requirement traceability."
        }
        "citation.md" => {
            "How to cite udpstp in research and publications, with BibTeX and the project's citation metadata."
        }
        "about.md" => {
            "About udpstp, its maintainer Peder Bergan, the MIT OR Apache-2.0 licence, and the site's privacy practices."
        }
        _ => return markdown::description(&page.markdown),
    };
    summary.to_string()
}

fn sitemap(pages: &[Page]) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n",
    );
    let _ = writeln!(out, "  <url><loc>{SITE}/</loc></url>");
    for page in pages {
        let _ = writeln!(
            out,
            "  <url><loc>{}</loc></url>",
            html_escape(&docs_url(&page.book))
        );
    }
    out.push_str("</urlset>\n");
    out
}

fn llms_txt(pages: &[Page], version: &str, released: bool, facts: &Facts) -> String {
    let status = if released {
        format!(
            "Version {version}. Install it with `curl -fsSL https://udpstp.com/install.sh | sh`, \
             `irm https://udpstp.com/install.ps1 | iex` on Windows, or \
             `cargo install --locked udpstp`."
        )
    } else {
        format!(
            "Source revision for version {version}. Install the latest published release \
             from {} or build this revision from \
             source (https://github.com/pederbe/udpstp).",
            docs_url("guides/installing-udpstp.md")
        )
    };
    let mut out = format!(
        "# udpstp\n\n\
         > udpstp is a Rust implementation of UDPSTP, the UDP Speed Test Protocol (RFC 9946). \
         It measures the Maximum IP-Layer Capacity of a network path (RFC 9097): a Client and a \
         Server exchange Load PDUs and Status PDUs over UDP, and the Server raises or lowers the \
         sending rate until it finds the highest rate the path delivers.\n\n\
         {status} {tested} of its {total} RFC Requirements are proven \
         by tests; the conformance pages list each one. It interoperates with OB-UDPST, the \
         Broadband Forum's C implementation. Terms are used as the glossary defines them. \
         Created by Peder Bergan (https://pederbe.dev). Every page below is Markdown; \
         llms-full.txt holds them all in one file.\n",
        tested = facts.count(Status::Tested) + facts.count(Status::Nightly),
        total = facts.total(),
    );
    let mut part = None;
    for page in pages {
        let name = if page.part.is_empty() {
            "Docs"
        } else {
            page.part
        };
        if part != Some(name) {
            part = Some(name);
            let _ = writeln!(out, "\n## {name}\n");
        }
        let description = markdown::description(&page.markdown);
        let _ = writeln!(
            out,
            "- [{}]({SITE}/{DOCS}/{}): {description}",
            page.label, page.book
        );
    }
    out
}

fn llms_full(pages: &[Page]) -> String {
    let mut out = String::from(
        "# udpstp documentation\n\nEvery page of https://udpstp.com in one file, in the order of \
         the docs' chapter list. Each page starts with its URL.\n",
    );
    for page in pages {
        let _ = write!(
            out,
            "\n---\n\nURL: {}\n\n{}\n",
            docs_url(&page.book),
            page.markdown.trim_end()
        );
    }
    out
}

/// The landing page from `website/index.html`, with its numbers, terminal
/// and diagram filled in.
fn landing(
    paths: &Paths,
    release: &Release,
    facts: &Facts,
    sequence: &markdown::Diagram,
) -> Result<String, String> {
    let template = read(&paths.website.join("index.html"))?;
    let tested = facts.count(Status::Tested) + facts.count(Status::Nightly);
    let golden = read(&paths.root.join("crates/udpstp/tests/golden/completed.txt"))?;
    let mut terminal = String::from(
        "<span class=\"prompt\">$</span> <span class=\"command\">udpstp client --key-file keys.toml 192.0.2.1</span>\n",
    );
    for line in golden.lines().take_while(|l| l.trim() != "Warnings") {
        let trimmed = line.trim_start();
        let escaped = html_escape(line);
        if matches!(trimmed, "Results" | "Sub-Interval results" | "Summary")
            || trimmed.starts_with("SI  ")
            || trimmed.starts_with("Phase  ")
            || trimmed.starts_with("RTT minimum (ms)")
        {
            let _ = writeln!(terminal, "<span class=\"info\">{escaped}</span>");
        } else if trimmed.starts_with("Maximum IP-Layer Capacity") {
            let _ = writeln!(terminal, "<span class=\"key\">{escaped}</span>");
        } else {
            let escaped = escaped.replace(
                "not measured",
                "<span class=\"warning\">not measured</span>",
            );
            let _ = writeln!(terminal, "{escaped}");
        }
    }
    let terminal = terminal.trim_end().to_string();
    // The table's columns cover the states in use today; a Requirement
    // pending or read by an Interpretation would need a column of its own.
    for state in [Status::Pending, Status::Interpretation] {
        if facts.count(state) > 0 {
            return Err(format!(
                "{} Requirements are {}: give website/index.html's coverage table a column for them",
                facts.count(state),
                state.label()
            ));
        }
    }
    let mut table = String::new();
    for section in &facts.sections {
        let count = |state: Status| {
            let index = conformance::STATES
                .iter()
                .position(|s| *s == state)
                .unwrap_or(0);
            section.counts[index]
        };
        let _ = writeln!(
            table,
            "<tr><th scope=\"row\"><a href=\"{DOCS}/{}\">{}</a> <span class=\"topic\">{}</span></th>\
             <td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            html_path(&section.page()),
            html_escape(&section.label),
            html_escape(&section.title),
            section.total(),
            count(Status::Tested) + count(Status::Nightly),
            count(Status::Deviation),
            count(Status::Operational),
            count(Status::NotApplicable),
        );
    }
    let figure = format!(
        "<picture><source srcset=\"diagrams/{key}-light.svg\" media=\"(prefers-color-scheme: light)\">\
         <img src=\"diagrams/{key}-dark.svg\" alt=\"\" width=\"{w}\" height=\"{h}\" loading=\"lazy\"></picture>",
        key = sequence.key,
        w = sequence.width,
        h = sequence.height
    );
    let replacements = [
        ("@REQ_TOTAL@", facts.total().to_string()),
        ("@REQ_TESTED@", tested.to_string()),
        (
            "@REQ_OPERATIONAL@",
            facts.count(Status::Operational).to_string(),
        ),
        (
            "@REQ_NOT_APPLICABLE@",
            facts.count(Status::NotApplicable).to_string(),
        ),
        ("@RECORDS@", facts.records().to_string()),
        ("@DEVIATIONS@", facts.deviations.to_string()),
        ("@INTERPRETATIONS@", facts.interpretations.to_string()),
        ("@OBUDPST_DIFFERENCES@", facts.with_obudpst.to_string()),
        ("<!-- terminal -->", terminal),
        ("<!-- conformance-table -->", table.trim_end().to_string()),
        ("<!-- sequence -->", figure),
    ];
    fill(&template, release.released, &replacements).map_err(|e| format!("website/index.html: {e}"))
}

/// A template with the release blocks that apply kept (`release_blocks`)
/// and its markers replaced. Every marker must be in the template, though
/// it may sit in a block that is left out. No marker may be left over.
fn fill(template: &str, released: bool, replacements: &[(&str, String)]) -> Result<String, String> {
    if let Some((marker, _)) = replacements.iter().find(|(m, _)| !template.contains(m)) {
        return Err(format!("no {marker}"));
    }
    let mut page = release_blocks(template, released)?;
    for (marker, value) in replacements {
        page = page.replace(marker, value);
    }
    let unknown = markers(&page);
    if !unknown.is_empty() {
        return Err(format!("unknown markers: {}", unknown.join(", ")));
    }
    Ok(page)
}

/// Every `@NAME@` marker, NAME in capitals and `_`, left in a page.
fn markers(page: &str) -> Vec<String> {
    let bytes = page.as_bytes();
    let mut found = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'@' {
            let len = bytes[i + 1..]
                .iter()
                .take_while(|b| b.is_ascii_uppercase() || **b == b'_')
                .count();
            if len > 0 && bytes.get(i + 1 + len) == Some(&b'@') {
                found.push(page[i..i + len + 2].to_string());
                i += len + 2;
                continue;
            }
        }
        i += 1;
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glossary_terms_get_anchors() {
        let context = "# UDPSTP\n\nIntro.\n\n## Language\n\n### Roles\n\n**Client**:\nThe endpoint.\n_Avoid_: Initiator\n\n**Load Sender** / **Load Receiver**:\nThe ends.\n";
        let page = glossary(context);
        assert!(page.contains("## Roles\n"));
        assert!(page.contains("### Client {#client}\n"));
        assert!(page.contains(
            "<a id=\"load-receiver\"></a>\n### Load Sender / Load Receiver {#load-sender}\n"
        ));
        assert!(page.contains("_Avoid:_ Initiator"));
        assert!(!page.contains("Intro."));
    }

    fn facts() -> Facts {
        Facts {
            sections: vec![conformance::Section {
                file: "rfc9946-06".into(),
                label: "RFC 9946 §6".into(),
                title: "Setup".into(),
                counts: [8, 1, 0, 1, 0, 0, 0],
            }],
            deviations: 1,
            interpretations: 2,
            obudpst_differences: 3,
            with_obudpst: 4,
        }
    }

    #[test]
    fn llms_txt_lists_every_page_by_part_and_says_whether_released() {
        let page = |book: &str, part, label: &str| Page {
            book: book.into(),
            label: label.into(),
            part,
            nested: false,
            markdown: format!("# {label}\n\nAbout {label}.\n"),
        };
        let pages = [
            page("getting-started.md", "", "Quickstart"),
            page("guides/index.md", "Guides", "Overview"),
            page("glossary.md", "Project", "Glossary"),
        ];
        let text = llms_txt(&pages, "0.1.0", false, &facts());
        assert!(text.starts_with("# udpstp\n\n> "));
        assert!(text.contains("Source revision for version 0.1.0."));
        assert!(!text.contains("pre-release"));
        assert!(text.contains("9 of its 10 RFC Requirements"));
        assert!(text.contains(
            "## Docs\n\n- [Quickstart](https://udpstp.com/docs/getting-started.md): About Quickstart."
        ));
        assert!(
            text.contains("## Guides\n\n- [Overview](https://udpstp.com/docs/guides/index.md)")
        );
        assert!(text.contains("## Project\n\n- [Glossary]"));
        let released = llms_txt(&pages, "0.1.0", true, &facts());
        assert!(!released.contains("pre-release"));
        assert!(released.contains("cargo install --locked udpstp"));
    }

    #[test]
    fn release_blocks_keep_the_one_that_applies() {
        let page =
            "a <!-- pre-release -->P<!-- /pre-release --><!-- released -->R<!-- /released --> b";
        assert_eq!(release_blocks(page, false).unwrap(), "a P b");
        assert_eq!(release_blocks(page, true).unwrap(), "a R b");
        assert!(release_blocks("<!-- released -->x", true).is_err());
        assert!(release_blocks("x<!-- /pre-release -->", true).is_err());
    }

    #[test]
    fn a_marker_may_sit_in_a_block_left_out_but_must_be_in_the_template() {
        let template = "a <!-- released -->@V@<!-- /released --> @N@ b";
        let values = [("@V@", "1".to_string()), ("@N@", "2".to_string())];
        assert_eq!(fill(template, false, &values).unwrap(), "a  2 b");
        assert_eq!(fill(template, true, &values).unwrap(), "a 1 2 b");
        // A marker the template lacks, and one nothing replaces.
        assert!(fill("a @N@", false, &values).is_err());
        assert!(fill("a @V@ @N@ @X@", true, &values).is_err());
    }

    // docs/website.md: release information belongs on GitHub.
    #[test]
    fn the_landing_page_links_to_releases_without_a_version() {
        let template = read(&crate::workspace_root().join("website/index.html")).unwrap();
        let found = markers(&template);
        let values: Vec<(&str, String)> = found
            .iter()
            .map(|m| (m.as_str(), "9.9.9".to_string()))
            .collect();
        for released in [false, true] {
            let page = fill(&template, released, &values).unwrap();
            assert!(!page.contains("softwareVersion"));
            assert!(!page.contains("Version "));
            assert!(!page.contains("changelog.html"));
            assert!(page.contains("href=\"https://github.com/pederbe/udpstp/releases\""));
        }
    }

    #[test]
    fn every_unknown_marker_is_found() {
        assert_eq!(
            markers("x@example.com \"@type\" @VERSION@ and @OTHER_ONE@ @ @A"),
            ["@VERSION@", "@OTHER_ONE@"]
        );
    }

    #[test]
    fn civil_dates() {
        assert_eq!(civil_date(0), (1970, 1, 1));
        assert_eq!(civil_date(20_724), (2026, 9, 28));
    }
}
