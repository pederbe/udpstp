//! ADR 0017's latest-only distribution policy and WinGet interruption recovery.
use super::*;
use std::{cell::RefCell, collections::BTreeMap};

const HEAD: &str = "1111111111111111111111111111111111111111";

struct Fixture {
    release: Value,
    prs: RefCell<BTreeMap<u64, Value>>,
    branches: RefCell<BTreeMap<String, String>>,
    files: BTreeMap<u64, Vec<Value>>,
    writes: RefCell<Vec<String>>,
    fail_delete: RefCell<bool>,
    fail_close: RefCell<bool>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            release: json!({"tag_name":"v0.3.0","draft":false,"prerelease":false}),
            prs: RefCell::new(BTreeMap::new()),
            branches: RefCell::new(BTreeMap::new()),
            files: BTreeMap::new(),
            writes: RefCell::new(vec![]),
            fail_delete: RefCell::new(false),
            fail_close: RefCell::new(false),
        }
    }
    fn add(&mut self, number: u64, selected: &str) {
        let name = format!("submission-{number}");
        self.branches.borrow_mut().insert(name.clone(), HEAD.into());
        self.prs.borrow_mut().insert(
            number,
            json!({"number":number,"state":"open","merged":false,
            "user":{"login":OWNER},"base":{"repo":{"full_name":REPO}},
            "head":{"ref":name,"sha":HEAD,"repo":{"full_name":FORK}}}),
        );
        self.files.insert(
            number,
            [
                "pederbe.udpstp.yaml",
                "pederbe.udpstp.installer.yaml",
                "pederbe.udpstp.locale.en-US.yaml",
            ]
            .into_iter()
            .map(|name| json!({"filename":format!("{PREFIX}{selected}/{name}"),"status":"added"}))
            .collect(),
        );
    }
    fn untouched(&self, number: u64) {
        assert_eq!(self.prs.borrow()[&number]["state"], "open");
        assert!(
            self.branches
                .borrow()
                .contains_key(&format!("submission-{number}"))
        );
    }
}

impl Github for Fixture {
    fn read(&self, endpoint: &str) -> Result<Value> {
        match endpoint {
            "user" => Ok(json!({"login":OWNER})),
            "repos/pederbe/udpstp/releases/latest" => Ok(self.release.clone()),
            "repos/pederbe/winget-pkgs" => Ok(json!({"full_name":FORK,"fork":true,
                "parent":{"full_name":REPO},"default_branch":"master"})),
            _ => {
                let number: u64 = endpoint
                    .strip_prefix(&format!("repos/{REPO}/pulls/"))
                    .ok_or("unexpected read")?
                    .parse()?;
                Ok(self.prs.borrow().get(&number).ok_or("unknown PR")?.clone())
            }
        }
    }
    fn list(&self, endpoint: &str) -> Result<Vec<Value>> {
        if endpoint == format!("repos/{FORK}/branches?per_page=100") {
            return Ok(std::iter::once(json!({"name":"master"}))
                .chain(
                    self.branches
                        .borrow()
                        .keys()
                        .map(|name| json!({"name":name})),
                )
                .collect());
        }
        if let Some(name) = endpoint.strip_prefix(&format!("repos/{FORK}/git/matching-refs/heads/"))
        {
            return Ok(self
                .branches
                .borrow()
                .get(name)
                .map(|head| vec![json!({"ref":format!("refs/heads/{name}"),"object":{"sha":head}})])
                .unwrap_or_default());
        }
        if let Some(name) =
            endpoint.strip_prefix(&format!("repos/{REPO}/pulls?state=all&head={OWNER}:"))
        {
            let name = name
                .strip_suffix("&per_page=100")
                .ok_or("missing pagination")?;
            return Ok(self
                .prs
                .borrow()
                .values()
                .filter(|p| p["head"]["ref"] == name)
                .cloned()
                .collect());
        }
        if let Some(number) = endpoint.strip_prefix(&format!("repos/{REPO}/pulls/")) {
            let number: u64 = number
                .strip_suffix("/files?per_page=100")
                .ok_or("missing file pagination")?
                .parse()?;
            return Ok(self.files.get(&number).ok_or("unknown PR files")?.clone());
        }
        Err(format!("unexpected list: {endpoint}").into())
    }
    fn check_fork_credential(&self) -> Result<()> {
        Ok(())
    }
    fn close(&self, number: u64) -> Result<()> {
        self.writes.borrow_mut().push(format!("close {number}"));
        self.prs.borrow_mut().get_mut(&number).unwrap()["state"] = "closed".into();
        if *self.fail_close.borrow() {
            *self.fail_close.borrow_mut() = false;
            return Err("connection lost after closure".into());
        }
        Ok(())
    }
    fn delete(&self, branch: &str, head: &str) -> Result<()> {
        assert_eq!(
            self.branches.borrow().get(branch).map(String::as_str),
            Some(head)
        );
        assert!(
            self.prs
                .borrow()
                .values()
                .any(|p| p["head"]["ref"] == branch && p["state"] == "closed")
        );
        if *self.fail_delete.borrow() {
            *self.fail_delete.borrow_mut() = false;
            return Err("fork credential temporarily unavailable".into());
        }
        self.writes.borrow_mut().push(format!("delete {branch}"));
        self.branches.borrow_mut().remove(branch);
        Ok(())
    }
}

#[test]
fn release_delivery_retires_only_older_unmerged_udpstp_submissions() {
    let mut f = Fixture::new();
    f.add(1, "0.1.1");
    f.add(2, "0.2.0");
    f.add(3, "0.3.0");
    f.add(4, "0.10.0");
    f.add(5, "0.1.0");
    f.prs.borrow_mut().get_mut(&5).unwrap()["merged"] = true.into();
    f.add(6, "0.1.0");
    f.prs.borrow_mut().get_mut(&6).unwrap()["user"]["login"] = "another".into();
    f.add(7, "0.1.0");
    f.files.get_mut(&7).unwrap().iter_mut().for_each(|file| {
        file["filename"] = file["filename"]
            .as_str()
            .unwrap()
            .replace("udpstp", "another")
            .into();
    });
    cleanup(&f, "0.3.0", true).unwrap();
    assert_eq!(
        *f.writes.borrow(),
        [
            "close 1",
            "delete submission-1",
            "close 2",
            "delete submission-2"
        ]
    );
    for number in 3..=7 {
        f.untouched(number);
    }
    cleanup(&f, "0.3.0", true).unwrap();
    assert_eq!(f.writes.borrow().len(), 4);
}

#[test]
fn preparation_reports_superseded_prs_without_changing_them() {
    let mut f = Fixture::new();
    f.add(1, "0.1.1");
    cleanup(&f, "0.3.0", false).unwrap();
    f.untouched(1);
    assert!(f.writes.borrow().is_empty());
}

#[test]
fn stale_release_dispatch_and_unpublished_versions_cannot_change_prs() {
    let mut f = Fixture::new();
    f.add(1, "0.1.1");
    for selected in ["0.2.0", "0.4.0", "0.3.0-beta.1", "0.03.0", "0.3"] {
        assert!(cleanup(&f, selected, true).is_err());
    }
    f.release["draft"] = true.into();
    assert!(cleanup(&f, "0.3.0", true).is_err());
    f.release["draft"] = false.into();
    f.release["prerelease"] = true.into();
    assert!(cleanup(&f, "0.3.0", true).is_err());
    f.untouched(1);
    assert!(f.writes.borrow().is_empty());
}

#[test]
fn retry_finishes_deletion_after_a_closed_pr_or_an_uncertain_close() {
    for failed_stage in ["close", "delete"] {
        let mut f = Fixture::new();
        f.add(1, "0.1.1");
        if failed_stage == "close" {
            *f.fail_close.borrow_mut() = true;
        } else {
            *f.fail_delete.borrow_mut() = true;
        }
        assert!(cleanup(&f, "0.3.0", true).is_err());
        assert_eq!(f.prs.borrow()[&1]["state"], "closed");
        assert!(f.branches.borrow().contains_key("submission-1"));
        cleanup(&f, "0.3.0", true).unwrap();
        assert_eq!(*f.writes.borrow(), ["close 1", "delete submission-1"]);
    }
}

#[test]
fn changed_branches_and_mixed_manifest_prs_require_inspection() {
    for changed in ["branch", "files"] {
        let mut f = Fixture::new();
        f.add(1, "0.1.1");
        if changed == "branch" {
            f.branches
                .borrow_mut()
                .insert("submission-1".into(), "2".repeat(40));
        } else {
            f.files
                .get_mut(&1)
                .unwrap()
                .push(json!({"filename":"unrelated.txt","status":"added"}));
        }
        assert!(cleanup(&f, "0.3.0", true).is_err());
        f.untouched(1);
        assert!(f.writes.borrow().is_empty());
    }
}

#[test]
fn branch_deletion_checks_the_expected_commit_atomically() {
    let request = deletion("fork-node-id", "submission", HEAD);
    let input = &request["variables"]["input"];
    assert_eq!(input["repositoryId"], "fork-node-id");
    assert_eq!(input["refUpdates"][0]["name"], "refs/heads/submission");
    assert_eq!(input["refUpdates"][0]["beforeOid"], HEAD);
    assert_eq!(
        input["refUpdates"][0]["afterOid"],
        "0000000000000000000000000000000000000000"
    );
}

#[test]
fn catalog_removals_and_metadata_repairs_are_not_pending_version_submissions() {
    for status in ["removed", "modified"] {
        let mut f = Fixture::new();
        f.add(1, "0.1.1");
        for file in f.files.get_mut(&1).unwrap() {
            file["status"] = status.into();
        }
        cleanup(&f, "0.3.0", true).unwrap();
        f.untouched(1);
        assert!(f.writes.borrow().is_empty());
    }
}
