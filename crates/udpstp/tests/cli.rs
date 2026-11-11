//! Seam 2a: the `udpstp` process with no network. CLI parsing, the Server's
//! TOML configuration with command-line overrides, and configuration-error
//! exit codes (docs/spec.md, "CLI, configuration, output and exit codes").

mod common;
#[path = "common/human_output.rs"]
mod human_output;

use std::path::PathBuf;
use std::process::{Command, Output};

use common::run_bounded;
use human_output::prose;

/// OB-UDPST's ErrorStatus for configuration errors (docs/spec.md).
const CONFIGURATION_ERROR: i32 = 50;

fn udpstp(args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_udpstp"));
    command.args(args);
    // Each run takes about 50 ms; 10 s leaves room for a slow CI start and
    // still ends a run that wrongly starts serving.
    run_bounded(command, std::time::Duration::from_secs(10))
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Check diagnostic meaning separately from intentional terminal wraps.
/// Identifiers and structured output continue to use the raw helpers.
fn diagnostic(output: &Output) -> String {
    prose(&stderr(output))
}

/// A configuration file in a fresh temporary directory.
struct ConfigFile {
    dir: PathBuf,
    path: PathBuf,
}

impl ConfigFile {
    fn new(name: &str, contents: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("udpstp-cli-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("server.toml");
        std::fs::write(&path, contents).expect("config written");
        Self { dir, path }
    }

    fn path(&self) -> &str {
        self.path.to_str().expect("UTF-8 temp path")
    }
}

impl Drop for ConfigFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The value of `key` in `--print-config` output (`key = value` lines).
fn printed(output: &Output, key: &str) -> Option<String> {
    stdout(output).lines().find_map(|line| {
        let (k, v) = line.split_once('=')?;
        (k.trim() == key).then(|| v.trim().to_owned())
    })
}

// ADR 0013 and ADR 0015: settings preview never loads keys or opens logs;
// validating --print-config remains the no-network key-file seam.
#[test]
fn settings_only_prints_settings_without_loading_keys_or_opening_logs() {
    let config = ConfigFile::new("settings-only", "");
    let keys = config.dir.join("invalid.keys");
    let log = config.dir.join("logs").join("server.log");
    for contents in [None, Some("not a key file: private-fixture-material")] {
        if let Some(contents) = contents {
            std::fs::write(&keys, contents).unwrap();
        }
        let args = [
            "server",
            "--key-file",
            keys.to_str().unwrap(),
            "--log-file",
            log.to_str().unwrap(),
            "--print-config",
        ];
        let preview = udpstp(&[&args[..], &["--settings-only"]].concat());
        assert!(preview.status.success(), "{}", stderr(&preview));
        let settings: toml::Value = toml::from_str(&stdout(&preview)).unwrap();
        assert_eq!(settings["key_file"].as_str(), keys.to_str());
        assert!(!stdout(&preview).contains("private-fixture-material"));
        assert!(!log.parent().unwrap().exists(), "preview opened a log");
        let validating = udpstp(&args);
        assert_eq!(
            validating.status.code(),
            Some(51),
            "{}",
            stderr(&validating)
        );
    }
    std::fs::write(&config.path, "no_such_setting = true").unwrap();
    let invalid = udpstp(&[
        "server",
        "--config",
        config.path(),
        "--log-file",
        log.to_str().unwrap(),
        "--print-config",
        "--settings-only",
    ]);
    assert_eq!(invalid.status.code(), Some(CONFIGURATION_ERROR));
    assert!(
        !log.parent().unwrap().exists(),
        "invalid preview opened a log"
    );
    assert!(stdout(&invalid).is_empty());
}

// ADR 0013 and ADR 0015: a settings preview cannot bypass Server startup checks.
#[test]
fn settings_only_requires_printing_and_cannot_run_as_a_service() {
    for args in [
        vec!["server", "--settings-only"],
        vec![
            "server",
            "--print-config",
            "--settings-only",
            "--windows-service",
        ],
    ] {
        let output = udpstp(&args);
        assert_eq!(output.status.code(), Some(CONFIGURATION_ERROR));
        assert!(stdout(&output).is_empty());
    }
}

// docs/spec.md story 66; GLOSSARY.md (Client, Server, Test)
#[test]
fn help_lists_the_client_and_server_subcommands() {
    let output = udpstp(&["--help"]);

    assert!(output.status.success(), "{}", stderr(&output));
    let help = stdout(&output);
    assert!(help.contains("client"), "{help}");
    assert!(help.contains("server"), "{help}");
    assert!(help.contains("Test"), "the glossary's Test: {help}");
}

// GLOSSARY.md (Test, Server, Test Connection)
#[test]
fn subcommand_help_uses_glossary_terms() {
    let client = stdout(&udpstp(&["client", "--help"]));
    assert!(
        client.contains("Server") && client.contains("Test"),
        "{client}"
    );

    let server = stdout(&udpstp(&["server", "--help"]));
    assert!(
        server.contains("Client") && server.contains("Test"),
        "{server}"
    );
}

// docs/interfaces.md terminal layout policy and ADR 0013: redirected help
// uses exactly 80 displayed columns, preserving stdout and status on every OS.
#[test]
fn redirected_help_uses_eighty_columns_despite_columns_environment() {
    let mut command = Command::new(env!("CARGO_BIN_EXE_udpstp"));
    command.args(["client", "--help"]).env("COLUMNS", "190");
    let output = run_bounded(command, std::time::Duration::from_secs(10));
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let help = stdout(&output);
    assert!(
        help.lines()
            .all(|line| unicode_width::UnicodeWidthStr::width(line) <= 80)
    );
    assert!(
        help.contains(concat!(
            "          The Minimum Test Connections: how many must succeed for the Test to go\n",
            "          on [default: all of them]. With all required, one lost Setup Response\n",
            "          aborts the Test"
        )),
        "{help}"
    );
    let mut command = Command::new(env!("CARGO_BIN_EXE_udpstp"));
    command.args(["client", "--help"]).env("COLUMNS", "60");
    let narrower_environment = run_bounded(command, std::time::Duration::from_secs(10));
    assert!(narrower_environment.status.success());
    assert!(narrower_environment.stderr.is_empty());
    assert_eq!(output.stdout, narrower_environment.stdout);
}

// Conformance Record: setup-response-from-another-address. Alternate
// addresses cannot widen the Unauthenticated Mode source restriction.
#[test]
fn alternate_server_authorization_requires_authentication() {
    let output = udpstp(&[
        "client",
        "--unauthenticated",
        "--allow-server-address",
        "192.0.2.1=192.0.2.77",
        "192.0.2.1",
    ]);
    assert_eq!(output.status.code(), Some(CONFIGURATION_ERROR));
    assert!(
        stderr(&output).contains("--unauthenticated"),
        "{}",
        stderr(&output)
    );
    let output = udpstp(&[
        "client",
        "--allow-server-address",
        "192.0.2.1=192.0.2.77",
        "192.0.2.1",
    ]);
    assert_eq!(output.status.code(), Some(CONFIGURATION_ERROR));
    assert!(
        stderr(&output).contains("--key-file"),
        "{}",
        stderr(&output)
    );
}

// docs/spec.md (Server configuration is a TOML file)
#[udpstp_req::req("RFC9946-12.1-L1")]
#[test]
fn the_server_defaults_apply_without_a_file() {
    let output = udpstp(&["server", "--print-config"]);

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(printed(&output, "port").as_deref(), Some("24601"));
    assert_eq!(
        printed(&output, "allow_unauthenticated").as_deref(),
        Some("false"),
        "unauthenticated mode is off unless opted in"
    );
    assert_eq!(printed(&output, "checksum").as_deref(), Some("false"));
    assert_eq!(
        printed(&output, "ephemeral_ports").as_deref(),
        Some("\"49152-65535\""),
        "the dynamic ports (RFC 6335 §6)"
    );
}

// docs/spec.md story 66 (a TOML configuration file with command-line
// overrides)
#[test]
fn command_line_flags_override_the_file() {
    let file = ConfigFile::new(
        "override",
        "port = 30000\nallow_unauthenticated = true\nchecksum = true\n",
    );

    let from_file = udpstp(&["server", "--config", file.path(), "--print-config"]);
    assert!(from_file.status.success(), "{}", stderr(&from_file));
    assert_eq!(printed(&from_file, "port").as_deref(), Some("30000"));
    assert_eq!(
        printed(&from_file, "allow_unauthenticated").as_deref(),
        Some("true")
    );

    let overridden = udpstp(&[
        "server",
        "--config",
        file.path(),
        "--port",
        "31000",
        "--allow-unauthenticated=false",
        "--print-config",
    ]);
    assert!(overridden.status.success(), "{}", stderr(&overridden));
    assert_eq!(printed(&overridden, "port").as_deref(), Some("31000"));
    assert_eq!(
        printed(&overridden, "allow_unauthenticated").as_deref(),
        Some("false"),
        "a flag can switch a file setting off"
    );
    assert_eq!(
        printed(&overridden, "checksum").as_deref(),
        Some("true"),
        "settings without a flag keep the file's value"
    );
}

// docs/spec.md (ErrorStatus 50: configuration errors)
#[test]
fn an_invalid_configuration_exits_with_50_and_says_why() {
    let unknown = ConfigFile::new("unknown", "prot = 30000\n");
    let bad_value = ConfigFile::new("value", "port = 0\n");
    let not_toml = ConfigFile::new("syntax", "port = = 1\n");
    let backwards = ConfigFile::new("range", "ephemeral_ports = \"50000-40000\"\n");
    let test_quota = ConfigFile::new("test-quota", "source_tests_per_quota_period = 0\n");
    let byte_quota = ConfigFile::new("byte-quota", "source_bytes_per_quota_period = 0\n");
    let cases: [(&str, Vec<&str>, &str); 9] = [
        (
            "a zero Test Quota",
            vec!["server", "--config", test_quota.path(), "--print-config"],
            "source_tests_per_quota_period: a Quota must be at least 1",
        ),
        (
            "a zero byte Quota",
            vec!["server", "--config", byte_quota.path(), "--print-config"],
            "source_bytes_per_quota_period: a Quota must be at least 1",
        ),
        (
            "a backwards Ephemeral Port range",
            vec!["server", "--config", backwards.path(), "--print-config"],
            "Use FIRST-LAST, for example 49152-65535",
        ),
        (
            "a Ephemeral Port range from 0",
            vec!["server", "--ephemeral-ports", "0-100", "--print-config"],
            "Ports must be from 1 to 65535 and FIRST must not exceed LAST",
        ),
        (
            "an unknown setting",
            vec!["server", "--config", unknown.path(), "--print-config"],
            "prot",
        ),
        (
            "port 0",
            vec!["server", "--config", bad_value.path(), "--print-config"],
            "port",
        ),
        (
            "invalid TOML",
            vec!["server", "--config", not_toml.path(), "--print-config"],
            "server.toml",
        ),
        (
            "a missing file",
            vec!["server", "--config", "no-such-file.toml", "--print-config"],
            "no-such-file.toml",
        ),
        (
            "an unknown flag",
            vec!["server", "--no-such-flag", "--print-config"],
            "--no-such-flag",
        ),
    ];
    for (what, args, mentioned) in cases {
        let output = udpstp(&args);

        assert_eq!(
            output.status.code(),
            Some(CONFIGURATION_ERROR),
            "{what}: {}",
            stderr(&output)
        );
        assert!(
            diagnostic(&output).contains(mentioned),
            "{what}: the message names {mentioned}: {}",
            stderr(&output)
        );
    }
}

// docs/spec.md (ErrorStatus 50: configuration errors)
#[test]
fn the_client_needs_a_server() {
    let output = udpstp(&["client"]);

    assert_eq!(output.status.code(), Some(CONFIGURATION_ERROR));
    assert!(stderr(&output).contains("SERVER"), "{}", stderr(&output));
}

// docs/spec.md (ErrorStatus 50); a UDP port is 1 to 65535
#[test]
fn a_port_outside_1_to_65535_on_the_command_line_is_a_configuration_error() {
    for port in ["0", "65536", "not-a-port"] {
        let output = udpstp(&["server", "--port", port, "--print-config"]);

        assert_eq!(
            output.status.code(),
            Some(CONFIGURATION_ERROR),
            "port {port}"
        );
        assert!(stderr(&output).contains(port), "{}", stderr(&output));
    }
}

// docs/spec.md story 66: switches are given as --flag or --flag=BOOL, never
// with the value as the next word, so they can't take an operand
#[test]
fn a_bare_switch_turns_its_setting_on() {
    let output = udpstp(&[
        "server",
        "--allow-unauthenticated",
        "--checksum",
        "--print-config",
    ]);

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        printed(&output, "allow_unauthenticated").as_deref(),
        Some("true")
    );
    assert_eq!(printed(&output, "checksum").as_deref(), Some("true"));

    let spaced = udpstp(&["server", "--checksum", "false", "--print-config"]);
    assert_eq!(
        spaced.status.code(),
        Some(CONFIGURATION_ERROR),
        "`false` is an operand, not the switch's value"
    );
}

// docs/spec.md "Load Rate Adjustment Algorithms" (DelayVar is the maximum,
// the delay range, by default; a Server setting can choose the average),
//
#[test]
fn the_delay_var_statistic_is_a_server_setting() {
    let default = udpstp(&["server", "--print-config"]);
    assert_eq!(
        printed(&default, "delay_var_statistic").as_deref(),
        Some("\"maximum\"")
    );

    let file = ConfigFile::new(
        "delay-var",
        "delay_var_statistic = \"average\"
",
    );
    let from_file = udpstp(&["server", "--config", file.path(), "--print-config"]);
    assert!(from_file.status.success(), "{}", stderr(&from_file));
    assert_eq!(
        printed(&from_file, "delay_var_statistic").as_deref(),
        Some("\"average\"")
    );

    let overridden = udpstp(&[
        "server",
        "--config",
        file.path(),
        "--delay-var-statistic",
        "maximum",
        "--print-config",
    ]);
    assert_eq!(
        printed(&overridden, "delay_var_statistic").as_deref(),
        Some("\"maximum\"")
    );

    let unknown = ConfigFile::new("delay-var-unknown", "delay_var_statistic = \"median\"\n");
    for output in [
        udpstp(&["server", "--config", unknown.path(), "--print-config"]),
        udpstp(&[
            "server",
            "--delay-var-statistic",
            "median",
            "--print-config",
        ]),
    ] {
        assert_eq!(
            output.status.code(),
            Some(CONFIGURATION_ERROR),
            "{}",
            stderr(&output)
        );
    }

    // The setting was delay_var before 0.1.0: the old names are refused,
    // naming the new one.
    let renamed = ConfigFile::new("delay-var-renamed", "delay_var = \"average\"\n");
    for output in [
        udpstp(&["server", "--config", renamed.path(), "--print-config"]),
        udpstp(&["server", "--delay-var", "average", "--print-config"]),
    ] {
        assert_eq!(output.status.code(), Some(CONFIGURATION_ERROR));
        assert!(
            stderr(&output).contains("delay_var_statistic"),
            "{}",
            stderr(&output)
        );
    }
}

//
#[udpstp_req::req("RFC9946-6-1")]
#[test]
fn a_client_refuses_a_multicast_or_broadcast_server() {
    for server in ["224.0.0.1", "255.255.255.255", "[ff02::1]:24601"] {
        let output = udpstp(&["client", "--unauthenticated", "--time-limit", "2", server]);
        assert_eq!(
            output.status.code(),
            Some(CONFIGURATION_ERROR),
            "{server}: {}",
            stderr(&output)
        );
        assert!(
            diagnostic(&output).contains("multicast or broadcast"),
            "{}",
            stderr(&output)
        );
    }
}

// docs/spec.md (a global Test Connection cap, a per-Test cap of 24, a
// Admitted Rate Cap and the NOMAXBW setting, off by default)
#[udpstp_req::req("RFC9946-5-L2")]
#[test]
fn the_admission_limits_are_server_settings() {
    let defaults = udpstp(&["server", "--print-config"]);
    assert!(defaults.status.success(), "{}", stderr(&defaults));
    assert_eq!(
        printed(&defaults, "max_test_connections").as_deref(),
        Some("256")
    );
    assert_eq!(
        printed(&defaults, "test_connections_per_test").as_deref(),
        Some("24")
    );
    assert_eq!(printed(&defaults, "admitted_rate_cap"), None, "no cap");
    // DF on; MaxHops unset, so the Server warns and uses 64.
    assert_eq!(printed(&defaults, "dont_fragment").as_deref(), Some("true"));
    assert_eq!(printed(&defaults, "max_hops"), None);
    let hops = udpstp(&[
        "server",
        "--max-hops",
        "12",
        "--dont-fragment=false",
        "--print-config",
    ]);
    assert!(hops.status.success(), "{}", stderr(&hops));
    assert_eq!(printed(&hops, "max_hops").as_deref(), Some("12"));
    assert_eq!(printed(&hops, "dont_fragment").as_deref(), Some("false"));
    for bad in [
        ["server", "--max-hops", "0"],
        ["server", "--max-hops", "256"],
    ] {
        let output = udpstp(&[&bad[..], &["--print-config"]].concat());
        assert_eq!(output.status.code(), Some(50), "{bad:?}");
    }
    // Only the default DSCP unless the administrator allows more.
    assert_eq!(printed(&defaults, "allowed_dscp").as_deref(), Some("[0]"));
    let dscp = udpstp(&["server", "--allowed-dscp", "0,46", "--print-config"]);
    assert_eq!(printed(&dscp, "allowed_dscp").as_deref(), Some("[0, 46]"));
    for bad in ["64", ""] {
        let output = udpstp(&["server", "--allowed-dscp", bad, "--print-config"]);
        assert_eq!(
            output.status.code(),
            Some(50),
            "{bad:?}: {}",
            stderr(&output)
        );
    }
    let file = ConfigFile::new("dscp", "operator_allowed_dscp = []\n");
    let output = udpstp(&["server", "--config", file.path(), "--print-config"]);
    assert_eq!(output.status.code(), Some(50), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("operator_allowed_dscp"),
        "{}",
        stderr(&output)
    );
    let file = ConfigFile::new("hops", "max_hops = 0\n");
    let output = udpstp(&["server", "--config", file.path(), "--print-config"]);
    assert_eq!(output.status.code(), Some(50), "{}", stderr(&output));
    assert!(stderr(&output).contains("max_hops"), "{}", stderr(&output));
    assert_eq!(
        printed(&defaults, "require_max_bandwidth").as_deref(),
        Some("false")
    );

    let set = udpstp(&[
        "server",
        "--max-test-connections",
        "100",
        "--test-connections-per-test",
        "4",
        "--admitted-rate-cap",
        "10000",
        "--require-max-bandwidth",
        "--ephemeral-ports",
        "50000-50149",
        "--print-config",
    ]);
    assert!(set.status.success(), "{}", stderr(&set));
    assert_eq!(
        printed(&set, "max_test_connections").as_deref(),
        Some("100")
    );
    assert_eq!(
        printed(&set, "test_connections_per_test").as_deref(),
        Some("4")
    );
    assert_eq!(printed(&set, "admitted_rate_cap").as_deref(), Some("10000"));
    assert_eq!(
        printed(&set, "require_max_bandwidth").as_deref(),
        Some("true")
    );
}

// The Ephemeral Port range holds the global cap plus the separate
// Unactivated Test Connection share (conformance: unactivated-test-connection-share).
#[test]
fn a_ephemeral_port_range_smaller_than_the_cap_plus_share_is_a_configuration_error() {
    for (args, mentioned) in [
        (
            vec![
                "server",
                "--ephemeral-ports",
                "50000-50382",
                "--print-config",
            ],
            "ephemeral_ports",
        ),
        (
            vec!["server", "--max-test-connections", "0", "--print-config"],
            "max_test_connections",
        ),
        (
            vec![
                "server",
                "--test-connections-per-test",
                "0",
                "--print-config",
            ],
            "test_connections_per_test",
        ),
        (
            vec!["server", "--admitted-rate-cap", "0", "--print-config"],
            "admitted_rate_cap",
        ),
        (
            vec!["server", "--source-setup-burst", "23", "--print-config"],
            "source_setup_burst",
        ),
        (
            vec![
                "server",
                "--source-setups-per-second",
                "0",
                "--print-config",
            ],
            "source_setups_per_second",
        ),
        (
            vec!["server", "--key-setup-burst", "23", "--print-config"],
            "key_setup_burst",
        ),
        (
            vec!["server", "--key-setups-per-second", "0", "--print-config"],
            "key_setups_per_second",
        ),
        (
            vec![
                "server",
                "--max-unactivated-test-connections",
                "23",
                "--print-config",
            ],
            "max_unactivated_test_connections",
        ),
        (
            vec!["client", "--max-bandwidth", "32768", "192.0.2.1"],
            "max-bandwidth",
        ),
    ] {
        let output = udpstp(&args);
        assert_eq!(
            output.status.code(),
            Some(CONFIGURATION_ERROR),
            "{args:?}: {}",
            stderr(&output)
        );
        assert!(
            diagnostic(&output).contains(mentioned),
            "{args:?}: {}",
            stderr(&output)
        );
    }
    // 383 ports is one short of the default cap plus share; 384 is enough.
    let enough = udpstp(&[
        "server",
        "--ephemeral-ports",
        "50000-50383",
        "--print-config",
    ]);
    assert!(enough.status.success(), "{}", stderr(&enough));
    // The share may exceed the global cap.
    for share in ["24", "256", "512"] {
        let output = udpstp(&[
            "server",
            "--max-unactivated-test-connections",
            share,
            "--print-config",
        ]);
        assert!(output.status.success(), "{share}: {}", stderr(&output));
    }
}

// The port bound uses the effective share, including the default floor,
// and never wraps on an oversized setting.
// (conformance: unactivated-test-connection-share)
#[test]
fn the_port_range_accounts_for_configured_and_default_shares_without_overflow() {
    for (per_test, share, needed) in [("6", None, 14), ("1", Some("16"), 24)] {
        for ports in [needed - 1, needed] {
            let range = format!("50000-{}", 50000 + ports - 1);
            let mut args = vec![
                "server",
                "--max-test-connections",
                "8",
                "--test-connections-per-test",
                per_test,
                "--ephemeral-ports",
                &range,
                "--print-config",
            ];
            if let Some(share) = share {
                args.extend(["--max-unactivated-test-connections", share]);
            }
            let output = udpstp(&args);
            assert_eq!(
                output.status.success(),
                ports == needed,
                "{args:?}: {}",
                stderr(&output)
            );
        }
    }
    let largest = usize::MAX.to_string();
    for setting in [
        "--max-test-connections",
        "--max-unactivated-test-connections",
    ] {
        let output = udpstp(&["server", setting, &largest, "--print-config"]);
        assert_eq!(
            output.status.code(),
            Some(CONFIGURATION_ERROR),
            "{}",
            stderr(&output)
        );
        assert!(
            stderr(&output).contains("ephemeral_ports"),
            "{}",
            stderr(&output)
        );
    }
}

// docs/spec.md (invalid Client option combinations exit with a
// configuration error)
#[test]
fn invalid_client_test_options_are_configuration_errors() {
    for (args, mentioned) in [
        (
            vec![
                "client",
                "--start-row",
                "5",
                "--fixed-rate",
                "5",
                "192.0.2.1",
            ],
            "--fixed-rate",
        ),
        (
            vec!["client", "--refuse-coercion", "bogus", "192.0.2.1"],
            "not an RFC 9946 Activation parameter",
        ),
        (vec!["client", "--delay-var", "both", "192.0.2.1"], "both"),
        (vec!["client", "--max-hops", "0", "192.0.2.1"], "--max-hops"),
        (vec!["client", "--dscp", "64", "192.0.2.1"], "--dscp"),
        (
            vec!["client", "--max-loss-ratio", "1.5", "192.0.2.1"],
            "--max-loss-ratio",
        ),
        (
            vec!["client", "--first-mode-sub-intervals", "0", "192.0.2.1"],
            "--first-mode-sub-intervals",
        ),
        (
            vec!["client", "--dont-fragment=maybe", "192.0.2.1"],
            "--dont-fragment",
        ),
    ] {
        let output = udpstp(&args);
        assert_eq!(
            output.status.code(),
            Some(CONFIGURATION_ERROR),
            "{args:?}: {}",
            stderr(&output)
        );
        assert!(
            diagnostic(&output).contains(mentioned),
            "{args:?}: {}",
            stderr(&output)
        );
    }
}

// : the statistics interval runs from 0 (off) to a day
#[test]
fn the_statistics_interval_is_validated() {
    let output = udpstp(&["server", "--statistics-interval", "86401", "--print-config"]);
    assert_eq!(output.status.code(), Some(CONFIGURATION_ERROR));
    let file = ConfigFile::new(
        "statistics",
        "statistics_interval = 86401
",
    );
    let output = udpstp(&["server", "--config", file.path(), "--print-config"]);
    assert_eq!(output.status.code(), Some(CONFIGURATION_ERROR));
    let off = udpstp(&["server", "--statistics-interval", "0", "--print-config"]);
    assert_eq!(printed(&off, "statistics_interval").as_deref(), Some("0"));
    let defaults = udpstp(&["server", "--print-config"]);
    assert_eq!(
        printed(&defaults, "statistics_interval").as_deref(),
        Some("60")
    );
}

// docs/spec.md story 78 (a warning names the limit to raise);
#[test]
fn socket_buffer_sizes_are_validated() {
    for bad in ["65535", "268435457"] {
        let output = udpstp(&["client", "--socket-buffer-size", bad, "192.0.2.1"]);
        assert_eq!(output.status.code(), Some(CONFIGURATION_ERROR), "{bad}");
        let output = udpstp(&["server", "--socket-buffer-size", bad, "--print-config"]);
        assert_eq!(output.status.code(), Some(CONFIGURATION_ERROR), "{bad}");
    }
    let defaults = udpstp(&["server", "--print-config"]);
    assert_eq!(
        printed(&defaults, "socket_buffer_size").as_deref(),
        Some("2097152")
    );
}

// : the Test Connection options are checked before anything is sent;
// less than 1 Mbps a Test Connection is refused
// (conformance: max-bandwidth-split-across-test-connections)
#[test]
fn multi_connection_options_are_validated() {
    let cases: [(&str, &[&str]); 4] = [
        (
            "fewer Test Connections than Servers",
            &["--test-connections", "1", "192.0.2.1", "192.0.2.2"],
        ),
        (
            "a minimum above the Test Connections",
            &[
                "--test-connections",
                "2",
                "--min-test-connections",
                "3",
                "192.0.2.1",
            ],
        ),
        (
            "less than 1 Mbps a Test Connection",
            &[
                "--test-connections",
                "4",
                "--max-bandwidth",
                "3",
                "192.0.2.1",
            ],
        ),
        (
            "IPv4 and IPv6 Servers together",
            &["192.0.2.1", "[2001:db8::1]"],
        ),
    ];
    for (what, args) in cases {
        let mut all = vec!["client", "--unauthenticated"];
        all.extend_from_slice(args);
        let output = udpstp(&all);
        assert_eq!(
            output.status.code(),
            Some(CONFIGURATION_ERROR),
            "{what}: {}",
            stderr(&output)
        );
        assert!(
            diagnostic(&output).contains("configuration error"),
            "{what}"
        );
    }
}

// : mode 1 is chosen for a SERVER that is among the SERVERs
#[test]
fn mode_1_is_chosen_for_one_of_the_servers() {
    let output = udpstp(&[
        "client",
        "--key-file",
        "keys.toml",
        "--mode-1-server",
        "192.0.2.9",
        "192.0.2.1",
    ]);
    assert_eq!(
        output.status.code(),
        Some(CONFIGURATION_ERROR),
        "{}",
        stderr(&output)
    );
    assert!(
        diagnostic(&output).contains("isn't one of the SERVERs"),
        "{}",
        stderr(&output)
    );
}

// : the listen addresses, checked like the other settings
#[test]
fn listen_addresses_are_validated() {
    let output = udpstp(&[
        "server",
        "--listen",
        "127.0.0.1",
        "--listen",
        "::1",
        "--print-config",
    ]);
    assert_eq!(
        printed(&output, "listen").as_deref(),
        Some(r#"["127.0.0.1", "::1"]"#),
        "{}",
        stdout(&output)
    );
    for bad in [["--listen", "224.0.0.1"], ["--listen", "255.255.255.255"]] {
        let output = udpstp(&["server", bad[0], bad[1], "--print-config"]);
        assert_eq!(output.status.code(), Some(CONFIGURATION_ERROR), "{bad:?}");
    }
    let twice = udpstp(&[
        "server",
        "--listen",
        "::1",
        "--listen",
        "::1",
        "--print-config",
    ]);
    assert_eq!(twice.status.code(), Some(CONFIGURATION_ERROR));
    let defaults = udpstp(&["server", "--print-config"]);
    assert_eq!(printed(&defaults, "listen").as_deref(), Some("[]"));
}

// : the start row cap
#[test]
fn the_start_row_cap_is_a_setting() {
    let defaults = udpstp(&["server", "--print-config"]);
    assert_eq!(printed(&defaults, "max_start_row").as_deref(), Some("100"));
    let set = udpstp(&["server", "--max-start-row", "500", "--print-config"]);
    assert_eq!(printed(&set, "max_start_row").as_deref(), Some("500"));
    let bad = udpstp(&["server", "--max-start-row", "65535", "--print-config"]);
    assert_eq!(bad.status.code(), Some(CONFIGURATION_ERROR));
}

// : Quotas, none by default
#[test]
fn quotas_are_settings() {
    let defaults = udpstp(&["server", "--print-config"]);
    assert_eq!(printed(&defaults, "quota_period").as_deref(), Some("3600"));
    assert_eq!(printed(&defaults, "source_tests_per_quota_period"), None);
    let set = udpstp(&[
        "server",
        "--quota-period",
        "86400",
        "--source-tests-per-quota-period",
        "24",
        "--source-bytes-per-quota-period",
        "10000000000",
        "--print-config",
    ]);
    assert_eq!(printed(&set, "quota_period").as_deref(), Some("86400"));
    assert_eq!(
        printed(&set, "source_tests_per_quota_period").as_deref(),
        Some("24")
    );
    assert_eq!(
        printed(&set, "source_bytes_per_quota_period").as_deref(),
        Some("10000000000")
    );
    for bad in [
        ["--quota-period", "0"],
        ["--source-tests-per-quota-period", "0"],
    ] {
        let output = udpstp(&["server", bad[0], bad[1], "--print-config"]);
        assert_eq!(output.status.code(), Some(CONFIGURATION_ERROR), "{bad:?}");
    }
}

// : one Test's share of the Admitted Rate Cap
#[test]
fn the_test_share_is_a_setting() {
    let defaults = udpstp(&["server", "--print-config"]);
    assert_eq!(printed(&defaults, "max_test_share").as_deref(), Some("50"));
    for bad in ["0", "101"] {
        let output = udpstp(&["server", "--max-test-share", bad, "--print-config"]);
        assert_eq!(output.status.code(), Some(CONFIGURATION_ERROR), "{bad}");
    }
}

// : the Sending Rate Table's variant and the Server's MTU
#[udpstp_req::req("RFC9946-6.1-10")]
#[test]
fn the_table_variant_and_server_mtu_are_settings() {
    let defaults = udpstp(&["server", "--print-config"]);
    assert_eq!(printed(&defaults, "jumbo").as_deref(), Some("true"));
    assert_eq!(
        printed(&defaults, "traditional_mtu").as_deref(),
        Some("false")
    );
    assert_eq!(printed(&defaults, "server_mtu").as_deref(), Some("1500"));
    let small = udpstp(&["server", "--server-mtu", "1279", "--print-config"]);
    assert_eq!(small.status.code(), Some(CONFIGURATION_ERROR));
    let traditional = udpstp(&[
        "server",
        "--traditional-mtu",
        "--server-mtu",
        "1400",
        "--print-config",
    ]);
    assert_eq!(traditional.status.code(), Some(CONFIGURATION_ERROR));
    // : the old name is refused with a hint to the new one, in a
    // configuration file too, whatever else the file holds.
    let dir = std::env::temp_dir().join(format!("udpstp-old-mtu-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("server.toml");
    std::fs::write(&file, "mtu = 1500\n").unwrap();
    let old = udpstp(&[
        "server",
        "--config",
        file.to_str().unwrap(),
        "--print-config",
    ]);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(old.status.code(), Some(CONFIGURATION_ERROR));
    assert!(diagnostic(&old).contains("renamed server_mtu"), "{old:?}");
    let renamed = udpstp(&["server", "--mtu", "1500", "--print-config"]);
    assert_eq!(renamed.status.code(), Some(CONFIGURATION_ERROR));
    assert!(
        String::from_utf8_lossy(&renamed.stderr).contains("--server-mtu"),
        "{renamed:?}"
    );
}

// : rows are the table's, 0 to 1152, and Authentication Modes need keys
#[test]
fn test_options_that_can_t_go_together_are_refused() {
    for args in [
        &["--unauthenticated", "--fixed-rate", "1153", "192.0.2.1"][..],
        &["--unauthenticated", "--start-row", "65535", "192.0.2.1"],
        &["--unauthenticated", "--auth-mode", "1", "192.0.2.1"],
        &[
            "--unauthenticated",
            "--fixed-rate",
            "5",
            "--start-row",
            "5",
            "192.0.2.1",
        ],
    ] {
        let mut all = vec!["client"];
        all.extend_from_slice(args);
        let output = udpstp(&all);
        assert_eq!(output.status.code(), Some(CONFIGURATION_ERROR), "{args:?}");
    }
}

// : a Client with no key and no --unauthenticated says so before it
// sends anything.
#[test]
fn a_client_without_a_key_says_so_before_sending() {
    let output = udpstp(&["client", "192.0.2.1"]);
    assert_eq!(output.status.code(), Some(50), "{}", stderr(&output));
    assert!(
        diagnostic(&output).contains("no key to authenticate with"),
        "{}",
        stderr(&output)
    );
    assert!(
        !stdout(&output).contains("Sending") && !stderr(&output).contains("Sending"),
        "{}",
        stdout(&output)
    );
}

/// The lines of the log files a `log_file` of `dir/server.log` writes.
fn log_file_lines(dir: &std::path::Path) -> Vec<String> {
    let mut lines = Vec::new();
    for entry in std::fs::read_dir(dir).expect("the log's directory") {
        let path = entry.expect("an entry").path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.starts_with("server.") && name.ends_with(".log") && name != "server.log" {
            let text = std::fs::read_to_string(&path).expect("a log file");
            lines.extend(text.lines().map(str::to_owned));
        }
    }
    lines
}

//  (with a log file named on the command line, a configuration error
// goes into it, with its exit status, as well as to stderr: a Windows
// service has no stderr)
#[test]
fn a_configuration_error_reaches_the_log_file() {
    let dir = ConfigFile::new("log-file-error", "");
    let log_file = dir.dir.join("server.log");
    let output = udpstp(&[
        "server",
        "--log-file",
        log_file.to_str().expect("UTF-8 temp path"),
        "--log-format",
        "json",
        "--source-setups-per-second",
        "0",
    ]);
    assert_eq!(output.status.code(), Some(50), "{}", stderr(&output));
    assert!(stderr(&output).contains("source_setups_per_second"));
    let lines = log_file_lines(&dir.dir);
    assert!(
        lines
            .iter()
            .any(|line| line.contains(r#""event":"StartupError""#)
                && line.contains(r#""exit_status":50"#)
                && line.contains("source_setups_per_second")),
        "{lines:?}"
    );
}

//  (--print-config writes no log file)
#[test]
fn print_config_writes_no_log_file() {
    let dir = ConfigFile::new("log-file-print", "");
    let log_file = dir.dir.join("server.log");
    let output = udpstp(&[
        "server",
        "--log-file",
        log_file.to_str().expect("UTF-8 temp path"),
        "--print-config",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(
        printed(&output, "log_file").is_some(),
        "{}",
        stdout(&output)
    );
    assert!(log_file_lines(&dir.dir).is_empty());
}

//  (--windows-service runs only where the Service Control Manager
// started the process, and only on Windows)
#[test]
fn windows_service_outside_a_service_is_a_configuration_error() {
    let dir = ConfigFile::new("windows-service", "");
    let log_file = dir.dir.join("server.log");
    let output = udpstp(&[
        "server",
        "--windows-service",
        "--log-file",
        log_file.to_str().expect("UTF-8 temp path"),
    ]);
    assert_eq!(output.status.code(), Some(50), "{}", stderr(&output));
    let expected = if cfg!(windows) {
        "the Service Control Manager didn't start this process"
    } else {
        "runs only on Windows"
    };
    assert!(
        diagnostic(&output).contains(expected),
        "{}",
        stderr(&output)
    );
}

// The Windows exe runs without the Visual C++ Redistributable: the C
// runtime is linked in (.cargo/config.toml), so it names no vcruntime DLL.
#[cfg(windows)]
#[test]
fn the_windows_exe_needs_no_visual_cpp_runtime() {
    let exe = std::fs::read(env!("CARGO_BIN_EXE_udpstp")).expect("the built udpstp.exe");
    let lower: Vec<u8> = exe.iter().map(u8::to_ascii_lowercase).collect();
    assert!(
        !lower.windows(9).any(|w| w == b"vcruntime"),
        "udpstp.exe imports the Visual C++ runtime; is RUSTFLAGS set without +crt-static?"
    );
}
