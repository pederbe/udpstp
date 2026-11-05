//! Seam 2a: key files, with no network (docs/spec.md, "Key table"; stories 38-41
//! and 45). A TOML Key Table or an OB-UDPST `udpst.keys` file is loaded,
//! checked and refused with ErrorStatus 51, and keys never show in output.

mod common;
#[path = "common/human_output.rs"]
mod human_output;
#[path = "common/key_file.rs"]
mod key_file;

use std::process::{Command, Output};

use common::run_bounded;
use human_output::prose;
use key_file::KeyFile;

/// OB-UDPST's ErrorStatus for configuration errors (docs/spec.md).
const CONFIGURATION_ERROR: i32 = 50;
/// OB-UDPST's ErrorStatus for key file errors, ERROR_CONF_KEYFILE.
const KEY_FILE_ERROR: i32 = 51;
/// Every key in these tests contains it, so output can be searched for it.
const SECRET: &str = "s3cr3t";

fn udpstp(args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_udpstp"));
    command.args(args);
    // A run takes about 50 ms; 10 s leaves room for a slow CI start.
    run_bounded(command, std::time::Duration::from_secs(10))
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Deliberate terminal wrapping does not change a diagnostic's meaning.
/// Paths, secrets and structured output are still checked as raw bytes.
fn diagnostic(output: &Output) -> String {
    prose(&stderr(output))
}

/// `udpstp server --key-file FILE --print-config`.
fn server_with(file: &KeyFile) -> Output {
    udpstp(&["server", "--key-file", file.path(), "--print-config"])
}

fn assert_no_secret(output: &Output, what: &str) {
    assert!(
        !stdout(output).contains(SECRET) && !stderr(output).contains(SECRET),
        "{what}: a key was shown\nstdout: {}\nstderr: {}",
        stdout(output),
        stderr(output)
    );
}

const TOML_TABLE: &str = r#"
[[key]]
admin_key_name = "consumers-2026"
key_id = 1
key_text = "s3cr3t-text"
send_lifetime_end = "20261231235959Z"
accept_lifetime_start = "20260101000000Z"
accept_lifetime_end = "20261231235959Z"

[[key]]
admin_key_name = "consumers-2027"
key_id = 1
kdf = "HMAC-SHA-256"
alg_id = "HMAC-SHA-256"
key_hex = "00ff5a"
send_lifetime_start = "20270101000000Z"
accept_lifetime_start = "20270101000000Z"

[[key]]
admin_key_name = "operators"
key_id = 7
key_text = "s3cr3t-operator"
operator = true
max_test_interval = 3600
"#;

// docs/spec.md "Key table" (TOML, one entry per key, RFC 7210 fields with
// send and accept lifetimes, explicit key_text or key_hex, the Operator
// extension); stories 40, 42, 43, 44
#[udpstp_req::req("RFC9946-5.4-3")]
#[test]
fn the_server_loads_a_toml_key_table() {
    let file = KeyFile::new("toml", "keys.toml", TOML_TABLE);

    let output = server_with(&file);

    assert!(output.status.success(), "{}", stderr(&output));
    assert_no_secret(&output, "--print-config");
}

// : a TOML Key Table may name its format, 1, which every valid file
// has; a newer one is refused above
#[test]
fn a_toml_key_table_may_name_its_format() {
    let file = KeyFile::new("format", "keys.toml", &format!("format = 1\n{TOML_TABLE}"));

    let output = server_with(&file);

    assert!(output.status.success(), "{}", stderr(&output));
}

// docs/spec.md "Key table" (OB-UDPST's key file format can be loaded
// directly); story 41
#[test]
fn the_server_loads_an_obudpst_key_file() {
    // OB-UDPST's format as its sample udpst.keys describes it: comments,
    // commas, spaces and tabs; keyIds are read like C's strtol.
    let file = KeyFile::new(
        "obudpst",
        "udpst.keys",
        "# Key file for udpst (CSV)\n\n0,s3cr3t0\t# Legacy clients\n1, s3cr3t1\n3\ts3cr3t3\n  2 s3cr3t2\r\n0x10,s3cr3t16\n010,s3cr3t8\n255,s3cr3t255 trailing words\n",
    );

    let output = server_with(&file);

    assert!(output.status.success(), "{}", stderr(&output));
    assert_no_secret(&output, "--print-config");
}

// docs/spec.md (--print-config shows the key file's path, never keys)
#[test]
fn print_config_shows_the_key_file_path_only() {
    let file = KeyFile::new("print", "keys.toml", TOML_TABLE);

    let output = server_with(&file);

    assert!(output.status.success(), "{}", stderr(&output));
    let printed = stdout(&output);
    let line = printed.lines().find(|l| l.starts_with("key_file"));
    assert!(
        line.is_some_and(|l| l.contains("keys.toml")),
        "the key_file setting: {printed}"
    );
    assert_no_secret(&output, "--print-config");
}

// docs/spec.md "CLI, configuration, output and exit codes" (ErrorStatus 51
// for key file errors); "Key table" (keys never logged)
#[test]
fn an_invalid_key_file_exits_with_51_and_says_where() {
    let row = |fields: &str| format!("[[key]]\nadmin_key_name = \"k\"\nkey_id = 1\n{fields}\n");
    let cases: Vec<(&str, &str, String, &str)> = vec![
        (
            "no key",
            "keys.toml",
            row(""),
            "exactly one of key_text and key_hex",
        ),
        (
            "both kinds of key",
            "keys.toml",
            row("key_text = \"s3cr3t\"\nkey_hex = \"00\""),
            "exactly one",
        ),
        (
            "odd hex",
            "keys.toml",
            row("key_hex = \"s3cr3t0\""),
            "key_hex",
        ),
        ("an empty key", "keys.toml", row("key_text = \"\""), "empty"),
        (
            "a mistyped keyId holding a key",
            "keys.toml",
            "[[key]]\nadmin_key_name = \"k\"\nkey_id = \"s3cr3t\"\nkey_text = \"x\"\n".into(),
            "line 3: not valid TOML",
        ),
        (
            "a key outside a [[key]] table",
            "keys.toml",
            "key = \"s3cr3t\"\n".into(),
            "line 1",
        ),
        (
            "two rows with one admin_key_name",
            "keys.toml",
            format!(
                "{}{}",
                row(
                    "key_text = \"s3cr3t1\"\nsend_lifetime_end = \"20200101000000Z\"\naccept_lifetime_end = \"20200101000000Z\""
                ),
                row(
                    "key_text = \"s3cr3t2\"\nsend_lifetime_start = \"20210101000000Z\"\naccept_lifetime_start = \"20210101000000Z\""
                )
            ),
            "not unique",
        ),
        (
            "a maximum Test Interval under 60 s",
            "keys.toml",
            row("key_text = \"s3cr3t\"\noperator = true\nmax_test_interval = 59"),
            "at least 60 s",
        ),
        (
            "TOML in a file not named .toml",
            "keys.conf",
            row("key_text = \"s3cr3t\""),
            "a TOML Key Table's name ends in .toml",
        ),
        (
            "a signed hex key",
            "keys.toml",
            row("key_hex = \"+f\""),
            "key_hex",
        ),
        (
            "another KDF",
            "keys.toml",
            row("key_text = \"s3cr3t\"\nkdf = \"HMAC-SHA-1\""),
            "kdf",
        ),
        (
            "another AlgID",
            "keys.toml",
            row("key_text = \"s3cr3t\"\nalg_id = \"HMAC-SHA-1\""),
            "alg_id",
        ),
        (
            "a bad time",
            "keys.toml",
            row("key_text = \"s3cr3t\"\nsend_lifetime_end = \"2026-01-01\""),
            "send_lifetime_end",
        ),
        (
            "a lifetime ending before it starts",
            "keys.toml",
            row(
                "key_text = \"s3cr3t\"\naccept_lifetime_start = \"20270101000000Z\"\naccept_lifetime_end = \"20260101000000Z\"",
            ),
            "ends before it starts",
        ),
        (
            "a Quota on an Operator Key",
            "keys.toml",
            row("key_text = \"s3cr3t\"\noperator = true\ntests_per_quota_period = 5"),
            "an Operator Key has no Quota",
        ),
        (
            "a Quota of 0",
            "keys.toml",
            row("key_text = \"s3cr3t\"\nbytes_per_quota_period = 0"),
            "at least 1",
        ),
        (
            "a maximum Test Interval without the Operator marker",
            "keys.toml",
            row("key_text = \"s3cr3t\"\nmax_test_interval = 3600"),
            "operator",
        ),
        (
            "overlapping rows",
            "keys.toml",
            format!(
                "{}{}",
                row("key_text = \"s3cr3t1\""),
                row("key_text = \"s3cr3t2\"").replace("\"k\"", "\"k2\"")
            ),
            "overlap",
        ),
        (
            "a newer format",
            "keys.toml",
            format!("format = 2\n{}", row("key_text = \"s3cr3t\"")),
            "format 2 is newer",
        ),
        (
            "a newer format with a column this version doesn't know",
            "keys.toml",
            format!(
                "format = 2\n{}",
                row("key_text = \"s3cr3t\"\nnew_column = true")
            ),
            "format 2 is newer",
        ),
        (
            "format 0",
            "keys.toml",
            format!("format = 0\n{}", row("key_text = \"s3cr3t\"")),
            "format must be 1",
        ),
        (
            "format in a row",
            "keys.toml",
            row("key_text = \"s3cr3t\"\nformat = 1"),
            "format must come before the first [[key]]",
        ),
        (
            "an unknown field",
            "keys.toml",
            row("key_text = \"s3cr3t\"\nkey_txet = \"x\""),
            "key_txet",
        ),
        (
            "a key that isn't a string",
            "keys.toml",
            "[[key]]\nadmin_key_name = \"k\"\nkey_id = 1\nkey_text = 5318008\n".into(),
            "line 4: not valid TOML, or a value of the wrong type",
        ),
        (
            "invalid TOML",
            "keys.toml",
            "[[key]\nkey_text = \"s3cr3t\"\n".into(),
            "line 1",
        ),
        ("no keys in TOML", "keys.toml", "".into(), "no keys"),
        (
            "an OB-UDPST row without a key",
            "udpst.keys",
            "1\n".into(),
            "line 1",
        ),
        (
            "a keyId above 255",
            "udpst.keys",
            "# c\n256,s3cr3t\n".into(),
            "line 2",
        ),
        (
            "a keyId that isn't a number",
            "udpst.keys",
            "one,s3cr3t\n".into(),
            "keyId",
        ),
        (
            "an octal keyId with an 8",
            "udpst.keys",
            "08,s3cr3t\n".into(),
            "keyId",
        ),
        (
            "a duplicate keyId",
            "udpst.keys",
            "1,s3cr3t1\n1,s3cr3t2\n".into(),
            "line 2: keyId 1 appears twice",
        ),
        (
            "a hex keyId, read as hex",
            "udpst.keys",
            "16,s3cr3t1\n0x10,s3cr3t2\n".into(),
            "keyId 16 appears twice",
        ),
        (
            "an octal keyId, read as octal",
            "udpst.keys",
            "8,s3cr3t1\n010,s3cr3t2\n".into(),
            "keyId 8 appears twice",
        ),
        (
            "a key over 64 characters",
            "udpst.keys",
            format!("1,{SECRET}{}\n", "x".repeat(65 - SECRET.len())),
            "64",
        ),
        (
            "no keys in udpst.keys",
            "udpst.keys",
            "# only comments\n".into(),
            "no keys",
        ),
    ];
    for (i, (what, name, contents, mentioned)) in cases.iter().enumerate() {
        let file = KeyFile::new(&format!("invalid{i}"), name, contents);

        let output = server_with(&file);

        assert_eq!(
            output.status.code(),
            Some(KEY_FILE_ERROR),
            "{what}: {}",
            stderr(&output)
        );
        assert!(
            diagnostic(&output).contains(mentioned),
            "{what}: the message mentions {mentioned:?}: {}",
            stderr(&output)
        );
        assert_no_secret(&output, what);
        assert!(
            stderr(&output).contains(file.path()),
            "{what}: the path remains intact: {}",
            stderr(&output)
        );
        assert!(
            !stderr(&output).contains("5318008"),
            "{what}: a key was shown"
        );
    }
}

// docs/spec.md (ErrorStatus 51 for key file errors)
#[test]
fn a_missing_key_file_exits_with_51() {
    let output = udpstp(&["server", "--key-file", "no-such.keys", "--print-config"]);

    assert_eq!(
        output.status.code(),
        Some(KEY_FILE_ERROR),
        "{}",
        stderr(&output)
    );
    assert!(
        stderr(&output).contains("no-such.keys"),
        "{}",
        stderr(&output)
    );
}

// docs/spec.md "Key table" (key files readable by group or others are
// refused); story 39
#[cfg(unix)]
#[test]
fn a_key_file_others_can_read_is_refused() {
    for mode in [0o640, 0o604, 0o644, 0o620, 0o602] {
        let file = KeyFile::new(&format!("mode{mode:o}"), "udpst.keys", "1,s3cr3t\n");
        file.set_mode(mode);

        let output = server_with(&file);

        assert_eq!(
            output.status.code(),
            Some(KEY_FILE_ERROR),
            "mode {mode:o}: {}",
            stderr(&output)
        );
        assert!(
            diagnostic(&output).contains("by group or others"),
            "{}",
            stderr(&output)
        );
    }
    let owner_only = KeyFile::new("mode600", "udpst.keys", "1,s3cr3t\n");
    owner_only.set_mode(0o600);
    assert!(server_with(&owner_only).status.success());
}

//  (a key file owned by another user, who could replace the keys,
// is refused). udpstp runs as root and the file is the test user's: as
// root directly, or through passwordless sudo (as on CI runners).
#[cfg(unix)]
#[test]
fn a_key_file_owned_by_another_user_is_refused() {
    let file = KeyFile::new(
        "owner",
        "udpst.keys",
        "1,s3cr3t
",
    );
    let args = ["server", "--key-file", file.path(), "--print-config"];
    let bin = env!("CARGO_BIN_EXE_udpstp");
    let uid = Command::new("id").arg("-u").output().unwrap();
    let command = if String::from_utf8_lossy(&uid.stdout).trim() == "0" {
        // Root gives the file to nobody.
        std::os::unix::fs::chown(file.path(), Some(65534), None).unwrap();
        let mut command = Command::new(bin);
        command.args(args);
        command
    } else if Command::new("sudo")
        .args(["-n", "true"])
        .output()
        .is_ok_and(|o| o.status.success())
    {
        let mut command = Command::new("sudo");
        command.arg("-n").arg(bin).args(args);
        command
    } else {
        assert!(
            std::env::var_os("CI").is_none(),
            "CI must run this test as root or with passwordless sudo"
        );
        eprintln!("skipped: needs root or passwordless sudo");
        return;
    };

    let output = common::run_bounded(command, std::time::Duration::from_secs(10));

    assert_eq!(
        output.status.code(),
        Some(KEY_FILE_ERROR),
        "{}",
        stderr(&output)
    );
    assert!(
        diagnostic(&output).contains("owned by another user"),
        "{}",
        stderr(&output)
    );
}

//  (a key file is bounded in size, so it is read into one buffer)
#[test]
fn an_oversized_key_file_is_refused() {
    let file = KeyFile::new("big", "udpst.keys", &"#".repeat(1024 * 1024 + 1));

    let output = server_with(&file);

    assert_eq!(
        output.status.code(),
        Some(KEY_FILE_ERROR),
        "{}",
        stderr(&output)
    );
    assert!(
        diagnostic(&output).contains("larger than"),
        "{}",
        stderr(&output)
    );
}

// docs/spec.md "Key table" (a key file is a regular file)
#[cfg(unix)]
#[test]
fn a_key_file_must_be_a_regular_file() {
    let output = udpstp(&["server", "--key-file", "/dev/null", "--print-config"]);

    assert_eq!(
        output.status.code(),
        Some(KEY_FILE_ERROR),
        "{}",
        stderr(&output)
    );
    assert!(
        diagnostic(&output).contains("not a regular file"),
        "{}",
        stderr(&output)
    );
}

// ADR 0015: a supported private Windows file loads quietly
// (conformance: key-file-private).
#[cfg(windows)]
#[test]
fn a_private_key_file_loads_on_windows() {
    let file = KeyFile::new("windows", "udpst.keys", "1,s3cr3t\n");

    assert!(server_with(&file).status.success());
}

// docs/spec.md "Key table" (keys are never accepted on the command line);
// story 38
#[test]
fn keys_are_not_accepted_on_the_command_line() {
    for args in [
        vec!["client", "--key", SECRET, "127.0.0.1"],
        vec!["client", "--auth-key", SECRET, "127.0.0.1"],
        vec!["server", "--key", SECRET, "--print-config"],
    ] {
        let output = udpstp(&args);

        assert_eq!(
            output.status.code(),
            Some(CONFIGURATION_ERROR),
            "{args:?}: {}",
            stderr(&output)
        );
    }
    let help = stdout(&udpstp(&["client", "--help"])) + &stdout(&udpstp(&["server", "--help"]));
    assert!(
        !help.contains("--key ") && !help.contains("--auth-key"),
        "{help}"
    );
}

// docs/spec.md (ErrorStatus 51 for key file errors, for the Client too)
#[test]
fn the_client_refuses_an_invalid_key_file() {
    let file = KeyFile::new("client", "udpst.keys", "one,s3cr3t\n");

    let output = udpstp(&["client", "--key-file", file.path(), "127.0.0.1:9"]);

    assert_eq!(
        output.status.code(),
        Some(KEY_FILE_ERROR),
        "{}",
        stderr(&output)
    );
    assert_no_secret(&output, "the Client");
}

// docs/spec.md (a keyId option pins the key; invalid combinations are
// configuration errors)
#[test]
fn key_options_that_do_not_fit_together_are_configuration_errors() {
    let file = KeyFile::new("combos", "udpst.keys", "1,s3cr3t\n");
    for args in [
        vec!["client", "--key-id", "1", "127.0.0.1:9"],
        vec![
            "client",
            "--unauthenticated",
            "--key-file",
            file.path(),
            "127.0.0.1:9",
        ],
        vec![
            "client",
            "--key-file",
            file.path(),
            "--key-id",
            "256",
            "127.0.0.1:9",
        ],
    ] {
        let output = udpstp(&args);

        assert_eq!(
            output.status.code(),
            Some(CONFIGURATION_ERROR),
            "{args:?}: {}",
            stderr(&output)
        );
    }
}

// ADR 0010 (OB-UDPST key file rows have no lifetimes, so a Client with
// several of them needs a pinned keyId); RFC 7210 §3 (a key is sent only
// within its send lifetime)
#[test]
fn a_client_without_a_usable_send_key_does_not_start() {
    let several = KeyFile::new("several", "udpst.keys", "1,s3cr3t1\n2,s3cr3t2\n");
    let expired = KeyFile::new(
        "expired",
        "keys.toml",
        "[[key]]\nadmin_key_name = \"old\"\nkey_id = 1\nkey_text = \"s3cr3t\"\nsend_lifetime_end = \"20200101000000Z\"\n",
    );
    for (what, file) in [
        ("two timeless keys", &several),
        ("an expired key", &expired),
    ] {
        let output = udpstp(&[
            "client",
            "--key-file",
            file.path(),
            "--time-limit",
            "1",
            "127.0.0.1:9",
        ]);

        assert_eq!(
            output.status.code(),
            Some(CONFIGURATION_ERROR),
            "{what}: {}",
            stderr(&output)
        );
        assert_no_secret(&output, what);
    }
}

// docs/spec.md (ErrorStatus 51 for key file errors: a keyId the file
// doesn't have, as OB-UDPST reports it)
#[test]
fn a_key_id_missing_from_the_key_file_exits_with_51() {
    let file = KeyFile::new("missing-id", "udpst.keys", "1,s3cr3t\n");

    let output = udpstp(&[
        "client",
        "--key-file",
        file.path(),
        "--key-id",
        "2",
        "127.0.0.1:9",
    ]);

    assert_eq!(
        output.status.code(),
        Some(KEY_FILE_ERROR),
        "{}",
        stderr(&output)
    );
    assert!(
        diagnostic(&output).contains("keyId 2"),
        "{}",
        stderr(&output)
    );
    assert!(
        !stdout(&output).contains("Sending"),
        "checked before any Setup Request"
    );
}

// ADR 0010 (several timeless keys need --key-id, and the Client says
// so)
#[test]
fn an_ambiguous_key_file_asks_for_a_key_id() {
    let file = KeyFile::new("ambiguous", "udpst.keys", "0,s3cr3t0\n1,s3cr3t1\n");

    let output = udpstp(&[
        "client",
        "--key-file",
        file.path(),
        "--time-limit",
        "1",
        "127.0.0.1:9",
    ]);

    assert_eq!(
        output.status.code(),
        Some(CONFIGURATION_ERROR),
        "{}",
        stderr(&output)
    );
    assert!(stderr(&output).contains("--key-id"), "{}", stderr(&output));
}

// docs/spec.md (the Server's TOML configuration can name the key file)
#[test]
fn the_server_configuration_file_can_name_the_key_file() {
    let keys = KeyFile::new("config", "udpst.keys", "1,s3cr3t\n");
    let config = KeyFile::new(
        "config-file",
        "server.toml",
        &format!("key_file = {:?}\n", keys.path()),
    );

    let output = udpstp(&["server", "--config", config.path(), "--print-config"]);

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stdout(&output).contains("udpst.keys"),
        "{}",
        stdout(&output)
    );
    assert_no_secret(&output, "--print-config");

    let broken = KeyFile::new("config-broken", "udpst.keys", "one,s3cr3t\n");
    let config = KeyFile::new(
        "config-file-broken",
        "server.toml",
        &format!("key_file = {:?}\n", broken.path()),
    );
    let output = udpstp(&["server", "--config", config.path(), "--print-config"]);
    assert_eq!(
        output.status.code(),
        Some(KEY_FILE_ERROR),
        "{}",
        stderr(&output)
    );
}
