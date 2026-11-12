# OpenWrt package

Build an APK package for OpenWrt 25.12 on x86_64 using the static Linux release
binary. This owned source feed includes the Client, a manually started
Server and an optional `udpstp-server` package for OpenWrt's procd service
manager. The service stays off until you configure and enable it. Neither
package installs a key or changes the firewall. The feed is not part of
the upstream OpenWrt packages feed, and there is no hosted APK repository.

## Build the package

Use a Linux x86_64 computer with the
[OpenWrt SDK prerequisites](https://openwrt.org/docs/guide-developer/toolchain/install-buildsystem)
and curl installed. Download and extract the SDK for the current OpenWrt 25.12
x86/64 release from [OpenWrt downloads](https://downloads.openwrt.org/).
The SDK and router must use the same release and target.

From the udpstp checkout, prepare a feed for the latest release. The helper
reads the release's SHA256SUMS and puts that version and its archive checksum
in the package recipe. The SDK checks the archive before packaging it.
No Rust compiler is needed.

```sh
sh contrib/openwrt/prepare-feed.sh "$PWD/.scratch/openwrt-feed"
```

In the extracted SDK, register the feed. Replace the path below with the
absolute path printed by the helper, without its final `/udpstp` component.
Use a path without spaces, as required by OpenWrt's build tools.

```sh
printf 'src-link udpstp /path/to/udpstp/.scratch/openwrt-feed\n' >> feeds.conf
./scripts/feeds update udpstp
./scripts/feeds install -p udpstp udpstp
make defconfig
make package/udpstp/compile V=s
cp bin/packages/x86_64/udpstp/udpstp-[0-9]*.apk udpstp.apk
cp bin/packages/x86_64/udpstp/udpstp-server-*.apk udpstp-server.apk
```

If `feeds.conf` does not exist, the command above creates it. This package needs
no other source feed. The resulting `udpstp.apk` contains the unchanged static
executable and its licence notices. OpenWrt's installed libc is its only package
dependency. Allow about 5 MiB of free storage for the installed package.
`udpstp-server.apk` adds the procd service and depends on `udpstp` and OpenWrt's
existing procd package. Installing only `udpstp.apk` adds no service or account.

Follow [installation and use](../../docs/guides/installing-udpstp.md#openwrt)
to transfer the APK to your router. The local package is unsigned, so install
only an APK you built or obtained from someone you trust.

## Update and remove

Run the preparation helper again to select the latest release, then rebuild
in the SDK. After refreshing the feed, clean the previous package build first
so the SDK reads the updated version and checksum.

```sh
./scripts/feeds update udpstp
./scripts/feeds install -p udpstp udpstp
make defconfig
make package/udpstp/clean
make package/udpstp/compile V=s
cp bin/packages/x86_64/udpstp/udpstp-[0-9]*.apk udpstp.apk
cp bin/packages/x86_64/udpstp/udpstp-server-*.apk udpstp-server.apk
```

Keep just the APK you intend to transfer if an older build remains in that
directory. Repeat the installation command with the new APK to upgrade.
Stop a manually started Server before replacing its executable, then restart
it with the same arguments. `apk del udpstp` removes the executable and
licences. If you installed `udpstp-server`, transfer and upgrade both packages
together. Its configuration is preserved and OpenWrt restarts an enabled
Server. `apk del udpstp-server` stops and removes the service while keeping the
Client package. User-owned keys and modified configuration stay in place.

## Configure the service

Follow [the OpenWrt service setup](../../docs/guides/installing-udpstp.md#optional-server-service)
to install `udpstp-server.apk`, give its unprivileged `udpstp` account a private
key file, and enable it. Its Server settings are in
`/etc/udpstp-server/server.toml`. The supplied settings listen only on loopback.
The UCI flag `udpstp.server.enabled` defaults to `0`, so package installation
and boot leave the Server stopped until you change it to `1`. OpenWrt registers
the init script for boot even while that flag is off.

The service sends standard output and errors to `logread`, disables core
dumps, and lets procd restart a failed process after five seconds. Repeated
startup failures stop after five retries. Use `service udpstp-server restart`
after changing the Server settings or keys. Removing or disabling the service
leaves its key file and modified configuration in place.

## Validation

Package acceptance uses the OpenWrt x86/64 disk image booted with QEMU and its
own kernel. Install the APK, check `udpstp --version`, and run authenticated
Downstream and Upstream Tests. The existing
[`release-smoke`](../../docs/testing.md#contribution-checks) command can run
inside the image when `xtask` is built for `x86_64-unknown-linux-musl`.
Check an upgrade from an older package, repeat a Test, and remove the package.
Confirm that installation never starts a Server and that private key files
retain their permissions and content through upgrade and removal. With the
optional package, test the disabled default across a reboot, then configure
it and run authenticated Tests against the procd-managed Server. Check its
unprivileged account, system log, restart after a failure, configuration reload,
boot startup, clean stop and package removal.

This validates packaging and protocol operation. It does not establish a
router's capacity or qualify other architectures. External feed admission
remains separate work.
