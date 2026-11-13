# Contributing to udpstp

Bug reports, interoperability reports, questions and documentation improvements are welcome. Before starting a substantial code change, open or comment on an issue so the work fits the project.

Report security problems through [private vulnerability reporting](https://github.com/pederbe/udpstp/security/advisories/new), following [SECURITY.md](SECURITY.md).

## Build and test

Install Rust through rustup. `rust-toolchain.toml` selects the toolchain and components.
On Linux, install the `git-cliff` version pinned in the
[release workflow](.github/workflows/release-plz.yml) before running workspace tests.
The release-note tests use local Git fixtures without contacting GitHub.

```sh
git clone https://github.com/pederbe/udpstp
cd udpstp
cargo build --release --locked -p udpstp
cargo test --locked --workspace
cargo test --locked -p udpstp --features fault-injection --test faults
cargo xtask req
```

Read the [glossary](GLOSSARY.md), [architecture](docs/architecture.md) and [testing guide](docs/testing.md) relevant to your change. [ARCHITECTURE.md](ARCHITECTURE.md) is the short map. The [specification](docs/spec.md) records the user stories, and the [changelog](CHANGELOG.md) lists what changed in each release.

[Design decisions](https://github.com/pederbe/udpstp/tree/main/docs/adr)
explain the reasoning behind important technical choices.

The project supports the last three stable Rust releases. The workspace's `rust-version` is the oldest supported version, which CI builds and tests. Raise it only when a newer release is needed and the oldest one leaves the last three, and describe the compatibility change.

Use the [validation workflow](docs/testing.md#validation-workflow) to select relevant checks and keep generated files current. Automated checks also validate pull requests.

## Make a change

Keep pull requests focused. Put protocol decisions in `src/protocol` and I/O in `src/runtime`. Use glossary terms. Add tests at the boundary where the changed behavior is observable. Tag tests proving an RFC Requirement with its ID, following the [conformance guide](docs/conformance/README.md).

udpstp has one application crate. Its modules implement the application together and do not establish separate developer APIs. [ADR 0018](docs/adr/0018-one-application-crate.md) records this scope. Test helpers, benchmarks and maintenance commands are unpublished tooling.

Keep first-party Rust safe. ADR 0015 describes the approved, isolated Windows key permission adapter. Expanding that exception requires the maintainer's explicit approval. Use an ADR for a consequential decision that is hard to reverse.

When work completes a feature, update the README Status table and any documentation that still calls it planned in the same change.

## Pull requests

Open a pull request with a focused change. Fill in the template, describe the behavior and validation, and link the relevant public issue when one exists.

Write a conventional pull request title describing the change for users, such as `fix: Read key files with Windows line endings` or `feat: Add JSON results for Tests`. Squash merges turn these titles into the [changelog](CHANGELOG.md). Use `fix(deps):` for dependency fixes and `ci:`, `build:`, `chore:`, `test:` or `refactor:` for internal work. Mark breaking changes with `!`, such as `feat!: Rename a command-line option`, and explain required upgrade actions in the description. Follow the [interface policy](docs/interfaces.md).

For interface deprecations, use `feat(deprecate):`. For removals, use `feat(remove)!:`. These produce the Deprecated and Removed sections required by the interface policy. Other breaking changes appear under Changed and carry a bold Breaking label.

Put compatibility effects and migration or upgrade steps in Solution. Release notes are generated from merged pull request titles. Do not add hand-written Unreleased entries or changelog footers. For release-tool changes, see the repository's [release guide](docs/releasing.md).

## Public issues and pull requests

Issue descriptions use **Problem**, **Desired outcome** and **Evidence**.
Describe the task and its impact, the result that would resolve it, and available
reproduction details or examples. Incomplete observations are welcome.
Pull requests use **Problem**, **Solution** and **Validation**. Explain the
reason for the change, its behavior and compatibility effects, and meaningful
checks. Put migration or upgrade instructions in Solution. Keep small changes
brief and links and examples within these sections.

Keep public discussion focused on software behavior, technical rationale and
evidence. Remove private data from reports, attribute checks accurately, and
preserve other people's observations and wording when updating a report.

## Licence and conduct

udpstp is dual-licensed under MIT or Apache-2.0. Unless you explicitly state otherwise, any contribution you intentionally submit for inclusion in udpstp, as defined in the Apache-2.0 license, is dual-licensed as above, without any additional terms or conditions.

Everyone taking part follows the [code of conduct](CODE_OF_CONDUCT.md).
