//! Platform differences, in one place.
//!
//! §5.7: "Windows is tier-1 from month one (case-insensitivity, path length,
//! symlinks, locking, watcher differences)". G1.12 makes that a HARD gate.
//!
//! The temptation is `#[cfg(unix)]` scattered through the engine, which works
//! and quietly makes Windows a second-class port. Everything platform-specific
//! lives here instead, so the differences are enumerable rather than discovered,
//! and each one that LOSES information is named — G3.4 requires every lossy
//! edge to be documented with a demonstrating test rather than found later by a
//! user.
//!
//! ## What differs, honestly
//!
//! | Property | Unix | Windows | Lossy? |
//! |---|---|---|---|
//! | permission bits | real `st_mode & 0o777` | not represented | **yes** — the executable bit does not survive a Windows checkout |
//! | symlinks | always available | needs privilege or Developer Mode | **yes, conditionally** — reported, never silent |
//! | filename bytes | arbitrary bytes | UTF-16, so not all byte sequences are nameable | **yes** — reported per name |
//!
//! None of these is a Lattice defect; all three are the platform. What would be
//! a defect is losing data without saying so, which is what this module exists
//! to prevent.

use std::ffi::OsString;
use std::fs;
use std::path::Path;

use crate::error::Result;

/// The permission bits to record for a file.
///
/// Windows has no Unix mode. Recording a plausible default keeps the tree format
/// identical across platforms — a repository saved on Windows and checked out on
/// Linux produces readable files rather than mode 0 — at the cost of the
/// executable bit, which is named in the table above as a lossy edge.
#[cfg(unix)]
pub fn file_mode(meta: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o777
}

#[cfg(not(unix))]
pub fn file_mode(meta: &fs::Metadata) -> u32 {
    // Read-only is the one bit Windows does expose.
    if meta.permissions().readonly() {
        0o444
    } else {
        0o644
    }
}

/// Apply recorded permission bits, where the platform has them.
#[cfg(unix)]
pub fn set_file_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
pub fn set_file_mode(path: &Path, mode: u32) -> Result<()> {
    // Only the read-only bit is representable. The executable bit is dropped,
    // which `mode_is_lossy_here` reports so a caller can surface it.
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_readonly(mode & 0o200 == 0);
    fs::set_permissions(path, perms)?;
    Ok(())
}

/// Does this platform lose information when applying `mode`?
///
/// Used to populate the checkout report, so a Windows user is told which
/// permission bits did not survive rather than discovering it when a script
/// fails to run or a private file is group-readable. On Windows only the
/// read-only bit is representable, so any mode that is not exactly the value
/// `set_file_mode` would produce there — 0o644 for writable, 0o444 for
/// read-only — loses information (the executable bit, or group/other
/// distinctions like 0o640).
pub fn mode_is_lossy_here(mode: u32) -> bool {
    if cfg!(unix) {
        return false;
    }
    let perms = mode & 0o777;
    perms != 0o644 && perms != 0o444
}

/// Create a symbolic link.
///
/// Windows distinguishes file and directory links and requires either
/// administrator rights or Developer Mode. The error propagates rather than
/// being swallowed: a checkout that silently skipped links would be losing
/// data, which is the one thing this project may not do.
#[cfg(unix)]
pub fn symlink(target: &Path, link: &Path) -> Result<()> {
    std::os::unix::fs::symlink(target, link)?;
    Ok(())
}

#[cfg(windows)]
pub fn symlink(target: &Path, link: &Path) -> Result<()> {
    // A dangling target has no type to inspect, so a file link is the default;
    // it is the correct choice for the overwhelming majority of cases and the
    // only one that works for a target that does not exist yet.
    let result = if target.is_dir() {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    };
    result?;
    Ok(())
}

#[cfg(not(any(unix, windows)))]
pub fn symlink(_target: &Path, _link: &Path) -> Result<()> {
    Err(crate::error::Error::Invalid(
        "this platform cannot create symbolic links".into(),
    ))
}

/// Turn stored name bytes back into a filesystem name.
///
/// On Unix this is exact: names are bytes and always round-trip. On Windows
/// names are UTF-16, so a byte sequence that is not valid UTF-8 has no faithful
/// representation. `None` means exactly that, and the caller reports it rather
/// than writing a mangled name — a silently renamed file is a lost file.
#[cfg(unix)]
pub fn os_string_from_bytes(bytes: &[u8]) -> Option<OsString> {
    use std::os::unix::ffi::OsStringExt;
    Some(OsString::from_vec(bytes.to_vec()))
}

#[cfg(not(unix))]
pub fn os_string_from_bytes(bytes: &[u8]) -> Option<OsString> {
    match std::str::from_utf8(bytes) {
        Ok(s) => Some(OsString::from(s)),
        Err(_) => None,
    }
}

/// Capture a filesystem name as bytes.
///
/// `as_encoded_bytes` is stable and lossless on both platforms; the encoding is
/// unspecified but self-consistent, which is all a content address needs.
pub fn bytes_from_os_str(name: &std::ffi::OsStr) -> Vec<u8> {
    name.as_encoded_bytes().to_vec()
}

/// Make a directory's own entries durable.
///
/// On Unix, creating or renaming a file is only committed once the containing
/// DIRECTORY is fsynced — the barrier G1.1's replayer models as OP_DIRSYNC.
/// Windows cannot open a directory with `File::open` at all, and NTFS journals
/// metadata itself, so there is nothing to call and nothing to lose.
///
/// This would have compiled cleanly and failed at RUNTIME on Windows, which is
/// exactly the class of defect a platform module exists to make impossible to
/// write by accident.
#[cfg(unix)]
pub fn sync_dir(dir: &Path) -> Result<()> {
    fs::File::open(dir)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
pub fn sync_dir(_dir: &Path) -> Result<()> {
    // NTFS journals directory metadata; there is no directory handle to sync.
    Ok(())
}

/// A stable identity for a filesystem object, WITHOUT following a final
/// symlink, for detecting when two distinct names the filesystem folded
/// together landed on the same object during checkout.
///
/// On Unix this is `(dev, ino)` from `symlink_metadata`: two names the
/// filesystem folds into one share an inode, while two distinct symlinks to the
/// same target do not (each link is its own inode). `None` means the platform
/// cannot supply a cheap identity here — the caller then cannot distinguish a
/// folded sibling from an unrelated pre-existing file, and overwrites, which is
/// a documented Windows limitation rather than silent loss on Unix.
#[cfg(unix)]
pub fn file_identity(meta: &fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
pub fn file_identity(_meta: &fs::Metadata) -> Option<(u64, u64)> {
    // std's symlink_metadata does not expose a stable file index on Windows
    // without opening the file, and opening would follow the link. Fold
    // detection on a case-insensitive Windows volume is a documented gap.
    None
}

/// Remove a file or a symlink, whatever kind of link it is.
///
/// Windows distinguishes file and directory symlinks: a directory link must be
/// removed with `remove_dir`, and `remove_file` fails on it. Unix has one
/// unlink for both, so the fallback is simply never taken there.
pub fn remove_file_or_symlink(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if cfg!(windows) => {
            fs::remove_dir(path).map_err(|_| e)?;
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

/// Take an exclusive lock on `path`, waiting up to `wait` for it.
///
/// The returned file owns the lock: dropping it releases it, and so does the
/// process exiting for any reason, including a kill. That is the property that
/// matters — a lock a crashed process could strand is a repository nobody can
/// open again, which is worse than the contention it was guarding.
///
/// `std::fs::File::lock` blocks with no deadline, so this polls `try_lock`
/// instead. A caller that waits forever cannot tell a busy repository from a
/// deadlocked one, and "every retry has a cap and backoff" is the house rule.
///
/// Portable by construction: `flock(LOCK_EX)` on Unix, `LockFileEx` on
/// Windows, both through std, so no dependency is added for it and G1.12 gets
/// the same behaviour on all three platforms.
pub fn lock_exclusive(path: &Path, wait: std::time::Duration) -> Result<fs::File> {
    use std::time::{Duration, Instant};

    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    let deadline = Instant::now() + wait;
    // Backoff doubles to a ceiling rather than spinning: eight contenders
    // polling a held lock is eight cores doing nothing.
    let mut backoff = Duration::from_millis(1);
    const MAX_BACKOFF: Duration = Duration::from_millis(50);
    loop {
        if file.try_lock().is_ok() {
            return Ok(file);
        }
        if Instant::now() >= deadline {
            return Err(crate::error::Error::Busy(format!(
                "another command has held this repository for longer than {} \
                 seconds",
                wait.as_secs()
            )));
        }
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

/// Human-readable name for the current platform, for reports.
pub fn platform_name() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(unix) {
        "unix"
    } else {
        "unknown"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_exclusive_lock_waits_and_then_reports_the_repository_busy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lock");
        let held = lock_exclusive(&path, std::time::Duration::from_secs(5)).unwrap();

        let started = std::time::Instant::now();
        let second = lock_exclusive(&path, std::time::Duration::from_millis(120));

        let err = second.expect_err("the lock is held, so this cannot succeed");
        assert_eq!(err.category(), crate::error::Category::Busy);
        assert!(
            started.elapsed() >= std::time::Duration::from_millis(100),
            "it must WAIT for the lock rather than fail on first sight — that \
             is the whole difference from redb's non-blocking one"
        );

        drop(held);
        lock_exclusive(&path, std::time::Duration::from_millis(500))
            .expect("dropping the holder releases it");
    }

    #[test]
    fn names_round_trip_through_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ordinary.txt");
        fs::write(&path, b"x").unwrap();
        let name = path.file_name().unwrap();
        let bytes = bytes_from_os_str(name);
        assert_eq!(os_string_from_bytes(&bytes).as_deref(), Some(name));
    }

    #[test]
    fn a_mode_with_the_executable_bit_is_lossy_only_off_unix() {
        assert_eq!(mode_is_lossy_here(0o755), cfg!(not(unix)));
        assert!(
            !mode_is_lossy_here(0o644),
            "a non-executable mode loses nothing anywhere"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_reports_real_permission_bits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("script");
        fs::write(&path, b"#!/bin/sh\n").unwrap();
        set_file_mode(&path, 0o755).unwrap();
        assert_eq!(file_mode(&fs::metadata(&path).unwrap()), 0o755);
    }

    #[cfg(not(unix))]
    #[test]
    fn non_utf8_names_are_refused_rather_than_mangled() {
        // Windows cannot name this. Returning None lets the caller report it;
        // writing a lossy approximation would silently rename the file.
        assert_eq!(os_string_from_bytes(&[0x66, 0xff, 0xfe]), None);
    }
}
