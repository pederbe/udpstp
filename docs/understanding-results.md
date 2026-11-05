# Understanding the results

What udpstp measures, what each line of its results means, and why a Test can fall short of a path's real capacity. [Running a Test](guides/running-a-test.md) has the steps.

## What a Test measures

A Test sends load across the path for its Test Interval, 10 s by default, split into Sub-Intervals of 1 s. In a Search Test, the Server's Load Rate Adjustment Algorithm raises the sending rate until the path shows loss or growing delay, then holds it around the limit.

Each Sub-Interval has an IP-Layer Capacity: the bits that arrived, IP and UDP headers included, over its length. The **Maximum IP-Layer Capacity** is the highest IP-Layer Capacity of the Sub-Intervals that meet the Performance Criterion, which caps the share of Load PDUs a Sub-Interval may lose (1% by default). It is the headline result, as RFC 9097 defines it.

The **Highest IP-Layer Capacity** is the highest of all Sub-Intervals, whatever they lost. OB-UDPST reports it as its maximum. udpstp shows it beside the Maximum whenever they differ.

## The text results

This Downstream Test ran over loopback, so the two hosts, not a network, set its rates:

```text
Test outcome: completed
Endpoints
  Test Connection 0: Client unknown; Server 127.0.0.1:24701
Results
  Downstream Search Test with 127.0.0.1:24701, started 2026-09-26T21:51:34Z (T0)
  Test Interval (I) 10 s in 10 Sub-Intervals (dt) of 1000 ms; Trial Interval
    (FT) and st 50 ms; 1 Test Connection (F); MaxHops 64; DSCP 0 agreed
  Performance Criterion: loss at most 1% of the Load PDUs sent, or one; Tmax up
    to 1000 ms (a late Load PDU counts within the Sub-Interval its gap was seen
    in)

  Sub-Interval results
  SI = Sub-Interval; C = IP-Layer Capacity; DV = delay variation (min/avg/max);
    RTTV = RTT variation (min-max, avg); - = no measurement.
  B = IP-Layer Sender Bit Rate.
  SI  C (Mbps)  Loss Ratio
   1    103.97    0.000000
   2    303.93    0.000000
   3    504.06    0.000000
   4    648.99    0.000000
   5    801.02    0.000000
   6   1450.50    0.000000
   7   1689.22    0.002149
   8   1809.98    0.000000
   9   1867.22    0.014783
  10   1860.05    0.015516

  SI  Lost  Reordered  Duplicates
   1     0          0           0
   2     0          0           0
   3     0          0           0
   4     0          0           0
   5     0          0           0
   6     0          0           0
   7   307          0           0
   8     0          0           0
   9  2340          0           0
  10  2443          0           0

  SI  DV (ms)  RTTV (ms)
   1    0/1/3     0-1, 0
   2    0/1/2     0-2, 1
   3    0/1/2     0-2, 1
   4    0/1/7     1-7, 2
   5    0/1/8     1-3, 1
   6   0/2/11     1-6, 2
   7   0/4/11     1-8, 4
   8    0/3/9     1-8, 3
   9   0/4/14     1-8, 4
  10   0/5/12     1-8, 5

  SI  B (Mbps)*
   1      93.75
   2     293.58
   3     493.68
   4     646.35
   5     789.84
   6    1432.82
   7    1665.09
   8    1810.14
   9    1893.99
  10    1893.41

  * The IP-Layer Sender Bit Rate estimated here, not measured by the Load
    Sender: the Load PDUs the Server sent in each st of its own clock, from
    their lpduSeqNo span, a lost one as big as those around it

  Summary
  F = Test Connections; SI = Sub-Interval; Max = Maximum IP-Layer Capacity; - =
    no valid result.
   Phase  F  SI  Max (Mbps)  Loss Ratio
  Search  1   8     1809.98    0.000000
  RTT minimum (ms)  RTT maximum (ms)
                 0                 8
  Maximum Sub-Interval result received at an unknown time
  Maximum IP-Layer Capacity 1809.98 Mbps in Sub-Interval 8 (Loss Ratio 0.000000,
    RTT variation 1-8 ms above a minimum of 0 ms, estimated IP-Layer Sender Bit
    Rate 1810.14 Mbps); Highest IP-Layer Capacity 1867.22 Mbps in Sub-Interval 9
    (Loss Ratio 0.014783)
  RTT minimum 0 ms
  Minimum one-way delay 0 ms (clockDeltaMin; valid only if the clocks are
    synchronized)

  This host: DF clear, socket buffers receive 2097152 and send 2097152 bytes;
    portable Data Phase Engine, no fast paths
Test completed
```

Line by line:

- **Test outcome**: `completed`, `completed with warnings or qualified results`, `partially completed`, `failed`, `aborted` or `interrupted`. A Multi-Connection Test also gives each Test Connection's own outcome.
- **Endpoints**: each Test Connection's Server and, where the socket knows it, the Client's address. A socket bound to every address can't name the Client and says `unknown`. `--address-privacy` applies here too.
- **Header**: the direction, whether the algorithm set the rate (a Search Test) or an Operator fixed it (a Fixed-Rate Test), the Server, and the start time T0 in UTC.
- **Parameters**, in RFC 9097's notation: the Test Interval I, its Sub-Intervals of length dt, the Trial Interval FT, st (the interval the IP-Layer Sender Bit Rate is taken over), and F, the number of Test Connections. MaxHops is this end's own setting. The DSCP is the one the Server agreed to.
- **Performance Criterion**: the share of Load PDUs a Sub-Interval may lose and still count, set with `--max-loss-ratio`. One lost Load PDU always passes. A Sub-Interval in which nothing arrived never does. Tmax is how late a Load PDU may arrive and still count as received.
- **The tables**, one row per Sub-Interval (SI). The terminal's width decides whether they fit in one table or several:
  - **C**: the IP-Layer Capacity.
  - **Loss Ratio** and **Lost**: the Load PDUs lost, as a share of those sent and as a count. **Reordered** and **Duplicates** count the other Sequence Errors. Duplicates count as received.
  - **DV**: the one-way delay variation's minimum, average and maximum.
  - **RTTV**: the round-trip time variation, lowest to highest, and its average, above the RTT minimum.
  - **B**: the IP-Layer Sender Bit Rate, what the Load Sender actually sent. In a Downstream Test the Client can only estimate the Server's rate, and the footnote says so. In an Upstream Test it is measured on this end.
- **Maximum IP-Layer Capacity**: the headline result, with its Sub-Interval's Loss Ratio, RTT variation and Sender Bit Rate. Sub-Intervals 9 and 10 above were faster but lost more than 1%, so they don't count, and the line ends with the Highest IP-Layer Capacity. With `--first-mode-sub-intervals` there is one such line per Capacity Mode.
- **Maximum Sub-Interval result received**: when this Client learned the Maximum's own Sub-Interval result, or `unknown`.
- **RTT minimum** and **Minimum one-way delay**. The one-way figure compares the two ends' clocks, so it means something only when they are synchronised.
- **Warnings**, when there are any ([below](#what-the-warnings-mean)).
- **This host**: whether the Don't Fragment bit (DF) was set, the socket buffers the OS granted, and which Data Phase Engine ran with which fast paths. On Linux this names batched sending and receiving, UDP GSO and kernel timestamps. This example ran on Windows, which has none. [Platform differences](platforms.md) lists them.

In a terminal, headings are cyan, a clean result green, warnings and partial results yellow, and failures red. `--color never` keeps the output plain.

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
