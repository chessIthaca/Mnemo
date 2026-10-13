// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The G10 second consumer (master-spec §5.9.10 :744-746) — a non-mnemo
//! process embedding the kernel through the public headless surface.
//! Builds with `cargo build --examples --no-default-features --features
//! kernel,headless` (no `app` machinery, no embeddings, no browser) and
//! demonstrates the whole contract in one file:
//!
//! * a **NoPolicy** agent (`SpawnOptions::policy: None` — the free
//!   agent) with a **non-coding identity** (a `PromptSpec` with no
//!   kernel coding blocks, no constitution),
//! * the tool surface narrowed to `[Read, Host]`,
//! * **one host tool** this example answers itself (the kernel↔host
//!   round-trip: dispatch emits `HostToolCall`, the host answers with
//!   the `HostToolResult` command),
//! * turn **limits** set (G7), and
//! * every session **event printed as JSON** (the same wire the Tauri
//!   frontend sees).
//!
//! Running it needs a configured endpoint (set `MNEMO_HOME` to a config
//! dir with `[general] default_model` + an endpoint, or rely on the
//! engine's dummy provider warning); CI runs it against the mock
//! provider (§9 :924).

use mnemo::agent::policy::PermissionPolicy;
use mnemo::agent::prompt::{ConstitutionMode, PromptSpec};
use mnemo::headless::{
    Engine, EngineOptions, Guardrails, HostToolSpec, MemoryBackend, SessionCommand,
    SessionLimits, SpawnOptions, ToolGroup, ToolSelection,
};
use mnemo::runtime::channels::SerializableAgentEvent;
use mnemo::tool::{SafetyLevel, ToolResult};
use mnemo::workspace::Workspace;

#[tokio::main]
async fn main() {
    // A scratch workspace (no git required) + the consumer's own config
    // dir (MNEMO_HOME if set — never the mnemo app's default).
    let root = std::env::temp_dir().join("second-consumer-demo");
    std::fs::create_dir_all(root.join(".coding")).expect("workspace state dir");
    let engine = Engine::builder(EngineOptions {
        workspace: Workspace {
            root: root.clone(),
            state_dir: root.join(".coding"),
            require_git: false,
        },
        config_dir: std::env::var_os("MNEMO_HOME").map(std::path::PathBuf::from),
        permissions: PermissionPolicy::OverridePermissions,
        memory: MemoryBackend::Sqlite { path: None },
        index_codegraph: false,
        require_git: false,
        auto_install_embedder: false,
    })
    .start()
    .await
    .expect("the engine starts (set MNEMO_HOME to a configured dir)");

    let session = engine
        .spawn(SpawnOptions {
            name: Some("support-demo".into()),
            // NoPolicy: the free agent. The guardrails carry the
            // non-coding identity + the narrowed tool surface.
            policy: None,
            guardrails: Some(Guardrails {
                prompt: Some(PromptSpec {
                    identity: "You are a support agent for ACME.".into(),
                    blocks: vec![], // non-coding: none of the kernel blocks
                    extra_stable: None,
                    constitution: ConstitutionMode::None,
                }),
                tools: ToolSelection {
                    include: vec![ToolGroup::Read, ToolGroup::Host],
                    exclude: vec![],
                },
                ..Guardrails::default()
            }),
            limits: Some(SessionLimits {
                max_turns: Some(4),
                wall_clock_ms: Some(120_000),
                ..SessionLimits::default()
            }),
            host_tools: vec![HostToolSpec {
                name: "acme_ticket_lookup".into(),
                description: "Look up an ACME support ticket by id".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {"ticket": {"type": "string"}},
                    "required": ["ticket"]
                }),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(5_000),
            }],
            ..SpawnOptions::default()
        })
        .await
        .expect("the session spawns");

    // The host side: print every event as JSON, and answer this
    // example's own host tool from its (one-ticket) database.
    let mut rx = session.events();
    let host = async {
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    println!("{}", serde_json::to_string(&ev).expect("event json"));
                    if let SerializableAgentEvent::HostToolCall { call_id, name, .. } = &ev.event
                    {
                        if name == "acme_ticket_lookup" {
                            session
                                .send(SessionCommand::HostToolResult {
                                    call_id: call_id.clone(),
                                    result: Some(ToolResult::success(
                                        "ticket #42: 'weather widget shows rain for Oslo' — open"
                                            .to_string(),
                                    )),
                                    error: None,
                                })
                                .await
                                .expect("the answer resolves the pending call");
                        }
                    }
                }
                Err(_) => return, // the session ended; the buffer is drained
            }
        }
    };
    let (outcome, ()) = tokio::join!(
        session.run_to_completion("Check ticket 42 and summarize it for the customer."),
        host
    );
    match outcome {
        Ok(turn) => println!("turn finished: {}", turn.final_text),
        Err(e) => eprintln!("turn failed ({}): {}", e.code(), e.message()),
    }
}
