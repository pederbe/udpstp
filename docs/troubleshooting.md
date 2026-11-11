# Troubleshooting

When a Test fails, udpstp prints one line that says why and exits with a status that names the cause. Find the message or the status below for its cause and its fix.

## No answer from the Server

```text
udpstp: no answer from 192.0.2.10:24601 within 3 s
```

The Client exits with status 200. A Server doesn't answer a request that fails authentication, so a wrong key, a wrong keyId and a blocked port all look the same. Check in this order:

1. Check the SERVER address and port in your command. The Standard Port is UDP 24601 unless the Server's administrator changed it.
2. Check the [firewall rules](guides/hosting-a-server.md#open-the-firewall). Setup Requests must reach the Standard Port, and their replies must reach the Client. After Setup, each Test Connection uses the Ephemeral Port the Server assigned.
3. Check that the Shared Key and keyId match the Server's Key Table, and that the key's lifetimes allow this Test. A key file that loads locally can still fail the Server's authentication check.
4. Synchronize both hosts' clocks. They must be within 5 s of each other for authenticated requests and responses to pass verification.

If it still fails, ask the Server's administrator to turn on [Troubleshooting Mode](guides/hosting-a-server.md#debug-a-failing-client) for a while. The Client then shows the reason and exits with status 107:

```text
udpstp: 192.0.2.10:24601: the Setup exchange didn't complete within 3 s
udpstp: an unverified Setup Response said: authentication failed; check the key
  and keyId
```

If you use `--checksum` and the Client reports that a response failed its header checksum, try without `--checksum`.

On Windows, a Server needs its own inbound firewall rule. [Try a Server in a window](guides/windows.md#try-a-server-in-a-window) adds it, and [If someone cancelled a firewall dialog](guides/windows.md#if-someone-cancelled-a-firewall-dialog) removes a Block rule that overrides it.

## The key file is refused

```text
udpstp: key file error: keys.toml: readable or writable by group or others (mode
  644); make it private, e.g. chmod 600
```

Both roles refuse a key file that other accounts can read, with exit status 51, before reading any key. On Linux and macOS, run `chmod 600 keys.toml`. On Windows, follow [Key file permissions](guides/windows.md#key-file-permissions).

Other key file errors also exit with status 51. Check the file, row or line named in the message. A missing or unreadable file has no line number. [Key files](key-files.md) describes the format.

## No key to authenticate with

```text
udpstp: configuration error: no key to authenticate with; give one with
  --key-file, or --unauthenticated for a Server that allows Unauthenticated Mode
```

The Client exits with status 50. Give it the key file the Server's administrator shared, as in [Get a key file](guides/running-a-test.md#get-a-key-file).

## Address privacy settings are incomplete

Both roles exit with status 50 when address privacy settings do not fit together:

| Message | What to do |
|---|---|
| `--address-privacy pseudonymize needs --address-privacy-key-file FILE` | Give a pseudonym key file containing at least 32 hexadecimal digits. This key controls address pseudonyms, separately from the Shared Key used to authenticate the Test |
| `--address-privacy-key-file requires --address-privacy pseudonymize` | Set `--address-privacy pseudonymize`, or remove `--address-privacy-key-file` |

In a Server configuration file, use `address_privacy = "pseudonymize"` and `address_privacy_key_file`. [Protect Client addresses](guides/hosting-a-server.md#protect-client-addresses) shows how to create the key.

## The Client's Test Connection counts are invalid

The Client exits with status 50 and shows the effective values, including defaults:

| Message | What to do |
|---|---|
| More than 255 Servers | Give at most 255 SERVERs |
| `--test-connections` is below the number of Servers | Increase it to the number shown in the error, or give fewer SERVERs |
| `--min-test-connections` exceeds `--test-connections` | Lower the minimum to the effective count shown in the error, or increase `--test-connections` |
| `--max-bandwidth` is too small for the Test Connections | Use at least 1 Mbps per Test Connection, or reduce `--test-connections` |

## A name lookup fails for `false`

Join a boolean value to its option with `=`, such as `--jumbo=false`. With `--jumbo false`, the Client treats `false` as a SERVER name. A failed lookup exits with status 75.

## The Server probably doesn't support mode 2

```text
udpstp: SERVER probably doesn't support mode 2: it rejected it, unverified; use --auth-mode 1 (the Client never falls back by itself)
```

OB-UDPST 9.1 doesn't support mode 2. Add `--auth-mode 1`, or `--mode-1-server SERVER` for each such Server of several. [Choose the Authentication Mode](guides/running-a-test.md#choose-the-authentication-mode) has the table.

## The Server refused the Test

| Status | Response | What to do |
|---|---|---|
| 102 | BADVER | Use a Client and Server that speak the same protocol version |
| 103, 111 | BADJS, BADTMTU | Match the Server's `--jumbo` or `--traditional-mtu` setting |
| 109 | NOMAXBW | Set `--max-bandwidth` |
| 110 | CAPEXC | Retry later, or ask for less with `--max-bandwidth` |
| 113 | CONNFAIL | Retry later. The Server is full, or you have reached its limit of Tests or your Quota. A second Test from your address while one runs gets this too |
| 152 | BADPARAM | Often a Fixed-Rate Test without an Operator Key, or a Test that no longer fits the Server's limits |

## Other exit statuses

| Status | What to do |
|---|---|
| 50 | Follow the option or setting named in the message |
| 51 | Fix the named key file, row or line. See [The key file is refused](#the-key-file-is-refused) for access errors |
| 75 | Read the startup error. For a failed name lookup, check SERVER. For a socket error, check the address and port |
| 107, 108, 157, 158 | Check the key, the keyId and both clocks |
| 200 | Before activation, see [No answer from the Server](#no-answer-from-the-server). If the Watchdog ended an active Test after 3 s with nothing received, check the path and Server. Any results cover only what was measured |
| 201 | Ask for a value the Server allows, or drop `--refuse-coercion` |
| 202 | Report it, with both ends' versions |
| 203 | Use Servers with the same configuration for one Multi-Connection Test |

[Exit statuses](interfaces.md#exit-statuses) lists them all.

## The Test completed with warnings

A Test that completed with warnings still exits with status 0. Each warning says what limited the result.

| Warning | What to do |
|---|---|
| No Sub-Interval met the Performance Criterion | Compare the Highest IP-Layer Capacity and the Loss Ratios. A bottleneck with a shallow buffer loses a few percent all along, and `--max-loss-ratio 0.1` accepts that |
| A Sub-Interval before or after the Maximum's ran faster, or the last one much slower | Look at the Loss Ratios after the Maximum's Sub-Interval. A few percent at a steady capacity is the search cycling around the bottleneck, and `--max-loss-ratio 0.05` accepts it. Through CAKE, use enough `--test-connections` that each carries under 1 Gbit/s. If the path slowed, test again |
| Datagrams discarded by this host before udpstp read them: its receive buffer overflowed (Receive Buffer Overflows) | The loss happened on this host, not on the path. Use a faster host, or fewer Test Connections per host. If the OS also [held the socket buffers below the size asked for](#the-os-held-the-socket-buffers-below-the-size-asked-for), raise them |
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

[What the warnings mean](understanding-results.md#what-the-warnings-mean) explains each one.

## The OS held the socket buffers below the size asked for

```text
udpstp: warning: the OS holds socket buffers below the 2097152 bytes asked for (receive …, send …); at high rates datagrams may be dropped locally, so raise the net.core.rmem_max and net.core.wmem_max sysctls
```

At high rates, small socket buffers drop datagrams on the host, which looks like loss on the path. udpstp asks for 2 MiB and warns when the OS grants less. On Linux, raise the limits, then check the "This host" line of the next Test:

```sh
sudo sysctl -w net.core.rmem_max=4194304 net.core.wmem_max=4194304
```

To keep the change across restarts, put it in a file under `/etc/sysctl.d/`. On macOS and FreeBSD the limit is `kern.ipc.maxsockbuf`.

## The Server warns when it starts

```text
udpstp: warning: no key file and Unauthenticated Mode is off, so the Server can
  admit no Test; give one with --key-file, or allow unauthenticated Tests
```

Start the Server with `--key-file`, as in [Add keys](guides/hosting-a-server.md#add-keys).

```text
udpstp: warning: max_hops (--max-hops) is not set, so test traffic has a TTL or
  hop limit of 64; RFC 9946 §10 says the operator MUST set it from the expected
  path length
```

Set `--max-hops` a little above the number of hops to your farthest Clients, as in [Set MaxHops](guides/hosting-a-server.md#set-maxhops).

## The Server stops with a configuration error

```text
udpstp: configuration error: ephemeral_ports: "50000-50099" has 100 ports, fewer
  than max_test_connections (256) plus the Unactivated Test Connection share
  (128)
```

An unknown key or an invalid value stops the Server with exit status 50 and names the setting. Check a configuration before you start the Server with `udpstp server --config FILE --print-config`. [Server configuration](../crates/udpstp/src/config.rs) lists every setting.

| Setting or option | What to change |
|---|---|
| `ephemeral_ports` or `--ephemeral-ports` | Use `FIRST-LAST`, such as `50000-50383`, with ports from 1 to 65535 and FIRST no greater than LAST. The range must also hold `max_test_connections` plus the Unactivated Test Connection share. The defaults need 384 ports |
| `source_setup_burst` or `--source-setup-burst` | Set at least `tests_per_source` multiplied by `test_connections_per_test`. With two Tests per Source and 24 Test Connections per Test, the minimum is 48 |
| `source_tests_per_quota_period` or `source_bytes_per_quota_period` | Set at least 1. To remove that Quota, leave its setting out of the configuration file and omit its command-line override. Zero does not disable a Quota |

## The Windows service won't start

The service's log in `C:\ProgramData\udpstp\logs` says why, and `sc.exe query udpstp-server` shows its exit status as `SERVICE_EXIT_CODE`. [Run the Server as a service](guides/windows.md#run-the-server-as-a-service) lists the statuses and the fix for error 1069.

## Ask for help

If none of this fits, [open an issue](https://github.com/pederbe/udpstp/issues). Include your operating system, the output of `udpstp --version`, the command, and its messages and warnings. Keep your key private.
