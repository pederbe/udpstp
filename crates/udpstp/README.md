<picture><source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/pederbe/udpstp/main/docs/assets/logo/udpstp-mark-dark.svg"><img src="https://raw.githubusercontent.com/pederbe/udpstp/main/docs/assets/logo/udpstp-mark-light.svg" alt="" width="56" height="56"></picture>

# udpstp

**Measure the capacity your network really delivers.**

udpstp is a Rust implementation of the UDP Speed Test Protocol in [RFC 9946](https://www.rfc-editor.org/rfc/rfc9946). Run a Client and a Server at the two ends of a network path to measure Maximum IP-Layer Capacity with the method in [RFC 9097](https://www.rfc-editor.org/rfc/rfc9097).

Use it to check access lines and circuits, compare network paths, or collect measurements for your own tools.

- **Measure in both directions.** Run authenticated Upstream and Downstream Tests.
- **See how the path behaves.** Inspect capacity, loss, delay variation and RTT in your terminal or collect JSON for your own tools.
- **Choose the load.** Combine Test Connections or use an Operator Key for Fixed-Rate Tests.
- **Use your existing setup.** Read OB-UDPST key files and export compatible JSON.
- **Build on the libraries.** Use the [wire codecs](https://docs.rs/udpstp-wire), [protocol core](https://docs.rs/udpstp-core) and [network runtime](https://docs.rs/udpstp-runtime) in Rust applications.

## Install

With Rust installed

```sh
cargo install --locked udpstp
```

Or install a prebuilt release for your platform.

**Linux**

```sh
curl -fsSL https://udpstp.com/install.sh | sh
```

**macOS**

```sh
brew install pederbe/tap/udpstp
```

**Windows**, in PowerShell

```powershell
irm https://udpstp.com/install.ps1 | iex
```

The [installation guide](https://udpstp.com/guides/installing-udpstp.html) covers signed Linux packages, portable downloads, Cargo and updates.

## Run a Test

Install on both hosts and give them the same private Key Table. [Getting started](https://udpstp.com/getting-started.html) shows how to create one and open the required UDP ports.

On the Server host

```sh
udpstp server --key-file keys.toml --max-hops 12
```

On the Client host

```sh
udpstp client --key-file keys.toml --max-hops 12 server.example.net
```

This runs a 10-second Downstream Test. Add `--direction upstream` for the other direction, or `--format json` to collect structured results. Choose MaxHops for your path.

## How a Test works

The Client asks the Server to start a Test and agrees its parameters. In a Downstream Test, the Server sends Load PDUs and uses reception feedback from the Client to adjust the rate. The [illustrated exchange](https://udpstp.com/#how-title) shows the Control Phase and Data Phase.

In an Upstream Test, the Client sends the Load PDUs and the Server still decides the rate from what it receives.

## Status

udpstp runs complete Tests and reports their results.

| Area | State |
| --- | --- |
| Authenticated Upstream and Downstream Tests, with text and JSON results | Done |
| Multi-Connection Tests and Operator-only Fixed-Rate Tests | Done |
| Authentication Modes 1 and 2, with Mode 2 the default | Done |
| Adaptive terminal layout, colors and compact results | Done |
| Linux batched I/O and available kernel UDP fast paths | Done |
| Linux systemd unit and Windows service | Done |
| Conformance Records and interoperability tests with OB-UDPST | Done |
| Hosted documentation | Done |
| Release binaries, signed Linux packages and installers | Done |

[Platform differences](https://udpstp.com/reference/platforms.html) describes options that depend on the operating system. JSON results, exit statuses, key files and address pseudonyms are stable from 0.1.0. The command line, configuration and Server JSON logs can change in minor 0.x releases, with changes listed in the [changelog](https://udpstp.com/changelog.html).

## Learn more

[Guides](https://udpstp.com/guides/index.html) · [Understanding results](https://udpstp.com/understanding-results.html) · [Hosting a Server](https://udpstp.com/guides/hosting-a-server.html) · [Architecture](https://udpstp.com/architecture.html) · [Contributing](https://github.com/pederbe/udpstp/blob/main/CONTRIBUTING.md) · [Security reports](https://github.com/pederbe/udpstp/blob/main/SECURITY.md)

Dual-licensed under [MIT](https://github.com/pederbe/udpstp/blob/main/LICENSE-MIT) or [Apache-2.0](https://github.com/pederbe/udpstp/blob/main/LICENSE-APACHE).
