# Technology

udpstp uses Rust for the protocol, runtime and command-line application. The [architecture guide](architecture.md) maps the crates. [GLOSSARY.md](../GLOSSARY.md) defines their domain terms.

## Language and toolchain

The workspace uses Rust edition 2024 and Cargo resolver 3. `Cargo.toml` records the minimum supported Rust version. `rust-toolchain.toml` selects stable Rust and the development components. The project supports its documented range of stable Rust releases, with the oldest supported version tested in CI.

First-party Rust is safe except for the approved, isolated Windows Shared Key permission adapter in [ADR 0015](adr/0015-windows-shared-key-private-file-policy.md). Its safe decoder and policy retain the unsafe-code prohibition, as do the protocol, wire code, network runtime and ordinary application code. Expanding the exception requires the maintainer's explicit approval. Dependencies may contain unsafe code internally.

Formatting uses rustfmt. CI denies Clippy warnings across the workspace and tests. The release profile retains unwinding so the Data Phase Engine can contain a Test Connection's panic, as required by ADR 0008.

The Windows binary links its C runtime statically through `.cargo/config.toml`. Checks guard against accidentally adding a dependency on the Visual C++ Redistributable.

<a id="direct-dependencies"></a>

## Libraries and boundaries

| Area | Libraries and purpose |
| --- | --- |
| Wire format and authentication | RustCrypto `hmac` and `sha2`, with independent known-answer vectors. `zeroize` wipes key material. ADR 0001 fixes the KDF layout. |
| Protocol core | Deterministic state machines with no I/O. Time, received PDUs and transport outcomes arrive as inputs. |
| Control Phase | Tokio drives sockets and second-scale deadlines. Precise Data Phase pacing stays outside Tokio timers under ADR 0003. |
| Data Phase | Dedicated engine execution, Send Plans and bounded catch-up under ADRs 0008, 0011 and 0012. |
| Socket interfaces | `socket2`, `nix` and `rustix` provide supported operating-system APIs. `quinn-udp` is restricted to the Windows Standard Port listener under ADR 0007. |
| Application | `clap` handles arguments and help. Serde and TOML handle configuration and structured data. Tracing supplies logs. |
| Windows service | `windows-service` integrates with the Service Control Manager under ADR 0014. |

Exact dependency versions and features live in the Cargo manifests and lockfiles. [Platform differences](platforms.md) describes the operating-system limits and available fast paths.

### Dependency audit

`cargo deny check` checks advisories, licenses and dependency sources under `deny.toml`. The Dependency audit job runs in the full CI profile.

## Build and CI

[CI](../.github/workflows/ci.yml) runs for pull requests and manual dispatch. The full profile covers native Linux, macOS and Windows checks. Documentation-only changes use the documentation profile. The protected `main` branch requires an up-to-date pull request with passing CI. Website delivery builds and checks the merged revision separately, without repeating the full test matrix.

The maintained commands check formatting, Clippy, workspace tests, RFC traceability, links and glossary terms. Linux integration checks cover network namespaces, OB-UDPST, systemd, containers and packaged crate verification. Windows checks cover the installer and Server service. The website has its own build and metadata checks.

See [the validation workflow](testing.md#validation-workflow) for commands and tier boundaries.

[Nightly](../.github/workflows/nightly.yml) is manually dispatched. It runs checks kept outside the normal gate, including fuzzing, mutation testing, quarantined tests and precision diagnostics.

### xtask

`cargo xtask` is the alias in `.cargo/config.toml`.

| Command | Purpose |
| --- | --- |
| `cargo xtask req` | Validate RFC Requirement IDs, classifications and proving tests, and update conformance output. |
| `cargo xtask links` | Check relative documentation links and anchors. |
| `cargo xtask glossary` | Check domain terms and documented exceptions. |
| `cargo xtask site prepare` | Stage the documentation, generated reference and website sources. |
| `cargo xtask site finish` | Finish the site and check its links, metadata and presentation requirements. |

## Release packaging

The [release artifact guide](releasing.md) describes archives, native packages, checksums, dependency inventories and attestations.

Container and package-repository workflows reuse the released binaries. The [installation guide](guides/installing-udpstp.md) records distribution availability.
