// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Phase A exit-criteria test — the IPC bridge round-trip.
//!
//! Verifies that:
//! 1. AgentEvents convert to serializable form correctly.
//! 2. The approval round-trip works: the oneshot is held in PendingApprovals,
//!    resolved by `resolve()`, and the agent receives the `Approval`.
//! 3. The serialized events round-trip through JSON (what the frontend sees).

use mnemo::provider::{ApprovalPreview, FinishReason};
use mnemo::runtime::channels::{AgentEvent, Approval, SerializableAgentEvent, SerializedEvent};
use mnemo::tool::ToolResult;

// We can't import from src-tauri (it's a separate crate), so we re-test the
// core logic that the IPC adapter relies on: into_serializable + the approval
// map pattern. The actual PendingApprovals map lives in src-tauri, keyed by
// (agent_id, tool_call_id) so that cleanup_for_agent drops only the exited
// agent's approvals — the multi-agent isolation behavior is unit-tested in
// src-tauri/src/ipc/approval.rs. The channel contract itself is tested here.

#[tokio::test]
async fn text_delta_serializes() {
    let event = AgentEvent::TextDelta("hello".into());
    let SerializedEvent {
        event: serial,
        approval_sender: sender,
        ..
    } = event.into_serializable();
    assert!(sender.is_none());
    match &serial {
        SerializableAgentEvent::TextDelta { text } => assert_eq!(text, "hello"),
        _ => panic!("expected TextDelta"),
    }
    // JSON round-trip (what the frontend receives).
    let json = serde_json::to_string(&serial).unwrap();
    assert!(json.contains("\"kind\":\"text_delta\""));
    assert!(json.contains("hello"));
    let back: SerializableAgentEvent = serde_json::from_str(&json).unwrap();
    match back {
        SerializableAgentEvent::TextDelta { text } => assert_eq!(text, "hello"),
        _ => panic!("expected TextDelta after round-trip"),
    }
}

#[tokio::test]
async fn approval_request_extracts_sender() {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let event = AgentEvent::ApprovalRequest {
        tool_call_id: "call_1".into(),
        tool_name: "file_edit".into(),
        args: serde_json::json!({"path": "a.rs"}),
        preview: Some(ApprovalPreview::Diff {
            path: "a.rs".into(),
            diff: "--- a.rs\n+++ a.rs\n".into(),
        }),
        core_operation: false,
        responder: tx,
    };
    let SerializedEvent {
        event: serial,
        approval_sender: sender,
        ..
    } = event.into_serializable();
    assert!(sender.is_some(), "sender should be extracted");

    // The serializable form has no sender.
    match &serial {
        SerializableAgentEvent::ApprovalRequest {
            tool_call_id,
            tool_name,
            ..
        } => {
            assert_eq!(tool_call_id, "call_1");
            assert_eq!(tool_name, "file_edit");
        }
        _ => panic!("expected ApprovalRequest"),
    }

    // The sender is held separately (in the real adapter, by PendingApprovals).
    // Resolve it.
    sender.unwrap().send(Approval::Approve).unwrap();
    let approval = rx.await.unwrap();
    assert_eq!(approval, Approval::Approve);
}

#[tokio::test]
async fn approval_deny_roundtrip() {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let event = AgentEvent::ApprovalRequest {
        tool_call_id: "call_2".into(),
        tool_name: "shell".into(),
        args: serde_json::json!({"command": "rm -rf /"}),
        preview: None,
        core_operation: false,
        responder: tx,
    };
    let SerializedEvent {
        approval_sender: sender,
        ..
    } = event.into_serializable();
    sender.unwrap().send(Approval::Deny).unwrap();
    assert_eq!(rx.await.unwrap(), Approval::Deny);
}

#[tokio::test]
async fn tool_result_serializes() {
    let event = AgentEvent::ToolResult {
        tool_call_id: "call_3".into(),
        result: ToolResult::success("file contents here"),
    };
    let SerializedEvent {
        event: serial,
        approval_sender: sender,
        ..
    } = event.into_serializable();
    assert!(sender.is_none());
    let json = serde_json::to_string(&serial).unwrap();
    assert!(json.contains("tool_result"));
    assert!(json.contains("file contents here"));
    let back: SerializableAgentEvent = serde_json::from_str(&json).unwrap();
    match back {
        SerializableAgentEvent::ToolResult { result, .. } => {
            assert!(result.success);
            assert_eq!(result.output, "file contents here");
        }
        _ => panic!("expected ToolResult"),
    }
}

#[tokio::test]
async fn workflow_state_changed_serializes() {
    let event = AgentEvent::WorkflowStateChanged {
        state: mnemo::workflow::WorkflowState::Executing,
        top_plan_id: Some("abc123".into()),
    };
    let SerializedEvent { event: serial, .. } = event.into_serializable();
    let json = serde_json::to_string(&serial).unwrap();
    assert!(json.contains("workflow_state_changed"));
    assert!(json.contains("executing"));
    assert!(json.contains("abc123"));
}

#[tokio::test]
async fn usage_serializes() {
    let event = AgentEvent::Usage {
        prompt_tokens: 1200,
        completion_tokens: 340,
        reasoning_tokens: 280,
        cached_tokens: 900,
        ttft_ms: Some(420),
        generation_ms: Some(3100),
    };
    let SerializedEvent {
        event: serial,
        approval_sender: sender,
        ..
    } = event.into_serializable();
    assert!(sender.is_none(), "usage has no oneshot sender");
    let json = serde_json::to_string(&serial).unwrap();
    assert!(json.contains("\"kind\":\"usage\""));
    assert!(json.contains("1200"));
    assert!(json.contains("340"));
    assert!(json.contains("280"));
    assert!(json.contains("900"));
    assert!(json.contains("420"));
    assert!(json.contains("3100"));
    // Round-trip (what the frontend receives).
    let back: SerializableAgentEvent = serde_json::from_str(&json).unwrap();
    match back {
        SerializableAgentEvent::Usage {
            prompt_tokens,
            completion_tokens,
            reasoning_tokens,
            cached_tokens,
            ttft_ms,
            generation_ms,
        } => {
            assert_eq!(prompt_tokens, 1200);
            assert_eq!(completion_tokens, 340);
            assert_eq!(reasoning_tokens, 280);
            assert_eq!(cached_tokens, 900);
            assert_eq!(ttft_ms, Some(420));
            assert_eq!(generation_ms, Some(3100));
        }
        _ => panic!("expected Usage"),
    }
}

#[tokio::test]
async fn finished_serializes() {
    let event = AgentEvent::Finished {
        reason: FinishReason::Stop,
    };
    let SerializedEvent { event: serial, .. } = event.into_serializable();
    let json = serde_json::to_string(&serial).unwrap();
    assert!(json.contains("finished"));
    assert!(json.contains("stop"));
}

#[tokio::test]
async fn error_serializes() {
    let event = AgentEvent::Error {
        error: "something broke".into(),
        retrying: false,
    };
    let SerializedEvent { event: serial, .. } = event.into_serializable();
    let json = serde_json::to_string(&serial).unwrap();
    assert!(json.contains("error"));
    assert!(json.contains("something broke"));
    assert!(json.contains("retrying"));
}

#[tokio::test]
async fn full_agent_event_lifecycle_serializes() {
    // Simulate a full turn: Started → TextDelta → ToolCallStart → ToolCallArgDelta
    // → ToolResult → Finished. All should serialize cleanly.
    let events = vec![
        AgentEvent::Started,
        AgentEvent::TextDelta("Let me read ".into()),
        AgentEvent::TextDelta("the file.".into()),
        AgentEvent::ToolCallStart {
            index: 0,
            id: "call_1".into(),
            name: "file_read".into(),
        },
        AgentEvent::ToolCallArgDelta {
            index: 0,
            fragment: r#"{"path":"a.rs"}"#.into(),
        },
        AgentEvent::ToolResult {
            tool_call_id: "call_1".into(),
            result: ToolResult::success("fn main() {}"),
        },
        AgentEvent::Finished {
            reason: FinishReason::Stop,
        },
    ];

    for event in events {
        let SerializedEvent {
            event: serial,
            approval_sender: sender,
            ..
        } = event.into_serializable();
        assert!(sender.is_none(), "no approval in lifecycle events");
        // Must serialize (this is what crosses the IPC boundary).
        let json = serde_json::to_string(&serial).unwrap();
        let _back: SerializableAgentEvent = serde_json::from_str(&json).unwrap();
    }
}
