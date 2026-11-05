# udpstp-runtime

The I/O for the UDP Speed Test Protocol ([RFC 9946](https://www.rfc-editor.org/rfc/rfc9946)) core, [`udpstp-core`](https://crates.io/crates/udpstp-core): UDP sockets with their IP options, clocks, the Tokio Control Phase and the Data Phase Engine. It makes no protocol decisions. `serve` runs a Server and `run_test` runs one Test as a Client.

Part of [udpstp](https://github.com/pederbe/udpstp). **Pre-release:** the API will change before 1.0.

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
