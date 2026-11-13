# Provenance of vendored files

Third-party files committed to this repository, where each came from and under which licence. Everything else third-party stays in the gitignored `reference/` folder.

Documents we may not redistribute are never committed: BBF TR-471, ITU-T Y.1540 and its amendments, and any other text whose licence doesn't allow it. Other reference material CI doesn't need (NIST SP 800-108, OB-UDPST's source and its netem test harness) stays in `reference/` too. The pinned OB-UDPST build the interop tests use is fetched from GitHub at test time by `scripts/obudpst-build.sh` into the gitignored `target/`, never committed ([testing.md](testing.md#the-pinned-ob-udpst-build)). Requirements quote only RFC text.

## RFC texts

| File | Source | Licence |
|---|---|---|
| `docs/rfc/rfc9946.txt` | <https://www.rfc-editor.org/rfc/rfc9946.txt> | IETF Trust Legal Provisions (RFCs may be reproduced in full, unmodified); Code Components extracted from them are under the Revised BSD licence (TLP section 4.e) |
| `docs/rfc/rfc9097.txt` | <https://www.rfc-editor.org/rfc/rfc9097.txt> | IETF Trust Legal Provisions (RFCs may be reproduced in full, unmodified); Code Components extracted from them are under the Revised BSD licence (TLP section 4.e) |

Both are byte-identical to the RFC Editor's files as fetched on 2026-09-24 (SHA-256 `e64af8dc…248c0` for RFC 9946, `1b198fd8…198287` for RFC 9097). `cargo xtask req` checks requirement texts against them.

## Derived test vectors

| File | Source | Licence |
|---|---|---|
| `src/tests/wire/vectors/hmac-rfc4231.txt` | Test cases 1-7 extracted from <https://www.rfc-editor.org/rfc/rfc4231.txt> by `cargo xtask kat` | IETF Trust Legal Provisions; the extracted test cases are Code Components under the Revised BSD licence (TLP section 4.e) |
| `src/tests/wire/vectors/obudpst-control.txt` | Control PDUs an OB-UDPST v9.1.0 Client and Server exchanged, captured by `cargo xtask capture`; the header records the build and commands | Output of OB-UDPST (BSD-3-Clause) run with our own test key; no OB-UDPST text |
| `src/tests/wire/vectors/kdf-openssl.txt` | Generated with OpenSSL's `kdf` command by `cargo xtask kat`; the header records the version and command | Our own output; no third-party text |

## OB-UDPST v9.1.0 fixtures

`testdata/obudpst-9.1.0/` holds files from OB-UDPST, copied unmodified from the Git objects of tag `v9.1.0`:

- Repository: <https://github.com/BroadbandForum/obudpst>
- Tag `v9.1.0`, commit `ceb8917d6b8cf1089ff488e19e06c2b52cfbff85`
- Licence: BSD-3-Clause, Copyright (c) 2020 Broadband Forum and AT&T Communications. The notice is kept in `testdata/obudpst-9.1.0/LICENSE`.

They are test fixtures: golden JSON output for the OB-UDPST-compatible JSON format, and a key file for loading OB-UDPST key files. No OB-UDPST source code is vendored.

| File (under `testdata/obudpst-9.1.0/`) | Upstream path | Git blob |
|---|---|---|
| `LICENSE` | `LICENSE` | `c053f5167c47479a21ef719547d6c6bd05c97103` |
| `udpst.keys` | `udpst.keys` | `196a8fc152132c2525d5b149d023fcef7bb2e4a1` |
| `json_examples/client_example_01.json` | same | `a740324ba75ecc54a43e7cfd0dfd2a5f41ccc491` |
| `json_examples/client_example_02.json` | same | `49ccb8f86d4448ed883afda5e659e09d26fd72f9` |
| `json_examples/client_example_03.json` | same | `f5f94d6419683012ea481ac0631f91e1553dde3f` |
| `json_examples/client_example_04.json` | same | `63430219ba25659c7671e88ff083c1b8fc7ee60e` |
| `json_examples/client_example_05.json` | same | `5704f2689a9a0745bb5dd681206221c038784f25` |
| `json_examples/client_example_06.json` | same | `7e10bb2d54c6de2021cf8c4edc4f16bc29950c17` |
| `json_examples/client_example_07.json` | same | `68252f824c0322e293c5d389e36ce03f39d9d73c` |
| `json_examples/client_example_08.json` | same | `f5f0f3097d3fbd58e587a19fec788e94971c2b1c` |
| `json_examples/client_example_09.json` | same | `a9a44d10a4b47f2e1d8b7828833def2ecc2224e0` |
| `json_examples/client_example_10.json` | same | `eb9823c95e2a5a7820945562b079a936b384e083` |
| `json_examples/client_example_11.json` | same | `766589b87df6ba35b52f1b0f5791a5a99fcf336c` |
| `json_examples/client_example_12.json` | same | `3c7695121521ab153b8c56cf2d47904489bd74bb` |
| `json_examples/client_json_examples.sh` | same | `6afe6aa4292cf2016d888688f6fbb4c3c04a11d9` |
| `json_examples/server_performance_stats.json` | same | `147751d81e5df1c6139a1d3f3a286fc3c43f3c31` |
| `json_examples/server_performance_stats.txt` | same | `8d0cc9a5946be8e545e20d610a2c9c2288fcc4ee` |

`client_json_examples.sh` is the upstream script that produced the client examples; it records the command line behind each one. A blob hash equals `git hash-object` of the committed file, so `git ls-tree -r HEAD testdata/obudpst-9.1.0` shows whether a file or its mode has drifted from upstream. `.gitattributes` marks vendored files `-text`, so Git never rewrites their line endings.

Tests reach the fixtures from the root package with `concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/obudpst-9.1.0/<file>")`, for `include_str!` or `std::fs`. The application package excludes test fixtures and suites from its published source, as recorded in the root `Cargo.toml`.

## Website fonts

The website's display face, `website/fonts/udpstp-display-bold.woff2`, is a subset of IBM Plex Sans Condensed Bold, made by `website/fonts/subset.sh` from the npm package `@ibm/plex-sans-condensed` 2.0.0 (`fonts/complete/woff2/IBMPlexSansCondensed-Bold.woff2`) with fonttools 4.60.1: Latin-1 and the punctuation the site uses. The font is under the SIL Open Font License 1.1 with the Reserved Font Name "Plex"; a subset is a Modified Version, so its family is renamed "udpstp Display". The licence is kept beside it in `website/fonts/OFL.txt`, copied unmodified from the package's `LICENSE.txt`.

| File | SHA-256 |
|---|---|
| `website/fonts/udpstp-display-bold.woff2` | `2e0a9fd5b187abcd83ff8a1f64f7f7d3994809b56b69ca38737e94e28ebb1d3f` |
| `website/fonts/OFL.txt` | `7e6b2818edbd8f6a01ae80641cc8f16a51080d08fb4e532be3a0b6f74adb07da` |

The social image also uses ASCII and nonbreaking-space subsets of DejaVu Sans,
DejaVu Sans Mono and DejaVu Sans Mono Bold from the upstream
[DejaVu 2.37 TTF release](https://github.com/dejavu-fonts/dejavu-fonts/releases/tag/version_2_37).
Its source archive has SHA-256
`fa9ca4d13871dd122f61258a80d01751d603b4d3ee14095d65453b4e846e17d7`.
`website/fonts/subset.sh` reproduces the WOFF2 subsets with fonttools 4.60.1,
preserving the source timestamps. Their licence is copied unmodified from
the archive. These fonts are build inputs for the social image, so its rendering
does not depend on fonts installed on the build host.

| File | SHA-256 |
|---|---|
| `website/fonts/udpstp-social-sans.woff2` | `e91fe0d2bbb3ba0b8e74c84f3a85f56b456cbe913717f67f628eea6f872d69da` |
| `website/fonts/udpstp-social-mono.woff2` | `8be14f8d4fb4401bacf0187b8af0a47e214a7b0fb0eb45960b23fb9f90661570` |
| `website/fonts/udpstp-social-mono-bold.woff2` | `4b9a411ce8960be818dab1144f561ae77fdece5c097aab877fbc82daa6ab823f` |
| `website/fonts/DejaVu-LICENSE.txt` | `7a083b136e64d064794c3419751e5c7dd10d2f64c108fe5ba161eae5e5958a93` |
