# Tokio runs the Control Phase; nothing precise runs on Tokio timers

The Control Phase (listening on the Standard Port, Setup and Activation exchanges, idle ephemeral sockets, second-scale watchdogs) is many mostly-idle I/O tasks, which is what an async runtime is good at, so it runs on Tokio. The Data Phase is the opposite: a few Test Connections each moving up to about 1 Mpps with 100 µs send intervals, where the rate comes from batched I/O (sendmmsg/GSO, recvmmsg) and precision from the timer. Tokio's timer is 1 ms-granular by design and measured more than 1 ms late, so no pacing, Status PDU timer or other precise deadline ever runs on Tokio timers.

The Data Phase runs in a separate engine behind a seam: Tokio hands over the socket, Derived Keys and the sans-IO core instance, and the engine executes the core's Send Plans at the core's deadlines. Which engine model to use (a dedicated thread per Test Connection with sleep-then-spin pacing, or one pinned single-threaded runtime per core with a high-resolution timer) is chosen by a prototype with pass criteria fixed before it is built, including a Server-scale case of about 200 connections at 50 Mbps. Kernel pacing (SO_TXTIME with the fq qdisc) needs root and a non-default qdisc, so it is opt-in only. A follow-up ADR records the chosen model and its measurements.

## Consequences

- Nothing the Control Phase does may block the Data Phase Engine, and the engine must release admission state on every exit path, including a panic.
- Because the core is sans-IO, changing the engine model later touches only the runtime layer.
