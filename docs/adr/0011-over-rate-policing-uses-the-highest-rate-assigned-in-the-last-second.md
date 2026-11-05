# Over-rate policing judges against the highest rate assigned in the last second

**Status: Accepted** (2026-09-25). It amends ADR 0008.

ADR 0008 has the Server police an Upstream Load Sender: after 3 Trial Intervals in a row above 1.5 times its assigned rate, the Server stops the Test Connection. That was written for a fixed rate. Once algorithm B moves the row every Trial Interval, "its assigned rate" is ambiguous. The Client switches to a new Sending Rate Structure about a round trip after the Status PDU carrying it leaves. What already queued at a bottleneck keeps arriving at the bottleneck rate until the queue drains, which takes at least upperThresh (90 ms by default) whenever DelayVar confirmed the Congestion. After algorithm B's one-time drop of 3 × highSpeedDelta rows, the next few Trial Intervals therefore receive more than 1.5 times the new row on any path whose buffer holds more than about one Trial Interval. An impairment test measured an Upstream search through 20 Mbit/s with a 200 ms buffer being stopped as OverRate after 19 Trial Intervals, although the Client did nothing wrong.

We police each Trial Interval against the highest rate the Server assigned within the last second, the current one included. One second covers a round trip plus the drain time of a queue at upperThresh plus a Trial Interval on any path the Test can measure. A Client that really sends too fast is still stopped: its rate exceeds 1.5 times every rate assigned in the window, and at most one second later than before.

## Consequences

- A Client may run at up to 1.5 times the highest rate of the last second for up to a second after a decrease. That bound is still far below what an unpoliced Client could send.
- The test `an_upstream_search_through_a_deep_buffer_is_not_stopped_over_rate` (crates/udpstp-core/tests/algorithm_b.rs) proves the fix; `the_server_stops_a_client_sending_over_its_rate` (tests/upstream.rs) still proves the policing.
- Alternatives considered: pausing policing for a fixed time after each decrease (more state, same effect), or a window sized from the measured RTT (the RTT isn't known before the first samples, and a bottleneck's drain time isn't the RTT).
