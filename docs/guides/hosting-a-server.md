# Hosting a Server

`udpstp server` answers Clients' Tests from the far end of a path. Four steps get it running, and a few more make it safe to open to Clients outside your own network. [Server configuration](../../crates/udpstp/src/config.rs) lists every setting, [Server log](../server-log.md) every event, and the [security model](../security.md) explains the defaults. On Windows, follow [udpstp on Windows](windows.md) alongside this guide.

The [required setup](#required-setup) gets a Server running. A Server that Clients outside your own network can reach should also take the [recommended steps](#recommended-for-a-public-server). Each [optional step](#optional-steps) says when you need it.

## Required setup

Every Server needs these four steps, in this order. Install udpstp first, as in [Installing udpstp](installing-udpstp.md).

### Add keys

The Server authenticates Clients with its Key Table. It reads the table once at startup, so restart the Server after you change it.

1. Generate a random Shared Key of at least 32 bytes:

   ```sh
   openssl rand -hex 32
   ```

   On Windows, use [PowerShell](windows.md#generate-a-shared-key).

2. Write a TOML Key Table with one `[[key]]` row per key:

   ```toml
   # Consumers: one key per population of Clients.
   [[key]]
   admin_key_name = "customers-2026"
   key_id = 1
   key_hex = "5b0c0e8d6f7a41c2b9d3e4f5061728394a5b6c7d8e9fa0b1c2d3e4f506172839"
   accept_lifetime_end = "20270101000000Z"
   send_lifetime_end = "20270101000000Z"
   tests_per_quota_period = 12

   # Next year's key, added in advance so no restart is needed.
   [[key]]
   admin_key_name = "customers-2027"
   key_id = 3
   key_hex = "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0"
   send_lifetime_start = "20261231000000Z"
   accept_lifetime_start = "20261231000000Z"

   # The NOC's Operator Key.
   [[key]]
   admin_key_name = "noc"
   key_id = 200
   key_hex = "c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4"
   operator = true
   max_test_interval = 3600
   ```

   Don't use these example keys. [Key files](../key-files.md) describes every field.

3. Make the file private. On Linux and macOS it must be mode `0600` or stricter, owned by the account running udpstp or by root, in a folder only its owner can write:

   ```sh
   chmod 600 keys.toml
   ```

   On Windows, follow [Key file permissions](windows.md#key-file-permissions).

4. Give each Client population its key and keyId over a secure channel.

Use a separate key for each population of Clients, and separate keys for Operators. To retire a key on schedule, set its `accept_lifetime_end` and `send_lifetime_end`. To revoke it at once, remove its row and restart the Server, which also ends the Tests running then.

### Open the firewall

| Traffic | Port | Setting |
|---|---|---|
| Setup Requests and Responses | UDP 24601, the Standard Port | `port` |
| Each Test Connection's traffic | One Ephemeral Port each, picked at random | `ephemeral_ports`, 49152-65535 by default |

What to open depends on the firewall:

- **A stateful firewall** on the Server's host or in a cloud security group needs only the Standard Port open inbound.
- **A stateless filter**, such as a router ACL, needs the Standard Port and the whole `ephemeral_ports` range.
- **A Server behind NAT** needs the Standard Port and the whole range forwarded to it, with the port numbers unchanged.
- **At high packet rates**, you may prefer to exempt Test traffic from connection tracking (nftables `notrack`) and allow the range as for a stateless filter.
- **Windows Defender Firewall** needs the whole range. See [udpstp on Windows](windows.md#try-a-server-in-a-window).

To keep a firewall rule small, narrow the range. It must hold at least `max_test_connections` plus the Unactivated Test Connection share, 384 ports with the defaults:

```sh
udpstp server --key-file keys.toml --ephemeral-ports 50000-50383 --max-hops 12
```

To listen on specific addresses instead of all of them, set `listen` (or repeat `--listen ADDRESS`).

### Set MaxHops

RFC 9946 says the operator MUST set MaxHops, the TTL or hop limit of Test traffic, so that load can't stray beyond the intended path. Until you set `max_hops`, the Server uses 64 and warns at every start:

```text
udpstp: warning: max_hops (--max-hops) is not set, so test traffic has a TTL or hop limit of 64; RFC 9946 §10 says the operator MUST set it from the expected path length
```

Set it a little above the number of hops to your farthest Clients, which `traceroute` shows. Clients set their own with `--max-hops`.

Keep `dont_fragment` on (the default), so a packet larger than the Path MTU is lost rather than fragmented. Turn it off only to measure a path with fragmentation.

### Start the Server

Run the Server with your key file and MaxHops:

```sh
udpstp server --key-file /etc/udpstp/keys.toml --max-hops 12
```

The Server listens on UDP 24601, the Standard Port, on every IPv4 and IPv6 address, and runs in the foreground. It needs no root privileges or capabilities.

Without a key file the Server can admit no Test, and warns at startup:

```text
udpstp: warning: no key file and Unauthenticated Mode is off, so the Server can admit no Test; give one with --key-file, or allow unauthenticated Tests
```

## Recommended for a public Server

For a Server that Clients outside your own network can reach.

### Limit what Clients can use

A Server open to the internet should cap the load it agrees to carry. The defaults already allow one Test at a time per Source (an IPv4 address or an IPv6 /64) and budget Setup Requests.

1. **Cap the admitted rate.** Set `admitted_rate_cap`, in Mbps for each direction, together with `require_max_bandwidth`. Without it, Tests that state no maxBandwidth don't count against the cap, and the Server warns:

   ```text
   udpstp: warning: admitted_rate_cap is set but require_max_bandwidth is off: Tests that state no maxBandwidth are admitted without counting
   ```

2. **Set Quotas** if you want to limit how often each Source or key tests. Quotas are off by default and refill evenly over the `quota_period` (3600 s by default):

   ```sh
   udpstp server --key-file keys.toml --max-hops 12 --source-tests-per-quota-period 24
   ```

   Set a key's own Quotas with `tests_per_quota_period` and `bytes_per_quota_period` in its row. Operator Keys are exempt. Use is counted in memory, so a restart forgets it.

3. **Give each population of Clients its own key**, and keys only to Clients you trust. The Server shares its capacity fairly between keys under a flood, but it can't tell one key's Clients apart.

Keep the other limits at their defaults unless you have a reason to change them. [Server configuration](../../crates/udpstp/src/config.rs) lists each one, and the [security model](../security.md#admission-under-load) explains how they hold up under a flood. A Client refused by a limit sees CONNFAIL, CAPEXC or NOMAXBW and exits with status 113, 110 or 109.

### Protect Client addresses

The log shows each Client's address in full by default, for abuse handling. Client addresses are personal data in many jurisdictions. Set `address_privacy` to show less:

| Value | The log shows |
|---|---|
| `full` | The address and port, such as `127.0.0.1:51801` |
| `truncate` | The IPv4 /24 or IPv6 /48, without the port |
| `pseudonymize` | `anon-` and 8 bytes of HMAC-SHA-256 over the IP address, such as `anon-63d4269f2e4b0c0d` |

`pseudonymize` needs a key in `address_privacy_key_file`: hexadecimal digits for 16 bytes or more. A Client keeps the same pseudonym for as long as you keep the key:

```sh
openssl rand -hex 32 > /etc/udpstp/pseudonym.key
udpstp server --key-file keys.toml --max-hops 12 --address-privacy pseudonymize --address-privacy-key-file /etc/udpstp/pseudonym.key
```

### Run it as a service

Whatever runs the Server, give it:

- a dedicated, unprivileged account that alone can read the key file
- core dumps disabled, for example with `ulimit -c 0`, since a dump can hold the Key Table
- a log system that rotates its standard output
- a clock synchronised with NTP, since clocks more than 5 s apart make every authenticated Test fail

#### With systemd

The supplied unit requires systemd 247 or later for credential loading.

[`contrib/systemd/udpstp-server.service`](../../contrib/systemd/udpstp-server.service) is a hardened unit. It runs the Server as a user systemd creates when it starts, with no capabilities and a read-only view of the system. It loads the Key Table as a credential and makes a mode-0400 copy in its private runtime directory. The source in `/etc/udpstp` stays root's, and the runtime copy is removed when the unit stops. The apt and dnf packages install it for you.

```sh
sudo install -m 755 target/release/udpstp /usr/local/bin/udpstp
sudo install -d -m 755 /etc/udpstp
sudo install -m 600 keys.toml /etc/udpstp/keys.toml
sudo install -m 644 server.toml /etc/udpstp/server.toml
sudo install -m 644 contrib/systemd/udpstp-server.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now udpstp-server
journalctl -u udpstp-server -f
```

`server.toml` is your configuration file. Leave `key_file` out of it, since the unit passes the key file itself. To bind a port below 1024, add `AmbientCapabilities=CAP_NET_BIND_SERVICE` and `CapabilityBoundingSet=CAP_NET_BIND_SERVICE`.

#### In a container

Each release publishes the image `ghcr.io/pederbe/udpstp:<version>`, which runs the Server as a non-root user (uid 65532) with no shell. Pin a version and upgrade on purpose. To build the image yourself:

```sh
podman build -f contrib/container/Containerfile -t udpstp .
```

Run it with the host's network (`--network host`). A container network puts NAT and a bridge in front of the Server, so results would describe the container's plumbing rather than the path. Pass the Key Table as a secret owned by uid 65532:

```sh
podman secret create udpstp-keys keys.toml
podman run -d --name udpstp --network host \
    --secret udpstp-keys,target=keys.toml,uid=65532,mode=0400 \
    ghcr.io/pederbe/udpstp:<version> --max-hops 16
```

Arguments after the image name are added to `udpstp server`'s and override the image's configuration file, `/etc/udpstp/server.toml`. Or mount your own file there. With Docker, pass the key file as a Compose secret, or bind-mount a file owned by uid 65532 with mode 0400. `podman stop` and `docker stop` stop the Server cleanly within 2 s.

#### On Windows

Run an unattended Server as the Windows service `udpstp-server`. See [Run the Server as a service](windows.md#run-the-server-as-a-service).

## Optional steps

Each step says when you need it. Skip the rest.

### Write a configuration file

**When:** you have more than a few settings, or want them kept in one file.

Every setting has a command-line flag and a TOML key of the same name, so `--max-hops` is `max_hops`. Flags replace the file's values, and the file replaces the defaults.

Write boolean overrides with `=`, such as `--jumbo=false`. In TOML, write `jumbo = false`.

1. Print the effective configuration to start a file:

   ```sh
   udpstp server --print-config > /etc/udpstp/server.toml
   ```

   Or write only what differs from the defaults:

   ```toml
   listen = ["192.0.2.10", "2001:db8::10"]
   key_file = "/etc/udpstp/keys.toml"
   ephemeral_ports = "50000-50383"
   max_hops = 12
   admitted_rate_cap = 10000
   require_max_bandwidth = true
   source_tests_per_quota_period = 24
   log_format = "json"
   address_privacy = "truncate"
   ```

2. Use absolute paths. A relative `key_file` is taken from the directory the Server starts in, not the file's.
3. Check the file, keys included:

   ```sh
   udpstp server --config /etc/udpstp/server.toml --print-config
   ```

4. Start the Server with it:

   ```sh
   udpstp server --config /etc/udpstp/server.toml
   ```

An unknown key or an invalid value stops the Server with exit status 50 and names the setting:

```text
udpstp: configuration error: ephemeral_ports: "50000-50099" has 100 ports, fewer than max_test_connections (256) plus the Unactivated Test Connection share (128)
```

[Troubleshooting configuration errors](../troubleshooting.md#the-server-stops-with-a-configuration-error) explains how to correct port ranges, Setup Request budgets and Quotas.

Keep `jumbo`, `traditional_mtu` and `server_mtu` at their defaults to serve OB-UDPST Clients as well as udpstp ones. A Client whose `--jumbo` or `--traditional-mtu` differs is refused with BADJS or BADTMTU.

### Raise the socket buffers

**When:** the Server warns that the OS holds socket buffers below the size asked for.

At high rates, small socket buffers drop datagrams on the host, which looks like loss on the path. The Server asks for `socket_buffer_size`, 2 MiB by default, and warns at startup when the OS grants less:

```text
udpstp: warning: the OS holds socket buffers below the 2097152 bytes asked for (receive …, send …); at high rates datagrams may be dropped locally, so raise the net.core.rmem_max and net.core.wmem_max sysctls
```

On Linux, raise the limits to at least `socket_buffer_size`, and make the change permanent in a file under `/etc/sysctl.d/`:

```sh
sudo sysctl -w net.core.rmem_max=4194304 net.core.wmem_max=4194304
```

On macOS and FreeBSD the limit is `kern.ipc.maxsockbuf`.

### Give it enough CPUs

**When:** the Server carries high rates or many Tests at once.

On Linux the Server runs one Engine Thread for each CPU it may use, each serving many Test Connections. Give it whole CPUs, and confine it with the service's CPU affinity, such as systemd's `CPUAffinity=` or `taskset`. A container's fractional CPU limit is rounded down.

For high-rate Multi-Connection Tests, give the Server at least as many CPUs as such a Test has Test Connections.

A Server that falls behind says so in its `TestSummary` log lines, with `skipped_bursts` when it couldn't send on time and `receive_buffer_overflows` for its Receive Buffer Overflows.

### Debug a failing Client

**When:** a Client gets no answer and you suspect its key.

By default a Setup Request that fails authentication gets no answer, so a Client with the wrong key sees only a timeout. To find out why, turn on Troubleshooting Mode for a while:

```sh
udpstp server --key-file keys.toml --max-hops 12 --troubleshooting
```

The Client then reports the reason:

```text
udpstp: an unverified Setup Response said: authentication failed; check the key and keyId
```

Turn it off when you're done. While it is on, anyone can learn whether the Server has keys, and spoofed requests get replies.

### Allow Tests without a key

**When:** on a lab network where every host is trusted, for Clients without a key.

Only on a network where every host is trusted, such as a lab, set `allow_unauthenticated` to accept Tests without a key, as default OB-UDPST installations do. It protects nothing, so anyone who can reach the Server can start a Test. The Server still accepts authenticated Tests.

### Read the log

**When:** you want to follow Tests, or feed the log to another system.

The Server writes one line per event to standard output, human-readable by default. Set `log_format = "json"` for one JSON object per line:

```text
2026-11-10T17:45:22.533499Z INFO Test Connection admitted
  | event="TestConnectionAdmitted" test_connection_id=1 client="127.0.0.1:45728"
  | mc_ident=3934 key_id=1
2026-11-10T17:45:33.074158Z INFO Test ended: Completed event="TestSummary"
  | client=127.0.0.1:45728 mc_ident=3934 key_id=1 test_connections=1
  | direction="Downstream" test_interval=10 sub_interval=1000 trial_interval=50
  | low_thresh=30 upper_thresh=90 seq_err_thresh=10 slow_adj_thresh=3
  | high_speed_delta=10 ignore_ooo_dup=true one_way_delay_variation=true
  | random_payload=false rate_adj_algo="B" dscp=0 sr_index_conf=65535
  | sr_index_is_start=false bytes_sent=4089512844 bytes_received=4125335714
  | ip_layer_sender_bit_rate=3335136496 skipped_bursts=5 skipped_datagrams=102
  | skipped_bytes=145644 withheld_bursts=0 withheld_datagrams=0 withheld_bytes=0
  | receive_buffer_overflows=0 result="Completed"
```

Filter on the `event` name and its fields, not on the message. Under a service manager, leave the log on standard output and let it rotate the log. Otherwise set `log_file` to write a new file each day, kept for 14 days, in a folder of its own. [Server log](../server-log.md) lists every event, field and reason.
