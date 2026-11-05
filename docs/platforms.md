# Platform differences

udpstp runs on Linux, macOS and Windows. A few features depend on what the operating system offers. [Installing udpstp](guides/installing-udpstp.md#platforms-and-installation-options) lists the release binaries.

| Feature | Linux | macOS | Windows |
|---|---|---|---|
| Don't Fragment (DF) on Test traffic | Set | Set | Not yet: both ends warn |
| A packet too large for the path | Lost, and reported as the Path MTU | Lost | Fragmented, since DF isn't set |
| DSCP marking | Yes | Yes | No: the Client asks for DSCP 0, and the Server changes every Test's DSCP to 0, each with a warning |
| Key file check | Mode `0600` or stricter, owned by the account or root | As Linux | Only the account, SYSTEM and Administrators may have access ([details](guides/windows.md#key-file-permissions)) |
| The Server's Data Phase Engine | A pool of Engine Threads, one per CPU it may use, each serving many Test Connections | One Engine Thread per Test Connection | One Engine Thread per Test Connection |
| Fast paths | Batched sending (`sendmmsg`), UDP GSO, batched receiving (`recvmmsg`), kernel receive timestamps, waking on arrival | None | None |
| Receive Buffer Overflows counted | Yes | No | No |
| Socket buffer limit to raise | `net.core.rmem_max`, `net.core.wmem_max` | `kern.ipc.maxsockbuf` | — |
| An interrupted Client exits with | The signal's status, such as 130 for Ctrl-C | The signal's status | 204 |
| Unattended Server | systemd unit, container image | Your own service manager | Windows service |
| Firewall for a Server | A stateful firewall needs only the Standard Port | Not tested | Windows Defender Firewall needs the whole Ephemeral Port range |

## Not yet measured

The Server's pool of Engine Threads has been measured carrying 150 Test Connections of 4 Mbit/s each on one CPU core. These haven't been measured yet:

- 1,000,000 Load PDUs a second on one Server, or 10 Gbit/s in all;
- Test Connections of 500 Mbit/s or more each sharing an Engine Thread. Give a Server that carries high-rate Multi-Connection Tests at least as many CPUs as such a Test has Test Connections.
