//! `cargo xtask site`: the udpstp.com website.
//!
//! The site is a hand-written landing page (`website/index.html`) and an
//! mdBook built from the repository's Markdown, which stays the source of
//! truth. `scripts/build-site.sh` runs the steps in order:
//!
//! 1. `node website/render.mjs` renders the PNG icons and the link-preview
//!    image into `target/site/rendered/`;
//! 2. `cargo xtask site prepare` writes the book's sources to
//!    `target/site/book/` (the repository's pages with their links
//!    rewritten, and the generated reference, conformance and glossary
//!    pages, with Mermaid diagrams rendered to SVG by mermaid-cli), and
//!    everything served beside the book to `target/site/extra/`: the
//!    landing page, a Markdown copy of every page, llms.txt, the sitemap and
//!    the response headers;
//! 3. `mdbook build target/site/book --dest-dir` writes
//!    `target/site/public/docs/`;
//! 4. `cargo xtask site finish` copies `extra/` over `target/site/public/`, gives every docs
//!    page its canonical URL, description, Markdown alternate and JSON-LD,
//!    and runs `check`: internal links and the palette's contrast.
//!
//! Nothing here deploys anything.

mod conformance;
mod contrast;
mod finish;
mod markdown;
mod prepare;
mod reference;

use std::path::{Path, PathBuf};

/// The site's canonical origin: the apex, no trailing slash.
pub const SITE: &str = "https://udpstp.com";

/// The book's directory on the site. The landing page is at the root, and
/// every docs page is under `/docs/`.
pub const DOCS: &str = "docs";

/// A docs page's canonical URL, from its path in the book. The book's front
/// page is the directory itself.
pub fn docs_url(book: &str) -> String {
    match book.trim_end_matches(".md") {
        "index" => format!("{SITE}/{DOCS}/"),
        page => format!("{SITE}/{DOCS}/{page}.html"),
    }
}

/// The repository paths of the book's user pages: every part but Project,
/// whose pages are for contributors.
pub fn user_pages() -> impl Iterator<Item = &'static str> {
    prepare::SOURCES
        .iter()
        .filter(|(_, _, part, _)| *part != "Project")
        .map(|(source, ..)| *source)
}

/// The repository, where links to files outside the book go.
pub const REPO: &str = "https://github.com/pederbe/udpstp";

pub const USAGE: &str = "usage: cargo xtask site <prepare|finish|check|contrast>

  prepare   write the book's sources and the landing page to target/site
            (needs UDPSTP_BIN, the built udpstp binary, and mermaid-cli in
            website/node_modules, or UDPSTP_MMDC)
  finish    complete target/site/public after mdbook built target/site/book
            into target/site/public/docs
  check     check target/site/public: links, contrast
  contrast  check the palette's token pairs (website/contrast.toml)

scripts/build-site.sh runs them all in order.";

pub fn run(args: &[String]) -> Result<(), String> {
    let paths = Paths::new(crate::workspace_root());
    match args.first().map(String::as_str) {
        Some("prepare") => prepare::run(&paths),
        Some("finish") => finish::run(&paths),
        Some("check") => finish::check(&paths),
        Some("contrast") => contrast::run(&paths.root),
        _ => Err(USAGE.to_string()),
    }
}

/// Where the site's inputs and outputs live.
pub struct Paths {
    pub root: PathBuf,
    /// `website/`: the landing page, theme and build tools.
    pub website: PathBuf,
    /// `target/site/`.
    pub out: PathBuf,
}

impl Paths {
    fn new(root: PathBuf) -> Self {
        Self {
            website: root.join("website"),
            out: root.join("target").join("site"),
            root,
        }
    }
    /// The mdBook project mdbook builds.
    pub fn book(&self) -> PathBuf {
        self.out.join("book")
    }
    pub fn src(&self) -> PathBuf {
        self.book().join("src")
    }
    /// Files served beside the book, copied into the site by `finish`.
    pub fn extra(&self) -> PathBuf {
        self.out.join("extra")
    }
    /// The finished site.
    pub fn public(&self) -> PathBuf {
        self.out.join("public")
    }
    /// The book in the finished site, where mdBook writes it.
    pub fn docs(&self) -> PathBuf {
        self.public().join(DOCS)
    }
    /// What `website/render.mjs` rendered.
    pub fn rendered(&self) -> PathBuf {
        self.out.join("rendered")
    }
    /// Rendered diagrams, by the hash of their source and theme.
    pub fn diagram_cache(&self) -> PathBuf {
        self.out.join("diagram-cache")
    }
    /// The page list `prepare` leaves for `finish`.
    pub fn pages_list(&self) -> PathBuf {
        self.out.join("pages.tsv")
    }
}

/// Writes a file, creating its directory.
pub fn write(path: &Path, content: impl AsRef<[u8]>) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("creating {}: {e}", parent.display()))?;
    }
    std::fs::write(path, content).map_err(|e| format!("writing {}: {e}", path.display()))
}

pub fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))
}

pub fn copy(from: &Path, to: &Path) -> Result<(), String> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("creating {}: {e}", parent.display()))?;
    }
    std::fs::copy(from, to)
        .map(|_| ())
        .map_err(|e| format!("copying {} to {}: {e}", from.display(), to.display()))
}

/// Every file under a directory, recursively, in name order.
pub fn files_under(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("reading {}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            out.extend(files_under(&path)?);
        } else {
            out.push(path);
        }
    }
    Ok(out)
}

/// A path relative to `base`, with forward slashes.
pub fn relative(path: &Path, base: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Escapes text for HTML content and attribute values.
pub fn html_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// A heading's anchor as mdBook makes it: lowercase, with runs of anything
/// but letters, digits, `-` and `_` removed and spaces made hyphens.
pub fn slug(text: &str) -> String {
    let mut out = String::new();
    for c in text.trim().chars() {
        if c.is_alphanumeric() || c == '-' || c == '_' {
            out.extend(c.to_lowercase());
        } else if c.is_whitespace() {
            out.push('-');
        }
    }
    out
}

/// The `udpstp` package version, from its manifest.
pub fn version(root: &Path) -> Result<String, String> {
    let manifest = read(&root.join("crates/udpstp/Cargo.toml"))?;
    manifest
        .lines()
        .find_map(|line| {
            line.strip_prefix("version = \"")
                .and_then(|rest| rest.strip_suffix('"'))
        })
        .map(str::to_string)
        .ok_or_else(|| "crates/udpstp/Cargo.toml has no version".to_string())
}

#[cfg(test)]
mod tests {
    use super::{docs_url, html_escape, slug};

    #[test]
    fn docs_pages_are_under_docs_and_the_front_page_is_the_directory() {
        assert_eq!(docs_url("index.md"), "https://udpstp.com/docs/");
        assert_eq!(
            docs_url("guides/index.md"),
            "https://udpstp.com/docs/guides/index.html"
        );
        assert_eq!(
            docs_url("getting-started.md"),
            "https://udpstp.com/docs/getting-started.html"
        );
    }

    #[test]
    fn slugs_follow_mdbook() {
        assert_eq!(slug("Key tables"), "key-tables");
        assert_eq!(
            slug("Load Sender / Load Receiver"),
            "load-sender--load-receiver"
        );
        assert_eq!(slug("mcIdent"), "mcident");
    }

    #[test]
    fn escapes() {
        assert_eq!(
            html_escape("<a href=\"x\">&"),
            "&lt;a href=&quot;x&quot;&gt;&amp;"
        );
    }
}
