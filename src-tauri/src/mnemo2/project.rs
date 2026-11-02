// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]

//! Project discovery, per-project state, and conservative lifecycle helpers.

use std::path::{Path, PathBuf};

use mnemo::config::projects::ProjectRegistry;
use mnemo::config::resolve_config_dir;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::mnemo2::phase3::git_value;
use crate::mnemo2::team::{DEFAULT_SLOTS, MAX_WORKER_SLOTS};

/// One real project and its actual saved/Git state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectRecord {
    /// The display name (the folder's basename, made unique in the registry).
    pub name: String,
    /// The canonical folder path.
    pub path: String,
    /// Actual current branch, or an empty string outside a Git checkout.
    #[serde(default)]
    pub branch: String,
    /// Last saved lead-session time in Unix epoch milliseconds.
    #[serde(default)]
    pub last_session_ms: Option<u128>,
    /// Number of resumable, unmerged worker records.
    #[serde(default)]
    pub unfinished_workers: u64,
    /// All retained unmerged branches, including review-only cleaned work.
    #[serde(default)]
    pub review_workers: u64,
    /// The durable live-worker cap for this project.
    #[serde(default = "default_slots")]
    pub worker_slots: usize,
    /// Saved lead model spec, when a session exists.
    #[serde(default)]
    pub lead_model: Option<String>,
    /// Whether the registered folder still exists and is a directory.
    #[serde(default = "default_available")]
    pub available: bool,
    /// An unreadable saved-state diagnostic, never silently discarded.
    #[serde(default)]
    pub state_error: Option<String>,
}

fn default_slots() -> usize {
    DEFAULT_SLOTS
}

fn default_available() -> bool {
    true
}

impl ProjectRecord {
    fn new(name: String, path: String) -> Self {
        Self {
            name,
            path,
            branch: String::new(),
            last_session_ms: None,
            unfinished_workers: 0,
            review_workers: 0,
            worker_slots: DEFAULT_SLOTS,
            lead_model: None,
            available: true,
            state_error: None,
        }
    }
}

/// The Workbench tabs a project may persist, in strip order.
///
/// The Files, Trace and Git steps appended theirs here, next to these, so the
/// stored tab and the frontend's `WorkbenchTab` union stay one decision.
pub const WORKBENCH_TABS: [&str; 6] = ["browser", "diff", "plan", "files", "trace", "git"];

/// The narrowest workbench width that may be persisted, in CSS pixels.
const MIN_WORKBENCH_WIDTH: u32 = 320;

/// The widest workbench width that may be persisted, in CSS pixels.
///
/// The panel is window-relative (`window - lead`), so a wide display may
/// legally hold a much wider panel than the old fixed 720 ceiling allowed.
const MAX_WORKBENCH_WIDTH: u32 = 8_192;

/// Workbench state restored independently for each project.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectPreferences {
    /// Whether the workbench was open.
    pub workbench_open: bool,
    /// `browser`, `diff`, `plan`, `files`, or a tab a later step adds to
    /// [`WORKBENCH_TABS`].
    pub workbench_tab: String,
    /// Saved desktop width in CSS pixels.
    pub workbench_width: u32,
    /// Last actual browser URL, or `about:blank`.
    pub browser_url: String,
    /// Last selected diff worker, when it still exists.
    pub diff_worker_id: Option<String>,
}

impl Default for ProjectPreferences {
    fn default() -> Self {
        Self {
            workbench_open: false,
            workbench_tab: "browser".to_string(),
            workbench_width: 480,
            browser_url: "about:blank".to_string(),
            diff_worker_id: None,
        }
    }
}

impl ProjectPreferences {
    fn validate(&self) -> Result<(), String> {
        if !WORKBENCH_TABS.contains(&self.workbench_tab.as_str()) {
            return Err("invalid workbench tab in project preferences".to_string());
        }
        if !(MIN_WORKBENCH_WIDTH..=MAX_WORKBENCH_WIDTH).contains(&self.workbench_width) {
            return Err(format!(
                "workbench width must be between {MIN_WORKBENCH_WIDTH} and {MAX_WORKBENCH_WIDTH} pixels"
            ));
        }
        if self.browser_url != "about:blank" {
            let url = tauri::Url::parse(&self.browser_url)
                .map_err(|err| format!("invalid saved browser URL: {err}"))?;
            if !matches!(url.scheme(), "http" | "https") {
                return Err("saved browser URLs must use HTTP(S)".to_string());
            }
        }
        if self.diff_worker_id.as_deref().is_some_and(|id| {
            id.is_empty() || id.len() > 128 || !id.chars().all(|c| c.is_ascii_alphanumeric())
        }) {
            return Err("invalid worker id in project preferences".to_string());
        }
        Ok(())
    }
}

fn registry_path(config_dir: Option<PathBuf>) -> PathBuf {
    resolve_config_dir(config_dir).join("projects.toml")
}

/// Canonicalize a selected project directory, rejecting missing files.
pub fn canonical_root(root: &Path) -> Result<PathBuf, String> {
    let root = dunce::canonicalize(root)
        .map_err(|err| format!("the selected project does not exist: {err}"))?;
    if !root.is_dir() {
        return Err(format!("the selected path is not a folder: {}", root.display()));
    }
    Ok(root)
}

/// The result of checking a project's current instance marker.
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct InstanceAttachment {
    /// Another live process using this project, if opening should be confirmed.
    pub conflict_pid: Option<u32>,
    /// The other process's recorded launch time, when present.
    pub conflict_started_at: Option<u64>,
}

/// Check for a live incumbent before claiming a project for this process.
pub fn attach(root: &Path, allow_conflict: bool) -> Result<InstanceAttachment, String> {
    let root = canonical_root(root)?;
    if let Some(conflict) = crate::instance_marker::conflict_for(
        &root,
        std::process::id(),
        crate::instance_marker::pid_alive,
    ) {
        if !allow_conflict {
            return Ok(InstanceAttachment {
                conflict_pid: Some(conflict.pid),
                conflict_started_at: Some(conflict.started_at),
            });
        }
    }
    let _ = crate::instance_marker::InstanceMarker::new_self().write(&root);
    Ok(InstanceAttachment::default())
}

/// Release only this process's project marker, preserving newer incumbents.
pub fn detach(root: &Path) {
    if let Ok(root) = canonical_root(root) {
        let _ = crate::instance_marker::remove_marker_if_self(&root, std::process::id());
    }
}

/// The folder under a project's `.coding/` that holds all of Mnemo2's state
/// for that project.
pub const STATE_SUBDIR: &str = "mnemo2";

/// Keeps the state folder out of the project's commits: its contents are
/// per-machine (absolute worktree paths, multi-MB transcripts), and a lead
/// save must never dirty the checkout that merges and cleanup require clean.
const STATE_GITIGNORE: &str = "# Mnemo2 per-machine session state; never committed.\n*\n";

/// The stamp inside a state folder naming the machine that created it.
const OWNER_FILE: &str = "owner";

/// This machine's owner id, kept in the app-data dir — the one piece of
/// Mnemo2 data outside projects. It proves a project's state folder was
/// created here: a folder that arrived with a repository or archive cannot
/// carry it, so its transcript is never resumed as the lead's own history.
const OWNER_ID_FILE: &str = "state-owner-id";

/// A project's Mnemo2 state lives inside the project, in `.coding/mnemo2/`:
/// the lead transcript, worker ledger, workbench preferences, and engine
/// workspace. Deleting the project deletes its state with it. This is the
/// path only — read or write it through [`open_state_dir`].
pub fn state_dir(root: &Path) -> Result<PathBuf, String> {
    Ok(canonical_root(root)?.join(".coding").join(STATE_SUBDIR))
}

/// This machine's owner id, created once (atomically) in `app_data`.
pub fn machine_owner_id(app_data: &Path) -> Result<String, String> {
    let path = app_data.join(OWNER_ID_FILE);
    let read = || {
        std::fs::read_to_string(&path)
            .ok()
            .map(|id| id.trim().to_string())
            .filter(|id| id.len() == 32 && id.chars().all(|c| c.is_ascii_hexdigit()))
    };
    if let Some(id) = read() {
        return Ok(id);
    }
    std::fs::create_dir_all(app_data)
        .map_err(|err| format!("could not create {}: {err}", app_data.display()))?;
    let id = uuid::Uuid::new_v4().simple().to_string();
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut file) => {
            use std::io::Write as _;
            file.write_all(id.as_bytes())
                .map_err(|err| format!("could not write {}: {err}", path.display()))?;
            Ok(id)
        }
        // Another instance created it first: use theirs.
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            read().ok_or_else(|| format!("{} is unreadable", path.display()))
        }
        Err(err) => Err(format!("could not create {}: {err}", path.display())),
    }
}

/// The verified state folder for a read-only operation, or its would-be path
/// when the project has no state yet (readers then simply find nothing).
pub fn state_owner_checked(root: &Path, owner: &str) -> Result<PathBuf, String> {
    match open_state_dir(root, owner, false)? {
        Some(dir) => Ok(dir),
        None => state_dir(root),
    }
}

/// Reject a path that is a symlink/junction or not a folder; `Ok(false)` when
/// it does not exist.
fn real_folder(path: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(format!(
            "{} is a link; Mnemo2 keeps project state only in a real folder inside the project",
            path.display()
        )),
        Ok(meta) if !meta.is_dir() => Err(format!("{} is not a folder", path.display())),
        Ok(_) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(format!("could not inspect {}: {err}", path.display())),
    }
}

/// Write a small file, refusing to follow a link planted in its place.
fn write_plain_file(path: &Path, contents: &str) -> Result<(), String> {
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if !meta.is_file() || meta.file_type().is_symlink() {
            return Err(format!("{} is not a plain file", path.display()));
        }
    }
    std::fs::write(path, contents).map_err(|err| format!("could not write {}: {err}", path.display()))
}

/// The project's state folder, verified before any of it is trusted:
/// - neither `.coding` nor `.coding/mnemo2` is a link, and the folder resolves
///   inside the project, so no write can land elsewhere;
/// - it carries this machine's owner stamp. An unstamped folder holding state
///   came from somewhere else (a cloned repository, an archive) and is refused,
///   never resumed. An unstamped folder with no state is simply new.
///
/// With `create`, a missing or new folder is created and stamped and its
/// `.gitignore` rewritten; without it, `Ok(None)` means "no state yet".
pub fn open_state_dir(root: &Path, owner: &str, create: bool) -> Result<Option<PathBuf>, String> {
    let root = canonical_root(root)?;
    let coding = root.join(".coding");
    let dir = coding.join(STATE_SUBDIR);
    // Check before creating: create_dir_all would follow a planted link.
    if !real_folder(&coding)? || !real_folder(&dir)? {
        if !create {
            return Ok(None);
        }
        std::fs::create_dir_all(&dir)
            .map_err(|err| format!("could not create {}: {err}", dir.display()))?;
        real_folder(&coding)?;
        real_folder(&dir)?;
    }
    let resolved = dunce::canonicalize(&dir)
        .map_err(|err| format!("could not resolve {}: {err}", dir.display()))?;
    if !resolved.starts_with(&root) {
        return Err(format!("{} resolves outside the project", dir.display()));
    }
    let stamp = dir.join(OWNER_FILE);
    let stamped = std::fs::symlink_metadata(&stamp).is_ok_and(|meta| meta.is_file())
        && std::fs::read_to_string(&stamp).is_ok_and(|id| id.trim() == owner);
    if !stamped {
        let holds_state = std::fs::read_dir(&dir)
            .map_err(|err| format!("could not read {}: {err}", dir.display()))?
            .filter_map(Result::ok)
            .any(|entry| entry.file_name() != ".gitignore");
        if holds_state {
            return Err(format!(
                "{} was not created by Mnemo2 on this machine (it may have come with the repository), so its conversation and worker history are not loaded. Delete or rename that folder to start a fresh session here.",
                dir.display()
            ));
        }
        if !create {
            return Ok(None);
        }
        write_plain_file(&stamp, owner)?;
    }
    if create {
        // Always rewrite: never trust an existing ignore file's contents.
        write_plain_file(&dir.join(".gitignore"), STATE_GITIGNORE)?;
    }
    Ok(Some(dir))
}

pub(crate) fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, String> {
    fn decode<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, String> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("could not read {}: {error}", path.display())),
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| format!("could not decode {}: {error}", path.display()))
    }

    let backup = path.with_extension("json.bak");
    match decode(path) {
        Ok(Some(value)) => Ok(Some(value)),
        Ok(None) => decode(&backup),
        Err(primary) => match decode(&backup) {
            Ok(Some(value)) => Ok(Some(value)),
            Ok(None) => Err(primary),
            Err(backup) => Err(format!("{primary}; backup also failed: {backup}")),
        },
    }
}

pub(crate) fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("could not create the state directory: {err}"))?;
    }
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|err| format!("could not encode project state: {err}"))?;
    let temp = path.with_extension("json.tmp");
    let backup = path.with_extension("json.bak");
    std::fs::write(&temp, bytes).map_err(|err| format!("could not write project state: {err}"))?;
    if !path.exists() && backup.exists() {
        std::fs::rename(&backup, path)
            .map_err(|err| format!("could not restore the state backup: {err}"))?;
    }
    if backup.exists() {
        std::fs::remove_file(&backup)
            .map_err(|err| format!("could not clear the old state backup: {err}"))?;
    }
    if path.exists() {
        std::fs::rename(path, &backup)
            .map_err(|err| format!("could not back up project state: {err}"))?;
    }
    match std::fs::rename(&temp, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            if backup.exists() {
                let _ = std::fs::rename(&backup, path);
            }
            Err(format!("could not replace project state: {error}"))
        }
    }
}

/// Load and validate a project's saved workbench preferences.
pub fn preferences(root: &Path, owner: &str) -> Result<ProjectPreferences, String> {
    let Some(dir) = open_state_dir(root, owner, false)? else {
        return Ok(ProjectPreferences::default());
    };
    let preferences = read_json::<ProjectPreferences>(&dir.join("preferences.json"))?
        .unwrap_or_default();
    preferences.validate()?;
    Ok(preferences)
}

/// Persist validated preferences through the backup-recoverable replacement path.
pub fn save_preferences(
    root: &Path,
    owner: &str,
    preferences: &ProjectPreferences,
) -> Result<(), String> {
    preferences.validate()?;
    let dir = open_state_dir(root, owner, true)?
        .ok_or_else(|| "the project state folder could not be created".to_string())?;
    write_json(&dir.join("preferences.json"), preferences)
}

/// List registered projects without inventing session metadata.
pub fn list(config_dir: Option<PathBuf>) -> Result<Vec<ProjectRecord>, String> {
    let path = registry_path(config_dir);
    let registry = ProjectRegistry::load_or_default(&path)
        .map_err(|err| format!("could not read projects.toml: {err}"))?;
    Ok(registry
        .iter()
        .map(|entry| ProjectRecord::new(entry.name.clone(), entry.path.clone()))
        .collect())
}

/// Register a real folder, deriving a stable display name and canonical path.
pub fn add(config_dir: Option<PathBuf>, folder: &Path) -> Result<ProjectRecord, String> {
    let root = canonical_root(folder)?;
    let base = root
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "could not derive a project name from the folder".to_string())?
        .to_string();
    let path = root.to_string_lossy().to_string();
    let registry_path = registry_path(config_dir);
    if let Some(parent) = registry_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("could not create the config directory: {err}"))?;
    }
    let mut registry = ProjectRegistry::load_or_default(&registry_path)
        .map_err(|err| format!("could not read projects.toml: {err}"))?;
    let mut name = base.clone();
    let mut suffix = 2;
    while let Some(existing) = registry.get(&name) {
        if existing == path
            || canonical_root(Path::new(existing)).ok().as_deref() == Some(root.as_path())
        {
            break;
        }
        name = format!("{base}-{suffix}");
        suffix += 1;
    }
    registry.add(&name, &path);
    registry
        .save(&registry_path)
        .map_err(|err| format!("could not save projects.toml: {err}"))?;
    Ok(ProjectRecord::new(name, path))
}

fn validate_new_project_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty()
        || name.len() > 80
        || matches!(name, "." | "..")
        || name.ends_with([' ', '.'])
        || name.chars().any(|c| c.is_control() || "/\\:*?\"<>|".contains(c))
    {
        return Err("use a short folder name without path separators or reserved characters".to_string());
    }
    let stem = name.split('.').next().unwrap_or(name);
    let reserved = matches!(stem.to_ascii_uppercase().as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.is_ascii() && stem.len() == 4
            && (stem[..3].eq_ignore_ascii_case("COM") || stem[..3].eq_ignore_ascii_case("LPT"))
            && stem[3..].chars().all(|c| matches!(c, '1'..='9')));
    if reserved {
        return Err("that folder name is reserved by Windows".to_string());
    }
    Ok(name.to_string())
}

/// Create a new directory and a non-main Git working branch, then register it.
pub async fn create(
    config_dir: Option<PathBuf>,
    parent: &Path,
    name: String,
) -> Result<ProjectRecord, String> {
    let name = validate_new_project_name(&name)?;
    let parent = canonical_root(parent)?;
    let root = parent.join(&name);
    std::fs::create_dir(&root).map_err(|err| {
        format!("could not create {} (choose a new, unused folder name): {err}", root.display())
    })?;
    let root_arg = root
        .to_str()
        .ok_or_else(|| "the project path is not valid UTF-8".to_string())?;
    let output = crate::mnemo2::phase3::git(
        &parent,
        &["init", "--initial-branch=wt/project", root_arg],
    )
    .await?;
    if !output.status.success() {
        return Err(crate::mnemo2::phase3::git_error(
            &["init", "--initial-branch=wt/project", root_arg],
            &output,
        ));
    }
    add(config_dir, &root)
}

async fn inspect(mut record: ProjectRecord, owner: &str) -> Result<ProjectRecord, String> {
    let root = match canonical_root(Path::new(&record.path)) {
        Ok(root) => root,
        Err(error) => {
            record.available = false;
            record.state_error = Some(error);
            return Ok(record);
        }
    };
    record.path = root.to_string_lossy().into_owned();
    if let Ok(branch) = git_value(&root, &["symbolic-ref", "--short", "HEAD"]).await {
        record.branch = branch;
    }
    let state = match open_state_dir(&root, owner, false) {
        Ok(Some(state)) => state,
        Ok(None) => return Ok(record),
        Err(error) => {
            // Shown on the start screen; untrusted state is never summarized.
            record.state_error = Some(error);
            return Ok(record);
        }
    };
    match read_json::<serde_json::Value>(&state.join("lead.json")) {
        Ok(Some(lead)) => {
            record.last_session_ms = lead.get("updated_at_ms").and_then(|value| value.as_u64()).map(u128::from);
            record.lead_model = lead.get("model").and_then(|value| value.as_str()).map(str::to_string);
            if record.last_session_ms.is_none() {
                record.last_session_ms = std::fs::metadata(state.join("lead.json"))
                    .ok()
                    .and_then(|metadata| metadata.modified().ok())
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|duration| duration.as_millis());
            }
        }
        Ok(None) => {}
        Err(error) => record.state_error = Some(error),
    }
    match read_json::<serde_json::Value>(&state.join("team.json")) {
        Ok(Some(team)) => {
            if let Some(slots) = team.get("slots").and_then(|value| value.as_u64()) {
                if (1..=MAX_WORKER_SLOTS as u64).contains(&slots) {
                    record.worker_slots = slots as usize;
                } else {
                    record.state_error = Some("saved worker slots are invalid".to_string());
                }
            }
            if let Some(workers) = team.get("workers").and_then(|value| value.as_array()) {
                record.review_workers = workers.iter().filter(|worker| {
                    worker.get("merge").is_none_or(|merge| merge.is_null())
                }).count() as u64;
                record.unfinished_workers = workers.iter().filter(|worker| {
                    worker.get("merge").is_none_or(|merge| merge.is_null())
                        && worker.get("status").and_then(|status| status.as_str()) != Some("cancelled")
                }).count() as u64;
            }
        }
        Ok(None) => {}
        Err(error) => record.state_error = Some(error),
    }
    Ok(record)
}

/// List registry entries enriched with actual Git and per-project saved state.
pub async fn list_with_state(
    config_dir: Option<PathBuf>,
    owner: &str,
) -> Result<Vec<ProjectRecord>, String> {
    let mut records = list(config_dir)?;
    for record in &mut records {
        *record = inspect(record.clone(), owner).await?;
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    const OWNER: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn registry_round_trip_and_duplicate_names_work() {
        let config = tempdir().expect("config");
        let one = tempdir().expect("one");
        let two = tempdir().expect("two");
        let first = one.path().join("same");
        let second = two.path().join("same");
        std::fs::create_dir(&first).expect("first");
        std::fs::create_dir(&second).expect("second");
        let a = add(Some(config.path().to_path_buf()), &first).expect("first add");
        let b = add(Some(config.path().to_path_buf()), &second).expect("second add");
        assert_eq!(a.name, "same");
        assert_eq!(b.name, "same-2");
        assert_eq!(list(Some(config.path().to_path_buf())).expect("list"), vec![a, b]);
    }

    #[test]
    fn add_rejects_missing_folders_and_files() {
        let config = tempdir().expect("config");
        assert!(add(Some(config.path().to_path_buf()), &config.path().join("missing")).is_err());
        let file = config.path().join("file");
        std::fs::write(&file, "not a directory").expect("file");
        assert!(add(Some(config.path().to_path_buf()), &file).is_err());
    }

    #[test]
    fn preferences_accept_every_workbench_tab_and_window_relative_widths() {
        for tab in WORKBENCH_TABS {
            let preferences = ProjectPreferences {
                workbench_open: true,
                workbench_tab: tab.to_string(),
                workbench_width: 1_024,
                ..ProjectPreferences::default()
            };
            assert!(preferences.validate().is_ok(), "the '{tab}' tab must be accepted");
        }
        assert!(
            WORKBENCH_TABS.contains(&"files"),
            "the Files tab is part of the persisted set"
        );
        assert!(
            WORKBENCH_TABS.contains(&"trace"),
            "the Trace tab is part of the persisted set"
        );
        assert!(
            WORKBENCH_TABS.contains(&"git"),
            "the Git tab is part of the persisted set"
        );
        let unknown = ProjectPreferences {
            workbench_tab: "console".to_string(),
            ..ProjectPreferences::default()
        };
        assert!(unknown.validate().is_err(), "an unknown tab is still refused");

        // The panel is window-relative, so widths well past the old 720 ceiling
        // are legal; only the outer bounds are refused.
        for width in [MIN_WORKBENCH_WIDTH, 720, 1_024, MAX_WORKBENCH_WIDTH] {
            let preferences = ProjectPreferences {
                workbench_width: width,
                ..ProjectPreferences::default()
            };
            assert!(
                preferences.validate().is_ok(),
                "a {width}px panel must be accepted"
            );
        }
        for width in [MIN_WORKBENCH_WIDTH - 1, MAX_WORKBENCH_WIDTH + 1] {
            let preferences = ProjectPreferences {
                workbench_width: width,
                ..ProjectPreferences::default()
            };
            assert!(
                preferences.validate().is_err(),
                "a {width}px panel must be refused"
            );
        }
    }

    /// Kills and reaps a helper child on drop, so a failing assertion cannot
    /// leave a live process running for the rest of the suite.
    struct LiveChild(std::process::Child);

    impl LiveChild {
        fn id(&self) -> u32 {
            self.0.id()
        }
    }

    impl Drop for LiveChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// A live incumbent must force confirmation. The stand-in process has to
    /// genuinely stay alive for the whole test: `cmd /c timeout /t 5` — the
    /// original helper — exits immediately once its stdin is redirected, so
    /// under full-suite load the "live" pid could already be gone, `pid_alive`
    /// correctly read it as dead, and the conflict assertion failed
    /// intermittently. Loopback `ping` never reads stdin and lives ~30 s.
    #[test]
    fn attach_requires_confirmation_for_a_live_incumbent() {
        let root = tempdir().expect("project");
        #[cfg(windows)]
        let mut command = std::process::Command::new("ping");
        #[cfg(windows)]
        command.args(["-n", "30", "127.0.0.1"]);
        #[cfg(not(windows))]
        let mut command = std::process::Command::new("sleep");
        #[cfg(not(windows))]
        command.args(["30"]);
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let child = LiveChild(command.spawn().expect("live helper process"));
        crate::instance_marker::InstanceMarker {
            pid: child.id(),
            started_at: 1,
        }
        .write(root.path())
        .expect("incumbent marker");
        assert_eq!(attach(root.path(), false).expect("conflict").conflict_pid, Some(child.id()));
        assert!(attach(root.path(), true).expect("allowed attachment").conflict_pid.is_none());
        detach(root.path());
        assert!(crate::instance_marker::read_marker(root.path()).is_none());
    }

    #[test]
    fn state_is_project_scoped_and_preferences_recover_the_last_good_backup() {
        let one = tempdir().expect("one");
        let two = tempdir().expect("two");
        let first = state_dir(one.path()).expect("first state");
        let second = state_dir(two.path()).expect("second state");
        assert_ne!(first, second);
        assert_eq!(state_dir(one.path()).expect("stable"), first);
        let preferences = ProjectPreferences {
            workbench_open: true,
            workbench_tab: "diff".to_string(),
            workbench_width: 640,
            browser_url: "http://localhost:3000/".to_string(),
            diff_worker_id: Some("worker1".to_string()),
        };
        save_preferences(one.path(), OWNER, &preferences).expect("save");
        let mut newer = preferences.clone();
        newer.workbench_width = 700;
        save_preferences(one.path(), OWNER, &newer).expect("second save");
        assert!(first.join("preferences.json.bak").exists(), "the last good backup must survive replacement");
        std::fs::write(first.join("preferences.json"), "broken").expect("corrupt primary");
        assert_eq!(super::preferences(one.path(), OWNER).expect("backup"), preferences);
        let mut invalid = preferences;
        invalid.browser_url = "file:///etc/passwd".to_string();
        assert!(save_preferences(one.path(), OWNER, &invalid).is_err());
    }

    /// Regression: state lived in the app-data dir keyed by a hash of the
    /// project path, so deleting a project's contents and reopening the same
    /// folder resurrected its old chat and worker history. It now lives in
    /// the project's own `.coding/mnemo2/`, ignored by Git so lead saves
    /// never dirty the checkout that merges and cleanup require clean.
    #[test]
    fn project_state_lives_in_dot_coding_and_never_dirties_git() {
        let project = tempdir().expect("project");
        let root = canonical_root(project.path()).expect("root");
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .expect("git runs");
            assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
            String::from_utf8_lossy(&output.stdout).into_owned()
        };
        git(&["init", "-b", "main"]);
        let dir = open_state_dir(project.path(), OWNER, true)
            .expect("state dir")
            .expect("created");
        assert_eq!(dir, root.join(".coding").join(STATE_SUBDIR));
        write_json(&dir.join("lead.json"), &serde_json::json!({ "transcript": { "messages": [] } }))
            .expect("lead state");
        assert!(git(&["status", "--porcelain", "--untracked-files=all"]).trim().is_empty());

        // Removing the project's files removes its state: nothing elsewhere
        // brings the old conversation back.
        std::fs::remove_dir_all(root.join(".coding")).expect("delete project state");
        assert!(read_json::<serde_json::Value>(&state_dir(project.path()).expect("dir").join("lead.json"))
            .expect("read")
            .is_none());
    }

    /// Security: a repository or archive can ship its own `.coding/mnemo2/`.
    /// Resuming that `lead.json` would inject a conversation into the lead,
    /// and a planted `team.json` would steer worktree cleanup. Only folders
    /// stamped by this machine are trusted.
    #[tokio::test]
    async fn shipped_or_foreign_state_is_refused_never_loaded() {
        let config = tempdir().expect("config");
        let parent = tempdir().expect("parent");
        let created = create(Some(config.path().to_path_buf()), parent.path(), "cloned".to_string())
            .await
            .expect("create");
        let root = Path::new(&created.path);
        let shipped = root.join(".coding").join(STATE_SUBDIR);
        std::fs::create_dir_all(&shipped).expect("shipped dir");
        std::fs::write(shipped.join("lead.json"), r#"{"transcript":{"messages":[]}}"#).expect("planted lead");

        let refused = open_state_dir(root, OWNER, true).expect_err("unstamped state is refused");
        assert!(refused.contains("not created by Mnemo2 on this machine"), "{refused}");
        assert!(super::preferences(root, OWNER).is_err());
        assert!(!shipped.join("owner").exists(), "a refused folder is never stamped");
        let listed = list_with_state(Some(config.path().to_path_buf()), OWNER).await.expect("list");
        assert!(listed[0].state_error.as_deref().is_some_and(|error| error.contains("not created by Mnemo2")));
        assert_eq!(listed[0].last_session_ms, None, "untrusted state is never summarized");

        // Another machine's stamp is just as foreign.
        std::fs::write(shipped.join("owner"), "ffffffffffffffffffffffffffffffff").expect("foreign stamp");
        assert!(open_state_dir(root, OWNER, false).is_err());

        // Renaming it away starts fresh; the new folder is stamped and trusted.
        std::fs::rename(&shipped, root.join(".coding").join("mnemo2.shipped")).expect("rename");
        let fresh = open_state_dir(root, OWNER, true).expect("fresh").expect("created");
        assert_eq!(std::fs::read_to_string(fresh.join("owner")).expect("stamp"), OWNER);
        assert_eq!(open_state_dir(root, OWNER, false).expect("trusted"), Some(fresh));
    }

    #[test]
    fn the_ignore_file_is_always_rewritten() {
        let project = tempdir().expect("project");
        let dir = open_state_dir(project.path(), OWNER, true).expect("dir").expect("created");
        std::fs::write(dir.join(".gitignore"), "!*\n").expect("tamper");
        open_state_dir(project.path(), OWNER, true).expect("reopen");
        assert_eq!(std::fs::read_to_string(dir.join(".gitignore")).expect("ignore"), STATE_GITIGNORE);
    }

    /// A `.coding` link would redirect every state write outside the project.
    #[cfg(windows)]
    #[test]
    fn a_linked_coding_folder_is_refused() {
        let project = tempdir().expect("project");
        let elsewhere = tempdir().expect("elsewhere");
        let link = project.path().join(".coding");
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(elsewhere.path())
            .output()
            .expect("cmd runs");
        assert!(made.status.success(), "{}", String::from_utf8_lossy(&made.stderr));
        let refused = open_state_dir(project.path(), OWNER, true).expect_err("linked .coding");
        assert!(refused.contains("is a link"), "{refused}");
        assert!(std::fs::read_dir(elsewhere.path()).expect("target").next().is_none(), "nothing was written through the link");
    }

    #[test]
    fn the_machine_owner_id_is_created_once_and_reused() {
        let app_data = tempdir().expect("app data");
        let first = machine_owner_id(app_data.path()).expect("created");
        assert_eq!(first.len(), 32);
        assert_eq!(machine_owner_id(app_data.path()).expect("reused"), first);
    }

    #[tokio::test]
    async fn create_initializes_a_non_main_repository_and_lists_real_metadata() {
        let config = tempdir().expect("config");
        let parent = tempdir().expect("parent");
        let created = create(Some(config.path().to_path_buf()), parent.path(), "demo".to_string())
            .await
            .expect("create");
        assert!(parent.path().join("demo/.git").exists());
        assert!(create(Some(config.path().to_path_buf()), parent.path(), "demo".to_string())
            .await
            .is_err());
        let state = open_state_dir(parent.path().join("demo").as_path(), OWNER, true)
            .expect("state")
            .expect("created");
        write_json(&state.join("lead.json"), &serde_json::json!({
            "model": "local/model",
            "updated_at_ms": 1234,
            "transcript": { "messages": [] }
        })).expect("lead metadata");
        write_json(&state.join("team.json"), &serde_json::json!({
            "slots": 5,
            "workers": [{ "id": "one", "status": "interrupted", "merge": null }]
        })).expect("team metadata");
        let records = list_with_state(Some(config.path().to_path_buf()), OWNER)
            .await
            .expect("metadata");
        assert_eq!(records[0].branch, "wt/project");
        assert_eq!(records[0].last_session_ms, Some(1234));
        assert_eq!(records[0].unfinished_workers, 1);
        assert_eq!(records[0].worker_slots, 5);
        assert_eq!(records[0].lead_model.as_deref(), Some("local/model"));
        assert_eq!(created.path, records[0].path);
    }
}
