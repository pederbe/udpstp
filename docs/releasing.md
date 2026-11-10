# Release artifacts and verification

## WinGet delivery

After publishing qualified Windows assets, the release workflow dispatches
`winget.yml` for that tag. The WinGet workflow verifies the published executable,
generates its portable manifest, and checks validation, installation, authenticated
loopback operation and removal on Windows. With submission enabled it uses
Komac to submit the version, then completes and reads back the PR description.
Microsoft still controls catalog admission.

Run the WinGet workflow on `main` with a published tag to retry delivery. Submission
defaults to off for manual runs. Enabling it requires `WINGET_ENABLED=true` and
`WINGET_TOKEN` in the `distribution` environment. The token must support Komac
submission and editing its author's PR in `microsoft/winget-pkgs`.
A separate `WINGET_FORK_TOKEN` in the same environment synchronizes the submission
fork before submission. Limit it to that fork with Contents and Workflows write
permissions, keeping `WINGET_TOKEN` public-only. Preparation does not use the fork
credential. Synchronization does not force or overwrite a divergent fork.
Retries reuse a matching open PR and preserve manual description edits. The CLA
checkbox reflects Microsoft's official check and never signs an agreement.

The [Release workflow](../.github/workflows/release.yml) builds the archives, native packages and verification files described here. Distribution channels become available when their first release is published. See [Installing udpstp](guides/installing-udpstp.md) for availability and installation instructions.

## Release process

Every Monday at 07:37 UTC, [release-plz](https://release-plz.dev) opens or updates one release pull request when there are unreleased changes. It updates Cargo versions and writes a dated section in the root changelog from merged pull request titles. Internal changes do not trigger releases. The pull request merges automatically after the required CI checks pass. New commits on `main` refresh an open release pull request so its checks cover the current source.

After preparing each release pull request, the workflow refreshes `fuzz/Cargo.lock` on its branch to match the local crate versions. It commits the lockfile only when it changes, before enabling auto-merge. Existing registry dependency versions stay locked.

Merging the release pull request publishes changed crates to crates.io in dependency order and creates their tags. The five published crates share a version group so a feature or breaking change in a library also raises the binary's version appropriately. Unchanged crates may keep their previous versions. The binary uses `vVERSION`, while the libraries use `PACKAGE-vVERSION`. Only the binary creates a GitHub draft release, using its combined changelog section as the release notes. The Release workflow builds and attests the assets, attaches them to that draft, runs the existing downloaded-archive and native-package qualification, and publishes it after those checks pass. Publication starts the existing container and signed package repository workflows. No manual dispatch or per-release approval is needed.

Published notes are already on `main` before tagging, and stay under their dated version heading. Keep `## [Unreleased]` empty. Release-plz manages version numbers and dependency requirements, including the library crates required by `cargo install udpstp`. Test helpers, benchmarks, fuzzing and `xtask` are not released.

### One-time setup

The repository needs auto-merge enabled and its `CI` check required on `main`. Set squash merge defaults to the pull request title and body, preserving conventional titles and upgrade instructions. Set `RELEASE_PLZ_TOKEN` to a repository-scoped token with Contents and Pull requests write access. This lets release pull requests run CI and lets published releases start distribution workflows. Release-plz uses the built-in workflow token for tags and drafts, then explicitly dispatches the Release workflow because workflow-token events do not start other workflows.

Configure crates.io trusted publishing for `udpstp`, `udpstp-wire`, `udpstp-core`, `udpstp-runtime` and `udpstp-key-permissions`, with repository `pederbe/udpstp` and workflow `release-plz.yml`. The workflow uses short-lived OpenID Connect credentials, so no crates.io API token is stored in GitHub. See the [release-plz setup guide](https://release-plz.dev/docs/github/quickstart).

### Recovery

If preparation or a Cargo upload fails, fix the reported cause and rerun the original failed Release-plz job to finish still-unpublished packages. Check which versions were accepted before retrying.

Cargo publication, tag creation and GitHub draft creation are separate operations. If Cargo accepts a version but creating its tag or draft fails, rerunning release-plz does not restore the missing GitHub state. A green retry alone does not establish that the release finished. Recover using the exact source commit from the failed run, cross-checking each version uploaded by that run against its published crate's `.cargo_vcs_info.json`. For versions already accepted by Cargo, restore only missing version tags at that commit with `git tag` and `git push`. For an already-uploaded binary version, restore its missing draft with `gh release create --draft --verify-tag --notes-file`, using the dated changelog section from that same commit. Leave still-unpublished packages to the original job retry. Precreating their tags would cause release-plz to skip their Cargo uploads. Existing tags and drafts must match the intended release. Do not move tags or overwrite published releases.

After restoring missing items for accepted versions, rerun the original Release-plz job for any remaining unpublished packages. If the binary had already been uploaded before that retry, explicitly dispatch Release on its `vVERSION` tag. Otherwise the retry dispatches Release when it newly publishes the binary. Explicit dispatch is also the recovery if dispatching the binary build failed after successful tagging. Rerunning Release-plz alone will not repeat that dispatch for an already-published binary.

If packaging or qualification fails, the GitHub release remains a draft. Rerun the failed Release jobs. Asset uploads replace only files in the unpublished draft. Published releases are immutable.

The Release-plz workflow can also be dispatched on `main` to prepare a release before the next weekly run. A failure stops the affected delivery stage and is visible in GitHub Actions.

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
