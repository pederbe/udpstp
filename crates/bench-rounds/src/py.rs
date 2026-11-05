//! Python's view of JSON and text: [`Json`], the one type every JSON file
//! of a Round is read into, as Python's `json` module reads it, with the
//! operations the Python tooling applies to what it reads; and Python's
//! `splitlines`, `split` and `strip`.
//!
//! The Python tooling is the reference implementation of rules written
//! down before each Round, so the port reads its files as Python does
//!:
//!
//! - a JSON object keeps its document order, as a Python `dict` does: the
//!   Python sums a sampled host's CPUs in `/proc/stat`'s order;
//! - a number with a fraction or an exponent is a float and one without is
//!   an int, as Python keeps them, since a report prints `2` for one and
//!   `2.0` for the other;
//! - floats are parsed exactly, as Python's `float()` parses them;
//! - a key given twice keeps its first place and its last value, as a
//!   `dict` built from the pairs does.
//!
//! What Python raises, a missing key or an operation on the wrong type,
//! is a [`PyError`], which the caller treats as the Python treats the
//! exception.
//!
//! # Where the parser differs from Python's
//!
//! It refuses some text Python reads, and only text no producer writes: the
//! design lets the Rust tool call such a file unreadable (§7.3, §7.7), and
//! the port never reads a value Python reads differently. serde_json
//! refuses `NaN`, `Infinity` and `-Infinity`, numbers beyond a float's
//! range (`1e400`, which Python reads as infinite), escaped lone
//! surrogates, and nesting deeper than 128. And an integer beyond 64 bits,
//! or `-0`, is refused before serde_json sees it, since serde_json would
//! read it as a float where Python reads an int. udpstp, OB-UDPST and the
//! Python tooling write none of these.

use std::fmt;
use std::sync::LazyLock;

use indexmap::IndexMap;
use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};

/// A JSON value as Python holds it.
///
/// Its `PartialEq` says whether two values are the same document: the same
/// kinds, a dict's keys in the same order, floats bit for bit (all NaNs the
/// same). That is stricter than Python's `==`, which takes `1 == 1.0`.
#[derive(Clone, Debug)]
pub enum Json {
    /// Python's `None`: JSON's `null`, and the value of a key `.get` didn't
    /// find.
    Null,
    Bool(bool),
    /// A number written without a fraction or an exponent.
    Int(i128),
    /// A number written with a fraction or an exponent.
    Float(f64),
    Str(String),
    List(Vec<Json>),
    /// A JSON object, its keys in document order.
    Dict(IndexMap<String, Json>),
}

/// `None`, for what `.get` returns when a key is missing.
static NONE: Json = Json::Null;

/// `{}`, for the Python's `x or {}`.
pub static EMPTY_DICT: LazyLock<Json> = LazyLock::new(|| Json::Dict(IndexMap::new()));

/// `[]`, for the Python's `x or []`.
pub static EMPTY_LIST: Json = Json::List(Vec::new());

impl Json {
    /// `json.loads(text)`.
    pub fn parse(text: &str) -> Result<Json, NotJson> {
        if let Some(int) = int_read_as_float(text) {
            return Err(NotJson(format!(
                "the integer {int}, which serde_json would read as a float"
            )));
        }
        serde_json::from_str(text).map_err(|e| NotJson(e.to_string()))
    }

    /// `json.load(f)` of a file opened as text: its bytes as UTF-8, then
    /// [`Json::parse`]. Python refuses bytes that aren't UTF-8 with a
    /// `ValueError`, as it does text that isn't JSON.
    pub fn parse_bytes(bytes: &[u8]) -> Result<Json, NotJson> {
        let text = std::str::from_utf8(bytes).map_err(|e| NotJson(e.to_string()))?;
        Json::parse(text)
    }

    /// Python's name for its type, as its errors say it.
    pub fn type_name(&self) -> &'static str {
        match self {
            Json::Null => "NoneType",
            Json::Bool(_) => "bool",
            Json::Int(_) => "int",
            Json::Float(_) => "float",
            Json::Str(_) => "str",
            Json::List(_) => "list",
            Json::Dict(_) => "dict",
        }
    }

    /// `bool(self)`: false for `None`, `False`, zero, and an empty string,
    /// list or dict. A NaN is true.
    pub fn truthy(&self) -> bool {
        match self {
            Json::Null => false,
            Json::Bool(b) => *b,
            Json::Int(i) => *i != 0,
            Json::Float(f) => *f != 0.0,
            Json::Str(s) => !s.is_empty(),
            Json::List(l) => !l.is_empty(),
            Json::Dict(d) => !d.is_empty(),
        }
    }

    /// `self or other`: `self` if it is true, else `other`.
    pub fn or<'a>(&'a self, other: &'a Json) -> &'a Json {
        if self.truthy() { self } else { other }
    }

    /// `self.get(key)`: the value, or `None` if the dict lacks the key.
    /// Anything but a dict has no `get`.
    pub fn get(&self, key: &str) -> Result<&Json, PyError> {
        self.get_or(key, &NONE)
    }

    /// `self.get(key, default)`.
    pub fn get_or<'a>(&'a self, key: &str, default: &'a Json) -> Result<&'a Json, PyError> {
        match self {
            Json::Dict(d) => Ok(d.get(key).unwrap_or(default)),
            other => Err(PyError::new(
                PyErrorKind::AttributeError,
                format!("'{}' object has no attribute 'get'", other.type_name()),
            )),
        }
    }

    /// `self[key]`, with a string key.
    pub fn item(&self, key: &str) -> Result<&Json, PyError> {
        match self {
            Json::Dict(d) => d
                .get(key)
                .ok_or_else(|| PyError::new(PyErrorKind::KeyError, quoted(key))),
            Json::List(_) => Err(PyError::new(
                PyErrorKind::TypeError,
                "list indices must be integers or slices, not str".to_string(),
            )),
            Json::Str(_) => Err(PyError::new(
                PyErrorKind::TypeError,
                "string indices must be integers, not 'str'".to_string(),
            )),
            other => Err(not_subscriptable(other)),
        }
    }

    /// `self[i]`, with an integer index: a list's item, counted from the
    /// end when negative. A dict's keys are strings, so an integer is never
    /// one of them.
    ///
    /// Python would take a string's `i`-th character; the port refuses it
    /// ([`PyErrorKind::Refused`]), since a string where the Python expects
    /// a list is a garbled file.
    pub fn at(&self, i: i64) -> Result<&Json, PyError> {
        match self {
            Json::List(l) => {
                let len = i64::try_from(l.len()).unwrap_or(i64::MAX);
                let at = if i < 0 { i.checked_add(len) } else { Some(i) };
                at.and_then(|at| usize::try_from(at).ok())
                    .and_then(|at| l.get(at))
                    .ok_or_else(|| {
                        PyError::new(PyErrorKind::IndexError, "list index out of range".into())
                    })
            }
            Json::Dict(_) => Err(PyError::new(PyErrorKind::KeyError, i.to_string())),
            Json::Str(_) => Err(PyError::new(
                PyErrorKind::Refused,
                "a character of a string where a list was expected".to_string(),
            )),
            other => Err(not_subscriptable(other)),
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Json::Null)
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&[Json]> {
        match self {
            Json::List(l) => Some(l),
            _ => None,
        }
    }

    pub fn as_dict(&self) -> Option<&IndexMap<String, Json>> {
        match self {
            Json::Dict(d) => Some(d),
            _ => None,
        }
    }
}

impl PartialEq for Json {
    fn eq(&self, other: &Json) -> bool {
        match (self, other) {
            (Json::Null, Json::Null) => true,
            (Json::Bool(a), Json::Bool(b)) => a == b,
            (Json::Int(a), Json::Int(b)) => a == b,
            (Json::Float(a), Json::Float(b)) => {
                a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan())
            }
            (Json::Str(a), Json::Str(b)) => a == b,
            (Json::List(a), Json::List(b)) => a == b,
            (Json::Dict(a), Json::Dict(b)) => a.iter().eq(b.iter()),
            _ => false,
        }
    }
}

/// The first integer written outside a string that serde_json reads as a
/// float, where Python reads an int: `-0`, or one beyond 64 bits. In
/// valid JSON the scan sees every number, and in text that isn't JSON
/// what it finds is refused anyway.
fn int_read_as_float(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    // An escape's second byte, a quote among them, is text.
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
            }
            b'-' | b'0'..=b'9' => {
                let start = i;
                while i < bytes.len()
                    && matches!(bytes[i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
                {
                    i += 1;
                }
                let token = &text[start..i];
                let int = !token.contains(['.', 'e', 'E']);
                if int
                    && (token == "-0"
                        || token.parse::<i64>().is_err() && token.parse::<u64>().is_err())
                {
                    return Some(token);
                }
            }
            _ => i += 1,
        }
    }
    None
}

/// Whether Python's `json.loads` refuses `text` too, with the `ValueError`
/// the Python's readers catch, where [`Json::parse`] refused it.
///
/// It checks the text against the grammar CPython's `_json` scanner reads:
/// JSON, with `NaN`, `Infinity` and `-Infinity`, numbers of any size and
/// any escaped code point, the port's refusals among them. It answers no
/// where Python reads the text, and where it can't be sure Python refuses
/// it: nesting 100 deep or more, which serde_json refuses from 128 and
/// Python reads far deeper (CPython 3.14 guards its C stack: it read
/// 50,000 levels, and raised a `RecursionError` at 100,000), and an
/// integer of over 4,300 digits, which `int()` refuses. So a reader that
/// takes text both refuse as the Python takes it (a result the Python
/// passes over, say) and refuses the rest never computes from text the
/// Python reads.
pub fn python_also_refuses(text: &str) -> bool {
    python_json(text.as_bytes()) == Some(false)
}

/// Whether CPython 3.14's `json.loads` reads `text`: `Some(true)` if it
/// does, `Some(false)` if it raises its `JSONDecodeError`, `None` if it
/// might fail otherwise.
fn python_json(b: &[u8]) -> Option<bool> {
    /// Nesting from which the port can't read what Python may: serde_json
    /// stops at 128 levels, with a margin.
    const DEEP: usize = 100;
    let space = |i: &mut usize| {
        while b
            .get(*i)
            .is_some_and(|c| matches!(c, b' ' | b'\t' | b'\n' | b'\r'))
        {
            *i += 1;
        }
    };
    // Open containers: true for an object, false for an array.
    let mut open: Vec<bool> = Vec::new();
    let mut i = 0;
    space(&mut i);
    'value: loop {
        // A value at `i`.
        let Some(&first) = b.get(i) else {
            return Some(false);
        };
        match first {
            b'{' | b'[' => {
                if open.len() + 1 >= DEEP {
                    return None;
                }
                let object = first == b'{';
                i += 1;
                space(&mut i);
                if b.get(i) == Some(if object { &b'}' } else { &b']' }) {
                    i += 1;
                } else {
                    open.push(object);
                    if object && !python_key(b, &mut i, &space) {
                        return Some(false);
                    }
                    continue 'value;
                }
            }
            b'"' => {
                if !python_string(b, &mut i) {
                    return Some(false);
                }
            }
            _ => {
                let literal = [
                    &b"null"[..],
                    b"true",
                    b"false",
                    b"NaN",
                    b"Infinity",
                    b"-Infinity",
                ]
                .into_iter()
                .find(|l| b[i..].starts_with(l));
                match literal {
                    Some(l) => i += l.len(),
                    None => match python_number(b, &mut i) {
                        Some(true) => {}
                        Some(false) => return Some(false),
                        None => return None,
                    },
                }
            }
        }
        // After a value: the container it is in goes on or ends.
        loop {
            space(&mut i);
            let Some(&object) = open.last() else {
                return Some(i == b.len());
            };
            match b.get(i) {
                Some(b',') => {
                    i += 1;
                    space(&mut i);
                    if object && !python_key(b, &mut i, &space) {
                        return Some(false);
                    }
                    continue 'value;
                }
                Some(b'}') if object => {
                    open.pop();
                    i += 1;
                }
                Some(b']') if !object => {
                    open.pop();
                    i += 1;
                }
                _ => return Some(false),
            }
        }
    }
}

/// An object's key and its colon at `i`, and the space after.
fn python_key(b: &[u8], i: &mut usize, space: &dyn Fn(&mut usize)) -> bool {
    if b.get(*i) != Some(&b'"') || !python_string(b, i) {
        return false;
    }
    space(i);
    if b.get(*i) != Some(&b':') {
        return false;
    }
    *i += 1;
    space(i);
    true
}

/// A string at `i`, as `_json`'s scanner reads one: no control character
/// raw, the eight short escapes, and `\u` with four hex digits, any code
/// point, a lone surrogate too.
fn python_string(b: &[u8], i: &mut usize) -> bool {
    *i += 1;
    loop {
        match b.get(*i) {
            None => return false,
            Some(b'"') => {
                *i += 1;
                return true;
            }
            Some(b'\\') => match b.get(*i + 1) {
                Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => *i += 2,
                Some(b'u')
                    if b.get(*i + 2..*i + 6)
                        .is_some_and(|h| h.iter().all(u8::is_ascii_hexdigit)) =>
                {
                    *i += 6;
                }
                _ => return false,
            },
            Some(&c) if c < 0x20 => return false,
            Some(_) => *i += 1,
        }
    }
}

/// A number at `i`, as `_json`'s scanner matches one: `-?(0|[1-9][0-9]*)`,
/// then a fraction where a digit follows the point, and an exponent where a
/// digit ends it. `Some(false)` where none starts here; `None` for an
/// integer beyond `int()`'s 4,300 digits.
fn python_number(b: &[u8], i: &mut usize) -> Option<bool> {
    let digits = |i: &mut usize| {
        let start = *i;
        while b.get(*i).is_some_and(u8::is_ascii_digit) {
            *i += 1;
        }
        *i - start
    };
    let start = *i;
    if b.get(*i) == Some(&b'-') {
        *i += 1;
    }
    match b.get(*i) {
        Some(b'0') => *i += 1,
        Some(b'1'..=b'9') => {
            digits(i);
        }
        _ => return Some(false),
    }
    let whole = *i - start;
    let mut float = false;
    if b.get(*i) == Some(&b'.') && b.get(*i + 1).is_some_and(u8::is_ascii_digit) {
        *i += 1;
        digits(i);
        float = true;
    }
    if matches!(b.get(*i), Some(b'e' | b'E')) {
        let at = *i;
        *i += 1;
        if matches!(b.get(*i), Some(b'+' | b'-')) {
            *i += 1;
        }
        if digits(i) > 0 {
            float = true;
        } else {
            *i = at;
        }
    }
    if !float && whole > 4300 {
        return None;
    }
    Some(true)
}

fn not_subscriptable(value: &Json) -> PyError {
    PyError::new(
        PyErrorKind::TypeError,
        format!("'{}' object is not subscriptable", value.type_name()),
    )
}

/// A key as a `KeyError` shows it, near enough: its `repr`.
fn quoted(key: &str) -> String {
    format!("'{}'", key.escape_debug())
}

impl<'de> Deserialize<'de> for Json {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Json, D::Error> {
        deserializer.deserialize_any(JsonVisitor)
    }
}

struct JsonVisitor;

impl<'de> Visitor<'de> for JsonVisitor {
    type Value = Json;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_bool<E>(self, v: bool) -> Result<Json, E> {
        Ok(Json::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Json, E> {
        Ok(Json::Int(v.into()))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Json, E> {
        Ok(Json::Int(v.into()))
    }

    fn visit_i128<E>(self, v: i128) -> Result<Json, E> {
        Ok(Json::Int(v))
    }

    fn visit_u128<E: de::Error>(self, v: u128) -> Result<Json, E> {
        i128::try_from(v)
            .map(Json::Int)
            .map_err(|_| E::custom("an integer beyond 127 bits"))
    }

    fn visit_f64<E>(self, v: f64) -> Result<Json, E> {
        Ok(Json::Float(v))
    }

    fn visit_str<E>(self, v: &str) -> Result<Json, E> {
        Ok(Json::Str(v.to_owned()))
    }

    fn visit_string<E>(self, v: String) -> Result<Json, E> {
        Ok(Json::Str(v))
    }

    fn visit_unit<E>(self) -> Result<Json, E> {
        Ok(Json::Null)
    }

    fn visit_none<E>(self) -> Result<Json, E> {
        Ok(Json::Null)
    }

    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Json, D::Error> {
        Json::deserialize(deserializer)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Json, A::Error> {
        let mut list = Vec::new();
        while let Some(item) = seq.next_element()? {
            list.push(item);
        }
        Ok(Json::List(list))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Json, A::Error> {
        let mut dict = IndexMap::new();
        while let Some((key, value)) = map.next_entry::<String, Json>()? {
            // A key given again keeps its first place and takes the new
            // value, as in a dict built from the pairs.
            dict.insert(key, value);
        }
        Ok(Json::Dict(dict))
    }
}

/// Text that `json.loads` refuses with a `ValueError`: what the Python's
/// `read_json` turns into `None`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotJson(String);

impl fmt::Display for NotJson {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "not JSON: {}", self.0)
    }
}

impl std::error::Error for NotJson {}

/// What Python would raise, as a value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PyError {
    kind: PyErrorKind,
    message: String,
}

impl PyError {
    pub fn new(kind: PyErrorKind, message: String) -> PyError {
        PyError { kind, message }
    }

    pub fn kind(&self) -> PyErrorKind {
        self.kind
    }
}

impl fmt::Display for PyError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}: {}", self.kind.name(), self.message)
    }
}

impl std::error::Error for PyError {}

/// The exceptions the port raises as values, by Python's class names, and
/// one of the port's own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PyErrorKind {
    KeyError,
    TypeError,
    AttributeError,
    IndexError,
    /// Where Python computes something from a garbled file and the port
    /// refuses the file instead, as the design allows (§7.7).
    Refused,
}

impl PyErrorKind {
    /// The Python exception's class name, or "Refused".
    pub fn name(self) -> &'static str {
        match self {
            PyErrorKind::KeyError => "KeyError",
            PyErrorKind::TypeError => "TypeError",
            PyErrorKind::AttributeError => "AttributeError",
            PyErrorKind::IndexError => "IndexError",
            PyErrorKind::Refused => "Refused",
        }
    }
}

/// `c.isspace()`: Python's whitespace, which `split()`, `strip()` and the
/// regular expressions' `\s` use. Beyond Unicode's White_Space (Rust's
/// `char::is_whitespace`), it takes the four information separators
/// U+001C to U+001F.
pub fn is_space(c: char) -> bool {
    matches!(
        c,
        '\u{9}'..='\u{d}'
            | '\u{1c}'..='\u{20}'
            | '\u{85}'
            | '\u{a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
    )
}

/// Whether `c` ends a line for `splitlines()`: besides `\n` and `\r`, the
/// vertical tab, the form feed, U+001C to U+001E, U+0085, and the Unicode
/// line and paragraph separators.
fn is_line_break(c: char) -> bool {
    matches!(
        c,
        '\n' | '\u{b}'
            | '\u{c}'
            | '\r'
            | '\u{1c}'
            | '\u{1d}'
            | '\u{1e}'
            | '\u{85}'
            | '\u{2028}'
            | '\u{2029}'
    )
}

/// `text.splitlines()`: the lines, without their breaks. `\r\n` is one
/// break, and a break at the end starts no empty line.
pub fn splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if is_line_break(c) {
            lines.push(&text[start..i]);
            let mut end = i + c.len_utf8();
            if c == '\r' && chars.next_if(|&(_, n)| n == '\n').is_some() {
                end += 1;
            }
            start = end;
        }
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// `text.split()`: the words between runs of Python's whitespace
/// ([`is_space`]), none empty. Python's `split(sep)` with a separator is
/// Rust's `str::split`.
pub fn split(text: &str) -> Vec<&str> {
    text.split(is_space).filter(|w| !w.is_empty()).collect()
}

/// `text.strip()`: without Python's whitespace ([`is_space`]) at either
/// end.
pub fn strip(text: &str) -> &str {
    text.trim_matches(is_space)
}

/// The lines of a text file as Python iterates one opened as text, each
/// without its line break: universal newlines break only at `\n`, `\r\n`
/// and `\r`, never at the other breaks [`splitlines`] takes, which JSON
/// strings may hold raw (serde_json writes U+2028 unescaped). A break at
/// the end starts no empty line. Python's lines end with `\n`, which
/// `json.loads` and `strip` don't see.
pub fn file_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                lines.push(&text[start..i]);
                start = i + 1;
            }
            b'\r' => {
                lines.push(&text[start..i]);
                if bytes.get(i + 1) == Some(&b'\n') {
                    i += 1;
                }
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// `json.dumps(value)`: `", "` between items and `": "` after keys, keys
/// in the dict's order, everything but printable ASCII escaped, floats as
/// Python's `repr` writes them ([`num::repr`](crate::num::repr)), and NaN
/// and the infinities as `NaN`, `Infinity` and `-Infinity`.
pub fn dumps(value: &Json) -> String {
    let mut out = String::new();
    write_json(value, None, 0, &mut out);
    out
}

/// `json.dumps(value, indent=indent)`, which `json.dump(value, f,
/// indent=indent)` writes too: each item on a line of its own, indented
/// `indent` spaces a level, `","` at the ends of lines and `": "` after
/// keys; an empty list or dict as `[]` or `{}`; no line break at the end.
pub fn dumps_indent(value: &Json, indent: usize) -> String {
    let mut out = String::new();
    write_json(value, Some(indent), 0, &mut out);
    out
}

fn write_json(value: &Json, indent: Option<usize>, level: usize, out: &mut String) {
    // Where an item starts: after a line break and its indent, or after a
    // space.
    let item_start = |out: &mut String, level: usize| {
        if let Some(n) = indent {
            out.push('\n');
            out.extend(std::iter::repeat_n(' ', n * level));
        }
    };
    let separator = if indent.is_some() { "," } else { ", " };
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Json::Int(i) => out.push_str(&i.to_string()),
        Json::Float(f) => out.push_str(&float_json(*f)),
        Json::Str(s) => write_json_str(s, out),
        Json::List(items) if items.is_empty() => out.push_str("[]"),
        Json::List(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(separator);
                }
                item_start(out, level + 1);
                write_json(item, indent, level + 1, out);
            }
            item_start(out, level);
            out.push(']');
        }
        Json::Dict(dict) if dict.is_empty() => out.push_str("{}"),
        Json::Dict(dict) => {
            out.push('{');
            for (i, (key, item)) in dict.iter().enumerate() {
                if i > 0 {
                    out.push_str(separator);
                }
                item_start(out, level + 1);
                write_json_str(key, out);
                out.push_str(": ");
                write_json(item, indent, level + 1, out);
            }
            item_start(out, level);
            out.push('}');
        }
    }
}

/// A float as `json.dumps` writes it.
fn float_json(f: f64) -> String {
    if f.is_nan() {
        "NaN".to_string()
    } else if f.is_infinite() {
        if f > 0.0 { "Infinity" } else { "-Infinity" }.to_string()
    } else {
        crate::num::repr(f)
    }
}

/// A string as `json.dumps` writes it with `ensure_ascii`: in double
/// quotes, `"` and `\` escaped, the five short escapes for `\b`, `\f`,
/// `\n`, `\r` and `\t`, and every other character outside `' '..='~'` as
/// `\u` and four lowercase hex digits, those beyond U+FFFF as a surrogate
/// pair.
fn write_json_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            ' '..='~' => out.push(c),
            _ => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(text: &str) -> Json {
        Json::parse(text).unwrap()
    }

    #[test]
    fn keeps_ints_and_floats_apart() {
        assert_eq!(json("2"), Json::Int(2));
        assert_eq!(json("2.0"), Json::Float(2.0));
        assert_eq!(json("2e0"), Json::Float(2.0));
        assert_eq!(json("-3"), Json::Int(-3));
        assert_eq!(json("18446744073709551615"), Json::Int(u64::MAX.into()));
        assert_eq!(json("-9223372036854775808"), Json::Int(i64::MIN.into()));
        assert_ne!(json("2"), json("2.0"));
    }

    #[test]
    fn parses_floats_exactly() {
        // 17 significant digits, as time.time() is written: the nearest
        // double, which Python's float() gives too.
        let text = "1790000000.1234567";
        assert_eq!(json(text), Json::Float(text.parse::<f64>().unwrap()));
        assert_eq!(json("-0.0"), Json::Float(-0.0));
        assert_ne!(json("-0.0"), Json::Float(0.0));
        assert_eq!(json("1e-400"), Json::Float(0.0));
    }

    #[test]
    fn keeps_document_order_and_the_last_duplicate_in_the_first_place() {
        let d = json(r#"{"10": 1, "2": 2, "10": 3}"#);
        let keys: Vec<&str> = d.as_dict().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, ["10", "2"]);
        assert_eq!(d.item("10"), Ok(&Json::Int(3)));
        // Order is part of being the same document.
        assert_ne!(json(r#"{"a": 1, "b": 2}"#), json(r#"{"b": 2, "a": 1}"#));
    }

    #[test]
    fn refuses_what_json_loads_refuses() {
        for text in [
            "",
            " ",
            "{",
            "[1,]",
            r#"{"a":1,}"#,
            "'a'",
            "01",
            "1.",
            ".5",
            "+1",
            "-",
            "[1] 2",
            "\"\u{1}\"",
            r#""\x""#,
            "\u{feff}{}",
            "nul",
            "True",
            "{1: 2}",
        ] {
            assert!(Json::parse(text).is_err(), "{text:?}");
        }
        assert!(Json::parse_bytes(b"\"\xff\"").is_err());
        assert_eq!(
            Json::parse(" \t\r\n[1]\n "),
            Ok(Json::List(vec![Json::Int(1)]))
        );
    }

    #[test]
    fn refuses_what_no_producer_writes_but_python_reads() {
        // Python reads these as nan, inf and inf; the port calls the file
        // unreadable.
        for text in ["NaN", "Infinity", "-Infinity", "1e400", r#""\ud800""#] {
            assert!(Json::parse(text).is_err(), "{text:?}");
        }
        // Python reads ints, serde_json would read floats: refused too.
        for text in [
            "-0",
            "[1, -0]",
            "18446744073709551616",
            "-9223372036854775809",
        ] {
            assert!(Json::parse(text).is_err(), "{text:?}");
        }
        // The same characters in a string, or as a float, are read.
        assert_eq!(
            json(r#"["-0", "18446744073709551616", "\"-0", -0.0, -0e1]"#),
            Json::List(vec![
                Json::Str("-0".into()),
                Json::Str("18446744073709551616".into()),
                Json::Str("\"-0".into()),
                Json::Float(-0.0),
                Json::Float(-0.0),
            ])
        );
    }

    #[test]
    fn says_where_python_refuses_json_too() {
        // Text neither reads: damage a run can leave, a Client's error.
        for text in [
            "",
            " ",
            "{",
            "[1,]",
            "{\"a\": 1,}",
            "{\"a\": 1,",
            "\"abc",
            "-",
            "1-2",
            "01",
            "1.",
            ".5",
            "1e",
            "[1] 2",
            "tru",
            "{\"Output\": {\"Incre",
            "{\"a\": \"\\u12\"}",
            "\"\\x\"",
            "\"\u{1}\"",
            "\u{feff}{}",
            "{NaN: 1}",
            "not JSON",
            "ERROR: Invalid Infinity",
            "nan",
            "-infinity",
            "[\"a\" \"b\"]",
            "{\"a\" 1}",
            "{\"a\": 1 \"b\": 2}",
        ] {
            assert!(Json::parse(text).is_err(), "{text:?}");
            assert!(python_also_refuses(text), "{text:?}");
        }
        // Text Python reads, or may fail on otherwise: what the port
        // refuses, and nesting deep enough for a RecursionError.
        let deep = "[".repeat(200) + &"]".repeat(200);
        let deep_and_broken = "[".repeat(1100);
        let long_int = "9".repeat(4301);
        for text in [
            "NaN",
            "[1, -Infinity]",
            " Infinity ",
            "1e400",
            "[-1e999]",
            "18446744073709551616",
            "-0",
            r#""\ud800""#,
            r#"{"a": "\uDC00x"}"#,
            &deep,
            &deep_and_broken,
            &long_int,
            "{\"a\": NaN}",
        ] {
            assert!(!python_also_refuses(text), "{text:?}");
        }
        // Python reads what the port does too, and refuses the rest.
        for text in [
            "{}",
            "[]",
            "[1, 2.5e3, \"\\u00e9\", null, true, false]",
            "  {\"a\": {\"b\": []}}\n",
        ] {
            assert!(!python_also_refuses(text), "{text:?}");
        }
        assert!(python_also_refuses(r#"{"NaN": "-0 1e400 Infinity", "x"#));
    }

    #[test]
    fn decodes_escapes() {
        // The backslash of each 4-digit escape is joined on, so the escape
        // reaches the parser as text.
        let b = '\\';
        let text = format!(r#""a\"b\\c\/d\b\f\n\r\t{b}u00e9{b}u00E9{b}ud83d{b}ude00""#);
        assert!(text.is_ascii());
        assert_eq!(
            json(&text),
            Json::Str("a\"b\\c/d\u{8}\u{c}\n\r\téé😀".into())
        );
    }

    #[test]
    fn get_and_item_follow_python() {
        let d = json(r#"{"a": 1, "b": null, "c": [10, 20, 30]}"#);
        assert_eq!(d.get("a"), Ok(&Json::Int(1)));
        assert_eq!(d.get("b"), Ok(&Json::Null));
        assert_eq!(d.get("z"), Ok(&Json::Null));
        assert_eq!(d.get_or("z", &Json::Int(0)), Ok(&Json::Int(0)));
        // A present None isn't replaced by the default.
        assert_eq!(d.get_or("b", &Json::Int(0)), Ok(&Json::Null));
        assert_eq!(d.item("z").unwrap_err().kind(), PyErrorKind::KeyError);
        let c = d.item("c").unwrap();
        assert_eq!(c.at(0), Ok(&Json::Int(10)));
        assert_eq!(c.at(-1), Ok(&Json::Int(30)));
        assert_eq!(c.at(-3), Ok(&Json::Int(10)));
        assert_eq!(c.at(3).unwrap_err().kind(), PyErrorKind::IndexError);
        assert_eq!(c.at(-4).unwrap_err().kind(), PyErrorKind::IndexError);
        assert_eq!(c.get("a").unwrap_err().kind(), PyErrorKind::AttributeError);
        assert_eq!(c.item("a").unwrap_err().kind(), PyErrorKind::TypeError);
        assert_eq!(d.at(0).unwrap_err().kind(), PyErrorKind::KeyError);
        assert_eq!(
            Json::Null.item("a").unwrap_err().kind(),
            PyErrorKind::TypeError
        );
        assert_eq!(
            Json::Int(1).at(0).unwrap_err().kind(),
            PyErrorKind::TypeError
        );
        assert_eq!(
            Json::Str("ab".into()).item("a").unwrap_err().kind(),
            PyErrorKind::TypeError
        );
        assert_eq!(
            Json::Str("ab".into()).at(0).unwrap_err().kind(),
            PyErrorKind::Refused
        );
        assert_eq!(
            Json::Null.get("a").unwrap_err().to_string(),
            "AttributeError: 'NoneType' object has no attribute 'get'"
        );
    }

    #[test]
    fn truthiness_and_or_follow_python() {
        let falsy = [
            Json::Null,
            Json::Bool(false),
            Json::Int(0),
            Json::Float(0.0),
            Json::Float(-0.0),
            Json::Str(String::new()),
            Json::List(vec![]),
            Json::Dict(IndexMap::new()),
        ];
        for v in &falsy {
            assert!(!v.truthy(), "{v:?}");
            assert_eq!(v.or(&EMPTY_DICT), &*EMPTY_DICT);
        }
        let truthy = [
            Json::Bool(true),
            Json::Int(-1),
            Json::Float(f64::NAN),
            Json::Str("0".into()),
            Json::List(vec![Json::Null]),
            json(r#"{"": null}"#),
        ];
        for v in &truthy {
            assert!(v.truthy(), "{v:?}");
            assert_eq!(v.or(&EMPTY_LIST), v);
        }
    }

    #[test]
    fn python_whitespace_takes_the_information_separators() {
        for c in ['\u{1c}', '\u{1d}', '\u{1e}', '\u{1f}'] {
            assert!(is_space(c) && !c.is_whitespace());
        }
        // Everything else is Unicode's White_Space, Rust's own.
        for c in (0..=0x10ffff).filter_map(char::from_u32) {
            if !('\u{1c}'..='\u{1f}').contains(&c) {
                assert_eq!(is_space(c), c.is_whitespace(), "{:x}", c as u32);
            }
        }
    }

    #[test]
    fn splitlines_follows_python() {
        assert_eq!(splitlines(""), Vec::<&str>::new());
        assert_eq!(splitlines("a"), ["a"]);
        assert_eq!(splitlines("a\n"), ["a"]);
        assert_eq!(splitlines("a\n\n"), ["a", ""]);
        assert_eq!(splitlines("\n"), [""]);
        assert_eq!(splitlines("a\r\nb\rc\nd"), ["a", "b", "c", "d"]);
        assert_eq!(splitlines("\r\r\n"), ["", ""]);
        assert_eq!(
            splitlines("a\u{b}b\u{c}c\u{1c}d\u{1d}e\u{1e}f\u{85}g\u{2028}h\u{2029}i"),
            ["a", "b", "c", "d", "e", "f", "g", "h", "i"]
        );
        // U+001F is whitespace but no line break.
        assert_eq!(splitlines("a\u{1f}b"), ["a\u{1f}b"]);
    }

    #[test]
    fn file_lines_break_only_where_a_text_file_does() {
        assert_eq!(file_lines(""), Vec::<&str>::new());
        assert_eq!(file_lines("a"), ["a"]);
        assert_eq!(file_lines("a\n"), ["a"]);
        assert_eq!(file_lines("\n"), [""]);
        assert_eq!(file_lines("a\n\nb"), ["a", "", "b"]);
        assert_eq!(file_lines("a\r\nb\rc\nd\r"), ["a", "b", "c", "d"]);
        assert_eq!(file_lines("\r\r\n"), ["", ""]);
        // The breaks splitlines takes besides these stay inside a line.
        assert_eq!(
            file_lines("{\"a\": \"x\u{2028}y\u{85}z\u{b}\u{c}\u{1c}\"}\n"),
            ["{\"a\": \"x\u{2028}y\u{85}z\u{b}\u{c}\u{1c}\"}"]
        );
    }

    #[test]
    fn dumps_writes_as_json_dumps_does() {
        let b = '\\';
        let doc = json(r#"{"b": [1, 2.0, -0.0, 1e16, true, null], "a": {}, "": [], "s": "x"}"#);
        assert_eq!(
            dumps(&doc),
            r#"{"b": [1, 2.0, -0.0, 1e+16, true, null], "a": {}, "": [], "s": "x"}"#
        );
        assert_eq!(
            dumps_indent(&doc, 1),
            "{\n \"b\": [\n  1,\n  2.0,\n  -0.0,\n  1e+16,\n  true,\n  null\n ],\n \"a\": {},\n \"\": [],\n \"s\": \"x\"\n}"
        );
        assert_eq!(dumps_indent(&Json::List(vec![]), 1), "[]");
        assert_eq!(dumps(&Json::Float(f64::NAN)), "NaN");
        assert_eq!(dumps(&Json::Float(f64::NEG_INFINITY)), "-Infinity");
        // Everything outside printable ASCII escaped, lowercase, a pair
        // beyond U+FFFF; DEL too.
        let text = "\"\\/\n\r\t\u{8}\u{c}\u{1}\u{1f}\u{7f}é\u{2028}😀~ ";
        assert_eq!(
            dumps(&Json::Str(text.into())),
            format!(
                "\"{b}\"{b}{b}/{b}n{b}r{b}t{b}b{b}f{b}u0001{b}u001f{b}u007f{b}u00e9{b}u2028{b}ud83d{b}ude00~ \""
            )
        );
        // What it writes reads back as the same document.
        let written = dumps(&doc);
        assert_eq!(json(&written), doc);
    }

    #[test]
    fn split_and_strip_follow_python() {
        assert_eq!(split(""), Vec::<&str>::new());
        assert_eq!(split("  \t\n "), Vec::<&str>::new());
        assert_eq!(
            split(" Udp: 1  2\u{a0}3\u{1f}4 "),
            ["Udp:", "1", "2", "3", "4"]
        );
        assert_eq!(strip("\u{1c} x y \u{3000}"), "x y");
        assert_eq!(strip("\u{200b}x"), "\u{200b}x");
    }
}
