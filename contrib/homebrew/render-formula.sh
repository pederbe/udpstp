#!/bin/sh
# Render the Homebrew formula for a release: the template's version and the
# sha256 that follows each archive URL, taken from the release's SHA256SUMS.
#
# usage: sh contrib/homebrew/render-formula.sh VERSION SHA256SUMS > udpstp.rb
#
# VERSION has no leading v. Fails if any archive the formula names is missing
# from SHA256SUMS, so a release can't ship a formula with a placeholder hash.
set -eu
version=$1 sums=$2
template=$(dirname "$0")/udpstp.rb
case $version in
    *[!0-9.]* | '' | .* | *.) echo "not a release version: $version" >&2; exit 1 ;;
esac
awk -v version="$version" -v sums="$sums" '
    BEGIN {
        while ((getline line < sums) > 0) {
            split(line, field, /[ *]+/)
            hash[field[2]] = field[1]
        }
    }
    /^  version "/ { print "  version \"" version "\""; next }
    /^ *url "#\{base\}-/ {
        match($0, /-[a-z0-9_]+-[a-z0-9_]+-[a-z0-9_-]+\.tar\.gz/)
        archive = "udpstp-v" version substr($0, RSTART, RLENGTH)
        if (!(archive in hash)) { print "SHA256SUMS has no " archive > "/dev/stderr"; failed = 1; exit 1 }
        pending = hash[archive]
        print; next
    }
    /^ *sha256 "/ && pending != "" {
        sub(/"[0-9a-f]+"/, "\"" pending "\"")
        pending = ""
    }
    { print }
    END { if (failed) exit 1 }
' "$template"
