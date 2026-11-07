use std::path::Path;
use std::time::{Duration, Instant};

use super::*;
use crate::mnemo2::team::{TeamRuntime, TeamRuntimeOptions, WorkerSessionSpec};
use mnemo::headless::{Engine, EngineOptions, SessionCommand, TraceApi};
use mnemo::provider::trace::{TraceActorKind, TraceIdentity, LlmRequestSummary};

/// A minimal worker identity for tests that spawn worker sessions
/// without a full WorkerRecord (the tool-profile tests reuse it).
pub(super) fn worker_identity(name: &str) -> TraceIdentity {
    TraceIdentity {
        actor_kind: TraceActorKind::Worker,
        agent_id: String::new(),
        session_id: String::new(),
        agent_name: name.to_string(),
        worker_id: Some("w-trace".to_string()),
        plan_id: Some("plan-trace".to_string()),
        plan_step: Some(1),
    }
}

fn run_git(root: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init_git(root: &Path) {
    run_git(root, &["init", "-b", "main"]);
    run_git(root, &["config", "user.name", "Mnemo2 Test"]);
    run_git(root, &["config", "user.email", "mnemo2@example.invalid"]);
    std::fs::write(root.join("README.md"), "seed\n").expect("seed file");
    std::fs::write(root.join(".gitignore"), ".worktrees/\n.coding/\n")
        .expect("ignore worktrees and state");
    run_git(root, &["add", "README.md", ".gitignore"]);
    run_git(root, &["commit", "-m", "seed"]);
}

/// A config whose only endpoint is unreachable (port 9), so the first
/// provider call fails fast and lands in the trace log with its
/// stamped identity — no scripted provider needed.
fn write_unreachable_config(config: &Path) {
    std::fs::create_dir_all(config).expect("config dir");
    std::fs::write(
        config.join("config.toml"),
        "[general]\ndefault_provider = \"test\"\ndefault_model = \"scripted-a\"\n",
    )
    .expect("config.toml");
    std::fs::write(
        config.join("endpoints.toml"),
        "[[endpoint]]\nname = \"test\"\nkind = \"local\"\nbase_url = \"http://127.0.0.1:9/v1/\"\nmodels = [\"scripted-a\"]\n",
    )
    .expect("endpoints.toml");
}

/// Wait until the trace log holds a terminal record, then return the
/// newest one: the first model call over the unreachable endpoint is
/// terminal the moment its connect fails.
async fn wait_for_terminal_record(
    trace: &TraceApi,
    budget_ms: u64,
) -> Option<LlmRequestSummary> {
    let deadline = Instant::now() + Duration::from_millis(budget_ms);
    while Instant::now() < deadline {
        if let Some(record) = trace.list().into_iter().rev().find(|s| s.is_complete) {
            return Some(record);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    None
}

#[tokio::test]
async fn lead_calls_carry_the_lead_identity() {
    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    write_unreachable_config(config.path());
    let lead = LeadRuntime::open(LeadRuntimeOptions {
        workspace_root: root.path().to_path_buf(),
        config_dir: Some(config.path().to_path_buf()),
        state_dir: state.path().to_path_buf(),
        model: None,
    })
    .await
    .expect("open the lead");

    // One prompt: the provider call fails fast (connection refused) but
    // its trace record carries the identity stamped at spawn.
    lead.send_prompt("hello".to_string(), Vec::new())
        .await
        .expect("send the prompt");
    let engine = lead.engine_handle().await.expect("the engine");
    let trace = engine.trace();
    let record =
        wait_for_terminal_record(&trace, 15_000).await.expect("a terminal lead record");

    assert_eq!(trace.run_id(), record.run_id, "the record names its run");
    let attribution = record.attribution.expect("the lead call is attributed");
    assert_eq!(attribution.actor_kind, TraceActorKind::Lead);
    assert_eq!(attribution.agent_name, "Lead");
    assert_eq!(attribution.worker_id, None);
    assert_eq!(attribution.plan_id, None);
    assert_eq!(attribution.plan_step, None);
    assert!(
        !attribution.agent_id.is_empty(),
        "the spawn path filled the agent id"
    );
    assert!(
        !attribution.session_id.is_empty(),
        "the spawn path filled the session id"
    );
}

#[tokio::test]
async fn worker_calls_carry_worker_plan_and_step() {
    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    write_unreachable_config(config.path());
    let engine = Engine::start(EngineOptions {
        workspace: mnemo::workspace::Workspace::new(root.path()),
        config_dir: Some(config.path().to_path_buf()),
        permissions: mnemo::agent::policy::PermissionPolicy::OverridePermissions,
        memory: mnemo::headless::MemoryBackend::Disabled,
        index_codegraph: false,
        require_git: true,
        auto_install_embedder: false,
    })
    .await
    .expect("start the engine");
    let engine = std::sync::Arc::new(engine);
    let team = TeamRuntime::open(TeamRuntimeOptions {
        engine: engine.clone(),
        workspace_root: root.path().to_path_buf(),
        state_dir: state.path().to_path_buf(),
        config_dir: state.path().join("config"),
        classifier: None,
        routing_log_path: Some(state.path().join("dispatch_routing.jsonl")),
        slots: Some(2),
        models: Vec::new(),
        pricing: Vec::new(),
    })
    .await
    .expect("open the team runtime");

    let session = team
        .spawn_worker_session(WorkerSessionSpec {
            model: None,
            reasoning_effort: None,
            worktree: root.path().to_path_buf(),
            resume: None,
            kind: "implementation".to_string(),
            trace_identity: worker_identity("Trace worker"),
            pinned_task: None,
        })
        .await
        .expect("spawn the worker session");
    session
        .send(SessionCommand::Prompt {
            text: "hello from the worker".to_string(),
            images: Vec::new(),
        })
        .await
        .expect("prompt the worker");
    let trace = engine.trace();
    let record =
        wait_for_terminal_record(&trace, 15_000).await.expect("a terminal worker record");

    assert_eq!(trace.run_id(), record.run_id, "one shared log, one run");
    let attribution = record.attribution.expect("the worker call is attributed");
    assert_eq!(attribution.actor_kind, TraceActorKind::Worker);
    assert_eq!(attribution.agent_name, "Trace worker");
    assert_eq!(attribution.worker_id.as_deref(), Some("w-trace"));
    assert_eq!(attribution.plan_id.as_deref(), Some("plan-trace"));
    assert_eq!(attribution.plan_step, Some(1));
    assert!(!attribution.session_id.is_empty(), "ids filled at spawn");
}
