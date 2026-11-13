#!/bin/sh
# Generate notices for the release binary with cargo-about 0.9.3.
set -eu
target=${1:?usage: release-notices.sh TARGET}
cd "$(dirname "$0")/.."
mkdir -p target/packaging
output=target/packaging/THIRD-PARTY-LICENSES.md
cargo-about generate --locked --fail --target "$target" \
    --manifest-path Cargo.toml --config about.toml about.hbs > "$output"
sysroot=$(rustc --print sysroot)
case "$(uname -s)" in MINGW*|MSYS*) sysroot=$(cygpath -u "$sysroot");; esac
standard_library="$sysroot/share/doc/rust/COPYRIGHT-library.html"
grep -q 'Copyright notices for The Rust Standard Library' "$standard_library"
printf '\n## Rust standard library\n\n' >> "$output"
cat "$standard_library" >> "$output"
