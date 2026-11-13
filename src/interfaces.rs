#![forbid(unsafe_code)]
//! Structural tests of udpstp's stable command-line and configuration
//! interfaces (ADR 0013): the CLI's options, their spellings and
//! defaults, read from clap's Command model, and every key of the Server's
//! TOML settings, from a fully populated settings struct. Each is compared
//! with a checked-in list under `tests/golden/`, which `UDPSTP_BLESS=1`
//! rewrites after an intended change; a change there needs a changelog
//! entry and, for a rename or removal, a deprecation period
//! (docs/interfaces.md).
//!
//! The other stable interfaces are tested beside their code: the log's
//! event names and fields (`names.rs`, `log.rs`), the JSON results
//! (`json.rs`), exit statuses (`exit.rs`), the Key Table's columns
//! (`keyfile.rs`) and the pseudonym algorithm (`privacy.rs`).

use std::collections::BTreeSet;
use std::net::IpAddr;
use std::path::PathBuf;

use clap::{Arg, Command, CommandFactory, Parser, ValueEnum};

use crate::config::{DelayVar, ServerSettings};
use crate::log::LogFormat;
use crate::privacy::AddressPrivacy;
use crate::results::tests::golden;
use crate::{Cli, Command as Subcommand};

/// One line per option of `command` and its subcommands, in the order
/// `--help` lists them: the option's name, how it takes values, its value
/// names, the values it accepts, its default, and whether it is required,
/// hidden or repeatable.
fn describe(command: &Command, path: &str, lines: &mut Vec<String>) {
    let path = if path.is_empty() {
        command.get_name().to_owned()
    } else {
        format!("{path} {}", command.get_name())
    };
    lines.push(format!("{path}\t(command)"));
    for arg in command.get_arguments() {
        lines.push(format!("{path}\t{}", describe_arg(arg)));
    }
    for sub in command.get_subcommands() {
        describe(sub, &path, lines);
    }
}

fn describe_arg(arg: &Arg) -> String {
    let name = match (arg.get_long(), arg.get_short()) {
        (Some(long), Some(short)) => format!("--{long} -{short}"),
        (Some(long), None) => format!("--{long}"),
        (None, Some(short)) => format!("-{short}"),
        (None, None) => format!("<{}>", arg.get_id()),
    };
    let mut fields = vec![name, format!("action={:?}", arg.get_action())];
    if let Some(names) = arg.get_value_names() {
        let names: Vec<&str> = names.iter().map(|n| n.as_str()).collect();
        fields.push(format!("value={}", names.join(",")));
    }
    if let Some(range) = arg.get_num_args() {
        fields.push(format!("num_args={range}"));
    }
    let possible: Vec<String> = arg
        .get_possible_values()
        .iter()
        .map(|value| {
            let names: Vec<&str> = value.get_name_and_aliases().collect();
            let hidden = if value.is_hide_set() { " (hidden)" } else { "" };
            format!("{}{hidden}", names.join("|"))
        })
        .collect();
    if !possible.is_empty() {
        fields.push(format!("possible={}", possible.join(",")));
    }
    let defaults: Vec<String> = arg
        .get_default_values()
        .iter()
        .map(|d| d.to_string_lossy().into_owned())
        .collect();
    if !defaults.is_empty() {
        fields.push(format!("default={}", defaults.join(",")));
    }
    if let Some(delimiter) = arg.get_value_delimiter() {
        fields.push(format!("delimiter={delimiter}"));
    }
    if let Some(aliases) = arg.get_all_aliases() {
        fields.push(format!("aliases={}", aliases.join(",")));
    }
    for (set, flag) in [
        (arg.is_required_set(), "required"),
        (arg.is_require_equals_set(), "require_equals"),
        (arg.is_hide_set(), "hidden"),
    ] {
        if set {
            fields.push(flag.to_owned());
        }
    }
    fields.join("\t")
}

//  (the CLI's option names, value spellings and defaults are stable
// from 0.1.0; read from clap's Command model, never from --help's text)
#[test]
fn every_cli_option_is_in_the_checked_in_list() {
    let mut command = Cli::command();
    command.build();
    let mut lines = Vec::new();
    describe(&command, "", &mut lines);
    let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
    golden("cli.txt", &text);
}

/// The Server's settings with every key present, none at its default, and
/// together valid. A struct literal, so a new setting fails to compile
/// here until it is added.
fn every_setting() -> ServerSettings {
    ServerSettings {
        listen: vec![
            "192.0.2.1".parse::<IpAddr>().unwrap(),
            "2001:db8::1".parse().unwrap(),
        ],
        port: 24700,
        allow_unauthenticated: true,
        checksum: true,
        jumbo: false,
        traditional_mtu: true,
        server_mtu: 9000,
        ephemeral_ports: "40000-40999".into(),
        delay_var_statistic: DelayVar::Average,
        tests_per_source: 2,
        max_test_connections: 512,
        test_connections_per_test: 16,
        max_unactivated_test_connections: Some(64),
        admitted_rate_cap: Some(10_000),
        max_test_share: 25,
        require_max_bandwidth: true,
        troubleshooting: true,
        unverified_setups_per_second: 1000,
        source_setup_burst: 32,
        source_setups_per_second: 2,
        key_setup_burst: 500,
        key_setups_per_second: 200,
        allowed_dscp: vec![0, 46],
        operator_allowed_dscp: Some(vec![0, 10, 46]),
        max_start_row: 50,
        quota_period: 86_400,
        source_tests_per_quota_period: Some(24),
        source_bytes_per_quota_period: Some(10_000_000_000),
        socket_buffer_size: 4 << 20,
        dont_fragment: false,
        max_hops: Some(12),
        log_format: LogFormat::Json,
        log_file: Some(PathBuf::from("/var/log/udpstp/server.log")),
        address_privacy: AddressPrivacy::Pseudonymize,
        address_privacy_key_file: Some(PathBuf::from("/etc/udpstp/pseudonym.key")),
        statistics_interval: 300,
        key_file: Some(PathBuf::from("/etc/udpstp/keys.toml")),
    }
}

//  (every TOML key and every default of the Server's settings is
// stable from 0.1.0)
#[test]
fn every_server_setting_is_in_the_checked_in_list() {
    let every = every_setting();
    let text = every.to_toml();
    golden("server-settings.toml", &text);
    golden("server-defaults.toml", &ServerSettings::default().to_toml());
    // The file reads back as written.
    assert_eq!(toml::from_str::<ServerSettings>(&text).unwrap(), every);
    // Every key is present, and none is at its default, so a renamed
    // default can't hide behind an equal value.
    let table: toml::Table = toml::from_str(&text).unwrap();
    let defaults: toml::Table = toml::from_str(&ServerSettings::default().to_toml()).unwrap();
    for (key, value) in &table {
        assert_ne!(defaults.get(key), Some(value), "{key} is at its default");
    }
}

/// The TOML keys, and the Server's options that set them.
fn server_keys_and_options() -> (toml::Table, Command) {
    let table: toml::Table = toml::from_str(&every_setting().to_toml()).unwrap();
    let command = Cli::command()
        .find_subcommand("server")
        .expect("a server subcommand")
        .clone();
    (table, command)
}

// ; hosting-a-server.md ("every setting has a command-line flag and a
// TOML key of the same name"): each option sets the key of its name.
#[test]
fn every_server_option_sets_the_toml_key_of_its_name() {
    let (table, command) = server_keys_and_options();
    // Options that aren't settings: the file, printing it, and renamed
    // options kept only to say so.
    let not_settings = [
        "color",
        "config",
        "print-config",
        "settings-only",
        "windows-service",
        "mtu",
        "delay-var",
        "help",
    ];
    let options: BTreeSet<String> = command
        .get_arguments()
        .filter_map(Arg::get_long)
        .filter(|long| !not_settings.contains(long))
        .map(|long| long.replace('-', "_"))
        .collect();
    let keys: BTreeSet<String> = table.keys().cloned().collect();
    assert_eq!(options, keys, "the Server's options and TOML keys differ");
    // Each option, given the populated value, sets its key to it.
    let mut args = vec!["udpstp".to_owned(), "server".to_owned()];
    for (key, value) in &table {
        let option = format!("--{}", key.replace('_', "-"));
        let values: Vec<&toml::Value> = match value {
            toml::Value::Array(items) => items.iter().collect(),
            other => vec![other],
        };
        for value in values {
            match value {
                toml::Value::Boolean(b) => args.push(format!("{option}={b}")),
                toml::Value::String(s) => args.extend([option.clone(), s.clone()]),
                other => args.extend([option.clone(), other.to_string()]),
            }
        }
    }
    let cli = Cli::try_parse_from(&args).unwrap_or_else(|e| panic!("{e}"));
    let Subcommand::Server(server) = cli.command else {
        panic!("not the server subcommand");
    };
    let loaded = ServerSettings::load(None, &server.overrides).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(loaded, every_setting());
}

/// A value's spelling in TOML.
fn toml_spelling<T: serde::Serialize>(value: &T) -> String {
    #[derive(serde::Serialize)]
    struct Wrapper<'a, T> {
        v: &'a T,
    }
    let text = toml::to_string(&Wrapper { v: value }).unwrap();
    let table: toml::Table = toml::from_str(&text).unwrap();
    table["v"].as_str().expect("a string").to_owned()
}

fn same_spellings<T: ValueEnum + serde::Serialize + std::fmt::Debug>() {
    for variant in T::value_variants() {
        let clap = variant.to_possible_value().expect("not skipped");
        assert_eq!(
            clap.get_name(),
            toml_spelling(variant),
            "{variant:?} is spelled differently on the command line and in TOML"
        );
    }
}

//  (a setting's values are spelled alike in TOML and on the command
// line, so cli.txt's spellings are the TOML ones too)
#[test]
fn enumerated_settings_are_spelled_alike_in_toml_and_on_the_command_line() {
    same_spellings::<DelayVar>();
    same_spellings::<LogFormat>();
    same_spellings::<AddressPrivacy>();
}
