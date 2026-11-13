# Documentation

udpstp measures how much traffic a network path can carry, along with its loss, delay variation and round-trip time. A Client and a Server run a Test over UDP, as [RFC 9946](https://www.rfc-editor.org/rfc/rfc9946) describes, and the Client prints the result as text or JSON. It runs on Linux, macOS and Windows, and interoperates with [OB-UDPST](https://github.com/BroadbandForum/obudpst).

## Where to start

| You want to | Start with |
|---|---|
| Try udpstp for the first time | [Installing udpstp](../../docs/guides/installing-udpstp.md), then the [quickstart](../../docs/getting-started.md) |
| Measure a path and read what the numbers mean | [Running a Test](../../docs/guides/running-a-test.md) and [Understanding the results](../../docs/understanding-results.md) |
| Run a Server for your own Clients or for the public | [Hosting a Server](../../docs/guides/hosting-a-server.md) and the [security model](../../docs/security.md) |
| Use udpstp on Windows | [udpstp on Windows](../../docs/guides/windows.md) |
| Collect results in a script or a measurement platform | [JSON results](../../docs/json-output.md) and [Stable interfaces](../../docs/interfaces.md) |
| Look up an option or a setting | [Command line](../../src/main.rs) and [Server configuration](../../src/config.rs) |
| Check how udpstp follows the RFCs | [Conformance](../../docs/conformance/summary.md) |
| Fix an error or a warning | [Troubleshooting](../../docs/troubleshooting.md) |

The [guides](../../docs/guides/README.md) cover each task step by step, and the [glossary](../../GLOSSARY.md) defines the terms these pages use.
