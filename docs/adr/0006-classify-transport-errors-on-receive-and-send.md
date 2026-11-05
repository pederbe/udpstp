# Transport errors are classified on both receive and send, not suppressed

ICMP-driven errors reach UDP sockets on every OS, and on Linux they reach the send path too: once a Test Connection's socket is connected (after the first authenticated Activation Request), a pending ICMP error makes the next `send`/`sendmmsg` fail and the whole batch is lost. Windows can disable its port-unreachable reset only through an ioctl that no vetted crate exposes except `noq-udp`, and that crate's switch also disables `WSAENETRESET`, Windows' only time-exceeded signal. So instead of suppressing errors, every receive and send path on every OS runs one classifier: would-block and interrupted are retried; ICMP-type errors (connection refused or reset, host or network unreachable, Windows 10052) are counted and ignored, and on the send path the same batch is retried once because the failed call consumed the error; `EMSGSIZE` becomes a path-MTU event and time-exceeded a hop-limit event, both passed to the core so results can report them (RFC 9946 §5, §10; spec story 32); only local errors (bad socket, invalid argument) are fatal.

## Consequences

- On Linux an off-path attacker who spoofs port-unreachable messages matching a connected Test Connection can still cost batches; the send retry limits the damage, and ICMP errors seen during the Data Phase are flagged in the results so a biased capacity is visible.
- Time-exceeded is read from the socket error queue on Linux (IP_RECVERR, drained so it cannot fill the receive buffer), from 10052 on Windows, and is not available on macOS, where the socket doesn't report it apart from other errors.
- `noq-udp`'s Windows reset switch is not used.
- The core gains an input for transport errors on a socket.
- On Linux a pending error is returned before datagrams already queued; on Windows in arrival order. Neither loses queued datagrams.
