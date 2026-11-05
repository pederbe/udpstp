//! Resource budgets for one CPU topology or IRQ affinity response.

use std::collections::BTreeSet;

use bench_rounds::py;

use super::super::Error;

const INPUT_BYTES: usize = 1 << 20;
const COMPONENTS: usize = 4096;
const EXPANSION: usize = 65536;
const CPUS: usize = 4096;
const RECORDS: usize = 4096;
const ENTRIES: usize = 16384;

#[derive(Default)]
pub(super) struct Budget {
    components: usize,
    expansion: usize,
    records: usize,
    entries: usize,
    cpus: BTreeSet<i64>,
}

impl Budget {
    pub(super) fn new(source: &str) -> Result<Self, Error> {
        if source.len() > INPUT_BYTES {
            return Err(exceeded(INPUT_BYTES, "input bytes"));
        }
        Ok(Self::default())
    }

    pub(super) fn record(&mut self, kind: &str) -> Result<(), Error> {
        charge(&mut self.records, 1, RECORDS, kind)
    }

    pub(super) fn cpu(&mut self, cpu: i64) -> Result<(), Error> {
        if !self.cpus.contains(&cpu) {
            if self.cpus.len() == CPUS {
                return Err(exceeded(CPUS, "distinct CPUs"));
            }
            self.cpus.insert(cpu);
        }
        Ok(())
    }

    pub(super) fn retain(&mut self) -> Result<(), Error> {
        charge(&mut self.entries, 1, ENTRIES, "retained CPU entries")
    }

    pub(super) fn cpu_list(&mut self, source: &str) -> Result<BTreeSet<i64>, Error> {
        let mut cpus = BTreeSet::new();
        for part in source.split(',') {
            charge(&mut self.components, 1, COMPONENTS, "range components")?;
            let (first, last) = part.split_once('-').unwrap_or((part, ""));
            if super::digits(first) && (last.is_empty() || super::digits(last)) {
                let start = super::parse_integer(first)?;
                let end = if last.is_empty() {
                    start
                } else {
                    super::parse_integer(last)?
                };
                // Nonnegative i64 endpoints make this inclusive length fit u64.
                let count = if end < start {
                    0
                } else {
                    (end as u64) - (start as u64) + 1
                };
                if count > (EXPANSION - self.expansion) as u64 {
                    return Err(exceeded(EXPANSION, "expansion steps"));
                }
                self.expansion += count as usize;
                if count > CPUS as u64 {
                    return Err(exceeded(CPUS, "distinct CPUs"));
                }
                // Charge duplicates and overlapping ranges before iterating.
                for cpu in start..=end {
                    self.cpu(cpu)?;
                    if !cpus.contains(&cpu) {
                        self.retain()?;
                        cpus.insert(cpu);
                    }
                }
            }
        }
        Ok(cpus)
    }
}

fn charge(used: &mut usize, count: usize, limit: usize, kind: &str) -> Result<(), Error> {
    if count > limit - *used {
        return Err(exceeded(limit, kind));
    }
    *used += count;
    Ok(())
}

fn exceeded(limit: usize, kind: &str) -> Error {
    Error(format!("CPU topology exceeds {limit} {kind}"))
}

/// Python's line breaks, streamed. Empty lines never carry topology records.
pub(super) fn lines(source: &str) -> impl Iterator<Item = &str> {
    source.split([
        '\n', '\r', '\u{b}', '\u{c}', '\u{1c}', '\u{1d}', '\u{1e}', '\u{85}', '\u{2028}',
        '\u{2029}',
    ])
}

/// Only enough fields to distinguish a record from an ignored malformed row.
pub(super) fn words(source: &str, count: usize) -> Vec<&str> {
    source
        .split(py::is_space)
        .filter(|s| !s.is_empty())
        .take(count)
        .collect()
}
