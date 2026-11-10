//! The generated reference pages: the command line from `udpstp --help`,
//! the Server's configuration from `--print-config` and the settings
//! struct's documentation, the exit statuses from `exit.rs`, and key files
//! from the Hosting guide and `keyfile.rs`.

use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

use std::collections::BTreeMap;

use super::read;

/// Conformance Record IDs to their page in the book.
pub type RecordPages = BTreeMap<String, String>;

/// A Markdown link to a record, from a page under `reference/`.
fn record_link(records: &RecordPages, id: &str) -> Result<String, String> {
    let page = records
        .get(id)
        .ok_or_else(|| format!("a reference page names an unknown Conformance Record: {id}"))?;
    Ok(format!("[{id}](../{page}#{id})"))
}

/// The generated pages, as (book path, Markdown).
pub fn pages(
    root: &Path,
    binary: &Path,
    records: &RecordPages,
) -> Result<Vec<(String, String)>, String> {
    Ok(vec![
        ("reference/cli.md".into(), cli(binary)?),
        (
            "reference/configuration.md".into(),
            configuration(root, binary)?,
        ),
        (
            "reference/exit-statuses.md".into(),
            exit_statuses(root, records)?,
        ),
        ("reference/key-files.md".into(), key_files(root, records)?),
    ])
}

/// Runs the binary in a fresh, empty directory, so that nothing there (a
/// configuration or key file) changes what it prints, and with no
/// environment but PATH.
fn run(binary: &Path, args: &[&str]) -> Result<String, String> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let dir = std::env::temp_dir().join(format!("udpstp-site-{}-{nanos}", std::process::id()));
    std::fs::create_dir(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let output = Command::new(binary)
        .args(args)
        .current_dir(&dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .output();
    let _ = std::fs::remove_dir(&dir);
    let output =
        output.map_err(|e| format!("running {} {}: {e}", binary.display(), args.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "{} {} failed: {}",
            binary.display(),
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    String::from_utf8(output.stdout).map_err(|e| format!("{}: {e}", args.join(" ")))
}

/// Escapes Markdown text for a table cell: HTML escaped outside code spans,
/// and `|`, which ends a cell even in a code span, escaped everywhere.
fn cell(text: &str) -> String {
    super::markdown::escape_outside_code(text, super::html_escape).replace('|', "\\|")
}

/// Escapes plain text for a table cell inside a `<code>` element.
fn code_cell(text: &str) -> String {
    super::html_escape(text).replace('|', "&#124;")
}

/// One option of clap's long help: its header line and its description.
#[derive(Debug, PartialEq)]
struct HelpOption {
    header: String,
    description: String,
}

/// The positional arguments and options of clap's long help.
fn parse_help(help: &str) -> (Vec<HelpOption>, Vec<HelpOption>) {
    let mut arguments = Vec::new();
    let mut options = Vec::new();
    let mut section = "";
    let mut current: Option<HelpOption> = None;
    let flush = |current: &mut Option<HelpOption>,
                 section: &str,
                 arguments: &mut Vec<HelpOption>,
                 options: &mut Vec<HelpOption>| {
        if let Some(mut option) = current.take() {
            option.description = option.description.trim().to_string();
            match section {
                "Arguments:" => arguments.push(option),
                _ => options.push(option),
            }
        }
    };
    for line in help.lines() {
        if !line.starts_with(' ') && line.ends_with(':') {
            flush(&mut current, section, &mut arguments, &mut options);
            // The examples that follow the options aren't options.
            section = match line {
                "Arguments:" => "Arguments:",
                "Options:" => "Options:",
                _ => "",
            };
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if (2..=6).contains(&indent) && !section.is_empty() {
            flush(&mut current, section, &mut arguments, &mut options);
            current = Some(HelpOption {
                header: line.trim().to_string(),
                description: String::new(),
            });
        } else if let Some(option) = current.as_mut() {
            let text = line.trim();
            if text.is_empty() {
                continue;
            }
            if !option.description.is_empty() {
                option.description.push(' ');
            }
            option.description.push_str(text);
        }
    }
    flush(&mut current, section, &mut arguments, &mut options);
    (arguments, options)
}

fn options_table(out: &mut String, options: &[HelpOption]) {
    out.push_str("| Option | Description |\n|---|---|\n");
    for option in options {
        let _ = writeln!(
            out,
            "| <code>{}</code> | {} |",
            code_cell(&option.header),
            cell(&option.description)
        );
    }
    out.push('\n');
}

fn cli(binary: &Path) -> Result<String, String> {
    let top = run(binary, &["--help"])?;
    let mut out = String::from(
        "# Command line\n\n\
         Every option of `udpstp client` and `udpstp server`, generated from the \
         binary's own `--help` output when the site was built. \
         `udpstp client --help` and `udpstp server --help` print the same.\n\n",
    );
    let _ = writeln!(out, "```text\n$ udpstp --help\n{}```\n", top);
    for (command, intro) in [
        (
            "client",
            "Runs a Test. [Running a Test](../guides/running-a-test.md) shows the options in use.",
        ),
        (
            "server",
            "Serves Tests. Every option also has a setting in the configuration file \
             ([Server configuration](configuration.md)); an option given on the command line \
             replaces the file's value. [Hosting a Server](../guides/hosting-a-server.md) \
             shows them in use.",
        ),
    ] {
        let help = run(binary, &[command, "--help"])?;
        let (arguments, options) = parse_help(&help);
        let usage = help
            .lines()
            .find(|l| l.starts_with("Usage:"))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "## udpstp {command}\n\n{intro}\n\n```text\n{usage}\n```\n"
        );
        if !arguments.is_empty() {
            out.push_str("| Argument | Description |\n|---|---|\n");
            for argument in &arguments {
                let _ = writeln!(
                    out,
                    "| <code>{}</code> | {} |",
                    code_cell(&argument.header),
                    cell(&argument.description)
                );
            }
            out.push('\n');
        }
        options_table(&mut out, &options);
    }
    Ok(out)
}

/// One field of the settings struct and its documentation.
#[derive(Debug, PartialEq)]
struct Field {
    name: String,
    doc: String,
}

/// The documented public fields of `struct <name>` in Rust source.
fn struct_fields(source: &str, name: &str) -> Vec<Field> {
    let Some(start) = source.find(&format!("struct {name} {{")) else {
        return Vec::new();
    };
    let body = &source[start..];
    let body = &body[..body.find("\n}").unwrap_or(body.len())];
    let mut fields = Vec::new();
    let mut doc = Vec::new();
    for line in body.lines().skip(1) {
        let line = line.trim();
        if let Some(text) = line.strip_prefix("///") {
            doc.push(text.trim().to_string());
        } else if line.starts_with("#[") {
            continue;
        } else if let Some((field, _)) = line
            .strip_prefix("pub ")
            .unwrap_or(line)
            .split_once(':')
            .filter(|_| line.ends_with(','))
        {
            fields.push(Field {
                name: field.trim().to_string(),
                doc: doc.join(" "),
            });
            doc.clear();
        }
    }
    fields
}

fn configuration(root: &Path, binary: &Path) -> Result<String, String> {
    let source = read(&root.join("crates/udpstp/src/config.rs"))?;
    let fields = struct_fields(&source, "ServerSettings");
    if fields.is_empty() {
        return Err("config.rs: no ServerSettings fields found".into());
    }
    let printed = run(binary, &["server", "--print-config"])?;
    let defaults: toml::Table =
        toml::from_str(&printed).map_err(|e| format!("--print-config output: {e}"))?;
    let mut out = String::from(
        "# Server configuration\n\n\
         The Server reads its settings from built-in defaults, then a TOML file \
         (`udpstp server --config FILE`), then the command line, each replacing the one \
         before. This page is generated from the settings' own documentation and from \
         `udpstp server --print-config`, which prints the effective settings as TOML and \
         exits.\n\n\
         ## Defaults\n\n\
         With no file and no options, `--print-config` prints:\n\n",
    );
    let _ = writeln!(out, "```toml\n{}```\n", printed);
    out.push_str(
        "A setting shown as \"none\" is absent by default and printed only when set.\n\n\
         ## Settings\n\n| Setting | Default | Meaning |\n|---|---|---|\n",
    );
    for field in &fields {
        let default = defaults
            .get(&field.name)
            .map(|v| format!("<code>{}</code>", code_cell(&v.to_string())))
            .unwrap_or_else(|| "none".into());
        let _ = writeln!(
            out,
            "| <code>{name}</code> | {default} | {doc} |",
            name = field.name,
            doc = cell(&field.doc)
        );
    }
    out.push_str(
        "\n[Hosting a Server](../guides/hosting-a-server.md) explains the settings in use, \
         and [Command line](cli.md#udpstp-server) lists the option for each.\n",
    );
    Ok(out)
}

fn exit_statuses(root: &Path, records: &RecordPages) -> Result<String, String> {
    let source = read(&root.join("crates/udpstp/src/exit.rs"))?;
    let doc: Vec<&str> = source
        .lines()
        .map_while(|l| l.strip_prefix("//!"))
        .map(|l| l.strip_prefix(' ').unwrap_or(l))
        .collect();
    let table: Vec<&str> = doc
        .iter()
        .copied()
        .skip_while(|l| !l.starts_with('|'))
        .take_while(|l| l.starts_with('|'))
        .collect();
    if table.is_empty() {
        return Err("exit.rs: no exit status table in the module documentation".into());
    }
    let after: Vec<&str> = doc
        .iter()
        .copied()
        .skip_while(|l| !l.starts_with('|'))
        .skip_while(|l| l.starts_with('|'))
        .take_while(|l| !l.starts_with("(conformance:"))
        .collect();
    let mut out = String::from(
        "# Exit statuses\n\n\
         `udpstp client` exits with the ErrorStatus that OB-UDPST reports, except that a Test \
         that completed with only warnings exits 0. In JSON results the same number is \
         `ErrorStatus`, warnings included. Generated from the documentation of \
         `crates/udpstp/src/exit.rs`.\n\n",
    );
    for line in table {
        out.push_str(line);
        out.push('\n');
    }
    out.push('\n');
    out.push_str(after.join("\n").trim());
    let links = doc
        .iter()
        .find_map(|l| l.strip_prefix("(conformance: "))
        .map(|ids| {
            ids.trim_end_matches(')')
                .split(',')
                .map(str::trim)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
        .into_iter()
        .map(|id| record_link(records, id))
        .collect::<Result<Vec<_>, _>>()?;
    let _ = writeln!(out, "\n\nConformance Records: {}.", links.join(", "));
    Ok(out)
}

/// The recorded choices from `crates/udpstp/src/keyfile.rs`, which follow
/// the key file format from `docs/key-files.md` on its page.
fn key_files(root: &Path, records: &RecordPages) -> Result<String, String> {
    let source = read(&root.join("crates/udpstp/src/keyfile.rs"))?;
    let choices: Vec<String> = source
        .lines()
        .map_while(|l| l.strip_prefix("//!"))
        .map(|l| l.trim().to_string())
        .skip_while(|l| l != "Recorded choices:")
        .skip(1)
        // The list ends at the first blank line; prose may follow it.
        .take_while(|l| !l.is_empty())
        .collect();
    let mut out = String::from("\n\n## Recorded choices\n\n");
    let joined = choices.join(" ");
    for item in joined.split("- ").map(str::trim).filter(|i| !i.is_empty()) {
        let (text, ids) = match item.split_once("(conformance: ") {
            Some((text, ids)) => (
                text.trim().trim_end_matches(','),
                ids.trim_end_matches([')', ';', '.']),
            ),
            None => (item.trim_end_matches([';', '.']), ""),
        };
        let links = ids
            .split(',')
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(|id| record_link(records, id))
            .collect::<Result<Vec<_>, _>>()?;
        if links.is_empty() {
            let _ = writeln!(out, "- {}.", text.trim_end_matches(';'));
        } else {
            let _ = writeln!(
                out,
                "- {} ({}).",
                text.trim_end_matches(';'),
                links.join(", ")
            );
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_clap_long_help() {
        let help = "Run a Test\n\nUsage: udpstp client [OPTIONS] <SERVER>...\n\nArguments:\n  <SERVER>...\n          The Server.\n          Several.\n\nOptions:\n      --key-file <FILE>\n          The key file\n\n          [default: 2]\n\n  -h, --help\n          Print help\n\nExamples:\n  udpstp client --key-file keys.toml server.example.net\n\nDocumentation: https://udpstp.com/\n";
        let (arguments, options) = parse_help(help);
        assert_eq!(arguments[0].header, "<SERVER>...");
        assert_eq!(arguments[0].description, "The Server. Several.");
        assert_eq!(options.len(), 2);
        assert_eq!(options[0].description, "The key file [default: 2]");
        assert_eq!(options[1].header, "-h, --help");
        // The examples and the link after the options aren't options.
        assert_eq!(options[1].description, "Print help");
    }

    #[test]
    fn reads_documented_struct_fields() {
        let source = "pub struct S {\n    /// The port.\n    /// Really.\n    pub port: u16,\n    #[serde(skip)]\n    /// Maybe.\n    pub cap: Option<u32>,\n}\n";
        assert_eq!(
            struct_fields(source, "S"),
            [
                Field {
                    name: "port".into(),
                    doc: "The port. Really.".into()
                },
                Field {
                    name: "cap".into(),
                    doc: "Maybe.".into()
                },
            ]
        );
    }
}
