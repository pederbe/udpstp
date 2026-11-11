# Running a Test

`udpstp client` runs a Test against a UDPSTP Server and prints the Maximum IP-Layer Capacity of the path between them. You need udpstp [installed](installing-udpstp.md), a Server to test against and its key. `udpstp client --help` lists every option.

## Get a key file

Tests are authenticated, so you need the same Shared Key as the Server. Ask the Server's administrator for the key and its keyId.

1. Save them in a key file. A name ending in `.toml` is a TOML Key Table:

   ```toml
   [[key]]
   admin_key_name = "customers-2026"
   key_id = 1
   key_hex = "5b0c0e8d6f7a41c2b9d3e4f5061728394a5b6c7d8e9fa0b1c2d3e4f506172839"
   ```

   Give the key as `key_hex` (hexadecimal) or `key_text` (its UTF-8 bytes), not both. Replace the example key with your own. Any other file name is read as an OB-UDPST `udpst.keys` file, with one `<keyId>,<key>` line per key.

2. Make the file private. On Linux and macOS:

   ```sh
   chmod 600 keys.toml
   ```

   On Windows, follow [Key file permissions](windows.md#key-file-permissions). udpstp refuses a key file others can read, with exit status 51.

If the file holds several keys valid now, choose one with `--key-id`. [Key files](../key-files.md) describes the format in full.

## Run a Test

Run a 10-second Downstream Test, in which the Server sends the load:

```sh
udpstp client --key-file keys.toml server.example.net
```

Give the Server as a host name or address, such as `server.example.net`, `192.0.2.10` or `[2001:db8::10]:24601`. Add a port if the Server doesn't listen on the Standard Port, 24601.

Run an Upstream Test, in which the Client sends the load:

```sh
udpstp client --key-file keys.toml --direction upstream server.example.net
```

## Choose the Authentication Mode

| You're testing against | Use |
|---|---|
| a udpstp Server, or any Server that supports mode 2 | nothing extra. Mode 2 is the default and authenticates every Status PDU too |
| an OB-UDPST Server | `--auth-mode 1`, since OB-UDPST 9.1 doesn't support mode 2 |
| several Servers, some without mode 2 | `--mode-1-server SERVER` for each of those |
| a Server that allows only Unauthenticated Mode | `--unauthenticated`, without a key file |

The Client never falls back from mode 2 to mode 1 by itself, so an attacker can't downgrade a Test. When a Server seems not to support mode 2, the Client says so:

```text
udpstp: SERVER probably doesn't support mode 2: it rejected it, unverified; use --auth-mode 1 (the Client never falls back by itself)
```

Use `--unauthenticated` only on networks where every host is trusted. It protects nothing, and the Server must allow it too.

## Common options

| Option | What it does | Default |
|---|---|---|
| `--direction upstream` | Run an Upstream Test | `downstream` |
| `--test-interval SECONDS` | The Test Interval, 5 to 60 s, or longer with an Operator Key that allows it ([Operator features](operator-features.md)) | 10 |
| `--max-bandwidth MBPS` | The most the Test is expected to carry. The Server searches no higher, and a Server with an Admitted Rate Cap may require it | none |
| `--start-row ROW` | Start the search at this Sending Rate Table row. Row N offers N Mbps up to row 1000 | the Server's choice |
| `--test-connections N` | Run a Multi-Connection Test with N Test Connections | one per SERVER |
| `--algorithm c` | Search with algorithm C (TR-471) instead of B | `b` |
| `--delay-var rtt` | Judge by round-trip delay variation instead of one-way, which follows clock drift | `one-way` |
| `--random-payload` | Fill Load PDUs with random bytes, so compression on the path can't inflate the result | zeros |
| `--max-loss-ratio RATIO` | The Performance Criterion: the most a Sub-Interval may lose and still count | 0.01 |
| `--refuse-coercion PARAMETER` | End the Test if the Server changes this Activation parameter, named as in RFC 9946, such as `testIntTime`. Repeatable | accept the change |
| `--format json` | Print JSON results ([below](#save-json-results)) | `text` |
| `--max-hops HOPS` | MaxHops: the TTL or hop limit of this end's Test traffic | 64 |
| `--dscp DSCP` | Mark the Test's traffic with this DSCP. The Server may change it to 0 | 0 |

`--jumbo` and `--traditional-mtu` must match the Server's settings, or it refuses the Test.

Write boolean values with `=`, such as `--jumbo=false` or `--json-extension=false`. With `--jumbo false`, the Client treats `false` as another SERVER and tries to look up that name.

## Read the results

The Client prints its progress, then the results:

```text
Sending the Setup Request to 192.0.2.10:24601
Setup accepted (Ephemeral Port 58730)
Test activated (Downstream)
The Server ended the Test
```

The headline is the Maximum IP-Layer Capacity, under **Final metrics**:

```text
Final metrics
  Maximum IP-Layer Capacity: 9516.51 Mbps (Sub-Interval 9)
```

Any warnings follow it under **Warnings**. A Test that completed with warnings still exits with status 0, so read them. Each one says what limited the result. [Troubleshooting](../troubleshooting.md#the-test-completed-with-warnings) says what to do about each, and [Understanding the results](../understanding-results.md) explains every line.

When the Server changes a parameter you asked for, a line says so before the Test starts, and the Test runs with the Server's value:

```text
The Server set testIntTime to 60 (asked for 120)
```

To end the Test instead, add `--refuse-coercion testIntTime`.

## Save JSON results

```sh
udpstp client --key-file keys.toml --format json server.example.net > result.json
```

The JSON follows OB-UDPST's schema, so tools written for OB-UDPST keep working. Progress and warnings go to standard error, so standard output holds only the JSON. A last `"udpstp"` object adds what OB-UDPST's schema lacks. Leave it out with `--json-extension=false` for tools that reject unknown keys.

In udpstp's JSON, `AtMax` and `ModalResult` are the Maximum IP-Layer Capacity, which meets the Performance Criterion, not OB-UDPST's highest Sub-Interval. [JSON results](../json-output.md) lists every field.

To keep your own address out of shared results, add `--address-privacy truncate` or `--address-privacy pseudonymize --address-privacy-key-file FILE`.

## Run a Multi-Connection Test

One Test Connection may not fill a fast path. Run several in the same direction, and the Client sums their results:

```sh
udpstp client --key-file keys.toml --test-connections 4 server.example.net
```

- `--test-connections N` runs 1 to 255 Test Connections. OB-UDPST Servers accept at most 24, and so does a udpstp Server by default.
- Several SERVERs share the Test Connections in turn. Give at least one Test Connection per SERVER. They must all be IPv4 or all IPv6.
- `--min-test-connections N` sets how many must start and keep running for the Test to go on. It cannot exceed `--test-connections`, whose default is one per SERVER. By default all Test Connections must succeed.
- `--max-bandwidth` is for the whole Test, shared equally between its Test Connections. Allow at least 1 Mbps each. For four Test Connections, use `--max-bandwidth 4` or higher.

## If the Server answers from another address

The Client accepts responses only from the address it sent to. If a multi-homed Server answers from another one, ask its administrator for that address and allow it:

```sh
udpstp client --key-file keys.toml --allow-server-address server.example.net=192.0.2.20 server.example.net
```

The SERVER before `=` must match the one you test against exactly, port included. Repeat the option for more addresses. Unauthenticated Tests can't use it.

## Stop a Test

Press Ctrl-C. The Client tells the Server to stop and prints the results measured so far. A second Ctrl-C ends the Client at once. [Exit statuses](../interfaces.md#exit-statuses) says how an interrupted Test exits.

## Fix common problems

When a Test fails, the Client prints one line that says why and exits with a status that names the cause. [Troubleshooting](../troubleshooting.md) lists each message, status and warning with its fix.
