//! Retire superseded udpstp submissions before delivering the current release.
use super::winget_submission::{REPO, Result, api, gh, gh_with_token};
use serde_json::{Value, json};

const APPLICATION: &str = "pederbe/udpstp";
const FORK: &str = "pederbe/winget-pkgs";
const OWNER: &str = "pederbe";
const PREFIX: &str = "manifests/p/pederbe/udpstp/";

fn version(text: &str) -> Result<[u64; 3]> {
    let parts = text
        .split('.')
        .map(|part| {
            if part.is_empty()
                || !part.bytes().all(|b| b.is_ascii_digit())
                || (part.len() > 1 && part.starts_with('0'))
            {
                return Err("expected a stable numeric version".into());
            }
            Ok(part.parse::<u64>()?)
        })
        .collect::<Result<Vec<_>>>()?;
    parts
        .try_into()
        .map_err(|_| "expected a stable numeric version".into())
}

fn current(release: &Value, selected: &str) -> Result<()> {
    version(selected)?;
    if release["tag_name"] != format!("v{selected}")
        || release["draft"] != false
        || release["prerelease"] != false
    {
        return Err(format!(
            "WinGet delivery requires the latest stable udpstp release, not v{selected}"
        )
        .into());
    }
    Ok(())
}

pub(super) fn require_latest(selected: &str) -> Result<()> {
    current(
        &api(&format!("repos/{APPLICATION}/releases/latest"))?,
        selected,
    )
}

trait Github {
    fn read(&self, endpoint: &str) -> Result<Value>;
    fn list(&self, endpoint: &str) -> Result<Vec<Value>>;
    fn check_fork_credential(&self) -> Result<()>;
    fn close(&self, number: u64) -> Result<()>;
    fn delete(&self, branch: &str, head: &str) -> Result<()>;
}

struct Cli;
impl Github for Cli {
    fn read(&self, endpoint: &str) -> Result<Value> {
        api(endpoint)
    }
    fn list(&self, endpoint: &str) -> Result<Vec<Value>> {
        let pages: Vec<Vec<Value>> =
            serde_json::from_str(&gh(&["api", "--paginate", "--slurp", endpoint], None)?)?;
        Ok(pages.into_iter().flatten().collect())
    }
    fn check_fork_credential(&self) -> Result<()> {
        let token = std::env::var("WINGET_FORK_TOKEN")
            .map_err(|_| "WINGET_FORK_TOKEN is required for submission branch cleanup")?;
        let actor: Value =
            serde_json::from_str(&gh_with_token(&["api", "user"], None, Some(&token))?)?;
        if actor["login"] != OWNER {
            return Err("WinGet fork credential must authenticate the submission owner".into());
        }
        Ok(())
    }
    fn close(&self, number: u64) -> Result<()> {
        gh(
            &[
                "api",
                "--method",
                "PATCH",
                &format!("repos/{REPO}/pulls/{number}"),
                "--input",
                "-",
            ],
            Some(&json!({"state":"closed"})),
        )?;
        Ok(())
    }
    fn delete(&self, branch: &str, head: &str) -> Result<()> {
        let repository = api(&format!("repos/{FORK}"))?;
        let id = repository["node_id"]
            .as_str()
            .ok_or("missing submission fork ID")?;
        let token = std::env::var("WINGET_FORK_TOKEN")?;
        let response: Value = serde_json::from_str(&gh_with_token(
            &["api", "graphql", "--input", "-"],
            Some(&deletion(id, branch, head)),
            Some(&token),
        )?)?;
        if !response["errors"].is_null() || !response["data"]["updateRefs"].is_object() {
            return Err("submission branch deletion failed".into());
        }
        Ok(())
    }
}

fn deletion(repository: &str, branch: &str, head: &str) -> Value {
    // GitHub checks beforeOid atomically, including when afterOid deletes a ref.
    json!({"query":"mutation($input: UpdateRefsInput!) { updateRefs(input: $input) { clientMutationId } }",
        "variables":{"input":{"repositoryId":repository,"refUpdates":[{
            "name":format!("refs/heads/{branch}"),"beforeOid":head,
            "afterOid":"0000000000000000000000000000000000000000"}]}}})
}

fn encode(text: &str) -> String {
    text.bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                char::from(byte).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

fn branch_head(github: &impl Github, branch: &str) -> Result<Option<String>> {
    let refs = github.list(&format!(
        "repos/{FORK}/git/matching-refs/heads/{}",
        encode(branch)
    ))?;
    refs.iter()
        .find(|r| r["ref"] == format!("refs/heads/{branch}"))
        .map(|r| {
            r["object"]["sha"]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| "missing branch head".into())
        })
        .transpose()
}

fn manifest_version(files: &[Value]) -> Result<Option<&str>> {
    // Catalog removals and metadata repairs are not pending new-version submissions.
    if files.iter().any(|file| file["status"] != "added") {
        return Ok(None);
    }
    let Some(path) = files
        .iter()
        .find_map(|f| f["filename"].as_str()?.strip_prefix(PREFIX))
    else {
        return Ok(None);
    };
    let (selected, _) = path
        .split_once('/')
        .ok_or("missing submitted package version")?;
    version(selected)?;
    let names = [
        "pederbe.udpstp.yaml",
        "pederbe.udpstp.installer.yaml",
        "pederbe.udpstp.locale.en-US.yaml",
    ];
    if files.len() != names.len()
        || names.iter().any(|name| {
            !files
                .iter()
                .any(|f| f["filename"] == format!("{PREFIX}{selected}/{name}"))
        })
    {
        return Err("udpstp submission mixes manifest versions or unrelated files, inspect it before cleanup".into());
    }
    Ok(Some(selected))
}

fn same_submission(actual: &Value, expected: &Value) -> bool {
    actual["head"]["sha"] == expected["head"]["sha"]
        && actual["head"]["ref"] == expected["head"]["ref"]
        && actual["head"]["repo"]["full_name"] == expected["head"]["repo"]["full_name"]
        && actual["user"]["login"] == expected["user"]["login"]
        && actual["base"]["repo"]["full_name"] == expected["base"]["repo"]["full_name"]
}

fn cleanup(github: &impl Github, selected: &str, submit: bool) -> Result<()> {
    current(
        &github.read(&format!("repos/{APPLICATION}/releases/latest"))?,
        selected,
    )?;
    let latest = version(selected)?;
    if github.read("user")?["login"] != OWNER {
        return Err("WinGet cleanup requires the submission owner's account".into());
    }
    let fork = github.read(&format!("repos/{FORK}"))?;
    if fork["full_name"] != FORK || fork["parent"]["full_name"] != REPO || fork["fork"] != true {
        return Err("WinGet cleanup requires the matching submission fork".into());
    }
    let default = fork["default_branch"]
        .as_str()
        .ok_or("missing fork default branch")?;
    if submit {
        github.check_fork_credential()?;
    }
    // Discover live branches, including a closed PR whose deletion failed.
    // Deleted submission branches do not make future runs scan old PR history.
    for branch in github.list(&format!("repos/{FORK}/branches?per_page=100"))? {
        let name = branch["name"]
            .as_str()
            .ok_or("missing submission branch name")?;
        if name == default {
            continue;
        }
        let prs = github.list(&format!(
            "repos/{REPO}/pulls?state=all&head={OWNER}:{}&per_page=100",
            encode(name)
        ))?;
        for listed in prs {
            let number = listed["number"]
                .as_u64()
                .ok_or("missing submission PR number")?;
            let endpoint = format!("repos/{REPO}/pulls/{number}");
            let pr = github.read(&endpoint)?;
            if pr["merged"] != false
                || pr["user"]["login"] != OWNER
                || pr["head"]["repo"]["full_name"] != FORK
                || pr["head"]["ref"] != name
                || pr["base"]["repo"]["full_name"] != REPO
            {
                continue;
            }
            let files = github.list(&format!("{endpoint}/files?per_page=100"))?;
            let Some(old) = manifest_version(&files)? else {
                continue;
            };
            if version(old)? >= latest {
                continue;
            }
            let head = pr["head"]["sha"]
                .as_str()
                .ok_or("missing submission PR head")?;
            if head.len() != 40 || !head.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err("invalid submission PR head".into());
            }
            if branch_head(github, name)?.as_deref() != Some(head) {
                return Err(
                    format!("PR #{number}'s branch changed, inspect it before retrying").into(),
                );
            }
            println!("Superseded udpstp {old}: https://github.com/{REPO}/pull/{number}");
            if !submit {
                continue;
            }
            current(
                &github.read(&format!("repos/{APPLICATION}/releases/latest"))?,
                selected,
            )?;
            let fresh = github.read(&endpoint)?;
            if !same_submission(&fresh, &pr)
                || fresh["merged"] != false
                || fresh["state"] != pr["state"]
            {
                return Err("submission changed before closure, retry cleanup".into());
            }
            if fresh["state"] == "open" {
                github.close(number)?;
            }
            let closed = github.read(&endpoint)?;
            if closed["state"] != "closed"
                || closed["merged"] != false
                || !same_submission(&closed, &pr)
            {
                return Err("superseded PR closure could not be verified".into());
            }
            match branch_head(github, name)? {
                None => (),
                Some(actual) if actual == head => github.delete(name, head)?,
                Some(_) => {
                    return Err(
                        "submission branch changed after closure, inspect it before retrying"
                            .into(),
                    );
                }
            }
            if branch_head(github, name)?.is_some() {
                return Err("submission branch deletion could not be verified".into());
            }
        }
    }
    Ok(())
}

pub(super) fn run(args: &[String]) -> Result<()> {
    let [selected, mode] = args else {
        return Err("usage: winget-cleanup VERSION (--prepare|--submit)".into());
    };
    if !matches!(mode.as_str(), "--prepare" | "--submit") {
        return Err("expected --prepare or --submit".into());
    }
    cleanup(&Cli, selected, mode == "--submit")
}

#[cfg(test)]
#[path = "winget_cleanup_tests.rs"]
mod tests;
