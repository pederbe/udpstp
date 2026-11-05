<#
.SYNOPSIS
Checks the udpstp Server as a Windows service, end to end.

.DESCRIPTION
Installs the service with contrib/windows/udpstp-server.ps1 and checks:
its configuration (virtual account, privileges, SID type, failure
actions, start type), the data folder's owner and ACL and the firewall
rule; a Test served by it; its stop during a Test, with its Tests' STOP
and the log's summaries; a start that fails on a broken configuration,
with its exit status and the error in its log; a second -Install; and
-Uninstall, with and without -RemoveData.

It changes the machine: it installs a service, a firewall rule and a
program under Program Files, so it runs only where nothing is lost: in
Windows Sandbox (scripts/windows-service-sandbox.ps1) or on a disposable
CI runner, never from the pre-push hook or on a machine in use. It needs
an elevated PowerShell and -AllowMachineChanges. It refuses an existing
installation or data folder. -Udpstp is the udpstp.exe to install; -Evidence, a
folder the service's logs and its sc.exe configuration are copied to
before the data folder is removed, and on a failure. Client output is
saved too. Automatic start configuration is checked; an actual reboot
and shutdown remain separate checks.
#>
param(
    [Parameter(Mandatory = $true)]
    [string]$Udpstp,
    [string]$Evidence,
    [switch]$AllowMachineChanges
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 3

$Name = 'udpstp-server'
$Data = Join-Path $env:ProgramData 'udpstp'
$Logs = Join-Path $Data 'logs'
$Config = Join-Path $Data 'server.toml'
$Keys = Join-Path $Data 'keys.toml'
$Program = Join-Path $env:ProgramW6432 'udpstp\udpstp.exe'
$RuleName = $Name
$Installer = Join-Path $PSScriptRoot '..\contrib\windows\udpstp-server.ps1'
$Work = Join-Path ([IO.Path]::GetTempPath()) ('udpstp-service-check-' + [guid]::NewGuid().ToString('N'))
$ClientKeys = Join-Path $Work 'keys.toml'
$ClientOutput = Join-Path $Work 'client.json'
$ClientError = Join-Path $Work 'client-stderr.txt'
$client = $null
$started = $false
$failures = 0

# Refuse before copying files, changing permissions or saving evidence from
# an existing installation. This check belongs only on a disposable machine.
if (-not $AllowMachineChanges) {
    throw 'Use -AllowMachineChanges only on a disposable Windows machine.'
}
$principal = New-Object Security.Principal.WindowsPrincipal(
    [Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run the check from an elevated PowerShell on a disposable machine.'
}
foreach ($path in @($Data, (Split-Path $Program))) {
    if (Test-Path -LiteralPath $path) { throw "Refusing existing path: $path" }
}
if (Get-Service -Name $Name -ErrorAction SilentlyContinue) {
    throw "Refusing existing service: $Name"
}
if (Get-NetFirewallRule -Name $RuleName -ErrorAction SilentlyContinue) {
    throw "Refusing existing firewall rule: $RuleName"
}
$Udpstp = (Resolve-Path -LiteralPath $Udpstp -ErrorAction Stop).Path

# The service's logs and configuration, kept for a run on a throwaway
# machine, where they'd be gone with it.
function SaveEvidence {
    if (-not $Evidence) { return }
    New-Item -ItemType Directory -Force -Path $Evidence | Out-Null
    if (Test-Path -LiteralPath $Logs) {
        Copy-Item -LiteralPath $Logs -Destination (Join-Path $Evidence 'logs') -Recurse -Force
    }
    if (Test-Path -LiteralPath $Config) {
        Copy-Item -LiteralPath $Config -Destination $Evidence -Force
    }
    foreach ($path in @($ClientOutput, $ClientError)) {
        if (Test-Path -LiteralPath $path) {
            Copy-Item -LiteralPath $path -Destination $Evidence -Force
        }
    }
    $ErrorActionPreference = 'Continue'
    foreach ($query in @('qc', 'query', 'qprivs', 'qsidtype', 'qfailure', 'qfailureflag')) {
        & sc.exe $query $Name 2>&1 | Out-File -FilePath (Join-Path $Evidence "sc-$query.txt")
    }
}
trap {
    if ($started) {
        if ($null -ne $client -and -not $client.HasExited) {
            $client.Kill()
            $client.WaitForExit(5000) | Out-Null
        }
        SaveEvidence
    }
    Write-Host "windows-service-check: stopped: $_"
    exit 1
}

function Check([bool]$Condition, [string]$What) {
    if ($Condition) {
        Write-Host "ok: $What"
    } else {
        Write-Host "FAILED: $What"
        $script:failures++
    }
}

function Install {
    & powershell -NoProfile -ExecutionPolicy Bypass -File $Installer -Install
    if ($LASTEXITCODE -ne 0) { throw "-Install failed ($LASTEXITCODE)" }
}

function WaitUntil([scriptblock]$Condition, [int]$Seconds, [string]$What) {
    $elapsed = [Diagnostics.Stopwatch]::StartNew()
    while (-not (& $Condition)) {
        if ($elapsed.Elapsed.TotalSeconds -ge $Seconds) { throw "Timed out: $What" }
        Start-Sleep -Milliseconds 100
    }
}

# A broken configuration cannot select log_format, so its StartupError uses
# the default human format. Keep the raw lines as well as parsed JSON events.
function RawLogLines {
    Get-ChildItem -LiteralPath $Logs -Filter 'server.*.log' |
        ForEach-Object { Get-Content -LiteralPath $_.FullName }
}

# The log's JSON lines, from every daily file.
function LogLines {
    RawLogLines |
        Where-Object { $_ -like '{*' } |
        ForEach-Object { $_ | ConvertFrom-Json }
}

# The service's last exit code, as sc.exe query gives it.
function ExitCode {
    $query = (& sc.exe query $Name) -join "`n"
    $specific = [regex]::Match($query, 'SERVICE_EXIT_CODE\s*:\s*(\d+)').Groups[1].Value
    $win32 = [regex]::Match($query, 'WIN32_EXIT_CODE\s*:\s*(\d+)').Groups[1].Value
    if ($win32 -eq '1066') { [int]$specific } else { [int]$win32 }
}

# The program under Program Files, the Key Table and a configuration.
$started = $true
New-Item -ItemType Directory -Path $Work | Out-Null
New-Item -ItemType Directory -Force -Path (Split-Path $Program) | Out-Null
Copy-Item -LiteralPath $Udpstp -Destination $Program -Force
New-Item -ItemType Directory -Force -Path $Data | Out-Null
$keyTable = "[[key]]`nadmin_key_name = `"ci`"`nkey_id = 1`nkey_text = `"ci-service-key`"`n"
Set-Content -LiteralPath $Keys -Value $keyTable -Encoding ASCII -NoNewline
Set-Content -LiteralPath $ClientKeys -Value $keyTable -Encoding ASCII -NoNewline
$goodConfig = @(
    "key_file = '$Keys'",
    'log_format = "json"',
    'max_hops = 8'
)
Set-Content -LiteralPath $Config -Value $goodConfig -Encoding ASCII

Install

# Its configuration.
$service = Get-Service -Name $Name
Check ($service.Status -eq 'Running') 'the service runs'
Check ($service.StartType -eq 'Automatic') 'it is configured for automatic start (a reboot is checked separately)'
$qc = (& sc.exe qc $Name) -join "`n"
Check ($qc -match 'SERVICE_START_NAME\s*:\s*NT SERVICE\\udpstp-server') 'it runs as NT SERVICE\udpstp-server'
Check ($qc -match [regex]::Escape("`"$Program`" server --windows-service")) 'its command line quotes the program'
$privs = (& sc.exe qprivs $Name) -join "`n"
Check (($privs -match 'SeChangeNotifyPrivilege') -and ($privs -notmatch 'SeImpersonatePrivilege')) 'it keeps only SeChangeNotifyPrivilege'
Check ((& sc.exe qsidtype $Name) -join "`n" -match 'UNRESTRICTED') 'its SID type is unrestricted'
Check ((& sc.exe qfailureflag $Name) -join "`n" -match 'TRUE') 'it restarts after a non-zero exit status'
Check ((& sc.exe qfailure $Name) -join "`n" -match 'RESTART') 'its failure actions restart it'

# The data folder's ACL: its own, owned by Administrators.
$acl = Get-Acl -LiteralPath $Data
Check ($acl.AreAccessRulesProtected) 'the data folder inherits nothing from ProgramData'
Check ($acl.Owner -eq 'BUILTIN\Administrators') 'the data folder is owned by Administrators'
$readers = $acl.Access | ForEach-Object { $_.IdentityReference.Value } | Sort-Object -Unique
Check (@($readers | Where-Object { $_ -notin @('BUILTIN\Administrators', 'NT AUTHORITY\SYSTEM', 'NT SERVICE\udpstp-server') }).Count -eq 0) "only Administrators, SYSTEM and the service have access ($($readers -join ', '))"

# The firewall rule.
$rule = Get-NetFirewallRule -Name $RuleName -ErrorAction SilentlyContinue
Check ($null -ne $rule) 'the firewall rule exists'
if ($rule) {
    $ports = $rule | Get-NetFirewallPortFilter
    Check (($ports.Protocol -eq 'UDP') -and ($ports.LocalPort -contains '24601') -and ($ports.LocalPort -contains '49152-65535')) 'the rule allows the Standard Port and the Ephemeral Ports over UDP'
    $application = ($rule | Get-NetFirewallApplicationFilter).Program
    Check ($application -eq $Program) 'the rule names the program'
}

# A Test it serves (5 s is the CLI minimum); then one it stops with STOP.
& $Program client --key-file $ClientKeys --test-interval 5 --max-hops 8 127.0.0.1 | Out-Null
Check ($LASTEXITCODE -eq 0) "a Test against the service completed ($LASTEXITCODE)"
$activatedBefore = @(LogLines | Where-Object { $_.event -eq 'TestConnectionActivated' }).Count
$client = Start-Process -FilePath $Program -PassThru -WindowStyle Hidden `
    -RedirectStandardOutput $ClientOutput -RedirectStandardError $ClientError `
    -ArgumentList @('client', '--key-file', "`"$ClientKeys`"", '--format', 'json', '--test-interval', '60', '--max-hops', '8', '127.0.0.1')
# Retain the handle so Windows PowerShell 5.1 can read ExitCode after exit.
$null = $client.Handle
WaitUntil {
    if ($client.HasExited) { throw "The Client exited before the service stop: $(Get-Content -LiteralPath $ClientError -Raw)" }
    @(LogLines | Where-Object { $_.event -eq 'TestConnectionActivated' }).Count -gt $activatedBefore
} 20 'the Client activates its Test Connection'
$service.Stop()
$service.WaitForStatus('Stopped', [TimeSpan]::FromSeconds(20))
if (-not $client.WaitForExit(10000)) { throw 'The Client did not exit after the service stopped.' }
# Complete the redirected output before reading it (Process.WaitForExit).
$client.WaitForExit()
Check ($client.ExitCode -eq 0) "the stopped Test's Client exits successfully ($($client.ExitCode))"
$result = Get-Content -LiteralPath $ClientOutput -Raw | ConvertFrom-Json
Check ($result.udpstp.EndReason -eq 'Completed') "the Client received STOP, rather than ending by its timer or Watchdog ($($result.udpstp.EndReason))"
Check ($result.ErrorStatus -ge 0 -and $result.ErrorStatus -lt 50) "the Client reports a completed Test, with at most a warning ($($result.ErrorStatus))"
$log = @(LogLines)
Check (@($log | Where-Object { $_.event -eq 'Stopping' -and $_.reason -eq 'ServiceStop' }).Count -eq 1) 'the log says it stopped on the service''s stop'
Check (@($log | Where-Object { $_.event -eq 'TestSummary' -and $_.result -eq 'ServerStopping' }).Count -ge 1) 'the Test it stopped is summarized as ServerStopping'
Check (@($log | Where-Object { $_.event -eq 'Statistics' }).Count -ge 1) 'the log reports its counters'
Check (@($log | Where-Object { $_.event -eq 'StartupWarning' }).Count -ge 1) 'its startup warnings are in the log'
Check ((ExitCode) -eq 0) 'it stopped with exit code 0'

# A start that fails: the error in its log, the exit status as its code.
Set-Content -LiteralPath $Config -Value ($goodConfig + 'no_such_setting = 1') -Encoding ASCII
try { Start-Service -Name $Name } catch { }
(Get-Service -Name $Name).WaitForStatus('Stopped', [TimeSpan]::FromSeconds(10))
Check ((Get-Service -Name $Name).Status -eq 'Stopped') 'a broken configuration stops it'
Check ((ExitCode) -eq 50) "its exit code is the configuration error's, 50 ($(ExitCode))"
WaitUntil {
    # Human log continuations may put the error and event fields on separate lines.
    $text = (RawLogLines) -join "`n"
    $text -match 'event="StartupError"' -and $text -match '\bexit_status=50\b' -and $text -match 'no_such_setting'
} 10 'the log records StartupError with configuration exit status 50'
Check $true 'its log has the configuration error, in the default human format'
Set-Content -LiteralPath $Config -Value $goodConfig -Encoding ASCII

# A second -Install changes nothing but starts it again.
Install
Check ((Get-Service -Name $Name).Status -eq 'Running') 'a second -Install runs it again'

SaveEvidence

# -Uninstall keeps the data, unless -RemoveData.
& powershell -NoProfile -ExecutionPolicy Bypass -File $Installer -Uninstall
Check ($LASTEXITCODE -eq 0) '-Uninstall exits successfully'
Check ($null -eq (Get-Service -Name $Name -ErrorAction SilentlyContinue)) '-Uninstall removes the service'
Check ($null -eq (Get-NetFirewallRule -Name $RuleName -ErrorAction SilentlyContinue)) '-Uninstall removes the firewall rule'
Check (Test-Path -LiteralPath $Keys) '-Uninstall keeps the Key Table'
& powershell -NoProfile -ExecutionPolicy Bypass -File $Installer -Uninstall -RemoveData
Check ($LASTEXITCODE -eq 0) '-Uninstall -RemoveData exits successfully'
Check (-not (Test-Path -LiteralPath $Data)) '-Uninstall -RemoveData removes the data folder'

if ($failures -gt 0) {
    Write-Host "windows-service-check: $failures failed"
    exit 1
}
Write-Host 'windows-service-check: all passed'
