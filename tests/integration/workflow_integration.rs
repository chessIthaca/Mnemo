// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Phase 4 exit-criteria tests — headless workflow + multi-agent fan-in.
//!
//! These tests verify the agent loop's core behaviors without a TUI:
//! - The Planning gate blocks write tools.
//! - A full workflow: create_plan → execute → complete_step.
//! - Resume on restart (reads plan file, continues at first unchecked step).
//! - Two agents spawn and stream events through the fan-in without blocking.

use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;
use mnemo::agent::context::ContextManager;
use mnemo::agent::AgentLoop;
use mnemo::config::SafetyMode;
use mnemo::error::Result;
use mnemo::memory::embedder::HashEmbedder;
use mnemo::memory::{MemoryStore, MemoryStoreTrait};
use mnemo::provider::ToolChoice;
use mnemo::provider::{
    Capabilities, FinishReason, LlmClient, LlmEvent, Message, ProviderKind, ToolSchema,
};
use mnemo::runtime::channels::AgentEvent;
use mnemo::runtime::AgentManager;
use mnemo::tool::agent::sandbox::Sandbox;
use mnemo::tool::agent::{file_read::FileReadTool, file_write::FileWriteTool};
use mnemo::tool::memory::{MemoryRecallTool, MemoryWriteTool};
use mnemo::tool::workflow::plan::{CompleteStepTool, CreatePlanTool, UpdatePlanTool};
use mnemo::tool::{Tool, ToolRegistry};
use mnemo::workflow::{Workflow, WorkflowState};
use tempfile::tempdir;
use tokio::sync::Mutex;

/// A mock provider with a sequence of canned responses.
struct MockProvider {
    responses: Arc<Mutex<std::collections::VecDeque<Vec<LlmEvent>>>>,
    caps: Capabilities,
}

impl MockProvider {
    fn sequence(responses: Vec<Vec<LlmEvent>>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses.into())),
            caps: Capabilities::openai(),
        }
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
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _tool_choice: Option<ToolChoice>,
    ) -> Result<BoxStream<'_, LlmEvent>> {
        let mut responses = self.responses.lock().await;
        let events = if let Some(front) = responses.pop_front() {
            front
        } else {
            vec![LlmEvent::Finish {
                reason: FinishReason::Stop,
            }]
        };
        drop(responses);
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

fn make_registry(sandbox: Sandbox, workflow: Arc<Mutex<Workflow>>) -> Arc<ToolRegistry> {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(FileReadTool::new(sandbox.clone())));
    registry.register(Box::new(FileWriteTool::new(sandbox)));
    registry.register(Box::new(CreatePlanTool::new(workflow.clone())));
    registry.register(Box::new(CompleteStepTool::new(workflow)));
    let embedder: Arc<dyn mnemo::memory::Embedder> = Arc::new(HashEmbedder::new());
    let store: Arc<dyn mnemo::memory::MemoryStoreTrait> =
        Arc::new(MemoryStore::open_in_memory(embedder).unwrap());
    registry.register(Box::new(MemoryWriteTool::new(store.clone())));
    registry.register(Box::new(MemoryRecallTool::new(store)));
    Arc::new(registry)
}

#[tokio::test]
async fn planning_gate_hides_write_tools_until_a_plan_exists() {
    // HARD RULE: no changes without a plan. In Planning state, write tools are
    // hidden (not merely approval-gated) so the agent cannot mutate the project
    // before a plan exists. Only read tools + create_plan are visible.
    let dir = tempdir().unwrap();
    let workflow = Workflow::new(dir.path().join("plans"));
    let registry = make_registry(
        Sandbox::new(dir.path()).unwrap(),
        Arc::new(Mutex::new(workflow)),
    );

    let caps = Capabilities::openai();
    let schemas = registry.schemas(&caps, &mnemo::tool::ToolFilter::Planning);

    let names: Vec<String> = schemas.iter().map(|s| s.name.clone()).collect();
    // file_read should be present (auto-run read).
    assert!(names.contains(&"file_read".to_string()));
    // create_plan should be present (the way forward).
    assert!(names.contains(&"create_plan".to_string()));
    // file_write is HIDDEN (a mutation) — no changes without a plan.
    assert!(!names.contains(&"file_write".to_string()));
    // complete_step is NOT available in Planning (no plan exists yet).
    assert!(!names.contains(&"complete_step".to_string()));
}

#[tokio::test]
async fn full_workflow_create_plan_execute_complete() {
    let dir = tempdir().unwrap();
    let workflow = Arc::new(Mutex::new(Workflow::new(dir.path().join("plans"))));
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let registry = make_registry((*sandbox).clone(), workflow.clone());
    let provider = Arc::new(MockProvider::sequence(vec![
        // Turn 1: create a plan.
        vec![
            LlmEvent::ToolCallStart {
                index: 0,
                id: "c1".into(),
                name: "create_plan".into(),
            },
            LlmEvent::ToolCallArgumentDelta {
                index: 0,
                fragment: r#"{"title":"T","goal":"G","context":"Integration fixture: write out.txt via src steps and verify by reading it back.","steps":["write out.txt at the project root per src/main.rs","done — no code changes"]}"#.into(),
            },
            LlmEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ],
        // Turn 2: write a file.
        vec![
            LlmEvent::ToolCallStart {
                index: 0,
                id: "c2".into(),
                name: "file_write".into(),
            },
            LlmEvent::ToolCallArgumentDelta {
                index: 0,
                fragment: r#"{"path":"out.txt","content":"hello"}"#.into(),
            },
            LlmEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ],
        // Turn 3: complete the step.
        vec![
            LlmEvent::ToolCallStart {
                index: 0,
                id: "c3".into(),
                name: "complete_step".into(),
            },
            LlmEvent::ToolCallArgumentDelta {
                index: 0,
                fragment: r#"{"step_index":1}"#.into(),
            },
            LlmEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ],
        // Turn 4: done.
        vec![
            LlmEvent::TextDelta {
                text: "All done!".into(),
            },
            LlmEvent::Finish {
                reason: FinishReason::Stop,
            },
        ],
    ]));

    let agent = AgentLoop::new(
        mnemo::agent::AgentLoopConfig {
            provider,
            tools: registry,
            workflow: workflow.clone(),
            sandbox,
            safety_mode: SafetyMode::Autonomous,
            context_manager: ContextManager::new(128_000, 0.5),
            memory: None,
            vision: None,
        },
        mnemo::project::Constitution::default(),
    );

    let (fanin_tx, _fanin_rx) = tokio::sync::mpsc::channel(64);
    let (_cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel(8);
    let mut messages = vec![Message::user_text("do the task")];

    let outcome = agent
        .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
        .await
        .unwrap();

    // The file should have been written.
    assert_eq!(
        std::fs::read_to_string(dir.path().join("out.txt")).unwrap(),
        "hello"
    );
    // The workflow should have progressed.
    let wf = workflow.lock().await;
    assert_eq!(wf.state(), WorkflowState::Executing);
    assert_eq!(wf.plan().unwrap().completed_count(), 1);
    assert_eq!(outcome.text, "All done!");
}

#[tokio::test]
async fn resume_on_restart() {
    let dir = tempdir().unwrap();
    let plans_dir = dir.path().join("plans");

    // Session 1: create a plan + complete step 0.
    {
        let workflow = Arc::new(Mutex::new(Workflow::new(plans_dir.clone())));
        let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
        let registry = make_registry((*sandbox).clone(), workflow.clone());
        let provider = Arc::new(MockProvider::sequence(vec![
            vec![
                LlmEvent::ToolCallStart {
                    index: 0,
                    id: "c1".into(),
                    name: "create_plan".into(),
                },
                LlmEvent::ToolCallArgumentDelta {
                    index: 0,
                    fragment: r#"{"title":"T","goal":"G","context":"Restart-resume fixture: two steps in src/main.rs, complete the first, then reload.","steps":["edit src/main.rs part one","edit src/main.rs part two"]}"#.into(),
                },
                LlmEvent::Finish {
                    reason: FinishReason::ToolCalls,
                },
            ],
            vec![
                LlmEvent::ToolCallStart {
                    index: 0,
                    id: "c2".into(),
                    name: "complete_step".into(),
                },
                LlmEvent::ToolCallArgumentDelta {
                    index: 0,
                    fragment: r#"{"step_index":1}"#.into(),
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
            mnemo::agent::AgentLoopConfig {
                provider,
                tools: registry,
                workflow,
                sandbox,
                safety_mode: SafetyMode::Autonomous,
                context_manager: ContextManager::new(128_000, 0.5),
                memory: None,
                vision: None,
            },
            mnemo::project::Constitution::default(),
        );
        let (fanin_tx, _fanin_rx) = tokio::sync::mpsc::channel(64);
        let (_cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel(8);
        let mut messages = vec![Message::user_text("start")];
        let _ = agent
            .run_turn(&mut messages, &fanin_tx, 1, &mut cmd_rx, None)
            .await
            .unwrap();
    }

    // Session 2: load the plan from disk — should resume at step 1 (Executing).
    let mut workflow = Workflow::new(plans_dir);
    workflow.load_latest().unwrap();
    assert_eq!(workflow.state(), WorkflowState::Executing);
    assert_eq!(workflow.current_step().unwrap().index, 1);
    assert_eq!(workflow.current_step().unwrap().text, "edit src/main.rs part two");
}

#[tokio::test]
async fn two_agents_fan_in_without_blocking() {
    // Verify the AgentManager can spawn two agents and fan-in their events.
    let mut mgr = AgentManager::new(64);
    let fanin_tx = mgr.fanin_sender();

    // Simulate two agents sending events concurrently.
    let fanin1 = fanin_tx.clone();
    let fanin2 = fanin_tx.clone();
    let h1 = tokio::spawn(async move {
        for i in 0..5 {
            fanin1
                .send((1, AgentEvent::TextDelta(format!("agent1-{i}"))))
                .await
                .unwrap();
        }
    });
    let h2 = tokio::spawn(async move {
        for i in 0..5 {
            fanin2
                .send((2, AgentEvent::TextDelta(format!("agent2-{i}"))))
                .await
                .unwrap();
        }
    });

    h1.await.unwrap();
    h2.await.unwrap();

    // Collect all events — should have 10, from both agents.
    let mut events = Vec::new();
    while let Ok(Some((id, event))) =
        tokio::time::timeout(std::time::Duration::from_millis(500), mgr.next_event()).await
    {
        events.push((id, event));
    }

    assert_eq!(events.len(), 10);
    let agent1_count = events.iter().filter(|(id, _)| *id == 1).count();
    let agent2_count = events.iter().filter(|(id, _)| *id == 2).count();
    assert_eq!(agent1_count, 5);
    assert_eq!(agent2_count, 5);
}

/// A mock provider that returns a single canned response set, then stops.
struct SingleResponseProvider {
    events: Vec<LlmEvent>,
    caps: Capabilities,
}

#[async_trait]
impl LlmClient for SingleResponseProvider {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
    fn kind(&self) -> ProviderKind {
        ProviderKind::OpenAI
    }
    fn model(&self) -> &str {
        "mock-single"
    }
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _tool_choice: Option<ToolChoice>,
    ) -> Result<BoxStream<'_, LlmEvent>> {
        Ok(Box::pin(futures::stream::iter(self.events.clone())))
    }
}

#[tokio::test]
async fn per_agent_workflows_are_independent_via_factory() {
    // The core multi-agent safety property, exercised end-to-end: two agents
    // built from the same AgentLoopFactory must have independent workflows.
    // Agent A creates a plan; agent B's workflow must be unaffected (still
    // Planning, no plan). Each agent's workflow_handle() returns its own plan.
    use mnemo::agent::context::ContextManager;
    use mnemo::agent::factory::AgentLoopFactory;
    use mnemo::config::SafetyMode;
    use mnemo::memory::embedder::HashEmbedder;
    use mnemo::memory::MemoryStore;
    use mnemo::project::ConstitutionSource;
    use mnemo::safety_rules::SafetyRules;
    use mnemo::tool::agent::sandbox::Sandbox;

    let dir = tempdir().unwrap();
    let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
    let embedder: Arc<dyn mnemo::memory::Embedder> = Arc::new(HashEmbedder::new());
    let store: Arc<dyn mnemo::memory::MemoryStoreTrait> =
        Arc::new(MemoryStore::open_in_memory(embedder).unwrap());
    let gpath = dir.path().join("global.md");
    let ppath = dir.path().join("project.md");
    std::fs::write(&gpath, "").unwrap();
    std::fs::write(&ppath, "").unwrap();
    let source = ConstitutionSource::new(&gpath, &ppath).unwrap();
    let safety_rules = Arc::new(SafetyRules::new(&dir.path().join("safety.toml")).unwrap());

    let factory = AgentLoopFactory::new(
        Arc::new(SingleResponseProvider {
            events: vec![],
            caps: Capabilities::openai(),
        }),
        source,
        Some(store),
        sandbox,
        dir.path().to_path_buf(),
        Some(safety_rules),
        Arc::new(std::sync::RwLock::new(SafetyMode::Autonomous)),
        ContextManager::new(128_000, 0.5),
        dir.path().join("plans"),
        None,
    );

    // Build two agents from the same factory.
    let agent_a = factory.build();
    let agent_b = factory.build();

    // Both start in Planning.
    {
        let wf_a = agent_a.workflow_handle();
        let wf_b = agent_b.workflow_handle();
        assert_eq!(wf_a.lock().await.state(), WorkflowState::Planning);
        assert_eq!(wf_b.lock().await.state(), WorkflowState::Planning);
    }

    // Agent A creates a plan directly via its workflow.
    {
        let wf_a = agent_a.workflow_handle();
        let mut wf_a = wf_a.lock().await;
        wf_a.create_plan("A's plan", "goal", "ctx", vec!["step1".into()])
            .unwrap();
    }

    // Agent A is now Executing; agent B must still be Planning with no plan.
    {
        let wf_a = agent_a.workflow_handle();
        let wf_b = agent_b.workflow_handle();
        let wf_a = wf_a.lock().await;
        let wf_b = wf_b.lock().await;
        assert_eq!(wf_a.state(), WorkflowState::Executing);
        assert!(wf_a.plan().is_some());
        assert_eq!(
            wf_b.state(),
            WorkflowState::Planning,
            "agent B's workflow must be independent — not affected by A's plan"
        );
        assert!(wf_b.plan().is_none());
    }
}

// ── Phase 4 end-to-end: bug_fixing plan through finish with auto-capture ──

/// The full semantic-memory pipeline, end to end: seed a .coding corpus →
/// derived index → bug_fixing plan → execute the locked skeleton → record the
/// regression test → PASS review → finish (regression-test gate + PLAN:/BUG:
/// digests + crash-marker supersede) → derived index still intact.
#[tokio::test]
async fn bug_fixing_plan_end_to_end_with_finish_capture() {
    let dir = tempdir().unwrap();
    let root = dir.path();

    // ── Seed the .coding corpus (plans + reviews + backlog) ────────────────
    let plans_dir = root.join(".coding/plans");
    let reviews_dir = root.join(".coding/reviews");
    std::fs::create_dir_all(&plans_dir).unwrap();
    std::fs::create_dir_all(&reviews_dir).unwrap();
    std::fs::write(
        plans_dir.join("old-plan.md"),
        "# Plan: Old plan\n\n## Goal\nDo the old thing\n\n## Kind\nimplementation\n\n## Context\nC\n\n## Steps\n- [x] 1. done\n",
    )
    .unwrap();
    std::fs::write(
        reviews_dir.join("old-review.md"),
        "## Verdict: PASS\n\nno findings\n",
    )
    .unwrap();
    let backlog_path = root.join(".coding/backlog.jsonl");
    std::fs::write(
        &backlog_path,
        "{\"id\":\"old-1\",\"text\":\"pending task\",\"images\":[],\"status\":\"pending\",\"created_at\":1,\"note\":null}\n",
    )
    .unwrap();

    // ── Memory store + derived index (the Phase-2 bootstrap) ──────────────
    let embedder: Arc<dyn mnemo::memory::Embedder> = Arc::new(HashEmbedder::new());
    let store = Arc::new(MemoryStore::open_in_memory(embedder).unwrap());
    let report = mnemo::memory::indexer::index_derived(
        store.as_ref(),
        &plans_dir,
        &backlog_path,
        &(|_: usize, _: usize| {}),
    )
    .await
    .unwrap();
    assert_eq!(report.indexed, 3, "plan + review + backlog indexed");
    let (authored, derived) = store.count_by_class().await.unwrap();
    assert_eq!((authored, derived), (0, 3), "all derived at bootstrap");

    // ── A crash marker for the plan we're about to create (seeded with the
    // plan id AFTER creation below — the capture supersedes by plan id).

    // ── The bug_fixing plan + locked skeleton ─────────────────────────────
    let workflow = Arc::new(Mutex::new(Workflow::new(plans_dir.clone())));
    let create = CreatePlanTool::new(workflow.clone());
    let res = create
        .execute(serde_json::json!({
            "title": "Fix the crash",
            "goal": "Make the crash on open go away",
            "context": "The app crashes on open; root cause: unguarded unwrap in src/app.rs:42. Regression test: app_crash_on_open in src/app/tests.rs fails without the fix.",
            "kind": "bug_fixing",
            "bug": "the app crashes on open",
            "steps": ["ignored — the skeleton is forced"]
        }))
        .await;
    assert!(res.success, "{}", res.output);
    let plan_id = res.data.unwrap()["plan_id"].as_str().unwrap().to_string();

    // The skeleton is locked: 4 steps, reproduce first, verify last.
    {
        let wf = workflow.lock().await;
        let plan = wf.plan().unwrap();
        assert_eq!(plan.kind, mnemo::workflow::PlanKind::BugFixing);
        assert_eq!(plan.steps.len(), 4, "the skeleton is forced");
        assert!(
            plan.steps[0]
                .text
                .contains("Reproduce with failing regression test"),
            "{}",
            plan.steps[0].text
        );
        assert_eq!(plan.bug_symptom.as_deref(), Some("the app crashes on open"));
    }
    // Steps replacement is refused (the skeleton lock).
    let update = UpdatePlanTool::new(workflow.clone());
    let res = update
        .execute(serde_json::json!({"steps": ["replacement"]}))
        .await;
    assert!(!res.success, "skeleton lock must refuse steps replacement");
    assert!(
        res.output.contains("locked 4-step skeleton"),
        "{}",
        res.output
    );

    // ── Seed the crash marker (mentions the plan id) ─────────────────────
    store
        .write(mnemo::memory::Memory::new(
            mnemo::memory::MemoryTier::Working,
            "ACTIVE: fix-crash plan",
            format!("plan {plan_id} in progress"),
            1000,
        ))
        .await
        .unwrap();

    // ── Execute the skeleton: 3 steps, record the regression test, step 4 ─
    let complete = CompleteStepTool::new(workflow.clone());
    for i in 1..=3 {
        let res = complete.execute(serde_json::json!({"step_index": i})).await;
        assert!(res.success, "step {i}: {}", res.output);
    }
    let res = update
        .execute(serde_json::json!({"regression_test": "crash_on_open_regression"}))
        .await;
    assert!(res.success, "{}", res.output);
    let res = complete.execute(serde_json::json!({"step_index": 4})).await;
    assert!(res.success, "{}", res.output);
    assert_eq!(
        workflow.lock().await.state(),
        WorkflowState::Reviewing,
        "bug plan completes to Reviewing"
    );

    // ── The regression-test gate: a missing symbol blocks finish ─────────
    let graph = Arc::new(mnemo::codegraph::CodeGraph::open_in_memory(root.to_path_buf()).unwrap());
    let reviews_dir_for_finish = root.join(".coding/reviews");
    let report_path = reviews_dir_for_finish.join("review.md");
    std::fs::write(&report_path, "## Verdict: PASS\nno findings").unwrap();
    let finish = mnemo::tool::workflow::plan::FinishTool::new(
        workflow.clone(),
        reviews_dir_for_finish.clone(),
    )
    .with_memory(store.clone())
    .with_codegraph(graph.clone());
    let res = finish
        .execute(serde_json::json!({"review_report": report_path.to_string_lossy()}))
        .await;
    assert!(!res.success, "missing symbol must block finish");
    assert!(
        res.output.contains("not found in the code graph"),
        "{}",
        res.output
    );
    assert_eq!(
        workflow.lock().await.state(),
        WorkflowState::Reviewing,
        "still Reviewing after the gate blocks"
    );

    // ── Seed the graph with the test symbol, then finish passes ──────────
    std::fs::write(root.join("tests.rs"), "fn crash_on_open_regression() {}\n").unwrap();
    graph.index(None).unwrap();
    let res = finish
        .execute(serde_json::json!({"review_report": report_path.to_string_lossy()}))
        .await;
    assert!(res.success, "{}", res.output);
    assert_eq!(
        workflow.lock().await.state(),
        WorkflowState::Complete,
        "finish transitions to Complete"
    );

    // ── The auto-capture landed: PLAN: + BUG: digests, authored ──────────
    let (authored, derived) = store.count_by_class().await.unwrap();
    assert_eq!(
        (authored, derived),
        (4, 3),
        "authored = 1 crash marker + 1 successor + PLAN: + BUG: digests; \
         the 3 derived rows untouched"
    );
    // The digests are within budget and pointer-first.
    let plan_digests = store
        .list_filtered(
            &mnemo::memory::MemoryFilter::new().record_type(mnemo::memory::MemoryRecordType::Plan),
        )
        .await
        .unwrap();
    let bug_digests = store
        .list_filtered(
            &mnemo::memory::MemoryFilter::new().record_type(mnemo::memory::MemoryRecordType::Bug),
        )
        .await
        .unwrap();
    let plan_digest = plan_digests
        .iter()
        .find(|m| m.title == "PLAN: Fix the crash")
        .expect("the PLAN: digest exists");
    assert!(
        plan_digest.content.chars().count() <= 400,
        "PLAN digest within budget: {}",
        plan_digest.content.chars().count()
    );
    assert!(
        plan_digest.content.contains("path .coding/plans/"),
        "{}",
        plan_digest.content
    );
    assert!(
        plan_digest
            .content
            .contains("regression test: crash_on_open_regression"),
        "{}",
        plan_digest.content
    );
    let bug_digest = bug_digests
        .iter()
        .find(|m| m.title == "BUG: Fix the crash")
        .expect("the BUG: digest exists");
    assert!(
        bug_digest.content.chars().count() <= 600,
        "BUG digest within budget: {}",
        bug_digest.content.chars().count()
    );
    assert!(
        bug_digest
            .content
            .contains("symptom: the app crashes on open"),
        "{}",
        bug_digest.content
    );

    // ── The crash marker was superseded (not deleted) ─────────────────────
    let markers = store
        .list_filtered(
            &mnemo::memory::MemoryFilter::new()
                .tier(mnemo::memory::MemoryTier::Working)
                .include_superseded(),
        )
        .await
        .unwrap();
    let marker = markers
        .iter()
        .find(|m| m.title == "ACTIVE: fix-crash plan")
        .expect("the marker still exists as history");
    assert!(
        marker.superseded_by.is_some(),
        "the crash marker is superseded"
    );
    // Default recall (no include_superseded) excludes it.
    let live_markers = store
        .list_filtered(&mnemo::memory::MemoryFilter::new().tier(mnemo::memory::MemoryTier::Working))
        .await
        .unwrap();
    assert!(
        live_markers
            .iter()
            .all(|m| m.title != "ACTIVE: fix-crash plan"),
        "the superseded marker is excluded from default listing"
    );

    // ── A derived rebuild never touches the captured digests ────────────
    let rebuild = mnemo::memory::indexer::rebuild_derived(
        store.as_ref(),
        &plans_dir,
        &backlog_path,
        &(|_: usize, _: usize| {}),
    )
    .await
    .unwrap();
    assert!(rebuild.indexed >= 3, "the corpus re-indexes");
    let (authored, derived) = store.count_by_class().await.unwrap();
    assert_eq!(
        (authored, derived),
        (4, 5),
        "authored digests survive the rebuild; derived rows regenerated \
         (the corpus grew: the bug plan file + the review report are now sources)"
    );
    assert!(
        store.get_memory(&plan_digest.id).await.unwrap().is_some(),
        "the PLAN: digest survives a derived rebuild"
    );
}
