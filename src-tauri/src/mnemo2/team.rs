// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]

//! The Mnemo2 team runtime: tracked workers, review, integration, and recovery.
//!
//! One [`TeamRuntime`] shares the lead's [`Engine`] and owns every real worker
//! session: its worktree, branch, model, brief, transcript, status, pending
//! interaction, and event pump. It deliberately has no Tauri types so the
//! worker lifecycle is testable against a scripted LLM.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use mnemo::headless::{ConstitutionMode, PromptBlock, PromptSpec};
use mnemo::app::worktrees::{
    agent_branch, agent_short_id, current_branch, provision_agent_worktree, remove_agent_worktree,
};
use mnemo::headless::{
    AgentSession, Engine, EventReceiver, Guardrails, HostToolSpec, PendingInteraction,
    QuestionPolicy, SessionCommand, SessionEvent, SessionId, SessionLimits, SpawnOptions,
    ToolSelection, Transcript, TurnEndPolicy,
};
use mnemo::tool::{SafetyLevel, ToolResult};
use mnemo::{PermissionPolicy, SerializableAgentEvent};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{broadcast, Mutex};
use tokio::task::JoinHandle;

use crate::mnemo2::phase3::{self, Finding, MergeRecord, ReviewRecord};
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

/// Default cumulative prompt + completion token budget for one worker.
pub const DEFAULT_WORKER_TOKEN_BUDGET: u32 = 200_000;

/// The default number of concurrently live worker sessions.
pub const DEFAULT_SLOTS: usize = 3;

/// The default delegation cap for one lead turn.
pub const DEFAULT_SPAWNS_PER_TURN: usize = 4;

fn default_token_budget() -> u32 {
    DEFAULT_WORKER_TOKEN_BUDGET
}

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
    /// The lead is reviewing an exact worker tip.
    InReview,
    /// A finding is being fixed by the tracked worker.
    Fixing,
    /// The serialized merge is executing.
    Merging,
    /// The worker branch merged into the local lead branch.
    Merged,
    /// A conflict stopped integration and routed the task back for review.
    Conflict,
    /// Its inactive checkout was cleaned; the retained branch is review-only.
    Cancelled,
}

/// A real check result bound to the worker commit on which it ran.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkerCheck {
    /// The check's display name or command.
    pub name: String,
    /// The check's outcome (`passed` or `failed`).
    pub status: String,
    /// Optional detail (command output or error).
    pub detail: Option<String>,
    /// Exact worker `HEAD`; old ledgers default to an intentionally stale value.
    #[serde(default)]
    pub tip: String,
}

/// One worker's durable record. It is the UI's truth and the recovery ledger.
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
    /// Latest context-window usage reported by the session.
    pub used_tokens: u32,
    /// The context window.
    pub max_tokens: u32,
    /// Maximum cumulative prompt + completion tokens before the worker pauses.
    #[serde(default = "default_token_budget")]
    pub token_budget: u32,
    /// Cumulative prompt + completion tokens across every worker session.
    #[serde(default)]
    pub total_tokens: u64,
    /// Cumulative prompt tokens from real usage events.
    #[serde(default)]
    pub prompt_tokens: u64,
    /// Cumulative completion tokens from real usage events.
    #[serde(default)]
    pub completion_tokens: u64,
    /// Cached prompt tokens (a subset of `prompt_tokens`).
    #[serde(default)]
    pub cached_tokens: u64,
    /// Completed provider requests reported by usage events.
    #[serde(default)]
    pub requests: u64,
    /// Estimated cost in millionths of a dollar when pricing is configured.
    #[serde(default)]
    pub cost_micros: Option<u64>,
    /// Worker creation time in Unix epoch milliseconds.
    #[serde(default)]
    pub created_at_ms: u128,
    /// Wall-clock time across all worker sessions in milliseconds.
    #[serde(default)]
    pub elapsed_ms: u128,
    /// Completed worker turns.
    #[serde(default)]
    pub turns: u32,
    /// Last observed activity in Unix epoch milliseconds.
    #[serde(default)]
    pub last_activity_ms: u128,
    /// Real exact-tip check results.
    pub checks: Vec<WorkerCheck>,
    /// Durable lead findings.
    #[serde(default)]
    pub findings: Vec<Finding>,
    /// Review of the current worker tip, invalidated by every fix commit.
    #[serde(default)]
    pub review: Option<ReviewRecord>,
    /// Finding currently routed to the tracked fixer.
    #[serde(default)]
    pub fixer_finding_id: Option<String>,
    /// Successful local merge and optional revert.
    #[serde(default)]
    pub merge: Option<MergeRecord>,
    /// The last error, when the worker failed or conflicted.
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

/// A worker-to-lead message routed through the `message_parent` host tool.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TeamMessage {
    /// The worker that sent it.
    pub worker_id: String,
    /// The message text.
    pub text: String,
    /// Unix epoch milliseconds.
    pub at_ms: u128,
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
    /// Explicit cap override; `None` restores the saved cap or the default.
    pub slots: Option<usize>,
    /// The per-lead-turn delegation cap.
    pub max_spawns_per_turn: usize,
    /// Configured per-model prices used to estimate real worker cost.
    pub pricing: Vec<mnemo::config::PricingEntry>,
}

/// The persisted team ledger.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct PersistedTeam {
    workers: Vec<WorkerRecord>,
    #[serde(default)]
    slots: Option<usize>,
}

/// The backup-recoverable `team.json` store.
#[derive(Clone, Debug)]
struct TeamPersistence {
    path: PathBuf,
    slots: Arc<AtomicUsize>,
}

impl TeamPersistence {
    fn new(state_dir: &std::path::Path) -> Self {
        Self {
            path: state_dir.join(TEAM_STATE_FILE),
            slots: Arc::new(AtomicUsize::new(DEFAULT_SLOTS)),
        }
    }

    fn decode(path: &std::path::Path) -> Result<Option<PersistedTeam>, String> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(format!("could not read {}: {err}", path.display()));
            }
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|err| format!("could not decode {}: {err}", path.display()))
    }

    fn load(&self) -> Result<Option<PersistedTeam>, String> {
        let backup = self.path.with_extension("json.bak");
        match Self::decode(&self.path) {
            Ok(Some(team)) => Ok(Some(team)),
            Ok(None) => Self::decode(&backup),
            Err(primary_error) => match Self::decode(&backup) {
                Ok(Some(team)) => Ok(Some(team)),
                Ok(None) => Err(primary_error),
                Err(backup_error) => Err(format!(
                    "{primary_error}; backup also failed: {backup_error}"
                )),
            },
        }
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
        let backup = self.path.with_extension("json.bak");
        if !self.path.exists() && backup.exists() {
            std::fs::rename(&backup, &self.path)
                .map_err(|err| format!("could not restore the team ledger backup: {err}"))?;
        }
        if backup.exists() {
            std::fs::remove_file(&backup)
                .map_err(|err| format!("could not clear the old team ledger backup: {err}"))?;
        }
        if self.path.exists() {
            std::fs::rename(&self.path, &backup)
                .map_err(|err| format!("could not back up the team ledger: {err}"))?;
        }
        match std::fs::rename(&temp, &self.path) {
            Ok(()) => {
                let _ = std::fs::remove_file(backup);
                Ok(())
            }
            Err(err) => {
                if backup.exists() {
                    let _ = std::fs::rename(&backup, &self.path);
                }
                Err(format!("could not replace the team ledger: {err}"))
            }
        }
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
    messages: Arc<Mutex<Vec<TeamMessage>>>,
    events: broadcast::Sender<TeamEvent>,
    generation: Arc<AtomicU64>,
    next_worker: AtomicU64,
    spawns_this_turn: AtomicUsize,
    lead_busy: AtomicBool,
    merge_lock: Mutex<()>,
    pricing: Arc<std::sync::RwLock<Vec<mnemo::config::PricingEntry>>>,
    max_spawns_per_turn: usize,
    default_token_budget: u32,
    persistence: TeamPersistence,
}

/// Upper bound for concurrently live worker sessions in one project.
pub const MAX_WORKER_SLOTS: usize = 32;

impl TeamRuntime {
    /// Open the team ledger, recover interrupted workers, and prepare to spawn.
    pub async fn open(options: TeamRuntimeOptions) -> Result<Self, String> {
        let persistence = TeamPersistence::new(&options.state_dir);
        let persisted = persistence.load()?;
        let slots = options
            .slots
            .or_else(|| persisted.as_ref().and_then(|team| team.slots))
            .unwrap_or(DEFAULT_SLOTS)
            .clamp(1, MAX_WORKER_SLOTS);
        persistence.slots.store(slots, Ordering::SeqCst);
        let mut workers = persisted.map(|team| team.workers).unwrap_or_default();
        for worker in &mut workers {
            recover_worker(&options.workspace_root, worker).await?;
        }
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        let runtime = Self {
            engine: options.engine,
            workspace_root: options.workspace_root,
            records: Arc::new(Mutex::new(workers)),
            live: Mutex::new(HashMap::new()),
            messages: Arc::new(Mutex::new(Vec::new())),
            events,
            generation: Arc::new(AtomicU64::new(0)),
            next_worker: AtomicU64::new(1),
            spawns_this_turn: AtomicUsize::new(0),
            lead_busy: AtomicBool::new(false),
            merge_lock: Mutex::new(()),
            pricing: Arc::new(std::sync::RwLock::new(options.pricing)),
            max_spawns_per_turn: options.max_spawns_per_turn.max(1),
            default_token_budget: DEFAULT_WORKER_TOKEN_BUDGET,
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
            slots: None,
            max_spawns_per_turn: DEFAULT_SPAWNS_PER_TURN,
            pricing: lead.pricing()?,
        })
        .await
    }

    /// Subscribe to every worker event.
    pub fn subscribe(&self) -> broadcast::Receiver<TeamEvent> {
        self.events.subscribe()
    }

    /// Set the default cumulative token budget for newly dispatched workers.
    pub fn with_worker_token_budget(mut self, token_budget: u32) -> Self {
        self.default_token_budget = token_budget.max(1);
        self
    }

    /// Reset the per-turn delegation budget and mark the lead non-idle.
    pub fn begin_lead_turn(&self) {
        self.spawns_this_turn.store(0, Ordering::SeqCst);
        self.lead_busy.store(true, Ordering::SeqCst);
    }

    /// Mirror the lead session's busy state into the merge gate.
    pub fn set_lead_busy(&self, busy: bool) {
        self.lead_busy.store(busy, Ordering::SeqCst);
    }

    /// Persist a new live-session cap without dropping active workers.
    pub async fn set_slots(&self, slots: usize) -> Result<TeamSnapshot, String> {
        if !(1..=MAX_WORKER_SLOTS).contains(&slots) {
            return Err(format!(
                "worker slots must be between 1 and {MAX_WORKER_SLOTS}"
            ));
        }
        let _operation = self.merge_lock.lock().await;
        let active = self.live.lock().await.len();
        if slots < active {
            return Err(format!(
                "stop {} worker(s) before reducing slots to {slots}",
                active - slots
            ));
        }
        let previous = self.persistence.slots.swap(slots, Ordering::SeqCst);
        if let Err(error) = self.persist().await {
            self.persistence.slots.store(previous, Ordering::SeqCst);
            let _ = self.persist().await;
            return Err(error);
        }
        Ok(self.snapshot().await)
    }

    /// Clean inactive clean checkouts and refresh this runtime's worker records.
    pub async fn cleanup(&self) -> Result<CleanupReport, String> {
        let _operation = self.merge_lock.lock().await;
        if !self.live.lock().await.is_empty() {
            return Err("stop all workers before cleaning their worktrees".to_string());
        }
        let state_dir = self
            .persistence
            .path
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| "the team ledger has no parent directory".to_string())?;
        let report = cleanup_workspace(&self.workspace_root, &state_dir).await?;
        let restored = self.persistence.load()?.unwrap_or_default();
        *self.records.lock().await = restored.workers;
        Ok(report)
    }

    /// The current team state.
    pub async fn snapshot(&self) -> TeamSnapshot {
        let workers = self.records.lock().await.clone();
        let active = self.live.lock().await.len();
        TeamSnapshot {
            workers,
            slots: self.persistence.slots.load(Ordering::SeqCst),
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

    /// Worker-to-lead messages received through `message_parent`.
    pub async fn messages(&self) -> Vec<TeamMessage> {
        self.messages.lock().await.clone()
    }

    /// The lead's team delegation, review, and repair host tools.
    pub fn lead_host_tools(&self) -> Vec<HostToolSpec> {
        let worker_arg = json!({"worker_id": {"type": "string"}});
        vec![
            HostToolSpec {
                name: "team_dispatch".to_string(),
                description: "Delegate a real coding task to a tracked worker on its own Git branch and worktree. Never use generic agent spawning.".to_string(),
                parameters: json!({"type":"object","properties":{"brief":{"type":"string"},"model":{"type":"string"}},"required":["brief"]}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(10_000),
            },
            HostToolSpec {
                name: "team_message".to_string(),
                description: "Send a message to a worker; a busy worker receives it as a steer.".to_string(),
                parameters: json!({"type":"object","properties":{"worker_id":{"type":"string"},"text":{"type":"string"}},"required":["worker_id","text"]}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(10_000),
            },
            HostToolSpec {
                name: "team_status".to_string(),
                description: "Read the team snapshot, or one worker by id.".to_string(),
                parameters: json!({"type":"object","properties":worker_arg}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(10_000),
            },
            HostToolSpec {
                name: "team_review".to_string(),
                description: "Persist your review of the current worker tip and its complete finding list. Re-review after every fix.".to_string(),
                parameters: json!({"type":"object","properties":{"worker_id":{"type":"string"},"summary":{"type":"string"},"findings":{"type":"array","items":{"type":"object"}}},"required":["worker_id","summary","findings"]}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(10_000),
            },
            HostToolSpec {
                name: "team_check".to_string(),
                description: "Record a real completed check against the current worker tip; never record a command you did not run.".to_string(),
                parameters: json!({"type":"object","properties":{"worker_id":{"type":"string"},"name":{"type":"string"},"status":{"type":"string","enum":["passed","failed"]},"detail":{"type":"string"}},"required":["worker_id","name","status"]}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(10_000),
            },
            HostToolSpec {
                name: "review_fix".to_string(),
                description: "Apply one exact, tiny source replacement for an open finding, commit it on the worker branch, and invalidate review/checks.".to_string(),
                parameters: json!({"type":"object","properties":{"worker_id":{"type":"string"},"finding_id":{"type":"string"},"path":{"type":"string"},"old_string":{"type":"string"},"new_string":{"type":"string"}},"required":["worker_id","finding_id","path","old_string","new_string"]}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(30_000),
            },
            HostToolSpec {
                name: "team_route_fix".to_string(),
                description: "Route a larger finding back to its tracked worker for repair and delta review.".to_string(),
                parameters: json!({"type":"object","properties":{"worker_id":{"type":"string"},"finding_id":{"type":"string"}},"required":["worker_id","finding_id"]}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(10_000),
            },
            HostToolSpec {
                name: "team_resolve_finding".to_string(),
                description: "Close a finding with fixed evidence or a written wontfix justification.".to_string(),
                parameters: json!({"type":"object","properties":{"worker_id":{"type":"string"},"finding_id":{"type":"string"},"status":{"type":"string","enum":["fixed","wontfix"]},"evidence":{"type":"string"},"justification":{"type":"string"}},"required":["worker_id","finding_id","status"]}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(10_000),
            },
            HostToolSpec {
                name: "team_merge".to_string(),
                description: "Merge one reviewed worker only after exact-tip checks and findings close; never targets main and never pushes.".to_string(),
                parameters: json!({"type":"object","properties":{"worker_id":{"type":"string"}},"required":["worker_id"]}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(120_000),
            },
            HostToolSpec {
                name: "team_revert_merge".to_string(),
                description: "Revert a worker's earlier local merge only while it remains unlanded on the original non-main lead branch.".to_string(),
                parameters: json!({"type":"object","properties":{"worker_id":{"type":"string"}},"required":["worker_id"]}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(120_000),
            },
        ]
    }

    /// The worker's host tool for reporting back to the lead.
    pub fn worker_host_tools(&self) -> Vec<HostToolSpec> {
        vec![HostToolSpec {
            name: "message_parent".to_string(),
            description: "Send a short status or question to the lead.".to_string(),
            parameters: json!({"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}),
            safety: SafetyLevel::AutoRun,
            timeout_ms: Some(10_000),
        }]
    }

    /// Execute a lead host-tool call.
    pub async fn handle_lead_host_tool(&self, name: &str, args: Value) -> ToolResult {
        if name == "team_status" {
            let result = match args.get("worker_id").and_then(Value::as_str) {
                Some(id) => self.worker(id).await,
                None => return worker_result(Ok(self.snapshot().await.workers), "listed workers"),
            };
            return worker_result(result.map(|worker| vec![worker]), "read worker");
        }
        let Some(worker_id) = args.get("worker_id").and_then(Value::as_str) else {
            if name == "team_dispatch" {
                let Some(brief) = args.get("brief").and_then(Value::as_str) else {
                    return ToolResult::error("team_dispatch requires a 'brief' string");
                };
                let model = args
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                return worker_result(self.dispatch(brief.to_string(), model).await, "dispatched");
            }
            return ToolResult::error(format!("{name} requires a 'worker_id' string"));
        };
        let result = match name {
            "team_message" => match args.get("text").and_then(Value::as_str) {
                Some(text) => self.message(worker_id, text.to_string()).await,
                None => Err("team_message requires a 'text' string".to_string()),
            },
            "team_review" => {
                let summary = args.get("summary").and_then(Value::as_str).unwrap_or("");
                match serde_json::from_value::<Vec<Finding>>(
                    args.get("findings").cloned().unwrap_or_else(|| json!([])),
                ) {
                    Ok(findings) => self.review(worker_id, summary.to_string(), findings).await,
                    Err(err) => Err(format!("invalid findings: {err}")),
                }
            }
            "team_check" => {
                self.record_check(
                    worker_id,
                    args.get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    args.get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    args.get("detail")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                )
                .await
            }
            "review_fix" => {
                self.review_fix(
                    worker_id,
                    args.get("finding_id").and_then(Value::as_str).unwrap_or(""),
                    args.get("path").and_then(Value::as_str).unwrap_or(""),
                    args.get("old_string").and_then(Value::as_str).unwrap_or(""),
                    args.get("new_string").and_then(Value::as_str).unwrap_or(""),
                )
                .await
            }
            "team_route_fix" => {
                self.route_fix(
                    worker_id,
                    args.get("finding_id").and_then(Value::as_str).unwrap_or(""),
                )
                .await
            }
            "team_resolve_finding" => {
                self.resolve_finding(
                    worker_id,
                    args.get("finding_id").and_then(Value::as_str).unwrap_or(""),
                    args.get("status").and_then(Value::as_str).unwrap_or(""),
                    args.get("evidence")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    args.get("justification")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                )
                .await
            }
            "team_merge" => self.merge(worker_id).await,
            "team_revert_merge" => self.revert_merge(worker_id).await,
            _ => Err(format!("unknown team host tool '{name}'")),
        };
        worker_result(result, name)
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
        let _guard = self.merge_lock.lock().await;
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
        if active >= self.persistence.slots.load(Ordering::SeqCst) {
            return Err(format!("all {} worker slots are busy", self.persistence.slots.load(Ordering::SeqCst)));
        }
        let base_ref = current_branch(&self.workspace_root).map_err(|err| {
            format!(
                "cannot dispatch a tracked worker: the selected project is not a usable Git checkout. Open the repository root (the folder containing .git), then retry. Git reported: {err}"
            )
        })?;
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
            token_budget: self.default_token_budget,
            total_tokens: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
            cached_tokens: 0,
            requests: 0,
            cost_micros: None,
            created_at_ms: millis,
            elapsed_ms: 0,
            turns: 0,
            last_activity_ms: millis,
            checks: Vec::new(),
            findings: Vec::new(),
            review: None,
            fixer_finding_id: None,
            merge: None,
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
            self.messages.clone(),
            self.records.clone(),
            self.pricing.clone(),
            self.persistence.clone(),
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

    pub(crate) async fn spawn_worker_session(
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
                host_tools: self.worker_host_tools(),
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
        let _guard = self.merge_lock.lock().await;
        self.message_inner(worker_id, text).await
    }

    async fn message_inner(&self, worker_id: &str, text: String) -> Result<WorkerRecord, String> {
        if text.trim().is_empty() {
            return Err("the worker message is empty".to_string());
        }
        let record = self.worker(worker_id).await?;
        if record.status == WorkerStatus::Cancelled {
            return Err("a cleaned worker cannot accept new work; review its retained branch".to_string());
        }
        if record.total_tokens >= record.token_budget as u64 {
            return Err("increase the worker token budget before continuing".to_string());
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
            worker.status = if worker.fixer_finding_id.is_some() {
                WorkerStatus::Fixing
            } else {
                WorkerStatus::Working
            };
            worker.pending = None;
        })
        .await;
        self.persist().await?;
        self.worker(worker_id).await
    }

    /// Pause the worker's current turn without ending its session.
    pub async fn pause(&self, worker_id: &str) -> Result<WorkerRecord, String> {
        let _guard = self.merge_lock.lock().await;
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
        let _guard = self.merge_lock.lock().await;
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
        let _guard = self.merge_lock.lock().await;
        self.stop_inner(worker_id).await
    }

    async fn stop_inner(&self, worker_id: &str) -> Result<WorkerRecord, String> {
        let record = self.worker(worker_id).await?;
        if record.status == WorkerStatus::Cancelled {
            return Err("a cleaned worker has no checkout to stop".to_string());
        }
        if matches!(record.status, WorkerStatus::Merged | WorkerStatus::Merging) {
            return Err("a merged worker cannot be stopped".to_string());
        }
        let live = self.live.lock().await.remove(worker_id);
        if let Some(live) = live {
            live.abort();
            let _ = live.session.close().await;
        }
        self.update_worker(worker_id, |worker| {
            worker.status = WorkerStatus::Stopped;
            worker.pending = None;
            worker.fixer_finding_id = None;
        })
        .await;
        self.persist().await?;
        self.worker(worker_id).await
    }

    /// Reattach a stopped/interrupted worker from its saved transcript. The
    /// session comes back idle; the lead then sends it a message.
    pub async fn resume(&self, worker_id: &str) -> Result<WorkerRecord, String> {
        let _guard = self.merge_lock.lock().await;
        self.resume_inner(worker_id).await
    }

    async fn resume_inner(&self, worker_id: &str) -> Result<WorkerRecord, String> {
        if self.live.lock().await.contains_key(worker_id) {
            return Err(format!("worker '{worker_id}' is already running"));
        }
        let record = self.worker(worker_id).await?;
        if record.status == WorkerStatus::Cancelled {
            return Err("a cleaned worker cannot be resumed; review its retained branch".to_string());
        }
        if matches!(record.status, WorkerStatus::Merged | WorkerStatus::Merging) {
            return Err("a merged worker cannot be resumed".to_string());
        }
        if record.total_tokens >= record.token_budget as u64 {
            return Err("increase the worker token budget before resuming".to_string());
        }
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
            self.messages.clone(),
            self.records.clone(),
            self.pricing.clone(),
            self.persistence.clone(),
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

    async fn worker_tip(&self, worker: &WorkerRecord) -> Result<String, String> {
        if worker.status == WorkerStatus::Cancelled && worker.worktree.as_os_str().is_empty() {
            if worker.branch != agent_branch(&worker.id)
                || worker.id.is_empty()
                || !worker.id.chars().all(|c| c.is_ascii_alphanumeric())
            {
                return Err("the worker ledger has an invalid branch identity".to_string());
            }
            return phase3::git_value(
                &self.workspace_root,
                &["rev-parse", &format!("refs/heads/{}^{{commit}}", worker.branch)],
            )
            .await;
        }
        phase3::tip(&worker.worktree).await
    }

    /// Persist a lead review against the current worker commit.
    pub async fn review(
        &self,
        worker_id: &str,
        summary: String,
        findings: Vec<Finding>,
    ) -> Result<WorkerRecord, String> {
        let _guard = self.merge_lock.lock().await;
        if summary.trim().is_empty() {
            return Err("the review summary is empty".to_string());
        }
        phase3::validate_findings(&findings)?;
        let worker = self.worker(worker_id).await?;
        if matches!(worker.status, WorkerStatus::Merged | WorkerStatus::Merging) {
            return Err("a merged worker cannot be reviewed again".to_string());
        }
        if worker.status == WorkerStatus::Failed {
            return Err("resume the failed worker and complete a fix before review".to_string());
        }
        if self.worker_is_busy(worker_id).await {
            return Err("the worker must be idle before review".to_string());
        }
        let tip = self.worker_tip(&worker).await?;
        let was_cancelled = worker.status == WorkerStatus::Cancelled;
        self.update_worker(worker_id, |worker| {
            worker.status = if was_cancelled {
                WorkerStatus::Cancelled
            } else {
                WorkerStatus::InReview
            };
            worker.review = Some(ReviewRecord {
                summary,
                tip,
                reviewed_at_ms: phase3::now_ms(),
            });
            worker.findings = findings;
            worker.fixer_finding_id = None;
            worker.error = None;
        })
        .await;
        self.persist().await?;
        self.worker(worker_id).await
    }

    /// Record a real check outcome against the server-observed worker tip.
    pub async fn record_check(
        &self,
        worker_id: &str,
        name: String,
        status: String,
        detail: Option<String>,
    ) -> Result<WorkerRecord, String> {
        let _guard = self.merge_lock.lock().await;
        if name.trim().is_empty() || !matches!(status.as_str(), "passed" | "failed") {
            return Err("a check needs a name and a passed/failed outcome".to_string());
        }
        let worker = self.worker(worker_id).await?;
        if matches!(worker.status, WorkerStatus::Merged | WorkerStatus::Merging) {
            return Err("checks cannot be added during or after merge".to_string());
        }
        let tip = self.worker_tip(&worker).await?;
        self.update_worker(worker_id, |worker| {
            worker.checks.retain(|check| check.name != name);
            worker.checks.push(WorkerCheck {
                name,
                status,
                detail,
                tip,
            });
        })
        .await;
        self.persist().await?;
        self.worker(worker_id).await
    }

    /// Raise one worker's cumulative token budget above its real usage.
    pub async fn set_token_budget(
        &self,
        worker_id: &str,
        token_budget: u32,
    ) -> Result<WorkerRecord, String> {
        let _guard = self.merge_lock.lock().await;
        let worker = self.worker(worker_id).await?;
        if token_budget == 0 || token_budget as u64 <= worker.total_tokens {
            return Err(format!(
                "the new budget must exceed the worker's cumulative usage of {} tokens",
                worker.total_tokens
            ));
        }
        let budget_error = worker
            .error
            .as_deref()
            .is_some_and(|error| error.contains("token budget"));
        self.update_worker(worker_id, |worker| {
            worker.token_budget = token_budget;
            if budget_error {
                worker.error = None;
            }
        })
        .await;
        self.persist().await?;
        self.worker(worker_id).await
    }

    /// Close one finding without changing the reviewed source tip.
    pub async fn resolve_finding(
        &self,
        worker_id: &str,
        finding_id: &str,
        status: &str,
        evidence: Option<String>,
        justification: Option<String>,
    ) -> Result<WorkerRecord, String> {
        let _guard = self.merge_lock.lock().await;
        if !matches!(status, "fixed" | "wontfix") {
            return Err("a finding can be closed as fixed or wontfix".to_string());
        }
        if status == "fixed" && evidence.as_deref().unwrap_or("").trim().is_empty() {
            return Err("fixed requires verification evidence".to_string());
        }
        if status == "wontfix" && justification.as_deref().unwrap_or("").trim().is_empty() {
            return Err("wontfix requires a written justification".to_string());
        }
        let worker = self.worker(worker_id).await?;
        if matches!(worker.status, WorkerStatus::Merged | WorkerStatus::Merging) {
            return Err("a merged worker's findings are immutable".to_string());
        }
        if !worker
            .findings
            .iter()
            .any(|finding| finding.id == finding_id)
        {
            return Err(format!("unknown finding '{finding_id}'"));
        }
        let fix_commit = if status == "fixed" {
            Some(self.worker_tip(&worker).await?)
        } else {
            None
        };
        self.update_worker(worker_id, |worker| {
            if let Some(finding) = worker
                .findings
                .iter_mut()
                .find(|item| item.id == finding_id)
            {
                finding.status = status.to_string();
                finding.fix_commit = fix_commit;
                finding.evidence = evidence;
                finding.justification = justification;
            }
        })
        .await;
        self.persist().await?;
        self.worker(worker_id).await
    }

    /// Apply and commit one constrained source replacement for a finding.
    pub async fn review_fix(
        &self,
        worker_id: &str,
        finding_id: &str,
        relative: &str,
        old_string: &str,
        new_string: &str,
    ) -> Result<WorkerRecord, String> {
        let _guard = self.merge_lock.lock().await;
        let worker = self.worker(worker_id).await?;
        if worker.status != WorkerStatus::InReview {
            return Err("review_fix requires a worker in review".to_string());
        }
        let review_tip = worker
            .review
            .as_ref()
            .map(|review| review.tip.as_str())
            .ok_or_else(|| "review_fix requires a fresh delta review".to_string())?;
        let current_tip = phase3::tip(&worker.worktree).await?;
        if review_tip != current_tip {
            return Err("review_fix requires a review of the current worker tip".to_string());
        }
        phase3::require_clean(&worker.worktree, "worker").await?;
        let finding = worker
            .findings
            .iter()
            .find(|finding| finding.id == finding_id && finding.status == "open")
            .ok_or_else(|| format!("unknown open finding '{finding_id}'"))?;
        if finding.file != relative {
            return Err("review_fix must edit the finding's source file".to_string());
        }
        if old_string.is_empty() || old_string == new_string {
            return Err("review_fix needs a non-empty, distinct replacement".to_string());
        }
        if old_string.lines().count() + new_string.lines().count() > 100 {
            return Err(
                "review_fix is limited to a tiny replacement; route this finding to the worker"
                    .to_string(),
            );
        }
        let path = phase3::review_file(&worker.worktree, relative)?;
        let original = std::fs::read_to_string(&path)
            .map_err(|err| format!("could not read {}: {err}", path.display()))?;
        if original.matches(old_string).count() != 1 {
            return Err("review_fix old_string must occur exactly once".to_string());
        }
        let updated = original.replacen(old_string, new_string, 1);
        std::fs::write(&path, updated)
            .map_err(|err| format!("could not write {}: {err}", path.display()))?;
        let add = match phase3::git(&worker.worktree, &["add", "--", relative]).await {
            Ok(output) => output,
            Err(err) => {
                let _ = std::fs::write(&path, &original);
                return Err(err);
            }
        };
        if !add.status.success() {
            let _ = phase3::git(&worker.worktree, &["reset", "--", relative]).await;
            let _ = std::fs::write(&path, &original);
            return Err(phase3::git_error(&["add", "--", relative], &add));
        }
        let message = format!("review: fix {finding_id}");
        let commit = match phase3::git(&worker.worktree, &["commit", "-m", &message]).await {
            Ok(output) => output,
            Err(err) => {
                let _ = phase3::git(&worker.worktree, &["reset", "--", relative]).await;
                let _ = std::fs::write(&path, &original);
                return Err(err);
            }
        };
        if !commit.status.success() {
            let _ = phase3::git(&worker.worktree, &["reset", "--", relative]).await;
            let _ = std::fs::write(&path, &original);
            return Err(phase3::git_error(&["commit", "-m", &message], &commit));
        }
        let fix_tip = phase3::tip(&worker.worktree).await?;
        self.update_worker(worker_id, |worker| {
            if let Some(finding) = worker
                .findings
                .iter_mut()
                .find(|item| item.id == finding_id)
            {
                finding.status = "fixed".to_string();
                finding.fix_commit = Some(fix_tip.clone());
                finding.evidence = Some(format!("review_fix committed {fix_tip}"));
                finding.justification = None;
            }
            worker.checks.clear();
            worker.review = None;
            worker.fixer_finding_id = None;
            worker.error = None;
        })
        .await;
        self.persist().await?;
        self.worker(worker_id).await
    }

    async fn worker_is_busy(&self, worker_id: &str) -> bool {
        self.live
            .lock()
            .await
            .get(worker_id)
            .is_some_and(|live| live.busy.load(Ordering::SeqCst))
    }

    /// Route a non-tiny finding to the original tracked worker.
    pub async fn route_fix(
        &self,
        worker_id: &str,
        finding_id: &str,
    ) -> Result<WorkerRecord, String> {
        let _guard = self.merge_lock.lock().await;
        let worker = self.worker(worker_id).await?;
        if matches!(worker.status, WorkerStatus::Merged | WorkerStatus::Merging) {
            return Err("a merged worker cannot receive a fixer".to_string());
        }
        let finding = worker
            .findings
            .iter()
            .find(|finding| finding.id == finding_id && finding.status == "open")
            .ok_or_else(|| format!("unknown open finding '{finding_id}'"))?
            .clone();
        if self.worker_is_busy(worker_id).await {
            return Err("the worker must be idle before routing a review fix".to_string());
        }
        if !self.live.lock().await.contains_key(worker_id) {
            self.resume_inner(worker_id).await?;
        }
        let previous = self.worker(worker_id).await?;
        self.update_worker(worker_id, |worker| {
            worker.status = WorkerStatus::Fixing;
            worker.fixer_finding_id = Some(finding_id.to_string());
            worker.checks.clear();
            worker.review = None;
        })
        .await;
        self.persist().await?;
        let brief = format!(
            "Fix review finding {}: {}\n{}\nFile: {}:{}\nCommit the fix, rerun the affected checks, and report evidence for delta review.",
            finding.id, finding.title, finding.detail, finding.file, finding.line
        );
        if let Err(err) = self.message_inner(worker_id, brief).await {
            self.update_worker(worker_id, |worker| *worker = previous)
                .await;
            self.persist().await?;
            return Err(err);
        }
        self.worker(worker_id).await
    }

    /// Merge one reviewed worker into the clean, idle, non-main lead branch.
    pub async fn merge(&self, worker_id: &str) -> Result<WorkerRecord, String> {
        let _guard = self.merge_lock.lock().await;
        if self.lead_busy.load(Ordering::SeqCst) {
            return Err("the lead must be idle before merge".to_string());
        }
        if self.worker_is_busy(worker_id).await {
            return Err("the worker must be idle before merge".to_string());
        }
        let target = current_branch(&self.workspace_root)
            .map_err(|err| format!("could not resolve the lead branch: {err}"))?;
        if target == "main" {
            return Err("Mnemo2 never merges a worker directly into main".to_string());
        }
        phase3::require_clean(&self.workspace_root, "lead").await?;
        let worker = self.worker(worker_id).await?;
        phase3::require_clean(&worker.worktree, "worker").await?;
        let tip = phase3::tip(&worker.worktree).await?;
        validate_merge(&worker, &tip)?;
        let previous = worker.clone();
        self.update_worker(worker_id, |worker| worker.status = WorkerStatus::Merging)
            .await;
        if let Err(err) = self.persist().await {
            self.update_worker(worker_id, |current| *current = previous.clone())
                .await;
            let _ = self.persist().await;
            return Err(err);
        }
        let message = format!("team: merge {} ({})", worker.name, worker.id);
        let output = match phase3::git(
            &self.workspace_root,
            &["merge", "--no-ff", &worker.branch, "-m", &message],
        )
        .await
        {
            Ok(output) => output,
            Err(err) => {
                self.update_worker(worker_id, |current| *current = previous.clone())
                    .await;
                let _ = self.persist().await;
                return Err(err);
            }
        };
        if !output.status.success() {
            let diagnostic = phase3::git_error(
                &["merge", "--no-ff", &worker.branch, "-m", &message],
                &output,
            );
            let merge_head = phase3::git(
                &self.workspace_root,
                &["rev-parse", "-q", "--verify", "MERGE_HEAD"],
            )
            .await?;
            if !merge_head.status.success() {
                self.update_worker(worker_id, |current| {
                    *current = previous.clone();
                    current.error = Some(diagnostic.clone());
                })
                .await;
                self.persist().await?;
                return Err(diagnostic);
            }
            let abort = match phase3::git(&self.workspace_root, &["merge", "--abort"]).await {
                Ok(output) => output,
                Err(err) => {
                    let diagnostic = format!("{diagnostic}\n{err}");
                    self.update_worker(worker_id, |current| {
                        current.error = Some(diagnostic.clone())
                    })
                    .await;
                    let _ = self.persist().await;
                    return Err(diagnostic);
                }
            };
            if !abort.status.success() {
                let diagnostic = format!(
                    "{diagnostic}\n{}",
                    phase3::git_error(&["merge", "--abort"], &abort)
                );
                self.update_worker(worker_id, |current| {
                    current.error = Some(diagnostic.clone())
                })
                .await;
                let _ = self.persist().await;
                return Err(diagnostic);
            }
            if let Err(err) =
                phase3::require_clean(&self.workspace_root, "lead after conflict").await
            {
                self.update_worker(worker_id, |current| current.error = Some(err.clone()))
                    .await;
                let _ = self.persist().await;
                return Err(err);
            }
            self.update_worker(worker_id, |worker| {
                worker.status = WorkerStatus::Conflict;
                worker.error = Some(diagnostic.clone());
                worker.review = None;
                worker.checks.clear();
                worker.fixer_finding_id = None;
            })
            .await;
            self.persist().await?;
            return Err(diagnostic);
        }
        let commit = phase3::tip(&self.workspace_root).await?;
        let record = merge_record_from_git(&self.workspace_root, &worker, commit, target).await?;
        self.update_worker(worker_id, |worker| {
            worker.status = WorkerStatus::Merged;
            worker.merge = Some(record);
            worker.error = None;
        })
        .await;
        self.persist().await?;
        let live = self.live.lock().await.remove(worker_id);
        if let Some(live) = live {
            live.abort();
            let _ = live.session.close().await;
        }
        if let Err(err) = cleanup_agent_worktree(&self.workspace_root, &worker.id).await {
            self.update_worker(worker_id, |worker| {
                worker.error = Some(format!(
                    "merged, but worktree cleanup needs attention: {err}"
                ));
            })
            .await;
            self.persist().await?;
        }
        self.worker(worker_id).await
    }

    /// Revert a previous local worker merge before the branch is landed.
    pub async fn revert_merge(&self, worker_id: &str) -> Result<WorkerRecord, String> {
        let _guard = self.merge_lock.lock().await;
        if self.lead_busy.load(Ordering::SeqCst) {
            return Err("the lead must be idle before revert".to_string());
        }
        phase3::require_clean(&self.workspace_root, "lead").await?;
        let worker = self.worker(worker_id).await?;
        if worker.status != WorkerStatus::Merged {
            return Err("only a merged worker can be reverted".to_string());
        }
        let merge = worker
            .merge
            .clone()
            .ok_or_else(|| "worker has no merge record".to_string())?;
        if merge.revert_commit.is_some() {
            return Err("this worker merge was already reverted".to_string());
        }
        let target = current_branch(&self.workspace_root)?;
        if target == "main" || target != merge.target {
            return Err("revert requires the original non-main lead branch".to_string());
        }
        let in_history = phase3::git(
            &self.workspace_root,
            &["merge-base", "--is-ancestor", &merge.commit, "HEAD"],
        )
        .await?;
        if !in_history.status.success() {
            return Err("the original merge is not in the current lead history".to_string());
        }
        let landed = phase3::git(
            &self.workspace_root,
            &["merge-base", "--is-ancestor", &merge.commit, "main"],
        )
        .await?;
        if landed.status.success() {
            return Err("the worker merge has already landed on main".to_string());
        }
        let output = phase3::git(
            &self.workspace_root,
            &["revert", "--no-edit", "-m", "1", &merge.commit],
        )
        .await?;
        if !output.status.success() {
            let _ = phase3::git(&self.workspace_root, &["revert", "--abort"]).await;
            return Err(phase3::git_error(
                &["revert", "--no-edit", "-m", "1", &merge.commit],
                &output,
            ));
        }
        let revert = phase3::tip(&self.workspace_root).await?;
        self.update_worker(worker_id, |worker| {
            if let Some(merge) = worker.merge.as_mut() {
                merge.revert_commit = Some(revert);
            }
            worker.error = None;
        })
        .await;
        self.persist().await?;
        self.worker(worker_id).await
    }

    /// Stop every live worker while preserving merged audit records.
    pub async fn stop_all(&self) -> Result<(), String> {
        let _guard = self.merge_lock.lock().await;
        let ids: Vec<String> = self.live.lock().await.keys().cloned().collect();
        for id in ids {
            self.stop_inner(&id).await?;
        }
        Ok(())
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
            let _ = worker.session.close().await;
        }
        let _ = self.persist().await;
    }
}

async fn cleanup_agent_worktree(root: &std::path::Path, worker_id: &str) -> Result<(), String> {
    let worktree = root
        .join(".worktrees")
        .join(format!("agent-{}", agent_short_id(worker_id)));
    let mut last_error = String::new();
    for _ in 0..12 {
        match remove_agent_worktree(root.to_path_buf(), worker_id.to_string()).await {
            Ok(()) => return Ok(()),
            Err(_) if !worktree.exists() => return Ok(()),
            Err(err) => last_error = err,
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    Err(last_error)
}

async fn merge_record_from_git(
    root: &std::path::Path,
    worker: &WorkerRecord,
    commit: String,
    target: String,
) -> Result<MergeRecord, String> {
    let review = worker
        .review
        .as_ref()
        .ok_or_else(|| "the merge has no durable review record".to_string())?;
    let (files, additions, deletions) = phase3::merge_diff_stats(root, &commit).await?;
    Ok(MergeRecord {
        commit,
        worker_tip: review.tip.clone(),
        branch: worker.branch.clone(),
        target,
        checks: worker
            .checks
            .iter()
            .map(|check| check.name.clone())
            .collect(),
        review_summary: review.summary.clone(),
        files,
        additions,
        deletions,
        worker_model: worker.model_id.clone().or_else(|| worker.model.clone()),
        worker_tokens: worker.total_tokens,
        cost_micros: worker.cost_micros,
        merged_at_ms: phase3::now_ms(),
        revert_commit: None,
    })
}

async fn recover_worker(root: &std::path::Path, worker: &mut WorkerRecord) -> Result<(), String> {
    if worker.total_tokens == 0 && worker.used_tokens > 0 {
        worker.total_tokens = worker.used_tokens as u64;
        worker.prompt_tokens = worker.used_tokens as u64;
    }
    if worker.status == WorkerStatus::Merging {
        let merge_head = phase3::git(root, &["rev-parse", "-q", "--verify", "MERGE_HEAD"]).await?;
        let lead_tip = phase3::tip(root).await?;
        let second_parent = phase3::git_value(root, &["rev-parse", &format!("{lead_tip}^2")])
            .await
            .ok();
        let reviewed_tip = worker.review.as_ref().map(|review| review.tip.clone());
        let completed = reviewed_tip.is_some() && second_parent == reviewed_tip;
        if completed {
            let target = current_branch(root)?;
            if target == "main" {
                return Err(
                    "an interrupted worker merge appears on main; inspect it before continuing"
                        .to_string(),
                );
            }
            phase3::require_clean(root, "lead after completed merge").await?;
            if merge_head.status.success() {
                let reset = phase3::git(root, &["reset", "--merge"]).await?;
                if !reset.status.success() {
                    return Err(phase3::git_error(&["reset", "--merge"], &reset));
                }
                phase3::require_clean(root, "lead after stale merge cleanup").await?;
            }
            let record = merge_record_from_git(root, worker, lead_tip, target).await?;
            worker.status = WorkerStatus::Merged;
            worker.merge = Some(record);
            worker.error = None;
            if let Err(err) = cleanup_agent_worktree(root, &worker.id).await {
                worker.error = Some(format!(
                    "merge recovered, but worktree cleanup needs attention: {err}"
                ));
            }
            return Ok(());
        }

        if merge_head.status.success() {
            let abort = phase3::git(root, &["merge", "--abort"]).await?;
            if !abort.status.success() {
                return Err(phase3::git_error(&["merge", "--abort"], &abort));
            }
            phase3::require_clean(root, "lead after merge abort").await?;
            worker.status = WorkerStatus::Conflict;
            worker.error = Some(
                "the interrupted merge was aborted; resolve the conflict and review the worker tip again"
                    .to_string(),
            );
        } else {
            phase3::require_clean(root, "lead after interrupted merge").await?;
            worker.status = WorkerStatus::Conflict;
            worker.error = Some(
                "the interrupted merge left no verifiable merge commit; inspect the lead and worker before retrying"
                    .to_string(),
            );
        }
        worker.review = None;
        worker.checks.clear();
        worker.merge = None;
        worker.fixer_finding_id = None;
    } else if matches!(
        worker.status,
        WorkerStatus::Dispatching
            | WorkerStatus::Working
            | WorkerStatus::NeedsYou
            | WorkerStatus::Fixing
    ) {
        worker.status = if worker.pending.is_some() {
            WorkerStatus::NeedsYou
        } else {
            WorkerStatus::Interrupted
        };
    }
    Ok(())
}

/// Handle a worker's host-tool call (`message_parent`) and return its result.
async fn worker_host_tool_result(
    messages: &Arc<Mutex<Vec<TeamMessage>>>,
    worker_id: &str,
    name: &str,
    args: &Value,
) -> ToolResult {
    if name != "message_parent" {
        return ToolResult::error(format!("unknown worker host tool '{name}'"));
    }
    let Some(text) = args.get("text").and_then(Value::as_str) else {
        return ToolResult::error("message_parent requires a 'text' string");
    };
    let at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    messages.lock().await.push(TeamMessage {
        worker_id: worker_id.to_string(),
        text: text.to_string(),
        at_ms,
    });
    ToolResult::success("message sent to the lead")
}

/// Send a host-tool result back to the worker session.
async fn send_worker_host_result(
    engine: &Arc<Engine>,
    session_id: SessionId,
    call_id: String,
    result: ToolResult,
) {
    if let Some(session) = engine.session(&session_id).await {
        let _ = session
            .send(SessionCommand::HostToolResult {
                call_id,
                result: Some(result),
                error: None,
            })
            .await;
    }
}

fn snapshot_model(snapshot: &mnemo::headless::SessionSnapshot) -> Option<String> {
    snapshot.model.clone().or_else(|| {
        snapshot
            .transcript
            .messages
            .iter()
            .rev()
            .find_map(|message| message.origin_model.clone())
    })
}

fn usage_cost_micros(
    pricing: &Arc<std::sync::RwLock<Vec<mnemo::config::PricingEntry>>>,
    model: Option<&str>,
    prompt_tokens: u64,
    cached_tokens: u64,
    completion_tokens: u64,
) -> Option<u64> {
    let pricing = pricing.read().expect("pricing lock poisoned");
    let model = model?;
    let bare_model = model.rsplit('/').next().unwrap_or(model);
    let rates = pricing
        .iter()
        .find(|entry| entry.model == model || entry.model == bare_model)?;
    let uncached = prompt_tokens.saturating_sub(cached_tokens) as f64;
    let micros = uncached * rates.input_per_1m
        + cached_tokens as f64 * rates.cached_per_1m
        + completion_tokens as f64 * rates.output_per_1m;
    if !micros.is_finite() || micros < 0.0 {
        return None;
    }
    Some(micros.round().min(u64::MAX as f64) as u64)
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
    messages: Arc<Mutex<Vec<TeamMessage>>>,
    records: Arc<Mutex<Vec<WorkerRecord>>>,
    pricing: Arc<std::sync::RwLock<Vec<mnemo::config::PricingEntry>>>,
    persistence: TeamPersistence,
) {
    while let Ok(event) = receiver.recv().await {
        if let SerializableAgentEvent::HostToolCall {
            call_id,
            name,
            args,
        } = &event.event
        {
            let result = worker_host_tool_result(&messages, &worker_id, name, args).await;
            send_worker_host_result(&engine, session_id, call_id.clone(), result).await;
        }
        let usage_model = if matches!(&event.event, SerializableAgentEvent::Usage { .. }) {
            match engine.session(&session_id).await {
                Some(session) => session
                    .snapshot()
                    .await
                    .ok()
                    .and_then(|snapshot| snapshot_model(&snapshot)),
                None => None,
            }
        } else {
            None
        };
        let mut terminal = false;
        let mut changed = false;
        let mut stop_for_budget = false;
        {
            let mut records = records.lock().await;
            if let Some(worker) = records.iter_mut().find(|worker| worker.id == worker_id) {
                let activity_ms = phase3::now_ms();
                worker.last_activity_ms = activity_ms;
                if worker.created_at_ms > 0 {
                    worker.elapsed_ms = activity_ms.saturating_sub(worker.created_at_ms);
                }
                match &event.event {
                    SerializableAgentEvent::Started => {
                        busy.store(true, Ordering::SeqCst);
                        worker.status = if worker.fixer_finding_id.is_some() {
                            WorkerStatus::Fixing
                        } else {
                            WorkerStatus::Working
                        };
                        changed = true;
                    }
                    SerializableAgentEvent::ModelChanged { model, .. } => {
                        worker.model_id = Some(model.clone());
                        changed = true;
                    }
                    SerializableAgentEvent::Usage {
                        prompt_tokens,
                        completion_tokens,
                        cached_tokens,
                        ..
                    } => {
                        let prompt = *prompt_tokens as u64;
                        let completion = *completion_tokens as u64;
                        let cached = *cached_tokens as u64;
                        worker.requests = worker.requests.saturating_add(1);
                        worker.prompt_tokens = worker.prompt_tokens.saturating_add(prompt);
                        worker.completion_tokens =
                            worker.completion_tokens.saturating_add(completion);
                        worker.cached_tokens = worker.cached_tokens.saturating_add(cached);
                        worker.total_tokens = worker
                            .total_tokens
                            .saturating_add(prompt.saturating_add(completion));
                        worker.used_tokens = worker.used_tokens.max(*prompt_tokens);
                        if worker.model_id.is_none() {
                            worker.model_id = usage_model.clone();
                        }
                        if let Some(cost) = usage_cost_micros(
                            &pricing,
                            worker.model_id.as_deref().or(worker.model.as_deref()),
                            prompt,
                            cached,
                            completion,
                        ) {
                            worker.cost_micros =
                                Some(worker.cost_micros.unwrap_or(0).saturating_add(cost));
                        }
                        changed = true;
                    }
                    SerializableAgentEvent::ContextUsage { used, max, .. } => {
                        worker.used_tokens = *used;
                        worker.max_tokens = *max;
                        changed = true;
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
                        let budget_paused = worker.status == WorkerStatus::NeedsYou
                            && worker
                                .error
                                .as_deref()
                                .is_some_and(|error| error.contains("token budget"));
                        if !budget_paused {
                            worker.status = WorkerStatus::Completed;
                        }
                        worker.turns = worker.turns.saturating_add(1);
                        terminal = true;
                    }
                    SerializableAgentEvent::Parked { .. } => {
                        busy.store(false, Ordering::SeqCst);
                        if !matches!(
                            worker.status,
                            WorkerStatus::Completed
                                | WorkerStatus::Interrupted
                                | WorkerStatus::Paused
                                | WorkerStatus::Stopped
                                | WorkerStatus::Failed
                                | WorkerStatus::Merged
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
                if worker.total_tokens >= worker.token_budget as u64
                    && !matches!(
                        worker.status,
                        WorkerStatus::Completed | WorkerStatus::Merged | WorkerStatus::Stopped
                    )
                {
                    busy.store(false, Ordering::SeqCst);
                    worker.status = WorkerStatus::NeedsYou;
                    worker.error = Some(format!(
                        "worker token budget of {} was exhausted; increase the budget before continuing",
                        worker.token_budget
                    ));
                    terminal = true;
                    changed = true;
                    stop_for_budget = true;
                }
            }
        }
        if stop_for_budget {
            if let Some(session) = engine.session(&session_id).await {
                let _ = session.send(SessionCommand::Interrupt).await;
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
            let worker_id = worker_id.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                let handle = engine.session(&session_id).await;
                let Some(handle) = handle else {
                    return;
                };
                let Ok(snapshot) = handle.snapshot().await else {
                    return;
                };
                let worktree = {
                    let records = records.lock().await;
                    records
                        .iter()
                        .find(|worker| worker.id == worker_id && worker.generation == generation)
                        .map(|worker| worker.worktree.clone())
                };
                let Some(worktree) = worktree else {
                    return;
                };
                let current_tip = phase3::tip(&worktree).await.ok();
                {
                    let mut records = records.lock().await;
                    if let Some(worker) = records
                        .iter_mut()
                        .find(|worker| worker.id == worker_id && worker.generation == generation)
                    {
                        if worker.model_id.is_none() {
                            worker.model_id = snapshot_model(&snapshot);
                        }
                        worker.transcript = snapshot.transcript;
                        worker.used_tokens = snapshot.used_tokens;
                        worker.max_tokens = snapshot.max_tokens;
                        worker.pending = snapshot.pending;
                        if current_tip.as_ref().is_some_and(|tip| {
                            worker
                                .review
                                .as_ref()
                                .is_some_and(|review| review.tip != *tip)
                                || worker.checks.iter().any(|check| check.tip != *tip)
                        }) {
                            worker.review = None;
                            worker.checks.clear();
                        }
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
    persistence.save(&PersistedTeam {
        workers,
        slots: Some(persistence.slots.load(Ordering::SeqCst)),
    })
}

/// Outcome of conservative start-screen cleanup.
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct CleanupReport {
    /// Clean worktrees actually removed.
    pub removed: Vec<String>,
    /// Branches/worktrees retained, with the reason for each.
    pub skipped: Vec<String>,
}

/// Clean only inactive, clean worker checkouts; retain every worker branch.
pub async fn cleanup_workspace(root: &Path, state_dir: &Path) -> Result<CleanupReport, String> {
    current_branch(root)?;
    let persistence = TeamPersistence::new(state_dir);
    let Some(mut ledger) = persistence.load()? else {
        return Ok(CleanupReport::default());
    };
    let mut report = CleanupReport::default();
    for index in 0..ledger.workers.len() {
        let worker = &mut ledger.workers[index];
        if worker.merge.is_some()
            || worker.worktree.as_os_str().is_empty()
            || matches!(
                worker.status,
                WorkerStatus::Dispatching
                    | WorkerStatus::Working
                    | WorkerStatus::NeedsYou
                    | WorkerStatus::Fixing
                    | WorkerStatus::Merging
                    | WorkerStatus::Conflict
            )
        {
            continue;
        }
        let id = worker.id.clone();
        let branch = worker.branch.clone();
        if branch != agent_branch(&id) || id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
            report.skipped.push(format!("{branch}: invalid worker identity"));
            continue;
        }
        let checkout = root
            .join(".worktrees")
            .join(format!("agent-{}", agent_short_id(&id)));
        let recorded = match dunce::canonicalize(&worker.worktree) {
            Ok(path) => path,
            Err(_) if !worker.worktree.exists() => checkout.clone(),
            Err(error) => {
                report.skipped.push(format!("{branch}: {error}"));
                continue;
            }
        };
        let expected = if checkout.exists() {
            dunce::canonicalize(&checkout).map_err(|err| err.to_string())?
        } else {
            checkout.clone()
        };
        if recorded != expected {
            report.skipped.push(format!("{branch}: checkout is outside its registered worktree"));
            continue;
        }
        let revision = format!("refs/heads/{branch}^{{commit}}");
        if let Err(error) = phase3::git_value(root, &["rev-parse", "--verify", &revision]).await {
            report.skipped.push(format!("{branch}: retained checkout because the branch cannot be verified: {error}"));
            continue;
        }
        if checkout.exists() {
            match phase3::git_value(&checkout, &["status", "--porcelain", "-uall"]).await {
                Ok(status) if status.trim().is_empty() => {}
                Ok(_) => {
                    report.skipped.push(format!("{branch}: uncommitted work is retained for review"));
                    continue;
                }
                Err(error) => {
                    report.skipped.push(format!("{branch}: cleanliness could not be verified: {error}"));
                    continue;
                }
            }
            if let Err(error) = remove_agent_worktree(root.to_path_buf(), id).await {
                report.skipped.push(format!("{branch}: cleanup failed: {error}"));
                continue;
            }
            report.removed.push(branch.clone());
        } else {
            report.skipped.push(format!("{branch}: checkout was already absent; branch retained"));
        }
        worker.status = WorkerStatus::Cancelled;
        worker.pending = None;
        worker.worktree = PathBuf::new();
        worker.error = Some("inactive checkout cleaned; the branch is retained for review".to_string());
        persistence.save(&ledger)?;
    }
    Ok(report)
}

fn worker_result<T: Serialize>(result: Result<T, String>, action: &str) -> ToolResult {
    match result {
        Ok(value) => {
            let data = serde_json::to_value(value).unwrap_or(Value::Null);
            ToolResult::success(action).with_data(data)
        }
        Err(err) => ToolResult::error(err),
    }
}

/// Enforce every source/evidence precondition while the merge lock is held.
fn validate_merge(worker: &WorkerRecord, tip: &str) -> Result<(), String> {
    if worker.status != WorkerStatus::InReview {
        return Err(format!("worker is {:?}, not in review", worker.status));
    }
    let review = worker
        .review
        .as_ref()
        .ok_or_else(|| "the current worker tip has not been reviewed".to_string())?;
    if review.tip != tip {
        return Err("the review is stale; review the current worker tip".to_string());
    }
    if let Some(finding) = worker
        .findings
        .iter()
        .find(|finding| finding.status == "open")
    {
        return Err(format!("finding '{}' is still open", finding.id));
    }
    phase3::validate_findings(&worker.findings)?;
    if worker.checks.is_empty() {
        return Err("no checks have passed on the merge-time tip".to_string());
    }
    if let Some(check) = worker
        .checks
        .iter()
        .find(|check| check.status != "passed" || check.tip != tip)
    {
        return Err(format!(
            "check '{}' has not passed on the merge-time tip",
            check.name
        ));
    }
    Ok(())
}

/// The worker's hard session limits.
fn executor_limits() -> SessionLimits {
    SessionLimits {
        max_turns: Some(WORKER_MAX_TURNS),
        wall_clock_ms: Some(WORKER_WALL_CLOCK_MS),
        ..SessionLimits::default()
    }
}

/// The Mnemo2 executor identity, distinct from Mnemo's read-only reviewer workflow.
pub(crate) fn executor_prompt() -> PromptSpec {
    PromptSpec {
        identity: concat!(
            "You are a Mnemo2 executor worker. Implement only the lead's scoped brief ",
            "in your assigned worktree. The lead reviews your work on its main thread; ",
            "you do not act as a reviewer. File editing, shell commands, tests, and Git ",
            "commits are available and expected when needed. ",
            "Do not create or modify plans, spawn agents, or ask the user. Run relevant ",
            "checks, commit on the assigned worker branch, and report the actual commit and checks."
        )
        .to_string(),
        blocks: vec![PromptBlock::FileToolRules],
        extra_stable: None,
        constitution: ConstitutionMode::Both,
    }
}

/// The executor surface: a real coding agent that cannot plan, finish, spawn
/// more agents, or ask the user — those are the lead's jobs.
fn executor_guardrails() -> Guardrails {
    let mut tools = ToolSelection::all();
    tools.exclude = vec![
        "create_plan".to_string(),
        "update_plan".to_string(),
        "finish".to_string(),
        "spawn_agent".to_string(),
        "ask_user".to_string(),
    ];
    Guardrails {
        prompt: Some(executor_prompt()),
        tools,
        route: vec!["free".to_string()],
        limits: Some(executor_limits()),
        gate_rules: Vec::new(),
        question_policy: QuestionPolicy::Deny,
        turn_end: TurnEndPolicy::Stop,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn phase_two_ledgers_load_with_phase_three_defaults() {
        let legacy = json!({
            "workers": [{
                "id": "00000001000000000001",
                "name": "Worker 00000001",
                "brief": "implement the task",
                "model": "test/scripted-a",
                "model_id": "scripted-a",
                "branch": "wt/agent-00000001",
                "worktree": "repo/.worktrees/agent-00000001",
                "status": "completed",
                "generation": 1,
                "transcript": { "messages": [] },
                "pending": null,
                "used_tokens": 120,
                "max_tokens": 128000,
                "checks": [{
                    "name": "cargo test --workspace",
                    "status": "passed",
                    "detail": null
                }],
                "error": null
            }]
        });

        let mut persisted: PersistedTeam = serde_json::from_value(legacy).expect("legacy ledger");
        let worker = &persisted.workers[0];
        assert_eq!(worker.token_budget, DEFAULT_WORKER_TOKEN_BUDGET);
        assert_eq!(worker.turns, 0);
        assert_eq!(worker.last_activity_ms, 0);
        assert!(worker.findings.is_empty());
        assert!(worker.review.is_none());
        assert!(worker.fixer_finding_id.is_none());
        assert!(worker.merge.is_none());
        assert_eq!(worker.checks[0].tip, "");
        recover_worker(std::path::Path::new("unused"), &mut persisted.workers[0])
            .await
            .expect("legacy recovery");
        assert_eq!(persisted.workers[0].total_tokens, 120);
        assert_eq!(persisted.workers[0].prompt_tokens, 120);
    }

    #[test]
    fn interrupted_ledger_replacement_recovers_the_backup() {
        let dir = tempfile::tempdir().expect("ledger dir");
        let persistence = TeamPersistence::new(dir.path());
        persistence
            .save(&PersistedTeam::default())
            .expect("initial save");
        let path = dir.path().join(TEAM_STATE_FILE);
        let backup = dir.path().join("team.json.bak");
        std::fs::rename(&path, &backup).expect("simulate interrupted replacement");
        std::fs::write(&path, "not json").expect("corrupt current ledger");
        assert!(persistence.load().expect("backup is readable").is_some());

        persistence
            .save(&PersistedTeam::default())
            .expect("replacement save");
        assert!(path.exists());
        assert!(!backup.exists());
        assert!(persistence.load().expect("valid replacement").is_some());
        std::fs::write(&path, "not json").expect("corrupt replacement");
        assert!(
            persistence.load().is_err(),
            "corruption must not become an empty team"
        );
    }

    fn lifecycle_git(root: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("git process");
        assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    }

    fn cleanup_worker(id: &str, branch: &str, worktree: &Path, status: WorkerStatus) -> WorkerRecord {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "name": format!("Worker {id}"),
            "brief": "Implement a scoped change",
            "model": null,
            "model_id": "test-model",
            "branch": branch,
            "worktree": worktree,
            "status": status,
            "generation": 1,
            "transcript": { "messages": [] },
            "pending": null,
            "used_tokens": 0,
            "max_tokens": 1000,
            "checks": [],
            "error": null,
        })).expect("worker fixture")
    }

    #[tokio::test]
    async fn cleanup_keeps_branches_and_dirty_work_but_removes_clean_checkouts() {
        let repo = tempfile::tempdir().expect("repo");
        let state = tempfile::tempdir().expect("state");
        let root = repo.path();
        lifecycle_git(root, &["init", "-b", "wt/test"]);
        lifecycle_git(root, &["config", "user.name", "Test"]);
        lifecycle_git(root, &["config", "user.email", "test@example.invalid"]);
        std::fs::write(root.join("file.txt"), "base\n").expect("base");
        lifecycle_git(root, &["add", "file.txt"]);
        lifecycle_git(root, &["commit", "-m", "base"]);
        let (clean_path, clean_branch) = provision_agent_worktree(
            root.to_path_buf(),
            "clean001".to_string(),
            "wt/test".to_string(),
        ).await.expect("clean worktree");
        let (dirty_path, dirty_branch) = provision_agent_worktree(
            root.to_path_buf(),
            "dirty002".to_string(),
            "wt/test".to_string(),
        ).await.expect("dirty worktree");
        std::fs::write(clean_path.join("file.txt"), "worker\n").expect("clean edit");
        lifecycle_git(&clean_path, &["add", "file.txt"]);
        lifecycle_git(&clean_path, &["commit", "-m", "clean change"]);
        std::fs::write(dirty_path.join("uncommitted.txt"), "keep me\n").expect("dirty edit");
        let clean = cleanup_worker("clean001", &clean_branch, &clean_path, WorkerStatus::Stopped);
        let dirty = cleanup_worker("dirty002", &dirty_branch, &dirty_path, WorkerStatus::InReview);
        let persistence = TeamPersistence::new(state.path());
        persistence.save(&PersistedTeam {
            workers: vec![clean.clone(), dirty.clone()],
            slots: Some(4),
        }).expect("ledger");

        let report = cleanup_workspace(root, state.path()).await.expect("cleanup");
        assert_eq!(report.removed, vec![clean_branch.clone()]);
        assert!(report.skipped.iter().any(|reason| reason.contains("uncommitted")));
        assert!(!clean_path.exists());
        assert!(dirty_path.join("uncommitted.txt").exists());
        let restored = persistence.load().expect("ledger").expect("saved team");
        assert_eq!(restored.slots, Some(4));
        let cleaned = restored.workers.iter().find(|worker| worker.id == clean.id).expect("clean record");
        assert_eq!(cleaned.status, WorkerStatus::Cancelled);
        assert!(cleaned.worktree.as_os_str().is_empty());
        let dirty_after = restored.workers.iter().find(|worker| worker.id == dirty.id).expect("dirty record");
        assert_eq!(dirty_after.status, WorkerStatus::InReview);
        assert!(dirty_after.worktree.exists());
        let diff = crate::mnemo2::workbench::worker_diff(root, cleaned)
            .await
            .expect("the cleaned branch remains reviewable");
        assert_eq!(diff.total_files, 1);
        assert!(phase3::git_value(root, &["rev-parse", &clean_branch]).await.is_ok());
        assert!(phase3::git_value(root, &["rev-parse", &dirty_branch]).await.is_ok());
    }

    #[test]
    fn pricing_uses_uncached_input_cached_input_and_output_rates() {
        let pricing = Arc::new(std::sync::RwLock::new(vec![mnemo::config::PricingEntry {
            model: "scripted-a".into(),
            input_per_1m: 2.0,
            output_per_1m: 10.0,
            cached_per_1m: 1.0,
        }]));
        assert_eq!(
            usage_cost_micros(&pricing, Some("scripted-a"), 100, 25, 10),
            Some(275)
        );
        assert_eq!(
            usage_cost_micros(&pricing, Some("test/scripted-a"), 100, 25, 10),
            Some(275)
        );
        assert_eq!(
            usage_cost_micros(&pricing, Some("unpriced"), 100, 25, 10),
            None
        );
        assert_eq!(usage_cost_micros(&pricing, None, 100, 25, 10), None);
    }
}
