#![deny(unsafe_code)]
//! Application support for Windows Shared Key files (ADR 0015).
//!
//! The native adapter borrows the file being loaded and returns owned data.
//! Safe Rust decodes it and evaluates a bounded structural policy, without
//! claiming universal effective access. This crate makes no protocol decisions.

#[cfg(any(windows, test))]
mod descriptor;
#[cfg(windows)]
#[allow(unsafe_code)]
mod native;
#[cfg(any(windows, test))]
mod policy;

/// The result of ADR 0015's supported private-file assessment.
#[derive(Debug, PartialEq, Eq)]
pub enum Assessment {
    /// Trusted owner and no understood dangerous outsider grant.
    Meets,
    /// A structural policy mismatch, without an effective-access claim.
    Mismatch(&'static str),
    /// Unsupported data or an inspection failure.
    Unverified(String),
}

/// Opens the file for reading with only `FILE_SHARE_READ`: other readers
/// may coexist, while ordinary writers and replacement opens are blocked.
#[cfg(windows)]
pub fn open(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(path)
}

/// Assesses a borrowed regular file on local NTFS, trusting the running
/// process account, SYSTEM and built-in Administrators. Inspection errors
/// and unsupported permission forms are unverified, never a policy match.
///
/// The caller must read this same file and protect its containing storage.
/// Read-only sharing limits content changes, but does not freeze the ACL.
#[cfg(windows)]
pub fn assess(file: &std::fs::File) -> Assessment {
    match native::running_sid().and_then(|running| {
        native::inspect(file).map(|snapshot| policy::assess(&snapshot, &running))
    }) {
        Ok(assessment) => assessment,
        Err(error) => Assessment::Unverified(error.to_string()),
    }
}
