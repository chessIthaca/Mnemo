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

use mnemo::config::{LayaMode, SafetyMode};
use mnemo::runtime::channels::{QuestionOption, SerializableAgentEvent};
use mnemo::workflow::plan_file::PlanFile;
use mnemo::workflow::WorkflowState;

use crate::ipc::agent::{ActiveSkillInfo, AgentInfo, PlanAncestorInfo, WorkflowStateInfo};
use crate::ipc::backlog_cmds::{BacklogChangedPayload, BacklogItemView, RunAllProgress};
use crate::ipc::codegraph_cmds::{CodegraphGraph, CodegraphStatus};
use crate::ipc::settings::{
    GetSettingsContext, GetSettingsGeneral, GetSettingsResponse, GetSettingsSteeringNotes,
    GetSettingsUi, LayaWire, ModelsConfigWire, ProjectWire, SaveEndpointsResponse,
    SaveSettingsResponse,
};
use mnemo::backlog::{BacklogItem, BacklogStatus};

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
                endpoint: None,
                mode: LayaMode::External,
                checkpoint: None,
            },
            enable_browser_inspection: false,
            auto_compact_on_plan_complete: false,
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
