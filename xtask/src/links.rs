//! `cargo xtask links`: checks that every relative link in the Markdown
//! files points at a file or directory that exists. Links to web pages and
//! anchors within the same page are not checked.

use std::path::{Path, PathBuf};

/// Directories never scanned: build output, gitignored third-party
/// sources, and the website's build tools (`website/node_modules`).
const SKIPPED_DIRS: &[&str] = &[".git", "target", "reference", "node_modules"];

pub fn run() -> Result<(), String> {
    let root = crate::workspace_root();
    let mut files = Vec::new();
    markdown_files(&root, &mut files)?;
    let mut broken = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file)
            .map_err(|e| format!("reading {}: {e}", file.display()))?;
        let dir = file.parent().expect("a file has a parent");
        for target in relative_links(&text) {
            if !dir.join(&target).exists() {
                let relative = file.strip_prefix(&root).unwrap_or(file);
                broken.push(format!("  {}: {target}", relative.display()));
            }
        }
    }
    println!(
        "Checked the relative links in {} Markdown files",
        files.len()
    );
    if broken.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} broken link(s):\n{}",
            broken.len(),
            broken.join("\n")
        ))
    }
}

fn markdown_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("reading {}: {e}", dir.display()))?;
    for entry in entries {
        let path = entry.map_err(|e| e.to_string())?.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if path.is_dir() {
            // Another checkout inside this one, such as a git worktree an
            // agent works in, is another tree with its own links.
            if !SKIPPED_DIRS.contains(&name) && !path.join(".git").exists() {
                markdown_files(&path, out)?;
            }
        } else if name.ends_with(".md") {
            out.push(path);
        }
    }
    out.sort();
    Ok(())
}

/// The targets of inline links and images (`[text](target)`) outside code
/// blocks and code spans, without any `#fragment`, skipping web links and
/// same-page anchors.
pub fn relative_links(markdown: &str) -> Vec<String> {
    let mut links = Vec::new();
    let mut in_fence = false;
    for line in markdown.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let prose: String = line.split('`').step_by(2).collect::<Vec<_>>().join(" ");
        let mut rest = prose.as_str();
        while let Some(start) = rest.find("](") {
            rest = &rest[start + 2..];
            let Some(end) = rest.find(')') else { break };
            let target = rest[..end].split_whitespace().next().unwrap_or("");
            let path = target.split('#').next().unwrap_or("");
            let external = target.contains("://") || target.starts_with("mailto:");
            if !external && !path.is_empty() {
                links.push(path.to_string());
            }
            rest = &rest[end..];
        }
    }
    links
}

#[cfg(test)]
mod tests {
    use super::{markdown_files, relative_links};

    #[test]
    fn finds_relative_links_and_drops_fragments() {
        let md = "See [the glossary](GLOSSARY.md) and [ADRs](docs/adr/#top).\n";
        assert_eq!(relative_links(md), ["GLOSSARY.md", "docs/adr/"]);
    }

    #[test]
    fn skips_web_links_anchors_and_code() {
        let md = "[a](https://example.com) [b](#status) [c](mailto:x@example.com)\n\
                  `[d](inline.md)`\n\
                  ```\n[e](fenced.md)\n```\n";
        assert!(relative_links(md).is_empty(), "{:?}", relative_links(md));
    }

    #[test]
    fn skips_another_checkout_inside_the_tree() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let root =
            std::env::temp_dir().join(format!("udpstp-links-{}-{nanos}", std::process::id()));
        let nested = root.join("worktrees").join("agent");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(root.join("README.md"), "").unwrap();
        std::fs::write(nested.join("README.md"), "").unwrap();
        // A git worktree has a .git file where a clone has a directory.
        std::fs::write(nested.join(".git"), "gitdir: elsewhere\n").unwrap();
        let mut files = Vec::new();
        markdown_files(&root, &mut files).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(files, [root.join("README.md")]);
    }
}
