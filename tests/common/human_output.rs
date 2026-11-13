//! Human prose assertions ignore deliberate terminal line breaks.
//! Keep identifiers, secrets, structured output, statuses and exact
//! result counts separate and check them in their original form.

pub fn prose(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
