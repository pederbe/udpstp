# Key Table rows follow RFC 7210 with an HMAC-SHA-256 AlgID, lifetimes checked on our clock, and an Operator extension

RFC 9946 §5.4 requires a Key Table with RFC 7210's AdminKeyName, LocalKeyName, KDF, AlgID, Key and send/accept lifetimes, but leaves how they are used to the implementation. We decide as follows.

**AlgID and KDF.** The KDF column is `HMAC-SHA-256`, the IANA "KeyTable KDFs" name. It stands for the NIST SP 800-108 counter-mode construction with the UDPSTP label (§5.4.1, ADR 0001). The "KeyTable AlgIDs" registry has no value for the authDigest algorithm, so we use the unregistered name `HMAC-SHA-256`, mirroring the KDF. They are the only values the table accepts. LocalKeyName is the one-octet keyId (§5.4).

**Lifetimes.** A send or accept lifetime is a pair of UTC bounds in whole seconds, both inclusive, as RFC 7210's `YYYYMMDDHHMMSSZ` format has. Either end may be open. A lifetime that ends before it starts is rejected when the row is loaded.
- **Server:** it uses accept lifetimes only. At Setup it looks up the row whose LocalKeyName is the request's keyId and whose accept lifetime includes the Server's own wall clock (RFC 7210 §3). If rows exist for the keyId but none is in its accept lifetime, the request is dropped silently like any authentication failure (`SilentRejectionReason::KeyOutsideLifetime`; Troubleshooting Mode, answers it as CHSR_CRSP_AUTHFAIL).
- **Established Test Connections:** they keep their Derived Keys when the row's lifetime later ends (RFC 7210 §3: "exceeding the lifetime does not have any direct impact").
- **Client:** it uses send lifetimes only. It sends with the row whose send lifetime includes its wall clock, preferring the most recent SendLifetimeStart (RFC 7210 §3's rollover rule). Rows tied for the newest start (every row of an OB-UDPST key file, which has no lifetimes) make the choice ambiguous. The Client then doesn't guess: it fails unless a keyId is pinned with `--key-id`, like OB-UDPST's `-y` option. OB-UDPST instead falls back to keyId 0 (`DEF_KEY_ID`, `udpst.c` `read_keyfile`). With no valid row it doesn't start the Test.
- **No clock-skew grace.** RFC 7210 §3 allows "some grace time for clock skew". We add none. Overlapping lifetimes across keyIds give the same effect under the operator's control (RFC 7210 §6).

**Shared keyIds.** keyId is only one octet, so rotation reuses keyIds.
- Two rows may share a keyId only if neither their accept lifetimes nor their send lifetimes overlap. Otherwise the second row is rejected when the table is loaded.
- For accept lifetimes this is necessary: the Server finds a row by keyId alone, and RFC 7210 §3 selects a key only "if one key matches".
- For send lifetimes it is for symmetry and simplicity. One key file can serve both roles, and a pinned keyId then always has at most one valid row.
- Caches must follow this. The Derived Key cache keys on the row and authUnixTime, not the keyId, so a hit can't outlive its row's accept lifetime.

**Extension to RFC 7210.** A row's `operator` column (`true`, or `false` when absent) marks an Operator Key, and an Operator row may carry its own maximum Test Interval. Both are needed for Operator-only Fixed-Rate Tests and for Tests longer than 60 s. RFC 7210 has no column for them, so this is an extension that belongs to our key file format.

The columns RFC 9946 §5.4 doesn't list (PeerKeyName, Peers, Interfaces, Protocol, ProtocolSpecificInfo, Direction) are not modelled.

## Consequences

- Proved by `src/tests/protocol/key_table.rs`:
  - expired and not-yet-valid keys are dropped silently, and the edges are accepted;
  - rollover between two rows with one keyId works;
  - overlapping and backwards lifetimes are rejected;
  - the Client picks the newest valid send key, and refuses to guess between equally new ones unless a keyId is pinned.
- OB-UDPST key files have no lifetimes. Loading one gives rows valid at all times, so a Client with several keys needs a pinned keyId.
- Rotating within one keyId leaves no overlap, so a Client and Server whose clocks disagree near the switch fail until they agree. A smooth rotation gives the new key a new keyId whose lifetimes overlap the old one's, and retires the old keyId later (RFC 7210 §6).
