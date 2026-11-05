//! The AWS provider, through the `aws` CLI v2: no SDK, no key pairs.
//!
//! What keeps a Round from leaving anything behind or spending too much,
//! as in the Python:
//!
//! - Every resource is tagged in the call that creates it, never by a
//!   later `create-tags`, so a create interrupted after AWS accepted it
//!   still carries the Round's tag.
//! - Each `run-instances` carries a client token of the Round and role,
//!   so a retry after a timeout returns the same instance.
//! - Each instance ends itself: its user data arms the dead-man switch
//!   ([`crate::dead_man`]) before anything else, and it is
//!   launched to terminate when it shuts down.
//! - The Round's SSH key goes in through the user data: there is no EC2
//!   key pair to leak or clean up.
//! - The security group admits SSH only from the home address, and UDP
//!   only from the Round's own hosts and, for a Round with Tests from home,
//!   the home address; never the whole internet.
//! - The Round Teardown terminates by tag and by the instance IDs
//!   recorded, waits until they are terminated, then deletes the security
//!   group, retrying while an interface still holds it; then lists again.
//!
//! The parts: [`plan`] builds each call's argv and decides from the
//! answers; [`AwsProvider`] makes the Python provider's calls in its
//! order through [`Calls`], which [`cli::Cli`] implements with the `aws`
//! CLI and [`fake::Local`] in-process; [`cli`] also makes
//! `AwsProvider<Cli>` a Round's provider, whose hosts it reaches over ssh;
//! [`fake`] is the EC2 model the `fake-aws` binary wraps; [`record`]
//! keeps the raw records of a Round's real calls outside the checkout,
//! never committed; [`replay`] replays them against the fake, by hand, and
//! says where it answers otherwise without saying the records' values.

pub mod base64;
pub mod cli;
pub mod fake;
pub mod plan;
pub mod record;
pub mod replay;

use std::collections::BTreeSet;
use std::fmt;

use indexmap::IndexMap;

use crate::dead_man;
use crate::num::Num;
use crate::py::Json;

/// What the Python raises as an `AwsError`: a call the CLI refused
/// (`aws X Y: <its stderr>`, which the retries read), or a refusal of the
/// provider's own (a price missing, credentials that expire too soon, an
/// image whose volume outlives it). The Round refuses to go on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AwsError(pub String);

impl fmt::Display for AwsError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for AwsError {}

/// Why a call to AWS didn't give the provider what it needed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallError {
    /// An [`AwsError`].
    Aws(AwsError),
    /// The call didn't end as a call does, where the Python raises
    /// something other than an `AwsError`: the CLI didn't start or took
    /// too long, its output isn't JSON, or an answer isn't of the shape
    /// the provider reads. Never retried.
    Broken(String),
    /// The Round was interrupted: never retried, never hidden behind
    /// another error.
    Interrupted,
}

impl fmt::Display for CallError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            CallError::Aws(e) => write!(f, "{e}"),
            CallError::Broken(e) => f.write_str(e),
            CallError::Interrupted => f.write_str("interrupted"),
        }
    }
}

impl std::error::Error for CallError {}

impl From<AwsError> for CallError {
    fn from(e: AwsError) -> CallError {
        CallError::Aws(e)
    }
}

/// The calls a provider makes, and its clock: the seam between what the
/// provider decides and the CLI that does it.
pub trait Calls {
    /// One call: `args` as [`plan`] builds them, to which the CLI appends
    /// the region and JSON output ([`plan::argv`]); what it printed, read
    /// as [`plan::read_answer`] reads it. A call the CLI ran and failed is
    /// [`CallError::Aws`], which the retries read; one that timed out or
    /// didn't run is [`CallError::Broken`].
    fn call(&mut self, args: &[String]) -> Result<Json, CallError>;

    /// `configure export-credentials`: when the credentials expire, never
    /// the credentials; `None` where the call failed or says nothing of it
    /// ([`plan::expiration`]).
    fn credentials_expire(&mut self) -> Result<Option<String>, CallError>;

    /// Waits between two attempts of a call; [`CallError::Interrupted`]
    /// where the Round was interrupted meanwhile.
    fn sleep(&mut self, seconds: u64) -> Result<(), CallError>;

    /// The wall clock, Unix time.
    fn now(&mut self) -> f64;
}

/// One host a Round launched: its instance, and its addresses as EC2
/// lists them (a public one only where it has one). The `Provider` built
/// on this ([`cli`]) refuses a host without a public or a private IPv4
/// address, where the Python goes on with `ubuntu@None`
///.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launched {
    pub instance_id: String,
    pub public_address: Option<String>,
    pub private_address: Option<String>,
}

/// The AWS provider's decisions and calls, in the Python's order, over
/// the CLI `C`: `AwsProvider` in `aws.py`, without the hosts it hands the
/// Round (the `Provider` built on it makes them).
#[derive(Clone, Debug)]
pub struct AwsProvider<C> {
    pub calls: C,
    pub region: String,
    pub zone: String,
    /// Each role's instance type.
    pub host_types: IndexMap<String, String>,
    /// Each instance type's pinned price, `$`/h.
    pub prices: IndexMap<String, Num>,
    /// This machine's public address, `/32`.
    pub home_cidr: String,
    /// Whether the Round has Tests from home, whose UDP the group admits.
    pub home_udp: bool,
    /// Each Round's instances, by role, as they were launched: the Round
    /// Teardown uses them besides the tag, which a list may not show yet
    /// for an instance just launched. Only this provider's own.
    instances: IndexMap<String, IndexMap<String, String>>,
    group_id: Option<String>,
}

fn string<'a>(value: &'a Json, what: &str) -> Result<&'a str, CallError> {
    value
        .as_str()
        .ok_or_else(|| CallError::Broken(format!("{what} isn't a string")))
}

fn item<'a>(value: &'a Json, key: &str, call: &'static str) -> Result<&'a Json, CallError> {
    value.item(key).map_err(plan::broken(call))
}

fn list<'a>(value: &'a Json, call: &str) -> Result<&'a [Json], CallError> {
    value.as_list().ok_or_else(|| {
        CallError::Broken(format!(
            "aws ec2 {call}: a {} where a list belongs",
            value.type_name()
        ))
    })
}

impl<C: Calls> AwsProvider<C> {
    pub fn new(
        calls: C,
        region: &str,
        zone: &str,
        host_types: IndexMap<String, String>,
        prices: IndexMap<String, Num>,
        home_cidr: &str,
        home_udp: bool,
    ) -> AwsProvider<C> {
        AwsProvider {
            calls,
            region: region.to_string(),
            zone: zone.to_string(),
            host_types,
            prices,
            home_cidr: home_cidr.to_string(),
            home_udp,
            instances: IndexMap::new(),
            group_id: None,
        }
    }

    /// The instances this provider launched for `round_id`, by role.
    pub fn launched(&self, round_id: &str) -> Option<&IndexMap<String, String>> {
        self.instances.get(round_id)
    }

    /// The security group it last made.
    pub fn group_id(&self) -> Option<&str> {
        self.group_id.as_deref()
    }

    /// `_retrying`: the call, tried again after [`plan::Retry::again`]'s
    /// wait while its error is worth it.
    fn retrying(&mut self, args: &[String], retry: plan::Retry) -> Result<Json, CallError> {
        for attempt in 0..retry.attempts {
            match self.calls.call(args) {
                Err(CallError::Aws(e)) => match retry.again(attempt, &e) {
                    Some(seconds) => self.calls.sleep(seconds)?,
                    None => return Err(CallError::Aws(e)),
                },
                other => return other,
            }
        }
        Err(CallError::Broken("a call with no attempts".into()))
    }

    /// `price_per_hour(host_type)`: its pinned price.
    pub fn price_per_hour(&self, host_type: &str) -> Result<Num, AwsError> {
        self.prices.get(host_type).copied().ok_or_else(|| {
            AwsError(format!(
                "no pinned price for {host_type}: add it to prices.toml"
            ))
        })
    }

    /// `check(max_hours)`: credentials that outlast a Round of
    /// `max_hours`, and an image whose volumes go with the instance; the
    /// image.
    pub fn check(&mut self, max_hours: f64) -> Result<Json, CallError> {
        self.calls.call(&plan::get_caller_identity())?;
        if let Some(expiration) = self.calls.credentials_expire()? {
            let now = self.calls.now();
            plan::credentials_outlast(&expiration, now, max_hours)?;
        }
        let image = self.image()?;
        plan::volumes_die_with(&image)?;
        Ok(image)
    }

    /// `image()`: the latest of Canonical's Ubuntu 24.04 images here.
    pub fn image(&mut self) -> Result<Json, CallError> {
        let answer = self.calls.call(&plan::describe_images())?;
        plan::latest_image(&answer)
    }

    /// `_instances(*filters)`: the live instances the filters take.
    fn instances(&mut self, filters: &[String]) -> Result<Vec<Json>, CallError> {
        let found = self.calls.call(&plan::describe_instances(filters))?;
        let empty = Json::List(Vec::new());
        let mut instances = Vec::new();
        let reservations = found
            .get_or("Reservations", &empty)
            .map_err(plan::broken("describe-instances"))?;
        // An answer of another shape is never read as none: the Round
        // Teardown would call it nothing left.
        for reservation in list(reservations, "describe-instances")? {
            let listed = reservation
                .get_or("Instances", &empty)
                .map_err(plan::broken("describe-instances"))?;
            instances.extend(list(listed, "describe-instances")?.iter().cloned());
        }
        Ok(instances)
    }

    /// `_groups(*filters)`: the security groups the filters take.
    fn groups(&mut self, filters: &[String]) -> Result<Vec<Json>, CallError> {
        let found = self.calls.call(&plan::describe_security_groups(filters))?;
        let empty = Json::List(Vec::new());
        let groups = found
            .get_or("SecurityGroups", &empty)
            .map_err(plan::broken("describe-security-groups"))?;
        Ok(list(groups, "describe-security-groups")?.to_vec())
    }

    /// `reap(now)`: destroys what earlier Rounds left past their expiry,
    /// whichever tool made them; the Round IDs it reaped, in the order
    /// their resources were listed.
    pub fn reap(&mut self, now: f64) -> Result<Vec<String>, CallError> {
        let mut rounds: IndexMap<Option<String>, f64> = IndexMap::new();
        for instance in self.instances(&[plan::any_round_filter()])? {
            let (round, expiry) = round_and_expiry(&instance)?;
            rounds.insert(round, expiry);
        }
        for group in self.groups(&[plan::any_round_filter()])? {
            let (round, expiry) = round_and_expiry(&group)?;
            rounds.entry(round).or_insert(expiry);
        }
        let reaped: Vec<String> = rounds
            .into_iter()
            .filter_map(|(round, expiry)| round.filter(|r| !r.is_empty() && expiry < now))
            .collect();
        for round_id in &reaped {
            self.destroy(round_id)?;
        }
        Ok(reaped)
    }

    /// `check_permissions(round_id)`: asks EC2, with `--dry-run`, whether
    /// this identity may do what a Round does; nothing is created. Each
    /// action's verdict: `allowed`, or the error AWS gave.
    pub fn check_permissions(
        &mut self,
        round_id: &str,
    ) -> Result<IndexMap<String, String>, CallError> {
        let subnets = self.calls.call(&plan::describe_subnets(&self.zone))?;
        let (subnet_id, vpc_id) = first_subnet(&subnets)?;
        let now = self.calls.now();
        let expiry = dead_man::whole_seconds(now + 3600.0)
            .ok_or_else(|| CallError::Broken(format!("the clock says {now}")))?;
        let mut attempts = vec![(
            "create-security-group".to_string(),
            plan::check_create_security_group(round_id, expiry, &vpc_id),
        )];
        let image = self.image()?;
        let image_id = string(item(&image, "ImageId", "describe-images")?, "ImageId")?.to_string();
        // Each instance type the Round launches, since the policy names
        // them.
        let types: BTreeSet<&String> = self.host_types.values().collect();
        for host_type in types {
            attempts.push((
                format!("run-instances {host_type}"),
                plan::check_run_instances(round_id, expiry, &image_id, host_type, &subnet_id),
            ));
        }
        let mut verdicts = IndexMap::new();
        for (action, args) in attempts {
            let verdict = match self.calls.call(&args) {
                Ok(_) => "unexpected: it ran without --dry-run's answer".to_string(),
                Err(CallError::Aws(e)) if e.0.contains("DryRunOperation") => "allowed".to_string(),
                Err(CallError::Aws(e)) => e.0,
                Err(other) => return Err(other),
            };
            verdicts.insert(action, verdict);
        }
        Ok(verdicts)
    }

    /// `create(round_id, expiry, roles, public_key)`: the Round's security
    /// group and one instance per role, once running; each role's
    /// instance and addresses. Each instance is recorded as soon as it is
    /// launched, so that a failure after it still leaves its ID to the
    /// Round Teardown.
    pub fn create(
        &mut self,
        round_id: &str,
        expiry: f64,
        roles: &[String],
        public_key: &str,
    ) -> Result<IndexMap<String, Launched>, CallError> {
        let deadline = dead_man::whole_seconds(expiry)
            .ok_or_else(|| CallError::Broken(format!("the expiry {expiry}")))?;
        let subnets = self.calls.call(&plan::describe_subnets(&self.zone))?;
        let (subnet_id, vpc_id) = first_subnet(&subnets)?;
        let group = self
            .calls
            .call(&plan::create_security_group(round_id, deadline, &vpc_id))?;
        let group_id =
            string(item(&group, "GroupId", "create-security-group")?, "GroupId")?.to_string();
        self.group_id = Some(group_id.clone());
        let rules = plan::ingress_rules(&self.home_cidr, &group_id, self.home_udp)?;
        self.calls
            .call(&plan::authorize_security_group_ingress(&group_id, &rules))?;
        let now = self.calls.now();
        let minutes = plan::minutes_left(expiry, now);
        let user_data = plan::user_data_base64(minutes, public_key, deadline);
        let image = self.image()?;
        let image_id = string(item(&image, "ImageId", "describe-images")?, "ImageId")?.to_string();
        for role in roles {
            let host_type = self
                .host_types
                .get(role)
                .ok_or_else(|| CallError::Broken(format!("no instance type for {role}")))?
                .clone();
            let launch = plan::Launch {
                round_id,
                role,
                expiry: deadline,
                image_id: &image_id,
                host_type: &host_type,
                subnet_id: &subnet_id,
                group_id: &group_id,
                user_data: &user_data,
            };
            let launched = self.retrying(&plan::run_instances(&launch), plan::RETRY_LAUNCH)?;
            let instance = item(&launched, "Instances", "run-instances")?
                .at(0)
                .map_err(plan::broken("run-instances"))?;
            let id = string(item(instance, "InstanceId", "run-instances")?, "InstanceId")?;
            self.instances
                .entry(round_id.to_string())
                .or_default()
                .insert(role.clone(), id.to_string());
        }
        let recorded = self
            .instances
            .get(round_id)
            .cloned()
            .ok_or_else(|| CallError::Broken("no instance was launched".into()))?;
        let ids: Vec<String> = recorded.values().cloned().collect();
        self.retrying(&plan::wait("instance-running", &ids), plan::RETRY)?;
        let mut described: IndexMap<String, Json> = IndexMap::new();
        for instance in self.instances(&[plan::round_filter(round_id)])? {
            let id = string(
                item(&instance, "InstanceId", "describe-instances")?,
                "InstanceId",
            )?;
            described.insert(id.to_string(), instance.clone());
        }
        let mut hosts = IndexMap::new();
        for (role, id) in recorded {
            let address = |key: &str| -> Result<Option<String>, CallError> {
                match described
                    .get(&id)
                    .and_then(|i| i.as_dict())
                    .and_then(|i| i.get(key))
                {
                    None | Some(Json::Null) => Ok(None),
                    Some(Json::Str(a)) => Ok(Some(a.clone())),
                    Some(_) => Err(CallError::Broken(format!("{id}'s {key} isn't a string"))),
                }
            };
            hosts.insert(
                role,
                Launched {
                    public_address: address("PublicIpAddress")?,
                    private_address: address("PrivateIpAddress")?,
                    instance_id: id,
                },
            );
        }
        Ok(hosts)
    }

    /// `destroy(round_id)`: terminates the Round's instances (by tag, and
    /// as recorded), waits for them, and deletes its security groups; what
    /// is left ([`AwsProvider::leftovers`]).
    pub fn destroy(&mut self, round_id: &str) -> Result<Vec<(String, String)>, CallError> {
        let mut ids = BTreeSet::new();
        for instance in self.instances(&[plan::round_filter(round_id)])? {
            let id = string(
                item(&instance, "InstanceId", "describe-instances")?,
                "InstanceId",
            )?;
            ids.insert(id.to_string());
        }
        if let Some(recorded) = self.instances.get(round_id) {
            ids.extend(recorded.values().cloned());
        }
        let ids: Vec<String> = ids.into_iter().collect();
        if !ids.is_empty() {
            self.retrying(&plan::terminate_instances(&ids), plan::RETRY)?;
            self.retrying(&plan::wait("instance-terminated", &ids), plan::RETRY)?;
        }
        for group in self.groups(&[plan::round_filter(round_id)])? {
            let id = string(
                item(&group, "GroupId", "describe-security-groups")?,
                "GroupId",
            )?;
            self.retrying(&plan::delete_security_group(id), plan::RETRY_DELETE_GROUP)?;
        }
        self.leftovers(round_id)
    }

    /// `leftovers(round_id)`: what of the Round still exists, as (kind,
    /// ID): `instance` and `security-group`.
    pub fn leftovers(&mut self, round_id: &str) -> Result<Vec<(String, String)>, CallError> {
        let mut left = Vec::new();
        for instance in self.instances(&[plan::round_filter(round_id)])? {
            let id = string(
                item(&instance, "InstanceId", "describe-instances")?,
                "InstanceId",
            )?;
            left.push(("instance".to_string(), id.to_string()));
        }
        for group in self.groups(&[plan::round_filter(round_id)])? {
            let id = string(
                item(&group, "GroupId", "describe-security-groups")?,
                "GroupId",
            )?;
            left.push(("security-group".to_string(), id.to_string()));
        }
        Ok(left)
    }

    /// `manual_cleanup(round_id)`: the commands that delete what is left,
    /// by hand.
    pub fn manual_cleanup(&self, round_id: &str) -> Vec<String> {
        plan::manual_cleanup(&self.region, round_id)
    }
}

/// The zone's first default subnet: its ID and its VPC's.
fn first_subnet(answer: &Json) -> Result<(String, String), CallError> {
    let subnet = item(answer, "Subnets", "describe-subnets")?
        .at(0)
        .map_err(plan::broken("describe-subnets"))?;
    let id = string(item(subnet, "SubnetId", "describe-subnets")?, "SubnetId")?;
    let vpc = string(item(subnet, "VpcId", "describe-subnets")?, "VpcId")?;
    Ok((id.to_string(), vpc.to_string()))
}

/// A resource's Round and expiry, as the reaper reads its tags: the Round
/// `None` where it has no such tag, the expiry 0 where it has none.
fn round_and_expiry(resource: &Json) -> Result<(Option<String>, f64), CallError> {
    let empty = Json::List(Vec::new());
    let shape = |e: crate::py::PyError| CallError::Broken(format!("a resource's tags: {e}"));
    let tags = resource.get_or("Tags", &empty).map_err(shape)?;
    let mut by_key: IndexMap<String, Json> = IndexMap::new();
    for tag in list(tags, "describe-instances")? {
        let key = tag.item("Key").map_err(shape)?;
        let value = tag.item("Value").map_err(shape)?;
        by_key.insert(string(key, "a tag's key")?.to_string(), value.clone());
    }
    let round = match by_key.get(plan::TAG) {
        None => None,
        Some(Json::Str(r)) => Some(r.clone()),
        Some(_) => return Err(CallError::Broken("a Round tag isn't a string".into())),
    };
    let expiry = match by_key.get(plan::EXPIRY_TAG) {
        None => 0.0,
        Some(Json::Str(text)) => {
            py_float(text).ok_or_else(|| CallError::Broken(format!("the expiry tag {text:?}")))?
        }
        Some(_) => return Err(CallError::Broken("an expiry tag isn't a string".into())),
    };
    Ok((round, expiry))
}

/// `float(text)` for the numbers the tools write in a tag: Python's
/// whitespace around a decimal number, which Rust's parser reads alike;
/// `None` for the rest, underscores among them, which Python reads.
fn py_float(text: &str) -> Option<f64> {
    let text = crate::py::strip(text);
    (!text.contains('_')).then(|| text.parse().ok()).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fake::Local;

    fn provider(state: &str, roles: &[&str], home_udp: bool) -> AwsProvider<Local> {
        let fake = Local::new(Json::parse(state).unwrap(), "eu-north-1", 1_790_000_000.0);
        AwsProvider::new(
            fake,
            "eu-north-1",
            "eu-north-1a",
            roles
                .iter()
                .map(|r| (r.to_string(), "c6in.large".to_string()))
                .collect(),
            IndexMap::from([("c6in.large".to_string(), Num::Float(0.142))]),
            "203.0.113.7/32",
            home_udp,
        )
    }

    fn roles(names: &[&str]) -> Vec<String> {
        names.iter().map(|r| r.to_string()).collect()
    }

    /// A CLI that answers each operation as its script says, for answers
    /// the fake never gives; `{}` for any other.
    struct Scripted(IndexMap<&'static str, Json>);

    impl Calls for Scripted {
        fn call(&mut self, args: &[String]) -> Result<Json, CallError> {
            let answer = self.0.get(args[1].as_str()).cloned();
            Ok(answer.unwrap_or_else(|| Json::Dict(IndexMap::new())))
        }

        fn credentials_expire(&mut self) -> Result<Option<String>, CallError> {
            Ok(None)
        }

        fn sleep(&mut self, _: u64) -> Result<(), CallError> {
            Ok(())
        }

        fn now(&mut self) -> f64 {
            1_790_000_000.0
        }
    }

    #[test]
    fn an_answer_of_another_shape_is_never_read_as_nothing_left() {
        // Where the Python iterates what isn't a list, it raises; a port
        // that read it as an empty list would call the Round's resources
        // gone, and its Round Teardown done.
        let scripted = |instances: &str, groups: &str| {
            AwsProvider::new(
                Scripted(IndexMap::from([
                    ("describe-instances", Json::parse(instances).unwrap()),
                    ("describe-security-groups", Json::parse(groups).unwrap()),
                ])),
                "eu-north-1",
                "eu-north-1a",
                IndexMap::new(),
                IndexMap::new(),
                "203.0.113.7/32",
                true,
            )
        };
        for (instances, groups) in [
            (
                r#"{"Reservations": {"r-1": []}}"#,
                r#"{"SecurityGroups": []}"#,
            ),
            (
                r#"{"Reservations": [{"Instances": "i-1"}]}"#,
                r#"{"SecurityGroups": []}"#,
            ),
            (
                r#"{"Reservations": []}"#,
                r#"{"SecurityGroups": {"sg-1": {}}}"#,
            ),
            (
                r#"{"Reservations": [], "extra": 1}"#,
                r#"{"SecurityGroups": "sg-1"}"#,
            ),
        ] {
            let mut p = scripted(instances, groups);
            assert!(
                matches!(p.leftovers("r1"), Err(CallError::Broken(_))),
                "{instances} {groups}"
            );
            assert!(
                matches!(p.destroy("r1"), Err(CallError::Broken(_))),
                "{instances} {groups}"
            );
            assert!(
                matches!(p.reap(2e9), Err(CallError::Broken(_))),
                "{instances} {groups}"
            );
        }
        // Lists with nothing in them, or no such key, are nothing, as in
        // the Python's `.get(key, [])`.
        for (instances, groups) in [
            (
                r#"{"Reservations": [{"Instances": []}]}"#,
                r#"{"SecurityGroups": []}"#,
            ),
            ("{}", "{}"),
        ] {
            assert_eq!(scripted(instances, groups).leftovers("r1"), Ok(vec![]));
        }
    }

    fn calls(p: &AwsProvider<Local>, operation: &str) -> Vec<Vec<String>> {
        p.calls
            .state
            .item("calls")
            .unwrap()
            .as_list()
            .unwrap()
            .iter()
            .map(|c| {
                c.as_list()
                    .unwrap()
                    .iter()
                    .map(|a| a.as_str().unwrap().to_string())
                    .collect::<Vec<_>>()
            })
            .filter(|c| c[1] == operation)
            .collect()
    }

    fn flag<'a>(call: &'a [String], name: &str) -> &'a str {
        &call[call.iter().position(|a| a == name).unwrap() + 1]
    }

    #[test]
    fn instances_are_tagged_at_creation_and_end_themselves() {
        // tests/test_round.py's FakeAws.
        let mut p = provider(
            r#"{"config": {"expiration": "2099-01-01T00:00:00Z"}}"#,
            &["server", "client"],
            true,
        );
        let expiry = 1_790_000_000.0 + 7200.0;
        let hosts = p
            .create(
                "r1",
                expiry,
                &roles(&["server", "client"]),
                "ssh-ed25519 AAAA t",
            )
            .unwrap();
        assert_eq!(
            hosts["server"].public_address.as_deref(),
            Some("198.51.100.1")
        );
        assert_eq!(hosts["client"].private_address.as_deref(), Some("10.0.0.2"));
        for call in calls(&p, "run-instances") {
            assert!(flag(&call, "--client-token").starts_with("r1-"));
            assert_eq!(
                flag(&call, "--instance-initiated-shutdown-behavior"),
                "terminate"
            );
            let data = base64::decode(flag(&call, "--user-data")).unwrap();
            let data = String::from_utf8(data).unwrap();
            assert!(data.starts_with("#!/bin/bash\nshutdown -P +120\n"));
            assert!(data.contains(&dead_man::at_boot(1_790_007_200)));
        }
        assert!(
            calls(&p, "create-tags").is_empty(),
            "tags come with the create"
        );
        assert!(calls(&p, "create-key-pair").is_empty(), "no EC2 key pairs");
    }

    #[test]
    fn a_throttled_launch_is_retried_with_the_same_token() {
        let mut p = provider(
            r#"{"config": {"throttle_run_instances": 2}}"#,
            &["server"],
            true,
        );
        p.create("r1", 1_790_007_200.0, &roles(&["server"]), "k")
            .unwrap();
        let tokens: BTreeSet<String> = calls(&p, "run-instances")
            .iter()
            .map(|c| flag(c, "--client-token").to_string())
            .collect();
        assert_eq!(tokens, BTreeSet::from(["r1-server".to_string()]));
        assert_eq!(p.calls.slept, [1, 2]);
    }

    #[test]
    fn the_round_teardown_terminates_waits_then_deletes_the_group() {
        let mut p = provider("{}", &["server", "client"], true);
        p.create("r1", 1_790_007_200.0, &roles(&["server", "client"]), "k")
            .unwrap();
        let Json::Dict(top) = &mut p.calls.state else {
            unreachable!()
        };
        let Json::Dict(config) = &mut top["config"] else {
            unreachable!()
        };
        config.insert("dependency_violations".into(), Json::Int(2));
        assert_eq!(p.destroy("r1").unwrap(), []);
        assert_eq!(p.calls.slept, [1, 2]);
        // A fresh provider finds the instances by their tag alone.
        let mut fresh = provider("{}", &[], true);
        fresh.calls.state = p.calls.state.clone();
        assert_eq!(fresh.destroy("r1").unwrap(), []);
    }

    #[test]
    fn what_is_left_is_reported() {
        let mut p = provider("{}", &["server", "client"], true);
        p.create("r1", 1_790_007_200.0, &roles(&["server", "client"]), "k")
            .unwrap();
        let Json::Dict(top) = &mut p.calls.state else {
            unreachable!()
        };
        let Json::Dict(config) = &mut top["config"] else {
            unreachable!()
        };
        config.insert("leave_instance".into(), Json::Str("-server".into()));
        assert_eq!(
            p.destroy("r1").unwrap(),
            [("instance".to_string(), "i-1".to_string())]
        );
        assert!(
            p.manual_cleanup("r1")
                .iter()
                .any(|c| c.contains("terminate-instances"))
        );
    }

    #[test]
    fn the_reaper_takes_only_expired_rounds() {
        let mut p = provider("{}", &["server"], true);
        let now = 1_790_000_000.0;
        p.create("old", now - 60.0, &roles(&["server"]), "k")
            .unwrap();
        p.create("new", now + 7200.0, &roles(&["server"]), "k")
            .unwrap();
        assert_eq!(p.reap(now).unwrap(), ["old"]);
    }

    #[test]
    fn the_permission_check_creates_nothing() {
        let mut p = provider("{}", &["server", "client"], true);
        let verdicts = p.check_permissions("permission-check").unwrap();
        assert_eq!(
            verdicts,
            IndexMap::from([
                ("create-security-group".to_string(), "allowed".to_string()),
                (
                    "run-instances c6in.large".to_string(),
                    "allowed".to_string()
                ),
            ])
        );
        let mut p = provider(
            r#"{"config": {"unauthorized": "run-instances"}}"#,
            &["server"],
            true,
        );
        let verdicts = p.check_permissions("permission-check").unwrap();
        assert!(verdicts["run-instances c6in.large"].contains("UnauthorizedOperation"));
        assert_eq!(
            p.calls.state.item("instances").unwrap(),
            &Json::Dict(IndexMap::new())
        );
    }

    #[test]
    fn credentials_and_the_image_are_checked() {
        let mut p = provider(
            r#"{"config": {"expiration": "2000-01-01T00:00:00Z"}}"#,
            &["server"],
            true,
        );
        assert!(matches!(p.check(2.0), Err(CallError::Aws(_))));
        let mut p = provider(
            r#"{"config": {"delete_on_termination": false}}"#,
            &["server"],
            true,
        );
        assert!(matches!(p.check(2.0), Err(CallError::Aws(_))));
        // No credentials to export: nothing to check.
        let mut p = provider("{}", &["server"], true);
        assert!(p.check(2.0).is_ok());
    }

    #[test]
    fn a_missing_price_is_refused() {
        let p = provider("{}", &["server"], true);
        assert_eq!(p.price_per_hour("c6in.large"), Ok(Num::Float(0.142)));
        assert_eq!(
            p.price_per_hour("m7i.large"),
            Err(AwsError(
                "no pinned price for m7i.large: add it to prices.toml".into()
            ))
        );
    }
}
