// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Golden JSON fixtures for the IPC DTOs — the app-crate half of the IPC
//! contract drift detector (Maintainability H4).
//!
//! For each key DTO this test builds one representative sample, serializes it
//! via `serde_json::to_value`, and asserts EXACT equality against a committed
//! fixture under `frontend/src/lib/ipc-fixtures/`. Missing fixtures are
//! written (pretty-printed) and reported at the end; once committed, any
//! shape change makes the test fail until the fixture is consciously updated.
//! The frontend vitest contract suite imports the same fixtures, so a change
//! is caught on the TS side too.
//!
//! Fixtures are read via `env!("CARGO_MANIFEST_DIR")` (the app crate root =
//! `src-tauri`), so the path is `../frontend/src/lib/ipc-fixtures/<name>.json`.

#![cfg(test)]

use std::path::PathBuf;

use serde::Serialize;
use serde_json::Value;

use mnemo::config::{OptimizerConfig, SafetyMode};
use mnemo::{QuestionOption, SerializableAgentEvent};
use mnemo::app::workflow::plan_file::PlanFile;
use mnemo::app::workflow::WorkflowState;

use crate::ipc::settings::{BudgetWire, RoutingWire};

use crate::ipc::agent::{ActiveSkillInfo, AgentInfo, PlanAncestorInfo, WorkflowStateInfo};
use crate::ipc::backlog_cmds::{BacklogChangedPayload, BacklogItemView, RunAllProgress};
use mnemo::headless::{
    CodegraphGraph, CodegraphStatus, MemoryCounts, MemoryDebugOverview, MemoryDebugWire,
    MemoryIndexStatus, ScoredMemoryDebugWire,
};
use mnemo::headless::stats::EngineStats;
use mnemo::memory::{
    CacheEfficiency, ModelBreakdown, ProjectStats, SavingsDayBreakdown, SavingsKindBreakdown,
    SavingsStats, SessionStats,
};
use crate::ipc::settings::{
    GetSettingsContext, GetSettingsGeneral, GetSettingsResponse, GetSettingsSteeringNotes,
    GetSettingsUi, LayaWire, ModelsConfigWire, ProjectWire, SaveEndpointsResponse,
    SaveSettingsResponse,
};
use mnemo::app::backlog::{BacklogItem, BacklogStatus};

/// Resolve the absolute path of a fixture under
/// `frontend/src/lib/ipc-fixtures/` from the app crate's manifest dir
/// (`src-tauri`).
fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("frontend")
        .join("src")
        .join("lib")
        .join("ipc-fixtures")
        .join(format!("{name}.json"))
}

/// Serialize `value` via `serde_json::to_value` and assert it equals the
/// committed fixture `name`. On a missing fixture, write it (pretty-printed)
/// and record it; the caller panics once at the end so all missing fixtures
/// are written in one run. On a mismatch, print both JSONs + the path.
fn assert_fixture<T: Serialize>(name: &str, value: &T, generated: &mut Vec<String>) {
    let path = fixture_path(name);
    let actual = serde_json::to_value(value).expect("value serializes");
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
fn dto_fixtures_match_serde() {
    let mut generated: Vec<String> = Vec::new();

    // ── AgentInfo ────────────────────────────────────────────────────────
    let main_agent = AgentInfo {
        id: 1,
        name: "main".into(),
        running: true,
        parent_id: None,
        model: Some("gpt-5".into()),
        provider: None,
        reasoning_effort: None,
    };
    assert_fixture("dto-agent-info", &main_agent, &mut generated);
    // The provider field serializes only when Some — the endpoint name that
    // serves the model (backlog 2980ca67: the label must not misresolve by
    // first-match when a model id is listed under two endpoints). The
    // reasoning_effort field follows the same shape (backlog 51dab4da: the
    // DISPLAY-space effective effort of the serving model).
    let with_provider = AgentInfo {
        provider: Some("openai".into()),
        reasoning_effort: Some("low".into()),
        ..main_agent.clone()
    };
    let json = serde_json::to_string(&with_provider).unwrap();
    assert!(json.contains("\"provider\":\"openai\""), "{json}");
    assert!(json.contains("\"reasoning_effort\":\"low\""), "{json}");
    assert!(
        !serde_json::to_string(&main_agent)
            .unwrap()
            .contains("\"provider\""),
        "None provider must not serialize"
    );

    // ── SerializableAgentEvent::UserQuestion (the ask_user question prompt) ─
    // Locks the user_question wire shape: question_id + question + options
    // (each with a label + optional description). The oneshot responder is
    // NOT in the serializable form (the IPC adapter holds it separately).
    let user_question = SerializableAgentEvent::UserQuestion {
        question_id: "q_1".into(),
        question: "Pick a skill to master".into(),
        options: vec![
            QuestionOption {
                label: "Music".into(),
                description: Some("Play any instrument".into()),
            },
            QuestionOption {
                label: "Languages".into(),
                description: Some("Speak every language".into()),
            },
            QuestionOption {
                label: "Cooking".into(),
                description: None,
            },
        ],
    };
    assert_fixture("event-user-question", &user_question, &mut generated);

    // ── WorkflowStateInfo (executing, with a plan, no skill) ─────────────
    // Exercises PlanFile + Step transitively. `skill` is skipped when None
    // (WorkflowStateInfo has skip_serializing_if on it).
    let plan = PlanFile::new(
        "Implement feature X",
        "Add the thing.",
        "Context notes here.",
        vec!["Step one".into(), "Step two".into()],
    );
    let ws_executing = WorkflowStateInfo {
        state: WorkflowState::Executing,
        plan: Some(plan),
        depth: 1,
        parents: vec![],
        skill: None,
    };
    assert_fixture("dto-workflow-state-info", &ws_executing, &mut generated);

    // ── WorkflowStateInfo (skill state, with the skill overlay) ──────────
    // Locks the ActiveSkillInfo wire shape + the `skill` key presence. The
    // skill state has no plan (planning/skill surfaces differ), so `plan` is
    // None (rendered as null — no skip on plan).
    let ws_skill = WorkflowStateInfo {
        state: WorkflowState::Skill,
        plan: None,
        depth: 0,
        parents: vec![PlanAncestorInfo {
            id: "parent-plan-id".into(),
            title: "Parent plan title".into(),
        }],
        skill: Some(ActiveSkillInfo {
            name: "merge_to_main".into(),
            prompt: "Merge the branch into main.".into(),
            target_state: WorkflowState::Complete,
        }),
    };
    assert_fixture("dto-workflow-state-info-skill", &ws_skill, &mut generated);

    // ── BacklogChangedPayload (exercises BacklogItemView + RunAllProgress) ─
    // The second item is a run-all checkpointed one: its note head is the
    // pre-item git checkpoint sha, which `BacklogItemView::new` parses into
    // `checkpoint_sha` — locked here so the payload shape AND the note-head
    // parsing both stay pinned (the first item locks the null case).
    let payload = BacklogChangedPayload {
        items: vec![
            BacklogItemView::new(BacklogItem {
                id: "2f3a4b5c-0000-4000-8000-000000000001".into(),
                text: "First task".into(),
                images: vec![],
                status: BacklogStatus::Done,
                created_at: 1_700_000_000,
                note: None,
                deferred: false,
                plan_id: None,
                plan_title: None,
                deleted_at: None,
            }),
            BacklogItemView::new(BacklogItem {
                id: "2f3a4b5c-0000-4000-8000-000000000002".into(),
                text: "Second task".into(),
                images: vec!["data:image/png;base64,iVBORw0KGgo=".into()],
                status: BacklogStatus::InFlight,
                created_at: 1_700_000_060,
                note: Some(
                    "a1b2c3d4e5f6789012345678901234567890abcd | approval requested — halted".into(),
                ),
                deferred: false,
                plan_id: Some("593f4a4e".into()),
                plan_title: Some("Second task plan".into()),
                deleted_at: None,
            }),
        ],
        auto_feed: true,
        parallel: false,
        run_all: RunAllProgress {
            active: true,
            done: 1,
            total: 2,
            compacting: false,
            concurrency: 1,
            spawned: Vec::new(),
            note: None,
        },
    };
    assert_fixture("dto-backlog-changed-payload", &payload, &mut generated);

    // ── Settings responses (the typed structs from Step 1) ───────────────
    // These fixtures double as proof that the typed structs render
    // byte-identically to the old json! bodies.
    // (E5: the legacy `get_config` fixture was deleted — `get_settings` is a
    // strict superset and all callers migrated to it.)
    let get_settings = GetSettingsResponse {
        config_dir: "/home/user/.mnemo".into(),
        general: GetSettingsGeneral {
            default_provider: None,
            default_model: None,
            safety: SafetyMode::ApproveEachAction,
            vision_model: None,
            embedding_model: None,
            bundled_embedding_model: None,
            laya: LayaWire {
                enabled: false,
                auto_type_memories: false,
                steer_tool_choice: false,
                failure_triage: false,
                failure_triage_knn: false,
                auto_finetune: false,
                routing: false,
            },
            enable_browser_inspection: false,
            auto_compact_on_plan_complete: false,
            optimizer: OptimizerConfig::default(),
            routing: RoutingWire {
                cheap: None,
                capable: None,
                lane_medium: None,
                lane_high: None,
                escalate: None,
                threshold: 0.80,
                enforce: false,
            },
            budget: BudgetWire {
                enabled: false,
                max_tokens_per_plan: 0,
                max_escalations_per_plan: 1,
                max_retries_per_lane: 3,
            },
        },
        context: GetSettingsContext {
            summarize_at_fill_rate: 0.5,
            proxy_cache_ceiling_tokens: Some(340_000),
        },
        ui: GetSettingsUi {
            theme: "dark".into(),
            show_token_usage: true,
            show_tool_images: true,
            show_tool_activity: true,
            show_knowledge_activity: true,
            show_delegation_notes: false,
            steering_notes: GetSettingsSteeringNotes {
                auto_delegated: false,
                search_nudge: true,
                shell_tip: true,
                graph_miss: true,
                recall_rider: true,
                read_nudge: true,
                literal_tip: true,
                known_memory_hit: true,
                consolidation_due: true,
                shell_redirect: true,
                edit_stale_read: true,
            },
            chat_thread_line: true,
            chat_prose_cap: true,
            chat_turn_tint: true,
            chat_hover_timestamps: true,
            sound_complete: true,
            sound_input_needed: true,
            sound_stopped_errors: true,
        },
        models: ModelsConfigWire {
            planning: None,
            executing: None,
            bug_fixing: None,
            reviewing: None,
            complete: None,
            subagent: None,
            summarize: None,
            skill: std::collections::HashMap::new(),
        },
        markdown: crate::ipc::settings::MarkdownWire {
            skip_dirs: mnemo::config::MarkdownConfig::default().skip_dirs,
        },
        git: crate::ipc::settings::GitWire {
            core_operations: mnemo::config::GitConfig::default().core_operations,
        },
        trace: crate::ipc::settings::GetSettingsTrace {
            memory_budget_mb: mnemo::config::TraceConfig::default().memory_budget_mb,
            request_body_cap_kb: mnemo::config::TraceConfig::default().request_body_cap_kb,
        },
        memory: mnemo::memory::MemorySearchConfig::default(),
        endpoints: vec![],
        pricing: vec![],
        projects: vec![ProjectWire {
            name: "demo".into(),
            path: "C:/code/demo".into(),
        }],
    };
    assert_fixture("dto-get-settings", &get_settings, &mut generated);

    let save_endpoints = SaveEndpointsResponse {
        default_provider: Some("openai".into()),
        default_model: Some("gpt-4o".into()),
        provider_swapped: true,
    };
    assert_fixture("dto-save-endpoints", &save_endpoints, &mut generated);

    let save_settings = SaveSettingsResponse {
        ok: true,
        safety: Some("autonomous"),
        vision_configured: false,
    };
    assert_fixture("dto-save-settings", &save_settings, &mut generated);

    // ── CodeGraph DTOs (the Graph tab's IPC surface) ─────────────────────
    // Locks the status shape (availability + counts + indexing flag) and
    // the graph payload (Symbol/EdgeRow wire forms via the slice types).
    let codegraph_status = CodegraphStatus {
        available: true,
        indexing: false,
        files: 120,
        symbols: 3400,
        edges: 8900,
        last_indexed_at: Some(1_700_000_000),
    };
    assert_fixture("dto-codegraph-status", &codegraph_status, &mut generated);

    let codegraph_graph = CodegraphGraph {
        nodes: vec![mnemo::codegraph::extract::Symbol {
            id: "src/lib.rs::helper::12".into(),
            name: "helper".into(),
            kind: mnemo::codegraph::extract::SymbolKind::Function,
            file: "src/lib.rs".into(),
            start_line: 12,
            end_line: 30,
        }],
        edges: vec![mnemo::codegraph::store::EdgeRow {
            from_id: "src/main.rs::main::1".into(),
            to_id: "src/lib.rs::helper::12".into(),
            kind: mnemo::codegraph::extract::EdgeKind::Calls,
        }],
    };
    assert_fixture("dto-codegraph-graph", &codegraph_graph, &mut generated);

    if !generated.is_empty() {
        panic!(
            "newly generated {} fixture(s) — review for sanity and re-run to assert them:\n{}",
            generated.len(),
            generated.join("\n")
        );
    }
}

/// Normalize CRLF line endings to LF (backlog b93c0f6e): `include_str!`
/// embeds working-tree bytes at compile time, so a git `autocrlf`-smudged
/// (CRLF) checkout breaks every `"\n}\n"`-anchored body slice in the
/// source-contract tests — the needle never matches `"\r\n}\r\n"` and the
/// slice over-captures to EOF. Source-contract tests normalize the embedded
/// source through this helper at the read boundary; committed file bytes are
/// never changed and no `.gitattributes`/eol config is required.
pub(crate) fn normalize_lf(src: &str) -> String {
    src.replace("\r\n", "\n")
}

#[test]
fn normalize_lf_converts_crlf_and_keeps_lf() {
    assert_eq!(normalize_lf("a\r\nb\r\n"), "a\nb\n");
    assert_eq!(normalize_lf("a\nb\n"), "a\nb\n");
    // Idempotent: normalizing twice equals once.
    let crlf = "fn f() {\r\n}\r\n";
    assert_eq!(normalize_lf(&normalize_lf(crlf)), normalize_lf(crlf));
    // A lone \r (classic-Mac line ending) is left alone — only CRLF pairs
    // are normalized.
    assert_eq!(normalize_lf("a\rb"), "a\rb");
}
/// Row 19 (master-spec §7 :838-840): the stats + memory + code-graph IPC
/// surfaces stay BYTE-IDENTICAL across the 3b.2 rewire (job-03b step
/// file §C D2): the memory-debug + codegraph wire types move into the
/// kernel lib (mnemo::headless) and the IPC commands become thin
/// callers — these fixtures are the pins. The get_{savings,session,
/// project}_stats commands are thin serde reads of the KERNEL-owned
/// stats DTOs (untouched by the rewire); their samples below pin those
/// shapes too. The codegraph DTOs are also pinned by
/// `dto_fixtures_match_serde` — re-asserted here for the row-19 name.
#[test]
fn stats_memory_codegraph_ipc_unchanged() {
    let mut generated: Vec<String> = Vec::new();

    // ── the memory-debug wire types (3b.2 moves them into mnemo) ──────
    let wire = MemoryDebugWire {
        id: "m-1".into(),
        tier: "working".into(),
        title: "T".into(),
        content: "hello".into(),
        strength: 0.5,
        access_count: 2,
        created_at: 100,
        last_accessed_at: 200,
        source_session_ids: vec!["s-1".into()],
        data: serde_json::json!({"k": "v"}),
    };
    assert_fixture("dto-memory-debug-wire", &wire, &mut generated);
    let scored = ScoredMemoryDebugWire {
        memory: wire,
        score: 0.75,
    };
    assert_fixture("dto-scored-memory-wire", &scored, &mut generated);
    let counts = MemoryCounts {
        working: 1,
        episodic: 2,
        semantic: 3,
        procedural: 4,
        total: 10,
    };
    assert_fixture("dto-memory-counts", &counts, &mut generated);
    let overview = MemoryDebugOverview {
        status: serde_json::json!({"ok": true}),
        model_id: "hash".into(),
        dim: 384,
        configured_model: None,
        fingerprints: vec![("hash".into(), 384)],
        fingerprint_matches: true,
        counts,
    };
    assert_fixture("dto-memory-debug-overview", &overview, &mut generated);

    // ── the stats DTOs behind get_{savings,session,project}_stats ────
    let savings = SavingsStats {
        saved_tokens_total: 1200,
        event_count: 3,
        per_kind: vec![SavingsKindBreakdown {
            kind: "compaction".into(),
            event_count: 2,
            saved_tokens: 800,
        }],
        per_day: vec![SavingsDayBreakdown {
            day: 20_000,
            event_count: 2,
            saved_tokens: 800,
        }],
        recent: Vec::new(),
        cache: CacheEfficiency {
            prompt_tokens: 1000,
            cached_tokens: 400,
            cached_not_null_requests: 2,
            request_count: 3,
        },
    };
    assert_fixture("dto-savings-stats", &savings, &mut generated);
    let session = SessionStats {
        session_id: "s-1".into(),
        prompt_tokens: 100,
        completion_tokens: 50,
        reasoning_tokens: 10,
        cached_tokens: 60,
        ttft_ms_total: 300,
        generation_ms_total: 2000,
        timed_requests: 2,
        request_count: 3,
        per_model: vec![ModelBreakdown {
            model: "gpt-5".into(),
            endpoint: Some("openai".into()),
            prompt_tokens: 100,
            completion_tokens: 50,
            reasoning_tokens: 10,
            cached_tokens: 60,
            ttft_ms_total: 300,
            generation_ms_total: 2000,
            timed_requests: 2,
            request_count: 3,
        }],
        created_at: 100,
        ended_at: Some(200),
    };
    assert_fixture("dto-session-stats", &session, &mut generated);
    let project = ProjectStats {
        prompt_tokens: 1000,
        completion_tokens: 500,
        reasoning_tokens: 100,
        cached_tokens: 600,
        request_count: 3,
        ..Default::default()
    };
    assert_fixture("dto-project-stats", &project, &mut generated);

    // ── job 6 (§5.9.10): the engine_status pull — the headless Engine's
    // §5.7 aggregate over the SAME shared brain (the IpcState-held
    // engine's proving consumer). Reuses the two store aggregates above;
    // the steering half mirrors the get_steering_stats shape.
    let engine_stats = EngineStats {
        project_stats: project.clone(),
        savings_stats: savings.clone(),
        steering_snapshot: mnemo::SteeringStatsSnapshot {
            fired: 2,
            switched: 1,
            by_marker: vec![],
            file_tool_mutations: 3,
            shell_mutations: 1,
        },
    };
    assert_fixture("dto-engine-stats", &engine_stats, &mut generated);

    // ── the derived-index status behind memory_index_status ────────
    let index_status = MemoryIndexStatus {
        authored_count: 12,
        derived_count: 34,
        last_built_at: Some(1_700_000_000),
    };
    assert_fixture("dto-memory-index-status", &index_status, &mut generated);

    // ── the code-graph wire types — re-pinned by the row-19 test too ─
    let codegraph_status = CodegraphStatus {
        available: true,
        indexing: false,
        files: 120,
        symbols: 3400,
        edges: 8900,
        last_indexed_at: Some(1_700_000_000),
    };
    assert_fixture("dto-codegraph-status", &codegraph_status, &mut generated);

    if !generated.is_empty() {
        panic!(
            "newly generated {} fixture(s) — review for sanity and re-run to assert them:\n{}",
            generated.len(),
            generated.join("\n")
        );
    }
}