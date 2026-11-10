# Quickstart

In a few minutes you will know how much traffic a network path can carry, in Mbps. You need udpstp [installed](guides/installing-udpstp.md) on two hosts, one at each end of the path.

## Choose two hosts

Run the Server at the far end of the path you want to measure, and the Client where you want to test from. Both hosts need UDP connectivity. Testing on one machine checks the setup but doesn't measure a network path.

Choose a [MaxHops value](guides/hosting-a-server.md#set-maxhops) slightly above the number of routers between the hosts. The examples below use 12.

## Share a key

Tests are authenticated by default. Generate a random Shared Key.

```sh
openssl rand -hex 32
```

On Windows, use the [PowerShell command](guides/windows.md#generate-a-shared-key) instead. Save the following as `keys.toml` on both hosts, replacing the placeholder with that same key.

```toml
[[key]]
admin_key_name = "first-test"
key_id = 1
key_hex = "replace-with-64-hexadecimal-digits"
```

Keep the file private. On Linux and macOS, run this on both hosts.

```sh
chmod 600 keys.toml
```

On Windows, [set the key file's permissions](guides/windows.md#key-file-permissions) before continuing. Keys stay in the file, never on the command line.

## Start the Server

Allow inbound UDP ports **24601** and **49152–65535** at the Server. [Open the firewall](guides/hosting-a-server.md#open-the-firewall) covers other setups. On Windows, follow [these steps](guides/windows.md#try-a-server-in-a-window) before starting it.

Start the Server as an ordinary account.

```sh
udpstp server --key-file keys.toml --max-hops 12
```

The Server keeps running in the foreground. Its first log lines include the addresses it listens on.

```text
2026-11-10T17:44:40.702489Z INFO listening on 0.0.0.0:24601 event="Listening"
  | address=0.0.0.0:24601
2026-11-10T17:44:40.702493Z INFO listening on [::]:24601 event="Listening"
  | address=[::]:24601
```

## Run your first Test

On the Client host, replace `server.example.net` with your Server's name or address.

```sh
udpstp client --key-file keys.toml --max-hops 12 server.example.net
```

This runs a 10-second Downstream Test, in which the Server sends the traffic. The Client prints a row for each second, then the result under **Final metrics**. Your figure depends on your path.

```text
Final metrics
  Maximum IP-Layer Capacity: 9713.89 Mbps (Sub-Interval 10)
```

Add `--direction upstream` to measure the other direction. [Understanding the results](understanding-results.md) explains every line and warning.

If the Client prints `no answer from` the Server instead, check the address, the firewall and the key, as [Troubleshooting](troubleshooting.md#no-answer-from-the-server) describes. On Windows, the Client normally needs no inbound firewall rule. A Server needs [its own rule](guides/windows.md#try-a-server-in-a-window), added by an administrator.

## Keep exploring

- [Running a Test](guides/running-a-test.md): options, JSON output and results.
- [Hosting a Server](guides/hosting-a-server.md): configuration and unattended operation.
- [Troubleshooting](troubleshooting.md): messages, exit statuses and warnings, each with its fix.
