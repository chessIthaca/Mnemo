// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The public headless-API types (§5.3 :283-343) — plain serde data, no
//! Tauri, no I/O. [`Engine`](crate::headless::Engine) and
//! [`AgentSession`](crate::headless::AgentSession) land in 3a.4/3a.5; the
//! types exist first so the wire contract is pinned by the round-trip
//! fixture (tests/integration/headless.rs) before any behavior rides it.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::agent::policy::PermissionPolicy;
pub use crate::host::notifier::HostEvent;
pub use crate::runtime::channels::SessionLimits;
use crate::runtime::channels::{AgentId, Approval, SerializableAgentEvent, UserAnswer};
use crate::workspace::Workspace;

/// A session's stable identifier (a newtype over the kernel's `AgentId`,
/// so the public surface never leaks the raw counter).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(AgentId);

impl SessionId {
    /// Crate-internal construction (the engine's spawn).
    pub(crate) fn new(agent: AgentId) -> Self {
        Self(agent)
    }
}

/// The outcome of a completed turn (§5.3 :322 — `run_to_completion`):
/// the final text, plus why the turn ended (additive — the usage fields
/// land with 3b's spend accounting).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnOutcome {
    /// The accumulated assistant text of the turn.
    pub final_text: String,
    /// Why the turn ended (G7, §5.9.7): `LimitReached { which }` names
    /// the session limit that parked the session. `None` on `Exited`
    /// and pre-3a.7 payloads.
    #[serde(default)]
    pub finish_reason: Option<crate::provider::FinishReason>,
}

/// Which memory subsystem an engine runs (§5.9.8; the backend wiring
/// lands in 3b — the enum is the wire surface `EngineOptions` carries).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryBackend {
    /// In-process SQLite store; `path: None` = `<state_dir>/memory.db`
    /// (the default).
    Sqlite {
        /// The database file; `None` derives from the workspace state dir.
        path: Option<PathBuf>,
    },
    /// No memory subsystem — recall + consolidation off.
    Disabled,
}

impl Default for MemoryBackend {
    /// `Sqlite { path: None }` (§5.9.8) — the derived Default cannot
    /// express a non-unit variant.
    fn default() -> Self {
        MemoryBackend::Sqlite { path: None }
    }
}

/// Engine startup options (§5.3 :284-293).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EngineOptions {
    /// The workspace (root + state dir; §5.9.3). No project picker — an
    /// unresolvable workspace becomes `EngineError` code `no_project`.
    pub workspace: Workspace,
    /// The config dir; `None` = `MNEMO_HOME` or `~/.mnemo`.
    pub config_dir: Option<PathBuf>,
    /// The permission policy every spawned session starts under unless
    /// [`SpawnOptions::permissions`] overrides it (§5.4; the kernel
    /// default is `OverridePermissions`).
    pub permissions: PermissionPolicy,
    /// The memory backend (§5.9.8; default `Sqlite { path: None }`).
    pub memory: MemoryBackend,
    /// Whether to index the code graph at startup (default true).
    pub index_codegraph: bool,
    /// Whether the workspace REQUIRES a git repo (§5.9.3; default false —
    /// engines work in plain directories; a missing repo becomes an
    /// open-time error when true).
    pub require_git: bool,
    /// Whether to auto-install the embedder model when missing (default
    /// false — a headless caller opts in explicitly).
    pub auto_install_embedder: bool,
}

/// Session spawn options (§5.3 :303-314, the 3a subset — the
/// `guardrails` / `host_tools` / `external_policy` fields are 3c's
/// additive land).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SpawnOptions {
    /// The session's display name.
    pub name: Option<String>,
    /// A forced model — an "endpoint/model" string; `None` = routing.
    pub model: Option<String>,
    /// A worktree binding (the agent's root override).
    pub worktree: Option<PathBuf>,
    /// A per-session permission override of the engine default (§5.4).
    pub permissions: Option<PermissionPolicy>,
    /// A registered policy name (§5.9.6 tier 1 — the app passes
    /// `"mnemo.workflow"`); `None` = the free agent (`NoPolicy`).
    pub policy: Option<String>,
    /// A transcript to resume from (§5.9.5; the G5 land is 3a.6).
    pub resume: Option<Transcript>,
    /// The session run limits (G7, §5.9.7) — `max_turns` +
    /// `wall_clock_ms` enforced; hitting one ends the turn with
    /// `FinishReason::LimitReached` and parks the session.
    pub limits: Option<SessionLimits>,
}

/// A command to a live session (§5.3 :327-338) — the public mirror of the
/// kernel's `AgentCommand`, plus the approve/answer arms the approval and
/// question flows need; converted at the session boundary (3a.5). The
/// `HostToolResult` + `PolicyAnswer` arms are 3c's additive land.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionCommand {
    /// Start a new turn with a user prompt. `images` carries base64 data
    /// URLs (`data:image/png;base64,…`) for image attachments.
    Prompt {
        /// The prompt text.
        text: String,
        /// Base64 data-URL image attachments.
        images: Vec<String>,
    },
    /// Mid-work guidance — queued, injected at the next turn boundary.
    Steer {
        /// The steer text.
        text: String,
    },
    /// Cancel a queued steer whose text matches.
    CancelSteer {
        /// The steer text to drop.
        text: String,
    },
    /// Cancel the current LLM generation.
    Interrupt,
    /// Manually compact the context (processed between turns).
    Compact,
    /// Clear the conversation history entirely — a fresh start.
    Clear,
    /// Answer an approval request. Only reachable when the session runs
    /// under `ApproveViaApi` (§5.4) — under `OverridePermissions` no
    /// approval is ever requested.
    Approve {
        /// The id from the `ApprovalRequest` event.
        request_id: String,
        /// The decision.
        decision: Approval,
    },
    /// Answer an `ask_user` question.
    Answer {
        /// The id from the `UserQuestion` event.
        question_id: String,
        /// The chosen option or freeform text.
        answer: UserAnswer,
    },
}

/// One agent event, tagged with the emitting agent (§5.3 :345-348) — the
/// same serializable body the IPC bridge emits, so a headless consumer
/// and the Tauri frontend see the identical shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEvent {
    /// The emitting agent — child-agent events are tagged with the child.
    pub agent: AgentId,
    /// The event body (today's `SerializableAgentEvent` JSON shape).
    pub event: SerializableAgentEvent,
    /// The per-request derived rates (§5.7, 3b.5) — attached when
    /// `event` is a Usage: input/output tok/s + the cache-hit ratio,
    /// computed by the engine's pump (the runtime emits only the raw
    /// counts). Absent on every other event, and omitted from the wire
    /// when `None` (the pre-3b.5 JSON shape is unchanged).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rates: Option<crate::headless::stats::UsageRates>,
}

/// The receive error for [`EventReceiver::recv`] (§5.3 :341).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvError {
    /// Every sender dropped — the session ended and the buffer drained.
    Closed,
    /// The subscriber fell behind and skipped `n` events.
    Lagged(u64),
}

impl From<tokio::sync::broadcast::error::RecvError> for RecvError {
    fn from(value: tokio::sync::broadcast::error::RecvError) -> Self {
        match value {
            tokio::sync::broadcast::error::RecvError::Closed => RecvError::Closed,
            tokio::sync::broadcast::error::RecvError::Lagged(n) => RecvError::Lagged(n),
        }
    }
}

/// A multi-subscriber event receiver (§5.3 :339-343): every subscriber
/// sees every event; a slow one gets [`RecvError::Lagged`] instead of
/// stalling anyone. New subscribers come from `AgentSession::events()`
/// (each call subscribes fresh); the receiver itself is not cloneable.
/// The session wiring (3a.5) constructs these from the per-session
/// broadcast channel.
#[derive(Debug)]
pub struct EventReceiver<T> {
    pub(crate) rx: tokio::sync::broadcast::Receiver<T>,
}

impl<T: Clone> EventReceiver<T> {
    /// Receive the next event. `Err(RecvError::Closed)` once the session
    /// ended and the buffer drained; `Err(RecvError::Lagged(n))` after
    /// falling `n` events behind.
    pub async fn recv(&mut self) -> Result<T, RecvError> {
        self.rx.recv().await.map_err(Into::into)
    }

    /// Non-blocking receive: `Ok(Some(event))` when one is buffered,
    /// `Ok(None)` when the stream is momentarily empty (the sender may
    /// still be live), `Err(RecvError::Closed)` once the bus closed and
    /// drained, `Err(RecvError::Lagged(n))` after skipping `n`.
    pub fn try_recv(&mut self) -> Result<Option<T>, RecvError> {
        match self.rx.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => Ok(None),
            Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                Err(RecvError::Closed)
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(n)) => {
                Err(RecvError::Lagged(n))
            }
        }
    }

    // The broadcast-wiring constructor lands with the 3a.5 session code
    // that calls it (ahead-of-use fns are dead code under
    // #![deny(warnings)]).
}

/// A serialized conversation transcript (§5.9.5, G5): the full message
/// history — roles, tool calls/results, reasoning, and the compaction
/// summaries all live IN the messages, so a resumed session reproduces
/// the original request byte prefix (§5.9.5 :693-695: same transcript +
/// same spec + same model ⇒ identical request body).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Transcript {
    /// The conversation messages, oldest first.
    pub messages: Vec<crate::provider::Message>,
}

/// A session's snapshot (§5.9.5): the transcript + the model + the
/// context usage + the pending interaction. The turn stats land with
/// 3b's spend accounting.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionSnapshot {
    /// The conversation at snapshot time (the pre-request view when
    /// taken mid-turn).
    pub transcript: Transcript,
    /// The session's model (None until the loop exposes one).
    pub model: Option<String>,
    /// The approximate prompt-token usage.
    pub used_tokens: u32,
    /// The context window.
    pub max_tokens: u32,
    /// The pending approval/question awaiting a SessionCommand answer.
    pub pending: Option<PendingInteraction>,
}

/// The pending interaction a snapshot reports (§5.9.5): an approval or
/// a question the session is blocked on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PendingInteraction {
    /// An approval awaiting SessionCommand::Approve.
    Approval {
        /// The id from the ApprovalRequest event.
        request_id: String,
    },
    /// A question awaiting SessionCommand::Answer.
    Question {
        /// The id from the UserQuestion event.
        question_id: String,
    },
}
