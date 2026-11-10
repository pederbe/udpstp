# Runs one installer case with HKCU redirected to its disposable registry key.
# Keep this outside the installer: no production switch may select test state.
param(
    [Parameter(Mandatory = $true)][string]$RegistryRoot,
    [Parameter(Mandatory = $true)][string]$Script,
    [string]$Installer,
    [string]$To,
    [string]$Url,
    [string]$After,
    [switch]$NoModifyPath,
    [switch]$Uninstall
)

$ErrorActionPreference = 'Stop'
if ($RegistryRoot -notmatch '^HKEY_CURRENT_USER\\Software\\udpstp-install-test-[0-9a-f]{32}$') {
    throw 'the installer test requires its own temporary registry key'
}
Remove-PSDrive -Name HKCU -Force
New-PSDrive -Name HKCU -PSProvider Registry -Root $RegistryRoot | Out-Null
$root = Get-Item -LiteralPath 'HKCU:\'
try {
    if ($root.Name.TrimEnd('\') -ne $RegistryRoot) { throw 'HKCU was not redirected' }
}
finally { $root.Dispose() }

# The fallback must also stay under the test profile when LOCALAPPDATA is absent.
$fallback = [Environment]::GetFolderPath('LocalApplicationData', 'DoNotVerify')
if (-not $env:LOCALAPPDATA -and -not $fallback.StartsWith($env:USERPROFILE + '\', [StringComparison]::OrdinalIgnoreCase)) {
    throw 'LocalAppData did not follow the test profile'
}

$arguments = @{}
foreach ($name in $PSBoundParameters.Keys) {
    if ($name -notin @('RegistryRoot', 'Script')) { $arguments[$name] = $PSBoundParameters[$name] }
}
& $Script @arguments
exit $LASTEXITCODE
