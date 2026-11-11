# Stable interfaces

Scripts and tools can rely on udpstp's exit statuses, JSON results, key files, use of stdout and stderr, and address pseudonyms in every release. The command line, the Server's settings and its JSON log settle during 0.x and become stable at 1.0. [ADR 0013](adr/0013-stable-interfaces-from-0-1-0.md) records the decision and its reasons, and the [changelog](../CHANGELOG.md) lists every change.

## What is stable

| Interface | What is stable | Reference |
|---|---|---|
| Exit statuses | The rule behind them, and every value assigned | [below](#exit-statuses) |
| JSON results (`--format json`) | Key names, types, enumerated values. The OB-UDPST part keeps OB-UDPST v9.1.0's schema, except the recorded differences | [JSON results](json-output.md) |
| Key files | The TOML Key Table's columns, with the deprecated `format` marker accepted during its notice period. OB-UDPST's `udpst.keys` as OB-UDPST reads it and the pseudonym key file | [below](#key-files) |
| stdout and stderr | Which output goes where | [below](#stdout-and-stderr) |
| Address pseudonyms | The algorithm, so one key gives one Client the same pseudonym in every version | [below](#address-pseudonyms) |

## Stable from 1.0

| Interface | What becomes stable | Reference |
|---|---|---|
| Command line and the Server's TOML settings | Option and key names, value spellings, defaults | `udpstp client --help`, `udpstp server --help`, `udpstp server --print-config`; [Server configuration](../crates/udpstp/src/config.rs) |
| The Server's JSON log (`log_format = "json"`) | Event names, field keys, enumerated values | [Server log](server-log.md) |

Until then, a 0.x minor release may rename or remove one of these, with no deprecation period. The changelog lists each change under Changed or Removed, so read it before upgrading a script or a configuration file.

## What is not

Human output adapts to the width of the terminal it is written to. Redirected output and a terminal whose width is unknown use 80 columns. Help, diagnostics and progress wrap with continuation indentation; results use a full table when it fits and grouped tables otherwise, with repeated Sub-Interval numbers and a nearby legend. An identifier wider than the terminal stays intact on a continuation line so it remains copyable. The human Server log marks continuation lines with `  | `; each event still starts with its own timestamp and level. JSON and configuration output are unaffected by this layout.

- Human-readable text: the text results, progress lines, the wording of warnings and errors, `--help`, and the human log's messages and layout. Parse the JSON results and the JSON log instead.
- Free text inside the JSON: `ErrorMessage`, `ErrorMessage2`, a warning's `Message`, the log's `message` and `error` fields. Use `ErrorStatus`, a warning's `Code`, the log's `event` and `reason`.
- The format of `udpstp --version`.
- `UDPSTP_FAULTS` and `UDPSTP_BLESS`, which exist for udpstp's own tests.
- `UDPSTP_KERNEL_TX_TIMESTAMPS`, an experimental setting for precision tests: set to `1` on Linux, a Load Sender asks the kernel to timestamp what it sends, and the Client's JSON results add `udpstp.KernelSendTimestamps` (`Samples`, `LagP50Us`, `LagP99Us`, `LagMaxUs`: how much later than lpduTime the kernel sent them). Set to `kernel-count`, it asks for the same timestamps matched by the kernel's own count of the messages on every kernel, as before Linux 6.13, rather than by numbers the messages carry, so a result can be checked on that send path. The variable, its values and the block may change or go in any release.
- `UDPSTP_ENGINE_CATCH_UP_US`, an experimental setting for measurements: the Data Phase Engine's catch-up budget in µs (ADR 0012), in place of 1 ms (2 ms on Windows). It may change or go in any release.
- `UDPSTP_ENGINE_THREADS`, an experimental setting for measurements of a Linux Server, whose Data Phase Engine runs on a pool of Engine Threads: the most Engine Threads the pool starts, in place of the CPUs the process may use (ADR 0008). It may change or go in any release.
- `UDPSTP_ENGINE_HOLD_OFF_US`, an experimental setting for measurements: on Linux it sets the Data Phase Engine's receive hold-off in µs, 0 for none, in place of the one derived from the receive buffer, and a Load Receiver takes it after every turn that read datagrams (a Load Sender, when flooded), where it takes the derived one only when more than one datagram is due within it. It may change or go in any release.
- The library crates (`udpstp-wire`, `udpstp-core`, `udpstp-runtime`, `udpstp-key-permissions`): they follow Cargo's semver rules only, and aren't a supported interface in 0.x.

## Terminal color

`--color auto|always|never` is global: it can precede or follow `client` or
`server`. The default, `auto`, checks stdout and stderr independently and styles
only a terminal that supports color. A nonempty `NO_COLOR` or `TERM=dumb`
disables automatic styling; an empty `NO_COLOR` does not. `always` and `never`
override these environment defaults. `CLICOLOR`, `CLICOLOR_FORCE` and `CI` do not
override this policy. `never` removes bold, dimming, colors and decorative status
symbols. Redirected automatic output keeps the plain presentation.

The same policy covers help, parser errors, startup diagnostics, Client progress
and text results, and human Server console logs. `always` keeps styling when
human output is redirected. JSON results, JSON logs, printed TOML configuration
and daily log files stay plain in every mode, including Windows service files.
Startup diagnostics copied to a log file stay plain there while their stderr
copies follow stderr's policy.

| Information | Style |
|---|---|
| Errors and failed or interrupted outcomes | Bold red, with ✗ on the Test outcome |
| Warnings and qualified results | Bold amber, with ⚠ on the outcome and warnings heading |
| Completed Test without warnings | Bold green, with ✔ on the outcome |
| Section and help headings | Bold purple |
| Progress and INFO log labels | Blue, with ▶ on the start of a step |
| Table headings, timestamps and host metadata | Muted gray |
| Commands and option names | Bold, default foreground |
| Final Maximum IP-Layer Capacity | The Test outcome's color: green for clean completion, amber for qualifications, red for failure or interruption |
| Other measurements, addresses and ordinary values | Default foreground |

Colors use RGB foregrounds on the terminal's default background: purple
`#bd93f9`, blue `#5cb8ff`, green `#5af78e`, amber `#f39c12`, red `#ff5c57`
and gray `#70788c`. Terminals with fewer colors may approximate them.
Client results show one aligned Sub-Interval table when it fits, or two metric
groups with repeated Sub-Interval numbers. Very narrow groups become labelled
values. Final metrics show each Capacity Mode's maximum once, followed by
supporting measurements, warnings and the aggregate Test outcome.
Labels remain understandable without color; a high rate alone is no reason for
green. Symbols supplement the outcome text. Styling preserves copyable values
and table alignment, with wrapping adapted to the displayed symbol width.

## How a stable interface changes

These rules cover the interfaces that are stable today, and from 1.0 the command line, settings and JSON log too.

- **Additions** come in a minor release: new options, settings, JSON keys, log events and fields, enumerated values, exit statuses and Key Table columns. Your tools should ignore what they don't know: an unknown key, field, event or value, and treat an unknown non-zero exit status as a failure.
- **A renamed or removed input** (an option, a setting, a value, a Key Table column) is still accepted for one minor release, with a warning on stderr naming its replacement. A name deprecated in one minor release works through that release's patch releases and may go in the next minor release.
- **A renamed or removed output** (a log event, field or value, a JSON key or value) is written under both names for one minor release, then only under the new one.
- **A default** changes in a minor release, announced in the changelog of the minor release before. A security fix may change one at once; the changelog says so.

Each is in the changelog, under Added, Deprecated, Changed or Removed. Read it before upgrading across a minor release.

## Exit statuses

A Client's exit status is its ErrorStatus, the one OB-UDPST reports, except that a Test that completed with only warnings exits 0. udpstp's own statuses use values OB-UDPST leaves unused. New statuses may be added; an assigned one never changes meaning.

| Status | Meaning |
|---|---|
| 0 | The Test completed. With `--format json`, `ErrorStatus` gives its most severe warning: 49 no valid Maximum IP-Layer Capacity, 4, 5, 3, 2 or 6 as [JSON results](json-output.md#where-udpstp-differs) lists |
| 50 | The configuration or command line is invalid |
| 51 | The key file can't be read or is invalid |
| 75 | The Client or Server couldn't start |
| 100 + code | The Server refused the Setup Request with that Command Response (102 to 113); 100 for a code RFC 9946 doesn't define |
| 150 + code | The Server refused the Activation Request; 152 is BADPARAM, 150 an undefined code |
| 107, 108, 157, 158 | A Setup or Activation Response failed verification, or its time was outside the window |
| 200 | The Test didn't complete |
| 201 | The Server coerced a parameter the Client was told to refuse |
| 202 | The Server set a parameter the Client can't run with |
| 203 | A Multi-Connection Test's Servers gave different parameters |
| 204 | The Test was interrupted, or a write to stdout failed. On Linux and macOS an interrupted Client ends by the signal (130 for Ctrl-C), and 204 is the JSON's `ErrorStatus` |
| 255 | Any other error |

[Troubleshooting](troubleshooting.md#other-exit-statuses) says what to do about each.

## stdout and stderr

| Program | stdout | stderr |
|---|---|---|
| `udpstp client --format json` | The JSON results, and nothing else | Progress, warnings, errors |
| `udpstp client` (text) | Progress and the text results | Errors, and warnings about this host |
| `udpstp server` | The log | Errors and warnings at startup |
| `udpstp server` with `log_file` | Nothing; the log goes to daily files named from `log_file`, with the startup's errors and warnings as `StartupError` and `StartupWarning` | Errors and warnings at startup |
| `udpstp server --windows-service` | None: a service has none; its log, startup included, goes to `log_file` | None |
| `udpstp server --print-config` | The effective configuration, as TOML | Errors |
| `udpstp server --print-config --settings-only` | Validated settings as TOML, without opening keys, logs or listeners | Settings errors |

Windows Shared Key files must meet [ADR 0015](adr/0015-windows-shared-key-private-file-policy.md)'s
supported local-NTFS policy. Mismatches and unverified permissions are distinct
key file errors (51), refused before content reading, with actionable reasons
that do not claim effective exposure. Client errors leave stdout empty; Server
errors also reach a configured daily log as `StartupError`. Ordinary
`--print-config` validates keys. Its `--settings-only` modifier previews settings
and establishes no key readiness; it requires `--print-config` and conflicts
with `--windows-service`. See [Key handling](security.md#key-handling).

## Key files

A TOML Key Table's columns keep their meaning in later versions: columns are added, never removed or read differently.

The optional top-level `format = 1` marker is deprecated. It is still accepted with a warning throughout the minor release that first publishes the deprecation and that release's patches. It may be removed in the following minor release. Delete the line before upgrading. The file keeps the same meaning without it ([ADR 0016](adr/0016-retire-unused-features.md)).

A file with a column this version doesn't know, or any `format` value other than 1, is refused rather than read in part, since the column it would skip may be a restriction such as a lifetime or a Quota. Upgrade every Server and Client that reads a Key Table before you add a new column to it. [Key files](key-files.md) lists the columns.

OB-UDPST `udpst.keys` files are read as OB-UDPST reads them. The pseudonym key file is hexadecimal digits, 16 bytes or more, with whitespace around them and nothing else.

## Address pseudonyms

With `address_privacy = "pseudonymize"` (the Server) or `--address-privacy pseudonymize` (the Client), an address shows as `anon-` and the lowercase hexadecimal of the first 8 bytes of HMAC-SHA-256 over the IP address's bytes, keyed with the pseudonym key: 4 bytes for IPv4, including an IPv4-mapped IPv6 address, and 16 for IPv6. The port is left out. `truncate` shows the IPv4 /24 or IPv6 /48.

## Container images

Each published release provides `ghcr.io/pederbe/udpstp:<version>`, tagged with the release's version number without the leading `v`, which never changes. `:latest` follows releases that are not marked as pre-releases, including one that ends a deprecation period. Pin `:<version>` and upgrade on purpose.
