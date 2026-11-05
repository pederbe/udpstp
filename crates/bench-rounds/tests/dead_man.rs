//! The dead-man switch's texts run under a real shell, with stand-ins for
//! what they call (`tests/test_round.py`'s `DeadMan`): the first boot's
//! user data, the script every later boot runs, and the check, each where
//! systemd and the user data would leave their files. Linux only: they
//! need `bash`, `sh` and coreutils.
#![cfg(target_os = "linux")]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use bench_rounds::dead_man;

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> TempDir {
        let dir = std::env::temp_dir().join(format!(
            "bench-rounds-dead-man-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Stand-ins for what the dead-man switch runs, in `work/fakes`: each
/// appends its name and arguments to `work/calls`; `date` prints
/// `$FAKE_NOW`, `install` makes the directory it is given last, and
/// `systemctl is-enabled` finds any unit enabled.
fn fake_commands(work: &Path) -> PathBuf {
    let fakes = work.join("fakes");
    std::fs::create_dir_all(&fakes).unwrap();
    let calls = work.join("calls");
    let record = format!("echo \"$(basename \"$0\") $*\" >> '{}'\n", calls.display());
    for (name, body) in [
        ("shutdown", record.clone()),
        ("systemctl", record.clone()),
        ("chown", record.clone()),
        (
            "install",
            format!("{record}for last; do :; done; mkdir -p \"$last\"\n"),
        ),
        ("date", "echo \"$FAKE_NOW\"\n".to_string()),
    ] {
        let path = fakes.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    fakes
}

/// Runs `argv` with the stand-ins first on `PATH` at Unix time `now`; the
/// calls recorded so far.
fn boot(work: &Path, fakes: &Path, argv: &[&str], now: i64) -> Vec<String> {
    let done = Command::new(argv[0])
        .args(&argv[1..])
        .env("PATH", format!("{}:/usr/bin:/bin", fakes.display()))
        .env("FAKE_NOW", now.to_string())
        .current_dir(work)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        done.status.success(),
        "{}",
        String::from_utf8_lossy(&done.stderr)
    );
    std::fs::read_to_string(work.join("calls"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn the_user_data_is_a_bash_script() {
    let data = dead_man::user_data(120, "ssh-ed25519 AAAA test", 1_790_018_000);
    let mut child = Command::new("bash")
        .arg("-n")
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(data.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
}

#[test]
fn the_switch_is_armed_again_at_every_boot() {
    // The first boot, then reboots, on paths of their own.
    let tmp = TempDir::new("boot");
    let (work, deadline) = (&tmp.0, 1_790_018_000);
    let root = work.join("root");
    let root_text = root.display().to_string();
    let data = dead_man::user_data(120, "ssh-ed25519 AAAA t", deadline)
        .replace(dead_man::PATH, &format!("{root_text}{}", dead_man::PATH))
        .replace(
            "/etc/systemd/system/",
            &format!("{root_text}/etc/systemd/system/"),
        )
        .replace("/home/ubuntu", &format!("{root_text}/home/ubuntu"));
    for directory in ["usr/local/sbin", "etc/systemd/system"] {
        std::fs::create_dir_all(root.join(directory)).unwrap();
    }
    let fakes = fake_commands(work);
    let calls = boot(work, &fakes, &["bash", "-c", &data], deadline - 7200);
    assert_eq!(calls[0], "shutdown -P +120");
    assert!(calls.contains(&format!("systemctl enable {}", dead_man::UNIT)));
    let unit =
        std::fs::read_to_string(root.join("etc/systemd/system").join(dead_man::UNIT)).unwrap();
    assert!(unit.contains(&format!("ExecStart={root_text}{}\n", dead_man::PATH)));
    assert!(unit.contains("WantedBy=multi-user.target\n"));
    let at_boot = PathBuf::from(format!("{root_text}{}", dead_man::PATH));
    assert_eq!(
        std::fs::read_to_string(&at_boot).unwrap(),
        dead_man::at_boot(deadline)
    );
    assert_ne!(
        std::fs::metadata(&at_boot).unwrap().permissions().mode() & 0o111,
        0
    );
    assert_eq!(
        std::fs::read_to_string(root.join("home/ubuntu/.ssh/authorized_keys")).unwrap(),
        "ssh-ed25519 AAAA t\n"
    );
    // Each later boot schedules the power-off for what is left of the
    // Round, in whole minutes rounded up, or powers off at once.
    let script = at_boot.display().to_string();
    for (now, last) in [
        (deadline - 3601, "shutdown -P +61"),
        (deadline - 30, "shutdown -P +1"),
        (deadline, "systemctl --no-block poweroff"),
        (deadline + 600, "systemctl --no-block poweroff"),
    ] {
        let calls = boot(work, &fakes, &["sh", &script], now);
        assert_eq!(calls.last().map(String::as_str), Some(last), "{now}");
    }
}

#[test]
fn the_check_reads_what_systemd_and_the_user_data_leave() {
    // The check itself, on this host, reading files where systemd and the
    // user data would leave them, with a systemctl that finds the unit
    // enabled.
    let tmp = TempDir::new("check");
    let work = &tmp.0;
    let scheduled = work.join("scheduled");
    std::fs::write(
        &scheduled,
        "USEC=1790018000000000\nWARN_WALL=1\nMODE=poweroff\n",
    )
    .unwrap();
    let at_boot = work.join("dead-man");
    std::fs::write(&at_boot, dead_man::at_boot(1_790_018_000)).unwrap();
    std::fs::set_permissions(&at_boot, std::fs::Permissions::from_mode(0o755)).unwrap();
    let fakes = fake_commands(work);
    let script = dead_man::CHECK_SCRIPT
        .replace(
            "/run/systemd/shutdown/scheduled",
            &scheduled.display().to_string(),
        )
        .replace(dead_man::PATH, &at_boot.display().to_string());
    let done = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .env("PATH", format!("{}:/usr/bin:/bin", fakes.display()))
        .current_dir(work)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let stdout = String::from_utf8(done.stdout).unwrap();
    assert_eq!(
        dead_man::verdict("server", done.status.code(), &stdout),
        Ok(
            "dead-man switch armed: USEC=1790018000000000 WARN_WALL=1 MODE=poweroff at every boot \
            until 1790018000"
                .to_string()
        )
    );
}
