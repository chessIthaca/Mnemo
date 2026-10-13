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
use mnemo::headless::{PendingInteraction, SessionCommand, SpawnOptions, Transcript};
use mnemo::provider::{
    Capabilities, FinishReason, LlmClient, LlmEvent, Message, ProviderKind, ToolChoice,
    ToolSchema,
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
