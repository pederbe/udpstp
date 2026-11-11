#!/bin/sh
# Render the binary PKGBUILD from qualified release inputs. The systemd
# unit must come from the same source commit as the release archives.
# Usage: sh contrib/aur/render-binary.sh VERSION SHA256SUMS UNIT > PKGBUILD
set -eu
if [ "$#" -ne 3 ]; then
    echo 'usage: render-binary.sh VERSION SHA256SUMS UNIT' >&2
    exit 1
fi
version=$1 sums=$2 unit=$3
case $version in
    *[!0-9.]* | '' | .* | *.) echo "not a release version: $version" >&2; exit 1 ;;
esac
unit_hash=$(sha256sum "$unit")
unit_hash=${unit_hash%% *}
awk -v version="$version" -v sums="$sums" -v unit_hash="$unit_hash" '
    BEGIN {
        while ((getline line < sums) > 0) {
            split(line, field, /[ *]+/)
            hash[field[2]] = field[1]
        }
        for (i = 1; i <= 2; i++) {
            arch = i == 1 ? "x86_64" : "aarch64"
            archive = "udpstp-v" version "-" arch "-unknown-linux-gnu.tar.gz"
            if (!(archive in hash)) {
                print "SHA256SUMS has no " archive > "/dev/stderr"
                failed = 1
            }
            archive_hash[arch] = hash[archive]
        }
        if (failed) exit 1
    }
    /^pkgver=/ { print "pkgver=" version; next }
    /^sha256sums=/ { print "sha256sums=(\047" unit_hash "\047)"; next }
    /^sha256sums_x86_64=/ {
        print "sha256sums_x86_64=(\047" archive_hash["x86_64"] "\047)"; next
    }
    /^sha256sums_aarch64=/ {
        print "sha256sums_aarch64=(\047" archive_hash["aarch64"] "\047)"; next
    }
    { print }
' "$(dirname "$0")/udpstp-bin/PKGBUILD"
