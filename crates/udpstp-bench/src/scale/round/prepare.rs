//! Preparation and the evidence describing the hosts.

use bench_rounds::{counters, dead_man};

use super::*;
use crate::obudpst;
use crate::scale::rules::{HOST_DROPS, PLATFORM_COUNTERS};

impl ScaleRound<'_> {
    pub(super) fn prepare(
        &mut self,
        provider: &mut dyn Provider,
        keys: &Path,
        log: &Log,
    ) -> Result<(), Stopped> {
        let missing = ROLES
            .into_iter()
            .filter(|r| !self.hosts.contains_key(*r))
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(failed(format!(
                "the Server-scale round needs hosts {}; missing {}",
                ROLES.join(", "),
                missing.join(", ")
            )));
        }
        if get(&self.matrix.plan, "keep_server_binaries").truthy() {
            provider.keep_files("server", &["udpstp", "udpst"])?;
            log.line(&format!("the Server host's udpstp and udpst stay in {} at the Round Teardown (keep_server_binaries), for perf report", self.host("server").work()));
        }
        fs::create_dir_all(self.options.out.join("scale")).map_err(failed)?;
        write_json(
            &self.options.out.join("scale/matrix.json"),
            &self.matrix.plan,
            true,
        )?;
        self.record_binary("udpstp", &self.options.udpstp)?;
        if self.prototypes() {
            let path = self
                .options
                .prototype
                .as_ref()
                .ok_or_else(|| failed("the prototype binary is missing"))?;
            self.record_binary("dpproto", path)?;
        }
        let ob = self.engines().contains("obudpst");
        if ob && let Some(Obudpst::Binary(path)) = &self.options.obudpst {
            self.record_binary("udpst", path)?;
        }
        let clock = self.clock;
        host::each(self.hosts.values().map(|h| {
            move || {
                for attempt in 0..30 {
                    match h.run_checked("true", Limit::secs(30)) {
                        Ok(_) => return Ok(()),
                        Err(Stop::Host(_)) if attempt < 29 => {
                            clock.sleep(Duration::from_secs(10))?
                        }
                        Err(error) => return Err(error),
                    }
                }
                unreachable!("last attempt returned")
            }
        }))?;
        let mut quieted = vec![String::new(); self.hosts.len()];
        if self.rented {
            let armed = host::each(
                self.hosts
                    .values()
                    .map(|h| move || dead_man::check(h.as_ref())),
            )?;
            for (role, armed) in self.hosts.keys().zip(armed) {
                log.line(&format!("{role}: {armed}"));
            }
            quieted = host::each(self.hosts.values().map(|h| {
                move || {
                    let done = h.run(host::QUIET_SCRIPT, host::QUIET_LIMIT)?;
                    Ok(done.stdout)
                }
            }))?;
        }
        self.check_counters()?;
        let current = std::env::current_exe().map_err(|e| failed(e.to_string()))?;
        let tool = self.options.measurement_tool.as_deref().unwrap_or(&current);
        host::stage_executable(tool, &keys.join("udpstp-bench"))
            .map_err(|e| failed(e.to_string()))?;
        for h in self.hosts.values() {
            h.run_checked("mkdir -p scripts runs", RUN_LIMIT)?;
            h.put(&self.options.udpstp, &remote("udpstp")?, 0o755)?;
            if self.prototypes() {
                h.put(
                    self.options.prototype.as_ref().expect("checked above"),
                    &remote("dpproto")?,
                    0o755,
                )?;
            }
            h.put(
                &self.options.scripts.join("server-scale.sh"),
                &remote("scripts/server-scale.sh")?,
                0o755,
            )?;
            h.put(
                &keys.join("udpstp-bench"),
                &remote("scripts/udpstp-bench")?,
                0o755,
            )?;
            h.put(&keys.join("keys.toml"), &remote("keys.toml")?, 0o600)?;
            h.put(&keys.join("udpst.keys"), &remote("udpst.keys")?, 0o600)?;
        }
        if ob {
            self.place_obudpst(keys)?;
        }
        let roles = self.hosts.keys().cloned().collect::<Vec<_>>();
        for (role, quiet) in roles.iter().zip(quieted) {
            let source = self.host(role).run("lscpu -p=CPU,CORE", RUN_LIMIT)?.stdout;
            let topology = hosts::topology(&source).map_err(|e| failed(format!("{role}: {e}")))?;
            self.topology.insert(role.clone(), topology);
            if self.options.raise_buffers {
                self.host(role).run(
                    "sudo -n sysctl -qw net.core.rmem_max=16777216 net.core.wmem_max=16777216",
                    RUN_LIMIT,
                )?;
            }
            if self.options.hold_irqs {
                let done = self.host(role).run("systemctl is-active --quiet irqbalance && sudo -n systemctl stop irqbalance && echo stopped", RUN_LIMIT)?;
                if done.stdout.contains("stopped") {
                    self.irqbalance_stopped.push(role.clone());
                }
                self.set_irqs(role, log)?;
            }
            if self.options.as_found {
                let done = self.host(role).run(
                    &hosts::as_found_script("/proc/interrupts", "/proc/irq"),
                    RUN_LIMIT,
                )?;
                set(&mut self.found, role, hosts::as_found(&done.stdout)?)?;
            }
            let done = self.host(role).run(
                &format!("{}{}", host::ENVIRONMENT_SCRIPT, hosts::ENV_EXTRA),
                RUN_LIMIT,
            )?;
            fs::write(
                self.options.out.join(format!("env-{role}.txt")),
                done.stdout + &done.stderr + &quiet,
            )
            .map_err(failed)?;
        }
        self.record_host_states("start")?;
        let topology = Json::Dict(
            self.topology
                .iter()
                .map(|(role, (all, per_core))| {
                    (role.clone(), Json::List(vec![ints(all), ints(per_core)]))
                })
                .collect(),
        );
        write_json(&self.options.out.join("topology.json"), &topology, false)?;
        if !self.irqs.is_empty() {
            let irqs = Json::Dict(
                self.irqs
                    .iter()
                    .map(|(role, layout)| {
                        (
                            role.clone(),
                            Json::Dict(
                                layout
                                    .iter()
                                    .map(|(irq, cpu)| (irq.to_string(), int(*cpu)))
                                    .collect(),
                            ),
                        )
                    })
                    .collect(),
            );
            write_json(&self.options.out.join("irqs.json"), &irqs, true)?;
        }
        if self.found.truthy() {
            let where_ = if ob {
                self.obudpst_cpus()?.map(|v| ints(&v)).unwrap_or(Json::Null)
            } else {
                Json::Null
            };
            write_json(
                &self.options.out.join("scale/as-found.json"),
                &dict([("hosts", self.found.clone()), ("obudpst_cpus", where_)]),
                true,
            )?;
            log.line(&format!(
                "the hosts as found, not changed (--as-found): {}",
                repr(&self.found)
            ));
        }
        self.check_confinement(log)?;
        self.stamp()?;
        let t = self
            .topology
            .iter()
            .map(|(r, (a, p))| {
                format!(
                    "{}: ({}, {})",
                    repr(&string(r)),
                    repr(&ints(a)),
                    repr(&ints(p))
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        // Python keeps IRQ keys as strings, whereas the placement calculation uses integers.
        let irqs = Json::Dict(
            self.irqs
                .iter()
                .map(|(role, layout)| {
                    (
                        role.clone(),
                        Json::Dict(
                            layout
                                .iter()
                                .map(|(irq, cpu)| (irq.to_string(), int(*cpu)))
                                .collect(),
                        ),
                    )
                })
                .collect(),
        );
        log.line(&format!(
            "CPUs (all, one per core): {{{t}}}; NIC queue interrupts: {}",
            repr(&irqs)
        ));
        if !self.client_env.is_empty() {
            log.line(&format!(
                "udpstp's Clients run with {}",
                env_text(&self.client_env)
            ));
        }
        if ob {
            let where_ = self.obudpst_cpus()?;
            let where_ = where_
                .filter(|c| !c.is_empty())
                .map(|c| format!("on logical CPUs {} (taskset)", hosts::cpu_text(&c)))
                .unwrap_or_else(|| "where the scheduler puts them".into());
            log.line(&format!(
                "OB-UDPST's instances: {where_}{}",
                if get(&self.matrix.plan, "confine").truthy() {
                    ", and confined ones on their case's CPUs"
                } else {
                    ""
                }
            ));
        }
        Ok(())
    }

    fn check_counters(&mut self) -> Result<(), Stopped> {
        let mut required = HOST_DROPS.to_vec();
        if self.rented {
            required.extend(PLATFORM_COUNTERS);
        }
        let found = host::each(
            self.hosts
                .values()
                .map(|h| move || h.run(counters::SCRIPT, Limit::secs(30))),
        )?;
        for (role, done) in self.hosts.keys().zip(found) {
            let gone = counters::missing(&done.stdout, &required).map_err(failed)?;
            if !gone.is_empty() {
                return Err(failed(format!(
                    "{role}: its counters lack {}, which the judging reads (round.py's COUNTERS_SCRIPT)",
                    gone.join(", ")
                )));
            }
            set(
                &mut self.counter_names,
                role,
                Json::List(required.iter().map(|s| string(s)).collect()),
            )?;
        }
        Ok(())
    }

    pub(super) fn record_host_states(&mut self, when: &str) -> Result<(), Stopped> {
        let script = format!(
            "{}{}\n",
            hosts::host_state_script(),
            counters::throttles_script(counters::CPUS)
        );
        let found = host::each(self.hosts.values().map(|h| {
            let script = &script;
            move || h.run(script, Limit::secs(60))
        }))?;
        let state = self
            .hosts
            .keys()
            .zip(found)
            .map(|(role, done)| Ok((role.clone(), hosts::host_state(&done.stdout)?)))
            .collect::<Result<IndexMap<_, _>, Error>>()?;
        set(&mut self.host_states, when, Json::Dict(state))?;
        write_json(
            &self.options.out.join("scale/host-state.json"),
            &self.host_states,
            true,
        )
    }

    fn place_obudpst(&self, keys: &Path) -> Result<(), Stopped> {
        match &self.options.obudpst {
            Some(Obudpst::Binary(path)) => {
                for h in self.hosts.values() {
                    h.put(path, &remote("udpst")?, 0o755)?;
                }
            }
            Some(Obudpst::Build) => {
                let server = self.host("server");
                obudpst::build(server, &self.options.obudpst_script)?;
                let local = keys.join("udpst");
                server.fetch_file(&remote("udpst")?, &local)?;
                self.record_binary("udpst", &local)?;
                for role in CLIENTS {
                    let h = self.host(role);
                    if (h.destination(), h.work()) != (server.destination(), server.work()) {
                        h.put(&local, &remote("udpst")?, 0o755)?;
                    }
                }
            }
            None => return Err(failed("the OB-UDPST binary is missing")),
        }
        Ok(())
    }

    fn record_binary(&self, name: &str, path: &Path) -> Result<(), Stopped> {
        let record = self.options.out.join("binaries.json");
        let mut binaries = fs::read_to_string(&record)
            .ok()
            .and_then(|s| Json::parse(&s).ok())
            .filter(Json::truthy)
            .unwrap_or_else(|| EMPTY_DICT.clone());
        set(&mut binaries, name, string(&hosts::sha256(path)?))?;
        write_json(&record, &binaries, true)
    }

    fn stamp(&self) -> Result<(), Stopped> {
        // A running executable's contents are fixed; cache its hash across scripted Rounds.
        static BINARY: std::sync::OnceLock<Result<String, String>> = std::sync::OnceLock::new();
        let hash = BINARY
            .get_or_init(|| {
                std::env::current_exe()
                    .map_err(|e| e.to_string())
                    .and_then(|path| hosts::sha256(&path).map_err(|e| e.to_string()))
            })
            .as_ref()
            .map_err(failed)?;
        let mut found = dict([
            (
                "udpstp-bench built from",
                string(env!("UDPSTP_BENCH_COMMIT")),
            ),
            ("udpstp-bench SHA-256", string(hash)),
            (
                "udpstp binary built from",
                string(&hosts::binary_commit(&self.options.udpstp)?),
            ),
        ]);
        set(
            &mut found,
            "server-scale.sh SHA-256",
            string(&hosts::sha256(
                &self.options.scripts.join("server-scale.sh"),
            )?),
        )?;
        let sampler_hash = match &self.options.measurement_tool {
            Some(tool) => hosts::sha256(tool)?,
            None => hash.clone(),
        };
        set(
            &mut found,
            "measurement tool SHA-256",
            string(&sampler_hash),
        )?;
        let name = self
            .options
            .matrix
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| failed("the matrix's name is not UTF-8"))?;
        set(
            &mut found,
            &format!("{name} SHA-256"),
            string(&hosts::sha256(&self.options.matrix)?),
        )?;
        if self.prototypes() {
            let done = self.host("server").run("./dpproto version", RUN_LIMIT)?;
            set(
                &mut found,
                "dpproto commit",
                string(py::strip(&done.stdout)),
            )?;
        }
        write_json(&self.options.out.join("scale/stamps.json"), &found, true)
    }
}
