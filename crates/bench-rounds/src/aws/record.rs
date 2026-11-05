//! The raw records of a Round's real calls to AWS: each call's argv, exit
//! status, duration, standard output and standard error, kept where only
//! this user can read them and never where a commit could take them.
//! Nothing of them is committed: [`super::replay`] checks the fake against
//! them where they are, by hand.
//!
//! - **Where:** `$XDG_STATE_HOME/udpstp-bench/calls/<name>/` (by default
//!   under `~/.local/state`), `<name>` the Round's ID: a directory made
//!   for the Round. It is refused inside any git checkout, found by
//!   itself, and inside each directory the caller names (the Round's
//!   `--out`), whether or not that exists yet.
//! - **What:** for call `N`, `N.json` (its argv after `aws`, its status and
//!   seconds), `N.stdout` and `N.stderr`.
//! - **Only this user's, checked:** the directory is made 0700 and each
//!   file 0600, and each mode is read back after it is made, before
//!   anything is written into it: one that any group or other bit is set
//!   in is refused, and removed. A filesystem that keeps no modes, as
//!   WSL's `/mnt/c` without `metadata`, shows 0777 whatever was asked, so
//!   the records are never kept there. Where modes can't be read at all
//!   (Windows), no records are kept.
//! - **Never credentials:** the output of `configure export-credentials`
//!   is never written, only whether it held an `Expiration` and its
//!   value; nor any output of a `configure` call, nor an output that looks
//!   like credentials (an access key ID, `SecretAccessKey`,
//!   `SessionToken`), which is withheld and said so.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use indexmap::IndexMap;

use crate::py::{self, Json};

/// What a call printed on its standard output, as the recorder is given
/// it.
#[derive(Clone, Copy, Debug)]
pub enum Stdout<'a> {
    Bytes(&'a [u8]),
    /// `configure export-credentials`: only when they expire.
    Credentials {
        expiration: Option<&'a str>,
    },
}

/// One finished call.
#[derive(Clone, Copy, Debug)]
pub struct Call<'a> {
    /// What followed `aws`: the call's words and flags, the region and the
    /// output format. Never the CLI's own path.
    pub argv: &'a [String],
    /// Its exit status, `None` where a signal ended it.
    pub status: Option<i32>,
    pub seconds: f64,
    pub stdout: Stdout<'a>,
    pub stderr: &'a [u8],
}

/// A call as its raw record holds it.
#[derive(Clone, Debug, PartialEq)]
pub struct RawRecord {
    pub argv: Vec<String>,
    pub status: Option<i32>,
    pub seconds: f64,
    /// `None` where it was withheld.
    pub stdout: Option<Vec<u8>>,
    /// For `configure export-credentials`: whether it held an
    /// `Expiration`, and its value.
    pub credentials: Option<Option<String>>,
    /// `None` where it was withheld.
    pub stderr: Option<Vec<u8>>,
}

/// Where the raw records of every Round go: `calls` in the tools' state
/// directory, `$XDG_STATE_HOME/udpstp-bench`, with `~/.local/state` for an
/// unset `XDG_STATE_HOME`, as the ledger's.
pub fn calls_directory(xdg_state_home: Option<&str>, home: &Path) -> PathBuf {
    let base = match xdg_state_home {
        Some(base) => PathBuf::from(base),
        None => home.join(".local").join("state"),
    };
    base.join("udpstp-bench").join("calls")
}

/// Records a Round's calls into a directory of its own.
#[derive(Debug)]
pub struct Recorder {
    dir: PathBuf,
    next: u32,
}

impl Recorder {
    /// Makes `calls/<name>/` for a Round: `name` one plain path component;
    /// the directory new and only this user's, checked; outside any git
    /// checkout, and outside each of `refuse_under` (the Round's `--out`),
    /// which needn't exist yet. Refused where modes can't be checked.
    pub fn create(calls: &Path, name: &str, refuse_under: &[&Path]) -> io::Result<Recorder> {
        if !cfg!(unix) {
            return Err(io::Error::other(
                "the raw records of AWS calls are kept only where their privacy can be checked \
                 (Linux, macOS): this system's file modes can't be",
            ));
        }
        let plain = !name.is_empty()
            && name != "."
            && name != ".."
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
        if !plain {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{name:?} isn't a plain name for a Round's records"),
            ));
        }
        // Refused before anything is made there.
        let resolved = resolve(calls)?;
        let outside = |place: String| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "the records would be in {}, inside {place}: they must stay outside it",
                    resolved.display()
                ),
            )
        };
        if let Some(checkout) = git_checkout(&resolved) {
            return Err(outside(format!("the git checkout {}", checkout.display())));
        }
        for forbidden in refuse_under {
            // Resolved as far as it exists: an --out not made yet counts.
            let forbidden = resolve(forbidden)?;
            if resolved.starts_with(&forbidden) {
                return Err(outside(forbidden.display().to_string()));
            }
        }
        private_dirs(calls)?;
        let dir = fs::canonicalize(calls)?.join(name);
        private_dir(&dir)?;
        Ok(Recorder { dir, next: 1 })
    }

    /// The Round's directory of records.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Writes one call's record: `N.json`, and `N.stdout` and `N.stderr`
    /// where they may be written.
    pub fn record(&mut self, call: &Call) -> io::Result<()> {
        let n = format!("{:04}", self.next);
        self.next += 1;
        let configure = call.argv.first().map(String::as_str) == Some("configure");
        let mut meta = IndexMap::from([
            (
                "argv".to_string(),
                Json::List(call.argv.iter().map(|a| Json::Str(a.clone())).collect()),
            ),
            (
                "status".to_string(),
                call.status.map_or(Json::Null, |s| Json::Int(s.into())),
            ),
            ("seconds".to_string(), Json::Float(call.seconds)),
        ]);
        match call.stdout {
            Stdout::Credentials { expiration } => {
                meta.insert("stdout".into(), Json::Str("withheld: credentials".into()));
                meta.insert(
                    "credentials".into(),
                    Json::Dict(IndexMap::from([(
                        "expiration".to_string(),
                        expiration.map_or(Json::Null, |e| Json::Str(e.to_string())),
                    )])),
                );
            }
            Stdout::Bytes(_) if configure => {
                meta.insert(
                    "stdout".into(),
                    Json::Str("withheld: a configure call".into()),
                );
            }
            Stdout::Bytes(bytes) if looks_like_credentials(bytes) => {
                meta.insert(
                    "stdout".into(),
                    Json::Str("withheld: it looks like credentials".into()),
                );
            }
            Stdout::Bytes(bytes) => {
                private_file(&self.dir.join(format!("{n}.stdout")), bytes)?;
                meta.insert("stdout".into(), Json::Str(format!("{n}.stdout")));
            }
        }
        if looks_like_credentials(call.stderr) {
            meta.insert(
                "stderr".into(),
                Json::Str("withheld: it looks like credentials".into()),
            );
        } else {
            private_file(&self.dir.join(format!("{n}.stderr")), call.stderr)?;
            meta.insert("stderr".into(), Json::Str(format!("{n}.stderr")));
        }
        let text = py::dumps_indent(&Json::Dict(meta), 1) + "\n";
        private_file(&self.dir.join(format!("{n}.json")), text.as_bytes())
    }
}

/// Reads a Round's raw records back, in the order they were made.
pub fn read(dir: &Path) -> io::Result<Vec<RawRecord>> {
    let mut names: Vec<String> = fs::read_dir(dir)?
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.ends_with(".json"))
        .collect();
    names.sort();
    let bad = |what: String| io::Error::new(io::ErrorKind::InvalidData, what);
    let mut records = Vec::new();
    for name in names {
        let meta = Json::parse_bytes(&fs::read(dir.join(&name))?)
            .map_err(|e| bad(format!("{name}: {e}")))?;
        let field = |key: &str| meta.item(key).map_err(|e| bad(format!("{name}: {e}")));
        let argv = field("argv")?
            .as_list()
            .ok_or_else(|| bad(format!("{name}: argv isn't a list")))?
            .iter()
            .map(|a| a.as_str().map(str::to_string))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| bad(format!("{name}: an argument isn't a string")))?;
        let status = match field("status")? {
            Json::Null => None,
            Json::Int(s) => {
                Some(i32::try_from(*s).map_err(|_| bad(format!("{name}: the status {s}")))?)
            }
            other => {
                return Err(bad(format!(
                    "{name}: the status is a {}",
                    other.type_name()
                )));
            }
        };
        let seconds = match field("seconds")? {
            Json::Float(s) => *s,
            Json::Int(s) => *s as f64,
            other => return Err(bad(format!("{name}: seconds is a {}", other.type_name()))),
        };
        let file = |key: &str| -> io::Result<Option<Vec<u8>>> {
            match field(key)?.as_str() {
                Some(file) if !file.starts_with("withheld") => {
                    if file.contains(['/', '\\']) || file.starts_with('.') {
                        return Err(bad(format!("{name}: the file {file:?}")));
                    }
                    fs::read(dir.join(file)).map(Some)
                }
                _ => Ok(None),
            }
        };
        let credentials = match meta.get("credentials") {
            Ok(Json::Dict(c)) => Some(
                c.get("expiration")
                    .and_then(Json::as_str)
                    .map(str::to_string),
            ),
            _ => None,
        };
        records.push(RawRecord {
            argv,
            status,
            seconds,
            stdout: file("stdout")?,
            credentials,
            stderr: file("stderr")?,
        });
    }
    Ok(records)
}

/// Whether bytes hold what could be credentials: an access key ID
/// (`AKIA` or `ASIA` and 16 capitals or digits), or the words of the
/// credentials' fields.
pub fn looks_like_credentials(bytes: &[u8]) -> bool {
    const WORDS: [&[u8]; 5] = [
        b"SecretAccessKey",
        b"SessionToken",
        b"aws_secret_access_key",
        b"aws_session_token",
        b"AccessKeyId",
    ];
    let has = |word: &[u8]| bytes.windows(word.len()).any(|w| w == word);
    WORDS.iter().any(|w| has(w)) || access_key_id(bytes)
}

/// An access key ID: `AKIA` or `ASIA`, then 16 capitals or digits.
fn access_key_id(bytes: &[u8]) -> bool {
    bytes.windows(20).any(|w| {
        (w.starts_with(b"AKIA") || w.starts_with(b"ASIA"))
            && w[4..]
                .iter()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
    })
}

/// `path` made absolute and free of links as far as it exists, the rest
/// of it after that.
fn resolve(path: &Path) -> io::Result<PathBuf> {
    let mut existing = path.to_path_buf();
    let mut rest = Vec::new();
    loop {
        let probe = if existing.as_os_str().is_empty() {
            Path::new(".")
        } else {
            existing.as_path()
        };
        match fs::canonicalize(probe) {
            Ok(mut found) => {
                found.extend(rest.iter().rev());
                return Ok(found);
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let name = existing.file_name().ok_or(e)?.to_os_string();
                rest.push(name);
                existing.pop();
            }
            Err(e) => return Err(e),
        }
    }
}

/// The git checkout `path` is in, if any: the nearest of it and its
/// parents that holds a `.git`, a directory or a worktree's file.
fn git_checkout(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|dir| fs::symlink_metadata(dir.join(".git")).is_ok())
        .map(Path::to_path_buf)
}

/// Makes `dir` and its missing parents, each only its user's.
fn private_dirs(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

/// Makes `dir` new and only its user's, checked; removed again where
/// others could reach it.
fn private_dir(dir: &Path) -> io::Result<()> {
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let checked = fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .and_then(|()| only_its_users(dir, &fs::symlink_metadata(dir)?));
        if let Err(e) = checked {
            let _ = fs::remove_dir(dir);
            return Err(e);
        }
    }
    Ok(())
}

/// Writes a new file, only its user's from creation: its mode is checked
/// before anything is written into it, and it is removed again where
/// others could reach it.
fn private_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path)?;
    if let Err(e) = file.metadata().and_then(|meta| only_its_users(path, &meta)) {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(e);
    }
    file.write_all(bytes)
}

/// Refuses what others could reach: a mode any group or other bit is set
/// in. A filesystem that keeps no modes, as WSL's `/mnt/c` without
/// `metadata`, shows 0777 whatever was asked; where modes can't be read at
/// all, nothing is private for certain.
fn only_its_users(path: &Path, meta: &fs::Metadata) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 == 0 {
            return Ok(());
        }
        Err(io::Error::other(format!(
            "{} has mode {mode:03o}, which others can reach: its filesystem doesn't keep the \
             mode asked for (as WSL's /mnt/c without `metadata`), so the records can't be kept \
             there; set XDG_STATE_HOME to a directory on a filesystem that does",
            path.display()
        )))
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        Err(io::Error::other(format!(
            "{}: this system's file modes can't be checked",
            path.display()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> TempDir {
            let dir = std::env::temp_dir()
                .join(format!("bench-rounds-record-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(unix)]
    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    /// Every file under `dir`, with its bytes.
    #[cfg(unix)]
    fn everything(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut found = Vec::new();
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                found.extend(everything(&path));
            } else {
                found.push((path.clone(), fs::read(&path).unwrap()));
            }
        }
        found
    }

    #[cfg(unix)]
    #[test]
    fn records_each_call_and_reads_it_back() {
        let tmp = TempDir::new("calls");
        let calls = calls_directory(Some(tmp.0.to_str().unwrap()), Path::new("/nowhere"));
        let mut recorder = Recorder::create(&calls, "r20260929T120000-ab12", &[]).unwrap();
        let describe = argv(&[
            "ec2",
            "describe-subnets",
            "--region",
            "eu-north-1",
            "--output",
            "json",
        ]);
        recorder
            .record(&Call {
                argv: &describe,
                status: Some(0),
                seconds: 1.25,
                stdout: Stdout::Bytes(b"{\"Subnets\": []}\n"),
                stderr: b"",
            })
            .unwrap();
        let records = read(recorder.dir()).unwrap();
        assert_eq!(
            records,
            [RawRecord {
                argv: describe,
                status: Some(0),
                seconds: 1.25,
                stdout: Some(b"{\"Subnets\": []}\n".to_vec()),
                credentials: None,
                stderr: Some(Vec::new()),
            }]
        );
        // A Round's directory is its own: never reused.
        assert!(Recorder::create(&calls, "r20260929T120000-ab12", &[]).is_err());
        assert!(Recorder::create(&calls, "../x", &[]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn credentials_are_never_written() {
        let tmp = TempDir::new("secret");
        let mut recorder = Recorder::create(&tmp.0.join("calls"), "r1", &[]).unwrap();
        let secret = b"{\"Version\": 1, \"AccessKeyId\": \"ASIAABCDEFGHIJKLMNOP\", \
                       \"SecretAccessKey\": \"wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY\", \
                       \"SessionToken\": \"IQoJb3JpZ2luX2VjEXAMPLE\", \"Expiration\": \"2026-09-29T20:00:00+00:00\"}";
        recorder
            .record(&Call {
                argv: &argv(&["configure", "export-credentials", "--format", "process"]),
                status: Some(0),
                seconds: 0.5,
                stdout: Stdout::Credentials {
                    expiration: Some("2026-09-29T20:00:00+00:00"),
                },
                stderr: b"",
            })
            .unwrap();
        // Handed the output itself by mistake, it withholds it too.
        recorder
            .record(&Call {
                argv: &argv(&["configure", "export-credentials", "--format", "process"]),
                status: Some(0),
                seconds: 0.5,
                stdout: Stdout::Bytes(secret),
                stderr: b"",
            })
            .unwrap();
        recorder
            .record(&Call {
                argv: &argv(&["sts", "get-caller-identity"]),
                status: Some(255),
                seconds: 0.5,
                stdout: Stdout::Bytes(secret),
                stderr: b"aws_secret_access_key = wJalrXUtnFEMI",
            })
            .unwrap();
        for (path, bytes) in everything(&tmp.0) {
            for needle in [
                &b"ASIAABCDEFGHIJKLMNOP"[..],
                b"wJalrXUtnFEMI",
                b"IQoJb3JpZ2lu",
            ] {
                assert!(
                    !bytes.windows(needle.len()).any(|w| w == needle),
                    "{} holds a credential",
                    path.display()
                );
            }
        }
        let records = read(recorder.dir()).unwrap();
        assert_eq!(
            records[0].credentials,
            Some(Some("2026-09-29T20:00:00+00:00".to_string()))
        );
        assert_eq!(
            (records[1].stdout.as_ref(), records[2].stderr.as_ref()),
            (None, None)
        );
    }

    #[cfg(unix)]
    #[test]
    fn records_stay_outside_the_rounds_output() {
        let tmp = TempDir::new("output");
        let out = tmp.0.join("results").join("round");
        fs::create_dir_all(&out).unwrap();
        let inside = out.join("state").join("udpstp-bench").join("calls");
        assert!(Recorder::create(&inside, "r1", &[&out]).is_err());
        assert!(!inside.exists(), "nothing is made there");
        assert!(Recorder::create(&tmp.0.join("state"), "r1", &[&out]).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn records_stay_outside_the_rounds_output_before_it_exists() {
        // An --out not made yet, which fs::canonicalize can't resolve.
        let tmp = TempDir::new("later");
        let out = tmp.0.join("results").join("round");
        let inside = out.join("calls");
        let refused = Recorder::create(&inside, "r1", &[&out]).unwrap_err();
        assert!(
            refused.to_string().contains("they must stay outside it"),
            "{refused}"
        );
        assert!(!out.exists(), "nothing is made there");
        // Through a link to where it will be, too.
        std::os::unix::fs::symlink(&tmp.0, tmp.0.join("link")).unwrap();
        let through = tmp
            .0
            .join("link")
            .join("results")
            .join("round")
            .join("calls");
        assert!(Recorder::create(&through, "r1", &[&out]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn records_stay_outside_any_git_checkout() {
        // A checkout found by itself, whatever the caller names: its .git
        // a directory, or a worktree's file.
        let tmp = TempDir::new("git");
        for (checkout, git) in [("repo", true), ("worktree", false)] {
            let checkout = tmp.0.join(checkout);
            fs::create_dir_all(&checkout).unwrap();
            if git {
                fs::create_dir(checkout.join(".git")).unwrap();
            } else {
                fs::write(checkout.join(".git"), "gitdir: /elsewhere\n").unwrap();
            }
            let inside = checkout.join("deep").join("state").join("calls");
            let refused = Recorder::create(&inside, "r1", &[]).unwrap_err();
            assert!(
                refused.to_string().contains("the git checkout"),
                "{refused}"
            );
            assert!(!checkout.join("deep").exists(), "nothing is made there");
        }
        assert!(Recorder::create(&tmp.0.join("state"), "r1", &[]).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn a_mode_others_can_reach_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new("reach");
        let (file, dir) = (tmp.0.join("f"), tmp.0.join("d"));
        fs::write(&file, "x").unwrap();
        fs::create_dir(&dir).unwrap();
        let set =
            |p: &Path, mode| fs::set_permissions(p, fs::Permissions::from_mode(mode)).unwrap();
        let check = |p: &Path| only_its_users(p, &fs::symlink_metadata(p).unwrap());
        for (mode, private) in [
            (0o600, true),
            (0o400, true),
            (0o640, false),
            (0o604, false),
            (0o777, false),
        ] {
            set(&file, mode);
            assert_eq!(check(&file).is_ok(), private, "{mode:o}");
        }
        for (mode, private) in [
            (0o700, true),
            (0o750, false),
            (0o705, false),
            (0o777, false),
        ] {
            set(&dir, mode);
            assert_eq!(check(&dir).is_ok(), private, "{mode:o}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn no_records_where_the_filesystem_keeps_no_modes() {
        // A directory on a filesystem that shows 0777 whatever is asked,
        // such as WSL's /mnt/c without `metadata`, outside any checkout:
        // run by hand, as `UDPSTP_BENCH_MODELESS_DIR=/mnt/c/Users/me/tmp
        // cargo test -p bench-rounds modes`.
        let Some(modeless) = std::env::var_os("UDPSTP_BENCH_MODELESS_DIR") else {
            eprintln!(
                "UDPSTP_BENCH_MODELESS_DIR isn't set: a filesystem without modes isn't tried"
            );
            return;
        };
        let calls =
            PathBuf::from(modeless).join(format!("bench-rounds-modeless-{}", std::process::id()));
        let refused = Recorder::create(&calls, "r1", &[]).unwrap_err();
        assert!(
            refused.to_string().contains("which others can reach"),
            "{refused}"
        );
        assert!(
            !calls.join("r1").exists(),
            "the directory others could reach is removed"
        );
        let _ = fs::remove_dir_all(&calls);
    }

    #[cfg(not(unix))]
    #[test]
    fn no_records_where_modes_cant_be_checked() {
        let tmp = TempDir::new("windows");
        assert!(Recorder::create(&tmp.0.join("calls"), "r1", &[]).is_err());
        assert!(!tmp.0.join("calls").exists());
    }

    #[cfg(unix)]
    #[test]
    fn records_are_only_their_users() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new("modes");
        let mut recorder = Recorder::create(&tmp.0.join("a").join("calls"), "r1", &[]).unwrap();
        recorder
            .record(&Call {
                argv: &argv(&["sts", "get-caller-identity"]),
                status: Some(0),
                seconds: 0.1,
                stdout: Stdout::Bytes(b"{}"),
                stderr: b"",
            })
            .unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(recorder.dir()), 0o700);
        assert_eq!(mode(&tmp.0.join("a")), 0o700);
        for (path, _) in everything(recorder.dir()) {
            assert_eq!(mode(&path), 0o600, "{}", path.display());
        }
    }
}
