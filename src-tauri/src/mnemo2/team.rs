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

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use mnemo::app::plan_board::{BoardPlan, BoardStatus, BoardStep, CloseOutcome, PlanBoard};
use mnemo::app::team_ledger::TeamLedger;
use mnemo::app::worktrees::{
    agent_branch, agent_short_id, current_branch, provision_agent_worktree, remove_agent_worktree,
};
use mnemo::headless::{
    AgentSession, Engine, EventReceiver, GateRule, GateVerdict, Guardrails, HostToolSpec,
    PendingInteraction, QuestionPolicy, SessionCommand, SessionEvent, SessionId, SpawnOptions,
    ToolSelection, Transcript, TurnEndPolicy,
};
use mnemo::headless::{ConstitutionMode, PromptBlock, PromptSpec};
use mnemo::tool::{SafetyLevel, ToolResult};
use mnemo::{PermissionPolicy, SerializableAgentEvent};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{broadcast, Mutex};
use tokio::task::JoinHandle;

use crate::mnemo2::ledger::{self, RecordsLedger};
use crate::mnemo2::phase3::{self, Finding, MergeRecord, ReviewEvidence, ReviewRecord};
use crate::mnemo2::runtime::{LeadModel, LeadRuntime};
use crate::mnemo2::usage::{event_session, usage_model_key, ProjectUsage, ProjectUsageView};

/// The Tauri event channel emitted by the team runtime.
pub const TEAM_EVENT_CHANNEL: &str = "mnemo2://team-event";

/// The persisted worker ledger inside the team's state directory.
const TEAM_STATE_FILE: &str = "team.json";

/// How many transient team events the broadcast bus keeps for a slow consumer.
const EVENT_BUFFER: usize = 1024;

/// The default number of concurrently live worker sessions.
pub const DEFAULT_SLOTS: usize = 3;

/// How long an attached external lead may go without calling in before it is
/// detached and the in-app lead takes over again.
pub const EXTERNAL_LEAD_IDLE_MS: u64 = 10 * 60 * 1000;

/// The durable-learning nudge appended to external-lead merge and close results.
pub const EXTERNAL_LEAD_MEMORY_REMINDER: &str = "Record durable SPEC:/DECISION:/HOW: learnings with memory_write and supersede stale records with memory_supersede.";

/// The most notices an unread external queue keeps; the oldest are dropped.
const EXTERNAL_QUEUE_CAP: usize = 256;

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
    /// The lead's brief: the task-specific instructions. Idempotent re-dispatch
    /// compares this, never the composed opening prompt.
    pub brief: String,
    /// The worker's composed first message (the plan-board brief header, then
    /// the lead's brief). `None` for a worker dispatched without a plan: its
    /// first message is the brief itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opening_prompt: Option<String>,
    /// The selected `endpoint/model` spec, when one was chosen.
    pub model: Option<String>,
    /// The actual model id observed from the live session, when known.
    pub model_id: Option<String>,
    /// The reasoning effort chosen for this worker; `None` = the model's
    /// configured default. Every session of the worker (resume, fixes) uses it.
    #[serde(default)]
    pub reasoning_effort: Option<String>,
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
    /// Whether the initial brief was accepted by a live session.
    #[serde(default)]
    pub brief_sent: bool,
    /// A real compaction command is running in this session.
    #[serde(default)]
    pub compacting: bool,
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
    /// Lead-assigned review risk: `standard` or `high` (legacy ledgers:
    /// `standard`). High-risk workers touch shared contracts, persistence,
    /// concurrency, security, or large cross-cutting surfaces.
    #[serde(default = "default_review_risk")]
    pub review_risk: String,
    /// The worker's role: `"implementation"` (default) or `"audit"`. Audit
    /// workers are fresh read-only independent reviewers; they are never
    /// reviewed, never merged, and carry no plan step.
    #[serde(default = "default_worker_kind")]
    pub kind: String,
    /// The implementation worker this audit reviews (audit workers only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit_target: Option<String>,
    /// The exact implementation worker tip this audit inspected. A fix
    /// commit moves the tip and makes this audit stale for the new tip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit_tip: Option<String>,

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
    /// The board plan this task was dispatched under (validated by the plan
    /// board at dispatch).
    #[serde(default)]
    pub plan_id: Option<String>,
    /// The 0-based plan step the task was dispatched under.
    #[serde(default)]
    pub plan_step: Option<usize>,
    /// The model tier (`difficult`, `average`, `simple`) the lead chose,
    /// when it dispatched by tier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
}

impl WorkerRecord {
    /// The first message the worker's session receives.
    fn first_message(&self) -> &str {
        self.opening_prompt.as_deref().unwrap_or(&self.brief)
    }
}

/// A plan-board signal for the host: the UI refreshes on `Changed`, the lead
/// is told about a `Notice`.
#[derive(Clone, Debug)]
pub enum BoardSignal {
    /// The board (or the workers stamped on it) may have changed.
    Changed,
    /// A deduplicated TeamManager notice for the lead.
    Notice {
        /// Dedup key.
        key: String,
        /// The notice body.
        text: String,
    },
}

/// A plan and step a task is dispatched under (`step` is 0-based).
#[derive(Clone, Debug, PartialEq, Eq)]
struct PlanRef {
    plan_id: String,
    step: usize,
}

/// A worker on a board step, as the Plan tab shows it.
#[derive(Clone, Debug, Serialize)]
pub struct BoardWorker {
    /// The worker id.
    pub id: String,
    /// The worker's display name.
    pub name: String,
    /// The worker's lifecycle status.
    pub status: WorkerStatus,
}

/// One board step plus the workers stamped with it.
#[derive(Clone, Debug, Serialize)]
pub struct BoardStepView {
    /// The step itself.
    #[serde(flatten)]
    pub step: BoardStep,
    /// The workers stamped with this plan and step.
    pub workers: Vec<BoardWorker>,
}

/// One board plan as the UI shows it: the report path is project-relative.
#[derive(Clone, Debug, Serialize)]
pub struct BoardPlanView {
    /// The plan id.
    pub id: String,
    /// The plan title.
    pub title: String,
    /// The plan goal.
    pub goal: String,
    /// `implementation`, `research`, or `bug_fixing`.
    pub kind: String,
    /// Open, closed, or abandoned.
    pub status: BoardStatus,
    /// Epoch milliseconds the plan closed or was abandoned.
    pub closed_at_ms: Option<u64>,
    /// The closing report, relative to the project root.
    pub report_path: Option<String>,
    /// The steps with their workers.
    pub steps: Vec<BoardStepView>,
}

/// The plan board's public snapshot.
#[derive(Clone, Debug, Serialize)]
pub struct BoardSnapshot {
    /// Every board plan.
    pub plans: Vec<BoardPlanView>,
}

/// Legacy ledgers carry no review risk; they decode as `standard`.
fn default_review_risk() -> String {
    "standard".to_string()
}

/// Legacy ledgers predate audit workers; they decode as `implementation`.
fn default_worker_kind() -> String {
    "implementation".to_string()
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
    /// Durable per-project usage and estimated cost across the lead and
    /// every worker; totals survive restarts and worker deletion.
    pub usage: ProjectUsageView,
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
    /// The configured model inventory: explicit worker models (and their
    /// reasoning efforts) must belong to it.
    pub models: Vec<LeadModel>,
    /// Configured per-model prices used to estimate real worker cost.
    pub pricing: Vec<mnemo::config::PricingEntry>,
}

/// The persisted team ledger.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct PersistedTeam {
    workers: Vec<WorkerRecord>,
    #[serde(default)]
    slots: Option<usize>,
    /// Raw per-model usage accumulated from live lead and worker events.
    /// The `#[serde(default)]` keeps ledgers written before this field
    /// (legacy `team.json`) decodable, starting them at an empty accumulator.
    #[serde(default)]
    usage: ProjectUsage,
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
    /// A failed close aborted the pump; only cleanup retries may use this handle.
    cleanup_retry_only: bool,
}

impl LiveWorker {
    fn abort(&self) {
        self.pump.abort();
    }

    /// Admit a non-cleanup session command only when the event pump is live.
    fn admit_session_command(&self, worker_id: &str) -> Result<(), String> {
        if self.cleanup_retry_only {
            return Err(format!(
                "worker '{worker_id}' session cleanup must be retried before sending commands"
            ));
        }
        Ok(())
    }
}

/// The state of an external lead (Claude Code leading over MCP) on one team.
#[derive(Debug, Default)]
struct ExternalLead {
    /// When it attached (Unix epoch milliseconds); `None` while detached.
    since_ms: Option<u64>,
    /// Notices queued for it, oldest first. The text is exactly what the
    /// in-app lead would have been sent.
    queue: VecDeque<String>,
    /// Notices an external lead never read and the in-app lead could not take
    /// (no session to receive them). They wait here, oldest first, for the
    /// in-app lead's next notice or the next external lead.
    undelivered: VecDeque<String>,
    /// Its last call (MCP, notices or wait), in Unix epoch milliseconds.
    last_activity_ms: u64,
    /// Successful memory searches in the current plan-creation window.
    memory_search_since_plan_create: bool,
    bug_search_since_plan_create: bool,
    /// Waiters currently blocked in [`TeamRuntime::wait_external_notices`].
    waiters: usize,
}

/// How a [`TeamRuntime::wait_external`] ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExternalWaitStatus {
    /// At least one notice was queued.
    Notices,
    /// Nothing arrived before the timeout.
    Timeout,
    /// The external lead detached while waiting (or was not attached).
    Detached,
}

impl ExternalWaitStatus {
    /// The wire name `/lead/wait` reports.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Notices => "notices",
            Self::Timeout => "timeout",
            Self::Detached => "detached",
        }
    }
}

/// The result of a [`TeamRuntime::wait_external`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalWait {
    /// The drained notices, oldest first (empty unless the status is
    /// [`ExternalWaitStatus::Notices`]).
    pub notices: Vec<String>,
    /// Why the wait ended.
    pub status: ExternalWaitStatus,
}

/// Counts one blocked waiter for as long as it lives, and counts the end of
/// the wait as activity.
struct ExternalWaiter<'a>(&'a TeamRuntime);

impl<'a> ExternalWaiter<'a> {
    fn new(team: &'a TeamRuntime) -> Self {
        let mut state = team.external_state();
        state.waiters += 1;
        state.last_activity_ms = unix_ms();
        drop(state);
        Self(team)
    }
}

impl Drop for ExternalWaiter<'_> {
    fn drop(&mut self) {
        let mut state = self.0.external_state();
        state.waiters = state.waiters.saturating_sub(1);
        state.last_activity_ms = unix_ms();
    }
}

/// Unix epoch milliseconds now.
fn unix_ms() -> u64 {
    u64::try_from(phase3::now_ms()).unwrap_or(u64::MAX)
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
    lead_busy: AtomicBool,
    #[cfg(test)]
    fail_next_close: AtomicBool,
    lead_session: Mutex<Option<SessionId>>,
    merge_lock: Mutex<()>,
    pricing: Arc<std::sync::RwLock<Vec<mnemo::config::PricingEntry>>>,
    models: std::sync::RwLock<Vec<LeadModel>>,
    tiers: std::sync::RwLock<mnemo::headless::TiersDto>,
    persistence: TeamPersistence,
    /// The durable per-project usage accumulator (lead + workers).
    usage: Arc<Mutex<ProjectUsage>>,
    /// Serializes every `team.json` write across worker pumps, the lead's
    /// usage recording, and cleanup so no usage report can be lost to a
    /// stale interleaving of concurrent ledger saves.
    save_lock: Arc<Mutex<()>>,
    board: std::sync::Mutex<Option<Arc<PlanBoard>>>,
    signals: broadcast::Sender<BoardSignal>,
    /// The external lead's attach flag, notice queue and activity clock.
    external: std::sync::Mutex<ExternalLead>,
    /// Wakes [`TeamRuntime::wait_external_notices`] on a push or a detach.
    external_wake: tokio::sync::Notify,
}

/// The model tiers the lead may dispatch by, with their one-line purpose.
const TIER_PURPOSES: [(&str, &str); 3] = [
    (
        "difficult",
        "design, debugging, bug_fixing plans, cross-cutting work, steps touching shared contracts",
    ),
    ("average", "typical implementation steps"),
    (
        "simple",
        "single-file mechanical edits, checks, lookups, scaffolding",
    ),
];

/// Upper bound for concurrently live worker sessions in one project.
pub const MAX_WORKER_SLOTS: usize = 32;

impl TeamRuntime {
    async fn close_session_for_cleanup(&self, session: &AgentSession) -> Result<(), String> {
        #[cfg(test)]
        if self.fail_next_close.swap(false, std::sync::atomic::Ordering::SeqCst) {
            return Err("injected close failure for transition tests".to_string());
        }
        session
            .close_and_wait()
            .await
            .map_err(|error| error.to_string())
    }

    #[cfg(test)]
    fn fail_next_close_for_test(&self) {
        self.fail_next_close
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

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
        let usage = persisted
            .as_ref()
            .map(|team| team.usage.clone())
            .unwrap_or_default();
        let mut workers = persisted.map(|team| team.workers).unwrap_or_default();
        for worker in &mut workers {
            recover_worker(&options.workspace_root, worker).await?;
        }
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        let (signals, _) = broadcast::channel(EVENT_BUFFER);
        let runtime = Self {
            engine: options.engine,
            workspace_root: options.workspace_root,
            records: Arc::new(Mutex::new(workers)),
            live: Mutex::new(HashMap::new()),
            messages: Arc::new(Mutex::new(Vec::new())),
            events,
            generation: Arc::new(AtomicU64::new(0)),
            next_worker: AtomicU64::new(1),
            lead_busy: AtomicBool::new(false),
            #[cfg(test)]
            fail_next_close: AtomicBool::new(false),
            lead_session: Mutex::new(None),
            merge_lock: Mutex::new(()),
            pricing: Arc::new(std::sync::RwLock::new(options.pricing)),
            models: std::sync::RwLock::new(options.models),
            tiers: std::sync::RwLock::new(mnemo::headless::TiersDto::default()),
            persistence,
            usage: Arc::new(Mutex::new(usage)),
            save_lock: Arc::new(Mutex::new(())),
            board: std::sync::Mutex::new(None),
            signals,
            external: std::sync::Mutex::new(ExternalLead::default()),
            external_wake: tokio::sync::Notify::new(),
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
            models: lead.models(),
            pricing: lead.pricing()?,
        })
        .await
        .map(|team| {
            team.set_tiers(lead.tiers());
            team
        })
    }

    /// Replace the model tiers new dispatches resolve against.
    pub fn set_tiers(&self, tiers: mnemo::headless::TiersDto) {
        *self.tiers.write().unwrap_or_else(|e| e.into_inner()) = tiers;
    }

    /// Refresh the model inventory, tiers and prices after a settings save.
    pub fn set_catalog(
        &self,
        models: Vec<LeadModel>,
        tiers: mnemo::headless::TiersDto,
        pricing: Vec<mnemo::config::PricingEntry>,
    ) {
        *self.models.write().unwrap_or_else(|e| e.into_inner()) = models;
        self.set_tiers(tiers);
        *self.pricing.write().expect("pricing lock poisoned") = pricing;
    }

    fn current_models(&self) -> Vec<LeadModel> {
        self.models
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The tier's configured models that are still available, in order.
    pub(crate) fn tier_models(&self, tier: &str) -> Vec<LeadModel> {
        let tiers = self.tiers.read().unwrap_or_else(|e| e.into_inner()).clone();
        let specs = match tier {
            "difficult" => tiers.difficult,
            "average" => tiers.average,
            "simple" => tiers.simple,
            _ => Vec::new(),
        };
        let models = self.current_models();
        specs
            .iter()
            .filter_map(|spec| models.iter().find(|m| &m.spec == spec).cloned())
            .collect()
    }

    /// Every available model some tier names, in tier order, once each.
    fn tiered_models(&self) -> Vec<LeadModel> {
        let mut models: Vec<LeadModel> = Vec::new();
        for (tier, _) in TIER_PURPOSES {
            for model in self.tier_models(tier) {
                if !models.iter().any(|known| known.spec == model.spec) {
                    models.push(model);
                }
            }
        }
        models
    }

    /// The model a lead's `tier`/`model` choice runs on. With tiers
    /// configured the lead stays inside them: an explicit model must be one
    /// a tier names. `None` when neither was given.
    fn lead_model_choice(
        &self,
        tier: Option<&str>,
        model: Option<String>,
    ) -> Result<Option<String>, String> {
        let model = match (model, tier) {
            (None, Some(tier)) => match self.tier_models(tier).into_iter().next() {
                Some(first) => Some(first.spec),
                None => {
                    return Err(format!(
                        "tier '{tier}' has no available models; pass a `model` or another tier"
                    ));
                }
            },
            (model, _) => model,
        };
        let tiered = self.tiered_models();
        match &model {
            Some(spec) if !tiered.is_empty() && !tiered.iter().any(|m| &m.spec == spec) => {
                Err(format!(
                    "'{spec}' is not in any model tier (Settings > Tiers); pass a `tier`, or a model a tier names: {}",
                    tiered.iter().map(|m| m.spec.as_str()).collect::<Vec<_>>().join(", ")
                ))
            }
            _ => Ok(model),
        }
    }

    /// The refusal for a dispatch with no tier while tiers are configured.
    fn tier_required(&self) -> Option<String> {
        self.tier_guidance().map(|guidance| {
            format!("choose a `tier` for this worker; its model comes from that tier:\n{guidance}")
        })
    }

    /// `$in/$out` per 1M tokens for a model, when priced.
    fn price_label(&self, model: &LeadModel) -> Option<String> {
        let pricing = self.pricing.read().expect("pricing lock poisoned");
        pricing
            .iter()
            .find(|p| p.model == model.model)
            .map(|p| format!("${}/${}", p.input_per_1m, p.output_per_1m))
    }

    /// The tier guidance shown to the lead: each tier with its one-line
    /// purpose and its models (with prices).
    fn tier_guidance(&self) -> Option<String> {
        let mut lines = Vec::new();
        for (tier, purpose) in TIER_PURPOSES {
            let models = self.tier_models(tier);
            if models.is_empty() {
                continue;
            }
            let list = models
                .iter()
                .map(|m| match self.price_label(m) {
                    Some(price) => format!("{} ({price})", m.spec),
                    None => m.spec.clone(),
                })
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!("{tier} - {purpose}: {list}"));
        }
        (!lines.is_empty()).then(|| lines.join("\n"))
    }

    /// Subscribe to every worker event.
    pub fn subscribe(&self) -> broadcast::Receiver<TeamEvent> {
        self.events.subscribe()
    }

    /// Subscribe to plan-board signals (UI refreshes and lead notices).
    pub fn subscribe_board(&self) -> broadcast::Receiver<BoardSignal> {
        self.signals.subscribe()
    }

    /// Tell subscribers the board may have changed.
    pub(crate) fn signal_board_changed(&self) {
        let _ = self.signals.send(BoardSignal::Changed);
    }

    fn notify_lead(&self, key: String, text: String) {
        let _ = self.signals.send(BoardSignal::Notice { key, text });
    }

    /// Share the lead's one plan board with the team.
    pub(crate) fn attach_board(&self, board: Arc<PlanBoard>) {
        board.set_external_lead_attached(self.external_lead_attached());
        *self.board.lock().unwrap_or_else(|e| e.into_inner()) = Some(board);
    }

    fn board(&self) -> Option<Arc<PlanBoard>> {
        self.board.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The markdown of the board plan `plan_id`: what the high-risk gate
    /// scans for acceptance scenarios and what an audit brief carries.
    fn plan_document(&self, plan_id: &str) -> Result<String, String> {
        self.board()
            .ok_or_else(|| "the plan board is not available".to_string())?
            .read_markdown(plan_id)
    }

    /// Every board plan with the workers stamped on each step. Report paths
    /// are relative to the project root.
    pub async fn board_snapshot(&self) -> Result<BoardSnapshot, String> {
        let board = self
            .board()
            .ok_or_else(|| "the plan board is not available".to_string())?;
        let plans = board.list()?;
        let workers = self.records.lock().await.clone();
        let plans = plans
            .into_iter()
            .map(|plan: BoardPlan| BoardPlanView {
                report_path: plan
                    .report_path
                    .as_deref()
                    .map(|path| project_relative(&self.workspace_root, path)),
                steps: plan
                    .steps
                    .into_iter()
                    .map(|step| {
                        let on_step = workers
                            .iter()
                            .filter(|w| {
                                w.plan_id.as_deref() == Some(plan.id.as_str())
                                    && w.plan_step == step.number.checked_sub(1)
                            })
                            .map(|w| BoardWorker {
                                id: w.id.clone(),
                                name: w.name.clone(),
                                status: w.status,
                            })
                            .collect();
                        BoardStepView {
                            step,
                            workers: on_step,
                        }
                    })
                    .collect(),
                id: plan.id,
                title: plan.title,
                goal: plan.goal,
                kind: plan.kind,
                status: plan.status,
                closed_at_ms: plan.closed_at_ms,
            })
            .collect();
        Ok(BoardSnapshot { plans })
    }

    /// Mark the lead non-idle: the merge gate's busy hook, re-armed at
    /// every turn boundary so mid-turn merges stay refused.
    pub fn begin_lead_turn(&self) {
        self.lead_busy.store(true, Ordering::SeqCst);
    }

    /// Mirror the lead session's busy state into the merge gate.
    pub fn set_lead_busy(&self, busy: bool) {
        self.lead_busy.store(busy, Ordering::SeqCst);
    }

    pub(crate) fn lead_is_busy(&self) -> bool {
        self.lead_busy.load(Ordering::SeqCst)
    }

    pub(crate) async fn attach_lead(&self, session_id: SessionId) {
        *self.lead_session.lock().await = Some(session_id);
    }

    pub(crate) async fn lead_is_current(&self, session_id: SessionId) -> bool {
        *self.lead_session.lock().await == Some(session_id)
    }

    fn external_state(&self) -> std::sync::MutexGuard<'_, ExternalLead> {
        self.external.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Make an external lead (Claude Code over MCP) this team's lead: from
    /// now on lead notices queue for it instead of reaching the in-app lead.
    /// Notices a previous external lead never read (and the in-app lead could
    /// not take) are handed to this one. Returns `true` when it was not
    /// attached before. Counts as activity.
    pub fn attach_external_lead(&self) -> bool {
        let mut state = self.external_state();
        state.last_activity_ms = unix_ms();
        if state.since_ms.is_some() {
            return false;
        }
        state.since_ms = Some(state.last_activity_ms);
        state.queue = std::mem::take(&mut state.undelivered);
        state.memory_search_since_plan_create = false;
        state.bug_search_since_plan_create = false;
        drop(state);
        if let Some(board) = self.board() {
            board.set_external_lead_attached(true);
        }
        true
    }

    /// Hand the team back to the in-app lead and wake blocked waiters.
    /// Returns `true` when an external lead was attached.
    ///
    /// Notices it never read are not dropped: they move to the undelivered
    /// list, which [`TeamRuntime::take_undelivered_notices`] hands over (the
    /// caller gives them to the in-app lead, see
    /// `LeadRuntime::release_external_lead`).
    pub fn detach_external_lead(&self) -> bool {
        let was_attached = {
            let mut state = self.external_state();
            let was_attached = state.since_ms.take().is_some();
            // They are newer than anything already held back.
            let unread = std::mem::take(&mut state.queue);
            state.undelivered.extend(unread);
            while state.undelivered.len() > EXTERNAL_QUEUE_CAP {
                state.undelivered.pop_front();
            }
            state.memory_search_since_plan_create = false;
            state.bug_search_since_plan_create = false;
            was_attached
        };
        if let Some(board) = self.board() {
            board.set_external_lead_attached(false);
        }
        self.external_wake.notify_waiters();
        was_attached
    }

    /// Whether an external lead currently leads this team.
    pub fn external_lead_attached(&self) -> bool {
        self.external_state().since_ms.is_some()
    }

    /// Refuse an external-lead plan until its attachment searched memory.
    /// Bug-fixing plans additionally require a search narrowed to bug records.
    pub fn external_plan_create_refusal(&self, bug_fixing: bool) -> Option<String> {
        let state = self.external_state();
        state.since_ms?;
        if !state.memory_search_since_plan_create {
            return Some(
                "plan_create refused: call memory_search first, then retry plan_create."
                    .to_string(),
            );
        }
        if bug_fixing && !state.bug_search_since_plan_create {
            return Some(
                "plan_create refused for kind bug_fixing: call memory_search with record_type \"bug\", then retry plan_create."
                    .to_string(),
            );
        }
        None
    }

    /// Record a successful external-lead memory search for the current
    /// attachment and current plan-creation window.
    pub fn record_external_memory_search(&self, bug_search: bool) {
        let mut state = self.external_state();
        if state.since_ms.is_some() {
            state.memory_search_since_plan_create = true;
            state.bug_search_since_plan_create |= bug_search;
        }
    }

    /// Start a fresh required-search window after a successful plan_create.
    pub fn record_external_plan_create(&self) {
        let mut state = self.external_state();
        if state.since_ms.is_some() {
            state.memory_search_since_plan_create = false;
            state.bug_search_since_plan_create = false;
        }
    }

    /// When the external lead attached (Unix epoch milliseconds), if it is.
    pub fn external_lead_since_ms(&self) -> Option<u64> {
        self.external_state().since_ms
    }

    /// Record a call from the external lead (keeps it from idling out).
    pub fn touch_external_lead(&self) {
        self.external_state().last_activity_ms = unix_ms();
    }

    /// Whether the attached external lead has been silent for `idle_ms` as of
    /// `now_ms`. A blocked waiter counts as presence, so a long-poll never
    /// idles out.
    pub fn external_idle_expired(&self, now_ms: u64, idle_ms: u64) -> bool {
        let state = self.external_state();
        state.since_ms.is_some()
            && state.waiters == 0
            && now_ms.saturating_sub(state.last_activity_ms) >= idle_ms
    }

    /// Whether a `wait` long-poll is blocked right now.
    pub fn external_waiter_alive(&self) -> bool {
        self.external_state().waiters > 0
    }

    /// Queue a notice for the external lead and wake any waiter. The text is
    /// the exact string the in-app lead would have been sent.
    ///
    /// The check and the push happen under one lock: with no external lead
    /// attached nothing is queued and this returns `false`, so the caller
    /// gives the notice to the in-app lead instead (a detach can never
    /// strand a notice in a queue nobody reads).
    pub fn push_external_notice(&self, text: String) -> bool {
        {
            let mut state = self.external_state();
            if state.since_ms.is_none() {
                return false;
            }
            if state.queue.len() >= EXTERNAL_QUEUE_CAP {
                state.queue.pop_front();
            }
            state.queue.push_back(text);
        }
        self.external_wake.notify_waiters();
        true
    }

    /// Take every queued notice, oldest first.
    pub fn drain_external_notices(&self) -> Vec<String> {
        self.external_state().queue.drain(..).collect()
    }

    /// Put notices that were drained but never reached the external lead
    /// (the response write failed) back at the front of its queue, in their
    /// original order. If the lead detached meanwhile they join the
    /// undelivered list instead, ahead of anything newer.
    pub fn requeue_external_notices(&self, notices: Vec<String>) {
        if notices.is_empty() {
            return;
        }
        {
            let mut state = self.external_state();
            let attached = state.since_ms.is_some();
            let target = if attached {
                &mut state.queue
            } else {
                &mut state.undelivered
            };
            for notice in notices.into_iter().rev() {
                target.push_front(notice);
            }
            while target.len() > EXTERNAL_QUEUE_CAP {
                target.pop_front();
            }
        }
        self.external_wake.notify_waiters();
    }

    /// Take the notices an external lead never read and the in-app lead has
    /// not yet received, oldest first.
    pub fn take_undelivered_notices(&self) -> Vec<String> {
        self.external_state().undelivered.drain(..).collect()
    }

    /// Put notices the in-app lead could not take back on the undelivered
    /// list, ahead of anything newer.
    pub fn restore_undelivered_notices(&self, notices: Vec<String>) {
        let mut state = self.external_state();
        for notice in notices.into_iter().rev() {
            state.undelivered.push_front(notice);
        }
        while state.undelivered.len() > EXTERNAL_QUEUE_CAP {
            state.undelivered.pop_front();
        }
    }

    /// The queued notices, left in place.
    pub fn peek_external_notices(&self) -> Vec<String> {
        self.external_state().queue.iter().cloned().collect()
    }

    /// Block until a notice is queued (then drain and return all of them), the
    /// external lead detaches, or `timeout` passes (then return `[]`).
    pub async fn wait_external_notices(&self, timeout: std::time::Duration) -> Vec<String> {
        self.wait_external(timeout).await.notices
    }

    /// Like [`TeamRuntime::wait_external_notices`], and says how the wait
    /// ended.
    pub async fn wait_external(&self, timeout: std::time::Duration) -> ExternalWait {
        let _waiter = ExternalWaiter::new(self);
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            // Arm the wake-up before looking, so a push in between is not lost.
            let woken = self.external_wake.notified();
            tokio::pin!(woken);
            woken.as_mut().enable();
            let notices = self.drain_external_notices();
            if !notices.is_empty() {
                return ExternalWait {
                    notices,
                    status: ExternalWaitStatus::Notices,
                };
            }
            if !self.external_lead_attached() {
                return ExternalWait {
                    notices,
                    status: ExternalWaitStatus::Detached,
                };
            }
            if tokio::time::timeout_at(deadline, woken).await.is_err() {
                let notices = self.drain_external_notices();
                let status = if !notices.is_empty() {
                    ExternalWaitStatus::Notices
                } else if self.external_lead_attached() {
                    ExternalWaitStatus::Timeout
                } else {
                    ExternalWaitStatus::Detached
                };
                return ExternalWait { notices, status };
            }
        }
    }

    /// How many worker turns are running now (dispatching, working, fixing or
    /// merging).
    pub async fn live_worker_turns(&self) -> usize {
        ledger::live_turns(&self.records.lock().await)
    }

    /// A short board, workers and tiers line for the external lead's session
    /// start.
    pub async fn external_summary(&self) -> String {
        let (running, waiting, merged, total) = {
            let workers = self.records.lock().await;
            (
                ledger::live_turns(&workers),
                workers
                    .iter()
                    .filter(|w| {
                        matches!(
                            w.status,
                            WorkerStatus::Completed
                                | WorkerStatus::InReview
                                | WorkerStatus::NeedsYou
                                | WorkerStatus::Failed
                                | WorkerStatus::Conflict
                                | WorkerStatus::Interrupted
                        )
                    })
                    .count(),
                workers.iter().filter(|w| ledger::merged(w)).count(),
                workers.len(),
            )
        };
        let mut summary = format!(
            "Mnemo2 team: {running} worker(s) running, {waiting} waiting on you (review, blocked or failed), {merged} merged, {total} total."
        );
        let plans: Vec<String> = self
            .board()
            .and_then(|board| board.list().ok())
            .unwrap_or_default()
            .into_iter()
            .filter(|plan| plan.status == BoardStatus::Open)
            .map(|plan| {
                let done = plan.steps.iter().filter(|step| step.done).count();
                format!("{} \"{}\" ({done}/{} steps done)", plan.id, plan.title, plan.steps.len())
            })
            .collect();
        if plans.is_empty() {
            summary.push_str(" No open plans.");
        } else {
            summary.push_str(&format!(" Open plans: {}.", plans.join("; ")));
        }
        if let Some(tiers) = self.tier_guidance() {
            summary.push_str(&format!(" Tiers - {}", tiers.replace('\n', "; ")));
        }
        summary
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
        // Serialize with concurrent usage recording (and pump saves) so
        // cleanup's own ledger rewrite cannot drop a usage report that
        // lands mid-cleanup; the in-memory accumulator stays authoritative.
        let _ledger_save = self.save_lock.lock().await;
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
        let usage = self.usage.lock().await.clone();
        let pricing = self
            .pricing
            .read()
            .expect("pricing lock poisoned")
            .clone();
        TeamSnapshot {
            workers,
            slots: self.persistence.slots.load(Ordering::SeqCst),
            active,
            usage: usage.view(&pricing),
        }
    }

    /// Fold one live usage report (lead or worker session) into the durable
    /// per-project accumulator and persist the ledger.
    ///
    /// The observed model id (already keyed through [`usage_model_key`])
    /// attributes the row; a report with no observed model lands on the
    /// literal `"unknown"` row so it still shows up in totals and in
    /// `unpriced_models` instead of being silently dropped. Raw token counts
    /// are stored (not costs), so the totals can be repriced whenever the
    /// pricing tables change.
    pub async fn record_usage(
        &self,
        model: Option<&str>,
        prompt_tokens: u64,
        cached_tokens: u64,
        completion_tokens: u64,
    ) -> Result<(), String> {
        let key = model
            .map(usage_model_key)
            .unwrap_or("unknown")
            .to_string();
        {
            let mut usage = self.usage.lock().await;
            usage.record(&key, prompt_tokens, cached_tokens, completion_tokens);
        }
        save_workers(&self.records, &self.usage, &self.save_lock, &self.persistence).await
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
        let guidance = self.tier_guidance();
        // With tiers configured the lead picks among the tiers' models only.
        let lead_models = if guidance.is_some() {
            self.tiered_models()
        } else {
            self.current_models()
        };
        let model_description = if guidance.is_some() {
            "A specific model from the tiers, only when the tier's first model does not fit; it overrides `tier`. Prefer `tier`; models outside the tiers are refused."
        } else {
            "The exact endpoint/model spec this worker runs on. Choose it per task: a strong reasoning model for design-heavy, cross-cutting, or debugging work; a fast, inexpensive model for mechanical edits, boilerplate, or test scaffolding. Omit to use the configured default."
        };
        let mut model = json!({"type": "string", "description": model_description});
        let tier_description = match &guidance {
            Some(text) => format!(
                "Required: choose the cheapest tier that fits the step; the worker runs on that tier's first model.\n{text}"
            ),
            None => "The model tier. No tiers are configured (Settings > Tiers), so omit it and the default model is used.".to_string(),
        };
        let tier = json!({
            "type": "string",
            "enum": TIER_PURPOSES.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
            "description": tier_description,
        });
        let specs: Vec<&str> = lead_models
            .iter()
            .map(|model| model.spec.as_str())
            .collect();
        if !specs.is_empty() {
            model["enum"] = json!(specs);
        }
        let mut efforts: Vec<&str> = Vec::new();
        for effort in lead_models
            .iter()
            .flat_map(|model| &model.reasoning_efforts)
        {
            if !efforts.contains(&effort.as_str()) {
                efforts.push(effort);
            }
        }
        let mut reasoning_effort = json!({
            "type": "string",
            "description": "The worker's reasoning effort, chosen with its model: higher for hard design or debugging, lower for mechanical work. Applies to the model `tier` or `model` picks; a value that model does not support is rejected with its allowed list. Omit to use the model's configured default."
        });
        if !efforts.is_empty() {
            reasoning_effort["enum"] = json!(efforts);
        }
        let mut dispatch_required = vec!["worker_id", "plan_id", "step", "brief"];
        if guidance.is_some() {
            dispatch_required.push("tier");
        }
        vec![
            HostToolSpec {
                name: "team_dispatch".to_string(),
                description: "Delegate one implementation task through TeamManager to a full executor worker, under a step of a plan on the plan board (plan first: plan_create, then dispatch with its plan_id and 1-based step). The worker receives the plan and its step automatically, so your brief is only the task-specific instructions. A step is checked off when one of its tasks merges and none is still open. Workers run in their own worktree and branch with file_edit, file_write, shell, git, and tests, and have no token or cost budget: give them whole, self-contained implementation tasks (code, tests, verification, commit), never read-only, content-only, or 'send me the file text' briefs. Choose a stable unique worker_id of 8-64 ASCII letters/digits and keep it with the brief/model; after a timeout or uncertain host result retry with that same identity, never a guessed replacement. Mark shared contracts, persistence, concurrency, security, or large/cross-cutting work with review_risk='high'; high-risk dispatch requires that plan to declare observable acceptance scenarios (plan_create acceptance_scenarios or plan_update add_acceptance_scenarios). The JSON response returns the authoritative worker ID and status. Never use generic agent spawning, Git, files, or team.json to infer or mutate worker lifecycle.".to_string(),
                parameters: json!({"type":"object","properties":{"worker_id":{"type":"string","minLength":8,"maxLength":64,"pattern":"^[A-Za-z0-9]+$"},"plan_id":{"type":"string","description":"The board plan this task belongs to (from plan_create or plan_list). Required."},"step":{"type":"integer","minimum":1,"description":"The 1-based step of that plan this task works on. Required; an unknown, closed, or abandoned plan, a missing step, or a step that is already done is refused."},"brief":{"type":"string","description":"Task-specific instructions only. The worker automatically receives the plan (title, goal, context, decisions, interfaces, acceptance scenarios, step list) and the full text of its step; do not repeat them."},"tier":tier.clone(),"model":model.clone(),"reasoning_effort":reasoning_effort.clone(),"review_risk":{"type":"string","enum":["standard","high"],"description":"Lead-assigned review risk; high risk requires observable acceptance scenarios on this plan and an independent audit before merge."}},"required":dispatch_required}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(10_000),
            },
            HostToolSpec {
                name: "team_audit_dispatch".to_string(),
                description: "Dispatch a fresh read-only independent audit of one implementation worker's current branch/tip for a high-risk review. The reviewer_id is a stable unique 8-64 ASCII letters/digits ID of your choosing; retry a timed-out dispatch with that same identity for the same worker and tip, and choose a new reviewer_id after any fix commit moves the tip. The audit receives the worker's board plan, its intent brief, and the target diff — never the worker's own summary or findings — and cannot edit files, commit, merge, spawn, ask the user, or tick plan steps. When it completes, link it through team_review's evidence.independent_reviewer_id and write an audit_adjudication; never merge the audit worker itself.".to_string(),
                parameters: json!({"type":"object","properties":{"worker_id":{"type":"string"},"reviewer_id":{"type":"string","minLength":8,"maxLength":64,"pattern":"^[A-Za-z0-9]+$"},"model":{"type":"string"},"reasoning_effort":{"type":"string","enum":["max","high","medium","low","minimal","none"]},"brief":{"type":"string"}},"required":["worker_id","reviewer_id"]}),
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
                description: "Read authoritative TeamManager state for every worker or one exact returned worker_id. This is the only worker lifecycle/ID authority; never infer status from Git, worktrees, timestamps, or files. Pass plan_id to see one plan's workers (their step, status, findings). Ask when you need it; status is not pushed.".to_string(),
                parameters: json!({"type":"object","properties":{"worker_id":{"type":"string"},"plan_id":{"type":"string","description":"Only the workers dispatched on this board plan."}}}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(10_000),
            },
            HostToolSpec {
                name: "team_stop".to_string(),
                description: "Cancel a worker's live session through TeamManager while retaining its record, branch, worktree, and commits. Use the exact ID returned by dispatch or team_status.".to_string(),
                parameters: json!({"type":"object","properties":{"worker_id":{"type":"string"}},"required":["worker_id"]}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(10_000),
            },
            HostToolSpec {
                name: "team_resume".to_string(),
                description: "Resume a stopped, interrupted, or failed worker from its saved transcript through TeamManager. Returns the same worker ID with a new session generation. To move the worker to another model (it failed, or its model does not fit the work), pass `tier` or `model` (and optionally `reasoning_effort`): an idle worker is restarted on it with its transcript kept; a working one must be stopped first. The resumed worker is idle: send it a team_message to continue.".to_string(),
                parameters: json!({"type":"object","properties":{"worker_id":{"type":"string"},"tier":tier,"model":model,"reasoning_effort":reasoning_effort},"required":["worker_id"]}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(10_000),
            },
            HostToolSpec {
                name: "team_compact".to_string(),
                description: "Request real context compaction for a quiescent worker. A blocked or stopped session is detached and resumed once first. A returned working status means requested, not completed; use team_status and compaction events for the outcome.".to_string(),
                parameters: json!({"type":"object","properties":{"worker_id":{"type":"string"}},"required":["worker_id"]}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(10_000),
            },
            HostToolSpec {
                name: "team_review".to_string(),
                description: "Persist your review of the current worker tip and its complete finding list. Review more than the worker's summary: read the diff unanchored first, then record substantive evidence — intent, execution path, integration, test quality, and residual risk. A passing test count alone is not a review. Re-review after every fix. Example finding: {\"title\":\"main.js uses a default import\",\"detail\":\"game.js exports Game as a named export, so the page fails to load\",\"file\":\"src/main.js\",\"line\":1,\"severity\":\"high\"}.".to_string(),
                parameters: json!({"type":"object","properties":{
                    "worker_id":{"type":"string"},
                    "summary":{"type":"string"},
                    "findings":{"type":"array","items":{
                        "type":"object",
                        "properties":{
                            "id":{"type":"string","description":"Stable id; derived from file+title when omitted. Reuse it when re-reviewing."},
                            "title":{"type":"string"},
                            "detail":{"type":"string","description":"Why it matters."},
                            "file":{"type":"string","description":"Repository-relative path."},
                            "line":{"type":"integer","minimum":0,"description":"1-based line; 0 or omitted for file-wide."},
                            "severity":{"type":"string","enum":crate::mnemo2::phase3::FINDING_SEVERITIES,"description":"Every open finding blocks the merge, whatever its severity."},
                            "status":{"type":"string","enum":crate::mnemo2::phase3::FINDING_STATUSES,"default":"open"},
                            "evidence":{"type":"string","description":"Required when status is fixed."},
                            "justification":{"type":"string","description":"Required when status is wontfix."}
                        },
                        "required":["title","detail","file","severity"]
                    }},
                    "evidence":{"type":"object","properties":{
                        "unanchored_pass":{"type":"boolean"},
                        "intent":{"type":"string"},
                        "execution":{"type":"string"},
                        "integration":{"type":"string"},
                        "test_quality":{"type":"string"},
                        "residual_risk":{"type":"string"},
                        "independent_reviewer_id":{"type":"string"},
                        "audit_adjudication":{"type":"string"}
                    },"required":["unanchored_pass","intent","execution","integration","test_quality","residual_risk"]}
                },"required":["worker_id","summary","findings","evidence"]}),
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
                description: "Revert a worker's earlier merge into the lead branch, only while that merge has not yet reached main.".to_string(),
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
        if name == "team_dispatch" {
            let (Some(brief), Some(worker_id)) = (
                args.get("brief").and_then(Value::as_str),
                args.get("worker_id").and_then(Value::as_str),
            ) else {
                return ToolResult::error(
                    "team_dispatch requires 'brief' and a caller-chosen stable 'worker_id'",
                );
            };
            let mut model = args
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_string);
            let review_risk = args
                .get("review_risk")
                .and_then(Value::as_str)
                .map(str::to_string);
            let tier = args.get("tier").and_then(Value::as_str).map(str::to_string);
            model = match self.lead_model_choice(tier.as_deref(), model) {
                Ok(Some(model)) => Some(model),
                Ok(None) => match self.tier_required() {
                    Some(required) => return ToolResult::error(required),
                    None => None,
                },
                Err(err) => return ToolResult::error(err),
            };
            let reasoning_effort = args
                .get("reasoning_effort")
                .and_then(Value::as_str)
                .map(str::to_string);
            let plan_id = args
                .get("plan_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let step = args
                .get("step")
                .and_then(Value::as_u64)
                .and_then(|step| usize::try_from(step).ok())
                .unwrap_or(0);
            let result = self
                .dispatch_on_plan(
                    brief.to_string(),
                    model,
                    reasoning_effort,
                    Some(worker_id.to_string()),
                    review_risk,
                    plan_id,
                    step,
                )
                .await
                .map(|(worker, reused)| json!({"worker": worker, "reused": reused}));
            let result = match (result, tier) {
                (Ok(mut value), Some(tier)) => {
                    self.record_tier(worker_id, &tier).await;
                    value["worker"]["tier"] = json!(tier);
                    Ok(value)
                }
                (result, _) => result,
            };
            return worker_result(result, "team_dispatch");
        }
        if name == "team_audit_dispatch" {
            let Some(worker_id) = args.get("worker_id").and_then(Value::as_str) else {
                return ToolResult::error("team_audit_dispatch requires a 'worker_id' string");
            };
            let Some(reviewer_id) = args.get("reviewer_id").and_then(Value::as_str) else {
                return ToolResult::error("team_audit_dispatch requires a 'reviewer_id' string");
            };
            let model = match self.lead_model_choice(
                None,
                args.get("model").and_then(Value::as_str).map(str::to_string),
            ) {
                Ok(model) => model,
                Err(err) => return ToolResult::error(err),
            };
            let reasoning_effort = args
                .get("reasoning_effort")
                .and_then(Value::as_str)
                .map(str::to_string);
            let brief = args
                .get("brief")
                .and_then(Value::as_str)
                .map(str::to_string);
            let result = self
                .audit_dispatch(
                    worker_id,
                    reviewer_id,
                    model,
                    reasoning_effort,
                    brief,
                )
                .await
                .map(|(worker, reused)| json!({"worker": worker, "reused": reused}));
            return worker_result(result, "team_audit_dispatch");
        }
        if name == "team_status" {
            let plan_id = args
                .get("plan_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty());
            let in_plan = |worker: &WorkerRecord| {
                plan_id.is_none_or(|id| worker.plan_id.as_deref() == Some(id))
            };
            let result = match args.get("worker_id").and_then(Value::as_str) {
                Some(id) => self
                    .worker(id)
                    .await
                    .map(|worker| vec![worker].into_iter().filter(|w| in_plan(w)).collect()),
                None => Ok(self
                    .snapshot()
                    .await
                    .workers
                    .into_iter()
                    .filter(|w| in_plan(w))
                    .collect::<Vec<_>>()),
            };
            return worker_result(result, "team_status");
        }
        let Some(worker_id) = args.get("worker_id").and_then(Value::as_str) else {
            return ToolResult::error(format!("{name} requires a 'worker_id' string"));
        };
        let result = match name {
            "team_message" => match args.get("text").and_then(Value::as_str) {
                Some(text) => self.message(worker_id, text.to_string()).await,
                None => Err("team_message requires a 'text' string".to_string()),
            },
            "team_stop" => self.stop(worker_id).await,
            "team_resume" => {
                let tier = args.get("tier").and_then(Value::as_str).map(str::to_string);
                let model = args.get("model").and_then(Value::as_str).map(str::to_string);
                let reasoning_effort = args
                    .get("reasoning_effort")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                if tier.is_none() && model.is_none() && reasoning_effort.is_none() {
                    self.resume(worker_id).await
                } else {
                    match self.lead_model_choice(tier.as_deref(), model) {
                        Err(err) => Err(err),
                        Ok(model) => {
                            // An effort alone keeps the worker's model.
                            let model = match model {
                                Some(model) => Some(model),
                                None => self.worker(worker_id).await.ok().and_then(|w| w.model),
                            };
                            let changed =
                                self.change_model(worker_id, model, reasoning_effort).await;
                            match (changed, tier) {
                                (Ok(_), Some(tier)) => {
                                    self.record_tier(worker_id, &tier).await;
                                    self.worker(worker_id).await
                                }
                                (changed, _) => changed,
                            }
                        }
                    }
                }
            }
            "team_compact" => self.compact(worker_id).await,
            "team_review" => {
                let summary = args.get("summary").and_then(Value::as_str).unwrap_or("");
                let evidence = args
                    .get("evidence")
                    .cloned()
                    .ok_or_else(|| {
                        "team_review requires a complete 'evidence' object with unanchored_pass, intent, execution, integration, test_quality, and residual_risk"
                            .to_string()
                    })
                    .and_then(|value| {
                        serde_json::from_value::<ReviewEvidence>(value)
                            .map_err(|err| format!("invalid review evidence: {err}"))
                    });
                let findings = phase3::parse_findings(
                    &args.get("findings").cloned().unwrap_or_else(|| json!([])),
                );
                match (evidence, findings) {
                    (Ok(evidence), Ok(findings)) => {
                        self.review(worker_id, summary.to_string(), findings, evidence)
                            .await
                    }
                    (Ok(_), Err(err)) => Err(format!("invalid findings: {err}")),
                    (Err(err), _) => Err(err),
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
            // The lead's own turn is parked on this call, so its busy flag
            // does not mean it is editing the branch.
            "team_merge" => self.merge_as(worker_id, true).await,
            "team_revert_merge" => self.revert_merge_as(worker_id, true).await,
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
        save_workers(&self.records, &self.usage, &self.save_lock, &self.persistence).await
    }
    /// The one production dispatch path: a task on a plan step (1-based),
    /// checked against the board before anything is provisioned. The lead's
    /// `team_dispatch` tool and the Control Room form both land here.
    pub async fn dispatch_on_plan(
        &self,
        brief: String,
        model: Option<String>,
        reasoning_effort: Option<String>,
        worker_id: Option<String>,
        review_risk: Option<String>,
        plan_id: &str,
        step: usize,
    ) -> Result<(WorkerRecord, bool), String> {
        let plan_id = plan_id.trim();
        if plan_id.is_empty() || step < 1 {
            return Err(
                "team_dispatch requires 'plan_id' (a board plan id) and 'step' (a 1-based integer). Create the plan first with plan_create."
                    .to_string(),
            );
        }
        let plan = PlanRef {
            plan_id: plan_id.to_string(),
            step: step - 1,
        };
        self.dispatch_request(
            brief,
            model,
            reasoning_effort,
            worker_id,
            Some(plan),
            review_risk,
        )
        .await
    }

    /// Plan-free dispatch, kept for tests that exercise worker lifecycle only.
    #[cfg(test)]
    pub async fn dispatch(
        &self,
        brief: String,
        model: Option<String>,
    ) -> Result<WorkerRecord, String> {
        self.dispatch_with_id(brief, model, None, None).await
    }

    #[cfg(test)]
    pub async fn dispatch_with_id(
        &self,
        brief: String,
        model: Option<String>,
        worker_id: Option<String>,
        review_risk: Option<String>,
    ) -> Result<WorkerRecord, String> {
        self.dispatch_request(
            brief,
            model,
            None,
            worker_id,
            None,
            review_risk,
        )
        .await
        .map(|(worker, _)| worker)
    }

    #[cfg(test)]
    pub async fn dispatch_task(
        &self,
        brief: String,
        model: Option<String>,
        reasoning_effort: Option<String>,
        worker_id: Option<String>,
    ) -> Result<WorkerRecord, String> {
        self.dispatch_request(
            brief,
            model,
            reasoning_effort,
            worker_id,
            None,
            None,
        )
            .await
            .map(|(worker, _)| worker)
    }

    async fn dispatch_request(
        &self,
        brief: String,
        model: Option<String>,
        reasoning_effort: Option<String>,
        worker_id: Option<String>,
        plan: Option<PlanRef>,
        review_risk: Option<String>,
    ) -> Result<(WorkerRecord, bool), String> {
        let result = self
            .dispatch_locked(
                brief,
                model,
                reasoning_effort,
                worker_id,
                plan,
                review_risk,
            )
            .await;
        if matches!(result, Ok((_, false))) {
            self.signal_board_changed();
        }
        result
    }

    async fn dispatch_locked(
        &self,
        brief: String,
        model: Option<String>,
        reasoning_effort: Option<String>,
        worker_id: Option<String>,
        plan: Option<PlanRef>,
        review_risk: Option<String>,
    ) -> Result<(WorkerRecord, bool), String> {
        let _guard = self.merge_lock.lock().await;
        if brief.trim().is_empty() {
            return Err("the worker brief is empty".to_string());
        }
        // High-risk work needs observable acceptance scenarios on the
        // dispatched board plan, which the lead verifies at closing
        // integration; standard dispatch needs none.
        let review_risk = match review_risk.as_deref() {
            None | Some("standard") => "standard".to_string(),
            Some("high") => {
                let plan = plan.as_ref().ok_or_else(|| {
                    "review_risk='high' requires a board plan step: dispatch with plan_id and step on a plan that declares acceptance scenarios".to_string()
                })?;
                let doc = self.plan_document(&plan.plan_id)?;
                require_acceptance_scenarios(&plan.plan_id, &doc)?;
                "high".to_string()
            }
            Some(other) => {
                return Err(format!(
                    "review_risk must be 'standard' or 'high', not '{other}'"
                ));
            }
        };
        if let Some(id) = worker_id.as_deref() {
            validate_worker_id(id)?;
            let existing = {
                let records = self.records.lock().await;
                records.iter().find(|worker| worker.id == id).cloned()
            };
            if let Some(existing) = existing {
                if existing.brief != brief
                    || existing.model != model
                    || existing.reasoning_effort != reasoning_effort
                    || plan.as_ref().is_some_and(|plan| {
                        existing.plan_id.as_deref() != Some(plan.plan_id.as_str())
                            || existing.plan_step != Some(plan.step)
                    })
                {
                    return Err(format!(
                        "worker '{id}' already exists with a different brief, model, reasoning effort, or plan step; use team_status instead of guessing a new ID (to move it to another model, use team_resume with `tier` or `model`)"
                    ));
                }
                if existing.branch != agent_branch(id) {
                    return Err(format!("worker '{id}' has an invalid branch identity"));
                }
                self.validate_model(&existing.model, &existing.reasoning_effort)?;
                if !existing.brief_sent
                    && !self.live.lock().await.contains_key(id)
                    && matches!(
                        existing.status,
                        WorkerStatus::Dispatching
                            | WorkerStatus::Failed
                            | WorkerStatus::Interrupted
                    )
                {
                    if !existing.worktree.is_dir() {
                        return Err(format!("worker '{id}' has no recoverable worktree"));
                    }
                    return self
                        .start_worker(existing)
                        .await
                        .map(|worker| (worker, true));
                }
                return Ok((existing, true));
            }
        }
        // The plan board refuses before any record, branch, or worktree exists.
        let opening = match &plan {
            Some(plan) => {
                let board = self
                    .board()
                    .ok_or_else(|| "the plan board is not available".to_string())?;
                let header = board.brief_for(&plan.plan_id, plan.step + 1)?;
                // Steps of this plan with an open task, other than this one:
                // parallel work, which needs contracts for shared files.
                let in_flight = self.in_flight_steps(&plan.plan_id, plan.step).await;
                board.contract_gate(&plan.plan_id, plan.step, &in_flight)?;
                Some(format!("{}\n---\n\n{brief}", header.trim_end()))
            }
            None => None,
        };
        self.validate_model(&model, &reasoning_effort)?;
        if let Some(id) = worker_id.as_deref() {
            let branch = agent_branch(id);
            if self
                .records
                .lock()
                .await
                .iter()
                .any(|worker| worker.branch == branch)
            {
                return Err(format!(
                    "worker ID prefix collides with an existing {branch}"
                ));
            }
        }
        let active = self.live.lock().await.len();
        let slots = self.persistence.slots.load(Ordering::SeqCst);
        if active >= slots {
            return Err(format!("all {slots} worker slots are busy"));
        }
        let placed = plan.as_ref().map(|plan| (plan.plan_id.clone(), plan.step));
        let result = self
            .provision_worker(
                brief,
                opening,
                model,
                reasoning_effort,
                worker_id,
                review_risk,
                plan,
            )
            .await;
        if result.is_ok() {
            if let Some((plan_id, step)) = &placed {
                // Parallel work: the seams between the steps need a final
                // integration check before the plan can close. Best-effort;
                // the dispatch already succeeded.
                let in_flight = self.in_flight_steps(plan_id, *step).await;
                if !in_flight.is_empty() {
                    if let Some(board) = self.board() {
                        let mut parallel = in_flight;
                        parallel.push(*step);
                        if let Err(e) = board.ensure_integration_step(plan_id, &parallel) {
                            eprintln!("team: integration step for {plan_id} not added: {e}");
                        }
                    }
                }
            }
        }
        result.map(|worker| (worker, false))
    }


    /// Dispatch one fresh read-only independent audit of an implementation
    /// worker's current branch/tip.
    ///
    /// The audit reuses the tracked session/telemetry lifecycle: it gets its
    /// own worktree checked out at the audited branch, its own ledger record
    /// (`kind: "audit"`, no plan step), and the standard worker event pump.
    /// Its opening brief carries the implementation worker's board plan, the
    /// lead's intent brief, and the target diff — never the implementation
    /// worker's own summary, findings, or review — and its tool selection is
    /// read-only by construction: no file mutation, shell, git writes,
    /// spawning, user questions, or plan-step ticks.
    ///
    /// Idempotency: retrying the same `reviewer_id` for the same worker and
    /// the same tip reuses (and restarts, when needed) the existing audit;
    /// a moved tip needs a fresh `reviewer_id` so the next audit has fresh
    /// context.
    pub async fn audit_dispatch(
        &self,
        worker_id: &str,
        reviewer_id: &str,
        model: Option<String>,
        reasoning_effort: Option<String>,
        brief: Option<String>,
    ) -> Result<(WorkerRecord, bool), String> {
        let _guard = self.merge_lock.lock().await;
        validate_worker_id(reviewer_id)?;
        if let Some(effort) = reasoning_effort.as_deref() {
            if !matches!(effort, "max" | "high" | "medium" | "low" | "minimal" | "none") {
                return Err(format!(
                    "reasoning_effort '{effort}' is not one of max/high/medium/low/minimal/none"
                ));
            }
        }
        let target = self.worker(worker_id).await?;
        if target.kind != "implementation" {
            return Err(format!(
                "worker '{worker_id}' is not an implementation worker; only implementation work is audited"
            ));
        }
        if matches!(target.status, WorkerStatus::Merged | WorkerStatus::Merging) {
            return Err("a merged worker no longer needs an audit; its review already landed".to_string());
        }
        if self.worker_is_busy(worker_id).await {
            return Err("the implementation worker must be idle before audit dispatch".to_string());
        }
        let tip = self.worker_tip(&target).await?;
        // Reuse semantics: the same reviewer may be retried for the same
        // worker at the same tip; a moved tip needs a fresh reviewer id so
        // the stale audit cannot be relinked at the new tip.
        {
            let existing = {
                let records = self.records.lock().await;
                records
                    .iter()
                    .find(|worker| worker.id == reviewer_id)
                    .cloned()
            };
            if let Some(existing) = existing {
                if existing.kind != "audit" {
                    return Err(format!(
                        "worker '{reviewer_id}' already exists as an implementation worker; choose a different reviewer_id"
                    ));
                }
                if existing.audit_target.as_deref() != Some(worker_id) {
                    return Err(format!(
                        "audit worker '{reviewer_id}' already reviews another implementation worker; choose a different reviewer_id"
                    ));
                }
                if existing.audit_tip.as_deref() != Some(tip.as_str()) {
                    return Err(format!(
                        "audit worker '{reviewer_id}' inspected an older tip of worker '{worker_id}'; the fix commit moved the tip — dispatch a fresh audit with a new reviewer_id"
                    ));
                }
                self.validate_model(&existing.model, &existing.reasoning_effort)?;
                if !existing.brief_sent
                    && !self.live.lock().await.contains_key(reviewer_id)
                    && matches!(
                        existing.status,
                        WorkerStatus::Dispatching | WorkerStatus::Failed | WorkerStatus::Interrupted
                    )
                {
                    if !existing.worktree.is_dir() {
                        return Err(format!(
                            "audit worker '{reviewer_id}' has no recoverable worktree"
                        ));
                    }
                    return self
                        .start_worker(existing)
                        .await
                        .map(|worker| (worker, true));
                }
                return Ok((existing, true));
            }
        }
        // Without a named model an effort cannot be applied to a session; it
        // stays in the audit brief as the depth the lead asked for.
        let depth_hint = reasoning_effort.clone();
        let reasoning_effort = reasoning_effort.filter(|_| model.is_some());
        self.validate_model(&model, &reasoning_effort)?;
        let branch = agent_branch(reviewer_id);
        if self
            .records
            .lock()
            .await
            .iter()
            .any(|worker| worker.branch == branch)
        {
            return Err(format!("reviewer ID prefix collides with an existing {branch}"));
        }
        // The audit reviews a plan step's change: the implementation
        // worker's board plan is its context, alongside the lead's intent
        // and the target diff.
        let plan_id = target.plan_id.clone().ok_or_else(|| {
            format!(
                "worker '{worker_id}' was not dispatched on a board plan; audits review a plan step's change, so only workers dispatched with plan_id and step can be audited"
            )
        })?;
        let plan_doc = self.plan_document(&plan_id)?;
        let audit_brief = audit_brief_text(
            &self.workspace_root,
            &target,
            &tip,
            &plan_id,
            &plan_doc,
            brief.as_deref(),
            depth_hint.as_deref(),
        )
        .await?;
        let active = self.live.lock().await.len();
        let slots = self.persistence.slots.load(Ordering::SeqCst);
        if active >= slots {
            return Err(format!("all {slots} worker slots are busy"));
        }
        // The audit worktree checks out the audited branch itself so the
        // reviewer reads the exact tree it is auditing.
        let (worktree, branch_created) =
            provision_agent_worktree(self.workspace_root.clone(), reviewer_id.to_string(), target.branch.clone())
                .await
                .map_err(|err| format!("could not provision the audit worktree: {err}"))?;
        let base_tip = phase3::tip(&worktree).await?;
        let millis = phase3::now_ms();
        let record = WorkerRecord {
            id: reviewer_id.to_string(),
            name: format!("Audit {}", agent_short_id(reviewer_id)),
            brief: audit_brief,
            model,
            model_id: None,
            branch: branch_created,
            worktree,
            status: WorkerStatus::Dispatching,
            generation: 0,
            transcript: Transcript::default(),
            pending: None,
            used_tokens: 0,
            max_tokens: 0,
            brief_sent: false,
            compacting: false,
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
            review_risk: "standard".to_string(),
            kind: "audit".to_string(),
            audit_target: Some(worker_id.to_string()),
            audit_tip: Some(tip),
            reasoning_effort,
            findings: Vec::new(),
            review: None,
            fixer_finding_id: None,
            merge: None,
            error: None,
            opening_prompt: None,
            plan_id: None,
            plan_step: None,
            tier: None,
        };
        self.start_audit_worker(record, base_tip)
            .await
            .map(|worker| (worker, false))
    }

    /// Provision and start one audit worker record (the audit counterpart of
    /// [`TeamRuntime::provision_worker`]).
    async fn start_audit_worker(
        &self,
        record: WorkerRecord,
        base_tip: String,
    ) -> Result<WorkerRecord, String> {
        let id = record.id.clone();
        self.records.lock().await.push(record.clone());
        if let Err(err) = self.persist().await {
            self.records.lock().await.retain(|worker| worker.id != id);
            let rollback = mnemo::app::worktrees::rollback_agent_worktree(
                self.workspace_root.clone(),
                id.clone(),
                base_tip,
            )
            .await;
            return Err(match rollback {
                Ok(()) => err,
                Err(rollback) => {
                    let error = format!(
                        "{err}; audit worker '{id}' retains its checkout because rollback was refused: {rollback}"
                    );
                    self.records.lock().await.push(WorkerRecord {
                        status: WorkerStatus::Failed,
                        error: Some(error.clone()),
                        ..record
                    });
                    let _ = self.persist().await;
                    error
                }
            });
        }
        self.start_worker(record).await
    }

    /// The other steps (0-based) of `plan_id` with an open task: a worker not
    /// merged, cancelled, or stopped (the ledger's notion of closed).
    async fn in_flight_steps(&self, plan_id: &str, except_step: usize) -> Vec<usize> {
        let records = self.records.lock().await;
        let mut steps: Vec<usize> = records
            .iter()
            .filter(|w| w.plan_id.as_deref() == Some(plan_id) && !ledger::is_closed(w))
            .filter_map(|w| w.plan_step)
            .filter(|step| *step != except_step)
            .collect();
        steps.sort_unstable();
        steps.dedup();
        steps
    }

    /// Stamp the tier the lead dispatched by onto the worker's record.
    async fn record_tier(&self, worker_id: &str, tier: &str) {
        {
            let mut records = self.records.lock().await;
            match records.iter_mut().find(|w| w.id == worker_id) {
                Some(worker) if worker.tier.as_deref() != Some(tier) => {
                    worker.tier = Some(tier.to_string());
                }
                _ => return,
            }
        }
        let _ = self.persist().await;
    }

    fn validate_model(
        &self,
        model: &Option<String>,
        reasoning_effort: &Option<String>,
    ) -> Result<(), String> {
        let models = self.current_models();
        let configured = match model {
            Some(spec) => Some(
                models
                    .iter()
                    .find(|configured| &configured.spec == spec)
                    .ok_or_else(|| {
                        format!("'{spec}' is not a configured model; choose one from team_dispatch's model list")
                    })?,
            ),
            None => None,
        };
        let Some(effort) = reasoning_effort else {
            return Ok(());
        };
        let Some(configured) = configured else {
            return Err(format!(
                "reasoning_effort '{effort}' needs an explicit model to apply to"
            ));
        };
        if configured.reasoning_efforts.is_empty() {
            return Err(format!(
                "'{}' does not support a reasoning effort; omit reasoning_effort",
                configured.spec
            ));
        }
        if !configured
            .reasoning_efforts
            .iter()
            .any(|allowed| allowed == effort)
        {
            return Err(format!(
                "'{}' does not support reasoning_effort '{effort}'; choose one of: {}",
                configured.spec,
                configured.reasoning_efforts.join(", ")
            ));
        }
        Ok(())
    }

    async fn provision_worker(
        &self,
        brief: String,
        opening_prompt: Option<String>,
        model: Option<String>,
        reasoning_effort: Option<String>,
        worker_id: Option<String>,
        review_risk: String,
        plan: Option<PlanRef>,
    ) -> Result<WorkerRecord, String> {
        let base_ref = current_branch(&self.workspace_root).map_err(|err| {
            format!(
                "cannot dispatch a tracked worker: the selected project is not a usable Git checkout. Open the repository root (the folder containing .git), then retry. Git reported: {err}"
            )
        })?;
        // The lead names its workers (`ghostai0001`): that id is the name. A
        // generated id is opaque, so it shows as "Worker <short id>".
        let (id, name) = match worker_id {
            Some(id) => (id.clone(), id),
            None => {
                let seq = self.next_worker.fetch_add(1, Ordering::SeqCst);
                let millis = phase3::now_ms();
                let id = format!("{seq:08x}{millis:012x}");
                let name = format!("Worker {}", agent_short_id(&id));
                (id, name)
            }
        };
        let millis = phase3::now_ms();
        let (worktree, branch) =
            provision_agent_worktree(self.workspace_root.clone(), id.clone(), base_ref).await?;
        let base_tip = phase3::tip(&worktree).await?;
        let record = WorkerRecord {
            id: id.clone(),
            name,
            brief,
            opening_prompt,
            model,
            model_id: None,
            reasoning_effort,
            branch,
            worktree,
            status: WorkerStatus::Dispatching,
            generation: 0,
            transcript: Transcript::default(),
            pending: None,
            used_tokens: 0,
            max_tokens: 0,
            brief_sent: false,
            compacting: false,
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
            review_risk,
            review: None,

            kind: "implementation".to_string(),
            audit_target: None,
            audit_tip: None,
            findings: Vec::new(),

            fixer_finding_id: None,
            merge: None,
            error: None,
            plan_id: plan.as_ref().map(|plan| plan.plan_id.clone()),
            plan_step: plan.map(|plan| plan.step),
            tier: None,
        };
        self.records.lock().await.push(record.clone());
        if let Err(err) = self.persist().await {
            self.records.lock().await.retain(|worker| worker.id != id);
            let rollback = mnemo::app::worktrees::rollback_agent_worktree(
                self.workspace_root.clone(),
                id.clone(),
                base_tip,
            )
            .await;
            return Err(match rollback {
                Ok(()) => err,
                Err(rollback) => {
                    let error = format!(
                        "{err}; worker '{id}' retains its checkout because rollback was refused: {rollback}"
                    );
                    self.records.lock().await.push(WorkerRecord {
                        status: WorkerStatus::Failed,
                        error: Some(error.clone()),
                        ..record
                    });
                    let _ = self.persist().await;
                    error
                }
            });
        }
        self.start_worker(record).await
    }

    async fn start_worker(&self, record: WorkerRecord) -> Result<WorkerRecord, String> {
        let active = self.live.lock().await.len();
        let slots = self.persistence.slots.load(Ordering::SeqCst);
        if active >= slots {
            return Err(format!("all {slots} worker slots are busy"));
        }
        let id = record.id.clone();
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.update_worker(&id, |worker| {
            worker.generation = generation;
            worker.status = WorkerStatus::Dispatching;
            worker.pending = None;
            worker.error = None;
            worker.compacting = false;
        })
        .await;
        self.persist().await?;
        let session = match self
            .spawn_worker_session(
                record.name.clone(),
                record.model.clone(),
                record.reasoning_effort.clone(),
                record.worktree.clone(),
                Some(record.transcript.clone()),
                record.kind.as_str(),
            )
            .await
        {
            Ok(session) => session,
            Err(err) => {
                let error = format!(
                    "{err}; worker '{id}' retains its checkout; retry with the same worker ID"
                );
                self.fail_worker(&id, error.clone()).await;
                return Err(error);
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
            self.usage.clone(),
            self.save_lock.clone(),
            self.pricing.clone(),
            self.persistence.clone(),
        ));
        if let Err(err) = session
            .send(SessionCommand::Prompt {
                text: record.first_message().to_string(),
                images: Vec::new(),
            })
            .await
        {
            pump.abort();
            let _ = session.close().await;
            let error = format!(
                "could not send the brief to worker '{id}': {err}; retry with the same worker ID"
            );
            self.fail_worker(&id, error.clone()).await;
            return Err(error);
        }
        self.live.lock().await.insert(
            id.clone(),
            LiveWorker {
                session,
                busy,
                pump,
                cleanup_retry_only: false,
            },
        );
        self.update_worker(&id, |worker| {
            worker.status = WorkerStatus::Working;
            worker.generation = generation;
            worker.brief_sent = true;
            worker.error = None;
        })
        .await;
        if let Err(err) = self.persist().await {
            let _ = self.stop_inner(&id).await;
            return Err(format!(
                "worker '{id}' accepted the brief but its ledger update failed: {err}; use the same worker ID and inspect team_status"
            ));
        }
        self.worker(&id).await
    }

    pub(crate) async fn spawn_worker_session(
        &self,
        name: String,
        model: Option<String>,
        reasoning_effort: Option<String>,
        worktree: PathBuf,
        resume: Option<Transcript>,
        kind: &str,
    ) -> Result<AgentSession, String> {
        self.engine
            .spawn(SpawnOptions {
                name: Some(name),
                model,
                reasoning_effort,
                worktree: Some(worktree),
                permissions: Some(PermissionPolicy::OverridePermissions),
                policy: None,
                resume,
                limits: None,
                host_tools: self.worker_host_tools(),
                external_policy: None,
                guardrails: Some(guardrails_for(kind)),
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
            return Err(
                "a cleaned worker cannot accept new work; review its retained branch".to_string(),
            );
        }
        if record.compacting {
            return Err(
                "wait for the worker's compaction to finish before messaging it".to_string(),
            );
        }
        let (busy, idle, sent) = {
            let live = self.live.lock().await;
            let live = live
                .get(worker_id)
                .ok_or_else(|| format!("worker '{worker_id}' is not running; resume it first"))?;
            live.admit_session_command(worker_id)?;
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
                SessionCommand::Steer {
                    text,
                    images: Vec::new(),
                }
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
            live.admit_session_command(worker_id)?;
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
            live.admit_session_command(worker_id)?;
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
        self.update_worker(worker_id, |worker| {
            worker.status = WorkerStatus::Stopped;
            worker.pending = None;
            worker.fixer_finding_id = None;
            worker.compacting = false;
        })
        .await;
        let live = self.live.lock().await.remove(worker_id);
        if let Some(mut live) = live {
            let snapshot = live.session.snapshot().await.ok();
            live.abort();
            if let Some(snapshot) = snapshot {
                self.update_worker(worker_id, |worker| {
                    if !snapshot.transcript.messages.is_empty() {
                        worker.transcript = snapshot.transcript.clone();
                        worker.used_tokens = snapshot.used_tokens;
                        worker.max_tokens = snapshot.max_tokens;
                        if worker.model_id.is_none() {
                            worker.model_id = snapshot_model(&snapshot);
                        }
                    } else if worker.transcript.messages.is_empty() && worker.brief_sent {
                        worker.transcript = Transcript {
                            messages: vec![mnemo::provider::Message::user_text(
                                worker.first_message().to_string(),
                            )],
                        };
                    }
                })
                .await;
            }
            if let Err(error) = self.close_session_for_cleanup(&live.session).await {
                let message = format!("could not close worker session: {error}");
                live.cleanup_retry_only = true;
                self.live.lock().await.insert(worker_id.to_string(), live);
                self.update_worker(worker_id, |worker| {
                    worker.error = Some(message.clone());
                })
                .await;
                self.persist().await?;
                return Err(message);
            }
        }
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
        let live = self.live.lock().await;
        if let Some(live) = live.get(worker_id) {
            live.admit_session_command(worker_id)?;
            return Err(format!("worker '{worker_id}' is already running"));
        }
        drop(live);
        let active = self.live.lock().await.len();
        let slots = self.persistence.slots.load(Ordering::SeqCst);
        if active >= slots {
            return Err(format!("all {slots} worker slots are busy"));
        }
        let record = self.worker(worker_id).await?;
        if record.status == WorkerStatus::Cancelled {
            return Err(
                "a cleaned worker cannot be resumed; review its retained branch".to_string(),
            );
        }
        if matches!(record.status, WorkerStatus::Merged | WorkerStatus::Merging) {
            return Err("a merged worker cannot be resumed".to_string());
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
                record.reasoning_effort.clone(),
                record.worktree.clone(),
                Some(record.transcript.clone()),
                record.kind.as_str(),
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
            self.usage.clone(),
            self.save_lock.clone(),
            self.pricing.clone(),
            self.persistence.clone(),
        ));
        self.live.lock().await.insert(
            worker_id.to_string(),
            LiveWorker {
                session,
                busy,
                pump,
                cleanup_retry_only: false,
            },
        );
        self.update_worker(worker_id, |worker| {
            worker.status = WorkerStatus::Idle;
            worker.generation = generation;
            worker.pending = None;
            worker.error = None;
            worker.compacting = false;
        })
        .await;
        self.persist().await?;
        self.worker(worker_id).await
    }

    /// Move a worker to another model (and reasoning effort), keeping its
    /// transcript, branch, and worktree. An idle live session is restarted
    /// on the new model; a stopped, interrupted, or failed one is resumed on
    /// it; a worker whose brief never reached a session is started with it.
    /// A working worker is refused: stop or pause it first.
    pub async fn change_model(
        &self,
        worker_id: &str,
        model: Option<String>,
        reasoning_effort: Option<String>,
    ) -> Result<WorkerRecord, String> {
        let _guard = self.merge_lock.lock().await;
        let record = self.worker(worker_id).await?;
        if record.status == WorkerStatus::Cancelled {
            return Err("a cleaned worker cannot change its model".to_string());
        }
        if matches!(record.status, WorkerStatus::Merged | WorkerStatus::Merging) {
            return Err("a merged worker cannot change its model".to_string());
        }
        self.validate_model(&model, &reasoning_effort)?;
        if self.worker_is_busy(worker_id).await {
            return Err(format!(
                "worker '{worker_id}' is working; pause or stop it before changing its model"
            ));
        }
        if self.live.lock().await.contains_key(worker_id) {
            self.stop_inner(worker_id).await?;
        }
        self.update_worker(worker_id, |worker| {
            worker.model = model.clone();
            worker.model_id = None;
            worker.reasoning_effort = reasoning_effort.clone();
            worker.tier = None;
        })
        .await;
        self.persist().await?;
        let record = self.worker(worker_id).await?;
        if !record.brief_sent {
            if !record.worktree.is_dir() {
                return Err(format!("worker '{worker_id}' has no recoverable worktree"));
            }
            return self.start_worker(record).await;
        }
        self.resume_inner(worker_id).await
    }

    /// Compact a quiescent worker. Blocked or stopped sessions are resumed once.
    pub async fn compact(&self, worker_id: &str) -> Result<WorkerRecord, String> {
        let _guard = self.merge_lock.lock().await;
        let mut record = self.worker(worker_id).await?;
        if record.compacting {
            return Err(format!("worker '{worker_id}' is already compacting"));
        }
        if record.status == WorkerStatus::NeedsYou && self.live.lock().await.contains_key(worker_id)
        {
            self.stop_inner(worker_id).await?;
            record = self.worker(worker_id).await?;
        }
        if !self.live.lock().await.contains_key(worker_id) {
            if !matches!(
                record.status,
                WorkerStatus::Stopped
                    | WorkerStatus::Interrupted
                    | WorkerStatus::Paused
                    | WorkerStatus::Completed
                    | WorkerStatus::Failed
                    | WorkerStatus::NeedsYou
                    | WorkerStatus::Idle
            ) {
                return Err(format!(
                    "worker '{worker_id}' cannot be compacted in its current state"
                ));
            }
            record = self.resume_inner(worker_id).await?;
        }
        let busy = {
            let live = self.live.lock().await;
            let live = live
                .get(worker_id)
                .ok_or_else(|| format!("worker '{worker_id}' is not running"))?;
            live.admit_session_command(worker_id)?;
            if live.busy.swap(true, Ordering::SeqCst) {
                return Err(format!(
                    "worker '{worker_id}' is busy; compact it when it becomes idle"
                ));
            }
            live.busy.clone()
        };
        let previous = record;
        self.update_worker(worker_id, |worker| {
            worker.compacting = true;
            worker.status = WorkerStatus::Working;
            worker.error = None;
        })
        .await;
        if let Err(error) = self.persist().await {
            busy.store(false, Ordering::SeqCst);
            self.update_worker(worker_id, |worker| *worker = previous.clone())
                .await;
            let _ = self.persist().await;
            return Err(error);
        }
        let sent = {
            let live = self.live.lock().await;
            match live.get(worker_id) {
                Some(live) => Some(live.session.send(SessionCommand::Compact).await),
                None => None,
            }
        };
        let Some(sent) = sent else {
            busy.store(false, Ordering::SeqCst);
            self.update_worker(worker_id, |worker| *worker = previous.clone())
                .await;
            self.persist().await?;
            return Err(format!("worker '{worker_id}' stopped before compaction"));
        };
        if let Err(error) = sent {
            busy.store(false, Ordering::SeqCst);
            self.update_worker(worker_id, |worker| *worker = previous)
                .await;
            self.persist().await?;
            return Err(format!("could not compact the worker: {error}"));
        }
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
                &[
                    "rev-parse",
                    &format!("refs/heads/{}^{{commit}}", worker.branch),
                ],
            )
            .await;
        }
        phase3::tip(&worker.worktree).await
    }

    /// Persist a lead review against the current worker commit.
    ///
    /// Every new review carries complete structured evidence: an unanchored
    /// diff pass plus substantive intent, execution, integration, test-quality,
    /// and residual-risk assessments. High-risk reviews additionally require a
    /// completed fresh independent audit (`independent_reviewer_id` plus a
    /// written `audit_adjudication`) linked at the exact reviewed tip.
    pub async fn review(
        &self,
        worker_id: &str,
        summary: String,
        findings: Vec<Finding>,
        evidence: ReviewEvidence,
    ) -> Result<WorkerRecord, String> {
        let _guard = self.merge_lock.lock().await;
        if summary.trim().is_empty() {
            return Err("the review summary is empty".to_string());
        }
        let worker = self.worker(worker_id).await?;
        if worker.kind == "audit" {
            return Err(
                "an audit worker is a read-only reviewer and is not reviewed; adjudicate it by recording team_review on its implementation worker with independent_reviewer_id".to_string(),
            );
        }
        // High-risk reviews require a completed, fresh independent audit at
        // the exact reviewed tip; every linked reviewer is validated against
        // the live ledger so a stale or foreign audit can never be attached.
        let require_audit = worker.review_risk == "high";
        phase3::validate_review_evidence(&evidence, require_audit)?;
        phase3::validate_findings(&findings)?;
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
        let records = self.records.lock().await.clone();
        validate_audit_linkage(&records, &evidence, worker_id, &tip)?;
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
                evidence: Some(evidence),
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
        self.merge_as(worker_id, false).await
    }

    /// `by_lead` is true when the lead itself asked, from inside its turn.
    async fn merge_as(&self, worker_id: &str, by_lead: bool) -> Result<WorkerRecord, String> {
        let worker = self.merge_locked(worker_id, by_lead).await?;
        self.after_merge(&worker).await;
        self.signal_board_changed();
        Ok(worker)
    }

    async fn merge_locked(&self, worker_id: &str, by_lead: bool) -> Result<WorkerRecord, String> {
        let _guard = self.merge_lock.lock().await;
        if !by_lead && self.lead_busy.load(Ordering::SeqCst) {
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
        phase3::require_clean_lead(&self.workspace_root, "lead").await?;
        let worker = self.worker(worker_id).await?;
        if worker.kind == "audit" {
            return Err(
                "an audit worker is a read-only reviewer and is never merged; adjudicate it on its implementation worker's review".to_string(),
            );
        }
        phase3::require_clean(&worker.worktree, "worker").await?;
        let tip = phase3::tip(&worker.worktree).await?;
        let records = self.records.lock().await.clone();
        validate_merge(&worker, &tip, &records)?;
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
                phase3::require_clean_lead(&self.workspace_root, "lead after conflict").await
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
        if let Some(mut live) = live {
            live.abort();
            if let Err(error) = self.close_session_for_cleanup(&live.session).await {
                live.cleanup_retry_only = true;
                self.live.lock().await.insert(worker_id.to_string(), live);
                self.update_worker(worker_id, |worker| {
                    // The Git merge landed, but keep the worker retryable:
                    // `team.merge` skips Merged records and would otherwise
                    // never retry session shutdown or checkout cleanup.
                    worker.status = WorkerStatus::InReview;
                    worker.merge = None;
                    worker.error = Some(format!(
                        "merge was committed, but cleanup needs attention; retry merge after closing the worker session: {error}"
                    ));
                })
                .await;
                self.persist().await?;
                return self.worker(worker_id).await;
            }
        }
        if let Err(err) = cleanup_agent_worktree(&self.workspace_root, &worker).await {
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

    /// Revert a worker merge on the lead branch before it reaches main.
    pub async fn revert_merge(&self, worker_id: &str) -> Result<WorkerRecord, String> {
        self.revert_merge_as(worker_id, false).await
    }

    async fn revert_merge_as(
        &self,
        worker_id: &str,
        by_lead: bool,
    ) -> Result<WorkerRecord, String> {
        let worker = self.revert_locked(worker_id, by_lead).await?;
        self.after_revert(&worker).await;
        self.signal_board_changed();
        Ok(worker)
    }

    async fn revert_locked(&self, worker_id: &str, by_lead: bool) -> Result<WorkerRecord, String> {
        let _guard = self.merge_lock.lock().await;
        if !by_lead && self.lead_busy.load(Ordering::SeqCst) {
            return Err("the lead must be idle before revert".to_string());
        }
        phase3::require_clean_lead(&self.workspace_root, "lead").await?;
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
            return Err("the worker merge has already reached main".to_string());
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

    /// After a merge: tick the worker's plan step once its tasks are all
    /// closed with at least one merged, then try to close the plan. Board
    /// errors never fail the merge; they go to the lead in the notice.
    async fn after_merge(&self, worker: &WorkerRecord) {
        let (Some(plan_id), Some(step)) = (worker.plan_id.as_deref(), worker.plan_step) else {
            return;
        };
        let Some(board) = self.board() else {
            return;
        };
        let _guard = self.merge_lock.lock().await;
        let tasks: Vec<WorkerRecord> = self
            .records
            .lock()
            .await
            .iter()
            .filter(|w| w.plan_id.as_deref() == Some(plan_id) && w.plan_step == Some(step))
            .cloned()
            .collect();
        let number = step + 1;
        let mut lines = Vec::new();
        if !tasks.iter().any(ledger::merged) {
            return;
        }
        if tasks.iter().any(|w| !ledger::is_closed(w)) {
            return;
        }
        let outcome = merge_outcome_line(number, worker);
        let mut notice = None;
        match board.tick(plan_id, step, &outcome) {
            Ok(true) => {
                lines.push(format!("Plan {plan_id} step {number} is done: {outcome}"));
                notice = Some(format!(
                    "tick:{}:{}",
                    worker.id,
                    worker.merge.as_ref().map_or("", |m| m.commit.as_str())
                ));
            }
            Ok(false) => {}
            Err(err) => {
                lines.push(format!(
                    "Plan {plan_id} step {number} could not be ticked: {err}"
                ));
                notice = Some(format!("tick-error:{}:{}", worker.id, now_key()));
            }
        }
        let ledger = RecordsLedger::new(self.records.clone());
        // `tick` released the board lock; closing reads the records again.
        match board.try_close(plan_id, &ledger as &dyn TeamLedger).await {
            CloseOutcome::Closed {
                report_path,
                retired_notes,
            } => {
                let relative = project_relative(&self.workspace_root, &report_path);
                lines.push(format!(
                    "Plan {plan_id} is closed. Review report: {relative}"
                ));
                if retired_notes > 0 {
                    lines.push(format!(
                        "{retired_notes} plan-scoped memory note(s) were retired into that report. Read them and re-store any durable fact without plan or branch references."
                    ));
                }
                if self.external_lead_attached() {
                    lines.push(EXTERNAL_LEAD_MEMORY_REMINDER.to_string());
                }
                notice = Some(format!("closed:{plan_id}:{relative}"));
            }
            CloseOutcome::Blocked { reasons } => {
                lines.push(format!(
                    "Plan {plan_id} cannot close yet: {}",
                    reasons.join("; ")
                ));
                notice = Some(format!("blocked:{plan_id}:{}", reasons.join("|")));
            }
            CloseOutcome::NotReady { .. } => {}
        }
        if let Some(key) = notice {
            self.notify_lead(
                format!("board:{key}"),
                format!(
                    "{}\nUse plan_show for the plan's current state.",
                    lines.join("\n")
                ),
            );
        }
    }

    /// After a revert: reopen the step when no other merged task remains.
    async fn after_revert(&self, worker: &WorkerRecord) {
        let (Some(plan_id), Some(step)) = (worker.plan_id.as_deref(), worker.plan_step) else {
            return;
        };
        let Some(board) = self.board() else {
            return;
        };
        let _guard = self.merge_lock.lock().await;
        let still_merged = self.records.lock().await.iter().any(|w| {
            w.plan_id.as_deref() == Some(plan_id) && w.plan_step == Some(step) && ledger::merged(w)
        });
        if still_merged {
            return;
        }
        let number = step + 1;
        let note = format!(
            "{} ({}) was reverted, so no merged task remains for it",
            worker.name, worker.id
        );
        let text = match board.untick(plan_id, step, &note) {
            Ok(true) => format!("Plan {plan_id} step {number} is reopened: {note}"),
            Ok(false) => return,
            Err(err) => format!("Plan {plan_id} step {number} could not be reopened: {err}"),
        };
        self.notify_lead(
            format!("board:untick:{}:{}", worker.id, now_key()),
            format!("{text}\nUse plan_show for the plan's current state."),
        );
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

    /// Describe what deleting these workers would remove, changing nothing.
    pub async fn deletion_preview(
        &self,
        worker_ids: &[String],
    ) -> Result<Vec<WorkerDeletion>, String> {
        let _guard = self.merge_lock.lock().await;
        let mut previews = Vec::with_capacity(worker_ids.len());
        for worker_id in worker_ids {
            previews.push(self.inspect_deletion(worker_id, worker_ids).await?);
        }
        Ok(previews)
    }

    /// Delete workers and their history: the ledger record (brief,
    /// transcript, telemetry, review, findings), inbox messages, checkout,
    /// and `wt/agent-*` branch. A live session is stopped first. Every
    /// worker is checked before any is deleted; work the lead branch lacks
    /// (uncommitted changes, unmerged commits) is discarded only when
    /// `discard_work` confirms it.
    pub async fn delete_workers(
        &self,
        worker_ids: &[String],
        discard_work: bool,
    ) -> Result<TeamSnapshot, String> {
        let _guard = self.merge_lock.lock().await;
        for worker_id in worker_ids {
            self.check_deletion(worker_id, worker_ids, discard_work)
                .await?;
        }
        for worker_id in worker_ids {
            if self.live.lock().await.contains_key(worker_id) {
                self.stop_inner(worker_id).await?;
            }
        }
        // Re-check after stopping: a session may have written more work
        // between the preview and its stop.
        let mut deletions = Vec::with_capacity(worker_ids.len());
        for worker_id in worker_ids {
            deletions.push(
                self.check_deletion(worker_id, worker_ids, discard_work)
                    .await?,
            );
        }
        for deletion in deletions {
            self.delete_one(&deletion).await?;
        }
        Ok(self.snapshot().await)
    }

    async fn check_deletion(
        &self,
        worker_id: &str,
        batch: &[String],
        discard_work: bool,
    ) -> Result<WorkerDeletion, String> {
        let deletion = self.inspect_deletion(worker_id, batch).await?;
        if let Some(reason) = &deletion.blocked {
            return Err(format!("{} cannot be deleted: {reason}", deletion.name));
        }
        if deletion.discards_work() && !discard_work {
            return Err(format!(
                "{} has unmerged or uncommitted work on {}; confirm discarding it to delete the worker",
                deletion.name, deletion.branch
            ));
        }
        Ok(deletion)
    }

    /// Inspect one worker of a deletion `batch`. Git state shared with a
    /// record outside the batch is never counted or touched.
    async fn inspect_deletion(
        &self,
        worker_id: &str,
        batch: &[String],
    ) -> Result<WorkerDeletion, String> {
        let record = self.worker(worker_id).await?;
        let shared_branch = self.records.lock().await.iter().any(|other| {
            other.id != record.id
                && other.branch == record.branch
                && !batch.iter().any(|id| id == &other.id)
        });
        let mut deletion = WorkerDeletion {
            worker_id: record.id.clone(),
            name: record.name.clone(),
            branch: record.branch.clone(),
            live: self.live.lock().await.contains_key(worker_id),
            checkout: false,
            uncommitted: false,
            unmerged_commits: 0,
            shared_branch,
            blocked: None,
        };
        if record.status == WorkerStatus::Merging {
            deletion.blocked = Some("a merge is in progress".to_string());
            return Ok(deletion);
        }
        if deletion.shared_branch || !owns_its_branch(&record) {
            // A shared or malformed legacy record: only the record itself is
            // removed; no Git state another record (or no record) owns is
            // touched.
            return Ok(deletion);
        }
        let checkout = worker_checkout(&self.workspace_root, &record.id);
        if std::fs::symlink_metadata(&checkout).is_ok() {
            deletion.checkout = true;
            if checkout_is_registered(&self.workspace_root, &checkout).await? {
                match current_branch(&checkout) {
                    Ok(branch) if branch == record.branch => {}
                    Ok(branch) => {
                        deletion.blocked = Some(format!(
                            "its checkout is on '{branch}', not {}",
                            record.branch
                        ));
                        return Ok(deletion);
                    }
                    Err(error) => {
                        deletion.blocked =
                            Some(format!("its checkout could not be read: {error}"));
                        return Ok(deletion);
                    }
                }
                let status =
                    phase3::git_value(&checkout, &["status", "--porcelain", "-uall"]).await?;
                deletion.uncommitted = !status.is_empty();
            }
        }
        if branch_exists(&self.workspace_root, &record.branch).await {
            let range = format!("HEAD..refs/heads/{}", record.branch);
            let count =
                phase3::git_value(&self.workspace_root, &["rev-list", "--count", &range]).await?;
            deletion.unmerged_commits = count.parse().unwrap_or(0);
        }
        Ok(deletion)
    }

    /// Remove one checked worker. Git state goes first and the record last,
    /// so a failure leaves a record that a retry can finish deleting.
    async fn delete_one(&self, deletion: &WorkerDeletion) -> Result<(), String> {
        let record = self.worker(&deletion.worker_id).await?;
        let live = self.live.lock().await.remove(&record.id);
        if let Some(mut live) = live {
            live.abort();
            if let Err(error) = self.close_session_for_cleanup(&live.session).await {
                let message = format!("could not close {}'s session: {error}", record.name);
                live.cleanup_retry_only = true;
                self.live.lock().await.insert(record.id.clone(), live);
                return Err(message);
            }
        }
        if !deletion.shared_branch && owns_its_branch(&record) {
            let checkout = worker_checkout(&self.workspace_root, &record.id);
            let mut last_error = String::new();
            for _ in 0..12 {
                if std::fs::symlink_metadata(&checkout).is_err() {
                    break;
                }
                if !checkout_is_registered(&self.workspace_root, &checkout).await? {
                    remove_leftover_checkout(&self.workspace_root, &record).await?;
                    break;
                }
                // `worktree remove --force`: discarding was confirmed above.
                match remove_agent_worktree(self.workspace_root.clone(), record.id.clone()).await {
                    Ok(()) if !checkout.exists() => break,
                    Ok(()) => {
                        last_error =
                            "Git deregistered the checkout but left its folder".to_string()
                    }
                    Err(error) => last_error = error,
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            if checkout.exists() {
                return Err(format!(
                    "could not remove {}'s checkout: {last_error}",
                    record.name
                ));
            }
            if branch_exists(&self.workspace_root, &record.branch).await {
                phase3::git_value(&self.workspace_root, &["branch", "-D", &record.branch])
                    .await
                    .map_err(|error| {
                        format!("could not delete {}'s branch: {error}", record.name)
                    })?;
            }
        }
        self.records
            .lock()
            .await
            .retain(|worker| worker.id != record.id);
        self.messages
            .lock()
            .await
            .retain(|message| message.worker_id != record.id);
        self.persist().await
    }

    /// Stop every live worker. The shared engine is owned by the lead and is
    /// not shut down here.
    pub async fn shutdown(&self) {
        *self.lead_session.lock().await = None;
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

/// Whether a record's branch is exactly the one its id names, so Git
/// cleanup can never touch a branch the worker does not own.
fn owns_its_branch(record: &WorkerRecord) -> bool {
    !record.id.is_empty()
        && record
            .id
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
        && record.branch == agent_branch(&record.id)
}

/// The worker's registered checkout path inside the project.
fn worker_checkout(root: &Path, worker_id: &str) -> PathBuf {
    root.join(".worktrees")
        .join(format!("agent-{}", agent_short_id(worker_id)))
}

async fn branch_exists(root: &Path, branch: &str) -> bool {
    let revision = format!("refs/heads/{branch}^{{commit}}");
    phase3::git_value(root, &["rev-parse", "--verify", "--quiet", &revision])
        .await
        .is_ok()
}

/// Whether Git's main-checkout worktree registry still lists this exact path.
/// Never probe an unregistered folder with Git: without its own `.git`, Git
/// walks up and answers for the main checkout instead.
async fn checkout_is_registered(root: &Path, checkout: &Path) -> Result<bool, String> {
    let arguments = ["worktree", "list", "--porcelain", "-z"];
    let output = phase3::git(root, &arguments).await?;
    if !output.status.success() {
        return Err(phase3::git_error(&arguments, &output));
    }
    let expected = normalized_checkout_path(checkout);
    Ok(worktree_list_paths(&output.stdout)?
        .iter()
        .any(|listed| same_checkout_path(listed, &expected)))
}

/// Decode NUL-delimited porcelain worktree paths without replacement
/// characters. Unix paths remain arbitrary bytes; platforms whose paths are
/// Unicode reject invalid UTF-8 instead of failing open as an unregistered
/// checkout.
fn worktree_list_paths(stdout: &[u8]) -> Result<Vec<PathBuf>, String> {
    stdout
        .split(|byte| *byte == 0)
        .filter_map(|entry| entry.strip_prefix(b"worktree "))
        .map(|path| {
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStringExt;

                Ok(PathBuf::from(std::ffi::OsString::from_vec(path.to_vec())))
            }
            #[cfg(not(unix))]
            {
                std::str::from_utf8(path)
                    .map(PathBuf::from)
                    .map_err(|error| format!("undecodable worktree path: {error}"))
            }
        })
        .collect()
}

/// Normalize a checkout without following a symlink or junction at its final
/// component. Git prints absolute paths; canonicalizing the parent also
/// reconciles Windows short names and verbatim prefixes.
fn normalized_checkout_path(path: &Path) -> PathBuf {
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => dunce::canonicalize(parent)
            .unwrap_or_else(|_| dunce::simplified(parent).to_path_buf())
            .join(name),
        _ => dunce::simplified(path).to_path_buf(),
    }
}

fn same_checkout_path(left: &Path, right: &Path) -> bool {
    let left = normalized_checkout_path(left);
    let right = normalized_checkout_path(right);
    #[cfg(windows)]
    {
        left.to_string_lossy()
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_lowercase()
            == right
                .to_string_lossy()
                .replace('/', "\\")
                .trim_end_matches('\\')
                .to_lowercase()
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

#[cfg(windows)]
fn is_link_or_junction(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    metadata.file_type().is_symlink() || metadata.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn is_link_or_junction(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

/// Remove an unregistered checkout with filesystem operations only. The
/// canonical path must remain the expected direct child of this repository's
/// `.worktrees`; links and junctions are refused rather than traversed.
async fn remove_leftover_checkout(root: &Path, worker: &WorkerRecord) -> Result<(), String> {
    if !owns_its_branch(worker) {
        return Err("refusing to remove a checkout for an invalid worker identity".to_string());
    }
    let checkout = worker_checkout(root, &worker.id);
    let metadata = match std::fs::symlink_metadata(&checkout) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    if is_link_or_junction(&metadata) {
        return Err(format!(
            "refusing to remove a symlink or junction at {}",
            checkout.display()
        ));
    }
    if !metadata.is_dir() {
        return Err(format!(
            "refusing to remove a non-directory at {}",
            checkout.display()
        ));
    }
    let canonical_root = dunce::canonicalize(root).map_err(|error| error.to_string())?;
    let worktrees = root.join(".worktrees");
    let canonical_worktrees =
        dunce::canonicalize(&worktrees).map_err(|error| error.to_string())?;
    if !same_checkout_path(&canonical_worktrees, &canonical_root.join(".worktrees")) {
        return Err(format!(
            "refusing to remove {}: .worktrees is not a direct child of the workspace",
            checkout.display()
        ));
    }
    let canonical = dunce::canonicalize(&checkout).map_err(|error| error.to_string())?;
    let expected = canonical_worktrees.join(format!("agent-{}", agent_short_id(&worker.id)));
    if !same_checkout_path(&canonical, &expected)
        || canonical.parent() != Some(canonical_worktrees.as_path())
        || canonical.file_name() != expected.file_name()
    {
        return Err(format!(
            "refusing to remove {}: it is not the worker's own checkout",
            checkout.display()
        ));
    }

    let mut last_error = String::new();
    for attempt in 0..12 {
        // Revalidate immediately before every destructive call, then delete
        // only the canonical path that passed the direct-child guard above.
        let current = match dunce::canonicalize(&canonical) {
            Ok(current) => current,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.to_string()),
        };
        let metadata = match std::fs::symlink_metadata(&canonical) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.to_string()),
        };
        if current != canonical || is_link_or_junction(&metadata) || !metadata.is_dir() {
            return Err(format!(
                "refusing to remove {}: the validated checkout changed",
                canonical.display()
            ));
        }
        let path = canonical.clone();
        match tokio::task::spawn_blocking(move || std::fs::remove_dir_all(path)).await {
            Ok(Ok(())) => return Ok(()),
            Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Ok(Err(error)) => last_error = error.to_string(),
            Err(error) => last_error = error.to_string(),
        }
        if !canonical.exists() {
            return Ok(());
        }
        if attempt < 11 {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    }
    Err(format!(
        "could not remove leftover checkout {}: {last_error}",
        canonical.display()
    ))
}

async fn cleanup_agent_worktree(root: &Path, worker: &WorkerRecord) -> Result<(), String> {
    validate_worker_id(&worker.id)?;
    let worktree = worker_checkout(root, &worker.id);
    match std::fs::symlink_metadata(&worktree) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    }
    if !checkout_is_registered(root, &worktree).await? {
        return remove_leftover_checkout(root, worker).await;
    }
    if current_branch(&worktree)? != agent_branch(&worker.id) {
        return Err("worker checkout is on an unexpected branch; checkout retained".to_string());
    }
    let mut last_error = String::new();
    for _ in 0..12 {
        phase3::require_clean(&worktree, "worker before worktree cleanup").await?;
        match remove_agent_worktree(root.to_path_buf(), worker.id.clone()).await {
            Ok(()) if !worktree.exists() => return Ok(()),
            Ok(()) => last_error = "Git deregistered the checkout but left its folder".to_string(),
            Err(_) if !worktree.exists() => return Ok(()),
            Err(err) => last_error = err,
        }
        if !checkout_is_registered(root, &worktree).await? {
            return remove_leftover_checkout(root, worker).await;
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
        review_risk: Some(worker.review_risk.clone()),
        review_evidence: review.evidence.clone(),
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

async fn merge_conflicts_only(root: &Path) -> Result<bool, String> {
    let status = phase3::git_value(root, &["status", "--porcelain", "-uall"]).await?;
    let mut conflicts = false;
    for line in status.lines().filter(|line| !line.is_empty()) {
        if line.len() < 3 || !matches!(&line[..2], "DD" | "AU" | "UD" | "UA" | "DU" | "AA" | "UU") {
            return Ok(false);
        }
        conflicts = true;
    }
    Ok(conflicts)
}

async fn recover_worker(root: &std::path::Path, worker: &mut WorkerRecord) -> Result<(), String> {
    worker.brief_sent = true;
    worker.compacting = false;
    let legacy_budget_pause = worker.error.as_deref().is_some_and(|error| {
        error.starts_with("worker token budget of ")
            || matches!(
                error,
                "increase the worker token budget before continuing"
                    | "increase the worker token budget before resuming"
            )
    });
    if legacy_budget_pause {
        worker.error = None;
        if worker.pending.is_none()
            && matches!(worker.status, WorkerStatus::NeedsYou | WorkerStatus::Failed)
        {
            worker.status = WorkerStatus::Interrupted;
        }
    }
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
            phase3::require_clean_lead(root, "lead after completed merge").await?;
            if merge_head.status.success() {
                let reset = phase3::git(root, &["reset", "--merge"]).await?;
                if !reset.status.success() {
                    return Err(phase3::git_error(&["reset", "--merge"], &reset));
                }
                phase3::require_clean_lead(root, "lead after stale merge cleanup").await?;
            }
            let record = merge_record_from_git(root, worker, lead_tip, target).await?;
            worker.status = WorkerStatus::Merged;
            worker.merge = Some(record);
            worker.error = None;
            if let Err(err) = cleanup_agent_worktree(root, worker).await {
                worker.error = Some(format!(
                    "merge recovered, but worktree cleanup needs attention: {err}"
                ));
            }
            return Ok(());
        }

        if merge_head.status.success() {
            if !merge_conflicts_only(root).await? {
                worker.status = WorkerStatus::Conflict;
                worker.error = Some(
                    "the interrupted merge is retained because the lead has unrelated changes or no verifiable conflict-only state; inspect it before recovery"
                        .to_string(),
                );
                return Ok(());
            }
            let abort = phase3::git(root, &["merge", "--abort"]).await?;
            if !abort.status.success() {
                return Err(phase3::git_error(&["merge", "--abort"], &abort));
            }
            phase3::require_clean_lead(root, "lead after merge abort").await?;
            worker.status = WorkerStatus::Conflict;
            worker.error = Some(
                "the interrupted merge was aborted; resolve the conflict and review the worker tip again"
                    .to_string(),
            );
        } else {
            phase3::require_clean_lead(root, "lead after interrupted merge").await?;
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
    usage: Arc<Mutex<ProjectUsage>>,
    save_lock: Arc<Mutex<()>>,
    pricing: Arc<std::sync::RwLock<Vec<mnemo::config::PricingEntry>>>,
    persistence: TeamPersistence,
) {
    let mut turn_active = false;
    loop {
        let event = match receiver.recv().await {
            Ok(event) => event,
            // A slow consumer skips what it missed rather than abandoning
            // the worker's ledger mid-turn.
            Err(mnemo::headless::RecvError::Lagged(_)) => continue,
            Err(mnemo::headless::RecvError::Closed) => break,
        };
        // The engine bus is shared with the lead and every other worker:
        // only this worker's own events may answer its host tools or touch
        // its record (status, telemetry, model, errors).
        if !event.is_from(session_id) {
            continue;
        }
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
        let mut refresh = false;
        let mut changed = false;
        {
            let mut records = records.lock().await;
            if let Some(worker) = records.iter_mut().find(|worker| {
                worker.id == worker_id
                    && (worker.generation == 0 || worker.generation == generation)
            }) {
                let activity_ms = phase3::now_ms();
                worker.last_activity_ms = activity_ms;
                if worker.created_at_ms > 0 {
                    worker.elapsed_ms = activity_ms.saturating_sub(worker.created_at_ms);
                }
                match &event.event {
                    SerializableAgentEvent::Started => {
                        if !matches!(
                            worker.status,
                            WorkerStatus::Paused
                                | WorkerStatus::Interrupted
                                | WorkerStatus::Stopped
                                | WorkerStatus::Failed
                                | WorkerStatus::Merged
                                | WorkerStatus::Cancelled
                        ) {
                            turn_active = true;
                            busy.store(true, Ordering::SeqCst);
                            worker.status = if worker.fixer_finding_id.is_some() {
                                WorkerStatus::Fixing
                            } else {
                                WorkerStatus::Working
                            };
                            changed = true;
                        }
                    }
                    SerializableAgentEvent::ModelChanged { model, .. } => {
                        // The engine's bus is engine-wide: only OUR
                        // session's model change may re-point this
                        // worker's observed model (and its next usage
                        // row) — a concurrent worker's ModelChanged must
                        // not cross into it.
                        if event_session(&event) == Some(session_id) {
                            worker.model_id = Some(model.clone());
                            changed = true;
                        }
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
                        // Fold the same report into the durable per-project
                        // accumulator: raw tokens keyed by the observed model,
                        // independent of this worker record so the totals
                        // survive worker deletion and restarts. The engine's
                        // bus is engine-wide, so fold only OUR session's
                        // report — otherwise every listening pump would fold
                        // the same event.
                        if event_session(&event) == Some(session_id) {
                            let accumulator_model = worker
                                .model_id
                                .clone()
                                .or_else(|| usage_model.clone())
                                .or_else(|| {
                                    worker
                                        .model
                                        .as_deref()
                                        .map(|spec| usage_model_key(spec).to_string())
                                });
                            usage.lock().await.record(
                                accumulator_model.as_deref().unwrap_or("unknown"),
                                prompt,
                                cached,
                                completion,
                            );
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
                    // A worker's host tool (`message_parent`) was answered
                    // above, before this fold: it is ordinary progress, never
                    // a wait on the user.
                    SerializableAgentEvent::CompactStarted => {
                        worker.compacting = true;
                        busy.store(true, Ordering::SeqCst);
                        worker.status = WorkerStatus::Working;
                        worker.error = None;
                        changed = true;
                    }
                    SerializableAgentEvent::Compacted { .. } => {
                        if worker.compacting {
                            worker.compacting = false;
                            busy.store(false, Ordering::SeqCst);
                            if !matches!(
                                worker.status,
                                WorkerStatus::Paused
                                    | WorkerStatus::Interrupted
                                    | WorkerStatus::Stopped
                            ) {
                                worker.status = if worker.pending.is_some() {
                                    WorkerStatus::NeedsYou
                                } else {
                                    WorkerStatus::Idle
                                };
                            }
                            worker.error = None;
                            changed = true;
                        }
                        refresh = true;
                    }
                    SerializableAgentEvent::Finished {
                        reason: mnemo::provider::FinishReason::LimitReached { which },
                    } => {
                        // The session parked on its runaway guard: the work is
                        // unfinished, so it must not read as completed.
                        turn_active = false;
                        worker.compacting = false;
                        busy.store(false, Ordering::SeqCst);
                        worker.pending = None;
                        worker.status = WorkerStatus::NeedsYou;
                        worker.error = Some(format!(
                            "the worker session reached a {} limit before finishing; team_stop then team_resume continues it in a fresh session",
                            limit_label(*which)
                        ));
                        terminal = true;
                    }
                    SerializableAgentEvent::Finished { .. } => {
                        let completed_turn = turn_active;
                        turn_active = false;
                        worker.compacting = false;
                        busy.store(false, Ordering::SeqCst);
                        worker.pending = None;
                        if completed_turn
                            && !matches!(
                                worker.status,
                                WorkerStatus::Completed
                                    | WorkerStatus::Paused
                                    | WorkerStatus::Interrupted
                                    | WorkerStatus::Stopped
                                    | WorkerStatus::Failed
                                    | WorkerStatus::Merged
                                    | WorkerStatus::Cancelled
                            )
                        {
                            worker.status = WorkerStatus::Completed;
                            worker.turns = worker.turns.saturating_add(1);
                        }
                        terminal = true;
                    }
                    SerializableAgentEvent::Exited => {
                        turn_active = false;
                        worker.compacting = false;
                        busy.store(false, Ordering::SeqCst);
                        worker.pending = None;
                        if !matches!(
                            worker.status,
                            WorkerStatus::Completed
                                | WorkerStatus::Idle
                                | WorkerStatus::NeedsYou
                                | WorkerStatus::Paused
                                | WorkerStatus::Interrupted
                                | WorkerStatus::Stopped
                                | WorkerStatus::Failed
                                | WorkerStatus::Merged
                                | WorkerStatus::Cancelled
                        ) {
                            worker.status = WorkerStatus::Interrupted;
                            worker.error = Some(
                                "the worker session exited before completing its turn".to_string(),
                            );
                        }
                        terminal = true;
                    }
                    SerializableAgentEvent::Parked { .. } => {
                        turn_active = false;
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
                    SerializableAgentEvent::Error { error, retrying } => {
                        if !retrying {
                            turn_active = false;
                        }
                        if worker.compacting {
                            worker.compacting = false;
                            busy.store(false, Ordering::SeqCst);
                            if !matches!(
                                worker.status,
                                WorkerStatus::Paused
                                    | WorkerStatus::Interrupted
                                    | WorkerStatus::Stopped
                            ) {
                                worker.status = if worker.pending.is_some() {
                                    WorkerStatus::NeedsYou
                                } else {
                                    WorkerStatus::Idle
                                };
                            }
                            worker.error = (!retrying).then(|| error.clone());
                            refresh = true;
                            changed = true;
                        } else if !retrying {
                            busy.store(false, Ordering::SeqCst);
                            worker.status = WorkerStatus::Failed;
                            worker.error = Some(error.clone());
                            terminal = true;
                        }
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
            let _ = save_workers(&records, &usage, &save_lock, &persistence).await;
        }
        if terminal || refresh {
            let engine = engine.clone();
            let records = records.clone();
            let usage = usage.clone();
            let save_lock = save_lock.clone();
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
                let _ = save_workers(&records, &usage, &save_lock, &persistence).await;
            });
        }
    }
}

/// Write the whole ledger — workers, slots, and the usage accumulator.
///
/// `save_lock` serializes every `team.json` write (pump saves, the lead's
/// usage recording, cleanup) so a usage report folded into the accumulator
/// can never be lost to a stale interleaving of concurrent saves: each save
/// clones the latest committed state while holding the lock.
async fn save_workers(
    records: &Arc<Mutex<Vec<WorkerRecord>>>,
    usage: &Arc<Mutex<ProjectUsage>>,
    save_lock: &Arc<Mutex<()>>,
    persistence: &TeamPersistence,
) -> Result<(), String> {
    let _serialized = save_lock.lock().await;
    let workers = records.lock().await.clone();
    let usage = usage.lock().await.clone();
    persistence.save(&PersistedTeam {
        workers,
        slots: Some(persistence.slots.load(Ordering::SeqCst)),
        usage,
    })
}

/// What deleting one worker removes, reported before anything is deleted so
/// the user can see exactly which unmerged work would be discarded.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct WorkerDeletion {
    /// The worker's id.
    pub worker_id: String,
    /// The worker's display name.
    pub name: String,
    /// The worker's `wt/agent-*` branch.
    pub branch: String,
    /// The worker's session is live; deleting stops it first.
    pub live: bool,
    /// The worker's checkout still exists and will be removed.
    pub checkout: bool,
    /// That checkout has uncommitted changes.
    pub uncommitted: bool,
    /// Commits on the branch that the lead branch does not contain.
    pub unmerged_commits: u32,
    /// Another worker record (outside this deletion) uses the same branch,
    /// so the branch and checkout are kept and only this record goes.
    pub shared_branch: bool,
    /// Why the worker cannot be deleted right now, when it cannot.
    pub blocked: Option<String>,
}

impl WorkerDeletion {
    /// Whether deleting this worker discards work the lead branch lacks.
    pub fn discards_work(&self) -> bool {
        self.uncommitted || self.unmerged_commits > 0
    }
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
        if branch != agent_branch(&id)
            || id.is_empty()
            || !id.chars().all(|c| c.is_ascii_alphanumeric())
        {
            report
                .skipped
                .push(format!("{branch}: invalid worker identity"));
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
        let expected = if std::fs::symlink_metadata(&checkout).is_ok() {
            dunce::canonicalize(&checkout).map_err(|err| err.to_string())?
        } else {
            checkout.clone()
        };
        if recorded != expected {
            report.skipped.push(format!(
                "{branch}: checkout is outside its registered worktree"
            ));
            continue;
        }
        let revision = format!("refs/heads/{branch}^{{commit}}");
        if let Err(error) = phase3::git_value(root, &["rev-parse", "--verify", &revision]).await {
            report.skipped.push(format!(
                "{branch}: retained checkout because the branch cannot be verified: {error}"
            ));
            continue;
        }
        if std::fs::symlink_metadata(&checkout).is_ok() {
            let registered = checkout_is_registered(root, &checkout).await?;
            let mut removal_error = None;
            if registered {
                match phase3::git_value(&checkout, &["status", "--porcelain", "-uall"]).await {
                    Ok(status) if status.trim().is_empty() => {}
                    Ok(_) => {
                        report
                            .skipped
                            .push(format!("{branch}: uncommitted work is retained for review"));
                        continue;
                    }
                    Err(error) => {
                        report.skipped.push(format!(
                            "{branch}: cleanliness could not be verified: {error}"
                        ));
                        continue;
                    }
                }
                if let Err(error) = remove_agent_worktree(root.to_path_buf(), id.clone()).await {
                    removal_error = Some(error);
                }
            }
            if std::fs::symlink_metadata(&checkout).is_ok() {
                if checkout_is_registered(root, &checkout).await? {
                    report.skipped.push(format!(
                        "{branch}: cleanup failed: {}",
                        removal_error.unwrap_or_else(|| {
                            "the registered checkout folder remains".to_string()
                        })
                    ));
                    continue;
                }
                if let Err(error) = remove_leftover_checkout(root, worker).await {
                    report
                        .skipped
                        .push(format!("{branch}: cleanup failed: {error}"));
                    continue;
                }
            }
            report.removed.push(branch.clone());
        } else {
            report.skipped.push(format!(
                "{branch}: checkout was already absent; branch retained"
            ));
        }
        worker.status = WorkerStatus::Cancelled;
        worker.pending = None;
        worker.worktree = PathBuf::new();
        worker.error =
            Some("inactive checkout cleaned; the branch is retained for review".to_string());
        persistence.save(&ledger)?;
    }
    Ok(report)
}

fn validate_worker_id(worker_id: &str) -> Result<(), String> {
    if !(8..=64).contains(&worker_id.len())
        || !worker_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
    {
        return Err(
            "worker_id must contain 8-64 ASCII letters or digits; reuse the ID returned by team_dispatch"
                .to_string(),
        );
    }
    Ok(())
}

fn without_transcripts(value: &mut Value) {
    match value {
        Value::Object(object) => {
            object.remove("transcript");
            for nested in object.values_mut() {
                without_transcripts(nested);
            }
        }
        Value::Array(items) => {
            for nested in items {
                without_transcripts(nested);
            }
        }
        _ => {}
    }
}

fn now_key() -> u128 {
    phase3::now_ms() as u128
}

/// `path` relative to the project root with forward slashes; the path as-is
/// when it is not under the root.
fn project_relative(root: &Path, path: &str) -> String {
    let path = Path::new(path);
    let relative = path
        .strip_prefix(root)
        .map(Path::to_path_buf)
        .ok()
        .or_else(|| {
            // The same location spelled differently (short names, `\\?\`).
            let root = std::fs::canonicalize(root).ok()?;
            let path = std::fs::canonicalize(path).ok()?;
            path.strip_prefix(root).map(Path::to_path_buf).ok()
        })
        .unwrap_or_else(|| path.to_path_buf());
    relative.to_string_lossy().replace('\\', "/")
}

/// One `## Progress` line for a merged task: step, worker, model, merge
/// commit, and the review summary when one was recorded.
fn merge_outcome_line(number: usize, worker: &WorkerRecord) -> String {
    let merge = worker.merge.as_ref();
    let model = merge
        .and_then(|m| m.worker_model.clone())
        .or_else(|| worker.model_id.clone())
        .or_else(|| worker.model.clone())
        .unwrap_or_else(|| "default model".to_string());
    let commit = merge.map_or("", |m| m.commit.as_str());
    let short: String = commit.chars().take(8).collect();
    let mut line = format!(
        "step {number} merged: {} ({}) on {model}, merge commit {short}",
        worker.name, worker.id
    );
    if let Some(summary) = merge
        .map(|m| m.review_summary.trim())
        .filter(|summary| !summary.is_empty())
    {
        line.push_str(&format!("; review: {summary}"));
    }
    line
}

fn worker_result<T: Serialize>(result: Result<T, String>, action: &str) -> ToolResult {
    match result {
        Ok(value) => {
            let data = serde_json::to_value(value).unwrap_or(Value::Null);
            let mut output = data.clone();
            without_transcripts(&mut output);
            let output = serde_json::to_string(&json!({"action": action, "result": output}))
                .unwrap_or_else(|_| action.to_string());
            ToolResult::success(output).with_data(data)
        }
        Err(err) => ToolResult::error(err.clone()).with_data(json!({
            "error_kind": "domain",
            "message": err,
        })),
    }
}

/// Scan a plan document for its `## Acceptance scenarios` section.
///
/// Returns whether the section exists and how many observable bullets it
/// carries. The section runs from its level-2 heading to the next heading of
/// level 2 or above; a bullet is observable when its list-item text carries at
/// least eight characters of substance, so placeholders like `- x` do not
/// count.
fn acceptance_scenarios(doc: &str) -> (bool, usize) {
    let mut in_section = false;
    let mut found = false;
    let mut bullets = 0;
    for line in doc.lines() {
        let trimmed = line.trim();
        let level = trimmed.chars().take_while(|c| *c == '#').count();
        if level > 0 && trimmed[level..].starts_with(' ') {
            if level <= 2 {
                in_section = level == 2
                    && trimmed[level..]
                        .trim()
                        .eq_ignore_ascii_case("acceptance scenarios");
                found = found || in_section;
            }
            continue;
        }
        if !in_section {
            continue;
        }
        let text = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .map(str::trim)
            .unwrap_or("");
        if text.len() >= 8 {
            bullets += 1;
        }
    }
    (found, bullets)
}

/// Require the dispatched board plan's document to declare observable
/// `## Acceptance scenarios` before a high-risk dispatch on it.
///
/// Returns an actionable error for a missing section and for a section
/// without an observable bullet.
fn require_acceptance_scenarios(plan_id: &str, doc: &str) -> Result<(), String> {
    let guidance = "add observable scenarios with plan_update add_acceptance_scenarios, or dispatch with review_risk='standard'";
    let (found, bullets) = acceptance_scenarios(doc);
    if !found {
        return Err(format!(
            "review_risk='high' requires acceptance scenarios, but plan '{plan_id}' does not declare any; {guidance}"
        ));
    }
    if bullets == 0 {
        return Err(format!(
            "review_risk='high' requires acceptance scenarios, but plan '{plan_id}' has no observable bullet (a concrete, verifiable outcome of at least eight characters); {guidance}"
        ));
    }
    Ok(())
}

/// Enforce every source/evidence precondition while the merge lock is held.
fn validate_merge(worker: &WorkerRecord, tip: &str, records: &[WorkerRecord]) -> Result<(), String> {
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
    // A summary plus passing checks alone cannot merge: every review of the
    // merge-time tip must carry complete structured evidence. Legacy reviews
    // decoded without evidence fail here, so old records need a fresh review.
    let evidence = review.evidence.as_ref().ok_or_else(|| {
        "the review predates structured evidence; re-review the current worker tip with a complete evidence object"
            .to_string()
    })?;
    // High-risk merges additionally require a completed, fresh, read-only
    // audit linked to this exact tip plus a written lead adjudication. Any
    // fix commit moved the tip and made the previous audit stale — this gate
    // blocks the merge until a fresh audit ran at the new tip.
    let require_audit = worker.review_risk == "high";
    phase3::validate_review_evidence(evidence, require_audit)?;
    validate_audit_linkage(records, evidence, &worker.id, tip)?;
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

/// A human label for the session limit a worker hit.
fn limit_label(which: mnemo::provider::LimitKind) -> &'static str {
    use mnemo::provider::LimitKind;
    match which {
        LimitKind::MaxTurns | LimitKind::MaxRequests => "request",
        LimitKind::MaxPromptTokens => "prompt-token",
        LimitKind::MaxCompletionTokens => "completion-token",
        LimitKind::MaxCostMicros => "cost",
        LimitKind::WallClock => "wall-clock",
        LimitKind::Idle => "idle",
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
            "Do not create or modify plans, spawn agents, or ask the user. When blocked, ",
            "use message_parent with the precise issue and wait for the lead; never modify ",
            "TeamManager state or use Git/files as worker lifecycle status. Run relevant ",
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
        // No session limits: no request, token, cost, or wall-clock cap.
        // Workers do the bulk of the implementation and idle for long
        // stretches while the lead reviews them; the lead or user stops
        // a worker explicitly.
        limits: None,
        gate_rules: executor_gate_rules(),
        question_policy: QuestionPolicy::Deny,
        turn_end: TurnEndPolicy::Stop,
    }
}

/// Guardrails for one worker kind: executors keep the coding surface, audit
/// workers are read-only independent reviewers.
fn guardrails_for(kind: &str) -> Guardrails {
    if kind == "audit" {
        audit_guardrails()
    } else {
        executor_guardrails()
    }
}
/// never touches lifecycle state.
pub(crate) fn audit_prompt() -> PromptSpec {
    PromptSpec {
        identity: concat!(
            "You are a Mnemo2 independent audit worker. You did not implement the change under ",
            "review; form your own opinion from the plan, the brief, and the diff alone — never ",
            "from the implementer's claims. You are read-only by construction: you cannot edit ",
            "files, run shell commands, commit, merge, push, spawn work, ask the user, or tick ",
            "plan steps. Audit correctness, error handling, integration with neighbouring ",
            "contracts, persistence, concurrency, security, and test quality against the plan's ",
            "intent and acceptance scenarios. Report findings with file/line evidence through ",
            "message_parent, then end your turn; the lead adjudicates your audit on the ",
            "implementation worker's review. Never merge, never mutate files, and never use ",
            "Git or files as lifecycle status."
        )
        .to_string(),
        blocks: Vec::new(),
        extra_stable: None,
        constitution: ConstitutionMode::Both,
    }
}

/// The audit surface: every read tool (files, search, graph, git_read, web,
/// memory reads) with all mutation paths excluded — no file writes, no
/// shell, no git writes, no spawning, no user questions, no plan-board or
/// step ticks, and no durable-memory mutations (writes, amendments, or
/// consolidation).
fn audit_guardrails() -> Guardrails {
    let mut tools = ToolSelection::all();
    tools.exclude = vec![
        "create_plan".to_string(),
        "update_plan".to_string(),
        "finish".to_string(),
        "spawn_agent".to_string(),
        "ask_user".to_string(),
        "file_edit".to_string(),
        "file_write".to_string(),
        "multi_edit".to_string(),
        "convert_line_endings".to_string(),
        "shell".to_string(),
        "git".to_string(),
        "memory_write".to_string(),
        "memory_amend".to_string(),
        "memory_update".to_string(),
        "memory_supersede".to_string(),
        "memory_delete".to_string(),
        "memory_consolidate".to_string(),
        "complete_step".to_string(),
        "abandon_plan".to_string(),
    ];
    Guardrails {
        prompt: Some(audit_prompt()),
        tools,
        route: vec!["free".to_string()],
        limits: None,
        gate_rules: Vec::new(),
        question_policy: QuestionPolicy::Deny,
        turn_end: TurnEndPolicy::Stop,
    }
}

/// Cap one brief section, keeping the cut visible to the reviewer.
fn truncate_for_brief(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let mut end = limit;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n[truncated — {end} of {} bytes shown; read the remainder in your checkout at the audited tip]",
        &text[..end],
        text.len()
    )
}

/// Build the audit worker's opening brief: the plan context, the lead's
/// intent brief, and the target diff. It never includes the implementation
/// worker's own summary, findings, or review — the reviewer starts
/// unanchored to the implementer's conclusions.
async fn audit_brief_text(
    root: &Path,
    target: &WorkerRecord,
    tip: &str,
    plan_id: &str,
    plan_doc: &str,
    lead_instructions: Option<&str>,
    reasoning_effort: Option<&str>,
) -> Result<String, String> {
    let merge_base = phase3::git_value(root, &["merge-base", "HEAD", tip])
        .await
        .map_err(|err| format!("could not compute the audit's diff base: {err}"))?;
    let log = phase3::git_value(root, &["log", "--oneline", &format!("{merge_base}..{tip}")])
        .await
        .unwrap_or_default();
    let stat = phase3::git_value(root, &["diff", "--stat", &merge_base, tip])
        .await
        .unwrap_or_default();
    let diff = truncate_for_brief(
        &phase3::git_value(root, &["diff", &merge_base, tip])
            .await
            .unwrap_or_default(),
        48_000,
    );
    let mut text = String::new();
    text.push_str("# Independent read-only audit\n\n");
    text.push_str(
        "You are a Mnemo2 independent audit worker. You did not implement this change; form your own opinion from the plan and the diff alone. You are read-only by construction: you cannot edit files, run shell commands, commit, merge, push, spawn work, ask the user, or tick plan steps. Do not try.\n\n",
    );
    text.push_str(&format!(
        "Implementation worker: {} ({}), branch `{}`, audited tip `{tip}`.\n\n",
        target.name, target.id, target.branch
    ));
    if let Some(effort) = reasoning_effort {
        text.push_str(&format!(
            "Lead-requested reasoning depth: {effort}. Plan the depth of your audit accordingly.\n\n"
        ));
    }
    text.push_str("## The lead's brief to the implementation worker (intent — not a summary of what was done)\n\n");
    text.push_str(&target.brief);
    text.push_str("\n\n");
    if let Some(instructions) = lead_instructions.filter(|value| !value.trim().is_empty()) {
        text.push_str("## Additional lead audit instructions\n\n");
        text.push_str(instructions);
        text.push_str("\n\n");
    }
    text.push_str(&format!(
        "## Plan context — board plan `{plan_id}`\n\n```markdown\n{plan_doc}\n```\n\n"
    ));
    text.push_str(&format!(
        "## Target diff `{merge_base}..{tip}`\n\n### Commits\n\n```\n{log}\n```\n\n### Stat\n\n```\n{stat}\n```\n\n### Full diff\n\n```diff\n{diff}\n```\n\n"
    ));
    text.push_str(
        "## Your task\n\nAudit the change against the plan's intent and its acceptance scenarios where present: correctness of the real execution path, error handling, integration with neighbouring contracts and persistence, concurrency and security interactions, and whether the tests genuinely exercise the change (a passing count alone is insufficient). Report every finding with file/line evidence through message_parent, then end your turn. Never merge, never modify files, never tick plan steps; the lead adjudicates your audit on the implementation worker's review.\n",
    );
    Ok(text)
}

/// Validate the review's independent-audit linkage against the live ledger.
///
/// Any linked reviewer must be a completed audit worker that targets this
/// worker at exactly this tip: a fix commit moves the tip and makes the
/// previous audit stale, blocking review and merge until a fresh audit ran
/// at the new tip. Linking a reviewer also requires a written lead
/// adjudication of its findings.
fn validate_audit_linkage(
    records: &[WorkerRecord],
    evidence: &ReviewEvidence,
    worker_id: &str,
    tip: &str,
) -> Result<(), String> {
    let Some(reviewer_id) = evidence
        .independent_reviewer_id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
    else {
        return Ok(());
    };
    if evidence
        .audit_adjudication
        .as_deref()
        .unwrap_or("")
        .trim()
        .is_empty()
    {
        return Err(
            "linking an independent audit requires a written audit_adjudication of its findings"
                .to_string(),
        );
    }
    let audit = records
        .iter()
        .find(|record| record.id == reviewer_id)
        .ok_or_else(|| {
            format!(
                "independent reviewer '{reviewer_id}' is not a tracked worker; dispatch one with team_audit_dispatch first"
            )
        })?;
    if audit.kind != "audit" {
        return Err(format!(
            "independent reviewer '{reviewer_id}' is an implementation worker, not a read-only audit worker"
        ));
    }
    if audit.audit_target.as_deref() != Some(worker_id) {
        return Err(format!(
            "audit worker '{reviewer_id}' reviews '{}' rather than worker '{worker_id}'",
            audit.audit_target.as_deref().unwrap_or("(none)")
        ));
    }
    if audit.audit_tip.as_deref() != Some(tip) {
        return Err(format!(
            "the audit worker '{reviewer_id}' inspected tip '{}' but worker '{worker_id}' is now at tip '{tip}'; the audit is stale — dispatch a fresh audit with team_audit_dispatch at the current tip",
            audit.audit_tip.as_deref().unwrap_or("(none)")
        ));
    }
    if !matches!(audit.status, WorkerStatus::Completed | WorkerStatus::Stopped) {
        return Err(format!(
            "the audit worker '{reviewer_id}' is {:?}, not completed; let it finish before linking it",
            audit.status
        ));
    }
    Ok(())
}

/// The refusal text every executor gate rule carries.
const EXECUTOR_GIT_DENY: &str = concat!(
    "TeamManager owns branches and integration: workers never push, merge, rebase, ",
    "switch or delete branches, manage worktrees, or use `gh pr`. Commit on your own ",
    "branch and report with message_parent."
);

/// Where a shell command starts inside the JSON-serialized `shell` args: the
/// `command` value, then any text up to a command boundary (start, whitespace
/// or shell punctuation, or an escaped newline/tab/quote) before the program.
const SHELL_CMD_PREFIX: &str =
    r#"(?i)"command":"(?:(?:[^"\\]|\\.)*?(?:\\[ntr"]|[^A-Za-z0-9_.\-\\"]))?"#;

/// `git` plus its global options, ending at whitespace before the subcommand
/// (`git -C dir push`, `git -c k=v merge`, `git --no-pager rebase`).
const SHELL_GIT: &str = r#"git(?:\s+(?:-C\s+\S+|-c\s+\S+|--[A-Za-z-]+(?:=\S+)?|-[pP]))*\s+"#;

/// One shell argument that is not the bare `--` path separator.
const SHELL_ARG_NOT_DASHDASH: &str =
    r#"(?:--[^\s;&|\\"]+|-[^-\s;&|\\"][^\s;&|\\"]*|-|[^-\s;&|\\"][^\s;&|\\"]*)"#;

/// The end of one shell command inside the serialized args.
const SHELL_CMD_END: &str = r#"\s*(?:[;&|)]|\\[ntr"]|")"#;

fn deny_rule(tool: &str, pattern: String) -> GateRule {
    GateRule {
        tool: tool.to_string(),
        arg_pattern: Some(pattern),
        verdict: GateVerdict::Deny {
            message: EXECUTOR_GIT_DENY.to_string(),
        },
    }
}

/// Deny rules that keep executor workers out of integration. A deny is a veto,
/// so these hold under `OverridePermissions`. Patterns match the call's
/// JSON-serialized args (see `DeclarativePolicy::gate_call`).
fn executor_gate_rules() -> Vec<GateRule> {
    let mut rules = Vec::new();
    // The structured `git` tool. `checkout` takes a branch, never paths
    // (paths go through `restore`), so every checkout is a branch switch.
    rules.push(deny_rule(
        "git",
        r#"(?i)"subcommand":"(?:push|merge|checkout|delete)""#.to_string(),
    ));
    rules.push(deny_rule("git", r#"(?i)"action":"delete""#.to_string()));
    // `action` alone can carry the subcommand, except for `stash` + `push`.
    rules.push(GateRule {
        tool: "git".to_string(),
        arg_pattern: Some(r#"(?i)"subcommand":"stash""#.to_string()),
        verdict: GateVerdict::Allow,
    });
    rules.push(deny_rule(
        "git",
        r#"(?i)"action":"(?:push|merge|checkout)""#.to_string(),
    ));
    // The `shell` tool: any git invocation, however chained or qualified.
    let pre = SHELL_CMD_PREFIX;
    let git = SHELL_GIT;
    for sub in [
        r"(?:push|merge|rebase|switch|worktree)\b",
        // Branch deletion: `-d`, `-D`, a combined short flag, or `--delete`.
        r#"branch\s+(?:[^;&|\\"]*\s)?(?:-[A-Za-z]*[dD][A-Za-z]*|--delete)(?:\s|[;&|\\"]|$)"#,
    ] {
        rules.push(deny_rule("shell", format!("{pre}{git}{sub}")));
    }
    // `checkout` of a branch; `checkout -- paths` and `checkout rev -- paths`
    // restore files and stay allowed.
    rules.push(deny_rule(
        "shell",
        format!("{pre}{git}checkout(?:\\s+{SHELL_ARG_NOT_DASHDASH})+{SHELL_CMD_END}"),
    ));
    rules.push(deny_rule("shell", format!(r"{pre}gh\s+pr\b")));
    rules
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
                "token_budget": 200000,
                "checks": [{
                    "name": "cargo test --workspace",
                    "status": "passed",
                    "detail": null
                }],
                "error": "worker token budget of 200000 was exhausted; increase the budget before continuing"
            }]
        });

        let mut persisted: PersistedTeam = serde_json::from_value(legacy).expect("legacy ledger");
        let worker = &persisted.workers[0];
        assert!(serde_json::to_value(worker)
            .expect("worker JSON")
            .get("token_budget")
            .is_none());
        assert_eq!(worker.turns, 0);
        assert_eq!(worker.last_activity_ms, 0);
        assert!(worker.findings.is_empty());
        assert!(worker.review.is_none());
        assert!(worker.fixer_finding_id.is_none());
        assert!(worker.merge.is_none());
        assert_eq!(worker.checks[0].tip, "");
        // Legacy ledgers predate review risk: they decode as standard.
        assert_eq!(worker.review_risk, "standard");
        recover_worker(std::path::Path::new("unused"), &mut persisted.workers[0])
            .await
            .expect("legacy recovery");
        assert!(persisted.workers[0].error.is_none());
        assert_eq!(persisted.workers[0].total_tokens, 120);
        assert_eq!(persisted.workers[0].prompt_tokens, 120);
    }

    /// Workers do the bulk of the work and run without any session limit.
    /// The old 64-request cap and 30-minute wall clock (counted from spawn,
    /// idle review time included) parked real implementation workers
    /// mid-task.
    #[test]
    fn executor_sessions_have_no_limits() {
        assert_eq!(executor_guardrails().limits, None);
    }

    /// Runs a call through the executor's declarative gate and reports
    /// whether it was denied.
    async fn executor_denies(tool: &str, args: Value) -> bool {
        use mnemo::agent::policy::{AgentPolicy, DeclarativePolicy};
        let policy =
            DeclarativePolicy::new(Arc::new(executor_guardrails())).expect("rules compile");
        matches!(
            policy.gate_call(tool, &args).await,
            GateVerdict::Deny { message } if message.contains("TeamManager owns")
        )
    }

    #[tokio::test]
    async fn executor_gate_denies_integration_through_the_git_tool() {
        for args in [
            json!({"subcommand": "push"}),
            json!({"subcommand": "PUSH", "branch": "main"}),
            json!({"action": "push"}),
            json!({"subcommand": "merge", "branch": "wt/other"}),
            json!({"subcommand": "checkout", "branch": "main"}),
            json!({"subcommand": "branch", "action": "delete", "branch": "x"}),
            json!({"subcommand": "delete", "branch": "x"}),
        ] {
            assert!(executor_denies("git", args.clone()).await, "{args}");
        }
        for args in [
            json!({"subcommand": "status"}),
            json!({"subcommand": "diff", "args": ["--stat"]}),
            json!({"subcommand": "log", "args": ["-5"]}),
            json!({"subcommand": "commit", "message": "push the button"}),
            json!({"subcommand": "stash", "action": "push"}),
            json!({"subcommand": "branch", "action": "list"}),
            json!({"subcommand": "restore", "paths": ["src/lib.rs"]}),
        ] {
            assert!(!executor_denies("git", args.clone()).await, "{args}");
        }
    }

    #[tokio::test]
    async fn executor_gate_denies_integration_through_the_shell() {
        for command in [
            "git push",
            "git push origin HEAD",
            "git -C ../repo push",
            "git -c user.name=x push --force",
            "git --no-pager merge wt/other",
            "cargo test && git push",
            "cargo test;git push",
            "echo hi\ngit push",
            "bash -c \"git push\"",
            "git rebase main",
            "git switch main",
            "git checkout main",
            "git checkout -b feature",
            "git checkout -- a.rs && git checkout main",
            "git branch -d old",
            "git branch -D old",
            "git branch --delete old",
            "git branch -f x -D y",
            "git worktree add ../x",
            "git worktree remove ../x",
            "gh pr create",
            "GIT push",
        ] {
            assert!(
                executor_denies("shell", json!({"command": command, "purpose": "p"})).await,
                "{command}"
            );
        }
        for command in [
            "git status",
            "git diff --stat",
            "git log -5 --oneline",
            "git add -A",
            "git commit -m \"push the fix, merge later\"",
            "git checkout -- a.rs",
            "git checkout HEAD~1 -- a.rs b.rs",
            "git -C ../repo checkout abc123 -- src/",
            "git branch --list",
            "git branch -a -v",
            "cargo test --workspace",
            "echo digit push",
            "ls .git",
            "gh issue list",
        ] {
            assert!(
                !executor_denies("shell", json!({"command": command, "purpose": "p"})).await,
                "{command}"
            );
        }
        // The purpose text is not a command.
        assert!(
            !executor_denies(
                "shell",
                json!({"command": "ls", "purpose": "never git push"})
            )
            .await
        );
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

    #[test]
    fn legacy_ledger_without_usage_decodes_with_an_empty_accumulator() {
        // Regression: team.json files written before the durable usage
        // accumulator existed have no `usage` key; they must decode to an
        // empty accumulator instead of failing the ledger load.
        let dir = tempfile::tempdir().expect("ledger dir");
        let path = dir.path().join(TEAM_STATE_FILE);
        std::fs::write(
            &path,
            serde_json::json!({ "workers": [], "slots": 4 }).to_string(),
        )
        .expect("legacy ledger");
        let persistence = TeamPersistence::new(dir.path());
        let loaded = persistence
            .load()
            .expect("legacy ledger decodes")
            .expect("some ledger");
        assert_eq!(loaded.slots, Some(4));
        assert!(loaded.usage.models.is_empty());

        // A usage-bearing ledger round-trips its raw token rows verbatim.
        let mut usage = ProjectUsage::default();
        usage.record("glm-5.3", 1_000, 250, 100);
        persistence
            .save(&PersistedTeam {
                workers: Vec::new(),
                slots: Some(4),
                usage: usage.clone(),
            })
            .expect("usage-bearing save");
        let restored = persistence.load().expect("reload").expect("some ledger");
        assert_eq!(restored.usage, usage);
    }

    fn lifecycle_git(root: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("git process");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn cleanup_worker(
        id: &str,
        branch: &str,
        worktree: &Path,
        status: WorkerStatus,
    ) -> WorkerRecord {
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
        }))
        .expect("worker fixture")
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
        )
        .await
        .expect("clean worktree");
        let (dirty_path, dirty_branch) = provision_agent_worktree(
            root.to_path_buf(),
            "dirty002".to_string(),
            "wt/test".to_string(),
        )
        .await
        .expect("dirty worktree");
        std::fs::write(clean_path.join("file.txt"), "worker\n").expect("clean edit");
        lifecycle_git(&clean_path, &["add", "file.txt"]);
        lifecycle_git(&clean_path, &["commit", "-m", "clean change"]);
        std::fs::write(dirty_path.join("uncommitted.txt"), "keep me\n").expect("dirty edit");
        let clean = cleanup_worker(
            "clean001",
            &clean_branch,
            &clean_path,
            WorkerStatus::Stopped,
        );
        let dirty = cleanup_worker(
            "dirty002",
            &dirty_branch,
            &dirty_path,
            WorkerStatus::InReview,
        );
        let persistence = TeamPersistence::new(state.path());
        persistence
            .save(&PersistedTeam {
                workers: vec![clean.clone(), dirty.clone()],
                slots: Some(4),
                usage: ProjectUsage::default(),
            })
            .expect("ledger");

        let report = cleanup_workspace(root, state.path())
            .await
            .expect("cleanup");
        assert_eq!(report.removed, vec![clean_branch.clone()]);
        assert!(report
            .skipped
            .iter()
            .any(|reason| reason.contains("uncommitted")));
        assert!(!clean_path.exists());
        assert!(dirty_path.join("uncommitted.txt").exists());
        let restored = persistence.load().expect("ledger").expect("saved team");
        assert_eq!(restored.slots, Some(4));
        let cleaned = restored
            .workers
            .iter()
            .find(|worker| worker.id == clean.id)
            .expect("clean record");
        assert_eq!(cleaned.status, WorkerStatus::Cancelled);
        assert!(cleaned.worktree.as_os_str().is_empty());
        let dirty_after = restored
            .workers
            .iter()
            .find(|worker| worker.id == dirty.id)
            .expect("dirty record");
        assert_eq!(dirty_after.status, WorkerStatus::InReview);
        assert!(dirty_after.worktree.exists());
        let diff = crate::mnemo2::workbench::worker_diff(root, cleaned)
            .await
            .expect("the cleaned branch remains reviewable");
        assert_eq!(diff.total_files, 1);
        assert!(phase3::git_value(root, &["rev-parse", &clean_branch])
            .await
            .is_ok());
        assert!(phase3::git_value(root, &["rev-parse", &dirty_branch])
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn the_lead_clean_check_ignores_plans_but_the_worker_check_does_not() {
        let repo = tempfile::tempdir().expect("repo");
        let root = repo.path();
        lifecycle_git(root, &["init", "-b", "wt/test"]);
        lifecycle_git(root, &["config", "user.name", "Test"]);
        lifecycle_git(root, &["config", "user.email", "test@example.invalid"]);
        std::fs::create_dir_all(root.join(".coding/plans")).expect("plans dir");
        std::fs::create_dir_all(root.join(".coding/knowledge")).expect("knowledge dir");
        std::fs::write(root.join("file.txt"), "base").expect("base");
        std::fs::write(root.join(".coding/plans/tracked.md"), "plan").expect("tracked plan");
        std::fs::write(root.join(".coding/plans/moved.md"), "plan").expect("moved plan");
        lifecycle_git(root, &["add", "."]);
        lifecycle_git(root, &["commit", "-m", "base"]);

        // Edited, new, renamed, and staged plan files do not dirty the lead.
        std::fs::write(root.join(".coding/plans/tracked.md"), "edited").expect("edit plan");
        std::fs::write(root.join(".coding/plans/new.md"), "new").expect("new plan");
        lifecycle_git(
            root,
            &["mv", ".coding/plans/moved.md", ".coding/plans/renamed.md"],
        );
        phase3::require_clean_lead(root, "lead")
            .await
            .expect("plans are ignored");
        let strict = phase3::require_clean(root, "worker")
            .await
            .expect_err("strict check");
        assert!(strict.contains("must be clean"), "{strict}");

        // Knowledge records are lead bookkeeping too; a source change still
        // blocks.
        std::fs::write(root.join(".coding/knowledge/note.md"), "note").expect("knowledge");
        phase3::require_clean_lead(root, "lead")
            .await
            .expect("knowledge records are lead bookkeeping too");
        std::fs::remove_file(root.join(".coding/knowledge/note.md")).expect("remove note");
        std::fs::write(root.join("file.txt"), "changed").expect("change source");
        phase3::require_clean_lead(root, "lead")
            .await
            .expect_err("source change");
        lifecycle_git(root, &["checkout", "--", "file.txt"]);
        // A rename out of the plans dir is not a plan-only change.
        lifecycle_git(root, &["mv", ".coding/plans/renamed.md", "stolen.md"]);
        phase3::require_clean_lead(root, "lead")
            .await
            .expect_err("rename out of plans");
    }

    fn run_git(root: &std::path::Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_git(root: &std::path::Path) {
        run_git(root, &["init", "-b", "main"]);
        run_git(root, &["config", "user.name", "Mnemo2 Test"]);
        run_git(root, &["config", "user.email", "mnemo2@example.invalid"]);
        std::fs::write(root.join("README.md"), "seed\n").expect("seed file");
        std::fs::write(root.join(".gitignore"), ".worktrees/\n.coding/\n")
            .expect("ignore worktrees and state");
        run_git(root, &["add", "README.md", ".gitignore"]);
        run_git(root, &["commit", "-m", "seed"]);
    }

    fn worker(root: &std::path::Path, id: &str) -> WorkerRecord {
        let branch = agent_branch(id);
        WorkerRecord {
            id: id.to_string(),
            name: id.to_string(),
            brief: "test worker".to_string(),
            opening_prompt: None,
            model: None,
            model_id: None,
            reasoning_effort: None,
            branch,
            worktree: worker_checkout(root, id),
            status: WorkerStatus::Completed,
            generation: 0,
            transcript: Transcript::default(),
            pending: None,
            used_tokens: 0,
            max_tokens: 0,
            brief_sent: true,
            compacting: false,
            total_tokens: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
            cached_tokens: 0,
            requests: 0,
            cost_micros: None,
            created_at_ms: 0,
            elapsed_ms: 0,
            turns: 0,
            last_activity_ms: 0,
            checks: Vec::new(),
            review_risk: "standard".to_string(),
            kind: "implementation".to_string(),
            audit_target: None,
            audit_tip: None,
            findings: Vec::new(),
            review: None,
            fixer_finding_id: None,
            merge: None,
            error: None,
            plan_id: None,
            plan_step: None,
            tier: None,
        }
    }

    fn deregister_worktree(root: &std::path::Path, worker_id: &str) {
        let admin = root
            .join(".git")
            .join("worktrees")
            .join(format!("agent-{}", agent_short_id(worker_id)));
        std::fs::remove_dir_all(&admin).expect("remove the worktree admin data");
        run_git(root, &["worktree", "prune"]);
    }

    async fn open_test_team(
        root: &std::path::Path,
        state: &std::path::Path,
        workers: Vec<WorkerRecord>,
    ) -> TeamRuntime {
        let config = tempfile::tempdir().expect("config");
        std::fs::write(
            config.path().join("config.toml"),
            "[general]\ndefault_provider = \"test\"\ndefault_model = \"scripted-a\"\n",
        )
        .expect("config.toml");
        std::fs::write(
            config.path().join("endpoints.toml"),
            "[[endpoint]]
name = \"test\"
kind = \"local\"
base_url = \"http://127.0.0.1:9/v1/\"
models = [\"scripted-a\"]
",
        )
        .expect("endpoints.toml");
        TeamPersistence::new(state)
            .save(&PersistedTeam {
                workers,
                slots: Some(2),
                usage: ProjectUsage::default(),
            })
            .expect("save the team ledger");
        let engine = mnemo::headless::Engine::start(mnemo::headless::EngineOptions {
            workspace: mnemo::workspace::Workspace::new(root),
            config_dir: Some(config.path().to_path_buf()),
            permissions: mnemo::agent::policy::PermissionPolicy::OverridePermissions,
            memory: mnemo::headless::MemoryBackend::Disabled,
            index_codegraph: false,
            require_git: true,
            auto_install_embedder: false,
        })
        .await
        .expect("start the test engine");
        TeamRuntime::open(TeamRuntimeOptions {
            engine: Arc::new(engine),
            workspace_root: root.to_path_buf(),
            state_dir: state.to_path_buf(),
            slots: Some(2),
            models: Vec::new(),
            pricing: Vec::new(),
        })
        .await
        .expect("open the test team")
    }

    async fn spawn_live_worker(team: &TeamRuntime, worker: &WorkerRecord) {
        let session = team
            .spawn_worker_session(
                worker.name.clone(),
                None,
                None,
                worker.worktree.clone(),
                None,
                "implementation",
            )
            .await
            .expect("spawn worker session");
        team.live.lock().await.insert(
            worker.id.clone(),
            LiveWorker {
                session,
                pump: tokio::spawn(std::future::pending::<()>()),
                cleanup_retry_only: false,
                busy: Arc::new(AtomicBool::new(false)),
            },
        );
    }

    #[cfg(windows)]
    fn create_directory_link(link: &std::path::Path, target: &std::path::Path) {
        let output = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .expect("create a junction");
        assert!(
            output.status.success(),
            "mklink failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(unix)]
    fn create_directory_link(link: &std::path::Path, target: &std::path::Path) {
        std::os::unix::fs::symlink(target, link).expect("create a directory symlink");
    }

    #[tokio::test]
    async fn a_half_removed_checkout_is_finished_after_merge() {
        let root = tempfile::tempdir().expect("workspace");
        init_git(root.path());
        let worker = worker(root.path(), "00000001000000000001");
        provision_agent_worktree(
            root.path().to_path_buf(),
            worker.id.clone(),
            "main".to_string(),
        )
        .await
        .expect("provision the worker checkout");
        deregister_worktree(root.path(), &worker.id);
        assert!(worker.worktree.join(".git").is_file());

        cleanup_agent_worktree(root.path(), &worker)
            .await
            .expect("finish removing the half-removed checkout");

        assert!(!worker.worktree.exists());
    }

    #[tokio::test]
    async fn a_leftover_checkout_does_not_block_deleting_its_worker() {
        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        let dangling = worker(root.path(), "00000001000000000001");
        provision_agent_worktree(
            root.path().to_path_buf(),
            dangling.id.clone(),
            "main".to_string(),
        )
        .await
        .expect("provision the worker checkout");
        deregister_worktree(root.path(), &dangling.id);
        let fragment = worker(root.path(), "00000002000000000002");
        run_git(root.path(), &["branch", &fragment.branch, "main"]);
        std::fs::create_dir_all(&fragment.worktree).expect("fragment checkout");
        std::fs::write(fragment.worktree.join("fragment.txt"), "leftover\n")
            .expect("fragment file");
        let team = open_test_team(
            root.path(),
            state.path(),
            vec![dangling.clone(), fragment.clone()],
        )
        .await;
        for worker in [&dangling, &fragment] {
            let session = team
                .spawn_worker_session(
                    worker.name.clone(),
                    None,
                    None,
                    worker.worktree.clone(),
                    None,
                    "implementation",
                )
                .await
                .expect("open a worktree-bound session and its code graph");
            assert!(worker.worktree.join(".coding/codegraph.db").is_file());
            team.live.lock().await.insert(
                worker.id.clone(),
                LiveWorker {
                    session,
                    pump: tokio::spawn(async {}),
                    cleanup_retry_only: false,
                    busy: Arc::new(AtomicBool::new(false)),
                },
            );
        }
        let ids = vec![dangling.id.clone(), fragment.id.clone()];

        let previews = team.deletion_preview(&ids).await.expect("preview leftovers");
        assert!(previews.iter().all(|preview| preview.blocked.is_none()));
        let snapshot = team
            .delete_workers(&ids, false)
            .await
            .expect("delete workers with leftover checkouts");

        assert!(snapshot.workers.is_empty());
        assert!(team.live.lock().await.is_empty());
        assert!(!dangling.worktree.exists());
        assert!(!fragment.worktree.exists());
    }

    #[tokio::test]
    async fn leftover_removal_never_leaves_the_worker_folder() {
        let root = tempfile::tempdir().expect("workspace");
        let outside = tempfile::tempdir().expect("outside");
        init_git(root.path());
        let leftover = worker(root.path(), "00000001000000000001");
        std::fs::create_dir_all(&leftover.worktree).expect("leftover checkout");
        std::fs::write(leftover.worktree.join("fragment.txt"), "leftover\n")
            .expect("leftover file");
        let sibling = root.path().join(".worktrees").join("keep.txt");
        std::fs::write(&sibling, "keep\n").expect("sibling file");
        let main_marker = root.path().join("README.md");
        let original_main = std::fs::read(&main_marker).expect("main checkout marker");

        cleanup_agent_worktree(root.path(), &leftover)
            .await
            .expect("remove only the leftover checkout");
        assert!(!leftover.worktree.exists());
        assert_eq!(std::fs::read_to_string(&sibling).expect("sibling"), "keep\n");
        assert_eq!(
            std::fs::read(&main_marker).expect("main checkout survives"),
            original_main
        );

        let linked = worker(root.path(), "00000002000000000002");
        let outside_marker = outside.path().join("keep.txt");
        std::fs::write(&outside_marker, "outside\n").expect("outside marker");
        create_directory_link(&linked.worktree, outside.path());
        let error = cleanup_agent_worktree(root.path(), &linked)
            .await
            .expect_err("a junction or symlink at the checkout path is refused");
        assert!(error.contains("refus"), "unexpected error: {error}");
        assert!(linked.worktree.exists());
        assert_eq!(
            std::fs::read_to_string(&outside_marker).expect("outside marker survives"),
            "outside\n"
        );
    }

    #[tokio::test]
    async fn stop_close_error_is_persisted_and_retryable() {
        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        let worker = worker(root.path(), "00000001000000000001");
        provision_agent_worktree(
            root.path().to_path_buf(),
            worker.id.clone(),
            "main".to_string(),
        )
        .await
        .expect("provision worker");
        let team = open_test_team(root.path(), state.path(), vec![worker.clone()]).await;
        spawn_live_worker(&team, &worker).await;
        assert!(!team
            .live
            .lock()
            .await
            .get(&worker.id)
            .expect("pending live worker")
            .pump
            .is_finished());
        team.fail_next_close_for_test();

        let error = team.stop_inner(&worker.id).await.expect_err("injected close failure");
        assert!(error.contains("injected close failure"), "{error}");
        assert!(team.live.lock().await.contains_key(&worker.id));
        let persisted = TeamPersistence::new(state.path())
            .load()
            .expect("load team")
            .expect("saved team");
        assert!(persisted.workers[0]
            .error
            .as_deref()
            .is_some_and(|error| error.contains("injected close failure")));
        let busy = team
            .live
            .lock()
            .await
            .get(&worker.id)
            .expect("retained live worker")
            .busy
            .clone();
        for _ in 0..10 {
            if team
                .live
                .lock()
                .await
                .get(&worker.id)
                .expect("retained live worker")
                .pump
                .is_finished()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(team
            .live
            .lock()
            .await
            .get(&worker.id)
            .expect("retained live worker")
            .pump
            .is_finished());
        let message = team
            .message_inner(&worker.id, "must not run without a pump".into())
            .await
            .expect_err("a cleanup-retry-only worker rejects messages");
        assert!(message.contains("cleanup"), "{message}");
        let resume = team
            .resume_inner(&worker.id)
            .await
            .expect_err("a cleanup-retry-only worker rejects resume");
        assert!(resume.contains("cleanup"), "{resume}");
        let before_commands = team.worker(&worker.id).await.expect("worker before commands");
        let compact = team
            .compact(&worker.id)
            .await
            .expect_err("a cleanup-retry-only worker rejects compact");
        assert!(compact.contains("cleanup"), "{compact}");
        let after_compact = team.worker(&worker.id).await.expect("worker after compact");
        assert_eq!(after_compact.status, before_commands.status);
        assert_eq!(after_compact.compacting, before_commands.compacting);
        assert!(!busy.load(Ordering::SeqCst));
        let pause = team
            .pause(&worker.id)
            .await
            .expect_err("a cleanup-retry-only worker rejects pause");
        assert!(pause.contains("cleanup"), "{pause}");
        let interrupt = team
            .interrupt(&worker.id)
            .await
            .expect_err("a cleanup-retry-only worker rejects interrupt");
        assert!(interrupt.contains("cleanup"), "{interrupt}");
        let after_commands = team.worker(&worker.id).await.expect("worker after commands");
        assert_eq!(after_commands.status, before_commands.status);
        assert_eq!(after_commands.compacting, before_commands.compacting);
        assert!(!busy.load(Ordering::SeqCst));

        let stopped = team.stop_inner(&worker.id).await.expect("stop retry");
        assert_eq!(stopped.status, WorkerStatus::Stopped);
        assert!(!team.live.lock().await.contains_key(&worker.id));
    }

    #[tokio::test]
    async fn merge_close_error_returns_to_review_and_retries_cleanup() {
        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        run_git(root.path(), &["checkout", "-b", "wt/mnemo"]);
        let mut worker = worker(root.path(), "00000001000000000001");
        worker.status = WorkerStatus::InReview;
        provision_agent_worktree(
            root.path().to_path_buf(),
            worker.id.clone(),
            "wt/mnemo".to_string(),
        )
        .await
        .expect("provision worker");
        std::fs::write(worker.worktree.join("merged.txt"), "merged\n").expect("worker change");
        run_git(&worker.worktree, &["add", "merged.txt"]);
        run_git(&worker.worktree, &["commit", "-m", "worker change"]);
        let tip = phase3::tip(&worker.worktree).await.expect("worker tip");
        worker.checks = vec![WorkerCheck {
            name: "cargo test --workspace".into(),
            status: "passed".into(),
            detail: None,
            tip: tip.clone(),
        }];
        worker.review = Some(ReviewRecord {
            summary: "reviewed".into(),
            tip,
            reviewed_at_ms: 1,
            evidence: Some(evidence()),
        });
        let team = open_test_team(root.path(), state.path(), vec![worker.clone()]).await;
        spawn_live_worker(&team, &worker).await;
        team.fail_next_close_for_test();

        let failed = team.merge(&worker.id).await.expect("merge close diagnostic");
        assert_eq!(failed.status, WorkerStatus::InReview);
        assert_eq!(failed.merge, None);
        assert!(failed
            .error
            .as_deref()
            .is_some_and(|error| error.contains("retry merge")));
        assert!(team.live.lock().await.contains_key(&worker.id));
        assert!(worker.worktree.exists());
        let persisted = TeamPersistence::new(state.path())
            .load()
            .expect("load team")
            .expect("saved team");
        assert_eq!(persisted.workers[0].status, WorkerStatus::InReview);
        assert!(persisted.workers[0].error.is_some());

        let merged = team.merge(&worker.id).await.expect("merge retry");
        assert_eq!(merged.status, WorkerStatus::Merged);
        assert_eq!(merged.error, None);
        assert!(!worker.worktree.exists());
        assert!(!team.live.lock().await.contains_key(&worker.id));
    }

    #[tokio::test]
    async fn merge_closes_a_live_session_before_finishing_its_checkout() {
        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        run_git(root.path(), &["checkout", "-b", "wt/mnemo"]);
        let mut worker = worker(root.path(), "00000001000000000001");
        worker.status = WorkerStatus::InReview;
        provision_agent_worktree(
            root.path().to_path_buf(),
            worker.id.clone(),
            "wt/mnemo".to_string(),
        )
        .await
        .expect("provision the worker checkout");
        std::fs::write(worker.worktree.join("merged.txt"), "merged\n").expect("worker change");
        run_git(&worker.worktree, &["add", "merged.txt"]);
        run_git(&worker.worktree, &["commit", "-m", "worker change"]);
        let tip = phase3::tip(&worker.worktree).await.expect("worker tip");
        worker.checks = vec![WorkerCheck {
            name: "cargo test --workspace".into(),
            status: "passed".into(),
            detail: None,
            tip: tip.clone(),
        }];
        worker.review = Some(ReviewRecord {
            summary: "reviewed".into(),
            tip,
            reviewed_at_ms: 1,
            evidence: Some(evidence()),
        });
        let team = open_test_team(root.path(), state.path(), vec![worker.clone()]).await;
        let session = team
            .spawn_worker_session(
                worker.name.clone(),
                None,
                None,
                worker.worktree.clone(),
                None,
                "implementation",
            )
            .await
            .expect("open a worktree-bound session and its code graph");
        assert!(worker.worktree.join(".coding/codegraph.db").is_file());
        team.live.lock().await.insert(
            worker.id.clone(),
            LiveWorker {
                session,
                pump: tokio::spawn(async {}),
                cleanup_retry_only: false,
                busy: Arc::new(AtomicBool::new(false)),
            },
        );

        let merged = team.merge(&worker.id).await.expect("merge the worker");

        assert_eq!(merged.status, WorkerStatus::Merged);
        assert_eq!(merged.error, None);
        assert!(!worker.worktree.exists());
        assert!(!team.live.lock().await.contains_key(&worker.id));
        assert_eq!(
            std::fs::read_to_string(root.path().join("merged.txt"))
                .expect("merged file")
                .trim_end(),
            "merged"
        );
        let persisted = TeamPersistence::new(state.path())
            .load()
            .expect("load persisted team")
            .expect("saved team");
        assert_eq!(persisted.workers[0].error, None);
    }

    #[test]
    fn porcelain_worktree_paths_are_nul_delimited_and_fail_closed() {
        let paths = worktree_list_paths(b"worktree /tmp/main\0worktree /tmp/agent\0\0")
            .expect("decode porcelain paths");
        assert_eq!(
            paths,
            vec![PathBuf::from("/tmp/main"), PathBuf::from("/tmp/agent")]
        );

        #[cfg(not(unix))]
        assert!(worktree_list_paths(b"worktree \xff\0").is_err());

        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;

            assert_eq!(
                worktree_list_paths(b"worktree \xff\0").expect("Unix paths are bytes"),
                vec![PathBuf::from(std::ffi::OsString::from_vec(vec![0xff]))]
            );
        }
    }

    /// Mnemo1 owns `runall-*` worktrees and `wt/runall-*` branches in the same
    /// `.worktrees/` directory; Mnemo2's cleanup only ever acts on its own
    /// `agent-*` records and refuses anything else, even a forged record.
    #[tokio::test]
    async fn cleanup_never_touches_mnemo1_runall_worktrees() {
        let repo = tempfile::tempdir().expect("repo");
        let state = tempfile::tempdir().expect("state");
        let root = repo.path();
        lifecycle_git(root, &["init", "-b", "main"]);
        lifecycle_git(root, &["config", "user.name", "Test"]);
        lifecycle_git(root, &["config", "user.email", "test@example.invalid"]);
        std::fs::write(root.join("file.txt"), "base").expect("base");
        lifecycle_git(root, &["add", "file.txt"]);
        lifecycle_git(root, &["commit", "-m", "base"]);
        lifecycle_git(root, &["checkout", "-b", "wt/lead"]);
        let (runall_path, runall_branch) = mnemo::app::worktrees::provision_item_worktree(
            root.to_path_buf(),
            "abcd1234-item".to_string(),
        )
        .await
        .expect("runall worktree");
        assert!(runall_branch.starts_with("wt/runall-"), "{runall_branch}");
        assert!(runall_path.ends_with("runall-abcd1234"), "{runall_path:?}");
        let (agent_path, agent_branch) = provision_agent_worktree(
            root.to_path_buf(),
            "agent001".to_string(),
            "wt/lead".to_string(),
        )
        .await
        .expect("agent worktree");
        let agent = cleanup_worker(
            "agent001",
            &agent_branch,
            &agent_path,
            WorkerStatus::Stopped,
        );
        // A forged record pointing at the runall checkout is refused.
        let forged = cleanup_worker(
            "abcd1234",
            &runall_branch,
            &runall_path,
            WorkerStatus::Stopped,
        );
        TeamPersistence::new(state.path())
            .save(&PersistedTeam {
                workers: vec![agent, forged],
                slots: None,
                usage: ProjectUsage::default(),
            })
            .expect("ledger");

        let report = cleanup_workspace(root, state.path())
            .await
            .expect("cleanup");
        assert_eq!(report.removed, vec![agent_branch.clone()]);
        assert!(!agent_path.exists());
        assert!(runall_path.is_dir(), "the Mnemo1 checkout survives");
        assert!(phase3::git_value(root, &["rev-parse", &runall_branch])
            .await
            .is_ok());
        assert!(
            report
                .skipped
                .iter()
                .any(|reason| reason.contains("invalid worker identity")),
            "{:?}",
            report.skipped
        );
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

    /// Complete structured review evidence for the merge-gate fixtures.
    fn evidence() -> ReviewEvidence {
        ReviewEvidence {
            unanchored_pass: true,
            intent: "the worker implements the scoped brief".into(),
            execution: "the change flows through the documented path".into(),
            integration: "the module fits the surrounding runtime".into(),
            test_quality: "the regression reproduces the defect before the fix".into(),
            residual_risk: "no residual risk beyond the reviewed diff".into(),
            independent_reviewer_id: None,
            audit_adjudication: None,
        }
    }

    /// A worker parked in review at one exact tip with passing checks.
    fn in_review_worker(evidence: Option<ReviewEvidence>) -> WorkerRecord {
        let mut worker = cleanup_worker(
            "evidence0001",
            "wt/agent-evidence0001",
            Path::new("unused"),
            WorkerStatus::InReview,
        );
        worker.checks = vec![WorkerCheck {
            name: "cargo test --workspace".into(),
            status: "passed".into(),
            detail: None,
            tip: "tip0001".into(),
        }];
        worker.review = Some(ReviewRecord {
            summary: "reviewed".into(),
            tip: "tip0001".into(),
            reviewed_at_ms: 1,
            evidence,
        });
        worker
    }

    #[test]
    fn legacy_workers_decode_as_standard_risk_without_evidence() {
        let worker = in_review_worker(None);
        assert_eq!(worker.review_risk, "standard");
        // A pre-evidence ledger round-trips its review without the new field.
        let value = serde_json::to_value(&worker).expect("worker JSON");
        let restored: WorkerRecord =
            serde_json::from_value(value).expect("legacy-shaped worker decodes");
        assert!(restored
            .review
            .as_ref()
            .expect("review")
            .evidence
            .is_none());
    }

    #[test]
    fn merge_gate_requires_substantive_review_evidence() {
        // A summary plus passing checks alone can no longer merge: the
        // legacy-shaped review (evidence absent) is rejected.
        let unevidenced = in_review_worker(None);
        let error = validate_merge(&unevidenced, "tip0001", &[])
            .expect_err("a legacy review must not merge without fresh evidence");
        assert!(error.contains("structured evidence"), "{error}");

        // An anchored-only pass is not substantive either.
        let mut unanchored = evidence();
        unanchored.unanchored_pass = false;
        let rejected = validate_merge(&in_review_worker(Some(unanchored)), "tip0001", &[])
            .expect_err("unanchored evidence must not merge");
        assert!(rejected.contains("unanchored"), "{rejected}");

        // Empty assessments are rejected.
        let mut empty_intent = evidence();
        empty_intent.intent = "  ".into();
        let empty = validate_merge(&in_review_worker(Some(empty_intent)), "tip0001", &[])
            .expect_err("empty intent must not merge");
        assert!(empty.contains("intent"), "{empty}");

        // Complete evidence passes the gate for standard-risk work.
        validate_merge(&in_review_worker(Some(evidence())), "tip0001", &[])
            .expect("complete evidence merges");
    }

    /// One audit-worker record for linkage tests.
    fn audit_worker(status: WorkerStatus, target: &str, tip: &str) -> WorkerRecord {
        let mut worker = cleanup_worker(
            "auditor0001",
            "wt/agent-auditor0001",
            Path::new("unused"),
            status,
        );
        worker.kind = "audit".into();
        worker.audit_target = Some(target.into());
        worker.audit_tip = Some(tip.into());
        worker
    }

    #[test]
    fn legacy_workers_decode_as_implementation_kind_without_audit_fields() {
        let mut worker = in_review_worker(Some(evidence()));
        worker.kind = "implementation".into();
        worker.audit_target = None;
        worker.audit_tip = None;
        // A phase-three ledger written before audits has none of the new
        // fields serialized; it must decode as a plain implementation worker.
        let legacy = json!({
            "id": "00000001000000000001",
            "name": "Worker 00000001",
            "brief": "implement the task",
            "model": "test/scripted-a",
            "model_id": "scripted-a",
            "branch": "wt/agent-00000001000000000001",
            "worktree": "",
            "status": "in_review",
            "generation": 0,
            "transcript": { "messages": [] },
            "used_tokens": 0,
            "max_tokens": 0,
            "brief_sent": true,
            "compacting": false,
            "created_at_ms": 1,
            "elapsed_ms": 1,
            "turns": 1,
            "last_activity_ms": 1,
            "checks": [],
            "findings": [],
        });
        let restored: WorkerRecord =
            serde_json::from_value(legacy).expect("pre-audit ledger decodes");
        assert_eq!(restored.kind, "implementation");
        assert!(restored.audit_target.is_none());
        assert!(restored.audit_tip.is_none());
        assert!(restored.reasoning_effort.is_none());
        // A modern audit record keeps its fields through a round-trip.
        let audit = audit_worker(WorkerStatus::Completed, "evidence0001", "tip0001");
        let audit_value = serde_json::to_value(&audit).expect("audit JSON");
        assert!(audit_value.get("kind").and_then(Value::as_str) == Some("audit"));
        let restored_audit: WorkerRecord =
            serde_json::from_value(audit_value).expect("audit worker round-trips");
        assert_eq!(restored_audit.kind, "audit");
        assert_eq!(restored_audit.audit_target.as_deref(), Some("evidence0001"));
        assert_eq!(restored_audit.audit_tip.as_deref(), Some("tip0001"));
    }

    #[test]
    fn high_risk_merge_gate_requires_a_fresh_completed_audit() {
        let records_empty: Vec<WorkerRecord> = Vec::new();

        // High-risk work cannot merge on the lead's evidence alone.
        let mut high = in_review_worker(Some(evidence()));
        high.review_risk = "high".into();
        let missing = validate_merge(&high, "tip0001", &records_empty)
            .expect_err("high-risk work needs an independent audit");
        assert!(
            missing.contains("high-risk reviews require an independent reviewer"),
            "{missing}"
        );

        // A linked adjudication without the tracked audit is refused.
        let mut untracked_evidence = evidence();
        untracked_evidence.independent_reviewer_id = Some("auditor0001".into());
        untracked_evidence.audit_adjudication = Some("accepted".into());
        let mut untracked_worker = in_review_worker(Some(untracked_evidence));
        untracked_worker.review_risk = "high".into();
        let untracked = validate_merge(&untracked_worker, "tip0001", &records_empty)
            .expect_err("the linked reviewer must be a tracked worker");
        assert!(untracked.contains("not a tracked worker"), "{untracked}");

        // Linking a tracked implementation worker is refused: only
        // read-only audit workers may be linked.
        let mut implementer = cleanup_worker(
            "auditor0001",
            "wt/agent-auditor0001",
            Path::new("unused"),
            WorkerStatus::Completed,
        );
        implementer.kind = "implementation".into();
        let wrong_kind = validate_merge(&untracked_worker, "tip0001", &[implementer])
            .expect_err("an implementation worker is not an independent audit");
        assert!(wrong_kind.contains("not a read-only audit worker"), "{wrong_kind}");

        // An audit still working cannot be linked.
        let mut live = audit_worker(WorkerStatus::Working, "evidence0001", "tip0001");
        live.status = WorkerStatus::Working;
        let incomplete = validate_merge(&untracked_worker, "tip0001", &[live])
            .expect_err("an unfinished audit must not merge");
        assert!(incomplete.contains("not completed"), "{incomplete}");

        // An audit of another worker is refused.
        let foreign = audit_worker(WorkerStatus::Completed, "other0001", "tip0001");
        let mismatched = validate_merge(&untracked_worker, "tip0001", &[foreign])
            .expect_err("a foreign audit must not link");
        assert!(mismatched.contains("reviews 'other0001'"), "{mismatched}");

        // An audit at an older tip is stale after a fix commit moved it.
        let stale = audit_worker(WorkerStatus::Completed, "evidence0001", "tip0000");
        let stale_error = validate_merge(&untracked_worker, "tip0001", &[stale])
            .expect_err("a stale audit must block the merge");
        assert!(stale_error.contains("stale"), "{stale_error}");

        // Linkage without a written adjudication is refused even for
        // standard-risk work.
        let mut unadjudicated = evidence();
        unadjudicated.independent_reviewer_id = Some("auditor0001".into());
        unadjudicated.audit_adjudication = None;
        let standard = in_review_worker(Some(unadjudicated));
        let unadjudicated_error = validate_merge(
            &standard,
            "tip0001",
            &[audit_worker(WorkerStatus::Completed, "evidence0001", "tip0001")],
        )
        .expect_err("linking an audit requires an adjudication");
        assert!(
            unadjudicated_error.contains("audit_adjudication"),
            "{unadjudicated_error}"
        );

        // A completed audit at the exact merge tip plus a written
        // adjudication merges.
        let mut adjudicated = evidence();
        adjudicated.independent_reviewer_id = Some("auditor0001".into());
        adjudicated.audit_adjudication = Some("accepted".into());
        let mut reviewed = in_review_worker(Some(adjudicated));
        reviewed.review_risk = "high".into();
        validate_merge(
            &reviewed,
            "tip0001",
            &[audit_worker(WorkerStatus::Completed, "evidence0001", "tip0001")],
        )
        .expect("a fresh completed audit with an adjudication merges");
    }

    #[test]
    fn high_risk_dispatch_requires_observable_acceptance_scenarios() {
        let no_section =
            require_acceptance_scenarios("planid01", "# Plan\n\n## Goal\nShip it.\n")
                .expect_err("a plan without the section must block high-risk dispatch");
        assert!(no_section.contains("plan 'planid01' does not declare"), "{no_section}");
        assert!(no_section.contains("add_acceptance_scenarios"), "{no_section}");

        // A heading the board escaped into the context is prose, not the
        // plan's section.
        let escaped = require_acceptance_scenarios(
            "planid01",
            "# Plan\n\n## Context\n\\## Acceptance scenarios\n- A restart reloads every record.\n",
        )
        .expect_err("an escaped heading must not count as the section");
        assert!(escaped.contains("does not declare"), "{escaped}");

        let no_bullet = require_acceptance_scenarios(
            "planid01",
            "# Plan\n\n## Acceptance scenarios\n\n- x\n\nNone yet.\n",
        )
        .expect_err("a section without observable bullets must block high-risk dispatch");
        assert!(no_bullet.contains("no observable bullet"), "{no_bullet}");

        require_acceptance_scenarios(
            "planid01",
            "# Plan\n\n## Acceptance scenarios\n\n- A standard worker cannot merge with only a summary and passing checks.\n\n## Steps\n- done\n",
        )
        .expect("observable bullets validate high-risk dispatch");
    }
}
