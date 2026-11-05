# Catch-up is bounded by a time budget

**Status: Accepted** (2026-09-26), amended 2026-09-27. It amends ADR 0008.

Bounding catch-up to one Sending Rate Structure interval makes the allowed lateness depend on the selected row. Rows of 100 Mbps and up send a Burst every 100 µs, which is shorter than a portable timer can reliably sleep. Skipping every Burst older than one interval can therefore make the host limit the Test before the network does.

**A missed Burst less than the catch-up budget late is still sent. One at least the budget late is a Skipped Burst.** The most recent missed Burst always goes, as under ADR 0008. The runtime supplies the budget through `DataPhase::set_catch_up_budget`. Windows uses 2 ms. Other platforms use the core's default of 1 ms. The Windows budget was increased from 1 ms after timer and loopback diagnostics showed that 1 ms did not cover ordinary late wake-ups.

## Decision

- **Keep the Send Plan interface.** `poll_burst` returns late Bursts one per call, oldest first, each with its scheduled time. A plan holds Deferred Datagrams plus at most one new Burst. `horizon` remains 0 and `report_sent` remains the backpressure channel.
- **Shift scheduled traffic without adding traffic.** Catch-up sends only planned Bursts. A late wake-up can move traffic into the next Trial Interval, but does not raise the long-run scheduled rate. The time bound limits that shift. At the default 50 ms Trial Interval, 1 ms is 2 % and 2 ms is 4 % per Transmitter. At the shortest 5 ms Trial Interval those figures are 20 % and 40 %. This is a single shift rather than sustained excess under ADR 0011.
- **Keep reception responsive.** The portable engine reads between Send Plans, so catching up does not defer a Status PDU's userspace receive timestamp until every late Burst has been sent.
- **Bound a turn as well as catch-up.** The engine sends for at most 1 ms per turn before processing its other work. This prevents a host that always takes longer than one interval to send from remaining in the send loop indefinitely.

## Considered options

- **One interval plus a spin tail.** Spinning can improve deadline precision, but costs CPU for each Test Connection. ADR 0008 rejects it at Server scale.
- **Coalesce several Bursts into one plan.** This changes the Send Plan contract and is unnecessary for a time budget.
- **Scale the budget to trialInt or the row.** This can shrink it below the timer lateness it needs to cover. A fixed time bound keeps the decision independent of the selected row.

## Consequences

- **Late Burst** and **Skipped Burst** in [GLOSSARY.md](../../GLOSSARY.md) refer to the catch-up budget rather than one row interval.
- Skipped Bursts remain counted, reported and deducted from the IP-Layer Sender Bit Rate.
- A longer stall still skips old traffic rather than releasing every missed Burst at once.
- The OB-UDPST Difference remains. OB-UDPST drops unsent datagrams and reschedules from now without catch-up.
