//! The local check of a Round's recorded AWS calls against the fake
//!, run by
//! hand on the raw records where they are, never on committed data:
//! nothing of G's live calls is committed. `UDPSTP_BENCH_CALLS` names a
//! Round's directory of raw records; without it the test does nothing.
//!
//! ```sh
//! UDPSTP_BENCH_CALLS=~/.local/state/udpstp-bench/calls/<round id> \
//!     cargo test -p bench-rounds --test aws_replay -- --nocapture
//! ```
//!
//! It prints each difference by the call's number, its operation and the
//! field, never a value of the records, which hold the account and the
//! addresses, and fails if there is any. Each difference then becomes a
//! small synthetic test of the fake, made up, not copied from the
//! records.

use std::path::Path;

use bench_rounds::aws::{record, replay};

#[test]
fn recorded_calls_replay_on_the_fake() {
    let Some(dir) = std::env::var_os("UDPSTP_BENCH_CALLS") else {
        eprintln!("UDPSTP_BENCH_CALLS isn't set: no recorded calls are replayed");
        return;
    };
    let records = match record::read(Path::new(&dir)) {
        Ok(records) => records,
        // An I/O error names the file, not what is in it.
        Err(e) => panic!("the raw records can't be read: {e}"),
    };
    let differences = replay::replay(&records);
    for difference in &differences {
        eprintln!("{difference}");
    }
    eprintln!(
        "{} recorded calls replayed on the fake: {} differences",
        records.len(),
        differences.len()
    );
    assert!(
        differences.is_empty(),
        "the fake answers otherwise in {} places, listed above",
        differences.len()
    );
}
