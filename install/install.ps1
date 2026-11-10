# Installs udpstp from its GitHub release, on Windows.
#
#   irm https://udpstp.com/install.ps1 | iex
#
# Through irm | iex, options come from environment variables:
#
#   $env:UDPSTP_INSTALL_DIR = "$env:LOCALAPPDATA\Programs\udpstp"
#   irm https://udpstp.com/install.ps1 | iex
#
# Saved to a file, it takes them as parameters as well:
#
#   .\install.ps1 -To "$env:LOCALAPPDATA\Programs\udpstp"
#   .\install.ps1 -NoModifyPath
#   .\install.ps1 -Uninstall
#
# What it does, in order (docs/guides/installing-udpstp.md, "Install script"):
#
#  1. Picks the release target: x86_64-pc-windows-msvc, which Windows 11 on
#     Arm runs under emulation.
#  2. Downloads SHA256SUMS, then the archive it names for the target, over
#     HTTPS from https://github.com/pederbe/udpstp/releases, from the latest
#     release.
#  3. Checks the archive against SHA256SUMS with Get-FileHash.
#  4. Copies udpstp.exe into %LOCALAPPDATA%\Programs\udpstp, or the directory -To or
#     $env:UDPSTP_INSTALL_DIR names, without elevation, and runs it to print
#     its version.
#  5. Appends that directory to the account's PATH and this process's PATH,
#     unless -NoModifyPath or UDPSTP_NO_MODIFY_PATH=1 is set. Existing PATH
#     entries, expandable variables and the registry value's type are kept.
#     Sign out and back in to refresh other applications.
#
# -Uninstall needs no download: it removes only udpstp.exe and the matching
# PATH entry, keeping the folder and any other files. Use the same -To for a
# custom directory. -NoModifyPath also leaves PATH alone during uninstall.
#
# For testing only: $env:UDPSTP_INSTALL_BASE_URL replaces
# https://github.com/pederbe/udpstp/releases, so that
# scripts/install-test.ps1 can serve a fake release over plain HTTP from
# 127.0.0.1. It also lifts the HTTPS-only rule. Never set it otherwise.
#
# The work is done in functions, called on the last line. Under irm | iex
# a failure prints its message and leaves the PowerShell window open; run
# as a file, the script then exits with status 1.

param(
    # The directory to install udpstp.exe into, rather than LocalAppData\Programs\udpstp.
    [string]$To = $env:UDPSTP_INSTALL_DIR,
    # Leave both the account's and this process's PATH alone.
    [switch]$NoModifyPath = ($env:UDPSTP_NO_MODIFY_PATH -eq '1'),
    # Remove udpstp.exe and its PATH entry, keeping everything else.
    [switch]$Uninstall
)

function Test-UdpstpPathEntry([string]$Entry, [string]$Directory, [bool]$Expand) {
    if ($Expand) { $Entry = [Environment]::ExpandEnvironmentVariables($Entry) }
    $Entry.Trim('"').TrimEnd('\', '/').Equals($Directory.TrimEnd('\', '/'), [StringComparison]::OrdinalIgnoreCase)
}

function Update-UdpstpPath([string]$Directory, [switch]$Remove) {
    # Use the provider's HKCU drive so the install tests can redirect the
    # entire registry boundary before running this script.
    $root = Get-Item -LiteralPath 'HKCU:\'
    $key = $null
    try {
        $key = $root.OpenSubKey('Environment', $true)
        if (-not $key -and -not $Remove) {
            $writable = $root.OpenSubKey('', $true)
            try { $key = $writable.CreateSubKey('Environment') }
            finally { $writable.Dispose() }
        }
        if ($key) {
            $raw = $key.GetValue('Path', $null, 'DoNotExpandEnvironmentNames')
            $kind = if ($key.GetValueNames() -contains 'Path') { $key.GetValueKind('Path') } else { [Microsoft.Win32.RegistryValueKind]::ExpandString }
            if ($kind -notin @('String', 'ExpandString')) { throw 'the account PATH is not a string; correct it in Windows Environment Variables before installing' }
            $entries = @($raw -split ';')
            $matching = @($entries | Where-Object { Test-UdpstpPathEntry $_ $Directory ($kind -eq 'ExpandString') })
            if ($Remove -and $matching.Count -gt 0) {
                $kept = @($entries | Where-Object { -not (Test-UdpstpPathEntry $_ $Directory ($kind -eq 'ExpandString')) })
                if ($kept.Count -eq 0) { $key.DeleteValue('Path') }
                else { $key.SetValue('Path', ($kept -join ';'), $kind) }
                Write-Host "Removed $Directory from your account's PATH. Sign out and back in to refresh other applications."
            }
            elseif (-not $Remove -and $matching.Count -eq 0) {
                $new = if ([string]::IsNullOrEmpty($raw)) { $Directory } else { "$raw;$Directory" }
                $key.SetValue('Path', $new, $kind)
                Write-Host "Added $Directory to your account's PATH. Sign out and back in to refresh other applications."
            }
        }
    }
    finally {
        if ($key) { $key.Dispose() }
        $root.Dispose()
    }
    $matching = @($env:Path -split ';' | Where-Object { Test-UdpstpPathEntry $_ $Directory $false })
    if ($Remove) {
        $env:Path = (@($env:Path -split ';' | Where-Object { -not (Test-UdpstpPathEntry $_ $Directory $false) }) -join ';')
    }
    elseif ($matching.Count -eq 0) {
        $env:Path = if ($env:Path) { "$env:Path;$Directory" } else { $Directory }
    }
}

function Install-Udpstp {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute('PSAvoidUsingWriteHost', '', Justification = 'An interactive installer talks to the console, and its output must not reach the pipeline under iex.')]
    param(
        [string]$To,
        [switch]$NoModifyPath,
        [switch]$Uninstall
    )

    Set-StrictMode -Version 3.0
    $ErrorActionPreference = 'Stop'
    # Windows PowerShell's progress bar slows Invoke-WebRequest down tenfold.
    $ProgressPreference = 'SilentlyContinue'

    if ($ExecutionContext.SessionState.LanguageMode -ne 'FullLanguage') {
        throw 'PowerShell is in Constrained Language mode. Download the Windows zip and SHA256SUMS from https://github.com/pederbe/udpstp/releases, verify its SHA-256 with Get-FileHash, extract udpstp.exe into LocalAppData\Programs\udpstp, and add that folder using Edit environment variables for your account in Windows.'
    }
    if ([Environment]::OSVersion.Platform -ne 'Win32NT') {
        throw 'install.ps1 is for Windows; on Linux and macOS, run: curl -fsSL https://udpstp.com/install.sh | sh'
    }

    $repo = 'pederbe/udpstp'
    $releases = "https://github.com/$repo/releases"
    $fromSource = 'build from source instead: cargo install --locked udpstp'

    # The options.
    if (-not $To) {
        $baseDir = $env:LOCALAPPDATA
        if (-not $baseDir) {
            $baseDir = [Environment]::GetFolderPath('LocalApplicationData', 'DoNotVerify')
        }
        if ([string]::IsNullOrWhiteSpace($baseDir)) {
            throw 'could not find LocalAppData; specify an install directory with -To or UDPSTP_INSTALL_DIR'
        }
        $To = Join-Path $baseDir 'Programs\udpstp'
    }
    $To = [IO.Path]::GetFullPath($To)
    if ($To -ne [IO.Path]::GetPathRoot($To)) { $To = $To.TrimEnd('\') }
    if (-not $NoModifyPath -and ($To.Contains(';') -or [Environment]::ExpandEnvironmentVariables($To) -ne $To)) {
        throw 'the directory cannot be represented as one literal PATH entry; choose another -To directory or use -NoModifyPath'
    }

    if ($Uninstall) {
        $exe = Join-Path $To 'udpstp.exe'
        if (Test-Path -LiteralPath $exe -PathType Leaf) { Remove-Item -LiteralPath $exe -Force }
        if (-not $NoModifyPath) { Update-UdpstpPath $To -Remove }
        Write-Host "Uninstalled udpstp.exe from $To; other files in that directory were kept."
        return
    }

    $base = $releases
    $testing = $false
    if ($env:UDPSTP_INSTALL_BASE_URL) {
        $base = $env:UDPSTP_INSTALL_BASE_URL.TrimEnd('/')
        $testing = $true
        Write-Warning "UDPSTP_INSTALL_BASE_URL is set, so the release comes from $base, over any protocol. It is for testing only."
    }

    # The release target.
    # A 32-bit PowerShell on 64-bit Windows sees x86 in
    # PROCESSOR_ARCHITECTURE and the machine's own in PROCESSOR_ARCHITEW6432.
    $arch = $env:PROCESSOR_ARCHITEW6432
    if (-not $arch) { $arch = $env:PROCESSOR_ARCHITECTURE }
    switch ($arch) {
        'AMD64' { $target = 'x86_64-pc-windows-msvc' }
        'ARM64' {
            if ([Environment]::OSVersion.Version.Build -lt 22000) {
                throw "there is no udpstp release for Windows 10 on ARM64, which can't run x86_64 programs. You can $fromSource"
            }
            Write-Host 'Windows on ARM64: installing the x86_64 build, which Windows 11 runs under emulation'
            $target = 'x86_64-pc-windows-msvc'
        }
        default {
            throw "there is no udpstp release for Windows on $arch`: releases are built for x86_64 (AMD64). You can $fromSource"
        }
    }

    if (-not $testing -and $PSVersionTable.PSVersion.Major -lt 6) {
        # Windows PowerShell may not offer TLS 1.2 by default.
        [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    }

    function Save-Url([string]$Url, [string]$Path) {
        if (-not $testing -and -not $Url.StartsWith('https://')) {
            throw "refusing to download $Url, which isn't HTTPS"
        }
        Invoke-WebRequest -Uri $Url -OutFile $Path -UseBasicParsing
    }

    $tmp = Join-Path ([IO.Path]::GetTempPath()) ('udpstp-install-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $tmp | Out-Null
    try {
        # SHA256SUMS names the archive, and with it the tag of the latest
        # release.
        $sumsUrl = "$base/latest/download/SHA256SUMS"
        Write-Host "Looking for the latest release of udpstp for $target"
        $sums = Join-Path $tmp 'SHA256SUMS'
        try {
            Save-Url $sumsUrl $sums
        }
        catch {
            throw "couldn't download $sumsUrl`: no release is published, or the network is down ($($_.Exception.Message)). The releases are listed at $releases"
        }
        $suffix = "-$target.zip"
        $entries = @(
            foreach ($line in Get-Content -LiteralPath $sums) {
                if ($line -match '^([0-9a-f]{64}) [ *](udpstp-v[A-Za-z0-9._+-]+)$' -and $Matches[2].EndsWith($suffix)) {
                    [pscustomobject]@{ Hash = $Matches[1]; File = $Matches[2] }
                }
            }
        )
        if ($entries.Count -ne 1) {
            throw "the latest release has no archive for $target in its SHA256SUMS ($sumsUrl)"
        }
        $expected = $entries[0].Hash
        $file = $entries[0].File
        $tag = $file.Substring('udpstp-'.Length, $file.Length - 'udpstp-'.Length - $suffix.Length)

        $url = "$base/download/$tag/$file"
        $archive = Join-Path $tmp $file
        Write-Host "Downloading $url"
        try {
            Save-Url $url $archive
        }
        catch {
            throw "couldn't download $url ($($_.Exception.Message))"
        }
        $actual = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($actual -ne $expected) {
            throw "checksum mismatch: $file has SHA-256 $actual, but the release's SHA256SUMS says $expected. The download was corrupted or tampered with, so nothing was installed. Try again; if it happens again, report it privately: https://github.com/$repo/security/advisories/new"
        }
        Write-Host "Checksum verified: $file matches SHA256SUMS"

        $unpacked = Join-Path $tmp 'unpacked'
        try {
            Expand-Archive -LiteralPath $archive -DestinationPath $unpacked
        }
        catch {
            throw "couldn't unpack $file ($($_.Exception.Message))"
        }
        $binary = Join-Path (Join-Path $unpacked $file.Substring(0, $file.Length - '.zip'.Length)) 'udpstp.exe'
        if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) {
            throw "$file has no udpstp.exe"
        }

        $exe = Join-Path $To 'udpstp.exe'
        $staged = Join-Path $To ".udpstp-install-$PID.exe"
        try {
            New-Item -ItemType Directory -Force -Path $To | Out-Null
            Copy-Item -LiteralPath $binary -Destination $staged -Force
            Move-Item -LiteralPath $staged -Destination $exe -Force
        }
        catch {
            Remove-Item -LiteralPath $staged -Force -ErrorAction SilentlyContinue
            throw "couldn't install udpstp.exe into $To ($($_.Exception.Message)). If udpstp is running, stop it and try again; or name another directory with -To or UDPSTP_INSTALL_DIR"
        }
    }
    finally {
        Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }

    $ErrorActionPreference = 'Continue'
    $installed = (& $exe --version 2>&1 | Out-String).Trim()
    $ErrorActionPreference = 'Stop'
    if ($LASTEXITCODE -ne 0) {
        throw "installed $exe, but it doesn't run: $installed"
    }
    Write-Host ''
    Write-Host "Installed $installed to $exe"
    if (-not $NoModifyPath) { Update-UdpstpPath $To }
    $command = 'udpstp'
    $onPath = @($env:Path -split ';' | Where-Object { Test-UdpstpPathEntry $_ $To $false })
    if ($onPath.Count -eq 0) {
        Write-Warning "$To isn't on your PATH. To add it for your account, use Edit environment variables for your account in Windows, edit Path, and add $To as a new entry."
        $command = "& '" + $exe.Replace("'", "''") + "'"
    }
    else {
        $found = Get-Command udpstp -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($found -and $found.Source -ne $exe) {
            Write-Warning "$($found.Source) comes first on your PATH, so 'udpstp' runs that one, not $exe"
        }
    }
    Write-Host "Next: run $command --help, and read https://udpstp.com/docs/getting-started.html to run your first Test."
}

function Invoke-UdpstpInstall {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute('PSAvoidUsingWriteHost', '', Justification = 'An interactive installer talks to the console.')]
    param(
        [string]$To,
        [switch]$NoModifyPath,
        [switch]$Uninstall,
        [bool]$AsFile
    )
    try {
        Install-Udpstp -To $To -NoModifyPath:$NoModifyPath -Uninstall:$Uninstall
    }
    catch {
        Write-Host "udpstp-install: error: $($_.Exception.Message)" -ForegroundColor Red
        # Run as a file, fail with a status; under irm | iex, exiting would
        # close the window, so return instead.
        if ($AsFile) { exit 1 }
    }
}

Invoke-UdpstpInstall -To $To -NoModifyPath:$NoModifyPath -Uninstall:$Uninstall -AsFile ([bool]$PSCommandPath)
