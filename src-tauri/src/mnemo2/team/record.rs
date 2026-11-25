// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Worker records and their small helpers: the durable ledger shape,
//! the inbox, routing and status types, and the kickoff text.

use std::path::PathBuf;
use std::sync::Arc;

use mnemo::app::plan_board::PLAN_READ_TOOL_GUIDANCE;
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

impl WorkerStatus {
    /// Whether this worker has active work in progress.
    ///
    /// One definition for the MCP summaries and the machine-wide instances
    /// overview, so the two can never count differently.
    pub(crate) fn is_running(self) -> bool {
        matches!(
            self,
            Self::Dispatching | Self::Working | Self::Fixing | Self::Merging
        )
    }

    /// Whether this worker is blocked on the person's review or intervention.
    pub(crate) fn waiting_on_user(self) -> bool {
        matches!(
            self,
            Self::Completed
                | Self::InReview
                | Self::NeedsYou
                | Self::Failed
                | Self::Conflict
                | Self::Interrupted
        )
    }
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
    /// The worker's own Claude Code agent, posting a progress note.
    ClaudeAgent,
}

impl MessageSender {
    pub(super) fn header(self) -> &'static str {
        match self {
            Self::ExternalLead => "[From the lead (Claude Code)]",
            Self::Lead => "[From the built-in lead]",
            Self::User => "[From the user in mission control]",
            Self::Mnemo => "[From Mnemo2]",
            Self::ClaudeAgent => "[From the worker's Claude Code agent]",
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
    /// A one-line progress note from a Claude Code agent (`team_progress`).
    Progress,
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

/// Where a worker runs when it is not local: one checkout of one paired
/// worker node (plan 9a2f4b67, phase 3).
///
/// The record keeps the node's stable identity (`node_id`) and display
/// (`node_name`, `os`), the project key the hub sends with every repo call,
/// the node checkout name and its path there, the last commit the hub copied
/// into the worker's shadow checkout, and — while the node is away — the Unix
/// millisecond the link was first seen down.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteCheckout {
    /// The paired node's 32-hex id.
    pub node_id: String,
    /// The node's operator-visible name.
    pub node_name: String,
    /// The node's operating system (`windows` or `macos`).
    pub os: String,
    /// The 12-hex project key both ends name the project by.
    pub project: String,
    /// The checkout name under the node's `<home>/checkouts/`.
    pub checkout: String,
    /// The checkout's path on the node.
    pub path: String,
    /// The node tip whose branch the hub last copied into the shadow.
    #[serde(default)]
    pub synced_tip: Option<String>,
    /// When the node's link was first seen down, in Unix milliseconds.
    #[serde(default)]
    pub offline_since_ms: Option<u64>,
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
    /// Where the worker runs when it is not local. `None` is a local worker;
    /// old ledgers written before remote workers load unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<RemoteCheckout>,
    /// The real lifecycle state.
    pub status: WorkerStatus,
    /// The session generation within this worker (resume advances it).
    pub generation: u64,
    /// The conversation so far. Shared: cloning a record never copies the
    /// conversation. Assign a new `Arc` whenever the transcript changes and
    /// never mutate through it: the ledger compares transcripts with
    /// `Arc::ptr_eq`.
    pub transcript: Arc<Transcript>,
    /// How many transcript messages the ledger's writer saved to this worker's
    /// own file. Only meaningful on disk: the in-memory record and every IPC
    /// snapshot carry 0. A load uses it to tell a conversation that was lost
    /// (file gone, count above zero) from a worker that never had one.
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub saved_messages: usize,
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
    /// The final report of a Claude Code agent worker, recorded by
    /// `team_agent_done`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<String>,
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
    /// The worker's completed-turn count captured when the latest review was
    /// recorded. A finding only counts as "stayed open" between two reviews
    /// when this count has grown in between: a lead `review_fix` round never
    /// runs a worker turn, so it must not read as a failed worker round.
    /// `None` on records written before this was tracked (and before any
    /// review): unknown, and the repeat rule never fires on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_turns: Option<u32>,
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
    /// The Claude Code model this worker runs as (`opus`, `sonnet`, `haiku`,
    /// `fable`), when the step was handed to the lead's own Claude Code
    /// agent instead of a Mnemo worker session. Such a worker has no live
    /// session; `team_agent_done` closes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_agent: Option<String>,
    /// The Claude Code effort such a worker's agent runs at (`low`,
    /// `medium`, `high`, `xhigh`, or `max`); `None` when its tier sets none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_agent_effort: Option<String>,
    /// The id of the Claude Code agent the lead launched for this worker,
    /// recorded by `team_agent_started`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_agent_id: Option<String>,
    /// The agent's transcript file (`agent-<id>.jsonl`), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_agent_transcript: Option<String>,
    /// What Mnemo took away when it handed this agent worker back to its own
    /// models (plan ee73bd2f decision 3): set by the hand-back, cleared once
    /// a Mnemo session starts on the worker or it runs as an agent again.
    /// While set, no Mnemo model takes over a dirty checkout, and
    /// `team_agent_started` may reclaim the worker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handed_back: Option<HandedBack>,
    /// The one-time token Mnemo put on the first line of the launch prompt
    /// it last handed the lead for this agent worker. The agent gate
    /// auto-approves only a launch whose prompt carries it, and clears it
    /// when it does, so each handed-out prompt runs without a permission
    /// prompt exactly once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_token: Option<String>,
    /// The branches `team_integrate` merged into this worker's branch, in
    /// order (plan ca7f2177 decision 6).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub integrations: Vec<Integration>,
    /// What Mnemo tells a Mnemo worker about integrations at the start of
    /// its next message, consumed when that message is delivered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integration_note: Option<String>,
    /// The full sha of the lead commit this worker's checkout started from
    /// (advanced when an idle worker without commits is fast-forwarded).
    /// `None` for workers recorded before it existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_commit: Option<String>,
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

/// An agent worker's hand-back to Mnemo: when it happened and what the
/// worker was before, so `team_agent_started` can restore it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandedBack {
    /// When the worker was handed back, in Unix epoch milliseconds.
    pub at_ms: u64,
    /// The Claude Code model it ran as.
    pub claude_agent: String,
    /// The Claude Code effort its agent ran at, if its tier set one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_agent_effort: Option<String>,
    /// Its status before the hand-back: Working or Fixing.
    pub status: WorkerStatus,
}

/// One branch `team_integrate` merged into a worker's branch: the branch, the
/// merge commit it made, and when.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Integration {
    pub branch: String,
    pub commit: String,
    pub at_ms: u64,
}

/// The opening prompt of a worker that implements its pinned task straight
/// away.
pub(crate) const KICKOFF: &str = "Your task is pinned in your instructions under \"Your task (pinned)\": the plan, your step's spec and the lead's brief. Implement it exactly as specified. If the spec contradicts the code or leaves a decision open, message the lead (message_parent) before guessing.";

/// The opening prompt of a read-back worker: it reads its pinned task and the
/// files it names, asks its questions, and changes nothing until the lead
/// answers.
pub(crate) const READ_BACK_KICKOFF: &str = "Your task is pinned in your instructions under \"Your task (pinned)\". Before changing any file, read the spec and every file it names. Then send the lead one message (message_parent) listing each ambiguity, contradiction with the code, or decision the spec leaves open, numbered, with file and line, or exactly \"No questions.\" Then stop and wait. Start implementing only after the lead replies.";

/// The opening prompt of a Claude Code agent that implements its inline task.
pub(crate) const AGENT_KICKOFF: &str = "Your task is below under \"Your task\": the plan, your step's spec and the lead's brief. Implement it exactly as specified. If the spec contradicts the code or leaves a decision open, end your turn with the question instead of guessing; the lead answers and continues you.";

/// The opening prompt of a read-back Claude Code agent: it asks its questions
/// by ending its turn, not through a Mnemo worker tool it does not have.
pub(crate) const AGENT_READ_BACK_KICKOFF: &str = "Your task is below under \"Your task\". Before changing any file, read the spec and every file it names. Then end your turn with one message that lists each ambiguity, contradiction with the code, or decision the spec leaves open, numbered, with file and line, or exactly \"No questions.\" Change nothing until the lead replies.";

/// The first user message a worker's session receives: the short kickoff that
/// points at the task pinned in the system head.
pub(super) fn kickoff_message(record: &WorkerRecord) -> &'static str {
    if record.read_back {
        READ_BACK_KICKOFF
    } else {
        KICKOFF
    }
}

/// The full self-contained task text for a Claude Code agent: the same
/// kickoff and the same pinned plan/step/brief a Mnemo worker receives,
/// plus the rules that matter outside Mnemo — the prepared worktree and
/// branch to work in, and what never to do. A Mnemo worker gets the kickoff
/// as its first message and the task pinned in its system head; the agent
/// gets both inlined, because it has no Mnemo session.
pub(super) fn agent_prompt(record: &WorkerRecord) -> String {
    let pinned = record
        .opening_prompt
        .clone()
        .unwrap_or_else(|| record.brief.clone());
    // An agent uses no Mnemo tools (plan 4d920eec decision 6): the plan in
    // its prompt is what it works from, and the lead passes on any change.
    let pinned = pinned.replace(PLAN_READ_TOOL_GUIDANCE, "");
    let kickoff = if record.read_back {
        AGENT_READ_BACK_KICKOFF
    } else {
        AGENT_KICKOFF
    };
    format!(
        "{kickoff}\n\nYour task:\n\n{pinned}\n\nThis step runs as a Claude Code agent:\n- You run in your own checkout of this project on branch {branch}; commit your work there.\n- Your shell already starts in that checkout: run each command as it is, without a cd into the checkout first.\n- Run shell commands in the foreground. Start one in the background only when it will run longer than about two minutes.\n- Never push, never use git stash, never merge, never switch branches.\n- Where the task tells you to message the lead, end your turn with that message instead; the lead answers and continues you.\n- Do not use Mnemo's team_* or plan_* tools.\n- Verify with the narrowest tests that cover your change (the touched crate's tests, or the tests the task names). Mnemo runs the full checks in .coding/checks.toml on your final commit.\n- End with a report of the commit hash, the tests you ran and their results, and anything left open.",
        kickoff = kickoff,
        pinned = pinned,
        branch = record.branch,
    )
}

/// A fresh one-time launch token for an agent worker's next launch prompt:
/// random, so only a prompt Mnemo handed out can carry it.
pub(super) fn fresh_launch_token() -> String {
    format!("mnemo-launch-{}", uuid::Uuid::new_v4().simple())
}

/// `prompt` with the worker's launch token on its first line, where the
/// agent gate (which reads only the first 64 KiB of a prompt) always sees it.
/// A worker with no token gets the prompt unchanged.
pub(super) fn with_launch_token(prompt: String, token: Option<&str>) -> String {
    match token {
        Some(token) => format!(
            "Mnemo launch token: {token} (Mnemo's gate starts this agent without a permission prompt once)\n\n{prompt}"
        ),
        None => prompt,
    }
}

/// [`agent_prompt`] for a step a Mnemo worker on `previous_model` already
/// worked on in this checkout (plan e52e0be1 decision 5): the same prompt,
/// then where to look before changing anything. A record from before base
/// commits were recorded gets a plain `git log --oneline`.
pub(super) fn agent_continue_prompt(record: &WorkerRecord, previous_model: &str) -> String {
    let log = match record.base_commit.as_deref() {
        Some(base) => format!("git log --oneline {base}..HEAD"),
        None => "git log --oneline".to_string(),
    };
    format!(
        "{}\n\nA Mnemo worker on {previous_model} already worked on this step in this checkout. Before you change anything, run git status, {log} and git diff to see where it stopped, then finish the step.",
        agent_prompt(record)
    )
}

/// The refusal the tools that drive a Mnemo session give for a worker that
/// runs as a Claude Code agent: its step lives on the lead's own agent, so
/// Mnemo cannot message, resume, compact or route-fix it. `None` for an
/// ordinary worker.
pub(super) fn agent_worker_refusal(worker: &WorkerRecord) -> Option<String> {
    worker.claude_agent.as_ref().map(|_| {
        let id = &worker.id;
        match worker.status {
            WorkerStatus::Working | WorkerStatus::Interrupted => format!(
                "{id} runs as a Claude Code agent: continue it with your own Agent (SendMessage to it); Mnemo finishes the worker when the agent ends."
            ),
            WorkerStatus::Failed | WorkerStatus::Stopped | WorkerStatus::Cancelled => format!(
                "{id} runs as a Claude Code agent and has ended: dispatch the step again with a new worker_id."
            ),
            WorkerStatus::Merged | WorkerStatus::Merging => format!(
                "{id} ran as a Claude Code agent and is merged: there is nothing left to continue."
            ),
            _ => format!(
                "{id} runs as a Claude Code agent: fix it with your own Agent (SendMessage to it), then run team_check and team_review on the new tip."
            ),
        }
    })
}

/// The refusal `team_resume` and `team_compact` give a worker that runs as a
/// Claude Code agent (plan ca7f2177 decision 5): Mnemo has no session to
/// drive, so the lead continues it through `team_message`. `None` for an
/// ordinary worker.
pub(super) fn agent_resume_refusal(worker: &WorkerRecord) -> Option<String> {
    worker.claude_agent.as_ref().map(|_| {
        let id = &worker.id;
        match worker.status {
            WorkerStatus::Merged | WorkerStatus::Merging => format!(
                "{id} ran as a Claude Code agent and is merged: there is nothing left to continue."
            ),
            // team_message refuses these too, so point at a new dispatch.
            WorkerStatus::Failed | WorkerStatus::Stopped | WorkerStatus::Cancelled => format!(
                "{id} runs as a Claude Code agent and has ended: dispatch the step again with a new worker_id."
            ),
            _ => format!(
                "{id} runs as a Claude Code agent: Mnemo can't resume or compact it. Continue it with team_message (which gives you the SendMessage instruction) or start a new Agent."
            ),
        }
    })
}

/// The refusal checks, review and merge give while a Claude Code agent
/// worker is still Working or Interrupted. Unlike session-driving tools,
/// these operations become valid as soon as Mnemo finishes it (its agent
/// ended) or `team_agent_done` marks the run.
pub(super) fn unfinished_agent_error(worker: &WorkerRecord) -> Option<String> {
    (worker.claude_agent.is_some()
        && matches!(worker.status, WorkerStatus::Working | WorkerStatus::Interrupted))
    .then(|| {
        format!(
            "{} runs as a Claude Code agent that has not finished: wait for the agent to end; Mnemo then finishes the worker",
            worker.id
        )
    })
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

impl WorkerRecord {
    /// Whether this worker is blocked on the person's review or intervention.
    ///
    /// The status decides, with one audit exception: an audit worker's job is
    /// over once its report is delivered — it is never reviewed, merged or
    /// resumed, so a finished audit would otherwise stay `Completed` and
    /// "waiting" forever, inflating every Mission Control waiting count. An
    /// audit whose report has not been delivered still waits: it is running,
    /// or it failed and needs a resume. Implementation workers keep the plain
    /// status rule.
    pub(crate) fn waiting_on_user(&self) -> bool {
        self.status.waiting_on_user() && !audit_finished(self)
    }

    /// The host this worker runs on: `local`, or the paired node's name.
    ///
    /// Every user-facing place that tells a host apart (team_status, notices,
    /// the review evidence) reads it here so an unnamed remote worker can never
    /// look local.
    pub fn host_label(&self) -> &str {
        self.remote
            .as_ref()
            .map(|remote| remote.node_name.as_str())
            .unwrap_or("local")
    }
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

/// Serde helper: `saved_messages` is omitted from the ledger at 0, which is
/// every in-memory record and every worker that never saved a conversation.
fn is_zero_usize(value: &usize) -> bool {
    *value == 0
}

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
