//! The §5.3 public-API contract fixtures (job 3a.3): the wire shapes and
//! the Send/Sync/'static guarantees are pinned BEFORE the Engine exists —
//! every later 3a step keeps these green (the types-first contract).

use serde_json::json;
use std::path::PathBuf;

use mnemo::agent::policy::{Continuation, GateVerdict, PermissionPolicy};
use mnemo::config::SafetyMode;
use mnemo::tool::{SafetyLevel, ToolResult};
use mnemo::headless::{
    Engine, EngineError, EngineOptions, EventReceiver, ExternalPolicySpec, GateRule,
    Guardrails, HookKind, HostToolSpec, MemoryBackend, PolicyAnswer, QuestionPolicy,
    RecvError, SessionCommand, SessionEvent, SessionId, SessionLimits, SpawnOptions,
    TimeoutAction, Transcript,
};
use mnemo::runtime::channels::{Approval, SerializableAgentEvent, UserAnswer};
use mnemo::workspace::Workspace;

/// Serialize → deserialize → re-serialize; the two JSON forms must be
/// equal (exercises both serde directions without needing PartialEq on
/// every type).
fn roundtrip<T: serde::Serialize + serde::de::DeserializeOwned>(value: &T) -> serde_json::Value {
    let first = serde_json::to_value(value).expect("serialize");
    let back: T = serde_json::from_value(first.clone()).expect("deserialize");
    let second = serde_json::to_value(&back).expect("re-serialize");
    assert_eq!(first, second, "the wire shape must survive a round-trip");
    first
}

fn sample_options() -> EngineOptions {
    EngineOptions {
        workspace: Workspace {
            root: "/tmp/w".into(),
            state_dir: "/tmp/w/.coding".into(),
            require_git: false,
        },
        config_dir: Some("/tmp/cfg".into()),
        permissions: PermissionPolicy::ApproveViaApi {
            safety_mode: SafetyMode::Autonomous,
        },
        memory: MemoryBackend::Sqlite { path: None },
        index_codegraph: true,
        require_git: false,
        auto_install_embedder: false,
    }
}

/// §7 row 13: every public type is plain serde data — the round-trip
/// holds for representative values of each, and the `type` tags are the
/// snake_case spellings a JS binding matches on.
#[test]
fn public_types_serde_roundtrip() {
    // EngineOptions — full value + the memory-Disabled variant.
    roundtrip(&sample_options());
    let mut opts = sample_options();
    opts.memory = MemoryBackend::Disabled;
    roundtrip(&opts);

    // SpawnOptions — default and full (the 3a subset).
    roundtrip(&SpawnOptions::default());
    roundtrip(&SpawnOptions {
        name: Some("worker".into()),
        model: Some("local/m2".into()),
        worktree: Some("/tmp/wt".into()),
        permissions: Some(PermissionPolicy::OverridePermissions),
        policy: Some("mnemo.workflow".into()),
        resume: Some(Transcript {
            messages: vec![Message::user_text("hi")],
        }),
        limits: Some(SessionLimits {
            max_turns: Some(3),
            wall_clock_ms: Some(600_000),
            ..SessionLimits::default()
        }),
        host_tools: vec![HostToolSpec {
            name: "get_weather".into(),
            description: "Get the current weather for a city".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"city": {"type": "string"}},
                "required": ["city"]
            }),
            safety: SafetyLevel::AutoRun,
            timeout_ms: Some(150),
        }],
        // 3c/G6 tier 3: a representative external-policy spec — the
        // hooks array, the timeout, and the timeout action all ride
        // the wire (the field's None form round-trips via the default
        // above).
        external_policy: Some(ExternalPolicySpec {
            hooks: vec![HookKind::ToolGate, HookKind::TurnEnd],
            timeout_ms: 30_000,
            on_timeout: TimeoutAction::Deny,
        }),
        // 3c/G6 tier 2: a representative guardrails spec (a non-default
        // route + one gate rule, so the wire carries the enum shapes);
        // the field's None form round-trips via the default above.
        guardrails: Some(Guardrails {
            route: vec!["gpt4".to_string()],
            gate_rules: vec![GateRule {
                tool: "shell".into(),
                arg_pattern: Some("rm\\s+-rf".into()),
                verdict: GateVerdict::Deny {
                    message: "no destructive shell".into(),
                },
            }],
            question_policy: QuestionPolicy::AnswerConstant("decide yourself".into()),
            ..Guardrails::default()
        }),
    });

    // 3c/G6 tier 3 (D5): the answer wire — the Gate/Deny form, the
    // Continuation/Continue note, and the Stop verb.
    roundtrip(&PolicyAnswer::Gate {
        verdict: GateVerdict::Deny {
            message: "no destructive shell".into(),
        },
    });
    roundtrip(&PolicyAnswer::Continuation {
        continuation: Continuation::Continue {
            note: Some("continue and finish".into()),
        },
    });
    roundtrip(&PolicyAnswer::Continuation {
        continuation: Continuation::Stop,
    });
    // 3c/G6 tier 2 (D5): the guardrails' default (today's free
    // session) + the question-policy arms (wire-only until job 5).
    roundtrip(&Guardrails::default());
    roundtrip(&QuestionPolicy::AnswerConstant("decide yourself".into()));
    roundtrip(&QuestionPolicy::Deny);

    // SessionCommand — every 3a variant.
    let commands = vec![
        SessionCommand::Prompt {
            text: "hi".into(),
            images: vec!["data:image/png;base64,AAA".into()],
        },
        SessionCommand::Steer { text: "go".into() },
        SessionCommand::CancelSteer { text: "go".into() },
        SessionCommand::Interrupt,
        SessionCommand::Compact,
        SessionCommand::Clear,
        SessionCommand::Approve {
            request_id: "r1".into(),
            decision: Approval::DenyAll,
        },
        SessionCommand::Answer {
            question_id: "q1".into(),
            answer: UserAnswer::Choice { index: 1 },
        },
        // 3c (D5): the G2/G6 wire shapes — the host-tool answer and
        // the policy-hook answer.
        SessionCommand::HostToolResult {
            call_id: "c1".into(),
            result: Some(ToolResult::success("sunny, 21°C".to_string())),
            error: None,
        },
        SessionCommand::PolicyAnswer {
            hook_id: "h1".into(),
            answer: PolicyAnswer::Gate {
                verdict: GateVerdict::Deny {
                    message: "no destructive shell".into(),
                },
            },
        },
    ];
    for command in &commands {
        roundtrip(command);
    }

    // SessionEvent — the newtype over SerializableAgentEvent + the tag.
    roundtrip(&SessionEvent {
        agent: 7,
        event: SerializableAgentEvent::Started,
        rates: None,
    });

    // 3b.5 (§5.7): a Usage event carries its derived rates — the wire
    // pins both the numbers and the field name a binding reads.
    roundtrip(&SessionEvent {
        agent: 7,
        event: SerializableAgentEvent::Usage {
            prompt_tokens: 1000,
            completion_tokens: 40,
            reasoning_tokens: 10,
            cached_tokens: 800,
            ttft_ms: Some(400),
            generation_ms: Some(200),
        },
        rates: Some(mnemo::headless::UsageRates {
            input_tok_per_s: Some(2500.0),
            output_tok_per_s: Some(200.0),
            cache_hit_ratio: 0.8,
        }),
    });

    // 3b.5 (§5.7): the live session stats — the binding-readiness
    // contract for the tally a consumer reads via
    // AgentSession::stats() (EngineStats is deliberately NOT here: it
    // is Serialize-only — SteeringStatsSnapshot has no Deserialize, and
    // a binding receives the pull but never sends it back).
    roundtrip(&mnemo::headless::SessionStats {
        requests: 2,
        prompt_tokens: 2000,
        completion_tokens: 80,
        reasoning_tokens: 20,
        cached_tokens: 1600,
        saved_tokens_by_kind: std::collections::BTreeMap::from([(
            "compaction_checkpoint".to_string(),
            0,
        )]),
        model_switches: 1,
        current_model: Some("m-x".into()),
        current_reasoning_effort: Some("high".into()),
        context_used: Some(1234),
        context_max: Some(128_000),
        ttft_ms_total: 400,
        timed_requests: 1,
        generation_ms_total: 200,
    });

    // The small types.
    roundtrip(&SessionLimits {
        max_turns: Some(3),
        wall_clock_ms: Some(120_000),
        ..SessionLimits::default()
    });
    roundtrip(&Transcript::default());
    // EngineError — constructed via serde (its constructors are
    // engine-internal; the wire shape is the public surface).
    let err: EngineError =
        serde_json::from_value(json!({"code": "session_closed", "message": "closed"}))
            .expect("the error wire shape");
    roundtrip(&err);

    // Pin the tag spellings a binding matches on (§5.3 :328).
    assert_eq!(
        serde_json::to_value(SessionCommand::Interrupt).unwrap(),
        json!({"type": "interrupt"})
    );
    assert_eq!(
        serde_json::to_value(SessionCommand::Prompt {
            text: "hi".into(),
            images: vec![]
        })
        .unwrap(),
        json!({"type": "prompt", "text": "hi", "images": []})
    );
    // The permission policy rides along in kebab-case (the 2c.5 lesson:
    // SafetyMode is kebab-case on the wire; the tag is "mode").
    let opts_json = serde_json::to_value(sample_options()).unwrap();
    assert_eq!(
        opts_json["permissions"],
        json!({"mode": "ApproveViaApi", "safety_mode": "autonomous"})
    );
}

/// §7 row 13: malformed `SessionCommand` JSON errors at the boundary —
/// unknown tags, missing fields, wrong tag keys, and non-objects all
/// reject instead of silently defaulting.
#[test]
fn session_command_malformed_json_errors() {
    // Unknown tag.
    assert!(
        serde_json::from_value::<SessionCommand>(json!({"type": "explode"})).is_err(),
        "an unknown tag must reject"
    );
    // Missing required field.
    assert!(
        serde_json::from_value::<SessionCommand>(json!({"type": "prompt"})).is_err(),
        "a missing field must reject"
    );
    // Wrong tag key ("type" is the public tag; anything else rejects).
    assert!(
        serde_json::from_value::<SessionCommand>(json!({"kind": "interrupt"})).is_err(),
        "a wrong tag key must reject"
    );
    // Non-object payload.
    assert!(
        serde_json::from_value::<SessionCommand>(json!("interrupt")).is_err(),
        "a non-object must reject"
    );
}

/// §7 row 13: every public type is Send + Sync + 'static — a headless
/// caller can hold sessions and receivers across threads and await
/// points freely.
#[test]
fn public_types_are_send_sync_static() {
    fn assert_send_sync_static<T: Send + Sync + 'static>() {}
    assert_send_sync_static::<EngineOptions>();
    assert_send_sync_static::<SpawnOptions>();
    assert_send_sync_static::<SessionCommand>();
    assert_send_sync_static::<SessionEvent>();
    assert_send_sync_static::<EventReceiver<SessionEvent>>();
    assert_send_sync_static::<SessionId>();
    assert_send_sync_static::<SessionLimits>();
    assert_send_sync_static::<MemoryBackend>();
    assert_send_sync_static::<Transcript>();
    assert_send_sync_static::<EngineError>();
    assert_send_sync_static::<RecvError>();
}

/// Real-start options for the engine fixtures: a fresh non-git workspace,
/// a temp config dir, indexing off (test speed), the free default.
fn engine_opts(workspace: mnemo::workspace::Workspace, config_dir: Option<PathBuf>) -> EngineOptions {
    EngineOptions {
        workspace,
        config_dir,
        permissions: PermissionPolicy::OverridePermissions,
        memory: MemoryBackend::Sqlite { path: None },
        index_codegraph: false,
        require_git: false,
        auto_install_embedder: false,
    }
}

/// Job 4 D1 (§5.8 :581-587): `Engine::start` without an injected provider
/// fails fast on an unready config — the bare-start fixture below
/// therefore seeds a READY one (an endpoint + key + default model; the
/// dead base_url is fine: start wires the provider, it never calls it).
fn write_ready_config(dir: &std::path::Path) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("config.toml"),
        "[general]\ndefault_provider = \"test\"\ndefault_model = \"test-model\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("endpoints.toml"),
        "[[endpoint]]\nname = \"test\"\nkind = \"openai\"\nbase_url = \"http://127.0.0.1:9/v1/\"\nmodels = [\"test-model\"]\n",
    )
    .unwrap();
    std::fs::write(dir.join("keys.toml"), "[test]\napi_key = \"sk-test\"\n").unwrap();
}

/// §7 row 9: `Engine::start` never blocks and never shows a picker — an
/// unusable workspace (a missing root, or `require_git` on a non-repo
/// root) is `EngineError` code `no_project`, returned promptly.
#[tokio::test]
async fn no_project_errors() {
    // A missing root.
    let opts = engine_opts(
        Workspace::new("/definitely/not/a/real/dir"),
        None,
    );
    let err = match Engine::start(opts).await {
        Ok(_) => panic!("a missing workspace root must error"),
        Err(e) => e,
    };
    assert_eq!(err.code(), "no_project", "never a picker: {}", err);

    // require_git + a non-repo root (the §5.9.3 :664 open-time gate —
    // either the option or the workspace marker demands a repo).
    let dir = tempfile::tempdir().unwrap();
    let opts = engine_opts(
        Workspace::new(dir.path()).requiring_git(),
        Some(tempfile::tempdir().unwrap().path().to_path_buf()),
    );
    let err = match Engine::start(opts).await {
        Ok(_) => panic!("require_git on a non-repo root must error"),
        Err(e) => e,
    };
    assert_eq!(err.code(), "no_project", "never a picker: {}", err);
}

/// §7 row 15: `start_blocking` works from a plain (non-async) context —
/// the engine owns a static tokio runtime, so a plain `fn main` can start
/// one with no ambient reactor.
#[test]
fn start_blocking_without_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    write_ready_config(cfg.path());
    let opts = engine_opts(
        Workspace::new(dir.path()),
        Some(cfg.path().to_path_buf()),
    );
    let engine =
        Engine::start_blocking(opts).expect("start_blocking builds its own runtime");
    // The host-events bus exists before any subscriber.
    let _ = engine.host_events();
    drop(engine); // the owned runtime drops with the engine
}

// --- 3a.5: the session surface (the scripted mock + the row fixtures) ---

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use futures::stream::BoxStream;

use mnemo::headless::AgentSession;
use mnemo::provider::{
    Capabilities, FinishReason, LlmClient, LlmEvent, Message, ProviderKind, ToolChoice,
    ToolSchema,
};

/// A scripted provider: each `complete` pops the next scripted event
/// batch; the call counter lets tests observe progress WITHOUT an event
/// subscriber (the zero-subscriber fixtures).
struct ScriptedLlm {
    responses: std::sync::Mutex<VecDeque<Vec<LlmEvent>>>,
    calls: AtomicUsize,
    caps: Capabilities,
}

impl ScriptedLlm {
    fn new(responses: Vec<Vec<LlmEvent>>) -> Self {
        Self {
            responses: std::sync::Mutex::new(responses.into()),
            calls: AtomicUsize::new(0),
            caps: Capabilities::openai(),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl LlmClient for ScriptedLlm {
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
        self.calls.fetch_add(1, Ordering::SeqCst);
        let next = self
            .responses
            .lock()
            .expect("script lock poisoned")
            .pop_front()
            .unwrap_or_default();
        Ok(Box::pin(futures::stream::iter(next)))
    }
}

fn text_turn(texts: &[&str]) -> Vec<LlmEvent> {
    let mut events: Vec<LlmEvent> = texts.iter().map(|t| LlmEvent::TextDelta { text: (*t).into() }).collect();
    events.push(LlmEvent::Finish {
        reason: FinishReason::Stop,
    });
    events
}

fn tool_turn(name: &str, args: &str) -> Vec<LlmEvent> {
    vec![
        LlmEvent::ToolCallStart {
            index: 0,
            id: "call_1".into(),
            name: name.into(),
        },
        LlmEvent::ToolCallArgumentDelta {
            index: 0,
            fragment: args.into(),
        },
        LlmEvent::Finish {
            reason: FinishReason::ToolCalls,
        },
    ]
}

/// A scripted engine over a fresh non-git workspace + temp config
/// (indexing off), plus the tempdirs (they must outlive the engine).
async fn scripted_engine(
    responses: Vec<Vec<LlmEvent>>,
) -> (Engine, AgentSession, tempfile::TempDir, tempfile::TempDir) {
    let root = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let opts = engine_opts(Workspace::new(root.path()), Some(cfg.path().to_path_buf()));
    let llm = std::sync::Arc::new(ScriptedLlm::new(responses));
    let engine = Engine::builder(opts)
        .provider(llm.clone())
        .start()
        .await
        .expect("the scripted engine starts");
    let session = engine.spawn(SpawnOptions::default()).await.expect("spawn");
    (engine, session, root, cfg)
}

fn git_init(root: &std::path::Path) {
    let out = std::process::Command::new("git")
        .args(["init", root.to_str().unwrap()])
        .output()
        .expect("git runs");
    assert!(out.status.success(), "git init failed: {:?}", out);
}

/// §7 row 10: run_to_completion on a scripted provider returns the final
/// text, and the stream shows Started … Finished in order.
#[tokio::test]
async fn run_to_completion_mock_provider() {
    let (_engine, session, _root, _cfg) = scripted_engine(vec![text_turn(&["hello ", "world"])])
        .await;
    let mut rx = session.events();
    let outcome = session.run_to_completion("hi").await.expect("the turn completes");
    assert_eq!(outcome.final_text, "hello world");
    let mut saw_started = false;
    let mut saw_finished = false;
    while let Ok(event) =
        tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await
    {
        match event.expect("event").event {
            SerializableAgentEvent::Started => saw_started = true,
            SerializableAgentEvent::Finished { .. } => {
                saw_finished = true;
                break;
            }
            _ => {}
        }
    }
    assert!(saw_started, "the stream shows Started");
    assert!(saw_finished, "the stream shows Finished");
}

/// §7 row 12: two events() subscribers both receive every event; dropping
/// one never stalls the other.
#[tokio::test]
async fn two_subscribers_both_receive() {
    let (_engine, session, _root, _cfg) =
        scripted_engine(vec![text_turn(&["hi"])]).await;
    let a = session.events();
    let b = session.events();
    session
        .run_to_completion("hello")
        .await
        .expect("the turn completes");
    /// Drain a subscription to its Finished event — the shared body of
    /// the two-subscriber comparison.
    async fn drain_to_finished(
        mut rx: mnemo::headless::EventReceiver<mnemo::headless::SessionEvent>,
    ) -> Vec<String> {
        let mut out = Vec::new();
        while let Ok(ev) =
            tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await
        {
            let ev = ev.expect("event");
            let kind = match &ev.event {
                SerializableAgentEvent::Started => "started".to_string(),
                SerializableAgentEvent::TextDelta { .. } => "text".to_string(),
                SerializableAgentEvent::Finished { .. } => {
                    out.push("finished".to_string());
                    break;
                }
                _ => "other".to_string(),
            };
            out.push(kind);
        }
        out
    }

    let sa = drain_to_finished(a).await;
    let sb = drain_to_finished(b).await;    assert_eq!(sa, sb, "both subscribers see the identical sequence");
    assert!(
        sa.contains(&"started".to_string()) && sa.contains(&"finished".to_string())
    );
}

/// §7 row 15: every send on a closed session fails with code
/// "session_closed".
#[tokio::test]
async fn closed_session_error_code() {
    let (_engine, session, _root, _cfg) = scripted_engine(vec![]).await;
    session.close().await.expect("close");
    let err = session
        .send(SessionCommand::Prompt {
            text: "hi".into(),
            images: vec![],
        })
        .await
        .expect_err("a closed session rejects sends");
    assert_eq!(err.code(), "session_closed");
}

/// §7 row 15: shutdown ends the sessions — the event bus closes.
#[tokio::test]
async fn shutdown_leaves_no_tasks() {
    let (engine, session, _root, _cfg) = scripted_engine(vec![]).await;
    let mut rx = session.events();
    engine.shutdown().await.expect("shutdown");
    // The session handle holds a bus-sender clone — drop it so the
    // channel's lifetime reflects the engine's alone (the pump aborted,
    // no engine tasks remain).
    drop(session);
    drop(_root);
    drop(_cfg);
    match rx.recv().await {
        Err(RecvError::Closed) => {}
        other => panic!("the bus must close after shutdown: {:?}", other.is_ok()),
    }
}

/// §5.3 :350-354 (row 14): a question with ZERO subscribers gets the
/// immediate constant Freeform answer — the turn never blocks (the second
/// LLM call happens without any subscriber).
#[tokio::test]
async fn ask_user_without_subscriber_answers_constant() {
    let root = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let opts = engine_opts(Workspace::new(root.path()), Some(cfg.path().to_path_buf()));
    let llm = std::sync::Arc::new(ScriptedLlm::new(vec![
        tool_turn(
            "ask_user",
            r#"{"question":"which?","options":[{"label":"A"},{"label":"B"}]}"#,
        ),
        text_turn(&["decided"]),
    ]));
    let engine = Engine::builder(opts)
        .provider(llm.clone())
        .start()
        .await
        .expect("starts");
    let session = engine.spawn(SpawnOptions::default()).await.expect("spawn");
    // NO events() subscription — zero subscribers at question time.
    session
        .send(SessionCommand::Prompt {
            text: "ask".into(),
            images: vec![],
        })
        .await
        .expect("prompt");
    for _ in 0..100 {
        if llm.calls() >= 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(
        llm.calls(),
        2,
        "the constant answer unblocked ask_user — the second request ran"
    );
}

/// §7 row 11 (engine form): under OverridePermissions a core operation
/// (git merge) executes with NO approval request — one ApprovalBypassed
/// audit event (reason "core_op") instead.
#[tokio::test]
async fn override_permissions_runs_merge_push_without_approval() {
    let root = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    git_init(root.path());
    let opts = engine_opts(Workspace::new(root.path()), Some(cfg.path().to_path_buf()));
    let engine = Engine::builder(opts)
        .provider(std::sync::Arc::new(ScriptedLlm::new(vec![
            tool_turn("git", r#"{"subcommand":"merge","branch":"feat-x"}"#),
            text_turn(&["merged"]),
        ])))
        .start()
        .await
        .expect("starts");
    let session = engine.spawn(SpawnOptions::default()).await.expect("spawn");
    let mut rx = session.events();
    session
        .run_to_completion("merge it")
        .await
        .expect("the turn completes");
    let mut bypassed = 0;
    let mut approval_requested = false;
    let mut saw_tool_result = false;
    while let Ok(ev) =
        tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await
    {
        match ev.expect("event").event {
            SerializableAgentEvent::ApprovalBypassed { tool, reason } => {
                assert_eq!(tool, "git");
                assert_eq!(reason, "core_op");
                bypassed += 1;
            }
            SerializableAgentEvent::ApprovalRequest { .. } => approval_requested = true,
            SerializableAgentEvent::ToolResult { .. } => saw_tool_result = true,
            SerializableAgentEvent::Finished { .. } => break,
            _ => {}
        }
    }
    assert_eq!(bypassed, 1, "exactly one bypass audit event");
    assert!(!approval_requested, "OverridePermissions never prompts");
    assert!(saw_tool_result, "the gated call executed directly");
}

/// §7 row 11 (engine form): under ApproveViaApi the approval surfaces as
/// ApprovalRequest and SessionCommand::Approve resolves it — the call
/// then executes.
#[tokio::test]
async fn approve_via_api_prompts_and_honours_decision() {
    let root = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    git_init(root.path());
    let opts = engine_opts(Workspace::new(root.path()), Some(cfg.path().to_path_buf()));
    let engine = Engine::builder(opts)
        .provider(std::sync::Arc::new(ScriptedLlm::new(vec![
            tool_turn("git", r#"{"subcommand":"commit","message":"x"}"#),
            text_turn(&["done"]),
        ])))
        .start()
        .await
        .expect("starts");
    let session = engine
        .spawn(SpawnOptions {
            permissions: Some(PermissionPolicy::ApproveViaApi {
                safety_mode: SafetyMode::ApproveEachAction,
            }),
            ..SpawnOptions::default()
        })
        .await
        .expect("spawn");
    let mut rx = session.events();
    session
        .send(SessionCommand::Prompt {
            text: "commit".into(),
            images: vec![],
        })
        .await
        .expect("prompt");
    let mut approval_requested = false;
    let mut saw_tool_result = false;
    while let Ok(ev) =
        tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await
    {
        match ev.expect("event").event {
            SerializableAgentEvent::ApprovalRequest { tool_call_id, .. } => {
                approval_requested = true;
                session
                    .send(SessionCommand::Approve {
                        request_id: tool_call_id,
                        decision: Approval::Approve,
                    })
                    .await
                    .expect("the approval resolves");
            }
            SerializableAgentEvent::ToolResult { .. } => saw_tool_result = true,
            SerializableAgentEvent::Finished { .. } => break,
            _ => {}
        }
    }
    assert!(approval_requested, "ApproveViaApi surfaces the request");
    assert!(saw_tool_result, "the approved call executed");
}

/// §7 row 8 (engine form): a free agent (NoPolicy — the engine default)
/// cannot write under .coding/plans/** — the sandbox refuses, the ToolResult
/// is an error.
#[tokio::test]
async fn free_agent_writes_no_plans() {
    let (_engine, session, root, _cfg) = scripted_engine(vec![
        tool_turn(
            "file_write",
            r#"{"path":".coding/plans/stack.json","content":"x"}"#,
        ),
        text_turn(&["ok"]),
    ])
    .await;
    let mut rx = session.events();
    session
        .run_to_completion("write the stack")
        .await
        .expect("the turn completes");
    let mut refusal: Option<(bool, String)> = None;
    while let Ok(ev) =
        tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await
    {
        if let SerializableAgentEvent::ToolResult { result, .. } = ev.expect("event").event {
            refusal = Some((result.success, result.output));
            break;
        }
    }
    let (success, output) = refusal.expect("the file_write call produced a result");
    assert!(!success, "the plan write is refused: {output}");
    assert!(
        !root.path().join(".coding/plans/stack.json").exists(),
        "nothing landed under .coding/plans"
    );
}

// --- Job 4 D7 (G8, §5.9.8 :725-731): the injected store + shared resources ---

/// A `MemoryStoreTrait` wrapper (row 33 leg 1) that LOGS every call and
/// forwards to an in-memory real store — the fixture proves the whole
/// agent funnel (the memory tools + the loop's capture + consolidation)
/// reaches the INJECTED instance, not an engine-built store.
struct LoggingStore {
    inner: std::sync::Arc<mnemo::memory::MemoryStore>,
    log: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl LoggingStore {
    fn new() -> Self {
        Self {
            inner: std::sync::Arc::new(
                mnemo::memory::MemoryStore::open_in_memory(std::sync::Arc::new(
                    mnemo::memory::embedder::HashEmbedder::new(),
                ))
                .unwrap(),
            ),
            log: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    fn note(&self, entry: String) {
        self.log.lock().expect("log lock poisoned").push(entry);
    }
}

use mnemo::error::Result as MemResult;
use mnemo::memory::types::{
    ArchiveSearchHit, ArchivedToolResult, Memory, MemoryFilter, MemoryTier, ProjectStats,
    RequestStats, SavingsEvent, SavingsStats, Session, SessionStats, SessionSummary, SpendEvent,
};
use mnemo::memory::{MemoryStoreTrait, ScoredMemory};

#[async_trait]
impl MemoryStoreTrait for LoggingStore {
    async fn write(&self, memory: Memory) -> MemResult<String> {
        self.note(format!("write:{}", memory.tier.to_string().to_lowercase()));
        self.inner.write(memory).await
    }
    async fn recall(&self, query: &str, _filter: &MemoryFilter) -> MemResult<Vec<ScoredMemory>> {
        self.note(format!("recall:{query}"));
        self.inner.recall(query, _filter).await
    }
    async fn recall_peek(&self, query: &str, filter: &MemoryFilter) -> MemResult<Vec<ScoredMemory>> {
        self.inner.recall_peek(query, filter).await
    }
    async fn access(&self, id: &str) -> MemResult<()> {
        self.inner.access(id).await
    }
    async fn list_by_tier(&self, tier: MemoryTier) -> MemResult<Vec<Memory>> {
        self.note(format!("list_by_tier:{}", tier.to_string().to_lowercase()));
        self.inner.list_by_tier(tier).await
    }
    async fn update_memory(
        &self,
        id: &str,
        title: Option<&str>,
        content: Option<&str>,
    ) -> MemResult<bool> {
        self.inner.update_memory(id, title, content).await
    }
    async fn supersede_memory(&self, old_id: &str, new_memory: Memory) -> MemResult<String> {
        self.inner.supersede_memory(old_id, new_memory).await
    }
    async fn delete_memory(&self, id: &str) -> MemResult<bool> {
        self.inner.delete_memory(id).await
    }
    async fn get_memory(&self, id: &str) -> MemResult<Option<Memory>> {
        self.inner.get_memory(id).await
    }
    async fn delete_derived(&self) -> MemResult<usize> {
        self.inner.delete_derived().await
    }
    async fn index_state_get(&self, source_key: &str) -> MemResult<Option<(String, String)>> {
        self.inner.index_state_get(source_key).await
    }
    async fn index_state_upsert(
        &self,
        source_key: &str,
        content_hash: &str,
        memory_id: &str,
    ) -> MemResult<()> {
        self.inner
            .index_state_upsert(source_key, content_hash, memory_id)
            .await
    }
    async fn index_state_remove(&self, source_key: &str) -> MemResult<()> {
        self.inner.index_state_remove(source_key).await
    }
    async fn index_state_clear(&self) -> MemResult<()> {
        self.inner.index_state_clear().await
    }
    async fn index_state_keys(&self) -> MemResult<Vec<String>> {
        self.inner.index_state_keys().await
    }
    async fn set_derived_metadata(
        &self,
        id: &str,
        superseded_by: Option<&str>,
        data: &serde_json::Value,
    ) -> MemResult<bool> {
        self.inner
            .set_derived_metadata(id, superseded_by, data)
            .await
    }
    async fn list_filtered(&self, filter: &MemoryFilter) -> MemResult<Vec<Memory>> {
        self.inner.list_filtered(filter).await
    }
    async fn delete_working_for_session(&self, session_id: &str) -> MemResult<usize> {
        self.note(format!("delete_working_for_session:{session_id}"));
        self.inner.delete_working_for_session(session_id).await
    }
    async fn stored_model_fingerprints(&self) -> MemResult<Vec<(String, usize)>> {
        self.inner.stored_model_fingerprints().await
    }
    async fn reembed_all(
        &self,
        embedder: std::sync::Arc<dyn mnemo::memory::embedder::Embedder>,
        progress: Option<&(dyn Fn(usize, usize) + Send + Sync)>,
    ) -> MemResult<usize> {
        self.inner.reembed_all(embedder, progress).await
    }
    async fn start_session(&self, project: &str) -> MemResult<Session> {
        self.inner.start_session(project).await
    }
    async fn end_session(&self, id: &str) -> MemResult<()> {
        self.inner.end_session(id).await
    }
    async fn record_tool_event(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        tool_input: serde_json::Value,
        tool_output: &str,
        error: Option<&str>,
    ) -> MemResult<String> {
        self.note(format!("capture:{tool_name}"));
        self.inner
            .record_tool_event(session_id, tool_name, tool_input, tool_output, error)
            .await
    }
    async fn record_request_stats(&self, stats: &RequestStats) -> MemResult<()> {
        self.inner.record_request_stats(stats).await
    }
    async fn record_savings_event(&self, event: &SavingsEvent) -> MemResult<()> {
        self.inner.record_savings_event(event).await
    }
    async fn savings_events_rows(&self, session_id: Option<&str>) -> MemResult<Vec<SavingsEvent>> {
        self.inner.savings_events_rows(session_id).await
    }
    async fn savings_stats(&self) -> MemResult<SavingsStats> {
        self.inner.savings_stats().await
    }
    async fn record_spend_event(&self, event: &SpendEvent) -> MemResult<()> {
        self.inner.record_spend_event(event).await
    }
    async fn spend_rows_for_plan(&self, plan_id: &str) -> MemResult<Vec<SpendEvent>> {
        self.inner.spend_rows_for_plan(plan_id).await
    }
    async fn archive_tool_result(
        &self,
        session_id: Option<&str>,
        tool: &str,
        detail: Option<&str>,
        content: &str,
    ) -> MemResult<String> {
        self.inner
            .archive_tool_result(session_id, tool, detail, content)
            .await
    }
    async fn expand_tool_result(&self, id: &str) -> MemResult<Option<ArchivedToolResult>> {
        self.inner.expand_tool_result(id).await
    }
    async fn search_archive(&self, query: &str, limit: usize) -> MemResult<Vec<ArchiveSearchHit>> {
        self.inner.search_archive(query, limit).await
    }
    async fn request_stats_rows(&self, session_id: &str) -> MemResult<Vec<RequestStats>> {
        self.inner.request_stats_rows(session_id).await
    }
    async fn session_stats(&self, session_id: &str) -> MemResult<SessionStats> {
        self.inner.session_stats(session_id).await
    }
    async fn project_stats(&self) -> MemResult<ProjectStats> {
        self.inner.project_stats().await
    }
    async fn session_list(&self) -> MemResult<Vec<SessionSummary>> {
        self.inner.session_list().await
    }
}

/// §5.9.8 :726-728 (row 33 leg 1): `EngineBuilder::memory_store` overrides
/// the backend wiring — a scripted run over the INJECTED store proves the
/// whole agent funnel reaches the injected instance: the memory tools'
/// write + recall, the loop's tool-event capture, and consolidation's
/// working-tier read. The engine-side debug API (`memory()`) is inert for
/// an injected store (it owns its own debug surface).
#[tokio::test]
async fn injected_memory_store_receives_the_agent_funnel_calls() {
    let root = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let opts = engine_opts(Workspace::new(root.path()), Some(cfg.path().to_path_buf()));
    let store = std::sync::Arc::new(LoggingStore::new());
    let llm = std::sync::Arc::new(ScriptedLlm::new(vec![
        tool_turn(
            "memory_write",
            r#"{"tier":"semantic","title":"DECISION: injected store","content":"the funnel must reach the injected store"}"#,
        ),
        tool_turn("memory_search", r#"{"query":"injected store"}"#),
        // A bogus session id: the consolidation READ (the working-tier
        // list) is the call under test, and the empty result keeps the
        // script deterministic (no consolidation LLM turns).
        tool_turn("memory_consolidate", r#"{"session_id":"bogus-session"}"#),
        text_turn(&["done"]),
        text_turn(&["done"]),
    ]));
    let engine = Engine::builder(opts)
        .memory_store(store.clone())
        .provider(llm)
        .start()
        .await
        .expect("the injected-store engine starts");
    assert!(
        engine.memory().is_err(),
        "the debug API is inert for an injected store"
    );
    let session = engine.spawn(SpawnOptions::default()).await.expect("spawn");
    let outcome = session
        .run_to_completion("use your memory tools")
        .await
        .expect("the turn completes");
    assert_eq!(outcome.final_text, "done");

    let log = store.log.lock().expect("log lock poisoned").join("\n");
    assert!(log.contains("write:semantic"), "the write reached the store:\n{log}");
    assert!(
        log.contains("recall:injected store"),
        "the recall reached the store:\n{log}"
    );
}

/// A counting embedder — every embed() bumps the shared counter, so two
/// stores sharing ONE instance are observable without ONNX.
struct CountingEmbedder {
    count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait]
impl mnemo::memory::embedder::Embedder for CountingEmbedder {
    async fn embed(&self, _text: &str) -> Vec<f32> {
        self.count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        vec![0.0; 8]
    }
    fn dim(&self) -> usize {
        8
    }
    fn model_id(&self) -> &str {
        // Matches the planned hash embedder, so the shared arm engages.
        "hash"
    }
}

/// §5.9.8 :729-731: two engines built over ONE `SharedResources` share
/// the SAME embedder instance (the N-copies guarantee — N tenants never
/// load N models). One memory_write per engine embeds through the single
/// shared counter; the shared-match arm also drops each engine's
/// background load plan (both report Ready).
#[tokio::test]
async fn shared_resources_share_one_embedder_across_engines() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let counter = Arc::new(AtomicUsize::new(0));
    let shared = Arc::new(
        mnemo::headless::SharedResources::new().with_embedder(Arc::new(CountingEmbedder {
            count: counter.clone(),
        })),
    );

    let mut previous = 0usize;
    for _ in 0..2 {
        let root = tempfile::tempdir().unwrap();
        let cfg = tempfile::tempdir().unwrap();
        let opts = engine_opts(Workspace::new(root.path()), Some(cfg.path().to_path_buf()));
        let llm = Arc::new(ScriptedLlm::new(vec![
            tool_turn(
                "memory_write",
                r#"{"tier":"semantic","title":"DECISION: shared","content":"one embedder for every engine"}"#,
            ),
            text_turn(&["done"]),
        ]));
        let engine = Engine::builder(opts)
            .shared_resources(shared.clone())
            .provider(llm)
            .start()
            .await
            .expect("the shared-resources engine starts");
        let setup =
            mnemo::headless::Setup::open(Some(cfg.path().to_path_buf())).expect("open");
        let live = setup.for_engine(&engine);
        assert_eq!(
            live.embedder_status(),
            mnemo::memory::embedder::EmbedderStatus::Ready,
            "the shared-match arm resolves Ready (no background plan)"
        );
        let session = engine.spawn(SpawnOptions::default()).await.expect("spawn");
        let outcome = session
            .run_to_completion("write one memory")
            .await
            .expect("the turn completes");
        assert_eq!(outcome.final_text, "done");
        let now = counter.load(Ordering::SeqCst);
        assert!(
            now > previous,
            "this engine's store embedded through the shared instance \
             (counter {now}, previous {previous})"
        );
        previous = now;
    }

    // The exact embed count is unpinned (the write + the loop's capture +
    // the auto-recall each embed); the SHARING is the contract: every
    // engine built over the handle incremented the ONE counter.
    assert!(
        previous >= 2,
        "both engines embedded through the shared instance ({previous} embeds)"
    );
}

/// Review round 1, finding 2 (job 4 D9): the classifier flip must reach
/// the LIVE config Arc, not just disk — the completing pipeline re-reads
/// the Arc for the autostart decision, so a disk-only flip leaves an
/// engine-backed enable dark and lets a mid-setup disable lose its race.
/// The guard-rejection path asserts the red/green WITHOUT spawning the
/// network pipeline: the flip is in the Arc even when the guard rejects,
/// and the disable leg live-applies too.
#[tokio::test]
async fn classifier_setup_flip_reaches_the_live_config_before_the_guard() {
    let root = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let opts = engine_opts(Workspace::new(root.path()), Some(cfg.path().to_path_buf()));
    // An injected provider bypasses the not_configured gate (the
    // fixture makes no LLM calls — the empty script is never popped).
    let llm = std::sync::Arc::new(ScriptedLlm::new(Vec::new()));
    let engine = Engine::builder(opts)
        .provider(llm)
        .start()
        .await
        .expect("the engine starts (provider-injected)");
    let setup = mnemo::headless::Setup::open(Some(cfg.path().to_path_buf())).expect("open");
    let live = setup.for_engine(&engine);
    assert_eq!(
        live.live_laya_enabled().await,
        Some(false),
        "engine_opts' config leaves Laya off"
    );
    // Pre-claim the guard the way a running pipeline would.
    assert!(live.laya_manager().begin_setup());
    let err = live
        .classifier_setup(true)
        .await
        .expect_err("the guard rejects");
    assert_eq!(err.code(), "setup_in_flight");
    // RED before the fix: the flip never reached the Arc (only disk).
    assert_eq!(
        live.live_laya_enabled().await,
        Some(true),
        "the flip is IN the live config even when the guard rejects"
    );
    // ... and on disk:
    let settings = live.settings().expect("settings");
    assert!(settings.general.laya.enabled);
    // The disable leg live-applies too (the mid-setup race winner):
    let mut rx = live
        .classifier_setup(false)
        .await
        .expect("the disable leg");
    let _ = rx.recv().await; // the terminal Done
    assert_eq!(
        live.live_laya_enabled().await,
        Some(false),
        "the disable wins the race — the Arc flips back immediately"
    );
}