# The Data Phase Engine: a pool of Engine Threads on a Linux Server, one per Test Connection elsewhere, with bounded catch-up

**Status: Accepted** (2026-09-23), amended by ADR 0011 and ADR 0012 for policing and catch-up, and ADR 0016 for wake-up grouping. Rewritten on 2026-10-01 to record the implemented pool model. The frozen Data Phase interface, bounded catch-up, over-rate policing, timer slack of 1 ns and the CLOCK_MONOTONIC_RAW Test Time Base stand as first decided.

The Data Phase prototype compared two engine models:

- **A:** a thread per Test Connection.
- **B:** one Engine Thread per core serving many Test Connections from one epoll loop.

The experimental wake-up grouping described below was retired by [ADR 0016](0016-retire-unused-features.md). Engine Threads now always wait for the earliest deadline, and startup logs omit the grouping window. The other decisions remain in force.

Each was measured with and without a spin tail, on a WSL2 host, in two rounds.

**The models differ at Server scale.** On a single Test Connection they are indistinguishable. With A confined to the same four cores, B:

- skipped 0.006 % of datagrams on 200 × 50 Mbps and 0.4 % on 20 × 500 Mbps, where A skipped 2.6 % and 12.7 %;
- used 20–30 % less thread CPU;
- ran 4 threads instead of 20–200.

A was better where high-rate Test Connections shared a core: on eight 1.25 Gbps Test Connections, and on one core carrying a 1 Mpps receive beside a sender, where B protected the receive and its sender skipped more.

**The first implementation ran A everywhere** (one thread per Test Connection, with batched sends and receives and a `ppoll` wait on its socket), since on a single Test Connection the models tie. On a real host it fell behind. In the Server-capacity check, at a 500-byte Load PDU every 1 ms per Test Connection, confined to one core's two logical CPUs, udpstp's Server carried 125 Test Connections Downstream where OB-UDPST's carried 150: at 150 it left 6 % of its Load PDUs unsent with both logical CPUs busy and about 143,000 wake-ups a second. A profile put its extra cycles in switching among its threads, entering system calls and reading clocks.

**With the pool, the same check read "not behind" in both directions**. Downstream on the same two logical CPUs, udpstp's Server carried 150, the check's top, on 0.86 busy logical CPUs and about 78,000 wake-ups a second; Upstream on one logical CPU it carried 150 on 0.34, where the thread per Test Connection had needed 0.93 and about 161,000 wake-ups a second. A single-Test precision check, one Test Connection Downstream at rows 500 and 900, found the pool no worse than a thread per Test Connection in Skipped and Withheld Load PDUs, Sub-Interval shortfall, send lag or Burst lateness.

**On a Linux Server we use a pool of Engine Threads:**

- as many as the CPUs the process may use (`std::thread::available_parallelism`, which counts its CPU affinity and cgroup quota), or as an experimental override sets; started as Test Connections arrive; unpinned, so a Server administrator places them with the service's CPU affinity;
- a new Test Connection goes to the Engine Thread serving the fewest, and stays there;
- every socket of an Engine Thread in one edge-triggered epoll set; a deadline heap; a ready list served at most 256 datagrams per Test Connection per turn, so one flooded socket can't starve pacing; sends before reads in every turn, and a turn's sends bounded to 1 ms; commands taken every turn, and readiness collected in every turn that runs without waiting, so a saturated Engine Thread still reads;
- the wait is `ppoll` on the epoll fd and an eventfd, to the earliest deadline at nanosecond resolution, waking exactly at each deadline by default;
- the receive hold-off is the Engine Thread's: after a turn that read, when more than one datagram is due within it, arrivals don't end the next wait until a fixed end: at most 250 µs, and less than any of its sockets' quarter of the time its receive buffer holds by its longest recent turn;
- the clock is read once as a pair when a turn starts, then the Test Time Base alone wherever the core judges or stamps time, and a pair after each read that returned datagrams, none around a read that returned nothing;
- the Engine Thread drains its sockets' error queues;
- timer slack of 1 ns; no spin tail and no idle polling. Both cost CPU at scale and the round-2 data showed no benefit.

**The Client, and every endpoint on Windows and macOS, run one Engine Thread per Test Connection** over the same per-Test-Connection code, reading the clock as they always have. A Client runs few Test Connections and needs precision at high rates, where A was the better model. Servers at scale run on Linux; Windows has no nanosecond readiness wait through a safe crate, and a macOS pool, which kqueue would allow, isn't worth building until a macOS Server needs scale.

**Receive uses header-only `recvmmsg` with `MSG_TRUNC`,** with per-datagram kernel timestamps and TOS. It met 1 Mpps on one physical core with the softirq on the sibling hyperthread. GRO stays optional: it couldn't be tested fairly on veth.

**A Transmitter that falls more than one interval behind sends only its most recent missed Burst.** _Amended by ADR 0012: a missed Burst less than the catch-up budget late is still sent, so Transmitters with a 100 µs interval catch up after a late timer; older Bursts are still skipped._

- Older Bursts are Skipped Bursts: counted, reported and deducted from the sender bit rate.
- Unbounded catch-up was rejected. That put up to 8 % more than planned into single Sub-Intervals, which inflates the RFC 9097 maximum on paths that aren't the bottleneck.
- SO_TXTIME kernel pacing is not adopted. It moves lpduTime about 250 µs away from departure, and adopting it would change lpduTime's meaning.

**The core's Data Phase interface is frozen** as the prototype's amended candidate:

- a heap-free Send Plan that holds at most one earlier Burst's Deferred Datagrams plus one new Burst;
- `write_datagrams` writes headers (and random payload when requested) into a runtime-owned buffer of twice the largest Burst;
- `report_sent` is the only backpressure channel;
- receive batches carry a clock pair and per-datagram kernel timestamps and TOS;
- Status PDUs carry their kernel receive time;
- transport errors include time-exceeded;
- there is a stop input and an event output;
- `poll_burst` keeps a `horizon` argument that must be 0 while kernel pacing is off.

The same contract suite passes against every executor: the simulator's, the per-Test-Connection executor and the pool, with one Test Connection and with 32 on one Engine Thread. It covers spacing, ordering, partial sends and the timestamp domain; the simulator's tests cover fresh lpduTime on Deferred Datagrams and bounded catch-up under stalls, and a counting allocator checks that 8 Tests on one Engine Thread allocate nothing in their steady state.

## Considered Options

- **A thread per Test Connection on every platform, tuned** (fewer clock reads and empty reads per wake-up). It is what ran first, and the Server-capacity check found it short on one core. Tuning it keeps one switch per Burst per Test Connection, which a shared Engine Thread avoids whenever several Bursts fall due together. The pool was chosen over a tuning pass, and the check then read "not behind" with it.
- **Pinned Engine Threads, one per core,** as first decided. The example systemd unit's `SystemCallFilter=~@privileged @resources` denies `sched_setaffinity` with SIGSYS, and no measurement showed pinning helping; the service's CPU affinity places an unpinned pool where the Server administrator wants it. Revisit with a measurement that shows migrations costing precision.
- **timerfd in the epoll set.** Each turn re-arms it, a system call more per wake-up than a `ppoll` timeout, and keeping arrivals from ending a wait during the hold-off would need `epoll_ctl` calls or a nested epoll set.
- **The epoll wait's own timeout.** `epoll_wait` takes milliseconds; `epoll_pwait2`, which takes nanoseconds, needs Linux 5.11, and the Linux builds run on 4.18 and later.
- **A hold-off per socket in a shared Engine Thread,** taking each socket out of the epoll set during its hold-off: two system calls per hold-off, and the other sockets still wake the thread.
- **Keeping heavy Load Receivers off pacing Engine Threads.** Sends go first in every turn and each Test Connection's reads are bounded, which protects pacing at the rates measured; a row changes every Trial Interval in a Search Test, so a rate at placement says little. The prototype's evidence stands: an Engine Thread saturated by receiving skips its Load Senders' Bursts. Revisit if one Engine Thread saturates while others idle.
- **A pooled Client.** Few Test Connections, and precision at high rates is where A measured better.
- **A pool on Windows and macOS.** Windows' readiness waits through mio round to milliseconds; macOS's kqueue would do, but no macOS Server needs the scale yet.
- **Sends batched across Test Connections with `io_uring`.** Submitting needs `unsafe` in the safe wrappers, and its system calls are outside the unit's filter; `sendmmsg` takes one socket.
- **A spin tail, or an idle-poll threshold.** Round 2 showed no benefit for their CPU.
- **Grouping wake-ups by default,** as OB-UDPST's fixed 100 µs tick does: an Engine Thread whose next deadlines fall within a window of the earliest waits for the last of them and serves them in one turn, each Burst at most the window late and still stamped at departure. It was built as an experimental setting, disabled by default. With a 50 µs window the Server-capacity check also read "not behind" and the Server woke about 60 % less often at 125 Test Connections. Its precision cost for Test Connections sharing an Engine Thread remained unmeasured because the available loopback runs could not isolate it from host-wide stalls and Skipped Bursts. Without that evidence, grouping was not adopted as the default. ADR 0016 later retired the experiment.
- **The Engine Thread count as a stable Server setting.** It is an experimental override for measurements; Server administrators size the pool with CPU affinity or a container's CPU limit, which it follows.

## Consequences

- **Protocol and runtime consequences:**
  - The Test Time Base is CLOCK_MONOTONIC_RAW on Linux: CLOCK_MONOTONIC is slewed. Waits are converted to CLOCK_MONOTONIC and re-checked against RAW on wake-up.
  - Receive timestamps are converted with clock pairs taken before and after each read.
  - Bounded catch-up, Skipped and Withheld Bursts, and a sender-limited warning join the Data Phase rules.
  - The Server polices a Load Sender that runs above its assigned rate: it stops the Test Connection after 3 Trial Intervals above 1.5×.
  - The late-burst metric gives way to over-rate, skip and shortfall gates, all re-judged on real hosts.
  - The Concurrency section says a Linux Server runs a pool of Engine Threads sized to its allowed CPUs, and everything else one Engine Thread per Test Connection.
- **Recorded OB-UDPST Difference.** OB-UDPST drops unsent datagrams, optionally rolls the sequence number back, and reschedules from "now" with no catch-up. It is recorded as the OB-UDPST Difference `missed-bursts-catch-up-or-are-skipped`.
- **Engine Threads:**
  - They catch panics around each Test Connection's work in a turn, so a panic retires one Test Connection and releases its Admission (ADR 0003), not the others its Engine Thread serves.
  - Every Test Connection handed to an Engine Thread reports its end, whatever happens to the thread: its report is sent when it is dropped, if it hasn't been sent already.
  - They are supervised from the Control Phase, which learns every end from the Engine Thread's report, after that Test Connection's last Data Phase event.
  - The release profile keeps `panic = "unwind"`.
  - In the pool, a stop reaches an Engine Thread at its next turn, or at once through its eventfd while it waits.
  - A Test Connection's end does nothing on its Engine Thread that could stall the others: reading the process's UDP tables for its Receive Buffer Overflows happens off it.
  - The Server logs how its engines run once as it starts, including the executor and the most Engine Threads it may start.
- **Settings.** The Engine Thread count, the hold-off, the catch-up budget and the kernel send timestamps are passed to the Data Phase Engine when it starts. The binary reads the experimental environment variables that set them, which stay experimental (ADR 0013).
- **A Data Phase socket leaves the Control Phase's reactor** when its Data Phase starts, so its traffic never wakes the Control Phase.
- **The hold-off's buffer share counts payload bytes,** not the memory the kernel charges for them, so on loopback and veth, which charge a datagram split from a UDP GSO message far more than its size, a fast Load Receiver sharing an Engine Thread with Load Senders can overflow its receive buffer; over a NIC the share holds.
- **Per-datagram random payload needs a generator cheap enough for 1 Mpps.** A naive one saturated the Engine Thread.
- **Deployment.** Timer slack must be 1 ns. A Server confined with the service's CPU affinity or a container CPU limit runs as many Engine Threads as it is given CPUs. The [hosting guide](../guides/hosting-a-server.md#give-it-enough-cpus) owns CPU-placement and receive softirq guidance.
