# udpstp

The `udpstp` command: a Client and a Server for the UDP Speed Test Protocol ([RFC 9946](https://www.rfc-editor.org/rfc/rfc9946)), which measures Maximum IP-Layer Capacity with the method of [RFC 9097](https://www.rfc-editor.org/rfc/rfc9097).

```sh
cargo install udpstp
udpstp server --key-file keys.toml
udpstp client --key-file keys.toml server.example.net
```

**Pre-release:** see the [repository](https://github.com/pederbe/udpstp) for what works today, the documentation, and the conformance evidence.

The protocol is also available as libraries: [`udpstp-wire`](https://crates.io/crates/udpstp-wire) (PDU codecs), [`udpstp-core`](https://crates.io/crates/udpstp-core) (the sans-IO protocol core) and [`udpstp-runtime`](https://crates.io/crates/udpstp-runtime) (sockets, clocks and Tokio).

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
