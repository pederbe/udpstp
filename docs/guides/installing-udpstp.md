# Installing udpstp

One command installs the latest release of udpstp. Install it on both Test hosts, then follow the [quickstart](../getting-started.md) to share a key and run a Test.

<a id="platforms-and-installation-options"></a>
<a id="installation-methods"></a>

## Linux

The installer selects a matching binary and places it in `~/.local/bin`.

```sh
curl -fsSL https://udpstp.com/install.sh | sh
```

Run it again to update. It leaves your shell configuration unchanged. If `~/.local/bin` is outside your PATH, add it using your shell's configuration.

## macOS

```sh
brew install pederbe/tap/udpstp
```

Use `brew upgrade udpstp` for updates and `brew uninstall udpstp` to remove it. Homebrew installs udpstp on Linux too.

<a id="install-script"></a>

## Windows

In PowerShell

```powershell
irm https://udpstp.com/install.ps1 | iex
```

The installer uses your account's `LocalAppData\Programs\udpstp` folder and adds it to PATH. You can use udpstp in the current PowerShell window immediately. Sign out and back in to refresh PATH for other applications. The [Windows guide](windows.md) covers key-file permissions, firewall rules and running the Server as a service.

WinGet catalog approval is pending. The PowerShell installer and portable download are available now.

## Check that it works

```sh
udpstp --version
```

It prints `udpstp` and the version. If the command isn't found, check that the install folder is in your PATH. Then go on to the [quickstart](../getting-started.md).

## Other ways to install

| Platform | Method | Availability |
| --- | --- | --- |
| Linux | Archive installer, signed apt and dnf repositories | Available |
| Arch Linux | Native pacman package | Prepared for regular releases |
| OpenWrt | Locally built APK for x86_64 | Available for OpenWrt 25.12 |
| Linux and macOS | Homebrew | Available |
| Windows | PowerShell installer and portable download | Available |
| Windows | WinGet | Awaiting catalog approval |
| All three | Cargo source build and cargo-binstall | Available |

## Signed Linux packages

The apt and dnf repositories provide normal package-manager updates and the Server's systemd unit. The service stays disabled until you configure it and requires systemd 247 or later. [Hosting a Server](hosting-a-server.md) covers configuration and startup.

### Debian and Ubuntu

Add the repository once, then install udpstp

```sh
curl -fsSL https://packages.udpstp.com/udpstp.gpg | sudo tee "/usr/share/keyrings/udpstp.gpg" >/dev/null
echo "deb [signed-by=/usr/share/keyrings/udpstp.gpg] https://packages.udpstp.com/apt stable main" | sudo tee /etc/apt/sources.list.d/udpstp.list
sudo apt update
sudo apt install udpstp
```

Updates arrive with `apt upgrade`. Remove the package with `sudo apt remove udpstp`.

### Fedora, RHEL and similar distributions

```sh
sudo curl -fsSLo /etc/yum.repos.d/udpstp.repo https://packages.udpstp.com/udpstp.repo
sudo dnf install udpstp
```

Updates arrive with `dnf upgrade`. Remove the package with `sudo dnf remove udpstp`.

## Arch Linux

The regular release workflow prepares a native Arch Linux package. Once
`udpstp-x86_64.pkg.tar.zst` is listed on the [latest release](https://github.com/pederbe/udpstp/releases/latest),
download it and install it with pacman

```sh
sudo pacman -U udpstp-x86_64.pkg.tar.zst
```

Download the latest file again and repeat the command to update. Remove it with
`sudo pacman -R udpstp-bin`. No AUR account or Rust toolchain is needed. The
Server's service stays disabled until you configure it.

## OpenWrt

OpenWrt 25.12 on x86_64 can install a local APK built from the static Linux
release binary. The [OpenWrt package guide](https://github.com/pederbe/udpstp/tree/main/contrib/openwrt)
shows how to build it on a Linux computer with the OpenWrt SDK. This owned
source feed is separate from OpenWrt's upstream packages feed. There is no
hosted APK repository. Other router targets are not yet qualified.

Transfer your built `udpstp.apk` to the router, replacing `router` with its
address, then install it

```sh
scp -O udpstp.apk root@router:/tmp/udpstp.apk
ssh root@router 'apk add --no-network --allow-untrusted /tmp/udpstp.apk'
```

The local APK is unsigned. `--allow-untrusted` permits that file for this
command. Use an APK you built or obtained from someone you trust. The package
needs about 5 MiB of storage and uses OpenWrt's existing libc package, with no
additional runtime libraries to install.

Create a Shared Key on your computer as in the [quickstart](../getting-started.md),
then transfer its `keys.toml` through SSH into a private directory

```sh
ssh root@router 'set -e; umask 077; mkdir -p /etc/udpstp; chmod 700 /etc/udpstp; cat > /etc/udpstp/keys.toml; chmod 600 /etc/udpstp/keys.toml' < keys.toml
```

On the router, run a Client against your Server

```sh
udpstp client --key-file /etc/udpstp/keys.toml --max-hops 12 server.example.net
```

The package also provides `udpstp server` for manual use, as described in
[Hosting a Server](hosting-a-server.md). Installation starts no Server and
adds no service or firewall rule. Allow inbound UDP on the Standard Port only
from the network you intend to test if you run a Server on the router.

To update, build the latest APK and repeat the transfer and installation
commands. Stop any manually started Server before the upgrade and restart it
afterwards. Remove the package with `apk del udpstp`. Upgrade and removal leave
your key files and configuration in place.

## Portable downloads

The [release page](https://github.com/pederbe/udpstp/releases) provides archives for the supported targets. Extract an archive and run its `udpstp` executable. [Platform differences](../platforms.md) explains the available features.

## Cargo

With [Rust installed](https://rustup.rs), build a release from source

```sh
cargo install --locked udpstp
```

With [cargo-binstall](https://github.com/cargo-bins/cargo-binstall) installed, fetch a matching prebuilt release

```sh
cargo binstall udpstp
```

[Building from source](building-udpstp.md) covers compiler and platform prerequisites.

AUR and Scoop recipes remain available as sources for later catalog submissions.
