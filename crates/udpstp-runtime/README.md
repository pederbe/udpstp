# udpstp-runtime

Run a UDP Speed Test Protocol Client or Server from Rust with `udpstp-runtime`. It connects [`udpstp-core`](https://crates.io/crates/udpstp-core) to UDP sockets, clocks, the Tokio Control Phase and the Data Phase Engine.

Use `serve` to run a Server and `run_test` to run a Client Test. The core handles protocol decisions while the runtime moves packets and schedules their delivery. See the [API reference](https://docs.rs/udpstp-runtime).

Part of [udpstp](https://github.com/pederbe/udpstp), a Rust tool for measuring network-path capacity, loss and delay.

## Licence

Licensed under either [Apache-2.0](https://github.com/pederbe/udpstp/blob/main/LICENSE-APACHE) or [MIT](https://github.com/pederbe/udpstp/blob/main/LICENSE-MIT).
