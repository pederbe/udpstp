//! Path checks and individual runs. A host failure voids a run; an
//! interrupt leaves it immediately for the outer Round Teardown.

use std::fs::OpenOptions;
use std::io::Write;

use bench_rounds::counters;

use super::*;

fn probe(direction: &str, count: i64, engine: &str) -> ScaleRun {
    ScaleRun {
        index: 0,
        repeat: 0,
        case: "check".into(),
        direction: direction.into(),
        row: 100,
        count,
        engine: engine.into(),
        test_interval: 5,
        warm_up: 1,
        kind: "check".into(),
        rerun: 0,
        more: 0,
        reserve_s: 0.0,
        cpus: "all".into(),
        stagger: Num::Float(matrix::STAGGER_S),
    }
}

impl ScaleRound<'_> {
    pub(super) fn check_path(&mut self, log: &Log) -> Result<(), Stopped> {
        let base = "runs/path-check";
        let directory = format!("{base}/server");
        let cpus = &self.topology["server"].0;
        self.host("server").run_checked(
            &format!(
                "mkdir -p {directory} && {}",
                commands::serve(
                    &directory,
                    self.options.udpstp_port.into(),
                    cpus,
                    cpus[cpus.len() - 1],
                    &BTreeMap::new()
                )
            ),
            RUN_LIMIT,
        )?;
        let pidfile = format!("{directory}/serve.pid");
        self.background.push(("server".into(), pidfile.clone()));
        let result = (|| -> Result<(), Stopped> {
            self.wait_listening("server", &directory)?;
            for direction in ["downstream", "upstream"] {
                let probe = probe(direction, 1, "udpstp");
                for role in CLIENTS {
                    self.path_test(
                        role,
                        &probe,
                        &format!("{base}/{role}-{direction}"),
                        &self.server_address(self.options.udpstp_port.into()),
                        "",
                        log,
                    )?;
                }
            }
            Ok(())
        })();
        self.stop("server", &pidfile, 15, 0)?;
        result?;
        if self.engines().contains("obudpst") {
            let ports = self.obudpst_ports(None)?;
            let directory = format!("{base}/obudpst");
            let cpus = self.obudpst_cpus()?.unwrap_or_default();
            let collector = self.topology["server"].0.last().copied();
            self.host("server").run_checked(
                &format!(
                    "mkdir -p {directory} && {}",
                    commands::obudpst_server(&directory, &ports, &cpus, collector)
                ),
                RUN_LIMIT,
            )?;
            let pidfile = format!("{directory}/server.pid");
            self.background.push(("server".into(), pidfile.clone()));
            let result = (|| {
                self.sleep(1.0)?;
                self.path_test(
                    "client1",
                    &probe("downstream", ports.len() as i64, "obudpst"),
                    &format!("{base}/client1-obudpst"),
                    &ports
                        .iter()
                        .map(|port| self.server_address(*port))
                        .collect::<Vec<_>>()
                        .join(","),
                    "--auth-mode 1",
                    log,
                )
            })();
            self.stop("server", &pidfile, 15, 0)?;
            result?;
        }
        log.line("path check passed");
        Ok(())
    }

    fn path_test(
        &self,
        role: &str,
        probe: &ScaleRun,
        directory: &str,
        servers: &str,
        options: &str,
        log: &Log,
    ) -> Result<(), Stopped> {
        for attempt in 1..=2 {
            let script = commands::clients(
                probe,
                &commands::Clients {
                    directory,
                    servers,
                    first: 1,
                    count: probe.count,
                    cpus: &self.topology[role].0,
                    stamps: false,
                    options,
                    environment: &self.client_env,
                },
            );
            let done = self.host(role).run(&script, Limit::secs(90))?;
            let mut failed_runs = BTreeMap::new();
            for n in 1..=probe.count {
                let source = self
                    .host(role)
                    .run(&format!("cat {directory}/clients/{n}.json"), RUN_LIMIT)?
                    .stdout;
                let result = Json::parse(&source).unwrap_or_else(|_| EMPTY_DICT.clone());
                if get(get(&result, "udpstp"), "EndReason").as_str() != Some("Completed") {
                    failed_runs.insert(n, result);
                }
            }
            if done.success() && failed_runs.is_empty() {
                return Ok(());
            }
            if attempt == 1
                && !failed_runs.is_empty()
                && failed_runs
                    .values()
                    .map(judge::unanswered)
                    .collect::<Result<Vec<_>, _>>()?
                    .iter()
                    .all(Json::truthy)
            {
                log.line(&format!("the path check ({role}, {} to {servers}): {} got no answer in its Control Phase; once more", probe.direction, failed_runs.len()));
                continue;
            }
            let n = failed_runs.keys().next().copied().unwrap_or(1);
            let ended = get(
                get(failed_runs.get(&n).unwrap_or(&EMPTY_DICT), "udpstp"),
                "EndReason",
            );
            let ended = if ended.truthy() {
                judge::display(ended)
            } else {
                "no results".into()
            };
            let err = self
                .host(role)
                .run(&format!("tail -5 {directory}/clients/{n}.err"), RUN_LIMIT)?
                .stdout;
            return Err(failed(format!(
                "the path check failed ({role}, {} to {servers}): {ended} {}",
                probe.direction,
                py::strip(&err)
            )));
        }
        unreachable!("second attempt returns")
    }

    pub(super) fn run_one(&mut self, run: &ScaleRun, log: &Log) -> Result<Json, Stopped> {
        let local = self.options.out.join("scale").join(run.label());
        fs::create_dir_all(&local).map_err(failed)?;
        let remote = format!("runs/{}", run.label());
        let cpus = Json::Dict(
            self.topology
                .iter()
                .map(|(r, (a, p))| (r.clone(), dict([("all", ints(a)), ("per_core", ints(p))])))
                .collect(),
        );
        let mut meta = dict([
            ("label", string(&run.label())),
            ("kind", string(&run.kind)),
            ("case", string(&run.case)),
            ("direction", string(&run.direction)),
            ("row", int(run.row)),
            ("mbps", int(matrix::row_mbps(run.row))),
            ("packet_bytes", num_json(matrix::row_packet_bytes(run.row))),
            ("count", int(run.count)),
            ("engine", string(&run.engine)),
            ("repeat", int(run.repeat)),
            ("rerun", int(run.rerun)),
            ("more", int(run.more)),
            ("test_interval", int(run.test_interval)),
            ("warm_up", int(run.warm_up)),
            ("stagger", num_json(run.stagger)),
            (
                "confine",
                if run.kind == "confine" {
                    string(&run.cpus)
                } else {
                    Json::Null
                },
            ),
            ("cpus", cpus),
            ("counters", self.counter_names.clone()),
            (
                "server_nic_drops",
                string(hosts::nic_receive_drops(if self.rented {
                    "aws"
                } else {
                    "static"
                })),
            ),
        ]);
        let started = self.clock.now();
        let mut error = None;
        let result = (|| -> Result<(), Stopped> {
            if matches!(run.engine.as_str(), "udpstp" | "obudpst") && run.kind != "p1" {
                let cpus = self.server_cpus(run)?;
                set(&mut meta, "server_cpus", ints(&cpus))?;
                set(&mut meta, "collector_cpu", int(self.collector_cpu(&cpus)?))?;
                set(
                    &mut meta,
                    "client_env",
                    get(&self.matrix.plan, "client_env").or(&EMPTY_DICT).clone(),
                )?;
            }
            if run.engine == "udpstp" && run.kind != "p1" {
                let environment = self.server_environment(run)?;
                set(&mut meta, "server_env", environment.clone())?;
                let executor = get(&environment, matrix::EXECUTOR_ENV)
                    .or(get(&self.matrix.settings, "executor"));
                set(&mut meta, "executor", executor.clone())?;
            }
            if run.engine == "obudpst" {
                set(
                    &mut meta,
                    "obudpst_cpus",
                    self.obudpst_where(run)?
                        .map(|v| ints(&v))
                        .unwrap_or(Json::Null),
                )?;
                set(
                    &mut meta,
                    "obudpst_ports",
                    ints(&self.obudpst_ports(Some(run))?),
                )?;
            }
            cleanup::strays(&self.hosts, log, RUN_LIMIT, "an earlier run", false)?;
            self.counters(&local, "before")?;
            host::each(self.hosts.iter().map(|(role, h)| {
                let script = format!("rm -rf {remote}/{role} && mkdir -p {remote}/{role}");
                move || h.run_checked(&script, RUN_LIMIT)
            }))?;
            match run.engine.as_str() {
                "udpstp" => self.run_udpstp(run, &remote)?,
                "obudpst" => self.run_obudpst(run, &remote)?,
                _ => {
                    set(
                        &mut meta,
                        "start_wall",
                        num_json(self.run_prototype(run, &remote)?),
                    )?;
                }
            }
            Ok(())
        })();
        match result {
            Ok(()) => {}
            Err(Stopped::Interrupted) => return Err(Stopped::Interrupted),
            Err(e) => {
                log.line(&format!("{}: {e}", run.label()));
                error = Some(e.to_string());
            }
        }
        let stopped = self.clock.now();
        for (role, pidfile) in self.background.clone() {
            match self.stop(&role, &pidfile, 15, 0) {
                Ok(()) => {}
                Err(Stopped::Interrupted) => return Err(Stopped::Interrupted),
                Err(e) => {
                    error.get_or_insert_with(|| format!("stopping {pidfile}: {e}"));
                }
            }
        }
        self.sleep((1.5 - (self.clock.now() - stopped)).max(0.0))?;
        match self.counters(&local, "after") {
            Ok(()) => {}
            Err(Stopped::Interrupted) => return Err(Stopped::Interrupted),
            Err(e) => {
                error.get_or_insert_with(|| format!("reading the counters after the run: {e}"));
            }
        }
        let collect = (|| -> Result<(), Stopped> {
            let calls = self
                .hosts
                .iter()
                .map(|(role, h)| {
                    let from = super::remote(&format!("{remote}/{role}"))?;
                    let to = local.join(role);
                    Ok(move || h.fetch_dir(&from, &to))
                })
                .collect::<Result<Vec<_>, Stopped>>()?;
            host::each(calls)?;
            host::each(self.hosts.iter().map(|(role, h)| {
                let script = format!("rm -rf {remote}/{role}");
                move || h.run_checked(&script, RUN_LIMIT)
            }))?;
            Ok(())
        })();
        match collect {
            Ok(()) => {}
            Err(Stopped::Interrupted) => return Err(Stopped::Interrupted),
            Err(e) => {
                error.get_or_insert_with(|| format!("collecting the results: {e}"));
            }
        }
        set(
            &mut meta,
            "seconds",
            num_json(
                Num::Float(self.clock.now() - started)
                    .round(Some(1))
                    .ok_or_else(|| failed("a run's elapsed time is not finite"))?,
            ),
        )?;
        set(
            &mut meta,
            "error",
            error.map(Json::Str).unwrap_or(Json::Null),
        )?;
        write_json(&local.join("meta.json"), &meta, true)?;
        let analysed = judge::RunFiles::load(&local).and_then(|files| judge::analyse(&files));
        let record = match analysed {
            Ok(record) => record,
            Err(e) => {
                judge::tooling_record(&meta, &format!("analysing the run: {}", exception(&e)))?
            }
        };
        self.results.push(record.clone());
        let mut summary = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.options.out.join("scale/summary.jsonl"))
            .map_err(failed)?;
        writeln!(summary, "{}", py::dumps(&record)).map_err(failed)?;
        Ok(record)
    }

    fn counters(&self, local: &Path, when: &str) -> Result<(), Stopped> {
        host::each(self.hosts.iter().map(|(role, h)| {
            move || {
                let script = if role == "server" {
                    format!("{}\n{}\n", counters::SCRIPT, hosts::INTERRUPTS_SCRIPT)
                } else {
                    counters::SCRIPT.to_owned()
                };
                let done = h.run(&script, Limit::secs(30))?;
                let (counters, tail) = done
                    .stdout
                    .split_once(hosts::INTERRUPTS_MARK)
                    .unwrap_or((&done.stdout, ""));
                fs::write(local.join(format!("counters.{role}.{when}.txt")), counters)
                    .map_err(|e| HostError::new(e.to_string()))?;
                if role == "server" {
                    let interrupts = if done.stdout.contains(hosts::INTERRUPTS_MARK) {
                        format!("{}{tail}", hosts::INTERRUPTS_MARK)
                    } else {
                        String::new()
                    };
                    fs::write(
                        local.join(format!("interrupts.{role}.{when}.txt")),
                        interrupts,
                    )
                    .map_err(|e| HostError::new(e.to_string()))?;
                }
                Ok(())
            }
        }))?;
        Ok(())
    }

    fn server_environment(&self, run: &ScaleRun) -> Result<Json, Error> {
        let case = self.cases.get(&run.case).unwrap_or(&EMPTY_DICT);
        matrix::environment(
            get(case, "server_env").or(&EMPTY_DICT),
            &format!("{}: server_env", run.case),
        )
    }

    fn run_udpstp(&mut self, run: &ScaleRun, base: &str) -> Result<(), Stopped> {
        let directory = format!("{base}/server");
        if run.kind == "p1" {
            let target = format!("{base}/client1");
            let cpus = &self.topology["client1"].0;
            self.host("client1").run_checked(
                &commands::serve(
                    &target,
                    self.options.udpstp_port.into(),
                    cpus,
                    cpus[cpus.len() - 1],
                    &BTreeMap::new(),
                ),
                RUN_LIMIT,
            )?;
            let pidfile = format!("{target}/serve.pid");
            self.background.push(("client1".into(), pidfile.clone()));
            self.wait_listening("client1", &target)?;
            let script = commands::clients(
                run,
                &commands::Clients {
                    directory: &directory,
                    servers: &format!(
                        "{}:{}",
                        self.host("client1").address(),
                        self.options.udpstp_port
                    ),
                    first: 1,
                    count: run.count,
                    cpus: &self.topology["server"].0,
                    stamps: true,
                    options: "",
                    environment: &BTreeMap::new(),
                },
            );
            self.host("server")
                .run_checked(&script, limit(Num::Int((run.test_interval + 90).into()))?)?;
            return self.stop("client1", &pidfile, 15, 0);
        }
        let cpus = self.server_cpus(run)?;
        self.host("server").run_checked(
            &commands::serve(
                &directory,
                self.options.udpstp_port.into(),
                &cpus,
                self.collector_cpu(&cpus)?,
                &environment(&self.server_environment(run)?),
            ),
            RUN_LIMIT,
        )?;
        let pidfile = format!("{directory}/serve.pid");
        self.background.push(("server".into(), pidfile.clone()));
        self.wait_listening("server", &directory)?;
        self.run_clients(
            run,
            base,
            &self.server_address(self.options.udpstp_port.into()),
            "",
        )?;
        self.stop("server", &pidfile, 15, 0)
    }

    fn run_clients(
        &self,
        run: &ScaleRun,
        base: &str,
        servers: &str,
        options: &str,
    ) -> Result<(), Stopped> {
        let stagger = match run.stagger {
            Num::Int(n) => Num::Int(
                n.checked_mul(run.count.into())
                    .ok_or_else(|| failed("Client stagger is out of range"))?,
            ),
            Num::Float(n) => Num::Float(run.count as f64 * n),
        };
        let timeout = limit(Num::Int((run.test_interval + 90).into()).py_add(stagger))?;
        let calls = hosts::split(run.count, 2)?
            .into_iter()
            .zip(CLIENTS)
            .filter(|((_, n), _)| *n != 0)
            .map(|((first, count), role)| {
                let h = self.host(role);
                let script = commands::clients(
                    run,
                    &commands::Clients {
                        directory: &format!("{base}/{role}"),
                        servers,
                        first,
                        count,
                        cpus: &self.topology[role].0,
                        stamps: false,
                        options,
                        environment: &self.client_env,
                    },
                );
                move || h.run_checked(&script, timeout)
            });
        host::each(calls)?;
        Ok(())
    }

    fn run_obudpst(&mut self, run: &ScaleRun, base: &str) -> Result<(), Stopped> {
        let directory = format!("{base}/server");
        let ports = self.obudpst_ports(Some(run))?;
        let collector = self.collector_cpu(&self.server_cpus(run)?)?;
        self.host("server").run_checked(
            &commands::obudpst_server(
                &directory,
                &ports,
                &self.obudpst_where(run)?.unwrap_or_default(),
                Some(collector),
            ),
            RUN_LIMIT,
        )?;
        let pidfile = format!("{directory}/server.pid");
        self.background.push(("server".into(), pidfile.clone()));
        self.sleep(1.0)?;
        let alive = self
            .host("server")
            .run(&commands::alive(&pidfile), RUN_LIMIT)?;
        if !alive.stdout.contains("alive") {
            let tail = self
                .host("server")
                .run(&format!("tail -n 3 {directory}/server-*.log"), RUN_LIMIT)?
                .stdout;
            return Err(failed(format!(
                "server: an OB-UDPST instance exited at once ({}): {}",
                py::strip(&alive.stdout),
                py::strip(&tail)
            )));
        }
        let servers = ports
            .iter()
            .map(|port| self.server_address(*port))
            .collect::<Vec<_>>()
            .join(",");
        self.run_clients(run, base, &servers, "--auth-mode 1")?;
        self.stop("server", &pidfile, 15, 0)
    }

    fn run_prototype(&mut self, run: &ScaleRun, base: &str) -> Result<Num, Stopped> {
        let now = self
            .host("server")
            .run("date +%s%N", Limit::secs(30))?
            .stdout;
        let now = py::strip(&now);
        let start = (!now.is_empty() && now.bytes().all(|b| b.is_ascii_digit()))
            .then(|| now.parse::<i128>().ok())
            .flatten()
            .and_then(|n| n.checked_add(4_000_000_000))
            .ok_or_else(|| failed("server: couldn't read its clock"))?;
        let start = Num::Int(start);
        let q_addresses = CLIENTS.map(|role| self.host(role).address().to_owned());
        let directory = format!("{base}/server");
        let (cpus, per_core) = &self.topology["server"];
        let p = commands::prototype(
            run,
            &commands::Prototype {
                side: "p",
                directory: &directory,
                start_wall: start,
                p_address: self.host("server").address(),
                q_addresses: &q_addresses,
                q_index: 0,
                cpus,
                per_core,
                stamps: run.kind == "p1",
            },
        )?;
        self.host("server")
            .run_checked(&commands::prototype_p(&p, &directory), RUN_LIMIT)?;
        let pidfile = format!("{directory}/p.pid");
        self.background.push(("server".into(), pidfile.clone()));
        let mut calls: Vec<Box<dyn FnOnce() -> Result<(), Stop> + Send + '_>> = Vec::new();
        let server = self.host("server");
        let clock = self.clock;
        let check_pid = &pidfile;
        let check_log = format!("{directory}/p.log");
        calls.push(Box::new(move || {
            clock.sleep(Duration::from_millis(1500))?;
            let done = server.run(&commands::alive(check_pid), RUN_LIMIT)?;
            if !done.stdout.contains("alive") {
                let tail = server
                    .run(&format!("tail -n 3 {check_log}"), RUN_LIMIT)?
                    .stdout;
                return Err(HostError::new(format!(
                    "server: the prototype's side P exited at once ({}): {}",
                    py::strip(&done.stdout),
                    py::strip(&tail)
                ))
                .into());
            }
            Ok(())
        }));
        let timeout = limit(Num::Int((4 + run.warm_up + run.test_interval + 60).into()))?;
        for (index, role) in CLIENTS.into_iter().enumerate() {
            let h = self.host(role);
            let (cpus, per_core) = &self.topology[role];
            let directory = format!("{base}/{role}");
            let q = commands::prototype(
                run,
                &commands::Prototype {
                    side: "q",
                    directory: &directory,
                    start_wall: start,
                    p_address: server.address(),
                    q_addresses: &q_addresses,
                    q_index: index as i64,
                    cpus,
                    per_core,
                    stamps: false,
                },
            )?;
            let script = commands::sampled(&format!("{q} > {directory}/q.log 2>&1"), &directory);
            calls.push(Box::new(move || {
                h.run_checked(&script, timeout).map(|_| ())
            }));
        }
        host::each(calls)?;
        self.stop("server", &pidfile, 10, 30)?;
        Ok(start)
    }
}

/// PyError's display includes its class; Python logs repr(e) here.
pub(super) fn exception(error: &Error) -> String {
    let message = error.to_string();
    if let Some(key) = message.strip_prefix("KeyError: ") {
        return format!("KeyError({key})");
    }
    for kind in [
        "TypeError",
        "ValueError",
        "IndexError",
        "OverflowError",
        "ZeroDivisionError",
    ] {
        if let Some(message) = message.strip_prefix(&format!("{kind}: ")) {
            return format!("{kind}({})", repr(&string(message)));
        }
    }
    message
}
