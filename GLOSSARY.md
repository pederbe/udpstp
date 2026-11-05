# udpstp

udpstp is a Rust implementation of the UDP Speed Test Protocol (UDPSTP, RFC 9946), which measures One-Way IP Capacity (RFC 9097) using a closed feedback loop between a Client and a Server.

## Language

### Roles

**Client**:
The endpoint that starts every Test by sending a Setup Request to the Server's Standard Port.
_Avoid_: Initiator, controller

**Server**:
The endpoint that listens on the Standard Port, admits Test Connections, and runs the Load Rate Adjustment Algorithm in both directions of a Test.
_Avoid_: Responder, reflector

**Load Sender** / **Load Receiver**:
The end of a Test Connection that sends Load PDUs, and the end that receives them and sends Status PDUs back. The Test's direction decides which the Client plays.
_Avoid_: Src/Dst (RFC 9097 wording), sender/receiver unqualified

**Client user**:
Whoever runs a Client and chooses its Test.
_Avoid_: User (unqualified)

**Server administrator**:
Whoever runs a Server and its Key Table, and so decides which Shared Keys it accepts and which are Operator Keys (RFC 9946 §6.2.1's server administrator).
_Avoid_: operator (lowercase)

**Operator**:
A network operator measuring within its own domain, the only party RFC 9946 §4.1 lets run Fixed-Rate Tests. A Server knows one only by an Operator Key.
_Avoid_: Admin, privileged user

**Consumer**:
Anyone measuring their own access (RFC 9946 §3); to a Server, any Client without an Operator Key, an unauthenticated one included.
_Avoid_: Subscriber (unqualified)

### Tests

**Upstream Test**:
A Test where the Client sends Load PDUs and the Server sends Status PDUs.
_Avoid_: Upload test, uplink test

**Downstream Test**:
A Test where the Server sends Load PDUs and the Client sends Status PDUs.
_Avoid_: Download test, downlink test

**Test**:
One measurement a Client runs in one direction: one or more Test Connections, to one or more Servers, sharing one mcIdent. A Server sees as the Test only the Test Connections that reach it from one Client address with that mcIdent.
_Avoid_: Speed test (unqualified), run, session

**mcIdent**:
The non-zero pseudorandom identifier every Setup Request of one Test carries, which associates its Test Connections (RFC 9946 §6.1). Beside it, mcIndex numbers the Test Connections from 0 and mcCount says how many the Client attempts.
_Avoid_: Test ID, session ID

**Test Connection**:
One Client–Server exchange of a Test, numbered by mcIndex, from its Setup Request to its end; once admitted it has its own Ephemeral Port.
_Avoid_: Session, flow, connection unqualified

**Multi-Connection Test**:
A Test with more than one Test Connection, told apart by mcIndex. All its Test Connections share the Test's direction, and the Client aggregates their results.
_Avoid_: Multi-flow test, parallel test

**Minimum Test Connections**:
The fewest of a Test's Test Connections that must be set up and keep running for the Test to go on; all of them unless the Client user sets fewer.
_Avoid_: Min connections, quorum

**Multi-Connection Coordinator**:
The part of the Client that runs a Test's Test Connections as one Test: the client process's job in RFC 9946 §4.
_Avoid_: Test manager, connection pool

**Search Test**:
A Test whose sending rate the Load Rate Adjustment Algorithm sets, starting at the default row or a requested one.
_Avoid_: Adaptive test

**Fixed-Rate Test**:
A Test whose Test Connections each hold one Sending Rate Table row for the whole Test Interval instead of running the Load Rate Adjustment Algorithm.
_Avoid_: Static test, constant-rate test

### The Server and its limits

**Standard Port**:
The Server UDP port where Setup Requests arrive: 24601, RFC 9946's assigned port, unless the Server is set to another.
_Avoid_: Control port (RFC 9946's name for it), listen port

**Ephemeral Port**:
The Server UDP port allocated to one Test Connection and returned as testPort in the Setup Response; all later PDUs of the Test Connection use it.
_Avoid_: Test port, data port

**Source**:
Where the Server counts Tests from: the Client's IPv4 address, or the /64 its IPv6 address is in. It stands in for the RFC's "path", which the Server can't see.
_Avoid_: Client address (for this grouping), path (for this grouping)

**maxBandwidth**:
The rate, in Mbps, a Setup Request says its Test Connection expects to send or receive in its direction (RFC 9946 §6.1). The Server admits it against the Admitted Rate Cap and never searches above it.
_Avoid_: Bandwidth (unqualified), requested capacity

**Admitted Rate Cap**:
The most maxBandwidth the Server admits at once in one direction, summed over the Test Connections of its activated Tests.
_Avoid_: Bandwidth cap, capacity limit

**Unactivated Test Connection**:
A Test Connection the Server has admitted but whose Activation Request it hasn't yet accepted.
_Avoid_: Pending connection, half-open connection

**Admission**:
The Server's acceptance of a Setup Request within its limits, which gives the Test Connection an Ephemeral Port and a place among the Test Connections the Server serves until it ends.

**Setup Request budget**:
The Server's limit on how fast it takes Setup Requests: one global budget for those not yet verified, and one per Source for authentic ones.

**Quota**:
A Server's limit on how many Tests, and how many load bytes, one Source or one Key Table row may run per Quota Period.
_Avoid_: Rate limit (that is a Setup Request budget), cap (that limits what the Server admits, not what a Source uses over time)

**Quota Period**:
The period a Quota is counted over; a Quota refills evenly over it rather than all at once when it ends.

### Packets on the path

**DF**:
The IPv4 Don't Fragment bit, set on test traffic so it is lost rather than fragmented on a path with a smaller MTU (RFC 9946 §5); on IPv6, where routers never fragment, the sender doesn't fragment either.
_Avoid_: No-fragment flag

**DSCP**:
The Diffserv code point test traffic is marked with, which the Client asks for in dscpEcn and the Server allows or coerces to 0 (RFC 9946 §7.1). ECN, the rest of that octet, is always Not-ECT.
_Avoid_: TOS (for the DSCP alone), priority, packet class

**MaxHops**:
The IPv4 TTL and IPv6 hop limit test traffic is sent with, set at each end from the expected path length so the traffic can't stray far beyond the intended path (RFC 9946 §10).
_Avoid_: TTL (for the setting), hop count

**Path MTU**:
The largest IP packet every link between the Client and the Server carries (RFC 9097 §6.5's PMTU); with DF set, a larger one is lost.
_Avoid_: MTU (unqualified; see Server MTU)

**Server MTU**:
The largest IP packet the Server is configured to send. Rows of its Sending Rate Table above 1 Gbps keep their rates with more, smaller packets when their own would be larger.
_Avoid_: Packet size limit, frame size

**Jumbo** / **Traditional MTU**:
The two Setup Request modifiers that choose the Sending Rate Table's packet sizes (RFC 9946 §6.1). Rates up to 1 Gbps use IP packets of at most 1250 bytes, or 1500 with Traditional MTU; Jumbo, set by default, lets faster rates use larger packets, up to 9000 bytes.

### Phases and messages

**Control Phase**:
The Setup exchange and the Activation exchange that precede measurement.
_Avoid_: Handshake, negotiation

**Data Phase**:
The exchange of Load PDUs and Status PDUs, ending with the STOP exchange or with an early end such as when the Watchdog expires.
_Avoid_: Test phase, measurement phase

**PDU**:
A single UDPSTP message carried in one UDP datagram, identified by its pduId.
_Avoid_: Packet, frame (message is an accepted synonym in the RFC)

**Control PDU**:
A PDU of the Control Phase: a Test Setup, Null or Test Activation PDU.

**Message Verification Procedure**:
The checks every received Control PDU and Status PDU must pass, in order, before it is acted on: size, checksum when used, authMode, authDigest, then authUnixTime (RFC 9946 §6.2). A PDU failing one is invalid.

**Setup exchange**:
The Setup Request, the Setup Response and, from the Ephemeral Port, the Null Request.

**Setup Request** / **Setup Response**:
The Test Setup PDU (0xACE1) sent by the Client, and the same PDU returned by the Server with its Command Response and the Ephemeral Port.

**Null Request**:
The Null PDU (0xDEAD) the Server sends from the new Ephemeral Port to open its own firewall pinhole; it gets no reply.

**Activation exchange**:
The Activation Request and the Activation Response.

**Activation Request** / **Activation Response**:
The Test Activation PDU (0xACE2) carrying the Test parameters, and the Server's accepted, coerced, or rejected version of it.

**Command Response**:
The cmdResponse code in a Setup Response or Activation Response: ACKOK, or a code naming why the request was rejected.
_Avoid_: Error code, status code

**Silent Rejection**:
Refusing a request by sending nothing back (RFC 9946 §5.3.1). A request only ignored, such as a duplicate already answered, is not one.
_Avoid_: Drop (unqualified)

**Coercion**:
The Server's change of one Activation Request parameter to a value it accepts, returned in the Activation Response with ACKOK.
_Avoid_: Override, adjustment (of a parameter)

**Load PDU**:
The PDU (0xBEEF) that carries the measurement load, with a sequence-numbered header followed by filler payload.
_Avoid_: Data packet, test packet

**Status PDU**:
The PDU (0xFEED) the Load Receiver sends every Trial Interval, carrying reception statistics and, when the Server sends it, the next sending parameters.
_Avoid_: Feedback message, status feedback message, report

**Stale Status PDU**:
A Status PDU whose spduSeqNo is 0 or no greater than that of the last one processed: a duplicated, reordered or replayed one.

**STOP exchange**:
The graceful end of a Test Connection: the Server marks its outgoing PDUs TEST_ACT_STOP2 and the Client confirms by marking its own.
_Avoid_: STOP2 exchange, stop handshake, teardown

### Timers

**Load PDU timeout** / **Status PDU timeout**:
The Load Receiver's timer, reset by every Load PDU received, and the Load Sender's, reset by every Status PDU received, whose expiry stops that end sending (RFC 9946 §5.1). The Test Connection ends later, at the Watchdog.

**Watchdog**:
The timer at each end of a Test Connection, reset by every valid PDU received while that end still sends, whose expiry ends the Test Connection without the STOP exchange (ADR 0002).
_Avoid_: Keepalive, idle timer, test termination timeout (RFC 9946 §6.1's name for it)

**Test initiation timer**:
The Client's timer, started with its Setup Request, that ends a Control Phase which hasn't reached the Data Phase within 3 s (RFC 9946 §6.1 lets it reuse the "test termination timeout"), or sooner if the Client user says so.

**Activation Watchdog**:
The Server's timer, started with a Setup Response, that closes the Test Connection if no valid Activation Request arrives within 3 s.

### Rate control

**Load Rate Adjustment Algorithm**:
The Server-side logic that picks the next Sending Rate Table row from measured impairments. The registered ones are algorithm B (ITU-T Y.1540 Amd2) and algorithm C (BBF TR-471).
_Avoid_: Congestion control, CCA, search algorithm

**Sending Rate Table**:
The Server's ordered table of rows, numbered from 0, each holding the transmission parameters for one offered sending rate.
_Avoid_: Rate table, rate ladder

**Sending rate**:
The rate of the Sending Rate Table row a Load Sender is set to send at (RFC 9946 §8.1; RFC 9097's "offered load").
_Avoid_: Offered load

**Sending Rate Structure**:
The transmission parameters of one Sending Rate Table row as carried on the wire (srStruct: two interval/payload/burst sets plus an add-on).
_Avoid_: Rate row (on the wire)

**Sequence Error**:
A Load PDU the Load Receiver counts, from the lpduSeqNo it sees, as lost, out of order or duplicated (RFC 9946 §8.2's seqErrLoss, seqErrOoo and seqErrDup). With ignoreOooDup set, as by default, only the lost ones count against seqErrThresh (RFC 9946 §7.1).
_Avoid_: Sequence anomaly

**Clean Interval**:
A Trial Interval whose counted Sequence Errors are at or below the threshold and whose delay variation is below the lower threshold. A Trial Interval with no delay variation sample is never clean.

**Errored Interval**:
A Trial Interval whose counted Sequence Errors exceed the threshold or whose delay variation exceeds the upper threshold.
_Avoid_: Impaired interval, congested interval

**Hold Interval**:
A Trial Interval that is neither clean nor errored, on which the algorithm makes no change.
_Avoid_: Neither, no-change interval

**Congestion**:
What the algorithm infers when a configured number of back-to-back Errored Intervals occur; inferring it sets Congestion Confirmed.
_Avoid_: Overload, impairment (an impairment is a single-interval observation)

**Congestion Confirmed**:
The latched state entered when Congestion is inferred. Algorithm B stays in it, and so in Single-Step Mode, for the rest of the Test Connection; algorithm C clears it only by a Fast-Mode Retry.
_Avoid_: Congested state, congestion flag

**Fast Mode**:
The state of a Load Rate Adjustment Algorithm, below the 1 Gbps row and before Congestion Confirmed, in which the rate rises by more than one row at a time.
_Avoid_: Ramp-up, slow start, high-speed mode (the RFC's name, kept only in the highSpeedDelta field)

**Single-Step Mode**:
The state in which every rate change is exactly one Sending Rate Table row up or down.
_Avoid_: Slow mode, fine-tuning

**Fast-Mode Retry**:
Algorithm C's re-entry into Fast Mode after enough single-step moves, clearing Congestion Confirmed.
_Avoid_: Retry (unqualified; clashes with retransmission)

**Lost Status Backoff**:
A Load Sender's lowering of its sending rate in a Search Test, as for an Errored Interval, each time no Status PDU has arrived for RFC 9097 §8.1's Lost Status Backoff timeout, which grows by one Trial Interval with each backoff.
_Avoid_: Feedback timeout backoff

**Over-rate policing**:
The Server's ending of a Test Connection whose Load Sender keeps sending well above the sending rates the Server assigned it (ADR 0008, ADR 0011).

### Measurement intervals and results

**Test Interval**:
The whole measurement duration (testIntTime, seconds) after which the Server starts the STOP exchange.
_Avoid_: Test duration, run time

**Sub-Interval**:
A fixed slice of the Test Interval (subIntPeriod, ms, a whole number of Trial Intervals) over which statistics are saved and reported in sisSav, and over which each IP-Layer Capacity is measured.

**IP-Layer Capacity**:
The IP-layer bit rate received in one Sub-Interval, IP and UDP headers included, over its nominal length (RFC 9097 §5.3, C(T,dt,PM)).
_Avoid_: Sub-Interval throughput, rate (unqualified)

**Trial Interval**:
The period between consecutive Status PDUs (trialInt, ms); the unit the algorithm decides on.
_Avoid_: Feedback interval, status interval

**Loss Ratio**:
The share of the Load PDUs sent in a Sub-Interval that were lost (RFC 9097 §5.4's ratio of lost packets, which §9 reports at the maximum).
_Avoid_: Packet loss rate, drop rate

**Tmax**:
The longest a Load PDU may take to arrive and still count as received rather than lost (RFC 9097 §4).
_Avoid_: Loss timeout, waiting time

**Performance Criterion**:
The loss target a Sub-Interval must meet for its IP-Layer Capacity to count towards the Maximum IP-Layer Capacity (RFC 9097 §6.3's PM criterion): at most a set Loss Ratio of the Load PDUs sent in it, or one lost Load PDU per Test Connection. One in which none were received meets none.
_Avoid_: PM threshold, quality filter

**Maximum IP-Layer Capacity**:
The highest IP-Layer Capacity of the Sub-Intervals that meet the Performance Criterion, within one Capacity Mode (RFC 9097 §6.3). The Test's headline result; a Test where none meets it has none.
_Avoid_: Throughput, bandwidth (except in maxBandwidth), speed, goodput

**Highest IP-Layer Capacity**:
The highest IP-Layer Capacity of any Sub-Interval, whether or not it meets the Performance Criterion: what OB-UDPST reports as its maximum.
_Avoid_: Peak rate, raw maximum

**Capacity Mode**:
One of the consecutive stretches of Sub-Intervals, split where the Client user says, that each get their own Maximum IP-Layer Capacity, for a path whose capacity changes on a schedule such as a "turbo mode" (RFC 9097 §6.6).
_Avoid_: Mode (unqualified), phase, first mode (TR-471's name, kept only in --first-mode-sub-intervals and FirstModeSplitIgnored)

**IP-Layer Sender Bit Rate**:
The IP-layer bit rate the Load Sender actually sent, over each st and each Sub-Interval's worth of them (RFC 9097 §7's B(S,st)); a Load Receiver can only estimate it, from the Load PDUs that arrive.
_Avoid_: Send rate (unqualified), Sender IP-Layer Bit Rate

**st**:
The length of the intervals, counted from the Load Sender's first send, over which the IP-Layer Sender Bit Rate is taken (RFC 9097 §7.2); udpstp's is one Trial Interval, offset from the Load Receiver's Trial Intervals by the one-way delay.

### Security

**Authentication Mode**:
How much of a Test Connection is authenticated (authMode): mode 1 authenticates the Control Phase and mode 2 also authenticates Status PDUs. RFC 9946 defines only these two, written mode 1 and mode 2.
_Avoid_: Auth level, security mode

**Unauthenticated Mode**:
OB-UDPST's authMode 0, which authenticates nothing and which RFC 9946 does not define; udpstp enables it only when both ends opt in, to interoperate with OB-UDPST.
_Avoid_: Mode 0, open mode

**Key Table**:
The table of Shared Keys a Client or Server authenticates with, one row per key with RFC 7210's fields, send and accept lifetimes among them (RFC 9946 §5.4).
_Avoid_: Key store, keyring

**Shared Key**:
A long-lived secret in the Key Table, selected by keyId, that is only ever used as input to the KDF.
_Avoid_: Password, secret unqualified

**Operator Key**:
A Shared Key that the Server's Key Table marks as an Operator's: only its Tests may be Fixed-Rate Tests, and they are spared some limits the Server sets for Consumers.
_Avoid_: Admin key, privileged key

**Derived Keys**:
The Client Authentication Key and Server Authentication Key, derived once per Test Connection from the Shared Key and the first Setup Request's authUnixTime.
_Avoid_: Session keys

**Troubleshooting Mode**:
A Server setting that answers authentication failures with a Command Response instead of a Silent Rejection, for debugging Client setups.
_Avoid_: Debug mode, verbose mode

### Conformance

**Requirement**:
One normative statement of RFC 9946, or of RFC 9097 as RFC 9946 uses it, with a stable ID. It counts as met when a test proves it, unless it is recorded otherwise.

**Deviation**:
A recorded departure from a Requirement, with its reason: a non-conformance, an interop accommodation, a platform gap or an unmet SHOULD.
_Avoid_: Deviation for a difference from OB-UDPST that conforms to the RFC (an OB-UDPST Difference)

**Interpretation**:
A recorded reading of a Requirement that is ambiguous, silent on a case, or collides with another one, or a choice the RFC leaves to the implementation; udpstp still conforms.

**OB-UDPST**:
The Broadband Forum's open-source C implementation of UDPSTP, and udpstp's interop peer.

**OB-UDPST Difference**:
Behaviour in which udpstp and OB-UDPST differ while udpstp conforms to the RFC.
_Avoid_: Deviation (from OB-UDPST)

**Conformance Record**:
The record of one Deviation, Interpretation or OB-UDPST Difference: what udpstp does, why, and the tests that prove it.
_Avoid_: Registry entry, deviations registry

### Load schedule

**Transmitter**:
One of a Load Sender's two independent Burst schedules (RFC 9946 §7.1's dual independent transmitters), each sending one Burst per interval as one interval/payload/burst set of its transmission parameters says.
_Avoid_: Timer, channel, stream

**Burst**:
The Load PDUs one Transmitter sends back to back at one scheduled time: burstSize datagrams, plus the Add-on on Transmitter 2.
_Avoid_: Batch, train

**Add-on**:
The single extra Load PDU of udpAddon2 bytes at the end of each Transmitter 2 Burst.

**Send Plan**:
The datagrams due at one scheduled time: the Deferred Datagrams still waiting, then one new Burst unless it is withheld.
_Avoid_: Burst plan

**Deferred Datagram**:
A Load PDU of a Send Plan that the host could not send. It leads the next Send Plan with its original sequence number and a fresh lpduTime.
_Avoid_: Retransmission, retry

**Catch-up budget**:
How late a missed Burst may be and still be sent when the Load Sender catches up (ADR 0012).
_Avoid_: Catch-up window, grace period

**Late Burst**:
A Burst sent after its scheduled time but less late than the catch-up budget, or the most recent Burst due.

**Burst lateness**:
How long after a Burst's scheduled time the Load Sender writes its Send Plan, and so stamps its lpduTime; the time from lpduTime to the datagram's departure is the send lag.
_Avoid_: Burst delay

**Skipped Burst**:
A Burst not sent because it was at least the catch-up budget late when its turn came and wasn't the most recent Burst due; its Load PDUs take no lpduSeqNo and are missing from the IP-Layer Sender Bit Rate.
_Avoid_: Dropped burst, lost burst (loss is what a Load Receiver counts from lpduSeqNo gaps)

**Withheld Burst**:
A Burst not sent because at least as many Deferred Datagrams as it holds are still waiting; counted like a Skipped Burst.

### Host

**Receive Buffer Overflow**:
A datagram the host's OS discarded at a Test Connection's socket before the Data Phase Engine read it, mostly for a full receive buffer; a Load Receiver counts it as lost though the path didn't lose it.
_Avoid_: Dropped datagram, socket drop, RcvbufErrors (the kernel's host-wide counter)

**Test Time Base**:
The steady clock every Data Phase time is taken from, which changes to the wall clock never move; lpduTime and spduTime are this clock plus a wall-clock offset fixed when the Test Connection activates.
_Avoid_: System time, wall time (for Data Phase timing)

**Data Phase Engine**:
The part of an endpoint that sends each Test Connection's Send Plans at their scheduled times and receives its Load PDUs and Status PDUs.
_Avoid_: Runtime (unqualified)

**Engine Thread**:
A thread that runs the Data Phase Engine for one or more Test Connections.
_Avoid_: Engine worker, per-core thread

### Benchmark rounds

**Receive probe**:
A bounded diagnostic UDP workload for observing host reception, packet conservation, coalescing and timestamp spread. Its packets are diagnostic datagrams with their own identity and sequence range.

**Round**:
One pass of the benchmark tooling over a matrix on one set of hosts: priced and planned, then provisioned; a Round makes its Round Runs and ends with its Round Teardown.

**Round Run**:
One engine measured at one count of one case of a Server-scale Round, or one Test of a first Round's matrix; valid or void, flagged or not.
_Avoid_: run (unqualified)

**Round Teardown**:
What ends every Round, whatever happened: its Servers stopped, every resource its provider made destroyed and listed again, its keys deleted, its ledger entry closed.
_Avoid_: teardown (unqualified)

**Carried Count**:
The most Test Connections of one case an engine of the Round carried: its point passed the gates at that count and at every smaller one; exact where the next count failed, a lower bound where it couldn't be judged.
_Avoid_: round capacity, Server capacity
