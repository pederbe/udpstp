//! ADR 0015 and spec story 39's refusal policy, at the native file and
//! process seams. Fixture PowerShell runs only in tests.
#![cfg(windows)]

mod common;
#[path = "common/human_output.rs"]
mod human_output;
#[path = "common/key_file.rs"]
mod key_file;
#[cfg(any(windows, test))]
#[path = "../src/windows_permissions/mod.rs"]
#[allow(dead_code, unused_imports)]
mod windows_permissions;

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Read};
use std::net::UdpSocket;
use std::os::windows::fs::OpenOptionsExt as _;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use crate::windows_permissions::{Assessment, assess, open};
use key_file::KeyFile;

const SECRET: &str = "s3cr3t";

fn powershell(script: &str, file: &Path, extra: &str) -> Output {
    let mut command = Command::new("powershell.exe");
    // Windows PowerShell must not inherit PowerShell 7's binary module paths.
    command.env_remove("PSModulePath");
    command.args(["-NoProfile", "-NonInteractive", "-Command", script]);
    command
        .env("UDPSTP_FIXTURE_FILE", file)
        .env("UDPSTP_FIXTURE_EXTRA", extra);
    let output = common::run_bounded(command, Duration::from_secs(30));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn set_acl(path: &Path, extra: &str) {
    powershell(
        r#"$ErrorActionPreference='Stop'; $me=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value;
        $acl=New-Object Security.AccessControl.FileSecurity;
        $sections=[Security.AccessControl.AccessControlSections]::Owner -bor [Security.AccessControl.AccessControlSections]::Access;
        $acl.SetSecurityDescriptorSddlForm("O:${me}D:P"+$env:UDPSTP_FIXTURE_EXTRA+"(A;;FA;;;$me)(A;;FA;;;SY)(A;;FA;;;BA)", $sections);
        [IO.File]::SetAccessControl($env:UDPSTP_FIXTURE_FILE, $acl)"#,
        path,
        extra,
    );
}

fn run(args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_udpstp"));
    command.args(args);
    common::run_bounded(command, Duration::from_secs(15))
}

fn assert_permissions(output: &Output, path: &str, expected: Option<&str>) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let text = human_output::prose(&stderr);
    if let Some(expected) = expected {
        assert!(text.contains(expected), "{text}");
        assert!(stderr.contains(path), "path must stay intact: {stderr}");
        assert!(text.contains("file refused"), "{text}");
        assert!(
            !text.contains("exposed"),
            "unsupported is not exposure: {text}"
        );
        assert!(
            text.contains("docs/guides/windows.md"),
            "remediation: {text}"
        );
    } else {
        assert!(
            !text.contains("Shared Key file"),
            "private file must be quiet: {text}"
        );
    }
    for stream in [&output.stdout, &output.stderr] {
        assert!(
            !String::from_utf8_lossy(stream).contains(SECRET),
            "key material in output"
        );
    }
}

// ADR 0015: supported structural policy and unsupported-case precedence.
#[cfg(windows)]
#[test]
fn native_acl_matrix_uses_the_production_assessment() {
    let cases = [
        ("private", "", "meets"),
        ("read", "(A;;FR;;;BU)", "mismatch"),
        ("write", "(A;;FW;;;BU)", "mismatch"),
        ("dac", "(A;;WD;;;BU)", "mismatch"),
        ("owner", "(A;;WO;;;BU)", "mismatch"),
        ("delete", "(A;;SD;;;BU)", "mismatch"),
        ("generic-read", "(A;;GR;;;BU)", "mismatch"),
        ("generic-write", "(A;;GW;;;BU)", "mismatch"),
        ("generic-execute", "(A;;GX;;;BU)", "mismatch"),
        ("generic-all", "(A;;GA;;;BU)", "mismatch"),
        ("metadata", "(A;;0x00120088;;;BU)", "meets"),
        ("inherited", "(A;ID;FR;;;BU)", "mismatch"),
        ("inherit-only", "(A;OIIO;FR;;;BU)", "meets"),
        ("deny-plus-allow", "(D;;FR;;;BG)(A;;FR;;;BG)", "mismatch"),
        (
            "callback",
            "(XA;;FR;;;BU;(Member_of {SID(BA)}))",
            "unverified",
        ),
        (
            "partial",
            "(A;;FR;;;BU)(XA;;FR;;;BU;(Member_of {SID(BA)}))",
            "unverified",
        ),
    ];
    for (name, extra, expected) in cases {
        let file = KeyFile::new(&format!("acl-{name}"), "udpst.keys", "1,s3cr3t\n");
        let path = Path::new(file.path());
        set_acl(path, extra);
        let result = assess(&open(path).unwrap());
        let tag = match result {
            Assessment::Meets => "meets",
            Assessment::Mismatch(_) => "mismatch",
            Assessment::Unverified(_) => "unverified",
        };
        assert_eq!(tag, expected, "{name}");
        set_acl(path, "");
    }
}

// ADR 0015: refusal before a Test starts, both roles and both formats.
#[cfg(windows)]
#[test]
fn both_roles_refuse_mismatch_and_unverified_without_key_material() {
    for (name, contents) in [
        ("udpst.keys", "1,s3cr3t\n"),
        (
            "keys.toml",
            "[[key]]\nadmin_key_name='fixture'\nkey_id=1\nkey_text='s3cr3t'\n",
        ),
    ] {
        let file = KeyFile::new(&format!("warning-{name}"), name, contents);
        for (extra, expected) in [
            ("", None),
            (
                "(A;;FR;;;BU)",
                Some("does not meet the supported Windows private-file policy"),
            ),
            (
                "(XA;;FR;;;BU;(Member_of {SID(BA)}))",
                Some("permissions could not be verified"),
            ),
        ] {
            set_acl(Path::new(file.path()), extra);
            let server = run(&["server", "--key-file", file.path(), "--print-config"]);
            assert_eq!(
                server.status.code(),
                Some(if expected.is_some() { 51 } else { 0 })
            );
            assert_permissions(&server, file.path(), expected);
            let client = run(&[
                "client",
                "--key-file",
                file.path(),
                "--time-limit",
                "1",
                "--format",
                "json",
                "127.0.0.1:9",
            ]);
            assert_eq!(
                client.status.code(),
                Some(if expected.is_some() { 51 } else { 200 })
            );
            assert_permissions(&client, file.path(), expected);
            if expected.is_some() {
                assert!(
                    client.stdout.is_empty(),
                    "startup refusal is not a completed Test"
                );
                assert!(server.stdout.is_empty());
            } else {
                let json: serde_json::Value = serde_json::from_slice(&client.stdout).unwrap();
                assert_eq!(json["ErrorStatus"], 200);
            }
            let preview = run(&[
                "server",
                "--key-file",
                file.path(),
                "--print-config",
                "--settings-only",
            ]);
            assert!(preview.status.success());
            assert_permissions(&preview, file.path(), None);
        }
        set_acl(Path::new(file.path()), "");
    }
}

// ADR 0015: inspection failure is unverified, and sharing protects content
// without claiming that it freezes the ACL.
#[cfg(windows)]
#[test]
fn native_inspection_failure_and_read_sharing() {
    let file = KeyFile::new("sharing-policy", "udpst.keys", "1,s3cr3t\n");
    let path = Path::new(file.path());
    set_acl(path, "");
    let mut reader = open(path).unwrap();
    assert_eq!(assess(&reader), Assessment::Meets);
    assert!(open(path).is_ok());
    for mode in [0x4000_0000, 0x0001_0000] {
        assert_eq!(
            OpenOptions::new()
                .access_mode(mode)
                .open(path)
                .unwrap_err()
                .raw_os_error(),
            Some(32)
        );
    }
    assert_eq!(
        std::fs::rename(path, path.with_extension("moved"))
            .unwrap_err()
            .raw_os_error(),
        Some(32)
    );
    set_acl(path, "(A;;FR;;;BU)");
    assert!(matches!(assess(&reader), Assessment::Mismatch(_)));
    let mut bytes = String::new();
    reader.read_to_string(&mut bytes).unwrap();
    assert_eq!(bytes, "1,s3cr3t\n");
    drop(reader);
    set_acl(path, "");
    let writer = OpenOptions::new().write(true).open(path).unwrap();
    assert_eq!(open(path).unwrap_err().raw_os_error(), Some(32));
    drop(writer);
    let content_only = OpenOptions::new().access_mode(1).open(path).unwrap();
    assert!(matches!(assess(&content_only), Assessment::Unverified(_)));
}

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

// ADR 0015: private files support a real successful authenticated Test.
#[cfg(windows)]
#[test]
fn a_private_file_supports_a_successful_authenticated_test() {
    let file = KeyFile::new("private-success", "udpst.keys", "1,s3cr3t\n");
    set_acl(Path::new(file.path()), "");
    let port = UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let log = Path::new(file.path()).parent().unwrap().join("server.log");
    let child = Command::new(env!("CARGO_BIN_EXE_udpstp"))
        .args([
            "server",
            "--key-file",
            file.path(),
            "--max-hops",
            "8",
            "--listen",
            "127.0.0.1",
            "--port",
            &port.to_string(),
            "--log-file",
            log.to_str().unwrap(),
            "--log-format",
            "json",
        ])
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let mut server = Server(child);
    let (send, recv) = mpsc::channel();
    let stderr = server.0.stderr.take().unwrap();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = send.send(line);
        }
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let read_log = || -> String {
        std::fs::read_dir(log.parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry.file_name().to_string_lossy().starts_with("server.")
                    && entry.path().extension().is_some_and(|e| e == "log")
            })
            .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
            .collect()
    };
    while !read_log().contains("Listening") {
        assert!(
            std::time::Instant::now() < deadline,
            "Server did not listen: {}",
            read_log()
        );
        assert!(
            server.0.try_wait().unwrap().is_none(),
            "Server exited: {}",
            read_log()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let client = run(&[
        "client",
        "--key-file",
        file.path(),
        "--format",
        "json",
        "--test-interval",
        "5",
        "--time-limit",
        "3",
        "--start-row",
        "1",
        &format!("127.0.0.1:{port}"),
    ]);
    assert!(
        client.status.success(),
        "{}",
        String::from_utf8_lossy(&client.stderr)
    );
    let _: serde_json::Value = serde_json::from_slice(&client.stdout).unwrap();
    let logged = read_log();
    assert!(!logged.contains("Shared Key file"));
    assert!(!logged.contains(SECRET));
    let mut diagnostic = String::new();
    while let Ok(line) = recv.recv_timeout(Duration::from_millis(100)) {
        diagnostic.push_str(&line);
    }
    assert!(!human_output::prose(&diagnostic).contains("Shared Key file"));
    assert_permissions(&client, file.path(), None);
    set_acl(Path::new(file.path()), "");
}

// ADR 0015 and ADR 0014: refusal reaches the configured startup log and stderr.
#[cfg(windows)]
#[test]
fn refused_server_keys_reach_stderr_and_the_daily_log() {
    for (name, contents) in [
        ("udpst.keys", "1,s3cr3t\n"),
        (
            "keys.toml",
            "[[key]]\nadmin_key_name='fixture'\nkey_id=1\nkey_text='s3cr3t'\n",
        ),
    ] {
        for (tag, extra, reason) in [
            (
                "mismatch",
                "(A;;FR;;;BU)",
                "does not meet the supported Windows private-file policy",
            ),
            (
                "unverified",
                "(XA;;FR;;;BU;(Member_of {SID(BA)}))",
                "permissions could not be verified",
            ),
        ] {
            let file = KeyFile::new(&format!("refused-log-{tag}-{name}"), name, contents);
            set_acl(Path::new(file.path()), extra);
            let dir = Path::new(file.path()).parent().unwrap();
            let log = dir.join("server.log");
            let output = run(&[
                "server",
                "--key-file",
                file.path(),
                "--log-file",
                log.to_str().unwrap(),
                "--log-format",
                "json",
            ]);
            assert_eq!(output.status.code(), Some(51));
            assert_permissions(&output, file.path(), Some(reason));
            let logged: String = std::fs::read_dir(dir)
                .unwrap()
                .flatten()
                .filter(|e| {
                    e.file_name().to_string_lossy().starts_with("server.")
                        && e.path().extension().is_some_and(|x| x == "log")
                })
                .map(|e| std::fs::read_to_string(e.path()).unwrap())
                .collect();
            let error = logged
                .lines()
                .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                .find(|e| e["event"] == "StartupError")
                .expect("startup refusal logged");
            assert_eq!(error["exit_status"], 51);
            assert!(error["message"].as_str().unwrap().contains(file.path()));
            assert!(error["message"].as_str().unwrap().contains(reason));
            assert!(!logged.contains(SECRET));
            assert!(!logged.contains("Listening"));
            set_acl(Path::new(file.path()), "");
        }
    }
}

// ADR 0015: replay the documented reconstruction against an existing
// explicit outsider grant, then inspect the actual file and production load.
#[cfg(windows)]
#[test]
fn manual_setup_removes_pre_existing_outsider_grants() {
    let file = KeyFile::new(
        "manual-reconstruction",
        "keys.toml",
        "[[key]]\nadmin_key_name='fixture'\nkey_id=1\nkey_text='s3cr3t'\n",
    );
    let path = Path::new(file.path());
    set_acl(path, "(A;;FA;;;BU)");
    assert!(matches!(
        assess(&open(path).unwrap()),
        Assessment::Mismatch(_)
    ));
    let guide = include_str!("../docs/guides/windows.md");
    let section = guide
        .split("<!-- windows-private-file-setup:start -->")
        .nth(1)
        .unwrap();
    let setup = section
        .split("```powershell\n")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    let script = format!(
        "Set-Location -LiteralPath (Split-Path -Path $env:UDPSTP_FIXTURE_FILE -Parent);\n{setup}\n\
        $actual=Get-Acl -LiteralPath $keys;\n\
        if (-not $actual.AreAccessRulesProtected) {{throw 'File inheritance not protected'}};\n\
        $owner=$actual.GetOwner([Security.Principal.SecurityIdentifier]).Value;\n\
        if ($owner -ne $me) {{throw 'Actual owner differs'}};\n\
        $rules=$actual.GetAccessRules($true,$true,[Security.Principal.SecurityIdentifier]);\n\
        if ($rules.Count -ne 3) {{throw 'Unexpected actual grant count'}};\n\
        foreach ($rule in $rules) {{\n\
            if ($rule.IdentityReference.Value -notin @($me,'S-1-5-18','S-1-5-32-544') -or\n\
                $rule.AccessControlType -ne 'Allow' -or $rule.FileSystemRights -ne 'FullControl') {{throw 'Unexpected actual grant'}}\n\
        }}"
    );
    powershell(&script, path, "");
    assert_eq!(assess(&open(path).unwrap()), Assessment::Meets);
    let output = run(&["server", "--key-file", file.path(), "--print-config"]);
    assert!(output.status.success());
    assert_permissions(&output, file.path(), None);
}
