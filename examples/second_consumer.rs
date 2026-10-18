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
//! Running it has two modes, and CI runs the offline one (§9 :924 —
//! wired in build.yml's `examples` job, backlog c96a96ca; the earlier
//! round's false claim that it ran is corrected here, H1):
//!
//! * `MNEMO_HOME` set → the engine resolves the configured endpoint
//!   exactly like a real deployment (the same `not_configured` gate
//!   surfaces when the dir carries no config).
//! * `MNEMO_HOME` unset → the engine gets the in-file scripted provider
//!   through the public `Engine::builder(..).provider(..)` override
//!   (the same injection the integration tests use), and the whole
//!   host-tool round trip runs offline.

use async_trait::async_trait;
use futures::stream::{self, BoxStream};

use mnemo::agent::policy::PermissionPolicy;
use mnemo::agent::prompt::{ConstitutionMode, PromptSpec};
use mnemo::headless::{
    Engine, EngineOptions, Guardrails, HostToolSpec, MemoryBackend, SessionCommand,
    SessionLimits, SpawnOptions, ToolGroup, ToolSelection,
};
use mnemo::provider::{
    Capabilities, FinishReason, LlmClient, LlmEvent, Message, ProviderKind, ToolChoice,
    ToolSchema,
};
use mnemo::runtime::channels::SerializableAgentEvent;
use mnemo::tool::{SafetyLevel, ToolResult};
use mnemo::workspace::Workspace;

/// The opening leg: the model calls the example's own host tool so the
/// kernel↔host round trip actually executes (`HostToolCall` → the host's
/// `HostToolResult` command → the result reaches the model).
fn tool_leg() -> Vec<LlmEvent> {
    vec![
        LlmEvent::ToolCallStart {
            index: 0,
            id: "call_1".into(),
            name: "acme_ticket_lookup".into(),
        },
        LlmEvent::ToolCallArgumentDelta {
            index: 0,
            fragment: r#"{"ticket": "42"}"#.into(),
        },
        LlmEvent::Finish {
            reason: FinishReason::ToolCalls,
        },
    ]
}

/// The closing leg: after the host answer lands, the model reports it
/// and ends the turn (the phrase matches the CI greps — the same words
/// the host's own `ToolResult` carries).
fn text_leg() -> Vec<LlmEvent> {
    vec![
        LlmEvent::TextDelta {
            text: "Ticket #42 checked — the weather widget shows rain for Oslo and the ticket is still open.".into(),
        },
        LlmEvent::Finish { reason: FinishReason::Stop },
    ]
}

/// A scripted provider the example owns in-file — the CI mock (§9 :924).
/// Each `complete` pops the next leg; the demo has exactly two, so the
/// host-tool round trip executes without any external endpoint. The
/// shapes mirror the integration suite's `ScriptedLlm` (forbidden to an
/// example — test support) but live here so the public-API contract is
/// demonstrated on the public surface only.
struct DemoLlm {
    legs: std::sync::Mutex<std::collections::VecDeque<Vec<LlmEvent>>>,
    calls: std::sync::atomic::AtomicUsize,
    caps: Capabilities,
}

impl DemoLlm {
    /// The demo script: the tool leg, then the closing text leg (the
    /// agent loop re-calls the provider once the `HostToolResult`
    /// lands, which is leg 2's trigger).
    fn new() -> Self {
        Self {
            legs: std::sync::Mutex::new(vec![tool_leg(), text_leg()].into()),
            calls: std::sync::atomic::AtomicUsize::new(0),
            caps: Capabilities::openai(),
        }
    }
}

#[async_trait]
impl LlmClient for DemoLlm {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
    fn kind(&self) -> ProviderKind {
        ProviderKind::OpenAI
    }
    fn model(&self) -> &str {
        "scripted"
    }
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _tool_choice: Option<ToolChoice>,
    ) -> mnemo::error::Result<BoxStream<'_, LlmEvent>> {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let next = self
            .legs
            .lock()
            .expect("script lock poisoned")
            .pop_front()
            .unwrap_or_default();
        Ok(Box::pin(stream::iter(next)))
    }
}

#[tokio::main]
async fn main() {
    // A scratch workspace (no git required); the consumer's own config
    // dir (MNEMO_HOME if set — never the mnemo app's default). Without
    // MNEMO_HOME the CI path kicks in: the in-file scripted provider
    // and the offline round trip.
    let root = std::env::temp_dir().join("second-consumer-demo");
    std::fs::create_dir_all(root.join(".coding")).expect("workspace state dir");
    let mnemo_home = std::env::var_os("MNEMO_HOME");
    let scripted = mnemo_home.is_none();
    // The offline CI path uses a BARE config dir (the same shape the
    // integration suite's scripted engine uses) so the engine never
    // touches a real machine config and never resolves a real endpoint —
    // the scripted provider below carries the whole demo.
    let config_dir = match &mnemo_home {
        Some(home) => std::path::PathBuf::from(home),
        None => {
            let bare = root.join("config");
            std::fs::create_dir_all(&bare).expect("bare config dir");
            bare
        }
    };
    let mut builder = Engine::builder(EngineOptions {
        workspace: Workspace {
            root: root.clone(),
            state_dir: root.join(".coding"),
            require_git: false,
        },
        config_dir: Some(config_dir),
        permissions: PermissionPolicy::OverridePermissions,
        memory: MemoryBackend::Sqlite { path: None },
        index_codegraph: false,
        require_git: false,
        auto_install_embedder: false,
    });
    if scripted {
        // The offline CI path: inject the scripted provider the way the
        // integration tests do — through the public builder override.
        builder = builder.provider(std::sync::Arc::new(DemoLlm::new()));
    }
    let engine = builder
        .start()
        .await
        .expect("the engine starts — MNEMO_HOME unset (the scripted CI path), or a configured dir");

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
    // The turn drives the exit: the host loop polls the event bus, which
    // outlives the turn (the session stays alive for further turns), so
    // join!ing it would hang the process after the turn ends — the first
    // real execution of this example exposed exactly that. select!
    // completes on the turn's outcome; every event up to that point is
    // already printed, and the losing host future is dropped.
    let turn = tokio::select! {
        out = session.run_to_completion("Check ticket 42 and summarize it for the customer.") => out,
        _ = host => unreachable!("the host loop only ends when the session does"),
    };
    match turn {
        Ok(turn) => println!("turn finished: {}", turn.final_text),
        Err(e) => {
            eprintln!("turn failed ({}): {}", e.code(), e.message());
            std::process::exit(1);
        }
    }

    // The host loop inside the select drains events up to the turn's
    // completion, but the tail events (the tool result, the closing
    // stream, the parked notice) can still sit in the buffer — dropping
    // the losing future loses them. A short settle drain prints the
    // rest; it ends on the window (the bus itself outlives the turn —
    // the session stays alive for further turns).
    let _ = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        async {
            while let Ok(ev) = rx.recv().await {
                println!("{}", serde_json::to_string(&ev).expect("event json"));
            }
        },
    )
    .await;
}
