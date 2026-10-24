// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]

//! The Phase 2 Mnemo2 team runtime.
//!
//! One [`TeamRuntime`] shares the lead's [`Engine`] and owns every real worker
//! session: its worktree, branch, model, brief, transcript, status, pending
//! interaction, and event pump. It deliberately has no Tauri types so the
//! worker lifecycle is testable against a scripted LLM.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use mnemo::app::worktrees::{
    agent_short_id, current_branch, provision_agent_worktree, remove_agent_worktree,
};
use mnemo::headless::{
    AgentSession, Engine, EventReceiver, Guardrails, PendingInteraction, QuestionPolicy,
    SessionCommand, SessionEvent, SessionId, SessionLimits, SpawnOptions, ToolSelection,
    Transcript, TurnEndPolicy,
};
use mnemo::{PermissionPolicy, SerializableAgentEvent};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, Mutex};
use tokio::task::JoinHandle;

use crate::mnemo2::runtime::LeadRuntime;

/// The Tauri event channel emitted by the team runtime.
pub const TEAM_EVENT_CHANNEL: &str = "mnemo2://team-event";

/// The persisted worker ledger inside the team's state directory.
const TEAM_STATE_FILE: &str = "team.json";

/// How many transient team events the broadcast bus keeps for a slow consumer.
const EVENT_BUFFER: usize = 1024;

/// The executor session's per-turn cap (a runaway worker stops on its own).
const WORKER_MAX_TURNS: u64 = 64;

/// The executor session's wall-clock cap.
const WORKER_WALL_CLOCK_MS: u64 = 30 * 60 * 1000;

/// The default number of concurrently live worker sessions.
pub const DEFAULT_SLOTS: usize = 3;

/// The default delegation cap for one lead turn.
pub const DEFAULT_SPAWNS_PER_TURN: usize = 4;

/// The real lifecycle state of one worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerStatus {
    /// The record exists but its session is still being spawned.
    Dispatching,
    /// A turn is running.
    Working,
    /// The worker is blocked on a lead/user answer.
    NeedsYou,
    /// The session is live but idle, waiting for a message.
    Idle,
    /// The lead paused the worker.
    Paused,
    /// The worker's turn was interrupted (including by a host restart).
    Interrupted,
    /// The worker's turn completed.
    Completed,
    /// The lead stopped the worker.
    Stopped,
    /// The worker failed.
    Failed,
}

/// A real check result recorded on a worker (Phase 1 keeps this empty until
/// the verify phase runs real commands; the field is already persistent).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkerCheck {
    /// The check's display name.
    pub name: String,
    /// The check's outcome (`passed`, `failed`, `running`).
    pub status: String,
    /// Optional detail (command output or error).
    pub detail: Option<String>,
}

/// One worker's durable record. It is the UI's truth and the recovery ledger:
/// a restart preserves the brief, model, branch, worktree, transcript, and
/// last known status.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkerRecord {
    /// Stable worker id (the first 8 chars name the branch and worktree).
    pub id: String,
    /// The worker's display name.
    pub name: String,
    /// The lead's brief — the worker's initial prompt.
    pub brief: String,
    /// The selected `endpoint/model` spec, when one was chosen.
    pub model: Option<String>,
    /// The actual model id observed from the live session, when known.
    pub model_id: Option<String>,
    /// The worker's `wt/agent-*` branch.
    pub branch: String,
    /// The linked worktree checkout.
    pub worktree: PathBuf,
    /// The real lifecycle state.
    pub status: WorkerStatus,
    /// The session generation within this worker (resume advances it).
    pub generation: u64,
    /// The conversation so far.
    pub transcript: Transcript,
    /// The interaction the worker is blocked on, if any.
    pub pending: Option<PendingInteraction>,
    /// Approximate prompt-token usage.
    pub used_tokens: u32,
    /// The context window.
    pub max_tokens: u32,
    /// Real check results (empty until checks are wired).
    pub checks: Vec<WorkerCheck>,
    /// The last error, when the worker failed.
    pub error: Option<String>,
}

/// The team's public snapshot.
#[derive(Clone, Debug, Serialize)]
pub struct TeamSnapshot {
    /// Every worker, including completed/stopped ones.
    pub workers: Vec<WorkerRecord>,
    /// The configured live-session cap.
    pub slots: usize,
    /// The number of currently live worker sessions.
    pub active: usize,
}

/// One event forwarded from a worker session, tagged with its worker id.
#[derive(Clone, Debug, Serialize)]
pub struct TeamEvent {
    /// The worker that emitted the event.
    pub worker_id: String,
    /// The worker session generation that emitted it.
    pub generation: u64,
    /// The underlying headless event.
    pub event: SessionEvent,
}

/// Team startup options. The engine is the lead's shared handle.
#[derive(Clone)]
pub struct TeamRuntimeOptions {
    /// The shared headless engine.
    pub engine: Arc<Engine>,
    /// The selected project root (worker worktrees fork from its branch).
    pub workspace_root: PathBuf,
    /// Mnemo2's app-owned state directory (the `team.json` ledger).
    pub state_dir: PathBuf,
    /// The live-session cap.
    pub slots: usize,
    /// The per-lead-turn delegation cap.
    pub max_spawns_per_turn: usize,
}

/// The persisted team ledger.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct PersistedTeam {
    workers: Vec<WorkerRecord>,
}

/// The small, atomic-ish `team.json` store.
#[derive(Clone, Debug)]
struct TeamPersistence {
    path: PathBuf,
}

impl TeamPersistence {
    fn new(state_dir: &std::path::Path) -> Self {
        Self {
            path: state_dir.join(TEAM_STATE_FILE),
        }
    }

    fn load(&self) -> Option<PersistedTeam> {
        let bytes = std::fs::read(&self.path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    fn save(&self, team: &PersistedTeam) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("could not create the team state dir: {err}"))?;
        }
        let bytes = serde_json::to_vec_pretty(team)
            .map_err(|err| format!("could not encode the team ledger: {err}"))?;
        let temp = self.path.with_extension("json.tmp");
        std::fs::write(&temp, bytes)
            .map_err(|err| format!("could not write the team ledger: {err}"))?;
        if self.path.exists() {
            let _ = std::fs::remove_file(&self.path);
        }
        std::fs::rename(&temp, &self.path)
            .map_err(|err| format!("could not replace the team ledger: {err}"))
    }
}

struct LiveWorker {
    session: AgentSession,
    busy: Arc<AtomicBool>,
    pump: JoinHandle<()>,
}

impl LiveWorker {
    fn abort(&self) {
        self.pump.abort();
    }
}

/// The shared team runtime: one engine, many real worker sessions.
pub struct TeamRuntime {
    engine: Arc<Engine>,
    workspace_root: PathBuf,
    records: Arc<Mutex<Vec<WorkerRecord>>>,
    live: Mutex<HashMap<String, LiveWorker>>,
    events: broadcast::Sender<TeamEvent>,
    generation: Arc<AtomicU64>,
    next_worker: AtomicU64,
    spawns_this_turn: AtomicUsize,
    max_spawns_per_turn: usize,
    slots: usize,
    persistence: TeamPersistence,
}

impl TeamRuntime {
    /// Open the team ledger, recover interrupted workers, and prepare to spawn.
    pub async fn open(options: TeamRuntimeOptions) -> Result<Self, String> {
        let persistence = TeamPersistence::new(&options.state_dir);
        let mut workers = persistence.load().unwrap_or_default().workers;
        for worker in &mut workers {
            if matches!(
                worker.status,
                WorkerStatus::Dispatching | WorkerStatus::Working | WorkerStatus::NeedsYou
            ) {
                worker.status = if worker.pending.is_some() {
                    WorkerStatus::NeedsYou
                } else {
                    WorkerStatus::Interrupted
                };
            }
        }
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        let runtime = Self {
            engine: options.engine,
            workspace_root: options.workspace_root,
            records: Arc::new(Mutex::new(workers)),
            live: Mutex::new(HashMap::new()),
            events,
            generation: Arc::new(AtomicU64::new(0)),
            next_worker: AtomicU64::new(1),
            spawns_this_turn: AtomicUsize::new(0),
            max_spawns_per_turn: options.max_spawns_per_turn.max(1),
            slots: options.slots.max(1),
            persistence,
        };
        runtime.persist().await?;
        Ok(runtime)
    }

    /// Open the team on the lead's shared engine.
    pub async fn from_lead(lead: &LeadRuntime, state_dir: PathBuf) -> Result<Self, String> {
        Self::open(TeamRuntimeOptions {
            engine: lead.engine_handle().await?,
            workspace_root: lead.workspace_root().to_path_buf(),
            state_dir,
            slots: DEFAULT_SLOTS,
            max_spawns_per_turn: DEFAULT_SPAWNS_PER_TURN,
        })
        .await
    }

    /// Subscribe to every worker event.
    pub fn subscribe(&self) -> broadcast::Receiver<TeamEvent> {
        self.events.subscribe()
    }

    /// Reset the per-turn delegation budget (called when a lead turn starts).
    pub fn begin_lead_turn(&self) {
        self.spawns_this_turn.store(0, Ordering::SeqCst);
    }

    /// The current team state.
    pub async fn snapshot(&self) -> TeamSnapshot {
        let workers = self.records.lock().await.clone();
        let active = self.live.lock().await.len();
        TeamSnapshot {
            workers,
            slots: self.slots,
            active,
        }
    }

    /// One worker by id.
    pub async fn worker(&self, worker_id: &str) -> Result<WorkerRecord, String> {
        self.records
            .lock()
            .await
            .iter()
            .find(|worker| worker.id == worker_id)
            .cloned()
            .ok_or_else(|| format!("unknown worker '{worker_id}'"))
    }

    async fn update_worker<F>(&self, worker_id: &str, update: F)
    where
        F: FnOnce(&mut WorkerRecord),
    {
        let mut records = self.records.lock().await;
        if let Some(worker) = records.iter_mut().find(|worker| worker.id == worker_id) {
            update(worker);
        }
    }

    async fn persist(&self) -> Result<(), String> {
        save_workers(&self.records, &self.persistence).await
    }
    /// Delegate one real worker task. The brief is the worker's first prompt.
    pub async fn dispatch(
        &self,
        brief: String,
        model: Option<String>,
    ) -> Result<WorkerRecord, String> {
        let used = self.spawns_this_turn.fetch_add(1, Ordering::SeqCst);
        if used >= self.max_spawns_per_turn {
            self.spawns_this_turn.fetch_sub(1, Ordering::SeqCst);
            return Err(format!(
                "the lead already delegated {} worker(s) this turn",
                self.max_spawns_per_turn
            ));
        }
        let result = self.dispatch_inner(brief, model).await;
        if result.is_err() {
            self.spawns_this_turn.fetch_sub(1, Ordering::SeqCst);
        }
        result
    }

    async fn dispatch_inner(
        &self,
        brief: String,
        model: Option<String>,
    ) -> Result<WorkerRecord, String> {
        if brief.trim().is_empty() {
            return Err("the worker brief is empty".to_string());
        }
        let active = self.live.lock().await.len();
        if active >= self.slots {
            return Err(format!("all {} worker slots are busy", self.slots));
        }
        let base_ref = current_branch(&self.workspace_root)
            .map_err(|err| format!("workers need a git branch: {err}"))?;
        let seq = self.next_worker.fetch_add(1, Ordering::SeqCst);
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or(0);
        let id = format!("{seq:08x}{millis:012x}");
        let name = format!("Worker {}", agent_short_id(&id));
        let (worktree, branch) =
            provision_agent_worktree(self.workspace_root.clone(), id.clone(), base_ref).await?;
        let worker_worktree = worktree.clone();
        let record = WorkerRecord {
            id: id.clone(),
            name: name.clone(),
            brief: brief.clone(),
            model: model.clone(),
            model_id: None,
            branch,
            worktree,
            status: WorkerStatus::Dispatching,
            generation: 0,
            transcript: Transcript::default(),
            pending: None,
            used_tokens: 0,
            max_tokens: 0,
            checks: Vec::new(),
            error: None,
        };
        self.records.lock().await.push(record);
        if let Err(err) = self.persist().await {
            self.records.lock().await.retain(|worker| worker.id != id);
            let _ = remove_agent_worktree(self.workspace_root.clone(), id.clone()).await;
            return Err(err);
        }
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let session = match self
            .spawn_worker_session(name, model, worker_worktree, None)
            .await
        {
            Ok(session) => session,
            Err(err) => {
                self.fail_worker(&id, err.clone()).await;
                let _ = remove_agent_worktree(self.workspace_root.clone(), id.clone()).await;
                return Err(err);
            }
        };
        let session_id = session.id();
        let busy = Arc::new(AtomicBool::new(true));
        let receiver = session.events();
        let pump = tokio::spawn(pump_worker(
            id.clone(),
            generation,
            self.engine.clone(),
            session_id,
            receiver,
            self.events.clone(),
            busy.clone(),
            self.records.clone(),
            self.persistence.clone(),
            self.generation.clone(),
        ));
        if let Err(err) = session
            .send(SessionCommand::Prompt {
                text: brief,
                images: Vec::new(),
            })
            .await
        {
            pump.abort();
            self.fail_worker(&id, format!("could not send the brief: {err}"))
                .await;
            let _ = remove_agent_worktree(self.workspace_root.clone(), id.clone()).await;
            return Err(format!("could not send the brief: {err}"));
        }
        self.live.lock().await.insert(
            id.clone(),
            LiveWorker {
                session,
                busy,
                pump,
            },
        );
        self.update_worker(&id, |worker| {
            worker.status = WorkerStatus::Working;
            worker.generation = generation;
        })
        .await;
        if let Err(err) = self.persist().await {
            let _ = self.stop(&id).await;
            return Err(err);
        }
        self.worker(&id).await
    }

    async fn spawn_worker_session(
        &self,
        name: String,
        model: Option<String>,
        worktree: PathBuf,
        resume: Option<Transcript>,
    ) -> Result<AgentSession, String> {
        self.engine
            .spawn(SpawnOptions {
                name: Some(name),
                model,
                worktree: Some(worktree),
                permissions: Some(PermissionPolicy::OverridePermissions),
                policy: None,
                resume,
                limits: None,
                host_tools: Vec::new(),
                external_policy: None,
                guardrails: Some(executor_guardrails()),
            })
            .await
            .map_err(|err| format!("could not spawn the worker session: {err}"))
    }

    async fn fail_worker(&self, worker_id: &str, error: String) {
        self.update_worker(worker_id, |worker| {
            worker.status = WorkerStatus::Failed;
            worker.error = Some(error);
        })
        .await;
        let _ = self.persist().await;
    }

    /// Send the lead's message to a worker. A busy worker gets it as a steer;
    /// an idle worker starts a new turn with it.
    pub async fn message(&self, worker_id: &str, text: String) -> Result<WorkerRecord, String> {
        if text.trim().is_empty() {
            return Err("the worker message is empty".to_string());
        }
        let (busy, idle, sent) = {
            let live = self.live.lock().await;
            let live = live
                .get(worker_id)
                .ok_or_else(|| format!("worker '{worker_id}' is not running; resume it first"))?;
            let busy = live.busy.clone();
            let idle = !busy.load(Ordering::SeqCst);
            if idle {
                busy.store(true, Ordering::SeqCst);
            }
            let command = if idle {
                SessionCommand::Prompt {
                    text,
                    images: Vec::new(),
                }
            } else {
                SessionCommand::Steer { text }
            };
            let result = live.session.send(command).await;
            (busy, idle, result)
        };
        if let Err(err) = sent {
            if idle {
                busy.store(false, Ordering::SeqCst);
            }
            return Err(format!("could not message the worker: {err}"));
        }
        self.update_worker(worker_id, |worker| {
            worker.status = WorkerStatus::Working;
            worker.pending = None;
        })
        .await;
        self.persist().await?;
        self.worker(worker_id).await
    }

    /// Pause the worker's current turn without ending its session.
    pub async fn pause(&self, worker_id: &str) -> Result<WorkerRecord, String> {
        {
            let live = self.live.lock().await;
            let live = live
                .get(worker_id)
                .ok_or_else(|| format!("worker '{worker_id}' is not running"))?;
            live.session
                .send(SessionCommand::Interrupt)
                .await
                .map_err(|err| format!("could not pause the worker: {err}"))?;
        }
        self.update_worker(worker_id, |worker| {
            worker.status = WorkerStatus::Paused;
            worker.pending = None;
        })
        .await;
        self.persist().await?;
        self.worker(worker_id).await
    }

    /// Interrupt the worker's current turn but keep the session open.
    pub async fn interrupt(&self, worker_id: &str) -> Result<WorkerRecord, String> {
        {
            let live = self.live.lock().await;
            let live = live
                .get(worker_id)
                .ok_or_else(|| format!("worker '{worker_id}' is not running"))?;
            live.session
                .send(SessionCommand::Interrupt)
                .await
                .map_err(|err| format!("could not interrupt the worker: {err}"))?;
        }
        self.update_worker(worker_id, |worker| {
            worker.status = WorkerStatus::Interrupted;
        })
        .await;
        self.persist().await?;
        self.worker(worker_id).await
    }

    /// Stop and detach a worker. The worktree and `wt/agent-*` branch stay for
    /// audit; `resume` can reattach the saved transcript later.
    pub async fn stop(&self, worker_id: &str) -> Result<WorkerRecord, String> {
        self.worker(worker_id).await?;
        let live = self.live.lock().await.remove(worker_id);
        if let Some(live) = live {
            live.abort();
            let _ = live.session.send(SessionCommand::Interrupt).await;
        }
        self.update_worker(worker_id, |worker| {
            worker.status = WorkerStatus::Stopped;
            worker.pending = None;
        })
        .await;
        self.persist().await?;
        self.worker(worker_id).await
    }

    /// Reattach a stopped/interrupted worker from its saved transcript. The
    /// session comes back idle; the lead then sends it a message.
    pub async fn resume(&self, worker_id: &str) -> Result<WorkerRecord, String> {
        if self.live.lock().await.contains_key(worker_id) {
            return Err(format!("worker '{worker_id}' is already running"));
        }
        let record = self.worker(worker_id).await?;
        if !record.worktree.is_dir() {
            return Err(format!(
                "the worker's worktree is missing: {}",
                record.worktree.display()
            ));
        }
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let session = self
            .spawn_worker_session(
                record.name.clone(),
                record.model.clone(),
                record.worktree.clone(),
                Some(record.transcript.clone()),
            )
            .await?;
        let session_id = session.id();
        let busy = Arc::new(AtomicBool::new(false));
        let receiver = session.events();
        let pump = tokio::spawn(pump_worker(
            worker_id.to_string(),
            generation,
            self.engine.clone(),
            session_id,
            receiver,
            self.events.clone(),
            busy.clone(),
            self.records.clone(),
            self.persistence.clone(),
            self.generation.clone(),
        ));
        self.live.lock().await.insert(
            worker_id.to_string(),
            LiveWorker {
                session,
                busy,
                pump,
            },
        );
        self.update_worker(worker_id, |worker| {
            worker.status = WorkerStatus::Idle;
            worker.generation = generation;
            worker.pending = None;
            worker.error = None;
        })
        .await;
        self.persist().await?;
        self.worker(worker_id).await
    }

    /// Stop every live worker. The shared engine is owned by the lead and is
    /// not shut down here.
    pub async fn shutdown(&self) {
        let live = {
            let mut map = self.live.lock().await;
            std::mem::take(&mut *map)
        };
        for worker in live.into_values() {
            worker.abort();
            let _ = worker.session.send(SessionCommand::Interrupt).await;
        }
        let _ = self.persist().await;
    }
}

/// Forward one worker session's events, update its durable record, and refresh
/// the transcript when the turn ends.
async fn pump_worker(
    worker_id: String,
    generation: u64,
    engine: Arc<Engine>,
    session_id: SessionId,
    mut receiver: EventReceiver<SessionEvent>,
    events: broadcast::Sender<TeamEvent>,
    busy: Arc<AtomicBool>,
    records: Arc<Mutex<Vec<WorkerRecord>>>,
    persistence: TeamPersistence,
    active_generation: Arc<AtomicU64>,
) {
    while let Ok(event) = receiver.recv().await {
        let mut terminal = false;
        let mut changed = false;
        {
            let mut records = records.lock().await;
            if let Some(worker) = records.iter_mut().find(|worker| worker.id == worker_id) {
                match &event.event {
                    SerializableAgentEvent::Started => {
                        busy.store(true, Ordering::SeqCst);
                        worker.status = WorkerStatus::Working;
                        changed = true;
                    }
                    SerializableAgentEvent::ModelChanged { model, .. } => {
                        worker.model_id = Some(model.clone());
                        changed = true;
                    }
                    SerializableAgentEvent::Usage { prompt_tokens, .. } => {
                        worker.used_tokens = worker.used_tokens.max(*prompt_tokens);
                    }
                    SerializableAgentEvent::ContextUsage { used, max, .. } => {
                        worker.used_tokens = *used;
                        worker.max_tokens = *max;
                    }
                    SerializableAgentEvent::ApprovalRequest { tool_call_id, .. } => {
                        worker.pending = Some(PendingInteraction::Approval {
                            request_id: tool_call_id.clone(),
                        });
                        worker.status = WorkerStatus::NeedsYou;
                        changed = true;
                    }
                    SerializableAgentEvent::UserQuestion { question_id, .. } => {
                        worker.pending = Some(PendingInteraction::Question {
                            question_id: question_id.clone(),
                        });
                        worker.status = WorkerStatus::NeedsYou;
                        changed = true;
                    }
                    SerializableAgentEvent::HostToolCall { call_id, .. } => {
                        worker.pending = Some(PendingInteraction::HostToolCall {
                            call_id: call_id.clone(),
                        });
                        worker.status = WorkerStatus::NeedsYou;
                        changed = true;
                    }
                    SerializableAgentEvent::Finished { .. } | SerializableAgentEvent::Exited => {
                        busy.store(false, Ordering::SeqCst);
                        worker.pending = None;
                        worker.status = WorkerStatus::Completed;
                        terminal = true;
                    }
                    SerializableAgentEvent::Parked { .. } => {
                        busy.store(false, Ordering::SeqCst);
                        // A normal turn end parks too (`NoWorkExpected`); it
                        // must not downgrade a completed/paused/stopped worker
                        // to "needs you".
                        if !matches!(
                            worker.status,
                            WorkerStatus::Completed
                                | WorkerStatus::Interrupted
                                | WorkerStatus::Paused
                                | WorkerStatus::Stopped
                        ) {
                            worker.status = WorkerStatus::NeedsYou;
                        }
                        terminal = true;
                    }
                    SerializableAgentEvent::Error { error, retrying } if !retrying => {
                        busy.store(false, Ordering::SeqCst);
                        worker.status = WorkerStatus::Failed;
                        worker.error = Some(error.clone());
                        terminal = true;
                    }
                    _ => {}
                }
            }
        }
        let _ = events.send(TeamEvent {
            worker_id: worker_id.clone(),
            generation,
            event,
        });
        if terminal || changed {
            let _ = save_workers(&records, &persistence).await;
        }
        if terminal {
            let engine = engine.clone();
            let records = records.clone();
            let persistence = persistence.clone();
            let active_generation = active_generation.clone();
            let worker_id = worker_id.clone();
            tokio::spawn(async move {
                if active_generation.load(Ordering::SeqCst) != generation {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                let handle = engine.session(&session_id).await;
                let Some(handle) = handle else {
                    return;
                };
                let Ok(snapshot) = handle.snapshot().await else {
                    return;
                };
                {
                    let mut records = records.lock().await;
                    if let Some(worker) = records.iter_mut().find(|worker| worker.id == worker_id) {
                        worker.transcript = snapshot.transcript;
                        worker.used_tokens = snapshot.used_tokens;
                        worker.max_tokens = snapshot.max_tokens;
                        worker.pending = snapshot.pending;
                    }
                }
                let _ = save_workers(&records, &persistence).await;
            });
        }
    }
}

async fn save_workers(
    records: &Arc<Mutex<Vec<WorkerRecord>>>,
    persistence: &TeamPersistence,
) -> Result<(), String> {
    let workers = records.lock().await.clone();
    persistence.save(&PersistedTeam { workers })
}

/// The worker's hard session limits.
fn executor_limits() -> SessionLimits {
    SessionLimits {
        max_turns: Some(WORKER_MAX_TURNS),
        wall_clock_ms: Some(WORKER_WALL_CLOCK_MS),
        ..SessionLimits::default()
    }
}

/// The executor surface: a real coding agent that cannot plan, finish, spawn
/// more agents, or ask the user — those are the lead's jobs.
fn executor_guardrails() -> Guardrails {
    let mut tools = ToolSelection::free_default();
    tools.exclude = vec![
        "create_plan".to_string(),
        "update_plan".to_string(),
        "finish".to_string(),
        "spawn_agent".to_string(),
        "ask_user".to_string(),
    ];
    Guardrails {
        prompt: None,
        tools,
        route: vec!["free".to_string()],
        limits: Some(executor_limits()),
        gate_rules: Vec::new(),
        question_policy: QuestionPolicy::Deny,
        turn_end: TurnEndPolicy::Stop,
    }
}
