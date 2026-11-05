use std::sync::{Arc, Mutex};

use bench_rounds::host::Completed;
use bench_rounds::interrupt::Flag;
use bench_rounds::round::WallClock;

use super::*;

struct Gated {
    role: String,
    work: RemotePath,
    flag: Flag,
    calls: Arc<Mutex<Vec<(String, String)>>>,
}
impl Shell for Gated {
    fn role(&self) -> &str {
        &self.role
    }
    fn address(&self) -> &str {
        "127.0.0.1"
    }
    fn public_address(&self) -> Option<&str> {
        None
    }
    fn destination(&self) -> &str {
        "local"
    }
    fn work(&self) -> &RemotePath {
        &self.work
    }
    fn run(&self, script: &str, _: Limit) -> Result<Completed, Stop> {
        // Match Runner: no host call can start while an active flag stops it.
        if self.flag.stops() {
            return Err(Stop::Interrupted);
        }
        self.calls
            .lock()
            .unwrap()
            .push((self.role.clone(), script.to_owned()));
        if self.role == "server" {
            panic!("one host panics in every cleanup phase");
        }
        if self.role == "client1" && script.starts_with("ps=$(cat ") {
            return Err(HostError::new("a stop lost its connection").into());
        }
        Ok(Completed {
            returncode: 0,
            stdout: if script == hosts::STRAYS_SCRIPT {
                "4242\n"
            } else {
                ""
            }
            .into(),
            stderr: String::new(),
        })
    }
    fn put(&self, _: &Path, _: &RemotePath, _: u32) -> Result<(), Stop> {
        unreachable!()
    }
    fn output(&self, _: &str, _: Limit) -> Result<Vec<u8>, Stop> {
        unreachable!()
    }
}

#[test]
fn interrupted_and_panicking_stops_cannot_skip_sweeps_or_irqbalance_restores() {
    let flag = Flag::new();
    let clock = WallClock { flag: flag.clone() };
    let calls = Arc::new(Mutex::new(Vec::new()));
    let options = Options {
        measurement_tool: None,
        udpstp: "unused".into(),
        prototype: None,
        obudpst: None,
        raise_buffers: false,
        hold_irqs: true,
        as_found: false,
        udpstp_port: 24601,
        obudpst_port: 25000,
        out: "unused".into(),
        ssh_dir: None,
        matrix: "unused".into(),
        scripts: "unused".into(),
        obudpst_script: "unused".into(),
    };
    let matrix = matrix::parse(include_str!(
        "../../../../../scripts/bench/tests/scale-smoke.toml"
    ))
    .unwrap();
    let mut body = ScaleRound::new(options, matrix, &clock, Runner::new(flag.clone())).unwrap();
    for role in ROLES {
        body.hosts.insert(
            role.into(),
            Box::new(Gated {
                role: role.into(),
                work: RemotePath::new("/work").unwrap(),
                flag: flag.clone(),
                calls: calls.clone(),
            }),
        );
        body.background
            .push((role.into(), format!("runs/one/{role}/serve.pid")));
        body.irqbalance_stopped.push(role.into());
    }
    flag.raise("SIGINT");
    assert!(calls.lock().unwrap().is_empty());
    let log = Log::kept();
    body.stop_servers(&flag.stop_acting(), &log);
    let calls = calls.lock().unwrap();
    for role in ROLES {
        let made: Vec<_> = calls
            .iter()
            .filter(|(r, _)| r == role)
            .map(|(_, c)| c.as_str())
            .collect();
        assert_eq!(
            made,
            [
                commands::stop(&format!("runs/one/{role}/serve.pid"), 5, 0),
                hosts::STRAYS_SCRIPT.into(),
                "sudo -n systemctl start irqbalance".into(),
            ]
        );
    }
    let log = log.lines();
    assert!(
        log.iter()
            .any(|line| line.contains("server: the stray sweep failed"))
    );
    for role in CLIENTS {
        assert!(log.contains(&format!(
            "{role}: killed what the round left running: ['4242']"
        )));
    }
}

struct TopologyReadback(RemotePath);

impl Shell for TopologyReadback {
    fn role(&self) -> &str {
        "server"
    }
    fn address(&self) -> &str {
        "127.0.0.1"
    }
    fn public_address(&self) -> Option<&str> {
        None
    }
    fn destination(&self) -> &str {
        "local"
    }
    fn work(&self) -> &RemotePath {
        &self.0
    }
    fn run(&self, script: &str, _: Limit) -> Result<Completed, Stop> {
        let stdout = if script == hosts::queue_irqs_script("/proc/interrupts") {
            "1 0\n"
        } else if script == hosts::as_found_script("/proc/interrupts", "/proc/irq") {
            "irq 1 0-9223372036854775807 nic\n"
        } else {
            "1 0-9223372036854775807\n"
        };
        Ok(Completed {
            returncode: 0,
            stdout: stdout.into(),
            stderr: String::new(),
        })
    }
    fn put(&self, _: &Path, _: &RemotePath, _: u32) -> Result<(), Stop> {
        unreachable!()
    }
    fn output(&self, _: &str, _: Limit) -> Result<Vec<u8>, Stop> {
        unreachable!()
    }
}

// Benchmark topology limits: the callers refuse bounds before tolerating a
// static-host placement mismatch, and log failed final checks during collection.
#[test]
fn oversized_readback_cannot_enter_static_placement_fallback() {
    let flag = Flag::new();
    let clock = WallClock { flag: flag.clone() };
    let out = bench_rounds::secrets::SecretDir::create().unwrap();
    fs::create_dir(out.path().join("scale")).unwrap();
    let options = Options {
        measurement_tool: None,
        udpstp: "unused".into(),
        prototype: None,
        obudpst: None,
        raise_buffers: false,
        hold_irqs: false,
        as_found: true,
        udpstp_port: 24601,
        obudpst_port: 25000,
        out: out.path().into(),
        ssh_dir: None,
        matrix: "unused".into(),
        scripts: "unused".into(),
        obudpst_script: "unused".into(),
    };
    let matrix = matrix::parse(include_str!(
        "../../../../../scripts/bench/tests/scale-smoke.toml"
    ))
    .unwrap();
    let mut body = ScaleRound::new(options, matrix, &clock, Runner::new(flag)).unwrap();
    body.hosts.insert(
        "server".into(),
        Box::new(TopologyReadback(RemotePath::new("/work").unwrap())),
    );
    body.topology.insert("server".into(), (vec![0], vec![0]));
    assert!(!body.rented);
    let log = Log::kept();
    let error = body.set_irqs("server", &log).unwrap_err().to_string();
    assert!(error.contains("CPU topology exceeds 65536 expansion steps"));
    assert!(body.irqs.is_empty());
    assert!(log.lines().is_empty());
    let completed = dict([("label", string("completed"))]);
    body.results.push(completed.clone());

    for as_found in [false, true] {
        body.irqs.clear();
        body.found = Json::Dict(IndexMap::new());
        if as_found {
            set(
                &mut body.found,
                "server",
                hosts::as_found("irq 1 0 nic\n").unwrap(),
            )
            .unwrap();
        } else {
            body.irqs.insert("server".into(), IndexMap::from([(1, 0)]));
        }
        let error = body.check_irqs(&log).unwrap_err().to_string();
        assert!(error.contains("CPU topology exceeds 65536 expansion steps"));
        let log = Log::kept();
        body.collect(&log).unwrap();
        assert_eq!(body.results(), std::slice::from_ref(&completed));
        assert!(log.lines().iter().any(|line| line.contains(
            "checking the NIC's interrupts failed: CPU topology exceeds 65536 expansion steps"
        )));
    }
}
