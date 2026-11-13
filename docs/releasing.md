# Release artifacts and verification

## WinGet delivery

After publishing qualified Windows assets, the release workflow dispatches
`winget.yml` for that tag. The WinGet workflow verifies the published executable,
generates its portable manifest, and checks validation, installation, authenticated
loopback operation and removal on Windows. With submission enabled it uses
Komac to submit the version, then completes and reads back the PR description.

The [WinGet workflow](../.github/workflows/winget.yml) provides a preparation-only
route and a separate submission action. Its inputs and environment declarations
are the source for current options and credential requirements. Preparation does
not use the fork credential. Synchronization does not force or overwrite a
divergent fork. Retries reuse a matching open PR and preserve manual description
edits. The CLA checkbox reflects Microsoft's official check and never signs an
agreement. Microsoft controls catalog admission.

The [Release workflow](../.github/workflows/release.yml) builds the archives, native packages and verification files described here. Distribution channels become available when their first release is published. See [Installing udpstp](guides/installing-udpstp.md) for availability and installation instructions.

## Release process

[Release-plz](https://release-plz.dev) prepares Cargo versions and dated changelog
entries from merged pull request titles, including relevant changes outside the
Cargo packages. Migration instructions belong in the pull request's Solution
section. The [release workflow](../.github/workflows/release-plz.yml) owns the
schedule, release selection and refresh behavior. Required CI checks gate its
release pull request.

After preparing each release pull request, the workflow refreshes `fuzz/Cargo.lock` on its branch to match the local crate versions. It commits the lockfile only when it changes, before enabling auto-merge. Existing registry dependency versions stay locked.

Merging the release pull request publishes changed crates to crates.io in dependency order and creates their tags. The published crates share a version group so a feature or breaking change in a library also raises the binary's version appropriately. Unchanged crates may keep their previous versions. The binary uses `vVERSION`, while the libraries use `PACKAGE-vVERSION`. Only the binary creates a GitHub draft release, using its combined changelog section as the release notes. The Release workflow builds and attests the assets, attaches them to that draft, runs the downloaded-archive and native-package qualification, and publishes it after those checks pass. Publication starts the container and signed package repository workflows.

Published notes are already on `main` before tagging, and stay under their dated version heading. Keep `## [Unreleased]` empty. Release-plz manages version numbers and dependency requirements, including the library crates required by `cargo install udpstp`. Test helpers, benchmarks, fuzzing and `xtask` are not released.

Release-plz uses short-lived trusted-publishing credentials for Cargo uploads.
The workflow explicitly dispatches the binary build because tags and drafts
created with the built-in workflow token do not start another workflow. This
dispatch boundary matters when recovering a partially completed publication.

### Recovery

If preparation or a Cargo upload fails, fix the reported cause and rerun the original failed Release-plz job to finish still-unpublished packages. Check which versions were accepted before retrying.

Cargo publication, tag creation and GitHub draft creation are separate operations. If Cargo accepts a version but creating its tag or draft fails, rerunning release-plz does not restore the missing GitHub state. A green retry alone does not establish that the release finished. Recover using the exact source commit from the failed run, cross-checking each version uploaded by that run against its published crate's `.cargo_vcs_info.json`. For versions already accepted by Cargo, restore only missing version tags at that commit with `git tag` and `git push`. For an already-uploaded binary version, restore its missing draft with `gh release create --draft --verify-tag --notes-file`, using the dated changelog section from that same commit. Leave still-unpublished packages to the original job retry. Precreating their tags would cause release-plz to skip their Cargo uploads. Existing tags and drafts must match the intended release. Do not move tags or overwrite published releases.

After restoring missing items for accepted versions, rerun the original Release-plz job for any remaining unpublished packages. If the binary had already been uploaded before that retry, explicitly dispatch Release on its `vVERSION` tag. Otherwise the retry dispatches Release when it newly publishes the binary. Explicit dispatch is also the recovery if dispatching the binary build failed after successful tagging. Rerunning Release-plz alone will not repeat that dispatch for an already-published binary.

If packaging or qualification fails, the GitHub release remains a draft. Rerun the failed Release jobs. Asset uploads replace only files in the unpublished draft. Published releases are immutable.

Use the workflow's current inputs when retrying a delivery stage. A failure
stops the affected stage and is visible in GitHub Actions.

## What a release contains

| Artifact | Contents and build |
|---|---|
| `udpstp-<tag>-x86_64-unknown-linux-gnu.tar.gz`, `…-aarch64-unknown-linux-gnu.tar.gz` | Linux builds targeting glibc 2.28 |
| `udpstp-<tag>-x86_64-unknown-linux-musl.tar.gz`, `…-aarch64-unknown-linux-musl.tar.gz`, `…-armv7-unknown-linux-musleabihf.tar.gz` | Static Linux builds for systems including Alpine and embedded Linux. The armv7 binary is cross-compiled without execution in the release workflow |
| `udpstp-<tag>-x86_64-pc-windows-msvc.zip` | Windows binary with its C runtime linked statically |
| `udpstp.exe` | The same Windows binary without an archive |
| `udpstp-<tag>-aarch64-apple-darwin.tar.gz`, `…-x86_64-apple-darwin.tar.gz` | macOS builds |
| `udpstp_<version>-1_amd64.deb`, `…_arm64.deb`, `udpstp-<version>-1.x86_64.rpm`, `….aarch64.rpm` | Linux packages installing the binary under `/usr/bin`, documentation, licences and a systemd unit. Installation does not enable the service |
| `udpstp-x86_64.pkg.tar.zst` | Arch Linux package using the qualified release executable, documentation, licences and disabled systemd unit |
| `udpstp-bin.PKGBUILD`, `udpstp-bin.SRCINFO` | Binary package recipe and metadata with the release version and source checksums |
| `udpstp-<tag>.cdx.json` | Rust binary dependency inventory in CycloneDX 1.5 |
| `udpstp.rb` | Homebrew formula rendered from the archive hashes for a full release |
| `SHA256SUMS` | SHA-256 hashes of the archives, packages, plain exe, dependency inventory and Homebrew formula |
| Build provenance | Signed attestations for attached files, including `SHA256SUMS` and the dependency inventory |
| SBOM attestations | Dependency inventory associated with the archives, native packages and plain exe |

`<tag>` includes the `v` prefix, such as `v0.1.0`. `<version>` omits it.

Each archive contains the binary, README, changelog, licence files and third-party notices under a directory named like the archive. The notices cover the binary's Rust dependencies and standard library. The [install scripts](../install/install.sh) and [`cargo-binstall` metadata](../crates/udpstp/Cargo.toml) consume these names, layouts and checksum format.

The software bill of materials (SBOM) includes Rust dependencies for all targets, including conditional Windows and macOS dependencies. It is a union, so a particular binary does not use every listed dependency. It does not inventory the container's Debian runtime or provide separate inventories per platform. Provenance and dependency inventory attestations answer different questions.

The workflow and [toolchain file](../rust-toolchain.toml) record build tool versions. Hosted runner images and bundled compilers can change, so those pins do not establish bit-for-bit reproducibility.

## Arch Linux packages

The Release workflow renders the [binary recipe](../contrib/aur/udpstp-bin/PKGBUILD)
with archive checksums and the systemd unit from the tagged source, then builds it
with `makepkg` in the official Arch Linux image. It preserves the published
executable bytes and checks installation, file integrity and removal.

Before publication, the existing qualification workflow downloads the package,
checks its checksum and provenance, and exercises the installed binary with
authenticated Tests. Removal preserves existing configuration. The package joins
the other native packages in build provenance and binary inventory attestations.

The release package targets Arch Linux's x86_64 platform. The recipe retains its
aarch64 inputs for Arch Linux ARM, which needs separate qualification before a
package is advertised. Publishing a native package does not require an AUR
account. AUR catalog submission remains separate.

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

New container releases publish only `ghcr.io/pederbe/udpstp:latest`. Pre-releases still run qualification, without publishing an image. A stable release is pushed by digest and attested before `:latest` advances. The workflow checks that the release is still GitHub's latest release before moving the tag. Previously published version tags remain available.

For a published container, verify the immutable image digest rather than a mutable tag:

```sh
gh attestation verify oci://ghcr.io/pederbe/udpstp@sha256:IMAGE_DIGEST \
    --repo pederbe/udpstp \
    --signer-workflow pederbe/udpstp/.github/workflows/release-container.yml \
    --source-ref refs/tags/v0.1.0 --source-digest FULL_SOURCE_COMMIT
```

The [signed apt and dnf repository](../contrib/package-repository/README.md) distributes the release packages without recompiling them. Its current indexes list only the latest release. Older package files, apt By-Hash indexes and signed dnf snapshots stay available for downloads already in progress. Upgrade tests install an older package file directly, then upgrade through the current index.

A manual package-repository run always selects GitHub's latest release. A delayed release event for an older version is refused. The workflow checks the latest release again before publishing, so an old retry cannot replace the current index. This replaces the manual `tag` input. [ADR 0017](adr/0017-latest-only-distribution.md) records the distribution policy.

Homebrew uses the release archives. Cargo, cargo-binstall and other package-manager sources are described in the [installation guide](guides/installing-udpstp.md). Catalog admission and availability are specific to each channel.
