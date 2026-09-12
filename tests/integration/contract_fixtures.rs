// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Golden JSON fixtures for `SerializableAgentEvent` — the IPC contract
//! drift detector (Maintainability H4).
//!
//! For every event variant this test constructs one representative sample,
//! serializes it via `serde_json::to_value`, and asserts EXACT equality
//! against a committed fixture under `frontend/src/lib/ipc-fixtures/`. If a
//! fixture is missing it is written and the test panics with "newly
//! generated — review & re-run"; once committed, any Rust-side shape change
//! (a renamed field, a new `skip_serializing_if`, a changed enum tag) makes
//! this test fail until the fixture is consciously updated. The frontend's
//! vitest contract suite imports the same fixtures and feeds them through
//! the real reducer, so a fixture change is caught on the TS side too — the
//! two sides cannot silently drift.
//!
//! Fixtures are read via `env!("CARGO_MANIFEST_DIR")` (the brain crate root =
//! the repo root), so the same files are the single source of truth for both
//! the Rust and TS drift checks.

use std::path::PathBuf;

use mnemo::provider::{ApprovalPreview, FinishReason};
use mnemo::runtime::channels::{RecallHit, SerializableAgentEvent};
use mnemo::tool::ToolResult;
use mnemo::workflow::WorkflowState;

use serde_json::{json, Value};

/// Resolve the absolute path of a fixture under
/// `frontend/src/lib/ipc-fixtures/` from the brain crate's manifest dir
/// (the repo root).
fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("frontend")
        .join("src")
        .join("lib")
        .join("ipc-fixtures")
        .join(format!("{name}.json"))
}

/// Assert `value` serializes to the committed fixture `name`. On a missing
/// fixture, write it (pretty-printed) and record it as newly generated (the
/// caller panics once at the end so ALL missing fixtures are written in a
/// single run — re-running then asserts them). On a mismatch, print the path
/// + both JSONs and panic.
fn assert_fixture(name: &str, event: SerializableAgentEvent, generated: &mut Vec<String>) {
    let path = fixture_path(name);
    let actual = serde_json::to_value(&event).expect("event serializes");
    if !path.exists() {
        let pretty = serde_json::to_string_pretty(&actual).expect("pretty");
        std::fs::create_dir_all(path.parent().expect("fixture has parent"))
            .expect("create fixtures dir");
        std::fs::write(&path, pretty).expect("write fixture");
        generated.push(path.display().to_string());
        return;
    }
    let raw = std::fs::read_to_string(&path).expect("read fixture");
    let expected: Value = serde_json::from_str(&raw).expect("fixture parses as JSON");
    if actual != expected {
        panic!(
            "fixture drift for {}: expected {}\nactual   {}\n(path {} — update the fixture if the \
             change is intentional)",
            name,
            expected,
            actual,
            path.display()
        );
    }
}

#[test]
fn event_fixtures_match_serde() {
    // One representative sample per SerializableAgentEvent variant, plus two
    // sub-shape extras that lock structurally-distinct wire forms:
    //   - event-finished-other.json : FinishReason::Other renders as
    //     { "other": string } (vs the simple string tags of the other
    //     FinishReason variants).
    //   - event-approval-request-new-file.json : the second ApprovalPreview
    //     variant (`new_file`), whose kind tag + `content` field differ from
    //     the `diff` form used by the main approval-request fixture.
    let samples: Vec<(&str, SerializableAgentEvent)> = vec![
        ("event-started", SerializableAgentEvent::Started),
        (
            "event-text-delta",
            SerializableAgentEvent::TextDelta { text: "hi".into() },
        ),
        (
            "event-reasoning-delta",
            SerializableAgentEvent::ReasoningDelta {
                text: "thinking".into(),
            },
        ),
        (
            "event-tool-call-start",
            SerializableAgentEvent::ToolCallStart {
                index: 0,
                id: "call-1".into(),
                name: "shell".into(),
            },
        ),
        (
            "event-tool-call-arg-delta",
            SerializableAgentEvent::ToolCallArgDelta {
                index: 0,
                fragment: "{\"cmd\":".into(),
            },
        ),
        (
            "event-approval-request",
            SerializableAgentEvent::ApprovalRequest {
                tool_call_id: "call-1".into(),
                tool_name: "file_edit".into(),
                args: json!({ "path": "a.ts", "old_string": "x", "new_string": "y" }),
                preview: Some(ApprovalPreview::Diff {
                    path: "a.ts".into(),
                    diff: "--- a\n+++ b".into(),
                }),
                core_operation: false,
            },
        ),
        (
            "event-approval-request-null-preview",
            SerializableAgentEvent::ApprovalRequest {
                tool_call_id: "call-2".into(),
                tool_name: "shell".into(),
                args: json!({}),
                preview: None,
                core_operation: false,
            },
        ),
        (
            "event-approval-request-new-file",
            SerializableAgentEvent::ApprovalRequest {
                tool_call_id: "call-3".into(),
                tool_name: "file_write".into(),
                args: json!({ "path": "new.ts" }),
                preview: Some(ApprovalPreview::NewFile {
                    path: "new.ts".into(),
                    content: "export const x = 1;\n".into(),
                }),
                core_operation: false,
            },
        ),
        (
            "event-tool-result",
            SerializableAgentEvent::ToolResult {
                tool_call_id: "call-1".into(),
                result: ToolResult {
                    success: true,
                    output: "ok".into(),
                    data: None,
                },
            },
        ),
        (
            "event-usage",
            SerializableAgentEvent::Usage {
                prompt_tokens: 100,
                completion_tokens: 50,
                reasoning_tokens: 10,
                cached_tokens: 20,
                ttft_ms: Some(1000),
                generation_ms: Some(2000),
            },
        ),
        (
            "event-context-usage",
            SerializableAgentEvent::ContextUsage {
                used: 1000,
                max: 8000,
                breakdown: mnemo::runtime::ContextBreakdown {
                    system: 500,
                    user: 200,
                    assistant: 200,
                    tool: 100,
                },
            },
        ),
        (
            "event-workflow-state-changed",
            SerializableAgentEvent::WorkflowStateChanged {
                state: WorkflowState::Executing,
                top_plan_id: Some("plan-1a2b3c".into()),
            },
        ),
        (
            "event-step-completed",
            SerializableAgentEvent::StepCompleted { step_index: 3 },
        ),
        (
            "event-suggestion-injected",
            SerializableAgentEvent::SuggestionInjected {
                text: "do X".into(),
                images: vec!["data:image/png;base64,iVBORw0KGgo=".into()],
            },
        ),
        (
            "event-prompt-dispatched",
            SerializableAgentEvent::PromptDispatched {
                text: "goal".into(),
                images: vec!["data:image/png;base64,iVBORw0KGgo=".into()],
            },
        ),
        (
            "event-skill-started",
            SerializableAgentEvent::SkillStarted {
                name: "merge_to_main".into(),
                prompt: "Merge the branch into main.".into(),
            },
        ),
        (
            "event-model-changed",
            SerializableAgentEvent::ModelChanged {
                model: "gpt-5".into(),
                provider: None,
                reasoning_effort: Some("low".into()),
            },
        ),
        (
            "event-memory-recalled",
            SerializableAgentEvent::MemoryRecalled {
                hits: vec![
                    RecallHit {
                        tier: "semantic".into(),
                        title: "merge instructions".into(),
                    },
                    RecallHit {
                        tier: "procedural".into(),
                        title: "release checklist".into(),
                    },
                    RecallHit {
                        tier: "episodic".into(),
                        title: "session summary 2026-04-20".into(),
                    },
                ],
            },
        ),
        (
            "event-finished",
            SerializableAgentEvent::Finished {
                reason: FinishReason::Stop,
            },
        ),
        (
            "event-finished-other",
            SerializableAgentEvent::Finished {
                reason: FinishReason::Other("max_tokens".into()),
            },
        ),
        (
            "event-phase",
            SerializableAgentEvent::Phase {
                phase: mnemo::runtime::channels::PhaseKind::RunningTools,
            },
        ),
        ("event-exited", SerializableAgentEvent::Exited),
        (
            "event-child-finished",
            SerializableAgentEvent::ChildFinished {
                child_id: 2,
                name: "reviewer".into(),
                success: true,
            },
        ),
        (
            "event-error",
            SerializableAgentEvent::Error {
                error: "boom".into(),
                retrying: false,
            },
        ),
        (
            "event-compacted",
            SerializableAgentEvent::Compacted {
                before: 500_000,
                after: 24_000,
            },
        ),
        (
            "event-vision-describe",
            SerializableAgentEvent::VisionDescribe {
                index: 1,
                total: 2,
                query: "Describe this image in detail.".into(),
            },
        ),
        (
            "event-vision-described",
            SerializableAgentEvent::VisionDescribed {
                index: 1,
                total: 2,
                success: true,
                description: "a photo of a cat".into(),
            },
        ),
    ];

    let mut generated: Vec<String> = Vec::new();
    for (name, event) in samples {
        assert_fixture(name, event, &mut generated);
    }
    if !generated.is_empty() {
        panic!(
            "newly generated {} fixture(s) — review for sanity and re-run to assert them:\n{}",
            generated.len(),
            generated.join("\n")
        );
    }
}
