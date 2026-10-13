//! The §5.3 public-API contract fixtures (job 3a.3): the wire shapes and
//! the Send/Sync/'static guarantees are pinned BEFORE the Engine exists —
//! every later 3a step keeps these green (the types-first contract).

use serde_json::json;
use std::path::PathBuf;

use mnemo::agent::policy::PermissionPolicy;
use mnemo::config::SafetyMode;
use mnemo::headless::{
    Engine, EngineError, EngineOptions, EventReceiver, MemoryBackend, RecvError, SessionCommand,
    SessionEvent, SessionId, SessionLimits, SpawnOptions, Transcript,
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
    });

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
    ];
    for command in &commands {
        roundtrip(command);
    }

    // SessionEvent — the newtype over SerializableAgentEvent + the tag.
    roundtrip(&SessionEvent {
        agent: 7,
        event: SerializableAgentEvent::Started,
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
    let opts_json = serde_json::to_value(&sample_options()).unwrap();
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
    let opts = engine_opts(
        Workspace::new(dir.path()),
        Some(tempfile::tempdir().unwrap().path().to_path_buf()),
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
