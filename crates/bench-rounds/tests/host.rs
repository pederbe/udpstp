//! Hosts with real processes, on Linux: local hosts through bash, ssh hosts through a
//! stand-in `ssh` and `scp` that record what they are given and run the
//! script as the remote side would, `each`, and the static provider with
//! the Round Teardown on local hosts.
#![cfg(target_os = "linux")]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use bench_rounds::host::{Host, Limit, RemotePath, Shell, Stop, each};
use bench_rounds::interrupt::Flag;
use bench_rounds::num::Num;
use bench_rounds::process::Runner;
use bench_rounds::provider::{Access, Provider, RoundId};
use bench_rounds::redact::{REDACTED_LENGTH, redact};
use bench_rounds::round_teardown::destroy_resources;
use bench_rounds::static_hosts::StaticHosts;

const LIMIT: Limit = Limit::secs(30);

#[test]
fn another_accounts_workspace_is_refused() {
    use std::os::unix::fs::MetadataExt;
    let _serial = serial();
    let dir = scratch("static-other-owner");
    let uid = std::fs::metadata(&dir).unwrap().uid();
    if uid == 0 {
        let work = dir.join("work");
        let mut provider = StaticHosts::new([local("server", &work, Runner::new(Flag::new()))]);
        let round = RoundId::from("r");
        let made = provider
            .create(&round, 0.0, &["server"], &Access::default())
            .unwrap();
        std::os::unix::fs::chown(&work, Some(65534), None).unwrap();
        assert!(made["server"].run_checked("true", LIMIT).is_err());
        assert!(provider.destroy(&round).is_err());
        std::os::unix::fs::chown(&work, Some(0), None).unwrap();
        provider.destroy(&round).unwrap();
    } else {
        // /tmp is root's on the supported hosts. Refusal must not mutate
        // its ownership, permissions, contents or directory identity.
        let work = Path::new("/tmp");
        let before = std::fs::metadata(work).unwrap();
        assert_ne!(before.uid(), uid);
        let mut provider = StaticHosts::new([local("server", work, Runner::new(Flag::new()))]);
        let round = RoundId::from("r");
        assert!(
            provider
                .create(&round, 0.0, &["server"], &Access::default())
                .is_err()
        );
        provider.destroy(&round).unwrap();
        let after = std::fs::metadata(work).unwrap();
        assert_eq!(
            (after.uid(), after.mode(), after.ino()),
            (before.uid(), before.mode(), before.ino())
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn allocation_result_loss_and_final_removal_failure_can_be_retried_safely() {
    let _serial = serial();
    for scenario in ["result-loss", "rmdir-failure", "before-mkdir"] {
        let dir = scratch(scenario);
        let bin = stand_in_openssh(&dir);
        let work = dir.join("work");
        let ssh = bin.join("ssh");
        if scenario != "rmdir-failure" {
            let allocation = if scenario == "before-mkdir" {
                "cat > /dev/null"
            } else {
                "bash -s > /dev/null"
            };
            std::fs::write(&ssh, format!("#!/bin/sh\nexport PATH={0}/bin:$PATH\nif [ ! -e {0}/lost ]; then\n{allocation}\ntouch {0}/lost\nexit 255\nfi\nexec bash -s\n", dir.display())).unwrap();
        } else {
            std::fs::write(
                &ssh,
                format!("#!/bin/sh\nPATH={}:$PATH exec bash -s\n", bin.display()),
            )
            .unwrap();
        }
        let rmdir = bin.join("rmdir");
        std::fs::write(&rmdir, format!("#!/bin/sh\nif [ ! -e {0}/failed ]; then\ntouch {0}/failed\nexit 1\nfi\nexec /usr/bin/rmdir \"$@\"\n", dir.display())).unwrap();
        std::fs::set_permissions(rmdir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let host = Host::ssh(
            "server",
            "alias",
            "127.0.0.1",
            remote(&work),
            Runner::new(Flag::new()),
        )
        .unwrap()
        .with_ssh_dir(&bin);
        let mut provider = StaticHosts::new([host]);
        let round = RoundId::from(scenario);
        let made = provider.create(&round, 0.0, &["server"], &Access::default());
        if scenario == "before-mkdir" {
            assert!(made.is_err());
            assert!(!work.exists());
            provider.destroy(&round).unwrap();
            assert!(provider.manual_cleanup(&round).is_empty());
            std::fs::remove_dir_all(dir).unwrap();
            continue;
        }
        if scenario == "result-loss" {
            assert!(made.is_err());
            assert!(work.join(".bench-owner").exists());
        } else {
            made.unwrap()["server"]
                .run_checked("echo key > keys.toml", LIMIT)
                .unwrap();
        }
        assert!(provider.destroy(&round).is_err());
        assert!(work.is_dir());
        assert!(!work.join(".bench-owner").exists());
        assert!(!work.join("keys.toml").exists());
        provider.destroy(&round).unwrap();
        assert!(!work.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
}

/// Private static workspaces are a tooling contract, not an RFC requirement.
#[test]
fn static_allocation_refuses_existing_directories_files_and_links() {
    let _serial = serial();
    let dir = scratch("static-refuse");
    let target = dir.join("target");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("sentinel"), "untouched").unwrap();
    for kind in ["directory", "file", "link"] {
        let work = dir.join(kind);
        match kind {
            "directory" => {
                std::fs::create_dir(&work).unwrap();
                std::fs::write(work.join("sentinel"), "untouched").unwrap();
            }
            "file" => std::fs::write(&work, "untouched").unwrap(),
            _ => std::os::unix::fs::symlink(&target, &work).unwrap(),
        }
        let mut provider = StaticHosts::new([local("server", &work, Runner::new(Flag::new()))]);
        provider.check(1.0, 0.0).unwrap();
        let round = RoundId::from("r");
        assert!(
            provider
                .create(&round, 0.0, &["server"], &Access::default())
                .is_err()
        );
        assert!(provider.destroy(&round).unwrap().is_empty());
        assert!(provider.manual_cleanup(&round).is_empty());
        assert!(std::fs::symlink_metadata(&work).is_ok());
    }
    assert_eq!(
        std::fs::read_to_string(target.join("sentinel")).unwrap(),
        "untouched"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn static_allocation_refuses_an_unprotected_parent() {
    let _serial = serial();
    let dir = scratch("static-parent");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
    let work = dir.join("work");
    let mut host = local("server", &work, Runner::new(Flag::new()));
    assert!(host.reserve_workspace().is_err());
    assert!(!work.exists());
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn owned_workspaces_refuse_replacement_and_never_recreate_missing_paths() {
    let _serial = serial();
    let dir = scratch("static-guard");
    let source = dir.join("source");
    std::fs::write(&source, "new binary").unwrap();
    for mutation in ["mode", "link", "replacement", "missing"] {
        let work = dir.join(mutation);
        let mut provider = StaticHosts::new([local("server", &work, Runner::new(Flag::new()))]);
        let round = RoundId::from(mutation);
        let made = provider
            .create(&round, 0.0, &["server"], &Access::default())
            .unwrap();
        let original = dir.join(format!("original-{mutation}"));
        match mutation {
            "mode" => {
                std::fs::set_permissions(&work, std::fs::Permissions::from_mode(0o777)).unwrap()
            }
            "missing" => std::fs::remove_dir_all(&work).unwrap(),
            _ => {
                std::fs::rename(&work, &original).unwrap();
                if mutation == "link" {
                    std::os::unix::fs::symlink(&original, &work).unwrap();
                } else {
                    std::fs::create_dir(&work).unwrap();
                    std::fs::set_permissions(&work, std::fs::Permissions::from_mode(0o700))
                        .unwrap();
                    std::fs::copy(original.join(".bench-owner"), work.join(".bench-owner"))
                        .unwrap();
                }
            }
        }
        let outside = dir.join(format!("executed-{mutation}"));
        let command = format!("true; echo escaped > {}", outside.display());
        assert!(made["server"].run_checked(&command, LIMIT).is_err());
        assert!(made["server"].output(&command, LIMIT).is_err());
        assert!(
            made["server"]
                .put(&source, &RemotePath::new("udpstp").unwrap(), 0o755)
                .is_err()
        );
        assert!(
            !outside.exists(),
            "a statement after a semicolon escaped the guard"
        );
        if mutation == "missing" {
            assert!(provider.destroy(&round).unwrap().is_empty());
            assert!(!work.exists());
        } else {
            assert!(
                provider.destroy(&round).is_err(),
                "cleanup accepted {mutation}"
            );
            assert!(work.exists());
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn static_teardown_only_removes_the_current_rounds_workspace() {
    let _serial = serial();
    let dir = scratch("static-round-owner");
    let work = dir.join("work");
    let mut provider = StaticHosts::new([local("server", &work, Runner::new(Flag::new()))]);
    let round = RoundId::from("owned");
    let made = provider
        .create(&round, 0.0, &["server"], &Access::default())
        .unwrap();
    made["server"]
        .run_checked("echo key > keys.toml", LIMIT)
        .unwrap();
    provider.destroy(&RoundId::from("other")).unwrap();
    assert!(work.join("keys.toml").exists());
    assert!(
        provider
            .create(&round, 0.0, &["server"], &Access::default())
            .is_err()
    );
    provider.destroy(&round).unwrap();
    provider.destroy(&round).unwrap();
    assert!(!work.exists());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn local_and_ssh_aliases_on_one_host_get_distinct_owned_workspaces() {
    let _serial = serial();
    let dir = scratch("static-aliases");
    let bin = stand_in_openssh(&dir);
    let work = dir.join("work");
    let runner = Runner::new(Flag::new());
    let ssh = |role, alias| {
        Host::ssh(role, alias, "127.0.0.1", remote(&work), runner.clone())
            .unwrap()
            .with_ssh_dir(&bin)
    };
    let mut provider = StaticHosts::new([
        local("client", &work, runner.clone()),
        ssh("server", "alias1"),
        ssh("peer", "alias2"),
        local("client2", &work, runner.clone()),
    ]);
    provider.check(1.0, 0.0).unwrap();
    assert!(!work.exists());
    let round = RoundId::from("aliases");
    let made = provider
        .create(
            &round,
            0.0,
            &["client", "server", "peer", "client2"],
            &Access::default(),
        )
        .unwrap();
    assert_eq!(made["client"].work(), made["client2"].work());
    assert_ne!(made["client"].work(), made["server"].work());
    assert_ne!(made["server"].work(), made["peer"].work());
    let source = dir.join("binary");
    std::fs::write(&source, "#!/bin/sh\necho intact\n").unwrap();
    for host in made.values() {
        assert_eq!(mode(Path::new(host.work().as_str())), 0o700);
        host.put(&source, &RemotePath::new("udpstp").unwrap(), 0o755)
            .unwrap();
        assert_eq!(host.output("./udpstp", LIMIT).unwrap(), b"intact\n");
    }
    provider.destroy(&round).unwrap();
    for host in made.values() {
        assert!(!Path::new(host.work().as_str()).exists());
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn interruption_during_allocation_records_ownership_before_teardown() {
    let _serial = serial();
    let dir = scratch("static-allocation-interrupt");
    let bin = stand_in_openssh(&dir);
    let work = dir.join("work");
    let flag = Flag::new();
    let ssh = bin.join("ssh");
    std::fs::write(&ssh, format!("#!/bin/sh\nbash -s > {0}/answer\nstatus=$?\ntouch {0}/allocated\nsleep 0.15\ncat {0}/answer\nexit $status\n", dir.display())).unwrap();
    let host = Host::ssh(
        "server",
        "alias",
        "127.0.0.1",
        remote(&work),
        Runner::new(flag.clone()),
    )
    .unwrap()
    .with_ssh_dir(&bin);
    let mut provider = StaticHosts::new([host]);
    let round = RoundId::from("interrupted");
    let trigger = dir.join("allocated");
    thread::scope(|scope| {
        scope.spawn(|| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !trigger.exists() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(1));
            }
            assert!(trigger.exists());
            flag.raise("SIGINT");
        });
        assert!(matches!(
            provider.create(&round, 0.0, &["server"], &Access::default()),
            Err(bench_rounds::provider::ProviderError::Interrupted)
        ));
    });
    assert_eq!(mode(&work), 0o700);
    let _begun = flag.stop_acting();
    provider.destroy(&round).unwrap();
    assert!(!work.exists());
    std::fs::remove_dir_all(dir).unwrap();
}

/// The profile matrix retains only its two binaries, even when another
/// role shares the Server's directory. Keys and all run evidence go.
#[test]
fn shared_static_roles_keep_only_the_requested_binaries() {
    let _serial = serial();
    let dir = scratch("static-keep");
    let work = dir.join("work");
    let runner = Runner::new(Flag::new());
    let mut provider = StaticHosts::new([
        local("server", &work, runner.clone()),
        local("client1", &work, runner),
    ]);
    let round = RoundId::from("r1");
    provider
        .create(&round, 0.0, &["server", "client1"], &Access::default())
        .unwrap();
    std::fs::create_dir_all(work.join("runs")).unwrap();
    for file in [
        "udpstp",
        "udpst",
        "keys.toml",
        "udpst.keys",
        "runs/result.json",
    ] {
        std::fs::write(work.join(file), file).unwrap();
    }
    provider.keep_files("server", &["udpstp", "udpst"]).unwrap();
    // A refused update, including a valid name before an invalid one,
    // must leave the previously requested retention in effect.
    for names in [&["../key"][..], &["keys.toml", "sub/file"][..]] {
        assert!(provider.keep_files("server", names).is_err());
    }
    assert!(provider.keep_files("absent", &["keys.toml"]).is_err());
    assert!(provider.destroy(&round).unwrap().is_empty());
    let mut files: Vec<_> = std::fs::read_dir(&work)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    files.sort();
    assert_eq!(files, [".bench-owner", "udpst", "udpstp"]);
    assert_eq!(
        provider.kept_lines(),
        [format!(
            "kept on purpose: {0}/udpstp, {0}/udpst on local; delete it once perf report has read it: rm -rf {0}",
            work.display()
        )]
    );
    assert_eq!(provider.manual_cleanup(&round).len(), 1);
    // A later attempt's diagnostics replace the previous attempt's.
    provider.keep_files("client1", &[]).unwrap();
    assert!(provider.destroy(&round).unwrap().is_empty());
    assert!(!work.exists());
    assert!(provider.kept_lines().is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}

/// One test at a time, each taking it first. A test writes a script and
/// then runs it; another test's fork meanwhile copies the script's open
/// write handle into its child, which holds it until it execs, and the
/// script can't be run while any process holds it open for writing: "Text
/// file busy" (ETXTBSY, rust-lang/rust), about 1 run in 100 here.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bench-rounds-host-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn remote(path: &Path) -> RemotePath {
    RemotePath::new(path.to_str().unwrap()).unwrap()
}

fn local(role: &str, work: &Path, runner: Runner) -> Host {
    Host::local(role, "127.0.0.1", remote(work), runner)
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

/// The Python's `Rules.test_a_host_error_carries_no_key`: a host's error
/// goes to round.log, meta.json and the report.
#[test]
fn a_host_error_carries_no_key() {
    let _serial = serial();
    let key = "0123456789abcdef0123456789abcdef";
    let work = scratch("key");
    let host = local("client", &work, Runner::new(Flag::new()));
    let Err(Stop::Host(e)) = host.run_checked(
        &format!("./udpst -a {key} -y 7 h:1; echo {key} >&2; exit 3"),
        LIMIT,
    ) else {
        panic!("a host error")
    };
    assert!(!e.message().contains(key), "{e}");
    assert!(e.message().contains("./udpst -a <redacted> -y 7"), "{e}");
    assert!(e.message().starts_with("client: exit 3: "), "{e}");
    let Err(Stop::Host(e)) = host.run(
        &format!("sleep 5 # {key}"),
        Limit::new(Num::Float(0.2)).unwrap(),
    ) else {
        panic!("a host error")
    };
    assert!(!e.message().contains(key), "{e}");
    assert_eq!(
        e.message(),
        "client: timed out after 0.2 s: sleep 5 # <redacted>"
    );
    assert!(redact(&"x".repeat(5000)).chars().count() <= REDACTED_LENGTH + 2);
    std::fs::remove_dir_all(&work).unwrap();
}

#[test]
fn scripts_run_in_the_work_directory_which_they_make() {
    let _serial = serial();
    let dir = scratch("work");
    let work = dir.join("w");
    let host = local("server", &work, Runner::new(Flag::new()));
    let done = host.run("pwd; true", LIMIT).unwrap();
    assert_eq!(done.stdout, format!("{}\n", work.display()));
    assert!(work.is_dir());
    assert_eq!(host.output("printf x", LIMIT).unwrap(), b"x");
    let Err(Stop::Host(e)) = host.output("echo bad >&2; exit 5", LIMIT) else {
        panic!("a host error")
    };
    assert_eq!(e.message(), "server: exit 5: echo bad >&2; exit 5\nbad");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Review m2: garbage from a host is its error, voiding the Round Run it
/// came in: output that isn't UTF-8, on either stream, and an archive that
/// can't be unpacked. Bytes are what `output` is for.
#[test]
fn garbage_from_a_host_is_its_error() {
    let _serial = serial();
    let dir = scratch("garbage");
    let host = local("client1", &dir, Runner::new(Flag::new()));
    for (script, stream) in [
        (r"printf 'ok \377'", "output"),
        (r"printf '\376' >&2", "error output"),
        (r"printf '\376' >&2; exit 4", "error output"),
    ] {
        for done in [host.run(script, LIMIT), host.run_checked(script, LIMIT)] {
            let Err(Stop::Host(e)) = done else {
                panic!("{script}: a host error, not {done:?}")
            };
            assert_eq!(
                e.message(),
                format!("client1: its {stream} isn't UTF-8: {script}")
            );
        }
    }
    assert_eq!(host.output(r"printf '\377'", LIMIT).unwrap(), b"\xff");
    // A directory that isn't one: tar's own error, as the host's.
    std::fs::write(dir.join("not-a-dir"), "x").unwrap();
    assert!(matches!(
        host.fetch_dir(&RemotePath::new("not-a-dir").unwrap(), &dir.join("into")),
        Err(Stop::Host(_))
    ));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The Python's `Fetch.test_a_directory_comes_back_whole`.
#[test]
fn a_directory_comes_back_whole() {
    let _serial = serial();
    let (work, into) = (scratch("fetch-work"), scratch("fetch-into"));
    std::fs::create_dir_all(work.join("runs/x/clients")).unwrap();
    std::fs::write(work.join("runs/x/server.log"), "{}\n").unwrap();
    std::fs::write(work.join("runs/x/clients/1.json"), "[1]").unwrap();
    let host = local("server", &work, Runner::new(Flag::new()));
    host.fetch_dir(&RemotePath::new("runs/x").unwrap(), &into)
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(into.join("clients/1.json")).unwrap(),
        "[1]"
    );
    let copy = into.join("copy");
    host.fetch_file(&RemotePath::new("runs/x/server.log").unwrap(), &copy)
        .unwrap();
    assert_eq!(std::fs::read_to_string(&copy).unwrap(), "{}\n");
    assert!(matches!(
        host.fetch_dir(&RemotePath::new("runs/missing").unwrap(), &into),
        Err(Stop::Host(_))
    ));
    for dir in [work, into] {
        std::fs::remove_dir_all(dir).unwrap();
    }
}

/// Review S1: a host that floods its output fails the call, as its own
/// error.
#[test]
fn a_host_that_floods_its_output_fails_the_call() {
    let _serial = serial();
    let work = scratch("flood");
    let host = local(
        "server",
        &work,
        Runner::new(Flag::new()).with_output_cap(1 << 20),
    );
    let Err(Stop::Host(e)) = host.output("head -c 3000000 /dev/zero", LIMIT) else {
        panic!("a host error")
    };
    assert_eq!(
        e.message(),
        "server: more than 1 MiB of output: head -c 3000000 /dev/zero"
    );
    std::fs::remove_dir_all(&work).unwrap();
}

#[test]
fn put_copies_into_the_work_directory_with_the_mode() {
    let _serial = serial();
    let dir = scratch("put");
    let source = dir.join("udpstp-built");
    std::fs::write(&source, "#!/bin/sh\necho hello\n").unwrap();
    let work = dir.join("w");
    let host = local("server", &work, Runner::new(Flag::new()));
    host.put(&source, &RemotePath::new("udpstp").unwrap(), 0o755)
        .unwrap();
    host.put(&source, &RemotePath::new("keys.toml").unwrap(), 0o600)
        .unwrap();
    assert_eq!(mode(&work.join("udpstp")), 0o755);
    assert_eq!(mode(&work.join("keys.toml")), 0o600);
    assert_eq!(host.output("./udpstp", LIMIT).unwrap(), b"hello\n");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A stand-in for OpenSSH in `dir/bin`: `ssh` records its argv and its
/// standard input, then runs its last argument with `sh -c`, reading that
/// input, as the remote login shell would; `scp` records its argv and
/// copies its last-but-one argument to the path after the last one's `:`.
fn stand_in_openssh(dir: &Path) -> PathBuf {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let d = dir.display();
    let ssh = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > {d}/ssh-argv\ncat > {d}/ssh-stdin\n\
         for a; do last=$a; done\nexec sh -c \"$last\" < {d}/ssh-stdin\n"
    );
    let scp = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > {d}/scp-argv\n\
         for a; do from=$to; to=$a; done\nexec cp \"$from\" \"${{to#*:}}\"\n"
    );
    for (name, text) in [("ssh", ssh), ("scp", scp)] {
        let path = bin.join(name);
        std::fs::write(&path, text).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    bin
}

/// Over ssh the script goes on standard input and `bash -s` alone on the
/// command line (§3.4).
#[test]
fn over_ssh_the_script_goes_on_standard_input() {
    let _serial = serial();
    let dir = scratch("ssh");
    let bin = stand_in_openssh(&dir);
    let work = dir.join("w");
    let host = Host::ssh(
        "server",
        "bench@testhost",
        "192.168.178.22",
        remote(&work),
        Runner::new(Flag::new()),
    )
    .unwrap()
    .with_ssh_dir(&bin)
    .with_known_hosts(dir.join("known_hosts"))
    .with_identity(dir.join("id_ed25519"));
    let done = host.run("echo hello; echo MARKER-51", LIMIT).unwrap();
    assert_eq!(done.stdout, "hello\nMARKER-51\n");
    let argv = std::fs::read_to_string(dir.join("ssh-argv")).unwrap();
    let argv: Vec<&str> = argv.lines().collect();
    assert_eq!(
        argv,
        [
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=10",
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "StrictHostKeyChecking=accept-new",
            "-o",
            &format!("UserKnownHostsFile={}", dir.join("known_hosts").display()),
            "-i",
            &dir.join("id_ed25519").display().to_string(),
            "-o",
            "IdentitiesOnly=yes",
            "-T",
            "bench@testhost",
            "bash -s"
        ]
    );
    let stdin = std::fs::read_to_string(dir.join("ssh-stdin")).unwrap();
    let w = work.display();
    assert_eq!(
        stdin,
        format!("mkdir -p {w} && cd {w} && echo hello; echo MARKER-51")
    );

    let source = dir.join("built");
    std::fs::write(&source, "binary").unwrap();
    host.put(&source, &RemotePath::new("udpstp").unwrap(), 0o755)
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(work.join("udpstp")).unwrap(),
        "binary"
    );
    assert_eq!(mode(&work.join("udpstp")), 0o755);
    let scp = std::fs::read_to_string(dir.join("scp-argv")).unwrap();
    let scp: Vec<&str> = scp.lines().collect();
    assert_eq!(
        scp[scp.len() - 3..],
        [
            "-q",
            source.to_str().unwrap(),
            &format!("bench@testhost:{w}/udpstp")
        ]
    );

    std::fs::create_dir_all(work.join("runs/r/server")).unwrap();
    std::fs::write(work.join("runs/r/server/server.log"), "{\"x\": 1}\n").unwrap();
    let into = dir.join("into");
    host.fetch_dir(&RemotePath::new("runs/r").unwrap(), &into)
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(into.join("server/server.log")).unwrap(),
        "{\"x\": 1}\n"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// An interrupt reaches the caller as one, from every kind of call, and
/// `each` puts it ahead of a host's error that came first.
#[test]
fn an_interrupt_is_never_a_host_error() {
    let _serial = serial();
    let dir = scratch("interrupt");
    let flag = Flag::new();
    let host = local("client1", &dir, Runner::new(flag.clone()));
    let failing = local("client2", &dir, Runner::new(flag.clone()));
    let raiser = {
        let flag = flag.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(300));
            flag.raise("a test");
        })
    };
    let start = Instant::now();
    let calls: Vec<Box<dyn FnOnce() -> Result<(), Stop> + Send + '_>> = vec![
        Box::new(|| failing.run_checked("exit 1", LIMIT).map(|_| ())),
        Box::new(|| host.run("sleep 30", LIMIT).map(|_| ())),
    ];
    let results = each(calls);
    assert!(matches!(results, Err(Stop::Interrupted)), "{results:?}");
    assert!(start.elapsed() < Duration::from_secs(3));
    raiser.join().unwrap();
    // Raised: nothing more starts, whatever the call.
    assert!(matches!(host.run("true", LIMIT), Err(Stop::Interrupted)));
    assert!(matches!(host.output("true", LIMIT), Err(Stop::Interrupted)));
    assert!(matches!(
        host.fetch_dir(&RemotePath::new("x").unwrap(), &dir.join("into")),
        Err(Stop::Interrupted)
    ));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A plan leaves nothing on the hosts, and the Round Teardown removes the
/// work directory with the Round's binaries and keys, after a Ctrl-C too:
/// once the flag has stopped acting, which the Round Teardown's proof
/// says, a raised flag stops none of its commands (review S2).
#[test]
fn static_hosts_leave_nothing_behind() {
    let _serial = serial();
    let dir = scratch("static");
    let work = dir.join("udpstp-bench-4242");
    let flag = Flag::new();
    let runner = Runner::new(flag.clone());
    let mut provider = StaticHosts::new([
        local("server", &work, runner.clone()),
        local("client", &work, runner.clone()),
    ]);
    provider.check(2.0, 0.0).unwrap();
    assert!(!work.exists(), "the check left its work directory");
    let round = RoundId::from("r1");
    let hosts = provider
        .create(&round, 0.0, &["server", "client"], &Access::default())
        .unwrap();
    hosts["server"]
        .run_checked("echo 7 key > udpst.keys", LIMIT)
        .unwrap();
    assert!(work.join("udpst.keys").exists());
    assert_eq!(mode(&work), 0o700);
    assert_eq!(hosts["server"].work(), hosts["client"].work());
    // Repeating preflight never deletes and recreates the acquired directory.
    provider.check(2.0, 0.0).unwrap();
    assert!(work.join("udpst.keys").exists());
    // Interrupted: from here the flag stops the Round's commands, until
    // the Round Teardown begins.
    flag.raise("SIGINT");
    assert!(matches!(
        hosts["server"].run("true", LIMIT),
        Err(Stop::Interrupted)
    ));
    let begun = flag.stop_acting();
    let (mut logged, mut alerted, mut slept) = (Vec::new(), Vec::new(), Vec::new());
    let left = destroy_resources(
        &mut provider,
        &round,
        begun,
        &mut |l: &str| logged.push(l.to_owned()),
        &mut |l: &str| alerted.push(l.to_owned()),
        &mut |d| slept.push(d),
    );
    assert!(left.is_empty() && logged.is_empty() && alerted.is_empty() && slept.is_empty());
    assert!(!work.exists(), "the Round Teardown left the keys");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A host that fails in the Round Teardown doesn't keep another's keys:
/// every host is tried, then the first failure is the attempt's.
#[test]
fn static_hosts_destroy_every_host_even_after_one_fails() {
    let _serial = serial();
    let dir = scratch("static-fail");
    let work = dir.join("w");
    let bin = stand_in_openssh(&dir);
    // Both workspaces are allocated before ssh becomes unavailable.
    let broken = Host::ssh(
        "server",
        "nobody@nowhere",
        "",
        remote(&work),
        Runner::new(Flag::new()),
    )
    .unwrap()
    .with_ssh_dir(&bin);
    let mut provider = StaticHosts::new([broken, local("client", &work, Runner::new(Flag::new()))]);
    let round = RoundId::from("r1");
    let made = provider
        .create(&round, 0.0, &["server", "client"], &Access::default())
        .unwrap();
    let client_work = PathBuf::from(made["client"].work().as_str());
    made["client"]
        .run_checked("echo key > keys.toml", LIMIT)
        .unwrap();
    std::fs::remove_file(bin.join("ssh")).unwrap();
    let error = provider.destroy(&RoundId::from("r1")).unwrap_err();
    assert!(
        error.to_string().starts_with("server: couldn't start"),
        "{error}"
    );
    assert!(!client_work.exists(), "the second host wasn't tried");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Review m3: a work directory `rm` can't remove fails each attempt, and
/// is left, with the command that removes it by hand; the Python took
/// whatever `rm` said until its fix on homelab-c.
#[test]
fn a_work_directory_rm_cant_remove_is_left() {
    let _serial = serial();
    if std::fs::read_to_string("/proc/self/status")
        .unwrap()
        .lines()
        .any(|l| l.starts_with("Uid:") && l.split_whitespace().nth(2) == Some("0"))
    {
        println!("skipped: as root, rm removes what its mode protects");
        return;
    }
    let dir = scratch("static-rm");
    let work = dir.join("udpstp-bench-4242");
    let flag = Flag::new();
    let mut provider = StaticHosts::new([local("client", &work, Runner::new(flag.clone()))]);
    provider
        .create(&RoundId::from("r1"), 0.0, &["client"], &Access::default())
        .unwrap();
    let kept = work.join("kept");
    std::fs::create_dir_all(&kept).unwrap();
    std::fs::write(kept.join("keys.toml"), "key").unwrap();
    // A directory whose entries can't be removed.
    std::fs::set_permissions(&kept, std::fs::Permissions::from_mode(0o555)).unwrap();
    let (mut logged, mut alerted, mut slept) = (Vec::new(), Vec::new(), Vec::new());
    let left = destroy_resources(
        &mut provider,
        &RoundId::from("r1"),
        flag.stop_acting(),
        &mut |l: &str| logged.push(l.to_owned()),
        &mut |l: &str| alerted.push(l.to_owned()),
        &mut |d| slept.push(d),
    );
    assert_eq!(left.len(), 1, "{left:?}");
    assert_eq!(logged.len(), 3, "{logged:?}");
    assert!(
        logged[0].starts_with("Round Teardown attempt 1 failed: client: exit 1:"),
        "{logged:?}"
    );
    assert_eq!(slept.len(), 3);
    assert_eq!(alerted.len(), 2, "{alerted:?}");
    assert!(alerted[0].contains("LEFT BEHIND, delete by hand: unknown (client: exit 1"));
    assert_eq!(
        alerted[1],
        format!("\x1b[31m  rm -rf {}\x1b[0m", work.display())
    );
    assert!(kept.join("keys.toml").exists());
    std::fs::set_permissions(&kept, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

/// `tc -s qdisc` of an mq root with two children, as the Python's
/// `Counters` tests give it (`tests/test_scale.py`'s `TC_MQ`).
const TC_MQ: &str = "qdisc mq 0: root
 Sent 90000 bytes 60 pkt (dropped 7, overlimits 0 requeues 1)
 backlog 0b 0p requeues 1
qdisc fq_codel 0: parent :2 limit 10240p flows 1024 quantum 1514 target 5ms interval 100ms ecn drop_batch 64
 Sent 45000 bytes 30 pkt (dropped 4, overlimits 0 requeues 0)
 backlog 0b 0p requeues 0
  maxpacket 0 drop_overlimit 0 new_flow_count 0 ecn_mark 0
qdisc fq_codel 0: parent :1 limit 10240p flows 1024 quantum 1514 target 5ms interval 100ms ecn drop_batch 64
 Sent 45000 bytes 30 pkt (dropped 3, overlimits 0 requeues 1)
";

/// The Python's `Counters.test_the_queueing_disciplines_drops_are_its_roots`:
/// an mq root sums its children, so counting both would double them.
#[test]
fn the_queueing_disciplines_drops_are_its_roots() {
    let _serial = serial();
    let work = scratch("qdisc");
    std::fs::write(work.join("tc.txt"), TC_MQ).unwrap();
    let host = local("server", &work, Runner::new(Flag::new()));
    let read = |script: &str| host.run_checked(script, LIMIT).unwrap().stdout;
    let dropped = bench_rounds::counters::QDISC_DROPPED;
    assert_eq!(read(&format!("{dropped} < tc.txt")), "qdisc_dropped: 7\n");
    // Nothing to read (no tc, no device): no count at all, never 0.
    assert_eq!(read(&format!("{dropped} < /dev/null")), "");
    std::fs::remove_dir_all(&work).unwrap();
}

/// The Python's `Counters.test_a_hosts_counters_are_read_whole`: the
/// script itself, on this host. Whatever its NIC and tc, every counter
/// the judging reads is there, the NIC's drops from the kernel's
/// statistics of the device it routes through.
#[test]
fn a_hosts_counters_are_read_whole() {
    use bench_rounds::counters;
    let _serial = serial();
    let work = scratch("counters");
    let host = local("server", &work, Runner::new(Flag::new()));
    let text = host.run(counters::SCRIPT, LIMIT).unwrap().stdout;
    let host_drops = [
        "RcvbufErrors",
        "InErrors",
        "softnet_dropped",
        "rx_drops",
        "rx_missed",
        "tx_drops",
        "qdisc_dropped",
    ];
    assert_eq!(
        counters::missing(&text, &host_drops).unwrap(),
        Vec::<String>::new(),
        "{text}"
    );
    let found = counters::parse(&text).unwrap();
    assert!(found.contains_key("SndbufErrors"), "{text}");
    let dev = host
        .run_checked(
            r"ip -o route get 1.1.1.1 | sed -n 's/.* dev \([^ ]*\).*/\1/p'",
            LIMIT,
        )
        .unwrap()
        .stdout;
    let dropped = std::fs::read_to_string(format!(
        "/sys/class/net/{}/statistics/tx_dropped",
        dev.trim()
    ))
    .unwrap();
    assert!(found["tx_drops"] <= dropped.trim().parse::<i128>().unwrap());
    std::fs::remove_dir_all(&work).unwrap();
}

/// The Python's `Counters.test_the_thermal_throttles_count_each_core_once`:
/// a laptop's logical CPUs 0 and 2 share core 0, 1 and 3 core 1, and
/// each reports its core's count and the package's.
#[test]
fn the_thermal_throttles_count_each_core_once() {
    use bench_rounds::counters;
    let _serial = serial();
    let work = scratch("throttles");
    for (cpu, core, count) in [(0, 0, 100), (1, 1, 40), (2, 0, 100), (3, 1, 40)] {
        let base = work.join(format!("cpu{cpu}"));
        for (name, value) in [
            ("topology/core_id", core),
            ("topology/physical_package_id", 0),
            ("thermal_throttle/core_throttle_count", count),
            ("thermal_throttle/package_throttle_count", 700),
        ] {
            let path = base.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, format!("{value}\n")).unwrap();
        }
    }
    let host = local("server", &work, Runner::new(Flag::new()));
    let cpus = work.to_str().unwrap();
    let text = host
        .run_checked(&counters::throttles_script(cpus), LIMIT)
        .unwrap()
        .stdout;
    assert_eq!(text, "core_throttle: 140\npackage_throttle: 700\n");
    let found: Vec<(String, i128)> = counters::parse(&text).unwrap().into_iter().collect();
    assert_eq!(
        found,
        [
            ("core_throttle".to_string(), 140),
            ("package_throttle".to_string(), 700)
        ]
    );
    // A VM has none: nothing at all, not 0.
    let none = counters::throttles_script(&format!("{cpus}/none"));
    assert_eq!(host.run_checked(&none, LIMIT).unwrap().stdout, "");
    std::fs::remove_dir_all(&work).unwrap();
}
