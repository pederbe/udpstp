# Release artifacts and verification

The [Release workflow](../.github/workflows/release.yml) builds the archives, native packages and verification files described here. Distribution channels become available when their first release is published. See [Installing udpstp](guides/installing-udpstp.md) for availability and installation instructions.

## What a release contains

| Artifact | Contents and build |
|---|---|
| `udpstp-<tag>-x86_64-unknown-linux-gnu.tar.gz`, `…-aarch64-unknown-linux-gnu.tar.gz` | Linux builds targeting glibc 2.28 |
| `udpstp-<tag>-x86_64-unknown-linux-musl.tar.gz`, `…-aarch64-unknown-linux-musl.tar.gz`, `…-armv7-unknown-linux-musleabihf.tar.gz` | Static Linux builds for systems including Alpine and embedded Linux. The armv7 binary is cross-compiled without execution in the release workflow |
| `udpstp-<tag>-x86_64-pc-windows-msvc.zip` | Windows binary with its C runtime linked statically |
| `udpstp.exe` | The same Windows binary without an archive |
| `udpstp-<tag>-aarch64-apple-darwin.tar.gz`, `…-x86_64-apple-darwin.tar.gz` | macOS builds |
| `udpstp_<version>-1_amd64.deb`, `…_arm64.deb`, `udpstp-<version>-1.x86_64.rpm`, `….aarch64.rpm` | Linux packages installing the binary under `/usr/bin`, documentation, licences and a systemd unit. Installation does not enable the service |
| `udpstp-<tag>.cdx.json` | Rust binary dependency inventory in CycloneDX 1.5 |
| `udpstp.rb` | Homebrew formula rendered from the archive hashes for a full release |
| `SHA256SUMS` | SHA-256 hashes of the archives, packages, plain exe, dependency inventory and Homebrew formula |
| Build provenance | Signed attestations for attached files, including `SHA256SUMS` and the dependency inventory |
| SBOM attestations | Dependency inventory associated with the archives, native packages and plain exe |

`<tag>` includes the `v` prefix, such as `v0.1.0`. `<version>` omits it.

Each archive contains the binary, README, changelog, licence files and third-party notices under a directory named like the archive. The notices cover the binary's Rust dependencies and standard library. The [install scripts](../install/install.sh) and [`cargo-binstall` metadata](../crates/udpstp/Cargo.toml) consume these names, layouts and checksum format.

The software bill of materials (SBOM) includes Rust dependencies for all targets, including conditional Windows and macOS dependencies. It is a union, so a particular binary does not use every listed dependency. It does not inventory the container's Debian runtime or provide separate inventories per platform. Provenance and dependency inventory attestations answer different questions.

The workflow and [toolchain file](../rust-toolchain.toml) record build tool versions. Hosted runner images and bundled compilers can change, so those pins do not establish bit-for-bit reproducibility.

## Verify a download

Download the assets and `SHA256SUMS` from the same [GitHub release](https://github.com/pederbe/udpstp/releases). On Linux, run this from the download directory:

```sh
sha256sum -c SHA256SUMS
```

On macOS use `shasum -a 256 -c SHA256SUMS`. On Windows compare the listed hashes with `Get-FileHash -Algorithm SHA256`. Download every file listed in `SHA256SUMS` when checking the complete manifest.

A matching checksum detects a damaged or changed download. To verify its build source as well, use GitHub CLI 2.97.0 or later. Replace the tag, full source commit and filename below with those from the intended release:

```sh
gh attestation verify FILE --repo pederbe/udpstp \
    --signer-workflow pederbe/udpstp/.github/workflows/release.yml \
    --source-ref refs/tags/v0.1.0 --source-digest FULL_SOURCE_COMMIT
```

For an archive, native package or plain exe, verify the SBOM association separately with the same command and `--predicate-type https://cyclonedx.org/bom`. See the [GitHub CLI verification reference](https://cli.github.com/manual/gh_attestation_verify) for output and options.

## Containers and package repositories

The [container workflow](../.github/workflows/release-container.yml) uses the released Linux binaries in a [digest-pinned runtime image](../contrib/container/Release.Containerfile). It verifies the archive checksums and provenance before building. Native platform checks exercise authenticated Tests and shutdown behavior. Container provenance identifies `release-container.yml` as its signer workflow.

For a published container, verify the immutable image digest rather than a mutable tag:

```sh
gh attestation verify oci://ghcr.io/pederbe/udpstp@sha256:IMAGE_DIGEST \
    --repo pederbe/udpstp \
    --signer-workflow pederbe/udpstp/.github/workflows/release-container.yml \
    --source-ref refs/tags/v0.1.0 --source-digest FULL_SOURCE_COMMIT
```

The [signed apt and dnf repository](../contrib/package-repository/README.md) distributes the release packages without recompiling them. Homebrew uses the release archives. Cargo, cargo-binstall and other package-manager sources are described in the [installation guide](guides/installing-udpstp.md). Catalog admission and availability are specific to each channel.
