<#
.SYNOPSIS
Installs or removes the udpstp Server as a Windows service.

.DESCRIPTION
Run it from an elevated Windows PowerShell or PowerShell 7. -Install makes
the Windows service udpstp-server, which runs `udpstp server
--windows-service` as its own virtual account, NT SERVICE\udpstp-server,
with no privileges but SeChangeNotifyPrivilege; restarts it after a
failure; locks the data folder %ProgramData%\udpstp to Administrators,
SYSTEM and the service; adds a firewall rule for the Server's UDP ports;
checks that the Server starts; and only then starts it at boot. Running
-Install again updates the service in place. Before changing it, installation
disables startup, stops it and clears recovery. A failure leaves applied changes
in place, with the service disabled and stopped and recovery cleared when
containment succeeds; repair the problem and run -Install again. -Uninstall stops and removes
the service and its firewall rule, and keeps the data folder (the
configuration, the Key Table and the logs) unless -RemoveData is given.

docs/guides/windows.md, "Run the Server as a service", describes what it does and
why (ADR 0014).

.EXAMPLE
.\udpstp-server.ps1 -Install

.EXAMPLE
.\udpstp-server.ps1 -Uninstall -RemoveData
#>
[CmdletBinding(DefaultParameterSetName = 'Install')]
param(
    [Parameter(ParameterSetName = 'Install', Mandatory = $true)]
    [switch]$Install,
    [Parameter(ParameterSetName = 'Uninstall', Mandatory = $true)]
    [switch]$Uninstall,
    # The program, which must be under Program Files, where only an
    # administrator can replace it.
    [Parameter(ParameterSetName = 'Install')]
    [string]$Program = (Join-Path $env:ProgramW6432 'udpstp\udpstp.exe'),
    [Parameter(ParameterSetName = 'Uninstall')]
    [switch]$RemoveData
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 3

$Name = 'udpstp-server'
$Account = "NT SERVICE\$Name"
$Data = Join-Path $env:ProgramData 'udpstp'
$Logs = Join-Path $Data 'logs'
$Config = Join-Path $Data 'server.toml'
$LogFile = Join-Path $Logs 'server.log'
$Keys = Join-Path $Data 'keys.toml'
$RuleName = $Name

function Fail([string]$Message) {
    throw "udpstp-server.ps1: $Message"
}

# Runs a program, its stderr taken as output: under $ErrorActionPreference
# = 'Stop', Windows PowerShell 5.1 would throw on its first stderr line.
function Native([scriptblock]$Command) {
    $ErrorActionPreference = 'Continue'
    & $Command
}

# sc.exe, stopping on failure; its output on success. Avoid `Sc`: Windows
# PowerShell's built-in alias for Set-Content would shadow that function.
function Invoke-ServiceControl([string[]]$Arguments) {
    $output = Native { & sc.exe @Arguments 2>&1 }
    if ($LASTEXITCODE -ne 0) {
        Fail "sc.exe $($Arguments -join ' ') failed ($LASTEXITCODE): $output"
    }
    $output
}

# Clear actions, not merely failureflag: a flag change may need a reboot.
# ProcessStartInfo preserves sc.exe's empty actions argument on both PowerShells.
function Clear-Recovery {
    $start = New-Object Diagnostics.ProcessStartInfo
    $start.FileName = 'sc.exe'
    $start.Arguments = "failure $Name reset= 86400 actions= `"`""
    $start.UseShellExecute = $false
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $process = New-Object Diagnostics.Process
    $process.StartInfo = $start
    try {
        [void]$process.Start()
        $output = $process.StandardOutput.ReadToEndAsync()
        $errors = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit(30000)) {
            $process.Kill()
            Fail 'sc.exe did not clear recovery within 30 seconds.'
        }
        if ($process.ExitCode -ne 0) {
            Fail "could not clear recovery: $($output.Result) $($errors.Result)"
        }
    } finally {
        $process.Dispose()
    }
    $query = (Invoke-ServiceControl @('qfailure', $Name)) -join "`n"
    # sc.exe omits FAILURE_ACTIONS entirely when the action list is empty.
    if ($query -notmatch '(?m)^[ \t]*RESET_PERIOD\b' -or
        $query -match '(?m)^[ \t]*FAILURE_ACTIONS[ \t]*:[ \t]*\S') {
        Fail "could not verify empty recovery actions: $query"
    }
}

function Stop-AndVerify {
    $service = Get-Service -Name $Name
    if ($service.Status -ne 'Stopped') {
        if ($service.Status -ne 'StopPending') { $service.Stop() }
        $service.WaitForStatus('Stopped', [TimeSpan]::FromSeconds(30))
    }
    $service.Refresh()
    if ($service.Status -ne 'Stopped' -or $service.StartType -ne 'Disabled') {
        Fail "containment not established: $($service.Status), startup $($service.StartType)"
    }
}

if ($ExecutionContext.SessionState.LanguageMode -ne 'FullLanguage') {
    Fail ('PowerShell runs in ' + $ExecutionContext.SessionState.LanguageMode +
        ' mode here, which this script can''t run in. Follow the manual steps in docs/guides/windows.md, "Run the Server as a service".')
}
$principal = New-Object Security.Principal.WindowsPrincipal(
    [Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Fail 'run it from an elevated PowerShell (Run as administrator).'
}

if ($Uninstall) {
    if (Get-Service -Name $Name -ErrorAction SilentlyContinue) {
        Stop-Service -Name $Name -ErrorAction SilentlyContinue
        Invoke-ServiceControl @('delete', $Name) | Out-Null
        Write-Host "Removed the service $Name."
    }
    Remove-NetFirewallRule -Name $RuleName -ErrorAction SilentlyContinue
    if ($RemoveData) {
        Remove-Item -LiteralPath $Data -Recurse -Force -ErrorAction SilentlyContinue
        Write-Host "Removed $Data."
    } else {
        Write-Host "Kept $Data (the configuration, the Key Table and the logs); -RemoveData removes it."
    }
    exit 0
}

# The program: under Program Files, where only an administrator can
# replace it, so the service and its firewall rule can't be given to
# another program.
$Program = [IO.Path]::GetFullPath($Program)
$programFiles = [IO.Path]::GetFullPath($env:ProgramW6432).TrimEnd('\') + '\'
if (-not $Program.StartsWith($programFiles, [StringComparison]::OrdinalIgnoreCase)) {
    Fail "the program must be under $programFiles, where only an administrator can replace it: $Program"
}
if (-not (Test-Path -LiteralPath $Program -PathType Leaf)) {
    Fail "no program at $Program"
}

# The service's SID, which exists before the service does.
$sid = (Invoke-ServiceControl @('showsid', $Name) | Select-String 'S-1-5-80-[0-9-]+').Matches[0].Value
$serviceSid = New-Object Security.Principal.SecurityIdentifier($sid)
$admins = New-Object Security.Principal.SecurityIdentifier('S-1-5-32-544')
$system = New-Object Security.Principal.SecurityIdentifier('S-1-5-18')

# A folder's own ACL, set before anything is written in it: owner
# Administrators, nothing inherited from ProgramData, which on some
# machines lets every signed-in account change what's in it.
function Lock([string]$Path, [Security.AccessControl.FileSystemRights]$ServiceRights) {
    if (-not (Test-Path -LiteralPath $Path)) {
        New-Item -ItemType Directory -Path $Path | Out-Null
    }
    $inherit = [Security.AccessControl.InheritanceFlags]'ContainerInherit, ObjectInherit'
    $none = [Security.AccessControl.PropagationFlags]::None
    $allow = [Security.AccessControl.AccessControlType]::Allow
    $acl = New-Object Security.AccessControl.DirectorySecurity
    $acl.SetOwner($admins)
    $acl.SetAccessRuleProtection($true, $false)
    foreach ($rule in @(
            (New-Object Security.AccessControl.FileSystemAccessRule($admins, 'FullControl', $inherit, $none, $allow)),
            (New-Object Security.AccessControl.FileSystemAccessRule($system, 'FullControl', $inherit, $none, $allow)),
            (New-Object Security.AccessControl.FileSystemAccessRule($serviceSid, $ServiceRights, $inherit, $none, $allow)))) {
        $acl.AddAccessRule($rule)
    }
    Set-Acl -LiteralPath $Path -AclObject $acl
    # Files already there take the folder's rules.
    Get-ChildItem -LiteralPath $Path -Recurse -Force | ForEach-Object {
        $item = Get-Acl -LiteralPath $_.FullName
        $item.SetOwner($admins)
        $item.SetAccessRuleProtection($false, $false)
        foreach ($rule in @($item.Access | Where-Object { -not $_.IsInherited })) {
            [void]$item.RemoveAccessRule($rule)
        }
        Set-Acl -LiteralPath $_.FullName -AclObject $item
    }
}
$deploymentStarted = $false
try {
    # Disabling prevents queued recovery; changing actions cannot cancel its queue.
    $deploymentStarted = $true
    if (Get-Service -Name $Name -ErrorAction SilentlyContinue) {
        Invoke-ServiceControl @('config', $Name, 'start=', 'disabled') | Out-Null
        Stop-AndVerify
    } else {
        Invoke-ServiceControl @('create', $Name, 'binPath=', $Program, 'obj=', $Account,
            'start=', 'disabled', 'DisplayName=', 'udpstp Server') | Out-Null
    }
    Clear-Recovery
    Lock $Data 'ReadAndExecute'
    Lock $Logs 'Modify'

    # A configuration to start from, if there is none: a TOML literal string, so
    # the path's backslashes are kept. The log file isn't in it: the service's
    # command line names it, so even an error in this file reaches the log.
    if (-not (Test-Path -LiteralPath $Config)) {
        $lines = @()
        if (Test-Path -LiteralPath $Keys) {
            $lines += "key_file = '$Keys'"
        }
        Set-Content -LiteralPath $Config -Value $lines -Encoding ASCII
        Write-Host "Wrote $Config; edit it and run -Install again to apply it."
    }

    # Preview settings and firewall ports without loading keys as the administrator.
    # Only the trial start checks the actual Key Table under the virtual account.
    $printed = Native { & $Program server --config $Config --log-file $LogFile --print-config --settings-only 2>&1 }
    if ($LASTEXITCODE -ne 0) {
        Fail "invalid settings in $Config (exit status $LASTEXITCODE): $($printed -join "`n")"
    }
    $printed = $printed -join "`n"
    $port = [regex]::Match($printed, '(?m)^port = (\d+)').Groups[1].Value
    $ephemeral = [regex]::Match($printed, '(?m)^ephemeral_ports = "(\d+-\d+)"').Groups[1].Value
    if (-not $port -or -not $ephemeral) {
        Fail "--print-config gave no port or ephemeral_ports: $printed"
    }

    # Reconfigure while disabled. sc.exe
    # gets the bare program path, and its command line is then written to the
    # registry as it is: Windows PowerShell 5.1 would split an argument with
    # quotes in it on its way to sc.exe.
    Invoke-ServiceControl @('config', $Name, 'obj=', $Account, 'start=', 'disabled') | Out-Null
    $commandLine = "`"$Program`" server --windows-service --config `"$Config`" --log-file `"$LogFile`""
    Set-ItemProperty -LiteralPath "HKLM:\SYSTEM\CurrentControlSet\Services\$Name" `
        -Name ImagePath -Value $commandLine
    Invoke-ServiceControl @('description', $Name, 'Serves UDPSTP Tests (RFC 9946): IP-layer capacity measurements.') | Out-Null
    # No privileges but the one every process keeps.
    Invoke-ServiceControl @('privs', $Name, 'SeChangeNotifyPrivilege') | Out-Null
    # A SID of its own in its token, which a firewall rule for the service needs.
    Invoke-ServiceControl @('sidtype', $Name, 'unrestricted') | Out-Null

    # The firewall rule: the program on the Server's UDP ports, replaced on
    # each run.
    Remove-NetFirewallRule -Name $RuleName -ErrorAction SilentlyContinue
    New-NetFirewallRule -Name $RuleName -DisplayName 'udpstp Server' -Direction Inbound `
        -Action Allow -Protocol UDP -LocalPort @($port, $ephemeral) -Program $Program `
        -Profile Any | Out-Null

    Invoke-ServiceControl @('config', $Name, 'start=', 'demand') | Out-Null
    try {
        Start-Service -Name $Name
        (Get-Service -Name $Name).WaitForStatus('Running', [TimeSpan]::FromSeconds(30))
    } catch {
        $query = Native { & sc.exe query $Name }
        Fail ("the service didn't start. sc.exe query ${Name}:`n" +
            ($query -join "`n") + "`nIts log is in $Logs.")
    }
    # Only now: restarted a minute after any failure, a non-zero exit status
    # included, and started at boot.
    Invoke-ServiceControl @('failure', $Name, 'reset=', '86400', 'actions=', 'restart/60000/restart/60000/restart/60000') | Out-Null
    Invoke-ServiceControl @('failureflag', $Name, '1') | Out-Null
    Invoke-ServiceControl @('config', $Name, 'start=', 'auto') | Out-Null
    Write-Host "The service $Name runs as $Account and starts at boot; its log is in $Logs."
} catch {
    $failure = $_.Exception.Message
    if ($deploymentStarted -and (Get-Service -Name $Name -ErrorAction SilentlyContinue)) {
        $containment = @()
        try {
            Invoke-ServiceControl @('config', $Name, 'start=', 'disabled') | Out-Null
        } catch {
            $containment += $_.Exception.Message
        }
        try { Clear-Recovery } catch { $containment += $_.Exception.Message }
        try { Stop-AndVerify } catch { $containment += $_.Exception.Message }
        if ($containment.Count -eq 0) {
            Write-Error "$failure`nThe service is disabled and stopped, with recovery cleared. Changes already applied remain. Repair the problem and run -Install again; its log is in $Logs." -ErrorAction Continue
        } else {
            $observed = Get-Service -Name $Name -ErrorAction SilentlyContinue
            $state = if ($observed) { "$($observed.Status), startup $($observed.StartType)" } else { 'absent' }
            Write-Error "$failure`nContainment failed: $($containment -join '; '). Observed service: $state. Inspect the service before continuing." -ErrorAction Continue
        }
    } else {
        Write-Error $failure -ErrorAction Continue
    }
    exit 1
}
