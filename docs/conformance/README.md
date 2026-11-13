# Conformance Records

`cargo xtask req` checks these files, prints coverage per section and each record's proving tests, and writes both to [summary.md](summary.md). It also writes [interop-masks.toml](interop-masks.toml). CI fails if either committed file is stale.

## Requirements

`requirements/` holds one TOML file per top-level RFC section, for example `rfc9946-06.toml` for RFC 9946 §6. Each normative statement is one entry:

```toml
[[requirement]]
id = "RFC9946-6.1-4"
text = "Note: Any field labeled as 'reserved for alignment', in any PDU, MUST be set to 0 and MUST be ignored upon receipt."
text_sha256 = "…"          # `cargo xtask req hash` < text
state = "pending"          # pending | deviation | interpretation | operational | not-applicable
reference = "ADR 0002"     # required for deviation and interpretation: an ADR or a record ID, or a list of them
ticket = 156               # required while pending and untested: the issue expected to test it, or a list
nightly = true             # optional: tested only by tests that run nightly
note = "…"                 # optional
```

- **ID:** `<document>-<subsection>-<ordinal>`. The ordinal counts the sentences in that subsection that contain a BCP 14 keyword (MUST, SHALL, SHOULD, MAY, REQUIRED, RECOMMENDED, OPTIONAL, in capitals), in the order they appear, starting at 1. IDs never change once assigned.
- **Lowercase statements:** a sentence without a BCP 14 keyword is also a Requirement when it prescribes what an endpoint sends, sets or does, or permits it something ("has to be set", "set to", "may restrict", "is recommended", "the client ends the test", "the OPTION to coerce"). Pure definitions of a field's meaning or unit, and restatements of a keyword sentence, are not. These have their own ordinal series with an `L` prefix, for example `RFC9946-7.2.1-L1`, counted in text order when the subsection is extracted; one added later takes the next free number.
- **Text:** the sentence quoted exactly from the RFC. Line breaks don't matter; the check collapses whitespace before comparing with the RFC text in `docs/rfc/`.
  - A sentence that introduces a list, or has a list inside it, is quoted with the list, because the list completes it. A sentence that introduces a figure or code ends at the colon.
  - A bullet that is a sentence of its own, with its own keyword, gets its own ID and is quoted without its bullet marker.
  - Field labels such as `srStruct:` are left out; a leading `Note:` is kept.
- **Hash:** the first 16 hex digits of SHA-256 over the whitespace-collapsed text. It catches accidental edits.
- **State:** "tested" is never written here. A pending Requirement counts as tested when a test that runs on the gating platform carries its tag.
- **Ticket:** a pending Requirement no test covers names the issue, or issues, expected to test it; the summary lists them by ticket. Once it is tested, its `ticket` goes.
- **Gating platform:** Linux, which CI runs. A test counts there unless it is `#[ignore]`d or its `#[cfg]`, or its file's `#![cfg]`, excludes Linux.
- **Nightly:** a Requirement whose only tests are ignored, because they run in the nightly workflow, is classified `nightly = true` and counted as tested nightly, apart from the rest.

## Records

`records/` holds the Conformance Records: every Deviation, Interpretation and OB-UDPST Difference (see [GLOSSARY.md](../../GLOSSARY.md)), one TOML file per area of behaviour. Each record is one behaviour:

```toml
[[record]]
id = "tests-per-source"         # kebab-case, unique, never reused
kind = "deviation"              # deviation | interpretation | obudpst-difference
deviation = "non-conformance"   # a Deviation's kind: non-conformance | interop-accommodation | platform-gap | unmet-should
summary = "…"                   # what udpstp does, and why
rfc = "RFC 9946 §4.2"           # where the Requirement or ambiguity is; optional for an OB-UDPST Difference
requirements = ["RFC9946-…"]    # optional: the Requirement IDs it concerns
obudpst = "…"                   # what OB-UDPST does, when it differs
default = "off"                 # on | off: for a Deviation, and whenever there is a setting
setting = "`--tests-per-source`, `tests_per_source` in TOML"
revisit = "…"                   # for a Deviation: what would make it worth revisiting
reference = ["ADR 0002", "docs/spec.md story 32"] # Durable public sources
tests = ["src/tests/protocol/tests_per_source.rs::…"]
masks = [{ pdu = "activation-response", fields = ["sr_index_conf"], when = ["max-bandwidth-stated"] }]
```

- **Kinds:** an Interpretation that OB-UDPST doesn't share is one `interpretation` record with `obudpst`; it counts as an OB-UDPST Difference too. A record whose ADR holds the reasoning keeps the summary short and names the ADR.
- **Deviations and Requirements:** a Deviation that lists a Requirement makes that Requirement's state `deviation`, with the record's ID in its `reference` (a list when several Deviations depart from it), and the other way round. An Interpretation or OB-UDPST Difference may list Requirements without changing their state: a Requirement it reads is met as read, and counts as tested once a test carries its tag, as any other.
- **Proving tests:** `path/to/file.rs::test_name`. The report says for each whether it runs, runs only under a `cfg`, is ignored, isn't a test or is missing. A record none of whose tests runs is unproven and listed in the summary.
- **Code notes:** code that implements a record says so in a comment ending `(conformance: <id>)`; the reason lives in the record.
- **Masks:** a difference from OB-UDPST that shows in a Control PDU udpstp sends (`setup-request`, `setup-response`, `activation-request`, `activation-response`, `null-request`) names the fields, as `src/wire` names them (`sending_rate.udp_payload1` for a nested one), and the conditions under which it shows, from `CONDITIONS` in `xtask/src/records.rs`. `interop-masks.toml` lists every masked field with its conditions and records; the OB-UDPST interop comparison ignores those fields in runs where the conditions hold. Per-run fields (authUnixTime, authDigest, ports, mcIdent) and the checksum, which follows the other fields, are the comparison's own business. OB-UDPST's uses of reserved fields are checked separately, by the check that they stay zero.

## Tags

A test proves Requirements with the `#[req]` attribute from the `udpstp-req` crate (a dev-dependency):

```rust
#[udpstp_req::req("RFC9946-6.1-4", "RFC9946-6.1-13")]
#[test]
fn reserved_field_is_ignored_on_receipt() { … }
```

Every normative statement of RFC 9946 and of the RFC 9097 sections it imports has an ID. A test for something without one (the specification, an ADR, a Conformance Record) names it in a plain comment. The check still reports `// req:` section markers, the form tests used before the IDs existed; none are left.

## What fails the check

- **Requirements:** a tag naming an unknown ID, a text whose hash doesn't match, a text not found in the RFC, a duplicate ID, a deviation or interpretation without a reference, and a reference that names neither an ADR nor a record of the same kind that lists the Requirement.
- **Completeness:** a BCP 14 keyword, in any subsection of a section that has a requirements file, outside every requirement's text (code blocks and table lines aside; a text the RFC repeats covers each place); a keyword series whose ordinals don't follow the text. The `L` series isn't checked for order, since one found later takes the next free number.
- **Classification:** a pending Requirement with no test and no `ticket`; a tested one that still names a `ticket`; one whose only tests don't run on the gating platform and isn't `nightly`; `nightly` on one that a gating test covers, or that no test covers; a `#[req]` tag on a function that isn't a test.
- **Records:** a duplicate ID; a Deviation without its kind, default or revisit trigger; a Deviation or Interpretation without `rfc`; a setting without a default, or a `--flag` the binary doesn't have; an unknown Requirement ID or ADR; a Deviation whose Requirement isn't marked; a listed test that is missing or isn't a test; a mask without `obudpst`, or naming an unknown field or condition; a `conformance:` note naming no record.

Unproven records are reported but don't fail the check yet.
