//! The final sweep's Linux process boundary (the bench design §3.9):
//! programs in the Round's directory go; a host's own Server stays.
#![cfg(target_os = "linux")]

use std::os::unix::fs::PermissionsExt;

use bench_rounds::{
    host::{Host, Limit, RemotePath, Shell},
    interrupt::Flag,
    process::Runner,
};
use std::{
    path::Path,
    process::{Child, Command, Stdio},
    time::Duration,
};
use udpstp_bench::scale::hosts::STRAYS_SCRIPT;

struct Process(Child);
impl Process {
    fn start(directory: &Path) -> Process {
        Self::named(directory, "udpstp")
    }
    fn named(directory: &Path, name: &str) -> Process {
        let program = directory.join(name);
        // A named shell script is portable across coreutils variants;
        // multicall sleep binaries select their command from argv[0].
        std::fs::write(&program, "#!/bin/sh\nwhile :; do sleep 0.2; done\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        Process(
            Command::new(&program)
                .arg("60")
                .current_dir(directory)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn the_sweep_kills_only_programs_in_its_work_directory() {
    let root = std::env::temp_dir().join(format!("udpstp-sweep-{}", std::process::id()));
    let work = root.join("work");
    let nested = work.join("run");
    let elsewhere = root.join("elsewhere");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();
    let mut inside = Process::start(&nested);
    let mut outside = Process::start(&elsewhere);
    let mut sampler = Process::named(&nested, "udpstp-bench");
    let mut unrelated_sampler = Process::named(&elsewhere, "udpstp-bench");
    std::thread::sleep(Duration::from_millis(200));
    let flag = Flag::new();
    let host = Host::local(
        "server",
        "127.0.0.1",
        RemotePath::new(work.to_str().unwrap()).unwrap(),
        Runner::new(flag.clone()),
    );
    // Already interrupted: the Round Teardown's proof makes the command
    // run to its limit regardless.
    flag.raise("SIGINT");
    let _begun = flag.stop_acting();
    let done = host.run_checked(STRAYS_SCRIPT, Limit::secs(30)).unwrap();
    // : a late sampler is swept inside this Round, preserving unrelated tools.
    let mut killed = done
        .stdout
        .lines()
        .map(|line| line.parse::<u32>().unwrap())
        .collect::<Vec<_>>();
    killed.sort();
    let mut owned = vec![inside.0.id(), sampler.0.id()];
    owned.sort();
    assert_eq!(killed, owned);
    assert!(!sampler.0.wait().unwrap().success());
    assert!(unrelated_sampler.0.try_wait().unwrap().is_none());
    assert!(!inside.0.wait().unwrap().success());
    assert!(outside.0.try_wait().unwrap().is_none());
    drop(inside);
    drop(outside);
    drop(sampler);
    drop(unrelated_sampler);
    std::fs::remove_dir_all(root).unwrap();
}
