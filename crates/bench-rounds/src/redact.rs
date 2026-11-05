//! What could be a key, removed from a host's error before it reaches a
//! Round's output (`round.log`, `meta.json`, a report), exactly as the
//! Python's `redact` removes it (`scripts/bench/hosts.py:25-34`): the word
//! after OB-UDPST's `-a`, and every run of 32 hex digits or more, such as
//! the Round's key text; then the text cut to 400 characters.
//!
//! The Python matches with regular expressions that look around, which
//! Rust's `regex` can't, so each is a small scanner here, with Python's
//! `\s` ([`py::is_space`](crate::py::is_space)).

use crate::py::is_space;

/// The most characters of a redacted text; a longer one is cut there and
/// ends with ` …`.
pub const REDACTED_LENGTH: usize = 400;

/// `redact(text)`: each key after `-a` and each long hex run replaced by
/// `<redacted>`, then at most [`REDACTED_LENGTH`] characters, counted as
/// Python counts them (code points), and ` …` where it was cut.
pub fn redact(text: &str) -> String {
    let text = hex_runs(&key_options(text));
    if text.chars().count() <= REDACTED_LENGTH {
        return text;
    }
    let cut: String = text.chars().take(REDACTED_LENGTH).collect();
    format!("{cut} …")
}

/// `re.sub(r"(?<!\S)(-a\s+)(\S+)", r"\1<redacted>", text)`: an `-a` at
/// the start or after whitespace, then whitespace, then a word; the word
/// is replaced.
fn key_options(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        match key_option_at(&chars, i) {
            Some((word, end)) => {
                out.extend(&chars[i..word]);
                out.push_str("<redacted>");
                i = end;
            }
            None => {
                out.push(chars[i]);
                i += 1;
            }
        }
    }
    out
}

/// The match at `i`, if one starts there: where its word starts and ends.
/// The whitespace takes all it can, as the greedy `\s+` does, and the word
/// runs to the next whitespace.
fn key_option_at(chars: &[char], i: usize) -> Option<(usize, usize)> {
    let after_space = i == 0 || is_space(chars[i - 1]);
    if !after_space || chars.get(i..i + 2) != Some(&['-', 'a'][..]) {
        return None;
    }
    let word = i + 2 + chars[i + 2..].iter().take_while(|&&c| is_space(c)).count();
    let end = word + chars[word..].iter().take_while(|&&c| !is_space(c)).count();
    (word > i + 2 && end > word).then_some((word, end))
}

/// `re.sub(r"(?<![0-9A-Fa-f])[0-9A-Fa-f]{32,}(?![0-9A-Fa-f])",
/// "<redacted>", text)`: each whole run of 32 hex digits or more.
fn hex_runs(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(|c: char| c.is_ascii_hexdigit()) {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let len = rest
            .find(|c: char| !c.is_ascii_hexdigit())
            .unwrap_or(rest.len());
        // Hex digits are ASCII, one byte each: `len` counts them.
        if len >= 32 {
            out.push_str("<redacted>");
        } else {
            out.push_str(&rest[..len]);
        }
        rest = &rest[len..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_the_word_after_dash_a() {
        assert_eq!(redact("udpst -a secret -x"), "udpst -a <redacted> -x");
        assert_eq!(redact("-a  key"), "-a  <redacted>");
        assert_eq!(redact("-a\tkey\nnext"), "-a\t<redacted>\nnext");
        // Python's \s takes U+001C, which Rust's whitespace doesn't.
        assert_eq!(redact("-a\u{1c}key"), "-a\u{1c}<redacted>");
        assert_eq!(redact("x -a\u{3000}κλειδί y"), "x -a\u{3000}<redacted> y");
        for unchanged in ["x-a key", "-akey", "-a", "-a ", "-b key", "--a key"] {
            assert_eq!(redact(unchanged), unchanged);
        }
        assert_eq!(redact("-a -a -a x"), "-a <redacted> -a <redacted>");
    }

    #[test]
    fn redacts_runs_of_32_hex_digits_or_more() {
        let hex31 = "0123456789abcdefABCDEF012345678";
        let hex32 = format!("{hex31}9");
        assert_eq!(redact(hex31), hex31);
        assert_eq!(redact(&hex32), "<redacted>");
        assert_eq!(redact(&format!("key={hex32}.")), "key=<redacted>.");
        assert_eq!(redact(&format!("x{hex32}{hex32}g")), "x<redacted>g");
        // Joined to a hex digit, a run is longer, not a new one.
        assert_eq!(redact(&format!("a{hex31}")), "<redacted>");
        assert_eq!(
            redact(&format!("-a key {hex32}")),
            "-a <redacted> <redacted>"
        );
    }

    #[test]
    fn cuts_at_400_characters_not_bytes() {
        let short = "é".repeat(400);
        assert_eq!(redact(&short), short);
        let long = "é".repeat(401);
        assert_eq!(redact(&long), format!("{short} …"));
    }
}
