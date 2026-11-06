# udpstp-wire

Encode and decode the packets used by the UDP Speed Test Protocol in [RFC 9946](https://www.rfc-editor.org/rfc/rfc9946). `udpstp-wire` provides layouts and codecs for Setup, Activation, Null Request, Load and Status PDUs, authentication digests and the header checksum.

Use it with [`udpstp-core`](https://crates.io/crates/udpstp-core), which handles protocol validation and decisions, or when working directly with protocol packets. See the [API reference](https://docs.rs/udpstp-wire).

Part of [udpstp](https://github.com/pederbe/udpstp), a Rust tool for measuring network-path capacity, loss and delay.

## Licence

Licensed under either [Apache-2.0](https://github.com/pederbe/udpstp/blob/main/LICENSE-APACHE) or [MIT](https://github.com/pederbe/udpstp/blob/main/LICENSE-MIT).
