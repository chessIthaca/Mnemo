// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

use super::context;
use super::loop_impl::{AgentLoop, AgentLoopConfig};
use crate::config::SafetyMode;
use crate::memory::embedder::HashEmbedder;
use crate::memory::{MemoryStore, MemoryTier};
use crate::provider::{
    Capabilities, FinishReason, LlmClient, LlmEvent, Message, ProviderKind, Role,
    ToolSchema,
};
use crate::runtime::{AgentEvent, PhaseKind};
use crate::tool::agent::sandbox::Sandbox;
use crate::tool::agent::{file_read::FileReadTool, file_write::FileWriteTool, git::GitTool};
use crate::tool::memory::retrieval::MemorySearchTool;
use crate::tool::memory::MemoryWriteTool;
use crate::tool::workflow::plan::{CompleteStepTool, CreatePlanTool};
use crate::tool::ToolRegistry;
use crate::workflow::Workflow;
use async_trait::async_trait;
use futures::stream::BoxStream;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use tempfile::tempdir;
use tokio::sync::{mpsc, Mutex};

/// Build an [`AgentLoopConfig`] with the common test defaults: Autonomous
/// safety mode, a 128 K / 50% context manager, no memory, no vision. The
/// four varying args (provider, tools, workflow, sandbox) are passed in;
/// everything else is the same across nearly every test.
fn test_config(
    provider: Arc<dyn LlmClient>,
    tools: Arc<ToolRegistry>,
    workflow: Arc<Mutex<Workflow>>,
    sandbox: Arc<Sandbox>,
) -> AgentLoopConfig {
    AgentLoopConfig {
        provider,
        tools,
        workflow,
        sandbox,
        safety_mode: SafetyMode::Autonomous,
        context_manager: context::ContextManager::new(128_000, 0.5),
        memory: None,
        vision: None,
    }
}

/// A mock provider that emits canned events. Returns each response set in
/// order; after the last, returns an empty text + Stop (ending the turn).
struct MockProvider {
    /// A queue of response sets. Each call to `complete` pops the front.
    responses: Arc<Mutex<std::collections::VecDeque<Vec<LlmEvent>>>>,
    caps: Capabilities,
    /// Tools-phase durations reported via `record_tools_phase_ms` (test hook
    /// for the per-trace-entry phase timings).
    tools_phases: Arc<std::sync::Mutex<Vec<u32>>>,
    /// The name `provider_name()` reports ("" = unnamed → stats record
    /// endpoint None, matching the LlmClient default).
    name: String,
}

impl MockProvider {
    /// Create a mock that always returns the same single response.
    fn single(events: Vec<LlmEvent>) -> Self {
        let mut dq = std::collections::VecDeque::new();
        dq.push_back(events);
        Self {
            responses: Arc::new(Mutex::new(dq)),
            caps: Capabilities::openai(),
            tools_phases: Arc::new(std::sync::Mutex::new(Vec::new())),
            name: String::new(),
        }
    }

    /// Create a mock that returns responses in sequence.
    fn sequence(responses: Vec<Vec<LlmEvent>>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses.into())),
            caps: Capabilities::openai(),
            tools_phases: Arc::new(std::sync::Mutex::new(Vec::new())),
            name: String::new(),
        }
    }

    /// Set the name `provider_name()` reports — the stats endpoint
    /// dimension (see `run_turn_records_request_stats_on_usage`).
    fn named(mut self, name: &str) -> Self {
        self.name = name.to_string();
        self
    }

    /// The tools-phase durations the turn loop reported, in order.
    fn take_tools_phases(&self) -> Vec<u32> {
        std::mem::take(
            &mut *self
                .tools_phases
                .lock()
                .expect("tools_phases lock poisoned"),
        )
    }
}

#[async_trait]
impl LlmClient for MockProvider {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
    fn kind(&self) -> ProviderKind {
        ProviderKind::OpenAI
    }
    fn model(&self) -> &str {
        "mock"
    }
    fn provider_name(&self) -> &str {
        &self.name
    }

    fn record_tools_phase_ms(&self, ms: u32) {
        self.tools_phases
            .lock()
            .expect("tools_phases lock poisoned")
            .push(ms);
    }

    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _tool_choice: Option<crate::provider::ToolChoice>,
    ) -> crate::error::Result<BoxStream<'_, LlmEvent>> {
        let mut responses = self.responses.lock().await;
        let events = if let Some(front) = responses.pop_front() {
            front
        } else {
            // No more responses — return a stop to end the turn.
            vec![LlmEvent::Finish {
                reason: FinishReason::Stop,
            }]
        };
        drop(responses);
        let stream = futures::stream::iter(events);
        Ok(Box::pin(stream))
    }
}

/// A minimal `spawn_agent` stub so dispatch tests can reach the
/// failed-reviewer respawn gate (which sits after the unknown-tool check).
struct SpawnAgentStub;

#[async_trait]
impl crate::tool::Tool for SpawnAgentStub {
    fn name(&self) -> &str {
        "spawn_agent"
    }
    fn category(&self) -> crate::tool::ToolCategory {
        crate::tool::ToolCategory::Agent
    }
    fn schema(&self) -> crate::provider::ToolSchema {
        crate::provider::ToolSchema::new(
            "spawn_agent",
            "spawn a subagent",
            serde_json::json!({"type": "object", "properties": {}}),
        )
    }
    fn safety(&self) -> crate::tool::SafetyLevel {
        crate::tool::SafetyLevel::NeedsApproval
    }
    async fn execute(&self, _args: serde_json::Value) -> crate::tool::ToolResult {
        crate::tool::ToolResult::success("spawned")
    }
}

/// Build a tool registry with file + workflow + memory tools, backed by a
/// fresh in-memory memory store.
fn make_registry(
    sandbox: Sandbox,
    workflow: Arc<tokio::sync::Mutex<Workflow>>,
) -> Arc<ToolRegistry> {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(FileReadTool::new(sandbox.clone())));
    registry.register(Box::new(FileWriteTool::new(sandbox.clone())));
    registry.register(Box::new(CreatePlanTool::new(workflow.clone())));
    registry.register(Box::new(CompleteStepTool::new(workflow.clone())));
    // abandon_plan — registered so the review-exit gate test exercises the
    // escape hatch through dispatch (otherwise the unknown-tool check
    // fires before the gate is reached).
    registry.register(Box::new(crate::tool::workflow::plan::AbandonPlanTool::new(
        workflow.clone(),
    )));
    // spawn_agent — registered so the failed-reviewer respawn gate (which
    // sits after the unknown-tool check in dispatch) is reachable in tests.
    registry.register(Box::new(SpawnAgentStub));
    // finish — registered so the dispatch main-agent-only gate (not the
    // unknown-tool check) is what subagent tests exercise. The reviews dir
    // mirrors the factory's derivation (plans_dir.parent().join("reviews")
    // with plans_dir = <root>/plans → <root>/reviews).
    registry.register(Box::new(crate::tool::workflow::plan::FinishTool::new(
        workflow.clone(),
        sandbox.root().join("reviews"),
    )));
    // Memory tools
    let embedder: Arc<dyn crate::memory::Embedder> = Arc::new(HashEmbedder::new());
    let store: Arc<dyn crate::memory::MemoryStoreTrait> =
        Arc::new(MemoryStore::open_in_memory(embedder).unwrap());
    registry.register(Box::new(MemoryWriteTool::new(store.clone())));
    registry.register(Box::new(MemorySearchTool::new(store)));
    Arc::new(registry)
}

#[tokio::test]
async fn full_turn_text_response() {
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());
    let provider = Arc::new(MockProvider::single(vec![
        LlmEvent::TextDelta {
            text: "Hello!".into(),
        },
        LlmEvent::Finish {
            reason: FinishReason::Stop,
        },
    ]));
    let agent = AgentLoop::new(
        test_config(provider, registry, workflow, sandbox.clone()),
        crate::project::Constitution::default(),
    );

    let (fanin_tx, _fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text("hi")];

    let outcome = agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await
        .unwrap();

    assert_eq!(outcome.text, "Hello!");
    assert_eq!(outcome.tool_calls_made, 0);
    // The conversation should have: system, user, assistant.
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[2].role, Role::Assistant);
}

#[tokio::test]
async fn null_turn_does_not_corrupt_history() {
    // Regression: when the model returns a "null turn" — a Finish with no
    // TextDelta and no tool calls (the "agent just stops" moment) — run_turn
    // must NOT push an empty assistant message into the persistent history.
    // An empty assistant message is rejected by validate_request_messages
    // ("messages[N] is an assistant message with no content and no tool
    // calls"), which wedges the session: every retry re-sends the same empty
    // message and re-fails identically, so the only escape is a restart. The
    // fix substitutes a minimal non-empty placeholder ("(no output)") so the
    // assistant turn is recorded while keeping the conversation valid.
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    // First response: a bare Finish (null turn — no text, no tool calls).
    // Second response: normal text + finish (the follow-up "continue").
    let provider = Arc::new(MockProvider::sequence(vec![
        vec![LlmEvent::Finish {
            reason: FinishReason::Stop,
        }],
        vec![
            LlmEvent::TextDelta { text: "ok".into() },
            LlmEvent::Finish {
                reason: FinishReason::Stop,
            },
        ],
    ]));
    let agent = AgentLoop::new(
        test_config(provider, registry, workflow, sandbox.clone()),
        crate::project::Constitution::default(),
    );

    let (fanin_tx, _fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text("hi")];

    // Turn 1: the null turn. Must succeed (not Err) and must NOT leave an
    // empty assistant message in history.
    let outcome1 = agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await
        .expect("null turn should complete, not error");
    assert_eq!(outcome1.text, "");

    // No assistant message in history may have empty content (the bug would
    // leave exactly one: the null turn's empty assistant message).
    let empty_assistants: Vec<_> = messages
        .iter()
        .filter(|m| {
            m.role == Role::Assistant
                && m.tool_calls.is_empty()
                && m.content.as_text().trim().is_empty()
        })
        .collect();
    assert!(
        empty_assistants.is_empty(),
        "a null turn must not push an empty assistant message; found {}",
        empty_assistants.len()
    );

    // Turn 2: the follow-up "continue". Must succeed and return the text —
    // proving the session is not wedged by a poisoned history. (Without the
    // fix, the empty assistant message from turn 1 would be re-sent and
    // rejected by validate_request_messages on every retry.)
    messages.push(Message::user_text("continue"));
    let outcome2 = agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await
        .expect("follow-up turn must succeed (history not corrupted)");
    assert_eq!(outcome2.text, "ok");
}

#[tokio::test]
async fn repair_recovers_pre_existing_empty_assistant_message() {
    // Defensive recovery: a session already corrupted by the empty-assistant-
    // message bug (e.g. from a prior crash mid-turn, or a version predating
    // the null-turn fix) must recover on the next run_turn WITHOUT a restart.
    // repair_empty_assistant_messages, called at the top of run_turn,
    // substitutes a non-empty placeholder for any empty assistant message
    // already in history. Without it, the empty message would be re-sent and
    // rejected by validate_request_messages on every retry, wedging the
    // session — exactly the "only restarting the app works" symptom.
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());
    let provider = Arc::new(MockProvider::single(vec![
        LlmEvent::TextDelta { text: "ok".into() },
        LlmEvent::Finish {
            reason: FinishReason::Stop,
        },
    ]));
    let agent = AgentLoop::new(
        test_config(provider, registry, workflow, sandbox.clone()),
        crate::project::Constitution::default(),
    );

    let (fanin_tx, _fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    // Pre-seed a corrupted history: an empty assistant message (the exact
    // shape the bug produced) sits between two user messages.
    let mut messages = vec![
        Message::user_text("first"),
        Message::assistant_text(""),
        Message::user_text("second"),
    ];

    let outcome = agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await
        .expect("turn must succeed despite pre-existing empty assistant message");
    assert_eq!(outcome.text, "ok");

    // No assistant message in history may have empty content — the pre-seeded
    // empty one must have been repaired to the non-empty placeholder.
    let empty_assistants: Vec<_> = messages
        .iter()
        .filter(|m| {
            m.role == Role::Assistant
                && m.tool_calls.is_empty()
                && m.content.as_text().trim().is_empty()
        })
        .collect();
    assert!(
        empty_assistants.is_empty(),
        "pre-existing empty assistant message must be repaired; found {} still empty",
        empty_assistants.len()
    );
}

#[tokio::test]
async fn whitespace_only_stop_signal_does_not_corrupt_history() {
    // Regression (review Finding 1): the stop-signal path (Interrupt/Cancel/
    // Steer during streaming) pushes the partial assistant text when it's
    // non-empty. The guard previously used `!text.is_empty()` (no trim), so
    // whitespace-only partial output (a leading space/newline is common) was
    // pushed as a whitespace-only assistant message — which
    // validate_request_messages rejects on `s.trim().is_empty()`, the same
    // class of bug as the null-turn case. The fix mirrors the root-cause fix:
    // use `!text.trim().is_empty()` so whitespace-only partial output is
    // skipped (nothing meaningful was produced). This test drives a steer
    // mid-stream after a whitespace-only TextDelta and asserts no empty/
    // whitespace-only assistant message is left in history.
    use crate::runtime::AgentCommand;
    use tokio::sync::Notify;

    /// A provider that yields a whitespace-only chunk, then blocks on a Notify
    /// until the test signals it (so a steer can arrive mid-stream), then
    /// yields a Finish.
    struct PausingProvider {
        gate: Arc<Notify>,
        second_call: StdMutex<bool>,
        caps: Capabilities,
    }

    #[async_trait::async_trait]
    impl LlmClient for PausingProvider {
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
        fn kind(&self) -> ProviderKind {
            ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            "mock"
        }
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _tool_choice: Option<crate::provider::ToolChoice>,
        ) -> crate::error::Result<BoxStream<'_, LlmEvent>> {
            use futures::StreamExt;
            let is_second = {
                let mut g = self.second_call.lock().unwrap();
                let was = *g;
                *g = true;
                was
            };
            if is_second {
                // Follow-up turn — just stop.
                return Ok(Box::pin(futures::stream::iter(vec![LlmEvent::Finish {
                    reason: FinishReason::Stop,
                }])));
            }
            // First turn: yield a whitespace-only chunk, then pause until the
            // test signals (the steer arrives mid-stream), then finish.
            let gate = self.gate.clone();
            let stream = futures::stream::iter(vec![LlmEvent::TextDelta { text: "   ".into() }])
                .chain(futures::stream::once(async move {
                    gate.notified().await;
                    LlmEvent::Finish {
                        reason: FinishReason::Stop,
                    }
                }));
            Ok(Box::pin(stream))
        }
    }

    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    let gate = Arc::new(Notify::new());
    let provider: Arc<dyn LlmClient> = Arc::new(PausingProvider {
        gate: gate.clone(),
        second_call: StdMutex::new(false),
        caps: Capabilities::openai(),
    });
    let agent = Arc::new(AgentLoop::new(
        test_config(provider, registry, workflow, sandbox),
        crate::project::Constitution::default(),
    ));

    let (fanin_tx, _fanin_rx) = mpsc::channel(64);
    let (cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text("hello")];

    // Drive the turn in a task so we can send the steer mid-stream. The task
    // returns the messages vec so we can inspect history after the turn.
    let agent_clone = Arc::clone(&agent);
    let turn_handle = tokio::spawn(async move {
        let outcome = agent_clone
            .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
            .await
            .unwrap();
        (outcome, messages)
    });

    // Give the stream a moment to emit the whitespace chunk + reach the pause.
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    // Send a steer mid-stream (while the stream is paused on the Notify).
    cmd_tx
        .send(AgentCommand::Suggestion("do this instead".into()))
        .await
        .unwrap();
    // Give the select! a moment to process the steer (set soft_stop).
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    // Release the pause so the stream can finish — the soft-stop fires.
    gate.notify_one();

    let (outcome, messages) = turn_handle.await.unwrap();
    // The steer is carried as stop_reason (soft-stop).
    assert_eq!(
        outcome.stop_reason,
        Some(crate::agent::StopReason::Steer(vec![
            "do this instead".into()
        ])),
        "mid-turn steer must be carried as stop_reason"
    );

    // No assistant message in history may have empty/whitespace-only content
    // (the bug would leave a whitespace-only assistant message). The
    // stop-signal path must skip pushing when text is whitespace-only.
    let empty_assistants: Vec<_> = messages
        .iter()
        .filter(|m| {
            m.role == Role::Assistant
                && m.tool_calls.is_empty()
                && m.content.as_text().trim().is_empty()
        })
        .collect();
    assert!(
        empty_assistants.is_empty(),
        "a whitespace-only stop-signal turn must not push an empty/whitespace-only \
         assistant message; found {}",
        empty_assistants.len()
    );
}

#[tokio::test]
async fn context_usage_event_carries_cached_token_count() {
    // The token count is now computed once per loop iteration and reused
    // for both the summarization threshold check and the ContextUsage
    // event (instead of counting twice). Verify the emitted ContextUsage
    // `used` matches the cached count -- messages as passed in PLUS the
    // tools-schema block the provider bills in usage.prompt_tokens -- not
    // a stale or double value.
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());
    let provider = Arc::new(MockProvider::single(vec![
        LlmEvent::TextDelta { text: "ok".into() },
        LlmEvent::Finish {
            reason: FinishReason::Stop,
        },
    ]));
    let agent = AgentLoop::new(
        test_config(provider, registry.clone(), workflow.clone(), sandbox.clone()),
        crate::project::Constitution::default(),
    );

    let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text("hello world this is a test")];

    // The ContextUsage event is emitted at the top of the loop iteration,
    // before the system prompt is prepended -- so `used` reflects the
    // messages as passed in plus the tools-schema overhead, computed from
    // the same schemas the loop sends (registry + workflow filter, with
    // the plan-mutation pruning the loop applies). Capture the expectation
    // before run_turn mutates `messages`.
    let expected_messages = context::ContextManager::count_tokens(&messages) as u32;
    let mut schemas =
        registry.schemas(&Capabilities::openai(), &workflow.lock().await.allowed_tools());
    if !agent.plan_mutations_allowed() {
        schemas.retain(|s| {
            !matches!(
                s.name.as_str(),
                "create_plan" | "update_plan" | "complete_step" | "abandon_plan" | "finish"
            )
        });
    }
    let expected_tools = crate::provider::estimate_tools_tokens(&schemas) as u32;
    let expected_used = expected_messages + expected_tools;

    let _outcome = agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await
        .unwrap();

    // Drain events and find the ContextUsage.
    let mut usage_used = None;
    let mut usage_max = None;
    while let Ok(Some((_id, event))) =
        tokio::time::timeout(std::time::Duration::from_millis(200), fanin_rx.recv()).await
    {
        if let AgentEvent::ContextUsage { used, max, .. } = event {
            usage_used = Some(used);
            usage_max = Some(max);
        }
    }
    let used = usage_used.expect("a ContextUsage event should be emitted");
    let max = usage_max.expect("ContextUsage event should carry max");
    // The count should match the cached count, computed once and reused --
    // messages at the start of the iteration + the tools-schema block
    // (instead of counting twice as before, or missing the schema block).
    assert_eq!(
        used, expected_used,
        "ContextUsage used ({used}) should match the cached count_tokens + \
         tools-schema block ({expected_used})"
    );
    assert!(expected_tools > 0, "the test registry must carry schemas");
    assert_eq!(max, 128_000);
}

#[tokio::test]
async fn context_usage_estimate_includes_tools_schema_overhead() {
    // Regression (2026-12-23, review finding on plan addf3727): the
    // tools-schema overhead was recorded AFTER the iteration's only
    // accounting reads and every loop-back's reset() zeroed it, so
    // `tools_tokens` was dead -- the top-of-loop ContextUsage estimate and
    // the summarize trigger both saw a message-only count. The FIRST
    // emission of the turn must already carry the schema block the request
    // will send: used == per-role buckets + estimate_tools_tokens(schemas).
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());
    let provider = Arc::new(MockProvider::single(vec![
        LlmEvent::TextDelta { text: "ok".into() },
        LlmEvent::Finish {
            reason: FinishReason::Stop,
        },
    ]));
    let agent = AgentLoop::new(
        test_config(provider, registry.clone(), workflow.clone(), sandbox.clone()),
        crate::project::Constitution::default(),
    );

    let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text("hello world this is a test")];

    // Mirror the loop's schema computation exactly: capabilities of the
    // default provider (MockProvider reports Capabilities::openai()), the
    // workflow's allowed-tools filter, and the plan-mutation pruning.
    let mut schemas =
        registry.schemas(&Capabilities::openai(), &workflow.lock().await.allowed_tools());
    if !agent.plan_mutations_allowed() {
        schemas.retain(|s| {
            !matches!(
                s.name.as_str(),
                "create_plan" | "update_plan" | "complete_step" | "abandon_plan" | "finish"
            )
        });
    }
    let expected_tools = crate::provider::estimate_tools_tokens(&schemas) as u32;
    assert!(expected_tools > 0, "the test registry must carry schemas");

    let _outcome = agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await
        .unwrap();

    // The FIRST ContextUsage event of the turn is the top-of-loop local
    // estimate; the mock response carries no Usage event, so it is the only
    // one. Its `used` must decompose exactly into the per-role buckets
    // (message-only, by design) plus the schema block.
    let mut first: Option<(u32, crate::runtime::ContextBreakdown)> = None;
    while let Ok(Some((_id, event))) =
        tokio::time::timeout(std::time::Duration::from_millis(200), fanin_rx.recv()).await
    {
        if let AgentEvent::ContextUsage {
            used,
            breakdown,
            ..
        } = event
        {
            if first.is_none() {
                first = Some((used, breakdown));
            }
        }
    }
    let (used, breakdown) = first.expect("a ContextUsage event should be emitted");
    let bucket_sum = breakdown.system + breakdown.user + breakdown.assistant + breakdown.tool;
    assert_eq!(
        used,
        bucket_sum + expected_tools,
        "top-of-loop estimate must include the tools-schema block \
         (used={used}, buckets={bucket_sum}, expected_tools={expected_tools})"
    );
}

#[tokio::test]
async fn context_usage_reanchors_to_provider_prompt_tokens() {
    // Regression (2026-12-23, backlog da4fc87d): the ctx bar held the local
    // estimate (which misses reasoning_content + the tools array) while the
    // provider counted far more — "ctx 54.5k" vs a 97.1k provider-visible
    // prompt. After each response the bar must re-anchor to the
    // provider-reported prompt_tokens (the exact wire basis); the local
    // estimate is only the pre-first-response fallback.
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());
    let provider = Arc::new(MockProvider::single(vec![
        LlmEvent::TextDelta { text: "ok".into() },
        LlmEvent::Usage {
            prompt_tokens: 4_200,
            completion_tokens: 20,
            reasoning_tokens: 0,
            cached_tokens: 0,
            ttft_ms: Some(100),
            generation_ms: Some(300),
        },
        LlmEvent::Finish {
            reason: FinishReason::Stop,
        },
    ]));
    let agent = AgentLoop::new(
        test_config(provider, registry, workflow, sandbox.clone()),
        crate::project::Constitution::default(),
    );

    let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text("hello world this is a test")];

    let _outcome = agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await
        .unwrap();

    // Collect every ContextUsage event; the LAST one is the post-response
    // provider anchor (the top-of-iteration local estimate comes first).
    let mut context_usages: Vec<(u32, u32)> = Vec::new();
    while let Ok(Some((_id, event))) =
        tokio::time::timeout(std::time::Duration::from_millis(200), fanin_rx.recv()).await
    {
        if let AgentEvent::ContextUsage { used, max, .. } = event {
            context_usages.push((used, max));
        }
    }
    let &(last_used, last_max) = context_usages
        .last()
        .expect("at least one ContextUsage event should be emitted");
    assert_eq!(
        last_used, 4_200,
        "the post-response ContextUsage must re-anchor `used` to the \
         provider-reported prompt_tokens (events: {context_usages:?})"
    );
    assert_eq!(last_max, 128_000);
}

#[tokio::test]
async fn full_turn_with_tool_call() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("test.txt"), "file contents").unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    // First response: a tool call to file_read.
    // Second response: text finishing the turn.
    let provider = Arc::new(MockProvider::sequence(vec![
        vec![
            LlmEvent::ToolCallStart {
                index: 0,
                id: "call_1".into(),
                name: "file_read".into(),
            },
            LlmEvent::ToolCallArgumentDelta {
                index: 0,
                fragment: r#"{"path":"test.txt"}"#.into(),
            },
            LlmEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ],
        vec![
            LlmEvent::TextDelta {
                text: "Done reading.".into(),
            },
            LlmEvent::Finish {
                reason: FinishReason::Stop,
            },
        ],
    ]));

    let agent = AgentLoop::new(
        test_config(provider, registry, workflow, sandbox.clone()),
        crate::project::Constitution::default(),
    );

    let (fanin_tx, _fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text("read test.txt")];

    let outcome = agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await
        .unwrap();

    // The conversation should contain a tool result message with the file contents.
    let tool_msg = messages
        .iter()
        .find(|m| m.role == Role::Tool)
        .expect("should have a tool message");
    assert!(tool_msg.content.as_text().contains("file contents"));
    assert_eq!(outcome.text, "Done reading.");
}

#[tokio::test]
async fn phase_events_cycle_across_requests_and_tools() {
    // The inflight-bar contract: each request cycle emits Sending → Waiting →
    // Streaming, and a tool-call response adds RunningTools before the next
    // request's Sending. A turn with one tool call + one text response must
    // produce exactly this phase sequence.
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("test.txt"), "file contents").unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    let provider = Arc::new(MockProvider::sequence(vec![
        vec![
            LlmEvent::ToolCallStart {
                index: 0,
                id: "call_1".into(),
                name: "file_read".into(),
            },
            LlmEvent::ToolCallArgumentDelta {
                index: 0,
                fragment: r#"{"path":"test.txt"}"#.into(),
            },
            LlmEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ],
        vec![
            LlmEvent::TextDelta {
                text: "Done reading.".into(),
            },
            LlmEvent::Finish {
                reason: FinishReason::Stop,
            },
        ],
    ]));

    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider: provider.clone(),
            tools: registry,
            workflow: workflow,
            sandbox: sandbox.clone(),
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(128_000, 0.5),
            memory: None,
            vision: None,
        },
        crate::project::Constitution::default(),
    );

    let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text("read test.txt")];

    agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await
        .unwrap();

    // Drain the fanned-in events and keep the phase transitions in order.
    let mut phases = Vec::new();
    while let Ok((_id, event)) = fanin_rx.try_recv() {
        if let AgentEvent::Phase { phase } = event {
            phases.push(phase);
        }
    }
    assert_eq!(
        phases,
        vec![
            PhaseKind::Sending,
            PhaseKind::Waiting,
            PhaseKind::Streaming,
            PhaseKind::RunningTools,
            PhaseKind::Sending,
            PhaseKind::Waiting,
            PhaseKind::Streaming,
        ],
        "the phase cycle must mirror sending → waiting → thinking → running tools"
    );

    // The tools phase was measured once (for the one tool batch) and
    // reported to the provider for trace attribution. The exact value is
    // environment-dependent (a fast tool call may measure 0ms) — assert it
    // exists and is a sane sub-5s bound.
    let tools_phases = provider.take_tools_phases();
    assert_eq!(tools_phases.len(), 1, "one tool batch → one tools phase");
    assert!(
        tools_phases[0] < 5_000,
        "unexpected duration: {}",
        tools_phases[0]
    );
}

#[tokio::test]
async fn phase_cycle_reasoning_then_answering() {
    // Regression (2026-08-22): reasoning deltas must flip the inflight bar to
    // the REASONING phase, distinct from the answer's STREAMING phase — the
    // split makes thinking visible instead of a silent "waiting…" stall
    // while the model thinks (Ollama `reasoning` / DeepSeek thinking mode).
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    let provider = Arc::new(MockProvider::sequence(vec![vec![
        LlmEvent::ReasoningDelta {
            text: "thinking…".into(),
        },
        LlmEvent::TextDelta {
            text: "Answer.".into(),
        },
        LlmEvent::Finish {
            reason: FinishReason::Stop,
        },
    ]]));

    let agent = AgentLoop::new(
        test_config(provider, registry, workflow, sandbox.clone()),
        crate::project::Constitution::default(),
    );

    let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text("hi")];

    agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await
        .unwrap();

    let mut phases = Vec::new();
    while let Ok((_id, event)) = fanin_rx.try_recv() {
        if let AgentEvent::Phase { phase } = event {
            phases.push(phase);
        }
    }
    assert_eq!(
        phases,
        vec![
            PhaseKind::Sending,
            PhaseKind::Waiting,
            PhaseKind::Reasoning,
            PhaseKind::Streaming,
        ],
        "reasoning deltas must flip to Reasoning before the answer's Streaming"
    );
}

/// A stream that yields a text delta, goes silent for 11 s (virtual time
/// under a paused clock), then yields another delta and finishes — used to
/// verify that a mid-stream stall does NOT change the inflight phase (the
/// status bar matches the trace graphs: stall is part of generate).
struct StallingStream {
    step: u8,
    sleep: std::pin::Pin<Box<tokio::time::Sleep>>,
}

impl futures::Stream for StallingStream {
    type Item = LlmEvent;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        match self.step {
            0 => {
                self.step = 1;
                std::task::Poll::Ready(Some(LlmEvent::TextDelta { text: "a".into() }))
            }
            1 => match std::future::Future::poll(self.sleep.as_mut(), cx) {
                std::task::Poll::Pending => std::task::Poll::Pending,
                std::task::Poll::Ready(()) => {
                    self.step = 2;
                    std::task::Poll::Ready(Some(LlmEvent::TextDelta { text: "b".into() }))
                }
            },
            2 => {
                self.step = 3;
                std::task::Poll::Ready(Some(LlmEvent::Finish {
                    reason: FinishReason::Stop,
                }))
            }
            _ => std::task::Poll::Ready(None),
        }
    }
}

/// A mock provider whose `complete` returns a [`StallingStream`] — one
/// delta, an 11 s byte-silence stall, a second delta, then Finish.
struct StallingProvider {
    caps: Capabilities,
}

#[async_trait]
impl LlmClient for StallingProvider {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
    fn kind(&self) -> ProviderKind {
        ProviderKind::OpenAI
    }
    fn model(&self) -> &str {
        "stall-mock"
    }
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _tool_choice: Option<crate::provider::ToolChoice>,
    ) -> crate::error::Result<BoxStream<'_, LlmEvent>> {
        Ok(Box::pin(StallingStream {
            step: 0,
            sleep: Box::pin(tokio::time::sleep(std::time::Duration::from_secs(11))),
        }))
    }
}

#[tokio::test(start_paused = true)]
async fn mid_stream_stall_keeps_streaming_phase() {
    // Regression (status bar matches the trace graph): a mid-stream stall
    // (the connection open but nothing arriving) is byte-silence INSIDE the
    // generate window — the trace graphs count stall_ms as part of
    // generation (decision f2b62d85), so the live status bar matches and
    // keeps showing "answering…"/"reasoning…" through the silence. The 11 s
    // silence must emit NO phase event: no Waiting flip, no re-emitted
    // Streaming. Paused clock: the 11 s silence auto-advances instantly.
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());
    let provider: Arc<dyn LlmClient> = Arc::new(StallingProvider {
        caps: Capabilities::openai(),
    });
    let agent = AgentLoop::new(
        test_config(provider, registry, workflow, sandbox.clone()),
        crate::project::Constitution::default(),
    );

    let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text("hi")];

    agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await
        .unwrap();

    let mut phases = Vec::new();
    while let Ok((_id, event)) = fanin_rx.try_recv() {
        if let AgentEvent::Phase { phase } = event {
            phases.push(phase);
        }
    }
    assert_eq!(
        phases,
        vec![
            PhaseKind::Sending,
            PhaseKind::Waiting,
            PhaseKind::Streaming,
        ],
        "the 11 s silence must not change the phase — stall is part of generate, not waiting"
    );
}

#[tokio::test]
async fn no_tool_calls_turn_skips_running_tools_phase() {
    // A text-only turn never emits RunningTools — the phase cycle ends at
    // Streaming and the turn finishes.
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    let provider = Arc::new(MockProvider::single(vec![
        LlmEvent::TextDelta {
            text: "Hello!".into(),
        },
        LlmEvent::Finish {
            reason: FinishReason::Stop,
        },
    ]));
    let agent = AgentLoop::new(
        test_config(provider, registry, workflow, sandbox.clone()),
        crate::project::Constitution::default(),
    );

    let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text("hi")];
    agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await
        .unwrap();

    let mut phases = Vec::new();
    while let Ok((_id, event)) = fanin_rx.try_recv() {
        if let AgentEvent::Phase { phase } = event {
            phases.push(phase);
        }
    }
    assert_eq!(
        phases,
        vec![PhaseKind::Sending, PhaseKind::Waiting, PhaseKind::Streaming],
        "text-only turns must not emit RunningTools"
    );
}

#[tokio::test]
async fn error_recovery_malformed_json() {
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    // Tool call with malformed JSON arguments, then a stop.
    let provider = Arc::new(MockProvider::sequence(vec![
        vec![
            LlmEvent::ToolCallStart {
                index: 0,
                id: "call_1".into(),
                name: "file_read".into(),
            },
            LlmEvent::ToolCallArgumentDelta {
                index: 0,
                fragment: "{bad json}".into(),
            },
            LlmEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ],
        vec![LlmEvent::Finish {
            reason: FinishReason::Stop,
        }],
    ]));

    let agent = AgentLoop::new(
        test_config(provider, registry, workflow, sandbox.clone()),
        crate::project::Constitution::default(),
    );

    let (fanin_tx, _fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text("read a file")];

    let _outcome = agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await;

    // Malformed JSON is now caught before tool execution and fed back
    // as a retry message. The tool message should contain the retry hint.
    let tool_msg = messages
        .iter()
        .find(|m| m.role == Role::Tool)
        .expect("should have a tool message");
    assert!(
        tool_msg.content.as_text().contains("malformed")
            || tool_msg.content.as_text().contains("truncated"),
        "expected malformed/truncated error, got: {}",
        tool_msg.content.as_text()
    );
    assert!(
        tool_msg
            .content
            .as_text()
            .contains("rewrite the COMPLETE call"),
        "malformed-JSON feedback should teach rewrite-complete (content first), got: {}",
        tool_msg.content.as_text()
    );
    assert!(
        tool_msg
            .content
            .as_text()
            .contains("never resend the broken call unchanged"),
        "malformed-JSON feedback should forbid resending the broken call, got: {}",
        tool_msg.content.as_text()
    );

    // The re-injected assistant message must carry VALID tool-call
    // arguments Î“Ã‡Ã¶ otherwise the broken arguments get serialized into the
    // next request body and the gateway rejects it (400 "Unterminated
    // string") when it parses the `arguments` field. The malformed
    // arguments are sanitized to "{}".
    let assistant_with_calls = messages
        .iter()
        .find(|m| m.role == Role::Assistant && !m.tool_calls.is_empty())
        .expect("should have an assistant message with tool calls");
    for tc in &assistant_with_calls.tool_calls {
        assert!(
            serde_json::from_str::<serde_json::Value>(&tc.arguments).is_ok(),
            "re-injected tool-call arguments must be valid JSON, got: {:?}",
            tc.arguments
        );
    }
}

#[tokio::test]
async fn unknown_tool_error_recovery() {
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    let provider = Arc::new(MockProvider::sequence(vec![
        vec![
            LlmEvent::ToolCallStart {
                index: 0,
                id: "call_1".into(),
                name: "nonexistent_tool".into(),
            },
            LlmEvent::ToolCallArgumentDelta {
                index: 0,
                fragment: "{}".into(),
            },
            LlmEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ],
        vec![LlmEvent::Finish {
            reason: FinishReason::Stop,
        }],
    ]));

    let agent = AgentLoop::new(
        test_config(provider, registry, workflow, sandbox.clone()),
        crate::project::Constitution::default(),
    );

    let (fanin_tx, _fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text("do something")];

    let _outcome = agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await;

    let tool_msg = messages
        .iter()
        .find(|m| m.role == Role::Tool)
        .expect("should have a tool message");
    assert!(tool_msg.content.as_text().contains("unknown tool"));
    // Structured-error regression: a FAILED tool result must be fed back as
    // a parseable `[tool error]` block (the LLM parses it and re-issues the
    // call with corrected arguments), never as an unprefixed bare output.
    assert!(
        tool_msg.content.as_text().starts_with("[tool error]"),
        "failed tool results must carry the [tool error] marker, got: {}",
        tool_msg.content.as_text()
    );
}

#[tokio::test]
async fn successful_tool_results_are_not_wrapped_in_tool_error_marker() {
    // The counterpart guard: SUCCESSFUL tool results must flow back verbatim
    // (no `[tool error]` prefix) so the model never misreads a success as a
    // failure.
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    let provider = Arc::new(MockProvider::sequence(vec![
        vec![
            LlmEvent::ToolCallStart {
                index: 0,
                id: "call_1".into(),
                name: "memory_search".into(),
            },
            LlmEvent::ToolCallArgumentDelta {
                index: 0,
                fragment: "{\"query\": \"needle\"}".into(),
            },
            LlmEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ],
        vec![LlmEvent::Finish {
            reason: FinishReason::Stop,
        }],
    ]));

    let agent = AgentLoop::new(
        test_config(provider, registry, workflow, sandbox.clone()),
        crate::project::Constitution::default(),
    );

    let (fanin_tx, _fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text("find something")];

    let _outcome = agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await;

    let tool_msg = messages
        .iter()
        .find(|m| m.role == Role::Tool)
        .expect("should have a tool message");
    assert!(
        !tool_msg.content.as_text().starts_with("[tool error]"),
        "successful results must not carry the [tool error] marker, got: {}",
        tool_msg.content.as_text()
    );
}

#[tokio::test]
async fn max_retries_aborts_after_consecutive_tool_errors() {
    // A tool that runs but FAILS (file_read on a non-existent path) returns
    // success:false. After MAX_RETRIES (3) consecutive such failures the agent
    // must emit a final Error and stop — it must not loop forever. (Bad-JSON
    // errors use the separate MAX_BAD_JSON_RETRIES cap, tested below.)
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    // Each response emits a file_read call with VALID JSON args for a path
    // that does not exist — the tool executes and returns success:false.
    let fail_call = |n: usize| {
        vec![
            LlmEvent::ToolCallStart {
                index: 0,
                id: format!("call_{n}"),
                name: "file_read".into(),
            },
            LlmEvent::ToolCallArgumentDelta {
                index: 0,
                fragment: r#"{"path":"does_not_exist.txt"}"#.into(),
            },
            LlmEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ]
    };
    let provider = Arc::new(MockProvider::sequence(vec![
        fail_call(1),
        fail_call(2),
        fail_call(3),
        fail_call(4),
    ]));

    let agent = AgentLoop::new(
        test_config(provider, registry, workflow, sandbox.clone()),
        crate::project::Constitution::default(),
    );

    let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text("read a file")];

    let outcome = agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await
        .unwrap();

    // The turn should have ended (not looped forever).
    // Drain events: final Error is required; trailing Finished is forbidden
    // (Quality C1 — Run-All must not see Error then Finished as two resolutions).
    let mut saw_final_error = false;
    let mut saw_finished = false;
    while let Ok(Some((_id, event))) =
        tokio::time::timeout(std::time::Duration::from_millis(200), fanin_rx.recv()).await
    {
        match event {
            AgentEvent::Error {
                retrying: false, ..
            } => saw_final_error = true,
            AgentEvent::Finished { .. } => saw_finished = true,
            _ => {}
        }
    }
    assert!(
        saw_final_error,
        "the agent should emit a final Error after hitting MAX_RETRIES"
    );
    assert!(
        !saw_finished,
        "MAX_RETRIES abort must not also emit Finished (terminal exclusivity)"
    );
    // The tool-execution-error abort fires AFTER the per-call loop feeds each
    // result back (the cap check is after the batch). So all 3 failing calls
    // push a tool message, then the 3rd trips the cap.
    let tool_msgs: Vec<_> = messages.iter().filter(|m| m.role == Role::Tool).collect();
    assert_eq!(
        tool_msgs.len(),
        3,
        "expected exactly 3 tool-error messages before the cap fired, got {}",
        tool_msgs.len()
    );
    let _ = outcome;
}

#[tokio::test]
async fn review_report_failures_abort_with_distinct_error() {
    // Backlog 5b46674d: write_review_report failures get DEDICATED retry
    // accounting — unlike tool_error_count (reset by ANY successful tool
    // call), the report-failure counter only resets on a successful
    // write_review_report. A reviewer that keeps failing verdict validation
    // (interleaved with successful reads) must abort with a DISTINCT final
    // error naming the review report, not finish silently report-less
    // (live-observed 2026-12-30: reviewers exhausted their turns and died
    // report-less after repeated verdict rejections).
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("exists.txt"), "contents").unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    // The reviewer allow-list: write_review_report + file_read dispatchable
    // (write_review_report is only visible under the Reviewer filter).
    workflow
        .lock()
        .await
        .set_reviewer_allowlist(Some(vec![
            "write_review_report".into(),
            "file_read".into(),
        ]));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(FileReadTool::new((*sandbox).clone())));
    registry.register(Box::new(
        crate::tool::agent::write_review_report::WriteReviewReportTool::new(
            dir.path().join("reviews"),
        ),
    ));
    let registry = Arc::new(registry);
    // A failing write_review_report call (verdict-less content → validation
    // rejection) and a SUCCESSFUL file_read (the file exists) — the
    // interleaved success must NOT reset the report-failure counter.
    let fail_report = |n: usize| {
        vec![
            LlmEvent::ToolCallStart {
                index: 0,
                id: format!("call_{n}"),
                name: "write_review_report".into(),
            },
            LlmEvent::ToolCallArgumentDelta {
                index: 0,
                fragment: r#"{"path":"r.md","content":"no verdict"}"#.into(),
            },
            LlmEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ]
    };
    let ok_read = |n: usize| {
        vec![
            LlmEvent::ToolCallStart {
                index: 0,
                id: format!("call_{n}"),
                name: "file_read".into(),
            },
            LlmEvent::ToolCallArgumentDelta {
                index: 0,
                fragment: r#"{"path":"exists.txt"}"#.into(),
            },
            LlmEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ]
    };
    let provider = Arc::new(MockProvider::sequence(vec![
        fail_report(1),
        ok_read(2),
        fail_report(3),
        ok_read(4),
        fail_report(5),
    ]));
    let agent = AgentLoop::new(
        test_config(provider, registry, workflow, sandbox.clone()),
        crate::project::Constitution::default(),
    );

    let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text("review it")];

    let _outcome = agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await
        .unwrap();

    // The distinct abort: a final Error naming the review report, and no
    // Finished after it (terminal exclusivity).
    let mut final_error = None;
    let mut saw_finished = false;
    while let Ok(Some((_id, event))) =
        tokio::time::timeout(std::time::Duration::from_millis(200), fanin_rx.recv()).await
    {
        match event {
            AgentEvent::Error {
                error, retrying: false, ..
            } => final_error = Some(error),
            AgentEvent::Finished { .. } => saw_finished = true,
            _ => {}
        }
    }
    let err = final_error.expect("the reviewer must abort with a final Error");
    assert!(
        err.contains("review report"),
        "the abort must name the review report distinctly: {err}"
    );
    assert!(
        !saw_finished,
        "the report-failure abort must not also emit Finished (terminal exclusivity)"
    );
    // All five calls fed back — the interleaved reads must not reset the
    // report-failure counter (the 5th call trips the cap).
    let tool_msgs: Vec<_> = messages.iter().filter(|m| m.role == Role::Tool).collect();
    assert_eq!(
        tool_msgs.len(),
        5,
        "expected 5 tool messages before the cap fired, got {}",
        tool_msgs.len()
    );
}

#[tokio::test]
async fn bad_json_retries_continue_past_three() {
    // Bad-JSON errors (LLM-produced malformed tool-call arguments) must NOT
    // count toward MAX_RETRIES — the model can recover by emitting valid JSON.
    // Four bad-JSON calls (past the old 3-strike cap) then a normal stop: the
    // agent must continue and end normally, NOT abort.
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    let bad_call = || {
        vec![
            LlmEvent::ToolCallStart {
                index: 0,
                id: "call_1".into(),
                name: "file_read".into(),
            },
            LlmEvent::ToolCallArgumentDelta {
                index: 0,
                fragment: "{bad json}".into(),
            },
            LlmEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ]
    };
    let provider = Arc::new(MockProvider::sequence(vec![
        bad_call(),
        bad_call(),
        bad_call(),
        bad_call(),
        vec![LlmEvent::Finish {
            reason: FinishReason::Stop,
        }],
    ]));

    let agent = AgentLoop::new(
        test_config(provider, registry, workflow, sandbox.clone()),
        crate::project::Constitution::default(),
    );

    let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text("read a file")];

    let outcome = agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await
        .unwrap();

    // No final Error — the model continued past 3 bad-JSON errors and stopped
    // normally on the 5th response.
    let mut saw_final_error = false;
    while let Ok(Some((_id, event))) =
        tokio::time::timeout(std::time::Duration::from_millis(200), fanin_rx.recv()).await
    {
        if let AgentEvent::Error {
            retrying: false, ..
        } = event
        {
            saw_final_error = true;
        }
    }
    assert!(
        !saw_final_error,
        "bad-JSON errors must not trip MAX_RETRIES; the agent should have continued"
    );
    // Each bad-JSON call feeds back a tool message (4 total); the 5th response
    // is a plain stop (no tool message).
    let tool_msgs: Vec<_> = messages.iter().filter(|m| m.role == Role::Tool).collect();
    assert_eq!(
        tool_msgs.len(),
        4,
        "expected 4 bad-JSON tool messages (one per retry), got {}",
        tool_msgs.len()
    );
    let _ = outcome;
}

#[tokio::test]
async fn bad_json_aborts_at_higher_cap() {
    // After MAX_BAD_JSON_RETRIES (8) consecutive bad-JSON errors, the agent
    // must abort — a safeguard against a pathological model that loops forever
    // on broken arguments.
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    let bad_call = || {
        vec![
            LlmEvent::ToolCallStart {
                index: 0,
                id: "call_1".into(),
                name: "file_read".into(),
            },
            LlmEvent::ToolCallArgumentDelta {
                index: 0,
                fragment: "{bad json}".into(),
            },
            LlmEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ]
    };
    let sequences: Vec<Vec<LlmEvent>> = (0..8).map(|_| bad_call()).collect();
    let provider = Arc::new(MockProvider::sequence(sequences));

    let agent = AgentLoop::new(
        test_config(provider, registry, workflow, sandbox.clone()),
        crate::project::Constitution::default(),
    );

    let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text("read a file")];

    // Drain the bounded fan-in channel CONCURRENTLY. The turn emits more
    // events than it used to — one synthetic ToolResult per bad-JSON retry
    // (backlog 63cbc20f, so the announced card ends) — and across 8 retry
    // iterations those extra sends overflow the 64-slot buffer mid-turn,
    // deadlocking run_turn's `.send().await` when nothing drains until it
    // returns. Collecting while running keeps the same assertions.
    let collector = tokio::spawn(async move {
        let mut events = Vec::new();
        while let Some((_id, event)) = fanin_rx.recv().await {
            events.push(event);
        }
        events
    });

    let outcome = agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await
        .unwrap();

    // Drop our sender so the collector ends and hands back the events.
    drop(fanin_tx);
    let events = collector.await.unwrap();
    let saw_final_error = events.iter().any(|e| {
        matches!(
            e,
            AgentEvent::Error {
                retrying: false,
                ..
            }
        )
    });
    assert!(
        saw_final_error,
        "the agent should abort after MAX_BAD_JSON_RETRIES consecutive bad-JSON errors"
    );
    // Review F2 (2026-09-18, round 2): pin BOTH bad-JSON arms' per-call
    // synthetic ToolResults by their DISTINCT payloads — the retry arm's
    // ends in "; retrying", the cap arm's is the bare "not run". An any()
    // over call id alone is satisfied by the 7 retry-arm emissions (same
    // id, fired before the cap), so a regression deleting either arm's
    // send loop must be caught by count, not by presence.
    let retry_arm_results = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::ToolResult { tool_call_id, result }
                    if tool_call_id == "call_1"
                        && result.output.contains("not run; retrying")
            )
        })
        .count();
    let cap_arm_results = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::ToolResult { tool_call_id, result }
                    if tool_call_id == "call_1"
                        && result.output.contains("not run")
                        && !result.output.contains("; retrying")
            )
        })
        .count();
    assert_eq!(
        retry_arm_results, 7,
        "the retry arm must end the announced card once per retry iteration (backlog 63cbc20f)"
    );
    assert_eq!(
        cap_arm_results, 1,
        "the bad-JSON cap arm must end the announced card exactly once (backlog 63cbc20f)"
    );
    // Calls 1-7 feed back a tool message; the 8th trips the cap (no message).
    let tool_msgs: Vec<_> = messages.iter().filter(|m| m.role == Role::Tool).collect();
    assert_eq!(
        tool_msgs.len(),
        7,
        "expected 7 bad-JSON tool messages before the cap fired, got {}",
        tool_msgs.len()
    );
    let _ = outcome;
}

#[tokio::test]
async fn create_plan_never_needs_approval() {
    // create_plan is AutoRun Î“Ã‡Ã¶ even in ApproveEachAction mode, it must run
    // without an approval prompt (otherwise the agent is stuck unable to act).
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    let provider = Arc::new(MockProvider::sequence(vec![
        vec![
            LlmEvent::ToolCallStart {
                index: 0,
                id: "call_1".into(),
                name: "create_plan".into(),
            },
            LlmEvent::ToolCallArgumentDelta {
                index: 0,
                fragment: r#"{"title":"T","goal":"G","context":"Dispatch fixture context: edit src/widget.rs and verify with cargo test.","steps":["edit src/widget.rs"]}"#.into(),
            },
            LlmEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ],
        vec![LlmEvent::Finish {
            reason: FinishReason::Stop,
        }],
    ]));

    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider: provider,
            tools: registry,
            workflow: workflow.clone(),
            sandbox: sandbox.clone(),
            safety_mode: SafetyMode::ApproveEachAction,
            context_manager: context::ContextManager::new(128_000, 0.5),
            memory: None,
            vision: None,
        },
        crate::project::Constitution::default(),
    );

    let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text("make a plan")];

    let _outcome = agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await
        .unwrap();

    // Drain events Î“Ã‡Ã¶ none should be an ApprovalRequest.
    let mut saw_approval = false;
    while let Ok(Some((_id, event))) =
        tokio::time::timeout(std::time::Duration::from_millis(100), fanin_rx.recv()).await
    {
        if matches!(event, AgentEvent::ApprovalRequest { .. }) {
            saw_approval = true;
        }
    }
    assert!(!saw_approval, "create_plan must not request approval");

    // The plan was created and the workflow transitioned to Executing.
    let wf = workflow.lock().await;
    assert_eq!(wf.state(), crate::workflow::WorkflowState::Executing);
}

#[tokio::test]
async fn git_merge_prompts_even_in_autonomous_mode() {
    // THE guarantee the agent-driven merge skill depends on: even under
    // SafetyMode::Autonomous (which skips approval for every other tool), a
    // core operation like `git merge` (via the git tool's never_auto_for)
    // must STILL surface an approval prompt and must NOT execute silently.
    // Drive a `git merge` call through run_turn in Autonomous mode and assert
    // an ApprovalRequest is emitted.
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    // Executing is required so the workflow ToolFilter allows `git` at
    // dispatch; this test is about never_auto_for, not Planning denial.
    {
        let mut wf = workflow.lock().await;
        wf.create_plan("merge", "g", "c", vec!["merge feat".into()])
            .unwrap();
    }
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(GitTool::new(dir.path())));
    let registry = Arc::new(registry);

    let provider = Arc::new(MockProvider::sequence(vec![
        vec![
            LlmEvent::ToolCallStart {
                index: 0,
                id: "call_merge".into(),
                name: "git".into(),
            },
            LlmEvent::ToolCallArgumentDelta {
                index: 0,
                fragment: r#"{"subcommand":"merge","branch":"feat"}"#.into(),
            },
            LlmEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ],
        vec![LlmEvent::Finish {
            reason: FinishReason::Stop,
        }],
    ]));

    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider: provider,
            tools: registry,
            workflow: workflow.clone(),
            sandbox: sandbox.clone(),
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(128_000, 0.5),
            memory: None,
            vision: None,
        },
        crate::project::Constitution::default(),
    );

    let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text("merge feat into main")];

    // Run the turn in a task so we can answer the approval request from here.
    let fanin_tx2 = fanin_tx.clone();
    let turn = tokio::spawn(async move {
        agent
            .run_turn(&mut messages, &fanin_tx2, 1, &mut cmd_rx, None)
            .await
    });

    // Wait for the ApprovalRequest and deny it (the gate must have fired).
    let mut saw_approval = false;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(300), fanin_rx.recv()).await {
            Ok(Some((
                _id,
                AgentEvent::ApprovalRequest {
                    tool_name,
                    core_operation,
                    responder,
                    ..
                },
            ))) => {
                assert_eq!(tool_name, "git");
                assert!(core_operation, "git merge must be flagged a core operation");
                saw_approval = true;
                let _ = responder.send(crate::runtime::Approval::Deny);
                break;
            }
            Ok(Some(_)) => continue,
            _ => break,
        }
    }
    assert!(
        saw_approval,
        "git merge must surface an approval prompt even in Autonomous mode"
    );

    // The turn completes (the denial feeds back as a tool error) without the
    // merge ever executing.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), turn).await;
}

#[tokio::test]
async fn run_turn_records_session_id_on_tool_events() {
    // Verify that passing a real session_id into run_turn causes the
    // working-memory tool event to be tagged with that session_id Î“Ã‡Ã¶
    // without it, consolidation is a no-op.
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("test.txt"), "file contents").unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));

    // Build a registry that shares the same memory store we'll inspect.
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let embedder: Arc<dyn crate::memory::Embedder> = Arc::new(HashEmbedder::new());
    let store: Arc<dyn crate::memory::MemoryStoreTrait> =
        Arc::new(MemoryStore::open_in_memory(embedder).unwrap());
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(FileReadTool::new((*sandbox).clone())));
    registry.register(Box::new(FileWriteTool::new((*sandbox).clone())));
    registry.register(Box::new(CreatePlanTool::new(workflow.clone())));
    registry.register(Box::new(CompleteStepTool::new(workflow.clone())));
    registry.register(Box::new(MemoryWriteTool::new(store.clone())));
    registry.register(Box::new(MemorySearchTool::new(store.clone())));
    let registry = Arc::new(registry);

    let provider = Arc::new(MockProvider::sequence(vec![
        vec![
            LlmEvent::ToolCallStart {
                index: 0,
                id: "call_1".into(),
                name: "file_write".into(),
            },
            LlmEvent::ToolCallArgumentDelta {
                index: 0,
                fragment: r#"{"path":"test.txt","content":"new contents"}"#.into(),
            },
            LlmEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ],
        vec![
            LlmEvent::TextDelta {
                text: "Done.".into(),
            },
            LlmEvent::Finish {
                reason: FinishReason::Stop,
            },
        ],
    ]));

    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider: provider,
            tools: registry,
            workflow: workflow,
            sandbox: sandbox.clone(),
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(128_000, 0.5),
            memory: Some(store.clone()),
            vision: None,
        },
        crate::project::Constitution::default(),
    );

    let (fanin_tx, _fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text("write to test.txt")];

    let _outcome = agent
        .run_turn(
            &mut messages,
            &fanin_tx,
            1,
            &mut cmd_rx,
            Some("test-session"),
        )
        .await
        .unwrap();

    // A working-memory event should have been recorded with the session_id.
    // (file_write is a durable action; read-only tools like file_read are no
    // longer recorded to avoid flooding recall with raw snapshots.)
    let working = store.list_by_tier(MemoryTier::Working).await.unwrap();
    let tool_event = working
        .iter()
        .find(|m| m.title == "tool: file_write")
        .expect("a file_write tool event should have been recorded");
    assert_eq!(
        tool_event.source_session_ids,
        vec!["test-session".to_string()],
        "the session_id must be threaded into record_tool_event"
    );
}

#[tokio::test]
async fn run_turn_records_error_request_stats() {
    // R21: a failed request (the stream died before any usage report —
    // trace id 255's BadGateway path) writes a request_stats row with our
    // estimated prompt tokens, cached_tokens NULL (not 0 — a real reported
    // miss stays distinguishable), and outcome = 'error'. Previously the
    // row was only written in the Usage arm, so error→retry cycles (each
    // re-sending the full prompt) were invisible to the cache aggregates.
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let embedder: Arc<dyn crate::memory::Embedder> = Arc::new(HashEmbedder::new());
    let store: Arc<dyn crate::memory::MemoryStoreTrait> =
        Arc::new(MemoryStore::open_in_memory(embedder).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    // The stream emits a single Error event and ends — the request died
    // before any SSE chunk (no usage, no timing).
    let provider = Arc::new(MockProvider::single(vec![LlmEvent::Error {
        error: "stream error (HTTP 200): connection reset (os error 10054)".into(),
    }]));

    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider: provider,
            tools: registry,
            workflow: workflow,
            sandbox: sandbox,
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(128_000, 0.5),
            memory: Some(store.clone()),
            vision: None,
        },
        crate::project::Constitution::default(),
    );

    let (fanin_tx, _fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);

    let session = store.start_session("test").await.unwrap();
    agent.set_session_id(session.id.clone());
    let sid = session.id.clone();
    let mut messages = vec![Message::user_text("hello")];
    // The turn fails (the outer retry layer owns retry messaging) — the
    // error row must still be recorded.
    let _ = agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, Some(&sid))
        .await;

    // The stats recording is fire-and-forget (spawned). Give it a moment.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let rows = store.request_stats_rows(&sid).await.unwrap();
    assert_eq!(rows.len(), 1, "exactly one error row");
    let row = &rows[0];
    assert_eq!(row.outcome.as_deref(), Some("error"));
    assert_eq!(row.purpose, None);
    assert_eq!(row.cached_tokens, None, "cached_tokens is NULL, not 0");
    assert_eq!(row.completion_tokens, 0);
    assert!(row.prompt_tokens > 0, "our estimate is recorded");
    assert_eq!(row.ttft_ms, None);
    assert_eq!(row.model, "mock");
}

#[tokio::test]
async fn run_turn_records_summarize_request_stats() {
    // R21: compaction's own mega-prompt (up to ~300K tokens, guaranteed 0%
    // cache) is invisible to the main loop's Usage arm —
    // summarize_with_interrupt consumes its own stream. The forwarded Usage
    // event must land in request_stats tagged purpose = 'summarize'.
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let embedder: Arc<dyn crate::memory::Embedder> = Arc::new(HashEmbedder::new());
    let store: Arc<dyn crate::memory::MemoryStoreTrait> =
        Arc::new(MemoryStore::open_in_memory(embedder).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    // First complete() call is the summarizer's (compaction runs before the
    // main request); second is the main request.
    let provider = Arc::new(MockProvider::sequence(vec![
        // Summarizer stream: the mega-prompt's usage.
        vec![
            LlmEvent::TextDelta {
                text: "summary of the conversation".into(),
            },
            LlmEvent::Usage {
                prompt_tokens: 250_000,
                completion_tokens: 500,
                reasoning_tokens: 0,
                cached_tokens: 0,
                ttft_ms: Some(900),
                generation_ms: Some(3000),
            },
            LlmEvent::Finish {
                reason: FinishReason::Stop,
            },
        ],
        // Main request after the summary replaced the history.
        vec![
            LlmEvent::TextDelta { text: "ok".into() },
            LlmEvent::Usage {
                prompt_tokens: 1200,
                completion_tokens: 50,
                reasoning_tokens: 0,
                cached_tokens: 0,
                ttft_ms: Some(100),
                generation_ms: Some(400),
            },
            LlmEvent::Finish {
                reason: FinishReason::Stop,
            },
        ],
    ]));

    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider: provider,
            tools: registry,
            workflow: workflow,
            sandbox: sandbox,
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(128_000, 0.5),
            memory: Some(store.clone()),
            vision: None,
        },
        crate::project::Constitution::default(),
    );

    let (fanin_tx, _fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);

    let session = store.start_session("test").await.unwrap();
    agent.set_session_id(session.id.clone());
    let sid = session.id.clone();
    // ~400KB of text ≈ 80K tiktoken tokens (" word" is one BPE token) —
    // past the 64K summarize threshold (128_000 * 0.5), triggering
    // compaction before the main request, and under the 96K hard ceiling
    // (128_000 - 32K headroom) so keep_recent stays at the normal 6. Nine
    // messages so summarize_with_interrupt has something to summarize (it
    // early-returns when len <= keep_recent + 1).
    let mut messages = vec![Message::user_text("start")];
    for i in 0..3 {
        messages.push(Message::assistant_text(format!("ack {i}")));
        messages.push(Message::user_text(format!("turn {i}")));
    }
    messages.push(Message::user_text("word ".repeat(80_000)));
    messages.push(Message::assistant_text("done"));
    let _ = agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, Some(&sid))
        .await
        .unwrap();

    // The stats recording is fire-and-forget (spawned). Give it a moment.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let rows = store.request_stats_rows(&sid).await.unwrap();
    assert_eq!(rows.len(), 2, "summarize row + main row; got: {rows:?}");
    let summarize_row = rows
        .iter()
        .find(|r| r.purpose.as_deref() == Some("summarize"))
        .expect("a purpose='summarize' row");
    assert_eq!(summarize_row.prompt_tokens, 250_000);
    assert_eq!(summarize_row.completion_tokens, 500);
    assert_eq!(summarize_row.cached_tokens, Some(0));
    assert_eq!(summarize_row.ttft_ms, Some(900));
    assert_eq!(summarize_row.outcome, None);
    let main_row = rows
        .iter()
        .find(|r| r.purpose.is_none())
        .expect("the main row");
    assert_eq!(main_row.prompt_tokens, 1200);
    assert_eq!(main_row.completion_tokens, 50);
    assert_eq!(main_row.outcome, None);
}

#[tokio::test]
async fn run_turn_records_request_stats_on_usage() {
    // When a Usage event arrives, the agent loop records a request_stats
    // row to the memory store with the session_id + model + tokens +
    // timing. The cache heuristic kicks in when cached_tokens is 0.
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let embedder: Arc<dyn crate::memory::Embedder> = Arc::new(HashEmbedder::new());
    let store: Arc<dyn crate::memory::MemoryStoreTrait> =
        Arc::new(MemoryStore::open_in_memory(embedder).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    let provider = Arc::new(MockProvider::sequence(vec![
        // First request: 1000 prompt, 200 completion, 800 cached (API-reported).
        vec![
            LlmEvent::TextDelta { text: "hi".into() },
            LlmEvent::Usage {
                prompt_tokens: 1000,
                completion_tokens: 200,
                reasoning_tokens: 0,
                cached_tokens: 800,
                ttft_ms: Some(420),
                generation_ms: Some(2000),
            },
            LlmEvent::Finish {
                reason: FinishReason::Stop,
            },
        ],
        // Second request: 2500 prompt, 400 completion, cached_tokens=0
        // → provider reports 0 cached; recorded as 0 (no heuristic).
        vec![
            LlmEvent::TextDelta {
                text: "again".into(),
            },
            LlmEvent::Usage {
                prompt_tokens: 2500,
                completion_tokens: 400,
                reasoning_tokens: 0,
                cached_tokens: 0,
                ttft_ms: Some(500),
                generation_ms: Some(4000),
            },
            LlmEvent::Finish {
                reason: FinishReason::Stop,
            },
        ],
    ]));

    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider: provider,
            tools: registry,
            workflow: workflow,
            sandbox: sandbox,
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(128_000, 0.5),
            memory: Some(store.clone()),
            vision: None,
        },
        crate::project::Constitution::default(),
    );

    let (fanin_tx, _fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);

    // First turn Î“Ã‡Ã¶ start a real session so session_stats has metadata
    // to look up. Normally AgentTask does this; here we do it directly.
    let session = store.start_session("test").await.unwrap();
    agent.set_session_id(session.id.clone());
    let sid = session.id.clone();
    let mut messages = vec![Message::user_text("hello")];
    let _ = agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, Some(&sid))
        .await
        .unwrap();
    // Second turn (provider reports cached_tokens=0 on this request).
    messages.push(Message::user_text("again"));
    let _ = agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, Some(&sid))
        .await
        .unwrap();

    // The stats recording is fire-and-forget (spawned). Give it a moment.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Two rows should be recorded for the "mock" model.
    let stats = store.session_stats(&sid).await.unwrap();
    assert_eq!(stats.request_count, 2);
    assert_eq!(stats.prompt_tokens, 3500);
    assert_eq!(stats.completion_tokens, 600);
    // First request reported 800 cached; second reported 0 (trusted as-is).
    assert_eq!(stats.cached_tokens, 800);
    assert_eq!(stats.per_model.len(), 1);
    assert_eq!(stats.per_model[0].model, "mock");
    // The default mock is unnamed (provider_name "" → endpoint None), so its
    // breakdown row groups with pre-endpoint rows.
    assert_eq!(stats.per_model[0].endpoint, None);
    assert_eq!(stats.per_model[0].timed_requests, 2);
    assert_eq!(stats.per_model[0].ttft_ms_total, 920);

    // Named-mock phase: a provider whose provider_name() reports "a100" must
    // record endpoint Some("a100") on its breakdown row — the per-endpoint
    // dimension (plan 07d2dc07). Fresh store/session/agent: this phase
    // asserts the recording path only, not cross-session aggregation.
    let embedder2: Arc<dyn crate::memory::Embedder> = Arc::new(HashEmbedder::new());
    let store2: Arc<dyn crate::memory::MemoryStoreTrait> =
        Arc::new(MemoryStore::open_in_memory(embedder2).unwrap());
    let dir2 = tempdir().unwrap();
    let workflow2 = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir2.path().join("plans"),
    )));
    let sandbox2 = Arc::new(Sandbox::new(dir2.path()).unwrap());
    let registry2 = make_registry((*sandbox2).clone(), workflow2.clone());
    let agent2 = AgentLoop::new(
        AgentLoopConfig {
            provider: Arc::new(
                MockProvider::single(vec![
                    LlmEvent::TextDelta { text: "hi".into() },
                    LlmEvent::Usage {
                        prompt_tokens: 100,
                        completion_tokens: 10,
                        reasoning_tokens: 0,
                        cached_tokens: 0,
                        ttft_ms: Some(100),
                        generation_ms: Some(500),
                    },
                    LlmEvent::Finish {
                        reason: FinishReason::Stop,
                    },
                ])
                .named("a100"),
            ),
            tools: registry2,
            workflow: workflow2,
            sandbox: sandbox2,
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(128_000, 0.5),
            memory: Some(store2.clone()),
            vision: None,
        },
        crate::project::Constitution::default(),
    );
    let session2 = store2.start_session("test").await.unwrap();
    agent2.set_session_id(session2.id.clone());
    let sid2 = session2.id.clone();
    let mut messages2 = vec![Message::user_text("hello")];
    let _ = agent2
        .run_turn(&mut messages2, &fanin_tx, 1, &mut cmd_rx, Some(&sid2))
        .await
        .unwrap();
    // The stats recording is fire-and-forget (spawned). Give it a moment.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let stats2 = store2.session_stats(&sid2).await.unwrap();
    assert_eq!(stats2.request_count, 1);
    assert_eq!(stats2.per_model.len(), 1);
    assert_eq!(
        stats2.per_model[0].endpoint,
        Some("a100".to_string()),
        "a named provider must record its endpoint on the breakdown row"
    );
}

#[tokio::test]
async fn cache_heuristic_skipped_after_summarization() {
    // Regression for the over-reporting finding: after a context
    // summarization the conversation prefix is rewritten, so the
    // min(prev, curr) cache heuristic must NOT be applied Î“Ã‡Ã¶ the next
    // request has no large shared-prefix cache to estimate and should
    // report cached_tokens = 0 (not min of the pre-summary sizes).
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let embedder: Arc<dyn crate::memory::Embedder> = Arc::new(HashEmbedder::new());
    let store: Arc<dyn crate::memory::MemoryStoreTrait> =
        Arc::new(MemoryStore::open_in_memory(embedder).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    // A tiny context window forces summarization (max 100 tokens,
    // summarize at 50). 9 messages > keep_recent(6) + 1, so a summary runs.
    let provider = Arc::new(MockProvider::sequence(vec![
        // First request (pre-summary): 1000 prompt, 800 API-reported cached.
        vec![
            LlmEvent::TextDelta { text: "hi".into() },
            LlmEvent::Usage {
                prompt_tokens: 1000,
                completion_tokens: 100,
                reasoning_tokens: 0,
                cached_tokens: 800,
                ttft_ms: None,
                generation_ms: None,
            },
            LlmEvent::Finish {
                reason: FinishReason::Stop,
            },
        ],
        // The summarization call itself (its text becomes the summary).
        vec![
            LlmEvent::TextDelta {
                text: "SUMMARY".into(),
            },
            LlmEvent::Finish {
                reason: FinishReason::Stop,
            },
        ],
        // Second request (post-summary): 900 prompt, cached_tokens=0 from
        // the API. The provider's value is trusted as-is → recorded as 0
        // (the old heuristic would have reported min(1000, 900) = 900).
        vec![
            LlmEvent::TextDelta {
                text: "again".into(),
            },
            LlmEvent::Usage {
                prompt_tokens: 900,
                completion_tokens: 100,
                reasoning_tokens: 0,
                cached_tokens: 0,
                ttft_ms: None,
                generation_ms: None,
            },
            LlmEvent::Finish {
                reason: FinishReason::Stop,
            },
        ],
    ]));

    // max_context = 100, fill_rate 0.5 Î“Ã¥Ã† summarize_at = 50 tokens. The 9
    // messages each carry >6 tokens, so the threshold trips on the 2nd turn.
    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider: provider,
            tools: registry,
            workflow: workflow,
            sandbox: sandbox,
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(100, 0.5),
            memory: Some(store.clone()),
            vision: None,
        },
        crate::project::Constitution::default(),
    );

    let (fanin_tx, _fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);

    let session = store.start_session("test").await.unwrap();
    agent.set_session_id(session.id.clone());
    let sid = session.id.clone();

    // Turn 1 uses a SHORT conversation (under the summarize threshold) so
    // no summary runs yet; the first response is the main request (1000
    // prompt / 800 API-reported cached).
    let mut messages = vec![Message::user_text("hi")];
    let _ = agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, Some(&sid))
        .await
        .unwrap();

    // Grow the conversation past the threshold (and past keep_recent + 1)
    // so the SECOND turn triggers a real summarization before its main
    // request. Each message carries >6 tokens; 8+ messages Î“Ã§Ã† over 50.
    while messages.len() < 9 {
        messages.push(Message::user_text(
            "additional conversation content with enough tokens to pass the threshold",
        ));
    }

    // Second turn: the conversation is over the summarize threshold, so a
    // summary runs (rewriting the prefix), then the main request records
    // 900 prompt with cached_tokens = 0 (heuristic suppressed).
    let _ = agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, Some(&sid))
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let stats = store.session_stats(&sid).await.unwrap();
    assert_eq!(stats.request_count, 2);
    // 800 (API-reported) + 0 (provider said 0, trusted as-is) = 800.
    assert_eq!(
        stats.cached_tokens, 800,
        "provider-reported cached_tokens=0 must be recorded as 0, not estimated"
    );
}

/// A mock provider that captures the system prompt from each `complete`
/// call, so tests can assert on the constitution injected into the prompt.
struct CapturingProvider {
    captured: Arc<Mutex<Vec<String>>>,
    /// The content of the LAST message of each `complete` call — lets tests
    /// assert the byte-stable CONTEXT_FOOTER is the final message sent.
    captured_tails: Arc<Mutex<Vec<String>>>,
    /// The (role, content) of the LAST TWO messages of each `complete` call —
    /// lets tests assert the trailing placement's roles (user on
    /// `tail_as_user_messages` vendors, which must never see a trailing system
    /// block) as well as the byte-stable footer position.
    captured_trailing: Arc<Mutex<Vec<Vec<(String, String)>>>>,
    /// The serialized tools array of each `complete` call — lets tests assert
    /// the plan-frozen advertisement is byte-stable across state transitions.
    captured_tool_sets: Arc<Mutex<Vec<String>>>,
    caps: Capabilities,
    /// The provider kind reported by `kind()` — OpenAI by default, settable so
    /// tests can drive the Local (Ollama) message-placement branch.
    kind: ProviderKind,
    /// The model id reported by `model()` — "mock-capture" by default,
    /// settable so tests can drive vendor-policy packaging branches (e.g.
    /// the DeepSeek `tail_as_user_messages` path).
    model: &'static str,
}

impl CapturingProvider {
    fn new() -> (Self, Arc<Mutex<Vec<String>>>) {
        let (provider, captured, _tails) = Self::new_with_tails();
        (provider, captured)
    }

    /// Like [`new`](Self::new) but also returns the captured last-message
    /// contents (one entry per `complete` call).
    fn new_with_tails() -> (Self, Arc<Mutex<Vec<String>>>, Arc<Mutex<Vec<String>>>) {
        Self::build(ProviderKind::OpenAI)
    }

    /// Like [`new_with_tails`](Self::new_with_tails) but the provider reports
    /// `ProviderKind::Local` — drives the Ollama message-placement branch in
    /// `run_turn` (tail + footer as trailing USER messages, no trailing system
    /// block).
    fn new_local_with_tails() -> (Self, Arc<Mutex<Vec<String>>>, Arc<Mutex<Vec<String>>>) {
        Self::build(ProviderKind::Local)
    }

    /// Like [`new_with_tails`](Self::new_with_tails) but the provider reports
    /// the given model id — drives vendor-policy packaging branches (e.g.
    /// DeepSeek's `tail_as_user_messages`) while `kind()` stays OpenAI.
    fn new_with_model_with_tails(
        model: &'static str,
    ) -> (Self, Arc<Mutex<Vec<String>>>, Arc<Mutex<Vec<String>>>) {
        let (mut provider, captured, tails) = Self::build(ProviderKind::OpenAI);
        provider.model = model;
        (provider, captured, tails)
    }

    /// Shared constructor for the `new_with_tails` / `new_local_with_tails`
    /// variants; `kind` is what `LlmClient::kind()` reports.
    fn build(kind: ProviderKind) -> (Self, Arc<Mutex<Vec<String>>>, Arc<Mutex<Vec<String>>>) {
        let (provider, captured, tails, _tool_sets) = Self::build_with_tool_sets(kind);
        (provider, captured, tails)
    }

    /// Like [`build`](Self::build) but also returns the serialized tools-array
    /// capture (one entry per `complete` call).
    fn build_with_tool_sets(
        kind: ProviderKind,
    ) -> (
        Self,
        Arc<Mutex<Vec<String>>>,
        Arc<Mutex<Vec<String>>>,
        Arc<Mutex<Vec<String>>>,
    ) {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let captured_tails = Arc::new(Mutex::new(Vec::new()));
        let captured_tool_sets = Arc::new(Mutex::new(Vec::new()));
        let captured_trailing = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                captured: Arc::clone(&captured),
                captured_tails: Arc::clone(&captured_tails),
                captured_tool_sets: Arc::clone(&captured_tool_sets),
                captured_trailing: Arc::clone(&captured_trailing),
                caps: Capabilities::openai(),
                kind,
                model: "mock-capture",
            },
            captured,
            captured_tails,
            captured_tool_sets,
        )
    }

    /// Provider + all three captures: the system prompt (head), the last
    /// message (footer), and the serialized tools array per `complete` call.
    fn new_with_tool_sets() -> (
        Self,
        Arc<Mutex<Vec<String>>>,
        Arc<Mutex<Vec<String>>>,
        Arc<Mutex<Vec<String>>>,
    ) {
        Self::build_with_tool_sets(ProviderKind::OpenAI)
    }

    /// Handle to the trailing (role, content) pairs captured so far — grab it
    /// before the provider is moved into the agent loop. One entry per
    /// `complete` call: the last two messages of the request.
    fn trailing_handle(&self) -> Arc<Mutex<Vec<Vec<(String, String)>>>> {
        Arc::clone(&self.captured_trailing)
    }
}

#[async_trait]
impl LlmClient for CapturingProvider {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
    fn kind(&self) -> ProviderKind {
        self.kind
    }
    fn model(&self) -> &str {
        self.model
    }
    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        _tool_choice: Option<crate::provider::ToolChoice>,
    ) -> crate::error::Result<BoxStream<'_, LlmEvent>> {
        // Capture the system prompt (first message).
        if let Some(sys) = messages.iter().find(|m| m.role == Role::System) {
            self.captured.lock().await.push(sys.content.as_text());
        }
        // Capture the LAST message (the byte-stable footer in production).
        if let Some(last) = messages.last() {
            self.captured_tails
                .lock()
                .await
                .push(last.content.as_text());
        }
        // Capture the last TWO messages with their roles — the trailing
        // placement (volatile tail + footer), whose roles differ per vendor.
        let trailing: Vec<(String, String)> = messages
            .iter()
            .rev()
            .take(2)
            .rev()
            .map(|m| {
                (
                    format!("{:?}", m.role).to_lowercase(),
                    m.content.as_text(),
                )
            })
            .collect();
        self.captured_trailing.lock().await.push(trailing);
        // Capture the serialized tools array (the plan-frozen advertisement).
        self.captured_tool_sets
            .lock()
            .await
            .push(serde_json::to_string(tools).unwrap_or_default());
        // End the turn immediately.
        let stream = futures::stream::iter(vec![LlmEvent::Finish {
            reason: FinishReason::Stop,
        }]);
        Ok(Box::pin(stream))
    }
}

#[tokio::test]
async fn constitution_reread_after_agent_md_edit() {
    // The agent loop must re-read agent.md from disk when it changes, so
    // editing the constitution takes effect on the next turn without
    // restarting. We back the loop with a ConstitutionSource over temp
    // files, run a turn, edit the project agent.md, run another turn, and
    // assert the new rule appears in the captured system prompt.
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    let gpath = dir.path().join("global.md");
    let ppath = dir.path().join("project.md");
    std::fs::write(&gpath, "GLOBAL-RULE-V1").unwrap();
    std::fs::write(&ppath, "PROJECT-RULE-V1").unwrap();
    let source = crate::project::ConstitutionSource::new(&gpath, &ppath).unwrap();

    let (provider, captured) = CapturingProvider::new();
    let agent = AgentLoop::with_constitution_source(
        AgentLoopConfig {
            provider: Arc::new(provider),
            tools: registry,
            workflow: workflow,
            sandbox: sandbox.clone(),
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(128_000, 0.5),
            memory: None,
            vision: None,
        },
        source,
    );

    let (fanin_tx, _fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);

    // Turn 1 Î“Ã‡Ã¶ the v1 rules should be in the system prompt.
    let mut messages = vec![Message::user_text("hi")];
    agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await
        .unwrap();
    let prompts = captured.lock().await;
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].contains("GLOBAL-RULE-V1"));
    assert!(prompts[0].contains("PROJECT-RULE-V1"));
    drop(prompts);

    // Edit the project constitution + bump mtime.
    std::fs::write(&ppath, "PROJECT-RULE-V2-NEW").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));

    // Turn 2 Î“Ã‡Ã¶ the new rule must appear (re-read from disk).
    let mut messages2 = vec![Message::user_text("again")];
    agent
        .run_turn(&mut messages2, &fanin_tx, 1, &mut cmd_rx, None)
        .await
        .unwrap();
    let prompts = captured.lock().await;
    assert_eq!(prompts.len(), 2);
    assert!(
        prompts[1].contains("PROJECT-RULE-V2-NEW"),
        "turn 2 system prompt should contain the edited rule; got: {}",
        prompts[1]
    );
    assert!(
        !prompts[1].contains("PROJECT-RULE-V1"),
        "the old rule should be gone after re-reading"
    );
}

#[tokio::test]
async fn volatile_tail_then_stable_footer_appended_and_popped() {
    // The request sent to the provider must end with the volatile tail
    // (workflow state) followed by the byte-stable CONTEXT_FOOTER as the LAST
    // message — per the empirical provider cache law, a constant final
    // message keeps the cached prefix reusable across tail changes. After the
    // turn, both transient messages must be popped from the caller's
    // persistent `messages` vec.
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    {
        let mut wf = workflow.lock().await;
        wf.create_plan("P", "G", "C", vec!["step 1".into()])
            .unwrap();
    }
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    let (provider, captured, captured_tails) = CapturingProvider::new_with_tails();
    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider: Arc::new(provider),
            tools: registry,
            workflow: workflow,
            sandbox: sandbox,
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(128_000, 0.5),
            memory: None,
            vision: None,
        },
        crate::project::Constitution::default(),
    );

    let (fanin_tx, _fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);

    let mut messages = vec![Message::user_text("hi")];
    agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await
        .unwrap();

    // (a) The request's LAST message is exactly the byte-stable footer.
    let tails = captured_tails.lock().await;
    assert_eq!(tails.len(), 1);
    assert_eq!(tails[0], crate::agent::prompt::CONTEXT_FOOTER);
    drop(tails);

    // (b) The captured system prompt (the .find() capture = the HEAD) is the
    // stable head and does NOT contain the footer.
    let prompts = captured.lock().await;
    assert_eq!(prompts.len(), 1);
    assert!(
        prompts[0].contains("plan-first workflow"),
        "head has the preamble"
    );
    assert!(
        !prompts[0].contains(crate::agent::prompt::CONTEXT_FOOTER),
        "footer is a separate trailing message, not part of the head"
    );
    drop(prompts);

    // (c) Both transient messages were popped: the persistent vec has no
    // footer and no workflow tail — just the stable head (inserted at index
    // 0), the input user message, and the turn's (empty) assistant message.
    assert_eq!(
        messages.len(),
        3,
        "stable head + input user msg + the turn's assistant msg"
    );
    assert_eq!(messages[0].role, Role::System);
    assert_eq!(messages[1].role, Role::User);
    assert_eq!(messages[2].role, Role::Assistant);
    assert!(
        !messages
            .iter()
            .any(|m| m.content.as_text() == crate::agent::prompt::CONTEXT_FOOTER),
        "footer must be popped after the request"
    );
    assert!(
        !messages
            .iter()
            .any(|m| m.content.as_text().contains("# WORKFLOW STATE")),
        "volatile tail must be popped after the request"
    );
}

#[tokio::test]
async fn deepseek_vendor_tail_rides_as_user_messages_no_trailing_system_block() {
    // Regression (2027-01-11 deepseek exit-note loop + OPEN sentinel-mirror
    // 5cf5469c): DeepSeek-vendor models echo trailing SYSTEM blocks instead of
    // answering the user. The tail + footer therefore ride at the end as USER
    // messages — the request ends the way a normal client turn does — while
    // messages[0] stays the byte-stable head, i.e. the prefix a prompt cache
    // keys on. Folding the tail into the head (the previous strategy) cost
    // 5.1-9.0% cache hit and 115-120K re-read tokens per complete_step:
    // `deepseek_head_stays_byte_stable_across_plan_progress_bumps`.
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    {
        let mut wf = workflow.lock().await;
        wf.create_plan("P", "G", "C", vec!["step 1".into()])
            .unwrap();
    }
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    let (provider, captured, captured_tails) =
        CapturingProvider::new_with_model_with_tails("deepseek-v4-flash");
    let trailing = provider.trailing_handle();
    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider: Arc::new(provider),
            tools: registry,
            workflow: workflow,
            sandbox: sandbox,
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(128_000, 0.5),
            memory: None,
            vision: None,
        },
        crate::project::Constitution::default(),
    );

    let (fanin_tx, _fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);

    let mut messages = vec![Message::user_text("hi")];
    agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await
        .unwrap();

    // (a) The trailing pair is [volatile tail, footer], both USER-role: nothing
    // trailing for DeepSeek to mirror, and the footer stays the byte-stable
    // final message (empirical cache law).
    let trailing = trailing.lock().await;
    assert_eq!(trailing.len(), 1);
    assert_eq!(trailing[0].len(), 2, "tail + footer must both be sent");
    assert_eq!(
        trailing[0][0].0, "user",
        "the volatile tail must ride as a user message"
    );
    assert!(
        trailing[0][0].1.contains("# WORKFLOW STATE"),
        "the workflow state must still reach the request"
    );
    assert_eq!(trailing[0][1].0, "user", "the footer must be a user message");
    assert_eq!(
        trailing[0][1].1,
        crate::agent::prompt::CONTEXT_FOOTER,
        "the byte-stable footer must be the final message"
    );
    drop(trailing);

    // (b) The head is the stable prefix: no workflow state folded in, so a
    // complete_step progress bump cannot change messages[0].
    let prompts = captured.lock().await;
    assert_eq!(prompts.len(), 1);
    assert!(
        !prompts[0].contains("# WORKFLOW STATE"),
        "the volatile tail must not be folded into the leading system message"
    );
    assert!(
        prompts[0].contains("plan-first workflow"),
        "head has the preamble"
    );
    drop(prompts);

    // (c) The final message is the footer — no trailing system block exists.
    let tails = captured_tails.lock().await;
    assert_eq!(tails.len(), 1);
    assert_eq!(tails[0], crate::agent::prompt::CONTEXT_FOOTER);
    drop(tails);

    // (d) Nothing transient persisted: head + user + assistant only.
    assert_eq!(
        messages.len(),
        3,
        "stable head + input user msg + the turn's assistant msg"
    );
    assert!(
        !messages
            .iter()
            .any(|m| m.content.as_text() == crate::agent::prompt::CONTEXT_FOOTER),
        "no footer must remain in the persistent vec"
    );
}

/// Regression (cache resets on every plan-item check-off — measured in a live
/// DeepSeek session, trace window 2026-09-14 04:48:52-04:51:06 UTC): the
/// volatile tail was folded into the LEADING system message, so each
/// `complete_step` rewrote messages[0] and the provider's prefix cache died at
/// ~6.9K tokens — five check-off rows landed at 5.1-9.0% hit while re-billing
/// 115-120K tokens each (`.coding/analysis/cache-hit-6-aggregates.txt`: section
/// B classifies the breaks as HEAD, k=0; section F carries the true per-request
/// numbers). The prefix a provider caches is messages[0], so it must not move on
/// a progress bump: the volatility rides in trailing USER messages instead, with
/// the constant CONTEXT_FOOTER as the byte-stable final message.
#[tokio::test]
async fn deepseek_head_stays_byte_stable_across_plan_progress_bumps() {
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    {
        let mut wf = workflow.lock().await;
        wf.create_plan("P", "G", "C", vec!["step 1".into(), "step 2".into()])
            .unwrap();
    }
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    let (provider, captured, captured_tails) =
        CapturingProvider::new_with_model_with_tails("deepseek-v4-flash");
    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider: Arc::new(provider),
            tools: registry,
            workflow: workflow.clone(),
            sandbox: sandbox,
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(128_000, 0.5),
            memory: None,
            vision: None,
        },
        crate::project::Constitution::default(),
    );

    let (fanin_tx, _fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);

    let mut messages = vec![Message::user_text("hi")];
    agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await
        .unwrap();
    // The check-off: step 1 of 2 done — PROGRESS moves from 0/2 to 1/2.
    {
        let mut wf = workflow.lock().await;
        wf.complete_step(1).unwrap();
    }
    agent
        .run_turn(&mut messages, &fanin_tx, 2, &mut cmd_rx, None)
        .await
        .unwrap();

    // (a) The cacheable prefix — the leading system message — is byte-identical
    // across the progress bump. This is the reproduction: with the tail folded
    // into the head the two differ by the PROGRESS line.
    let prompts = captured.lock().await;
    assert_eq!(prompts.len(), 2, "one captured system prompt per turn");
    assert_eq!(
        prompts[0], prompts[1],
        "the leading system message is the provider's cached prefix and must not \
         change on a plan-progress bump"
    );
    assert!(
        !prompts[1].contains("# WORKFLOW STATE"),
        "the volatile tail must not be folded into the leading system message"
    );
    drop(prompts);

    // (b) The volatility rides at the end instead, and the LAST message stays
    // the constant footer (the empirical cache law needs a byte-stable final
    // message).
    let tails = captured_tails.lock().await;
    assert_eq!(tails.len(), 2);
    assert_eq!(
        tails[0],
        crate::agent::prompt::CONTEXT_FOOTER,
        "the constant footer must be the final message on the DeepSeek path"
    );
    assert_eq!(
        tails[0], tails[1],
        "the final message must be byte-stable across progress bumps"
    );
}

#[tokio::test]
async fn plan_frozen_tools_head_and_footer_byte_stable_across_executing_reviewing() {
    // Perf review L4: the tools array rides at the head of the request body,
    // so the array changing at the Executing→Reviewing transition resets the
    // provider prefix cache and re-bills the whole conversation (~40-75s of
    // server-side prefill at late-plan context sizes). While a plan is active
    // the SCHEMA array must be byte-stable — the plan-frozen advertisement —
    // and the head and the CONTEXT_FOOTER must be byte-stable with it.
    // Dispatch still enforces the per-state rules; this asserts the
    // advertisement only.
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    {
        let mut wf = workflow.lock().await;
        wf.create_plan("P", "G", "C", vec!["step 1".into(), "step 2".into()])
            .unwrap();
    }
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    let (provider, captured, captured_tails, captured_tool_sets) =
        CapturingProvider::new_with_tool_sets();
    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider: Arc::new(provider),
            tools: registry,
            workflow: workflow.clone(),
            sandbox: sandbox,
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(128_000, 0.5),
            memory: None,
            vision: None,
        },
        crate::project::Constitution::default(),
    );

    let (fanin_tx, _fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);

    // Iteration 1: Executing with the plan active.
    let mut messages = vec![Message::user_text("hi")];
    agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await
        .unwrap();

    // Complete every step → the workflow transitions to Reviewing (the state
    // the closing sequence runs in).
    {
        let mut wf = workflow.lock().await;
        wf.complete_step(0).unwrap();
        wf.complete_step(1).unwrap();
        assert_eq!(wf.state(), crate::workflow::WorkflowState::Reviewing);
    }

    // Iteration 2: Reviewing.
    let mut messages2 = vec![Message::user_text("again")];
    agent
        .run_turn(&mut messages2, &fanin_tx, 2, &mut cmd_rx, None)
        .await
        .unwrap();

    // (a) The tools array is byte-identical across the transition.
    let tool_sets = captured_tool_sets.lock().await;
    assert_eq!(tool_sets.len(), 2);
    assert_eq!(
        tool_sets[0], tool_sets[1],
        "the tools array must be byte-identical across the Executing→Reviewing \
         transition (any byte change resets the provider prefix cache)"
    );
    assert!(
        tool_sets[0].contains("\"name\":\"finish\""),
        "the frozen array must already advertise finish so Reviewing adds nothing"
    );
    drop(tool_sets);

    // (b) The stable head is byte-identical across the transition.
    let heads = captured.lock().await;
    assert_eq!(heads.len(), 2);
    assert_eq!(
        heads[0], heads[1],
        "the stable head must be byte-identical across the transition"
    );
    drop(heads);

    // (c) The last message is the byte-stable footer in both iterations.
    let tails = captured_tails.lock().await;
    assert_eq!(tails.len(), 2);
    assert_eq!(tails[0], crate::agent::prompt::CONTEXT_FOOTER);
    assert_eq!(tails[1], crate::agent::prompt::CONTEXT_FOOTER);
}

#[tokio::test]
async fn plan_frozen_tool_set_is_stable_within_and_across_plans() {
    // The advertised array must be byte-stable both WITHIN a plan and ACROSS a
    // plan-KIND change: it rides at the head of the request body, so any change
    // resets the provider's prefix cache. Research plans used to get a narrower
    // surface (no file_write), which rewrote the head on every plan-kind change
    // — measured 2027-01-11: the cached prefix collapsed to 6,016 tokens with
    // 15.7–17.9s TTFT (`.coding/analysis/cache-hit-6-report.md`, cause 2). The
    // research restriction is a DISPATCH-time gate, not an advertised one.
    // Same session, two plans of different kinds, three turns.
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    {
        let mut wf = workflow.lock().await;
        wf.create_plan_with_kind(
            "R",
            "G",
            "C",
            vec!["step 1".into()],
            crate::workflow::PlanKind::Research,
            None,
        )
        .unwrap();
    }
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    let (provider, _captured, _tails, captured_tool_sets) =
        CapturingProvider::new_with_tool_sets();
    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider: Arc::new(provider),
            tools: registry,
            workflow: workflow.clone(),
            sandbox: sandbox,
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(128_000, 0.5),
            memory: None,
            vision: None,
        },
        crate::project::Constitution::default(),
    );

    let (fanin_tx, _fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);

    // Turn 1: the research plan is active.
    let mut messages = vec![Message::user_text("hi")];
    agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await
        .unwrap();

    // Turn 2: SAME plan, no plan change — the array must be identical.
    let mut messages1b = vec![Message::user_text("again")];
    agent
        .run_turn(&mut messages1b, &fanin_tx, 2, &mut cmd_rx, None)
        .await
        .unwrap();

    // Swap the plan scope: abandon the research plan, create an
    // implementation plan — a KIND change, the case that used to rewrite the
    // head.
    {
        let mut wf = workflow.lock().await;
        wf.abandon_plan().unwrap();
        wf.create_plan("P", "G", "C", vec!["step 1".into()]).unwrap();
    }

    let mut messages2 = vec![Message::user_text("again")];
    agent
        .run_turn(&mut messages2, &fanin_tx, 3, &mut cmd_rx, None)
        .await
        .unwrap();

    let tool_sets = captured_tool_sets.lock().await;
    assert_eq!(tool_sets.len(), 3);
    // The source-mutating tool is advertised on BOTH plan kinds — enforcement
    // (which denies it on a research plan) happens at dispatch.
    for (i, set) in tool_sets.iter().enumerate() {
        assert!(
            set.contains("\"name\":\"file_write\""),
            "turn {i} must advertise the full frozen surface"
        );
        assert!(set.contains("\"name\":\"file_read\""));
    }
    assert_eq!(
        tool_sets[0], tool_sets[1],
        "the frozen surface must stay byte-identical within one plan"
    );
    assert_eq!(
        tool_sets[1], tool_sets[2],
        "a plan-kind change must not rewrite the advertised array (prefix cache)"
    );
}

#[tokio::test]
async fn local_provider_tail_rides_as_user_messages_no_trailing_system_block() {
    // A Local (Ollama/vLLM/LM Studio) provider rejects any system message that
    // is not the FIRST message ("system message must be at the beginning").
    // So for Local the volatile tail + footer ride at the end as USER messages
    // (permitted there) and messages[0] stays the byte-stable head — the same
    // placement DeepSeek-vendor needs for a different reason (echo). Mirrors
    // the DeepSeek test above with a Local-kind CapturingProvider.
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    {
        let mut wf = workflow.lock().await;
        wf.create_plan("P", "G", "C", vec!["step 1".into()])
            .unwrap();
    }
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    let (provider, captured, captured_tails) = CapturingProvider::new_local_with_tails();
    let trailing = provider.trailing_handle();
    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider: Arc::new(provider),
            tools: registry,
            workflow: workflow,
            sandbox: sandbox,
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(128_000, 0.5),
            memory: None,
            vision: None,
        },
        crate::project::Constitution::default(),
    );

    let (fanin_tx, _fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);

    let mut messages = vec![Message::user_text("hi")];
    agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await
        .unwrap();

    // (a) The trailing pair is [volatile tail, footer], both USER-role — the
    // only placement Ollama accepts (a system message may occupy first
    // position only), and it leaves messages[0] byte-stable.
    let trailing = trailing.lock().await;
    assert_eq!(trailing.len(), 1);
    assert_eq!(trailing[0].len(), 2, "tail + footer must both be sent");
    assert_eq!(
        trailing[0][0].0, "user",
        "the volatile tail rides as a user message"
    );
    assert!(trailing[0][0].1.contains("# WORKFLOW STATE"));
    assert_eq!(trailing[0][1].0, "user", "the footer rides as a user message");
    assert_eq!(trailing[0][1].1, crate::agent::prompt::CONTEXT_FOOTER);
    drop(trailing);

    // (a2) The LAST message is the footer, so the request ends with a user
    // message and carries no trailing system block at all.
    let tails = captured_tails.lock().await;
    assert_eq!(tails.len(), 1);
    assert_eq!(tails[0], crate::agent::prompt::CONTEXT_FOOTER);
    drop(tails);

    // (b) The single leading system message (messages[0]) is the STABLE HEAD
    // only — deliberately: it is the prefix a prompt cache keys on, so the
    // workflow state must never be folded into it (folding cost DeepSeek
    // 115-120K re-read tokens per complete_step, and Local shares the path).
    let prompts = captured.lock().await;
    assert_eq!(prompts.len(), 1);
    assert!(
        prompts[0].contains("plan-first workflow"),
        "head has the preamble"
    );
    assert!(
        !prompts[0].contains("# WORKFLOW STATE"),
        "the volatile tail must not be folded into the leading system message"
    );
    drop(prompts);

    // (c) Nothing transient leaks into the persistent vec: exactly the stable
    // head (index 0), the input user message, and the turn's assistant
    // message — the tail + footer were pushed for the request and popped.
    assert_eq!(
        messages.len(),
        3,
        "stable head + input user msg + the turn's assistant msg"
    );
    assert_eq!(messages[0].role, Role::System);
    assert_eq!(messages[1].role, Role::User);
    assert_eq!(messages[2].role, Role::Assistant);
    assert!(
        !messages
            .iter()
            .any(|m| m.content.as_text() == crate::agent::prompt::CONTEXT_FOOTER),
        "no footer was ever pushed on Local"
    );
}

/// A mock provider whose `complete` fails the first `fail_times` calls,
/// then returns a minimal stop stream. Used to verify that
/// `complete_with_retry` surfaces every failed attempt as a transient
/// retrying error event (2026-08-22 retry-visibility report) and that each
/// backoff sleep is parked on the provider for the next attempt's record.
struct FlakyProvider {
    caps: Capabilities,
    /// How many times `complete` was called (to verify retry count).
    calls: Arc<std::sync::atomic::AtomicU32>,
    /// How many initial calls fail before the first success.
    fail_times: u32,
    /// Captures every `record_backoff_ms` call (the parked retry sleeps).
    backoffs: Arc<std::sync::Mutex<Vec<u32>>>,
    /// Optional Usage event emitted on the success stream (before Finish) —
    /// lets a test exercise the success stats row alongside the error rows.
    success_usage: Option<LlmEvent>,
}

#[async_trait]
impl LlmClient for FlakyProvider {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
    fn kind(&self) -> ProviderKind {
        ProviderKind::OpenAI
    }
    fn model(&self) -> &str {
        "mock-flaky"
    }
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _tool_choice: Option<crate::provider::ToolChoice>,
    ) -> crate::error::Result<BoxStream<'_, LlmEvent>> {
        let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if n < self.fail_times {
            return Err(crate::error::Error::Provider("flaky failure".into()));
        }
        let mut events = Vec::new();
        if let Some(usage) = &self.success_usage {
            events.push(usage.clone());
        }
        events.push(LlmEvent::Finish {
            reason: FinishReason::Stop,
        });
        let stream = futures::stream::iter(events);
        Ok(Box::pin(stream))
    }

    fn record_backoff_ms(&self, ms: u32) {
        self.backoffs
            .lock()
            .expect("FlakyProvider backoffs lock poisoned")
            .push(ms);
    }
}

#[tokio::test(start_paused = true)]
async fn complete_with_retry_emits_one_retrying_error_per_failure() {
    // A provider that fails twice then succeeds: each failed attempt must
    // surface as a transient Error{retrying:true} event naming the attempt,
    // so the inflight bar's activity log explains a multi-second "sending"
    // window (2026-08-22 retry-visibility report).
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());
    let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let provider: Arc<dyn LlmClient> = Arc::new(FlakyProvider {
        caps: Capabilities::openai(),
        calls: Arc::clone(&calls),
        fail_times: 2,
        backoffs: Arc::new(std::sync::Mutex::new(Vec::new())),
        success_usage: None,
    });
    let agent = AgentLoop::new(
        test_config(provider, registry, workflow, sandbox.clone()),
        crate::project::Constitution::default(),
    );
    let provider = agent.provider();

    let (fanin_tx, mut fanin_rx) = mpsc::channel(8);
    let result = agent
        .complete_with_retry(&provider, &[], &[], &fanin_tx, 1, None)
        .await;
    assert!(result.is_ok(), "the third attempt must succeed");

    let mut notes = Vec::new();
    while let Ok((_id, event)) = fanin_rx.try_recv() {
        if let AgentEvent::Error { error, retrying } = event {
            assert!(retrying, "retry notes must be transient: {error}");
            notes.push(error);
        }
    }
    assert_eq!(notes.len(), 2, "one retry note per failed attempt");
    assert!(
        notes[0].contains("attempt 1/3"),
        "unexpected first note: {}",
        notes[0]
    );
    assert!(
        notes[1].contains("attempt 2/3"),
        "unexpected second note: {}",
        notes[1]
    );
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        3,
        "fail twice, then succeed on the third call"
    );
}

#[tokio::test(start_paused = true)]
async fn complete_with_retry_records_error_stats_per_attempt() {
    // R21 (round-1 review LOW 4): every failed complete_with_retry attempt
    // re-sends the full prompt — each must land a request_stats row with
    // outcome='error' and cached_tokens NULL (the provider never reported
    // usage), so error→retry cycles (and a LiteLLM fallback switch, which
    // guarantees a cold next request) are visible to the cache aggregates.
    // The motivating case is trace id 255: the request died at BadGateway
    // before any SSE chunk and left no row at all.
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());
    let embedder: Arc<dyn crate::memory::Embedder> = Arc::new(HashEmbedder::new());
    let store: Arc<dyn crate::memory::MemoryStoreTrait> =
        Arc::new(MemoryStore::open_in_memory(embedder).unwrap());
    let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let provider: Arc<dyn LlmClient> = Arc::new(FlakyProvider {
        caps: Capabilities::openai(),
        calls: Arc::clone(&calls),
        fail_times: 2,
        backoffs: Arc::new(std::sync::Mutex::new(Vec::new())),
        success_usage: Some(LlmEvent::Usage {
            prompt_tokens: 900,
            completion_tokens: 40,
            reasoning_tokens: 0,
            cached_tokens: 0,
            ttft_ms: Some(120),
            generation_ms: Some(500),
        }),
    });
    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider,
            tools: registry,
            workflow: workflow,
            sandbox: sandbox,
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(128_000, 0.5),
            memory: Some(store.clone()),
            vision: None,
        },
        crate::project::Constitution::default(),
    );

    let (fanin_tx, _fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let session = store.start_session("test").await.unwrap();
    let sid = session.id.clone();
    let mut messages = vec![Message::user_text("hello")];
    // Driven through run_turn so the success stream is consumed by
    // consume_stream (the success row's recording site) while the two failed
    // attempts record inside complete_with_retry.
    agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, Some(&sid))
        .await
        .unwrap();

    // The stats recording is fire-and-forget (spawned). Give it a moment.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let rows = store.request_stats_rows(&sid).await.unwrap();
    assert_eq!(rows.len(), 3, "two error rows + one success row");
    let errors: Vec<_> = rows
        .iter()
        .filter(|r| r.outcome.as_deref() == Some("error"))
        .collect();
    assert_eq!(errors.len(), 2, "one error row per failed attempt");
    for row in &errors {
        assert_eq!(row.cached_tokens, None, "cached_tokens is NULL, not 0");
        assert_eq!(row.completion_tokens, 0);
        assert!(row.prompt_tokens > 0, "our estimate is recorded");
        assert_eq!(row.purpose, None);
        assert_eq!(row.model, "mock-flaky");
    }
    let success = rows
        .iter()
        .find(|r| r.outcome.is_none())
        .expect("the success row");
    assert_eq!(success.prompt_tokens, 900);
    assert_eq!(success.completion_tokens, 40);
    assert_eq!(success.cached_tokens, Some(0));
    assert_eq!(success.ttft_ms, Some(120));
}

#[tokio::test(start_paused = true)]
async fn complete_with_retry_attributes_backoff_sleeps_to_next_attempt() {
    // Regression (trace-graph honesty): the 1s/2s backoff sleeps between
    // failed attempts used to be attributed to NO trace record — an
    // invisible gap between per-attempt columns in the graph. Now each
    // sleep is parked on the provider via `record_backoff_ms` right after
    // it elapses, so the NEXT attempt's record carries it as `backoff_ms`
    // (the waiting family in the graph). Paused clock: the sleeps
    // auto-advance instantly.
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());
    let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let backoffs: Arc<std::sync::Mutex<Vec<u32>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let provider: Arc<dyn LlmClient> = Arc::new(FlakyProvider {
        caps: Capabilities::openai(),
        calls: Arc::clone(&calls),
        fail_times: 2,
        backoffs: Arc::clone(&backoffs),
        success_usage: None,
    });
    let agent = AgentLoop::new(
        test_config(provider, registry, workflow, sandbox.clone()),
        crate::project::Constitution::default(),
    );
    let provider = agent.provider();

    let (fanin_tx, _fanin_rx) = mpsc::channel(8);
    let result = agent
        .complete_with_retry(&provider, &[], &[], &fanin_tx, 1, None)
        .await;
    assert!(result.is_ok(), "the third attempt must succeed");

    // The two sleeps were each parked for the NEXT attempt's record, with
    // equal jitter: attempt 1 ∈ [500, 1000] ms, attempt 2 ∈ [1000, 2000] ms
    // (half fixed + half random — see `retry_backoff_ms`).
    let parked = backoffs
        .lock()
        .expect("FlakyProvider backoffs lock poisoned")
        .clone();
    assert_eq!(parked.len(), 2, "one park per sleep: {parked:?}");
    assert!(
        (500..=1000).contains(&parked[0]),
        "attempt-1 backoff out of the equal-jitter band: {}",
        parked[0]
    );
    assert!(
        (1000..=2000).contains(&parked[1]),
        "attempt-2 backoff out of the equal-jitter band: {}",
        parked[1]
    );
}

#[tokio::test]
async fn auto_compaction_emits_compacting_phase_inside_sending() {
    // Auto-compaction runs a whole extra LLM call inside the "sending"
    // window — it must surface as its own live phase (Sending → Compacting
    // → Sending) so the inflight bar doesn't look stuck (2026-08-22 report).
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    let provider = Arc::new(MockProvider::single(vec![
        LlmEvent::TextDelta {
            text: "Hello!".into(),
        },
        LlmEvent::Finish {
            reason: FinishReason::Stop,
        },
    ]));
    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider,
            tools: registry,
            workflow,
            sandbox: sandbox.clone(),
            safety_mode: SafetyMode::Autonomous,
            // Tiny context window — summarize_at = 5 tokens; the user message
            // below is far over it, so the compaction branch runs this turn.
            context_manager: context::ContextManager::new(10, 0.5),
            memory: None,
            vision: None,
        },
        crate::project::Constitution::default(),
    );

    let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text(
        "this message is long enough to exceed the five-token summarization threshold",
    )];
    agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await
        .unwrap();

    let mut phases = Vec::new();
    while let Ok((_id, event)) = fanin_rx.try_recv() {
        if let AgentEvent::Phase { phase } = event {
            phases.push(phase);
        }
    }
    assert_eq!(
        phases,
        vec![
            PhaseKind::Sending,
            PhaseKind::Compacting,
            PhaseKind::Sending,
            PhaseKind::Waiting,
            PhaseKind::Streaming,
        ],
        "auto-compaction must surface as its own phase inside the sending window"
    );
}

#[tokio::test]
async fn auto_compaction_announces_only_once_per_turn() {
    // Review finding (2026-08-22): while the conversation stays above the
    // threshold, auto-compaction re-fires on EVERY turn-loop iteration — the
    // transcript announcements (CompactStarted + Compacted) must be emitted
    // only ONCE per turn, or a wedged-over-threshold turn spams start/end
    // note pairs. The transient inflight-bar Phase::Compacting indicator
    // still fires per iteration.
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    // First response: a tool call (so the loop iterates). Second: final text.
    // The tiny context window keeps every iteration over the threshold, so
    // the compaction branch runs twice (both no-ops: too few messages to
    // summarize) — only the first may announce.
    let provider = Arc::new(MockProvider::sequence(vec![
        vec![
            LlmEvent::ToolCallStart {
                index: 0,
                id: "call_1".into(),
                name: "file_read".into(),
            },
            LlmEvent::ToolCallArgumentDelta {
                index: 0,
                fragment: r#"{"path":"test.txt"}"#.into(),
            },
            LlmEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ],
        vec![
            LlmEvent::TextDelta {
                text: "Done reading.".into(),
            },
            LlmEvent::Finish {
                reason: FinishReason::Stop,
            },
        ],
    ]));
    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider,
            tools: registry,
            workflow,
            sandbox: sandbox.clone(),
            safety_mode: SafetyMode::Autonomous,
            // Tiny context window — summarize_at = 5 tokens; every iteration is
            // over it, so the compaction branch runs twice this turn.
            context_manager: context::ContextManager::new(10, 0.5),
            memory: None,
            vision: None,
        },
        crate::project::Constitution::default(),
    );

    let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text(
        "this message is long enough to exceed the five-token summarization threshold",
    )];
    agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await
        .unwrap();

    let mut started = 0u32;
    let mut compacted = 0u32;
    let mut compacting_phases = 0u32;
    while let Ok((_id, event)) = fanin_rx.try_recv() {
        match event {
            AgentEvent::CompactStarted => started += 1,
            AgentEvent::Compacted { .. } => compacted += 1,
            AgentEvent::Phase { phase } if phase == PhaseKind::Compacting => {
                compacting_phases += 1;
            }
            _ => {}
        }
    }
    assert_eq!(
        started, 1,
        "CompactStarted must announce only once per turn"
    );
    assert_eq!(compacted, 1, "Compacted must pair the single announcement");
    assert_eq!(
        compacting_phases, 2,
        "the transient inflight phase still fires per compaction"
    );
}

/// A mock provider whose `complete` always returns an error. Used to test
/// that `complete_with_retry` returns an error (not panics) after
/// exhausting retries.
struct FailingProvider {
    caps: Capabilities,
    /// How many times `complete` was called (to verify retry count).
    calls: Arc<std::sync::atomic::AtomicU32>,
}

#[async_trait]
impl LlmClient for FailingProvider {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
    fn kind(&self) -> ProviderKind {
        ProviderKind::OpenAI
    }
    fn model(&self) -> &str {
        "mock-failing"
    }
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _tool_choice: Option<crate::provider::ToolChoice>,
    ) -> crate::error::Result<BoxStream<'_, LlmEvent>> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(crate::error::Error::Provider("always fails".into()))
    }
}

#[tokio::test]
async fn complete_with_retry_returns_err_not_panic() {
    // After exhausting all retries, complete_with_retry must return an
    // Err (not panic via unreachable!()). The provider is called exactly
    // 3 times (the retry cap).
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());
    let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let provider: Arc<dyn LlmClient> = Arc::new(FailingProvider {
        caps: Capabilities::openai(),
        calls: Arc::clone(&calls),
    });
    let agent = AgentLoop::new(
        test_config(provider, registry, workflow, sandbox.clone()),
        crate::project::Constitution::default(),
    );

    // The provider is passed explicitly (the per-turn snapshot), so use a
    // clone of the Arc that built the agent (the original moved into `new`).
    // Bind it to a local so it lives across the await.
    let provider = agent.provider();
    let (fanin_tx, _fanin_rx) = mpsc::channel(8);
    let result = agent
        .complete_with_retry(&provider, &[], &[], &fanin_tx, 1, None)
        .await;

    // Must be an error, not a panic.
    match result {
        Err(err) => {
            assert!(
                err.to_string().contains("always fails"),
                "error should carry the provider's message; got: {err}"
            );
        }
        Ok(_) => panic!("expected Err after exhausting retries, got Ok"),
    }
    // The provider was called exactly 3 times (the retry cap).
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        3,
        "complete_with_retry should try 3 times"
    );
}

#[tokio::test]
async fn phase_waiting_emitted_on_provider_error_path() {
    // Regression (2026-12-04): Phase::Waiting must fire BEFORE the provider
    // request — even when the request fails. Previously Waiting was emitted
    // AFTER `stream_result?`, which is skipped on Err, so a 30s connect
    // timeout or a retry backoff sleep showed as "sending" in the inflight
    // bar. Now Waiting is emitted before complete_with_retry, so it fires on
    // every request attempt, success or failure.
    //
    // Uses the non-retryable ContextOverflowProvider, so complete_with_retry
    // short-circuits after a single provider call (no backoff sleeps) — the
    // test is instant.
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());
    let provider: Arc<dyn LlmClient> = Arc::new(ContextOverflowProvider {
        caps: Capabilities::openai(),
        calls: Arc::new(std::sync::atomic::AtomicU32::new(0)),
    });
    let agent = AgentLoop::new(
        test_config(provider, registry, workflow, sandbox.clone()),
        crate::project::Constitution::default(),
    );

    let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text("hello")];

    let result = agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await;
    assert!(
        result.is_err(),
        "ContextOverflowProvider should produce Err"
    );

    // Drain the fanned-in events and collect phase transitions.
    let mut phases = Vec::new();
    while let Ok((_id, event)) = fanin_rx.try_recv() {
        if let AgentEvent::Phase { phase } = event {
            phases.push(phase);
        }
    }
    // Waiting MUST be present — before the fix it was never emitted on the
    // error path (it sat after `stream_result?` which is skipped on Err).
    assert!(
        phases.contains(&PhaseKind::Waiting),
        "Phase::Waiting must fire before the provider request, even on error; got phases: {phases:?}"
    );
    // Sending must precede Waiting (local prep → network handoff).
    let send_idx = phases.iter().position(|p| *p == PhaseKind::Sending);
    let wait_idx = phases.iter().position(|p| *p == PhaseKind::Waiting);
    assert!(
        send_idx.is_some() && wait_idx.is_some() && send_idx < wait_idx,
        "Sending must precede Waiting; got phases: {phases:?}"
    );
}

/// A provider that always returns a non-retryable context-overflow error.
struct ContextOverflowProvider {
    caps: Capabilities,
    /// How many times `complete` was called (to verify retry count).
    calls: Arc<std::sync::atomic::AtomicU32>,
}

#[async_trait]
impl LlmClient for ContextOverflowProvider {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
    fn kind(&self) -> ProviderKind {
        ProviderKind::OpenAI
    }
    fn model(&self) -> &str {
        "mock-overflow"
    }
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _tool_choice: Option<crate::provider::ToolChoice>,
    ) -> crate::error::Result<BoxStream<'_, LlmEvent>> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(crate::error::Error::Provider(
            "This model's maximum context length is 262144 tokens. However, you \
             requested 131072 output tokens and your prompt contains at least \
             131073 input tokens"
                .into(),
        ))
    }
}

#[tokio::test]
async fn complete_with_retry_does_not_retry_context_overflow() {
    // A context-overflow error is non-retryable: the prompt won't shrink
    // between attempts, so retrying just wastes 3×1s/2s backoff sleeps.
    // complete_with_retry must call the provider exactly ONCE and return
    // the error immediately (not 3 attempts with ~3s of sleeps).
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());
    let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let provider: Arc<dyn LlmClient> = Arc::new(ContextOverflowProvider {
        caps: Capabilities::openai(),
        calls: Arc::clone(&calls),
    });
    let agent = AgentLoop::new(
        test_config(provider, registry, workflow, sandbox.clone()),
        crate::project::Constitution::default(),
    );
    let provider = agent.provider();
    let (fanin_tx, _fanin_rx) = mpsc::channel(8);
    let result = agent
        .complete_with_retry(&provider, &[], &[], &fanin_tx, 1, None)
        .await;
    assert!(result.is_err(), "context overflow must produce Err");
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "non-retryable error must not be retried (1 call, not 3)"
    );
}

/// A provider that always returns a rate-limit (HTTP 429) error.
struct RateLimitProvider {
    caps: Capabilities,
    calls: Arc<std::sync::atomic::AtomicU32>,
}

#[async_trait]
impl LlmClient for RateLimitProvider {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
    fn kind(&self) -> ProviderKind {
        ProviderKind::OpenAI
    }
    fn model(&self) -> &str {
        "mock-429"
    }
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _tool_choice: Option<crate::provider::ToolChoice>,
    ) -> crate::error::Result<BoxStream<'_, LlmEvent>> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(crate::error::Error::Provider(
            "stream request: HTTP 429 from https://api.example.com/v1/chat/completions — \
             {\"error\":{\"message\":\"Too Many Requests\"}}"
                .into(),
        ))
    }
}

#[tokio::test]
async fn complete_with_retry_does_not_retry_rate_limit() {
    // A 429 (rate limit) must NOT burn the 3× same-provider retry stack — the
    // provider is out of quota, so retrying it never helps ("stop after a
    // single 429"). complete_with_retry must call the provider exactly ONCE
    // and return the error immediately (not 3 attempts with ~3s of sleeps).
    // The turn-level retry layer (run_turn_attempt) handles the recovery
    // (switching to the same model on a different endpoint).
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());
    let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let provider: Arc<dyn LlmClient> = Arc::new(RateLimitProvider {
        caps: Capabilities::openai(),
        calls: Arc::clone(&calls),
    });
    let agent = AgentLoop::new(
        test_config(provider, registry, workflow, sandbox.clone()),
        crate::project::Constitution::default(),
    );
    let provider = agent.provider();
    let (fanin_tx, _fanin_rx) = mpsc::channel(8);
    let result = agent
        .complete_with_retry(&provider, &[], &[], &fanin_tx, 1, None)
        .await;
    assert!(result.is_err(), "429 must produce Err");
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "rate-limited error must not be retried against the same provider (1 call, not 3)"
    );
}

/// A mid-stream provider error (e.g. a connection reset / unexpected EOF)
/// that arrives *before* any text or tool-call deltas must surface as an
/// `Err` from `run_turn` — not an `Ok` with empty output — so the outer
/// `run_turn_with_retry` layer retries the whole turn with backoff. The
/// turn must NOT emit a final `AgentEvent::Error { retrying: false }` on
/// this path (the outer layer owns retry messaging).
#[tokio::test]
async fn midstream_error_with_no_output_returns_err() {
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());
    // The stream emits a single Error event (simulating a connection reset
    // before any data) and then ends.
    let provider = Arc::new(MockProvider::single(vec![LlmEvent::Error {
        error: "stream error (HTTP 200): connection reset (os error 10054)".into(),
    }]));
    let agent = AgentLoop::new(
        test_config(provider, registry, workflow, sandbox.clone()),
        crate::project::Constitution::default(),
    );

    let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text("hi")];

    let result = agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await;

    // Must be an Err so the outer retry layer kicks in.
    match &result {
        Err(e) => {
            assert!(
                e.to_string().contains("connection reset"),
                "error should carry the stream error message; got: {e}"
            );
        }
        Ok(_) => panic!("expected Err for a mid-stream error with no output, got Ok"),
    }

    // No final (non-retrying) error event should have been emitted — the
    // outer `run_turn_with_retry` layer owns retry messaging. Drain the
    // fan-in channel and assert no `Error { retrying: false }` arrived.
    let mut saw_final_error = false;
    while let Ok((_id, event)) = fanin_rx.try_recv() {
        if let AgentEvent::Error { retrying, .. } = event {
            if !retrying {
                saw_final_error = true;
            }
        }
    }
    assert!(
        !saw_final_error,
        "run_turn must not emit a final Error event for a retryable mid-stream failure"
    );
}

/// A mid-stream error that arrives AFTER partial text output must NOT be
/// retried (the partial content is usable) and must NOT kill the turn.
/// Instead it surfaces a non-fatal note (`retrying: true`) so the user sees
/// the stream was truncated, and the turn returns `Ok` with the partial text.
#[tokio::test]
async fn midstream_error_after_partial_output_surfaces_note_and_keeps_text() {
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());
    // The stream emits some text, then an error (truncation), then ends.
    let provider = Arc::new(MockProvider::single(vec![
        LlmEvent::TextDelta {
            text: "partial response".into(),
        },
        LlmEvent::Error {
            error: "stream error (HTTP 200): connection reset (os error 10054)".into(),
        },
    ]));
    let agent = AgentLoop::new(
        test_config(provider, registry, workflow, sandbox.clone()),
        crate::project::Constitution::default(),
    );

    let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text("hi")];

    let outcome = agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await
        .expect("turn should succeed with partial output, not return Err");

    // The partial text is preserved.
    assert_eq!(outcome.text, "partial response");

    // A non-fatal (retrying: true) error note was emitted so the truncation
    // is visible. No final (retrying: false) error.
    let mut saw_nonfatal_note = false;
    let mut saw_final_error = false;
    while let Ok((_id, event)) = fanin_rx.try_recv() {
        if let AgentEvent::Error { retrying, .. } = event {
            if retrying {
                saw_nonfatal_note = true;
            } else {
                saw_final_error = true;
            }
        }
    }
    assert!(
        saw_nonfatal_note,
        "a non-fatal truncation note should be emitted"
    );
    assert!(!saw_final_error, "no final error should stop the agent");
}

#[tokio::test]
async fn safety_rule_auto_approves_in_approve_each_mode() {
    // In ApproveEachAction mode, a file_write call normally requires an
    // approval prompt. But if a safety rule matches the call's signature,
    // the prompt must be skipped and the tool executed directly.
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    // Write tools are only dispatch-allowed while Executing.
    {
        let mut wf = workflow.lock().await;
        wf.create_plan("t", "g", "c", vec!["write ok.txt".into()])
            .unwrap();
    }
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    // Build a safety-rules store with a rule matching file_write to "ok.txt".
    let safety_path = dir.path().join("safety.toml");
    let safety_rules = Arc::new(crate::safety_rules::SafetyRules::new(&safety_path).unwrap());
    safety_rules
        .add_rule("file_write", &serde_json::json!({"path": "ok.txt"}))
        .unwrap();

    let provider = Arc::new(MockProvider::sequence(vec![
        vec![
            LlmEvent::ToolCallStart {
                index: 0,
                id: "call_1".into(),
                name: "file_write".into(),
            },
            LlmEvent::ToolCallArgumentDelta {
                index: 0,
                fragment: r#"{"path":"ok.txt","content":"hi"}"#.into(),
            },
            LlmEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ],
        vec![LlmEvent::Finish {
            reason: FinishReason::Stop,
        }],
    ]));

    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider: provider,
            tools: registry,
            workflow: workflow,
            sandbox: sandbox.clone(),
            safety_mode: SafetyMode::ApproveEachAction,
            context_manager: context::ContextManager::new(128_000, 0.5),
            memory: None,
            vision: None,
        },
        crate::project::Constitution::default(),
    )
    .with_safety_rules(safety_rules);

    let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text("write ok.txt")];

    let _outcome = agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await
        .unwrap();

    // Drain events Î“Ã‡Ã¶ none should be an ApprovalRequest (the rule
    // auto-approved the call).
    let mut saw_approval = false;
    let mut saw_tool_result = false;
    while let Ok(Some((_id, event))) =
        tokio::time::timeout(std::time::Duration::from_millis(200), fanin_rx.recv()).await
    {
        if matches!(event, AgentEvent::ApprovalRequest { .. }) {
            saw_approval = true;
        }
        if matches!(event, AgentEvent::ToolResult { .. }) {
            saw_tool_result = true;
        }
    }
    assert!(
        !saw_approval,
        "a matching safety rule must skip the approval prompt"
    );
    assert!(
        saw_tool_result,
        "the tool should still have executed and emitted a ToolResult"
    );

    // The file was actually written (the tool ran without approval).
    assert_eq!(
        std::fs::read_to_string(dir.path().join("ok.txt")).unwrap(),
        "hi"
    );
}

#[tokio::test]
async fn safety_rule_non_match_still_prompts_in_approve_each_mode() {
    // A safety rule that does NOT match must fall through to the normal
    // approval prompt. We verify by checking that an ApprovalRequest is
    // emitted (and the call is left pending, since no one answers it).
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    // Write tools are only dispatch-allowed while Executing.
    {
        let mut wf = workflow.lock().await;
        wf.create_plan("t", "g", "c", vec!["write other.txt".into()])
            .unwrap();
    }
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    // A rule for "ok.txt" Î“Ã‡Ã¶ but the call writes "other.txt".
    let safety_path = dir.path().join("safety.toml");
    let safety_rules = Arc::new(crate::safety_rules::SafetyRules::new(&safety_path).unwrap());
    safety_rules
        .add_rule("file_write", &serde_json::json!({"path": "ok.txt"}))
        .unwrap();

    let provider = Arc::new(MockProvider::sequence(vec![
        vec![
            LlmEvent::ToolCallStart {
                index: 0,
                id: "call_1".into(),
                name: "file_write".into(),
            },
            LlmEvent::ToolCallArgumentDelta {
                index: 0,
                fragment: r#"{"path":"other.txt","content":"x"}"#.into(),
            },
            LlmEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ],
        vec![LlmEvent::Finish {
            reason: FinishReason::Stop,
        }],
    ]));

    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider: provider,
            tools: registry,
            workflow: workflow,
            sandbox: sandbox.clone(),
            safety_mode: SafetyMode::ApproveEachAction,
            context_manager: context::ContextManager::new(128_000, 0.5),
            memory: None,
            vision: None,
        },
        crate::project::Constitution::default(),
    )
    .with_safety_rules(safety_rules);

    let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
    // A command channel that never answers Î“Ã‡Ã¶ the approval will hang until
    // the channel closes (ChannelClosed), which is fine; we just need to
    // observe that an ApprovalRequest was emitted.
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text("write other.txt")];

    // Run with a timeout so the test can't hang forever if the approval
    // never resolves.
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        agent.run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None),
    )
    .await;

    // An ApprovalRequest must have been emitted (the rule didn't match).
    let mut saw_approval = false;
    while let Ok(Some((_id, event))) =
        tokio::time::timeout(std::time::Duration::from_millis(200), fanin_rx.recv()).await
    {
        if matches!(event, AgentEvent::ApprovalRequest { .. }) {
            saw_approval = true;
            break;
        }
    }
    assert!(
        saw_approval,
        "a non-matching call must still emit an ApprovalRequest"
    );
}

#[tokio::test]
async fn describe_image_errors_when_no_vision_client() {
    // When no vision client is configured, describe_image must return an
    // error (not panic). The caller should check is_multimodal() first.
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());
    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider: Arc::new(MockProvider::single(vec![LlmEvent::Finish {
                reason: FinishReason::Stop,
            }])),
            tools: registry,
            workflow,
            sandbox: sandbox.clone(),
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(128_000, 0.5),
            memory: None,
            vision: None, // no vision client
        },
        crate::project::Constitution::default(),
    );
    let result = agent
        .describe_image("https://example.com/img.png", "describe")
        .await;
    assert!(
        result.is_err(),
        "describe_image should error without a vision client"
    );
    assert!(
        result.unwrap_err().to_string().contains("no vision client"),
        "error should mention no vision client"
    );
}

#[test]
fn is_multimodal_reflects_provider_caps() {
    // is_multimodal() must reflect the provider's capability set. A mock
    // provider with multimodal=false Î“Ã¥Ã† is_multimodal() == false; with
    // multimodal=true Î“Ã¥Ã† true.
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    // Non-multimodal provider.
    let mut caps = Capabilities::openai();
    caps.multimodal = false;
    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider: Arc::new(MockProvider {
                responses: Arc::new(Mutex::new(std::collections::VecDeque::new())),
                caps,
                tools_phases: Arc::new(std::sync::Mutex::new(Vec::new())),
                name: String::new(),
            }),
            tools: registry.clone(),
            workflow: workflow.clone(),
            sandbox: sandbox.clone(),
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(128_000, 0.5),
            memory: None,
            vision: None,
        },
        crate::project::Constitution::default(),
    );
    assert!(
        !agent.is_multimodal(),
        "is_multimodal should be false when caps.multimodal is false"
    );

    // Multimodal provider.
    let mut caps = Capabilities::openai();
    caps.multimodal = true;
    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider: Arc::new(MockProvider {
                responses: Arc::new(Mutex::new(std::collections::VecDeque::new())),
                caps,
                tools_phases: Arc::new(std::sync::Mutex::new(Vec::new())),
                name: String::new(),
            }),
            tools: registry,
            workflow,
            sandbox,
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(128_000, 0.5),
            memory: None,
            vision: None,
        },
        crate::project::Constitution::default(),
    );
    assert!(
        agent.is_multimodal(),
        "is_multimodal should be true when caps.multimodal is true"
    );
}

/// Shared helper: AgentLoop + channels wired for direct `execute_tool_call`
/// tests (no full turn / provider round-trip).
fn make_dispatch_fixture(
    workflow: Arc<tokio::sync::Mutex<Workflow>>,
    sandbox: Arc<Sandbox>,
) -> (
    AgentLoop,
    mpsc::Sender<(crate::runtime::AgentId, AgentEvent)>,
    mpsc::Receiver<crate::runtime::AgentCommand>,
) {
    let registry = make_registry((*sandbox).clone(), workflow.clone());
    let provider = Arc::new(MockProvider {
        responses: Arc::new(Mutex::new(std::collections::VecDeque::new())),
        caps: Capabilities::openai(),
        tools_phases: Arc::new(std::sync::Mutex::new(Vec::new())),
        name: String::new(),
    });
    let agent = AgentLoop::new(
        test_config(provider, registry, workflow, sandbox),
        crate::project::Constitution::default(),
    );
    let (fanin_tx, _fanin_rx) = mpsc::channel(8);
    let (_cmd_tx, cmd_rx) = mpsc::channel(8);
    (agent, fanin_tx, cmd_rx)
}

#[tokio::test]
async fn dispatch_denies_write_tool_in_planning() {
    // Quality H1: ToolFilter is re-checked at dispatch. In Planning, write
    // tools are omitted from the schema — but a hallucinated `file_write`
    // must still be rejected before approval or execution.
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let (agent, fanin_tx, mut cmd_rx) = make_dispatch_fixture(workflow, sandbox);

    let tc = crate::provider::ToolCall::new("c1", "file_write", r#"{"path":"x.txt","content":"nope"}"#);
    let mut deny_all_latched = false;
    let mut stop_signal: Option<super::StopReason> = None;
    let (result, buffered) = agent
        .execute_tool_call(
            &tc,
            &fanin_tx,
            1,
            &mut cmd_rx,
            &mut deny_all_latched,
            &mut stop_signal,
        )
        .await;

    assert!(!result.success, "Planning must deny file_write at dispatch");
    assert!(
        result.output.contains("not allowed") && result.output.contains("Planning"),
        "error should name the denial + workflow state, got: {}",
        result.output
    );
    assert!(buffered.is_empty());
    // Side-effect free: the file must not have been created.
    assert!(
        !dir.path().join("x.txt").exists(),
        "denied write must not create the file"
    );
}

#[tokio::test]
async fn dispatch_denies_finish_for_subagent_even_in_reviewing() {
    // `finish` is main-agent-only (like the plan-mutation tools): a subagent
    // spawned during the parent's Reviewing state must not be able to close
    // out the review — it shares the parent's plan, so the Reviewing
    // ToolFilter alone would otherwise admit finish.
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    // Drive the workflow into Reviewing (the state where finish is visible).
    {
        let mut wf = workflow.lock().await;
        wf.create_plan("T", "G", "C", vec!["a".into()]).unwrap();
        wf.complete_step(0).unwrap();
        assert_eq!(wf.state(), crate::workflow::WorkflowState::Reviewing);
    }
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let (agent, fanin_tx, mut cmd_rx) = make_dispatch_fixture(workflow, sandbox);
    // Mark the loop as a sub-agent (plan mutations denied).
    agent.set_plan_mutations_allowed(false);

    let tc = crate::provider::ToolCall::new("c1", "finish", r#"{"review_report":"/tmp/report.md"}"#);
    let mut deny_all_latched = false;
    let mut stop_signal: Option<super::StopReason> = None;
    let (result, _) = agent
        .execute_tool_call(
            &tc,
            &fanin_tx,
            1,
            &mut cmd_rx,
            &mut deny_all_latched,
            &mut stop_signal,
        )
        .await;

    assert!(!result.success, "subagent must not be able to call finish");
    assert!(
        result.output.contains("restricted to the main agent"),
        "error should name the main-agent-only gate, got: {}",
        result.output
    );
    // The review is not closed out.
    let wf = agent.workflow_handle();
    let wf = wf.lock().await;
    assert_eq!(
        wf.state(),
        crate::workflow::WorkflowState::Reviewing,
        "finish must not have transitioned the workflow"
    );
}

#[tokio::test]
async fn dispatch_denies_reviewer_respawn_while_failure_pending() {
    // Failed-reviewer protocol: while the reviewer_failure_pending latch is
    // set (a reviewer child failed with no report), a spawn_agent call with
    // role:"reviewer" must be denied at dispatch — the parent must ask the
    // user first instead of blindly respawning the same failing reviewer.
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let (agent, fanin_tx, mut cmd_rx) = make_dispatch_fixture(workflow, sandbox);
    agent.set_reviewer_failure_pending(true);

    let tc = crate::provider::ToolCall::new("c1", "spawn_agent", r#"{"name":"reviewer","task":"review the diff","role":"reviewer"}"#);
    let mut deny_all_latched = false;
    let mut stop_signal: Option<super::StopReason> = None;
    let (result, _) = agent
        .execute_tool_call(
            &tc,
            &fanin_tx,
            1,
            &mut cmd_rx,
            &mut deny_all_latched,
            &mut stop_signal,
        )
        .await;

    assert!(
        !result.success,
        "reviewer respawn must be denied while failure is pending"
    );
    assert!(
        result.output.contains("ask_user") && result.output.contains("do NOT respawn"),
        "error should instruct asking the user, got: {}",
        result.output
    );
}

#[tokio::test]
async fn dispatch_clears_failure_pending_on_ask_user() {
    // The failed-reviewer latch clears when the agent asks the user — after
    // that, a reviewer spawn is allowed again (whatever the user decided).
    use crate::tool::workflow::ask_user::AskUserTool;

    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    // Registry with ask_user registered (so dispatch's interception arm runs
    // the tool instead of erroring on an unknown tool).
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(AskUserTool::new()));
    let provider: Arc<dyn LlmClient> = Arc::new(MockProvider {
        responses: Arc::new(Mutex::new(std::collections::VecDeque::new())),
        caps: Capabilities::openai(),
        tools_phases: Arc::new(std::sync::Mutex::new(Vec::new())),
        name: String::new(),
    });
    let agent = Arc::new(AgentLoop::new(
        AgentLoopConfig {
            provider: provider,
            tools: Arc::new(registry),
            workflow: workflow,
            sandbox: sandbox,
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(128_000, 0.5),
            memory: None,
            vision: None,
        },
        crate::project::Constitution::default(),
    ));
    agent.set_reviewer_failure_pending(true);
    assert!(
        agent.reviewer_failure_pending(),
        "latch set before ask_user"
    );

    let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let tc = crate::provider::ToolCall::new(
        "q1",
        "ask_user",
        serde_json::json!({
            "question": "The review failed — how should we proceed?",
            "options": [{"label": "Retry on another model"}, {"label": "Abandon"}]
        })
        .to_string(),
    );
    // Drive the ask_user call in a task (it blocks on the oneshot).
    let agent_clone = Arc::clone(&agent);
    let q_handle = tokio::spawn(async move {
        let mut deny_all_latched = false;
        let mut stop_signal: Option<super::StopReason> = None;
        agent_clone
            .execute_tool_call(
                &tc,
                &fanin_tx,
                1,
                &mut cmd_rx,
                &mut deny_all_latched,
                &mut stop_signal,
            )
            .await
    });
    // The interception arm clears the latch BEFORE emitting the question —
    // answer it and confirm both the answer and the cleared latch.
    let (_qid, _options, responder) = recv_question(&mut fanin_rx).await;
    responder
        .send(crate::runtime::UserAnswer::Choice { index: 0 })
        .unwrap();
    let (result, _) = q_handle.await.unwrap();
    assert!(result.success, "ask_user should succeed");
    assert!(
        !agent.reviewer_failure_pending(),
        "asking the user clears the failed-reviewer latch"
    );
    // Backlog c8e48f81: asking while a failure was pending is the protocol's
    // user consultation — it opens the one-spawn retry sanction (the respawn
    // may carry the user-picked model).
    assert!(
        agent.reviewer_retry_sanctioned(),
        "ask_user while a failure was pending opens the retry sanction"
    );
}

#[tokio::test]
async fn dispatch_denies_ad_hoc_reviewer_model_pick() {
    // Backlog c8e48f81, end-to-end wiring pin (review L4): the gate-2 denial
    // must fire through execute_tool_call — the JSON arg extraction
    // (role/model keys) and the sanction store-back, not just the gate
    // function. The 2026-12-30 session invented per-reviewer "model
    // diversity" (glm-5.2/glm-5.3-gcp/deepseek-v4-flash); the deepseek
    // reviewer failed with no report. The fixture runs Autonomous
    // (spawn_agent is not a core op), so a call that passes the gate
    // proceeds without an approval pause.
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let (agent, fanin_tx, mut cmd_rx) = make_dispatch_fixture(workflow, sandbox);

    let call = |id: &str, model: &str| {
        crate::provider::ToolCall::new(
            id,
            "spawn_agent",
            format!(
                r#"{{"name":"reviewer","task":"review the diff","role":"reviewer","model":"{model}"}}"#
            ),
        )
    };
    let mut deny_all_latched = false;
    let mut stop_signal: Option<super::StopReason> = None;

    // Ad-hoc pick: denied at dispatch with the reviewer-model guidance.
    let (result, _) = agent
        .execute_tool_call(
            &call("c1", "deepseek-v4-flash"),
            &fanin_tx,
            1,
            &mut cmd_rx,
            &mut deny_all_latched,
            &mut stop_signal,
        )
        .await;
    assert!(
        !result.success && result.output.contains("must OMIT the model parameter"),
        "ad-hoc reviewer model must be dispatch-denied, got: {}",
        result.output
    );

    // The sanctioned retry (ask_user ran while a failure was pending — see
    // dispatch_clears_failure_pending_on_ask_user): the same call passes the
    // gate (whatever the tool itself does next in this fixture) and the
    // sanction is consumed.
    agent.set_reviewer_retry_sanctioned(true);
    let (result, _) = agent
        .execute_tool_call(
            &call("c2", "glm-5.3-gcp"),
            &fanin_tx,
            1,
            &mut cmd_rx,
            &mut deny_all_latched,
            &mut stop_signal,
        )
        .await;
    assert!(
        !result.output.contains("must OMIT the model parameter"),
        "the sanctioned retry must pass the model gate, got: {}",
        result.output
    );
    assert!(
        !agent.reviewer_retry_sanctioned(),
        "the retry window closes with the spawn (sanction consumed)"
    );

    // A third model-carrying spawn is ad-hoc again — denied.
    let (result, _) = agent
        .execute_tool_call(
            &call("c3", "glm-5.3-gcp"),
            &fanin_tx,
            1,
            &mut cmd_rx,
            &mut deny_all_latched,
            &mut stop_signal,
        )
        .await;
    assert!(
        !result.success && result.output.contains("must OMIT the model parameter"),
        "the sanction does not persist past one spawn, got: {}",
        result.output
    );
}

/// A mock descendant tracker whose "running descendants" answer is fixed at
/// construction. Used to exercise the dispatch state-transition gate.
struct FixedDescendantTracker {
    running: bool,
}

#[async_trait]
impl crate::runtime::DescendantTracker for FixedDescendantTracker {
    async fn has_running_descendants(&self, _agent_id: u64) -> bool {
        self.running
    }
}

#[tokio::test]
async fn dispatch_allows_parallel_work_tools_while_descendants_running() {
    // Review-exit gate (backlog 569b5922): only the transitions that END a
    // workflow phase are gated on running descendants. create_plan (a
    // Planning → Executing push) and abandon_plan (the designated
    // failed-review escape hatch) do not end a review phase — parallel work
    // (spawned reviewers/coders) must be able to push plans and escape a
    // wedged review while children run.
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());
    let provider = Arc::new(MockProvider {
        responses: Arc::new(Mutex::new(std::collections::VecDeque::new())),
        caps: Capabilities::openai(),
        tools_phases: Arc::new(std::sync::Mutex::new(Vec::new())),
        name: String::new(),
    });
    let agent = AgentLoop::new(
        test_config(provider, registry, workflow.clone(), sandbox),
        crate::project::Constitution::default(),
    )
    .with_agent_id(1)
    .with_descendant_tracker(Arc::new(FixedDescendantTracker { running: true }));

    let (fanin_tx, _fanin_rx) = mpsc::channel(8);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut deny_all_latched = false;
    let mut stop_signal: Option<super::StopReason> = None;

    // create_plan must proceed even while descendants run: pushing a plan
    // (or sub-plan) is parallel work, not a phase exit.
    let tc = crate::provider::ToolCall::new(
        "c1",
        "create_plan",
        r#"{"title":"T","goal":"G","context":"Dispatch fixture context: edit src/widget.rs and verify with cargo test.","steps":["edit src/widget.rs"]}"#,
    );
    let (result, _) = agent
        .execute_tool_call(
            &tc,
            &fanin_tx,
            1,
            &mut cmd_rx,
            &mut deny_all_latched,
            &mut stop_signal,
        )
        .await;
    assert!(
        result.success,
        "create_plan must proceed while descendants run, got: {}",
        result.output
    );
    {
        let wf = workflow.lock().await;
        assert_eq!(
            wf.state(),
            crate::workflow::WorkflowState::Executing,
            "create_plan must have transitioned Planning → Executing"
        );
    }

    // abandon_plan must proceed too: it is the escape hatch, and a hung
    // reviewer (a running descendant) must not block the escape.
    let tc = crate::provider::ToolCall::new("c2", "abandon_plan", "{}");
    let (result, _) = agent
        .execute_tool_call(
            &tc,
            &fanin_tx,
            1,
            &mut cmd_rx,
            &mut deny_all_latched,
            &mut stop_signal,
        )
        .await;
    assert!(
        result.success,
        "abandon_plan must proceed while descendants run, got: {}",
        result.output
    );
    let wf = workflow.lock().await;
    assert_eq!(
        wf.state(),
        crate::workflow::WorkflowState::Planning,
        "abandon_plan must have popped the plan"
    );
}

#[tokio::test]
async fn dispatch_allows_nonfinal_complete_step_while_descendants_running() {
    // A non-final complete_step is a checklist tick, not a phase exit — a
    // 'spawn reviewer' step completes at spawn while the reviewer runs.
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    // Seed a 2-step implementation plan directly (state: Executing).
    workflow
        .lock()
        .await
        .create_plan("T", "G", "", vec!["a".into(), "b".into()])
        .unwrap();
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());
    let provider = Arc::new(MockProvider {
        responses: Arc::new(Mutex::new(std::collections::VecDeque::new())),
        caps: Capabilities::openai(),
        tools_phases: Arc::new(std::sync::Mutex::new(Vec::new())),
        name: String::new(),
    });
    let agent = AgentLoop::new(
        test_config(provider, registry, workflow.clone(), sandbox),
        crate::project::Constitution::default(),
    )
    .with_agent_id(1)
    .with_descendant_tracker(Arc::new(FixedDescendantTracker { running: true }));

    // Step 1 of 2 — non-final: must succeed while descendants run.
    let tc = crate::provider::ToolCall::new("c1", "complete_step", r#"{"step_index":1}"#);
    let (fanin_tx, _fanin_rx) = mpsc::channel(8);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut deny_all_latched = false;
    let mut stop_signal: Option<super::StopReason> = None;
    let (result, _) = agent
        .execute_tool_call(
            &tc,
            &fanin_tx,
            1,
            &mut cmd_rx,
            &mut deny_all_latched,
            &mut stop_signal,
        )
        .await;
    assert!(
        result.success,
        "non-final complete_step must proceed while descendants run, got: {}",
        result.output
    );
    let wf = workflow.lock().await;
    assert_eq!(
        wf.state(),
        crate::workflow::WorkflowState::Executing,
        "a non-final step must not exit Executing"
    );
    assert_eq!(
        wf.plan().map(|p| p.completed_count()),
        Some(1),
        "step 1 must be ticked"
    );
}

#[tokio::test]
async fn dispatch_blocks_final_complete_step_while_descendants_running() {
    // The FINAL complete_step exits Executing (root plan → Reviewing) —
    // parallel coders/reviewers must finish before the plan exits the
    // phase, so the call stays gated while descendants run.
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    {
        let mut wf = workflow.lock().await;
        wf.create_plan("T", "G", "", vec!["a".into(), "b".into()])
            .unwrap();
        wf.complete_step(0).unwrap();
    }
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());
    let provider = Arc::new(MockProvider {
        responses: Arc::new(Mutex::new(std::collections::VecDeque::new())),
        caps: Capabilities::openai(),
        tools_phases: Arc::new(std::sync::Mutex::new(Vec::new())),
        name: String::new(),
    });
    let agent = AgentLoop::new(
        test_config(provider, registry, workflow.clone(), sandbox),
        crate::project::Constitution::default(),
    )
    .with_agent_id(1)
    .with_descendant_tracker(Arc::new(FixedDescendantTracker { running: true }));

    // Step 2 of 2 — the final step: must be refused while descendants run.
    let tc = crate::provider::ToolCall::new("c1", "complete_step", r#"{"step_index":2}"#);
    let (fanin_tx, _fanin_rx) = mpsc::channel(8);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut deny_all_latched = false;
    let mut stop_signal: Option<super::StopReason> = None;
    let (result, _) = agent
        .execute_tool_call(
            &tc,
            &fanin_tx,
            1,
            &mut cmd_rx,
            &mut deny_all_latched,
            &mut stop_signal,
        )
        .await;
    assert!(
        !result.success,
        "final complete_step must be blocked while descendants run"
    );
    assert!(
        result.output.contains("still running"),
        "error should explain the gate, got: {}",
        result.output
    );

    // Whitespace-padded numeric strings are valid step_index args (the
    // tool's StepNumber::parse trims) — the gate's arg mirror must trim
    // too, or a padded FINAL step_index bypasses the gate (review L1).
    let tc = crate::provider::ToolCall::new("c2", "complete_step", r#"{"step_index":" 2 "}"#);
    let (result, _) = agent
        .execute_tool_call(
            &tc,
            &fanin_tx,
            1,
            &mut cmd_rx,
            &mut deny_all_latched,
            &mut stop_signal,
        )
        .await;
    assert!(
        !result.success,
        "padded final complete_step must be blocked while descendants run"
    );
    assert!(
        result.output.contains("still running"),
        "error should explain the gate, got: {}",
        result.output
    );
    let wf = workflow.lock().await;
    assert_eq!(
        wf.state(),
        crate::workflow::WorkflowState::Executing,
        "the plan must not have exited Executing"
    );
    assert_eq!(
        wf.plan().map(|p| p.completed_count()),
        Some(1),
        "the final step must NOT be ticked by a gated call"
    );
}

#[tokio::test]
async fn dispatch_blocks_finish_while_descendants_running() {
    // finish (Reviewing → Complete) is always gated while descendants run:
    // the closing reviewer must land its report before the plan completes
    // (the non-empty-report finish gate is the second lock).
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    {
        let mut wf = workflow.lock().await;
        wf.create_plan("T", "G", "", vec!["a".into(), "b".into()])
            .unwrap();
        wf.complete_step(0).unwrap();
        wf.complete_step(1).unwrap();
    }
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());
    let provider = Arc::new(MockProvider {
        responses: Arc::new(Mutex::new(std::collections::VecDeque::new())),
        caps: Capabilities::openai(),
        tools_phases: Arc::new(std::sync::Mutex::new(Vec::new())),
        name: String::new(),
    });
    let agent = AgentLoop::new(
        test_config(provider, registry, workflow.clone(), sandbox),
        crate::project::Constitution::default(),
    )
    .with_agent_id(1)
    .with_descendant_tracker(Arc::new(FixedDescendantTracker { running: true }));

    let tc = crate::provider::ToolCall::new("c1", "finish", r#"{"review_report":"x.md"}"#);
    let (fanin_tx, _fanin_rx) = mpsc::channel(8);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut deny_all_latched = false;
    let mut stop_signal: Option<super::StopReason> = None;
    let (result, _) = agent
        .execute_tool_call(
            &tc,
            &fanin_tx,
            1,
            &mut cmd_rx,
            &mut deny_all_latched,
            &mut stop_signal,
        )
        .await;
    assert!(
        !result.success,
        "finish must be blocked while descendants run"
    );
    assert!(
        result.output.contains("still running"),
        "error should explain the descendant gate (not the report gate), got: {}",
        result.output
    );
    let wf = workflow.lock().await;
    assert_eq!(
        wf.state(),
        crate::workflow::WorkflowState::Reviewing,
        "the plan must not have completed"
    );
}

#[tokio::test]
async fn dispatch_allows_subplan_final_step_while_descendants_running() {
    // A sub-plan's FINAL step is not a phase exit: completing it pops to
    // the parent (still Executing), so it stays ungated while descendants
    // run — only the ROOT plan's completion exits Executing (review L2:
    // pin the stack.len() != 1 branch of the finality query).
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let root_id = {
        let mut wf = workflow.lock().await;
        let id = wf
            .create_plan("Root", "G", "", vec!["root-a".into(), "root-b".into()])
            .unwrap();
        // Push a sub-plan on top of the root plan (create_plan while a
        // plan is active pushes onto the stack).
        wf.create_plan("Sub", "G", "", vec!["sub-a".into(), "sub-b".into()])
            .unwrap();
        wf.complete_step(0).unwrap();
        id
    };
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());
    let provider = Arc::new(MockProvider {
        responses: Arc::new(Mutex::new(std::collections::VecDeque::new())),
        caps: Capabilities::openai(),
        tools_phases: Arc::new(std::sync::Mutex::new(Vec::new())),
        name: String::new(),
    });
    let agent = AgentLoop::new(
        test_config(provider, registry, workflow.clone(), sandbox),
        crate::project::Constitution::default(),
    )
    .with_agent_id(1)
    .with_descendant_tracker(Arc::new(FixedDescendantTracker { running: true }));

    // Sub-plan step 2 of 2 — final for the SUB-plan, but completing it
    // pops to the root plan (still Executing): must succeed while
    // descendants run.
    let tc = crate::provider::ToolCall::new("c1", "complete_step", r#"{"step_index":2}"#);
    let (fanin_tx, _fanin_rx) = mpsc::channel(8);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut deny_all_latched = false;
    let mut stop_signal: Option<super::StopReason> = None;
    let (result, _) = agent
        .execute_tool_call(
            &tc,
            &fanin_tx,
            1,
            &mut cmd_rx,
            &mut deny_all_latched,
            &mut stop_signal,
        )
        .await;
    assert!(
        result.success,
        "sub-plan final step must proceed while descendants run, got: {}",
        result.output
    );
    let wf = workflow.lock().await;
    assert_eq!(
        wf.state(),
        crate::workflow::WorkflowState::Executing,
        "popping to the parent plan stays Executing"
    );
    assert_eq!(
        wf.plan_id().as_deref(),
        Some(root_id.as_str()),
        "the sub-plan must have popped; the root plan is active again"
    );
}

#[tokio::test]
async fn dispatch_allows_state_transition_when_no_descendants_running() {
    // The companion: when the tracker reports NO running descendants, the
    // state-transition gate does not fire and create_plan proceeds normally.
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());
    let provider = Arc::new(MockProvider {
        responses: Arc::new(Mutex::new(std::collections::VecDeque::new())),
        caps: Capabilities::openai(),
        tools_phases: Arc::new(std::sync::Mutex::new(Vec::new())),
        name: String::new(),
    });
    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider: provider,
            tools: registry,
            workflow: workflow.clone(),
            sandbox: sandbox,
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(128_000, 0.5),
            memory: None,
            vision: None,
        },
        crate::project::Constitution::default(),
    )
    .with_agent_id(1)
    .with_descendant_tracker(Arc::new(FixedDescendantTracker { running: false }));

    let tc = crate::provider::ToolCall::new("c1", "create_plan", r#"{"title":"T","goal":"G","context":"Dispatch fixture context: edit src/widget.rs and verify with cargo test.","steps":["edit src/widget.rs"]}"#);
    let (fanin_tx, _fanin_rx) = mpsc::channel(8);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut deny_all_latched = false;
    let mut stop_signal: Option<super::StopReason> = None;
    let (result, _) = agent
        .execute_tool_call(
            &tc,
            &fanin_tx,
            1,
            &mut cmd_rx,
            &mut deny_all_latched,
            &mut stop_signal,
        )
        .await;

    assert!(
        result.success,
        "create_plan must proceed when no descendants run, got: {}",
        result.output
    );
    let wf = workflow.lock().await;
    assert_eq!(wf.state(), crate::workflow::WorkflowState::Executing);
}

#[tokio::test]
async fn dispatch_no_tracker_allows_state_transition() {
    // When no descendant tracker is wired (tests / no IPC spawner), the gate
    // is not enforced — create_plan proceeds. This is the backward-compat
    // path for tests that build a loop directly.
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let (agent, fanin_tx, mut cmd_rx) = make_dispatch_fixture(workflow.clone(), sandbox);

    let tc = crate::provider::ToolCall::new("c1", "create_plan", r#"{"title":"T","goal":"G","context":"Dispatch fixture context: edit src/widget.rs and verify with cargo test.","steps":["edit src/widget.rs"]}"#);
    let mut deny_all_latched = false;
    let mut stop_signal: Option<super::StopReason> = None;
    let (result, _) = agent
        .execute_tool_call(
            &tc,
            &fanin_tx,
            1,
            &mut cmd_rx,
            &mut deny_all_latched,
            &mut stop_signal,
        )
        .await;
    assert!(
        result.success,
        "no tracker → no gate, got: {}",
        result.output
    );
    let wf = workflow.lock().await;
    assert_eq!(wf.state(), crate::workflow::WorkflowState::Executing);
}

#[tokio::test]
async fn dispatch_denies_tool_outside_skill_allow_list() {
    // Skill allow-list is enforced at dispatch, not only schema visibility.
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    {
        let mut wf = workflow.lock().await;
        wf.start_skill(
            "narrow",
            "do the skill",
            crate::workflow::WorkflowState::Planning,
            vec!["file_read".into()],
        )
        .unwrap();
    }
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let (agent, fanin_tx, mut cmd_rx) = make_dispatch_fixture(workflow, sandbox);

    let tc = crate::provider::ToolCall::new("c1", "file_write", r#"{"path":"x.txt","content":"nope"}"#);
    let mut deny_all_latched = false;
    let mut stop_signal: Option<super::StopReason> = None;
    let (result, _) = agent
        .execute_tool_call(
            &tc,
            &fanin_tx,
            1,
            &mut cmd_rx,
            &mut deny_all_latched,
            &mut stop_signal,
        )
        .await;

    assert!(!result.success, "Skill allow-list must deny file_write");
    assert!(
        result.output.contains("not allowed") && result.output.contains("Skill"),
        "error should name the denial + Skill state, got: {}",
        result.output
    );
    assert!(!dir.path().join("x.txt").exists());
}

#[tokio::test]
async fn dispatch_allows_write_tool_in_executing() {
    // Executing still permits normal write tools (approval is separate; this
    // fixture uses Autonomous so the call runs without a prompt).
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    {
        let mut wf = workflow.lock().await;
        wf.create_plan("t", "g", "c", vec!["step one".into()])
            .unwrap();
    }
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let (agent, fanin_tx, mut cmd_rx) = make_dispatch_fixture(workflow, sandbox);

    let tc = crate::provider::ToolCall::new("c1", "file_write", r#"{"path":"ok.txt","content":"yes"}"#);
    let mut deny_all_latched = false;
    let mut stop_signal: Option<super::StopReason> = None;
    let (result, _) = agent
        .execute_tool_call(
            &tc,
            &fanin_tx,
            1,
            &mut cmd_rx,
            &mut deny_all_latched,
            &mut stop_signal,
        )
        .await;

    assert!(
        result.success,
        "Executing must allow file_write, got: {}",
        result.output
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("ok.txt")).unwrap(),
        "yes"
    );
}

/// A workflow with a single-step RESEARCH plan already active — the kind whose
/// file tools are denied by name, so only `.coding/**` artifacts pass dispatch.
async fn research_workflow(dir: &tempfile::TempDir) -> Arc<tokio::sync::Mutex<Workflow>> {
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    {
        let mut wf = workflow.lock().await;
        wf.create_plan_with_kind(
            "Investigate",
            "G",
            "C",
            vec!["step one".into()],
            crate::workflow::PlanKind::Research,
            None,
        )
        .unwrap();
    }
    workflow
}

#[tokio::test]
async fn dispatch_allows_research_artifact_write() {
    // The 2027-01-11 carve-out: a research plan may write its own ARTIFACTS
    // under `.coding/**` with the file tools. Before it, the deliverable had to
    // go through `shell` (bypassing the file tools' diff preview and path
    // safety) or the plan got misfiled as `implementation`.
    let dir = tempdir().unwrap();
    let workflow = research_workflow(&dir).await;
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let (agent, fanin_tx, mut cmd_rx) = make_dispatch_fixture(workflow, sandbox);

    let tc = crate::provider::ToolCall::new(
        "c1",
        "file_write",
        r#"{"path":".coding/analysis/notes.md","content":"findings"}"#,
    );
    let mut deny_all_latched = false;
    let mut stop_signal: Option<super::StopReason> = None;
    let (result, _) = agent
        .execute_tool_call(
            &tc,
            &fanin_tx,
            1,
            &mut cmd_rx,
            &mut deny_all_latched,
            &mut stop_signal,
        )
        .await;

    assert!(
        result.success,
        "a research plan must be able to write its own .coding/ artifact, got: {}",
        result.output
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join(".coding/analysis/notes.md")).unwrap(),
        "findings"
    );
}

#[tokio::test]
async fn dispatch_denies_research_source_write() {
    // The other half of the carve-out: source is still refused, and the error
    // names the boundary so the model does not retry blindly.
    let dir = tempdir().unwrap();
    let workflow = research_workflow(&dir).await;
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let (agent, fanin_tx, mut cmd_rx) = make_dispatch_fixture(workflow, sandbox);

    let tc = crate::provider::ToolCall::new(
        "c1",
        "file_write",
        r#"{"path":"src/lib.rs","content":"// sneaky"}"#,
    );
    let mut deny_all_latched = false;
    let mut stop_signal: Option<super::StopReason> = None;
    let (result, _) = agent
        .execute_tool_call(
            &tc,
            &fanin_tx,
            1,
            &mut cmd_rx,
            &mut deny_all_latched,
            &mut stop_signal,
        )
        .await;

    assert!(
        !result.success,
        "a research plan must not write source: {}",
        result.output
    );
    assert!(
        result.output.contains("research plan") && result.output.contains(".coding/"),
        "the denial should name the artifact boundary, got: {}",
        result.output
    );
    assert!(!dir.path().join("src/lib.rs").exists(), "side-effect free");
}

#[tokio::test]
async fn dispatch_denies_research_traversal_write() {
    // `.coding/../src/lib.rs` FOLDS to `src/lib.rs` while the raw argument still
    // starts with `.coding/` — a naive prefix check would have granted it. The
    // lexical resolution runs before the artifact test, so it stays denied.
    let dir = tempdir().unwrap();
    let workflow = research_workflow(&dir).await;
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let (agent, fanin_tx, mut cmd_rx) = make_dispatch_fixture(workflow, sandbox);

    let tc = crate::provider::ToolCall::new(
        "c1",
        "file_write",
        r#"{"path":".coding/../src/lib.rs","content":"// sneaky"}"#,
    );
    let mut deny_all_latched = false;
    let mut stop_signal: Option<super::StopReason> = None;
    let (result, _) = agent
        .execute_tool_call(
            &tc,
            &fanin_tx,
            1,
            &mut cmd_rx,
            &mut deny_all_latched,
            &mut stop_signal,
        )
        .await;

    assert!(
        !result.success,
        ".coding/../src/lib.rs must not pass the artifact check: {}",
        result.output
    );
    assert!(!dir.path().join("src/lib.rs").exists(), "side-effect free");
}

#[tokio::test]
async fn dispatch_denies_research_protected_write() {
    // The protected side-car entries stay out of the allowance: the file tools
    // refuse them anyway, and the carve-out must not become a second door.
    let dir = tempdir().unwrap();
    let workflow = research_workflow(&dir).await;
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let (agent, fanin_tx, mut cmd_rx) = make_dispatch_fixture(workflow, sandbox);

    let tc = crate::provider::ToolCall::new(
        "c1",
        "file_write",
        r#"{"path":".coding/plans/stack.json","content":"{}"}"#,
    );
    let mut deny_all_latched = false;
    let mut stop_signal: Option<super::StopReason> = None;
    let (result, _) = agent
        .execute_tool_call(
            &tc,
            &fanin_tx,
            1,
            &mut cmd_rx,
            &mut deny_all_latched,
            &mut stop_signal,
        )
        .await;

    assert!(
        !result.success,
        "protected .coding/plans files stay read-only: {}",
        result.output
    );
    // The message must name PROTECTION as the reason: an implementation
    // sub-plan does not unlock `.coding/plans/**`, so the research-boundary
    // advice would send the model after a fix that cannot work (review LOW-4).
    assert!(
        result.output.contains("protected"),
        "the denial must name protection, not the research boundary, got: {}",
        result.output
    );
    assert!(!dir.path().join(".coding/plans/stack.json").exists());
}

#[tokio::test]
async fn dispatch_denies_research_write_through_an_escaping_link() {
    // A symlink/junction planted inside `.coding/` that points OUT of the root
    // must not be granted: the canonical resolution is refused and the lexical
    // fallback is only trusted when no link component exists (review LOW-1).
    // Asserted end-to-end — the bytes must never reach the filesystem.
    let dir = tempdir().unwrap();
    let outside = tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".coding")).unwrap();
    let link = dir.path().join(".coding/link");
    #[cfg(unix)]
    let linked = std::os::unix::fs::symlink(outside.path(), &link).is_ok();
    #[cfg(windows)]
    let linked = {
        // `mklink /J` is a cmd builtin and mis-parses through Command's
        // quoting; New-Item's Junction type is the API-level spelling and needs
        // no elevation (a real symlink would need Developer Mode).
        let script = format!(
            "New-Item -ItemType Junction -Path '{}' -Target '{}' | Out-Null",
            link.display(),
            outside.path().display()
        );
        let junction = std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", script.as_str()])
            .output();
        match junction {
            Ok(out) if out.status.success() => true,
            Ok(out) => {
                eprintln!(
                    "junction fixture unavailable ({}): {}{}",
                    out.status,
                    String::from_utf8_lossy(&out.stdout).trim(),
                    String::from_utf8_lossy(&out.stderr).trim()
                );
                std::os::windows::fs::symlink_dir(outside.path(), &link).is_ok()
            }
            Err(_) => std::os::windows::fs::symlink_dir(outside.path(), &link).is_ok(),
        }
    };
    #[cfg(not(any(unix, windows)))]
    let linked = false;
    if !linked {
        eprintln!("SKIP: could not create a link for the escaping-link fixture");
        return;
    }
    let workflow = research_workflow(&dir).await;
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let (agent, fanin_tx, mut cmd_rx) = make_dispatch_fixture(workflow, sandbox);

    let tc = crate::provider::ToolCall::new(
        "c1",
        "file_write",
        r#"{"path":".coding/link/x.md","content":"escaped"}"#,
    );
    let mut deny_all_latched = false;
    let mut stop_signal: Option<super::StopReason> = None;
    let (result, _) = agent
        .execute_tool_call(
            &tc,
            &fanin_tx,
            1,
            &mut cmd_rx,
            &mut deny_all_latched,
            &mut stop_signal,
        )
        .await;

    assert!(
        !result.success,
        "a write through a link out of the root must be denied: {}",
        result.output
    );
    assert!(
        !outside.path().join("x.md").exists(),
        "the write must not follow the link out of the sandbox root"
    );
}

#[tokio::test]
async fn dispatch_allows_read_tool_on_skill_allow_list() {
    // Positive Skill case: a tool named on the allow-list still runs.
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("hello.txt"), "hi").unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    {
        let mut wf = workflow.lock().await;
        wf.start_skill(
            "narrow",
            "do the skill",
            crate::workflow::WorkflowState::Planning,
            vec!["file_read".into()],
        )
        .unwrap();
    }
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let (agent, fanin_tx, mut cmd_rx) = make_dispatch_fixture(workflow, sandbox);

    let tc = crate::provider::ToolCall::new("c1", "file_read", r#"{"path":"hello.txt"}"#);
    let mut deny_all_latched = false;
    let mut stop_signal: Option<super::StopReason> = None;
    let (result, _) = agent
        .execute_tool_call(
            &tc,
            &fanin_tx,
            1,
            &mut cmd_rx,
            &mut deny_all_latched,
            &mut stop_signal,
        )
        .await;

    assert!(
        result.success,
        "Skill allow-list must allow file_read, got: {}",
        result.output
    );
    assert!(result.output.contains("hi"), "should return file contents");
}

#[tokio::test]
async fn dispatch_approval_request_includes_file_write_preview() {
    // Quality H5: live ApprovalRequest must carry ApprovalPreview for file
    // tools (not always preview: None). ApproveEachAction so we hit the gate.
    use crate::provider::ApprovalPreview;
    use crate::runtime::Approval;

    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    {
        let mut wf = workflow.lock().await;
        wf.create_plan("t", "g", "c", vec!["step".into()]).unwrap();
    }
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());
    let provider = Arc::new(MockProvider {
        responses: Arc::new(Mutex::new(std::collections::VecDeque::new())),
        caps: Capabilities::openai(),
        tools_phases: Arc::new(std::sync::Mutex::new(Vec::new())),
        name: String::new(),
    });
    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider: provider,
            tools: registry,
            workflow: workflow,
            sandbox: sandbox,
            safety_mode: SafetyMode::ApproveEachAction,
            context_manager: context::ContextManager::new(128_000, 0.5),
            memory: None,
            vision: None,
        },
        crate::project::Constitution::default(),
    );

    let (fanin_tx, mut fanin_rx) = mpsc::channel(8);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);

    // Approver task: wait for ApprovalRequest, assert preview, approve.
    let approver = tokio::spawn(async move {
        while let Some((_id, event)) = fanin_rx.recv().await {
            if let AgentEvent::ApprovalRequest {
                preview,
                responder,
                tool_name,
                ..
            } = event
            {
                assert_eq!(tool_name, "file_write");
                match preview {
                    Some(ApprovalPreview::NewFile { path, content }) => {
                        assert!(
                            path.to_string_lossy().ends_with("previewed.txt")
                                || path.file_name().and_then(|s| s.to_str())
                                    == Some("previewed.txt"),
                            "path={path:?}"
                        );
                        assert_eq!(content, "hello preview");
                    }
                    other => panic!("expected Some(NewFile), got {other:?}"),
                }
                let _ = responder.send(Approval::Approve);
                return;
            }
        }
        panic!("fan-in closed without ApprovalRequest");
    });

    let tc = crate::provider::ToolCall::new("call_preview", "file_write", r#"{"path":"previewed.txt","content":"hello preview"}"#);
    let mut deny_all_latched = false;
    let mut stop_signal: Option<super::StopReason> = None;
    let (result, _) = agent
        .execute_tool_call(
            &tc,
            &fanin_tx,
            1,
            &mut cmd_rx,
            &mut deny_all_latched,
            &mut stop_signal,
        )
        .await;

    approver.await.expect("approver");
    assert!(
        result.success,
        "approved write should succeed: {}",
        result.output
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("previewed.txt")).unwrap(),
        "hello preview"
    );
}

#[tokio::test]
async fn deny_all_latches_and_skips_remaining_tool_calls() {
    // Quality M1: DenyAll must stop remaining tool calls in the same turn
    // without re-prompting (and without executing them).
    use crate::runtime::Approval;

    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    {
        let mut wf = workflow.lock().await;
        wf.create_plan("t", "g", "c", vec!["step".into()]).unwrap();
    }
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());
    let provider = Arc::new(MockProvider {
        responses: Arc::new(Mutex::new(std::collections::VecDeque::new())),
        caps: Capabilities::openai(),
        tools_phases: Arc::new(std::sync::Mutex::new(Vec::new())),
        name: String::new(),
    });
    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider: provider,
            tools: registry,
            workflow: workflow,
            sandbox: sandbox,
            safety_mode: SafetyMode::ApproveEachAction,
            context_manager: context::ContextManager::new(128_000, 0.5),
            memory: None,
            vision: None,
        },
        crate::project::Constitution::default(),
    );

    let (fanin_tx, mut fanin_rx) = mpsc::channel(16);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);

    // First approval → DenyAll; second must never reach the UI.
    let approver = tokio::spawn(async move {
        let mut saw = 0u32;
        while let Some((_id, event)) = fanin_rx.recv().await {
            if let AgentEvent::ApprovalRequest { responder, .. } = event {
                saw += 1;
                assert_eq!(saw, 1, "DenyAll must suppress later approval prompts");
                let _ = responder.send(Approval::DenyAll);
            }
        }
        saw
    });

    let mut deny_all_latched = false;
    let mut stop_signal: Option<super::StopReason> = None;
    let tc1 = crate::provider::ToolCall::new("c1", "file_write", r#"{"path":"a.txt","content":"one"}"#);
    let (r1, _) = agent
        .execute_tool_call(
            &tc1,
            &fanin_tx,
            1,
            &mut cmd_rx,
            &mut deny_all_latched,
            &mut stop_signal,
        )
        .await;
    assert!(!r1.success, "first call denied by DenyAll");
    assert!(deny_all_latched, "latch must be set after DenyAll");
    assert!(
        r1.output.contains("denied all"),
        "error should mention deny-all, got: {}",
        r1.output
    );

    let tc2 = crate::provider::ToolCall::new("c2", "file_write", r#"{"path":"b.txt","content":"two"}"#);
    let (r2, _) = agent
        .execute_tool_call(
            &tc2,
            &fanin_tx,
            1,
            &mut cmd_rx,
            &mut deny_all_latched,
            &mut stop_signal,
        )
        .await;
    assert!(!r2.success, "latched call must be skipped");
    assert!(
        r2.output.contains("skipped") || r2.output.contains("denied all"),
        "skipped error text, got: {}",
        r2.output
    );
    assert!(!dir.path().join("a.txt").exists());
    assert!(!dir.path().join("b.txt").exists());

    drop(fanin_tx);
    let saw = approver.await.expect("approver");
    assert_eq!(saw, 1, "only one ApprovalRequest should have been emitted");
}

#[test]
fn user_denial_output_helper_matches_dispatch_messages() {
    // Keep in sync with dispatch Denied / DeniedAll / skip / interrupt strings
    // so MAX_RETRIES does not count deliberate denials (review H1).
    assert!(crate::agent::turn_denial_for_test(
        "user denied the file_write call"
    ));
    assert!(crate::agent::turn_denial_for_test(
        "user denied all remaining actions (file_write call denied)"
    ));
    assert!(crate::agent::turn_denial_for_test(
        "user denied all remaining actions (file_write call skipped)"
    ));
    assert!(crate::agent::turn_denial_for_test(
        "interrupted while awaiting approval"
    ));
    assert!(!crate::agent::turn_denial_for_test(
        "path validation failed"
    ));
    assert!(!crate::agent::turn_denial_for_test("unknown tool 'x'"));
}

#[tokio::test]
async fn mid_turn_steer_soft_stops_and_carries_pending_steer() {
    // Steer fix: a steer arriving mid-stream must soft-stop the turn at the
    // next break point (keeping partial output) and carry the steer text as
    // `TurnOutcome.pending_steer` — NOT inject it as a System message. The
    // follow-up turn (driven by AgentTask) then runs the steer as a user
    // message. This test verifies the turn-driver half: the outcome carries
    // the steer + the partial text is kept.
    use crate::runtime::AgentCommand;
    use tokio::sync::Notify;

    /// A provider that yields one text chunk, then blocks on a Notify until the
    /// test signals it (so a steer can arrive mid-stream), then yields a Finish.
    /// On the SECOND call (the follow-up turn) it returns a plain stop.
    struct PausingProvider {
        gate: Arc<Notify>,
        second_call: StdMutex<bool>,
        caps: Capabilities,
    }

    #[async_trait::async_trait]
    impl LlmClient for PausingProvider {
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
        fn kind(&self) -> ProviderKind {
            ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            "mock"
        }
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _tool_choice: Option<crate::provider::ToolChoice>,
        ) -> crate::error::Result<BoxStream<'_, LlmEvent>> {
            use futures::StreamExt;
            let is_second = {
                let mut g = self.second_call.lock().unwrap();
                let was = *g;
                *g = true;
                was
            };
            if is_second {
                // Follow-up turn — just stop.
                return Ok(Box::pin(futures::stream::iter(vec![LlmEvent::Finish {
                    reason: FinishReason::Stop,
                }])));
            }
            // First turn: yield a chunk, then pause until the test signals.
            let gate = self.gate.clone();
            let stream = futures::stream::iter(vec![LlmEvent::TextDelta {
                text: "partial".into(),
            }])
            .chain(futures::stream::once(async move {
                gate.notified().await;
                LlmEvent::Finish {
                    reason: FinishReason::Stop,
                }
            }));
            Ok(Box::pin(stream))
        }
    }

    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    let gate = Arc::new(Notify::new());
    let provider: Arc<dyn LlmClient> = Arc::new(PausingProvider {
        gate: gate.clone(),
        second_call: StdMutex::new(false),
        caps: Capabilities::openai(),
    });
    let agent = Arc::new(AgentLoop::new(
        test_config(provider, registry, workflow, sandbox),
        crate::project::Constitution::default(),
    ));

    let (fanin_tx, _fanin_rx) = mpsc::channel(64);
    let (cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text("hello")];

    // Drive the turn in a task so we can send the steer mid-stream.
    let agent_clone = Arc::clone(&agent);
    let turn_handle = tokio::spawn(async move {
        agent_clone
            .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
            .await
            .unwrap()
    });

    // Give the stream a moment to emit the "partial" chunk + reach the pause.
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    // Send a steer mid-stream (while the stream is paused on the Notify).
    cmd_tx
        .send(AgentCommand::Suggestion("do this instead".into()))
        .await
        .unwrap();
    // Give the select! a moment to process the steer (set soft_stop).
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    // Release the pause so the stream can finish — the soft-stop fires at the
    // next break point (the stream completing).
    gate.notify_one();

    let outcome = turn_handle.await.unwrap();
    // The steer is carried as stop_reason (not injected as a System message).
    assert_eq!(
        outcome.stop_reason,
        Some(crate::agent::StopReason::Steer(vec![
            "do this instead".into()
        ])),
        "mid-turn steer must be carried as stop_reason"
    );
    // Partial output is kept (unlike a hard Interrupt).
    assert_eq!(outcome.text, "partial");
}

#[tokio::test]
async fn mid_stream_steer_drains_the_announced_tool_call() {
    // 2026-12-30 bug (interrupted turns silently drop in-flight tool calls):
    // a steer arriving mid-stream used to end the turn at the post-stream
    // stop block BEFORE the tool loop — the call the model just announced
    // never executed and got only a UI-only synthetic "interrupted: not run"
    // result, while the same steer arriving a moment later (after stream-end)
    // let the batch run. Sub-second timing decided whether calls executed at
    // all. Now a bare Steer falls through to the tool loop: the announced
    // call DRAINS (executes with a real result recorded in history) and the
    // steer drives the follow-up turn afterwards.
    use crate::runtime::AgentCommand;
    use tokio::sync::Notify;

    /// A provider that yields a tool-call start (args delta), then blocks on a
    /// Notify until the test signals it (so a steer can arrive mid-stream),
    /// then yields a Finish.
    struct SteerPausingProvider {
        gate: Arc<Notify>,
        caps: Capabilities,
    }

    #[async_trait::async_trait]
    impl LlmClient for SteerPausingProvider {
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
        fn kind(&self) -> ProviderKind {
            ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            "mock"
        }
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _tool_choice: Option<crate::provider::ToolChoice>,
        ) -> crate::error::Result<BoxStream<'_, LlmEvent>> {
            use futures::StreamExt;
            let gate = self.gate.clone();
            let stream = futures::stream::iter(vec![
                LlmEvent::ToolCallStart {
                    index: 0,
                    id: "call_rf".into(),
                    name: "read_files".into(),
                },
                LlmEvent::ToolCallArgumentDelta {
                    index: 0,
                    fragment: "{\"files\": []}".into(),
                },
            ])
            .chain(futures::stream::once(async move {
                gate.notified().await;
                LlmEvent::Finish {
                    reason: FinishReason::Stop,
                }
            }));
            Ok(Box::pin(stream))
        }
    }

    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    let gate = Arc::new(Notify::new());
    let provider: Arc<dyn LlmClient> = Arc::new(SteerPausingProvider {
        gate: gate.clone(),
        caps: Capabilities::openai(),
    });
    let agent = Arc::new(AgentLoop::new(
        test_config(provider, registry, workflow, sandbox),
        crate::project::Constitution::default(),
    ));

    let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
    let turn_tx = fanin_tx.clone();
    let (cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text("hello")];

    // Collect every fan-in event in the background (until all senders drop).
    let collect_handle = tokio::spawn(async move {
        let mut events: Vec<AgentEvent> = Vec::new();
        while let Some((_id, event)) = fanin_rx.recv().await {
            events.push(event);
        }
        events
    });

    // Drive the turn in a task so we can send the steer mid-stream. The task
    // hands the messages back too so the assertions below can check history.
    let agent_clone = Arc::clone(&agent);
    let turn_handle = tokio::spawn(async move {
        let outcome = agent_clone
            .run_turn(&mut messages, &turn_tx, 1, &mut cmd_rx, None)
            .await
            .unwrap();
        (outcome, messages)
    });

    // Give the stream a moment to emit the tool-call start + reach the pause.
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    // Send a steer mid-stream (while the stream is paused on the Notify).
    cmd_tx
        .send(AgentCommand::Suggestion("do this instead".into()))
        .await
        .unwrap();
    // Give the select! a moment to fold the steer into stop_reason.
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    // Release the pause — the soft-stop fires at the stream break point.
    gate.notify_one();

    let (outcome, messages) = turn_handle.await.unwrap();
    // The steer is still carried as stop_reason (existing soft-stop contract).
    assert_eq!(
        outcome.stop_reason,
        Some(crate::agent::StopReason::Steer(vec![
            "do this instead".into()
        ])),
        "mid-stream steer must still be carried as stop_reason"
    );

    // Drop our sender so the collector task ends and hands back the events.
    drop(fanin_tx);
    let events = collect_handle.await.unwrap();

    // The announced call reached the UI…
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolCallStart { id, .. } if id == "call_rf")),
        "ToolCallStart for call_rf must reach the fan-in"
    );
    // …and DRAINED: the tool loop executed it, so its ToolResult is the real
    // execution outcome — never the synthetic "interrupted: not run" marker
    // (that marker now only appears on HARD stops, which end the turn before
    // the batch runs).
    let result_pos = events.iter().position(|e| {
        matches!(
            e,
            AgentEvent::ToolResult { tool_call_id, result }
                if tool_call_id == "call_rf"
                    && !result.output.contains("interrupted: not run")
        )
    });
    let finished_pos = events
        .iter()
        .position(|e| matches!(e, AgentEvent::Finished { .. }));
    assert!(
        result_pos.is_some(),
        "the announced call must drain (execute) and get its real ToolResult"
    );
    if let (Some(rp), Some(fp)) = (result_pos, finished_pos) {
        assert!(
            rp < fp,
            "the real ToolResult must be emitted before Finished"
        );
    }

    // The context reflects what actually ran: a real tool_result message for
    // the drained call follows the assistant turn in history.
    assert!(
        messages
            .iter()
            .any(|m| m.role == Role::Tool && m.tool_call_id.as_deref() == Some("call_rf")),
        "the drained call must be recorded as a tool_result message in history"
    );
}

#[tokio::test]
async fn mid_stream_steer_drains_a_multi_call_batch() {
    // The instance-2/5 shape of the 2026-12-30 bug: a steer arriving
    // mid-stream over a MULTI-call batch used to silently cancel the whole
    // batch — no results, nothing in history. A steer must drain EVERY call
    // in the batch: each executes with a real result in both the UI and
    // history, and the steer still drives the follow-up turn.
    use crate::runtime::AgentCommand;
    use tokio::sync::Notify;

    /// Streams a two-call batch, then blocks on a Notify until the test
    /// signals it (so a steer can arrive mid-stream), then yields a Finish.
    struct TwoCallPausingProvider {
        gate: Arc<Notify>,
        caps: Capabilities,
    }

    #[async_trait::async_trait]
    impl LlmClient for TwoCallPausingProvider {
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
        fn kind(&self) -> ProviderKind {
            ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            "mock"
        }
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _tool_choice: Option<crate::provider::ToolChoice>,
        ) -> crate::error::Result<BoxStream<'_, LlmEvent>> {
            use futures::StreamExt;
            let gate = self.gate.clone();
            let stream = futures::stream::iter(vec![
                LlmEvent::ToolCallStart {
                    index: 0,
                    id: "call_a".into(),
                    name: "read_files".into(),
                },
                LlmEvent::ToolCallArgumentDelta {
                    index: 0,
                    fragment: "{\"files\": []}".into(),
                },
                LlmEvent::ToolCallStart {
                    index: 1,
                    id: "call_b".into(),
                    name: "read_files".into(),
                },
                LlmEvent::ToolCallArgumentDelta {
                    index: 1,
                    fragment: "{\"files\": []}".into(),
                },
            ])
            .chain(futures::stream::once(async move {
                gate.notified().await;
                LlmEvent::Finish {
                    reason: FinishReason::Stop,
                }
            }));
            Ok(Box::pin(stream))
        }
    }

    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    let gate = Arc::new(Notify::new());
    let provider: Arc<dyn LlmClient> = Arc::new(TwoCallPausingProvider {
        gate: gate.clone(),
        caps: Capabilities::openai(),
    });
    let agent = Arc::new(AgentLoop::new(
        test_config(provider, registry, workflow, sandbox),
        crate::project::Constitution::default(),
    ));

    let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
    let turn_tx = fanin_tx.clone();
    let (cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text("hello")];

    let collect_handle = tokio::spawn(async move {
        let mut events: Vec<AgentEvent> = Vec::new();
        while let Some((_id, event)) = fanin_rx.recv().await {
            events.push(event);
        }
        events
    });

    let agent_clone = Arc::clone(&agent);
    let turn_handle = tokio::spawn(async move {
        let outcome = agent_clone
            .run_turn(&mut messages, &turn_tx, 1, &mut cmd_rx, None)
            .await
            .unwrap();
        (outcome, messages)
    });

    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    cmd_tx
        .send(AgentCommand::Suggestion("do this instead".into()))
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    gate.notify_one();

    let (outcome, messages) = turn_handle.await.unwrap();
    assert_eq!(
        outcome.stop_reason,
        Some(crate::agent::StopReason::Steer(vec![
            "do this instead".into()
        ])),
        "mid-stream steer must still be carried as stop_reason"
    );

    drop(fanin_tx);
    let events = collect_handle.await.unwrap();

    // BOTH calls drained: each got a real (non-synthetic) ToolResult and a
    // tool_result message in history.
    for call_id in ["call_a", "call_b"] {
        assert!(
            events.iter().any(|e| matches!(
                e,
                AgentEvent::ToolResult { tool_call_id, result }
                    if tool_call_id == call_id
                        && !result.output.contains("interrupted: not run")
            )),
            "{call_id} must drain (execute) with a real ToolResult"
        );
        assert!(
            messages
                .iter()
                .any(|m| m.role == Role::Tool && m.tool_call_id.as_deref() == Some(call_id)),
            "{call_id} must be recorded as a tool_result message in history"
        );
    }
}

#[tokio::test]
async fn mid_stream_interrupt_still_stops_before_the_batch() {
    // The hard-stop counterpart: an Interrupt (Stop button) arriving
    // mid-stream over an announced tool call must still end the turn BEFORE
    // the batch runs — the call does NOT execute and gets the synthetic
    // "interrupted: not run" result. (Plan edfff8d9 step 4 additionally
    // records the cancelled calls in history; this test pins the
    // no-execution contract.)
    use crate::runtime::AgentCommand;
    use tokio::sync::Notify;

    /// Same shape as the steer provider: one announced call, a mid-stream
    /// pause, then Finish.
    struct InterruptPausingProvider {
        gate: Arc<Notify>,
        caps: Capabilities,
    }

    #[async_trait::async_trait]
    impl LlmClient for InterruptPausingProvider {
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
        fn kind(&self) -> ProviderKind {
            ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            "mock"
        }
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _tool_choice: Option<crate::provider::ToolChoice>,
        ) -> crate::error::Result<BoxStream<'_, LlmEvent>> {
            use futures::StreamExt;
            let gate = self.gate.clone();
            let stream = futures::stream::iter(vec![
                LlmEvent::ToolCallStart {
                    index: 0,
                    id: "call_rf".into(),
                    name: "read_files".into(),
                },
                LlmEvent::ToolCallArgumentDelta {
                    index: 0,
                    fragment: "{\"files\": []}".into(),
                },
            ])
            .chain(futures::stream::once(async move {
                gate.notified().await;
                LlmEvent::Finish {
                    reason: FinishReason::Stop,
                }
            }));
            Ok(Box::pin(stream))
        }
    }

    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    let gate = Arc::new(Notify::new());
    let provider: Arc<dyn LlmClient> = Arc::new(InterruptPausingProvider {
        gate: gate.clone(),
        caps: Capabilities::openai(),
    });
    let agent = Arc::new(AgentLoop::new(
        test_config(provider, registry, workflow, sandbox),
        crate::project::Constitution::default(),
    ));

    let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
    let turn_tx = fanin_tx.clone();
    let (cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text("hello")];

    let collect_handle = tokio::spawn(async move {
        let mut events: Vec<AgentEvent> = Vec::new();
        while let Some((_id, event)) = fanin_rx.recv().await {
            events.push(event);
        }
        events
    });

    let agent_clone = Arc::clone(&agent);
    let turn_handle = tokio::spawn(async move {
        let outcome = agent_clone
            .run_turn(&mut messages, &turn_tx, 1, &mut cmd_rx, None)
            .await
            .unwrap();
        (outcome, messages)
    });

    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    cmd_tx.send(AgentCommand::Interrupt).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    gate.notify_one();

    let (outcome, messages) = turn_handle.await.unwrap();
    assert_eq!(
        outcome.stop_reason,
        Some(crate::agent::StopReason::Interrupt),
        "mid-stream Interrupt must be carried as stop_reason"
    );

    drop(fanin_tx);
    let events = collect_handle.await.unwrap();

    // The call did NOT execute: its only ToolResult is the synthetic
    // "interrupted: not run" marker, emitted before Finished.
    let result_pos = events.iter().position(|e| {
        matches!(
            e,
            AgentEvent::ToolResult { tool_call_id, result }
                if tool_call_id == "call_rf"
                    && result.output.contains("interrupted: not run")
        )
    });
    let finished_pos = events
        .iter()
        .position(|e| matches!(e, AgentEvent::Finished { .. }));
    assert!(
        result_pos.is_some(),
        "the interrupted call must get the synthetic not-run ToolResult"
    );
    if let (Some(rp), Some(fp)) = (result_pos, finished_pos) {
        assert!(rp < fp, "the synthetic ToolResult must precede Finished");
    }

    // (step 4) The cancelled call is recorded in HISTORY: the assistant turn
    // carries the tool_call and exactly one "interrupted: not run" tool_result
    // message follows it — the model's next turn sees what it announced and
    // that it did not run (the every-call-has-a-result invariant).
    assert!(
        messages.iter().any(|m| {
            m.role == Role::Assistant && m.tool_calls.iter().any(|tc| tc.id == "call_rf")
        }),
        "the cancelled call must be recorded on the assistant turn in history"
    );
    let results: Vec<_> = messages
        .iter()
        .filter(|m| m.role == Role::Tool && m.tool_call_id.as_deref() == Some("call_rf"))
        .collect();
    assert_eq!(
        results.len(),
        1,
        "exactly one tool_result message per tool_call id (every-call-has-a-result invariant)"
    );
}

#[tokio::test]
async fn mid_stream_interrupt_sanitizes_truncated_args_in_history() {
    // The raw-echo hazard (audit finding D): a stream cut MID-ARGUMENTS
    // leaves the accumulated call with invalid JSON args. Recording those
    // verbatim in history would resend the malformed arguments on the next
    // request (gateway 400 "Unterminated string"). The hard-stop record must
    // sanitize them to an empty JSON object (same rule as the bad-JSON path)
    // and drop the raw echo, while the every-call-has-a-result invariant
    // still holds.
    use crate::runtime::AgentCommand;
    use tokio::sync::Notify;

    /// Streams a call whose argument JSON is cut mid-object, then blocks on
    /// a Notify until the test signals it, then yields a Finish.
    struct TruncatedArgsPausingProvider {
        gate: Arc<Notify>,
        caps: Capabilities,
    }

    #[async_trait::async_trait]
    impl LlmClient for TruncatedArgsPausingProvider {
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
        fn kind(&self) -> ProviderKind {
            ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            "mock"
        }
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _tool_choice: Option<crate::provider::ToolChoice>,
        ) -> crate::error::Result<BoxStream<'_, LlmEvent>> {
            use futures::StreamExt;
            let gate = self.gate.clone();
            let stream = futures::stream::iter(vec![
                LlmEvent::ToolCallStart {
                    index: 0,
                    id: "call_cut".into(),
                    name: "read_files".into(),
                },
                LlmEvent::ToolCallArgumentDelta {
                    index: 0,
                    fragment: "{\"files\": ".into(),
                },
            ])
            .chain(futures::stream::once(async move {
                gate.notified().await;
                LlmEvent::Finish {
                    reason: FinishReason::Stop,
                }
            }));
            Ok(Box::pin(stream))
        }
    }

    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    let gate = Arc::new(Notify::new());
    let provider: Arc<dyn LlmClient> = Arc::new(TruncatedArgsPausingProvider {
        gate: gate.clone(),
        caps: Capabilities::openai(),
    });
    let agent = Arc::new(AgentLoop::new(
        test_config(provider, registry, workflow, sandbox),
        crate::project::Constitution::default(),
    ));

    let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
    let turn_tx = fanin_tx.clone();
    let (cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text("hello")];

    let collect_handle = tokio::spawn(async move {
        let mut events: Vec<AgentEvent> = Vec::new();
        while let Some((_id, event)) = fanin_rx.recv().await {
            events.push(event);
        }
        events
    });

    let agent_clone = Arc::clone(&agent);
    let turn_handle = tokio::spawn(async move {
        let outcome = agent_clone
            .run_turn(&mut messages, &turn_tx, 1, &mut cmd_rx, None)
            .await
            .unwrap();
        (outcome, messages)
    });

    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    cmd_tx.send(AgentCommand::Interrupt).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    gate.notify_one();

    let (outcome, messages) = turn_handle.await.unwrap();
    assert_eq!(
        outcome.stop_reason,
        Some(crate::agent::StopReason::Interrupt),
        "mid-stream Interrupt must be carried as stop_reason"
    );

    drop(fanin_tx);
    let _ = collect_handle.await.unwrap();

    // The truncated call is recorded with SANITIZED arguments.
    let assistant = messages
        .iter()
        .find(|m| m.role == Role::Assistant && m.tool_calls.iter().any(|tc| tc.id == "call_cut"))
        .expect("the cut call must be recorded on the assistant turn");
    let recorded = assistant
        .tool_calls
        .iter()
        .find(|tc| tc.id == "call_cut")
        .unwrap();
    assert_eq!(
        recorded.arguments, "{}",
        "truncated arguments must be sanitized to an empty JSON object in history"
    );
    // …and the every-call-has-a-result invariant holds for it too.
    assert_eq!(
        messages
            .iter()
            .filter(|m| m.role == Role::Tool && m.tool_call_id.as_deref() == Some("call_cut"))
            .count(),
        1,
        "exactly one tool_result message for the cut call"
    );
}

#[tokio::test]
async fn run_turn_records_cancelled_request_stats() {
    // D1: a user interrupt that drops a live stream mid-flight must record
    // a request_stats row with outcome='cancelled' (the provider billed the
    // tokens generated so far — aborted requests stay countable in the
    // latency/token aggregates; cached_tokens is NULL — no usage was
    // reported).
    use crate::runtime::AgentCommand;
    use tokio::sync::Notify;

    /// Streams one TextDelta, then blocks on a Notify until the test
    /// signals it, then yields a Finish.
    struct PausingAfterTextProvider {
        gate: Arc<Notify>,
        caps: Capabilities,
    }

    #[async_trait::async_trait]
    impl LlmClient for PausingAfterTextProvider {
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
        fn kind(&self) -> ProviderKind {
            ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            "mock"
        }
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _tool_choice: Option<crate::provider::ToolChoice>,
        ) -> crate::error::Result<BoxStream<'_, LlmEvent>> {
            use futures::StreamExt;
            let gate = self.gate.clone();
            let stream = futures::stream::iter(vec![LlmEvent::TextDelta {
                text: "partial".into(),
            }])
            .chain(futures::stream::once(async move {
                gate.notified().await;
                LlmEvent::Finish {
                    reason: FinishReason::Stop,
                }
            }));
            Ok(Box::pin(stream))
        }
    }

    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());
    let embedder: Arc<dyn crate::memory::Embedder> = Arc::new(HashEmbedder::new());
    let store: Arc<dyn crate::memory::MemoryStoreTrait> =
        Arc::new(MemoryStore::open_in_memory(embedder).unwrap());

    let gate = Arc::new(Notify::new());
    let provider: Arc<dyn LlmClient> = Arc::new(PausingAfterTextProvider {
        gate: gate.clone(),
        caps: Capabilities::openai(),
    });
    let agent = Arc::new(AgentLoop::new(
        AgentLoopConfig {
            provider,
            tools: registry,
            workflow: workflow,
            sandbox: sandbox,
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(128_000, 0.5),
            memory: Some(store.clone()),
            vision: None,
        },
        crate::project::Constitution::default(),
    ));

    let (fanin_tx, _fanin_rx) = mpsc::channel(64);
    let turn_tx = fanin_tx.clone();
    let (cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let session = store.start_session("test").await.unwrap();
    let sid = session.id.clone();
    let sid_for_turn = sid.clone();
    let mut messages = vec![Message::user_text("hello")];

    let agent_clone = Arc::clone(&agent);
    let turn_handle = tokio::spawn(async move {
        agent_clone
            .run_turn(&mut messages, &turn_tx, 1, &mut cmd_rx, Some(&sid_for_turn))
            .await
            .unwrap()
    });

    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    cmd_tx.send(AgentCommand::Interrupt).await.unwrap();
    let outcome = turn_handle.await.unwrap();
    assert_eq!(
        outcome.stop_reason,
        Some(crate::agent::StopReason::Interrupt),
        "mid-stream Interrupt must be carried as stop_reason"
    );
    gate.notify_one();

    // The stats recording is fire-and-forget (spawned). Give it a moment.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let rows = store.request_stats_rows(&sid).await.unwrap();
    assert_eq!(rows.len(), 1, "exactly one cancelled row");
    let row = &rows[0];
    assert_eq!(row.outcome.as_deref(), Some("cancelled"));
    assert_eq!(row.cached_tokens, None, "cached_tokens is NULL — no usage");
    assert_eq!(row.completion_tokens, 0);
    assert!(row.prompt_tokens > 0, "our estimate is recorded");
    assert_eq!(row.purpose, None);
}

#[tokio::test]
async fn run_turn_usage_then_interrupt_records_no_cancelled_row() {
    // D1: a stream that reported Usage before the user interrupted already
    // recorded its success row — the usage_recorded guard must not
    // double-record a cancelled row for the same request.
    use crate::runtime::AgentCommand;
    use tokio::sync::Notify;

    struct UsageThenPauseProvider {
        gate: Arc<Notify>,
        caps: Capabilities,
    }

    #[async_trait::async_trait]
    impl LlmClient for UsageThenPauseProvider {
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
        fn kind(&self) -> ProviderKind {
            ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            "mock"
        }
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _tool_choice: Option<crate::provider::ToolChoice>,
        ) -> crate::error::Result<BoxStream<'_, LlmEvent>> {
            use futures::StreamExt;
            let gate = self.gate.clone();
            let stream = futures::stream::iter(vec![LlmEvent::Usage {
                prompt_tokens: 700,
                completion_tokens: 30,
                reasoning_tokens: 0,
                cached_tokens: 0,
                ttft_ms: Some(80),
                generation_ms: Some(300),
            }])
            .chain(futures::stream::once(async move {
                gate.notified().await;
                LlmEvent::Finish {
                    reason: FinishReason::Stop,
                }
            }));
            Ok(Box::pin(stream))
        }
    }

    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());
    let embedder: Arc<dyn crate::memory::Embedder> = Arc::new(HashEmbedder::new());
    let store: Arc<dyn crate::memory::MemoryStoreTrait> =
        Arc::new(MemoryStore::open_in_memory(embedder).unwrap());

    let gate = Arc::new(Notify::new());
    let provider: Arc<dyn LlmClient> = Arc::new(UsageThenPauseProvider {
        gate: gate.clone(),
        caps: Capabilities::openai(),
    });
    let agent = Arc::new(AgentLoop::new(
        AgentLoopConfig {
            provider,
            tools: registry,
            workflow: workflow,
            sandbox: sandbox,
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(128_000, 0.5),
            memory: Some(store.clone()),
            vision: None,
        },
        crate::project::Constitution::default(),
    ));

    let (fanin_tx, _fanin_rx) = mpsc::channel(64);
    let turn_tx = fanin_tx.clone();
    let (cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let session = store.start_session("test").await.unwrap();
    let sid = session.id.clone();
    let sid_for_turn = sid.clone();
    let mut messages = vec![Message::user_text("hello")];

    let agent_clone = Arc::clone(&agent);
    let turn_handle = tokio::spawn(async move {
        agent_clone
            .run_turn(&mut messages, &turn_tx, 1, &mut cmd_rx, Some(&sid_for_turn))
            .await
            .unwrap()
    });

    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    cmd_tx.send(AgentCommand::Interrupt).await.unwrap();
    let _outcome = turn_handle.await.unwrap();
    gate.notify_one();

    // The stats recording is fire-and-forget (spawned). Give it a moment.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let rows = store.request_stats_rows(&sid).await.unwrap();
    assert_eq!(
        rows.len(),
        1,
        "the success row only — no cancelled double-record"
    );
    assert_eq!(rows[0].outcome, None);
    assert_eq!(rows[0].prompt_tokens, 700);
    assert_eq!(rows[0].completion_tokens, 30);
}

/// A tool that sleeps for a configurable duration then succeeds — drives the
/// grace-window drain tests (a fast bookkeeping-style call completes within
/// the window; a slow call is cancelled after it). Agent category + AutoRun
/// safety passes the workflow ToolFilter in every state (the Workflow arm is
/// name-allow-listed per state, which a generic test tool cannot satisfy).
struct SleepTool {
    ms: u64,
}

#[async_trait]
impl crate::tool::Tool for SleepTool {
    fn name(&self) -> &str {
        "sleep_ms"
    }
    fn category(&self) -> crate::tool::ToolCategory {
        crate::tool::ToolCategory::Agent
    }
    fn schema(&self) -> crate::provider::ToolSchema {
        crate::provider::ToolSchema::new(
            "sleep_ms",
            "sleep then succeed",
            serde_json::json!({"type": "object", "properties": {}}),
        )
    }
    fn safety(&self) -> crate::tool::SafetyLevel {
        crate::tool::SafetyLevel::AutoRun
    }
    async fn execute(&self, _args: serde_json::Value) -> crate::tool::ToolResult {
        tokio::time::sleep(std::time::Duration::from_millis(self.ms)).await;
        crate::tool::ToolResult::success("slept")
    }
}

#[tokio::test]
async fn interrupt_mid_execution_drains_a_fast_tool_with_its_real_result() {
    // 2026-12-30 bug (root cause C): an Interrupt arriving mid-execution used
    // to DROP the tool future immediately — a fast bookkeeping mutation
    // (backlog_add, complete_step, memory_write) could end half-applied or
    // unapplied with only a synthetic "interrupted during execution" error.
    // Now the in-flight call is granted a bounded grace window
    // (DRAIN_GRACE_MS): a fast call completes with its REAL result (the turn
    // still stops — the stop signal is set).
    use crate::runtime::AgentCommand;

    let dir = tempdir().unwrap();
    let workflow = Arc::new(Mutex::new(Workflow::new(dir.path().join("plans"))));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(SleepTool { ms: 100 }));
    let provider = Arc::new(MockProvider {
        responses: Arc::new(Mutex::new(std::collections::VecDeque::new())),
        caps: Capabilities::openai(),
        tools_phases: Arc::new(StdMutex::new(Vec::new())),
        name: String::new(),
    });
    let agent = AgentLoop::new(
        test_config(provider, Arc::new(registry), workflow, sandbox),
        crate::project::Constitution::default(),
    );

    let tc = crate::provider::ToolCall::new("call_sleep", "sleep_ms", "{}");
    let (fanin_tx, _fanin_rx) = mpsc::channel(16);
    let (cmd_tx, mut cmd_rx) = mpsc::channel(8);
    // The Interrupt is queued BEFORE dispatch polls the channel — the tool is
    // still sleeping when the grace window opens.
    cmd_tx.send(AgentCommand::Interrupt).await.unwrap();
    let mut deny_all_latched = false;
    let mut stop_signal: Option<super::StopReason> = None;

    let (result, buffered) = agent
        .execute_tool_call(
            &tc,
            &fanin_tx,
            1,
            &mut cmd_rx,
            &mut deny_all_latched,
            &mut stop_signal,
        )
        .await;

    assert!(
        result.success && result.output.contains("slept"),
        "a fast tool must drain to completion with its real result, got: {}",
        result.output
    );
    assert_eq!(
        stop_signal,
        Some(crate::agent::StopReason::Interrupt),
        "the stop signal must still be set — the turn stops after the drained call"
    );
    assert!(buffered.is_empty(), "no commands were buffered");
}

#[tokio::test]
async fn interrupt_mid_execution_cancels_a_slow_tool_after_the_grace_window() {
    // The other half of the grace-window drain: a call that blows the window
    // (here 10s >> DRAIN_GRACE_MS) is dropped and gets the explicit
    // "interrupted during execution" result — Stop still stops promptly
    // (this test takes ~DRAIN_GRACE_MS, the window itself).
    use crate::runtime::AgentCommand;

    let dir = tempdir().unwrap();
    let workflow = Arc::new(Mutex::new(Workflow::new(dir.path().join("plans"))));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(SleepTool { ms: 10_000 }));
    let provider = Arc::new(MockProvider {
        responses: Arc::new(Mutex::new(std::collections::VecDeque::new())),
        caps: Capabilities::openai(),
        tools_phases: Arc::new(StdMutex::new(Vec::new())),
        name: String::new(),
    });
    let agent = AgentLoop::new(
        test_config(provider, Arc::new(registry), workflow, sandbox),
        crate::project::Constitution::default(),
    );

    let tc = crate::provider::ToolCall::new("call_slow", "sleep_ms", "{}");
    let (fanin_tx, _fanin_rx) = mpsc::channel(16);
    let (cmd_tx, mut cmd_rx) = mpsc::channel(8);
    cmd_tx.send(AgentCommand::Interrupt).await.unwrap();
    let mut deny_all_latched = false;
    let mut stop_signal: Option<super::StopReason> = None;

    let (result, _buffered) = agent
        .execute_tool_call(
            &tc,
            &fanin_tx,
            1,
            &mut cmd_rx,
            &mut deny_all_latched,
            &mut stop_signal,
        )
        .await;

    assert!(
        !result.success && result.output.contains("interrupted during execution"),
        "a slow tool must be cancelled with the explicit interrupted result, got: {}",
        result.output
    );
    assert_eq!(
        stop_signal,
        Some(crate::agent::StopReason::Interrupt),
        "the stop signal must be set — the turn stops after the cancelled call"
    );
}

#[tokio::test]
async fn commands_arriving_during_the_grace_window_are_not_lost() {
    // While the grace window drains a fast call, a steer arriving on the
    // command channel must NOT be dropped: it stays queued in cmd_rx for the
    // next fold point — the batch loop's between-call drain, or (after the
    // turn ends) the run loop's pre-inject drain, whose fold now PROMOTES a
    // steer into Some(Interrupt) to InterruptWithSteers (2026-12-30 review
    // finding 1) so it drives the follow-up turn. This test pins the
    // dispatch-level half of that contract (the steer is still queued when
    // dispatch returns); the fold promotion and the end-to-end merge are
    // pinned by fold_suggestion_into_interrupt_promotes_to_interrupt_with_steers
    // and interrupt_with_buffered_steer_carries_the_steer.
    use crate::runtime::AgentCommand;

    let dir = tempdir().unwrap();
    let workflow = Arc::new(Mutex::new(Workflow::new(dir.path().join("plans"))));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(SleepTool { ms: 100 }));
    let provider = Arc::new(MockProvider {
        responses: Arc::new(Mutex::new(std::collections::VecDeque::new())),
        caps: Capabilities::openai(),
        tools_phases: Arc::new(StdMutex::new(Vec::new())),
        name: String::new(),
    });
    let agent = AgentLoop::new(
        test_config(provider, Arc::new(registry), workflow, sandbox),
        crate::project::Constitution::default(),
    );

    let tc = crate::provider::ToolCall::new("call_sleep", "sleep_ms", "{}");
    let (fanin_tx, _fanin_rx) = mpsc::channel(16);
    let (cmd_tx, mut cmd_rx) = mpsc::channel(8);
    cmd_tx.send(AgentCommand::Interrupt).await.unwrap();
    cmd_tx
        .send(AgentCommand::Suggestion("steer text".into()))
        .await
        .unwrap();
    let mut deny_all_latched = false;
    let mut stop_signal: Option<super::StopReason> = None;

    let (result, buffered) = agent
        .execute_tool_call(
            &tc,
            &fanin_tx,
            1,
            &mut cmd_rx,
            &mut deny_all_latched,
            &mut stop_signal,
        )
        .await;

    assert!(
        result.success && result.output.contains("slept"),
        "the fast tool drained with its real result, got: {}",
        result.output
    );
    assert!(
        buffered.is_empty(),
        "the drain path does not buffer — the steer stays queued in the channel"
    );
    // The steer is still in the channel for the next safe point.
    let queued = cmd_rx
        .try_recv()
        .expect("the steer sent during the grace window must still be queued");
    assert!(
        matches!(queued, AgentCommand::Suggestion(text) if text.text == "steer text"),
        "the steer arriving during the grace window must not be lost"
    );
}

#[tokio::test]
async fn interrupt_with_buffered_steer_carries_the_steer() {
    // The end-to-end grace-window seam (2026-12-30 review finding 1, plan
    // edfff8d9): the user presses Stop while a tool call executes, then
    // types a follow-up BEFORE the turn ends. The steer is buffered during
    // execution, the Interrupt opens the grace window, the fast tool
    // completes with its real result — and the merge must carry BOTH: the
    // stop (Interrupt) and the steer (InterruptWithSteers), so the user's
    // message drives the follow-up turn instead of vanishing. (Before the
    // fix the buffered re-injection was skipped under hard_stop and the
    // fold's `_ => {}` arm discarded the steer.)
    use crate::runtime::AgentCommand;

    /// Streams one sleep_ms call, then Finish (no pause — the timing comes
    /// from the tool's own sleep).
    struct SleepCallProvider {
        caps: Capabilities,
    }

    #[async_trait::async_trait]
    impl LlmClient for SleepCallProvider {
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
        fn kind(&self) -> ProviderKind {
            ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            "mock"
        }
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _tool_choice: Option<crate::provider::ToolChoice>,
        ) -> crate::error::Result<BoxStream<'_, LlmEvent>> {
            let stream = futures::stream::iter(vec![
                LlmEvent::ToolCallStart {
                    index: 0,
                    id: "call_sleep".into(),
                    name: "sleep_ms".into(),
                },
                LlmEvent::ToolCallArgumentDelta {
                    index: 0,
                    fragment: "{}".into(),
                },
                LlmEvent::Finish {
                    reason: FinishReason::Stop,
                },
            ]);
            Ok(Box::pin(stream))
        }
    }

    let dir = tempdir().unwrap();
    let workflow = Arc::new(Mutex::new(Workflow::new(dir.path().join("plans"))));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(SleepTool { ms: 300 }));
    let provider: Arc<dyn LlmClient> = Arc::new(SleepCallProvider {
        caps: Capabilities::openai(),
    });
    let agent = Arc::new(AgentLoop::new(
        test_config(provider, Arc::new(registry), workflow, sandbox),
        crate::project::Constitution::default(),
    ));

    let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
    let turn_tx = fanin_tx.clone();
    let (cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text("hello")];

    let collect_handle = tokio::spawn(async move {
        let mut events: Vec<AgentEvent> = Vec::new();
        while let Some((_id, event)) = fanin_rx.recv().await {
            events.push(event);
        }
        events
    });

    let agent_clone = Arc::clone(&agent);
    let turn_handle = tokio::spawn(async move {
        let outcome = agent_clone
            .run_turn(&mut messages, &turn_tx, 1, &mut cmd_rx, None)
            .await
            .unwrap();
        (outcome, messages)
    });

    // Both commands land WHILE the tool is executing (300ms sleep): the
    // steer is buffered by dispatch's steer arm; the Interrupt opens the
    // grace window.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    cmd_tx
        .send(AgentCommand::Suggestion("actually do X instead".into()))
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    cmd_tx.send(AgentCommand::Interrupt).await.unwrap();

    let (outcome, messages) = turn_handle.await.unwrap();
    assert_eq!(
        outcome.stop_reason,
        Some(crate::agent::StopReason::InterruptWithSteers(vec![
            "actually do X instead".into()
        ])),
        "the stop AND the steer must both survive the merge"
    );

    drop(fanin_tx);
    let events = collect_handle.await.unwrap();

    // The fast tool drained with its real result (not the synthetic
    // interrupted marker).
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::ToolResult { tool_call_id, result }
                if tool_call_id == "call_sleep"
                    && result.success
                    && result.output.contains("slept")
        )),
        "the fast tool must drain with its real result"
    );
    // …and the context records it.
    assert!(
        messages
            .iter()
            .any(|m| m.role == Role::Tool && m.tool_call_id.as_deref() == Some("call_sleep")),
        "the drained call must be recorded as a tool_result message in history"
    );
}

#[tokio::test]
async fn bad_json_tool_call_emits_tool_result_event() {
    // Backlog 63cbc20f (second orphaning path): the bad-JSON retry branch
    // pushes Role::Tool error messages into history but emitted NO ToolResult
    // events — the card the model already announced stays "running" while the
    // turn continues (it only ends later, so even a Finished-sweep can't
    // rescue it). The branch must emit a per-call ToolResult so the card ends.
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    // First response: a tool call whose arguments are truncated JSON (hit the
    // token limit mid-argument) with finish_reason Length. The MockProvider's
    // empty queue then serves a bare Finish (ending the retried turn).
    let provider: Arc<dyn LlmClient> = Arc::new(MockProvider::single(vec![
        LlmEvent::ToolCallStart {
            index: 0,
            id: "call_bj".into(),
            name: "file_write".into(),
        },
        LlmEvent::ToolCallArgumentDelta {
            index: 0,
            fragment: "{\"path\": \"a".into(),
        },
        LlmEvent::Finish {
            reason: FinishReason::Length,
        },
    ]));
    let agent = Arc::new(AgentLoop::new(
        test_config(provider, registry, workflow, sandbox),
        crate::project::Constitution::default(),
    ));

    let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text("write a file")];

    let outcome = agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await
        .unwrap();
    // The turn retried once (bad JSON), then ended on the clean second pass.
    assert_eq!(outcome.finish_reason, FinishReason::Stop);

    // Drain every emitted event and find the call's terminal result.
    let mut saw_tool_result = false;
    while let Ok((_id, event)) = fanin_rx.try_recv() {
        if let AgentEvent::ToolResult {
            tool_call_id,
            result,
        } = event
        {
            if tool_call_id == "call_bj" {
                saw_tool_result = true;
                assert!(!result.success, "the bad-JSON result must be a failure");
                assert!(
                    result.output.contains("malformed") || result.output.contains("truncated"),
                    "the result must say the args were malformed/truncated, got: {}",
                    result.output
                );
            }
        }
    }
    assert!(
        saw_tool_result,
        "a bad-JSON tool call must still emit a ToolResult event (stuck-card bug)"
    );
}

#[tokio::test]
async fn ask_user_two_questions_round_trip() {
    // The two-questions example: an agent asks Q1 (3 options), the user
    // answers by clicking an option, then asks Q2, the user answers freeform.
    // Asserts both answers reach the agent (as the ToolResult) — one question
    // at a time (the turn blocks on each oneshot).
    use crate::tool::workflow::ask_user::AskUserTool;
    use crate::tool::ToolRegistry;

    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    // Registry with ask_user registered (so dispatch finds it).
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(AskUserTool::new()));
    let provider: Arc<dyn LlmClient> = Arc::new(MockProvider {
        responses: Arc::new(Mutex::new(std::collections::VecDeque::new())),
        caps: Capabilities::openai(),
        tools_phases: Arc::new(std::sync::Mutex::new(Vec::new())),
        name: String::new(),
    });
    let agent = Arc::new(AgentLoop::new(
        AgentLoopConfig {
            provider: provider,
            tools: Arc::new(registry),
            workflow: workflow,
            sandbox: sandbox,
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(128_000, 0.5),
            memory: None,
            vision: None,
        },
        crate::project::Constitution::default(),
    ));

    let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
    let (cmd_tx1, mut cmd_rx1) = mpsc::channel(8);

    // ── Q1: "Pick a skill to master" with 3 options ──────────────────────
    let tc1 = crate::provider::ToolCall::new(
        "call_q1",
        "ask_user",
        serde_json::json!({
            "question": "Pick a skill to master",
            "options": [
                {"label": "Music"},
                {"label": "Languages"},
                {"label": "Cooking"}
            ]
        })
        .to_string(),
    );
    // Drive the ask_user call in a task (it blocks on the oneshot).
    let agent_clone: Arc<AgentLoop> = Arc::clone(&agent);
    let fanin_tx_clone = fanin_tx.clone();
    let q1_handle = tokio::spawn(async move {
        let mut deny_all_latched = false;
        let mut stop_signal: Option<super::StopReason> = None;
        agent_clone
            .execute_tool_call(
                &tc1,
                &fanin_tx_clone,
                1,
                &mut cmd_rx1,
                &mut deny_all_latched,
                &mut stop_signal,
            )
            .await
    });
    // Receive the UserQuestion event + answer it (click option 1 = "Languages").
    let (q1_id, q1_options, q1_responder) = recv_question(&mut fanin_rx).await;
    assert_eq!(q1_options.len(), 3);
    assert_eq!(q1_options[0].label, "Music");
    // Resolve Q1 with Choice(1).
    q1_responder
        .send(crate::runtime::UserAnswer::Choice { index: 1 })
        .unwrap();
    let (result1, _) = q1_handle.await.unwrap();
    assert!(result1.success, "Q1 should succeed");
    assert_eq!(result1.output, "Languages", "Q1 answer is the chosen label");
    let _ = q1_id;
    let _ = cmd_tx1; // keep the sender alive so the channel isn't closed early

    // ── Q2: "Favorite color?" — answered freeform ────────────────────────
    let (cmd_tx2, mut cmd_rx2) = mpsc::channel(8);
    let tc2 = crate::provider::ToolCall::new(
        "call_q2",
        "ask_user",
        serde_json::json!({
            "question": "Favorite color?",
            "options": [{"label": "Blue"}, {"label": "Green"}, {"label": "Red"}]
        })
        .to_string(),
    );
    let agent_clone: Arc<AgentLoop> = Arc::clone(&agent);
    let fanin_tx_clone = fanin_tx.clone();
    let q2_handle = tokio::spawn(async move {
        let mut deny_all_latched = false;
        let mut stop_signal: Option<super::StopReason> = None;
        agent_clone
            .execute_tool_call(
                &tc2,
                &fanin_tx_clone,
                1,
                &mut cmd_rx2,
                &mut deny_all_latched,
                &mut stop_signal,
            )
            .await
    });
    let (q2_id, q2_options, q2_responder) = recv_question(&mut fanin_rx).await;
    assert_eq!(q2_options.len(), 3);
    // Resolve Q2 with Freeform.
    q2_responder
        .send(crate::runtime::UserAnswer::Freeform {
            text: "it's purple".into(),
        })
        .unwrap();
    let (result2, _) = q2_handle.await.unwrap();
    assert!(result2.success, "Q2 should succeed");
    assert_eq!(
        result2.output, "it's purple",
        "Q2 answer is the freeform text"
    );
    let _ = q2_id;
    let _ = cmd_tx2;
}

#[tokio::test]
async fn ask_user_interrupt_returns_error() {
    // T2: an Interrupt arriving while ask_user is awaiting the answer must
    // return an error result (not hang). Mirrors the approval interrupt path.
    use crate::tool::workflow::ask_user::AskUserTool;
    use crate::tool::ToolRegistry;

    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(AskUserTool::new()));
    let provider: Arc<dyn LlmClient> = Arc::new(MockProvider {
        responses: Arc::new(Mutex::new(std::collections::VecDeque::new())),
        caps: Capabilities::openai(),
        tools_phases: Arc::new(std::sync::Mutex::new(Vec::new())),
        name: String::new(),
    });
    let agent = Arc::new(AgentLoop::new(
        AgentLoopConfig {
            provider: provider,
            tools: Arc::new(registry),
            workflow: workflow,
            sandbox: sandbox,
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(128_000, 0.5),
            memory: None,
            vision: None,
        },
        crate::project::Constitution::default(),
    ));

    let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
    let (cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut deny_all_latched = false;
    let mut stop_signal: Option<super::StopReason> = None;

    let tc = crate::provider::ToolCall::new(
        "call_q",
        "ask_user",
        serde_json::json!({
            "question": "Pick one",
            "options": [{"label": "A"}, {"label": "B"}]
        })
        .to_string(),
    );
    let agent_clone: Arc<AgentLoop> = Arc::clone(&agent);
    let fanin_tx_clone = fanin_tx.clone();
    let handle = tokio::spawn(async move {
        agent_clone
            .execute_tool_call(
                &tc,
                &fanin_tx_clone,
                1,
                &mut cmd_rx,
                &mut deny_all_latched,
                &mut stop_signal,
            )
            .await
    });

    // Wait for the UserQuestion event to be emitted (the agent is now blocked
    // on the oneshot). Keep the responder alive so the oneshot isn't closed
    // (a dropped responder would make answer_rx return Err before the
    // Interrupt arrives).
    let q_responder = recv_question(&mut fanin_rx).await.2;
    // Give the ask_user select! a moment to start listening on cmd_rx.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    // Send an Interrupt — the ask_user select! should catch it + return error.
    cmd_tx
        .send(crate::runtime::AgentCommand::Interrupt)
        .await
        .unwrap();
    let (result, _) = handle.await.unwrap();
    assert!(
        !result.success,
        "Interrupt must abort ask_user with an error"
    );
    assert!(
        result.output.contains("interrupted"),
        "error should mention interrupted, got: {}",
        result.output
    );
    let _ = q_responder;
}

/// Receive a UserQuestion event from the fan-in channel, returning
/// (question_id, options, responder). Panics if the next event isn't a
/// UserQuestion. The responder is returned so the test can send the answer
/// directly (this direct-dispatch path does NOT go through the IPC
/// PendingQuestions map — execute_tool_call holds the receiver + awaits it).
async fn recv_question(
    rx: &mut mpsc::Receiver<(crate::runtime::AgentId, AgentEvent)>,
) -> (
    String,
    Vec<crate::runtime::QuestionOption>,
    tokio::sync::oneshot::Sender<crate::runtime::UserAnswer>,
) {
    loop {
        match rx.recv().await {
            Some((
                _,
                AgentEvent::UserQuestion {
                    question_id,
                    options,
                    responder,
                    ..
                },
            )) => return (question_id, options, responder),
            Some(_) => continue, // skip non-question events
            None => panic!("fan-in channel closed before UserQuestion"),
        }
    }
}

#[tokio::test]
async fn resolve_turn_provider_uses_override_when_configured() {
    // T1: when a [models] override is configured for the current workflow
    // state, resolve_turn_provider must return a provider for THAT model (not
    // the default). And when no override matches, it returns None so the turn
    // uses the default snapshot.
    use crate::config::{Endpoint, GeneralConfig, ModelRef, ModelsConfig};
    use crate::model_resolver::{ConfigModelResolver, ModelContext, ModelResolver};
    use crate::workflow::WorkflowState;
    let config = crate::config::Config {
        general: GeneralConfig {
            models: ModelsConfig {
                planning: Some(ModelRef {
                    endpoint: "openai".into(),
                    model: "o3".into(),
                    reasoning_effort: None,
                }),
                ..ModelsConfig::default()
            },
            ..GeneralConfig::default()
        },
        endpoints: vec![Endpoint {
            name: "openai".into(),
            base_url: "https://api.openai.com/v1/".into(),
            models: vec!["gpt-4o".into(), "o3".into()],
            ..Endpoint::test_default()
        }],
        pricing: vec![],
        keys: Default::default(),
        mcp: Vec::new(),
        projects: Default::default(),
    };
    let resolver = ConfigModelResolver::new(
        Arc::new(std::sync::RwLock::new(config)),
        Arc::new(crate::provider::trace::LlmRequestLog::new()),
    );

    // Planning override → resolves to "o3".
    let ctx = ModelContext::new(WorkflowState::Planning, None, false, None);
    let model_ref = resolver.resolve(ctx).expect("planning override resolves");
    assert_eq!(model_ref.model, "o3");
    let (provider, _cm) = resolver
        .build_turn_provider(&model_ref, 0.5)
        .expect("provider builds");
    assert_eq!(
        provider.model(),
        "o3",
        "resolved provider must be the override model, not the default"
    );

    // Executing has no override → None (fall back to default).
    let ctx = ModelContext::new(WorkflowState::Executing, None, false, None);
    assert!(
        resolver.resolve(ctx).is_none(),
        "no override for Executing → None (use default)"
    );
}

#[tokio::test]
async fn resolve_turn_provider_forced_model_beats_state_override() {
    // The forced-model short-circuit: when a loop has BOTH a [models] state
    // override (planning → "o3") AND a forced model set (via spawn_agent's
    // `model` arg), resolve_turn_provider must return the FORCED provider,
    // not the state override. This is the feature's core behavior — a
    // regression that reorders the forced_model check below resolver.resolve
    // (or drops it) would pass every other test while silently breaking it.
    use crate::config::{Endpoint, GeneralConfig, ModelRef, ModelsConfig};
    use crate::model_resolver::ConfigModelResolver;
    use crate::provider::Capabilities;
    use crate::workflow::WorkflowState;

    /// A provider whose `model()` returns a fixed id, so the test can tell
    /// which model a built provider is for.
    struct FixedModelProvider {
        model: &'static str,
    }
    #[async_trait::async_trait]
    impl crate::provider::LlmClient for FixedModelProvider {
        fn capabilities(&self) -> &Capabilities {
            // max_context is read by build_turn_provider to size the CM; a
            // sane value keeps the test independent of provider-kind defaults.
            const C: Capabilities = Capabilities {
                supports_tool_choice: true,
                supports_strict_schema: true,
                supports_parallel_tools: true,
                reliable_finish_reason: true,
                max_context: 128_000,
                max_output_tokens: 4096,
                multimodal: false,
            };
            &C
        }
        fn kind(&self) -> crate::provider::ProviderKind {
            crate::provider::ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            self.model
        }
        async fn complete(
            &self,
            _messages: &[crate::provider::Message],
            _tools: &[crate::provider::ToolSchema],
            _tool_choice: Option<crate::provider::ToolChoice>,
        ) -> crate::error::Result<futures::stream::BoxStream<'_, crate::provider::LlmEvent>>
        {
            unreachable!("resolve_turn_provider must not call complete")
        }
    }

    let config = crate::config::Config {
        general: GeneralConfig {
            models: ModelsConfig {
                planning: Some(ModelRef {
                    endpoint: "openai".into(),
                    model: "o3".into(),
                    reasoning_effort: None,
                }),
                ..ModelsConfig::default()
            },
            ..GeneralConfig::default()
        },
        endpoints: vec![Endpoint {
            name: "openai".into(),
            base_url: "https://api.openai.com/v1/".into(),
            models: vec!["gpt-4o".into(), "o3".into(), "forced-model".into()],
            ..Endpoint::test_default()
        }],
        pricing: vec![],
        keys: Default::default(),
        mcp: Vec::new(),
        projects: Default::default(),
    };
    let resolver = Arc::new(ConfigModelResolver::new(
        Arc::new(std::sync::RwLock::new(config)),
        Arc::new(crate::provider::trace::LlmRequestLog::new()),
    ));

    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    // The loop's default provider is "default-model"; the resolver would
    // resolve planning → "o3"; the forced model is "forced-model".
    let default_provider: Arc<dyn crate::provider::LlmClient> = Arc::new(FixedModelProvider {
        model: "default-model",
    });
    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider: default_provider,
            tools: registry,
            workflow: workflow,
            sandbox: sandbox,
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(128_000, 0.5),
            memory: None,
            vision: None,
        },
        crate::project::Constitution::default(),
    )
    .with_model_resolver(resolver);

    // Sanity: WITHOUT a forced model, planning resolves to the state override.
    let (provider, _cm) = agent
        .resolve_turn_provider(WorkflowState::Planning, None, None)
        .expect("state override should resolve without a forced model");
    assert_eq!(
        provider.model(),
        "o3",
        "without a forced model, the state override wins"
    );

    // Now force a different model and assert it wins over the state override.
    agent.set_forced_model(ModelRef {
        endpoint: "openai".into(),
        model: "forced-model".into(),
        reasoning_effort: None,
    });
    let (provider, _cm) = agent
        .resolve_turn_provider(WorkflowState::Planning, None, None)
        .expect("forced model should resolve");
    assert_eq!(
        provider.model(),
        "forced-model",
        "forced model must beat the state override"
    );
}

#[tokio::test]
async fn summarize_provider_routes_through_the_summarize_slot() {
    // [models.summarize] routes compaction summaries to a dedicated (often
    // cheaper) model: with the slot set, summarize_provider returns a
    // provider for THAT model; unset, it returns the turn's provider
    // unchanged (today's behavior). The routing must not disturb the turn's
    // display state (set_resolved_model et al.).
    use crate::config::{Endpoint, GeneralConfig, ModelRef, ModelsConfig};
    use crate::model_resolver::ConfigModelResolver;
    use crate::provider::Capabilities;

    /// A provider whose `model()` returns a fixed id — tells the test which
    /// model resolved (and never completes; routing only inspects the id).
    struct FixedModelProvider {
        model: &'static str,
    }
    #[async_trait::async_trait]
    impl crate::provider::LlmClient for FixedModelProvider {
        fn capabilities(&self) -> &Capabilities {
            const C: Capabilities = Capabilities {
                supports_tool_choice: true,
                supports_strict_schema: true,
                supports_parallel_tools: true,
                reliable_finish_reason: true,
                max_context: 128_000,
                max_output_tokens: 4096,
                multimodal: false,
            };
            &C
        }
        fn kind(&self) -> crate::provider::ProviderKind {
            crate::provider::ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            self.model
        }
        async fn complete(
            &self,
            _messages: &[crate::provider::Message],
            _tools: &[crate::provider::ToolSchema],
            _tool_choice: Option<crate::provider::ToolChoice>,
        ) -> crate::error::Result<futures::stream::BoxStream<'_, crate::provider::LlmEvent>>
        {
            unreachable!("summarize_provider must not call complete")
        }
    }

    let make_loop = |summarize: Option<ModelRef>| -> (
        AgentLoop,
        Arc<dyn crate::provider::LlmClient>,
    ) {
        let config = crate::config::Config {
            general: GeneralConfig {
                models: ModelsConfig {
                    summarize,
                    ..ModelsConfig::default()
                },
                ..GeneralConfig::default()
            },
            endpoints: vec![Endpoint {
                name: "openai".into(),
                base_url: "https://api.openai.com/v1/".into(),
                models: vec!["gpt-4o".into(), "cheap-summary".into()],
                ..Endpoint::test_default()
            }],
            pricing: vec![],
            keys: Default::default(),
            mcp: Vec::new(),
            projects: Default::default(),
        };
        let resolver = Arc::new(ConfigModelResolver::new(
            Arc::new(std::sync::RwLock::new(config)),
            Arc::new(crate::provider::trace::LlmRequestLog::new()),
        ));
        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
        let registry = make_registry((*sandbox).clone(), workflow.clone());
        let turn_provider: Arc<dyn crate::provider::LlmClient> =
            Arc::new(FixedModelProvider {
                model: "default-model",
            });
        let agent = AgentLoop::new(
            AgentLoopConfig {
                provider: turn_provider.clone(),
                tools: registry,
                workflow: workflow,
                sandbox: sandbox,
                safety_mode: SafetyMode::Autonomous,
                context_manager: context::ContextManager::new(128_000, 0.5),
                memory: None,
                vision: None,
            },
            crate::project::Constitution::default(),
        )
        .with_model_resolver(resolver);
        (agent, turn_provider)
    };

    // Slot set → the summary provider is the slot's model.
    let (agent, turn) = make_loop(Some(ModelRef {
        endpoint: "openai".into(),
        model: "cheap-summary".into(),
        reasoning_effort: None,
    }));
    let summary = agent.summarize_provider(&turn);
    assert_eq!(
        summary.model(),
        "cheap-summary",
        "with [models.summarize] set, summaries ride the slot's model"
    );

    // Slot unset → the turn's provider, unchanged (today's behavior).
    let (agent, turn) = make_loop(None);
    let summary = agent.summarize_provider(&turn);
    assert_eq!(
        summary.model(),
        "default-model",
        "unset slot → the turn's provider (no config, no change)"
    );
}

#[tokio::test]
async fn explicit_picker_provider_beats_the_config_override_chain() {
    // Regression (2026-08-22 user report): "I stopped you and set it to
    // deepseek but it you continue in kimi." The per-agent model picker
    // (`set_model` with an `agent_id`) only swapped the loop's DEFAULT
    // provider slot, while `resolve_turn_provider` consults the config-backed
    // override chain FIRST — so a configured `[models.planning]`/executing
    // override silently discarded the picker's choice on the next turn and
    // the agent kept running on the old model. The fix pins the picker's
    // provider via `set_explicit_provider`, which resolution returns BEFORE
    // the chain. This test fails on the pre-fix code (the resolver's "o3"
    // override wins) and passes after.
    use crate::agent::context::ContextManager;
    use crate::config::{Endpoint, GeneralConfig, ModelRef, ModelsConfig};
    use crate::model_resolver::ConfigModelResolver;
    use crate::workflow::WorkflowState;

    /// A provider whose `model()` returns a fixed id — tells the test which
    /// model resolved (and never completes; resolution only inspects the id).
    struct FixedModelProvider {
        model: &'static str,
    }
    #[async_trait::async_trait]
    impl crate::provider::LlmClient for FixedModelProvider {
        fn capabilities(&self) -> &Capabilities {
            const C: Capabilities = Capabilities {
                supports_tool_choice: true,
                supports_strict_schema: true,
                supports_parallel_tools: true,
                reliable_finish_reason: true,
                max_context: 128_000,
                max_output_tokens: 4096,
                multimodal: false,
            };
            &C
        }
        fn kind(&self) -> crate::provider::ProviderKind {
            crate::provider::ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            self.model
        }
        async fn complete(
            &self,
            _messages: &[crate::provider::Message],
            _tools: &[crate::provider::ToolSchema],
            _tool_choice: Option<crate::provider::ToolChoice>,
        ) -> crate::error::Result<futures::stream::BoxStream<'_, crate::provider::LlmEvent>>
        {
            unreachable!("resolve_turn_provider must not call complete")
        }
    }

    let config = crate::config::Config {
        general: GeneralConfig {
            models: ModelsConfig {
                planning: Some(ModelRef {
                    endpoint: "openai".into(),
                    model: "o3".into(),
                    reasoning_effort: None,
                }),
                ..ModelsConfig::default()
            },
            ..GeneralConfig::default()
        },
        endpoints: vec![Endpoint {
            name: "openai".into(),
            base_url: "https://api.openai.com/v1/".into(),
            models: vec!["o3".into(), "deepseek-v4-flash".into()],
            ..Endpoint::test_default()
        }],
        pricing: vec![],
        keys: Default::default(),
        mcp: Vec::new(),
        projects: Default::default(),
    };
    let resolver = Arc::new(ConfigModelResolver::new(
        Arc::new(std::sync::RwLock::new(config)),
        Arc::new(crate::provider::trace::LlmRequestLog::new()),
    ));

    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    // The loop's default provider + the resolver's planning override both
    // resolve to "o3"-shaped models; the picker then pins deepseek.
    let default_provider: Arc<dyn crate::provider::LlmClient> = Arc::new(FixedModelProvider {
        model: "default-model",
    });
    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider: default_provider,
            tools: registry,
            workflow: workflow,
            sandbox: sandbox,
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(128_000, 0.5),
            memory: None,
            vision: None,
        },
        crate::project::Constitution::default(),
    )
    .with_model_resolver(resolver);

    // Sanity: WITHOUT a picker pin, planning resolves to the state override.
    let (provider, _cm) = agent
        .resolve_turn_provider(WorkflowState::Planning, None, None)
        .expect("state override should resolve without a pin");
    assert_eq!(
        provider.model(),
        "o3",
        "without a picker pin, the state override wins"
    );

    // The picker's per-agent switch: build the deepseek provider + pin it.
    let picked: Arc<dyn crate::provider::LlmClient> = Arc::new(FixedModelProvider {
        model: "deepseek-v4-flash",
    });
    agent.set_explicit_provider(picked, ContextManager::new(128_000, 0.5));

    // The pin wins over the state override. A skill context WITHOUT a
    // configured skill entry also keeps the pin (skill short-circuit only
    // fires when `[models.skill.<name>]` is set — see
    // `skill_override_beats_explicit_picker_pin` for the configured case).
    let (provider, _cm) = agent
        .resolve_turn_provider(WorkflowState::Planning, None, None)
        .expect("explicit provider should resolve");
    assert_eq!(
        provider.model(),
        "deepseek-v4-flash",
        "the picker's explicit provider must beat the config override chain"
    );
    let (provider, _cm) = agent
        .resolve_turn_provider(WorkflowState::Skill, Some("merge_to_main"), None)
        .expect("explicit provider should resolve");
    assert_eq!(
        provider.model(),
        "deepseek-v4-flash",
        "without a configured skill override the pin still applies on skill turns"
    );

    // The default slot is also updated (non-resolving paths + list_agents
    // report the picked model), like the pre-fix swap behavior.
    assert_eq!(agent.provider().model(), "deepseek-v4-flash");
}

/// A provider whose `model()` returns a fixed id — tells the test which
/// model resolved (and never completes; resolution only inspects the id).
/// Shared by the state-scoped pin tests below (each earlier test keeps its
/// own inner mock to stay self-contained).
struct FixedModelProvider {
    model: &'static str,
}
#[async_trait::async_trait]
impl crate::provider::LlmClient for FixedModelProvider {
    fn capabilities(&self) -> &Capabilities {
        const C: Capabilities = Capabilities {
            supports_tool_choice: true,
            supports_strict_schema: true,
            supports_parallel_tools: true,
            reliable_finish_reason: true,
            max_context: 128_000,
            max_output_tokens: 4096,
            multimodal: false,
        };
        &C
    }
    fn kind(&self) -> crate::provider::ProviderKind {
        crate::provider::ProviderKind::OpenAI
    }
    fn model(&self) -> &str {
        self.model
    }
    async fn complete(
        &self,
        _messages: &[crate::provider::Message],
        _tools: &[crate::provider::ToolSchema],
        _tool_choice: Option<crate::provider::ToolChoice>,
    ) -> crate::error::Result<futures::stream::BoxStream<'_, crate::provider::LlmEvent>> {
        unreachable!("resolve_turn_provider must not call complete")
    }
}

/// The endpoints + models lists every state-scoped pin test resolves
/// against — one OpenAI-shaped endpoint serving every model the tests name.
fn pin_test_endpoint(models: Vec<&'static str>) -> crate::config::Endpoint {
    crate::config::Endpoint {
        name: "openai".into(),
        base_url: "https://api.openai.com/v1/".into(),
        models: models.into_iter().map(Into::into).collect(),
        ..crate::config::Endpoint::test_default()
    }
}

#[tokio::test]
async fn picker_pin_yields_to_configured_model_on_state_change() {
    // Regression (2026-12-20 user report): "on state change the configured
    // agent should take over." A picker pin (picked mid-Planning) used to
    // outrank the `[models.*]` chain in EVERY state forever, so a chat that
    // had once used the model picker never switched to
    // `[models.executing]` after create_plan. The pin is now STATE-SCOPED:
    // it holds in the workflow state it was first served in (still beating
    // that state's own slot — the 2026-08-22 guarantee), and a configured
    // slot for a DIFFERENT state takes over. The pin is dormant, not
    // deleted: returning to its state resumes it. This test fails on the
    // pre-fix code (the second resolve returns deepseek instead of the
    // executing override) and passes after.
    use crate::agent::context::ContextManager;
    use crate::config::{GeneralConfig, ModelRef, ModelsConfig};
    use crate::model_resolver::ConfigModelResolver;
    use crate::workflow::WorkflowState;

    let config = crate::config::Config {
        general: GeneralConfig {
            models: ModelsConfig {
                planning: Some(ModelRef {
                    endpoint: "openai".into(),
                    model: "o3".into(),
                    reasoning_effort: None,
                }),
                executing: Some(ModelRef {
                    endpoint: "openai".into(),
                    model: "gpt-5-codex".into(),
                    reasoning_effort: None,
                }),
                ..ModelsConfig::default()
            },
            ..GeneralConfig::default()
        },
        endpoints: vec![pin_test_endpoint(vec![
            "o3",
            "gpt-5-codex",
            "deepseek-v4-flash",
        ])],
        ..crate::config::Config::default()
    };
    let resolver = Arc::new(ConfigModelResolver::new(
        Arc::new(std::sync::RwLock::new(config)),
        Arc::new(crate::provider::trace::LlmRequestLog::new()),
    ));

    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());
    let default_provider: Arc<dyn crate::provider::LlmClient> = Arc::new(FixedModelProvider {
        model: "default-model",
    });
    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider: default_provider,
            tools: registry,
            workflow: workflow,
            sandbox: sandbox,
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(128_000, 0.5),
            memory: None,
            vision: None,
        },
        crate::project::Constitution::default(),
    )
    .with_model_resolver(resolver);

    // The picker pins deepseek while the agent is in Planning.
    let picked: Arc<dyn crate::provider::LlmClient> = Arc::new(FixedModelProvider {
        model: "deepseek-v4-flash",
    });
    agent.set_explicit_provider(picked, ContextManager::new(128_000, 0.5));

    // In the pin's own state it still beats the planning slot (2026-08-22).
    let (provider, _cm) = agent
        .resolve_turn_provider(WorkflowState::Planning, None, None)
        .expect("pin should resolve in its own state");
    assert_eq!(
        provider.model(),
        "deepseek-v4-flash",
        "in the state it was picked in, the pin still beats the state override"
    );

    // create_plan flips Planning → Executing: the CONFIGURED executing
    // model takes over (the user-reported fix).
    let (provider, _cm) = agent
        .resolve_turn_provider(WorkflowState::Executing, None, None)
        .expect("executing override should resolve");
    assert_eq!(
        provider.model(),
        "gpt-5-codex",
        "on a state change, the configured [models.executing] model takes over"
    );

    // The pin is dormant, not deleted: abandon_plan back to Planning
    // resumes it.
    let (provider, _cm) = agent
        .resolve_turn_provider(WorkflowState::Planning, None, None)
        .expect("pin should resolve again back in its state");
    assert_eq!(
        provider.model(),
        "deepseek-v4-flash",
        "the dormant pin resumes when the workflow returns to its state"
    );
}

#[tokio::test]
async fn picker_pin_yields_to_bug_fixing_slot_in_a_bug_plan() {
    // Review LOW-2 (2027-01-07, bug-fixing model slot): the pin_holds probe
    // now receives the active plan's kind, so a picker pin (picked mid-
    // Planning) goes dormant when a bug_fixing plan's Executing resolves a
    // configured [models.bug_fixing] slot — identical to the executing-slot
    // takeover on a plain state change. Without the plan_kind threading, a
    // regression reverting the probe to None would let the pin survive into
    // the bug plan's Executing despite the configured slot, silently
    // serving the wrong model. This test fails on such a revert (the
    // second resolve returns the pin's deepseek instead of the bug-fixing
    // slot) and passes after.
    use crate::agent::context::ContextManager;
    use crate::config::{GeneralConfig, ModelRef, ModelsConfig};
    use crate::model_resolver::ConfigModelResolver;
    use crate::workflow::{PlanKind, WorkflowState};

    let config = crate::config::Config {
        general: GeneralConfig {
            models: ModelsConfig {
                planning: Some(ModelRef {
                    endpoint: "openai".into(),
                    model: "o3".into(),
                    reasoning_effort: None,
                }),
                bug_fixing: Some(ModelRef {
                    endpoint: "openai".into(),
                    model: "gpt-5-codex".into(),
                    reasoning_effort: None,
                }),
                ..ModelsConfig::default()
            },
            ..GeneralConfig::default()
        },
        endpoints: vec![pin_test_endpoint(vec![
            "o3",
            "gpt-5-codex",
            "deepseek-v4-flash",
        ])],
        ..crate::config::Config::default()
    };
    let resolver = Arc::new(ConfigModelResolver::new(
        Arc::new(std::sync::RwLock::new(config)),
        Arc::new(crate::provider::trace::LlmRequestLog::new()),
    ));
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());
    let default_provider: Arc<dyn crate::provider::LlmClient> = Arc::new(FixedModelProvider {
        model: "default-model",
    });
    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider: default_provider,
            tools: registry,
            workflow: workflow,
            sandbox: sandbox,
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(128_000, 0.5),
            memory: None,
            vision: None,
        },
        crate::project::Constitution::default(),
    )
    .with_model_resolver(resolver);

    // The picker pins deepseek while the agent is in Planning.
    let picked: Arc<dyn crate::provider::LlmClient> = Arc::new(FixedModelProvider {
        model: "deepseek-v4-flash",
    });
    agent.set_explicit_provider(picked, ContextManager::new(128_000, 0.5));
    // In the pin's own state it still beats the planning slot (2026-08-22).
    let (provider, _cm) = agent
        .resolve_turn_provider(WorkflowState::Planning, None, None)
        .expect("pin should resolve in its own state");
    assert_eq!(
        provider.model(),
        "deepseek-v4-flash",
        "in the state it was picked in, the pin still beats the state override"
    );

    // create_plan(kind=bug_fixing) flips Planning → Executing with an
    // active bug plan: the CONFIGURED bug-fixing slot takes over (the pin
    // goes dormant — the pin_holds probe sees the plan kind).
    let (provider, _cm) = agent
        .resolve_turn_provider(WorkflowState::Executing, None, Some(PlanKind::BugFixing))
        .expect("bug-fixing slot should resolve");
    assert_eq!(
        provider.model(),
        "gpt-5-codex",
        "in a bug plan's Executing, the configured [models.bug_fixing] model takes over"
    );

    // The same Executing state under an IMPLEMENTATION plan resolves no
    // slot here (no [models.executing] configured) → the pin resumes.
    let (provider, _cm) = agent
        .resolve_turn_provider(
            WorkflowState::Executing,
            None,
            Some(PlanKind::Implementation),
        )
        .expect("pin should resume for an implementation plan");
    assert_eq!(
        provider.model(),
        "deepseek-v4-flash",
        "without a configured slot for the context, the dormant pin resumes"
    );

    // The pin is dormant, not deleted: back in Planning it serves again.
    let (provider, _cm) = agent
        .resolve_turn_provider(WorkflowState::Planning, None, None)
        .expect("pin should resolve again back in its state");
    assert_eq!(
        provider.model(),
        "deepseek-v4-flash",
        "the dormant pin resumes when the workflow returns to its state"
    );
}

#[tokio::test]
async fn picker_pin_survives_state_change_when_no_model_configured() {
    // The user's clarifying rule: the configured model takes over ONLY when
    // one is configured for the new state. With no [models.executing] /
    // [models.complete] slots, a pin picked in Planning keeps serving
    // through the state changes — falling to the default model would
    // discard the user's live choice for no configured gain.
    use crate::agent::context::ContextManager;
    use crate::config::{GeneralConfig, ModelRef, ModelsConfig};
    use crate::model_resolver::ConfigModelResolver;
    use crate::workflow::WorkflowState;

    let config = crate::config::Config {
        general: GeneralConfig {
            models: ModelsConfig {
                planning: Some(ModelRef {
                    endpoint: "openai".into(),
                    model: "o3".into(),
                    reasoning_effort: None,
                }),
                ..ModelsConfig::default()
            },
            ..GeneralConfig::default()
        },
        endpoints: vec![pin_test_endpoint(vec!["o3", "deepseek-v4-flash"])],
        ..crate::config::Config::default()
    };
    let resolver = Arc::new(ConfigModelResolver::new(
        Arc::new(std::sync::RwLock::new(config)),
        Arc::new(crate::provider::trace::LlmRequestLog::new()),
    ));

    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());
    let default_provider: Arc<dyn crate::provider::LlmClient> = Arc::new(FixedModelProvider {
        model: "default-model",
    });
    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider: default_provider,
            tools: registry,
            workflow: workflow,
            sandbox: sandbox,
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(128_000, 0.5),
            memory: None,
            vision: None,
        },
        crate::project::Constitution::default(),
    )
    .with_model_resolver(resolver);

    let picked: Arc<dyn crate::provider::LlmClient> = Arc::new(FixedModelProvider {
        model: "deepseek-v4-flash",
    });
    agent.set_explicit_provider(picked, ContextManager::new(128_000, 0.5));

    for (state, label) in [
        (WorkflowState::Planning, "the pin's own state"),
        (WorkflowState::Executing, "an unconfigured state"),
        (WorkflowState::Complete, "another unconfigured state"),
    ] {
        let (provider, _cm) = agent
            .resolve_turn_provider(state, None, None)
            .expect("pin should keep serving");
        assert_eq!(
            provider.model(),
            "deepseek-v4-flash",
            "with nothing configured for {label}, the pin keeps serving"
        );
    }
}

#[tokio::test]
async fn fresh_picker_pick_restamps_in_the_current_state() {
    // A NEW picker pick after the old pin went dormant is re-stamped with
    // the state it first serves in (set_explicit_provider resets the
    // stamp): picking while in Executing makes the pin an Executing pin —
    // it beats the executing slot there, and the next configured state
    // (Complete) still takes over from it.
    use crate::agent::context::ContextManager;
    use crate::config::{GeneralConfig, ModelRef, ModelsConfig};
    use crate::model_resolver::ConfigModelResolver;
    use crate::workflow::WorkflowState;

    let config = crate::config::Config {
        general: GeneralConfig {
            models: ModelsConfig {
                planning: Some(ModelRef {
                    endpoint: "openai".into(),
                    model: "o3".into(),
                    reasoning_effort: None,
                }),
                executing: Some(ModelRef {
                    endpoint: "openai".into(),
                    model: "gpt-5-codex".into(),
                    reasoning_effort: None,
                }),
                complete: Some(ModelRef {
                    endpoint: "openai".into(),
                    model: "o4-mini".into(),
                    reasoning_effort: None,
                }),
                ..ModelsConfig::default()
            },
            ..GeneralConfig::default()
        },
        endpoints: vec![pin_test_endpoint(vec![
            "o3",
            "gpt-5-codex",
            "o4-mini",
            "deepseek-v4-flash",
            "kimi-k3",
        ])],
        ..crate::config::Config::default()
    };
    let resolver = Arc::new(ConfigModelResolver::new(
        Arc::new(std::sync::RwLock::new(config)),
        Arc::new(crate::provider::trace::LlmRequestLog::new()),
    ));

    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());
    let default_provider: Arc<dyn crate::provider::LlmClient> = Arc::new(FixedModelProvider {
        model: "default-model",
    });
    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider: default_provider,
            tools: registry,
            workflow: workflow,
            sandbox: sandbox,
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(128_000, 0.5),
            memory: None,
            vision: None,
        },
        crate::project::Constitution::default(),
    )
    .with_model_resolver(resolver);

    // First pin, picked in Planning: serves Planning, yields to executing.
    let first: Arc<dyn crate::provider::LlmClient> = Arc::new(FixedModelProvider {
        model: "deepseek-v4-flash",
    });
    agent.set_explicit_provider(first, ContextManager::new(128_000, 0.5));
    let (provider, _cm) = agent
        .resolve_turn_provider(WorkflowState::Planning, None, None)
        .expect("first pin should serve in Planning");
    assert_eq!(provider.model(), "deepseek-v4-flash");
    let (provider, _cm) = agent
        .resolve_turn_provider(WorkflowState::Executing, None, None)
        .expect("executing override should resolve");
    assert_eq!(
        provider.model(),
        "gpt-5-codex",
        "the executing slot takes over from the Planning pin"
    );

    // The user picks again WHILE in Executing: the new pin is an Executing
    // pin — it beats the executing slot there.
    let second: Arc<dyn crate::provider::LlmClient> =
        Arc::new(FixedModelProvider { model: "kimi-k3" });
    agent.set_explicit_provider(second, ContextManager::new(128_000, 0.5));
    let (provider, _cm) = agent
        .resolve_turn_provider(WorkflowState::Executing, None, None)
        .expect("fresh pick should serve in the state it was made in");
    assert_eq!(
        provider.model(),
        "kimi-k3",
        "a fresh pick is an Executing pin: it beats the executing slot there"
    );

    // finish flips Executing → Complete: the configured complete slot still
    // takes over from the re-stamped pin.
    let (provider, _cm) = agent
        .resolve_turn_provider(WorkflowState::Complete, None, None)
        .expect("complete override should resolve");
    assert_eq!(
        provider.model(),
        "o4-mini",
        "the configured [models.complete] model takes over from the restamped pin"
    );
}

#[tokio::test]
async fn set_explicit_provider_immediate_when_context_same_or_larger() {
    // When the new provider's max_context >= the current one, the swap is
    // immediate (no deferral). has_pending_swap() is false.
    struct CtxProvider {
        model: &'static str,
        max_ctx: usize,
    }
    #[async_trait::async_trait]
    impl crate::provider::LlmClient for CtxProvider {
        fn capabilities(&self) -> &crate::provider::Capabilities {
            // Box::leak is fine for tests — the provider lives for the test's
            // duration and the Capabilities are tiny.
            Box::leak(Box::new(crate::provider::Capabilities {
                supports_tool_choice: true,
                supports_strict_schema: true,
                supports_parallel_tools: true,
                reliable_finish_reason: true,
                max_context: self.max_ctx,
                max_output_tokens: 4096,
                multimodal: false,
            }))
        }
        fn kind(&self) -> crate::provider::ProviderKind {
            crate::provider::ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            self.model
        }
        async fn complete(
            &self,
            _messages: &[crate::provider::Message],
            _tools: &[crate::provider::ToolSchema],
            _tool_choice: Option<crate::provider::ToolChoice>,
        ) -> crate::error::Result<futures::stream::BoxStream<'_, crate::provider::LlmEvent>>
        {
            unimplemented!("not needed for swap tests")
        }
    }

    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());
    let provider: Arc<dyn crate::provider::LlmClient> = Arc::new(CtxProvider {
        model: "original",
        max_ctx: 128_000,
    });
    let agent = AgentLoop::new(
        test_config(provider, registry, workflow, sandbox),
        crate::project::Constitution::default(),
    );
    let new_provider: Arc<dyn crate::provider::LlmClient> = Arc::new(CtxProvider {
        model: "new-model",
        max_ctx: 128_000,
    });
    agent.set_explicit_provider(new_provider, context::ContextManager::new(128_000, 0.5));
    assert!(
        !agent.has_pending_swap(),
        "same context → immediate swap, no pending"
    );
    assert_eq!(agent.provider().model(), "new-model");
}

#[tokio::test]
async fn set_explicit_provider_deferred_when_context_smaller() {
    // When the new provider's max_context < the current one, the swap is
    // deferred. has_pending_swap() is true, provider unchanged, take clears it.
    struct CtxProvider {
        model: &'static str,
        max_ctx: usize,
    }
    #[async_trait::async_trait]
    impl crate::provider::LlmClient for CtxProvider {
        fn capabilities(&self) -> &crate::provider::Capabilities {
            Box::leak(Box::new(crate::provider::Capabilities {
                supports_tool_choice: true,
                supports_strict_schema: true,
                supports_parallel_tools: true,
                reliable_finish_reason: true,
                max_context: self.max_ctx,
                max_output_tokens: 4096,
                multimodal: false,
            }))
        }
        fn kind(&self) -> crate::provider::ProviderKind {
            crate::provider::ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            self.model
        }
        async fn complete(
            &self,
            _messages: &[crate::provider::Message],
            _tools: &[crate::provider::ToolSchema],
            _tool_choice: Option<crate::provider::ToolChoice>,
        ) -> crate::error::Result<futures::stream::BoxStream<'_, crate::provider::LlmEvent>>
        {
            unimplemented!("not needed for swap tests")
        }
    }

    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());
    let provider: Arc<dyn crate::provider::LlmClient> = Arc::new(CtxProvider {
        model: "big-model",
        max_ctx: 200_000,
    });
    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider: provider,
            tools: registry,
            workflow: workflow,
            sandbox: sandbox,
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(200_000, 0.5),
            memory: None,
            vision: None,
        },
        crate::project::Constitution::default(),
    );
    let small_provider: Arc<dyn crate::provider::LlmClient> = Arc::new(CtxProvider {
        model: "small-model",
        max_ctx: 64_000,
    });
    agent.set_explicit_provider(small_provider, context::ContextManager::new(64_000, 0.5));
    assert!(agent.has_pending_swap(), "smaller context → deferred swap");
    assert_eq!(
        agent.provider().model(),
        "big-model",
        "provider unchanged after deferral"
    );
    let pending = agent.take_pending_swap();
    assert!(pending.is_some(), "take returns the pending swap");
    assert_eq!(pending.unwrap().model, "small-model");
    assert!(!agent.has_pending_swap(), "take clears the pending swap");
}

#[tokio::test]
async fn set_explicit_provider_clears_stale_deferred_swap_on_immediate() {
    // LOW 1 regression test: a deferred swap (smaller context B) must be
    // cleared when a later immediate swap (larger/equal context C) supersedes
    // it. Without the `pending_swap = None` clear in the immediate path,
    // take_pending_swap would later return B, overriding C.
    struct CtxProvider {
        model: &'static str,
        max_ctx: usize,
    }
    #[async_trait::async_trait]
    impl crate::provider::LlmClient for CtxProvider {
        fn capabilities(&self) -> &crate::provider::Capabilities {
            Box::leak(Box::new(crate::provider::Capabilities {
                supports_tool_choice: true,
                supports_strict_schema: true,
                supports_parallel_tools: true,
                reliable_finish_reason: true,
                max_context: self.max_ctx,
                max_output_tokens: 4096,
                multimodal: false,
            }))
        }
        fn kind(&self) -> crate::provider::ProviderKind {
            crate::provider::ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            self.model
        }
        async fn complete(
            &self,
            _messages: &[crate::provider::Message],
            _tools: &[crate::provider::ToolSchema],
            _tool_choice: Option<crate::provider::ToolChoice>,
        ) -> crate::error::Result<futures::stream::BoxStream<'_, crate::provider::LlmEvent>>
        {
            unimplemented!("not needed for swap tests")
        }
    }

    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());
    let provider: Arc<dyn crate::provider::LlmClient> = Arc::new(CtxProvider {
        model: "original",
        max_ctx: 200_000,
    });
    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider: provider,
            tools: registry,
            workflow: workflow,
            sandbox: sandbox,
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(200_000, 0.5),
            memory: None,
            vision: None,
        },
        crate::project::Constitution::default(),
    );

    // Step 1: defer a swap to a smaller-context model B.
    let small_b: Arc<dyn crate::provider::LlmClient> = Arc::new(CtxProvider {
        model: "small-B",
        max_ctx: 64_000,
    });
    agent.set_explicit_provider(small_b, context::ContextManager::new(64_000, 0.5));
    assert!(agent.has_pending_swap(), "swap to B should be deferred");

    // Step 2: supersede with an immediate swap to a larger-context model C.
    let large_c: Arc<dyn crate::provider::LlmClient> = Arc::new(CtxProvider {
        model: "large-C",
        max_ctx: 200_000,
    });
    agent.set_explicit_provider(large_c, context::ContextManager::new(200_000, 0.5));

    // The stale deferred swap must be cleared — C is the active provider.
    assert!(
        !agent.has_pending_swap(),
        "immediate swap must clear stale deferred swap"
    );
    assert_eq!(
        agent.provider().model(),
        "large-C",
        "C is the active provider, not the stale deferred B"
    );
    assert!(
        agent.take_pending_swap().is_none(),
        "take returns None — stale B was cleared"
    );
}

#[tokio::test]
async fn run_turn_completes_deferred_swap_after_summarization() {
    // MEDIUM 3: the riskiest path — run_turn's pending-swap consumption.
    // Set up a deferred swap (smaller context), then run a turn. The turn
    // should: (1) take the pending swap, (2) summarize using the OLD provider,
    // (3) complete the swap, (4) run the turn with the NEW provider.
    //
    // Sizing: new context = 2_000 (summarize_at = 1_000). The conversation
    // must be > 80% of 2_000 = 1_600 tokens to trigger model-switch
    // summarization, but after summarization (keep 6) it must be < 1_000 to
    // avoid the regular summarization triggering again on the new provider.
    // Using diverse English text (~4 chars/token for tiktoken): 20 messages
    // × ~400 chars = ~8_000 chars = ~2_000 tokens > 1_600. After keep-6:
    // ~2_400 chars = ~600 tokens < 1_000. ✓
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    // A tool-free registry: the tools-schema block is part of the accounting
    // (and thus the summarize trigger) now, and an 8-tool schema block would
    // swamp this deliberately tiny 2_000-token sizing — after summarization
    // the trigger would re-fire and consume the new provider's queued
    // response. The deferred-swap flow under test needs no tools.
    let registry = Arc::new(ToolRegistry::new());

    // OLD provider: large context (200K). Its queue has the summary response.
    let old_provider = Arc::new(MockProvider {
        responses: Arc::new(Mutex::new({
            let mut dq = std::collections::VecDeque::new();
            dq.push_back(vec![
                LlmEvent::TextDelta {
                    text: "Summary of the conversation so far.".into(),
                },
                LlmEvent::Finish {
                    reason: FinishReason::Stop,
                },
            ]);
            dq
        })),
        caps: Capabilities {
            supports_tool_choice: true,
            supports_strict_schema: true,
            supports_parallel_tools: true,
            reliable_finish_reason: true,
            max_context: 200_000,
            max_output_tokens: 4096,
            multimodal: false,
        },
        tools_phases: Arc::new(StdMutex::new(Vec::new())),
        name: String::new(),
    });

    // NEW provider: smaller context (2K). Its queue has the turn response.
    let new_provider = Arc::new(MockProvider {
        responses: Arc::new(Mutex::new({
            let mut dq = std::collections::VecDeque::new();
            dq.push_back(vec![
                LlmEvent::TextDelta {
                    text: "Hello from new model!".into(),
                },
                LlmEvent::Finish {
                    reason: FinishReason::Stop,
                },
            ]);
            dq
        })),
        caps: Capabilities {
            supports_tool_choice: true,
            supports_strict_schema: true,
            supports_parallel_tools: true,
            reliable_finish_reason: true,
            max_context: 2_000,
            max_output_tokens: 4096,
            multimodal: false,
        },
        tools_phases: Arc::new(StdMutex::new(Vec::new())),
        name: String::new(),
    });

    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider: old_provider,
            tools: registry,
            workflow: workflow,
            sandbox: sandbox,
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(200_000, 0.5),
            memory: None,
            vision: None,
        },
        crate::project::Constitution::default(),
    );

    // Build messages: 1 system + 19 user/assistant, each ~400 chars of
    // diverse English text (tiktoken ~4 chars/token, not the efficient
    // single-char runs that "x".repeat() produces).
    let phrase = "The quick brown fox jumps over the lazy dog. ";
    let msg_text = phrase.repeat(10); // ~450 chars
    let mut messages = vec![Message::system("system prompt")];
    for i in 0..19 {
        let role = if i % 2 == 0 {
            Role::User
        } else {
            Role::Assistant
        };
        messages.push(Message::text(role, msg_text.clone()));
    }

    // Defer the swap: new provider has smaller context (2_000 < 200_000).
    agent.set_explicit_provider(
        new_provider.clone(),
        context::ContextManager::new(2_000, 0.5),
    );
    assert!(agent.has_pending_swap(), "swap should be deferred");
    assert_eq!(
        agent.provider().model(),
        "mock",
        "old provider still active"
    );

    // Run the turn — should summarize with old provider, then complete swap.
    let (fanin_tx, _fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let outcome = agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await
        .unwrap();

    // The turn ran on the NEW provider → its response text.
    assert_eq!(outcome.text, "Hello from new model!");
    // The swap completed — the new provider's caps (2_000) are active.
    assert_eq!(
        agent.provider().capabilities().max_context,
        2_000,
        "swap completed — new provider's caps are active"
    );
    // Summarization rewrote the messages — the summary should be present.
    assert!(
        messages
            .iter()
            .any(|m| { m.content.as_text().contains("## Conversation summary") }),
        "messages should contain the conversation summary after model-switch summarization"
    );
    // No pending swap after the turn.
    assert!(
        !agent.has_pending_swap(),
        "pending swap consumed after run_turn"
    );
}

#[tokio::test]
async fn run_turn_model_switch_interrupt_emits_paired_compact_event() {
    // LOW 2 regression test: when an interrupt arrives during model-switch
    // summarization, the turn must emit a paired end event for the
    // CompactStarted (an Error note), complete the swap, and return early.
    // Without the fix, CompactStarted would be dangling (no paired end).
    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    // OLD provider: large context. Uses a HANGING stream (yields one delta,
    // then pends forever) so the buffered Interrupt deterministically wins
    // the unbiased `select!` in summarize_with_interrupt. A synchronous
    // MockProvider stream would race the interrupt (~12% flake rate).
    struct HangingProvider {
        caps: Capabilities,
    }
    #[async_trait]
    impl crate::provider::LlmClient for HangingProvider {
        fn capabilities(&self) -> &crate::provider::Capabilities {
            &self.caps
        }
        fn kind(&self) -> crate::provider::ProviderKind {
            crate::provider::ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            "mock-hanging"
        }
        async fn complete(
            &self,
            _messages: &[crate::provider::Message],
            _tools: &[crate::provider::ToolSchema],
            _tool_choice: Option<crate::provider::ToolChoice>,
        ) -> crate::error::Result<futures::stream::BoxStream<'_, crate::provider::LlmEvent>>
        {
            use futures::StreamExt;
            let stream = futures::stream::once(async {
                crate::provider::LlmEvent::TextDelta {
                    text: "partial".into(),
                }
            })
            .chain(futures::stream::pending::<crate::provider::LlmEvent>());
            Ok(Box::pin(stream))
        }
    }
    let old_provider: Arc<dyn crate::provider::LlmClient> = Arc::new(HangingProvider {
        caps: Capabilities {
            supports_tool_choice: true,
            supports_strict_schema: true,
            supports_parallel_tools: true,
            reliable_finish_reason: true,
            max_context: 200_000,
            max_output_tokens: 4096,
            multimodal: false,
        },
    });

    // NEW provider: smaller context (2K). Its queue has the turn response
    // (should NOT be consumed — the interrupt returns early before the turn).
    let new_provider = Arc::new(MockProvider {
        responses: Arc::new(Mutex::new({
            let mut dq = std::collections::VecDeque::new();
            dq.push_back(vec![
                LlmEvent::TextDelta {
                    text: "Should not reach here".into(),
                },
                LlmEvent::Finish {
                    reason: FinishReason::Stop,
                },
            ]);
            dq
        })),
        caps: Capabilities {
            supports_tool_choice: true,
            supports_strict_schema: true,
            supports_parallel_tools: true,
            reliable_finish_reason: true,
            max_context: 2_000,
            max_output_tokens: 4096,
            multimodal: false,
        },
        tools_phases: Arc::new(StdMutex::new(Vec::new())),
        name: String::new(),
    });

    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider: old_provider,
            tools: registry,
            workflow: workflow,
            sandbox: sandbox,
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(200_000, 0.5),
            memory: None,
            vision: None,
        },
        crate::project::Constitution::default(),
    );

    // Build messages large enough to trigger model-switch summarization
    // (>80% of 2K = 1.6K tokens). 20 messages × ~450 chars ≈ ~2.1K tokens.
    let phrase = "The quick brown fox jumps over the lazy dog. ";
    let msg_text = phrase.repeat(10);
    let mut messages = vec![Message::system("system prompt")];
    for i in 0..19 {
        let role = if i % 2 == 0 {
            Role::User
        } else {
            Role::Assistant
        };
        messages.push(Message::text(role, msg_text.clone()));
    }

    // Defer the swap (smaller context).
    agent.set_explicit_provider(
        new_provider.clone(),
        context::ContextManager::new(2_000, 0.5),
    );

    // Push an interrupt command BEFORE run_turn — it will be received by
    // summarize_with_interrupt's select! loop, aborting the summary.
    let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
    let (cmd_tx, mut cmd_rx) = mpsc::channel(8);
    cmd_tx
        .send(crate::runtime::AgentCommand::Interrupt)
        .await
        .unwrap();

    let outcome = agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await
        .unwrap();

    // The turn returned early due to the interrupt.
    assert!(
        outcome.stop_reason.is_some(),
        "turn should return early with a stop reason on interrupt"
    );

    // Collect all events — there must be a CompactStarted AND a paired
    // Error event mentioning the interruption (no dangling CompactStarted).
    let mut events = Vec::new();
    while let Ok(ev) = fanin_rx.try_recv() {
        events.push(ev);
    }
    let has_compact_started = events
        .iter()
        .any(|(_, ev)| matches!(ev, AgentEvent::CompactStarted));
    let has_interrupt_note = events.iter().any(
        |(_, ev)| matches!(ev, AgentEvent::Error { error, .. } if error.contains("interrupt")),
    );
    assert!(
        has_compact_started,
        "CompactStarted should be emitted before summarization"
    );
    assert!(
        has_interrupt_note,
        "a paired Error event mentioning 'interrupt' should be emitted \
         (no dangling CompactStarted)"
    );

    // The swap completed despite the interrupt (the user chose this model).
    assert_eq!(
        agent.provider().capabilities().max_context,
        2_000,
        "swap completed even on interrupt"
    );
    assert!(
        !agent.has_pending_swap(),
        "pending swap consumed even on interrupt"
    );
}

/// A provider for the over-threshold re-compaction regression test: branches
/// on the request shape. The summarizer's one-message prompt (it contains
/// "summar") gets a short summary; every model request gets the same
/// `file_read` call whose result is the big file the test writes — so the
/// context stays over the threshold even when the kept-verbatim tail
/// shrinks to a single message. Counts both call kinds; after 12 model
/// responses it returns Stop so an unguarded (pre-fix) turn still
/// terminates instead of hanging the test.
struct BranchMock {
    caps: Capabilities,
    summarizer_calls: StdMutex<usize>,
    model_calls: StdMutex<usize>,
}

#[async_trait]
impl LlmClient for BranchMock {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
    fn kind(&self) -> ProviderKind {
        ProviderKind::OpenAI
    }
    fn model(&self) -> &str {
        "mock"
    }
    fn provider_name(&self) -> &str {
        ""
    }
    fn record_tools_phase_ms(&self, _ms: u32) {}

    async fn complete(
        &self,
        messages: &[Message],
        _tools: &[ToolSchema],
        _tool_choice: Option<crate::provider::ToolChoice>,
    ) -> crate::error::Result<BoxStream<'_, LlmEvent>> {
        let is_summarizer = messages
            .last()
            .map(|m| m.content.as_text().to_lowercase().contains("summar"))
            .unwrap_or(false);
        let events = if is_summarizer {
            *self.summarizer_calls.lock().unwrap() += 1;
            vec![
                LlmEvent::TextDelta {
                    text: "## Conversation summary\n\ncompacted.".into(),
                },
                LlmEvent::Finish {
                    reason: FinishReason::Stop,
                },
            ]
        } else {
            let n = {
                let mut calls = self.model_calls.lock().unwrap();
                *calls += 1;
                *calls
            };
            if n > 12 {
                // Pre-fix the turn would loop forever (every iteration
                // re-compacts and re-requests); stop it so the test fails
                // on assertions instead of hanging.
                vec![LlmEvent::Finish {
                    reason: FinishReason::Stop,
                }]
            } else {
                vec![
                    // Vary the text per round so the cross-turn repetition
                    // guard (Fix B) doesn't trip — this test isolates the
                    // compaction ladder (Fix A).
                    LlmEvent::TextDelta {
                        text: format!("attempt {n}"),
                    },
                    LlmEvent::ToolCallStart {
                        index: 0,
                        id: format!("call_{n}"),
                        name: "file_read".into(),
                    },
                    LlmEvent::ToolCallArgumentDelta {
                        index: 0,
                        fragment: r#"{"path":"test.txt"}"#.into(),
                    },
                    LlmEvent::Finish {
                        reason: FinishReason::ToolCalls,
                    },
                ]
            }
        };
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

#[tokio::test]
async fn over_threshold_turn_bounds_compaction_attempts() {
    // Regression (2027-01-07 live incident): a turn whose context stays
    // over the summarize threshold re-compacts EVERY iteration — each cycle
    // burns a summarizer LLM call plus a model request on a nearly
    // identical context ([system, summary, same kept-verbatim tail]),
    // which also drives byte-identical re-emission of the same tool call.
    // Observed live: ~114 LLM requests over 95 minutes with no new work,
    // and a 5,526,459-token request rejected 400 (limit 1,048,576).
    //
    // Sizing: window 20_000 (summarize_at = 10_000). The file_read result
    // is ~50K chars of diverse English (~11K tokens) — over the threshold
    // on its own, so even the escalated keep_recent=1 tail stays over and
    // the ladder must abort the turn instead of looping forever.
    let dir = tempdir().unwrap();
    let phrase = "The quick brown fox jumps over the lazy dog. ";
    std::fs::write(dir.path().join("test.txt"), phrase.repeat(1_150)).unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    let provider = Arc::new(BranchMock {
        caps: Capabilities {
            supports_tool_choice: true,
            supports_strict_schema: true,
            supports_parallel_tools: true,
            reliable_finish_reason: true,
            max_context: 20_000,
            max_output_tokens: 4096,
            multimodal: false,
        },
        summarizer_calls: StdMutex::new(0),
        model_calls: StdMutex::new(0),
    });

    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider: provider.clone(),
            tools: registry,
            workflow,
            sandbox,
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(20_000, 0.5),
            memory: None,
            vision: None,
        },
        crate::project::Constitution::default(),
    );

    let (fanin_tx, mut fanin_rx) = mpsc::channel(256);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    // Seed a few text exchanges BEFORE the tool loop: Rule 4 (never trim
    // inside an open tool loop) otherwise blocks compaction entirely for a
    // pure tool-loop conversation, and the test would exercise the wrong
    // defect (the count ballooning with compaction unable to run). With the
    // seed, the cut lands in the text region and the re-compaction loop
    // engages: every iteration summarizes, the kept-verbatim tool-loop tail
    // keeps the count over the threshold, and the next iteration re-compacts.
    let mut messages = vec![Message::user_text("read test.txt")];
    for i in 0..3 {
        messages.push(Message::text(
            Role::Assistant,
            format!("note {i}: {}", phrase.repeat(20)),
        ));
        messages.push(Message::user_text(format!("ack {i}")));
    }

    let outcome = agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await
        .unwrap();

    // The bound: a runaway tool loop that compaction CAN fix (the ingestion
    // caps shrink every result, so each cycle lands under the threshold and
    // re-arms the attempt budget) must still terminate — the per-turn TOTAL
    // ceiling stops it with a clear error instead of re-compacting forever.
    // Before the sendable post-condition existed this fixture exhausted the
    // stuck ladder instead; compaction now genuinely fixes the size each cycle,
    // so the ladder never fills and the total ceiling is what bounds the burn.
    let summarizer_calls = *provider.summarizer_calls.lock().unwrap();
    assert!(
        summarizer_calls <= super::turn::MAX_COMPACTIONS_PER_TURN as usize,
        "the re-compaction loop must be bounded (≤{} summarizer calls per \
         turn), got {summarizer_calls}",
        super::turn::MAX_COMPACTIONS_PER_TURN
    );
    let mut saw_churn_error = false;
    let mut compacted_events = 0usize;
    let mut finished_events = 0usize;
    while let Ok((_, ev)) = fanin_rx.try_recv() {
        match ev {
            AgentEvent::Error { error, .. }
                if error.contains("unbounded re-compaction loop") =>
            {
                saw_churn_error = true;
            }
            AgentEvent::Compacted { .. } => compacted_events += 1,
            AgentEvent::Finished { .. } => finished_events += 1,
            _ => {}
        }
    }
    assert!(
        saw_churn_error,
        "hitting the per-turn compaction ceiling must abort the turn with a \
         clear error event"
    );
    assert!(
        compacted_events <= super::turn::MAX_COMPACTIONS_PER_TURN as usize,
        "Compacted events must be bounded too, got {compacted_events}"
    );
    assert_eq!(
        finished_events, 0,
        "the abort path must be Error-only — no trailing Finished (the \
         MAX_RETRIES terminal-exclusivity contract)"
    );
    assert_eq!(outcome.finish_reason, FinishReason::Stop);
}

#[tokio::test]
async fn identical_responses_trip_repetition_guard() {
    // Regression (2027-01-07 live incident): the model re-emitted the same
    // read_files call over and over — each iteration executed it, appended
    // the result, and the next iteration re-emitted the identical response
    // (pattern continuation on a nearly-identical context). The in-stream
    // R10 guard can't see it (each generation is short and clean); the
    // repetition lives ACROSS iterations. The turn-level guard must break
    // the turn on the 3rd identical consecutive response.
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("test.txt"), "file contents").unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    // The same file_read call ten times over — with FRESH provider-assigned
    // ids each round, because the guard must key on name+arguments (the
    // real re-emission had fresh ids), not on ids.
    let responses: Vec<Vec<LlmEvent>> = (0..10)
        .map(|i| {
            vec![
                LlmEvent::ToolCallStart {
                    index: 0,
                    id: format!("call_{i}"),
                    name: "file_read".into(),
                },
                LlmEvent::ToolCallArgumentDelta {
                    index: 0,
                    fragment: r#"{"path":"test.txt"}"#.into(),
                },
                LlmEvent::Finish {
                    reason: FinishReason::ToolCalls,
                },
            ]
        })
        .collect();
    let provider = Arc::new(MockProvider::sequence(responses));

    let agent = AgentLoop::new(
        test_config(provider, registry, workflow, sandbox.clone()),
        crate::project::Constitution::default(),
    );

    let (fanin_tx, mut fanin_rx) = mpsc::channel(256);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text("read test.txt")];

    let outcome = agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await
        .unwrap();

    // The guard trips on the third identical response: exactly two tool
    // results (the third call is NOT executed) and a clear error event.
    let tool_results = messages.iter().filter(|m| m.role == Role::Tool).count();
    assert_eq!(
        tool_results, 2,
        "the third identical response must not execute its tool call"
    );
    let mut saw_guard = false;
    let mut finished_events = 0usize;
    while let Ok((_, ev)) = fanin_rx.try_recv() {
        match ev {
            AgentEvent::Error { error, .. } if error.contains("repetition guard") => {
                saw_guard = true;
            }
            AgentEvent::Finished { .. } => finished_events += 1,
            _ => {}
        }
    }
    assert!(
        saw_guard,
        "the repetition guard must emit a clear error event"
    );
    assert_eq!(
        finished_events, 0,
        "the guard path must be Error-only — no trailing Finished (the \
         MAX_RETRIES terminal-exclusivity contract)"
    );
    assert_eq!(outcome.finish_reason, FinishReason::Stop);
}

#[tokio::test]
async fn legitimate_long_turn_compactions_reset_budget() {
    // L3 coverage (review round 1): the attempt-budget RESET — a turn that
    // legitimately crosses the fill rate repeatedly, each compaction
    // eventually landing under the threshold, must never hit the attempt-5
    // abort. Drives maybe_compact directly with pure TEXT messages (a
    // run_turn-shaped test cannot oscillate: the tool loop grows
    // monotonically and Rule 4's retreat keeps it verbatim, so it goes
    // stuck by design). Phase 1 pins the keep_recent=1 escalation's
    // shrinking effect: attempts 1-2 (keep_recent 6/3) keep the big text
    // tail over the threshold, attempt 3 (keep_recent=1) keeps one small
    // message and lands under, resetting the budget. Phase 2 crosses the
    // fill rate again with smaller pushes — each crossing must compact and
    // reset at attempt 1. If the reset regressed, the counter would climb
    // across crossings and the 5th would abort; if the escalation
    // regressed, phase 1 would abort at attempt 5.
    let dir = tempdir().unwrap();
    let phrase = "The quick brown fox jumps over the lazy dog. ";
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    let provider = Arc::new(BranchMock {
        caps: Capabilities {
            supports_tool_choice: true,
            supports_strict_schema: true,
            supports_parallel_tools: true,
            reliable_finish_reason: true,
            max_context: 40_000,
            max_output_tokens: 4096,
            multimodal: false,
        },
        summarizer_calls: StdMutex::new(0),
        model_calls: StdMutex::new(0),
    });

    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider: provider.clone(),
            tools: registry,
            workflow,
            sandbox,
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(40_000, 0.5),
            memory: None,
            vision: None,
        },
        crate::project::Constitution::default(),
    );
    let dyn_provider: Arc<dyn LlmClient> = provider.clone();
    let context_manager = context::ContextManager::new(40_000, 0.5);

    let (fanin_tx, _fanin_rx) = mpsc::channel(256);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut state = super::turn::TurnState::fresh();
    let mut messages = vec![Message::user_text("hello")];

    // One gated maybe_compact call — mirrors run_turn's trigger (only
    // when the count is at/over the fill-rate threshold).
    macro_rules! compact_if_over {
        () => {{
            let count = context::ContextManager::count_tokens(&messages);
            if count >= context_manager.effective_summarize_at() {
                let mut breakdown =
                    context::ContextManager::count_tokens_by_role(&messages);
                agent
                    .maybe_compact(
                        &mut state,
                        &mut messages,
                        &dyn_provider,
                        &context_manager,
                        &fanin_tx,
                        1,
                        None,
                        &mut cmd_rx,
                        count,
                        &mut breakdown,
                    )
                    .await
            } else {
                None
            }
        }};
    }

    // Phase 1 — the escalation: six ~26K-token text messages (~156K over
    // the 20K threshold). Attempts 1-2 (keep_recent 6 or 3) keep the big
    // tail over; attempt 3 (keep_recent=1) keeps one small message and
    // lands under, resetting the budget.
    for i in 0..6 {
        messages.push(Message::text(
            Role::Assistant,
            format!("note {i}: {}", phrase.repeat(2500)),
        ));
        messages.push(Message::user_text(format!("ack {i}")));
    }
    for _ in 0..6 {
        let outcome = compact_if_over!();
        assert!(
            outcome.is_none(),
            "the escalation cycle must land under and reset, not abort"
        );
    }

    // Phase 2 — repeated fill-rate crossings with ~3K-token pushes: each
    // crossing compacts at attempt 1 (the small tail lands under) and
    // resets. Two crossings happen within the rounds below.
    for round in 0..16 {
        messages.push(Message::text(
            Role::Assistant,
            format!("round {round}: {}", phrase.repeat(275)),
        ));
        messages.push(Message::user_text(format!("ack {round}")));
        let outcome = compact_if_over!();
        assert!(
            outcome.is_none(),
            "a legitimate long turn (each compaction landing under the \
             threshold) must never hit the attempt-5 abort"
        );
    }

    // The escalation cycle (3 calls) plus two phase-2 crossings — the
    // budget reset each time (otherwise the 5th attempt would have
    // aborted above).
    let summarizer_calls = *provider.summarizer_calls.lock().unwrap();
    assert!(
        summarizer_calls >= 5,
        "expected the escalation cycle plus repeated fill-rate crossings, \
         got {summarizer_calls}"
    );
}

#[tokio::test]
async fn skill_override_beats_explicit_picker_pin() {
    // Regression (2026-09-04 user report): after the 2026-08-22 picker-pin fix,
    // `resolve_turn_provider` returned the explicit picker pin BEFORE any
    // resolver lookup — so a configured `[models.skill.merge_to_main]` never
    // applied once the user had ever used the per-agent model picker. The
    // intended precedence is:
    //   skill override > picker pin > forced/subagent/state overrides
    // so a skill turn still runs on the skill's configured model even when a
    // pin is set, while the pin still beats state overrides (2026-08-22 stays
    // fixed). This test fails on the overcorrected pin-first code (returns the
    // pin model) and passes after the skill-before-pin reorder.
    use crate::agent::context::ContextManager;
    use crate::config::{Endpoint, GeneralConfig, ModelRef, ModelsConfig};
    use crate::model_resolver::ConfigModelResolver;
    use crate::workflow::WorkflowState;
    use std::collections::HashMap;

    /// A provider whose `model()` returns a fixed id — resolution only inspects
    /// the id, so `complete` is never reached.
    struct FixedModelProvider {
        model: &'static str,
    }
    #[async_trait::async_trait]
    impl crate::provider::LlmClient for FixedModelProvider {
        fn capabilities(&self) -> &Capabilities {
            const C: Capabilities = Capabilities {
                supports_tool_choice: true,
                supports_strict_schema: true,
                supports_parallel_tools: true,
                reliable_finish_reason: true,
                max_context: 128_000,
                max_output_tokens: 4096,
                multimodal: false,
            };
            &C
        }
        fn kind(&self) -> crate::provider::ProviderKind {
            crate::provider::ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            self.model
        }
        async fn complete(
            &self,
            _messages: &[crate::provider::Message],
            _tools: &[crate::provider::ToolSchema],
            _tool_choice: Option<crate::provider::ToolChoice>,
        ) -> crate::error::Result<futures::stream::BoxStream<'_, crate::provider::LlmEvent>>
        {
            unreachable!("resolve_turn_provider must not call complete")
        }
    }

    let mut skill_map = HashMap::new();
    skill_map.insert(
        "merge_to_main".into(),
        ModelRef {
            endpoint: "openai".into(),
            model: "skill-deepseek".into(),
            reasoning_effort: None,
        },
    );
    let config = crate::config::Config {
        general: GeneralConfig {
            models: ModelsConfig {
                // State override differs from both the pin and the skill model,
                // so we can assert pin still beats state while skill beats pin.
                planning: Some(ModelRef {
                    endpoint: "openai".into(),
                    model: "planning-o3".into(),
                    reasoning_effort: None,
                }),
                skill: skill_map,
                ..ModelsConfig::default()
            },
            ..GeneralConfig::default()
        },
        endpoints: vec![Endpoint {
            name: "openai".into(),
            base_url: "https://api.openai.com/v1/".into(),
            models: vec![
                "planning-o3".into(),
                "picker-kimi".into(),
                "skill-deepseek".into(),
            ],
            ..Endpoint::test_default()
        }],
        pricing: vec![],
        keys: Default::default(),
        mcp: Vec::new(),
        projects: Default::default(),
    };
    let resolver = Arc::new(ConfigModelResolver::new(
        Arc::new(std::sync::RwLock::new(config)),
        Arc::new(crate::provider::trace::LlmRequestLog::new()),
    ));

    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    let default_provider: Arc<dyn crate::provider::LlmClient> = Arc::new(FixedModelProvider {
        model: "default-model",
    });
    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider: default_provider,
            tools: registry,
            workflow: workflow,
            sandbox: sandbox,
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(128_000, 0.5),
            memory: None,
            vision: None,
        },
        crate::project::Constitution::default(),
    )
    .with_model_resolver(resolver);

    // Pin the picker to a model distinct from both the skill and state overrides.
    let picked: Arc<dyn crate::provider::LlmClient> = Arc::new(FixedModelProvider {
        model: "picker-kimi",
    });
    agent.set_explicit_provider(picked, ContextManager::new(128_000, 0.5));

    // 2026-08-22 must stay fixed: pin still beats a plain state override.
    let (provider, _cm) = agent
        .resolve_turn_provider(WorkflowState::Planning, None, None)
        .expect("pinned provider should resolve for state turns");
    assert_eq!(
        provider.model(),
        "picker-kimi",
        "picker pin must still beat state overrides (2026-08-22)"
    );

    // The regression: with an active skill that has a configured override,
    // the skill model must win over the pin.
    let (provider, _cm) = agent
        .resolve_turn_provider(WorkflowState::Skill, Some("merge_to_main"), None)
        .expect("skill override should resolve even with a picker pin");
    assert_eq!(
        provider.model(),
        "skill-deepseek",
        "configured [models.skill.merge_to_main] must beat the picker pin"
    );
}

#[tokio::test]
async fn run_turn_uses_resolved_override_provider() {
    // T1 (turn wiring): a turn whose resolver returns an override must talk to
    // the override provider, not the loop's default. We inject a canned
    // resolver that always returns a provider whose model() is "override-model",
    // and assert the turn's completion came from that provider (via the model
    // id recorded on the captured request).
    use crate::config::ModelRef;
    use crate::model_resolver::{ModelContext, ModelResolver};
    use std::sync::Mutex as StdMutex;

    /// A resolver that always returns a fixed provider (built once), ignoring
    /// the context. Records that `resolve` was called.
    struct CannedResolver {
        provider: Arc<dyn LlmClient>,
        called: StdMutex<bool>,
    }

    #[async_trait::async_trait]
    impl ModelResolver for CannedResolver {
        fn resolve(&self, _ctx: ModelContext<'_>) -> Option<ModelRef> {
            *self.called.lock().unwrap() = true;
            Some(ModelRef {
                endpoint: "canned".into(),
                model: "override-model".into(),
                reasoning_effort: None,
            })
        }
        fn build_turn_provider(
            &self,
            _model: &ModelRef,
            _fill_rate: f64,
        ) -> Option<(Arc<dyn LlmClient>, context::ContextManager)> {
            Some((
                self.provider.clone(),
                context::ContextManager::new(128_000, 0.5),
            ))
        }
    }

    /// A provider that records the model id it was asked for and returns a
    /// canned stop. Its `model()` returns "override-model" so we can tell it
    /// apart from the default.
    struct RecordingProvider {
        model: &'static str,
        caps: Capabilities,
    }

    #[async_trait::async_trait]
    impl LlmClient for RecordingProvider {
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
        fn kind(&self) -> ProviderKind {
            ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            self.model
        }
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _tool_choice: Option<crate::provider::ToolChoice>,
        ) -> crate::error::Result<BoxStream<'_, LlmEvent>> {
            Ok(Box::pin(futures::stream::iter(vec![
                LlmEvent::TextDelta {
                    text: "from-override".into(),
                },
                LlmEvent::Finish {
                    reason: FinishReason::Stop,
                },
            ])))
        }
    }

    let dir = tempdir().unwrap();
    let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
        dir.path().join("plans"),
    )));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());

    // The loop's DEFAULT provider is "default-model"; the resolver overrides
    // with "override-model".
    let default_provider: Arc<dyn LlmClient> = Arc::new(RecordingProvider {
        model: "default-model",
        caps: Capabilities::openai(),
    });
    let override_provider: Arc<dyn LlmClient> = Arc::new(RecordingProvider {
        model: "override-model",
        caps: Capabilities::openai(),
    });
    let resolver = Arc::new(CannedResolver {
        provider: override_provider,
        called: StdMutex::new(false),
    });

    let agent = AgentLoop::new(
        test_config(default_provider, registry, workflow, sandbox),
        crate::project::Constitution::default(),
    )
    .with_model_resolver(resolver.clone());

    let (fanin_tx, _fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text("hi")];

    let outcome = agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await
        .unwrap();

    // The resolver was consulted and the override provider's text came back.
    assert!(*resolver.called.lock().unwrap(), "resolver was consulted");
    assert_eq!(outcome.text, "from-override");
}

#[tokio::test]
async fn run_turn_switches_to_skill_model_after_midturn_skill_start() {
    // Regression (2026-08-18): the agent can call `skill_start` MID-TURN
    // (e.g. finish → Complete → skill_start("merge_to_main") all inside one
    // turn, as the merge_to_main skill run did). The per-context model
    // resolution used to snapshot the provider once at turn start, so every
    // request after the mid-turn skill_start kept the pre-skill model — the
    // `[models.skill.merge_to_main]` override was never applied. The fix
    // re-resolves the provider at the top of every loop iteration, so the
    // request AFTER the skill_start tool call must be served by the skill's
    // override provider. This test fails on the pre-fix code (round 2 hits
    // the default provider, whose text is "from-default") and passes after.
    use crate::config::ModelRef;
    use crate::model_resolver::{ModelContext, ModelResolver};
    use crate::skill::{SkillRegistry, SkillSpec};
    use crate::tool::workflow::skill::SkillStartTool;
    use crate::workflow::WorkflowState;
    use std::sync::Mutex as StdMutex2;

    /// A resolver that returns an override ONLY while the named skill is
    // active — mirrors ConfigModelResolver's `[models.skill.<name>]` lookup.
    struct SkillScopedResolver {
        provider: Arc<dyn LlmClient>,
        skill: &'static str,
        called: StdMutex2<bool>,
    }
    #[async_trait::async_trait]
    impl ModelResolver for SkillScopedResolver {
        fn resolve(&self, ctx: ModelContext<'_>) -> Option<ModelRef> {
            if ctx.skill_name == Some(self.skill) {
                *self.called.lock().unwrap() = true;
                Some(ModelRef {
                    endpoint: "skill-ep".into(),
                    model: "skill-model".into(),
                    reasoning_effort: None,
                })
            } else {
                None
            }
        }
        fn build_turn_provider(
            &self,
            _model: &ModelRef,
            _fill_rate: f64,
        ) -> Option<(Arc<dyn LlmClient>, context::ContextManager)> {
            if self.skill_model_active() {
                Some((
                    self.provider.clone(),
                    context::ContextManager::new(128_000, 0.5),
                ))
            } else {
                None
            }
        }
        // The resolver trait has no extra method; keep the gate inside
        // build via the same condition (a real resolver builds for whatever
        // resolve returned, and resolve only fires while the skill is
        // active, so the condition below is only reached in that case).
        // This helper exists purely to document the invariant.
    }
    impl SkillScopedResolver {
        fn skill_model_active(&self) -> bool {
            *self.called.lock().unwrap()
        }
    }

    /// Provider that emits a fixed text + Stop; distinguishes the skill
    /// override provider from the default by the text it returns.
    struct FixedTextProvider {
        text: &'static str,
        caps: Capabilities,
    }
    #[async_trait::async_trait]
    impl LlmClient for FixedTextProvider {
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
        fn kind(&self) -> ProviderKind {
            ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            "skill-model"
        }
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _tool_choice: Option<crate::provider::ToolChoice>,
        ) -> crate::error::Result<BoxStream<'_, LlmEvent>> {
            Ok(Box::pin(futures::stream::iter(vec![
                LlmEvent::TextDelta {
                    text: self.text.into(),
                },
                LlmEvent::Finish {
                    reason: FinishReason::Stop,
                },
            ])))
        }
    }

    let dir = tempdir().unwrap();
    let workflow = Arc::new(Mutex::new(Workflow::new(dir.path().join("plans"))));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let mut registry = ToolRegistry::new();
    // The skill registry with a skill available from Planning.
    let mut skills = SkillRegistry::new();
    skills.insert(SkillSpec {
        name: "merge_to_main".into(),
        available_in: vec![WorkflowState::Planning, WorkflowState::Complete],
        target_state: WorkflowState::Planning,
        tools: vec!["skill_end".into(), "file_read".into()],
        prompt: "Merge the branch into main.".into(),
    });
    let skills = Arc::new(skills);
    registry.register(Box::new(SkillStartTool::new(workflow.clone(), skills)));
    let registry = Arc::new(registry);

    // Default provider round 1: call skill_start mid-turn. Round 2 (only
    // reached on the broken pre-fix code): distinguishable text so the
    // final assertion fails loudly instead of vacuously passing.
    let default_provider = Arc::new(MockProvider::sequence(vec![
        vec![
            LlmEvent::ToolCallStart {
                index: 0,
                id: "call_skill".into(),
                name: "skill_start".into(),
            },
            LlmEvent::ToolCallArgumentDelta {
                index: 0,
                fragment: r#"{"skill":"merge_to_main"}"#.into(),
            },
            LlmEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ],
        vec![
            LlmEvent::TextDelta {
                text: "from-default".into(),
            },
            LlmEvent::Finish {
                reason: FinishReason::Stop,
            },
        ],
    ]));

    let resolver = Arc::new(SkillScopedResolver {
        provider: Arc::new(FixedTextProvider {
            text: "from-skill-model",
            caps: Capabilities::openai(),
        }),
        skill: "merge_to_main",
        called: StdMutex2::new(false),
    });

    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider: default_provider,
            tools: registry,
            workflow: workflow.clone(),
            sandbox: sandbox,
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(128_000, 0.5),
            memory: None,
            vision: None,
        },
        crate::project::Constitution::default(),
    )
    .with_model_resolver(resolver.clone());

    let (fanin_tx, _fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text("merge it")];

    let outcome = agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await
        .unwrap();

    // The skill is active and the skill override fired.
    assert_eq!(
        workflow.lock().await.state(),
        WorkflowState::Skill,
        "skill_start must have transitioned the workflow"
    );
    assert!(
        *resolver.called.lock().unwrap(),
        "the skill-scoped resolver must have been consulted"
    );
    // THE regression assertion: the request after mid-turn skill_start must
    // come from the skill override provider, not the default snapshot.
    assert_eq!(
        outcome.text, "from-skill-model",
        "the request after a mid-turn skill_start must be served by the skill's override model"
    );
}

#[tokio::test]
async fn mid_turn_skill_model_switch_emits_model_changed_events() {
    // Regression (2026-04-20, user report): when a mid-turn skill_start
    // switches the effective model via `[models.skill.<name>]`, the UI must
    // be told — the per-turn override path updated `resolved_model` but
    // never emitted ModelChanged, so the toolbar/tab kept showing the
    // pre-skill model ("the skill moved us to grok but the toolbar never
    // updated"). This test fails on pre-fix code (zero ModelChanged events)
    // and passes after: one event for the override, one for the revert when
    // the skill ends mid-turn.
    use crate::config::ModelRef;
    use crate::model_resolver::{ModelContext, ModelResolver};
    use crate::skill::{SkillRegistry, SkillSpec};
    use crate::tool::workflow::skill::{SkillEndTool, SkillStartTool};
    use crate::workflow::WorkflowState;

    /// Resolver returning an override only while `merge_to_main` is active —
    /// mirrors ConfigModelResolver's `[models.skill.merge_to_main]` lookup.
    /// The override carries a per-context reasoning_effort ("low") and
    /// implements `display_effort_for` like ConfigModelResolver does, so the
    /// flip test can assert the effort rides the ModelChanged events
    /// (backlog 51dab4da).
    struct SkillScopedResolver {
        provider: Arc<dyn LlmClient>,
        skill: &'static str,
    }
    #[async_trait::async_trait]
    impl ModelResolver for SkillScopedResolver {
        fn resolve(&self, ctx: ModelContext<'_>) -> Option<ModelRef> {
            if ctx.skill_name == Some(self.skill) {
                Some(ModelRef {
                    endpoint: "skill-ep".into(),
                    model: "skill-model".into(),
                    reasoning_effort: Some("low".into()),
                })
            } else {
                None
            }
        }
        fn build_turn_provider(
            &self,
            _model: &ModelRef,
            _fill_rate: f64,
        ) -> Option<(Arc<dyn LlmClient>, context::ContextManager)> {
            Some((
                self.provider.clone(),
                context::ContextManager::new(128_000, 0.5),
            ))
        }
        fn display_effort_for(&self, model: &ModelRef) -> Option<String> {
            // Mirrors ConfigModelResolver: the ref's override, display-space.
            model.reasoning_effort.clone()
        }
    }

    /// The skill override provider (model id "skill-model"): its single
    /// scripted request calls `skill_end`, so the override also REVERTS
    /// mid-turn and the default provider serves the next request.
    struct SkillModelProvider {
        caps: Capabilities,
    }
    #[async_trait::async_trait]
    impl LlmClient for SkillModelProvider {
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
        fn kind(&self) -> ProviderKind {
            ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            "skill-model"
        }
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _tool_choice: Option<crate::provider::ToolChoice>,
        ) -> crate::error::Result<BoxStream<'_, LlmEvent>> {
            Ok(Box::pin(futures::stream::iter(vec![
                LlmEvent::ToolCallStart {
                    index: 0,
                    id: "call_end".into(),
                    name: "skill_end".into(),
                },
                LlmEvent::ToolCallArgumentDelta {
                    index: 0,
                    fragment: "{}".into(),
                },
                LlmEvent::Finish {
                    reason: FinishReason::ToolCalls,
                },
            ])))
        }
    }

    let dir = tempdir().unwrap();
    let workflow = Arc::new(Mutex::new(Workflow::new(dir.path().join("plans"))));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let mut registry = ToolRegistry::new();
    let mut skills = SkillRegistry::new();
    skills.insert(SkillSpec {
        name: "merge_to_main".into(),
        available_in: vec![WorkflowState::Planning, WorkflowState::Complete],
        target_state: WorkflowState::Planning,
        tools: vec!["skill_end".into(), "file_read".into()],
        prompt: "Merge the branch into main.".into(),
    });
    let skills = Arc::new(skills);
    registry.register(Box::new(SkillStartTool::new(workflow.clone(), skills)));
    registry.register(Box::new(SkillEndTool::new(workflow.clone())));
    let registry = Arc::new(registry);

    // Default provider: round 1 calls skill_start mid-turn; round 2 (served
    // after the skill ends and the override reverts) returns distinguishable
    // text + Stop.
    let default_provider = Arc::new(MockProvider::sequence(vec![
        vec![
            LlmEvent::ToolCallStart {
                index: 0,
                id: "call_skill".into(),
                name: "skill_start".into(),
            },
            LlmEvent::ToolCallArgumentDelta {
                index: 0,
                fragment: r#"{"skill":"merge_to_main"}"#.into(),
            },
            LlmEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ],
        vec![
            LlmEvent::TextDelta {
                text: "back-to-default".into(),
            },
            LlmEvent::Finish {
                reason: FinishReason::Stop,
            },
        ],
    ]));

    let resolver = Arc::new(SkillScopedResolver {
        provider: Arc::new(SkillModelProvider {
            caps: Capabilities::openai(),
        }),
        skill: "merge_to_main",
    });

    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider: default_provider,
            tools: registry,
            workflow: workflow.clone(),
            sandbox: sandbox,
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(128_000, 0.5),
            memory: None,
            vision: None,
        },
        crate::project::Constitution::default(),
    )
    .with_model_resolver(resolver);

    let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text("merge it")];

    let outcome = agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await
        .unwrap();
    drop(fanin_tx);

    let mut events = Vec::new();
    while let Ok((_id, event)) = fanin_rx.try_recv() {
        events.push(event);
    }
    let model_events: Vec<(String, Option<String>)> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ModelChanged {
                model,
                reasoning_effort,
                ..
            } => Some((model.clone(), reasoning_effort.clone())),
            _ => None,
        })
        .collect();
    // THE regression assertions: the override emits ModelChanged("skill-model")
    // when the skill starts, and ModelChanged("mock") (the default provider's
    // model) when the skill ends and the resolution falls back. The override's
    // per-context effort ("low") rides the first event; the fallback carries
    // None (this test loop has no factory-stamped default effort — the UI
    // falls back) — backlog 51dab4da: the status bar must show the effort of
    // the model actually serving, never a stale value.
    assert_eq!(
        model_events,
        vec![
            ("skill-model".to_string(), Some("low".to_string())),
            ("mock".to_string(), None),
        ],
        "a mid-turn skill model switch must emit ModelChanged(override, its effort) then ModelChanged(default)"
    );
    assert_eq!(
        outcome.text, "back-to-default",
        "after skill_end the default provider must serve the next request"
    );
}

#[tokio::test]
async fn no_model_override_no_model_changed_events() {
    // Companion guard: with no per-context override in play, a turn must NOT
    // emit ModelChanged (no event spam — the label stays as-is when nothing
    // changed).
    let dir = tempdir().unwrap();
    let workflow = Arc::new(Mutex::new(Workflow::new(dir.path().join("plans"))));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());
    let provider = Arc::new(MockProvider::single(vec![
        LlmEvent::TextDelta { text: "hi".into() },
        LlmEvent::Finish {
            reason: FinishReason::Stop,
        },
    ]));
    let agent = AgentLoop::new(
        test_config(provider, registry, workflow, sandbox.clone()),
        crate::project::Constitution::default(),
    );

    let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text("hello")];
    agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await
        .unwrap();
    drop(fanin_tx);

    let mut model_changed_count = 0usize;
    while let Ok((_id, event)) = fanin_rx.try_recv() {
        if matches!(event, AgentEvent::ModelChanged { .. }) {
            model_changed_count += 1;
        }
    }
    assert_eq!(
        model_changed_count, 0,
        "no override in play → no ModelChanged events"
    );
}

#[tokio::test]
async fn auto_recall_emits_memory_recalled_event() {
    // Regression (2026-04-20, user request + review finding 1): the per-turn
    // AUTO-recall injected memories into the prompt invisibly — there must be
    // a notification when memory is accessed — but it must fire ONCE per
    // FRESH recall, not on every cache-reuse iteration. This is a
    // TWO-iteration turn: round 1 is a small file_read, kept small so this
    // test isolates the cache-reuse path (under the OLD code a > 4096-byte
    // result would have separately invalidated the cache — that invalidation
    // was removed 2026-08-20 and is covered by
    // large_tool_result_does_not_reemit_memory_recalled), round 2 reuses the
    // cached recall. Pre-fix code emits the event twice (once per iteration);
    // the fix emits exactly one.
    let dir = tempdir().unwrap();
    let workflow = Arc::new(Mutex::new(Workflow::new(dir.path().join("plans"))));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());
    // A tiny file for the round-1 tool call (small result keeps the cache).
    std::fs::write(dir.path().join("note.txt"), "hi").unwrap();

    // Seed the agent's own memory store (the auto-recall source) with a
    // memory the query "merge it" will hit via the FTS/keyword prefilter.
    let embedder: Arc<dyn crate::memory::Embedder> = Arc::new(HashEmbedder::new());
    let store: Arc<dyn crate::memory::MemoryStoreTrait> =
        Arc::new(MemoryStore::open_in_memory(embedder).unwrap());
    store
        .write(crate::memory::Memory::new(
            MemoryTier::Semantic,
            "merge instructions",
            "how to merge feature branches into main safely",
            1_700_000_000,
        ))
        .await
        .unwrap();

    let provider = Arc::new(MockProvider::sequence(vec![
        // Round 1: a small-output tool call.
        vec![
            LlmEvent::ToolCallStart {
                index: 0,
                id: "call_read".into(),
                name: "file_read".into(),
            },
            LlmEvent::ToolCallArgumentDelta {
                index: 0,
                fragment: r#"{"path":"note.txt"}"#.into(),
            },
            LlmEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ],
        // Round 2: plain text + Stop (served with the cached recall).
        vec![
            LlmEvent::TextDelta { text: "ok".into() },
            LlmEvent::Finish {
                reason: FinishReason::Stop,
            },
        ],
    ]));
    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider: provider,
            tools: registry,
            workflow: workflow,
            sandbox: sandbox.clone(),
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(128_000, 0.5),
            memory: Some(store),
            vision: None,
        },
        crate::project::Constitution::default(),
    );

    let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text("merge it")];
    let outcome = agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await
        .unwrap();
    drop(fanin_tx);

    let mut recalled: Vec<Vec<crate::runtime::channels::RecallHit>> = Vec::new();
    while let Ok((_id, event)) = fanin_rx.try_recv() {
        if let AgentEvent::MemoryRecalled { hits } = event {
            recalled.push(hits);
        }
    }
    assert_eq!(
        outcome.text, "ok",
        "round 2 must have run (two-iteration turn)"
    );
    assert_eq!(
        recalled.len(),
        1,
        "exactly one MemoryRecalled event — the second iteration reused the recall cache"
    );
    assert!(
        !recalled[0].is_empty(),
        "the event reports at least one hit (the seeded memory)"
    );
    assert_eq!(
        recalled[0][0].title, "merge instructions",
        "the top hit is the seeded memory"
    );
    assert_eq!(recalled[0][0].tier, "semantic", "tier rides along");
}

#[tokio::test]
async fn same_turn_memory_write_invalidates_auto_recall_cache() {
    // Regression (2026-09-08 memory review, suggestion 4): the per-turn
    // auto-recall cache was keyed ONLY on the user-query text — a memory
    // written MID-TURN (the agent recording a decision via memory_write)
    // was invisible to the next recall of the same query: the stale cache
    // served until summarize. The store's version counter (bumped on every
    // mutating op) now invalidates the cache. This is a TWO-iteration turn:
    // round 1 recalls query "merge it" (0 hits — the store starts empty),
    // then its tool call WRITES a memory matching that query (bumping the
    // store version); round 2 re-runs the same query — the version mismatch
    // forces a FRESH recall, so the mid-turn write appears (a
    // MemoryRecalled event; the old code emitted none — cache hit).
    let dir = tempdir().unwrap();
    let workflow = Arc::new(Mutex::new(Workflow::new(dir.path().join("plans"))));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());

    // The agent's memory store — EMPTY (the write happens mid-turn).
    let embedder: Arc<dyn crate::memory::Embedder> = Arc::new(HashEmbedder::new());
    let store: Arc<dyn crate::memory::MemoryStoreTrait> =
        Arc::new(MemoryStore::open_in_memory(embedder).unwrap());

    // A registry whose memory_write tool writes to the SAME store the
    // auto-recall reads (make_registry gives the memory tools their own
    // store — this test needs them shared).
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(MemoryWriteTool::new(store.clone())));
    let registry = Arc::new(registry);

    let provider = Arc::new(MockProvider::sequence(vec![
        // Round 1: the agent records a decision mid-turn (memory_write).
        vec![
            LlmEvent::ToolCallStart {
                index: 0,
                id: "call_write".into(),
                name: "memory_write".into(),
            },
            LlmEvent::ToolCallArgumentDelta {
                index: 0,
                fragment: r#"{"tier":"semantic","title":"merge instructions","content":"how to merge feature branches into main safely"}"#.into(),
            },
            LlmEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ],
        // Round 2: plain text + Stop (same user query — the version check
        // must force a fresh recall that sees the round-1 write).
        vec![
            LlmEvent::TextDelta { text: "ok".into() },
            LlmEvent::Finish {
                reason: FinishReason::Stop,
            },
        ],
    ]));
    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider: provider,
            tools: registry,
            workflow: workflow,
            sandbox: sandbox.clone(),
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(128_000, 0.5),
            memory: Some(store),
            vision: None,
        },
        crate::project::Constitution::default(),
    );

    let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text("merge it")];
    let outcome = agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await
        .unwrap();
    drop(fanin_tx);

    let mut recalled: Vec<Vec<crate::runtime::channels::RecallHit>> = Vec::new();
    while let Ok((_id, event)) = fanin_rx.try_recv() {
        if let AgentEvent::MemoryRecalled { hits } = event {
            recalled.push(hits);
        }
    }
    assert_eq!(
        outcome.text, "ok",
        "round 2 must have run (two-iteration turn)"
    );
    assert_eq!(
        recalled.len(),
        1,
        "the mid-turn write must invalidate the cache: round 2 re-recalls \
         and emits the event (old code: cache hit, no event)"
    );
    assert_eq!(
        recalled[0][0].title, "merge instructions",
        "the top hit is the memory written mid-turn"
    );
}

#[tokio::test]
async fn large_tool_result_does_not_reemit_memory_recalled() {
    // Regression (2026-08-20, "auto-recall after file read" user report): a
    // tool result > 4096 bytes used to invalidate the per-turn recall cache,
    // but the recall query is ONLY the latest User-role message text — tool
    // outputs are Tool-role messages and can never change it. The old
    // invalidation re-ran an identical recall on the next iteration and
    // re-emitted a duplicate MemoryRecalled transcript entry. The fix removes
    // the invalidation, so this TWO-iteration turn with a LARGE round-1 tool
    // result emits exactly one MemoryRecalled event (old code: two).
    let dir = tempdir().unwrap();
    let workflow = Arc::new(Mutex::new(Workflow::new(dir.path().join("plans"))));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());
    // A file whose file_read output exceeds the old 4 KiB invalidation
    // threshold (8 KiB of content → line-numbered output well above it).
    std::fs::write(dir.path().join("big.txt"), "x".repeat(8192)).unwrap();

    // Seed the agent's own memory store so the auto-recall has a hit.
    let embedder: Arc<dyn crate::memory::Embedder> = Arc::new(HashEmbedder::new());
    let store: Arc<dyn crate::memory::MemoryStoreTrait> =
        Arc::new(MemoryStore::open_in_memory(embedder).unwrap());
    store
        .write(crate::memory::Memory::new(
            MemoryTier::Semantic,
            "merge instructions",
            "how to merge feature branches into main safely",
            1_700_000_000,
        ))
        .await
        .unwrap();

    let provider = Arc::new(MockProvider::sequence(vec![
        // Round 1: a LARGE-output tool call (file read of big.txt).
        vec![
            LlmEvent::ToolCallStart {
                index: 0,
                id: "call_read".into(),
                name: "file_read".into(),
            },
            LlmEvent::ToolCallArgumentDelta {
                index: 0,
                fragment: r#"{"path":"big.txt"}"#.into(),
            },
            LlmEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ],
        // Round 2: plain text + Stop — under the OLD code this iteration ran
        // a FRESH recall (cache had been invalidated) and re-emitted the event.
        vec![
            LlmEvent::TextDelta { text: "ok".into() },
            LlmEvent::Finish {
                reason: FinishReason::Stop,
            },
        ],
    ]));
    let agent = AgentLoop::new(
        AgentLoopConfig {
            provider: provider,
            tools: registry,
            workflow: workflow,
            sandbox: sandbox.clone(),
            safety_mode: SafetyMode::Autonomous,
            context_manager: context::ContextManager::new(128_000, 0.5),
            memory: Some(store),
            vision: None,
        },
        crate::project::Constitution::default(),
    );

    let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
    let mut messages = vec![Message::user_text("merge it")];
    let outcome = agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await
        .unwrap();
    drop(fanin_tx);

    let mut recalled = 0usize;
    let mut large_result_seen = false;
    while let Ok((_id, event)) = fanin_rx.try_recv() {
        match event {
            AgentEvent::MemoryRecalled { .. } => recalled += 1,
            AgentEvent::ToolResult { result, .. } => {
                if result.output.len() > 4096 {
                    large_result_seen = true;
                }
            }
            _ => {}
        }
    }
    assert_eq!(
        outcome.text, "ok",
        "round 2 must have run (two-iteration turn)"
    );
    assert!(
        large_result_seen,
        "precondition: the round-1 tool result exceeded the old 4 KiB threshold"
    );
    assert_eq!(
        recalled, 1,
        "exactly one MemoryRecalled event — a large tool result must NOT \
         invalidate the recall cache (the query, the user message, is unchanged)"
    );
}
