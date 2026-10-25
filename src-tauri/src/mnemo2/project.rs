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
use sha2::{Digest, Sha256};

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

/// Workbench state restored independently for each project.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectPreferences {
    /// Whether the workbench was open.
    pub workbench_open: bool,
    /// `browser`, `diff`, or `plan`.
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
        if !matches!(self.workbench_tab.as_str(), "browser" | "diff" | "plan") {
            return Err("invalid workbench tab in project preferences".to_string());
        }
        if !(320..=720).contains(&self.workbench_width) {
            return Err("workbench width must be between 320 and 720 pixels".to_string());
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

/// One stable state directory per canonical project, not a shared global `lead/`.
pub fn state_dir(app_data: &Path, root: &Path) -> Result<PathBuf, String> {
    let root = canonical_root(root)?;
    let digest = Sha256::digest(root.to_string_lossy().as_bytes());
    let key = digest.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    Ok(app_data.join("projects").join(key))
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
pub fn preferences(app_data: &Path, root: &Path) -> Result<ProjectPreferences, String> {
    let path = state_dir(app_data, root)?.join("preferences.json");
    let preferences = read_json::<ProjectPreferences>(&path)?.unwrap_or_default();
    preferences.validate()?;
    Ok(preferences)
}

/// Persist validated preferences through the backup-recoverable replacement path.
pub fn save_preferences(
    app_data: &Path,
    root: &Path,
    preferences: &ProjectPreferences,
) -> Result<(), String> {
    preferences.validate()?;
    write_json(
        &state_dir(app_data, root)?.join("preferences.json"),
        preferences,
    )
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

async fn inspect(mut record: ProjectRecord, app_data: &Path) -> Result<ProjectRecord, String> {
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
    let state = state_dir(app_data, &root)?;
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
    app_data: &Path,
) -> Result<Vec<ProjectRecord>, String> {
    let mut records = list(config_dir)?;
    for record in &mut records {
        *record = inspect(record.clone(), app_data).await?;
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

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
    fn attach_requires_confirmation_for_a_live_incumbent() {
        let root = tempdir().expect("project");
        #[cfg(windows)]
        let mut command = std::process::Command::new("cmd");
        #[cfg(windows)]
        command.args(["/c", "timeout", "/t", "5"]);
        #[cfg(not(windows))]
        let mut command = std::process::Command::new("sh");
        #[cfg(not(windows))]
        command.args(["-c", "sleep 5"]);
        let mut child = command.spawn().expect("live helper process");
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
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn state_is_project_scoped_and_preferences_recover_the_last_good_backup() {
        let app_data = tempdir().expect("app data");
        let one = tempdir().expect("one");
        let two = tempdir().expect("two");
        let first = state_dir(app_data.path(), one.path()).expect("first state");
        let second = state_dir(app_data.path(), two.path()).expect("second state");
        assert_ne!(first, second);
        assert_eq!(state_dir(app_data.path(), one.path()).expect("stable"), first);
        let preferences = ProjectPreferences {
            workbench_open: true,
            workbench_tab: "diff".to_string(),
            workbench_width: 640,
            browser_url: "http://localhost:3000/".to_string(),
            diff_worker_id: Some("worker1".to_string()),
        };
        save_preferences(app_data.path(), one.path(), &preferences).expect("save");
        let mut newer = preferences.clone();
        newer.workbench_width = 700;
        save_preferences(app_data.path(), one.path(), &newer).expect("second save");
        assert!(first.join("preferences.json.bak").exists(), "the last good backup must survive replacement");
        std::fs::write(first.join("preferences.json"), "broken").expect("corrupt primary");
        assert_eq!(super::preferences(app_data.path(), one.path()).expect("backup"), preferences);
        let mut invalid = preferences;
        invalid.browser_url = "file:///etc/passwd".to_string();
        assert!(save_preferences(app_data.path(), one.path(), &invalid).is_err());
    }

    #[tokio::test]
    async fn create_initializes_a_non_main_repository_and_lists_real_metadata() {
        let config = tempdir().expect("config");
        let parent = tempdir().expect("parent");
        let app_data = tempdir().expect("app data");
        let created = create(Some(config.path().to_path_buf()), parent.path(), "demo".to_string())
            .await
            .expect("create");
        assert!(parent.path().join("demo/.git").exists());
        assert!(create(Some(config.path().to_path_buf()), parent.path(), "demo".to_string())
            .await
            .is_err());
        let state = state_dir(app_data.path(), parent.path().join("demo").as_path()).expect("state");
        write_json(&state.join("lead.json"), &serde_json::json!({
            "model": "local/model",
            "updated_at_ms": 1234,
            "transcript": { "messages": [] }
        })).expect("lead metadata");
        write_json(&state.join("team.json"), &serde_json::json!({
            "slots": 5,
            "workers": [{ "id": "one", "status": "interrupted", "merge": null }]
        })).expect("team metadata");
        let records = list_with_state(Some(config.path().to_path_buf()), app_data.path())
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
