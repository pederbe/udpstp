//! The Server-scale Round's host I/O, around the pure schedule and judging.
//! The order, shell text and evidence files follow scale.py's ScaleRound.
//! The generic bracket owns the ledger, secrets and provider teardown.

mod placement;
mod prepare;
mod runs;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use bench_rounds::host::{self, HostError, Limit, RUN_LIMIT, RemotePath, Shell, Stop};
use bench_rounds::interrupt::RoundTeardown;
use bench_rounds::num::Num;
use bench_rounds::process::Runner;
use bench_rounds::provider::Provider;
use bench_rounds::py::{self, EMPTY_DICT, Json};
use bench_rounds::round::{self, Clock, Log, RoundBody, Start, Stopped};
use bench_rounds::secrets;
use indexmap::IndexMap;

use super::judge::{self, num_json, set};
use super::matrix::{self, Matrix, ScaleRun, dict, get, int, integer, list, repr, string, text};
use super::schedule::{Schedule, Step};
use super::{Error, commands, hosts, report};
use crate::cleanup;
use crate::first_round::round::{MakeKey, Obudpst, STOP_BEFORE_EXPIRY_S, make_keys};

pub const ROLES: [&str; 3] = ["server", "client1", "client2"];
const CLIENTS: [&str; 2] = ["client1", "client2"];

/// Paths and switches supplied by the command line.
#[derive(Clone, Debug)]
pub struct Options {
    pub udpstp: PathBuf,
    pub measurement_tool: Option<PathBuf>,
    pub prototype: Option<PathBuf>,
    pub obudpst: Option<Obudpst>,
    pub raise_buffers: bool,
    pub hold_irqs: bool,
    pub as_found: bool,
    pub udpstp_port: u16,
    pub obudpst_port: u16,
    pub out: PathBuf,
    pub ssh_dir: Option<PathBuf>,
    pub matrix: PathBuf,
    pub scripts: PathBuf,
    pub obudpst_script: PathBuf,
}

/// One Round, including everything its teardown may need after a failure.
pub struct ScaleRound<'a> {
    options: Options,
    matrix: Matrix,
    clock: &'a dyn Clock,
    runner: Runner,
    hosts: IndexMap<String, Box<dyn Shell + 'a>>,
    background: Vec<(String, String)>,
    irqbalance_stopped: Vec<String>,
    topology: IndexMap<String, (Vec<i64>, Vec<i64>)>,
    irqs: IndexMap<String, IndexMap<i64, i64>>,
    found: Json,
    counter_names: Json,
    confined: Json,
    host_states: Json,
    results: Vec<Json>,
    client_env: BTreeMap<String, String>,
    cases: IndexMap<String, Json>,
    rented: bool,
    ssh_key: Option<&'a MakeKey>,
}

impl From<Error> for Stopped {
    fn from(error: Error) -> Self {
        Stopped::Failed(error.to_string())
    }
}

fn failed(error: impl std::fmt::Display) -> Stopped {
    Stopped::Failed(error.to_string())
}

fn remote(path: &str) -> Result<RemotePath, Stopped> {
    RemotePath::new(path).map_err(failed)
}

fn ints(values: &[i64]) -> Json {
    Json::List(values.iter().copied().map(int).collect())
}

fn environment(value: &Json) -> BTreeMap<String, String> {
    value
        .as_dict()
        .into_iter()
        .flatten()
        .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_owned())))
        .collect()
}

fn env_text(value: &BTreeMap<String, String>) -> String {
    value
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn write_json(path: &Path, value: &Json, indent: bool) -> Result<(), Stopped> {
    let source = if indent {
        py::dumps_indent(value, 1)
    } else {
        py::dumps(value)
    };
    fs::write(path, source).map_err(failed)
}

fn limit(value: Num) -> Result<Limit, Stopped> {
    Limit::new(value).ok_or_else(|| failed("a run's time limit is out of range"))
}

impl<'a> ScaleRound<'a> {
    pub fn new(
        options: Options,
        matrix: Matrix,
        clock: &'a dyn Clock,
        runner: Runner,
    ) -> Result<Self, Error> {
        let cases = list(get(&matrix.plan, "cases"))?
            .iter()
            .map(|case| Ok((text(get(case, "name"))?.to_owned(), case.clone())))
            .collect::<Result<_, Error>>()?;
        let client_env = environment(get(&matrix.plan, "client_env"));
        Ok(Self {
            options,
            matrix,
            clock,
            runner,
            hosts: IndexMap::new(),
            background: Vec::new(),
            irqbalance_stopped: Vec::new(),
            topology: IndexMap::new(),
            irqs: IndexMap::new(),
            found: EMPTY_DICT.clone(),
            counter_names: EMPTY_DICT.clone(),
            confined: EMPTY_DICT.clone(),
            host_states: EMPTY_DICT.clone(),
            results: Vec::new(),
            client_env,
            cases,
            rented: false,
            ssh_key: None,
        })
    }

    pub fn with_ssh_key(mut self, make: &'a MakeKey) -> Self {
        self.ssh_key = Some(make);
        self
    }

    pub fn results(&self) -> &[Json] {
        &self.results
    }

    /// Everything after provisioning, also used by the scripted-host comparisons.
    pub fn steps(
        &mut self,
        hosts: IndexMap<String, Box<dyn Shell + 'a>>,
        provider: &mut dyn Provider,
        keys: &Path,
        deadline: f64,
        log: &Log,
    ) -> Result<(), Stopped> {
        self.hosts = hosts;
        self.rented = provider.rented();
        self.prepare(provider, keys, log)?;
        self.check_path(log)?;
        let mut schedule = Schedule::new(&self.matrix)?;
        loop {
            let next = schedule.next(self.clock.now(), deadline)?;
            for line in next.logs {
                log.line(&line);
            }
            match next.step {
                Step::Run(run) => {
                    let record = self.run_one(&run, log)?;
                    for line in schedule.record(record)? {
                        log.line(&line);
                    }
                }
                Step::SaveConfinement(value) => {
                    self.confined = schedule.confined().clone();
                    write_json(
                        &self.options.out.join("scale/confinement.json"),
                        &value,
                        true,
                    )?;
                }
                Step::Done | Step::Stopped(_) | Step::OutOfTime => break,
            }
        }
        self.collect(log)
    }

    fn host(&self, role: &str) -> &dyn Shell {
        // prepare checks all required roles before any host-dependent step.
        self.hosts[role].as_ref()
    }

    fn engines(&self) -> BTreeSet<&str> {
        self.matrix.runs.iter().map(|r| r.engine.as_str()).collect()
    }

    fn prototypes(&self) -> bool {
        self.engines().iter().any(|e| e.starts_with("prototype-"))
    }

    fn sleep(&self, seconds: f64) -> Result<(), Stopped> {
        let duration = Duration::try_from_secs_f64(seconds).map_err(failed)?;
        Ok(self.clock.sleep(duration)?)
    }

    fn stop(&mut self, role: &str, pidfile: &str, wait: i64, first: i64) -> Result<(), Stopped> {
        let result = self.host(role).run(
            &commands::stop(pidfile, wait, first),
            limit(Num::Int((first + wait + 30).into()))?,
        );
        self.background.retain(|(r, p)| r != role || p != pidfile);
        result.map(|_| ()).map_err(Into::into)
    }

    fn wait_listening(&self, role: &str, directory: &str) -> Result<(), Stopped> {
        let done = self
            .host(role)
            .run(&commands::wait_listening(directory), Limit::secs(30))?;
        if !done.success() {
            return Err(failed(format!(
                "{role}: udpstp's Server didn't start: {}",
                py::strip(&done.stdout)
            )));
        }
        Ok(())
    }

    fn collect(&mut self, log: &Log) -> Result<(), Stopped> {
        match self.check_irqs(log) {
            Ok(()) => {}
            Err(Stopped::Interrupted) => return Err(Stopped::Interrupted),
            Err(error) => log.line(&format!("checking the NIC's interrupts failed: {error}")),
        }
        match self.record_host_states("end") {
            Ok(()) => {}
            Err(Stopped::Interrupted) => return Err(Stopped::Interrupted),
            Err(error) => log.line(&format!(
                "reading the hosts' state at the end failed: {error}"
            )),
        }
        let outcome =
            report::RoundFiles::load(&self.options.out).and_then(|out| report::report(&out, None));
        match outcome {
            Ok(source) => {
                let path = self.options.out.join("scale/report.md");
                fs::write(&path, source).map_err(failed)?;
                log.line(&format!("report: {}", path.display()));
            }
            Err(error) => log.line(&format!("the report failed: {}", runs::exception(&error))),
        }
        Ok(())
    }
}

impl RoundBody for ScaleRound<'_> {
    fn run(&mut self, provider: &mut dyn Provider, start: &Start<'_>) -> Result<(), Stopped> {
        let _key = make_keys(start.keys)?;
        let runner = self.runner.clone();
        let make = self.ssh_key;
        let access = round::access(
            provider,
            start.keys,
            self.options.ssh_dir.as_deref(),
            &mut |keys| match make {
                Some(make) => make(keys),
                None => secrets::ssh_key(keys, &runner),
            },
        )?;
        let hosts = provider.create(start.round, start.expiry, &ROLES, &access)?;
        let listed = hosts
            .iter()
            .map(|(role, h)| {
                format!(
                    "{}: ({}, {})",
                    repr(&string(role)),
                    repr(&string(h.destination())),
                    repr(&string(h.address()))
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        start.log.line(&format!("hosts: {{{listed}}}"));
        self.steps(
            hosts,
            provider,
            start.keys.path(),
            start.expiry - STOP_BEFORE_EXPIRY_S,
            start.log,
        )
    }

    fn stop_servers(&mut self, begun: &RoundTeardown, log: &Log) {
        let _ = begun;
        for (role, pidfile) in self.background.clone() {
            let _ = cleanup::guarded(|| self.stop(&role, &pidfile, 5, 0));
        }
        cleanup::final_sweep(&self.hosts, log);
        for role in &self.irqbalance_stopped {
            let _ = cleanup::guarded(|| {
                self.host(role)
                    .run("sudo -n systemctl start irqbalance", Limit::secs(30))
            });
        }
    }

    fn made(&self) -> String {
        format!("{} Tests", self.results.len())
    }
}
