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
