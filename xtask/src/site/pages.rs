//! The book's pages, in chapter order. Repository links, user-documentation
//! checks and assembly all use this registration.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use super::markdown::{self, LinkMap};
use super::{read, slug};

/// One page of the book.
pub(super) struct Page {
    /// Its path in the book, `guides/running-a-test.md`.
    pub book: String,
    /// Its entry in the chapter list.
    pub label: String,
    /// The part of the chapter list it is in; empty before the first part.
    pub part: &'static str,
    pub nested: bool,
    /// The Markdown served beside the page.
    pub markdown: String,
}

enum Chapter {
    /// Repository path, book path, part, label (or the heading when empty),
    /// and whether the chapter is nested beneath the preceding chapter.
    Repository(&'static str, &'static str, &'static str, &'static str, bool),
    Reference(&'static str),
    Conformance,
    Citation,
}

use Chapter::{Citation, Conformance, Reference, Repository};

const CHAPTERS: &[Chapter] = &[
    Repository("website/pages/index.md", "index.md", "", "", false),
    Repository(
        "docs/guides/installing-udpstp.md",
        "guides/installing-udpstp.md",
        "Getting started",
        "Installation",
        false,
    ),
    Repository(
        "docs/guides/building-udpstp.md",
        "guides/building-udpstp.md",
        "Getting started",
        "Building from source",
        true,
    ),
    Repository(
        "docs/getting-started.md",
        "getting-started.md",
        "Getting started",
        "",
        false,
    ),
    Repository(
        "docs/guides/README.md",
        "guides/index.md",
        "Guides",
        "Overview",
        false,
    ),
    Repository(
        "docs/guides/running-a-test.md",
        "guides/running-a-test.md",
        "Guides",
        "Running a Test",
        false,
    ),
    Repository(
        "docs/guides/hosting-a-server.md",
        "guides/hosting-a-server.md",
        "Guides",
        "",
        false,
    ),
    Repository(
        "docs/guides/windows.md",
        "guides/windows.md",
        "Guides",
        "",
        false,
    ),
    Repository(
        "docs/guides/operator-features.md",
        "guides/operator-features.md",
        "Guides",
        "",
        false,
    ),
    Reference("reference/cli.md"),
    Reference("reference/configuration.md"),
    Reference("reference/exit-statuses.md"),
    Repository(
        "docs/json-output.md",
        "reference/json-output.md",
        "Reference",
        "",
        false,
    ),
    Repository(
        "docs/server-log.md",
        "reference/server-log.md",
        "Reference",
        "",
        false,
    ),
    Repository(
        "docs/key-files.md",
        "reference/key-files.md",
        "Reference",
        "",
        false,
    ),
    Repository(
        "docs/platforms.md",
        "reference/platforms.md",
        "Reference",
        "",
        false,
    ),
    Repository(
        "docs/interfaces.md",
        "reference/interfaces.md",
        "Reference",
        "",
        false,
    ),
    Repository("GLOSSARY.md", "glossary.md", "Reference", "Glossary", false),
    Repository(
        "docs/understanding-results.md",
        "understanding-results.md",
        "Explanation",
        "",
        false,
    ),
    Repository("docs/security.md", "security.md", "Explanation", "", false),
    Conformance,
    Repository(
        "docs/troubleshooting.md",
        "troubleshooting.md",
        "Help",
        "",
        false,
    ),
    Repository(
        "website/pages/contributing.md",
        "contributing.md",
        "Project",
        "Contributing",
        false,
    ),
    Repository(
        "docs/conformance/README.md",
        "conformance/format.md",
        "Project",
        "How the records work",
        true,
    ),
    Repository(
        "docs/architecture.md",
        "architecture.md",
        "Project",
        "",
        false,
    ),
    Repository("docs/technology.md", "technology.md", "Project", "", true),
    Repository(
        "SECURITY.md",
        "security-policy.md",
        "Project",
        "Reporting a vulnerability",
        false,
    ),
    Citation,
    Repository("website/pages/about.md", "about.md", "Project", "", false),
];

/// Where each repository page is published.
pub(super) fn repository_links() -> BTreeMap<String, String> {
    CHAPTERS
        .iter()
        .filter_map(|chapter| match chapter {
            Repository(source, book, ..) => Some((source.to_string(), book.to_string())),
            _ => None,
        })
        .collect()
}

/// The repository paths of user pages, excluding the contributor-only Project part.
pub fn user_pages() -> impl Iterator<Item = &'static str> {
    CHAPTERS.iter().filter_map(|chapter| match chapter {
        Repository(source, _, part, ..) if *part != "Project" => Some(*source),
        _ => None,
    })
}

/// Assembles repository and generated Markdown in the registered chapter order.
pub(super) fn assemble(
    links: &LinkMap<'_>,
    reference: &[(String, String)],
    conformance: &[(String, String)],
    citation: String,
) -> Result<Vec<Page>, String> {
    let reference_markdown = |book: &str| {
        reference
            .iter()
            .find(|(path, _)| path == book)
            .map(|(_, markdown)| markdown.clone())
            .ok_or_else(|| format!("no generated page {book}"))
    };
    let generated = |book: &str, part, nested, markdown: String| Page {
        label: markdown::title(&markdown).unwrap_or_default(),
        book: book.to_string(),
        part,
        nested,
        markdown,
    };
    let mut pages = Vec::new();
    for chapter in CHAPTERS {
        match *chapter {
            Repository(source, book, part, label, nested) => {
                let text = read(&links.root.join(source))?;
                let text = if source == "GLOSSARY.md" {
                    glossary(&text)
                } else {
                    text
                };
                let mut markdown = links.rewrite(&text, source, book);
                let label = if label.is_empty() {
                    markdown::title(&markdown).ok_or_else(|| format!("{source}: no # heading"))?
                } else {
                    label.to_string()
                };
                // The Key Table's recorded choices follow its hand-written format.
                if source == "docs/key-files.md" {
                    markdown.push_str(&reference_markdown(book)?);
                }
                pages.push(Page {
                    book: book.to_string(),
                    label,
                    part,
                    nested,
                    markdown,
                });
            }
            Reference(book) => pages.push(generated(
                book,
                "Reference",
                false,
                reference_markdown(book)?,
            )),
            Conformance => {
                for (index, (book, markdown)) in conformance.iter().enumerate() {
                    pages.push(generated(book, "Conformance", index > 0, markdown.clone()));
                }
            }
            Citation => pages.push(generated("citation.md", "Project", false, citation.clone())),
        }
    }
    Ok(pages)
}

/// GLOSSARY.md as the glossary page, with one anchor per term.
fn glossary(context: &str) -> String {
    let mut out = String::from(
        "# Glossary\n\n\
         Press **Ctrl+F** (**Command+F** on macOS) to find a term on this page.\n\n\
         udpstp uses these terms the same way in its output, documentation, code and issues. \
         They come from the repository's `GLOSSARY.md`. Each term has its own anchor, so you \
         can link to it. _Avoid_ lists words udpstp doesn't use for it.\n",
    );
    let mut lines = context.lines().peekable();
    while let Some(line) = lines.peek() {
        if line.starts_with("### ") {
            break;
        }
        lines.next();
    }
    for line in lines {
        // The benchmark tooling's vocabulary is not part of the website.
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

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeSet;

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

    // docs/website.md: preserve chapter order and nesting, generated reference
    // and conformance pages, and the recorded choices appended to key files.
    #[test]
    fn assembly_preserves_navigation_and_generated_content() {
        let root = crate::workspace_root();
        let map = repository_links();
        let links = LinkMap {
            pages: &map,
            git_ref: "main",
            root: &root,
            tracked: &BTreeSet::new(),
            untracked: RefCell::new(Vec::new()),
        };
        let reference = [
            ("reference/cli.md".into(), "# Command line\n".into()),
            (
                "reference/configuration.md".into(),
                "# Configuration\n".into(),
            ),
            (
                "reference/exit-statuses.md".into(),
                "# Exit statuses\n".into(),
            ),
            (
                "reference/key-files.md".into(),
                "\n## Recorded Key Table choices\n".into(),
            ),
        ];
        let conformance = [
            ("conformance/index.md".into(), "# Conformance\n".into()),
            ("conformance/example.md".into(), "# Example\n".into()),
        ];
        let pages = assemble(&links, &reference, &conformance, "# Citing udpstp\n".into()).unwrap();
        let position = |book: &str| pages.iter().position(|page| page.book == book).unwrap();
        let installation = position("guides/installing-udpstp.md");
        assert_eq!(pages[installation].label, "Installation");
        assert_eq!(pages[installation + 1].book, "guides/building-udpstp.md");
        assert!(pages[installation + 1].nested);
        assert_eq!(pages[installation + 2].book, "getting-started.md");
        assert!(!pages[installation + 2].nested);
        let cli = position("reference/cli.md");
        assert_eq!(pages[cli + 1].book, "reference/configuration.md");
        assert_eq!(pages[cli + 2].book, "reference/exit-statuses.md");
        assert_eq!(pages[cli + 3].book, "reference/json-output.md");
        assert!(
            pages[position("reference/key-files.md")]
                .markdown
                .ends_with(&reference[3].1)
        );
        let conformance = position("conformance/index.md");
        assert!(!pages[conformance].nested);
        assert_eq!(pages[conformance + 1].part, "Conformance");
        assert!(pages[conformance + 1].nested);
        assert_eq!(pages[conformance + 2].book, "troubleshooting.md");
        assert!(pages[position("conformance/format.md")].nested);
        assert!(pages[position("technology.md")].nested);
        assert_eq!(pages[position("citation.md")].markdown, "# Citing udpstp\n");
        assert_eq!(pages.last().unwrap().book, "about.md");
        assert_eq!(
            map["docs/guides/installing-udpstp.md"],
            "guides/installing-udpstp.md"
        );
        let users: Vec<_> = user_pages().collect();
        assert!(users.contains(&"docs/guides/installing-udpstp.md"));
        assert!(!users.contains(&"CONTRIBUTING.md"));
    }
}
