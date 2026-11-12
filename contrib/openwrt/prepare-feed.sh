#!/bin/sh
# Prepare a local OpenWrt source feed from the latest published release.
set -eu
if [ "$#" -ne 1 ]; then
    echo 'usage: sh contrib/openwrt/prepare-feed.sh FEED_DIRECTORY' >&2
    exit 1
fi

here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
trap 'exit 1' HUP INT TERM

# Resolve the latest release once. Its version and checksum then stay together,
# even if another release becomes latest while the SDK is building.
curl --proto '=https' --tlsv1.2 -fsSL \
    https://github.com/pederbe/udpstp/releases/latest/download/SHA256SUMS \
    -o "$work/SHA256SUMS"
archive=$(awk '$2 ~ /^udpstp-v[0-9.]+-x86_64-unknown-linux-musl\.tar\.gz$/ { print $2 }' "$work/SHA256SUMS")
version=${archive#udpstp-v}
version=${version%-x86_64-unknown-linux-musl.tar.gz}
case $version in
    *[!0-9.]* | '' | .* | *.) echo 'cannot identify the latest release' >&2; exit 1 ;;
esac
hash=$(awk -v archive="$archive" '$2 == archive { print $1 }' "$work/SHA256SUMS")
case $hash in
    *[!0-9a-f]* | '') echo "SHA256SUMS has no valid checksum for $archive" >&2; exit 1 ;;
esac
[ "${#hash}" -eq 64 ] || { echo "invalid checksum for $archive" >&2; exit 1; }

mkdir -p "$1/udpstp"
sed -e "s/@VERSION@/$version/g" -e "s/@HASH@/$hash/g" "$here/Makefile.in" > "$work/Makefile"
cp "$work/Makefile" "$1/udpstp/Makefile"
printf 'Prepared udpstp %s in %s/udpstp\n' "$version" "$1"
