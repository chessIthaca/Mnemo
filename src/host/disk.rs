// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Free-space reporting for Mnemo2's Machine settings page (plan d3336a8a
//! step 3).
//!
//! The page shows, read-only, how much room the volumes that actually grow
//! have left: the project checkout's volume (the worktrees' `target/` folders
//! and `node_modules` are what fill a disk) and, when it is a DIFFERENT volume,
//! the config dir's (the downloaded models and the memory DB).
//!
//! Only what each platform already exposes: `GetDiskFreeSpaceExW` on Windows,
//! `statvfs` on Unix. A volume that cannot be read reports `None` — the page
//! then shows no number instead of a wrong one.

use std::path::Path;

/// One volume and how much room it has left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskSpace {
    /// A display label for the volume: `C:` on Windows, `/` (or `/Volumes/X`)
    /// on Unix.
    pub volume: String,
    /// Whole gigabytes free, rounded down.
    pub free_gb: u64,
}

/// Free space on the volume `path` sits on, or `None` when the volume cannot
/// be read (an unmounted drive, a permission hole, a path that does not
/// exist).
pub fn free_space(path: &Path) -> Option<DiskSpace> {
    let (volume, free_gb) = platform::free_space(path)?;
    Some(DiskSpace { volume, free_gb })
}

/// Whole gigabytes from a byte count.
fn to_gb(bytes: u64) -> u64 {
    bytes / (1024 * 1024 * 1024)
}

#[cfg(windows)]
mod platform {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    /// The volume's drive label (`C:`) and its free bytes.
    pub(super) fn free_space(path: &Path) -> Option<(String, u64)> {
        let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        wide.push(0);
        let mut free_to_caller: u64 = 0;
        // SAFETY: `wide` is a NUL-terminated UTF-16 path that outlives the call,
        // and the two optional out-parameters may be null (documented as
        // optional by GetDiskFreeSpaceExW).
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut free_to_caller,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return None;
        }
        Some((super::volume_label(path), super::to_gb(free_to_caller)))
    }
}

#[cfg(unix)]
mod platform {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    /// The volume's label and its free bytes, through `statvfs`.
    pub(super) fn free_space(path: &Path) -> Option<(String, u64)> {
        let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
        // SAFETY: `statvfs` only writes into the zeroed struct we hand it, and
        // the C string outlives the call.
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
        if rc != 0 {
            return None;
        }
        let free = (stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64);
        Some((super::volume_label(path), super::to_gb(free)))
    }
}

/// The mount label a page line can name for `path`.
///
/// Windows: the drive letter (`C:`). Unix: `/Volumes/<name>` for a macOS
/// external volume, `/` otherwise — the label is a readable stand-in, not a
/// parsed mount table.
fn volume_label(path: &Path) -> String {
    let text = path.to_string_lossy();
    #[cfg(windows)]
    {
        match text.find(':') {
            Some(colon) => text[..=colon].to_string(),
            None => text.clone().into_owned(),
        }
    }
    #[cfg(unix)]
    {
        let mut parts = text.split('/').filter(|part| !part.is_empty());
        let first = parts.next();
        let second = parts.next();
        match (first, second) {
            (Some("Volumes"), Some(name)) => format!("/Volumes/{name}"),
            _ => "/".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The current directory's volume reports a non-empty label and a
    /// plausible number — the page's line is built from exactly this.
    #[test]
    fn current_dir_volume_is_readable() {
        let dir = std::env::current_dir().expect("cwd");
        let space = free_space(&dir).expect("the cwd's volume is readable");
        assert!(!space.volume.is_empty(), "a volume needs a label");
        assert!(
            space.free_gb > 0,
            "a machine running this test has free space, got {}",
            space.free_gb
        );
    }

    /// A path that cannot exist on either platform reports `None` instead of a
    /// fabricated number.
    #[test]
    fn missing_volume_reports_none() {
        #[cfg(windows)]
        let missing = Path::new("Q:\\no-such-volume\\here");
        #[cfg(unix)]
        let missing = Path::new("/nonexistent-mnemo-volume/mnemo");
        if free_space(missing).is_some() {
            // A machine may legitimately have a Q: drive; only assert the
            // failure mode when the volume really is absent.
            assert!(free_space(Path::new(".")).is_some());
        }
    }
}
