udpstp implements RFC 9946 (UDPSTP) in Rust. Read [CONTRIBUTING.md](CONTRIBUTING.md) for contributor setup and pull requests.

## Sources of truth

In order of authority

1. The RFCs, `docs/rfc/rfc9946.txt` and `docs/rfc/rfc9097.txt`.
2. The specification, `docs/spec.md`, including its user stories.
3. ADRs in `docs/adr/`. Read those that touch the area you work in. Surface a contradiction before changing a settled decision.
4. The glossary, `GLOSSARY.md`. Use its terms in code, tests, events, CLI output, docs and issues. Raise a missing concept before inventing a name.

`docs/architecture.md` maps the crates and the core/runtime boundary. `docs/testing.md` describes the test tiers and maintained validation commands.

## Conventions

- Protocol decisions live in `udpstp-core`, which does no I/O and takes time as input. `udpstp-runtime` moves bytes and reads clocks.
- A test proving an RFC requirement carries `#[udpstp_req::req("ID")]`, using the IDs in `docs/conformance/requirements/`. A test proving a specification, ADR or Conformance Record names it in a comment.
- Record a departure from the RFC, an ambiguous requirement or a difference from OB-UDPST in `docs/conformance/records/`. Code names the record with `(conformance: <id>)`. Follow `docs/conformance/README.md`.
- Keep first-party Rust safe. ADR 0015 describes the approved, isolated Windows key permission adapter. Expanding that exception requires the maintainer's explicit approval.
- Use [the testing guide](docs/testing.md#validation-workflow) for test tiers, maintained commands and generated-file checks.

## Contributing

Keep changes focused, explain their behavior and validation, and follow [CONTRIBUTING.md](CONTRIBUTING.md).

Third-party sources under `/reference/` are ignored and never committed. CI's vendored sources are listed in `docs/provenance.md`.

## Keeping docs current

When work completes a feature, update the README Status table and any page that still describes it as planned in the same change. Mark a feature Done only when a user can run it.

Include a concise Unreleased changelog entry for user-visible work, with compatibility consequences and required upgrade actions. Otherwise explain why no entry is needed in the pull request's Changelog section.

## Project guides

- Reporting issues, [issue tracker](docs/agents/issue-tracker.md).
- Triage labels, [label guide](docs/agents/triage-labels.md).
- One root glossary and ADR directory, [domain guide](docs/agents/domain.md).
