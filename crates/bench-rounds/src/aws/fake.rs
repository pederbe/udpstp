//! The fake `aws` CLI's model of EC2 (`scripts/bench/tests/fake_aws.py`),
//! as a library: a call's argv and the state in, what the CLI would print
//! and its exit status out, the state changed in place. The `fake-aws`
//! binary keeps the state in the file `FAKE_AWS_STATE` names, as the
//! Python's fake does, in the same form, so that the Python's tests and
//! provider can run against it too; tests in this workspace can use the
//! model in-process ([`Local`]).
//!
//! It never reaches AWS. It records every call in the state's `calls`,
//! and keeps instances (`i-1`, `i-2`, ...: public address `198.51.100.N`,
//! private `10.0.0.N`) and security groups (`sg-1`, ...) with their tags
//! and rules. What a test can set in the state's `config`, as in the
//! Python's fake:
//!
//! - `expiration`: what `configure export-credentials` reports, or none;
//! - `credentials_without_expiration`: a successful export without expiry;
//! - `delete_on_termination`: the image's root volume setting (true);
//! - `dependency_violations`: how many times `delete-security-group`
//!   fails first;
//! - `throttle_run_instances`: how many times `run-instances` is
//!   throttled first;
//! - `leave_instance`: `terminate-instances` leaves the instance whose
//!   name ends so running;
//! - `unauthorized`: a dry run whose argv holds this word is refused.
//!
//! And two of the Rust fake's own, for comparing the tools' orchestration
//! under failure, where
//! `command` is an operation such as `run-instances` and `n` counts its
//! calls from 1, dry runs aside:
//!
//! - `fail_call`, `{"command", "n", "code"}`: its `n`-th call and every
//!   later one fail with the error `code` (`InsufficientInstanceCapacity`
//!   unless given): for good, or, with `RequestLimitExceeded`, throttled
//!   past its retries;
//! - `pause_call`, `{"command", "n", "until"}`: its `n`-th call does what
//!   it does and the state is saved, then the binary waits until the file
//!   `until` exists before it answers.
//!
//! Where the Python's fake crashes on a call it can't answer (an unknown
//! instance or group, a flag without its value), this answers with an
//! error of its own ([`BadCall`]), and the state is left as it was.

use std::fmt;

use indexmap::IndexMap;

use super::plan;
use super::{AwsError, CallError, Calls};
use crate::py::{self, Json};

/// What the fake CLI prints and how it exits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer {
    pub stdout: String,
    pub stderr: String,
    pub status: u8,
    /// A file to wait for before answering (`pause_call`).
    pub pause_until: Option<String>,
}

impl Answer {
    fn out(out: Json) -> Answer {
        Answer {
            stdout: py::dumps(&out) + "\n",
            stderr: String::new(),
            status: 0,
            pause_until: None,
        }
    }

    fn fail(message: &str) -> Answer {
        Answer {
            stdout: String::new(),
            stderr: format!("{message}\n"),
            status: 254,
            pause_until: None,
        }
    }
}

/// A call or a state the fake can't answer: the Python's fake would
/// crash, and the state isn't saved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BadCall(pub String);

impl fmt::Display for BadCall {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "fake aws: {}", self.0)
    }
}

impl std::error::Error for BadCall {}

fn bad(message: impl Into<String>) -> BadCall {
    BadCall(message.into())
}

type Dict = IndexMap<String, Json>;

fn dict_at<'a>(top: &'a mut Dict, key: &str) -> Result<&'a mut Dict, BadCall> {
    match top.get_mut(key) {
        Some(Json::Dict(d)) => Ok(d),
        _ => Err(bad(format!("the state's {key} aren't a dict"))),
    }
}

fn s(text: &str) -> Json {
    Json::Str(text.to_string())
}

fn dict(pairs: Vec<(&str, Json)>) -> Json {
    Json::Dict(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

fn rule_descriptions_allowed(value: &Json) -> bool {
    let Some(rules) = value.as_list() else {
        return false;
    };
    rules.iter().all(|rule| {
        let Some(rule) = rule.as_dict() else {
            return false;
        };
        [
            "IpRanges",
            "Ipv6Ranges",
            "UserIdGroupPairs",
            "PrefixListIds",
        ]
        .iter()
        .all(|source| {
            let Some(peers) = rule.get(*source) else {
                return true;
            };
            let Some(peers) = peers.as_list() else {
                return false;
            };
            peers.iter().all(|peer| {
                let Some(peer) = peer.as_dict() else {
                    return false;
                };
                match peer.get("Description") {
                    None => true,
                    Some(Json::Str(text)) => {
                        text.len() <= 255
                            && text.bytes().all(|c| {
                                c.is_ascii_alphanumeric()
                                    || b" ._-:/()#,@[]+=;{}!$*".contains(&c)
                                    || (c == b'&' && matches!(*source, "IpRanges" | "Ipv6Ranges"))
                            })
                    }
                    _ => false,
                }
            })
        })
    })
}

/// One call of the fake CLI: `argv`, what follows `aws`, against `state`,
/// the state file's JSON, which it changes as the call does.
pub fn call(state: &mut Json, argv: &[String]) -> Result<Answer, BadCall> {
    let Json::Dict(top) = state else {
        return Err(bad("the state isn't a dict"));
    };
    // `setdefault`s in the Python's order, which is the file's.
    let calls = top
        .entry("calls".to_string())
        .or_insert_with(|| Json::List(Vec::new()));
    let Json::List(calls) = calls else {
        return Err(bad("the state's calls aren't a list"));
    };
    calls.push(Json::List(argv.iter().map(|a| s(a)).collect()));
    for key in ["config", "instances", "groups"] {
        let found = top
            .entry(key.to_string())
            .or_insert_with(|| Json::Dict(IndexMap::new()));
        if !matches!(found, Json::Dict(_)) {
            return Err(bad(format!("the state's {key} aren't a dict")));
        }
    }
    let mut config = dict_at(top, "config")?.clone();
    let answer = answer(top, &mut config, argv)?;
    *dict_at(top, "config")? = config;
    Ok(answer)
}

/// `arg(name)`: the word after `name`.
fn arg<'a>(argv: &'a [String], name: &str) -> Result<Option<&'a str>, BadCall> {
    match argv.iter().position(|a| a == name) {
        None => Ok(None),
        Some(at) => argv
            .get(at + 1)
            .map(|v| Some(v.as_str()))
            .ok_or_else(|| bad(format!("{name} has no value"))),
    }
}

/// `values(name)`: the words after `name`, up to the next flag.
fn values<'a>(argv: &'a [String], name: &str) -> Vec<&'a str> {
    match argv.iter().position(|a| a == name) {
        None => Vec::new(),
        Some(at) => argv[at + 1..]
            .iter()
            .take_while(|a| !a.starts_with("--"))
            .map(String::as_str)
            .collect(),
    }
}

/// A resource's tags, which the Python's fake reads as a dict.
fn tags_of(resource: &Json) -> Result<&Dict, BadCall> {
    resource
        .item("tags")
        .map_err(|e| bad(e.to_string()))?
        .as_dict()
        .ok_or_else(|| bad("a resource's tags aren't a dict"))
}

/// `matches(tags, filters)`: whether the tags pass every `Name=tag:K` and
/// `Name=tag-key` filter; other filters pass.
fn matches(tags: &Dict, filters: &[&str]) -> bool {
    filters.iter().all(|filter| {
        let (name, vals) = filter.split_once(",Values=").unwrap_or((filter, ""));
        let name = name.strip_prefix("Name=").unwrap_or(name);
        if let Some(key) = name.strip_prefix("tag:") {
            tags.get(key)
                .and_then(Json::as_str)
                .is_some_and(|value| vals.split(',').any(|v| v == value))
        } else if name == "tag-key" {
            tags.contains_key(vals)
        } else {
            true
        }
    })
}

/// `{t["Key"]: t["Value"] for t in specs[0]["Tags"]}` of a JSON
/// `--tag-specifications`.
fn first_tags(argv: &[String]) -> Result<Json, BadCall> {
    let text = arg(argv, "--tag-specifications")?.ok_or_else(|| bad("no --tag-specifications"))?;
    let specs = Json::parse(text).map_err(|e| bad(format!("--tag-specifications: {e}")))?;
    let tags = specs
        .at(0)
        .and_then(|spec| spec.item("Tags"))
        .map_err(|e| bad(format!("--tag-specifications: {e}")))?;
    let mut found = IndexMap::new();
    for tag in tags.as_list().ok_or_else(|| bad("Tags isn't a list"))? {
        let key = tag.item("Key").map_err(|e| bad(e.to_string()))?;
        let value = tag.item("Value").map_err(|e| bad(e.to_string()))?;
        let key = key
            .as_str()
            .ok_or_else(|| bad("a tag's key isn't a string"))?;
        found.insert(key.to_string(), value.clone());
    }
    Ok(Json::Dict(found))
}

/// `[{"Key": k, "Value": v}, ...]` of a resource's tags.
fn tag_list(tags: &Dict) -> Json {
    Json::List(
        tags.iter()
            .map(|(k, v)| dict(vec![("Key", s(k)), ("Value", v.clone())]))
            .collect(),
    )
}

/// A number in the config, as the Python compares one with 0.
fn count(config: &Dict, key: &str) -> Result<Json, BadCall> {
    match config.get(key) {
        None => Ok(Json::Int(0)),
        Some(n @ (Json::Int(_) | Json::Float(_))) => Ok(n.clone()),
        Some(other) => Err(bad(format!("config {key} is a {}", other.type_name()))),
    }
}

fn positive(n: &Json) -> bool {
    match n {
        Json::Int(i) => *i > 0,
        Json::Float(f) => *f > 0.0,
        _ => false,
    }
}

fn less_one(n: &Json) -> Json {
    match n {
        Json::Int(i) => Json::Int(i - 1),
        Json::Float(f) => Json::Float(f - 1.0),
        other => other.clone(),
    }
}

/// How many calls of operation `command`, dry runs aside, the state has
/// recorded, this one among them.
fn calls_of(top: &Dict, command: &str) -> usize {
    let Some(Json::List(calls)) = top.get("calls") else {
        return 0;
    };
    calls
        .iter()
        .filter_map(Json::as_list)
        .filter(|call| {
            call.get(1).and_then(Json::as_str) == Some(command)
                && !call.iter().any(|a| a.as_str() == Some("--dry-run"))
        })
        .count()
}

/// A `fail_call` or `pause_call` of the config that takes this call: its
/// settings.
fn nth_call<'a>(top: &Dict, config: &'a Dict, key: &str, argv: &[String]) -> Option<&'a Dict> {
    let settings = config.get(key)?.as_dict()?;
    let command = settings.get("command")?.as_str()?;
    let n = match settings.get("n")? {
        Json::Int(n) => usize::try_from(*n).ok()?,
        _ => return None,
    };
    (argv.get(1).map(String::as_str) == Some(command)).then_some(())?;
    let seen = calls_of(top, command);
    let takes = if key == "fail_call" {
        seen >= n
    } else {
        seen == n
    };
    takes.then_some(settings)
}

/// `run-instances` as `RunInstances`: the operation's name in an error.
fn operation(command: &str) -> String {
    command
        .split('-')
        .map(|w| {
            let mut chars = w.chars();
            chars
                .next()
                .map_or_else(String::new, |c| c.to_uppercase().chain(chars).collect())
        })
        .collect()
}

fn answer(top: &mut Dict, config: &mut Dict, argv: &[String]) -> Result<Answer, BadCall> {
    let cmd: Vec<&str> = argv.iter().take(2).map(String::as_str).collect();
    if argv.iter().any(|a| a == "--dry-run") {
        let refused = config
            .get("unauthorized")
            .and_then(Json::as_str)
            .is_some_and(|word| argv.iter().any(|a| a == word));
        if refused {
            return Ok(Answer::fail(
                "An error occurred (UnauthorizedOperation) when calling the operation: \
                 You are not authorized to perform this operation.",
            ));
        }
        return Ok(Answer::fail(
            "An error occurred (DryRunOperation) when calling the operation: \
             Request would have succeeded, but DryRun flag is set.",
        ));
    }
    if let Some(fail) = nth_call(top, config, "fail_call", argv) {
        let code = fail
            .get("code")
            .and_then(Json::as_str)
            .unwrap_or("InsufficientInstanceCapacity");
        return Ok(Answer::fail(&format!(
            "An error occurred ({code}) when calling the {} operation: the fake's failure for good",
            operation(cmd[1])
        )));
    }
    let pause_until = nth_call(top, config, "pause_call", argv)
        .and_then(|pause| pause.get("until"))
        .and_then(Json::as_str)
        .map(str::to_string);
    let mut answered = dispatch(top, config, argv, &cmd)?;
    answered.pause_until = pause_until;
    Ok(answered)
}

fn dispatch(
    top: &mut Dict,
    config: &mut Dict,
    argv: &[String],
    cmd: &[&str],
) -> Result<Answer, BadCall> {
    let out = match cmd {
        ["sts", "get-caller-identity"] => dict(vec![("Account", s("123456789012"))]),
        ["configure", "export-credentials"] => match config.get("expiration") {
            None | Some(Json::Null)
                if config.get("credentials_without_expiration") == Some(&Json::Bool(true)) =>
            {
                Json::Dict(IndexMap::new())
            }
            None | Some(Json::Null) => return Ok(Answer::fail("no credentials to export")),
            Some(expiration) => dict(vec![("Expiration", expiration.clone())]),
        },
        ["ec2", "describe-images"] => {
            let dies = config
                .get("delete_on_termination")
                .cloned()
                .unwrap_or(Json::Bool(true));
            dict(vec![(
                "Images",
                Json::List(vec![dict(vec![
                    ("ImageId", s("ami-1")),
                    ("CreationDate", s("2026-09-01")),
                    (
                        "BlockDeviceMappings",
                        Json::List(vec![dict(vec![(
                            "Ebs",
                            dict(vec![("DeleteOnTermination", dies)]),
                        )])]),
                    ),
                ])]),
            )])
        }
        ["ec2", "describe-subnets"] => dict(vec![(
            "Subnets",
            Json::List(vec![dict(vec![
                ("SubnetId", s("subnet-1")),
                ("VpcId", s("vpc-1")),
            ])]),
        )]),
        ["ec2", "create-security-group"] => {
            let groups = dict_at(top, "groups")?;
            let id = format!("sg-{}", groups.len() + 1);
            let tags = first_tags(argv)?;
            groups.insert(
                id.clone(),
                dict(vec![("tags", tags), ("rules", Json::List(Vec::new()))]),
            );
            dict(vec![("GroupId", Json::Str(id))])
        }
        ["ec2", "authorize-security-group-ingress"] => {
            let id = arg(argv, "--group-id")?.ok_or_else(|| bad("no --group-id"))?;
            let text = arg(argv, "--ip-permissions")?.ok_or_else(|| bad("no --ip-permissions"))?;
            let rules = Json::parse(text).map_err(|e| bad(format!("--ip-permissions: {e}")))?;
            if !rule_descriptions_allowed(&rules) {
                return Ok(Answer::fail(
                    "An error occurred (InvalidParameterValue) when calling the \
                     AuthorizeSecurityGroupIngress operation: Invalid rule description",
                ));
            }
            let group = dict_at(top, "groups")?
                .get_mut(id)
                .ok_or_else(|| bad(format!("no group {id}")))?;
            let Json::Dict(group) = group else {
                return Err(bad(format!("the group {id} isn't a dict")));
            };
            group.insert("rules".to_string(), rules);
            Json::Dict(IndexMap::new())
        }
        ["ec2", "run-instances"] => {
            let throttled = count(config, "throttle_run_instances")?;
            if positive(&throttled) {
                config.insert("throttle_run_instances".to_string(), less_one(&throttled));
                return Ok(Answer::fail(
                    "An error occurred (RequestLimitExceeded) when calling the RunInstances operation",
                ));
            }
            let token = arg(argv, "--client-token")?.map_or(Json::Null, s);
            let instances = dict_at(top, "instances")?;
            let mut same = None;
            for (id, instance) in instances.iter() {
                let theirs = instance.item("token").map_err(|e| bad(e.to_string()))?;
                if theirs == &token {
                    same = Some(id.clone());
                    break;
                }
            }
            let id = match same {
                Some(id) => id,
                None => {
                    let n = instances.len() + 1;
                    let id = format!("i-{n}");
                    let tags = first_tags(argv)?;
                    let instances = dict_at(top, "instances")?;
                    instances.insert(
                        id.clone(),
                        dict(vec![
                            ("tags", tags),
                            ("token", token),
                            ("state", s("running")),
                            ("public", Json::Str(format!("198.51.100.{n}"))),
                            ("private", Json::Str(format!("10.0.0.{n}"))),
                        ]),
                    );
                    id
                }
            };
            dict(vec![(
                "Instances",
                Json::List(vec![dict(vec![("InstanceId", Json::Str(id))])]),
            )])
        }
        ["ec2", "wait"] => Json::Dict(IndexMap::new()),
        ["ec2", "describe-instances"] => {
            let filters = values(argv, "--filters");
            let live = filters
                .iter()
                .find(|f| f.starts_with("Name=instance-state-name"));
            // `live[0].split("Values=")[1].split(",")`.
            let states: Option<Vec<&str>> = match live {
                None => None,
                Some(f) => Some(
                    f.split("Values=")
                        .nth(1)
                        .ok_or_else(|| bad(format!("{f} has no Values=")))?
                        .split(',')
                        .collect(),
                ),
            };
            let by_tag: Vec<&str> = filters
                .iter()
                .copied()
                .filter(|f| !f.starts_with("Name=instance-state"))
                .collect();
            let mut found = Vec::new();
            for (id, instance) in dict_at(top, "instances")?.iter() {
                let field = |key: &str| instance.item(key).map_err(|e| bad(e.to_string()));
                let tags = tags_of(instance)?;
                if !matches(tags, &by_tag) {
                    continue;
                }
                // Read only for the instances the tags take, as the Python
                // reads it.
                let state = field("state")?;
                if let Some(states) = &states
                    && !state.as_str().is_some_and(|st| states.contains(&st))
                {
                    continue;
                }
                found.push(dict(vec![
                    ("InstanceId", s(id)),
                    ("State", dict(vec![("Name", state.clone())])),
                    ("Tags", tag_list(tags)),
                    ("PublicIpAddress", field("public")?.clone()),
                    ("PrivateIpAddress", field("private")?.clone()),
                ]));
            }
            dict(vec![(
                "Reservations",
                Json::List(vec![dict(vec![("Instances", Json::List(found))])]),
            )])
        }
        ["ec2", "terminate-instances"] => {
            let instances = dict_at(top, "instances")?;
            for id in values(argv, "--instance-ids") {
                let instance = instances
                    .get_mut(id)
                    .ok_or_else(|| bad(format!("no instance {id}")))?;
                let name = match tags_of(instance)?.get("Name") {
                    None => String::new(),
                    Some(Json::Str(name)) => name.clone(),
                    Some(other) => {
                        return Err(bad(format!("a Name tag is a {}", other.type_name())));
                    }
                };
                let leave = match config.get("leave_instance") {
                    None => "\0",
                    Some(Json::Str(leave)) => leave.as_str(),
                    Some(other) => {
                        return Err(bad(format!(
                            "config leave_instance is a {}",
                            other.type_name()
                        )));
                    }
                };
                if name.ends_with(leave) {
                    continue;
                }
                let Json::Dict(instance) = instance else {
                    unreachable!("tags_of found its tags, so it is a dict");
                };
                instance.insert("state".to_string(), s("terminated"));
            }
            Json::Dict(IndexMap::new())
        }
        ["ec2", "describe-security-groups"] => {
            let filters = values(argv, "--filters");
            let mut found = Vec::new();
            for (id, group) in dict_at(top, "groups")?.iter() {
                let tags = tags_of(group)?;
                if matches(tags, &filters) {
                    found.push(dict(vec![("GroupId", s(id)), ("Tags", tag_list(tags))]));
                }
            }
            dict(vec![("SecurityGroups", Json::List(found))])
        }
        ["ec2", "delete-security-group"] => {
            let violations = count(config, "dependency_violations")?;
            if positive(&violations) {
                config.insert("dependency_violations".to_string(), less_one(&violations));
                return Ok(Answer::fail(
                    "An error occurred (DependencyViolation) when calling the DeleteSecurityGroup operation",
                ));
            }
            if let Some(id) = arg(argv, "--group-id")? {
                dict_at(top, "groups")?.shift_remove(id);
            }
            Json::Dict(IndexMap::new())
        }
        _ => {
            return Ok(Answer::fail(&format!(
                "fake aws: unknown command {}",
                repr_list(argv)
            )));
        }
    };
    Ok(Answer::out(out))
}

/// Python's `repr` of a list of strings, for the fake's message about a
/// call it doesn't know: exact for ASCII; other characters pass as they
/// are, where Python escapes the ones it can't print.
fn repr_list(items: &[String]) -> String {
    let quoted: Vec<String> = items
        .iter()
        .map(|item| {
            let quote = if item.contains('\'') && !item.contains('"') {
                '"'
            } else {
                '\''
            };
            let mut out = String::from(quote);
            for c in item.chars() {
                match c {
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c if c == quote => {
                        out.push('\\');
                        out.push(c);
                    }
                    c if (c as u32) < 0x20 || c == '\u{7f}' => {
                        out.push_str(&format!("\\x{:02x}", c as u32));
                    }
                    c => out.push(c),
                }
            }
            out.push(quote);
            out
        })
        .collect();
    format!("[{}]", quoted.join(", "))
}

/// The model in-process, as a provider's [`Calls`]: each call runs
/// against [`Local::state`] as the `fake-aws` binary would, with the
/// region and JSON output appended; waits are recorded, not slept, and the
/// clock is [`Local::now`]. A `pause_call` doesn't pause here.
#[derive(Clone, Debug)]
pub struct Local {
    pub state: Json,
    pub region: String,
    pub now: f64,
    /// The seconds each wait between attempts would have slept.
    pub slept: Vec<u64>,
}

impl Local {
    /// The fake with `state` (the state file's JSON), in `region`, at
    /// Unix time `now`.
    pub fn new(state: Json, region: &str, now: f64) -> Local {
        Local {
            state,
            region: region.to_string(),
            now,
            slept: Vec::new(),
        }
    }
}

impl Calls for Local {
    fn call(&mut self, args: &[String]) -> Result<Json, CallError> {
        let argv = plan::argv(&[], args, &self.region);
        let before = self.state.clone();
        match call(&mut self.state, &argv) {
            Ok(answer) => plan::read_answer(
                args,
                Some(i32::from(answer.status)),
                &answer.stdout,
                &answer.stderr,
            ),
            Err(e) => {
                self.state = before;
                Err(CallError::Broken(e.to_string()))
            }
        }
    }

    fn credentials_expire(&mut self) -> Result<Option<String>, CallError> {
        match self.call(&plan::export_credentials()) {
            Ok(credentials) => plan::expiration(&credentials),
            Err(CallError::Aws(AwsError(_))) => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn sleep(&mut self, seconds: u64) -> Result<(), CallError> {
        self.slept.push(seconds);
        Ok(())
    }

    fn now(&mut self) -> f64 {
        self.now
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    fn state(text: &str) -> Json {
        Json::parse(text).unwrap()
    }

    #[test]
    fn invalid_rule_descriptions_leave_the_group_unchanged() {
        // AWS rule-description constraints, exercised by the private Round replay.
        // https://docs.aws.amazon.com/vpc/latest/userguide/security-group-rules.html
        let mut st = state(r#"{"groups":{"sg-1":{"tags":{},"rules":[]}}}"#);
        let answer = call(
            &mut st,
            &argv(&[
                "ec2",
                "authorize-security-group-ingress",
                "--group-id",
                "sg-1",
                "--ip-permissions",
                r#"[{"IpProtocol":"udp","FromPort":1024,"ToPort":65535,"UserIdGroupPairs":[{"GroupId":"sg-1","Description":"Test's source"}]}]"#,
            ]),
        )
        .unwrap();
        assert_eq!(answer.status, 254);
        assert!(answer.stderr.contains("InvalidParameterValue"));
        assert_eq!(
            st.item("groups")
                .unwrap()
                .item("sg-1")
                .unwrap()
                .item("rules")
                .unwrap(),
            &Json::List(Vec::new())
        );
    }

    #[test]
    fn credentials_without_expiration_can_be_exported() {
        // A saved access key exports successfully without an Expiration field.
        let mut st = state(r#"{"config":{"credentials_without_expiration":true}}"#);
        let answer = call(&mut st, &argv(&["configure", "export-credentials"])).unwrap();
        assert_eq!(answer.status, 0);
        assert_eq!(answer.stdout, "{}\n");
    }

    #[test]
    fn rule_description_limits_match_ec2() {
        // EC2 allows 255 ASCII characters from its documented character set.
        for (description, allowed) in [
            ("A".repeat(255), true),
            ("A".repeat(256), false),
            ("SSH & UDP / hosts [1] = test".to_string(), true),
            ("line\nbreak".to_string(), false),
            ("non-ASCII é".to_string(), false),
        ] {
            let mut st = state(r#"{"groups":{"sg-1":{"tags":{},"rules":[]}}}"#);
            let rules = Json::List(vec![dict(vec![
                ("IpProtocol", s("tcp")),
                ("FromPort", Json::Int(22)),
                ("ToPort", Json::Int(22)),
                (
                    "IpRanges",
                    Json::List(vec![dict(vec![
                        ("CidrIp", s("203.0.113.7/32")),
                        ("Description", s(&description)),
                    ])]),
                ),
            ])]);
            let answer = call(
                &mut st,
                &argv(&[
                    "ec2",
                    "authorize-security-group-ingress",
                    "--group-id",
                    "sg-1",
                    "--ip-permissions",
                    &py::dumps(&rules),
                ]),
            )
            .unwrap();
            assert_eq!(answer.status == 0, allowed, "{description:?}");
        }
    }

    #[test]
    fn group_and_prefix_list_descriptions_reject_ampersands() {
        // EC2 UserIdGroupPair and PrefixListId descriptions exclude '&'.
        for (source, key, value, allowed) in [
            ("IpRanges", "CidrIp", "203.0.113.7/32", true),
            ("Ipv6Ranges", "CidrIpv6", "2001:db8::/64", true),
            ("UserIdGroupPairs", "GroupId", "sg-1", false),
            ("PrefixListIds", "PrefixListId", "pl-1", false),
        ] {
            let mut st = state(r#"{"groups":{"sg-1":{"tags":{},"rules":[]}}}"#);
            let rules = Json::List(vec![dict(vec![
                ("IpProtocol", s("udp")),
                ("FromPort", Json::Int(1024)),
                ("ToPort", Json::Int(65535)),
                (
                    source,
                    Json::List(vec![dict(vec![
                        (key, s(value)),
                        ("Description", s("SSH & UDP")),
                    ])]),
                ),
            ])]);
            let answer = call(
                &mut st,
                &argv(&[
                    "ec2",
                    "authorize-security-group-ingress",
                    "--group-id",
                    "sg-1",
                    "--ip-permissions",
                    &py::dumps(&rules),
                ]),
            )
            .unwrap();
            assert_eq!(answer.status == 0, allowed, "{source}");
            let empty = Json::List(Vec::new());
            assert_eq!(
                st.get("groups")
                    .unwrap()
                    .get("sg-1")
                    .unwrap()
                    .get("rules")
                    .unwrap(),
                if allowed { &rules } else { &empty },
                "{source}"
            );
        }
    }

    #[test]
    fn records_every_call_and_answers_as_the_pythons_fake() {
        let mut st = state(r#"{"config": {"expiration": "2099-01-01T00:00:00Z"}}"#);
        let answer = call(
            &mut st,
            &argv(&[
                "sts",
                "get-caller-identity",
                "--region",
                "r",
                "--output",
                "json",
            ]),
        )
        .unwrap();
        assert_eq!(answer.stdout, "{\"Account\": \"123456789012\"}\n");
        assert_eq!(answer.status, 0);
        // The state as the Python's fake leaves it, keys in its order.
        assert_eq!(
            py::dumps(&st),
            r#"{"config": {"expiration": "2099-01-01T00:00:00Z"}, "calls": [["sts", "get-caller-identity", "--region", "r", "--output", "json"]], "instances": {}, "groups": {}}"#
        );
        let answer = call(&mut st, &argv(&["ec2", "fly", "it's"])).unwrap();
        assert_eq!(
            answer.stderr,
            "fake aws: unknown command ['ec2', 'fly', \"it's\"]\n"
        );
        assert_eq!(answer.status, 254);
    }

    #[test]
    fn dry_runs_are_answered_and_refused_as_configured() {
        let mut st = state(r#"{"config": {"unauthorized": "run-instances"}}"#);
        let answer = call(
            &mut st,
            &argv(&["ec2", "create-security-group", "--dry-run"]),
        )
        .unwrap();
        assert!(answer.stderr.contains("DryRunOperation"));
        let answer = call(&mut st, &argv(&["ec2", "run-instances", "--dry-run"])).unwrap();
        assert!(answer.stderr.contains("UnauthorizedOperation"));
    }

    #[test]
    fn a_repeated_client_token_gives_the_same_instance() {
        // As EC2 does, which makes a launch safe to try again after a
        // timeout: the provider retries with the Round's and role's token.
        let launch = |token: &str| {
            argv(&[
                "ec2",
                "run-instances",
                "--client-token",
                token,
                "--tag-specifications",
                "[{\"Tags\": [{\"Key\": \"Name\", \"Value\": \"n\"}]}]",
            ])
        };
        let mut st = state("{}");
        let first = call(&mut st, &launch("r1-server")).unwrap();
        let again = call(&mut st, &launch("r1-server")).unwrap();
        let other = call(&mut st, &launch("r1-client")).unwrap();
        assert_eq!(
            first.stdout,
            "{\"Instances\": [{\"InstanceId\": \"i-1\"}]}\n"
        );
        assert_eq!(again.stdout, first.stdout);
        assert_eq!(
            other.stdout,
            "{\"Instances\": [{\"InstanceId\": \"i-2\"}]}\n"
        );
        let instances = st.item("instances").unwrap().as_dict().unwrap();
        assert_eq!(instances.keys().collect::<Vec<_>>(), ["i-1", "i-2"]);
    }

    #[test]
    fn the_nth_call_can_fail_for_good_or_pause() {
        let launch = argv(&[
            "ec2",
            "run-instances",
            "--client-token",
            "r1-a",
            "--tag-specifications",
            "[{\"Tags\": []}]",
        ]);
        let mut st = state(r#"{"config": {"fail_call": {"command": "run-instances", "n": 2}}}"#);
        assert_eq!(call(&mut st, &launch).unwrap().status, 0);
        let second = call(&mut st, &launch).unwrap();
        assert_eq!(
            second.stderr,
            "An error occurred (InsufficientInstanceCapacity) when calling the RunInstances operation: \
             the fake's failure for good\n"
        );
        assert_eq!(call(&mut st, &launch).unwrap().status, 254);
        let mut st = state(
            r#"{"config": {"pause_call": {"command": "run-instances", "n": 2, "until": "/tmp/go"}}}"#,
        );
        assert_eq!(call(&mut st, &launch).unwrap().pause_until, None);
        assert_eq!(
            call(&mut st, &launch).unwrap().pause_until.as_deref(),
            Some("/tmp/go")
        );
        assert_eq!(call(&mut st, &launch).unwrap().pause_until, None);
    }

    #[test]
    fn a_call_it_cant_answer_leaves_the_state_as_it_was() {
        let mut fake = Local::new(state("{}"), "eu-north-1", 0.0);
        let before = fake.state.clone();
        let result = fake.call(&argv(&[
            "ec2",
            "terminate-instances",
            "--instance-ids",
            "i-9",
        ]));
        assert!(matches!(result, Err(CallError::Broken(_))));
        assert_eq!(fake.state, before);
    }

    #[test]
    fn operations_are_named_as_the_cli_names_them() {
        assert_eq!(operation("run-instances"), "RunInstances");
        assert_eq!(operation("delete-security-group"), "DeleteSecurityGroup");
    }
}
