//! Part of `cargo xtask glossary`: checks that the user documentation names
//! no release, so no page goes stale when the project releases. It reads the
//! README, the crates' READMEs, the install scripts and the book's user
//! pages (every part of the book but Project), and reports each number
//! shaped like `1.2.3` or `v1.2.3`. Release lines such as "1.0" have two
//! parts and pass, as do IP addresses and RFC section numbers ("§5.3.1").

use std::path::Path;

/// Exact phrases a page may carry: another product's version that the
/// reader needs, and sample data.
const ALLOWED: &[&str] = &[
    // The OB-UDPST release whose JSON output and behavior udpstp matches,
    // and the directory of its vendored examples.
    "OB-UDPST v9.1.0",
    "testdata/obudpst-9.1.0",
];

const FILES: &[&str] = &["README.md", "install/install.sh", "install/install.ps1"];

pub fn run() -> Result<(), String> {
    let root = crate::workspace_root();
    let mut files: Vec<String> = FILES.iter().map(|f| (*f).to_string()).collect();
    let crates = std::fs::read_dir(root.join("crates")).map_err(|e| format!("crates: {e}"))?;
    for entry in crates {
        let name = entry.map_err(|e| e.to_string())?.file_name();
        let readme = format!("crates/{}/README.md", name.to_string_lossy());
        if root.join(&readme).exists() {
            files.push(readme);
        }
    }
    files.extend(crate::site::user_pages().map(str::to_string));
    files.sort();
    files.dedup();
    let mut found = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(root.join(Path::new(file)))
            .map_err(|e| format!("reading {file}: {e}"))?;
        for (number, line) in text.lines().enumerate() {
            for token in release_numbers(line) {
                found.push(format!("  {file}:{}: {token}", number + 1));
            }
        }
    }
    println!("Checked {} user pages for release numbers", files.len());
    if found.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} release number(s) in the user documentation; remove the number, or add the \
             exact phrase to ALLOWED in xtask/src/release_numbers.rs when it is another \
             product's version or sample data:\n{}",
            found.len(),
            found.join("\n")
        ))
    }
}

/// The tokens of a line shaped like `1.2.3` or `v1.2.3`, outside the
/// allowed phrases. A number with more parts, such as an IP address, is
/// not one, nor is a section number after "§".
fn release_numbers(line: &str) -> Vec<&str> {
    let mut line_without_allowed = line.to_string();
    for phrase in ALLOWED {
        // Same length, so that the tokens found below index into `line`.
        line_without_allowed = line_without_allowed.replace(phrase, &" ".repeat(phrase.len()));
    }
    let bytes = line_without_allowed.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        // A run of digits and single dots between digits.
        let start = i;
        let mut parts = 1;
        loop {
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if bytes.get(i) == Some(&b'.') && bytes.get(i + 1).is_some_and(u8::is_ascii_digit) {
                parts += 1;
                i += 1;
            } else {
                break;
            }
        }
        let before = line_without_allowed[..start].chars().next_back();
        let v = before == Some('v')
            && !line_without_allowed[..start - 1].ends_with(|c: char| c.is_alphanumeric());
        let bare = !before.is_some_and(|c| c.is_alphanumeric() || c == '§');
        if parts == 3 && (v || bare) {
            tokens.push(&line[start - usize::from(v)..i]);
        }
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::release_numbers;

    #[test]
    fn finds_release_numbers_with_and_without_the_v() {
        let line = "Download udpstp-v1.2.3.tar.gz (1.2.3), or `--version 0.10.0`.";
        assert_eq!(release_numbers(line), ["v1.2.3", "1.2.3", "0.10.0"]);
    }

    #[test]
    fn passes_release_lines_addresses_sections_and_allowed_phrases() {
        for line in [
            "Stable from 1.0, and 0.x until then.",
            "Listens on 127.0.0.1 and 192.0.2.7:25000, never 255.255.255.255.",
            "See RFC 9946 §5.3.1, §7.2.1.",
            "Compatible with OB-UDPST v9.1.0's client output.",
            "The examples in `testdata/obudpst-9.1.0/json_examples`.",
            "ipv4.example and sha256.sum",
        ] {
            assert!(release_numbers(line).is_empty(), "{line}");
        }
    }

    #[test]
    fn an_allowed_phrase_does_not_excuse_another_number_on_its_line() {
        let line = "OB-UDPST v9.1.0 matches udpstp 0.2.0.";
        assert_eq!(release_numbers(line), ["0.2.0"]);
    }
}
