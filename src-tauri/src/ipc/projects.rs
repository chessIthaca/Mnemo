// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Project-picker Tauri commands.
//!
//! Backs the startup project picker and the runtime "switch project" control.
//! The project list lives in `projects.toml` (alongside `keys.toml` /
//! `endpoints.toml` / `endpoints.toml`) and is loaded into `Config.projects`.
//!
//! Switching projects restarts the app: the chosen path is written to a
//! one-shot marker file (`write_pending_project`), then
//! `tauri::process::restart()` relaunches the process. On restart,
//! `build_brain` consumes the marker (`take_pending_project`) and opens that
//! project. A restart reuses the original argv, so the marker is the
//! side-channel that carries the selection across the restart boundary.

use std::path::Path;

use tauri::{AppHandle, Manager, State};
use tauri_plugin_dialog::DialogExt;

use mnemo::config::{global_config_dir, write_pending_project};
use mnemo::project::Project;

use crate::ipc::codegraph_cmds::{emit_index_progress, IndexProgressEvent};
use crate::ipc::error::IpcError;
use crate::ipc::memory_maintenance::{should_forward_progress, PROGRESS_EVERY};
use crate::ipc::settings::ProjectWire;
use crate::ipc::state::IpcState;

/// List the registered projects (name → path) from `projects.toml`.
///
/// The picker shows these as quick-open entries. The list is read from the
/// live in-memory config (kept in sync with disk by `create_project` /
/// `remove_project`).
#[tauri::command]
pub async fn list_projects(state: State<'_, IpcState>) -> Result<Vec<ProjectWire>, IpcError> {
    let config = state.project.config.lock().await;
    Ok(config
        .projects
        .iter()
        .map(|e| ProjectWire {
            name: e.name.clone(),
            path: e.path.clone(),
        })
        .collect())
}

/// Whether the app started without a resolved project and is waiting for the
/// user to pick one. The frontend checks this on mount (before
/// `get_startup_error`) and shows the project picker instead of the normal UI
/// when it returns `true`.
#[tauri::command]
pub async fn get_needs_project(state: State<'_, IpcState>) -> Result<bool, IpcError> {
    Ok(state.runtime.needs_project)
}

/// Create a project at `path`: scaffold `.coding/` + the root `agent.md`
/// template (idempotent — a directory that is already a project is left
/// as-is, except a missing `agent.md` and missing shipped skill files are
/// re-scaffolded, write-if-missing),
/// eagerly seed the on-disk stores (create `memory.db` with its schema and,
/// when the codegraph is enabled, run the first source index) so the first
/// session after switching starts with semantic + source search ready,
/// register it in `projects.toml` under `name` (defaulting to the directory's
/// basename), reload the config, and return the project path.
///
/// The caller then calls `switch_project(path)` to restart into it. Creating
/// does not switch on its own so a failed switch can't leave a half-opened
/// project.
///
/// When the codegraph is enabled, the first source index streams
/// [`IndexProgressEvent`]s on `codegraph://index-progress` (`started` →
/// `progress` → `done`/`failed`) so the picker's overlay can show a bar +
/// "N/M files indexed" counter. No 1s gate here (that gate is startup-only):
/// a NEW project always has the full first index ahead of it, so feedback
/// starts immediately.
#[tauri::command]
pub async fn create_project(
    app: AppHandle,
    state: State<'_, IpcState>,
    path: String,
    name: Option<String>,
) -> Result<String, IpcError> {
    let path = path.trim();
    if path.is_empty() {
        return Err("project path must not be empty".into());
    }
    let dir = Path::new(path);
    if !dir.is_dir() {
        return Err(format!(
            "project path '{path}' is not a directory (create the folder first, then pick it)"
        )
        .into());
    }

    // Scaffold .coding/ + agent.md. Idempotent: a directory that is already a
    // project is left untouched (a missing agent.md is still written, and
    // missing shipped skill files are re-seeded — user-modified files are
    // never overwritten).
    let project =
        Project::init(dir).map_err(|e| format!("failed to initialize project at '{path}': {e}"))?;

    // Read the codegraph gate first, in a scope so the config lock is dropped
    // before the blocking seed work below (no guard is held across its await).
    let codegraph_enabled = {
        let config = state.project.config.lock().await;
        config.general.general.codegraph
    };

    // Eagerly seed the project's on-disk stores (memory.db + the first
    // codegraph index) on the blocking pool — the index parses the whole
    // tree. Best-effort: a seed failure logs and continues (app startup
    // re-creates/re-indexes anyway; seeding must never fail creation).
    //
    // The pass streams IndexProgressEvent ticks on codegraph://index-progress
    // for the picker's overlay (bar + "N/M files indexed"). Emission is
    // best-effort on both sides: a dead window drops the events, and a seed
    // failure still logs + continues as before.
    let seed_project = project.clone();
    emit_index_progress(&app, &IndexProgressEvent::Started);
    let progress_app = app.clone();
    match tokio::task::spawn_blocking(move || {
        let progress = move |done: usize, total: usize| {
            if should_forward_progress(done, total, PROGRESS_EVERY) {
                emit_index_progress(&progress_app, &IndexProgressEvent::Progress { done, total });
            }
        };
        seed_project.seed_stores(codegraph_enabled, Some(&progress))
    })
    .await
    {
        Ok(Ok(())) => {
            emit_index_progress(
                &app,
                &IndexProgressEvent::Done {
                    summary: "project stores seeded — first index complete".to_string(),
                },
            );
        }
        Ok(Err(e)) => {
            emit_index_progress(
                &app,
                &IndexProgressEvent::Failed {
                    error: e.to_string(),
                },
            );
            eprintln!("warning: project store seeding failed: {e}")
        }
        Err(e) => {
            emit_index_progress(
                &app,
                &IndexProgressEvent::Failed {
                    error: format!("project store seeding task failed: {e}"),
                },
            );
            eprintln!("warning: project store seeding task failed: {e}")
        }
    }

    // Default the name to the directory's basename when not supplied.
    let name = name
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .or_else(|| {
            dir.file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| "project".to_string());

    // Register in projects.toml + reload the live config so the picker list
    // updates immediately.
    {
        let mut config = state.project.config.lock().await;
        config.projects.add(&name, path);
        let projects_toml = global_config_dir().join("projects.toml");
        config
            .projects
            .save(&projects_toml)
            .map_err(|e| format!("failed to write projects.toml: {e}"))?;
    }

    Ok(path.to_string())
}

/// Switch to a project: write its path to the pending-project marker, then
/// restart the app. On restart `build_brain` consumes the marker and opens
/// the project.
///
/// `path` must be an existing Mnemo project directory: it must exist AND its
/// `.coding/` must be present (see [`validate_switch_target`]). The create
/// flow scaffolds `.coding/` before switching, so this never blocks that
/// path — while a registered project whose `.coding/` was deleted (or never
/// scaffolded) now fails loudly at the dialog instead of silently re-showing
/// the picker after the restart (backlog 16e4a7f8: the user pressed Open on
/// an existing project, the app restarted, and the switch dialog came back
/// with no explanation).
#[tauri::command]
pub async fn switch_project(app: AppHandle, path: String) -> Result<(), IpcError> {
    let path = path.trim();
    if path.is_empty() {
        return Err("project path must not be empty".into());
    }
    let dir = Path::new(path);
    validate_switch_target(dir)?;
    write_pending_project(dir).map_err(|e| format!("failed to stage project switch: {e}"))?;
    // Restart the current process. The marker carries the selection across
    // the restart (argv is reused, so it can't be passed as a flag). This call
    // does not return — the process is terminated and relaunched.
    tauri::process::restart(&app.env());
}

/// Validate a [`switch_project`] target: an existing directory that is a
/// Mnemo project (its `.coding/` directory is present).
///
/// The `.coding/` requirement closes the silent-picker failure: without it a
/// registered project missing `.coding/` passed validation, the app
/// restarted, and the reload's marker arm fell through to
/// `BrainOutcome::NeedsProject` — the switch dialog reappeared with no
/// explanation (backlog 16e4a7f8).
fn validate_switch_target(dir: &Path) -> Result<(), IpcError> {
    if !dir.is_dir() {
        return Err(format!("project path '{}' is not a directory", dir.display()).into());
    }
    if !dir.join(Project::CODING_DIR_NAME).is_dir() {
        return Err(format!(
            "project path '{}' is not a Mnemo project: its .coding/ directory is missing",
            dir.display()
        )
        .into());
    }
    Ok(())
}

/// Remove a project from the registry (does not delete any files on disk).
/// Reloads the config so the picker list updates immediately.
#[tauri::command]
pub async fn remove_project(state: State<'_, IpcState>, name: String) -> Result<(), IpcError> {
    let name = name.trim();
    if name.is_empty() {
        return Err("project name must not be empty".into());
    }
    {
        let mut config = state.project.config.lock().await;
        if !config.projects.remove(name) {
            return Err(format!("no registered project named '{name}'").into());
        }
        let projects_toml = global_config_dir().join("projects.toml");
        config
            .projects
            .save(&projects_toml)
            .map_err(|e| format!("failed to write projects.toml: {e}"))?;
    }
    Ok(())
}

/// Open a native folder-picker and return the chosen directory path, or `None`
/// if the user cancelled. Used by the picker's "create new project" flow.
#[tauri::command]
pub async fn pick_directory(app: AppHandle) -> Result<Option<String>, IpcError> {
    // The dialog pumps the OS message loop (blocking), so run it on a
    // blocking thread to avoid stalling the async runtime.
    let picked = tokio::task::spawn_blocking(move || app.dialog().file().blocking_pick_folder())
        .await
        .map_err(|e| format!("dialog failed: {e}"))?;

    let Some(file_path) = picked else {
        return Ok(None);
    };
    let path = file_path
        .into_path()
        .map_err(|e| format!("invalid picked path: {e}"))?;
    Ok(Some(path.to_string_lossy().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test for backlog 16e4a7f8: `switch_project` used to accept
    /// any existing directory — a target whose `.coding/` directory is missing
    /// passed the old `is_dir()`-only check, the app restarted, and the
    /// reload's marker arm silently fell through to the project picker
    /// instead of opening the project. The switch must fail loudly at the
    /// dialog instead.
    #[test]
    fn switch_target_requires_coding_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");

        let bare = tmp.path().join("bare");
        std::fs::create_dir_all(&bare).expect("create bare dir");
        assert!(
            validate_switch_target(&bare).is_err(),
            "a directory without .coding/ must be rejected"
        );

        let project = tmp.path().join("proj");
        std::fs::create_dir_all(project.join(".coding")).expect("create project dir");
        assert!(
            validate_switch_target(&project).is_ok(),
            "a directory with .coding/ must be accepted"
        );
    }
}
