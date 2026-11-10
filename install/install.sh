#!/bin/sh
# Installs udpstp from its GitHub release, on Linux and macOS.
#
#   curl -fsSL https://udpstp.com/install.sh | sh
#   curl -fsSL https://udpstp.com/install.sh | sh -s -- --to ~/bin
#
# What it does, in order (docs/guides/installing-udpstp.md, "Install script"):
#
#  1. Picks the release target for this OS and architecture. On Linux that
#     is the glibc build when glibc 2.28 or later is present, and the static
#     musl build otherwise.
#  2. Downloads SHA256SUMS, then the archive it names for the target, over
#     HTTPS from https://github.com/pederbe/udpstp/releases, from the latest
#     release.
#  3. Checks the archive against SHA256SUMS with sha256sum or shasum.
#  4. Copies the udpstp binary into ~/.local/bin, or the directory --to or
#     $UDPSTP_INSTALL_DIR names, without sudo, and runs it to print its
#     version. Nothing outside that directory changes: no PATH, no profile.
#
# Options:
#   --to <dir>   install into <dir> rather than ~/.local/bin;
#                $UDPSTP_INSTALL_DIR works too
#   -h, --help   print this help
#
# For testing only: UDPSTP_INSTALL_BASE_URL replaces
# https://github.com/pederbe/udpstp/releases, so that scripts/install-test.sh
# can serve a fake release over plain HTTP from 127.0.0.1. It also lifts the
# HTTPS-only rule. Never set it otherwise.
#
# The whole script is one function, called on its last line, so a download
# cut short runs nothing.

udpstp_install() {
    set -eu

    repo=pederbe/udpstp
    releases=https://github.com/$repo/releases
    from_source="build from source instead: cargo install --locked udpstp"

    say() {
        printf '%s\n' "$*"
    }
    warn() {
        printf 'udpstp-install: warning: %s\n' "$*" >&2
    }
    die() {
        printf 'udpstp-install: error: %s\n' "$*" >&2
        exit 1
    }
    usage() {
        cat <<EOF
Installs the latest release of udpstp from GitHub, on Linux and macOS.

Usage: install.sh [--to <dir>]
       curl -fsSL https://udpstp.com/install.sh | sh -s -- [options]

Options:
  --to <dir>   install into <dir> rather than ~/.local/bin;
               \$UDPSTP_INSTALL_DIR works too
  -h, --help   print this help

The archive is checked against the release's SHA256SUMS.
EOF
    }
    has() {
        command -v "$1" >/dev/null 2>&1
    }

    # The options.
    dir=${UDPSTP_INSTALL_DIR:-}
    while [ $# -gt 0 ]; do
        case $1 in
            --to)
                [ $# -ge 2 ] || die "--to needs a directory"
                dir=$2
                shift 2
                ;;
            --to=*)
                dir=${1#--to=}
                shift
                ;;
            -h | --help)
                usage
                return 0
                ;;
            *)
                die "unknown option $1 (see --help)"
                ;;
        esac
    done
    if [ -z "$dir" ]; then
        [ -n "${HOME:-}" ] || die "HOME isn't set; name a directory with --to"
        dir=$HOME/.local/bin
    fi
    case $dir in
        /*) ;;
        *) dir=$(pwd)/$dir ;;
    esac
    dir=${dir%/}

    base=$releases
    testing=false
    if [ -n "${UDPSTP_INSTALL_BASE_URL:-}" ]; then
        base=${UDPSTP_INSTALL_BASE_URL%/}
        testing=true
        warn "UDPSTP_INSTALL_BASE_URL is set, so the release comes from $base, over any protocol. It is for testing only."
    fi

    # The release target for this OS and architecture.
    os=$(uname -s)
    arch=$(uname -m)
    case $os in
        Linux | Darwin) ;;
        MINGW* | MSYS* | CYGWIN* | Windows_NT)
            die "on Windows, install udpstp from PowerShell instead: irm https://udpstp.com/install.ps1 | iex"
            ;;
        *)
            die "there is no udpstp release for $os: releases are built for Linux, macOS and Windows. You can $from_source"
            ;;
    esac
    case $os/$arch in
        */x86_64 | */amd64) arch=x86_64 ;;
        */aarch64 | */arm64) arch=aarch64 ;;
        Linux/armv7l | Linux/armv7) arch=armv7 ;;
        *)
            die "there is no udpstp release for $os on $arch: releases are built for x86_64 and aarch64 (arm64), and for armv7 on Linux. You can $from_source"
            ;;
    esac
    if [ "$os" = Darwin ]; then
        # An x86_64 shell under Rosetta on Apple silicon: take the native build.
        if [ "$arch" = x86_64 ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || true)" = 1 ]; then
            arch=aarch64
        fi
        target=$arch-apple-darwin
    else
        # The gnu builds need glibc 2.28 or later, through its dynamic
        # loader. Everywhere else (musl, an older glibc, armv7) the static
        # musl build runs.
        libc=gnu
        case $arch in
            x86_64) loader=/lib64/ld-linux-x86-64.so.2 ;;
            aarch64) loader=/lib/ld-linux-aarch64.so.1 ;;
            *) loader= ;;
        esac
        if [ -z "$loader" ] || [ ! -e "$loader" ]; then
            libc=musl
        fi
        glibc=$(getconf GNU_LIBC_VERSION 2>/dev/null || true)
        # A musl system with a glibc loader (gcompat) still gets the musl
        # build; a glibc system with a musl package installed doesn't.
        if [ -z "$glibc" ]; then
            for f in /lib/ld-musl-*; do
                if [ -e "$f" ]; then
                    libc=musl
                fi
            done
        fi
        case $glibc in
            "glibc "*)
                glibc=${glibc#glibc }
                major=${glibc%%.*}
                minor=${glibc#*.}
                minor=${minor%%.*}
                case $major$minor in
                    '' | *[!0-9]*) ;;
                    *)
                        if [ "$major" -lt 2 ] || { [ "$major" -eq 2 ] && [ "$minor" -lt 28 ]; }; then
                            libc=musl
                        fi
                        ;;
                esac
                ;;
        esac
        case $arch/$libc in
            armv7/*) target=armv7-unknown-linux-musleabihf ;;
            *) target=$arch-unknown-linux-$libc ;;
        esac
    fi

    # The tools it needs.
    if has curl; then
        fetcher=curl
    elif has wget; then
        fetcher=wget
    else
        die "downloading the release needs curl or wget; install one of them and run this again"
    fi
    if has sha256sum; then
        hasher="sha256sum"
    elif has shasum; then
        hasher="shasum -a 256"
    else
        die "checking the download needs sha256sum or shasum; install one of them (coreutils or perl) and run this again"
    fi
    for tool in tar mktemp; do
        has "$tool" || die "installing needs $tool; install it and run this again"
    done

    fetch() {
        if [ "$fetcher" = curl ]; then
            if $testing; then
                curl -fsSL -o "$2" "$1"
            else
                curl --proto '=https' --proto-redir '=https' --tlsv1.2 -fsSL -o "$2" "$1"
            fi
        else
            # BusyBox wget has no --https-only; the URLs it is given are
            # HTTPS, and GitHub redirects only to HTTPS.
            if ! $testing && wget --help 2>&1 | grep -q -- --https-only; then
                wget --https-only -q -O "$2" "$1"
            else
                wget -q -O "$2" "$1"
            fi
        fi
    }
    sha256() {
        $hasher "$1" | awk '{ print $1 }'
    }

    tmp=$(mktemp -d 2>/dev/null || mktemp -d -t udpstp-install) || die "couldn't create a temporary directory"
    trap 'rm -rf "$tmp"' EXIT
    trap 'exit 1' HUP INT TERM

    # SHA256SUMS names the archive, and with it the tag of the latest release.
    sums_url=$base/latest/download/SHA256SUMS
    say "Looking for the latest release of udpstp for $target"
    fetch "$sums_url" "$tmp/SHA256SUMS" ||
        die "couldn't download $sums_url: no release is published, or the network is down. The releases are listed at $releases"
    line=$(awk -v suffix="-$target.tar.gz" '
        { name = $2; sub(/^\*/, "", name) }
        index(name, "udpstp-v") == 1 && length(name) > length(suffix) &&
        substr(name, length(name) - length(suffix) + 1) == suffix { print $1, name; n++ }
        END { exit n != 1 }' "$tmp/SHA256SUMS") ||
        die "the latest release has no archive for $target in its SHA256SUMS ($sums_url)"
    expected=${line%% *}
    file=${line#* }
    case $expected in
        *[!0-9a-f]*) die "SHA256SUMS has a malformed checksum for $file" ;;
    esac
    [ ${#expected} -eq 64 ] || die "SHA256SUMS has a malformed checksum for $file"
    case $file in
        *[!A-Za-z0-9._+-]*) die "SHA256SUMS names a malformed archive, $file" ;;
    esac
    tag=${file#udpstp-}
    tag=${tag%-"$target".tar.gz}

    url=$base/download/$tag/$file
    say "Downloading $url"
    fetch "$url" "$tmp/$file" || die "couldn't download $url"
    actual=$(sha256 "$tmp/$file")
    if [ "$actual" != "$expected" ]; then
        die "checksum mismatch: $file has SHA-256 $actual, but the release's SHA256SUMS says $expected. The download was corrupted or tampered with, so nothing was installed. Try again; if it happens again, report it privately: https://github.com/$repo/security/advisories/new"
    fi
    say "Checksum verified: $file matches SHA256SUMS"

    (cd "$tmp" && tar xzf "$file") || die "couldn't unpack $file"
    binary=$tmp/${file%.tar.gz}/udpstp
    [ -f "$binary" ] || die "$file has no udpstp binary"

    mkdir -p "$dir" || die "couldn't create $dir; name another directory with --to"
    [ -w "$dir" ] || die "$dir isn't writable, and this script never uses sudo; name another directory with --to"
    staged=$dir/.udpstp-install.$$
    if ! { cp "$binary" "$staged" && chmod 755 "$staged" && mv -f "$staged" "$dir/udpstp"; }; then
        rm -f "$staged"
        die "couldn't install udpstp into $dir"
    fi
    installed=$("$dir/udpstp" --version 2>&1) ||
        die "installed $dir/udpstp, but it doesn't run: $installed"

    say ""
    say "Installed $installed to $dir/udpstp"
    command=udpstp
    case ":$PATH:" in
        *":$dir:"*)
            found=$(command -v udpstp 2>/dev/null || true)
            if [ -n "$found" ] && [ "$found" != "$dir/udpstp" ]; then
                warn "$found comes first on your PATH, so 'udpstp' runs that one, not $dir/udpstp"
            fi
            ;;
        *)
            warn "$dir isn't on your PATH. Add it in your shell's profile, for example: export PATH=\"$dir:\$PATH\""
            command=$dir/udpstp
            ;;
    esac
    say "Next: run '$command --help', and read https://udpstp.com/docs/getting-started.html to run your first Test."
}

udpstp_install "$@"
