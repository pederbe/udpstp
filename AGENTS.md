udpstp implements RFC 9946 (UDPSTP) in Rust. Read [CONTRIBUTING.md](CONTRIBUTING.md) for contributor setup and pull requests.

## Product scope

udpstp is one application crate in the root package. Keep its protocol, networking, commands and Windows permission adapter in ordinary internal modules. Development tools and fuzz harnesses are unpublished and are not application components. [ADR 0018](docs/adr/0018-one-application-crate.md) records this layout.

The RFCs and explicit maintainer instructions define requirements. Internal Rust types and visibility do not create a developer API or an external compatibility promise. Assess compatibility through the application's commands, files, output and protocol behavior. Write the project name as `udpstp`, using `UDPSTP` for the protocol and its exact wire identifiers.

## Sources of truth

In order of authority

1. The RFCs, `docs/rfc/rfc9946.txt` and `docs/rfc/rfc9097.txt`.
2. The specification, `docs/spec.md`, including its user stories.
3. ADRs in `docs/adr/`. Read those that touch the area you work in. Surface a contradiction before changing a settled decision.
4. The glossary, `GLOSSARY.md`. Use its terms in code, tests, events, CLI output, docs and issues. Raise a missing concept before inventing a name.

`docs/architecture.md` maps the application modules and the protocol/runtime boundary. `docs/testing.md` describes the test tiers and maintained validation commands.

## Conventions

- Protocol decisions live in `src/protocol`, which does no I/O and takes time as input. `src/runtime` moves bytes and reads clocks.
- A test proving an RFC requirement carries `#[udpstp_req::req("ID")]`, using the IDs in `docs/conformance/requirements/`. A test proving a specification, ADR or Conformance Record names it in a comment.
- Record a departure from the RFC, an ambiguous requirement or a difference from OB-UDPST in `docs/conformance/records/`. Code names the record with `(conformance: <id>)`. Follow `docs/conformance/README.md`.
- Keep first-party Rust safe. The application denies unsafe code, its ordinary modules forbid it, and only `src/windows_permissions/native.rs` allows ADR 0015's approved native calls. Expanding that exception requires the maintainer's explicit approval.
- Use [the testing guide](docs/testing.md#validation-workflow) for test tiers, maintained commands and generated-file checks.

## Contributing

Keep changes focused, explain their behavior and validation, and follow [CONTRIBUTING.md](CONTRIBUTING.md).

Third-party sources under `/reference/` are ignored and never committed. CI's vendored sources are listed in `docs/provenance.md`.

## Keeping docs current

When work completes a feature, update the README Status table and any page that still describes it as planned in the same change. Mark a feature Done only when a user can run it.

Release-plz generates dated changelog entries from conventional pull request titles. Keep `## [Unreleased]` empty. Describe compatibility consequences and required upgrade actions in the pull request description, using a `BREAKING CHANGE:` paragraph when needed. Follow [CONTRIBUTING.md](CONTRIBUTING.md) for title conventions and upgrade instructions.

## Project guides

- Reporting issues, [issue tracker](docs/agents/issue-tracker.md).
- Triage labels, [label guide](docs/agents/triage-labels.md).
- One root glossary and ADR directory, [domain guide](docs/agents/domain.md).
