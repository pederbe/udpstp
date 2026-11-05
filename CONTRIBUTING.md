# Contributing to udpstp

Bug reports, interoperability reports, questions and documentation improvements are welcome. Before starting a substantial code change, open or comment on an issue so the work fits the project.

Report security problems through [private vulnerability reporting](https://github.com/pederbe/udpstp/security/advisories/new), following [SECURITY.md](SECURITY.md).

## Build and test

Install Rust through rustup. `rust-toolchain.toml` selects the toolchain and components.

```sh
git clone https://github.com/pederbe/udpstp
cd udpstp
cargo build --release --locked -p udpstp
cargo test --locked --workspace
cargo xtask req
```

Read the [glossary](GLOSSARY.md), [architecture](docs/architecture.md), [testing guide](docs/testing.md) and the [ADRs](docs/adr/) relevant to your change. The [specification](docs/spec.md) records the user stories.

The project started on the latest stable Rust and supports the last three stable releases from then on. The workspace's `rust-version` is the oldest supported version, which CI builds and tests. Raise it only when a newer release is needed and the oldest one leaves the last three, and describe the compatibility change.

Use the [validation workflow](docs/testing.md#validation-workflow) to select relevant checks and keep generated files current. Automated checks also validate pull requests.

## Make a change

Keep pull requests focused. Put protocol decisions in `udpstp-core` and I/O in `udpstp-runtime`. Use glossary terms. Add tests at the boundary where the changed behavior is observable. Tag tests proving an RFC Requirement with its ID, following the [conformance guide](docs/conformance/README.md).

Keep first-party Rust safe. ADR 0015 describes the approved, isolated Windows key permission adapter. Expanding that exception requires the maintainer's explicit approval. Use an ADR for a consequential decision that is hard to reverse.

When work completes a feature, update the README Status table and any documentation that still calls it planned in the same change.

## Pull requests

Open a pull request with a focused change. Fill in the template, describe the behavior and validation, and link the relevant public issue when one exists.

Include a concise Unreleased entry in [CHANGELOG.md](CHANGELOG.md) for user-visible work. Cover capabilities, fixes, installation, compatibility consequences and required upgrade actions. If none is needed, explain why in the pull request's Changelog section. Follow the [interface policy](docs/interfaces.md).

## Licence and conduct

udpstp is dual-licensed under MIT or Apache-2.0. Unless you explicitly state otherwise, any contribution you intentionally submit for inclusion in udpstp, as defined in the Apache-2.0 license, is dual-licensed as above, without any additional terms or conditions.

Everyone taking part follows the [code of conduct](CODE_OF_CONDUCT.md).
