//! Hosts that exist already, such as the home lab or bare metal rented by
//! hand: nothing to create, pay for or destroy but each host's work
//! directory, so that a whole Round, ssh included, can be tried for free.

use std::time::Duration;

use indexmap::IndexMap;

use crate::host::{Host, Limit, RemotePath, Shell, Stop};
use crate::provider::{Access, Leftover, Provider, ProviderError, RoundId, UnixTime};
use crate::quote::join;

/// The Python's limit for a host's answer in the preflight.
const CHECK_LIMIT: Limit = Limit::secs(30);

/// The Python's limit for removing a host's work directory.
const DESTROY_LIMIT: Limit = Limit::secs(60);

/// The static provider: the hosts it was given, by role, in the order
/// given.
#[derive(Clone, Debug)]
pub struct StaticHosts {
    hosts: Vec<Host>,
    /// Whose work directory the last destroy may have left, by position.
    left: Vec<usize>,
    keep: IndexMap<(String, RemotePath), Vec<String>>,
    kept: Vec<(usize, Vec<String>)>,
    round: Option<RoundId>,
}

impl StaticHosts {
    /// The hosts; a role given twice keeps its last host, in the first's
    /// place, as the Python's dict does.
    pub fn new(hosts: impl IntoIterator<Item = Host>) -> StaticHosts {
        let mut by_role: IndexMap<String, Host> = IndexMap::new();
        for host in hosts {
            by_role.insert(host.role().to_owned(), host);
        }
        StaticHosts {
            hosts: by_role.into_values().collect(),
            left: Vec::new(),
            keep: IndexMap::new(),
            kept: Vec::new(),
            round: None,
        }
    }

    /// An unpredictable candidate, exclusively reserved when the Round
    /// creates its static hosts. The PID is kept only for caller compatibility.
    pub fn work_dir(_pid: u32) -> std::io::Result<RemotePath> {
        let suffix = crate::secrets::random_hex(16)?;
        Ok(RemotePath::new(format!("/tmp/udpstp-bench-{suffix}")).expect("hex is safe"))
    }

    /// The hosts, in the order given.
    pub fn hosts(&self) -> &[Host] {
        &self.hosts
    }

    /// The host with `role`.
    pub fn host(&self, role: &str) -> Option<&Host> {
        self.hosts.iter().find(|h| h.role() == role)
    }

    fn unique_hosts(&self) -> Vec<usize> {
        let mut unique = IndexMap::new();
        for (at, host) in self.hosts.iter().enumerate() {
            if host.has_workspace() {
                unique.insert((host.destination(), host.work()), at);
            }
        }
        unique.into_values().collect()
    }

    fn retained(&self, host: &Host) -> Vec<String> {
        let key = (host.destination().to_string(), host.work().clone());
        self.keep.get(&key).cloned().unwrap_or_default()
    }

    fn remove_by_hand(host: &Host) -> String {
        let remove = ["rm", "-rf", host.work().as_str()];
        match host.ssh_program() {
            None => join(&remove),
            Some(ssh) => {
                let ssh = ssh.to_string_lossy();
                join(&[&[ssh.as_ref(), host.destination()][..], &remove[..]].concat())
            }
        }
    }
}

impl Provider for StaticHosts {
    fn name(&self) -> &'static str {
        "static"
    }

    fn rented(&self) -> bool {
        false
    }

    fn price_per_hour(&self, _host_type: &str) -> Result<f64, ProviderError> {
        Ok(0.0)
    }

    /// Each host answers without creating or deleting any workspace.
    fn check(&mut self, _max_hours: f64, _now: UnixTime) -> Result<(), ProviderError> {
        for host in &self.hosts {
            host.outside_work("true", CHECK_LIMIT)?;
        }
        Ok(())
    }

    fn reap(&mut self, _now: UnixTime) -> Result<Vec<RoundId>, ProviderError> {
        Ok(Vec::new())
    }

    fn create(
        &mut self,
        round: &RoundId,
        _expiry: UnixTime,
        roles: &[&str],
        _access: &Access,
    ) -> Result<IndexMap<String, Box<dyn Shell>>, ProviderError> {
        if self.round.is_some() {
            return Err(ProviderError::Failed(
                "static hosts already belong to a Round".into(),
            ));
        }
        let positions: Vec<usize> = roles
            .iter()
            .map(
                |&role| match self.hosts.iter().position(|h| h.role() == role) {
                    Some(at) => Ok(at),
                    None => Err(ProviderError::Failed(format!(
                        "no host for the role {role}: give --host {role}=SSH,ADDRESS"
                    ))),
                },
            )
            .collect::<Result<_, _>>()?;
        self.round = Some(round.clone());
        let mut destinations: IndexMap<String, usize> = IndexMap::new();
        for &at in &positions {
            let destination = self.hosts[at].destination().to_owned();
            if let Some(&prior) = destinations.get(&destination) {
                let shared = self.hosts[prior].clone();
                self.hosts[at].share_workspace(&shared);
                continue;
            }
            // Different aliases may reach the same physical host. They
            // receive distinct candidates instead of competing for a name.
            if !destinations.is_empty() {
                let suffix = crate::secrets::random_hex(16)
                    .map_err(|e| ProviderError::Failed(format!("workspace name: {e}")))?;
                let work = RemotePath::new(format!("{}-{suffix}", self.hosts[at].work()))
                    .expect("hex suffix is safe");
                self.hosts[at] = self.hosts[at].clone().with_work(work);
            }
            self.hosts[at].reserve_workspace()?;
            destinations.insert(destination, at);
        }
        Ok(roles
            .iter()
            .zip(positions)
            .map(|(&role, at)| {
                (
                    role.to_owned(),
                    Box::new(self.hosts[at].clone()) as Box<dyn Shell>,
                )
            })
            .collect())
    }

    /// Removes each host's work directory, with the Round's binaries and
    /// keys. A removal that exits other than 0 failed, as one that times
    /// out does (review m3). Every host is tried, even after one failed, so
    /// that one host that doesn't answer keeps no other's keys; then the
    /// first failure, or an interrupt ahead of it, fails the attempt. The
    /// Python, since its fix on homelab-c, also tries every host and counts
    /// a failed `rm` (`hosts.py:189-206`), but returns the hosts it left as
    /// what is left rather than failing the attempt.
    fn destroy(&mut self, round: &RoundId) -> Result<Vec<Leftover>, ProviderError> {
        if self.round.as_ref().is_none_or(|owned| owned != round) {
            return Ok(Vec::new());
        }
        let mut first: Option<Stop> = None;
        self.left.clear();
        self.kept.clear();
        for at in self.unique_hosts() {
            let host = &self.hosts[at];
            let keep = self.retained(host);
            let removed = self.hosts[at].recover_workspace().and_then(|present| {
                if present {
                    self.hosts[at].remove_workspace(&keep, DESTROY_LIMIT)?;
                }
                Ok(present)
            });
            match removed {
                Err(stop) => {
                    self.left.push(at);
                    if first.is_none() || matches!(stop, Stop::Interrupted) {
                        first = Some(stop);
                    }
                }
                Ok(true) if !keep.is_empty() => self.kept.push((at, keep)),
                _ => {}
            }
        }
        match first {
            Some(stop) => Err(stop.into()),
            None => Ok(Vec::new()),
        }
    }

    fn leftovers(&mut self, _round: &RoundId) -> Result<Vec<Leftover>, ProviderError> {
        Ok(Vec::new())
    }

    fn recheck_after(&self) -> Option<Duration> {
        None
    }

    /// For each allocated host whose work directory the last destroy may
    /// have left: `SSH HOST rm -rf DIR`, `SSH` the
    /// ssh program the Round itself reaches the host with, `--ssh-dir`'s if
    /// given; or `rm -rf DIR` for this machine. So the Python prints them
    /// (`hosts.py:208-212`, homelab-c).
    fn manual_cleanup(&self, _round: &RoundId) -> Vec<String> {
        let all = self.unique_hosts();
        let left = if self.left.is_empty() {
            &all
        } else {
            &self.left
        };
        left.iter()
            .map(|&at| Self::remove_by_hand(&self.hosts[at]))
            .collect()
    }

    fn keep_files(&mut self, role: &str, names: &[&str]) -> Result<(), ProviderError> {
        for name in names {
            if name.is_empty()
                || [".", "..", ".bench-owner", "keys.toml", "udpst.keys"].contains(name)
                || name.contains(['/', '\\', '*', '?', '[', ']'])
                || name.chars().any(char::is_control)
            {
                return Err(ProviderError::Failed(
                    "retained files must be literal basenames".into(),
                ));
            }
        }
        let host = self
            .host(role)
            .ok_or_else(|| ProviderError::Failed(format!("no host for the role {role}")))?;
        let key = (host.destination().to_string(), host.work().clone());
        self.keep
            .insert(key, names.iter().map(|s| (*s).to_string()).collect());
        Ok(())
    }

    fn kept_lines(&self) -> Vec<String> {
        self.kept
            .iter()
            .map(|(at, keep)| {
                let host = &self.hosts[*at];
                let files = keep
                    .iter()
                    .map(|name| format!("{}/{name}", host.work()))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "kept on purpose: {files} on {}; delete it once perf report has read it: {}",
                    host.destination(),
                    Self::remove_by_hand(host)
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interrupt::Flag;
    use crate::process::Runner;

    fn hosts() -> StaticHosts {
        let work = StaticHosts::work_dir(4242).unwrap();
        let server = Host::ssh(
            "server",
            "bench@testhost",
            "192.168.178.22",
            work.clone(),
            Runner::new(Flag::new()),
        )
        .unwrap();
        let client = Host::local(
            "client",
            "192.168.178.40",
            work.clone(),
            Runner::new(Flag::new()),
        );
        let again = Host::local("server", "10.0.0.1", work, Runner::new(Flag::new()));
        StaticHosts::new([server, client, again])
    }

    #[test]
    fn existing_hosts_cost_nothing_and_leave_nothing_to_list() {
        let mut hosts = hosts();
        assert_eq!(hosts.name(), "static");
        assert!(!hosts.rented());
        assert_eq!(hosts.price_per_hour("c6in.large").unwrap(), 0.0);
        assert!(hosts.reap(0.0).unwrap().is_empty());
        let round = RoundId::from("r1");
        assert!(hosts.leftovers(&round).unwrap().is_empty());
        assert_eq!(hosts.recheck_after(), None);
        let first = StaticHosts::work_dir(4242).unwrap();
        let second = StaticHosts::work_dir(4242).unwrap();
        assert_ne!(first, second);
        assert_eq!(first.as_str().len(), "/tmp/udpstp-bench-".len() + 32);
    }

    #[test]
    fn retention_refuses_paths_and_patterns() {
        let mut hosts = hosts();
        hosts.keep_files("server", &["udpstp", "udpst"]).unwrap();
        for bad in [
            "",
            ".",
            "..",
            "../key",
            "sub/file",
            "sub\\file",
            "*",
            "udpst?",
            "[a]",
            "a\n",
            "keys.toml",
            "udpst.keys",
        ] {
            assert!(hosts.keep_files("server", &[bad]).is_err(), "{bad:?}");
        }
        assert!(hosts.keep_files("absent", &["udpstp"]).is_err());
    }

    /// Review m3: what deletes a work directory by hand, over ssh or here.
    #[test]
    fn the_commands_by_hand_remove_the_work_directories() {
        let work = RemotePath::new("/tmp/udpstp-bench-4242").unwrap();
        let flag = Flag::new();
        let server = Host::ssh(
            "server",
            "bench@testhost",
            "192.168.178.22",
            work.clone(),
            Runner::new(flag.clone()),
        )
        .unwrap();
        let client = Host::local("client", "192.168.178.40", work, Runner::new(flag));
        assert_eq!(
            [
                StaticHosts::remove_by_hand(&server),
                StaticHosts::remove_by_hand(&client)
            ],
            [
                "ssh bench@testhost rm -rf /tmp/udpstp-bench-4242",
                "rm -rf /tmp/udpstp-bench-4242"
            ]
        );
    }

    /// A review of stage B: the command by hand runs the ssh the Round ran,
    /// as the Python's does (`hosts.py:212`, homelab-c). In the home lab,
    /// only the ssh that `--ssh-dir` names knows the Server host's name and
    /// holds its key, so PATH's `ssh` would fail there.
    #[test]
    fn a_command_by_hand_runs_the_rounds_own_ssh() {
        let server = Host::ssh(
            "server",
            "testhost-wired",
            "192.168.178.22",
            RemotePath::new("/tmp/udpstp-bench-4242").unwrap(),
            Runner::new(Flag::new()),
        )
        .unwrap()
        .with_ssh_dir("/opt/win-ssh");
        #[cfg(unix)]
        let ssh = "/opt/win-ssh/ssh";
        // Windows joins with `\`, which shlex.quote quotes.
        #[cfg(windows)]
        let ssh = r"'/opt/win-ssh\ssh'";
        assert_eq!(
            StaticHosts::remove_by_hand(&server),
            format!("{ssh} testhost-wired rm -rf /tmp/udpstp-bench-4242")
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn create_hands_back_the_roles_asked_for_in_their_order() {
        let mut hosts = hosts();
        // The second "server" replaced the first, in its place.
        let roles: Vec<&str> = hosts.hosts().iter().map(Host::role).collect();
        assert_eq!(roles, ["server", "client"]);
        assert_eq!(hosts.host("server").unwrap().address(), "10.0.0.1");
        let round = RoundId::from("r1");
        let made = hosts
            .create(&round, 0.0, &["client", "server"], &Access::default())
            .unwrap();
        let made: Vec<(&str, &str)> = made
            .iter()
            .map(|(r, h)| (r.as_str(), h.destination()))
            .collect();
        assert_eq!(made, [("client", "local"), ("server", "local")]);
        hosts.destroy(&round).unwrap();
        let Err(ProviderError::Failed(why)) =
            self::hosts().create(&round, 0.0, &["client2"], &Access::default())
        else {
            panic!("a missing role")
        };
        assert!(why.contains("--host client2=SSH,ADDRESS"), "{why}");
    }
}
