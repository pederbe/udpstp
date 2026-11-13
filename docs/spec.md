# udpstp specification

udpstp implements the UDP Speed Test Protocol in RFC 9946 and the Maximum IP-Layer Capacity method in RFC 9097. It provides a Client and a Server for authenticated network measurement, with human-readable and JSON results.

The numbered stories below define intended behavior and retain stable identifiers for tests. They are requirements, not a claim that every feature or platform is complete. The README Status table describes what users can run. Platform limits are documented in `docs/platforms.md`, and protocol interpretations and departures are recorded in `docs/conformance/records/`.

## User stories

### Running a Test as a Client user

1. As a Client user, I want to run a Downstream Test against a Server by host name or address, so that I can measure my download IP-Layer Capacity.
2. As a Client user, I want to run an Upstream Test, so that I can measure my upload IP-Layer Capacity.
3. As a Client user, I want the Client to use the Standard Port 24601 unless I specify another, so that I don't need to know Server internals.
4. As a Client user, I want a host name that resolves to both IPv4 and IPv6 to use the first address the resolver returns, so that address-family choice follows my system's normal behaviour.
5. As a Client user, I want a Test to default to a 10 second Test Interval, so that a default run is short and within the RFC's limit.
6. As a Client user, I want sensible defaults for every Test parameter (Trial Interval, Sub-Interval, thresholds, algorithm B), so that I can run a correct Test with no options.
7. As a Client user, I want to choose algorithm B or C, so that I can use the variant that suits my access type.
8. As a Client user, I want to choose one-way delay variation or RTT variation as the algorithm's delay input, so that I can match my clock situation.
9. As a Client user, I want to set a maximum bandwidth for the Test, so that the Server does not attempt rates beyond what I expect.
10. As a Client user, I want to request a starting Sending Rate Table row, so that a search can start near the expected rate.
11. As a Client user, I want to request a DSCP value for the load, so that I can measure a specific traffic class.
12. As a Client user, I want to request randomized payload content, so that compression on the path cannot inflate the result.
13. As a Client user, I want to enable or disable Jumbo datagram support and Traditional MTU, so that datagram sizes match my network.
14. As a Client user, I want to see every parameter the Server coerced, so that I know the conditions my result was measured under.
15. As a Client user, I want the Client to end the Test if the Server's coerced parameters are unacceptable to me, so that I never get a result under conditions I didn't accept.
16. As a Client user, I want a clear, specific message for every Setup or Activation rejection code, so that I can fix the problem.
17. As a Client user, I want a clear diagnostic when a Server appears not to support auth mode 2, so that I know to retry with mode 1 instead of seeing a bare timeout.
18. As a Client user, I want a clear diagnostic when authentication fails because my clock is off, so that I can fix my clock.
19. As a Client user, I want the Client to give up with an error if the Control Phase doesn't complete in time, so that a dead Server never hangs my run.
20. As a Client user, I want the Test to stop cleanly when the Server's Test Interval ends, so that I get complete results.
21. As a Client user, I want the Test to stop within 1 second if the load or Status PDUs stop arriving, so that a broken path never leaves traffic running.
22. As a Client user, I want a warning in my output when traffic stops, so that I know the result was affected.
23. As a Client user, I want human-readable results by default, so that I can read them directly.
24. As a Client user, I want JSON output compatible with OB-UDPST's schema, so that existing TR-471 tooling keeps working.
25. As a Client user, I want the Maximum IP-Layer Capacity reported per RFC 9097, including IP and UDP header bytes, so that the number is comparable with other conforming tools.
26. As a Client user, I want per-Sub-Interval rates, loss, delay variation and RTT range, so that I can see how the path behaved over time.
27. As a Client user, I want loss and round-trip delay at the maximum, and the sender bit rate, reported, so that the result meets RFC 9097's reporting requirements.
28. As a Client user, I want a missing final Sub-Interval reported as missing rather than zero, so that lost Status PDUs never distort my result.
29. As a Client user, I want the results to say which fast-path features were active, so that I can judge whether host limits affected the measurement.
30. As a Client user, I want the process exit status to reflect the outcome, so that scripts can react to failures.
31. As a Client user, I want a completed Test with only warnings to exit with status 0, so that scripts treat it as a completed measurement.
32. As a Client user, I want to be told when a path MTU problem or an ICMP time-exceeded occurred, so that I don't mistake it for low capacity.
33. Retired. The Client preserves the Server's transmission parameters instead of re-splitting a row.
34. Retired with story 33.

### Authentication and keys

35. As a Client user, I want auth mode 2 by default, so that Status PDUs are authenticated as the RFC recommends.
36. As a Client user, I want to choose auth mode 1 explicitly, per Server if needed, so that I can test against Servers that don't support mode 2.
37. As a Client user, I want no automatic fallback from mode 2 to mode 1, so that an attacker cannot downgrade my Test.
38. As a Client user, I want to supply my Shared Key from a key file, never on the command line, so that it doesn't leak through the process list.
39. As a Client user or Server administrator, I want key files readable by other users to be refused, so that keys aren't exposed by accident.
40. As a Client user or Server administrator, I want keys written explicitly as text or hex, so that a key is never misinterpreted.
41. As a Client user or Server administrator, I want to load OB-UDPST key files directly, so that I can reuse existing keys.
42. As a Server administrator, I want key-table entries with the RFC 7210 fields and send/accept lifetimes, so that I can rotate keys safely.
43. As a Server administrator, I want two entries to share a keyId when their lifetimes don't overlap, so that keys can roll over within the one-octet keyId space.
44. As a Server administrator, I want to mark entries as Operator Keys, so that only Operators can run Fixed-Rate Tests and long Tests.
45. As a Server administrator, I want keys wiped from memory after use and never logged, so that a crash dump or log never reveals them.
46. As a Client user or Server administrator, I want Unauthenticated Mode available only when both ends explicitly enable it, so that I can interoperate with default OB-UDPST installations without weakening normal operation.

### Operator

47. As an Operator, I want to run a Fixed-Rate Test at a chosen row, so that I can verify a known capacity after installation or repair.
48. As an Operator, I want Fixed-Rate Tests refused for everyone without an Operator Key, so that consumers cannot flood shared paths.
49. As an Operator, I want to run Tests longer than 60 seconds when my key allows it, so that I can observe longer-term behaviour.
50. As an Operator, I want long Tests to fall back to RTT variation, so that clock drift doesn't corrupt one-way delay variation.
51. As an Operator, I want to set MaxHops on both ends, so that test traffic can't stray beyond the intended path.
52. As an Operator, I want a warning when MaxHops was not set explicitly, so that I meet my obligation to set it.
53. As an Operator, I want to exempt my keys from the 500 ms congestion-inference limit, so that I can use radio-path settings the ITU recommends.

### Multi-Connection Tests

54. As a Client user, I want to run a Multi-Connection Test with up to 255 Test Connections, so that I can fill links a single flow can't.
55. As a Client user, I want to spread connections across a list of Servers, so that one Server isn't the bottleneck.
56. As a Client user, I want all Setups done before any Activation, and the Test aborted before any load if too few connections succeed, so that a partial setup never sends stray load.
57. As a Client user, I want to set the minimum number of connections required, so that I can accept partial Tests when I choose.
58. As a Client user, I want all Activations sent together and the coerced parameters checked for consistency, so that the aggregated result is meaningful.
59. As a Client user, I want the Test to continue if some connections fail mid-Test while enough remain, so that one flaky connection doesn't waste the run.
60. As a Client user, I want the Test stopped if several connections fail together, so that a path failure ends the Test as the RFC intends.
61. As a Client user, I want Downstream Multi-Connection results aggregated on one common timeline, so that summed capacity is not inflated by misaligned Sub-Intervals.
62. As a Client user, I want Upstream Multi-Connection results flagged as approximate with the largest offset shown, so that I know their limits.
63. As a Client user, I want each Sub-Interval's active connection count shown, so that I can compare maxima fairly.
64. As a Client user, I want the most severe connection status reported for the Test, so that a failure is never hidden by another connection's success.

### Hosting a Server

65. As a Server administrator, I want to run the Server unprivileged, so that a flaw in packet parsing can't compromise the host.
66. As a Server administrator, I want a TOML configuration file with command-line overrides, so that deployments are reproducible.
67. As a Server administrator, I want the Server to choose Test parameters from its own ranges and coerce out-of-range requests, so that every Test runs within safe limits.
68. As a Server administrator, I want a maximum Test Interval of 60 seconds by default, so that production Tests are constrained as the RFC requires.
69. As a Server administrator, I want to cap total admitted bandwidth and reject over-capacity requests, so that the Server never over-commits its link.
70. As a Server administrator, I want to cap concurrent Test Connections globally and per Test, so that resources can't be exhausted.
71. As a Server administrator, I want one concurrent Test per client address by default, configurable higher for carrier-grade NAT, so that the RFC's one-test-per-path rule holds by default.
72. As a Server administrator, I want to optionally require clients to state maxBandwidth, so that admission control has real numbers.
73. As a Server administrator, I want optional per-key and per-source frequency and volume quotas, so that I can limit heavy testers.
74. As a Server administrator, I want to configure the Ephemeral Port range, so that my firewall can allow exactly that range.
75. As a Server administrator, I want the Server to reply from the address the Setup arrived on, so that multi-homed and anycast deployments work.
76. As a Server administrator, I want an optional Troubleshooting Mode that sends rejection responses even when authentication fails, with a warning while it's on, so that I can debug client setups.
77. As a Server administrator, I want authentication failures to be silent by default, so that attackers learn nothing.
78. As a Server administrator, I want the Server's socket buffers and fast paths to degrade with a clear warning when the system isn't tuned, so that I know what to fix.
79. As a Server administrator, I want a hardened example systemd unit, so that I can deploy safely.
80. As a Server administrator, I want a container image, documented to need host networking for accurate results, so that I can deploy it easily.
81. As a Server administrator, I want periodic statistics and a summary record per Test, so that I can monitor usage and abuse.
82. As a Server administrator, I want client addresses logged in full, pseudonymized, or truncated by configuration, so that I can balance abuse handling and privacy.
83. As a Server administrator, I want structured logs in human or JSON format, so that I can feed them to my log pipeline.

### Security

84. As a Server administrator, I want replayed Setup and Activation Requests dropped, including replays across sibling connections, so that captured PDUs can't start Tests.
85. As a Server administrator, I want floods of unverified Setup Requests dropped against a global budget, so that spoofed sources can't exhaust a victim's budget.
86. As a Server administrator, I want per-source limits applied only after authentication and allowing bursts of at least a full Multi-Connection Test, so that legitimate Tests aren't dropped.
87. As a Server administrator, I want Derived Keys cached per key and timestamp, so that a flood costs one HMAC per PDU rather than a KDF run.
88. As a Server administrator, I want each Test Connection locked to its client after the first authenticated Activation Request, so that off-path traffic can't hijack it.
89. As a Server administrator, I want Test and Setup packets from multicast or broadcast addresses dropped, so that the Server can't be used for amplification.
90. As a Server administrator, I want attack-triggerable log events rate-limited, so that a flood can't fill my disk.
91. As a Client user, I want sending parameters in Status PDUs clamped to my Test's limits, so that a forged Status PDU can't make my Client flood the path.
92. As a Client user and Server administrator, I want Status PDUs with stale sequence numbers excluded from rate decisions and from STOP handling, so that replays can't steer or stop a Test.
93. As a Client user, I want the Client to pick a fresh mcIdent for every Test, so that retries don't collide with leftover state.

### Measurement correctness

94. As a Client user, I want sending rates to follow the Sending Rate Table exactly, with the RFC's datagram size limits for each rate range, so that load matches the plan.
95. As a Client user, I want DF set on test traffic by default, so that fragmentation doesn't distort the measurement.
96. As a Client user, I want to be told when my Load PDUs are too large for the path, with what fixes it (dropping `--traditional-mtu`, or a lower `server_mtu` on the Server), so that jumbo rows never fail silently on ordinary networks.
97. As a Client user, I want Status PDUs sent only after the first Load PDU arrives, so that no empty feedback is sent.
98. As a Client user, I want sequence loss, reordering and duplication counted per the RFC with a 32-entry lookback, so that impairments are measured consistently with OB-UDPST.
99. As a Client user, I want timestamps that are immune to wall-clock steps and slewing during a Test, so that delay variation and RTT stay accurate.
100. As a Client user, I want per-datagram kernel receive timestamps where available, so that receiver scheduling stalls don't appear as network delay.
101. As a Client user, I want delay values rounded exactly as OB-UDPST rounds them, so that both implementations make the same decisions from the same packets.
102. As a Client user, I want datagrams that a partial send left unsent to go out in the next burst with fresh timestamps and their original sequence numbers, so that host backpressure isn't reported as loss.
103. As a Client user, I want the load rate to back off when Status PDUs are lost in a Downstream Test, so that the Test doesn't congest a path whose feedback is failing.
104. As a Client user, I want algorithm B to stay in Single-Step Mode once Congestion is confirmed, so that the search converges instead of oscillating.
105. As a Client user, I want algorithm C's Fast-Mode Retry to work as the standards describe, so that radio paths are measured accurately.
106. As a Client user, I want Congestion inferred only from back-to-back Errored Intervals, so that the RFC's strict reading is honoured.
107. As a Client user, I want intervals with no delay data treated as "no change", so that total loss never looks clean.

### Interoperability

108. As a Client user, I want our Client to work against OB-UDPST v9.1.0 Servers in auth mode 1, so that I can use existing Servers.
109. As a Server administrator, I want OB-UDPST v9.1.0 Clients to work against our Server in auth mode 1, so that existing users can switch Servers.
110. As a Client user, I want our Client to accept OB-UDPST's random-size flag and its IPv6 size convention, so that Upstream Tests against OB-UDPST Servers stay within the RFC's size limits.
111. As a Client user, I want OB-UDPST's data in reserved fields ignored, so that its extensions can't confuse our implementation.
112. As a maintainer, I want every known difference from OB-UDPST recorded, so that users understand why results may differ.

### Maintainer and verification

113. As a maintainer, I want every normative statement in RFC 9946 and the RFC 9097 requirements it imports tracked in a machine-readable matrix, so that conformance is provable.
114. As a maintainer, I want CI to fail when a requirement has no test, deviation, interpretation or operational classification, so that coverage can't silently erode.
115. As a maintainer, I want CI to fail on unknown or orphaned requirement IDs and on requirement tests that don't run on the gating platform, so that the matrix stays honest.
116. As a maintainer, I want requirement text hashed, so that I notice when a requirement's wording is edited.
117. As a maintainer, I want every deviation recorded with its kind, default, setting, proving test and revisit trigger, so that deviations are deliberate and reviewable.
118. As a maintainer, I want the protocol core deterministic and free of I/O, so that every behaviour can be tested and replayed exactly.
119. As a maintainer, I want a simulator with realistic network and host flaws, so that timing requirements are tested without flakiness.
120. As a maintainer, I want scripted peers built from the RFC text, so that behaviour our own Client and Server would share is checked independently.
121. As a maintainer, I want known-answer vectors from independent sources, so that wire-level correctness isn't checked against our own reading.
122. As a maintainer, I want decoders and the Server core fuzzed, so that hostile input can't crash or exhaust the Server.
123. As a maintainer, I want the OB-UDPST interop matrix in CI, so that interop regressions are caught before release.
124. As a maintainer, I want tests on Linux, Windows and macOS scoped per platform, so that every supported platform builds and works.
125. As a maintainer, I want no `unsafe` code outside the narrowly bounded Windows Shared Key permission adapter approved in ADR 0015, so that protocol, wire, runtime and ordinary application memory safety comes from the compiler while native permission inspection has an explicit reviewed boundary. The bounded production adapter and supported-policy refusal are implemented under ADR 0015.
126. As a maintainer, I want nightly precision, fuzz, impairment and mutation runs, so that slow checks still run regularly.
127. As a maintainer, I want release builds for x86_64 and aarch64 Linux, Windows and macOS with attestation and an SBOM, so that users can trust what they install.
128. As a maintainer, I want the toolchain to start on the latest stable Rust and then support the last three stable releases, so that users on slightly older toolchains can build it.

## Implementation decisions

udpstp has one application crate. `src/protocol` makes protocol decisions without I/O and receives time as an input. `src/wire` encodes and decodes PDUs. `src/runtime` drives sockets and clocks, with Tokio for the Control Phase and a dedicated Data Phase Engine. The other application modules own configuration, key loading, presentation and process exit behavior. These internal boundaries are implementation choices described in `docs/architecture.md` and ADR 0018.

The ADRs record the settled KDF layout, timing, Sending Rate Table conventions, algorithms, transport-error reporting, runtime libraries, Data Phase Engine contract, key-file policy and stable interfaces. Read the relevant ADR before changing one of those decisions. `GLOSSARY.md` defines the domain terms.

First-party Rust remains safe except for the approved Windows Shared Key permission adapter isolated by ADR 0015. The protocol, wire code, network runtime, safe policy and decoder keep their prohibition of unsafe code. Expanding the approved exception requires the maintainer's explicit approval.

## Testing decisions

Tests check observable behavior at maintained boundaries. Expected values come from the RFCs, hand-derived scenarios, independent known-answer vectors or retained independent fixtures. Tests do not regenerate their expected results from the implementation under test.

- S1 drives the sans-IO core through the deterministic simulator or a scripted peer. The virtual clock and seeded network make failures reproducible. The in-crate core interfaces listed in `docs/testing.md` may also be tested directly.
- S2a exercises the binary's arguments, configuration, output, reports and exit statuses. S2b runs Client and Server loopback Tests and fault injection.
- S2c runs shaped network-namespace and OB-UDPST interoperability Tests on Linux. It is separate from physical-network performance qualification.
- S3 checks wire codecs and cryptography against independent values, and supplies fuzzing targets.
- Presentation and site-generation tests use the explicitly listed production interfaces in `docs/testing.md`. Integrated process tests and a complete site build verify their wiring.

Tests proving an RFC Requirement carry its requirement ID. The traceability check validates the IDs, classifications and proving tests. A specification, ADR or Conformance Record test names its source in a plain comment.

Linux, macOS and Windows receive native platform checks. Linux-only tests retain that scope. Performance claims require the measurements and limits recorded for their exact workload. Passing functional checks alone establishes no rate or precision claim.

The maintained commands, tier definitions, generated-file checks and failure policy live in `docs/testing.md`.

## Scope limits

Path MTU discovery, encryption, combined IPv4 and IPv6 measurement within one Test, automatic authentication downgrade and compatibility with OB-UDPST's command-line flags are outside this specification. Mode 2 interoperability with OB-UDPST v9.1.0 is unavailable because that peer does not implement it. The Windows Don't Fragment and UDP Segmentation Offload work remains separate from the currently supported Windows path.
