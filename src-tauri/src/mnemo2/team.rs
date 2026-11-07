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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use mnemo::app::plan_board::{BoardPlan, BoardStatus, BoardStep, CloseOutcome, PlanBoard};
use mnemo::app::team_ledger::TeamLedger;
use mnemo::app::worktrees::{
    agent_branch, agent_short_id, current_branch, provision_agent_worktree, remove_agent_worktree,
};
use mnemo::route_question;
use mnemo::config::endpoints::{Tier, TierLadder};
use mnemo::headless::{
    AgentSession, Engine, EventReceiver, HostToolSpec, PendingInteraction, SessionCommand,
    SessionEvent, SessionId, SpawnOptions, Transcript,
};
use mnemo::headless::session::ModelSwitch;
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

mod guardrails;
mod persistence;
mod record;

use guardrails::guardrails_for;
use persistence::{PersistedTeam, TeamPersistence};
use record::{
    append_inbox, audit_finished, audit_finished_error, kickoff_message, note_model,
    session_model_spec, validate_worker_id,
};

#[cfg(test)]
pub(crate) use guardrails::{audit_prompt, executor_prompt};
#[cfg(test)]
pub(crate) use record::{KICKOFF, READ_BACK_KICKOFF};
pub use record::{
    ChangeSource, InboxEntry, InboxKind, MessageSender, ToolErrorNote, WorkerCheck, WorkerRecord,
    WorkerRouting, WorkerStatus,
};

/// The Tauri event channel emitted by the team runtime.
pub const TEAM_EVENT_CHANNEL: &str = "mnemo2://team-event";

/// The Tauri event channel streaming per-worker deletion-flow progress
/// (`DeletionProgress`) while `team_delete_preview` or `team_delete` runs.
pub const DELETION_EVENT_CHANNEL: &str = "mnemo2://worker-deletion";

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
    /// The classification an explicit-choice dispatch defers to the
    /// background (plan c74b1987): `Some` only when the lead's tier or
    /// model choice made the inline await pure overhead. The dispatch
    /// returns with a null Laya label; the spawned task writes the corpus
    /// rows and attaches the label to the worker's routing record.
    pending: Option<PendingClassification>,
    error: Option<String>,
}

/// Everything the background classification of an explicit-choice dispatch
/// needs: the same classifier, permit, timeout and corpus rows as the
/// inline path, plus the dispatch-time choices those rows record. The
/// worker id is attached when the task is spawned, after provisioning.
struct PendingClassification {
    /// The classifier resolved at dispatch time, exactly like the inline
    /// path's.
    classifier: std::sync::Arc<dyn mnemo::memory::classifier::Classifier>,
    /// The kNN overlay over the routing log, cloned at dispatch time.
    knn: Option<Arc<RouteKnn>>,
    /// The serialized routing permit, shared with the inline path and the
    /// drift checks.
    permit: Arc<tokio::sync::Semaphore>,
    /// The classified step text plus brief.
    text: String,
    /// The tiers that resolve to a model.
    enabled: EnabledTiers,
    /// The dispatch-routing settings snapshot at dispatch time.
    settings: RoutingSettings,
    /// The plan the dispatch ran under, for the corpus rows.
    plan_id: String,
    /// The 1-based step for the corpus rows.
    step: usize,
    /// The lead's requested tier, when one was passed.
    explicit_tier: Option<String>,
    /// The tier the lead's choice resolved to.
    chosen_tier: Option<String>,
    /// The model spec the dispatch runs on.
    chosen_model: Option<String>,
    /// Whether a model choice was passed: the override comparison reads the
    /// resolved tier then, the requested tier otherwise.
    model_choice: bool,
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
/// (design §4.6). Deliberately still 3 after plan 865249fd raised the
/// session's doom-guard cap (`MAX_RETRIES`, src/agent/mod.rs) 3 → 5: the
/// stuck-worker nudge tells the lead early, while the worker itself keeps
/// running to the 5th consecutive failing batch before the turn aborts.
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
        // App start: a checkout a previous run could not remove (something
        // held it) is retried now, and the branch it was blocking goes with it.
        runtime.retry_leftover_cleanups().await;
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
                pending: None,
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
        // An explicit lead/user choice never waits on the classification
        // (plan c74b1987): the decision cannot change what runs — refusals
        // only occur on non-explicit paths — so it runs in the background
        // under the same permit and timeout, writes the same corpus rows,
        // and attaches its label to the worker's routing record when it
        // lands. Skip conditions (too short, one tier, no classifier) stay
        // inline: their outcome is decided without a call, exactly as
        // before.
        if has_explicit_choice
            && classified_text.is_some()
            && enabled.offered().count() >= 2
        {
            if let Some(classifier) = classifier {
                let (chosen_model, chosen_tier) =
                    self.lead_choice_with_tier(explicit_tier, request.model.clone())?;
                let reasoning_effort = request.reasoning_effort.clone().or_else(|| {
                    chosen_tier.as_deref().and_then(|tier| {
                        self.tier_ladder()
                            .get(tier)
                            .and_then(|rung| rung.effort.clone())
                    })
                });
                let routing = WorkerRouting {
                    laya_tier: None,
                    confidence: None,
                    cleared: false,
                    lead_tier: explicit_tier
                        .map(str::to_string)
                        .or_else(|| chosen_tier.clone()),
                    routed_by: "lead".to_string(),
                    ran_tier: chosen_tier.clone(),
                    nudged: None,
                };
                return Ok(RoutedDispatch {
                    model: chosen_model.clone(),
                    tier: chosen_tier.clone(),
                    reasoning_effort,
                    routing: Some(routing),
                    rows: Vec::new(),
                    pending: Some(PendingClassification {
                        classifier,
                        knn: self.route_knn.clone(),
                        permit: self.routing_permit.clone(),
                        text: classified_text.unwrap_or_default(),
                        enabled,
                        settings,
                        plan_id: request
                            .plan
                            .as_ref()
                            .map(|plan| plan.plan_id.clone())
                            .unwrap_or_default(),
                        step: request.plan.as_ref().map_or(0, |plan| plan.step + 1),
                        explicit_tier: explicit_tier.map(str::to_string),
                        chosen_tier: chosen_tier.clone(),
                        chosen_model,
                        model_choice: request.model.is_some(),
                    }),
                    error: None,
                });
            }
        }
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
        let ran_tier = refusal.is_none().then(|| chosen_tier.clone()).flatten();
        let ran_model = refusal.is_none().then(|| chosen_model.clone()).flatten();
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
            ran_tier: ran_tier.clone(),
            nudged: None,
        };
        let plan_id = request.plan.as_ref().map(|plan| plan.plan_id.clone());
        let step = request.plan.as_ref().map(|plan| plan.step + 1);
        let ts = unix_ms();
        let row = dispatch_route_row(
            ts,
            request.worker_id.as_deref().unwrap_or_default(),
            plan_id.as_deref().unwrap_or_default(),
            step.unwrap_or_default(),
            settings.mode,
            decision.as_ref(),
            skip_reason,
            routing.laya_tier.as_deref(),
            routing.cleared,
            routing.lead_tier.as_deref(),
            ran_tier.as_deref(),
            ran_model.as_deref(),
            classified_text.as_deref().unwrap_or_default(),
            enabled,
            &settings,
        );
        let mut rows = vec![row];
        if routed_by == "lead_override" {
            if let Some(laya_tier) = routing.laya_tier.clone() {
                rows.push(dispatch_lead_override_row(
                    ts,
                    request.worker_id.as_deref().unwrap_or_default(),
                    plan_id.as_deref().unwrap_or_default(),
                    step.unwrap_or_default(),
                    &laya_tier,
                    chosen_tier.as_deref(),
                    explicit_tier
                        .map(str::to_string)
                        .filter(|requested| Some(requested.as_str()) != chosen_tier.as_deref())
                        .as_deref(),
                    decision.as_ref().map_or(0.0, |decision| decision.confidence),
                ));
            }
        }
        Ok(RoutedDispatch {
            model: chosen_model,
            tier: chosen_tier,
            reasoning_effort,
            routing: Some(routing),
            rows,
            pending: None,
            error: refusal,
        })
    }

    /// Run the classification an explicit-choice dispatch deferred
    /// (plan c74b1987): the same permit and 3-second timeout as the
    /// inline path, the same corpus rows, and — when the worker record
    /// still exists — the label attached to its routing block, following
    /// the nudge path's live-record mutation. The dispatch has already
    /// returned; nothing here can block or fail it, so a record gone to
    /// cleanup drops the attach silently while the corpus rows still land.
    fn spawn_pending_classification(&self, pending: PendingClassification, worker_id: String) {
        let records = self.records.clone();
        let routing_log_path = self.routing_log_path.clone();
        let usage = self.usage.clone();
        let save_lock = self.save_lock.clone();
        let persistence = self.persistence.clone();
        tokio::spawn(async move {
            let attempt = async {
                let _permit = pending
                    .permit
                    .acquire()
                    .await
                    .map_err(|_| "Laya routing is closed".to_string())?;
                Ok::<Option<DispatchRoutingDecision>, String>(
                    classify_dispatch(
                        pending.classifier.as_ref(),
                        pending.knn.as_deref(),
                        &pending.text,
                        pending.enabled,
                        &pending.settings,
                    )
                    .await,
                )
            };
            // The same 3-second budget as the inline path, permit wait
            // included; a closed permit or a timeout is an offline skip.
            let decision = match tokio::time::timeout(
                std::time::Duration::from_secs(3),
                attempt,
            )
            .await
            {
                Ok(Ok(decision)) => decision,
                Ok(Err(_)) | Err(_) => None,
            };
            let skip = decision.is_none().then_some("offline");
            let laya_tier = decision
                .as_ref()
                .and_then(|decision| decision.tier)
                .map(str::to_string);
            let cleared = decision.as_ref().is_some_and(|decision| decision.cleared);
            let lead_tier = pending
                .explicit_tier
                .clone()
                .or_else(|| pending.chosen_tier.clone());
            let ts = unix_ms();
            let row = dispatch_route_row(
                ts,
                &worker_id,
                &pending.plan_id,
                pending.step,
                pending.settings.mode,
                decision.as_ref(),
                skip,
                laya_tier.as_deref(),
                cleared,
                lead_tier.as_deref(),
                pending.chosen_tier.as_deref(),
                pending.chosen_model.as_deref(),
                &pending.text,
                pending.enabled,
                &pending.settings,
            );
            mnemo::model_routing::append_row(&routing_log_path, &row);
            // The override row an explicit choice logs when Laya cleared a
            // different tier — the same condition the inline path's
            // `routed_by` computation applies to an explicit choice.
            let cleared_tier = decision
                .as_ref()
                .filter(|decision| decision.cleared)
                .and_then(|decision| decision.tier);
            let lead_choice_tier = if pending.model_choice {
                pending.chosen_tier.as_deref()
            } else {
                pending.explicit_tier.as_deref()
            };
            if cleared_tier.is_some() && cleared_tier != lead_choice_tier {
                if let Some(laya_tier) = laya_tier.clone() {
                    let row = dispatch_lead_override_row(
                        ts,
                        &worker_id,
                        &pending.plan_id,
                        pending.step,
                        &laya_tier,
                        pending.chosen_tier.as_deref(),
                        pending
                            .explicit_tier
                            .as_deref()
                            .map(str::to_string)
                            .filter(|requested| {
                                Some(requested.as_str()) != pending.chosen_tier.as_deref()
                            })
                            .as_deref(),
                        decision.as_ref().map_or(0.0, |decision| decision.confidence),
                    );
                    mnemo::model_routing::append_row(&routing_log_path, &row);
                }
            }
            // Attach the label to the live record; a record gone to cleanup
            // drops the attach silently — the corpus rows above still land.
            let attached = {
                let mut records = records.lock().await;
                records
                    .iter_mut()
                    .find(|worker| worker.id == worker_id)
                    .and_then(|worker| worker.routing.as_mut())
                    .map(|routing| {
                        routing.laya_tier = laya_tier.clone();
                        routing.confidence =
                            decision.as_ref().map(|decision| decision.confidence);
                        routing.cleared = cleared;
                    })
                    .is_some()
            };
            if attached {
                let _ = save_workers(&records, &usage, &save_lock, &persistence).await;
            }
        });
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
                .await
                // Audits are never classified: no deferred label can land.
                .map(|(worker, reused)| (worker, reused, false));
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
        .map(|(worker, reused, _)| (worker, reused))
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
        .map(|(worker, _, _)| worker)
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
        .map(|(worker, _, _)| worker)
    }

    /// The one dispatch entry point. The returned triple is the worker, the
    /// idempotent-reuse flag, and whether a deferred classification is
    /// running in the background (plan c74b1987) — the dispatch answer
    /// reports that instead of a label that has not landed yet.
    async fn dispatch_request(
        &self,
        request: DispatchRequest,
    ) -> Result<(WorkerRecord, bool, bool), String> {
        let routed = self.prepare_dispatch_routing(&request).await?;
        let result = self.dispatch_locked(request, routed).await;
        if matches!(result, Ok((_, false, _))) {
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
    ) -> Result<(WorkerRecord, bool, bool), String> {
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
                        .map(|worker| (worker, true, false));
                }
                return Ok((existing, true, false));
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
        let (model, reasoning_effort, tier, routing, routing_rows, routing_error, routing_pending) =
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
                    routed.pending,
                )
            } else {
                (model, reasoning_effort, tier, None, Vec::new(), None, None)
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
        let mut pending_spawned = false;
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
                // The classification an explicit-choice dispatch deferred:
                // spawned now that the worker id exists, never blocking the
                // dispatch that has already returned without it.
                if let Some(pending) = routing_pending {
                    self.spawn_pending_classification(pending, record.id.clone());
                    pending_spawned = true;
                }
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
        result.map(|worker| (worker, false, pending_spawned))
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
                    worker.error = Some(format!("{WORKTREE_CLEANUP_ERROR}: {err}"));
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
        self.retry_checkout_cleanups_locked(worker_id).await;
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

    /// Retry the checkout removal for merged workers whose earlier removal
    /// failed: a folder something held at merge time, typically a shell whose
    /// working folder was inside it. Runs with the merge lock held. A removal
    /// that works (or finds the folder already gone) clears the note; one
    /// that still fails leaves the record as it was. Deleting the branch of a
    /// worker whose checkout is now gone is `sweep_merged_branches`'s job.
    async fn retry_checkout_cleanups_locked(&self, skip: &str) {
        let candidates: Vec<WorkerRecord> = {
            let records = self.records.lock().await;
            records
                .iter()
                .filter(|candidate| {
                    candidate.id != skip
                        && candidate.status == WorkerStatus::Merged
                        && candidate
                            .error
                            .as_deref()
                            .is_some_and(is_worktree_cleanup_error)
                })
                .cloned()
                .collect()
        };
        let mut cleared = false;
        for candidate in candidates {
            if cleanup_agent_worktree(&self.workspace_root, &candidate)
                .await
                .is_err()
            {
                continue;
            }
            self.update_worker(&candidate.id, |worker| {
                if worker
                    .error
                    .as_deref()
                    .is_some_and(is_worktree_cleanup_error)
                {
                    worker.error = None;
                }
            })
            .await;
            cleared = true;
        }
        if cleared {
            let _ = self.persist().await;
        }
    }

    /// App start: retry earlier stuck checkout removals, then the branch
    /// deletes they were blocking. Takes the merge lock itself.
    async fn retry_leftover_cleanups(&self) {
        let _guard = self.merge_lock.lock().await;
        self.retry_checkout_cleanups_locked("").await;
        self.sweep_merged_branches("").await;
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

/// The durable notes a failed checkout removal leaves on a merged worker.
/// `merge_locked` writes the first, `recover_worker` the second; both are
/// retried by `retry_checkout_cleanups_locked`.
const WORKTREE_CLEANUP_ERROR: &str = "merged, but worktree cleanup needs attention";
const RECOVERED_WORKTREE_CLEANUP_ERROR: &str =
    "merge recovered, but worktree cleanup needs attention";

/// Whether an error is one of the checkout-cleanup notes above.
fn is_worktree_cleanup_error(error: &str) -> bool {
    error.starts_with(WORKTREE_CLEANUP_ERROR)
        || error.starts_with(RECOVERED_WORKTREE_CLEANUP_ERROR)
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
                worker.error = Some(format!("{RECOVERED_WORKTREE_CLEANUP_ERROR}: {err}"));
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

/// The training-corpus `Route` row for one dispatch classification: the
/// single construction shared by the inline path and the deferred
/// explicit-choice classification, so both write identical rows.
#[allow(clippy::too_many_arguments)]
fn dispatch_route_row(
    ts: u64,
    worker_id: &str,
    plan_id: &str,
    step: usize,
    mode: RoutingMode,
    decision: Option<&DispatchRoutingDecision>,
    skip: Option<&str>,
    laya_tier: Option<&str>,
    cleared: bool,
    lead_tier: Option<&str>,
    ran_tier: Option<&str>,
    ran_model: Option<&str>,
    text: &str,
    enabled: EnabledTiers,
    settings: &RoutingSettings,
) -> DispatchRoutingRow {
    use sha2::Digest as _;
    let text_sha256 = sha2::Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    DispatchRoutingRow::Route {
        ts,
        worker_id: worker_id.to_string(),
        plan_id: plan_id.to_string(),
        step,
        mode,
        text_sha256,
        label: decision
            .map(|decision| decision.label.clone())
            .unwrap_or_default(),
        confidence: decision.map_or(0.0, |decision| decision.confidence),
        probabilities: decision
            .map(|decision| decision.probabilities.clone())
            .unwrap_or_default(),
        tier: laya_tier.map(str::to_string),
        threshold: decision.map_or_else(
            || threshold_for(enabled.offered().count(), settings),
            |decision| {
                if decision.source == "knn" {
                    settings.knn_min_share
                } else {
                    threshold_for(enabled.offered().count(), settings)
                }
            },
        ),
        cleared,
        lead_tier: lead_tier.map(str::to_string),
        ran_tier: ran_tier.map(str::to_string),
        ran_model: ran_model.map(str::to_string),
        text: text.to_string(),
        skip: skip.map(str::to_string),
        source: decision.map(|decision| decision.source.to_string()),
        knn_label: decision.and_then(|decision| decision.knn_label.clone()),
        knn_share: decision.and_then(|decision| decision.knn_share),
        knn_k: decision.and_then(|decision| decision.knn_k),
    }
}

/// The `LeadOverride` corpus row an explicit choice logs when it overrode a
/// tier Laya cleared: shared by the inline path and the deferred
/// classification so both write identical rows. `lead_tier` is the tier
/// the choice resolved to (`custom` when it has none) and
/// `requested_tier` the tier the lead asked for when that differs.
#[allow(clippy::too_many_arguments)]
fn dispatch_lead_override_row(
    ts: u64,
    worker_id: &str,
    plan_id: &str,
    step: usize,
    laya_tier: &str,
    lead_tier: Option<&str>,
    requested_tier: Option<&str>,
    confidence: f64,
) -> DispatchRoutingRow {
    DispatchRoutingRow::LeadOverride {
        ts,
        worker_id: worker_id.to_string(),
        plan_id: plan_id.to_string(),
        step,
        laya_tier: Some(laya_tier.to_string()),
        lead_tier: lead_tier.unwrap_or("custom").to_string(),
        requested_tier: requested_tier.map(str::to_string),
        confidence,
    }
}

/// A dispatch answer: the compact worker entry next to the idempotent-reuse
/// flag. `data` keeps the whole record, as the single-worker answers do.
fn dispatch_result(
    result: Result<(WorkerRecord, bool, bool), String>,
    action: &str,
    mode: RoutingMode,
    fold_request: Option<String>,
    explicit_model: bool,
) -> ToolResult {
    match result {
        Ok((worker, reused, laya_pending)) => {
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
            if let Some(line) = dispatch_routing_line(mode, routing, laya_pending) {
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

fn dispatch_routing_line(
    mode: RoutingMode,
    routing: Option<&WorkerRouting>,
    laya_pending: bool,
) -> Option<String> {
    let routing = routing?;
    if mode == RoutingMode::Off {
        return None;
    }
    // An explicit-choice dispatch returns before its classification lands
    // (plan c74b1987): say so instead of misreporting the still-null label
    // as an offline classifier.
    if laya_pending {
        return Some(
            "Laya: classifying in the background; the label will land on the routing record."
                .to_string(),
        );
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

/// A bare worker record for tests in sibling modules (`notice_tag`): the
/// shape the team test fixture builds, without a checkout on disk.
#[cfg(test)]
pub(crate) fn test_worker_record(id: &str) -> WorkerRecord {
    tests::worker(std::path::Path::new("."), id)
}

#[cfg(test)]
#[path = "team/tests.rs"]
mod tests;
