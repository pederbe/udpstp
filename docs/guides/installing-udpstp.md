# Installing udpstp

udpstp runs on Linux, macOS and Windows. The first binary release and its package-manager listings are being prepared. Until they are available, install the source version on both Test hosts with [Rust](https://rustup.rs).

```sh
cargo install --locked --git https://github.com/pederbe/udpstp udpstp
```

[Building from source](building-udpstp.md) covers the compiler and platform prerequisites. Then follow [Getting started](../getting-started.md) to share a key and run a Test.

<a id="platforms-and-installation-options"></a>

## Planned installation methods

These commands become usable after the corresponding release channel is published and verified. They install prebuilt binaries. Package-manager updates provide later versions.

| Platform | Method | Status |
| --- | --- | --- |
| Linux | Signed apt and dnf repositories | Pending first publication |
| Linux and macOS | Homebrew in the owned tap | Pending first publication |
| Windows | PowerShell installer and portable download | Pending first publication |
| Windows | WinGet | Pending publication and catalog admission |
| All three | Cargo source build and cargo-binstall | Registry publication pending |

### Linux

The archive installer will select a matching Linux binary. Rerun it to update an archive installation.

```sh
curl -fsSL https://udpstp.com/install.sh | sh
```

Signed apt and dnf repositories are also planned. Their packages install the Server's systemd unit and receive normal package-manager updates. The service stays disabled until you configure it. [Hosting a Server](hosting-a-server.md) covers configuration and startup.

### macOS

```sh
brew install pederbe/tap/udpstp
```

Use `brew upgrade udpstp` for updates and `brew uninstall udpstp` to remove it.

<a id="install-script"></a>

### Windows

In PowerShell

```powershell
irm https://udpstp.com/install.ps1 | iex
```

The installer uses your account's `LocalAppData\Programs\udpstp` folder and adds it to your PATH. Open a new terminal afterward. The [Windows guide](windows.md) covers key-file permissions, firewall rules and running the Server as a service.

### Portable downloads

The [release page](https://github.com/pederbe/udpstp/releases) will provide archives for each supported target. Extract an archive and run its `udpstp` executable. [Platform differences](../platforms.md) explains the available features.

### Cargo

After registry publication, build a release from source with

```sh
cargo install --locked udpstp
```

Or install a matching prebuilt release with

```sh
cargo binstall udpstp
```

AUR and Scoop recipes are retained in the repository for later activation. Their presence does not mean a public listing is available.
