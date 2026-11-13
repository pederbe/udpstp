#![forbid(unsafe_code)]
//! Human output layout. Width belongs to the destination, and prose and
//! tables count displayed columns. Unbroken identifiers stay intact on
//! their own continuation line even when they are wider than a terminal.

use std::io::IsTerminal as _;

use unicode_width::UnicodeWidthStr as _;

pub const UNKNOWN_WIDTH: usize = 80;

pub fn stdout_width() -> usize {
    let output = std::io::stdout();
    destination_width(output.is_terminal(), || {
        terminal_size::terminal_size_of(&output)
    })
}

pub fn stderr_width() -> usize {
    let output = std::io::stderr();
    destination_width(output.is_terminal(), || {
        terminal_size::terminal_size_of(&output)
    })
}

fn destination_width(
    is_terminal: bool,
    size: impl FnOnce() -> Option<(terminal_size::Width, terminal_size::Height)>,
) -> usize {
    if is_terminal {
        size()
            .map(|(width, _)| usize::from(width.0))
            .filter(|&w| w > 0)
    } else {
        None
    }
    .unwrap_or(UNKNOWN_WIDTH)
}

pub fn columns(text: &str) -> usize {
    if text.contains('\u{1b}') {
        anstream::adapter::strip_str(text).to_string().width()
    } else {
        text.width()
    }
}

/// Retain existing short lines, including table padding. A long prose
/// line continues two columns beyond its initial indentation. No token
/// is clipped or changed, so addresses and diagnostics remain copyable.
pub fn wrap(text: &str, width: usize) -> String {
    let mut output = String::new();
    for line in text.split_inclusive('\n') {
        let (line, newline) = line.strip_suffix('\n').map_or((line, ""), |l| (l, "\n"));
        if columns(line) <= width {
            output.push_str(line);
        } else {
            let indent = line.chars().take_while(|c| *c == ' ').count();
            let initial = " ".repeat(indent);
            let continued = format!("{initial}  ");
            output.push_str(&wrap_line(line.trim_start(), width, &initial, &continued));
        }
        output.push_str(newline);
    }
    output
}

/// A distinct continuation marker keeps adjacent Server log events apart.
pub fn log_entry(text: &str, width: usize) -> String {
    let mut output = wrap_line(text.trim_end(), width, "", "  | ");
    output.push('\n');
    output
}

fn wrap_line(text: &str, width: usize, initial: &str, continued: &str) -> String {
    let mut output = initial.to_owned();
    let mut used = columns(initial);
    let mut occupied = false;
    for word in text.split_whitespace() {
        let word_width = columns(word);
        if occupied && used + 1 + word_width > width {
            output.push('\n');
            output.push_str(continued);
            used = columns(continued);
            occupied = false;
        }
        if occupied {
            output.push(' ');
            used += 1;
        }
        output.push_str(word);
        used += word_width;
        occupied = true;
    }
    output
}

/// Aligned tables use real cell widths, including values larger than the
/// examples. If even a grouped table cannot fit, labelled rows keep every
/// value without clipping or breaking numbers.
pub fn table(headers: &[&str], rows: &[Vec<String>], width: usize) -> String {
    let widths: Vec<_> = headers
        .iter()
        .enumerate()
        .map(|(i, header)| {
            rows.iter()
                .map(|row| columns(&row[i]))
                .chain([columns(header)])
                .max()
                .unwrap_or(0)
        })
        .collect();
    let table_width = 2 + widths.iter().sum::<usize>() + 2 * widths.len().saturating_sub(1);
    let mut out = String::new();
    if table_width > width {
        for row in rows {
            for (header, value) in headers.iter().zip(row) {
                out.push_str(&wrap(&format!("  {header}: {value}\n"), width));
            }
            out.push('\n');
        }
        return out;
    }
    for row in std::iter::once(headers.iter().map(|h| h.to_string()).collect::<Vec<_>>())
        .chain(rows.iter().cloned())
    {
        out.push_str("  ");
        for (i, cell) in row.iter().enumerate() {
            if i > 0 {
                out.push_str("  ");
            }
            out.push_str(&" ".repeat(widths[i] - columns(cell)));
            out.push_str(cell);
        }
        out.push('\n');
    }
    out
}

pub fn table_fits(headers: &[&str], rows: &[Vec<String>], width: usize) -> bool {
    let actual = table(headers, rows, usize::MAX);
    actual.lines().all(|line| columns(line) <= width)
}

#[cfg(test)]
mod tests {
    use super::*;

    // docs/spec.md's human results and the terminal layout policy: alignment
    // counts displayed columns while preserving the characters printed.
    #[test]
    fn displayed_columns_preserve_sequences_and_table_alignment() {
        assert_eq!(columns("測定"), 4);
        assert_eq!(columns("e\u{301}"), 1);
        let text = wrap("  測定 測定 e\u{301} e\u{301}\n", 12);
        assert_eq!(text, "  測定 測定\n    e\u{301} e\u{301}\n");
        let table = table(
            &["Name", "Value"],
            &[vec!["測定".into(), "e\u{301}".into()]],
            20,
        );
        assert_eq!(table, "  Name  Value\n  測定      e\u{301}\n");
    }

    // docs/spec.md's diagnostics and the terminal layout policy: an oversized
    // identifier stays copyable on a continuation line without clipping.
    #[test]
    fn long_identifier_is_intact_on_a_continuation_line() {
        let identifier = "[2001:db8:1234:5678:abcd:eeee:ffff:9999%long-interface-name]:24601";
        let output = wrap(
            &format!("udpstp: cannot resolve {identifier} after trying every address\n"),
            60,
        );
        assert!(
            output.lines().any(|line| line.trim() == identifier),
            "{output}"
        );
        assert!(output.contains("    ") || output.contains("\n  "));
        assert!(!output.contains('\u{1b}'));
    }
}
