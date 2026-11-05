//! Cleanup shared by both Rounds. During the Round Teardown, one host's
//! failure or panic must not skip another host or a later cleanup phase.

use std::panic::{AssertUnwindSafe, catch_unwind};

use bench_rounds::{
    host::{self, HostError, Limit, Shell, Stop},
    py,
    round::Log,
};
use indexmap::IndexMap;

use crate::{first_round::matrix::repr_str, scale::hosts::STRAYS_SCRIPT};

pub(crate) fn guarded<T>(call: impl FnOnce() -> T) -> Result<T, String> {
    catch_unwind(AssertUnwindSafe(call)).map_err(|panic| {
        let said = panic
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
            .unwrap_or("no message");
        format!("panicked: {said}")
    })
}

pub(crate) fn strays(
    hosts: &IndexMap<String, Box<dyn Shell + '_>>,
    log: &Log,
    limit: Limit,
    left_by: &str,
    or_say: bool,
) -> Result<(), Stop> {
    let found = host::each(hosts.values().map(|host| {
        move || {
            if or_say {
                // Keep each failure as an answer, so even a panic cannot lose
                // successful sweeps or prevent their results being logged.
                Ok(guarded(|| host.run_checked(STRAYS_SCRIPT, limit))
                    .unwrap_or_else(|why| Err(HostError::new(why).into())))
            } else {
                host.run_checked(STRAYS_SCRIPT, limit).map(Ok)
            }
        }
    }))?;
    for (role, done) in hosts.keys().zip(found) {
        match done {
            Err(e) => log.line(&format!(
                "{role}: the stray sweep failed; check its work directory by hand: {e}"
            )),
            Ok(done) if !py::strip(&done.stdout).is_empty() => {
                let pids = py::split(&done.stdout)
                    .iter()
                    .map(|p| repr_str(p))
                    .collect::<Vec<_>>()
                    .join(", ");
                log.line(&format!(
                    "{role}: killed what {left_by} left running: [{pids}]"
                ));
            }
            Ok(_) => {}
        }
    }
    Ok(())
}

pub(crate) fn final_sweep(hosts: &IndexMap<String, Box<dyn Shell + '_>>, log: &Log) {
    let outcome = guarded(|| strays(hosts, log, Limit::secs(30), "the round", true));
    let why = match outcome {
        Ok(Ok(())) => return,
        Ok(Err(e)) => e.to_string(),
        Err(why) => why,
    };
    log.line(&format!(
        "the stray sweep failed; check the work directories by hand: {why}"
    ));
}
