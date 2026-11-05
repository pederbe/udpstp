//! A fetched directory's `.tar.gz`, unpacked in-process: regular files and
//! directories only, at relative paths that stay inside the destination
//!. The Python unpacks with
//! `tarfile`'s `data` filter (`hosts.py:136-137`); a host that went wrong
//! could otherwise write anywhere this machine's user can.
//!
//! The whole archive is read and checked before anything is written, so an
//! archive that is refused writes nothing. Refused, where the Python's
//! filter differs:
//!
//! - links, hard or symbolic, even inside the destination, which the
//!   Python takes;
//! - sparse files, which the Python takes: the tar crate fills their holes
//!   with zeros of its own, which the cap below never sees, so a header's
//!   word could ask for any size (review S1);
//! - an absolute path, whose leading `/` the Python strips;
//! - a name a Windows file system would take for another file or a device
//!   (a `\` or a `:` in it, a trailing dot or space, `CON`, `NUL`, `COM1`,
//!   ...), on every platform, so that results unpack the same everywhere;
//! - an archive that doesn't end as gzip says it should.
//!
//! As the Python's filter does, it refuses devices and FIFOs, and paths
//! that climb out with `..`; on Unix a file's mode loses the set-ID bits
//! and others' and the group's write, keeps execution only where its owner
//! has it, and gains the owner's read and write. Ownership and times are
//! the unpacking user's and now. A global PAX header, which describes the
//! archive rather than a file, is passed over, as `tarfile` does.
//!
//! The destination is the Round's own directory: a link already in it
//! would be followed, as the archive's own never are.
//!
//! Since the archive is held in memory while it is checked, what it
//! unpacks to is capped at [`MAX_UNPACKED`], so that a hostile host's
//! small archive can't take this machine's memory; the Python, which
//! unpacks as it reads, would fill the disk instead. And its members, their
//! depth, their names and their paths are bounded ([`MAX_MEMBERS`],
//! [`MAX_DEPTH`], [`MAX_NAME`], [`MAX_PATH`]), far beyond a run's, so that
//! checking it stays quick.

use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use flate2::read::MultiGzDecoder;

/// The most an archive may unpack to, its tar headers and padding
/// included: 256 MiB, some 150 times a role's directory of the largest
/// recorded run (1.6 MB, at 200 Test Connections).
pub const MAX_UNPACKED: u64 = 256 << 20;

/// The most members an archive may hold; a run's directory holds a few
/// hundred files.
pub const MAX_MEMBERS: usize = 100_000;

/// The most names a member's path may have below the destination; a run's
/// have three or four.
pub const MAX_DEPTH: usize = 64;

/// The longest name, in bytes, as file systems take them.
pub const MAX_NAME: usize = 255;

/// The longest path, in bytes, as Linux's `PATH_MAX`.
pub const MAX_PATH: usize = 4096;

/// What an archive may hold. Besides memory, the bounds keep checking it
/// quick: a path of many names made the layout check quadratic, 16,000 of
/// them in 170 bytes of gzip taking 5 s (review S1).
#[derive(Clone, Copy, Debug)]
struct Bounds {
    unpacked: u64,
    members: usize,
    depth: usize,
    name: usize,
    path: usize,
}

const BOUNDS: Bounds = Bounds {
    unpacked: MAX_UNPACKED,
    members: MAX_MEMBERS,
    depth: MAX_DEPTH,
    name: MAX_NAME,
    path: MAX_PATH,
};

/// A reader that fails once more than its cap has been read.
struct Capped<R> {
    inner: R,
    cap: u64,
    left: u64,
}

impl<R: Read> Read for Capped<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.left = self.left.checked_sub(n as u64).ok_or_else(|| {
            io::Error::other(format!(
                "it unpacks to more than {} KiB, far beyond what a run leaves",
                self.cap >> 10
            ))
        })?;
        Ok(n)
    }
}

/// Why an archive was refused, or couldn't be unpacked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnpackError(String);

impl fmt::Display for UnpackError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UnpackError {}

fn refused(message: impl Into<String>) -> UnpackError {
    UnpackError(message.into())
}

/// What an archive holds that is taken: its path's names, from the
/// destination down.
enum Member {
    Dir(Vec<String>),
    File {
        names: Vec<String>,
        mode: u32,
        data: Vec<u8>,
    },
}

impl Member {
    fn names(&self) -> &[String] {
        match self {
            Member::Dir(names) | Member::File { names, .. } => names,
        }
    }
}

/// Unpacks the `.tar.gz` in `gz` into the directory `into`, which it makes
/// if it isn't there.
pub fn unpack(gz: &[u8], into: &Path) -> Result<(), UnpackError> {
    unpack_within(gz, into, BOUNDS)
}

/// [`unpack`] within `bounds`.
fn unpack_within(gz: &[u8], into: &Path, bounds: Bounds) -> Result<(), UnpackError> {
    let members = read(gz, bounds)?;
    check_layout(&members)?;
    write(&members, into).map_err(|e| refused(format!("unpacking into {}: {e}", into.display())))
}

fn read(gz: &[u8], bounds: Bounds) -> Result<Vec<Member>, UnpackError> {
    let broken = |e: io::Error| refused(format!("not a readable .tar.gz: {e}"));
    if gz.is_empty() {
        return Err(refused("not a readable .tar.gz: it is empty"));
    }
    let mut archive = tar::Archive::new(Capped {
        inner: MultiGzDecoder::new(gz),
        cap: bounds.unpacked,
        left: bounds.unpacked,
    });
    let mut members = Vec::new();
    for (n, entry) in archive.entries().map_err(broken)?.enumerate() {
        if n == bounds.members {
            return Err(refused(format!(
                "more than {} members, far beyond what a run leaves",
                bounds.members
            )));
        }
        let mut entry = entry.map_err(broken)?;
        let kind = entry.header().entry_type();
        let path = entry.path_bytes().into_owned();
        let shown = shown(&path);
        if kind.is_pax_global_extensions() {
            continue;
        }
        let names = names(&path, bounds).map_err(|why| refused(format!("{shown}: {why}")))?;
        if kind.is_dir() {
            members.push(Member::Dir(names));
        } else if kind.is_file() || kind == tar::EntryType::Continuous {
            // tarfile's regular files but GNUTYPE_SPARSE: REGTYPE, AREGTYPE
            // and CONTTYPE.
            if names.is_empty() {
                return Err(refused(format!("{shown}: a file where the directory is")));
            }
            let mode = entry.header().mode().map_err(broken)?;
            let size = entry.size();
            let mut data = Vec::new();
            entry.read_to_end(&mut data).map_err(broken)?;
            if data.len() as u64 != size {
                return Err(refused(format!(
                    "{shown}: cut short, {} of its {size} bytes",
                    data.len()
                )));
            }
            members.push(Member::File { names, mode, data });
        } else {
            return Err(refused(format!(
                "{shown}: a {}, where only files and directories are taken",
                kind_name(kind)
            )));
        }
    }
    // To gzip's end: its length and checksum, and nothing after.
    io::copy(&mut archive.into_inner(), &mut io::sink()).map_err(broken)?;
    Ok(members)
}

fn kind_name(kind: tar::EntryType) -> &'static str {
    match kind {
        tar::EntryType::Symlink => "symbolic link",
        tar::EntryType::Link => "hard link",
        tar::EntryType::Char => "character device",
        tar::EntryType::Block => "block device",
        tar::EntryType::Fifo => "FIFO",
        tar::EntryType::GNUSparse => "sparse file",
        _ => "member of another kind",
    }
}

/// A member's path for a message: quoted, and cut after 100 characters.
fn shown(path: &[u8]) -> String {
    let text = String::from_utf8_lossy(path);
    let mut chars = text.chars();
    let start: String = chars.by_ref().take(100).collect();
    if chars.next().is_some() {
        format!("{start:?} …")
    } else {
        format!("{start:?}")
    }
}

/// The names of a member's path, from the destination down: `.` and empty
/// names passed over, as `tar -C DIR .` writes `./`, and everything that
/// could leave the destination, mean another file, or pass `bounds`
/// refused.
fn names(path: &[u8], bounds: Bounds) -> Result<Vec<String>, String> {
    if path.len() > bounds.path {
        return Err(format!("a path of more than {} bytes", bounds.path));
    }
    let path = std::str::from_utf8(path).map_err(|_| "a name that isn't UTF-8")?;
    if path.starts_with('/') {
        return Err("an absolute path".into());
    }
    let mut names = Vec::new();
    for name in path.split('/') {
        match name {
            "" | "." => {}
            ".." => return Err("a path out of the destination (..)".into()),
            _ if name.len() > bounds.name => {
                return Err(format!("a name of more than {} bytes", bounds.name));
            }
            _ if names.len() == bounds.depth => {
                return Err(format!("a path more than {} names deep", bounds.depth));
            }
            _ => {
                portable(name)?;
                names.push(name.to_owned());
            }
        }
    }
    Ok(names)
}

/// Refuses a name a Windows file system would take for another file, a
/// stream of one, or a device, and control characters.
fn portable(name: &str) -> Result<(), &'static str> {
    if name
        .chars()
        .any(|c| c.is_control() || c == '\\' || c == ':')
    {
        return Err("a name with a backslash, a colon or a control character");
    }
    if name.ends_with(['.', ' ']) {
        return Err("a name ending with a dot or a space");
    }
    let stem = name.split('.').next().unwrap_or(name).trim_end();
    let upper = stem.to_ascii_uppercase();
    let device = matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (upper.len() == 4
            && (upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.as_bytes()[3].is_ascii_digit());
    if device {
        return Err("a device's name on Windows");
    }
    Ok(())
}

/// Refuses a path that is both a file and a directory, or runs through a
/// file, which could only be written in part.
fn check_layout(members: &[Member]) -> Result<(), UnpackError> {
    let mut dirs: HashSet<&[String]> = HashSet::new();
    for member in members {
        let names = member.names();
        for end in 0..names.len() {
            dirs.insert(&names[..end]);
        }
        if let Member::Dir(names) = member {
            dirs.insert(names);
        }
    }
    for member in members {
        if let Member::File { names, .. } = member
            && dirs.contains(names.as_slice())
        {
            return Err(refused(format!(
                "{:?}: both a file and a directory",
                names.join("/")
            )));
        }
    }
    Ok(())
}

fn write(members: &[Member], into: &Path) -> io::Result<()> {
    fs::create_dir_all(into)?;
    for member in members {
        let path: PathBuf = member
            .names()
            .iter()
            .fold(into.to_path_buf(), |p, n| p.join(n));
        match member {
            Member::Dir(_) => fs::create_dir_all(&path)?,
            Member::File { mode, data, .. } => {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(&path, data)?;
                set_mode(&path, *mode)?;
            }
        }
    }
    Ok(())
}

/// tarfile's `data` filter's mode for a regular file.
#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(data_mode(mode)))
}

#[cfg(not(unix))]
fn set_mode(_: &Path, _: u32) -> io::Result<()> {
    Ok(())
}

/// `mode & 0o755`, execution only where the owner has it, and the owner's
/// read and write: `tarfile._get_filtered_attrs` for a regular file.
#[cfg_attr(not(unix), allow(dead_code))]
fn data_mode(mode: u32) -> u32 {
    let mut mode = mode & 0o755;
    if mode & 0o100 == 0 {
        mode &= !0o111;
    }
    mode | 0o600
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;
    use tar::{EntryType, Header};

    /// An archive of raw members, each (type, path, mode, data), written
    /// without the checks `tar::Builder` makes of a path, as a hostile host
    /// could write it.
    fn archive(members: &[(EntryType, &[u8], u32, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (kind, path, mode, data) in members {
            builder
                .append(&header(*kind, path, *mode, data.len()), *data)
                .unwrap();
        }
        gzip(&builder.into_inner().unwrap())
    }

    fn header(kind: EntryType, path: &[u8], mode: u32, size: usize) -> Header {
        let mut header = Header::new_gnu();
        header.as_gnu_mut().unwrap().name[..path.len()].copy_from_slice(path);
        header.set_entry_type(kind);
        header.set_mode(mode);
        header.set_size(size as u64);
        if kind == EntryType::Symlink || kind == EntryType::Link {
            header.as_gnu_mut().unwrap().linkname[..4].copy_from_slice(b"/etc");
        }
        header.set_cksum();
        header
    }

    fn gzip(tar: &[u8]) -> Vec<u8> {
        let mut gz = GzEncoder::new(Vec::new(), Compression::default());
        gz.write_all(tar).unwrap();
        gz.finish().unwrap()
    }

    /// A fresh directory under the system's temporary one.
    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("bench-rounds-unpack-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn file<'a>(path: &'a [u8], data: &'a [u8]) -> (EntryType, &'a [u8], u32, &'a [u8]) {
        (EntryType::Regular, path, 0o644, data)
    }

    #[test]
    fn a_directory_comes_back_whole() {
        let gz = archive(&[
            (EntryType::Directory, b"./", 0o755, b""),
            (EntryType::Directory, b"./clients/", 0o755, b""),
            file(b"./clients/1.json", b"[1]"),
            file(b"./server.log", b"{}\n"),
            (EntryType::Directory, b"./empty/", 0o700, b""),
        ]);
        let into = scratch("whole");
        unpack(&gz, &into).unwrap();
        assert_eq!(fs::read(into.join("clients/1.json")).unwrap(), b"[1]");
        assert_eq!(fs::read(into.join("server.log")).unwrap(), b"{}\n");
        assert!(into.join("empty").is_dir());
        fs::remove_dir_all(&into).unwrap();
    }

    #[test]
    fn what_tar_writes_is_read() {
        // Long names, which GNU tar and the tar crate write as extra
        // members, and PAX headers.
        let long = format!("{}/samples.jsonl", "d".repeat(120));
        let mut builder = tar::Builder::new(Vec::new());
        let mut h = Header::new_gnu();
        h.set_mode(0o644);
        h.set_size(3);
        builder.append_data(&mut h, &long, &b"abc"[..]).unwrap();
        builder
            .append_pax_extensions([("comment", &b"a Round's results"[..])])
            .unwrap();
        let mut h = Header::new_ustar();
        h.set_mode(0o600);
        h.set_size(1);
        builder.append_data(&mut h, "x/p.json", &b"1"[..]).unwrap();
        let gz = gzip(&builder.into_inner().unwrap());
        let into = scratch("long");
        unpack(&gz, &into).unwrap();
        assert_eq!(fs::read(into.join(&long)).unwrap(), b"abc");
        assert_eq!(fs::read(into.join("x/p.json")).unwrap(), b"1");
        fs::remove_dir_all(&into).unwrap();
    }

    /// Each hostile archive is refused, and writes nothing: not even the
    /// destination.
    fn refuses(name: &str, gz: &[u8], why: &str) {
        let into = scratch(name);
        let error = unpack(gz, &into).expect_err(name).to_string();
        assert!(error.contains(why), "{name}: {error}");
        assert!(!into.exists(), "{name}: wrote something");
    }

    #[test]
    fn paths_out_of_the_destination_are_refused() {
        let good = file(b"./ok", b"ok");
        for (name, path) in [
            ("parent", &b"../evil"[..]),
            ("deeper", b"./a/../../evil"),
            ("inner", b"a/../b"),
            ("absolute", b"/etc/evil"),
        ] {
            let why = if name == "absolute" { "absolute" } else { ".." };
            refuses(name, &archive(&[good, file(path, b"x")]), why);
        }
    }

    #[test]
    fn links_devices_and_fifos_are_refused() {
        for (name, kind, why) in [
            ("symlink", EntryType::Symlink, "symbolic link"),
            ("hardlink", EntryType::Link, "hard link"),
            ("char", EntryType::Char, "character device"),
            ("block", EntryType::Block, "block device"),
            ("fifo", EntryType::Fifo, "FIFO"),
        ] {
            let gz = archive(&[file(b"./ok", b"ok"), (kind, b"./x", 0o644, b"")]);
            refuses(name, &gz, why);
        }
    }

    #[test]
    fn a_hostile_name_in_an_extension_header_is_refused() {
        // A PAX header's path, and a GNU long name, replace the member's.
        let mut builder = tar::Builder::new(Vec::new());
        builder
            .append_pax_extensions([("path", &b"../evil"[..])])
            .unwrap();
        builder
            .append(
                &header(EntryType::Regular, b"innocent", 0o644, 1),
                &b"x"[..],
            )
            .unwrap();
        refuses("pax", &gzip(&builder.into_inner().unwrap()), "..");

        let long = b"./a/../../evil\0";
        let mut builder = tar::Builder::new(Vec::new());
        builder
            .append(
                &header(EntryType::GNULongName, b"././@LongLink", 0o644, long.len()),
                &long[..],
            )
            .unwrap();
        builder
            .append(
                &header(EntryType::Regular, b"innocent", 0o644, 1),
                &b"x"[..],
            )
            .unwrap();
        refuses("longname", &gzip(&builder.into_inner().unwrap()), "..");
    }

    #[test]
    fn names_another_file_system_would_misread_are_refused() {
        for (name, path) in [
            ("backslash", &b"a\\..\\..\\evil"[..]),
            ("colon", b"file:stream"),
            ("drive", b"C:evil"),
            ("dot", b"evil."),
            ("space", b"evil "),
            ("con", b"con"),
            ("nul", b"NUL.txt"),
            ("com", b"dir/COM1"),
            ("control", b"a\x01b"),
            ("utf8", b"\xff"),
        ] {
            refuses(name, &archive(&[file(path, b"x")]), "");
        }
        // Names that only look alike are taken.
        let gz = archive(&[
            file(b"console", b"1"),
            file(b"COM10", b"2"),
            file(b"a.b", b"3"),
        ]);
        let into = scratch("alike");
        unpack(&gz, &into).unwrap();
        fs::remove_dir_all(&into).unwrap();
    }

    #[test]
    fn a_path_that_is_a_file_and_a_directory_is_refused() {
        refuses(
            "file-then-dir",
            &archive(&[file(b"a", b"x"), file(b"a/b", b"y")]),
            "both a file and a directory",
        );
        refuses(
            "dir-then-file",
            &archive(&[(EntryType::Directory, b"a/", 0o755, b""), file(b"a", b"x")]),
            "both a file and a directory",
        );
        refuses(
            "root-file",
            &archive(&[file(b"./", b"x")]),
            "a file where the directory is",
        );
    }

    #[test]
    fn a_broken_archive_is_refused() {
        let good = archive(&[file(b"./a", b"some data")]);
        refuses("empty", b"", "empty");
        refuses("not-gzip", b"not a gzip stream at all", "not a readable");
        refuses("cut", &good[..good.len() - 6], "not a readable");
        let mut trailing = good.clone();
        trailing.extend_from_slice(b"garbage");
        refuses("trailing", &trailing, "not a readable");
        // A member cut short inside a whole gzip stream.
        let mut builder = tar::Builder::new(Vec::new());
        builder
            .append(
                &header(EntryType::Regular, b"a", 0o644, 4096),
                &[7u8; 4096][..],
            )
            .unwrap();
        let tar = builder.into_inner().unwrap();
        refuses("short", &gzip(&tar[..512 + 100]), "");
    }

    #[test]
    fn an_archive_that_unpacks_to_too_much_is_refused() {
        const CAP: u64 = 64 << 10;
        let refused_within = |name: &str, gz: &[u8]| {
            let into = scratch(name);
            let error = unpack_within(gz, &into, capped(CAP))
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("more than 64 KiB, far beyond what a run leaves"),
                "{error}"
            );
            assert!(!into.exists());
        };
        // A member of more than the cap, in a few hundred bytes of gzip.
        let mut builder = tar::Builder::new(Vec::new());
        let size = CAP + 1;
        builder
            .append(
                &header(EntryType::Regular, b"bomb", 0o644, size as usize),
                io::repeat(0).take(size),
            )
            .unwrap();
        let gz = gzip(&builder.into_inner().unwrap());
        assert!(gz.len() < 1024, "{} bytes", gz.len());
        refused_within("bomb", &gz);
        // Zeros after the archive's end count too.
        let mut builder = tar::Builder::new(Vec::new());
        builder
            .append(&header(EntryType::Regular, b"a", 0o644, 1), &b"x"[..])
            .unwrap();
        let mut tar = builder.into_inner().unwrap();
        tar.resize(tar.len() + CAP as usize, 0);
        refused_within("padding", &gzip(&tar));
        // Just under the cap is taken.
        let mut builder = tar::Builder::new(Vec::new());
        builder
            .append(
                &header(EntryType::Regular, b"a", 0o644, 30 << 10),
                io::repeat(1).take(30 << 10),
            )
            .unwrap();
        let into = scratch("under");
        unpack_within(&gzip(&builder.into_inner().unwrap()), &into, capped(CAP)).unwrap();
        assert_eq!(fs::metadata(into.join("a")).unwrap().len(), 30 << 10);
        fs::remove_dir_all(&into).unwrap();
        assert_eq!(MAX_UNPACKED, 256 << 20);
    }

    fn capped(unpacked: u64) -> Bounds {
        Bounds { unpacked, ..BOUNDS }
    }

    /// A one-byte file at each of `paths`, in a GNU archive, long names
    /// and all, as `tar` writes them.
    fn archive_of_paths(paths: &[String]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for path in paths {
            let mut header = Header::new_gnu();
            header.set_mode(0o644);
            header.set_size(1);
            builder.append_data(&mut header, path, &b"x"[..]).unwrap();
        }
        gzip(&builder.into_inner().unwrap())
    }

    /// Review S1: bounds on names, depth, path length and members, far
    /// beyond a run's, keep checking an archive quick; 16,000 names deep
    /// took the layout check 5 s.
    #[test]
    fn deep_long_or_many_paths_are_refused() {
        let deep = |n: usize| vec!["d"; n].join("/");
        let into = scratch("deep64");
        unpack(&archive_of_paths(&[deep(64)]), &into).unwrap();
        fs::remove_dir_all(&into).unwrap();
        refuses(
            "deep65",
            &archive_of_paths(&[deep(65)]),
            "more than 64 names deep",
        );

        let into = scratch("name255");
        unpack(&archive_of_paths(&["n".repeat(255)]), &into).unwrap();
        fs::remove_dir_all(&into).unwrap();
        refuses(
            "name256",
            &archive_of_paths(&["n".repeat(256)]),
            "a name of more than 255 bytes",
        );

        // Shallow, each name short enough, and too long a path.
        let long = vec!["p".repeat(250); 17].join("/");
        refuses(
            "path",
            &archive_of_paths(&[long]),
            "a path of more than 4096 bytes",
        );

        let start = std::time::Instant::now();
        refuses("deep16000", &archive_of_paths(&[deep(16_000)]), "");
        let took = start.elapsed();
        assert!(took < std::time::Duration::from_secs(2), "took {took:?}");

        let files = |n: usize| (0..n).map(|i| format!("f{i}")).collect::<Vec<_>>();
        let three = Bounds {
            members: 3,
            ..BOUNDS
        };
        let into = scratch("members3");
        unpack_within(&archive_of_paths(&files(3)), &into, three).unwrap();
        fs::remove_dir_all(&into).unwrap();
        let into = scratch("members4");
        let error = unpack_within(&archive_of_paths(&files(4)), &into, three)
            .unwrap_err()
            .to_string();
        assert!(error.contains("more than 3 members"), "{error}");
        assert!(!into.exists());

        assert_eq!(
            (MAX_MEMBERS, MAX_DEPTH, MAX_NAME, MAX_PATH),
            (100_000, 64, 255, 4096)
        );
    }

    /// Review S1: a sparse member's holes are zeros the tar crate makes
    /// itself, past the cap, so a hundred bytes of archive could ask for any
    /// size.
    #[test]
    fn a_sparse_file_is_refused() {
        let mut header = Header::new_gnu();
        header.as_gnu_mut().unwrap().name[..6].copy_from_slice(b"sparse");
        header.set_entry_type(EntryType::GNUSparse);
        header.set_mode(0o644);
        header.set_size(1);
        {
            let gnu = header.as_gnu_mut().unwrap();
            // A megabyte of hole, then one byte of data.
            gnu.set_real_size((1 << 20) + 1);
            gnu.sparse[0].set_offset(1 << 20);
            gnu.sparse[0].set_length(1);
        }
        header.set_cksum();
        let mut builder = tar::Builder::new(Vec::new());
        builder.append(&header, &b"x"[..]).unwrap();
        let gz = gzip(&builder.into_inner().unwrap());
        assert!(gz.len() < 200, "{} bytes", gz.len());
        // Within a cap its hole passes 16 times over.
        let into = scratch("sparse");
        let error = unpack_within(&gz, &into, capped(64 << 10))
            .unwrap_err()
            .to_string();
        assert!(error.contains("a sparse file"), "{error}");
        assert!(!into.exists());
    }

    #[test]
    fn a_global_pax_header_is_passed_over() {
        let gz = archive(&[
            (
                EntryType::XGlobalHeader,
                b"pax_global_header",
                0o644,
                b"20 comment=a Round\n",
            ),
            file(b"./a", b"1"),
        ]);
        let into = scratch("global");
        unpack(&gz, &into).unwrap();
        assert_eq!(fs::read(into.join("a")).unwrap(), b"1");
        fs::remove_dir_all(&into).unwrap();
    }

    #[test]
    fn modes_are_the_python_data_filters() {
        for (given, kept) in [
            (0o644, 0o644),
            (0o755, 0o755),
            (0o4755, 0o755),
            (0o777, 0o755),
            (0o000, 0o600),
            (0o444, 0o644),
            (0o070, 0o640),
            (0o711, 0o711),
            (0o611, 0o600),
        ] {
            assert_eq!(data_mode(given), kept, "{given:o}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn files_get_the_python_data_filters_modes() {
        use std::os::unix::fs::PermissionsExt;
        let gz = archive(&[
            (EntryType::Regular, b"run", 0o4755, b"#!/bin/sh\n"),
            (EntryType::Regular, b"data", 0o666, b"1"),
        ]);
        let into = scratch("modes");
        unpack(&gz, &into).unwrap();
        let mode =
            |name: &str| fs::metadata(into.join(name)).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode("run"), 0o755);
        assert_eq!(mode("data"), 0o644);
        fs::remove_dir_all(&into).unwrap();
    }
}
