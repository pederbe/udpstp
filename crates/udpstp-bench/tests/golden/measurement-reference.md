# Measurement reference fixtures

These fixtures were captured with CPython 3.14.7 on Linux before removing the
measurement scripts. Their source is commit
`75e66024e7f228146cab8e00cb2c56bb4713df10`.

- `scripts/server-scale.py` SHA-256 is
  `09b2a2af29a08e463cc889f03f964d23f46edf1207d80f1b6127771b26b283d7`.
- `scripts/precision.py` SHA-256 is
  `39c2aef668061a013f5a562494865bda9362ccd15322177a8c8585a2a90b5126`.
- The retired first-Round capture helper SHA-256 is
  `3e70aa32d94bb6a724b9a8bcca1a64a8ebf95a41ec5e45672cdd6136b9635d3e`.

`campaign-recorded.json` contains Markdown and `summary.json` from the same fixed
campaign files the Rust process test reads, including CPU/thread counters,
missing Client results, suppressed events and a packet capture.
`campaign-empty.json` is the reference report with no measurements.

`precision-recorded.json` contains the original judging outputs and formatted
JSON for Linux and Windows contexts, exact rate/lag boundaries, missing evidence,
above-1-Gbps observations and the kernel-count mode. The context fixtures are
judged by the Linux reference interpreter and are not native Windows timing
measurements.

`bench-rounds/tests/golden/sampler-recorded.json` stores the source proc trees
and byte-identical reference records. Its input adapter redirects the original
sampler's file reads and fixes the clock. It covers several processes, names with
parentheses, a vanished thread, namespace counters and a missing process.

The temporary export adapters were removed after capturing these independent
expectations. Do not regenerate expectations from the implementation being
checked. Orchestration fixtures map the retired command names, executable
modes and the final sweep's Rust program name. Public preparation also anonymized
host and account labels, replaced private issue and decision wording with
descriptive text, and removed matrix comments. Matrix identity hashes reflect
those comment-free inputs. Measurement inputs, numeric results, settings,
schedules and judgments were not regenerated or changed. Tool source hashes
change at the migration and are checked separately from measurement values.
Timeout text applies the original 400-character cap to the recorded full command
after its name is mapped, including copies embedded in reports and JSON files.
