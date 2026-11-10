# Building udpstp

Build udpstp from source to develop it, try changes on `main`, or use a platform without a release binary. To install a published release, see [Installing udpstp](installing-udpstp.md).

You need Git and Rust installed through rustup. The repository's `rust-toolchain.toml` selects the toolchain, and Cargo uses the checked-in dependency versions with `--locked`.

```sh
git clone https://github.com/pederbe/udpstp
cd udpstp
cargo build --locked --release -p udpstp
```

The binary is `target/release/udpstp`, or `target\release\udpstp.exe` on Windows. Run it by that path, or install it into Cargo's binary directory so the guides' `udpstp` commands work.

```sh
cargo install --locked --path crates/udpstp
udpstp --version
```

If Cargo says its binary directory isn't on your PATH, add that directory using your shell's settings. Then follow the [quickstart](../getting-started.md) on both hosts.

For development checks and contributions, see [CONTRIBUTING.md](../../CONTRIBUTING.md).
