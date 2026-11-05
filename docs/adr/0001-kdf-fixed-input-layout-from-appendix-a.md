# KDF fixed-input layout follows RFC 9946 Appendix A

RFC 9946 §5.4.1 names the KDF inputs (counter-mode NIST SP 800-108, HMAC-SHA-256, r = 32, Label "UDPSTP", Context = decimal authUnixTime, 512 output bits) but leaves the byte layout open: at least five readings are consistent with the prose, and each derives different keys. We take the layout that Appendix A's OpenSSL KBKDF call produces, `BE32(i) ‖ "UDPSTP" ‖ 0x00 ‖ decimal(authUnixTime) ‖ BE32(512)`, because it is the only one the RFC pins down in code, OB-UDPST v9.1.0 uses it, and four independent computations (OpenSSL 3.5 on Windows and WSL, a hand-written HMAC construction, and the RustCrypto `kbkdf` crate) agree on its known-answer vectors.

## Consequences

- A reader following only the §5.4.1 prose might expect no separator byte or no length field; do not "fix" the layout to match the prose.
- The Shared Key is fed to the KDF as raw bytes. Unlike Appendix A's `strlen(Kin)`, it is not cut at the first zero byte; for text keys (as OB-UDPST uses) the two agree.
