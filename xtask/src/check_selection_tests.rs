//! Exercise the maintained shell selector with real changed Git paths.
#![cfg(unix)]

use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Repository(PathBuf);

impl Repository {
    fn new(paths: &[&str]) -> (Self, String, String) {
        let root = std::env::temp_dir().join(format!(
            "udpstp-check-selection-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let this = Self(root);
        this.git(&["init", "--quiet"]);
        this.git(&["config", "user.name", "Check selection fixture"]);
        this.git(&["config", "user.email", "fixture@localhost"]);
        fs::write(this.0.join("baseline"), "baseline\n").unwrap();
        this.git(&["add", "."]);
        this.git(&["commit", "--quiet", "-m", "baseline"]);
        let base = this.git(&["rev-parse", "HEAD"]);
        for path in paths {
            let path = this.0.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "changed\n").unwrap();
        }
        this.git(&["add", "."]);
        this.git(&["commit", "--quiet", "-m", "changed"]);
        let head = this.git(&["rev-parse", "HEAD"]);
        (this, base, head)
    }

    fn git(&self, args: &[&str]) -> String {
        let output = Command::new("git")
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(&self.0)
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn docs_only(&self, base: &str, head: &str) -> bool {
        let script = include_str!("../../scripts/check-commit.sh");
        let selector = script
            .split_once("docs_only() {")
            .unwrap()
            .1
            .split_once("\nif docs_only; then")
            .unwrap()
            .0;
        Command::new("sh")
            .args(["-c", &format!("docs_only() {{{selector}\ndocs_only")])
            .env("repo", &self.0)
            .env("base", base)
            .env("commit", head)
            .status()
            .unwrap()
            .success()
    }
}

impl Drop for Repository {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn only_independent_documentation_selects_documentation_checks() {
    for path in [
        "README.md",
        "docs/guides/results.md",
        "website/pages/index.md",
    ] {
        let (repo, base, head) = Repository::new(&[path]);
        assert!(repo.docs_only(&base, &head), "{path}");
        assert!(!repo.docs_only("", &head), "missing base: {path}");
        assert!(
            !repo.docs_only("not-a-commit", &head),
            "invalid base: {path}"
        );
        assert!(!repo.docs_only(&head, &head), "empty change: {path}");
    }
    for path in [
        "crates/udpstp/src/main.rs",
        "crates/udpstp/tests/fixtures/model.md",
        "scripts/check.sh",
        "xtask/fixtures/README.md",
        "fuzz/README.md",
        "tools/example/README.md",
        "tests/fixtures/model.md",
        "fixtures/model.md",
        "examples/README.md",
        "contrib/package-repository/publish.sh",
        "contrib/container/Release.Containerfile",
        "contrib/homebrew/udpstp.rb",
        "contrib/windows/udpstp-server.ps1",
        "contrib/systemd/udpstp-server.service",
        "contrib/README.md",
        ".github/workflows/ci.yml",
        "docs/adr/example.md",
        "docs/conformance/records/example.md",
        "docs/rfc/rfc9946.txt",
        "docs/guides/windows.md",
        "Cargo.lock",
        "unknown-input",
    ] {
        let (repo, base, head) = Repository::new(&[path]);
        assert!(!repo.docs_only(&base, &head), "{path}");
        let (repo, base, head) = Repository::new(&["README.md", path]);
        assert!(!repo.docs_only(&base, &head), "mixed change: {path}");
    }
}

#[test]
fn moving_application_input_to_documentation_keeps_full_checks() {
    let (repo, _, base) = Repository::new(&["crates/udpstp/fixture.md"]);
    fs::create_dir(repo.0.join("docs")).unwrap();
    repo.git(&["mv", "crates/udpstp/fixture.md", "docs/fixture.md"]);
    repo.git(&["commit", "--quiet", "-m", "move fixture"]);
    let head = repo.git(&["rev-parse", "HEAD"]);
    assert!(!repo.docs_only(&base, &head));
}

#[test]
fn check_wrapper_passes_only_the_supplied_comparison_base() {
    for base in [None, Some("captured-main-revision")] {
        let mut command = Command::new("bash");
        command
            .args([
                "-c",
                "timeout() { printf '%s\\n' \"$@\"; }; export -f timeout; bash scripts/check.sh",
            ])
            .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."));
        if let Some(base) = base {
            command.env("CHECK_BASE", base);
        } else {
            command.env_remove("CHECK_BASE");
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!(
                "--kill-after=30\n20m\nsh\nscripts/check-commit.sh\n.\nHEAD\n{}\n",
                base.unwrap_or_default()
            )
        );
    }
}
