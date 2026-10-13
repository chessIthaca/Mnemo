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
    EventReceiver, PendingInteraction, SessionCommand, SessionEvent, SessionId, SessionSnapshot,
    Transcript, TurnOutcome,
};
use crate::runtime::channels::ConversationExport;
use crate::host::runtime::HostRuntime;
use crate::runtime::channels::{
    AgentEvent, AgentId, Approval, HostToolOutcome, PolicyAnswer, SerializableAgentEvent,
    SteerPayload, UserAnswer,
};
use crate::runtime::AgentCommand;

/// Shared engine state the sessions + the pump hold: the host core, the
/// live-session registry (the closed check), and the pending responder
/// maps (the pump stashes, `Approve`/`Answer` resolve).
pub(crate) struct EngineInner {
    /// The shared host core (3a.2).
    pub(crate) host: HostRuntime,
    /// Live sessions (SessionId → the agent id).
    pub(crate) sessions: Mutex<HashMap<SessionId, AgentId>>,
    /// Approval responders by request id.
    pub(crate) pending_approvals: Mutex<HashMap<String, oneshot::Sender<Approval>>>,
    /// Question responders by question id.
    pub(crate) pending_questions: Mutex<HashMap<String, oneshot::Sender<UserAnswer>>>,
    /// Host-tool outcome responders by call id (3c/G2) — the pump stashes
    /// them; `SessionCommand::HostToolResult` resolves.
    pub(crate) pending_host_tools: Mutex<HashMap<String, oneshot::Sender<HostToolOutcome>>>,
    /// Policy-hook answer responders by hook id (3c/G6 tier 3) — the pump
    /// stashes them; `SessionCommand::PolicyAnswer` resolves.
    pub(crate) pending_hooks: Mutex<HashMap<String, oneshot::Sender<PolicyAnswer>>>,
    /// Engine-registered Rust custom tools (3c/G2, §5.9.2) — the Host
    /// group, joined into every subsequent spawn's registry (in-process
    /// execution like any kernel tool — NOT a spec-based host tool; no
    /// round-trip). std Mutex: the register is sync, the lock is never
    /// held across an await.
    pub(crate) host_tools: std::sync::Mutex<Vec<std::sync::Arc<dyn crate::tool::Tool>>>,
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
                    .insert(tool_call_id.clone(), responder);
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
                    .insert(question_id.clone(), responder);
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
                    .insert(call_id.clone(), responder);
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
                    .insert(hook_id.clone(), responder);
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
            SessionCommand::Steer { text } => {
                self.agent_command(AgentCommand::Suggestion(SteerPayload {
                    text,
                    images: vec![],
                }))
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
                let responder = self.inner.pending_approvals.lock().await.remove(&request_id);
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
                let responder = self.inner.pending_questions.lock().await.remove(&question_id);
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
                let responder = self.inner.pending_host_tools.lock().await.remove(&call_id);
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
                let responder = self.inner.pending_hooks.lock().await.remove(&hook_id);
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
        let pending = {
            let approvals = self.inner.pending_approvals.lock().await;
            if let Some(id) = approvals.keys().next() {
                Some(PendingInteraction::Approval {
                    request_id: id.clone(),
                })
            } else {
                let questions = self.inner.pending_questions.lock().await;
                if let Some(id) = questions.keys().next() {
                    Some(PendingInteraction::Question {
                        question_id: id.clone(),
                    })
                } else {
                    let host_tools = self.inner.pending_host_tools.lock().await;
                    if let Some(id) = host_tools.keys().next() {
                        Some(PendingInteraction::HostToolCall {
                            call_id: id.clone(),
                        })
                    } else {
                        let hooks = self.inner.pending_hooks.lock().await;
                        hooks.keys().next().map(|id| PendingInteraction::PolicyHook {
                            hook_id: id.clone(),
                        })
                    }
                }
            }
        };
        Ok(SessionSnapshot {
            transcript,
            model,
            used_tokens,
            max_tokens,
            pending,
        })
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
        loop {
            match rx.recv().await {
                Ok(SessionEvent { agent, event, .. }) => {
                    if agent != self.agent {
                        // Other agents' events ride the same bus, tagged —
                        // they never end this session's turn.
                        continue;
                    }
                    match event {
                        SerializableAgentEvent::TextDelta { text } => {
                            final_text.push_str(&text)
                        }
                        SerializableAgentEvent::Finished { reason } => {
                            // (Parking is engine-scoped — the pump set it.)
                            return Ok(TurnOutcome {
                                final_text,
                                finish_reason: Some(reason),
                            })
                        }
                        SerializableAgentEvent::Exited => {
                            return Ok(TurnOutcome {
                                final_text,
                                finish_reason: None,
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

    async fn agent_command(&self, command: AgentCommand) -> Result<(), EngineError> {
        let mgr = self.inner.host.manager.lock().await;
        mgr.send(self.agent, command)
            .map_err(|_| EngineError::session_closed("the agent task ended"))
    }
}
