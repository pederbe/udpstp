# Changelog

User-facing changes in udpstp, including capabilities, installation and compatibility.

## [0.1.1] - 2026-11-05

### Added

- A Client and Server for the UDP Speed Test Protocol in RFC 9946, using the Maximum IP-Layer Capacity method in RFC 9097.
- Authenticated Upstream and Downstream Tests, Multi-Connection Tests, and Operator-only Fixed-Rate Tests. Mode 2 is the default. Mode 1 is available when selected, and Unauthenticated Mode requires both ends to opt in.
- Setup and Activation exchanges, Load Rate Adjustment Algorithms B and C, Lost Status Backoff, the Sending Rate Table, optional header checksums, the STOP exchange and protocol watchdogs.
- Maximum IP-Layer Capacity per Capacity Mode, with loss, delay variation, RTT and sender bit rate. Text and JSON results preserve measured Sub-Intervals from incomplete Tests and explain warnings and partial completion. JSON supports OB-UDPST-compatible fields and a separate udpstp extension.
- Adaptive terminal layout, compact results and `--color auto|always|never`. Automatic color handles stdout and stderr independently. JSON, printed configuration and persistent logs stay plain.
- TOML Key Tables with lifetimes, Operator Keys and Quotas, plus OB-UDPST key files. Unix ownership and permissions are checked before reading keys. Windows requires the supported local-NTFS private-file policy and distinguishes a policy mismatch from unverifiable permissions.
- Client-selected Server addresses, with explicit authorization of alternate addresses for authenticated Tests. Server admission limits, replay protection, Setup Request budgets and rate-limited diagnostics.
- Server TOML configuration, command-line overrides, validated configuration previews, structured logs, periodic statistics and address-privacy controls. Settings-only previews open no key files, log files or listeners.
- A systemd unit, a Windows service installer with a dedicated virtual account, and container build and publication workflows for the Server. Services require configuration before use.
- Available Linux batched I/O and kernel UDP fast paths, receive timestamps and Receive Buffer Overflow reporting. Experimental send timing diagnostics are separate from stable interfaces.
- RFC requirement traceability, Conformance Records, deterministic protocol simulation, fault injection and interoperability tests with OB-UDPST v9.1.0 in the documented scope.
- Release packaging and installer sources for Linux, macOS and Windows. Signed apt/dnf repository tooling, Homebrew, cargo-binstall and registry manifest sources are included. Distribution availability follows actual publication and catalog admission.

### Compatibility

- JSON results, exit statuses, key files and address pseudonyms are stable from 0.1.0. The command line, configuration and Server JSON logs may change in minor 0.x releases under the documented interface policy.
- Windows does not yet support Don't Fragment or DSCP marking. Other platform limits are documented in `docs/platforms.md`.
- This is the first public release. There is no earlier public version to upgrade from. Existing development installations should review the key-file policy and configuration before switching.
