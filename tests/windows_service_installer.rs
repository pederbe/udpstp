//! The installer's read-only service SID lookup in Windows PowerShell 5.1.
//! This uses its actual helper definitions and lookup expression without
//! installing a service or changing files, permissions or firewall rules.

#![cfg(windows)]

use std::process::Command;

// ADR 0014: the installer supports Windows PowerShell as well as PowerShell 7.
// In Windows PowerShell, the built-in `sc` alias shadows a function named Sc.
#[test]
fn the_service_installer_resolves_its_sid_in_windows_powershell() {
    let output = Command::new("powershell.exe")
        .env_remove("PSModulePath")
        .args(["-NoProfile", "-NonInteractive", "-Command"])
        .arg(
            r#"
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 3
$tokens = $null
$parseErrors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile($env:UDPSTP_TEST_INSTALLER, [ref]$tokens, [ref]$parseErrors)
if ($parseErrors.Count) { throw 'The installer must parse' }
$ast.FindAll({ param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] }, $true) |
    ForEach-Object { Invoke-Expression $_.Extent.Text }
$lookup = $ast.Find({ param($node)
    $node -is [Management.Automation.Language.AssignmentStatementAst] -and $node.Left.Extent.Text -eq '$sid'
}, $true)
if ($null -eq $lookup) { throw 'The installer must resolve its service SID' }
$Name = 'udpstp-server'
Invoke-Expression $lookup.Extent.Text
if ($sid -notmatch '^S-1-5-80(-\d+){5}$') { throw "Not a service SID: $sid" }
Write-Output $sid
"#,
        )
        .env(
            "UDPSTP_TEST_INSTALLER",
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/contrib/windows/udpstp-server.ps1"
            ),
        )
        .output()
        .expect("Windows PowerShell starts");
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}
