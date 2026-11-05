<#
.SYNOPSIS
Inside Windows Sandbox: runs the Windows service's end-to-end check.
scripts/windows-service-sandbox.ps1 starts it as the Sandbox's
logon command; it isn't meant to run anywhere else.

.DESCRIPTION
First it turns off Smart App Control's evaluation mode, which the
Sandbox starts in and which can slow every program it starts by
minutes. Then it runs
windows-service-check.ps1 on the udpstp.exe beside it, writing its
output to check.txt and its exit status to exit-status.txt in the
result folder, the service's logs and configuration with them.

-RestartProbe instead records each logon in restart-probe.txt and
restarts the Sandbox after the first: two lines there show a Sandbox
runs its logon command again after a restart it starts.
#>
param(
    [switch]$RestartProbe
)

$ErrorActionPreference = 'Continue'
$check = 'C:\udpstp-check'
$result = 'C:\udpstp-result'

if ($RestartProbe) {
    $probe = Join-Path $result 'restart-probe.txt'
    $first = -not (Test-Path -LiteralPath $probe)
    Add-Content -LiteralPath $probe -Value "logon $(Get-Date -Format o)"
    if ($first) {
        shutdown.exe /r /t 5
    } else {
        Set-Content -LiteralPath (Join-Path $result 'exit-status.txt') -Value '0'
    }
    exit 0
}

& reg.exe add 'HKLM\SYSTEM\CurrentControlSet\Control\CI\Policy' /v VerifiedAndReputablePolicyState /t REG_DWORD /d 0 /f | Out-Null
& CiTool.exe --refresh --json | Out-Null

& powershell.exe -NoProfile -ExecutionPolicy Bypass -File (Join-Path $check 'scripts\windows-service-check.ps1') `
    -Udpstp (Join-Path $check 'udpstp.exe') -AllowMachineChanges -Evidence (Join-Path $result 'evidence') *>&1 |
    Out-File -FilePath (Join-Path $result 'check.txt') -Encoding UTF8
Set-Content -LiteralPath (Join-Path $result 'exit-status.txt') -Value "$LASTEXITCODE"
