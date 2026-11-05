# bench-rounds

The general machinery of a benchmark Round: what a project needs that rents or borrows hosts, runs a matrix of measurements on them, and judges the results by rules written down beforehand. It knows nothing of udpstp; [`udpstp-bench`](../udpstp-bench) holds udpstp's own Rounds on top of it.

Part of [udpstp](https://github.com/pederbe/udpstp)'s tooling, and never published: its interface is internal until another project uses it.

## Words

- A **Round** is one pass of the benchmark tooling over a matrix on one set of hosts: priced and planned, then provisioned.
- A Round makes its **Round Runs**: each measures one point of the matrix once, and is valid or void, flagged or not.
- It ends with its **Round Teardown**, whatever happened: its Servers stopped, every resource its provider made destroyed and listed again, its keys deleted, its ledger entry closed.

udpstp's [GLOSSARY.md](../../GLOSSARY.md) defines them for udpstp's Rounds.

## Why it reproduces Python

The retired Python tooling supplied independent reference fixtures for the rules written down before each Round. Reports preserve its output, including JSON ordering, floating-point sums and rounding.

## Modules

- `aws`: the AWS provider through the `aws` CLI. `plan` builds each call's argv, the tags, the security group's rules and the user data as the Python builds them, its JSON as `json.dumps` writes it; `AwsProvider` makes the Python provider's calls in its order through a seam the CLI implements; `cli` is that CLI: it runs each call in a process group of its own, stops it at its own limit or an interrupt, reads the credentials only for when they expire, records each call where a recorder is given, and takes the real CLI only for a Round given `--live`; it also makes `AwsProvider` a provider of the Round's, whose rented hosts it reaches over ssh, refusing one without a public or a private address; `fake` is the fake CLI's model of EC2, which the `fake-aws` binary wraps for tests; `record` keeps the raw records of a Round's real calls where only their user can read them, outside the checkout, never credentials; `replay` replays them against the fake, by hand, and says where the fake answers otherwise without saying the records' values. Nothing from them is committed.
- `cost`: the most a Round can cost, the Round's and the month's caps, and the ledger of Rounds, which the Rust and the Python tools read and write alike, byte for byte.
- `counters`: the counter script, run on each host before and after each Round Run, beside its reader: a Linux host's counters as the script prints them, how far they rose over a Round Run, which ones a file lacks, and the platform's allowance counters.
- `dead_man`: a rented host's dead-man switch, byte for byte the Python's: the first boot's user data, the unit and script that arm it at every boot, and the check, run on the host, with its verdict.
- `host`: a host of the Round, over ssh or this machine. A script runs in the host's work directory: over ssh always as `ssh -T ... HOST "bash -s"` with the script on standard input, so the host's login shell parses only `bash -s`, and locally as `bash -c`. Files go in with `scp` and come back through `tar`. A call finishes, with its output as text as Python's `subprocess` gives it, fails on the host, or is interrupted, and an interrupt is never a host's error; garbage from a host, output that isn't UTF-8 or an archive that can't be unpacked, is its error, and voids only the Round Run it came in. Time limits keep the Python's int or float, as its messages print them. `each` runs a call on several hosts at once and puts an interrupt ahead of any error. It also holds the scripts with which a Round records a host's environment and quiets a rented one.
- `interrupt`: SIGINT, SIGTERM and SIGHUP, and on Windows the console's Ctrl-C, Ctrl-Break and close, only raise a flag the Round reads. The handlers go in first, for all three whatever the tool started with, and unblock them, so its children start with them unblocked and at their defaults. When the Round Teardown begins, the flag stops acting and gives the proof that the Round Teardown's steps require.
- `num`: numbers that keep Python's int or float kind where a report prints them, with Python's `statistics.median`, `max`, `sum` (compensated, as CPython 3.14 on Linux adds floats), `round`, `f` format and `repr`.
- `order`: the order Round Runs go in: each engine first in turn, a point's Round Runs spread over the repeats, the queue that tries an invalid one again a few later, and the stop rule's streaks.
- `process`: runs a program for a Round: in a process group of its own, with the script or nothing on its standard input, within a time limit, and killed with its group when the flag is raised, except in the Round Teardown. A killed program's output readers are let go about 2 s later, and drop the pipe. Its output is capped at 1 GiB; a secret's, such as the `aws` CLI's credentials, is kept in one buffer that never grows and is wiped.
- `provider`: where a Round's hosts come from, and what a Round left behind.
- `py`: `Json`, the one type every JSON file is read into, as Python's `json` reads it: objects in document order, ints and floats apart, floats parsed exactly; and written back as `json.dumps` writes it. With it, Python's `.get`, `[key]`, `or` and truthiness, and `splitlines`, `split` and `strip` with Python's line breaks and whitespace, and a text file's lines as Python iterates them. Where it can't read a text, it says whether Python's `json.loads` refuses it too, by the grammar CPython's scanner reads.
- `quote`: POSIX shell quoting, exactly `shlex.quote` and `shlex.join`.
- `redact`: what could be a key removed from a message, the word after `-a` and long hex runs, then the message cut at 400 characters.
- `round`: the Round bracket, `execute`, around a Round's body, which a project such as `udpstp-bench` gives: the ledger entry first, the Round's private key directory, the body under `catch_unwind`, then whatever happened the Round Teardown, with the flag no longer stopping anything: the body's Servers stopped, the provider's resources destroyed and what is left said, the keys removed and the ledger entry closed; and the exit status, 3 whenever something is left. With it, what stops a body, the Round's log and clock, whose writes and waits never stop the Round Teardown, and the ssh access to rented hosts.
- `round_teardown`: the provider's part of the Round Teardown, which takes the proof that the flag stopped acting. Destroying is tried three times and what is left listed again, each attempt that failed or left something is logged, a panic in the provider counts as a failed attempt, one in the log, the terminal or the clock is passed over, and what is left is said, with the commands that delete it by hand, before the ledger entry is closed.
- `sampler`: the Linux CPU, thread and UDP producer, keeping the existing JSON record format. Saved proc trees can be read through its complete record interface.
- `samples`: what a host's CPU samples say of a Round Run's window: logical CPUs busy, the busiest, softirq and irq; the sampled process's threads and wake-ups; other processes' share.
- `secrets`: the Round's private directory for its keys, 0700 on Unix under a random name, removed at the end and whenever the Round ends otherwise; random hex from the OS; and a rented host's SSH key, made there by `ssh-keygen`.
- `static_hosts`: hosts that exist already, which cost nothing, with a private work directory exclusively reserved at Round creation and guarded through preparation and Round Teardown; one it can't remove is left, with the command that removes it by hand.
- `stats`: the median, percentile and quantile the Server-scale rules were written with.
- `unpack`: a fetched `.tar.gz` unpacked with regular files and directories only, inside the destination, and within bounds of size, members, depth and name and path length that keep it quick; an archive that is refused writes nothing.
- `utc`: the times the tooling writes, from a Unix time as `datetime.fromtimestamp` rounds it, and the timestamps udpstp, OB-UDPST and the `aws` CLI write, read as `datetime.fromisoformat` reads them.

## Tests

The expected values in `tests/golden/` were exported from CPython 3.14 on Linux before the operational switch. `cargo test` checks every retained case with an independent JSON parser. Never regenerate a reference from the Rust implementation being tested. A rule change needs independent expectations and an explicit pre-registration amendment.

For AWS, `tests/golden/aws.json` holds what the Python's provider did on the Python's fake CLI in each of its tests' scenarios: each call's argv, the fake's answer and the state it left, and each result. `tests/aws_golden.rs` checks the fake and `AwsProvider` against it, call for call. `tests/aws_cli.rs` runs the same scenarios through the CLI adapter on the `fake-aws` binary, each call a process of its own in an environment where no real AWS configuration or credentials can be found.

`tests/golden/aws-failures.json` holds what the Python's provider and its Round Teardown did on the Rust `fake-aws` with a launch failing for good, throttled past its retries or interrupted mid-call, an instance that outlives the Round Teardown and a security group held: the Python's fake can't fail or pause a call. `tests/aws_cli.rs` holds the Rust provider and its Round Teardown to it, the log read into the Python's words a whole line at a time, and checks that whatever the Round made is gone or said to be left. It holds the same of a Round Teardown whose own calls fail for good or run past the CLI's limit.

On Unix, `tests/aws_cli.rs` also runs scripts of its own as the CLI and as ssh: the CLI runs in the environment it is given and nothing else; the credentials are read for their expiry alone and never recorded; a Round recorded through the adapter replays on the fake without a difference; a record that can't be written ends the recording, never the Round; only what reached the CLI is recorded; and the rented hosts are reached with the Round's own ssh, known-hosts file and key. Its tests, and those of `tests/host.rs`, run one at a time, since a script another test's fork holds open for writing can't be run ("Text file busy").

`tests/execute.rs` runs the Round bracket around fake bodies and providers that succeed, fail, panic or are interrupted, and a ledger or key directory that can't be written: the Round Teardown runs whatever happened once the ledger entry is written, what is left is said before the entry is closed, and the keys go.

`tests/dead_man.rs` boots the user data and runs the check under a real shell, with stand-ins for what they call (Linux only).

AWS call recordings can contain account identifiers and addresses. Keep them private and outside the repository. To replay your own recording against the fake, run:

```sh
UDPSTP_BENCH_CALLS=PATH_TO_RECORDING \
    cargo test -p bench-rounds --test aws_replay -- --nocapture
```

It lists each place the fake answers otherwise: the exit status, the error's code, a field of an answer the tool reads, a call the fake can't take. It says each by the call's number, its operation and the field, never a value of the records, which hold the account and the addresses, so its list can be shown. Each difference then becomes a small synthetic test of the fake, made up rather than copied from the records. Resources the Round didn't make, such as another Round's in the reaper's lists, show as differences the fake can't have. Without `UDPSTP_BENCH_CALLS` the test does nothing.

The runner's own tests run everywhere: `tests/runner.rs` has its own `main` and starts itself again as the child, so it needs neither a shell nor a socket. The adapters' tests that need a Unix shell run on Linux only: `tests/process.rs` and `tests/host.rs`, and the signal checks in `tests/interrupt.rs`. That one has its own `main` too, which installs the handlers first as a Round's does. It starts itself again with the three signals blocked, ignored or both, and checks that the probe takes them, that its children start with them unblocked and at their defaults, and that a child and a Server started the Round's way end on SIGTERM; a control probe without the handlers shows the same children outliving it. Elsewhere it checks only that the handlers install. One test is ignored, since it needs a controlling terminal: `a_child_that_reads_the_terminal` in `tests/process.rs`, run in tmux.
