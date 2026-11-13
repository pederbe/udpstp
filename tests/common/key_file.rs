//! A key file for the `udpstp` process tests.

use std::path::PathBuf;

/// A key file in a fresh temporary directory, readable by its owner only.
pub struct KeyFile {
    dir: PathBuf,
    path: PathBuf,
}

impl KeyFile {
    pub fn new(test: &str, name: &str, contents: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("udpstp-keys-{test}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join(name);
        std::fs::write(&path, contents).expect("key file written");
        let file = Self { dir, path };
        file.set_mode(0o600);
        file
    }

    #[cfg(unix)]
    pub fn set_mode(&self, mode: u32) {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(mode))
            .expect("permissions set");
    }

    #[cfg(not(unix))]
    pub fn set_mode(&self, _mode: u32) {}

    pub fn path(&self) -> &str {
        self.path.to_str().expect("UTF-8 temp path")
    }
}

impl Drop for KeyFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
