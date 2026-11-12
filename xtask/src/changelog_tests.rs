use std::{fs, path::PathBuf};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

#[cfg(target_os = "linux")]
#[test]
fn changelog_includes_installation_changes_without_expanding_commit_bodies() {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let directory = std::env::temp_dir().join(format!(
        "udpstp-changelog-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(directory.join("scripts")).unwrap();
    for path in ["release-plz.toml", "scripts/changelog.sh"] {
        fs::copy(root().join(path), directory.join(path)).unwrap();
    }
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .args(args)
            .current_dir(&directory)
            .env("GIT_AUTHOR_NAME", "Fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "Fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    };
    git(&["init", "--quiet"]);
    fs::write(directory.join("installation"), "original").unwrap();
    git(&["add", "."]);
    git(&["commit", "--quiet", "-m", "fix: previous release (#1001)"]);
    git(&["tag", "v0.5.0"]);
    for (index, message) in [
        "fix: restore package installation (#1002)",
        "feat!: remove old switch (#1003)\n\nMigration instructions stay in the PR Solution.",
        "ci: tidy automation (#1004)",
    ]
    .into_iter()
    .enumerate()
    {
        fs::write(directory.join("installation"), index.to_string()).unwrap();
        git(&["add", "installation"]);
        git(&["commit", "--quiet", "-m", message]);
        if index == 0 {
            git(&["tag", "v0.5.1"]);
        }
    }
    let mut child = Command::new("bash")
        .arg("scripts/changelog.sh")
        .current_dir(&directory)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"# Changelog\n\nUDPSTP_REPOSITORY_CHANGELOG 0.6.0 2026-11-12 0.5.0\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let notes = String::from_utf8(output.stdout).unwrap();
    assert!(notes.contains("## [0.6.0] - 2026-11-12"), "{notes}");
    assert!(
        notes.contains(
            "Restore package installation ([#1002](https://github.com/pederbe/udpstp/pull/1002))"
        ),
        "{notes}"
    );
    assert!(
        notes.contains(
            "**Breaking:** Remove old switch ([#1003](https://github.com/pederbe/udpstp/pull/1003))"
        ),
        "{notes}"
    );
    for omitted in [
        "previous release",
        "Migration instructions",
        "Tidy automation",
        "[0.5.1]",
    ] {
        assert!(!notes.contains(omitted), "{omitted}: {notes}");
    }
    fs::remove_dir_all(directory).unwrap();
}
