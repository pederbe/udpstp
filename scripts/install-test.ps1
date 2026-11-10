# Tests install/install.ps1 against a fake release served over HTTP
# from 127.0.0.1: a stub udpstp.exe that prints a version (built with
# rustc), in the Release workflow's archive layout and naming
# (.github/workflows/release.yml), with its SHA256SUMS. Checks a good install
# (by -To, by the environment variables, into LocalAppData\Programs\udpstp, and
# through irm | iex), a tampered archive refused, a missing release, and an
# unknown architecture. Nothing leaves the machine, and nothing is installed
# outside a temporary directory. install-test.sh is its Linux and macOS
# twin.
#
# Needs rustc and Python 3 (for its http.server). Runs under Windows
# PowerShell 5.1 and PowerShell 7; the installer runs under the same one.
#
# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File scripts\install-test.ps1

[Diagnostics.CodeAnalysis.SuppressMessageAttribute('PSAvoidUsingWriteHost', '', Justification = 'A test script reports to the console.')]
[Diagnostics.CodeAnalysis.SuppressMessageAttribute('PSAvoidUsingPositionalParameters', '', Justification = 'The cases read as a table: description, outcome, text.')]
param()

Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$repoRoot = Split-Path -Parent $PSScriptRoot
$installer = Join-Path $repoRoot 'install\install.ps1'
$runner = Join-Path $PSScriptRoot 'install-test-run.ps1'
$tag = 'v0.0.0-test'
$target = 'x86_64-pc-windows-msvc'
$shell = (Get-Process -Id $PID).Path
$work = Join-Path ([IO.Path]::GetTempPath()) ('udpstp-install-test-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $work | Out-Null
$server = $null
$script:passed = 0
$script:failed = 0
$script:out = ''
$registryRoot = 'HKEY_CURRENT_USER\Software\udpstp-install-test-' + [guid]::NewGuid().ToString('N')
$registryPath = "Registry::$registryRoot"
$isolated = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $runner, '-RegistryRoot', $registryRoot, '-Script')
# The variables a case may set, cleared around each one.
$caseVariables = 'UDPSTP_INSTALL_BASE_URL', 'UDPSTP_INSTALL_DIR', 'UDPSTP_NO_MODIFY_PATH', 'PROCESSOR_ARCHITECTURE', 'PROCESSOR_ARCHITEW6432', 'USERPROFILE', 'LOCALAPPDATA', 'Path', 'UDPSTP_TEST_RELEASE', 'UDPSTP_TEST_GH_LOG'

# Read-only snapshot of the real account's whole Environment key, including
# raw expandable values and their registry types. Never restore it by writing.
function Get-AccountEnvironment {
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment')
    try {
        if (-not $key) { return 'absent' }
        $values = foreach ($name in $key.GetValueNames() | Sort-Object) {
            @($name, $key.GetValueKind($name).ToString(), $key.GetValue($name, $null, 'DoNotExpandEnvironmentNames'))
        }
        ConvertTo-Json -InputObject @($values) -Depth 5 -Compress
    }
    finally { if ($key) { $key.Dispose() } }
}

$accountBefore = Get-AccountEnvironment
# The installer uses only the HKCU provider. Refuse an added bypass before any
# case can execute it; the child remaps that drive and verifies the opened key.
$source = Get-Content -LiteralPath $installer -Raw
if ($source -match '(?i)\[Microsoft\.Win32\.Registry(?:Key)?\]|HKEY_|Registry::|SetEnvironmentVariable|\breg\.exe\b') {
    throw 'installer contains a persistence API that bypasses the isolated HKCU drive'
}

function Set-TestPath([string]$Value, [string]$Kind = 'ExpandString') {
    New-Item -Path "$registryPath\Environment" -Force | Out-Null
    New-ItemProperty -LiteralPath "$registryPath\Environment" -Name Path -Value $Value -PropertyType $Kind -Force | Out-Null
}

function Test-PathValue([string]$Description, [string]$Value, [string]$Kind = 'ExpandString') {
    $key = Get-Item -LiteralPath "$registryPath\Environment"
    try {
        $actual = $key.GetValue('Path', $null, 'DoNotExpandEnvironmentNames')
        if ($actual -ceq $Value -and $key.GetValueKind('Path').ToString() -eq $Kind) { Write-Pass $Description }
        else { Write-Fail $Description "expected $Kind '$Value', got '$actual'" }
    }
    finally { $key.Dispose() }
}

function Write-Pass([string]$Description) {
    Write-Host "ok: $Description"
    $script:passed++
}

function Write-Fail([string]$Description, [string]$Why) {
    Write-Host "FAIL: $Description"
    foreach ($line in ($script:out + "`n($Why)") -split "`r?`n") { Write-Host "    $line" }
    $script:failed++
}

# Runs Command (a script block) with the environment variables in Env set,
# and checks that it succeeds or fails as Want says, with Text in its output.
function Test-Case([string]$Description, [string]$Want, [string]$Text, [hashtable]$Env, [scriptblock]$Command) {
    $saved = @{}
    foreach ($name in $caseVariables) { $saved[$name] = [Environment]::GetEnvironmentVariable($name) }
    try {
        $env:USERPROFILE = Join-Path $work 'profile'
        $env:LOCALAPPDATA = Join-Path $env:USERPROFILE 'AppData\Local'
        Remove-Item Env:UDPSTP_INSTALL_DIR, Env:UDPSTP_NO_MODIFY_PATH -ErrorAction SilentlyContinue
        foreach ($name in $Env.Keys) {
            if ($null -eq $Env[$name]) { Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue }
            else { [Environment]::SetEnvironmentVariable($name, $Env[$name]) }
        }
        $ErrorActionPreference = 'Continue'
        $script:out = (& $Command 2>&1 | Out-String)
        $status = $LASTEXITCODE
        $ErrorActionPreference = 'Stop'
    }
    finally {
        # Keep absent variables absent; an empty PROCESSOR_ARCHITEW6432
        # changes the architecture passed to child processes in PowerShell 7.
        foreach ($name in $caseVariables) {
            if ($null -eq $saved[$name]) { Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue }
            else { [Environment]::SetEnvironmentVariable($name, $saved[$name]) }
        }
        if ((Get-AccountEnvironment) -cne $accountBefore) { throw 'the real account Environment key changed during a test' }
    }
    $got = if ($status -eq 0) { 'ok' } else { 'fails' }
    if ($got -eq $Want -and $script:out.Contains($Text)) {
        Write-Pass $Description
    }
    else {
        Write-Fail $Description "expected the command to be $Want with `"$Text`" in its output; it was $got"
    }
}

function Test-Contain([string]$Description, [string]$Text, [bool]$Present = $true) {
    if ($script:out.Contains($Text) -eq $Present) { Write-Pass $Description }
    else { Write-Fail $Description "expected `"$Text`" $(if ($Present) { 'in' } else { 'not in' }) the output" }
}

function Test-Installed([string]$Description, [string]$Exe) {
    $version = if (Test-Path -LiteralPath $Exe) { (& $Exe --version | Out-String).Trim() } else { '' }
    if ($version -eq 'udpstp 0.0.0-test') { Write-Pass $Description }
    else { Write-Fail $Description "$Exe isn't the stub" }
}

function Test-Nothing([string]$Description, [string]$Dir) {
    if (Test-Path -LiteralPath (Join-Path $Dir 'udpstp.exe')) { Write-Fail $Description "$Dir\udpstp.exe exists" }
    else { Write-Pass $Description }
}

try {
    New-Item -Path $registryPath -Force | Out-Null
    # The stub, and the fake release as the Release workflow lays it out.
    $stubSource = Join-Path $work 'stub.rs'
    Set-Content -LiteralPath $stubSource -Value 'fn main() { println!("udpstp 0.0.0-test"); }' -Encoding Ascii
    $name = "udpstp-$tag-$target"
    $stage = Join-Path $work $name
    New-Item -ItemType Directory -Path $stage | Out-Null
    & rustc --edition 2021 -o (Join-Path $stage 'udpstp.exe') $stubSource
    if ($LASTEXITCODE -ne 0) { throw 'rustc failed to build the stub' }
    Get-ChildItem -LiteralPath $stage -Filter '*.pdb' | Remove-Item
    foreach ($file in 'README.md', 'LICENSE-MIT', 'LICENSE-APACHE') {
        Copy-Item -LiteralPath (Join-Path $repoRoot $file) -Destination $stage
    }
    $release = Join-Path $work 'release'
    New-Item -ItemType Directory -Path $release | Out-Null
    Compress-Archive -Path $stage -DestinationPath (Join-Path $release "$name.zip")
    Copy-Item -LiteralPath (Join-Path $stage 'udpstp.exe') -Destination $release
    Set-Content -LiteralPath (Join-Path $release "udpstp-$tag-x86_64-unknown-linux-gnu.tar.gz") -Value 'not a tarball'
    Set-Content -LiteralPath (Join-Path $release "udpstp-$tag.cdx.json") -Value '{}'
    # The packages, which SHA256SUMS lists too: none may be taken
    # for an archive.
    foreach ($package in 'udpstp_0.0.0-1_amd64.deb', 'udpstp_0.0.0-1_arm64.deb', 'udpstp-0.0.0-1.x86_64.rpm', 'udpstp-0.0.0-1.aarch64.rpm') {
        Set-Content -LiteralPath (Join-Path $release $package) -Value 'not a package'
    }
    $sums = foreach ($file in Get-ChildItem -LiteralPath $release | Sort-Object Name) {
        '{0}  {1}' -f (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant(), $file.Name
    }
    [IO.File]::WriteAllText((Join-Path $release 'SHA256SUMS'), (($sums -join "`n") + "`n"))

    # Served as GitHub serves releases: /download/<tag>/<file>, and
    # /latest/download/<file> for the latest.
    $site = Join-Path $work 'site'
    foreach ($variant in 'good', 'tampered') {
        foreach ($dir in "$variant\download\$tag", "$variant\latest\download") {
            $path = Join-Path $site $dir
            New-Item -ItemType Directory -Path $path | Out-Null
            Copy-Item -Path (Join-Path $release '*') -Destination $path
            if ($variant -eq 'tampered') {
                $zip = Join-Path $path "$name.zip"
                $bytes = [IO.File]::ReadAllBytes($zip)
                $bytes[$bytes.Length - 1] = $bytes[$bytes.Length - 1] -bxor 1
                [IO.File]::WriteAllBytes($zip, $bytes)
            }
        }
    }
    Copy-Item -LiteralPath $installer -Destination (Join-Path $site 'install.ps1')
    # The site's command, irm | iex, from a script file: some endpoint
    # protection refuses a powershell -Command that pipes a download to iex.
    $piped = Join-Path $work 'piped.ps1'
    Set-Content -LiteralPath $piped -Encoding Ascii -Value @'
param([string]$Url, [string]$After)
irm $Url | iex
if (Get-Command udpstp -CommandType Application -ErrorAction SilentlyContinue) {
    "resolved: $((Get-Command udpstp -CommandType Application | Select-Object -First 1).Source)"
}
if ($After) { $After }
'@

    $python = Get-Command python, python3 -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $python) { throw 'install-test needs Python 3 for its HTTP server' }
    $listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
    $listener.Start()
    $port = $listener.LocalEndpoint.Port
    $listener.Stop()
    $server = Start-Process -FilePath $python.Source -ArgumentList '-m', 'http.server', '--bind', '127.0.0.1', '--directory', "`"$site`"", $port -PassThru -WindowStyle Hidden -RedirectStandardOutput (Join-Path $work 'server.out') -RedirectStandardError (Join-Path $work 'server.err')
    $url = "http://127.0.0.1:$port"
    $up = $false
    foreach ($try in 1..50) {
        try {
            Invoke-WebRequest -Uri "$url/install.ps1" -UseBasicParsing | Out-Null
            $up = $true
            break
        }
        catch { Start-Sleep -Milliseconds 100 }
    }
    if (-not $up) { throw "the HTTP server didn't start" }
    $good = "$url/good"
    $tampered = "$url/tampered"

    # Exercise the normal release path without the testing override.
    # The downloader serves only our local fixture; gh pretends to be logged
    # in but fails verification, and must never be invoked by the installer.
    $tools = Join-Path $work 'release-tools'
    New-Item -ItemType Directory -Path $tools | Out-Null
    Set-Content -LiteralPath (Join-Path $tools 'gh.cmd') -Encoding Ascii -Value @'
@echo off
echo %*>>"%UDPSTP_TEST_GH_LOG%"
if "%1 %2"=="auth status" exit /b 0
exit /b 1
'@
    $normal = Join-Path $work 'normal-release.ps1'
    Set-Content -LiteralPath $normal -Encoding Ascii -Value @'
param([string]$Installer, [string]$To)
function Invoke-WebRequest {
    param([string]$Uri, [string]$OutFile, [switch]$UseBasicParsing)
    $prefix = 'https://github.com/pederbe/udpstp/releases/'
    if (-not $Uri.StartsWith($prefix)) { throw "unexpected download: $Uri" }
    $relative = $Uri.Substring($prefix.Length).Replace('/', '\')
    Copy-Item -LiteralPath (Join-Path $env:UDPSTP_TEST_RELEASE $relative) -Destination $OutFile
}
& $Installer -To $To
exit $LASTEXITCODE
'@
    $dir = Join-Path $work 'no-gh'
    $ghLog = Join-Path $work 'gh.log'
    Test-Case 'a release installs without invoking gh' ok "Installed udpstp 0.0.0-test to $dir\udpstp.exe" @{
        UDPSTP_INSTALL_BASE_URL = $null
        UDPSTP_TEST_RELEASE = Join-Path $site 'good'
        UDPSTP_TEST_GH_LOG = $ghLog
        Path = "$tools;$env:Path"
    } {
        & $shell @isolated $normal -Installer $installer -To $dir
    }
    Test-Installed 'the normal release path installs the stub' "$dir\udpstp.exe"
    Test-PathValue 'an absent Environment key gets a PATH with no empty first entry' $dir
    Test-Contain 'the normal release path verifies the checksum' 'Checksum verified'
    if (-not (Test-Path -LiteralPath $ghLog)) { Write-Pass 'gh is never invoked' }
    else { Write-Fail 'gh is never invoked' (Get-Content -LiteralPath $ghLog | Out-String) }

    $dir = Join-Path $work 'to'
    $originalPath = '%USERPROFILE%\before;;C:\after\'
    Set-TestPath $originalPath
    Test-Case 'the latest release installs with -To' ok "Installed udpstp 0.0.0-test to $dir\udpstp.exe" @{ UDPSTP_INSTALL_BASE_URL = $good } {
        & $shell @isolated $installer -To $dir
    }
    Test-Installed '-To installs the stub' "$dir\udpstp.exe"
    Test-Contain 'the checksum is verified' 'Checksum verified'
    Test-PathValue 'the directory is appended without expanding or changing the existing PATH' "$originalPath;$dir"
    Test-Contain 'the next step is printed' 'Next: run udpstp --help'
    Test-Case 'a repeated install does not duplicate the PATH entry' ok 'Installed udpstp' @{ UDPSTP_INSTALL_BASE_URL = $good } {
        & $shell @isolated $installer -To $dir
    }
    Test-PathValue 'reinstall leaves the raw PATH and its type unchanged' "$originalPath;$dir"

    Set-TestPath '%USERPROFILE%\literal;C:\before' String
    Test-Case 'REG_SZ remains REG_SZ' ok 'Installed udpstp' @{ UDPSTP_INSTALL_BASE_URL = $good } {
        & $shell @isolated $installer -To $dir
    }
    Test-PathValue 'REG_SZ variables stay literal and the directory comes last' "%USERPROFILE%\literal;C:\before;$dir" String

    $dir = Join-Path $work 'dir'
    Test-Case 'the latest release installs into UDPSTP_INSTALL_DIR' ok "Installed udpstp 0.0.0-test to $dir\udpstp.exe" @{ UDPSTP_INSTALL_BASE_URL = $good; UDPSTP_INSTALL_DIR = $dir } {
        $env:Path = "$dir;$env:Path"
        try { & $shell @isolated $installer }
        finally { $env:Path = $env:Path.Substring($dir.Length + 1) }
    }
    Test-Installed 'UDPSTP_INSTALL_DIR installs the stub' "$dir\udpstp.exe"
    Test-Contain "a directory on the PATH isn't warned about" "isn't on your PATH" $false

    $profileDir = Join-Path $work 'profile'
    New-Item -ItemType Directory -Path $profileDir -Force | Out-Null
    $localAppData = Join-Path $profileDir 'AppData\Local'
    $dir = Join-Path $localAppData 'Programs\udpstp'
    Test-Case 'the default directory is LOCALAPPDATA\Programs\udpstp' ok "Installed udpstp 0.0.0-test to $dir\udpstp.exe" @{ UDPSTP_INSTALL_BASE_URL = $good; USERPROFILE = $profileDir; LOCALAPPDATA = $localAppData } {
        & $shell @isolated $installer
    }
    Test-Installed 'the default directory gets the stub' "$dir\udpstp.exe"

    Set-TestPath '"%LOCALAPPDATA%\Programs\UDPSTP\";C:\after'
    Test-Case 'a quoted expandable PATH entry already names the default folder' ok 'Installed udpstp' @{ UDPSTP_INSTALL_BASE_URL = $good; LOCALAPPDATA = $localAppData } {
        & $shell @isolated $installer
    }
    Test-PathValue 'case and trailing slash differences do not add a duplicate' '"%LOCALAPPDATA%\Programs\UDPSTP\";C:\after'
    Test-Case 'LocalAppData falls back to the Windows known folder' ok "Installed udpstp 0.0.0-test to $dir\udpstp.exe" @{ UDPSTP_INSTALL_BASE_URL = $good; LOCALAPPDATA = $null } {
        & $shell @isolated $installer
    }

    $dir = Join-Path $work "O'Brien tools"
    Set-TestPath $originalPath
    Test-Case 'PATH changes can be skipped by parameter' ok 'Installed udpstp' @{ UDPSTP_INSTALL_BASE_URL = $good } {
        & $shell @isolated $installer -To $dir -NoModifyPath
    }
    Test-PathValue '-NoModifyPath preserves the account PATH' $originalPath
    Test-Contain 'the direct command escapes an apostrophe' "Next: run & '$($dir.Replace("'", "''"))\udpstp.exe' --help"
    Test-Case 'PATH changes can be skipped under irm | iex' ok 'Installed udpstp' @{ UDPSTP_INSTALL_BASE_URL = $good; UDPSTP_INSTALL_DIR = $dir; UDPSTP_NO_MODIFY_PATH = '1' } {
        & $shell @isolated $piped -Url "$url/install.ps1"
    }
    Test-PathValue 'UDPSTP_NO_MODIFY_PATH preserves the account PATH' $originalPath
    Test-Contain 'the opt-out keeps the current process PATH unchanged' "resolved: $dir\udpstp.exe" $false
    Test-Case 'spaces and apostrophes work in a persistent PATH entry' ok 'Installed udpstp' @{ UDPSTP_INSTALL_BASE_URL = $good } {
        & $shell @isolated $installer -To $dir
    }
    Test-PathValue 'the apostrophe is stored literally' "$originalPath;$dir"

    $dir = Join-Path $work 'iex'
    Test-Case 'irm | iex, as the site''s command runs it' ok "Installed udpstp 0.0.0-test to $dir\udpstp.exe" @{ UDPSTP_INSTALL_BASE_URL = $good; UDPSTP_INSTALL_DIR = $dir } {
        & $shell @isolated $piped -Url "$url/install.ps1"
    }
    Test-Installed 'irm | iex installs the stub' "$dir\udpstp.exe"
    Test-Contain 'irm | iex updates the current process PATH' "resolved: $dir\udpstp.exe"

    $dir = Join-Path $work 'tampered'
    Set-TestPath $originalPath
    Test-Case 'a tampered archive is refused' fails 'checksum mismatch' @{ UDPSTP_INSTALL_BASE_URL = $tampered } {
        & $shell @isolated $installer -To $dir
    }
    Test-Nothing 'nothing is installed from a tampered archive' $dir
    Test-PathValue 'a checksum failure leaves PATH unchanged' $originalPath
    $dir = Join-Path $work 'to'
    $before = (Get-FileHash -LiteralPath "$dir\udpstp.exe").Hash
    Test-Case 'a tampered archive leaves an installed udpstp alone' fails 'checksum mismatch' @{ UDPSTP_INSTALL_BASE_URL = $tampered } {
        & $shell @isolated $installer -To $dir
    }
    if ((Get-FileHash -LiteralPath "$dir\udpstp.exe").Hash -eq $before) { Write-Pass 'the installed udpstp is unchanged' }
    else { Write-Fail 'the installed udpstp is unchanged' 'udpstp.exe changed' }

    $dir = Join-Path $work 'iex-tampered'
    Test-Case 'under irm | iex, a failure returns to the shell rather than exiting' ok 'still here' @{ UDPSTP_INSTALL_BASE_URL = $tampered; UDPSTP_INSTALL_DIR = $dir } {
        & $shell @isolated $piped -Url "$url/install.ps1" -After 'still here'
    }
    Test-Contain 'the failure under irm | iex is reported' 'checksum mismatch'
    Test-Nothing 'nothing is installed from a tampered archive under irm | iex' $dir

    $dir = Join-Path $work 'missing'
    Test-Case 'a missing release is refused' fails 'no release is published' @{ UDPSTP_INSTALL_BASE_URL = "$url/absent" } {
        & $shell @isolated $installer -To $dir
    }
    Test-Nothing 'nothing is installed for a missing release' $dir

    $dir = Join-Path $work 'x86'
    Test-Case 'an unknown architecture is refused' fails 'there is no udpstp release for Windows on x86' @{ UDPSTP_INSTALL_BASE_URL = $good; PROCESSOR_ARCHITECTURE = 'x86'; PROCESSOR_ARCHITEW6432 = $null } {
        & $shell @isolated $installer -To $dir
    }
    Test-Nothing 'nothing is installed for an unknown architecture' $dir

    $constrained = Join-Path $work 'constrained.ps1'
    Set-Content -LiteralPath $constrained -Encoding Ascii -Value @'
param([string]$Installer, [string]$To)
$ExecutionContext.SessionState.LanguageMode = 'ConstrainedLanguage'
& $Installer -To $To
'@
    $dir = Join-Path $work 'constrained'
    Test-Case 'Constrained Language mode stops with manual steps' fails 'PowerShell is in Constrained Language mode. Download' @{ UDPSTP_INSTALL_BASE_URL = 'http://127.0.0.1:1' } {
        & $shell @isolated $constrained -Installer $installer -To $dir
    }
    Test-Nothing 'Constrained Language mode installs nothing' $dir
    Test-PathValue 'Constrained Language mode leaves PATH unchanged' $originalPath

    # The public uninstall command removes only this binary and this PATH
    # component, even offline and when the directory contains other files.
    $dir = Join-Path $work 'uninstall'
    New-Item -ItemType Directory -Path $dir | Out-Null
    Copy-Item -LiteralPath (Join-Path $stage 'udpstp.exe') -Destination $dir
    Set-Content -LiteralPath (Join-Path $dir 'keep.txt') -Value 'keep me'
    Set-TestPath "%USERPROFILE%\before;;$dir\;$dir-extra;C:\after\"
    Test-Case 'uninstall works offline' ok "Uninstalled udpstp.exe from $dir" @{ UDPSTP_INSTALL_BASE_URL = 'http://127.0.0.1:1' } {
        & $shell @isolated $installer -Uninstall -To $dir
    }
    Test-Nothing 'uninstall removes the binary' $dir
    Test-PathValue 'uninstall removes only the matching PATH component' "%USERPROFILE%\before;;$dir-extra;C:\after\"
    if ((Get-Content -LiteralPath (Join-Path $dir 'keep.txt')) -eq 'keep me') { Write-Pass 'uninstall keeps unrelated files' }
    else { Write-Fail 'uninstall keeps unrelated files' 'keep.txt changed' }
    Test-Case 'uninstall can be repeated' ok 'Uninstalled udpstp.exe' @{ UDPSTP_INSTALL_BASE_URL = 'http://127.0.0.1:1' } {
        & $shell @isolated $installer -Uninstall -To $dir
    }
    Test-PathValue 'repeated uninstall leaves PATH unchanged' "%USERPROFILE%\before;;$dir-extra;C:\after\"
    Set-TestPath $dir String
    Test-Case 'uninstall removes a PATH containing only this directory' ok 'Uninstalled udpstp.exe' @{} {
        & $shell @isolated $installer -Uninstall -To $dir
    }
    $key = Get-Item -LiteralPath "$registryPath\Environment"
    try {
        if ($key.GetValueNames() -notcontains 'Path') { Write-Pass 'the sole PATH entry is deleted' }
        else { Write-Fail 'the sole PATH entry is deleted' 'Path still exists' }
    }
    finally { $key.Dispose() }

    Remove-Item -LiteralPath "$registryPath\Environment" -Recurse -Force
    $uninstallSession = Join-Path $work 'uninstall-session.ps1'
    Set-Content -LiteralPath $uninstallSession -Encoding Ascii -Value @'
param([string]$Installer, [string]$To)
$env:Path = "$env:Path;$To"
& $Installer -Uninstall -To $To
if (($env:Path -split ';') -contains $To) { throw 'the current process still has the uninstalled directory on PATH' }
'current process PATH cleaned'
'@
    Test-Case 'uninstall cleans the process PATH even without an account Environment key' ok 'current process PATH cleaned' @{} {
        & $shell @isolated $uninstallSession -Installer $installer -To $dir
    }
}
finally {
    if ($server) { Stop-Process -Id $server.Id -Force -ErrorAction SilentlyContinue; $server.WaitForExit(5000) | Out-Null }
    if ($registryPath -notmatch '^Registry::HKEY_CURRENT_USER\\Software\\udpstp-install-test-[0-9a-f]{32}$') { throw 'invalid cleanup key' }
    Remove-Item -LiteralPath $registryPath -Recurse -Force -ErrorAction SilentlyContinue
    if (-not ([IO.Path]::GetFullPath($work)).StartsWith([IO.Path]::GetTempPath(), [StringComparison]::OrdinalIgnoreCase)) { throw 'invalid cleanup directory' }
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
    if ((Get-AccountEnvironment) -cne $accountBefore) { throw 'the real account Environment key changed' }
    Write-Pass 'the real account Environment key is unchanged'
}

Write-Host "install-test: $script:passed passed, $script:failed failed"
if ($script:failed -ne 0) { exit 1 }
