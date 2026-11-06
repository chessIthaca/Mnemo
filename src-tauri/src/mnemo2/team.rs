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
use mnemo::route_question;
use mnemo::config::endpoints::{Tier, TierLadder};
use mnemo::headless::{
    AgentSession, Engine, EventReceiver, GateRule, GateVerdict, Guardrails, HostToolSpec,
    PendingInteraction, QuestionPolicy, SessionCommand, SessionEvent, SessionId, SpawnOptions,
    ToolSelection, Transcript, TurnEndPolicy,
};
use mnemo::headless::session::ModelSwitch;
use mnemo::headless::{ConstitutionMode, PromptBlock, PromptSpec};
use mnemo::EmbedderGetter;
use mnemo::RouteKnn;
use mnemo::memory::classifier::ClassifierSlot;
use mnemo::tool::{SafetyLevel, ToolResult};
use mnemo::provider::trace::{TraceActorKind, TraceIdentity};
use mnemo::{PermissionPolicy, SerializableAgentEvent};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{broadcast, Mutex};
use tokio::task::JoinHandle;

use crate::mnemo2::dispatch_routing::{
    classify_dispatch, load_routing_settings, outcome_label, routing_text, threshold_for,
    DispatchRoutingDecision, DispatchRoutingRow, EnabledTiers, RoutingMode, RoutingSettings,
    TEXT_MAX_CHARS,
};
use crate::mnemo2::ledger::{self, RecordsLedger};
use crate::mnemo2::lock_holders;
use crate::mnemo2::phase3::{self, Finding, MergeRecord, ReviewEvidence, ReviewRecord};
use crate::mnemo2::runtime::LeadModel;
use crate::mnemo2::pricing::usage_model_key;
use crate::mnemo2::usage::{event_session, ProjectUsage, ProjectUsageView};

/// The Tauri event channel emitted by the team runtime.
pub const TEAM_EVENT_CHANNEL: &str = "mnemo2://team-event";

/// The Tauri event channel streaming per-worker deletion-flow progress
/// (`DeletionProgress`) while `team_delete_preview` or `team_delete` runs.
pub const DELETION_EVENT_CHANNEL: &str = "mnemo2://worker-deletion";

/// The persisted worker ledger inside the team's state directory.
const TEAM_STATE_FILE: &str = "team.json";
/// Original bytes preserved before the one-time legacy tier-id conversion.
const TEAM_PRE_TIERS_BACKUP: &str = "team.json.pre-tiers.bak";

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

/// Who sent a message to a worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageSender {
    /// Claude Code leading through Mnemo2's MCP endpoint.
    ExternalLead,
    /// Mnemo2's built-in lead session.
    Lead,
    /// The user replying in mission control.
    User,
    /// Mnemo2 itself, for a generated repair or route-fix brief or a plan-changed notice.
    Mnemo,
}

impl MessageSender {
    fn header(self) -> &'static str {
        match self {
            Self::ExternalLead => "[From the lead (Claude Code)]",
            Self::Lead => "[From the built-in lead]",
            Self::User => "[From the user in mission control]",
            Self::Mnemo => "[From Mnemo2]",
        }
    }
}

/// The purpose or delivery form of a message sent to a worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InboxKind {
    /// A prompt sent to an idle worker.
    Prompt,
    /// Guidance queued while the worker is busy.
    Steer,
    /// A worker brief.
    Brief,
    /// A generated review-fix brief.
    Fix,
}

/// One durable lead-to-worker or system-to-worker message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxEntry {
    /// Who sent the message.
    pub sender: MessageSender,
    /// The message's purpose or delivery form.
    pub kind: InboxKind,
    /// The original text, without the delivery header.
    pub text: String,
    /// Unix epoch milliseconds when Mnemo2 accepted the message.
    pub at_ms: u128,
}

/// The one dispatch-time routing decision persisted with a worker.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkerRouting {
    /// The tier Laya selected, when it returned a known label.
    pub laya_tier: Option<String>,
    /// Laya's confidence, when it answered.
    pub confidence: Option<f64>,
    /// Whether the answer cleared the configured gate.
    pub cleared: bool,
    /// The explicit or fallback tier supplied by the lead, when one was used.
    pub lead_tier: Option<String>,
    /// Who selected the running tier: `lead`, `laya`, or `lead_override`.
    pub routed_by: String,
    /// The tier the worker actually started on at dispatch, frozen beside the
    /// decision so a later manual reroute cannot rewrite what a re-dispatch
    /// reports (audit finding M1).
    #[serde(default)]
    pub ran_tier: Option<String>,
    /// The one nudge sent for this worker, if any.
    pub nudged: Option<String>,
}

/// Who moved a worker to another model: the lead through `team_resume`, or
/// the user through mission control. The reroute row logged beside the
/// switch carries the source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeSource {
    /// A lead-side switch: `team_resume` with a tier or model.
    Lead,
    /// The user's model change in mission control.
    User,
    /// An automatic switch after a review, audit, or strike-streak nudge.
    Escalation,
    /// A fix round deliberately running on a cheaper tier.
    FixDownshift,
    /// The restoration of the prior model after a failed fix downshift.
    FixDownshiftRestore,
}

impl ChangeSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Lead => "lead",
            Self::User => "user",
            Self::Escalation => "escalation",
            Self::FixDownshift => "fix_downshift",
            Self::FixDownshiftRestore => "fix_downshift_restore",
        }
    }
}

/// One failed tool call in a worker's current failing streak: the tool's
/// name and its cleaned output.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolErrorNote {
    /// The tool that failed.
    pub tool: String,
    /// The failed call's output: whitespace collapsed and length-capped.
    pub output: String,
}

/// One worker's durable record. It is the UI's truth and the recovery ledger.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkerRecord {
    /// Stable worker id (the first 8 chars name the branch and worktree).
    pub id: String,
    /// Messages sent to this worker, oldest first and capped at the latest 200.
    #[serde(default)]
    pub inbox: Vec<InboxEntry>,
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
    /// The worker's first turn is a read-back: it reports questions about
    /// its spec and waits for the lead before changing files.
    #[serde(default)]
    pub read_back: bool,
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
    /// The last cost-alert threshold the lead was told about, in millionths
    /// of a dollar: the notice fires once at `cost_notice_usd`, then at each
    /// doubling. 0 when no notice has gone out yet.
    #[serde(default)]
    pub cost_notified_micros: u64,
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
    /// An audit worker delivered its read-only report; its job is over.
    #[serde(default)]
    pub report_delivered: bool,
    /// Durable lead findings.
    #[serde(default)]
    pub findings: Vec<Finding>,
    /// Review of the current worker tip, invalidated by every fix commit.
    #[serde(default)]
    pub review: Option<ReviewRecord>,
    /// Finding currently routed to the tracked fixer.
    #[serde(default)]
    pub fixer_finding_id: Option<String>,
    /// Review rounds: `team_review` calls recorded on this worker.
    #[serde(default)]
    pub review_rounds: u32,
    /// Fix rounds: `route_fix` and `review_fix` applications on this worker.
    #[serde(default)]
    pub fix_rounds: u32,
    /// Independent audits dispatched against this worker.
    #[serde(default)]
    pub audits: u32,
    /// The model specs this worker's sessions ran on, in order, with no
    /// consecutive duplicates.
    #[serde(default)]
    pub models_used: Vec<String>,
    /// Successful local merge and optional revert.
    #[serde(default)]
    pub merge: Option<MergeRecord>,
    /// The last error, when the worker failed or conflicted.
    pub error: Option<String>,
    /// The current streak of failed tool calls, oldest first, cleared by a
    /// successful call. Only the latest [`TOOL_ERRORS_KEPT`] are kept, each
    /// output capped at [`TOOL_ERROR_CHARS`] characters.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_errors: Vec<ToolErrorNote>,
    /// The board plan this task was dispatched under (validated by the plan
    /// board at dispatch).
    #[serde(default)]
    pub plan_id: Option<String>,
    /// The 0-based plan step the task was dispatched under.
    #[serde(default)]
    pub plan_step: Option<usize>,
    /// The resolved ladder tier (`trivial`, `routine`, `medium`, `high`, or
    /// `escalate`) the worker ran on, including later model switches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    /// The dispatch-time Laya decision, absent for legacy and unrouted workers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing: Option<WorkerRouting>,
    /// The `lead_reroute` / `user_reroute` rows this worker logged, in order:
    /// the reroute half of its routing history, kept beside the record so the
    /// outcome label and the Effort table read the worker without scanning
    /// the log.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reroutes: Vec<DispatchRoutingRow>,
}

/// The opening prompt of a worker that implements its pinned task straight
/// away.
pub(crate) const KICKOFF: &str = "Your task is pinned in your instructions under \"Your task (pinned)\": the plan, your step's spec and the lead's brief. Implement it exactly as specified. If the spec contradicts the code or leaves a decision open, message the lead (message_parent) before guessing.";

/// The opening prompt of a read-back worker: it reads its pinned task and the
/// files it names, asks its questions, and changes nothing until the lead
/// answers.
pub(crate) const READ_BACK_KICKOFF: &str = "Your task is pinned in your instructions under \"Your task (pinned)\". Before changing any file, read the spec and every file it names. Then send the lead one message (message_parent) listing each ambiguity, contradiction with the code, or decision the spec leaves open, numbered, with file and line, or exactly \"No questions.\" Then stop and wait. Start implementing only after the lead replies.";

/// The first user message a worker's session receives: the short kickoff that
/// points at the task pinned in the system head.
fn kickoff_message(record: &WorkerRecord) -> &'static str {
    if record.read_back {
        READ_BACK_KICKOFF
    } else {
        KICKOFF
    }
}

/// The model a worker's session actually runs on: the lead's chosen spec, else
/// the id the live session resolved, else the routing default.
fn session_model_spec(worker: &WorkerRecord) -> &str {
    worker
        .model
        .as_deref()
        .filter(|spec| !spec.trim().is_empty())
        .or_else(|| {
            worker
                .model_id
                .as_deref()
                .filter(|id| !id.trim().is_empty())
        })
        .unwrap_or("default")
}

/// Record the model a worker's session actually runs on, in order of use and
/// without consecutive duplicates: a resume on the same model adds nothing.
fn note_model(worker: &mut WorkerRecord, spec: &str) {
    if worker.models_used.last().map(String::as_str) != Some(spec) {
        worker.models_used.push(spec.to_string());
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

struct RoutedDispatch {
    model: Option<String>,
    tier: Option<String>,
    reasoning_effort: Option<String>,
    routing: Option<WorkerRouting>,
    rows: Vec<DispatchRoutingRow>,
    error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DispatchSource {
    LeadTool,
    Ipc,
}

/// One dispatch request's knobs, threaded through the dispatch path together.
#[derive(Clone, Debug)]
struct DispatchRequest {
    /// The lead's task-specific instructions.
    brief: String,
    /// The requested tier, including `auto` where the mode offers it.
    tier: Option<String>,
    /// Whether this implementation dispatch uses dispatch-time routing.
    dispatch_routing: bool,
    /// Which public entry point supplied the dispatch.
    source: DispatchSource,
    /// The exact model spec, when the lead chose one.
    model: Option<String>,
    /// The chosen reasoning effort, when the lead chose one.
    reasoning_effort: Option<String>,
    /// The stable worker id, when the lead named one.
    worker_id: Option<String>,
    /// The plan step the task belongs to, when it is on the board.
    plan: Option<PlanRef>,
    /// The lead-assigned review risk.
    review_risk: Option<String>,
    /// Whether the first turn reads the spec back and asks questions.
    read_back: bool,
}

/// The inputs one live worker session needs: where and on what it runs, its
/// transcript to resume from, its trace identity, and the task pinned into
/// its system head.
pub(crate) struct WorkerSessionSpec {
    /// The exact model spec, when the lead chose one.
    pub(crate) model: Option<String>,
    /// The chosen reasoning effort, when the lead chose one.
    pub(crate) reasoning_effort: Option<String>,
    /// The checkout the session is bound to.
    pub(crate) worktree: PathBuf,
    /// The saved transcript to resume from.
    pub(crate) resume: Option<Transcript>,
    /// `implementation` or `audit`, selecting the guardrails.
    pub(crate) kind: String,
    /// The trace identity every model call is attributed to.
    pub(crate) trace_identity: TraceIdentity,
    /// The task rendered into the system head for the session's whole life.
    pub(crate) pinned_task: Option<String>,
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

/// Whether an audit worker's job is over: its report was recorded, or a
/// ledger written before `report_delivered` existed merely records it as
/// `Completed`. A finished audit is never resumed, compacted, messaged or
/// model-changed — dispatch a fresh audit instead.
fn audit_finished(record: &WorkerRecord) -> bool {
    record.kind == "audit"
        && (record.report_delivered || record.status == WorkerStatus::Completed)
}

/// The refusal every finished-audit gate carries.
fn audit_finished_error(worker_id: &str) -> String {
    format!(
        "audit worker '{worker_id}' already delivered its report; dispatch a new audit with team_audit_dispatch"
    )
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
    /// The lead's config directory, which holds Mnemo2's `mnemo2.toml`.
    pub config_dir: PathBuf,
    /// The engine's live Laya classifier slot, shared with every session.
    pub classifier: Option<ClassifierSlot>,
    /// Test-only routing-log override; production uses the global training log.
    pub routing_log_path: Option<PathBuf>,
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
        let loaded = match Self::decode(&self.path) {
            Ok(Some(team)) => Some((team, self.path.clone())),
            Ok(None) => Self::decode(&backup)?.map(|team| (team, backup.clone())),
            Err(primary_error) => match Self::decode(&backup) {
                Ok(Some(team)) => Some((team, backup.clone())),
                Ok(None) => return Err(primary_error),
                Err(backup_error) => {
                    return Err(format!(
                        "{primary_error}; backup also failed: {backup_error}"
                    ));
                }
            },
        };
        let Some((mut team, source)) = loaded else {
            return Ok(None);
        };
        if convert_legacy_team_tiers(&mut team) {
            let pre_tiers = self.path.with_file_name(TEAM_PRE_TIERS_BACKUP);
            if !pre_tiers.exists() {
                std::fs::copy(&source, &pre_tiers).map_err(|error| {
                    format!(
                        "could not preserve legacy team ledger as {}: {error}",
                        pre_tiers.display()
                    )
                })?;
            }
            self.save(&team)?;
        }
        Ok(Some(team))
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

/// Convert one stored tier id, returning whether it changed.
fn convert_legacy_tier(tier: &mut String) -> bool {
    let Some(converted) = TierLadder::convert_old_id(tier) else {
        return false;
    };
    *tier = converted.to_string();
    true
}

/// Convert an optional stored tier id, returning whether it changed.
fn convert_legacy_optional_tier(tier: &mut Option<String>) -> bool {
    tier.as_mut().is_some_and(convert_legacy_tier)
}

/// Convert every tier id embedded in one historical routing row.
fn convert_legacy_row_tiers(row: &mut DispatchRoutingRow) -> bool {
    let mut changed = false;
    match row {
        DispatchRoutingRow::Route {
            tier,
            lead_tier,
            ran_tier,
            ..
        } => {
            changed |= convert_legacy_optional_tier(tier);
            changed |= convert_legacy_optional_tier(lead_tier);
            changed |= convert_legacy_optional_tier(ran_tier);
        }
        DispatchRoutingRow::LeadOverride {
            laya_tier,
            lead_tier,
            requested_tier,
            ..
        } => {
            changed |= convert_legacy_optional_tier(laya_tier);
            changed |= convert_legacy_tier(lead_tier);
            changed |= convert_legacy_optional_tier(requested_tier);
        }
        DispatchRoutingRow::LeadReroute {
            from_tier,
            to_tier,
            ..
        }
        | DispatchRoutingRow::UserReroute {
            from_tier,
            to_tier,
            ..
        }
        | DispatchRoutingRow::Nudge {
            from_tier,
            to_tier,
            ..
        }
        => {
            changed |= convert_legacy_tier(from_tier);
            changed |= convert_legacy_tier(to_tier);
        }
        DispatchRoutingRow::NudgeIgnored { tier, .. } => {
            changed |= convert_legacy_tier(tier);
        }
        DispatchRoutingRow::Outcome {
            final_tier,
            label,
            ..
        } => {
            changed |= convert_legacy_tier(final_tier);
            changed |= convert_legacy_optional_tier(label);
        }
    }
    changed
}

/// Convert legacy ids throughout a decoded ledger. The caller preserves the
/// original bytes once before saving the converted records.
fn convert_legacy_team_tiers(team: &mut PersistedTeam) -> bool {
    let mut changed = false;
    for worker in &mut team.workers {
        changed |= worker.tier.as_mut().is_some_and(convert_legacy_tier);
        if let Some(routing) = &mut worker.routing {
            changed |= convert_legacy_optional_tier(&mut routing.laya_tier);
            changed |= convert_legacy_optional_tier(&mut routing.lead_tier);
            changed |= convert_legacy_optional_tier(&mut routing.ran_tier);
            changed |= convert_legacy_optional_tier(&mut routing.nudged);
        }
        for row in &mut worker.reroutes {
            changed |= convert_legacy_row_tiers(row);
        }
    }
    changed
}

struct LiveWorker {
    session: AgentSession,
    busy: Arc<AtomicBool>,
    pump: JoinHandle<()>,
    /// A failed close aborted the pump; only cleanup retries may use this handle.
    cleanup_retry_only: bool,
    /// The nudge strike counter (design §4.6), shared with the pump: three
    /// consecutive failed or verbatim-repeated tool calls nudge the lead.
    /// Beside the session handle, never persisted; a reroute resets it
    /// ([`TeamRuntime::change_model`]).
    strikes: Arc<Mutex<route_question::Strikes>>,
    /// A steer was sent and its follow-up turn has not started: set before the
    /// `SessionCommand::Steer` goes out, cleared by the follow-up's `Started`,
    /// by `Exited`, by a final `Error`, and by the follow-up deadline in
    /// [`pump_worker`]. A steered turn end is not a finished turn, so the pump
    /// holds it until the follow-up starts or the deadline settles it.
    steer_pending: Arc<AtomicBool>,
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
    /// When it was released for silence (Unix epoch milliseconds). It has
    /// not left, so the UI still names it; cleared when it attaches again or
    /// really detaches.
    idle_since_ms: Option<u64>,
    /// Notices queued for it, oldest first. The text is exactly what the
    /// in-app lead would have been sent.
    queue: VecDeque<String>,
    /// Notices an external lead never read and the in-app lead could not take
    /// (no session to receive them). They wait here, oldest first, for the
    /// in-app lead's next notice or the next external lead.
    undelivered: VecDeque<String>,
    /// Helper process ids that made a tool call while the lead was attached.
    /// A named close releases the lead only after the last id has left.
    bridges: std::collections::BTreeSet<String>,
    /// Its last call (MCP, notices or wait), in Unix epoch milliseconds.
    last_activity_ms: u64,
    /// Successful memory searches in the current plan-creation window.
    memory_search_since_plan_create: bool,
    bug_search_since_plan_create: bool,
    /// Waiters currently blocked in [`TeamRuntime::wait_external_notices`].
    waiters: usize,
    /// The Claude Code session that staked this lead, when it named itself
    /// (the Stop hook suggests `wait --session <id>`; Claude Code tells its
    /// MCP servers no session id, so a bare tool call cannot). Kept through
    /// the idle release, so that session's own hooks can re-attach it; a
    /// fresh anonymous claim or a forgotten idle lead clears it.
    session_id: Option<String>,
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

/// A worker status as the short string the routing rows log (`working`,
/// `stopped`, …) — the serde tag of [`WorkerStatus`].
fn status_name(status: WorkerStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// Whether the Laya-derived routing rows (`nudge`, `nudge_ignored`) may be
/// written for this worker: it was dispatched with routing (its record
/// carries a decision) or routing is on now. With routing off and an
/// unrouted worker nothing is appended, so off mode keeps writing exactly
/// what it wrote before routing existed. Cleanup, which has no settings
/// handle, passes [`RoutingMode::Off`] and goes by the record alone. Label
/// rows — a hand's reroute and a rerouted worker's terminal outcome — go
/// through [`logs_label`] instead.
fn logs_routing(record: &WorkerRecord, mode: RoutingMode) -> bool {
    record.routing.is_some() || mode != RoutingMode::Off
}

/// Whether a *label* row is logged for this worker: every user or lead
/// model change is a learning (decided 2026-11-04), so a reroute is appended
/// in every routing mode, and a worker that was rerouted at least once
/// keeps logging its terminal `outcome` row in every mode too — outcome
/// labels are what calibration learns from.
fn logs_label(record: &WorkerRecord, mode: RoutingMode) -> bool {
    logs_routing(record, mode) || !record.reroutes.is_empty()
}

/// The tier a worker finished on: the destination of its last reroute, else
/// its recorded tier, `custom` when it never had one.
fn final_tier(record: &WorkerRecord) -> String {
    record
        .reroutes
        .iter()
        .rev()
        .find_map(|row| match row {
            DispatchRoutingRow::LeadReroute { to_tier, .. }
            | DispatchRoutingRow::UserReroute { to_tier, .. } => Some(to_tier.clone()),
            _ => None,
        })
        .or_else(|| record.tier.clone())
        .unwrap_or_else(|| "custom".to_string())
}

/// The tier a reroute row moves the worker to — the label the record's
/// `tier` field tracks after a switch, read from the same row [`final_tier`]
/// prefers so they (and team_status) agree. `None` when the destination is a
/// custom model.
fn reroute_destination(row: &DispatchRoutingRow) -> Option<String> {
    let to_tier = match row {
        DispatchRoutingRow::LeadReroute { to_tier, .. }
        | DispatchRoutingRow::UserReroute { to_tier, .. } => to_tier,
        _ => return None,
    };
    (to_tier != "custom").then(|| to_tier.clone())
}

/// The `outcome` row for a worker that reached a terminal status (design
/// §4.7), with the label derived from its own routing history
/// ([`outcome_label`], design §4.8). An audit worker is a reviewer, never a
/// routed outcome: `None`.
fn outcome_row(record: &WorkerRecord, status: &str) -> Option<DispatchRoutingRow> {
    if record.kind == "audit" {
        return None;
    }
    let final_tier = final_tier(record);
    let high_findings = record
        .findings
        .iter()
        .filter(|finding| finding.severity == "high")
        .count();
    let label = outcome_label(
        &record.reroutes,
        status,
        &final_tier,
        record.fix_rounds,
        high_findings,
    );
    Some(DispatchRoutingRow::Outcome {
        ts: unix_ms(),
        worker_id: record.id.clone(),
        plan_id: record.plan_id.clone().unwrap_or_default(),
        step: record.plan_step.map_or(0, |step| step + 1),
        status: status.to_string(),
        final_tier,
        models_used: record.models_used.clone(),
        review_rounds: record.review_rounds,
        fix_rounds: record.fix_rounds,
        findings: record.findings.len(),
        findings_high: high_findings,
        audits: record.audits as usize,
        cost_micros: record.cost_micros.unwrap_or(0),
        label: label.map(|(label, _)| label.to_string()),
    })
}

/// Convert the settings wire shape into the only tier-reading interface.
/// An entirely empty ladder is allowed here (no models configured yet); the
/// Settings save path performs the stricter non-empty-`trivial` validation.
fn tier_ladder_from_dto(tiers: &mnemo::headless::TiersDto) -> TierLadder {
    TierLadder {
        tiers: tiers
            .tiers
            .iter()
            .map(|tier| Tier {
                id: tier.id.clone(),
                name: tier.name.clone(),
                definition: tier.definition.clone(),
                models: tier.models.clone(),
                effort: tier.effort.clone(),
                role: tier.role.clone(),
            })
            .collect(),
    }
}

/// Ladder ids in their fixed order, for schemas and diagnostics.
fn tier_ids(ladder: &TierLadder) -> Vec<&str> {
    ladder.ordered().iter().map(|tier| tier.id.as_str()).collect()
}

/// Resolve a requested tier to its nearest available model at or below it.
fn resolve_tier_over(
    models: &[LeadModel],
    ladder: &TierLadder,
    tier: &str,
) -> Result<(String, LeadModel), String> {
    if ladder.get(tier).is_none() {
        return Err(format!(
            "unknown tier '{tier}'; expected one of: {}",
            tier_ids(ladder).join(", ")
        ));
    }
    let (mut ran_tier, _) = ladder.resolve(tier).ok_or_else(|| {
        format!("tier '{tier}' has no available models; pass a `model` or another tier")
    })?;
    loop {
        if let Some(model) = ladder
            .get(&ran_tier)
            .into_iter()
            .flat_map(|rung| rung.models.iter())
            .find_map(|spec| models.iter().find(|model| model.spec == *spec))
        {
            return Ok((ran_tier.to_string(), model.clone()));
        }
        let rank = ladder
            .ordered()
            .iter()
            .position(|candidate| candidate.id == ran_tier)
            .expect("a resolved tier is on the ladder");
        if rank == 0 {
            break;
        }
        ran_tier = ladder.ordered()[rank - 1].id.clone();
    }
    Err(format!(
        "tier '{tier}' has no available models; pass a `model` or another tier"
    ))
}

/// The tier ladder the nudges climb, lowest first. `escalate` is the
/// deliberate final destination after the highest ordinary tier fails.
const NUDGE_LADDER: [&str; 5] = ["trivial", "routine", "medium", "high", "escalate"];

/// Consecutive failed or verbatim-repeated tool calls that fire a nudge
/// (design §4.6).
const NUDGE_STRIKE_LIMIT: u32 = 3;

/// How many failed tool calls a worker's streak keeps: enough for the lead to
/// see the last few errors without unbounded growth.
const TOOL_ERRORS_KEPT: usize = 3;

/// How many characters of one failed call's output the streak keeps.
const TOOL_ERROR_CHARS: usize = 300;

/// Fold one finished tool call into the worker's current failing streak: a
/// successful call clears it, a failed one appends the tool and its cleaned
/// output, keeping only the latest [`TOOL_ERRORS_KEPT`].
fn note_tool_outcome(worker: &mut WorkerRecord, tool: &str, success: bool, output: &str) {
    if success {
        worker.tool_errors.clear();
        return;
    }
    let collapsed = output.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut note: String = collapsed.chars().take(TOOL_ERROR_CHARS).collect();
    if collapsed.chars().count() > TOOL_ERROR_CHARS {
        note.push('…');
    }
    worker.tool_errors.push(ToolErrorNote {
        tool: tool.to_string(),
        output: note,
    });
    let overflow = worker.tool_errors.len().saturating_sub(TOOL_ERRORS_KEPT);
    if overflow > 0 {
        worker.tool_errors.drain(..overflow);
    }
}

/// The worker's current failing streak as notice text: empty when the last
/// tool call succeeded, otherwise the kept failures, oldest first.
pub(crate) fn tool_errors_text(worker: &WorkerRecord) -> String {
    if worker.tool_errors.is_empty() {
        return String::new();
    }
    let mut text = String::from("\nLast tool errors:");
    for note in &worker.tool_errors {
        text.push_str("\n- ");
        text.push_str(&note.tool);
        text.push_str(": ");
        text.push_str(&note.output);
    }
    text
}

/// The enabled-tier ladder over a model inventory and its tier assignment:
/// a tier is enabled when at least one of the models the tier names is
/// available. [`TeamRuntime::enabled_routing_tiers`] over shared handles,
/// so a worker pump computes the same ladder without the runtime.
fn enabled_tiers_over(
    models: &[LeadModel],
    tiers: &mnemo::headless::TiersDto,
) -> EnabledTiers {
    let ladder = tier_ladder_from_dto(tiers);
    let resolves = |tier| resolve_tier_over(models, &ladder, tier).is_ok();
    EnabledTiers {
        trivial: resolves("trivial"),
        routine: resolves("routine"),
        medium: resolves("medium"),
        high: resolves("high"),
        escalate: resolves("escalate"),
    }
}

/// Directly modelled tiers for nudges. Unlike dispatch routing, a nudge must
/// not target an empty tier that would fold back to the worker's current one,
/// nor a tier whose first available model is the model the worker already
/// runs: a suggestion that changes nothing is noise.
fn direct_tiers_over(
    models: &[LeadModel],
    tiers: &mnemo::headless::TiersDto,
    current_model: Option<&str>,
) -> EnabledTiers {
    let ladder = tier_ladder_from_dto(tiers);
    let direct = |tier| {
        let Some(rung) = ladder.get(tier) else {
            return false;
        };
        // The model a dispatch on this tier would actually run: the first
        // spec in the rung that the inventory serves.
        let first = rung
            .models
            .iter()
            .find(|spec| models.iter().any(|model| model.spec == **spec));
        match first {
            Some(spec) => current_model != Some(spec.as_str()),
            None => false,
        }
    };
    EnabledTiers {
        trivial: direct("trivial"),
        routine: direct("routine"),
        medium: direct("medium"),
        high: direct("high"),
        escalate: direct("escalate"),
    }
}

/// The lowest enabled tier strictly above `current`: the tier a nudge
/// suggests. `None` when `current` is not on the ladder or already at its
/// enabled top — a worker on the top tier is never nudged.
fn next_tier_above(enabled: EnabledTiers, current: &str) -> Option<&'static str> {
    let rank = NUDGE_LADDER.iter().position(|tier| *tier == current)?;
    NUDGE_LADDER
        .iter()
        .zip([
            enabled.trivial,
            enabled.routine,
            enabled.medium,
            enabled.high,
            enabled.escalate,
        ])
        .skip(rank + 1)
        .find(|(_, offered)| *offered)
        .map(|(tier, _)| *tier)
}

/// Whether `candidate` sits strictly above `tier` on the nudge ladder.
fn tier_is_above(tier: &str, candidate: &str) -> bool {
    match (
        NUDGE_LADDER.iter().position(|name| *name == tier),
        NUDGE_LADDER.iter().position(|name| *name == candidate),
    ) {
        (Some(rank), Some(candidate)) => candidate > rank,
        _ => false,
    }
}

/// The one nudge claimed for a worker: what the notice says and the row
/// logs (design §4.6/§4.7).
struct ClaimedNudge {
    /// The worker's display name, for the notice.
    name: String,
    /// The tier the worker is on.
    from_tier: String,
    /// The tier the nudge suggests.
    to_tier: String,
    /// The plan the step belongs to, when known.
    plan_id: String,
    /// The 1-based step number in the plan.
    step: usize,
    /// Whether the routing rows' `logs_routing` gate passes for this
    /// worker: the notice may fire without it, the row may not.
    logs: bool,
}

/// Claim the worker's one nudge (design §4.6) under the record lock: mark
/// `routing.nudged` with the tier the nudge suggests, so at most one ever
/// fires for a worker, and report what the notice and row need. `None`
/// when the worker is gone, carries no routing record (there is nowhere to
/// mark the one nudge), was already nudged, or has no enabled tier above
/// its own. `suggest` names the tier Laya cleared (the drift nudge), which
/// must still sit above the worker's own; `None` takes the next enabled
/// tier up. The caller must not hold the records lock.
async fn claim_nudge(
    records: &Arc<Mutex<Vec<WorkerRecord>>>,
    worker_id: &str,
    enabled: EnabledTiers,
    mode: RoutingMode,
    suggest: Option<&str>,
) -> Option<ClaimedNudge> {
    let mut records = records.lock().await;
    let worker = records.iter_mut().find(|worker| worker.id == worker_id)?;
    let from_tier = final_tier(worker);
    let logs = logs_routing(worker, mode);
    let routing = worker.routing.as_mut()?;
    if routing.nudged.is_some() {
        return None;
    }
    let to_tier = match suggest {
        Some(tier) if tier_is_above(&from_tier, tier) => tier.to_string(),
        Some(_) => return None,
        None => next_tier_above(enabled, &from_tier)?.to_string(),
    };
    routing.nudged = Some(to_tier.clone());
    Some(ClaimedNudge {
        name: worker.name.clone(),
        from_tier,
        to_tier,
        plan_id: worker.plan_id.clone().unwrap_or_default(),
        step: worker.plan_step.map_or(0, |step| step + 1),
        logs,
    })
}

/// Deliver a claimed nudge: queue the lead notice on the board-signal bus
/// (the runtime delivers it prefixed like every TeamManager notice) and
/// append the `Nudge` row (design §4.7) behind the same `logs_routing`
/// gate as the reroute rows. Never switches the worker's model. `tool_errors`
/// is the strike nudge's failing-streak text (empty for every other nudge).
fn deliver_nudge(
    signals: &broadcast::Sender<BoardSignal>,
    routing_log_path: &Path,
    worker_id: &str,
    claim: &ClaimedNudge,
    reason: &str,
    detail: &str,
    tool_errors: &str,
) {
    let tool_errors = if tool_errors.is_empty() {
        String::new()
    } else {
        format!("{tool_errors}\nRead these first: a tool problem is not fixed by a stronger model.")
    };
    let _ = signals.send(BoardSignal::Notice {
        key: format!("nudge:{worker_id}"),
        text: format!(
            "worker_id={worker_id} Laya nudge: {} {detail}; consider team_resume with tier {}.{tool_errors}",
            claim.name, claim.to_tier
        ),
    });
    if claim.logs {
        mnemo::model_routing::append_row(
            routing_log_path,
            &DispatchRoutingRow::Nudge {
                ts: unix_ms(),
                worker_id: worker_id.to_string(),
                plan_id: claim.plan_id.clone(),
                step: claim.step,
                reason: reason.to_string(),
                from_tier: claim.from_tier.clone(),
                to_tier: claim.to_tier.clone(),
                detail: detail.to_string(),
            },
        );
    }
}

/// The `nudge_ignored` row (design §4.7): a nudged worker reached a
/// truly terminal status — merged, stopped, or abandoned in cleanup —
/// still on the tier it was nudged from, so the lead never acted on the
/// nudge. A failed worker is resumable (`team_resume` on a stronger tier
/// takes the nudge and logs a reroute), so failure never logs it.
/// `None` when the worker was not nudged, or ended on the suggested
/// tier (every reroute clears the nudge, so an acted-on nudge is never
/// seen here).
fn nudge_ignored_row(record: &WorkerRecord) -> Option<DispatchRoutingRow> {
    let nudged = record.routing.as_ref()?.nudged.as_deref()?;
    let tier = final_tier(record);
    if tier == nudged {
        return None;
    }
    Some(DispatchRoutingRow::NudgeIgnored {
        ts: unix_ms(),
        worker_id: record.id.clone(),
        plan_id: record.plan_id.clone().unwrap_or_default(),
        step: record.plan_step.map_or(0, |step| step + 1),
        tier,
    })
}

/// The shared handles a pump-side strike nudge needs (design §4.6): the
/// records to claim on, the tier-ladder inputs, the notice bus, the routing
/// log, and the settings in force.
struct StrikeNudges<'a> {
    records: &'a Arc<Mutex<Vec<WorkerRecord>>>,
    models: &'a Arc<std::sync::RwLock<Vec<LeadModel>>>,
    tiers: &'a Arc<std::sync::RwLock<mnemo::headless::TiersDto>>,
    signals: &'a broadcast::Sender<BoardSignal>,
    routing_log_path: &'a Path,
    settings: &'a RoutingSettings,
}

/// Try the worker's one strike nudge (design §4.6): the pump saw its
/// third consecutive failed or verbatim-repeated tool call. Claims the
/// nudge slot, queues the notice, and appends the row; `repeated` says
/// whether the firing call repeated the previous one verbatim ("going in
/// circles") or failed anew. No-op when nudges are off, the worker was
/// already nudged, has no routing record, or sits on the top enabled
/// tier.
async fn try_strike_nudge(worker_id: &str, nudges: StrikeNudges<'_>, repeated: bool) {
    // The worker's model and its current failing streak come from one lock
    // read; the records lock must be released before `claim_nudge` takes it.
    let (current_model, tool_errors) = {
        let records = nudges.records.lock().await;
        let Some(worker) = records.iter().find(|worker| worker.id == worker_id) else {
            return;
        };
        (worker.model.clone(), tool_errors_text(worker))
    };
    let enabled = direct_tiers_over(
        &nudges.models.read().unwrap_or_else(|e| e.into_inner()),
        &nudges.tiers.read().unwrap_or_else(|e| e.into_inner()),
        current_model.as_deref(),
    );
    let Some(claim) =
        claim_nudge(nudges.records, worker_id, enabled, nudges.settings.mode, None).await
    else {
        return;
    };
    let (reason, detail) = if repeated {
        ("circles", "looks stuck (going in circles)")
    } else {
        ("strikes", "looks stuck (3 failing tool calls in a row)")
    };
    deliver_nudge(
        nudges.signals,
        nudges.routing_log_path,
        worker_id,
        &claim,
        reason,
        detail,
        &tool_errors,
    );
}

fn append_reroute_metadata(
    path: &Path,
    row: &DispatchRoutingRow,
    source: ChangeSource,
    finding_closed: Option<bool>,
) {
    let mut value = match serde_json::to_value(row) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("failed to serialize dispatch routing row: {error}");
            return;
        }
    };
    if let Some(object) = value.as_object_mut() {
        if !matches!(source, ChangeSource::Lead | ChangeSource::User) {
            object.insert("source".into(), source.as_str().into());
        }
        if let Some(closed) = finding_closed {
            object.insert("finding_closed".into(), closed.into());
        }
    }
    mnemo::model_routing::append_row(path, &value);
}

/// The unreviewed model downshift on this routed fix, if any. `nudged` is the
/// durable pending marker on this internal row; settling the review clears it.
fn pending_fix_downshift(
    worker: &WorkerRecord,
    findings: &[Finding],
) -> Option<(DispatchRoutingRow, bool)> {
    let finding_id = worker.fixer_finding_id.as_deref()?;
    let DispatchRoutingRow::LeadReroute {
        from_tier,
        to_tier,
        nudged: true,
        ..
    } = worker.reroutes.last()?
    else {
        return None;
    };
    let from = NUDGE_LADDER.iter().position(|tier| tier == from_tier)?;
    let to = NUDGE_LADDER.iter().position(|tier| tier == to_tier)?;
    if from <= to {
        return None;
    }
    let closed = !findings
        .iter()
        .any(|finding| finding.id == finding_id && finding.status == "open");
    Some((worker.reroutes.last()?.clone(), closed))
}

fn route_fix_brief(worker: &WorkerRecord, routed: &Finding) -> String {
    // Keep the established routed-finding prefix for callers that display or
    // match it, then include every OTHER open finding in full as well.
    let mut brief = format!(
        "Fix review finding {}: {}\n{}\nFile: {}:{}\n",
        routed.id, routed.title, routed.detail, routed.file, routed.line
    );
    for open in worker
        .findings
        .iter()
        .filter(|candidate| candidate.status == "open" && candidate.id != routed.id)
    {
        brief.push_str(&format!(
            "\nAlso fix open finding {} [{}]: {}\n{}\nFile: {}:{}\n",
            open.id, open.severity, open.title, open.detail, open.file, open.line
        ));
    }
    brief.push_str(
        "\nCommit fixes for every finding above, rerun the affected checks, and report evidence for delta review.",
    );
    brief
}

/// The shared state an automatic idle escalation touches. The worker pump and
/// the lead review path both have these handles already.
struct AutoEscalation<'a> {
    records: &'a Arc<Mutex<Vec<WorkerRecord>>>,
    live: &'a Arc<Mutex<HashMap<String, LiveWorker>>>,
    models: &'a Arc<std::sync::RwLock<Vec<LeadModel>>>,
    tiers: &'a Arc<std::sync::RwLock<mnemo::headless::TiersDto>>,
    signals: &'a broadcast::Sender<BoardSignal>,
    routing_log_path: &'a Path,
    usage: &'a Arc<Mutex<ProjectUsage>>,
    save_lock: &'a Arc<Mutex<()>>,
    persistence: &'a TeamPersistence,
}

/// Switch a nudged idle worker without stopping its session. `Ok(false)` means
/// the original suggestion remains in force (busy, already claimed, or no live
/// session); it is not an error and a later idle event may try again.
async fn auto_escalate_idle(worker_id: &str, ctx: AutoEscalation<'_>) -> Result<bool, String> {
    let record = ctx
        .records
        .lock()
        .await
        .iter()
        .find(|worker| worker.id == worker_id)
        .cloned()
        .ok_or_else(|| format!("worker '{worker_id}' not found"))?;
    if record.kind == "audit" {
        return Ok(false);
    }
    let Some(to_tier) = record
        .routing
        .as_ref()
        .and_then(|routing| routing.nudged.clone())
    else {
        return Ok(false);
    };
    let (ran_tier, choice, effort) = {
        let models = ctx.models.read().unwrap_or_else(|e| e.into_inner());
        let tiers = ctx.tiers.read().unwrap_or_else(|e| e.into_inner());
        let ladder = tier_ladder_from_dto(&tiers);
        let (ran_tier, choice) = resolve_tier_over(&models, &ladder, &to_tier)?;
        let effort = ladder.get(&ran_tier).and_then(|rung| rung.effort.clone());
        (ran_tier, choice, effort)
    };
    let model = choice.spec.clone();
    let new_model_id = {
        let live = ctx.live.lock().await;
        let Some(live) = live.get(worker_id) else {
            return Ok(false);
        };
        if live.busy.load(Ordering::SeqCst) {
            return Ok(false);
        }
        live.admit_session_command(worker_id)?;
        match live.session.set_model(&model, effort.clone()).await {
            Ok(ModelSwitch::NextRequest { model })
            | Ok(ModelSwitch::CompactsFirst { model }) => model,
            Err(error) => {
                return Err(format!(
                    "could not switch worker '{worker_id}' to '{model}': {error}"
                ));
            }
        }
    };
    let from_model = record.model.clone();
    let from_tier = final_tier(&record);
    let row = DispatchRoutingRow::LeadReroute {
        ts: unix_ms(),
        worker_id: worker_id.to_string(),
        plan_id: record.plan_id.clone().unwrap_or_default(),
        step: record.plan_step.map_or(0, |step| step + 1),
        from_tier: from_tier.clone(),
        to_tier: ran_tier.clone(),
        from_model,
        to_model: Some(model.clone()),
        status: status_name(record.status),
        review_rounds: record.review_rounds,
        fix_rounds: record.fix_rounds,
        nudged: true,
    };
    let mut records = ctx.records.lock().await;
    let worker = records
        .iter_mut()
        .find(|worker| worker.id == worker_id)
        .ok_or_else(|| format!("worker '{worker_id}' not found"))?;
    if worker.routing.as_ref().and_then(|routing| routing.nudged.as_deref())
        != Some(to_tier.as_str())
    {
        return Ok(false);
    }
    worker.model = Some(model.clone());
    if worker.model_id.as_deref() != Some(new_model_id.as_str()) {
        worker.model_id = None;
    }
    worker.reasoning_effort = effort;
    worker.tier = Some(ran_tier.clone());
    worker.reroutes.push(row.clone());
    if let Some(routing) = worker.routing.as_mut() {
        routing.nudged = None;
    }
    note_model(worker, &model);
    drop(records);
    if let Some(live) = ctx.live.lock().await.get(worker_id) {
        live.strikes.lock().await.reset();
    }
    save_workers(ctx.records, ctx.usage, ctx.save_lock, ctx.persistence).await?;
    append_reroute_metadata(ctx.routing_log_path, &row, ChangeSource::Escalation, None);
    let _ = ctx.signals.send(BoardSignal::Notice {
        key: format!("escalation:auto:{worker_id}"),
        text: format!(
            "Auto escalation switched worker {worker_id} from {from_tier} to {ran_tier} ({model}); its session and transcript were kept."
        ),
    });
    Ok(true)
}

/// The shared team runtime: one engine, many real worker sessions.
pub struct TeamRuntime {
    engine: Arc<Engine>,
    workspace_root: PathBuf,
    records: Arc<Mutex<Vec<WorkerRecord>>>,
    live: Arc<Mutex<HashMap<String, LiveWorker>>>,
    messages: Arc<Mutex<Vec<TeamMessage>>>,
    events: broadcast::Sender<TeamEvent>,
    generation: Arc<AtomicU64>,
    next_worker: AtomicU64,
    lead_busy: AtomicBool,
    /// Transition tests flip this to inject a worker-session close
    /// failure; production never sets it. Shared with every worker pump
    /// so the audit release closes sessions exactly like `stop_inner`.
    fail_next_close: Arc<AtomicBool>,
    lead_session: Mutex<Option<SessionId>>,
    merge_lock: Mutex<()>,
    pricing: Arc<std::sync::RwLock<Vec<mnemo::config::PricingEntry>>>,
    config_dir: PathBuf,
    /// The dispatch-routing settings, shared with every worker pump so a
    /// failure's `outcome` row follows the mode in force when it happens.
    routing: Arc<std::sync::RwLock<RoutingSettings>>,
    classifier: Option<ClassifierSlot>,
    /// The online-learning overlay over `dispatch_routing.jsonl`, present when
    /// the engine exposes its shared embedder. Consulted before Laya.
    route_knn: Option<Arc<RouteKnn>>,
    /// One classifier call at a time, shared with the drift checks.
    routing_permit: Arc<tokio::sync::Semaphore>,
    routing_log_path: PathBuf,
    /// The model catalog and its tier assignment, shared with the worker
    /// pumps so a nudge suggests a tier from the catalog in force when the
    /// worker stuck (design §4.6).
    models: Arc<std::sync::RwLock<Vec<LeadModel>>>,
    tiers: Arc<std::sync::RwLock<mnemo::headless::TiersDto>>,
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

/// Upper bound for concurrently live worker sessions in one project.
pub const MAX_WORKER_SLOTS: usize = 32;

impl TeamRuntime {
    async fn close_session_for_cleanup(&self, session: &AgentSession) -> Result<(), String> {
        close_worker_session(session, &self.fail_next_close).await
    }

    #[cfg(test)]
    fn fail_next_close_for_test(&self) {
        self.fail_next_close
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Open the team ledger, recover interrupted workers, and prepare to spawn.
    pub async fn open(options: TeamRuntimeOptions) -> Result<Self, String> {
        let config_dir = options.config_dir.clone();
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
        let routing_log_path = options.routing_log_path.clone().unwrap_or_else(
            crate::mnemo2::dispatch_routing::dispatch_routing_log_path,
        );
        // The kNN overlay shares the app's live embedder (never a second
        // backend). With no own memory store there is nothing to embed with,
        // so the overlay is absent and Laya decides every dispatch.
        let route_knn = options.engine.embedder_handle().map(|initial| {
            let engine = Arc::clone(&options.engine);
            let getter: EmbedderGetter =
                Arc::new(move || engine.embedder_handle().unwrap_or_else(|| initial.clone()));
            Arc::new(RouteKnn::new(getter, routing_log_path.clone()))
        });
        let runtime = Self {
            engine: options.engine,
            workspace_root: options.workspace_root,
            records: Arc::new(Mutex::new(workers)),
            live: Arc::new(Mutex::new(HashMap::new())),
            messages: Arc::new(Mutex::new(Vec::new())),
            events,
            generation: Arc::new(AtomicU64::new(0)),
            next_worker: AtomicU64::new(1),
            lead_busy: AtomicBool::new(false),
            fail_next_close: Arc::new(AtomicBool::new(false)),
            lead_session: Mutex::new(None),
            merge_lock: Mutex::new(()),
            pricing: Arc::new(std::sync::RwLock::new(options.pricing)),
            config_dir: config_dir.clone(),
            routing: Arc::new(std::sync::RwLock::new(load_routing_settings(&config_dir))),
            classifier: options.classifier,
            route_knn,
            routing_permit: Arc::new(tokio::sync::Semaphore::new(1)),
            routing_log_path,
            models: Arc::new(std::sync::RwLock::new(options.models)),
            tiers: Arc::new(std::sync::RwLock::new(mnemo::headless::TiersDto::default())),
            persistence,
            usage: Arc::new(Mutex::new(usage)),
            save_lock: Arc::new(Mutex::new(())),
            board: std::sync::Mutex::new(None),
            signals,
            external: std::sync::Mutex::new(ExternalLead::default()),
            external_wake: tokio::sync::Notify::new(),
        };
        runtime.persist().await?;
        // App start: sweep checkouts a previous run renamed into the
        // trash folder but could not remove yet (something held them).
        schedule_trash_sweep(&runtime.workspace_root);
        Ok(runtime)
    }

    /// The live dispatch-routing kNN overlay, when the engine exposes its
    /// shared embedder. The eval reads it to report Laya-only and kNN+Laya
    /// accuracy side by side.
    pub fn route_knn(&self) -> Option<Arc<RouteKnn>> {
        self.route_knn.clone()
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
        self.reload_routing_settings();
        *self.pricing.write().expect("pricing lock poisoned") = pricing;
    }

    /// Re-read the `[routing]` table of `mnemo2.toml` into the live team, so a
    /// routing-settings save (the Laya page's Save, the eval's Apply) takes
    /// effect on the next dispatch without a restart. [`Self::set_catalog`] is
    /// the other reload path — a model-settings save.
    pub fn reload_routing_settings(&self) {
        *self.routing.write().unwrap_or_else(|e| e.into_inner()) =
            load_routing_settings(&self.config_dir);
    }

    fn current_models(&self) -> Vec<LeadModel> {
        self.models
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The live ladder new dispatches and tool descriptions resolve against.
    fn tier_ladder(&self) -> TierLadder {
        tier_ladder_from_dto(&self.tiers.read().unwrap_or_else(|e| e.into_inner()))
    }

    /// The tier's configured models that are still available, in order.
    pub(crate) fn tier_models(&self, tier: &str) -> Vec<LeadModel> {
        let ladder = self.tier_ladder();
        let models = self.current_models();
        ladder
            .get(tier)
            .into_iter()
            .flat_map(|rung| rung.models.iter())
            .filter_map(|spec| models.iter().find(|model| model.spec == *spec).cloned())
            .collect()
    }

    /// Resolve one lead tier through the ladder's fold-down rule.
    fn resolve_tier_choice(&self, tier: &str) -> Result<(String, LeadModel), String> {
        resolve_tier_over(&self.current_models(), &self.tier_ladder(), tier)
    }

    /// Every available model some tier names, in tier order, once each.
    fn tiered_models(&self) -> Vec<LeadModel> {
        let mut models: Vec<LeadModel> = Vec::new();
        let ladder = self.tier_ladder();
        for tier in tier_ids(&ladder) {
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
        if let Some(tier) = tier {
            if self.tier_ladder().get(tier).is_none() {
                return Err(format!(
                    "unknown tier '{tier}'; expected one of: {}",
                    tier_ids(&self.tier_ladder()).join(", ")
                ));
            }
        }
        let model = match (model, tier) {
            (None, Some(tier)) => Some(self.resolve_tier_choice(tier)?.1.spec),
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

    /// Resolve a lead's model/tier choice and retain the tier that actually
    /// runs (which may be below the requested tier).
    fn lead_choice_with_tier(
        &self,
        tier: Option<&str>,
        model: Option<String>,
    ) -> Result<(Option<String>, Option<String>), String> {
        if model.is_none() {
            if let Some(tier) = tier {
                let (ran_tier, choice) = self.resolve_tier_choice(tier)?;
                return Ok((Some(choice.spec), Some(ran_tier)));
            }
        }
        let model = self.lead_model_choice(tier, model)?;
        let ran_tier = self.tier_for_model(model.as_deref());
        Ok((model, ran_tier))
    }

    async fn route_implementation_dispatch(
        &self,
        request: &DispatchRequest,
        step_text: &str,
        brief: &str,
    ) -> Result<RoutedDispatch, String> {
        let settings = self
            .routing
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let requested_tier = request.tier.as_deref();
        if requested_tier == Some("auto") && matches!(settings.mode, RoutingMode::Off | RoutingMode::Shadow) {
            return Err("tier auto needs routing mode suggest or auto (Settings > Tiers)".into());
        }
        // The escalation rung is never a first pick: fresh work dispatches on
        // high, and team_resume or team_route_fix moves an existing worker up.
        if request.source == DispatchSource::LeadTool {
            let ladder = self.tier_ladder();
            if let (Some(tier), Some(rung)) = (requested_tier, ladder.escalation()) {
                if tier == rung.id {
                    return Err(
                        "tier escalate is escalation only, never a first pick; dispatch on high, or move an existing worker up with team_resume or team_route_fix and tier escalate"
                            .into(),
                    );
                }
            }
        }

        if requested_tier.is_none()
            && request.model.is_none()
            && settings.mode != RoutingMode::Auto
            && request.source == DispatchSource::LeadTool
        {
            if let Some(required) = self.tier_required() {
                return Err(required);
            }
        }
        if settings.mode == RoutingMode::Off {
            let (model, tier) =
                self.lead_choice_with_tier(requested_tier, request.model.clone())?;
            let reasoning_effort = request.reasoning_effort.clone().or_else(|| {
                tier.as_deref()
                    .and_then(|tier| self.tier_ladder().get(tier).and_then(|rung| rung.effort.clone()))
            });
            return Ok(RoutedDispatch {
                model,
                tier,
                reasoning_effort,
                routing: None,
                rows: Vec::new(),
                error: None,
            });
        }

        let explicit_tier = requested_tier.filter(|tier| *tier != "auto");
        let has_explicit_choice = explicit_tier.is_some() || request.model.is_some();
        let enabled = self.enabled_routing_tiers();
        let classified_text = routing_text(step_text, brief).filter(|text| {
            text.split_whitespace().count() >= settings.min_words
        });
        let classifier = self.classifier.as_ref().and_then(|slot| {
            slot.read()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        });
        let (decision, skip_reason): (Option<DispatchRoutingDecision>, Option<&'static str>) =
            if classified_text.is_none() {
                (None, Some("too_short"))
            } else if enabled.offered().count() < 2 {
                (None, Some("one_tier"))
            } else if let Some(classifier) = classifier {
                let attempt = async {
                    let _permit = self
                        .routing_permit
                        .acquire()
                        .await
                        .map_err(|_| "Laya routing is closed".to_string())?;
                    Ok::<Option<DispatchRoutingDecision>, String>(
                        classify_dispatch(
                            classifier.as_ref(),
                            self.route_knn.as_deref(),
                            classified_text.as_deref().unwrap_or(""),
                            enabled,
                            &settings,
                        )
                        .await,
                    )
                };
                match tokio::time::timeout(std::time::Duration::from_secs(3), attempt).await {
                    Ok(Ok(Some(decision))) => (Some(decision), None),
                    Ok(Ok(None)) | Err(_) => (None, Some("offline")),
                    Ok(Err(error)) => return Err(error),
                }
            } else {
                (None, Some("offline"))
            };
        let cleared_tier = decision
            .as_ref()
            .filter(|decision| decision.cleared)
            .and_then(|decision| decision.tier);

        let mut refusal = None;
        let (chosen_model, chosen_tier) = if has_explicit_choice {
            self.lead_choice_with_tier(explicit_tier, request.model.clone())?
        } else if let Some(tier) = cleared_tier.filter(|_| {
            settings.mode == RoutingMode::Auto
                || (settings.mode == RoutingMode::Suggest && requested_tier == Some("auto"))
        }) {
            let (ran_tier, choice) = self.resolve_tier_choice(tier)?;
            (Some(choice.spec), Some(ran_tier))
        } else if settings.mode == RoutingMode::Suggest && requested_tier == Some("auto") {
            refusal = Some(if skip_reason == Some("offline") {
                "Laya is not running; pass a tier".to_string()
            } else {
                format!(
                    "Laya is unsure ({:.2}); pass a tier",
                    decision.as_ref().map_or(0.0, |decision| decision.confidence)
                )
            });
            (None, None)
        } else if settings.mode == RoutingMode::Auto {
            let tier = settings.fallback_tier.clone();
            if self.tiered_models().is_empty() {
                (self.lead_model_choice(None, None)?, None)
            } else {
                let (ran_tier, choice) = self.resolve_tier_choice(&tier)?;
                (Some(choice.spec), Some(ran_tier))
            }
        } else {
            (self.lead_model_choice(None, None)?, None)
        };
        let auto_fallback_tier = (!has_explicit_choice
            && settings.mode == RoutingMode::Auto
            && cleared_tier.is_none()
            && chosen_tier.is_some())
        .then(|| settings.fallback_tier.clone());
        let routed_by = if has_explicit_choice {
            let lead_tier = if request.model.is_some() {
                chosen_tier.as_deref()
            } else {
                explicit_tier
            };
            if cleared_tier.is_some() && cleared_tier != lead_tier {
                "lead_override"
            } else {
                "lead"
            }
        } else if cleared_tier.is_some()
            && refusal.is_none()
            && (settings.mode == RoutingMode::Auto
                || (settings.mode == RoutingMode::Suggest && requested_tier == Some("auto")))
        {
            "laya"
        } else {
            "lead"
        };
        let reasoning_effort = request.reasoning_effort.clone().or_else(|| {
            chosen_tier.as_deref().and_then(|tier| {
                self.tier_ladder()
                    .get(tier)
                    .and_then(|rung| rung.effort.clone())
            })
        });
        let routing = WorkerRouting {
            laya_tier: decision
                .as_ref()
                .and_then(|decision| decision.tier)
                .map(str::to_string),
            confidence: decision.as_ref().map(|decision| decision.confidence),
            cleared: decision.as_ref().is_some_and(|decision| decision.cleared),
            lead_tier: (routed_by != "laya")
                .then(|| {
                    explicit_tier
                        .map(str::to_string)
                        .or_else(|| auto_fallback_tier.clone())
                        .or_else(|| chosen_tier.clone())
                })
                .flatten(),
            routed_by: routed_by.to_string(),
            ran_tier: refusal.is_none().then(|| chosen_tier.clone()).flatten(),
            nudged: None,
        };
        let plan_id = request.plan.as_ref().map(|plan| plan.plan_id.clone());
        let step = request.plan.as_ref().map(|plan| plan.step + 1);
        let skip = skip_reason;
        use sha2::Digest as _;
        let text_sha256 = sha2::Sha256::digest(
            classified_text.as_deref().unwrap_or("").as_bytes(),
        )
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
        let ts = unix_ms();
        let row = DispatchRoutingRow::Route {
            ts,
            worker_id: request.worker_id.clone().unwrap_or_default(),
            plan_id: plan_id.unwrap_or_default(),
            step: step.unwrap_or_default(),
            mode: settings.mode,
            text_sha256,
            label: decision
                .as_ref()
                .map(|decision| decision.label.clone())
                .unwrap_or_default(),
            confidence: decision.as_ref().map_or(0.0, |decision| decision.confidence),
            probabilities: decision
                .as_ref()
                .map(|decision| decision.probabilities.clone())
                .unwrap_or_default(),
            tier: routing.laya_tier.clone(),
            threshold: decision.as_ref().map_or_else(
                || threshold_for(enabled.offered().count(), &settings),
                |decision| {
                    if decision.source == "knn" {
                        settings.knn_min_share
                    } else {
                        threshold_for(enabled.offered().count(), &settings)
                    }
                },
            ),
            cleared: routing.cleared,
            lead_tier: routing.lead_tier.clone(),
            ran_tier: refusal.is_none().then(|| chosen_tier.clone()).flatten(),
            ran_model: refusal.is_none().then(|| chosen_model.clone()).flatten(),
            text: classified_text.unwrap_or_default(),
            skip: skip.map(str::to_string),
            source: decision.as_ref().map(|decision| decision.source.to_string()),
            knn_label: decision.as_ref().and_then(|decision| decision.knn_label.clone()),
            knn_share: decision.as_ref().and_then(|decision| decision.knn_share),
            knn_k: decision.as_ref().and_then(|decision| decision.knn_k),
        };
        let mut rows = vec![row];
        if routed_by == "lead_override" {
            if let Some(laya_tier) = routing.laya_tier.clone() {
                rows.push(DispatchRoutingRow::LeadOverride {
                    ts,
                    worker_id: request.worker_id.clone().unwrap_or_default(),
                    plan_id: request
                        .plan
                        .as_ref()
                        .map(|plan| plan.plan_id.clone())
                        .unwrap_or_default(),
                    step: request.plan.as_ref().map_or(0, |plan| plan.step + 1),
                    laya_tier: Some(laya_tier),
                    lead_tier: chosen_tier.clone().unwrap_or_else(|| "custom".into()),
                    requested_tier: explicit_tier
                        .map(str::to_string)
                        .filter(|requested| Some(requested.as_str()) != chosen_tier.as_deref()),
                    confidence: decision.as_ref().map_or(0.0, |decision| decision.confidence),
                });
            }
        }
        Ok(RoutedDispatch {
            model: chosen_model,
            tier: chosen_tier,
            reasoning_effort,
            routing: Some(routing),
            rows,
            error: refusal,
        })
    }

    /// The tier a model spec belongs to: the first ladder rung whose list
    /// names an available model.
    fn tier_of(&self, spec: &str) -> Option<String> {
        let ladder = self.tier_ladder();
        let models = self.current_models();
        tier_ids(&ladder).into_iter().find(|tier| {
            ladder
                .get(tier)
                .into_iter()
                .flat_map(|rung| rung.models.iter())
                .any(|candidate| {
                    candidate == spec && models.iter().any(|model| model.spec == *candidate)
                })
        }).map(str::to_string)
    }

    fn tier_for_model(&self, model: Option<&str>) -> Option<String> {
        self.tier_of(model?)
    }

    fn enabled_routing_tiers(&self) -> EnabledTiers {
        enabled_tiers_over(
            &self.current_models(),
            &self.tiers
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        )
    }

    /// Nudge destinations have their own models, so a nudge cannot fold back
    /// onto the tier the worker is already running, nor onto a tier whose
    /// first available model is the model the worker already runs.
    fn direct_nudge_tiers(&self, current_model: Option<&str>) -> EnabledTiers {
        direct_tiers_over(
            &self.current_models(),
            &self.tiers
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            current_model,
        )
    }

    fn append_dispatch_routing_row(&self, row: &DispatchRoutingRow) {
        mnemo::model_routing::append_row(&self.routing_log_path, row);
    }

    /// Append a reroute with the step-6 metadata kept outside the legacy typed
    /// row, so old readers and parallel settings code retain their contract.
    fn append_reroute_row(
        &self,
        row: &DispatchRoutingRow,
        source: ChangeSource,
        finding_closed: Option<bool>,
    ) {
        append_reroute_metadata(&self.routing_log_path, row, source, finding_closed);
    }

    async fn auto_escalate_worker(&self, worker_id: &str) -> Result<bool, String> {
        auto_escalate_idle(
            worker_id,
            AutoEscalation {
                records: &self.records,
                live: &self.live,
                models: &self.models,
                tiers: &self.tiers,
                signals: &self.signals,
                routing_log_path: &self.routing_log_path,
                usage: &self.usage,
                save_lock: &self.save_lock,
                persistence: &self.persistence,
            },
        )
        .await
    }

    /// The routing mode in force right now.
    fn routing_mode(&self) -> RoutingMode {
        self.routing.read().unwrap_or_else(|e| e.into_inner()).mode
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

    /// Generate the lead's tier guidance from the ladder itself: editable
    /// names and definitions, available models with prices, effort and the
    /// fold-down destination of an empty tier.
    fn tier_guidance(&self) -> Option<String> {
        if self.tiered_models().is_empty() {
            return None;
        }
        let ladder = self.tier_ladder();
        let mut lines = Vec::new();
        for rung in ladder.ordered() {
            let tier = rung.id.as_str();
            let models = self.tier_models(tier);
            let list = if models.is_empty() {
                match self.resolve_tier_choice(tier) {
                    Ok((ran_tier, _)) if ran_tier != tier => {
                        format!("no model; folds down to {ran_tier}")
                    }
                    _ => "no model".to_string(),
                }
            } else {
                models
                    .iter()
                    .map(|model| match self.price_label(model) {
                        Some(price) => format!("{} ({price})", model.spec),
                        None => model.spec.clone(),
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let effort = rung
                .effort
                .as_deref()
                .map(|effort| format!("; effort {effort}"))
                .unwrap_or_default();
            let escalation = if rung.role.as_deref() == Some("escalate") {
                "; escalation only, never a first pick"
            } else {
                ""
            };
            lines.push(format!(
                "{tier} ({}) - {}: {list}{effort}{escalation}",
                rung.name, rung.definition
            ));
        }
        if matches!(
            self.routing.read().unwrap_or_else(|e| e.into_inner()).mode,
            RoutingMode::Suggest | RoutingMode::Auto
        ) {
            lines.push("auto - let Laya choose from the step spec (never escalate)".into());
        }
        Some(lines.join("\n"))
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

    /// Install a stub classifier for runtime integration tests.
    #[cfg(test)]
    pub(crate) fn set_classifier_for_test(&mut self, classifier: ClassifierSlot) {
        self.classifier = Some(classifier);
    }

    /// Inspect the classifier handle installed at open in integration tests.
    #[cfg(test)]
    pub(crate) fn classifier_slot_for_test(&self) -> Option<ClassifierSlot> {
        self.classifier.clone()
    }

    /// Inspect the routing settings loaded at open in integration tests.
    #[cfg(test)]
    pub(crate) fn routing_settings_for_test(&self) -> RoutingSettings {
        self.routing
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Share the lead's one plan board with the team and register the observer
    /// that tells working workers when the lead records something new.
    pub(crate) fn attach_board(self: &Arc<Self>, board: Arc<PlanBoard>) {
        board.set_external_lead_attached(self.external_lead_attached());
        let team = Arc::downgrade(self);
        let runtime = tokio::runtime::Handle::try_current().ok();
        board.set_edit_observer(Box::new(move |plan_id, kinds| {
            let Some(team) = team.upgrade() else {
                return;
            };
            let plan_id = plan_id.to_string();
            let text = format!(
                "Plan {plan_id} changed: new {}. Call plan_read to re-read it before you continue.",
                kinds.join(", ")
            );
            // The board calls this synchronously after releasing its own
            // guard, so the observer only hands the work to the runtime.
            let Some(runtime) = runtime.clone() else {
                eprintln!(
                    "team: no runtime is available to tell workers that plan {plan_id} changed"
                );
                return;
            };
            runtime.spawn(async move {
                let workers = {
                    let records = team.records.lock().await;
                    records
                        .iter()
                        .filter(|worker| worker.plan_id.as_deref() == Some(plan_id.as_str()))
                        .filter(|worker| {
                            matches!(
                                worker.status,
                                WorkerStatus::Working
                                    | WorkerStatus::Dispatching
                                    | WorkerStatus::Fixing
                            )
                        })
                        .map(|worker| worker.id.clone())
                        .collect::<Vec<_>>()
                };
                for worker_id in workers {
                    // Mnemo2 writes this notice and cannot tell who edited the plan, so it signs it itself.
                    if let Err(error) = team
                        .message(&worker_id, text.clone(), MessageSender::Mnemo)
                        .await
                    {
                        eprintln!(
                            "team: could not tell worker '{worker_id}' that plan {plan_id} changed: {error}"
                        );
                    }
                }
            });
        }));
        *self.board.lock().unwrap_or_else(|e| e.into_inner()) = Some(board);
    }

    fn board(&self) -> Option<Arc<PlanBoard>> {
        self.board.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The markdown of the board plan `plan_id`: what the high-risk gate
    /// scans for acceptance scenarios, what an audit brief carries, and what
    /// the Plans tab expands into (through `board_plan_document`).
    pub(crate) fn plan_document(&self, plan_id: &str) -> Result<String, String> {
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
    /// attached before. Counts as activity. The claim stays anonymous: a bare
    /// MCP tool call cannot know Claude Code's session id.
    pub fn attach_external_lead(&self) -> bool {
        self.attach_external_lead_as(None)
    }

    /// [`TeamRuntime::attach_external_lead`] naming the Claude Code session
    /// that staked the claim, when the caller knows it: the waiter runs with
    /// `--session <id>` (the Stop hook names it — only a hook knows a session
    /// id). A named claim also names an already-attached lead, so that
    /// session's own hooks can re-attach it after an idle release
    /// ([`TeamRuntime::reattach_idle_external_lead`]); a fresh anonymous
    /// claim clears the name instead, so no session can reach through a hook
    /// for a lead it never staked.
    pub fn attach_external_lead_as(&self, session: Option<&str>) -> bool {
        let named = session.map(str::trim).filter(|session| !session.is_empty());
        let mut state = self.external_state();
        state.last_activity_ms = unix_ms();
        if let Some(named) = named {
            state.session_id = Some(named.to_string());
        }
        if state.since_ms.is_some() {
            return false;
        }
        if named.is_none() {
            state.session_id = None;
        }
        state.since_ms = Some(state.last_activity_ms);
        state.idle_since_ms = None;
        state.queue = std::mem::take(&mut state.undelivered);
        state.memory_search_since_plan_create = false;
        state.bug_search_since_plan_create = false;
        drop(state);
        if let Some(board) = self.board() {
            board.set_external_lead_attached(true);
        }
        true
    }

    /// Bring an idle-released lead back through its own hook. Only when the
    /// lead was released for silence (it has not really detached) and
    /// `session` names the Claude Code session that staked the claim: a hook
    /// from another session must never take the lead, and an anonymous lead
    /// has no hooks to come back with. Returns `true` when the lead attached
    /// again — which also clears the idle mark and keeps the claim staked,
    /// so it can come back the same way the next time it idles out.
    pub fn reattach_idle_external_lead(&self, session: Option<&str>) -> bool {
        let Some(session) = session.map(str::trim).filter(|session| !session.is_empty()) else {
            return false;
        };
        let staked = {
            let state = self.external_state();
            state.idle_since_ms.is_some() && state.session_id.as_deref() == Some(session)
        };
        if !staked {
            return false;
        }
        self.attach_external_lead_as(Some(session))
    }

    /// Remember a helper process that made a tool call while this external
    /// lead was attached. Does nothing while no external lead is attached.
    pub fn note_external_bridge(&self, bridge: &str) {
        let mut state = self.external_state();
        if state.since_ms.is_some() {
            state.bridges.insert(bridge.to_string());
        }
    }

    /// Forget a closing helper process and return how many leading helpers
    /// remain. The caller releases the lead only when this returns zero.
    pub fn forget_external_bridge(&self, bridge: &str) -> usize {
        let mut state = self.external_state();
        state.bridges.remove(bridge);
        state.bridges.len()
    }

    /// Hand the team back to the in-app lead and wake blocked waiters.
    /// Returns `true` when an external lead was attached.
    ///
    /// Notices it never read are not dropped: they move to the undelivered
    /// list, which [`TeamRuntime::take_undelivered_notices`] hands over (the
    /// caller gives them to the in-app lead, see
    /// `LeadRuntime::release_external_lead`).
    ///
    /// The staking session is kept: the idle release runs through here too,
    /// and that session's own hooks must still be able to re-attach it
    /// ([`TeamRuntime::reattach_idle_external_lead`]); a fresh claim or a
    /// forgotten idle lead clears it.
    pub fn detach_external_lead(&self) -> bool {
        let was_attached = {
            let mut state = self.external_state();
            let was_attached = state.since_ms.take().is_some();
            state.idle_since_ms = None;
            state.bridges.clear();
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

    /// Remember that the external lead was released for silence at `at_ms`
    /// rather than leaving. No-op while it is attached.
    pub fn mark_external_lead_idle(&self, at_ms: u64) {
        let mut state = self.external_state();
        if state.since_ms.is_none() {
            state.idle_since_ms = Some(at_ms);
        }
    }

    /// When the external lead was released for silence, if it has not
    /// attached again or detached since.
    pub fn external_lead_idle_since_ms(&self) -> Option<u64> {
        self.external_state().idle_since_ms
    }

    /// The lead state a `team_status` answer reports, read under one lock so
    /// an attach or idle release cannot land between the two timestamps.
    fn lead_status(&self) -> TeamLeadStatus {
        let (since_ms, idle_since_ms) = {
            let state = self.external_state();
            (state.since_ms, state.idle_since_ms)
        };
        TeamLeadStatus {
            kind: if since_ms.is_some() || idle_since_ms.is_some() {
                "external"
            } else {
                "in_app"
            },
            attached: since_ms.is_some(),
            since_ms,
            idle_since_ms,
        }
    }

    /// Forget an idle external lead, together with the Claude Code session
    /// that staked it (after a real detach even that session's hooks must not
    /// bring it back). Returns `true` when one was idle.
    pub fn clear_external_lead_idle(&self) -> bool {
        let mut state = self.external_state();
        let was_idle = state.idle_since_ms.take().is_some();
        state.session_id = None;
        was_idle
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

    /// Keep the notices that still describe their worker: a tagged notice
    /// whose worker moved on, or no longer exists, is dropped. Untagged
    /// notices always stay.
    pub(crate) async fn current_notices(&self, notices: Vec<String>) -> Vec<String> {
        let records = self.records.lock().await;
        notices
            .into_iter()
            .filter(
                |notice| match crate::mnemo2::notice_tag::TurnTag::parse(notice) {
                    Some(tag) => records
                        .iter()
                        .any(|worker| worker.id == tag.worker_id && tag.still_current(worker)),
                    None => true,
                },
            )
            .collect()
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
            let notices = self.current_notices(self.drain_external_notices()).await;
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
                let notices = self.current_notices(self.drain_external_notices()).await;
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

    /// Test seam for the lost-follow-up path: mark a live worker as waiting for
    /// a steered turn's follow-up without sending a steer.
    #[cfg(test)]
    pub(crate) async fn set_steer_pending_for_test(&self, worker_id: &str, pending: bool) {
        if let Some(live) = self.live.lock().await.get(worker_id) {
            live.steer_pending.store(pending, Ordering::SeqCst);
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
        let report = cleanup_workspace(&self.workspace_root, &state_dir, &self.routing_log_path)
            .await?;
        let restored = self.persistence.load()?.unwrap_or_default();
        *self.records.lock().await = restored.workers;
        Ok(report)
    }

    /// The raw durable per-model usage accumulator behind
    /// [`TeamSnapshot::usage`] (read-only; the Cost tab prices its rows per
    /// model).
    pub async fn usage(&self) -> ProjectUsage {
        self.usage.lock().await.clone()
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
        let routing_mode = self
            .routing
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .mode;
        let model_description = if guidance.is_some() {
            "A specific model from the tiers, only when the tier's first model does not fit; it overrides `tier`. Prefer `tier`; models outside the tiers are refused."
        } else {
            "The exact endpoint/model spec this worker runs on. Choose it per task: a strong reasoning model for design-heavy, cross-cutting, or debugging work; a fast, inexpensive model for mechanical edits, boilerplate, or test scaffolding. Omit to use the configured default."
        };
        let mut model = json!({"type": "string", "description": model_description});
        let tier_description = match (&guidance, routing_mode) {
            (Some(text), RoutingMode::Off) => format!(
                "Required: choose the cheapest tier that fits the step (a step with a complete spec fits routine or trivial, even across files); the worker runs on that tier's first available model; an empty tier folds down.\n{text}"
            ),
            (Some(text), RoutingMode::Auto) => format!(
                "Optional: choose the cheapest tier that fits the step (a step with a complete spec fits routine or trivial, even across files), or auto to let Laya choose; the worker runs on that tier's first available model; an empty tier folds down.\n{text}"
            ),
            (Some(text), _) => format!(
                "Required: choose the cheapest tier that fits the step (a step with a complete spec fits routine or trivial, even across files), or auto where offered; the worker runs on that tier's first available model; an empty tier folds down.\n{text}"
            ),
            (None, _) => "The model tier. No tiers are configured (Settings > Tiers), so omit it and the default model is used.".to_string(),
        };
        let ladder = self.tier_ladder();
        let mut tier_names = tier_ids(&ladder);
        if matches!(routing_mode, RoutingMode::Suggest | RoutingMode::Auto) {
            tier_names.push("auto");
        }
        let tier = json!({
            "type": "string",
            "enum": tier_names,
            "description": tier_description,
        });
        let audit_tier = json!({
            "type": "string",
            "enum": tier_ids(&self.tier_ladder()),
            "description": "The audit's ladder tier; defaults to high. Empty tiers fold down. Audits are never routed by Laya or kNN; an unreported audit may be switched with team_resume, a reported audit is locked.",
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
        if guidance.is_some() && routing_mode != RoutingMode::Auto {
            dispatch_required.push("tier");
        }
        vec![
            HostToolSpec {
                name: "team_dispatch".to_string(),
                description: "Delegate one implementation task through TeamManager to a full executor worker, under a step of a plan on the plan board (plan first: plan_create, then dispatch with its plan_id and 1-based step). The worker receives the plan and its step automatically, so your brief is only the task-specific instructions. A step is checked off when one of its tasks merges and none is still open. Workers run in their own worktree and branch with file_edit, file_write, shell, git, and tests, and have no token or cost budget: give them whole, self-contained implementation tasks (code, tests, verification, commit), never read-only, content-only, or 'send me the file text' briefs. Choose a stable unique worker_id of 8-64 ASCII letters/digits and keep it with the brief/model; after a timeout or uncertain host result retry with that same identity, never a guessed replacement. Mark shared contracts, persistence, concurrency, security, or large/cross-cutting work with review_risk='high'; high-risk dispatch requires that plan to declare observable acceptance scenarios (plan_create acceptance_scenarios or plan_update add_acceptance_scenarios). The JSON response returns the authoritative worker ID and status. Never use generic agent spawning, Git, files, or team.json to infer or mutate worker lifecycle.".to_string(),
                parameters: json!({"type":"object","properties":{"worker_id":{"type":"string","minLength":8,"maxLength":64,"pattern":"^[A-Za-z0-9]+$"},"plan_id":{"type":"string","description":"The board plan this task belongs to (from plan_create or plan_list). Required."},"step":{"type":"integer","minimum":1,"description":"The 1-based step of that plan this task works on. Required; an unknown, closed, or abandoned plan, a missing step, or a step that is already done is refused."},"brief":{"type":"string","description":"Task-specific instructions only. The worker automatically receives the plan (title, goal, context, decisions, interfaces, acceptance scenarios, step list) and the full text of its step; do not repeat them."},"tier":tier.clone(),"model":model.clone(),"reasoning_effort":reasoning_effort.clone(),"read_back":{"type":"boolean","description":"Optional. When true, the worker's first turn only reads its spec and the files it names, then messages the lead with numbered questions (ambiguities, contradictions with the code, open decisions) or exactly 'No questions.', and waits for the lead's reply before changing files. Use it for large or subtle specs."}},"required":dispatch_required}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(10_000),
            },
            HostToolSpec {
                name: "team_audit_dispatch".to_string(),
                description: "Dispatch a fresh read-only independent audit of one implementation worker's current branch/tip for a high-risk review. The reviewer_id is a stable unique 8-64 ASCII letters/digits ID of your choosing; retry a timed-out dispatch with that same identity for the same worker and tip, and choose a new reviewer_id after any fix commit moves the tip. The audit receives the worker's board plan, its intent brief, and the target diff — never the worker's own summary or findings — and cannot edit files, commit, merge, spawn, ask the user, or tick plan steps. When it completes, link it through team_review's evidence.independent_reviewer_id and write an audit_adjudication; never merge the audit worker itself.".to_string(),
                parameters: json!({"type":"object","properties":{"worker_id":{"type":"string"},"reviewer_id":{"type":"string","minLength":8,"maxLength":64,"pattern":"^[A-Za-z0-9]+$"},"tier":audit_tier,"model":{"type":"string"},"reasoning_effort":{"type":"string","enum":["max","high","medium","low","minimal","none"]},"brief":{"type":"string"}},"required":["worker_id","reviewer_id"]}),
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
                description: "Read authoritative TeamManager state for every worker or one exact returned worker_id. This is the only worker lifecycle/ID authority; never infer status from Git, worktrees, timestamps, or files. Pass plan_id to see one plan's workers. Each worker is a compact entry by default (id, name, status, kind, tier, model, plan, branch, worktree, error, pending, open findings, checks, review summary, merge commit, cost, last activity); pass detail true for the full record with all review evidence. The answer also carries `lead`: whether Claude Code leads the team (kind external, attached true), idled out after a silent spell (attached false, idle_since_ms set), or the in-app lead leads (kind in_app). Ask when you need it; status is not pushed.".to_string(),
                parameters: json!({"type":"object","properties":{
                    "worker_id":{"type":"string"},
                    "plan_id":{"type":"string","description":"Only the workers dispatched on this board plan."},
                    "detail":{"type":"boolean","description":"Return each worker's full record instead of the compact entry."}
                }}),
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
                description: "Resume a stopped, interrupted, or failed worker (including an audit that has not reported) from its saved transcript through TeamManager. Returns the same worker ID with a new session generation. To move the worker to another model (it failed, or its model does not fit the work), pass `tier` or `model` (and optionally `reasoning_effort`): an idle worker is restarted on it with its transcript kept; a working one must be stopped first. The resumed worker is idle: send it a team_message to continue.".to_string(),
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
                description: "Route a larger finding back to its tracked worker for repair and delta review. Pass `tier` to run this fix round on a ladder tier, or `auto` to classify the finding; auto never steps down for high-severity findings or shared contracts. The prompt includes every open finding.".to_string(),
                parameters: json!({"type":"object","properties":{"worker_id":{"type":"string"},"finding_id":{"type":"string"},"tier":{"type":"string","enum":["auto","trivial","routine","medium","high","escalate"]}},"required":["worker_id","finding_id"]}),
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
                description: "Merge one reviewed worker only after exact-tip checks and findings close; never targets main and never pushes. After a clean merge Mnemo2 closes the worker's session, removes its checkout and deletes its branch (a safe delete: a branch that is not fully merged is kept), so never delete worker branches yourself; the merge commit keeps the worker's tip as its second parent.".to_string(),
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

    /// The worker's host tools: report back to the lead, and re-read the plan.
    pub fn worker_host_tools(&self) -> Vec<HostToolSpec> {
        vec![
            HostToolSpec {
                name: "message_parent".to_string(),
                description: "Send a short status or question to the lead.".to_string(),
                parameters: json!({"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(10_000),
            },
            HostToolSpec {
                name: "plan_read".to_string(),
                description: "Re-read your plan as it stands now (decisions and interfaces the lead added after you started), or with capability, that capability's current OpenSpec spec.".to_string(),
                parameters: json!({"type":"object","properties":{"capability":{"type":"string","description":"Optional kebab-case capability name, e.g. tier-ladder"}}}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(10_000),
            },
        ]
    }

    /// Execute a built-in lead host-tool call.
    pub async fn handle_lead_host_tool(&self, name: &str, args: Value) -> ToolResult {
        self.handle_lead_host_tool_from(name, args, MessageSender::Lead)
            .await
    }

    /// Execute a lead host-tool call with its caller's identity attached.
    pub async fn handle_lead_host_tool_from(
        &self,
        name: &str,
        args: Value,
        sender: MessageSender,
    ) -> ToolResult {
        if name == "team_dispatch" {
            let (Some(brief), Some(worker_id)) = (
                args.get("brief").and_then(Value::as_str),
                args.get("worker_id").and_then(Value::as_str),
            ) else {
                return ToolResult::error(
                    "team_dispatch requires 'brief' and a caller-chosen stable 'worker_id'",
                );
            };
            let model = args
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_string);
            let review_risk = args
                .get("review_risk")
                .and_then(Value::as_str)
                .map(str::to_string);
            let read_back = args
                .get("read_back")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let tier = args.get("tier").and_then(Value::as_str).map(str::to_string);
            let reasoning_effort = args
                .get("reasoning_effort")
                .and_then(Value::as_str)
                .map(str::to_string);
            let plan_id = args
                .get("plan_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim();
            let step = args
                .get("step")
                .and_then(Value::as_u64)
                .and_then(|step| usize::try_from(step).ok())
                .unwrap_or(0);
            if plan_id.is_empty() || step < 1 {
                return ToolResult::error(
                    "team_dispatch requires 'plan_id' (a board plan id) and 'step' (a 1-based integer). Create the plan first with plan_create.",
                );
            }
            let mode = self
                .routing
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .mode;
            let explicit_model = model.is_some();
            let fold_request = if model.is_none() { tier.clone() } else { None };
            let result = self
                .dispatch_request(DispatchRequest {
                    brief: brief.to_string(),
                    tier,
                    dispatch_routing: true,
                    source: DispatchSource::LeadTool,
                    model,
                    reasoning_effort,
                    worker_id: Some(worker_id.to_string()),
                    plan: Some(PlanRef {
                        plan_id: plan_id.to_string(),
                        step: step - 1,
                    }),
                    review_risk,
                    read_back,
                })
                .await;
            return dispatch_result(
                result,
                "team_dispatch",
                mode,
                fold_request,
                explicit_model,
            );
        }
        if name == "team_audit_dispatch" {
            let Some(worker_id) = args.get("worker_id").and_then(Value::as_str) else {
                return ToolResult::error("team_audit_dispatch requires a 'worker_id' string");
            };
            let Some(reviewer_id) = args.get("reviewer_id").and_then(Value::as_str) else {
                return ToolResult::error("team_audit_dispatch requires a 'reviewer_id' string");
            };
            let requested_tier = args.get("tier").and_then(Value::as_str).unwrap_or("high");
            let model = args.get("model").and_then(Value::as_str).map(str::to_string);
            let explicit_model = model.is_some();
            let fold_request = model.is_none().then(|| requested_tier.to_string());
            let reasoning_effort = args
                .get("reasoning_effort")
                .and_then(Value::as_str)
                .map(str::to_string);
            let brief = args
                .get("brief")
                .and_then(Value::as_str)
                .map(str::to_string);
            let result = self
                .audit_dispatch_with_tier(
                    worker_id,
                    reviewer_id,
                    Some(requested_tier),
                    model,
                    reasoning_effort,
                    brief,
                )
                .await;
            return dispatch_result(
                result,
                "team_audit_dispatch",
                RoutingMode::Off,
                fold_request,
                explicit_model,
            );
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
            // The compact entry is the default because the answer lands in the
            // lead's own context; `detail` asks for the whole record.
            let detail = args.get("detail").and_then(Value::as_bool).unwrap_or(false);
            let result = result.map(|workers| {
                workers
                    .iter()
                    .map(|worker| worker_status_view(worker, detail))
                    .collect::<Vec<_>>()
            });
            return team_status_result(result, self.lead_status());
        }
        let Some(worker_id) = args.get("worker_id").and_then(Value::as_str) else {
            return ToolResult::error(format!("{name} requires a 'worker_id' string"));
        };
        let result = match name {
            "team_message" => match args.get("text").and_then(Value::as_str) {
                Some(text) => self.message(worker_id, text.to_string(), sender).await,
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
                            let changed = self
                                .change_model(worker_id, model, reasoning_effort, ChangeSource::Lead)
                                .await;
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
                self.route_fix_with_tier(
                    worker_id,
                    args.get("finding_id").and_then(Value::as_str).unwrap_or(""),
                    args.get("tier").and_then(Value::as_str),
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
    // One parameter per dispatch knob — the tool and the Control Room form pass them explicitly.
    #[allow(clippy::too_many_arguments)]
    pub async fn dispatch_on_plan(
        &self,
        brief: String,
        model: Option<String>,
        reasoning_effort: Option<String>,
        worker_id: Option<String>,
        review_risk: Option<String>,
        read_back: bool,
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
        self.dispatch_request(DispatchRequest {
            brief,
            tier: None,
            dispatch_routing: true,
            source: DispatchSource::Ipc,
            model,
            reasoning_effort,
            worker_id,
            plan: Some(plan),
            review_risk,
            read_back,
        })
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
        self.dispatch_request(DispatchRequest {
            brief,
            tier: None,
            dispatch_routing: false,
            source: DispatchSource::Ipc,
            model,
            reasoning_effort: None,
            worker_id,
            plan: None,
            review_risk,
            read_back: false,
        })
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
        self.dispatch_request(DispatchRequest {
            brief,
            tier: None,
            dispatch_routing: false,
            source: DispatchSource::Ipc,
            model,
            reasoning_effort,
            worker_id,
            plan: None,
            review_risk: None,
            read_back: false,
        })
        .await
        .map(|(worker, _)| worker)
    }

    async fn dispatch_request(
        &self,
        request: DispatchRequest,
    ) -> Result<(WorkerRecord, bool), String> {
        let routed = self.prepare_dispatch_routing(&request).await?;
        let result = self.dispatch_locked(request, routed).await;
        if matches!(result, Ok((_, false))) {
            self.signal_board_changed();
        }
        result
    }

    async fn prepare_dispatch_routing(
        &self,
        request: &DispatchRequest,
    ) -> Result<Option<RoutedDispatch>, String> {
        if !request.dispatch_routing {
            return Ok(None);
        }
        if let Some(worker_id) = request.worker_id.as_deref() {
            let records = self.records.lock().await;
            if records.iter().any(|worker| worker.id == worker_id) {
                return Ok(None);
            }
        }
        let plan = request
            .plan
            .as_ref()
            .ok_or_else(|| "dispatch routing requires a board plan step".to_string())?;
        let board = self
            .board()
            .ok_or_else(|| "the plan board is not available".to_string())?;
        let step_text = board
            .get(&plan.plan_id)?
            .steps
            .get(plan.step)
            .ok_or_else(|| format!("plan {} has no step {}", plan.plan_id, plan.step + 1))?
            .text
            .clone();
        self.route_implementation_dispatch(request, &step_text, &request.brief)
            .await
            .map(Some)
    }

    async fn dispatch_locked(
        &self,
        request: DispatchRequest,
        routed: Option<RoutedDispatch>,
    ) -> Result<(WorkerRecord, bool), String> {
        let DispatchRequest {
            brief,
            tier,
            dispatch_routing,
            source: _,
            model,
            reasoning_effort,
            worker_id,
            plan,
            review_risk,
            read_back,
        } = request;
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
                let retry_model = if dispatch_routing {
                    match tier.as_deref() {
                        Some("auto") | None if model.is_none() => existing.model.clone(),
                        _ => self.lead_model_choice(
                            tier.as_deref().filter(|tier| *tier != "auto"),
                            model.clone(),
                        )?,
                    }
                } else {
                    model.clone()
                };
                let retry_effort = if dispatch_routing && reasoning_effort.is_none() {
                    existing.reasoning_effort.clone()
                } else {
                    reasoning_effort.clone()
                };
                if existing.brief != brief
                    || existing.model != retry_model
                    || existing.reasoning_effort != retry_effort
                    || existing.read_back != read_back
                    || plan.as_ref().is_some_and(|plan| {
                        existing.plan_id.as_deref() != Some(plan.plan_id.as_str())
                            || existing.plan_step != Some(plan.step)
                    })
                {
                    return Err(format!(
                        "worker '{id}' already exists with a different brief, model, reasoning effort, read_back, or plan step; use team_status instead of guessing a new ID (to move it to another model, use team_resume with `tier` or `model`)"
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
                let blockers = board.dispatch_blockers(&plan.plan_id)?;
                if !blockers.is_empty() {
                    return Err(format!(
                        "plan {} cannot be dispatched until its OpenSpec artifacts are complete:\n\n{}",
                        plan.plan_id,
                        blockers.join("\n\n")
                    ));
                }
                Some(format!("{}\n---\n\n{brief}", header.trim_end()))
            }
            None => None,
        };
        let (model, reasoning_effort, tier, routing, routing_rows, routing_error) =
            if dispatch_routing {
                let routed = routed
                    .ok_or_else(|| "dispatch routing was not prepared".to_string())?;
                (
                    routed.model,
                    routed.reasoning_effort,
                    routed.tier,
                    routed.routing,
                    routed.rows,
                    routed.error,
                )
            } else {
                (model, reasoning_effort, tier, None, Vec::new(), None)
            };
        if let Some(error) = routing_error {
            for row in &routing_rows {
                self.append_dispatch_routing_row(row);
            }
            return Err(error);
        }
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
        let provisioned = self
            .provision_worker(
                brief,
                opening,
                model,
                reasoning_effort,
                worker_id,
                review_risk,
                tier,
                plan,
            )
            .await;
        // Provisioning leaves the worker stopped; the read-back flag lands on
        // the stored record before its session starts, so the opening kickoff
        // matches the record the session was started from.
        let result = match provisioned {
            Ok(record) => {
                for mut row in routing_rows {
                    row.with_worker_id(&record.id);
                    self.append_dispatch_routing_row(&row);
                }
                let routing_for_record = routing.clone();
                self.update_worker(&record.id, |worker| {
                    worker.read_back = read_back;
                    worker.routing = routing_for_record;
                })
                .await;
                self.start_worker(WorkerRecord {
                    read_back,
                    routing,
                    ..record
                })
                .await
            }
            Err(err) => Err(err),
        };
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
        self.audit_dispatch_with_tier(
            worker_id,
            reviewer_id,
            None,
            model,
            reasoning_effort,
            brief,
        )
        .await
    }

    /// Dispatch an audit on an optional ladder tier (default `high`). The
    /// resolved model and tier are stored before the worker starts.
    pub async fn audit_dispatch_with_tier(
        &self,
        worker_id: &str,
        reviewer_id: &str,
        tier: Option<&str>,
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
        let requested_tier = tier.unwrap_or("high");
        if self.tier_ladder().get(requested_tier).is_none() {
            return Err(format!(
                "unknown tier '{requested_tier}'; expected one of: {}",
                tier_ids(&self.tier_ladder()).join(", ")
            ));
        }
        let (model, resolved_tier) = if model.is_none() && self.tiered_models().is_empty() {
            (
                self.lead_model_choice(None, None)?,
                Some(requested_tier.to_string()),
            )
        } else {
            self.lead_choice_with_tier(Some(requested_tier), model)?
        };
        let resolved_tier = resolved_tier.or_else(|| Some(requested_tier.to_string()));
        let reasoning_effort = reasoning_effort.or_else(|| {
            resolved_tier.as_deref().and_then(|tier| {
                self.tier_ladder()
                    .get(tier)
                    .and_then(|rung| rung.effort.clone())
            })
        });
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
            inbox: Vec::new(),
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
            read_back: false,
            compacting: false,
            total_tokens: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
            cached_tokens: 0,
            requests: 0,
            cost_micros: None,
            cost_notified_micros: 0,
            created_at_ms: millis,
            elapsed_ms: 0,
            turns: 0,
            last_activity_ms: millis,
            checks: Vec::new(),
            review_risk: "standard".to_string(),
            kind: "audit".to_string(),
            audit_target: Some(worker_id.to_string()),
            audit_tip: Some(tip),
            report_delivered: false,
            reasoning_effort,
            review_rounds: 0,
            fix_rounds: 0,
            audits: 0,
            models_used: Vec::new(),
            findings: Vec::new(),
            review: None,
            fixer_finding_id: None,
            merge: None,
            error: None,
            tool_errors: Vec::new(),
            opening_prompt: None,
            plan_id: None,
            plan_step: None,
            tier: resolved_tier,
            routing: None,
            reroutes: Vec::new(),
        };
        let audit = self.start_audit_worker(record, base_tip).await?;
        // One audit per audit worker: retrying the same reviewer at the same
        // tip reuses its record, so it is not a new round.
        self.update_worker(worker_id, |worker| worker.audits += 1)
            .await;
        self.persist().await?;
        Ok((audit, false))
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

    /// Stamp the resolved tier for a lead's explicit tier choice. An empty
    /// requested tier folds down exactly as it did for model selection.
    async fn record_tier(&self, worker_id: &str, tier: &str) {
        let Ok((resolved, _)) = self.resolve_tier_choice(tier) else {
            return;
        };
        {
            let mut records = self.records.lock().await;
            match records.iter_mut().find(|w| w.id == worker_id) {
                Some(worker) if worker.tier.as_deref() != Some(resolved.as_str()) => {
                    worker.tier = Some(resolved);
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

    // One parameter per provisioning input the dispatch sites pass explicitly.
    #[allow(clippy::too_many_arguments)]
    async fn provision_worker(
        &self,
        brief: String,
        opening_prompt: Option<String>,
        model: Option<String>,
        reasoning_effort: Option<String>,
        worker_id: Option<String>,
        review_risk: String,
        tier: Option<String>,
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
            inbox: Vec::new(),
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
            read_back: false,
            compacting: false,
            total_tokens: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
            cached_tokens: 0,
            requests: 0,
            cost_micros: None,
            cost_notified_micros: 0,
            created_at_ms: millis,
            elapsed_ms: 0,
            turns: 0,
            last_activity_ms: millis,
            checks: Vec::new(),
            review_risk,
            review: None,
            review_rounds: 0,
            fix_rounds: 0,
            audits: 0,
            models_used: Vec::new(),

            kind: "implementation".to_string(),
            audit_target: None,
            audit_tip: None,
            report_delivered: false,
            findings: Vec::new(),

            fixer_finding_id: None,
            merge: None,
            error: None,
            tool_errors: Vec::new(),
            plan_id: plan.as_ref().map(|plan| plan.plan_id.clone()),
            plan_step: plan.map(|plan| plan.step),
            tier,
            routing: None,
            reroutes: Vec::new(),
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
        Ok(record)
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
            let model = session_model_spec(worker).to_string();
            note_model(worker, &model);
        })
        .await;
        self.persist().await?;
        let session = match self
            .spawn_worker_session(WorkerSessionSpec {
                model: record.model.clone(),
                reasoning_effort: record.reasoning_effort.clone(),
                worktree: record.worktree.clone(),
                resume: Some(record.transcript.clone()),
                kind: record.kind.clone(),
                trace_identity: Self::worker_trace_identity(&record),
                pinned_task: Some(
                    record
                        .opening_prompt
                        .clone()
                        .unwrap_or_else(|| record.brief.clone()),
                ),
            })
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
        let strikes = Arc::new(Mutex::new(route_question::Strikes::new(NUDGE_STRIKE_LIMIT)));
        let steer_pending = Arc::new(AtomicBool::new(false));
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
            self.routing_log_path.clone(),
            self.routing.clone(),
            self.signals.clone(),
            strikes.clone(),
            steer_pending.clone(),
            self.models.clone(),
            self.tiers.clone(),
            self.live.clone(),
            self.fail_next_close.clone(),
            self.board(),
        ));
        if let Err(err) = session
            .send(SessionCommand::Prompt {
                text: kickoff_message(&record).to_string(),
                images: Vec::new(),
            })
            .await
        {
            pump.abort();
            let _ = session.close().await;
            let error = format!(
                "could not send the opening prompt to worker '{id}': {err}; retry with the same worker ID"
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
                strikes,
                steer_pending,
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

    /// The trace identity for one worker (plan c3ba2b58 step B): built from
    /// its record at spawn, so every model call the worker makes is
    /// attributed to this worker, its plan, and its step. The agent and
    /// session ids are filled in by the spawn path; the plan step converts
    /// from the record's 0-based step to the plan's 1-based numbering.
    fn worker_trace_identity(record: &WorkerRecord) -> TraceIdentity {
        TraceIdentity {
            actor_kind: TraceActorKind::Worker,
            agent_id: String::new(),
            session_id: String::new(),
            agent_name: record.name.clone(),
            worker_id: Some(record.id.clone()),
            plan_id: record.plan_id.clone(),
            plan_step: record.plan_step.map(|step| step as u32 + 1),
        }
    }

    /// Open one live worker session on this team's engine (the brief's
    /// spawn path): the guardrails match the worker `kind`, the worktree is
    /// bound to the session, and the trace identity attributes every model
    /// call to this worker (plan c3ba2b58 step B). The session's display
    /// name is the trace identity's `agent_name` — both are the worker
    /// record's name at every call site.
    pub(crate) async fn spawn_worker_session(
        &self,
        spec: WorkerSessionSpec,
    ) -> Result<AgentSession, String> {
        let WorkerSessionSpec {
            model,
            reasoning_effort,
            worktree,
            resume,
            kind,
            trace_identity,
            pinned_task,
        } = spec;
        let name = trace_identity.agent_name.clone();
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
                guardrails: Some(guardrails_for(&kind)),
                // Plan c3ba2b58 step B: the worker's trace identity, built
                // from its record — every model call the worker makes is
                // attributed to this worker, its plan, and its step.
                trace_identity: Some(trace_identity),
                // Plan 40eef714 step 2: the worker's whole task — the plan,
                // its step spec, and the lead's brief — pinned into the
                // system head so compaction and model switches never
                // summarize it away.
                pinned_task,
                // Plan 2ba2168a step 1: the worker's turns are never
                // classified by the per-prompt gate — Mnemo2 routes at
                // dispatch instead.
                skip_turn_routing: true,
            })
            .await
            .map_err(|err| format!("could not spawn the worker session: {err}"))
    }

    /// Mark a worker failed after its session could not spawn or resume.
    /// Failed-and-stopped is a terminal status (design §4.7): the worker's
    /// `outcome` row is appended once the record persists.
    async fn fail_worker(&self, worker_id: &str, error: String) {
        self.update_worker(worker_id, |worker| {
            worker.status = WorkerStatus::Failed;
            worker.error = Some(error);
        })
        .await;
        let _ = self.persist().await;
        if let Ok(record) = self.worker(worker_id).await {
            // A failure is not terminal — the worker can still be resumed
            // on a stronger tier and take its nudge — so only the outcome
            // row, never nudge_ignored.
            if logs_label(&record, self.routing_mode()) {
                if let Some(row) = outcome_row(&record, "failed") {
                    self.append_dispatch_routing_row(&row);
                }
            }
        }
    }

    /// Send a message to a worker with its sender attached. A busy worker gets
    /// it as a steer; an idle worker starts a new turn with it.
    pub async fn message(
        &self,
        worker_id: &str,
        text: String,
        sender: MessageSender,
    ) -> Result<WorkerRecord, String> {
        let _guard = self.merge_lock.lock().await;
        self.message_inner(worker_id, text, sender).await
    }

    async fn message_inner(
        &self,
        worker_id: &str,
        text: String,
        sender: MessageSender,
    ) -> Result<WorkerRecord, String> {
        self.message_inner_with_kind(worker_id, text, sender, None)
            .await
    }

    async fn message_inner_with_kind(
        &self,
        worker_id: &str,
        text: String,
        sender: MessageSender,
        kind: Option<InboxKind>,
    ) -> Result<WorkerRecord, String> {
        if text.trim().is_empty() {
            return Err("the worker message is empty".to_string());
        }
        let record = self.worker(worker_id).await?;
        if record.status == WorkerStatus::Cancelled {
            return Err(
                "a cleaned worker cannot accept new work; review its retained branch".to_string(),
            );
        }
        if audit_finished(&record) {
            return Err(audit_finished_error(worker_id));
        }
        if record.compacting {
            return Err(
                "wait for the worker's compaction to finish before messaging it".to_string(),
            );
        }
        let previous_inbox = record.inbox.clone();
        let delivered_text = format!("{}\n{text}", sender.header());
        let (busy, idle, steer_pending, sent) = {
            let live = self.live.lock().await;
            let live = live
                .get(worker_id)
                .ok_or_else(|| format!("worker '{worker_id}' is not running; resume it first"))?;
            live.admit_session_command(worker_id)?;
            let busy = live.busy.clone();
            let steer_pending = live.steer_pending.clone();
            let idle = !busy.load(Ordering::SeqCst);
            let kind = kind.unwrap_or(if idle {
                InboxKind::Prompt
            } else {
                InboxKind::Steer
            });
            self.update_worker(worker_id, |worker| {
                append_inbox(
                    worker,
                    InboxEntry {
                        sender,
                        kind,
                        text: text.clone(),
                        at_ms: now_key(),
                    },
                );
            })
            .await;
            // Persist the receipt before the command can end another turn.
            if let Err(err) = self.persist().await {
                self.update_worker(worker_id, |worker| {
                    worker.inbox = previous_inbox.clone();
                })
                .await;
                let _ = self.persist().await;
                return Err(err);
            }
            if idle {
                busy.store(true, Ordering::SeqCst);
            }
            let command = if idle {
                SessionCommand::Prompt {
                    text: delivered_text,
                    images: Vec::new(),
                }
            } else {
                SessionCommand::Steer {
                    text: delivered_text,
                    images: Vec::new(),
                }
            };
            // A steer ends the current turn at its next break and drives a
            // follow-up turn: mark the wait before the command can land, so the
            // pump never reads the steered turn end as a finished turn.
            if !idle {
                steer_pending.store(true, Ordering::SeqCst);
            }
            let result = live.session.send(command).await;
            (busy, idle, steer_pending, result)
        };
        if let Err(err) = sent {
            if idle {
                busy.store(false, Ordering::SeqCst);
            }
            if !idle {
                // The steer never reached the session: there is no follow-up
                // turn to wait for.
                steer_pending.store(false, Ordering::SeqCst);
            }
            self.update_worker(worker_id, |worker| worker.inbox = previous_inbox)
                .await;
            let _ = self.persist().await;
            return Err(format!("could not message the worker: {err}"));
        }
        self.update_worker(worker_id, |worker| {
            worker.status = if worker.fixer_finding_id.is_some() {
                WorkerStatus::Fixing
            } else {
                WorkerStatus::Working
            };
            worker.pending = None;
            // A delivered message starts the worker over: a stale failure is
            // no longer the record's truth.
            worker.error = None;
        })
        .await;
        self.persist().await?;
        self.spawn_drift_check(worker_id, &text).await;
        self.worker(worker_id).await
    }

    /// The drift nudge (design §4.6): a delivered lead message that Laya
    /// reads a tier above the worker's own nudges the lead — in the
    /// background, so it never blocks or fails the message. Off in routing
    /// mode `off` (no classifier calls) and when nudges are disabled.
    async fn spawn_drift_check(&self, worker_id: &str, text: &str) {
        let settings = self
            .routing
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if settings.mode == RoutingMode::Off || !settings.nudges {
            return;
        }
        if text.split_whitespace().count() < settings.min_words {
            return;
        }
        let Some(classifier) = self
            .classifier
            .as_ref()
            .and_then(|slot| slot.read().unwrap_or_else(|e| e.into_inner()).clone())
        else {
            return;
        };
        // The pre-checks run without detaching: a worker without a routing
        // record, already nudged, or on the top enabled tier never spends
        // a classifier call on drift. The enabled set is built from this
        // worker's own model, so a nudge never suggests the model it runs.
        let (enabled, worth_asking) = {
            let records = self.records.lock().await;
            let Some(worker) = records.iter().find(|worker| worker.id == worker_id) else {
                return;
            };
            let Some(routing) = worker.routing.as_ref() else {
                return;
            };
            let enabled = self.direct_nudge_tiers(worker.model.as_deref());
            (
                enabled,
                routing.nudged.is_none()
                    && next_tier_above(enabled, &final_tier(worker)).is_some(),
            )
        };
        if !worth_asking {
            return;
        }
        let records = self.records.clone();
        let signals = self.signals.clone();
        let routing_log_path = self.routing_log_path.clone();
        let permit = self.routing_permit.clone();
        let route_knn = self.route_knn.clone();
        let worker_id = worker_id.to_string();
        let text: String = text.chars().take(TEXT_MAX_CHARS).collect();
        let mode = settings.mode;
        tokio::spawn(async move {
            // The same 3 s timeout and one-at-a-time permit as dispatch,
            // without holding the merge lock: a message never waits on
            // Laya.
            let classified = async {
                let _permit = permit.acquire().await;
                classify_dispatch(
                    classifier.as_ref(),
                    route_knn.as_deref(),
                    &text,
                    enabled,
                    &settings,
                )
                .await
            };
            let decision =
                match tokio::time::timeout(std::time::Duration::from_secs(3), classified).await {
                    Ok(Some(decision)) => decision,
                    _ => return,
                };
            if !decision.cleared {
                return;
            }
            let Some(to_tier) = decision.tier else {
                return;
            };
            let Some(claim) =
                claim_nudge(&records, &worker_id, enabled, mode, Some(to_tier)).await
            else {
                return;
            };
            let detail = format!(
                "this got harder (Laya reads the message as {}, {:.2})",
                decision.label, decision.confidence
            );
            deliver_nudge(
                &signals,
                &routing_log_path,
                &worker_id,
                &claim,
                "drift",
                &detail,
                "",
            );
        });
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
    /// audit; `resume` can reattach the saved transcript later. A worker that
    /// was not already stopped has reached a terminal status, so its `outcome`
    /// row (design §4.7) is appended beside the record.
    pub async fn stop(&self, worker_id: &str) -> Result<WorkerRecord, String> {
        let _guard = self.merge_lock.lock().await;
        let before = self.worker(worker_id).await?;
        let record = self.stop_inner(worker_id).await?;
        if !matches!(before.status, WorkerStatus::Stopped) {
            // A hand reroute is a learning in every mode, so a rerouted
            // worker's outcome row is written even off; the nudge row is
            // Laya-derived and keeps the `logs_routing` gate.
            if logs_label(&record, self.routing_mode()) {
                if let Some(row) = outcome_row(&record, "stopped") {
                    self.append_dispatch_routing_row(&row);
                }
            }
            if logs_routing(&record, self.routing_mode()) {
                if let Some(row) = nudge_ignored_row(&record) {
                    self.append_dispatch_routing_row(&row);
                }
            }
        }
        Ok(record)
    }

    async fn stop_inner(&self, worker_id: &str) -> Result<WorkerRecord, String> {
        let record = self.worker(worker_id).await?;
        if record.status == WorkerStatus::Cancelled {
            return Err("a cleaned worker has no checkout to stop".to_string());
        }
        if matches!(record.status, WorkerStatus::Merged | WorkerStatus::Merging) {
            return Err("a merged worker cannot be stopped".to_string());
        }
        let preserve_downshift_finding = record.reroutes.last().is_some_and(|row| {
            matches!(
                row,
                DispatchRoutingRow::LeadReroute {
                    from_tier,
                    to_tier,
                    nudged: true,
                    ..
                } if NUDGE_LADDER.iter().position(|tier| tier == from_tier)
                    > NUDGE_LADDER.iter().position(|tier| tier == to_tier)
            )
        });
        self.update_worker(worker_id, |worker| {
            worker.status = WorkerStatus::Stopped;
            worker.pending = None;
            if !preserve_downshift_finding {
                worker.fixer_finding_id = None;
            }
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
                                kickoff_message(worker).to_string(),
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
        let live_idle = {
            let live = self.live.lock().await;
            match live.get(worker_id) {
                Some(live) => {
                    live.admit_session_command(worker_id)?;
                    Some(!live.busy.load(Ordering::SeqCst))
                }
                None => None,
            }
        };
        if let Some(idle) = live_idle {
            let record = self.worker(worker_id).await?;
            // A failed worker whose session is still alive and idle is
            // recoverable in place: the lead can keep the same model with
            // `team_message`, or retry on another one through a resume that
            // respawns the session.
            if idle && record.status == WorkerStatus::Failed {
                self.update_worker(worker_id, |worker| {
                    worker.status = WorkerStatus::Idle;
                    worker.error = None;
                    worker.pending = None;
                })
                .await;
                self.persist().await?;
                return self.worker(worker_id).await;
            }
            return Err(format!(
                "worker '{worker_id}' is already running; team_message it to continue"
            ));
        }
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
        if audit_finished(&record) {
            return Err(audit_finished_error(worker_id));
        }
        if !record.worktree.is_dir() {
            return Err(format!(
                "the worker's worktree is missing: {}",
                record.worktree.display()
            ));
        }
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let session = self
            .spawn_worker_session(WorkerSessionSpec {
                model: record.model.clone(),
                reasoning_effort: record.reasoning_effort.clone(),
                worktree: record.worktree.clone(),
                resume: Some(record.transcript.clone()),
                kind: record.kind.clone(),
                trace_identity: Self::worker_trace_identity(&record),
                pinned_task: Some(
                    record
                        .opening_prompt
                        .clone()
                        .unwrap_or_else(|| record.brief.clone()),
                ),
            })
            .await?;
        let session_id = session.id();
        let busy = Arc::new(AtomicBool::new(false));
        let receiver = session.events();
        let strikes = Arc::new(Mutex::new(route_question::Strikes::new(NUDGE_STRIKE_LIMIT)));
        let steer_pending = Arc::new(AtomicBool::new(false));
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
            self.routing_log_path.clone(),
            self.routing.clone(),
            self.signals.clone(),
            strikes.clone(),
            steer_pending.clone(),
            self.models.clone(),
            self.tiers.clone(),
            self.live.clone(),
            self.fail_next_close.clone(),
            self.board(),
        ));
        self.live.lock().await.insert(
            worker_id.to_string(),
            LiveWorker {
                session,
                busy,
                pump,
                cleanup_retry_only: false,
                strikes,
                steer_pending,
            },
        );
        self.update_worker(worker_id, |worker| {
            worker.status = WorkerStatus::Idle;
            worker.generation = generation;
            worker.pending = None;
            worker.error = None;
            worker.compacting = false;
            let model = session_model_spec(worker).to_string();
            note_model(worker, &model);
        })
        .await;
        self.persist().await?;
        self.worker(worker_id).await
    }

    /// The reroute row for a model switch (design §4.7): the worker's route
    /// before and after, the status it was in, the nudge it carried, and who
    /// moved it. `None` when the switch does not move the model (an
    /// effort-only change): nothing was rerouted, nothing is logged.
    fn reroute_row(
        &self,
        record: &WorkerRecord,
        from_spec: &str,
        to_model: Option<&str>,
        source: ChangeSource,
        nudged: Option<&str>,
    ) -> Option<DispatchRoutingRow> {
        let to_spec = to_model.unwrap_or("default");
        if to_spec == from_spec {
            return None;
        }
        let ts = unix_ms();
        let worker_id = record.id.clone();
        let plan_id = record.plan_id.clone().unwrap_or_default();
        let step = record.plan_step.map_or(0, |step| step + 1);
        let from_tier = self
            .tier_of(from_spec)
            .or_else(|| record.tier.clone())
            .unwrap_or_else(|| "custom".to_string());
        let to_tier = self
            .tier_of(to_spec)
            .unwrap_or_else(|| "custom".to_string());
        let from_model = record.model.clone().filter(|spec| !spec.trim().is_empty());
        let to_model = to_model.map(str::to_string);
        let status = status_name(record.status);
        let review_rounds = record.review_rounds;
        let fix_rounds = record.fix_rounds;
        // A downshift stays marked until its delta review records the outcome;
        // this is the durable pending marker in team.json.
        let nudged = nudged.is_some() || source == ChangeSource::FixDownshift;
        Some(match source {
            ChangeSource::Lead
            | ChangeSource::Escalation
            | ChangeSource::FixDownshift
            | ChangeSource::FixDownshiftRestore => DispatchRoutingRow::LeadReroute {
                ts,
                worker_id,
                plan_id,
                step,
                from_tier,
                to_tier,
                from_model,
                to_model,
                status,
                review_rounds,
                fix_rounds,
                nudged,
            },
            ChangeSource::User => DispatchRoutingRow::UserReroute {
                ts,
                worker_id,
                plan_id,
                step,
                from_tier,
                to_tier,
                from_model,
                to_model,
                status,
                review_rounds,
                fix_rounds,
                nudged,
            },
        })
    }

    /// Move a worker to another model (and reasoning effort), keeping its
    /// transcript, branch, and worktree. A WORKING worker switches live at
    /// its next request: the call in flight finishes on the old model, the
    /// next one serves on the new one — no stop, no restart. An audit
    /// worker's model is never switchable. An idle live session is
    /// restarted on the new model; a stopped, interrupted, or failed one
    /// is resumed on it; a worker whose brief never reached a session is
    /// started with it.
    ///
    /// A switch that moves the model appends a `lead_reroute` or
    /// `user_reroute` row (design §4.7) — by [`source`] — carrying the
    /// route before and after, in every routing mode: a hand-moved model is
    /// always a learning (decided 2026-11-04). It repoints the record's
    /// tier at the destination tier (`None` for a custom model) and clears
    /// a pending nudge (the nudge was answered by a hand, not by Laya). A
    /// same-tier switch logs with `from_tier == to_tier` and never counts
    /// as an override; an effort-only change logs nothing.
    pub async fn change_model(
        &self,
        worker_id: &str,
        model: Option<String>,
        reasoning_effort: Option<String>,
        source: ChangeSource,
    ) -> Result<WorkerRecord, String> {
        let _guard = self.merge_lock.lock().await;
        self.change_model_locked(worker_id, model, reasoning_effort, source)
            .await
    }

    async fn change_model_locked(
        &self,
        worker_id: &str,
        model: Option<String>,
        reasoning_effort: Option<String>,
        source: ChangeSource,
    ) -> Result<WorkerRecord, String> {
        let record = self.worker(worker_id).await?;
        if record.status == WorkerStatus::Cancelled {
            return Err("a cleaned worker cannot change its model".to_string());
        }
        if matches!(record.status, WorkerStatus::Merged | WorkerStatus::Merging) {
            return Err("a merged worker cannot change its model".to_string());
        }
        if audit_finished(&record) {
            return Err(audit_finished_error(worker_id));
        }
        self.validate_model(&model, &reasoning_effort)?;
        let from_spec = session_model_spec(&record).to_string();
        let nudged = record
            .routing
            .as_ref()
            .and_then(|routing| routing.nudged.clone());
        // The record keeps its reroute either way (team.json bookkeeping),
        // and the calibration log gets it too: a hand-moved model is a
        // learning in every routing mode (decided 2026-11-04).
        let requested = model.as_deref().filter(|spec| !spec.trim().is_empty());
        if self.worker_is_busy(worker_id).await {
            // Live switch — no stop, no restart: the provider request in
            // flight finishes on the old model; the worker's next request
            // serves on the new one. The kernel emits its model_changed
            // agent event once the new model serves, and the pump re-points
            // `model_id` from it. Moving a working worker to routing
            // (model: None) needs the restart path below — refuse it.
            let Some(spec) = model.clone() else {
                return Err(format!(
                    "worker '{worker_id}' is working; a live model switch needs a model; stop the worker to move it to routing"
                ));
            };
            let reroute =
                self.reroute_row(&record, &from_spec, requested, source, nudged.as_deref());
            let switched = {
                let live = self.live.lock().await;
                let live = live
                    .get(worker_id)
                    .ok_or_else(|| format!("worker '{worker_id}' is not running"))?;
                live.admit_session_command(worker_id)?;
                live.session
                    .set_model(&spec, reasoning_effort.clone())
                    .await
            };
            let new_model_id = match switched {
                Ok(ModelSwitch::NextRequest { model: served })
                | Ok(ModelSwitch::CompactsFirst { model: served }) => served,
                Err(error) => {
                    return Err(format!(
                        "could not switch worker '{worker_id}' to '{spec}': {error}"
                    ));
                }
            };
            self.update_worker(worker_id, |worker| {
                worker.model = model.clone();
                // Never erase an already-observed id that already matches
                // the new model: the new model's events may have been
                // pumped before this update (the pin landed mid-turn, or
                // the target was already serving) — an unconditional clear
                // could leave the record without its correct id, with no
                // later event left to repair it.
                if worker.model_id.as_deref() != Some(new_model_id.as_str()) {
                    worker.model_id = None;
                }
                worker.reasoning_effort = reasoning_effort.clone();
                if let Some(row) = &reroute {
                    worker.tier = reroute_destination(row);
                    worker.reroutes.push(row.clone());
                    if let Some(routing) = worker.routing.as_mut() {
                        routing.nudged = None;
                    }
                }
                let model = session_model_spec(worker).to_string();
                note_model(worker, &model);
            })
            .await;
            self.persist().await?;
            if reroute.is_some() {
                // A reroute resets the nudge budget (design §4.6): the
                // strike counter starts over on the new tier. The stopped
                // path below drops the live worker; its replacement on
                // resume builds a fresh counter.
                if let Some(live) = self.live.lock().await.get(worker_id) {
                    live.strikes.lock().await.reset();
                }
            }
            if record.kind != "audit" && source != ChangeSource::FixDownshift {
                if let Some(row) = reroute.as_ref() {
                    self.append_reroute_row(row, source, None);
                }
            }
            return self.worker(worker_id).await;
        }
        if self.live.lock().await.contains_key(worker_id) {
            self.stop_inner(worker_id).await?;
        }
        let reroute = self.reroute_row(&record, &from_spec, requested, source, nudged.as_deref());
        self.update_worker(worker_id, |worker| {
            worker.model = model.clone();
            worker.model_id = None;
            worker.reasoning_effort = reasoning_effort.clone();
            if let Some(row) = &reroute {
                worker.tier = reroute_destination(row);
                worker.reroutes.push(row.clone());
                if let Some(routing) = worker.routing.as_mut() {
                    routing.nudged = None;
                }
            }
            let model = session_model_spec(worker).to_string();
            note_model(worker, &model);
        })
        .await;
        self.persist().await?;
        if record.kind != "audit" && source != ChangeSource::FixDownshift {
            if let Some(row) = reroute.as_ref() {
                self.append_reroute_row(row, source, None);
            }
        }
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
        if audit_finished(&record) {
            return Err(audit_finished_error(worker_id));
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
        // Capture every escalation trigger before this review replaces the
        // previous finding set and clears the routed-finding marker.
        let rounds = worker.review_rounds.saturating_add(1);
        let repeated = (rounds >= 2)
            .then(|| {
                findings.iter().find(|finding| {
                    finding.status == "open"
                        && worker.findings.iter().any(|old| {
                            old.id == finding.id && old.status == "open"
                        })
                })
            })
            .flatten()
            .cloned();
        let routed = worker.fixer_finding_id.as_deref().and_then(|id| {
            findings
                .iter()
                .find(|finding| finding.id == id && finding.status == "open")
                .cloned()
        });
        let audited = (worker.fix_rounds > 0 && evidence.independent_reviewer_id.is_some())
            .then(|| {
                findings
                    .iter()
                    .find(|finding| finding.status == "open" && finding.severity == "high")
                    .cloned()
            })
            .flatten();
        let (escalation_finding, escalation_trigger) = match (routed, repeated, audited) {
            (Some(finding), _, _) => (Some(finding), "route_fix"),
            (None, Some(finding), _) => (Some(finding), "review"),
            (None, None, Some(finding)) => (Some(finding), "audit"),
            (None, None, None) => (None, "review"),
        };
        let downshift = pending_fix_downshift(&worker, &findings);
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
            worker.review_rounds += 1;
            worker.fixer_finding_id = None;
            worker.error = None;
        })
        .await;
        self.persist().await?;
        let updated = self.worker(worker_id).await?;
        let updated = match downshift {
            Some((row, closed)) => self.settle_fix_downshift(worker_id, row, closed).await?,
            None => updated,
        };
        // Off/shadow/suggest leave a lead notice. Auto additionally switches
        // an idle worker, preserving the live session and transcript.
        let settings = self
            .routing
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if settings.mode != RoutingMode::Off && settings.nudges {
            if let Some(finding) = escalation_finding {
                let enabled = self.direct_nudge_tiers(updated.model.as_deref());
                if let Some(claim) =
                    claim_nudge(&self.records, worker_id, enabled, settings.mode, None).await
                {
                    let detail = if escalation_trigger == "audit" {
                        format!(
                            "high-severity audit finding {} remains after a fix round",
                            finding.id
                        )
                    } else if finding.severity == "high" && updated.review_rounds >= 2 {
                        format!(
                            "keeps a high finding after {} review rounds",
                            updated.review_rounds
                        )
                    } else if escalation_trigger == "route_fix" {
                        format!("route-fix finding {} is still open", finding.id)
                    } else {
                        format!(
                            "review finding {} stayed open after {} review rounds",
                            finding.id, updated.review_rounds
                        )
                    };
                    deliver_nudge(
                        &self.signals,
                        &self.routing_log_path,
                        worker_id,
                        &claim,
                        "review",
                        &detail,
                        "",
                    );
                    if settings.mode == RoutingMode::Auto {
                        self.auto_escalate_worker(worker_id).await?;
                    }
                }
                // The claim (and any auto switch) changed the stored record.
                return self.worker(worker_id).await;
            }
        }
        Ok(updated)
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
            worker.fix_rounds += 1;
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

    async fn settle_fix_downshift(
        &self,
        worker_id: &str,
        mut row: DispatchRoutingRow,
        finding_closed: bool,
    ) -> Result<WorkerRecord, String> {
        let (ts, from_tier, from_model) = match &row {
            DispatchRoutingRow::LeadReroute {
                ts,
                from_tier,
                from_model,
                ..
            } => (*ts, from_tier.clone(), from_model.clone()),
            _ => return self.worker(worker_id).await,
        };
        if let DispatchRoutingRow::LeadReroute { nudged, .. } = &mut row {
            *nudged = false;
        }
        self.update_worker(worker_id, |worker| {
            if let Some(DispatchRoutingRow::LeadReroute { nudged, .. }) = worker
                .reroutes
                .iter_mut()
                .rev()
                .find(|candidate| {
                    matches!(
                        candidate,
                        DispatchRoutingRow::LeadReroute { ts: candidate_ts, .. }
                            if *candidate_ts == ts
                    )
                })
            {
                *nudged = false;
            }
        })
        .await;
        self.persist().await?;
        self.append_reroute_row(&row, ChangeSource::FixDownshift, Some(finding_closed));
        if finding_closed {
            return self.worker(worker_id).await;
        }

        // The cheaper round did not close its finding: restore the prior
        // model now, while idle, so the next fix round starts there.
        let model = match from_model {
            Some(model) => Some(model),
            None if NUDGE_LADDER.contains(&from_tier.as_str()) => self
                .resolve_tier_choice(&from_tier)
                .map(|(_, choice)| Some(choice.spec))?,
            None => None,
        };
        let effort = self
            .tier_ladder()
            .get(&from_tier)
            .and_then(|rung| rung.effort.clone());
        let status = self.worker(worker_id).await?.status;
        self.change_model_locked(
            worker_id,
            model,
            effort,
            ChangeSource::FixDownshiftRestore,
        )
        .await?;
        self.update_worker(worker_id, |worker| worker.status = status)
            .await;
        self.persist().await?;
        self.worker(worker_id).await
    }

    fn finding_is_shared_contract(&self, worker: &WorkerRecord, finding: &Finding) -> bool {
        let Some(plan_id) = worker.plan_id.as_deref() else {
            return false;
        };
        let Some(board) = self.board() else {
            return false;
        };
        let Ok(markdown) = board.read_markdown(plan_id) else {
            return false;
        };
        let mut interfaces = false;
        for line in markdown.lines() {
            if line.starts_with("## ") {
                interfaces = line.trim() == "## Interfaces";
            } else if interfaces && line.contains(&finding.file) {
                return true;
            }
        }
        false
    }

    /// Classify a routed finding for `team_route_fix(tier: "auto")`. The
    /// classification is logged for shadow/gate diagnostics but is not an
    /// ordinary dispatch-training row (`skip` stays set).
    async fn classify_fix_tier(
        &self,
        worker: &WorkerRecord,
        finding: &Finding,
    ) -> Result<Option<(String, LeadModel)>, String> {
        let settings = self
            .routing
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if settings.mode == RoutingMode::Off {
            return Err("tier 'auto' needs routing mode shadow, suggest, or auto".to_string());
        }
        let text = routing_text(
            &finding.title,
            &format!("{}\nFile: {}", finding.detail, finding.file),
        )
        .unwrap_or_default();
        let enabled = self.enabled_routing_tiers();
        let classifier = self.classifier.as_ref().and_then(|slot| {
            slot.read()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        });
        let decision = if let Some(classifier) = classifier {
            let attempt = async {
                let _permit = self
                    .routing_permit
                    .acquire()
                    .await
                    .map_err(|_| "Laya routing is closed".to_string())?;
                Ok::<_, String>(
                    classify_dispatch(
                        classifier.as_ref(),
                        self.route_knn.as_deref(),
                        &text,
                        enabled,
                        &settings,
                    )
                    .await,
                )
            };
            match tokio::time::timeout(std::time::Duration::from_secs(3), attempt).await {
                Ok(result) => result?,
                Err(_) => None,
            }
        } else {
            None
        };
        let current_tier = worker
            .tier
            .clone()
            .or_else(|| worker.routing.as_ref().and_then(|routing| routing.ran_tier.clone()));
        let protected = finding.severity == "high" || self.finding_is_shared_contract(worker, finding);
        let mut selected = None;
        let skip = if decision.is_none() {
            "fix_downshift_offline"
        } else if !decision.as_ref().is_some_and(|decision| decision.cleared) {
            "fix_downshift_below_gate"
        } else if settings.mode == RoutingMode::Shadow {
            "fix_downshift_shadow"
        } else if protected {
            "fix_downshift_protected"
        } else if let Some(tier) = decision.as_ref().and_then(|decision| decision.tier) {
            let (ran_tier, choice) = self.resolve_tier_choice(tier)?;
            let cheaper = current_tier.as_deref().is_some_and(|current| {
                NUDGE_LADDER.iter().position(|candidate| *candidate == ran_tier)
                    < NUDGE_LADDER.iter().position(|candidate| *candidate == current)
            });
            if cheaper {
                selected = Some((ran_tier, choice));
                "fix_downshift_selected"
            } else {
                "fix_downshift_not_cheaper"
            }
        } else {
            "fix_downshift_unknown_label"
        };
        use sha2::Digest as _;
        let text_sha256 = sha2::Sha256::digest(text.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let ran_model = selected
            .as_ref()
            .map(|(_, choice)| choice.spec.clone())
            .or_else(|| worker.model.clone());
        let row = DispatchRoutingRow::Route {
            ts: unix_ms(),
            worker_id: worker.id.clone(),
            plan_id: worker.plan_id.clone().unwrap_or_default(),
            step: worker.plan_step.map_or(0, |step| step + 1),
            mode: settings.mode,
            text_sha256,
            label: decision
                .as_ref()
                .map(|decision| decision.label.clone())
                .unwrap_or_default(),
            confidence: decision.as_ref().map_or(0.0, |decision| decision.confidence),
            probabilities: decision
                .as_ref()
                .map(|decision| decision.probabilities.clone())
                .unwrap_or_default(),
            tier: decision.as_ref().and_then(|decision| decision.tier).map(str::to_string),
            threshold: decision.as_ref().map_or_else(
                || threshold_for(enabled.offered().count(), &settings),
                |decision| {
                    if decision.source == "knn" {
                        settings.knn_min_share
                    } else {
                        threshold_for(enabled.offered().count(), &settings)
                    }
                },
            ),
            cleared: decision.as_ref().is_some_and(|decision| decision.cleared),
            lead_tier: current_tier.clone(),
            ran_tier: selected
                .as_ref()
                .map(|(tier, _)| tier.clone())
                .or(current_tier),
            ran_model: ran_model.clone(),
            text,
            skip: Some(skip.to_string()),
            source: decision.as_ref().map(|decision| decision.source.to_string()),
            knn_label: decision.as_ref().and_then(|decision| decision.knn_label.clone()),
            knn_share: decision.as_ref().and_then(|decision| decision.knn_share),
            knn_k: decision.as_ref().and_then(|decision| decision.knn_k),
        };
        self.append_dispatch_routing_row(&row);
        Ok(selected)
    }

    /// Route a non-tiny finding to the original tracked worker.
    pub async fn route_fix(
        &self,
        worker_id: &str,
        finding_id: &str,
    ) -> Result<WorkerRecord, String> {
        self.route_fix_with_tier(worker_id, finding_id, None).await
    }

    /// Route a fix on its current model or an explicitly/classifier-selected
    /// ladder tier. The worker keeps its session and transcript.
    pub async fn route_fix_with_tier(
        &self,
        worker_id: &str,
        finding_id: &str,
        tier: Option<&str>,
    ) -> Result<WorkerRecord, String> {
        let _guard = self.merge_lock.lock().await;
        let worker = self.worker(worker_id).await?;
        // Audits only ever produce findings; they never receive them.
        debug_assert_ne!(
            worker.kind,
            "audit",
            "route_fix never targets an audit worker"
        );
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
        let selected = match tier.map(str::trim).filter(|tier| !tier.is_empty()) {
            None => None,
            Some("auto") => self.classify_fix_tier(&previous, &finding).await?,
            Some(tier) => Some(self.resolve_tier_choice(tier)?),
        };
        if let Some((ran_tier, choice)) = selected {
            let current_tier = previous
                .tier
                .as_deref()
                .or_else(|| previous.routing.as_ref().and_then(|routing| routing.ran_tier.as_deref()));
            let cheaper = current_tier.is_some_and(|current| {
                NUDGE_LADDER.iter().position(|candidate| *candidate == ran_tier)
                    < NUDGE_LADDER.iter().position(|candidate| *candidate == current)
            });
            let effort = self
                .tier_ladder()
                .get(&ran_tier)
                .and_then(|rung| rung.effort.clone());
            self.change_model_locked(
                worker_id,
                Some(choice.spec),
                effort,
                if cheaper {
                    ChangeSource::FixDownshift
                } else {
                    ChangeSource::Lead
                },
            )
            .await?;
        }
        self.update_worker(worker_id, |worker| {
            worker.status = WorkerStatus::Fixing;
            worker.fixer_finding_id = Some(finding_id.to_string());
            worker.checks.clear();
            worker.fix_rounds += 1;
            worker.review = None;
        })
        .await;
        self.persist().await?;
        let brief = route_fix_brief(&worker, &finding);
        if let Err(err) = self
            .message_inner_with_kind(worker_id, brief, MessageSender::Mnemo, Some(InboxKind::Fix))
            .await
        {
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
        // An audit worker is a read-only reviewer: it is never mergeable in ANY
        // state, so this refusal comes before every other gate. A still-running
        // audit (or a differently-named lead branch) must not mask the reason.
        let worker = self.worker(worker_id).await?;
        if worker.kind == "audit" {
            return Err(
                "an audit worker is a read-only reviewer and is never merged; adjudicate it on its implementation worker's review".to_string(),
            );
        }
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
        match cleanup_agent_worktree(&self.workspace_root, &worker).await {
            Err(err) => {
                self.update_worker(worker_id, |worker| {
                    worker.error = Some(format!(
                        "merged, but worktree cleanup needs attention: {err}"
                    ));
                })
                .await;
                self.persist().await?;
            }
            Ok(()) => {
                // The checkout is gone, so Git no longer holds the branch. The
                // merge commit keeps the worker's tip as its second parent, so
                // deleting the branch loses no work.
                let shared = self
                    .records
                    .lock()
                    .await
                    .iter()
                    .any(|other| other.id != worker.id && other.branch == worker.branch);
                if !shared {
                    if let Err(err) = delete_merged_branch(&self.workspace_root, &worker).await {
                        self.update_worker(worker_id, |worker| {
                            worker.error = Some(format!("{BRANCH_CLEANUP_ERROR}: {err}"));
                        })
                        .await;
                        self.persist().await?;
                    }
                }
            }
        }
        self.sweep_merged_branches(worker_id).await;
        let record = self.worker(worker_id).await?;
        if record.status == WorkerStatus::Merged {
            // The one terminal end a worker is labelled at (design §4.7):
            // the merge landed and the record is Merged. The retryable
            // InReview cleanup path above logs nothing; its successful retry
            // arrives here. The outcome row follows `logs_label` — a hand
            // reroute is a learning in every mode — while the nudge row is
            // Laya-derived and keeps the `logs_routing` gate.
            if logs_label(&record, self.routing_mode()) {
                if let Some(row) = outcome_row(&record, "merged") {
                    self.append_dispatch_routing_row(&row);
                }
            }
            if logs_routing(&record, self.routing_mode()) {
                if let Some(row) = nudge_ignored_row(&record) {
                    self.append_dispatch_routing_row(&row);
                }
            }
        }
        Ok(record)
    }

    /// Retry the branch delete for earlier merged workers. Runs inside
    /// `merge_locked`, so the merge lock is held. Best effort and silent: a
    /// branch Git refuses to delete stays until the next merge.
    async fn sweep_merged_branches(&self, just_merged: &str) {
        let candidates: Vec<WorkerRecord> = {
            let records = self.records.lock().await;
            records
                .iter()
                .filter(|candidate| {
                    candidate.id != just_merged
                        && ledger::merged(candidate)
                        && owns_its_branch(candidate)
                        && !records.iter().any(|other| {
                            other.id != candidate.id && other.branch == candidate.branch
                        })
                })
                .cloned()
                .collect()
        };
        let mut cleared = false;
        for candidate in candidates {
            // A checkout folder that is still there means its cleanup failed;
            // leave the whole worker alone.
            if std::fs::symlink_metadata(worker_checkout(&self.workspace_root, &candidate.id))
                .is_ok()
            {
                continue;
            }
            if let Ok(true) = delete_merged_branch(&self.workspace_root, &candidate).await {
                self.update_worker(&candidate.id, |worker| {
                    if worker
                        .error
                        .as_deref()
                        .is_some_and(|error| error.starts_with(BRANCH_CLEANUP_ERROR))
                    {
                        worker.error = None;
                    }
                })
                .await;
                cleared = true;
            }
        }
        if cleared {
            let _ = self.persist().await;
        }
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
    ///
    /// The preview is read-only, so it does not take the merge lock:
    /// `delete_workers` re-checks every worker under the lock before
    /// changing anything. Batch-wide Git facts are gathered once and the
    /// workers are inspected concurrently, so a large batch costs about
    /// the same as a small one.
    pub async fn deletion_preview(
        &self,
        worker_ids: &[String],
    ) -> Result<Vec<WorkerDeletion>, String> {
        self.deletion_preview_streaming(worker_ids, |_| {}).await
    }

    /// `deletion_preview` with per-worker progress: `on_preview` receives
    /// each worker's preview the moment its checks finish, so the open
    /// sheet can fill its rows in as they arrive.
    pub async fn deletion_preview_streaming(
        &self,
        worker_ids: &[String],
        on_preview: impl Fn(WorkerDeletion),
    ) -> Result<Vec<WorkerDeletion>, String> {
        self.inspect_batch(worker_ids, on_preview).await
    }

    /// Delete workers and their history: the ledger record (brief,
    /// transcript, telemetry, review, findings), inbox messages, checkout,
    /// and `wt/agent-*` branch. A live session is stopped first. Every
    /// worker is checked before any is deleted; work the lead branch lacks
    /// (uncommitted changes, unmerged commits) is discarded only when
    /// `discard_work` confirms it. A worker that cannot be deleted is
    /// reported with its error and stays listed — every other worker is
    /// still deleted.
    pub async fn delete_workers(
        &self,
        worker_ids: &[String],
        discard_work: bool,
    ) -> Result<TeamDeleteOutcome, String> {
        self.delete_workers_streaming(worker_ids, discard_work, |_| {}).await
    }

    /// `delete_workers` with per-worker progress: `on_result` receives
    /// each worker's outcome the moment it is known, so the open sheet
    /// can show each row's result as it happens.
    pub async fn delete_workers_streaming(
        &self,
        worker_ids: &[String],
        discard_work: bool,
        on_result: impl Fn(WorkerDeletionResult),
    ) -> Result<TeamDeleteOutcome, String> {
        let _guard = self.merge_lock.lock().await;
        // One result per requested worker, in request order: a failure
        // sets its worker's error and never stops the rest of the batch.
        let mut outcomes: Vec<WorkerDeletionResult> = Vec::with_capacity(worker_ids.len());
        for worker_id in worker_ids {
            outcomes.push(WorkerDeletionResult {
                worker_id: worker_id.clone(),
                name: self
                    .worker(worker_id)
                    .await
                    .map(|record| record.name)
                    .unwrap_or_else(|_| worker_id.clone()),
                error: None,
            });
        }
        // Phase A: check every worker before any is deleted.
        let previews = self.inspect_batch(worker_ids, |_| {}).await?;
        let mut stopped: Vec<(usize, WorkerDeletion)> = Vec::new();
        for (position, preview) in previews.into_iter().enumerate() {
            match Self::deletion_decision(&preview, discard_work) {
                Ok(()) => stopped.push((position, preview)),
                Err(error) => {
                    outcomes[position].error = Some(error.clone());
                    on_result(outcomes[position].clone());
                }
            }
        }
        // Phase B: stop every live session.
        let mut pending: Vec<(usize, WorkerDeletion)> = Vec::new();
        for (position, deletion) in stopped {
            if self.live.lock().await.contains_key(&deletion.worker_id) {
                match self.stop_inner(&deletion.worker_id).await {
                    Ok(_stopped) => pending.push((position, deletion)),
                    Err(error) => {
                        outcomes[position].error = Some(error.clone());
                        on_result(outcomes[position].clone());
                    }
                }
            } else {
                pending.push((position, deletion));
            }
        }
        // Phase C: re-check after stopping — a session may have written
        // more work between the preview and its stop. The batch for this
        // pass is the set that still proceeds, so Git state shared with a
        // worker that failed a check or a stop is never touched.
        let survivor_ids: Vec<String> = pending
            .iter()
            .map(|(_, deletion)| deletion.worker_id.clone())
            .collect();
        let rechecked = self.inspect_batch(&survivor_ids, |_| {}).await?;
        let mut deletions: Vec<(usize, WorkerDeletion)> = Vec::new();
        for (index, preview) in rechecked.into_iter().enumerate() {
            let (position, _) = pending[index];
            match Self::deletion_decision(&preview, discard_work) {
                Ok(()) => deletions.push((position, preview)),
                Err(error) => {
                    outcomes[position].error = Some(error.clone());
                    on_result(outcomes[position].clone());
                }
            }
        }
        // Phase D: delete each worker — one failure never stops the rest.
        for (position, deletion) in deletions {
            if let Err(error) = self.delete_one(&deletion).await {
                outcomes[position].error = Some(error.clone());
            }
            on_result(outcomes[position].clone());
        }
        Ok(TeamDeleteOutcome {
            results: outcomes,
            snapshot: self.snapshot().await,
        })
    }

    /// The decision for one worker's preview: `Ok` deletes it, an `Err`
    /// reports why it must stay — blocked, or work the lead branch lacks
    /// that the user did not confirm discarding.
    fn deletion_decision(deletion: &WorkerDeletion, discard_work: bool) -> Result<(), String> {
        if let Some(reason) = &deletion.blocked {
            return Err(format!("{} cannot be deleted: {reason}", deletion.name));
        }
        if deletion.discards_work() && !discard_work {
            return Err(format!(
                "{} has unmerged or uncommitted work on {}; confirm discarding it to delete the worker",
                deletion.name, deletion.branch
            ));
        }
        Ok(())
    }

    /// Inspect every worker of a deletion batch, streaming each preview
    /// as soon as its checks finish. Batch-wide Git facts are gathered
    /// with two Git runs and the per-worker checkout checks run
    /// `DELETION_CHECK_CONCURRENCY` at a time, so a large batch costs
    /// about the same as a small one. Git state shared with a record
    /// outside the batch is never counted or touched.
    async fn inspect_batch(
        &self,
        batch: &[String],
        on_preview: impl Fn(WorkerDeletion),
    ) -> Result<Vec<WorkerDeletion>, String> {
        let facts = DeletionGitFacts::gather(&self.workspace_root).await?;
        let mut cores = Vec::with_capacity(batch.len());
        for worker_id in batch {
            cores.push(self.inspect_core(worker_id, batch, &facts).await?);
        }
        let mut previews: Vec<Option<WorkerDeletion>> = vec![None; cores.len()];
        let mut pending = Vec::new();
        for (index, core) in cores.iter().enumerate() {
            if core.needs_check {
                pending.push((index, core.checkout.clone(), core.branch.clone()));
            }
        }
        // The Git-heavy half runs as one task per worker, bounded: every
        // worker in a chunk is checked concurrently while the chunks run
        // in order. A preview is finished — and emitted — the moment its
        // check lands.
        for chunk in pending.chunks(DELETION_CHECK_CONCURRENCY) {
            let mut handles = Vec::with_capacity(chunk.len());
            for (index, checkout, branch) in chunk {
                handles.push((
                    *index,
                    tokio::spawn(inspect_checkout(checkout.clone(), branch.clone())),
                ));
            }
            for (index, handle) in handles {
                let check = handle
                    .await
                    .map_err(|error| format!("a checkout check task failed: {error}"))??;
                let preview =
                    self.finish_preview(&cores[index], Some(check), &facts).await?;
                on_preview(preview.clone());
                previews[index] = Some(preview);
            }
        }
        // Workers whose Git state needs no per-worker check (no checkout,
        // unregistered, shared branch, merging) finish immediately.
        for index in 0..cores.len() {
            if previews[index].is_none() {
                let preview = self.finish_preview(&cores[index], None, &facts).await?;
                on_preview(preview.clone());
                previews[index] = Some(preview);
            }
        }
        Ok(previews
            .into_iter()
            .map(|preview| preview.expect("every preview finished"))
            .collect())
    }

    /// The in-memory half of one worker's preview: everything except its
    /// per-worker Git checks. Git state shared with a record outside the
    /// batch is never counted or touched.
    async fn inspect_core(
        &self,
        worker_id: &str,
        batch: &[String],
        facts: &DeletionGitFacts,
    ) -> Result<DeletionCore, String> {
        let record = self.worker(worker_id).await?;
        let shared_branch = self.records.lock().await.iter().any(|other| {
            other.id != record.id
                && other.branch == record.branch
                && !batch.iter().any(|id| id == &other.id)
        });
        // Informative in every state, blocked included: another worker may
        // have landed this record's plan step through its own commits.
        let superseded_by = {
            let records = self.records.lock().await;
            landing_worker_name(&records, &record)
        };
        let mut core = DeletionCore {
            worker_id: record.id.clone(),
            name: record.name.clone(),
            branch: record.branch.clone(),
            live: self.live.lock().await.contains_key(worker_id),
            checkout: worker_checkout(&self.workspace_root, &record.id),
            checkout_exists: false,
            superseded_by,
            shared_branch,
            blocked: None,
            needs_check: false,
        };
        if record.status == WorkerStatus::Merging {
            core.blocked = Some("a merge is in progress".to_string());
            return Ok(core);
        }
        if core.shared_branch || !owns_its_branch(&record) {
            // A shared or malformed legacy record: only the record itself is
            // removed; no Git state another record (or no record) owns is
            // touched.
            return Ok(core);
        }
        core.checkout_exists = std::fs::symlink_metadata(&core.checkout).is_ok();
        // Never probe an unregistered folder with Git: without its own
        // `.git`, Git walks up and answers for the main checkout instead.
        core.needs_check = core.checkout_exists && facts.lists(&core.checkout);
        Ok(core)
    }

    /// Merge a core and its finished checkout check into the worker's
    /// preview. The unmerged count comes from the batch's ahead counts; a
    /// Git without the `%(ahead-behind)` format atom falls back to
    /// counting that one branch with its own `rev-list` call.
    async fn finish_preview(
        &self,
        core: &DeletionCore,
        check: Option<CheckoutCheck>,
        facts: &DeletionGitFacts,
    ) -> Result<WorkerDeletion, String> {
        let (uncommitted, blocked) = match check {
            Some(CheckoutCheck { uncommitted, blocked }) => (uncommitted, core.blocked.clone().or(blocked)),
            None => (false, core.blocked.clone()),
        };
        let unmerged_commits = match &facts.ahead_counts {
            Some(counts) => counts
                .get(&format!("refs/heads/{}", core.branch))
                .copied()
                .unwrap_or(0),
            None => {
                if branch_exists(&self.workspace_root, &core.branch).await {
                    let range = format!("HEAD..refs/heads/{}", core.branch);
                    phase3::git_value(&self.workspace_root, &["rev-list", "--count", &range])
                        .await?
                        .parse()
                        .unwrap_or(0)
                } else {
                    0
                }
            }
        };
        Ok(WorkerDeletion {
            worker_id: core.worker_id.clone(),
            name: core.name.clone(),
            branch: core.branch.clone(),
            live: core.live,
            checkout: core.checkout_exists,
            uncommitted,
            unmerged_commits,
            superseded_by: core.superseded_by.clone(),
            shared_branch: core.shared_branch,
            blocked,
        })
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
            self.remove_checkout(&record).await?;
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

    /// Remove the worker's checkout folder from `.worktrees` and deregister
    /// its worktree. The folder is first renamed into `.worktrees/.trash/`
    /// with one cheap filesystem move — the user never waits for the slow
    /// file-by-file delete of a large checkout; the trash folder is
    /// removed in the background instead. When the rename is refused (on
    /// Windows: something holds the folder or a file in it), the removal
    /// falls back to the foreground retry path, which names the holder.
    async fn remove_checkout(&self, worker: &WorkerRecord) -> Result<(), String> {
        let Some(canonical) = validated_checkout(&self.workspace_root, worker)? else {
            return Ok(());
        };
        match move_checkout_to_trash(&self.workspace_root, &canonical).await {
            Ok(()) => {
                // The renamed checkout no longer sits at its registered
                // path: sweep the trash folder in the background and drop
                // the stale worktree registration, so the branch delete
                // below cannot fail on it.
                schedule_trash_sweep(&self.workspace_root);
                let arguments = ["worktree", "prune"];
                let prune = phase3::git(&self.workspace_root, &arguments).await?;
                if !prune.status.success() {
                    return Err(phase3::git_error(&arguments, &prune));
                }
            }
            Err(_held) => {
                // The move was refused — the folder is held. Fall back to
                // the foreground removal, which waits for holders and
                // reports the one it finds.
                self.remove_checkout_in_front(worker, &canonical).await?;
            }
        }
        Ok(())
    }

    /// The foreground checkout removal, used when the rename into trash
    /// was refused. Runs `git worktree remove --force` for a registered
    /// checkout (discarding was confirmed before) and filesystem removal
    /// for a leftover, then names the holder when the folder refuses to
    /// go.
    async fn remove_checkout_in_front(
        &self,
        worker: &WorkerRecord,
        checkout: &Path,
    ) -> Result<(), String> {
        let mut last_error = String::new();
        for _ in 0..12 {
            if std::fs::symlink_metadata(checkout).is_err() {
                break;
            }
            if !checkout_is_registered(&self.workspace_root, checkout).await? {
                remove_leftover_checkout(&self.workspace_root, worker).await?;
                break;
            }
            match remove_agent_worktree(self.workspace_root.clone(), worker.id.clone()).await {
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
            // Name what still holds the folder, so the user knows what to
            // close. The probe is best effort and never closes anything.
            let probe = checkout.to_path_buf();
            let holders =
                tokio::task::spawn_blocking(move || lock_holders::holders(&[probe]))
                    .await
                    .unwrap_or_default();
            return Err(format!(
                "could not remove {}'s checkout: {last_error}{}",
                worker.name,
                lock_holders::held_by_suffix(&holders)
            ));
        }
        Ok(())
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

/// The merged worker that landed this record's plan step, when the ledger
/// knows one: another implementation record on the same plan step was
/// merged and never reverted, so the step's work reached the lead branch
/// through that worker's commits — typically a replay worker that
/// re-applied this record's conflicted step by hand. Git cannot prove the
/// two commits are the same work; the ledger can. Purely informative: the
/// caller keeps counting this record's own unmerged commits.
fn landing_worker_name(records: &[WorkerRecord], record: &WorkerRecord) -> Option<String> {
    let (plan_id, plan_step) = (record.plan_id.as_deref()?, record.plan_step?);
    records
        .iter()
        .find(|other| {
            other.id != record.id
                && other.kind == "implementation"
                && other.plan_id.as_deref() == Some(plan_id)
                && other.plan_step == Some(plan_step)
                && ledger::merged(other)
        })
        .map(|other| other.name.clone())
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

/// How many workers a deletion preview checks concurrently. Each checked
/// worker runs two short Git calls; a small bound keeps a large batch
/// from fighting the live sessions it lists for the CPU.
const DELETION_CHECK_CONCURRENCY: usize = 4;

/// The in-memory half of one worker's deletion preview: everything the
/// [`WorkerDeletion`] reports before its per-worker Git checks run.
struct DeletionCore {
    /// The worker's id.
    worker_id: String,
    /// The worker's display name.
    name: String,
    /// The worker's `wt/agent-*` branch.
    branch: String,
    /// The worker's session is live; deleting stops it first.
    live: bool,
    /// The worker's expected checkout path.
    checkout: PathBuf,
    /// The checkout still exists at that path.
    checkout_exists: bool,
    /// The merged worker that landed this record's plan step through its
    /// own commits, when the ledger knows one.
    superseded_by: Option<String>,
    /// Another worker record outside the batch uses the same branch.
    shared_branch: bool,
    /// Why the worker cannot be deleted right now, when already known.
    blocked: Option<String>,
    /// The checkout exists and is registered, so its branch and
    /// uncommitted state still need to be read.
    needs_check: bool,
}

/// One worker's per-worker Git checks: whether its checkout holds
/// anything uncommitted, and why it is blocked when its branch moved.
struct CheckoutCheck {
    /// The checkout holds uncommitted changes.
    uncommitted: bool,
    /// Why the checkout blocks the worker's deletion, when it does.
    blocked: Option<String>,
}

/// Batch-wide, read-only Git facts for one deletion preview. One
/// `git worktree list` run and one `git for-each-ref` run serve the whole
/// batch instead of each worker spawning its own Git processes (each Git
/// call costs about 0.1 s on this codebase).
struct DeletionGitFacts {
    /// Every checkout path Git's worktree registry lists for this repo.
    registered_checkouts: Vec<PathBuf>,
    /// `<full refname> -> commits ahead of the lead branch's HEAD` for
    /// every local branch. `None` on a Git without the `%(ahead-behind)`
    /// format atom — the preview then counts each branch with its own
    /// `rev-list --count` call.
    ahead_counts: Option<HashMap<String, u32>>,
}

impl DeletionGitFacts {
    /// Gather the batch's facts with two Git runs.
    async fn gather(root: &Path) -> Result<Self, String> {
        let registered_checkouts = registered_checkouts(root).await?;
        let ahead_counts = match phase3::git_value(
            root,
            &[
                "for-each-ref",
                "--format=%(refname) %(ahead-behind:HEAD)",
                "refs/heads/",
            ],
        )
        .await
        {
            Ok(list) => parse_ahead_counts(&list),
            Err(_) => None,
        };
        Ok(Self {
            registered_checkouts,
            ahead_counts,
        })
    }

    /// Whether Git's worktree registry lists this checkout path.
    fn lists(&self, checkout: &Path) -> bool {
        let expected = normalized_checkout_path(checkout);
        self.registered_checkouts
            .iter()
            .any(|listed| same_checkout_path(listed, &expected))
    }
}

/// Every local branch's commits ahead of HEAD, read from one
/// `git for-each-ref --format='%(refname) %(ahead-behind:HEAD)'` run.
/// `None` when a line fails to parse — the preview falls back to
/// counting that branch with its own `rev-list` call.
fn parse_ahead_counts(list: &str) -> Option<HashMap<String, u32>> {
    let mut counts = HashMap::with_capacity(list.lines().count());
    for line in list.lines() {
        let mut fields = line.split_whitespace();
        let refname = fields.next()?;
        let ahead = fields.next()?;
        counts.insert(refname.to_string(), ahead.parse().ok()?);
    }
    Some(counts)
}

/// Every checkout path Git's worktree registry lists, in one
/// `git worktree list` run instead of one per worker.
async fn registered_checkouts(root: &Path) -> Result<Vec<PathBuf>, String> {
    let arguments = ["worktree", "list", "--porcelain", "-z"];
    let output = phase3::git(root, &arguments).await?;
    if !output.status.success() {
        return Err(phase3::git_error(&arguments, &output));
    }
    worktree_list_paths(&output.stdout)
}

/// One worker's per-worker Git checks. Untracked files are listed without
/// descending into untracked directories (`--untracked-files=normal`): the
/// preview only needs to know that *something* is uncommitted, not
/// enumerate a `node_modules` tree.
async fn inspect_checkout(checkout: PathBuf, branch: String) -> Result<CheckoutCheck, String> {
    let current = checkout_branch(&checkout).await?;
    if current != branch {
        return Ok(CheckoutCheck {
            uncommitted: false,
            blocked: Some(format!("its checkout is on '{current}', not {branch}")),
        });
    }
    let status = phase3::git_value(
        &checkout,
        &["status", "--porcelain", "--untracked-files=normal"],
    )
    .await?;
    Ok(CheckoutCheck {
        uncommitted: !status.is_empty(),
        blocked: None,
    })
}

/// A checkout's current branch, without blocking the async executor (the
/// shared [`current_branch`] helper runs Git synchronously).
async fn checkout_branch(checkout: &Path) -> Result<String, String> {
    let branch = phase3::git_value(checkout, &["rev-parse", "--abbrev-ref", "HEAD"]).await?;
    let branch = branch.trim().to_string();
    if branch.is_empty() || branch == "HEAD" {
        return Err("the checkout is on a detached HEAD".to_string());
    }
    Ok(branch)
}

/// Whether Git's main-checkout worktree registry still lists this exact path.
/// Never probe an unregistered folder with Git: without its own `.git`, Git
/// walks up and answers for the main checkout instead.
async fn checkout_is_registered(root: &Path, checkout: &Path) -> Result<bool, String> {
    let expected = normalized_checkout_path(checkout);
    Ok(registered_checkouts(root)
        .await?
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

/// The checkout path after every ownership guard passes: the worker's own
/// direct-child folder of the canonical `.worktrees`, never a link or
/// junction, never anything else. `None` when it no longer exists.
fn validated_checkout(root: &Path, worker: &WorkerRecord) -> Result<Option<PathBuf>, String> {
    if !owns_its_branch(worker) {
        return Err("refusing to remove a checkout for an invalid worker identity".to_string());
    }
    let checkout = worker_checkout(root, &worker.id);
    let metadata = match std::fs::symlink_metadata(&checkout) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
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
    Ok(Some(canonical))
}

/// Remove an unregistered checkout with filesystem operations only. The
/// canonical path must remain the expected direct child of this repository's
/// `.worktrees`; links and junctions are refused rather than traversed.
async fn remove_leftover_checkout(root: &Path, worker: &WorkerRecord) -> Result<(), String> {
    let Some(canonical) = validated_checkout(root, worker)? else {
        return Ok(());
    };

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
    // Name what still holds the folder, so the user knows what to close. The
    // probe is best effort and never closes anything itself.
    let probe = canonical.clone();
    let holders = tokio::task::spawn_blocking(move || lock_holders::holders(&[probe]))
        .await
        .unwrap_or_default();
    Err(format!(
        "could not remove leftover checkout {}: {last_error}{}",
        canonical.display(),
        lock_holders::held_by_suffix(&holders)
    ))
}

/// The trash folder renamed-away checkouts wait in for background removal:
/// `.worktrees/.trash` — inside the gitignored `.worktrees`, never a
/// tracked path.
fn trash_root(root: &Path) -> PathBuf {
    root.join(".worktrees").join(".trash")
}

/// Every folder waiting in the trash.
fn trash_entries(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(trash_root(root)) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .collect()
}

/// Rename a validated checkout folder into the trash folder with one
/// `rename` (same volume, same parent tree): the delete flow waits for a
/// cheap move instead of the slow file-by-file removal of a large
/// checkout. Fails when something holds the folder — the caller then
/// falls back to the foreground removal path.
async fn move_checkout_to_trash(root: &Path, checkout: &Path) -> Result<(), String> {
    tokio::task::spawn_blocking({
        let root = root.to_path_buf();
        let checkout = checkout.to_path_buf();
        move || {
            let trash = trash_root(&root);
            std::fs::create_dir_all(&trash).map_err(|error| error.to_string())?;
            let name = checkout
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| "the checkout path has no folder name".to_string())?;
            let mut target = trash.join(format!("{name}-{}", phase3::now_ms()));
            let mut attempt = 0;
            while target.exists() {
                attempt += 1;
                target = trash.join(format!("{name}-{}-{attempt}", phase3::now_ms()));
            }
            std::fs::rename(checkout, &target).map_err(|error| error.to_string())
        }
    })
    .await
    .map_err(|error| format!("the checkout move task failed: {error}"))?
}

/// Sweep the trash folder in the background: try to remove every folder in
/// it, retrying for a bounded window — a file can be held for a moment by
/// an indexer or a scanner right after the rename. Anything still held
/// stays for the next sweep; every app start schedules one.
fn schedule_trash_sweep(root: &Path) {
    let root = root.to_path_buf();
    tokio::spawn(async move {
        for _ in 0..12 {
            if trash_entries(&root).is_empty() {
                return;
            }
            sweep_trash(&root).await;
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    });
}

/// Remove every folder under `.worktrees/.trash/` once. Each folder is
/// removed independently — one locked folder never keeps the others.
async fn sweep_trash(root: &Path) {
    for entry in trash_entries(root) {
        let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(entry)).await;
    }
}

/// The durable error prefix a failed `git branch -d` leaves on a merged
/// worker: the merge landed and only the branch cleanup is outstanding.
const BRANCH_CLEANUP_ERROR: &str = "merged, but branch cleanup needs attention";

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

/// Delete a merged worker's branch with Git's safe delete. `Ok(true)` when
/// the branch was deleted. `Ok(false)` when there is nothing for this worker
/// to delete: the branch is not the one its id names, or it is already gone.
/// `git branch -d` refuses a branch that is not fully merged into the
/// checked-out lead branch, or that a worktree still uses; that refusal comes
/// back as `Err` and the branch stays.
async fn delete_merged_branch(root: &Path, worker: &WorkerRecord) -> Result<bool, String> {
    if !owns_its_branch(worker) || !branch_exists(root, &worker.branch).await {
        return Ok(false);
    }
    phase3::git_value(root, &["branch", "-d", &worker.branch])
        .await
        .map(|_| true)
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

/// The plan coordinate a worker may read: its own plan, or for an audit the
/// plan of the worker it audits. The step stays 0-based, like the record.
async fn worker_plan_stamp(
    records: &Arc<Mutex<Vec<WorkerRecord>>>,
    worker_id: &str,
) -> Option<(String, usize)> {
    let records = records.lock().await;
    let record = records.iter().find(|worker| worker.id == worker_id)?;
    let source = if record.kind == "audit" {
        let target = record.audit_target.as_deref()?;
        records.iter().find(|worker| worker.id == target)?
    } else {
        record
    };
    Some((source.plan_id.clone()?, source.plan_step?))
}

/// Handle a worker's host-tool call (`message_parent`, `plan_read`) and return
/// its result.
async fn worker_host_tool_result(
    messages: &Arc<Mutex<Vec<TeamMessage>>>,
    worker_id: &str,
    name: &str,
    args: &Value,
    board: Option<&Arc<PlanBoard>>,
    plan_stamp: Option<&(String, usize)>,
) -> ToolResult {
    if name == "plan_read" {
        let Some(board) = board else {
            return ToolResult::error("the plan board is not available");
        };
        if let Some(capability) = args.get("capability") {
            let Some(capability) = capability.as_str() else {
                return ToolResult::error("plan_read expects 'capability' to be a string");
            };
            return match board.capability_spec(capability) {
                Ok(Some(text)) => ToolResult::success(text),
                Ok(None) => {
                    ToolResult::success(format!("No current spec for '{capability}' yet."))
                }
                Err(error) => ToolResult::error(error),
            };
        }
        let Some((plan_id, step)) = plan_stamp else {
            return ToolResult::error("you have no plan to read");
        };
        return match board.worker_view(plan_id, step + 1) {
            Ok(text) => ToolResult::success(text),
            Err(error) => ToolResult::error(error),
        };
    }
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
    ToolResult::success(
        "message sent to the lead. A reply, if any, arrives later as a new message that starts with a [From …] header; do not assume or invent a reply — continue your task or end your turn.",
    )
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

/// The new highest threshold passed, when a cost notice is due. `threshold`
/// is 0 when notices are off.
fn cost_notice_due(cost: u64, notified: u64, threshold: u64) -> Option<u64> {
    if threshold == 0 || cost < threshold {
        return None;
    }
    let mut level = threshold;
    loop {
        let doubled = level.saturating_mul(2);
        // `saturating_mul` stops growing at the u64 ceiling: stop there too,
        // so a pathological cost can never spin this loop.
        if doubled > cost || doubled <= level {
            break;
        }
        level = doubled;
    }
    (level > notified).then_some(level)
}

/// Forward one worker session's events, update its durable record, and refresh
/// the transcript when the turn ends.
/// How long a steered turn end waits for its follow-up turn before it counts
/// as finished, so a lost steer can never leave a worker Working forever.
const FOLLOW_UP_GRACE: std::time::Duration = if cfg!(test) {
    std::time::Duration::from_millis(200)
} else {
    std::time::Duration::from_secs(60)
};

// Each parameter is one shared worker handle — the pump's wiring stays flat.
#[allow(clippy::too_many_arguments)]
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
    routing_log_path: PathBuf,
    routing: Arc<std::sync::RwLock<RoutingSettings>>,
    signals: broadcast::Sender<BoardSignal>,
    strikes: Arc<Mutex<route_question::Strikes>>,
    steer_pending: Arc<AtomicBool>,
    models: Arc<std::sync::RwLock<Vec<LeadModel>>>,
    tiers: Arc<std::sync::RwLock<mnemo::headless::TiersDto>>,
    live: Arc<Mutex<HashMap<String, LiveWorker>>>,
    fail_next_close: Arc<AtomicBool>,
    board: Option<Arc<PlanBoard>>,
) {
    let mut turn_active = false;
    // A steered turn end is held here until its follow-up turn starts (the
    // held `Finished`) or `FOLLOW_UP_GRACE` passes (the deadline). Both are
    // cleared by `Started`, `Exited`, a final `Error`, and the deadline.
    let mut follow_up_deadline: Option<tokio::time::Instant> = None;
    let mut held_finished: Option<SessionEvent> = None;
    // A `Parked` that settled a held steered turn end: its held `Finished` is
    // re-sent after `save_workers`, so the finished notice reads Completed.
    let mut settled_parked_finish: Option<SessionEvent> = None;
    let mut plan_stamp = worker_plan_stamp(&records, &worker_id).await;
    // The `outcome` row owed when the worker lands in a terminal status this
    // iteration; appended once the record save below has persisted it.
    let mut pending_outcome: Option<DispatchRoutingRow> = None;
    // The tool calls whose results have not landed yet (the nudge strike
    // counter, design §4.6): the session streams arguments as index-keyed
    // fragments, matched to results by the provider's call id.
    let mut call_ids: HashMap<u32, String> = HashMap::new();
    let mut pending_calls: HashMap<String, (String, String)> = HashMap::new();
    let mut previous_signature: Option<String> = None;
    loop {
        // While a steered turn end waits for its follow-up, the receive is
        // bounded by an absolute deadline: the bus is engine-wide, so another
        // session's events must neither satisfy nor extend this wait.
        let received = match follow_up_deadline {
            Some(deadline) => match tokio::time::timeout_at(deadline, receiver.recv()).await {
                Ok(received) => received,
                Err(_) => {
                    // The follow-up never started: settle the steered turn end
                    // as this worker's finished turn, so a lost steer can never
                    // leave it Working forever.
                    let mut found = false;
                    let mut completed = false;
                    {
                        let mut records = records.lock().await;
                        if let Some(worker) = records.iter_mut().find(|worker| {
                            worker.id == worker_id
                                && (worker.generation == 0 || worker.generation == generation)
                        }) {
                            if worker.compacting {
                                // A real compaction is running; its own events
                                // follow, so give it another grace period.
                                follow_up_deadline =
                                    Some(tokio::time::Instant::now() + FOLLOW_UP_GRACE);
                                continue;
                            }
                            found = true;
                            if matches!(
                                worker.status,
                                WorkerStatus::Working
                                    | WorkerStatus::Fixing
                                    | WorkerStatus::Dispatching
                            ) {
                                worker.status = WorkerStatus::Completed;
                                worker.turns = worker.turns.saturating_add(1);
                                completed = true;
                            }
                        }
                    }
                    steer_pending.store(false, Ordering::SeqCst);
                    follow_up_deadline = None;
                    let held = held_finished.take();
                    if !found {
                        // The worker was deleted or replaced: nothing to
                        // settle and no notice to fire.
                        continue;
                    }
                    busy.store(false, Ordering::SeqCst);
                    if completed {
                        let _ = save_workers(&records, &usage, &save_lock, &persistence).await;
                    }
                    if let Some(event) = held {
                        // The held turn end, now that the record reads
                        // Completed: this fires the normal finished notice.
                        let _ = events.send(TeamEvent {
                            worker_id: worker_id.clone(),
                            generation,
                            event,
                        });
                    }
                    continue;
                }
            },
            None => receiver.recv().await,
        };
        let event = match received {
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
        // A follow-up turn started: the steer's turn end is not this worker's
        // finished turn, so the held event and the deadline end here. This
        // runs before the record block, so a paused worker clears the wait too.
        if matches!(event.event, SerializableAgentEvent::Started) {
            steer_pending.store(false, Ordering::SeqCst);
            follow_up_deadline = None;
            held_finished = None;
        }
        if let SerializableAgentEvent::HostToolCall {
            call_id,
            name,
            args,
        } = &event.event
        {
            if name == "plan_read" {
                plan_stamp = worker_plan_stamp(&records, &worker_id).await;
            }
            let result = worker_host_tool_result(
                &messages,
                &worker_id,
                name,
                args,
                board.as_ref(),
                plan_stamp.as_ref(),
            )
            .await;
            send_worker_host_result(&engine, session_id, call_id.clone(), result).await;
            // An audit's report is the message_parent call that delivered
            // it (plan 5779641): mark the record the moment the message is
            // recorded, so the turn's Finished event below can end the
            // session and free the slot it holds.
            if name == "message_parent" {
                let mut records = records.lock().await;
                if let Some(worker) = records.iter_mut().find(|worker| {
                    worker.id == worker_id
                        && (worker.generation == 0 || worker.generation == generation)
                        && worker.kind == "audit"
                }) {
                    worker.report_delivered = true;
                }
            }
        }
        // The nudge strike counter (design §4.6): fingerprint every finished
        // tool call — failures and verbatim repeats of the previous call
        // both count — and try the one nudge after the record block below
        // releases its lock. `repeated` keeps which kind of streak fired.
        let mut strike_fired: Option<bool> = None;
        // The cost alert this event owes the lead (plan 06f1a204): filled
        // inside the record block below, sent once that lock is released.
        let mut cost_notice: Option<(String, String)> = None;
        // The finished tool call this event (if any) carries, captured beside
        // the strike counter so the record block below can fold it into the
        // worker's current failing streak.
        let mut tool_outcome: Option<(String, bool, String)> = None;
        match &event.event {
            SerializableAgentEvent::ToolCallStart { index, id, name } => {
                // A provider may supersede an unexecuted call at the same
                // index; drop the old entry so the buffers stay bounded.
                if let Some(superseded) = call_ids.insert(*index, id.clone()) {
                    pending_calls.remove(&superseded);
                }
                pending_calls.insert(id.clone(), (name.clone(), String::new()));
            }
            SerializableAgentEvent::ToolCallArgDelta { index, fragment } => {
                if let Some(id) = call_ids.get(index) {
                    if let Some((_, args)) = pending_calls.get_mut(id) {
                        args.push_str(fragment);
                    }
                }
            }
            SerializableAgentEvent::ToolResult { tool_call_id, result } => {
                if let Some((name, args)) = pending_calls.remove(tool_call_id) {
                    let parsed = serde_json::from_str::<Value>(&args)
                        .unwrap_or(Value::String(args));
                    let signature = route_question::call_signature(&name, &parsed);
                    let repeated =
                        previous_signature.as_deref() == Some(signature.as_str());
                    previous_signature = Some(signature.clone());
                    let failed = !result.success;
                    if strikes.lock().await.observe(signature, failed) {
                        strike_fired = Some(repeated);
                    }
                    tool_outcome = Some((name.clone(), result.success, result.output.clone()));
                }
            }
            _ => {}
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
        // Whether this event ended a reported audit's turn (plan 5779641):
        // its live session is released below, once the save persists the
        // report and the `Completed` status.
        let mut audit_release = false;
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
                if let Some((tool, success, output)) = &tool_outcome {
                    let had_errors = !worker.tool_errors.is_empty();
                    note_tool_outcome(worker, tool, *success, output);
                    if !*success || had_errors {
                        changed = true;
                    }
                }
                match &event.event {
                    SerializableAgentEvent::Started
                        if !matches!(
                            worker.status,
                            WorkerStatus::Paused
                                | WorkerStatus::Interrupted
                                | WorkerStatus::Stopped
                                | WorkerStatus::Failed
                                | WorkerStatus::Merged
                                | WorkerStatus::Cancelled
                        ) =>
                    {
                        turn_active = true;
                        busy.store(true, Ordering::SeqCst);
                        worker.status = if worker.fixer_finding_id.is_some() {
                            WorkerStatus::Fixing
                        } else {
                            WorkerStatus::Working
                        };
                        changed = true;
                    }
                    SerializableAgentEvent::ModelChanged { model, .. }
                        if event_session(&event) == Some(session_id) =>
                    {
                        // The engine's bus is engine-wide: only OUR
                        // session's model change may re-point this
                        // worker's observed model (and its next usage
                        // row) — a concurrent worker's ModelChanged must
                        // not cross into it.
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
                        // The cost alert (plan 06f1a204): the first notice at
                        // `cost_notice_usd`, then one at each doubling. It
                        // never stops or changes the worker.
                        let threshold = (routing
                            .read()
                            .unwrap_or_else(|e| e.into_inner())
                            .cost_notice_usd
                            .max(0.0)
                            * 1_000_000.0) as u64;
                        let spent = worker.cost_micros.unwrap_or(0);
                        if let Some(level) =
                            cost_notice_due(spent, worker.cost_notified_micros, threshold)
                        {
                            worker.cost_notified_micros = level;
                            let dollars = spent as f64 / 1_000_000.0;
                            let model = worker
                                .model_id
                                .as_deref()
                                .or(worker.model.as_deref())
                                .unwrap_or("-");
                            let tier = worker.tier.as_deref().unwrap_or("-");
                            cost_notice = Some((
                                format!("cost:{worker_id}:{level}"),
                                format!(
                                    "worker_id={worker_id} {} has cost about ${dollars:.2} so far: {} requests on {model}, tier {tier}. It keeps running. If that looks wrong, read team_status, then team_message to steer it or team_stop to cancel it.",
                                    worker.name, worker.requests
                                ),
                            ));
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
                            // A compaction INSIDE a live turn (a mid-run
                            // compact-then-switch, or the new model's own
                            // trigger right after one) must not idle the
                            // worker: the turn continues on the new model, so
                            // only the badge ends. Between turns (turn_active
                            // false) the worker really is idle again.
                            if !turn_active {
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
                    SerializableAgentEvent::Finished { .. }
                        if steer_pending.load(Ordering::SeqCst)
                            && matches!(
                                worker.status,
                                WorkerStatus::Working
                                    | WorkerStatus::Fixing
                                    | WorkerStatus::Dispatching
                            ) =>
                    {
                        // A steer ended this turn at its next break and drives a
                        // follow-up turn (`StopReason::Steer`): the worker has
                        // not finished. Status, busy and the turn count stay as
                        // they are; `Started` clears the wait, and the deadline
                        // settles it as finished if that turn never starts.
                        follow_up_deadline =
                            Some(tokio::time::Instant::now() + FOLLOW_UP_GRACE);
                        held_finished = Some(event.clone());
                        changed = true;
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
                        // An audit that reported ends its own job: its
                        // session is released below so it stops holding a
                        // worker slot. A turn that ended without a report
                        // (failure, interrupt) keeps today's behavior —
                        // the lead can still resume it.
                        audit_release = worker.kind == "audit"
                            && worker.report_delivered
                            && matches!(worker.status, WorkerStatus::Completed);
                        terminal = true;
                    }
                    SerializableAgentEvent::Exited => {
                        turn_active = false;
                        worker.compacting = false;
                        busy.store(false, Ordering::SeqCst);
                        // The session is gone: no follow-up turn is coming.
                        steer_pending.store(false, Ordering::SeqCst);
                        follow_up_deadline = None;
                        held_finished = None;
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
                        // A parked session has nothing queued: when it parks
                        // while a steered turn end is held, no follow-up turn is
                        // coming, so settle that turn end as finished now —
                        // clearer than waiting for the deadline. Rare race: a
                        // steer that lands right after a natural finish can
                        // settle as Completed and send a notice, then `Started`
                        // flips the worker back to Working, and
                        // `current_notices` drops that notice at delivery while
                        // it is still queued.
                        if held_finished.is_some()
                            && matches!(
                                worker.status,
                                WorkerStatus::Working
                                    | WorkerStatus::Fixing
                                    | WorkerStatus::Dispatching
                            )
                        {
                            worker.status = WorkerStatus::Completed;
                            worker.turns = worker.turns.saturating_add(1);
                            steer_pending.store(false, Ordering::SeqCst);
                            follow_up_deadline = None;
                            settled_parked_finish = held_finished.take();
                        } else if !matches!(
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
                    // The deferred compact-then-switch COMPLETED: the worker
                    // summarized and now runs on the new model. Informational
                    // — the turn continues, so status stays Working, busy
                    // stays set, and no error is recorded (bug 2026-11-02:
                    // this used to arrive as an `Error`, which failed a
                    // perfectly healthy worker). This path emits
                    // CompactStarted but no Compacted, so the badge ends here.
                    SerializableAgentEvent::ModelSwapSummarized { .. } => {
                        changed |= worker.compacting;
                        worker.compacting = false;
                    }
                    // Mnemo2 routes at dispatch; a Mnemo1-only session notice
                    // cannot alter worker status, busy, or compaction state.
                    SerializableAgentEvent::RoutingNotice { .. } => {}
                    SerializableAgentEvent::Error { error, retrying } => {
                        if !retrying {
                            turn_active = false;
                            // A final error ends the turn for good: no
                            // follow-up turn is coming.
                            steer_pending.store(false, Ordering::SeqCst);
                            follow_up_deadline = None;
                            held_finished = None;
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
                            // Failed-and-stopped is terminal (design §4.7):
                            // queue the worker's `outcome` row, appended after
                            // the save below. Only the transition into Failed
                            // queues it — a repeated terminal error does not.
                            let first_failure = worker.status != WorkerStatus::Failed;
                            worker.status = WorkerStatus::Failed;
                            worker.error = Some(error.clone());
                            if first_failure {
                                let mode = routing
                                    .read()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .mode;
                                // A failure is resumable, not terminal: the
                                // outcome row only — nudge_ignored waits for
                                // stop, merge, or cleanup.
                                pending_outcome = outcome_row(worker, "failed")
                                    .filter(|_| logs_label(worker, mode));
                            }
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
        if let Some(held) = settled_parked_finish.take() {
            // The held turn end, now that the record reads Completed: this
            // fires the normal finished notice.
            let _ = events.send(TeamEvent {
                worker_id: worker_id.clone(),
                generation,
                event: held,
            });
        }
        if audit_release {
            // The record with the report and the `Completed` status is
            // persisted above; end the live session exactly as
            // `stop_inner` does — but keep the status `Completed`, since
            // the audit finished its job on its own. Then stop pumping:
            // the session is gone and this pump only outlives its turn.
            // The ledger handles move over: the loop breaks right after.
            let _ = release_finished_audit(
                &worker_id,
                generation,
                &live,
                PumpLedger {
                    records: records.clone(),
                    usage: usage.clone(),
                    save_lock: save_lock.clone(),
                    persistence,
                },
                &fail_next_close,
            )
            .await;
            break;
        }
        if let Some(row) = pending_outcome.take() {
            mnemo::model_routing::append_row(&routing_log_path, &row);
        }
        if let Some((key, text)) = cost_notice.take() {
            let _ = signals.send(BoardSignal::Notice { key, text });
        }
        if let Some(repeated) = strike_fired.take() {
            let settings = routing
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if settings.nudges {
                try_strike_nudge(
                    &worker_id,
                    StrikeNudges {
                        records: &records,
                        models: &models,
                        tiers: &tiers,
                        signals: &signals,
                        routing_log_path: &routing_log_path,
                        settings: &settings,
                    },
                    repeated,
                )
                .await;
            }
        }
        let settings = routing
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if settings.nudges && settings.mode == RoutingMode::Auto {
            // A strike fires during a busy turn. Trying on every event makes
            // the first idle event apply the pending nudge without a restart.
            let _ = auto_escalate_idle(
                &worker_id,
                AutoEscalation {
                    records: &records,
                    live: &live,
                    models: &models,
                    tiers: &tiers,
                    signals: &signals,
                    routing_log_path: &routing_log_path,
                    usage: &usage,
                    save_lock: &save_lock,
                    persistence: &persistence,
                },
            )
            .await;
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
                // The tip probe runs outside the ledger lock, so a review recorded
                // while it ran (the lead reviewing the commit it just made) is newer
                // than this observation and cannot be stale against it.
                let tip_read_ms = phase3::now_ms();
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
                        clear_review_for_a_newer_tip(worker, current_tip.as_deref(), tip_read_ms);
                    }
                }
                let _ = save_workers(&records, &usage, &save_lock, &persistence).await;
            });
        }
    }
}

/// Drop a review and its checks when this transcript snapshot saw a worker
/// commit that the review does not cover.
///
/// `tip_read_ms` is the moment before the snapshot's `git rev-parse` ran. A
/// review recorded at or after it describes whatever `HEAD` is now, so a tip
/// this probe read earlier cannot make it stale: the review merely landed
/// while the probe was in flight. Before that guard a slow runner let the lead
/// commit and review inside that window, the refresh dropped both, and the
/// merge failed with "the current worker tip has not been reviewed" (macOS CI
/// run 42071163294). Keeping such a review is safe — [`validate_merge`]
/// re-reads the live tip and every check tip before it merges.
fn clear_review_for_a_newer_tip(
    worker: &mut WorkerRecord,
    current_tip: Option<&str>,
    tip_read_ms: u128,
) {
    let review_is_newer = worker
        .review
        .as_ref()
        .is_some_and(|review| review.reviewed_at_ms >= tip_read_ms);
    if review_is_newer {
        return;
    }
    let tip_moved = current_tip.is_some_and(|tip| {
        worker
            .review
            .as_ref()
            .is_some_and(|review| review.tip != tip)
            || worker.checks.iter().any(|check| check.tip != tip)
    });
    if tip_moved {
        worker.review = None;
        worker.checks.clear();
    }
}

/// Close a worker session for cleanup, honoring the transition tests'
/// injected close failure. Shared by [`TeamRuntime::stop_inner`] and the
/// audit release below so both paths behave identically.
async fn close_worker_session(
    session: &AgentSession,
    fail_next_close: &Arc<AtomicBool>,
) -> Result<(), String> {
    if fail_next_close.swap(false, Ordering::SeqCst) {
        return Err("injected close failure for transition tests".to_string());
    }
    session
        .close_and_wait()
        .await
        .map_err(|error| error.to_string())
}

/// The shared ledger handles a worker pump holds: the record store plus
/// everything [`save_workers`] needs. [`release_finished_audit`] takes
/// them as one group because the pump hands them over on its way out.
struct PumpLedger {
    records: Arc<Mutex<Vec<WorkerRecord>>>,
    usage: Arc<Mutex<ProjectUsage>>,
    save_lock: Arc<Mutex<()>>,
    persistence: TeamPersistence,
}

/// End a finished audit's live session from inside its own pump (plan
/// 5779641): the report was recorded and `Completed` is persisted, so the
/// slot is freed the way [`TeamRuntime::stop_inner`] frees a stopped
/// one — snapshot folded into the record, session closed, record and
/// worktree kept — but the status stays `Completed`, because the audit
/// ended its job on its own rather than being stopped.
///
/// On a failed close the [`LiveWorker`] goes back into `live` with
/// `cleanup_retry_only` set, exactly like a failed stop: a later stop
/// retries the close. The pump calling this breaks its loop right after,
/// so no self-abort is needed.
async fn release_finished_audit(
    worker_id: &str,
    generation: u64,
    live: &Arc<Mutex<HashMap<String, LiveWorker>>>,
    ledger: PumpLedger,
    fail_next_close: &Arc<AtomicBool>,
) -> Result<(), String> {
    let Some(mut live_worker) = live.lock().await.remove(worker_id) else {
        // Not live: a repeated Finished event, or the lead already
        // stopped or deleted the audit. Nothing to release.
        return Ok(());
    };
    let snapshot = live_worker.session.snapshot().await.ok();
    {
        let mut records = ledger.records.lock().await;
        if let Some(worker) = records.iter_mut().find(|worker| {
            worker.id == worker_id
                && (worker.generation == 0 || worker.generation == generation)
        }) {
            if let Some(snapshot) = &snapshot {
                if !snapshot.transcript.messages.is_empty() {
                    worker.transcript = snapshot.transcript.clone();
                    worker.used_tokens = snapshot.used_tokens;
                    worker.max_tokens = snapshot.max_tokens;
                    if worker.model_id.is_none() {
                        worker.model_id = snapshot_model(snapshot);
                    }
                } else if worker.transcript.messages.is_empty() && worker.brief_sent {
                    worker.transcript = Transcript {
                        messages: vec![mnemo::provider::Message::user_text(
                            kickoff_message(worker).to_string(),
                        )],
                    };
                }
            }
        }
    }
    if let Err(error) = close_worker_session(&live_worker.session, fail_next_close).await {
        let message = format!("could not close worker session: {error}");
        live_worker.cleanup_retry_only = true;
        live.lock().await.insert(worker_id.to_string(), live_worker);
        {
            let mut records = ledger.records.lock().await;
            if let Some(worker) = records.iter_mut().find(|worker| {
                worker.id == worker_id
                    && (worker.generation == 0 || worker.generation == generation)
            }) {
                worker.error = Some(message.clone());
            }
        }
        let _ = save_workers(&ledger.records, &ledger.usage, &ledger.save_lock, &ledger.persistence).await;
        return Err(message);
    }
    let _ = save_workers(&ledger.records, &ledger.usage, &ledger.save_lock, &ledger.persistence).await;
    Ok(())
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
    /// The merged worker that landed this record's plan step through its
    /// own commits, when the ledger knows one (a replay worker, for
    /// example): those commits re-apply work that already reached the lead
    /// branch, so the preview says whose instead of presenting them as the
    /// only copy. `None` — serialized as `null` — when no merged worker
    /// shares the step. Purely informative: the commit count and the
    /// discard check are unaffected.
    pub superseded_by: Option<String>,
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

/// One worker's outcome of a completed deletion batch: `error` is `None`
/// exactly when the worker was deleted.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct WorkerDeletionResult {
    /// The worker the result describes.
    pub worker_id: String,
    /// The worker's display name.
    pub name: String,
    /// Why the worker could not be deleted, or `None` when it was deleted.
    pub error: Option<String>,
}

/// `team_delete`'s outcome: one result per requested worker, in request
/// order, plus the team snapshot after every deletion that succeeded.
#[derive(Clone, Debug, Serialize)]
pub struct TeamDeleteOutcome {
    /// One result per requested worker, in request order.
    pub results: Vec<WorkerDeletionResult>,
    /// The team after every deletion that succeeded.
    pub snapshot: TeamSnapshot,
}

/// One streamed deletion-flow update on [`DELETION_EVENT_CHANNEL`]: a
/// worker's finished preview (checking phase) or its deletion result
/// (deleting phase), emitted the moment it is known so the open sheet
/// can fill its rows in.
#[derive(Clone, Debug, Serialize)]
pub struct DeletionProgress {
    /// The phase the update belongs to.
    pub phase: DeletionPhase,
    /// The worker the update describes.
    pub worker_id: String,
    /// The worker's display name.
    pub name: String,
    /// The worker's finished preview; `None` in the deleting phase.
    pub preview: Option<WorkerDeletion>,
    /// Why the worker's deletion failed; `None` when it was deleted.
    pub error: Option<String>,
}

/// Which phase of a deletion flow an update belongs to.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeletionPhase {
    /// `team_delete_preview` streams a worker's finished preview.
    Checking,
    /// `team_delete` streams a worker's deletion result.
    Deleting,
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
pub async fn cleanup_workspace(
    root: &Path,
    state_dir: &Path,
    routing_log_path: &Path,
) -> Result<CleanupReport, String> {
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
        // Abandoned is terminal (design §4.7): the worker's `outcome` row
        // and, when its one nudge was never acted on, the `nudge_ignored`
        // row land beside the cleaned checkout — the outcome row for a
        // worker routing ever applied to or one that was rerouted by hand
        // (a hand reroute is a learning in every mode), the nudge row only
        // for a worker routing ever applied to (cleanup has no settings
        // handle, so the record decides).
        let outcome = outcome_row(worker, "abandoned")
            .filter(|_| logs_label(worker, RoutingMode::Off));
        let nudge_ignored = nudge_ignored_row(worker)
            .filter(|_| logs_routing(worker, RoutingMode::Off));
        let rows = [outcome, nudge_ignored]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        persistence.save(&ledger)?;
        for row in rows {
            mnemo::model_routing::append_row(routing_log_path, &row);
        }
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

const WORKER_INBOX_LIMIT: usize = 200;

fn append_inbox(worker: &mut WorkerRecord, entry: InboxEntry) {
    let overflow = worker
        .inbox
        .len()
        .saturating_add(1)
        .saturating_sub(WORKER_INBOX_LIMIT);
    if overflow > 0 {
        worker.inbox.drain(..overflow);
    }
    worker.inbox.push(entry);
}

fn now_key() -> u128 {
    phase3::now_ms()
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

/// Who leads the team in a `team_status` answer: an external Claude Code
/// session that leads or idled out, or the in-app lead.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
struct TeamLeadStatus {
    /// `external` while a Claude Code session leads or has idled out;
    /// `in_app` when no external session has taken the team.
    kind: &'static str,
    /// Whether an external lead is attached right now.
    attached: bool,
    /// Unix epoch milliseconds it attached, while attached.
    since_ms: Option<u64>,
    /// Unix epoch milliseconds it was released for silence, while idle.
    idle_since_ms: Option<u64>,
}

/// The first eight characters of a commit id: enough to tell tips apart
/// without repeating a full hash in every answer.
fn tip_prefix(tip: &str) -> String {
    tip.chars().take(8).collect()
}

/// One worker as `team_status` reports it: the compact entry the lead reads
/// by default, or the whole record on `detail`. Transcripts are dropped from
/// the text of both forms by [`without_transcripts`].
fn worker_status_view(worker: &WorkerRecord, detail: bool) -> Value {
    if detail {
        return serde_json::to_value(worker).unwrap_or(Value::Null);
    }
    let findings = worker
        .findings
        .iter()
        .filter(|finding| finding.status == "open")
        .map(|finding| {
            json!({
                "id": finding.id,
                "title": finding.title,
                "severity": finding.severity,
                "file": finding.file,
                "line": finding.line,
            })
        })
        .collect::<Vec<_>>();
    let checks = worker
        .checks
        .iter()
        .map(|check| {
            json!({
                "name": check.name,
                "status": check.status,
                "tip": tip_prefix(&check.tip),
            })
        })
        .collect::<Vec<_>>();
    // The observed model id beats the selected spec: it is what actually ran.
    let model = worker.model_id.as_ref().or(worker.model.as_ref());
    let mut view = json!({
        "id": worker.id,
        "name": worker.name,
        "status": worker.status,
        "kind": worker.kind,
        "tier": worker.tier,
        "model": model,
        "plan_id": worker.plan_id,
        "plan_step": worker.plan_step,
        "branch": worker.branch,
        "worktree": worker.worktree,
        "error": worker.error,
        "pending": worker.pending,
        "findings": findings,
        "checks": checks,
        "cost_micros": worker.cost_micros,
        "last_activity_ms": worker.last_activity_ms,
    });
    if let Some(routing) = &worker.routing {
        view["routing"] = json!(routing);
    }
    if let Some(review) = &worker.review {
        view["review"] = json!({
            "summary": review.summary,
            "tip": tip_prefix(&review.tip),
        });
    }
    if let Some(merge) = &worker.merge {
        view["merge"] = json!({ "commit": merge.commit });
    }
    if !worker.tool_errors.is_empty() {
        view["tool_errors"] = json!(worker.tool_errors);
    }
    view
}

/// The `team_status` answer: the worker entries under `result` (an array, as
/// existing readers expect) with the lead's own state next to it.
fn team_status_result(result: Result<Vec<Value>, String>, lead: TeamLeadStatus) -> ToolResult {
    let entries = match result {
        Ok(entries) => entries,
        Err(err) => return team_tool_error(err),
    };
    let data = Value::Array(entries);
    let mut reported = data.clone();
    without_transcripts(&mut reported);
    let output = serde_json::to_string(&json!({
        "action": "team_status",
        "result": reported,
        "lead": lead,
    }))
    .unwrap_or_else(|_| "team_status".to_string());
    ToolResult::success(output).with_data(data)
}

/// The one failure every team tool answers with: the lead reads the message,
/// while `data` keeps the machine-readable kind.
fn team_tool_error(err: String) -> ToolResult {
    ToolResult::error(err.clone()).with_data(json!({
        "error_kind": "domain",
        "message": err,
    }))
}

/// One worker as a team tool answers it: the text carries the compact entry
/// (the answer lands in the lead's own context), while `data` keeps the whole
/// record for internal callers. `team_status` with `detail: true` serves the
/// whole record in the text when the lead asks for it.
fn worker_answer(worker: &WorkerRecord, action: &str) -> ToolResult {
    let mut reported = worker_status_view(worker, false);
    without_transcripts(&mut reported);
    let output = serde_json::to_string(&json!({"action": action, "result": reported}))
        .unwrap_or_else(|_| action.to_string());
    let data = serde_json::to_value(worker).unwrap_or(Value::Null);
    ToolResult::success(output).with_data(data)
}

/// A team tool answer for one worker.
fn worker_result(result: Result<WorkerRecord, String>, action: &str) -> ToolResult {
    match result {
        Ok(worker) => worker_answer(&worker, action),
        Err(err) => team_tool_error(err),
    }
}

/// A dispatch answer: the compact worker entry next to the idempotent-reuse
/// flag. `data` keeps the whole record, as the single-worker answers do.
fn dispatch_result(
    result: Result<(WorkerRecord, bool), String>,
    action: &str,
    mode: RoutingMode,
    fold_request: Option<String>,
    explicit_model: bool,
) -> ToolResult {
    match result {
        Ok((worker, reused)) => {
            let routing = worker.routing.as_ref();
            let requested = if explicit_model {
                None
            } else {
                match fold_request.as_deref() {
                    Some("auto") | None => routing.and_then(|routing| {
                        if routing.routed_by == "laya" {
                            routing.laya_tier.as_deref()
                        } else {
                            routing.lead_tier.as_deref()
                        }
                    }),
                    Some(tier) => Some(tier),
                }
            };
            let ran_tier = routing
                .and_then(|routing| routing.ran_tier.as_deref())
                .or(worker.tier.as_deref());
            let routing_note = match (requested, ran_tier) {
                (Some(requested), Some(ran))
                    if requested != ran && TierLadder::default().get(requested).is_some() =>
                {
                    Some(format!("{requested} has no model; ran on {ran}"))
                }
                _ => None,
            };
            let output = serde_json::to_string(&json!({
                "action": action,
                "result": {
                    "worker": worker_status_view(&worker, false),
                    "reused": reused,
                    "routing_note": routing_note,
                },
            }))
            .unwrap_or_else(|_| action.to_string());
            let mut lines = Vec::new();
            if let Some(line) = dispatch_routing_line(mode, routing) {
                lines.push(line);
            }
            if let Some(note) = &routing_note {
                lines.push(note.clone());
            }
            let output = if lines.is_empty() {
                output
            } else {
                format!("{output}\n{}", lines.join("\n"))
            };
            let data = serde_json::to_value(&worker).unwrap_or(Value::Null);
            ToolResult::success(output).with_data(
                json!({"worker": data, "reused": reused, "routing_note": routing_note}),
            )
        }
        Err(err) => team_tool_error(err),
    }
}

fn dispatch_routing_line(mode: RoutingMode, routing: Option<&WorkerRouting>) -> Option<String> {
    let routing = routing?;
    if mode == RoutingMode::Off {
        return None;
    }
    // The dispatch-time ran tier (audit finding M1): a later manual reroute
    // may have moved the worker to another tier, but what the dispatch ran
    // is what this line reports.
    let ran_tier = routing.ran_tier.as_deref();
    let confidence = routing.confidence;

    if routing.routed_by == "lead_override" {
        let choice = match (ran_tier, routing.lead_tier.as_deref()) {
            (Some(ran), Some(requested)) if ran != requested => {
                format!("you ran {ran} (requested {requested})")
            }
            _ => format!(
                "you chose {}",
                routing
                    .lead_tier
                    .as_deref()
                    .or(ran_tier)
                    .unwrap_or("a custom model")
            ),
        };
        return Some(format!(
            "Laya: {} ({:.2}) — {choice}; logged as an override.",
            routing.laya_tier.as_deref().unwrap_or("unknown"),
            confidence.unwrap_or(0.0),
        ));
    }
    if routing.routed_by == "laya" {
        return Some(format!(
            "Laya: routed to {} ({:.2}).",
            routing.laya_tier.as_deref().unwrap_or("unknown"),
            confidence.unwrap_or(0.0)
        ));
    }
    if mode == RoutingMode::Shadow {
        return Some(match (routing.laya_tier.as_deref(), confidence, routing.cleared) {
            (Some(tier), Some(confidence), true) => {
                format!("Laya would pick {tier} ({confidence:.2}).")
            }
            (_, Some(confidence), _) => format!("Laya: unsure ({confidence:.2})."),
            _ => "Laya: offline.".to_string(),
        });
    }
    if !routing.cleared {
        return Some(match confidence {
            Some(confidence) => format!("Laya: unsure ({confidence:.2})."),
            None => "Laya: offline.".to_string(),
        });
    }
    Some(format!(
        "Laya: {} ({:.2}) — matches your tier.",
        routing.laya_tier.as_deref().unwrap_or("unknown"),
        confidence.unwrap_or(0.0)
    ))
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

/// Require observable acceptance scenarios before a high-risk dispatch.
/// Either the lead's `## Acceptance scenarios` bullets or a complete OpenSpec
/// `Scenario:` block under `## Spec changes` satisfies the requirement.
fn require_acceptance_scenarios(plan_id: &str, doc: &str) -> Result<(), String> {
    let guidance = "add observable scenarios with plan_update add_acceptance_scenarios or spec_deltas, or dispatch with review_risk='standard'";
    let (found, bullets) = acceptance_scenarios(doc);
    if found && bullets > 0 {
        return Ok(());
    }

    let mut in_spec_changes = false;
    let mut in_scenario = false;
    let mut has_when = false;
    let mut has_then = false;
    for line in doc.lines() {
        let trimmed = line.trim();
        let level = trimmed.chars().take_while(|c| *c == '#').count();
        if level > 0 && trimmed[level..].starts_with(' ') {
            if in_scenario && has_when && has_then {
                return Ok(());
            }
            in_scenario = false;
            has_when = false;
            has_then = false;
            let heading = trimmed[level..].trim();
            if level <= 2 {
                in_spec_changes = level == 2 && heading.eq_ignore_ascii_case("spec changes");
            } else if in_spec_changes && heading.starts_with("Scenario:") {
                in_scenario = true;
            }
            continue;
        }
        if !in_spec_changes || !in_scenario {
            continue;
        }
        let Some(text) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        else {
            continue;
        };
        if let Some(value) = text.trim().strip_prefix("**WHEN**") {
            has_when = has_when || !value.trim().is_empty();
        } else if let Some(value) = text.trim().strip_prefix("**THEN**") {
            has_then = has_then || !value.trim().is_empty();
        }
    }
    if in_scenario && has_when && has_then {
        return Ok(());
    }

    if !found {
        return Err(format!(
            "review_risk='high' requires acceptance scenarios, but plan '{plan_id}' does not declare any; {guidance}"
        ));
    }
    Err(format!(
        "review_risk='high' requires acceptance scenarios, but plan '{plan_id}' has no observable bullet (a concrete, verifiable outcome of at least eight characters) or complete OpenSpec scenario; {guidance}"
    ))
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
            "use message_parent with the precise issue and wait for the lead. A reply, if any, ",
            "arrives later as a new message that starts with a [From …] header; do not assume ",
            "or invent a reply — continue your task or end your turn. Never modify ",
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
    // The structured `git` tool. `checkout` takes a branch, never paths
    // (paths go through `restore`), so every checkout is a branch switch.
    let mut rules = vec![deny_rule(
        "git",
        r#"(?i)"subcommand":"(?:push|merge|checkout|delete)""#.to_string(),
    )];
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

/// A bare worker record for tests in sibling modules (`notice_tag`): the
/// shape the team test fixture builds, without a checkout on disk.
#[cfg(test)]
pub(crate) fn test_worker_record(id: &str) -> WorkerRecord {
    tests::worker(std::path::Path::new("."), id)
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! tiers_dto {
        (difficult: $difficult:expr, average: $average:expr, simple: $simple:expr $(,)?) => {{
            let mut dto = mnemo::headless::TiersDto::default();
            dto.tiers[0].models = $simple;
            dto.tiers[1].models = $average;
            dto.tiers[3].models = $difficult;
            dto
        }};
        (simple: $simple:expr, average: $average:expr, difficult: $difficult:expr $(,)?) => {{
            tiers_dto!(difficult: $difficult, average: $average, simple: $simple)
        }};
    }

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
        assert!(worker.inbox.is_empty());
        // Legacy ledgers predate review risk: they decode as standard.
        assert_eq!(worker.review_risk, "standard");
        recover_worker(std::path::Path::new("unused"), &mut persisted.workers[0])
            .await
            .expect("legacy recovery");
        assert!(persisted.workers[0].error.is_none());
        assert_eq!(persisted.workers[0].total_tokens, 120);
        assert_eq!(persisted.workers[0].prompt_tokens, 120);
    }

    #[test]
    fn inbox_entries_use_the_stable_snapshot_shape() {
        let entry = InboxEntry {
            sender: MessageSender::ExternalLead,
            kind: InboxKind::Fix,
            text: "repair this".to_string(),
            at_ms: 7,
        };
        assert_eq!(
            serde_json::to_value(entry).expect("inbox JSON"),
            json!({
                "sender": "external_lead",
                "kind": "fix",
                "text": "repair this",
                "at_ms": 7,
            })
        );
    }

    #[test]
    fn inbox_keeps_only_the_latest_200_entries() {
        let mut record = worker(std::path::Path::new("unused"), "inboxcap");
        for index in 0..(WORKER_INBOX_LIMIT + 5) {
            append_inbox(
                &mut record,
                InboxEntry {
                    sender: MessageSender::User,
                    kind: InboxKind::Prompt,
                    text: format!("message {index}"),
                    at_ms: index as u128,
                },
            );
        }
        assert_eq!(record.inbox.len(), WORKER_INBOX_LIMIT);
        assert_eq!(record.inbox.first().expect("oldest retained").text, "message 5");
        assert_eq!(
            record.inbox.last().expect("newest retained").text,
            "message 204"
        );
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

        let report = cleanup_workspace(
            root,
            state.path(),
            &state.path().join("dispatch_routing.jsonl"),
        )
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
        std::fs::create_dir_all(root.join(".coding/openspec/specs/importer"))
            .expect("OpenSpec dir");
        std::fs::write(
            root.join(".coding/openspec/specs/importer/spec.md"),
            "# Importer\n",
        )
        .expect("OpenSpec spec");
        phase3::require_clean_lead(root, "lead")
            .await
            .expect("OpenSpec specs and archives are lead bookkeeping too");
        std::fs::remove_dir_all(root.join(".coding/openspec")).expect("remove OpenSpec dir");
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

    pub(super) fn worker(root: &std::path::Path, id: &str) -> WorkerRecord {
        let branch = agent_branch(id);
        WorkerRecord {
            id: id.to_string(),
            inbox: Vec::new(),
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
            read_back: false,
            compacting: false,
            total_tokens: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
            cached_tokens: 0,
            requests: 0,
            cost_micros: None,
            cost_notified_micros: 0,
            created_at_ms: 0,
            elapsed_ms: 0,
            turns: 0,
            last_activity_ms: 0,
            checks: Vec::new(),
            review_risk: "standard".to_string(),
            kind: "implementation".to_string(),
            audit_target: None,
            audit_tip: None,
            report_delivered: false,
            findings: Vec::new(),
            review: None,
            review_rounds: 0,
            fix_rounds: 0,
            audits: 0,
            models_used: Vec::new(),
            fixer_finding_id: None,
            merge: None,
            error: None,
            tool_errors: Vec::new(),
            plan_id: None,
            plan_step: None,
            tier: None,
            routing: None,
            reroutes: Vec::new(),
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
        open_test_team_with_config(root, state, workers, config.path()).await
    }

    async fn open_test_team_with_config(
        root: &std::path::Path,
        state: &std::path::Path,
        workers: Vec<WorkerRecord>,
        config: &std::path::Path,
    ) -> TeamRuntime {
        std::fs::write(
            config.join("config.toml"),
            "[general]\ndefault_provider = \"test\"\ndefault_model = \"scripted-a\"\n",
        )
        .expect("config.toml");
        std::fs::write(
            config.join("endpoints.toml"),
            "[[endpoint]]
name = \"test\"
kind = \"local\"
base_url = \"http://127.0.0.1:9/v1/\"
models = [\"scripted-a\", \"scripted-b\"]
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
            config_dir: Some(config.to_path_buf()),
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
            config_dir: config.to_path_buf(),
            classifier: None,
            routing_log_path: Some(state.join("dispatch_routing.jsonl")),
            slots: Some(2),
            models: Vec::new(),
            pricing: Vec::new(),
        })
        .await
        .expect("open the test team")
    }

    async fn spawn_live_worker(team: &TeamRuntime, worker: &WorkerRecord) {
        let session = team
            .spawn_worker_session(WorkerSessionSpec {
                model: None,
                reasoning_effort: None,
                worktree: worker.worktree.clone(),
                resume: None,
                kind: "implementation".to_string(),
                trace_identity: TeamRuntime::worker_trace_identity(worker),
                pinned_task: None,
            })
            .await
            .expect("spawn worker session");
        team.live.lock().await.insert(
            worker.id.clone(),
            LiveWorker {
                session,
                pump: tokio::spawn(std::future::pending::<()>()),
                cleanup_retry_only: false,
                busy: Arc::new(AtomicBool::new(false)),
                strikes: Arc::new(Mutex::new(route_question::Strikes::new(NUDGE_STRIKE_LIMIT))),
                steer_pending: Arc::new(AtomicBool::new(false)),
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
                .spawn_worker_session(WorkerSessionSpec {
                    model: None,
                    reasoning_effort: None,
                    worktree: worker.worktree.clone(),
                    resume: None,
                    kind: "implementation".to_string(),
                    trace_identity: TeamRuntime::worker_trace_identity(worker),
                    pinned_task: None,
                })
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
                    strikes: Arc::new(Mutex::new(route_question::Strikes::new(NUDGE_STRIKE_LIMIT))),
                    steer_pending: Arc::new(AtomicBool::new(false)),
                },
            );
        }
        let ids = vec![dangling.id.clone(), fragment.id.clone()];

        let previews = team.deletion_preview(&ids).await.expect("preview leftovers");
        assert!(previews.iter().all(|preview| preview.blocked.is_none()));
        let outcome = team
            .delete_workers(&ids, false)
            .await
            .expect("delete workers with leftover checkouts");

        assert!(outcome.snapshot.workers.is_empty());
        assert!(team.live.lock().await.is_empty());
        assert!(!dangling.worktree.exists());
        assert!(!fragment.worktree.exists());
    }

    /// One worker whose checkout refuses to delete must not keep the other
    /// workers from being deleted: the flow reports that worker's error and
    /// still listed, and removes every other worker.
    #[tokio::test]
    async fn delete_workers_continues_past_a_failed_worker() {
        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        let first = worker(root.path(), "00000001000000000001");
        let stuck = worker(root.path(), "00000002000000000002");
        let third = worker(root.path(), "00000003000000000003");
        for record in [&first, &stuck, &third] {
            run_git(root.path(), &["branch", &record.branch, "main"]);
            std::fs::create_dir_all(&record.worktree).expect("checkout folder");
            std::fs::write(record.worktree.join("note.txt"), "left\n").expect("checkout file");
        }
        // The stuck worker's checkout path is a file, so its removal is
        // refused — the other two workers must still be deleted.
        std::fs::remove_dir_all(&stuck.worktree).expect("clear the stuck path");
        std::fs::write(&stuck.worktree, "not a directory\n").expect("undeletable checkout");
        let team = open_test_team(
            root.path(),
            state.path(),
            vec![first.clone(), stuck.clone(), third.clone()],
        )
        .await;
        let ids = vec![first.id.clone(), stuck.id.clone(), third.id.clone()];

        let outcome = team
            .delete_workers(&ids, false)
            .await
            .expect("delete every worker that can be deleted");

        // One result per requested worker, in request order.
        assert_eq!(outcome.results.len(), 3, "{:?}", outcome.results);
        assert!(outcome.results[0].error.is_none(), "{:?}", outcome.results);
        let stuck_error = outcome.results[1]
            .error
            .as_ref()
            .expect("the stuck worker is reported with its error");
        assert!(stuck_error.contains("refus"), "unexpected error: {stuck_error}");
        assert!(outcome.results[2].error.is_none(), "{:?}", outcome.results);
        // The first and third workers are gone; the stuck one is still listed.
        assert!(team.worker(&first.id).await.is_err());
        assert!(team.worker(&third.id).await.is_err());
        assert!(team.worker(&stuck.id).await.is_ok());
        assert_eq!(outcome.snapshot.workers.len(), 1);
        assert_eq!(outcome.snapshot.workers[0].id, stuck.id);
        // The deletable checkouts and branches are gone; the stuck
        // worker's branch and checkout stay.
        assert!(!first.worktree.exists());
        assert!(!third.worktree.exists());
        assert!(!branch_exists(root.path(), &first.branch).await);
        assert!(!branch_exists(root.path(), &third.branch).await);
        assert!(stuck.worktree.exists());
        assert!(branch_exists(root.path(), &stuck.branch).await);
    }

    /// A deleted worker's checkout is renamed away with one move and its
    /// worktree registration is pruned, so the delete flow never waits for
    /// the file-by-file removal of a large checkout. The renamed folder is
    /// removed by the trash sweep.
    #[tokio::test]
    async fn a_deleted_checkout_is_renamed_to_trash_and_deregistered() {
        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        let first = worker(root.path(), "00000001000000000001");
        provision_agent_worktree(
            root.path().to_path_buf(),
            first.id.clone(),
            "main".to_string(),
        )
        .await
        .expect("provision the worker checkout");
        let team = open_test_team(root.path(), state.path(), vec![first.clone()]).await;

        let outcome = team
            .delete_workers(std::slice::from_ref(&first.id), true)
            .await
            .expect("delete the worker");

        assert!(outcome.results[0].error.is_none(), "{:?}", outcome.results);
        // The checkout path is gone at once: the folder was renamed away,
        // not deleted file by file.
        assert!(!first.worktree.exists());
        // The worktree registration is pruned and the branch is gone —
        // the branch delete would refuse while a worktree still uses it.
        assert!(
            !checkout_is_registered(root.path(), &first.worktree)
                .await
                .expect("worktree registry"),
            "the renamed checkout is deregistered"
        );
        assert!(!branch_exists(root.path(), &first.branch).await);
        // The renamed folder leaves the trash with the sweep.
        sweep_trash(root.path()).await;
        assert!(trash_entries(root.path()).is_empty());
    }

    /// The trash sweep removes every renamed-away checkout folder, one
    /// locked folder never keeping the others.
    #[tokio::test]
    async fn the_trash_sweep_removes_renamed_checkouts() {
        let root = tempfile::tempdir().expect("workspace");
        init_git(root.path());
        let record = worker(root.path(), "00000001000000000001");
        std::fs::create_dir_all(&record.worktree).expect("checkout folder");
        std::fs::write(record.worktree.join("note.txt"), "left\n").expect("checkout file");
        let canonical = validated_checkout(root.path(), &record)
            .expect("the checkout validates")
            .expect("it exists");

        move_checkout_to_trash(root.path(), &canonical)
            .await
            .expect("rename into trash");

        assert!(!record.worktree.exists());
        assert_eq!(trash_entries(root.path()).len(), 1);
        sweep_trash(root.path()).await;
        assert!(trash_entries(root.path()).is_empty());
    }

    /// The streamed preview emits one update per worker as each finishes,
    /// while the returned previews stay in request order.
    #[tokio::test]
    async fn deletion_preview_streams_each_worker_as_it_checks() {
        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        let first = worker(root.path(), "00000001000000000001");
        let second = worker(root.path(), "00000002000000000002");
        for record in [&first, &second] {
            run_git(root.path(), &["branch", &record.branch, "main"]);
            std::fs::create_dir_all(&record.worktree).expect("checkout folder");
        }
        let team = open_test_team(
            root.path(),
            state.path(),
            vec![first.clone(), second.clone()],
        )
        .await;
        let ids = vec![first.id.clone(), second.id.clone()];

        let streamed: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        let previews = team
            .deletion_preview_streaming(&ids, |preview| {
                streamed.lock().expect("stream lock").push(preview.worker_id.clone());
            })
            .await
            .expect("preview");

        let mut seen = streamed.into_inner().expect("stream lock");
        seen.sort();
        assert_eq!(seen, ids, "every worker streamed exactly once");
        let returned: Vec<String> = previews.iter().map(|preview| preview.worker_id.clone()).collect();
        assert_eq!(returned, ids, "previews return in request order");
    }

    /// On Windows, a held checkout refuses the rename into trash: the
    /// removal falls back to the foreground path, reports the holder, and
    /// still deletes every other worker in the batch.
    #[cfg(windows)]
    #[tokio::test]
    async fn a_held_checkout_falls_back_to_the_foreground_removal_and_reports_it() {
        use std::os::windows::fs::OpenOptionsExt;

        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        let held = worker(root.path(), "00000001000000000001");
        let other = worker(root.path(), "00000002000000000002");
        for record in [&held, &other] {
            provision_agent_worktree(
                root.path().to_path_buf(),
                record.id.clone(),
                "main".to_string(),
            )
            .await
            .expect("provision the worker checkout");
        }
        // One exclusively-open file inside the checkout refuses both the
        // rename and any file-by-file delete.
        let hold = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .share_mode(0)
            .open(held.worktree.join("held.txt"))
            .expect("hold a file inside the checkout");
        let team = open_test_team(
            root.path(),
            state.path(),
            vec![held.clone(), other.clone()],
        )
        .await;

        let outcome = team
            .delete_workers(&[held.id.clone(), other.id.clone()], true)
            .await
            .expect("delete every worker that can be deleted");

        let held_error = outcome.results[0]
            .error
            .as_ref()
            .expect("the held worker is reported with its error");
        assert!(
            held_error.contains("could not remove"),
            "unexpected error: {held_error}"
        );
        assert!(team.worker(&held.id).await.is_ok());
        assert!(held.worktree.exists());
        assert!(outcome.results[1].error.is_none(), "{:?}", outcome.results);
        assert!(team.worker(&other.id).await.is_err());
        assert!(!other.worktree.exists());
        drop(hold);
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
            .message_inner(
                &worker.id,
                "must not run without a pump".into(),
                MessageSender::Lead,
            )
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
        assert!(
            branch_exists(root.path(), &worker.branch).await,
            "a failed close keeps the branch"
        );
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
        assert!(!branch_exists(root.path(), &worker.branch).await);
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
            .spawn_worker_session(WorkerSessionSpec {
                model: None,
                reasoning_effort: None,
                worktree: worker.worktree.clone(),
                resume: None,
                kind: "implementation".to_string(),
                trace_identity: TeamRuntime::worker_trace_identity(&worker),
                pinned_task: None,
            })
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
                strikes: Arc::new(Mutex::new(route_question::Strikes::new(NUDGE_STRIKE_LIMIT))),
                steer_pending: Arc::new(AtomicBool::new(false)),
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

    /// A clean merge deletes the worker's branch with Git's safe delete, and
    /// the merge commit keeps the reviewed tip as its second parent, so the
    /// work is still there without the branch ref.
    #[tokio::test]
    async fn merge_deletes_the_merged_branch_and_keeps_the_work() {
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
            tip: tip.clone(),
            reviewed_at_ms: 1,
            evidence: Some(evidence()),
        });
        let team = open_test_team(root.path(), state.path(), vec![worker.clone()]).await;
        assert!(branch_exists(root.path(), &worker.branch).await);

        let merged = team.merge(&worker.id).await.expect("merge");

        assert_eq!(merged.status, WorkerStatus::Merged);
        assert_eq!(merged.error, None);
        assert!(!worker.worktree.exists());
        assert!(
            !branch_exists(root.path(), &worker.branch).await,
            "the merge deletes the worker branch"
        );
        let commit = merged
            .merge
            .as_ref()
            .expect("the merge record")
            .commit
            .clone();
        assert_eq!(
            phase3::git_value(root.path(), &["rev-parse", &format!("{commit}^2")])
                .await
                .expect("the merge commit's second parent"),
            tip,
            "the merge commit keeps the reviewed tip as its second parent"
        );
        run_git(root.path(), &["merge-base", "--is-ancestor", &tip, "HEAD"]);
    }

    #[tokio::test]
    async fn delete_merged_branch_deletes_a_fully_merged_branch() {
        let root = tempfile::tempdir().expect("workspace");
        init_git(root.path());
        run_git(root.path(), &["checkout", "-b", "wt/mnemo"]);
        let worker = worker(root.path(), "00000001000000000001");
        run_git(root.path(), &["branch", &worker.branch, "main"]);

        assert_eq!(delete_merged_branch(root.path(), &worker).await, Ok(true));
        assert!(!branch_exists(root.path(), &worker.branch).await);
    }

    #[tokio::test]
    async fn delete_merged_branch_refuses_a_branch_that_is_not_fully_merged() {
        let root = tempfile::tempdir().expect("workspace");
        init_git(root.path());
        run_git(root.path(), &["checkout", "-b", "wt/mnemo"]);
        let worker = worker(root.path(), "00000001000000000001");
        run_git(root.path(), &["checkout", "-b", &worker.branch]);
        std::fs::write(root.path().join("unmerged.txt"), "unmerged\n").expect("worker file");
        run_git(root.path(), &["add", "unmerged.txt"]);
        run_git(root.path(), &["commit", "-m", "unmerged work"]);
        run_git(root.path(), &["checkout", "wt/mnemo"]);

        let deleted = delete_merged_branch(root.path(), &worker).await;

        assert!(
            deleted.is_err(),
            "Git refuses to delete an unmerged branch: {deleted:?}"
        );
        assert!(branch_exists(root.path(), &worker.branch).await);
    }

    #[tokio::test]
    async fn delete_merged_branch_skips_a_branch_the_worker_does_not_own() {
        let root = tempfile::tempdir().expect("workspace");
        init_git(root.path());
        run_git(root.path(), &["checkout", "-b", "wt/mnemo"]);
        let mut other = worker(root.path(), "00000001000000000001");
        other.branch = "wt/other".to_string();
        run_git(root.path(), &["branch", "wt/other", "main"]);

        assert_eq!(delete_merged_branch(root.path(), &other).await, Ok(false));
        assert!(branch_exists(root.path(), "wt/other").await);

        let missing = worker(root.path(), "00000002000000000002");
        assert_eq!(delete_merged_branch(root.path(), &missing).await, Ok(false));
    }

    /// The leftover branch of an earlier merged worker — its checkout already
    /// gone — is deleted by the next merge's sweep.
    #[tokio::test]
    async fn a_later_merge_sweeps_an_earlier_merged_workers_leftover_branch() {
        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        run_git(root.path(), &["checkout", "-b", "wt/mnemo"]);
        let early = merged_worker(root.path(), "00000001000000000001");
        run_git(root.path(), &["branch", &early.branch, "main"]);
        let mut worker = worker(root.path(), "00000002000000000002");
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
        let team =
            open_test_team(root.path(), state.path(), vec![early.clone(), worker.clone()]).await;

        let merged = team.merge(&worker.id).await.expect("merge");

        assert_eq!(merged.status, WorkerStatus::Merged);
        assert!(!branch_exists(root.path(), &worker.branch).await);
        assert!(
            !branch_exists(root.path(), &early.branch).await,
            "the sweep deletes the earlier merged worker's leftover branch"
        );
        assert_eq!(team.worker(&early.id).await.expect("early worker").error, None);
    }

    /// The sweep keeps every branch it must not touch: a reverted merge, an
    /// unreleased checkout, and a branch with commits that never reached the
    /// lead branch.
    #[tokio::test]
    async fn the_sweep_keeps_reverted_held_and_unmerged_branches() {
        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        run_git(root.path(), &["checkout", "-b", "wt/mnemo"]);
        let mut reverted = merged_worker(root.path(), "00000003000000000003");
        reverted
            .merge
            .as_mut()
            .expect("the merge record")
            .revert_commit = Some("deadbeef".into());
        run_git(root.path(), &["branch", &reverted.branch, "main"]);
        let held = merged_worker(root.path(), "00000004000000000004");
        std::fs::create_dir_all(&held.worktree).expect("held checkout");
        run_git(root.path(), &["branch", &held.branch, "main"]);
        let unmerged = merged_worker(root.path(), "00000005000000000005");
        run_git(root.path(), &["checkout", "-b", &unmerged.branch]);
        std::fs::write(root.path().join("unmerged.txt"), "unmerged\n").expect("unmerged file");
        run_git(root.path(), &["add", "unmerged.txt"]);
        run_git(root.path(), &["commit", "-m", "unmerged work"]);
        run_git(root.path(), &["checkout", "wt/mnemo"]);
        let mut worker = worker(root.path(), "00000002000000000002");
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
        let team = open_test_team(
            root.path(),
            state.path(),
            vec![reverted.clone(), held.clone(), unmerged.clone(), worker.clone()],
        )
        .await;

        let merged = team.merge(&worker.id).await.expect("merge");

        assert_eq!(merged.status, WorkerStatus::Merged);
        assert!(!branch_exists(root.path(), &worker.branch).await);
        assert!(
            branch_exists(root.path(), &reverted.branch).await,
            "a reverted merge keeps its branch"
        );
        assert!(
            branch_exists(root.path(), &held.branch).await,
            "a checkout that is still there keeps its branch"
        );
        assert!(
            branch_exists(root.path(), &unmerged.branch).await,
            "commits outside the lead branch keep the branch"
        );
    }

    #[tokio::test]
    async fn audit_merge_is_refused_even_while_the_audit_is_busy() {
        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        run_git(root.path(), &["checkout", "-b", "wt/mnemo"]);
        let mut audit = worker(root.path(), "00000001000000000002");
        audit.kind = "audit".to_string();
        audit.audit_target = Some("00000001000000000001".to_string());
        audit.status = WorkerStatus::Working;
        provision_agent_worktree(
            root.path().to_path_buf(),
            audit.id.clone(),
            "wt/mnemo".to_string(),
        )
        .await
        .expect("provision the audit checkout");
        let team = open_test_team(root.path(), state.path(), vec![audit.clone()]).await;
        spawn_live_worker(&team, &audit).await;
        team.live
            .lock()
            .await
            .get(&audit.id)
            .expect("the audit is live")
            .busy
            .store(true, Ordering::SeqCst);
        assert!(team.worker_is_busy(&audit.id).await, "the audit is running");
        let before = team.worker(&audit.id).await.expect("audit record").status;

        let refused = team
            .merge(&audit.id)
            .await
            .expect_err("an audit worker never merges, busy or not");
        assert!(refused.contains("never merged"), "{refused}");
        assert_eq!(
            team.worker(&audit.id).await.expect("audit record").status,
            before
        );
        assert!(team.live.lock().await.contains_key(&audit.id));
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

        let report = cleanup_workspace(
            root,
            state.path(),
            &state.path().join("dispatch_routing.jsonl"),
        )
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

    #[test]
    fn high_risk_dispatch_accepts_complete_openspec_delta_scenarios() {
        require_acceptance_scenarios(
            "planid01",
            "# Plan\n\n## Spec changes\n\n### importer\n\n#### ADDED Requirements\n\n##### Requirement: Rows survive\nThe system SHALL retain rows.\n\n###### Scenario: Contract exercised\n- **WHEN** rows are imported\n- **THEN** every row remains visible\n",
        )
        .expect("a complete OpenSpec scenario satisfies high-risk dispatch");
    }

    #[test]
    fn high_risk_dispatch_refuses_spec_changes_without_complete_scenarios() {
        require_acceptance_scenarios(
            "planid01",
            "# Plan\n\n## Spec changes\n\n### importer\n\n#### REMOVED Requirements\n\n##### Requirement: Old behavior\nNo longer needed.\n",
        )
        .expect_err("a Spec changes section without scenarios must not pass");

        require_acceptance_scenarios(
            "planid01",
            "# Plan\n\n## Spec changes\n\n### importer\n\n#### MODIFIED Requirements\n\n##### Requirement: Rows survive\nThe system SHALL retain rows.\n\n###### Scenario: Incomplete\n- **WHEN** rows are imported\n",
        )
        .expect_err("a scenario without THEN must not pass");
    }

    /// The parsed answer of one in-app lead `team_status` call.
    async fn team_status_answer(team: &TeamRuntime, args: Value) -> Value {
        let result = team.handle_lead_host_tool("team_status", args).await;
        assert!(result.success, "{}", result.output);
        serde_json::from_str(&result.output).expect("team_status answers with JSON")
    }

    fn reviewed_worker(root: &std::path::Path, id: &str) -> WorkerRecord {
        let mut worker = worker(root, id);
        worker.brief = "the whole brief".to_string();
        worker.opening_prompt = Some("the whole opening prompt".to_string());
        worker.model = Some("test/scripted-a".to_string());
        worker.model_id = Some("scripted-a".to_string());
        worker.plan_id = Some("plan0001".to_string());
        worker.plan_step = Some(2);
        worker.tier = Some("routine".to_string());
        worker.status = WorkerStatus::InReview;
        worker.error = Some("a recorded error".to_string());
        worker.cost_micros = Some(1234);
        worker.last_activity_ms = 42;
        worker.findings = vec![
            Finding {
                id: "finding1".to_string(),
                severity: "high".to_string(),
                title: "the open one".to_string(),
                detail: "the whole finding detail".to_string(),
                file: "src/a.rs".to_string(),
                line: 7,
                status: "open".to_string(),
                fix_commit: None,
                evidence: None,
                justification: None,
            },
            Finding {
                id: "finding2".to_string(),
                severity: "low".to_string(),
                title: "the fixed one".to_string(),
                detail: "the closed finding detail".to_string(),
                file: "src/b.rs".to_string(),
                line: 9,
                status: "fixed".to_string(),
                fix_commit: Some("1234567890abcdef".to_string()),
                evidence: None,
                justification: None,
            },
        ];
        worker.checks = vec![WorkerCheck {
            name: "cargo test".to_string(),
            status: "passed".to_string(),
            detail: Some("the whole check output".to_string()),
            tip: "0123456789abcdef".to_string(),
        }];
        worker.review = Some(ReviewRecord {
            summary: "looks right".to_string(),
            tip: "0123456789abcdef".to_string(),
            reviewed_at_ms: 5,
            evidence: Some(ReviewEvidence {
                unanchored_pass: true,
                intent: "the whole intent".to_string(),
                execution: "the whole execution".to_string(),
                integration: "the whole integration".to_string(),
                test_quality: "the whole test quality".to_string(),
                residual_risk: "the whole residual risk".to_string(),
                independent_reviewer_id: None,
                audit_adjudication: None,
            }),
        });
        worker
    }

    fn merged_worker(root: &std::path::Path, id: &str) -> WorkerRecord {
        let mut worker = worker(root, id);
        worker.status = WorkerStatus::Merged;
        worker.merge = Some(MergeRecord {
            commit: "abcdef0123456789".to_string(),
            worker_tip: "0123456789abcdef".to_string(),
            branch: agent_branch(id),
            target: "wt/agent-lead".to_string(),
            checks: vec!["cargo test".to_string()],
            review_summary: "looks right".to_string(),
            review_risk: Some("standard".to_string()),
            review_evidence: None,
            files: 1,
            additions: 2,
            deletions: 3,
            worker_model: Some("scripted-a".to_string()),
            worker_tokens: 10,
            cost_micros: Some(5),
            merged_at_ms: 7,
            revert_commit: None,
        });
        worker
    }

    /// `team_status` answers compactly by default — the answer lands in the
    /// lead's own context — and with the whole record on `detail`.
    #[tokio::test]
    async fn team_status_is_compact_by_default_and_full_on_detail() {
        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        let reviewed = reviewed_worker(root.path(), "00000001000000000001");
        let merged = merged_worker(root.path(), "00000001000000000002");
        let team = open_test_team(root.path(), state.path(), vec![reviewed, merged]).await;

        let answer = team_status_answer(&team, json!({})).await;
        assert_eq!(answer["action"], "team_status");
        let entries = answer["result"].as_array().expect("a worker array");
        assert_eq!(entries.len(), 2);
        let entry = &entries[0];
        assert_eq!(entry["id"], "00000001000000000001");
        assert_eq!(entry["status"], "in_review");
        assert_eq!(entry["kind"], "implementation");
        assert_eq!(entry["tier"], "routine");
        assert_eq!(entry["model"], "scripted-a");
        assert_eq!(entry["plan_id"], "plan0001");
        assert_eq!(entry["plan_step"], 2);
        assert_eq!(entry["branch"], agent_branch("00000001000000000001"));
        assert_eq!(entry["error"], "a recorded error");
        assert_eq!(entry["cost_micros"], 1234);
        assert_eq!(entry["last_activity_ms"], 42);
        assert!(entry["pending"].is_null(), "{entry}");
        for oversized in ["brief", "opening_prompt", "inbox", "transcript"] {
            assert!(
                entry.get(oversized).is_none(),
                "the compact entry carries {oversized}: {entry}"
            );
        }
        // Only the open finding, and only its identity.
        let findings = entry["findings"].as_array().expect("findings");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0]["id"], "finding1");
        assert_eq!(findings[0]["severity"], "high");
        assert_eq!(findings[0]["title"], "the open one");
        assert_eq!(findings[0]["file"], "src/a.rs");
        assert_eq!(findings[0]["line"], 7);
        assert!(
            findings[0].get("detail").is_none(),
            "the compact finding carries its detail: {}",
            findings[0]
        );
        // Checks keep their verdict and tip prefix, not the output.
        let checks = entry["checks"].as_array().expect("checks");
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0]["name"], "cargo test");
        assert_eq!(checks[0]["status"], "passed");
        assert_eq!(checks[0]["tip"], "01234567");
        assert!(
            checks[0].get("detail").is_none(),
            "the compact check carries its output: {}",
            checks[0]
        );
        // The review keeps its summary and tip, not the evidence.
        assert_eq!(entry["review"]["summary"], "looks right");
        assert_eq!(entry["review"]["tip"], "01234567");
        assert!(
            entry["review"].get("evidence").is_none(),
            "the compact review carries its evidence: {}",
            entry["review"]
        );
        // A merged worker names its merge commit and carries no review block.
        let merged_entry = &entries[1];
        assert_eq!(merged_entry["status"], "merged");
        assert_eq!(merged_entry["merge"]["commit"], "abcdef0123456789");
        assert!(merged_entry.get("review").is_none(), "{merged_entry}");

        // `detail` returns the record as before, transcripts aside.
        let full = team_status_answer(&team, json!({"detail": true})).await;
        let entries = full["result"].as_array().expect("a worker array");
        assert_eq!(entries.len(), 2);
        let entry = &entries[0];
        assert_eq!(entry["brief"], "the whole brief");
        assert_eq!(entry["opening_prompt"], "the whole opening prompt");
        assert_eq!(entry["checks"][0]["detail"], "the whole check output");
        assert_eq!(entry["findings"].as_array().map(Vec::len), Some(2));
        assert_eq!(
            entry["review"]["evidence"]["residual_risk"],
            "the whole residual risk"
        );
        assert!(
            entry.get("transcript").is_none(),
            "transcripts stay out: {entry}"
        );

        // The worker_id filter works in both forms; an unknown id still fails.
        for detail in [false, true] {
            let answer = team_status_answer(
                &team,
                json!({"worker_id": "00000001000000000002", "detail": detail}),
            )
            .await;
            let entries = answer["result"].as_array().expect("a worker array");
            assert_eq!(entries.len(), 1, "detail {detail}: {answer}");
            assert_eq!(entries[0]["id"], "00000001000000000002");
        }
        let missing = team
            .handle_lead_host_tool("team_status", json!({"worker_id": "nope"}))
            .await;
        assert!(!missing.success, "an unknown worker id fails: {}", missing.output);
        team.shutdown().await;
    }

    /// Every team tool answers with the same compact worker entry the lead
    /// already reads from `team_status`; the whole record stays in `data`.
    #[tokio::test]
    async fn team_tools_answer_with_the_compact_worker_entry() {
        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        // A cleaned-up worker keeps its branch, so a check resolves the tip
        // without a live checkout.
        let id = "00000001000000000001";
        let mut worker = reviewed_worker(root.path(), id);
        worker.status = WorkerStatus::Cancelled;
        worker.worktree = std::path::PathBuf::new();
        worker.checks.clear();
        run_git(root.path(), &["branch", &agent_branch(id)]);
        let team = open_test_team(root.path(), state.path(), vec![worker]).await;

        let answer = team
            .handle_lead_host_tool(
                "team_check",
                json!({
                    "worker_id": id,
                    "name": "cargo test --workspace",
                    "status": "passed",
                    "detail": "the whole check output again",
                }),
            )
            .await;
        assert!(answer.success, "{}", answer.output);
        let receipt: Value = serde_json::from_str(&answer.output).expect("JSON receipt");
        assert_eq!(receipt["action"], "team_check");
        let entry = &receipt["result"];
        assert_eq!(entry["id"], id);
        assert_eq!(entry["status"], "cancelled");
        // Status, checks and findings stay; the oversized fields do not.
        let checks = entry["checks"].as_array().expect("checks");
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0]["name"], "cargo test --workspace");
        assert_eq!(checks[0]["status"], "passed");
        assert_eq!(checks[0]["tip"].as_str().map(str::len), Some(8), "{entry}");
        assert!(
            checks[0].get("detail").is_none(),
            "the compact check carries its output: {}",
            checks[0]
        );
        let findings = entry["findings"].as_array().expect("findings");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0]["id"], "finding1");
        for oversized in ["brief", "opening_prompt", "inbox", "transcript"] {
            assert!(
                entry.get(oversized).is_none(),
                "the team tool answer carries {oversized}: {entry}"
            );
        }
        // The whole record stays in `data` for callers that need it.
        let data = answer.data.as_ref().expect("structured data");
        assert_eq!(data["brief"], "the whole brief");
        assert_eq!(data["opening_prompt"], "the whole opening prompt");

        team.shutdown().await;
    }

    /// A release forgets every helper that led, so a helper that closes after
    /// one cannot keep a stale claim alive, and a detached lead remembers no
    /// new helper until it attaches again.
    #[tokio::test]
    async fn a_release_forgets_every_external_bridge() {
        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        let team = open_test_team(root.path(), state.path(), Vec::new()).await;

        // Two helpers led; one closing must leave the other in charge.
        assert!(team.attach_external_lead(), "the first attach takes the team");
        team.note_external_bridge("a");
        team.note_external_bridge("b");
        assert_eq!(team.forget_external_bridge("a"), 1, "b still leads");

        // A release forgets every helper, and none is noted while detached.
        assert!(team.detach_external_lead());
        team.note_external_bridge("c");
        assert_eq!(team.forget_external_bridge("b"), 0, "the release forgot b");
        assert_eq!(team.forget_external_bridge("c"), 0, "a detached lead notes no helper");

        // The next attach starts with no remembered helpers.
        assert!(team.attach_external_lead(), "it can lead again");
        assert_eq!(team.forget_external_bridge("a"), 0, "the release had cleared the set");
        team.shutdown().await;
    }

    /// `team_status` says who leads: the in-app lead, an attached Claude Code
    /// session, or one that was released for silence.
    #[tokio::test]
    async fn team_status_names_the_lead_and_whether_it_is_attached() {
        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        let team = open_test_team(root.path(), state.path(), Vec::new()).await;

        // No external lead: Mnemo2's own lead leads.
        let answer = team_status_answer(&team, json!({})).await;
        assert_eq!(
            answer["lead"],
            json!({"kind": "in_app", "attached": false, "since_ms": null, "idle_since_ms": null}),
            "{answer}"
        );

        // An attached Claude Code session leads.
        assert!(team.attach_external_lead(), "the first attach takes the team");
        let answer = team_status_answer(&team, json!({})).await;
        assert_eq!(answer["lead"]["kind"], "external", "{answer}");
        assert_eq!(answer["lead"]["attached"], true, "{answer}");
        assert!(answer["lead"]["since_ms"].as_u64().is_some(), "{answer}");
        assert!(answer["lead"]["idle_since_ms"].is_null(), "{answer}");

        // Released for silence: still the external session, no longer attached.
        assert!(team.detach_external_lead());
        team.mark_external_lead_idle(4242);
        let answer = team_status_answer(&team, json!({})).await;
        assert_eq!(answer["lead"]["kind"], "external", "{answer}");
        assert_eq!(answer["lead"]["attached"], false, "{answer}");
        assert!(answer["lead"]["since_ms"].is_null(), "{answer}");
        assert_eq!(answer["lead"]["idle_since_ms"], 4242, "{answer}");
        team.shutdown().await;
    }

    /// The Plans tab reads a plan's markdown through the team. An id no board
    /// row names is refused before any plan file is touched, so a stray file
    /// in `.coding/plans/` can never leak out through the command.
    #[tokio::test]
    async fn plan_document_returns_the_markdown_and_refuses_unknown_ids() {
        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        let plans_dir = root.path().join(".coding/plans");
        std::fs::create_dir_all(&plans_dir).expect("plans dir");
        let board = PlanBoard::new(&plans_dir, root.path().join(".coding/reviews"));
        let plan = board
            .create(mnemo::app::plan_board::PlanDraft {
                title: "Plans tab".into(),
                goal: "Show the whole plan".into(),
                context: "verified: the card hides the context; src-tauri/src/mnemo2/ipc.rs and mnemo2/src/components/PlanTab.svelte carry the fix; verify with cargo test.".into(),
                steps: vec!["**Expose** the document in src-tauri/src/mnemo2/ipc.rs".into()],

                acceptance: vec!["Details shows the context".into()],
                ..Default::default()
            })
            .expect("create the plan");
        let stray = "deadbeef";
        std::fs::write(
            plans_dir.join(format!("{stray}.md")),
            "# Plan: stray\n\n## Context\n\nunreachable\n",
        )
        .expect("stray plan file");
        let team = open_test_team(root.path(), state.path(), Vec::new()).await;
        let team = Arc::new(team);
        team.attach_board(Arc::new(board));

        let document = team.plan_document(&plan.id).expect("the plan document");
        assert!(document.contains("## Context"), "{document}");
        assert!(document.contains("verified: the card hides the context"), "{document}");

        let refused = team
            .plan_document(stray)
            .expect_err("an id no board row names is refused");
        assert!(refused.contains(stray), "{refused}");
        team.shutdown().await;
    }

    /// The lead's `team_dispatch` schema offers a read-back first turn for
    /// large or subtle specs, as a boolean opt-in.
    #[tokio::test]
    async fn team_dispatch_schema_offers_read_back() {
        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        let team = open_test_team(root.path(), state.path(), Vec::new()).await;

        let dispatch = team
            .lead_host_tools()
            .into_iter()
            .find(|tool| tool.name == "team_dispatch")
            .expect("team_dispatch");
        let read_back = &dispatch.parameters["properties"]["read_back"];
        assert_eq!(read_back["type"], json!("boolean"), "{read_back}");
        let description = read_back["description"]
            .as_str()
            .expect("a read_back description");
        assert!(description.contains("No questions."), "{description}");
        assert!(description.contains("waits for the lead's reply"), "{description}");
        team.shutdown().await;
    }

    #[tokio::test]
    async fn team_dispatch_refuses_unmet_openspec_apply_requirements() {
        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        let team = open_test_team(root.path(), state.path(), Vec::new()).await;
        let openspec = root.path().join(".coding/openspec");
        let templates = openspec.join("schemas/custom/templates");
        std::fs::create_dir_all(&templates).expect("schema templates");
        for id in ["proposal", "research", "tasks"] {
            std::fs::write(templates.join(format!("{id}.md")), format!("# {id}\n"))
                .expect("template");
        }
        std::fs::write(
            openspec.join("schemas/custom/schema.yaml"),
            "name: custom\nversion: 1\nartifacts:\n  - {id: proposal, generates: proposal.md, template: proposal.md}\n  - {id: research, generates: research.md, template: research.md, instruction: Record evidence.}\n  - {id: tasks, generates: tasks.md, template: tasks.md}\napply: {requires: [research], tracks: tasks.md}\n",
        )
        .expect("schema.yaml");
        std::fs::write(
            openspec.join("config.yaml"),
            "schema: custom\nrules:\n  research: Cite the source.\n",
        )
        .expect("config.yaml");
        let board = PlanBoard::new(
            root.path().join(".coding/plans"),
            root.path().join(".coding/reviews"),
        );
        let plan = board
            .create(mnemo::app::plan_board::PlanDraft {
                title: "Needs research".to_string(),
                goal: "Implement safely".to_string(),
                context: "Symptom: dispatch lacks artifact gates; src-tauri/src/mnemo2/team.rs validates the plan; verify with cargo test.".to_string(),
                steps: vec!["Implement in src-tauri/src/mnemo2/team.rs and run cargo test".to_string()],
                ..Default::default()
            })
            .expect("creation is allowed before apply requirements are met");
        let team = Arc::new(team);
        team.attach_board(Arc::new(board));

        let error = team
            .dispatch_request(DispatchRequest {
                brief: "Implement the plan".to_string(),
                tier: None,
                dispatch_routing: false,
                source: DispatchSource::Ipc,
                model: None,
                reasoning_effort: None,
                worker_id: None,
                plan: Some(PlanRef {
                    plan_id: plan.id.clone(),
                    step: 0,
                }),
                review_risk: None,
                read_back: false,
            })
            .await
            .expect_err("apply.requires must refuse dispatch");
        assert!(error.contains("cannot be dispatched"), "{error}");
        assert!(error.contains("artifact 'research'"), "{error}");
        assert!(error.contains("Cite the source."), "{error}");
        assert!(team.records.lock().await.is_empty());
        team.shutdown().await;
    }

    /// Idempotent re-dispatch compares `read_back` like the brief: the same
    /// worker id with a different value is refused rather than silently
    /// re-pinned.
    #[tokio::test]
    async fn redispatch_with_a_different_read_back_is_refused() {
        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        let mut existing = worker(root.path(), "00000001000000000001");
        existing.brief = "BRIEF-91x".to_string();
        let team = open_test_team(root.path(), state.path(), vec![existing.clone()]).await;

        let (reused, fresh) = team
            .dispatch_request(DispatchRequest {
                brief: "BRIEF-91x".to_string(),
                tier: None,
                dispatch_routing: false,
            source: DispatchSource::Ipc,
                model: None,
                reasoning_effort: None,
                worker_id: Some(existing.id.clone()),
                plan: None,
                review_risk: None,
                read_back: false,
            })
            .await
            .expect("the identical re-dispatch reuses the worker");
        assert!(fresh, "the identical re-dispatch is a reuse");
        assert_eq!(reused.id, existing.id);

        let refused = team
            .dispatch_request(DispatchRequest {
                brief: "BRIEF-91x".to_string(),
                tier: None,
                dispatch_routing: false,
            source: DispatchSource::Ipc,
                model: None,
                reasoning_effort: None,
                worker_id: Some(existing.id.clone()),
                plan: None,
                review_risk: None,
                read_back: true,
            })
            .await
            .expect_err("a different read_back is refused");
        assert!(refused.contains("read_back"), "{refused}");
        team.shutdown().await;
    }

    /// An open high finding for the round counters.
    fn open_finding(id: &str) -> Finding {
        Finding {
            id: id.to_string(),
            severity: "high".to_string(),
            title: "the unchecked path".to_string(),
            detail: "the whole finding detail".to_string(),
            file: "src/a.rs".to_string(),
            line: 7,
            status: "open".to_string(),
            fix_commit: None,
            evidence: None,
            justification: None,
        }
    }

    struct ReviewFixture {
        _root: tempfile::TempDir,
        state: tempfile::TempDir,
        _config: tempfile::TempDir,
        team: TeamRuntime,
        record: WorkerRecord,
    }

    impl ReviewFixture {
        fn rows(&self) -> Vec<DispatchRoutingRow> {
            std::fs::read_to_string(self.state.path().join("dispatch_routing.jsonl"))
                .unwrap_or_default()
                .lines()
                .map(|line| serde_json::from_str(line).expect("routing row"))
                .collect()
        }
    }

    async fn review_fixture(
        mode: crate::mnemo2::dispatch_routing::RoutingMode,
        id: &str,
    ) -> ReviewFixture {
        use crate::mnemo2::dispatch_routing::{save_routing_settings, RoutingSettings};
        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        let config = tempfile::tempdir().expect("config");
        init_git(root.path());
        save_routing_settings(
            config.path(),
            &RoutingSettings {
                mode,
                ..RoutingSettings::default()
            },
        )
        .expect("routing settings");
        let mut record = worker(root.path(), id);
        record.model = Some("test/scripted-difficult".into());
        record.tier = Some("high".into());
        record.routing = Some(WorkerRouting {
            laya_tier: Some("high".into()),
            confidence: Some(0.8),
            cleared: true,
            lead_tier: Some("high".into()),
            routed_by: "lead".into(),
            ran_tier: Some("high".into()),
            nudged: None,
        });
        provision_agent_worktree(
            root.path().to_path_buf(),
            record.id.clone(),
            "main".to_string(),
        )
        .await
        .expect("worker checkout");
        let team = open_test_team_with_config(
            root.path(),
            state.path(),
            vec![record.clone()],
            config.path(),
        )
        .await;
        let mut tiers = tiers_dto! {
            difficult: vec!["test/scripted-difficult".into()],
            average: vec!["test/scripted-average".into()],
            simple: vec!["test/scripted-simple".into()],
        };
        tiers.tiers[4].models = vec!["test/scripted-escalate".into()];
        team.set_catalog(
            vec![
                lead_model("scripted-difficult"),
                lead_model("scripted-average"),
                lead_model("scripted-simple"),
                lead_model("scripted-escalate"),
            ],
            tiers,
            Vec::new(),
        );
        ReviewFixture {
            _root: root,
            state,
            _config: config,
            team,
            record,
        }
    }

    /// One configured model in the team's catalog, so `change_model` accepts
    /// the spec the test endpoint also serves.
    fn lead_model(model: &str) -> LeadModel {
        LeadModel {
            endpoint: "test".to_string(),
            model: model.to_string(),
            spec: format!("test/{model}"),
            is_default: model == "scripted-a",
            reasoning_efforts: Vec::new(),
            default_effort: None,
        }
    }

    /// Every review of a worker is a review round, and a finding routed back
    /// to it is a fix round (plan 40eef714 step 4).
    #[tokio::test]
    async fn review_and_fix_rounds_are_counted() {
        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        let record = worker(root.path(), "rounds000001");
        provision_agent_worktree(
            root.path().to_path_buf(),
            record.id.clone(),
            "main".to_string(),
        )
        .await
        .expect("provision the worker checkout");
        let team = open_test_team(root.path(), state.path(), vec![record.clone()]).await;

        let first = team
            .review(
                &record.id,
                "first pass".into(),
                vec![open_finding("finding1")],
                evidence(),
            )
            .await
            .expect("record the first review");
        assert_eq!(first.review_rounds, 1, "{first:?}");
        assert_eq!(first.fix_rounds, 0, "{first:?}");

        let routed = team
            .route_fix(&record.id, "finding1")
            .await
            .expect("route the finding back to the worker");
        assert_eq!(routed.review_rounds, 1, "{routed:?}");
        assert_eq!(routed.fix_rounds, 1, "{routed:?}");

        // The test endpoint cannot serve the fixer turn; freeze the session and
        // review the still-open finding again: that is review round two.
        team.stop_inner(&record.id).await.expect("stop the worker");
        let second = team
            .review(
                &record.id,
                "second pass".into(),
                vec![open_finding("finding1")],
                evidence(),
            )
            .await
            .expect("record the second review");
        assert_eq!(second.review_rounds, 2, "{second:?}");
        assert_eq!(second.fix_rounds, 1, "{second:?}");
        team.shutdown().await;
    }

    /// An independent audit is counted on the worker it audits, never on the
    /// audit worker itself (plan 40eef714 step 4).
    #[tokio::test]
    async fn audit_dispatch_counts_on_the_target() {
        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        let plans_dir = root.path().join(".coding/plans");
        std::fs::create_dir_all(&plans_dir).expect("plans dir");
        let board = PlanBoard::new(&plans_dir, root.path().join(".coding/reviews"));
        let plan = board
            .create(mnemo::app::plan_board::PlanDraft {
                title: "Audited plan".into(),
                goal: "Count the audits".into(),
                context: "verified: src-tauri/src/mnemo2/team.rs records the audit round.".into(),
                steps: vec!["**Count** the audits in src-tauri/src/mnemo2/team.rs".into()],
                acceptance: vec!["The audited worker records the audit".into()],
                ..Default::default()
            })
            .expect("create the plan");
        let mut record = worker(root.path(), "audited00001");
        record.plan_id = Some(plan.id.clone());
        record.plan_step = Some(0);
        provision_agent_worktree(
            root.path().to_path_buf(),
            record.id.clone(),
            "main".to_string(),
        )
        .await
        .expect("provision the worker checkout");
        let team = open_test_team(root.path(), state.path(), vec![record.clone()]).await;
        let team = Arc::new(team);
        team.attach_board(Arc::new(board));

        let (audit, reused) = team
            .audit_dispatch(&record.id, "auditworker1", None, None, None)
            .await
            .expect("dispatch the audit");
        assert!(!reused, "a fresh audit is not a reuse");
        assert_eq!(audit.kind, "audit", "{audit:?}");
        assert_eq!(audit.audits, 0, "the count lands on the target: {audit:?}");
        let target = team.worker(&record.id).await.expect("the audited worker");
        assert_eq!(target.audits, 1, "{target:?}");
        team.shutdown().await;
    }

    /// An audit that reported ends its own session (plan 5779641): the
    /// slot it held is freed the moment its turn ends, without the lead
    /// stopping it, and its report is already recorded when that happens.
    #[tokio::test]
    async fn finished_audit_releases_its_slot() {
        let (fixture, _notices) = nudge_fixture_with(
            crate::mnemo2::dispatch_routing::RoutingSettings::default(),
            None,
            std::time::Duration::ZERO,
            vec![
                // The audit reports through message_parent, then ends its
                // turn — exactly what audit_prompt() tells it to do.
                tool_call_batch(
                    "audit-report",
                    "message_parent",
                    r#"{"text":"Read-only audit: no findings; the change matches its plan."}"#,
                ),
                vec![
                    mnemo::provider::LlmEvent::TextDelta {
                        text: "reported".to_string(),
                    },
                    mnemo::provider::LlmEvent::Finish {
                        reason: mnemo::provider::FinishReason::Stop,
                    },
                ],
            ],
        )
        .await;
        let team = &fixture.team;
        // One slot: the audit itself is the only worker that may hold it.
        team.persistence.slots.store(1, Ordering::SeqCst);
        // The audited implementation worker: an idle record with a live
        // checkout, dispatched on the fixture's board plan.
        let mut target = worker(fixture._root.path(), "target00001");
        target.plan_id = Some(fixture.plan_id.clone());
        target.plan_step = Some(0);
        provision_agent_worktree(
            fixture._root.path().to_path_buf(),
            target.id.clone(),
            "main".to_string(),
        )
        .await
        .expect("provision the target checkout");
        team.records.lock().await.push(target.clone());

        let (audit, reused) = team
            .audit_dispatch(&target.id, "auditworker1", None, None, None)
            .await
            .expect("dispatch the audit into the one slot");
        assert!(!reused, "a fresh audit is not a reuse");
        let audit_id = audit.id.clone();
        let finished = eventually(|| async {
            let record = team.worker(&audit_id).await.ok()?;
            (record.status == WorkerStatus::Completed).then_some(record)
        })
        .await;
        // The report was recorded before the session ended.
        assert!(finished.report_delivered, "{finished:?}");
        assert!(
            team.messages()
                .await
                .iter()
                .any(|message| message.worker_id == audit_id),
            "the audit's report is recorded for the lead"
        );
        // The finished audit no longer counts against the slots.
        assert_eq!(
            team.live.lock().await.len(),
            0,
            "the finished audit released its slot"
        );
        // The freed slot dispatches a fresh audit with no manual stop
        // (a different id prefix: the finished audit's branch stays).
        team.audit_dispatch(&target.id, "audittwo001", None, None, None)
            .await
            .expect("the released slot dispatches a fresh audit");
        team.shutdown().await;
    }

    /// A finished audit never comes back (plan 5779641): resume, compact,
    /// model change and message are all refused with the same words.
    #[tokio::test]
    async fn finished_audit_cannot_be_resumed() {
        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        let mut audit = worker(root.path(), "auditworker1");
        audit.kind = "audit".to_string();
        audit.status = WorkerStatus::Completed;
        audit.report_delivered = true;
        let team = open_test_team(root.path(), state.path(), vec![audit]).await;
        let refusal = "audit worker 'auditworker1' already delivered its report; dispatch a new audit with team_audit_dispatch";
        let error = team
            .resume("auditworker1")
            .await
            .expect_err("resume refuses a finished audit");
        assert_eq!(error, refusal);
        let error = team
            .compact("auditworker1")
            .await
            .expect_err("compact refuses a finished audit");
        assert_eq!(error, refusal);
        let error = team
            .change_model("auditworker1", None, None, ChangeSource::User)
            .await
            .expect_err("change_model refuses a finished audit");
        assert_eq!(error, refusal);
        let error = team
            .message(
                "auditworker1",
                "one more look at the diff".to_string(),
                MessageSender::User,
            )
            .await
            .expect_err("message refuses a finished audit");
        assert_eq!(error, refusal);
        team.set_catalog(
            vec![lead_model("scripted-difficult")],
            tiers_dto! {
                difficult: vec!["test/scripted-difficult".into()],
                average: Vec::new(),
                simple: Vec::new(),
            },
            Vec::new(),
        );
        let result = team
            .handle_lead_host_tool(
                "team_resume",
                json!({"worker_id": "auditworker1", "tier": "high"}),
            )
            .await;
        assert!(!result.success);
        assert!(result.output.contains(refusal), "{}", result.output);
        team.shutdown().await;
    }

    /// The other side of the coin: an audit that failed before it reported
    /// keeps today's resume path — the lead can still bring it back.
    #[tokio::test]
    async fn audit_that_failed_before_reporting_can_still_be_resumed() {
        let (fixture, _notices) = nudge_fixture_with(
            crate::mnemo2::dispatch_routing::RoutingSettings::default(),
            None,
            std::time::Duration::ZERO,
            vec![vec![
                mnemo::provider::LlmEvent::TextDelta {
                    text: "done".to_string(),
                },
                mnemo::provider::LlmEvent::Finish {
                    reason: mnemo::provider::FinishReason::Stop,
                },
            ]],
        )
        .await;
        let team = &fixture.team;
        let mut audit = worker(fixture._root.path(), "auditworker1");
        audit.kind = "audit".to_string();
        audit.status = WorkerStatus::Failed;
        audit.error = Some("the audit's turn failed before any report".to_string());
        provision_agent_worktree(
            fixture._root.path().to_path_buf(),
            audit.id.clone(),
            "main".to_string(),
        )
        .await
        .expect("provision the audit checkout");
        team.records.lock().await.push(audit.clone());
        let resumed = team
            .resume("auditworker1")
            .await
            .expect("a failed audit can still be resumed");
        assert_eq!(resumed.status, WorkerStatus::Idle, "{resumed:?}");
        team.shutdown().await;
    }

    /// Ledgers written before `report_delivered` existed decode without it;
    /// a `Completed` audit from such a ledger still counts as finished.
    #[tokio::test]
    async fn legacy_completed_audit_counts_as_finished() {
        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        let mut audit = worker(root.path(), "auditworker1");
        audit.kind = "audit".to_string();
        audit.status = WorkerStatus::Completed;
        // The on-disk ledger predates the field: serialize and strip it.
        let mut value = serde_json::to_value(&audit).expect("serialize the audit");
        value
            .as_object_mut()
            .expect("the record serializes as an object")
            .remove("report_delivered");
        let legacy: WorkerRecord =
            serde_json::from_value(value).expect("decode the legacy ledger");
        assert!(!legacy.report_delivered, "{legacy:?}");
        assert!(audit_finished(&legacy), "{legacy:?}");
        let team = open_test_team(root.path(), state.path(), vec![legacy]).await;
        let error = team
            .resume("auditworker1")
            .await
            .expect_err("resume refuses a legacy completed audit");
        assert_eq!(
            error,
            "audit worker 'auditworker1' already delivered its report; dispatch a new audit with team_audit_dispatch"
        );
        team.shutdown().await;
    }

    /// A model switch appends the new spec once: `models_used` is the order the
    /// worker ran in, without consecutive duplicates (plan 40eef714 step 4).
    #[tokio::test]
    async fn model_switch_appends_models_used() {
        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        let team = open_test_team(root.path(), state.path(), Vec::new()).await;
        team.set_catalog(
            vec![lead_model("scripted-a"), lead_model("scripted-b")],
            mnemo::headless::TiersDto::default(),
            Vec::new(),
        );

        let dispatched = team
            .dispatch_with_id(
                "implement the counting feature".into(),
                Some("test/scripted-a".into()),
                Some("models00001".into()),
                None,
            )
            .await
            .expect("dispatch the worker on model A");
        assert_eq!(
            dispatched.models_used,
            vec!["test/scripted-a".to_string()],
            "{dispatched:?}"
        );

        let switched = team
            .change_model(
                "models00001",
                Some("test/scripted-b".into()),
                None,
                ChangeSource::User,
            )
            .await
            .expect("switch the worker to model B");
        assert_eq!(
            switched.models_used,
            vec![
                "test/scripted-a".to_string(),
                "test/scripted-b".to_string()
            ],
            "{switched:?}"
        );

        let again = team
            .change_model(
                "models00001",
                Some("test/scripted-b".into()),
                None,
                ChangeSource::User,
            )
            .await
            .expect("switch to model B again");
        assert_eq!(
            again.models_used,
            vec![
                "test/scripted-a".to_string(),
                "test/scripted-b".to_string()
            ],
            "a repeated model adds nothing: {again:?}"
        );
        team.shutdown().await;
    }

    struct StubDispatchClassifier {
        answer: Option<mnemo::memory::Answer>,
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        delay: std::time::Duration,
    }

    impl StubDispatchClassifier {
        fn new(answer: Option<mnemo::memory::Answer>) -> Self {
            Self {
                answer,
                calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                delay: std::time::Duration::ZERO,
            }
        }

        fn slow(answer: Option<mnemo::memory::Answer>, delay: std::time::Duration) -> Self {
            Self { delay, ..Self::new(answer) }
        }

        fn calls(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::Relaxed)
        }
    }

    #[async_trait::async_trait]
    impl mnemo::memory::Classifier for StubDispatchClassifier {
        async fn classify(
            &self,
            _state: &str,
            _question: &mnemo::memory::Question,
        ) -> Option<mnemo::memory::Answer> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            tokio::time::sleep(self.delay).await;
            self.answer.clone()
        }
    }

    struct RoutingFixture {
        _root: tempfile::TempDir,
        state: tempfile::TempDir,
        _config: tempfile::TempDir,
        team: std::sync::Arc<TeamRuntime>,
        plan_id: String,
        classifier: std::sync::Arc<StubDispatchClassifier>,
    }

    impl RoutingFixture {
        fn log_path(&self) -> std::path::PathBuf {
            self.state.path().join("dispatch_routing.jsonl")
        }

        /// The rows written so far; an absent log (routing off, never
        /// touched) reads as no rows.
        fn rows(&self) -> Vec<crate::mnemo2::dispatch_routing::DispatchRoutingRow> {
            std::fs::read_to_string(self.log_path())
                .unwrap_or_default()
                .lines()
                .map(|line| serde_json::from_str(line).expect("routing row"))
                .collect()
        }

        async fn dispatch(&self, tier: Option<&str>, model: Option<&str>) -> ToolResult {
            let mut args = json!({
                "worker_id": "routingworker",
                "plan_id": self.plan_id,
                "step": 1,
                "brief": "Implement the step exactly and run its focused regression tests.",
            });
            if let Some(tier) = tier {
                args["tier"] = json!(tier);
            }
            if let Some(model) = model {
                args["model"] = json!(model);
            }
            self.team.handle_lead_host_tool("team_dispatch", args).await
        }
    }

    fn routing_answer(label: &str, confidence: f64) -> Option<mnemo::memory::Answer> {
        Some(mnemo::memory::Answer::Choice {
            label: label.into(),
            confidence,
            probabilities: std::collections::BTreeMap::new(),
        })
    }

    async fn routing_fixture_with(
        settings: crate::mnemo2::dispatch_routing::RoutingSettings,
        answer: Option<mnemo::memory::Answer>,
        install_classifier: bool,
        delay: std::time::Duration,
    ) -> RoutingFixture {
        use crate::mnemo2::dispatch_routing::save_routing_settings;
        use mnemo::app::plan_board::{PlanBoard, PlanDraft};

        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        let config = tempfile::tempdir().expect("config");
        init_git(root.path());
        save_routing_settings(config.path(), &settings).expect("save routing mode");
        let mut team = open_test_team_with_config(
            root.path(),
            state.path(),
            Vec::new(),
            config.path(),
        )
        .await;
        let models = vec![
            lead_model("scripted-difficult"),
            lead_model("scripted-average"),
            lead_model("scripted-simple"),
        ];
        team.set_catalog(
            models,
            tiers_dto! {
                difficult: vec!["test/scripted-difficult".into()],
                average: vec!["test/scripted-average".into()],
                simple: vec!["test/scripted-simple".into()],
            },
            Vec::new(),
        );
        let classifier = std::sync::Arc::new(StubDispatchClassifier::slow(answer, delay));
        if install_classifier {
            let slot: mnemo::memory::classifier::ClassifierSlot =
                std::sync::Arc::new(std::sync::RwLock::new(Some(classifier.clone())));
            team.classifier = Some(slot);
        }
        let board = Arc::new(
            PlanBoard::new(
                root.path().join(".coding/plans"),
                root.path().join(".coding/reviews"),
            ),
        );
        let plan = board
            .create(PlanDraft {
                title: "Dispatch routing test".into(),
                goal: "Exercise Laya dispatch routing through the real worker lifecycle".into(),
                context: "A persisted board step supplies the exact implementation text and worker brief.".into(),
                steps: vec!["Implement the requested routing behavior in src-tauri/src/mnemo2/team.rs and verify it with focused regression tests".into()],
                ..PlanDraft::default()
            })
            .expect("create routing plan");
        let team = Arc::new(team);
        team.attach_board(board);
        RoutingFixture {
            _root: root,
            state,
            _config: config,
            team,
            plan_id: plan.id,
            classifier,
        }
    }

    async fn routing_fixture(
        mode: crate::mnemo2::dispatch_routing::RoutingMode,
        answer: Option<mnemo::memory::Answer>,
        install_classifier: bool,
    ) -> RoutingFixture {
        routing_fixture_with(
            crate::mnemo2::dispatch_routing::RoutingSettings {
                mode,
                ..crate::mnemo2::dispatch_routing::RoutingSettings::default()
            },
            answer,
            install_classifier,
            std::time::Duration::ZERO,
        )
        .await
    }

    /// Poll `probe` every 25 ms until it resolves to `Some`, or fail after
    /// 20 s: the worker pumps and the detached drift work run concurrently
    /// with the test.
    async fn eventually<T, F, Fut>(probe: F) -> T
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Option<T>>,
    {
        for _ in 0..800 {
            if let Some(value) = probe().await {
                return value;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        panic!("the polled condition never became true");
    }

    /// A scripted LLM client (the team module's scripted-mock seam): every
    /// `complete` serves the next scripted batch, then a plain text stop —
    /// so a dispatched worker runs real turns, executes real tool calls,
    /// and the pump observes real tool events.
    struct ScriptedLlm {
        responses: std::sync::Mutex<VecDeque<Vec<mnemo::provider::LlmEvent>>>,
    }

    impl ScriptedLlm {
        fn new(responses: Vec<Vec<mnemo::provider::LlmEvent>>) -> Self {
            Self {
                responses: std::sync::Mutex::new(responses.into()),
            }
        }
    }

    #[async_trait::async_trait]
    impl mnemo::provider::LlmClient for ScriptedLlm {
        fn capabilities(&self) -> &mnemo::provider::Capabilities {
            static CAPS: std::sync::OnceLock<mnemo::provider::Capabilities> =
                std::sync::OnceLock::new();
            CAPS.get_or_init(mnemo::provider::Capabilities::openai)
        }

        fn kind(&self) -> mnemo::provider::ProviderKind {
            mnemo::provider::ProviderKind::OpenAI
        }

        fn model(&self) -> &str {
            "scripted"
        }

        async fn complete(
            &self,
            _messages: &[mnemo::provider::Message],
            _tools: &[mnemo::provider::ToolSchema],
            _tool_choice: Option<mnemo::provider::ToolChoice>,
        ) -> mnemo::error::Result<
            futures::stream::BoxStream<'_, mnemo::provider::LlmEvent>,
        > {
            let events = self
                .responses
                .lock()
                .map(|mut queue| queue.pop_front())
                .unwrap_or_default()
                .unwrap_or_else(|| {
                    vec![
                        mnemo::provider::LlmEvent::TextDelta {
                            text: "done".to_string(),
                        },
                        mnemo::provider::LlmEvent::Finish {
                            reason: mnemo::provider::FinishReason::Stop,
                        },
                    ]
                });
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    /// One scripted batch: a single tool call the session executes for
    /// real. An unknown tool name deterministically fails; `read_files` on
    /// the worktree's seeded README.md deterministically succeeds.
    fn tool_call_batch(call_id: &str, name: &str, args: &str) -> Vec<mnemo::provider::LlmEvent> {
        vec![
            mnemo::provider::LlmEvent::ToolCallStart {
                index: 0,
                id: call_id.to_string(),
                name: name.to_string(),
            },
            mnemo::provider::LlmEvent::ToolCallArgumentDelta {
                index: 0,
                fragment: args.to_string(),
            },
            mnemo::provider::LlmEvent::Finish {
                reason: mnemo::provider::FinishReason::ToolCalls,
            },
        ]
    }

    /// The nudge tests' live team (design §4.6): the routing settings come
    /// from the config dir, all three tiers are in the catalog, and the
    /// worker sessions run real turns served by the scripted LLM — so the
    /// pump observes real tool results and the strike counter counts real
    /// failures. Returns the usual routing fixture plus a board-signal
    /// subscription taken before any dispatch, to catch nudge notices.
    async fn nudge_fixture_with(
        settings: crate::mnemo2::dispatch_routing::RoutingSettings,
        answer: Option<mnemo::memory::Answer>,
        delay: std::time::Duration,
        responses: Vec<Vec<mnemo::provider::LlmEvent>>,
    ) -> (RoutingFixture, broadcast::Receiver<BoardSignal>) {
        use crate::mnemo2::dispatch_routing::save_routing_settings;
        use mnemo::app::plan_board::{PlanBoard, PlanDraft};

        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        let config = tempfile::tempdir().expect("config");
        init_git(root.path());
        save_routing_settings(config.path(), &settings).expect("save routing settings");
        std::fs::write(
            config.path().join("config.toml"),
            "[general]\ndefault_provider = \"test\"\ndefault_model = \"scripted-a\"\n",
        )
        .expect("config.toml");
        std::fs::write(
            config.path().join("endpoints.toml"),
            "[[endpoint]]\nname = \"test\"\nkind = \"local\"\nbase_url = \"http://127.0.0.1:9/v1/\"\nmodels = [\"scripted-a\", \"scripted-b\"]\n",
        )
        .expect("endpoints.toml");
        TeamPersistence::new(state.path())
            .save(&PersistedTeam {
                workers: Vec::new(),
                slots: Some(2),
                usage: ProjectUsage::default(),
            })
            .expect("save the team ledger");
        let engine = mnemo::headless::EngineBuilder::new(mnemo::headless::EngineOptions {
            workspace: mnemo::workspace::Workspace::new(root.path()),
            config_dir: Some(config.path().to_path_buf()),
            permissions: mnemo::agent::policy::PermissionPolicy::OverridePermissions,
            memory: mnemo::headless::MemoryBackend::Disabled,
            index_codegraph: false,
            require_git: true,
            auto_install_embedder: false,
        })
        .provider(Arc::new(ScriptedLlm::new(responses)))
        .start()
        .await
        .expect("start the scripted engine");
        let mut team = TeamRuntime::open(TeamRuntimeOptions {
            engine: Arc::new(engine),
            workspace_root: root.path().to_path_buf(),
            state_dir: state.path().to_path_buf(),
            config_dir: config.path().to_path_buf(),
            classifier: None,
            routing_log_path: Some(state.path().join("dispatch_routing.jsonl")),
            slots: Some(2),
            models: Vec::new(),
            pricing: Vec::new(),
        })
        .await
        .expect("open the team");
        team.set_catalog(
            vec![
                lead_model("scripted-difficult"),
                lead_model("scripted-average"),
                lead_model("scripted-simple"),
            ],
            tiers_dto! {
                difficult: vec!["test/scripted-difficult".into()],
                average: vec!["test/scripted-average".into()],
                simple: vec!["test/scripted-simple".into()],
            },
            Vec::new(),
        );
        let classifier = std::sync::Arc::new(StubDispatchClassifier::slow(answer, delay));
        let slot: mnemo::memory::classifier::ClassifierSlot =
            std::sync::Arc::new(std::sync::RwLock::new(Some(classifier.clone())));
        team.classifier = Some(slot);
        let board = Arc::new(PlanBoard::new(
            root.path().join(".coding/plans"),
            root.path().join(".coding/reviews"),
        ));
        let plan = board
            .create(PlanDraft {
                title: "Nudge test".into(),
                goal: "Exercise the nudge paths through the real worker lifecycle".into(),
                context: "A persisted board step supplies the exact implementation text and worker brief.".into(),
                steps: vec!["Implement the requested nudge behavior in src-tauri/src/mnemo2/team.rs and verify it with focused regression tests".into()],
                ..PlanDraft::default()
            })
            .expect("create the nudge plan");
        let team = Arc::new(team);
        team.attach_board(board);
        let notices = team.subscribe_board();
        (
            RoutingFixture {
                _root: root,
                state,
                _config: config,
                team,
                plan_id: plan.id,
                classifier,
            },
            notices,
        )
    }

    /// A shadow-mode nudge fixture with the scripted responses.
    async fn nudge_fixture(
        responses: Vec<Vec<mnemo::provider::LlmEvent>>,
    ) -> (RoutingFixture, broadcast::Receiver<BoardSignal>) {
        nudge_fixture_with(
            crate::mnemo2::dispatch_routing::RoutingSettings {
                mode: crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
                ..crate::mnemo2::dispatch_routing::RoutingSettings::default()
            },
            routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.62),
            std::time::Duration::ZERO,
            responses,
        )
        .await
    }

    /// Wait until the live worker's turn settles: the pump processes the
    /// turn's tool events synchronously, so once the status flips the
    /// nudge decision has also been made.
    async fn wait_worker_settled(team: &TeamRuntime, worker_id: &str) -> WorkerRecord {
        eventually(|| async {
            let worker = team.worker(worker_id).await.expect("worker");
            matches!(
                worker.status,
                WorkerStatus::Idle
                    | WorkerStatus::Completed
                    | WorkerStatus::Stopped
                    | WorkerStatus::Failed
            )
            .then_some(worker)
        })
        .await
    }

    /// Poll the board signals until the worker's nudge notice lands;
    /// unrelated refreshes drain away.
    async fn wait_nudge_notice(
        notices: &mut broadcast::Receiver<BoardSignal>,
        worker_id: &str,
    ) -> String {
        let key = format!("nudge:{worker_id}");
        for _ in 0..800 {
            while let Ok(signal) = notices.try_recv() {
                if let BoardSignal::Notice { key: notice_key, text } = signal {
                    if notice_key == key {
                        return text;
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        panic!("the nudge notice never arrived");
    }

    /// Every cost-alert notice on the board-signal stream so far; unrelated
    /// refreshes and notices drain away.
    fn drain_cost_notices(
        notices: &mut broadcast::Receiver<BoardSignal>,
        worker_id: &str,
    ) -> Vec<String> {
        let prefix = format!("cost:{worker_id}:");
        let mut found = Vec::new();
        loop {
            match notices.try_recv() {
                Ok(BoardSignal::Notice { key, text }) if key.starts_with(&prefix) => {
                    found.push(text);
                }
                Ok(_) => {}
                Err(broadcast::error::TryRecvError::Lagged(_)) => {}
                Err(_) => break,
            }
        }
        found
    }

    /// The cost alert fires at the threshold, then at each doubling — never
    /// twice for the same level, and never when it is off (plan 06f1a204).
    #[test]
    fn cost_notice_due_fires_at_the_threshold_and_each_doubling() {
        let threshold = 2_000_000;
        assert_eq!(
            cost_notice_due(1_999_999, 0, threshold),
            None,
            "below the threshold"
        );
        assert_eq!(
            cost_notice_due(2_000_000, 0, threshold),
            Some(2_000_000),
            "the threshold itself"
        );
        assert_eq!(
            cost_notice_due(3_000_000, 2_000_000, threshold),
            None,
            "between two levels"
        );
        assert_eq!(
            cost_notice_due(4_000_000, 2_000_000, threshold),
            Some(4_000_000),
            "the first doubling"
        );
        assert_eq!(
            cost_notice_due(9_000_000, 0, threshold),
            Some(8_000_000),
            "the highest doubling passed, not the next one"
        );
        assert_eq!(
            cost_notice_due(u64::MAX, 0, threshold),
            Some(u64::MAX),
            "a saturated cost still terminates"
        );
        assert_eq!(
            cost_notice_due(9_000_000, 0, 0),
            None,
            "0 turns the alert off"
        );
    }

    /// One worker whose estimated cost passes the alert tells the lead once,
    /// and keeps running: the notice never stops or changes it (plan
    /// 06f1a204).
    #[tokio::test]
    async fn a_worker_past_the_threshold_notifies_the_lead_once() {
        let responses = vec![vec![
            mnemo::provider::LlmEvent::Usage {
                prompt_tokens: 3,
                completion_tokens: 0,
                reasoning_tokens: 0,
                cached_tokens: 0,
                ttft_ms: None,
                generation_ms: None,
            },
            mnemo::provider::LlmEvent::TextDelta {
                text: "done".to_string(),
            },
            mnemo::provider::LlmEvent::Finish {
                reason: mnemo::provider::FinishReason::Stop,
            },
        ]];
        let (fixture, mut notices) = nudge_fixture(responses).await;
        // $1 per token on the scripted provider's model: the turn's three
        // prompt tokens cost $3.00, past the default $2.00 alert.
        fixture.team.set_catalog(
            vec![
                lead_model("scripted-difficult"),
                lead_model("scripted-average"),
                lead_model("scripted-simple"),
            ],
            tiers_dto! {
                difficult: vec!["test/scripted-difficult".into()],
                average: vec!["test/scripted-average".into()],
                simple: vec!["test/scripted-simple".into()],
            },
            vec![mnemo::config::PricingEntry {
                model: "scripted".to_string(),
                input_per_1m: 1_000_000.0,
                output_per_1m: 0.0,
                cached_per_1m: 0.0,
            }],
        );
        let result = fixture.dispatch(Some("routine"), None).await;
        assert!(result.success, "{}", result.output);
        let settled = wait_worker_settled(&fixture.team, "routingworker").await;
        // The pump sends the notice right after the record block of the
        // Usage event, which precedes the Finished event that settled the
        // worker above — so it is on the stream by now.
        let found = drain_cost_notices(&mut notices, "routingworker");
        assert_eq!(
            found.len(),
            1,
            "exactly one cost notice: {found:?}; worker: {settled:?}"
        );
        let notice = &found[0];
        assert!(
            notice.starts_with("worker_id=routingworker "),
            "the notice names the worker: {notice}"
        );
        assert!(
            notice.contains("has cost about $3.00 so far"),
            "the notice names the spend: {notice}"
        );
        assert!(
            notice.contains("1 requests on scripted, tier routine."),
            "the notice names the request count, model and tier: {notice}"
        );
        assert!(
            notice.ends_with(
                "It keeps running. If that looks wrong, read team_status, then team_message to steer it or team_stop to cancel it."
            ),
            "{notice}"
        );
        assert_eq!(
            settled.cost_notified_micros, 2_000_000,
            "the record remembers the level the lead was told about: {settled:?}"
        );
        assert_eq!(settled.cost_micros, Some(3_000_000), "{settled:?}");
        assert!(
            matches!(settled.status, WorkerStatus::Completed),
            "the alert never stopped or changed the worker: {settled:?}"
        );
        assert!(settled.error.is_none(), "{settled:?}");
    }

    /// A lead message long enough for drift classification (≥ 20 words).
    fn drift_message() -> String {
        "This change turned out to touch the shared routing contract after all, \
         so please be extra careful with the integration boundary and run the \
         full regression battery once more before you call the step done."
            .to_string()
    }

    /// Three failing tool calls in a row nudge the lead once — and only
    /// once, even when the streak keeps failing (design §4.6).
    #[tokio::test]
    async fn three_failing_tool_calls_nudge_once() {
        let responses = (1..=3)
            .map(|attempt| tool_call_batch(&format!("call-{attempt}"), "not_a_real_tool", &format!("{{\"attempt\":{attempt}}}")))
            .chain([vec![
                mnemo::provider::LlmEvent::TextDelta { text: "done".to_string() },
                mnemo::provider::LlmEvent::Finish { reason: mnemo::provider::FinishReason::Stop },
            ]])
            .collect::<Vec<_>>();
        let (fixture, mut notices) = nudge_fixture(responses).await;
        let result = fixture.dispatch(Some("routine"), None).await;
        assert!(result.success, "{}", result.output);
        let row = eventually(|| async {
            fixture
                .rows()
                .into_iter()
                .find(|row| matches!(row, DispatchRoutingRow::Nudge { .. }))
        })
        .await;
        assert!(
            matches!(
                &row,
                DispatchRoutingRow::Nudge {
                    reason,
                    from_tier,
                    to_tier,
                    detail,
                    ..
                } if reason == "strikes"
                    && from_tier == "routine"
                    && to_tier == "high"
                    && detail == "looks stuck (3 failing tool calls in a row)"
            ),
            "the strike nudge row: {row:?}"
        );
        let notice = wait_nudge_notice(&mut notices, "routingworker").await;
        let prefix = "worker_id=routingworker Laya nudge: routingworker looks stuck (3 failing tool calls in a row); consider team_resume with tier high.\nLast tool errors:";
        assert!(notice.starts_with(prefix), "{notice}");
        assert_eq!(
            notice.matches("\n- not_a_real_tool: ").count(),
            3,
            "the failed call's tool name, once per kept failure: {notice}"
        );
        assert!(
            notice.contains("unknown tool 'not_a_real_tool'"),
            "the failed call's actual output: {notice}"
        );
        assert!(
            notice.ends_with(
                "\nRead these first: a tool problem is not fixed by a stronger model."
            ),
            "{notice}"
        );
        let settled = wait_worker_settled(&fixture.team, "routingworker").await;
        assert_eq!(
            settled.routing.as_ref().expect("routed at dispatch").nudged.as_deref(),
            Some("high"),
            "the nudge is marked on the record: {settled:?}"
        );
        // Three consecutive real failures also trip the session's own
        // doom guard, so the worker lands Failed — exactly when the nudge
        // telling the lead to resume on a stronger tier is useful. The
        // nudge never stopped or switched anything itself.
        assert!(
            matches!(settled.status, WorkerStatus::Failed),
            "the doom guard, not the nudge, failed the worker: {settled:?}"
        );
        assert_ne!(
            settled.model.as_deref(),
            Some("test/scripted-difficult"),
            "the nudge never switched the model: {settled:?}"
        );
        assert_eq!(
            fixture
                .rows()
                .into_iter()
                .filter(|row| matches!(row, DispatchRoutingRow::Nudge { .. }))
                .count(),
            1,
            "exactly one nudge: {:?}",
            fixture.rows()
        );
        // A failure is resumable, not terminal: the doom-guard failure
        // logs the outcome but never nudge_ignored — a team_resume on a
        // stronger tier can still take the nudge.
        assert!(
            matches!(
                fixture.rows().as_slice(),
                [DispatchRoutingRow::Route { .. },
                 DispatchRoutingRow::Nudge { .. },
                 DispatchRoutingRow::Outcome { status, .. }]
                    if status == "failed"
            ),
            "the route, the nudge, and the outcome rows: {:?}",
            fixture.rows()
        );
        fixture.team.shutdown().await;
    }

    /// The failing-tool streak keeps the last three calls, collapses each
    /// output's whitespace, cuts it at the character limit without splitting a
    /// character, and a successful call clears it.
    #[test]
    fn note_tool_outcome_keeps_the_last_three_and_clears_on_success() {
        let mut worker = worker(std::path::Path::new("."), "streak0001");
        for (index, tool) in ["file_read", "shell", "git", "plan_read"]
            .iter()
            .enumerate()
        {
            note_tool_outcome(&mut worker, tool, false, &format!("  failed\n\ncall {index}  "));
        }
        let tools: Vec<&str> = worker
            .tool_errors
            .iter()
            .map(|note| note.tool.as_str())
            .collect();
        assert_eq!(tools, ["shell", "git", "plan_read"], "only the last three");
        assert_eq!(worker.tool_errors[0].output, "failed call 1");

        // A multi-byte output is cut by character, never mid-character.
        let long = "é".repeat(TOOL_ERROR_CHARS + 5);
        note_tool_outcome(&mut worker, "shell", false, &long);
        let kept = worker.tool_errors.last().expect("the newest failure");
        assert_eq!(kept.output.chars().count(), TOOL_ERROR_CHARS + 1);
        assert!(kept.output.ends_with('…'), "{kept:?}");
        assert_eq!(
            kept.output.chars().take(TOOL_ERROR_CHARS).collect::<String>(),
            "é".repeat(TOOL_ERROR_CHARS)
        );
        assert_eq!(
            tool_errors_text(&worker),
            format!(
                "\nLast tool errors:\n- git: failed call 2\n- plan_read: failed call 3\n- shell: {}…",
                "é".repeat(TOOL_ERROR_CHARS)
            )
        );

        // A success clears the streak, and an empty streak has no text.
        note_tool_outcome(&mut worker, "file_read", true, "ok");
        assert!(worker.tool_errors.is_empty());
        assert!(tool_errors_text(&worker).is_empty());
    }

    /// The strike nudge carries the actual failed calls — the tool name and
    /// its output for each of the last three failures — and the warning that a
    /// stronger model does not fix a tool problem.
    #[tokio::test]
    async fn the_strike_nudge_shows_the_tool_errors() {
        let responses = (1..=3)
            .map(|attempt| {
                tool_call_batch(
                    &format!("call-{attempt}"),
                    "not_a_real_tool",
                    &format!("{{\"attempt\":{attempt}}}"),
                )
            })
            .collect::<Vec<_>>();
        let (fixture, mut notices) = nudge_fixture(responses).await;
        let result = fixture.dispatch(Some("routine"), None).await;
        assert!(result.success, "{}", result.output);
        let notice = wait_nudge_notice(&mut notices, "routingworker").await;
        let prefix = "worker_id=routingworker Laya nudge: routingworker looks stuck (3 failing tool calls in a row); consider team_resume with tier high.\nLast tool errors:";
        assert!(notice.starts_with(prefix), "{notice}");
        assert_eq!(
            notice.matches("\n- not_a_real_tool: ").count(),
            3,
            "the tool's name on each kept failure: {notice}"
        );
        assert!(
            notice.contains("unknown tool 'not_a_real_tool'"),
            "the tool's own error: {notice}"
        );
        assert!(
            notice.ends_with("\nRead these first: a tool problem is not fixed by a stronger model."),
            "{notice}"
        );
        let settled = wait_worker_settled(&fixture.team, "routingworker").await;
        assert_eq!(settled.tool_errors.len(), 3, "{settled:?}");
        fixture.team.shutdown().await;
    }

    /// `team_status` reports each worker's current failing streak, and omits
    /// the field for a worker whose last call succeeded.
    #[tokio::test]
    async fn team_status_reports_tool_errors() {
        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        let mut failing = reviewed_worker(root.path(), "00000001000000000003");
        failing.tool_errors = vec![
            ToolErrorNote {
                tool: "shell".to_string(),
                output: "exit status 1".to_string(),
            },
            ToolErrorNote {
                tool: "git".to_string(),
                output: "no such branch".to_string(),
            },
        ];
        let clean = reviewed_worker(root.path(), "00000001000000000004");
        let team = open_test_team(root.path(), state.path(), vec![failing, clean]).await;
        let answer = team_status_answer(&team, json!({})).await;
        let entries = answer["result"].as_array().expect("a worker array");
        assert_eq!(entries[0]["tool_errors"][0]["tool"], "shell");
        assert_eq!(entries[0]["tool_errors"][1]["output"], "no such branch");
        assert!(
            entries[1].get("tool_errors").is_none(),
            "a clean worker has no tool_errors key: {}",
            entries[1]
        );
        team.shutdown().await;
    }

    /// A failed worker whose session is alive and idle resumes in place:
    /// `team_resume` makes it idle instead of dead-ending on "already
    /// running", clears the error, and keeps the failing streak readable.
    #[tokio::test]
    async fn resume_makes_a_failed_worker_with_a_live_session_idle() {
        let responses = (1..=3)
            .map(|attempt| {
                tool_call_batch(
                    &format!("call-{attempt}"),
                    "not_a_real_tool",
                    &format!("{{\"attempt\":{attempt}}}"),
                )
            })
            .collect::<Vec<_>>();
        let (fixture, _notices) = nudge_fixture(responses).await;
        let result = fixture.dispatch(Some("routine"), None).await;
        assert!(result.success, "{}", result.output);
        let failed = wait_worker_settled(&fixture.team, "routingworker").await;
        assert_eq!(failed.status, WorkerStatus::Failed, "{failed:?}");
        assert!(
            fixture.team.live.lock().await.contains_key("routingworker"),
            "the failed worker's session is still live"
        );
        let resumed = fixture
            .team
            .resume("routingworker")
            .await
            .expect("a failed worker with a live idle session resumes");
        assert_eq!(resumed.status, WorkerStatus::Idle, "{resumed:?}");
        assert!(resumed.error.is_none(), "{resumed:?}");
        assert!(resumed.pending.is_none(), "{resumed:?}");
        assert_eq!(resumed.tool_errors.len(), 3, "the errors stay readable");
        fixture.team.shutdown().await;
    }

    /// A worker that is actually running still refuses a resume, now pointing
    /// the lead at `team_message`.
    #[tokio::test]
    async fn resume_still_refuses_a_running_worker() {
        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        let mut busy = worker(root.path(), "00000001000000000005");
        busy.status = WorkerStatus::Working;
        provision_agent_worktree(
            root.path().to_path_buf(),
            busy.id.clone(),
            "main".to_string(),
        )
        .await
        .expect("provision the running worker");
        let team = open_test_team(root.path(), state.path(), vec![busy.clone()]).await;
        spawn_live_worker(&team, &busy).await;
        team.live
            .lock()
            .await
            .get(&busy.id)
            .expect("the live worker")
            .busy
            .store(true, Ordering::SeqCst);
        let error = team
            .resume(&busy.id)
            .await
            .expect_err("resume refuses a running worker");
        assert!(error.contains("already running"), "{error}");
        assert!(error.contains("team_message"), "{error}");
        team.shutdown().await;
    }

    /// A delivered message starts the worker over: the stale failure error is
    /// cleared from the record.
    #[tokio::test]
    async fn a_message_clears_a_failed_workers_error() {
        let responses = (1..=3)
            .map(|attempt| {
                tool_call_batch(
                    &format!("call-{attempt}"),
                    "not_a_real_tool",
                    &format!("{{\"attempt\":{attempt}}}"),
                )
            })
            .collect::<Vec<_>>();
        let (fixture, _notices) = nudge_fixture(responses).await;
        let result = fixture.dispatch(Some("routine"), None).await;
        assert!(result.success, "{}", result.output);
        let failed = wait_worker_settled(&fixture.team, "routingworker").await;
        assert!(failed.error.is_some(), "{failed:?}");
        let messaged = fixture
            .team
            .message(
                "routingworker",
                "that tool does not exist; read the file instead".to_string(),
                MessageSender::Lead,
            )
            .await
            .expect("a failed worker with a live session takes a message");
        assert!(messaged.error.is_none(), "{messaged:?}");
        fixture.team.shutdown().await;
    }

    /// A failure is resumable, not terminal: the doom-guard failure that
    /// rides the strike nudge logs the outcome but never `nudge_ignored` —
    /// `team_resume` on a stronger tier can still take the nudge.
    #[tokio::test]
    async fn failed_worker_does_not_log_nudge_ignored() {
        let responses = (1..=3)
            .map(|attempt| {
                tool_call_batch(
                    &format!("call-{attempt}"),
                    "not_a_real_tool",
                    &format!("{{\"attempt\":{attempt}}}"),
                )
            })
            .collect::<Vec<_>>();
        let (fixture, _notices) = nudge_fixture(responses).await;
        let result = fixture.dispatch(Some("routine"), None).await;
        assert!(result.success, "{}", result.output);
        // The terminal failure row is the deterministic marker that the
        // pump has fully processed the worker's failure; the nudge row
        // lands with the third tool result, before the Error event.
        let outcome = eventually(|| async {
            fixture.rows().into_iter().find(|row| {
                matches!(row, DispatchRoutingRow::Outcome { status, .. } if status == "failed")
            })
        })
        .await;
        assert!(
            matches!(&outcome, DispatchRoutingRow::Outcome { final_tier, .. } if final_tier == "routine"),
            "the failure outcome: {outcome:?}"
        );
        assert!(
            fixture
                .rows()
                .iter()
                .any(|row| matches!(row, DispatchRoutingRow::Nudge { .. })),
            "the strike nudge fired before the failure: {:?}",
            fixture.rows()
        );
        assert!(
            fixture
                .rows()
                .iter()
                .all(|row| !matches!(row, DispatchRoutingRow::NudgeIgnored { .. })),
            "a failed worker never logs nudge_ignored: {:?}",
            fixture.rows()
        );
        fixture.team.shutdown().await;
    }

    /// Stopping the failed worker is truly terminal: the ignored nudge logs
    /// there, once — a repeated stop writes nothing more.
    #[tokio::test]
    async fn stopped_after_failure_logs_nudge_ignored_once() {
        let responses = (1..=3)
            .map(|attempt| {
                tool_call_batch(
                    &format!("call-{attempt}"),
                    "not_a_real_tool",
                    &format!("{{\"attempt\":{attempt}}}"),
                )
            })
            .collect::<Vec<_>>();
        let (fixture, _notices) = nudge_fixture(responses).await;
        let result = fixture.dispatch(Some("routine"), None).await;
        assert!(result.success, "{}", result.output);
        // The strike nudge fires and the doom guard fails the worker.
        let failed = eventually(|| async {
            let worker = fixture
                .team
                .worker("routingworker")
                .await
                .expect("worker");
            (worker.status == WorkerStatus::Failed).then_some(worker)
        })
        .await;
        assert!(
            failed.routing.as_ref().expect("routed at dispatch").nudged.is_some(),
            "the strike nudge was claimed before the failure: {failed:?}"
        );
        assert!(
            fixture
                .rows()
                .iter()
                .all(|row| !matches!(row, DispatchRoutingRow::NudgeIgnored { .. })),
            "the failure itself never logs nudge_ignored: {:?}",
            fixture.rows()
        );
        // The lead stops the failed worker: the ignored nudge lands beside
        // the stopped outcome.
        let stopped = fixture
            .team
            .stop("routingworker")
            .await
            .expect("stop the failed worker");
        assert!(matches!(stopped.status, WorkerStatus::Stopped), "{stopped:?}");
        assert!(
            matches!(
                fixture.rows().as_slice(),
                [..,
                 DispatchRoutingRow::Outcome { status, .. },
                 DispatchRoutingRow::NudgeIgnored { tier, .. }
                ] if status == "stopped" && tier == "routine"
            ),
            "the stopped outcome plus the ignored nudge: {:?}",
            fixture.rows()
        );
        // Stopping again writes nothing more.
        fixture
            .team
            .stop("routingworker")
            .await
            .expect("stopping again is a no-op");
        assert_eq!(
            fixture
                .rows()
                .iter()
                .filter(|row| matches!(row, DispatchRoutingRow::NudgeIgnored { .. }))
                .count(),
            1,
            "nudge_ignored logs once: {:?}",
            fixture.rows()
        );
        fixture.team.shutdown().await;
    }

    /// Identical tool calls count as strikes even when they succeed: the
    /// repeat is the strike, and the nudge says "going in circles".
    #[tokio::test]
    async fn identical_calls_count_as_strikes() {
        let batch = tool_call_batch("same-call", "read_files", "{\"files\":[\"README.md\"]}");
        let responses = vec![
            batch.clone(),
            batch.clone(),
            batch.clone(),
            batch,
            vec![
                mnemo::provider::LlmEvent::TextDelta { text: "done".to_string() },
                mnemo::provider::LlmEvent::Finish { reason: mnemo::provider::FinishReason::Stop },
            ],
        ];
        let (fixture, _notices) = nudge_fixture(responses).await;
        let result = fixture.dispatch(Some("routine"), None).await;
        assert!(result.success, "{}", result.output);
        let row = eventually(|| async {
            fixture
                .rows()
                .into_iter()
                .find(|row| matches!(row, DispatchRoutingRow::Nudge { .. }))
        })
        .await;
        assert!(
            matches!(
                &row,
                DispatchRoutingRow::Nudge { reason, detail, to_tier, .. }
                    if reason == "circles"
                        && detail == "looks stuck (going in circles)"
                        && to_tier == "high"
            ),
            "the circles nudge row: {row:?}"
        );
        fixture.team.shutdown().await;
    }

    /// A successful, non-repeated call resets the strike counter: two
    /// failures, a success, two more failures never reach three.
    #[tokio::test]
    async fn a_success_resets_the_strikes() {
        let responses = vec![
            tool_call_batch("call-1", "not_a_real_tool", "{\"attempt\":1}"),
            tool_call_batch("call-2", "not_a_real_tool", "{\"attempt\":2}"),
            tool_call_batch("call-3", "read_files", "{\"files\":[\"README.md\"]}"),
            tool_call_batch("call-4", "not_a_real_tool", "{\"attempt\":4}"),
            tool_call_batch("call-5", "not_a_real_tool", "{\"attempt\":5}"),
            vec![
                mnemo::provider::LlmEvent::TextDelta { text: "done".to_string() },
                mnemo::provider::LlmEvent::Finish { reason: mnemo::provider::FinishReason::Stop },
            ],
        ];
        let (fixture, mut notices) = nudge_fixture(responses).await;
        let result = fixture.dispatch(Some("routine"), None).await;
        assert!(result.success, "{}", result.output);
        let settled = wait_worker_settled(&fixture.team, "routingworker").await;
        assert!(
            fixture
                .rows()
                .into_iter()
                .all(|row| !matches!(row, DispatchRoutingRow::Nudge { .. })),
            "the reset streak never nudged: {:?}",
            fixture.rows()
        );
        assert!(
            settled.routing.as_ref().expect("routed at dispatch").nudged.is_none(),
            "the reset streak never claimed a nudge: {settled:?}"
        );
        let mut saw_nudge_notice = false;
        while let Ok(signal) = notices.try_recv() {
            if let BoardSignal::Notice { key, .. } = signal {
                if key.starts_with("nudge:") {
                    saw_nudge_notice = true;
                }
            }
        }
        assert!(!saw_nudge_notice, "no nudge notice was queued");
        fixture.team.shutdown().await;
    }

    /// A worker already on the top enabled tier is never nudged.
    #[tokio::test]
    async fn no_nudge_on_the_top_tier() {
        let responses = (1..=3)
            .map(|attempt| tool_call_batch(&format!("call-{attempt}"), "not_a_real_tool", &format!("{{\"attempt\":{attempt}}}")))
            .chain([vec![
                mnemo::provider::LlmEvent::TextDelta { text: "done".to_string() },
                mnemo::provider::LlmEvent::Finish { reason: mnemo::provider::FinishReason::Stop },
            ]])
            .collect::<Vec<_>>();
        let (fixture, _notices) = nudge_fixture(responses).await;
        let result = fixture.dispatch(Some("high"), None).await;
        assert!(result.success, "{}", result.output);
        let settled = wait_worker_settled(&fixture.team, "routingworker").await;
        assert_eq!(settled.tier.as_deref(), Some("high"), "{settled:?}");
        assert!(
            fixture
                .rows()
                .into_iter()
                .all(|row| !matches!(row, DispatchRoutingRow::Nudge { .. })),
            "the top tier is never nudged: {:?}",
            fixture.rows()
        );
        assert!(
            settled.routing.as_ref().expect("routed at dispatch").nudged.is_none(),
            "no nudge was claimed on the top tier: {settled:?}"
        );
        fixture.team.shutdown().await;
    }

    /// A lead message Laya reads a tier above the worker's nudges with
    /// reason `drift` (design §4.6).
    #[tokio::test]
    async fn drift_message_nudges_when_laya_reads_it_harder() {
        let (fixture, mut notices) = nudge_fixture_with(
            crate::mnemo2::dispatch_routing::RoutingSettings {
                mode: crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
                ..crate::mnemo2::dispatch_routing::RoutingSettings::default()
            },
            routing_answer(mnemo::agent::route_question::LABEL_HARD, 0.91),
            std::time::Duration::ZERO,
            vec![vec![
                mnemo::provider::LlmEvent::TextDelta { text: "done".to_string() },
                mnemo::provider::LlmEvent::Finish { reason: mnemo::provider::FinishReason::Stop },
            ]],
        )
        .await;
        let result = fixture.dispatch(Some("routine"), None).await;
        assert!(result.success, "{}", result.output);
        wait_worker_settled(&fixture.team, "routingworker").await;
        let record = fixture
            .team
            .message("routingworker", drift_message(), MessageSender::Lead)
            .await
            .expect("deliver the message");
        assert!(record.routing.is_some(), "the dispatched worker is routed");
        let row = eventually(|| async {
            fixture
                .rows()
                .into_iter()
                .find(|row| matches!(row, DispatchRoutingRow::Nudge { .. }))
        })
        .await;
        assert!(
            matches!(
                &row,
                DispatchRoutingRow::Nudge { reason, from_tier, to_tier, .. }
                    if reason == "drift" && from_tier == "routine" && to_tier == "high"
            ),
            "the drift nudge row: {row:?}"
        );
        let notice = wait_nudge_notice(&mut notices, "routingworker").await;
        assert_eq!(
            notice,
            "worker_id=routingworker Laya nudge: routingworker this got harder (Laya reads the message as hard, 0.91); consider team_resume with tier high."
        );
        assert_eq!(
            fixture.classifier.calls(),
            2,
            "dispatch and drift each asked Laya once"
        );
        fixture.team.shutdown().await;
    }

    /// Drift classification is detached: a slow Laya never delays the
    /// message, and the nudge still lands (design §4.6).
    #[tokio::test]
    async fn drift_never_blocks_the_message() {
        let (fixture, _notices) = nudge_fixture_with(
            crate::mnemo2::dispatch_routing::RoutingSettings {
                mode: crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
                ..crate::mnemo2::dispatch_routing::RoutingSettings::default()
            },
            routing_answer(mnemo::agent::route_question::LABEL_HARD, 0.91),
            std::time::Duration::from_secs(2),
            vec![vec![
                mnemo::provider::LlmEvent::TextDelta { text: "done".to_string() },
                mnemo::provider::LlmEvent::Finish { reason: mnemo::provider::FinishReason::Stop },
            ]],
        )
        .await;
        let result = fixture.dispatch(Some("routine"), None).await;
        assert!(result.success, "{}", result.output);
        wait_worker_settled(&fixture.team, "routingworker").await;
        let started = std::time::Instant::now();
        fixture
            .team
            .message("routingworker", drift_message(), MessageSender::Lead)
            .await
            .expect("the message returns without waiting on Laya");
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(1_000),
            "the message waited {elapsed:?} on drift classification"
        );
        assert_eq!(
            fixture.classifier.calls(),
            1,
            "only the dispatch classified before the message"
        );
        let row = eventually(|| async {
            fixture
                .rows()
                .into_iter()
                .find(|row| matches!(row, DispatchRoutingRow::Nudge { .. }))
        })
        .await;
        assert!(
            matches!(&row, DispatchRoutingRow::Nudge { reason, .. } if reason == "drift"),
            "the slow drift nudge still landed: {row:?}"
        );
        fixture.team.shutdown().await;
    }

    /// The deterministic review nudge: a second review round that still
    /// records a high finding nudges the lead to the next tier up, in
    /// every mode except off (design §4.6).
    #[tokio::test]
    async fn second_high_finding_review_nudges() {
        use crate::mnemo2::dispatch_routing::{save_routing_settings, RoutingSettings, RoutingMode};
        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        let config = tempfile::tempdir().expect("config");
        init_git(root.path());
        save_routing_settings(
            config.path(),
            &RoutingSettings {
                mode: RoutingMode::Shadow,
                ..RoutingSettings::default()
            },
        )
        .expect("save routing settings");
        let mut record = worker(root.path(), "nudgereview001");
        record.tier = Some("routine".into());
        // A failing streak on the record must never leak into a review nudge:
        // the review, not a tool, is what the lead is being asked to act on.
        record.tool_errors = vec![ToolErrorNote {
            tool: "shell".to_string(),
            output: "exit status 1".to_string(),
        }];
        record.routing = Some(WorkerRouting {
            laya_tier: Some("routine".into()),
            confidence: Some(0.62),
            cleared: true,
            lead_tier: Some("routine".into()),
            routed_by: "lead".into(),
            ran_tier: Some("routine".into()),
            nudged: None,
        });
        provision_agent_worktree(
            root.path().to_path_buf(),
            record.id.clone(),
            "main".to_string(),
        )
        .await
        .expect("provision the worker checkout");
        let team = open_test_team_with_config(
            root.path(),
            state.path(),
            vec![record.clone()],
            config.path(),
        )
        .await;
        team.set_catalog(
            vec![
                lead_model("scripted-difficult"),
                lead_model("scripted-average"),
                lead_model("scripted-simple"),
            ],
            tiers_dto! {
                difficult: vec!["test/scripted-difficult".into()],
                average: vec!["test/scripted-average".into()],
                simple: vec!["test/scripted-simple".into()],
            },
            Vec::new(),
        );
        let mut notices = team.subscribe_board();

        let first = team
            .review(&record.id, "first pass".into(), vec![open_finding("finding1")], evidence())
            .await
            .expect("record the first review");
        assert_eq!(first.review_rounds, 1, "{first:?}");
        assert!(
            first.routing.as_ref().expect("routed").nudged.is_none(),
            "one high finding in round one is not a pattern: {first:?}"
        );
        team.route_fix(&record.id, "finding1")
            .await
            .expect("route the finding back to the worker");
        team.stop_inner(&record.id)
            .await
            .expect("stop the fixer session");
        let second = team
            .review(&record.id, "second pass".into(), vec![open_finding("finding1")], evidence())
            .await
            .expect("record the second review");
        assert_eq!(second.review_rounds, 2, "{second:?}");
        assert_eq!(
            second.routing.as_ref().expect("routed").nudged.as_deref(),
            Some("high"),
            "the second high review round nudged: {second:?}"
        );
        let review_notice = wait_nudge_notice(&mut notices, &record.id).await;
        assert!(
            !review_notice.contains("Last tool errors"),
            "a review nudge never shows tool errors: {review_notice}"
        );
        assert!(
            !review_notice.contains("Read these first"),
            "{review_notice}"
        );
        let rows = std::fs::read_to_string(state.path().join("dispatch_routing.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str::<DispatchRoutingRow>(line).expect("routing row"))
            .collect::<Vec<_>>();
        assert!(
            matches!(
                rows.as_slice(),
                [DispatchRoutingRow::Nudge { reason, from_tier, to_tier, detail, .. }]
                    if reason == "review"
                        && from_tier == "routine"
                        && to_tier == "high"
                        && detail == "keeps a high finding after 2 review rounds"
            ),
            "the review nudge row: {rows:?}"
        );
        team.shutdown().await;
    }

    /// A nudged worker that finishes on the tier it was nudged from logs
    /// `nudge_ignored` (design §4.7).
    #[tokio::test]
    async fn ignored_nudge_is_logged_at_merge() {
        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        run_git(root.path(), &["checkout", "-b", "wt/mnemo"]);
        let mut worker = worker(root.path(), "00000003000000000003");
        worker.status = WorkerStatus::InReview;
        worker.plan_id = Some("plan0001".into());
        worker.plan_step = Some(0);
        worker.tier = Some("routine".into());
        worker.model = Some("test/scripted-average".into());
        worker.routing = Some(WorkerRouting {
            laya_tier: Some("routine".into()),
            confidence: Some(0.62),
            cleared: true,
            lead_tier: Some("routine".into()),
            routed_by: "lead".into(),
            ran_tier: Some("routine".into()),
            // Laya nudged difficult while the worker sat on average, and
            // no reroute ever took it.
            nudged: Some("high".into()),
        });
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
        let merged = team.merge(&worker.id).await.expect("merge the worker");
        assert!(matches!(merged.status, WorkerStatus::Merged), "{merged:?}");
        let rows: Vec<DispatchRoutingRow> =
            std::fs::read_to_string(state.path().join("dispatch_routing.jsonl"))
                .expect("the routing log")
                .lines()
                .map(|line| serde_json::from_str(line).expect("routing row"))
                .collect();
        assert!(
            matches!(
                rows.as_slice(),
                [DispatchRoutingRow::Outcome { status, final_tier, .. },
                 DispatchRoutingRow::NudgeIgnored { tier, .. }]
                    if status == "merged" && final_tier == "routine" && tier == "routine"
            ),
            "the merged outcome plus the ignored nudge: {rows:?}"
        );
        team.shutdown().await;
    }

    /// `nudges = false` disables all three kinds: strikes never fire, a
    /// hard-read message never reaches Laya, and a second high review
    /// round never nudges (design §4.6).
    #[tokio::test]
    async fn nudges_off_disables_all_three() {
        let settings = crate::mnemo2::dispatch_routing::RoutingSettings {
            mode: crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
            nudges: false,
            ..crate::mnemo2::dispatch_routing::RoutingSettings::default()
        };
        // Strikes and drift, on a live scripted worker.
        let responses = (1..=3)
            .map(|attempt| tool_call_batch(&format!("call-{attempt}"), "not_a_real_tool", &format!("{{\"attempt\":{attempt}}}")))
            .chain([vec![
                mnemo::provider::LlmEvent::TextDelta { text: "done".to_string() },
                mnemo::provider::LlmEvent::Finish { reason: mnemo::provider::FinishReason::Stop },
            ]])
            .collect::<Vec<_>>();
        let (fixture, _notices) = nudge_fixture_with(
            settings.clone(),
            routing_answer(mnemo::agent::route_question::LABEL_HARD, 0.91),
            std::time::Duration::ZERO,
            responses,
        )
        .await;
        let result = fixture.dispatch(Some("routine"), None).await;
        assert!(result.success, "{}", result.output);
        let settled = wait_worker_settled(&fixture.team, "routingworker").await;
        assert!(
            fixture
                .rows()
                .into_iter()
                .all(|row| !matches!(row, DispatchRoutingRow::Nudge { .. })),
            "strikes never nudged: {:?}",
            fixture.rows()
        );
        assert!(
            settled.routing.as_ref().expect("routed at dispatch").nudged.is_none(),
            "no strike nudge was claimed: {settled:?}"
        );
        let calls_after_turn = fixture.classifier.calls();
        fixture
            .team
            .message("routingworker", drift_message(), MessageSender::Lead)
            .await
            .expect("deliver the message");
        wait_worker_settled(&fixture.team, "routingworker").await;
        assert_eq!(
            fixture.classifier.calls(),
            calls_after_turn,
            "drift never asked Laya with nudges off"
        );
        assert!(
            fixture
                .rows()
                .into_iter()
                .all(|row| !matches!(row, DispatchRoutingRow::Nudge { .. })),
            "nothing nudged with nudges off: {:?}",
            fixture.rows()
        );
        fixture.team.shutdown().await;

        // The review nudge, on a record-based team with the same settings.
        use crate::mnemo2::dispatch_routing::save_routing_settings;
        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        let config = tempfile::tempdir().expect("config");
        init_git(root.path());
        save_routing_settings(config.path(), &settings).expect("save routing settings");
        let mut record = worker(root.path(), "nudgesoff0001");
        record.tier = Some("routine".into());
        record.routing = Some(WorkerRouting {
            laya_tier: Some("routine".into()),
            confidence: Some(0.62),
            cleared: true,
            lead_tier: Some("routine".into()),
            routed_by: "lead".into(),
            ran_tier: Some("routine".into()),
            nudged: None,
        });
        provision_agent_worktree(
            root.path().to_path_buf(),
            record.id.clone(),
            "main".to_string(),
        )
        .await
        .expect("provision the worker checkout");
        let team = open_test_team_with_config(
            root.path(),
            state.path(),
            vec![record.clone()],
            config.path(),
        )
        .await;
        team.set_catalog(
            vec![
                lead_model("scripted-difficult"),
                lead_model("scripted-average"),
                lead_model("scripted-simple"),
            ],
            tiers_dto! {
                difficult: vec!["test/scripted-difficult".into()],
                average: vec!["test/scripted-average".into()],
                simple: vec!["test/scripted-simple".into()],
            },
            Vec::new(),
        );
        team.review(&record.id, "first pass".into(), vec![open_finding("finding1")], evidence())
            .await
            .expect("record the first review");
        team.route_fix(&record.id, "finding1")
            .await
            .expect("route the finding back to the worker");
        team.stop_inner(&record.id)
            .await
            .expect("stop the fixer session");
        let second = team
            .review(&record.id, "second pass".into(), vec![open_finding("finding1")], evidence())
            .await
            .expect("record the second review");
        assert_eq!(second.review_rounds, 2, "{second:?}");
        assert!(
            second.routing.as_ref().expect("routed").nudged.is_none(),
            "the review never nudged: {second:?}"
        );
        let rows = std::fs::read_to_string(state.path().join("dispatch_routing.jsonl"))
            .unwrap_or_default();
        assert!(
            rows.lines().all(|line| !line.contains("\"nudge\"")),
            "nothing was written with nudges off: {rows}"
        );
        team.shutdown().await;
    }

    /// Review R1: a routing-settings save must reach a team that is already
    /// open. The Laya page's Save and the eval's Apply share
    /// `apply_routing_settings`, which writes the table and then reloads the
    /// live team — no restart, no unrelated model-settings save.
    #[tokio::test]
    async fn saved_routing_mode_reaches_the_open_team() {
        use crate::mnemo2::dispatch_routing::{load_routing_settings, RoutingMode, RoutingSettings};

        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        let config = tempfile::tempdir().expect("config");
        let team =
            open_test_team_with_config(root.path(), state.path(), Vec::new(), config.path()).await;
        assert_eq!(
            team.routing_mode(),
            RoutingMode::Off,
            "no [routing] table yet: off"
        );

        let shadow = RoutingSettings { mode: RoutingMode::Shadow, ..RoutingSettings::default() };
        crate::mnemo2::laya_settings::apply_routing_settings(config.path(), &shadow, Some(&team))
            .expect("save through the command's core");

        assert_eq!(
            team.routing_mode(),
            RoutingMode::Shadow,
            "the open team reloads the saved mode without a restart"
        );
        assert_eq!(
            load_routing_settings(config.path()).mode,
            RoutingMode::Shadow,
            "and the table is on disk for the next open"
        );
        team.shutdown().await;
    }

    #[tokio::test]
    async fn off_mode_never_asks_laya() {
        let fixture = routing_fixture(
            crate::mnemo2::dispatch_routing::RoutingMode::Off,
            routing_answer(mnemo::agent::route_question::LABEL_HARD, 0.91),
            true,
        )
        .await;
        let result = fixture.dispatch(Some("routine"), None).await;
        assert!(result.success, "{}", result.output);
        assert_eq!(fixture.classifier.calls(), 0);
        assert!(!fixture.log_path().exists());
        assert!(fixture
            .team
            .worker("routingworker")
            .await
            .expect("worker")
            .routing
            .is_none());
        fixture.team.shutdown().await;
    }

    #[tokio::test]
    async fn shadow_logs_layas_pick_and_runs_the_lead_tier() {
        let fixture = routing_fixture(
            crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
            routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.62),
            true,
        )
        .await;
        let result = fixture.dispatch(Some("routine"), None).await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("Laya would pick routine (0.62)."));
        let worker = fixture.team.worker("routingworker").await.expect("worker");
        assert_eq!(worker.tier.as_deref(), Some("routine"));
        let routing = worker.routing.expect("routing metadata");
        assert_eq!(routing.laya_tier.as_deref(), Some("routine"));
        assert_eq!(routing.lead_tier.as_deref(), Some("routine"));
        assert_eq!(routing.routed_by, "lead");
        assert_eq!(routing.confidence, Some(0.62));
        assert!(routing.cleared);
        let rows = fixture.rows();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(matches!(
            &rows[0],
            crate::mnemo2::dispatch_routing::DispatchRoutingRow::Route {
                tier: Some(tier),
                ran_tier: Some(ran),
                ..
            } if tier == "routine" && ran == "routine"
        ));
        let status = team_status_answer(&fixture.team, json!({})).await;
        assert_eq!(status["result"][0]["routing"]["laya_tier"], "routine");
        fixture.team.shutdown().await;
    }

    #[tokio::test]
    async fn shadow_override_is_logged_when_laya_cleared_elsewhere() {
        let fixture = routing_fixture(
            crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
            routing_answer(mnemo::agent::route_question::LABEL_HARD, 0.71),
            true,
        )
        .await;
        assert!(fixture.dispatch(Some("routine"), None).await.success);
        assert_eq!(fixture.classifier.calls(), 1);
        let rows = fixture.rows();
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert!(matches!(
            &rows[1],
            crate::mnemo2::dispatch_routing::DispatchRoutingRow::LeadOverride {
                laya_tier: Some(laya),
                lead_tier,
                ..
            } if laya == "high" && lead_tier == "routine"
        ));
        assert_eq!(
            fixture
                .team
                .worker("routingworker")
                .await
                .expect("worker")
                .routing
                .expect("routing")
                .routed_by,
            "lead_override"
        );
        fixture.team.shutdown().await;
    }

    #[tokio::test]
    async fn suggest_auto_uses_a_cleared_laya_tier() {
        let fixture = routing_fixture(
            crate::mnemo2::dispatch_routing::RoutingMode::Suggest,
            routing_answer(mnemo::agent::route_question::LABEL_HARD, 0.71),
            true,
        )
        .await;
        let result = fixture.dispatch(Some("auto"), None).await;
        assert!(result.success, "{}", result.output);
        let worker = fixture.team.worker("routingworker").await.expect("worker");
        assert_eq!(worker.tier.as_deref(), Some("high"));
        assert_eq!(worker.model.as_deref(), Some("test/scripted-difficult"));
        assert_eq!(worker.routing.expect("routing").routed_by, "laya");
        fixture.team.shutdown().await;
    }

    #[tokio::test]
    async fn suggest_auto_refuses_when_unsure() {
        let fixture = routing_fixture(
            crate::mnemo2::dispatch_routing::RoutingMode::Suggest,
            routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.39),
            true,
        )
        .await;
        let result = fixture.dispatch(Some("auto"), None).await;
        assert!(!result.success);
        assert_eq!(result.output, "Laya is unsure (0.39); pass a tier");
        assert!(matches!(
            fixture.rows().as_slice(),
            [crate::mnemo2::dispatch_routing::DispatchRoutingRow::Route {
                skip: None,
                ran_tier: None,
                ..
            }]
        ));
        fixture.team.shutdown().await;
    }

    #[tokio::test]
    async fn suggest_auto_refuses_when_laya_is_offline() {
        let fixture = routing_fixture(
            crate::mnemo2::dispatch_routing::RoutingMode::Suggest,
            None,
            false,
        )
        .await;
        let result = fixture.dispatch(Some("auto"), None).await;
        assert!(!result.success);
        assert_eq!(result.output, "Laya is not running; pass a tier");
        assert!(matches!(
            fixture.rows().as_slice(),
            [crate::mnemo2::dispatch_routing::DispatchRoutingRow::Route {
                skip: Some(reason),
                ..
            }] if reason == "offline"
        ));
        fixture.team.shutdown().await;
    }


    #[tokio::test]
    async fn auto_mode_falls_back_when_unsure() {
        let fixture = routing_fixture(
            crate::mnemo2::dispatch_routing::RoutingMode::Auto,
            routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.39),
            true,
        )
        .await;
        let result = fixture.dispatch(None, None).await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("Laya: unsure (0.39)."));
        let worker = fixture.team.worker("routingworker").await.expect("worker");
        assert_eq!(worker.tier.as_deref(), Some("routine"));
        assert_eq!(worker.routing.expect("routing").routed_by, "lead");
        assert!(matches!(
            fixture.rows().as_slice(),
            [crate::mnemo2::dispatch_routing::DispatchRoutingRow::Route {
                skip: None,
                ran_tier: Some(ran),
                ..
            }] if ran == "routine"
        ));
        fixture.team.shutdown().await;
    }

    #[tokio::test]
    async fn explicit_model_wins_in_auto_mode() {
        let fixture = routing_fixture(
            crate::mnemo2::dispatch_routing::RoutingMode::Auto,
            routing_answer(mnemo::agent::route_question::LABEL_HARD, 0.71),
            true,
        )
        .await;
        let result = fixture.dispatch(None, Some("test/scripted-simple")).await;
        assert!(result.success, "{}", result.output);
        let worker = fixture.team.worker("routingworker").await.expect("worker");
        assert_eq!(worker.model.as_deref(), Some("test/scripted-simple"));
        assert_eq!(worker.tier.as_deref(), Some("trivial"));
        assert_eq!(worker.routing.expect("routing").routed_by, "lead_override");
        assert_eq!(fixture.rows().len(), 2);
        fixture.team.shutdown().await;
    }

    #[tokio::test(start_paused = true)]
    async fn slow_laya_times_out_as_offline() {
        let fixture = routing_fixture_with(
            crate::mnemo2::dispatch_routing::RoutingSettings {
                mode: crate::mnemo2::dispatch_routing::RoutingMode::Auto,
                ..crate::mnemo2::dispatch_routing::RoutingSettings::default()
            },
            routing_answer(mnemo::agent::route_question::LABEL_HARD, 0.91),
            true,
            std::time::Duration::from_secs(5),
        )
        .await;
        let started = tokio::time::Instant::now();
        let result = fixture.dispatch(None, None).await;
        let elapsed = started.elapsed();
        assert!(result.success, "{}", result.output);
        assert!(elapsed >= std::time::Duration::from_secs(3), "{elapsed:?}");
        assert!(elapsed < std::time::Duration::from_secs(5), "{elapsed:?}");
        assert!(result.output.contains("Laya: offline."));
        assert!(matches!(
            fixture.rows().as_slice(),
            [crate::mnemo2::dispatch_routing::DispatchRoutingRow::Route {
                skip: Some(reason),
                ..
            }] if reason == "offline"
        ));
        fixture.team.shutdown().await;
    }

    #[tokio::test]
    async fn redispatch_does_not_classify_twice() {
        let fixture = routing_fixture(
            crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
            routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.62),
            true,
        )
        .await;
        assert!(fixture.dispatch(Some("routine"), None).await.success);
        let reused = fixture.dispatch(Some("routine"), None).await;
        assert!(reused.success, "{}", reused.output);
        assert!(reused.output.contains("\"reused\":true"));
        assert_eq!(fixture.classifier.calls(), 1);
        assert_eq!(fixture.rows().len(), 1);
        assert_eq!(
            fixture
                .team
                .worker("routingworker")
                .await
                .expect("worker")
                .routing
                .expect("stored routing")
                .confidence,
            Some(0.62)
        );
        fixture.team.shutdown().await;
    }

    #[tokio::test]
    async fn user_model_change_logs_a_user_reroute() {
        let fixture = routing_fixture(
            crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
            routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.62),
            true,
        )
        .await;
        assert!(fixture.dispatch(Some("routine"), None).await.success);
        // A pending nudge travels with the reroute row and leaves the record.
        fixture
            .team
            .update_worker("routingworker", |worker| {
                if let Some(routing) = worker.routing.as_mut() {
                    routing.nudged = Some("high".into());
                }
            })
            .await;
        fixture
            .team
            .change_model(
                "routingworker",
                Some("test/scripted-difficult".into()),
                None,
                ChangeSource::User,
            )
            .await
            .expect("the user moves the worker onto difficult");
        let rows = fixture.rows();
        assert_eq!(rows.len(), 2, "{rows:?}");
        match &rows[1] {
            DispatchRoutingRow::UserReroute {
                worker_id,
                from_tier,
                to_tier,
                from_model,
                to_model,
                nudged,
                ..
            } => {
                assert_eq!(worker_id, "routingworker");
                assert_eq!(from_tier, "routine");
                assert_eq!(to_tier, "high");
                assert_eq!(from_model.as_deref(), Some("test/scripted-average"));
                assert_eq!(to_model.as_deref(), Some("test/scripted-difficult"));
                assert!(*nudged, "the pending nudge rides the row");
            }
            other => panic!("a user reroute row is logged: {other:?}"),
        }
        let worker = fixture.team.worker("routingworker").await.expect("worker");
        assert_eq!(worker.reroutes.len(), 1);
        assert!(
            worker
                .routing
                .as_ref()
                .is_some_and(|routing| routing.nudged.is_none()),
            "the nudge leaves the record: {worker:?}"
        );
        fixture.team.shutdown().await;
    }

    #[tokio::test]
    async fn lead_resume_onto_another_tier_logs_a_lead_reroute() {
        let fixture = routing_fixture(
            crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
            routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.62),
            true,
        )
        .await;
        assert!(fixture.dispatch(Some("routine"), None).await.success);
        let resumed = fixture
            .team
            .handle_lead_host_tool(
                "team_resume",
                json!({"worker_id": "routingworker", "tier": "high"}),
            )
            .await;
        assert!(resumed.success, "{}", resumed.output);
        let worker = fixture.team.worker("routingworker").await.expect("worker");
        assert_eq!(worker.tier.as_deref(), Some("high"), "{worker:?}");
        let rows = fixture.rows();
        assert_eq!(rows.len(), 2, "{rows:?}");
        match &rows[1] {
            DispatchRoutingRow::LeadReroute {
                worker_id,
                from_tier,
                to_tier,
                nudged,
                ..
            } => {
                assert_eq!(worker_id, "routingworker");
                assert_eq!(from_tier, "routine");
                assert_eq!(to_tier, "high");
                assert!(!*nudged);
            }
            other => panic!("a lead reroute row is logged: {other:?}"),
        }
        assert_eq!(worker.reroutes.len(), 1);
        fixture.team.shutdown().await;
    }

    #[tokio::test]
    async fn same_tier_switch_logs_without_counting_as_override() {
        let fixture = routing_fixture(
            crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
            routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.62),
            true,
        )
        .await;
        fixture.team.set_catalog(
            vec![
                lead_model("scripted-difficult"),
                lead_model("scripted-average"),
                lead_model("scripted-average2"),
                lead_model("scripted-simple"),
            ],
            tiers_dto! {
                difficult: vec!["test/scripted-difficult".into()],
                average: vec![
                    "test/scripted-average".into(),
                    "test/scripted-average2".into(),
                ],
                simple: vec!["test/scripted-simple".into()],
            },
            Vec::new(),
        );
        assert!(fixture.dispatch(Some("routine"), None).await.success);
        fixture
            .team
            .change_model(
                "routingworker",
                Some("test/scripted-average2".into()),
                None,
                ChangeSource::User,
            )
            .await
            .expect("the user picks another model inside the average tier");
        let rows = fixture.rows();
        match &rows[1] {
            DispatchRoutingRow::UserReroute { from_tier, to_tier, .. } => {
                assert_eq!(from_tier, "routine");
                assert_eq!(
                    to_tier, "routine",
                    "a same-tier switch logs equal tiers"
                );
            }
            other => panic!("a reroute row is still logged: {other:?}"),
        }
        // Equal tiers never count as an override: there is no effective
        // reroute, so with two fix rounds the merged label follows the
        // no-reroute rule (the tier above, weak) instead of the reroute
        // rule (average, strong).
        let worker = fixture.team.worker("routingworker").await.expect("worker");
        let (label, weak) = outcome_label(&worker.reroutes, "merged", "routine", 2, 0)
            .expect("a label still derives");
        assert_eq!(label, "medium");
        assert!(weak);
        fixture.team.shutdown().await;
    }

    #[tokio::test]
    async fn merge_appends_an_outcome_with_the_derived_label() {
        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        run_git(root.path(), &["checkout", "-b", "wt/mnemo"]);
        let mut worker = worker(root.path(), "00000002000000000002");
        worker.status = WorkerStatus::InReview;
        worker.plan_id = Some("plan0001".into());
        worker.plan_step = Some(0);
        worker.tier = Some("routine".into());
        worker.model = Some("test/scripted-average".into());
        worker.routing = Some(WorkerRouting {
            laya_tier: Some("routine".into()),
            confidence: Some(0.62),
            cleared: true,
            lead_tier: Some("routine".into()),
            routed_by: "lead".into(),
            ran_tier: Some("routine".into()),
            nudged: None,
        });
        // The worker was rerouted up to difficult before its review closed.
        worker.reroutes = vec![DispatchRoutingRow::LeadReroute {
            ts: 1,
            worker_id: worker.id.clone(),
            plan_id: "plan0001".into(),
            step: 1,
            from_tier: "routine".into(),
            to_tier: "high".into(),
            from_model: Some("test/scripted-average".into()),
            to_model: Some("test/scripted-difficult".into()),
            status: "idle".into(),
            review_rounds: 0,
            fix_rounds: 0,
            nudged: false,
        }];
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
        let merged = team.merge(&worker.id).await.expect("merge the worker");
        assert!(matches!(merged.status, WorkerStatus::Merged), "{merged:?}");
        let rows: Vec<DispatchRoutingRow> =
            std::fs::read_to_string(state.path().join("dispatch_routing.jsonl"))
                .expect("the routing log")
                .lines()
                .map(|line| serde_json::from_str(line).expect("routing row"))
                .collect();
        // The reroute up to difficult, merged with no fix rounds and no high
        // findings, derives the strong difficult label (design §4.8 rule 2).
        assert!(
            matches!(
                rows.as_slice(),
                [DispatchRoutingRow::Outcome {
                    worker_id,
                    plan_id,
                    status,
                    final_tier,
                    label: Some(label),
                    review_rounds,
                    fix_rounds,
                    ..
                }]
                    if worker_id == "00000002000000000002"
                        && plan_id == "plan0001"
                        && status == "merged"
                        && final_tier == "high"
                        && label == "high"
                        && *review_rounds == 0
                        && *fix_rounds == 0
            ),
            "the merged outcome row: {rows:?}"
        );
        team.shutdown().await;
    }

    #[tokio::test]
    async fn failed_worker_outcome_has_no_label() {
        // Routing on, so the worker carries a decision and its terminal
        // failure is logged (with routing off nothing is written at all).
        let fixture = routing_fixture(
            crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
            routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.62),
            true,
        )
        .await;
        // A forced spec the engine cannot resolve to an endpoint: the
        // dispatch records the worker, its session fails to spawn, and the
        // worker lands Failed (the same path as a real host failure).
        fixture.team.set_catalog(
            vec![LeadModel {
                endpoint: "broken".into(),
                model: "broken".into(),
                spec: "broken".into(),
                is_default: false,
                reasoning_efforts: Vec::new(),
                default_effort: None,
            }],
            mnemo::headless::TiersDto::default(),
            Vec::new(),
        );
        let result = fixture.dispatch(Some("routine"), Some("broken")).await;
        assert!(!result.success, "{result:?}");
        let worker = fixture
            .team
            .worker("routingworker")
            .await
            .expect("the failed worker is tracked");
        assert!(
            matches!(worker.status, WorkerStatus::Failed),
            "{worker:?}"
        );
        // Failed-and-stopped is terminal (design §4.7): the dispatch's route
        // row is followed by one outcome row, and no label — the worker never
        // merged.
        let rows = fixture.rows();
        assert!(
            matches!(
                rows.last(),
                Some(DispatchRoutingRow::Outcome {
                    worker_id,
                    status,
                    label: None,
                    ..
                })
                    if worker_id == "routingworker" && status == "failed"
            ),
            "one unlabeled outcome row: {rows:?}"
        );
        fixture.team.shutdown().await;
    }

    #[tokio::test]
    async fn reused_dispatch_line_keeps_the_dispatch_time_tier() {
        let fixture = routing_fixture(
            crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
            routing_answer(mnemo::agent::route_question::LABEL_HARD, 0.91),
            true,
        )
        .await;
        // Laya clears difficult, the lead passes average: an override that
        // runs average.
        let first = fixture.dispatch(Some("routine"), None).await;
        assert!(first.success, "{}", first.output);
        assert!(
            first
                .output
                .contains("you chose routine; logged as an override."),
            "{}",
            first.output
        );
        // The worker's tier changes afterwards, on the same model: the
        // record's mutable tier now reads 'difficult' while the dispatch ran
        // average. (A switch that also moved the model is refused by the
        // reuse guard, correctly — only the tier moved here.)
        fixture
            .team
            .record_tier("routingworker", "high")
            .await;
        let worker = fixture.team.worker("routingworker").await.expect("worker");
        assert_eq!(worker.tier.as_deref(), Some("high"), "{worker:?}");
        // An idempotent re-dispatch reports the dispatch-time ran tier, not
        // the tier the worker was rerouted onto (audit finding M1).
        let reused = fixture.dispatch(Some("routine"), None).await;
        assert!(reused.success, "{}", reused.output);
        assert!(
            reused
                .output
                .contains("you chose routine; logged as an override."),
            "the reused dispatch keeps the dispatch-time tier: {}",
            reused.output
        );
        assert!(
            !reused.output.contains("you ran difficult (requested average)"),
            "{}",
            reused.output
        );
        fixture.team.shutdown().await;
    }

    #[tokio::test]
    async fn off_mode_model_change_logs_the_user_reroute() {
        let fixture = routing_fixture(
            crate::mnemo2::dispatch_routing::RoutingMode::Off,
            routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.62),
            false,
        )
        .await;
        assert!(fixture.dispatch(Some("routine"), None).await.success);
        // No hand reroute yet: off mode keeps the log absent.
        assert!(!fixture.log_path().exists());
        fixture
            .team
            .change_model(
                "routingworker",
                Some("test/scripted-difficult".into()),
                None,
                ChangeSource::User,
            )
            .await
            .expect("the user moves the worker");
        // A hand-moved model is a learning even with routing off (decided
        // 2026-11-04): exactly one user reroute row lands in the log, and
        // the record's tier is the destination tier — final_tier and
        // team_status agree instead of showing null.
        let rows = fixture.rows();
        assert_eq!(rows.len(), 1, "{rows:?}");
        match &rows[0] {
            DispatchRoutingRow::UserReroute {
                worker_id,
                from_tier,
                to_tier,
                from_model,
                to_model,
                ..
            } => {
                assert_eq!(worker_id, "routingworker");
                assert_eq!(from_tier, "routine");
                assert_eq!(to_tier, "high");
                assert_eq!(from_model.as_deref(), Some("test/scripted-average"));
                assert_eq!(to_model.as_deref(), Some("test/scripted-difficult"));
            }
            other => panic!("a user reroute row is logged: {other:?}"),
        }
        let worker = fixture.team.worker("routingworker").await.expect("worker");
        assert_eq!(worker.reroutes.len(), 1, "{worker:?}");
        assert_eq!(worker.tier.as_deref(), Some("high"), "{worker:?}");
        assert_eq!(final_tier(&worker), "high");
        let status = team_status_answer(&fixture.team, json!({})).await;
        assert_eq!(status["result"][0]["tier"], "high");
        fixture.team.shutdown().await;
    }

    #[tokio::test]
    async fn off_mode_unrerouted_merge_writes_no_outcome_row() {
        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        run_git(root.path(), &["checkout", "-b", "wt/mnemo"]);
        let mut worker = worker(root.path(), "00000002000000000003");
        worker.status = WorkerStatus::InReview;
        worker.plan_id = Some("plan0001".into());
        worker.plan_step = Some(0);
        worker.tier = Some("routine".into());
        worker.model = Some("test/scripted-average".into());
        // No dispatch decision and no hand reroute: this worker ran with
        // routing off and was never moved by a hand, so it stays off the
        // calibration log.
        worker.routing = None;
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
        let merged = team.merge(&worker.id).await.expect("merge the worker");
        assert!(matches!(merged.status, WorkerStatus::Merged), "{merged:?}");
        assert!(
            !state.path().join("dispatch_routing.jsonl").exists(),
            "off mode writes no outcome row for a worker with no reroute"
        );
        team.shutdown().await;
    }

    #[tokio::test]
    async fn off_mode_rerouted_merge_writes_the_outcome_row() {
        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        run_git(root.path(), &["checkout", "-b", "wt/mnemo"]);
        let mut worker = worker(root.path(), "00000002000000000004");
        worker.status = WorkerStatus::InReview;
        worker.plan_id = Some("plan0001".into());
        worker.plan_step = Some(0);
        worker.tier = Some("routine".into());
        worker.model = Some("test/scripted-average".into());
        // No dispatch decision: this worker ran with routing off — but the
        // user moved it to difficult by hand, and a hand reroute is a
        // learning in every mode (decided 2026-11-04), so its terminal
        // outcome row is calibration data.
        worker.routing = None;
        worker.reroutes = vec![DispatchRoutingRow::UserReroute {
            ts: 1,
            worker_id: worker.id.clone(),
            plan_id: "plan0001".into(),
            step: 1,
            from_tier: "routine".into(),
            to_tier: "high".into(),
            from_model: Some("test/scripted-average".into()),
            to_model: Some("test/scripted-difficult".into()),
            status: "idle".into(),
            review_rounds: 0,
            fix_rounds: 0,
            nudged: false,
        }];
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
        let merged = team.merge(&worker.id).await.expect("merge the worker");
        assert!(matches!(merged.status, WorkerStatus::Merged), "{merged:?}");
        let rows: Vec<DispatchRoutingRow> =
            std::fs::read_to_string(state.path().join("dispatch_routing.jsonl"))
                .expect("the routing log")
                .lines()
                .map(|line| serde_json::from_str(line).expect("routing row"))
                .collect();
        // The hand reroute makes the off-mode worker calibration data: its
        // merged outcome row is written, labelled difficult by the reroute
        // rule (design §4.8 rule 2).
        assert!(
            matches!(
                rows.as_slice(),
                [DispatchRoutingRow::Outcome {
                    worker_id,
                    status,
                    final_tier,
                    label: Some(label),
                    ..
                }]
                    if worker_id == "00000002000000000004"
                        && status == "merged"
                        && final_tier == "high"
                        && label == "high"
            ),
            "the rerouted worker's outcome row: {rows:?}"
        );
        team.shutdown().await;
    }

    #[tokio::test]
    async fn routed_worker_still_logs_after_routing_is_turned_off() {
        let fixture = routing_fixture(
            crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
            routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.62),
            true,
        )
        .await;
        assert!(fixture.dispatch(Some("routine"), None).await.success);
        // Routing is switched off after the worker was routed: the worker's
        // own decision still makes it calibration data.
        *fixture.team.routing.write().unwrap_or_else(|e| e.into_inner()) =
            crate::mnemo2::dispatch_routing::RoutingSettings::default();
        fixture
            .team
            .change_model(
                "routingworker",
                Some("test/scripted-difficult".into()),
                None,
                ChangeSource::User,
            )
            .await
            .expect("the user moves the routed worker");
        let rows = fixture.rows();
        assert!(
            matches!(
                rows.as_slice(),
                [_, DispatchRoutingRow::UserReroute { worker_id, .. }]
                    if worker_id == "routingworker"
            ),
            "a routed worker still logs its reroute: {rows:?}"
        );
        fixture.team.shutdown().await;
    }

    #[tokio::test]
    async fn audit_dispatch_is_never_classified() {
        let fixture = routing_fixture(
            crate::mnemo2::dispatch_routing::RoutingMode::Off,
            routing_answer(mnemo::agent::route_question::LABEL_HARD, 0.91),
            true,
        )
        .await;
        assert!(fixture.dispatch(Some("routine"), None).await.success);
        fixture
            .team
            .stop("routingworker")
            .await
            .expect("stop the audit target");
        // Routing off and the worker was dispatched without a decision: the
        // stop writes nothing at all.
        assert!(
            fixture.rows().is_empty(),
            "off mode writes no stop outcome row"
        );
        *fixture.team.routing.write().unwrap_or_else(|e| e.into_inner()) =
            crate::mnemo2::dispatch_routing::RoutingSettings {
                mode: crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
                ..crate::mnemo2::dispatch_routing::RoutingSettings::default()
            };
        let result = fixture
            .team
            .handle_lead_host_tool(
                "team_audit_dispatch",
                json!({
                    "worker_id": "routingworker",
                    "reviewer_id": "routingaudit",
                    "plan_id": fixture.plan_id,
                    "step": 1,
                    "brief": "Audit the implementation independently against the complete step specification.",
                }),
            )
            .await;
        assert!(result.success, "{}", result.output);
        assert_eq!(fixture.classifier.calls(), 0);
        // The audit dispatch classifies nothing and logs nothing: not even a
        // shadow-mode row appears for it.
        assert!(
            !fixture.log_path().exists(),
            "an audit dispatch adds no routing row"
        );
        fixture.team.shutdown().await;
    }

    #[tokio::test]
    async fn schema_offers_auto_only_in_suggest_and_auto() {
        for (mode, offers_auto) in [
            (crate::mnemo2::dispatch_routing::RoutingMode::Off, false),
            (crate::mnemo2::dispatch_routing::RoutingMode::Shadow, false),
            (crate::mnemo2::dispatch_routing::RoutingMode::Suggest, true),
            (crate::mnemo2::dispatch_routing::RoutingMode::Auto, true),
        ] {
            let root = tempfile::tempdir().expect("workspace");
            let state = tempfile::tempdir().expect("state");
            init_git(root.path());
            let team = open_test_team(root.path(), state.path(), Vec::new()).await;
            team.set_catalog(
                vec![lead_model("scripted-model")],
                tiers_dto! {
                    simple: vec!["test/scripted-model".into()],
                    average: vec!["test/scripted-model".into()],
                    difficult: vec!["test/scripted-model".into()],
                },
                Vec::new(),
            );
            *team.routing.write().unwrap_or_else(|e| e.into_inner()) =
                crate::mnemo2::dispatch_routing::RoutingSettings {
                    mode,
                    ..crate::mnemo2::dispatch_routing::RoutingSettings::default()
                };
            let tools = team.lead_host_tools();
            let dispatch = tools.iter().find(|tool| tool.name == "team_dispatch").expect("dispatch");
            let tiers = dispatch.parameters["properties"]["tier"]["enum"]
                .as_array()
                .expect("tier enum");
            assert_eq!(tiers.contains(&json!("auto")), offers_auto, "{mode:?}");
        }
    }

    #[tokio::test]
    async fn tier_is_optional_only_in_auto_mode() {
        for (mode, required) in [
            (crate::mnemo2::dispatch_routing::RoutingMode::Off, true),
            (crate::mnemo2::dispatch_routing::RoutingMode::Shadow, true),
            (crate::mnemo2::dispatch_routing::RoutingMode::Suggest, true),
            (crate::mnemo2::dispatch_routing::RoutingMode::Auto, false),
        ] {
            let root = tempfile::tempdir().expect("workspace");
            let state = tempfile::tempdir().expect("state");
            init_git(root.path());
            let team = open_test_team(root.path(), state.path(), Vec::new()).await;
            team.set_catalog(
                vec![lead_model("scripted-model")],
                tiers_dto! {
                    simple: vec!["test/scripted-model".into()],
                    average: vec!["test/scripted-model".into()],
                    difficult: vec!["test/scripted-model".into()],
                },
                Vec::new(),
            );
            *team.routing.write().unwrap_or_else(|e| e.into_inner()) =
                crate::mnemo2::dispatch_routing::RoutingSettings {
                    mode,
                    ..crate::mnemo2::dispatch_routing::RoutingSettings::default()
                };
            let tools = team.lead_host_tools();
            let dispatch = tools.iter().find(|tool| tool.name == "team_dispatch").expect("dispatch");
            let required_fields = dispatch.parameters["required"]
                .as_array()
                .expect("required fields");
            assert_eq!(required_fields.contains(&json!("tier")), required, "{mode:?}");
        }
    }


    #[tokio::test]
    async fn suggest_auto_refuses_when_laya_returns_no_answer() {
        let fixture = routing_fixture(
            crate::mnemo2::dispatch_routing::RoutingMode::Suggest,
            None,
            true,
        )
        .await;
        let result = fixture.dispatch(Some("auto"), None).await;
        assert!(!result.success);
        assert_eq!(result.output, "Laya is not running; pass a tier");
        assert_eq!(fixture.classifier.calls(), 1);
        assert!(matches!(
            fixture.rows().as_slice(),
            [crate::mnemo2::dispatch_routing::DispatchRoutingRow::Route {
                skip: Some(reason),
                ..
            }] if reason == "offline"
        ));
        fixture.team.shutdown().await;
    }

    #[tokio::test]
    async fn classification_happens_outside_merge_lock() {
        let fixture = std::sync::Arc::new(
            routing_fixture(
                crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
                routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.62),
                true,
            )
            .await,
        );
        let merge_guard = fixture.team.merge_lock.lock().await;
        let dispatch_fixture = std::sync::Arc::clone(&fixture);
        let dispatch = tokio::spawn(async move {
            dispatch_fixture.dispatch(Some("routine"), None).await
        });
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while fixture.classifier.calls() == 0 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("classification runs while merge_lock is held elsewhere");
        assert!(!dispatch.is_finished(), "dispatch still waits for merge_lock");
        drop(merge_guard);
        let result = dispatch.await.expect("dispatch task");
        assert!(result.success, "{}", result.output);
        fixture.team.shutdown().await;
    }

    #[tokio::test(start_paused = true)]
    async fn permit_wait_counts_toward_laya_timeout() {
        let fixture = routing_fixture(
            crate::mnemo2::dispatch_routing::RoutingMode::Auto,
            routing_answer(mnemo::agent::route_question::LABEL_HARD, 0.91),
            true,
        )
        .await;
        let _permit = fixture.team.routing_permit.acquire().await.expect("permit");
        let request = DispatchRequest {
            brief: "Use the complete step specification to choose this worker's model tier".into(),
            tier: None,
            dispatch_routing: true,
            source: DispatchSource::Ipc,
            model: None,
            reasoning_effort: None,
            worker_id: Some("permitwait".into()),
            plan: Some(PlanRef {
                plan_id: fixture.plan_id.clone(),
                step: 0,
            }),
            review_risk: None,
            read_back: false,
        };
        let routed = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            fixture.team.route_implementation_dispatch(
                &request,
                "Implement the requested routing behavior in src-tauri/src/mnemo2/team.rs and verify it with focused regression tests",
                &request.brief,
            ),
        )
        .await
        .expect("the permit wait is inside Laya's 3 s timeout")
        .expect("routing decision");
        assert_eq!(routed.tier.as_deref(), Some("routine"));
        assert_eq!(fixture.classifier.calls(), 0);
        assert!(matches!(
            routed.rows.as_slice(),
            [crate::mnemo2::dispatch_routing::DispatchRoutingRow::Route {
                skip: Some(reason),
                ..
            }] if reason == "offline"
        ));
        fixture.team.shutdown().await;
    }

    #[tokio::test]
    async fn shadow_without_tiers_keeps_the_default_model() {
        let fixture = routing_fixture(
            crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
            routing_answer(mnemo::agent::route_question::LABEL_HARD, 0.91),
            true,
        )
        .await;
        fixture.team.set_catalog(
            vec![lead_model("scripted-model")],
            mnemo::headless::TiersDto::default(),
            Vec::new(),
        );
        let result = fixture.dispatch(None, None).await;
        assert!(result.success, "{}", result.output);
        let worker = fixture.team.worker("routingworker").await.expect("worker");
        assert_eq!(worker.model, None);
        assert_eq!(worker.tier, None);
        assert_eq!(fixture.classifier.calls(), 0);
        fixture.team.shutdown().await;
    }

    #[tokio::test]
    async fn team_status_omits_routing_when_none() {
        let fixture = routing_fixture(
            crate::mnemo2::dispatch_routing::RoutingMode::Off,
            None,
            false,
        )
        .await;
        assert!(fixture.dispatch(Some("routine"), None).await.success);
        let compact = team_status_answer(&fixture.team, json!({})).await;
        assert!(compact["result"][0].get("routing").is_none());
        let detailed = team_status_answer(&fixture.team, json!({"detail": true})).await;
        assert!(detailed["result"][0].get("routing").is_none());
        fixture.team.shutdown().await;
    }


    #[tokio::test]
    async fn auto_fallback_without_that_tier_folds_down() {
        let fixture = routing_fixture(
            crate::mnemo2::dispatch_routing::RoutingMode::Auto,
            routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.39),
            true,
        )
        .await;
        fixture.team.set_catalog(
            vec![lead_model("scripted-model")],
            tiers_dto! {
                simple: vec!["test/scripted-model".into()],
                average: Vec::new(),
                difficult: vec!["test/scripted-model".into()],
            },
            Vec::new(),
        );
        let result = fixture.dispatch(None, None).await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("routine has no model; ran on trivial"));
        let worker = fixture.team.worker("routingworker").await.expect("worker");
        assert_eq!(worker.model.as_deref(), Some("test/scripted-model"));
        assert_eq!(worker.tier.as_deref(), Some("trivial"));
        let routing = worker.routing.expect("routing");
        assert_eq!(routing.routed_by, "lead");
        assert_eq!(routing.lead_tier.as_deref(), Some("routine"));
        assert_eq!(routing.ran_tier.as_deref(), Some("trivial"));
        assert_eq!(fixture.classifier.calls(), 1);
        assert!(matches!(
            fixture.rows().as_slice(),
            [crate::mnemo2::dispatch_routing::DispatchRoutingRow::Route {
                skip: None,
                lead_tier: Some(lead_tier),
                ran_tier: Some(ran_tier),
                ..
            }] if lead_tier == "routine" && ran_tier == "trivial"
        ));
        fixture.team.shutdown().await;
    }

    #[tokio::test]
    async fn racing_duplicate_dispatch_logs_one_route_row() {
        let fixture = routing_fixture_with(
            crate::mnemo2::dispatch_routing::RoutingSettings {
                mode: crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
                ..crate::mnemo2::dispatch_routing::RoutingSettings::default()
            },
            routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.62),
            true,
            std::time::Duration::from_millis(50),
        )
        .await;
        let (first, second) = tokio::join!(
            fixture.dispatch(Some("routine"), None),
            fixture.dispatch(Some("routine"), None)
        );
        assert!(first.success, "{}", first.output);
        assert!(second.success, "{}", second.output);
        assert_eq!(fixture.classifier.calls(), 2);
        assert_eq!(fixture.rows().len(), 1, "exactly one Route row is committed");
        fixture.team.shutdown().await;
    }


    #[tokio::test]
    async fn no_free_slot_writes_no_route_row() {
        let fixture = routing_fixture(
            crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
            routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.62),
            true,
        )
        .await;
        fixture
            .team
            .persistence
            .slots
            .store(0, Ordering::SeqCst);
        let result = fixture.dispatch(Some("routine"), None).await;
        assert!(!result.success);
        assert_eq!(result.output, "all 0 worker slots are busy");
        assert_eq!(fixture.classifier.calls(), 1);
        assert!(!fixture.log_path().exists());
        fixture.team.shutdown().await;
    }

    #[tokio::test]
    async fn provisioning_failure_writes_no_route_row() {
        let fixture = routing_fixture(
            crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
            routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.62),
            true,
        )
        .await;
        let git = fixture._root.path().join(".git");
        let parked = fixture._root.path().join(".git-parked");
        std::fs::rename(&git, &parked).expect("park git metadata");
        let result = fixture.dispatch(Some("routine"), None).await;
        assert!(!result.success);
        assert!(result.output.contains("not a usable Git checkout"));
        assert_eq!(fixture.classifier.calls(), 1);
        assert!(!fixture.log_path().exists());
        std::fs::rename(parked, git).expect("restore git metadata");
        fixture.team.shutdown().await;
    }


    #[test]
    fn dispatch_routing_line_truth_table() {
        let routing = WorkerRouting {
            laya_tier: Some("high".into()),
            confidence: Some(0.71),
            cleared: true,
            lead_tier: Some("routine".into()),
            routed_by: "lead_override".into(),
            ran_tier: Some("routine".into()),
            nudged: None,
        };
        assert_eq!(
            dispatch_routing_line(RoutingMode::Shadow, Some(&routing)).as_deref(),
            Some("Laya: high (0.71) — you chose routine; logged as an override.")
        );
        // The dispatch-time ran tier decides the override line (audit finding
        // M1): a reroute after dispatch never rewrites what this reports.
        let routing = WorkerRouting {
            ran_tier: Some("high".into()),
            ..routing
        };
        assert_eq!(
            dispatch_routing_line(RoutingMode::Shadow, Some(&routing)).as_deref(),
            Some(
                "Laya: high (0.71) — you ran high (requested routine); logged as an override."
            )
        );
        let routing = WorkerRouting {
            laya_tier: Some("routine".into()),
            confidence: Some(0.62),
            lead_tier: Some("routine".into()),
            routed_by: "lead".into(),
            ..routing
        };
        assert_eq!(
            dispatch_routing_line(RoutingMode::Shadow, Some(&routing)).as_deref(),
            Some("Laya would pick routine (0.62).")
        );
        assert_eq!(
            dispatch_routing_line(RoutingMode::Suggest, Some(&routing)).as_deref(),
            Some("Laya: routine (0.62) — matches your tier.")
        );
        assert_eq!(dispatch_routing_line(RoutingMode::Off, Some(&routing)), None);
        let routing = WorkerRouting {
            routed_by: "laya".into(),
            lead_tier: None,
            ..routing
        };
        assert_eq!(
            dispatch_routing_line(RoutingMode::Auto, Some(&routing)).as_deref(),
            Some("Laya: routed to routine (0.62).")
        );
        let routing = WorkerRouting {
            confidence: Some(0.39),
            cleared: false,
            routed_by: "lead".into(),
            lead_tier: Some("routine".into()),
            ..routing
        };
        assert_eq!(
            dispatch_routing_line(RoutingMode::Auto, Some(&routing)).as_deref(),
            Some("Laya: unsure (0.39).")
        );
        let routing = WorkerRouting {
            laya_tier: None,
            confidence: None,
            ..routing
        };
        assert_eq!(
            dispatch_routing_line(RoutingMode::Shadow, Some(&routing)).as_deref(),
            Some("Laya: offline.")
        );
    }

    #[tokio::test]
    async fn suggest_non_auto_without_tiers_uses_default_model() {
        let fixture = routing_fixture(
            crate::mnemo2::dispatch_routing::RoutingMode::Suggest,
            None,
            false,
        )
        .await;
        fixture.team.set_catalog(
            vec![lead_model("scripted-model")],
            mnemo::headless::TiersDto::default(),
            Vec::new(),
        );
        let result = fixture.dispatch(None, None).await;
        assert!(result.success, "{}", result.output);
        let worker = fixture.team.worker("routingworker").await.expect("worker");
        assert_eq!(worker.model, None);
        assert_eq!(worker.tier, None);
        fixture.team.shutdown().await;
    }

    #[tokio::test]
    async fn gated_answer_has_no_skip_reason() {
        let fixture = routing_fixture(
            crate::mnemo2::dispatch_routing::RoutingMode::Auto,
            routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.39),
            true,
        )
        .await;
        assert!(fixture.dispatch(None, None).await.success);
        assert!(matches!(
            fixture.rows().as_slice(),
            [crate::mnemo2::dispatch_routing::DispatchRoutingRow::Route {
                skip: None,
                cleared: false,
                confidence,
                ..
            }] if (*confidence - 0.39).abs() < f64::EPSILON
        ));
        fixture.team.shutdown().await;
    }

    #[tokio::test]
    async fn off_mode_schema_uses_the_ladder_without_auto() {
        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        let team = open_test_team(root.path(), state.path(), Vec::new()).await;
        team.set_catalog(
            vec![lead_model("scripted-model")],
            tiers_dto! {
                simple: vec!["test/scripted-model".into()],
                average: vec!["test/scripted-model".into()],
                difficult: vec!["test/scripted-model".into()],
            },
            Vec::new(),
        );
        let tools = team.lead_host_tools();
        let dispatch = tools.iter().find(|tool| tool.name == "team_dispatch").expect("dispatch");
        let tier = &dispatch.parameters["properties"]["tier"];
        assert_eq!(
            tier["enum"],
            json!(["trivial", "routine", "medium", "high", "escalate"])
        );
        let description = tier["description"].as_str().expect("tier description");
        assert!(description.contains("trivial (Trivial)"));
        assert!(description.contains("routine (Routine)"));
        assert!(description.contains("medium (Medium)"));
        assert!(description.contains("no model; folds down to routine"));
        assert!(description.contains("high (High)"));
        assert!(description.contains("escalation only, never a first pick"));
        assert!(!description.contains("\nauto -"));
        assert_eq!(
            dispatch.parameters["properties"]["model"]["enum"],
            json!(["test/scripted-model"])
        );
        assert!(dispatch.parameters["required"]
            .as_array()
            .is_some_and(|required| required.contains(&json!("tier"))));
    }


    #[tokio::test]
    async fn ipc_dispatch_on_plan_logs_a_shadow_route_row() {
        let fixture = routing_fixture(
            crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
            routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.62),
            true,
        )
        .await;
        let (worker, reused) = fixture
            .team
            .dispatch_on_plan(
                "Implement the IPC-dispatched step and run its regression tests".into(),
                Some("test/scripted-average".into()),
                None,
                Some("ipcworker".into()),
                None,
                false,
                &fixture.plan_id,
                1,
            )
            .await
            .expect("IPC dispatch");
        assert!(!reused);
        assert_eq!(fixture.classifier.calls(), 1);
        assert_eq!(fixture.rows().len(), 1);
        let routing = worker.routing.expect("IPC worker routing");
        assert_eq!(routing.laya_tier.as_deref(), Some("routine"));
        assert_eq!(routing.lead_tier.as_deref(), Some("routine"));
        assert_eq!(routing.routed_by, "lead");
        fixture.team.shutdown().await;
    }


    #[tokio::test]
    async fn dispatch_uses_every_ladder_tier_and_refuses_legacy_ids() {
        for (tier, model, ran_tier, fold_note) in [
            ("trivial", "test/scripted-simple", "trivial", None),
            ("routine", "test/scripted-average", "routine", None),
            (
                "medium",
                "test/scripted-average",
                "routine",
                Some("medium has no model; ran on routine"),
            ),
            ("high", "test/scripted-difficult", "high", None),
        ] {
            let fixture = routing_fixture(
                crate::mnemo2::dispatch_routing::RoutingMode::Off,
                None,
                false,
            )
            .await;
            let result = fixture.dispatch(Some(tier), None).await;
            assert!(result.success, "tier {tier}: {}", result.output);
            if let Some(note) = fold_note {
                assert!(result.output.contains(note), "{}", result.output);
            }
            let worker = fixture.team.worker("routingworker").await.expect("worker");
            assert_eq!(worker.model.as_deref(), Some(model), "tier {tier}");
            assert_eq!(worker.tier.as_deref(), Some(ran_tier), "tier {tier}");
            fixture.team.shutdown().await;
        }

        let fixture = routing_fixture(
            crate::mnemo2::dispatch_routing::RoutingMode::Off,
            None,
            false,
        )
        .await;
        for old_id in ["simple", "average", "difficult"] {
            let result = fixture.dispatch(Some(old_id), None).await;
            assert!(!result.success, "legacy tier {old_id} was accepted");
            assert!(result.output.contains(old_id), "{}", result.output);
            assert!(
                result
                    .output
                    .contains("trivial, routine, medium, high, escalate"),
                "{}",
                result.output
            );
        }
        let result = fixture.dispatch(Some("escalate"), None).await;
        assert!(!result.success, "tier escalate was accepted as a first pick");
        assert!(
            result.output.contains("escalation only, never a first pick"),
            "{}",
            result.output
        );
        fixture.team.shutdown().await;
    }

    #[tokio::test]
    async fn lead_dispatch_refuses_escalate_in_every_routing_mode() {
        for mode in [
            crate::mnemo2::dispatch_routing::RoutingMode::Off,
            crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
            crate::mnemo2::dispatch_routing::RoutingMode::Suggest,
            crate::mnemo2::dispatch_routing::RoutingMode::Auto,
        ] {
            let fixture = routing_fixture(mode, None, false).await;
            let result = fixture.dispatch(Some("escalate"), None).await;
            assert!(
                !result.success,
                "routing mode {mode:?}: escalate was accepted as a first pick"
            );
            assert!(
                result.output.contains("escalation only, never a first pick"),
                "routing mode {mode:?}: {}",
                result.output
            );
            assert!(
                fixture.team.worker("routingworker").await.is_err(),
                "routing mode {mode:?}: the refused dispatch provisioned a worker"
            );
            fixture.team.shutdown().await;
        }
    }

    #[test]
    fn legacy_team_tier_ids_convert_once_with_a_preserved_backup() {
        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        let mut legacy = worker(root.path(), "legacyworker1");
        legacy.tier = Some("simple".into());
        legacy.routing = Some(WorkerRouting {
            laya_tier: Some("average".into()),
            confidence: Some(0.9),
            cleared: true,
            lead_tier: Some("simple".into()),
            routed_by: "lead_override".into(),
            ran_tier: Some("difficult".into()),
            nudged: Some("average".into()),
        });
        legacy.reroutes = vec![DispatchRoutingRow::UserReroute {
            ts: 1,
            worker_id: legacy.id.clone(),
            plan_id: "plan0001".into(),
            step: 1,
            from_tier: "simple".into(),
            to_tier: "difficult".into(),
            from_model: None,
            to_model: None,
            status: "idle".into(),
            review_rounds: 0,
            fix_rounds: 0,
            nudged: false,
        }];
        let persisted = PersistedTeam {
            workers: vec![legacy],
            ..PersistedTeam::default()
        };
        let original = serde_json::to_vec_pretty(&persisted).expect("legacy JSON");
        let path = state.path().join(TEAM_STATE_FILE);
        std::fs::write(&path, &original).expect("write legacy team");
        let persistence = TeamPersistence::new(state.path());

        let converted = persistence
            .load()
            .expect("load legacy team")
            .expect("legacy team exists");
        let converted_worker = &converted.workers[0];
        assert_eq!(converted_worker.tier.as_deref(), Some("trivial"));
        let routing = converted_worker.routing.as_ref().expect("routing");
        assert_eq!(routing.laya_tier.as_deref(), Some("routine"));
        assert_eq!(routing.lead_tier.as_deref(), Some("trivial"));
        assert_eq!(routing.ran_tier.as_deref(), Some("high"));
        assert_eq!(routing.nudged.as_deref(), Some("routine"));
        assert!(matches!(
            &converted_worker.reroutes[0],
            DispatchRoutingRow::UserReroute { from_tier, to_tier, .. }
                if from_tier == "trivial" && to_tier == "high"
        ));
        let backup = state.path().join(TEAM_PRE_TIERS_BACKUP);
        assert_eq!(std::fs::read(&backup).expect("pre-tier backup"), original);

        // A second legacy load converts the live file but never replaces the
        // first backup's original bytes.
        std::fs::write(&path, &original).expect("write legacy team again");
        persistence.load().expect("second conversion").expect("team");
        assert_eq!(std::fs::read(&backup).expect("backup remains"), original);
    }

    #[tokio::test]
    async fn audit_defaults_to_high_and_unreported_audits_switch_tiers() {
        let fixture = routing_fixture(
            crate::mnemo2::dispatch_routing::RoutingMode::Off,
            None,
            false,
        )
        .await;
        let team = &fixture.team;
        let mut target = worker(fixture._root.path(), "audittarget1");
        target.plan_id = Some(fixture.plan_id.clone());
        target.plan_step = Some(1);
        provision_agent_worktree(
            fixture._root.path().to_path_buf(),
            target.id.clone(),
            "main".to_string(),
        )
        .await
        .expect("provision audit target");
        team.records.lock().await.push(target.clone());

        let (audit, reused) = team
            .audit_dispatch(&target.id, "auditdefault1", None, None, None)
            .await
            .expect("default-tier audit");
        assert!(!reused);
        assert_eq!(audit.tier.as_deref(), Some("high"));
        assert_eq!(audit.model.as_deref(), Some("test/scripted-difficult"));
        let immediate = worker_status_view(&audit, false);
        assert_eq!(immediate["tier"], json!("high"));
        assert_eq!(immediate["model"], json!("test/scripted-difficult"));

        let switched = team
            .change_model(
                &audit.id,
                Some("test/scripted-average".into()),
                None,
                ChangeSource::User,
            )
            .await
            .expect("unreported audit model switch");
        assert_eq!(switched.tier.as_deref(), Some("routine"));
        assert_eq!(switched.model.as_deref(), Some("test/scripted-average"));

        let resumed = team
            .handle_lead_host_tool(
                "team_resume",
                json!({"worker_id": audit.id, "tier": "high"}),
            )
            .await;
        assert!(resumed.success, "{}", resumed.output);
        let switched = team.worker(&audit.id).await.expect("switched audit");
        assert_eq!(switched.tier.as_deref(), Some("high"));
        assert_eq!(switched.model.as_deref(), Some("test/scripted-difficult"));
        assert!(!switched.report_delivered);
        assert!(switched.reroutes.len() >= 2, "audit switches stay in its ledger");
        assert!(fixture.rows().is_empty(), "audit switches are not training labels");
        team.shutdown().await;
    }

    #[tokio::test]
    async fn tier_plus_cross_tier_model_logs_the_ran_tier() {
        for (requested, ran, laya_label, expected_effort) in [
            ("trivial", "high", mnemo::agent::route_question::LABEL_TRIVIAL, "high"),
            ("high", "trivial", mnemo::agent::route_question::LABEL_HARD, "low"),
        ] {
            let fixture = routing_fixture_with(
                crate::mnemo2::dispatch_routing::RoutingSettings {
                    mode: crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
                    ..crate::mnemo2::dispatch_routing::RoutingSettings::default()
                },
                routing_answer(laya_label, 0.91),
                true,
                std::time::Duration::ZERO,
            )
            .await;
            let mut tiers = tiers_dto! {
                difficult: vec!["test/scripted-high".into()],
                average: vec!["test/scripted-routine".into()],
                simple: vec!["test/scripted-trivial".into()],
            };
            tiers.tiers[0].effort = Some("low".into());
            tiers.tiers[3].effort = Some("high".into());
            fixture.team.set_catalog(
                ["high", "routine", "trivial"]
                    .into_iter()
                    .map(|tier| LeadModel {
                        reasoning_efforts: vec!["low".into(), "high".into()],
                        ..lead_model(&format!("scripted-{tier}"))
                    })
                    .collect(),
                tiers,
                Vec::new(),
            );
            let model = format!("test/scripted-{ran}");
            let result = fixture.dispatch(Some(requested), Some(&model)).await;
            assert!(result.success, "{requested} -> {ran}: {}", result.output);
            assert!(result.output.contains(&format!(
                "Laya: {requested} (0.91) — you ran {ran} (requested {requested}); logged as an override."
            )));
            let worker = fixture.team.worker("routingworker").await.expect("worker");
            assert_eq!(worker.tier.as_deref(), Some(ran));
            assert_eq!(worker.model.as_deref(), Some(model.as_str()));
            assert_eq!(worker.reasoning_effort.as_deref(), Some(expected_effort));
            let routing = worker.routing.expect("routing");
            assert_eq!(routing.lead_tier.as_deref(), Some(requested));
            assert_eq!(routing.laya_tier.as_deref(), Some(requested));
            assert_eq!(routing.routed_by, "lead_override");
            let rows = fixture.rows();
            assert_eq!(rows.len(), 2);
            assert!(matches!(
                &rows[0],
                crate::mnemo2::dispatch_routing::DispatchRoutingRow::Route {
                    lead_tier: Some(lead),
                    ran_tier: Some(actual),
                    ..
                } if lead == requested && actual == ran
            ));
            assert!(matches!(
                &rows[1],
                crate::mnemo2::dispatch_routing::DispatchRoutingRow::LeadOverride {
                    lead_tier,
                    requested_tier,
                    ..
                } if lead_tier == ran && requested_tier.as_deref() == Some(requested)
            ));
            fixture.team.shutdown().await;
        }
    }

    #[tokio::test]
    async fn auto_below_gate_with_a_different_fallback_reports_unsure() {
        let fixture = routing_fixture_with(
            crate::mnemo2::dispatch_routing::RoutingSettings {
                mode: crate::mnemo2::dispatch_routing::RoutingMode::Auto,
                fallback_tier: "high".into(),
                ..crate::mnemo2::dispatch_routing::RoutingSettings::default()
            },
            routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.39),
            true,
            std::time::Duration::ZERO,
        )
        .await;
        let result = fixture.dispatch(None, None).await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("Laya: unsure (0.39)."));
        assert!(!result.output.contains("routed to difficult"));
        let worker = fixture.team.worker("routingworker").await.expect("worker");
        assert_eq!(worker.tier.as_deref(), Some("high"));
        fixture.team.shutdown().await;
    }


    #[tokio::test]
    async fn ipc_auto_id_dispatch_row_carries_the_real_worker_id() {
        let fixture = routing_fixture(
            crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
            routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.62),
            true,
        )
        .await;
        let (worker, reused) = fixture
            .team
            .dispatch_on_plan(
                "Implement the auto-id IPC step and verify its routing log".into(),
                Some("test/scripted-average".into()),
                None,
                None,
                None,
                false,
                &fixture.plan_id,
                1,
            )
            .await
            .expect("auto-id IPC dispatch");
        assert!(!reused);
        assert_eq!(fixture.classifier.calls(), 1);
        let rows = fixture.rows();
        assert_eq!(rows.len(), 1);
        assert!(matches!(
            &rows[0],
            crate::mnemo2::dispatch_routing::DispatchRoutingRow::Route { worker_id, .. }
                if worker_id == &worker.id
        ));
        assert!(!worker.id.is_empty());


        fixture.team.shutdown().await;
    }


    #[tokio::test]
    async fn ipc_shadow_dispatch_without_model_classifies_and_uses_the_default() {
        let fixture = routing_fixture(
            crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
            routing_answer(mnemo::agent::route_question::LABEL_HARD, 0.71),
            true,
        )
        .await;
        let (worker, reused) = fixture
            .team
            .dispatch_on_plan(
                "Implement the IPC default-model step and verify its routing metadata".into(),
                None,
                None,
                Some("ipcdefault".into()),
                None,
                false,
                &fixture.plan_id,
                1,
            )
            .await
            .expect("IPC default-model dispatch");
        assert!(!reused);
        assert_eq!(worker.model, None);
        assert_eq!(worker.tier, None);
        assert_eq!(fixture.classifier.calls(), 1);
        assert_eq!(fixture.rows().len(), 1);
        let routing = worker.routing.expect("routing");
        assert_eq!(routing.laya_tier.as_deref(), Some("high"));
        assert_eq!(routing.routed_by, "lead");

        let lead_tool = fixture.dispatch(None, None).await;
        assert!(!lead_tool.success);
        assert!(lead_tool.output.contains("choose a `tier` for this worker"));
        fixture.team.shutdown().await;
    }

    #[test]
    fn route_fix_prompt_contains_every_open_finding_detail() {
        let root = tempfile::tempdir().expect("workspace");
        let mut record = worker(root.path(), "fixprompt001");
        let routed = open_finding("finding1");
        let mut other = open_finding("finding2");
        other.severity = "medium".into();
        other.title = "the second unchecked path".into();
        other.detail = "EVERY OTHER OPEN FINDING DETAIL".into();
        other.file = "src/other.rs".into();
        record.findings = vec![routed.clone(), other];
        let mut closed = open_finding("finding3");
        closed.status = "fixed".into();
        closed.detail = "CLOSED DETAIL MUST NOT BE SENT".into();
        record.findings.push(closed);
        let brief = route_fix_brief(&record, &routed);
        assert!(brief.contains(&routed.detail), "{brief}");
        assert!(brief.contains("EVERY OTHER OPEN FINDING DETAIL"), "{brief}");
        assert!(brief.contains("src/other.rs"), "{brief}");
        assert!(!brief.contains("CLOSED DETAIL MUST NOT BE SENT"), "{brief}");
    }

    #[tokio::test]
    async fn failed_downshift_restores_prior_tier_and_logs_both_outcomes() {
        let fixture = review_fixture(
            crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
            "downshift001",
        )
        .await;
        let routed = open_finding("finding1");
        fixture
            .team
            .review(&fixture.record.id, "first".into(), vec![routed.clone()], evidence())
            .await
            .expect("first review");
        let down = fixture
            .team
            .route_fix_with_tier(&fixture.record.id, "finding1", Some("routine"))
            .await
            .expect("explicit downshift is allowed even for a high finding");
        assert_eq!(down.tier.as_deref(), Some("routine"));
        assert_eq!(down.model.as_deref(), Some("test/scripted-average"));
        fixture.team.stop_inner(&fixture.record.id).await.expect("stop fixer");
        let restored = fixture
            .team
            .review(&fixture.record.id, "still open".into(), vec![routed.clone()], evidence())
            .await
            .expect("failed downshift review");
        assert_eq!(restored.tier.as_deref(), Some("high"));
        assert_eq!(restored.model.as_deref(), Some("test/scripted-difficult"));
        let raw = std::fs::read_to_string(fixture.state.path().join("dispatch_routing.jsonl"))
            .expect("routing log");
        assert!(raw.lines().any(|line| {
            serde_json::from_str::<Value>(line).is_ok_and(|row| {
                row["source"] == "fix_downshift" && row["finding_closed"] == false
            })
        }), "{raw}");

        fixture
            .team
            .route_fix_with_tier(&fixture.record.id, "finding1", Some("routine"))
            .await
            .expect("second deliberate downshift");
        fixture.team.stop_inner(&fixture.record.id).await.expect("stop second fixer");
        let mut closed = routed;
        closed.status = "fixed".into();
        closed.evidence = Some("fixed and checked".into());
        let stayed = fixture
            .team
            .review(&fixture.record.id, "closed".into(), vec![closed], evidence())
            .await
            .expect("closed downshift review");
        assert_eq!(stayed.tier.as_deref(), Some("routine"));
        let raw = std::fs::read_to_string(fixture.state.path().join("dispatch_routing.jsonl"))
            .expect("routing log");
        assert!(raw.lines().any(|line| {
            serde_json::from_str::<Value>(line).is_ok_and(|row| {
                row["source"] == "fix_downshift" && row["finding_closed"] == true
            })
        }), "{raw}");
        fixture.team.shutdown().await;
    }

    #[tokio::test]
    async fn auto_fix_classification_never_lowers_a_high_severity_finding() {
        let mut fixture = review_fixture(
            crate::mnemo2::dispatch_routing::RoutingMode::Auto,
            "protected001",
        )
        .await;
        let classifier = std::sync::Arc::new(StubDispatchClassifier::slow(
            routing_answer(mnemo::agent::route_question::LABEL_TRIVIAL, 0.95),
            std::time::Duration::ZERO,
        ));
        fixture.team.classifier = Some(std::sync::Arc::new(std::sync::RwLock::new(Some(
            classifier.clone(),
        ))));
        fixture
            .team
            .review(
                &fixture.record.id,
                "first".into(),
                vec![open_finding("finding1")],
                evidence(),
            )
            .await
            .expect("first review");
        let unchanged = fixture
            .team
            .route_fix_with_tier(&fixture.record.id, "finding1", Some("auto"))
            .await
            .expect("classified but protected fix");
        assert_eq!(unchanged.tier.as_deref(), Some("high"));
        assert_eq!(unchanged.model.as_deref(), Some("test/scripted-difficult"));
        assert_eq!(classifier.calls(), 1);
        assert!(fixture.rows().iter().any(|row| matches!(
            row,
            DispatchRoutingRow::Route { skip, .. }
                if skip.as_deref() == Some("fix_downshift_protected")
        )));
        fixture.team.shutdown().await;
    }

    #[tokio::test]
    async fn high_audit_finding_after_fix_nudges_to_escalate() {
        let fixture = review_fixture(
            crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
            "auditnudge001",
        )
        .await;
        fixture
            .team
            .review(
                &fixture.record.id,
                "first".into(),
                vec![open_finding("finding1")],
                evidence(),
            )
            .await
            .expect("first review");
        fixture
            .team
            .route_fix(&fixture.record.id, "finding1")
            .await
            .expect("route fix");
        fixture.team.stop_inner(&fixture.record.id).await.expect("stop fixer");
        let tip = fixture.team.worker_tip(&fixture.record).await.expect("tip");
        let audit = audit_worker(WorkerStatus::Completed, &fixture.record.id, &tip);
        fixture.team.records.lock().await.push(audit);
        fixture.team.persist().await.expect("save audit");
        let mut audit_evidence = evidence();
        audit_evidence.independent_reviewer_id = Some("auditor0001".into());
        audit_evidence.audit_adjudication = Some("accepted the high finding".into());
        let reviewed = fixture
            .team
            .review(
                &fixture.record.id,
                "audit still open".into(),
                vec![open_finding("finding1")],
                audit_evidence,
            )
            .await
            .expect("audited review");
        assert_eq!(
            reviewed.routing.as_ref().and_then(|routing| routing.nudged.as_deref()),
            Some("escalate")
        );
        assert!(fixture.rows().iter().any(|row| matches!(
            row,
            DispatchRoutingRow::Nudge { to_tier, .. } if to_tier == "escalate"
        )));
        fixture.team.shutdown().await;
    }

    #[tokio::test]
    async fn auto_mode_switches_an_idle_nudged_worker_and_notifies_the_lead() {
        let (fixture, mut notices) = nudge_fixture_with(
            crate::mnemo2::dispatch_routing::RoutingSettings {
                mode: crate::mnemo2::dispatch_routing::RoutingMode::Auto,
                ..crate::mnemo2::dispatch_routing::RoutingSettings::default()
            },
            routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.8),
            std::time::Duration::ZERO,
            vec![vec![
                mnemo::provider::LlmEvent::TextDelta { text: "done".into() },
                mnemo::provider::LlmEvent::Finish {
                    reason: mnemo::provider::FinishReason::Stop,
                },
            ]],
        )
        .await;
        let dispatched = fixture.dispatch(Some("routine"), None).await;
        assert!(dispatched.success, "{}", dispatched.output);
        wait_worker_settled(&fixture.team, "routingworker").await;
        fixture
            .team
            .update_worker("routingworker", |worker| {
                if let Some(routing) = worker.routing.as_mut() {
                    routing.nudged = Some("high".into());
                }
            })
            .await;
        fixture.team.persist().await.expect("save pending nudge");
        assert!(
            fixture
                .team
                .auto_escalate_worker("routingworker")
                .await
                .expect("auto escalation")
        );
        let switched = fixture.team.worker("routingworker").await.expect("switched");
        assert_eq!(switched.model.as_deref(), Some("test/scripted-difficult"));
        assert_eq!(switched.tier.as_deref(), Some("high"));
        assert!(switched.routing.expect("routing").nudged.is_none());
        let raw = std::fs::read_to_string(fixture.log_path()).expect("routing log");
        assert!(raw.lines().any(|line| {
            serde_json::from_str::<Value>(line).is_ok_and(|row| {
                row["event"] == "lead_reroute" && row["source"] == "escalation"
            })
        }), "{raw}");
        let mut notice = None;
        for _ in 0..800 {
            while let Ok(signal) = notices.try_recv() {
                if let BoardSignal::Notice { key, text } = signal {
                    if key == "escalation:auto:routingworker" {
                        notice = Some(text);
                    }
                }
            }
            if notice.is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        let notice = notice.expect("auto escalation notice");
        assert!(notice.contains("session and transcript were kept"), "{notice}");
        fixture.team.shutdown().await;
    }

    #[tokio::test]
    async fn auto_strike_streak_escalates_when_the_worker_becomes_idle() {
        let responses = (1..=3)
            .map(|attempt| {
                tool_call_batch(
                    &format!("auto-call-{attempt}"),
                    "not_a_real_tool",
                    &format!("{{\"attempt\":{attempt}}}"),
                )
            })
            .chain([vec![
                mnemo::provider::LlmEvent::TextDelta { text: "done".into() },
                mnemo::provider::LlmEvent::Finish {
                    reason: mnemo::provider::FinishReason::Stop,
                },
            ]])
            .collect::<Vec<_>>();
        let (fixture, _notices) = nudge_fixture_with(
            crate::mnemo2::dispatch_routing::RoutingSettings {
                mode: crate::mnemo2::dispatch_routing::RoutingMode::Auto,
                ..crate::mnemo2::dispatch_routing::RoutingSettings::default()
            },
            routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.8),
            std::time::Duration::ZERO,
            responses,
        )
        .await;
        let dispatched = fixture.dispatch(Some("routine"), None).await;
        assert!(dispatched.success, "{}", dispatched.output);
        let switched = eventually(|| async {
            let worker = fixture.team.worker("routingworker").await.expect("worker");
            (worker.model.as_deref() == Some("test/scripted-difficult")).then_some(worker)
        })
        .await;
        assert_eq!(switched.tier.as_deref(), Some("high"));
        assert!(switched.routing.expect("routing").nudged.is_none());
        // The row is appended after the ledger save, so poll for it: the
        // in-memory switch can be visible before the file write lands.
        eventually(|| async {
            let raw = std::fs::read_to_string(fixture.log_path()).ok()?;
            raw.lines()
                .any(|line| {
                    serde_json::from_str::<Value>(line).is_ok_and(|row| {
                        row["source"] == "escalation" && row["nudged"] == true
                    })
                })
                .then_some(())
        })
        .await;
        fixture.team.shutdown().await;
    }

    #[tokio::test]
    async fn worker_host_tools_offer_plan_read() {
        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        let team = open_test_team(root.path(), state.path(), Vec::new()).await;
        let tools = team.worker_host_tools();
        let plan_read = tools
            .iter()
            .find(|tool| tool.name == "plan_read")
            .expect("plan_read is offered");
        assert!(matches!(plan_read.safety, SafetyLevel::AutoRun));
        assert_eq!(plan_read.timeout_ms, Some(10_000));
        assert_eq!(
            plan_read.parameters["properties"]["capability"]["type"],
            "string"
        );
        assert!(plan_read.parameters.get("required").is_none());
        team.shutdown().await;
    }

    /// A tagged notice describes one turn of one worker: a notice whose worker
    /// moved on, or no longer exists, is dropped at delivery. An untagged
    /// notice (a report, a plan change) always stays.
    #[tokio::test]
    async fn current_notices_drop_a_moved_on_worker() {
        use crate::mnemo2::notice_tag::{NoticeState, TurnTag};
        use crate::mnemo2::runtime::TEAM_NOTICE_PREFIX;

        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        let mut completed = worker(root.path(), "00000001000000000001");
        completed.status = WorkerStatus::Completed;
        completed.turns = 2;
        let mut working = worker(root.path(), "00000001000000000002");
        working.status = WorkerStatus::Working;
        let team = open_test_team(
            root.path(),
            state.path(),
            vec![completed.clone(), working.clone()],
        )
        .await;

        let tagged = |worker: &WorkerRecord, state| {
            format!(
                "{TEAM_NOTICE_PREFIX} worker_id={}{}\nthe detail",
                worker.id,
                TurnTag::of(worker, 1, state).suffix()
            )
        };
        let current_finished = tagged(&completed, NoticeState::Finished);
        let stale_finished = tagged(&working, NoticeState::Finished);
        let unknown = tagged(
            &worker(root.path(), "00000001000000000009"),
            NoticeState::Finished,
        );
        let untagged = format!("{TEAM_NOTICE_PREFIX}\nplan changed");
        let plain = "worker alpha finished its turn".to_string();

        let kept = team
            .current_notices(vec![
                current_finished.clone(),
                stale_finished,
                unknown,
                untagged.clone(),
                plain.clone(),
            ])
            .await;
        assert_eq!(kept, vec![current_finished, untagged, plain]);
        team.shutdown().await;
    }

    /// A stale notice drained from the queue must not end the wait: the lead
    /// keeps waiting for a notice that still describes its worker.
    #[tokio::test]
    async fn waiting_skips_stale_notices() {
        use crate::mnemo2::notice_tag::{NoticeState, TurnTag};
        use crate::mnemo2::runtime::TEAM_NOTICE_PREFIX;

        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        let mut working = worker(root.path(), "00000001000000000002");
        working.status = WorkerStatus::Working;
        let team = open_test_team(root.path(), state.path(), vec![working.clone()]).await;
        assert!(team.attach_external_lead(), "the external lead attaches");
        let stale = format!(
            "{TEAM_NOTICE_PREFIX} worker_id={}{}\nstale detail",
            working.id,
            TurnTag::of(&working, 1, NoticeState::Finished).suffix()
        );
        assert!(team.push_external_notice(stale), "the notice is queued");

        let outcome = team.wait_external(std::time::Duration::from_millis(150)).await;
        assert_eq!(outcome.status, ExternalWaitStatus::Timeout, "{outcome:?}");
        assert!(outcome.notices.is_empty(), "{:?}", outcome.notices);
        team.shutdown().await;
    }


    #[tokio::test]
    async fn plan_read_returns_the_current_plan() {
        let root = tempfile::tempdir().expect("workspace");
        init_git(root.path());
        let plans_dir = root.path().join(".coding/plans");
        let board = PlanBoard::new(&plans_dir, root.path().join(".coding/reviews"));
        let plan = board
            .create(mnemo::app::plan_board::PlanDraft {
                title: "Plan reading".into(),
                goal: "Let a worker re-read its plan".into(),
                context: "A worker re-reads src-tauri/src/mnemo2/team.rs after dispatch and verifies with cargo test -p mnemo-app --features mnemo2 --lib mnemo2::team.".into(),
                steps: vec![
                    "Implement plan_read in src-tauri/src/mnemo2/team.rs and verify with cargo test -p mnemo-app --features mnemo2 --lib mnemo2::team".into(),
                    "Handle notices in src-tauri/src/mnemo2/team.rs and verify with cargo test -p mnemo-app --features mnemo2 --lib mnemo2::team".into(),
                ],
                ..Default::default()
            })
            .expect("create plan");
        board
            .update(
                &plan.id,
                mnemo::app::plan_board::PlanEdit {
                    decision: Some("Use the shared capability reader".into()),
                    ..Default::default()
                },
            )
            .expect("record the decision");
        let board = Arc::new(board);
        let messages = Arc::new(Mutex::new(Vec::new()));
        let stamp = Some((plan.id.clone(), 1usize));
        let result = worker_host_tool_result(
            &messages,
            "planreader",
            "plan_read",
            &json!({}),
            Some(&board),
            stamp.as_ref(),
        )
        .await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("Use the shared capability reader"),
            "{}",
            result.output
        );
        assert!(
            result.output.contains("Re-read this plan at any time"),
            "{}",
            result.output
        );
        assert!(result.output.contains("## Your step (2)"), "{}", result.output);

        let no_plan = worker_host_tool_result(
            &messages,
            "planreader",
            "plan_read",
            &json!({}),
            Some(&board),
            None,
        )
        .await;
        assert!(!no_plan.success);
        assert!(no_plan.output.contains("no plan"), "{}", no_plan.output);
    }

    #[tokio::test]
    async fn plan_read_reads_a_capability_spec() {
        let root = tempfile::tempdir().expect("workspace");
        init_git(root.path());
        let spec_dir = root
            .path()
            .join(".coding/openspec/specs/tier-ladder");
        std::fs::create_dir_all(&spec_dir).expect("spec dir");
        std::fs::write(spec_dir.join("spec.md"), "# Tier ladder\n").expect("spec file");
        let board = Arc::new(PlanBoard::new(
            root.path().join(".coding/plans"),
            root.path().join(".coding/reviews"),
        ));
        let messages = Arc::new(Mutex::new(Vec::new()));
        let current = worker_host_tool_result(
            &messages,
            "planreader",
            "plan_read",
            &json!({"capability": "tier-ladder"}),
            Some(&board),
            None,
        )
        .await;
        assert!(current.success, "{}", current.output);
        assert_eq!(current.output, "# Tier ladder\n");

        let missing = worker_host_tool_result(
            &messages,
            "planreader",
            "plan_read",
            &json!({"capability": "not-a-capability"}),
            Some(&board),
            None,
        )
        .await;
        assert!(missing.success, "{}", missing.output);
        assert_eq!(
            missing.output,
            "No current spec for 'not-a-capability' yet."
        );

        let invalid = worker_host_tool_result(
            &messages,
            "planreader",
            "plan_read",
            &json!({"capability": "../x"}),
            Some(&board),
            None,
        )
        .await;
        assert!(!invalid.success);
        assert!(invalid.output.contains("invalid capability"), "{}", invalid.output);
    }

    #[tokio::test]
    async fn plan_edit_notifies_only_working_workers() {
        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        let board = PlanBoard::new(
            root.path().join(".coding/plans"),
            root.path().join(".coding/reviews"),
        );
        let plan = board
            .create(mnemo::app::plan_board::PlanDraft {
                title: "Plan change notices".into(),
                goal: "Tell only the workers still working".into(),
                context: "One worker is editing src-tauri/src/mnemo2/team.rs while another waits for review; the working one verifies with cargo test -p mnemo-app --features mnemo2 --lib mnemo2::team.".into(),
                steps: vec![
                    "Send notices in src-tauri/src/mnemo2/team.rs and verify with cargo test -p mnemo-app --features mnemo2 --lib mnemo2::team".into(),
                    "Review the notice in src-tauri/src/mnemo2/team.rs and verify with cargo test -p mnemo-app --features mnemo2 --lib mnemo2::team".into(),
                ],
                ..Default::default()
            })
            .expect("create plan");
        let working_id = "00000001000000000001";
        let completed_id = "00000002000000000002";
        let mut working = worker(root.path(), working_id);
        working.status = WorkerStatus::Working;
        working.plan_id = Some(plan.id.clone());
        working.plan_step = Some(0);
        let mut completed = worker(root.path(), completed_id);
        completed.status = WorkerStatus::Completed;
        completed.plan_id = Some(plan.id.clone());
        completed.plan_step = Some(1);
        provision_agent_worktree(
            root.path().to_path_buf(),
            working.id.clone(),
            "main".to_string(),
        )
        .await
        .expect("provision the working worker");
        let team = open_test_team(root.path(), state.path(), vec![working.clone(), completed.clone()]).await;
        team.update_worker(working_id, |record| {
            record.status = WorkerStatus::Working;
        })
        .await;
        team.update_worker(completed_id, |record| {
            record.status = WorkerStatus::Completed;
        })
        .await;
        spawn_live_worker(&team, &working).await;
        let team = Arc::new(team);
        let board = Arc::new(board);
        team.attach_board(board.clone());
        board
            .update(
                &plan.id,
                mnemo::app::plan_board::PlanEdit {
                    decision: Some("A decision recorded after dispatch".into()),
                    ..Default::default()
                },
            )
            .expect("record the decision");
        let expected = format!(
            "Plan {} changed: new decision. Call plan_read to re-read it before you continue.",
            plan.id
        );
        let mut notice = None;
        for _ in 0..250 {
            let worker = team.worker(working_id).await.expect("working worker");
            if let Some(entry) = worker.inbox.first() {
                notice = Some(entry.clone());
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let notice = match notice {
            Some(notice) => notice,
            None => {
                let worker = team.worker(working_id).await.expect("working worker");
                panic!(
                    "the working worker got no notice; status {:?}, inbox {:?}",
                    worker.status, worker.inbox
                )
            }
        };
        assert_eq!(notice.text, expected);
        assert_eq!(notice.sender, MessageSender::Mnemo);
        assert_eq!(
            team.worker(completed_id)
                .await
                .expect("completed worker")
                .inbox,
            Vec::new()
        );

        board
            .update(
                &plan.id,
                mnemo::app::plan_board::PlanEdit {
                    progress_note: Some("Only a progress note".into()),
                    ..Default::default()
                },
            )
            .expect("record the progress note");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(
            team.worker(working_id).await.expect("working worker").inbox.len(),
            1
        );
        team.shutdown().await;
    }

    /// A refresh reads the worker tip outside the ledger lock, so the lead can
    /// commit and review inside that window: the review then postdates the
    /// probe and covers the current tip. Clearing it made the merge fail with
    /// "the current worker tip has not been reviewed" (plan 0d8a1112, macOS CI
    /// run 42071163294).
    #[test]
    fn a_refresh_that_read_the_tip_before_the_review_keeps_that_review() {
        let root = tempfile::tempdir().expect("workspace");
        init_git(root.path());
        let mut worker = reviewed_worker(root.path(), "00000001000000000001");
        {
            let review = worker.review.as_mut().expect("review");
            review.tip = "reviewed-tip".to_string();
            review.reviewed_at_ms = 200;
        }
        worker.checks[0].tip = "reviewed-tip".to_string();

        // The probe ran at 100 ms, the lead's commit and review landed at 200.
        clear_review_for_a_newer_tip(&mut worker, Some("older-tip"), 100);
        assert!(
            worker.review.is_some(),
            "a review recorded after the tip probe is not stale"
        );
        assert_eq!(worker.checks.len(), 1, "its check survives with it");

        // A probe that ran after the review, but on the reviewed commit.
        clear_review_for_a_newer_tip(&mut worker, Some("reviewed-tip"), 300);
        assert!(worker.review.is_some(), "the reviewed tip is never stale");

        // A probe that ran after the review and saw a newer commit is a real
        // verdict: the review no longer covers the worker's tip.
        clear_review_for_a_newer_tip(&mut worker, Some("newer-tip"), 300);
        assert!(worker.review.is_none(), "an older review is cleared");
        assert!(worker.checks.is_empty(), "and so are its checks");
    }
}
