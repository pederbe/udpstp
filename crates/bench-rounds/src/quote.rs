//! POSIX shell quoting, exactly as Python's `shlex.quote` and `shlex.join`
//! quote: every command a Round sends a host is built with them, and is
//! compared byte for byte with the Python's.

use std::borrow::Cow;

/// shlex's characters that need no quoting: `%+,-./0-9:=@A-Z_a-z`.
fn is_safe(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(c, '%' | '+' | ',' | '-' | '.' | '/' | ':' | '=' | '@' | '_')
}

/// `shlex.quote(s)`: `s` as it is where it is only safe ASCII characters,
/// `''` where it is empty, else `s` in single quotes with each `'` as
/// `'"'"'`.
pub fn quote(s: &str) -> Cow<'_, str> {
    if s.is_empty() {
        Cow::Borrowed("''")
    } else if s.chars().all(is_safe) {
        Cow::Borrowed(s)
    } else {
        Cow::Owned(format!("'{}'", s.replace('\'', "'\"'\"'")))
    }
}

/// `shlex.join(args)`: each argument quoted, with a space between.
pub fn join<S: AsRef<str>>(args: &[S]) -> String {
    let quoted: Vec<Cow<str>> = args.iter().map(|a| quote(a.as_ref())).collect();
    quoted.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_as_shlex_does() {
        assert_eq!(quote(""), "''");
        assert_eq!(quote("abc"), "abc");
        assert_eq!(quote("%+,-./:=@_09AZaz"), "%+,-./:=@_09AZaz");
        assert_eq!(quote("a b"), "'a b'");
        assert_eq!(quote("it's"), r#"'it'"'"'s'"#);
        assert_eq!(quote("'"), r#"''"'"''"#);
        assert_eq!(quote("$HOME"), "'$HOME'");
        assert_eq!(quote("~"), "'~'");
        assert_eq!(quote("a\nb"), "'a\nb'");
        // Anything not ASCII is quoted, letters too.
        assert_eq!(quote("é"), "'é'");
    }

    #[test]
    fn joins_as_shlex_does() {
        assert_eq!(join::<&str>(&[]), "");
        assert_eq!(join(&[""]), "''");
        assert_eq!(
            join(&["ssh", "-o", "BatchMode=yes", "host", "bash -s"]),
            "ssh -o BatchMode=yes host 'bash -s'"
        );
    }
}
