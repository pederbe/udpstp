# Architecture

udpstp is one application crate. Its binary runs as a Client or a Server, with internal modules for protocol logic, I/O and presentation.

1. **`src/wire`** holds PDU layouts, codecs, checksum, HMAC-SHA-256 and the KDF.
2. **`src/protocol`** implements both protocol roles. Its caller supplies packets and time, and it returns actions and deadlines without performing I/O.
3. **`src/runtime`** owns sockets, clocks, the Tokio Control Phase loops and the Data Phase Engine.
4. **The other `src` modules** own commands, configuration, key files, exit statuses and output. `src/windows_permissions` contains the Windows key-file policy and its approved native adapter.

A Client's Setup Request reaches the Server's Standard Port. The Server admits the Test Connection and gives it an Ephemeral Port, and the Activation exchange agrees the Test's parameters. In the Data Phase, the Load Sender sends Load PDUs, the Load Receiver answers with Status PDUs, and the Server's Load Rate Adjustment Algorithm picks the next sending rate.

Because the core takes time as an input, tests run a Client core against a Server core on a virtual clock, with no sockets.

[docs/architecture.md](docs/architecture.md) has the full picture, with diagrams of each phase and the Data Phase Engine. Decisions are recorded in [docs/adr](docs/adr/), and [GLOSSARY.md](GLOSSARY.md) defines the terms.
