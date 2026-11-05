# Getting started

udpstp measures how much traffic a network path can carry, in Mbps. Put a Client and a Server at the two ends of the path, share a key, and run your first Test.

## Install udpstp

Install udpstp on both hosts using the [installation guide](guides/installing-udpstp.md).

## Choose two hosts

Run the Server at the far end of the path you want to measure, and the Client where you want to test from. Both hosts need UDP connectivity. Testing on one machine checks the setup but doesn't measure a network path.

Choose a [MaxHops value](guides/hosting-a-server.md#set-maxhops) slightly above the number of routers between the hosts; the examples below use 12.

## Share a key

Tests are authenticated by default. Generate a random Shared Key with `openssl rand -hex 32`, or use the [Windows instructions](guides/windows.md#generate-a-shared-key). Save the following as `keys.toml` on both hosts, replacing the placeholder with that same key:

```toml
[[key]]
admin_key_name = "first-test"
key_id = 1
key_hex = "replace-with-64-hexadecimal-digits"
```

Keep the file private. On Linux and macOS:

```sh
chmod 600 keys.toml
```

On Windows, [set the key file's permissions](guides/windows.md#key-file-permissions) before continuing. Keys stay in the file, never on the command line.

## Start the Server

Allow inbound UDP ports **24601** and **49152–65535** at the Server. [Open the firewall](guides/hosting-a-server.md#open-the-firewall) covers other setups. On Windows, follow [these steps](guides/windows.md#try-a-server-in-a-window) before starting it.

As an ordinary account on the Server host:

```sh
udpstp server --key-file keys.toml --max-hops 12
```

## Run your first Test

On the Client host, replace `server.example.net` with your Server's name or address:

```sh
udpstp client --key-file keys.toml --max-hops 12 server.example.net
```

This runs a 10-second Downstream Test: the Server sends the traffic. Add `--direction upstream` to measure the other direction. The result shows the Maximum IP-Layer Capacity in Mbps and any warnings. [Understanding the results](understanding-results.md) explains what they mean.

On Windows, the Client normally needs no inbound firewall rule. A Server needs [its own rule](guides/windows.md#try-a-server-in-a-window), added by an administrator.

## Keep exploring

- [Running a Test](guides/running-a-test.md): options, JSON output and results.
- [Hosting a Server](guides/hosting-a-server.md): configuration and unattended operation.
- [Share feedback](https://github.com/pederbe/udpstp/issues): include your OS, `udpstp --version`, command and warnings, keeping your key private.
