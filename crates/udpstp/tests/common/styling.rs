//! Visible process output, with complete SGR sequences required.

pub fn plain(text: &str) -> String {
    let mut parts = text.split('\x1b');
    let mut output = parts.next().unwrap_or_default().to_owned();
    for part in parts {
        let (control, rest) = part.split_once('m').expect("a complete SGR sequence");
        let parameters = control.strip_prefix('[').expect("only SGR styling");
        assert!(
            parameters
                .chars()
                .all(|ch| ch.is_ascii_digit() || ch == ';')
        );
        output.push_str(rest);
    }
    output
}
