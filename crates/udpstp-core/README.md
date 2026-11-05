# udpstp-core

The sans-IO protocol core of the UDP Speed Test Protocol ([RFC 9946](https://www.rfc-editor.org/rfc/rfc9946)): the Client and Server state machines, the Load Rate Adjustment Algorithms, the Data Phase, and the RFC 9097 results. It performs no I/O and reads no clock, so it runs the same under a simulator as on a network; [`udpstp-runtime`](https://crates.io/crates/udpstp-runtime) drives it with real sockets.

Part of [udpstp](https://github.com/pederbe/udpstp). **Pre-release:** the API will change before 1.0.

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
