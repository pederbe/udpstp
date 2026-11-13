<#
.SYNOPSIS
Checks Shared Key refusal and recovery containment in the actual Windows service.

Proves ADR 0014, ADR 0015 and specification story 39 (conformance: key-file-private).

.DESCRIPTION
Requires an elevated PowerShell on a disposable machine and -AllowMachineChanges.
Refuses existing installation paths, service and firewall rule before mutation.
Uses external local-NTFS Key Tables so installer ACL repair cannot hide refusal.
Saves raw installer output, service state and startup logs in -Evidence.
#>
param([Parameter(Mandatory = $true)][string]$Udpstp,
    [Parameter(Mandatory = $true)][string]$Evidence,
    [switch]$AllowMachineChanges)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 3
$Name = 'udpstp-server'
$Data = Join-Path $env:ProgramData 'udpstp'
$ProgramDir = Join-Path $env:ProgramW6432 'udpstp'
$Program = Join-Path $ProgramDir 'udpstp.exe'
$Installer = Join-Path $PSScriptRoot '..\contrib\windows\udpstp-server.ps1'
$External = Join-Path $env:ProgramData ('udpstp-service-keys-' + [guid]::NewGuid().ToString('N'))
$Config = Join-Path $Data 'server.toml'
$failures = 0
if (-not $AllowMachineChanges) { throw 'Requires -AllowMachineChanges on a disposable machine.' }
$principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'Requires an elevated PowerShell.' }
foreach ($path in @($Data, $ProgramDir, $External, $Evidence)) {
    if (Test-Path $path) { throw "Refusing existing path: $path" }
}
if ((Get-Service $Name -ErrorAction SilentlyContinue) -or (Get-NetFirewallRule -Name $Name -ErrorAction SilentlyContinue)) { throw 'Refusing existing service or firewall rule.' }
$Udpstp = (Resolve-Path $Udpstp).Path
function Check([bool]$Condition, [string]$What) {
    if ($Condition) { Write-Host "ok: $What" } else { Write-Host "FAILED: $What"; $script:failures++ }
}
function WaitUntil([scriptblock]$Condition, [int]$Seconds) {
    $timer = [Diagnostics.Stopwatch]::StartNew()
    while (-not (& $Condition)) {
        if ($timer.Elapsed.TotalSeconds -ge $Seconds) { throw 'Service fixture timed out.' }
        Start-Sleep -Milliseconds 100
    }
}
function Events {
    Get-ChildItem "$Data\logs" -Filter 'server.*.log' -ErrorAction SilentlyContinue |
        ForEach-Object { Get-Content $_.FullName } | Where-Object { $_ -like '{*' } | ForEach-Object { $_ | ConvertFrom-Json }
}
function Key([string]$Format, [string]$Policy) {
    $sid = ((& sc.exe showsid $Name | Select-String 'S-1-5-80-[0-9-]+').Matches[0].Value)
    $sections = [Security.AccessControl.AccessControlSections]::Owner -bor [Security.AccessControl.AccessControlSections]::Access
    $directory = New-Object Security.AccessControl.DirectorySecurity
    $directory.SetSecurityDescriptorSddlForm("O:BAD:P(A;OICI;FA;;;BA)(A;OICI;FA;;;SY)(A;OICI;FRFX;;;$sid)", $sections)
    [IO.Directory]::SetAccessControl($External, $directory)
    $path = Join-Path $External $Format
    $content = if ($Format -eq 'keys.toml') { '[[key]]', 'admin_key_name = "fixture"', 'key_id = 1', 'key_text = "fixture-shared-key"' } else { '1,fixture-shared-key' }
    Set-Content $path $content -Encoding ASCII
    $extra = switch ($Policy) { 'mismatch' { '(A;;FR;;;BU)' } 'unverified' { '(XA;;FR;;;BU;(Member_of {SID(BA)}))' } 'private' { '' } }
    $file = New-Object Security.AccessControl.FileSecurity
    $file.SetSecurityDescriptorSddlForm("O:BAD:P(A;;FA;;;BA)(A;;FA;;;SY)(A;;FR;;;$sid)$extra", $sections)
    [IO.File]::SetAccessControl($path, $file)
    Set-Content $Config @("key_file = '$path'", 'listen = ["127.0.0.1"]', 'log_format = "json"', 'max_hops = 8') -Encoding ASCII
    return $path
}
function Install([string]$Label, [bool]$ExpectedSuccess) {
    $out = Join-Path $Evidence "$Label-out.txt"
    $err = Join-Path $Evidence "$Label-err.txt"
    $attempt = Start-Process powershell -PassThru -WindowStyle Hidden -RedirectStandardOutput $out -RedirectStandardError $err -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$Installer`"", '-Install')
    $null = $attempt.Handle
    if (-not $attempt.WaitForExit(45000)) { $attempt.Kill(); throw 'Installer timed out.' }
    $attempt.WaitForExit()
    Check (($attempt.ExitCode -eq 0) -eq $ExpectedSuccess) "$Label installer exit status"
    return ((Get-Content $out -Raw) + (Get-Content $err -Raw))
}
function Refuse([string]$Format, [string]$Policy, [string]$Phase, [int]$Observe = 3) {
    $path = Key $Format $Policy
    $before = @(Events | Where-Object event -eq 'Listening').Count
    $errorsBefore = @(Events | Where-Object event -eq 'StartupError').Count
    $label = "$Phase-$Format-$Policy"
    $text = Install $label $false
    Check ($text -match 'disabled and stopped, with recovery cleared') "$label containment reported"
    Check ($text -notmatch 'starts at boot|fixture-shared-key') "$label no success or key output"
    $query = (& sc.exe query $Name) -join "`n"
    Check ($query -match 'SERVICE_EXIT_CODE\s*:\s*51') "$label service-specific status 51"
    $query | Set-Content (Join-Path $Evidence "$label-service.txt")
    $recovery = (& sc.exe qfailure $Name) -join "`n"
    Check ($recovery -notmatch 'RESTART') "$label recovery cleared"
    $recovery | Set-Content (Join-Path $Evidence "$label-recovery.txt")
    $newErrors = @(Events | Where-Object event -eq 'StartupError' | Select-Object -Skip $errorsBefore)
    $diagnosis = if ($Policy -eq 'mismatch') { 'does not meet the supported Windows private-file policy' } else { 'permissions could not be verified' }
    Check (@($newErrors | Where-Object { $_.exit_status -eq 51 -and $_.error -match [regex]::Escape($path) -and $_.error -match $diagnosis -and $_.error -match 'Key file permissions' }).Count -ge 1) "$label distinct actionable StartupError"
    Check ([string]($newErrors | ConvertTo-Json -Depth 8) -notmatch 'fixture-shared-key') "$label no logged key"
    $timer = [Diagnostics.Stopwatch]::StartNew()
    $contained = $true
    while ($timer.Elapsed.TotalSeconds -lt $Observe) {
        $state = Get-Service $Name
        if ($state.Status -ne 'Stopped' -or $state.StartType -ne 'Disabled' -or (Get-NetUDPEndpoint -LocalPort 24601 -ErrorAction SilentlyContinue)) { $contained = $false }
        Start-Sleep -Milliseconds 100
    }
    Check $contained "$label remains disabled/stopped with no listener"
    Check (@(Events | Where-Object event -eq 'Listening').Count -eq $before) "$label no Listening event"
}
New-Item -ItemType Directory $Evidence,$External,$Data,$ProgramDir | Out-Null
Copy-Item $Udpstp $Program
try {
    foreach ($format in @('keys.toml', 'udpst.keys')) {
        foreach ($policy in @('mismatch', 'unverified')) {
            Refuse $format $policy 'first'
            & powershell -NoProfile -ExecutionPolicy Bypass -File $Installer -Uninstall | Out-Null
            if ($LASTEXITCODE) { throw 'Failed first-install fixture could not uninstall.' }
        }
    }
    [void](Key 'keys.toml' 'private')
    [void](Install 'before-recovery' $true)
    # Prove effective recovery without assuming a new failureflag works before reboot.
    & sc.exe failure $Name reset= 86400 actions= restart/2000 | Out-Null
    if ($LASTEXITCODE) { throw 'Could not shorten fixture recovery.' }
    $oldPid = (Get-CimInstance Win32_Service -Filter "Name='$Name'").ProcessId
    Stop-Process -Id $oldPid -Force
    WaitUntil { $s = Get-CimInstance Win32_Service -Filter "Name='$Name'"; $s.State -eq 'Running' -and $s.ProcessId -ne 0 -and $s.ProcessId -ne $oldPid } 15
    Check $true 'effective recovery restarted the killed service'
    foreach ($format in @('keys.toml', 'udpst.keys')) {
        foreach ($policy in @('mismatch', 'unverified')) {
            [void](Key $format 'private')
            [void](Install "before-$format-$policy" $true)
            Refuse $format $policy 'reinstall'
            [void](Key $format 'private')
            [void](Install "repair-$format-$policy" $true)
            Check ((Get-Service $Name).Status -eq 'Running' -and (Get-Service $Name).StartType -eq 'Automatic') "repair $format $policy running/automatic"
            Check (((& sc.exe qfailure $Name) -join "`n") -match 'RESTART') "repair $format $policy recovery restored"
        }
    }
    & sc.exe failure $Name reset= 86400 actions= restart/8000 | Out-Null
    if ($LASTEXITCODE) { throw 'Could not configure queued recovery.' }
    Stop-Process -Id (Get-CimInstance Win32_Service -Filter "Name='$Name'").ProcessId -Force
    WaitUntil { (Get-Service $Name).Status -eq 'Stopped' } 5
    Refuse 'keys.toml' 'mismatch' 'queued' 9
    [void](Key 'keys.toml' 'private')
    [void](Install 'repair-queued' $true)
    Check ((Get-Service $Name).Status -eq 'Running' -and (Get-Service $Name).StartType -eq 'Automatic') 'repair after queued recovery running/automatic'
} finally {
    if (Test-Path "$Data\logs") { Copy-Item "$Data\logs" $Evidence -Recurse -Force }
    & powershell -NoProfile -ExecutionPolicy Bypass -File $Installer -Uninstall -RemoveData | Out-Null
    if ($LASTEXITCODE) { throw 'Fixture uninstall failed; keep the reservation.' }
    Remove-Item $External,$ProgramDir -Recurse -Force
}
if ($failures) { throw "$failures service refusal checks failed." }
Write-Host 'windows-key-service-check: all passed'
