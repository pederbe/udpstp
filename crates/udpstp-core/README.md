# udpstp-core

Add the UDP Speed Test Protocol to a Rust application with `udpstp-core`. It implements the Client and Server state machines, Load Rate Adjustment Algorithms and Data Phase from [RFC 9946](https://www.rfc-editor.org/rfc/rfc9946), with IP-Layer Capacity results from [RFC 9097](https://www.rfc-editor.org/rfc/rfc9097).

The core takes packets and time as inputs and returns actions for its caller. This lets you drive the same protocol logic with real sockets or deterministic simulations. [`udpstp-runtime`](https://crates.io/crates/udpstp-runtime) provides the socket and clock integration. See the [API reference](https://docs.rs/udpstp-core).

Part of [udpstp](https://github.com/pederbe/udpstp), a Rust tool for measuring network-path capacity, loss and delay.

## Licence

Licensed under either [Apache-2.0](https://github.com/pederbe/udpstp/blob/main/LICENSE-APACHE) or [MIT](https://github.com/pederbe/udpstp/blob/main/LICENSE-MIT).
