# udpstp's stable interfaces from 0.1.0

**Status: Accepted** (2026-09-26). Amended on 2026-11-01: the CLI, the TOML settings and the Server's JSON log settle during 0.x and become stable at 1.0.

Measurement platforms and Operators script against udpstp: they parse its JSON results and the Server's JSON log, branch on its exit status, generate its configuration and Key Tables, and join pseudonymized addresses across days. From 0.1.0 they need to know which of these they can build on and how a change reaches them. The rules apply to the `udpstp` binary; the library crates are a separate case (below). [docs/interfaces.md](../interfaces.md) is the user-facing summary.

## Decision

**Stable from 0.1.0.** These change only with a [changelog](../../CHANGELOG.md) entry and a deprecation period:

- exit statuses: the rule behind them and every value assigned so far (below);
- the JSON results: key names, types and enumerated values, but not the free text inside them (`ErrorMessage`, `ErrorMessage2`, a warning's `Message`). The OB-UDPST part tracks OB-UDPST v9.1.0's schema, except the OB-UDPST Differences recorded in `docs/conformance/records/client.toml`;
- the key file formats (below), and the pseudonym key file: hexadecimal digits, at least 16 bytes, with whitespace around them and nothing else;
- which output goes to stdout and which to stderr: the Client's JSON results alone on stdout with `--format json`, its text results and progress on stdout otherwise, errors and warnings about the host on stderr; the Server's log on stdout, its startup errors and warnings on stderr, `--print-config` on stdout; with a log file set (`log_file`, [ADR 0014](0014-the-server-as-a-windows-service.md)), the log and the startup's errors and warnings (`StartupError`, `StartupWarning`) go to the file, the latter to stderr too;
- the pseudonymized-address algorithm: `anon-` and the lowercase hexadecimal of the first 8 bytes of HMAC-SHA-256, keyed with the pseudonym key, over the IP address's octets (4 for IPv4, an IPv4-mapped IPv6 address taken as its IPv4 address; 16 for IPv6); `truncate` shows the IPv4 /24 or IPv6 /48.

**Settled during 0.x, stable from 1.0.** These may change in any 0.x minor release, with a changelog entry under Changed or Removed and no deprecation period. From 1.0 they follow the same rules as the list above:

- the CLI and the Server's TOML settings: option and key names, value spellings and defaults;
- the Server's JSON log: event names, field keys and enumerated values. The keys every line has (`timestamp`, `level`, `message`) come from tracing-subscriber; `message` is free text.

Scripts and measurement platforms mostly read the JSON results and branch on the exit status, which are stable from the first release. Command lines, settings and log names haven't met users yet. Letting them settle during 0.x means early feedback can fix them without carrying an alias for every rename.

**Not stable:** all human-readable text (the text results, progress lines, warning and error wording, `--help`, the human log's message text and layout); the `--version` string's format; the test-only environment variables `UDPSTP_FAULTS` and `UDPSTP_BLESS`, and the experimental ones such as `UDPSTP_ENGINE_HOLD_OFF_US` and `UDPSTP_ENGINE_CATCH_UP_US`, which may change or go in any release.

**Additions are not changes.** A minor release may add options, settings, JSON keys, log events and fields, enumerated values, exit statuses and Key Table columns. A tool must ignore what it doesn't know. An addition gets a changelog entry under Added.

**Deprecating an input** (from 0.1.0 a Key Table column, and from 1.0 also an option, a setting or a value spelling): for one minor release the old name is still accepted, through a serde alias or a clap alias, with a warning on stderr naming the new one, and the changelog lists it under Deprecated. An alias alone is silent, so the warning comes from the code that reads the input, as `mtu`'s hint does today (a hidden option, and a look at the file's keys before the strict parse; `config.rs`). A name deprecated in 0.3.0 is accepted through 0.3.x and may be removed in 0.4.0, which the changelog lists under Removed.

**Deprecating an output name** (from 0.1.0 a JSON key or value, and from 1.0 also a log event, field or value): for one minor release both the old and the new name are emitted, and the changelog says so. Then the old one goes.

**Defaults** have no alias: from 1.0 a default changes in a minor release, announced in the changelog of the minor release before. A security fix may change a default, or refuse an input, without the period; its changelog entry says so.

**Changes before 0.1.0** need no deprecation period: nothing is stable yet. ADR 0015 replaces Windows permission warnings with refusal at status 51 and removes the two warning reasons before 0.1.0. The additive `--settings-only` modifier preserves ordinary `--print-config` key validation. `mtu` became `server_mtu` and is refused with a hint.

### Exit statuses

The ErrorStatus is OB-UDPST's (`udpst.h`), and the exit status equals it, except that a Test that completed with only warnings exits 0. udpstp's own statuses take values OB-UDPST leaves unused: 49 in its warning range, and 201 and up in its connection-error range, which uses only 200. A new status may be added; an assigned one never changes meaning (`crates/udpstp/src/exit.rs`):

| ErrorStatus | Meaning | Exit status |
|---|---|---|
| 0 | The Test completed | 0 |
| 2 to 6 | It completed with a warning, the most severe of: 4 nothing received for 1 s, 5 the peer said it stopped receiving, 3 the peer missed Status PDUs, 2 Status PDUs never arrived, 6 invalid PDUs | 0 |
| 49 | Ours: it completed without a valid Maximum IP-Layer Capacity; the most severe warning | 0 |
| 50 | The configuration or command line is invalid | 50 |
| 51 | The key file can't be read or is invalid | 51 |
| 75 | The Client or Server couldn't start | 75 |
| 100 + cmdResponse | The Server refused the Setup Request (102 to 113); 100 for a code RFC 9946 doesn't define; 107 or 108 for a Setup Response that failed verification or the time window | the same |
| 150 + cmdResponse | The same for the Activation exchange: 152 BADPARAM, 150 an undefined code, 157 or 158 | the same |
| 200 | The Test didn't complete: no answer in time, or the Watchdog ended it | 200 |
| 201 | Ours: the Server coerced a parameter the Client was told to refuse | 201 |
| 202 | Ours: the Server set a parameter the Client can't run with | 202 |
| 203 | Ours: a Multi-Connection Test's Servers gave different parameters | 203 |
| 204 | Ours: the Client user interrupted the Test, or a write to standard output failed. On Unix a Client stopped by a signal then dies of it, so a shell sees 128 plus the signal (130 for SIGINT); 204 is then the JSON ErrorStatus | 204, or the signal's |
| 255 | Any other error | 255 |

### Key files

The optional format marker decision below is superseded by [ADR 0016](0016-retire-unused-features.md). `format = 1` is deprecated and still accepted during its minor-release window. Delete that line without changing the Key Table's meaning. The columns and unknown-field rejection remain unchanged.

**Forward-incompatible by design.** Every file valid in a release keeps its meaning in every later one: a column is never removed or read differently, only added. A file written for a newer udpstp, with a column this version doesn't know or a higher `format`, is refused whole rather than read in part, because the column it would skip may be a restriction: a lifetime, a Quota, an Operator Key's limits. An older Server that ignored it would widen what a key allows.

A TOML Key Table may start with `format = 1`; a file without it is format 1. A later format number is needed only for a file that an older udpstp would misread even though it knows every column, which the rule above rules out, so it is reserved for a new format beside this one. A renamed column is an input like any other: its old name is accepted, with a warning, for one minor release. OB-UDPST's `udpst.keys` format is read as OB-UDPST reads it (conformance: obudpst-key-file-reading), and follows OB-UDPST.

### Names come from tables

Every log event, JSON enumerated value and warning code is written from the explicit tables in `crates/udpstp/src/names.rs`, never from a Rust `Debug` name, so renaming a core variant can't change an output. The core's own names are free to change.

### The library crates

The published library crates, `udpstp-wire`, `udpstp-core` and `udpstp-runtime`, follow Cargo's semver rules only, and aren't a supported interface in 0.x: a minor release may break them without a deprecation period. The binary is the interface.

### The container image

Superseded by [ADR 0017, Latest-only distribution](0017-latest-only-distribution.md).

Publishing a GitHub release starts the separate Release container workflow, which publishes `ghcr.io/pederbe/udpstp:<version>` (such as `:0.1.0`) from its verified Linux binaries. `:<version>` never moves. `:latest` follows releases that are not marked as pre-releases, including one that ends a deprecation period, so a deployment pins `:<version>` and upgrades on purpose. [Release artifacts and verification](../releasing.md) describes its downloads and provenance.

## Considered options

- **Promise nothing until 1.0.** Platforms script against the JSON results and the exit status from the first release; without a promise, every 0.x release could break them unannounced.
- **Hold every interface to the deprecation rule from 0.1.0.** This was the decision until 2026-11-01. It froze command-line, settings and log names before any user had tried them, so every early fix would need an alias and a warning for a release.
- **Snapshot `--help` and the log's text.** Text changes for good reasons, and the specification asserts on structured output, never on text. The tests read clap's Command model, the settings struct and the JSON log's parsed lines instead.
- **Accept unknown Key Table columns with a warning.** It lets one Key Table serve a mix of versions, but an older Server would drop a restriction it can't read, which widens access; a warning in a log nobody reads doesn't stop that.
- **Keep deprecated names forever.** Every alias is a second spelling to document and test; one minor release gives users a release to see the warning and move.
- **Make the library crates a supported interface.** Their API still changes with each feature, and the core's sans-IO shape isn't settled enough to promise.

## Consequences

- Recorded interface fixtures make changes fail a test and expose intentional updates in the commit diff for review. The [testing guide](../testing.md#cli-and-loopback-integration-tests-s2a-s2b) lists the fixtures, their source modules and the regeneration command.
- Each release's changelog lists what was added, deprecated and removed; [releasing.md](../releasing.md) has the step.
- Open: how an enumerated value is emitted under both names during its period, since one field holds one value (a second field for the new value, or the old value kept until the period ends). The first such deprecation decides it.
- Whatever 0.1.0 ships in the JSON results, exit statuses, key files, stdout and stderr, and pseudonyms becomes stable, inconsistencies too. The golden tests still catch every CLI, settings and log change during 0.x, so each one is deliberate and has its changelog entry. Two command-line inconsistencies were settled before 0.1.0: the Client's `--checksum` takes an optional boolean as the Server's does, and the Server's `--delay-var` / `delay_var`, the statistic it classifies by, became `--delay-var-statistic` / `delay_var_statistic`, so `--delay-var` names only the Client's choice of delay variation.
