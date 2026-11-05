#!/bin/sh
# Tests install/install.sh against a fake release served over HTTP
# from 127.0.0.1: a stub udpstp that prints a version, in the Release
# workflow's archive layout and naming (.github/workflows/release.yml), with
# its SHA256SUMS. Checks a good install (by --to, UDPSTP_INSTALL_DIR, the
# default ~/.local/bin, and piped into sh), a tampered archive refused, a
# missing release, unknown platforms, and a missing downloader. Nothing
# leaves the machine, and nothing is installed outside a temporary
# directory. Linux without a usable glibc (musl, or another architecture's
# loader missing, faked here through uname) gets the static musl build.
#
# Needs python3 (for its http.server), curl or wget, tar, and sha256sum or
# shasum. Runs on Linux and macOS; install-test.ps1 is its Windows twin.
#
# Usage: scripts/install-test.sh
set -eu

cd "$(dirname "$0")/.."
installer=$(pwd)/install/install.sh
tag=v0.0.0-test
work=$(mktemp -d)
server=
cleanup() {
    if [ -n "$server" ]; then
        kill "$server" 2>/dev/null || true
    fi
    rm -rf "$work"
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM

if command -v sha256sum >/dev/null 2>&1; then
    sums() { sha256sum "$@"; }
else
    sums() { shasum -a 256 "$@"; }
fi
if command -v curl >/dev/null 2>&1; then
    get() { curl -fsS "$1"; }
else
    get() { wget -q -O - "$1"; }
fi

# The fake release, as the Release workflow lays it out.
release=$work/release
mkdir -p "$release"
for target in x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu x86_64-unknown-linux-musl aarch64-unknown-linux-musl armv7-unknown-linux-musleabihf x86_64-apple-darwin aarch64-apple-darwin; do
    name=udpstp-$tag-$target
    mkdir "$work/$name"
    printf '#!/bin/sh\necho "udpstp 0.0.0-test"\n' >"$work/$name/udpstp"
    chmod 755 "$work/$name/udpstp"
    cp README.md LICENSE-MIT LICENSE-APACHE "$work/$name/"
    (cd "$work" && tar czf "$release/$name.tar.gz" "$name")
done
printf 'not a zip\n' >"$release/udpstp-$tag-x86_64-pc-windows-msvc.zip"
printf 'not an exe\n' >"$release/udpstp.exe"
printf '{}\n' >"$release/udpstp-$tag.cdx.json"
# The packages, which SHA256SUMS lists too: none may be taken for
# an archive.
for package in udpstp_0.0.0-1_amd64.deb udpstp_0.0.0-1_arm64.deb udpstp-0.0.0-1.x86_64.rpm udpstp-0.0.0-1.aarch64.rpm; do
    printf 'not a package\n' >"$release/$package"
done
(cd "$release" && sums udpstp-* udpstp_* udpstp.exe >SHA256SUMS)

# Served as GitHub serves releases: /download/<tag>/<file>, and
# /latest/download/<file> for the latest.
site=$work/site
for variant in good tampered; do
    mkdir -p "$site/$variant/download/$tag" "$site/$variant/latest/download"
    cp "$release"/* "$site/$variant/download/$tag/"
    cp "$release"/* "$site/$variant/latest/download/"
done
for archive in "$site"/tampered/download/"$tag"/*.tar.gz "$site"/tampered/latest/download/*.tar.gz; do
    printf 'x' >>"$archive"
done
cp "$installer" "$site/install.sh"

port=$(python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')
python3 -m http.server --bind 127.0.0.1 --directory "$site" "$port" >"$work/server.log" 2>&1 &
server=$!
url=http://127.0.0.1:$port
tries=0
until get "$url/install.sh" >/dev/null 2>&1; do
    tries=$((tries + 1))
    if [ "$tries" -gt 50 ]; then
        cat "$work/server.log"
        echo "install-test: the HTTP server didn't start"
        exit 1
    fi
    sleep 0.1
done
good=$url/good
tampered=$url/tampered

# PATH directories: a fake uname, and the installer's tools without a
# downloader, or with wget alone.
link() {
    for tool in "$@"; do
        found=$(command -v "$tool" 2>/dev/null || true)
        case $found in
            /*) ln -s "$found" "$bin/$tool" ;;
        esac
    done
}
fake=$work/fake-uname
mkdir "$fake"
# shellcheck disable=SC2016 # The fake uname expands them when it runs.
printf '#!/bin/sh\nif [ "${1:-}" = -m ]; then echo "$FAKE_ARCH"; else echo "$FAKE_OS"; fi\n' >"$fake/uname"
chmod 755 "$fake/uname"
bin=$work/no-downloader
mkdir "$bin"
link uname getconf sysctl
bin=$work/wget-only
mkdir "$bin"
link uname getconf sysctl tar gzip mktemp sha256sum shasum perl awk rm cp chmod mv mkdir cat grep wget

passed=0
failed=0
out=$work/out
pass() {
    echo "ok: $1"
    passed=$((passed + 1))
}
fail() {
    echo "FAIL: $1"
    sed 's/^/    /' "$out"
    failed=$((failed + 1))
}
# expect DESCRIPTION ok|fails TEXT COMMAND...: the command succeeds or
# fails as expected, and its output contains TEXT.
expect() {
    desc=$1
    want=$2
    text=$3
    shift 3
    if "$@" >"$out" 2>&1; then got=ok; else got=fails; fi
    if [ "$got" = "$want" ] && grep -qF -- "$text" "$out"; then
        pass "$desc"
    else
        echo "    (expected the command to be $want with \"$text\" in its output)" >>"$out"
        fail "$desc"
    fi
}
# contains DESCRIPTION TEXT: the last command's output contains TEXT.
contains() {
    if grep -qF -- "$2" "$out"; then pass "$1"; else fail "$1"; fi
}
# absent DESCRIPTION TEXT: the last command's output doesn't contain TEXT.
absent() {
    if grep -qF -- "$2" "$out"; then fail "$1"; else pass "$1"; fi
}
# installed DESCRIPTION PATH: PATH is the stub, and runs.
installed() {
    if [ -x "$2" ] && [ "$("$2" --version)" = "udpstp 0.0.0-test" ]; then
        pass "$1"
    else
        echo "    ($2 isn't the stub)" >>"$out"
        fail "$1"
    fi
}
# nothing DESCRIPTION DIR: nothing was installed into DIR.
nothing() {
    if [ -e "$2/udpstp" ]; then
        echo "    ($2/udpstp exists)" >>"$out"
        fail "$1"
    else
        pass "$1"
    fi
}

expect "--help prints the usage" ok "Usage: install.sh" sh "$installer" --help

# Unknown platforms are refused before anything is downloaded.
expect "an unknown OS is refused" fails "there is no udpstp release for SunOS" \
    env PATH="$fake:$PATH" FAKE_OS=SunOS FAKE_ARCH=sun4v UDPSTP_INSTALL_BASE_URL="$good" \
    sh "$installer" --to "$work/unknown-os"
nothing "nothing is installed for an unknown OS" "$work/unknown-os"
expect "an unknown architecture is refused" fails "there is no udpstp release for Linux on riscv64" \
    env PATH="$fake:$PATH" FAKE_OS=Linux FAKE_ARCH=riscv64 UDPSTP_INSTALL_BASE_URL="$good" \
    sh "$installer" --to "$work/unknown-arch"
nothing "nothing is installed for an unknown architecture" "$work/unknown-arch"
expect "Windows is sent to install.ps1" fails "irm https://udpstp.com/install.ps1 | iex" \
    env PATH="$fake:$PATH" FAKE_OS=MINGW64_NT-10.0 FAKE_ARCH=x86_64 UDPSTP_INSTALL_BASE_URL="$good" \
    sh "$installer" --to "$work/windows"

# Linux without a usable glibc gets the static musl build: armv7 always,
# and an architecture whose glibc loader this host lacks.
glibc=true
if [ "$(uname -s)" = Linux ]; then
    case $(uname -m) in
        x86_64 | amd64) loader=/lib64/ld-linux-x86-64.so.2 other=aarch64 other_loader=/lib/ld-linux-aarch64.so.1 ;;
        *) loader=/lib/ld-linux-aarch64.so.1 other=x86_64 other_loader=/lib64/ld-linux-x86-64.so.2 ;;
    esac
    [ -e "$loader" ] || glibc=false
    if [ -z "$(getconf GNU_LIBC_VERSION 2>/dev/null || true)" ]; then
        for f in /lib/ld-musl-*; do
            if [ -e "$f" ]; then glibc=false; fi
        done
    fi
    expect "armv7 Linux gets the static musl build" ok "for armv7-unknown-linux-musleabihf" \
        env PATH="$fake:$PATH" FAKE_OS=Linux FAKE_ARCH=armv7l UDPSTP_INSTALL_BASE_URL="$good" \
        sh "$installer" --to "$work/armv7"
    installed "the armv7 install installs the stub" "$work/armv7/udpstp"
    if [ -e "$other_loader" ]; then
        echo "skip: this host has the $other glibc loader too"
    else
    expect "a missing glibc loader gets the static musl build" ok "for $other-unknown-linux-musl" \
        env PATH="$fake:$PATH" FAKE_OS=Linux FAKE_ARCH=$other UDPSTP_INSTALL_BASE_URL="$good" \
        sh "$installer" --to "$work/no-loader"
    installed "the musl install installs the stub" "$work/no-loader/udpstp"
    fi
fi
if ! $glibc; then
    expect "this system without glibc gets the static musl build" ok "unknown-linux-musl" \
        env UDPSTP_INSTALL_BASE_URL="$good" sh "$installer" --to "$work/musl"
    installed "the musl install installs the stub" "$work/musl/udpstp"
    echo "install-test: no glibc here, so the glibc download cases are skipped"
else
    # Exercise the normal release path without the testing override.
    # The downloader serves only our local fixture; gh pretends to be logged
    # in but fails verification, and must never be invoked by the installer.
    mkdir "$work/release-tools"
    cat >"$work/release-tools/curl" <<'EOF'
#!/bin/sh
set -eu
output=
url=
while [ "$#" -gt 0 ]; do
    case $1 in
        -o) output=$2; shift ;;
        https://*) url=$1 ;;
    esac
    shift
done
case $url in
    https://github.com/pederbe/udpstp/releases/*)
        cp "$UDPSTP_TEST_RELEASE/${url#https://github.com/pederbe/udpstp/releases/}" "$output"
        ;;
    *) exit 1 ;;
esac
EOF
    cat >"$work/release-tools/gh" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >>"$UDPSTP_TEST_GH_LOG"
[ "$*" = 'auth status' ]
EOF
    chmod +x "$work/release-tools/curl" "$work/release-tools/gh"
    expect "a release installs without invoking gh" ok "Installed udpstp 0.0.0-test" \
        env UDPSTP_INSTALL_BASE_URL= UDPSTP_TEST_RELEASE="$site/good" \
        UDPSTP_TEST_GH_LOG="$work/gh.log" PATH="$work/release-tools:$PATH" \
        sh "$installer" --to "$work/no-gh"
    installed "the normal release path installs the stub" "$work/no-gh/udpstp"
    contains "the normal release path verifies the checksum" "Checksum verified"
    if [ ! -e "$work/gh.log" ]; then pass "gh is never invoked"; else fail "gh is never invoked"; fi

    expect "the latest release installs with --to" ok "Installed udpstp 0.0.0-test to $work/to/udpstp" \
        env UDPSTP_INSTALL_BASE_URL="$good" sh "$installer" --to "$work/to"
    installed "--to installs the stub" "$work/to/udpstp"
    contains "the checksum is verified" "Checksum verified"
    contains "a directory off the PATH is warned about" "isn't on your PATH"
    contains "the next step is printed" "Next: run '$work/to/udpstp --help'"

    expect "a release installs by --version, into UDPSTP_INSTALL_DIR" ok "Installed udpstp 0.0.0-test to $work/dir/udpstp" \
        env UDPSTP_INSTALL_BASE_URL="$good" UDPSTP_INSTALL_DIR="$work/dir" PATH="$work/dir:$PATH" \
        sh "$installer" --version "${tag#v}"
    installed "UDPSTP_INSTALL_DIR installs the stub" "$work/dir/udpstp"
    absent "a directory on the PATH isn't warned about" "isn't on your PATH"

    mkdir "$work/home"
    expect "the default directory is ~/.local/bin" ok "to $work/home/.local/bin/udpstp" \
        env UDPSTP_INSTALL_BASE_URL="$good" HOME="$work/home" sh "$installer"
    installed "the default directory gets the stub" "$work/home/.local/bin/udpstp"

    expect "piped into sh, as the site's command runs it" ok "Installed udpstp 0.0.0-test" \
        env UDPSTP_INSTALL_BASE_URL="$good" sh -c "$(if command -v curl >/dev/null 2>&1; then echo curl -fsSL; else echo wget -qO-; fi) '$url/install.sh' | sh -s -- --to '$work/piped'"
    installed "the piped install installs the stub" "$work/piped/udpstp"

    if [ -e "$work/wget-only/wget" ]; then
        expect "wget downloads when curl is missing" ok "Installed udpstp 0.0.0-test" \
            env UDPSTP_INSTALL_BASE_URL="$good" PATH="$work/wget-only" "$(command -v sh)" "$installer" --to "$work/wget"
        installed "the wget install installs the stub" "$work/wget/udpstp"
    else
        echo "skip: wget isn't installed"
    fi

    expect "a tampered archive is refused" fails "checksum mismatch" \
        env UDPSTP_INSTALL_BASE_URL="$tampered" sh "$installer" --to "$work/tampered"
    nothing "nothing is installed from a tampered archive" "$work/tampered"
    cp "$work/to/udpstp" "$work/keep"
    expect "a tampered archive leaves an installed udpstp alone" fails "checksum mismatch" \
        env UDPSTP_INSTALL_BASE_URL="$tampered" sh "$installer" --to "$work/to"
    if cmp -s "$work/to/udpstp" "$work/keep"; then pass "the installed udpstp is unchanged"; else fail "the installed udpstp is unchanged"; fi

    expect "a missing release is refused" fails "there is no such release" \
        env UDPSTP_INSTALL_BASE_URL="$good" sh "$installer" --version v9.9.9 --to "$work/missing"
    nothing "nothing is installed for a missing release" "$work/missing"

    expect "a missing downloader is refused" fails "needs curl or wget" \
        env UDPSTP_INSTALL_BASE_URL="$good" PATH="$work/no-downloader" "$(command -v sh)" "$installer" --to "$work/no-downloader-dir"
fi

expect "an unknown option is refused" fails "unknown option --frobnicate" sh "$installer" --frobnicate

echo "install-test: $passed passed, $failed failed"
[ "$failed" -eq 0 ]
