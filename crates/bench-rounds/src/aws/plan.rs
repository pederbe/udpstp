//! Each call a Round makes to the `aws` CLI, as the Python makes it
//! (`scripts/bench/aws.py:150-352`): its argv, the tags every resource
//! gets in the call that creates it, the security group's rules, the
//! instances' user data, the retries and their backoff, and what the
//! provider decides from the answers. Nothing here runs a call.
//!
//! The JSON-valued arguments (`--tag-specifications`, `--ip-permissions`)
//! are written as Python's `json.dumps` writes them ([`py::dumps`]), so
//! every argv compares byte for byte with the Python's, and a Round of
//! either tool makes the same calls.

use std::net::Ipv4Addr;
use std::time::Duration;

use indexmap::IndexMap;

use super::base64;
use super::{AwsError, CallError};
use crate::dead_man;
use crate::py::{self, Json};
use crate::utc;

/// The tag every resource of a Round carries, with the Round's ID.
///
/// It and [`EXPIRY_TAG`] are `udpstp-bench`'s, hard-coded here, in a crate
/// that otherwise knows nothing of udpstp, until the switch: the Python
/// tool writes the same, and each tool must reap the other's expired
/// Rounds.
pub const TAG: &str = "udpstp-bench";
/// The tag with the Round's expiry, Unix time in whole seconds, which the
/// reaper reads.
pub const EXPIRY_TAG: &str = "udpstp-bench-expiry";
/// The states in which an instance still costs or holds its security
/// group: a terminated one stays listed for about an hour.
pub const LIVE_STATES: &str = "pending,running,stopping,stopped,shutting-down";
/// Canonical's account, whose images the Round launches: public, and in
/// the policy the Round's identity has.
pub const UBUNTU_OWNER: &str = "099720109477";
/// Ubuntu 24.04 LTS, Canonical's own images.
pub const UBUNTU_NAME: &str = "ubuntu/images/hvm-ssd-gp3/ubuntu-noble-24.04-amd64-server-*";
/// After a Round Teardown that left nothing, its resources are listed
/// again this long after, since a list can lag behind what was just
/// created.
pub const RECHECK: Duration = Duration::from_secs(30);
/// How long one call may take.
pub const CALL_LIMIT: Duration = Duration::from_secs(300);

/// When a failed call is tried again: how many attempts in all, and the
/// words in its error that make it worth one more.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Retry {
    pub attempts: u32,
    pub on: &'static [&'static str],
}

/// Most calls: throttled ones, six attempts.
pub const RETRY: Retry = Retry {
    attempts: 6,
    on: &["RequestLimitExceeded", "Throttling"],
};

/// `run-instances`: also one that timed out, which its client token makes
/// safe to try again.
///
/// As in the Python (`aws.py:297`), "timed out" probably never matches:
/// the aws CLI v2's timeouts read "Read timeout on endpoint URL" and
/// "Connect timeout on endpoint URL". So a launch that times out ends the
/// Round in both tools until G shows the real text, and then both change
/// together.
pub const RETRY_LAUNCH: Retry = Retry {
    attempts: 6,
    on: &["RequestLimitExceeded", "Throttling", "timed out"],
};

/// `delete-security-group`: while an interface still holds the group, ten
/// attempts.
pub const RETRY_DELETE_GROUP: Retry = Retry {
    attempts: 10,
    on: &["DependencyViolation", "RequestLimitExceeded"],
};

impl Retry {
    /// After attempt `attempt` (from 0) failed with `error`: how many
    /// seconds to wait before the next, or `None` where it was the last or
    /// the error isn't worth another.
    pub fn again(&self, attempt: u32, error: &AwsError) -> Option<u64> {
        let worth = self.on.iter().any(|word| error.0.contains(word));
        (attempt + 1 < self.attempts && worth).then(|| backoff(attempt))
    }
}

/// `min(2 ** attempt, 30)`: the seconds before attempt `attempt + 1`.
pub fn backoff(attempt: u32) -> u64 {
    2u64.checked_pow(attempt).map_or(30, |s| s.min(30))
}

fn words(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

/// The argv the CLI runs for a call: the CLI (its program and anything
/// before the call, such as a test's fake), the call's `args`, then the
/// region and JSON output.
pub fn argv(cli: &[String], args: &[String], region: &str) -> Vec<String> {
    let mut argv = cli.to_vec();
    argv.extend_from_slice(args);
    argv.extend(words(&["--region", region, "--output", "json"]));
    argv
}

/// What a finished call gave, as the Python's `aws()` reads it: where it
/// failed (a status other than 0, or none where a signal ended it), an
/// error with the call's first two words and its stderr stripped; `{}` for
/// no output; else its output as JSON.
pub fn read_answer(
    args: &[String],
    status: Option<i32>,
    stdout: &str,
    stderr: &str,
) -> Result<Json, CallError> {
    if status != Some(0) {
        return Err(CallError::Aws(AwsError(format!(
            "aws {}: {}",
            args.iter().take(2).cloned().collect::<Vec<_>>().join(" "),
            py::strip(stderr)
        ))));
    }
    if py::strip(stdout).is_empty() {
        return Ok(Json::Dict(IndexMap::new()));
    }
    Json::parse(stdout).map_err(|e| {
        CallError::Broken(format!(
            "aws {}: its output: {e}",
            args.iter().take(2).cloned().collect::<Vec<_>>().join(" ")
        ))
    })
}

/// `_tags(round_id, expiry, resource, name)`: a resource's tags, given in
/// the call that creates it: the Round's ID, its expiry in whole seconds,
/// and a name.
pub fn tags(round_id: &str, expiry: i64, resource: &str, name: &str) -> Json {
    let tag = |key: &str, value: &str| {
        Json::Dict(IndexMap::from([
            ("Key".to_string(), Json::Str(key.to_string())),
            ("Value".to_string(), Json::Str(value.to_string())),
        ]))
    };
    Json::Dict(IndexMap::from([
        ("ResourceType".to_string(), Json::Str(resource.to_string())),
        (
            "Tags".to_string(),
            Json::List(vec![
                tag(TAG, round_id),
                tag(EXPIRY_TAG, &expiry.to_string()),
                tag("Name", name),
            ]),
        ),
    ]))
}

/// The three kinds of resource an instance's launch tags.
const LAUNCHED: [&str; 3] = ["instance", "volume", "network-interface"];

fn launch_tags(round_id: &str, expiry: i64, name: &str) -> String {
    py::dumps(&Json::List(
        LAUNCHED
            .iter()
            .map(|r| tags(round_id, expiry, r, name))
            .collect(),
    ))
}

/// `ingress_rules(home_cidr, group_id, home_udp)`: the security group's
/// rules. SSH from home; UDP from the Round's own hosts and, with
/// `home_udp` (a Round with Tests from home), from home. Never the whole
/// internet: the home address must be one IPv4 address with `/32`, which
/// the Python checks only by its suffix.
pub fn ingress_rules(home_cidr: &str, group_id: &str, home_udp: bool) -> Result<Json, AwsError> {
    let one_address = home_cidr
        .split_once('/')
        .is_some_and(|(address, bits)| bits == "32" && address.parse::<Ipv4Addr>().is_ok());
    if !one_address {
        return Err(AwsError(format!(
            "the home address must be one IPv4 address (/32), not {home_cidr}"
        )));
    }
    let s = |text: &str| Json::Str(text.to_string());
    let pair = |key: &str, id: &str, description: &str| {
        Json::Dict(IndexMap::from([
            (key.to_string(), s(id)),
            ("Description".to_string(), s(description)),
        ]))
    };
    // A rule of `protocol` on ports `from..=to`, for the peers under `key`.
    let rule = |protocol: &str, from: i128, to: i128, key: &str, peer: Json| {
        Json::Dict(IndexMap::from([
            ("IpProtocol".to_string(), s(protocol)),
            ("FromPort".to_string(), Json::Int(from)),
            ("ToPort".to_string(), Json::Int(to)),
            (key.to_string(), Json::List(vec![peer])),
        ]))
    };
    let mut rules = vec![rule(
        "tcp",
        22,
        22,
        "IpRanges",
        pair("CidrIp", home_cidr, "SSH from home"),
    )];
    if home_udp {
        rules.push(rule(
            "udp",
            1024,
            65535,
            "IpRanges",
            pair("CidrIp", home_cidr, "Tests from home"),
        ));
    }
    rules.push(rule(
        "udp",
        1024,
        65535,
        "UserIdGroupPairs",
        pair("GroupId", group_id, "Tests between Round hosts"),
    ));
    Ok(Json::List(rules))
}

/// `sts get-caller-identity`: whose credentials these are.
pub fn get_caller_identity() -> Vec<String> {
    words(&["sts", "get-caller-identity"])
}

/// `configure export-credentials --format process`: the credentials,
/// read only for when they expire.
pub fn export_credentials() -> Vec<String> {
    words(&["configure", "export-credentials", "--format", "process"])
}

/// Canonical's available Ubuntu 24.04 images.
pub fn describe_images() -> Vec<String> {
    words(&[
        "ec2",
        "describe-images",
        "--owners",
        UBUNTU_OWNER,
        "--filters",
        &format!("Name=name,Values={UBUNTU_NAME}"),
        "Name=state,Values=available",
    ])
}

/// The zone's default subnet.
pub fn describe_subnets(zone: &str) -> Vec<String> {
    words(&[
        "ec2",
        "describe-subnets",
        "--filters",
        "Name=default-for-az,Values=true",
        &format!("Name=availability-zone,Values={zone}"),
    ])
}

/// The Round's security group, tagged as it is made.
pub fn create_security_group(round_id: &str, expiry: i64, vpc_id: &str) -> Vec<String> {
    let name = format!("{TAG}-{round_id}");
    words(&[
        "ec2",
        "create-security-group",
        "--group-name",
        &name,
        "--description",
        &format!("udpstp benchmark round {round_id}"),
        "--vpc-id",
        vpc_id,
        "--tag-specifications",
        &py::dumps(&Json::List(vec![tags(
            round_id,
            expiry,
            "security-group",
            &name,
        )])),
    ])
}

/// The group's rules ([`ingress_rules`]).
pub fn authorize_security_group_ingress(group_id: &str, rules: &Json) -> Vec<String> {
    words(&[
        "ec2",
        "authorize-security-group-ingress",
        "--group-id",
        group_id,
        "--ip-permissions",
        &py::dumps(rules),
    ])
}

/// What one host of a Round is launched as.
#[derive(Clone, Copy, Debug)]
pub struct Launch<'a> {
    pub round_id: &'a str,
    pub role: &'a str,
    pub expiry: i64,
    pub image_id: &'a str,
    pub host_type: &'a str,
    pub subnet_id: &'a str,
    pub group_id: &'a str,
    /// The user data, base64 ([`user_data_base64`]).
    pub user_data: &'a str,
}

/// One host of the Round: tagged as it is made (the instance, its volume
/// and its interface), a client token of the Round and role so that a
/// retry after a timeout gives the same instance, terminated when it
/// shuts down, IMDSv2 only, and the user data that arms its dead-man
/// switch.
pub fn run_instances(launch: &Launch) -> Vec<String> {
    let name = format!("{TAG}-{}-{}", launch.round_id, launch.role);
    words(&[
        "ec2",
        "run-instances",
        "--image-id",
        launch.image_id,
        "--instance-type",
        launch.host_type,
        "--count",
        "1",
        "--subnet-id",
        launch.subnet_id,
        "--security-group-ids",
        launch.group_id,
        "--client-token",
        &format!("{}-{}", launch.round_id, launch.role),
        "--instance-initiated-shutdown-behavior",
        "terminate",
        "--metadata-options",
        "HttpTokens=required",
        "--user-data",
        launch.user_data,
        "--tag-specifications",
        &launch_tags(launch.round_id, launch.expiry, &name),
    ])
}

/// Waits (`instance-running`, `instance-terminated`) for the instances.
pub fn wait(waiter: &str, ids: &[String]) -> Vec<String> {
    let mut argv = words(&["ec2", "wait", waiter, "--instance-ids"]);
    argv.extend_from_slice(ids);
    argv
}

/// The filter of Round `round_id`'s resources.
pub fn round_filter(round_id: &str) -> String {
    format!("Name=tag:{TAG},Values={round_id}")
}

/// The filter of every Round's resources, the reaper's.
pub fn any_round_filter() -> String {
    format!("Name=tag-key,Values={TAG}")
}

/// The live instances that `filters` take.
pub fn describe_instances(filters: &[String]) -> Vec<String> {
    let mut argv = words(&["ec2", "describe-instances", "--filters"]);
    argv.extend_from_slice(filters);
    argv.push(format!("Name=instance-state-name,Values={LIVE_STATES}"));
    argv
}

/// The security groups that `filters` take.
pub fn describe_security_groups(filters: &[String]) -> Vec<String> {
    let mut argv = words(&["ec2", "describe-security-groups", "--filters"]);
    argv.extend_from_slice(filters);
    argv
}

pub fn terminate_instances(ids: &[String]) -> Vec<String> {
    let mut argv = words(&["ec2", "terminate-instances", "--instance-ids"]);
    argv.extend_from_slice(ids);
    argv
}

pub fn delete_security_group(group_id: &str) -> Vec<String> {
    words(&["ec2", "delete-security-group", "--group-id", group_id])
}

/// The permission check's security group, with `--dry-run`: nothing is
/// made.
pub fn check_create_security_group(round_id: &str, expiry: i64, vpc_id: &str) -> Vec<String> {
    let name = format!("{TAG}-{round_id}");
    words(&[
        "ec2",
        "create-security-group",
        "--dry-run",
        "--group-name",
        &name,
        "--description",
        "udpstp permission check",
        "--vpc-id",
        vpc_id,
        "--tag-specifications",
        &py::dumps(&Json::List(vec![tags(
            round_id,
            expiry,
            "security-group",
            &name,
        )])),
    ])
}

/// The permission check's launch of one instance type, with `--dry-run`.
pub fn check_run_instances(
    round_id: &str,
    expiry: i64,
    image_id: &str,
    host_type: &str,
    subnet_id: &str,
) -> Vec<String> {
    let name = format!("{TAG}-{round_id}");
    words(&[
        "ec2",
        "run-instances",
        "--dry-run",
        "--image-id",
        image_id,
        "--instance-type",
        host_type,
        "--count",
        "1",
        "--subnet-id",
        subnet_id,
        "--instance-initiated-shutdown-behavior",
        "terminate",
        "--tag-specifications",
        &launch_tags(round_id, expiry, &name),
    ])
}

/// `max(1, int((expiry - now) / 60))`: the minutes until the first boot's
/// power-off.
pub fn minutes_left(expiry: f64, now: f64) -> i64 {
    let minutes = ((expiry - now) / 60.0).trunc();
    // A NaN goes to 0, then to 1; Python would raise.
    (minutes as i64).max(1)
}

/// The user data (`dead_man::user_data`) as `run-instances` takes it:
/// base64 of its UTF-8.
pub fn user_data_base64(minutes: i64, public_key: &str, deadline: i64) -> String {
    base64::encode(dead_man::user_data(minutes, public_key, deadline).as_bytes())
}

/// Whether credentials that expire at `expiration` (the CLI's ISO 8601)
/// outlast a Round of `max_hours` from `now`, with half an hour to spare;
/// the Python's refusal where they don't.
pub fn credentials_outlast(expiration: &str, now: f64, max_hours: f64) -> Result<(), CallError> {
    let expires = utc::timestamp(expiration)
        .map_err(|e| CallError::Broken(format!("the credentials' expiry: {e}")))?;
    let left = expires - now;
    if left < (max_hours + 0.5) * 3600.0 {
        return Err(CallError::Aws(AwsError(format!(
            "the credentials expire in {} h, before the round could end: sign in again (aws sso login)",
            crate::num::fixed(left / 3600.0, 1)
        ))));
    }
    Ok(())
}

/// `creds.get("Expiration")` of `export-credentials`' answer: `None` where
/// it says nothing or nothing true.
pub fn expiration(credentials: &Json) -> Result<Option<String>, CallError> {
    let found = credentials
        .get("Expiration")
        .map_err(|e| CallError::Broken(format!("the credentials: {e}")))?;
    match found {
        Json::Str(s) if !s.is_empty() => Ok(Some(s.clone())),
        other if !other.truthy() => Ok(None),
        other => Err(CallError::Broken(format!(
            "the credentials' expiry is a {}",
            other.type_name()
        ))),
    }
}

/// `image()`'s choice among `describe-images`' answer: the latest by
/// `CreationDate`, the last of equals.
pub fn latest_image(answer: &Json) -> Result<Json, CallError> {
    let empty = Json::List(Vec::new());
    let images = answer
        .get_or("Images", &empty)
        .map_err(broken("describe-images"))?;
    let images = images
        .as_list()
        .ok_or_else(|| CallError::Broken("aws ec2 describe-images: Images isn't a list".into()))?;
    let mut dated = Vec::new();
    for image in images {
        let date = image
            .item("CreationDate")
            .map_err(broken("describe-images"))?;
        let date = date.as_str().ok_or_else(|| {
            CallError::Broken("aws ec2 describe-images: a CreationDate isn't a string".into())
        })?;
        dated.push((date, image));
    }
    // Stable, as sorted() is: of equal dates the last in the answer is last.
    dated.sort_by(|a, b| a.0.cmp(b.0));
    dated
        .last()
        .map(|(_, image)| (*image).clone())
        .ok_or_else(|| CallError::Aws(AwsError("no Ubuntu 24.04 image in this region".into())))
}

/// The check that an image's volumes go with the instance
/// (`DeleteOnTermination` on each of its EBS mappings).
pub fn volumes_die_with(image: &Json) -> Result<(), CallError> {
    let empty = Json::List(Vec::new());
    let mappings = image
        .get_or("BlockDeviceMappings", &empty)
        .map_err(broken("describe-images"))?;
    let mappings = mappings.as_list().ok_or_else(|| {
        CallError::Broken("aws ec2 describe-images: BlockDeviceMappings isn't a list".into())
    })?;
    let none = Json::Dict(IndexMap::new());
    for mapping in mappings {
        let Some(m) = mapping.as_dict() else {
            return Err(CallError::Broken(
                "aws ec2 describe-images: a mapping isn't a dict".into(),
            ));
        };
        if !m.contains_key("Ebs") {
            continue;
        }
        let ebs = mapping
            .get_or("Ebs", &none)
            .map_err(broken("describe-images"))?;
        let dies = ebs
            .get_or("DeleteOnTermination", &Json::Bool(false))
            .map_err(broken("describe-images"))?;
        if !dies.truthy() {
            let id = image.item("ImageId").map_err(broken("describe-images"))?;
            let id = id.as_str().map_or_else(|| py::dumps(id), str::to_string);
            return Err(CallError::Aws(AwsError(format!(
                "{id}: a volume outlives its instance (DeleteOnTermination)"
            ))));
        }
    }
    Ok(())
}

/// The commands that delete what a Round left, by hand.
pub fn manual_cleanup(region: &str, round_id: &str) -> Vec<String> {
    vec![
        format!(
            "aws ec2 describe-instances --region {region} --filters Name=tag:{TAG},Values={round_id} \
             Name=instance-state-name,Values={LIVE_STATES} --query 'Reservations[].Instances[].InstanceId'"
        ),
        format!("aws ec2 terminate-instances --region {region} --instance-ids <IDs above>"),
        format!(
            "aws ec2 describe-security-groups --region {region} --filters Name=tag:{TAG},Values={round_id} \
             --query 'SecurityGroups[].GroupId'"
        ),
        format!("aws ec2 delete-security-group --region {region} --group-id <ID above>"),
    ]
}

/// `sealed_env(path)`: an environment in which no real AWS credentials or
/// configuration can be found, for tests: `vars` without any `AWS_`
/// variable, the CLI's two files the null device, and `path` as `PATH`.
pub fn sealed_env(
    vars: impl IntoIterator<Item = (String, String)>,
    path: &str,
) -> IndexMap<String, String> {
    let null = if cfg!(windows) { "nul" } else { "/dev/null" };
    let mut env: IndexMap<String, String> = vars
        .into_iter()
        .filter(|(k, _)| !k.starts_with("AWS_"))
        .collect();
    env.insert("AWS_CONFIG_FILE".into(), null.into());
    env.insert("AWS_SHARED_CREDENTIALS_FILE".into(), null.into());
    env.insert("PATH".into(), path.into());
    env
}

/// A shape the provider can't read, from the call `what`.
pub(super) fn broken(what: &'static str) -> impl Fn(py::PyError) -> CallError {
    move |e| CallError::Broken(format!("aws ec2 {what}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_security_group_never_admits_the_internet() {
        // tests/test_round.py's Rules.
        let rules = ingress_rules("203.0.113.7/32", "sg-1", true).unwrap();
        let text = py::dumps(&rules);
        assert!(!text.contains("0.0.0.0/0") && !text.contains("::/0"));
        assert_eq!(
            text,
            "[{\"IpProtocol\": \"tcp\", \"FromPort\": 22, \"ToPort\": 22, \"IpRanges\": [{\"CidrIp\": \"203.0.113.7/32\", \
             \"Description\": \"SSH from home\"}]}, {\"IpProtocol\": \"udp\", \"FromPort\": 1024, \"ToPort\": 65535, \
             \"IpRanges\": [{\"CidrIp\": \"203.0.113.7/32\", \"Description\": \"Tests from home\"}]}, {\"IpProtocol\": \
             \"udp\", \"FromPort\": 1024, \"ToPort\": 65535, \"UserIdGroupPairs\": [{\"GroupId\": \"sg-1\", \
             \"Description\": \"Tests between Round hosts\"}]}]"
        );
        for cidr in [
            "0.0.0.0/0",
            "::/0",
            "203.0.113.0/24",
            "203.0.113.7",
            "2001:db8::1/32",
            "203.0.113.07/32",
            "home/32",
        ] {
            assert!(ingress_rules(cidr, "sg-1", true).is_err(), "{cidr}");
        }
        // UDP from home only for a Round with Tests from home.
        let rules = py::dumps(&ingress_rules("203.0.113.7/32", "sg-1", false).unwrap());
        assert!(!rules.contains("Tests from home"));
    }

    #[test]
    fn every_resource_is_tagged_in_the_call_that_makes_it() {
        let argv = create_security_group("r1", 1_790_007_200, "vpc-1");
        assert_eq!(
            argv.last().unwrap(),
            "[{\"ResourceType\": \"security-group\", \"Tags\": [{\"Key\": \"udpstp-bench\", \"Value\": \"r1\"}, \
             {\"Key\": \"udpstp-bench-expiry\", \"Value\": \"1790007200\"}, {\"Key\": \"Name\", \"Value\": \
             \"udpstp-bench-r1\"}]}]"
        );
        let launch = Launch {
            round_id: "r1",
            role: "server",
            expiry: 1_790_007_200,
            image_id: "ami-1",
            host_type: "c6in.large",
            subnet_id: "subnet-1",
            group_id: "sg-1",
            user_data: "IyEvYmluL2Jhc2gK",
        };
        let argv = run_instances(&launch);
        let at = |flag: &str| &argv[argv.iter().position(|a| a == flag).unwrap() + 1];
        assert_eq!(at("--client-token"), "r1-server");
        assert_eq!(at("--instance-initiated-shutdown-behavior"), "terminate");
        assert_eq!(at("--metadata-options"), "HttpTokens=required");
        let specs = Json::parse(at("--tag-specifications")).unwrap();
        let kinds: Vec<&Json> = specs
            .as_list()
            .unwrap()
            .iter()
            .map(|s| s.item("ResourceType").unwrap())
            .collect();
        assert_eq!(
            kinds,
            [
                &Json::Str("instance".into()),
                &Json::Str("volume".into()),
                &Json::Str("network-interface".into())
            ]
        );
    }

    #[test]
    fn a_failed_call_says_its_two_words_and_its_error() {
        let args = words(&["ec2", "run-instances", "--image-id", "ami-1"]);
        assert_eq!(
            read_answer(
                &args,
                Some(254),
                "",
                "\nAn error occurred (Throttling) when calling\n"
            ),
            Err(CallError::Aws(AwsError(
                "aws ec2 run-instances: An error occurred (Throttling) when calling".into()
            )))
        );
        assert_eq!(
            read_answer(&args, Some(0), " \n", ""),
            Ok(Json::Dict(IndexMap::new()))
        );
        assert!(matches!(
            read_answer(&args, Some(0), "not json", ""),
            Err(CallError::Broken(_))
        ));
        assert!(matches!(
            read_answer(&args, None, "", ""),
            Err(CallError::Aws(_))
        ));
    }

    #[test]
    fn retries_back_off_on_the_errors_worth_it() {
        let throttled = AwsError("aws ec2 x: An error occurred (RequestLimitExceeded)".into());
        let other = AwsError("aws ec2 x: An error occurred (InvalidParameter)".into());
        let waits: Vec<Option<u64>> = (0..6).map(|a| RETRY.again(a, &throttled)).collect();
        assert_eq!(waits, [Some(1), Some(2), Some(4), Some(8), Some(16), None]);
        assert_eq!(RETRY.again(0, &other), None);
        assert_eq!(
            RETRY_DELETE_GROUP.again(5, &AwsError("DependencyViolation".into())),
            Some(30)
        );
        assert_eq!(
            RETRY_LAUNCH.again(0, &AwsError("the read operation timed out".into())),
            Some(1)
        );
        assert_eq!(RETRY.again(0, &AwsError("timed out".into())), None);
        assert_eq!(backoff(40), 30);
    }

    #[test]
    fn credentials_must_outlast_the_round() {
        let now = 1_790_000_000.0;
        assert_eq!(
            credentials_outlast("2099-01-01T00:00:00Z", now, 2.0),
            Ok(())
        );
        assert_eq!(
            credentials_outlast("2026-09-21T15:00:00Z", now, 2.0),
            Err(CallError::Aws(AwsError(
                "the credentials expire in 0.8 h, before the round could end: sign in again (aws sso login)"
                    .into()
            )))
        );
        assert_eq!(
            expiration(&Json::parse(r#"{"Expiration": ""}"#).unwrap()),
            Ok(None)
        );
        assert!(expiration(&Json::parse(r#"{"Expiration": 5}"#).unwrap()).is_err());
    }

    #[test]
    fn the_latest_image_is_chosen_and_its_volumes_must_die_with_it() {
        let answer = Json::parse(
            r#"{"Images": [{"ImageId": "ami-b", "CreationDate": "2026-09-02"},
                           {"ImageId": "ami-a", "CreationDate": "2026-09-01"},
                           {"ImageId": "ami-c", "CreationDate": "2026-09-02",
                            "BlockDeviceMappings": [{"DeviceName": "/dev/sda1"},
                                                    {"Ebs": {"DeleteOnTermination": false}}]}]}"#,
        )
        .unwrap();
        let image = latest_image(&answer).unwrap();
        assert_eq!(image.item("ImageId").unwrap(), &Json::Str("ami-c".into()));
        assert_eq!(
            volumes_die_with(&image),
            Err(CallError::Aws(AwsError(
                "ami-c: a volume outlives its instance (DeleteOnTermination)".into()
            )))
        );
        assert_eq!(
            latest_image(&Json::parse("{}").unwrap()),
            Err(CallError::Aws(AwsError(
                "no Ubuntu 24.04 image in this region".into()
            )))
        );
    }

    #[test]
    fn the_first_boot_powers_off_in_whole_minutes_at_least_one() {
        assert_eq!(minutes_left(1_000_000.0 + 7200.0, 1_000_000.0), 120);
        assert_eq!(minutes_left(1_000_000.0 + 119.9, 1_000_000.0), 1);
        assert_eq!(minutes_left(1_000_000.0 - 600.0, 1_000_000.0), 1);
        let data = user_data_base64(120, "ssh-ed25519 AAAA t", 1_790_018_000);
        let text = String::from_utf8(base64::decode(&data).unwrap()).unwrap();
        assert!(text.starts_with("#!/bin/bash\nshutdown -P +120\n"));
    }

    #[test]
    fn the_environment_is_sealed() {
        let env = sealed_env(
            [
                ("AWS_PROFILE".to_string(), "x".to_string()),
                ("AWS_ACCESS_KEY_ID".to_string(), "x".to_string()),
                ("HOME".to_string(), "/home/u".to_string()),
            ],
            "/usr/bin:/bin",
        );
        let aws: Vec<&String> = env.keys().filter(|k| k.starts_with("AWS_")).collect();
        assert_eq!(aws, ["AWS_CONFIG_FILE", "AWS_SHARED_CREDENTIALS_FILE"]);
        assert_eq!(env["PATH"], "/usr/bin:/bin");
        assert_eq!(env["HOME"], "/home/u");
    }
}
