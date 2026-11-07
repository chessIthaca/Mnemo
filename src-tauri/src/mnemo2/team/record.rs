// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Worker records and their small helpers: the durable ledger shape,
//! the inbox, routing and status types, and the kickoff text.

use std::path::PathBuf;

use mnemo::headless::{PendingInteraction, Transcript};
use serde::{Deserialize, Serialize};

use crate::mnemo2::dispatch_routing::DispatchRoutingRow;
use crate::mnemo2::phase3::{Finding, MergeRecord, ReviewRecord};

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
    pub(super) fn header(self) -> &'static str {
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
    pub(super) fn as_str(self) -> &'static str {
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
pub(super) fn kickoff_message(record: &WorkerRecord) -> &'static str {
    if record.read_back {
        READ_BACK_KICKOFF
    } else {
        KICKOFF
    }
}

/// The model a worker's session actually runs on: the lead's chosen spec, else
/// the id the live session resolved, else the routing default.
pub(super) fn session_model_spec(worker: &WorkerRecord) -> &str {
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
pub(super) fn note_model(worker: &mut WorkerRecord, spec: &str) {
    if worker.models_used.last().map(String::as_str) != Some(spec) {
        worker.models_used.push(spec.to_string());
    }
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
pub(super) fn audit_finished(record: &WorkerRecord) -> bool {
    record.kind == "audit"
        && (record.report_delivered || record.status == WorkerStatus::Completed)
}

/// The refusal every finished-audit gate carries.
pub(super) fn audit_finished_error(worker_id: &str) -> String {
    format!(
        "audit worker '{worker_id}' already delivered its report; dispatch a new audit with team_audit_dispatch"
    )
}

pub(super) fn validate_worker_id(worker_id: &str) -> Result<(), String> {
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

pub(super) const WORKER_INBOX_LIMIT: usize = 200;

pub(super) fn append_inbox(worker: &mut WorkerRecord, entry: InboxEntry) {
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
