//! Key files (docs/spec.md, "Key table"): our TOML Key Table, or an OB-UDPST
//! `udpst.keys` file. A file whose name ends in `.toml` is a TOML Key Table;
//! any other is read as OB-UDPST's format.
//!
//! Keys never appear in errors or output: errors name the file, the line
//! and what was wrong, in our own words. The file's bytes are wiped from
//! memory once parsed; the TOML parser's own copies are beyond our reach.
//!
//! Recorded choices:
//! - lifetimes are written `YYYYMMDDHHMMSSZ` (conformance: key-lifetime-format);
//! - `key_text` is an extension, and `key_hex` accepts either case
//!   (conformance: key-text-extension);
//! - the KDF and AlgID columns are `HMAC-SHA-256`, and the Operator marker
//!   and `max_test_interval` extend RFC 7210
//!   (conformance: key-table-alg-id, key-table-operator-extension);
//! - OB-UDPST files are read like OB-UDPST reads them, with two exceptions
//!   (conformance: obudpst-key-file-reading);
//! - a key file group or others can read or write is refused on Unix, and
//!   Windows policy mismatches or unverified permissions are refused
//!   (ADR 0015; conformance: key-file-private);
//! - on Unix a key file must be owned by the user running udpstp or root
//!   (conformance: key-file-owner);
//! - a key file over 1 MiB is refused (conformance: key-file-size-limit).
//!
//! Unknown TOML fields are refused rather than silently dropping a key's
//! restrictions. The redundant top-level `format = 1` marker is deprecated
//! but still accepted during its minor-release window (ADR 0016).

use std::collections::HashSet;
use std::fmt;
use std::io::Read as _;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use udpstp_core::{AlgId, Kdf, KeyEntry, KeyRole, KeyTable, KeyTableError, Lifetime};
use zeroize::Zeroizing;

/// OB-UDPST's longest key, in bytes (`AUTH_KEY_SIZE`).
const OBUDPST_MAX_KEY_LEN: usize = 64;

/// The largest key file read, far above any real Key Table.
const MAX_KEY_FILE_LEN: u64 = 1024 * 1024;

/// Why a key file can't be used.
#[derive(Debug)]
pub struct KeyFileError {
    path: String,
    line: Option<usize>,
    message: String,
}

impl fmt::Display for KeyFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(line) => write!(f, "{}, line {line}: {}", self.path, self.message),
            None => write!(f, "{}: {}", self.path, self.message),
        }
    }
}

/// A validated Key Table and its deprecated input marker, if present.
pub struct LoadedKeyTable {
    pub table: KeyTable,
    pub format_deprecated: bool,
}

/// Loads the Key Table from `path`, inspecting and reading the same file.
pub fn load(path: &Path) -> Result<LoadedKeyTable, KeyFileError> {
    let error = |line: Option<usize>, message: String| KeyFileError {
        path: path.display().to_string(),
        line,
        message,
    };
    #[cfg(windows)]
    let mut file = udpstp_key_permissions::open(path).map_err(|e| error(None, e.to_string()))?;
    #[cfg(not(windows))]
    let mut file = std::fs::File::open(path).map_err(|e| error(None, e.to_string()))?;
    let metadata = file.metadata().map_err(|e| error(None, e.to_string()))?;
    if !metadata.is_file() {
        return Err(error(None, "not a regular file".into()));
    }
    check_private(&metadata).map_err(|message| error(None, message))?;
    check_windows_private(&file).map_err(|message| error(None, message))?;
    if metadata.len() > MAX_KEY_FILE_LEN {
        return Err(error(
            None,
            format!("larger than {MAX_KEY_FILE_LEN} bytes; not a key file"),
        ));
    }
    let bytes = read_whole(&mut file, metadata.len() as usize).map_err(|m| error(None, m))?;
    let is_toml = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"));
    let parsed = if is_toml {
        parse_toml(&bytes)
    } else {
        parse_obudpst(&bytes).map_err(|(line, message)| {
            (
                line,
                format!("{message} (read as an OB-UDPST key file; a TOML Key Table's name ends in .toml)"),
            )
        })
    }
    .map_err(|(line, message)| error(line, message))?;
    if parsed.entries.is_empty() {
        return Err(error(None, "no keys".into()));
    }
    let mut table = KeyTable::default();
    for (line, entry) in parsed.entries {
        table.insert(entry).map_err(|e| {
            let message = match e {
                KeyTableError::OverlappingLifetimes { key_id, names } => format!(
                    "keyId {key_id}: the lifetimes of {:?} and {:?} overlap",
                    names.0, names.1
                ),
                KeyTableError::EmptyLifetime { name } => {
                    format!("{name:?}: a lifetime ends before it starts")
                }
            };
            error(line, message)
        })?;
    }
    Ok(LoadedKeyTable {
        table,
        format_deprecated: parsed.format_deprecated,
    })
}

/// The supported policy is a startup assessment, not effective-access
/// analysis (ADR 0015; conformance: key-file-private). Refuse before reading.
#[cfg(windows)]
fn check_windows_private(file: &std::fs::File) -> Result<(), String> {
    use udpstp_key_permissions::Assessment;
    match udpstp_key_permissions::assess(file) {
        Assessment::Meets => Ok(()),
        Assessment::Mismatch(reason) => Err(format!(
            "Shared Key file does not meet the supported Windows private-file policy: {reason}; file refused; reconstruct private permissions as described in docs/guides/windows.md, Key file permissions"
        )),
        Assessment::Unverified(reason) => Err(format!(
            "Shared Key file permissions could not be verified: {reason}; file refused; use a local NTFS file with the supported permissions described in docs/guides/windows.md, Key file permissions"
        )),
    }
}

#[cfg(not(windows))]
fn check_windows_private(_file: &std::fs::File) -> Result<(), String> {
    Ok(())
}

/// Reads the file's `len` bytes into one buffer that is never
/// reallocated, so no unwiped copy of the key bytes is left behind. A file
/// that grew or shrank since its metadata was read is refused.
fn read_whole(file: &mut std::fs::File, len: usize) -> Result<Zeroizing<Vec<u8>>, String> {
    // One byte more, to see a file that grew.
    let mut bytes = Zeroizing::new(vec![0u8; len + 1]);
    let mut filled = 0;
    while filled < bytes.len() {
        match file.read(&mut bytes[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    if filled != len {
        return Err("changed while it was read; try again".into());
    }
    // Shortening keeps the allocation, which Zeroizing wipes whole.
    bytes.truncate(filled);
    Ok(bytes)
}

/// Refuses a key file that group or others can read, or write and so
/// replace the keys (docs/spec.md asks for "readable"), or that someone other
/// than the user running udpstp (or root) owns and so could replace.
#[cfg(unix)]
fn check_private(metadata: &std::fs::Metadata) -> Result<(), String> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let mode = metadata.permissions().mode();
    if mode & 0o066 != 0 {
        return Err(format!(
            "readable or writable by group or others (mode {:03o}); make it private, e.g. chmod 600",
            mode & 0o777
        ));
    }
    owner_allowed(metadata.uid(), nix::unistd::geteuid().as_raw())
}

/// A key file must be owned by the user running udpstp, or by root.
#[cfg(unix)]
fn owner_allowed(owner: u32, user: u32) -> Result<(), String> {
    if owner == user || owner == 0 {
        return Ok(());
    }
    Err(format!(
        "owned by another user (uid {owner}), who could replace the keys; it must be owned by the user running udpstp (uid {user}) or root"
    ))
}

/// Windows permissions are assessed on the opened file above.
#[cfg(not(unix))]
fn check_private(_metadata: &std::fs::Metadata) -> Result<(), String> {
    Ok(())
}

struct ParsedKeyFile {
    entries: Vec<(Option<usize>, KeyEntry)>,
    format_deprecated: bool,
}

type Parsed = Result<ParsedKeyFile, (Option<usize>, String)>;

/// OB-UDPST's format: `<keyId>,<key>` per line, with commas, spaces and
/// tabs separating fields. A line ends at `#`, `\r` or a NUL byte. The keyId
/// is decimal, `0x` hexadecimal or `0` octal, as C's `strtol` reads it; the
/// key is its bytes, up to 64. Anything after the key is ignored, as
/// OB-UDPST does. The rows have no lifetimes, so they are valid at all
/// times (ADR 0010).
fn parse_obudpst(bytes: &[u8]) -> Parsed {
    let mut entries: Vec<(Option<usize>, KeyEntry)> = Vec::new();
    for (index, line) in bytes.split(|&b| b == b'\n').enumerate() {
        let number = Some(index + 1);
        let content = line
            .split(|&b| matches!(b, b'#' | b'\r' | 0))
            .next()
            .unwrap_or_default();
        let mut fields = content
            .split(|&b| matches!(b, b',' | b' ' | b'\t'))
            .filter(|field| !field.is_empty());
        let Some(id) = fields.next() else {
            continue;
        };
        let key = fields
            .next()
            .ok_or((number, "expected <keyId>,<key>".to_string()))?;
        let key_id = std::str::from_utf8(id)
            .ok()
            .and_then(parse_c_integer)
            .and_then(|id| u8::try_from(id).ok())
            .ok_or((
                number,
                "the keyId is not a number from 0 to 255".to_string(),
            ))?;
        if entries.iter().any(|(_, e)| e.key_id == key_id) {
            return Err((number, format!("keyId {key_id} appears twice")));
        }
        if key.len() > OBUDPST_MAX_KEY_LEN {
            return Err((
                number,
                format!("the key is longer than {OBUDPST_MAX_KEY_LEN} bytes"),
            ));
        }
        let name = format!("keyId {key_id}");
        let entry = KeyEntry::new(
            &name,
            key_id,
            key.to_vec(),
            Lifetime::ALWAYS,
            Lifetime::ALWAYS,
        );
        entries.push((number, entry));
    }
    Ok(ParsedKeyFile {
        entries,
        format_deprecated: false,
    })
}

/// A non-negative integer as C's `strtol` with base 0 reads a whole token.
fn parse_c_integer(text: &str) -> Option<u64> {
    let text = text.strip_prefix('+').unwrap_or(text);
    let (digits, radix) =
        if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
            (hex, 16)
        } else if text.len() > 1 && text.starts_with('0') {
            (&text[1..], 8)
        } else {
            (text, 10)
        };
    if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    u64::from_str_radix(digits, radix).ok()
}

/// Our TOML Key Table: one `[[key]]` table per row (RFC 7210 §2), plus the
/// Operator extension (ADR 0010).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TomlKeyTable {
    /// Deprecated compatibility input, accepted only when it is 1 (ADR 0016).
    format: Option<i64>,
    #[serde(default)]
    key: Vec<TomlKey>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TomlKey {
    /// AdminKeyName: unique in the file.
    admin_key_name: String,
    /// LocalKeyName: the keyId in the PDUs.
    key_id: u8,
    kdf: Option<String>,
    alg_id: Option<String>,
    /// The key as text, used as its UTF-8 bytes.
    key_text: Option<Zeroizing<String>>,
    /// The key as hexadecimal digits.
    key_hex: Option<Zeroizing<String>>,
    send_lifetime_start: Option<String>,
    send_lifetime_end: Option<String>,
    accept_lifetime_start: Option<String>,
    accept_lifetime_end: Option<String>,
    /// Extension: an Operator Key.
    #[serde(default)]
    operator: bool,
    /// Extension: an Operator Key's maximum Test Interval, in seconds.
    max_test_interval: Option<u64>,
    /// Extension: this row's Quota per the Server's Quota Period.
    tests_per_quota_period: Option<u32>,
    bytes_per_quota_period: Option<u64>,
}

fn parse_toml(bytes: &[u8]) -> Parsed {
    let text = std::str::from_utf8(bytes).map_err(|_| (None, "not UTF-8 text".to_string()))?;
    let at = |e: toml::de::Error| {
        let start = e.span().map_or(0, |span| span.start.min(text.len()));
        let line = text[..start].matches('\n').count() + 1;
        (Some(line), toml_error_message(e.message()))
    };
    let table: TomlKeyTable = toml::from_str(text).map_err(at)?;
    if table.format.is_some_and(|format| format != 1) {
        return Err((None, "format must be 1".into()));
    }
    let mut names = HashSet::new();
    let entries = table
        .key
        .into_iter()
        .map(|row| {
            let name = row.admin_key_name.clone();
            if name.is_empty() || !names.insert(name.clone()) {
                return Err((
                    None,
                    format!("admin_key_name {name:?} is empty or not unique"),
                ));
            }
            toml_entry(row)
                .map(|entry| (None, entry))
                .map_err(|m| (None, format!("{name:?}: {m}")))
        })
        .collect::<Result<_, _>>()?;
    Ok(ParsedKeyFile {
        entries,
        format_deprecated: table.format.is_some(),
    })
}

/// toml's messages can quote a mistyped value, which may be a key, and its
/// error display quotes the whole line. Only messages that name fields are
/// passed on.
fn toml_error_message(message: &str) -> String {
    let names_a_field = ["unknown field", "missing field", "duplicate key"]
        .iter()
        .any(|prefix| message.starts_with(prefix));
    if names_a_field {
        message.trim_end().to_string()
    } else {
        "not valid TOML, or a value of the wrong type".to_string()
    }
}

fn toml_entry(row: TomlKey) -> Result<KeyEntry, String> {
    let key = match (&row.key_text, &row.key_hex) {
        (Some(text), None) => text.as_bytes().to_vec(),
        (None, Some(hex)) => {
            decode_hex(hex).ok_or("key_hex is not an even number of hex digits")?
        }
        _ => return Err("give exactly one of key_text and key_hex".into()),
    };
    if key.is_empty() {
        return Err("the key is empty".into());
    }
    if row
        .kdf
        .as_deref()
        .is_some_and(|kdf| kdf != Kdf::HmacSha256.as_str())
    {
        return Err(format!("kdf must be {:?}", Kdf::HmacSha256.as_str()));
    }
    if row
        .alg_id
        .as_deref()
        .is_some_and(|alg| alg != AlgId::HmacSha256.as_str())
    {
        return Err(format!("alg_id must be {:?}", AlgId::HmacSha256.as_str()));
    }
    let time = |field: &str, value: &Option<String>| -> Result<Option<u64>, String> {
        value
            .as_deref()
            .map(|v| {
                Lifetime::parse_time(v).ok_or(format!("{field} is not a YYYYMMDDHHMMSSZ time"))
            })
            .transpose()
    };
    let send = Lifetime::new(
        time("send_lifetime_start", &row.send_lifetime_start)?,
        time("send_lifetime_end", &row.send_lifetime_end)?,
    );
    let accept = Lifetime::new(
        time("accept_lifetime_start", &row.accept_lifetime_start)?,
        time("accept_lifetime_end", &row.accept_lifetime_end)?,
    );
    let mut entry = KeyEntry::new(&row.admin_key_name, row.key_id, key, send, accept);
    entry.role = match (row.operator, row.max_test_interval) {
        (false, None) => KeyRole::Consumer,
        (false, Some(_)) => return Err("max_test_interval needs operator = true".into()),
        (true, Some(0..=59)) => {
            return Err(
                "max_test_interval must be at least 60 s: every key allows Tests up to 60 s".into(),
            );
        }
        (true, max) => KeyRole::Operator {
            max_test_interval: max.map(Duration::from_secs),
        },
    };
    // Extension: the row's Quota. An Operator Key is exempt, so it
    // has none.
    if row.tests_per_quota_period == Some(0) || row.bytes_per_quota_period == Some(0) {
        return Err("a Quota must be at least 1; leave it out for none".into());
    }
    let quota = udpstp_core::QuotaLimits {
        tests: row.tests_per_quota_period,
        bytes: row.bytes_per_quota_period,
    };
    if row.operator && quota.is_limited() {
        return Err("an Operator Key has no Quota".into());
    }
    entry.quota = quota;
    Ok(entry)
}

fn decode_hex(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut key = Zeroizing::new(Vec::with_capacity(hex.len() / 2));
    for i in (0..hex.len()).step_by(2) {
        key.push(u8::from_str_radix(&hex[i..i + 2], 16).ok()?);
    }
    Some(std::mem::take(&mut *key))
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::read_whole;

    const KEYS: &[u8] = b"1,s3cr3t\n";

    //  (key bytes are read into one buffer that is never
    // reallocated; a file whose length changed since its metadata was read
    // is refused)
    #[test]
    fn a_key_file_is_read_into_one_buffer() {
        let path = std::env::temp_dir().join(format!("udpstp-read-whole-{}", std::process::id()));
        std::fs::File::create(&path)
            .and_then(|mut f| f.write_all(KEYS))
            .unwrap();
        let open = || std::fs::File::open(&path).unwrap();

        let read = read_whole(&mut open(), 9);
        let grew = read_whole(&mut open(), 8);
        let shrank = read_whole(&mut open(), 10);
        std::fs::remove_file(&path).unwrap();

        let bytes = read.unwrap();
        assert_eq!((&bytes[..], bytes.capacity()), (KEYS, 10));
        for changed in [grew, shrank] {
            let message = changed.unwrap_err();
            assert!(message.contains("changed"), "{message}");
        }
    }

    /// The field names a struct's `Deserialize` reads, as serde passes
    /// them to the deserializer: the names a file writes.
    fn columns<'de, T: serde::Deserialize<'de>>() -> &'static [&'static str] {
        use serde::de::{Error as _, Visitor, value::Error};

        struct Introspect<'a>(&'a mut &'static [&'static str]);

        impl<'de> serde::Deserializer<'de> for Introspect<'_> {
            type Error = Error;

            fn deserialize_any<V: Visitor<'de>>(self, _: V) -> Result<V::Value, Error> {
                Err(Error::custom("not a struct"))
            }

            fn deserialize_struct<V: Visitor<'de>>(
                self,
                _: &'static str,
                fields: &'static [&'static str],
                _: V,
            ) -> Result<V::Value, Error> {
                *self.0 = fields;
                Err(Error::custom("read"))
            }

            serde::forward_to_deserialize_any! {
                bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str
                string bytes byte_buf option unit unit_struct newtype_struct
                seq tuple tuple_struct map enum identifier ignored_any
            }
        }

        let mut fields: &'static [&'static str] = &[];
        let _ = T::deserialize(Introspect(&mut fields));
        fields
    }

    // ADR 0013 and ADR 0016: supported columns include the deprecated marker.
    #[test]
    fn every_key_table_column_is_in_the_checked_in_list() {
        let mut text = String::from("format\t1\n");
        for (table, fields) in [
            ("table", columns::<super::TomlKeyTable>()),
            ("key", columns::<super::TomlKey>()),
        ] {
            assert!(!fields.is_empty(), "{table} has no columns");
            for field in fields {
                text.push_str(&format!("{table}\t{field}\n"));
            }
        }
        assert_eq!(text, include_str!("../tests/golden/key-table-columns.txt"));
    }
}

#[cfg(all(test, unix))]
mod unix_tests {
    use super::owner_allowed;

    //  (only the user running udpstp, or root, may own the key
    // file)
    #[test]
    fn only_the_user_or_root_may_own_the_key_file() {
        assert!(owner_allowed(1000, 1000).is_ok());
        assert!(owner_allowed(0, 1000).is_ok());
        assert!(owner_allowed(0, 0).is_ok());
        let refused = owner_allowed(1001, 1000).unwrap_err();
        assert!(refused.contains("uid 1001"), "{refused}");
        assert!(
            owner_allowed(1000, 0).is_err(),
            "root refuses a user's file"
        );
    }
}
