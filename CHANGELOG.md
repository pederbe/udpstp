# Changelog

User-facing changes in udpstp, including capabilities, installation and compatibility.

## [Unreleased]

## [0.3.0] - 2026-11-14

### Changed

- Consolidate udpstp into one application crate ([#59](https://github.com/pederbe/udpstp/pull/59))

### Fixed

- Enforce traffic limits and protect validation credentials ([#57](https://github.com/pederbe/udpstp/pull/57))


## [0.2.0] - 2026-11-13

### Removed

- Remove release selection from the install scripts ([#34](https://github.com/pederbe/udpstp/pull/34))
- **Breaking:** Retire wake-up grouping and offer only the latest release ([#42](https://github.com/pederbe/udpstp/pull/42))

### Added

- Add native Arch Linux release packages ([#37](https://github.com/pederbe/udpstp/pull/37))
- Package udpstp for OpenWrt ([#45](https://github.com/pederbe/udpstp/pull/45))
- Add an optional OpenWrt Server service ([#46](https://github.com/pederbe/udpstp/pull/46))

### Deprecated

- Deprecate redundant Key Table format markers ([#41](https://github.com/pederbe/udpstp/pull/41))

### Fixed

- Make CLI diagnostics and recovery guidance actionable ([#40](https://github.com/pederbe/udpstp/pull/40))
- Keep failed configuration previews from opening logs ([#50](https://github.com/pederbe/udpstp/pull/50))

### Documentation

- Reorganize the documentation and add troubleshooting ([#32](https://github.com/pederbe/udpstp/pull/32))
- Move the documentation to udpstp.com/docs/ ([#33](https://github.com/pederbe/udpstp/pull/33))
- Restore punctuation before installation examples ([#47](https://github.com/pederbe/udpstp/pull/47))
- Complete the Cargo installation sentence ([#48](https://github.com/pederbe/udpstp/pull/48))
- Clarify the contributor link to design decisions ([#51](https://github.com/pederbe/udpstp/pull/51))
- Clarify ADR status and separate operating guidance ([#52](https://github.com/pederbe/udpstp/pull/52))
- Focus contributor guidance on contribution tasks ([#55](https://github.com/pederbe/udpstp/pull/55))


## [0.1.3] - 2026-11-10

### Other changes

- Trim completed test-fixture traffic and scope platform faults ([#17](https://github.com/pederbe/udpstp/pull/17))

## [0.1.2] - 2026-11-06

### Fixed

- Align the crates.io overview with the project README and update the documentation for published releases.
- Refresh the website's product overview and link Fastmash, Ripscope and Esdieff from About and the footer.

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
