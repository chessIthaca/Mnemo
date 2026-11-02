// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]

//! The per-instance run file: how the `mnemo2-mcp` helper finds a live Mnemo2.
//!
//! Each running Mnemo2 writes `<config>/run/mnemo2-<pid>.json` with the port
//! and bearer token of its local MCP endpoint plus the project it has open.
//! The helper walks the directory, drops entries whose process is gone, and
//! picks the one whose `project` contains its working directory. The file is
//! replaced atomically (temp file + rename) whenever it changes and removed on
//! a clean exit; a crash leaves a stale entry that readers prune by pid. The
//! run folder and file are restricted to the current user (it holds the
//! bearer token), on Windows with the same user-only DACL as `keys.toml`; the
//! running app writes the file again if it disappears
//! ([`RunFileHandle::restore_if_missing`]).
//!
//! Every function takes the config directory so tests run in a temp dir.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// The folder under the config dir that holds the run files.
pub const RUN_DIR: &str = "run";

/// One instance's run file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunFile {
    /// The Mnemo2 process id.
    pub pid: u32,
    /// The local MCP endpoint's port on 127.0.0.1.
    pub port: u16,
    /// The bearer token every request must carry (32 hex characters).
    pub token: String,
    /// The Mnemo2 version.
    pub version: String,
    /// The canonical root of the open project, if one is open.
    pub project: Option<String>,
    /// When this instance started, in Unix epoch milliseconds.
    pub started_ms: u64,
}

/// The run-file directory under a config dir.
pub fn run_dir(config_dir: &Path) -> PathBuf {
    config_dir.join(RUN_DIR)
}

/// One instance's run-file path.
pub fn run_path(config_dir: &Path, pid: u32) -> PathBuf {
    run_dir(config_dir).join(format!("mnemo2-{pid}.json"))
}

/// A fresh bearer token: 32 lowercase hex characters from a random UUID.
pub fn new_token() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Unix epoch milliseconds now.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Restrict `path` to the current user, the same way `keys.toml` is (mode
/// `0600` on Unix, a user-only DACL on Windows). Best effort: a filesystem
/// that cannot do it only costs a warning, never the run file.
fn restrict_to_user(path: &Path) {
    if let Err(error) = mnemo::config::keys::restrict_permissions(path) {
        eprintln!(
            "mnemo2: could not restrict '{}' to the current user: {error}",
            path.display()
        );
    }
}

/// Restrict the run folder to the current user (`0700` on Unix, a user-only
/// DACL that files created inside inherit on Windows: the helper's spawn lock
/// must stay readable to the helper). Best effort, like the file.
fn restrict_dir_to_user(dir: &Path) {
    if let Err(error) = mnemo::config::keys::restrict_dir_permissions(dir) {
        eprintln!(
            "mnemo2: could not restrict '{}' to the current user: {error}",
            dir.display()
        );
    }
}

/// Write the run file atomically (temp file in the same folder, then rename).
/// The run folder and the file are readable by the current user only.
pub fn write(config_dir: &Path, file: &RunFile) -> Result<(), String> {
    let dir = run_dir(config_dir);
    std::fs::create_dir_all(&dir)
        .map_err(|err| format!("could not create the run directory: {err}"))?;
    restrict_dir_to_user(&dir);
    let path = run_path(config_dir, file.pid);
    let temp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(file)
        .map_err(|err| format!("could not encode the run file: {err}"))?;
    write_private(&temp, &bytes)?;
    std::fs::rename(&temp, &path).map_err(|err| {
        let _ = std::fs::remove_file(&temp);
        format!("could not replace the run file: {err}")
    })
}

/// Create the file, restrict it to the current user, and only then put the
/// token into it, so the secret is never readable by anyone else.
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write as _;

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|err| format!("could not write the run file: {err}"))?;
    restrict_to_user(path);
    file.write_all(bytes)
        .map_err(|err| format!("could not write the run file: {err}"))
}

/// Remove one instance's run file; a missing file is fine.
pub fn remove(config_dir: &Path, pid: u32) {
    let _ = std::fs::remove_file(run_path(config_dir, pid));
}

/// Read one run file.
pub fn read(path: &Path) -> Result<RunFile, String> {
    let bytes =
        std::fs::read(path).map_err(|err| format!("could not read {}: {err}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map_err(|err| format!("could not decode {}: {err}", path.display()))
}

/// Every readable run file, oldest start first. Unreadable or foreign files
/// (a half-written temp file, a hand-edited one) are skipped, never fatal.
pub fn list(config_dir: &Path) -> Vec<RunFile> {
    let Ok(entries) = std::fs::read_dir(run_dir(config_dir)) else {
        return Vec::new();
    };
    let mut files: Vec<RunFile> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("mnemo2-") && name.ends_with(".json"))
        })
        .filter_map(|path| read(&path).ok())
        .collect();
    files.sort_by_key(|file| (file.started_ms, file.pid));
    files
}

/// Whether an entry is stale because its process is gone. Pass
/// [`crate::instance_marker::pid_alive`] as `alive` in production.
pub fn is_stale(file: &RunFile, alive: impl Fn(u32) -> bool) -> bool {
    !alive(file.pid)
}

/// Delete the run files of dead processes; returns the pids removed.
pub fn prune_stale(config_dir: &Path, alive: impl Fn(u32) -> bool) -> Vec<u32> {
    list(config_dir)
        .into_iter()
        .filter(|file| is_stale(file, &alive))
        .map(|file| {
            remove(config_dir, file.pid);
            file.pid
        })
        .collect()
}

/// How often the app checks that its run file still exists.
pub const RESTORE_EVERY: std::time::Duration = std::time::Duration::from_secs(30);

/// This process's run file: created at startup, rewritten when the open
/// project changes, removed on exit.
pub struct RunFileHandle {
    config_dir: PathBuf,
    state: Mutex<RunFile>,
    /// Set by [`RunFileHandle::remove`]; a removed handle never writes again.
    removed: AtomicBool,
}

impl RunFileHandle {
    /// Write this process's run file for an endpoint on `port`.
    pub fn create(config_dir: &Path, port: u16, token: String) -> Result<Self, String> {
        let file = RunFile {
            pid: std::process::id(),
            port,
            token,
            version: env!("CARGO_PKG_VERSION").to_string(),
            project: None,
            started_ms: now_ms(),
        };
        write(config_dir, &file)?;
        Ok(Self {
            config_dir: config_dir.to_path_buf(),
            state: Mutex::new(file),
            removed: AtomicBool::new(false),
        })
    }

    /// The current contents.
    pub fn snapshot(&self) -> RunFile {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Record the open project (`None` once it closes) and rewrite the file.
    pub fn set_project(&self, project: Option<&Path>) -> Result<(), String> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.project = project.map(|root| root.to_string_lossy().into_owned());
        write(&self.config_dir, &state)
    }

    /// Write the file again if it has disappeared (cleaned up by hand, an
    /// over-eager pruner, antivirus), so the helper can still find this
    /// instance. `Ok(true)` when it was restored; `Ok(false)` when it was
    /// still there or the handle was removed on purpose.
    pub fn restore_if_missing(&self) -> Result<bool, String> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if self.removed.load(Ordering::SeqCst) || run_path(&self.config_dir, state.pid).exists() {
            return Ok(false);
        }
        write(&self.config_dir, &state)?;
        Ok(true)
    }

    /// Whether [`RunFileHandle::remove`] has run.
    pub fn is_removed(&self) -> bool {
        self.removed.load(Ordering::SeqCst)
    }

    /// Remove the file (a clean exit).
    pub fn remove(&self) {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.removed.store(true, Ordering::SeqCst);
        remove(&self.config_dir, state.pid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(pid: u32, started_ms: u64) -> RunFile {
        RunFile {
            pid,
            port: 4242,
            token: new_token(),
            version: "1.0.0".into(),
            project: None,
            started_ms,
        }
    }

    #[test]
    fn tokens_are_32_hex_characters_and_unique() {
        let (first, second) = (new_token(), new_token());
        assert_eq!(first.len(), 32);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()), "{first}");
        assert_ne!(first, second);
    }

    #[test]
    fn write_read_update_remove_round_trip() {
        let dir = tempfile::tempdir().expect("config dir");
        let mut file = sample(111, 5);
        write(dir.path(), &file).expect("write");
        let path = run_path(dir.path(), 111);
        assert!(path.ends_with("run/mnemo2-111.json") || path.ends_with("run\\mnemo2-111.json"));
        assert_eq!(read(&path).expect("read"), file);
        // No temp file is left behind by the atomic replace.
        assert!(!path.with_extension("json.tmp").exists());

        file.project = Some("C:/work/app".into());
        write(dir.path(), &file).expect("rewrite");
        assert_eq!(
            read(&path).expect("read").project.as_deref(),
            Some("C:/work/app")
        );
        assert_eq!(list(dir.path()), vec![file]);

        remove(dir.path(), 111);
        assert!(list(dir.path()).is_empty());
        remove(dir.path(), 111); // removing twice is fine
    }

    #[test]
    fn the_wire_shape_matches_the_documented_contract() {
        let mut file = sample(7, 99);
        file.token = "0123456789abcdef0123456789abcdef".into();
        file.project = Some("/p".into());
        let value = serde_json::to_value(&file).expect("encode");
        assert_eq!(
            value,
            serde_json::json!({
                "pid": 7,
                "port": 4242,
                "token": "0123456789abcdef0123456789abcdef",
                "version": "1.0.0",
                "project": "/p",
                "started_ms": 99
            })
        );
        file.project = None;
        assert!(serde_json::to_value(&file).expect("encode")["project"].is_null());
    }

    #[test]
    fn list_skips_junk_and_orders_by_start() {
        let dir = tempfile::tempdir().expect("config dir");
        write(dir.path(), &sample(2, 20)).expect("write");
        write(dir.path(), &sample(1, 10)).expect("write");
        let run = run_dir(dir.path());
        std::fs::write(run.join("mnemo2-3.json"), b"{not json").expect("junk");
        std::fs::write(run.join("mnemo2-4.json.tmp"), b"{}").expect("temp");
        std::fs::write(run.join("other.json"), b"{}").expect("foreign");
        let pids: Vec<u32> = list(dir.path()).iter().map(|file| file.pid).collect();
        assert_eq!(pids, vec![1, 2]);
        assert!(list(&dir.path().join("missing")).is_empty());
    }

    #[test]
    fn stale_entries_are_pruned_by_pid() {
        let dir = tempfile::tempdir().expect("config dir");
        for pid in [10, 11, 12] {
            write(dir.path(), &sample(pid, u64::from(pid))).expect("write");
        }
        assert!(is_stale(&sample(11, 0), |pid| pid != 11));
        let mut removed = prune_stale(dir.path(), |pid| pid != 11);
        removed.sort_unstable();
        assert_eq!(removed, vec![11]);
        let left: Vec<u32> = list(dir.path()).iter().map(|file| file.pid).collect();
        assert_eq!(left, vec![10, 12]);
    }

    #[test]
    fn the_handle_tracks_the_open_project_and_cleans_up() {
        let dir = tempfile::tempdir().expect("config dir");
        let handle = RunFileHandle::create(dir.path(), 5555, new_token()).expect("create");
        let pid = std::process::id();
        let path = run_path(dir.path(), pid);
        let created = read(&path).expect("created");
        assert_eq!((created.pid, created.port), (pid, 5555));
        assert_eq!(created.version, env!("CARGO_PKG_VERSION"));
        assert!(created.project.is_none());

        handle.set_project(Some(Path::new("/work/app"))).expect("open");
        assert_eq!(read(&path).expect("read").project.as_deref(), Some("/work/app"));
        assert_eq!(handle.snapshot().project.as_deref(), Some("/work/app"));
        handle.set_project(None).expect("close");
        assert!(read(&path).expect("read").project.is_none());

        handle.remove();
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn the_run_file_is_private_to_the_user() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("config dir");
        write(dir.path(), &sample(9, 1)).expect("write");
        let mode = std::fs::metadata(run_path(dir.path(), 9))
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let dir_mode = std::fs::metadata(run_dir(dir.path()))
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(dir_mode & 0o777, 0o700);
    }

    /// The same bar as `keys.toml`: on Windows only the current user keeps an
    /// access entry, on both the run folder and the file (no inherited
    /// SYSTEM, Administrators or Users entries).
    #[cfg(windows)]
    #[test]
    fn the_run_file_and_folder_are_private_to_the_user_on_windows() {
        let dir = tempfile::tempdir().expect("config dir");
        write(dir.path(), &sample(9, 1)).expect("write");
        for path in [run_dir(dir.path()), run_path(dir.path(), 9)] {
            let output = std::process::Command::new("icacls")
                .arg(&path)
                .output()
                .expect("icacls runs");
            let text = String::from_utf8_lossy(&output.stdout).into_owned();
            assert!(output.status.success(), "{text}");
            // One access entry (`DOMAIN\user:(F)`), nobody else.
            assert_eq!(text.matches(":(").count(), 1, "{text}");
            let others = [
                "NT AUTHORITY\\SYSTEM",
                "BUILTIN\\Administrators",
                "BUILTIN\\Users",
                "Everyone",
            ];
            for other in others {
                assert!(!text.contains(other), "{other} still has access: {text}");
            }
        }
        // Files other processes create in the folder (the helper's spawn
        // lock) inherit the user's entry, so they can be read back.
        let lock = run_dir(dir.path()).join("spawn-test.lock");
        std::fs::write(&lock, "token").expect("a helper can create its lock");
        assert_eq!(std::fs::read_to_string(&lock).expect("and read it back"), "token");
        // A rewrite (new project) keeps the restriction.
        let handle = RunFileHandle::create(dir.path(), 1, new_token()).expect("create");
        handle.set_project(Some(Path::new("C:/work"))).expect("rewrite");
        let output = std::process::Command::new("icacls")
            .arg(run_path(dir.path(), std::process::id()))
            .output()
            .expect("icacls runs");
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        assert_eq!(text.matches(":(").count(), 1, "{text}");
    }

    #[test]
    fn a_vanished_run_file_is_written_again_with_the_current_project() {
        let dir = tempfile::tempdir().expect("config dir");
        let handle = RunFileHandle::create(dir.path(), 5556, new_token()).expect("create");
        let path = run_path(dir.path(), std::process::id());
        assert_eq!(handle.restore_if_missing(), Ok(false), "nothing to restore");

        handle.set_project(Some(Path::new("/work/app"))).expect("open");
        let before = read(&path).expect("read");
        std::fs::remove_file(&path).expect("someone deletes it");
        assert_eq!(handle.restore_if_missing(), Ok(true));
        assert_eq!(read(&path).expect("restored"), before);
        assert_eq!(handle.restore_if_missing(), Ok(false), "restored once");

        // The run folder disappearing is repaired too.
        std::fs::remove_dir_all(run_dir(dir.path())).expect("folder deleted");
        assert_eq!(handle.restore_if_missing(), Ok(true));
        assert_eq!(read(&path).expect("restored"), before);
    }

    #[test]
    fn a_removed_handle_never_writes_the_file_again() {
        let dir = tempfile::tempdir().expect("config dir");
        let handle = RunFileHandle::create(dir.path(), 5557, new_token()).expect("create");
        let path = run_path(dir.path(), std::process::id());
        assert!(!handle.is_removed());
        handle.remove();
        assert!(handle.is_removed());
        assert!(!path.exists());
        assert_eq!(handle.restore_if_missing(), Ok(false), "a clean exit stays clean");
        assert!(!path.exists());
    }
}
