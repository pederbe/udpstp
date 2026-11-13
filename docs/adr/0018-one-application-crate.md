# One application crate

**Status: Accepted** (2026-11-13). Supersedes ADR 0013's library-crate policy and ADR 0015's separate helper-package layout. Their application compatibility and Windows permission behavior remain in force.

udpstp implements the RFCs as one application, with one root Cargo package and one binary target. Packet encoding, protocol decisions, networking, presentation and Windows permissions are internal modules. Their Rust visibility does not establish a developer API or require an external migration policy.

Separate production packages added manifests, version coordination and registry dependencies without serving a separate product. Ordinary modules retain the useful organization and supplied-time protocol tests. A library plus a binary would simplify some harness imports, but introduces a second application crate. Development targets instead include the maintained modules they exercise, with their dependencies declared explicitly.

The existing Windows native adapter moves into `src/windows_permissions/native.rs`. The application denies unsafe code by default. Its ordinary modules, protocol, networking, descriptor decoder and permission policy forbid unsafe code. Only the existing native module allows the approved operations from ADR 0015. The outer permission module also compiles in test builds on other platforms so its safe tests remain active.

The application alone is published. Existing published component versions remain available. Test attributes, maintenance commands, benchmarks and fuzz harnesses are unpublished development tooling. The weekly release schedule, installed commands, files, output and protocol behavior are unchanged.
