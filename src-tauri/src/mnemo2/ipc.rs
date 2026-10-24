// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]

//! Mnemo2's typed Tauri command surface.
//!
//! One [`LeadRuntime`] lives in [`Mnemo2State`]. Commands never hold the
//! runtime lock across an engine await; they clone the `Arc` and release the
//! slot first. A single forwarder subscribes to the runtime's event bus and
//! emits every [`LeadEvent`] on [`LEAD_EVENT_CHANNEL`].
//!
//! ## Event contract
//!
//! Each payload is:
//!
//! ```json
//! { "generation": 1, "event": { "agent": 7, "event": { "kind": "text_delta", "text": "…" } } }
//! ```
//!
//! `generation` identifies the live session. A model switch resumes a fresh
//! session and advances the generation; the frontend MUST ignore envelopes
//! whose generation differs from its current snapshot.

use std::path::PathBuf;
use std::sync::Arc;

use serde::Deserialize;
use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{broadcast, Mutex};

use mnemo::{Approval, UserAnswer};

use crate::mnemo2::runtime::{
    LeadRuntime, LeadRuntimeOptions, LeadSnapshot, LEAD_EVENT_CHANNEL,
};

/// The shared Mnemo2 app state: one live lead and its event forwarder.
#[derive(Default)]
pub struct Mnemo2State {
    runtime: Mutex<Option<Arc<LeadRuntime>>>,
    forwarder: Mutex<Option<JoinHandle<()>>>,
}

/// The UI's approval decision on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    /// Approve the action.
    Approve,
    /// Deny the action.
    Deny,
    /// Deny this and all subsequent approvals in the turn.
    DenyAll,
}

impl From<ApprovalDecision> for Approval {
    fn from(value: ApprovalDecision) -> Self {
        match value {
            ApprovalDecision::Approve => Approval::Approve,
            ApprovalDecision::Deny => Approval::Deny,
            ApprovalDecision::DenyAll => Approval::DenyAll,
        }
    }
}

/// The UI's answer to an `ask_user` question.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AnswerInput {
    /// A clicked option.
    Choice {
        /// The 0-indexed option position.
        index: usize,
    },
    /// A freeform answer.
    Freeform {
        /// The text the user typed.
        text: String,
    },
}

impl From<AnswerInput> for UserAnswer {
    fn from(value: AnswerInput) -> Self {
        match value {
            AnswerInput::Choice { index } => UserAnswer::Choice { index },
            AnswerInput::Freeform { text } => UserAnswer::Freeform { text },
        }
    }
}

async fn current(state: &Mnemo2State) -> Result<Arc<LeadRuntime>, String> {
    state
        .runtime
        .lock()
        .await
        .clone()
        .ok_or_else(|| "Mnemo2 has no open lead; call lead_open first".to_string())
}

/// Open (or replace) the lead conversation on a project.
#[tauri::command]
pub async fn lead_open(
    app: AppHandle,
    state: State<'_, Mnemo2State>,
    workspace_root: String,
    config_dir: Option<String>,
    model: Option<String>,
) -> Result<LeadSnapshot, String> {
    if let Some(forwarder) = state.forwarder.lock().await.take() {
        forwarder.abort();
    }
    if let Some(old) = state.runtime.lock().await.take() {
        old.shutdown().await;
    }
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|err| format!("could not resolve the Mnemo2 app data dir: {err}"))?;
    let runtime = Arc::new(
        LeadRuntime::open(LeadRuntimeOptions {
            workspace_root: PathBuf::from(workspace_root),
            config_dir: config_dir.map(PathBuf::from),
            state_dir: app_data.join("lead"),
            model,
        })
        .await?,
    );
    let snapshot = runtime.snapshot().await?;
    let mut events = runtime.subscribe();
    let app_handle = app.clone();
    let forwarder = tauri::async_runtime::spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => {
                    let _ = app_handle.emit(LEAD_EVENT_CHANNEL, &event);
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    *state.runtime.lock().await = Some(runtime);
    *state.forwarder.lock().await = Some(forwarder);
    Ok(snapshot)
}

/// Read the current lead snapshot (transcript, model, pending interaction).
#[tauri::command]
pub async fn lead_snapshot(state: State<'_, Mnemo2State>) -> Result<LeadSnapshot, String> {
    current(&state).await?.snapshot().await
}

/// Send a user prompt to the real lead.
#[tauri::command]
pub async fn lead_prompt(state: State<'_, Mnemo2State>, text: String) -> Result<(), String> {
    current(&state).await?.send_prompt(text).await
}

/// Cancel the current lead generation without closing the session.
#[tauri::command]
pub async fn lead_interrupt(state: State<'_, Mnemo2State>) -> Result<(), String> {
    current(&state).await?.interrupt().await
}

/// Change the lead model while idle, resuming the transcript on the new model.
#[tauri::command]
pub async fn lead_set_model(
    state: State<'_, Mnemo2State>,
    spec: String,
) -> Result<LeadSnapshot, String> {
    current(&state).await?.set_model(spec).await
}

/// Resolve a pending approval request.
#[tauri::command]
pub async fn lead_approve(
    state: State<'_, Mnemo2State>,
    request_id: String,
    decision: ApprovalDecision,
) -> Result<(), String> {
    current(&state)
        .await?
        .approve(request_id, decision.into())
        .await
}

/// Resolve a pending `ask_user` question.
#[tauri::command]
pub async fn lead_answer(
    state: State<'_, Mnemo2State>,
    question_id: String,
    answer: AnswerInput,
) -> Result<(), String> {
    current(&state)
        .await?
        .answer(question_id, answer.into())
        .await
}

/// Close the lead session and stop its event forwarder.
#[tauri::command]
pub async fn lead_close(state: State<'_, Mnemo2State>) -> Result<(), String> {
    if let Some(forwarder) = state.forwarder.lock().await.take() {
        forwarder.abort();
    }
    let runtime = state.runtime.lock().await.take();
    if let Some(runtime) = runtime {
        runtime.shutdown().await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mnemo::headless::SessionEvent;
    use mnemo::runtime::channels::SerializableAgentEvent;

    #[test]
    fn approval_decisions_parse_from_the_wire() {
        for (wire, expected) in [
            ("\"approve\"", ApprovalDecision::Approve),
            ("\"deny\"", ApprovalDecision::Deny),
            ("\"deny_all\"", ApprovalDecision::DenyAll),
        ] {
            let parsed: ApprovalDecision = serde_json::from_str(wire).expect("decision");
            assert_eq!(parsed, expected);
        }
    }

    #[test]
    fn question_answers_parse_from_the_wire() {
        let choice: AnswerInput =
            serde_json::from_str(r#"{"kind":"choice","index":2}"#).expect("choice");
        assert_eq!(choice, AnswerInput::Choice { index: 2 });
        let freeform: AnswerInput =
            serde_json::from_str(r#"{"kind":"freeform","text":"later"}"#).expect("freeform");
        assert_eq!(
            freeform,
            AnswerInput::Freeform {
                text: "later".into()
            }
        );
    }

    #[test]
    fn lead_event_envelope_carries_generation_and_inner_event() {
        let event = crate::mnemo2::runtime::LeadEvent {
            generation: 7,
            event: SessionEvent {
                agent: 3,
                event: SerializableAgentEvent::TextDelta {
                    text: "hello".into(),
                },
                rates: None,
            },
        };
        let value = serde_json::to_value(event).expect("serialize");
        assert_eq!(value["generation"], 7);
        assert_eq!(value["event"]["agent"], 3);
        assert_eq!(value["event"]["event"]["kind"], "text_delta");
        assert_eq!(value["event"]["event"]["text"], "hello");
        assert_eq!(LEAD_EVENT_CHANNEL, "mnemo2://lead-event");
    }
}
