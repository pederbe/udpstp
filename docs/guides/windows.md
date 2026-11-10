# udpstp on Windows

On Windows, udpstp needs a private key file for both roles, and a Server needs a firewall rule. A trial Server runs in a window, and an unattended one runs as a Windows service. Use these steps alongside [Running a Test](running-a-test.md) and [Hosting a Server](hosting-a-server.md). [Platform differences](../platforms.md) lists what Windows can't do yet.

## Generate a Shared Key

PowerShell can generate a 32-byte key without OpenSSL:

```powershell
$bytes = New-Object byte[] 32
$rng = [Security.Cryptography.RandomNumberGenerator]::Create()
$rng.GetBytes($bytes)
$rng.Dispose()
-join ($bytes | ForEach-Object { $_.ToString('x2') })
```

Put the output in `key_hex`, and copy the key file to the other host securely.

## Key file permissions

Both roles refuse a key file that other accounts can read or change, with exit status 51 and a message naming the reason. Only the running account, SYSTEM and Administrators may have access.

For a Client, or a Server you run in a window:

1. Put the key file in a folder of its own in your profile, holding only key files. Never use a folder created directly under `C:\`, where every signed-in account can change what's inside.
2. In Windows PowerShell 5.1, as the account that will run udpstp, go to that folder and run the following. For an OB-UDPST file, replace `keys.toml` with its name.

<!-- windows-private-file-setup:start -->
```powershell
$ErrorActionPreference = 'Stop'
$keys = (Resolve-Path -LiteralPath 'keys.toml').Path
$folder = Split-Path -Path $keys -Parent
$me = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$trusted = "(A;;FA;;;$me)(A;;FA;;;SY)(A;;FA;;;BA)"
$sections = [Security.AccessControl.AccessControlSections]::Owner -bor [Security.AccessControl.AccessControlSections]::Access
$directory = New-Object Security.AccessControl.DirectorySecurity
$directory.SetSecurityDescriptorSddlForm("O:${me}D:P(A;OICI;FA;;;$me)(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)", $sections)
[IO.Directory]::SetAccessControl($folder, $directory)
$file = New-Object Security.AccessControl.FileSecurity
$file.SetSecurityDescriptorSddlForm("O:${me}D:P$trusted", $sections)
[IO.File]::SetAccessControl($keys, $file)
# Examine the actual file, not only its containing folder.
Get-Acl -LiteralPath $keys | Format-List Owner, AreAccessRulesProtected, AccessToString
icacls $keys
```
<!-- windows-private-file-setup:end -->

3. Check the output. The file should be owned by your account, have inheritance protected, and list only SYSTEM, Administrators and your account, each with `(F)`.
4. Check that udpstp accepts it, with no key file error:

   ```powershell
   udpstp server --key-file keys.toml --print-config
   ```

The script replaces the folder's and the file's permissions completely, removing any other account's access. If other accounts need the folder, keep a copy elsewhere first. To add a key file later, copy it into the folder rather than moving it, since a moved file keeps its old permissions, and run the script again for it.

For an unattended Server, the [service installer](#run-the-server-as-a-service) sets up its own protected folder instead.

## Run a Client

The Client starts every exchange itself, so it normally needs no inbound firewall rule. A Server run from the same `udpstp.exe` still needs its own rule.

## Try a Server in a window

For a trial, before the Server first starts:

1. **Install the program where only administrators can change it.** As administrator, copy `udpstp.exe` into `C:\Program Files\udpstp`. The firewall rule names this path and lets in whatever program is there.
2. **Add the firewall rule**, as administrator, before `udpstp.exe` first runs from that path. Allow your `port` and the whole `ephemeral_ports` range, here the defaults:

   ```powershell
   $udpstp = "$env:ProgramW6432\udpstp\udpstp.exe"   # the udpstp.exe that runs the Server
   New-NetFirewallRule -DisplayName "udpstp Server" -Group "udpstp" `
       -Direction Inbound -Action Allow -Program $udpstp `
       -Protocol UDP -LocalPort '24601','49152-65535' -Profile Any
   ```

   For a Server that should answer only its own LAN, add `-RemoteAddress LocalSubnet`. To remove the rule later, run `Remove-NetFirewallRule -Group udpstp`.
3. **Protect the key file** as [above](#key-file-permissions), for your own account.
4. **Start the Server from a window that isn't elevated.** Never use "Run as administrator", which would give a network-facing process administrator rights:

   ```powershell
   & "$env:ProgramW6432\udpstp\udpstp.exe" server --key-file keys.toml --max-hops 12
   ```

The log appears in the window. Ctrl+C stops the Server, and so does closing the window or signing out. Check that a Client on another host can run a Test, since a loopback Test doesn't prove inbound access.

### Avoid reserved ports

Windows can reserve ranges of ports, which the Server then can't use. The Server skips them without a warning, so a range that overlaps them holds fewer Test Connections than it seems, and Clients get CONNFAIL. If you narrow `ephemeral_ports`, list the reserved ranges first and pick a range outside them:

```powershell
netsh int ipv4 show excludedportrange protocol=udp
netsh int ipv6 show excludedportrange protocol=udp
```

### If someone cancelled a firewall dialog

Without a rule for its path, Windows may ask whether to allow `udpstp.exe` when the Server starts. **Allow**, answered by an administrator, adds an Allow rule. **Cancel** adds Block rules, which override any Allow rule for that path, and Windows won't ask again.

To recover, remove only the dialog's Block rules for that path, as administrator, with `$udpstp` set as above:

```powershell
Get-NetFirewallApplicationFilter | Where-Object Program -eq $udpstp |
    Get-NetFirewallRule | Where-Object { $_.Name -like '*Query User*' -and $_.Action -eq 'Block' } |
    Remove-NetFirewallRule
```

Then confirm that the remaining rules name the program and your UDP ports:

```powershell
Get-NetFirewallApplicationFilter | Where-Object Program -eq $udpstp |
    Get-NetFirewallRule | Format-Table Name, Enabled, Direction, Action, Profile
Get-NetFirewallApplicationFilter | Where-Object Program -eq $udpstp |
    Get-NetFirewallRule | Get-NetFirewallPortFilter | Format-Table Protocol, LocalPort
```

This keeps rules an administrator created, and doesn't override an organisation's firewall policy.

## Run the Server as a service

An unattended Server runs as the Windows service `udpstp-server`, under its own virtual account.

1. As administrator, copy `udpstp.exe` to `C:\Program Files\udpstp\udpstp.exe`. The service runs whatever program is at that path, so it must be where only administrators can change it.
2. Put your Key Table in `C:\ProgramData\udpstp\keys.toml`.
3. From an elevated PowerShell, run the install script from the source's `contrib\windows` folder:

   ```powershell
   powershell -ExecutionPolicy Bypass -File .\udpstp-server.ps1 -Install
   ```

The script:

- locks `C:\ProgramData\udpstp` to Administrators and SYSTEM, with read access for the service and write access to its `logs` folder
- writes a starting configuration, `C:\ProgramData\udpstp\server.toml`, if there is none
- checks the configuration, so a mistake shows up here rather than as a service that won't start
- creates the service, running as `NT SERVICE\udpstp-server` with no privileges and no credential to set or expire
- adds the firewall rule `udpstp-server` for your `port` and `ephemeral_ports`
- starts the service once to check its keys, and only then sets it to start at boot and restart after a failure

If a step fails, the script leaves the service stopped and disabled and reports why. Fix the problem and run `-Install` again.

**To change the configuration**, edit `server.toml` and run `-Install` again. Write paths in single quotes, such as `'C:\ProgramData\udpstp\keys.toml'`, since a backslash inside double quotes starts an escape. Save the file as UTF-8, not as the UTF-16 that Windows PowerShell 5.1's `>` writes.

**To upgrade**, stop the service with `Stop-Service udpstp-server`, replace `udpstp.exe`, and start it again. The firewall rule stays valid because the path doesn't change.

**To uninstall**, run the script with `-Uninstall`. It keeps `C:\ProgramData\udpstp` unless you add `-RemoveData`.

**The log** is in `C:\ProgramData\udpstp\logs\server.<date>.log`, a new file each day, with the last 14 kept. If the service won't start, its log says why, and `sc.exe query udpstp-server` shows its exit status as `SERVICE_EXIT_CODE`: 50 for a configuration error, 51 for a key file error, 75 when it couldn't start.

If the service fails with error 1069, a group policy has removed "Log on as a service" from `NT SERVICE\ALL SERVICES`. Grant it to `NT SERVICE\udpstp-server`.

Where PowerShell runs in Constrained Language mode, the script stops. Make the same changes by hand, following the list above.

With Fast Startup on, which client editions of Windows enable, "Shut down" hibernates the service instead of stopping it. Restart Windows to stop it.

## Installation and firewall rules

Firewall rules name the program's full path. The [install script](installing-udpstp.md#install-script) keeps `udpstp.exe` at the same path across upgrades, so its rules keep working. If you move the program, add a rule for the new path.

Scoop and winget run udpstp through a shim or a versioned folder. A rule for the shim doesn't cover the program it runs, and a versioned path changes with each upgrade. For a Server, use the service above.
