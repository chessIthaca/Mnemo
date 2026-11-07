use super::*;

use mnemo::headless::GateVerdict;

use super::guardrails::executor_guardrails;
use super::persistence::{TEAM_PRE_TIERS_BACKUP, TEAM_STATE_FILE};
use super::record::WORKER_INBOX_LIMIT;


macro_rules! tiers_dto {
    (difficult: $difficult:expr, average: $average:expr, simple: $simple:expr $(,)?) => {{
        let mut dto = mnemo::headless::TiersDto::default();
        dto.tiers[0].models = $simple;
        dto.tiers[1].models = $average;
        dto.tiers[3].models = $difficult;
        dto
    }};
    (simple: $simple:expr, average: $average:expr, difficult: $difficult:expr $(,)?) => {{
        tiers_dto!(difficult: $difficult, average: $average, simple: $simple)
    }};
}

#[tokio::test]

async fn phase_two_ledgers_load_with_phase_three_defaults() {
    let legacy = json!({
        "workers": [{
            "id": "00000001000000000001",
            "name": "Worker 00000001",
            "brief": "implement the task",
            "model": "test/scripted-a",
            "model_id": "scripted-a",
            "branch": "wt/agent-00000001",
            "worktree": "repo/.worktrees/agent-00000001",
            "status": "completed",
            "generation": 1,
            "transcript": { "messages": [] },
            "pending": null,
            "used_tokens": 120,
            "max_tokens": 128000,
            "token_budget": 200000,
            "checks": [{
                "name": "cargo test --workspace",
                "status": "passed",
                "detail": null
            }],
            "error": "worker token budget of 200000 was exhausted; increase the budget before continuing"
        }]
    });

    let mut persisted: PersistedTeam = serde_json::from_value(legacy).expect("legacy ledger");
    let worker = &persisted.workers[0];
    assert!(serde_json::to_value(worker)
        .expect("worker JSON")
        .get("token_budget")
        .is_none());
    assert_eq!(worker.turns, 0);
    assert_eq!(worker.last_activity_ms, 0);
    assert!(worker.findings.is_empty());
    assert!(worker.review.is_none());
    assert!(worker.fixer_finding_id.is_none());
    assert!(worker.merge.is_none());
    assert_eq!(worker.checks[0].tip, "");
    assert!(worker.inbox.is_empty());
    // Legacy ledgers predate review risk: they decode as standard.
    assert_eq!(worker.review_risk, "standard");
    recover_worker(std::path::Path::new("unused"), &mut persisted.workers[0])
        .await
        .expect("legacy recovery");
    assert!(persisted.workers[0].error.is_none());
    assert_eq!(persisted.workers[0].total_tokens, 120);
    assert_eq!(persisted.workers[0].prompt_tokens, 120);
}

#[test]
fn inbox_entries_use_the_stable_snapshot_shape() {
    let entry = InboxEntry {
        sender: MessageSender::ExternalLead,
        kind: InboxKind::Fix,
        text: "repair this".to_string(),
        at_ms: 7,
    };
    assert_eq!(
        serde_json::to_value(entry).expect("inbox JSON"),
        json!({
            "sender": "external_lead",
            "kind": "fix",
            "text": "repair this",
            "at_ms": 7,
        })
    );
}

#[test]
fn inbox_keeps_only_the_latest_200_entries() {
    let mut record = worker(std::path::Path::new("unused"), "inboxcap");
    for index in 0..(WORKER_INBOX_LIMIT + 5) {
        append_inbox(
            &mut record,
            InboxEntry {
                sender: MessageSender::User,
                kind: InboxKind::Prompt,
                text: format!("message {index}"),
                at_ms: index as u128,
            },
        );
    }
    assert_eq!(record.inbox.len(), WORKER_INBOX_LIMIT);
    assert_eq!(record.inbox.first().expect("oldest retained").text, "message 5");
    assert_eq!(
        record.inbox.last().expect("newest retained").text,
        "message 204"
    );
}

/// Workers do the bulk of the work and run without any session limit.
/// The old 64-request cap and 30-minute wall clock (counted from spawn,
/// idle review time included) parked real implementation workers
/// mid-task.
#[test]
fn executor_sessions_have_no_limits() {
    assert_eq!(executor_guardrails().limits, None);
}

/// Runs a call through the executor's declarative gate and reports
/// whether it was denied.
async fn executor_denies(tool: &str, args: Value) -> bool {
    use mnemo::agent::policy::{AgentPolicy, DeclarativePolicy};
    let policy =
        DeclarativePolicy::new(Arc::new(executor_guardrails())).expect("rules compile");
    matches!(
        policy.gate_call(tool, &args).await,
        GateVerdict::Deny { message } if message.contains("TeamManager owns")
    )
}

#[tokio::test]
async fn executor_gate_denies_integration_through_the_git_tool() {
    for args in [
        json!({"subcommand": "push"}),
        json!({"subcommand": "PUSH", "branch": "main"}),
        json!({"action": "push"}),
        json!({"subcommand": "merge", "branch": "wt/other"}),
        json!({"subcommand": "checkout", "branch": "main"}),
        json!({"subcommand": "branch", "action": "delete", "branch": "x"}),
        json!({"subcommand": "delete", "branch": "x"}),
    ] {
        assert!(executor_denies("git", args.clone()).await, "{args}");
    }
    for args in [
        json!({"subcommand": "status"}),
        json!({"subcommand": "diff", "args": ["--stat"]}),
        json!({"subcommand": "log", "args": ["-5"]}),
        json!({"subcommand": "commit", "message": "push the button"}),
        json!({"subcommand": "stash", "action": "push"}),
        json!({"subcommand": "branch", "action": "list"}),
        json!({"subcommand": "restore", "paths": ["src/lib.rs"]}),
    ] {
        assert!(!executor_denies("git", args.clone()).await, "{args}");
    }
}

#[tokio::test]
async fn executor_gate_denies_integration_through_the_shell() {
    for command in [
        "git push",
        "git push origin HEAD",
        "git -C ../repo push",
        "git -c user.name=x push --force",
        "git --no-pager merge wt/other",
        "cargo test && git push",
        "cargo test;git push",
        "echo hi\ngit push",
        "bash -c \"git push\"",
        "git rebase main",
        "git switch main",
        "git checkout main",
        "git checkout -b feature",
        "git checkout -- a.rs && git checkout main",
        "git branch -d old",
        "git branch -D old",
        "git branch --delete old",
        "git branch -f x -D y",
        "git worktree add ../x",
        "git worktree remove ../x",
        "gh pr create",
        "GIT push",
    ] {
        assert!(
            executor_denies("shell", json!({"command": command, "purpose": "p"})).await,
            "{command}"
        );
    }
    for command in [
        "git status",
        "git diff --stat",
        "git log -5 --oneline",
        "git add -A",
        "git commit -m \"push the fix, merge later\"",
        "git checkout -- a.rs",
        "git checkout HEAD~1 -- a.rs b.rs",
        "git -C ../repo checkout abc123 -- src/",
        "git branch --list",
        "git branch -a -v",
        "cargo test --workspace",
        "echo digit push",
        "ls .git",
        "gh issue list",
    ] {
        assert!(
            !executor_denies("shell", json!({"command": command, "purpose": "p"})).await,
            "{command}"
        );
    }
    // The purpose text is not a command.
    assert!(
        !executor_denies(
            "shell",
            json!({"command": "ls", "purpose": "never git push"})
        )
        .await
    );
}

#[test]
fn interrupted_ledger_replacement_recovers_the_backup() {
    let dir = tempfile::tempdir().expect("ledger dir");
    let persistence = TeamPersistence::new(dir.path());
    persistence
        .save(&PersistedTeam::default())
        .expect("initial save");
    let path = dir.path().join(TEAM_STATE_FILE);
    let backup = dir.path().join("team.json.bak");
    std::fs::rename(&path, &backup).expect("simulate interrupted replacement");
    std::fs::write(&path, "not json").expect("corrupt current ledger");
    assert!(persistence.load().expect("backup is readable").is_some());

    persistence
        .save(&PersistedTeam::default())
        .expect("replacement save");
    assert!(path.exists());
    assert!(!backup.exists());
    assert!(persistence.load().expect("valid replacement").is_some());
    std::fs::write(&path, "not json").expect("corrupt replacement");
    assert!(
        persistence.load().is_err(),
        "corruption must not become an empty team"
    );
}

#[test]
fn legacy_ledger_without_usage_decodes_with_an_empty_accumulator() {
    // Regression: team.json files written before the durable usage
    // accumulator existed have no `usage` key; they must decode to an
    // empty accumulator instead of failing the ledger load.
    let dir = tempfile::tempdir().expect("ledger dir");
    let path = dir.path().join(TEAM_STATE_FILE);
    std::fs::write(
        &path,
        serde_json::json!({ "workers": [], "slots": 4 }).to_string(),
    )
    .expect("legacy ledger");
    let persistence = TeamPersistence::new(dir.path());
    let loaded = persistence
        .load()
        .expect("legacy ledger decodes")
        .expect("some ledger");
    assert_eq!(loaded.slots, Some(4));
    assert!(loaded.usage.models.is_empty());

    // A usage-bearing ledger round-trips its raw token rows verbatim.
    let mut usage = ProjectUsage::default();
    usage.record("glm-5.3", 1_000, 250, 100);
    persistence
        .save(&PersistedTeam {
            workers: Vec::new(),
            slots: Some(4),
            usage: usage.clone(),
        })
        .expect("usage-bearing save");
    let restored = persistence.load().expect("reload").expect("some ledger");
    assert_eq!(restored.usage, usage);
}

fn lifecycle_git(root: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("git process");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn cleanup_worker(
    id: &str,
    branch: &str,
    worktree: &Path,
    status: WorkerStatus,
) -> WorkerRecord {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "name": format!("Worker {id}"),
        "brief": "Implement a scoped change",
        "model": null,
        "model_id": "test-model",
        "branch": branch,
        "worktree": worktree,
        "status": status,
        "generation": 1,
        "transcript": { "messages": [] },
        "pending": null,
        "used_tokens": 0,
        "max_tokens": 1000,
        "checks": [],
        "error": null,
    }))
    .expect("worker fixture")
}

#[tokio::test]
async fn cleanup_keeps_branches_and_dirty_work_but_removes_clean_checkouts() {
    let repo = tempfile::tempdir().expect("repo");
    let state = tempfile::tempdir().expect("state");
    let root = repo.path();
    lifecycle_git(root, &["init", "-b", "wt/test"]);
    lifecycle_git(root, &["config", "user.name", "Test"]);
    lifecycle_git(root, &["config", "user.email", "test@example.invalid"]);
    std::fs::write(root.join("file.txt"), "base\n").expect("base");
    lifecycle_git(root, &["add", "file.txt"]);
    lifecycle_git(root, &["commit", "-m", "base"]);
    let (clean_path, clean_branch) = provision_agent_worktree(
        root.to_path_buf(),
        "clean001".to_string(),
        "wt/test".to_string(),
    )
    .await
    .expect("clean worktree");
    let (dirty_path, dirty_branch) = provision_agent_worktree(
        root.to_path_buf(),
        "dirty002".to_string(),
        "wt/test".to_string(),
    )
    .await
    .expect("dirty worktree");
    std::fs::write(clean_path.join("file.txt"), "worker\n").expect("clean edit");
    lifecycle_git(&clean_path, &["add", "file.txt"]);
    lifecycle_git(&clean_path, &["commit", "-m", "clean change"]);
    std::fs::write(dirty_path.join("uncommitted.txt"), "keep me\n").expect("dirty edit");
    let clean = cleanup_worker(
        "clean001",
        &clean_branch,
        &clean_path,
        WorkerStatus::Stopped,
    );
    let dirty = cleanup_worker(
        "dirty002",
        &dirty_branch,
        &dirty_path,
        WorkerStatus::InReview,
    );
    let persistence = TeamPersistence::new(state.path());
    persistence
        .save(&PersistedTeam {
            workers: vec![clean.clone(), dirty.clone()],
            slots: Some(4),
            usage: ProjectUsage::default(),
        })
        .expect("ledger");

    let report = cleanup_workspace(
        root,
        state.path(),
        &state.path().join("dispatch_routing.jsonl"),
    )
    .await
    .expect("cleanup");
    assert_eq!(report.removed, vec![clean_branch.clone()]);
    assert!(report
        .skipped
        .iter()
        .any(|reason| reason.contains("uncommitted")));
    assert!(!clean_path.exists());
    assert!(dirty_path.join("uncommitted.txt").exists());
    let restored = persistence.load().expect("ledger").expect("saved team");
    assert_eq!(restored.slots, Some(4));
    let cleaned = restored
        .workers
        .iter()
        .find(|worker| worker.id == clean.id)
        .expect("clean record");
    assert_eq!(cleaned.status, WorkerStatus::Cancelled);
    assert!(cleaned.worktree.as_os_str().is_empty());
    let dirty_after = restored
        .workers
        .iter()
        .find(|worker| worker.id == dirty.id)
        .expect("dirty record");
    assert_eq!(dirty_after.status, WorkerStatus::InReview);
    assert!(dirty_after.worktree.exists());
    let diff = crate::mnemo2::workbench::worker_diff(root, cleaned)
        .await
        .expect("the cleaned branch remains reviewable");
    assert_eq!(diff.total_files, 1);
    assert!(phase3::git_value(root, &["rev-parse", &clean_branch])
        .await
        .is_ok());
    assert!(phase3::git_value(root, &["rev-parse", &dirty_branch])
        .await
        .is_ok());
}

#[tokio::test]
async fn the_lead_clean_check_ignores_plans_but_the_worker_check_does_not() {
    let repo = tempfile::tempdir().expect("repo");
    let root = repo.path();
    lifecycle_git(root, &["init", "-b", "wt/test"]);
    lifecycle_git(root, &["config", "user.name", "Test"]);
    lifecycle_git(root, &["config", "user.email", "test@example.invalid"]);
    std::fs::create_dir_all(root.join(".coding/plans")).expect("plans dir");
    std::fs::create_dir_all(root.join(".coding/knowledge")).expect("knowledge dir");
    std::fs::write(root.join("file.txt"), "base").expect("base");
    std::fs::write(root.join(".coding/plans/tracked.md"), "plan").expect("tracked plan");
    std::fs::write(root.join(".coding/plans/moved.md"), "plan").expect("moved plan");
    lifecycle_git(root, &["add", "."]);
    lifecycle_git(root, &["commit", "-m", "base"]);

    // Edited, new, renamed, and staged plan files do not dirty the lead.
    std::fs::write(root.join(".coding/plans/tracked.md"), "edited").expect("edit plan");
    std::fs::write(root.join(".coding/plans/new.md"), "new").expect("new plan");
    lifecycle_git(
        root,
        &["mv", ".coding/plans/moved.md", ".coding/plans/renamed.md"],
    );
    phase3::require_clean_lead(root, "lead")
        .await
        .expect("plans are ignored");
    let strict = phase3::require_clean(root, "worker")
        .await
        .expect_err("strict check");
    assert!(strict.contains("must be clean"), "{strict}");

    // Knowledge records are lead bookkeeping too; a source change still
    // blocks.
    std::fs::write(root.join(".coding/knowledge/note.md"), "note").expect("knowledge");
    phase3::require_clean_lead(root, "lead")
        .await
        .expect("knowledge records are lead bookkeeping too");
    std::fs::remove_file(root.join(".coding/knowledge/note.md")).expect("remove note");
    std::fs::create_dir_all(root.join(".coding/openspec/specs/importer"))
        .expect("OpenSpec dir");
    std::fs::write(
        root.join(".coding/openspec/specs/importer/spec.md"),
        "# Importer\n",
    )
    .expect("OpenSpec spec");
    phase3::require_clean_lead(root, "lead")
        .await
        .expect("OpenSpec specs and archives are lead bookkeeping too");
    std::fs::remove_dir_all(root.join(".coding/openspec")).expect("remove OpenSpec dir");
    std::fs::write(root.join("file.txt"), "changed").expect("change source");
    phase3::require_clean_lead(root, "lead")
        .await
        .expect_err("source change");
    lifecycle_git(root, &["checkout", "--", "file.txt"]);
    // A rename out of the plans dir is not a plan-only change.
    lifecycle_git(root, &["mv", ".coding/plans/renamed.md", "stolen.md"]);
    phase3::require_clean_lead(root, "lead")
        .await
        .expect_err("rename out of plans");
}

fn run_git(root: &std::path::Path, args: &[&str]) {
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

fn init_git(root: &std::path::Path) {
    run_git(root, &["init", "-b", "main"]);
    run_git(root, &["config", "user.name", "Mnemo2 Test"]);
    run_git(root, &["config", "user.email", "mnemo2@example.invalid"]);
    std::fs::write(root.join("README.md"), "seed\n").expect("seed file");
    std::fs::write(root.join(".gitignore"), ".worktrees/\n.coding/\n")
        .expect("ignore worktrees and state");
    run_git(root, &["add", "README.md", ".gitignore"]);
    run_git(root, &["commit", "-m", "seed"]);
}

pub(super) fn worker(root: &std::path::Path, id: &str) -> WorkerRecord {
    let branch = agent_branch(id);
    WorkerRecord {
        id: id.to_string(),
        inbox: Vec::new(),
        name: id.to_string(),
        brief: "test worker".to_string(),
        opening_prompt: None,
        model: None,
        model_id: None,
        reasoning_effort: None,
        branch,
        worktree: worker_checkout(root, id),
        status: WorkerStatus::Completed,
        generation: 0,
        transcript: Transcript::default(),
        pending: None,
        used_tokens: 0,
        max_tokens: 0,
        brief_sent: true,
        read_back: false,
        compacting: false,
        total_tokens: 0,
        prompt_tokens: 0,
        completion_tokens: 0,
        cached_tokens: 0,
        requests: 0,
        cost_micros: None,
        cost_notified_micros: 0,
        created_at_ms: 0,
        elapsed_ms: 0,
        turns: 0,
        last_activity_ms: 0,
        checks: Vec::new(),
        review_risk: "standard".to_string(),
        kind: "implementation".to_string(),
        audit_target: None,
        audit_tip: None,
        report_delivered: false,
        findings: Vec::new(),
        review: None,
        review_rounds: 0,
        fix_rounds: 0,
        audits: 0,
        models_used: Vec::new(),
        fixer_finding_id: None,
        merge: None,
        error: None,
        tool_errors: Vec::new(),
        plan_id: None,
        plan_step: None,
        tier: None,
        routing: None,
        reroutes: Vec::new(),
    }
}

fn deregister_worktree(root: &std::path::Path, worker_id: &str) {
    let admin = root
        .join(".git")
        .join("worktrees")
        .join(format!("agent-{}", agent_short_id(worker_id)));
    std::fs::remove_dir_all(&admin).expect("remove the worktree admin data");
    run_git(root, &["worktree", "prune"]);
}

async fn open_test_team(
    root: &std::path::Path,
    state: &std::path::Path,
    workers: Vec<WorkerRecord>,
) -> TeamRuntime {
    let config = tempfile::tempdir().expect("config");
    open_test_team_with_config(root, state, workers, config.path()).await
}

async fn open_test_team_with_config(
    root: &std::path::Path,
    state: &std::path::Path,
    workers: Vec<WorkerRecord>,
    config: &std::path::Path,
) -> TeamRuntime {
    std::fs::write(
        config.join("config.toml"),
        "[general]\ndefault_provider = \"test\"\ndefault_model = \"scripted-a\"\n",
    )
    .expect("config.toml");
    std::fs::write(
        config.join("endpoints.toml"),
        "[[endpoint]]
name = \"test\"
kind = \"local\"
base_url = \"http://127.0.0.1:9/v1/\"
models = [\"scripted-a\", \"scripted-b\"]
",
    )
    .expect("endpoints.toml");
    TeamPersistence::new(state)
        .save(&PersistedTeam {
            workers,
            slots: Some(2),
            usage: ProjectUsage::default(),
        })
        .expect("save the team ledger");
    let engine = mnemo::headless::Engine::start(mnemo::headless::EngineOptions {
        workspace: mnemo::workspace::Workspace::new(root),
        config_dir: Some(config.to_path_buf()),
        permissions: mnemo::agent::policy::PermissionPolicy::OverridePermissions,
        memory: mnemo::headless::MemoryBackend::Disabled,
        index_codegraph: false,
        require_git: true,
        auto_install_embedder: false,
    })
    .await
    .expect("start the test engine");
    TeamRuntime::open(TeamRuntimeOptions {
        engine: Arc::new(engine),
        workspace_root: root.to_path_buf(),
        state_dir: state.to_path_buf(),
        config_dir: config.to_path_buf(),
        classifier: None,
        routing_log_path: Some(state.join("dispatch_routing.jsonl")),
        slots: Some(2),
        models: Vec::new(),
        pricing: Vec::new(),
    })
    .await
    .expect("open the test team")
}

async fn spawn_live_worker(team: &TeamRuntime, worker: &WorkerRecord) {
    let session = team
        .spawn_worker_session(WorkerSessionSpec {
            model: None,
            reasoning_effort: None,
            worktree: worker.worktree.clone(),
            resume: None,
            kind: "implementation".to_string(),
            trace_identity: TeamRuntime::worker_trace_identity(worker),
            pinned_task: None,
        })
        .await
        .expect("spawn worker session");
    team.live.lock().await.insert(
        worker.id.clone(),
        LiveWorker {
            session,
            pump: tokio::spawn(std::future::pending::<()>()),
            cleanup_retry_only: false,
            busy: Arc::new(AtomicBool::new(false)),
            strikes: Arc::new(Mutex::new(route_question::Strikes::new(NUDGE_STRIKE_LIMIT))),
            steer_pending: Arc::new(AtomicBool::new(false)),
        },
    );
}

#[cfg(windows)]
fn create_directory_link(link: &std::path::Path, target: &std::path::Path) {
    let output = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .expect("create a junction");
    assert!(
        output.status.success(),
        "mklink failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
fn create_directory_link(link: &std::path::Path, target: &std::path::Path) {
    std::os::unix::fs::symlink(target, link).expect("create a directory symlink");
}

#[tokio::test]
async fn a_half_removed_checkout_is_finished_after_merge() {
    let root = tempfile::tempdir().expect("workspace");
    init_git(root.path());
    let worker = worker(root.path(), "00000001000000000001");
    provision_agent_worktree(
        root.path().to_path_buf(),
        worker.id.clone(),
        "main".to_string(),
    )
    .await
    .expect("provision the worker checkout");
    deregister_worktree(root.path(), &worker.id);
    assert!(worker.worktree.join(".git").is_file());

    cleanup_agent_worktree(root.path(), &worker)
        .await
        .expect("finish removing the half-removed checkout");

    assert!(!worker.worktree.exists());
}

#[tokio::test]
async fn a_leftover_checkout_does_not_block_deleting_its_worker() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let dangling = worker(root.path(), "00000001000000000001");
    provision_agent_worktree(
        root.path().to_path_buf(),
        dangling.id.clone(),
        "main".to_string(),
    )
    .await
    .expect("provision the worker checkout");
    deregister_worktree(root.path(), &dangling.id);
    let fragment = worker(root.path(), "00000002000000000002");
    run_git(root.path(), &["branch", &fragment.branch, "main"]);
    std::fs::create_dir_all(&fragment.worktree).expect("fragment checkout");
    std::fs::write(fragment.worktree.join("fragment.txt"), "leftover\n")
        .expect("fragment file");
    let team = open_test_team(
        root.path(),
        state.path(),
        vec![dangling.clone(), fragment.clone()],
    )
    .await;
    for worker in [&dangling, &fragment] {
        let session = team
            .spawn_worker_session(WorkerSessionSpec {
                model: None,
                reasoning_effort: None,
                worktree: worker.worktree.clone(),
                resume: None,
                kind: "implementation".to_string(),
                trace_identity: TeamRuntime::worker_trace_identity(worker),
                pinned_task: None,
            })
            .await
            .expect("open a worktree-bound session and its code graph");
        assert!(worker.worktree.join(".coding/codegraph.db").is_file());
        team.live.lock().await.insert(
            worker.id.clone(),
            LiveWorker {
                session,
                pump: tokio::spawn(async {}),
                cleanup_retry_only: false,
                busy: Arc::new(AtomicBool::new(false)),
                strikes: Arc::new(Mutex::new(route_question::Strikes::new(NUDGE_STRIKE_LIMIT))),
                steer_pending: Arc::new(AtomicBool::new(false)),
            },
        );
    }
    let ids = vec![dangling.id.clone(), fragment.id.clone()];

    let previews = team.deletion_preview(&ids).await.expect("preview leftovers");
    assert!(previews.iter().all(|preview| preview.blocked.is_none()));
    let outcome = team
        .delete_workers(&ids, false)
        .await
        .expect("delete workers with leftover checkouts");

    assert!(outcome.snapshot.workers.is_empty());
    assert!(team.live.lock().await.is_empty());
    assert!(!dangling.worktree.exists());
    assert!(!fragment.worktree.exists());
}

/// One worker whose checkout refuses to delete must not keep the other
/// workers from being deleted: the flow reports that worker's error and
/// still listed, and removes every other worker.
#[tokio::test]
async fn delete_workers_continues_past_a_failed_worker() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let first = worker(root.path(), "00000001000000000001");
    let stuck = worker(root.path(), "00000002000000000002");
    let third = worker(root.path(), "00000003000000000003");
    for record in [&first, &stuck, &third] {
        run_git(root.path(), &["branch", &record.branch, "main"]);
        std::fs::create_dir_all(&record.worktree).expect("checkout folder");
        std::fs::write(record.worktree.join("note.txt"), "left\n").expect("checkout file");
    }
    // The stuck worker's checkout path is a file, so its removal is
    // refused — the other two workers must still be deleted.
    std::fs::remove_dir_all(&stuck.worktree).expect("clear the stuck path");
    std::fs::write(&stuck.worktree, "not a directory\n").expect("undeletable checkout");
    let team = open_test_team(
        root.path(),
        state.path(),
        vec![first.clone(), stuck.clone(), third.clone()],
    )
    .await;
    let ids = vec![first.id.clone(), stuck.id.clone(), third.id.clone()];

    let outcome = team
        .delete_workers(&ids, false)
        .await
        .expect("delete every worker that can be deleted");

    // One result per requested worker, in request order.
    assert_eq!(outcome.results.len(), 3, "{:?}", outcome.results);
    assert!(outcome.results[0].error.is_none(), "{:?}", outcome.results);
    let stuck_error = outcome.results[1]
        .error
        .as_ref()
        .expect("the stuck worker is reported with its error");
    assert!(stuck_error.contains("refus"), "unexpected error: {stuck_error}");
    assert!(outcome.results[2].error.is_none(), "{:?}", outcome.results);
    // The first and third workers are gone; the stuck one is still listed.
    assert!(team.worker(&first.id).await.is_err());
    assert!(team.worker(&third.id).await.is_err());
    assert!(team.worker(&stuck.id).await.is_ok());
    assert_eq!(outcome.snapshot.workers.len(), 1);
    assert_eq!(outcome.snapshot.workers[0].id, stuck.id);
    // The deletable checkouts and branches are gone; the stuck
    // worker's branch and checkout stay.
    assert!(!first.worktree.exists());
    assert!(!third.worktree.exists());
    assert!(!branch_exists(root.path(), &first.branch).await);
    assert!(!branch_exists(root.path(), &third.branch).await);
    assert!(stuck.worktree.exists());
    assert!(branch_exists(root.path(), &stuck.branch).await);
}

/// A deleted worker's checkout is renamed away with one move and its
/// worktree registration is pruned, so the delete flow never waits for
/// the file-by-file removal of a large checkout. The renamed folder is
/// removed by the trash sweep.
#[tokio::test]
async fn a_deleted_checkout_is_renamed_to_trash_and_deregistered() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let first = worker(root.path(), "00000001000000000001");
    provision_agent_worktree(
        root.path().to_path_buf(),
        first.id.clone(),
        "main".to_string(),
    )
    .await
    .expect("provision the worker checkout");
    let team = open_test_team(root.path(), state.path(), vec![first.clone()]).await;

    let outcome = team
        .delete_workers(std::slice::from_ref(&first.id), true)
        .await
        .expect("delete the worker");

    assert!(outcome.results[0].error.is_none(), "{:?}", outcome.results);
    // The checkout path is gone at once: the folder was renamed away,
    // not deleted file by file.
    assert!(!first.worktree.exists());
    // The worktree registration is pruned and the branch is gone —
    // the branch delete would refuse while a worktree still uses it.
    assert!(
        !checkout_is_registered(root.path(), &first.worktree)
            .await
            .expect("worktree registry"),
        "the renamed checkout is deregistered"
    );
    assert!(!branch_exists(root.path(), &first.branch).await);
    // The renamed folder leaves the trash with the sweep.
    sweep_trash(root.path()).await;
    assert!(trash_entries(root.path()).is_empty());
}

/// The trash sweep removes every renamed-away checkout folder, one
/// locked folder never keeping the others.
#[tokio::test]
async fn the_trash_sweep_removes_renamed_checkouts() {
    let root = tempfile::tempdir().expect("workspace");
    init_git(root.path());
    let record = worker(root.path(), "00000001000000000001");
    std::fs::create_dir_all(&record.worktree).expect("checkout folder");
    std::fs::write(record.worktree.join("note.txt"), "left\n").expect("checkout file");
    let canonical = validated_checkout(root.path(), &record)
        .expect("the checkout validates")
        .expect("it exists");

    move_checkout_to_trash(root.path(), &canonical)
        .await
        .expect("rename into trash");

    assert!(!record.worktree.exists());
    assert_eq!(trash_entries(root.path()).len(), 1);
    sweep_trash(root.path()).await;
    assert!(trash_entries(root.path()).is_empty());
}

/// The streamed preview emits one update per worker as each finishes,
/// while the returned previews stay in request order.
#[tokio::test]
async fn deletion_preview_streams_each_worker_as_it_checks() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let first = worker(root.path(), "00000001000000000001");
    let second = worker(root.path(), "00000002000000000002");
    for record in [&first, &second] {
        run_git(root.path(), &["branch", &record.branch, "main"]);
        std::fs::create_dir_all(&record.worktree).expect("checkout folder");
    }
    let team = open_test_team(
        root.path(),
        state.path(),
        vec![first.clone(), second.clone()],
    )
    .await;
    let ids = vec![first.id.clone(), second.id.clone()];

    let streamed: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    let previews = team
        .deletion_preview_streaming(&ids, |preview| {
            streamed.lock().expect("stream lock").push(preview.worker_id.clone());
        })
        .await
        .expect("preview");

    let mut seen = streamed.into_inner().expect("stream lock");
    seen.sort();
    assert_eq!(seen, ids, "every worker streamed exactly once");
    let returned: Vec<String> = previews.iter().map(|preview| preview.worker_id.clone()).collect();
    assert_eq!(returned, ids, "previews return in request order");
}

/// On Windows, a held checkout refuses the rename into trash: the
/// removal falls back to the foreground path, reports the holder, and
/// still deletes every other worker in the batch.
#[cfg(windows)]
#[tokio::test]
async fn a_held_checkout_falls_back_to_the_foreground_removal_and_reports_it() {
    use std::os::windows::fs::OpenOptionsExt;

    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let held = worker(root.path(), "00000001000000000001");
    let other = worker(root.path(), "00000002000000000002");
    for record in [&held, &other] {
        provision_agent_worktree(
            root.path().to_path_buf(),
            record.id.clone(),
            "main".to_string(),
        )
        .await
        .expect("provision the worker checkout");
    }
    // One exclusively-open file inside the checkout refuses both the
    // rename and any file-by-file delete.
    let hold = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .share_mode(0)
        .open(held.worktree.join("held.txt"))
        .expect("hold a file inside the checkout");
    let team = open_test_team(
        root.path(),
        state.path(),
        vec![held.clone(), other.clone()],
    )
    .await;

    let outcome = team
        .delete_workers(&[held.id.clone(), other.id.clone()], true)
        .await
        .expect("delete every worker that can be deleted");

    let held_error = outcome.results[0]
        .error
        .as_ref()
        .expect("the held worker is reported with its error");
    assert!(
        held_error.contains("could not remove"),
        "unexpected error: {held_error}"
    );
    assert!(team.worker(&held.id).await.is_ok());
    assert!(held.worktree.exists());
    assert!(outcome.results[1].error.is_none(), "{:?}", outcome.results);
    assert!(team.worker(&other.id).await.is_err());
    assert!(!other.worktree.exists());
    drop(hold);
}

#[tokio::test]
async fn leftover_removal_never_leaves_the_worker_folder() {
    let root = tempfile::tempdir().expect("workspace");
    let outside = tempfile::tempdir().expect("outside");
    init_git(root.path());
    let leftover = worker(root.path(), "00000001000000000001");
    std::fs::create_dir_all(&leftover.worktree).expect("leftover checkout");
    std::fs::write(leftover.worktree.join("fragment.txt"), "leftover\n")
        .expect("leftover file");
    let sibling = root.path().join(".worktrees").join("keep.txt");
    std::fs::write(&sibling, "keep\n").expect("sibling file");
    let main_marker = root.path().join("README.md");
    let original_main = std::fs::read(&main_marker).expect("main checkout marker");

    cleanup_agent_worktree(root.path(), &leftover)
        .await
        .expect("remove only the leftover checkout");
    assert!(!leftover.worktree.exists());
    assert_eq!(std::fs::read_to_string(&sibling).expect("sibling"), "keep\n");
    assert_eq!(
        std::fs::read(&main_marker).expect("main checkout survives"),
        original_main
    );

    let linked = worker(root.path(), "00000002000000000002");
    let outside_marker = outside.path().join("keep.txt");
    std::fs::write(&outside_marker, "outside\n").expect("outside marker");
    create_directory_link(&linked.worktree, outside.path());
    let error = cleanup_agent_worktree(root.path(), &linked)
        .await
        .expect_err("a junction or symlink at the checkout path is refused");
    assert!(error.contains("refus"), "unexpected error: {error}");
    assert!(linked.worktree.exists());
    assert_eq!(
        std::fs::read_to_string(&outside_marker).expect("outside marker survives"),
        "outside\n"
    );
}

#[tokio::test]
async fn stop_close_error_is_persisted_and_retryable() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let worker = worker(root.path(), "00000001000000000001");
    provision_agent_worktree(
        root.path().to_path_buf(),
        worker.id.clone(),
        "main".to_string(),
    )
    .await
    .expect("provision worker");
    let team = open_test_team(root.path(), state.path(), vec![worker.clone()]).await;
    spawn_live_worker(&team, &worker).await;
    assert!(!team
        .live
        .lock()
        .await
        .get(&worker.id)
        .expect("pending live worker")
        .pump
        .is_finished());
    team.fail_next_close_for_test();

    let error = team.stop_inner(&worker.id).await.expect_err("injected close failure");
    assert!(error.contains("injected close failure"), "{error}");
    assert!(team.live.lock().await.contains_key(&worker.id));
    let persisted = TeamPersistence::new(state.path())
        .load()
        .expect("load team")
        .expect("saved team");
    assert!(persisted.workers[0]
        .error
        .as_deref()
        .is_some_and(|error| error.contains("injected close failure")));
    let busy = team
        .live
        .lock()
        .await
        .get(&worker.id)
        .expect("retained live worker")
        .busy
        .clone();
    for _ in 0..10 {
        if team
            .live
            .lock()
            .await
            .get(&worker.id)
            .expect("retained live worker")
            .pump
            .is_finished()
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(team
        .live
        .lock()
        .await
        .get(&worker.id)
        .expect("retained live worker")
        .pump
        .is_finished());
    let message = team
        .message_inner(
            &worker.id,
            "must not run without a pump".into(),
            MessageSender::Lead,
        )
        .await
        .expect_err("a cleanup-retry-only worker rejects messages");
    assert!(message.contains("cleanup"), "{message}");
    let resume = team
        .resume_inner(&worker.id)
        .await
        .expect_err("a cleanup-retry-only worker rejects resume");
    assert!(resume.contains("cleanup"), "{resume}");
    let before_commands = team.worker(&worker.id).await.expect("worker before commands");
    let compact = team
        .compact(&worker.id)
        .await
        .expect_err("a cleanup-retry-only worker rejects compact");
    assert!(compact.contains("cleanup"), "{compact}");
    let after_compact = team.worker(&worker.id).await.expect("worker after compact");
    assert_eq!(after_compact.status, before_commands.status);
    assert_eq!(after_compact.compacting, before_commands.compacting);
    assert!(!busy.load(Ordering::SeqCst));
    let pause = team
        .pause(&worker.id)
        .await
        .expect_err("a cleanup-retry-only worker rejects pause");
    assert!(pause.contains("cleanup"), "{pause}");
    let interrupt = team
        .interrupt(&worker.id)
        .await
        .expect_err("a cleanup-retry-only worker rejects interrupt");
    assert!(interrupt.contains("cleanup"), "{interrupt}");
    let after_commands = team.worker(&worker.id).await.expect("worker after commands");
    assert_eq!(after_commands.status, before_commands.status);
    assert_eq!(after_commands.compacting, before_commands.compacting);
    assert!(!busy.load(Ordering::SeqCst));

    let stopped = team.stop_inner(&worker.id).await.expect("stop retry");
    assert_eq!(stopped.status, WorkerStatus::Stopped);
    assert!(!team.live.lock().await.contains_key(&worker.id));
}

#[tokio::test]
async fn merge_close_error_returns_to_review_and_retries_cleanup() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    run_git(root.path(), &["checkout", "-b", "wt/mnemo"]);
    let mut worker = worker(root.path(), "00000001000000000001");
    worker.status = WorkerStatus::InReview;
    provision_agent_worktree(
        root.path().to_path_buf(),
        worker.id.clone(),
        "wt/mnemo".to_string(),
    )
    .await
    .expect("provision worker");
    std::fs::write(worker.worktree.join("merged.txt"), "merged\n").expect("worker change");
    run_git(&worker.worktree, &["add", "merged.txt"]);
    run_git(&worker.worktree, &["commit", "-m", "worker change"]);
    let tip = phase3::tip(&worker.worktree).await.expect("worker tip");
    worker.checks = vec![WorkerCheck {
        name: "cargo test --workspace".into(),
        status: "passed".into(),
        detail: None,
        tip: tip.clone(),
    }];
    worker.review = Some(ReviewRecord {
        summary: "reviewed".into(),
        tip,
        reviewed_at_ms: 1,
        evidence: Some(evidence()),
    });
    let team = open_test_team(root.path(), state.path(), vec![worker.clone()]).await;
    spawn_live_worker(&team, &worker).await;
    team.fail_next_close_for_test();

    let failed = team.merge(&worker.id).await.expect("merge close diagnostic");
    assert_eq!(failed.status, WorkerStatus::InReview);
    assert_eq!(failed.merge, None);
    assert!(failed
        .error
        .as_deref()
        .is_some_and(|error| error.contains("retry merge")));
    assert!(team.live.lock().await.contains_key(&worker.id));
    assert!(worker.worktree.exists());
    assert!(
        branch_exists(root.path(), &worker.branch).await,
        "a failed close keeps the branch"
    );
    let persisted = TeamPersistence::new(state.path())
        .load()
        .expect("load team")
        .expect("saved team");
    assert_eq!(persisted.workers[0].status, WorkerStatus::InReview);
    assert!(persisted.workers[0].error.is_some());

    let merged = team.merge(&worker.id).await.expect("merge retry");
    assert_eq!(merged.status, WorkerStatus::Merged);
    assert_eq!(merged.error, None);
    assert!(!worker.worktree.exists());
    assert!(!team.live.lock().await.contains_key(&worker.id));
    assert!(!branch_exists(root.path(), &worker.branch).await);
}

#[tokio::test]
async fn merge_closes_a_live_session_before_finishing_its_checkout() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    run_git(root.path(), &["checkout", "-b", "wt/mnemo"]);
    let mut worker = worker(root.path(), "00000001000000000001");
    worker.status = WorkerStatus::InReview;
    provision_agent_worktree(
        root.path().to_path_buf(),
        worker.id.clone(),
        "wt/mnemo".to_string(),
    )
    .await
    .expect("provision the worker checkout");
    std::fs::write(worker.worktree.join("merged.txt"), "merged\n").expect("worker change");
    run_git(&worker.worktree, &["add", "merged.txt"]);
    run_git(&worker.worktree, &["commit", "-m", "worker change"]);
    let tip = phase3::tip(&worker.worktree).await.expect("worker tip");
    worker.checks = vec![WorkerCheck {
        name: "cargo test --workspace".into(),
        status: "passed".into(),
        detail: None,
        tip: tip.clone(),
    }];
    worker.review = Some(ReviewRecord {
        summary: "reviewed".into(),
        tip,
        reviewed_at_ms: 1,
        evidence: Some(evidence()),
    });
    let team = open_test_team(root.path(), state.path(), vec![worker.clone()]).await;
    let session = team
        .spawn_worker_session(WorkerSessionSpec {
            model: None,
            reasoning_effort: None,
            worktree: worker.worktree.clone(),
            resume: None,
            kind: "implementation".to_string(),
            trace_identity: TeamRuntime::worker_trace_identity(&worker),
            pinned_task: None,
        })
        .await
        .expect("open a worktree-bound session and its code graph");
    assert!(worker.worktree.join(".coding/codegraph.db").is_file());
    team.live.lock().await.insert(
        worker.id.clone(),
        LiveWorker {
            session,
            pump: tokio::spawn(async {}),
            cleanup_retry_only: false,
            busy: Arc::new(AtomicBool::new(false)),
            strikes: Arc::new(Mutex::new(route_question::Strikes::new(NUDGE_STRIKE_LIMIT))),
            steer_pending: Arc::new(AtomicBool::new(false)),
        },
    );

    let merged = team.merge(&worker.id).await.expect("merge the worker");

    assert_eq!(merged.status, WorkerStatus::Merged);
    assert_eq!(merged.error, None);
    assert!(!worker.worktree.exists());
    assert!(!team.live.lock().await.contains_key(&worker.id));
    assert_eq!(
        std::fs::read_to_string(root.path().join("merged.txt"))
            .expect("merged file")
            .trim_end(),
        "merged"
    );
    let persisted = TeamPersistence::new(state.path())
        .load()
        .expect("load persisted team")
        .expect("saved team");
    assert_eq!(persisted.workers[0].error, None);
}

/// A clean merge deletes the worker's branch with Git's safe delete, and
/// the merge commit keeps the reviewed tip as its second parent, so the
/// work is still there without the branch ref.
#[tokio::test]
async fn merge_deletes_the_merged_branch_and_keeps_the_work() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    run_git(root.path(), &["checkout", "-b", "wt/mnemo"]);
    let mut worker = worker(root.path(), "00000001000000000001");
    worker.status = WorkerStatus::InReview;
    provision_agent_worktree(
        root.path().to_path_buf(),
        worker.id.clone(),
        "wt/mnemo".to_string(),
    )
    .await
    .expect("provision worker");
    std::fs::write(worker.worktree.join("merged.txt"), "merged\n").expect("worker change");
    run_git(&worker.worktree, &["add", "merged.txt"]);
    run_git(&worker.worktree, &["commit", "-m", "worker change"]);
    let tip = phase3::tip(&worker.worktree).await.expect("worker tip");
    worker.checks = vec![WorkerCheck {
        name: "cargo test --workspace".into(),
        status: "passed".into(),
        detail: None,
        tip: tip.clone(),
    }];
    worker.review = Some(ReviewRecord {
        summary: "reviewed".into(),
        tip: tip.clone(),
        reviewed_at_ms: 1,
        evidence: Some(evidence()),
    });
    let team = open_test_team(root.path(), state.path(), vec![worker.clone()]).await;
    assert!(branch_exists(root.path(), &worker.branch).await);

    let merged = team.merge(&worker.id).await.expect("merge");

    assert_eq!(merged.status, WorkerStatus::Merged);
    assert_eq!(merged.error, None);
    assert!(!worker.worktree.exists());
    assert!(
        !branch_exists(root.path(), &worker.branch).await,
        "the merge deletes the worker branch"
    );
    let commit = merged
        .merge
        .as_ref()
        .expect("the merge record")
        .commit
        .clone();
    assert_eq!(
        phase3::git_value(root.path(), &["rev-parse", &format!("{commit}^2")])
            .await
            .expect("the merge commit's second parent"),
        tip,
        "the merge commit keeps the reviewed tip as its second parent"
    );
    run_git(root.path(), &["merge-base", "--is-ancestor", &tip, "HEAD"]);
}

#[tokio::test]
async fn delete_merged_branch_deletes_a_fully_merged_branch() {
    let root = tempfile::tempdir().expect("workspace");
    init_git(root.path());
    run_git(root.path(), &["checkout", "-b", "wt/mnemo"]);
    let worker = worker(root.path(), "00000001000000000001");
    run_git(root.path(), &["branch", &worker.branch, "main"]);

    assert_eq!(delete_merged_branch(root.path(), &worker).await, Ok(true));
    assert!(!branch_exists(root.path(), &worker.branch).await);
}

#[tokio::test]
async fn delete_merged_branch_refuses_a_branch_that_is_not_fully_merged() {
    let root = tempfile::tempdir().expect("workspace");
    init_git(root.path());
    run_git(root.path(), &["checkout", "-b", "wt/mnemo"]);
    let worker = worker(root.path(), "00000001000000000001");
    run_git(root.path(), &["checkout", "-b", &worker.branch]);
    std::fs::write(root.path().join("unmerged.txt"), "unmerged\n").expect("worker file");
    run_git(root.path(), &["add", "unmerged.txt"]);
    run_git(root.path(), &["commit", "-m", "unmerged work"]);
    run_git(root.path(), &["checkout", "wt/mnemo"]);

    let deleted = delete_merged_branch(root.path(), &worker).await;

    assert!(
        deleted.is_err(),
        "Git refuses to delete an unmerged branch: {deleted:?}"
    );
    assert!(branch_exists(root.path(), &worker.branch).await);
}

#[tokio::test]
async fn delete_merged_branch_skips_a_branch_the_worker_does_not_own() {
    let root = tempfile::tempdir().expect("workspace");
    init_git(root.path());
    run_git(root.path(), &["checkout", "-b", "wt/mnemo"]);
    let mut other = worker(root.path(), "00000001000000000001");
    other.branch = "wt/other".to_string();
    run_git(root.path(), &["branch", "wt/other", "main"]);

    assert_eq!(delete_merged_branch(root.path(), &other).await, Ok(false));
    assert!(branch_exists(root.path(), "wt/other").await);

    let missing = worker(root.path(), "00000002000000000002");
    assert_eq!(delete_merged_branch(root.path(), &missing).await, Ok(false));
}

/// The leftover branch of an earlier merged worker — its checkout already
/// gone — is deleted by the next merge's sweep.
#[tokio::test]
async fn a_later_merge_sweeps_an_earlier_merged_workers_leftover_branch() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    run_git(root.path(), &["checkout", "-b", "wt/mnemo"]);
    let early = merged_worker(root.path(), "00000001000000000001");
    run_git(root.path(), &["branch", &early.branch, "main"]);
    let mut worker = worker(root.path(), "00000002000000000002");
    worker.status = WorkerStatus::InReview;
    provision_agent_worktree(
        root.path().to_path_buf(),
        worker.id.clone(),
        "wt/mnemo".to_string(),
    )
    .await
    .expect("provision worker");
    std::fs::write(worker.worktree.join("merged.txt"), "merged\n").expect("worker change");
    run_git(&worker.worktree, &["add", "merged.txt"]);
    run_git(&worker.worktree, &["commit", "-m", "worker change"]);
    let tip = phase3::tip(&worker.worktree).await.expect("worker tip");
    worker.checks = vec![WorkerCheck {
        name: "cargo test --workspace".into(),
        status: "passed".into(),
        detail: None,
        tip: tip.clone(),
    }];
    worker.review = Some(ReviewRecord {
        summary: "reviewed".into(),
        tip,
        reviewed_at_ms: 1,
        evidence: Some(evidence()),
    });
    let team =
        open_test_team(root.path(), state.path(), vec![early.clone(), worker.clone()]).await;

    let merged = team.merge(&worker.id).await.expect("merge");

    assert_eq!(merged.status, WorkerStatus::Merged);
    assert!(!branch_exists(root.path(), &worker.branch).await);
    assert!(
        !branch_exists(root.path(), &early.branch).await,
        "the sweep deletes the earlier merged worker's leftover branch"
    );
    assert_eq!(team.worker(&early.id).await.expect("early worker").error, None);
}

/// The sweep keeps every branch it must not touch: a reverted merge, an
/// unreleased checkout, and a branch with commits that never reached the
/// lead branch.
#[tokio::test]
async fn the_sweep_keeps_reverted_held_and_unmerged_branches() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    run_git(root.path(), &["checkout", "-b", "wt/mnemo"]);
    let mut reverted = merged_worker(root.path(), "00000003000000000003");
    reverted
        .merge
        .as_mut()
        .expect("the merge record")
        .revert_commit = Some("deadbeef".into());
    run_git(root.path(), &["branch", &reverted.branch, "main"]);
    let held = merged_worker(root.path(), "00000004000000000004");
    std::fs::create_dir_all(&held.worktree).expect("held checkout");
    run_git(root.path(), &["branch", &held.branch, "main"]);
    let unmerged = merged_worker(root.path(), "00000005000000000005");
    run_git(root.path(), &["checkout", "-b", &unmerged.branch]);
    std::fs::write(root.path().join("unmerged.txt"), "unmerged\n").expect("unmerged file");
    run_git(root.path(), &["add", "unmerged.txt"]);
    run_git(root.path(), &["commit", "-m", "unmerged work"]);
    run_git(root.path(), &["checkout", "wt/mnemo"]);
    let mut worker = worker(root.path(), "00000002000000000002");
    worker.status = WorkerStatus::InReview;
    provision_agent_worktree(
        root.path().to_path_buf(),
        worker.id.clone(),
        "wt/mnemo".to_string(),
    )
    .await
    .expect("provision worker");
    std::fs::write(worker.worktree.join("merged.txt"), "merged\n").expect("worker change");
    run_git(&worker.worktree, &["add", "merged.txt"]);
    run_git(&worker.worktree, &["commit", "-m", "worker change"]);
    let tip = phase3::tip(&worker.worktree).await.expect("worker tip");
    worker.checks = vec![WorkerCheck {
        name: "cargo test --workspace".into(),
        status: "passed".into(),
        detail: None,
        tip: tip.clone(),
    }];
    worker.review = Some(ReviewRecord {
        summary: "reviewed".into(),
        tip,
        reviewed_at_ms: 1,
        evidence: Some(evidence()),
    });
    let team = open_test_team(
        root.path(),
        state.path(),
        vec![reverted.clone(), held.clone(), unmerged.clone(), worker.clone()],
    )
    .await;

    let merged = team.merge(&worker.id).await.expect("merge");

    assert_eq!(merged.status, WorkerStatus::Merged);
    assert!(!branch_exists(root.path(), &worker.branch).await);
    assert!(
        branch_exists(root.path(), &reverted.branch).await,
        "a reverted merge keeps its branch"
    );
    assert!(
        branch_exists(root.path(), &held.branch).await,
        "a checkout that is still there keeps its branch"
    );
    assert!(
        branch_exists(root.path(), &unmerged.branch).await,
        "commits outside the lead branch keep the branch"
    );
}

/// A worker parked in review at one exact, reviewed tip, ready to merge.
async fn ready_to_merge(root: &std::path::Path, id: &str) -> WorkerRecord {
    let mut worker = worker(root, id);
    worker.status = WorkerStatus::InReview;
    provision_agent_worktree(
        root.to_path_buf(),
        worker.id.clone(),
        "wt/mnemo".to_string(),
    )
    .await
    .expect("provision worker");
    std::fs::write(worker.worktree.join("merged.txt"), "merged\n").expect("worker change");
    run_git(&worker.worktree, &["add", "merged.txt"]);
    run_git(&worker.worktree, &["commit", "-m", "worker change"]);
    let tip = phase3::tip(&worker.worktree).await.expect("worker tip");
    worker.checks = vec![WorkerCheck {
        name: "cargo test --workspace".into(),
        status: "passed".into(),
        detail: None,
        tip: tip.clone(),
    }];
    worker.review = Some(ReviewRecord {
        summary: "reviewed".into(),
        tip,
        reviewed_at_ms: 1,
        evidence: Some(evidence()),
    });
    worker
}

/// The regression: an earlier merged worker whose checkout an earlier merge
/// could not remove (something held it) is retried by the next merge. The
/// removal clears the note, and the existing branch sweep then deletes the
/// branch the stuck checkout was blocking.
#[tokio::test]
async fn a_later_merge_retries_an_earlier_merged_workers_stuck_checkout() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    let outside = tempfile::tempdir().expect("outside");
    init_git(root.path());
    run_git(root.path(), &["checkout", "-b", "wt/mnemo"]);
    let mut early = merged_worker(root.path(), "00000001000000000001");
    let note = format!("{WORKTREE_CLEANUP_ERROR}: could not remove leftover checkout: held");
    early.error = Some(note.clone());
    run_git(root.path(), &["branch", &early.branch, "main"]);
    let worker = ready_to_merge(root.path(), "00000002000000000002").await;
    // A link at the checkout path is refused by the cleanup, so the note
    // survives the open-time retry exactly like a checkout something held
    // on an earlier run. The worker above was provisioned first, so
    // `.worktrees` exists.
    create_directory_link(&early.worktree, outside.path());
    let team =
        open_test_team(root.path(), state.path(), vec![early.clone(), worker.clone()]).await;
    assert_eq!(
        team.worker(&early.id).await.expect("early worker").error,
        Some(note.clone()),
        "the note survives the open-time retry while the checkout is held"
    );
    // Now the checkout is removable: a plain, unregistered leftover.
    // Remove only the link itself (a junction comes off with `remove_dir`,
    // a unix symlink with `remove_file`); the outside folder is untouched.
    if std::fs::remove_dir(&early.worktree).is_err() {
        std::fs::remove_file(&early.worktree).expect("unlink the stuck checkout");
    }
    std::fs::create_dir_all(&early.worktree).expect("stuck checkout");
    std::fs::write(early.worktree.join("fragment.txt"), "leftover\n")
        .expect("leftover file");

    let merged = team.merge(&worker.id).await.expect("merge");

    assert_eq!(merged.status, WorkerStatus::Merged);
    assert!(
        !early.worktree.exists(),
        "the later merge retries and removes the stuck checkout"
    );
    assert_eq!(team.worker(&early.id).await.expect("early worker").error, None);
    assert!(
        !branch_exists(root.path(), &early.branch).await,
        "the sweep deletes the branch the stuck checkout was blocking"
    );
    assert!(!branch_exists(root.path(), &worker.branch).await);
    let persisted = TeamPersistence::new(state.path())
        .load()
        .expect("load team")
        .expect("saved team");
    let early_persisted = persisted
        .workers
        .iter()
        .find(|worker| worker.id == early.id)
        .expect("the early worker is persisted");
    assert_eq!(early_persisted.error, None);
}

/// A stuck checkout that still cannot be removed (a link outside
/// `.worktrees`) keeps its exact note, its checkout and its branch through
/// every retry, and the outside folder is never touched.
#[tokio::test]
async fn a_stuck_checkout_that_still_cannot_be_removed_keeps_its_note_and_branch() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    let outside = tempfile::tempdir().expect("outside");
    init_git(root.path());
    run_git(root.path(), &["checkout", "-b", "wt/mnemo"]);
    let mut early = merged_worker(root.path(), "00000001000000000001");
    let note = format!("{WORKTREE_CLEANUP_ERROR}: could not remove leftover checkout: held");
    early.error = Some(note.clone());
    run_git(root.path(), &["branch", &early.branch, "main"]);
    let worker = ready_to_merge(root.path(), "00000002000000000002").await;
    std::fs::write(outside.path().join("keep.txt"), "outside\n").expect("outside file");
    create_directory_link(&early.worktree, outside.path());
    let team =
        open_test_team(root.path(), state.path(), vec![early.clone(), worker.clone()]).await;

    let merged = team.merge(&worker.id).await.expect("merge");

    assert_eq!(merged.status, WorkerStatus::Merged);
    assert_eq!(
        team.worker(&early.id).await.expect("early worker").error,
        Some(note)
    );
    assert!(
        std::fs::symlink_metadata(&early.worktree).is_ok(),
        "the stuck checkout is still there"
    );
    assert_eq!(
        std::fs::read_to_string(outside.path().join("keep.txt")).expect("outside file"),
        "outside\n"
    );
    assert!(
        branch_exists(root.path(), &early.branch).await,
        "the stuck checkout keeps its branch"
    );
}

/// The retry only touches merged workers whose error is one of the
/// checkout-cleanup notes; any other error leaves the checkout alone.
#[tokio::test]
async fn the_retry_ignores_a_merged_worker_whose_error_is_not_a_checkout_note() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    run_git(root.path(), &["checkout", "-b", "wt/mnemo"]);
    let mut early = merged_worker(root.path(), "00000001000000000001");
    early.error = Some("something else needs attention".to_string());
    run_git(root.path(), &["branch", &early.branch, "main"]);
    let worker = ready_to_merge(root.path(), "00000002000000000002").await;
    let team =
        open_test_team(root.path(), state.path(), vec![early.clone(), worker.clone()]).await;
    std::fs::create_dir_all(&early.worktree).expect("leftover checkout");
    std::fs::write(early.worktree.join("fragment.txt"), "leftover\n")
        .expect("leftover file");

    let merged = team.merge(&worker.id).await.expect("merge");

    assert_eq!(merged.status, WorkerStatus::Merged);
    assert!(
        early.worktree.exists(),
        "a note that is not a checkout-cleanup note never triggers a retry"
    );
    assert_eq!(
        team.worker(&early.id).await.expect("early worker").error,
        Some("something else needs attention".to_string())
    );
}

/// App start retries a checkout a previous run could not remove, clears the
/// note and sweeps the branch the stuck checkout was blocking.
#[tokio::test]
async fn open_retries_a_stuck_checkout_left_by_an_earlier_run() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    run_git(root.path(), &["checkout", "-b", "wt/mnemo"]);
    let mut early = merged_worker(root.path(), "00000001000000000001");
    early.error = Some(format!(
        "{WORKTREE_CLEANUP_ERROR}: could not remove leftover checkout: held"
    ));
    run_git(root.path(), &["branch", &early.branch, "main"]);
    std::fs::create_dir_all(&early.worktree).expect("leftover checkout");
    std::fs::write(early.worktree.join("fragment.txt"), "leftover\n")
        .expect("leftover file");

    let team = open_test_team(root.path(), state.path(), vec![early.clone()]).await;

    assert!(
        !early.worktree.exists(),
        "the open-time retry removes the leftover checkout"
    );
    assert_eq!(team.worker(&early.id).await.expect("early worker").error, None);
    assert!(
        !branch_exists(root.path(), &early.branch).await,
        "the open-time sweep deletes the branch the stuck checkout was blocking"
    );
    let persisted = TeamPersistence::new(state.path())
        .load()
        .expect("load team")
        .expect("saved team");
    assert_eq!(persisted.workers[0].error, None);
}

/// An audit worker is never mergeable — in particular, a still-running
/// (busy) audit must be refused with the audit reason, not the generic
/// "worker must be idle" gate, which would hide what it actually is.
#[tokio::test]
async fn audit_merge_is_refused_even_while_the_audit_is_busy() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    run_git(root.path(), &["checkout", "-b", "wt/mnemo"]);
    let mut audit = worker(root.path(), "00000001000000000002");
    audit.kind = "audit".to_string();
    audit.audit_target = Some("00000001000000000001".to_string());
    audit.status = WorkerStatus::Working;
    provision_agent_worktree(
        root.path().to_path_buf(),
        audit.id.clone(),
        "wt/mnemo".to_string(),
    )
    .await
    .expect("provision the audit checkout");
    let team = open_test_team(root.path(), state.path(), vec![audit.clone()]).await;
    spawn_live_worker(&team, &audit).await;
    team.live
        .lock()
        .await
        .get(&audit.id)
        .expect("the audit is live")
        .busy
        .store(true, Ordering::SeqCst);
    assert!(team.worker_is_busy(&audit.id).await, "the audit is running");
    let before = team.worker(&audit.id).await.expect("audit record").status;

    let refused = team
        .merge(&audit.id)
        .await
        .expect_err("an audit worker never merges, busy or not");
    assert!(refused.contains("never merged"), "{refused}");
    assert_eq!(
        team.worker(&audit.id).await.expect("audit record").status,
        before
    );
    assert!(team.live.lock().await.contains_key(&audit.id));
}

#[test]
fn porcelain_worktree_paths_are_nul_delimited_and_fail_closed() {
    let paths = worktree_list_paths(b"worktree /tmp/main\0worktree /tmp/agent\0\0")
        .expect("decode porcelain paths");
    assert_eq!(
        paths,
        vec![PathBuf::from("/tmp/main"), PathBuf::from("/tmp/agent")]
    );

    #[cfg(not(unix))]
    assert!(worktree_list_paths(b"worktree \xff\0").is_err());

    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;

        assert_eq!(
            worktree_list_paths(b"worktree \xff\0").expect("Unix paths are bytes"),
            vec![PathBuf::from(std::ffi::OsString::from_vec(vec![0xff]))]
        );
    }
}

/// Mnemo1 owns `runall-*` worktrees and `wt/runall-*` branches in the same
/// `.worktrees/` directory; Mnemo2's cleanup only ever acts on its own
/// `agent-*` records and refuses anything else, even a forged record.
#[tokio::test]
async fn cleanup_never_touches_mnemo1_runall_worktrees() {
    let repo = tempfile::tempdir().expect("repo");
    let state = tempfile::tempdir().expect("state");
    let root = repo.path();
    lifecycle_git(root, &["init", "-b", "main"]);
    lifecycle_git(root, &["config", "user.name", "Test"]);
    lifecycle_git(root, &["config", "user.email", "test@example.invalid"]);
    std::fs::write(root.join("file.txt"), "base").expect("base");
    lifecycle_git(root, &["add", "file.txt"]);
    lifecycle_git(root, &["commit", "-m", "base"]);
    lifecycle_git(root, &["checkout", "-b", "wt/lead"]);
    let (runall_path, runall_branch) = mnemo::app::worktrees::provision_item_worktree(
        root.to_path_buf(),
        "abcd1234-item".to_string(),
    )
    .await
    .expect("runall worktree");
    assert!(runall_branch.starts_with("wt/runall-"), "{runall_branch}");
    assert!(runall_path.ends_with("runall-abcd1234"), "{runall_path:?}");
    let (agent_path, agent_branch) = provision_agent_worktree(
        root.to_path_buf(),
        "agent001".to_string(),
        "wt/lead".to_string(),
    )
    .await
    .expect("agent worktree");
    let agent = cleanup_worker(
        "agent001",
        &agent_branch,
        &agent_path,
        WorkerStatus::Stopped,
    );
    // A forged record pointing at the runall checkout is refused.
    let forged = cleanup_worker(
        "abcd1234",
        &runall_branch,
        &runall_path,
        WorkerStatus::Stopped,
    );
    TeamPersistence::new(state.path())
        .save(&PersistedTeam {
            workers: vec![agent, forged],
            slots: None,
            usage: ProjectUsage::default(),
        })
        .expect("ledger");

    let report = cleanup_workspace(
        root,
        state.path(),
        &state.path().join("dispatch_routing.jsonl"),
    )
    .await
    .expect("cleanup");
    assert_eq!(report.removed, vec![agent_branch.clone()]);
    assert!(!agent_path.exists());
    assert!(runall_path.is_dir(), "the Mnemo1 checkout survives");
    assert!(phase3::git_value(root, &["rev-parse", &runall_branch])
        .await
        .is_ok());
    assert!(
        report
            .skipped
            .iter()
            .any(|reason| reason.contains("invalid worker identity")),
        "{:?}",
        report.skipped
    );
}

#[test]
fn pricing_uses_uncached_input_cached_input_and_output_rates() {
    let pricing = Arc::new(std::sync::RwLock::new(vec![mnemo::config::PricingEntry {
        model: "scripted-a".into(),
        input_per_1m: 2.0,
        output_per_1m: 10.0,
        cached_per_1m: 1.0,
    }]));
    assert_eq!(
        usage_cost_micros(&pricing, Some("scripted-a"), 100, 25, 10),
        Some(275)
    );
    assert_eq!(
        usage_cost_micros(&pricing, Some("test/scripted-a"), 100, 25, 10),
        Some(275)
    );
    assert_eq!(
        usage_cost_micros(&pricing, Some("unpriced"), 100, 25, 10),
        None
    );
    assert_eq!(usage_cost_micros(&pricing, None, 100, 25, 10), None);
}

/// Complete structured review evidence for the merge-gate fixtures.
fn evidence() -> ReviewEvidence {
    ReviewEvidence {
        unanchored_pass: true,
        intent: "the worker implements the scoped brief".into(),
        execution: "the change flows through the documented path".into(),
        integration: "the module fits the surrounding runtime".into(),
        test_quality: "the regression reproduces the defect before the fix".into(),
        residual_risk: "no residual risk beyond the reviewed diff".into(),
        independent_reviewer_id: None,
        audit_adjudication: None,
    }
}

/// A worker parked in review at one exact tip with passing checks.
fn in_review_worker(evidence: Option<ReviewEvidence>) -> WorkerRecord {
    let mut worker = cleanup_worker(
        "evidence0001",
        "wt/agent-evidence0001",
        Path::new("unused"),
        WorkerStatus::InReview,
    );
    worker.checks = vec![WorkerCheck {
        name: "cargo test --workspace".into(),
        status: "passed".into(),
        detail: None,
        tip: "tip0001".into(),
    }];
    worker.review = Some(ReviewRecord {
        summary: "reviewed".into(),
        tip: "tip0001".into(),
        reviewed_at_ms: 1,
        evidence,
    });
    worker
}

#[test]
fn legacy_workers_decode_as_standard_risk_without_evidence() {
    let worker = in_review_worker(None);
    assert_eq!(worker.review_risk, "standard");
    // A pre-evidence ledger round-trips its review without the new field.
    let value = serde_json::to_value(&worker).expect("worker JSON");
    let restored: WorkerRecord =
        serde_json::from_value(value).expect("legacy-shaped worker decodes");
    assert!(restored
        .review
        .as_ref()
        .expect("review")
        .evidence
        .is_none());
}

#[test]
fn merge_gate_requires_substantive_review_evidence() {
    // A summary plus passing checks alone can no longer merge: the
    // legacy-shaped review (evidence absent) is rejected.
    let unevidenced = in_review_worker(None);
    let error = validate_merge(&unevidenced, "tip0001", &[])
        .expect_err("a legacy review must not merge without fresh evidence");
    assert!(error.contains("structured evidence"), "{error}");

    // An anchored-only pass is not substantive either.
    let mut unanchored = evidence();
    unanchored.unanchored_pass = false;
    let rejected = validate_merge(&in_review_worker(Some(unanchored)), "tip0001", &[])
        .expect_err("unanchored evidence must not merge");
    assert!(rejected.contains("unanchored"), "{rejected}");

    // Empty assessments are rejected.
    let mut empty_intent = evidence();
    empty_intent.intent = "  ".into();
    let empty = validate_merge(&in_review_worker(Some(empty_intent)), "tip0001", &[])
        .expect_err("empty intent must not merge");
    assert!(empty.contains("intent"), "{empty}");

    // Complete evidence passes the gate for standard-risk work.
    validate_merge(&in_review_worker(Some(evidence())), "tip0001", &[])
        .expect("complete evidence merges");
}

/// One audit-worker record for linkage tests.
fn audit_worker(status: WorkerStatus, target: &str, tip: &str) -> WorkerRecord {
    let mut worker = cleanup_worker(
        "auditor0001",
        "wt/agent-auditor0001",
        Path::new("unused"),
        status,
    );
    worker.kind = "audit".into();
    worker.audit_target = Some(target.into());
    worker.audit_tip = Some(tip.into());
    worker
}

#[test]
fn legacy_workers_decode_as_implementation_kind_without_audit_fields() {
    let mut worker = in_review_worker(Some(evidence()));
    worker.kind = "implementation".into();
    worker.audit_target = None;
    worker.audit_tip = None;
    // A phase-three ledger written before audits has none of the new
    // fields serialized; it must decode as a plain implementation worker.
    let legacy = json!({
        "id": "00000001000000000001",
        "name": "Worker 00000001",
        "brief": "implement the task",
        "model": "test/scripted-a",
        "model_id": "scripted-a",
        "branch": "wt/agent-00000001000000000001",
        "worktree": "",
        "status": "in_review",
        "generation": 0,
        "transcript": { "messages": [] },
        "used_tokens": 0,
        "max_tokens": 0,
        "brief_sent": true,
        "compacting": false,
        "created_at_ms": 1,
        "elapsed_ms": 1,
        "turns": 1,
        "last_activity_ms": 1,
        "checks": [],
        "findings": [],
    });
    let restored: WorkerRecord =
        serde_json::from_value(legacy).expect("pre-audit ledger decodes");
    assert_eq!(restored.kind, "implementation");
    assert!(restored.audit_target.is_none());
    assert!(restored.audit_tip.is_none());
    assert!(restored.reasoning_effort.is_none());
    // A modern audit record keeps its fields through a round-trip.
    let audit = audit_worker(WorkerStatus::Completed, "evidence0001", "tip0001");
    let audit_value = serde_json::to_value(&audit).expect("audit JSON");
    assert!(audit_value.get("kind").and_then(Value::as_str) == Some("audit"));
    let restored_audit: WorkerRecord =
        serde_json::from_value(audit_value).expect("audit worker round-trips");
    assert_eq!(restored_audit.kind, "audit");
    assert_eq!(restored_audit.audit_target.as_deref(), Some("evidence0001"));
    assert_eq!(restored_audit.audit_tip.as_deref(), Some("tip0001"));
}

#[test]
fn high_risk_merge_gate_requires_a_fresh_completed_audit() {
    let records_empty: Vec<WorkerRecord> = Vec::new();

    // High-risk work cannot merge on the lead's evidence alone.
    let mut high = in_review_worker(Some(evidence()));
    high.review_risk = "high".into();
    let missing = validate_merge(&high, "tip0001", &records_empty)
        .expect_err("high-risk work needs an independent audit");
    assert!(
        missing.contains("high-risk reviews require an independent reviewer"),
        "{missing}"
    );

    // A linked adjudication without the tracked audit is refused.
    let mut untracked_evidence = evidence();
    untracked_evidence.independent_reviewer_id = Some("auditor0001".into());
    untracked_evidence.audit_adjudication = Some("accepted".into());
    let mut untracked_worker = in_review_worker(Some(untracked_evidence));
    untracked_worker.review_risk = "high".into();
    let untracked = validate_merge(&untracked_worker, "tip0001", &records_empty)
        .expect_err("the linked reviewer must be a tracked worker");
    assert!(untracked.contains("not a tracked worker"), "{untracked}");

    // Linking a tracked implementation worker is refused: only
    // read-only audit workers may be linked.
    let mut implementer = cleanup_worker(
        "auditor0001",
        "wt/agent-auditor0001",
        Path::new("unused"),
        WorkerStatus::Completed,
    );
    implementer.kind = "implementation".into();
    let wrong_kind = validate_merge(&untracked_worker, "tip0001", &[implementer])
        .expect_err("an implementation worker is not an independent audit");
    assert!(wrong_kind.contains("not a read-only audit worker"), "{wrong_kind}");

    // An audit still working cannot be linked.
    let mut live = audit_worker(WorkerStatus::Working, "evidence0001", "tip0001");
    live.status = WorkerStatus::Working;
    let incomplete = validate_merge(&untracked_worker, "tip0001", &[live])
        .expect_err("an unfinished audit must not merge");
    assert!(incomplete.contains("not completed"), "{incomplete}");

    // An audit of another worker is refused.
    let foreign = audit_worker(WorkerStatus::Completed, "other0001", "tip0001");
    let mismatched = validate_merge(&untracked_worker, "tip0001", &[foreign])
        .expect_err("a foreign audit must not link");
    assert!(mismatched.contains("reviews 'other0001'"), "{mismatched}");

    // An audit at an older tip is stale after a fix commit moved it.
    let stale = audit_worker(WorkerStatus::Completed, "evidence0001", "tip0000");
    let stale_error = validate_merge(&untracked_worker, "tip0001", &[stale])
        .expect_err("a stale audit must block the merge");
    assert!(stale_error.contains("stale"), "{stale_error}");

    // Linkage without a written adjudication is refused even for
    // standard-risk work.
    let mut unadjudicated = evidence();
    unadjudicated.independent_reviewer_id = Some("auditor0001".into());
    unadjudicated.audit_adjudication = None;
    let standard = in_review_worker(Some(unadjudicated));
    let unadjudicated_error = validate_merge(
        &standard,
        "tip0001",
        &[audit_worker(WorkerStatus::Completed, "evidence0001", "tip0001")],
    )
    .expect_err("linking an audit requires an adjudication");
    assert!(
        unadjudicated_error.contains("audit_adjudication"),
        "{unadjudicated_error}"
    );

    // A completed audit at the exact merge tip plus a written
    // adjudication merges.
    let mut adjudicated = evidence();
    adjudicated.independent_reviewer_id = Some("auditor0001".into());
    adjudicated.audit_adjudication = Some("accepted".into());
    let mut reviewed = in_review_worker(Some(adjudicated));
    reviewed.review_risk = "high".into();
    validate_merge(
        &reviewed,
        "tip0001",
        &[audit_worker(WorkerStatus::Completed, "evidence0001", "tip0001")],
    )
    .expect("a fresh completed audit with an adjudication merges");
}

#[test]
fn high_risk_dispatch_requires_observable_acceptance_scenarios() {
    let no_section =
        require_acceptance_scenarios("planid01", "# Plan\n\n## Goal\nShip it.\n")
            .expect_err("a plan without the section must block high-risk dispatch");
    assert!(no_section.contains("plan 'planid01' does not declare"), "{no_section}");
    assert!(no_section.contains("add_acceptance_scenarios"), "{no_section}");

    // A heading the board escaped into the context is prose, not the
    // plan's section.
    let escaped = require_acceptance_scenarios(
        "planid01",
        "# Plan\n\n## Context\n\\## Acceptance scenarios\n- A restart reloads every record.\n",
    )
    .expect_err("an escaped heading must not count as the section");
    assert!(escaped.contains("does not declare"), "{escaped}");

    let no_bullet = require_acceptance_scenarios(
        "planid01",
        "# Plan\n\n## Acceptance scenarios\n\n- x\n\nNone yet.\n",
    )
    .expect_err("a section without observable bullets must block high-risk dispatch");
    assert!(no_bullet.contains("no observable bullet"), "{no_bullet}");

    require_acceptance_scenarios(
        "planid01",
        "# Plan\n\n## Acceptance scenarios\n\n- A standard worker cannot merge with only a summary and passing checks.\n\n## Steps\n- done\n",
    )
    .expect("observable bullets validate high-risk dispatch");
}

#[test]
fn high_risk_dispatch_accepts_complete_openspec_delta_scenarios() {
    require_acceptance_scenarios(
        "planid01",
        "# Plan\n\n## Spec changes\n\n### importer\n\n#### ADDED Requirements\n\n##### Requirement: Rows survive\nThe system SHALL retain rows.\n\n###### Scenario: Contract exercised\n- **WHEN** rows are imported\n- **THEN** every row remains visible\n",
    )
    .expect("a complete OpenSpec scenario satisfies high-risk dispatch");
}

#[test]
fn high_risk_dispatch_refuses_spec_changes_without_complete_scenarios() {
    require_acceptance_scenarios(
        "planid01",
        "# Plan\n\n## Spec changes\n\n### importer\n\n#### REMOVED Requirements\n\n##### Requirement: Old behavior\nNo longer needed.\n",
    )
    .expect_err("a Spec changes section without scenarios must not pass");

    require_acceptance_scenarios(
        "planid01",
        "# Plan\n\n## Spec changes\n\n### importer\n\n#### MODIFIED Requirements\n\n##### Requirement: Rows survive\nThe system SHALL retain rows.\n\n###### Scenario: Incomplete\n- **WHEN** rows are imported\n",
    )
    .expect_err("a scenario without THEN must not pass");
}

/// The parsed answer of one in-app lead `team_status` call.
async fn team_status_answer(team: &TeamRuntime, args: Value) -> Value {
    let result = team.handle_lead_host_tool("team_status", args).await;
    assert!(result.success, "{}", result.output);
    serde_json::from_str(&result.output).expect("team_status answers with JSON")
}

fn reviewed_worker(root: &std::path::Path, id: &str) -> WorkerRecord {
    let mut worker = worker(root, id);
    worker.brief = "the whole brief".to_string();
    worker.opening_prompt = Some("the whole opening prompt".to_string());
    worker.model = Some("test/scripted-a".to_string());
    worker.model_id = Some("scripted-a".to_string());
    worker.plan_id = Some("plan0001".to_string());
    worker.plan_step = Some(2);
    worker.tier = Some("routine".to_string());
    worker.status = WorkerStatus::InReview;
    worker.error = Some("a recorded error".to_string());
    worker.cost_micros = Some(1234);
    worker.last_activity_ms = 42;
    worker.findings = vec![
        Finding {
            id: "finding1".to_string(),
            severity: "high".to_string(),
            title: "the open one".to_string(),
            detail: "the whole finding detail".to_string(),
            file: "src/a.rs".to_string(),
            line: 7,
            status: "open".to_string(),
            fix_commit: None,
            evidence: None,
            justification: None,
        },
        Finding {
            id: "finding2".to_string(),
            severity: "low".to_string(),
            title: "the fixed one".to_string(),
            detail: "the closed finding detail".to_string(),
            file: "src/b.rs".to_string(),
            line: 9,
            status: "fixed".to_string(),
            fix_commit: Some("1234567890abcdef".to_string()),
            evidence: None,
            justification: None,
        },
    ];
    worker.checks = vec![WorkerCheck {
        name: "cargo test".to_string(),
        status: "passed".to_string(),
        detail: Some("the whole check output".to_string()),
        tip: "0123456789abcdef".to_string(),
    }];
    worker.review = Some(ReviewRecord {
        summary: "looks right".to_string(),
        tip: "0123456789abcdef".to_string(),
        reviewed_at_ms: 5,
        evidence: Some(ReviewEvidence {
            unanchored_pass: true,
            intent: "the whole intent".to_string(),
            execution: "the whole execution".to_string(),
            integration: "the whole integration".to_string(),
            test_quality: "the whole test quality".to_string(),
            residual_risk: "the whole residual risk".to_string(),
            independent_reviewer_id: None,
            audit_adjudication: None,
        }),
    });
    worker
}

fn merged_worker(root: &std::path::Path, id: &str) -> WorkerRecord {
    let mut worker = worker(root, id);
    worker.status = WorkerStatus::Merged;
    worker.merge = Some(MergeRecord {
        commit: "abcdef0123456789".to_string(),
        worker_tip: "0123456789abcdef".to_string(),
        branch: agent_branch(id),
        target: "wt/agent-lead".to_string(),
        checks: vec!["cargo test".to_string()],
        review_summary: "looks right".to_string(),
        review_risk: Some("standard".to_string()),
        review_evidence: None,
        files: 1,
        additions: 2,
        deletions: 3,
        worker_model: Some("scripted-a".to_string()),
        worker_tokens: 10,
        cost_micros: Some(5),
        merged_at_ms: 7,
        revert_commit: None,
    });
    worker
}

/// `team_status` answers compactly by default — the answer lands in the
/// lead's own context — and with the whole record on `detail`.
#[tokio::test]
async fn team_status_is_compact_by_default_and_full_on_detail() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let reviewed = reviewed_worker(root.path(), "00000001000000000001");
    let merged = merged_worker(root.path(), "00000001000000000002");
    let team = open_test_team(root.path(), state.path(), vec![reviewed, merged]).await;

    let answer = team_status_answer(&team, json!({})).await;
    assert_eq!(answer["action"], "team_status");
    let entries = answer["result"].as_array().expect("a worker array");
    assert_eq!(entries.len(), 2);
    let entry = &entries[0];
    assert_eq!(entry["id"], "00000001000000000001");
    assert_eq!(entry["status"], "in_review");
    assert_eq!(entry["kind"], "implementation");
    assert_eq!(entry["tier"], "routine");
    assert_eq!(entry["model"], "scripted-a");
    assert_eq!(entry["plan_id"], "plan0001");
    assert_eq!(entry["plan_step"], 2);
    assert_eq!(entry["branch"], agent_branch("00000001000000000001"));
    assert_eq!(entry["error"], "a recorded error");
    assert_eq!(entry["cost_micros"], 1234);
    assert_eq!(entry["last_activity_ms"], 42);
    assert!(entry["pending"].is_null(), "{entry}");
    for oversized in ["brief", "opening_prompt", "inbox", "transcript"] {
        assert!(
            entry.get(oversized).is_none(),
            "the compact entry carries {oversized}: {entry}"
        );
    }
    // Only the open finding, and only its identity.
    let findings = entry["findings"].as_array().expect("findings");
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0]["id"], "finding1");
    assert_eq!(findings[0]["severity"], "high");
    assert_eq!(findings[0]["title"], "the open one");
    assert_eq!(findings[0]["file"], "src/a.rs");
    assert_eq!(findings[0]["line"], 7);
    assert!(
        findings[0].get("detail").is_none(),
        "the compact finding carries its detail: {}",
        findings[0]
    );
    // Checks keep their verdict and tip prefix, not the output.
    let checks = entry["checks"].as_array().expect("checks");
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0]["name"], "cargo test");
    assert_eq!(checks[0]["status"], "passed");
    assert_eq!(checks[0]["tip"], "01234567");
    assert!(
        checks[0].get("detail").is_none(),
        "the compact check carries its output: {}",
        checks[0]
    );
    // The review keeps its summary and tip, not the evidence.
    assert_eq!(entry["review"]["summary"], "looks right");
    assert_eq!(entry["review"]["tip"], "01234567");
    assert!(
        entry["review"].get("evidence").is_none(),
        "the compact review carries its evidence: {}",
        entry["review"]
    );
    // A merged worker names its merge commit and carries no review block.
    let merged_entry = &entries[1];
    assert_eq!(merged_entry["status"], "merged");
    assert_eq!(merged_entry["merge"]["commit"], "abcdef0123456789");
    assert!(merged_entry.get("review").is_none(), "{merged_entry}");

    // `detail` returns the record as before, transcripts aside.
    let full = team_status_answer(&team, json!({"detail": true})).await;
    let entries = full["result"].as_array().expect("a worker array");
    assert_eq!(entries.len(), 2);
    let entry = &entries[0];
    assert_eq!(entry["brief"], "the whole brief");
    assert_eq!(entry["opening_prompt"], "the whole opening prompt");
    assert_eq!(entry["checks"][0]["detail"], "the whole check output");
    assert_eq!(entry["findings"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        entry["review"]["evidence"]["residual_risk"],
        "the whole residual risk"
    );
    assert!(
        entry.get("transcript").is_none(),
        "transcripts stay out: {entry}"
    );

    // The worker_id filter works in both forms; an unknown id still fails.
    for detail in [false, true] {
        let answer = team_status_answer(
            &team,
            json!({"worker_id": "00000001000000000002", "detail": detail}),
        )
        .await;
        let entries = answer["result"].as_array().expect("a worker array");
        assert_eq!(entries.len(), 1, "detail {detail}: {answer}");
        assert_eq!(entries[0]["id"], "00000001000000000002");
    }
    let missing = team
        .handle_lead_host_tool("team_status", json!({"worker_id": "nope"}))
        .await;
    assert!(!missing.success, "an unknown worker id fails: {}", missing.output);
    team.shutdown().await;
}

/// Every team tool answers with the same compact worker entry the lead
/// already reads from `team_status`; the whole record stays in `data`.
#[tokio::test]
async fn team_tools_answer_with_the_compact_worker_entry() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    // A cleaned-up worker keeps its branch, so a check resolves the tip
    // without a live checkout.
    let id = "00000001000000000001";
    let mut worker = reviewed_worker(root.path(), id);
    worker.status = WorkerStatus::Cancelled;
    worker.worktree = std::path::PathBuf::new();
    worker.checks.clear();
    run_git(root.path(), &["branch", &agent_branch(id)]);
    let team = open_test_team(root.path(), state.path(), vec![worker]).await;

    let answer = team
        .handle_lead_host_tool(
            "team_check",
            json!({
                "worker_id": id,
                "name": "cargo test --workspace",
                "status": "passed",
                "detail": "the whole check output again",
            }),
        )
        .await;
    assert!(answer.success, "{}", answer.output);
    let receipt: Value = serde_json::from_str(&answer.output).expect("JSON receipt");
    assert_eq!(receipt["action"], "team_check");
    let entry = &receipt["result"];
    assert_eq!(entry["id"], id);
    assert_eq!(entry["status"], "cancelled");
    // Status, checks and findings stay; the oversized fields do not.
    let checks = entry["checks"].as_array().expect("checks");
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0]["name"], "cargo test --workspace");
    assert_eq!(checks[0]["status"], "passed");
    assert_eq!(checks[0]["tip"].as_str().map(str::len), Some(8), "{entry}");
    assert!(
        checks[0].get("detail").is_none(),
        "the compact check carries its output: {}",
        checks[0]
    );
    let findings = entry["findings"].as_array().expect("findings");
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0]["id"], "finding1");
    for oversized in ["brief", "opening_prompt", "inbox", "transcript"] {
        assert!(
            entry.get(oversized).is_none(),
            "the team tool answer carries {oversized}: {entry}"
        );
    }
    // The whole record stays in `data` for callers that need it.
    let data = answer.data.as_ref().expect("structured data");
    assert_eq!(data["brief"], "the whole brief");
    assert_eq!(data["opening_prompt"], "the whole opening prompt");

    team.shutdown().await;
}

/// A release forgets every helper that led, so a helper that closes after
/// one cannot keep a stale claim alive, and a detached lead remembers no
/// new helper until it attaches again.
#[tokio::test]
async fn a_release_forgets_every_external_bridge() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let team = open_test_team(root.path(), state.path(), Vec::new()).await;

    // Two helpers led; one closing must leave the other in charge.
    assert!(team.attach_external_lead(), "the first attach takes the team");
    team.note_external_bridge("a");
    team.note_external_bridge("b");
    assert_eq!(team.forget_external_bridge("a"), 1, "b still leads");

    // A release forgets every helper, and none is noted while detached.
    assert!(team.detach_external_lead());
    team.note_external_bridge("c");
    assert_eq!(team.forget_external_bridge("b"), 0, "the release forgot b");
    assert_eq!(team.forget_external_bridge("c"), 0, "a detached lead notes no helper");

    // The next attach starts with no remembered helpers.
    assert!(team.attach_external_lead(), "it can lead again");
    assert_eq!(team.forget_external_bridge("a"), 0, "the release had cleared the set");
    team.shutdown().await;
}

/// `team_status` says who leads: the in-app lead, an attached Claude Code
/// session, or one that was released for silence.
#[tokio::test]
async fn team_status_names_the_lead_and_whether_it_is_attached() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let team = open_test_team(root.path(), state.path(), Vec::new()).await;

    // No external lead: Mnemo2's own lead leads.
    let answer = team_status_answer(&team, json!({})).await;
    assert_eq!(
        answer["lead"],
        json!({"kind": "in_app", "attached": false, "since_ms": null, "idle_since_ms": null}),
        "{answer}"
    );

    // An attached Claude Code session leads.
    assert!(team.attach_external_lead(), "the first attach takes the team");
    let answer = team_status_answer(&team, json!({})).await;
    assert_eq!(answer["lead"]["kind"], "external", "{answer}");
    assert_eq!(answer["lead"]["attached"], true, "{answer}");
    assert!(answer["lead"]["since_ms"].as_u64().is_some(), "{answer}");
    assert!(answer["lead"]["idle_since_ms"].is_null(), "{answer}");

    // Released for silence: still the external session, no longer attached.
    assert!(team.detach_external_lead());
    team.mark_external_lead_idle(4242);
    let answer = team_status_answer(&team, json!({})).await;
    assert_eq!(answer["lead"]["kind"], "external", "{answer}");
    assert_eq!(answer["lead"]["attached"], false, "{answer}");
    assert!(answer["lead"]["since_ms"].is_null(), "{answer}");
    assert_eq!(answer["lead"]["idle_since_ms"], 4242, "{answer}");
    team.shutdown().await;
}

/// The Plans tab reads a plan's markdown through the team. An id no board
/// row names is refused before any plan file is touched, so a stray file
/// in `.coding/plans/` can never leak out through the command.
#[tokio::test]
async fn plan_document_returns_the_markdown_and_refuses_unknown_ids() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let plans_dir = root.path().join(".coding/plans");
    std::fs::create_dir_all(&plans_dir).expect("plans dir");
    let board = PlanBoard::new(&plans_dir, root.path().join(".coding/reviews"));
    let plan = board
        .create(mnemo::app::plan_board::PlanDraft {
            title: "Plans tab".into(),
            goal: "Show the whole plan".into(),
            context: "verified: the card hides the context; src-tauri/src/mnemo2/ipc.rs and mnemo2/src/components/PlanTab.svelte carry the fix; verify with cargo test.".into(),
            steps: vec!["**Expose** the document in src-tauri/src/mnemo2/ipc.rs".into()],

            acceptance: vec!["Details shows the context".into()],
            ..Default::default()
        })
        .expect("create the plan");
    let stray = "deadbeef";
    std::fs::write(
        plans_dir.join(format!("{stray}.md")),
        "# Plan: stray\n\n## Context\n\nunreachable\n",
    )
    .expect("stray plan file");
    let team = open_test_team(root.path(), state.path(), Vec::new()).await;
    let team = Arc::new(team);
    team.attach_board(Arc::new(board));

    let document = team.plan_document(&plan.id).expect("the plan document");
    assert!(document.contains("## Context"), "{document}");
    assert!(document.contains("verified: the card hides the context"), "{document}");

    let refused = team
        .plan_document(stray)
        .expect_err("an id no board row names is refused");
    assert!(refused.contains(stray), "{refused}");
    team.shutdown().await;
}

/// The lead's `team_dispatch` schema offers a read-back first turn for
/// large or subtle specs, as a boolean opt-in.
#[tokio::test]
async fn team_dispatch_schema_offers_read_back() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let team = open_test_team(root.path(), state.path(), Vec::new()).await;

    let dispatch = team
        .lead_host_tools()
        .into_iter()
        .find(|tool| tool.name == "team_dispatch")
        .expect("team_dispatch");
    let read_back = &dispatch.parameters["properties"]["read_back"];
    assert_eq!(read_back["type"], json!("boolean"), "{read_back}");
    let description = read_back["description"]
        .as_str()
        .expect("a read_back description");
    assert!(description.contains("No questions."), "{description}");
    assert!(description.contains("waits for the lead's reply"), "{description}");
    team.shutdown().await;
}

#[tokio::test]
async fn team_dispatch_refuses_unmet_openspec_apply_requirements() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let team = open_test_team(root.path(), state.path(), Vec::new()).await;
    let openspec = root.path().join(".coding/openspec");
    let templates = openspec.join("schemas/custom/templates");
    std::fs::create_dir_all(&templates).expect("schema templates");
    for id in ["proposal", "research", "tasks"] {
        std::fs::write(templates.join(format!("{id}.md")), format!("# {id}\n"))
            .expect("template");
    }
    std::fs::write(
        openspec.join("schemas/custom/schema.yaml"),
        "name: custom\nversion: 1\nartifacts:\n  - {id: proposal, generates: proposal.md, template: proposal.md}\n  - {id: research, generates: research.md, template: research.md, instruction: Record evidence.}\n  - {id: tasks, generates: tasks.md, template: tasks.md}\napply: {requires: [research], tracks: tasks.md}\n",
    )
    .expect("schema.yaml");
    std::fs::write(
        openspec.join("config.yaml"),
        "schema: custom\nrules:\n  research: Cite the source.\n",
    )
    .expect("config.yaml");
    let board = PlanBoard::new(
        root.path().join(".coding/plans"),
        root.path().join(".coding/reviews"),
    );
    let plan = board
        .create(mnemo::app::plan_board::PlanDraft {
            title: "Needs research".to_string(),
            goal: "Implement safely".to_string(),
            context: "Symptom: dispatch lacks artifact gates; src-tauri/src/mnemo2/team.rs validates the plan; verify with cargo test.".to_string(),
            steps: vec!["Implement in src-tauri/src/mnemo2/team.rs and run cargo test".to_string()],
            ..Default::default()
        })
        .expect("creation is allowed before apply requirements are met");
    let team = Arc::new(team);
    team.attach_board(Arc::new(board));

    let error = team
        .dispatch_request(DispatchRequest {
            brief: "Implement the plan".to_string(),
            tier: None,
            dispatch_routing: false,
            source: DispatchSource::Ipc,
            model: None,
            reasoning_effort: None,
            worker_id: None,
            plan: Some(PlanRef {
                plan_id: plan.id.clone(),
                step: 0,
            }),
            review_risk: None,
            read_back: false,
        })
        .await
        .expect_err("apply.requires must refuse dispatch");
    assert!(error.contains("cannot be dispatched"), "{error}");
    assert!(error.contains("artifact 'research'"), "{error}");
    assert!(error.contains("Cite the source."), "{error}");
    assert!(team.records.lock().await.is_empty());
    team.shutdown().await;
}

/// Idempotent re-dispatch compares `read_back` like the brief: the same
/// worker id with a different value is refused rather than silently
/// re-pinned.
#[tokio::test]
async fn redispatch_with_a_different_read_back_is_refused() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let mut existing = worker(root.path(), "00000001000000000001");
    existing.brief = "BRIEF-91x".to_string();
    let team = open_test_team(root.path(), state.path(), vec![existing.clone()]).await;

    let (reused, fresh, _) = team
        .dispatch_request(DispatchRequest {
            brief: "BRIEF-91x".to_string(),
            tier: None,
            dispatch_routing: false,
        source: DispatchSource::Ipc,
            model: None,
            reasoning_effort: None,
            worker_id: Some(existing.id.clone()),
            plan: None,
            review_risk: None,
            read_back: false,
        })
        .await
        .expect("the identical re-dispatch reuses the worker");
    assert!(fresh, "the identical re-dispatch is a reuse");
    assert_eq!(reused.id, existing.id);

    let refused = team
        .dispatch_request(DispatchRequest {
            brief: "BRIEF-91x".to_string(),
            tier: None,
            dispatch_routing: false,
        source: DispatchSource::Ipc,
            model: None,
            reasoning_effort: None,
            worker_id: Some(existing.id.clone()),
            plan: None,
            review_risk: None,
            read_back: true,
        })
        .await
        .expect_err("a different read_back is refused");
    assert!(refused.contains("read_back"), "{refused}");
    team.shutdown().await;
}

/// An open high finding for the round counters.
fn open_finding(id: &str) -> Finding {
    Finding {
        id: id.to_string(),
        severity: "high".to_string(),
        title: "the unchecked path".to_string(),
        detail: "the whole finding detail".to_string(),
        file: "src/a.rs".to_string(),
        line: 7,
        status: "open".to_string(),
        fix_commit: None,
        evidence: None,
        justification: None,
    }
}

struct ReviewFixture {
    _root: tempfile::TempDir,
    state: tempfile::TempDir,
    _config: tempfile::TempDir,
    team: TeamRuntime,
    record: WorkerRecord,
}

impl ReviewFixture {
    fn rows(&self) -> Vec<DispatchRoutingRow> {
        std::fs::read_to_string(self.state.path().join("dispatch_routing.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).expect("routing row"))
            .collect()
    }
}

async fn review_fixture(
    mode: crate::mnemo2::dispatch_routing::RoutingMode,
    id: &str,
) -> ReviewFixture {
    use crate::mnemo2::dispatch_routing::{save_routing_settings, RoutingSettings};
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    let config = tempfile::tempdir().expect("config");
    init_git(root.path());
    save_routing_settings(
        config.path(),
        &RoutingSettings {
            mode,
            ..RoutingSettings::default()
        },
    )
    .expect("routing settings");
    let mut record = worker(root.path(), id);
    record.model = Some("test/scripted-difficult".into());
    record.tier = Some("high".into());
    record.routing = Some(WorkerRouting {
        laya_tier: Some("high".into()),
        confidence: Some(0.8),
        cleared: true,
        lead_tier: Some("high".into()),
        routed_by: "lead".into(),
        ran_tier: Some("high".into()),
        nudged: None,
    });
    provision_agent_worktree(
        root.path().to_path_buf(),
        record.id.clone(),
        "main".to_string(),
    )
    .await
    .expect("worker checkout");
    let team = open_test_team_with_config(
        root.path(),
        state.path(),
        vec![record.clone()],
        config.path(),
    )
    .await;
    let mut tiers = tiers_dto! {
        difficult: vec!["test/scripted-difficult".into()],
        average: vec!["test/scripted-average".into()],
        simple: vec!["test/scripted-simple".into()],
    };
    tiers.tiers[4].models = vec!["test/scripted-escalate".into()];
    team.set_catalog(
        vec![
            lead_model("scripted-difficult"),
            lead_model("scripted-average"),
            lead_model("scripted-simple"),
            lead_model("scripted-escalate"),
        ],
        tiers,
        Vec::new(),
    );
    ReviewFixture {
        _root: root,
        state,
        _config: config,
        team,
        record,
    }
}

/// One configured model in the team's catalog, so `change_model` accepts
/// the spec the test endpoint also serves.
fn lead_model(model: &str) -> LeadModel {
    LeadModel {
        endpoint: "test".to_string(),
        model: model.to_string(),
        spec: format!("test/{model}"),
        is_default: model == "scripted-a",
        reasoning_efforts: Vec::new(),
        default_effort: None,
    }
}

/// Every review of a worker is a review round, and a finding routed back
/// to it is a fix round (plan 40eef714 step 4).
#[tokio::test]
async fn review_and_fix_rounds_are_counted() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let record = worker(root.path(), "rounds000001");
    provision_agent_worktree(
        root.path().to_path_buf(),
        record.id.clone(),
        "main".to_string(),
    )
    .await
    .expect("provision the worker checkout");
    let team = open_test_team(root.path(), state.path(), vec![record.clone()]).await;

    let first = team
        .review(
            &record.id,
            "first pass".into(),
            vec![open_finding("finding1")],
            evidence(),
        )
        .await
        .expect("record the first review");
    assert_eq!(first.review_rounds, 1, "{first:?}");
    assert_eq!(first.fix_rounds, 0, "{first:?}");

    let routed = team
        .route_fix(&record.id, "finding1")
        .await
        .expect("route the finding back to the worker");
    assert_eq!(routed.review_rounds, 1, "{routed:?}");
    assert_eq!(routed.fix_rounds, 1, "{routed:?}");

    // The test endpoint cannot serve the fixer turn; freeze the session and
    // review the still-open finding again: that is review round two.
    team.stop_inner(&record.id).await.expect("stop the worker");
    let second = team
        .review(
            &record.id,
            "second pass".into(),
            vec![open_finding("finding1")],
            evidence(),
        )
        .await
        .expect("record the second review");
    assert_eq!(second.review_rounds, 2, "{second:?}");
    assert_eq!(second.fix_rounds, 1, "{second:?}");
    team.shutdown().await;
}

/// An independent audit is counted on the worker it audits, never on the
/// audit worker itself (plan 40eef714 step 4).
#[tokio::test]
async fn audit_dispatch_counts_on_the_target() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let plans_dir = root.path().join(".coding/plans");
    std::fs::create_dir_all(&plans_dir).expect("plans dir");
    let board = PlanBoard::new(&plans_dir, root.path().join(".coding/reviews"));
    let plan = board
        .create(mnemo::app::plan_board::PlanDraft {
            title: "Audited plan".into(),
            goal: "Count the audits".into(),
            context: "verified: src-tauri/src/mnemo2/team.rs records the audit round.".into(),
            steps: vec!["**Count** the audits in src-tauri/src/mnemo2/team.rs".into()],
            acceptance: vec!["The audited worker records the audit".into()],
            ..Default::default()
        })
        .expect("create the plan");
    let mut record = worker(root.path(), "audited00001");
    record.plan_id = Some(plan.id.clone());
    record.plan_step = Some(0);
    provision_agent_worktree(
        root.path().to_path_buf(),
        record.id.clone(),
        "main".to_string(),
    )
    .await
    .expect("provision the worker checkout");
    let team = open_test_team(root.path(), state.path(), vec![record.clone()]).await;
    let team = Arc::new(team);
    team.attach_board(Arc::new(board));

    let (audit, reused) = team
        .audit_dispatch(&record.id, "auditworker1", None, None, None)
        .await
        .expect("dispatch the audit");
    assert!(!reused, "a fresh audit is not a reuse");
    assert_eq!(audit.kind, "audit", "{audit:?}");
    assert_eq!(audit.audits, 0, "the count lands on the target: {audit:?}");
    let target = team.worker(&record.id).await.expect("the audited worker");
    assert_eq!(target.audits, 1, "{target:?}");
    team.shutdown().await;
}

/// An audit that reported ends its own session (plan 5779641): the
/// slot it held is freed the moment its turn ends, without the lead
/// stopping it, and its report is already recorded when that happens.
#[tokio::test]
async fn finished_audit_releases_its_slot() {
    let (fixture, _notices) = nudge_fixture_with(
        crate::mnemo2::dispatch_routing::RoutingSettings::default(),
        None,
        std::time::Duration::ZERO,
        vec![
            // The audit reports through message_parent, then ends its
            // turn — exactly what audit_prompt() tells it to do.
            tool_call_batch(
                "audit-report",
                "message_parent",
                r#"{"text":"Read-only audit: no findings; the change matches its plan."}"#,
            ),
            vec![
                mnemo::provider::LlmEvent::TextDelta {
                    text: "reported".to_string(),
                },
                mnemo::provider::LlmEvent::Finish {
                    reason: mnemo::provider::FinishReason::Stop,
                },
            ],
        ],
    )
    .await;
    let team = &fixture.team;
    // One slot: the audit itself is the only worker that may hold it.
    team.persistence.slots.store(1, Ordering::SeqCst);
    // The audited implementation worker: an idle record with a live
    // checkout, dispatched on the fixture's board plan.
    let mut target = worker(fixture._root.path(), "target00001");
    target.plan_id = Some(fixture.plan_id.clone());
    target.plan_step = Some(0);
    provision_agent_worktree(
        fixture._root.path().to_path_buf(),
        target.id.clone(),
        "main".to_string(),
    )
    .await
    .expect("provision the target checkout");
    team.records.lock().await.push(target.clone());

    let (audit, reused) = team
        .audit_dispatch(&target.id, "auditworker1", None, None, None)
        .await
        .expect("dispatch the audit into the one slot");
    assert!(!reused, "a fresh audit is not a reuse");
    let audit_id = audit.id.clone();
    let finished = eventually(|| async {
        let record = team.worker(&audit_id).await.ok()?;
        (record.status == WorkerStatus::Completed).then_some(record)
    })
    .await;
    // The report was recorded before the session ended.
    assert!(finished.report_delivered, "{finished:?}");
    assert!(
        team.messages()
            .await
            .iter()
            .any(|message| message.worker_id == audit_id),
        "the audit's report is recorded for the lead"
    );
    // The finished audit no longer counts against the slots.
    assert_eq!(
        team.live.lock().await.len(),
        0,
        "the finished audit released its slot"
    );
    // The freed slot dispatches a fresh audit with no manual stop
    // (a different id prefix: the finished audit's branch stays).
    team.audit_dispatch(&target.id, "audittwo001", None, None, None)
        .await
        .expect("the released slot dispatches a fresh audit");
    team.shutdown().await;
}

/// A finished audit never comes back (plan 5779641): resume, compact,
/// model change and message are all refused with the same words.
#[tokio::test]
async fn finished_audit_cannot_be_resumed() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let mut audit = worker(root.path(), "auditworker1");
    audit.kind = "audit".to_string();
    audit.status = WorkerStatus::Completed;
    audit.report_delivered = true;
    let team = open_test_team(root.path(), state.path(), vec![audit]).await;
    let refusal = "audit worker 'auditworker1' already delivered its report; dispatch a new audit with team_audit_dispatch";
    let error = team
        .resume("auditworker1")
        .await
        .expect_err("resume refuses a finished audit");
    assert_eq!(error, refusal);
    let error = team
        .compact("auditworker1")
        .await
        .expect_err("compact refuses a finished audit");
    assert_eq!(error, refusal);
    let error = team
        .change_model("auditworker1", None, None, ChangeSource::User)
        .await
        .expect_err("change_model refuses a finished audit");
    assert_eq!(error, refusal);
    let error = team
        .message(
            "auditworker1",
            "one more look at the diff".to_string(),
            MessageSender::User,
        )
        .await
        .expect_err("message refuses a finished audit");
    assert_eq!(error, refusal);
    team.set_catalog(
        vec![lead_model("scripted-difficult")],
        tiers_dto! {
            difficult: vec!["test/scripted-difficult".into()],
            average: Vec::new(),
            simple: Vec::new(),
        },
        Vec::new(),
    );
    let result = team
        .handle_lead_host_tool(
            "team_resume",
            json!({"worker_id": "auditworker1", "tier": "high"}),
        )
        .await;
    assert!(!result.success);
    assert!(result.output.contains(refusal), "{}", result.output);
    team.shutdown().await;
}

/// The other side of the coin: an audit that failed before it reported
/// keeps today's resume path — the lead can still bring it back.
#[tokio::test]
async fn audit_that_failed_before_reporting_can_still_be_resumed() {
    let (fixture, _notices) = nudge_fixture_with(
        crate::mnemo2::dispatch_routing::RoutingSettings::default(),
        None,
        std::time::Duration::ZERO,
        vec![vec![
            mnemo::provider::LlmEvent::TextDelta {
                text: "done".to_string(),
            },
            mnemo::provider::LlmEvent::Finish {
                reason: mnemo::provider::FinishReason::Stop,
            },
        ]],
    )
    .await;
    let team = &fixture.team;
    let mut audit = worker(fixture._root.path(), "auditworker1");
    audit.kind = "audit".to_string();
    audit.status = WorkerStatus::Failed;
    audit.error = Some("the audit's turn failed before any report".to_string());
    provision_agent_worktree(
        fixture._root.path().to_path_buf(),
        audit.id.clone(),
        "main".to_string(),
    )
    .await
    .expect("provision the audit checkout");
    team.records.lock().await.push(audit.clone());
    let resumed = team
        .resume("auditworker1")
        .await
        .expect("a failed audit can still be resumed");
    assert_eq!(resumed.status, WorkerStatus::Idle, "{resumed:?}");
    team.shutdown().await;
}

/// Ledgers written before `report_delivered` existed decode without it;
/// a `Completed` audit from such a ledger still counts as finished.
#[tokio::test]
async fn legacy_completed_audit_counts_as_finished() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let mut audit = worker(root.path(), "auditworker1");
    audit.kind = "audit".to_string();
    audit.status = WorkerStatus::Completed;
    // The on-disk ledger predates the field: serialize and strip it.
    let mut value = serde_json::to_value(&audit).expect("serialize the audit");
    value
        .as_object_mut()
        .expect("the record serializes as an object")
        .remove("report_delivered");
    let legacy: WorkerRecord =
        serde_json::from_value(value).expect("decode the legacy ledger");
    assert!(!legacy.report_delivered, "{legacy:?}");
    assert!(audit_finished(&legacy), "{legacy:?}");
    let team = open_test_team(root.path(), state.path(), vec![legacy]).await;
    let error = team
        .resume("auditworker1")
        .await
        .expect_err("resume refuses a legacy completed audit");
    assert_eq!(
        error,
        "audit worker 'auditworker1' already delivered its report; dispatch a new audit with team_audit_dispatch"
    );
    team.shutdown().await;
}

/// A model switch appends the new spec once: `models_used` is the order the
/// worker ran in, without consecutive duplicates (plan 40eef714 step 4).
#[tokio::test]
async fn model_switch_appends_models_used() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let team = open_test_team(root.path(), state.path(), Vec::new()).await;
    team.set_catalog(
        vec![lead_model("scripted-a"), lead_model("scripted-b")],
        mnemo::headless::TiersDto::default(),
        Vec::new(),
    );

    let dispatched = team
        .dispatch_with_id(
            "implement the counting feature".into(),
            Some("test/scripted-a".into()),
            Some("models00001".into()),
            None,
        )
        .await
        .expect("dispatch the worker on model A");
    assert_eq!(
        dispatched.models_used,
        vec!["test/scripted-a".to_string()],
        "{dispatched:?}"
    );

    let switched = team
        .change_model(
            "models00001",
            Some("test/scripted-b".into()),
            None,
            ChangeSource::User,
        )
        .await
        .expect("switch the worker to model B");
    assert_eq!(
        switched.models_used,
        vec![
            "test/scripted-a".to_string(),
            "test/scripted-b".to_string()
        ],
        "{switched:?}"
    );

    let again = team
        .change_model(
            "models00001",
            Some("test/scripted-b".into()),
            None,
            ChangeSource::User,
        )
        .await
        .expect("switch to model B again");
    assert_eq!(
        again.models_used,
        vec![
            "test/scripted-a".to_string(),
            "test/scripted-b".to_string()
        ],
        "a repeated model adds nothing: {again:?}"
    );
    team.shutdown().await;
}

struct StubDispatchClassifier {
    answer: Option<mnemo::memory::Answer>,
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    delay: std::time::Duration,
}

impl StubDispatchClassifier {
    fn new(answer: Option<mnemo::memory::Answer>) -> Self {
        Self {
            answer,
            calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            delay: std::time::Duration::ZERO,
        }
    }

    fn slow(answer: Option<mnemo::memory::Answer>, delay: std::time::Duration) -> Self {
        Self { delay, ..Self::new(answer) }
    }

    fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::Relaxed)
    }
}

#[async_trait::async_trait]
impl mnemo::memory::Classifier for StubDispatchClassifier {
    async fn classify(
        &self,
        _state: &str,
        _question: &mnemo::memory::Question,
    ) -> Option<mnemo::memory::Answer> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        tokio::time::sleep(self.delay).await;
        self.answer.clone()
    }
}

struct RoutingFixture {
    _root: tempfile::TempDir,
    state: tempfile::TempDir,
    _config: tempfile::TempDir,
    team: std::sync::Arc<TeamRuntime>,
    plan_id: String,
    classifier: std::sync::Arc<StubDispatchClassifier>,
}

impl RoutingFixture {
    fn log_path(&self) -> std::path::PathBuf {
        self.state.path().join("dispatch_routing.jsonl")
    }

    /// The rows written so far; an absent log (routing off, never
    /// touched) reads as no rows.
    fn rows(&self) -> Vec<crate::mnemo2::dispatch_routing::DispatchRoutingRow> {
        std::fs::read_to_string(self.log_path())
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).expect("routing row"))
            .collect()
    }

    async fn dispatch(&self, tier: Option<&str>, model: Option<&str>) -> ToolResult {
        let mut args = json!({
            "worker_id": "routingworker",
            "plan_id": self.plan_id,
            "step": 1,
            "brief": "Implement the step exactly and run its focused regression tests.",
        });
        if let Some(tier) = tier {
            args["tier"] = json!(tier);
        }
        if let Some(model) = model {
            args["model"] = json!(model);
        }
        self.team.handle_lead_host_tool("team_dispatch", args).await
    }
}

fn routing_answer(label: &str, confidence: f64) -> Option<mnemo::memory::Answer> {
    Some(mnemo::memory::Answer::Choice {
        label: label.into(),
        confidence,
        probabilities: std::collections::BTreeMap::new(),
    })
}

async fn routing_fixture_with(
    settings: crate::mnemo2::dispatch_routing::RoutingSettings,
    answer: Option<mnemo::memory::Answer>,
    install_classifier: bool,
    delay: std::time::Duration,
) -> RoutingFixture {
    use crate::mnemo2::dispatch_routing::save_routing_settings;
    use mnemo::app::plan_board::{PlanBoard, PlanDraft};

    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    let config = tempfile::tempdir().expect("config");
    init_git(root.path());
    save_routing_settings(config.path(), &settings).expect("save routing mode");
    let mut team = open_test_team_with_config(
        root.path(),
        state.path(),
        Vec::new(),
        config.path(),
    )
    .await;
    let models = vec![
        lead_model("scripted-difficult"),
        lead_model("scripted-average"),
        lead_model("scripted-simple"),
    ];
    team.set_catalog(
        models,
        tiers_dto! {
            difficult: vec!["test/scripted-difficult".into()],
            average: vec!["test/scripted-average".into()],
            simple: vec!["test/scripted-simple".into()],
        },
        Vec::new(),
    );
    let classifier = std::sync::Arc::new(StubDispatchClassifier::slow(answer, delay));
    if install_classifier {
        let slot: mnemo::memory::classifier::ClassifierSlot =
            std::sync::Arc::new(std::sync::RwLock::new(Some(classifier.clone())));
        team.classifier = Some(slot);
    }
    let board = Arc::new(
        PlanBoard::new(
            root.path().join(".coding/plans"),
            root.path().join(".coding/reviews"),
        ),
    );
    let plan = board
        .create(PlanDraft {
            title: "Dispatch routing test".into(),
            goal: "Exercise Laya dispatch routing through the real worker lifecycle".into(),
            context: "A persisted board step supplies the exact implementation text and worker brief.".into(),
            steps: vec!["Implement the requested routing behavior in src-tauri/src/mnemo2/team.rs and verify it with focused regression tests".into()],
            ..PlanDraft::default()
        })
        .expect("create routing plan");
    let team = Arc::new(team);
    team.attach_board(board);
    RoutingFixture {
        _root: root,
        state,
        _config: config,
        team,
        plan_id: plan.id,
        classifier,
    }
}

async fn routing_fixture(
    mode: crate::mnemo2::dispatch_routing::RoutingMode,
    answer: Option<mnemo::memory::Answer>,
    install_classifier: bool,
) -> RoutingFixture {
    routing_fixture_with(
        crate::mnemo2::dispatch_routing::RoutingSettings {
            mode,
            ..crate::mnemo2::dispatch_routing::RoutingSettings::default()
        },
        answer,
        install_classifier,
        std::time::Duration::ZERO,
    )
    .await
}

/// Poll `probe` every 25 ms until it resolves to `Some`, or fail after
/// 20 s: the worker pumps and the detached drift work run concurrently
/// with the test.
async fn eventually<T, F, Fut>(probe: F) -> T
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    for _ in 0..800 {
        if let Some(value) = probe().await {
            return value;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("the polled condition never became true");
}

/// Wait until the deferred classification of an explicit-choice dispatch
/// has written its `Route` row — the sync point for every assertion
/// about an async dispatch's corpus and record (plan c74b1987).
async fn wait_route_row(fixture: &RoutingFixture) -> DispatchRoutingRow {
    eventually(|| async {
        fixture
            .rows()
            .into_iter()
            .find(|row| matches!(row, DispatchRoutingRow::Route { .. }))
    })
    .await
}

/// Wait until the deferred label has landed on the worker's routing
/// block, then return the block.
async fn wait_laya_label(team: &TeamRuntime, worker_id: &str) -> WorkerRouting {
    eventually(|| async {
        team.worker(worker_id)
            .await
            .expect("worker")
            .routing
            .filter(|routing| routing.confidence.is_some())
    })
    .await
}

/// A scripted LLM client (the team module's scripted-mock seam): every
/// `complete` serves the next scripted batch, then a plain text stop —
/// so a dispatched worker runs real turns, executes real tool calls,
/// and the pump observes real tool events.
struct ScriptedLlm {
    responses: std::sync::Mutex<VecDeque<Vec<mnemo::provider::LlmEvent>>>,
}

impl ScriptedLlm {
    fn new(responses: Vec<Vec<mnemo::provider::LlmEvent>>) -> Self {
        Self {
            responses: std::sync::Mutex::new(responses.into()),
        }
    }
}

#[async_trait::async_trait]
impl mnemo::provider::LlmClient for ScriptedLlm {
    fn capabilities(&self) -> &mnemo::provider::Capabilities {
        static CAPS: std::sync::OnceLock<mnemo::provider::Capabilities> =
            std::sync::OnceLock::new();
        CAPS.get_or_init(mnemo::provider::Capabilities::openai)
    }

    fn kind(&self) -> mnemo::provider::ProviderKind {
        mnemo::provider::ProviderKind::OpenAI
    }

    fn model(&self) -> &str {
        "scripted"
    }

    async fn complete(
        &self,
        _messages: &[mnemo::provider::Message],
        _tools: &[mnemo::provider::ToolSchema],
        _tool_choice: Option<mnemo::provider::ToolChoice>,
    ) -> mnemo::error::Result<
        futures::stream::BoxStream<'_, mnemo::provider::LlmEvent>,
    > {
        let events = self
            .responses
            .lock()
            .map(|mut queue| queue.pop_front())
            .unwrap_or_default()
            .unwrap_or_else(|| {
                vec![
                    mnemo::provider::LlmEvent::TextDelta {
                        text: "done".to_string(),
                    },
                    mnemo::provider::LlmEvent::Finish {
                        reason: mnemo::provider::FinishReason::Stop,
                    },
                ]
            });
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

/// One scripted batch: a single tool call the session executes for
/// real. An unknown tool name deterministically fails; `read_files` on
/// the worktree's seeded README.md deterministically succeeds.
fn tool_call_batch(call_id: &str, name: &str, args: &str) -> Vec<mnemo::provider::LlmEvent> {
    vec![
        mnemo::provider::LlmEvent::ToolCallStart {
            index: 0,
            id: call_id.to_string(),
            name: name.to_string(),
        },
        mnemo::provider::LlmEvent::ToolCallArgumentDelta {
            index: 0,
            fragment: args.to_string(),
        },
        mnemo::provider::LlmEvent::Finish {
            reason: mnemo::provider::FinishReason::ToolCalls,
        },
    ]
}

/// The nudge tests' live team (design §4.6): the routing settings come
/// from the config dir, all three tiers are in the catalog, and the
/// worker sessions run real turns served by the scripted LLM — so the
/// pump observes real tool results and the strike counter counts real
/// failures. Returns the usual routing fixture plus a board-signal
/// subscription taken before any dispatch, to catch nudge notices.
async fn nudge_fixture_with(
    settings: crate::mnemo2::dispatch_routing::RoutingSettings,
    answer: Option<mnemo::memory::Answer>,
    delay: std::time::Duration,
    responses: Vec<Vec<mnemo::provider::LlmEvent>>,
) -> (RoutingFixture, broadcast::Receiver<BoardSignal>) {
    use crate::mnemo2::dispatch_routing::save_routing_settings;
    use mnemo::app::plan_board::{PlanBoard, PlanDraft};

    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    let config = tempfile::tempdir().expect("config");
    init_git(root.path());
    save_routing_settings(config.path(), &settings).expect("save routing settings");
    std::fs::write(
        config.path().join("config.toml"),
        "[general]\ndefault_provider = \"test\"\ndefault_model = \"scripted-a\"\n",
    )
    .expect("config.toml");
    std::fs::write(
        config.path().join("endpoints.toml"),
        "[[endpoint]]\nname = \"test\"\nkind = \"local\"\nbase_url = \"http://127.0.0.1:9/v1/\"\nmodels = [\"scripted-a\", \"scripted-b\"]\n",
    )
    .expect("endpoints.toml");
    TeamPersistence::new(state.path())
        .save(&PersistedTeam {
            workers: Vec::new(),
            slots: Some(2),
            usage: ProjectUsage::default(),
        })
        .expect("save the team ledger");
    let engine = mnemo::headless::EngineBuilder::new(mnemo::headless::EngineOptions {
        workspace: mnemo::workspace::Workspace::new(root.path()),
        config_dir: Some(config.path().to_path_buf()),
        permissions: mnemo::agent::policy::PermissionPolicy::OverridePermissions,
        memory: mnemo::headless::MemoryBackend::Disabled,
        index_codegraph: false,
        require_git: true,
        auto_install_embedder: false,
    })
    .provider(Arc::new(ScriptedLlm::new(responses)))
    .start()
    .await
    .expect("start the scripted engine");
    let mut team = TeamRuntime::open(TeamRuntimeOptions {
        engine: Arc::new(engine),
        workspace_root: root.path().to_path_buf(),
        state_dir: state.path().to_path_buf(),
        config_dir: config.path().to_path_buf(),
        classifier: None,
        routing_log_path: Some(state.path().join("dispatch_routing.jsonl")),
        slots: Some(2),
        models: Vec::new(),
        pricing: Vec::new(),
    })
    .await
    .expect("open the team");
    team.set_catalog(
        vec![
            lead_model("scripted-difficult"),
            lead_model("scripted-average"),
            lead_model("scripted-simple"),
        ],
        tiers_dto! {
            difficult: vec!["test/scripted-difficult".into()],
            average: vec!["test/scripted-average".into()],
            simple: vec!["test/scripted-simple".into()],
        },
        Vec::new(),
    );
    let classifier = std::sync::Arc::new(StubDispatchClassifier::slow(answer, delay));
    let slot: mnemo::memory::classifier::ClassifierSlot =
        std::sync::Arc::new(std::sync::RwLock::new(Some(classifier.clone())));
    team.classifier = Some(slot);
    let board = Arc::new(PlanBoard::new(
        root.path().join(".coding/plans"),
        root.path().join(".coding/reviews"),
    ));
    let plan = board
        .create(PlanDraft {
            title: "Nudge test".into(),
            goal: "Exercise the nudge paths through the real worker lifecycle".into(),
            context: "A persisted board step supplies the exact implementation text and worker brief.".into(),
            steps: vec!["Implement the requested nudge behavior in src-tauri/src/mnemo2/team.rs and verify it with focused regression tests".into()],
            ..PlanDraft::default()
        })
        .expect("create the nudge plan");
    let team = Arc::new(team);
    team.attach_board(board);
    let notices = team.subscribe_board();
    (
        RoutingFixture {
            _root: root,
            state,
            _config: config,
            team,
            plan_id: plan.id,
            classifier,
        },
        notices,
    )
}

/// A shadow-mode nudge fixture with the scripted responses.
async fn nudge_fixture(
    responses: Vec<Vec<mnemo::provider::LlmEvent>>,
) -> (RoutingFixture, broadcast::Receiver<BoardSignal>) {
    nudge_fixture_with(
        crate::mnemo2::dispatch_routing::RoutingSettings {
            mode: crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
            ..crate::mnemo2::dispatch_routing::RoutingSettings::default()
        },
        routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.62),
        std::time::Duration::ZERO,
        responses,
    )
    .await
}

/// Wait until the live worker's turn settles: the pump processes the
/// turn's tool events synchronously, so once the status flips the
/// nudge decision has also been made.
async fn wait_worker_settled(team: &TeamRuntime, worker_id: &str) -> WorkerRecord {
    eventually(|| async {
        let worker = team.worker(worker_id).await.expect("worker");
        matches!(
            worker.status,
            WorkerStatus::Idle
                | WorkerStatus::Completed
                | WorkerStatus::Stopped
                | WorkerStatus::Failed
        )
        .then_some(worker)
    })
    .await
}

/// Poll the board signals until the worker's nudge notice lands;
/// unrelated refreshes drain away.
async fn wait_nudge_notice(
    notices: &mut broadcast::Receiver<BoardSignal>,
    worker_id: &str,
) -> String {
    let key = format!("nudge:{worker_id}");
    for _ in 0..800 {
        while let Ok(signal) = notices.try_recv() {
            if let BoardSignal::Notice { key: notice_key, text } = signal {
                if notice_key == key {
                    return text;
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("the nudge notice never arrived");
}

/// Every cost-alert notice on the board-signal stream so far; unrelated
/// refreshes and notices drain away.
fn drain_cost_notices(
    notices: &mut broadcast::Receiver<BoardSignal>,
    worker_id: &str,
) -> Vec<String> {
    let prefix = format!("cost:{worker_id}:");
    let mut found = Vec::new();
    loop {
        match notices.try_recv() {
            Ok(BoardSignal::Notice { key, text }) if key.starts_with(&prefix) => {
                found.push(text);
            }
            Ok(_) => {}
            Err(broadcast::error::TryRecvError::Lagged(_)) => {}
            Err(_) => break,
        }
    }
    found
}

/// The cost alert fires at the threshold, then at each doubling — never
/// twice for the same level, and never when it is off (plan 06f1a204).
#[test]
fn cost_notice_due_fires_at_the_threshold_and_each_doubling() {
    let threshold = 2_000_000;
    assert_eq!(
        cost_notice_due(1_999_999, 0, threshold),
        None,
        "below the threshold"
    );
    assert_eq!(
        cost_notice_due(2_000_000, 0, threshold),
        Some(2_000_000),
        "the threshold itself"
    );
    assert_eq!(
        cost_notice_due(3_000_000, 2_000_000, threshold),
        None,
        "between two levels"
    );
    assert_eq!(
        cost_notice_due(4_000_000, 2_000_000, threshold),
        Some(4_000_000),
        "the first doubling"
    );
    assert_eq!(
        cost_notice_due(9_000_000, 0, threshold),
        Some(8_000_000),
        "the highest doubling passed, not the next one"
    );
    assert_eq!(
        cost_notice_due(u64::MAX, 0, threshold),
        Some(u64::MAX),
        "a saturated cost still terminates"
    );
    assert_eq!(
        cost_notice_due(9_000_000, 0, 0),
        None,
        "0 turns the alert off"
    );
}

/// One worker whose estimated cost passes the alert tells the lead once,
/// and keeps running: the notice never stops or changes it (plan
/// 06f1a204).
#[tokio::test]
async fn a_worker_past_the_threshold_notifies_the_lead_once() {
    let responses = vec![vec![
        mnemo::provider::LlmEvent::Usage {
            prompt_tokens: 3,
            completion_tokens: 0,
            reasoning_tokens: 0,
            cached_tokens: 0,
            ttft_ms: None,
            generation_ms: None,
        },
        mnemo::provider::LlmEvent::TextDelta {
            text: "done".to_string(),
        },
        mnemo::provider::LlmEvent::Finish {
            reason: mnemo::provider::FinishReason::Stop,
        },
    ]];
    let (fixture, mut notices) = nudge_fixture(responses).await;
    // $1 per token on the scripted provider's model: the turn's three
    // prompt tokens cost $3.00, past the default $2.00 alert.
    fixture.team.set_catalog(
        vec![
            lead_model("scripted-difficult"),
            lead_model("scripted-average"),
            lead_model("scripted-simple"),
        ],
        tiers_dto! {
            difficult: vec!["test/scripted-difficult".into()],
            average: vec!["test/scripted-average".into()],
            simple: vec!["test/scripted-simple".into()],
        },
        vec![mnemo::config::PricingEntry {
            model: "scripted".to_string(),
            input_per_1m: 1_000_000.0,
            output_per_1m: 0.0,
            cached_per_1m: 0.0,
        }],
    );
    let result = fixture.dispatch(Some("routine"), None).await;
    assert!(result.success, "{}", result.output);
    let settled = wait_worker_settled(&fixture.team, "routingworker").await;
    // The pump sends the notice right after the record block of the
    // Usage event, which precedes the Finished event that settled the
    // worker above — so it is on the stream by now.
    let found = drain_cost_notices(&mut notices, "routingworker");
    assert_eq!(
        found.len(),
        1,
        "exactly one cost notice: {found:?}; worker: {settled:?}"
    );
    let notice = &found[0];
    assert!(
        notice.starts_with("worker_id=routingworker "),
        "the notice names the worker: {notice}"
    );
    assert!(
        notice.contains("has cost about $3.00 so far"),
        "the notice names the spend: {notice}"
    );
    assert!(
        notice.contains("1 requests on scripted, tier routine."),
        "the notice names the request count, model and tier: {notice}"
    );
    assert!(
        notice.ends_with(
            "It keeps running. If that looks wrong, read team_status, then team_message to steer it or team_stop to cancel it."
        ),
        "{notice}"
    );
    assert_eq!(
        settled.cost_notified_micros, 2_000_000,
        "the record remembers the level the lead was told about: {settled:?}"
    );
    assert_eq!(settled.cost_micros, Some(3_000_000), "{settled:?}");
    assert!(
        matches!(settled.status, WorkerStatus::Completed),
        "the alert never stopped or changed the worker: {settled:?}"
    );
    assert!(settled.error.is_none(), "{settled:?}");
}

/// A lead message long enough for drift classification (≥ 20 words).
fn drift_message() -> String {
    "This change turned out to touch the shared routing contract after all, \
         so please be extra careful with the integration boundary and run the \
         full regression battery once more before you call the step done."
        .to_string()
}

/// The session's consecutive-failing-batch cap — `MAX_RETRIES` in
/// src/agent/mod.rs, raised 3 → 5 by plan 865249fd. It is `pub(crate)`
/// in the mnemo crate, so not reachable from src-tauri; mirrored here
/// for the tests that script a worker into the doom-guard failure.
/// Keep in sync with src/agent/mod.rs.
const TEST_MAX_RETRIES: usize = 5;

/// Three failing tool calls in a row nudge the lead once — and only
/// once, even when the streak keeps failing (design §4.6). The nudge
/// fires at Laya's strike limit (3); the worker itself now runs on to
/// the doom-guard cap (TEST_MAX_RETRIES, plan 865249fd) before it
/// lands Failed.
#[tokio::test]
async fn three_failing_tool_calls_nudge_once() {
    let responses = (1..=TEST_MAX_RETRIES)
        .map(|attempt| tool_call_batch(&format!("call-{attempt}"), "not_a_real_tool", &format!("{{\"attempt\":{attempt}}}")))
        .chain([vec![
            mnemo::provider::LlmEvent::TextDelta { text: "done".to_string() },
            mnemo::provider::LlmEvent::Finish { reason: mnemo::provider::FinishReason::Stop },
        ]])
        .collect::<Vec<_>>();
    let (fixture, mut notices) = nudge_fixture(responses).await;
    let result = fixture.dispatch(Some("routine"), None).await;
    assert!(result.success, "{}", result.output);
    // The deferred Route row lands before the nudge and outcome rows.
    wait_route_row(&fixture).await;
    let row = eventually(|| async {
        fixture
            .rows()
            .into_iter()
            .find(|row| matches!(row, DispatchRoutingRow::Nudge { .. }))
    })
    .await;
    assert!(
        matches!(
            &row,
            DispatchRoutingRow::Nudge {
                reason,
                from_tier,
                to_tier,
                detail,
                ..
            } if reason == "strikes"
                && from_tier == "routine"
                && to_tier == "high"
                && detail == "looks stuck (3 failing tool calls in a row)"
        ),
        "the strike nudge row: {row:?}"
    );
    let notice = wait_nudge_notice(&mut notices, "routingworker").await;
    let prefix = "worker_id=routingworker Laya nudge: routingworker looks stuck (3 failing tool calls in a row); consider team_resume with tier high.\nLast tool errors:";
    assert!(notice.starts_with(prefix), "{notice}");
    assert_eq!(
        notice.matches("\n- not_a_real_tool: ").count(),
        3,
        "the failed call's tool name, once per kept failure: {notice}"
    );
    assert!(
        notice.contains("unknown tool 'not_a_real_tool'"),
        "the failed call's actual output: {notice}"
    );
    assert!(
        notice.ends_with(
            "\nRead these first: a tool problem is not fixed by a stronger model."
        ),
        "{notice}"
    );
    let settled = wait_worker_settled(&fixture.team, "routingworker").await;
    assert_eq!(
        settled.routing.as_ref().expect("routed at dispatch").nudged.as_deref(),
        Some("high"),
        "the nudge is marked on the record: {settled:?}"
    );
    // TEST_MAX_RETRIES consecutive real failures also trip the
    // session's own doom guard, so the worker lands Failed — after the
    // nudge (at the 3rd strike) has already told the lead to resume on
    // a stronger tier. The nudge never stopped or switched anything
    // itself.
    assert!(
        matches!(settled.status, WorkerStatus::Failed),
        "the doom guard, not the nudge, failed the worker: {settled:?}"
    );
    assert_ne!(
        settled.model.as_deref(),
        Some("test/scripted-difficult"),
        "the nudge never switched the model: {settled:?}"
    );
    assert_eq!(
        fixture
            .rows()
            .into_iter()
            .filter(|row| matches!(row, DispatchRoutingRow::Nudge { .. }))
            .count(),
        1,
        "exactly one nudge: {:?}",
        fixture.rows()
    );
    // A failure is resumable, not terminal: the doom-guard failure
    // logs the outcome but never nudge_ignored — a team_resume on a
    // stronger tier can still take the nudge. The dispatch's Route row
    // lands in the background (plan c74b1987), so the three rows are
    // asserted as a set, not an order.
    let rows = fixture.rows();
    assert_eq!(rows.len(), 3, "the route, the nudge, and the outcome rows: {rows:?}");
    assert!(
        rows.iter().any(|row| matches!(row, DispatchRoutingRow::Route { .. })),
        "the deferred route row landed: {rows:?}"
    );
    assert!(
        rows.iter().any(|row| matches!(row, DispatchRoutingRow::Nudge { .. })),
        "the strike nudge row: {rows:?}"
    );
    assert!(
        rows.iter()
            .any(|row| matches!(row, DispatchRoutingRow::Outcome { status, .. } if status == "failed")),
        "the failed outcome row: {rows:?}"
    );
    fixture.team.shutdown().await;
}

/// The failing-tool streak keeps the last three calls, collapses each
/// output's whitespace, cuts it at the character limit without splitting a
/// character, and a successful call clears it.
#[test]
fn note_tool_outcome_keeps_the_last_three_and_clears_on_success() {
    let mut worker = worker(std::path::Path::new("."), "streak0001");
    for (index, tool) in ["file_read", "shell", "git", "plan_read"]
        .iter()
        .enumerate()
    {
        note_tool_outcome(&mut worker, tool, false, &format!("  failed\n\ncall {index}  "));
    }
    let tools: Vec<&str> = worker
        .tool_errors
        .iter()
        .map(|note| note.tool.as_str())
        .collect();
    assert_eq!(tools, ["shell", "git", "plan_read"], "only the last three");
    assert_eq!(worker.tool_errors[0].output, "failed call 1");

    // A multi-byte output is cut by character, never mid-character.
    let long = "é".repeat(TOOL_ERROR_CHARS + 5);
    note_tool_outcome(&mut worker, "shell", false, &long);
    let kept = worker.tool_errors.last().expect("the newest failure");
    assert_eq!(kept.output.chars().count(), TOOL_ERROR_CHARS + 1);
    assert!(kept.output.ends_with('…'), "{kept:?}");
    assert_eq!(
        kept.output.chars().take(TOOL_ERROR_CHARS).collect::<String>(),
        "é".repeat(TOOL_ERROR_CHARS)
    );
    assert_eq!(
        tool_errors_text(&worker),
        format!(
            "\nLast tool errors:\n- git: failed call 2\n- plan_read: failed call 3\n- shell: {}…",
            "é".repeat(TOOL_ERROR_CHARS)
        )
    );

    // A success clears the streak, and an empty streak has no text.
    note_tool_outcome(&mut worker, "file_read", true, "ok");
    assert!(worker.tool_errors.is_empty());
    assert!(tool_errors_text(&worker).is_empty());
}

/// The strike nudge carries the actual failed calls — the tool name and
/// its output for each of the last three failures — and the warning that a
/// stronger model does not fix a tool problem.
#[tokio::test]
async fn the_strike_nudge_shows_the_tool_errors() {
    let responses = (1..=3)
        .map(|attempt| {
            tool_call_batch(
                &format!("call-{attempt}"),
                "not_a_real_tool",
                &format!("{{\"attempt\":{attempt}}}"),
            )
        })
        .collect::<Vec<_>>();
    let (fixture, mut notices) = nudge_fixture(responses).await;
    let result = fixture.dispatch(Some("routine"), None).await;
    assert!(result.success, "{}", result.output);
    let notice = wait_nudge_notice(&mut notices, "routingworker").await;
    let prefix = "worker_id=routingworker Laya nudge: routingworker looks stuck (3 failing tool calls in a row); consider team_resume with tier high.\nLast tool errors:";
    assert!(notice.starts_with(prefix), "{notice}");
    assert_eq!(
        notice.matches("\n- not_a_real_tool: ").count(),
        3,
        "the tool's name on each kept failure: {notice}"
    );
    assert!(
        notice.contains("unknown tool 'not_a_real_tool'"),
        "the tool's own error: {notice}"
    );
    assert!(
        notice.ends_with("\nRead these first: a tool problem is not fixed by a stronger model."),
        "{notice}"
    );
    let settled = wait_worker_settled(&fixture.team, "routingworker").await;
    assert_eq!(settled.tool_errors.len(), 3, "{settled:?}");
    fixture.team.shutdown().await;
}

/// `team_status` reports each worker's current failing streak, and omits
/// the field for a worker whose last call succeeded.
#[tokio::test]
async fn team_status_reports_tool_errors() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let mut failing = reviewed_worker(root.path(), "00000001000000000003");
    failing.tool_errors = vec![
        ToolErrorNote {
            tool: "shell".to_string(),
            output: "exit status 1".to_string(),
        },
        ToolErrorNote {
            tool: "git".to_string(),
            output: "no such branch".to_string(),
        },
    ];
    let clean = reviewed_worker(root.path(), "00000001000000000004");
    let team = open_test_team(root.path(), state.path(), vec![failing, clean]).await;
    let answer = team_status_answer(&team, json!({})).await;
    let entries = answer["result"].as_array().expect("a worker array");
    assert_eq!(entries[0]["tool_errors"][0]["tool"], "shell");
    assert_eq!(entries[0]["tool_errors"][1]["output"], "no such branch");
    assert!(
        entries[1].get("tool_errors").is_none(),
        "a clean worker has no tool_errors key: {}",
        entries[1]
    );
    team.shutdown().await;
}

/// A failed worker whose session is alive and idle resumes in place:
/// `team_resume` makes it idle instead of dead-ending on "already
/// running", clears the error, and keeps the failing streak readable.
#[tokio::test]
async fn resume_makes_a_failed_worker_with_a_live_session_idle() {
    // TEST_MAX_RETRIES failing calls trip the session's doom guard
    // (raised 3 → 5); only the last TOOL_ERRORS_KEPT errors are kept.
    let responses = (1..=TEST_MAX_RETRIES)
        .map(|attempt| {
            tool_call_batch(
                &format!("call-{attempt}"),
                "not_a_real_tool",
                &format!("{{\"attempt\":{attempt}}}"),
            )
        })
        .collect::<Vec<_>>();
    let (fixture, _notices) = nudge_fixture(responses).await;
    let result = fixture.dispatch(Some("routine"), None).await;
    assert!(result.success, "{}", result.output);
    let failed = wait_worker_settled(&fixture.team, "routingworker").await;
    assert_eq!(failed.status, WorkerStatus::Failed, "{failed:?}");
    assert!(
        fixture.team.live.lock().await.contains_key("routingworker"),
        "the failed worker's session is still live"
    );
    let resumed = fixture
        .team
        .resume("routingworker")
        .await
        .expect("a failed worker with a live idle session resumes");
    assert_eq!(resumed.status, WorkerStatus::Idle, "{resumed:?}");
    assert!(resumed.error.is_none(), "{resumed:?}");
    assert!(resumed.pending.is_none(), "{resumed:?}");
    assert_eq!(resumed.tool_errors.len(), 3, "the errors stay readable");
    fixture.team.shutdown().await;
}

/// A worker that is actually running still refuses a resume, now pointing
/// the lead at `team_message`.
#[tokio::test]
async fn resume_still_refuses_a_running_worker() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let mut busy = worker(root.path(), "00000001000000000005");
    busy.status = WorkerStatus::Working;
    provision_agent_worktree(
        root.path().to_path_buf(),
        busy.id.clone(),
        "main".to_string(),
    )
    .await
    .expect("provision the running worker");
    let team = open_test_team(root.path(), state.path(), vec![busy.clone()]).await;
    spawn_live_worker(&team, &busy).await;
    team.live
        .lock()
        .await
        .get(&busy.id)
        .expect("the live worker")
        .busy
        .store(true, Ordering::SeqCst);
    let error = team
        .resume(&busy.id)
        .await
        .expect_err("resume refuses a running worker");
    assert!(error.contains("already running"), "{error}");
    assert!(error.contains("team_message"), "{error}");
    team.shutdown().await;
}

/// A delivered message starts the worker over: the stale failure error is
/// cleared from the record.
#[tokio::test]
async fn a_message_clears_a_failed_workers_error() {
    // TEST_MAX_RETRIES failing calls trip the session's doom guard
    // (raised 3 → 5), which is what lands the worker Failed.
    let responses = (1..=TEST_MAX_RETRIES)
        .map(|attempt| {
            tool_call_batch(
                &format!("call-{attempt}"),
                "not_a_real_tool",
                &format!("{{\"attempt\":{attempt}}}"),
            )
        })
        .collect::<Vec<_>>();
    let (fixture, _notices) = nudge_fixture(responses).await;
    let result = fixture.dispatch(Some("routine"), None).await;
    assert!(result.success, "{}", result.output);
    let failed = wait_worker_settled(&fixture.team, "routingworker").await;
    assert!(failed.error.is_some(), "{failed:?}");
    let messaged = fixture
        .team
        .message(
            "routingworker",
            "that tool does not exist; read the file instead".to_string(),
            MessageSender::Lead,
        )
        .await
        .expect("a failed worker with a live session takes a message");
    assert!(messaged.error.is_none(), "{messaged:?}");
    fixture.team.shutdown().await;
}

/// A failure is resumable, not terminal: the doom-guard failure that
/// rides the strike nudge logs the outcome but never `nudge_ignored` —
/// `team_resume` on a stronger tier can still take the nudge.
#[tokio::test]
async fn failed_worker_does_not_log_nudge_ignored() {
    let responses = (1..=TEST_MAX_RETRIES)
        .map(|attempt| {
            tool_call_batch(
                &format!("call-{attempt}"),
                "not_a_real_tool",
                &format!("{{\"attempt\":{attempt}}}"),
            )
        })
        .collect::<Vec<_>>();
    let (fixture, _notices) = nudge_fixture(responses).await;
    let result = fixture.dispatch(Some("routine"), None).await;
    assert!(result.success, "{}", result.output);
    // The terminal failure row is the deterministic marker that the
    // pump has fully processed the worker's failure; the nudge row
    // lands with the third tool result, before the Error event.
    let outcome = eventually(|| async {
        fixture.rows().into_iter().find(|row| {
            matches!(row, DispatchRoutingRow::Outcome { status, .. } if status == "failed")
        })
    })
    .await;
    assert!(
        matches!(&outcome, DispatchRoutingRow::Outcome { final_tier, .. } if final_tier == "routine"),
        "the failure outcome: {outcome:?}"
    );
    assert!(
        fixture
            .rows()
            .iter()
            .any(|row| matches!(row, DispatchRoutingRow::Nudge { .. })),
        "the strike nudge fired before the failure: {:?}",
        fixture.rows()
    );
    assert!(
        fixture
            .rows()
            .iter()
            .all(|row| !matches!(row, DispatchRoutingRow::NudgeIgnored { .. })),
        "a failed worker never logs nudge_ignored: {:?}",
        fixture.rows()
    );
    fixture.team.shutdown().await;
}

/// Stopping the failed worker is truly terminal: the ignored nudge logs
/// there, once — a repeated stop writes nothing more.
#[tokio::test]
async fn stopped_after_failure_logs_nudge_ignored_once() {
    let responses = (1..=TEST_MAX_RETRIES)
        .map(|attempt| {
            tool_call_batch(
                &format!("call-{attempt}"),
                "not_a_real_tool",
                &format!("{{\"attempt\":{attempt}}}"),
            )
        })
        .collect::<Vec<_>>();
    let (fixture, _notices) = nudge_fixture(responses).await;
    let result = fixture.dispatch(Some("routine"), None).await;
    assert!(result.success, "{}", result.output);
    // The strike nudge fires and the doom guard fails the worker.
    let failed = eventually(|| async {
        let worker = fixture
            .team
            .worker("routingworker")
            .await
            .expect("worker");
        (worker.status == WorkerStatus::Failed).then_some(worker)
    })
    .await;
    assert!(
        failed.routing.as_ref().expect("routed at dispatch").nudged.is_some(),
        "the strike nudge was claimed before the failure: {failed:?}"
    );
    assert!(
        fixture
            .rows()
            .iter()
            .all(|row| !matches!(row, DispatchRoutingRow::NudgeIgnored { .. })),
        "the failure itself never logs nudge_ignored: {:?}",
        fixture.rows()
    );
    // The lead stops the failed worker: the ignored nudge lands beside
    // the stopped outcome.
    let stopped = fixture
        .team
        .stop("routingworker")
        .await
        .expect("stop the failed worker");
    assert!(matches!(stopped.status, WorkerStatus::Stopped), "{stopped:?}");
    assert!(
        matches!(
            fixture.rows().as_slice(),
            [..,
             DispatchRoutingRow::Outcome { status, .. },
             DispatchRoutingRow::NudgeIgnored { tier, .. }
            ] if status == "stopped" && tier == "routine"
        ),
        "the stopped outcome plus the ignored nudge: {:?}",
        fixture.rows()
    );
    // Stopping again writes nothing more.
    fixture
        .team
        .stop("routingworker")
        .await
        .expect("stopping again is a no-op");
    assert_eq!(
        fixture
            .rows()
            .iter()
            .filter(|row| matches!(row, DispatchRoutingRow::NudgeIgnored { .. }))
            .count(),
        1,
        "nudge_ignored logs once: {:?}",
        fixture.rows()
    );
    fixture.team.shutdown().await;
}

/// Plan 865249fd: the strike nudge to the lead still fires at the 3rd
/// failing call (Laya's limit stays 3), but the raised doom-guard cap
/// (TEST_MAX_RETRIES) lets the worker keep running — 3 failures then a
/// success finishes the turn normally instead of landing Failed.
#[tokio::test]
async fn a_worker_still_runs_after_the_strike_nudge_until_the_fifth_error() {
    let responses = (1..=3)
        .map(|attempt| {
            tool_call_batch(
                &format!("call-{attempt}"),
                "not_a_real_tool",
                &format!("{{\"attempt\":{attempt}}}"),
            )
        })
        .chain([tool_call_batch(
            "call-ok",
            "read_files",
            "{\"files\":[\"README.md\"]}",
        )])
        .chain([vec![
            mnemo::provider::LlmEvent::TextDelta { text: "done".to_string() },
            mnemo::provider::LlmEvent::Finish { reason: mnemo::provider::FinishReason::Stop },
        ]])
        .collect::<Vec<_>>();
    let (fixture, _notices) = nudge_fixture(responses).await;
    let result = fixture.dispatch(Some("routine"), None).await;
    assert!(result.success, "{}", result.output);
    wait_route_row(&fixture).await;
    // The stuck-worker nudge row logged at the 3rd failure, as before.
    let row = eventually(|| async {
        fixture
            .rows()
            .into_iter()
            .find(|row| matches!(row, DispatchRoutingRow::Nudge { .. }))
    })
    .await;
    assert!(
        matches!(
            &row,
            DispatchRoutingRow::Nudge {
                reason,
                detail,
                ..
            } if reason == "strikes"
                && detail == "looks stuck (3 failing tool calls in a row)"
        ),
        "the strike nudge row: {row:?}"
    );
    // ...but the worker kept running past the nudge and finished
    // normally on the successful 4th batch — not Failed.
    let settled = wait_worker_settled(&fixture.team, "routingworker").await;
    assert!(
        matches!(settled.status, WorkerStatus::Completed),
        "3 failures no longer fail the worker — it runs on to the \
             success: {settled:?}"
    );
    assert!(
        fixture
            .rows()
            .iter()
            .all(|row| !matches!(row, DispatchRoutingRow::Outcome { status, .. } if status == "failed")),
        "no failed outcome row: {:?}",
        fixture.rows()
    );
    fixture.team.shutdown().await;
}

/// Identical tool calls count as strikes even when they succeed: the
/// repeat is the strike, and the nudge says "going in circles".
#[tokio::test]
async fn identical_calls_count_as_strikes() {
    let batch = tool_call_batch("same-call", "read_files", "{\"files\":[\"README.md\"]}");
    let responses = vec![
        batch.clone(),
        batch.clone(),
        batch.clone(),
        batch,
        vec![
            mnemo::provider::LlmEvent::TextDelta { text: "done".to_string() },
            mnemo::provider::LlmEvent::Finish { reason: mnemo::provider::FinishReason::Stop },
        ],
    ];
    let (fixture, _notices) = nudge_fixture(responses).await;
    let result = fixture.dispatch(Some("routine"), None).await;
    assert!(result.success, "{}", result.output);
    let row = eventually(|| async {
        fixture
            .rows()
            .into_iter()
            .find(|row| matches!(row, DispatchRoutingRow::Nudge { .. }))
    })
    .await;
    assert!(
        matches!(
            &row,
            DispatchRoutingRow::Nudge { reason, detail, to_tier, .. }
                if reason == "circles"
                    && detail == "looks stuck (going in circles)"
                    && to_tier == "high"
        ),
        "the circles nudge row: {row:?}"
    );
    fixture.team.shutdown().await;
}

/// A successful, non-repeated call resets the strike counter: two
/// failures, a success, two more failures never reach three.
#[tokio::test]
async fn a_success_resets_the_strikes() {
    let responses = vec![
        tool_call_batch("call-1", "not_a_real_tool", "{\"attempt\":1}"),
        tool_call_batch("call-2", "not_a_real_tool", "{\"attempt\":2}"),
        tool_call_batch("call-3", "read_files", "{\"files\":[\"README.md\"]}"),
        tool_call_batch("call-4", "not_a_real_tool", "{\"attempt\":4}"),
        tool_call_batch("call-5", "not_a_real_tool", "{\"attempt\":5}"),
        vec![
            mnemo::provider::LlmEvent::TextDelta { text: "done".to_string() },
            mnemo::provider::LlmEvent::Finish { reason: mnemo::provider::FinishReason::Stop },
        ],
    ];
    let (fixture, mut notices) = nudge_fixture(responses).await;
    let result = fixture.dispatch(Some("routine"), None).await;
    assert!(result.success, "{}", result.output);
    let settled = wait_worker_settled(&fixture.team, "routingworker").await;
    assert!(
        fixture
            .rows()
            .into_iter()
            .all(|row| !matches!(row, DispatchRoutingRow::Nudge { .. })),
        "the reset streak never nudged: {:?}",
        fixture.rows()
    );
    assert!(
        settled.routing.as_ref().expect("routed at dispatch").nudged.is_none(),
        "the reset streak never claimed a nudge: {settled:?}"
    );
    let mut saw_nudge_notice = false;
    while let Ok(signal) = notices.try_recv() {
        if let BoardSignal::Notice { key, .. } = signal {
            if key.starts_with("nudge:") {
                saw_nudge_notice = true;
            }
        }
    }
    assert!(!saw_nudge_notice, "no nudge notice was queued");
    fixture.team.shutdown().await;
}

/// A worker already on the top enabled tier is never nudged.
#[tokio::test]
async fn no_nudge_on_the_top_tier() {
    let responses = (1..=3)
        .map(|attempt| tool_call_batch(&format!("call-{attempt}"), "not_a_real_tool", &format!("{{\"attempt\":{attempt}}}")))
        .chain([vec![
            mnemo::provider::LlmEvent::TextDelta { text: "done".to_string() },
            mnemo::provider::LlmEvent::Finish { reason: mnemo::provider::FinishReason::Stop },
        ]])
        .collect::<Vec<_>>();
    let (fixture, _notices) = nudge_fixture(responses).await;
    let result = fixture.dispatch(Some("high"), None).await;
    assert!(result.success, "{}", result.output);
    let settled = wait_worker_settled(&fixture.team, "routingworker").await;
    assert_eq!(settled.tier.as_deref(), Some("high"), "{settled:?}");
    assert!(
        fixture
            .rows()
            .into_iter()
            .all(|row| !matches!(row, DispatchRoutingRow::Nudge { .. })),
        "the top tier is never nudged: {:?}",
        fixture.rows()
    );
    assert!(
        settled.routing.as_ref().expect("routed at dispatch").nudged.is_none(),
        "no nudge was claimed on the top tier: {settled:?}"
    );
    fixture.team.shutdown().await;
}

/// A lead message Laya reads a tier above the worker's nudges with
/// reason `drift` (design §4.6).
#[tokio::test]
async fn drift_message_nudges_when_laya_reads_it_harder() {
    let (fixture, mut notices) = nudge_fixture_with(
        crate::mnemo2::dispatch_routing::RoutingSettings {
            mode: crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
            ..crate::mnemo2::dispatch_routing::RoutingSettings::default()
        },
        routing_answer(mnemo::agent::route_question::LABEL_HARD, 0.91),
        std::time::Duration::ZERO,
        vec![vec![
            mnemo::provider::LlmEvent::TextDelta { text: "done".to_string() },
            mnemo::provider::LlmEvent::Finish { reason: mnemo::provider::FinishReason::Stop },
        ]],
    )
    .await;
    let result = fixture.dispatch(Some("routine"), None).await;
    assert!(result.success, "{}", result.output);
    wait_worker_settled(&fixture.team, "routingworker").await;
    let record = fixture
        .team
        .message("routingworker", drift_message(), MessageSender::Lead)
        .await
        .expect("deliver the message");
    assert!(record.routing.is_some(), "the dispatched worker is routed");
    let row = eventually(|| async {
        fixture
            .rows()
            .into_iter()
            .find(|row| matches!(row, DispatchRoutingRow::Nudge { .. }))
    })
    .await;
    assert!(
        matches!(
            &row,
            DispatchRoutingRow::Nudge { reason, from_tier, to_tier, .. }
                if reason == "drift" && from_tier == "routine" && to_tier == "high"
        ),
        "the drift nudge row: {row:?}"
    );
    let notice = wait_nudge_notice(&mut notices, "routingworker").await;
    assert_eq!(
        notice,
        "worker_id=routingworker Laya nudge: routingworker this got harder (Laya reads the message as hard, 0.91); consider team_resume with tier high."
    );
    assert_eq!(
        fixture.classifier.calls(),
        2,
        "dispatch and drift each asked Laya once"
    );
    fixture.team.shutdown().await;
}

/// Drift classification is detached: a slow Laya never delays the
/// message, and the nudge still lands (design §4.6).
#[tokio::test]
async fn drift_never_blocks_the_message() {
    let (fixture, _notices) = nudge_fixture_with(
        crate::mnemo2::dispatch_routing::RoutingSettings {
            mode: crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
            ..crate::mnemo2::dispatch_routing::RoutingSettings::default()
        },
        routing_answer(mnemo::agent::route_question::LABEL_HARD, 0.91),
        std::time::Duration::from_secs(1),
        vec![vec![
            mnemo::provider::LlmEvent::TextDelta { text: "done".to_string() },
            mnemo::provider::LlmEvent::Finish { reason: mnemo::provider::FinishReason::Stop },
        ]],
    )
    .await;
    let result = fixture.dispatch(Some("routine"), None).await;
    assert!(result.success, "{}", result.output);
    // The dispatch's deferred classification holds the permit for the
    // stub's delay; the drift classification below waits for it and
    // still fits its own 3-second budget (1 s permit wait + 1 s ask).
    wait_worker_settled(&fixture.team, "routingworker").await;
    let started = std::time::Instant::now();
    fixture
        .team
        .message("routingworker", drift_message(), MessageSender::Lead)
        .await
        .expect("the message returns without waiting on Laya");
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_millis(1_000),
        "the message waited {elapsed:?} on drift classification"
    );
    assert_eq!(
        fixture.classifier.calls(),
        1,
        "only the dispatch classified before the message"
    );
    let row = eventually(|| async {
        fixture
            .rows()
            .into_iter()
            .find(|row| matches!(row, DispatchRoutingRow::Nudge { .. }))
    })
    .await;
    assert!(
        matches!(&row, DispatchRoutingRow::Nudge { reason, .. } if reason == "drift"),
        "the slow drift nudge still landed: {row:?}"
    );
    fixture.team.shutdown().await;
}

/// The deterministic review nudge: a second review round that still
/// records a high finding nudges the lead to the next tier up, in
/// every mode except off (design §4.6).
#[tokio::test]
async fn second_high_finding_review_nudges() {
    use crate::mnemo2::dispatch_routing::{save_routing_settings, RoutingSettings, RoutingMode};
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    let config = tempfile::tempdir().expect("config");
    init_git(root.path());
    save_routing_settings(
        config.path(),
        &RoutingSettings {
            mode: RoutingMode::Shadow,
            ..RoutingSettings::default()
        },
    )
    .expect("save routing settings");
    let mut record = worker(root.path(), "nudgereview001");
    record.tier = Some("routine".into());
    // A failing streak on the record must never leak into a review nudge:
    // the review, not a tool, is what the lead is being asked to act on.
    record.tool_errors = vec![ToolErrorNote {
        tool: "shell".to_string(),
        output: "exit status 1".to_string(),
    }];
    record.routing = Some(WorkerRouting {
        laya_tier: Some("routine".into()),
        confidence: Some(0.62),
        cleared: true,
        lead_tier: Some("routine".into()),
        routed_by: "lead".into(),
        ran_tier: Some("routine".into()),
        nudged: None,
    });
    provision_agent_worktree(
        root.path().to_path_buf(),
        record.id.clone(),
        "main".to_string(),
    )
    .await
    .expect("provision the worker checkout");
    let team = open_test_team_with_config(
        root.path(),
        state.path(),
        vec![record.clone()],
        config.path(),
    )
    .await;
    team.set_catalog(
        vec![
            lead_model("scripted-difficult"),
            lead_model("scripted-average"),
            lead_model("scripted-simple"),
        ],
        tiers_dto! {
            difficult: vec!["test/scripted-difficult".into()],
            average: vec!["test/scripted-average".into()],
            simple: vec!["test/scripted-simple".into()],
        },
        Vec::new(),
    );
    let mut notices = team.subscribe_board();

    let first = team
        .review(&record.id, "first pass".into(), vec![open_finding("finding1")], evidence())
        .await
        .expect("record the first review");
    assert_eq!(first.review_rounds, 1, "{first:?}");
    assert!(
        first.routing.as_ref().expect("routed").nudged.is_none(),
        "one high finding in round one is not a pattern: {first:?}"
    );
    team.route_fix(&record.id, "finding1")
        .await
        .expect("route the finding back to the worker");
    team.stop_inner(&record.id)
        .await
        .expect("stop the fixer session");
    let second = team
        .review(&record.id, "second pass".into(), vec![open_finding("finding1")], evidence())
        .await
        .expect("record the second review");
    assert_eq!(second.review_rounds, 2, "{second:?}");
    assert_eq!(
        second.routing.as_ref().expect("routed").nudged.as_deref(),
        Some("high"),
        "the second high review round nudged: {second:?}"
    );
    let review_notice = wait_nudge_notice(&mut notices, &record.id).await;
    assert!(
        !review_notice.contains("Last tool errors"),
        "a review nudge never shows tool errors: {review_notice}"
    );
    assert!(
        !review_notice.contains("Read these first"),
        "{review_notice}"
    );
    let rows = std::fs::read_to_string(state.path().join("dispatch_routing.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str::<DispatchRoutingRow>(line).expect("routing row"))
        .collect::<Vec<_>>();
    assert!(
        matches!(
            rows.as_slice(),
            [DispatchRoutingRow::Nudge { reason, from_tier, to_tier, detail, .. }]
                if reason == "review"
                    && from_tier == "routine"
                    && to_tier == "high"
                    && detail == "keeps a high finding after 2 review rounds"
        ),
        "the review nudge row: {rows:?}"
    );
    team.shutdown().await;
}

/// A nudged worker that finishes on the tier it was nudged from logs
/// `nudge_ignored` (design §4.7).
#[tokio::test]
async fn ignored_nudge_is_logged_at_merge() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    run_git(root.path(), &["checkout", "-b", "wt/mnemo"]);
    let mut worker = worker(root.path(), "00000003000000000003");
    worker.status = WorkerStatus::InReview;
    worker.plan_id = Some("plan0001".into());
    worker.plan_step = Some(0);
    worker.tier = Some("routine".into());
    worker.model = Some("test/scripted-average".into());
    worker.routing = Some(WorkerRouting {
        laya_tier: Some("routine".into()),
        confidence: Some(0.62),
        cleared: true,
        lead_tier: Some("routine".into()),
        routed_by: "lead".into(),
        ran_tier: Some("routine".into()),
        // Laya nudged difficult while the worker sat on average, and
        // no reroute ever took it.
        nudged: Some("high".into()),
    });
    provision_agent_worktree(
        root.path().to_path_buf(),
        worker.id.clone(),
        "wt/mnemo".to_string(),
    )
    .await
    .expect("provision the worker checkout");
    std::fs::write(worker.worktree.join("merged.txt"), "merged\n").expect("worker change");
    run_git(&worker.worktree, &["add", "merged.txt"]);
    run_git(&worker.worktree, &["commit", "-m", "worker change"]);
    let tip = phase3::tip(&worker.worktree).await.expect("worker tip");
    worker.checks = vec![WorkerCheck {
        name: "cargo test --workspace".into(),
        status: "passed".into(),
        detail: None,
        tip: tip.clone(),
    }];
    worker.review = Some(ReviewRecord {
        summary: "reviewed".into(),
        tip,
        reviewed_at_ms: 1,
        evidence: Some(evidence()),
    });
    let team = open_test_team(root.path(), state.path(), vec![worker.clone()]).await;
    let merged = team.merge(&worker.id).await.expect("merge the worker");
    assert!(matches!(merged.status, WorkerStatus::Merged), "{merged:?}");
    let rows: Vec<DispatchRoutingRow> =
        std::fs::read_to_string(state.path().join("dispatch_routing.jsonl"))
            .expect("the routing log")
            .lines()
            .map(|line| serde_json::from_str(line).expect("routing row"))
            .collect();
    assert!(
        matches!(
            rows.as_slice(),
            [DispatchRoutingRow::Outcome { status, final_tier, .. },
             DispatchRoutingRow::NudgeIgnored { tier, .. }]
                if status == "merged" && final_tier == "routine" && tier == "routine"
        ),
        "the merged outcome plus the ignored nudge: {rows:?}"
    );
    team.shutdown().await;
}

/// `nudges = false` disables all three kinds: strikes never fire, a
/// hard-read message never reaches Laya, and a second high review
/// round never nudges (design §4.6).
#[tokio::test]
async fn nudges_off_disables_all_three() {
    let settings = crate::mnemo2::dispatch_routing::RoutingSettings {
        mode: crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
        nudges: false,
        ..crate::mnemo2::dispatch_routing::RoutingSettings::default()
    };
    // Strikes and drift, on a live scripted worker.
    let responses = (1..=3)
        .map(|attempt| tool_call_batch(&format!("call-{attempt}"), "not_a_real_tool", &format!("{{\"attempt\":{attempt}}}")))
        .chain([vec![
            mnemo::provider::LlmEvent::TextDelta { text: "done".to_string() },
            mnemo::provider::LlmEvent::Finish { reason: mnemo::provider::FinishReason::Stop },
        ]])
        .collect::<Vec<_>>();
    let (fixture, _notices) = nudge_fixture_with(
        settings.clone(),
        routing_answer(mnemo::agent::route_question::LABEL_HARD, 0.91),
        std::time::Duration::ZERO,
        responses,
    )
    .await;
    let result = fixture.dispatch(Some("routine"), None).await;
    assert!(result.success, "{}", result.output);
    let settled = wait_worker_settled(&fixture.team, "routingworker").await;
    assert!(
        fixture
            .rows()
            .into_iter()
            .all(|row| !matches!(row, DispatchRoutingRow::Nudge { .. })),
        "strikes never nudged: {:?}",
        fixture.rows()
    );
    assert!(
        settled.routing.as_ref().expect("routed at dispatch").nudged.is_none(),
        "no strike nudge was claimed: {settled:?}"
    );
    let calls_after_turn = fixture.classifier.calls();
    fixture
        .team
        .message("routingworker", drift_message(), MessageSender::Lead)
        .await
        .expect("deliver the message");
    wait_worker_settled(&fixture.team, "routingworker").await;
    assert_eq!(
        fixture.classifier.calls(),
        calls_after_turn,
        "drift never asked Laya with nudges off"
    );
    assert!(
        fixture
            .rows()
            .into_iter()
            .all(|row| !matches!(row, DispatchRoutingRow::Nudge { .. })),
        "nothing nudged with nudges off: {:?}",
        fixture.rows()
    );
    fixture.team.shutdown().await;

    // The review nudge, on a record-based team with the same settings.
    use crate::mnemo2::dispatch_routing::save_routing_settings;
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    let config = tempfile::tempdir().expect("config");
    init_git(root.path());
    save_routing_settings(config.path(), &settings).expect("save routing settings");
    let mut record = worker(root.path(), "nudgesoff0001");
    record.tier = Some("routine".into());
    record.routing = Some(WorkerRouting {
        laya_tier: Some("routine".into()),
        confidence: Some(0.62),
        cleared: true,
        lead_tier: Some("routine".into()),
        routed_by: "lead".into(),
        ran_tier: Some("routine".into()),
        nudged: None,
    });
    provision_agent_worktree(
        root.path().to_path_buf(),
        record.id.clone(),
        "main".to_string(),
    )
    .await
    .expect("provision the worker checkout");
    let team = open_test_team_with_config(
        root.path(),
        state.path(),
        vec![record.clone()],
        config.path(),
    )
    .await;
    team.set_catalog(
        vec![
            lead_model("scripted-difficult"),
            lead_model("scripted-average"),
            lead_model("scripted-simple"),
        ],
        tiers_dto! {
            difficult: vec!["test/scripted-difficult".into()],
            average: vec!["test/scripted-average".into()],
            simple: vec!["test/scripted-simple".into()],
        },
        Vec::new(),
    );
    team.review(&record.id, "first pass".into(), vec![open_finding("finding1")], evidence())
        .await
        .expect("record the first review");
    team.route_fix(&record.id, "finding1")
        .await
        .expect("route the finding back to the worker");
    team.stop_inner(&record.id)
        .await
        .expect("stop the fixer session");
    let second = team
        .review(&record.id, "second pass".into(), vec![open_finding("finding1")], evidence())
        .await
        .expect("record the second review");
    assert_eq!(second.review_rounds, 2, "{second:?}");
    assert!(
        second.routing.as_ref().expect("routed").nudged.is_none(),
        "the review never nudged: {second:?}"
    );
    let rows = std::fs::read_to_string(state.path().join("dispatch_routing.jsonl"))
        .unwrap_or_default();
    assert!(
        rows.lines().all(|line| !line.contains("\"nudge\"")),
        "nothing was written with nudges off: {rows}"
    );
    team.shutdown().await;
}

/// Review R1: a routing-settings save must reach a team that is already
/// open. The Laya page's Save and the eval's Apply share
/// `apply_routing_settings`, which writes the table and then reloads the
/// live team — no restart, no unrelated model-settings save.
#[tokio::test]
async fn saved_routing_mode_reaches_the_open_team() {
    use crate::mnemo2::dispatch_routing::{load_routing_settings, RoutingMode, RoutingSettings};

    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let config = tempfile::tempdir().expect("config");
    let team =
        open_test_team_with_config(root.path(), state.path(), Vec::new(), config.path()).await;
    assert_eq!(
        team.routing_mode(),
        RoutingMode::Off,
        "no [routing] table yet: off"
    );

    let shadow = RoutingSettings { mode: RoutingMode::Shadow, ..RoutingSettings::default() };
    crate::mnemo2::laya_settings::apply_routing_settings(config.path(), &shadow, Some(&team))
        .expect("save through the command's core");

    assert_eq!(
        team.routing_mode(),
        RoutingMode::Shadow,
        "the open team reloads the saved mode without a restart"
    );
    assert_eq!(
        load_routing_settings(config.path()).mode,
        RoutingMode::Shadow,
        "and the table is on disk for the next open"
    );
    team.shutdown().await;
}

#[tokio::test]
async fn off_mode_never_asks_laya() {
    let fixture = routing_fixture(
        crate::mnemo2::dispatch_routing::RoutingMode::Off,
        routing_answer(mnemo::agent::route_question::LABEL_HARD, 0.91),
        true,
    )
    .await;
    let result = fixture.dispatch(Some("routine"), None).await;
    assert!(result.success, "{}", result.output);
    assert_eq!(fixture.classifier.calls(), 0);
    assert!(!fixture.log_path().exists());
    assert!(fixture
        .team
        .worker("routingworker")
        .await
        .expect("worker")
        .routing
        .is_none());
    fixture.team.shutdown().await;
}

#[tokio::test]
async fn shadow_logs_layas_pick_and_runs_the_lead_tier() {
    let fixture = routing_fixture(
        crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
        routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.62),
        true,
    )
    .await;
    let result = fixture.dispatch(Some("routine"), None).await;
    assert!(result.success, "{}", result.output);
    // The explicit choice never waits on the classification: the label
    // is deferred, not offline.
    assert!(result.output.contains(
        "Laya: classifying in the background; the label will land on the routing record."
    ));
    let worker = fixture.team.worker("routingworker").await.expect("worker");
    assert_eq!(worker.tier.as_deref(), Some("routine"));
    let routing = wait_laya_label(&fixture.team, "routingworker").await;
    assert_eq!(routing.laya_tier.as_deref(), Some("routine"));
    assert_eq!(routing.lead_tier.as_deref(), Some("routine"));
    assert_eq!(routing.routed_by, "lead");
    assert_eq!(routing.confidence, Some(0.62));
    assert!(routing.cleared);
    let rows = fixture.rows();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(matches!(
        &rows[0],
        crate::mnemo2::dispatch_routing::DispatchRoutingRow::Route {
            tier: Some(tier),
            ran_tier: Some(ran),
            ..
        } if tier == "routine" && ran == "routine"
    ));
    let status = team_status_answer(&fixture.team, json!({})).await;
    assert_eq!(status["result"][0]["routing"]["laya_tier"], "routine");
    fixture.team.shutdown().await;
}

#[tokio::test]
async fn shadow_override_is_logged_when_laya_cleared_elsewhere() {
    let fixture = routing_fixture(
        crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
        routing_answer(mnemo::agent::route_question::LABEL_HARD, 0.71),
        true,
    )
    .await;
    assert!(fixture.dispatch(Some("routine"), None).await.success);
    let rows = eventually(|| async {
        let rows = fixture.rows();
        (rows.len() == 2).then_some(rows)
    })
    .await;
    assert_eq!(fixture.classifier.calls(), 1);
    assert!(matches!(
        &rows[1],
        crate::mnemo2::dispatch_routing::DispatchRoutingRow::LeadOverride {
            laya_tier: Some(laya),
            lead_tier,
            ..
        } if laya == "high" && lead_tier == "routine"
    ));
    // `routed_by` resolves at dispatch time (plan c74b1987): the record
    // keeps "lead" even though the corpus logged the override.
    assert_eq!(
        wait_laya_label(&fixture.team, "routingworker").await.routed_by,
        "lead"
    );
    fixture.team.shutdown().await;
}

#[tokio::test]
async fn suggest_auto_uses_a_cleared_laya_tier() {
    let fixture = routing_fixture(
        crate::mnemo2::dispatch_routing::RoutingMode::Suggest,
        routing_answer(mnemo::agent::route_question::LABEL_HARD, 0.71),
        true,
    )
    .await;
    let result = fixture.dispatch(Some("auto"), None).await;
    assert!(result.success, "{}", result.output);
    let worker = fixture.team.worker("routingworker").await.expect("worker");
    assert_eq!(worker.tier.as_deref(), Some("high"));
    assert_eq!(worker.model.as_deref(), Some("test/scripted-difficult"));
    assert_eq!(worker.routing.expect("routing").routed_by, "laya");
    fixture.team.shutdown().await;
}

#[tokio::test]
async fn suggest_auto_refuses_when_unsure() {
    let fixture = routing_fixture(
        crate::mnemo2::dispatch_routing::RoutingMode::Suggest,
        routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.39),
        true,
    )
    .await;
    let result = fixture.dispatch(Some("auto"), None).await;
    assert!(!result.success);
    assert_eq!(result.output, "Laya is unsure (0.39); pass a tier");
    assert!(matches!(
        fixture.rows().as_slice(),
        [crate::mnemo2::dispatch_routing::DispatchRoutingRow::Route {
            skip: None,
            ran_tier: None,
            ..
        }]
    ));
    fixture.team.shutdown().await;
}

#[tokio::test]
async fn suggest_auto_refuses_when_laya_is_offline() {
    let fixture = routing_fixture(
        crate::mnemo2::dispatch_routing::RoutingMode::Suggest,
        None,
        false,
    )
    .await;
    let result = fixture.dispatch(Some("auto"), None).await;
    assert!(!result.success);
    assert_eq!(result.output, "Laya is not running; pass a tier");
    assert!(matches!(
        fixture.rows().as_slice(),
        [crate::mnemo2::dispatch_routing::DispatchRoutingRow::Route {
            skip: Some(reason),
            ..
        }] if reason == "offline"
    ));
    fixture.team.shutdown().await;
}


#[tokio::test]
async fn auto_mode_falls_back_when_unsure() {
    let fixture = routing_fixture(
        crate::mnemo2::dispatch_routing::RoutingMode::Auto,
        routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.39),
        true,
    )
    .await;
    let result = fixture.dispatch(None, None).await;
    assert!(result.success, "{}", result.output);
    assert!(result.output.contains("Laya: unsure (0.39)."));
    let worker = fixture.team.worker("routingworker").await.expect("worker");
    assert_eq!(worker.tier.as_deref(), Some("routine"));
    assert_eq!(worker.routing.expect("routing").routed_by, "lead");
    assert!(matches!(
        fixture.rows().as_slice(),
        [crate::mnemo2::dispatch_routing::DispatchRoutingRow::Route {
            skip: None,
            ran_tier: Some(ran),
            ..
        }] if ran == "routine"
    ));
    fixture.team.shutdown().await;
}

#[tokio::test]
async fn explicit_model_wins_in_auto_mode() {
    let fixture = routing_fixture(
        crate::mnemo2::dispatch_routing::RoutingMode::Auto,
        routing_answer(mnemo::agent::route_question::LABEL_HARD, 0.71),
        true,
    )
    .await;
    let result = fixture.dispatch(None, Some("test/scripted-simple")).await;
    assert!(result.success, "{}", result.output);
    let worker = fixture.team.worker("routingworker").await.expect("worker");
    assert_eq!(worker.model.as_deref(), Some("test/scripted-simple"));
    assert_eq!(worker.tier.as_deref(), Some("trivial"));
    // `routed_by` resolves at dispatch time (plan c74b1987): the override
    // is corpus-only, the record keeps "lead".
    assert_eq!(
        wait_laya_label(&fixture.team, "routingworker").await.routed_by,
        "lead"
    );
    eventually(|| async {
        let rows = fixture.rows();
        (rows.len() == 2).then_some(rows)
    })
    .await;
    fixture.team.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn slow_laya_times_out_as_offline() {
    let fixture = routing_fixture_with(
        crate::mnemo2::dispatch_routing::RoutingSettings {
            mode: crate::mnemo2::dispatch_routing::RoutingMode::Auto,
            ..crate::mnemo2::dispatch_routing::RoutingSettings::default()
        },
        routing_answer(mnemo::agent::route_question::LABEL_HARD, 0.91),
        true,
        std::time::Duration::from_secs(5),
    )
    .await;
    let started = tokio::time::Instant::now();
    let result = fixture.dispatch(None, None).await;
    let elapsed = started.elapsed();
    assert!(result.success, "{}", result.output);
    assert!(elapsed >= std::time::Duration::from_secs(3), "{elapsed:?}");
    assert!(elapsed < std::time::Duration::from_secs(5), "{elapsed:?}");
    assert!(result.output.contains("Laya: offline."));
    assert!(matches!(
        fixture.rows().as_slice(),
        [crate::mnemo2::dispatch_routing::DispatchRoutingRow::Route {
            skip: Some(reason),
            ..
        }] if reason == "offline"
    ));
    fixture.team.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn explicit_choice_dispatch_never_waits_for_the_classification() {
    let fixture = routing_fixture_with(
        crate::mnemo2::dispatch_routing::RoutingSettings {
            mode: crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
            ..crate::mnemo2::dispatch_routing::RoutingSettings::default()
        },
        routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.62),
        true,
        std::time::Duration::from_secs(60),
    )
    .await;
    let started = tokio::time::Instant::now();
    let result = fixture.dispatch(Some("routine"), None).await;
    let elapsed = started.elapsed();
    assert!(result.success, "{}", result.output);
    // The worker starts while the never-resolving classifier is still
    // pending: no 3-second wait — virtual time has not moved at all.
    assert!(elapsed < std::time::Duration::from_secs(3), "{elapsed:?}");
    let worker = fixture.team.worker("routingworker").await.expect("worker");
    let routing = worker.routing.expect("routing");
    assert_eq!(routing.routed_by, "lead");
    assert_eq!(routing.laya_tier, None);
    assert_eq!(routing.confidence, None);
    assert!(!routing.cleared);
    // The background classification still runs — under the same
    // 3-second timeout, so its Route row lands as an offline skip once
    // the never-resolving classifier times out.
    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
    assert!(matches!(
        fixture.rows().as_slice(),
        [crate::mnemo2::dispatch_routing::DispatchRoutingRow::Route {
            skip: Some(reason),
            ..
        }] if reason == "offline"
    ));
    fixture.team.shutdown().await;
}

#[tokio::test]
async fn deferred_label_lands_on_the_record_and_the_ledger() {
    let fixture = routing_fixture(
        crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
        routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.62),
        true,
    )
    .await;
    let result = fixture.dispatch(Some("routine"), None).await;
    assert!(result.success, "{}", result.output);
    // The deferred label lands on the routing record...
    let routing = wait_laya_label(&fixture.team, "routingworker").await;
    assert_eq!(routing.laya_tier.as_deref(), Some("routine"));
    assert_eq!(routing.confidence, Some(0.62));
    assert!(routing.cleared);
    assert_eq!(routing.routed_by, "lead");
    // ...and the ledger effort row records Laya's say next to the
    // lead's routed_by.
    fixture
        .team
        .stop("routingworker")
        .await
        .expect("stop the worker");
    let worker = fixture.team.worker("routingworker").await.expect("worker");
    let review = crate::mnemo2::ledger::plan_review(
        &[worker],
        &fixture.plan_id,
        "Ship the routing plan",
    );
    let report = match review {
        mnemo::app::team_ledger::PlanReview::Ready { report } => report,
        other => panic!("the stopped worker closes the plan: {other:?}"),
    };
    assert!(report.contains("routine 0.62"), "{report}");
    assert!(report.contains("| lead |"), "{report}");
    fixture.team.shutdown().await;
}

#[tokio::test]
async fn deferred_classification_honors_the_permit() {
    let fixture = routing_fixture(
        crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
        routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.62),
        true,
    )
    .await;
    // The permit is held elsewhere: the dispatch still returns at once,
    // and the background classification waits for the same permit
    // before it asks Laya.
    let _permit = fixture.team.routing_permit.acquire().await.expect("permit");
    let result = fixture.dispatch(Some("routine"), None).await;
    assert!(result.success, "{}", result.output);
    assert_eq!(fixture.classifier.calls(), 0);
    drop(_permit);
    wait_route_row(&fixture).await;
    assert_eq!(fixture.classifier.calls(), 1);
    fixture.team.shutdown().await;
}

#[tokio::test]
async fn redispatch_does_not_classify_twice() {
    let fixture = routing_fixture(
        crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
        routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.62),
        true,
    )
    .await;
    assert!(fixture.dispatch(Some("routine"), None).await.success);
    let reused = fixture.dispatch(Some("routine"), None).await;
    assert!(reused.success, "{}", reused.output);
    assert!(reused.output.contains("\"reused\":true"));
    // Only the first dispatch classifies — in the background; the reuse
    // drops its deferred classification entirely.
    wait_route_row(&fixture).await;
    assert_eq!(fixture.classifier.calls(), 1);
    assert_eq!(fixture.rows().len(), 1);
    assert_eq!(
        wait_laya_label(&fixture.team, "routingworker")
            .await
            .confidence,
        Some(0.62)
    );
    fixture.team.shutdown().await;
}

#[tokio::test]
async fn user_model_change_logs_a_user_reroute() {
    let fixture = routing_fixture(
        crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
        routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.62),
        true,
    )
    .await;
    assert!(fixture.dispatch(Some("routine"), None).await.success);
    // The deferred Route row lands before the reroute rows below.
    wait_route_row(&fixture).await;
    // A pending nudge travels with the reroute row and leaves the record.
    fixture
        .team
        .update_worker("routingworker", |worker| {
            if let Some(routing) = worker.routing.as_mut() {
                routing.nudged = Some("high".into());
            }
        })
        .await;
    fixture
        .team
        .change_model(
            "routingworker",
            Some("test/scripted-difficult".into()),
            None,
            ChangeSource::User,
        )
        .await
        .expect("the user moves the worker onto difficult");
    let rows = fixture.rows();
    assert_eq!(rows.len(), 2, "{rows:?}");
    match &rows[1] {
        DispatchRoutingRow::UserReroute {
            worker_id,
            from_tier,
            to_tier,
            from_model,
            to_model,
            nudged,
            ..
        } => {
            assert_eq!(worker_id, "routingworker");
            assert_eq!(from_tier, "routine");
            assert_eq!(to_tier, "high");
            assert_eq!(from_model.as_deref(), Some("test/scripted-average"));
            assert_eq!(to_model.as_deref(), Some("test/scripted-difficult"));
            assert!(*nudged, "the pending nudge rides the row");
        }
        other => panic!("a user reroute row is logged: {other:?}"),
    }
    let worker = fixture.team.worker("routingworker").await.expect("worker");
    assert_eq!(worker.reroutes.len(), 1);
    assert!(
        worker
            .routing
            .as_ref()
            .is_some_and(|routing| routing.nudged.is_none()),
        "the nudge leaves the record: {worker:?}"
    );
    fixture.team.shutdown().await;
}

#[tokio::test]
async fn lead_resume_onto_another_tier_logs_a_lead_reroute() {
    let fixture = routing_fixture(
        crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
        routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.62),
        true,
    )
    .await;
    assert!(fixture.dispatch(Some("routine"), None).await.success);
    // The deferred Route row lands before the reroute rows below.
    wait_route_row(&fixture).await;
    let resumed = fixture
        .team
        .handle_lead_host_tool(
            "team_resume",
            json!({"worker_id": "routingworker", "tier": "high"}),
        )
        .await;
    assert!(resumed.success, "{}", resumed.output);
    let worker = fixture.team.worker("routingworker").await.expect("worker");
    assert_eq!(worker.tier.as_deref(), Some("high"), "{worker:?}");
    let rows = fixture.rows();
    assert_eq!(rows.len(), 2, "{rows:?}");
    match &rows[1] {
        DispatchRoutingRow::LeadReroute {
            worker_id,
            from_tier,
            to_tier,
            nudged,
            ..
        } => {
            assert_eq!(worker_id, "routingworker");
            assert_eq!(from_tier, "routine");
            assert_eq!(to_tier, "high");
            assert!(!*nudged);
        }
        other => panic!("a lead reroute row is logged: {other:?}"),
    }
    assert_eq!(worker.reroutes.len(), 1);
    fixture.team.shutdown().await;
}

#[tokio::test]
async fn same_tier_switch_logs_without_counting_as_override() {
    let fixture = routing_fixture(
        crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
        routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.62),
        true,
    )
    .await;
    fixture.team.set_catalog(
        vec![
            lead_model("scripted-difficult"),
            lead_model("scripted-average"),
            lead_model("scripted-average2"),
            lead_model("scripted-simple"),
        ],
        tiers_dto! {
            difficult: vec!["test/scripted-difficult".into()],
            average: vec![
                "test/scripted-average".into(),
                "test/scripted-average2".into(),
            ],
            simple: vec!["test/scripted-simple".into()],
        },
        Vec::new(),
    );
    assert!(fixture.dispatch(Some("routine"), None).await.success);
    // The deferred Route row lands before the reroute rows below.
    wait_route_row(&fixture).await;
    fixture
        .team
        .change_model(
            "routingworker",
            Some("test/scripted-average2".into()),
            None,
            ChangeSource::User,
        )
        .await
        .expect("the user picks another model inside the average tier");
    let rows = fixture.rows();
    match &rows[1] {
        DispatchRoutingRow::UserReroute { from_tier, to_tier, .. } => {
            assert_eq!(from_tier, "routine");
            assert_eq!(
                to_tier, "routine",
                "a same-tier switch logs equal tiers"
            );
        }
        other => panic!("a reroute row is still logged: {other:?}"),
    }
    // Equal tiers never count as an override: there is no effective
    // reroute, so with two fix rounds the merged label follows the
    // no-reroute rule (the tier above, weak) instead of the reroute
    // rule (average, strong).
    let worker = fixture.team.worker("routingworker").await.expect("worker");
    let (label, weak) = outcome_label(&worker.reroutes, "merged", "routine", 2, 0)
        .expect("a label still derives");
    assert_eq!(label, "medium");
    assert!(weak);
    fixture.team.shutdown().await;
}

#[tokio::test]
async fn merge_appends_an_outcome_with_the_derived_label() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    run_git(root.path(), &["checkout", "-b", "wt/mnemo"]);
    let mut worker = worker(root.path(), "00000002000000000002");
    worker.status = WorkerStatus::InReview;
    worker.plan_id = Some("plan0001".into());
    worker.plan_step = Some(0);
    worker.tier = Some("routine".into());
    worker.model = Some("test/scripted-average".into());
    worker.routing = Some(WorkerRouting {
        laya_tier: Some("routine".into()),
        confidence: Some(0.62),
        cleared: true,
        lead_tier: Some("routine".into()),
        routed_by: "lead".into(),
        ran_tier: Some("routine".into()),
        nudged: None,
    });
    // The worker was rerouted up to difficult before its review closed.
    worker.reroutes = vec![DispatchRoutingRow::LeadReroute {
        ts: 1,
        worker_id: worker.id.clone(),
        plan_id: "plan0001".into(),
        step: 1,
        from_tier: "routine".into(),
        to_tier: "high".into(),
        from_model: Some("test/scripted-average".into()),
        to_model: Some("test/scripted-difficult".into()),
        status: "idle".into(),
        review_rounds: 0,
        fix_rounds: 0,
        nudged: false,
    }];
    provision_agent_worktree(
        root.path().to_path_buf(),
        worker.id.clone(),
        "wt/mnemo".to_string(),
    )
    .await
    .expect("provision the worker checkout");
    std::fs::write(worker.worktree.join("merged.txt"), "merged\n").expect("worker change");
    run_git(&worker.worktree, &["add", "merged.txt"]);
    run_git(&worker.worktree, &["commit", "-m", "worker change"]);
    let tip = phase3::tip(&worker.worktree).await.expect("worker tip");
    worker.checks = vec![WorkerCheck {
        name: "cargo test --workspace".into(),
        status: "passed".into(),
        detail: None,
        tip: tip.clone(),
    }];
    worker.review = Some(ReviewRecord {
        summary: "reviewed".into(),
        tip,
        reviewed_at_ms: 1,
        evidence: Some(evidence()),
    });
    let team = open_test_team(root.path(), state.path(), vec![worker.clone()]).await;
    let merged = team.merge(&worker.id).await.expect("merge the worker");
    assert!(matches!(merged.status, WorkerStatus::Merged), "{merged:?}");
    let rows: Vec<DispatchRoutingRow> =
        std::fs::read_to_string(state.path().join("dispatch_routing.jsonl"))
            .expect("the routing log")
            .lines()
            .map(|line| serde_json::from_str(line).expect("routing row"))
            .collect();
    // The reroute up to difficult, merged with no fix rounds and no high
    // findings, derives the strong difficult label (design §4.8 rule 2).
    assert!(
        matches!(
            rows.as_slice(),
            [DispatchRoutingRow::Outcome {
                worker_id,
                plan_id,
                status,
                final_tier,
                label: Some(label),
                review_rounds,
                fix_rounds,
                ..
            }]
                if worker_id == "00000002000000000002"
                    && plan_id == "plan0001"
                    && status == "merged"
                    && final_tier == "high"
                    && label == "high"
                    && *review_rounds == 0
                    && *fix_rounds == 0
        ),
        "the merged outcome row: {rows:?}"
    );
    team.shutdown().await;
}

#[tokio::test]
async fn failed_worker_outcome_has_no_label() {
    // Routing on, so the worker carries a decision and its terminal
    // failure is logged (with routing off nothing is written at all).
    let fixture = routing_fixture(
        crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
        routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.62),
        true,
    )
    .await;
    // A forced spec the engine cannot resolve to an endpoint: the
    // dispatch records the worker, its session fails to spawn, and the
    // worker lands Failed (the same path as a real host failure).
    fixture.team.set_catalog(
        vec![LeadModel {
            endpoint: "broken".into(),
            model: "broken".into(),
            spec: "broken".into(),
            is_default: false,
            reasoning_efforts: Vec::new(),
            default_effort: None,
        }],
        mnemo::headless::TiersDto::default(),
        Vec::new(),
    );
    let result = fixture.dispatch(Some("routine"), Some("broken")).await;
    assert!(!result.success, "{result:?}");
    let worker = fixture
        .team
        .worker("routingworker")
        .await
        .expect("the failed worker is tracked");
    assert!(
        matches!(worker.status, WorkerStatus::Failed),
        "{worker:?}"
    );
    // Failed-and-stopped is terminal (design §4.7): the dispatch's
    // deferred route row is followed by one outcome row, and no label —
    // the worker never merged.
    wait_route_row(&fixture).await;
    let rows = eventually(|| async {
        let rows = fixture.rows();
        matches!(rows.last(), Some(DispatchRoutingRow::Outcome { .. })).then_some(rows)
    })
    .await;
    assert!(
        matches!(
            rows.last(),
            Some(DispatchRoutingRow::Outcome {
                worker_id,
                status,
                label: None,
                ..
            })
                if worker_id == "routingworker" && status == "failed"
        ),
        "one unlabeled outcome row: {rows:?}"
    );
    fixture.team.shutdown().await;
}

#[tokio::test]
async fn reused_dispatch_line_keeps_the_dispatch_time_tier() {
    let fixture = routing_fixture(
        crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
        routing_answer(mnemo::agent::route_question::LABEL_HARD, 0.91),
        true,
    )
    .await;
    // Laya clears difficult, the lead passes average: an override that
    // runs average — deferred, so the dispatch answers before the label.
    let first = fixture.dispatch(Some("routine"), None).await;
    assert!(first.success, "{}", first.output);
    assert!(
        first.output.contains(
            "Laya: classifying in the background; the label will land on the routing record."
        ),
        "{}",
        first.output
    );
    // The label lands: Laya cleared difficult while the lead chose
    // average — an override in the corpus, `lead` on the record.
    let routing = wait_laya_label(&fixture.team, "routingworker").await;
    assert_eq!(routing.laya_tier.as_deref(), Some("high"));
    assert_eq!(routing.routed_by, "lead");
    // The worker's tier changes afterwards, on the same model: the
    // record's mutable tier now reads 'difficult' while the dispatch ran
    // average. (A switch that also moved the model is refused by the
    // reuse guard, correctly — only the tier moved here.)
    fixture
        .team
        .record_tier("routingworker", "high")
        .await;
    let worker = fixture.team.worker("routingworker").await.expect("worker");
    assert_eq!(worker.tier.as_deref(), Some("high"), "{worker:?}");
    // An idempotent re-dispatch reports the dispatch-time ran tier, not
    // the tier the worker was rerouted onto (audit finding M1).
    let reused = fixture.dispatch(Some("routine"), None).await;
    assert!(reused.success, "{}", reused.output);
    assert!(
        reused.output.contains("Laya would pick high (0.91)."),
        "the reused dispatch keeps the dispatch-time tier: {}",
        reused.output
    );
    assert!(
        !reused.output.contains("you ran difficult (requested average)"),
        "{}",
        reused.output
    );
    fixture.team.shutdown().await;
}

#[tokio::test]
async fn off_mode_model_change_logs_the_user_reroute() {
    let fixture = routing_fixture(
        crate::mnemo2::dispatch_routing::RoutingMode::Off,
        routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.62),
        false,
    )
    .await;
    assert!(fixture.dispatch(Some("routine"), None).await.success);
    // No hand reroute yet: off mode keeps the log absent.
    assert!(!fixture.log_path().exists());
    fixture
        .team
        .change_model(
            "routingworker",
            Some("test/scripted-difficult".into()),
            None,
            ChangeSource::User,
        )
        .await
        .expect("the user moves the worker");
    // A hand-moved model is a learning even with routing off (decided
    // 2026-11-04): exactly one user reroute row lands in the log, and
    // the record's tier is the destination tier — final_tier and
    // team_status agree instead of showing null.
    let rows = fixture.rows();
    assert_eq!(rows.len(), 1, "{rows:?}");
    match &rows[0] {
        DispatchRoutingRow::UserReroute {
            worker_id,
            from_tier,
            to_tier,
            from_model,
            to_model,
            ..
        } => {
            assert_eq!(worker_id, "routingworker");
            assert_eq!(from_tier, "routine");
            assert_eq!(to_tier, "high");
            assert_eq!(from_model.as_deref(), Some("test/scripted-average"));
            assert_eq!(to_model.as_deref(), Some("test/scripted-difficult"));
        }
        other => panic!("a user reroute row is logged: {other:?}"),
    }
    let worker = fixture.team.worker("routingworker").await.expect("worker");
    assert_eq!(worker.reroutes.len(), 1, "{worker:?}");
    assert_eq!(worker.tier.as_deref(), Some("high"), "{worker:?}");
    assert_eq!(final_tier(&worker), "high");
    let status = team_status_answer(&fixture.team, json!({})).await;
    assert_eq!(status["result"][0]["tier"], "high");
    fixture.team.shutdown().await;
}

#[tokio::test]
async fn off_mode_unrerouted_merge_writes_no_outcome_row() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    run_git(root.path(), &["checkout", "-b", "wt/mnemo"]);
    let mut worker = worker(root.path(), "00000002000000000003");
    worker.status = WorkerStatus::InReview;
    worker.plan_id = Some("plan0001".into());
    worker.plan_step = Some(0);
    worker.tier = Some("routine".into());
    worker.model = Some("test/scripted-average".into());
    // No dispatch decision and no hand reroute: this worker ran with
    // routing off and was never moved by a hand, so it stays off the
    // calibration log.
    worker.routing = None;
    provision_agent_worktree(
        root.path().to_path_buf(),
        worker.id.clone(),
        "wt/mnemo".to_string(),
    )
    .await
    .expect("provision the worker checkout");
    std::fs::write(worker.worktree.join("merged.txt"), "merged\n").expect("worker change");
    run_git(&worker.worktree, &["add", "merged.txt"]);
    run_git(&worker.worktree, &["commit", "-m", "worker change"]);
    let tip = phase3::tip(&worker.worktree).await.expect("worker tip");
    worker.checks = vec![WorkerCheck {
        name: "cargo test --workspace".into(),
        status: "passed".into(),
        detail: None,
        tip: tip.clone(),
    }];
    worker.review = Some(ReviewRecord {
        summary: "reviewed".into(),
        tip,
        reviewed_at_ms: 1,
        evidence: Some(evidence()),
    });
    let team = open_test_team(root.path(), state.path(), vec![worker.clone()]).await;
    let merged = team.merge(&worker.id).await.expect("merge the worker");
    assert!(matches!(merged.status, WorkerStatus::Merged), "{merged:?}");
    assert!(
        !state.path().join("dispatch_routing.jsonl").exists(),
        "off mode writes no outcome row for a worker with no reroute"
    );
    team.shutdown().await;
}

#[tokio::test]
async fn off_mode_rerouted_merge_writes_the_outcome_row() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    run_git(root.path(), &["checkout", "-b", "wt/mnemo"]);
    let mut worker = worker(root.path(), "00000002000000000004");
    worker.status = WorkerStatus::InReview;
    worker.plan_id = Some("plan0001".into());
    worker.plan_step = Some(0);
    worker.tier = Some("routine".into());
    worker.model = Some("test/scripted-average".into());
    // No dispatch decision: this worker ran with routing off — but the
    // user moved it to difficult by hand, and a hand reroute is a
    // learning in every mode (decided 2026-11-04), so its terminal
    // outcome row is calibration data.
    worker.routing = None;
    worker.reroutes = vec![DispatchRoutingRow::UserReroute {
        ts: 1,
        worker_id: worker.id.clone(),
        plan_id: "plan0001".into(),
        step: 1,
        from_tier: "routine".into(),
        to_tier: "high".into(),
        from_model: Some("test/scripted-average".into()),
        to_model: Some("test/scripted-difficult".into()),
        status: "idle".into(),
        review_rounds: 0,
        fix_rounds: 0,
        nudged: false,
    }];
    provision_agent_worktree(
        root.path().to_path_buf(),
        worker.id.clone(),
        "wt/mnemo".to_string(),
    )
    .await
    .expect("provision the worker checkout");
    std::fs::write(worker.worktree.join("merged.txt"), "merged\n").expect("worker change");
    run_git(&worker.worktree, &["add", "merged.txt"]);
    run_git(&worker.worktree, &["commit", "-m", "worker change"]);
    let tip = phase3::tip(&worker.worktree).await.expect("worker tip");
    worker.checks = vec![WorkerCheck {
        name: "cargo test --workspace".into(),
        status: "passed".into(),
        detail: None,
        tip: tip.clone(),
    }];
    worker.review = Some(ReviewRecord {
        summary: "reviewed".into(),
        tip,
        reviewed_at_ms: 1,
        evidence: Some(evidence()),
    });
    let team = open_test_team(root.path(), state.path(), vec![worker.clone()]).await;
    let merged = team.merge(&worker.id).await.expect("merge the worker");
    assert!(matches!(merged.status, WorkerStatus::Merged), "{merged:?}");
    let rows: Vec<DispatchRoutingRow> =
        std::fs::read_to_string(state.path().join("dispatch_routing.jsonl"))
            .expect("the routing log")
            .lines()
            .map(|line| serde_json::from_str(line).expect("routing row"))
            .collect();
    // The hand reroute makes the off-mode worker calibration data: its
    // merged outcome row is written, labelled difficult by the reroute
    // rule (design §4.8 rule 2).
    assert!(
        matches!(
            rows.as_slice(),
            [DispatchRoutingRow::Outcome {
                worker_id,
                status,
                final_tier,
                label: Some(label),
                ..
            }]
                if worker_id == "00000002000000000004"
                    && status == "merged"
                    && final_tier == "high"
                    && label == "high"
        ),
        "the rerouted worker's outcome row: {rows:?}"
    );
    team.shutdown().await;
}

#[tokio::test]
async fn routed_worker_still_logs_after_routing_is_turned_off() {
    let fixture = routing_fixture(
        crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
        routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.62),
        true,
    )
    .await;
    assert!(fixture.dispatch(Some("routine"), None).await.success);
    // The deferred Route row lands before the reroute row below.
    wait_route_row(&fixture).await;
    // Routing is switched off after the worker was routed: the worker's
    // own decision still makes it calibration data.
    *fixture.team.routing.write().unwrap_or_else(|e| e.into_inner()) =
        crate::mnemo2::dispatch_routing::RoutingSettings::default();
    fixture
        .team
        .change_model(
            "routingworker",
            Some("test/scripted-difficult".into()),
            None,
            ChangeSource::User,
        )
        .await
        .expect("the user moves the routed worker");
    let rows = fixture.rows();
    assert!(
        matches!(
            rows.as_slice(),
            [_, DispatchRoutingRow::UserReroute { worker_id, .. }]
                if worker_id == "routingworker"
        ),
        "a routed worker still logs its reroute: {rows:?}"
    );
    fixture.team.shutdown().await;
}

#[tokio::test]
async fn audit_dispatch_is_never_classified() {
    let fixture = routing_fixture(
        crate::mnemo2::dispatch_routing::RoutingMode::Off,
        routing_answer(mnemo::agent::route_question::LABEL_HARD, 0.91),
        true,
    )
    .await;
    assert!(fixture.dispatch(Some("routine"), None).await.success);
    fixture
        .team
        .stop("routingworker")
        .await
        .expect("stop the audit target");
    // Routing off and the worker was dispatched without a decision: the
    // stop writes nothing at all.
    assert!(
        fixture.rows().is_empty(),
        "off mode writes no stop outcome row"
    );
    *fixture.team.routing.write().unwrap_or_else(|e| e.into_inner()) =
        crate::mnemo2::dispatch_routing::RoutingSettings {
            mode: crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
            ..crate::mnemo2::dispatch_routing::RoutingSettings::default()
        };
    let result = fixture
        .team
        .handle_lead_host_tool(
            "team_audit_dispatch",
            json!({
                "worker_id": "routingworker",
                "reviewer_id": "routingaudit",
                "plan_id": fixture.plan_id,
                "step": 1,
                "brief": "Audit the implementation independently against the complete step specification.",
            }),
        )
        .await;
    assert!(result.success, "{}", result.output);
    assert_eq!(fixture.classifier.calls(), 0);
    // The audit dispatch classifies nothing and logs nothing: not even a
    // shadow-mode row appears for it.
    assert!(
        !fixture.log_path().exists(),
        "an audit dispatch adds no routing row"
    );
    fixture.team.shutdown().await;
}

#[tokio::test]
async fn schema_offers_auto_only_in_suggest_and_auto() {
    for (mode, offers_auto) in [
        (crate::mnemo2::dispatch_routing::RoutingMode::Off, false),
        (crate::mnemo2::dispatch_routing::RoutingMode::Shadow, false),
        (crate::mnemo2::dispatch_routing::RoutingMode::Suggest, true),
        (crate::mnemo2::dispatch_routing::RoutingMode::Auto, true),
    ] {
        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        let team = open_test_team(root.path(), state.path(), Vec::new()).await;
        team.set_catalog(
            vec![lead_model("scripted-model")],
            tiers_dto! {
                simple: vec!["test/scripted-model".into()],
                average: vec!["test/scripted-model".into()],
                difficult: vec!["test/scripted-model".into()],
            },
            Vec::new(),
        );
        *team.routing.write().unwrap_or_else(|e| e.into_inner()) =
            crate::mnemo2::dispatch_routing::RoutingSettings {
                mode,
                ..crate::mnemo2::dispatch_routing::RoutingSettings::default()
            };
        let tools = team.lead_host_tools();
        let dispatch = tools.iter().find(|tool| tool.name == "team_dispatch").expect("dispatch");
        let tiers = dispatch.parameters["properties"]["tier"]["enum"]
            .as_array()
            .expect("tier enum");
        assert_eq!(tiers.contains(&json!("auto")), offers_auto, "{mode:?}");
    }
}

#[tokio::test]
async fn tier_is_optional_only_in_auto_mode() {
    for (mode, required) in [
        (crate::mnemo2::dispatch_routing::RoutingMode::Off, true),
        (crate::mnemo2::dispatch_routing::RoutingMode::Shadow, true),
        (crate::mnemo2::dispatch_routing::RoutingMode::Suggest, true),
        (crate::mnemo2::dispatch_routing::RoutingMode::Auto, false),
    ] {
        let root = tempfile::tempdir().expect("workspace");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        let team = open_test_team(root.path(), state.path(), Vec::new()).await;
        team.set_catalog(
            vec![lead_model("scripted-model")],
            tiers_dto! {
                simple: vec!["test/scripted-model".into()],
                average: vec!["test/scripted-model".into()],
                difficult: vec!["test/scripted-model".into()],
            },
            Vec::new(),
        );
        *team.routing.write().unwrap_or_else(|e| e.into_inner()) =
            crate::mnemo2::dispatch_routing::RoutingSettings {
                mode,
                ..crate::mnemo2::dispatch_routing::RoutingSettings::default()
            };
        let tools = team.lead_host_tools();
        let dispatch = tools.iter().find(|tool| tool.name == "team_dispatch").expect("dispatch");
        let required_fields = dispatch.parameters["required"]
            .as_array()
            .expect("required fields");
        assert_eq!(required_fields.contains(&json!("tier")), required, "{mode:?}");
    }
}


#[tokio::test]
async fn suggest_auto_refuses_when_laya_returns_no_answer() {
    let fixture = routing_fixture(
        crate::mnemo2::dispatch_routing::RoutingMode::Suggest,
        None,
        true,
    )
    .await;
    let result = fixture.dispatch(Some("auto"), None).await;
    assert!(!result.success);
    assert_eq!(result.output, "Laya is not running; pass a tier");
    assert_eq!(fixture.classifier.calls(), 1);
    assert!(matches!(
        fixture.rows().as_slice(),
        [crate::mnemo2::dispatch_routing::DispatchRoutingRow::Route {
            skip: Some(reason),
            ..
        }] if reason == "offline"
    ));
    fixture.team.shutdown().await;
}

#[tokio::test]
async fn classification_happens_outside_merge_lock() {
    let fixture = std::sync::Arc::new(
        routing_fixture(
            crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
            routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.62),
            true,
        )
        .await,
    );
    let merge_guard = fixture.team.merge_lock.lock().await;
    let dispatch_team = std::sync::Arc::clone(&fixture.team);
    let plan_id = fixture.plan_id.clone();
    let dispatch = tokio::spawn(async move {
        dispatch_team
            .dispatch_on_plan(
                "Implement the requested routing behavior and verify it with focused regression tests".into(),
                None,
                None,
                Some("routingworker".into()),
                None,
                false,
                &plan_id,
                1,
            )
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while fixture.classifier.calls() == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("classification runs while merge_lock is held elsewhere");
    assert!(!dispatch.is_finished(), "dispatch still waits for merge_lock");
    drop(merge_guard);
    let result = dispatch.await.expect("dispatch task");
    let (worker, _) = result.expect("the non-explicit IPC dispatch succeeds");
    assert_eq!(worker.id, "routingworker");
    fixture.team.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn permit_wait_counts_toward_laya_timeout() {
    let fixture = routing_fixture(
        crate::mnemo2::dispatch_routing::RoutingMode::Auto,
        routing_answer(mnemo::agent::route_question::LABEL_HARD, 0.91),
        true,
    )
    .await;
    let _permit = fixture.team.routing_permit.acquire().await.expect("permit");
    let request = DispatchRequest {
        brief: "Use the complete step specification to choose this worker's model tier".into(),
        tier: None,
        dispatch_routing: true,
        source: DispatchSource::Ipc,
        model: None,
        reasoning_effort: None,
        worker_id: Some("permitwait".into()),
        plan: Some(PlanRef {
            plan_id: fixture.plan_id.clone(),
            step: 0,
        }),
        review_risk: None,
        read_back: false,
    };
    let routed = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        fixture.team.route_implementation_dispatch(
            &request,
            "Implement the requested routing behavior in src-tauri/src/mnemo2/team.rs and verify it with focused regression tests",
            &request.brief,
        ),
    )
    .await
    .expect("the permit wait is inside Laya's 3 s timeout")
    .expect("routing decision");
    assert_eq!(routed.tier.as_deref(), Some("routine"));
    assert_eq!(fixture.classifier.calls(), 0);
    assert!(matches!(
        routed.rows.as_slice(),
        [crate::mnemo2::dispatch_routing::DispatchRoutingRow::Route {
            skip: Some(reason),
            ..
        }] if reason == "offline"
    ));
    fixture.team.shutdown().await;
}

#[tokio::test]
async fn shadow_without_tiers_keeps_the_default_model() {
    let fixture = routing_fixture(
        crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
        routing_answer(mnemo::agent::route_question::LABEL_HARD, 0.91),
        true,
    )
    .await;
    fixture.team.set_catalog(
        vec![lead_model("scripted-model")],
        mnemo::headless::TiersDto::default(),
        Vec::new(),
    );
    let result = fixture.dispatch(None, None).await;
    assert!(result.success, "{}", result.output);
    let worker = fixture.team.worker("routingworker").await.expect("worker");
    assert_eq!(worker.model, None);
    assert_eq!(worker.tier, None);
    assert_eq!(fixture.classifier.calls(), 0);
    fixture.team.shutdown().await;
}

#[tokio::test]
async fn team_status_omits_routing_when_none() {
    let fixture = routing_fixture(
        crate::mnemo2::dispatch_routing::RoutingMode::Off,
        None,
        false,
    )
    .await;
    assert!(fixture.dispatch(Some("routine"), None).await.success);
    let compact = team_status_answer(&fixture.team, json!({})).await;
    assert!(compact["result"][0].get("routing").is_none());
    let detailed = team_status_answer(&fixture.team, json!({"detail": true})).await;
    assert!(detailed["result"][0].get("routing").is_none());
    fixture.team.shutdown().await;
}


#[tokio::test]
async fn auto_fallback_without_that_tier_folds_down() {
    let fixture = routing_fixture(
        crate::mnemo2::dispatch_routing::RoutingMode::Auto,
        routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.39),
        true,
    )
    .await;
    fixture.team.set_catalog(
        vec![lead_model("scripted-model")],
        tiers_dto! {
            simple: vec!["test/scripted-model".into()],
            average: Vec::new(),
            difficult: vec!["test/scripted-model".into()],
        },
        Vec::new(),
    );
    let result = fixture.dispatch(None, None).await;
    assert!(result.success, "{}", result.output);
    assert!(result.output.contains("routine has no model; ran on trivial"));
    let worker = fixture.team.worker("routingworker").await.expect("worker");
    assert_eq!(worker.model.as_deref(), Some("test/scripted-model"));
    assert_eq!(worker.tier.as_deref(), Some("trivial"));
    let routing = worker.routing.expect("routing");
    assert_eq!(routing.routed_by, "lead");
    assert_eq!(routing.lead_tier.as_deref(), Some("routine"));
    assert_eq!(routing.ran_tier.as_deref(), Some("trivial"));
    assert_eq!(fixture.classifier.calls(), 1);
    assert!(matches!(
        fixture.rows().as_slice(),
        [crate::mnemo2::dispatch_routing::DispatchRoutingRow::Route {
            skip: None,
            lead_tier: Some(lead_tier),
            ran_tier: Some(ran_tier),
            ..
        }] if lead_tier == "routine" && ran_tier == "trivial"
    ));
    fixture.team.shutdown().await;
}

#[tokio::test]
async fn racing_duplicate_dispatch_logs_one_route_row() {
    let fixture = routing_fixture_with(
        crate::mnemo2::dispatch_routing::RoutingSettings {
            mode: crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
            ..crate::mnemo2::dispatch_routing::RoutingSettings::default()
        },
        routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.62),
        true,
        std::time::Duration::from_millis(50),
    )
    .await;
    let (first, second) = tokio::join!(
        fixture.dispatch(Some("routine"), None),
        fixture.dispatch(Some("routine"), None)
    );
    assert!(first.success, "{}", first.output);
    assert!(second.success, "{}", second.output);
    // Only the provisioning dispatch classifies — in the background;
    // the idempotent reuse drops its deferred classification.
    wait_route_row(&fixture).await;
    assert_eq!(fixture.classifier.calls(), 1);
    assert_eq!(fixture.rows().len(), 1, "exactly one Route row is committed");
    fixture.team.shutdown().await;
}


#[tokio::test]
async fn no_free_slot_writes_no_route_row() {
    let fixture = routing_fixture(
        crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
        routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.62),
        true,
    )
    .await;
    fixture
        .team
        .persistence
        .slots
        .store(0, Ordering::SeqCst);
    let result = fixture.dispatch(Some("routine"), None).await;
    assert!(!result.success);
    assert_eq!(result.output, "all 0 worker slots are busy");
    // The deferred classification is dropped: no worker was provisioned,
    // so nothing classifies and no row is written.
    assert_eq!(fixture.classifier.calls(), 0);
    assert!(!fixture.log_path().exists());
    fixture.team.shutdown().await;
}

#[tokio::test]
async fn provisioning_failure_writes_no_route_row() {
    let fixture = routing_fixture(
        crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
        routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.62),
        true,
    )
    .await;
    let git = fixture._root.path().join(".git");
    let parked = fixture._root.path().join(".git-parked");
    std::fs::rename(&git, &parked).expect("park git metadata");
    let result = fixture.dispatch(Some("routine"), None).await;
    assert!(!result.success);
    assert!(result.output.contains("not a usable Git checkout"));
    // The deferred classification is dropped: provisioning failed, so
    // nothing classifies and no row is written.
    assert_eq!(fixture.classifier.calls(), 0);
    assert!(!fixture.log_path().exists());
    std::fs::rename(parked, git).expect("restore git metadata");
    fixture.team.shutdown().await;
}


#[test]
fn dispatch_routing_line_truth_table() {
    let routing = WorkerRouting {
        laya_tier: Some("high".into()),
        confidence: Some(0.71),
        cleared: true,
        lead_tier: Some("routine".into()),
        routed_by: "lead_override".into(),
        ran_tier: Some("routine".into()),
        nudged: None,
    };
    assert_eq!(
        dispatch_routing_line(RoutingMode::Shadow, Some(&routing), false).as_deref(),
        Some("Laya: high (0.71) — you chose routine; logged as an override.")
    );
    // The dispatch-time ran tier decides the override line (audit finding
    // M1): a reroute after dispatch never rewrites what this reports.
    let routing = WorkerRouting {
        ran_tier: Some("high".into()),
        ..routing
    };
    assert_eq!(
        dispatch_routing_line(RoutingMode::Shadow, Some(&routing), false).as_deref(),
        Some(
            "Laya: high (0.71) — you ran high (requested routine); logged as an override."
        )
    );
    let routing = WorkerRouting {
        laya_tier: Some("routine".into()),
        confidence: Some(0.62),
        lead_tier: Some("routine".into()),
        routed_by: "lead".into(),
        ..routing
    };
    assert_eq!(
        dispatch_routing_line(RoutingMode::Shadow, Some(&routing), false).as_deref(),
        Some("Laya would pick routine (0.62).")
    );
    assert_eq!(
        dispatch_routing_line(RoutingMode::Suggest, Some(&routing), false).as_deref(),
        Some("Laya: routine (0.62) — matches your tier.")
    );
    assert_eq!(dispatch_routing_line(RoutingMode::Off, Some(&routing), false), None);
    let routing = WorkerRouting {
        routed_by: "laya".into(),
        lead_tier: None,
        ..routing
    };
    assert_eq!(
        dispatch_routing_line(RoutingMode::Auto, Some(&routing), false).as_deref(),
        Some("Laya: routed to routine (0.62).")
    );
    let routing = WorkerRouting {
        confidence: Some(0.39),
        cleared: false,
        routed_by: "lead".into(),
        lead_tier: Some("routine".into()),
        ..routing
    };
    assert_eq!(
        dispatch_routing_line(RoutingMode::Auto, Some(&routing), false).as_deref(),
        Some("Laya: unsure (0.39).")
    );
    let routing = WorkerRouting {
        laya_tier: None,
        confidence: None,
        ..routing
    };
    assert_eq!(
        dispatch_routing_line(RoutingMode::Shadow, Some(&routing), false).as_deref(),
        Some("Laya: offline.")
    );
    // A deferred classification (plan c74b1987) is pending, not offline —
    // and Off never prints a routing line, pending or not.
    assert_eq!(
        dispatch_routing_line(RoutingMode::Shadow, Some(&routing), true).as_deref(),
        Some(
            "Laya: classifying in the background; the label will land on the routing record."
        )
    );
    assert_eq!(dispatch_routing_line(RoutingMode::Off, Some(&routing), true), None);
}

#[tokio::test]
async fn suggest_non_auto_without_tiers_uses_default_model() {
    let fixture = routing_fixture(
        crate::mnemo2::dispatch_routing::RoutingMode::Suggest,
        None,
        false,
    )
    .await;
    fixture.team.set_catalog(
        vec![lead_model("scripted-model")],
        mnemo::headless::TiersDto::default(),
        Vec::new(),
    );
    let result = fixture.dispatch(None, None).await;
    assert!(result.success, "{}", result.output);
    let worker = fixture.team.worker("routingworker").await.expect("worker");
    assert_eq!(worker.model, None);
    assert_eq!(worker.tier, None);
    fixture.team.shutdown().await;
}

#[tokio::test]
async fn gated_answer_has_no_skip_reason() {
    let fixture = routing_fixture(
        crate::mnemo2::dispatch_routing::RoutingMode::Auto,
        routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.39),
        true,
    )
    .await;
    assert!(fixture.dispatch(None, None).await.success);
    assert!(matches!(
        fixture.rows().as_slice(),
        [crate::mnemo2::dispatch_routing::DispatchRoutingRow::Route {
            skip: None,
            cleared: false,
            confidence,
            ..
        }] if (*confidence - 0.39).abs() < f64::EPSILON
    ));
    fixture.team.shutdown().await;
}

#[tokio::test]
async fn off_mode_schema_uses_the_ladder_without_auto() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let team = open_test_team(root.path(), state.path(), Vec::new()).await;
    team.set_catalog(
        vec![lead_model("scripted-model")],
        tiers_dto! {
            simple: vec!["test/scripted-model".into()],
            average: vec!["test/scripted-model".into()],
            difficult: vec!["test/scripted-model".into()],
        },
        Vec::new(),
    );
    let tools = team.lead_host_tools();
    let dispatch = tools.iter().find(|tool| tool.name == "team_dispatch").expect("dispatch");
    let tier = &dispatch.parameters["properties"]["tier"];
    assert_eq!(
        tier["enum"],
        json!(["trivial", "routine", "medium", "high", "escalate"])
    );
    let description = tier["description"].as_str().expect("tier description");
    assert!(description.contains("trivial (Trivial)"));
    assert!(description.contains("routine (Routine)"));
    assert!(description.contains("medium (Medium)"));
    assert!(description.contains("no model; folds down to routine"));
    assert!(description.contains("high (High)"));
    assert!(description.contains("escalation only, never a first pick"));
    assert!(!description.contains("\nauto -"));
    assert_eq!(
        dispatch.parameters["properties"]["model"]["enum"],
        json!(["test/scripted-model"])
    );
    assert!(dispatch.parameters["required"]
        .as_array()
        .is_some_and(|required| required.contains(&json!("tier"))));
}


#[tokio::test]
async fn ipc_dispatch_on_plan_logs_a_shadow_route_row() {
    let fixture = routing_fixture(
        crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
        routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.62),
        true,
    )
    .await;
    let (_, reused) = fixture
        .team
        .dispatch_on_plan(
            "Implement the IPC-dispatched step and run its regression tests".into(),
            Some("test/scripted-average".into()),
            None,
            Some("ipcworker".into()),
            None,
            false,
            &fixture.plan_id,
            1,
        )
        .await
        .expect("IPC dispatch");
    assert!(!reused);
    // The explicit model choice defers the classification: the label
    // and its Route row land in the background.
    let routing = wait_laya_label(&fixture.team, "ipcworker").await;
    assert_eq!(fixture.classifier.calls(), 1);
    assert_eq!(fixture.rows().len(), 1);
    assert_eq!(routing.laya_tier.as_deref(), Some("routine"));
    assert_eq!(routing.lead_tier.as_deref(), Some("routine"));
    assert_eq!(routing.routed_by, "lead");
    fixture.team.shutdown().await;
}


#[tokio::test]
async fn dispatch_uses_every_ladder_tier_and_refuses_legacy_ids() {
    for (tier, model, ran_tier, fold_note) in [
        ("trivial", "test/scripted-simple", "trivial", None),
        ("routine", "test/scripted-average", "routine", None),
        (
            "medium",
            "test/scripted-average",
            "routine",
            Some("medium has no model; ran on routine"),
        ),
        ("high", "test/scripted-difficult", "high", None),
    ] {
        let fixture = routing_fixture(
            crate::mnemo2::dispatch_routing::RoutingMode::Off,
            None,
            false,
        )
        .await;
        let result = fixture.dispatch(Some(tier), None).await;
        assert!(result.success, "tier {tier}: {}", result.output);
        if let Some(note) = fold_note {
            assert!(result.output.contains(note), "{}", result.output);
        }
        let worker = fixture.team.worker("routingworker").await.expect("worker");
        assert_eq!(worker.model.as_deref(), Some(model), "tier {tier}");
        assert_eq!(worker.tier.as_deref(), Some(ran_tier), "tier {tier}");
        fixture.team.shutdown().await;
    }

    let fixture = routing_fixture(
        crate::mnemo2::dispatch_routing::RoutingMode::Off,
        None,
        false,
    )
    .await;
    for old_id in ["simple", "average", "difficult"] {
        let result = fixture.dispatch(Some(old_id), None).await;
        assert!(!result.success, "legacy tier {old_id} was accepted");
        assert!(result.output.contains(old_id), "{}", result.output);
        assert!(
            result
                .output
                .contains("trivial, routine, medium, high, escalate"),
            "{}",
            result.output
        );
    }
    let result = fixture.dispatch(Some("escalate"), None).await;
    assert!(!result.success, "tier escalate was accepted as a first pick");
    assert!(
        result.output.contains("escalation only, never a first pick"),
        "{}",
        result.output
    );
    fixture.team.shutdown().await;
}

#[tokio::test]
async fn lead_dispatch_refuses_escalate_in_every_routing_mode() {
    for mode in [
        crate::mnemo2::dispatch_routing::RoutingMode::Off,
        crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
        crate::mnemo2::dispatch_routing::RoutingMode::Suggest,
        crate::mnemo2::dispatch_routing::RoutingMode::Auto,
    ] {
        let fixture = routing_fixture(mode, None, false).await;
        let result = fixture.dispatch(Some("escalate"), None).await;
        assert!(
            !result.success,
            "routing mode {mode:?}: escalate was accepted as a first pick"
        );
        assert!(
            result.output.contains("escalation only, never a first pick"),
            "routing mode {mode:?}: {}",
            result.output
        );
        assert!(
            fixture.team.worker("routingworker").await.is_err(),
            "routing mode {mode:?}: the refused dispatch provisioned a worker"
        );
        fixture.team.shutdown().await;
    }
}

#[test]
fn legacy_team_tier_ids_convert_once_with_a_preserved_backup() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    let mut legacy = worker(root.path(), "legacyworker1");
    legacy.tier = Some("simple".into());
    legacy.routing = Some(WorkerRouting {
        laya_tier: Some("average".into()),
        confidence: Some(0.9),
        cleared: true,
        lead_tier: Some("simple".into()),
        routed_by: "lead_override".into(),
        ran_tier: Some("difficult".into()),
        nudged: Some("average".into()),
    });
    legacy.reroutes = vec![DispatchRoutingRow::UserReroute {
        ts: 1,
        worker_id: legacy.id.clone(),
        plan_id: "plan0001".into(),
        step: 1,
        from_tier: "simple".into(),
        to_tier: "difficult".into(),
        from_model: None,
        to_model: None,
        status: "idle".into(),
        review_rounds: 0,
        fix_rounds: 0,
        nudged: false,
    }];
    let persisted = PersistedTeam {
        workers: vec![legacy],
        ..PersistedTeam::default()
    };
    let original = serde_json::to_vec_pretty(&persisted).expect("legacy JSON");
    let path = state.path().join(TEAM_STATE_FILE);
    std::fs::write(&path, &original).expect("write legacy team");
    let persistence = TeamPersistence::new(state.path());

    let converted = persistence
        .load()
        .expect("load legacy team")
        .expect("legacy team exists");
    let converted_worker = &converted.workers[0];
    assert_eq!(converted_worker.tier.as_deref(), Some("trivial"));
    let routing = converted_worker.routing.as_ref().expect("routing");
    assert_eq!(routing.laya_tier.as_deref(), Some("routine"));
    assert_eq!(routing.lead_tier.as_deref(), Some("trivial"));
    assert_eq!(routing.ran_tier.as_deref(), Some("high"));
    assert_eq!(routing.nudged.as_deref(), Some("routine"));
    assert!(matches!(
        &converted_worker.reroutes[0],
        DispatchRoutingRow::UserReroute { from_tier, to_tier, .. }
            if from_tier == "trivial" && to_tier == "high"
    ));
    let backup = state.path().join(TEAM_PRE_TIERS_BACKUP);
    assert_eq!(std::fs::read(&backup).expect("pre-tier backup"), original);

    // A second legacy load converts the live file but never replaces the
    // first backup's original bytes.
    std::fs::write(&path, &original).expect("write legacy team again");
    persistence.load().expect("second conversion").expect("team");
    assert_eq!(std::fs::read(&backup).expect("backup remains"), original);
}

#[tokio::test]
async fn audit_defaults_to_high_and_unreported_audits_switch_tiers() {
    let fixture = routing_fixture(
        crate::mnemo2::dispatch_routing::RoutingMode::Off,
        None,
        false,
    )
    .await;
    let team = &fixture.team;
    let mut target = worker(fixture._root.path(), "audittarget1");
    target.plan_id = Some(fixture.plan_id.clone());
    target.plan_step = Some(1);
    provision_agent_worktree(
        fixture._root.path().to_path_buf(),
        target.id.clone(),
        "main".to_string(),
    )
    .await
    .expect("provision audit target");
    team.records.lock().await.push(target.clone());

    let (audit, reused) = team
        .audit_dispatch(&target.id, "auditdefault1", None, None, None)
        .await
        .expect("default-tier audit");
    assert!(!reused);
    assert_eq!(audit.tier.as_deref(), Some("high"));
    assert_eq!(audit.model.as_deref(), Some("test/scripted-difficult"));
    let immediate = worker_status_view(&audit, false);
    assert_eq!(immediate["tier"], json!("high"));
    assert_eq!(immediate["model"], json!("test/scripted-difficult"));

    let switched = team
        .change_model(
            &audit.id,
            Some("test/scripted-average".into()),
            None,
            ChangeSource::User,
        )
        .await
        .expect("unreported audit model switch");
    assert_eq!(switched.tier.as_deref(), Some("routine"));
    assert_eq!(switched.model.as_deref(), Some("test/scripted-average"));

    let resumed = team
        .handle_lead_host_tool(
            "team_resume",
            json!({"worker_id": audit.id, "tier": "high"}),
        )
        .await;
    assert!(resumed.success, "{}", resumed.output);
    let switched = team.worker(&audit.id).await.expect("switched audit");
    assert_eq!(switched.tier.as_deref(), Some("high"));
    assert_eq!(switched.model.as_deref(), Some("test/scripted-difficult"));
    assert!(!switched.report_delivered);
    assert!(switched.reroutes.len() >= 2, "audit switches stay in its ledger");
    assert!(fixture.rows().is_empty(), "audit switches are not training labels");
    team.shutdown().await;
}

#[tokio::test]
async fn tier_plus_cross_tier_model_logs_the_ran_tier() {
    for (requested, ran, laya_label, expected_effort) in [
        ("trivial", "high", mnemo::agent::route_question::LABEL_TRIVIAL, "high"),
        ("high", "trivial", mnemo::agent::route_question::LABEL_HARD, "low"),
    ] {
        let fixture = routing_fixture_with(
            crate::mnemo2::dispatch_routing::RoutingSettings {
                mode: crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
                ..crate::mnemo2::dispatch_routing::RoutingSettings::default()
            },
            routing_answer(laya_label, 0.91),
            true,
            std::time::Duration::ZERO,
        )
        .await;
        let mut tiers = tiers_dto! {
            difficult: vec!["test/scripted-high".into()],
            average: vec!["test/scripted-routine".into()],
            simple: vec!["test/scripted-trivial".into()],
        };
        tiers.tiers[0].effort = Some("low".into());
        tiers.tiers[3].effort = Some("high".into());
        fixture.team.set_catalog(
            ["high", "routine", "trivial"]
                .into_iter()
                .map(|tier| LeadModel {
                    reasoning_efforts: vec!["low".into(), "high".into()],
                    ..lead_model(&format!("scripted-{tier}"))
                })
                .collect(),
            tiers,
            Vec::new(),
        );
        let model = format!("test/scripted-{ran}");
        let result = fixture.dispatch(Some(requested), Some(&model)).await;
        assert!(result.success, "{requested} -> {ran}: {}", result.output);
        // The explicit choice defers the classification: the dispatch
        // answers before the label lands.
        assert!(result.output.contains(
            "Laya: classifying in the background; the label will land on the routing record."
        ));
        let worker = fixture.team.worker("routingworker").await.expect("worker");
        assert_eq!(worker.tier.as_deref(), Some(ran));
        assert_eq!(worker.model.as_deref(), Some(model.as_str()));
        assert_eq!(worker.reasoning_effort.as_deref(), Some(expected_effort));
        let routing = wait_laya_label(&fixture.team, "routingworker").await;
        assert_eq!(routing.lead_tier.as_deref(), Some(requested));
        assert_eq!(routing.laya_tier.as_deref(), Some(requested));
        // `routed_by` resolves at dispatch time (plan c74b1987): the
        // override is corpus-only, the record keeps "lead".
        assert_eq!(routing.routed_by, "lead");
        let rows = eventually(|| async {
            let rows = fixture.rows();
            (rows.len() == 2).then_some(rows)
        })
        .await;
        assert!(matches!(
            &rows[0],
            crate::mnemo2::dispatch_routing::DispatchRoutingRow::Route {
                lead_tier: Some(lead),
                ran_tier: Some(actual),
                ..
            } if lead == requested && actual == ran
        ));
        assert!(matches!(
            &rows[1],
            crate::mnemo2::dispatch_routing::DispatchRoutingRow::LeadOverride {
                lead_tier,
                requested_tier,
                ..
            } if lead_tier == ran && requested_tier.as_deref() == Some(requested)
        ));
        fixture.team.shutdown().await;
    }
}

#[tokio::test]
async fn auto_below_gate_with_a_different_fallback_reports_unsure() {
    let fixture = routing_fixture_with(
        crate::mnemo2::dispatch_routing::RoutingSettings {
            mode: crate::mnemo2::dispatch_routing::RoutingMode::Auto,
            fallback_tier: "high".into(),
            ..crate::mnemo2::dispatch_routing::RoutingSettings::default()
        },
        routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.39),
        true,
        std::time::Duration::ZERO,
    )
    .await;
    let result = fixture.dispatch(None, None).await;
    assert!(result.success, "{}", result.output);
    assert!(result.output.contains("Laya: unsure (0.39)."));
    assert!(!result.output.contains("routed to difficult"));
    let worker = fixture.team.worker("routingworker").await.expect("worker");
    assert_eq!(worker.tier.as_deref(), Some("high"));
    fixture.team.shutdown().await;
}


#[tokio::test]
async fn ipc_auto_id_dispatch_row_carries_the_real_worker_id() {
    let fixture = routing_fixture(
        crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
        routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.62),
        true,
    )
    .await;
    let (worker, reused) = fixture
        .team
        .dispatch_on_plan(
            "Implement the auto-id IPC step and verify its routing log".into(),
            Some("test/scripted-average".into()),
            None,
            None,
            None,
            false,
            &fixture.plan_id,
            1,
        )
        .await
        .expect("auto-id IPC dispatch");
    assert!(!reused);
    // The explicit model choice defers the classification: the Route
    // row lands in the background, stamped with the real worker id.
    let rows = eventually(|| async {
        let rows = fixture.rows();
        (rows.len() == 1).then_some(rows)
    })
    .await;
    assert_eq!(fixture.classifier.calls(), 1);
    assert!(matches!(
        &rows[0],
        crate::mnemo2::dispatch_routing::DispatchRoutingRow::Route { worker_id, .. }
            if worker_id == &worker.id
    ));
    assert!(!worker.id.is_empty());


    fixture.team.shutdown().await;
}


#[tokio::test]
async fn ipc_shadow_dispatch_without_model_classifies_and_uses_the_default() {
    let fixture = routing_fixture(
        crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
        routing_answer(mnemo::agent::route_question::LABEL_HARD, 0.71),
        true,
    )
    .await;
    let (worker, reused) = fixture
        .team
        .dispatch_on_plan(
            "Implement the IPC default-model step and verify its routing metadata".into(),
            None,
            None,
            Some("ipcdefault".into()),
            None,
            false,
            &fixture.plan_id,
            1,
        )
        .await
        .expect("IPC default-model dispatch");
    assert!(!reused);
    assert_eq!(worker.model, None);
    assert_eq!(worker.tier, None);
    assert_eq!(fixture.classifier.calls(), 1);
    assert_eq!(fixture.rows().len(), 1);
    let routing = worker.routing.expect("routing");
    assert_eq!(routing.laya_tier.as_deref(), Some("high"));
    assert_eq!(routing.routed_by, "lead");

    let lead_tool = fixture.dispatch(None, None).await;
    assert!(!lead_tool.success);
    assert!(lead_tool.output.contains("choose a `tier` for this worker"));
    fixture.team.shutdown().await;
}

#[test]
fn route_fix_prompt_contains_every_open_finding_detail() {
    let root = tempfile::tempdir().expect("workspace");
    let mut record = worker(root.path(), "fixprompt001");
    let routed = open_finding("finding1");
    let mut other = open_finding("finding2");
    other.severity = "medium".into();
    other.title = "the second unchecked path".into();
    other.detail = "EVERY OTHER OPEN FINDING DETAIL".into();
    other.file = "src/other.rs".into();
    record.findings = vec![routed.clone(), other];
    let mut closed = open_finding("finding3");
    closed.status = "fixed".into();
    closed.detail = "CLOSED DETAIL MUST NOT BE SENT".into();
    record.findings.push(closed);
    let brief = route_fix_brief(&record, &routed);
    assert!(brief.contains(&routed.detail), "{brief}");
    assert!(brief.contains("EVERY OTHER OPEN FINDING DETAIL"), "{brief}");
    assert!(brief.contains("src/other.rs"), "{brief}");
    assert!(!brief.contains("CLOSED DETAIL MUST NOT BE SENT"), "{brief}");
}

#[tokio::test]
async fn failed_downshift_restores_prior_tier_and_logs_both_outcomes() {
    let fixture = review_fixture(
        crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
        "downshift001",
    )
    .await;
    let routed = open_finding("finding1");
    fixture
        .team
        .review(&fixture.record.id, "first".into(), vec![routed.clone()], evidence())
        .await
        .expect("first review");
    let down = fixture
        .team
        .route_fix_with_tier(&fixture.record.id, "finding1", Some("routine"))
        .await
        .expect("explicit downshift is allowed even for a high finding");
    assert_eq!(down.tier.as_deref(), Some("routine"));
    assert_eq!(down.model.as_deref(), Some("test/scripted-average"));
    fixture.team.stop_inner(&fixture.record.id).await.expect("stop fixer");
    let restored = fixture
        .team
        .review(&fixture.record.id, "still open".into(), vec![routed.clone()], evidence())
        .await
        .expect("failed downshift review");
    assert_eq!(restored.tier.as_deref(), Some("high"));
    assert_eq!(restored.model.as_deref(), Some("test/scripted-difficult"));
    let raw = std::fs::read_to_string(fixture.state.path().join("dispatch_routing.jsonl"))
        .expect("routing log");
    assert!(raw.lines().any(|line| {
        serde_json::from_str::<Value>(line).is_ok_and(|row| {
            row["source"] == "fix_downshift" && row["finding_closed"] == false
        })
    }), "{raw}");

    fixture
        .team
        .route_fix_with_tier(&fixture.record.id, "finding1", Some("routine"))
        .await
        .expect("second deliberate downshift");
    fixture.team.stop_inner(&fixture.record.id).await.expect("stop second fixer");
    let mut closed = routed;
    closed.status = "fixed".into();
    closed.evidence = Some("fixed and checked".into());
    let stayed = fixture
        .team
        .review(&fixture.record.id, "closed".into(), vec![closed], evidence())
        .await
        .expect("closed downshift review");
    assert_eq!(stayed.tier.as_deref(), Some("routine"));
    let raw = std::fs::read_to_string(fixture.state.path().join("dispatch_routing.jsonl"))
        .expect("routing log");
    assert!(raw.lines().any(|line| {
        serde_json::from_str::<Value>(line).is_ok_and(|row| {
            row["source"] == "fix_downshift" && row["finding_closed"] == true
        })
    }), "{raw}");
    fixture.team.shutdown().await;
}

#[tokio::test]
async fn auto_fix_classification_never_lowers_a_high_severity_finding() {
    let mut fixture = review_fixture(
        crate::mnemo2::dispatch_routing::RoutingMode::Auto,
        "protected001",
    )
    .await;
    let classifier = std::sync::Arc::new(StubDispatchClassifier::slow(
        routing_answer(mnemo::agent::route_question::LABEL_TRIVIAL, 0.95),
        std::time::Duration::ZERO,
    ));
    fixture.team.classifier = Some(std::sync::Arc::new(std::sync::RwLock::new(Some(
        classifier.clone(),
    ))));
    fixture
        .team
        .review(
            &fixture.record.id,
            "first".into(),
            vec![open_finding("finding1")],
            evidence(),
        )
        .await
        .expect("first review");
    let unchanged = fixture
        .team
        .route_fix_with_tier(&fixture.record.id, "finding1", Some("auto"))
        .await
        .expect("classified but protected fix");
    assert_eq!(unchanged.tier.as_deref(), Some("high"));
    assert_eq!(unchanged.model.as_deref(), Some("test/scripted-difficult"));
    assert_eq!(classifier.calls(), 1);
    assert!(fixture.rows().iter().any(|row| matches!(
        row,
        DispatchRoutingRow::Route { skip, .. }
            if skip.as_deref() == Some("fix_downshift_protected")
    )));
    fixture.team.shutdown().await;
}

#[tokio::test]
async fn high_audit_finding_after_fix_nudges_to_escalate() {
    let fixture = review_fixture(
        crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
        "auditnudge001",
    )
    .await;
    fixture
        .team
        .review(
            &fixture.record.id,
            "first".into(),
            vec![open_finding("finding1")],
            evidence(),
        )
        .await
        .expect("first review");
    fixture
        .team
        .route_fix(&fixture.record.id, "finding1")
        .await
        .expect("route fix");
    fixture.team.stop_inner(&fixture.record.id).await.expect("stop fixer");
    let tip = fixture.team.worker_tip(&fixture.record).await.expect("tip");
    let audit = audit_worker(WorkerStatus::Completed, &fixture.record.id, &tip);
    fixture.team.records.lock().await.push(audit);
    fixture.team.persist().await.expect("save audit");
    let mut audit_evidence = evidence();
    audit_evidence.independent_reviewer_id = Some("auditor0001".into());
    audit_evidence.audit_adjudication = Some("accepted the high finding".into());
    let reviewed = fixture
        .team
        .review(
            &fixture.record.id,
            "audit still open".into(),
            vec![open_finding("finding1")],
            audit_evidence,
        )
        .await
        .expect("audited review");
    assert_eq!(
        reviewed.routing.as_ref().and_then(|routing| routing.nudged.as_deref()),
        Some("escalate")
    );
    assert!(fixture.rows().iter().any(|row| matches!(
        row,
        DispatchRoutingRow::Nudge { to_tier, .. } if to_tier == "escalate"
    )));
    fixture.team.shutdown().await;
}

#[tokio::test]
async fn auto_mode_switches_an_idle_nudged_worker_and_notifies_the_lead() {
    let (fixture, mut notices) = nudge_fixture_with(
        crate::mnemo2::dispatch_routing::RoutingSettings {
            mode: crate::mnemo2::dispatch_routing::RoutingMode::Auto,
            ..crate::mnemo2::dispatch_routing::RoutingSettings::default()
        },
        routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.8),
        std::time::Duration::ZERO,
        vec![vec![
            mnemo::provider::LlmEvent::TextDelta { text: "done".into() },
            mnemo::provider::LlmEvent::Finish {
                reason: mnemo::provider::FinishReason::Stop,
            },
        ]],
    )
    .await;
    let dispatched = fixture.dispatch(Some("routine"), None).await;
    assert!(dispatched.success, "{}", dispatched.output);
    wait_worker_settled(&fixture.team, "routingworker").await;
    fixture
        .team
        .update_worker("routingworker", |worker| {
            if let Some(routing) = worker.routing.as_mut() {
                routing.nudged = Some("high".into());
            }
        })
        .await;
    fixture.team.persist().await.expect("save pending nudge");
    assert!(
        fixture
            .team
            .auto_escalate_worker("routingworker")
            .await
            .expect("auto escalation")
    );
    let switched = fixture.team.worker("routingworker").await.expect("switched");
    assert_eq!(switched.model.as_deref(), Some("test/scripted-difficult"));
    assert_eq!(switched.tier.as_deref(), Some("high"));
    assert!(switched.routing.expect("routing").nudged.is_none());
    let raw = std::fs::read_to_string(fixture.log_path()).expect("routing log");
    assert!(raw.lines().any(|line| {
        serde_json::from_str::<Value>(line).is_ok_and(|row| {
            row["event"] == "lead_reroute" && row["source"] == "escalation"
        })
    }), "{raw}");
    let mut notice = None;
    for _ in 0..800 {
        while let Ok(signal) = notices.try_recv() {
            if let BoardSignal::Notice { key, text } = signal {
                if key == "escalation:auto:routingworker" {
                    notice = Some(text);
                }
            }
        }
        if notice.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let notice = notice.expect("auto escalation notice");
    assert!(notice.contains("session and transcript were kept"), "{notice}");
    fixture.team.shutdown().await;
}

#[tokio::test]
async fn auto_strike_streak_escalates_when_the_worker_becomes_idle() {
    let responses = (1..=3)
        .map(|attempt| {
            tool_call_batch(
                &format!("auto-call-{attempt}"),
                "not_a_real_tool",
                &format!("{{\"attempt\":{attempt}}}"),
            )
        })
        .chain([vec![
            mnemo::provider::LlmEvent::TextDelta { text: "done".into() },
            mnemo::provider::LlmEvent::Finish {
                reason: mnemo::provider::FinishReason::Stop,
            },
        ]])
        .collect::<Vec<_>>();
    let (fixture, _notices) = nudge_fixture_with(
        crate::mnemo2::dispatch_routing::RoutingSettings {
            mode: crate::mnemo2::dispatch_routing::RoutingMode::Auto,
            ..crate::mnemo2::dispatch_routing::RoutingSettings::default()
        },
        routing_answer(mnemo::agent::route_question::LABEL_ROUTINE, 0.8),
        std::time::Duration::ZERO,
        responses,
    )
    .await;
    let dispatched = fixture.dispatch(Some("routine"), None).await;
    assert!(dispatched.success, "{}", dispatched.output);
    let switched = eventually(|| async {
        let worker = fixture.team.worker("routingworker").await.expect("worker");
        (worker.model.as_deref() == Some("test/scripted-difficult")).then_some(worker)
    })
    .await;
    assert_eq!(switched.tier.as_deref(), Some("high"));
    assert!(switched.routing.expect("routing").nudged.is_none());
    // The row is appended after the ledger save, so poll for it: the
    // in-memory switch can be visible before the file write lands.
    eventually(|| async {
        let raw = std::fs::read_to_string(fixture.log_path()).ok()?;
        raw.lines()
            .any(|line| {
                serde_json::from_str::<Value>(line).is_ok_and(|row| {
                    row["source"] == "escalation" && row["nudged"] == true
                })
            })
            .then_some(())
    })
    .await;
    fixture.team.shutdown().await;
}

#[tokio::test]
async fn worker_host_tools_offer_plan_read() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let team = open_test_team(root.path(), state.path(), Vec::new()).await;
    let tools = team.worker_host_tools();
    let plan_read = tools
        .iter()
        .find(|tool| tool.name == "plan_read")
        .expect("plan_read is offered");
    assert!(matches!(plan_read.safety, SafetyLevel::AutoRun));
    assert_eq!(plan_read.timeout_ms, Some(10_000));
    assert_eq!(
        plan_read.parameters["properties"]["capability"]["type"],
        "string"
    );
    assert!(plan_read.parameters.get("required").is_none());
    team.shutdown().await;
}

/// A tagged notice describes one turn of one worker: a notice whose worker
/// moved on, or no longer exists, is dropped at delivery. An untagged
/// notice (a report, a plan change) always stays.
#[tokio::test]
async fn current_notices_drop_a_moved_on_worker() {
    use crate::mnemo2::notice_tag::{NoticeState, TurnTag};
    use crate::mnemo2::runtime::TEAM_NOTICE_PREFIX;

    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let mut completed = worker(root.path(), "00000001000000000001");
    completed.status = WorkerStatus::Completed;
    completed.turns = 2;
    let mut working = worker(root.path(), "00000001000000000002");
    working.status = WorkerStatus::Working;
    let team = open_test_team(
        root.path(),
        state.path(),
        vec![completed.clone(), working.clone()],
    )
    .await;

    let tagged = |worker: &WorkerRecord, state| {
        format!(
            "{TEAM_NOTICE_PREFIX} worker_id={}{}\nthe detail",
            worker.id,
            TurnTag::of(worker, 1, state).suffix()
        )
    };
    let current_finished = tagged(&completed, NoticeState::Finished);
    let stale_finished = tagged(&working, NoticeState::Finished);
    let unknown = tagged(
        &worker(root.path(), "00000001000000000009"),
        NoticeState::Finished,
    );
    let untagged = format!("{TEAM_NOTICE_PREFIX}\nplan changed");
    let plain = "worker alpha finished its turn".to_string();

    let kept = team
        .current_notices(vec![
            current_finished.clone(),
            stale_finished,
            unknown,
            untagged.clone(),
            plain.clone(),
        ])
        .await;
    assert_eq!(kept, vec![current_finished, untagged, plain]);
    team.shutdown().await;
}

/// A stale notice drained from the queue must not end the wait: the lead
/// keeps waiting for a notice that still describes its worker.
#[tokio::test]
async fn waiting_skips_stale_notices() {
    use crate::mnemo2::notice_tag::{NoticeState, TurnTag};
    use crate::mnemo2::runtime::TEAM_NOTICE_PREFIX;

    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let mut working = worker(root.path(), "00000001000000000002");
    working.status = WorkerStatus::Working;
    let team = open_test_team(root.path(), state.path(), vec![working.clone()]).await;
    assert!(team.attach_external_lead(), "the external lead attaches");
    let stale = format!(
        "{TEAM_NOTICE_PREFIX} worker_id={}{}\nstale detail",
        working.id,
        TurnTag::of(&working, 1, NoticeState::Finished).suffix()
    );
    assert!(team.push_external_notice(stale), "the notice is queued");

    let outcome = team.wait_external(std::time::Duration::from_millis(150)).await;
    assert_eq!(outcome.status, ExternalWaitStatus::Timeout, "{outcome:?}");
    assert!(outcome.notices.is_empty(), "{:?}", outcome.notices);
    team.shutdown().await;
}


#[tokio::test]
async fn plan_read_returns_the_current_plan() {
    let root = tempfile::tempdir().expect("workspace");
    init_git(root.path());
    let plans_dir = root.path().join(".coding/plans");
    let board = PlanBoard::new(&plans_dir, root.path().join(".coding/reviews"));
    let plan = board
        .create(mnemo::app::plan_board::PlanDraft {
            title: "Plan reading".into(),
            goal: "Let a worker re-read its plan".into(),
            context: "A worker re-reads src-tauri/src/mnemo2/team.rs after dispatch and verifies with cargo test -p mnemo-app --features mnemo2 --lib mnemo2::team.".into(),
            steps: vec![
                "Implement plan_read in src-tauri/src/mnemo2/team.rs and verify with cargo test -p mnemo-app --features mnemo2 --lib mnemo2::team".into(),
                "Handle notices in src-tauri/src/mnemo2/team.rs and verify with cargo test -p mnemo-app --features mnemo2 --lib mnemo2::team".into(),
            ],
            ..Default::default()
        })
        .expect("create plan");
    board
        .update(
            &plan.id,
            mnemo::app::plan_board::PlanEdit {
                decision: Some("Use the shared capability reader".into()),
                ..Default::default()
            },
        )
        .expect("record the decision");
    let board = Arc::new(board);
    let messages = Arc::new(Mutex::new(Vec::new()));
    let stamp = Some((plan.id.clone(), 1usize));
    let result = worker_host_tool_result(
        &messages,
        "planreader",
        "plan_read",
        &json!({}),
        Some(&board),
        stamp.as_ref(),
    )
    .await;
    assert!(result.success, "{}", result.output);
    assert!(
        result.output.contains("Use the shared capability reader"),
        "{}",
        result.output
    );
    assert!(
        result.output.contains("Re-read this plan at any time"),
        "{}",
        result.output
    );
    assert!(result.output.contains("## Your step (2)"), "{}", result.output);

    let no_plan = worker_host_tool_result(
        &messages,
        "planreader",
        "plan_read",
        &json!({}),
        Some(&board),
        None,
    )
    .await;
    assert!(!no_plan.success);
    assert!(no_plan.output.contains("no plan"), "{}", no_plan.output);
}

#[tokio::test]
async fn plan_read_reads_a_capability_spec() {
    let root = tempfile::tempdir().expect("workspace");
    init_git(root.path());
    let spec_dir = root
        .path()
        .join(".coding/openspec/specs/tier-ladder");
    std::fs::create_dir_all(&spec_dir).expect("spec dir");
    std::fs::write(spec_dir.join("spec.md"), "# Tier ladder\n").expect("spec file");
    let board = Arc::new(PlanBoard::new(
        root.path().join(".coding/plans"),
        root.path().join(".coding/reviews"),
    ));
    let messages = Arc::new(Mutex::new(Vec::new()));
    let current = worker_host_tool_result(
        &messages,
        "planreader",
        "plan_read",
        &json!({"capability": "tier-ladder"}),
        Some(&board),
        None,
    )
    .await;
    assert!(current.success, "{}", current.output);
    assert_eq!(current.output, "# Tier ladder\n");

    let missing = worker_host_tool_result(
        &messages,
        "planreader",
        "plan_read",
        &json!({"capability": "not-a-capability"}),
        Some(&board),
        None,
    )
    .await;
    assert!(missing.success, "{}", missing.output);
    assert_eq!(
        missing.output,
        "No current spec for 'not-a-capability' yet."
    );

    let invalid = worker_host_tool_result(
        &messages,
        "planreader",
        "plan_read",
        &json!({"capability": "../x"}),
        Some(&board),
        None,
    )
    .await;
    assert!(!invalid.success);
    assert!(invalid.output.contains("invalid capability"), "{}", invalid.output);
}

#[tokio::test]
async fn plan_edit_notifies_only_working_workers() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let board = PlanBoard::new(
        root.path().join(".coding/plans"),
        root.path().join(".coding/reviews"),
    );
    let plan = board
        .create(mnemo::app::plan_board::PlanDraft {
            title: "Plan change notices".into(),
            goal: "Tell only the workers still working".into(),
            context: "One worker is editing src-tauri/src/mnemo2/team.rs while another waits for review; the working one verifies with cargo test -p mnemo-app --features mnemo2 --lib mnemo2::team.".into(),
            steps: vec![
                "Send notices in src-tauri/src/mnemo2/team.rs and verify with cargo test -p mnemo-app --features mnemo2 --lib mnemo2::team".into(),
                "Review the notice in src-tauri/src/mnemo2/team.rs and verify with cargo test -p mnemo-app --features mnemo2 --lib mnemo2::team".into(),
            ],
            ..Default::default()
        })
        .expect("create plan");
    let working_id = "00000001000000000001";
    let completed_id = "00000002000000000002";
    let mut working = worker(root.path(), working_id);
    working.status = WorkerStatus::Working;
    working.plan_id = Some(plan.id.clone());
    working.plan_step = Some(0);
    let mut completed = worker(root.path(), completed_id);
    completed.status = WorkerStatus::Completed;
    completed.plan_id = Some(plan.id.clone());
    completed.plan_step = Some(1);
    provision_agent_worktree(
        root.path().to_path_buf(),
        working.id.clone(),
        "main".to_string(),
    )
    .await
    .expect("provision the working worker");
    let team = open_test_team(root.path(), state.path(), vec![working.clone(), completed.clone()]).await;
    team.update_worker(working_id, |record| {
        record.status = WorkerStatus::Working;
    })
    .await;
    team.update_worker(completed_id, |record| {
        record.status = WorkerStatus::Completed;
    })
    .await;
    spawn_live_worker(&team, &working).await;
    let team = Arc::new(team);
    let board = Arc::new(board);
    team.attach_board(board.clone());
    board
        .update(
            &plan.id,
            mnemo::app::plan_board::PlanEdit {
                decision: Some("A decision recorded after dispatch".into()),
                ..Default::default()
            },
        )
        .expect("record the decision");
    let expected = format!(
        "Plan {} changed: new decision. Call plan_read to re-read it before you continue.",
        plan.id
    );
    let mut notice = None;
    for _ in 0..250 {
        let worker = team.worker(working_id).await.expect("working worker");
        if let Some(entry) = worker.inbox.first() {
            notice = Some(entry.clone());
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let notice = match notice {
        Some(notice) => notice,
        None => {
            let worker = team.worker(working_id).await.expect("working worker");
            panic!(
                "the working worker got no notice; status {:?}, inbox {:?}",
                worker.status, worker.inbox
            )
        }
    };
    assert_eq!(notice.text, expected);
    assert_eq!(notice.sender, MessageSender::Mnemo);
    assert_eq!(
        team.worker(completed_id)
            .await
            .expect("completed worker")
            .inbox,
        Vec::new()
    );

    board
        .update(
            &plan.id,
            mnemo::app::plan_board::PlanEdit {
                progress_note: Some("Only a progress note".into()),
                ..Default::default()
            },
        )
        .expect("record the progress note");
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(
        team.worker(working_id).await.expect("working worker").inbox.len(),
        1
    );
    team.shutdown().await;
}

/// A refresh reads the worker tip outside the ledger lock, so the lead can
/// commit and review inside that window: the review then postdates the
/// probe and covers the current tip. Clearing it made the merge fail with
/// "the current worker tip has not been reviewed" (plan 0d8a1112, macOS CI
/// run 42071163294).
#[test]
fn a_refresh_that_read_the_tip_before_the_review_keeps_that_review() {
    let root = tempfile::tempdir().expect("workspace");
    init_git(root.path());
    let mut worker = reviewed_worker(root.path(), "00000001000000000001");
    {
        let review = worker.review.as_mut().expect("review");
        review.tip = "reviewed-tip".to_string();
        review.reviewed_at_ms = 200;
    }
    worker.checks[0].tip = "reviewed-tip".to_string();

    // The probe ran at 100 ms, the lead's commit and review landed at 200.
    clear_review_for_a_newer_tip(&mut worker, Some("older-tip"), 100);
    assert!(
        worker.review.is_some(),
        "a review recorded after the tip probe is not stale"
    );
    assert_eq!(worker.checks.len(), 1, "its check survives with it");

    // A probe that ran after the review, but on the reviewed commit.
    clear_review_for_a_newer_tip(&mut worker, Some("reviewed-tip"), 300);
    assert!(worker.review.is_some(), "the reviewed tip is never stale");

    // A probe that ran after the review and saw a newer commit is a real
    // verdict: the review no longer covers the worker's tip.
    clear_review_for_a_newer_tip(&mut worker, Some("newer-tip"), 300);
    assert!(worker.review.is_none(), "an older review is cleared");
    assert!(worker.checks.is_empty(), "and so are its checks");
}
