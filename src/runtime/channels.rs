// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Channel contract — the decoupled agent↔UI boundary.
//!
//! The agent and UI share no mutable state. All communication is via typed
//! channels. An `AgentManager` sits between them: it spawns agents, routes
//! commands, and fans-in their events.
//!
//! **IPC note:** `AgentEvent::ApprovalRequest` carries a `oneshot::Sender`
//! which is NOT serializable across the Tauri IPC boundary. The IPC adapter
//! (`ipc/approval.rs`) holds the oneshot in a map keyed by `tool_call_id`,
//! and emits a serializable `ApprovalRequestData` to the frontend instead.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use crate::provider::{ApprovalPreview, FinishReason};
use crate::tool::ToolResult;
use crate::workflow::WorkflowState;

/// A unique agent identifier.
pub type AgentId = u64;

/// One clickable option in an `ask_user` question.
///
/// `label` is the short text on the button; `description` is an optional
/// longer explanation shown beneath it. Both are serializable across the IPC
/// boundary (the agent builds them; the frontend renders them).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuestionOption {
    /// The short button label.
    pub label: String,
    /// An optional longer description shown under the label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A per-role token breakdown of the context window, for the context popup.
/// Computed alongside the total token count at every `ContextUsage` emission.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ContextBreakdown {
    /// System messages (system prompt, constitution, summaries, volatile tail).
    pub system: u32,
    /// User messages (prompts, steers converted to user messages).
    pub user: u32,
    /// Assistant messages (text + tool calls).
    pub assistant: u32,
    /// Tool result messages.
    pub tool: u32,
}

/// A steering message's payload: the text plus any pasted image attachments
/// (base64 data URLs, `data:image/png;base64,...`), carried end-to-end
/// through the steer pipeline (command → fold/queue → injection) so a
/// steered image reaches the model exactly like a normal prompt's images.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteerPayload {
    /// The steer's text.
    pub text: String,
    /// Pasted image attachments (base64 data URLs); may be empty.
    pub images: Vec<String>,
}

impl From<&str> for SteerPayload {
    fn from(text: &str) -> Self {
        SteerPayload {
            text: text.to_string(),
            images: Vec::new(),
        }
    }
}

impl From<String> for SteerPayload {
    fn from(text: String) -> Self {
        SteerPayload {
            text,
            images: Vec::new(),
        }
    }
}

/// UI → Agent (pushed to the agent's inbox).
#[derive(Debug, Clone)]
pub enum AgentCommand {
    /// Start a new turn with a user prompt. `images` is a list of base64 data
    /// URLs (`data:image/png;base64,...`) for pasted image attachments. When
    /// non-empty, the agent builds a multipart user message (text + image_url
    /// blocks).
    Prompt { text: String, images: Vec<String> },
    /// Mid-work guidance — queued, injected at the next turn boundary.
    /// Carries the steer's images alongside its text so they ride the same
    /// fold → injection pipeline (injected as a user message with image
    /// blocks, exactly like a normal prompt's images).
    Suggestion(SteerPayload),
    /// Cancel a queued steer whose text matches — drops it before it's
    /// injected (the "x" on a pending steer in the backlog). Sent by the
    /// frontend when the user dismisses a pending steer. Handled by
    /// [`crate::agent::StopReason::fold`] (removes the text from any
    /// `Steer`/`InterruptWithSteers`/`CompactWithSteers` list, collapsing an
    /// empty list) and the provider-request buffer; a no-op between turns
    /// (nothing is queued — a between-turn `Suggestion` is consumed
    /// immediately as a turn). Matching is by text, so two pending steers
    /// with identical text are both cancelled.
    CancelSuggestion(String),
    /// Cancel the current LLM generation.
    Interrupt,
    /// Stop the agent entirely.
    Cancel,
    /// Manually compact the context — summarize old messages into a summary
    /// system message, keeping the system prompt + recent messages. Processed
    /// between turns; if sent mid-turn, the turn ends first (via
    /// [`crate::agent::StopReason::Compact`]) so the compaction runs on a
    /// quiescent conversation. Backed by the `/compact` slash command + the
    /// context-popup Compact button.
    Compact,
    /// Clear the conversation history entirely — a fresh start. Processed
    /// between turns; if sent mid-turn, the turn ends first (via
    /// [`crate::agent::StopReason::Clear`]). Backed by the `/new` slash
    /// command.
    Clear,
}

/// The marker prefix of an unattended (run-all) dispatched prompt.
///
/// `run_all_prompt` (src-tauri/src/ipc/run_all.rs) builds every run-all
/// dispatched prompt with this prefix — the unattended-mode preamble rides
/// INSIDE the Prompt text (one command, one turn) — and the agent task's
/// `Prompt` arm detects it to set its unattended mode: the runtime's only
/// harness-visible dispatch-context signal. In unattended mode the
/// auto-continue gate also covers Planning (a premature turn end
/// mid-exploration would otherwise park with nobody watching and halt the
/// whole run); interactive Planning still parks (a deliberate turn end
/// there is usually a question-wait for the user).
pub const UNATTENDED_PROMPT_PREFIX: &str = "[backlog run-all — unattended mode]";

/// One recalled memory in a [`AgentEvent::MemoryRecalled`] payload: its tier
/// + title, so the transcript entry can expand to the full hit list (user
/// request 2026-08-20). The recall limit already caps the vec.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RecallHit {
    /// The memory tier (snake_case, e.g. "semantic").
    pub tier: String,
    /// The memory's title.
    pub title: String,
}

/// Why an agent parked (went idle awaiting input) instead of
/// auto-continuing — the pre-stall evidence carried by
/// [`AgentEvent::Parked`] so the watchdog ring and the UI can distinguish a
/// deliberate wait from a stall needing manual input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParkReason {
    /// The user pressed Stop — the turn was interrupted mid-flight. Never
    /// auto-resumed (deliberate); the UI shows an "interrupted" state so it
    /// does not look like a hang.
    Interrupted,
    /// The auto-continue budget (MAX_AUTO_CONTINUE consecutive synthetic
    /// turns) ran out while the workflow still expected progress — a
    /// possible genuine deadlock; needs a nudge.
    BudgetExhausted,
    /// Spawned descendants (reviewers, parallel workers) are still running —
    /// the designed spawn→end-turn wait; the child-completion Suggestion
    /// resumes the agent.
    WaitingForDescendants,
    /// The workflow does not expect progress (Planning/Complete/Skill) —
    /// the agent is idle between tasks by design.
    NoWorkExpected,
}

/// Agent → UI (fanned-in through the manager, tagged with AgentId).
///
/// This enum is NOT directly serializable because `ApprovalRequest` carries a
/// `oneshot::Sender`. Use `AgentEvent::to_serializable()` to convert it for
/// the IPC boundary (the adapter holds the oneshot separately).
#[derive(Debug)]
pub enum AgentEvent {
    /// The agent started processing.
    Started,
    /// A fragment of assistant text.
    TextDelta(String),
    /// A fragment of reasoning text (reasoning models).
    ReasoningDelta(String),
    /// The start of a tool call.
    ToolCallStart {
        index: u32,
        id: String,
        name: String,
    },
    /// A fragment of a tool-call's arguments.
    ToolCallArgDelta { index: u32, fragment: String },
    /// A chunk of a running tool call's output — the LIVE VIEW of a call that
    /// has not finished yet (user request 2027-01-16: `shell` used to show
    /// nothing until the child exited).
    ///
    /// DISPLAY-ONLY: what the model consumes is the final
    /// [`AgentEvent::ToolResult`], which carries the complete output. Chunks
    /// arrive in per-stream order and always precede the matching `ToolResult`
    /// (the tool joins its readers before returning). A consumer must ignore a
    /// delta whose call already has a result — a timed-out or cancelled call can
    /// leave a reader emitting after its result was sent.
    ToolOutputDelta {
        tool_call_id: String,
        stream: crate::tool::ToolOutputStream,
        text: String,
    },
    /// The agent requests approval for a mutating action.
    ///
    /// `core_operation` is true when the call is a core operation (e.g.
    /// `git merge` / `git push`) that must always prompt regardless of safety
    /// mode or rules — the UI hides the no-op "Mark Safe" / "Allow for
    /// project" buttons for these, and the IPC layer never auto-resolves
    /// them on a mode change.
    ApprovalRequest {
        tool_call_id: String,
        tool_name: String,
        args: Value,
        preview: Option<ApprovalPreview>,
        core_operation: bool,
        responder: oneshot::Sender<Approval>,
    },
    /// The result of a tool call.
    ToolResult {
        tool_call_id: String,
        result: ToolResult,
    },
    /// Token usage for a completed LLM request within a turn.
    Usage {
        prompt_tokens: u32,
        completion_tokens: u32,
        reasoning_tokens: u32,
        /// Prompt tokens served from the provider's cache (subset of
        /// prompt_tokens — they're already included, reported for pricing).
        cached_tokens: u32,
        /// Time-to-first-token (ms): POST → first streamed chunk. `None` when
        /// timing wasn't captured. Input tok/sec = prompt_tokens / ttft_ms.
        ttft_ms: Option<u32>,
        /// Generation time (ms): first chunk → usage event. `None` when timing
        /// wasn't captured. Output tok/sec = completion_tokens / generation_ms.
        generation_ms: Option<u32>,
    },
    /// Context window usage — tokens used vs. the window max, with a
    /// per-role breakdown (system/user/assistant/tool) for the context popup.
    ContextUsage {
        used: u32,
        max: u32,
        breakdown: ContextBreakdown,
        /// Lever 6's S–F quality grade (backlog e4a50d22). Graded at the
        /// top-of-loop and post-compaction emissions, plus the `/new` reset
        /// (fill 0). `None` when the `[general.optimizer] quality_score` flag
        /// is off, on the mid-stream provider-exact re-anchor, and on the
        /// app-side spawn seed — the frontend reads an absent grade as
        /// "unchanged", so the popup keeps the last one instead of blanking.
        quality: Option<crate::agent::optimizer::QualityReport>,
    },
    /// The workflow state changed.
    WorkflowStateChanged {
        state: WorkflowState,
        /// The id of the ROOT plan (bottom of the plan stack), if any. Constant
        /// across sub-plan push/pop and skill transitions; changes only when a
        /// fresh top-level plan starts (`create_plan` from Planning or
        /// Complete). Lets the frontend reset per-top-plan UI (e.g. the Diff
        /// tab's changed-file list) exactly when a new top-level plan begins.
        top_plan_id: Option<String>,
    },
    /// A plan step was completed. `step_index` is the 1-indexed step number
    /// (1 = the first step — the number shown in the plan document and in
    /// complete_step's result echo).
    StepCompleted { step_index: u32 },
    /// A queued user suggestion was injected into the conversation as a
    /// system message (at a turn boundary, after summarization, or after an
    /// approval resolved). Lets the UI mark the steer as "landed" and
    /// highlight it in the agent window. `images` is the steer's pasted
    /// image attachments (base64 data URLs; may be empty) — carried through
    /// so the transcript's steer entry can render them like a user prompt's.
    SuggestionInjected { text: String, images: Vec<String> },
    /// A prompt was dispatched to this agent from outside the main input —
    /// currently the backlog dispatch paths (manual ▶, auto-feed, Run-All).
    /// Emitted by the dispatcher (not the agent itself) right after sending
    /// the `Prompt` command, so the frontend can show the prompt as the goal
    /// at the top of the transcript — mirroring how the main input optimistically
    /// appends the user message before the agent starts streaming. `images` is
    /// the list of base64 data URLs the backlog item carried (may be empty).
    PromptDispatched { text: String, images: Vec<String> },
    /// A skill was started on this agent from outside the normal tool-call
    /// pipeline — currently the toolbar path (`enter_skill` / the "Merge to
    /// main" button). Emitted by `enter_skill` right after `start_skill`
    /// succeeds, so the frontend can announce the skill in the transcript like
    /// a tool call does (`▶ skill "name" start`), mirroring how `skill_end`
    /// already appears. The agent-driven `skill_start` tool announces itself
    /// via its own ToolCard, so it does NOT emit this event (that would
    /// duplicate). `name` is the skill name; `prompt` is the goal the agent
    /// drives toward while the skill is active.
    SkillStarted { name: String, prompt: String },
    /// The active model changed for this agent — emitted by the IPC layer
    /// (`set_model` / `save_endpoints`) right after the provider is swapped
    /// into every live agent loop, so the frontend can update the per-agent
    /// model label (`agentModels`) immediately without a `list_agents` poll.
    /// `model` is the new effective model id (the swapped provider's model).
    /// `provider` is the `endpoints.toml` endpoint name serving that model —
    /// the frontend needs it to label the provider correctly when the same
    /// model id is listed under two endpoints (first-match resolution cannot
    /// disambiguate; `None`/empty makes the frontend fall back to its
    /// endpoint-list resolution).
    /// This is a display event: it carries no oneshot and does not affect the
    /// agent's turn (the swap takes effect on the next turn).
    ModelChanged {
        model: String,
        provider: Option<String>,
        /// The DISPLAY-space effective reasoning effort of the model now
        /// serving (`"off" | "low" | "medium" | "high" | "max"`), `None`
        /// when unknown (mock-backed loops) — the UI falls back to the
        /// toolbar echo / endpoint default. The same resolution the request
        /// builder uses, in UI vocabulary (backlog 51dab4da: the status bar
        /// must show the effort of the model actually in use, never a stale
        /// value).
        reasoning_effort: Option<String>,
    },
    /// The per-turn AUTO-RECALL just injected memories into the agent's
    /// prompt — emitted once per FRESH recall that returned hits (not on
    /// cache reuse or empty results), so the frontend can show that the
    /// memory system is being used ("a notification when semantic memory is
    /// accessed", user request 2026-04-20). `hits` carries the tier + title
    /// of every recalled memory so the transcript entry can expand to the
    /// full list (user request 2026-08-20). Display-only (no oneshot, no
    /// effect on the turn).
    MemoryRecalled { hits: Vec<RecallHit> },
    /// The agent asked the user a question via the `ask_user` tool and is
    /// paused awaiting the answer. Mirrors [`AgentEvent::ApprovalRequest`]:
    /// the `oneshot::Sender` can't cross the IPC boundary, so the adapter
    /// holds it in a pending-questions map keyed by `question_id` and emits a
    /// serializable form to the frontend. The agent task blocks on the
    /// receiver until the user answers (one question at a time — the turn
    /// can't proceed, so a second question can't be asked until this one is
    /// answered).
    UserQuestion {
        /// A unique id for this question (used to match the answer).
        question_id: String,
        /// The question text.
        question: String,
        /// The clickable options (label + optional description).
        options: Vec<QuestionOption>,
        /// Resolved by the `answer_question` command.
        responder: oneshot::Sender<UserAnswer>,
    },
    /// The agent finished a turn.
    ///
    /// Emitted at the end of every turn — the agent is idle but still alive
    /// and ready to accept new prompts. The forwarder should set `running =
    /// false` but NOT remove the agent.
    Finished { reason: FinishReason },
    /// The agent's turn moved into a new phase of the request loop.
    ///
    /// The phases cycle per request: [`PhaseKind::Sending`] (local prep
    /// only — token counting, auto-recall, prompt build, model resolution),
    /// with [`PhaseKind::Compacting`] overlaid while an auto-compaction
    /// summary call runs inside that window, [`PhaseKind::Waiting`]
    /// (network-bound: TCP/TLS connect, POST upload, time-to-first-token,
    /// AND retry backoff sleeps — everything from handing off to the
    /// network until the first content byte arrives; a mid-stream stall
    /// keeps the current streaming phase), [`PhaseKind::Reasoning`]
    /// (reasoning deltas arriving — the model is thinking),
    /// [`PhaseKind::Streaming`] (answer/tool-call deltas arriving — the
    /// model is answering), and [`PhaseKind::RunningTools`]
    /// (tool calls executing between requests). The frontend's inflight bar
    /// renders the current phase instead of a static "thinking…" label, with
    /// approval/question pauses overlaid on top.
    Phase { phase: PhaseKind },
    /// The agent task has exited (inbox closed or cancelled).
    ///
    /// Emitted only when the agent task truly terminates — the forwarder
    /// should remove the agent from the manager so `list_agents` stays
    /// accurate. This is distinct from `Finished`, which fires at the end of
    /// every turn while the agent remains alive.
    Exited,
    /// A tool-spawned background agent finished its task.
    ///
    /// Emitted by the event forwarder (not the agent itself) when an agent
    /// with a recorded `parent_id` reaches `Finished` (or a final `Error`).
    /// The forwarder uses it to route a completion notification back to the
    /// parent agent, and the frontend can surface a "done" marker on the child
    /// in the sidebar. `success` is `false` when the task ended in an error.
    ChildFinished {
        child_id: AgentId,
        name: String,
        success: bool,
    },
    /// The agent encountered an error.
    ///
    /// `retrying` is `true` when this is a transient retry note (the agent will
    /// try again) and `false` for a final failure (the agent is stopping). The
    /// frontend uses this boolean — not string-matching on the error text — to
    /// decide whether to keep `running = true`.
    Error { error: String, retrying: bool },
    /// Context compaction completed — token count before/after.
    ///
    /// Emitted after a completed manual (`/compact` slash command, the
    /// context-popup Compact button) or automatic threshold-triggered
    /// compaction so the UI can confirm the reduction in the transcript
    /// (`Context compacted: 500K → 24K`) — or, when nothing was compacted
    /// (already below the threshold / too few messages), a no-change note so
    /// every [`AgentEvent::CompactStarted`] has a paired end. Not emitted
    /// when the summarization call fails — that path emits
    /// [`AgentEvent::Error`] instead.
    Compacted { before: u32, after: u32 },
    /// Context compaction started — the summarization LLM call is running.
    ///
    /// Emitted by every compaction path (manual `/compact`, the context-popup
    /// Compact button, mid-turn compact, and auto-compaction) so the UI can
    /// announce "Compacting context…" in the transcript; paired with
    /// [`AgentEvent::Compacted`] on completion or [`AgentEvent::Error`] on
    /// failure.
    CompactStarted,
    /// A vision-model image description is starting — the agent is about to
    /// ask the configured vision model to describe a pasted image attachment
    /// (the text-only-provider fallback in the agent task's prompt loop).
    ///
    /// Emitted BEFORE each vision round-trip so the UI can announce
    /// "image parsing" in the transcript instead of pausing silently
    /// (user report: prompts with images seemed to hang). `index` is the
    /// 1-based image number and `total` the attachment count; `query` is the
    /// prompt sent to the vision model. Paired with exactly one
    /// [`AgentEvent::VisionDescribed`].
    VisionDescribe {
        /// 1-based position of this image among the prompt's attachments.
        index: usize,
        /// Total number of image attachments in the prompt.
        total: usize,
        /// The prompt text sent to the vision model (the default describe
        /// prompt for attachments — shown in the transcript card).
        query: String,
    },
    /// A vision-model image description finished — the counterpart to
    /// [`AgentEvent::VisionDescribe`].
    ///
    /// Emitted once per [`AgentEvent::VisionDescribe`], after the vision call
    /// resolves. On success `description` holds the vision model's answer
    /// (the text folded into the prompt); on failure it holds the error text
    /// and `success` is false (the prompt continues with a
    /// "description unavailable" note — the turn is not aborted).
    VisionDescribed {
        /// 1-based position of this image among the prompt's attachments.
        index: usize,
        /// Total number of image attachments in the prompt.
        total: usize,
        /// Whether the vision call succeeded.
        success: bool,
        /// The vision model's answer, or the error text when `success` is
        /// false.
        description: String,
    },
    /// The agent parked (went idle awaiting input) instead of
    /// auto-continuing — the pre-stall evidence for the watchdog ring and
    /// the UI.
    ///
    /// Emitted at the main-agent park sites in `run_turn_with_retry` when a
    /// turn ends without an auto-continue: a user Stop (interrupt), the
    /// auto-continue budget running out while the workflow still expects
    /// progress, the designed wait while spawned descendants run, and idle
    /// states that expect no progress. Carries the full pre-stall state —
    /// workflow state, whether descendants are running, the auto-continue
    /// streak — so a live stall is diagnosable from the watchdog activity
    /// ring without reproduction (2027-01-07: two live parks needed a manual
    /// "c" and the cause had to be inferred). The frontend surfaces the two
    /// manual-input reasons (`Interrupted`, `BudgetExhausted`) as a distinct
    /// banner so an interrupted turn does not look like a hang; the
    /// by-design reasons are evidence-only. NOT emitted at subagents'
    /// routine turn-end parks (they would flood the ring); a user Stop on
    /// a subagent still emits `Interrupted` — rare, user-initiated, and
    /// worth the evidence.
    Parked {
        /// Why the agent parked.
        reason: ParkReason,
        /// The workflow state at the park (evidence label, e.g. "Reviewing").
        workflow_state: String,
        /// Whether spawned descendants were still running.
        descendants_running: bool,
        /// The auto-continue streak at the park.
        auto_continue_streak: u32,
    },
}

/// The phases of the agent's request loop, reported via
/// [`AgentEvent::Phase`] so the UI can show what the agent is doing instead
/// of a static "thinking…" label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseKind {
    /// Local prep only: token counting, auto-recall embedding search, prompt
    /// build, model resolution. Excludes all network time — once the request
    /// is handed to the network, the phase flips to [`Self::Waiting`].
    Sending,
    /// Auto-compaction is running inside the sending window: the
    /// summarization LLM call that shrinks the conversation before the
    /// request. Emitted right before that call; `Sending` is re-emitted
    /// when it finishes.
    Compacting,
    /// Network-bound: TCP/TLS connect, POST upload, time-to-first-token,
    /// AND retry backoff sleeps — everything from handing off to the
    /// network until the first content byte arrives. Emitted right before
    /// the provider request so a 30s connect timeout or a 1s/2s backoff
    /// sleep shows as "waiting", not a multi-second "sending" stall.
    /// A mid-stream stall (byte-silence after streaming began) deliberately
    /// does NOT flip back to Waiting — it keeps Reasoning/Streaming,
    /// matching the trace graphs where stall_ms is part of the generate
    /// window.
    Waiting,
    /// Reasoning deltas are streaming in — the model is thinking (DeepSeek
    /// thinking mode / GLM `reasoning_content` / Ollama `reasoning` / inline
    /// `<think>` blocks). Distinct from [`Self::Streaming`] so the inflight
    /// bar can show "reasoning…" vs "answering…".
    Reasoning,
    /// Answer content or tool-call deltas are streaming in — the model is
    /// generating the visible reply.
    Streaming,
    /// Tool calls from the previous response are executing between requests.
    RunningTools,
}

/// A serializable version of `AgentEvent` for the IPC boundary.
///
/// `ApprovalRequest` becomes `ApprovalRequest` without the `responder` — the
/// adapter holds the oneshot in a pending-approvals map.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SerializableAgentEvent {
    Started,
    TextDelta {
        text: String,
    },
    ReasoningDelta {
        text: String,
    },
    ToolCallStart {
        index: u32,
        id: String,
        name: String,
    },
    ToolCallArgDelta {
        index: u32,
        fragment: String,
    },
    /// A chunk of a running tool call's output. See
    /// [`AgentEvent::ToolOutputDelta`] — display-only, stream-ordered, and
    /// always ahead of the matching `ToolResult`.
    ToolOutputDelta {
        tool_call_id: String,
        stream: crate::tool::ToolOutputStream,
        text: String,
    },
    ApprovalRequest {
        tool_call_id: String,
        tool_name: String,
        args: Value,
        preview: Option<ApprovalPreview>,
        /// Whether this is a core operation (git merge/push) that always
        /// prompts regardless of mode/rules. The UI hides the no-op
        /// "Mark Safe" / "Allow for project" buttons when true, and the IPC
        /// layer never auto-resolves these on a mode change.
        core_operation: bool,
    },
    ToolResult {
        tool_call_id: String,
        result: ToolResult,
    },
    Usage {
        prompt_tokens: u32,
        completion_tokens: u32,
        reasoning_tokens: u32,
        cached_tokens: u32,
        ttft_ms: Option<u32>,
        generation_ms: Option<u32>,
    },
    ContextUsage {
        used: u32,
        max: u32,
        breakdown: ContextBreakdown,
        /// Mirrors [`AgentEvent::ContextUsage::quality`]. Omitted from the wire
        /// when `None`, so the pre-lever JSON shape is unchanged.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        quality: Option<crate::agent::optimizer::QualityReport>,
    },
    WorkflowStateChanged {
        state: WorkflowState,
        /// The id of the ROOT plan (bottom of the plan stack), if any. Constant
        /// across sub-plan push/pop and skill transitions; changes only when a
        /// fresh top-level plan starts. Lets the frontend reset per-top-plan
        /// UI (e.g. the Diff tab's changed-file list) when the root plan
        /// changes. Mirrors [`AgentEvent::WorkflowStateChanged`].
        top_plan_id: Option<String>,
    },
    StepCompleted {
        step_index: u32,
    }, // 1-indexed step number (1 = first step)
    /// A queued user suggestion was injected into the conversation. See
    /// [`AgentEvent::SuggestionInjected`].
    SuggestionInjected {
        text: String,
        images: Vec<String>,
    },
    /// A prompt was dispatched to this agent from outside the main input
    /// (backlog dispatch). See [`AgentEvent::PromptDispatched`].
    PromptDispatched {
        text: String,
        images: Vec<String>,
    },
    /// A skill was started on this agent from the toolbar path. See
    /// [`AgentEvent::SkillStarted`].
    SkillStarted {
        name: String,
        prompt: String,
    },
    /// The active model changed for this agent. See
    /// [`AgentEvent::ModelChanged`]. Emitted by the IPC layer right after a
    /// provider swap so the frontend updates the per-agent model label without
    /// a `list_agents` poll. `provider` (the serving endpoint's
    /// `endpoints.toml` name) is absent when unknown — the frontend then falls
    /// back to resolving the model id itself. `reasoning_effort` is the
    /// DISPLAY-space effective effort of the model now serving (absent when
    /// unknown — the frontend falls back to the toolbar echo / endpoint
    /// default).
    ModelChanged {
        model: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_effort: Option<String>,
    },
    /// The per-turn auto-recall injected memories into the prompt. See
    /// [`AgentEvent::MemoryRecalled`]. Display-only.
    MemoryRecalled {
        hits: Vec<RecallHit>,
    },
    /// The agent asked the user a question (the `ask_user` tool) and is
    /// paused awaiting the answer. See [`AgentEvent::UserQuestion`]. The
    /// `responder` is held by the IPC adapter's pending-questions map.
    UserQuestion {
        question_id: String,
        question: String,
        options: Vec<QuestionOption>,
    },
    Finished {
        reason: FinishReason,
    },
    /// The turn moved into a new request-loop phase. See
    /// [`AgentEvent::Phase`].
    Phase {
        phase: PhaseKind,
    },
    Exited,
    /// A tool-spawned background agent finished its task. See
    /// [`AgentEvent::ChildFinished`].
    ChildFinished {
        child_id: AgentId,
        name: String,
        success: bool,
    },
    Error {
        error: String,
        retrying: bool,
    },
    /// Context compaction completed — token count before/after. See
    /// [`AgentEvent::Compacted`].
    Compacted {
        before: u32,
        after: u32,
    },
    /// Context compaction started. See [`AgentEvent::CompactStarted`].
    CompactStarted,
    /// A vision-model image description is starting. See
    /// [`AgentEvent::VisionDescribe`].
    VisionDescribe {
        /// 1-based position of this image among the prompt's attachments.
        index: u32,
        /// Total number of image attachments in the prompt.
        total: u32,
        /// The prompt text sent to the vision model.
        query: String,
    },
    /// A vision-model image description finished. See
    /// [`AgentEvent::VisionDescribed`].
    VisionDescribed {
        /// 1-based position of this image among the prompt's attachments.
        index: u32,
        /// Total number of image attachments in the prompt.
        total: u32,
        /// Whether the vision call succeeded.
        success: bool,
        /// The vision model's answer, or the error text when `success` is
        /// false.
        description: String,
    },
    /// The agent parked awaiting input — pre-stall evidence. See
    /// [`AgentEvent::Parked`].
    Parked {
        /// Why the agent parked.
        reason: ParkReason,
        /// The workflow state at the park (evidence label).
        workflow_state: String,
        /// Whether spawned descendants were still running.
        descendants_running: bool,
        /// The auto-continue streak at the park.
        auto_continue_streak: u32,
    },
}

/// The output of converting an [`AgentEvent`] to its serializable form: the
/// serializable event itself plus any non-serializable oneshot senders the IPC
/// adapter must hold separately (an approval sender and/or a question sender).
///
/// Both are `Option` because most events carry neither; `ApprovalRequest`
/// carries only an approval sender, `UserQuestion` carries only a question
/// sender. They never co-occur on a single event.
#[derive(Debug)]
pub struct SerializedEvent {
    /// The serializable event to emit to the frontend.
    pub event: SerializableAgentEvent,
    /// The approval oneshot, when the event was an `ApprovalRequest`.
    pub approval_sender: Option<oneshot::Sender<Approval>>,
    /// The question oneshot, when the event was a `UserQuestion`.
    pub question_sender: Option<oneshot::Sender<UserAnswer>>,
}

impl AgentEvent {
    /// Convert to a serializable form for the IPC boundary.
    ///
    /// Returns a [`SerializedEvent`] holding the serializable event plus any
    /// non-serializable oneshot senders the adapter must hold in a pending map
    /// (an approval sender for `ApprovalRequest`, a question sender for
    /// `UserQuestion`).
    pub fn into_serializable(self) -> SerializedEvent {
        match self {
            AgentEvent::Started => SerializedEvent {
                event: SerializableAgentEvent::Started,
                approval_sender: None,
                question_sender: None,
            },
            AgentEvent::TextDelta(text) => SerializedEvent {
                event: SerializableAgentEvent::TextDelta { text },
                approval_sender: None,
                question_sender: None,
            },
            AgentEvent::ReasoningDelta(text) => SerializedEvent {
                event: SerializableAgentEvent::ReasoningDelta { text },
                approval_sender: None,
                question_sender: None,
            },
            AgentEvent::ToolCallStart { index, id, name } => SerializedEvent {
                event: SerializableAgentEvent::ToolCallStart { index, id, name },
                approval_sender: None,
                question_sender: None,
            },
            AgentEvent::ToolCallArgDelta { index, fragment } => SerializedEvent {
                event: SerializableAgentEvent::ToolCallArgDelta { index, fragment },
                approval_sender: None,
                question_sender: None,
            },
            AgentEvent::ToolOutputDelta {
                tool_call_id,
                stream,
                text,
            } => SerializedEvent {
                event: SerializableAgentEvent::ToolOutputDelta {
                    tool_call_id,
                    stream,
                    text,
                },
                approval_sender: None,
                question_sender: None,
            },
            AgentEvent::ApprovalRequest {
                tool_call_id,
                tool_name,
                args,
                preview,
                core_operation,
                responder,
            } => SerializedEvent {
                event: SerializableAgentEvent::ApprovalRequest {
                    tool_call_id,
                    tool_name,
                    args,
                    preview,
                    core_operation,
                },
                approval_sender: Some(responder),
                question_sender: None,
            },
            AgentEvent::ToolResult {
                tool_call_id,
                result,
            } => SerializedEvent {
                event: SerializableAgentEvent::ToolResult {
                    tool_call_id,
                    result,
                },
                approval_sender: None,
                question_sender: None,
            },
            AgentEvent::Usage {
                prompt_tokens,
                completion_tokens,
                reasoning_tokens,
                cached_tokens,
                ttft_ms,
                generation_ms,
            } => SerializedEvent {
                event: SerializableAgentEvent::Usage {
                    prompt_tokens,
                    completion_tokens,
                    reasoning_tokens,
                    cached_tokens,
                    ttft_ms,
                    generation_ms,
                },
                approval_sender: None,
                question_sender: None,
            },
            AgentEvent::ContextUsage {
                used,
                max,
                breakdown,
                quality,
            } => SerializedEvent {
                event: SerializableAgentEvent::ContextUsage {
                    used,
                    max,
                    breakdown,
                    quality,
                },
                approval_sender: None,
                question_sender: None,
            },
            AgentEvent::WorkflowStateChanged { state, top_plan_id } => SerializedEvent {
                event: SerializableAgentEvent::WorkflowStateChanged { state, top_plan_id },
                approval_sender: None,
                question_sender: None,
            },
            AgentEvent::StepCompleted { step_index } => SerializedEvent {
                event: SerializableAgentEvent::StepCompleted { step_index },
                approval_sender: None,
                question_sender: None,
            },
            AgentEvent::SuggestionInjected { text, images } => SerializedEvent {
                event: SerializableAgentEvent::SuggestionInjected { text, images },
                approval_sender: None,
                question_sender: None,
            },
            AgentEvent::PromptDispatched { text, images } => SerializedEvent {
                event: SerializableAgentEvent::PromptDispatched { text, images },
                approval_sender: None,
                question_sender: None,
            },
            AgentEvent::SkillStarted { name, prompt } => SerializedEvent {
                event: SerializableAgentEvent::SkillStarted { name, prompt },
                approval_sender: None,
                question_sender: None,
            },
            AgentEvent::ModelChanged {
                model,
                provider,
                reasoning_effort,
            } => SerializedEvent {
                event: SerializableAgentEvent::ModelChanged {
                    model,
                    provider,
                    reasoning_effort,
                },
                approval_sender: None,
                question_sender: None,
            },
            AgentEvent::MemoryRecalled { hits } => SerializedEvent {
                event: SerializableAgentEvent::MemoryRecalled { hits },
                approval_sender: None,
                question_sender: None,
            },
            AgentEvent::UserQuestion {
                question_id,
                question,
                options,
                responder,
            } => SerializedEvent {
                event: SerializableAgentEvent::UserQuestion {
                    question_id,
                    question,
                    options,
                },
                approval_sender: None,
                question_sender: Some(responder),
            },
            AgentEvent::Finished { reason } => SerializedEvent {
                event: SerializableAgentEvent::Finished { reason },
                approval_sender: None,
                question_sender: None,
            },
            AgentEvent::Phase { phase } => SerializedEvent {
                event: SerializableAgentEvent::Phase { phase },
                approval_sender: None,
                question_sender: None,
            },
            AgentEvent::Exited => SerializedEvent {
                event: SerializableAgentEvent::Exited,
                approval_sender: None,
                question_sender: None,
            },
            AgentEvent::ChildFinished {
                child_id,
                name,
                success,
            } => SerializedEvent {
                event: SerializableAgentEvent::ChildFinished {
                    child_id,
                    name,
                    success,
                },
                approval_sender: None,
                question_sender: None,
            },
            AgentEvent::Error { error, retrying } => SerializedEvent {
                event: SerializableAgentEvent::Error { error, retrying },
                approval_sender: None,
                question_sender: None,
            },
            AgentEvent::Compacted { before, after } => SerializedEvent {
                event: SerializableAgentEvent::Compacted { before, after },
                approval_sender: None,
                question_sender: None,
            },
            AgentEvent::CompactStarted => SerializedEvent {
                event: SerializableAgentEvent::CompactStarted,
                approval_sender: None,
                question_sender: None,
            },
            AgentEvent::VisionDescribe {
                index,
                total,
                query,
            } => SerializedEvent {
                event: SerializableAgentEvent::VisionDescribe {
                    index: index as u32,
                    total: total as u32,
                    query,
                },
                approval_sender: None,
                question_sender: None,
            },
            AgentEvent::VisionDescribed {
                index,
                total,
                success,
                description,
            } => SerializedEvent {
                event: SerializableAgentEvent::VisionDescribed {
                    index: index as u32,
                    total: total as u32,
                    success,
                    description,
                },
                approval_sender: None,
                question_sender: None,
            },
            AgentEvent::Parked {
                reason,
                workflow_state,
                descendants_running,
                auto_continue_streak,
            } => SerializedEvent {
                event: SerializableAgentEvent::Parked {
                    reason,
                    workflow_state,
                    descendants_running,
                    auto_continue_streak,
                },
                approval_sender: None,
                question_sender: None,
            },
        }
    }
}

/// The UI's answer to an approval request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Approval {
    /// Approve the action.
    Approve,
    /// Deny the action.
    Deny,
    /// Deny this and all subsequent approvals (auto-deny rest of turn).
    DenyAll,
}

/// The user's answer to an `ask_user` question.
///
/// `Choice` carries the 0-indexed position of the clicked option; `Freeform`
/// carries the text the user typed into the always-present "Let's talk about
/// it" input. Mirrors [`Approval`] in shape (a small serializable enum the
/// oneshot carries across the pause/resume boundary).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UserAnswer {
    /// The user clicked option at the given 0-indexed position.
    Choice {
        /// 0-indexed position in the question's `options` list.
        index: usize,
    },
    /// The user typed a freeform answer ("Let's talk about it").
    Freeform {
        /// The text the user entered.
        text: String,
    },
}

/// A handle to a spawned agent.
#[derive(Debug)]
pub struct AgentHandle {
    pub id: AgentId,
    pub name: String,
    pub command_tx: mpsc::Sender<AgentCommand>,
    /// Whether the agent is currently running a turn. Updated atomically on
    /// `Started`/`Finished` events by the event forwarder, so `list_agents`
    /// returns the real state without holding the manager lock.
    pub running: std::sync::atomic::AtomicBool,
    /// Whether this agent has EVER begun a turn — set by the first
    /// `set_running(true)` and never cleared. Distinguishes a freshly spawned
    /// agent whose first turn hasn't started yet (`running == false`,
    /// `has_ever_started == false`) from one that ran and went idle
    /// (`running == false`, `has_ever_started == true`). Both look identical
    /// through `running` alone, which made `cleanup_inactive_subagents` cancel
    /// not-yet-started siblings when several `spawn_agent` calls landed in one
    /// assistant message (only the last-spawned survived).
    pub has_ever_started: std::sync::atomic::AtomicBool,
    /// The agent that spawned this one via the `spawn_agent` tool, if any.
    /// `Some` for tool-spawned background agents, `None` for the main/UI
    /// agents. Used to route a completion notification back to the parent when
    /// this agent finishes its task.
    pub parent_id: Option<AgentId>,
    /// The role the agent was spawned with (`Some("reviewer")` for a
    /// read-only reviewer sub-agent; `None` otherwise). `None` also for the
    /// main/UI agents. Used by the event forwarder to apply role-specific
    /// completion handling (e.g. the failed-reviewer protocol: a failed
    /// reviewer with no report must NOT be treated as a done reviewer).
    pub role: Option<String>,
}

impl AgentHandle {
    /// Create a handle for a freshly spawned agent. Starts in the "not
    /// running", "never started" state — the first `Started` event flips both
    /// (`has_ever_started` latches permanently).
    pub fn new(id: AgentId, name: String, command_tx: mpsc::Sender<AgentCommand>) -> Self {
        Self {
            id,
            name,
            command_tx,
            running: std::sync::atomic::AtomicBool::new(false),
            has_ever_started: std::sync::atomic::AtomicBool::new(false),
            parent_id: None,
            role: None,
        }
    }

    /// Set the parent agent id (the agent that spawned this one via the
    /// `spawn_agent` tool). Used to route a completion notification back to
    /// the parent when this agent finishes. Returns `self` for chaining.
    pub fn with_parent(mut self, parent_id: AgentId) -> Self {
        self.parent_id = Some(parent_id);
        self
    }

    /// Set the spawn role (`Some("reviewer")` for a read-only reviewer).
    /// Returns `self` for chaining.
    pub fn with_role(mut self, role: Option<String>) -> Self {
        self.role = role;
        self
    }

    /// Whether the agent is currently running a turn.
    pub fn is_running(&self) -> bool {
        self.running.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Whether the agent has ever begun a turn. `false` only for an agent
    /// spawned but not yet started — a *pending* agent, which must never be
    /// treated as a completed idle one (see [`Self::has_ever_started`]).
    pub fn has_ever_started(&self) -> bool {
        self.has_ever_started
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// Set the running flag. The first `true` also latches
    /// `has_ever_started`, which is never cleared afterwards.
    ///
    /// Store order matters (quality review Q1, 2026-09-08): `running` goes
    /// first, then the latch. On the FIRST latch transition (false → true —
    /// a freshly spawned agent), a cleanup pass running between the two
    /// stores must see either "pending" (latch false — spared) or "running"
    /// (spared) — never "started + idle" (latch true, running false), the
    /// cancel signature of a completed agent. The latch store is `Release`
    /// and [`Self::has_ever_started`] loads with `Acquire` so weakly-ordered
    /// targets (ARM64) cannot publish the latch before the `running` store
    /// it follows. On RE-starts (turn 2+, latch already true) the Acquire
    /// load may read the previous turn's latch store, leaving a
    /// nanosecond-scale "started + idle" residual — equivalent to the
    /// cleanup winning the race during the agent's genuine idle gap
    /// between turns, which is the designed semantics (idle started
    /// subagents are cancellable).
    pub fn set_running(&self, running: bool) {
        self.running
            .store(running, std::sync::atomic::Ordering::Relaxed);
        if running {
            self.has_ever_started
                .store(true, std::sync::atomic::Ordering::Release);
        }
    }
}

/// Configuration for spawning an agent.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub name: String,
    pub project_root: PathBuf,
    pub model: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_request_into_serializable_extracts_sender() {
        let (tx, _rx) = oneshot::channel::<Approval>();
        let event = AgentEvent::ApprovalRequest {
            tool_call_id: "call_1".into(),
            tool_name: "file_edit".into(),
            args: serde_json::json!({}),
            preview: None,
            core_operation: false,
            responder: tx,
        };
        let SerializedEvent {
            event: serial,
            approval_sender: sender,
            ..
        } = event.into_serializable();
        match serial {
            SerializableAgentEvent::ApprovalRequest { tool_call_id, .. } => {
                assert_eq!(tool_call_id, "call_1");
            }
            _ => panic!("expected ApprovalRequest"),
        }
        assert!(sender.is_some(), "sender should be extracted");
    }

    #[test]
    fn text_delta_into_serializable_no_sender() {
        let event = AgentEvent::TextDelta("hello".into());
        let SerializedEvent {
            event: serial,
            approval_sender: sender,
            ..
        } = event.into_serializable();
        match serial {
            SerializableAgentEvent::TextDelta { text } => assert_eq!(text, "hello"),
            _ => panic!("expected TextDelta"),
        }
        assert!(sender.is_none());
    }

    #[test]
    fn compacted_into_serializable_no_sender_and_roundtrips() {
        // A Compacted event (context compaction confirmation) carries no
        // oneshot and must round-trip as `{"kind":"compacted",...}`.
        let event = AgentEvent::Compacted {
            before: 500_000,
            after: 24_000,
        };
        let SerializedEvent {
            event: serial,
            approval_sender: sender,
            ..
        } = event.into_serializable();
        match serial {
            SerializableAgentEvent::Compacted { before, after } => {
                assert_eq!(before, 500_000);
                assert_eq!(after, 24_000);
            }
            _ => panic!("expected Compacted"),
        }
        assert!(sender.is_none(), "compacted has no oneshot");

        // Wire form: snake_case kind tag.
        let json = serde_json::to_string(&serial).unwrap();
        assert!(json.contains("\"kind\":\"compacted\""), "json: {json}");
        let back: SerializableAgentEvent = serde_json::from_str(&json).unwrap();
        match back {
            SerializableAgentEvent::Compacted { before, after } => {
                assert_eq!(before, 500_000);
                assert_eq!(after, 24_000);
            }
            _ => panic!("expected Compacted"),
        }
    }

    #[test]
    fn compact_started_into_serializable_no_sender_and_roundtrips() {
        // A CompactStarted event (compaction-begin announcement) carries no
        // oneshot and must round-trip as `{"kind":"compact_started"}`.
        let event = AgentEvent::CompactStarted;
        let SerializedEvent {
            event: serial,
            approval_sender: sender,
            ..
        } = event.into_serializable();
        assert!(matches!(serial, SerializableAgentEvent::CompactStarted));
        assert!(sender.is_none(), "compact_started has no oneshot");

        // Wire form: snake_case kind tag.
        let json = serde_json::to_string(&serial).unwrap();
        assert!(
            json.contains("\"kind\":\"compact_started\""),
            "json: {json}"
        );
        let back: SerializableAgentEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, SerializableAgentEvent::CompactStarted));
    }

    #[test]
    fn vision_describe_roundtrips_json() {
        // A VisionDescribe event (image-parsing announcement) carries no
        // oneshot and must round-trip as
        // `{"kind":"vision_describe","index":1,...}` with u32 fields.
        let event = AgentEvent::VisionDescribe {
            index: 1,
            total: 2,
            query: "Describe this image in detail.".into(),
        };
        let SerializedEvent {
            event: serial,
            approval_sender: sender,
            ..
        } = event.into_serializable();
        assert!(
            matches!(
                serial,
                SerializableAgentEvent::VisionDescribe {
                    index: 1,
                    total: 2,
                    ..
                }
            ),
            "index/total must map usize → u32"
        );
        assert!(sender.is_none(), "vision_describe has no oneshot");

        // Wire form: snake_case kind tag.
        let json = serde_json::to_string(&serial).unwrap();
        assert!(
            json.contains("\"kind\":\"vision_describe\""),
            "json: {json}"
        );
        let back: SerializableAgentEvent = serde_json::from_str(&json).unwrap();
        match back {
            SerializableAgentEvent::VisionDescribe {
                index,
                total,
                query,
            } => {
                assert_eq!(index, 1);
                assert_eq!(total, 2);
                assert_eq!(query, "Describe this image in detail.");
            }
            _ => panic!("expected VisionDescribe"),
        }
    }

    #[test]
    fn vision_described_roundtrips_json() {
        // A VisionDescribed event (image-parsing result) carries no oneshot
        // and must round-trip as `{"kind":"vision_described",...}`.
        let event = AgentEvent::VisionDescribed {
            index: 1,
            total: 1,
            success: false,
            description: "vision is down".into(),
        };
        let SerializedEvent {
            event: serial,
            approval_sender: sender,
            ..
        } = event.into_serializable();
        assert!(sender.is_none(), "vision_described has no oneshot");

        let json = serde_json::to_string(&serial).unwrap();
        assert!(
            json.contains("\"kind\":\"vision_described\""),
            "json: {json}"
        );
        let back: SerializableAgentEvent = serde_json::from_str(&json).unwrap();
        match back {
            SerializableAgentEvent::VisionDescribed {
                index,
                total,
                success,
                description,
            } => {
                assert_eq!(index, 1);
                assert_eq!(total, 1);
                assert!(!success);
                assert_eq!(description, "vision is down");
            }
            _ => panic!("expected VisionDescribed"),
        }
    }

    #[test]
    fn serializable_event_roundtrips_json() {
        let event = SerializableAgentEvent::TextDelta { text: "hi".into() };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"kind\":\"text_delta\""));
        let back: SerializableAgentEvent = serde_json::from_str(&json).unwrap();
        match back {
            SerializableAgentEvent::TextDelta { text } => assert_eq!(text, "hi"),
            _ => panic!("expected TextDelta"),
        }
    }

    #[test]
    fn parked_roundtrips_json() {
        // A Parked event must serialize as
        // `{"kind":"parked","reason":"interrupted",...}` and round-trip
        // back — the pre-stall evidence (workflow state, descendants,
        // streak) rides along so the watchdog ring and the UI banner can
        // act on it.
        let event = SerializableAgentEvent::Parked {
            reason: ParkReason::Interrupted,
            workflow_state: "Reviewing".into(),
            descendants_running: false,
            auto_continue_streak: 12,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"kind\":\"parked\""));
        assert!(json.contains("\"reason\":\"interrupted\""));
        let back: SerializableAgentEvent = serde_json::from_str(&json).unwrap();
        match back {
            SerializableAgentEvent::Parked {
                reason,
                workflow_state,
                descendants_running,
                auto_continue_streak,
            } => {
                assert_eq!(reason, ParkReason::Interrupted);
                assert_eq!(workflow_state, "Reviewing");
                assert!(!descendants_running);
                assert_eq!(auto_continue_streak, 12);
            }
            _ => panic!("expected Parked"),
        }
    }

    #[test]
    fn suggestion_injected_roundtrips_json() {
        // A SuggestionInjected event must serialize as
        // `{"kind":"suggestion_injected","text":"...","images":[...]}` and
        // round-trip back — the images (steered image attachments) ride
        // along so the transcript's steer entry can render them.
        let event = AgentEvent::SuggestionInjected {
            text: "watch the types".into(),
            images: vec!["data:image/png;base64,xxx".into()],
        };
        let SerializedEvent {
            event: serial,
            approval_sender: sender,
            ..
        } = event.into_serializable();
        assert!(sender.is_none(), "suggestion_injected has no oneshot");
        let json = serde_json::to_string(&serial).unwrap();
        assert!(json.contains("\"kind\":\"suggestion_injected\""));
        assert!(json.contains("watch the types"));
        assert!(json.contains("data:image/png;base64,xxx"));
        let back: SerializableAgentEvent = serde_json::from_str(&json).unwrap();
        match back {
            SerializableAgentEvent::SuggestionInjected { text, images } => {
                assert_eq!(text, "watch the types");
                assert_eq!(images, vec!["data:image/png;base64,xxx".to_string()]);
            }
            _ => panic!("expected SuggestionInjected"),
        }
    }

    #[test]
    fn prompt_dispatched_roundtrips_json() {
        // A PromptDispatched event must serialize as
        // `{"kind":"prompt_dispatched","text":"...","images":[...]}` and
        // round-trip back, so the frontend can show a backlog-dispatched
        // prompt as the goal at the top of the transcript.
        let event = AgentEvent::PromptDispatched {
            text: "fix the flaky test".into(),
            images: vec!["data:image/png;base64,xxx".into()],
        };
        let SerializedEvent {
            event: serial,
            approval_sender: sender,
            ..
        } = event.into_serializable();
        assert!(sender.is_none(), "prompt_dispatched has no oneshot");
        let json = serde_json::to_string(&serial).unwrap();
        assert!(json.contains("\"kind\":\"prompt_dispatched\""));
        assert!(json.contains("fix the flaky test"));
        assert!(json.contains("data:image/png;base64,xxx"));
        let back: SerializableAgentEvent = serde_json::from_str(&json).unwrap();
        match back {
            SerializableAgentEvent::PromptDispatched { text, images } => {
                assert_eq!(text, "fix the flaky test");
                assert_eq!(images, vec!["data:image/png;base64,xxx".to_string()]);
            }
            _ => panic!("expected PromptDispatched"),
        }
    }

    #[test]
    fn skill_started_roundtrips_json() {
        // A SkillStarted event must serialize as
        // `{"kind":"skill_started","name":"...","prompt":"..."}` and
        // round-trip back, so the frontend can announce a toolbar-started
        // skill in the transcript like a tool call does.
        let event = AgentEvent::SkillStarted {
            name: "merge_to_main".into(),
            prompt: "Merge the branch into main.".into(),
        };
        let SerializedEvent {
            event: serial,
            approval_sender: sender,
            ..
        } = event.into_serializable();
        assert!(sender.is_none(), "skill_started has no oneshot");
        let json = serde_json::to_string(&serial).unwrap();
        assert!(json.contains("\"kind\":\"skill_started\""));
        assert!(json.contains("\"name\":\"merge_to_main\""));
        assert!(json.contains("Merge the branch into main."));
        let back: SerializableAgentEvent = serde_json::from_str(&json).unwrap();
        match back {
            SerializableAgentEvent::SkillStarted { name, prompt } => {
                assert_eq!(name, "merge_to_main");
                assert_eq!(prompt, "Merge the branch into main.");
            }
            _ => panic!("expected SkillStarted"),
        }
    }

    #[test]
    fn model_changed_roundtrips_json() {
        // A ModelChanged event must serialize as
        // `{"kind":"model_changed","model":"..."}` and round-trip back, so the
        // frontend can update the per-agent model label immediately after a
        // provider swap without a list_agents poll.
        //
        // The `provider` field (the serving endpoint's name — the frontend
        // needs it to disambiguate a model id listed under two endpoints,
        // backlog 2980ca67): `Some(name)` serializes; `None` is SKIPPED so
        // mock-backed swaps keep the exact pre-wire JSON, and a JSON without
        // the key still deserializes to `None` (`#[serde(default)]`). The
        // `reasoning_effort` field (the DISPLAY-space effective effort of the
        // model now serving, backlog 51dab4da) follows the same shape.
        let event = AgentEvent::ModelChanged {
            model: "gpt-5".into(),
            provider: Some("openai".into()),
            reasoning_effort: Some("low".into()),
        };
        let SerializedEvent {
            event: serial,
            approval_sender: sender,
            ..
        } = event.into_serializable();
        assert!(sender.is_none(), "model_changed has no oneshot");
        let json = serde_json::to_string(&serial).unwrap();
        assert!(json.contains("\"kind\":\"model_changed\""));
        assert!(json.contains("\"model\":\"gpt-5\""));
        assert!(json.contains("\"provider\":\"openai\""));
        assert!(json.contains("\"reasoning_effort\":\"low\""));
        let back: SerializableAgentEvent = serde_json::from_str(&json).unwrap();
        match back {
            SerializableAgentEvent::ModelChanged {
                model,
                provider,
                reasoning_effort,
            } => {
                assert_eq!(model, "gpt-5");
                assert_eq!(provider.as_deref(), Some("openai"));
                assert_eq!(reasoning_effort.as_deref(), Some("low"));
            }
            _ => panic!("expected ModelChanged"),
        }

        // None provider/effort: the keys are absent from the JSON, and absent
        // deserializes back to None.
        let event = AgentEvent::ModelChanged {
            model: "mock".into(),
            provider: None,
            reasoning_effort: None,
        };
        let SerializedEvent { event: serial, .. } = event.into_serializable();
        let json = serde_json::to_string(&serial).unwrap();
        assert!(
            !json.contains("\"provider\""),
            "None provider must be skipped: {json}"
        );
        assert!(
            !json.contains("\"reasoning_effort\""),
            "None effort must be skipped: {json}"
        );
        let back: SerializableAgentEvent = serde_json::from_str(&json).unwrap();
        match back {
            SerializableAgentEvent::ModelChanged {
                model,
                provider,
                reasoning_effort,
            } => {
                assert_eq!(model, "mock");
                assert!(provider.is_none());
                assert!(reasoning_effort.is_none());
            }
            _ => panic!("expected ModelChanged"),
        }
    }

    #[test]
    fn memory_recalled_roundtrips_json() {
        // A MemoryRecalled event must serialize as
        // `{"kind":"memory_recalled","hits":[{"tier":"semantic","title":"…"},…]}`
        // and round-trip back, so the frontend can show + expand auto-recall
        // usage.
        let event = AgentEvent::MemoryRecalled {
            hits: vec![
                RecallHit {
                    tier: "semantic".into(),
                    title: "backlog decisions".into(),
                },
                RecallHit {
                    tier: "procedural".into(),
                    title: "merge workflow".into(),
                },
            ],
        };
        let SerializedEvent {
            event: serial,
            approval_sender: sender,
            ..
        } = event.into_serializable();
        assert!(sender.is_none(), "memory_recalled has no oneshot");
        let json = serde_json::to_string(&serial).unwrap();
        assert!(json.contains("\"kind\":\"memory_recalled\""));
        assert!(json.contains("\"tier\":\"semantic\""));
        assert!(json.contains("\"title\":\"backlog decisions\""));
        let back: SerializableAgentEvent = serde_json::from_str(&json).unwrap();
        match back {
            SerializableAgentEvent::MemoryRecalled { hits } => {
                assert_eq!(hits.len(), 2);
                assert_eq!(hits[0].tier, "semantic");
                assert_eq!(hits[0].title, "backlog decisions");
            }
            _ => panic!("expected MemoryRecalled"),
        }
    }

    #[test]
    fn child_finished_roundtrips_json() {
        // The completion-notification event must serialize as
        // `{"kind":"child_finished","child_id":N,"name":"...","success":bool}`
        // and round-trip back, so the frontend can mark the child "done".
        let event = AgentEvent::ChildFinished {
            child_id: 3,
            name: "rust-core-reviewer".into(),
            success: true,
        };
        let SerializedEvent {
            event: serial,
            approval_sender: sender,
            ..
        } = event.into_serializable();
        assert!(sender.is_none(), "child_finished has no oneshot");
        let json = serde_json::to_string(&serial).unwrap();
        assert!(json.contains("\"kind\":\"child_finished\""));
        assert!(json.contains("\"child_id\":3"));
        assert!(json.contains("rust-core-reviewer"));
        assert!(json.contains("\"success\":true"));
        let back: SerializableAgentEvent = serde_json::from_str(&json).unwrap();
        match back {
            SerializableAgentEvent::ChildFinished {
                child_id,
                name,
                success,
            } => {
                assert_eq!(child_id, 3);
                assert_eq!(name, "rust-core-reviewer");
                assert!(success);
            }
            other => panic!("expected ChildFinished, got {other:?}"),
        }
    }

    #[test]
    fn approval_serde() {
        let json = serde_json::to_string(&Approval::Approve).unwrap();
        assert_eq!(json, "\"approve\"");
        let back: Approval = serde_json::from_str(&json).unwrap();
        assert_eq!(back, Approval::Approve);

        // DenyAll must serialize as "deny_all" (snake_case), not "denyall".
        let json = serde_json::to_string(&Approval::DenyAll).unwrap();
        assert_eq!(json, "\"deny_all\"");
        let back: Approval = serde_json::from_str(&json).unwrap();
        assert_eq!(back, Approval::DenyAll);
    }

    #[test]
    fn user_question_into_serializable_extracts_sender() {
        // Mirrors approval_request_into_serializable_extracts_sender: the
        // oneshot sender must be pulled out so it can be held in the
        // pending-questions map, and the serializable form carries the
        // question + options (no sender).
        let (tx, _rx) = oneshot::channel::<UserAnswer>();
        let event = AgentEvent::UserQuestion {
            question_id: "q_1".into(),
            question: "Pick one".into(),
            options: vec![
                QuestionOption {
                    label: "A".into(),
                    description: Some("first".into()),
                },
                QuestionOption {
                    label: "B".into(),
                    description: None,
                },
            ],
            responder: tx,
        };
        let SerializedEvent {
            event: serial,
            approval_sender,
            question_sender,
        } = event.into_serializable();
        match serial {
            SerializableAgentEvent::UserQuestion {
                question_id,
                question,
                options,
            } => {
                assert_eq!(question_id, "q_1");
                assert_eq!(question, "Pick one");
                assert_eq!(options.len(), 2);
                assert_eq!(options[0].label, "A");
                assert_eq!(options[0].description.as_deref(), Some("first"));
                assert_eq!(options[1].label, "B");
                assert!(options[1].description.is_none());
            }
            _ => panic!("expected UserQuestion"),
        }
        assert!(
            approval_sender.is_none(),
            "no approval sender for a question"
        );
        assert!(
            question_sender.is_some(),
            "question sender should be extracted"
        );
    }

    #[test]
    fn user_question_roundtrips_json() {
        // A UserQuestion event must serialize as
        // `{"kind":"user_question","question_id":"...","question":"...","options":[...]}`.
        let event = AgentEvent::UserQuestion {
            question_id: "q_42".into(),
            question: "Favorite color?".into(),
            options: vec![
                QuestionOption {
                    label: "Blue".into(),
                    description: None,
                },
                QuestionOption {
                    label: "Green".into(),
                    description: None,
                },
                QuestionOption {
                    label: "Red".into(),
                    description: None,
                },
            ],
            responder: oneshot::channel::<UserAnswer>().0,
        };
        let SerializedEvent {
            event: serial,
            question_sender,
            ..
        } = event.into_serializable();
        assert!(question_sender.is_some(), "user_question has a sender");
        let json = serde_json::to_string(&serial).unwrap();
        assert!(json.contains("\"kind\":\"user_question\""));
        assert!(json.contains("\"question_id\":\"q_42\""));
        assert!(json.contains("Favorite color?"));
        assert!(json.contains("\"label\":\"Blue\""));
        let back: SerializableAgentEvent = serde_json::from_str(&json).unwrap();
        match back {
            SerializableAgentEvent::UserQuestion {
                question_id,
                question,
                options,
            } => {
                assert_eq!(question_id, "q_42");
                assert_eq!(question, "Favorite color?");
                assert_eq!(options.len(), 3);
                assert_eq!(options[0].label, "Blue");
            }
            _ => panic!("expected UserQuestion"),
        }
    }

    #[test]
    fn phase_event_roundtrips_json() {
        // A Phase event serializes as `{"kind":"phase","phase":"running_tools"}`
        // (snake_case both on the tag and on the PhaseKind value).
        for (phase, wire) in [
            (PhaseKind::Sending, "sending"),
            (PhaseKind::Compacting, "compacting"),
            (PhaseKind::Waiting, "waiting"),
            (PhaseKind::Reasoning, "reasoning"),
            (PhaseKind::Streaming, "streaming"),
            (PhaseKind::RunningTools, "running_tools"),
        ] {
            let SerializedEvent { event: serial, .. } =
                AgentEvent::Phase { phase }.into_serializable();
            let json = serde_json::to_string(&serial).unwrap();
            assert!(json.contains("\"kind\":\"phase\""), "{json}");
            assert!(json.contains(&format!("\"phase\":\"{wire}\"")), "{json}");
            let back: SerializableAgentEvent = serde_json::from_str(&json).unwrap();
            match back {
                SerializableAgentEvent::Phase { phase: p } => assert_eq!(p, phase),
                _ => panic!("expected Phase, got {back:?}"),
            }
        }
    }

    #[test]
    fn user_answer_serde() {
        // Choice serializes as {"kind":"choice","index":N}.
        let choice = UserAnswer::Choice { index: 1 };
        let json = serde_json::to_string(&choice).unwrap();
        assert_eq!(json, r#"{"kind":"choice","index":1}"#);
        let back: UserAnswer = serde_json::from_str(&json).unwrap();
        assert_eq!(back, UserAnswer::Choice { index: 1 });

        // Freeform serializes as {"kind":"freeform","text":"..."}.
        let free = UserAnswer::Freeform {
            text: "it's complicated".into(),
        };
        let json = serde_json::to_string(&free).unwrap();
        assert_eq!(json, r#"{"kind":"freeform","text":"it's complicated"}"#);
        let back: UserAnswer = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back,
            UserAnswer::Freeform {
                text: "it's complicated".into()
            }
        );
    }

    #[test]
    fn context_usage_serializes_quality_only_when_present() {
        // Lever 6 (backlog e4a50d22): the grade is additive on the wire. With
        // `quality: None` the JSON carries no `quality` key at all, so the
        // pre-lever shape — and the ipc fixture pinning it — stays valid.
        let bare = SerializableAgentEvent::ContextUsage {
            used: 1000,
            max: 8000,
            breakdown: ContextBreakdown::default(),
            quality: None,
        };
        let json = serde_json::to_value(&bare).unwrap();
        assert!(json.get("quality").is_none(), "{json}");

        let graded = SerializableAgentEvent::ContextUsage {
            used: 1000,
            max: 8000,
            breakdown: ContextBreakdown::default(),
            quality: Some(crate::agent::optimizer::QualityReport {
                grade: crate::agent::optimizer::QualityGrade::A,
                fill_pct: 12,
                waste_tokens: 34,
                stale_read_rate: 5,
                decision_density: 50,
            }),
        };
        let json = serde_json::to_value(&graded).unwrap();
        assert_eq!(json["quality"]["grade"], serde_json::json!("A"), "{json}");
        assert_eq!(json["quality"]["fill_pct"], serde_json::json!(12));
        assert_eq!(json["quality"]["waste_tokens"], serde_json::json!(34));
        assert_eq!(json["quality"]["stale_read_rate"], serde_json::json!(5));
        assert_eq!(json["quality"]["decision_density"], serde_json::json!(50));
        // The app deserializes what the core serializes.
        let back: SerializableAgentEvent = serde_json::from_value(json).unwrap();
        match back {
            SerializableAgentEvent::ContextUsage {
                quality: Some(report),
                ..
            } => {
                assert_eq!(report.grade, crate::agent::optimizer::QualityGrade::A);
                assert_eq!(report.decision_density, 50);
            }
            other => panic!("expected a graded ContextUsage, got {other:?}"),
        }
    }
}
