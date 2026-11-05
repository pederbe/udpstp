//! Reading host topology and settings, without contacting a host.

use std::collections::{BTreeMap, BTreeSet};
use std::{fs, path::Path};

use bench_rounds::py::{self, Json};
use indexmap::IndexMap;
use sha2::{Digest, Sha256};

mod scripts;
pub use scripts::*;
mod limits;
use limits::Budget;

use super::{
    Error,
    matrix::{dict, int, string},
};

/// Contiguous shares of Clients, numbered from one.
pub fn split(count: i64, parts: i64) -> Result<Vec<(i64, i64)>, Error> {
    if count < 0 || parts < 1 {
        return Err(Error(
            "a Client split needs a nonnegative count and positive parts".into(),
        ));
    }
    let mut first = 1_i64;
    (0..parts)
        .map(|k| {
            let n = count / parts + i64::from(k < count % parts);
            let part = (first, n);
            first = first
                .checked_add(n)
                .ok_or_else(|| Error("Client number overflow".into()))?;
            Ok(part)
        })
        .collect()
}

/// Logical CPUs and the first listed logical CPU of each core.
pub fn topology(source: &str) -> Result<(Vec<i64>, Vec<i64>), Error> {
    let mut budget = Budget::new(source)?;
    let mut cores = BTreeMap::new();
    let mut cpus = Vec::new();
    for line in limits::lines(source) {
        let line = py::strip(line);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        budget.record("topology rows")?;
        let mut fields = line.split(',');
        let mut number = || {
            fields
                .next()
                .and_then(|s| s.trim().parse::<i64>().ok())
                .ok_or_else(|| Error("invalid CPU,Core line in lscpu's output".into()))
        };
        let cpu = number()?;
        let core = number()?;
        budget.cpu(cpu)?;
        budget.retain()?;
        cpus.push(cpu);
        if !cores.contains_key(&core) {
            budget.retain()?;
        }
        cores.entry(core).or_insert(cpu);
    }
    if cpus.is_empty() {
        return Err(Error("no CPUs in lscpu's output".into()));
    }
    cpus.sort_unstable();
    let mut per_core: Vec<_> = cores.into_values().collect();
    per_core.sort_unstable();
    Ok((cpus, per_core))
}

/// Queue interrupts as `(IRQ, queue)` pairs in the host's order.
pub fn queue_irqs(source: &str) -> Result<Vec<(i64, i64)>, Error> {
    let mut budget = Budget::new(source)?;
    let mut found = Vec::new();
    for line in limits::lines(source) {
        let parts = limits::words(line, 2);
        if parts.len() >= 2 && digits(parts[0]) && digits(parts[1]) {
            budget.record("IRQ records")?;
            found.push((parse_integer(parts[0])?, parse_integer(parts[1])?));
        }
    }
    Ok(found)
}

/// IRQ placement fixed by the pre-registration, the Server's second
/// logical CPUs first, or all its CPUs where SMT is absent.
pub fn irq_layout(
    queues: &[(i64, i64)],
    cpus: &[i64],
    per_core: &[i64],
    role: &str,
) -> Result<IndexMap<i64, i64>, Error> {
    let others: Vec<_> = cpus
        .iter()
        .copied()
        .filter(|c| !per_core.contains(c))
        .collect();
    let targets = if role == "server" && !others.is_empty() {
        &others
    } else {
        cpus
    };
    if targets.is_empty() && !queues.is_empty() {
        return Err(Error("an IRQ layout needs CPUs".into()));
    }
    Ok(queues
        .iter()
        .map(|(irq, queue)| {
            (
                *irq,
                targets[queue.rem_euclid(targets.len() as i64) as usize],
            )
        })
        .collect())
}

/// A sorted, unique selection of logical CPUs, used for confinement and
/// NIC placement. Raw topology rows remain separate because their order
/// and duplicates are part of the reference reader's output.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CpuSet(BTreeSet<i64>);

impl CpuSet {
    /// Invalid range components are ignored as in Python. Resource limits
    /// apply before expanding ranges (see the benchmark topology guide).
    pub fn parse(source: &str) -> Result<Self, Error> {
        Ok(Self(Budget::new(source)?.cpu_list(source)?))
    }

    /// A range list for taskset and the prototype, such as `0-3,6`.
    pub fn text(&self) -> String {
        let mut cpus = self.0.iter().copied().peekable();
        let mut parts = Vec::new();
        let mut start = None;
        while let Some(cpu) = cpus.next() {
            let first = *start.get_or_insert(cpu);
            if cpus
                .peek()
                .is_none_or(|next| Some(*next) != cpu.checked_add(1))
            {
                parts.push(if first == cpu {
                    cpu.to_string()
                } else {
                    format!("{first}-{cpu}")
                });
                start = None;
            }
        }
        parts.join(",")
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn contains(&self, cpu: &i64) -> bool {
        self.0.contains(cpu)
    }
    pub fn is_subset(&self, other: &Self) -> bool {
        self.0.is_subset(&other.0)
    }
    pub fn is_disjoint(&self, other: &Self) -> bool {
        self.0.is_disjoint(&other.0)
    }
    pub fn difference(&self, other: &Self) -> Self {
        self.0.difference(&other.0).copied().collect()
    }
    pub fn intersection(&self, other: &Self) -> Self {
        self.0.intersection(&other.0).copied().collect()
    }
}

impl FromIterator<i64> for CpuSet {
    fn from_iter<T: IntoIterator<Item = i64>>(iter: T) -> Self {
        Self(iter.into_iter().collect())
    }
}

impl IntoIterator for CpuSet {
    type Item = i64;
    type IntoIter = std::collections::btree_set::IntoIter<i64>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl Extend<i64> for CpuSet {
    fn extend<T: IntoIterator<Item = i64>>(&mut self, iter: T) {
        self.0.extend(iter);
    }
}

/// Logical CPUs from a range list, in ascending order without duplicates.
pub fn cpu_list(source: &str) -> Result<CpuSet, Error> {
    CpuSet::parse(source)
}

fn digits(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit())
}
fn parse_integer(value: &str) -> Result<i64, Error> {
    value
        .parse()
        .map_err(|_| Error("host counter is out of range".into()))
}

/// Socket-buffer limits and effective IRQ affinities read without sudo.
pub fn as_found(source: &str) -> Result<Json, Error> {
    let mut budget = Budget::new(source)?;
    let mut values = IndexMap::from([
        ("rmem_max".to_string(), Json::Null),
        ("wmem_max".into(), Json::Null),
        ("irqbalance".into(), Json::Null),
        ("interrupts".into(), Json::Dict(IndexMap::new())),
    ]);
    let mut interrupts = IndexMap::new();
    for line in limits::lines(source) {
        let words = limits::words(line, 5);
        match words.as_slice() {
            [name, value] if ["rmem_max", "wmem_max"].contains(name) && digits(value) => {
                values.insert((*name).into(), int(parse_integer(value)?));
            }
            ["irqbalance", state] => {
                values.insert("irqbalance".into(), string(state));
            }
            ["irq", irq, cpus, name] if digits(irq) => {
                budget.record("IRQ records")?;
                let cpus = budget.cpu_list(cpus)?;
                interrupts.insert(
                    (*irq).into(),
                    dict([
                        ("cpus", Json::List(cpus.into_iter().map(int).collect())),
                        ("name", string(name)),
                    ]),
                );
            }
            _ => {}
        }
    }
    values.insert("interrupts".into(), Json::Dict(interrupts));
    Ok(Json::Dict(values))
}

/// IRQ readback in source order, preserving exact affinity text for comparison.
/// The same budgets as `as_found` apply before a placement mismatch is tolerated.
pub fn irq_affinities(source: &str) -> Result<Vec<(String, String)>, Error> {
    let mut budget = Budget::new(source)?;
    let mut found = Vec::new();
    for line in limits::lines(source) {
        let words = limits::words(line, 3);
        if let [irq, cpus] = words.as_slice() {
            budget.record("IRQ records")?;
            budget.cpu_list(cpus)?;
            found.push(((*irq).into(), (*cpus).into()));
        }
    }
    Ok(found)
}

/// Power, kernel, network and thermal-throttle readings in source order.
pub fn host_state(source: &str) -> Result<Json, Error> {
    const WORDS: &[&str] = &[
        "governor",
        "driver",
        "epp",
        "no_turbo",
        "idle_driver",
        "idle_governor",
        "kernel",
        "preempt",
        "hz",
        "scheduler",
        "sched_ext",
        "wifi",
    ];
    let mut state = IndexMap::from([
        ("power".into(), Json::List(Vec::new())),
        ("links".into(), Json::List(Vec::new())),
    ]);
    for line in py::splitlines(source) {
        let (word, rest) = py::strip(line)
            .split_once(' ')
            .unwrap_or((py::strip(line), ""));
        if word == "power" {
            let mut parts = py::split(rest);
            parts.resize(4, "-");
            if let Some(Json::List(power)) = state.get_mut("power") {
                power.push(Json::List(parts.into_iter().map(string).collect()));
            }
        } else if word == "link" {
            if let Some(Json::List(links)) = state.get_mut("links") {
                links.push(string(py::strip(rest)));
            }
        } else if WORDS.contains(&word) {
            state.insert(word.into(), string(py::strip(rest)));
        } else if ["core_throttle:", "package_throttle:"].contains(&word) && digits(py::strip(rest))
        {
            state.insert(
                word.trim_end_matches(':').into(),
                int(parse_integer(py::strip(rest))?),
            );
        }
    }
    Ok(Json::Dict(state))
}

/// A rented ENA host and an existing LAN host account for NIC loss
/// through different kernel counters (the home-lab pre-registration).
pub fn nic_receive_drops(provider: &str) -> &'static str {
    if provider == "aws" {
        "rx_drops"
    } else {
        "rx_missed"
    }
}

/// A sorted CPU list for taskset and the prototype, such as `0-3,6`.
pub fn cpu_text(cpus: &[i64]) -> String {
    cpus.iter().copied().collect::<CpuSet>().text()
}

/// The exact binary uploaded to a host, as a hexadecimal SHA-256 stamp.
pub fn sha256(path: &Path) -> Result<String, Error> {
    let bytes = fs::read(path).map_err(|e| Error(e.to_string()))?;
    Ok(Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// The build's commit sidecar; a missing file records why the commit is
/// unknown, while invalid UTF-8 fails the stamp as in the reference.
pub fn binary_commit(path: &Path) -> Result<String, Error> {
    let mut sidecar = path.as_os_str().to_owned();
    sidecar.push(".commit");
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let bytes = match fs::read(sidecar) {
        Ok(bytes) => bytes,
        Err(_) => return Ok(format!("unknown: no {name}.commit beside the binary")),
    };
    let source = String::from_utf8(bytes).map_err(|e| Error(e.to_string()))?;
    let source = source.replace("\r\n", "\n").replace('\r', "\n");
    let commit = py::strip(&source);
    Ok(if commit.is_empty() {
        format!("unknown: {name}.commit is empty")
    } else {
        commit.into()
    })
}
