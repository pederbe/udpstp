# Stop traffic after 1 s of silence, tear down the Test Connection at 3 s

RFC 9946 contradicts itself on timeouts: §5.1 items 7–9 require the Load PDU and Status PDU timeouts to stop the test within at most 1 s of silence, while §6.1 describes a 1 s watchdog that only warns and a termination 2 s later (which OB-UDPST and TR-471 implement as a 3 s stop). We satisfy both: after 1 s without Load PDUs the Load Receiver stops sending Status PDUs, and after 1 s without Status PDUs the Load Sender stops sending Load PDUs (§5.1), which bounds any self-induced congestion; the Test Connection itself, with its socket and state, is torn down at 3 s (§6.1).

## Consequences

- Against OB-UDPST, our endpoints go quiet earlier than OB-UDPST does (1 s vs 3 s). Tear-down times differ by up to about 1 s (see below).
- Traffic that resumes between 1 s and 3 s does not restart load, nor reset the Watchdog; the Test Connection is already ending. §5.1's "SHALL be closed" is read as final, over §8.1's "until traffic is received again".
- rxStopped is asserted from 0.5 s, half the 1 s timeout, so the PDUs sent before an end stops carry it. §8.1 recommends 1 s, which the 1 s timeouts would never let anyone see.
- An end that stopped sending also stops refreshing its peer's Watchdog. After a break in one direction, an OB-UDPST peer therefore tears down about 1 s after we do (its Watchdog was reset by our last PDU, about 1 s later than ours), and it reports traffic stopped in both directions. This is an OB-UDPST Difference.
