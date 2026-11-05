//! CPU placement and the checks that keep a run's placement meaningful.

use super::*;
use hosts::CpuSet;

impl ScaleRound<'_> {
    pub(super) fn nic_cpus(&self) -> Result<CpuSet, Stopped> {
        let placed: CpuSet = self
            .irqs
            .get("server")
            .into_iter()
            .flat_map(|v| v.values().copied())
            .collect();
        if !placed.is_empty() {
            return Ok(placed);
        }
        let mut cpus = CpuSet::default();
        for irq in get(get(&self.found, "server"), "interrupts")
            .as_dict()
            .into_iter()
            .flat_map(|v| v.values())
        {
            for cpu in list(get(irq, "cpus"))? {
                cpus.extend([integer(cpu)?]);
            }
        }
        Ok(cpus)
    }

    pub(super) fn server_cpus(&self, run: &ScaleRun) -> Result<Vec<i64>, Stopped> {
        if run.kind == "confine" {
            return Ok(hosts::cpu_list(&run.cpus)?.into_iter().collect());
        }
        if run.cpus == "confined" {
            let case = get(
                self.cases.get(&run.case).unwrap_or(&EMPTY_DICT),
                "confined_as",
            );
            let case = if case.truthy() {
                text(case)?
            } else {
                &run.case
            };
            let chosen = get(&self.confined, case);
            if chosen.is_null() {
                return Err(failed(format!(
                    "server: no logical CPUs chosen for {case}: the calibration step didn't end"
                )));
            }
            return Ok(hosts::cpu_list(text(chosen)?)?.into_iter().collect());
        }
        Ok(self.topology["server"].0.clone())
    }

    pub(super) fn collector_cpu(&self, cpus: &[i64]) -> Result<i64, Stopped> {
        let every = &self.topology["server"].0;
        if every.iter().all(|c| cpus.contains(c)) {
            return every
                .last()
                .copied()
                .ok_or_else(|| failed("the Server host has no CPUs"));
        }
        let outside: Vec<_> = every
            .iter()
            .copied()
            .filter(|c| !cpus.contains(c))
            .collect();
        let nic = self.nic_cpus()?;
        outside
            .iter()
            .rev()
            .find(|c| !nic.contains(c))
            .or_else(|| outside.last())
            .copied()
            .ok_or_else(|| failed("the Server host has no logical CPU for the sampler"))
    }

    pub(super) fn obudpst_cpus(&self) -> Result<Option<Vec<i64>>, Stopped> {
        if !get(&self.matrix.plan, "obudpst_pinned").truthy() {
            return Ok(None);
        }
        let (cpus, per_core) = &self.topology["server"];
        let mut taken = self.nic_cpus()?;
        if taken.is_empty() {
            taken.extend(cpus.iter().filter(|c| !per_core.contains(c)).copied());
        }
        let free: Vec<_> = cpus
            .iter()
            .copied()
            .filter(|c| !taken.contains(c))
            .collect();
        Ok(Some(if free.is_empty() { cpus.clone() } else { free }))
    }

    pub(super) fn obudpst_where(&self, run: &ScaleRun) -> Result<Option<Vec<i64>>, Stopped> {
        let cpus = self.server_cpus(run)?;
        if cpus != self.topology["server"].0 {
            Ok(Some(cpus))
        } else {
            self.obudpst_cpus()
        }
    }

    pub(super) fn obudpst_ports(&self, run: Option<&ScaleRun>) -> Result<Vec<i64>, Stopped> {
        let explicit = get(&self.matrix.plan, "obudpst_instances");
        let count = if explicit.truthy() {
            integer(explicit)?
        } else if let Some(run) = run {
            let cpus = self.server_cpus(run)?;
            (if cpus != self.topology["server"].0 {
                cpus.len()
            } else {
                self.topology["server"].1.len()
            }) as i64
        } else {
            let mut most = self.topology["server"].1.len();
            for candidate in self.candidates()? {
                most = most.max(hosts::cpu_list(candidate)?.len());
            }
            most as i64
        };
        Ok((0..count)
            .map(|k| i64::from(self.options.obudpst_port) + k)
            .collect())
    }

    pub(super) fn server_address(&self, port: i64) -> String {
        format!("{}:{port}", self.host("server").address())
    }

    fn candidates(&self) -> Result<Vec<&str>, Error> {
        match get(get(&self.matrix.plan, "confine"), "candidates") {
            Json::Null => Ok(Vec::new()),
            value => list(value)?.iter().map(text).collect(),
        }
    }

    pub(super) fn check_confinement(&self, log: &Log) -> Result<(), Stopped> {
        let every: CpuSet = self.topology["server"].0.iter().copied().collect();
        let nic = self.nic_cpus()?;
        let candidates = self.candidates()?;
        for candidate in &candidates {
            let cpus = hosts::cpu_list(candidate)?;
            if !cpus.is_subset(&every) {
                return Err(failed(format!(
                    "[confine]: the Server host has no logical CPU {}",
                    cpus.difference(&every).text()
                )));
            }
            if !cpus.is_disjoint(&nic) {
                return Err(failed(format!(
                    "[confine]: {candidate} takes logical CPU {}, where the Server host's NIC's interrupts go: they stay outside the confined CPUs",
                    cpus.intersection(&nic).text()
                )));
            }
            if every.difference(&cpus).is_subset(&nic) {
                return Err(failed(format!(
                    "[confine]: {candidate} leaves the Server host no logical CPU for the sampler"
                )));
            }
        }
        if get(&self.matrix.plan, "confine").truthy() {
            let cpus = nic.text();
            let named = if cpus.is_empty() {
                "logical CPUs unknown".into()
            } else {
                format!(
                    "logical CPU{} {cpus}",
                    if cpus.contains([',', '-']) { "s" } else { "" }
                )
            };
            log.line(&format!("the calibration step's candidates: {}; the Server host's NIC's interrupts on {named}", candidates.join("; ")));
        }
        Ok(())
    }

    pub(super) fn set_irqs(&mut self, role: &str, log: &Log) -> Result<(), Stopped> {
        let (cpus, per_core) = &self.topology[role];
        let done = self
            .host(role)
            .run(&hosts::queue_irqs_script("/proc/interrupts"), RUN_LIMIT)?;
        let layout = hosts::irq_layout(&hosts::queue_irqs(&done.stdout)?, cpus, per_core, role)?;
        let problem = if layout.is_empty() {
            Some("found no queue interrupts of its NIC".to_owned())
        } else {
            let done = self
                .host(role)
                .run(&hosts::irq_command(&layout), RUN_LIMIT)?;
            let got: BTreeMap<_, _> = hosts::irq_affinities(&done.stdout)?
                .into_iter()
                .filter_map(|(irq, cpus)| {
                    if !irq.is_empty() && irq.bytes().all(|c| c.is_ascii_digit()) {
                        irq.parse::<i64>().ok().map(|n| (n, cpus))
                    } else {
                        None
                    }
                })
                .collect();
            let wrong = layout
                .iter()
                .filter(|(irq, cpu)| got.get(irq) != Some(&cpu.to_string()))
                .map(|(irq, _)| {
                    format!(
                        "{irq}: {}",
                        got.get(irq)
                            .map(|s| repr(&string(s)))
                            .unwrap_or_else(|| "None".into())
                    )
                })
                .collect::<Vec<_>>();
            if wrong.is_empty() {
                let mut layout: Vec<_> = layout.into_iter().collect();
                layout.sort_unstable();
                self.irqs
                    .insert(role.to_owned(), layout.into_iter().collect());
                None
            } else {
                Some(format!(
                    "its NIC's queue interrupts didn't move as set: {{{}}}",
                    wrong.join(", ")
                ))
            }
        };
        if let Some(problem) = problem {
            if self.rented {
                return Err(failed(format!("{role}: {problem}")));
            }
            log.line(&format!(
                "{role}: {problem}; its interrupts stay where they were"
            ));
        }
        Ok(())
    }

    pub(super) fn check_irqs(&self, log: &Log) -> Result<(), Stopped> {
        for (role, layout) in &self.irqs {
            let script = layout
                .keys()
                .map(|irq| format!("echo \"{irq} $(cat /proc/irq/{irq}/smp_affinity_list)\";"))
                .collect::<Vec<_>>()
                .join(" ");
            let done = self.host(role).run(&script, RUN_LIMIT)?;
            let got: BTreeMap<_, _> = hosts::irq_affinities(&done.stdout)?.into_iter().collect();
            let moved = Json::Dict(
                layout
                    .iter()
                    .filter(|(irq, cpu)| got.get(&irq.to_string()) != Some(&cpu.to_string()))
                    .map(|(irq, _)| {
                        (
                            irq.to_string(),
                            got.get(irq.to_string().as_str())
                                .map(|s| string(s))
                                .unwrap_or(Json::Null),
                        )
                    })
                    .collect(),
            );
            if moved.truthy() {
                log.line(&format!(
                    "{role}: its NIC's queue interrupts moved during the round: {}",
                    repr(&moved)
                ));
            }
        }
        for (role, found) in self.found.as_dict().into_iter().flatten() {
            let done = self.host(role).run(
                &hosts::as_found_script("/proc/interrupts", "/proc/irq"),
                RUN_LIMIT,
            )?;
            let now = hosts::as_found(&done.stdout)?;
            if &now != found {
                log.line(&format!(
                    "{role}: changed during the round: {} at the start, {} at the end",
                    repr(found),
                    repr(&now)
                ));
            }
        }
        Ok(())
    }
}
