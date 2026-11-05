//! The dead-man switch of a rented host, which powers it off at the
//! Round's deadline whatever becomes of the tool: the first boot's user
//! data, which schedules the power-off before anything that could fail;
//! the unit and script that schedule it again at every later boot, since
//! a scheduled shutdown lives in `/run` and user data runs only once; and
//! the check that it is armed before anything is copied to the host
//! (`scripts/bench/aws.py:50-104`, `round.py:465-488`).
//!
//! The texts are the Python's byte for byte: a Round of either tool arms
//! the same switch.

use crate::host::{HostError, Limit, Shell, Stop};
use crate::py::{self, Json};

/// The unit that arms the switch at every boot.
///
/// It and [`PATH`] are `udpstp-bench`'s names, hard-coded here, in a crate
/// that otherwise knows nothing of udpstp, until the switch, so that
/// either tool's Round arms and checks the same switch
///.
pub const UNIT: &str = "udpstp-bench-dead-man.service";

/// The script the unit runs.
pub const PATH: &str = "/usr/local/sbin/udpstp-bench-dead-man";

/// `dead_man_at_boot(deadline)`: the script a rented host runs at every
/// boot. It schedules the power-off at the Round's `deadline` (Unix time,
/// whole seconds), in whole minutes rounded up, or powers off at once
/// where the deadline has passed.
pub fn at_boot(deadline: i64) -> String {
    format!(
        "#!/bin/sh\n\
         # The udpstp bench round's dead-man switch, at every boot.\n\
         deadline={deadline}\n\
         left=$(( ($deadline - $(date +%s) + 59) / 60 ))\n\
         if [ \"$left\" -gt 0 ]; then exec shutdown -P \"+$left\"; fi\n\
         exec systemctl --no-block poweroff\n"
    )
}

/// `dead_man_unit()`: the unit that runs [`at_boot`]'s script at every
/// boot, once logind, which holds a scheduled shutdown, is up.
pub fn unit() -> String {
    format!(
        "[Unit]\n\
         Description=The udpstp bench round's dead-man switch: power off at the round's deadline\n\
         After=systemd-logind.service\n\
         \n\
         [Service]\n\
         Type=oneshot\n\
         ExecStart={PATH}\n\
         \n\
         [Install]\n\
         WantedBy=multi-user.target\n"
    )
}

/// `user_data(minutes, public_key, deadline)`: the instance's first boot.
/// The dead-man switch first, before anything that could fail, `minutes`
/// from now; then the same switch at every later boot, from the Round's
/// `deadline`; then the Round's SSH key, stripped of Python's whitespace
/// and quoted as `json.dumps` quotes it.
pub fn user_data(minutes: i64, public_key: &str, deadline: i64) -> String {
    let key = py::dumps(&Json::Str(py::strip(public_key).to_string()));
    format!(
        "#!/bin/bash\n\
         shutdown -P +{minutes}\n\
         cat > {PATH} <<'EOF'\n{at_boot}EOF\n\
         chmod 755 {PATH}\n\
         cat > /etc/systemd/system/{UNIT} <<'EOF'\n{unit}EOF\n\
         systemctl enable {UNIT}\n\
         install -d -m 700 -o ubuntu -g ubuntu /home/ubuntu/.ssh\n\
         echo {key} >> /home/ubuntu/.ssh/authorized_keys\n\
         chown ubuntu:ubuntu /home/ubuntu/.ssh/authorized_keys\n\
         chmod 600 /home/ubuntu/.ssh/authorized_keys\n",
        at_boot = at_boot(deadline),
        unit = unit(),
    )
}

/// `int(t)` of a Unix time, as the texts take the deadline: truncated
/// toward zero; `None` for a NaN or an infinity, where Python raises.
pub fn whole_seconds(t: f64) -> Option<i64> {
    t.is_finite().then(|| t.trunc() as i64)
}

/// The script that checks the switch on a host (`DEAD_MAN_SCRIPT`):
/// systemd's record of the shutdown the user data scheduled, and the unit
/// with the deadline it keeps. cloud-init runs the user data after ssh
/// answers, so it waits for them.
pub const CHECK_SCRIPT: &str = concat!(
    "for _ in $(seq 90); do [ -s /run/systemd/shutdown/scheduled ] && break; sleep 2; done;",
    " [ -s /run/systemd/shutdown/scheduled ] || exit 1; tr '\\n' ' ' < /run/systemd/shutdown/scheduled;",
    " for _ in $(seq 15); do systemctl is-enabled --quiet udpstp-bench-dead-man.service && break; sleep 2; done;",
    " systemctl is-enabled --quiet udpstp-bench-dead-man.service && [ -x /usr/local/sbin/udpstp-bench-dead-man ] &&",
    " echo \"at every boot until $(sed -n 's/^deadline=//p' /usr/local/sbin/udpstp-bench-dead-man)\"; exit 0",
);

/// How long [`CHECK_SCRIPT`] may take on a host: the Python's `timeout=300`,
/// an int, as a host's error prints it.
pub const CHECK_LIMIT: Limit = Limit::secs(300);

/// `check_dead_man(host)` (`round.py:477-488`): runs [`CHECK_SCRIPT`] on
/// `host` for at most [`CHECK_LIMIT`], whatever its exit status, then
/// gives [`verdict`]'s: what systemd scheduled and the deadline the unit
/// keeps, for the log. A switch that isn't armed, or wouldn't survive a
/// reboot, is the host's error, whose message is the Python's refusal: the
/// Round stops before anything is copied to the host, as the Python's
/// `Refused` stops it. An interrupt stays one.
pub fn check(host: &dyn Shell) -> Result<String, Stop> {
    let done = host.run(CHECK_SCRIPT, CHECK_LIMIT)?;
    verdict(host.role(), Some(done.returncode), &done.stdout)
        .map_err(|refusal| HostError::new(refusal).into())
}

/// `check_dead_man`'s verdict on what [`CHECK_SCRIPT`] said on the host
/// of `role`: its exit status (`None` where a signal ended it) and its
/// standard output. What systemd scheduled and the deadline the unit
/// keeps, for the log; or the refusal: the host's switch isn't armed, or
/// wouldn't survive a reboot.
///
/// The unit's deadline must be ASCII digits, which the user data writes;
/// the Python's `\d` would take other scripts' digits too.
pub fn verdict(role: &str, status: Option<i32>, stdout: &str) -> Result<String, String> {
    if status != Some(0) || !stdout.contains("USEC=") {
        return Err(format!(
            "{role}: its dead-man switch isn't armed (no shutdown scheduled)"
        ));
    }
    const UNTIL: &str = "at every boot until ";
    let armed_at_boot = stdout
        .match_indices(UNTIL)
        .any(|(at, _)| stdout[at + UNTIL.len()..].starts_with(|c: char| c.is_ascii_digit()));
    if !armed_at_boot {
        return Err(format!(
            "{role}: its dead-man switch wouldn't survive a reboot (no unit arms it at boot)"
        ));
    }
    Ok(format!("dead-man switch armed: {}", py::strip(stdout)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dead_man_switch_comes_first() {
        let data = user_data(120, "ssh-ed25519 AAAA test", 1_790_018_000);
        let lines: Vec<&str> = data.lines().collect();
        assert_eq!(lines[..2], ["#!/bin/bash", "shutdown -P +120"]);
        assert!(data.contains(&at_boot(1_790_018_000)));
        assert!(data.contains("deadline=1790018000\n"));
        assert!(data.contains(&format!("systemctl enable {UNIT}\n")));
        assert!(
            data.contains("echo \"ssh-ed25519 AAAA test\" >> /home/ubuntu/.ssh/authorized_keys\n")
        );
    }

    #[test]
    fn the_key_is_stripped_and_quoted_as_json_dumps_quotes_it() {
        let data = user_data(1, " ssh-ed25519 AAAA \"x\"\\ é\n", 0);
        assert!(data.contains("echo \"ssh-ed25519 AAAA \\\"x\\\"\\\\ \\u00e9\" >> "));
    }

    #[test]
    fn a_deadline_is_whole_seconds() {
        assert_eq!(whole_seconds(1_790_018_000.9), Some(1_790_018_000));
        assert_eq!(whole_seconds(-0.5), Some(0));
        assert_eq!(whole_seconds(f64::NAN), None);
    }

    #[test]
    fn a_host_without_its_shutdown_scheduled_is_refused() {
        // tests/test_round.py's DeadMan.
        let armed = verdict(
            "client1",
            Some(0),
            "USEC=1790018000000000 WARN_WALL=1 MODE=poweroff at every boot until 1790018000\n",
        )
        .unwrap();
        assert_eq!(
            armed,
            "dead-man switch armed: USEC=1790018000000000 WARN_WALL=1 MODE=poweroff at every boot until 1790018000"
        );
        for (status, stdout) in [(Some(1), ""), (Some(0), "something else"), (None, "USEC=1")] {
            assert_eq!(
                verdict("client1", status, stdout).unwrap_err(),
                "client1: its dead-man switch isn't armed (no shutdown scheduled)"
            );
        }
        assert_eq!(
            verdict(
                "client1",
                Some(0),
                "USEC=1790018000000000 WARN_WALL=1 MODE=poweroff "
            )
            .unwrap_err(),
            "client1: its dead-man switch wouldn't survive a reboot (no unit arms it at boot)"
        );
        assert!(verdict("client1", Some(0), "USEC=1 at every boot until \n").is_err());
    }

    /// A host that answers the check as its script says, and keeps what it
    /// was asked to run.
    struct Answering {
        answer: fn() -> Result<Completed, Stop>,
        asked: std::sync::Mutex<Vec<(String, Limit)>>,
    }

    impl Answering {
        fn new(answer: fn() -> Result<Completed, Stop>) -> Answering {
            Answering {
                answer,
                asked: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    impl Shell for Answering {
        fn work(&self) -> &RemotePath {
            static WORK: std::sync::OnceLock<RemotePath> = std::sync::OnceLock::new();

            WORK.get_or_init(|| RemotePath::new("/work").unwrap())
        }
        fn role(&self) -> &str {
            "client1"
        }
        fn address(&self) -> &str {
            "10.0.0.2"
        }
        fn public_address(&self) -> Option<&str> {
            Some("198.51.100.2")
        }
        fn destination(&self) -> &str {
            "ubuntu@198.51.100.2"
        }
        fn run(&self, script: &str, limit: Limit) -> Result<Completed, Stop> {
            self.asked.lock().unwrap().push((script.to_string(), limit));
            (self.answer)()
        }
        fn put(&self, _: &std::path::Path, _: &RemotePath, _: u32) -> Result<(), Stop> {
            unreachable!("the check copies nothing")
        }
        fn output(&self, _: &str, _: Limit) -> Result<Vec<u8>, Stop> {
            unreachable!("the check reads its answer as text")
        }
    }

    use crate::host::{Completed, RemotePath};

    fn completed(returncode: i32, stdout: &str) -> Result<Completed, Stop> {
        Ok(Completed {
            returncode,
            stdout: stdout.to_string(),
            stderr: String::new(),
        })
    }

    #[test]
    fn the_check_runs_its_script_on_the_host_and_refuses_as_the_python() {
        // An armed switch: what systemd and the unit said, for the log.
        let host = Answering::new(|| {
            completed(
                0,
                "USEC=1790018000000000 WARN_WALL=1 MODE=poweroff at every boot until 1790018000\n",
            )
        });
        assert_eq!(
            check(&host).unwrap(),
            "dead-man switch armed: USEC=1790018000000000 WARN_WALL=1 MODE=poweroff at every boot until 1790018000"
        );
        let asked = host.asked.lock().unwrap();
        assert_eq!(asked.as_slice(), [(CHECK_SCRIPT.to_string(), CHECK_LIMIT)]);
        // The Python's `timeout=300`, an int: a message says `300 s`.
        assert_eq!(CHECK_LIMIT.to_string(), "300");
        drop(asked);
        // Not armed, or not at boot: the host's error, in the Python's words.
        for (host, why) in [
            (
                Answering::new(|| completed(1, "")),
                "client1: its dead-man switch isn't armed (no shutdown scheduled)",
            ),
            (
                Answering::new(|| completed(-15, "USEC=1 at every boot until 1\n")),
                "client1: its dead-man switch isn't armed (no shutdown scheduled)",
            ),
            (
                Answering::new(|| {
                    completed(0, "USEC=1790018000000000 WARN_WALL=1 MODE=poweroff \n")
                }),
                "client1: its dead-man switch wouldn't survive a reboot (no unit arms it at boot)",
            ),
        ] {
            let Err(Stop::Host(e)) = check(&host) else {
                panic!("refused: {why}")
            };
            assert_eq!(e.message(), why);
        }
        // A host that failed the call, or an interrupt, passes as it came.
        let failed = Answering::new(|| {
            Err(HostError::new("client1: timed out after 300 s: for _ in ...").into())
        });
        assert!(
            matches!(check(&failed), Err(Stop::Host(e)) if e.message().starts_with("client1: timed out after 300 s"))
        );
        let interrupted = Answering::new(|| Err(Stop::Interrupted));
        assert!(matches!(check(&interrupted), Err(Stop::Interrupted)));
    }
}
