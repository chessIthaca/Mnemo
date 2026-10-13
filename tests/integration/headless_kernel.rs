//! The kernel-level engine fixtures (job 3a.4): the workspace contract —
//! an engine starts on a plain NON-git directory (G3) and honors an
//! out-of-tree state dir (the row-27 engine form).

use std::path::PathBuf;

use mnemo::agent::policy::PermissionPolicy;
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
    PendingInteraction, SessionCommand, SessionEvent, SessionLimits, SpawnOptions, Transcript,
};
use mnemo::provider::{
    Capabilities, FinishReason, LimitKind, LlmClient, LlmEvent, Message, ProviderKind,
    ToolChoice, ToolSchema,
};
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
