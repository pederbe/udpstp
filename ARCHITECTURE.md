# Architecture

udpstp is one binary that runs as a Client or a Server, built from four main crates with a firm line between protocol logic and I/O.

1. **`udpstp-wire`** holds the PDU layouts and codecs, the checksum, HMAC-SHA-256 and the KDF. It makes no protocol decisions.
2. **`udpstp-core`** is the protocol core for both roles. It performs no I/O and reads no clock. Its caller passes in received datagrams and the time, and the core returns what to send and when to call it next.
3. **`udpstp-runtime`** moves the bytes. It owns the sockets and clocks, the Tokio Control Phase loops and the Data Phase Engine, and makes no protocol decisions.
4. **`udpstp`** is the binary, with the command line, the Server's configuration, key files, exit statuses and output.

A Client's Setup Request reaches the Server's Standard Port. The Server admits the Test Connection and gives it an Ephemeral Port, and the Activation exchange agrees the Test's parameters. In the Data Phase, the Load Sender sends Load PDUs, the Load Receiver answers with Status PDUs, and the Server's Load Rate Adjustment Algorithm picks the next sending rate.

Because the core takes time as an input, tests run a Client core against a Server core on a virtual clock, with no sockets.

[docs/architecture.md](docs/architecture.md) has the full picture, with diagrams of each phase and the Data Phase Engine. Decisions are recorded in [docs/adr](docs/adr/), and [GLOSSARY.md](GLOSSARY.md) defines the terms.
