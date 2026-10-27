// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]

//! Mnemo2's typed Tauri command surface.
//!
//! One [`LeadRuntime`] lives in [`Mnemo2State`]. Commands never hold the
//! runtime lock across an engine await; they clone the `Arc` and release the
//! slot first. A single forwarder subscribes to the runtime's event bus and
//! emits every [`LeadEvent`] on [`LEAD_EVENT_CHANNEL`].
//!
//! ## Event contract
//!
//! Each payload is:
//!
//! ```json
//! { "generation": 1, "event": { "agent": 7, "event": { "kind": "text_delta", "text": "…" } } }
//! ```
//!
//! `generation` identifies the live session. A model switch resumes a fresh
//! session and advances the generation; the frontend MUST ignore envelopes
//! whose generation differs from its current snapshot.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use serde::Deserialize;
use tauri_plugin_notification::NotificationExt;
use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{broadcast, Mutex};

use mnemo::{Approval, UserAnswer};

use crate::mnemo2::phase3::Finding;
use crate::mnemo2::project::{self, ProjectPreferences, ProjectRecord};
use crate::mnemo2::runtime::{LeadRuntime, LeadRuntimeOptions, LeadSnapshot, LEAD_EVENT_CHANNEL};
use crate::mnemo2::team::{
    TeamMessage, TeamRuntime, TeamSnapshot, WorkerRecord, TEAM_EVENT_CHANNEL,
};
use crate::mnemo2::workbench::{self, DiffSnapshot};

/// The shared Mnemo2 app state: one live lead and its event forwarder.
#[derive(Default)]
pub struct Mnemo2State {
    runtime: Mutex<Option<Arc<LeadRuntime>>>,
    forwarder: Mutex<Option<JoinHandle<()>>>,
    team_forwarder: Mutex<Option<JoinHandle<()>>>,
    marker_root: Mutex<Option<PathBuf>>,
    preferences: Mutex<Option<(PathBuf, ProjectPreferences)>>,
    notified: Mutex<HashSet<String>>,
}

impl Mnemo2State {
    /// Stop the real runtimes and release only this process's project claim.
    pub async fn shutdown(&self) {
        if let Some(forwarder) = self.forwarder.lock().await.take() {
            forwarder.abort();
        }
        if let Some(forwarder) = self.team_forwarder.lock().await.take() {
            forwarder.abort();
        }
        let marker_root = self.marker_root.lock().await.take();
        let runtime = self.runtime.lock().await.take();
        if let Some(runtime) = runtime {
            let root = runtime.workspace_root().to_path_buf();
            runtime.shutdown().await;
            project::detach(&root);
        } else if let Some(root) = marker_root {
            project::detach(&root);
        }
        self.preferences.lock().await.take();
    }
}

impl Mnemo2State {
    /// Flush the last successfully saved project preferences on native exit.
    pub async fn save_cached_preferences(&self, app_data: &std::path::Path) -> Result<(), String> {
        let Some((root, preferences)) = self.preferences.lock().await.clone() else {
            return Ok(());
        };
        project::save_preferences(app_data, &root, &preferences)
    }
}

/// The UI's approval decision on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    /// Approve the action.
    Approve,
    /// Deny the action.
    Deny,
    /// Deny this and all subsequent approvals in the turn.
    DenyAll,
}

impl From<ApprovalDecision> for Approval {
    fn from(value: ApprovalDecision) -> Self {
        match value {
            ApprovalDecision::Approve => Approval::Approve,
            ApprovalDecision::Deny => Approval::Deny,
            ApprovalDecision::DenyAll => Approval::DenyAll,
        }
    }
}

/// The UI's answer to an `ask_user` question.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AnswerInput {
    /// A clicked option.
    Choice {
        /// The 0-indexed option position.
        index: usize,
    },
    /// A freeform answer.
    Freeform {
        /// The text the user typed.
        text: String,
    },
}

impl From<AnswerInput> for UserAnswer {
    fn from(value: AnswerInput) -> Self {
        match value {
            AnswerInput::Choice { index } => UserAnswer::Choice { index },
            AnswerInput::Freeform { text } => UserAnswer::Freeform { text },
        }
    }
}

fn notification_for(worker: &WorkerRecord) -> Option<(&'static str, String)> {
    use crate::mnemo2::team::WorkerStatus;

    match worker.status {
        WorkerStatus::Completed | WorkerStatus::InReview => Some((
            "Worker ready for review",
            format!("{} is ready for review.", worker.name),
        )),
        WorkerStatus::NeedsYou => Some((
            "Worker needs you",
            format!("{} is waiting for input.", worker.name),
        )),
        WorkerStatus::Failed => Some((
            "Worker failed",
            format!("{} needs attention.", worker.name),
        )),
        WorkerStatus::Conflict => Some((
            "Merge conflict",
            format!("{} has a merge conflict.", worker.name),
        )),
        _ => None,
    }
}

fn notification_key(worker: &WorkerRecord) -> String {
    format!(
        "{}:{}:{:?}:{}",
        worker.id, worker.generation, worker.status, worker.last_activity_ms
    )
}

async fn notify_worker_event(
    app: &AppHandle,
    team: &Arc<TeamRuntime>,
    event: &crate::mnemo2::team::TeamEvent,
) {
    if !matches!(
        &event.event.event,
        mnemo::SerializableAgentEvent::Finished { .. }
            | mnemo::SerializableAgentEvent::Error { retrying: false, .. }
            | mnemo::SerializableAgentEvent::ApprovalRequest { .. }
            | mnemo::SerializableAgentEvent::UserQuestion { .. }
            | mnemo::SerializableAgentEvent::HostToolCall { .. }
    ) {
        return;
    }
    let Ok(worker) = team.worker(&event.worker_id).await else {
        return;
    };
    let Some((title, body)) = notification_for(&worker) else {
        return;
    };
    let Some(state) = app.try_state::<Mnemo2State>() else {
        return;
    };
    let mut notified = state.notified.lock().await;
    if !notified.insert(notification_key(&worker)) {
        return;
    }
    if notified.len() > 1024 {
        notified.clear();
    }
    drop(notified);
    if app
        .get_window("main")
        .is_some_and(|window| window.is_focused().unwrap_or(false))
    {
        return;
    }
    if let Err(error) = app.notification().builder().title(title).body(body).show() {
        eprintln!("mnemo2: could not show worker notification: {error}");
    }
}

async fn current(state: &Mnemo2State) -> Result<Arc<LeadRuntime>, String> {
    state
        .runtime
        .lock()
        .await
        .clone()
        .ok_or_else(|| "Mnemo2 has no open lead; call lead_open first".to_string())
}

async fn current_team(state: &Mnemo2State) -> Result<Arc<TeamRuntime>, String> {
    current(state).await?.team_handle().await
}

/// List the real projects from `projects.toml`.
#[tauri::command]
pub async fn project_list(
    app: AppHandle,
    config_dir: Option<String>,
) -> Result<Vec<ProjectRecord>, String> {
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|err| format!("could not resolve the Mnemo2 app data dir: {err}"))?;
    project::list_with_state(config_dir.map(PathBuf::from), &app_data).await
}

/// Create a new project directory on a non-main local Git branch.
#[tauri::command]
pub async fn project_create(
    config_dir: Option<String>,
    parent: String,
    name: String,
) -> Result<ProjectRecord, String> {
    project::create(config_dir.map(PathBuf::from), std::path::Path::new(&parent), name).await
}

/// Register a real folder, deriving its display name and canonical path.
#[tauri::command]
pub async fn project_add(
    config_dir: Option<String>,
    path: String,
) -> Result<ProjectRecord, String> {
    project::add(config_dir.map(PathBuf::from), std::path::Path::new(&path))
}

/// Open the native folder picker and return the chosen folder, if any.
#[tauri::command]
pub async fn project_pick_folder(app: AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;

    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().pick_folder(move |folder| {
        let picked = folder
            .and_then(|path| path.into_path().ok())
            .map(|path| path.to_string_lossy().to_string());
        let _ = tx.send(picked);
    });
    rx.await
        .map_err(|_| "the folder picker closed unexpectedly".to_string())
}

/// Check and claim a project for this process, warning about another live one.
#[tauri::command]
pub async fn project_attach(
    state: State<'_, Mnemo2State>,
    workspace_root: String,
    allow_conflict: Option<bool>,
) -> Result<project::InstanceAttachment, String> {
    let root = project::canonical_root(std::path::Path::new(&workspace_root))?;
    let attachment = project::attach(&root, allow_conflict.unwrap_or(false))?;
    if attachment.conflict_pid.is_none() {
        *state.marker_root.lock().await = Some(root);
    }
    Ok(attachment)
}

/// Remove only confirmed inactive, clean worktrees from a registered project.
#[tauri::command]
pub async fn project_cleanup(
    app: AppHandle,
    state: State<'_, Mnemo2State>,
    workspace_root: String,
    config_dir: Option<String>,
) -> Result<crate::mnemo2::team::CleanupReport, String> {
    let root = project::canonical_root(std::path::Path::new(&workspace_root))?;
    let registered = project::list(config_dir.map(PathBuf::from))?.into_iter().any(|record| {
        project::canonical_root(std::path::Path::new(&record.path))
            .ok()
            .as_deref()
            == Some(root.as_path())
    });
    if !registered {
        return Err("cleanup is allowed only for a registered project".to_string());
    }
    if let Some(runtime) = state.runtime.lock().await.clone() {
        if project::canonical_root(runtime.workspace_root())? == root {
            return Err("close this project before cleaning its worktrees".to_string());
        }
    }
    let attachment = project::attach(&root, false)?;
    if let Some(pid) = attachment.conflict_pid {
        return Err(format!("another Mnemo2 instance (pid {pid}) is using this project"));
    }
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|err| format!("could not resolve the Mnemo2 app data dir: {err}"))?;
    let state_dir = project::state_dir(&app_data, &root)?;
    let result = crate::mnemo2::team::cleanup_workspace(&root, &state_dir).await;
    project::detach(&root);
    *state.marker_root.lock().await = None;
    result
}

/// Open (or replace) the lead conversation on a project.
#[tauri::command]
pub async fn lead_open(
    app: AppHandle,
    state: State<'_, Mnemo2State>,
    browser: State<'_, crate::mnemo2::browser::BrowserController>,
    workspace_root: String,
    config_dir: Option<String>,
    model: Option<String>,
    allow_instance_conflict: Option<bool>,
) -> Result<LeadSnapshot, String> {
    let root = project::canonical_root(std::path::Path::new(&workspace_root))?;
    let attachment = project::attach(&root, allow_instance_conflict.unwrap_or(false))?;
    if let Some(pid) = attachment.conflict_pid {
        return Err(format!(
            "project is already in use by process {pid}; confirm opening another instance"
        ));
    }
    if let Some(forwarder) = state.forwarder.lock().await.take() {
        forwarder.abort();
    }
    if let Some(forwarder) = state.team_forwarder.lock().await.take() {
        forwarder.abort();
    }
    if let Some(old) = state.runtime.lock().await.take() {
        let old_root = old.workspace_root().to_path_buf();
        old.shutdown().await;
        if old_root != root {
            project::detach(&old_root);
            let _ = browser.close().await;
        }
    }
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|err| format!("could not resolve the Mnemo2 app data dir: {err}"))?;
    let project_state = project::state_dir(&app_data, &root)?;
    let preferences = match project::preferences(&app_data, &root) {
        Ok(preferences) => preferences,
        Err(error) => {
            project::detach(&root);
            *state.marker_root.lock().await = None;
            return Err(error);
        }
    };
    let runtime = Arc::new(match LeadRuntime::open(LeadRuntimeOptions {
        workspace_root: root.clone(),
        config_dir: config_dir.map(PathBuf::from),
        state_dir: project_state,
        model,
    })
    .await
    {
        Ok(runtime) => runtime,
        Err(error) => {
            project::detach(&root);
            *state.marker_root.lock().await = None;
            return Err(error);
        }
    });
    let snapshot = match runtime.snapshot().await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            runtime.shutdown().await;
            project::detach(&root);
            *state.marker_root.lock().await = None;
            return Err(error);
        }
    };
    *state.marker_root.lock().await = Some(root.clone());
    *state.preferences.lock().await = Some((root.clone(), preferences));
    state.notified.lock().await.clear();
    let mut events = runtime.subscribe();
    let app_handle = app.clone();
    let forwarder = tauri::async_runtime::spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => {
                    let _ = app_handle.emit(LEAD_EVENT_CHANNEL, &event);
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    let team = match runtime.team_handle().await {
        Ok(team) => team,
        Err(error) => {
            runtime.shutdown().await;
            project::detach(&root);
            *state.marker_root.lock().await = None;
            return Err(error);
        }
    };
    let mut team_events = team.subscribe();
    let team_app = app.clone();
    let team_forwarder = tauri::async_runtime::spawn(async move {
        loop {
            match team_events.recv().await {
                Ok(event) => {
                    let _ = team_app.emit(TEAM_EVENT_CHANNEL, &event);
                    notify_worker_event(&team_app, &team, &event).await;
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    *state.runtime.lock().await = Some(runtime);
    *state.forwarder.lock().await = Some(forwarder);
    *state.team_forwarder.lock().await = Some(team_forwarder);
    Ok(snapshot)
}

/// Read the current lead snapshot (transcript, model, pending interaction).
#[tauri::command]
pub async fn lead_snapshot(state: State<'_, Mnemo2State>) -> Result<LeadSnapshot, String> {
    current(&state).await?.snapshot().await
}

/// Send a user prompt to the real lead.
#[tauri::command]
pub async fn lead_prompt(state: State<'_, Mnemo2State>, text: String) -> Result<(), String> {
    current(&state).await?.send_prompt(text).await
}

/// Cancel the current lead generation without closing the session.
#[tauri::command]
pub async fn lead_interrupt(state: State<'_, Mnemo2State>) -> Result<(), String> {
    current(&state).await?.interrupt().await
}

/// Change the lead model while idle, resuming the transcript on the new model.
#[tauri::command]
pub async fn lead_set_model(
    state: State<'_, Mnemo2State>,
    spec: String,
) -> Result<LeadSnapshot, String> {
    current(&state).await?.set_model(spec).await
}

/// Resolve a pending approval request.
#[tauri::command]
pub async fn lead_approve(
    state: State<'_, Mnemo2State>,
    request_id: String,
    decision: ApprovalDecision,
) -> Result<(), String> {
    current(&state)
        .await?
        .approve(request_id, decision.into())
        .await
}

/// Resolve a pending `ask_user` question.
#[tauri::command]
pub async fn lead_answer(
    state: State<'_, Mnemo2State>,
    question_id: String,
    answer: AnswerInput,
) -> Result<(), String> {
    current(&state)
        .await?
        .answer(question_id, answer.into())
        .await
}

/// Read the real team snapshot.
#[tauri::command]
pub async fn team_snapshot(state: State<'_, Mnemo2State>) -> Result<TeamSnapshot, String> {
    Ok(current_team(&state).await?.snapshot().await)
}

/// Delegate a real task to a worker.
#[tauri::command]
pub async fn team_dispatch(
    state: State<'_, Mnemo2State>,
    brief: String,
    model: Option<String>,
    worker_id: Option<String>,
) -> Result<WorkerRecord, String> {
    current_team(&state)
        .await?
        .dispatch_with_id(brief, model, worker_id)
        .await
}

/// Send a lead message to a worker.
#[tauri::command]
pub async fn team_message(
    state: State<'_, Mnemo2State>,
    worker_id: String,
    text: String,
) -> Result<WorkerRecord, String> {
    current_team(&state).await?.message(&worker_id, text).await
}

/// Pause a worker's current turn.
#[tauri::command]
pub async fn team_pause(
    state: State<'_, Mnemo2State>,
    worker_id: String,
) -> Result<WorkerRecord, String> {
    current_team(&state).await?.pause(&worker_id).await
}

/// Interrupt a worker's current turn.
#[tauri::command]
pub async fn team_interrupt(
    state: State<'_, Mnemo2State>,
    worker_id: String,
) -> Result<WorkerRecord, String> {
    current_team(&state).await?.interrupt(&worker_id).await
}

/// Stop a worker and keep its branch/worktree for audit.
#[tauri::command]
pub async fn team_stop(
    state: State<'_, Mnemo2State>,
    worker_id: String,
) -> Result<WorkerRecord, String> {
    current_team(&state).await?.stop(&worker_id).await
}

/// Resume a stopped/interrupted worker from its transcript.
#[tauri::command]
pub async fn team_resume(
    state: State<'_, Mnemo2State>,
    worker_id: String,
) -> Result<WorkerRecord, String> {
    current_team(&state).await?.resume(&worker_id).await
}

/// Compact an idle worker, resuming a blocked/stopped session when needed.
#[tauri::command]
pub async fn team_compact(
    state: State<'_, Mnemo2State>,
    worker_id: String,
) -> Result<WorkerRecord, String> {
    current_team(&state).await?.compact(&worker_id).await
}

/// Persist the lead's review and complete finding list.
#[tauri::command]
pub async fn team_review(
    state: State<'_, Mnemo2State>,
    worker_id: String,
    summary: String,
    findings: Vec<Finding>,
) -> Result<WorkerRecord, String> {
    current_team(&state)
        .await?
        .review(&worker_id, summary, findings)
        .await
}

/// Record a real check on the current worker tip.
#[tauri::command]
pub async fn team_record_check(
    state: State<'_, Mnemo2State>,
    worker_id: String,
    name: String,
    status: String,
    detail: Option<String>,
) -> Result<WorkerRecord, String> {
    current_team(&state)
        .await?
        .record_check(&worker_id, name, status, detail)
        .await
}

/// Close a finding with fixed evidence or a wontfix justification.
#[tauri::command]
pub async fn team_resolve_finding(
    state: State<'_, Mnemo2State>,
    worker_id: String,
    finding_id: String,
    status: String,
    evidence: Option<String>,
    justification: Option<String>,
) -> Result<WorkerRecord, String> {
    current_team(&state)
        .await?
        .resolve_finding(&worker_id, &finding_id, &status, evidence, justification)
        .await
}

/// Apply a constrained tiny review fix on the worker branch.
#[tauri::command]
pub async fn team_review_fix(
    state: State<'_, Mnemo2State>,
    worker_id: String,
    finding_id: String,
    path: String,
    old_string: String,
    new_string: String,
) -> Result<WorkerRecord, String> {
    current_team(&state)
        .await?
        .review_fix(&worker_id, &finding_id, &path, &old_string, &new_string)
        .await
}

/// Route a larger finding back to the tracked worker.
#[tauri::command]
pub async fn team_route_fix(
    state: State<'_, Mnemo2State>,
    worker_id: String,
    finding_id: String,
) -> Result<WorkerRecord, String> {
    current_team(&state)
        .await?
        .route_fix(&worker_id, &finding_id)
        .await
}

/// Merge one reviewed worker through the serialized local integration queue.
#[tauri::command]
pub async fn team_merge(
    state: State<'_, Mnemo2State>,
    worker_id: String,
) -> Result<WorkerRecord, String> {
    current_team(&state).await?.merge(&worker_id).await
}

/// Revert a worker's earlier local merge before landing.
#[tauri::command]
pub async fn team_revert_merge(
    state: State<'_, Mnemo2State>,
    worker_id: String,
) -> Result<WorkerRecord, String> {
    current_team(&state).await?.revert_merge(&worker_id).await
}

/// Stop all live workers without discarding their branches or records.
#[tauri::command]
pub async fn team_stop_all(state: State<'_, Mnemo2State>) -> Result<(), String> {
    current_team(&state).await?.stop_all().await
}

/// Persist the project's live-worker cap.
#[tauri::command]
pub async fn team_set_slots(
    state: State<'_, Mnemo2State>,
    slots: usize,
) -> Result<TeamSnapshot, String> {
    current_team(&state).await?.set_slots(slots).await
}

/// Read the current project's durable workbench preferences.
#[tauri::command]
pub async fn workbench_preferences(
    app: AppHandle,
    state: State<'_, Mnemo2State>,
) -> Result<ProjectPreferences, String> {
    let runtime = current(&state).await?;
    let root = runtime.workspace_root().to_path_buf();
    if let Some((cached_root, preferences)) = state.preferences.lock().await.as_ref() {
        if cached_root == &root {
            return Ok(preferences.clone());
        }
    }
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|err| format!("could not resolve the Mnemo2 app data dir: {err}"))?;
    project::preferences(&app_data, &root)
}

/// Save the current project's validated workbench preferences.
#[tauri::command]
pub async fn workbench_preferences_set(
    app: AppHandle,
    state: State<'_, Mnemo2State>,
    workspace_root: String,
    preferences: ProjectPreferences,
) -> Result<(), String> {
    let runtime = current(&state).await?;
    let requested = project::canonical_root(std::path::Path::new(&workspace_root))?;
    let current_root = project::canonical_root(runtime.workspace_root())?;
    if requested != current_root {
        return Err("the project changed before preferences could be saved".to_string());
    }
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|err| format!("could not resolve the Mnemo2 app data dir: {err}"))?;
    project::save_preferences(&app_data, &requested, &preferences)?;
    *state.preferences.lock().await = Some((requested, preferences));
    Ok(())
}

/// Read the selected worker's actual Git delta.
#[tauri::command]
pub async fn workbench_diff(
    state: State<'_, Mnemo2State>,
    worker_id: String,
) -> Result<DiffSnapshot, String> {
    let runtime = current(&state).await?;
    let worker = current_team(&state).await?.worker(&worker_id).await?;
    workbench::worker_diff(runtime.workspace_root(), &worker).await
}

/// Read a genuine worker screenshot for the native preview pane.
#[tauri::command]
pub async fn workbench_screenshot(
    state: State<'_, Mnemo2State>,
    worker_id: String,
    path: String,
) -> Result<String, String> {
    let runtime = current(&state).await?;
    let worker = current_team(&state).await?.worker(&worker_id).await?;
    workbench::worker_screenshot(runtime.workspace_root(), &worker, &path).await
}

/// Read worker-to-lead messages.
#[tauri::command]
pub async fn team_messages(state: State<'_, Mnemo2State>) -> Result<Vec<TeamMessage>, String> {
    Ok(current_team(&state).await?.messages().await)
}

/// Close the lead session and stop its event forwarder.
#[tauri::command]
pub async fn lead_close(
    state: State<'_, Mnemo2State>,
    browser: State<'_, crate::mnemo2::browser::BrowserController>,
) -> Result<(), String> {
    state.shutdown().await;
    browser.close().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use mnemo::headless::SessionEvent;
    use mnemo::runtime::channels::SerializableAgentEvent;

    #[test]
    fn approval_decisions_parse_from_the_wire() {
        for (wire, expected) in [
            ("\"approve\"", ApprovalDecision::Approve),
            ("\"deny\"", ApprovalDecision::Deny),
            ("\"deny_all\"", ApprovalDecision::DenyAll),
        ] {
            let parsed: ApprovalDecision = serde_json::from_str(wire).expect("decision");
            assert_eq!(parsed, expected);
        }
    }

    #[test]
    fn question_answers_parse_from_the_wire() {
        let choice: AnswerInput =
            serde_json::from_str(r#"{"kind":"choice","index":2}"#).expect("choice");
        assert_eq!(choice, AnswerInput::Choice { index: 2 });
        let freeform: AnswerInput =
            serde_json::from_str(r#"{"kind":"freeform","text":"later"}"#).expect("freeform");
        assert_eq!(
            freeform,
            AnswerInput::Freeform {
                text: "later".into()
            }
        );
    }

    #[test]
    fn lead_event_envelope_carries_generation_and_inner_event() {
        let event = crate::mnemo2::runtime::LeadEvent {
            generation: 7,
            event: SessionEvent {
                agent: 3,
                event: SerializableAgentEvent::TextDelta {
                    text: "hello".into(),
                },
                rates: None,
            },
        };
        let value = serde_json::to_value(event).expect("serialize");
        assert_eq!(value["generation"], 7);
        assert_eq!(value["event"]["agent"], 3);
        assert_eq!(value["event"]["event"]["kind"], "text_delta");
        assert_eq!(value["event"]["event"]["text"], "hello");
        assert_eq!(LEAD_EVENT_CHANNEL, "mnemo2://lead-event");
    }

    #[test]
    fn notifications_target_real_worker_transitions_and_deduplicate_by_activity() {
        let mut worker: WorkerRecord = serde_json::from_value(serde_json::json!({
            "id": "worker1",
            "name": "Worker one",
            "brief": "Task",
            "model": null,
            "model_id": "test-model",
            "branch": "wt/agent-worker1",
            "worktree": ".worktrees/agent-worker1",
            "status": "completed",
            "generation": 1,
            "transcript": { "messages": [] },
            "pending": null,
            "used_tokens": 0,
            "max_tokens": 1000,
            "checks": [],
            "error": null,
            "last_activity_ms": 10,
        }))
        .expect("worker record");
        assert_eq!(
            notification_for(&worker).map(|notification| notification.0),
            Some("Worker ready for review")
        );
        let first = notification_key(&worker);
        assert_eq!(first, notification_key(&worker));
        worker.last_activity_ms += 1;
        assert_ne!(first, notification_key(&worker));
        worker.status = crate::mnemo2::team::WorkerStatus::Cancelled;
        assert!(notification_for(&worker).is_none());
        worker.status = crate::mnemo2::team::WorkerStatus::Failed;
        assert_eq!(
            notification_for(&worker).map(|notification| notification.0),
            Some("Worker failed")
        );
    }
}
