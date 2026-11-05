# Running a Test

For Client users: Operators, measurement platforms, and anyone who runs `udpstp client` against a UDPSTP Server. [Understanding the results](../understanding-results.md) explains what the results mean. `udpstp client --help` lists every option.

## Get a key file

Tests are authenticated, so you need the same Shared Key as the Server. Ask the Server's administrator for the key and its keyId.

1. Save them in a key file. A name ending in `.toml` is a TOML Key Table:

   ```toml
   [[key]]
   admin_key_name = "customers-2026"
   key_id = 1
   key_hex = "5b0c0e8d6f7a41c2b9d3e4f5061728394a5b6c7d8e9fa0b1c2d3e4f506172839"
   ```

   Give the key as `key_hex` (hexadecimal) or `key_text` (its UTF-8 bytes), not both. Any other file name is read as an OB-UDPST `udpst.keys` file, with one `<keyId>,<key>` line per key.

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

Give the Server as a host name or address, with a port if it isn't the Standard Port, 24601: `server.example.net:24601`, `192.0.2.10` or `[2001:db8::10]:24601`.

Run an Upstream Test, in which the Client sends the load:

```sh
udpstp client --key-file keys.toml --direction upstream server.example.net
```

## Choose the Authentication Mode

| You're testing against | Use |
|---|---|
| a udpstp Server, or any Server that supports mode 2 | nothing extra: mode 2 is the default and authenticates every Status PDU too |
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

## Read the results

The Client prints its progress, then the results:

```text
Sending the Setup Request to 192.0.2.10:24601
Setup accepted, Ephemeral Port 56843
Test activated (Downstream)
The Server ended the Test
```

The headline is the Maximum IP-Layer Capacity near the end of the results:

```text
  Maximum IP-Layer Capacity 1809.98 Mbps in Sub-Interval 8 (Loss Ratio 0.000000, …
```

Any warnings follow it under **Warnings**. A Test that completed with warnings still exits with status 0, so read them: each one says what limited the result. [Fix common problems](#fix-common-problems) says what to do about each, and [Understanding the results](../understanding-results.md) explains every line.

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
- Several SERVERs share the Test Connections in turn. They must all be IPv4 or all IPv6.
- `--min-test-connections N` sets how many must start for the Test to go on. By default all of them must.
- `--max-bandwidth` is for the whole Test, shared equally between its Test Connections.

## If the Server answers from another address

The Client accepts responses only from the address it sent to. If a multi-homed Server answers from another one, ask its administrator for that address and allow it:

```sh
udpstp client --key-file keys.toml --allow-server-address server.example.net=192.0.2.20 server.example.net
```

The SERVER before `=` must match the one you test against exactly, port included. Repeat the option for more addresses. Unauthenticated Tests can't use it.

## Stop a Test

Press Ctrl-C. The Client tells the Server to stop and prints the results measured so far. A second Ctrl-C ends the Client at once. [Exit statuses](../interfaces.md#exit-statuses) says how an interrupted Test exits.

## Fix common problems

### No answer from the Server

```text
udpstp: no answer from 192.0.2.10:24601 within 3 s
```

A Server doesn't answer a request that fails authentication, so a wrong key, a wrong keyId and a blocked port all look the same. Check, in order:

1. the SERVER address and port;
2. firewalls between you and the Server;
3. the key and keyId;
4. that both clocks are within 5 s of true time.

If it still fails, ask the Server's administrator to turn on [Troubleshooting Mode](hosting-a-server.md#debug-a-failing-client) for a while. The Client then shows the reason:

```text
udpstp: an unverified Setup Response said: authentication failed; check the key and keyId
```

If you use `--checksum` and the Client reports that a response failed its header checksum, try without `--checksum`.

### The Server refused the Test

| Status | Response | What to do |
|---|---|---|
| 102 | BADVER | Use a Client and Server that speak the same protocol version |
| 103, 111 | BADJS, BADTMTU | Match the Server's `--jumbo` or `--traditional-mtu` setting |
| 109 | NOMAXBW | Set `--max-bandwidth` |
| 110 | CAPEXC | Retry later, or ask for less with `--max-bandwidth` |
| 113 | CONNFAIL | Retry later. The Server is full, or you have reached its limit of Tests or your Quota. A second Test from your address while one runs gets this too |
| 152 | BADPARAM | Often a Fixed-Rate Test without an Operator Key, or a Test that no longer fits the Server's limits |

### Other exit statuses

| Status | What to do |
|---|---|
| 50 | Fix the command line, or give a key file or `--unauthenticated` |
| 51 | Fix the key file. The message names the problem and the line |
| 75 | Check the SERVER name and your network |
| 107, 108, 157, 158 | Check the key, the keyId and both clocks |
| 200 | See [No answer from the Server](#no-answer-from-the-server) |
| 201 | Ask for a value the Server allows, or drop `--refuse-coercion` |
| 202 | Report it, with both ends' versions |
| 203 | Use Servers with the same configuration for one Multi-Connection Test |

[Exit statuses](../interfaces.md#exit-statuses) lists them all.

### Warnings

| Warning | What to do |
|---|---|
| No Sub-Interval met the Performance Criterion | Compare the Highest IP-Layer Capacity and the Loss Ratios. A bottleneck with a shallow buffer loses a few percent all along, and `--max-loss-ratio 0.1` accepts that |
| A Sub-Interval before or after the Maximum's ran faster, or the last one much slower | Look at the Loss Ratios after the Maximum's Sub-Interval. A few percent at a steady capacity is the search cycling around the bottleneck, and `--max-loss-ratio 0.05` accepts it. Through CAKE, use enough `--test-connections` that each carries under 1 Gbit/s. If the path slowed, test again |
| Load PDUs not sent: this host fell behind (Skipped Bursts) | Use a faster host, or fewer Test Connections per host |
| Load PDUs not sent: the send buffer was full (Withheld Bursts) | [Raise the socket buffers](#the-os-held-the-socket-buffers-below-the-size-asked-for) |
| The Server sent below its Fixed-Rate row | Test against a less loaded Server or at a lower row, or ask its administrator to check its log |
| The search never confirmed Congestion: the maxBandwidth | Raise `--max-bandwidth`, or leave it out |
| The search never confirmed Congestion: the top of the Sending Rate Table | Use more `--test-connections` |
| The search never confirmed Congestion: Hold Intervals | Test over a steadier path. A busy Wi-Fi link's delay varies too much for the search to rise |
| The search never confirmed Congestion: still rising | Use a longer `--test-interval`, or `--algorithm c`, which rises faster |
| The search never confirmed Congestion: a Load Sender stopped rising | If it was the Server's, test against a less loaded Server. If it was this host's, as for Skipped Bursts |
| Datagrams too large for the path with DF set | Drop `--traditional-mtu`, or ask the Server's administrator to lower its `server_mtu` |
| ICMP time-exceeded errors: --max-hops is too small for the path | Raise `--max-hops` above the path length |
| Status PDUs never arrived, or the peer had missed Status PDUs | Look for loss in the reverse direction |
| Status PDUs failed authentication and were ignored | Something on the path changed or injected them |
| Nothing was received for 1 s, or the Watchdog ended the Test | Check the path and the Server |
| Load PDUs without a delay sample: the wall clock stepped | Let NTP settle before testing |

[What the warnings mean](../understanding-results.md#what-the-warnings-mean) explains each one.

### The OS held the socket buffers below the size asked for

At high rates, small socket buffers drop datagrams on the host, which looks like loss on the path. udpstp asks for 2 MiB and warns when the OS grants less:

```text
udpstp: warning: the OS holds socket buffers below the 2097152 bytes asked for (receive …, send …); at high rates datagrams may be dropped locally, so raise the net.core.rmem_max and net.core.wmem_max sysctls
```

On Linux, raise the limits, then check the "This host" line of the next Test:

```sh
sudo sysctl -w net.core.rmem_max=4194304 net.core.wmem_max=4194304
```

On macOS and FreeBSD the limit is `kern.ipc.maxsockbuf`.
