# Operator features

For Operators measuring within their own networks, and for the Server administrators who give them Operator Keys.

## Operator Keys

An Operator Key is a Shared Key that the Server's Key Table marks as belonging to an Operator. The Server decides: a Test gets an Operator's allowances only when its Setup Request verified with a row marked `operator = true`, whatever the Client claims.

On the Server, in a TOML Key Table ([Key files](../key-files.md)):

```toml
[[key]]
admin_key_name = "noc"
key_id = 200
key_hex = "c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4"
operator = true
max_test_interval = 3600
```

`max_test_interval` is optional: without it, an Operator Key's Tests are limited to 60 s like everyone else's. An OB-UDPST `udpst.keys` file can't mark Operator Keys.

On the Client, the key file needs only the key; `operator` there changes nothing:

```toml
[[key]]
admin_key_name = "noc"
key_id = 200
key_hex = "c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4"
```

Give Operator Keys their own keyIds, and never share them with Consumers.

## What an Operator Key allows

| Allowance | Without an Operator Key | With one |
|---|---|---|
| Fixed-Rate Tests (`--fixed-rate`) | Refused with BADPARAM | Allowed |
| Test Interval (`--test-interval`) | At most 60 s; a longer request is coerced to 60 | Up to the key's `max_test_interval` |
| Starting row of a search (`--start-row`) | At most `max_start_row`, row 100 by default | Any row |
| Share of the Admitted Rate Cap one Test may state | At most `max_test_share`, 50% by default | Up to the whole cap |
| Quotas of Tests and load bytes | Counted | Exempt |
| DSCPs the Test may mark its traffic with | `allowed_dscp`, only 0 by default | `operator_allowed_dscp`, every DSCP but 48 and 56 (network control) by default |
| Time to infer Congestion (slowAdjThresh × trialInt) | Lowered to 500 ms at most | Kept as requested |

The last row matters only to Clients that set slowAdjThresh or trialInt, which the udpstp Client doesn't.

An Operator Key is **not** exempt from authentication, `tests_per_source` (one Test at a time per Source by default), the Test Connection limits, the Admitted Rate Cap itself, `require_max_bandwidth`, or the Setup Request budgets.

## Fixed-Rate Tests

A Fixed-Rate Test holds one Sending Rate Table row for its whole Test Interval instead of searching. Use it to check that a path carries a known rate, for example a provisioned service rate.

```sh
udpstp client --key-file noc.toml --fixed-rate 100 server.example.net
```

Rows 1 to 1000 offer that many Mbps at the IP layer; rows 1001 to 1152 offer the rates above 1 Gbps. Row 0 offers about 0.1 Mbps. The results' header names the Test type:

```text
Results
  Downstream Fixed-Rate Test with 127.0.0.1:24701, started 2026-09-26T14:50:11Z (T0)
```

A Test with an ordinary key is refused:

```text
udpstp: the Server rejected the Activation Request; only an Operator Key may run a Fixed-Rate Test (RFC 9946 §4.1), so check the key, or the Server's log
```

with exit status 152; the Server logs `reason="FixedRateNeedsOperatorKey"`. `--fixed-rate` and `--start-row` can't be combined.

**Check the IP-Layer Sender Bit Rate.** A Fixed-Rate Test sends only as fast as the Load Sender can. Compare the B column, the Sender Bit Rate, with the row's rate. When it is below the row's rate, the host, not the path, set the result, and the Server's log shows `BurstsSkipped`.

## Long Test Intervals

An Operator Key with `max_test_interval` allows Tests longer than 60 s, up to that many seconds:

```sh
udpstp client --key-file noc.toml --test-interval 900 --delay-var rtt server.example.net
```

Above 60 s the Server judges by round-trip delay variation instead of one-way delay variation, which clock drift would distort over a long Test. Ask for it with `--delay-var rtt`; otherwise the Server coerces it and the Client says so:

```text
The Server set useOwDelVar to 0 (asked for 1)
```

A Test Interval beyond the key's maximum is coerced to it. Add `--refuse-coercion testIntTime` to have the Client end the Test instead (exit status 201).

## MaxHops

MaxHops, the TTL or IPv6 hop limit of test traffic, keeps a Test's load from straying beyond the path you mean to measure (RFC 9946 §10). Each end sets its own: the Server with `max_hops`, which RFC 9946 says the operator MUST set, and the Client with `--max-hops` (64 by default).

```sh
udpstp client --key-file noc.toml --max-hops 12 server.example.net
```

Set it a little above the number of hops on the path. Too small, and the load dies on the way: the Client reports "ICMP time-exceeded errors: --max-hops is too small for the path" instead of a low capacity, and the Server logs it.

## DSCP

To measure a specific traffic class, mark the Test's traffic with its DSCP:

```sh
udpstp client --key-file noc.toml --dscp 46 server.example.net
```

The Server allows a DSCP from its `operator_allowed_dscp` set for an Operator Key, and from `allowed_dscp` for anyone else. A DSCP outside the set is coerced to 0. The results' parameters line says which DSCP was agreed, such as `DSCP 46 agreed`.

A Server administrator narrows the Operator set with, for example:

```sh
udpstp server --key-file keys.toml --max-hops 12 --operator-allowed-dscp 0,10,18,26,34,46
```

The ECN bits are always Not-ECT. Windows doesn't let udpstp mark traffic ([Platform differences](../platforms.md)).

## Multi-Connection Tests at high rates

One Test Connection may not fill a multi-gigabit path. Spread the Test over several Test Connections, and over several Servers if one host can't send enough:

```sh
udpstp client --key-file noc.toml --test-connections 8 --max-bandwidth 10000 server-a.example.net server-b.example.net
```

[Running a Test](running-a-test.md#run-a-multi-connection-test) explains the options, and [Understanding the results](../understanding-results.md#multi-connection-results) how they are summed.
