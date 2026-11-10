//! Run the release workflow's lockfile refresh against a local Git remote.
#![cfg(unix)]

use std::{fs, path::PathBuf, process::Command};

struct Repository(PathBuf);

impl Repository {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("udpstp-release-pr-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let repo = Self(root);
        repo.git(&["init", "--quiet", "--initial-branch=main"]);
        repo.git(&["config", "user.name", "Release fixture"]);
        repo.git(&["config", "user.email", "fixture@localhost"]);
        repo.git(&["config", "core.hooksPath", "/dev/null"]);
        repo.git(&["config", "commit.gpgsign", "false"]);
        repo.write(
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/udpstp-core\", \"crates/udpstp-wire\"]\nexclude = [\"fuzz\"]\nresolver = \"3\"\n",
        );
        repo.packages("0.1.3");
        repo.write(
            "fuzz/Cargo.toml",
            "[package]\nname = \"udpstp-fuzz\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[workspace]\n[dependencies]\nudpstp-core = { path = \"../crates/udpstp-core\" }\nudpstp-wire = { path = \"../crates/udpstp-wire\" }\n",
        );
        repo.write("fuzz/src/lib.rs", "#![forbid(unsafe_code)]\n");
        repo.checked(Command::new("cargo").args([
            "generate-lockfile",
            "--manifest-path",
            "fuzz/Cargo.toml",
            "--offline",
        ]));
        repo.git(&["add", "."]);
        repo.git(&["commit", "--quiet", "-m", "baseline"]);
        repo.git(&["init", "--quiet", "--bare", "remote.git"]);
        repo.git(&["remote", "add", "origin", "remote.git"]);
        repo.git(&["push", "--quiet", "origin", "main"]);
        repo.git(&["switch", "--quiet", "-c", "release-plz-fixture"]);
        repo.packages("0.1.4");
        repo.git(&["add", "crates"]);
        repo.git(&["commit", "--quiet", "-m", "chore: release v0.1.4"]);
        repo.git(&["push", "--quiet", "origin", "HEAD"]);
        repo.git(&["switch", "--quiet", "main"]);
        repo
    }

    fn packages(&self, version: &str) {
        for name in ["udpstp-core", "udpstp-wire"] {
            self.write(
                &format!("crates/{name}/Cargo.toml"),
                &format!(
                    "[package]\nname = \"{name}\"\nversion = \"{version}\"\nedition = \"2024\"\n"
                ),
            );
            self.write(
                &format!("crates/{name}/src/lib.rs"),
                "#![forbid(unsafe_code)]\n",
            );
        }
    }

    fn write(&self, path: &str, content: &str) {
        let path = self.0.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn checked(&self, command: &mut Command) -> String {
        let output = command.current_dir(&self.0).output().unwrap();
        assert!(output.status.success(), "{command:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn git(&self, args: &[&str]) -> String {
        self.checked(Command::new("git").args(args))
    }

    fn refresh(&self, body: &str) {
        // Only credential setup is replaced. Fetch, checkout, Cargo, commit and
        // push run as written in the workflow, against this fixture's remote.
        self.checked(
            Command::new("bash")
                .args([
                    "-c",
                    &format!(
                        "gh() {{ test \"$*\" = 'auth setup-git --hostname github.com'; }};\n{body}"
                    ),
                ])
                .env("RELEASE_BRANCH", "release-plz-fixture")
                .env("CARGO_NET_OFFLINE", "true"),
        );
    }
}

impl Drop for Repository {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

// docs/releasing.md: refresh the release branch's local crate versions before
// enabling auto-merge, committing only when the lockfile changes.
#[test]
fn release_pr_refreshes_its_fuzz_lockfile_once_and_leaves_main_untouched() {
    let workflow: serde_json::Value =
        serde_saphyr::from_str(include_str!("../../.github/workflows/release-plz.yml")).unwrap();
    let steps = workflow["jobs"]["release-pr"]["steps"].as_array().unwrap();
    let refresh = steps
        .iter()
        .position(|step| step["id"] == "fuzz-lock")
        .unwrap();
    let release = steps
        .iter()
        .position(|step| step["id"] == "release-plz")
        .unwrap();
    let merge = steps
        .iter()
        .position(|step| step["name"] == "Merge the release pull request once CI passes")
        .unwrap();
    assert!(release < refresh && refresh < merge);
    assert_eq!(steps[refresh]["if"], steps[merge]["if"]);
    let body = steps[refresh]["run"].as_str().unwrap();

    let repo = Repository::new();
    let main = repo.git(&["rev-parse", "main"]);
    let release = repo.git(&["rev-parse", "release-plz-fixture"]);
    let stale = repo.git(&["show", "release-plz-fixture:fuzz/Cargo.lock"]);
    repo.refresh(body);
    let refreshed = repo.git(&["rev-parse", "HEAD"]);
    assert_ne!(refreshed, release);
    assert_eq!(repo.git(&["rev-parse", "HEAD^"]), release);
    assert_eq!(
        repo.git(&["diff", "--name-only", "HEAD^", "HEAD"]),
        "fuzz/Cargo.lock"
    );
    assert_eq!(
        repo.git(&["show", "HEAD:fuzz/Cargo.lock"]),
        stale.replace("version = \"0.1.3\"", "version = \"0.1.4\"")
    );
    assert_eq!(
        repo.git(&["rev-parse", "origin/release-plz-fixture"]),
        refreshed
    );
    assert_eq!(repo.git(&["rev-parse", "origin/main"]), main);
    repo.refresh(body);
    assert_eq!(repo.git(&["rev-parse", "HEAD"]), refreshed);
}
