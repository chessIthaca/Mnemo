// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The headless session (§5.3 :315-326) — one agent's command/event
//! surface: `send` (the public `SessionCommand` → the kernel
//! `AgentCommand`), the multi-subscriber event bus, `run_to_completion`,
//! `close`. The engine's event pump (fan-in → broadcast + the pending
//! approval/question maps) lives here — the session's moving parts in
//! one place.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{broadcast, mpsc, Mutex, oneshot};

use crate::headless::error::EngineError;
use crate::headless::types::{
    EventReceiver, PendingInteraction, SessionCommand, SessionEvent, SessionId, SessionSnapshot,
    Transcript, TurnOutcome,
};
use crate::runtime::channels::ConversationExport;
use crate::host::runtime::HostRuntime;
use crate::runtime::channels::{
    AgentEvent, AgentId, Approval, SerializableAgentEvent, SteerPayload, UserAnswer,
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
            AgentEvent::Finished { .. } => {
                inner.host.bookkeeping.on_turn_ended(agent, true).await
            }
            AgentEvent::Exited => inner.host.bookkeeping.on_exited(agent).await,
            _ => {}
        }
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
        // Best-effort broadcast: zero subscribers → the event drops.
        let _ = bus.send(SessionEvent {
            agent,
            event: forwarded.event,
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
                        EngineError::session_closed("the approval already resolved")
                    }),
                    None => Err(EngineError::session_closed(format!(
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
                        EngineError::session_closed("the question already resolved")
                    }),
                    None => Err(EngineError::session_closed(format!(
                        "no pending question '{question_id}'"
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
    /// context usage + the pending approval/question (if any). The turn
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
        let pending = {
            let approvals = self.inner.pending_approvals.lock().await;
            if let Some(id) = approvals.keys().next() {
                Some(PendingInteraction::Approval {
                    request_id: id.clone(),
                })
            } else {
                let questions = self.inner.pending_questions.lock().await;
                questions
                    .keys()
                    .next()
                    .map(|id| PendingInteraction::Question {
                        question_id: id.clone(),
                    })
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
        self.send(SessionCommand::Prompt {
            text: prompt.to_string(),
            images: vec![],
        })
        .await?;
        let mut rx = self.events();
        let mut final_text = String::new();
        loop {
            match rx.recv().await {
                Ok(SessionEvent { agent, event }) => {
                    if agent != self.agent {
                        // Other agents' events ride the same bus, tagged —
                        // they never end this session's turn.
                        continue;
                    }
                    match event {
                        SerializableAgentEvent::TextDelta { text } => {
                            final_text.push_str(&text)
                        }
                        SerializableAgentEvent::Finished { .. } => {
                            return Ok(TurnOutcome { final_text })
                        }
                        SerializableAgentEvent::Exited => {
                            return Ok(TurnOutcome { final_text })
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
