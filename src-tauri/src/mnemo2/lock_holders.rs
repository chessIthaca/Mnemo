// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Best-effort naming of the processes holding files in a folder.
//!
//! A Windows worker checkout can refuse to delete with `os error 32` (the file
//! is in use), and the OS error never says which process holds it. On Windows
//! this module asks the Restart Manager (`rstrtmgr.dll`) which applications
//! have files under the folder open, so a failed removal can name what the
//! user has to close. It is diagnosis only: nothing is ever closed or killed,
//! and every API failure degrades to an empty list instead of a new error path.

#[cfg(windows)]
use std::path::Path;
use std::path::PathBuf;

/// One process the Restart Manager reports as using a probed file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LockHolder {
    /// The application name the Restart Manager reports (for example
    /// `Code.exe`); empty when the API returns none.
    pub name: String,
    /// The process id.
    pub pid: u32,
}

/// The most files handed to the Restart Manager in one session. A checkout can
/// hold a `node_modules` tree with tens of thousands of files, and the Restart
/// Manager resolves every registered file before it answers, so probe a
/// bounded, newest-first sample — an editor, shell, or dev server holds the
/// files it touched last — rather than letting one failed removal stall.
#[cfg(windows)]
const MAX_PROBED_FILES: usize = 256;

/// The processes holding any file under `paths`, best effort.
///
/// A path may be a file (probed directly) or a directory (whose files are
/// probed, newest first, up to an internal cap). The list is empty when no
/// holder is found, on every non-Windows platform, and after any Restart
/// Manager failure. Nothing is ever closed or killed.
pub fn holders(paths: &[PathBuf]) -> Vec<LockHolder> {
    #[cfg(windows)]
    {
        restart_manager_holders(&probe_files(paths))
    }
    #[cfg(not(windows))]
    {
        let _ = paths;
        Vec::new()
    }
}

/// The note a failed checkout removal appends to its error: the holders, or —
/// when the Restart Manager reports none — a hint that a terminal or program
/// whose working folder is inside the checkout also blocks removal, because
/// the manager only sees open files, never directory handles.
pub fn held_by_suffix(holders: &[LockHolder]) -> String {
    if holders.is_empty() {
        return " (no process found holding a file in it; a terminal or program whose working folder is inside it can also block removal)".to_string();
    }
    let named: Vec<String> = holders
        .iter()
        .map(|holder| {
            let name = if holder.name.is_empty() {
                "an unnamed process"
            } else {
                holder.name.as_str()
            };
            format!("{name} (pid {})", holder.pid)
        })
        .collect();
    format!("; held by {}", named.join(", "))
}

/// The files to probe for `paths`: files are used as given and directories are
/// walked newest-first until the cap is reached.
#[cfg(windows)]
fn probe_files(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for path in paths {
        if files.len() >= MAX_PROBED_FILES {
            break;
        }
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_file() => files.push(path.clone()),
            Ok(metadata) if metadata.is_dir() => collect_files(path, &mut files),
            _ => {}
        }
    }
    files
}

/// Collect at most [`MAX_PROBED_FILES`] files under `directory`. Links are
/// skipped so a junction cannot loop the walk, and every directory is visited
/// newest-first so the most recently touched files are the ones probed.
#[cfg(windows)]
fn collect_files(directory: &Path, files: &mut Vec<PathBuf>) {
    let mut pending = vec![directory.to_path_buf()];
    while let Some(directory) = pending.pop() {
        if files.len() >= MAX_PROBED_FILES {
            return;
        }
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        let mut children: Vec<std::fs::DirEntry> = entries.filter_map(Result::ok).collect();
        children.sort_by_key(|entry| {
            std::cmp::Reverse(
                std::fs::symlink_metadata(entry.path())
                    .and_then(|metadata| metadata.modified())
                    .ok(),
            )
        });
        for entry in children {
            if files.len() >= MAX_PROBED_FILES {
                return;
            }
            let metadata = match std::fs::symlink_metadata(entry.path()) {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if metadata.is_file() {
                files.push(entry.path());
            }
        }
    }
}

/// Ask the Restart Manager which applications hold files in `files`.
///
/// Returns an empty list on every failure: a session that cannot start, a
/// registration the manager rejects, or a process list that changes under us.
#[cfg(windows)]
fn restart_manager_holders(files: &[PathBuf]) -> Vec<LockHolder> {
    use std::os::windows::ffi::OsStrExt as _;

    use windows_sys::Win32::Foundation::{ERROR_MORE_DATA, ERROR_SUCCESS};
    use windows_sys::Win32::System::RestartManager::{
        RmGetList, RmRegisterResources, RmStartSession, CCH_RM_SESSION_KEY, RM_PROCESS_INFO,
    };

    if files.is_empty() {
        return Vec::new();
    }

    // The session key is CCH_RM_SESSION_KEY wide characters plus a NUL.
    let mut key = [0u16; CCH_RM_SESSION_KEY as usize + 1];
    let mut handle = 0u32;
    // SAFETY: `handle` and `key` are valid writable out-parameters; the session
    // this opens is closed by `Session` on every path out of this function.
    let started = unsafe { RmStartSession(&mut handle, 0, key.as_mut_ptr()) };
    if started != ERROR_SUCCESS {
        return Vec::new();
    }
    let session = Session(handle);

    // RmRegisterResources wants an array of NUL-terminated wide paths; both the
    // buffers and the pointer array must outlive the call.
    let names: Vec<Vec<u16>> = files
        .iter()
        .map(|file| {
            file.as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect()
        })
        .collect();
    let pointers: Vec<*const u16> = names.iter().map(|name| name.as_ptr()).collect();
    // SAFETY: every name is a valid NUL-terminated wide string, the pointer
    // array holds exactly `pointers.len()` entries, and no applications or
    // services are registered.
    let registered = unsafe {
        RmRegisterResources(
            session.0,
            pointers.len() as u32,
            pointers.as_ptr(),
            0,
            std::ptr::null(),
            0,
            std::ptr::null(),
        )
    };
    if registered != ERROR_SUCCESS {
        return Vec::new();
    }

    // The first call sizes the process list, the second fills it. A process can
    // appear between the two calls, so grow and retry a bounded number of
    // times instead of trusting one allocation.
    let mut needed = 0u32;
    let mut count = 0u32;
    let mut reasons = 0u32;
    // SAFETY: a null buffer with a zero count is the documented sizing call.
    let mut status = unsafe {
        RmGetList(
            session.0,
            &mut needed,
            &mut count,
            std::ptr::null_mut(),
            &mut reasons,
        )
    };
    let mut infos: Vec<RM_PROCESS_INFO> = Vec::new();
    for _ in 0..4 {
        if status != ERROR_MORE_DATA {
            break;
        }
        infos = vec![RM_PROCESS_INFO::default(); needed as usize];
        count = needed;
        // SAFETY: `infos` holds exactly `count` writable entries.
        status = unsafe {
            RmGetList(
                session.0,
                &mut needed,
                &mut count,
                infos.as_mut_ptr(),
                &mut reasons,
            )
        };
    }
    if status != ERROR_SUCCESS {
        return Vec::new();
    }
    infos.truncate(count.min(infos.len() as u32) as usize);

    let mut holders: Vec<LockHolder> = Vec::new();
    for info in infos {
        let pid = info.Process.dwProcessId;
        let name = wide_string(&info.strAppName);
        if !holders
            .iter()
            .any(|holder| holder.pid == pid && holder.name == name)
        {
            holders.push(LockHolder { name, pid });
        }
    }
    holders
}

/// A Restart Manager session that is always ended, including on an early
/// return or a panic.
#[cfg(windows)]
struct Session(u32);

#[cfg(windows)]
impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: `self.0` is the session handle `RmStartSession` returned.
        unsafe {
            windows_sys::Win32::System::RestartManager::RmEndSession(self.0);
        }
    }
}

/// Decode one fixed-size, NUL-terminated UTF-16 field of `RM_PROCESS_INFO`.
#[cfg(windows)]
fn wide_string(wide: &[u16]) -> String {
    let end = wide
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(wide.len());
    String::from_utf16_lossy(&wide[..end]).trim().to_string()
}

#[cfg(all(test, windows))]
mod tests {
    use std::os::windows::fs::OpenOptionsExt as _;

    use super::{held_by_suffix, holders, LockHolder};

    #[test]
    fn holders_find_the_process_keeping_a_file_open() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let file = directory.path().join("held.txt");
        std::fs::write(&file, b"held").expect("write the held file");
        // Share mode 0 is what blocks deletion with `os error 32`: no other
        // process may open the file until this handle is dropped.
        let handle = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&file)
            .expect("keep the file open");

        let found = holders(&[directory.path().to_path_buf()]);

        let own_pid = std::process::id();
        assert!(
            found.iter().any(|holder| holder.pid == own_pid),
            "expected own pid {own_pid} among {found:?}"
        );
        // The removal error appends exactly this suffix, so the message the
        // user sees names the process that has to be closed.
        let suffix = held_by_suffix(&found);
        assert!(
            suffix.starts_with("; held by ") && suffix.contains(&format!("(pid {own_pid})")),
            "suffix: {suffix}"
        );
        drop(handle);
    }

    #[test]
    fn the_suffix_names_holders_or_says_none_were_found() {
        assert_eq!(
            held_by_suffix(&[]),
            " (no process found holding a file in it; a terminal or program whose working folder is inside it can also block removal)"
        );
        let found = [
            LockHolder {
                name: "Code.exe".to_string(),
                pid: 42,
            },
            LockHolder {
                name: String::new(),
                pid: 7,
            },
        ];
        assert_eq!(
            held_by_suffix(&found),
            "; held by Code.exe (pid 42), an unnamed process (pid 7)"
        );
    }
}
