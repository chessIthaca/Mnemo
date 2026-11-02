// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The headless session (§5.3 :315-326) — one agent's command/event
//! surface: `send` (the public `SessionCommand` → the kernel
//! `AgentCommand`), the multi-subscriber event bus, `run_to_completion`,
//! `close`. The engine's event pump (fan-in → broadcast + the pending
//! approval/question maps) lives here — the session's moving parts in
//! one place.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tokio::sync::{broadcast, mpsc, Mutex, oneshot};

use crate::headless::error::EngineError;
use crate::headless::stats::{SessionStats, UsageRates};
use crate::headless::types::{
    AgentPlan, EventReceiver, PendingInteraction, SessionCommand, SessionEvent, SessionId,
    SessionSnapshot, Transcript, TurnOutcome,
};
use crate::runtime::channels::ConversationExport;
use crate::host::runtime::HostRuntime;
use crate::runtime::channels::{
    AgentEvent, AgentId, Approval, HostToolOutcome, PolicyAnswer, SerializableAgentEvent,
    SteerPayload, UserAnswer,
};
use crate::runtime::AgentCommand;

/// Pending responders by interaction id, each tagged with the agent whose
/// event raised it — the engine is shared, so a session's snapshot reports
/// only its own (or its spawned descendants') interactions.
pub(crate) type PendingMap<T> = Mutex<HashMap<String, (AgentId, oneshot::Sender<T>)>>;

/// Shared engine state the sessions + the pump hold: the host core, the
/// live-session registry (the closed check), and the pending responder
/// maps (the pump stashes, `Approve`/`Answer` resolve).
pub(crate) struct EngineInner {
    /// The shared host core (3a.2).
    pub(crate) host: HostRuntime,
    /// Live sessions (SessionId → the agent id).
    pub(crate) sessions: Mutex<HashMap<SessionId, AgentId>>,
    /// Approval responders by request id.
    pub(crate) pending_approvals: PendingMap<Approval>,
    /// Question responders by question id.
    pub(crate) pending_questions: PendingMap<UserAnswer>,
    /// Host-tool outcome responders by call id (3c/G2) — the pump stashes
    /// them; `SessionCommand::HostToolResult` resolves.
    pub(crate) pending_host_tools: PendingMap<HostToolOutcome>,
    /// Policy-hook answer responders by hook id (3c/G6 tier 3) — the pump
    /// stashes them; `SessionCommand::PolicyAnswer` resolves.
    pub(crate) pending_hooks: PendingMap<PolicyAnswer>,
    /// Engine-registered Rust custom tools (3c/G2, §5.9.2) — the Host
    /// group, joined into every subsequent spawn's registry (in-process
    /// execution like any kernel tool — NOT a spec-based host tool; no
    /// round-trip). std Mutex: the register is sync, the lock is never
    /// held across an await.
    pub(crate) host_tools: std::sync::Mutex<Vec<std::sync::Arc<dyn crate::tool::Tool>>>,
    /// Named policy factories (3c/G6 tier 1, D4) —
    /// [`Engine::register_policy`](crate::headless::engine::Engine::register_policy)
    /// inserts; `spawn` resolves `SpawnOptions::policy` against it
    /// (unknown → `spawn_failed`). std Mutex: the register is sync,
    /// the lock is never held across an await (the `host_tools`
    /// pattern).
    pub(crate) policy_registry: std::sync::Mutex<
        HashMap<String, std::sync::Arc<dyn crate::agent::policy::AgentPolicyFactory>>,
    >,
    /// Parked sessions (G7, §5.9.7): agents whose Finished{LimitReached}
    /// the pump saw — every later Prompt errors `session_parked`.
    /// Engine-scoped, not per-handle (review round-1 F3): the pump is
    /// the single writer, so the raw send()/events() surface and any
    /// re-acquired handle see the same state.
    pub(crate) parked: Mutex<HashSet<AgentId>>,
    /// The live session tallies (§5.7, 3b.5): the pump folds each
    /// agent's events into its entry as they flow —
    /// [`AgentSession::stats`](AgentSession::stats) reads the running
    /// totals (the default shape until something flows). Engine-scoped
    /// like `parked` (the pump is the single writer); `std::sync` —
    /// the fold is sync, so `stats()` never awaits.
    pub(crate) tallies: std::sync::Mutex<HashMap<AgentId, SessionStats>>,
    /// Whether worktree-less sessions share the project's main plans dir
    /// (`<state_dir>/plans`) instead of a private `agents/<id>` one. Off by
    /// default; a host opts in with `Engine::use_project_plans`. Worktree-bound
    /// sessions always keep their own plans dir inside their checkout.
    pub(crate) shared_plans: std::sync::atomic::AtomicBool,
}

/// §5.3 :350-354 — the constant answer a zero-subscriber question gets
/// (the turn never blocks on a user who is not listening).
pub(crate) const NO_SUBSCRIBER_ANSWER: &str = "user unavailable — decide yourself";

/// The engine's event pump: drains the host fan-in, runs the lifecycle
/// bookkeeping (the same per-event maintenance the console REPL does),
/// stashes approval/question responders, answers zero-subscriber
/// questions with the constant, and broadcasts every event on the
/// session bus.
pub(crate) async fn pump_task(
    mut fanin: mpsc::Receiver<(AgentId, AgentEvent)>,
    bus: broadcast::Sender<SessionEvent>,
    inner: Arc<EngineInner>,
) {
    while let Some((agent, event)) = fanin.recv().await {
        // The lifecycle bookkeeping — exactly where the console's run
        // loop + the GUI forwarder do it (running flags, parent
        // notifications, dead-agent removal).
        match &event {
            AgentEvent::Started => inner.host.bookkeeping.on_started(agent).await,
            AgentEvent::Finished { reason } => {
                // G7 (§5.9.7): a limit hit parks the session — the pump
                // observes EVERY Finished, so parking is engine-scoped
                // (the raw send surface + re-acquired handles see it;
                // review round-1 F3).
                if matches!(
                    reason,
                    crate::provider::FinishReason::LimitReached { .. }
                ) {
                    inner.parked.lock().await.insert(agent);
                }
                inner.host.bookkeeping.on_turn_ended(agent, true).await
            }
            // Review round-1 F4: the console's final-error arm — a
            // non-retrying Error ends the turn's work with success=false
            // (a later bookkeeping Finished, if any, mirrors the
            // console's own second call).
            AgentEvent::Error {
                retrying: false, ..
            } => inner.host.bookkeeping.on_turn_ended(agent, false).await,
            AgentEvent::Exited => inner.host.bookkeeping.on_exited(agent).await,
            _ => {}
        }
        // 3b.5 (§5.7): the live tally + the per-request derived rates —
        // computed HERE (the headless layer), not the runtime. The
        // rates wrap a Usage; every event folds into the agent's
        // running totals, so stats() always equals the sum of what
        // the bus carried.
        let rates = if let AgentEvent::Usage {
            prompt_tokens,
            completion_tokens,
            cached_tokens,
            ttft_ms,
            generation_ms,
            ..
        } = &event
        {
            Some(UsageRates::from_usage(
                *prompt_tokens,
                *completion_tokens,
                *cached_tokens,
                *ttft_ms,
                *generation_ms,
            ))
        } else {
            None
        };
        inner
            .tallies
            .lock()
            .expect("tallies lock poisoned")
            .entry(agent)
            .or_default()
            .tally(&event);
        let forwarded = event.into_serializable();
        // Stash the approval responder for SessionCommand::Approve.
        if let Some(responder) = forwarded.approval_sender {
            if let SerializableAgentEvent::ApprovalRequest { tool_call_id, .. } = &forwarded.event
            {
                inner
                    .pending_approvals
                    .lock()
                    .await
                    .insert(tool_call_id.clone(), (agent, responder));
            }
        }
        // The question responder — zero subscribers means the constant
        // answer (§5.3 :350-354); otherwise it waits for `Answer`.
        if let Some(responder) = forwarded.question_sender {
            if bus.receiver_count() == 0 {
                let _ = responder.send(UserAnswer::Freeform {
                    text: NO_SUBSCRIBER_ANSWER.to_string(),
                });
            } else if let SerializableAgentEvent::UserQuestion { question_id, .. } =
                &forwarded.event
            {
                inner
                    .pending_questions
                    .lock()
                    .await
                    .insert(question_id.clone(), (agent, responder));
            }
        }
        // Stash the host-tool responder for SessionCommand::HostToolResult
        // (3c/G2). No zero-subscriber shortcut here: an unanswered call
        // resolves by the dispatch-side timeout, so a silent host never
        // hangs the agent.
        if let Some(responder) = forwarded.host_tool_sender {
            if let SerializableAgentEvent::HostToolCall { call_id, .. } = &forwarded.event {
                inner
                    .pending_host_tools
                    .lock()
                    .await
                    .insert(call_id.clone(), (agent, responder));
            }
        }
        // Stash the policy-hook responder for SessionCommand::PolicyAnswer
        // (3c/G6 tier 3). Same as host tools: the hook's dispatch/turn-site
        // timeout resolves an absent answer per the spec's on_timeout.
        if let Some(responder) = forwarded.policy_hook_sender {
            if let SerializableAgentEvent::PolicyHook { hook_id, .. } = &forwarded.event {
                inner
                    .pending_hooks
                    .lock()
                    .await
                    .insert(hook_id.clone(), (agent, responder));
            }
        }
        // Best-effort broadcast: zero subscribers → the event drops.
        let _ = bus.send(SessionEvent {
            agent,
            event: forwarded.event,
            rates,
        });
    }
}

/// A live session (§5.3 :315-326): one agent's id + the send/events/
/// completion surface. One handle per spawn; `events()` clones the
/// subscription, not the session.
pub struct AgentSession {
    pub(crate) id: SessionId,
    pub(crate) agent: AgentId,
    /// Actual loop ownership, independent of manager/registry bookkeeping.
    pub(crate) agent_loop: std::sync::Weak<crate::agent::AgentLoop>,
    pub(crate) inner: Arc<EngineInner>,
    pub(crate) bus: broadcast::Sender<SessionEvent>,
}

impl AgentSession {
    /// The session's stable id.
    pub fn id(&self) -> SessionId {
        self.id
    }

    /// A NEW event subscription (§5.3 :318-321): every subscriber sees
    /// every event; a slow one lags, never stalling anyone (§7 row 12).
    pub fn events(&self) -> EventReceiver<SessionEvent> {
        EventReceiver {
            rx: self.bus.subscribe(),
        }
    }

    /// Send a command (§5.3 :327-338). On a closed session every command
    /// fails with `EngineError` code `session_closed` (§7 row 15).
    pub async fn send(&self, command: SessionCommand) -> Result<(), EngineError> {
        if !self.inner.sessions.lock().await.contains_key(&self.id) {
            return Err(EngineError::session_closed(format!(
                "session {:?} is closed",
                self.id
            )));
        }
        // G7 (§5.9.7): a parked session (a limit was hit — the pump set
        // it engine-scoped on Finished{LimitReached}) takes no more
        // prompts; the check rides ANY handle (review round-1 F3).
        if self.inner.parked.lock().await.contains(&self.agent)
            && matches!(command, SessionCommand::Prompt { .. })
        {
            return Err(EngineError::session_parked(
                "a session limit was hit — the session is parked",
            ));
        }
        match command {
            SessionCommand::Prompt { text, images } => {
                self.agent_command(AgentCommand::Prompt { text, images })
                    .await
            }
            SessionCommand::Steer { text, images } => {
                self.agent_command(AgentCommand::Suggestion(SteerPayload { text, images }))
                    .await
            }
            SessionCommand::CancelSteer { text } => {
                self.agent_command(AgentCommand::CancelSuggestion(text))
                    .await
            }
            SessionCommand::Interrupt => self.agent_command(AgentCommand::Interrupt).await,
            SessionCommand::Compact => self.agent_command(AgentCommand::Compact).await,
            SessionCommand::Clear => self.agent_command(AgentCommand::Clear).await,
            SessionCommand::Approve {
                request_id,
                decision,
            } => {
                let responder = self
                    .inner
                    .pending_approvals
                    .lock()
                    .await
                    .remove(&request_id)
                    .map(|(_, responder)| responder);
                match responder {
                    Some(responder) => responder.send(decision).map_err(|_| {
                        EngineError::unknown_request("the approval already resolved")
                    }),
                    None => Err(EngineError::unknown_request(format!(
                        "no pending approval '{request_id}'"
                    ))),
                }
            }
            SessionCommand::Answer {
                question_id,
                answer,
            } => {
                let responder = self
                    .inner
                    .pending_questions
                    .lock()
                    .await
                    .remove(&question_id)
                    .map(|(_, responder)| responder);
                match responder {
                    Some(responder) => responder.send(answer).map_err(|_| {
                        EngineError::unknown_request("the question already resolved")
                    }),
                    None => Err(EngineError::unknown_request(format!(
                        "no pending question '{question_id}'"
                    ))),
                }
            }
            SessionCommand::HostToolResult {
                call_id,
                result,
                error,
            } => {
                // The `result | error` pair (§5.9.2): exactly one must ride
                // the answer — neither is a protocol error, not a silent
                // success.
                let outcome = match (result, error) {
                    (Some(result), _) => Ok(result),
                    (None, Some(error)) => Err(error),
                    (None, None) => {
                        return Err(EngineError::unknown_request(format!(
                            "host tool result '{call_id}' carries neither result nor error"
                        )))
                    }
                };
                let responder = self
                    .inner
                    .pending_host_tools
                    .lock()
                    .await
                    .remove(&call_id)
                    .map(|(_, responder)| responder);
                match responder {
                    Some(responder) => responder.send(outcome).map_err(|_| {
                        EngineError::unknown_request("the host-tool call already resolved")
                    }),
                    None => Err(EngineError::unknown_request(format!(
                        "no pending host-tool call '{call_id}'"
                    ))),
                }
            }
            SessionCommand::PolicyAnswer { hook_id, answer } => {
                let responder = self
                    .inner
                    .pending_hooks
                    .lock()
                    .await
                    .remove(&hook_id)
                    .map(|(_, responder)| responder);
                match responder {
                    Some(responder) => responder.send(answer).map_err(|_| {
                        EngineError::unknown_request("the policy hook already resolved")
                    }),
                    None => Err(EngineError::unknown_request(format!(
                        "no pending policy hook '{hook_id}'"
                    ))),
                }
            }
        }
    }

    /// The conversation transcript (§5.3 :323 / §5.9.5, G5) — the full
    /// message history, layout-preserving (roles, tool calls/results,
    /// reasoning, and the compaction summaries all live in the
    /// messages).
    pub async fn history(&self) -> Result<Transcript, EngineError> {
        let export = self.export_conversation().await?;
        Ok(Transcript {
            messages: export.messages,
        })
    }

    /// Read the session's registered tool names for diagnostics.
    pub async fn tool_names(&self) -> Result<Vec<String>, EngineError> {
        let agent = self
            .inner
            .host
            .agent_loops
            .lock()
            .await
            .get(&self.agent)
            .cloned()
            .ok_or_else(|| EngineError::session_closed("the agent task has ended"))?;
        Ok(agent
            .tools()
            .iter()
            .map(|tool| tool.name().to_string())
            .collect())
    }

    /// Read this session's active plan from its exact plans directory.
    pub async fn plan(&self) -> Result<Option<AgentPlan>, EngineError> {
        let plans_dir = self
            .inner
            .host
            .agent_loops
            .lock()
            .await
            .get(&self.agent)
            .map(|agent| agent.plans_dir().to_path_buf())
            .ok_or_else(|| EngineError::session_closed("the agent task has ended"))?;
        if plans_dir.as_os_str().is_empty() {
            return Ok(None);
        }
        tokio::task::spawn_blocking(move || read_agent_plan(plans_dir))
            .await
            .map_err(|err| EngineError::plan_failed(format!("plan read task failed: {err}")))?
    }

    /// A snapshot (§5.3 :324 / §5.9.5): the transcript + the model + the
    /// context usage + the pending interaction (if any). The turn
    /// stats land with 3b's spend accounting.
    pub async fn snapshot(&self) -> Result<SessionSnapshot, EngineError> {
        // G5: the transcript part is best-effort — mid-block (a pending
        // approval) the live export times out and the snapshot degrades
        // to an empty transcript rather than hanging; `pending` is
        // engine-local and ALWAYS accurate.
        let export = self.export_conversation().await.ok();
        let (transcript, model, used_tokens, max_tokens) = match export {
            Some(e) => (
                Transcript { messages: e.messages },
                e.model,
                e.used_tokens,
                e.max_tokens,
            ),
            None => (Transcript::default(), None, 0, 0),
        };
        // The four pending maps in a fixed order (approval → question →
        // host tool → policy hook): only one interaction kind is normally
        // live at a time, and the snapshot reports the first it finds.
        // The maps are engine-wide, so only this session's own (or its
        // spawned descendants') entries count — a sibling session on a
        // shared engine never shows up as this session's pending.
        let pending = if let Some(id) = self.first_owned(&self.inner.pending_approvals).await {
            Some(PendingInteraction::Approval { request_id: id })
        } else if let Some(id) = self.first_owned(&self.inner.pending_questions).await {
            Some(PendingInteraction::Question { question_id: id })
        } else if let Some(id) = self.first_owned(&self.inner.pending_host_tools).await {
            Some(PendingInteraction::HostToolCall { call_id: id })
        } else {
            self.first_owned(&self.inner.pending_hooks)
                .await
                .map(|id| PendingInteraction::PolicyHook { hook_id: id })
        };
        Ok(SessionSnapshot {
            transcript,
            model,
            used_tokens,
            max_tokens,
            pending,
        })
    }

    /// The id of the first entry in `map` raised by this session's agent
    /// or one of its spawned descendants (whose interactions only this
    /// session's host can answer — children get no session handle).
    async fn first_owned<T>(&self, map: &PendingMap<T>) -> Option<String> {
        let candidates: Vec<(String, AgentId)> = map
            .lock()
            .await
            .iter()
            .map(|(id, (agent, _))| (id.clone(), *agent))
            .collect();
        for (id, agent) in candidates {
            if self.owns_agent(agent).await {
                return Some(id);
            }
        }
        None
    }

    /// Whether `agent` is this session's agent or a descendant of it (a
    /// walk up the manager's parent chain, bounded against a malformed
    /// cycle).
    async fn owns_agent(&self, agent: AgentId) -> bool {
        if agent == self.agent {
            return true;
        }
        let manager = self.inner.host.manager.lock().await;
        let mut current = agent;
        for _ in 0..64 {
            match manager.parent_id(current) {
                Some(parent) if parent == self.agent => return true,
                Some(parent) => current = parent,
                None => return false,
            }
        }
        false
    }

    /// §5.7's live session stats — the running totals the pump folded
    /// from this agent's events (Usage / Savings / ModelChanged /
    /// ContextUsage); a session that has emitted nothing reports the
    /// default shape. Always equals the sum of what `events()`
    /// delivered — the pump folds before it broadcasts.
    pub fn stats(&self) -> SessionStats {
        self.inner
            .tallies
            .lock()
            .expect("tallies lock poisoned")
            .get(&self.agent)
            .cloned()
            .unwrap_or_default()
    }

    /// §5.7's durable half — the store's `session_stats` rows for the
    /// loop's CURRENT session id (the same read the IPC's
    /// `get_session_stats` performs). Errors `session_closed` when
    /// the agent task has ended or no prompt has been served yet (no
    /// session id), and `memory_disabled` on a
    /// `MemoryBackend::Disabled` engine.
    pub async fn stored_stats(&self) -> Result<crate::memory::types::SessionStats, EngineError> {
        let session_id = {
            let loops = self.inner.host.agent_loops.lock().await;
            let agent_loop = loops
                .get(&self.agent)
                .ok_or_else(|| EngineError::session_closed("the agent task has ended"))?;
            agent_loop
                .session_id()
                .ok_or_else(|| EngineError::session_closed("no prompt served yet — no session id"))?
        };
        let store = self
            .inner
            .host
            .factory
            .memory_handle()
            .ok_or_else(EngineError::memory_disabled)?;
        store
            .session_stats(&session_id)
            .await
            .map_err(|e| EngineError::stats_failed(format!("session stats read failed: {e}")))
    }

    /// The export plumbing: send ExportHistory through the command
    /// channel and await the reply (the task answers between turns from
    /// its live state; mid-request from the pre-request snapshot).
    async fn export_conversation(&self) -> Result<ConversationExport, EngineError> {
        if !self.inner.sessions.lock().await.contains_key(&self.id) {
            return Err(EngineError::session_closed(format!(
                "session {:?} is closed",
                self.id
            )));
        }
        let (reply_tx, mut reply_rx) = tokio::sync::mpsc::channel(1);
        let mgr = self.inner.host.manager.lock().await;
        mgr.send(
            self.agent,
            AgentCommand::ExportHistory { reply: reply_tx },
        )
        .map_err(|_| EngineError::session_closed("the agent task ended"))?;
        drop(mgr);
        let export = match tokio::time::timeout(
            std::time::Duration::from_secs(2),
            reply_rx.recv(),
        )
        .await
        {
            Ok(Some(export)) => export,
            Ok(None) => {
                return Err(EngineError::session_closed("the agent task ended"))
            }
            // G5: mid-block (a pending approval/question) the turn holds
            // the conversation and does not drain the command channel —
            // the export is genuinely unavailable, never a hang.
            Err(_) => {
                return Err(EngineError::session_busy(
                    "the history is mid-flight — the turn holds it while a tool blocks; \
                     retry between turns",
                ))
            }
        };
        Ok(export)
    }

    /// Send a prompt and pump the session's own event stream until the
    /// turn finishes (§5.3 :322 — §7 row 10: the final text; the stream
    /// shows Started … Finished in order).
    pub async fn run_to_completion(&self, prompt: &str) -> Result<TurnOutcome, EngineError> {
        // Subscribe BEFORE the send (review round-1 F2): a Finished
        // landing between the send and a later subscribe would never be
        // seen and the loop below would block forever — the
        // wall-clock-immediate shape (Finished at the first request
        // head, zero provider work) makes that window real.
        let mut rx = self.events();
        self.send(SessionCommand::Prompt {
            text: prompt.to_string(),
            images: vec![],
        })
        .await?;
        let mut final_text = String::new();
        // 3c/G6 tier 3 (row 32, §5.9.6): a turn end answered `Continue`
        // runs ANOTHER turn — the agent only parks when it has nothing
        // more to run, and the logical turn this prompt started spans
        // those model turns. Keep pumping (accumulating the deltas; the
        // last Finished's reason wins) until the agent PARKS or the
        // task exits — returning at the first per-turn `Finished` would
        // hand back turn one's text while a continued turn two is still
        // streaming, a stale "completion".
        let mut finish_reason = None;
        loop {
            match rx.recv().await {
                Ok(SessionEvent { agent, event, .. }) => {
                    if agent != self.agent {
                        // Other agents' events ride the same bus, tagged —
                        // they never end this session's turn.
                        continue;
                    }
                    match event {
                        SerializableAgentEvent::Started => {
                            // Each model turn starts fresh — under a
                            // tier-3 Continue the earlier turns' text is
                            // conversation history, not this turn's
                            // "final text" (row 32; §5.3 row 10).
                            final_text.clear();
                        }
                        SerializableAgentEvent::TextDelta { text } => {
                            final_text.push_str(&text)
                        }
                        SerializableAgentEvent::Finished { reason } => {
                            // (Parking is engine-scoped — the pump set it.)
                            // One MODEL turn's end — under a tier-3
                            // Continue the session runs on; record the
                            // reason and keep pumping for the park.
                            finish_reason = Some(reason);
                        }
                        SerializableAgentEvent::Parked { .. } => {
                            return Ok(TurnOutcome {
                                final_text,
                                finish_reason,
                            })
                        }
                        SerializableAgentEvent::Exited => {
                            return Ok(TurnOutcome {
                                final_text,
                                finish_reason,
                            })
                        }
                        _ => {}
                    }
                }
                Err(_) => {
                    return Err(EngineError::session_closed(
                        "the session ended before the turn finished",
                    ))
                }
            }
        }
    }

    /// Close the session (§5.3 :325-326): cancel the agent + deregister —
    /// later sends fail with `session_closed`.
    pub async fn close(&self) -> Result<(), EngineError> {
        let _ = self.agent_command(AgentCommand::Cancel).await;
        self.inner.sessions.lock().await.remove(&self.id);
        Ok(())
    }

    /// Close the session and wait until the agent and all of its descendants
    /// have released their loops. Worktree-bound callers use this before
    /// deleting the checkout: descendants inherit the same code graph and its
    /// open SQLite connection.
    pub async fn close_and_wait(&self) -> Result<(), EngineError> {
        self.inner.sessions.lock().await.remove(&self.id);
        let mut family_loops = vec![(self.agent, self.agent_loop.clone())];
        let mut cancelled = std::collections::HashSet::new();
        let released = async {
            loop {
                let mut cancel_failed = None;
                {
                    let manager = self.inner.host.manager.lock().await;
                    let loops = self.inner.host.agent_loops.lock().await;
                    let family: Vec<_> = loops
                        .iter()
                        .filter(|(id, loop_ref)| {
                            **id == self.agent || loop_ref.family_root() == Some(self.agent)
                        })
                        .map(|(id, loop_ref)| (*id, loop_ref.clone()))
                        .collect();
                    for (id, loop_ref) in family {
                        let weak = std::sync::Arc::downgrade(&loop_ref);
                        if !family_loops
                            .iter()
                            .any(|(known, existing)| known == &id && existing.ptr_eq(&weak))
                        {
                            family_loops.push((id, weak));
                        }
                        if manager.get(id).is_some() && cancelled.insert(id) {
                            match manager.send(id, AgentCommand::Cancel) {
                                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                                    cancel_failed = Some(id);
                                }
                                // A stale manager entry whose receiver has
                                // closed is already exiting. Keep polling the
                                // loop weak rather than reporting a failure.
                                Ok(()) | Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {}
                            }
                        }
                    }
                }
                if let Some(id) = cancel_failed {
                    return Err(EngineError::session_closed(format!(
                        "could not queue cancellation for agent {id}; its loop is still live"
                    )));
                }
                if family_loops
                    .iter()
                    .all(|(_, weak)| weak.upgrade().is_none())
                {
                    // Once the parent is dead no new child can be spawned. A
                    // final lineage scan catches a child registered just as
                    // the parent exited, even if an intermediate is gone.
                    let loops = self.inner.host.agent_loops.lock().await;
                    if !loops.iter().any(|(id, loop_ref)| {
                        *id == self.agent || loop_ref.family_root() == Some(self.agent)
                    }) {
                        return Ok(());
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(10), released)
            .await
            .map_err(|_| {
                EngineError::session_closed(
                    "timed out waiting for the agent family to release its loops",
                )
            })??;
        Ok(())
    }

    async fn agent_command(&self, command: AgentCommand) -> Result<(), EngineError> {
        let mgr = self.inner.host.manager.lock().await;
        mgr.send(self.agent, command)
            .map_err(|_| EngineError::session_closed("the agent task ended"))
    }
}

#[cfg(test)]
mod plan_tests {
    use super::*;

    fn write_plan(plans_dir: &std::path::Path, id: &str, title: &str) {
        std::fs::create_dir_all(plans_dir).expect("plans directory");
        std::fs::write(
            plans_dir.join("stack.json"),
            format!(r#"{{"stack":["{id}"],"reviewed":false,"review_required":false}}"#),
        )
        .expect("stack");
        std::fs::write(
            plans_dir.join(format!("{id}.md")),
            format!("# Plan: {title}\n\n## Goal\nA real goal\n\n## Steps\n- [x] 1. **Read** — src/a.rs\n- [ ] 2. Implement src/b.rs\n"),
        )
        .expect("plan");
    }

    #[test]
    fn reads_the_plan_from_the_exact_session_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lead = dir.path().join("agents/1");
        let other = dir.path().join("agents/2");
        write_plan(&lead, "lead1234", "Lead plan");
        write_plan(&other, "other123", "Other plan");

        let plan = read_agent_plan(lead).expect("read plan").expect("active plan");
        assert_eq!(plan.id, "lead1234");
        assert_eq!(plan.title, "Lead plan");
        assert_eq!(plan.goal, "A real goal");
        assert_eq!(plan.steps.len(), 2);
        assert!(plan.steps[0].done);
        assert!(!plan.steps[1].done);
    }

    #[test]
    fn absent_stack_is_empty_but_a_missing_or_invalid_active_plan_is_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(read_agent_plan(dir.path().to_path_buf())
            .expect("missing stack")
            .is_none());
        write_plan(dir.path(), "valid123", "Valid");
        std::fs::remove_file(dir.path().join("valid123.md")).expect("remove active plan");
        assert!(read_agent_plan(dir.path().to_path_buf()).is_err());

        std::fs::write(
            dir.path().join("stack.json"),
            r#"{"stack":["../outside"],"reviewed":false,"review_required":false}"#,
        )
        .expect("unsafe stack");
        assert!(read_agent_plan(dir.path().to_path_buf()).is_err());
    }

    #[test]
    fn malformed_stack_does_not_become_an_empty_plan() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("stack.json"), "not json").expect("stack");
        assert!(read_agent_plan(dir.path().to_path_buf()).is_err());
    }
}

#[derive(serde::Deserialize)]
struct AgentPlanStack {
    #[serde(default)]
    stack: Vec<String>,
}

fn read_agent_plan(plans_dir: std::path::PathBuf) -> Result<Option<AgentPlan>, EngineError> {
    let stack_path = plans_dir.join("stack.json");
    let bytes = match std::fs::read(&stack_path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(EngineError::plan_failed(format!(
                "could not read {}: {err}",
                stack_path.display()
            )))
        }
    };
    let stack: AgentPlanStack = serde_json::from_slice(&bytes).map_err(|err| {
        EngineError::plan_failed(format!("could not decode {}: {err}", stack_path.display()))
    })?;
    let Some(id) = stack.stack.last() else {
        return Ok(None);
    };
    if id.is_empty()
        || id.len() > 128
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(EngineError::plan_failed(format!(
            "invalid active plan id '{id}'"
        )));
    }
    let path = plans_dir.join(format!("{id}.md"));
    let source = std::fs::read_to_string(&path).map_err(|err| {
        EngineError::plan_failed(format!("could not read active plan {}: {err}", path.display()))
    })?;
    let plan = crate::memory::plan_format::PlanFile::parse(&source).map_err(|err| {
        EngineError::plan_failed(format!("could not parse active plan {}: {err}", path.display()))
    })?;
    Ok(Some(AgentPlan {
        id: id.clone(),
        title: plan.title,
        goal: plan.goal,
        steps: plan.steps,
    }))
}

#[cfg(test)]
mod lifecycle_tests {
    use super::super::engine::Engine;
    use crate::headless::{SessionCommand, SpawnOptions};

    #[tokio::test]
    async fn close_waits_for_the_loop_after_registry_exit() {
        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        std::fs::write(
            config.path().join("config.toml"),
            "[general]\ndefault_provider = \"test\"\ndefault_model = \"local\"\n",
        )
        .expect("config");
        std::fs::write(
            config.path().join("endpoints.toml"),
            "[[endpoint]]\nname = \"test\"\nkind = \"local\"\nbase_url = \"http://127.0.0.1:9/v1/\"\nmodels = [\"local\"]\n",
        )
        .expect("endpoints");
        let engine = Engine::start(crate::headless::EngineOptions {
            workspace: crate::workspace::Workspace::new(root.path()),
            config_dir: Some(config.path().to_path_buf()),
            permissions: crate::agent::policy::PermissionPolicy::OverridePermissions,
            memory: crate::headless::MemoryBackend::Disabled,
            index_codegraph: false,
            require_git: false,
            auto_install_embedder: false,
        })
        .await
        .expect("start engine");
        let session = engine.spawn(SpawnOptions::default()).await.expect("spawn");
        let held_loop = session.agent_loop.upgrade().expect("live loop");
        let weak_loop = session.agent_loop.clone();
        let inner = session.inner.clone();
        let agent = session.agent;
        let mut closing = tokio::spawn(async move { session.close_and_wait().await });

        for _ in 0..100 {
            let manager_gone = inner.host.manager.lock().await.get(agent).is_none();
            let registry_gone = !inner.host.agent_loops.lock().await.contains_key(&agent);
            if manager_gone && registry_gone {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(inner.host.manager.lock().await.get(agent).is_none());
        assert!(!inner.host.agent_loops.lock().await.contains_key(&agent));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut closing)
                .await
                .is_err(),
            "registry removal returned while the AgentLoop was still alive"
        );
        assert!(weak_loop.upgrade().is_some());

        drop(held_loop);
        closing
            .await
            .expect("join close")
            .expect("close after the final loop reference is released");
        assert!(weak_loop.upgrade().is_none());
    }

    fn write_test_config(config: &std::path::Path) {
        std::fs::write(
            config.join("config.toml"),
            "[general]\ndefault_provider = \"test\"\ndefault_model = \"local\"\n",
        )
        .expect("config");
        std::fs::write(
            config.join("endpoints.toml"),
            "[[endpoint]]\nname = \"test\"\nkind = \"local\"\nbase_url = \"http://127.0.0.1:9/v1/\"\nmodels = [\"local\"]\n",
        )
        .expect("endpoints");
    }

    fn test_engine_options(
        root: &std::path::Path,
        config: &std::path::Path,
    ) -> crate::headless::EngineOptions {
        crate::headless::EngineOptions {
            workspace: crate::workspace::Workspace::new(root),
            config_dir: Some(config.to_path_buf()),
            permissions: crate::agent::policy::PermissionPolicy::OverridePermissions,
            memory: crate::headless::MemoryBackend::Disabled,
            index_codegraph: false,
            require_git: false,
            auto_install_embedder: false,
        }
    }

    #[tokio::test]
    async fn close_waits_for_a_descendant_holding_the_inherited_graph() {
        let project = tempfile::tempdir().expect("project");
        let worktree = tempfile::tempdir().expect("worktree");
        let config = tempfile::tempdir().expect("config");
        write_test_config(config.path());
        let engine = Engine::start(test_engine_options(project.path(), config.path()))
            .await
            .expect("start engine");
        let session = engine
            .spawn(SpawnOptions {
                worktree: Some(worktree.path().to_path_buf()),
                ..Default::default()
            })
            .await
            .expect("spawn worktree parent");
        let inner = session.inner.clone();
        let parent = session.agent;
        let (child, _) = crate::host::runtime::spawn_agent_shared(
            crate::host::runtime::SpawnHost {
                factory: inner.host.factory.clone(),
                manager: inner.host.manager.clone(),
                agent_loops: inner.host.agent_loops.clone(),
            },
            "child".to_string(),
            None,
            Some(parent),
            crate::host::runtime::SessionOptions {
                model: None,
                role: None,
                own_plans_dir: false,
                worktree: None,
                permission: Some(crate::agent::policy::PermissionPolicy::OverridePermissions),
                initial_messages: None,
                limits: None,
                host_tools: Vec::new(),
                trace_identity: None,
            },
        )
        .await
        .expect("spawn a real descendant");
        let (parent_loop, child_loop) = {
            let loops = inner.host.agent_loops.lock().await;
            (
                loops.get(&parent).cloned().expect("parent loop"),
                loops.get(&child).cloned().expect("child loop"),
            )
        };
        assert!(std::sync::Arc::ptr_eq(
            parent_loop.graph.as_ref().expect("parent graph"),
            child_loop.graph.as_ref().expect("inherited child graph")
        ));
        drop(parent_loop);
        let weak_child = std::sync::Arc::downgrade(&child_loop);
        let mut closing = tokio::spawn(async move { session.close_and_wait().await });

        for _ in 0..100 {
            let manager_gone = inner.host.manager.lock().await.get(child).is_none();
            let registry_gone = !inner.host.agent_loops.lock().await.contains_key(&child);
            if manager_gone && registry_gone {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(inner.host.manager.lock().await.get(child).is_none());
        assert!(!inner.host.agent_loops.lock().await.contains_key(&child));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut closing)
                .await
                .is_err(),
            "close returned while a descendant still owned the checkout graph"
        );

        drop(child_loop);
        closing
            .await
            .expect("join close")
            .expect("close after the descendant releases its loop");
        assert!(weak_child.upgrade().is_none());
        std::fs::remove_dir_all(worktree.path()).expect("remove the released worktree");
    }

    #[tokio::test]
    async fn close_waits_for_a_grandchild_after_the_intermediate_exits() {
        let project = tempfile::tempdir().expect("project");
        let worktree = tempfile::tempdir().expect("worktree");
        let config = tempfile::tempdir().expect("config");
        write_test_config(config.path());
        let engine = Engine::start(test_engine_options(project.path(), config.path()))
            .await
            .expect("start engine");
        let session = engine
            .spawn(SpawnOptions {
                worktree: Some(worktree.path().to_path_buf()),
                ..Default::default()
            })
            .await
            .expect("spawn worktree parent");
        let inner = session.inner.clone();
        let parent = session.agent;
        let make_host = || crate::host::runtime::SpawnHost {
            factory: inner.host.factory.clone(),
            manager: inner.host.manager.clone(),
            agent_loops: inner.host.agent_loops.clone(),
        };
        let child_options = || crate::host::runtime::SessionOptions {
            model: None,
            role: None,
            own_plans_dir: false,
            worktree: None,
            permission: Some(crate::agent::policy::PermissionPolicy::OverridePermissions),
            initial_messages: None,
            limits: None,
            host_tools: Vec::new(),
            trace_identity: None,
        };
        let (child, _) = crate::host::runtime::spawn_agent_shared(
            make_host(),
            "child".to_string(),
            None,
            Some(parent),
            child_options(),
        )
        .await
        .expect("spawn child");
        let (grandchild, _) = crate::host::runtime::spawn_agent_shared(
            make_host(),
            "grandchild".to_string(),
            None,
            Some(child),
            child_options(),
        )
        .await
        .expect("spawn grandchild");
        let (parent_loop, grandchild_loop) = {
            let loops = inner.host.agent_loops.lock().await;
            (
                loops.get(&parent).cloned().expect("parent loop"),
                loops.get(&grandchild).cloned().expect("grandchild loop"),
            )
        };
        assert_eq!(grandchild_loop.family_root(), Some(parent));
        assert!(std::sync::Arc::ptr_eq(
            parent_loop.graph.as_ref().expect("parent graph"),
            grandchild_loop.graph.as_ref().expect("inherited graph")
        ));
        drop(parent_loop);
        let weak_grandchild = std::sync::Arc::downgrade(&grandchild_loop);

        inner
            .host
            .manager
            .lock()
            .await
            .send(child, crate::runtime::AgentCommand::Cancel)
            .expect("cancel the intermediate child");
        for _ in 0..100 {
            let manager_gone = inner.host.manager.lock().await.get(child).is_none();
            let registry_gone = !inner.host.agent_loops.lock().await.contains_key(&child);
            if manager_gone && registry_gone {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(inner.host.manager.lock().await.get(child).is_none());
        assert!(!inner.host.agent_loops.lock().await.contains_key(&child));
        assert!(inner.host.manager.lock().await.get(grandchild).is_some());
        assert!(inner
            .host
            .agent_loops
            .lock()
            .await
            .contains_key(&grandchild));

        let mut closing = tokio::spawn(async move { session.close_and_wait().await });
        for _ in 0..100 {
            let manager_gone = inner.host.manager.lock().await.get(grandchild).is_none();
            let registry_gone = !inner
                .host
                .agent_loops
                .lock()
                .await
                .contains_key(&grandchild);
            if manager_gone && registry_gone {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(inner.host.manager.lock().await.get(grandchild).is_none());
        assert!(!inner
            .host
            .agent_loops
            .lock()
            .await
            .contains_key(&grandchild));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut closing)
                .await
                .is_err(),
            "close returned while the grandchild still owned the graph"
        );

        drop(grandchild_loop);
        closing
            .await
            .expect("join close")
            .expect("close after the grandchild releases its loop");
        assert!(weak_grandchild.upgrade().is_none());
        std::fs::remove_dir_all(worktree.path()).expect("remove the released worktree");
    }

    struct DigestLlm {
        caps: crate::provider::Capabilities,
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl crate::provider::LlmClient for DigestLlm {
        fn capabilities(&self) -> &crate::provider::Capabilities {
            &self.caps
        }

        fn kind(&self) -> crate::provider::ProviderKind {
            crate::provider::ProviderKind::OpenAI
        }

        fn model(&self) -> &str {
            "digest-test"
        }

        async fn complete(
            &self,
            _messages: &[crate::provider::Message],
            _tools: &[crate::provider::ToolSchema],
            _tool_choice: Option<crate::provider::ToolChoice>,
        ) -> crate::error::Result<futures::stream::BoxStream<'_, crate::provider::LlmEvent>> {
            if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                Ok(Box::pin(futures::stream::iter([
                    crate::provider::LlmEvent::TextDelta { text: "served".into() },
                    crate::provider::LlmEvent::Finish {
                        reason: crate::provider::FinishReason::Stop,
                    },
                ])))
            } else {
                Ok(Box::pin(futures::stream::pending()))
            }
        }
    }

    #[tokio::test]
    async fn close_waits_for_the_worktree_corpus_digest() {
        let project = tempfile::tempdir().expect("project");
        let worktree = tempfile::tempdir().expect("worktree");
        let config = tempfile::tempdir().expect("config");
        write_test_config(config.path());
        std::fs::create_dir_all(project.path().join(".coding")).expect("memory state");
        let llm = std::sync::Arc::new(DigestLlm {
            caps: crate::provider::Capabilities::openai(),
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let mut options = test_engine_options(project.path(), config.path());
        options.memory = crate::headless::MemoryBackend::Sqlite { path: None };
        let engine = Engine::builder(options)
            .provider(llm)
            .start()
            .await
            .expect("start memory engine");
        let session = engine
            .spawn(SpawnOptions {
                worktree: Some(worktree.path().to_path_buf()),
                ..Default::default()
            })
            .await
            .expect("spawn worktree session");
        let plans_dir = session
            .agent_loop
            .upgrade()
            .expect("parent loop")
            .plans_dir()
            .to_path_buf();
        std::fs::create_dir_all(&plans_dir).expect("plans dir");
        std::fs::write(plans_dir.join("digest.md"), "plan corpus").expect("plan corpus");
        session
            .run_to_completion("your summary is too long — keep it to 3 lines")
            .await
            .expect("establish working memory");
        let gate = crate::runtime::agent::digest_test_gate::install(plans_dir.clone());
        let mut closing = tokio::spawn(async move { session.close_and_wait().await });

        tokio::time::timeout(std::time::Duration::from_secs(2), gate.entered.notified())
            .await
            .expect("digest reached its blocking phase");
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut closing)
                .await
                .is_err(),
            "close returned before the worktree digest finished"
        );
        assert!(plans_dir.join("digest.md").is_file());

        gate.release.notify_one();
        closing
            .await
            .expect("join close")
            .expect("close after the digest completes");
        std::fs::remove_dir_all(worktree.path()).expect("remove the digested worktree");
    }

    #[tokio::test]
    async fn slow_close_does_not_requeue_cancel_until_the_queue_is_full() {
        let project = tempfile::tempdir().expect("project");
        let worktree = tempfile::tempdir().expect("worktree");
        let config = tempfile::tempdir().expect("config");
        write_test_config(config.path());
        std::fs::create_dir_all(project.path().join(".coding")).expect("memory state");
        let llm = std::sync::Arc::new(DigestLlm {
            caps: crate::provider::Capabilities::openai(),
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let mut options = test_engine_options(project.path(), config.path());
        options.memory = crate::headless::MemoryBackend::Sqlite { path: None };
        let engine = Engine::builder(options)
            .provider(llm)
            .start()
            .await
            .expect("start memory engine");
        let session = engine
            .spawn(SpawnOptions {
                worktree: Some(worktree.path().to_path_buf()),
                ..Default::default()
            })
            .await
            .expect("spawn worktree session");
        let plans_dir = session
            .agent_loop
            .upgrade()
            .expect("parent loop")
            .plans_dir()
            .to_path_buf();
        std::fs::create_dir_all(&plans_dir).expect("plans dir");
        std::fs::write(plans_dir.join("slow.md"), "plan corpus").expect("plan corpus");
        session
            .run_to_completion("your summary is too long — keep it to 3 lines")
            .await
            .expect("establish working memory");
        let filler = engine
            .session(&session.id())
            .await
            .expect("second session handle");
        let inner = session.inner.clone();
        let parent = session.agent;
        let (intercept_tx, mut intercepted) = tokio::sync::mpsc::channel(64);
        let original_tx = {
            let mut manager = inner.host.manager.lock().await;
            manager
                .replace_command_sender(parent, intercept_tx)
                .expect("parent handle")
        };
        let gate = crate::runtime::agent::digest_test_gate::install(plans_dir);
        let mut closing = tokio::spawn(async move { session.close_and_wait().await });

        let first = intercepted.recv().await.expect("first cancellation");
        assert!(matches!(first, crate::runtime::AgentCommand::Cancel));
        original_tx.send(first).await.expect("forward first Cancel");
        tokio::time::timeout(std::time::Duration::from_secs(2), gate.entered.notified())
            .await
            .expect("close reached the digest phase");
        let mut full = false;
        for _ in 0..1024 {
            if filler.send(SessionCommand::Clear).await.is_err() {
                full = true;
                break;
            }
        }
        assert!(full, "the digest pause leaves the command queue full");
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(700), &mut closing)
                .await
                .is_err(),
            "close requeued Cancel into the full queue and failed early"
        );

        gate.release.notify_one();
        closing
            .await
            .expect("join close")
            .expect("cancel-once close survives the slow digest");
        let mut cancellations = 1;
        while let Ok(command) = intercepted.try_recv() {
            if matches!(command, crate::runtime::AgentCommand::Cancel) {
                cancellations += 1;
            }
        }
        assert_eq!(cancellations, 1, "Cancel must be sent only once");
        std::fs::remove_dir_all(worktree.path()).expect("remove the released worktree");
    }

    #[tokio::test]
    async fn close_treats_a_closed_stale_command_channel_as_exit_in_progress() {
        let project = tempfile::tempdir().expect("project");
        let config = tempfile::tempdir().expect("config");
        write_test_config(config.path());
        let engine = Engine::start(test_engine_options(project.path(), config.path()))
            .await
            .expect("start engine");
        let session = engine.spawn(SpawnOptions::default()).await.expect("spawn");
        let inner = session.inner.clone();
        let parent = session.agent;
        let held_loop = session.agent_loop.upgrade().expect("parent loop");
        let weak_loop = session.agent_loop.clone();
        let (stale_tx, stale_rx) = tokio::sync::mpsc::channel(1);
        let original_tx = {
            let mut manager = inner.host.manager.lock().await;
            manager
                .replace_command_sender(parent, stale_tx)
                .expect("parent handle")
        };
        drop(stale_rx);

        let mut closing = tokio::spawn(async move { session.close_and_wait().await });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut closing)
                .await
                .is_err(),
            "a closed stale channel must not fail close while the weak is alive"
        );

        original_tx
            .send(crate::runtime::AgentCommand::Cancel)
            .await
            .expect("finish the real agent exit");
        drop(held_loop);
        closing
            .await
            .expect("join close")
            .expect("closed channel is exit-in-progress, not a close failure");
        assert!(weak_loop.upgrade().is_none());
    }

    #[tokio::test]
    async fn spawn_trace_identity_reaches_the_session_loop() {
        // Plan c3ba2b58 step A: the SpawnOptions trace identity survives
        // the engine's pass-through to the host spawn path and lands on the
        // session's loop with the allocated agent/session ids filled in —
        // the Mnemo2 lead/worker spawn contract.
        let project = tempfile::tempdir().expect("project");
        let config = tempfile::tempdir().expect("config");
        write_test_config(config.path());
        let engine = Engine::start(test_engine_options(project.path(), config.path()))
            .await
            .expect("start engine");
        let session = engine
            .spawn(SpawnOptions {
                trace_identity: Some(crate::provider::trace::TraceIdentity {
                    actor_kind: crate::provider::trace::TraceActorKind::Worker,
                    agent_id: String::new(),
                    session_id: String::new(),
                    agent_name: "wt-engine".to_string(),
                    worker_id: Some("wt-engine".to_string()),
                    plan_id: Some("plan-42".to_string()),
                    plan_step: Some(1),
                }),
                ..Default::default()
            })
            .await
            .expect("spawn with a trace identity");
        let loop_arc = session.agent_loop.upgrade().expect("session loop alive");
        let identity = loop_arc.trace_identity().expect("identity stamped");
        assert_eq!(
            identity.actor_kind,
            crate::provider::trace::TraceActorKind::Worker
        );
        assert_eq!(identity.agent_name, "wt-engine");
        assert_eq!(identity.worker_id.as_deref(), Some("wt-engine"));
        assert_eq!(identity.plan_id.as_deref(), Some("plan-42"));
        assert_eq!(identity.plan_step, Some(1));
        assert_eq!(
            identity.agent_id,
            session.agent.to_string(),
            "the allocated AgentId is filled in"
        );
        assert_eq!(
            identity.session_id,
            session.agent.to_string(),
            "the headless SessionId (the same allocation today) stays its own wire concept"
        );
    }
}
