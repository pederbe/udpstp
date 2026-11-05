<#
.SYNOPSIS
Runs the Windows service's end-to-end check in Windows Sandbox.

.DESCRIPTION
scripts/windows-service-check.ps1 installs a service, a firewall rule and
a program under Program Files, which this machine mustn't get. Windows
Sandbox is a throwaway Windows, an administrator inside, gone when it
closes, so the check runs there instead. Windows Sandbox is an optional
feature of Windows 10 and 11 Pro and Enterprise, turned on once by an
administrator ("Turn Windows features on or off", then a restart).

This copies the check, the installer and the udpstp.exe to check into a
snapshot folder, so edits made meanwhile don't change what runs, and
records the commit and the exe's SHA-256 beside the result. It starts a
Sandbox with no network, the snapshot mapped read-only and a result
folder mapped writable, waits for the check to finish, and prints its
result; the service's logs and configuration are in the result folder.
Close the Sandbox window afterwards.

-RestartProbe instead checks whether a Sandbox keeps running its logon
command across a restart it starts itself, which a check of the service
starting at boot would need.

.EXAMPLE
cargo build -p udpstp; .\scripts\windows-service-sandbox.ps1
#>
param(
    [string]$Udpstp = (Join-Path $PSScriptRoot '..\target\debug\udpstp.exe'),
    [switch]$RestartProbe,
    [int]$TimeoutMinutes = 20
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 3

$sandbox = Join-Path $env:SystemRoot 'System32\WindowsSandbox.exe'
if (-not (Test-Path -LiteralPath $sandbox)) {
    Write-Error ('Windows Sandbox isn''t turned on: as an administrator, turn on "Windows Sandbox" ' +
        'in "Turn Windows features on or off" and restart, then run this again.') -ErrorAction Continue
    exit 1
}
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$Udpstp = (Resolve-Path $Udpstp).Path

$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$root = Join-Path ([IO.Path]::GetTempPath()) "udpstp-sandbox-$stamp"
$snapshot = Join-Path $root 'check'
$result = Join-Path $root 'result'
New-Item -ItemType Directory -Force -Path (Join-Path $snapshot 'scripts'), (Join-Path $snapshot 'contrib\windows'), $result | Out-Null
Copy-Item -LiteralPath (Join-Path $repo 'scripts\windows-service-check.ps1') -Destination (Join-Path $snapshot 'scripts')
Copy-Item -LiteralPath (Join-Path $repo 'scripts\windows-service-sandbox-run.ps1') -Destination (Join-Path $snapshot 'scripts')
Copy-Item -LiteralPath (Join-Path $repo 'contrib\windows\udpstp-server.ps1') -Destination (Join-Path $snapshot 'contrib\windows')
Copy-Item -LiteralPath $Udpstp -Destination (Join-Path $snapshot 'udpstp.exe')

# What ran: the commit, whether the working tree differed from it, and the exe.
$commit = (& git -C $repo rev-parse HEAD).Trim()
$dirty = [bool](& git -C $repo status --porcelain -- scripts contrib crates Cargo.toml Cargo.lock .cargo)
$hash = (Get-FileHash -LiteralPath $Udpstp -Algorithm SHA256).Hash
@("commit $commit$(if ($dirty) { ' (working tree changed)' })", "udpstp.exe $Udpstp", "sha256 $hash") |
    Set-Content -LiteralPath (Join-Path $result 'what-ran.txt')

$mode = if ($RestartProbe) { '-RestartProbe' } else { '' }
$wsb = Join-Path $root 'check.wsb'
@"
<Configuration>
  <Networking>Disable</Networking>
  <MappedFolders>
    <MappedFolder>
      <HostFolder>$snapshot</HostFolder>
      <SandboxFolder>C:\udpstp-check</SandboxFolder>
      <ReadOnly>true</ReadOnly>
    </MappedFolder>
    <MappedFolder>
      <HostFolder>$result</HostFolder>
      <SandboxFolder>C:\udpstp-result</SandboxFolder>
      <ReadOnly>false</ReadOnly>
    </MappedFolder>
  </MappedFolders>
  <LogonCommand>
    <Command>powershell.exe -NoProfile -ExecutionPolicy Bypass -File C:\udpstp-check\scripts\windows-service-sandbox-run.ps1 $mode</Command>
  </LogonCommand>
</Configuration>
"@ | Set-Content -LiteralPath $wsb -Encoding UTF8

Write-Host "Starting Windows Sandbox; the result goes to $result"
Start-Process -FilePath $sandbox -ArgumentList "`"$wsb`""

$done = Join-Path $result 'exit-status.txt'
$deadline = (Get-Date).AddMinutes($TimeoutMinutes)
while (-not (Test-Path -LiteralPath $done)) {
    if ((Get-Date) -gt $deadline) {
        Write-Error "no result after $TimeoutMinutes minutes; see $result" -ErrorAction Continue
        exit 1
    }
    Start-Sleep -Seconds 5
}
Start-Sleep -Seconds 1
$transcript = Join-Path $result 'check.txt'
if (Test-Path -LiteralPath $transcript) { Get-Content -LiteralPath $transcript }
$status = [int](Get-Content -LiteralPath $done -Raw).Trim()
Write-Host "Exit status $status; the result, logs included, is in $result. Close the Sandbox window."
exit $status
