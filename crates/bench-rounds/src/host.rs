//! The hosts of a Round: run a script, copy a file in, fetch a directory
//! or a file back; and one call on several hosts at once
//!, as the Python's
//! `hosts.Host` and `scale.in_parallel` do.
//!
//! A host is reached over ssh, or is this machine. Over ssh it is always
//! one remote command, `ssh -T ... HOST "bash -s"`, with the script on
//! standard input: the host's login shell parses only `bash -s`, so a
//! login shell that isn't POSIX, such as fish, never sees the script.
//! Locally the script runs as `bash -c SCRIPT`, with standard input null.
//! Every script runs in the host's work directory. Static reservations
//! are checked first and never recreated; rented hosts make theirs first.
//!
//! A call ends in one of three ways: it finished, the host failed it
//! ([`Stop::Host`], whose message is redacted), or the Round was
//! interrupted ([`Stop::Interrupted`]). The last two are separate so that
//! no site that turns a host's error into "the tooling failed" can swallow
//! an interrupt: such a site matches `Stop::Host` only.
//!
//! Garbage from a host is the host's failure, and so voids only the Round
//! Run it came in, never the Round (the design's §6, review m2): output
//! that isn't UTF-8 where text is wanted, on either stream, an archive that
//! can't be unpacked, and output past the runner's cap. The Python, since
//! its fix on homelab-c, does the same with the first two, and has no cap.

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

use crate::interrupt::Interrupted;
use crate::num::{self, Num};
use crate::process::{Finished, RunError, Runner, lossy_text, text};
use crate::py;
use crate::quote::{join, quote};
use crate::redact::redact;
use crate::unpack::unpack;

/// The Python's default limit for a command (`Host.run`'s `timeout`).
pub const RUN_LIMIT: Limit = Limit::secs(120);

/// The Python's limit for a copy and for a fetch (`scp`'s and
/// `Host.output`'s).
pub const COPY_LIMIT: Limit = Limit::secs(300);

/// What a Round records of each host once it is prepared, into its
/// `env-<role>.txt`: the kernel, the CPU, the clock source and its
/// synchronisation, the socket-buffer limits, and the NIC the host routes
/// through, with its driver and offloads (`round.py`'s `ENV_SCRIPT`).
pub const ENVIRONMENT_SCRIPT: &str = r#"
uname -a; echo; lscpu 2>/dev/null | head -20; echo
cat /sys/devices/system/clocksource/clocksource0/current_clocksource 2>/dev/null
chronyc tracking 2>/dev/null | head -8; echo
sysctl net.core.rmem_max net.core.wmem_max 2>/dev/null
dev=$(ip -o route get 1.1.1.1 2>/dev/null | sed -n 's/.* dev \([^ ]*\).*/\1/p')
echo "dev $dev"; ip link show "$dev" 2>/dev/null
ethtool -i "$dev" 2>/dev/null; ethtool -k "$dev" 2>/dev/null | grep -E 'segmentation|receive-offload|scatter'
"#;

/// What would load a fresh rented host during the Round, stopped: apt's
/// daily timers and unattended-upgrades (which on first boot often hold
/// dpkg's lock, too), and snap's refreshes, held for a day. Its output
/// goes into the host's environment record (`round.py`'s `QUIET_SCRIPT`).
pub const QUIET_SCRIPT: &str = r#"
sudo -n systemctl stop apt-daily.timer apt-daily-upgrade.timer 2>/dev/null
sudo -n timeout 150 systemctl stop unattended-upgrades.service apt-daily.service apt-daily-upgrade.service 2>/dev/null
sudo -n snap refresh --hold=24h >/dev/null 2>&1
echo; echo "apt timers, unattended-upgrades: $(systemctl is-active apt-daily.timer apt-daily-upgrade.timer unattended-upgrades.service 2>/dev/null | tr '\n' ' ')"
echo "snap: $(snap refresh --time 2>/dev/null | grep -i '^hold' || echo 'no hold')"
"#;

/// The Python's limit for [`QUIET_SCRIPT`].
pub const QUIET_LIMIT: Limit = Limit::secs(240);

/// Why a host call didn't finish: a host's error, or an interrupt, never
/// one disguised as the other.
#[derive(Debug)]
pub enum Stop {
    Host(HostError),
    Interrupted,
}

/// A host that failed a call: the message says which host and why, with
/// anything that could be a key redacted, and goes to the Round's log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostError(String);

impl HostError {
    pub fn new(message: impl Into<String>) -> HostError {
        HostError(message.into())
    }

    pub fn message(&self) -> &str {
        &self.0
    }

    /// A command that exited with `returncode`, other than 0, as the Python
    /// says it: `ROLE: exit N: COMMAND` and its standard error, `stderr`,
    /// on the next line, both redacted.
    pub fn exited(role: &str, script: &str, returncode: i32, stderr: &str) -> HostError {
        HostError(format!(
            "{role}: exit {returncode}: {}\n{}",
            redact(script),
            redact(py::strip(stderr))
        ))
    }

    /// A command whose output isn't the text the Round reads.
    pub fn not_text(role: &str, script: &str, stream: &str) -> HostError {
        HostError(format!(
            "{role}: its {stream} isn't UTF-8: {}",
            redact(script)
        ))
    }

    /// A command that ran past its limit and was killed.
    pub fn timed_out(role: &str, script: &str, limit: Limit) -> HostError {
        HostError(format!(
            "{role}: timed out after {limit} s: {}",
            redact(script)
        ))
    }

    /// A command whose output passed the runner's cap, and was killed: a
    /// difference from the Python, which holds whatever comes.
    pub fn too_much_output(role: &str, script: &str, cap: u64) -> HostError {
        HostError(format!(
            "{role}: more than {} of output: {}",
            bytes(cap),
            redact(script)
        ))
    }
}

/// A number of bytes as a message says it: `1 GiB`, `64 KiB`, `1000 bytes`.
fn bytes(n: u64) -> String {
    match n {
        n if n != 0 && n % (1 << 30) == 0 => format!("{} GiB", n >> 30),
        n if n != 0 && n % (1 << 20) == 0 => format!("{} MiB", n >> 20),
        n if n != 0 && n % (1 << 10) == 0 => format!("{} KiB", n >> 10),
        n => format!("{n} bytes"),
    }
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HostError {}

impl fmt::Display for Stop {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Stop::Host(e) => e.fmt(f),
            Stop::Interrupted => Interrupted.fmt(f),
        }
    }
}

impl std::error::Error for Stop {}

impl From<HostError> for Stop {
    fn from(e: HostError) -> Stop {
        Stop::Host(e)
    }
}

impl From<Interrupted> for Stop {
    fn from(_: Interrupted) -> Stop {
        Stop::Interrupted
    }
}

/// A command that ran to its end on a host, its output as the Python's
/// `subprocess.run(..., text=True)` gives it: UTF-8, each `\r\n` and lone
/// `\r` made `\n`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completed {
    /// Python's `returncode`: the exit status, or minus the signal that
    /// ended the command.
    pub returncode: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Completed {
    /// Whether it exited with 0.
    pub fn success(&self) -> bool {
        self.returncode == 0
    }

    /// What `script` gave, as text: output that isn't UTF-8, on either
    /// stream, is the host's error, as in the Python since its fix on
    /// homelab-c, whose message says where the bytes went wrong rather
    /// than which stream.
    pub fn from_finished(role: &str, script: &str, done: Finished) -> Result<Completed, HostError> {
        let stdout = text(&done.stdout).map_err(|_| HostError::not_text(role, script, "output"))?;
        let stderr =
            text(&done.stderr).map_err(|_| HostError::not_text(role, script, "error output"))?;
        Ok(Completed {
            returncode: done.returncode,
            stdout,
            stderr,
        })
    }
}

/// A time limit as the Python gives one: an int or a float of seconds.
/// A message prints it as Python prints the `timeout` it was given, `120`
/// for an int and `101.0` for a float, since the message reaches a run's
/// `meta.json` and its report (review m6).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Limit(Num);

impl Limit {
    /// `seconds`, an int.
    pub const fn secs(seconds: u32) -> Limit {
        Limit(Num::Int(seconds as i128))
    }

    /// A limit of `seconds`, of the Python's kind; `None` for one no clock
    /// could wait: negative, not a number, or beyond a `Duration`.
    pub fn new(seconds: Num) -> Option<Limit> {
        let fits = match seconds {
            Num::Int(i) => u64::try_from(i).is_ok(),
            Num::Float(f) => f >= 0.0 && Duration::try_from_secs_f64(f).is_ok(),
        };
        fits.then_some(Limit(seconds))
    }

    /// The seconds, of the Python's kind.
    pub fn seconds(self) -> Num {
        self.0
    }

    /// How long that is.
    pub fn duration(self) -> Duration {
        match self.0 {
            Num::Int(i) => Duration::from_secs(i as u64),
            Num::Float(f) => Duration::from_secs_f64(f),
        }
    }
}

/// `str(seconds)`: an int as it is, a float as Python's `str`, which is its
/// `repr`, writes it ([`num::repr`]).
impl fmt::Display for Limit {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.0 {
            Num::Int(i) => write!(f, "{i}"),
            Num::Float(x) => f.write_str(&num::repr(x)),
        }
    }
}

/// A host of the Round, as a Round uses it. Implemented by [`Host`], and
/// by the scripted hosts of tests.
pub trait Shell: Sync {
    /// Its role in the Round: `server`, `client`, `client1`, `home`, ...
    fn role(&self) -> &str;

    /// Where the other hosts reach it for Tests.
    fn address(&self) -> &str;

    /// Where the machine running the Round reaches it, if not
    /// [`Shell::address`].
    fn public_address(&self) -> Option<&str>;

    /// Its ssh destination (`user@host`, or an ssh config alias), or
    /// `local` for this machine.
    fn destination(&self) -> &str;

    /// The directory every script runs in, also the identity of retained
    /// files when several roles share a host and directory.
    fn work(&self) -> &RemotePath;

    /// Runs `script` with bash in the work directory: completed, whatever
    /// its exit status, as the Python's `run(..., check=False)`, its output
    /// as text ([`Completed`]).
    fn run(&self, script: &str, limit: Limit) -> Result<Completed, Stop>;

    /// [`Shell::run`], with an exit status other than 0 a host error: the
    /// Python's `run(...)`, whose `check` is true unless it says otherwise.
    fn run_checked(&self, script: &str, limit: Limit) -> Result<Completed, Stop> {
        let done = self.run(script, limit)?;
        if done.success() {
            Ok(done)
        } else {
            Err(HostError::exited(self.role(), script, done.returncode, &done.stderr).into())
        }
    }

    /// Copies the local file `local` into the work directory as `name`,
    /// then sets its `mode`.
    fn put(&self, local: &Path, name: &RemotePath, mode: u32) -> Result<(), Stop>;

    /// Runs `script` as [`Shell::run`] does: its standard output, or a
    /// host error where it exits other than 0.
    fn output(&self, script: &str, limit: Limit) -> Result<Vec<u8>, Stop>;

    /// Copies the directory `remote`, in the work directory, into the
    /// local directory `local` through tar, taking regular files and
    /// directories only ([`unpack`]). tar's exit 1, a file that changed as
    /// it was read, is fine: a sampler may still add its last line.
    fn fetch_dir(&self, remote: &RemotePath, local: &Path) -> Result<(), Stop> {
        let script = format!(
            "tar -czf - -C {} .; s=$?; [ $s -le 1 ]",
            quote(remote.as_str())
        );
        let data = self.output(&script, COPY_LIMIT)?;
        let failed = |e: &dyn fmt::Display| {
            HostError::new(format!("{}: fetching {remote}: {e}", self.role()))
        };
        fs::create_dir_all(local).map_err(|e| failed(&e))?;
        unpack(&data, local).map_err(|e| failed(&e))?;
        Ok(())
    }

    /// Copies the file `remote`, in the work directory, to the local file
    /// `local`.
    fn fetch_file(&self, remote: &RemotePath, local: &Path) -> Result<(), Stop> {
        let data = self.output(&format!("cat {}", quote(remote.as_str())), COPY_LIMIT)?;
        fs::write(local, data).map_err(|e| {
            HostError::new(format!(
                "{}: fetching {remote} into {}: {e}",
                self.role(),
                local.display()
            ))
        })?;
        Ok(())
    }
}

/// A path on a host, relative to its work directory or absolute: joined
/// with `/`, whatever this machine uses.
///
/// Only shlex's safe characters (`%+,-./0-9:=@A-Z_a-z`), and not starting
/// with `-`: such a path means the same quoted or not, in a script and in
/// `scp`'s target, and is never taken for an option. Every path a Round
/// names is one: a work directory, a label, a role, a file name.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RemotePath(String);

/// A path [`RemotePath`] refuses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotRemotePath(String);

impl fmt::Display for NotRemotePath {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{:?} isn't a path a Round names on a host: only the characters shlex leaves unquoted, and not starting with -",
            self.0
        )
    }
}

impl std::error::Error for NotRemotePath {}

impl RemotePath {
    pub fn new(path: impl Into<String>) -> Result<RemotePath, NotRemotePath> {
        let path = path.into();
        let safe = !path.is_empty() && !path.starts_with('-') && quote(&path) == path.as_str();
        if safe {
            Ok(RemotePath(path))
        } else {
            Err(NotRemotePath(path))
        }
    }

    /// `self/name`.
    pub fn join(&self, name: &str) -> Result<RemotePath, NotRemotePath> {
        RemotePath::new(format!("{}/{name}", self.0))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RemotePath {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A host over ssh, or this machine: the Python's `hosts.Host`.
#[derive(Clone, Debug)]
pub struct Host {
    role: String,
    via: Via,
    address: String,
    public_address: Option<String>,
    work: RemotePath,
    runner: Runner,
    workspace: Option<Workspace>,
}

#[derive(Clone, Debug)]
struct Workspace {
    proof: String,
    identity: Option<String>,
}

const WORKSPACE_SCRIPT: &str = include_str!("../../../scripts/bench/workspace.sh");

#[derive(Clone, Debug)]
enum Via {
    Local,
    Ssh(Ssh),
}

#[derive(Clone, Debug)]
struct Ssh {
    destination: String,
    /// `--ssh-dir`: where `ssh` and `scp` are, if not on PATH.
    dir: Option<PathBuf>,
    /// A fresh cloud host's key file: its key is new, and this Round's
    /// alone.
    known_hosts: Option<PathBuf>,
    identity: Option<PathBuf>,
}

impl Ssh {
    fn program(&self, name: &str) -> OsString {
        match &self.dir {
            Some(dir) => dir.join(name).into_os_string(),
            None => name.into(),
        }
    }

    /// `ssh` or `scp` and the options both take, as the Python's
    /// `_ssh_base`.
    fn base(&self, program: &str) -> Vec<OsString> {
        let mut argv: Vec<OsString> = vec![self.program(program)];
        for option in [
            "BatchMode=yes",
            "ConnectTimeout=10",
            "ServerAliveInterval=15",
        ] {
            argv.extend(["-o".into(), option.into()]);
        }
        if let Some(file) = &self.known_hosts {
            let mut known = OsString::from("UserKnownHostsFile=");
            known.push(file);
            argv.extend([
                "-o".into(),
                "StrictHostKeyChecking=accept-new".into(),
                "-o".into(),
                known,
            ]);
        }
        if let Some(file) = &self.identity {
            argv.extend([
                "-i".into(),
                file.into(),
                "-o".into(),
                "IdentitiesOnly=yes".into(),
            ]);
        }
        argv
    }

    /// The one remote command: `bash -s`, reading the script from standard
    /// input.
    fn shell_argv(&self) -> Vec<OsString> {
        let mut argv = self.base("ssh");
        argv.extend([
            "-T".into(),
            self.destination.as_str().into(),
            "bash -s".into(),
        ]);
        argv
    }

    fn copy_argv(&self, local: &Path, target: &str) -> Vec<OsString> {
        let mut argv = self.base("scp");
        argv.extend([
            "-q".into(),
            local.into(),
            format!("{}:{target}", self.destination).into(),
        ]);
        argv
    }
}

fn command(argv: &[OsString]) -> Command {
    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]);
    command
}

impl Host {
    /// This machine, as a host of the Round.
    pub fn local(
        role: impl Into<String>,
        address: impl Into<String>,
        work: RemotePath,
        runner: Runner,
    ) -> Host {
        Host {
            role: role.into(),
            via: Via::Local,
            address: address.into(),
            public_address: None,
            work,
            runner,
            workspace: None,
        }
    }

    /// A host reached over ssh at `destination`, which must not start with
    /// `-`: ssh would take it for an option.
    pub fn ssh(
        role: impl Into<String>,
        destination: impl Into<String>,
        address: impl Into<String>,
        work: RemotePath,
        runner: Runner,
    ) -> Result<Host, HostError> {
        let role = role.into();
        let destination = destination.into();
        if destination.is_empty() || destination.starts_with('-') {
            return Err(HostError::new(format!(
                "{role}: {destination:?} isn't an ssh destination"
            )));
        }
        Ok(Host {
            role,
            via: Via::Ssh(Ssh {
                destination,
                dir: None,
                known_hosts: None,
                identity: None,
            }),
            address: address.into(),
            public_address: None,
            work,
            runner,
            workspace: None,
        })
    }

    /// Where the machine running the Round reaches it, if not its address.
    pub fn with_public_address(mut self, address: impl Into<String>) -> Host {
        self.public_address = Some(address.into());
        self
    }

    /// Over ssh: `ssh` and `scp` from `dir`, not from PATH (`--ssh-dir`).
    pub fn with_ssh_dir(mut self, dir: impl Into<PathBuf>) -> Host {
        if let Via::Ssh(ssh) = &mut self.via {
            ssh.dir = Some(dir.into());
        }
        self
    }

    /// Over ssh: the host's key goes into `file`, taken on first sight.
    pub fn with_known_hosts(mut self, file: impl Into<PathBuf>) -> Host {
        if let Via::Ssh(ssh) = &mut self.via {
            ssh.known_hosts = Some(file.into());
        }
        self
    }

    /// Over ssh: the key to sign in with, and no other.
    pub fn with_identity(mut self, file: impl Into<PathBuf>) -> Host {
        if let Via::Ssh(ssh) = &mut self.via {
            ssh.identity = Some(file.into());
        }
        self
    }

    pub fn work(&self) -> &RemotePath {
        &self.work
    }

    /// Reserve exactly this candidate, refusing any existing entry. The
    /// short allocation cannot be cancelled between mkdir and recording
    /// its identity; a pending proof permits recovery after transport loss.
    pub fn reserve_workspace(&mut self) -> Result<(), Stop> {
        if self.workspace.is_some() {
            return Err(
                HostError::new(format!("{}: workspace already reserved", self.role)).into(),
            );
        }
        if self.runner.flag().stops() {
            return Err(Stop::Interrupted);
        }
        let proof = crate::secrets::random_hex(16)
            .map_err(|e| HostError::new(format!("{}: workspace proof: {e}", self.role)))?;
        self.workspace = Some(Workspace {
            proof: proof.clone(),
            identity: None,
        });
        let script = format!(
            "{WORKSPACE_SCRIPT}\nbench_allocate {} {}",
            quote(self.work.as_str()),
            quote(&proof)
        );
        let runner = Runner::new(crate::interrupt::Flag::new());
        let done = self.raw_call(
            &script,
            "allocate private workspace",
            Limit::secs(30),
            &runner,
        )?;
        let done = Completed::from_finished(&self.role, "allocate private workspace", done)?;
        if !done.success() {
            // A refused candidate is not ours and must never be removed.
            if [72, 73].contains(&done.returncode) {
                self.workspace = None;
            }
            return Err(HostError::exited(
                &self.role,
                "allocate private workspace",
                done.returncode,
                &done.stderr,
            )
            .into());
        }
        self.record_workspace_identity(&done.stdout)?;
        if self.runner.flag().stops() {
            return Err(Stop::Interrupted);
        }
        Ok(())
    }

    fn record_workspace_identity(&mut self, output: &str) -> Result<(), Stop> {
        let identity = output.trim();
        let valid = identity.split_once(':').is_some_and(|(device, inode)| {
            [device, inode]
                .iter()
                .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
        });
        if !valid {
            return Err(
                HostError::new(format!("{}: invalid workspace identity", self.role)).into(),
            );
        }
        self.workspace
            .as_mut()
            .expect("allocation recorded")
            .identity = Some(identity.to_owned());
        Ok(())
    }

    /// Resolve a lost allocation response before removing its receipt.
    /// If this read fails, no destructive cleanup has started.
    pub(crate) fn recover_workspace(&mut self) -> Result<bool, Stop> {
        if self
            .workspace
            .as_ref()
            .is_some_and(|owned| owned.identity.is_some())
        {
            return Ok(true);
        }
        let work = quote(self.work.as_str());
        let script = format!(
            "if [[ ! -e {work} && ! -L {work} ]]; then echo absent; exit 0; fi\n{}stat -c '%d:%i' -- {work}",
            self.workspace_guard()
        );
        let done = self.outside_work(&script, Limit::secs(30))?;
        if done.stdout.trim() == "absent" {
            self.workspace = None;
            return Ok(false);
        }
        self.record_workspace_identity(&done.stdout)?;
        Ok(true)
    }

    pub(crate) fn has_workspace(&self) -> bool {
        self.workspace.is_some()
    }

    pub(crate) fn with_work(mut self, work: RemotePath) -> Host {
        self.work = work;
        self
    }

    pub(crate) fn share_workspace(&mut self, host: &Host) {
        self.work = host.work.clone();
        self.workspace = host.workspace.clone();
    }

    fn workspace_guard(&self) -> String {
        let owned = self.workspace.as_ref().expect("owned workspace");
        format!(
            "{WORKSPACE_SCRIPT}\nbench_guard {} {} {} || {{ echo 'private workspace ownership check failed' >&2; exit 1; }}\n",
            quote(self.work.as_str()),
            quote(&owned.proof),
            quote(owned.identity.as_deref().unwrap_or(""))
        )
    }

    /// Transport without making or entering any workspace, for preflight
    /// and guarded allocation/removal only.
    pub(crate) fn outside_work(&self, script: &str, limit: Limit) -> Result<Completed, Stop> {
        let done = self.raw_call(script, script, limit, &self.runner)?;
        let done = Completed::from_finished(&self.role, script, done)?;
        if !done.success() {
            return Err(
                HostError::exited(&self.role, script, done.returncode, &done.stderr).into(),
            );
        }
        Ok(done)
    }

    pub(crate) fn remove_workspace(&self, keep: &[String], limit: Limit) -> Result<(), Stop> {
        let owned = self.workspace.as_ref().expect("owned workspace");
        let args = [
            self.work.as_str(),
            &owned.proof,
            owned.identity.as_deref().unwrap_or(""),
        ];
        let kept = keep.iter().map(String::as_str).collect::<Vec<_>>();
        let guarded = format!(
            "{WORKSPACE_SCRIPT}\nbench_remove {} {}",
            join(&args),
            join(&kept)
        );
        let script = "remove private workspace";
        let done = self.raw_call(&guarded, script, limit, &self.runner)?;
        let done = Completed::from_finished(&self.role, script, done)?;
        if !done.success() {
            return Err(
                HostError::exited(&self.role, script, done.returncode, &done.stderr).into(),
            );
        }
        Ok(())
    }

    pub fn is_local(&self) -> bool {
        matches!(self.via, Via::Local)
    }

    /// Over ssh, the `ssh` program the Round reaches the host with:
    /// `--ssh-dir`'s, or PATH's. `None` for this machine.
    pub fn ssh_program(&self) -> Option<OsString> {
        match &self.via {
            Via::Ssh(ssh) => Some(ssh.program("ssh")),
            Via::Local => None,
        }
    }

    /// `script`, run in the checked reservation, or the rented host's
    /// work directory, which it makes first.
    fn in_work(&self, script: &str) -> String {
        let work = quote(self.work.as_str());
        if self.workspace.is_some() {
            format!("{}cd -- {work} || exit 1\n{script}", self.workspace_guard())
        } else {
            format!("mkdir -p {work} && cd {work} && {script}")
        }
    }

    /// What runs `script` (with the work directory's prefix): its argv, and
    /// its standard input.
    fn shell(&self, script: String) -> (Vec<OsString>, Option<String>) {
        match &self.via {
            Via::Local => (vec!["bash".into(), "-c".into(), script.into()], None),
            Via::Ssh(ssh) => (ssh.shell_argv(), Some(script)),
        }
    }

    /// Runs `script` in the work directory; `script` alone goes into
    /// messages.
    fn call(&self, script: &str, limit: Limit) -> Result<Finished, Stop> {
        self.raw_call(&self.in_work(script), script, limit, &self.runner)
    }

    fn raw_call(
        &self,
        script: &str,
        description: &str,
        limit: Limit,
        runner: &Runner,
    ) -> Result<Finished, Stop> {
        let (argv, stdin) = self.shell(script.to_owned());
        runner
            .run(
                &mut command(&argv),
                stdin.as_deref().map(str::as_bytes),
                limit.duration(),
            )
            .map_err(|e| {
                self.stopped(
                    e,
                    &argv[0],
                    || HostError::timed_out(&self.role, description, limit),
                    |cap| HostError::too_much_output(&self.role, description, cap),
                )
            })
    }

    /// A run's error as a host call's.
    fn stopped(
        &self,
        error: RunError,
        program: &OsString,
        timed_out: impl FnOnce() -> HostError,
        too_much: impl FnOnce(u64) -> HostError,
    ) -> Stop {
        let program = Path::new(program).display();
        match error {
            RunError::Interrupted => Stop::Interrupted,
            RunError::TimedOut => timed_out().into(),
            RunError::TooMuchOutput { cap } => too_much(cap).into(),
            RunError::Spawn(e) => {
                HostError::new(format!("{}: couldn't start {program}: {e}", self.role)).into()
            }
            RunError::Wait(e) => {
                HostError::new(format!("{}: waiting for {program}: {e}", self.role)).into()
            }
        }
    }
}

impl Shell for Host {
    fn work(&self) -> &RemotePath {
        &self.work
    }
    fn role(&self) -> &str {
        &self.role
    }

    fn address(&self) -> &str {
        &self.address
    }

    fn public_address(&self) -> Option<&str> {
        self.public_address.as_deref()
    }

    fn destination(&self) -> &str {
        match &self.via {
            Via::Local => "local",
            Via::Ssh(ssh) => &ssh.destination,
        }
    }

    fn run(&self, script: &str, limit: Limit) -> Result<Completed, Stop> {
        let done = self.call(script, limit)?;
        Ok(Completed::from_finished(&self.role, script, done)?)
    }

    /// Bytes, as the Python's `output` takes them; standard error, which
    /// only a failure's message shows, with what isn't UTF-8 replaced, as
    /// the Python's `decode(errors="replace")` does.
    fn output(&self, script: &str, limit: Limit) -> Result<Vec<u8>, Stop> {
        let done = self.call(script, limit)?;
        if done.success() {
            Ok(done.stdout)
        } else {
            let stderr = lossy_text(&done.stderr);
            Err(HostError::exited(&self.role, script, done.returncode, &stderr).into())
        }
    }

    fn put(&self, local: &Path, name: &RemotePath, mode: u32) -> Result<(), Stop> {
        let failed =
            |why: String| HostError::new(format!("{}: copying {name} failed: {why}", self.role));
        match &self.via {
            Via::Local => {
                let work = Path::new(self.work.as_str());
                if self.workspace.is_some() {
                    self.run_checked("true", RUN_LIMIT)?;
                } else {
                    fs::create_dir_all(work).map_err(|e| failed(e.to_string()))?;
                }
                fs::copy(local, work.join(name.as_str())).map_err(|e| failed(e.to_string()))?;
            }
            Via::Ssh(ssh) => {
                // The work directory, made.
                self.run_checked("true", RUN_LIMIT)?;
                let argv = ssh.copy_argv(local, &format!("{}/{name}", self.work));
                let done = self
                    .runner
                    .run(&mut command(&argv), None, COPY_LIMIT.duration())
                    .map_err(|e| {
                        self.stopped(
                            e,
                            &argv[0],
                            || failed(format!("timed out after {COPY_LIMIT} s")),
                            |cap| failed(format!("more than {} of output", bytes(cap))),
                        )
                    })?;
                if !done.success() {
                    return Err(failed(redact(py::strip(&lossy_text(&done.stderr)))).into());
                }
            }
        }
        let mode = format!("{mode:04o}");
        self.run_checked(&join(&["chmod", mode.as_str(), name.as_str()]), RUN_LIMIT)?;
        Ok(())
    }
}

/// Runs each call at once, each in a thread of its own, and waits until
/// all have ended: their results in order. Then `Interrupted` if any call
/// was interrupted, else the first error by position. So an interrupt is
/// never lost behind an earlier host's error, as it can be in the Python's
/// `in_parallel`; and since a raised flag stops every command in flight,
/// waiting for all is quick. A call that panicked panics again here, once
/// all have ended.
pub fn each<T, F>(calls: impl IntoIterator<Item = F>) -> Result<Vec<T>, Stop>
where
    F: FnOnce() -> Result<T, Stop> + Send,
    T: Send,
{
    let ended: Vec<thread::Result<Result<T, Stop>>> = thread::scope(|scope| {
        let started: Vec<_> = calls
            .into_iter()
            .map(|call| {
                thread::Builder::new()
                    .name("bench-rounds-host".into())
                    .spawn_scoped(scope, call)
            })
            .collect();
        started
            .into_iter()
            .map(|thread| match thread {
                Ok(thread) => thread.join(),
                Err(e) => Ok(Err(HostError::new(format!(
                    "couldn't start a thread for a host: {e}"
                ))
                .into())),
            })
            .collect()
    });
    let mut results = Vec::with_capacity(ended.len());
    let mut panicked = None;
    for outcome in ended {
        match outcome {
            Ok(result) => results.push(result),
            Err(panic) => {
                panicked.get_or_insert(panic);
            }
        }
    }
    if let Some(panic) = panicked {
        std::panic::resume_unwind(panic);
    }
    if results.iter().any(|r| matches!(r, Err(Stop::Interrupted))) {
        return Err(Stop::Interrupted);
    }
    results.into_iter().collect()
}

/// Stage a prebuilt host executable without duplicating its build tree.
pub fn stage_executable(source: &std::path::Path, target: &std::path::Path) -> std::io::Result<()> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if std::fs::hard_link(source, target).is_err() {
        std::fs::copy(source, target)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interrupt::Flag;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    fn work() -> RemotePath {
        RemotePath::new("/tmp/udpstp-bench-4242").unwrap()
    }

    fn ssh_host() -> Host {
        Host::ssh(
            "server",
            "bench@testhost",
            "192.168.178.22",
            work(),
            Runner::new(Flag::new()),
        )
        .unwrap()
    }

    fn strings(argv: &[OsString]) -> Vec<String> {
        argv.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn ssh_of(host: &Host) -> &Ssh {
        match &host.via {
            Via::Ssh(ssh) => ssh,
            Via::Local => panic!("a local host"),
        }
    }

    #[test]
    fn ssh_runs_bash_s_as_the_python_does() {
        let host = ssh_host();
        assert_eq!(
            strings(&ssh_of(&host).shell_argv()),
            [
                "ssh",
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=10",
                "-o",
                "ServerAliveInterval=15",
                "-T",
                "bench@testhost",
                "bash -s"
            ]
        );
        let rented = ssh_host()
            .with_known_hosts("/k/known_hosts")
            .with_identity("/k/id_ed25519")
            .with_ssh_dir("/opt/ssh");
        let dir = Path::new("/opt/ssh");
        assert_eq!(
            strings(&ssh_of(&rented).shell_argv()),
            [
                dir.join("ssh").to_string_lossy().as_ref(),
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=10",
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "StrictHostKeyChecking=accept-new",
                "-o",
                "UserKnownHostsFile=/k/known_hosts",
                "-i",
                "/k/id_ed25519",
                "-o",
                "IdentitiesOnly=yes",
                "-T",
                "bench@testhost",
                "bash -s"
            ]
        );
        assert_eq!(
            strings(&ssh_of(&rented).copy_argv(Path::new("/b/udpstp"), "/w/udpstp")),
            [
                dir.join("scp").to_string_lossy().as_ref(),
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=10",
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "StrictHostKeyChecking=accept-new",
                "-o",
                "UserKnownHostsFile=/k/known_hosts",
                "-i",
                "/k/id_ed25519",
                "-o",
                "IdentitiesOnly=yes",
                "-q",
                "/b/udpstp",
                "bench@testhost:/w/udpstp"
            ]
        );
    }

    /// Nothing but `bash -s` reaches the remote side on the command line
    /// (the design's §3.4): no option that runs a command, the destination
    /// then `bash -s` last, and the script only on standard input.
    #[test]
    fn no_argv_carries_anything_else_to_the_remote_side() {
        let script = "echo MARKER-7f3a; rm -rf /tmp/x";
        let variants = [
            ssh_host(),
            ssh_host().with_known_hosts("/k/kh"),
            ssh_host().with_identity("/k/id"),
            ssh_host()
                .with_known_hosts("/k/kh")
                .with_identity("/k/id")
                .with_ssh_dir("/opt/ssh")
                .with_public_address("198.51.100.1"),
        ];
        let allowed_options = [
            "BatchMode=yes",
            "ConnectTimeout=10",
            "ServerAliveInterval=15",
            "StrictHostKeyChecking=accept-new",
            "UserKnownHostsFile=/k/kh",
            "IdentitiesOnly=yes",
        ];
        for host in variants {
            let (argv, stdin) = host.shell(host.in_work(script));
            let argv = strings(&argv);
            assert!(argv[0].ends_with("ssh"), "{argv:?}");
            let n = argv.len();
            assert_eq!(
                argv[n - 3..],
                ["-T", "bench@testhost", "bash -s"],
                "{argv:?}"
            );
            let mut options = argv[1..n - 3].iter();
            while let Some(flag) = options.next() {
                let value = options.next().expect("each option has its value");
                match flag.as_str() {
                    "-o" => assert!(allowed_options.contains(&value.as_str()), "{value}"),
                    "-i" => assert_eq!(value, "/k/id"),
                    _ => panic!("an option the adapter never builds: {flag} in {argv:?}"),
                }
            }
            assert!(argv.iter().all(|a| !a.contains("MARKER")), "{argv:?}");
            let stdin = stdin.expect("the script goes on standard input");
            assert_eq!(
                stdin,
                format!("mkdir -p /tmp/udpstp-bench-4242 && cd /tmp/udpstp-bench-4242 && {script}")
            );
        }
        for destination in ["-oProxyCommand=sh", "-T", ""] {
            assert!(
                Host::ssh("server", destination, "", work(), Runner::new(Flag::new())).is_err()
            );
        }
    }

    #[test]
    fn a_local_host_runs_bash_c_with_nothing_on_standard_input() {
        let host = Host::local("home", "", work(), Runner::new(Flag::new()));
        let (argv, stdin) = host.shell(host.in_work("true"));
        assert_eq!(
            strings(&argv),
            [
                "bash",
                "-c",
                "mkdir -p /tmp/udpstp-bench-4242 && cd /tmp/udpstp-bench-4242 && true"
            ]
        );
        assert_eq!(stdin, None);
        assert_eq!(host.destination(), "local");
        assert!(host.is_local());
        // The ssh settings mean nothing here.
        let host = host.with_identity("/k/id").with_ssh_dir("/opt");
        assert!(host.is_local());
    }

    #[test]
    fn remote_paths_are_safe_as_they_are() {
        for good in [
            "runs/0001-probe-case-downstream-row5-n100-udpstp-r1/server",
            "/tmp/udpstp-bench-12",
            "udpst",
            "a:b",
            "k=v%2,x+y@z",
        ] {
            assert_eq!(RemotePath::new(good).unwrap().as_str(), good);
        }
        for bad in ["", "-rf", "a b", "a;b", "$(x)", "a'b", "é", "a\nb", "*"] {
            assert!(RemotePath::new(bad).is_err(), "{bad:?}");
        }
        let runs = RemotePath::new("runs").unwrap();
        assert_eq!(runs.join("0001").unwrap().as_str(), "runs/0001");
        assert!(runs.join("a b").is_err());
    }

    #[test]
    fn limits_print_as_the_python_gave_them() {
        // Review m6: the kind the Python gave, as its str() prints it.
        let float = |x: f64| Limit::new(Num::Float(x)).unwrap().to_string();
        assert_eq!(Limit::secs(120).to_string(), "120");
        assert_eq!(Limit::new(Num::Int(101)).unwrap().to_string(), "101");
        assert_eq!(float(101.0), "101.0");
        assert_eq!(float(0.2), "0.2");
        assert_eq!(float(104.5), "104.5");
        assert_eq!(float(0.0), "0.0");
        assert_eq!(float(0.0001), "0.0001");
        assert_eq!(float(0.00001), "1e-05");
        assert_eq!(float(9999999999999998.0), "9999999999999998.0");
        assert_eq!(float(1e16), "1e+16");
        assert_eq!(float(1.2345678901234568e17), "1.2345678901234568e+17");
        // Halfway between two shortest candidates: Python keeps the even
        // last digit, where Rust's own formatting rounds up to …002.3.
        assert_eq!(float(1_164_944_778_159_002.0 + 0.25), "1164944778159002.2");
        // What no clock waits for.
        for seconds in [
            Num::Int(-1),
            Num::Float(-0.5),
            Num::Float(f64::NAN),
            Num::Float(1e300),
        ] {
            assert!(Limit::new(seconds).is_none(), "{seconds:?}");
        }
        assert_eq!(float(1.5), "1.5");
        assert_eq!(
            Limit::new(Num::Float(1.5)).unwrap().duration(),
            Duration::from_millis(1500)
        );
        assert_eq!(RUN_LIMIT.duration(), Duration::from_secs(120));
    }

    #[test]
    fn too_much_output_says_how_much() {
        assert_eq!(
            HostError::too_much_output("server", "tar -czf - -C runs/x .", 1 << 30).message(),
            "server: more than 1 GiB of output: tar -czf - -C runs/x ."
        );
        assert_eq!(bytes(1 << 20), "1 MiB");
        assert_eq!(bytes(64 << 10), "64 KiB");
        assert_eq!(bytes(1000), "1000 bytes");
        assert_eq!(bytes(0), "0 bytes");
    }

    /// A scripted host: each call answers from the closure, its output read
    /// as a host's is.
    struct Scripted(fn(&str) -> Result<Finished, Stop>);

    impl Shell for Scripted {
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
            None
        }
        fn destination(&self) -> &str {
            "local"
        }
        fn run(&self, script: &str, _: Limit) -> Result<Completed, Stop> {
            Ok(Completed::from_finished(
                self.role(),
                script,
                (self.0)(script)?,
            )?)
        }
        fn put(&self, _: &Path, _: &RemotePath, _: u32) -> Result<(), Stop> {
            Ok(())
        }
        fn output(&self, script: &str, limit: Limit) -> Result<Vec<u8>, Stop> {
            Ok(self.run_checked(script, limit)?.stdout.into_bytes())
        }
    }

    /// Review m2: output that isn't UTF-8, on either stream, is the host's
    /// error, which voids the Round Run it came in, as in the Python since
    /// its fix on homelab-c.
    #[test]
    fn output_that_isnt_text_is_the_hosts_error() {
        let key = "0123456789abcdef0123456789abcdef";
        let script = format!("./udpst -a {key}");
        for (host, stream) in [
            (
                Scripted(|_| {
                    Ok(Finished {
                        returncode: 0,
                        stdout: b"ok \xff".to_vec(),
                        stderr: Vec::new(),
                    })
                }),
                "output",
            ),
            (
                Scripted(|_| {
                    Ok(Finished {
                        returncode: 1,
                        stdout: Vec::new(),
                        stderr: b"\xfe".to_vec(),
                    })
                }),
                "error output",
            ),
        ] {
            for done in [
                host.run(&script, RUN_LIMIT),
                host.run_checked(&script, RUN_LIMIT),
            ] {
                let Err(Stop::Host(e)) = done else {
                    panic!("a host error: {done:?}")
                };
                assert_eq!(
                    e.message(),
                    format!("client1: its {stream} isn't UTF-8: ./udpst -a <redacted>")
                );
            }
        }
    }

    /// Review m2: an archive that can't be unpacked is the host's error too.
    #[test]
    fn an_archive_that_cant_be_unpacked_is_the_hosts_error() {
        let host = Scripted(|_| {
            Ok(Finished {
                returncode: 0,
                stdout: b"not a gzip stream at all".to_vec(),
                stderr: Vec::new(),
            })
        });
        let into =
            std::env::temp_dir().join(format!("bench-rounds-host-archive-{}", std::process::id()));
        let Err(Stop::Host(e)) = host.fetch_dir(&RemotePath::new("runs/x").unwrap(), &into) else {
            panic!("a host error")
        };
        assert!(
            e.message()
                .starts_with("client1: fetching runs/x: not a readable .tar.gz"),
            "{e}"
        );
        let _ = fs::remove_dir_all(&into);
    }

    #[test]
    fn a_checked_run_says_what_failed_without_the_key() {
        let key = "0123456789abcdef0123456789abcdef";
        let host = Scripted(|_| {
            Ok(Finished {
                returncode: 3,
                stdout: Vec::new(),
                stderr: b"bash: ./udpst: No such file\r\n0123456789abcdef0123456789abcdef\n"
                    .to_vec(),
            })
        });
        let script = format!("./udpst -a {key} -y 7 h:1; echo {key} >&2; exit 3");
        let Err(Stop::Host(e)) = host.run_checked(&script, RUN_LIMIT) else {
            panic!("a host error")
        };
        assert_eq!(
            e.message(),
            "client1: exit 3: ./udpst -a <redacted> -y 7 h:1; echo <redacted> >&2; exit 3\n\
             bash: ./udpst: No such file\n<redacted>"
        );
        assert_eq!(
            HostError::timed_out(
                "client",
                &format!("sleep 5 # {key}"),
                Limit::new(Num::Float(0.2)).unwrap()
            )
            .message(),
            "client: timed out after 0.2 s: sleep 5 # <redacted>"
        );
        // The Python's `run.test_interval + 90`, a float: 101.0, not 101.
        assert_eq!(
            HostError::timed_out(
                "client1",
                "udpstp client",
                Limit::new(Num::Float(101.0)).unwrap()
            )
            .message(),
            "client1: timed out after 101.0 s: udpstp client"
        );
        // A run that finished, whatever its status, isn't checked.
        assert_eq!(host.run("x", RUN_LIMIT).unwrap().returncode, 3);
    }

    #[test]
    fn each_returns_the_results_in_order_once_all_have_ended() {
        let ended = AtomicUsize::new(0);
        let slow = |n: u64| {
            let ended = &ended;
            move || {
                thread::sleep(Duration::from_millis(n));
                ended.fetch_add(1, Ordering::SeqCst);
                Ok::<u64, Stop>(n)
            }
        };
        assert_eq!(
            each([slow(120), slow(10), slow(60)]).unwrap(),
            [120, 10, 60]
        );
        assert_eq!(ended.load(Ordering::SeqCst), 3);
        assert!(
            each(Vec::<fn() -> Result<(), Stop>>::new())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn each_returns_an_interrupt_ahead_of_any_error() {
        let host_error =
            |m: &'static str| move || -> Result<(), Stop> { Err(HostError::new(m).into()) };
        type Call = Box<dyn FnOnce() -> Result<(), Stop> + Send>;
        // The first host fails at once; the second sees the interrupt later.
        let calls: Vec<Call> = vec![
            Box::new(host_error("client1: exit 1")),
            Box::new(|| {
                thread::sleep(Duration::from_millis(80));
                Err(Stop::Interrupted)
            }),
            Box::new(host_error("client2: exit 2")),
        ];
        let start = Instant::now();
        assert!(matches!(each(calls), Err(Stop::Interrupted)));
        assert!(
            start.elapsed() >= Duration::from_millis(80),
            "waited for all"
        );
        // Without an interrupt: the first error by position, not by time.
        let calls: Vec<Call> = vec![
            Box::new(|| Ok(())),
            Box::new(|| {
                thread::sleep(Duration::from_millis(80));
                Err(HostError::new("second").into())
            }),
            Box::new(host_error("third")),
        ];
        match each(calls) {
            Err(Stop::Host(e)) => assert_eq!(e.message(), "second"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn each_raises_a_panic_again_once_all_have_ended() {
        let ended = AtomicUsize::new(0);
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            type Call<'a> = Box<dyn FnOnce() -> Result<(), Stop> + Send + 'a>;
            let calls: Vec<Call> = vec![
                Box::new(|| panic!("a bug")),
                Box::new(|| {
                    thread::sleep(Duration::from_millis(80));
                    ended.fetch_add(1, Ordering::SeqCst);
                    Err(Stop::Interrupted)
                }),
            ];
            each(calls)
        }));
        assert!(caught.is_err());
        assert_eq!(
            ended.load(Ordering::SeqCst),
            1,
            "the other call ended first"
        );
    }
}
