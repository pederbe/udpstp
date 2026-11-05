#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."

# CHECK_BASE selects the revision to compare when choosing the check profile.
# Manual checks without a comparison base retain the full sequence.
timeout --kill-after=30 20m sh scripts/check-commit.sh . HEAD "${CHECK_BASE:-}"
