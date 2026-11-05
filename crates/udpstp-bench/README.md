# udpstp-bench

udpstp's benchmark Rounds, on [`bench-rounds`](../bench-rounds):

- the first Round, which compares udpstp with OB-UDPST on rented hosts;
- the Server-scale Round, which finds how many Test Connections one Server carries.

Its `udpstp-bench` binary plans and runs both Round types and writes their reports. It reads udpstp's JSON results and the Server's JSON log as any consumer does, never through udpstp's crates, so the judging stays independent of the code it judges.

Part of [udpstp](https://github.com/pederbe/udpstp)'s tooling, and never published.

## Modules

- `cli`: `udpstp-bench round` plans, prices and checks either kind of Round with `round.py`'s options, names and defaults, prints the plan, and with `--yes` runs it in `bench-rounds`' Round bracket. `report OUT`, `scale-report OUT [--first FIRST_OUT]` and `proxy-report FILE [--feasibility]` reproduce the Python reports. `--provider aws` needs `--live`, which alone makes the real `aws` CLI, and `--yes` with it is refused on Windows itself; a paid Round's calls are recorded privately only where `--record-calls` asks, and one whose calls can't be is then refused before anything is created.
- `first_round::matrix`: a first-round matrix, read as the Python reads it and refused in its words, with its Round Runs in the order a Round makes them and their labels; how long they take, the checks a Round makes of them before it is priced, and what they add to its worst case. It is stricter where the Python reads what no committed matrix holds: an unknown key, a fraction or a numeric string.
- `first_round::commands`: udpstp's and OB-UDPST's Client and Server command lines, which read the Round's Operator Key from its key files, never from the command line.
- `first_round::judge`: each Round Run's Maximum and Highest IP-Layer Capacity from its JSON result, udpstp's and OB-UDPST's alike, keeping the kind of number the files hold; and the criterion fixed before the first paid Round, with how a Round Run that failed, or that the platform limited, counts.
- `first_round::report`: the Round's report, byte for byte the Python's for the original settings, or none where the Python refuses it too or would compute from what no Round writes. It reads the files the Python reads, when the Python reads them, through a seam: the Round's `--out` on disk, or a test's own. Explicit Server Engine Thread experiments keep their settings separate in the report.
- `first_round::round`: the first Round's body, which `bench-rounds`' Round bracket runs: the Round's Operator Key in both engines' key files, the hosts created, each prepared (its dead-man switch checked and the host quieted where it is rented, the binaries and keys copied in, its environment recorded), the Jumbo path checked, the Servers started, the path check, the matrix until the deadline or three Tests failing in a row, each Round Run's counters, result and line of `summary.jsonl`, the Servers' logs collected, and the Servers stopped. Cloud-pair counters follow the Test peer's route to the underlying NIC and retain its driver counters. Other host calls retain the original sequence. An explicit Server Engine Thread cap uses an isolated Server for its Test.
- `obudpst`: the pinned OB-UDPST built on a host of either Round with `scripts/obudpst-build.sh`, the tools installed where they are missing; a failure says what the installation or the build's log said, in the Python's words.
- `scale::matrix`: the Server-scale matrix's settings, report plan and ordered Round Runs, including confinement, probes, P1, engine rotation, hold-off A/B pairs, grouping, precision pairs and the close-call reserve. Its plan and refusals are compared with the Python at the reference revision recorded in the golden file.
- `scale::commands` and `scale::hosts`: the Server-scale commands and the host readings they use: CPU topology, the `CpuSet` selections used by confinement, IRQ placement, socket-buffer limits and host state. The commands invoke the uploaded Rust sampler.
- `scale::judge` and `scale::report`: the frozen Server-scale rules, per-run records, point verdicts, Carried Counts, confinement, hold-off A/B, grouping and precision readings, including exact Mann-Whitney probabilities. Evidence loading ends at `RunFiles` and `RoundFiles`; the judgments and reports also work from memory.
- `scale::schedule`: the run order and its decisions, taking records and time as inputs: calibration, probes, confinement, retries, close calls and precision pairs.
- `scale::round`: host preparation, path checks, CPU and IRQ placement, all three engines, evidence collection and the Round Teardown. Each Server stop, final process sweep and IRQ-balance restoration is attempted independently, including after an interrupt or a panic. Static Rounds may keep the named binaries a matrix requests.
- `proxy`: the parser, judging and text of the proxy report and its feasibility mode.

Build with `cargo build --locked --release -p udpstp-bench` before taking measurement locks. Run `udpstp-bench --help` for its commands. Live cloud Rounds create paid resources. Review the planned matrix and cost before running them.

## Measurement commands

- `sample OUT PIDS [CLIENT_NS [SECONDS]]` appends the existing Linux CPU,
  thread and UDP records every 0.5 seconds until the processes exit or its limit.
- `capture start STEM TARGET SECONDS LOG`, `capture collect STEM TARGET SECONDS`
  and `capture stop STEM` manage owned first-Round evidence. Stops verify the
  saved process identity and directory before signaling. Missing evidence stays
  unavailable.
- `campaign-report DIR SERVER_CPUS CLIENT_CPUS` reproduces the earlier
  campaign's Markdown and `summary.json`.
- `precision --bin UDPSTP` runs the loopback suite, with the existing rows,
  repeats, gates and statuses. `--replay FILE` re-judges saved raw results, and
  `--json FILE` writes the existing result format.

Rounds upload a prebuilt Rust executable. `--measurement-tool PATH` selects a
Linux build matching the hosts' architecture and libc. It defaults to the
coordinator's executable. The local home role uses that executable. Build before
taking measurement locks. Native Windows coordinators must supply the Linux
host build for static sampling. The small `scripts/first-capture.sh` launcher
only forwards capture arguments to Rust.

First-Round `[[test]]` cases may set `server_engine_threads` to a positive integer
for an explicit udpstp Server pool-cap experiment. It applies only to pairs with
a udpstp Server. A case without such a pair is refused. Each capped Test starts
an isolated Server with `UDPSTP_ENGINE_THREADS`, retaining its log and the
requested cap in the plan and result. Its setup reserve is included in the
duration and cost model. A capped precision Test uses the same isolation.
Cap-containing schedules use plan version 2, which older reporters refuse.
Cap-free schedules retain version 1.

Reports keep different requested caps in separate groups. They describe the
requested maximum separately from the Server's observed pool capacity and
sampled thread counts. Missing evidence remains unavailable. Capped Search
Tests receive no verdict from the original default OB-UDPST comparison rule.
Uncapped matrices and reports retain their original behavior. A cloud experiment creates paid resources and needs a supported matrix and judging rules.

The [measurement fixture note](tests/golden/measurement-reference.md) records
independent reference provenance. Source-name and executable-mode mappings in the
old orchestration fixtures reflect the new tools. Tool and launcher hashes now
identify Rust, while recorded measurement and judging expectations remain intact.

## Static work directories

The Rust CLI reserves a fresh work directory on each static host when a Round starts. Its name contains 128 bits of randomness, and it is created exclusively with mode 0700. Existing directories, files and links are refused. Preflight only checks connectivity, so a plan creates no work directory and never removes one from another Round.

The host must provide Bash and GNU `stat`, and the work directory's ancestors must belong to the benchmark account or root. A shared writable ancestor such as `/tmp` must have the sticky bit. Before copying or executing files, and again before Round Teardown, the tooling checks the directory's owner, permissions and recorded identity. When an allocation response was lost, its private `.bench-owner` receipt proves ownership instead. It never recreates a missing directory. Roles using the same SSH destination share one reservation; different aliases and local roles receive distinct names even when they reach the same machine.

Allocation defers an interrupt until ownership is recorded, within its 30 s limit. If a timeout or lost SSH response leaves allocation uncertain, Round Teardown uses the receipt to recover and record the directory identity before removing files. An absent candidate needs no removal. An existing unproven or changed directory is reported for inspection and is never automatically removed.

Round Teardown removes the Round's files and Shared Keys. A profile matrix requesting retained binaries keeps only those binaries and the ownership receipt; the log identifies their directory and gives a removal command for use after `perf report`. Partial file removal preserves the receipt. If removing the final empty directory fails, its recorded identity still allows a later attempt to retry safely.

## CPU topology limits

The Rust tooling refuses oversized CPU
topology and IRQ affinity output. Limits apply together to each complete input:

| Resource | Maximum |
|---|---|
| UTF-8 input bytes | 1 MiB (1,048,576 bytes) |
| Comma-separated CPU range components, including invalid or empty components | 4,096 |
| Expanded CPU entries visited, including duplicates and overlapping ranges | 65,536 |
| Distinct logical CPU IDs across the input | 4,096 |
| Topology rows or IRQ records, including repeated records | 4,096 |
| CPU entries produced for retained lists | 16,384 |

A compact range is checked before expansion. Affinity lists in one host response
share the budgets, including IRQ records that overwrite earlier ones. Producing
an affinity again spends its entries again, even when it replaces an earlier
record or readback validation discards the expanded list. Raw topology rows keep
their duplicates; the rows and the first logical CPU per core count toward the
entry budget. CPU range endpoints use ASCII decimal digits within the signed
64-bit integer range; the limits bound counts, without requiring dense CPU IDs.

The initial `--as-found` reading and later state checks use the same parser and
budgets. IRQ placement readback validates affinities under those budgets before
comparing their original text. An oversized response produces a bounded error
such as `CPU topology exceeds 65536 expansion steps`, with no partial topology
or fallback placement. During preparation this refuses the Round; during final
collection it is logged as a failed host check and existing results stay available.
Ordinary multi-range lists remain sorted and unique; descending ranges and unknown
affinities remain empty, and repeated IRQs within the budgets still use the last
record. Recorded plans and results retain their format and interpretation.

## Command and transfer limits

The Rust tooling bounds each host's command output and
result transfer. These limits cover failed commands, SSH and SCP diagnostics,
directory collection and individual-file collection:

| Resource | Maximum per command or archive |
|---|---|
| Standard output and standard error together, before UTF-8 decoding | 1 GiB (1,073,741,824 bytes) |
| Compressed directory transfer or individual-file transfer, sharing the command's output budget | 1 GiB |
| Decompressed archive, including headers, padding and concatenated gzip data | 256 MiB (268,435,456 bytes) |
| Each logical archive file and total regular-file bytes written, counting repeated names again | 256 MiB |
| Archive members, including Python's hidden GNU/PAX extension headers | 100,000 |
| Path components, each name and entire path | 64 components, 255 bytes, 4,096 bytes |

Archives accept regular files and directories. Links and sparse files are refused.
The existing time limits remain: commands default to 120 seconds and transfers
to 300 seconds. Timeout or output overflow stops the command through the Rust
process runner. Its regression tests cover interruption, inherited pipes and
bounded cleanup. The Server-scale Round collects at most three hosts at once,
each with its own command and transfer budgets.

A refused transfer raises the existing redacted `HostError`. Collection errors
void the affected Round Run through its tooling error; Round Teardown still
attempts the other hosts, removes Shared Keys and closes the ledger. An error
found after extraction starts can leave partial results in the Round's output
directory, within the extraction budget. Ordinary file contents and report
formats stay the same. Individual extraction can change directory timestamps;
reports use file contents.

## Tests

The retained goldens were exported from the retired Python implementation on
CPython 3.14 on Linux. Their provenance is recorded in each fixture. An independent
JSON parser reads expected values. Never regenerate a golden from the Rust code
being tested. At the switch, executable references in matrix comments and report
instructions were changed mechanically to Rust commands. No judging expectation
changed. An amended rule needs independently justified expectations.

The default suite covers helpers, matrices, commands, host readings, judging,
reports, scheduling, provider refusals and both Round brackets. Synthetic
`scripts/bench/tests/topology-limits.json` recipes cover cumulative parsing
limits. Process and host adapter regressions cover ownership, interruption,
collection, retained files and Round Teardown. No test can opt into real AWS.

The complete analysis, report and host-fault corpora run separately in the
manually triggered Nightly workflow. Run them after changing benchmark behavior:

```sh
cargo test -p udpstp-bench --test scale_analysis --test scale_report --test scale_round -- --ignored
```

Linux real-signal tests retain the independent reference transcripts at every
scripted host-call boundary. A separate whole-Round test interrupts the second
fake launch for each Round type and checks termination, ledger closure, key
removal and surviving Round Teardown children:

```sh
UDPSTP_BENCH_INTERRUPTS=1 cargo test -p udpstp-bench --test step_interrupt
cargo build -p bench-rounds --bin fake-aws
UDPSTP_BENCH_FAKE_AWS="$PWD/target/debug/fake-aws" cargo test -p udpstp-bench --test round_interrupt
```

On Linux, `cargo test -p udpstp-bench --test obudpst -- --ignored` builds the
pinned OB-UDPST through a real local host. Qualify measurements against a defined workload and judging rules, keeping diagnostic results separate from performance claims.

## Finite receive diagnostics

`receive-probe` measures diagnostic datagrams for the optional GRO experiment in
ADR 0008. It keeps production sockets and RFC Load PDUs unchanged. The sender
fills a 1172-byte UDP payload, giving a 1200-byte IPv4 packet with ordinary
headers. Its 32-byte header is `UDPRCV01`, then big-endian 64-bit run ID,
sequence starting at zero, and sender CLOCK_MONOTONIC_RAW nanoseconds. The
remaining bytes are zero. Both endpoints use connected IPv4 sockets with DF.

Run the receiver first on a permitted logical CPU, then the sender on the other
host. Each output has adjacent `.before.txt`, `.after.txt` counter captures and
the receiver's `.ready.json` socket-readback receipt. Wait for readiness before
starting traffic. Use the same binary, source revision, run ID, count and rate.
Supply explicit local and peer ports in reverse order at the other endpoint.
For example, a tiny functional check uses the following commands in separate
terminals after replacing the addresses, CPUs and source revision.

```sh
udpstp-bench receive-probe receive --mode header-only \
  --bind 192.0.2.2:30002 --peer 192.0.2.1:30001 --cpu 2 \
  --source 0123456789012345678901234567890123456789 --run-id 42 \
  --count 100 --pps 1000 --deadline-ms 15000 --out receiver.json
udpstp-bench receive-probe send \
  --bind 192.0.2.1:30001 --peer 192.0.2.2:30002 --cpu 3 \
  --source 0123456789012345678901234567890123456789 --run-id 42 \
  --count 100 --pps 1000 --deadline-ms 3000 --out sender.json
udpstp-bench receive-probe evaluate \
  --sender sender.json --receiver receiver.json --out evaluation.json
```

`receive-probe pair` runs one matching sender/receiver observation through the
maintained Round bracket on two existing Linux hosts. Supply SSH destinations
(or `local`), explicit private endpoint addresses, the endpoint executable and
its independently retained source/build/hash identity. For example, after
replacing these example identities, addresses and permitted CPUs:

```sh
udpstp-bench receive-probe pair \
  --sender-host sender --receiver-host receiver \
  --sender 192.0.2.1:30001 --receiver 192.0.2.2:30002 \
  --sender-cpu 3 --receiver-cpu 2 --sampler-cpu 4 \
  --measurement-tool /path/to/udpstp-bench \
  --endpoint-sha256 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa \
  --endpoint-source 0123456789012345678901234567890123456789 \
  --endpoint-features default --run-id 42 --mode header-only \
  --count 20 --pps 1000 --deadline-ms 20000 --readiness-ms 10000 \
  --drain-ms 100 --out new-pair-evidence
```

The coordinator and staged endpoint may be different binaries. `identities.json`
keeps their source, feature and SHA256 identities separately. Endpoint source
and feature labels are declarations supplied with the independently pinned build
receipt. The executable hash is checked locally and after staging. Normal and
profiling payloads have separate hashes and output directories. Use
`--endpoint-profiling` only for a payload built with profiling enabled.

The operation exclusively reserves private workspaces, admits traffic after
fresh readiness, live owned PID/executable/argument checks and an owned connected
UDP socket with the expected bind/peer, and starts the
existing sampler against the numeric receiver PID. The sampler CPU must be
outside the receiver's physical core, with known nonnegative package/core IDs.
It retains placement readbacks, commands,
statuses, endpoint logs, counter captures, sampler receipts and partial results.
Readiness, SSH calls, endpoint lifetime, export and cleanup have finite bounds.
The endpoint socket deadline is at most 180 seconds. Its process wrapper allows
another 75 seconds for the existing bounded host captures and closes remaining
writers through timeout. SSH control calls take at most 15 seconds and owned
process cleanup at most 45 seconds per role. Archive exports use the maintained
300-second bound. There is one pair per invocation and no implicit retry.

Signals, early failures and panics still enter the Round Teardown. Removal
requires confirmed owned process/sampler stops and SHA256-verified exported
files. Exit 3 leaves recoverable workspaces when either is unconfirmed. Keep
those workspaces until their owned processes are stopped and their evidence is
exported and verified. A local SSH exit never establishes remote cleanup.
A completed operation can still describe an ineligible diagnostic observation.
Physical acceptance remains false. The command neither creates/deletes rented
hosts nor enforces paid billing or account cleanup. Static-host zero-cost ledger
entries do not replace the separately frozen paid physical plan and its budget.

`--mode gro` asks for UDP_GRO and records its readback or explicit unsupported
result. Ordinary datagrams without GRO metadata remain valid observations.
A genuine aggregate is required for GRO feasibility. A short final GRO segment
is split and retained as a shape failure. Header-only reads copy 32 bytes and
retain the full datagram length through MSG_TRUNC. The GRO receiver copies whole
bounded aggregates and preserves the kernel timestamp shared by their segments.

At 1 Mpps the sender offers 100 datagrams every 100 microseconds using ordinary
sendmmsg calls of 64 and 36 datagrams. Each call shares a stamp taken before that
syscall. Smaller rates use one datagram per absolute scheduling slot. There is
no GSO, spin tail or retry of refused slots. Planned, offered, kernel-accepted,
unsent and uniquely received counts stay separate. Kernel acceptance does not
prove physical departure. Each full offered-rate second and the actual elapsed
rate must meet the request. An under-offered run stays saved and is
ineligible. Per-second counts, deadline misses and lateness expose the shortfall.

Inputs cap the workload at 120 million datagrams and 120 seconds, with a hard
lifetime of at most 180 seconds and a drain of at most 10 seconds. Signals stop
socket loops within a 50 ms wait. Counter capture commands have separate 30 s
bounds. Sequence memory is at most 15 MB. Two preallocated timestamp histograms
use about 32 MB in total, with 1 microsecond bins from minus to plus 1 second,
exact sample counts and exact extrema. Outliers remain counted and make
percentiles unavailable. Saved summaries never contain the histogram arrays.
Missing or accepted sequence ranges have at most 4096 entries. If fragmentation
exceeds that limit, counts survive and exact intersection knownness is lost.
JSON inputs and outputs are capped at 4 MiB. Existing result files are preserved.

Software timestamp reports include local read-minus-kernel delay and
receive-minus-sender spread after removing the constant offset. Percentiles have
1 microsecond resolution. The reported range uses exact extrema. Clock pairs
bracket the reads. Pair width is mapping uncertainty, recorded separately from
suspected clock discontinuities. A pair wider than 1 ms is counted as wide.
An offset change beyond both pairs' uncertainty plus 1 ms and 1000 ppm of
intervening RAW time marks a suspected discontinuity. A wide startup or empty
read alone does not discard later narrow observations. Results retain pair
spans, maximum mapping uncertainty, offset changes and missing/sample counts.
This is a continuity detector, not a timestamp-error acceptance threshold.
The spread includes burst scheduling, path jitter, drift and coalescing. It is
not absolute per-packet NIC timestamp error.

The application pins its workload thread and records topology and affinity
before and after it runs. Existing helper threads are placed outside that
physical core when the allowed CPU mask permits. Host counters run outside the
measurement window. Nothing changes IRQ/RSS policy, offloads, rings,
coalescing, governors or privileges. The socket's sk_drops count is read before
close. Its unit remains kernel messages and may differ from original datagrams
with GRO. No lower-layer count is added to sequence loss. Host counter changes
retain their raw names and units as observations for the physical reviewer.
Receive packet/queue counter increases are expected traffic, not rejection
reasons. The socket's known positive discard count is judged separately.

Saved-result evaluation is portable and opens no sockets. It keeps diagnostic
completion, packet conservation, timestamp availability and physical-review
eligibility separate. Physical acceptance is always pending an independent
review against a frozen physical plan. A loopback success cannot qualify M9.
The M9 shape also requires at least one complete second at 1 Mpps, genuine GRO,
unchanged application affinity and proved receive processing on the same
physical core. SMT siblings qualify as one core when the topology proves it.

An optional `--host host.json` supplies version 1 `HostEvidence`, whose schema
is defined in `receive_probe::HostEvidence`. It names the run, receiver and
binary, topology rows, observed receive-processing CPUs, sender/sampler CPUs,
queue/IRQ/RPS/RFS knownness and counters with raw names, units and optional
before/after values. Its `receipts` map requires `topology`, `queue-before`,
`queue-after`, `irq-before`, `irq-after`, `rps-rfs-before`, `rps-rfs-after`,
`counters-before` and `counters-after`. Each entry has a relative `path` and
`sha256`. Files are bounded at 64 MiB and their hashes are checked offline.
The physical plan and reviewer must establish that these raw receipts support
the declared placement facts. Unknown counters remain unknown beside exact
endpoint conservation. `declared_placement_consistent` checks those supplied
facts, not the meaning of raw IRQ or topology receipts. Hashes establish file
identity only. Every declared sibling needs a coherent topology/online state.
A known offline sibling can accompany a separately planned same-logical-CPU
layout. Failed final affinity/helper readbacks remain explicitly unknown.
Missing placement facts prevent a consistent-declaration result.

The default suite keeps portable packet/result fixtures and CLI evaluation.
The three native process checks run explicitly or in the existing Nightly
ignored-test job, keeping real host snapshots outside the default tier.

```sh
cargo test -p udpstp-bench --test receive_probe -- --ignored
```

### Optional profiling

Normal builds contain no profiler. The installed hotpath CLI can inspect a
profile from this reproducible small Rust workload. Use `hotpath-alloc` instead
of `hotpath` to include allocation profiling.

```sh
cargo test -p udpstp-bench --features hotpath --test receive_probe \
  tiny_native_pair_writes_saved_evaluation_without_rate_gate -- --ignored --nocapture
cargo test -p udpstp-bench --features hotpath-alloc --test receive_probe \
  tiny_native_pair_writes_saved_evaluation_without_rate_gate -- --ignored --nocapture
```

Focused instrumentation covers socket batches, GRO splitting and sequence
accounting. Profiling results are diagnostics. Physical measurements and final
rate comparisons use a normal build with profiling disabled, which the saved
result records.
