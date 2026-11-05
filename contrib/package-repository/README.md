# Signed apt and dnf repository

These scripts keep a signed apt repository (Debian, Ubuntu) and dnf repository
(Fedora, RHEL, AlmaLinux, Rocky Linux) at `packages.udpstp.com`, for x86_64
and aarch64. Once the channel is published, users who add it get new versions
through `apt upgrade` and `dnf upgrade`, and every published version stays
available. See the [installation guide](../../docs/guides/installing-udpstp.md)
for channel availability.

The [Package repository workflow](../../.github/workflows/package-repository.yml)
runs when a release is published. It downloads that release's packages, checks
them against its `SHA256SUMS` and their build attestations from `release.yml`,
and never rebuilds them. Then it:

1. syncs the current repository from R2, so every earlier version stays
   available, and checks every `.deb` already there against its release's
   `SHA256SUMS` (storage credentials alone cannot add a package);
2. runs [`package-repository.sh`](package-repository.sh), which publishes the
   `.deb` files unchanged, adds a header signature to each `.rpm` (checking
   that its header and payload digests stay the same), refuses any file that
   is not a udpstp package matching its name, and regenerates and signs the
   apt and dnf metadata;
3. runs [`test.sh`](test.sh), which installs the version from the generated
   repository in offline Debian 10 and 12, Ubuntu 24.04, AlmaLinux 8 and
   Fedora containers with signature checks on, runs it, and removes it. When
   the version is the newest, it installs the previous version first and
   upgrades;
4. runs [`publish.sh`](publish.sh) to upload packages, apt By-Hash indexes
   and complete dnf snapshots before advancing the signed apt index and dnf
   mirrorlist, then compares the published metadata with what it signed.

## Layout

```text
udpstp.asc, udpstp.gpg       signing key, armored and binary
udpstp.repo                  dnf repository definition
apt/dists/stable/            InRelease, Release, Release.gpg, main/binary-{amd64,arm64}/Packages
apt/dists/stable/main/binary-*/by-hash/   retained SHA256 and SHA512 indexes
apt/pool/main/u/udpstp/      every published .deb
rpm/                         every published .rpm, signed
rpm/snapshots/<sha256>/repodata/   permanent metadata, with repomd.xml.asc
rpm/mirrorlist               URL of the current dnf snapshot
```

Packages, By-Hash indexes and dnf snapshots are cached as immutable. The
current apt metadata, the dnf mirrorlist, the keys and `udpstp.repo` are served
with `Cache-Control: no-cache`. apt fetches the exact index named in its signed
`InRelease` (`Acquire-By-Hash`), and dnf reads one snapshot URL from the
mirrorlist, so an interrupted upload never mixes an index with another
generation's signature. Old indexes and snapshots remain available.

## Signing key

A dedicated RSA 4096 key signs the repository. RSA keeps RHEL 8 able to check
RPM signatures, and the key doesn't expire because apt users keep their own
copy. Its public half is [`udpstp.asc`](udpstp.asc), and the script refuses to
sign with any other key:

```text
7767 31EF C684 4744 0F38  B115 5958 1289 0249 EF6C
```

## Checking locally

`test.sh` needs only Podman or Docker. To exercise the whole path without a
release, build a repository from throwaway packages and a throwaway key in a
container with `apt-utils createrepo-c rpm gnupg` (Ubuntu 26.04 or later,
whose RPM 6 supports `rpmsign --rpmv4`), then run:

```sh
bash contrib/package-repository/test.sh REPOSITORY 0.1.1 0.1.0
```
