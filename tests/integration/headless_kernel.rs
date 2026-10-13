//! The kernel-level engine fixtures (job 3a.4): the workspace contract —
//! an engine starts on a plain NON-git directory (G3) and honors an
//! out-of-tree state dir (the row-27 engine form).

use std::path::PathBuf;

use mnemo::agent::policy::{Continuation, GateVerdict, PermissionPolicy};
use mnemo::headless::{Engine, EngineOptions, MemoryBackend};
use mnemo::workspace::Workspace;

/// Real-start options: a fresh workspace, a temp config dir, indexing off
/// (test speed), the free default.
fn opts(workspace: Workspace, config_dir: Option<PathBuf>) -> EngineOptions {
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

/// G3 (§5.9.3): an engine works on ANY directory — a fresh temp dir with
/// no git repo starts cleanly (require_git false; the Git tool group
/// simply does not register).
#[tokio::test]
async fn workspace_without_git_starts() {
    let root = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let engine = Engine::start(opts(
        Workspace::new(root.path()),
        Some(cfg.path().to_path_buf()),
    ))
    .await
    .expect("a plain non-git directory starts");
    engine
        .shutdown()
        .await
        .expect("shutdown is clean with no sessions");
}

/// G3's row-27 engine form: the state dir may sit anywhere — the memory
/// database lands at the OUT-of-tree state dir, not under the root.
#[tokio::test]
async fn state_dir_outside_tree() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let workspace = Workspace::with_state_dir(root.path(), state.path());
    let engine = Engine::start(opts(workspace, Some(cfg.path().to_path_buf())))
        .await
        .expect("an out-of-tree state dir starts");
    engine.shutdown().await.expect("shutdown ok");
    assert!(
        state.path().join("memory.db").exists(),
        "the memory db lands at the out-of-tree state dir ({})",
        state.path().display()
    );
    assert!(
        !root.path().join(".coding").exists(),
        "the root stays untouched — no in-tree state dir appears"
    );
}

// --- 3a.6: G5 (the transcript fixtures — the recording mock) ---

use std::collections::VecDeque;

use async_trait::async_trait;
use futures::stream::BoxStream;

use mnemo::config::SafetyMode;
use mnemo::headless::{
    ExternalPolicySpec, HostEvent, HookKind, HostToolSpec, PendingInteraction, PolicyAnswer,
    SessionCommand, SessionEvent, SessionLimits, SpawnOptions, TimeoutAction, Transcript,
};
use mnemo::provider::{
    Capabilities, FinishReason, LimitKind, LlmClient, LlmEvent, Message, ProviderKind, Role,
    ToolChoice, ToolSchema,
};
use mnemo::tool::{SafetyLevel, ToolResult};
use mnemo::runtime::channels::{Approval, SerializableAgentEvent};

/// A scripted provider that RECORDS every request's message slice — the
/// byte-prefix fixture's source of truth.
struct RecordingLlm {
    responses: std::sync::Mutex<VecDeque<Vec<LlmEvent>>>,
    requests: std::sync::Mutex<Vec<Vec<Message>>>,
    caps: Capabilities,
}

impl RecordingLlm {
    fn new(responses: Vec<Vec<LlmEvent>>) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            responses: std::sync::Mutex::new(responses.into()),
            requests: std::sync::Mutex::new(Vec::new()),
            caps: Capabilities::openai(),
        })
    }

    fn requests(&self) -> Vec<Vec<Message>> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl LlmClient for RecordingLlm {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
    fn kind(&self) -> ProviderKind {
        ProviderKind::OpenAI
    }
    fn model(&self) -> &str {
        "recording"
    }
    async fn complete(
        &self,
        messages: &[Message],
        _tools: &[ToolSchema],
        _tool_choice: Option<ToolChoice>,
    ) -> mnemo::error::Result<BoxStream<'_, LlmEvent>> {
        self.requests.lock().unwrap().push(messages.to_vec());
        let next = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_default();
        Ok(Box::pin(futures::stream::iter(next)))
    }
}

fn g5_text_turn(texts: &[&str]) -> Vec<LlmEvent> {
    let mut events: Vec<LlmEvent> = texts
        .iter()
        .map(|t| LlmEvent::TextDelta { text: (*t).into() })
        .collect();
    events.push(LlmEvent::Finish {
        reason: FinishReason::Stop,
    });
    events
}

fn g5_tool_turn(name: &str, args: &str) -> Vec<LlmEvent> {
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

fn g5_git_init(root: &std::path::Path) {
    let out = std::process::Command::new("git")
        .args(["init", root.to_str().unwrap()])
        .output()
        .expect("git runs");
    assert!(out.status.success(), "git init failed: {:?}", out);
}

/// §5.9.5 :693-695 (the provider prefix-cache contract): a resumed
/// session's first request reproduces the original byte prefix — every
/// original message serializes IDENTICALLY (same workspace + config ⇒
/// same head/tail; the transcript + the new prompt are the only delta).
#[tokio::test]
async fn history_resume_preserves_request_bytes() {
    let root = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let base = || opts(Workspace::new(root.path()), Some(cfg.path().to_path_buf()));

    // Engine 1: one scripted turn; capture its request.
    let llm1 = RecordingLlm::new(vec![g5_text_turn(&["hello"])]);
    let engine1 = Engine::builder(base())
        .provider(llm1.clone())
        .start()
        .await
        .expect("engine 1 starts");
    let session1 = engine1.spawn(SpawnOptions::default()).await.expect("spawn 1");
    session1
        .run_to_completion("hi")
        .await
        .expect("turn 1 completes");
    let transcript = session1.history().await.expect("the history exports");
    assert!(
        transcript.messages.iter().any(|m| m.role
            == mnemo::provider::Role::User),
        "the transcript carries the conversation"
    );
    let first = llm1.requests()[0].clone();
    engine1.shutdown().await.expect("engine 1 shuts down");
    drop(session1);

    // Engine 2 on the SAME workspace + config (same head/tail), resumed
    // from the transcript.
    let llm2 = RecordingLlm::new(vec![g5_text_turn(&["ok"])]);
    let engine2 = Engine::builder(base())
        .provider(llm2.clone())
        .start()
        .await
        .expect("engine 2 starts");
    let session2 = engine2
        .spawn(SpawnOptions {
            resume: Some(Transcript {
                messages: transcript.messages.clone(),
            }),
            ..SpawnOptions::default()
        })
        .await
        .expect("spawn 2 resumes");
    session2
        .run_to_completion("more")
        .await
        .expect("turn 2 completes");
    let second = llm2.requests()[0].clone();

    // The contract (§5.9.5 :693-695): the same transcript + PromptSpec +
    // ToolSelection + model reproduces the pre-restart request bytes —
    // every original message rides BYTE-IDENTICAL. The volatile tail +
    // context footer sit AFTER the conversation history by design
    // (build_volatile_tail: the stable head stays cache-immune), so the
    // resumed layout is [head, history(+reply, +new prompt), tail, footer]
    // — positions shift by the 2 inserted messages.
    assert_eq!(
        second.len(),
        first.len() + 2,
        "resumed request = the transcript (reply + new prompt appended)"
    );
    for (i, message) in first.iter().enumerate() {
        let j = if i <= 1 { i } else { i + 2 };
        let a = serde_json::to_value(message).unwrap();
        let b = serde_json::to_value(&second[j]).unwrap();
        assert_eq!(
            a, b,
            "message {i} must serialize byte-identically after resume:\nENGINE1: {a}\nENGINE2[{j}]: {b}"
        );
    }
    // The inserted pair: the transcript's carried reply (byte-identical to
    // the export) + the new user prompt.
    let carried = transcript
        .messages
        .last()
        .expect("the transcript has the reply");
    assert_eq!(
        serde_json::to_value(carried).unwrap(),
        serde_json::to_value(&second[2]).unwrap(),
        "the carried reply rides byte-identically"
    );
    assert_eq!(
        serde_json::to_value(&second[3]).unwrap()["role"],
        "user",
        "the new prompt follows the history"
    );
    engine2.shutdown().await.expect("engine 2 shuts down");
}

/// §5.9.5: the snapshot reports a pending approval while the session
/// blocks on one (and clears it once answered).
#[tokio::test]
async fn snapshot_reports_pending_approval() {
    let root = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    g5_git_init(root.path());
    let engine = Engine::builder(opts(Workspace::new(root.path()), Some(cfg.path().to_path_buf())))
        .provider(RecordingLlm::new(vec![
            g5_tool_turn("git", r#"{"subcommand":"commit","message":"x"}"#),
            g5_text_turn(&["done"]),
        ]))
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

    // Wait for the approval, then snapshot mid-block.
    let mut approval_id = None;
    while let Ok(ev) =
        tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await
    {
        if let SerializableAgentEvent::ApprovalRequest { tool_call_id, .. } =
            ev.expect("event").event
        {
            approval_id = Some(tool_call_id);
            break;
        }
    }
    let approval_id = approval_id.expect("the approval surfaces");
    let snap = session.snapshot().await.expect("the snapshot works mid-block");
    match snap.pending {
        Some(PendingInteraction::Approval { request_id }) => {
            assert_eq!(request_id, approval_id);
        }
        other => panic!("the snapshot must report the pending approval: {other:?}"),
    }

    // Answer it — the pending clears and the turn finishes.
    session
        .send(SessionCommand::Approve {
            request_id: approval_id,
            decision: Approval::Approve,
        })
        .await
        .expect("the approval resolves");
    while let Ok(ev) =
        tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await
    {
        if let SerializableAgentEvent::Finished { .. } = ev.expect("event").event {
            break;
        }
    }
    let after = session.snapshot().await.expect("the snapshot works after");
    assert!(after.pending.is_none(), "the pending cleared once answered");
    assert!(
        !after.transcript.messages.is_empty(),
        "between turns the snapshot carries the full transcript"
    );
    engine.shutdown().await.expect("shutdown");
}

// --- 3a.7: G7 (the limits fixtures) ---

/// §5.9.7 :718-723: `max_turns` counts the session's provider requests —
/// the third prompt's turn ends with Finished{LimitReached{max_turns}}
/// WITHOUT reaching the provider, and the session parks (a fourth prompt
/// errors `session_parked`).
#[tokio::test]
async fn limits_max_turns_parks_the_session() {
    let root = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let llm = RecordingLlm::new(vec![g5_text_turn(&["one"]), g5_text_turn(&["two"])]);
    let engine = Engine::builder(opts(Workspace::new(root.path()), Some(cfg.path().to_path_buf())))
        .provider(llm.clone())
        .start()
        .await
        .expect("engine starts");
    let session = engine
        .spawn(SpawnOptions {
            limits: Some(SessionLimits {
                max_turns: Some(2),
                ..SessionLimits::default()
            }),
            ..SpawnOptions::default()
        })
        .await
        .expect("spawn");
    let first = session.run_to_completion("a").await.expect("turn 1");
    assert_eq!(first.final_text, "one");
    let second = session.run_to_completion("b").await.expect("turn 2");
    assert_eq!(second.final_text, "two");

    // Turn 3 never reaches the provider — the limit ends it.
    let third = session
        .run_to_completion("c")
        .await
        .expect("the limited turn still completes (with an empty outcome)");
    assert!(
        matches!(
            third.finish_reason,
            Some(FinishReason::LimitReached {
                which: LimitKind::MaxTurns
            })
        ),
        "turn 3 reports LimitReached::MaxTurns, got {:?}",
        third.finish_reason
    );
    assert_eq!(
        llm.requests().len(),
        2,
        "the provider saw exactly the two allowed requests"
    );

    // Parked: a further prompt errors session_parked.
    let err = session
        .run_to_completion("d")
        .await
        .expect_err("a parked session rejects the prompt");
    assert_eq!(err.code(), "session_parked");

    // The raw send surface parks too (engine-scoped, the pump sets it —
    // review round-1 F3), and a re-acquired handle sees the same state
    // (F1: Engine::session).
    let again = engine
        .session(&session.id())
        .await
        .expect("the handle re-acquires while the session is live");
    let err = again
        .send(SessionCommand::Prompt {
            text: "raw".into(),
            images: vec![],
        })
        .await
        .expect_err("the raw surface parks too");
    assert_eq!(err.code(), "session_parked");
    engine.shutdown().await.expect("engine shuts down");
}

/// §5.9.7 :718-723: the wall-clock budget (from the limits epoch = the
/// spawn) ends a turn with Finished{LimitReached{wall_clock}} once
/// elapsed.
#[tokio::test]
async fn limits_wall_clock_parks_the_session() {
    let root = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let llm = RecordingLlm::new(vec![g5_text_turn(&["one"])]);
    let engine = Engine::builder(opts(Workspace::new(root.path()), Some(cfg.path().to_path_buf())))
        .provider(llm.clone())
        .start()
        .await
        .expect("engine starts");
    let session = engine
        .spawn(SpawnOptions {
            limits: Some(SessionLimits {
                wall_clock_ms: Some(200),
                ..SessionLimits::default()
            }),
            ..SpawnOptions::default()
        })
        .await
        .expect("spawn");
    let first = session.run_to_completion("a").await.expect("turn 1");
    assert_eq!(first.final_text, "one");

    // Burn past the budget; the next turn's pre-request check hits.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let second = session
        .run_to_completion("b")
        .await
        .expect("the limited turn still completes (with an empty outcome)");
    assert!(
        matches!(
            second.finish_reason,
            Some(FinishReason::LimitReached {
                which: LimitKind::WallClock
            })
        ),
        "turn 2 reports LimitReached::WallClock, got {:?}",
        second.finish_reason
    );
    assert_eq!(
        llm.requests().len(),
        1,
        "the wall-clock turn never reached the provider"
    );
    engine.shutdown().await.expect("engine shuts down");
}

// --- 3a.8: D8/G4 (the two-engines isolation fixture) ---

/// §5.9.4 :680-682 (G4): two Engines — different workspaces + config
/// dirs — run one session each CONCURRENTLY; stats, memory, trace and
/// events never cross. Observable isolation: each engine's event bus
/// carries only its own session's traffic (alpha never rides engine B's
/// bus and vice versa) and each transcript carries only its own
/// conversation.
#[tokio::test]
async fn two_engines_isolated() {
    let root_a = tempfile::tempdir().unwrap();
    let cfg_a = tempfile::tempdir().unwrap();
    let root_b = tempfile::tempdir().unwrap();
    let cfg_b = tempfile::tempdir().unwrap();

    let llm_a = RecordingLlm::new(vec![g5_text_turn(&["alpha"])]);
    let llm_b = RecordingLlm::new(vec![g5_text_turn(&["beta"])]);
    let engine_a =
        Engine::builder(opts(Workspace::new(root_a.path()), Some(cfg_a.path().to_path_buf())))
            .provider(llm_a)
            .start()
            .await
            .expect("engine a starts");
    let engine_b =
        Engine::builder(opts(Workspace::new(root_b.path()), Some(cfg_b.path().to_path_buf())))
            .provider(llm_b)
            .start()
            .await
            .expect("engine b starts");
    let session_a = engine_a.spawn(SpawnOptions::default()).await.expect("spawn a");
    let session_b = engine_b.spawn(SpawnOptions::default()).await.expect("spawn b");

    // Subscribe BEFORE the concurrent turns, then run both together.
    let mut events_a = session_a.events();
    let mut events_b = session_b.events();
    let (out_a, out_b) = tokio::join!(
        session_a.run_to_completion("prompt-alpha"),
        session_b.run_to_completion("prompt-beta"),
    );
    assert_eq!(out_a.expect("a completes").final_text, "alpha");
    assert_eq!(out_b.expect("b completes").final_text, "beta");

    // Each engine's bus carries ONLY its own session's traffic.
    let drain = |rx: &mut mnemo::headless::EventReceiver<SessionEvent>| {
        let mut text = String::new();
        while let Ok(Some(event)) = rx.try_recv() {
            if let SerializableAgentEvent::TextDelta { text: delta } = event.event {
                text.push_str(&delta);
            }
        }
        text
    };
    let text_a = drain(&mut events_a);
    let text_b = drain(&mut events_b);
    assert_eq!(text_a, "alpha", "engine A's bus saw only alpha");
    assert_eq!(text_b, "beta", "engine B's bus saw only beta");

    // The transcripts stay per-engine — beta's prompt never rides A's
    // history and vice versa.
    let hist_a = session_a.history().await.expect("a history");
    let hist_b = session_b.history().await.expect("b history");
    let a_json = serde_json::to_string(&hist_a.messages).unwrap();
    let b_json = serde_json::to_string(&hist_b.messages).unwrap();
    assert!(a_json.contains("prompt-alpha"), "a carries its own prompt");
    assert!(!a_json.contains("prompt-beta"), "b's prompt never crosses");
    assert!(b_json.contains("prompt-beta"), "b carries its own prompt");
    assert!(!b_json.contains("prompt-alpha"), "a's prompt never crosses");

    engine_a.shutdown().await.expect("engine a shuts down");
    engine_b.shutdown().await.expect("engine b shuts down");
}

/// Review round-1 F5: an Approve naming an unknown (or already
/// resolved) request id errors `unknown_request` on a LIVE session —
/// never `session_closed` (a caller must not read a stale id as the
/// session ending).
#[tokio::test]
async fn unknown_approval_errors_unknown_request_not_closed() {
    let root = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let llm = RecordingLlm::new(vec![]);
    let engine = Engine::builder(opts(Workspace::new(root.path()), Some(cfg.path().to_path_buf())))
        .provider(llm)
        .start()
        .await
        .expect("engine starts");
    let session = engine.spawn(SpawnOptions::default()).await.expect("spawn");
    let err = session
        .send(SessionCommand::Approve {
            request_id: "no-such-request".into(),
            decision: Approval::Approve,
        })
        .await
        .expect_err("the unknown id errors");
    assert_eq!(err.code(), "unknown_request");
    engine.shutdown().await.expect("engine shuts down");
}
// --- 3b.1: D1 (the EngineOptions consumption fixture) ---

/// 3b.1 (D1): MemoryBackend::Disabled + index_codegraph: false consume
/// cleanly — the engine starts, a session runs a turn, and NO state-dir
/// artifacts appear (no memory.db, no codegraph.db): no store, no
/// graph, no watcher.
#[tokio::test]
async fn memory_disabled_consumes_no_store() {
    let root = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let llm = RecordingLlm::new(vec![g5_text_turn(&["ok"])]);
    let mut options = opts(Workspace::new(root.path()), Some(cfg.path().to_path_buf()));
    options.memory = MemoryBackend::Disabled;
    options.index_codegraph = false;
    let engine = Engine::builder(options)
        .provider(llm)
        .start()
        .await
        .expect("engine starts");
    let session = engine.spawn(SpawnOptions::default()).await.expect("spawn");
    let outcome = session.run_to_completion("hi").await.expect("turn");
    assert_eq!(outcome.final_text, "ok");
    let state = root.path().join(".coding");
    assert!(
        !state.join("memory.db").exists(),
        "a Disabled engine writes no memory store"
    );
    assert!(
        !state.join("codegraph.db").exists(),
        "index_codegraph: false writes no graph"
    );
    engine.shutdown().await.expect("engine shuts down");
}
// --- 3b.2: D2 (the §5.6 data-API fixtures) ---

/// §5.6 row 17 (canonical): `memory: false` (MemoryBackend::Disabled)
/// means no store — `Engine::memory()` errors code `memory_disabled` —
/// and `index_codegraph: false` means `Engine::codegraph()` reports the
/// unavailable shape (no graph was opened, no watcher started).
#[tokio::test]
async fn memory_disabled_errors() {
    let root = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let llm = RecordingLlm::new(vec![g5_text_turn(&["ok"])]);
    let mut options = opts(Workspace::new(root.path()), Some(cfg.path().to_path_buf()));
    options.memory = MemoryBackend::Disabled;
    options.index_codegraph = false;
    let engine = Engine::builder(options)
        .provider(llm)
        .start()
        .await
        .expect("engine starts");
    let err = match engine.memory() {
        Err(err) => err,
        Ok(_) => panic!("a Disabled engine errors"),
    };
    assert_eq!(err.code(), "memory_disabled");
    let status = engine.codegraph().status().await.expect("status reads");
    assert!(
        !status.available,
        "index_codegraph: false reports unavailable"
    );
    engine.shutdown().await.expect("engine shuts down");
}

/// §5.6 row 17: a FREE agent can call `memory_search` and the code-graph
/// tool — the 2c free gate already allows them; this pins it end-to-end
/// through the engine (the scripted mock drives both dispatches, then
/// the text final). The post-turn transcript proves both tools really
/// executed (their tool_use entries rode the conversation).
#[tokio::test]
async fn free_agent_uses_memory_and_codegraph_tools() {
    let root = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    // The store + graph DBs need their parent dir to open cleanly.
    std::fs::create_dir_all(root.path().join(".coding")).unwrap();
    let llm = RecordingLlm::new(vec![
        g5_tool_turn("memory_search", r#"{"query": "how does recall rank"}"#),
        g5_tool_turn("graph_search", r#"{"query": "Engine"}"#),
        g5_text_turn(&["done"]),
    ]);
    let engine = Engine::builder(opts(
        Workspace::new(root.path()),
        Some(cfg.path().to_path_buf()),
    ))
    .provider(llm)
    .start()
    .await
    .expect("engine starts");
    let session = engine.spawn(SpawnOptions::default()).await.expect("spawn");
    let outcome = session
        .run_to_completion("search memory and the graph")
        .await
        .expect("turn completes");
    assert_eq!(outcome.final_text, "done");
    // Both dispatches rode the conversation — the transcript carries
    // their tool_use entries.
    let transcript = session.history().await.expect("history export");
    let json = serde_json::to_string(&transcript).unwrap_or_default();
    assert!(
        json.contains("memory_search"),
        "the free agent dispatched memory_search"
    );
    assert!(
        json.contains("graph_search"),
        "the free agent dispatched graph_search"
    );
    engine.shutdown().await.expect("engine shuts down");
}
/// §5.6 row 17 (canonical): `Engine::start` emits `EmbedderStatus`
/// (the new variant — the initial snapshot; each deferred-load
/// transition forwards too) and the graph's startup-pass stream on
/// `host_events()`: wire_brain's spawned pass forwards IndexProgress,
/// whose terminal `Done` is the "ready" signal (§5.1's sketch named it
/// CodegraphReady — job 1 landed the stream as IndexProgress; see
/// notifier.rs's doc note). The startup events ride QUEUED tasks, so a
/// subscriber connecting right after `start` (current-thread runtime —
/// the queue polls at this test's first await) still sees them.
#[tokio::test]
async fn engine_start_emits_index_host_events() {
    let root = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(".coding")).unwrap();
    // Some source so the pass has something to index.
    std::fs::write(root.path().join("a.rs"), "fn a() {}\n").unwrap();
    std::fs::write(root.path().join("b.rs"), "struct B;\n").unwrap();
    let llm = RecordingLlm::new(vec![g5_text_turn(&["ok"])]);
    // The shared opts() helper keeps the graph OFF (hermetic engine
    // tests — no watcher, no pass noise); THIS fixture opts in: the
    // startup pass + its events are the contract under test.
    let mut options = opts(Workspace::new(root.path()), Some(cfg.path().to_path_buf()));
    options.index_codegraph = true;
    let engine = Engine::builder(options)
        .provider(llm)
        .start()
        .await
        .expect("engine starts");
    let mut events = engine.host_events();
    let mut saw_status = false;
    for _ in 0..16 {
        if saw_status {
            break;
        }
        match tokio::time::timeout(std::time::Duration::from_secs(5), events.recv()).await {
            Ok(Ok(HostEvent::EmbedderStatus(_))) => saw_status = true,
            Ok(Ok(_)) => {}
            Ok(Err(_)) | Err(_) => break,
        }
    }
    assert!(saw_status, "EmbedderStatus rides host_events()");
    // The graph's startup pass is wire_brain's spawn — its IndexProgress
    // stream flows to whoever subscribes DURING the pass; on a tiny tree
    // the pass completes inside `start()` before this test could
    // subscribe (the bus is lossy by design — no subscribers yet → the
    // event drops, BusNotifier's documented contract). The pass's
    // RESULT is deterministically observable: the graph's DB carries the
    // run (row 17's "ready" for a subscriber that connected after
    // start).
    let mut indexed = None;
    for _ in 0..100 {
        let status = engine.codegraph().status().await.expect("status reads");
        assert!(status.available, "the graph opened (index_codegraph default)");
        if status.last_indexed_at.is_some() {
            indexed = status.last_indexed_at;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        indexed.is_some(),
        "the startup pass indexed the tree before ready"
    );
    engine.shutdown().await.expect("engine shuts down");
}
// --- 3b.5: D5 — the stats pull + rates (§5.7 rows 17/18) ---

/// A scripted provider whose `model()` name switches when a response
/// carrying [`MODEL_SWITCH_MARKER`] is popped — the ModelChanged
/// fixture (§5.7's serve seam announces each change; the scripted
/// turns serve the seed name and stay silent, the compact's summarize
/// reply carries the marker, and the next turn's serve announces
/// EXACTLY one switch).
struct StepModelLlm {
    responses: std::sync::Mutex<VecDeque<Vec<LlmEvent>>>,
    step: std::sync::atomic::AtomicUsize,
    caps: Capabilities,
}

const MODEL_STEPS: [&str; 2] = ["m-seed", "m-switched"];

/// The text marker that steps the mock's model name — one
/// deterministic model switch at a scripted point (the compact's
/// summarize reply carries it).
const MODEL_SWITCH_MARKER: &str = "SWITCH-MODEL";

impl StepModelLlm {
    fn new(responses: Vec<Vec<LlmEvent>>) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            responses: std::sync::Mutex::new(responses.into()),
            step: std::sync::atomic::AtomicUsize::new(0),
            caps: Capabilities::openai(),
        })
    }

    fn name(&self) -> &'static str {
        MODEL_STEPS[self
            .step
            .load(std::sync::atomic::Ordering::SeqCst)
            .min(MODEL_STEPS.len() - 1)]
    }
}

#[async_trait]
impl LlmClient for StepModelLlm {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
    fn kind(&self) -> ProviderKind {
        ProviderKind::OpenAI
    }
    fn model(&self) -> &str {
        self.name()
    }
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _tool_choice: Option<ToolChoice>,
    ) -> mnemo::error::Result<BoxStream<'_, LlmEvent>> {
        let next = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_default();
        if next.iter().any(|ev| {
            matches!(&ev, LlmEvent::TextDelta { text } if text.contains(MODEL_SWITCH_MARKER))
        }) {
            self.step
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        Ok(Box::pin(futures::stream::iter(next)))
    }
}

/// One text turn carrying a usage report — the token/timing counts the
/// Usage event (and its rates) carry.
fn g5_usage(text: &str) -> Vec<LlmEvent> {
    vec![
        LlmEvent::TextDelta { text: text.into() },
        LlmEvent::Usage {
            prompt_tokens: 1000,
            completion_tokens: 40,
            reasoning_tokens: 10,
            cached_tokens: 800,
            ttft_ms: Some(400),
            generation_ms: Some(200),
        },
        LlmEvent::Finish {
            reason: FinishReason::Stop,
        },
    ]
}

/// §5.7 row 18 (the rates half): a Usage event rides `events()` with
/// its derived rates — the pump's computation, correct to the
/// formulas the variant documents (input = prompt/ttft, output =
/// completion/generation) plus the cache-hit ratio.
#[tokio::test]
async fn usage_rates_formula() {
    let root = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(".coding")).unwrap();
    let llm = RecordingLlm::new(vec![g5_usage("ok")]);
    let engine = Engine::builder(opts(
        Workspace::new(root.path()),
        Some(cfg.path().to_path_buf()),
    ))
    .provider(llm)
    .start()
    .await
    .expect("engine starts");
    let session = engine.spawn(SpawnOptions::default()).await.expect("spawn");
    let mut rx = session.events();
    let outcome = session
        .run_to_completion("hi")
        .await
        .expect("the turn completes");
    assert_eq!(outcome.final_text, "ok");
    // Drain the bus — the Usage event (with its rates) is in there.
    let mut usage = None;
    while let Ok(Some(ev)) = rx.try_recv() {
        if matches!(ev.event, SerializableAgentEvent::Usage { .. }) {
            usage = Some(ev);
        }
    }
    let ev = usage.expect("a Usage event rode events()");
    let rates = ev.rates.expect("the Usage event carries its rates");
    assert_eq!(rates.input_tok_per_s, Some(2500.0), "1000 tok / 0.4 s");
    assert_eq!(rates.output_tok_per_s, Some(200.0), "40 tok / 0.2 s");
    assert_eq!(rates.cache_hit_ratio, 0.8, "800 / 1000 cached");
    engine.shutdown().await.expect("shutdown");
}

/// §5.7 row 18 (canonical): on a scripted provider with usage + one
/// model switch + one output-compaction saving, `events()` carries
/// Usage (with UsageRates correct to the formula), ModelChanged and
/// Savings; `AgentSession::stats()` totals equal the sum of those
/// events, and `Engine::stats().savings_stats.saved_tokens_total`
/// includes the saving. The compact drives the survival checkpoint's
/// savings lever (`compaction_survival` defaults on). The Spend half of
/// the row is PLAN-GATED and pins SILENT here: the headless registry
/// ships no plan tools (create_plan is app-layer), so every scripted
/// turn is planless and clears the attribution — the Spend fan itself
/// is covered by savings_and_spend_emitted_where_persisted (3b.4);
/// wiring the plan tools into the headless factory (a later job) will
/// let it script end-to-end.
#[tokio::test]
async fn stats_totals_match_events() {
    let root = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(".coding")).unwrap();
    // Four plain turns (the second carries the usage report) grow the
    // transcript past the checkpoint's keep-recent window (6 messages)
    // so the compact actually drops a region — that checkpoint lever
    // is the compact's Savings row. The summarize reply carries the
    // model-switch marker; the final turn's serve announces EXACTLY
    // one ModelChanged.
    let llm = StepModelLlm::new(vec![
        g5_text_turn(&["answer"]),
        g5_usage("second"),
        g5_text_turn(&["third"]),
        g5_text_turn(&["fourth"]),
        g5_text_turn(&["summary SWITCH-MODEL"]),
        g5_text_turn(&["done"]),
    ]);
    let engine = Engine::builder(opts(
        Workspace::new(root.path()),
        Some(cfg.path().to_path_buf()),
    ))
    .provider(llm)
    .start()
    .await
    .expect("engine starts");
    let session = engine.spawn(SpawnOptions::default()).await.expect("spawn");
    let mut rx = session.events();
    let outcome = session
        .run_to_completion("plan then answer")
        .await
        .expect("turn 1 completes");
    assert_eq!(outcome.final_text, "answer");

    // Turns 2-4 — the transcript must exceed the checkpoint's
    // keep-recent window (6 messages) so the compact drops a region
    // (the empty-region path records no Savings row). Turn 2 carries
    // the usage report.
    let outcome = session
        .run_to_completion("second")
        .await
        .expect("turn 2 completes");
    assert_eq!(outcome.final_text, "second");
    let outcome = session
        .run_to_completion("third")
        .await
        .expect("turn 3 completes");
    assert_eq!(outcome.final_text, "third");
    let outcome = session
        .run_to_completion("fourth")
        .await
        .expect("turn 4 completes");
    assert_eq!(outcome.final_text, "fourth");

    // The output-compaction saving: Compact drives the checkpoint
    // lever — wait for its Savings event (the command runs async in
    // the task; the poll bounds the wait hermetically). The poll CONSUMES
    // the bus, so it collects too — the drain later only sees what
    // arrived after it.
    let mut collected: Vec<SessionEvent> = Vec::new();
    session
        .send(SessionCommand::Compact)
        .await
        .expect("compact sends");
    let mut saw_savings = false;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !saw_savings && std::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await {
            Ok(Ok(ev)) => {
                if matches!(ev.event, SerializableAgentEvent::Savings { .. }) {
                    saw_savings = true;
                }
                collected.push(ev);
            }
            Ok(Err(_)) => break,
            Err(_) => {}
        }
    }
    assert!(saw_savings, "the compact's Savings event rode events()");

    let outcome = session
        .run_to_completion("done")
        .await
        .expect("turn 2 completes");
    assert_eq!(outcome.final_text, "done");

    // Drain the bus (one session, no spawned children — every event is
    // this session's) + settle laps for any pump lag.
    for _ in 0..20 {
        while let Ok(Some(ev)) = rx.try_recv() {
            collected.push(ev);
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    // Derive the expected totals from the collected events (row 18:
    // stats() equals the SUM of what events() carried).
    let mut usage_events = Vec::new();
    let mut model_changes = Vec::new();
    let mut savings = Vec::new();
    let mut spend_count = 0u64;
    for ev in &collected {
        match &ev.event {
            SerializableAgentEvent::Usage { .. } => usage_events.push(ev),
            SerializableAgentEvent::ModelChanged {
                model,
                reasoning_effort,
                ..
            } => model_changes.push((model.clone(), reasoning_effort.clone())),
            SerializableAgentEvent::Savings {
                lever, tokens_saved, ..
            } => savings.push((lever.clone(), *tokens_saved)),
            SerializableAgentEvent::Spend { .. } => spend_count += 1,
            _ => {}
        }
    }
    assert!(!usage_events.is_empty(), "the scripted usage rode events()");
    assert!(!savings.is_empty(), "the compact's savings rode events()");
    // The Spend ledger is PLAN-GATED and pins silent here (row 18's
    // documented deviation): the headless registry ships no plan tools
    // (create_plan is app-layer), so every scripted turn is planless
    // and clears the attribution. The Spend fan itself is covered by
    // savings_and_spend_emitted_where_persisted (3b.4).
    assert_eq!(
        spend_count, 0,
        "a planless session never bills — the Spend fan needs a live plan"
    );
    assert_eq!(model_changes.len(), 1, "exactly one model switch announced");

    // Rates on every Usage event — correct to the formula.
    for ev in &usage_events {
        let SerializableAgentEvent::Usage {
            prompt_tokens,
            completion_tokens,
            cached_tokens,
            ttft_ms,
            generation_ms,
            ..
        } = &ev.event
        else {
            unreachable!("matched Usage above")
        };
        assert_eq!(
            ev.rates,
            Some(mnemo::headless::UsageRates::from_usage(
                *prompt_tokens,
                *completion_tokens,
                *cached_tokens,
                *ttft_ms,
                *generation_ms
            )),
            "the rates follow the documented formulas"
        );
    }

    let mut prompt_sum = 0u64;
    let mut completion_sum = 0u64;
    let mut reasoning_sum = 0u64;
    let mut cached_sum = 0u64;
    for ev in &usage_events {
        if let SerializableAgentEvent::Usage {
            prompt_tokens,
            completion_tokens,
            reasoning_tokens,
            cached_tokens,
            ..
        } = &ev.event
        {
            prompt_sum += *prompt_tokens as u64;
            completion_sum += *completion_tokens as u64;
            reasoning_sum += *reasoning_tokens as u64;
            cached_sum += *cached_tokens as u64;
        }
    }
    let stats = session.stats();
    assert_eq!(stats.requests, usage_events.len() as u64);
    assert_eq!(stats.prompt_tokens, prompt_sum);
    assert_eq!(stats.completion_tokens, completion_sum);
    assert_eq!(stats.reasoning_tokens, reasoning_sum);
    assert_eq!(stats.cached_tokens, cached_sum);
    assert_eq!(stats.model_switches, 1);
    assert_eq!(stats.current_model, Some(model_changes[0].0.clone()));
    assert_eq!(stats.current_reasoning_effort, model_changes[0].1);
    let mut expected_saved = std::collections::BTreeMap::new();
    for (lever, saved) in &savings {
        *expected_saved.entry(lever.clone()).or_insert(0i64) += *saved;
    }
    assert_eq!(
        stats.saved_tokens_by_kind, expected_saved,
        "saved tokens tally per lever"
    );

    // The durable session half: stored_stats() reads the loop's
    // current session rows from the store (the same read the IPC's
    // get_session_stats performs; poll-bounded — the row writes are
    // fire-and-forget).
    let mut stored_ok = false;
    for _ in 0..100 {
        match session.stored_stats().await {
            Ok(stored) if stored.prompt_tokens as u64 >= prompt_sum => {
                stored_ok = true;
                break;
            }
            Ok(_) => {}
            Err(e) => panic!("stored_stats() errored: {e}"),
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(stored_ok, "stored_stats() reads the turn's durable rows");

    // The engine pull: the durable savings ledger includes the compact's
    // saving (the row write is fire-and-forget — poll bounded).
    let saved_total: i64 = savings.iter().map(|(_, s)| *s).sum();
    let mut included = false;
    for _ in 0..100 {
        let engine_stats = engine.stats().await.expect("the engine stats read");
        if engine_stats.savings_stats.saved_tokens_total >= saved_total {
            included = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        included,
        "Engine::stats().savings_stats.saved_tokens_total includes the saving"
    );
    engine.shutdown().await.expect("engine shuts down");
}
// --- 3c.2: G2 host tools (§7 row 31) ---

/// §7 row 31 (G2): a host tool is called by the scripted model, the host
/// answers via `HostToolResult`, and the result reaches the model (the
/// next request carries it as the tool message; `snapshot()` reports the
/// pending call mid-flight — §5.9.5); an unanswered call (a 150 ms
/// per-tool timeout) resolves to the timeout error result and the turn
/// still finishes — an absent host can never hang the agent (R4).
#[tokio::test]
async fn host_tool_round_trip_and_timeout_row_31() {
    let root = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(".coding")).unwrap();
    // Leg 1: the model calls the host tool, then ends the turn on the
    // answer. Leg 2: another call nobody answers (the 150 ms spec
    // timeout), then the closing reply.
    let llm = RecordingLlm::new(vec![
        g5_tool_turn("get_weather", r#"{"city": "Oslo"}"#),
        g5_text_turn(&["the weather is sunny"]),
        g5_tool_turn("get_weather", r#"{"city": "Oslo"}"#),
        g5_text_turn(&["the call timed out"]),
    ]);
    let engine = Engine::builder(opts(
        Workspace::new(root.path()),
        Some(cfg.path().to_path_buf()),
    ))
    .provider(llm.clone())
    .start()
    .await
    .expect("engine starts");

    // Leg 1 — answered: the default 120 s timeout applies, but the host
    // answers immediately.
    let session = engine
        .spawn(SpawnOptions {
            host_tools: vec![HostToolSpec {
                name: "get_weather".into(),
                description: "Get the current weather for a city".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {"city": {"type": "string"}},
                    "required": ["city"]
                }),
                safety: SafetyLevel::AutoRun,
                timeout_ms: None,
            }],
            ..SpawnOptions::default()
        })
        .await
        .expect("spawn (leg 1)");
    let mut rx = session.events();
    // Drive the turn + the answer loop concurrently — both borrow the
    // session immutably (`run_to_completion` + `send` are `&self`), so
    // join them; the answer loop consumes its own subscription copy.
    let answer = async {
        loop {
            let ev = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
                .await
                .expect("events keep flowing")
                .expect("event arrives");
            if let SerializableAgentEvent::HostToolCall { call_id, name, .. } = ev.event {
                assert_eq!(name, "get_weather");
                // The pump stashed the call — the snapshot reports it
                // mid-flight (§5.9.5: engine-local, always accurate).
                let pending = session.snapshot().await.expect("snapshot").pending;
                assert!(
                    matches!(pending, Some(PendingInteraction::HostToolCall { .. })),
                    "the snapshot reports the pending host-tool call: {pending:?}"
                );
                session
                    .send(SessionCommand::HostToolResult {
                        call_id,
                        result: Some(ToolResult::success("sunny, 21°C".to_string())),
                        error: None,
                    })
                    .await
                    .expect("the answer resolves the pending call");
                return;
            }
        }
    };
    let (outcome, ()) = tokio::join!(
        session.run_to_completion("check the weather in Oslo"),
        answer
    );
    let outcome = outcome.expect("leg-1 turn completes");
    assert_eq!(outcome.final_text, "the weather is sunny");
    // The result reached the model: the second request carries the tool
    // message with the host's answer.
    let reqs = llm.requests();
    assert!(reqs.len() >= 2, "the tool call produced a second request");
    let tool_msg = reqs[1]
        .iter()
        .find(|m| m.role == Role::Tool && m.tool_call_id.as_deref() == Some("call_1"))
        .expect("request 2 carries the host tool's result")
        .content
        .as_text();
    assert!(
        tool_msg.contains("sunny, 21°C"),
        "the model sees the host's answer: {tool_msg}"
    );

    // Leg 2 — unanswered: a 150 ms per-tool timeout resolves the call as
    // an error result; nobody answers, and the turn still finishes.
    let session = engine
        .spawn(SpawnOptions {
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
            ..SpawnOptions::default()
        })
        .await
        .expect("spawn (leg 2)");
    let outcome = session
        .run_to_completion("check the weather again — nobody will answer")
        .await
        .expect("leg-2 turn completes despite the unanswered call");
    assert_eq!(outcome.final_text, "the call timed out");
    // The timeout error reached the model: leg 2's second request names
    // the per-tool timeout (R4 — the turn continues).
    let reqs = llm.requests();
    assert!(reqs.len() >= 4, "leg 2 also produced a second request");
    let tool_msg = reqs[3]
        .iter()
        .find(|m| m.role == Role::Tool && m.tool_call_id.as_deref() == Some("call_1"))
        .expect("leg-2 request 2 carries the timeout error")
        .content
        .as_text();
    assert!(
        tool_msg.contains("timed out after 150 ms"),
        "the model sees the abandoned-call error: {tool_msg}"
    );
}/// Row 32 (G6 tier 3, §5.9.6): the external policy's three legs, one
/// engine, a fresh session each. (a) A selected ToolGate answered with
/// Deny vetoes the call BEFORE the host-tool branch — the model sees
/// the deny message as the call's result (no `HostToolCall` ever
/// fires), and the snapshot reports the pending hook mid-flight. (b) A
/// selected TurnEnd answered `Continue { note }` runs another turn and
/// the HOST's note arrives as its user message verbatim (the runtime's
/// note-honoring arm); answering `Stop` at the next turn end parks. (c)
/// `on_timeout: Allow` + a short timeout with nobody answering the
/// gate: the call PROCEEDS (the host-tool branch fires; R4's per-tool
/// timeout resolves it) and the turn still finishes.
#[tokio::test]
async fn external_policy_gate_and_turn_end_row_32() {
    let root = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(".coding")).unwrap();
    let llm = RecordingLlm::new(vec![
        // Leg (a): the model calls the gated host tool (denied), then
        // ends the turn on the deny message.
        g5_tool_turn("get_weather", r#"{"city": "Oslo"}"#),
        g5_text_turn(&["it was denied"]),
        // Leg (b): turn one ends plain; the TurnEnd answer continues
        // with the host's note; the second turn ends plain; the next
        // TurnEnd answer stops.
        g5_text_turn(&["turn one"]),
        g5_text_turn(&["turn two done"]),
        // Leg (c): the gate times out unanswered (Allow → proceed),
        // the host tool then times out unanswered (150 ms), the model
        // ends.
        g5_tool_turn("get_weather", r#"{"city": "Oslo"}"#),
        g5_text_turn(&["it proceeded"]),
    ]);
    let engine = Engine::builder(opts(
        Workspace::new(root.path()),
        Some(cfg.path().to_path_buf()),
    ))
    .provider(llm.clone())
    .start()
    .await
    .expect("engine starts");

    // Leg (a) — the gate vetoes before the host-tool branch: the deny
    // message reaches the model as the call's result.
    let session = engine
        .spawn(SpawnOptions {
            host_tools: vec![HostToolSpec {
                name: "get_weather".into(),
                description: "Get the current weather for a city".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {"city": {"type": "string"}},
                    "required": ["city"]
                }),
                safety: SafetyLevel::AutoRun,
                timeout_ms: None,
            }],
            external_policy: Some(ExternalPolicySpec {
                hooks: vec![HookKind::ToolGate],
                timeout_ms: 5_000,
                on_timeout: TimeoutAction::Allow,
            }),
            ..SpawnOptions::default()
        })
        .await
        .expect("spawn (leg a)");
    let mut rx = session.events();
    let answer = async {
        loop {
            let ev = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
                .await
                .expect("events keep flowing")
                .expect("event arrives");
            if let SerializableAgentEvent::PolicyHook {
                hook_id,
                hook_kind,
                payload,
            } = ev.event
            {
                assert_eq!(hook_id, "hook_1");
                assert_eq!(hook_kind, HookKind::ToolGate);
                assert_eq!(
                    payload,
                    serde_json::json!({"tool": "get_weather", "args": {"city": "Oslo"}})
                );
                // The pump stashed the responder — the snapshot reports
                // the pending hook mid-flight (§5.9.5: engine-local,
                // always accurate).
                let pending = session.snapshot().await.expect("snapshot").pending;
                assert!(
                    matches!(pending, Some(PendingInteraction::PolicyHook { .. })),
                    "the snapshot reports the pending policy hook: {pending:?}"
                );
                session
                    .send(SessionCommand::PolicyAnswer {
                        hook_id,
                        answer: PolicyAnswer::Gate {
                            verdict: GateVerdict::Deny {
                                message: "denied by the external policy".into(),
                            },
                        },
                    })
                    .await
                    .expect("the answer resolves the pending hook");
                return;
            }
        }
    };
    let (outcome, ()) = tokio::join!(
        session.run_to_completion("check the weather in Oslo"),
        answer
    );
    let outcome = outcome.expect("leg-a turn completes");
    assert_eq!(outcome.final_text, "it was denied");
    let reqs = llm.requests();
    assert!(
        reqs.len() >= 2,
        "the denied call still produced a second request"
    );
    let tool_msg = reqs[1]
        .iter()
        .find(|m| m.role == Role::Tool && m.tool_call_id.as_deref() == Some("call_1"))
        .expect("request 2 carries the denied call's error result")
        .content
        .as_text();
    assert!(
        tool_msg.contains("denied by the external policy"),
        "the model sees the deny message: {tool_msg}"
    );

    // Leg (b) — the TurnEnd answer continues with the host's note; the
    // second TurnEnd answer stops.
    let session = engine
        .spawn(SpawnOptions {
            external_policy: Some(ExternalPolicySpec {
                hooks: vec![HookKind::TurnEnd],
                timeout_ms: 5_000,
                on_timeout: TimeoutAction::Allow,
            }),
            ..SpawnOptions::default()
        })
        .await
        .expect("spawn (leg b)");
    let mut rx = session.events();
    let answer = async {
        let first_id = loop {
            let ev = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
                .await
                .expect("events keep flowing")
                .expect("event arrives");
            if let SerializableAgentEvent::PolicyHook {
                hook_id,
                hook_kind,
                payload,
            } = ev.event
            {
                assert_eq!(hook_id, "hook_1");
                assert_eq!(hook_kind, HookKind::TurnEnd);
                assert_eq!(
                    payload,
                    serde_json::json!({
                        "success": true,
                        "unattended": false,
                        "stop_reason": null,
                    })
                );
                break hook_id;
            }
        };
        session
            .send(SessionCommand::PolicyAnswer {
                hook_id: first_id,
                answer: PolicyAnswer::Continuation {
                    continuation: Continuation::Continue {
                        note: Some("the host says: continue and finish".into()),
                    },
                },
            })
            .await
            .expect("the Continue answer resolves the pending hook");
        let second_id = loop {
            let ev = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
                .await
                .expect("events keep flowing")
                .expect("event arrives");
            if let SerializableAgentEvent::PolicyHook {
                hook_id, hook_kind, ..
            } = ev.event
            {
                assert_eq!(hook_id, "hook_2");
                assert_eq!(hook_kind, HookKind::TurnEnd);
                break hook_id;
            }
        };
        session
            .send(SessionCommand::PolicyAnswer {
                hook_id: second_id,
                answer: PolicyAnswer::Continuation {
                    continuation: Continuation::Stop,
                },
            })
            .await
            .expect("the Stop answer resolves the pending hook");
    };
    let (outcome, ()) = tokio::join!(session.run_to_completion("start the task"), answer);
    let outcome = outcome.expect("leg-b session completes");
    assert_eq!(outcome.final_text, "turn two done");
    // The host's note arrived as the second turn's user message —
    // verbatim, not the kernel's harness note.
    let reqs = llm.requests();
    let note_msg = reqs[3]
        .iter()
        .rev()
        .find(|m| m.role == Role::User)
        .expect("the second turn's request carries the host's note")
        .content
        .as_text();
    assert_eq!(
        note_msg, "the host says: continue and finish",
        "the host's note is the next turn's user message verbatim"
    );

    // Leg (c) — on_timeout: Allow, nobody answers the gate: the call
    // proceeds (the host-tool branch fires; R4's 150 ms per-tool
    // timeout resolves it), the turn still finishes.
    let session = engine
        .spawn(SpawnOptions {
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
            external_policy: Some(ExternalPolicySpec {
                hooks: vec![HookKind::ToolGate],
                timeout_ms: 150,
                on_timeout: TimeoutAction::Allow,
            }),
            ..SpawnOptions::default()
        })
        .await
        .expect("spawn (leg c)");
    let mut rx = session.events();
    let watch = async {
        // Nobody answers the gate — record that BOTH the consult and
        // the (proceeded) host-tool call fired.
        let mut saw_hook = false;
        let mut saw_host_call = false;
        loop {
            let ev = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
                .await
                .expect("events keep flowing")
                .expect("event arrives");
            match ev.event {
                SerializableAgentEvent::PolicyHook { hook_kind, .. } => {
                    assert_eq!(hook_kind, HookKind::ToolGate);
                    saw_hook = true;
                }
                SerializableAgentEvent::HostToolCall { name, .. } => {
                    assert_eq!(name, "get_weather");
                    saw_host_call = true;
                }
                _ => {}
            }
            if saw_hook && saw_host_call {
                return;
            }
        }
    };
    let (outcome, ()) = tokio::join!(
        session.run_to_completion("check the weather — nobody will answer"),
        watch
    );
    let outcome = outcome.expect("leg-c turn completes despite the unanswered gate");
    assert_eq!(outcome.final_text, "it proceeded");
    let reqs = llm.requests();
    let tool_msg = reqs[5]
        .iter()
        .find(|m| m.role == Role::Tool && m.tool_call_id.as_deref() == Some("call_1"))
        .expect("leg-c request 2 carries the per-tool timeout error")
        .content
        .as_text();
    assert!(
        tool_msg.contains("timed out after 150 ms"),
        "the call proceeded to the host-tool branch (R4 timeout): {tool_msg}"
    );
}