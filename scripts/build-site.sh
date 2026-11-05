#!/bin/sh
# Build udpstp.com locally into target/site/public: the landing page and the
# mdBook docs, with diagrams, icons and the link-preview image rendered at
# build time. See docs/website.md for sources, previewing and build checks.
#
# usage: scripts/build-site.sh [--dev] [--audit]   (Linux or WSL)
#
#   --dev    a local preview: build from a tree with uncommitted changes or
#            without the release tag, pinning links to the commit, and drop
#            links to untracked files with a warning (UDPSTP_SITE_DEV=1)
#   --audit  also run `npm audit` on the build tools (it asks the npm
#            registry, so it is left out by default)
#
# Needs: cargo; mdBook 0.5.4 (`cargo install --locked mdbook@0.5.4`); Node.js
# 22.13 or later with npm. The first run installs the pinned build tools in
# website/node_modules (`npm ci --ignore-scripts`) and Puppeteer's Chrome
# (`npx puppeteer browsers install chrome`, into ~/.cache/puppeteer; pinned
# by version, not checked against a hash).
#
# Without --dev, the build needs the tag v<version> on the checked-out commit
# and a clean tree, or UDPSTP_SITE_REF naming the ref that links to
# repository files pin.
#
# Environment:
#   UDPSTP_SITE_REF  the Git ref that links to repository files pin
#   CARGO_TARGET_DIR respected for the binary and xtask builds
set -eu

audit=false
for arg in "$@"; do
  case $arg in
    --dev) UDPSTP_SITE_DEV=1; export UDPSTP_SITE_DEV ;;
    --audit) audit=true ;;
    *) echo "usage: scripts/build-site.sh [--dev] [--audit]" >&2; exit 2 ;;
  esac
done

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
out=target/site
mdbook_version=0.5.4

if ! command -v mdbook >/dev/null 2>&1 || [ "$(mdbook --version)" != "mdbook v$mdbook_version" ]; then
  echo "error: needs mdbook $mdbook_version: cargo install --locked mdbook@$mdbook_version" >&2
  exit 1
fi
# The pinned Mermaid CLI requires Node.js 22.13.
if ! node -e 'const [a, b] = process.versions.node.split(".").map(Number); process.exit(a > 22 || (a === 22 && b >= 13) ? 0 : 1)'; then
  echo "error: needs Node.js 22.13 or later, found $(node --version 2>/dev/null || echo none)" >&2
  exit 1
fi

echo "== build tools (website/package-lock.json)"
(cd website && npm ci --ignore-scripts --no-audit --no-fund --silent)
if $audit; then
  (cd website && npm audit --audit-level=low)
fi
(cd website && npx --no-install puppeteer browsers install chrome >/dev/null)

echo "== udpstp binary, for the generated reference pages"
cargo build --release --locked -p udpstp
target_dir=${CARGO_TARGET_DIR:-$root/target}
UDPSTP_BIN="$target_dir/release/udpstp"
export UDPSTP_BIN

echo "== icons and link-preview image"
node website/render.mjs "$out/rendered"

echo "== book sources and landing page"
cargo run --quiet --locked -p xtask -- site prepare

echo "== mdBook"
rm -rf "$out/public"
mdbook build "$out/book" --dest-dir "$root/$out/public"

echo "== finish and check"
cargo run --quiet --locked -p xtask -- site finish

echo "Built $out/public. Preview it with:"
echo "  python3 -m http.server --bind 127.0.0.1 --directory $out/public 8000"
