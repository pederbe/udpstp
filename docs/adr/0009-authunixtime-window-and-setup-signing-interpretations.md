# authUnixTime is checked within ±5 s inclusive modulo 2^32, the first Setup Request is signed with the Client key, and digests cover the PDU as received

RFC 9946 leaves several details of mode 1 open. We resolve them as follows.

**The time window.** A received authUnixTime is acceptable when it is within 5 seconds of the receiver's wall clock, in whole seconds, with both edges inclusive. That matches the RFC's "10-second tolerance window (+/- 5 seconds)" (§5.3.1) and OB-UDPST (`udpst_control.c:2280-2286`). The difference is taken modulo 2^32 as a signed 32-bit value, so times on either side of the field's 2106-02-07 06:28:16 UTC wraparound compare correctly. OB-UDPST converts to a 64-bit `time_t` and would reject every PDU after the wrap. Senders put the low 32 bits of the wall clock's seconds in the field, and the KDF context is the decimal form of that 32-bit value (ADR 0001). Both ends check the window, on requests and on responses.

**Which key signs the first Setup Request.** The Client derives the Derived Keys before sending, from the authUnixTime its first Setup Request carries, and signs that request with the Client Authentication Key. The Shared Key is never used directly. This is the only reading consistent with "this key SHALL only be used as input to the KDF" (§5.4) and "derived ... at the client prior to transmission" (§5.4.1), and OB-UDPST does the same. The Server derives from the received request's authUnixTime before verifying, and discards the keys if the digest fails.

**keyId in responses.** The Client selects no key from a response's keyId. It verifies every response with the Derived Keys of its own Setup Request. The Server echoes the request's keyId in its responses.

**What the digest covers.** The receiver computes the HMAC over the PDU exactly as received, with only authDigest and checkSum zeroed. Reserved fields are included even though their values are otherwise ignored ("the entire PDU", §5.3.1). A decoded PDU drops reserved fields, so it is never re-encoded for verification. OB-UDPST signs the raw buffer and carries values in Activation `reserved2` and `reserved4` when some options are on.

**Activation in mode 1.** The keys, authMode and keyId are fixed by the Setup Request and kept per Test Connection (§5.4.1). An Activation Request must carry the Setup's authMode and keyId. Both are checked before the digest, as part of "authMode ... appropriate" (§6.2 step 3), and a mismatch is dropped silently like any other verification failure (§7.2.1). OB-UDPST doesn't check keyId, but its Client always sends the Setup's, so this doesn't affect interop. Responses, BADPARAM included, carry the Setup's authMode and keyId, a fresh authUnixTime and the Server Authentication Key's digest.

**A response that fails verification is ignored, not fatal.** The Client drops a Setup or Activation Response that fails verification and keeps waiting; the test initiation timer decides the outcome. §5.3.1 says the receiver "SHOULD NOT continue with the Control phase", and ignoring the response satisfies that without letting a forged datagram end a genuine Test. OB-UDPST ends the Test at once.

## Consequences

- Proved by `src/tests/protocol/setup_mode1.rs` and `activation_mode1.rs`:
  - accepted at ±5 s, dropped at ±6 s;
  - accepted across the 2106 wraparound;
  - scripted peers check which key signs each PDU in both directions;
  - a non-zero reserved field signed as sent is accepted;
  - an Activation Request with another authMode or keyId than its Setup is dropped silently.
- The requirement entries for §5.3.1 and §5.4.1 reference this ADR.
- A replay of a Setup or Activation Request inside the window is dropped by the replay cache (`src/protocol/replay.rs`). Mode 2 Status PDUs aren't in it: a replayed one has a stale spduSeqNo, so it can't change the rate or deliver STOP.
