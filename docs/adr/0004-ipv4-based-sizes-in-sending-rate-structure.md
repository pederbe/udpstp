# Sending Rate Structure sizes are IPv4-based; IPv6 senders subtract 20 bytes

RFC 9946 has two SHALLs that collide on IPv6. §8.1 says the Client SHALL use the transmission parameters the Server sends, unchanged, and §6.1 caps IP packet sizes at 1250, 1500 or 9000 bytes. The RFC doesn't say whether the payload sizes in the Sending Rate Structure assume IPv4 or IPv6 headers. OB-UDPST stores IPv4-based payload sizes (IP size minus 28) and has whichever side sends the load subtract 20 bytes on IPv6. That keeps packet sizes and rates the same on both address families. We adopt the same convention for both our Client and our Server, and record it as a reconciliation of the two SHALLs rather than as a free interpretation. Our Client can't tell an OB-UDPST Server from any other, so it has to subtract every time. A Server that sent IPv6-based sizes would then get packets 20 bytes too small, and a Server that sent IPv4-based sizes without the Client subtracting would break the §6.1 limits.

## Consequences

- IPv4 vs IPv6 is decided from the actual IP version of each Test Connection's packets, never from the socket family. An IPv4 peer on a dual-stack socket (`::ffff:a.b.c.d`) is IPv4.
- A third-party Client that follows §8.1 literally will send 1270-byte IPv6 packets when talking to our Server, which breaks §6.1. This is documented, not prevented.
