//! A Round's recorded AWS calls replayed against the fake, by hand
//!: nothing
//! of G's live calls is committed, so the fake's fidelity is checked
//! locally, on the raw records ([`super::record`]) where they are, and
//! each difference then becomes a small synthetic test of the fake.
//!
//! Each call is replayed in order on one fake, from the state the
//! records show where it can be told:
//!
//! - the fake's settings are taken from the records: the credentials'
//!   expiry, whether the image's volumes die with it, a dry run refused,
//!   the launches throttled and the group deletions refused before the
//!   first that went through;
//! - the IDs AWS gave (subnet, VPC, image, group, instances) are learned
//!   from its answers as the fake gives its own for the same calls, and a
//!   recorded call's IDs are the fake's before it is replayed; a call with
//!   an ID the fake never gave isn't replayed.
//!
//! What it compares, as the tool reads it: the exit status, the error's
//! code, and the fields of each answer the tool reads, IDs through what
//! was learned, addresses only for being there. Every difference is said
//! in the check's own words, the call's number and operation and the
//! field's path, never a value of the records, which hold the account and
//! the addresses: its report can be shown.

use std::collections::HashMap;
use std::fmt;

use indexmap::IndexMap;

use super::fake;
use super::plan;
use super::record::RawRecord;
use crate::py::Json;

/// One way the fake answered a recorded call otherwise, said without the
/// record's values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Difference {
    /// The record's number, as its files are named (`0001` on).
    pub call: usize,
    /// The call's operation, where it is one of the tool's.
    pub operation: Option<&'static str>,
    /// What differed, in the check's words.
    pub what: String,
}

impl fmt::Display for Difference {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "call {:04} ({}): {}",
            self.call,
            self.operation.unwrap_or("a call the tool doesn't make"),
            self.what
        )
    }
}

/// The calls the tool makes, by their first two words.
const OPERATIONS: &[&str] = &[
    "sts get-caller-identity",
    "configure export-credentials",
    "ec2 describe-images",
    "ec2 describe-subnets",
    "ec2 create-security-group",
    "ec2 authorize-security-group-ingress",
    "ec2 run-instances",
    "ec2 wait",
    "ec2 describe-instances",
    "ec2 describe-security-groups",
    "ec2 terminate-instances",
    "ec2 delete-security-group",
];

fn operation(argv: &[String]) -> Option<&'static str> {
    let words = argv.iter().take(2).cloned().collect::<Vec<_>>().join(" ");
    OPERATIONS.iter().copied().find(|op| *op == words)
}

/// The prefixes of the IDs AWS gives the tool's resources.
const ID_PREFIXES: [&str; 5] = ["i-", "sg-", "subnet-", "vpc-", "ami-"];

fn is_id(word: &str) -> bool {
    ID_PREFIXES.iter().any(|prefix| {
        word.strip_prefix(prefix)
            .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_hexdigit()))
    })
}

fn dry_run(argv: &[String]) -> bool {
    argv.iter().any(|a| a == "--dry-run")
}

/// `An error occurred (CODE) ...`'s code.
fn error_code(stderr: &str) -> Option<&str> {
    let rest = &stderr[stderr.find("An error occurred (")? + "An error occurred (".len()..];
    Some(&rest[..rest.find(')')?])
}

fn text(bytes: &Option<Vec<u8>>) -> &str {
    bytes
        .as_deref()
        .and_then(|b| std::str::from_utf8(b).ok())
        .unwrap_or("")
}

/// What the tool reads of an answer, as JSON; `{}` for none.
fn answer(bytes: &Option<Vec<u8>>) -> Option<Json> {
    let text = std::str::from_utf8(bytes.as_deref()?).ok()?;
    if crate::py::strip(text).is_empty() {
        return Some(Json::Dict(IndexMap::new()));
    }
    Json::parse(text).ok()
}

/// The fake's settings, as the records show them.
fn config(records: &[RawRecord]) -> IndexMap<String, Json> {
    let mut config = IndexMap::new();
    if let Some(expiration) = records.iter().find_map(|r| r.credentials.clone()).flatten() {
        config.insert("expiration".to_string(), Json::Str(expiration));
    } else if records.iter().any(|r| {
        operation(&r.argv) == Some("configure export-credentials")
            && r.status == Some(0)
            && r.credentials == Some(None)
    }) {
        config.insert(
            "credentials_without_expiration".to_string(),
            Json::Bool(true),
        );
    }
    let images = records
        .iter()
        .find(|r| operation(&r.argv) == Some("ec2 describe-images") && r.status == Some(0))
        .and_then(|r| answer(&r.stdout))
        .and_then(|a| plan::latest_image(&a).ok());
    if let Some(image) = images {
        let dies = plan::volumes_die_with(&image).is_ok();
        config.insert("delete_on_termination".to_string(), Json::Bool(dies));
    }
    let refused = records
        .iter()
        .find(|r| dry_run(&r.argv) && error_code(text(&r.stderr)) == Some("UnauthorizedOperation"));
    if let Some(word) = refused.and_then(|r| r.argv.get(1)) {
        config.insert("unauthorized".to_string(), Json::Str(word.clone()));
    }
    // Only failures before the first call that went through: the fake
    // fails the first ones.
    let failed_first = |op: &str, codes: &[&str]| {
        records
            .iter()
            .filter(|r| operation(&r.argv) == Some(op) && !dry_run(&r.argv))
            .take_while(|r| r.status != Some(0))
            .filter(|r| error_code(text(&r.stderr)).is_some_and(|c| codes.contains(&c)))
            .count()
    };
    let throttled = failed_first("ec2 run-instances", &["RequestLimitExceeded", "Throttling"]);
    if throttled > 0 {
        config.insert(
            "throttle_run_instances".to_string(),
            Json::Int(throttled as i128),
        );
    }
    let held = failed_first("ec2 delete-security-group", &["DependencyViolation"]);
    if held > 0 {
        config.insert("dependency_violations".to_string(), Json::Int(held as i128));
    }
    config
}

/// A recorded call's argv with the fake's IDs for those AWS gave; `None`
/// where it holds one the fake never gave.
fn with_fakes_ids(argv: &[String], ids: &HashMap<String, String>) -> Option<Vec<String>> {
    // Longest first, so that no ID is replaced inside a longer one.
    let mut known: Vec<(&String, &String)> = ids.iter().collect();
    known.sort_by_key(|(real, _)| std::cmp::Reverse(real.len()));
    argv.iter()
        .map(|word| {
            if let Some(fake) = ids.get(word) {
                return Some(fake.clone());
            }
            let mut out = word.clone();
            for (real, fake) in &known {
                out = out.replace(real.as_str(), fake);
            }
            // An ID left, whole or inside a JSON argument, the fake never
            // gave.
            let left = out
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
                .any(|token| is_id(token) && !ids.values().any(|fake| fake == token));
            (!left).then_some(out)
        })
        .collect()
}

/// `a.b[0].c` of an answer, where every step is there.
fn at<'a>(value: &'a Json, path: &[Step]) -> Option<&'a Json> {
    path.iter().try_fold(value, |v, step| match step {
        Step::Key(k) => v.as_dict()?.get(*k),
        Step::First => v.as_list()?.first(),
    })
}

#[derive(Clone, Copy)]
enum Step {
    Key(&'static str),
    First,
}

/// What lists the resources of an answer: [`instances`] or [`groups`].
type Lister = fn(&Json) -> Option<Vec<&Json>>;

/// The instances of a `describe-instances` answer, flattened as the
/// provider flattens them.
fn instances(answer: &Json) -> Option<Vec<&Json>> {
    let mut found = Vec::new();
    for reservation in answer
        .as_dict()?
        .get("Reservations")
        .map_or(Some(&[][..]), Json::as_list)?
    {
        let listed = reservation.as_dict()?.get("Instances");
        found.extend(listed.map_or(Some(&[][..]), Json::as_list)?.iter());
    }
    Some(found)
}

fn groups(answer: &Json) -> Option<Vec<&Json>> {
    let listed = answer.as_dict()?.get("SecurityGroups");
    Some(
        listed
            .map_or(Some(&[][..]), Json::as_list)?
            .iter()
            .collect(),
    )
}

/// A resource's tags, `{key: value}`.
fn tags(resource: &Json) -> Option<IndexMap<String, Json>> {
    let listed = resource.as_dict()?.get("Tags");
    let mut found = IndexMap::new();
    for tag in listed.map_or(Some(&[][..]), Json::as_list)? {
        let tag = tag.as_dict()?;
        found.insert(
            tag.get("Key")?.as_str()?.to_string(),
            tag.get("Value")?.clone(),
        );
    }
    Some(found)
}

fn named<'a>(resources: &[&'a Json], name: &Json) -> Option<&'a Json> {
    resources
        .iter()
        .copied()
        .find(|r| tags(r).and_then(|t| t.get("Name").cloned()).as_ref() == Some(name))
}

/// Learns the IDs AWS gave for the same resources as the fake's answer
/// names: `ids` maps AWS's to the fake's, the first learned kept.
fn learn(op: &str, recorded: &Json, faked: &Json, ids: &mut HashMap<String, String>) {
    let mut pair = |real: Option<&Json>, fake: Option<&Json>| {
        if let (Some(Json::Str(real)), Some(Json::Str(fake))) = (real, fake) {
            ids.entry(real.clone()).or_insert_with(|| fake.clone());
        }
    };
    let subnet = [Step::Key("Subnets"), Step::First];
    match op {
        "ec2 describe-subnets" => {
            for key in ["SubnetId", "VpcId"] {
                let path = [subnet[0], subnet[1], Step::Key(key)];
                pair(at(recorded, &path), at(faked, &path));
            }
        }
        "ec2 describe-images" => {
            let latest = |a: &Json| plan::latest_image(a).ok();
            let (real, fake) = (latest(recorded), latest(faked));
            pair(
                real.as_ref().and_then(|i| at(i, &[Step::Key("ImageId")])),
                fake.as_ref().and_then(|i| at(i, &[Step::Key("ImageId")])),
            );
        }
        "ec2 create-security-group" => {
            pair(
                at(recorded, &[Step::Key("GroupId")]),
                at(faked, &[Step::Key("GroupId")]),
            );
        }
        "ec2 run-instances" => {
            let path = [Step::Key("Instances"), Step::First, Step::Key("InstanceId")];
            pair(at(recorded, &path), at(faked, &path));
        }
        "ec2 describe-instances" | "ec2 describe-security-groups" => {
            let (list, id): (Lister, &str) = if op == "ec2 describe-instances" {
                (instances, "InstanceId")
            } else {
                (groups, "GroupId")
            };
            let (Some(real), Some(fake)) = (list(recorded), list(faked)) else {
                return;
            };
            // The same resource by its Name tag, which the Round gave both.
            for r in &real {
                if let Some(name) = tags(r).and_then(|t| t.get("Name").cloned())
                    && let Some(f) = named(&fake, &name)
                {
                    pair(at(r, &[Step::Key(id)]), at(f, &[Step::Key(id)]));
                }
            }
        }
        _ => {}
    }
}

/// What the tool reads of an answer, in the check's own words: of a
/// recorded answer (`ids` given), AWS's IDs as the fake's, those never
/// learned as `unknown`; of the fake's, its own; addresses only for being
/// there. `None` where the answer isn't of the shape the tool reads.
fn view(op: &str, answer: &Json, ids: Option<&HashMap<String, String>>) -> Option<Json> {
    let id = |v: Option<&Json>| -> Json {
        match (v, ids) {
            (Some(Json::Str(s)), Some(ids)) => {
                Json::Str(ids.get(s).cloned().unwrap_or_else(|| "unknown".to_string()))
            }
            (Some(Json::Str(s)), None) => Json::Str(s.clone()),
            (Some(other), _) => Json::Str(format!("a {}", other.type_name())),
            (None, _) => Json::Null,
        }
    };
    let present = |v: Option<&Json>| Json::Bool(v.is_some_and(|v| v.as_str().is_some()));
    let dict = |pairs: Vec<(String, Json)>| Json::Dict(pairs.into_iter().collect());
    Some(match op {
        "ec2 describe-subnets" => dict(vec![
            (
                "SubnetId".into(),
                id(at(
                    answer,
                    &[Step::Key("Subnets"), Step::First, Step::Key("SubnetId")],
                )),
            ),
            (
                "VpcId".into(),
                id(at(
                    answer,
                    &[Step::Key("Subnets"), Step::First, Step::Key("VpcId")],
                )),
            ),
        ]),
        "ec2 describe-images" => {
            let latest = plan::latest_image(answer).ok()?;
            dict(vec![
                ("ImageId".into(), id(at(&latest, &[Step::Key("ImageId")]))),
                (
                    "volumes die with it".into(),
                    Json::Bool(plan::volumes_die_with(&latest).is_ok()),
                ),
            ])
        }
        "ec2 create-security-group" => dict(vec![(
            "GroupId".into(),
            id(at(answer, &[Step::Key("GroupId")])),
        )]),
        "ec2 run-instances" => dict(vec![(
            "InstanceId".into(),
            id(at(
                answer,
                &[Step::Key("Instances"), Step::First, Step::Key("InstanceId")],
            )),
        )]),
        "ec2 describe-instances" => {
            let mut listed = Vec::new();
            for instance in instances(answer)? {
                let Json::Str(key) = id(instance.as_dict()?.get("InstanceId")) else {
                    return None;
                };
                let seen = dict(vec![
                    ("Tags".into(), Json::Dict(tags(instance)?)),
                    (
                        "PublicIpAddress".into(),
                        present(instance.as_dict()?.get("PublicIpAddress")),
                    ),
                    (
                        "PrivateIpAddress".into(),
                        present(instance.as_dict()?.get("PrivateIpAddress")),
                    ),
                ]);
                listed.push((key, seen));
            }
            dict(listed)
        }
        "ec2 describe-security-groups" => {
            let mut listed = Vec::new();
            for group in groups(answer)? {
                let Json::Str(key) = id(group.as_dict()?.get("GroupId")) else {
                    return None;
                };
                listed.push((key, dict(vec![("Tags".into(), Json::Dict(tags(group)?))])));
            }
            dict(listed)
        }
        _ => Json::Dict(IndexMap::new()),
    })
}

/// A key of a view as the report may say it: the check's own words and
/// the fake's IDs, and of tags only the tool's own keys.
fn say_key(key: &str) -> String {
    let own = [
        "SubnetId",
        "VpcId",
        "ImageId",
        "volumes die with it",
        "GroupId",
        "InstanceId",
        "Tags",
        "PublicIpAddress",
        "PrivateIpAddress",
        plan::TAG,
        plan::EXPIRY_TAG,
        "Name",
    ];
    if own.contains(&key) || (is_id(key) && key.len() <= 12) {
        key.to_string()
    } else if key == "unknown" {
        "(an ID the fake never gave)".to_string()
    } else {
        "(a key the tool doesn't write)".to_string()
    }
}

/// Where two views differ, each place said by its path.
fn differences(recorded: &Json, faked: &Json, path: &str, out: &mut Vec<String>) {
    match (recorded, faked) {
        (Json::Dict(r), Json::Dict(f)) => {
            for (key, value) in r {
                let here = format!(
                    "{path}{}{}",
                    if path.is_empty() { "" } else { "." },
                    say_key(key)
                );
                match f.get(key) {
                    Some(other) => differences(value, other, &here, out),
                    None => out.push(format!("answer {here}: the fake's lacks it")),
                }
            }
            for key in f.keys().filter(|k| !r.contains_key(*k)) {
                let here = format!(
                    "{path}{}{}",
                    if path.is_empty() { "" } else { "." },
                    say_key(key)
                );
                out.push(format!("answer {here}: only the fake's has it"));
            }
        }
        (r, f) if std::mem::discriminant(r) != std::mem::discriminant(f) => {
            out.push(format!("answer {path}: of another kind in the fake's"));
        }
        (r, f) if r != f => out.push(format!("answer {path}: differs")),
        _ => {}
    }
}

/// Replays `records`, a Round's raw records in order, against the fake:
/// every difference, said without the records' values.
pub fn replay(records: &[RawRecord]) -> Vec<Difference> {
    let mut state = Json::Dict(IndexMap::from([(
        "config".to_string(),
        Json::Dict(config(records)),
    )]));
    let mut ids: HashMap<String, String> = HashMap::new();
    let mut found = Vec::new();
    for (i, record) in records.iter().enumerate() {
        let op = operation(&record.argv);
        let mut say = |what: String| {
            found.push(Difference {
                call: i + 1,
                operation: op,
                what,
            })
        };
        let Some(argv) = with_fakes_ids(&record.argv, &ids) else {
            say("argv: it holds an ID the fake never gave, so it isn't replayed".into());
            continue;
        };
        let before = state.clone();
        let answer_of_fake = match fake::call(&mut state, &argv) {
            Ok(a) => a,
            Err(_) => {
                state = before;
                say("argv: the fake can't take it as recorded".into());
                continue;
            }
        };
        if answer_of_fake
            .stderr
            .starts_with("fake aws: unknown command")
        {
            say("argv: the fake doesn't know this call".into());
            continue;
        }
        let Some(recorded_status) = record.status else {
            say("status: a signal ended the recorded call, so it isn't compared".into());
            continue;
        };
        let fake_status = i32::from(answer_of_fake.status);
        match (recorded_status == 0, fake_status == 0) {
            (true, false) => {
                say("status: the recorded call went through, the fake's failed".into())
            }
            (false, true) => {
                say("status: the recorded call failed, the fake's went through".into())
            }
            (false, false) => {
                let recorded_code = error_code(text(&record.stderr));
                if recorded_code != error_code(&answer_of_fake.stderr) {
                    say("error: its code differs".into());
                } else if recorded_status != fake_status {
                    say("status: both failed, with other exit statuses".into());
                }
            }
            (true, true) => {
                if record.credentials.is_some() {
                    // Only whether they expire was recorded, which the
                    // fake was given.
                    continue;
                }
                let faked = Json::parse(&answer_of_fake.stdout)
                    .unwrap_or_else(|_| Json::Dict(IndexMap::new()));
                let Some(recorded) = answer(&record.stdout) else {
                    say(
                        "answer: the record's isn't JSON, or was withheld, so it isn't compared"
                            .into(),
                    );
                    continue;
                };
                let op = op.unwrap_or("");
                learn(op, &recorded, &faked, &mut ids);
                match (view(op, &recorded, Some(&ids)), view(op, &faked, None)) {
                    (Some(r), Some(f)) => {
                        let mut out = Vec::new();
                        differences(&r, &f, "", &mut out);
                        out.into_iter().for_each(&mut say);
                    }
                    (None, _) => {
                        say("answer: the record's isn't of the shape the tool reads".into())
                    }
                    (_, None) => say("answer: the fake's isn't of the shape the tool reads".into()),
                }
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aws::{AwsProvider, CallError, Calls};
    use crate::num::Num;

    /// The fake, with each call kept as the recorder keeps it.
    struct Recording {
        state: Json,
        records: Vec<RawRecord>,
    }

    impl Calls for Recording {
        fn call(&mut self, args: &[String]) -> Result<Json, CallError> {
            let argv = plan::argv(&[], args, "eu-north-1");
            let answer = fake::call(&mut self.state, &argv).expect("the fake answers");
            self.records.push(RawRecord {
                argv,
                status: Some(i32::from(answer.status)),
                seconds: 0.5,
                stdout: Some(answer.stdout.clone().into_bytes()),
                credentials: None,
                stderr: Some(answer.stderr.clone().into_bytes()),
            });
            plan::read_answer(
                args,
                Some(i32::from(answer.status)),
                &answer.stdout,
                &answer.stderr,
            )
        }

        fn credentials_expire(&mut self) -> Result<Option<String>, CallError> {
            let expires = match self.call(&plan::export_credentials()) {
                Ok(c) => plan::expiration(&c)?,
                Err(CallError::Aws(_)) => None,
                Err(e) => return Err(e),
            };
            let record = self.records.last_mut().expect("the call just made");
            record.stdout = None;
            record.credentials = Some(expires.clone());
            Ok(expires)
        }

        fn sleep(&mut self, _: u64) -> Result<(), CallError> {
            Ok(())
        }

        fn now(&mut self) -> f64 {
            1_790_000_000.0
        }
    }

    /// What AWS gives for what the fake gives: IDs, addresses and the
    /// account as a real Round's records hold them.
    const REAL: &[(&str, &str)] = &[
        ("i-1", "i-0a1b2c3d4e5f60718"),
        ("i-2", "i-0f1e2d3c4b5a69788"),
        ("sg-1", "sg-0123456789abcdef0"),
        ("subnet-1", "subnet-0a1b2c3d4e5f60789"),
        ("vpc-1", "vpc-0123abcd"),
        ("ami-1", "ami-0abcdef1234567890"),
        ("198.51.100.1", "16.170.14.201"),
        ("198.51.100.2", "13.48.77.5"),
        ("10.0.0.1", "172.31.20.5"),
        ("10.0.0.2", "172.31.28.19"),
        ("123456789012", "210987654321"),
    ];

    fn real(text: &str) -> String {
        REAL.iter().fold(text.to_string(), |t, (fake, real)| {
            // Whole IDs only: i-1 isn't in i-12.
            let mut out = String::new();
            let mut rest = t.as_str();
            while let Some(at) = rest.find(fake) {
                let after = &rest[at + fake.len()..];
                let whole = !after.starts_with(|c: char| c.is_ascii_alphanumeric())
                    && !rest[..at].ends_with(|c: char| c.is_ascii_alphanumeric() || c == '.');
                out.push_str(&rest[..at]);
                out.push_str(if whole { real } else { fake });
                rest = after;
            }
            out.push_str(rest);
            out
        })
    }

    fn realistic(records: Vec<RawRecord>) -> Vec<RawRecord> {
        let bytes =
            |b: Option<Vec<u8>>| b.map(|b| real(&String::from_utf8(b).unwrap()).into_bytes());
        records
            .into_iter()
            .map(|r| RawRecord {
                argv: r.argv.iter().map(|a| real(a)).collect(),
                stdout: bytes(r.stdout),
                stderr: bytes(r.stderr),
                ..r
            })
            .collect()
    }

    /// A Round of two hosts on the fake, recorded: its preflight, its
    /// hosts, the permission check, and its Round Teardown.
    fn round(config: &str) -> Vec<RawRecord> {
        let state = Json::parse(&format!("{{\"config\": {config}}}")).unwrap();
        let recording = Recording {
            state,
            records: Vec::new(),
        };
        let mut provider = AwsProvider::new(
            recording,
            "eu-north-1",
            "eu-north-1a",
            IndexMap::from([
                ("server".to_string(), "c6in.large".to_string()),
                ("client".to_string(), "c6in.large".to_string()),
            ]),
            IndexMap::from([("c6in.large".to_string(), Num::Float(0.142))]),
            "203.0.113.7/32",
            true,
        );
        provider.check(2.0).unwrap();
        provider.check_permissions("permission-check").unwrap();
        let roles = ["server".to_string(), "client".to_string()];
        provider
            .create(
                "r20260929T120000-ab12",
                1_790_007_200.0,
                &roles,
                "ssh-ed25519 AAAA t",
            )
            .unwrap();
        assert_eq!(provider.destroy("r20260929T120000-ab12").unwrap(), []);
        realistic(provider.calls.records)
    }

    #[test]
    fn a_round_that_went_as_the_fake_says_replays_without_a_difference() {
        let records = round(
            r#"{"expiration": "2099-01-01T00:00:00Z", "throttle_run_instances": 2,
                "dependency_violations": 1, "unauthorized": "create-security-group"}"#,
        );
        assert!(records.len() > 20);
        // The records hold AWS's IDs and addresses, none the fake gives.
        let all: String = records
            .iter()
            .map(|r| r.argv.join(" ") + text(&r.stdout))
            .collect();
        assert!(all.contains("i-0a1b2c3d4e5f60718") && !all.contains("\"i-1\""));
        assert_eq!(replay(&records), []);
    }

    #[test]
    fn credential_exports_without_expiration_keep_their_recorded_status() {
        // Successful access-key exports and failed exports both record no expiry.
        for settings in [r#"{}"#, r#"{"credentials_without_expiration":true}"#] {
            let records = round(settings);
            assert_eq!(replay(&records), [], "{settings}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_round_recorded_to_disk_replays_as_read_back() {
        use crate::aws::record::{self, Call, Recorder, Stdout};
        let records =
            round(r#"{"expiration": "2099-01-01T00:00:00Z", "throttle_run_instances": 1}"#);
        let dir = std::env::temp_dir().join(format!("bench-rounds-replay-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut recorder =
            Recorder::create(&dir.join("calls"), "r20260929T120000-ab12", &[]).unwrap();
        for r in &records {
            let stdout = match &r.credentials {
                Some(expiration) => Stdout::Credentials {
                    expiration: expiration.as_deref(),
                },
                None => Stdout::Bytes(r.stdout.as_deref().unwrap_or_default()),
            };
            recorder
                .record(&Call {
                    argv: &r.argv,
                    status: r.status,
                    seconds: r.seconds,
                    stdout,
                    stderr: r.stderr.as_deref().unwrap_or_default(),
                })
                .unwrap();
        }
        let back = record::read(recorder.dir()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(back, records);
        assert_eq!(replay(&back), []);
    }

    #[test]
    fn every_difference_is_said_without_the_records_values() {
        let records = round(r#"{"expiration": "2099-01-01T00:00:00Z"}"#);
        let index = |op: &str, nth: usize| {
            records
                .iter()
                .enumerate()
                .filter(|(_, r)| operation(&r.argv) == Some(op) && !dry_run(&r.argv))
                .nth(nth)
                .map(|(i, _)| i)
                .unwrap()
        };
        let mut changed = records.clone();
        // AWS listed one instance fewer than the fake does.
        let listed = index("ec2 describe-instances", 0);
        let answer = text(&changed[listed].stdout).to_string();
        let start = answer
            .find("{\"InstanceId\": \"i-0f1e2d3c4b5a69788\"")
            .unwrap();
        let cut = answer[..start].trim_end_matches(", ").to_string()
            + &answer[answer[start..].find("}]}").map(|e| start + e + 1).unwrap()..];
        changed[listed].stdout = Some(cut.into_bytes());
        // AWS refused a deletion the fake lets through.
        let delete = index("ec2 delete-security-group", 0);
        changed[delete].status = Some(254);
        changed[delete].stdout = Some(Vec::new());
        changed[delete].stderr = Some(
            b"\nAn error occurred (InvalidGroup.InUse) when calling the DeleteSecurityGroup operation: \
              Group sg-0123456789abcdef0 is used by 210987654321\n"
                .to_vec(),
        );
        // AWS named the group otherwise, and answered a call the fake
        // doesn't know.
        let described = index("ec2 describe-security-groups", 0);
        let renamed =
            text(&changed[described].stdout).replace("udpstp-bench-r2026", "udpstp-bench-q2026");
        changed[described].stdout = Some(renamed.into_bytes());
        let mut unknown = changed[0].clone();
        unknown.argv = [
            "ec2",
            "describe-volumes",
            "--region",
            "eu-north-1",
            "--output",
            "json",
        ]
        .map(String::from)
        .to_vec();
        changed.push(unknown);
        let found: Vec<String> = replay(&changed).iter().map(ToString::to_string).collect();
        let at = |i: usize| format!("call {:04}", i + 1);
        assert!(
            found.contains(&format!(
                "{} (ec2 describe-instances): answer i-2: only the fake's has it",
                at(listed)
            )),
            "{found:#?}"
        );
        assert!(found.contains(&format!("{} (ec2 delete-security-group): status: the recorded call failed, the fake's went through", at(delete))), "{found:#?}");
        assert!(
            found.contains(&format!(
                "{} (ec2 describe-security-groups): answer sg-1.Tags.Name: differs",
                at(described)
            )),
            "{found:#?}"
        );
        assert!(
            found.contains(&format!(
                "call {:04} (a call the tool doesn't make): argv: the fake doesn't know this call",
                changed.len()
            )),
            "{found:#?}"
        );
        // Never a value of the records: IDs, addresses, the account, the
        // Round's ID or its key.
        for (_, value) in REAL {
            assert!(
                found.iter().all(|f| !f.contains(value)),
                "{value}: {found:#?}"
            );
        }
        for value in ["r2026", "q2026", "ssh-ed25519", "InvalidGroup"] {
            assert!(
                found.iter().all(|f| !f.contains(value)),
                "{value}: {found:#?}"
            );
        }
    }

    #[test]
    fn a_call_with_an_id_the_fake_never_gave_isnt_replayed() {
        let mut records = round(r#"{"expiration": "2099-01-01T00:00:00Z"}"#);
        let terminate = records
            .iter()
            .position(|r| operation(&r.argv) == Some("ec2 terminate-instances"))
            .unwrap();
        records[terminate]
            .argv
            .push("i-0999999999999999f".to_string());
        let found = replay(&records);
        assert_eq!(
            found.first(),
            Some(&Difference {
                call: terminate + 1,
                operation: Some("ec2 terminate-instances"),
                what: "argv: it holds an ID the fake never gave, so it isn't replayed".into(),
            })
        );
        // The instances it would have ended then differ, said so too.
        assert!(found.len() > 1, "{found:#?}");
        assert!(found.iter().all(|f| !f.to_string().contains("i-0999")));
    }
}
