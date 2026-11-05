//! Where a Round's hosts come from: hosts that exist
//! (`static_hosts`), or hosts it rents, which it must pay for and
//! destroy. The Python's
//! providers are `hosts.StaticProvider` and `aws.AwsProvider`.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use indexmap::IndexMap;

use crate::host::{Shell, Stop};
use crate::utc::Utc;

/// A Unix time in seconds, as Python's `time.time()` gives it.
pub type UnixTime = f64;

/// A Round's ID, `rYYYYmmddTHHMMSS-xxxx`: in its resources' tags, its
/// ledger entry and its log. One the reaper finds in a tag may be any
/// text.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RoundId(String);

impl RoundId {
    /// The ID of a Round started at `now`, with `suffix` (four random hex
    /// digits), as the Python makes it (`round.py:746-747`): `None` where
    /// Python's `fromtimestamp` raises.
    pub fn new(now: UnixTime, suffix: &str) -> Option<RoundId> {
        let start = Utc::from_timestamp(now)?;
        Some(RoundId(format!("r{}-{suffix}", start.basic())))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for RoundId {
    fn from(id: String) -> RoundId {
        RoundId(id)
    }
}

impl From<&str> for RoundId {
    fn from(id: &str) -> RoundId {
        RoundId(id.to_owned())
    }
}

impl fmt::Display for RoundId {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a Round left behind: a resource's kind and ID, such as
/// `instance i-0abc`, or [`Leftover::unknown`] where the Round Teardown
/// couldn't tell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Leftover {
    pub kind: String,
    pub id: String,
}

impl Leftover {
    /// The kind of what couldn't be listed: its ID is the error.
    pub const UNKNOWN: &'static str = "unknown";

    pub fn new(kind: impl Into<String>, id: impl Into<String>) -> Leftover {
        Leftover {
            kind: kind.into(),
            id: id.into(),
        }
    }

    /// What a failed attempt may have left: to be checked by hand.
    pub fn unknown(error: impl fmt::Display) -> Leftover {
        Leftover::new(Leftover::UNKNOWN, error.to_string())
    }
}

impl fmt::Display for Leftover {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.kind == Leftover::UNKNOWN {
            write!(f, "unknown ({})", self.id)
        } else {
            write!(f, "{} {}", self.kind, self.id)
        }
    }
}

/// Leftovers in a line: `instance i-1, security-group sg-2`.
pub fn listed(left: &[Leftover]) -> String {
    let each: Vec<String> = left.iter().map(Leftover::to_string).collect();
    each.join(", ")
}

/// How the Round reaches the hosts it creates: the public half of its SSH
/// key, which a rented host's first boot installs, the files ssh then
/// signs in with, and where `ssh` and `scp` are, if not on PATH
/// (`--ssh-dir`, which the Python sets for every host). Hosts that exist
/// are reached as they are.
#[derive(Clone, Debug, Default)]
pub struct Access {
    pub public_key: String,
    pub known_hosts: Option<PathBuf>,
    pub identity: Option<PathBuf>,
    pub ssh_dir: Option<PathBuf>,
}

/// Why a provider's call failed: its error, or an interrupt, never one
/// disguised as the other.
#[derive(Debug)]
pub enum ProviderError {
    Failed(String),
    Interrupted,
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ProviderError::Failed(message) => f.write_str(message),
            ProviderError::Interrupted => f.write_str("interrupted"),
        }
    }
}

impl std::error::Error for ProviderError {}

impl From<Stop> for ProviderError {
    fn from(stop: Stop) -> ProviderError {
        match stop {
            Stop::Host(e) => ProviderError::Failed(e.to_string()),
            Stop::Interrupted => ProviderError::Interrupted,
        }
    }
}

/// Where a Round's hosts come from.
pub trait Provider {
    /// `static` or `aws`: in the plan and the ledger.
    fn name(&self) -> &'static str;

    /// Whether its hosts are rented: paid by the hour, with a dead-man
    /// switch, allowance counters and strict layouts.
    fn rented(&self) -> bool;

    /// What a host of `host_type` costs an hour, $.
    fn price_per_hour(&self, host_type: &str) -> Result<f64, ProviderError>;

    /// The preflight: whatever it needs to create a Round that can run for
    /// `max_hours` from `now`, checked without creating anything.
    fn check(&mut self, max_hours: f64, now: UnixTime) -> Result<(), ProviderError>;

    /// Destroys what earlier Rounds left past their expiry: their IDs.
    fn reap(&mut self, now: UnixTime) -> Result<Vec<RoundId>, ProviderError>;

    /// Creates a host for each of `roles`, tagged with `round` and ending
    /// itself by `expiry`: the hosts, in the order of `roles`.
    fn create(
        &mut self,
        round: &RoundId,
        expiry: UnixTime,
        roles: &[&str],
        access: &Access,
    ) -> Result<IndexMap<String, Box<dyn Shell>>, ProviderError>;

    /// Destroys what the Round created: what is left.
    fn destroy(&mut self, round: &RoundId) -> Result<Vec<Leftover>, ProviderError>;

    /// Lists what of the Round still exists.
    fn leftovers(&mut self, round: &RoundId) -> Result<Vec<Leftover>, ProviderError>;

    /// After a destroy that left nothing, how long to wait before listing
    /// again, since a list can lag behind what was just created or ended;
    /// `None` for a provider that creates nothing.
    fn recheck_after(&self) -> Option<Duration>;

    /// The commands that delete what the Round left, by hand.
    fn manual_cleanup(&self, round: &RoundId) -> Vec<String>;

    /// Retain named files in this role's work directory at the Round
    /// Teardown. Only a provider of existing hosts supports this; a default
    /// refusal prevents a provider promising to keep files it will destroy.
    fn keep_files(&mut self, _role: &str, _names: &[&str]) -> Result<(), ProviderError> {
        Err(ProviderError::Failed(
            "this provider cannot retain files at the Round Teardown".into(),
        ))
    }

    /// Files retained on purpose by the latest destroy, and how to remove
    /// them after use. They are not resources left by a failed teardown.
    fn kept_lines(&self) -> Vec<String> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::HostError;

    #[test]
    fn a_round_id_is_its_start_in_utc_and_a_suffix() {
        // 2026-09-29T12:34:56.789Z
        let id = RoundId::new(1_790_685_296.789, "a1b2").unwrap();
        assert_eq!(id.as_str(), "r20260929T123456-a1b2");
        assert_eq!(RoundId::new(f64::NAN, "a1b2"), None);
        assert_eq!(RoundId::from("old").to_string(), "old");
    }

    #[test]
    fn leftovers_read_as_what_to_delete() {
        let left = [
            Leftover::new("instance", "i-1"),
            Leftover::new("security-group", "sg-2"),
            Leftover::unknown("aws ec2 terminate-instances: throttled"),
        ];
        assert_eq!(
            listed(&left),
            "instance i-1, security-group sg-2, unknown (aws ec2 terminate-instances: throttled)"
        );
        assert_eq!(listed(&[]), "");
    }

    #[test]
    fn an_interrupt_stays_an_interrupt() {
        assert!(matches!(
            ProviderError::from(Stop::Interrupted),
            ProviderError::Interrupted
        ));
        let failed = ProviderError::from(Stop::Host(HostError::new("server: exit 1: x\n")));
        assert_eq!(failed.to_string(), "server: exit 1: x\n");
    }
}
