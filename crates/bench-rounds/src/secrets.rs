//! A private directory for a Round's keys, and random hex from the OS's
//! generator.
//!
//! The directory is outside the Round's output, in the system's temporary
//! directory, under a random name, and only its owner may enter it (0700
//! on Unix; on Windows, the user's own temporary directory keeps others
//! out). Its files are the owner's alone from their creation (0600). It is
//! removed at the end of the Round Teardown, and again on `Drop`, so that
//! no way out of a Round, a panic included, leaves the keys behind.
//!
//! A rented host's SSH key is made there too, by `ssh-keygen`
//! ([`ssh_key`]).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::process::{RunError, Runner, lossy_text};
use crate::round::Stopped;

/// `n` random bytes from the OS's generator, as lowercase hex: Python's
/// `secrets.token_hex(n)`.
pub fn random_hex(n: usize) -> io::Result<String> {
    let mut bytes = vec![0u8; n];
    getrandom::fill(&mut bytes)
        .map_err(|e| io::Error::other(format!("the OS's random numbers: {e}")))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// A Round's private directory for its keys: removed explicitly with
/// [`SecretDir::remove`], and on `Drop` whatever happened.
#[derive(Debug)]
pub struct SecretDir {
    path: PathBuf,
    removed: bool,
}

impl SecretDir {
    /// A new directory `udpstp-bench-keys-<16 hex digits>` in the system's
    /// temporary directory.
    pub fn create() -> io::Result<SecretDir> {
        SecretDir::create_in(&std::env::temp_dir())
    }

    /// A new directory `udpstp-bench-keys-<16 hex digits>` in `parent`.
    pub fn create_in(parent: &Path) -> io::Result<SecretDir> {
        let mut tries = 0;
        loop {
            let path = parent.join(format!("udpstp-bench-keys-{}", random_hex(8)?));
            match private_dir(&path) {
                Ok(()) => {
                    return Ok(SecretDir {
                        path,
                        removed: false,
                    });
                }
                // Another's by chance: take another name.
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists && tries < 10 => tries += 1,
                Err(e) => return Err(e),
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Writes `contents` to the new file `name` in the directory, the
    /// owner's alone from its creation: its path.
    pub fn write(&self, name: &str, contents: &[u8]) -> io::Result<PathBuf> {
        let plain =
            !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', ':']);
        if !plain {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{name:?} isn't a file name"),
            ));
        }
        let path = self.path.join(name);
        let mut file = private_file(&path)?;
        file.write_all(contents)?;
        file.sync_all()?;
        Ok(path)
    }

    /// Removes the directory and everything in it. If that fails, `Drop`
    /// tries again.
    pub fn remove(&mut self) -> io::Result<()> {
        if !self.removed {
            fs::remove_dir_all(&self.path)?;
            self.removed = true;
        }
        Ok(())
    }
}

impl Drop for SecretDir {
    fn drop(&mut self) {
        let _ = self.remove();
    }
}

/// A rented host's SSH key: the private key's path, and the public half.
pub type SshKey = (PathBuf, String);

/// How long `ssh-keygen` may take.
pub const SSH_KEYGEN_LIMIT: Duration = Duration::from_secs(60);

/// A fresh SSH key for the Round's rented hosts, `id_ed25519` in `dir`,
/// made by `ssh-keygen` from PATH (`--ssh-dir` names only `ssh` and
/// `scp`), through `runner`: its path and its public half
/// (`make_ssh_key`, `round.py:224-229`).
pub fn ssh_key(dir: &SecretDir, runner: &Runner) -> Result<SshKey, Stopped> {
    let path = dir.path().join("id_ed25519");
    let mut keygen = Command::new("ssh-keygen");
    keygen
        .args(["-q", "-t", "ed25519", "-N", "", "-C", "udpstp-bench", "-f"])
        .arg(&path);
    let failed = |why: String| Stopped::Failed(format!("ssh-keygen: {why}"));
    let done = runner
        .run(&mut keygen, None, SSH_KEYGEN_LIMIT)
        .map_err(|e| match e {
            RunError::Interrupted => Stopped::Interrupted,
            RunError::TimedOut => {
                failed(format!("timed out after {} s", SSH_KEYGEN_LIMIT.as_secs()))
            }
            RunError::Spawn(e) => failed(format!("couldn't start it: {e}")),
            RunError::Wait(e) => failed(format!("waiting for it: {e}")),
            RunError::TooMuchOutput { cap } => failed(format!("more than {cap} bytes of output")),
        })?;
    if !done.success() {
        return Err(failed(format!(
            "exit {}: {}",
            done.returncode,
            lossy_text(&done.stderr).trim()
        )));
    }
    let mut public = path.clone().into_os_string();
    public.push(".pub");
    let public = fs::read_to_string(&public).map_err(|e| failed(e.to_string()))?;
    Ok((path, public))
}

#[cfg(unix)]
fn private_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().mode(0o700).create(path)
}

#[cfg(not(unix))]
fn private_dir(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new().create(path)
}

#[cfg(unix)]
fn private_file(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn private_file(path: &Path) -> io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parent(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "bench-rounds-secrets-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn random_hex_is_token_hex() {
        let two = random_hex(2).unwrap();
        assert_eq!(two.len(), 4);
        assert!(
            two.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')),
            "{two}"
        );
        let key = random_hex(16).unwrap();
        assert_eq!(key.len(), 32);
        assert_ne!(key, random_hex(16).unwrap());
        assert_eq!(random_hex(0).unwrap(), "");
    }

    #[test]
    fn the_directory_is_private_and_goes_on_drop() {
        let parent = parent("drop");
        let path = {
            let dir = SecretDir::create_in(&parent).unwrap();
            let name = dir.path().file_name().unwrap().to_str().unwrap().to_owned();
            let hex = name.strip_prefix("udpstp-bench-keys-").unwrap();
            assert_eq!(hex.len(), 16, "{name}");
            let key = dir.write("keys.toml", b"key_text = \"x\"\n").unwrap();
            assert_eq!(fs::read(&key).unwrap(), b"key_text = \"x\"\n");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode(dir.path()), 0o700);
                assert_eq!(mode(&key), 0o600);
            }
            dir.path().to_owned()
        };
        assert!(!path.exists(), "removed on Drop");
        fs::remove_dir_all(&parent).unwrap();
    }

    #[test]
    fn remove_takes_everything_once() {
        let parent = parent("remove");
        let mut dir = SecretDir::create_in(&parent).unwrap();
        dir.write("id_ed25519", b"secret").unwrap();
        fs::create_dir(dir.path().join("home")).unwrap();
        fs::write(dir.path().join("home").join("udpstp"), b"binary").unwrap();
        dir.remove().unwrap();
        assert!(!dir.path().exists());
        dir.remove().unwrap();
        drop(dir);
        fs::remove_dir_all(&parent).unwrap();
    }

    #[test]
    fn a_file_is_written_once_and_only_by_name() {
        let parent = parent("names");
        let dir = SecretDir::create_in(&parent).unwrap();
        dir.write("udpst.keys", b"7 x\n").unwrap();
        assert_eq!(
            dir.write("udpst.keys", b"again").unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        for bad in ["", ".", "..", "a/b", "a\\b", "../x", "c:x"] {
            assert_eq!(
                dir.write(bad, b"x").unwrap_err().kind(),
                io::ErrorKind::InvalidInput,
                "{bad:?}"
            );
        }
        drop(dir);
        fs::remove_dir_all(&parent).unwrap();
    }

    #[test]
    fn two_directories_never_share_a_name() {
        let parent = parent("two");
        let a = SecretDir::create_in(&parent).unwrap();
        let b = SecretDir::create_in(&parent).unwrap();
        assert_ne!(a.path(), b.path());
        drop((a, b));
        fs::remove_dir_all(&parent).unwrap();
    }

    #[test]
    fn a_rented_hosts_key_is_made_in_the_directory() {
        use crate::interrupt::Flag;
        let parent = parent("ssh-key");
        let dir = SecretDir::create_in(&parent).unwrap();
        let (path, public) = ssh_key(&dir, &Runner::new(Flag::new())).unwrap();
        assert_eq!(path, dir.path().join("id_ed25519"));
        assert!(path.is_file());
        assert!(public.starts_with("ssh-ed25519 "), "{public}");
        assert!(public.trim_end().ends_with(" udpstp-bench"), "{public}");
        // An interrupted Round makes none.
        let flag = Flag::new();
        flag.raise("a test");
        let other = SecretDir::create_in(&parent).unwrap();
        assert_eq!(
            ssh_key(&other, &Runner::new(flag)),
            Err(Stopped::Interrupted)
        );
        drop((dir, other));
        fs::remove_dir_all(&parent).unwrap();
    }
}
