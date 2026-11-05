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
use std::time::Instant;

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use tauri_plugin_notification::NotificationExt;
use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{broadcast, Mutex};

use mnemo::{Approval, UserAnswer};

use crate::mnemo2::mcp_server::{McpServer, OpenFailure};
use crate::mnemo2::phase3::{Finding, ReviewEvidence};
use crate::mnemo2::project::{self, ProjectPreferences, ProjectRecord};
use crate::mnemo2::indexing::{IndexingSnapshot, INDEXING_CHANNEL};
use crate::mnemo2::run_file::RunFileHandle;
use crate::mnemo2::runtime::{LeadRuntime, LeadRuntimeOptions, LeadSnapshot, LEAD_EVENT_CHANNEL};
use crate::mnemo2::team::{
    BoardSignal, BoardSnapshot, ChangeSource, DeletionPhase, DeletionProgress, MessageSender,
    TeamMessage, TeamRuntime, TeamSnapshot, TeamDeleteOutcome, WorkerDeletion, WorkerRecord,
    DELETION_EVENT_CHANNEL, TEAM_EVENT_CHANNEL,
};
use crate::mnemo2::workbench::{self, DiffSnapshot};

/// The Tauri event emitted (no payload) whenever the plan board may have changed.
pub const BOARD_CHANGED_CHANNEL: &str = "mnemo2://board-changed";

/// The shared Mnemo2 app state: one live lead and its event forwarder.
#[derive(Default)]
pub struct Mnemo2State {
    runtime: Mutex<Option<Arc<LeadRuntime>>>,
    forwarder: Mutex<Option<JoinHandle<()>>>,
    team_forwarder: Mutex<Option<JoinHandle<()>>>,
    board_forwarder: Mutex<Option<JoinHandle<()>>>,
    indexing_forwarder: Mutex<Option<JoinHandle<()>>>,
    marker_root: Mutex<Option<PathBuf>>,
    preferences: Mutex<Option<(PathBuf, ProjectPreferences)>>,
    notified: Mutex<HashSet<String>>,
    /// The canonical `--project` from the command line, until the frontend
    /// takes it.
    startup_project: std::sync::Mutex<Option<String>>,
    /// This process's run file, once the local endpoint is up.
    run_file: std::sync::Mutex<Option<Arc<RunFileHandle>>>,
    /// The local endpoint for an external lead; dropping it stops listening.
    endpoint: std::sync::Mutex<Option<McpServer>>,
    /// Why the UI could not open a project Claude Code asked for: the
    /// canonical root, the reason and when it was reported, until the next
    /// request or open.
    open_failure: std::sync::Mutex<Option<(PathBuf, String, Instant)>>,
    /// True while a project open is in flight — `lead_open` has begun and the
    /// runtime (with its engine) is not installed yet. The Memory tab reads it
    /// to say "waiting for the project engine to start" instead of showing an
    /// empty list: every memory command goes through the runtime, so before
    /// the install there is nothing to read from yet.
    opening: AtomicBool,
}

impl Mnemo2State {
    /// The reported failure to open the project at `root`.
    pub(crate) fn open_failure(&self, root: &std::path::Path) -> Option<OpenFailure> {
        let failure = self.open_failure.lock().unwrap_or_else(|e| e.into_inner());
        failure
            .as_ref()
            .filter(|(failed, _, _)| failed.as_path() == root)
            .map(|(_, reason, reported)| OpenFailure {
                reason: reason.clone(),
                age: reported.elapsed(),
            })
    }

    /// Remember that the UI could not open `root` (canonical). `open` is the
    /// canonical root of the project open right now: a report that arrives
    /// after that project opened is late and would only mislead the helper.
    pub(crate) fn record_open_failure(
        &self,
        root: PathBuf,
        reason: String,
        open: Option<&std::path::Path>,
    ) {
        if open == Some(root.as_path()) {
            return;
        }
        *self.open_failure.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((root, reason, Instant::now()));
    }

    /// Forget a reported open failure (a new request, or a project opened).
    pub(crate) fn clear_open_failure(&self) {
        *self.open_failure.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// Mark a project open as in flight. The returned guard clears the flag on
    /// EVERY exit path — success, an early `return Err`, or a `?`.
    pub(crate) fn begin_opening(&self) -> OpeningGuard<'_> {
        self.opening.store(true, Ordering::SeqCst);
        OpeningGuard { state: self }
    }

    /// True while a project open is in flight (its engine is still starting).
    pub(crate) fn is_opening(&self) -> bool {
        self.opening.load(Ordering::SeqCst)
    }
}

/// Clears [`Mnemo2State`]'s `opening` flag when a project open finishes —
/// however it exits, so a failed open never leaves the tab waiting forever.
pub(crate) struct OpeningGuard<'a> {
    state: &'a Mnemo2State,
}

impl Drop for OpeningGuard<'_> {
    fn drop(&mut self) {
        self.state.opening.store(false, Ordering::SeqCst);
    }
}

/// The external lead's state on the open project, as the UI reads it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExternalLeadStatus {
    /// Whether an external lead (Claude Code over MCP) leads the open project.
    pub attached: bool,
    /// The open project's root while attached or idle.
    pub root: Option<String>,
    /// When it attached, in Unix epoch milliseconds.
    pub since_ms: Option<u64>,
    /// When it was released for silence (it has not left), in Unix epoch
    /// milliseconds; `None` while attached or gone.
    pub idle_since_ms: Option<u64>,
}

impl Mnemo2State {
    /// Remember the `--project` the app was started with.
    pub fn set_startup_project(&self, root: Option<String>) {
        *self.startup_project.lock().unwrap_or_else(|e| e.into_inner()) = root;
    }

    /// Take the `--project` once; later calls get `None`.
    pub fn take_startup_project(&self) -> Option<String> {
        self.startup_project
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }

    /// Keep the running endpoint and this process's run file.
    pub fn install_endpoint(&self, server: McpServer, run_file: Arc<RunFileHandle>) {
        *self.endpoint.lock().unwrap_or_else(|e| e.into_inner()) = Some(server);
        *self.run_file.lock().unwrap_or_else(|e| e.into_inner()) = Some(run_file);
    }

    /// Stop the endpoint and remove the run file (app exit).
    pub fn shutdown_endpoint(&self) {
        self.endpoint.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(run_file) = self.run_file.lock().unwrap_or_else(|e| e.into_inner()).take() {
            run_file.remove();
        }
    }

    /// Record the open project (`None` once closed) in the run file.
    fn sync_run_file(&self, project: Option<&std::path::Path>) {
        if project.is_some() {
            self.clear_open_failure();
        }
        let run_file = self.run_file.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(run_file) = run_file {
            if let Err(error) = run_file.set_project(project) {
                eprintln!("mnemo2: could not update the run file: {error}");
            }
        }
    }

    /// The open project's lead, if one is open.
    pub async fn open_lead(&self) -> Option<Arc<LeadRuntime>> {
        self.runtime.lock().await.clone()
    }

    /// The team of the open project, if one is open.
    pub async fn open_team(&self) -> Option<Arc<TeamRuntime>> {
        self.open_lead().await?.team_handle().await.ok()
    }

    /// Hand the open project back to the in-app lead if an external lead
    /// leads it (its unread notices go to the in-app lead), telling the UI.
    /// Run before the project closes or is replaced.
    async fn release_external_lead(&self, app: &AppHandle) {
        let Some(lead) = self.open_lead().await else {
            return;
        };
        if lead.release_external_lead().await {
            let _ = app.emit(
                crate::mnemo2::mcp_server::EXTERNAL_LEAD_CHANNEL,
                serde_json::json!({ "attached": false, "root": null }),
            );
        }
    }
}

impl Mnemo2State {
    /// Install an open runtime for shutdown integration tests.
    #[cfg(test)]
    pub(crate) fn install_runtime_for_test(&mut self, runtime: Arc<LeadRuntime>) {
        *self.runtime.get_mut() = Some(runtime);
    }

    /// Stop the real runtimes and release only this process's project claim.
    pub async fn shutdown(&self) {
        if let Some(forwarder) = self.forwarder.lock().await.take() {
            forwarder.abort();
        }
        if let Some(forwarder) = self.team_forwarder.lock().await.take() {
            forwarder.abort();
        }
        if let Some(forwarder) = self.board_forwarder.lock().await.take() {
            forwarder.abort();
        }
        if let Some(forwarder) = self.indexing_forwarder.lock().await.take() {
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
        self.sync_run_file(None);
    }
}

impl Mnemo2State {
    /// Flush the last successfully saved project preferences on native exit.
    pub async fn save_cached_preferences(&self, owner: &str) -> Result<(), String> {
        let Some((root, preferences)) = self.preferences.lock().await.clone() else {
            return Ok(());
        };
        project::save_preferences(&root, owner, &preferences)
    }
}

/// This machine's owner id, which stamps and verifies project state folders.
pub(crate) fn state_owner(app: &AppHandle) -> Result<String, String> {
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|err| format!("could not resolve the Mnemo2 app data dir: {err}"))?;
    project::machine_owner_id(&app_data)
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
            match worker.error.as_deref() {
                Some(reason) => format!("{}: {reason}", worker.name),
                None => format!("{} is blocked; open it to see why.", worker.name),
            },
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
    project::list_with_state(config_dir.map(PathBuf::from), &state_owner(&app)?).await
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
    // Cleanup acts on the worker ledger, so only a verified ledger may steer it.
    let state_dir = match project::state_owner_checked(&root, &state_owner(&app)?) {
        Ok(dir) => dir,
        Err(error) => {
            project::detach(&root);
            *state.marker_root.lock().await = None;
            return Err(error);
        }
    };
    let result = crate::mnemo2::team::cleanup_workspace(
        &root,
        &state_dir,
        &crate::mnemo2::dispatch_routing::dispatch_routing_log_path(),
    )
    .await;
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
    // From here the engine is starting: until the runtime is installed, memory
    // readers report "the project engine is still starting" (the tab keeps its
    // spinner and retries) instead of "no open project".
    let _opening = state.begin_opening();
    if let Some(forwarder) = state.forwarder.lock().await.take() {
        forwarder.abort();
    }
    if let Some(forwarder) = state.team_forwarder.lock().await.take() {
        forwarder.abort();
    }
    if let Some(forwarder) = state.board_forwarder.lock().await.take() {
        forwarder.abort();
    }
    if let Some(forwarder) = state.indexing_forwarder.lock().await.take() {
        forwarder.abort();
    }
    state.release_external_lead(&app).await;
    if let Some(old) = state.runtime.lock().await.take() {
        let old_root = old.workspace_root().to_path_buf();
        old.shutdown().await;
        state.sync_run_file(None);
        if old_root != root {
            project::detach(&old_root);
            let _ = browser.close().await;
        }
    }
    let opened = state_owner(&app).and_then(|owner| {
        let dir = project::open_state_dir(&root, &owner, true)?
            .ok_or_else(|| "the project state folder could not be created".to_string())?;
        Ok((owner, dir))
    });
    let (owner, project_state) = match opened {
        Ok(opened) => opened,
        Err(error) => {
            project::detach(&root);
            *state.marker_root.lock().await = None;
            return Err(error);
        }
    };
    let preferences = match project::preferences(&root, &owner) {
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
    let mut board_events = team.subscribe_board();
    let board_app = app.clone();
    let board_forwarder = tauri::async_runtime::spawn(async move {
        loop {
            match board_events.recv().await {
                Ok(BoardSignal::Notice { .. }) => {}
                Ok(BoardSignal::Changed) | Err(broadcast::error::RecvError::Lagged(_)) => {
                    let _ = board_app.emit(BOARD_CHANGED_CHANNEL, ());
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    let mut indexing_events = runtime.indexing().subscribe();
    let indexing_app = app.clone();
    let indexing_forwarder = tauri::async_runtime::spawn(async move {
        loop {
            match indexing_events.recv().await {
                Ok(snapshot) => {
                    let _ = indexing_app.emit(INDEXING_CHANNEL, &snapshot);
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    // The Laya check (plan 8106337a) starts with the project: unforced, it
    // skips silently when routing is off, Laya is down or the record's
    // fingerprint still matches. It must come after the indexing forwarder
    // so the pass is visible, and an error never fails the open.
    if let Err(error) = runtime.start_laya_check(false).await {
        eprintln!("mnemo2: the Laya check could not start: {error}");
    }
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
    *state.board_forwarder.lock().await = Some(board_forwarder);
    *state.indexing_forwarder.lock().await = Some(indexing_forwarder);
    state.sync_run_file(Some(&root));
    Ok(snapshot)
}

/// The project named by `--project` on the command line (its canonical root),
/// once; every later call gets `None`. The frontend opens it through its
/// normal project-open flow.
#[tauri::command]
pub fn mnemo2_startup_project(state: State<'_, Mnemo2State>) -> Option<String> {
    state.take_startup_project()
}

/// The UI could not open a project Claude Code asked for (`--project` or an
/// external open request); the local endpoint reports `reason` to the
/// waiting helper instead of letting it time out.
#[tauri::command]
pub async fn external_open_failed(
    state: State<'_, Mnemo2State>,
    root: String,
    reason: String,
) -> Result<(), String> {
    let root = project::canonical_root(std::path::Path::new(&root))?;
    let open = state.open_lead().await.map(|lead| {
        let open = lead.workspace_root();
        project::canonical_root(open).unwrap_or_else(|_| open.to_path_buf())
    });
    state.record_open_failure(root, reason, open.as_deref());
    Ok(())
}

/// Whether an external lead (Claude Code over MCP) leads the open project.
/// Changes arrive on `mnemo2://external-lead`.
#[tauri::command]
pub async fn external_lead_status(
    state: State<'_, Mnemo2State>,
) -> Result<ExternalLeadStatus, String> {
    let Some(lead) = state.open_lead().await else {
        return Ok(ExternalLeadStatus {
            attached: false,
            root: None,
            since_ms: None,
            idle_since_ms: None,
        });
    };
    let team = lead.team_handle().await?;
    let since_ms = team.external_lead_since_ms();
    let idle_since_ms = team.external_lead_idle_since_ms();
    let named = since_ms.is_some() || idle_since_ms.is_some();
    Ok(ExternalLeadStatus {
        attached: since_ms.is_some(),
        root: named.then(|| lead.workspace_root().to_string_lossy().into_owned()),
        since_ms,
        idle_since_ms,
    })
}

/// The open project's startup indexing status (code index + memory). Later
/// changes arrive on [`INDEXING_CHANNEL`].
#[tauri::command]
pub async fn indexing_snapshot(state: State<'_, Mnemo2State>) -> Result<IndexingSnapshot, String> {
    Ok(current(&state).await?.indexing().snapshot())
}

/// Read the current lead snapshot (transcript, model, pending interaction).
#[tauri::command]
pub async fn lead_snapshot(state: State<'_, Mnemo2State>) -> Result<LeadSnapshot, String> {
    current(&state).await?.snapshot().await
}

/// Send a user prompt (with optional image attachments) to the real
/// lead. A busy lead receives it as a queued steer injected at the next
/// safe turn boundary instead of an error.
#[tauri::command]
pub async fn lead_prompt(
    state: State<'_, Mnemo2State>,
    text: String,
    images: Option<Vec<String>>,
) -> Result<(), String> {
    current(&state)
        .await?
        .send_prompt(text, images.unwrap_or_default())
        .await
}

/// Cancel the current lead generation without closing the session.
#[tauri::command]
pub async fn lead_interrupt(state: State<'_, Mnemo2State>) -> Result<(), String> {
    current(&state).await?.interrupt().await
}

/// Compact the lead's context window between turns without closing the
/// session. The outcome arrives as `compact_started` / `compacted` events
/// on the lead event channel.
#[tauri::command]
pub async fn lead_compact(state: State<'_, Mnemo2State>) -> Result<(), String> {
    current(&state).await?.compact().await
}

/// Change the lead model while idle, resuming the transcript on the new model.
#[tauri::command]
pub async fn lead_set_model(
    state: State<'_, Mnemo2State>,
    spec: String,
    reasoning_effort: Option<String>,
) -> Result<LeadSnapshot, String> {
    current(&state)
        .await?
        .set_model(spec, reasoning_effort)
        .await
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

/// Read the plan board: every plan with the workers on each step.
#[tauri::command]
pub async fn board_snapshot(state: State<'_, Mnemo2State>) -> Result<BoardSnapshot, String> {
    current_team(&state).await?.board_snapshot().await
}

/// One board plan's markdown, for the Plans tab's expanded details.
#[tauri::command]
pub async fn board_plan_document(
    state: State<'_, Mnemo2State>,
    plan_id: String,
) -> Result<String, String> {
    current_team(&state).await?.plan_document(&plan_id)
}

/// Delegate a real task to a worker on a board plan step (1-based).
#[tauri::command]
// Each parameter is one wire field of the JS invoke — the command mirrors the IPC payload.
#[allow(clippy::too_many_arguments)]
pub async fn team_dispatch(
    state: State<'_, Mnemo2State>,
    brief: String,
    model: Option<String>,
    reasoning_effort: Option<String>,
    worker_id: Option<String>,
    review_risk: Option<String>,
    plan_id: String,
    step: usize,
) -> Result<WorkerRecord, String> {
    current_team(&state)
        .await?
        .dispatch_on_plan(
            brief,
            model,
            reasoning_effort,
            worker_id,
            review_risk,
            // The Control Room form has no read-back switch; the lead's
            // `team_dispatch` tool is the read-back surface.
            false,
            &plan_id,
            step,
        )
        .await
        .map(|(worker, _)| worker)
}

/// Dispatch a fresh read-only independent audit of one worker's current tip.
#[tauri::command]
pub async fn team_audit_dispatch(
    state: State<'_, Mnemo2State>,
    worker_id: String,
    reviewer_id: String,
    tier: Option<String>,
    model: Option<String>,
    reasoning_effort: Option<String>,
    brief: Option<String>,
) -> Result<WorkerRecord, String> {
    current_team(&state)
        .await?
        .audit_dispatch_with_tier(
            &worker_id,
            &reviewer_id,
            tier.as_deref(),
            model,
            reasoning_effort,
            brief,
        )
        .await
        .map(|(worker, _reused)| worker)
}

/// Send a mission-control user message to a worker.
#[tauri::command]
pub async fn team_message(
    state: State<'_, Mnemo2State>,
    worker_id: String,
    text: String,
) -> Result<WorkerRecord, String> {
    current_team(&state)
        .await?
        .message(&worker_id, text, MessageSender::User)
        .await
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

/// Move a worker to another model and reasoning effort, keeping its
/// transcript; a working worker switches at its next call, an idle or
/// stopped one comes back idle on the new model.
#[tauri::command]
pub async fn team_set_model(
    state: State<'_, Mnemo2State>,
    worker_id: String,
    model: Option<String>,
    reasoning_effort: Option<String>,
) -> Result<WorkerRecord, String> {
    current_team(&state)
        .await?
        .change_model(&worker_id, model, reasoning_effort, ChangeSource::User)
        .await
}

/// Compact an idle worker, resuming a blocked/stopped session when needed.
#[tauri::command]
pub async fn team_compact(
    state: State<'_, Mnemo2State>,
    worker_id: String,
) -> Result<WorkerRecord, String> {
    current_team(&state).await?.compact(&worker_id).await
}

/// Persist the lead's review, complete finding list, and structured evidence.
#[tauri::command]
pub async fn team_review(
    state: State<'_, Mnemo2State>,
    worker_id: String,
    summary: String,
    findings: Vec<Finding>,
    evidence: ReviewEvidence,
) -> Result<WorkerRecord, String> {
    current_team(&state)
        .await?
        .review(&worker_id, summary, findings, evidence)
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

/// Report what deleting these workers would remove, changing nothing. Each
/// worker's finished preview is streamed on `mnemo2://worker-deletion` the
/// moment its checks are done, so the open sheet fills its rows in.
#[tauri::command]
pub async fn team_delete_preview(
    app: AppHandle,
    state: State<'_, Mnemo2State>,
    worker_ids: Vec<String>,
) -> Result<Vec<WorkerDeletion>, String> {
    current_team(&state)
        .await?
        .deletion_preview_streaming(&worker_ids, |preview| {
            let _ = app.emit(
                DELETION_EVENT_CHANNEL,
                DeletionProgress {
                    phase: DeletionPhase::Checking,
                    worker_id: preview.worker_id.clone(),
                    name: preview.name.clone(),
                    preview: Some(preview),
                    error: None,
                },
            );
        })
        .await
}

/// Delete inactive workers with their history, checkout, and branch. Every
/// worker that can be deleted is deleted even when some fail: the outcome
/// carries one per-worker result plus the team snapshot, and each worker's
/// result is streamed on `mnemo2://worker-deletion` the moment it is known.
#[tauri::command]
pub async fn team_delete(
    app: AppHandle,
    state: State<'_, Mnemo2State>,
    worker_ids: Vec<String>,
    discard_work: bool,
) -> Result<TeamDeleteOutcome, String> {
    current_team(&state)
        .await?
        .delete_workers_streaming(&worker_ids, discard_work, |result| {
            let _ = app.emit(
                DELETION_EVENT_CHANNEL,
                DeletionProgress {
                    phase: DeletionPhase::Deleting,
                    worker_id: result.worker_id.clone(),
                    name: result.name.clone(),
                    preview: None,
                    error: result.error.clone(),
                },
            );
        })
        .await
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
    project::preferences(&root, &state_owner(&app)?)
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
    project::save_preferences(&requested, &state_owner(&app)?, &preferences)?;
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
    app: AppHandle,
    state: State<'_, Mnemo2State>,
    browser: State<'_, crate::mnemo2::browser::BrowserController>,
) -> Result<(), String> {
    state.release_external_lead(&app).await;
    state.shutdown().await;
    browser.close().await
}

// ── Settings: endpoints and model tiers ────────────────────────────────────
//
// These work with or without an open lead. With one, the engine's shared
// config and the lead/team model catalog are refreshed after every save, so
// the next dispatch sees new endpoints, models, prices and tiers.

/// A `Setup` on the lead's config dir (attached to its engine when a lead is
/// open), plus the lead for post-save catalog refresh.
pub(crate) async fn settings_setup(
    state: &Mnemo2State,
) -> Result<(mnemo::headless::Setup, Option<Arc<LeadRuntime>>), String> {
    let runtime = state.runtime.lock().await.clone();
    match runtime {
        Some(runtime) => {
            let setup = mnemo::headless::Setup::open(Some(runtime.config_dir().to_path_buf()))
                .map_err(|err| err.to_string())?;
            let engine = runtime.engine_handle().await?;
            Ok((setup.for_engine(&engine), Some(runtime)))
        }
        None => Ok((
            mnemo::headless::Setup::open(None).map_err(|err| err.to_string())?,
            None,
        )),
    }
}

pub(crate) async fn settings_refresh(runtime: Option<Arc<LeadRuntime>>) -> Result<(), String> {
    match runtime {
        Some(runtime) => runtime.refresh_catalog().await,
        None => Ok(()),
    }
}

/// Every configured endpoint with its models, specs, prices and key presence.
#[tauri::command]
pub async fn settings_endpoints(
    state: State<'_, Mnemo2State>,
) -> Result<Vec<mnemo::headless::EndpointDetailDto>, String> {
    let (setup, _) = settings_setup(&state).await?;
    setup.endpoints_detail().map_err(|err| err.to_string())
}

/// Create or update one endpoint (rename via `previous_name`); returns the
/// refreshed endpoint list.
#[tauri::command]
pub async fn settings_upsert_endpoint(
    state: State<'_, Mnemo2State>,
    endpoint: mnemo::headless::EndpointDetailDto,
    previous_name: Option<String>,
) -> Result<Vec<mnemo::headless::EndpointDetailDto>, String> {
    let (setup, runtime) = settings_setup(&state).await?;
    setup
        .upsert_endpoint_detail(endpoint, previous_name)
        .await
        .map_err(|err| err.to_string())?;
    settings_refresh(runtime).await?;
    setup.endpoints_detail().map_err(|err| err.to_string())
}

/// Delete an endpoint (its key, tier entries and orphaned prices go too);
/// returns the refreshed endpoint list.
#[tauri::command]
pub async fn settings_remove_endpoint(
    state: State<'_, Mnemo2State>,
    name: String,
) -> Result<Vec<mnemo::headless::EndpointDetailDto>, String> {
    let (setup, runtime) = settings_setup(&state).await?;
    setup
        .remove_endpoint_applied(&name)
        .await
        .map_err(|err| err.to_string())?;
    settings_refresh(runtime).await?;
    setup.endpoints_detail().map_err(|err| err.to_string())
}

/// Store an endpoint's API key (write-only; the endpoint must exist).
#[tauri::command]
pub async fn settings_set_api_key(
    state: State<'_, Mnemo2State>,
    endpoint_name: String,
    api_key: String,
) -> Result<(), String> {
    let (setup, runtime) = settings_setup(&state).await?;
    setup
        .set_api_key_applied(&endpoint_name, mnemo::headless::Secret::new(api_key))
        .await
        .map_err(|err| err.to_string())?;
    settings_refresh(runtime).await
}

/// Probe an endpoint's `/models` with the given or saved credentials.
#[tauri::command]
pub async fn settings_test_endpoint(
    state: State<'_, Mnemo2State>,
    endpoint_name: String,
    base_url: Option<String>,
    api_key: Option<String>,
    kind: Option<String>,
) -> Result<mnemo::headless::EndpointTestDto, String> {
    let (setup, _) = settings_setup(&state).await?;
    Ok(setup
        .test_endpoint_detail(&endpoint_name, base_url, api_key, kind)
        .await)
}

/// List the models the provider serves (to pick from when adding models).
#[tauri::command]
pub async fn settings_list_provider_models(
    state: State<'_, Mnemo2State>,
    endpoint_name: String,
    base_url: Option<String>,
    api_key: Option<String>,
    kind: Option<String>,
) -> Result<Vec<mnemo::headless::ProviderModelDto>, String> {
    let (setup, _) = settings_setup(&state).await?;
    setup
        .list_provider_models(&endpoint_name, base_url, api_key, kind)
        .await
        .map_err(|err| err.to_string())
}

/// What still references an endpoint (tier entries, the default model).
#[tauri::command]
pub async fn settings_endpoint_usage(
    state: State<'_, Mnemo2State>,
    name: String,
) -> Result<Vec<String>, String> {
    let (setup, _) = settings_setup(&state).await?;
    setup.endpoint_usage(&name).map_err(|err| err.to_string())
}

/// The configured model tiers.
#[tauri::command]
pub async fn settings_tiers(
    state: State<'_, Mnemo2State>,
) -> Result<mnemo::headless::TiersDto, String> {
    let (setup, _) = settings_setup(&state).await?;
    setup.tiers().map_err(|err| err.to_string())
}

/// Validate and save the model tiers; returns what was stored.
#[tauri::command]
pub async fn settings_save_tiers(
    state: State<'_, Mnemo2State>,
    tiers: mnemo::headless::TiersDto,
) -> Result<mnemo::headless::TiersDto, String> {
    let (setup, runtime) = settings_setup(&state).await?;
    let saved = setup
        .set_tiers(tiers)
        .await
        .map_err(|err| err.to_string())?;
    settings_refresh(runtime).await?;
    Ok(saved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mnemo::headless::SessionEvent;
    use mnemo::runtime::channels::SerializableAgentEvent;

    #[test]
    fn the_startup_project_is_handed_over_once() {
        let state = Mnemo2State::default();
        assert_eq!(state.take_startup_project(), None);
        state.set_startup_project(Some("C:/work/app".into()));
        assert_eq!(state.take_startup_project().as_deref(), Some("C:/work/app"));
        assert_eq!(state.take_startup_project(), None, "a second take gets nothing");
    }

    #[test]
    fn the_run_file_follows_the_open_project() {
        let dir = tempfile::tempdir().expect("config dir");
        let state = Mnemo2State::default();
        // Without an endpoint there is nothing to update.
        state.sync_run_file(Some(std::path::Path::new("/ignored")));

        let handle = Arc::new(
            RunFileHandle::create(dir.path(), 1234, "tok".into()).expect("run file"),
        );
        let path = crate::mnemo2::run_file::run_path(dir.path(), std::process::id());
        *state.run_file.lock().expect("slot") = Some(handle);
        state.sync_run_file(Some(std::path::Path::new("/work/app")));
        assert_eq!(
            crate::mnemo2::run_file::read(&path).expect("read").project.as_deref(),
            Some("/work/app")
        );
        state.sync_run_file(None);
        assert!(crate::mnemo2::run_file::read(&path).expect("read").project.is_none());
        state.shutdown_endpoint();
        assert!(!path.exists(), "the file goes with the endpoint");
    }

    #[test]
    fn a_reported_open_failure_carries_its_age_and_is_cleared_by_a_new_attempt() {
        let state = Mnemo2State::default();
        let root = std::path::PathBuf::from("/work/app");
        assert_eq!(state.open_failure(&root), None);
        state.record_open_failure(root.clone(), "no engine".into(), None);
        let failure = state.open_failure(&root).expect("reported");
        assert_eq!(failure.reason, "no engine");
        assert!(failure.age < std::time::Duration::from_secs(5));
        assert_eq!(state.open_failure(std::path::Path::new("/work/other")), None);
        state.clear_open_failure();
        assert_eq!(state.open_failure(&root), None);
        // Opening a project forgets a failure too.
        state.record_open_failure(root.clone(), "no engine".into(), None);
        state.sync_run_file(Some(&root));
        assert_eq!(state.open_failure(&root), None);
    }

    #[test]
    fn a_late_failure_report_for_the_open_project_is_ignored() {
        let state = Mnemo2State::default();
        let root = std::path::PathBuf::from("/work/app");
        // The project opened while the UI was still reporting its failure.
        state.record_open_failure(root.clone(), "late".into(), Some(&root));
        assert_eq!(state.open_failure(&root), None);
        // Another open project does not excuse the report.
        let other = std::path::PathBuf::from("/work/other");
        state.record_open_failure(root.clone(), "busy".into(), Some(&other));
        assert_eq!(state.open_failure(&root).map(|failure| failure.reason).as_deref(), Some("busy"));
    }

    #[test]
    fn external_lead_status_has_the_documented_wire_shape() {
        let status = ExternalLeadStatus {
            attached: true,
            root: Some("/p".into()),
            since_ms: Some(5),
            idle_since_ms: None,
        };
        assert_eq!(
            serde_json::to_value(status).expect("encode"),
            serde_json::json!({ "attached": true, "root": "/p", "since_ms": 5, "idle_since_ms": null })
        );
        let idle = ExternalLeadStatus {
            attached: false,
            root: Some("/p".into()),
            since_ms: None,
            idle_since_ms: Some(9),
        };
        assert_eq!(
            serde_json::to_value(idle).expect("encode"),
            serde_json::json!({ "attached": false, "root": "/p", "since_ms": null, "idle_since_ms": 9 })
        );
        let gone = ExternalLeadStatus {
            attached: false,
            root: None,
            since_ms: None,
            idle_since_ms: None,
        };
        assert_eq!(
            serde_json::to_value(gone).expect("encode"),
            serde_json::json!({ "attached": false, "root": null, "since_ms": null, "idle_since_ms": null })
        );
    }

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

    /// Regression: a "needs you" notification said only "is waiting for
    /// input" — no question, no reason — so the user had nothing to act on.
    #[test]
    fn needs_you_notification_says_why() {
        let mut worker: WorkerRecord = serde_json::from_value(serde_json::json!({
            "id": "worker1",
            "name": "Worker one",
            "brief": "Task",
            "model": null,
            "model_id": null,
            "branch": "wt/agent-worker1",
            "worktree": ".worktrees/agent-worker1",
            "status": "needs_you",
            "generation": 1,
            "transcript": { "messages": [] },
            "pending": null,
            "used_tokens": 0,
            "max_tokens": 0,
            "checks": [],
            "error": "the worker session reached its request limit before finishing",
        }))
        .expect("worker record");
        let (title, body) = notification_for(&worker).expect("a needs-you notification");
        assert_eq!(title, "Worker needs you");
        assert!(body.contains("reached its request limit"), "{body}");
        assert!(!body.contains("waiting for input"), "{body}");
        worker.error = None;
        let (_, body) = notification_for(&worker).expect("a needs-you notification");
        assert!(!body.contains("waiting for input"), "{body}");
    }
}
