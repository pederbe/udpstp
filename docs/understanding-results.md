# Understanding the results

A Test's headline is the Maximum IP-Layer Capacity, the most the path carried while losing no more than the Performance Criterion allows. The lines around it say how the Test got there, and its warnings say when the result may fall short of the path's real capacity. [Running a Test](guides/running-a-test.md) has the steps.

## What a Test measures

A Test sends load across the path for its Test Interval, 10 s by default, split into Sub-Intervals of 1 s. In a Search Test, the Server's Load Rate Adjustment Algorithm raises the sending rate until the path shows loss or growing delay, then holds it around the limit.

Each Sub-Interval has an IP-Layer Capacity: the bits that arrived, IP and UDP headers included, over its length. The **Maximum IP-Layer Capacity** is the highest IP-Layer Capacity of the Sub-Intervals that meet the Performance Criterion, which caps the share of Load PDUs a Sub-Interval may lose (1% by default). It is the headline result, as RFC 9097 defines it.

The **Highest IP-Layer Capacity** is the highest of all Sub-Intervals, whatever they lost. OB-UDPST reports it as its maximum. udpstp shows it beside the Maximum whenever they differ.

## The text results

This Downstream Test ran over loopback, so the host, not a network, set its rates.

```console
$ udpstp client --key-file keys.toml --max-hops 12 127.0.0.1:24701
Sending the Setup Request to 127.0.0.1:24701
Setup accepted (Ephemeral Port 58730)
Test activated (Downstream)
The Server ended the Test
--------------------------------------------------------------------------------
Endpoints
  Test Connection 0: Client unknown; Server 127.0.0.1:24701
Results Summary
  Downstream Search Test with 127.0.0.1:24701, started 2026-11-10T17:41:45Z (T0)
  Test Interval (I) 10 s · 10 Sub-Intervals (dt) 1000 ms · Trial Interval (FT)
    and st 50 ms
  1 Test Connection (F) · MaxHops 12 · DSCP 0 agreed
  Performance Criterion: loss ≤ 1% or one Load PDU · Tmax ≤ 1000 ms
  Late Load PDUs count in the Sub-Interval where their gap was seen.

  Sub-Interval results
  SI  C (Mbps)  Loss Ratio   Lost  Reordered  Duplicates
   1    104.50    0.000000      0          0           0
   2    304.98    0.000000      0          0           0
   3    504.98    0.000000      0          0           0
   4    705.05    0.000000      0          0           0
   5    905.07    0.000000      0          0           0
   6   2049.74    0.000000      0          0           0
   7   4049.82    0.000000      0          0           0
   8   6049.85    0.000000      0          0           0
   9   9516.51    0.009479   7594          0           0
  10   9933.89    0.014537  12212          0           0

  SI  DV (ms)  RTTV (ms)  B (Mbps)*
   1    0/0/0     0-0, 0      94.52
   2    0/0/0     0-0, 0     294.97
   3    0/0/0     0-0, 0     494.98
   4    0/0/0     0-0, 0     695.05
   5    0/0/0     0-0, 0     895.07
   6    0/0/0     0-0, 0    1949.75
   7    0/0/0     0-0, 0    3949.83
   8    0/0/0     0-0, 0    5949.86
   9    0/0/0     0-0, 0    9453.69
  10    0/0/0     0-0, 0   10078.78

  SI: Sub-Interval · C: IP-Layer Capacity · B: IP-Layer Sender Bit Rate
  DV: delay variation (min/avg/max) · RTTV: RTT variation (min-max, avg) · -: no
    measurement
  * Estimated from the Server's lpduSeqNo span on its st clock.
  Lost Load PDUs use the size of surrounding Load PDUs.

Final metrics
  Maximum IP-Layer Capacity: 9516.51 Mbps (Sub-Interval 9)
  Maximum Sub-Interval result received at 2026-11-10T17:41:54.515674360Z
  Loss Ratio 0.009479, RTT minimum 0 ms, maximum 0 ms, estimated IP-Layer Sender
    Bit Rate 9453.69 Mbps; Highest IP-Layer Capacity 9933.89 Mbps in
    Sub-Interval 10 (Loss Ratio 0.014537)
  RTT minimum 0 ms
  Minimum one-way delay 0 ms (clockDeltaMin; valid only if the clocks are
    synchronized)

  Warnings
    22396 datagrams discarded by this host before udpstp read them: its receive
      buffer overflowed (Receive Buffer Overflows), and a Load Receiver counts
      them as lost

  This host: DF set, socket buffers receive 2097152 and send 2097152 bytes;
    portable Data Phase Engine, receiving with recvmmsg and kernel timestamps,
    waking on arrival with ppoll
--------------------------------------------------------------------------------
Test outcome: completed with warnings or qualified results
```

From the top:

- **Progress**: one line for each step of the Control Phase, then how the Test ended.
- **Endpoints**: each Test Connection's Server and, where the socket knows it, the Client's address. A socket bound to every address can't name the Client and says `unknown`. `--address-privacy` applies here too.
- **Results Summary** opens with the direction, whether the algorithm set the rate (a Search Test) or an Operator fixed it (a Fixed-Rate Test), the Server, and the start time T0 in UTC.
- **Parameters**, in RFC 9097's notation: the Test Interval I, its Sub-Intervals of length dt, the Trial Interval FT, st (the interval the IP-Layer Sender Bit Rate is taken over), and F, the number of Test Connections. MaxHops is this end's own setting. The DSCP is the one the Server agreed to.
- **Performance Criterion**: the share of Load PDUs a Sub-Interval may lose and still count, set with `--max-loss-ratio`. One lost Load PDU always passes. A Sub-Interval in which nothing arrived never does. Tmax is how late a Load PDU may arrive and still count as received.
- **The tables**, one row per Sub-Interval (SI), with their legend below them. The terminal's width decides whether they fit in one table or several:
  - **C**: the IP-Layer Capacity.
  - **Loss Ratio** and **Lost**: the Load PDUs lost, as a share of those sent and as a count. **Reordered** and **Duplicates** count the other Sequence Errors. Duplicates count as received.
  - **DV**: the one-way delay variation's minimum, average and maximum.
  - **RTTV**: the round-trip time variation, lowest to highest, and its average, above the RTT minimum.
  - **B**: the IP-Layer Sender Bit Rate, what the Load Sender actually sent. In a Downstream Test the Client can only estimate the Server's rate, and the footnote says so. In an Upstream Test it is measured on this end.
- **Maximum IP-Layer Capacity**, under **Final metrics**: the headline result and its Sub-Interval. Two lines down come that Sub-Interval's Loss Ratio, RTT and Sender Bit Rate. Sub-Interval 10 above was faster but lost more than 1%, so it doesn't count, and that line ends with the Highest IP-Layer Capacity. With `--first-mode-sub-intervals` there is one such group per Capacity Mode.
- **Maximum Sub-Interval result received**: when this Client learned the Maximum's own Sub-Interval result, or `an unknown time`.
- **RTT minimum** and **Minimum one-way delay**. The one-way figure compares the two ends' clocks, so it means something only when they are synchronised.
- **Warnings**, when there are any ([below](#what-the-warnings-mean)). Here the host's receive buffer overflowed.
- **This host**: whether the Don't Fragment bit (DF) was set, the socket buffers the OS granted, and which Data Phase Engine ran with which fast paths. On Linux this names batched sending and receiving, UDP GSO and kernel timestamps. macOS and Windows have none. [Platform differences](platforms.md) lists them.
- **Test outcome**: `completed`, `completed with warnings or qualified results`, `partially completed`, `failed`, `aborted` or `interrupted`. A Multi-Connection Test also gives each Test Connection's own outcome.

In a terminal, headings are purple, a clean result green, warnings, qualified results and partial results amber, and failures red. `--color never` keeps the output plain. [Terminal color](interfaces.md#terminal-color) lists every style.

## Was the network or a host the limit?

A search that rises past the path's limit meets loss or growing delay there, so its Trial Intervals turn errored:

- Below the 1 Gbps row the search climbs fast, and three Errored Intervals come back to back at its first overshoot. It confirms Congestion.
- Above that row it climbs one row at a time. Through a shallow queue it cycles around the limit, with Errored Intervals often too few back to back to confirm Congestion. It met the limit when they recur among its last Trial Intervals.

The Client counts its own Trial Intervals. When no Test Connection's search found the limit either way, it warns that the result may understate the path, and says why ([below](#when-the-search-never-confirmed-congestion)).

In a Fixed-Rate Test, which doesn't search, look for yourself. If the Sender Bit Rate sits close to the Capacity in every Sub-Interval and the Loss Ratio stays near 0, the Load Sender may be the bottleneck rather than the path. A Server that falls behind logs `BurstsSkipped`. A Client that falls behind lists Skipped Bursts in its warnings.

## What the warnings mean

A completed Test's warnings appear under **Warnings** in the text results and in `udpstp.Warnings` in the JSON, each with a `Code` to match on rather than its wording ([JSON results](json-output.md#the-extension)). The Test still exits 0. In the JSON, `ErrorStatus` carries the most severe one.

**No Sub-Interval met the Performance Criterion.** There is no valid Maximum IP-Layer Capacity (JSON ErrorStatus 49): the path lost more than `--max-loss-ratio` all along. A bottleneck with a shallow buffer does this. The search keeps stepping a row or two above its rate and back, losing a few percent there, as OB-UDPST's does too. The Highest IP-Layer Capacity is then the path's.

**A Sub-Interval ran faster than the Maximum, or the last one much slower** (JSON `SubIntervalsAfterMaximumMissed`). In a Search Test, every Sub-Interval after the Maximum's lost too much to count, and either one of them ran more than 5% faster than the Maximum (`--faster-sub-interval-margin`), or the last one ran under 60% of it. Common causes:

- **A drop-tail bottleneck.** After its first overshoot, the search cycles a row or two around the bottleneck's rate, losing a few percent in each Sub-Interval. The Maximum then falls to a Sub-Interval of the climb, as low as half the path's rate. This is most common from the 1 Gbps row up, where rows are 100 Mbps apart, and over long round trips.
- **A queue that keeps dropping after it overflowed**, as CAKE can when a few Test Connections carry a fast link's load. The rest of the Test then runs well below the link's rate. With enough Test Connections that each carries under 1 Gbit/s, CAKE stops the loss within a few seconds and the Test reads the link's rate.
- **A policer or shaper** that let a burst through at first. The Maximum is then its sustained rate, which is right.
- **A path that slowed** during the Test, or, Downstream, **a Server that fell behind**.

An Upstream Test in which this host left more than 1% of its Load PDUs unsent warns of its Skipped or Withheld Bursts instead.

**Skipped Bursts**: this host couldn't send on schedule, so the result is limited by the sender. **Withheld Bursts**: the OS couldn't take the datagrams fast enough.

**Receive Buffer Overflows.** This host's OS discarded datagrams at the socket before udpstp read them (JSON `ReceiveBufferOverflows`). A Load Receiver counts them as lost, so that loss happened on this host and not on the path. They are counted on Linux.

**The Server sent below its Fixed-Rate row.** In a Downstream Fixed-Rate Test, the Server sent less than its row, as this Client estimated it. It may have skipped or withheld Bursts, so the Server limited the result. A Search Test's Server doesn't tell the Client its sending rate, so it gets the next warning instead.

**The search never confirmed Congestion.** No Test Connection's search reached the path's limit, as this Client counted their Trial Intervals, so the result may understate the path (JSON `CongestionNeverConfirmed`). The rest of the line names the cause ([below](#when-the-search-never-confirmed-congestion)).

**Datagrams too large for the path with DF set.** A hop's MTU is below the Test's IP packets, so the Test didn't measure the path's capacity. Rates up to 1 Gbps use IP packets of 1250 bytes, or 1500 with `--traditional-mtu`. Faster Jumbo rows use up to 9000. This is reported on Linux.

**ICMP time-exceeded errors.** The path is longer than MaxHops, so the load expired on the way.

**Status PDUs never arrived, or the peer had missed Status PDUs.** Status PDUs were lost. In a Downstream Test the Server lowers the rate when that happens.

**Status PDUs failed authentication** (mode 2). Status PDUs arrived that didn't verify, so something on the path changed or injected them.

**Nothing was received for 1 s**, so this end stopped sending. **The Watchdog ended the Test** after 3 s of silence, without the STOP exchange. The results cover what was measured.

**Load PDUs without a delay sample: the wall clock stepped.** The clock was adjusted during the Test.

## When the search never confirmed Congestion

In a Search Test, this Client classifies each Trial Interval itself, from the Status PDUs it sends in a Downstream Test and receives in an Upstream one, with the thresholds the Server accepted. A search found the path's limit when it confirmed Congestion, or met the limit at its end: of its last 64 Trial Intervals, from the first Errored Interval among them on, at least 4 were Errored Intervals and at least 1 in 4, counting Hold Intervals of both kinds.

When no Test Connection's search found the limit either way, the warning names the first cause that fits:

- **The maxBandwidth.** The search stopped where a udpstp Server stops it, at the highest row at or below the maxBandwidth stated, or its share of it in a Multi-Connection Test. Above 1 Gbps the rows are 100 Mbps apart up to 7.2 Gbps and 720 Mbps apart beyond, so with `--max-bandwidth 10000` the search stops at 9360 Mbps. An OB-UDPST Server keeps to the maxBandwidth only in a build with rate limiting, run with `-B`.
- **The top of the Sending Rate Table**, judged from the Load PDUs sent at the peak. The Server's MTU decides their size: the table tops out at 12 Gbps with the default 1500 bytes, and at up to 72 Gbps with 9000-byte Jumbo packets.
- **Delay variation**, when more than half the Trial Intervals were Hold Intervals with a delay sample, between lowThresh and upperThresh. A Hold Interval in which no Load PDU arrived in order shows no delay variation and isn't counted here.
- **Still rising**, when the last Sub-Interval's IP-Layer Sender Bit Rate is above every earlier one's by more than Fast Mode adds with half its Trial Intervals clean, 100 Mbps at the defaults. A Load Sender held to a packet rate steps up once too, as its Load PDUs grow past the 1 Gbps row, so from about 500 Mbps a short Test can read as still rising where a longer one would name the Load Sender.
- **A Load Sender that stopped rising**, the Server's or this host's, though few of the last 64 Trial Intervals were Errored Intervals.

Some paths and Servers make this warning more likely:

- **A managed queue can hold the search in Hold Intervals.** fq_codel, with a Load Sender only just above the bottleneck, can hold a standing queue whose drops stay under seqErrThresh. The search's last Trial Intervals are then Hold Intervals with no Errored Interval among them, until CoDel drops faster. A Test that ends before then names the Load Sender, or delay variation, though the path limited it.
- **The Client classifies by each Trial Interval's largest delay variation**, the delay range the RFCs judge, as a udpstp Server does by default. An OB-UDPST Server classifies by the average, which is lower, so against one, delay variation can be named for a search it didn't hold.
- **This Client's count can differ from the Server's.** It counts the one or two Trial Intervals it reports after the Server's STOP1, and Status PDUs lost on the way back, which the Server never classifies. It can't see the Server's Lost Status Backoffs, which count as Errored Intervals there. Algorithm C's Fast-Mode Retry restarts the Server's run of Errored Intervals but not this Client's. So the warning can come although the Server's search confirmed Congestion.
- **An OB-UDPST Server counts more loosely.** A Clean or Hold Interval doesn't reset its count of Errored Intervals, where this Client, like a udpstp Server, needs them back to back ([ADR 0005](adr/0005-latched-congestion-confirmed-state.md)). Against an OB-UDPST Server, the warning can come although its search confirmed Congestion.

## Multi-Connection results

A Multi-Connection Test prints the summed results first, then each Test Connection's own Maximum IP-Layer Capacity. The summed table has an **Active** column: how many Test Connections received in that Sub-Interval. A Test Connection that didn't complete says how it ended, and standard error says why, naming it.

A Downstream Test's Test Connections are summed on one time grid. An Upstream Test's are each measured by their own Load Receiver, at the Server, and summed by Sub-Interval number, so the sum is approximate, and a warning gives the largest offset between their Sub-Intervals.

The "This host" line covers all Test Connections: the smallest socket buffers the OS granted any of them, and the fast paths every one that completed used.
