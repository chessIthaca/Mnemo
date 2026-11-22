use super::*;

use std::collections::VecDeque;

use mnemo::app::worktrees::{
    agent_branch, agent_short_id, discard_and_release_pool_checkout, provision_agent_worktree,
    release_pool_checkout,
};
use mnemo::headless::{GateVerdict, Transcript};
use mnemo::SerializableAgentEvent;
use mnemo::tool::{SafetyLevel, ToolResult};
use serde_json::{json, Value};

use crate::mnemo2::dispatch_routing::{outcome_label, DispatchRoutingRow, RoutingMode};
use crate::mnemo2::phase3::{Finding, MergeRecord, ReviewEvidence, ReviewLane, ReviewRecord};

use super::control::recover_worker;
use super::deletion::{
    branch_exists, checkout_is_registered, move_checkout_to_trash, sweep_trash, trash_entries,
    validated_checkout, worktree_list_paths,
};
use super::dispatch::{
    claude_agent_block, dispatch_result, dispatch_routing_line,
    require_acceptance_scenarios, DispatchRequest, DispatchSource, PlanRef,
};
use super::guardrails::{audit_guardrails, executor_guardrails};
use super::lead_tools::worker_status_view;
use super::merge::{
    delete_merged_branch, merge_outcome_line, merge_tip, validate_merge, WORKTREE_CLEANUP_ERROR,
};
use super::nudge::{message_can_show_drift, note_tool_outcome, route_fix_brief, TOOL_ERROR_CHARS, NUDGE_STRIKE_LIMIT};
use super::persistence::{
    PersistedTeam, TEAM_PRE_TIERS_BACKUP, TEAM_STATE_FILE, TRANSCRIPTS_DIR,
};
use super::pump::{
    counted_check, counted_checks, cost_notice_due, usage_cost_micros, worker_host_tool_result,
    pump_worker,
};
use super::review::CheckRecorder;
use super::record::{append_inbox, audit_finished, MessageSender, WORKER_INBOX_LIMIT};
use super::review::clear_review_for_a_newer_tip;
use super::routing::final_tier;

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
    recover_worker(std::path::Path::new("unused"), &mut persisted.workers[0], &[])
        .await
        .expect("legacy recovery");
    assert!(persisted.workers[0].error.is_none());
    assert_eq!(persisted.workers[0].total_tokens, 120);
    assert_eq!(persisted.workers[0].prompt_tokens, 120);
}

#[test]
fn worker_record_clones_share_the_transcript() {
    let dir = tempfile::tempdir().expect("dir");
    let mut record = worker(dir.path(), "arcshare01");
    record.transcript = Arc::new(Transcript {
        messages: vec![mnemo::provider::Message::text(
            mnemo::provider::Role::User,
            "hello",
        )],
    });
    let copy = record.clone();
    assert!(Arc::ptr_eq(&record.transcript, &copy.transcript));
}

#[test]
fn worker_record_transcript_json_shape_is_unchanged() {
    let dir = tempfile::tempdir().expect("dir");
    let mut record = worker(dir.path(), "arcshare02");
    record.transcript = Arc::new(Transcript {
        messages: vec![mnemo::provider::Message::text(
            mnemo::provider::Role::User,
            "hello",
        )],
    });
    let value = serde_json::to_value(&record).unwrap();
    assert_eq!(value["transcript"]["messages"].as_array().unwrap().len(), 1);
    let back: WorkerRecord = serde_json::from_value(value).unwrap();
    assert_eq!(back.transcript.messages.len(), 1);
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

#[tokio::test]
async fn team_snapshots_leave_out_transcripts_the_caller_holds() {
    let root = tempfile::tempdir().expect("root");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let mut record = worker(root.path(), "transcriptrevs");
    record.transcript = Arc::new(Transcript {
        messages: vec![mnemo::provider::Message::text(
            mnemo::provider::Role::User,
            "hello",
        )],
    });
    let team = open_test_team(root.path(), state.path(), vec![record]).await;
    let id = "transcriptrevs".to_string();

    // A caller that holds nothing receives the whole thread and its revision.
    let first = team.snapshot_known(&HashMap::new()).await;
    assert_eq!(first.workers.len(), 1);
    assert_eq!(first.workers[0].transcript.messages.len(), 1);
    let rev = first.transcript_revs[&id];
    assert!(first.omitted_transcripts.is_empty());

    // A caller that already holds that revision receives no transcript.
    let known = HashMap::from([(id.clone(), rev)]);
    let second = team.snapshot_known(&known).await;
    assert!(second.workers[0].transcript.messages.is_empty());
    assert_eq!(second.omitted_transcripts, vec![id.clone()]);
    assert_eq!(second.transcript_revs[&id], rev);

    // A replaced transcript is a new revision, so it travels in full again.
    {
        let mut records = team.records.lock().await;
        records[0].transcript = Arc::new(Transcript {
            messages: vec![
                mnemo::provider::Message::text(mnemo::provider::Role::User, "hello"),
                mnemo::provider::Message::text(mnemo::provider::Role::Assistant, "hi"),
            ],
        });
    }
    let third = team.snapshot_known(&known).await;
    assert_eq!(third.workers[0].transcript.messages.len(), 2);
    assert_ne!(third.transcript_revs[&id], rev);
    assert!(third.omitted_transcripts.is_empty());

    // The plain snapshot every other caller uses always carries the thread.
    let full = team.snapshot().await;
    assert_eq!(full.workers[0].transcript.messages.len(), 2);
    assert!(full.omitted_transcripts.is_empty());
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

/// Runs a call through the executor's declarative gate and reports the
/// refusal message — `None` when the call is allowed — so tests can tell
/// the shared-stash refusal from the integration one.
async fn executor_refusal(tool: &str, args: Value) -> Option<String> {
    use mnemo::agent::policy::{AgentPolicy, DeclarativePolicy};
    let policy =
        DeclarativePolicy::new(Arc::new(executor_guardrails())).expect("rules compile");
    match policy.gate_call(tool, &args).await {
        GateVerdict::Deny { message } => Some(message),
        // Not a refusal: the call is either allowed outright or proceeds
        // to the regular approval flow.
        GateVerdict::Allow | GateVerdict::RequireApproval => None,
    }
}

/// Runs a call through the executor's declarative gate and reports
/// whether it was denied as a shared-stash violation.
async fn executor_denies_stash(tool: &str, args: Value) -> bool {
    executor_refusal(tool, args)
        .await
        .is_some_and(|message| message.contains("shared by every worker checkout"))
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
        // `stash push` is no longer in the allowed set: the shared-stash
        // refusal covers it (executor_gate_refuses_the_shared_stash_stack).
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

/// A job runs the same commands as `shell`, so the gate that keeps workers out
/// of integration must cover `job` too (plans 5b85ab61 step 2, 5b498359) —
/// otherwise a worker could push or merge by wrapping the command in a job.
#[tokio::test]
async fn executor_gate_denies_integration_through_job() {
    for command in [
        "git push",
        "git -C ../repo push",
        "cargo test && git push",
        "git merge wt/other",
        "git checkout main",
        "gh pr create",
    ] {
        assert!(
            executor_denies(
                "job",
                json!({"command": command, "purpose": "building"})
            )
            .await,
            "{command}"
        );
    }
    for command in [
        "cargo test --workspace",
        "cargo clippy --workspace --all-targets",
        "git status",
    ] {
        assert!(
            !executor_denies(
                "job",
                json!({"command": command, "purpose": "building"})
            )
            .await,
            "{command}"
        );
    }
}

/// Every worker checkout is a worktree of the same repository, so they all
/// share one git stash stack (`refs/stash`): a worker's pop or drop takes
/// whatever is on top — another worker's WIP (2026-11-20: symVec01's pop
/// applied hostToken01's stash in the wrong checkout) or a months-old stash
/// from the lead checkout. The executor gate must refuse every push, pop and
/// drop on that shared stack — through the structured git tool, `shell` and
/// `job` alike — while the read-only forms and `git stash apply <sha>`
/// (recovering a known dangling commit) still run.
#[tokio::test]
async fn executor_gate_refuses_the_shared_stash_stack() {
    // The structured git tool has no ref parameter, so every stash pop and
    // drop through it lands on the shared stack: every carrier of a push,
    // pop or drop is refused.
    for args in [
        json!({"subcommand": "stash", "action": "push"}),
        json!({"subcommand": "stash", "action": "pop"}),
        json!({"subcommand": "stash", "action": "drop"}),
        // No action — or the subcommand repeated in `action` — defaults
        // to push.
        json!({"subcommand": "stash"}),
        json!({"subcommand": "stash", "action": "stash"}),
        // Stash action names ride either field.
        json!({"subcommand": "pop"}),
        json!({"subcommand": "drop"}),
        json!({"action": "pop"}),
        json!({"action": "drop"}),
        json!({"action": "stash"}),
    ] {
        assert!(executor_denies_stash("git", args.clone()).await, "{args}");
    }
    // `stash list` is read-only: both carrier spellings stay allowed.
    for args in [
        json!({"subcommand": "stash", "action": "list"}),
        json!({"subcommand": "list", "action": "stash"}),
    ] {
        assert!(!executor_denies_stash("git", args.clone()).await, "{args}");
    }
    // A bare `git stash` is a push ("you can omit push"), and so is a bare
    // `git stash <push-option>` (`-m`, `-- pathspec`); the named mutators
    // and `apply` without a SHA — no argument, or a `stash@{…}` ref — all
    // touch the shared stack.
    for command in [
        "git stash",
        "git stash push",
        "git stash save",
        "git stash pop",
        "git stash drop",
        "git stash clear",
        "git stash store",
        "git stash branch",
        "git stash -m \"wip\"",
        "git stash -- src/",
        "git -C x stash pop",
        "cargo test && git stash",
        "cargo test && git stash pop",
        "git stash apply",
        "git stash apply stash@{0}",
        "git stash apply stash@{1}",
        "git stash apply -q",
    ] {
        for tool in ["shell", "job"] {
            assert!(
                executor_denies_stash(tool, json!({"command": command, "purpose": "p"})).await,
                "{tool}: {command}"
            );
        }
    }
    // The read-only forms and the SHA recovery run.
    for command in [
        "git stash list",
        "git stash list --format=%gd",
        "git stash show -p",
        "git stash show --stat stash@{0}",
        "git stash create",
        "git stash apply 78b29bbd",
        "git stash apply 78b29bbd1c2d3e4f5a6b7c8d9e0f1a2b3c4d5e6f",
    ] {
        for tool in ["shell", "job"] {
            assert!(
                !executor_denies_stash(tool, json!({"command": command, "purpose": "p"}))
                    .await,
                "{tool}: {command}"
            );
        }
    }
    // The refusal names the WIP-commit alternative and the SHA recovery.
    let refusal = executor_refusal("git", json!({"subcommand": "stash", "action": "pop"}))
        .await
        .expect("stash pop is refused");
    assert!(
        refusal.contains("Park work as a WIP commit"),
        "the stash refusal must name the WIP-commit alternative: {refusal}"
    );
    assert!(
        refusal.contains("git stash apply"),
        "the stash refusal must name the stash-apply recovery: {refusal}"
    );
}

/// The audit worker is read-only by construction: the job tools ride
/// `ToolGroup::Shell`, so they must be excluded from its surface by name
/// (plans 5b85ab61 step 2, 5b498359).
#[test]
fn audit_surface_excludes_the_job_tools() {
    let audit = audit_guardrails().tools.exclude;
    let executor = executor_guardrails().tools.exclude;
    for tool in ["job", "job_wait", "job_tail", "job_kill"] {
        assert!(
            audit.iter().any(|excluded| excluded == tool),
            "the audit surface still allows {tool}"
        );
        assert!(
            !executor.iter().any(|excluded| excluded == tool),
            "the executor surface lost {tool}"
        );
    }
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

#[test]
fn ledger_saves_are_compact_json() {
    // Regression (PERF-1): the ledger is rewritten on every state change, so
    // it must not pay for pretty-printed indentation on top of every record.
    let dir = tempfile::tempdir().expect("ledger dir");
    let root = tempfile::tempdir().expect("workspace");
    let persistence = TeamPersistence::new(dir.path());
    persistence
        .save(&PersistedTeam {
            workers: vec![worker(root.path(), "00000004000000000004")],
            slots: Some(4),
            usage: ProjectUsage::default(),
        })
        .expect("compact save");
    let bytes = std::fs::read(dir.path().join(TEAM_STATE_FILE)).expect("read team.json");
    assert!(
        !bytes.contains(&b'\n'),
        "team.json must be one compact JSON document"
    );
    let loaded = persistence.load().expect("reload").expect("some ledger");
    assert_eq!(loaded.workers.len(), 1);
    assert_eq!(loaded.workers[0].id, "00000004000000000004");
}

/// Backdate a file so the temp-file grace treats it as a crash leftover.
fn age_file(path: &Path, by: std::time::Duration) {
    let old = std::time::SystemTime::now() - by;
    std::fs::File::options()
        .write(true)
        .open(path)
        .expect("open the aged file")
        .set_modified(old)
        .expect("age the file");
}

#[test]
fn a_crash_leftover_temp_is_swept_on_load() {
    // A crash mid-save leaves a uniquely-named temp (`team.json.<pid>.<seq>.tmp`)
    // that no later save overwrites; load sweeps it once it is old enough that
    // no save running right now can own it.
    let dir = tempfile::tempdir().expect("ledger dir");
    let root = tempfile::tempdir().expect("workspace");
    let persistence = TeamPersistence::new(dir.path());
    persistence
        .save(&PersistedTeam {
            workers: vec![worker(root.path(), "00000005000000000005")],
            slots: Some(4),
            usage: ProjectUsage::default(),
        })
        .expect("compact save");
    let stale = dir.path().join("team.json.123.0.tmp");
    std::fs::write(&stale, b"{\"workers\":[]}").expect("stale temp");
    age_file(&stale, std::time::Duration::from_secs(120));
    let loaded = persistence.load().expect("load").expect("some ledger");
    assert!(!stale.exists(), "the crash-leftover temp is swept on load");
    assert_eq!(loaded.workers.len(), 1);
    assert_eq!(loaded.workers[0].id, "00000005000000000005");
}

#[test]
fn a_young_ledger_temp_survives_a_load() {
    // Regression: a load through a fresh TeamPersistence must not delete the
    // temp file of a save running right now, or that save's rename fails and
    // its coalesced flush is lost.
    let dir = tempfile::tempdir().expect("ledger dir");
    let root = tempfile::tempdir().expect("workspace");
    let persistence = TeamPersistence::new(dir.path());
    persistence
        .save(&PersistedTeam {
            workers: vec![worker(root.path(), "00000006000000000006")],
            slots: Some(4),
            usage: ProjectUsage::default(),
        })
        .expect("compact save");
    let live = dir.path().join("team.json.456.1.tmp");
    std::fs::write(&live, b"{\"workers\":[]}").expect("live temp");
    let loaded = persistence.load().expect("load").expect("some ledger");
    assert!(live.exists(), "a young ledger temp is not swept");
    assert_eq!(loaded.workers.len(), 1);
}

#[test]
fn a_reader_never_sees_the_ledger_missing() {
    // Regression: a save replaces the ledger atomically. Moving the live ledger
    // aside first left `team.json` absent for a window a reader could observe,
    // and a concurrent `decode` would report "no ledger".
    let dir = tempfile::tempdir().expect("ledger dir");
    let root = tempfile::tempdir().expect("workspace");
    let persistence = Arc::new(TeamPersistence::new(dir.path()));
    let team = PersistedTeam {
        workers: vec![worker(root.path(), "00000007000000000007")],
        slots: Some(4),
        usage: ProjectUsage::default(),
    };
    persistence.save(&team).expect("first save");

    let writer = {
        let persistence = Arc::clone(&persistence);
        let team = team.clone();
        std::thread::spawn(move || {
            for _ in 0..200 {
                persistence.save(&team).expect("save the ledger");
            }
        })
    };
    let path = dir.path().join(TEAM_STATE_FILE);
    let mut reads = 0_u32;
    while !writer.is_finished() {
        match std::fs::read(&path) {
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                panic!("team.json was missing while a save replaced it");
            }
            // A share violation while the file is being replaced is fine.
            Err(_) => {}
        }
        reads += 1;
    }
    writer.join().expect("the writer finishes");
    assert!(reads > 0, "the reader ran while the writer was saving");
}

#[tokio::test]
async fn usage_reports_reach_the_ledger_within_the_flush_delay() {
    // Regression (PERF-2): every usage report used to rewrite the whole
    // ledger. Reports now coalesce into one flush, which must still reach
    // team.json within the flush delay.
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let team = open_test_team(root.path(), state.path(), Vec::new()).await;
    for _ in 0..3 {
        team.record_usage(Some("perf-model"), 10, 0, 5)
            .await
            .expect("record usage");
    }
    let persistence = TeamPersistence::new(state.path());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let prompt_tokens = loop {
        // A concurrent save renames team.json while it writes, so a miss is
        // just "not yet", not an error.
        let prompt_tokens = persistence
            .load()
            .expect("load the ledger")
            .and_then(|loaded| {
                loaded
                    .usage
                    .models
                    .get("perf-model")
                    .map(|row| row.prompt_tokens)
            });
        if prompt_tokens == Some(30) {
            break prompt_tokens;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the coalesced usage save never reached the ledger: {prompt_tokens:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    assert_eq!(prompt_tokens, Some(30));
    assert!(
        !team.persistence.flush_scheduled.load(Ordering::SeqCst),
        "the pending flag clears before the ledger write"
    );
    team.shutdown().await;
}

#[tokio::test]
async fn telemetry_saves_coalesce_into_one_pending_flush() {
    // Regression (PERF-2): usage and context-size updates arrive with every
    // LLM request. Twenty reports in a row must leave exactly one flush
    // pending, and that flush must still write the current state.
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let team = open_test_team(root.path(), state.path(), Vec::new()).await;
    {
        let mut usage = team.usage.lock().await;
        usage.record("coalesced-model", 7, 0, 0);
    }
    for _ in 0..20 {
        super::pump::save_workers_soon(
            &team.records,
            &team.usage,
            &team.save_lock,
            &team.persistence,
        );
    }
    assert!(
        team.persistence.flush_scheduled.load(Ordering::SeqCst),
        "the first call schedules the flush, the rest ride along"
    );
    let persistence = TeamPersistence::new(state.path());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let saved = persistence
            .load()
            .expect("load the ledger")
            .and_then(|loaded| {
                loaded
                    .usage
                    .models
                    .get("coalesced-model")
                    .map(|row| row.prompt_tokens)
            });
        if saved == Some(7) && !team.persistence.flush_scheduled.load(Ordering::SeqCst) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the coalesced flush never wrote the ledger"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    team.shutdown().await;
}

/// One user-message transcript, as the transcript-file tests store it.
fn transcript_of(texts: &[&str]) -> Arc<Transcript> {
    Arc::new(Transcript {
        messages: texts
            .iter()
            .map(|text| mnemo::provider::Message::text(mnemo::provider::Role::User, *text))
            .collect(),
    })
}

/// `team.json` decoded as raw JSON, to assert what is (not) embedded there.
fn ledger_json(dir: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(dir.join(TEAM_STATE_FILE)).expect("read team.json"))
        .expect("decode team.json")
}

#[test]
fn transcripts_are_saved_beside_the_ledger() {
    let dir = tempfile::tempdir().expect("ledger dir");
    let root = tempfile::tempdir().expect("workspace");
    let persistence = TeamPersistence::new(dir.path());
    let mut record = worker(root.path(), "trans0001");
    record.transcript = transcript_of(&["hello"]);
    persistence
        .save(&PersistedTeam {
            workers: vec![record],
            slots: Some(4),
            usage: ProjectUsage::default(),
        })
        .expect("save the ledger");

    let file = dir.path().join(TRANSCRIPTS_DIR).join("trans0001.json");
    let stored: Transcript = serde_json::from_slice(&std::fs::read(&file).expect("transcript file"))
        .expect("decode the transcript file");
    assert_eq!(stored.messages.len(), 1);
    assert_eq!(
        ledger_json(dir.path())["workers"][0]["transcript"]["messages"],
        json!([])
    );

    let loaded = TeamPersistence::new(dir.path())
        .load()
        .expect("load the ledger")
        .expect("some ledger");
    assert_eq!(loaded.workers[0].transcript.messages.len(), 1);
}

#[test]
fn an_unchanged_transcript_is_not_rewritten() {
    // Regression (PERF-7): the ledger is saved on every state change, so a
    // transcript that did not change must not be rewritten. The sentinel proves
    // it: an unchanged save leaves the file's bytes alone.
    let dir = tempfile::tempdir().expect("ledger dir");
    let root = tempfile::tempdir().expect("workspace");
    let persistence = TeamPersistence::new(dir.path());
    let mut record = worker(root.path(), "steady001");
    record.transcript = transcript_of(&["hello"]);
    let team = PersistedTeam {
        workers: vec![record],
        slots: Some(4),
        usage: ProjectUsage::default(),
    };
    persistence.save(&team).expect("first save");
    let file = dir.path().join(TRANSCRIPTS_DIR).join("steady001.json");
    assert!(file.exists());

    std::fs::write(&file, b"sentinel").expect("overwrite with sentinel bytes");
    persistence.save(&team).expect("unchanged save");
    assert_eq!(
        std::fs::read(&file).expect("read the transcript file"),
        b"sentinel",
        "an unchanged transcript is not rewritten"
    );

    let mut changed = team.clone();
    changed.workers[0].transcript = transcript_of(&["hello"]);
    persistence.save(&changed).expect("changed save");
    let stored: Transcript = serde_json::from_slice(&std::fs::read(&file).expect("transcript file"))
        .expect("valid transcript file");
    assert_eq!(stored.messages.len(), 1, "a new transcript Arc is written");
}

#[test]
fn a_deleted_transcript_file_is_written_again() {
    // Regression: `written` is not the sole authority. team.json keeps only a
    // placeholder, so a transcript file deleted out of band must come back on
    // the next save even though the worker's transcript `Arc` never changed.
    let dir = tempfile::tempdir().expect("ledger dir");
    let root = tempfile::tempdir().expect("workspace");
    let persistence = TeamPersistence::new(dir.path());
    let mut record = worker(root.path(), "vanish001");
    record.transcript = transcript_of(&["hello"]);
    let team = PersistedTeam {
        workers: vec![record],
        slots: Some(4),
        usage: ProjectUsage::default(),
    };
    persistence.save(&team).expect("first save");
    let file = dir.path().join(TRANSCRIPTS_DIR).join("vanish001.json");
    assert!(file.exists());

    std::fs::remove_file(&file).expect("delete the transcript file");
    persistence.save(&team).expect("restoring save");
    let stored: Transcript = serde_json::from_slice(&std::fs::read(&file).expect("restored file"))
        .expect("valid restored transcript");
    assert_eq!(stored.messages.len(), 1, "the deleted file is written again");
    let reloaded = TeamPersistence::new(dir.path())
        .load()
        .expect("load")
        .expect("some ledger");
    assert_eq!(
        reloaded.workers[0].transcript.messages.len(),
        1,
        "a restart still sees the conversation"
    );
}

#[test]
fn a_legacy_embedded_transcript_migrates_to_its_file() {
    let dir = tempfile::tempdir().expect("ledger dir");
    let root = tempfile::tempdir().expect("workspace");
    let mut record = worker(root.path(), "legacy001");
    record.transcript = transcript_of(&["first", "second"]);
    let legacy = PersistedTeam {
        workers: vec![record],
        slots: Some(4),
        usage: ProjectUsage::default(),
    };
    std::fs::write(
        dir.path().join(TEAM_STATE_FILE),
        serde_json::to_vec(&legacy).expect("legacy JSON"),
    )
    .expect("write the legacy ledger");
    let file = dir.path().join(TRANSCRIPTS_DIR).join("legacy001.json");
    assert!(!file.exists());

    let persistence = TeamPersistence::new(dir.path());
    let loaded = persistence.load().expect("load").expect("some ledger");
    assert_eq!(loaded.workers[0].transcript.messages.len(), 2);

    persistence.save(&loaded).expect("migrating save");
    assert!(file.exists(), "the migrated transcript has its own file");
    assert_eq!(
        ledger_json(dir.path())["workers"][0]["transcript"]["messages"],
        json!([])
    );
    let reloaded = TeamPersistence::new(dir.path())
        .load()
        .expect("reload")
        .expect("some ledger");
    assert_eq!(reloaded.workers[0].transcript.messages.len(), 2);
}

#[test]
fn a_transcript_file_wins_over_the_embedded_one() {
    let dir = tempfile::tempdir().expect("ledger dir");
    let root = tempfile::tempdir().expect("workspace");
    let mut record = worker(root.path(), "embedded1");
    record.transcript = transcript_of(&["embedded only"]);
    std::fs::write(
        dir.path().join(TEAM_STATE_FILE),
        serde_json::to_vec(&PersistedTeam {
            workers: vec![record],
            slots: Some(4),
            usage: ProjectUsage::default(),
        })
        .expect("ledger JSON"),
    )
    .expect("write the legacy ledger");
    let transcripts = dir.path().join(TRANSCRIPTS_DIR);
    std::fs::create_dir_all(&transcripts).expect("transcripts dir");
    std::fs::write(
        transcripts.join("embedded1.json"),
        serde_json::to_vec(&Transcript {
            messages: vec![
                mnemo::provider::Message::text(mnemo::provider::Role::User, "newer"),
                mnemo::provider::Message::text(mnemo::provider::Role::User, "newest"),
            ],
        })
        .expect("transcript JSON"),
    )
    .expect("write the transcript file");

    let loaded = TeamPersistence::new(dir.path())
        .load()
        .expect("load")
        .expect("some ledger");
    assert_eq!(loaded.workers[0].transcript.messages.len(), 2);
}

#[test]
fn a_removed_worker_loses_its_transcript_file() {
    let dir = tempfile::tempdir().expect("ledger dir");
    let root = tempfile::tempdir().expect("workspace");
    let persistence = TeamPersistence::new(dir.path());
    let mut keep = worker(root.path(), "keepme001");
    keep.transcript = transcript_of(&["keep"]);
    let mut drop = worker(root.path(), "dropme001");
    drop.transcript = transcript_of(&["drop"]);
    persistence
        .save(&PersistedTeam {
            workers: vec![keep.clone(), drop],
            slots: Some(4),
            usage: ProjectUsage::default(),
        })
        .expect("first save");
    let transcripts = dir.path().join(TRANSCRIPTS_DIR);
    assert!(transcripts.join("keepme001.json").exists());
    assert!(transcripts.join("dropme001.json").exists());

    persistence
        .save(&PersistedTeam {
            workers: vec![keep],
            slots: Some(4),
            usage: ProjectUsage::default(),
        })
        .expect("second save");
    assert!(
        !transcripts.join("dropme001.json").exists(),
        "a removed worker's transcript file is deleted"
    );
    assert!(transcripts.join("keepme001.json").exists());
}

#[test]
fn a_failed_transcript_removal_is_retried() {
    // A denied removal keeps the id recorded, because an unknown file is
    // deliberately preserved and would otherwise never be removed.
    let dir = tempfile::tempdir().expect("ledger dir");
    let root = tempfile::tempdir().expect("workspace");
    let persistence = TeamPersistence::new(dir.path());
    let mut record = worker(root.path(), "stubborn01");
    record.transcript = transcript_of(&["drop"]);
    persistence
        .save(&PersistedTeam {
            workers: vec![record],
            slots: Some(4),
            usage: ProjectUsage::default(),
        })
        .expect("first save");
    let file = dir.path().join(TRANSCRIPTS_DIR).join("stubborn01.json");

    std::fs::remove_file(&file).expect("remove the transcript file");
    std::fs::create_dir(&file).expect("block the transcript path");
    std::fs::write(file.join("child"), "blocker").expect("non-empty directory");
    persistence
        .save(&PersistedTeam {
            workers: Vec::new(),
            slots: Some(4),
            usage: ProjectUsage::default(),
        })
        .expect("a failed removal does not fail the save");

    std::fs::remove_dir_all(&file).expect("unblock the transcript path");
    std::fs::write(&file, "stale").expect("a stale transcript file");
    persistence
        .save(&PersistedTeam {
            workers: Vec::new(),
            slots: Some(4),
            usage: ProjectUsage::default(),
        })
        .expect("the retried removal succeeds");
    assert!(!file.exists(), "the kept entry retries and removes the file");
}

#[test]
fn a_transcript_this_instance_never_knew_survives_a_save() {
    // Two instances can share a state dir ("Open anyway"), so an unknown
    // `<id>.json` belongs to the other instance's worker, not to an orphan this
    // instance may delete.
    let dir = tempfile::tempdir().expect("ledger dir");
    let root = tempfile::tempdir().expect("workspace");
    let persistence = TeamPersistence::new(dir.path());
    let mut mine = worker(root.path(), "keepmine01");
    mine.transcript = transcript_of(&["mine"]);
    let team = PersistedTeam {
        workers: vec![mine],
        slots: Some(4),
        usage: ProjectUsage::default(),
    };
    persistence.save(&team).expect("first save");
    let transcripts = dir.path().join(TRANSCRIPTS_DIR);
    let theirs = transcripts.join("otherinst1.json");
    let bytes = serde_json::to_vec(&Transcript {
        messages: vec![mnemo::provider::Message::text(
            mnemo::provider::Role::User,
            "theirs",
        )],
    })
    .expect("their transcript JSON");
    std::fs::write(&theirs, &bytes).expect("another instance's transcript");

    persistence.save(&team).expect("second save");
    assert_eq!(
        std::fs::read(&theirs).expect("their transcript survives"),
        bytes,
        "a transcript this instance never wrote is left alone"
    );
    assert!(transcripts.join("keepmine01.json").exists());
}

#[test]
fn a_corrupt_transcript_file_keeps_the_embedded_one() {
    // Decode failure only: the bytes are damaged, so the file is kept aside as
    // `.corrupt` and the embedded transcript stands until the next save.
    let dir = tempfile::tempdir().expect("ledger dir");
    let root = tempfile::tempdir().expect("workspace");
    let mut record = worker(root.path(), "broken0001");
    record.transcript = transcript_of(&["survives"]);
    std::fs::write(
        dir.path().join(TEAM_STATE_FILE),
        serde_json::to_vec(&PersistedTeam {
            workers: vec![record],
            slots: Some(4),
            usage: ProjectUsage::default(),
        })
        .expect("ledger JSON"),
    )
    .expect("write the ledger");
    let transcripts = dir.path().join(TRANSCRIPTS_DIR);
    std::fs::create_dir_all(&transcripts).expect("transcripts dir");
    let file = transcripts.join("broken0001.json");
    std::fs::write(&file, "not json").expect("corrupt transcript file");

    let persistence = TeamPersistence::new(dir.path());
    let loaded = persistence.load().expect("load").expect("some ledger");
    assert_eq!(
        loaded.workers[0].transcript.messages.len(),
        1,
        "a corrupt file keeps the embedded transcript"
    );
    assert_eq!(
        std::fs::read(transcripts.join("broken0001.json.corrupt")).expect("the kept bytes"),
        b"not json".to_vec(),
        "the damaged bytes are kept aside, not destroyed"
    );

    persistence.save(&loaded).expect("repairing save");
    let stored: Transcript = serde_json::from_slice(&std::fs::read(&file).expect("repaired file"))
        .expect("valid transcript file");
    assert_eq!(stored.messages.len(), 1);
}

#[test]
fn a_corrupt_transcript_is_kept_aside() {
    // A saved conversation whose file is damaged must not vanish silently: the
    // file is renamed aside, the record is flagged, and the next save leaves no
    // empty file behind to hide it.
    let dir = tempfile::tempdir().expect("ledger dir");
    let root = tempfile::tempdir().expect("workspace");
    let persistence = TeamPersistence::new(dir.path());
    let mut record = worker(root.path(), "damaged001");
    record.transcript = transcript_of(&["hello"]);
    persistence
        .save(&PersistedTeam {
            workers: vec![record],
            slots: Some(4),
            usage: ProjectUsage::default(),
        })
        .expect("first save");
    let transcripts = dir.path().join(TRANSCRIPTS_DIR);
    let file = transcripts.join("damaged001.json");
    assert!(file.exists());
    std::fs::write(&file, "not json").expect("corrupt transcript file");

    let reopened = TeamPersistence::new(dir.path());
    let loaded = reopened.load().expect("load").expect("some ledger");
    assert_eq!(
        loaded.workers[0].transcript.messages.len(),
        0,
        "the placeholder stands"
    );
    let error = loaded.workers[0]
        .error
        .clone()
        .expect("the damage is flagged");
    assert!(
        error.contains("damaged001.json.corrupt"),
        "the flag names the kept file: {error}"
    );
    let kept = transcripts.join("damaged001.json.corrupt");
    assert_eq!(
        std::fs::read(&kept).expect("the kept bytes"),
        b"not json".to_vec()
    );
    assert!(!file.exists(), "the damaged file is moved aside");

    reopened.save(&loaded).expect("repairing save");
    assert!(
        !file.exists(),
        "an empty transcript does not recreate the file"
    );
}

#[test]
fn a_lost_conversation_keeps_the_earlier_error_too() {
    // An error the worker already carried must not hide the loss.
    let dir = tempfile::tempdir().expect("ledger dir");
    let root = tempfile::tempdir().expect("workspace");
    let persistence = TeamPersistence::new(dir.path());
    let mut record = worker(root.path(), "bothloss01");
    record.transcript = transcript_of(&["hello"]);
    record.error = Some("the last merge failed".to_string());
    persistence
        .save(&PersistedTeam {
            workers: vec![record],
            slots: Some(4),
            usage: ProjectUsage::default(),
        })
        .expect("first save");
    let file = dir.path().join(TRANSCRIPTS_DIR).join("bothloss01.json");
    std::fs::remove_file(&file).expect("delete the conversation file");

    let loaded = TeamPersistence::new(dir.path())
        .load()
        .expect("load")
        .expect("some ledger");
    let error = loaded.workers[0]
        .error
        .clone()
        .expect("the loss is flagged beside the earlier error");
    assert!(
        error.contains("saved conversation is missing"),
        "the loss is named: {error}"
    );
    assert!(
        error.contains("the last merge failed"),
        "the earlier error is kept: {error}"
    );
}

#[test]
fn a_transcript_that_became_empty_rewrites_its_file() {
    // The other half of the skip rule: a worker whose file exists is rewritten,
    // so a transcript that really became empty cannot leave stale content
    // winning.
    let dir = tempfile::tempdir().expect("ledger dir");
    let root = tempfile::tempdir().expect("workspace");
    let persistence = TeamPersistence::new(dir.path());
    let mut record = worker(root.path(), "emptied001");
    record.transcript = transcript_of(&["hello"]);
    let mut team = PersistedTeam {
        workers: vec![record],
        slots: Some(4),
        usage: ProjectUsage::default(),
    };
    persistence.save(&team).expect("first save");
    let file = dir.path().join(TRANSCRIPTS_DIR).join("emptied001.json");
    assert!(file.exists());

    team.workers[0].transcript = Arc::default();
    persistence.save(&team).expect("emptying save");
    let stored: Transcript =
        serde_json::from_slice(&std::fs::read(&file).expect("the emptied file"))
            .expect("valid transcript");
    assert_eq!(stored.messages.len(), 0, "the stale content is replaced");

    let loaded = TeamPersistence::new(dir.path())
        .load()
        .expect("load")
        .expect("some ledger");
    assert_eq!(loaded.workers[0].transcript.messages.len(), 0);
    assert!(
        loaded.workers[0].error.is_none(),
        "an empty transcript is not a loss: {:?}",
        loaded.workers[0].error
    );
}

#[test]
fn a_missing_conversation_is_flagged_not_recreated() {
    // A deleted conversation file must stay visible: the record is flagged and
    // no empty file is written in its place, which would make the loss look
    // like a worker that never spoke.
    let dir = tempfile::tempdir().expect("ledger dir");
    let root = tempfile::tempdir().expect("workspace");
    let persistence = TeamPersistence::new(dir.path());
    let mut record = worker(root.path(), "lostconv01");
    record.transcript = transcript_of(&["hello"]);
    record.requests = 3;
    persistence
        .save(&PersistedTeam {
            workers: vec![record],
            slots: Some(4),
            usage: ProjectUsage::default(),
        })
        .expect("first save");
    let file = dir.path().join(TRANSCRIPTS_DIR).join("lostconv01.json");
    assert!(file.exists());
    std::fs::remove_file(&file).expect("delete the conversation file");

    let reopened = TeamPersistence::new(dir.path());
    let loaded = reopened.load().expect("load").expect("some ledger");
    assert!(loaded.workers[0].transcript.messages.is_empty());
    let error = loaded.workers[0]
        .error
        .clone()
        .expect("the lost conversation is flagged");
    assert_eq!(
        loaded.workers[0].saved_messages, 0,
        "the in-memory record never carries the saved count"
    );
    assert!(
        error.contains("lostconv01"),
        "the flag names the missing file: {error}"
    );

    reopened.save(&loaded).expect("save the loaded ledger");
    assert!(
        !file.exists(),
        "an empty transcript does not recreate a missing file"
    );

    let again = TeamPersistence::new(dir.path())
        .load()
        .expect("load")
        .expect("some ledger");
    assert!(
        again.workers[0].error.is_some(),
        "a fresh load still flags the loss"
    );
}

#[test]
fn a_new_worker_is_not_flagged_and_gets_no_file() {
    // A worker that never ran has nothing to lose: no flag, and an empty
    // transcript needs no file.
    let dir = tempfile::tempdir().expect("ledger dir");
    let root = tempfile::tempdir().expect("workspace");
    let persistence = TeamPersistence::new(dir.path());
    let mut record = worker(root.path(), "fresh00001");
    record.requests = 0;
    record.turns = 0;
    record.brief_sent = false;
    record.transcript = Arc::default();
    persistence
        .save(&PersistedTeam {
            workers: vec![record],
            slots: Some(4),
            usage: ProjectUsage::default(),
        })
        .expect("save the ledger");
    assert!(!dir
        .path()
        .join(TRANSCRIPTS_DIR)
        .join("fresh00001.json")
        .exists());

    let loaded = TeamPersistence::new(dir.path())
        .load()
        .expect("load")
        .expect("some ledger");
    assert!(
        loaded.workers[0].error.is_none(),
        "a worker that never ran is not flagged: {:?}",
        loaded.workers[0].error
    );
}

#[test]
fn a_worker_interrupted_in_its_first_turn_is_not_flagged() {
    // A worker stopped or interrupted during its first turn has requests but
    // never saved a conversation, which is not a loss: only a file the ledger
    // recorded as written can go missing.
    let dir = tempfile::tempdir().expect("ledger dir");
    let root = tempfile::tempdir().expect("workspace");
    let persistence = TeamPersistence::new(dir.path());
    let mut record = worker(root.path(), "interrupted1");
    record.requests = 3;
    record.turns = 0;
    record.brief_sent = true;
    record.transcript = Arc::default();
    persistence
        .save(&PersistedTeam {
            workers: vec![record],
            slots: Some(4),
            usage: ProjectUsage::default(),
        })
        .expect("save the ledger");
    let file = dir
        .path()
        .join(TRANSCRIPTS_DIR)
        .join("interrupted1.json");
    assert!(!file.exists(), "an empty transcript gets no file");

    let loaded = TeamPersistence::new(dir.path())
        .load()
        .expect("load")
        .expect("some ledger");
    assert!(
        loaded.workers[0].error.is_none(),
        "nothing was saved, so nothing is missing: {:?}",
        loaded.workers[0].error
    );
    assert_eq!(loaded.workers[0].saved_messages, 0);
    assert!(!file.exists());
}

#[test]
fn a_damaged_transcript_that_cannot_be_moved_refuses_to_load() {
    // If the damaged file cannot be kept aside, the load must refuse: the open
    // save would otherwise replace the only copy of the bytes.
    let dir = tempfile::tempdir().expect("ledger dir");
    let root = tempfile::tempdir().expect("workspace");
    let ledger = PersistedTeam {
        workers: vec![worker(root.path(), "stuck00001")],
        slots: Some(4),
        usage: ProjectUsage::default(),
    };
    std::fs::write(
        dir.path().join(TEAM_STATE_FILE),
        serde_json::to_vec(&ledger).expect("ledger JSON"),
    )
    .expect("write the ledger");
    let transcripts = dir.path().join(TRANSCRIPTS_DIR);
    std::fs::create_dir_all(&transcripts).expect("transcripts dir");
    let file = transcripts.join("stuck00001.json");
    std::fs::write(&file, "not json").expect("corrupt transcript file");
    let blocked = transcripts.join("stuck00001.json.corrupt");
    std::fs::create_dir(&blocked).expect("block the corrupt path");
    std::fs::write(blocked.join("child"), "blocker").expect("non-empty directory");

    let error = TeamPersistence::new(dir.path())
        .load()
        .expect_err("the load refuses to overwrite the damaged bytes");
    assert!(
        error.contains("stuck00001"),
        "the failure names the worker: {error}"
    );
    assert_eq!(
        std::fs::read(&file).expect("the original bytes"),
        b"not json".to_vec(),
        "the damaged bytes are still there"
    );
}

#[test]
fn a_transcript_read_failure_refuses_to_load() {
    // A read that does not succeed must fail the load: the ledger holds only a
    // placeholder, so the save that follows an open would erase the
    // conversation with it.
    let dir = tempfile::tempdir().expect("ledger dir");
    let root = tempfile::tempdir().expect("workspace");
    let persistence = TeamPersistence::new(dir.path());
    let mut record = worker(root.path(), "denied0001");
    record.transcript = transcript_of(&["first"]);
    persistence
        .save(&PersistedTeam {
            workers: vec![record],
            slots: Some(4),
            usage: ProjectUsage::default(),
        })
        .expect("first save");
    let file = dir.path().join(TRANSCRIPTS_DIR).join("denied0001.json");
    std::fs::remove_file(&file).expect("remove the transcript file");
    std::fs::create_dir(&file).expect("make the transcript path unreadable");

    let error = TeamPersistence::new(dir.path())
        .load()
        .expect_err("the load refuses to continue");
    assert!(
        error.contains("denied0001"),
        "the failure names the worker: {error}"
    );
    assert!(file.is_dir(), "the unreadable path is left untouched");
}

#[test]
fn a_non_alphanumeric_worker_id_stays_embedded() {
    let dir = tempfile::tempdir().expect("ledger dir");
    let root = tempfile::tempdir().expect("workspace");
    let persistence = TeamPersistence::new(dir.path());
    let mut odd = worker(root.path(), "worker-1");
    odd.transcript = transcript_of(&["stays embedded"]);
    let mut plain = worker(root.path(), "alnum0001");
    plain.transcript = transcript_of(&["moves out"]);
    persistence
        .save(&PersistedTeam {
            workers: vec![odd, plain],
            slots: Some(4),
            usage: ProjectUsage::default(),
        })
        .expect("save the ledger");

    let transcripts = dir.path().join(TRANSCRIPTS_DIR);
    assert!(
        !transcripts.join("worker-1.json").exists(),
        "a worker id that cannot name a file stays in team.json"
    );
    assert!(transcripts.join("alnum0001.json").exists());
    assert_eq!(
        ledger_json(dir.path())["workers"][0]["transcript"]["messages"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
}

#[test]
fn a_failed_transcript_write_leaves_the_ledger_untouched() {
    // A directory where the transcript file belongs makes the rename fail on
    // both platforms; the save must report it before replacing team.json.
    let dir = tempfile::tempdir().expect("ledger dir");
    let root = tempfile::tempdir().expect("workspace");
    let persistence = TeamPersistence::new(dir.path());
    let mut record = worker(root.path(), "blocked01");
    record.transcript = transcript_of(&["first"]);
    let mut team = PersistedTeam {
        workers: vec![record],
        slots: Some(4),
        usage: ProjectUsage::default(),
    };
    persistence.save(&team).expect("first save");
    let ledger_path = dir.path().join(TEAM_STATE_FILE);
    let before = std::fs::read(&ledger_path).expect("read team.json");

    let file = dir.path().join(TRANSCRIPTS_DIR).join("blocked01.json");
    std::fs::remove_file(&file).expect("remove the transcript file");
    std::fs::create_dir(&file).expect("block the transcript file path");
    team.workers[0].transcript = transcript_of(&["second"]);
    team.slots = Some(8);
    let error = persistence.save(&team).expect_err("the transcript write fails");
    assert!(
        error.contains("transcript"),
        "the failure names the transcript write: {error}"
    );
    assert_eq!(
        std::fs::read(&ledger_path).expect("read team.json"),
        before,
        "team.json is not replaced when a transcript write fails"
    );
    // Unblock the path first: an unreadable transcript correctly refuses a load.
    std::fs::remove_dir(&file).expect("unblock the transcript path");
    assert_eq!(
        TeamPersistence::new(dir.path())
            .load()
            .expect("load")
            .expect("some ledger")
            .slots,
        Some(4),
        "the previous ledger still stands"
    );
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
        &[],
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
        claude_agent: None,
        claude_agent_effort: None,
        claude_agent_id: None,
        claude_agent_transcript: None,
        base_commit: None,
        report: None,
        branch,
        worktree: root
            .join(".worktrees")
            .join(format!("agent-{}", agent_short_id(id))),
        status: WorkerStatus::Completed,
        generation: 0,
        transcript: Arc::default(),
        saved_messages: 0,
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
        review_turns: None,
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

async fn worker_check_fixture(
    id: &str,
) -> (
    tempfile::TempDir,
    tempfile::TempDir,
    TeamRuntime,
    WorkerRecord,
) {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    run_git(root.path(), &["checkout", "-b", "wt/mnemo"]);
    let worker = worker(root.path(), id);
    provision_agent_worktree(
        root.path().to_path_buf(),
        id.to_string(),
        "wt/mnemo".to_string(),
    )
    .await
    .expect("provision the worker worktree");
    let team = open_test_team(root.path(), state.path(), vec![worker.clone()]).await;
    (root, state, team, worker)
}

fn shell_result(exit_code: i64, tree_before: Option<&str>, tree_after: Option<&str>) -> ToolResult {
    let mut data = json!({ "exit_code": exit_code });
    if let Some(tree) = tree_before {
        data["tree_before"] = json!(tree);
    }
    if let Some(tree) = tree_after {
        data["tree_after"] = json!(tree);
    }
    ToolResult::success("command ran").with_data(data)
}

/// The repository's own allow-list, as `load_check_commands` trims it.
fn allow_list() -> Vec<String> {
    vec![
        "cargo test --workspace".to_string(),
        "cargo clippy --workspace --all-targets".to_string(),
        "npm test --workspace mnemo2".to_string(),
        "npm run check --workspace mnemo2".to_string(),
    ]
}

async fn finish_worker_checks(
    team: &TeamRuntime,
    worker: &WorkerRecord,
    mut observations: HashMap<String, String>,
) {
    CheckRecorder::new(
        &team.records,
        &team.usage,
        &team.save_lock,
        &team.persistence,
    )
    .record_worker_checks(
        &team.merge_lock,
        &worker.id,
        worker.generation,
        &mut observations,
    )
    .await;
}

#[test]
fn check_allow_list_comes_only_from_valid_lead_root_config() {
    let lead = tempfile::tempdir().expect("lead root");
    let worker = tempfile::tempdir().expect("worker checkout");
    std::fs::create_dir_all(worker.path().join(".coding")).expect("worker coding dir");
    std::fs::write(
        worker.path().join(".coding/checks.toml"),
        "commands = [\"cargo test --workspace\"]\n",
    )
    .expect("worker allow-list");
    assert!(
        super::spawn::load_check_commands(lead.path()).is_empty(),
        "a worker-local file must not enable automatic checks"
    );

    std::fs::create_dir_all(lead.path().join(".coding")).expect("lead coding dir");
    std::fs::write(
        lead.path().join(".coding/checks.toml"),
        "commands = [\"  cargo test --workspace  \"]\n",
    )
    .expect("lead allow-list");
    assert_eq!(
        super::spawn::load_check_commands(lead.path()).as_slice(),
        ["cargo test --workspace".to_string()]
    );

    let invalid_commands = [
        "",
        "   ",
        "cargo test --workspace; echo no",
        "cargo test --workspace | Select-String x",
        "cargo test --workspace && echo no",
        "cargo test --workspace > log.txt",
        "cargo test --workspace < input.txt",
        "cargo test --workspace `echo no`",
        "cargo test --workspace $(echo no)",
        "cargo\ntest --workspace",
        "cargo\rtest --workspace",
        "cargo\ttest --workspace",
    ];
    let encoded = invalid_commands
        .iter()
        .map(|command| format!("{command:?}"))
        .chain([r#""cargo test --workspace""#.to_string()])
        .collect::<Vec<_>>()
        .join(", ");
    std::fs::write(
        lead.path().join(".coding/checks.toml"),
        format!("commands = [{encoded}]\n"),
    )
    .expect("compound lead allow-list");
    let commands = super::spawn::load_check_commands(lead.path());
    assert_eq!(
        commands.as_slice(),
        ["cargo test --workspace".to_string()],
        "every empty, compound, or control-character entry is dropped"
    );
    let checkout = tempfile::tempdir().expect("checkout");
    let tree = "0123456789abcdef0123456789abcdef01234567";
    for command in invalid_commands {
        assert_eq!(
            counted_check(
                "shell",
                &json!({ "command": command }),
                &shell_result(0, Some(tree), Some(tree)),
                checkout.path(),
                &commands,
            ),
            None,
            "invalid allow-list entry {command:?} must never count"
        );
    }

    std::fs::write(lead.path().join(".coding/checks.toml"), "commands = 42\n")
        .expect("malformed lead allow-list");
    assert!(
        super::spawn::load_check_commands(lead.path()).is_empty(),
        "a malformed file disables automatic checks"
    );
}

#[test]
fn counted_check_accepts_only_allow_listed_unchanging_runs() {
    let checkout = tempfile::tempdir().expect("checkout");
    let checkout = checkout.path();
    let foreign = tempfile::tempdir().expect("foreign cwd");
    let allowed = allow_list();
    let tree = "0123456789abcdef0123456789abcdef01234567";
    let listed = json!({ "command": "cargo test --workspace" });
    // An allow-listed command, exit 0, no cwd, equal trees: counted.
    assert_eq!(
        counted_check(
            "shell",
            &listed,
            &shell_result(0, Some(tree), Some(tree)),
            checkout,
            &allowed,
        ),
        Some(("cargo test --workspace".to_string(), tree.to_string()))
    );
    // Only leading and trailing whitespace is trimmed; internal bytes matter.
    assert_eq!(
        counted_check(
            "shell",
            &json!({ "command": "  cargo test --workspace  " }),
            &shell_result(0, Some(tree), Some(tree)),
            checkout,
            &allowed,
        ),
        Some(("cargo test --workspace".to_string(), tree.to_string()))
    );
    for command in ["cargo\ntest --workspace", "cargo  test --workspace"] {
        assert_eq!(
            counted_check(
                "shell",
                &json!({ "command": command }),
                &shell_result(0, Some(tree), Some(tree)),
                checkout,
                &allowed,
            ),
            None,
            "{command:?} must not count"
        );
    }
    // A cwd that canonicalizes to the checkout root still counts.
    assert_eq!(
        counted_check(
            "shell",
            &json!({ "command": "cargo test --workspace", "cwd": checkout.display().to_string() }),
            &shell_result(0, Some(tree), Some(tree)),
            checkout,
            &allowed,
        ),
        Some(("cargo test --workspace".to_string(), tree.to_string()))
    );
    // Not on the allow-list.
    assert_eq!(
        counted_check(
            "shell",
            &json!({ "command": "cargo test --workspace --no-run" }),
            &shell_result(0, Some(tree), Some(tree)),
            checkout,
            &allowed,
        ),
        None
    );
    // A foreign cwd is never the worker's checkout.
    assert_eq!(
        counted_check(
            "shell",
            &json!({ "command": "cargo test --workspace", "cwd": foreign.path().display().to_string() }),
            &shell_result(0, Some(tree), Some(tree)),
            checkout,
            &allowed,
        ),
        None
    );
    // Missing tree fields — the run was not measured.
    assert_eq!(
        counted_check(
            "shell",
            &listed,
            &shell_result(0, None, None),
            checkout,
            &allowed,
        ),
        None
    );
    // The tree changed while the command ran.
    assert_eq!(
        counted_check(
            "shell",
            &listed,
            &shell_result(0, Some(tree), Some("fedcba9876543210")),
            checkout,
            &allowed,
        ),
        None
    );
    // A non-zero exit code.
    assert_eq!(
        counted_check(
            "shell",
            &listed,
            &shell_result(1, Some(tree), Some(tree)),
            checkout,
            &allowed,
        ),
        None
    );
    // A failed tool result.
    assert_eq!(
        counted_check(
            "shell",
            &listed,
            &ToolResult::error("tool failed")
                .with_data(json!({ "exit_code": 0, "tree_before": tree, "tree_after": tree })),
            checkout,
            &allowed,
        ),
        None
    );
    // Another tool never counts.
    assert_eq!(
        counted_check(
            "file_read",
            &listed,
            &shell_result(0, Some(tree), Some(tree)),
            checkout,
            &allowed,
        ),
        None
    );
}

#[tokio::test]
async fn a_worker_run_check_on_the_committed_tree_is_recorded() {
    let (_root, _state, team, worker) = worker_check_fixture("workercheck1").await;
    std::fs::write(worker.worktree.join("checked.txt"), "checked\n").expect("worker edit");
    let index = phase3::git_value(&worker.worktree, &["rev-parse", "--git-path", "index"])
        .await
        .expect("locate the real index");
    let index = std::path::PathBuf::from(index);
    let index = if index.is_absolute() {
        index
    } else {
        worker.worktree.join(index)
    };
    let index_before = std::fs::read(&index).expect("read the real index");
    let tree = phase3::content_tree(&worker.worktree)
        .await
        .expect("hash the worker content");
    assert_eq!(
        std::fs::read(&index).expect("reread the real index"),
        index_before,
        "content_tree must not change the worker's real index"
    );
    let allowed = allow_list();
    let (name, observed_tree) = counted_check(
        "shell",
        &json!({ "command": "cargo test --workspace" }),
        &shell_result(0, Some(&tree), Some(&tree)),
        &worker.worktree,
        &allowed,
    )
    .expect("the measured passing command counts");
    run_git(&worker.worktree, &["add", "checked.txt"]);
    run_git(&worker.worktree, &["commit", "-m", "worker change"]);
    let tip = phase3::tip(&worker.worktree).await.expect("worker tip");

    finish_worker_checks(&team, &worker, HashMap::from([(name, observed_tree)])).await;

    let recorded = team.worker(&worker.id).await.expect("worker record");
    assert_eq!(recorded.checks.len(), 1);
    let check = &recorded.checks[0];
    assert_eq!(check.name, "cargo test --workspace");
    assert_eq!(check.status, "passed");
    assert_eq!(check.tip, tip);
    let detail = check.detail.as_deref().expect("worker detail");
    assert!(detail.contains("run by the worker"), "{detail}");
    assert!(detail.contains(&tip[..7]), "{detail}");
    let persisted = team
        .persistence
        .load()
        .expect("load the persisted team")
        .expect("persisted team");
    let persisted_worker = persisted
        .workers
        .iter()
        .find(|record| record.id == worker.id)
        .expect("persisted worker");
    assert_eq!(persisted_worker.checks.len(), 1);
    assert_eq!(persisted_worker.checks[0].tip, tip);
    team.shutdown().await;
}

#[tokio::test]
async fn a_check_run_before_a_later_edit_is_not_recorded() {
    let (_root, _state, team, worker) = worker_check_fixture("workercheck2").await;
    std::fs::write(worker.worktree.join("checked.txt"), "tested\n").expect("tested edit");
    let tested_tree = phase3::content_tree(&worker.worktree)
        .await
        .expect("hash the tested tree");
    let allowed = allow_list();
    let (name, observed_tree) = counted_check(
        "shell",
        &json!({ "command": "cargo test --workspace" }),
        &shell_result(0, Some(&tested_tree), Some(&tested_tree)),
        &worker.worktree,
        &allowed,
    )
    .expect("the measured passing command counts");
    std::fs::write(worker.worktree.join("checked.txt"), "edited later\n")
        .expect("edit after the test");
    run_git(&worker.worktree, &["add", "checked.txt"]);
    run_git(&worker.worktree, &["commit", "-m", "commit the later edit"]);

    finish_worker_checks(&team, &worker, HashMap::from([(name, observed_tree)])).await;

    let recorded = team.worker(&worker.id).await.expect("worker record");
    assert!(
        recorded.checks.is_empty(),
        "a different committed tree must not inherit the earlier run: {:?}",
        recorded.checks
    );
    team.shutdown().await;
}

#[tokio::test]
async fn a_failed_foreign_or_unmeasured_run_is_not_recorded() {
    let (_root, _state, team, worker) = worker_check_fixture("workercheck3").await;
    let foreign = tempfile::tempdir().expect("foreign cwd");
    std::fs::write(worker.worktree.join("checked.txt"), "committed\n").expect("worker edit");
    run_git(&worker.worktree, &["add", "checked.txt"]);
    run_git(&worker.worktree, &["commit", "-m", "worker change"]);
    let tree = phase3::content_tree(&worker.worktree)
        .await
        .expect("hash the committed tree");
    let allowed = allow_list();
    let cases = [
        (
            json!({ "command": "cargo test --workspace" }),
            shell_result(1, Some(&tree), Some(&tree)),
        ),
        (
            json!({
                "command": "cargo test --workspace",
                "cwd": foreign.path().display().to_string(),
            }),
            shell_result(0, Some(&tree), Some(&tree)),
        ),
        (
            json!({ "command": "cargo test --workspace" }),
            shell_result(0, None, None),
        ),
        (
            json!({ "command": "cargo test --workspace --no-run" }),
            shell_result(0, Some(&tree), Some(&tree)),
        ),
        (
            json!({ "command": "cargo test --workspace" }),
            shell_result(0, Some(&tree), Some("fedcba9876543210")),
        ),
    ];
    let mut observations = HashMap::new();
    for (args, result) in &cases {
        if let Some((name, observed_tree)) =
            counted_check("shell", args, result, &worker.worktree, &allowed)
        {
            observations.insert(name, observed_tree);
        }
    }

    finish_worker_checks(&team, &worker, observations).await;

    let recorded = team.worker(&worker.id).await.expect("worker record");
    assert!(recorded.checks.is_empty(), "{:?}", recorded.checks);
    team.shutdown().await;
}

#[tokio::test]
async fn a_lead_check_with_the_same_name_still_replaces_it() {
    let (_root, _state, team, worker) = worker_check_fixture("workercheck4").await;
    std::fs::write(worker.worktree.join("checked.txt"), "committed\n").expect("worker edit");
    run_git(&worker.worktree, &["add", "checked.txt"]);
    run_git(&worker.worktree, &["commit", "-m", "worker change"]);
    let tree = phase3::content_tree(&worker.worktree)
        .await
        .expect("hash the committed tree");
    let allowed = allow_list();
    let (name, observed_tree) = counted_check(
        "shell",
        &json!({ "command": "cargo test --workspace" }),
        &shell_result(0, Some(&tree), Some(&tree)),
        &worker.worktree,
        &allowed,
    )
    .expect("the measured passing command counts");
    finish_worker_checks(&team, &worker, HashMap::from([(name.clone(), observed_tree)])).await;

    let recorded = team
        .record_check(
            &worker.id,
            name,
            "passed".to_string(),
            Some("recorded by the lead".to_string()),
        )
        .await
        .expect("the lead check replaces the worker observation");

    assert_eq!(recorded.checks.len(), 1);
    assert_eq!(recorded.checks[0].name, "cargo test --workspace");
    assert_eq!(
        recorded.checks[0].detail.as_deref(),
        Some("recorded by the lead")
    );
    team.shutdown().await;
}

/// A real `shell` tool call with equal `tree_before`/`tree_after` fields rides
/// the whole pump path: the worker session executes the command for real, the
/// pump keeps the observation, and the finished turn records the check bound
/// to the committed tip.
#[tokio::test]
async fn a_real_shell_check_run_records_a_check_at_turn_end() {
    let (root, _state, _config, team, mut events) = report_streak_team(vec![tool_call_batch(
        "check-1",
        "shell",
        r#"{"command":"git status","purpose":"verify the checkout"}"#,
    )])
    .await;
    std::fs::create_dir_all(root.path().join(".coding")).expect("checks dir");
    std::fs::write(
        root.path().join(".coding/checks.toml"),
        "commands = [\"git status\"]\n",
    )
    .expect("checks.toml");
    let worker = team
        .dispatch("run the allowed check".to_string(), None)
        .await
        .expect("the worker dispatches");
    // The pump records the checks BEFORE it forwards `Finished` — waiting for
    // that event, not for the `Completed` status, proves the ordering.
    loop {
        let forwarded = forwarded_events(&mut events).await;
        if forwarded
            .iter()
            .any(|event| matches!(event, SerializableAgentEvent::Finished { .. }))
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let finished = team.worker(&worker.id).await.expect("worker record");

    assert_eq!(finished.checks.len(), 1, "{:?}", finished.checks);
    let check = &finished.checks[0];
    assert_eq!(check.name, "git status");
    assert_eq!(check.status, "passed");
    let tip = phase3::tip(&finished.worktree).await.expect("worker tip");
    assert_eq!(check.tip, tip);
    let detail = check.detail.as_deref().expect("worker detail");
    assert!(detail.contains("run by the worker"), "{detail}");
    team.shutdown().await;
}

/// One `data.checks` entry of the `run_checks` fixture: command, exit code,
/// tree before, tree after (`None` is a JSON `null` / a missing tree).
type CheckEntry<'a> = (&'a str, Option<i64>, Option<&'a str>, Option<&'a str>);

/// A `run_checks` result: one `data.checks` entry per command, each claiming
/// `checkout` as its cwd, exactly as `RunChecksTool` writes them.
fn run_checks_result(checkout: &Path, entries: &[CheckEntry<'_>]) -> ToolResult {
    let checks: Vec<Value> = entries
        .iter()
        .map(|(command, exit_code, tree_before, tree_after)| {
            json!({
                "command": command,
                "exit_code": exit_code,
                "tree_before": tree_before.unwrap_or(""),
                "tree_after": tree_after.unwrap_or(""),
                "cwd": checkout.display().to_string(),
            })
        })
        .collect();
    ToolResult::success("checks ran").with_data(json!({ "checks": checks }))
}

/// One `run_checks` result yields one observation per PASSING check, so the
/// turn records every passing check of the run, and no failing one.
#[test]
fn run_checks_result_counts_each_passing_check() {
    let checkout = tempfile::tempdir().expect("checkout");
    let checkout = checkout.path();
    let tree = "0123456789abcdef0123456789abcdef01234567";
    let result = run_checks_result(
        checkout,
        &[
            ("cargo test --workspace", Some(0), Some(tree), Some(tree)),
            (
                "cargo clippy --workspace --all-targets",
                Some(0),
                Some(tree),
                Some(tree),
            ),
            ("npm test --workspace mnemo2", Some(1), Some(tree), Some(tree)),
        ],
    );

    assert_eq!(
        counted_checks(
            "run_checks",
            &json!({ "purpose": "run the checks" }),
            &result,
            checkout,
            &allow_list(),
        ),
        vec![
            ("cargo test --workspace".to_string(), tree.to_string()),
            (
                "cargo clippy --workspace --all-targets".to_string(),
                tree.to_string()
            ),
        ]
    );
}

/// A run_checks entry that changed the tree, never exited (`exit_code: null`,
/// a killed check), or could not be measured records nothing.
#[test]
fn run_checks_tree_change_is_not_counted() {
    let checkout = tempfile::tempdir().expect("checkout");
    let checkout = checkout.path();
    let tree = "0123456789abcdef0123456789abcdef01234567";
    let result = run_checks_result(
        checkout,
        &[
            (
                "cargo test --workspace",
                Some(0),
                Some(tree),
                Some("fedcba9876543210"),
            ),
            (
                "cargo clippy --workspace --all-targets",
                None,
                Some(tree),
                Some(tree),
            ),
            ("npm test --workspace mnemo2", Some(0), Some(tree), Some("")),
        ],
    );

    assert!(
        counted_checks(
            "run_checks",
            &json!({ "purpose": "run the checks" }),
            &result,
            checkout,
            &allow_list(),
        )
        .is_empty(),
        "none of these runs may certify the tip"
    );
}

/// A real `run_checks` tool call rides the whole pump path like a counted
/// `shell` run: the worker session executes every allow-listed check for real,
/// and the finished turn records each passing one against the committed tip.
#[tokio::test]
async fn a_real_run_checks_run_records_every_passing_check_at_turn_end() {
    let (root, _state, _config, team, mut events) = report_streak_team(vec![tool_call_batch(
        "check-1",
        "run_checks",
        r#"{"purpose":"run the checks"}"#,
    )])
    .await;
    std::fs::create_dir_all(root.path().join(".coding")).expect("checks dir");
    std::fs::write(
        root.path().join(".coding/checks.toml"),
        "commands = [\"git status\", \"git diff --stat\"]\n",
    )
    .expect("checks.toml");
    let worker = team
        .dispatch("run the checks".to_string(), None)
        .await
        .expect("the worker dispatches");
    loop {
        let forwarded = forwarded_events(&mut events).await;
        if forwarded
            .iter()
            .any(|event| matches!(event, SerializableAgentEvent::Finished { .. }))
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let finished = team.worker(&worker.id).await.expect("worker record");

    let mut names: Vec<&str> = finished
        .checks
        .iter()
        .map(|check| check.name.as_str())
        .collect();
    names.sort_unstable();
    assert_eq!(
        names,
        ["git diff --stat", "git status"],
        "{:?}",
        finished.checks
    );
    let tip = phase3::tip(&finished.worktree).await.expect("worker tip");
    for check in &finished.checks {
        assert_eq!(check.status, "passed", "{:?}", finished.checks);
        assert_eq!(check.tip, tip, "{:?}", finished.checks);
        let detail = check.detail.as_deref().expect("worker detail");
        assert!(detail.contains("run by the worker"), "{detail}");
    }
    team.shutdown().await;
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
            check_commands: std::sync::Arc::new(Vec::new()),
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

    cleanup_agent_worktree(root.path(), &worker, &[])
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
                check_commands: std::sync::Arc::new(Vec::new()),
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
    let canonical = validated_checkout(root.path(), &record, &[])
        .expect("the checkout validates")
        .expect("it exists")
        .path()
        .to_path_buf();

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

    cleanup_agent_worktree(root.path(), &leftover, &[])
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
    let error = cleanup_agent_worktree(root.path(), &linked, &[])
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
        lane: None,
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
        lane: None,
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
            check_commands: std::sync::Arc::new(Vec::new()),
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
        lane: None,
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
        lane: None,
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
        lane: None,
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
        lane: None,
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

/// The retired Mnemo1 app (deleted 2026-11-11) left `runall-*` worktrees and
/// `wt/runall-*` branches in the same `.worktrees/` directory; Mnemo2's cleanup
/// only ever acts on its own `agent-*` records and refuses anything else, even
/// a forged record. The legacy checkout is built with plain git — no Mnemo2
/// code path may recreate it.
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
    // The retired app's shape: branch `wt/runall-<item8>`, checkout
    // `.worktrees/runall-<item8>` (gitignored, beside the agent ones).
    let runall_path = root.join(".worktrees").join("runall-abcd1234");
    std::fs::create_dir_all(root.join(".worktrees")).expect(".worktrees");
    lifecycle_git(
        root,
        &[
            "worktree",
            "add",
            "-b",
            "wt/runall-abcd1234",
            runall_path.to_str().expect("runall path"),
        ],
    );
    let runall_branch = "wt/runall-abcd1234".to_string();
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
        &[],
    )
    .await
    .expect("cleanup");
    assert_eq!(report.removed, vec![agent_branch.clone()]);
    assert!(!agent_path.exists());
    assert!(runall_path.is_dir(), "the retired app's checkout survives");
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

/// Intent-only evidence — what a light review records: the unanchored pass
/// and the intent alone.
fn light_evidence() -> ReviewEvidence {
    ReviewEvidence {
        unanchored_pass: true,
        intent: "the diff only pins the reviewed behavior".into(),
        execution: String::new(),
        integration: String::new(),
        test_quality: String::new(),
        residual_risk: String::new(),
        independent_reviewer_id: None,
        audit_adjudication: None,
    }
}

/// The full review lane for the merge-gate fixtures: a diff that is not
/// light, so the gate demands the complete evidence.
fn full_lane() -> ReviewLane {
    ReviewLane {
        light: false,
        reason: "62 changed lines in 3 non-test files".to_string(),
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
        lane: None,
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
    let error = validate_merge(&unevidenced, "tip0001", &[], &full_lane(), &proving::MergeProof::default())
        .expect_err("a legacy review must not merge without fresh evidence");
    assert!(error.contains("structured evidence"), "{error}");

    // An anchored-only pass is not substantive either.
    let mut unanchored = evidence();
    unanchored.unanchored_pass = false;
    let rejected = validate_merge(&in_review_worker(Some(unanchored)), "tip0001", &[], &full_lane(), &proving::MergeProof::default())
        .expect_err("unanchored evidence must not merge");
    assert!(rejected.contains("unanchored"), "{rejected}");

    // Empty assessments are rejected.
    let mut empty_intent = evidence();
    empty_intent.intent = "  ".into();
    let empty = validate_merge(&in_review_worker(Some(empty_intent)), "tip0001", &[], &full_lane(), &proving::MergeProof::default())
        .expect_err("empty intent must not merge");
    assert!(empty.contains("intent"), "{empty}");

    // Complete evidence passes the gate for standard-risk work.
    validate_merge(&in_review_worker(Some(evidence())), "tip0001", &[], &full_lane(), &proving::MergeProof::default())
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
    let missing = validate_merge(&high, "tip0001", &records_empty, &full_lane(), &proving::MergeProof::default())
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
    let untracked = validate_merge(&untracked_worker, "tip0001", &records_empty, &full_lane(), &proving::MergeProof::default())
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
    let wrong_kind = validate_merge(&untracked_worker, "tip0001", &[implementer], &full_lane(), &proving::MergeProof::default())
        .expect_err("an implementation worker is not an independent audit");
    assert!(wrong_kind.contains("not a read-only audit worker"), "{wrong_kind}");

    // An audit still working cannot be linked.
    let mut live = audit_worker(WorkerStatus::Working, "evidence0001", "tip0001");
    live.status = WorkerStatus::Working;
    let incomplete = validate_merge(&untracked_worker, "tip0001", &[live], &full_lane(), &proving::MergeProof::default())
        .expect_err("an unfinished audit must not merge");
    assert!(incomplete.contains("not completed"), "{incomplete}");

    // An audit of another worker is refused.
    let foreign = audit_worker(WorkerStatus::Completed, "other0001", "tip0001");
    let mismatched = validate_merge(&untracked_worker, "tip0001", &[foreign], &full_lane(), &proving::MergeProof::default())
        .expect_err("a foreign audit must not link");
    assert!(mismatched.contains("reviews 'other0001'"), "{mismatched}");

    // An audit at an older tip is stale after a fix commit moved it.
    let stale = audit_worker(WorkerStatus::Completed, "evidence0001", "tip0000");
    let stale_error = validate_merge(&untracked_worker, "tip0001", &[stale], &full_lane(), &proving::MergeProof::default())
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
        &full_lane(),
        &proving::MergeProof::default(),
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
        &full_lane(),
        &proving::MergeProof::default(),
    )
    .expect("a fresh completed audit with an adjudication merges");
}

/// A team whose worker branch carries one commit with `changes` relative to
/// the lead branch, so a review of the worker sees exactly that diff.
async fn lane_fixture(
    id: &str,
    changes: &[(&str, String)],
) -> (
    tempfile::TempDir,
    tempfile::TempDir,
    TeamRuntime,
    WorkerRecord,
) {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    // A merge refuses main, so the fixture reviews and merges on a real
    // lead branch.
    run_git(root.path(), &["checkout", "-b", "wt/lead"]);
    let record = worker(root.path(), id);
    provision_agent_worktree(
        root.path().to_path_buf(),
        record.id.clone(),
        "wt/lead".to_string(),
    )
    .await
    .expect("provision the worker worktree");
    for (path, contents) in changes {
        let file = record.worktree.join(path);
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).expect("change directory");
        }
        std::fs::write(&file, contents).expect("write the change");
    }
    run_git(&record.worktree, &["add", "."]);
    run_git(&record.worktree, &["commit", "-m", "worker change"]);
    let team = open_test_team(root.path(), state.path(), vec![record.clone()]).await;
    (root, state, team, record)
}

#[tokio::test]
async fn light_change_accepts_intent_only_evidence() {
    // A diff that only touches a *.test.ts file reviews on the unanchored
    // pass and the intent alone, and the answer says the light lane applied.
    let (_root, _state, team, record) = lane_fixture(
        "lightlane0001",
        &[("src/app.test.ts", "it('works', () => {});\n".to_string())],
    )
    .await;

    let reviewed = team
        .handle_lead_host_tool(
            "team_review",
            json!({
                "worker_id": record.id,
                "summary": "test-only change",
                "findings": [],
                "evidence": {
                    "unanchored_pass": true,
                    "intent": "the diff only pins the reviewed behavior"
                }
            }),
        )
        .await;
    assert!(reviewed.success, "{}", reviewed.output);
    assert!(
        reviewed.output.contains("light review"),
        "the answer says which lane applied: {}",
        reviewed.output
    );

    let persisted = team.worker(&record.id).await.expect("the reviewed worker");
    let review = persisted.review.as_ref().expect("the review record");
    assert_eq!(
        review.lane.as_deref(),
        Some("light review: test-only change"),
        "{persisted:?}"
    );
    // The light review persisted the intent alone: the four optional fields
    // stay empty and the record still decodes.
    let evidence = review.evidence.as_ref().expect("the evidence");
    assert_eq!(evidence.execution, "");
    assert_eq!(evidence.residual_risk, "");
    team.shutdown().await;
}

#[tokio::test]
async fn large_change_requires_full_evidence() {
    // A 200-line source change with the same intent-only evidence is
    // refused: the error names the changed-line count and the four fields
    // the full lane still requires.
    let (_root, _state, team, record) = lane_fixture(
        "lightlane0002",
        &[("src/lib.rs", "pub fn changed() {}\n".repeat(200))],
    )
    .await;

    let refused = team
        .handle_lead_host_tool(
            "team_review",
            json!({
                "worker_id": record.id,
                "summary": "large change",
                "findings": [],
                "evidence": {
                    "unanchored_pass": true,
                    "intent": "the diff only pins the reviewed behavior"
                }
            }),
        )
        .await;
    assert!(!refused.success);
    let error = refused.output;
    assert!(error.contains("not light"), "{error}");
    assert!(error.contains("200"), "{error}");
    for field in ["execution", "integration", "test_quality", "residual_risk"] {
        assert!(error.contains(field), "{error}");
    }
    // Nothing was recorded: the worker never entered review.
    let untouched = team.worker(&record.id).await.expect("the worker");
    assert_eq!(untouched.status, WorkerStatus::Completed);
    team.shutdown().await;
}

#[tokio::test]
async fn merge_gate_accepts_a_light_review() {
    // A light review records the unanchored pass and the intent alone, and
    // the merge gate — re-deciding lightness at the same tip — lets it
    // merge without the four optional assessments.
    let (_root, _state, team, record) = lane_fixture(
        "lightlane0003",
        &[("src/app.spec.ts", "it('works', () => {});\n".to_string())],
    )
    .await;

    let reviewed = team
        .review(
            &record.id,
            "test-only change".into(),
            Vec::new(),
            light_evidence(),
        )
        .await
        .expect("the light review records");
    assert_eq!(reviewed.status, WorkerStatus::InReview);
    assert_eq!(
        reviewed
            .review
            .as_ref()
            .and_then(|review| review.lane.clone()),
        Some("light review: test-only change".to_string())
    );

    team.record_check(&record.id, "cargo test".into(), "passed".into(), None)
        .await
        .expect("record the check");
    let merged = team
        .merge(&record.id)
        .await
        .expect("a light review merges");
    assert_eq!(merged.status, WorkerStatus::Merged);
    assert!(merged.merge.is_some());
    team.shutdown().await;
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
            category: None,
            enforce: None,
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
            category: None,
            enforce: None,
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
        lane: None,
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
            sender: MessageSender::User,
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
            sender: MessageSender::User,
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
            sender: MessageSender::User,
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
        category: None,
        enforce: None,
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

/// Every newly recorded finding lands in the workspace findings ledger, and
/// re-recording the same review appends nothing (plan 3c2ec71c step 1).
#[tokio::test]
async fn review_appends_findings_ledger_rows() {
    use crate::mnemo2::dispatch_routing::RoutingMode;
    let fixture = review_fixture(RoutingMode::Off, "ledger000001").await;
    let mut finding = open_finding("finding1");
    finding.category = Some("vacuous-test".to_string());
    finding.enforce = Some("test".to_string());
    fixture
        .team
        .review(
            &fixture.record.id,
            "first pass".into(),
            vec![finding.clone()],
            evidence(),
        )
        .await
        .expect("record the review");
    let path = fixture._root.path().join(".coding/reviews/findings.jsonl");
    let rows = std::fs::read_to_string(&path).expect("findings ledger");
    let rows: Vec<serde_json::Value> = rows
        .lines()
        .map(|line| serde_json::from_str(line).expect("ledger row"))
        .collect();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["worker_id"], fixture.record.id.as_str());
    assert_eq!(rows[0]["finding_id"], "finding1");
    assert_eq!(rows[0]["category"], "vacuous-test");
    assert_eq!(rows[0]["enforce"], "test");
    assert_eq!(rows[0]["tier"], "high");
    fixture
        .team
        .review(
            &fixture.record.id,
            "second pass".into(),
            vec![finding],
            evidence(),
        )
        .await
        .expect("re-record the same finding");
    let rows = std::fs::read_to_string(&path).expect("findings ledger");
    assert_eq!(rows.lines().count(), 1, "the same review appends nothing");
    fixture.team.shutdown().await;
}

/// A review's answer tells the lead which finding categories are in use, so
/// it can reuse one instead of inventing a slug (plan 3c2ec71c step 1).
#[tokio::test]
async fn team_review_answer_lists_finding_categories() {
    use crate::mnemo2::dispatch_routing::RoutingMode;
    let fixture = review_fixture(RoutingMode::Off, "category000001").await;
    let args = json!({
        "worker_id": fixture.record.id,
        "summary": "checked the diff",
        "findings": [{
            "title": "vacuous assert",
            "detail": "asserts true",
            "file": "src/lib.rs",
            "severity": "low",
            "category": "vacuous-test",
            "enforce": "test"
        }],
        "evidence": serde_json::to_value(evidence()).expect("evidence"),
    });
    let answer = fixture.team.handle_lead_host_tool("team_review", args).await;
    assert!(answer.success, "{answer:?}");
    assert!(
        answer
            .output
            .contains("Finding categories in use: vacuous-test (1)"),
        "{}",
        answer.output
    );
    fixture.team.shutdown().await;
}

/// A review answer says when a category has now come up in its third worker's
/// review, and that review adds exactly one backlog proposal for a guard
/// (plan 3c2ec71c step 2).
#[tokio::test]
async fn third_worker_with_category_proposes_a_guard() {
    use crate::mnemo2::dispatch_routing::{save_routing_settings, RoutingMode, RoutingSettings};
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    let config = tempfile::tempdir().expect("config");
    init_git(root.path());
    save_routing_settings(
        config.path(),
        &RoutingSettings {
            mode: RoutingMode::Off,
            ..RoutingSettings::default()
        },
    )
    .expect("routing settings");
    let mut records = Vec::new();
    // Distinct in their first 8 chars: that prefix names the branch/worktree.
    for id in ["thrdwkr1", "thrdwkr2", "thrdwkr3", "thrdwkr4"] {
        let record = worker(root.path(), id);
        provision_agent_worktree(
            root.path().to_path_buf(),
            record.id.clone(),
            "main".to_string(),
        )
        .await
        .expect("worker checkout");
        records.push(record);
    }
    let team = open_test_team_with_config(
        root.path(),
        state.path(),
        records.clone(),
        config.path(),
    )
    .await;
    let review = |record: &WorkerRecord| {
        json!({
            "worker_id": record.id,
            "summary": "checked the diff",
            "findings": [{
                "title": "vacuous assert",
                "detail": "asserts true",
                "file": "src/lib.rs",
                "severity": "low",
                "category": "vacuous-test",
                "enforce": "test"
            }],
            "evidence": serde_json::to_value(evidence()).expect("evidence"),
        })
    };
    let backlog = root.path().join(".coding").join("backlog.jsonl");
    let marker = "[finding-category:vacuous-test]";
    for record in &records[..2] {
        let answer = team.handle_lead_host_tool("team_review", review(record)).await;
        assert!(answer.success, "{answer:?}");
        assert!(
            !answer.output.contains("Finding category 'vacuous-test'"),
            "two workers are not a pattern yet: {}",
            answer.output
        );
    }
    assert!(!backlog.exists(), "two workers propose nothing");
    let answer = team.handle_lead_host_tool("team_review", review(&records[2])).await;
    assert!(answer.success, "{answer:?}");
    assert!(
        answer.output.contains(
            "Finding category 'vacuous-test' has now come up in 3 reviews; \
             a backlog item proposes enforcing it."
        ),
        "{}",
        answer.output
    );
    let proposals: Vec<String> = std::fs::read_to_string(&backlog)
        .expect("backlog file")
        .lines()
        .filter(|line| line.contains(marker))
        .map(str::to_string)
        .collect();
    assert_eq!(proposals.len(), 1, "{proposals:?}");
    let answer = team.handle_lead_host_tool("team_review", review(&records[3])).await;
    assert!(answer.success, "{answer:?}");
    assert!(
        answer.output.contains(
            "Finding category 'vacuous-test' has now come up in 4 reviews; \
             a backlog item proposes enforcing it."
        ),
        "{}",
        answer.output
    );
    let after = std::fs::read_to_string(&backlog).expect("backlog file");
    assert_eq!(
        after.lines().filter(|line| line.contains(marker)).count(),
        1,
        "a fourth worker adds no second proposal"
    );
    team.shutdown().await;
}
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
    // The finished audit no longer counts against the slots. The release runs
    // after the finish is persisted, so poll instead of racing it.
    eventually(|| async { (team.live.lock().await.is_empty()).then_some(()) }).await;
    // The freed slot dispatches a fresh audit with no manual stop
    // (a different id prefix: the finished audit's branch stays).
    team.audit_dispatch(&target.id, "audittwo001", None, None, None)
        .await
        .expect("the released slot dispatches a fresh audit");
    team.shutdown().await;
}

/// An audit that wrote its findings as its final message instead of calling
/// `message_parent` still delivers a report (bug 2026-11-16): the pump takes
/// the turn's final assistant text, records it as the report — the same
/// `TeamMessage` and the same forwarded `message_parent` call — before the
/// turn's `Finished` event, so the lead reads the report notice first and the
/// audit frees its slot in the same pass.
#[tokio::test]
async fn an_audit_ending_with_plain_text_delivers_it_as_its_report() {
    const FINDINGS: &str = "Audit findings: none.";
    let (fixture, _notices) = nudge_fixture_with(
        crate::mnemo2::dispatch_routing::RoutingSettings::default(),
        None,
        std::time::Duration::ZERO,
        vec![vec![
            // The audit writes its findings and ends its turn: it never
            // calls message_parent (the fourthAud1 case).
            mnemo::provider::LlmEvent::TextDelta {
                text: FINDINGS.to_string(),
            },
            mnemo::provider::LlmEvent::Finish {
                reason: mnemo::provider::FinishReason::Stop,
            },
        ]],
    )
    .await;
    let team = &fixture.team;
    // The forwarded event stream: its order is the order of the lead's
    // notices.
    let mut events = team.subscribe();
    // One slot: the audit itself is the only worker that may hold it.
    team.persistence.slots.store(1, Ordering::SeqCst);
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
        (record.status == WorkerStatus::Completed && record.report_delivered)
            .then_some(record)
    })
    .await;
    // The plain-text turn delivered a report, exactly as a message_parent
    // call would have.
    assert!(finished.report_delivered, "{finished:?}");
    let reports: Vec<_> = team
        .messages()
        .await
        .into_iter()
        .filter(|message| message.worker_id == audit_id)
        .collect();
    assert_eq!(
        reports.len(),
        1,
        "one report, not an empty or repeated one: {reports:?}"
    );
    assert_eq!(reports[0].text, FINDINGS, "{reports:?}");
    // The report notice precedes the adjudication notice: the pump forwards
    // the report as a `message_parent` call before the turn's `Finished`
    // event.
    let forwarded = forwarded_events(&mut events).await;
    let report_at = forwarded
        .iter()
        .position(|event| {
            matches!(
                event,
                SerializableAgentEvent::HostToolCall { name, .. }
                    if name.as_str() == "message_parent"
            )
        })
        .unwrap_or_else(|| {
            panic!("the report is forwarded as a message_parent call: {forwarded:?}")
        });
    let finished_at = forwarded
        .iter()
        .position(|event| matches!(event, SerializableAgentEvent::Finished { .. }))
        .unwrap_or_else(|| panic!("the turn's finish is forwarded: {forwarded:?}"));
    assert!(
        report_at < finished_at,
        "the report notice precedes the adjudication notice: {forwarded:?}"
    );
    // The reported audit released its session in the same pass.
    eventually(|| async { (team.live.lock().await.is_empty()).then_some(()) }).await;
    // The freed slot dispatches a fresh audit with no manual stop.
    team.audit_dispatch(&target.id, "audittwo001", None, None, None)
        .await
        .expect("the released slot dispatches a fresh audit");
    team.shutdown().await;
}

/// An audit that ends its turn with no text at all is asked for its report
/// (bug 2026-11-16): one automatic host message tells it to send its findings
/// through `message_parent`, and its session stays available to answer
/// instead of being released with nothing delivered.
#[tokio::test]
async fn an_audit_ending_silently_is_asked_for_its_report_once() {
    const ASK: &str = "Your audit report did not reach the lead. Send your complete findings with message_parent now, then end your turn.";
    let (fixture, _notices) = nudge_fixture_with(
        crate::mnemo2::dispatch_routing::RoutingSettings::default(),
        None,
        std::time::Duration::ZERO,
        vec![
            // The audit's first turn ends with no text and no report.
            vec![mnemo::provider::LlmEvent::Finish {
                reason: mnemo::provider::FinishReason::Stop,
            }],
        ],
    )
    .await;
    let team = &fixture.team;
    team.persistence.slots.store(1, Ordering::SeqCst);
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

    let (audit, _reused) = team
        .audit_dispatch(&target.id, "auditworker1", None, None, None)
        .await
        .expect("dispatch the audit into the one slot");
    let audit_id = audit.id.clone();
    let asked = eventually(|| async {
        let record = team.worker(&audit_id).await.ok()?;
        (!record.inbox.is_empty()).then_some(record)
    })
    .await;
    // One automatic message, from the host, with the decided words.
    assert_eq!(asked.inbox.len(), 1, "one automatic ask: {asked:?}");
    assert_eq!(asked.inbox[0].sender, MessageSender::Mnemo, "{asked:?}");
    assert_eq!(asked.inbox[0].kind, InboxKind::Prompt, "{asked:?}");
    assert_eq!(asked.inbox[0].text, ASK, "{asked:?}");
    // The silent turn delivered no empty report, and its session was not
    // released: the ask's follow-up turn (the scripted default "done" text)
    // is what carries the report and frees the slot.
    let finished = eventually(|| async {
        let record = team.worker(&audit_id).await.ok()?;
        (record.status == WorkerStatus::Completed && record.report_delivered)
            .then_some(record)
    })
    .await;
    assert!(finished.report_delivered, "{finished:?}");
    let reports: Vec<_> = team
        .messages()
        .await
        .into_iter()
        .filter(|message| message.worker_id == audit_id)
        .collect();
    assert_eq!(
        reports.len(),
        1,
        "the silent turn delivered no report: {reports:?}"
    );
    assert_eq!(
        reports[0].text, "done",
        "the ask's follow-up turn carried the report: {reports:?}"
    );
    let final_record = team.worker(&audit_id).await.expect("the audit record");
    assert_eq!(
        final_record.inbox.len(),
        1,
        "exactly one ask, never a second: {final_record:?}"
    );
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

/// A finished audit waits on nobody, so the session-start summary must not count it. An implementation worker awaiting review and an audit that failed before reporting still wait on the lead.
#[tokio::test]
async fn external_summary_does_not_count_finished_audits() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let mut finished = worker(root.path(), "auditworker1");
    finished.kind = "audit".to_string();
    finished.status = WorkerStatus::Completed;
    finished.report_delivered = true;
    let mut reviewing = worker(root.path(), "implworker01");
    reviewing.status = WorkerStatus::Completed;
    let mut failed_audit = worker(root.path(), "auditworker2");
    failed_audit.kind = "audit".to_string();
    failed_audit.status = WorkerStatus::Failed;
    let team = open_test_team(root.path(), state.path(), vec![finished, reviewing, failed_audit]).await;
    let summary = team.external_summary().await;
    assert!(summary.contains("2 waiting on you"), "{summary}");
    assert!(summary.contains("3 total"), "{summary}");
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
    /// `Some((first_gated_call, open))`: calls numbered `first_gated_call`
    /// (1-based) and later wait until `open` reads true; earlier calls
    /// answer at once. `None`: no call ever waits.
    gate: Option<(usize, tokio::sync::watch::Receiver<bool>)>,
    /// Every text Laya was asked about, in call order.
    texts: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl StubDispatchClassifier {
    fn new(answer: Option<mnemo::memory::Answer>) -> Self {
        Self {
            answer,
            calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            delay: std::time::Duration::ZERO,
            gate: None,
            texts: std::sync::Arc::default(),
        }
    }

    fn slow(answer: Option<mnemo::memory::Answer>, delay: std::time::Duration) -> Self {
        Self { delay, ..Self::new(answer) }
    }

    /// A classifier whose calls from `first_gated_call` (1-based) on block
    /// until the returned sender sends `true`; earlier calls answer at once.
    fn gated(
        answer: Option<mnemo::memory::Answer>,
        first_gated_call: usize,
    ) -> (Self, tokio::sync::watch::Sender<bool>) {
        let (open, gate) = tokio::sync::watch::channel(false);
        (
            Self { gate: Some((first_gated_call, gate)), ..Self::new(answer) },
            open,
        )
    }

    fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn texts(&self) -> Vec<String> {
        self.texts.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[async_trait::async_trait]
impl mnemo::memory::Classifier for StubDispatchClassifier {
    async fn classify(
        &self,
        state: &str,
        _question: &mnemo::memory::Question,
    ) -> Option<mnemo::memory::Answer> {
        self.texts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(state.to_string());
        let call = self
            .calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        if let Some((first_gated_call, gate)) = &self.gate {
            if call >= *first_gated_call {
                let mut gate = gate.clone();
                // A dropped sender also ends the wait: the test is over.
                let _ = gate.wait_for(|open| *open).await;
            }
        }
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

/// A team whose engine serves scripted LLM batches (plan 9a6cffc1's
/// report-streak tests): a dispatched worker runs real turns and makes
/// real `message_parent` host-tool calls, which the pump answers. When
/// the scripted batches run out, the LLM serves a plain "done" text
/// turn, so a turn the host does not cut short ends naturally. Returns
/// the team plus a `TeamEvent` subscription taken before any dispatch,
/// so a test sees exactly what the pump forwards.
async fn report_streak_team(
    responses: Vec<Vec<mnemo::provider::LlmEvent>>,
) -> (
    tempfile::TempDir,
    tempfile::TempDir,
    tempfile::TempDir,
    TeamRuntime,
    broadcast::Receiver<TeamEvent>,
) {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    let config = tempfile::tempdir().expect("config");
    init_git(root.path());
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
    let team = TeamRuntime::open(TeamRuntimeOptions {
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
    let events = team.subscribe();
    (root, state, config, team, events)
}

/// The events the pump has forwarded so far (plan 9a6cffc1): the raw
/// event bodies, in order, tolerating lag. What is missing from this
/// list never reaches the runtime — so it never becomes a lead notice.
async fn forwarded_events(
    events: &mut broadcast::Receiver<TeamEvent>,
) -> Vec<SerializableAgentEvent> {
    let mut forwarded = Vec::new();
    loop {
        match events.try_recv() {
            Ok(event) => forwarded.push(event.event.event),
            Err(broadcast::error::TryRecvError::Empty) => return forwarded,
            Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
            Err(broadcast::error::TryRecvError::Closed) => return forwarded,
        }
    }
}

/// How many `message_parent` host-tool calls the pump forwarded (plan
/// 9a6cffc1) — one forwarded call is one potential lead notice.
fn forwarded_report_calls(forwarded: &[SerializableAgentEvent]) -> usize {
    forwarded
        .iter()
        .filter(|event| {
            matches!(
                event,
                SerializableAgentEvent::HostToolCall { name, .. }
                    if name.as_str() == "message_parent"
            )
        })
        .count()
}

/// Poll the worker record until `want` holds (plan 9a6cffc1), then
/// return it; panic with the last record after ~10s. The sleep before
/// the caller reads the event stream covers the pump's own forwarding
/// step, which runs after the record update the wait observes. The
/// record's transcript lands via a detached snapshot task a few ms
/// after the turn ends (the pump's post-turn refresh), so a `want`
/// that needs transcript content polls for it here instead of
/// trusting a single read.
async fn wait_for_worker_record(
    team: &TeamRuntime,
    worker_id: &str,
    want: impl Fn(&WorkerRecord) -> bool,
) -> WorkerRecord {
    let mut last = None;
    for _ in 0..400 {
        let record = team.worker(worker_id).await.expect("worker record");
        if want(&record) {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            return team.worker(worker_id).await.expect("worker record");
        }
        last = Some(record);
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for the worker record: {last:?}");
}

/// Plan 9a6cffc1: the first report in a turn keeps today's behaviour —
/// recorded in the lead's inbox, forwarded as a TeamEvent (what the
/// runtime turns into the lead's `reports:` notice), and answered with
/// the byte-identical result text.
#[tokio::test]
async fn first_report_is_unchanged() {
    let (_root, _state, _config, team, mut events) = report_streak_team(vec![
        tool_call_batch("c1", "message_parent", r#"{"text":"task complete"}"#),
        tool_call_batch("c2", "message_parent", r#"{"text":"task complete, again"}"#),
    ])
    .await;
    let worker = team
        .dispatch("do the work".to_string(), None)
        .await
        .expect("the worker dispatches");
    let record = wait_for_worker_record(&team, &worker.id, |record| {
        record.status == WorkerStatus::Completed
            && record.pending.is_none()
            && record
                .transcript
                .messages
                .iter()
                .any(|message| {
                    message.name.as_deref() == Some("message_parent")
                        && message.content.as_text()
                            == "message sent to the lead. A reply, if any, arrives later as a new message that starts with a [From …] header; do not assume or invent a reply — continue your task or end your turn."
                })
    })
    .await;
    // The inbox holds the report verbatim.
    let messages = team.messages().await;
    assert_eq!(messages.len(), 1, "exactly one TeamMessage: {messages:?}");
    assert_eq!(messages[0].worker_id, worker.id);
    assert_eq!(messages[0].text, "task complete");
    // The call was forwarded — exactly once — so the lead sees the notice.
    let forwarded = forwarded_events(&mut events).await;
    assert_eq!(
        forwarded_report_calls(&forwarded),
        1,
        "the first report's call was forwarded: {forwarded:?}"
    );
    // And the result the worker saw is byte-identical to today's.
    let reported = record
        .transcript
        .messages
        .iter()
        .find(|message| message.name.as_deref() == Some("message_parent"))
        .cloned()
        .expect("the report's tool result in the transcript");
    assert!(!reported.tool_is_error);
    assert_eq!(
        reported.content.as_text(),
        "message sent to the lead. A reply, if any, arrives later as a new message that starts with a [From …] header; do not assume or invent a reply — continue your task or end your turn."
    );
    team.shutdown().await;
}

/// Plan 9a6cffc1: the 2nd `message_parent` call in a row — no other
/// tool call in between — is refused: not recorded as a TeamMessage and
/// not forwarded as a TeamEvent, so the runtime never turns it into a
/// lead notice. The refusal tells the worker to end its turn now, with
/// no further tool calls.
#[tokio::test]
async fn second_report_in_a_row_is_not_forwarded() {
    let (_root, _state, _config, team, mut events) = report_streak_team(vec![
        tool_call_batch("c1", "message_parent", r#"{"text":"task complete"}"#),
        tool_call_batch("c2", "message_parent", r#"{"text":"task complete — please review"}"#),
    ])
    .await;
    let worker = team
        .dispatch("do the work".to_string(), None)
        .await
        .expect("the worker dispatches");
    let record = wait_for_worker_record(&team, &worker.id, |record| {
        let reports: Vec<_> = record
            .transcript
            .messages
            .iter()
            .filter(|message| message.name.as_deref() == Some("message_parent"))
            .collect();
        record.status == WorkerStatus::Completed
            && record.pending.is_none()
            && reports.len() == 2
            && reports[1].content.as_text()
                == "not sent to the lead: this report already reached it. End your turn now, with no further tool calls, so the lead can review your work."
    })
    .await;
    // Only the first report reached the inbox.
    let messages = team.messages().await;
    assert_eq!(
        messages.len(),
        1,
        "the repeat is not recorded as a TeamMessage: {messages:?}"
    );
    assert_eq!(messages[0].text, "task complete");
    // And only the first report's call was forwarded — the repeat never
    // becomes a lead notice.
    let forwarded = forwarded_events(&mut events).await;
    assert_eq!(
        forwarded_report_calls(&forwarded),
        1,
        "the repeat is not forwarded as a TeamEvent: {forwarded:?}"
    );
    // The repeat's answer names what to do next: end the turn now, with
    // no further tool calls.
    let reports: Vec<_> = record
        .transcript
        .messages
        .iter()
        .filter(|message| message.name.as_deref() == Some("message_parent"))
        .collect();
    assert_eq!(reports.len(), 2, "both calls are answered: {reports:?}");
    assert!(!reports[1].tool_is_error, "the refusal is not an error");
    assert_eq!(
        reports[1].content.as_text(),
        "not sent to the lead: this report already reached it. End your turn now, with no further tool calls, so the lead can review your work."
    );
    team.shutdown().await;
}

/// Plan 9a6cffc1: an ordinary tool call between two reports breaks the
/// run of back-to-back reports — the second report is forwarded like a
/// first one (the acceptance scenario: report, read a file, report
/// again produces two report notices).
#[tokio::test]
async fn other_tool_call_resets_the_report_streak() {
    let (_root, _state, _config, team, mut events) = report_streak_team(vec![
        tool_call_batch("c1", "message_parent", r#"{"text":"halfway there"}"#),
        tool_call_batch("c2", "read_files", r#"{"files":["README.md"]}"#),
        tool_call_batch("c3", "message_parent", r#"{"text":"task complete"}"#),
    ])
    .await;
    let worker = team
        .dispatch("do the work".to_string(), None)
        .await
        .expect("the worker dispatches");
    let _record = wait_for_worker_record(&team, &worker.id, |record| {
        record.status == WorkerStatus::Completed && record.pending.is_none()
    })
    .await;
    let messages = team.messages().await;
    assert_eq!(
        messages.len(),
        2,
        "both reports are recorded: {messages:?}"
    );
    assert_eq!(messages[0].text, "halfway there");
    assert_eq!(messages[1].text, "task complete");
    let forwarded = forwarded_events(&mut events).await;
    assert_eq!(
        forwarded_report_calls(&forwarded),
        2,
        "both reports' calls are forwarded: {forwarded:?}"
    );
    team.shutdown().await;
}

/// Plan 9a6cffc1: a `memory_*` call between two reports is not progress
/// — the second report still counts as a repeat and is refused.
#[tokio::test]
async fn memory_writes_do_not_reset_the_report_streak() {
    let (_root, _state, _config, team, mut events) = report_streak_team(vec![
        tool_call_batch("c1", "message_parent", r#"{"text":"task complete"}"#),
        tool_call_batch(
            "c2",
            "memory_write",
            r#"{"tier":"semantic","title":"t","content":"c"}"#,
        ),
        tool_call_batch("c3", "message_parent", r#"{"text":"task complete — see memory"}"#),
    ])
    .await;
    let worker = team
        .dispatch("do the work".to_string(), None)
        .await
        .expect("the worker dispatches");
    let _record = wait_for_worker_record(&team, &worker.id, |record| {
        record.status == WorkerStatus::Completed && record.pending.is_none()
    })
    .await;
    let messages = team.messages().await;
    assert_eq!(
        messages.len(),
        1,
        "the memory write did not reset the streak: {messages:?}"
    );
    assert_eq!(messages[0].text, "task complete");
    let forwarded = forwarded_events(&mut events).await;
    assert_eq!(
        forwarded_report_calls(&forwarded),
        1,
        "the post-memory repeat is refused: {forwarded:?}"
    );
    team.shutdown().await;
}

/// Plan 9a6cffc1: the 3rd report in a row is refused and the host ends
/// the worker's turn for it. The interrupt lands in the normal
/// `Finished` path, so the lead gets the ordinary "finished its turn"
/// flow and the worker is left idle for review. Proof the turn was cut
/// short for the worker: the scripted queue is empty from the 3rd call
/// on, so an uninterrupted turn would stream a fallback text "done"
/// turn before finishing — no text delta may appear.
#[tokio::test]
async fn third_report_in_a_row_ends_the_turn() {
    let (_root, _state, _config, team, mut events) = report_streak_team(vec![
        tool_call_batch("c1", "message_parent", r#"{"text":"task complete"}"#),
        tool_call_batch("c2", "message_parent", r#"{"text":"task complete (repeat)"}"#),
        tool_call_batch("c3", "message_parent", r#"{"text":"task complete (again)"}"#),
    ])
    .await;
    let worker = team
        .dispatch("do the work".to_string(), None)
        .await
        .expect("the worker dispatches");
    let record = wait_for_worker_record(&team, &worker.id, |record| {
        record.status == WorkerStatus::Completed && record.pending.is_none()
    })
    .await;
    // The worker ends up idle for review: completed, nothing pending.
    assert_eq!(record.status, WorkerStatus::Completed);
    assert!(record.pending.is_none(), "idle: {record:?}");
    // One report recorded, one report call forwarded — the 2nd and 3rd
    // were refused.
    let messages = team.messages().await;
    assert_eq!(
        messages.len(),
        1,
        "only the first report is recorded: {messages:?}"
    );
    let forwarded = forwarded_events(&mut events).await;
    assert_eq!(
        forwarded_report_calls(&forwarded),
        1,
        "only the first report is forwarded: {forwarded:?}"
    );
    // The turn was ended for the worker at the third report: no tool call
    // started after it. A short final text may still stream on a fast runner
    // before the interrupt lands; the promise is that reporting stops.
    let tool_starts: Vec<&str> = forwarded
        .iter()
        .filter_map(|event| match event {
            SerializableAgentEvent::ToolCallStart { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        tool_starts,
        vec!["c1", "c2", "c3"],
        "no tool call after the third report: {forwarded:?}"
    );
    // And it ended through the ordinary Finished path, which the lead
    // turns into the "finished its turn" notice.
    assert!(
        forwarded
            .iter()
            .any(|event| matches!(event, SerializableAgentEvent::Finished { .. })),
        "the ended turn finished normally: {forwarded:?}"
    );
    team.shutdown().await;
}

/// The nudge fixture with a stub classifier that answers `answer` after
/// `delay`.
async fn nudge_fixture_with(
    settings: crate::mnemo2::dispatch_routing::RoutingSettings,
    answer: Option<mnemo::memory::Answer>,
    delay: std::time::Duration,
    responses: Vec<Vec<mnemo::provider::LlmEvent>>,
) -> (RoutingFixture, broadcast::Receiver<BoardSignal>) {
    nudge_fixture_with_classifier(
        settings,
        StubDispatchClassifier::slow(answer, delay),
        responses,
    )
    .await
}

/// The nudge tests' live team (design §4.6): the routing settings come
/// from the config dir, all three tiers are in the catalog, and the
/// worker sessions run real turns served by the scripted LLM — so the
/// pump observes real tool results and the strike counter counts real
/// failures. Returns the usual routing fixture plus a board-signal
/// subscription taken before any dispatch, to catch nudge notices.
/// The caller builds the classifier (a gated one, say).
async fn nudge_fixture_with_classifier(
    settings: crate::mnemo2::dispatch_routing::RoutingSettings,
    classifier: StubDispatchClassifier,
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
    let classifier = std::sync::Arc::new(classifier);
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

/// An answer to a read-back in the shape the 2026-11-08 nudges fired on: long
/// enough for the drift check, and it adds no work.
const READ_BACK_ANSWER: &str = "Answers are in the plan's Decisions as the step's read-back answers: your proposals stand on all four questions, so proceed with the step as specified, implement it, run the verification commands and commit on your branch.";

/// A nudge fixture whose Laya reads every text as hard, so any message that
/// reaches the drift check nudges a routine worker.
async fn hard_reading_nudge_fixture() -> (RoutingFixture, broadcast::Receiver<BoardSignal>) {
    nudge_fixture_with(
        crate::mnemo2::dispatch_routing::RoutingSettings {
            mode: crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
            ..crate::mnemo2::dispatch_routing::RoutingSettings::default()
        },
        routing_answer(mnemo::agent::route_question::LABEL_HARD, 0.91),
        std::time::Duration::ZERO,
        Vec::new(),
    )
    .await
}

/// Send a lead message that adds real scope, wait for its drift nudge, and
/// return every text Laya was asked about. The nudge is the sync point for the
/// no-drift tests: drift checks take the routing permit one at a time in the
/// order they were spawned, so a check an earlier message started has already
/// asked Laya by the time this nudge lands.
async fn classified_when_scope_nudges(fixture: &RoutingFixture) -> Vec<String> {
    fixture
        .team
        .message("routingworker", drift_message(), MessageSender::ExternalLead)
        .await
        .expect("deliver the scope message");
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
    fixture.classifier.texts()
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
    let rows = eventually(|| async {
        let rows = fixture.rows();
        (rows.len() >= 3).then_some(rows)
    })
    .await;
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

/// A stale-read redirect is steering, not a strike (plan 5b85ab61 step 4).
/// One drift-class failure arms the gate on README.md; the worker then retries
/// with THREE different `old_string`s, so every retry is a fresh call that the
/// gate intercepts (EDIT INTERCEPTED). Before the fix each interception counted
/// toward the strike streak, so the third one nudged the lead with "looks
/// stuck" — while the worker was doing exactly what the gate told it to.
#[tokio::test]
async fn stale_read_redirects_do_not_nudge_the_lead() {
    let edit = |id: &str, old_string: &str| {
        tool_call_batch(
            id,
            "file_edit",
            &format!(
                "{{\"path\":\"README.md\",\"old_string\":\"{old_string}\",\"new_string\":\"X\"}}"
            ),
        )
    };
    let responses = vec![
        // The read the edit needs to be judged a drift, not a blind write.
        tool_call_batch("call-read", "read_files", "{\"path\":\"README.md\"}"),
        // A drift-class failure: old_string not found. Arms the gate.
        edit("call-drift", "missing-one"),
        // Three intercepted retries with distinct arguments — no exact repeat.
        edit("call-e1", "missing-two"),
        edit("call-e2", "missing-three"),
        edit("call-e3", "missing-four"),
        vec![
            mnemo::provider::LlmEvent::TextDelta {
                text: "done".to_string(),
            },
            mnemo::provider::LlmEvent::Finish {
                reason: mnemo::provider::FinishReason::Stop,
            },
        ],
    ];
    let (fixture, _notices) = nudge_fixture(responses).await;
    let result = fixture.dispatch(Some("routine"), None).await;
    assert!(result.success, "{}", result.output);
    let settled = wait_worker_settled(&fixture.team, "routingworker").await;

    let rows = fixture.rows();
    assert!(
        !rows
            .iter()
            .any(|row| matches!(row, DispatchRoutingRow::Nudge { .. })),
        "intercepted retries must not nudge the lead: {rows:?}"
    );
    assert!(
        !matches!(settled.status, WorkerStatus::Failed),
        "the worker finished normally: {settled:?}"
    );

    // The redirects really happened: the worker's transcript carries the
    // gate's marker, so "no nudge" is not "the gate never ran".
    let transcript = fixture
        .state
        .path()
        .join(TRANSCRIPTS_DIR)
        .join("routingworker.json");
    let saved = eventually(|| async {
        std::fs::read_to_string(&transcript)
            .ok()
            .filter(|text| text.contains(mnemo::EDIT_GATE_MARK))
    })
    .await;
    assert_eq!(
        saved.matches(mnemo::EDIT_GATE_MARK).count(),
        3,
        "all three retries were intercepted (not drift failures): {saved}"
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

/// Drift classification is detached: the message returns while Laya's
/// drift answer is still held back, and the nudge lands once Laya answers
/// (design §4.6). A gate, not a clock, holds Laya back, so machine load
/// cannot fail the test.
#[tokio::test]
async fn drift_never_blocks_the_message() {
    // Call 1 is the dispatch's deferred classification and answers at
    // once; call 2 is the drift check and waits for the gate.
    let (classifier, open_gate) = StubDispatchClassifier::gated(
        routing_answer(mnemo::agent::route_question::LABEL_HARD, 0.91),
        2,
    );
    let (fixture, _notices) = nudge_fixture_with_classifier(
        crate::mnemo2::dispatch_routing::RoutingSettings {
            mode: crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
            ..crate::mnemo2::dispatch_routing::RoutingSettings::default()
        },
        classifier,
        vec![vec![
            mnemo::provider::LlmEvent::TextDelta { text: "done".to_string() },
            mnemo::provider::LlmEvent::Finish { reason: mnemo::provider::FinishReason::Stop },
        ]],
    )
    .await;
    let result = fixture.dispatch(Some("routine"), None).await;
    assert!(result.success, "{}", result.output);
    wait_worker_settled(&fixture.team, "routingworker").await;
    // The dispatch's own classification has answered (its Route row is
    // written after Laya replies), so the next classifier call is the
    // drift check.
    wait_route_row(&fixture).await;
    assert_eq!(
        fixture.classifier.calls(),
        1,
        "only the dispatch classified before the message"
    );
    // While the gate is closed Laya cannot answer. A message that waited
    // on drift classification would hang here, or let the drift check
    // time out behind the closed gate so that no nudge lands below.
    tokio::time::timeout(
        std::time::Duration::from_secs(20),
        fixture
            .team
            .message("routingworker", drift_message(), MessageSender::Lead),
    )
    .await
    .expect("the message returned while Laya's drift answer was held back")
    .expect("the message is delivered");
    assert!(
        !fixture
            .rows()
            .iter()
            .any(|row| matches!(row, DispatchRoutingRow::Nudge { .. })),
        "no nudge can land before Laya answers"
    );
    open_gate
        .send(true)
        .expect("the stub classifier still holds the gate");
    let row = eventually(|| async {
        fixture
            .rows()
            .into_iter()
            .find(|row| matches!(row, DispatchRoutingRow::Nudge { .. }))
    })
    .await;
    assert!(
        matches!(&row, DispatchRoutingRow::Nudge { reason, .. } if reason == "drift"),
        "the held-back drift nudge still landed: {row:?}"
    );
    assert_eq!(
        fixture.classifier.calls(),
        2,
        "the dispatch and the drift check each asked Laya once"
    );
    fixture.team.shutdown().await;
}

/// A read-back worker's first lead message answers its questions and adds no
/// work: Laya never reads it, and the worker's one nudge stays available for a
/// later message that really adds scope (design §4.6).
#[tokio::test]
async fn read_back_answer_draws_no_drift_nudge() {
    assert!(
        READ_BACK_ANSWER.split_whitespace().count()
            >= crate::mnemo2::dispatch_routing::MIN_WORDS,
        "long enough for the drift check"
    );
    let (fixture, _notices) = hard_reading_nudge_fixture().await;
    let result = fixture
        .team
        .handle_lead_host_tool(
            "team_dispatch",
            json!({
                "worker_id": "routingworker",
                "plan_id": fixture.plan_id,
                "step": 1,
                "tier": "routine",
                "read_back": true,
                "brief": "Implement the step exactly and run its focused regression tests.",
            }),
        )
        .await;
    assert!(result.success, "{}", result.output);
    wait_worker_settled(&fixture.team, "routingworker").await;
    wait_route_row(&fixture).await;
    assert_eq!(
        fixture.classifier.calls(),
        1,
        "only the dispatch classified before the answer"
    );
    fixture
        .team
        .message(
            "routingworker",
            READ_BACK_ANSWER.to_string(),
            MessageSender::ExternalLead,
        )
        .await
        .expect("deliver the read-back answer");
    wait_worker_settled(&fixture.team, "routingworker").await;
    let classified = classified_when_scope_nudges(&fixture).await;
    assert!(
        !classified.iter().any(|text| text == READ_BACK_ANSWER),
        "the read-back answer reached the drift check: {classified:#?}"
    );
    assert!(
        classified.contains(&drift_message()),
        "the scope message after the answer is still read: {classified:#?}"
    );
    assert_eq!(
        fixture.classifier.calls(),
        2,
        "the dispatch and the scope message each asked Laya once"
    );
    fixture.team.shutdown().await;
}

/// A routed fix prompt is Mnemo2 restating review findings: Laya never reads
/// it, and the worker's one nudge stays available (design §4.6).
#[tokio::test]
async fn route_fix_prompt_draws_no_drift_nudge() {
    let (fixture, _notices) = hard_reading_nudge_fixture().await;
    let result = fixture.dispatch(Some("routine"), None).await;
    assert!(result.success, "{}", result.output);
    wait_worker_settled(&fixture.team, "routingworker").await;
    wait_route_row(&fixture).await;
    assert_eq!(
        fixture.classifier.calls(),
        1,
        "only the dispatch classified before the fix"
    );
    fixture
        .team
        .review(
            "routingworker",
            "first pass".into(),
            vec![open_finding("finding1")],
            evidence(),
        )
        .await
        .expect("record the review");
    let routed = fixture
        .team
        .route_fix("routingworker", "finding1")
        .await
        .expect("route the finding back to the worker");
    let prompt = routed.inbox.last().expect("the fix prompt reached the inbox");
    assert_eq!(prompt.kind, InboxKind::Fix, "{prompt:?}");
    assert!(
        prompt.text.split_whitespace().count()
            >= crate::mnemo2::dispatch_routing::MIN_WORDS,
        "long enough for the drift check: {}",
        prompt.text
    );
    wait_worker_settled(&fixture.team, "routingworker").await;
    let classified = classified_when_scope_nudges(&fixture).await;
    assert!(
        !classified
            .iter()
            .any(|text| text.starts_with("Fix review finding")),
        "the fix prompt reached the drift check: {classified:#?}"
    );
    assert!(
        classified.contains(&drift_message()),
        "the scope message after the fix is still read: {classified:#?}"
    );
    assert_eq!(
        fixture.classifier.calls(),
        2,
        "the dispatch and the scope message each asked Laya once"
    );
    fixture.team.shutdown().await;
}

/// What reaches the drift check, including the order seen on real read-back
/// workers: Mnemo2's plan-changed notices first, then the lead's answer.
#[test]
fn drift_check_skips_generated_messages_and_the_read_back_answer() {
    let root = tempfile::tempdir().expect("workspace");
    let entry = |sender, kind| InboxEntry {
        sender,
        kind,
        text: "earlier".to_string(),
        at_ms: 1,
    };
    let plain = worker(root.path(), "plainworker1");
    assert!(message_can_show_drift(&plain, MessageSender::ExternalLead));
    assert!(message_can_show_drift(&plain, MessageSender::Lead));
    assert!(message_can_show_drift(&plain, MessageSender::User));
    assert!(
        !message_can_show_drift(&plain, MessageSender::Mnemo),
        "Mnemo2's own text is never read"
    );
    let mut read_back = worker(root.path(), "readback0001");
    read_back.read_back = true;
    for sender in [
        MessageSender::ExternalLead,
        MessageSender::Lead,
        MessageSender::User,
    ] {
        assert!(
            !message_can_show_drift(&read_back, sender),
            "the first reply is the read-back answer: {sender:?}"
        );
    }
    read_back.inbox.push(entry(MessageSender::Mnemo, InboxKind::Steer));
    read_back.inbox.push(entry(MessageSender::Mnemo, InboxKind::Prompt));
    assert!(
        !message_can_show_drift(&read_back, MessageSender::ExternalLead),
        "plan-changed notices before the answer do not count as the answer"
    );
    read_back
        .inbox
        .push(entry(MessageSender::ExternalLead, InboxKind::Steer));
    assert!(
        message_can_show_drift(&read_back, MessageSender::ExternalLead),
        "a lead reply delivered as a steer was the answer, so the next message is read"
    );
    assert!(message_can_show_drift(&read_back, MessageSender::User));
    assert!(!message_can_show_drift(&read_back, MessageSender::Mnemo));
}

/// The deterministic review nudge: a second review round, after the
/// worker's own turn, that still records a high finding nudges the lead to
/// the next tier up, in every mode except off (design §4.6). A lead-only
/// review round never nudges.
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
    // The fixer's completed turn: a round with no worker turn in between is
    // the lead's own review_fix work, and must never escalate.
    team.update_worker(&record.id, |worker| worker.turns += 1)
        .await;
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
        lane: None,
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
        lane: None,
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

/// A revert appends a `reverted` row that pairs with the merge's `outcome`
/// row, which carries the merge commit (plan dac2028d).
#[tokio::test]
async fn revert_merge_appends_a_reverted_row() {
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
    // A reroute keeps the terminal outcome row logged in the default mode.
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
        lane: None,
    });
    let team = open_test_team(root.path(), state.path(), vec![worker.clone()]).await;
    let merged = team.merge(&worker.id).await.expect("merge the worker");
    let merge_commit = merged
        .merge
        .as_ref()
        .expect("merge record")
        .commit
        .clone();
    let reverted = team
        .revert_merge(&worker.id)
        .await
        .expect("revert the merge");
    let revert_commit = reverted
        .merge
        .as_ref()
        .and_then(|merge| merge.revert_commit.clone())
        .expect("the revert commit is recorded");
    let rows: Vec<DispatchRoutingRow> =
        std::fs::read_to_string(state.path().join("dispatch_routing.jsonl"))
            .expect("the routing log")
            .lines()
            .map(|line| serde_json::from_str(line).expect("routing row"))
            .collect();
    assert!(
        matches!(
            rows.as_slice(),
            [
                DispatchRoutingRow::Outcome {
                    merge_commit: Some(row_merge),
                    step: 1,
                    ..
                },
                DispatchRoutingRow::Reverted {
                    worker_id,
                    plan_id,
                    step,
                    revert_commit: row_revert,
                    ..
                }
            ] if worker_id == &worker.id
                && plan_id == "plan0001"
                && *step == 1
                && row_merge == &merge_commit
                && row_revert == &revert_commit
        ),
        "the merge outcome then its revert: {rows:?}"
    );
    team.shutdown().await;
}

/// A project that keeps `.coding/laya` carries its decisions in git: the
/// merge refreshes the bundle with the outcome row it just appended.
#[tokio::test]
async fn a_merge_refreshes_the_project_learnings() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    run_git(root.path(), &["checkout", "-b", "wt/mnemo"]);
    std::fs::create_dir_all(root.path().join(".coding/laya")).expect("the learnings folder");
    std::fs::create_dir_all(root.path().join(".coding/plans")).expect("the plans folder");
    std::fs::write(root.path().join(".coding/plans/plan0001.md"), "# Plan\n").expect("plan file");
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
        lane: None,
    });
    let team = open_test_team(root.path(), state.path(), vec![worker.clone()]).await;
    let merged = team.merge(&worker.id).await.expect("merge the worker");
    assert!(matches!(merged.status, WorkerStatus::Merged), "{merged:?}");
    // The export is best effort and off the merge path, so poll for it.
    let bundle_path = root.path().join(".coding/laya/dispatch_routing.jsonl");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let bundle = std::fs::read_to_string(&bundle_path).unwrap_or_default();
        let exported: Vec<DispatchRoutingRow> = bundle
            .lines()
            .map(|line| serde_json::from_str(line).expect("a bundle row"))
            .collect();
        let exported_outcome = exported.iter().any(|row| {
            matches!(
                row,
                DispatchRoutingRow::Outcome {
                    worker_id,
                    plan_id,
                    status,
                    label: Some(label),
                    ..
                }
                    if worker_id == "00000002000000000002"
                        && plan_id == "plan0001"
                        && status == "merged"
                        && label == "high"
            )
        });
        if exported_outcome {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the merge's outcome row reached the bundle: {bundle:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    team.shutdown().await;
}

/// A project without `.coding/laya` exports nothing: its decisions stay on
/// this machine.
#[tokio::test]
async fn a_merge_without_the_folder_writes_no_learnings() {
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
        lane: None,
    });
    let team = open_test_team(root.path(), state.path(), vec![worker.clone()]).await;
    let merged = team.merge(&worker.id).await.expect("merge the worker");
    assert!(matches!(merged.status, WorkerStatus::Merged), "{merged:?}");
    // Give a stray export time to land before asserting it never happens.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(
        !root.path().join(".coding/laya").exists(),
        "a project without the folder exports nothing"
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
        lane: None,
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
        lane: None,
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

/// The lead's `team_review` schema offers a finding category and its
/// enforcement home, both optional (plan 3c2ec71c step 1).
#[tokio::test]
async fn team_review_schema_offers_finding_category_and_enforce() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let team = open_test_team(root.path(), state.path(), Vec::new()).await;
    let review = team
        .lead_host_tools()
        .into_iter()
        .find(|tool| tool.name == "team_review")
        .expect("team_review");
    let finding = &review.parameters["properties"]["findings"]["items"];
    let properties = &finding["properties"];
    let category = &properties["category"];
    assert_eq!(category["type"], json!("string"), "{category}");
    assert!(
        category["description"]
            .as_str()
            .is_some_and(|text| text.contains("kebab-case")),
        "{category}"
    );
    let enforce = &properties["enforce"];
    assert_eq!(
        enforce["enum"],
        json!(["code", "build", "test", "review"]),
        "{enforce}"
    );
    let required = finding["required"].as_array().expect("required fields");
    assert!(!required.contains(&json!("category")), "category stays optional");
    assert!(!required.contains(&json!("enforce")), "enforce stays optional");
    team.shutdown().await;
}

/// `team_dispatch` offers `review_risk`: a lead that follows the schema can
/// only request the high-risk audit gate if the parameter is declared.
#[tokio::test]
async fn team_dispatch_schema_offers_review_risk() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let team = open_test_team(root.path(), state.path(), Vec::new()).await;
    let tools = team.lead_host_tools();
    let dispatch = tools.iter().find(|tool| tool.name == "team_dispatch").expect("dispatch");
    let review_risk = &dispatch.parameters["properties"]["review_risk"];
    assert_eq!(review_risk["type"], json!("string"), "{review_risk}");
    assert_eq!(review_risk["enum"], json!(["standard", "high"]), "{review_risk}");
    assert!(
        review_risk["description"].as_str().is_some_and(|text| !text.is_empty()),
        "{review_risk}"
    );
    let required = dispatch.parameters["required"].as_array().expect("required fields");
    assert!(!required.contains(&json!("review_risk")), "review_risk stays optional");
    team.shutdown().await;
}

/// Every argument the `team_dispatch` handler reads must be declared in its
/// schema, or a lead that follows the schema can never send it.
#[tokio::test]
async fn team_dispatch_schema_declares_every_argument_its_handler_reads() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let team = open_test_team(root.path(), state.path(), Vec::new()).await;
    let tools = team.lead_host_tools();
    let dispatch = tools.iter().find(|tool| tool.name == "team_dispatch").expect("dispatch");
    let declared = dispatch.parameters["properties"].as_object().expect("properties");
    let source = include_str!("lead_tools.rs");
    let start = source
        .find("if name == \"team_dispatch\" {")
        .expect("the team_dispatch handler");
    let end = source
        .find("if name == \"team_audit_dispatch\" {")
        .expect("the handler after team_dispatch");
    let read: Vec<&str> = source[start..end]
        .split(".get(\"")
        .skip(1)
        .map(|rest| rest.split('"').next().expect("an argument name"))
        .collect();
    // A renamed accessor must not turn this into a scan that finds nothing.
    assert!(read.contains(&"review_risk"), "the scan sees the handler's reads: {read:?}");
    assert!(read.contains(&"worker_id"), "the scan sees the handler's reads: {read:?}");
    let undeclared: Vec<&str> = read
        .iter()
        .copied()
        .filter(|arg| !declared.contains_key(*arg))
        .collect();
    assert!(undeclared.is_empty(), "read but not declared: {undeclared:?}");
    team.shutdown().await;
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
            sender: MessageSender::User,
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
    // Auto mode applies a pending nudge on the first idle event, so the pump
    // may claim it while the ledger save yields; either pass performs the same
    // switch, so assert the outcome rather than which caller ran it.
    let _ = fixture
        .team
        .auto_escalate_worker("routingworker")
        .await
        .expect("auto escalation");
    let switched = eventually(|| async {
        let worker = fixture.team.worker("routingworker").await.expect("worker");
        (worker.model.as_deref() == Some("test/scripted-difficult")).then_some(worker)
    })
    .await;
    assert_eq!(switched.model.as_deref(), Some("test/scripted-difficult"));
    assert_eq!(switched.tier.as_deref(), Some("high"));
    assert!(switched.routing.expect("routing").nudged.is_none());
    let _ = eventually(|| async {
        let raw = std::fs::read_to_string(fixture.log_path()).expect("routing log");
        raw.lines()
            .any(|line| {
                serde_json::from_str::<Value>(line).is_ok_and(|row| {
                    row["event"] == "lead_reroute" && row["source"] == "escalation"
                })
            })
            .then_some(raw)
    })
    .await;
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

    // An unknown name is an error naming the real capabilities, never a
    // success that reads as "nothing to see".
    let missing = worker_host_tool_result(
        &messages,
        "planreader",
        "plan_read",
        &json!({"capability": "not-a-capability"}),
        Some(&board),
        None,
    )
    .await;
    assert!(!missing.success, "{}", missing.output);
    assert_eq!(
        missing.output,
        "There is no OpenSpec capability named 'not-a-capability'. Known capabilities: tier-ladder. To re-read your plan, call plan_read with no arguments."
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

/// Regression: an unknown capability name must fail with the list of real
/// capabilities and directions to call plan_read without a capability.
/// Bug root cause: PlanBoard::capability_spec returns success("No current
/// spec for '<x>' yet.") even when '<x>' is not a known capability, so
/// workers loop indefinitely on invented names (2026-11-16, oneRun01).
#[tokio::test]
async fn plan_read_with_an_unknown_capability_names_the_real_ones() {
    let root = tempfile::tempdir().expect("workspace");
    init_git(root.path());

    // Create a known capability so we have real names to list.
    std::fs::create_dir_all(root.path().join(".coding/openspec/specs/tier-ladder"))
        .expect("create tier-ladder dir");
    std::fs::write(
        root.path().join(".coding/openspec/specs/tier-ladder/spec.md"),
        "# Tier ladder\n",
    )
    .expect("spec file");

    let board = PlanBoard::new(
        root.path().join(".coding/plans"),
        root.path().join(".coding/reviews"),
    );
    let board = Arc::new(board);
    let messages = Arc::new(Mutex::new(Vec::new()));

    // Calling with an unknown capability MUST return failure, listing the
    // real capabilities and telling the worker to call plan_read without args.
    let unknown = worker_host_tool_result(
        &messages,
        "planreader",
        "plan_read",
        &json!({"capability": "worker-checks-current-plan"}),
        Some(&board),
        None,
    )
    .await;

    // THIS FAILS ON TODAY'S CODE: it returns success == true with
    // "No current spec for 'worker-checks-current-plan' yet."
    assert!(
        !unknown.success,
        "expected failure for unknown capability, got success: {}",
        unknown.output
    );
    assert!(
        unknown.output.contains("Known capabilities: tier-ladder"),
        "expected 'Known capabilities: tier-ladder', got: {}",
        unknown.output
    );
    assert!(
        unknown.output.contains("call plan_read with no arguments"),
        "expected direction to call plan_read without args, got: {}",
        unknown.output
    );
}

/// A real capability folder whose spec.md does not exist yet stays a success
/// answer, and it too points the worker back at its plan.
#[tokio::test]
async fn plan_read_known_capability_without_a_spec_points_back_at_the_plan() {
    let root = tempfile::tempdir().expect("workspace");
    init_git(root.path());
    std::fs::create_dir_all(root.path().join(".coding/openspec/specs/empty-capability"))
        .expect("capability dir");
    let board = Arc::new(PlanBoard::new(
        root.path().join(".coding/plans"),
        root.path().join(".coding/reviews"),
    ));
    let messages = Arc::new(Mutex::new(Vec::new()));
    let current = worker_host_tool_result(
        &messages,
        "planreader",
        "plan_read",
        &json!({"capability": "empty-capability"}),
        Some(&board),
        None,
    )
    .await;
    assert!(current.success, "{}", current.output);
    assert_eq!(
        current.output,
        "No current spec for 'empty-capability' yet. To re-read your plan, call plan_read with no arguments."
    );

    // No capability folders at all: the error says "none" rather than an
    // empty list.
    let bare_root = tempfile::tempdir().expect("bare workspace");
    init_git(bare_root.path());
    let bare_board = Arc::new(PlanBoard::new(
        bare_root.path().join(".coding/plans"),
        bare_root.path().join(".coding/reviews"),
    ));
    let none = worker_host_tool_result(
        &messages,
        "planreader",
        "plan_read",
        &json!({"capability": "invented"}),
        Some(&bare_board),
        None,
    )
    .await;
    assert!(!none.success, "{}", none.output);
    assert_eq!(
        none.output,
        "There is no OpenSpec capability named 'invented'. Known capabilities: none. To re-read your plan, call plan_read with no arguments."
    );
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

/// `session_search` is offered to the lead only and answers from the real
/// per-worker transcript files under the state directory.
#[tokio::test]
async fn session_search_is_a_lead_tool_and_answers() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let team = open_test_team(root.path(), state.path(), Vec::new()).await;

    assert!(
        team.lead_host_tools()
            .iter()
            .any(|tool| tool.name == "session_search"),
        "the lead is offered session_search"
    );
    assert!(
        !team
            .worker_host_tools()
            .iter()
            .any(|tool| tool.name == "session_search"),
        "a worker is not offered session_search"
    );

    std::fs::create_dir_all(state.path().join("transcripts")).expect("transcripts dir");
    std::fs::write(
        state.path().join("transcripts").join("agent-abc.json"),
        json!({
            "messages": [
                {"role": "system", "content": "You are a coding agent."},
                {"role": "user", "content": "the flux capacitor test flaked twice"}
            ]
        })
        .to_string(),
    )
    .expect("write the transcript");

    let found = team
        .handle_lead_host_tool("session_search", json!({"query": "flux capacitor"}))
        .await;
    assert!(found.success, "{}", found.output);
    assert!(found.output.starts_with("1 matches"), "{}", found.output);
    assert!(found.output.contains("agent-abc #1 user:"), "{}", found.output);
    assert!(found.output.contains("flaked"), "{}", found.output);

    let one = team
        .handle_lead_host_tool("session_search", json!({"worker": "agent-abc", "seq": 1}))
        .await;
    assert!(one.success, "{}", one.output);
    assert!(one.output.starts_with("agent-abc #1 user"), "{}", one.output);
    assert!(one.output.contains("flaked"), "{}", one.output);

    team.shutdown().await;
}

/// An open high finding on the file the test worker checkout seeds, so a
/// `review_fix` can edit exactly the finding's source file.
fn readme_finding(id: &str) -> Finding {
    Finding {
        file: "README.md".to_string(),
        line: 1,
        ..open_finding(id)
    }
}

/// A review fixture whose worker checkout holds `content` as a committed,
/// clean `README.md`, with the review of that tip already recorded — the
/// state `review_fix` needs before it may edit.
async fn review_fix_fixture(id: &str, content: &str) -> ReviewFixture {
    let fixture = review_fixture(RoutingMode::Off, id).await;
    let path = fixture.record.worktree.join("README.md");
    std::fs::write(&path, content).expect("seed README");
    run_git(&fixture.record.worktree, &["add", "README.md"]);
    run_git(&fixture.record.worktree, &["commit", "-m", "seed README"]);
    fixture
        .team
        .review(
            &fixture.record.id,
            "first pass".into(),
            vec![readme_finding("finding1")],
            evidence(),
        )
        .await
        .expect("review of the seeded tip");
    fixture
}

/// A record-based team with one routine-tier worker and no live session, for
/// the repeated-finding escalation: nothing but the test itself can bump the
/// worker's turn count.
async fn repeated_finding_fixture(mode: RoutingMode, id: &str) -> ReviewFixture {
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
    .expect("save routing settings");
    let mut record = worker(root.path(), id);
    record.tier = Some("routine".to_string());
    record.model = Some("test/scripted-simple".to_string());
    record.routing = Some(WorkerRouting {
        laya_tier: Some("routine".to_string()),
        confidence: Some(0.62),
        cleared: true,
        lead_tier: Some("routine".to_string()),
        routed_by: "lead".to_string(),
        ran_tier: Some("routine".to_string()),
        nudged: None,
    });
    provision_agent_worktree(
        root.path().to_path_buf(),
        record.id.clone(),
        "main".to_string(),
    )
    .await
    .expect("provision the worker checkout");
    let team =
        open_test_team_with_config(root.path(), state.path(), vec![record.clone()], config.path())
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
    ReviewFixture {
        _root: root,
        state,
        _config: config,
        team,
        record,
    }
}

/// A lead `review_fix` round is the lead's own edit, not a failed worker
/// round: with no worker turn between the two reviews, a finding that is
/// still open must not escalate, and Auto mode must leave the worker's tier
/// and model alone (bug report 2026-11-15 on worker lightDocs01).
#[tokio::test]
async fn lead_review_fix_rounds_do_not_escalate() {
    let fixture = repeated_finding_fixture(RoutingMode::Auto, "reviewfix001").await;
    let team = &fixture.team;
    let first = team
        .review(
            &fixture.record.id,
            "first pass".into(),
            vec![readme_finding("finding1"), open_finding("finding2")],
            evidence(),
        )
        .await
        .expect("first review");
    assert_eq!(first.review_rounds, 1, "{first:?}");
    let fixed = team
        .review_fix(&fixture.record.id, "finding1", "README.md", "seed", "fixed")
        .await
        .expect("the lead fixes the wording finding itself");
    assert_eq!(fixed.fix_rounds, 1, "{fixed:?}");
    assert_eq!(fixed.turns, 0, "review_fix never runs a worker turn");
    let second = team
        .review(
            &fixture.record.id,
            "second pass".into(),
            vec![open_finding("finding2")],
            evidence(),
        )
        .await
        .expect("second review");
    assert_eq!(second.review_rounds, 2, "{second:?}");
    assert!(
        second
            .routing
            .as_ref()
            .expect("routed worker")
            .nudged
            .is_none(),
        "a lead-only review round never nudges: {second:?}"
    );
    assert_eq!(second.tier.as_deref(), Some("routine"), "{second:?}");
    assert_eq!(
        second.model.as_deref(),
        Some("test/scripted-simple"),
        "{second:?}"
    );
    assert!(
        !fixture
            .rows()
            .iter()
            .any(|row| matches!(row, DispatchRoutingRow::Nudge { .. })),
        "no nudge row: {:?}",
        fixture.rows()
    );
    assert!(
        !fixture
            .rows()
            .iter()
            .any(|row| matches!(row, DispatchRoutingRow::LeadReroute { .. })),
        "no auto switch: {:?}",
        fixture.rows()
    );
    team.shutdown().await;
}

/// Regression (plan f89fd63d step 1): `review_fix` matched `old_string`
/// byte-exactly, so an LF old_string missed in a CRLF file. It now matches
/// through the worker edit tool's matcher and writes the replacement in the
/// file's own ending style.
#[tokio::test]
async fn review_fix_matches_lf_old_string_in_crlf_file() {
    let fixture = review_fix_fixture("crlffix00001", "a\r\nb\r\nc\r\n").await;
    let fixed = fixture
        .team
        .review_fix(&fixture.record.id, "finding1", "README.md", "a\nb", "x\ny")
        .await
        .expect("an LF old_string matches the CRLF file");
    assert_eq!(fixed.fix_rounds, 1, "{fixed:?}");
    assert_eq!(
        std::fs::read_to_string(fixture.record.worktree.join("README.md")).expect("README"),
        "x\r\ny\r\nc\r\n"
    );
    fixture.team.shutdown().await;
}

/// The replacement is re-emitted in the file's own endings: lines the
/// new_string adds stay CRLF in a CRLF file.
#[tokio::test]
async fn review_fix_keeps_crlf_endings_in_new_lines() {
    let fixture = review_fix_fixture("crlfkeep0001", "a\r\nb\r\nc\r\n").await;
    fixture
        .team
        .review_fix(
            &fixture.record.id,
            "finding1",
            "README.md",
            "a\nb",
            "x\ny\nz",
        )
        .await
        .expect("the CRLF file takes the LF replacement");
    let text = std::fs::read_to_string(fixture.record.worktree.join("README.md")).expect("README");
    assert_eq!(text, "x\r\ny\r\nz\r\nc\r\n");
    assert!(
        text.match_indices('\n')
            .all(|(i, _)| i > 0 && text.as_bytes()[i - 1] == b'\r'),
        "no bare LF may survive: {text:?}"
    );
    fixture.team.shutdown().await;
}

/// An ambiguous anchor is still refused, with the historical message and no
/// write.
#[tokio::test]
async fn review_fix_still_refuses_ambiguous_old_string() {
    let fixture = review_fix_fixture("crlfambig001", "a\r\nb\r\na\r\nb\r\nc\r\n").await;
    let err = fixture
        .team
        .review_fix(&fixture.record.id, "finding1", "README.md", "a\nb", "x")
        .await
        .expect_err("two occurrences must be refused");
    assert_eq!(err, "review_fix old_string must occur exactly once");
    assert_eq!(
        std::fs::read_to_string(fixture.record.worktree.join("README.md")).expect("README"),
        "a\r\nb\r\na\r\nb\r\nc\r\n"
    );
    fixture.team.shutdown().await;
}

/// An LF file keeps its historical behaviour: the LF replacement lands
/// verbatim and the fix round is recorded.
#[tokio::test]
async fn review_fix_lf_file_unchanged_behavior() {
    let fixture = review_fix_fixture("crlflf000001", "a\nb\nc\n").await;
    let fixed = fixture
        .team
        .review_fix(&fixture.record.id, "finding1", "README.md", "a\nb", "x\ny")
        .await
        .expect("the LF file edits as before");
    assert_eq!(fixed.fix_rounds, 1, "{fixed:?}");
    assert_eq!(
        std::fs::read_to_string(fixture.record.worktree.join("README.md")).expect("README"),
        "x\ny\nc\n"
    );
    fixture.team.shutdown().await;
}

/// A miss keeps the historical message and appends the matcher's near-miss
/// detail — without the worker-facing fresh-read nudge, which steers a worker
/// turn, not the lead.
#[tokio::test]
async fn review_fix_reports_near_miss_without_the_read_nudge() {
    let fixture =
        review_fix_fixture("crlfmiss0001", "alpha\r\nbeta\r\ngamma\r\ndelta\r\n").await;
    let err = fixture
        .team
        .review_fix(
            &fixture.record.id,
            "finding1",
            "README.md",
            "alpha\nbeta\ngamma\ndelto",
            "x",
        )
        .await
        .expect_err("an absent anchor must be refused");
    assert!(
        err.starts_with("review_fix old_string must occur exactly once — near-miss"),
        "{err}"
    );
    assert!(!err.contains("Re-read the file"), "{err}");
    assert_eq!(
        std::fs::read_to_string(fixture.record.worktree.join("README.md")).expect("README"),
        "alpha\r\nbeta\r\ngamma\r\ndelta\r\n"
    );
    fixture.team.shutdown().await;
}

/// The repeated-finding escalation still fires when the worker really did
/// take a turn between the two reviews and left the high finding open.
#[tokio::test]
async fn finding_open_after_a_worker_turn_still_escalates() {
    let fixture = repeated_finding_fixture(RoutingMode::Shadow, "reviewturn01").await;
    let team = &fixture.team;
    team.review(
        &fixture.record.id,
        "first pass".into(),
        vec![open_finding("finding1")],
        evidence(),
    )
    .await
    .expect("first review");
    // A finished worker turn, exactly as the pump records one.
    team.update_worker(&fixture.record.id, |worker| worker.turns += 1)
        .await;
    let second = team
        .review(
            &fixture.record.id,
            "second pass".into(),
            vec![open_finding("finding1")],
            evidence(),
        )
        .await
        .expect("second review");
    assert_eq!(second.review_rounds, 2, "{second:?}");
    assert_eq!(second.turns, 1, "{second:?}");
    assert_eq!(
        second
            .routing
            .as_ref()
            .and_then(|routing| routing.nudged.as_deref()),
        Some("high"),
        "{second:?}"
    );
    assert!(
        fixture.rows().iter().any(|row| matches!(
            row,
            DispatchRoutingRow::Nudge { reason, to_tier, .. }
                if reason == "review" && to_tier == "high"
        )),
        "a review nudge row: {:?}",
        fixture.rows()
    );
    team.shutdown().await;
}

/// A record persisted before the review turn count existed decodes with the
/// unknown value, and the repeat rule never fires on that unknown — even
/// when the worker has since taken a turn.
#[tokio::test]
async fn legacy_review_rounds_without_a_turn_count_never_escalate() {
    let fixture = repeated_finding_fixture(RoutingMode::Auto, "reviewlegacy1").await;
    let team = &fixture.team;
    team.review(
        &fixture.record.id,
        "first pass".into(),
        vec![open_finding("finding1")],
        evidence(),
    )
    .await
    .expect("first review");
    let stored = team.worker(&fixture.record.id).await.expect("stored worker");
    let mut legacy = serde_json::to_value(&stored).expect("the record as json");
    assert!(
        legacy
            .as_object_mut()
            .expect("record object")
            .remove("review_turns")
            .is_some(),
        "a recorded review carries its turn count"
    );
    let decoded: WorkerRecord =
        serde_json::from_value(legacy).expect("a record written before the field decodes");
    assert_eq!(decoded.review_rounds, 1, "{decoded:?}");
    assert_eq!(decoded.review_turns, None, "a missing turn count is unknown");
    // Back to the legacy shape, with a worker turn since: unknown must never
    // escalate on the repeat rule.
    team.update_worker(&fixture.record.id, |worker| {
        worker.review_turns = None;
        worker.turns += 1;
    })
    .await;
    let second = team
        .review(
            &fixture.record.id,
            "second pass".into(),
            vec![open_finding("finding1")],
            evidence(),
        )
        .await
        .expect("second review");
    assert_eq!(second.review_rounds, 2, "{second:?}");
    assert!(
        second
            .routing
            .as_ref()
            .expect("routed worker")
            .nudged
            .is_none(),
        "an unknown turn count never escalates: {second:?}"
    );
    assert_eq!(second.tier.as_deref(), Some("routine"), "{second:?}");
    assert_eq!(
        second.model.as_deref(),
        Some("test/scripted-simple"),
        "{second:?}"
    );
    team.shutdown().await;
}


async fn provision_pool_worker(team: &TeamRuntime, id: &str) -> WorkerRecord {
    team.provision_worker(
        "pool test worker".to_string(),
        None,
        None,
        None,
        Some(id.to_string()),
        "standard".to_string(),
        None,
        None,
    )
    .await
    .expect("provision pool worker")
}

async fn make_worker_reviewable(team: &TeamRuntime, worker: &WorkerRecord) {
    let file = format!("merged-{}.txt", worker.id);
    std::fs::write(worker.worktree.join(&file), "merged\n").expect("worker change");
    run_git(&worker.worktree, &["add", &file]);
    run_git(&worker.worktree, &["commit", "-m", "worker change"]);
    let tip = phase3::tip(&worker.worktree).await.expect("worker tip");
    team.update_worker(&worker.id, |current| {
        current.status = WorkerStatus::InReview;
        current.checks = vec![WorkerCheck {
            name: "cargo test --workspace".into(),
            status: "passed".into(),
            detail: None,
            tip: tip.clone(),
        }];
        current.review = Some(ReviewRecord {
            summary: "reviewed".into(),
            tip: tip.clone(),
            reviewed_at_ms: 1,
            evidence: Some(evidence()),
            lane: None,
        });
    })
    .await;
}

#[tokio::test]
async fn two_live_workers_get_different_pool_checkouts() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let team = open_test_team(root.path(), state.path(), Vec::new()).await;

    let first = provision_pool_worker(&team, "poola0001").await;
    let second = provision_pool_worker(&team, "poolb0001").await;
    let third = provision_pool_worker(&team, "poolc0001").await;

    for (worker, name) in [(&first, "pool-1"), (&second, "pool-2"), (&third, "pool-3")] {
        assert_eq!(worker.worktree.file_name().and_then(|name| name.to_str()), Some(name));
        assert!(worker.worktree.join(".git").is_file());
    }
    team.shutdown().await;
}

#[tokio::test]
async fn concurrent_dispatches_never_share_a_pool() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let team = open_test_team(root.path(), state.path(), Vec::new()).await;

    let (first, second) = tokio::join!(
        provision_pool_worker(&team, "conca0001"),
        provision_pool_worker(&team, "concb0001"),
    );

    assert_ne!(first.worktree, second.worktree);
    assert!(first.worktree.join(".git").is_file());
    assert!(second.worktree.join(".git").is_file());
    team.shutdown().await;
}


#[tokio::test]
async fn a_merged_workers_pool_is_reused_by_the_next_dispatch() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    run_git(root.path(), &["checkout", "-b", "wt/mnemo"]);
    std::fs::write(root.path().join(".git/info/exclude"), "target/\nnode_modules/\n")
        .expect("ignore build caches");
    let team = open_test_team(root.path(), state.path(), Vec::new()).await;
    let first = provision_pool_worker(&team, "mergea0001").await;
    let pool = first.worktree.clone();
    std::fs::create_dir_all(pool.join("target")).expect("target cache");
    std::fs::write(pool.join("target/warm.txt"), "warm\n").expect("warm cache");
    std::fs::create_dir_all(pool.join(".coding/tmp")).expect("leftover dir");
    std::fs::write(pool.join(".coding/tmp/stale.txt"), "stale\n").expect("ignored leftover");
    make_worker_reviewable(&team, &first).await;

    let merged = team.merge(&first.id).await.expect("merge pool worker");
    assert!(merged.worktree.as_os_str().is_empty());
    assert!(pool.exists());
    let detached = phase3::git(&pool, &["rev-parse", "--abbrev-ref", "HEAD"])
        .await
        .expect("detached pool HEAD");
    assert!(detached.status.success());
    assert_eq!(String::from_utf8_lossy(&detached.stdout).trim(), "HEAD");
    assert_eq!(
        std::fs::read_to_string(pool.join("target/warm.txt")).expect("cache survives"),
        "warm\n"
    );
    let branches = phase3::git(root.path(), &["branch", "--list", &first.branch])
        .await
        .expect("list worker branch");
    assert!(branches.status.success());
    assert!(String::from_utf8_lossy(&branches.stdout).trim().is_empty());

    let mut saved = TeamPersistence::new(state.path())
        .load()
        .expect("load merged ledger")
        .expect("saved ledger");
    assert!(
        saved
            .workers
            .iter()
            .find(|worker| worker.id == first.id)
            .expect("merged record on disk")
            .worktree
            .as_os_str()
            .is_empty(),
        "clearing the pool path is persisted before the next operation"
    );
    team.shutdown().await;

    // Simulate the crash window fixed by startup repair: the checkout is
    // detached but the old ledger still says this Merged record holds it.
    saved
        .workers
        .iter_mut()
        .find(|worker| worker.id == first.id)
        .expect("merged record")
        .worktree = pool.clone();
    TeamPersistence::new(state.path())
        .save(&saved)
        .expect("save stale pool record");
    let team = open_test_team(root.path(), state.path(), saved.workers.clone()).await;
    assert!(
        team.worker(&first.id)
            .await
            .expect("reloaded merged worker")
            .worktree
            .as_os_str()
            .is_empty(),
        "startup repairs the detached pool record"
    );
    let repaired = TeamPersistence::new(state.path())
        .load()
        .expect("load repaired ledger")
        .expect("repaired ledger");
    assert!(
        repaired.workers[0].worktree.as_os_str().is_empty(),
        "startup repair is persisted"
    );

    let second = provision_pool_worker(&team, "mergeb0001").await;
    assert_eq!(second.worktree, pool);
    assert_eq!(second.branch, agent_branch(&second.id));
    assert_eq!(
        std::fs::read_to_string(pool.join("target/warm.txt")).expect("cache remains warm"),
        "warm\n"
    );
    assert!(!pool.join(".coding/tmp/stale.txt").exists());
    assert_eq!(
        mnemo::app::worktrees::current_branch(&pool).expect("reused pool branch"),
        second.branch
    );
    team.shutdown().await;
}

#[tokio::test]
async fn deleting_a_failed_worker_discards_and_frees_its_pool() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    std::fs::write(root.path().join(".git/info/exclude"), "target/\n")
        .expect("ignore build cache");
    let team = open_test_team(root.path(), state.path(), Vec::new()).await;
    let failed = provision_pool_worker(&team, "deletea001").await;
    team.update_worker(&failed.id, |worker| worker.status = WorkerStatus::Failed)
        .await;
    std::fs::write(failed.worktree.join("README.md"), "dirty\n").expect("dirty checkout");
    std::fs::write(failed.worktree.join("untracked.txt"), "discard\n")
        .expect("untracked change");
    std::fs::create_dir_all(failed.worktree.join("target")).expect("target cache");
    std::fs::write(failed.worktree.join("target/warm.txt"), "warm\n")
        .expect("warm cache");

    let preview = team
        .deletion_preview(std::slice::from_ref(&failed.id))
        .await
        .expect("pool deletion preview");
    assert!(preview[0].pool);
    assert!(preview[0].checkout);
    assert!(preview[0].uncommitted);
    team.delete_workers(std::slice::from_ref(&failed.id), true)
        .await
        .expect("delete failed worker");
    assert!(failed.worktree.exists());
    assert_eq!(
        std::fs::read_to_string(failed.worktree.join("README.md"))
            .expect("reset tracked file")
            .trim(),
        "seed"
    );
    assert!(!failed.worktree.join("untracked.txt").exists());
    assert!(failed.worktree.join("target/warm.txt").exists());

    let next = provision_pool_worker(&team, "deleteb001").await;
    assert_eq!(next.worktree, failed.worktree);
    team.shutdown().await;
}


#[tokio::test]
async fn legacy_agent_checkouts_still_merge_and_delete() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    run_git(root.path(), &["checkout", "-b", "wt/mnemo"]);
    let mergeable = worker(root.path(), "legacya0001");
    let failed = worker(root.path(), "legacyb0001");
    for worker in [&mergeable, &failed] {
        run_git(
            root.path(),
            &[
                "worktree",
                "add",
                "-b",
                &worker.branch,
                worker.worktree.to_str().expect("checkout path"),
                "wt/mnemo",
            ],
        );
    }
    let team = open_test_team(
        root.path(),
        state.path(),
        vec![mergeable.clone(), failed.clone()],
    )
    .await;
    make_worker_reviewable(&team, &mergeable).await;
    team.update_worker(&failed.id, |worker| worker.status = WorkerStatus::Failed)
        .await;

    team.merge(&mergeable.id).await.expect("merge legacy worker");
    team.delete_workers(std::slice::from_ref(&failed.id), false)
        .await
        .expect("delete legacy worker");

    assert!(!mergeable.worktree.exists());
    assert!(!failed.worktree.exists());
    team.shutdown().await;
}


#[tokio::test]
async fn a_dirty_free_pool_is_never_reused() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let dirty = root.path().join(".worktrees/pool-1");
    run_git(
        root.path(),
        &[
            "worktree",
            "add",
            "--detach",
            dirty.to_str().expect("dirty pool path"),
            "main",
        ],
    );
    std::fs::write(dirty.join("uncommitted.txt"), "keep\n").expect("dirty pool change");
    let team = open_test_team(root.path(), state.path(), Vec::new()).await;

    let worker = provision_pool_worker(&team, "dirtya0001").await;

    assert_eq!(worker.worktree.file_name().and_then(|name| name.to_str()), Some("pool-2"));
    assert_eq!(
        std::fs::read_to_string(dirty.join("uncommitted.txt")).expect("dirty pool is untouched"),
        "keep\n"
    );
    team.shutdown().await;
}

#[tokio::test]
async fn a_pool_held_by_another_record_is_blocked_in_preview_and_deletion() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let team = open_test_team(root.path(), state.path(), Vec::new()).await;
    let first = provision_pool_worker(&team, "shareda01").await;
    team.update_worker(&first.id, |worker| worker.status = WorkerStatus::Failed)
        .await;
    let mut second = first.clone();
    second.id = "sharedb01".to_string();
    second.name = second.id.clone();
    second.branch = agent_branch(&second.id);
    team.records.lock().await.push(second);

    let preview = team
        .deletion_preview(std::slice::from_ref(&first.id))
        .await
        .expect("pool deletion preview");
    let blocked = preview[0].blocked.as_deref().expect("preview is blocked");
    assert!(blocked.contains("another worker record holds"), "{blocked}");

    let outcome = team
        .delete_workers(std::slice::from_ref(&first.id), true)
        .await
        .expect("the batch reports the guard failure");
    let error = outcome.results[0].error.as_deref().expect("deletion refused");
    assert!(error.contains("another worker record holds"), "{error}");
    assert!(team.worker(&first.id).await.is_ok());
    assert!(branch_exists(root.path(), &first.branch).await);
    assert_eq!(
        mnemo::app::worktrees::current_branch(&first.worktree).expect("unchanged pool branch"),
        first.branch
    );
    team.shutdown().await;
}


fn assert_saved_worktree_empty(state: &std::path::Path, worker_id: &str) {
    let saved = TeamPersistence::new(state)
        .load()
        .expect("load saved team")
        .expect("saved team");
    let worker = saved
        .workers
        .iter()
        .find(|worker| worker.id == worker_id)
        .expect("saved worker");
    assert!(worker.worktree.as_os_str().is_empty(), "saved worktree for {worker_id}");
}

#[tokio::test]
async fn retry_after_a_lost_detach_save_persists_and_reuses_the_pool() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let mut stale = worker(root.path(), "retrysave01");
    let pool = root.path().join(".worktrees/pool-1");
    run_git(
        root.path(),
        &[
            "worktree",
            "add",
            "-b",
            &stale.branch,
            pool.to_str().expect("pool path"),
            "main",
        ],
    );
    release_pool_checkout(root.path().to_path_buf(), pool.clone(), stale.branch.clone())
        .await
        .expect("detach before the lost save");
    stale.worktree = pool.clone();
    stale.status = WorkerStatus::Merged;
    stale.error = Some(format!("{WORKTREE_CLEANUP_ERROR}: simulated lost save"));

    let team = open_test_team(root.path(), state.path(), vec![stale.clone()]).await;

    assert!(team.worker(&stale.id).await.expect("retried worker").worktree.as_os_str().is_empty());
    assert_saved_worktree_empty(state.path(), &stale.id);
    let next = provision_pool_worker(&team, "retrynext01").await;
    assert_eq!(next.worktree, pool);
    team.shutdown().await;
}


#[tokio::test]
async fn merging_recovery_after_a_lost_detach_save_persists_and_reuses_the_pool() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    run_git(root.path(), &["checkout", "-b", "wt/mnemo"]);
    let mut stale = worker(root.path(), "recovsave01");
    let pool = root.path().join(".worktrees/pool-1");
    run_git(
        root.path(),
        &[
            "worktree",
            "add",
            "-b",
            &stale.branch,
            pool.to_str().expect("pool path"),
            "wt/mnemo",
        ],
    );
    std::fs::write(pool.join("merged.txt"), "merged\n").expect("worker change");
    run_git(&pool, &["add", "merged.txt"]);
    run_git(&pool, &["commit", "-m", "worker change"]);
    let tip = phase3::tip(&pool).await.expect("worker tip");
    run_git(
        root.path(),
        &["merge", "--no-ff", &stale.branch, "-m", "merge worker"],
    );
    release_pool_checkout(root.path().to_path_buf(), pool.clone(), stale.branch.clone())
        .await
        .expect("detach before the lost save");
    stale.worktree = pool.clone();
    stale.status = WorkerStatus::Merging;
    stale.review = Some(ReviewRecord {
        summary: "reviewed before recovery".into(),
        tip: tip.clone(),
        reviewed_at_ms: 1,
        evidence: Some(evidence()),
        lane: None,
    });
    stale.checks = vec![WorkerCheck {
        name: "cargo test --workspace".into(),
        status: "passed".into(),
        detail: None,
        tip,
    }];

    let team = open_test_team(root.path(), state.path(), vec![stale.clone()]).await;

    let recovered = team.worker(&stale.id).await.expect("recovered worker");
    assert_eq!(recovered.status, WorkerStatus::Merged);
    assert!(recovered.worktree.as_os_str().is_empty());
    assert_saved_worktree_empty(state.path(), &stale.id);
    let next = provision_pool_worker(&team, "recovnext01").await;
    assert_eq!(next.worktree, pool);
    team.shutdown().await;
}


#[tokio::test]
async fn workspace_cleanup_after_a_lost_detach_save_persists_and_reuses_the_pool() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let mut stale = worker(root.path(), "cleansave01");
    let pool = root.path().join(".worktrees/pool-1");
    run_git(
        root.path(),
        &[
            "worktree",
            "add",
            "-b",
            &stale.branch,
            pool.to_str().expect("pool path"),
            "main",
        ],
    );
    release_pool_checkout(root.path().to_path_buf(), pool.clone(), stale.branch.clone())
        .await
        .expect("detach before the lost save");
    stale.worktree = pool.clone();
    stale.status = WorkerStatus::Stopped;
    let persistence = TeamPersistence::new(state.path());
    persistence
        .save(&PersistedTeam {
            workers: vec![stale.clone()],
            slots: Some(2),
            usage: ProjectUsage::default(),
        })
        .expect("save stale cleanup record");

    let report = cleanup_workspace(
        root.path(),
        state.path(),
        &state.path().join("dispatch_routing.jsonl"),
        &[],
    )
    .await
    .expect("retry workspace cleanup");

    assert_eq!(report.removed, vec![stale.branch.clone()]);
    assert_saved_worktree_empty(state.path(), &stale.id);
    let saved = persistence.load().expect("load cleaned team").expect("cleaned team");
    let team = open_test_team(root.path(), state.path(), saved.workers).await;
    let next = provision_pool_worker(&team, "cleannext01").await;
    assert_eq!(next.worktree, pool);
    team.shutdown().await;
}


#[tokio::test]
async fn a_dirty_detached_pool_keeps_its_merged_record() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let pool = root.path().join(".worktrees/pool-1");
    run_git(
        root.path(),
        &[
            "worktree",
            "add",
            "--detach",
            pool.to_str().expect("pool path"),
            "main",
        ],
    );
    std::fs::write(pool.join("dirty.txt"), "keep\n").expect("dirty detached pool");
    let mut stale = worker(root.path(), "dirtyrepair1");
    stale.worktree = pool.clone();
    stale.status = WorkerStatus::Merged;

    let team = open_test_team(root.path(), state.path(), vec![stale.clone()]).await;

    assert_eq!(team.worker(&stale.id).await.expect("dirty worker").worktree, pool);
    let saved = TeamPersistence::new(state.path())
        .load()
        .expect("load dirty team")
        .expect("dirty team");
    assert_eq!(saved.workers[0].worktree, pool);
    team.shutdown().await;
}

#[tokio::test]
async fn a_missing_held_pool_still_reserves_its_index() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    std::fs::create_dir_all(root.path().join(".worktrees/pool-1")).expect("pool-1 folder");
    let mut held = worker(root.path(), "heldpool01");
    held.worktree = root.path().join(".worktrees/pool-2");
    held.status = WorkerStatus::Failed;
    assert!(!held.worktree.exists());
    let team = open_test_team(root.path(), state.path(), vec![held.clone()]).await;

    let next = provision_pool_worker(&team, "nextpool01").await;

    assert_eq!(next.worktree, root.path().join(".worktrees/pool-3"));
    assert!(!held.worktree.exists(), "the held missing pool is not recreated");
    team.shutdown().await;
}


async fn assert_detached_pool_can_be_deleted(dirty: bool) {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let team = open_test_team(root.path(), state.path(), Vec::new()).await;
    let worker_id = if dirty { "detachdirty1" } else { "detachclean1" };
    let failed = provision_pool_worker(&team, worker_id).await;
    team.update_worker(&failed.id, |worker| worker.status = WorkerStatus::Failed)
        .await;
    let pool = failed.worktree.clone();
    release_pool_checkout(root.path().to_path_buf(), pool.clone(), failed.branch.clone())
        .await
        .expect("detach before the lost deletion save");
    if dirty {
        std::fs::write(pool.join("dirty.txt"), "discard\n").expect("dirty detached pool");
    }

    let preview = team
        .deletion_preview(std::slice::from_ref(&failed.id))
        .await
        .expect("detached pool preview");
    assert!(preview[0].pool);
    assert!(preview[0].blocked.is_none(), "{:?}", preview[0].blocked);
    assert_eq!(preview[0].uncommitted, dirty);
    let outcome = team
        .delete_workers(std::slice::from_ref(&failed.id), true)
        .await
        .expect("delete detached pool worker");
    assert!(outcome.results[0].error.is_none(), "{:?}", outcome.results[0].error);
    assert!(team.worker(&failed.id).await.is_err());
    assert!(!branch_exists(root.path(), &failed.branch).await);
    assert!(pool.exists());
    assert!(!pool.join("dirty.txt").exists());
    let saved = TeamPersistence::new(state.path())
        .load()
        .expect("load after deletion")
        .expect("saved team");
    assert!(saved.workers.iter().all(|worker| worker.id != failed.id));

    let next_id = if dirty { "detachednext2" } else { "detachednext1" };
    let next = provision_pool_worker(&team, next_id).await;
    assert_eq!(next.worktree, pool);
    team.shutdown().await;
}

#[tokio::test]
async fn detached_failed_worker_pools_can_be_deleted_clean_or_dirty_and_reused() {
    assert_detached_pool_can_be_deleted(false).await;
    assert_detached_pool_can_be_deleted(true).await;
}


#[tokio::test]
async fn retry_save_failure_is_best_effort_for_the_merge_caller() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let team = open_test_team(root.path(), state.path(), Vec::new()).await;
    let mut stale = worker(root.path(), "retrysave02");
    let pool = root.path().join(".worktrees/pool-1");
    run_git(
        root.path(),
        &[
            "worktree",
            "add",
            "-b",
            &stale.branch,
            pool.to_str().expect("pool path"),
            "main",
        ],
    );
    release_pool_checkout(root.path().to_path_buf(), pool.clone(), stale.branch.clone())
        .await
        .expect("detach the retry candidate");
    stale.worktree = pool.clone();
    stale.status = WorkerStatus::Merged;
    stale.error = Some(format!("{WORKTREE_CLEANUP_ERROR}: retry save failure"));
    let mut just_merged = worker(root.path(), "justmerge01");
    just_merged.status = WorkerStatus::Merged;
    team.records.lock().await.push(stale.clone());
    team.records.lock().await.push(just_merged.clone());
    team.persist().await.expect("save before the injected failure");

    let transcript = state
        .path()
        .join(TRANSCRIPTS_DIR)
        .join(format!("{}.json", stale.id));
    std::fs::create_dir(&transcript).expect("make the transcript rename fail");

    // Unit contract for the cleanup retry used after a merge: this other
    // worker's save failure cannot become an error after the just-merged
    // worker's own required save.
    team.retry_leftover_cleanups().await;

    let retried = team.worker(&stale.id).await.expect("retried worker");
    assert!(retried.worktree.as_os_str().is_empty());
    assert!(retried.error.is_none());
    let merged = team
        .worker(&just_merged.id)
        .await
        .expect("the merge caller still has its merged record");
    assert_eq!(merged.status, WorkerStatus::Merged);
    std::fs::remove_dir(&transcript).expect("unblock the transcript path");
    let saved = TeamPersistence::new(state.path())
        .load()
        .expect("load the old ledger")
        .expect("old ledger");
    let saved_stale = saved
        .workers
        .iter()
        .find(|worker| worker.id == stale.id)
        .expect("stale worker on disk");
    assert_eq!(saved_stale.worktree, pool, "the injected save really failed");
    team.shutdown().await;
}


#[tokio::test]
async fn an_unregistered_pool_is_left_alone_and_never_touches_the_lead_edit() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let pool = root.path().join(".worktrees/pool-1");
    std::fs::create_dir_all(&pool).expect("plain unregistered pool");
    std::fs::write(pool.join("marker.txt"), "keep\n").expect("pool marker");
    let mut failed = worker(root.path(), "unregpool01");
    failed.worktree = pool.clone();
    failed.status = WorkerStatus::Failed;
    run_git(root.path(), &["branch", &failed.branch]);
    std::fs::write(root.path().join("README.md"), "uncommitted lead edit\n")
        .expect("lead edit");
    let team = open_test_team(root.path(), state.path(), vec![failed.clone()]).await;

    let preview = team
        .deletion_preview(std::slice::from_ref(&failed.id))
        .await
        .expect("unregistered pool preview");
    assert!(preview[0].blocked.is_none());
    assert!(preview[0].pool && preview[0].checkout);
    let release = release_pool_checkout(
        root.path().to_path_buf(),
        pool.clone(),
        failed.branch.clone(),
    )
    .await
    .expect_err("unregistered release refuses");
    assert!(release.contains("registered"), "{release}");
    let discard = discard_and_release_pool_checkout(
        root.path().to_path_buf(),
        pool.clone(),
        failed.branch.clone(),
    )
    .await
    .expect_err("unregistered discard refuses");
    assert!(discard.contains("registered"), "{discard}");
    assert_eq!(
        std::fs::read_to_string(root.path().join("README.md")).expect("lead edit survives"),
        "uncommitted lead edit\n"
    );

    let outcome = team
        .delete_workers(std::slice::from_ref(&failed.id), true)
        .await
        .expect("delete unregistered pool record");
    assert!(outcome.results[0].error.is_none());
    assert!(team.worker(&failed.id).await.is_err());
    assert!(!branch_exists(root.path(), &failed.branch).await);
    assert_eq!(
        std::fs::read_to_string(pool.join("marker.txt")).expect("unregistered folder remains"),
        "keep\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("README.md")).expect("lead edit remains"),
        "uncommitted lead edit\n"
    );
    team.shutdown().await;
}


#[tokio::test]
async fn a_detached_pool_behind_its_branch_refuses_to_merge() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    run_git(root.path(), &["checkout", "-b", "wt/mnemo"]);
    let team = open_test_team(root.path(), state.path(), Vec::new()).await;
    let worker = provision_pool_worker(&team, "mergeguard1").await;
    make_worker_reviewable(&team, &worker).await;
    let reviewed_tip = phase3::tip(&worker.worktree).await.expect("reviewed tip A");
    std::fs::write(worker.worktree.join("branch-b.txt"), "B\n").expect("branch B change");
    run_git(&worker.worktree, &["add", "branch-b.txt"]);
    run_git(&worker.worktree, &["commit", "-m", "branch advances to B"]);
    let branch_tip = phase3::tip(&worker.worktree).await.expect("branch tip B");
    assert_ne!(reviewed_tip, branch_tip);
    run_git(
        &worker.worktree,
        &["switch", "--detach", &reviewed_tip],
    );
    let lead_tip = phase3::tip(root.path()).await.expect("lead tip before refusal");

    let error = team.merge(&worker.id).await.expect_err("detached A cannot merge B");

    assert_eq!(
        error,
        "the worker checkout is not on its branch; refusing to merge"
    );
    assert_eq!(
        phase3::tip(root.path()).await.expect("lead tip after refusal"),
        lead_tip
    );
    assert_eq!(
        team.worker(&worker.id).await.expect("worker after refusal").status,
        WorkerStatus::InReview
    );
    assert_eq!(
        phase3::git_value(root.path(), &["rev-parse", &worker.branch])
            .await
            .expect("branch tip after refusal"),
        branch_tip
    );
    team.shutdown().await;
}


#[tokio::test]
async fn merge_tip_pins_the_reviewed_commit_after_the_branch_advances() {
    let root = tempfile::tempdir().expect("workspace");
    init_git(root.path());
    run_git(root.path(), &["checkout", "-b", "wt/mnemo"]);
    let checkout = root.path().join(".worktrees/tip-worker");
    let branch = "wt/agent-immutable01";
    run_git(
        root.path(),
        &[
            "worktree",
            "add",
            "-b",
            branch,
            checkout.to_str().expect("worker checkout"),
            "wt/mnemo",
        ],
    );
    std::fs::write(checkout.join("a.txt"), "reviewed A\n").expect("reviewed change");
    run_git(&checkout, &["add", "a.txt"]);
    run_git(&checkout, &["commit", "-m", "reviewed A"]);
    let reviewed = phase3::tip(&checkout).await.expect("reviewed tip");
    std::fs::write(checkout.join("b.txt"), "later B\n").expect("later change");
    run_git(&checkout, &["add", "b.txt"]);
    run_git(&checkout, &["commit", "-m", "later B"]);
    let later = phase3::tip(&checkout).await.expect("later branch tip");

    let output = merge_tip(root.path(), &reviewed, "merge exact reviewed tip")
        .await
        .expect("run exact-tip merge");
    assert!(output.status.success(), "{}", phase3::git_error(&["merge"], &output));

    assert_eq!(
        phase3::git_value(root.path(), &["rev-parse", "HEAD^2"])
            .await
            .expect("merge second parent"),
        reviewed
    );
    assert_eq!(
        phase3::git_value(root.path(), &["rev-parse", branch])
            .await
            .expect("advanced branch remains"),
        later
    );
    assert!(!root.path().join("b.txt").exists(), "later B is not merged");
}


#[tokio::test]
async fn unregistered_pool_is_rejected_before_worker_cleanliness_or_tip_probes() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    run_git(root.path(), &["checkout", "-b", "wt/mnemo"]);
    let mut worker = worker(root.path(), "guardorder1");
    worker.worktree = root.path().join(".worktrees/pool-1");
    worker.status = WorkerStatus::InReview;
    std::fs::create_dir_all(&worker.worktree).expect("plain pool folder");
    // If merge probes this folder before checking registration, Git fails on
    // this malformed gitfile instead of returning the registration refusal.
    std::fs::write(worker.worktree.join(".git"), "not a gitfile\n").expect("invalid gitfile");
    run_git(root.path(), &["branch", &worker.branch]);
    let lead_tip = phase3::tip(root.path()).await.expect("lead tip");
    let team = open_test_team(root.path(), state.path(), vec![worker.clone()]).await;

    let error = team.merge(&worker.id).await.expect_err("unregistered pool refused");

    assert!(error.contains("not a registered worktree"), "{error}");
    assert!(!error.contains("gitfile"), "worker Git was probed first: {error}");
    assert_eq!(phase3::tip(root.path()).await.expect("unchanged lead"), lead_tip);
    assert_eq!(
        team.worker(&worker.id).await.expect("unchanged worker").status,
        WorkerStatus::InReview
    );
    team.shutdown().await;
}

// --- The two proofs of plan 970d8d66: the red check and the mutation check ---

/// A proof runner that answers with one scripted outcome, so the recording and
/// the notice are exercised without a real test build. The call counters
/// cover the trigger paths: how many times each job really ran.
struct ScriptedProofs {
    outcome: proof::RedOutcome,
    mutants: Vec<proof::Mutant>,
    /// The error a mutation run answers with, when it is scripted to fail.
    mutate_error: Option<String>,
    /// Whether that failing run first leaves an untracked file in the checkout.
    mutate_leaves_dirt: bool,
    prove_calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    mutate_calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl ScriptedProofs {
    fn new(outcome: proof::RedOutcome) -> Self {
        Self {
            outcome,
            mutants: Vec::new(),
            mutate_error: None,
            mutate_leaves_dirt: false,
            prove_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            mutate_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    /// A runner that also reports the given uncaught mutants.
    fn with_mutants(mutants: Vec<proof::Mutant>) -> Self {
        Self {
            mutants,
            ..Self::new(proof::RedOutcome::Red {
                output: "test regression_proves_red ... FAILED".to_string(),
            })
        }
    }

    /// A runner whose mutation run fails with `error`, the way a run that could
    /// not run does (a missing tool, a command that failed).
    fn with_mutate_error(mut self, error: String) -> Self {
        self.mutate_error = Some(error);
        self
    }

    /// A runner whose mutation run leaves an untracked file in the checkout and
    /// then fails: the checkout itself is off its tip.
    fn with_dirty_mutate_error(mut self, error: String) -> Self {
        self.mutate_error = Some(error);
        self.mutate_leaves_dirt = true;
        self
    }
}

#[async_trait::async_trait]
impl proving::ProofRunner for ScriptedProofs {
    async fn prove(&self, _request: proving::ProveRequest) -> Result<proof::RedOutcome, String> {
        self.prove_calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.outcome.clone())
    }

    async fn mutate(&self, request: proving::MutateRequest) -> Result<Vec<proof::Mutant>, String> {
        self.mutate_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(error) = &self.mutate_error {
            if self.mutate_leaves_dirt {
                std::fs::write(request.checkout.join("left-by-the-run.txt"), "dirt\n")
                    .expect("a file the run left behind");
            }
            return Err(error.clone());
        }
        Ok(self.mutants.clone())
    }
}

/// Wait for one lead notice whose text contains `needle`.
async fn drain_notice(
    receiver: &mut tokio::sync::broadcast::Receiver<BoardSignal>,
    needle: &str,
) -> Option<String> {
    for _ in 0..400 {
        while let Ok(signal) = receiver.try_recv() {
            if let BoardSignal::Notice { text, .. } = signal {
                if text.contains(needle) {
                    return Some(text);
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    None
}

/// A team whose board holds a `bug_fixing` plan naming `regression_proves_red`
/// as its regression test, with a provisioned worker whose checkout carries a
/// commit that adds that name in `fix.rs`.
async fn proof_fixture(
    id: &str,
) -> (
    tempfile::TempDir,
    tempfile::TempDir,
    Arc<TeamRuntime>,
    String,
) {
    proof_fixture_with_change(id, "fix.rs").await
}

/// The same fixture, whose worker's one commit adds `rel` instead of `fix.rs`,
/// so a proof can be exercised on a change of another shape (a test-only one).
async fn proof_fixture_with_change(
    id: &str,
    rel: &str,
) -> (
    tempfile::TempDir,
    tempfile::TempDir,
    Arc<TeamRuntime>,
    String,
) {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    std::fs::create_dir_all(root.path().join(".coding")).expect("coding dir");
    std::fs::write(
        root.path().join(".coding/checks.toml"),
        "[regression]\ncommand = \"cargo test -- {name}\"\nran = \"{name} ... \"\n\n[mutation]\ncommand = \"cargo mutants --in-diff {diff} --output {out}\"\nmissed = \"{out}/missed.txt\"\n",
    )
    .expect("checks.toml");
    let plans_dir = root.path().join(".coding/plans");
    std::fs::create_dir_all(&plans_dir).expect("plans dir");
    let board = PlanBoard::new(&plans_dir, root.path().join(".coding/reviews"));
    let plan = board
        .create(mnemo::app::plan_board::PlanDraft {
            title: "Prove the fix".to_string(),
            goal: "The regression test fails without the fix".to_string(),
            context: "verified: the proof wiring lives in src-tauri/src/mnemo2/team/proving.rs; verify with cargo test.".to_string(),
            kind: mnemo::memory::plan_format::PlanKind::BugFixing,
            bug: Some("a filter accepts everything".to_string()),
            steps: vec![
                "**Wire** the proof in src-tauri/src/mnemo2/team/proving.rs".to_string(),
            ],
            ..Default::default()
        })
        .expect("create the plan");
    board
        .update(
            &plan.id,
            mnemo::app::plan_board::PlanEdit {
                regression_test: Some("regression_proves_red".to_string()),
                ..Default::default()
            },
        )
        .expect("record the regression test");
    let team = Arc::new(open_test_team(root.path(), state.path(), Vec::new()).await);
    team.attach_board(Arc::new(board));
    let worker = provision_pool_worker(&team, id).await;
    let change = worker.worktree.join(rel);
    if let Some(parent) = change.parent() {
        std::fs::create_dir_all(parent).expect("change parent");
    }
    std::fs::write(&change, "fn regression_proves_red() {}\n").expect("worker change");
    run_git(&worker.worktree, &["add", rel]);
    run_git(
        &worker.worktree,
        &["commit", "-m", "add the regression test"],
    );
    team.update_worker(&worker.id, |current| {
        current.plan_id = Some(plan.id.clone());
        current.plan_step = Some(1);
        // A finished worker reads Completed: the claim's idle test (F1)
        // accepts only a settled worker.
        current.status = WorkerStatus::Completed;
    })
    .await;
    let record = team.worker(&worker.id).await.expect("worker");
    assert!(
        team.proof_ctx().worker_plan(&record).await.is_some(),
        "the fixture's plan parses back from the board"
    );
    assert!(
        proof::load_proof_config(root.path()).regression.is_some(),
        "the fixture configures a regression command: {:?}",
        std::fs::read_to_string(root.path().join(".coding/checks.toml"))
    );
    (root, state, team, worker.id)
}

/// While a proof runs, every entry point that would move the worker's checkout
/// refuses with one message naming the proof, and the merge gate refuses even
/// when it is reached directly.
#[tokio::test]
async fn merge_refuses_while_proving() {
    let (_root, _state, team, worker_id) = proof_fixture("proveBusy01").await;
    assert!(team.proof_ctx().proving.hold_for_test(&worker_id));
    let expected = proving::proving_error(&worker_id);
    assert!(
        expected.contains("is proving"),
        "the refusal names the proof: {expected}"
    );
    assert_eq!(
        team.message(&worker_id, "hello".to_string(), MessageSender::Lead)
            .await
            .expect_err("a message is refused while the proof runs"),
        expected
    );
    assert_eq!(
        team.merge_as(&worker_id, true)
            .await
            .expect_err("a merge is refused while the proof runs"),
        expected
    );
    assert_eq!(
        team.resume(&worker_id)
            .await
            .expect_err("a resume is refused while the proof runs"),
        expected
    );
    assert_eq!(
        team.route_fix(&worker_id, &proof_finding(&worker_id))
            .await
            .expect_err("a routed fix is refused while the proof runs"),
        expected
    );
    let worker = team.worker(&worker_id).await.expect("worker");
    let running = proving::MergeProof {
        running: true,
        ..Default::default()
    };
    let error = validate_merge(&worker, "tip0001", &[], &full_lane(), &running)
        .expect_err("the gate refuses a running proof");
    assert_eq!(error, expected);
    team.shutdown().await;
}

/// A resolved finding a routed fix would carry: the route is refused before the
/// finding matters.
fn proof_finding(worker_id: &str) -> String {
    format!("{worker_id}-finding")
}

/// A bug-fix diff that adds the plan's regression test cannot merge until the
/// tip carries the passed red check or the lead resolved the proof finding.
#[tokio::test]
async fn merge_requires_red_proof_for_bug_fix_adding_the_test() {
    let worker = in_review_worker(Some(evidence()));
    let required = proving::MergeProof {
        regression_required: true,
        ..Default::default()
    };
    let error = validate_merge(&worker, "tip0001", &[], &full_lane(), &required)
        .expect_err("an unproven regression test blocks the merge");
    assert_eq!(
        error,
        "the regression test has not been proven red at this tip; wait for the proof or run team_prove_regression"
    );
    let proven = proving::MergeProof {
        regression_required: true,
        regression_proven: true,
        ..Default::default()
    };
    validate_merge(&worker, "tip0001", &[], &full_lane(), &proven)
        .expect("a red check at the tip merges");
    let waived = proving::MergeProof {
        regression_required: true,
        regression_waived: true,
        ..Default::default()
    };
    validate_merge(&worker, "tip0001", &[], &full_lane(), &waived)
        .expect("a resolved proof finding waives the red check");
}

/// A red outcome records the passed check at the proved tip and tells the lead.
#[tokio::test]
async fn red_outcome_records_a_passed_check_and_notice() {
    let (_root, _state, team, worker_id) = proof_fixture("proveRed01").await;
    let proofs = ScriptedProofs::new(proof::RedOutcome::Red {
        output: "test regression_proves_red ... FAILED\nfailures:\n    regression_proves_red".to_string(),
    });
    team.set_proof_runner_for_test(Arc::new(proofs));
    let mut notices = team.subscribe_board();
    let worker = team.worker(&worker_id).await.expect("worker");
    let tip = phase3::tip(&worker.worktree).await.expect("tip");
    team.prove_regression_tool(&worker_id)
        .await
        .expect("start the red proof");
    let notice = drain_notice(&mut notices, "regression proof for")
        .await
        .expect("the proof finishes with a notice");
    assert!(notice.contains("red (fails without the fix)"), "{notice}");
    let check = eventually(|| async {
        team.worker(&worker_id).await.ok().and_then(|worker| {
            worker
                .checks
                .into_iter()
                .find(|check| check.name == proving::RED_CHECK)
        })
    })
    .await;
    assert_eq!(check.status, "passed");
    assert_eq!(check.tip, tip);
    let detail = check.detail.unwrap_or_default();
    assert!(detail.contains("regression_proves_red"), "{detail}");
    let proven = team.worker(&worker_id).await.expect("worker");
    let inputs = team.proof_ctx().merge_inputs(&proven, &tip).await;
    assert!(inputs.regression_required, "{inputs:?}");
    assert!(inputs.regression_proven, "{inputs:?}");
    team.shutdown().await;
}

/// A regression test that still passes without the fix opens the high finding
/// naming the file, and the gate then refuses the merge.
#[tokio::test]
async fn not_red_outcome_opens_a_high_finding() {
    let (_root, _state, team, worker_id) = proof_fixture("proveNotRed01").await;
    let proofs = ScriptedProofs::new(proof::RedOutcome::NotRed {
        output: "test regression_proves_red ... ok".to_string(),
    });
    team.set_proof_runner_for_test(Arc::new(proofs));
    let mut notices = team.subscribe_board();
    let worker = team.worker(&worker_id).await.expect("worker");
    let tip = phase3::tip(&worker.worktree).await.expect("tip");
    team.prove_regression_tool(&worker_id)
        .await
        .expect("start the red proof");
    let notice = drain_notice(&mut notices, "regression proof for")
        .await
        .expect("the proof finishes with a notice");
    assert!(
        notice.contains("NOT red, finding regression-not-red opened"),
        "{notice}"
    );
    let finding = eventually(|| async {
        team.worker(&worker_id).await.ok().and_then(|worker| {
            worker
                .findings
                .into_iter()
                .find(|finding| finding.id == proving::NOT_RED_ID)
        })
    })
    .await;
    assert_eq!(finding.severity, "high");
    assert_eq!(finding.status, "open");
    assert_eq!(finding.title, "The regression test passes without the fix");
    assert_eq!(finding.file, "fix.rs");
    assert!(
        finding.detail.contains("the test does not reproduce the defect"),
        "{}",
        finding.detail
    );
    let inputs = team.proof_ctx().merge_inputs(&worker, &tip).await;
    assert!(inputs.regression_required, "{inputs:?}");
    assert!(!inputs.regression_proven, "{inputs:?}");
    team.shutdown().await;
}

/// The mutation tool is refused with the configuration hint when the project
/// configured no `[mutation]` table, and both proof tools are exposed.
#[tokio::test]
async fn mutation_tool_refuses_without_config() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    let team = open_test_team(root.path(), state.path(), Vec::new()).await;
    let specs = team.lead_host_tools();
    for name in ["team_prove_regression", "team_mutation_check"] {
        assert!(
            specs.iter().any(|spec| spec.name == name),
            "{name} is exposed to the lead"
        );
    }
    assert_eq!(
        team.mutation_check_tool("mutationCfg01")
            .await
            .expect_err("no [mutation] table configured"),
        "configure [mutation] in .coding/checks.toml"
    );
    team.shutdown().await;
}

// --- The review fixes: claimRace, secondJobDropped, deleteDuringProof,
// parkedTrigger, steerTimeoutTrigger, proofPerTip, restartWhileProving,
// auditKindFilter ---

/// A claim for a worker whose turn just started is refused: no proof may run
/// underneath a turn that edits the same checkout (finding claimRace). The
/// refused claim leaves nothing behind — no claim, no marker.
#[tokio::test]
async fn claim_refuses_a_busy_worker() {
    let (_root, _state, team, worker_id) = proof_fixture("proveBusyRace1").await;
    team.update_worker(&worker_id, |current| {
        current.status = WorkerStatus::Working;
    })
    .await;
    let error = team
        .proof_ctx()
        .spawn(&worker_id, &[proving::ProofJob::Regression])
        .await
        .expect_err("a turn that just started is refused");
    assert!(error.contains("is busy"), "{error}");
    assert!(
        !team.proof_ctx().proving.is_active(&worker_id),
        "the refused claim is released"
    );
    assert!(
        !team.workspace_root.join(".coding/mnemo2/proving").join(format!("{worker_id}.json")).exists(),
        "the refused claim writes no marker"
    );
    team.shutdown().await;
}

/// After a claim, the message entry point refuses under the merge lock: the
/// claim and the turn start are serialized on it, so the guard cannot miss a
/// claim that happened between the lock and the check (finding claimRace).
#[tokio::test]
async fn message_after_claim_is_refused_under_lock() {
    let (_root, _state, team, worker_id) = proof_fixture("proveClaimLock1").await;
    assert!(team.proof_ctx().proving.hold_for_test(&worker_id));
    assert_eq!(
        team.message(&worker_id, "hello".to_string(), MessageSender::Lead)
            .await
            .expect_err("a message is refused while the proof runs"),
        proving::proving_error(&worker_id)
    );
    team.shutdown().await;
}

/// One claim runs both jobs a high-risk turn owes, in order: regression first,
/// then mutation — the second job is never dropped by the first claim (finding
/// secondJobDropped). Each job records its own result.
#[tokio::test]
async fn turn_end_runs_both_owed_proofs() {
    let (_root, _state, team, worker_id) = proof_fixture("proveBothJobs1").await;
    team.update_worker(&worker_id, |current| {
        current.review_risk = "high".to_string();
    })
    .await;
    let proofs = ScriptedProofs::new(proof::RedOutcome::Red {
        output: "test regression_proves_red ... FAILED".to_string(),
    });
    let prove_calls = proofs.prove_calls.clone();
    let mutate_calls = proofs.mutate_calls.clone();
    team.set_proof_runner_for_test(Arc::new(proofs));
    proving::spawn_turn_proofs(&team.proof_ctx(), worker_id.clone()).await;
    eventually(|| async {
        (prove_calls.load(Ordering::SeqCst) == 1 && mutate_calls.load(Ordering::SeqCst) == 1)
            .then_some(())
    })
    .await;
    eventually(|| async {
        team.worker(&worker_id).await.ok().and_then(|worker| {
            let red = worker
                .checks
                .iter()
                .any(|check| check.name == proving::RED_CHECK && check.status == "passed");
            let clean = worker.checks.iter().any(|check| {
                check.name == proving::MUTATION_CLEAN_CHECK && check.status == "passed"
            });
            (red && clean).then_some(())
        })
    })
    .await;
    // One claim, released when both jobs finished: the last record lands
    // before the guard drops, so the release is awaited, not assumed.
    eventually(|| async {
        (!team.proof_ctx().proving.is_active(&worker_id)).then_some(())
    })
    .await;
    team.shutdown().await;
}

/// Deleting a worker whose proof runs is refused with the proof's refusal
/// text: the deletion would trash the checkout the proof is restoring (finding
/// deleteDuringProof).
#[tokio::test]
async fn delete_refuses_while_proving() {
    let (_root, _state, team, worker_id) = proof_fixture("proveDelete1").await;
    assert!(team.proof_ctx().proving.hold_for_test(&worker_id));
    let outcome = team
        .delete_workers(std::slice::from_ref(&worker_id), true)
        .await
        .expect("the deletion flow runs and reports per worker");
    assert_eq!(outcome.results.len(), 1, "{:?}", outcome.results);
    assert_eq!(
        outcome.results[0].error.as_deref(),
        Some(proving::proving_error(&worker_id).as_str()),
        "the refused deletion names the proof"
    );
    team.worker(&worker_id)
        .await
        .expect("the proved worker is not deleted");
    team.shutdown().await;
}

/// Only an implementation worker can be proven: an audit worker's checkout
/// is the reviewer's, and its findings are the deliverable (finding
/// auditKindFilter).
#[tokio::test]
async fn proof_tools_refuse_audit_workers() {
    let (_root, _state, team, worker_id) = proof_fixture("proveAuditKind1").await;
    team.update_worker(&worker_id, |current| {
        current.kind = "audit".to_string();
    })
    .await;
    assert_eq!(
        team.prove_regression_tool(&worker_id)
            .await
            .expect_err("an audit worker is not proven"),
        "only implementation workers are proven"
    );
    assert_eq!(
        team.mutation_check_tool(&worker_id)
            .await
            .expect_err("an audit worker is not mutated"),
        "only implementation workers are proven"
    );
    team.shutdown().await;
}

/// A proof finding names its tip, so a worker that moves its tip is proven
/// again: the open finding is updated in place, and a Red result closes it as
/// fixed with the tip it was proven at (finding proofPerTip).
#[tokio::test]
async fn reproof_at_new_tip_runs_and_closes_old_finding() {
    let (_root, _state, team, worker_id) = proof_fixture("proveTipMove1").await;
    let worker = team.worker(&worker_id).await.expect("worker");
    let first_tip = phase3::tip(&worker.worktree).await.expect("first tip");
    let proofs = ScriptedProofs::new(proof::RedOutcome::NotRed {
        output: "test regression_proves_red ... ok".to_string(),
    });
    team.set_proof_runner_for_test(Arc::new(proofs));
    team.prove_regression_tool(&worker_id)
        .await
        .expect("start the first proof");
    let finding = eventually(|| async {
        team.worker(&worker_id).await.ok().and_then(|worker| {
            worker
                .findings
                .into_iter()
                .find(|finding| finding.id == proving::NOT_RED_ID && finding.status == "open")
        })
    })
    .await;
    assert_eq!(
        finding.detail.lines().next(),
        Some(format!("tip {first_tip}").as_str()),
        "the finding names the tip it was proven at: {}",
        finding.detail
    );
    // The worker commits again: the moved tip has no proof result, so the
    // re-proof is due and updates the same finding in place.
    std::fs::write(
        worker.worktree.join("fix.rs"),
        "fn regression_proves_red() {}\nfn more() {}\n",
    )
    .expect("the second worker change");
    run_git(&worker.worktree, &["add", "fix.rs"]);
    run_git(&worker.worktree, &["commit", "-m", "the second change"]);
    let second_tip = phase3::tip(&worker.worktree).await.expect("second tip");
    assert_ne!(first_tip, second_tip);
    team.set_proof_runner_for_test(Arc::new(ScriptedProofs::new(proof::RedOutcome::Red {
        output: "test regression_proves_red ... FAILED".to_string(),
    })));
    team.prove_regression_tool(&worker_id)
        .await
        .expect("start the re-proof");
    let (closed, check_tip) = eventually(|| async {
        team.worker(&worker_id).await.ok().and_then(|worker| {
            let closed = worker.findings.iter().find(|finding| {
                finding.id == proving::NOT_RED_ID && finding.status == "fixed"
            })?;
            let tip = worker
                .checks
                .iter()
                .find(|check| check.name == proving::RED_CHECK && check.status == "passed")?
                .tip
                .clone();
            Some((closed.clone(), tip))
        })
    })
    .await;
    assert_eq!(
        closed.evidence.as_deref(),
        Some(format!("proven red at tip {second_tip}").as_str()),
        "the closed finding carries the proof"
    );
    assert_eq!(check_tip, second_tip, "the red check is stamped at the new tip");
    team.shutdown().await;
}

/// A finding closed `fixed` never waives the gate: fixed must be shown by the
/// red check at the tip being merged. Only a wontfix — a written reason —
/// waives (finding proofPerTip).
#[tokio::test]
async fn fixed_finding_does_not_waive_the_gate() {
    let (_root, _state, team, worker_id) = proof_fixture("proveFixedGate1").await;
    let worker = team.worker(&worker_id).await.expect("worker");
    let tip = phase3::tip(&worker.worktree).await.expect("tip");
    let detail = format!("tip {tip}\nthe test does not reproduce the defect");
    team.update_worker(&worker_id, |current| {
        current.findings.push(Finding {
            id: proving::NOT_RED_ID.to_string(),
            severity: "high".to_string(),
            title: "The regression test passes without the fix".to_string(),
            detail: detail.clone(),
            file: "fix.rs".to_string(),
            line: 0,
            status: "fixed".to_string(),
            fix_commit: Some(tip.clone()),
            evidence: Some(format!("proven red at tip {tip}")),
            justification: None,
            category: None,
            enforce: None,
        });
    })
    .await;
    let worker = team.worker(&worker_id).await.expect("worker");
    let fixed = team.proof_ctx().merge_inputs(&worker, &tip).await;
    assert!(fixed.regression_required, "{fixed:?}");
    assert!(!fixed.regression_proven, "{fixed:?}");
    assert!(!fixed.regression_waived, "a fixed finding never waives: {fixed:?}");
    let review = in_review_worker(Some(evidence()));
    let error = validate_merge(&review, "tip0001", &[], &full_lane(), &fixed)
        .expect_err("a fixed finding must not pass the gate");
    assert!(
        error.contains("not been proven red"),
        "the gate names the missing proof: {error}"
    );
    // The same finding resolved wontfix — a written reason — is the only
    // waiver.
    team.update_worker(&worker_id, |current| {
        if let Some(finding) = current
            .findings
            .iter_mut()
            .find(|finding| finding.id == proving::NOT_RED_ID)
        {
            finding.status = "wontfix".to_string();
            finding.justification =
                Some("the harness cannot run this test red".to_string());
        }
    })
    .await;
    let worker = team.worker(&worker_id).await.expect("worker");
    let wontfix = team.proof_ctx().merge_inputs(&worker, &tip).await;
    assert!(wontfix.regression_waived, "{wontfix:?}");
    validate_merge(&review, "tip0001", &[], &full_lane(), &wontfix)
        .expect("a wontfix with a written reason waives the gate");
    team.shutdown().await;
}

/// A hard kill mid-proof leaves a claim marker; at open the same restore the
/// proof itself uses repairs the checkout back to the marker's tip. A marker
/// whose checkout moved past its tip is never touched (finding
/// restartWhileProving).
#[tokio::test]
async fn startup_repairs_an_interrupted_proof() {
    let (root, _state, team, worker_id) = proof_fixture("proveRepair1").await;
    let worker = team.worker(&worker_id).await.expect("worker");
    let tip = phase3::tip(&worker.worktree).await.expect("tip");
    // An interrupted proof: the reverse-applied source never made it back.
    std::fs::write(
        worker.worktree.join("fix.rs"),
        "the reverse-applied remnant\n",
    )
    .expect("the dirty remnant");
    std::fs::write(worker.worktree.join("stray.txt"), "stray\n").expect("stray file");
    let marker = root
        .path()
        .join(".coding/mnemo2/proving")
        .join(format!("{worker_id}.json"));
    std::fs::create_dir_all(marker.parent().expect("marker dir")).expect("marker dir");
    std::fs::write(
        &marker,
        serde_json::json!({"tip": tip, "checkout": worker.worktree.to_string_lossy()})
            .to_string(),
    )
    .expect("the claim marker");
    let workers = vec![team.worker(&worker_id).await.expect("worker")];
    proving::repair_interrupted_proofs(root.path(), &workers).await;
    let status = phase3::git_value(&worker.worktree, &["status", "--porcelain", "-uall"])
        .await
        .expect("read the checkout status");
    assert_eq!(status, "", "the interrupted proof is repaired to its tip");
    assert!(!marker.exists(), "the processed marker is deleted");
    // A marker whose checkout moved past its tip is never touched: the HEAD
    // is some other commit's work, not an interrupted proof of this tip.
    run_git(&worker.worktree, &["checkout", "--", "fix.rs"]);
    run_git(
        &worker.worktree,
        &["commit", "--allow-empty", "-m", "past the marker"],
    );
    let moved_tip = phase3::tip(&worker.worktree).await.expect("moved tip");
    std::fs::write(
        &marker,
        serde_json::json!({"tip": tip, "checkout": worker.worktree.to_string_lossy()})
            .to_string(),
    )
    .expect("the stale marker");
    std::fs::write(worker.worktree.join("stray.txt"), "stray\n").expect("stray file");
    proving::repair_interrupted_proofs(root.path(), &workers).await;
    let status = phase3::git_value(&worker.worktree, &["status", "--porcelain", "-uall"])
        .await
        .expect("read the checkout status");
    assert!(
        status.contains("stray.txt"),
        "a checkout past the marker's tip ({moved_tip}) is never touched"
    );
    team.shutdown().await;
}

/// The proof fixture over the scripted engine: the same bug-fixing plan, proof
/// configuration and provisioned worker as [`proof_fixture`], but the LLM is
/// scripted, so a worker's real turn can be driven into the pump's settling
/// paths (findings parkedTrigger and steerTimeoutTrigger).
///
/// The team's own session spawn sets no run limits and keeps its steer flag
/// inside the pump, so the tests wire the session and the real pump themselves
/// through [`manual_pump_worker`].
async fn scripted_proof_fixture(
    id: &str,
) -> (
    tempfile::TempDir,
    tempfile::TempDir,
    tempfile::TempDir,
    Arc<TeamRuntime>,
    String,
) {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    let config = tempfile::tempdir().expect("config");
    init_git(root.path());
    std::fs::create_dir_all(root.path().join(".coding")).expect("coding dir");
    std::fs::write(
        root.path().join(".coding/checks.toml"),
        "[regression]\ncommand = \"cargo test -- {name}\"\nran = \"{name} ... \"\n\n[mutation]\ncommand = \"cargo mutants --in-diff {diff} --output {out}\"\nmissed = \"{out}/missed.txt\"\n",
    )
    .expect("checks.toml");
    std::fs::write(
        config.path().join("config.toml"),
        "[general]\ndefault_provider = \"test\"\ndefault_model = \"scripted-a\"\n",
    )
    .expect("config.toml");
    std::fs::write(
        config.path().join("endpoints.toml"),
        "[[endpoint]]\nname = \"test\"\nkind = \"local\"\nbase_url = \"http://127.0.0.1:9/v1/\"\nmodels = [\"scripted-a\"]\n",
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
    .provider(Arc::new(ScriptedLlm::new(Vec::new())))
    .start()
    .await
    .expect("start the scripted engine");
    let team = Arc::new(
        TeamRuntime::open(TeamRuntimeOptions {
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
        .expect("open the scripted team"),
    );
    let plans_dir = root.path().join(".coding/plans");
    std::fs::create_dir_all(&plans_dir).expect("plans dir");
    let board = PlanBoard::new(&plans_dir, root.path().join(".coding/reviews"));
    let plan = board
        .create(mnemo::app::plan_board::PlanDraft {
            title: "Prove the fix".to_string(),
            goal: "The regression test fails without the fix".to_string(),
            context: "verified: the proof wiring lives in src-tauri/src/mnemo2/team/proving.rs; verify with cargo test.".to_string(),
            kind: mnemo::memory::plan_format::PlanKind::BugFixing,
            bug: Some("a filter accepts everything".to_string()),
            steps: vec![
                "**Wire** the proof in src-tauri/src/mnemo2/team/proving.rs".to_string(),
            ],
            ..Default::default()
        })
        .expect("create the plan");
    board
        .update(
            &plan.id,
            mnemo::app::plan_board::PlanEdit {
                regression_test: Some("regression_proves_red".to_string()),
                ..Default::default()
            },
        )
        .expect("record the regression test");
    team.attach_board(Arc::new(board));
    let worker = provision_pool_worker(&team, id).await;
    std::fs::write(
        worker.worktree.join("fix.rs"),
        "fn regression_proves_red() {}\n",
    )
    .expect("worker change");
    run_git(&worker.worktree, &["add", "fix.rs"]);
    run_git(&worker.worktree, &["commit", "-m", "add the regression test"]);
    team.update_worker(&worker.id, |current| {
        current.plan_id = Some(plan.id.clone());
        current.plan_step = Some(1);
        current.status = WorkerStatus::Completed;
    })
    .await;
    (root, state, config, team, worker.id)
}

/// Spawn one worker session directly on the scripted engine — with `limits`,
/// which the team's own spawn path never sets — wire the REAL pump over it,
/// with the pump's own steer flag preset, then prompt it. The handles are the
/// team's own, exactly as `start_worker` wires them. The session stays with
/// the team's live map: `AgentSession` has no `Clone`.
async fn manual_pump_worker(
    team: &Arc<TeamRuntime>,
    worker: &WorkerRecord,
    limits: Option<mnemo::headless::SessionLimits>,
    steer_pending: bool,
) {
    let generation = team.generation.fetch_add(1, Ordering::SeqCst) + 1;
    team.update_worker(&worker.id, |current| {
        current.generation = generation;
    })
    .await;
    let session = team
        .engine
        .clone()
        .spawn(mnemo::headless::SpawnOptions {
            name: Some(worker.name.clone()),
            model: None,
            reasoning_effort: None,
            worktree: Some(worker.worktree.clone()),
            check_commands: Vec::new(),
            permissions: Some(mnemo::agent::policy::PermissionPolicy::OverridePermissions),
            policy: None,
            resume: None,
            limits,
            host_tools: Vec::new(),
            external_policy: None,
            guardrails: None,
            trace_identity: Some(TeamRuntime::worker_trace_identity(worker)),
            pinned_task: None,
            skip_turn_routing: true,
        })
        .await
        .expect("spawn the scripted session");
    let session_id = session.id();
    let busy = Arc::new(AtomicBool::new(false));
    let receiver = session.events();
    let strikes = Arc::new(Mutex::new(route_question::Strikes::new(NUDGE_STRIKE_LIMIT)));
    let steer = Arc::new(AtomicBool::new(steer_pending));
    let pump = tokio::spawn(pump_worker(
        worker.id.clone(),
        generation,
        team.engine.clone(),
        session_id,
        receiver,
        team.events.clone(),
        busy.clone(),
        team.messages.clone(),
        team.records.clone(),
        team.usage.clone(),
        team.save_lock.clone(),
        team.pricing.clone(),
        team.merge_lock.clone(),
        Arc::new(Vec::new()),
        team.persistence.clone(),
        team.routing_log_path.clone(),
        team.routing.clone(),
        team.signals.clone(),
        strikes.clone(),
        steer.clone(),
        team.models.clone(),
        team.tiers.clone(),
        team.live.clone(),
        team.fail_next_close.clone(),
        team.board(),
        team.proof_ctx(),
    ));
    session
        .send(mnemo::headless::SessionCommand::Prompt {
            text: "do the work".to_string(),
            images: Vec::new(),
        })
        .await
        .expect("prompt the session");
    team.live.lock().await.insert(
        worker.id.clone(),
        LiveWorker {
            session,
            pump,
            cleanup_retry_only: false,
            busy,
            strikes,
            steer_pending: steer,
        },
    );
}

/// A turn that parks on a run limit did not finish its work: no proof runs,
/// even though the plan, the diff and the missing red check would owe one
/// (finding parkedTrigger).
#[tokio::test]
async fn parked_turn_end_triggers_no_proof() {
    let (_root, _state, _config, team, worker_id) =
        scripted_proof_fixture("proveParkedEnd1").await;
    let worker = team.worker(&worker_id).await.expect("worker");
    let proofs = ScriptedProofs::new(proof::RedOutcome::Red {
        output: "test regression_proves_red ... FAILED".to_string(),
    });
    let prove_calls = proofs.prove_calls.clone();
    team.set_proof_runner_for_test(Arc::new(proofs));
    manual_pump_worker(
        &team,
        &worker,
        Some(mnemo::headless::SessionLimits {
            max_turns: Some(0),
            ..Default::default()
        }),
        false,
    )
    .await;
    // The park settles with the limit error; the turn never reads Completed.
    eventually(|| async {
        team.worker(&worker_id).await.ok().and_then(|worker| {
            worker
                .error
                .as_deref()
                .is_some_and(|error| error.contains("limit before finishing"))
                .then_some(())
        })
    })
    .await;
    // Past the steer follow-up grace, so a wrongly parked trigger had its
    // chance.
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    assert_eq!(
        prove_calls.load(Ordering::SeqCst),
        0,
        "a parked turn never triggered its proof"
    );
    assert!(
        !team.proof_ctx().proving.is_active(&worker_id),
        "no claim was taken"
    );
    let worker = team.worker(&worker_id).await.expect("worker");
    assert!(
        !worker.checks.iter().any(|check| check.name == proving::RED_CHECK),
        "the parked turn recorded no red check"
    );
    team.shutdown().await;
}

/// A steered turn end the follow-up deadline settles as Completed owes its
/// proof like any other turn end — the deadline branch continues and never
/// reaches the natural trigger below it (finding steerTimeoutTrigger).
#[tokio::test]
async fn steer_timeout_completion_triggers_proof() {
    let (_root, _state, _config, team, worker_id) =
        scripted_proof_fixture("proveSteerGrace1").await;
    let worker = team.worker(&worker_id).await.expect("worker");
    let tip = phase3::tip(&worker.worktree).await.expect("tip");
    let proofs = ScriptedProofs::new(proof::RedOutcome::Red {
        output: "test regression_proves_red ... FAILED".to_string(),
    });
    let prove_calls = proofs.prove_calls.clone();
    team.set_proof_runner_for_test(Arc::new(proofs));
    manual_pump_worker(&team, &worker, None, true).await;
    // The held finish settles through the deadline, which fires the trigger.
    eventually(|| async {
        team.worker(&worker_id).await.ok().and_then(|worker| {
            worker
                .checks
                .iter()
                .find(|check| {
                    check.name == proving::RED_CHECK
                        && check.status == "passed"
                        && check.tip == tip
                })
                .map(|_| ())
        })
    })
    .await;
    assert_eq!(prove_calls.load(Ordering::SeqCst), 1, "the proof ran once");
    let worker = team.worker(&worker_id).await.expect("worker");
    assert_eq!(worker.status, WorkerStatus::Completed, "{worker:?}");
    team.shutdown().await;
}

// --- The delta review fixes: staleWaiver, mutantIds, cleanupDuringProof,
// engineTurnUnderClaim, reviewDropsProofFindings, spdxHeader ---

/// A wontfix waives the gate only for the tip its finding names: the same
/// wontfix at an earlier tip does not cover a later one (finding staleWaiver).
#[tokio::test]
async fn waiver_is_scoped_to_its_tip() {
    let (_root, _state, team, worker_id) = proof_fixture("proveWaiverTip1").await;
    let worker = team.worker(&worker_id).await.expect("worker");
    let tip = phase3::tip(&worker.worktree).await.expect("tip");
    let detail =
        "tip 0000000000000000000000000000000000000000\nthe harness cannot run this red".to_string();
    team.update_worker(&worker_id, |current| {
        current.findings.push(Finding {
            id: proving::INCONCLUSIVE_ID.to_string(),
            severity: "low".to_string(),
            title: "The regression test could not be proven red".to_string(),
            detail: detail.clone(),
            file: "fix.rs".to_string(),
            line: 0,
            status: "wontfix".to_string(),
            fix_commit: None,
            evidence: None,
            justification: Some("the harness cannot run this red".to_string()),
            category: None,
            enforce: None,
        });
    })
    .await;
    let worker = team.worker(&worker_id).await.expect("worker");
    let stale = team.proof_ctx().merge_inputs(&worker, &tip).await;
    assert!(!stale.regression_waived, "a wontfix for an earlier tip does not waive: {stale:?}");
    // The same wontfix, written for the tip being merged, waives.
    team.update_worker(&worker_id, |current| {
        if let Some(finding) = current
            .findings
            .iter_mut()
            .find(|finding| finding.id == proving::INCONCLUSIVE_ID)
        {
            finding.detail = format!("tip {tip}\nthe harness cannot run this red");
        }
    })
    .await;
    let worker = team.worker(&worker_id).await.expect("worker");
    let current = team.proof_ctx().merge_inputs(&worker, &tip).await;
    assert!(current.regression_waived, "a wontfix for this tip waives: {current:?}");
    team.shutdown().await;
}

/// A NotRed proof at a new tip re-opens the proof finding the lead closed at
/// the earlier tip: the closure described that tip, and the re-opened
/// finding's detail names the new one (finding staleWaiver).
#[tokio::test]
async fn not_red_at_new_tip_reopens_closed_finding() {
    let (_root, _state, team, worker_id) = proof_fixture("proveReopen1").await;
    let worker = team.worker(&worker_id).await.expect("worker");
    let first_tip = phase3::tip(&worker.worktree).await.expect("first tip");
    let proofs = ScriptedProofs::new(proof::RedOutcome::NotRed {
        output: "test regression_proves_red ... ok".to_string(),
    });
    team.set_proof_runner_for_test(Arc::new(proofs));
    team.prove_regression_tool(&worker_id)
        .await
        .expect("start the first proof");
    eventually(|| async {
        team.worker(&worker_id).await.ok().and_then(|worker| {
            worker
                .findings
                .iter()
                .find(|finding| finding.id == proving::NOT_RED_ID && finding.status == "open")
                .map(|_| ())
        })
    })
    .await;
    // The lead resolves it wontfix — for the first tip.
    team.update_worker(&worker_id, |current| {
        if let Some(finding) = current
            .findings
            .iter_mut()
            .find(|finding| finding.id == proving::NOT_RED_ID)
        {
            finding.status = "wontfix".to_string();
            finding.justification = Some("the harness cannot run this red".to_string());
        }
    })
    .await;
    std::fs::write(
        worker.worktree.join("fix.rs"),
        "fn regression_proves_red() {}\nfn more() {}\n",
    )
    .expect("the second worker change");
    run_git(&worker.worktree, &["add", "fix.rs"]);
    run_git(&worker.worktree, &["commit", "-m", "the second change"]);
    let second_tip = phase3::tip(&worker.worktree).await.expect("second tip");
    assert_ne!(first_tip, second_tip);
    team.set_proof_runner_for_test(Arc::new(ScriptedProofs::new(
        proof::RedOutcome::NotRed {
            output: "test regression_proves_red ... ok".to_string(),
        },
    )));
    team.prove_regression_tool(&worker_id)
        .await
        .expect("start the re-proof");
    let reopened = eventually(|| async {
        team.worker(&worker_id).await.ok().and_then(|worker| {
            let matches: Vec<_> = worker
                .findings
                .iter()
                .filter(|finding| finding.id == proving::NOT_RED_ID)
                .collect();
            (matches.len() == 1
                && matches[0].status == "open"
                && matches[0].detail.lines().next() == Some(format!("tip {second_tip}").as_str()))
                .then_some(matches[0].clone())
        })
    })
    .await;
    assert!(
        reopened.justification.is_none() && reopened.evidence.is_none() && reopened.fix_commit.is_none(),
        "the re-open clears the earlier tip's resolution: {reopened:?}"
    );
    team.shutdown().await;
}

/// A mutant finding's id is its content, not its position: the same uncaught
/// change keeps one finding across runs, and a different change never reuses
/// it (finding mutantIds).
#[test]
fn mutant_ids_are_stable_by_content() {
    let same = proving::mutant_id("src/fix.rs", "x == 1");
    assert_eq!(same, proving::mutant_id("src/fix.rs", "x == 1"));
    assert_ne!(same, proving::mutant_id("src/fix.rs", "x == 2"));
    assert_ne!(same, proving::mutant_id("src/other.rs", "x == 1"));
    let suffix = same.strip_prefix("mutant-").expect("the id carries its prefix");
    assert_eq!(suffix.len(), 8, "{same}");
    assert!(
        suffix.chars().all(|digit| digit.is_ascii_hexdigit() && !digit.is_ascii_uppercase()),
        "{same}"
    );
}

/// A later mutation run opens the newly uncaught mutants and closes the ones
/// it no longer reports; a mutant it reports again keeps its finding (finding
/// mutantIds).
#[tokio::test]
async fn later_run_reports_new_mutant_and_closes_gone_one() {
    let (_root, _state, team, worker_id) = proof_fixture("proveMutantIds1").await;
    let worker = team.worker(&worker_id).await.expect("worker");
    let tip = phase3::tip(&worker.worktree).await.expect("tip");
    let mutant = |line: u32, text: &str| proof::Mutant {
        file: "src/fix.rs".to_string(),
        line,
        text: text.to_string(),
    };
    team.set_proof_runner_for_test(Arc::new(ScriptedProofs::with_mutants(vec![
        mutant(1, "x == 1"),
        mutant(2, "y == 2"),
    ])));
    team.mutation_check_tool(&worker_id)
        .await
        .expect("start the first mutation run");
    eventually(|| async {
        team.worker(&worker_id).await.ok().and_then(|worker| {
            let open: Vec<_> = worker
                .findings
                .iter()
                .filter(|finding| finding.id.starts_with("mutant-") && finding.status == "open")
                .collect();
            (open.len() == 2).then_some(())
        })
    })
    .await;
    // The first run's job still holds its claim while it records: wait for
    // the release, so the second run's claim is not refused.
    eventually(|| async {
        (!team.proof_ctx().proving.is_active(&worker_id)).then_some(())
    })
    .await;
    team.set_proof_runner_for_test(Arc::new(ScriptedProofs::with_mutants(vec![
        mutant(2, "y == 2"),
        mutant(3, "z == 3"),
    ])));
    team.mutation_check_tool(&worker_id)
        .await
        .expect("start the second mutation run");
    let worker = eventually(|| async {
        team.worker(&worker_id).await.ok().and_then(|worker| {
            let gone = worker.findings.iter().find(|finding| {
                finding.title == "No test catches: x == 1" && finding.status == "fixed"
            })?;
            let kept = worker.findings.iter().find(|finding| {
                finding.title == "No test catches: y == 2" && finding.status == "open"
            })?;
            let fresh = worker.findings.iter().find(|finding| {
                finding.title == "No test catches: z == 3" && finding.status == "open"
            })?;
            Some((gone.clone(), kept.clone(), fresh.clone()))
        })
    })
    .await;
    assert_eq!(
        worker.0.evidence.as_deref(),
        Some(format!("not reported at tip {tip}").as_str()),
        "the closed mutant names the tip it stopped being reported at"
    );
    assert!(
        worker.2.detail.lines().next() == Some(format!("tip {tip}").as_str()),
        "the new mutant's finding names its tip: {}",
        worker.2.detail
    );
    team.shutdown().await;
}

/// Cleanup skips a worker whose proof is running and says so with the proof's
/// refusal text; the checkout survives (finding cleanupDuringProof).
#[tokio::test]
async fn cleanup_skips_a_proving_worker() {
    let (_root, _state, team, worker_id) = proof_fixture("proveCleanup1").await;
    let worker = team.worker(&worker_id).await.expect("worker");
    // cleanup_workspace loads the ledger from disk, so the fixture's
    // Completed status must be persisted for the worker to reach the
    // cleanup eligibility checks.
    team.persist().await.expect("persist the ledger");
    assert!(team.proof_ctx().proving.hold_for_test(&worker_id));
    let report = team.cleanup().await.expect("cleanup runs");
    assert!(
        report
            .skipped
            .iter()
            .any(|entry| entry.contains(&proving::proving_error(&worker_id))),
        "the skipped worker is reported with the proving text: {:?}",
        report.skipped
    );
    assert!(
        worker.worktree.join("fix.rs").exists(),
        "the proved checkout is not removed"
    );
    team.shutdown().await;
}

/// The claim's second look: a job whose worker became busy under the claim
/// ends with the skip notice and never runs its proof (finding
/// engineTurnUnderClaim).
#[tokio::test]
async fn job_skips_when_worker_became_busy() {
    let (_root, _state, _config, team, worker_id) =
        scripted_proof_fixture("proveBusyMidJob1").await;
    let worker = team.worker(&worker_id).await.expect("worker");
    let proofs = ScriptedProofs::new(proof::RedOutcome::Red {
        output: "test regression_proves_red ... FAILED".to_string(),
    });
    let prove_calls = proofs.prove_calls.clone();
    team.set_proof_runner_for_test(Arc::new(proofs));
    // The scripted turn ends Completed, which triggers its owed proof: wait
    // for that one to finish and release its claim before the test holds
    // the claim and flips the busy flag.
    manual_pump_worker(&team, &worker, None, false).await;
    eventually(|| async {
        (prove_calls.load(Ordering::SeqCst) == 1).then_some(())
    })
    .await;
    eventually(|| async {
        (!team.proof_ctx().proving.is_active(&worker_id)).then_some(())
    })
    .await;
    assert!(team.proof_ctx().proving.hold_for_test(&worker_id));
    team.live
        .lock()
        .await
        .get(&worker_id)
        .expect("the live session")
        .busy
        .store(true, Ordering::SeqCst);
    let notice = team.proof_ctx().run_regression(&worker_id).await;
    assert!(
        notice.contains("skipped: the worker started a turn"),
        "the skip notice names what happened: {notice}"
    );
    assert_eq!(
        prove_calls.load(Ordering::SeqCst),
        1,
        "the busy job never ran its proof"
    );
    team.live
        .lock()
        .await
        .get(&worker_id)
        .expect("the live session")
        .busy
        .store(false, Ordering::SeqCst);
    team.shutdown().await;
}

/// A review whose findings list does not mention an open proof finding keeps
/// it: Mnemo2 generated it, and only the proof or an explicit resolution
/// closes it (finding reviewDropsProofFindings).
#[tokio::test]
async fn review_keeps_unmentioned_proof_findings() {
    let (_root, _state, team, worker_id) = proof_fixture("proveReviewKeep1").await;
    let worker = team.worker(&worker_id).await.expect("worker");
    let tip = phase3::tip(&worker.worktree).await.expect("tip");
    team.update_worker(&worker_id, |current| {
        current.findings.push(Finding {
            id: proving::NOT_RED_ID.to_string(),
            severity: "high".to_string(),
            title: "The regression test passes without the fix".to_string(),
            detail: format!("tip {tip}\nthe test does not reproduce the defect"),
            file: "fix.rs".to_string(),
            line: 0,
            status: "open".to_string(),
            fix_commit: None,
            evidence: None,
            justification: None,
            category: None,
            enforce: None,
        });
    })
    .await;
    team.review(
        &worker_id,
        "the lead's fresh review".into(),
        vec![open_finding("lead-finding")],
        evidence(),
    )
    .await
    .expect("the review records");
    let reviewed = team.worker(&worker_id).await.expect("worker");
    assert!(
        reviewed
            .findings
            .iter()
            .any(|finding| finding.id == "lead-finding"),
        "the lead's finding is recorded: {:?}",
        reviewed.findings
    );
    let kept = reviewed
        .findings
        .iter()
        .find(|finding| finding.id == proving::NOT_RED_ID)
        .expect("the unmentioned proof finding survives the review");
    assert_eq!(kept.status, "open", "the proof finding stays open");
    team.shutdown().await;
}

// --- The mutation-lifecycle delta fixes: mutantCloseOnEmpty, mutantCapClose,
// mutantMoreStale, mutantWontfixReopen ---

/// One scripted mutation round: run it and wait for its claim to drop, so the
/// next round's claim is never refused.
async fn run_mutation_round(
    team: &Arc<TeamRuntime>,
    worker_id: &str,
    mutants: Vec<proof::Mutant>,
) {
    team.set_proof_runner_for_test(Arc::new(ScriptedProofs::with_mutants(mutants)));
    team.mutation_check_tool(worker_id)
        .await
        .expect("start the mutation run");
    // `spawn` claims before it returns, so the release is what to wait for.
    eventually(|| async {
        (!team.proof_ctx().proving.is_active(worker_id)).then_some(())
    })
    .await;
}

/// A mutant the scripted run reports.
fn scripted_mutant(line: u32, text: &str) -> proof::Mutant {
    proof::Mutant {
        file: "src/fix.rs".to_string(),
        line,
        text: text.to_string(),
    }
}

/// A run that catches everything closes the previous run's mutant findings —
/// they are no longer reported — and records the clean check (findings
/// mutantCloseOnEmpty and mutantMoreStale).
#[tokio::test]
async fn empty_mutation_run_closes_previous_mutants() {
    let (_root, _state, team, worker_id) = proof_fixture("proveMutantEmpty1").await;
    let worker = team.worker(&worker_id).await.expect("worker");
    let tip = phase3::tip(&worker.worktree).await.expect("tip");
    run_mutation_round(
        &team,
        &worker_id,
        vec![scripted_mutant(1, "x == 1"), scripted_mutant(2, "y == 2")],
    )
    .await;
    run_mutation_round(&team, &worker_id, Vec::new()).await;
    let worker = eventually(|| async {
        team.worker(&worker_id).await.ok().and_then(|worker| {
            let closed = worker.findings.iter().filter(|finding| {
                finding.id.starts_with("mutant-") && finding.status == "fixed"
            });
            let clean = worker
                .checks
                .iter()
                .any(|check| check.name == proving::MUTATION_CLEAN_CHECK && check.status == "passed");
            (closed.count() == 2 && clean).then_some(worker)
        })
    })
    .await;
    for finding in worker
        .findings
        .iter()
        .filter(|finding| finding.id.starts_with("mutant-"))
    {
        assert_eq!(
            finding.evidence.as_deref(),
            Some(format!("not reported at tip {tip}").as_str()),
            "the closed mutant names the tip that caught it: {finding:?}"
        );
    }
    assert!(
        worker
            .checks
            .iter()
            .any(|check| check.name == proving::MUTATION_CLEAN_CHECK && check.status == "passed"),
        "the clean check is recorded"
    );
    team.shutdown().await;
}

/// A mutant still uncaught past the 20-finding cap is not closed: the reported
/// set is every mutant of the run, not the capped list (finding mutantCapClose).
#[tokio::test]
async fn mutant_past_cap_is_not_closed() {
    let (_root, _state, team, worker_id) = proof_fixture("proveMutantCap1").await;
    // Run 1: three mutants, A first, so its finding exists and is open.
    run_mutation_round(
        &team,
        &worker_id,
        vec![
            scripted_mutant(1, "a == 1"),
            scripted_mutant(2, "b == 2"),
            scripted_mutant(3, "c == 3"),
        ],
    )
    .await;
    let first = eventually(|| async {
        team.worker(&worker_id).await.ok().and_then(|worker| {
            worker
                .findings
                .iter()
                .find(|finding| finding.title == "No test catches: a == 1")
                .filter(|finding| finding.status == "open")
                .cloned()
        })
    })
    .await;
    // Run 2: 21 mutants with A LAST, so A is past the cap and gets no finding
    // of its own this run.
    let mut mutants: Vec<proof::Mutant> = (10..30)
        .map(|line| scripted_mutant(line, &format!("m{line} == {line}")))
        .collect();
    mutants.truncate(20);
    mutants.push(scripted_mutant(1, "a == 1"));
    assert_eq!(mutants.len(), 21);
    run_mutation_round(&team, &worker_id, mutants).await;
    let worker = team.worker(&worker_id).await.expect("worker");
    let after = worker
        .findings
        .iter()
        .find(|finding| finding.id == first.id)
        .expect("A's finding survives the second run");
    assert_eq!(
        after.status, "open",
        "a mutant past the cap is still uncaught: {after:?}"
    );
    assert!(
        worker
            .findings
            .iter()
            .any(|finding| finding.id == "mutant-more" && finding.status == "open"),
        "the overflowing run keeps its overflow finding: {:?}",
        worker.findings
    );
    team.shutdown().await;
}

/// The overflow marker closes when a run no longer overflows (finding
/// mutantMoreStale).
#[tokio::test]
async fn mutant_more_closes_when_overflow_ends() {
    let (_root, _state, team, worker_id) = proof_fixture("proveMutantMore1").await;
    let worker = team.worker(&worker_id).await.expect("worker");
    let tip = phase3::tip(&worker.worktree).await.expect("tip");
    let overflow: Vec<proof::Mutant> = (100..125)
        .map(|line| scripted_mutant(line, &format!("o{line} == {line}")))
        .collect();
    assert_eq!(overflow.len(), 25);
    run_mutation_round(&team, &worker_id, overflow).await;
    let marker = eventually(|| async {
        team.worker(&worker_id).await.ok().and_then(|worker| {
            worker
                .findings
                .iter()
                .find(|finding| finding.id == "mutant-more")
                .filter(|finding| finding.status == "open")
                .cloned()
        })
    })
    .await;
    assert!(
        marker.title.contains("5 more uncaught changes"),
        "{marker:?}"
    );
    // 15 mutants: no overflow any more.
    let fits: Vec<proof::Mutant> = (100..115)
        .map(|line| scripted_mutant(line, &format!("o{line} == {line}")))
        .collect();
    run_mutation_round(&team, &worker_id, fits).await;
    let closed = eventually(|| async {
        team.worker(&worker_id).await.ok().and_then(|worker| {
            worker
                .findings
                .iter()
                .find(|finding| finding.id == "mutant-more")
                .filter(|finding| finding.status == "fixed")
                .cloned()
        })
    })
    .await;
    assert_eq!(
        closed.evidence.as_deref(),
        Some(format!("not reported at tip {tip}").as_str()),
        "{closed:?}"
    );
    team.shutdown().await;
}

/// A mutant finding the lead resolved wontfix is never reopened by a later
/// run: the justification is about that code change, which its id names
/// (finding mutantWontfixReopen).
#[tokio::test]
async fn wontfix_mutant_survives_rerun() {
    let (_root, _state, team, worker_id) = proof_fixture("proveMutantWontfix1").await;
    run_mutation_round(&team, &worker_id, vec![scripted_mutant(1, "a == 1")]).await;
    let open = eventually(|| async {
        team.worker(&worker_id).await.ok().and_then(|worker| {
            worker
                .findings
                .iter()
                .find(|finding| {
                    finding.title == "No test catches: a == 1" && finding.status == "open"
                })
                .cloned()
        })
    })
    .await;
    team.update_worker(&worker_id, |current| {
        if let Some(finding) = current
            .findings
            .iter_mut()
            .find(|finding| finding.id == open.id)
        {
            finding.status = "wontfix".to_string();
            finding.justification = Some("the branch is unreachable under test".to_string());
        }
    })
    .await;
    // The same mutant reported again.
    run_mutation_round(&team, &worker_id, vec![scripted_mutant(1, "a == 1")]).await;
    let worker = team.worker(&worker_id).await.expect("worker");
    let after = worker
        .findings
        .iter()
        .find(|finding| finding.id == open.id)
        .expect("the finding survives");
    assert_eq!(after.status, "wontfix", "{after:?}");
    assert_eq!(
        after.justification.as_deref(),
        Some("the branch is unreachable under test"),
        "the lead's justification is untouched"
    );
    team.shutdown().await;
}
// --- The quiet mutation check: a run that cannot run is a notice, and a turn
// that owes nothing new starts no run (decision 2026-11-19) ---

/// The error a run against a machine without the mutation tool leaves: no
/// missed-mutant file, and the shell's own missing-command line.
const MISSING_MUTATION_TOOL: &str = "the mutation run wrote no missed-mutant file at \
     C:\\tmp\\missed.txt: the system cannot find the file specified\n\
     error: no such command: `mutants`";

/// A worker in `proof_fixture` that owes the mutation run: its review risk is
/// high, which is what triggers the automatic run.
async fn high_risk(team: &TeamRuntime, worker_id: &str) {
    team.update_worker(worker_id, |current| {
        current.review_risk = "high".to_string();
    })
    .await;
}

/// Every notice text already queued for the test.
fn notice_texts(
    receiver: &mut tokio::sync::broadcast::Receiver<BoardSignal>,
) -> Vec<String> {
    let mut texts = Vec::new();
    while let Ok(signal) = receiver.try_recv() {
        if let BoardSignal::Notice { text, .. } = signal {
            texts.push(text);
        }
    }
    texts
}

/// The first queued notice whose text contains `needle`. Every proof sends its
/// notice before it releases the claim, so a caller that waited for the claim
/// to end finds it here.
fn notice_with(
    receiver: &mut tokio::sync::broadcast::Receiver<BoardSignal>,
    needle: &str,
) -> Option<String> {
    notice_texts(receiver)
        .into_iter()
        .find(|text| text.contains(needle))
}

/// A missing mutation tool is a notice, not a worker error: the run could not
/// run, which says nothing about the worker (decision 2026-11-19). The
/// automatic run names it once per session; the lead's own request is always
/// answered, even about a tool already reported.
#[tokio::test]
async fn missing_mutation_tool_is_a_notice_not_a_worker_error() {
    let (_root, _state, team, worker_id) = proof_fixture("proveMutantMissing1").await;
    high_risk(&team, &worker_id).await;
    let proofs = ScriptedProofs::new(proof::RedOutcome::Red {
        output: "test regression_proves_red ... FAILED".to_string(),
    })
    .with_mutate_error(MISSING_MUTATION_TOOL.to_string());
    let mutate_calls = proofs.mutate_calls.clone();
    team.set_proof_runner_for_test(Arc::new(proofs));
    let mut notices = team.subscribe_board();
    proving::spawn_turn_proofs(&team.proof_ctx(), worker_id.clone()).await;
    eventually(|| async { (!team.proof_ctx().proving.is_active(&worker_id)).then_some(()) }).await;
    assert_eq!(mutate_calls.load(Ordering::SeqCst), 1);
    let notice = notice_with(&mut notices, "not installed")
        .expect("the lead is told the tool is missing");
    assert!(notice.contains("cargo install cargo-mutants"), "{notice}");
    assert!(
        notice.contains("no such command"),
        "the notice names the line that said so: {notice}"
    );
    let worker = team.worker(&worker_id).await.expect("worker");
    assert!(
        worker.error.is_none(),
        "a missing tool is not a worker error: {:?}",
        worker.error
    );
    assert!(
        worker.findings.is_empty(),
        "a run that could not run opens no finding: {:?}",
        worker.findings
    );

    // A new tip is worth a second automatic run, but the session already named
    // the missing tool: no second notice.
    let worker = team.worker(&worker_id).await.expect("worker");
    std::fs::write(worker.worktree.join("later.rs"), "fn later() {}\n").expect("source change");
    run_git(&worker.worktree, &["add", "later.rs"]);
    run_git(
        &worker.worktree,
        &["commit", "-m", "a later source change"],
    );
    proving::spawn_turn_proofs(&team.proof_ctx(), worker_id.clone()).await;
    eventually(|| async { (!team.proof_ctx().proving.is_active(&worker_id)).then_some(()) }).await;
    assert_eq!(
        mutate_calls.load(Ordering::SeqCst),
        2,
        "the new tip really reaches the runner"
    );
    let seen = notice_texts(&mut notices);
    assert!(
        seen.iter().all(|text| !text.contains("not installed")),
        "the automatic run repeats no missing-tool notice: {seen:?}"
    );

    // The lead's own request is answered even about a tool already reported.
    team.mutation_check_tool(&worker_id)
        .await
        .expect("the lead's own mutation run starts");
    eventually(|| async { (!team.proof_ctx().proving.is_active(&worker_id)).then_some(()) }).await;
    let notice = notice_with(&mut notices, "not installed")
        .expect("the lead's request is answered");
    assert!(notice.contains("cargo install cargo-mutants"), "{notice}");
    assert_eq!(mutate_calls.load(Ordering::SeqCst), 3);
    team.shutdown().await;
}

/// A mutation run that fails while leaving the checkout clean is a notice: the
/// worker is not at fault for a command that could not run (decision
/// 2026-11-19).
#[tokio::test]
async fn failed_mutation_run_with_clean_checkout_is_a_notice() {
    let (_root, _state, team, worker_id) = proof_fixture("proveMutantFail1").await;
    high_risk(&team, &worker_id).await;
    let proofs = ScriptedProofs::new(proof::RedOutcome::Red {
        output: "test regression_proves_red ... FAILED".to_string(),
    })
    .with_mutate_error(
        "the mutation run wrote no missed-mutant file at C:\\tmp\\missed.txt: \
         the process exited with code 2"
            .to_string(),
    );
    team.set_proof_runner_for_test(Arc::new(proofs));
    let mut notices = team.subscribe_board();
    proving::spawn_turn_proofs(&team.proof_ctx(), worker_id.clone()).await;
    eventually(|| async { (!team.proof_ctx().proving.is_active(&worker_id)).then_some(()) }).await;
    let notice = notice_with(&mut notices, "could not run").expect("the failed run is a notice");
    assert!(
        notice.contains("the mutation run wrote no missed-mutant file"),
        "{notice}"
    );
    let worker = team.worker(&worker_id).await.expect("worker");
    assert!(
        worker.error.is_none(),
        "a failed run with a clean checkout is not a worker error: {:?}",
        worker.error
    );
    team.shutdown().await;
}

/// A run that leaves the checkout dirty is still the worker's error: the
/// checkout itself is off its tip, which is what `fail` was written for.
#[tokio::test]
async fn mutation_run_leaving_a_dirty_checkout_still_fails_the_worker() {
    let (_root, _state, team, worker_id) = proof_fixture("proveMutantDirty1").await;
    high_risk(&team, &worker_id).await;
    let proofs = ScriptedProofs::new(proof::RedOutcome::Red {
        output: "test regression_proves_red ... FAILED".to_string(),
    })
    .with_dirty_mutate_error("the mutation run changed the checkout".to_string());
    team.set_proof_runner_for_test(Arc::new(proofs));
    proving::spawn_turn_proofs(&team.proof_ctx(), worker_id.clone()).await;
    let worker = eventually(|| async {
        team.worker(&worker_id)
            .await
            .ok()
            .and_then(|worker| worker.error.is_some().then_some(worker))
    })
    .await;
    assert_eq!(
        worker.error.as_deref(),
        Some("the mutation run changed the checkout")
    );
    team.shutdown().await;
}

/// A second turn end at the same tip starts no second mutation run: the session
/// already mutated that tip (decision 2026-11-19).
#[tokio::test]
async fn automatic_mutation_skips_an_unchanged_tip() {
    let (_root, _state, team, worker_id) = proof_fixture("proveMutantSameTip1").await;
    high_risk(&team, &worker_id).await;
    let proofs = ScriptedProofs::new(proof::RedOutcome::Red {
        output: "test regression_proves_red ... FAILED".to_string(),
    });
    let mutate_calls = proofs.mutate_calls.clone();
    team.set_proof_runner_for_test(Arc::new(proofs));
    proving::spawn_turn_proofs(&team.proof_ctx(), worker_id.clone()).await;
    eventually(|| async { (mutate_calls.load(Ordering::SeqCst) == 1).then_some(()) }).await;
    eventually(|| async { (!team.proof_ctx().proving.is_active(&worker_id)).then_some(()) }).await;

    proving::spawn_turn_proofs(&team.proof_ctx(), worker_id.clone()).await;
    // A second run would reach the runner well inside this window; the tip it
    // would mutate is the one the session already did, so none may start.
    for _ in 0..160 {
        if mutate_calls.load(Ordering::SeqCst) != 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert_eq!(
        mutate_calls.load(Ordering::SeqCst),
        1,
        "the tip was already mutated in this session"
    );
    let worker = team.worker(&worker_id).await.expect("worker");
    assert!(worker.error.is_none(), "{:?}", worker.error);
    team.shutdown().await;
}

/// A change of only test files starts no automatic mutation run: there is no
/// new code for the tool to change. The lead's own request still runs
/// (decision 2026-11-19).
#[tokio::test]
async fn automatic_mutation_skips_a_test_only_change() {
    let (_root, _state, team, worker_id) =
        proof_fixture_with_change("proveMutantTests1", "tests/x.rs").await;
    high_risk(&team, &worker_id).await;
    let proofs = ScriptedProofs::new(proof::RedOutcome::Red {
        output: "test regression_proves_red ... FAILED".to_string(),
    });
    let mutate_calls = proofs.mutate_calls.clone();
    team.set_proof_runner_for_test(Arc::new(proofs));
    proving::spawn_turn_proofs(&team.proof_ctx(), worker_id.clone()).await;
    eventually(|| async { (!team.proof_ctx().proving.is_active(&worker_id)).then_some(()) }).await;
    assert_eq!(
        mutate_calls.load(Ordering::SeqCst),
        0,
        "a test-only change is not mutated"
    );
    team.mutation_check_tool(&worker_id)
        .await
        .expect("the lead asked for the run");
    eventually(|| async { (mutate_calls.load(Ordering::SeqCst) == 1).then_some(()) }).await;
    team.shutdown().await;
}

/// The instances summary's counts-only accessor agrees with the snapshot
/// counts it replaced, and it counts without cloning transcripts.
#[tokio::test]
async fn activity_counts_match_the_snapshot_counts() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    run_git(root.path(), &["checkout", "-b", "wt/mnemo"]);
    let team = open_test_team(root.path(), state.path(), Vec::new()).await;
    {
        let mut records = team.records.lock().await;
        for (index, status) in [
            WorkerStatus::Working,
            WorkerStatus::Dispatching,
            WorkerStatus::Fixing,
            WorkerStatus::Merging,
            WorkerStatus::NeedsYou,
            WorkerStatus::Conflict,
            WorkerStatus::Idle,
            WorkerStatus::Completed,
        ]
        .into_iter()
        .enumerate()
        {
            let mut record = worker(root.path(), &format!("actcount{index:04}"));
            record.status = status;
            records.push(record);
        }
        // Two finished audits (their reports delivered) never wait; one audit
        // that failed before reporting still needs its resume.
        for (index, status, delivered) in [
            (0, WorkerStatus::Completed, true),
            (1, WorkerStatus::Completed, true),
            (2, WorkerStatus::Failed, false),
        ] {
            let mut record = worker(root.path(), &format!("actaudit{index:04}"));
            record.kind = "audit".to_string();
            record.status = status;
            record.report_delivered = delivered;
            records.push(record);
        }
    }

    let counts = team.activity_counts().await;
    let snapshot = team.snapshot().await;
    let from_snapshot = (
        snapshot
            .workers
            .iter()
            .filter(|worker| worker.status.is_running())
            .count(),
        snapshot
            .workers
            .iter()
            .filter(|worker| worker.waiting_on_user())
            .count(),
    );
    assert_eq!(counts, from_snapshot, "the accessor and the snapshot agree");
    assert_eq!(
        counts,
        (4, 4),
        "working, dispatching, fixing and merging run; needs-you, conflict, completed and the \
         unreported audit wait, while the two delivered audits do not"
    );
    team.shutdown().await;
}

/// A finished audit is never reviewed or merged, so it stays `Completed`
/// forever: it must not count as waiting on the person (the Mission Control
/// instances list reported eight waiting workers with nothing waiting on the
/// user — all of them completed audits).
#[tokio::test]
async fn completed_audits_do_not_count_as_waiting() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    run_git(root.path(), &["checkout", "-b", "wt/mnemo"]);
    let team = open_test_team(root.path(), state.path(), Vec::new()).await;
    {
        let mut records = team.records.lock().await;
        let mut implementation = worker(root.path(), "waitimpl01");
        implementation.status = WorkerStatus::Completed;
        records.push(implementation);
        for index in 0..2 {
            let mut audit = worker(root.path(), &format!("waitaudit{index:02}"));
            audit.kind = "audit".to_string();
            audit.status = WorkerStatus::Completed;
            audit.report_delivered = true;
            records.push(audit);
        }
    }

    let (running, waiting) = team.activity_counts().await;
    assert_eq!(running, 0, "nothing is running");
    assert_eq!(
        waiting, 1,
        "only the implementation awaiting review waits; the two finished audits do not"
    );
    let snapshot = team.snapshot().await;
    assert_eq!(
        snapshot
            .workers
            .iter()
            .filter(|worker| worker.waiting_on_user())
            .count(),
        waiting,
        "the snapshot counts the same"
    );
    // An audit that failed before delivering its report still needs a resume.
    {
        let mut records = team.records.lock().await;
        let mut failed = worker(root.path(), "waitfaile01");
        failed.kind = "audit".to_string();
        failed.status = WorkerStatus::Failed;
        records.push(failed);
    }
    assert_eq!(
        team.activity_counts().await.1,
        2,
        "an unreported failed audit waits for its resume"
    );
    team.shutdown().await;
}

// ---------------------------------------------------------------------------
// Claude Code agent tiers (plan e21c4820): a tier may run as the lead's own
// Claude Code agent, with team_agent_done closing the worker.
// ---------------------------------------------------------------------------

/// The routing fixture with the high tier set to run as a Claude Code agent
/// (`claude_model`): a dispatch that settles on high from the external lead
/// hands the step to the lead's agent instead of a Mnemo session.
async fn agent_fixture(
    mode: RoutingMode,
    answer: Option<mnemo::memory::Answer>,
    install_classifier: bool,
    claude_model: &str,
    high_models: Vec<String>,
) -> RoutingFixture {
    agent_fixture_with_settings(
        crate::mnemo2::dispatch_routing::RoutingSettings {
            mode,
            ..crate::mnemo2::dispatch_routing::RoutingSettings::default()
        },
        answer,
        install_classifier,
        claude_model,
        high_models,
    )
    .await
}

/// The agent fixture with explicit routing settings, for the Auto fallback.
async fn agent_fixture_with_settings(
    settings: crate::mnemo2::dispatch_routing::RoutingSettings,
    answer: Option<mnemo::memory::Answer>,
    install_classifier: bool,
    claude_model: &str,
    high_models: Vec<String>,
) -> RoutingFixture {
    let fixture = routing_fixture_with(
        settings,
        answer,
        install_classifier,
        std::time::Duration::ZERO,
    )
    .await;
    let mut dto = tiers_dto! {
        difficult: high_models,
        average: vec!["test/scripted-average".into()],
        simple: vec!["test/scripted-simple".into()],
    };
    dto.tiers[3].claude_agent = Some(claude_model.into());
    fixture.team.set_catalog(
        vec![
            lead_model("scripted-difficult"),
            lead_model("scripted-average"),
            lead_model("scripted-simple"),
        ],
        dto,
        Vec::new(),
    );
    fixture
}

/// An explicit dispatch on the high tier from the external lead, the way
/// Claude Code's `team_dispatch` lands.
fn agent_dispatch_request(plan_id: &str, brief: &str, worker_id: &str) -> DispatchRequest {
    DispatchRequest {
        brief: brief.into(),
        tier: Some("high".into()),
        model: None,
        reasoning_effort: None,
        worker_id: Some(worker_id.into()),
        review_risk: Some("standard".into()),
        source: DispatchSource::LeadTool,
        sender: MessageSender::ExternalLead,
        dispatch_routing: true,
        plan: Some(PlanRef {
            plan_id: plan_id.into(),
            step: 0,
        }),
        read_back: false,
    }
}

#[tokio::test]
async fn agent_tier_dispatch_prepares_the_checkout_without_a_session() {
    let fixture = agent_fixture(
        RoutingMode::Off,
        None,
        false,
        "opus",
        vec!["test/scripted-difficult".into()],
    )
    .await;
    assert!(fixture.team.attach_external_lead());
    let brief = "Add the per-tier claude_agent handling";
    let (record, reused, _pending) = fixture
        .team
        .dispatch_request(agent_dispatch_request(&fixture.plan_id, brief, "agenttier01"))
        .await
        .expect("the agent dispatch prepares the step");

    // Provisioning ran as today: a real worktree on the agent branch.
    assert!(!reused);
    assert!(std::path::Path::new(&record.worktree).is_dir());
    assert_eq!(record.branch, agent_branch("agenttier01"));
    // The record claims the agent model and the settled pre-fold tier.
    assert_eq!(record.claude_agent.as_deref(), Some("opus"));
    assert_eq!(record.tier.as_deref(), Some("high"));
    assert_eq!(record.status, WorkerStatus::Working);
    // No Mnemo session was started for the agent worker.
    assert!(
        fixture.team.live.lock().await.get("agenttier01").is_none(),
        "an agent worker has no Mnemo session"
    );

    // The block the lead hands its own Agent: the model, the prepared
    // checkout, and the full self-contained prompt.
    let block = claude_agent_block(&record).expect("the dispatch answer's claude_agent block");
    let prompt = block["prompt"].as_str().expect("the prompt text");
    assert_eq!(block["model"].as_str(), Some("opus"));
    assert_eq!(
        block["worktree"].as_str().map(str::to_string),
        Some(record.worktree.display().to_string())
    );
    assert_eq!(
        block["branch"].as_str().map(str::to_string),
        Some(record.branch.clone())
    );
    assert!(prompt.contains(record.worktree.to_str().expect("a utf-8 worktree path")), "the prompt names the worktree: {prompt}");
    assert!(prompt.contains(&record.branch), "the prompt names the branch: {prompt}");
    assert!(prompt.contains("Never push"), "{prompt}");
    assert!(prompt.contains("git stash"), "{prompt}");
    assert!(prompt.contains(".coding/checks.toml"), "{prompt}");
    assert!(
        prompt.contains(
            "Implement the requested routing behavior in src-tauri/src/mnemo2/team.rs"
        ),
        "the prompt carries the step text: {prompt}"
    );
    assert!(prompt.contains(brief), "the prompt carries the brief: {prompt}");

    // The block rides both halves of the dispatch answer, and team_status
    // shows the worker as a Claude Code agent.
    let answer = dispatch_result(
        Ok((record.clone(), false, false)),
        "team_dispatch",
        RoutingMode::Off,
        None,
        false,
        Vec::new(),
    );
    let result: Value =
        serde_json::from_str(&answer.output).expect("the dispatch answer JSON");
    assert_eq!(
        result["result"]["claude_agent"]["model"].as_str(),
        Some("opus"),
        "{result}"
    );
    assert_eq!(
        answer.data.as_ref().unwrap()["claude_agent"]["prompt"].as_str(),
        block["prompt"].as_str(),
        "data carries the same block"
    );
    let view = worker_status_view(&record, false);
    assert_eq!(view["model"].as_str(), Some("claude-code/opus"), "{view}");

    // An idempotent re-dispatch returns the same agent block.
    let (again, reused, _) = fixture
        .team
        .dispatch_request(agent_dispatch_request(&fixture.plan_id, brief, "agenttier01"))
        .await
        .expect("the re-dispatch reuses the worker");
    assert!(reused);
    assert_eq!(again.claude_agent.as_deref(), Some("opus"));
    let again_block = claude_agent_block(&again).expect("the reused agent block");
    assert_eq!(
        again_block["prompt"].as_str(),
        block["prompt"].as_str(),
        "the reuse returns the same prompt"
    );
}

#[tokio::test]
async fn agent_tier_without_models_is_still_an_agent_tier() {
    let fixture = agent_fixture(RoutingMode::Off, None, false, "sonnet", vec![]).await;
    assert!(fixture.team.attach_external_lead());
    let (record, _, _) = fixture
        .team
        .dispatch_request(agent_dispatch_request(
            &fixture.plan_id,
            "hand the empty tier to the agent",
            "agenttier02",
        ))
        .await
        .expect("the empty agent tier still goes to the agent");
    // The pre-fold settled tier claims the agent before fold-down moves the
    // dispatch onto a lower tier's model.
    assert_eq!(record.claude_agent.as_deref(), Some("sonnet"));
    assert_eq!(record.tier.as_deref(), Some("high"));
    assert!(fixture.team.live.lock().await.get("agenttier02").is_none());
    // The routed model still names the fold target; the status view shows
    // the Claude Code agent.
    assert_eq!(record.model.as_deref(), Some("test/scripted-average"));
    assert_eq!(
        worker_status_view(&record, false)["model"].as_str(),
        Some("claude-code/sonnet")
    );
}

#[tokio::test]
async fn agent_tier_with_an_explicit_model_still_starts_a_mnemo_worker() {
    let fixture = agent_fixture(
        RoutingMode::Off,
        None,
        false,
        "opus",
        vec!["test/scripted-difficult".into()],
    )
    .await;
    assert!(fixture.team.attach_external_lead());
    let mut request = agent_dispatch_request(
        &fixture.plan_id,
        "the lead explicitly chose this Mnemo model",
        "agenttier11",
    );
    request.model = Some("test/scripted-difficult".into());
    let (record, _, _) = fixture
        .team
        .dispatch_request(request)
        .await
        .expect("the explicit-model dispatch");
    assert!(record.claude_agent.is_none());
    assert_eq!(record.model.as_deref(), Some("test/scripted-difficult"));
    assert!(fixture.team.live.lock().await.get("agenttier11").is_some());
}

#[tokio::test]
async fn agent_tier_is_ignored_without_an_external_lead() {
    // No external lead attached: the in-app lead's dispatch on the agent
    // tier starts a normal Mnemo worker on the tier's models.
    let fixture = agent_fixture(
        RoutingMode::Off,
        None,
        false,
        "opus",
        vec!["test/scripted-difficult".into()],
    )
    .await;
    let mut request = agent_dispatch_request(&fixture.plan_id, "in-app brief", "agenttier03");
    request.sender = MessageSender::Lead;
    let (record, _, _) = fixture
        .team
        .dispatch_request(request)
        .await
        .expect("the in-app dispatch starts a normal worker");
    assert!(
        record.claude_agent.is_none(),
        "without an external lead the tier resolves exactly as today"
    );
    assert_eq!(record.model.as_deref(), Some("test/scripted-difficult"));
    assert!(
        fixture.team.live.lock().await.get("agenttier03").is_some(),
        "a normal Mnemo session started"
    );

    // An attached external lead does not claim the in-app lead's dispatch:
    // the sender, not the attach state alone, decides.
    let fixture = agent_fixture(
        RoutingMode::Off,
        None,
        false,
        "opus",
        vec!["test/scripted-difficult".into()],
    )
    .await;
    assert!(fixture.team.attach_external_lead());
    let mut request =
        agent_dispatch_request(&fixture.plan_id, "in-app brief with the lead attached", "agenttier04");
    request.sender = MessageSender::Lead;
    let (record, _, _) = fixture
        .team
        .dispatch_request(request)
        .await
        .expect("the in-app dispatch with the lead attached");
    assert!(record.claude_agent.is_none());
    assert!(fixture.team.live.lock().await.get("agenttier04").is_some());

    // IPC dispatches (the Control Room form) never take the agent path,
    // even when Laya clears the agent tier while a lead is attached.
    let fixture = agent_fixture(
        RoutingMode::Auto,
        routing_answer(mnemo::agent::route_question::LABEL_HARD, 0.93),
        true,
        "opus",
        vec!["test/scripted-difficult".into()],
    )
    .await;
    assert!(fixture.team.attach_external_lead());
    let (record, _) = fixture
        .team
        .dispatch_on_plan(
            "the control-room brief with enough implementation detail for Laya to classify this step".into(),
            None,
            None,
            Some("agenttier05".into()),
            None,
            false,
            &fixture.plan_id,
            1,
        )
        .await
        .expect("the IPC dispatch");
    assert!(
        record.claude_agent.is_none(),
        "IPC dispatches never take the agent path"
    );
    assert_eq!(
        record.model.as_deref(),
        Some("test/scripted-difficult"),
        "Laya cleared high and the worker runs on its model"
    );
    assert!(fixture.team.live.lock().await.get("agenttier05").is_some());
}

#[tokio::test]
async fn laya_auto_into_an_agent_tier_hands_it_to_the_lead() {
    let fixture = agent_fixture(
        RoutingMode::Auto,
        routing_answer(mnemo::agent::route_question::LABEL_HARD, 0.93),
        true,
        "opus",
        vec!["test/scripted-difficult".into()],
    )
    .await;
    assert!(fixture.team.attach_external_lead());
    let request = DispatchRequest {
        brief: "auto-routed brief with enough implementation detail for Laya to classify this step".into(),
        tier: None,
        model: None,
        reasoning_effort: None,
        worker_id: Some("layaagnt01".into()),
        review_risk: Some("standard".into()),
        source: DispatchSource::LeadTool,
        sender: MessageSender::ExternalLead,
        dispatch_routing: true,
        plan: Some(PlanRef {
            plan_id: fixture.plan_id.clone(),
            step: 0,
        }),
        read_back: false,
    };
    let (record, _, _) = fixture
        .team
        .dispatch_request(request)
        .await
        .expect("the auto dispatch");
    assert_eq!(
        record.claude_agent.as_deref(),
        Some("opus"),
        "Laya's cleared tier hands the step to the lead's agent: {record:?}"
    );
    assert_eq!(record.tier.as_deref(), Some("high"));
    assert!(fixture.team.live.lock().await.get("layaagnt01").is_none());
}

#[tokio::test]
async fn auto_fallback_into_an_agent_tier_hands_it_to_the_lead() {
    let fixture = agent_fixture_with_settings(
        crate::mnemo2::dispatch_routing::RoutingSettings {
            mode: RoutingMode::Auto,
            fallback_tier: "high".into(),
            ..crate::mnemo2::dispatch_routing::RoutingSettings::default()
        },
        None,
        false,
        "fable",
        vec!["test/scripted-difficult".into()],
    )
    .await;
    assert!(fixture.team.attach_external_lead());
    let mut request = agent_dispatch_request(
        &fixture.plan_id,
        "Auto has no cleared Laya decision, so its fallback tier settles the dispatch",
        "agenttier12",
    );
    request.tier = None;
    let (record, _, _) = fixture
        .team
        .dispatch_request(request)
        .await
        .expect("the Auto fallback dispatch");
    assert_eq!(record.claude_agent.as_deref(), Some("fable"));
    assert_eq!(record.tier.as_deref(), Some("high"));
    assert!(fixture.team.live.lock().await.get("agenttier12").is_none());
}

#[tokio::test]
async fn team_agent_done_finishes_the_worker_and_notifies_the_lead() {
    let fixture = agent_fixture(
        RoutingMode::Off,
        None,
        false,
        "opus",
        vec!["test/scripted-difficult".into()],
    )
    .await;
    // The merge lands on the lead branch, not main.
    run_git(fixture._root.path(), &["checkout", "-b", "wt/mnemo"]);
    assert!(fixture.team.attach_external_lead());
    let (record, _, _) = fixture
        .team
        .dispatch_request(agent_dispatch_request(
            &fixture.plan_id,
            "implement the agent-tier handling",
            "agenttier06",
        ))
        .await
        .expect("the agent dispatch");
    let worktree = std::path::PathBuf::from(&record.worktree);

    // The lead's own agent commits in the prepared worktree.
    std::fs::write(worktree.join("agent-work.txt"), "done\n").expect("write in the worktree");
    run_git(&worktree, &["add", "."]);
    run_git(&worktree, &["commit", "-m", "the agent's work"]);

    // With no live entry, the session-free agent must still be closed by
    // team_agent_done before checks, review or merge can start.
    let unfinished =
        "agenttier06 runs as a Claude Code agent: call team_agent_done when it has finished";
    let error = fixture
        .team
        .record_check(
            "agenttier06",
            "cargo test --workspace".into(),
            "passed".into(),
            None,
        )
        .await
        .expect_err("an unfinished agent worker cannot be checked");
    assert_eq!(error, unfinished);
    let error = fixture
        .team
        .review(
            "agenttier06",
            "this review is premature".into(),
            Vec::new(),
            evidence(),
        )
        .await
        .expect_err("an unfinished agent worker cannot be reviewed");
    assert_eq!(error, unfinished);
    let error = fixture
        .team
        .merge("agenttier06")
        .await
        .expect_err("an unfinished agent worker cannot be merged");
    assert_eq!(error, unfinished);

    // team_agent_done records the report, completes the worker and bumps
    // the turn as the pump's Finished handling does.
    let report = "Committed the change; cargo test --workspace and clippy pass.";
    let done = fixture
        .team
        .agent_done("agenttier06", report.to_string(), false)
        .await
        .expect("finish the agent worker");
    assert_eq!(done.status, WorkerStatus::Completed);
    assert_eq!(done.turns, 1);
    assert_eq!(done.report.as_deref(), Some(report));

    // The finished notice reaches the external lead's queue, and survives
    // current_notices exactly as a session's tagged notice does.
    let notices = fixture
        .team
        .current_notices(fixture.team.drain_external_notices())
        .await;
    let notice = notices
        .iter()
        .find(|notice| notice.contains("agenttier06"))
        .expect("the finished notice reached the external lead");
    assert!(notice.contains("finished its turn"), "{notice}");
    assert!(notice.contains("Review it now"), "{notice}");
    assert!(
        notice.ends_with(
            "Use team_status with this exact worker_id. Fix findings through your own agent (SendMessage to it); team_stop clears the worker. Never infer lifecycle from Git, files, timestamps, or team.json."
        ),
        "{notice}"
    );

    // team_check, team_review and team_merge then work as for any worker.
    fixture
        .team
        .record_check(
            "agenttier06",
            "cargo test --workspace".into(),
            "passed".into(),
            None,
        )
        .await
        .expect("record the check");
    fixture
        .team
        .review("agenttier06", "the agent's work is good".into(), Vec::new(), evidence())
        .await
        .expect("review the agent's work");
    let merged = fixture
        .team
        .merge("agenttier06")
        .await
        .expect("merge the agent worker");
    assert_eq!(merged.status, WorkerStatus::Merged);
}

#[tokio::test]
async fn team_agent_done_failed_marks_failed() {
    let fixture = agent_fixture(
        RoutingMode::Off,
        None,
        false,
        "opus",
        vec!["test/scripted-difficult".into()],
    )
    .await;
    assert!(fixture.team.attach_external_lead());
    fixture
        .team
        .dispatch_request(agent_dispatch_request(
            &fixture.plan_id,
            "implement the agent-tier handling",
            "agenttier07",
        ))
        .await
        .expect("the agent dispatch");
    // Reopen recovery can leave an agent worker Interrupted; the lead can
    // still close it with team_agent_done.
    fixture
        .team
        .update_worker("agenttier07", |worker| {
            worker.status = WorkerStatus::Interrupted;
        })
        .await;
    let report = "the agent hit a failing build it could not fix";
    let failed = fixture
        .team
        .agent_done("agenttier07", report.to_string(), true)
        .await
        .expect("mark the agent worker failed");
    assert_eq!(failed.status, WorkerStatus::Failed);
    assert_eq!(failed.turns, 1);
    assert_eq!(failed.report.as_deref(), Some(report));

    // The failure notice is failure-shaped: it names the failure, quotes the
    // report, and never tells the lead to review now.
    let notices = fixture
        .team
        .current_notices(fixture.team.drain_external_notices())
        .await;
    let notice = notices
        .iter()
        .find(|notice| notice.contains("agenttier07"))
        .expect("the failed notice reached the external lead");
    assert!(notice.contains("could not finish its turn"), "{notice}");
    assert!(notice.contains(report), "{notice}");
    assert!(!notice.contains("Review it now"), "{notice}");
}

#[tokio::test]
async fn team_agent_done_refuses_a_mnemo_worker() {
    // A worker with a Mnemo session is never an agent worker.
    let fixture = agent_fixture(
        RoutingMode::Off,
        None,
        false,
        "opus",
        vec!["test/scripted-difficult".into()],
    )
    .await;
    let mut request = agent_dispatch_request(&fixture.plan_id, "a normal Mnemo worker", "agenttier08");
    request.sender = MessageSender::Lead;
    fixture
        .team
        .dispatch_request(request)
        .await
        .expect("a normal worker dispatch");
    let error = fixture
        .team
        .agent_done("agenttier08", "report".to_string(), false)
        .await
        .expect_err("a Mnemo worker is not an agent worker");
    assert!(
        error.contains("does not run as a Claude Code agent"),
        "{error}"
    );

    // And an agent worker outside Working/Interrupted is refused with its state.
    let fixture = agent_fixture(
        RoutingMode::Off,
        None,
        false,
        "opus",
        vec!["test/scripted-difficult".into()],
    )
    .await;
    assert!(fixture.team.attach_external_lead());
    fixture
        .team
        .dispatch_request(agent_dispatch_request(
            &fixture.plan_id,
            "implement the agent-tier handling",
            "agenttier09",
        ))
        .await
        .expect("the agent dispatch");
    fixture
        .team
        .agent_done("agenttier09", "done".to_string(), false)
        .await
        .expect("finish the agent worker");
    let error = fixture
        .team
        .agent_done("agenttier09", "again".to_string(), false)
        .await
        .expect_err("an already-completed worker is refused");
    assert!(error.contains("completed"), "{error}");
}

#[tokio::test]
async fn session_tools_refuse_an_agent_worker() {
    let fixture = agent_fixture(
        RoutingMode::Off,
        None,
        false,
        "opus",
        vec!["test/scripted-difficult".into()],
    )
    .await;
    assert!(fixture.team.attach_external_lead());
    fixture
        .team
        .dispatch_request(agent_dispatch_request(
            &fixture.plan_id,
            "implement the agent-tier handling",
            "agenttier10",
        ))
        .await
        .expect("the agent dispatch");

    // Every tool that would drive a Mnemo session refuses, pointing at the
    // lead's own agent instead.
    for refusal in [
        fixture
            .team
            .message("agenttier10", "status?".into(), MessageSender::ExternalLead)
            .await
            .expect_err("team_message refuses"),
        fixture
            .team
            .resume("agenttier10")
            .await
            .expect_err("team_resume refuses"),
        fixture
            .team
            .change_model("agenttier10", Some("test/scripted-average".into()), None, ChangeSource::Lead)
            .await
            .expect_err("team_resume with a model refuses"),
        fixture
            .team
            .compact("agenttier10")
            .await
            .expect_err("team_compact refuses"),
        fixture
            .team
            .route_fix_with_tier("agenttier10", "finding1", None)
            .await
            .expect_err("team_route_fix refuses"),
    ] {
        assert_eq!(
            refusal,
            "agenttier10 runs as a Claude Code agent: continue it with your own Agent (SendMessage to it), then call team_agent_done."
        );
    }

    fixture
        .team
        .agent_done("agenttier10", "finished".into(), false)
        .await
        .expect("finish the agent worker");
    let refusal = fixture
        .team
        .message("agenttier10", "fix the finding".into(), MessageSender::ExternalLead)
        .await
        .expect_err("a finished agent worker is fixed through the lead's agent");
    assert_eq!(
        refusal,
        "agenttier10 runs as a Claude Code agent: fix it with your own Agent (SendMessage to it), then run team_check and team_review on the new tip."
    );

    // team_stop still works: the lead cancels the record itself.
    let stopped = fixture
        .team
        .stop("agenttier10")
        .await
        .expect("team_stop still works on an agent worker");
    assert_eq!(stopped.status, WorkerStatus::Stopped);
    let refusal = fixture
        .team
        .resume("agenttier10")
        .await
        .expect_err("an ended agent worker cannot be resumed");
    assert_eq!(
        refusal,
        "agenttier10 runs as a Claude Code agent and has ended: dispatch the step again with a new worker_id."
    );
}

#[tokio::test]
async fn tier_guidance_names_the_claude_agent() {
    let fixture = agent_fixture(
        RoutingMode::Off,
        None,
        false,
        "opus",
        vec!["test/scripted-difficult".into()],
    )
    .await;
    let guidance = fixture.team.tier_guidance().expect("tier guidance");
    let line = guidance
        .lines()
        .find(|line| line.starts_with("high "))
        .expect("the high line");
    assert!(
        line.ends_with("; runs as a Claude Code agent (opus) when Claude Code leads"),
        "{line}"
    );

    // An agent tier with no models still appears, naming the fold target as
    // today, followed by the same suffix.
    let fixture = agent_fixture(RoutingMode::Off, None, false, "haiku", vec![]).await;
    let guidance = fixture.team.tier_guidance().expect("tier guidance");
    let line = guidance
        .lines()
        .find(|line| line.starts_with("high "))
        .expect("the high line");
    assert!(line.contains("no model; folds down to routine"), "{line}");
    assert!(
        line.ends_with("; runs as a Claude Code agent (haiku) when Claude Code leads"),
        "{line}"
    );
}

/// The agent fixture with the high tier's Claude Code effort set too.
async fn agent_fixture_with_effort(
    claude_model: &str,
    effort: &str,
    high_models: Vec<String>,
) -> RoutingFixture {
    let fixture = agent_fixture(RoutingMode::Off, None, false, claude_model, high_models).await;
    let mut dto = mnemo::headless::TiersDto::from(fixture.team.tier_ladder());
    dto.tiers[3].claude_agent_effort = Some(effort.into());
    fixture.team.set_tiers(dto);
    fixture
}

#[tokio::test]
async fn agent_dispatch_carries_the_tier_effort() {
    let fixture = agent_fixture_with_effort("opus", "xhigh", vec!["test/scripted-difficult".into()])
        .await;
    assert!(fixture.team.attach_external_lead());
    let request = agent_dispatch_request(
        &fixture.plan_id,
        "run the step at the tier's effort",
        "agenttier14",
    );
    let (record, _, _) = fixture
        .team
        .dispatch_request(request)
        .await
        .expect("the agent dispatch");
    let block = claude_agent_block(&record).expect("the agent block");
    assert_eq!(block["effort"], "xhigh", "{block}");

    // An idempotent re-dispatch returns the same block.
    let request = agent_dispatch_request(
        &fixture.plan_id,
        "run the step at the tier's effort",
        "agenttier14",
    );
    let (retry, reused, _) = fixture
        .team
        .dispatch_request(request)
        .await
        .expect("the same-id retry");
    assert!(reused);
    assert_eq!(block, claude_agent_block(&retry).expect("the retry block"));

    // A tier without an effort carries null.
    let fixture = agent_fixture(
        RoutingMode::Off,
        None,
        false,
        "opus",
        vec!["test/scripted-difficult".into()],
    )
    .await;
    assert!(fixture.team.attach_external_lead());
    let request = agent_dispatch_request(
        &fixture.plan_id,
        "run the step without an effort",
        "agenttier15",
    );
    let (record, _, _) = fixture
        .team
        .dispatch_request(request)
        .await
        .expect("the agent dispatch");
    let block = claude_agent_block(&record).expect("the agent block");
    assert!(block["effort"].is_null(), "{block}");
}

#[tokio::test]
async fn tier_guidance_names_the_agent_effort() {
    let fixture = agent_fixture_with_effort("opus", "xhigh", vec!["test/scripted-difficult".into()])
        .await;
    let guidance = fixture.team.tier_guidance().expect("tier guidance");
    let line = guidance
        .lines()
        .find(|line| line.starts_with("high "))
        .expect("the high line");
    assert!(
        line.ends_with("; runs as a Claude Code agent (opus, xhigh effort) when Claude Code leads"),
        "{line}"
    );

    // Without an effort the suffix stays exactly as it was.
    let fixture = agent_fixture(
        RoutingMode::Off,
        None,
        false,
        "opus",
        vec!["test/scripted-difficult".into()],
    )
    .await;
    let guidance = fixture.team.tier_guidance().expect("tier guidance");
    let line = guidance
        .lines()
        .find(|line| line.starts_with("high "))
        .expect("the high line");
    assert!(
        line.ends_with("; runs as a Claude Code agent (opus) when Claude Code leads"),
        "{line}"
    );
}


#[tokio::test]
async fn agent_dispatch_read_back_uses_the_updated_record_and_reuses_the_same_block() {
    let fixture = agent_fixture(
        RoutingMode::Auto,
        None,
        false,
        "opus",
        vec!["test/scripted-difficult".into()],
    )
    .await;
    assert!(fixture.team.attach_external_lead());
    let mut request = agent_dispatch_request(
        &fixture.plan_id,
        "read the step back before implementing it",
        "agenttier13",
    );
    request.read_back = true;
    let (record, _, _) = fixture
        .team
        .dispatch_request(request)
        .await
        .expect("the read-back agent dispatch");
    let first = claude_agent_block(&record).expect("the first agent block");
    assert!(record.read_back, "the returned record keeps read_back: {record:?}");
    assert!(record.routing.is_some(), "the returned record keeps routing");
    assert!(
        first["prompt"]
            .as_str()
            .is_some_and(|prompt| prompt.starts_with(super::record::AGENT_READ_BACK_KICKOFF)),
        "the prompt uses the READ_BACK kickoff"
    );

    let mut retry_request = agent_dispatch_request(
        &fixture.plan_id,
        "read the step back before implementing it",
        "agenttier13",
    );
    retry_request.read_back = true;
    let (retry, reused, _) = fixture
        .team
        .dispatch_request(retry_request)
        .await
        .expect("the same-id retry");
    assert!(reused);
    assert_eq!(first, claude_agent_block(&retry).expect("the retry block"));

    let answer = dispatch_result(
        Ok((record, false, false)),
        "team_dispatch",
        RoutingMode::Auto,
        None,
        false,
        Vec::new(),
    );
    let data = answer.data.expect("dispatch data");
    assert_eq!(data["worker"]["read_back"], true);
    assert!(data["worker"]["routing"].is_object());
}

#[tokio::test]
async fn agent_workers_count_against_dispatch_slots() {
    let fixture = agent_fixture(
        RoutingMode::Off,
        None,
        false,
        "opus",
        vec!["test/scripted-difficult".into()],
    )
    .await;
    fixture.team.set_slots(1).await.expect("one worker slot");
    assert!(fixture.team.attach_external_lead());
    fixture
        .team
        .dispatch_request(agent_dispatch_request(
            &fixture.plan_id,
            "the first session-free agent occupies the only slot",
            "slotOne0001",
        ))
        .await
        .expect("the first agent dispatch");

    let error = fixture
        .team
        .dispatch_request(agent_dispatch_request(
            &fixture.plan_id,
            "the second agent must wait for a free slot",
            "slotTwo0001",
        ))
        .await
        .expect_err("the working agent occupies the only slot");
    assert_eq!(error, "all 1 worker slots are busy");

    fixture
        .team
        .agent_done("slotOne0001", "finished".into(), false)
        .await
        .expect("free the agent slot");
    let (second, _, _) = fixture
        .team
        .dispatch_request(agent_dispatch_request(
            &fixture.plan_id,
            "the second agent runs after team_agent_done",
            "slotTwo0001",
        ))
        .await
        .expect("the slot is available after team_agent_done");
    assert_eq!(second.status, WorkerStatus::Working);
}


#[tokio::test]
async fn agent_workers_count_when_reducing_slots() {
    let fixture = agent_fixture(
        RoutingMode::Off,
        None,
        false,
        "opus",
        vec!["test/scripted-difficult".into()],
    )
    .await;
    assert!(fixture.team.attach_external_lead());
    for (id, brief) in [
        ("slotThree01", "the first agent counts as an active worker"),
        ("slotFour001", "the second agent also counts as an active worker"),
    ] {
        fixture
            .team
            .dispatch_request(agent_dispatch_request(&fixture.plan_id, brief, id))
            .await
            .expect("an agent dispatch within the original slot cap");
    }

    let error = fixture
        .team
        .set_slots(1)
        .await
        .expect_err("two active agents prevent reducing the cap to one");
    assert_eq!(error, "stop 1 worker(s) before reducing slots to 1");
    fixture
        .team
        .agent_done("slotThree01", "finished".into(), false)
        .await
        .expect("finish one agent");
    fixture
        .team
        .set_slots(1)
        .await
        .expect("one active agent fits the reduced cap");
}

#[tokio::test]
async fn agent_merge_and_outcome_use_the_claude_code_model() {
    let fixture = agent_fixture(
        RoutingMode::Off,
        None,
        false,
        "opus",
        vec!["test/scripted-difficult".into()],
    )
    .await;
    run_git(fixture._root.path(), &["checkout", "-b", "wt/mnemo"]);
    assert!(fixture.team.attach_external_lead());
    let (record, _, _) = fixture
        .team
        .dispatch_request(agent_dispatch_request(
            &fixture.plan_id,
            "attribute the merged work to the Claude Code agent model",
            "agenttier14",
        ))
        .await
        .expect("the agent dispatch");
    let worktree = std::path::PathBuf::from(&record.worktree);
    std::fs::write(worktree.join("attribution.txt"), "done\n").expect("agent work");
    run_git(&worktree, &["add", "."]);
    run_git(&worktree, &["commit", "-m", "agent attribution"]);
    fixture
        .team
        .agent_done("agenttier14", "finished".into(), false)
        .await
        .expect("finish the agent");
    fixture
        .team
        .record_check(
            "agenttier14",
            "cargo test --workspace".into(),
            "passed".into(),
            None,
        )
        .await
        .expect("record the check");
    fixture
        .team
        .review("agenttier14", "reviewed".into(), Vec::new(), evidence())
        .await
        .expect("review the agent");
    let merged = fixture.team.merge("agenttier14").await.expect("merge the agent");

    assert_eq!(merged.models_used, vec!["claude-code/opus".to_string()]);
    let merge = merged.merge.as_ref().expect("the merge record");
    assert_eq!(merge.worker_model.as_deref(), Some("claude-code/opus"));
    let progress = merge_outcome_line(1, &merged);
    assert!(progress.contains("claude-code/opus"), "{progress}");
    let mut without_merge = merged.clone();
    without_merge.merge = None;
    let progress = merge_outcome_line(1, &without_merge);
    assert!(progress.contains("claude-code/opus"), "{progress}");

    // A merged agent worker is done: no surface may offer to continue it.
    let refusal = fixture
        .team
        .message("agenttier14", "continue please".into(), MessageSender::ExternalLead)
        .await
        .expect_err("a merged agent worker has nothing left to continue");
    assert_eq!(
        refusal,
        "agenttier14 ran as a Claude Code agent and is merged: there is nothing left to continue."
    );
}


/// A git command's trimmed stdout.
fn git_stdout(root: &std::path::Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// The lead commits `file` on the fixture's lead branch.
fn lead_commit(root: &std::path::Path, file: &str, message: &str) -> String {
    std::fs::write(root.join(file), format!("{message}\n")).expect("lead file");
    run_git(root, &["add", file]);
    run_git(root, &["commit", "-m", message]);
    git_stdout(root, &["rev-parse", "HEAD"])
}

/// The worker's transcript as one JSON string, for what a session received.
async fn transcript_text(team: &TeamRuntime, worker_id: &str) -> String {
    let worker = team.worker(worker_id).await.expect("worker record");
    serde_json::to_string(&worker.transcript.messages).expect("transcript json")
}

const FAST_FORWARD_NOTE: &str =
    "[From Mnemo2] The lead branch moved: your checkout was fast-forwarded to";

#[tokio::test]
async fn dispatch_records_and_names_the_base_commit() {
    let (fixture, _notices) = nudge_fixture(Vec::new()).await;
    let root = fixture._root.path();
    let lead_tip = git_stdout(root, &["rev-parse", "HEAD"]);
    let result = fixture.dispatch(Some("routine"), None).await;
    assert!(result.success, "{}", result.output);
    let record = fixture.team.worker("routingworker").await.expect("record");
    assert_eq!(record.base_commit.as_deref(), Some(lead_tip.as_str()));
    let prompt = record.opening_prompt.as_deref().expect("opening prompt");
    let line = format!(
        "You start at {} (seed), the tip of main when you were dispatched.",
        &lead_tip[..8]
    );
    assert!(prompt.ends_with(&line), "{prompt}");
    fixture.team.shutdown().await;
}

#[tokio::test]
async fn dispatch_after_a_lead_commit_gets_the_new_tip() {
    let fixture = agent_fixture(
        RoutingMode::Off,
        None,
        false,
        "opus",
        vec!["test/scripted-difficult".into()],
    )
    .await;
    let root = fixture._root.path();
    assert!(fixture.team.attach_external_lead());
    let first_tip = git_stdout(root, &["rev-parse", "HEAD"]);
    let (first, _, _) = fixture
        .team
        .dispatch_request(agent_dispatch_request(
            &fixture.plan_id,
            "first fresh worker",
            "alphabase1",
        ))
        .await
        .expect("first dispatch");
    assert_eq!(first.base_commit.as_deref(), Some(first_tip.as_str()));
    let second_tip = lead_commit(root, "lead.txt", "lead moves on");
    let (second, _, _) = fixture
        .team
        .dispatch_request(agent_dispatch_request(
            &fixture.plan_id,
            "second fresh worker",
            "betabase02",
        ))
        .await
        .expect("second dispatch");
    assert_eq!(second.base_commit.as_deref(), Some(second_tip.as_str()));
    assert_eq!(git_stdout(&second.worktree, &["rev-parse", "HEAD"]), second_tip);
    let prompt = second.opening_prompt.as_deref().expect("opening prompt");
    assert!(
        prompt.ends_with(&format!(
            "You start at {} (lead moves on), the tip of main when you were dispatched.",
            &second_tip[..8]
        )),
        "{prompt}"
    );
    let first_again = fixture.team.worker("alphabase1").await.expect("first record");
    assert_eq!(first_again.base_commit.as_deref(), Some(first_tip.as_str()));
}

#[tokio::test]
async fn idle_worker_without_commits_follows_the_lead() {
    let (fixture, _notices) = nudge_fixture(Vec::new()).await;
    let root = fixture._root.path();
    let result = fixture.dispatch(Some("routine"), None).await;
    assert!(result.success, "{}", result.output);
    let settled = wait_worker_settled(&fixture.team, "routingworker").await;
    let lead_tip = lead_commit(root, "lead.txt", "lead moves on");
    assert_ne!(settled.base_commit.as_deref(), Some(lead_tip.as_str()));

    fixture
        .team
        .message("routingworker", "carry on".to_string(), MessageSender::Lead)
        .await
        .expect("message the idle worker");
    assert_eq!(git_stdout(&settled.worktree, &["rev-parse", "HEAD"]), lead_tip);
    let record = fixture.team.worker("routingworker").await.expect("record");
    assert_eq!(record.base_commit.as_deref(), Some(lead_tip.as_str()));
    let expected = format!("{FAST_FORWARD_NOTE} {} (lead moves on).", &lead_tip[..8]);
    eventually(|| async {
        transcript_text(&fixture.team, "routingworker")
            .await
            .contains(&expected)
            .then_some(())
    })
    .await;
    fixture.team.shutdown().await;
}

#[tokio::test]
async fn worker_with_commits_is_flagged_stale_not_rewritten() {
    let (fixture, _notices) = nudge_fixture(Vec::new()).await;
    let root = fixture._root.path();
    let result = fixture.dispatch(Some("routine"), None).await;
    assert!(result.success, "{}", result.output);
    let settled = wait_worker_settled(&fixture.team, "routingworker").await;
    std::fs::write(settled.worktree.join("work.txt"), "work\n").expect("worker file");
    run_git(&settled.worktree, &["add", "work.txt"]);
    run_git(&settled.worktree, &["commit", "-m", "worker commit"]);
    let worker_tip = git_stdout(&settled.worktree, &["rev-parse", "HEAD"]);
    lead_commit(root, "lead.txt", "lead moves on");

    fixture
        .team
        .message("routingworker", "carry on".to_string(), MessageSender::Lead)
        .await
        .expect("message the worker");
    assert_eq!(git_stdout(&settled.worktree, &["rev-parse", "HEAD"]), worker_tip);
    assert!(!transcript_text(&fixture.team, "routingworker")
        .await
        .contains(FAST_FORWARD_NOTE));

    let status = fixture
        .team
        .handle_lead_host_tool("team_status", json!({"worker_id": "routingworker"}))
        .await;
    let status: Value = serde_json::from_str(&status.output).expect("status json");
    assert_eq!(status["result"][0]["stale_base"], 1, "{status}");

    let check = fixture
        .team
        .handle_lead_host_tool(
            "team_check",
            json!({"worker_id": "routingworker", "name": "cargo test", "status": "passed"}),
        )
        .await;
    assert!(check.success, "{}", check.output);
    assert!(
        check.output.contains(
            "stale base: the lead branch has 1 commit(s) this branch does not; the checks ran without them. Rebase or merge deliberately if they matter."
        ),
        "{}",
        check.output
    );
    fixture.team.shutdown().await;
}

#[tokio::test]
async fn dirty_worker_is_not_fast_forwarded() {
    let (fixture, _notices) = nudge_fixture(Vec::new()).await;
    let root = fixture._root.path();
    let result = fixture.dispatch(Some("routine"), None).await;
    assert!(result.success, "{}", result.output);
    let settled = wait_worker_settled(&fixture.team, "routingworker").await;
    let base = git_stdout(&settled.worktree, &["rev-parse", "HEAD"]);
    std::fs::write(settled.worktree.join("README.md"), "edited by the worker\n")
        .expect("uncommitted change");
    lead_commit(root, "lead.txt", "lead moves on");

    fixture
        .team
        .message("routingworker", "carry on".to_string(), MessageSender::Lead)
        .await
        .expect("message the dirty worker");
    assert_eq!(git_stdout(&settled.worktree, &["rev-parse", "HEAD"]), base);
    let record = fixture.team.worker("routingworker").await.expect("record");
    assert_eq!(record.base_commit.as_deref(), Some(base.as_str()));
    assert!(!transcript_text(&fixture.team, "routingworker")
        .await
        .contains(FAST_FORWARD_NOTE));
    fixture.team.shutdown().await;
}

#[tokio::test]
async fn dispatch_warns_about_uncommitted_coding_files() {
    let (fixture, _notices) = nudge_fixture(Vec::new()).await;
    let root = fixture._root.path();
    // Track `.coding/` for this test: the fixture ignores it.
    std::fs::write(root.join(".gitignore"), ".worktrees/\n").expect("gitignore");
    run_git(root, &["add", ".gitignore"]);
    run_git(root, &["commit", "-m", "track .coding"]);
    std::fs::create_dir_all(root.join(".coding/plans")).expect("plans dir");
    std::fs::write(root.join(".coding/plans/x.md"), "dirty plan\n").expect("plan file");

    let result = fixture.dispatch(Some("routine"), None).await;
    assert!(result.success, "{}", result.output);
    let answer: Value = serde_json::from_str(result.output.lines().next().unwrap()).expect("dispatch json");
    assert_eq!(answer["result"]["warnings"], json!([]), "plan files alone: {answer}");

    std::fs::write(root.join(".coding/notes.md"), "scratch\n").expect("notes file");
    // The same dispatch again is an idempotent reuse and still warns.
    let again = fixture.dispatch(Some("routine"), None).await;
    assert!(again.success, "{}", again.output);
    let answer: Value = serde_json::from_str(again.output.lines().next().unwrap()).expect("dispatch json");
    assert_eq!(
        answer["result"]["warnings"][0],
        "The lead checkout has 1 uncommitted file(s) under .coding/ that this worker's checkout will not contain: .coding/notes.md. Commit them first if the worker needs them.",
        "{answer}"
    );
    assert_eq!(again.data.as_ref().unwrap()["warnings"], answer["result"]["warnings"]);
    fixture.team.shutdown().await;
}

#[tokio::test]
async fn agent_done_persist_failure_rolls_back_and_can_retry() {
    let fixture = agent_fixture(
        RoutingMode::Off,
        None,
        false,
        "opus",
        vec!["test/scripted-difficult".into()],
    )
    .await;
    assert!(fixture.team.attach_external_lead());
    fixture
        .team
        .dispatch_request(agent_dispatch_request(
            &fixture.plan_id,
            "a failed completion write must leave the agent retryable",
            "agenttier15",
        ))
        .await
        .expect("the agent dispatch");

    // A fresh transcript Arc makes the persistence seam attempt the write;
    // the directory where that transcript belongs makes the rename fail.
    let transcript = fixture
        .state
        .path()
        .join(TRANSCRIPTS_DIR)
        .join("agenttier15.json");
    let mut snapshot = fixture
        .team
        .worker("agenttier15")
        .await
        .expect("the working snapshot");
    // Record the original transcript Arc as written. The ordinary concurrent
    // saver then skips the transcript directory and races only the ledger,
    // while the snapshot override's fresh Arc still forces the failure.
    std::fs::write(
        &transcript,
        serde_json::to_vec(&*snapshot.transcript).expect("encode the empty transcript"),
    )
    .expect("seed the transcript");
    fixture.team.persist().await.expect("record the transcript write");
    std::fs::remove_file(&transcript).expect("replace the transcript with a directory");
    std::fs::create_dir(&transcript).expect("inject the transcript rename failure");
    snapshot.transcript = std::sync::Arc::new(Transcript::default());

    let save_guard = fixture.team.save_lock.lock().await;
    let saver_team = fixture.team.clone();
    let saver = tokio::spawn(async move { saver_team.persist().await });
    tokio::task::yield_now().await;
    let done_team = fixture.team.clone();
    let done = tokio::spawn(async move {
        done_team
            .agent_done_with_snapshot(
                "agenttier15",
                "this completion must not become durable".into(),
                false,
                Some(snapshot),
            )
            .await
    });
    // Before the fix the shared record becomes Completed while the first
    // saver is queued. With the fix it stays Working until the save lock is
    // available, so the timeout is what lets the test proceed.
    let _ = tokio::time::timeout(std::time::Duration::from_millis(100), async {
        loop {
            let worker = fixture
                .team
                .worker("agenttier15")
                .await
                .expect("the worker while completion races a save");
            if worker.status == WorkerStatus::Completed {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    drop(save_guard);
    let error = done
        .await
        .expect("join the completion")
        .expect_err("the injected transcript write fails");
    saver
        .await
        .expect("join the concurrent saver")
        .expect("the concurrent ledger save");
    assert!(error.contains("could not replace the transcript"), "{error}");

    let current = fixture
        .team
        .worker("agenttier15")
        .await
        .expect("the in-memory record after the failed write");
    assert_eq!(current.status, WorkerStatus::Working);
    assert_eq!(current.turns, 0);
    assert!(current.report.is_none());
    assert!(fixture.team.drain_external_notices().is_empty());
    std::fs::remove_dir(&transcript).expect("clear the injected failure");
    let saved = TeamPersistence::new(fixture.state.path())
        .load()
        .expect("reload the durable ledger")
        .expect("a saved team");
    let saved_worker = saved
        .workers
        .iter()
        .find(|worker| worker.id == "agenttier15")
        .expect("the durable agent record");
    assert_eq!(saved_worker.status, WorkerStatus::Working);

    let done = fixture
        .team
        .agent_done("agenttier15", "the clean retry completes".into(), false)
        .await
        .expect("the clean retry");
    assert_eq!(done.status, WorkerStatus::Completed);
    assert!(fixture
        .team
        .drain_external_notices()
        .iter()
        .any(|notice| notice.contains("agenttier15") && notice.contains("finished its turn")));
    let saved = TeamPersistence::new(fixture.state.path())
        .load()
        .expect("reload after the retry")
        .expect("a saved team");
    let saved_worker = saved
        .workers
        .iter()
        .find(|worker| worker.id == "agenttier15")
        .expect("the persisted completed agent");
    assert_eq!(saved_worker.status, WorkerStatus::Completed);
    assert_eq!(saved_worker.report.as_deref(), Some("the clean retry completes"));
}


#[tokio::test]
async fn agent_workers_count_against_resume_slots() {
    let fixture = agent_fixture(
        RoutingMode::Off,
        None,
        false,
        "opus",
        vec!["test/scripted-difficult".into()],
    )
    .await;
    fixture.team.set_slots(1).await.expect("one worker slot");
    assert!(fixture.team.attach_external_lead());
    let mut normal = agent_dispatch_request(
        &fixture.plan_id,
        "a stopped Mnemo worker will later need the only slot",
        "resumeB01",
    );
    normal.sender = MessageSender::Lead;
    fixture
        .team
        .dispatch_request(normal)
        .await
        .expect("dispatch the Mnemo worker");
    fixture.team.stop("resumeB01").await.expect("stop the worker");
    fixture
        .team
        .dispatch_request(agent_dispatch_request(
            &fixture.plan_id,
            "the agent now owns the only worker slot",
            "agentRsum01",
        ))
        .await
        .expect("dispatch the agent");

    let error = fixture
        .team
        .resume("resumeB01")
        .await
        .expect_err("resume must not run alongside the active agent");
    assert_eq!(error, "all 1 worker slots are busy");
    fixture
        .team
        .agent_done("agentRsum01", "finished".into(), false)
        .await
        .expect("release the agent slot");
    fixture
        .team
        .resume("resumeB01")
        .await
        .expect("resume succeeds once the agent is done");
}


#[tokio::test]
async fn agent_workers_count_against_audit_slots() {
    let fixture = agent_fixture(
        RoutingMode::Off,
        None,
        false,
        "opus",
        vec!["test/scripted-difficult".into()],
    )
    .await;
    fixture.team.set_slots(1).await.expect("one worker slot");
    assert!(fixture.team.attach_external_lead());
    fixture
        .team
        .dispatch_request(agent_dispatch_request(
            &fixture.plan_id,
            "the active agent occupies the audit's only possible slot",
            "agentAud001",
        ))
        .await
        .expect("dispatch the agent");

    let mut target = worker(fixture._root.path(), "auditTarget");
    target.plan_id = Some(fixture.plan_id.clone());
    target.plan_step = Some(0);
    provision_agent_worktree(
        fixture._root.path().to_path_buf(),
        target.id.clone(),
        "main".to_string(),
    )
    .await
    .expect("provision the audit target");
    fixture.team.records.lock().await.push(target.clone());

    let error = fixture
        .team
        .audit_dispatch(&target.id, "auditSlot01", None, None, None)
        .await
        .expect_err("audit dispatch must not run alongside the active agent");
    assert_eq!(error, "all 1 worker slots are busy");
    fixture
        .team
        .agent_done("agentAud001", "finished".into(), false)
        .await
        .expect("release the agent slot");
    let (_, reused) = fixture
        .team
        .audit_dispatch(&target.id, "auditSlot01", None, None, None)
        .await
        .expect("audit dispatch succeeds once the agent is done");
    assert!(!reused);
}


#[tokio::test]
async fn agent_prompt_names_only_tools_a_claude_agent_has() {
    let fixture = agent_fixture(
        RoutingMode::Off,
        None,
        false,
        "opus",
        vec!["test/scripted-difficult".into()],
    )
    .await;
    assert!(fixture.team.attach_external_lead());
    let (record, _, _) = fixture
        .team
        .dispatch_request(agent_dispatch_request(
            &fixture.plan_id,
            "use only the tools available to a Claude Code agent",
            "promptNorm1",
        ))
        .await
        .expect("the agent dispatch");
    let block = claude_agent_block(&record).expect("the agent block");
    let prompt = block["prompt"].as_str().expect("the agent prompt");
    assert!(!prompt.contains("message_parent"), "{prompt}");
    assert!(!prompt.contains("plan_read"), "{prompt}");
    assert!(!prompt.contains("(pinned)"), "{prompt}");
    assert!(prompt.contains("plan_show"), "{prompt}");
    assert!(prompt.contains("end your turn"), "{prompt}");
    assert!(
        prompt.contains(record.worktree.to_str().expect("a utf-8 worktree")),
        "{prompt}"
    );

    let mut read_back = agent_dispatch_request(
        &fixture.plan_id,
        "read the task back without Mnemo worker tools",
        "promptRead1",
    );
    read_back.read_back = true;
    let (record, _, _) = fixture
        .team
        .dispatch_request(read_back)
        .await
        .expect("the read-back agent dispatch");
    let block = claude_agent_block(&record).expect("the read-back agent block");
    let prompt = block["prompt"].as_str().expect("the read-back prompt");
    assert!(prompt.contains("No questions."), "{prompt}");
    assert!(!prompt.contains("message_parent"), "{prompt}");
    assert!(!prompt.contains("plan_read"), "{prompt}");
}


#[tokio::test]
async fn team_agent_done_rollback_keeps_a_concurrent_routing_update() {
    let fixture = agent_fixture(
        RoutingMode::Off,
        None,
        false,
        "opus",
        vec!["test/scripted-difficult".into()],
    )
    .await;
    assert!(fixture.team.attach_external_lead());
    fixture
        .team
        .dispatch_request(agent_dispatch_request(
            &fixture.plan_id,
            "a concurrent routing classification must survive rollback",
            "agentRb0001",
        ))
        .await
        .expect("the agent dispatch");
    let transcript = fixture
        .state
        .path()
        .join(TRANSCRIPTS_DIR)
        .join("agentRb0001.json");
    std::fs::create_dir(&transcript).expect("inject the transcript rename failure");
    let mut snapshot = fixture
        .team
        .worker("agentRb0001")
        .await
        .expect("the working snapshot");
    snapshot.transcript = std::sync::Arc::new(Transcript::default());

    let save_guard = fixture.team.save_lock.lock().await;
    let done_team = fixture.team.clone();
    let done = tokio::spawn(async move {
        done_team
            .agent_done_with_snapshot(
                "agentRb0001",
                "the failed completion must not erase routing".into(),
                false,
                Some(snapshot),
            )
            .await
    });
    // agent_done captures its rollback snapshot before waiting for this lock.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let routing = super::record::WorkerRouting {
        laya_tier: Some("high".into()),
        confidence: Some(0.91),
        cleared: true,
        lead_tier: Some("high".into()),
        routed_by: "laya".into(),
        ran_tier: Some("high".into()),
        nudged: None,
    };
    fixture
        .team
        .update_worker("agentRb0001", |worker| {
            worker.routing = Some(routing.clone());
        })
        .await;
    drop(save_guard);
    let error = done
        .await
        .expect("join the failed completion")
        .expect_err("the injected save fails");
    assert!(error.contains("could not replace the transcript"), "{error}");

    let current = fixture
        .team
        .worker("agentRb0001")
        .await
        .expect("the rolled-back worker");
    assert_eq!(current.status, WorkerStatus::Working);
    assert_eq!(current.turns, 0);
    assert!(current.report.is_none());
    assert_eq!(current.routing.as_ref(), Some(&routing));
}

/// A team whose `.coding/checks.toml` lists `commands` and whose one pool
/// worker sits idle on a committed change. Returns the workspace, the state
/// dir, the config dir, the team and the worker id.
async fn check_run_fixture(
    id: &str,
    commands: &[&str],
) -> (
    tempfile::TempDir,
    tempfile::TempDir,
    tempfile::TempDir,
    Arc<TeamRuntime>,
    String,
) {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    let config = tempfile::tempdir().expect("config");
    init_git(root.path());
    // Mnemo2 never merges into main: the lead works on its own branch.
    run_git(root.path(), &["switch", "-c", "wt/mnemo"]);
    std::fs::create_dir_all(root.path().join(".coding")).expect("coding dir");
    let list = commands
        .iter()
        .map(|command| format!("{command:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    std::fs::write(
        root.path().join(".coding/checks.toml"),
        format!("commands = [{list}]\n"),
    )
    .expect("checks.toml");
    let team = Arc::new(
        open_test_team_with_config(root.path(), state.path(), Vec::new(), config.path()).await,
    );
    let worker = provision_pool_worker(&team, id).await;
    std::fs::write(worker.worktree.join("change.txt"), "change\n").expect("worker change");
    run_git(&worker.worktree, &["add", "change.txt"]);
    run_git(&worker.worktree, &["commit", "-m", "worker change"]);
    team.update_worker(&worker.id, |current| {
        current.status = WorkerStatus::Completed;
    })
    .await;
    (root, state, config, team, worker.id)
}

/// Take every heavy-command slot of the fixture's machine (the default two),
/// so a check run started now waits until the returned guards drop.
fn hold_all_slots(config: &std::path::Path) -> Vec<mnemo::machine_slots::SlotGuard> {
    let slots = mnemo::machine_slots::HeavySlots::new(
        mnemo::machine_slots::default_dir(config),
        2,
    );
    (0..slots.slots())
        .map(|_| slots.try_take().guard().expect("a free slot"))
        .collect()
}

/// Mnemo's check run records one check per command on the tip it started at,
/// names the exit code and run time in the detail, and the notice reports
/// each command and the next move.
#[tokio::test]
async fn team_run_checks_records_every_command_on_the_tip() {
    let (_root, _state, _config, team, worker_id) = check_run_fixture(
        "runChecks01",
        &["git --version", "git rev-parse --verify refs/heads/nope", "git status"],
    )
    .await;
    let worker = team.worker(&worker_id).await.expect("worker");
    let tip = phase3::tip(&worker.worktree).await.expect("tip");
    let mut notices = team.subscribe_board();
    let started = team
        .run_checks_tool(Some(&worker_id))
        .await
        .expect("the run starts");
    assert!(started.contains("a notice follows"), "{started}");
    let notice = drain_notice(&mut notices, "Mnemo ran the checks for")
        .await
        .expect("the run ends with a notice");
    assert!(notice.starts_with(&format!("worker_id={worker_id}\n")), "{notice}");
    assert!(
        notice.contains(&format!(
            "Mnemo ran the checks for {worker_id} at {}: 2 passed, 1 failed.",
            &tip[..8]
        )),
        "{notice}"
    );
    assert!(notice.contains("PASS git --version (exit 0"), "{notice}");
    assert!(
        notice.contains("FAIL git rev-parse --verify refs/heads/nope (exit "),
        "{notice}"
    );
    assert!(notice.ends_with("Route the failures with team_route_fix."), "{notice}");
    let worker = team.worker(&worker_id).await.expect("worker");
    assert_eq!(worker.checks.len(), 3, "{:?}", worker.checks);
    for (name, status) in [
        ("git --version", "passed"),
        ("git rev-parse --verify refs/heads/nope", "failed"),
        ("git status", "passed"),
    ] {
        let check = worker
            .checks
            .iter()
            .find(|check| check.name == name)
            .unwrap_or_else(|| panic!("a check named {name}: {:?}", worker.checks));
        assert_eq!(check.status, status, "{name}");
        assert_eq!(check.tip, tip, "{name}");
        let detail = check.detail.clone().unwrap_or_default();
        assert!(detail.starts_with("run by Mnemo: exit "), "{detail}");
    }
    team.shutdown().await;
}

/// A tip that moves while the run waits for a slot makes the run record
/// nothing and say so.
#[tokio::test]
async fn team_run_checks_discards_results_when_the_tip_moves() {
    let (_root, _state, config, team, worker_id) =
        check_run_fixture("runChecks02", &["git --version"]).await;
    let slots = hold_all_slots(config.path());
    let mut notices = team.subscribe_board();
    team.run_checks_tool(Some(&worker_id))
        .await
        .expect("the run starts");
    let worker = team.worker(&worker_id).await.expect("worker");
    run_git(
        &worker.worktree,
        &["commit", "--allow-empty", "-m", "the tip moves under the run"],
    );
    drop(slots);
    let notice = drain_notice(&mut notices, "while they ran")
        .await
        .expect("the run says the tip moved");
    assert!(notice.contains("nothing was recorded"), "{notice}");
    let worker = team.worker(&worker_id).await.expect("worker");
    assert!(worker.checks.is_empty(), "{:?}", worker.checks);
    team.shutdown().await;
}

/// A review with no open findings at a tip without checks starts Mnemo's own
/// run and says so; team_merge waits for it and succeeds after its notice.
#[tokio::test]
async fn a_clean_review_starts_the_missing_checks() {
    let (_root, _state, config, team, worker_id) =
        check_run_fixture("runChecks03", &["git --version", "git status"]).await;
    let slots = hold_all_slots(config.path());
    let mut notices = team.subscribe_board();
    let worker = team.worker(&worker_id).await.expect("worker");
    let tip = phase3::tip(&worker.worktree).await.expect("tip");
    let answer = team
        .handle_lead_host_tool(
            "team_review",
            json!({
                "worker_id": worker_id,
                "summary": "checked the diff",
                "findings": [],
                "evidence": serde_json::to_value(evidence()).expect("evidence"),
            }),
        )
        .await;
    assert!(answer.success, "{answer:?}");
    assert!(
        answer.output.contains(&format!(
            "Mnemo is running the missing checks at {}; merge after its notice.",
            &tip[..8]
        )),
        "{}",
        answer.output
    );
    let refused = team
        .handle_lead_host_tool("team_merge", json!({"worker_id": worker_id}))
        .await;
    assert!(!refused.success, "{refused:?}");
    assert!(
        refused.output.contains(&format!(
            "Mnemo is running the checks for {worker_id}; wait for its notice."
        )),
        "{}",
        refused.output
    );
    drop(slots);
    let notice = drain_notice(&mut notices, "2 passed, 0 failed")
        .await
        .expect("the run ends with a notice");
    assert!(notice.contains("Merge it with team_merge."), "{notice}");
    let merged = team
        .handle_lead_host_tool("team_merge", json!({"worker_id": worker_id}))
        .await;
    assert!(merged.success, "{merged:?}");
    team.shutdown().await;
}

/// While a run is in progress the entry points that would move or judge the
/// checkout refuse with one message.
#[tokio::test]
async fn merge_waits_for_a_running_check_run() {
    let (_root, _state, _config, team, worker_id) =
        check_run_fixture("runChecks04", &["git --version"]).await;
    assert!(team.check_runs.hold_for_test(&worker_id));
    let expected = format!("Mnemo is running the checks for {worker_id}; wait for its notice.");
    assert_eq!(
        team.merge_as(&worker_id, true)
            .await
            .expect_err("a merge is refused while the checks run"),
        expected
    );
    assert_eq!(
        team.message(&worker_id, "hello".to_string(), MessageSender::Lead)
            .await
            .expect_err("a message is refused while the checks run"),
        expected
    );
    assert_eq!(
        team.resume(&worker_id)
            .await
            .expect_err("a resume is refused while the checks run"),
        expected
    );
    assert_eq!(
        team.route_fix(&worker_id, "any-finding")
            .await
            .expect_err("a routed fix is refused while the checks run"),
        expected
    );
    assert_eq!(
        team.review_fix(&worker_id, "any-finding", "change.txt", "change", "changed")
            .await
            .expect_err("review_fix is refused while the checks run"),
        expected
    );
    assert_eq!(
        team.run_checks_tool(Some(&worker_id))
            .await
            .expect_err("a second run is refused"),
        expected
    );
    // A re-dispatch of the same worker id starts a turn in its checkout, so it
    // is refused too.
    team.update_worker(&worker_id, |current| {
        current.status = WorkerStatus::Failed;
        current.brief_sent = false;
    })
    .await;
    let brief = team.worker(&worker_id).await.expect("worker").brief;
    assert_eq!(
        team.dispatch_request(DispatchRequest {
            brief,
            tier: None,
            dispatch_routing: false,
            source: DispatchSource::Ipc,
            sender: MessageSender::User,
            model: None,
            reasoning_effort: None,
            worker_id: Some(worker_id.clone()),
            plan: None,
            review_risk: None,
            read_back: false,
        })
        .await
        .expect_err("a re-dispatch is refused while the checks run"),
        expected
    );
    team.shutdown().await;
}

/// Without a worker the run goes to the lead checkout: the notice names the
/// lead branch and its tip, and nothing is recorded on any worker.
#[tokio::test]
async fn team_run_checks_without_a_worker_runs_in_the_lead_checkout() {
    let (root, _state, _config, team, worker_id) =
        check_run_fixture("runChecks05", &["git --version"]).await;
    let tip = phase3::tip(root.path()).await.expect("lead tip");
    let mut notices = team.subscribe_board();
    let answer = team
        .handle_lead_host_tool("team_run_checks", json!({}))
        .await;
    assert!(answer.success, "{answer:?}");
    let notice = drain_notice(&mut notices, "lead checkout")
        .await
        .expect("the run ends with a notice");
    assert!(
        notice.contains(&format!(
            "Mnemo ran the checks in the lead checkout (wt/mnemo at {}): 1 passed, 0 failed.",
            &tip[..8]
        )),
        "{notice}"
    );
    let worker = team.worker(&worker_id).await.expect("worker");
    assert!(worker.checks.is_empty(), "{:?}", worker.checks);
    team.shutdown().await;
}

/// The routed-fix prompt asks for the narrowest covering tests instead of the
/// full checks.
#[test]
fn route_fix_brief_asks_for_narrow_tests() {
    let root = tempfile::tempdir().expect("workspace");
    let mut record = worker(root.path(), "narrowfix001");
    let routed = open_finding("finding1");
    record.findings = vec![routed.clone()];
    let brief = route_fix_brief(&record, &routed);
    assert!(
        brief.ends_with(
            "\nCommit fixes for every finding above. Verify them with the narrowest tests that cover your change (for example the touched crate's tests or the tests the findings name), not with run_checks: Mnemo runs the full checks once at the reviewed tip. Report the commit and the tests you ran for delta review."
        ),
        "{brief}"
    );
}

/// A command that exits 0 but rewrites tracked files is not a passed check:
/// the pump's counting rule rejects it, Mnemo records it failed with the
/// reason, and the merge gate refuses because of it.
#[tokio::test]
async fn team_run_checks_does_not_count_a_command_that_changes_files() {
    let (_root, _state, _config, team, worker_id) =
        check_run_fixture("runChecks06", &["git mv change.txt moved.txt"]).await;
    let mut notices = team.subscribe_board();
    let answer = team
        .handle_lead_host_tool(
            "team_review",
            json!({
                "worker_id": worker_id,
                "summary": "checked the diff",
                "findings": [],
                "evidence": serde_json::to_value(evidence()).expect("evidence"),
            }),
        )
        .await;
    assert!(answer.success, "{answer:?}");
    let notice = drain_notice(&mut notices, "Mnemo ran the checks for")
        .await
        .expect("the run ends with a notice");
    assert!(notice.contains("0 passed, 1 failed"), "{notice}");
    let worker = team.worker(&worker_id).await.expect("worker");
    let check = worker
        .checks
        .iter()
        .find(|check| check.name == "git mv change.txt moved.txt")
        .expect("the command is recorded");
    assert_eq!(check.status, "failed");
    let detail = check.detail.clone().unwrap_or_default();
    assert!(detail.starts_with("run by Mnemo: exit 0, "), "{detail}");
    assert!(
        detail.ends_with(", but it changed the checkout's files, so it does not count. Commit or discard the change, or fix the command."),
        "{detail}"
    );
    // The command left the checkout dirty and its check is failed: the merge
    // is refused (the dirty-tree gate answers first).
    let refused = team
        .handle_lead_host_tool("team_merge", json!({"worker_id": worker_id}))
        .await;
    assert!(!refused.success, "{refused:?}");
    assert!(
        refused.output.contains("the worker working tree must be clean"),
        "{}",
        refused.output
    );
    team.shutdown().await;
}

/// A turn-end proof cannot claim a checkout while Mnemo's check run measures
/// it: the proof's edit and restore would taint what the checks see.
#[tokio::test]
async fn a_proof_cannot_claim_during_a_check_run() {
    let (_root, _state, team, worker_id) = proof_fixture("proveVsChecks1").await;
    assert!(team.check_runs.hold_for_test(&worker_id));
    let error = team
        .proof_ctx()
        .spawn(&worker_id, &[proving::ProofJob::Regression])
        .await
        .expect_err("a proof is refused while the checks run");
    assert_eq!(
        error,
        format!("Mnemo is running the checks for {worker_id}; wait for its notice.")
    );
    team.shutdown().await;
}

/// The run uses the configured core-operation list, like a worker session's
/// tool: a check command that is one of them is refused, not run.
#[tokio::test]
async fn mnemo_check_run_refuses_a_configured_core_operation() {
    let (_root, _state, config, team, worker_id) =
        check_run_fixture("runChecks07", &["git --version", "git status"]).await;
    let mut text = std::fs::read_to_string(config.path().join("config.toml")).expect("config.toml");
    text.push_str("\n[git]\ncore_operations = [\"status\"]\n");
    std::fs::write(config.path().join("config.toml"), text).expect("config.toml");
    let mut notices = team.subscribe_board();
    team.run_checks_tool(Some(&worker_id))
        .await
        .expect("the run starts");
    let notice = drain_notice(&mut notices, "Mnemo ran the checks for")
        .await
        .expect("the run ends with a notice");
    assert!(notice.contains("1 passed, 1 failed"), "{notice}");
    assert!(
        notice.contains("FAIL git status (refused: core git operation)"),
        "{notice}"
    );
    let worker = team.worker(&worker_id).await.expect("worker");
    let check = worker
        .checks
        .iter()
        .find(|check| check.name == "git status")
        .expect("the refused command is recorded");
    assert_eq!(check.status, "failed");
    team.shutdown().await;
}

/// Mnemo checks only a committed tip: a checkout with uncommitted changes is
/// refused up front, and the automatic start after a clean review says so in
/// the review result instead of starting.
#[tokio::test]
async fn team_run_checks_refuses_a_dirty_checkout() {
    let (_root, _state, _config, team, worker_id) =
        check_run_fixture("runChecks08", &["git --version"]).await;
    let worker = team.worker(&worker_id).await.expect("worker");
    std::fs::write(worker.worktree.join("scratch.txt"), "uncommitted\n").expect("dirty file");
    let sentence = format!(
        "{worker_id}'s checkout has uncommitted changes; Mnemo checks only a committed tip. Commit or discard them first."
    );
    assert_eq!(
        team.run_checks_tool(Some(&worker_id))
            .await
            .expect_err("a dirty checkout is refused"),
        sentence
    );
    let answer = team
        .handle_lead_host_tool(
            "team_review",
            json!({
                "worker_id": worker_id,
                "summary": "checked the diff",
                "findings": [],
                "evidence": serde_json::to_value(evidence()).expect("evidence"),
            }),
        )
        .await;
    assert!(answer.success, "{answer:?}");
    assert!(answer.output.contains(&sentence), "{}", answer.output);
    assert!(!answer.output.contains("Mnemo is running the missing checks"), "{}", answer.output);
    team.shutdown().await;
}

/// A lead branch that moves while the lead-checkout run waits for a slot gets
/// the moved-tip notice, with no pass claim.
#[tokio::test]
async fn the_lead_check_run_reports_a_moved_tip() {
    let (root, _state, config, team, _worker_id) =
        check_run_fixture("runChecks09", &["git --version"]).await;
    let slots = hold_all_slots(config.path());
    let mut notices = team.subscribe_board();
    team.run_checks_tool(None).await.expect("the run starts");
    run_git(
        root.path(),
        &["commit", "--allow-empty", "-m", "the lead moves under the run"],
    );
    drop(slots);
    let notice = drain_notice(&mut notices, "while the checks ran")
        .await
        .expect("the run says the lead branch moved");
    assert!(notice.contains("the lead branch moved from "), "{notice}");
    assert!(notice.contains("run team_run_checks again"), "{notice}");
    assert!(!notice.contains("passed"), "{notice}");
    assert!(!notice.contains("PASS"), "{notice}");
    team.shutdown().await;
}

/// A check run that panics still tells the lead, records nothing, and gives
/// the claim back, so the next request is accepted.
#[tokio::test]
async fn a_panicking_check_run_still_notifies() {
    let (_root, _state, _config, team, worker_id) =
        check_run_fixture("runChecks10", &["git --version"]).await;
    team.check_runs
        .panic_next
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let mut notices = team.subscribe_board();
    team.run_checks_tool(Some(&worker_id))
        .await
        .expect("the run starts");
    let notice = drain_notice(&mut notices, "stopped unexpectedly")
        .await
        .expect("the stopped run still sends a notice");
    assert!(
        notice.contains(&format!(
            "Mnemo's check run for {worker_id} stopped unexpectedly; nothing was recorded. Run team_run_checks again."
        )),
        "{notice}"
    );
    let worker = team.worker(&worker_id).await.expect("worker");
    assert!(worker.checks.is_empty(), "{:?}", worker.checks);
    eventually(|| async { (!team.check_runs.is_active(&worker_id)).then_some(()) }).await;
    team.run_checks_tool(Some(&worker_id))
        .await
        .expect("the claim was released, so a new run is accepted");
    team.shutdown().await;
}


// --- Claude Code agent workers are observable (plan ca7f2177, step 1) -------

/// A transcript with two tool_use blocks across two assistant lines, one
/// garbage line, and usage that sums to 1125 tokens.
const AGENT_TRANSCRIPT: &str = concat!(
    "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"do the step\"}}\n",
    "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"Bash\",\"input\":{}}],\"usage\":{\"input_tokens\":10,\"output_tokens\":5,\"cache_read_input_tokens\":100,\"cache_creation_input_tokens\":1000}}}\n",
    "this line is not json\n",
    "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"ok\"},{\"type\":\"tool_use\",\"id\":\"t2\",\"name\":\"Read\",\"input\":{}}],\"usage\":{\"input_tokens\":1,\"output_tokens\":2,\"cache_read_input_tokens\":3,\"cache_creation_input_tokens\":4}}}\n",
);

/// A started agent worker, a projects folder to find its transcript in, and
/// the transcript path it will have.
async fn agent_watch_fixture(
    worker_id: &str,
) -> (RoutingFixture, tempfile::TempDir, std::path::PathBuf) {
    let fixture = agent_fixture(
        RoutingMode::Off,
        None,
        false,
        "opus",
        vec!["test/scripted-difficult".into()],
    )
    .await;
    assert!(fixture.team.attach_external_lead());
    fixture
        .team
        .dispatch_request(agent_dispatch_request(
            &fixture.plan_id,
            "implement the agent-tier handling",
            worker_id,
        ))
        .await
        .expect("the agent dispatch");
    let projects = tempfile::tempdir().expect("projects folder");
    fixture
        .team
        .agent_watch
        .set_projects_root_for_test(projects.path().to_path_buf());
    let transcript = projects
        .path()
        .join("D--proj")
        .join("session1")
        .join("subagents")
        .join("agent-a1b2c3.jsonl");
    (fixture, projects, transcript)
}

fn write_agent_transcript(path: &std::path::Path, text: &str) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("folders");
    std::fs::write(path, text).expect("write the transcript");
}

async fn start_agent(team: &TeamRuntime, worker_id: &str, extra: serde_json::Value) -> ToolResult {
    let mut args = serde_json::json!({"worker_id": worker_id, "agent_id": "a1b2c3"});
    if let (Some(args), Some(extra)) = (args.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            args.insert(key.clone(), value.clone());
        }
    }
    team.handle_lead_host_tool("team_agent_started", args).await
}

async fn status_agent(team: &TeamRuntime, worker_id: &str) -> serde_json::Value {
    let result = team
        .handle_lead_host_tool("team_status", serde_json::json!({"worker_id": worker_id}))
        .await;
    assert!(result.success, "{}", result.output);
    let parsed: serde_json::Value = serde_json::from_str(&result.output).expect("status json");
    parsed["result"][0]["agent"].clone()
}

fn count_notices(
    receiver: &mut tokio::sync::broadcast::Receiver<BoardSignal>,
    needle: &str,
) -> Vec<String> {
    let mut found = Vec::new();
    while let Ok(signal) = receiver.try_recv() {
        if let BoardSignal::Notice { text, .. } = signal {
            if text.contains(needle) {
                found.push(text);
            }
        }
    }
    found
}

#[tokio::test]
async fn team_agent_started_records_the_agent_and_its_transcript() {
    let (fixture, _projects, transcript) = agent_watch_fixture("agentWatch01").await;
    let team = &fixture.team;
    // The documented location is searched when no path is given; a missing
    // file stores the id and says so.
    let early = start_agent(team, "agentWatch01", serde_json::json!({})).await;
    assert!(early.success, "{}", early.output);
    assert!(early.output.contains("not found yet"), "{}", early.output);
    let worker = team.worker("agentWatch01").await.expect("worker");
    assert_eq!(worker.claude_agent_id.as_deref(), Some("a1b2c3"));
    assert_eq!(worker.claude_agent_transcript, None);
    // The file appears: a later read finds it and keeps it on the record.
    write_agent_transcript(&transcript, AGENT_TRANSCRIPT);
    let agent = status_agent(team, "agentWatch01").await;
    assert_eq!(agent["transcript"], transcript.to_string_lossy().as_ref());
    let worker = team.worker("agentWatch01").await.expect("worker");
    assert_eq!(
        worker.claude_agent_transcript.as_deref(),
        Some(transcript.to_string_lossy().as_ref())
    );
    // A given path wins, and an unknown worker is refused.
    let named = start_agent(
        team,
        "agentWatch01",
        serde_json::json!({"transcript_path": transcript.to_string_lossy()}),
    )
    .await;
    assert!(named.success, "{}", named.output);
    assert!(named.output.contains("follows its transcript"), "{}", named.output);
    let missing = start_agent(team, "noSuchWorker1", serde_json::json!({})).await;
    assert!(!missing.success);
    // The watcher runs while the registered worker works, and stops on shutdown.
    assert!(team.agent_watch.running_for_test());
    team.shutdown().await;
    assert!(!team.agent_watch.running_for_test());
}

#[tokio::test]
async fn team_status_shows_agent_activity_from_the_transcript() {
    let (fixture, _projects, transcript) = agent_watch_fixture("agentWatch02").await;
    let team = &fixture.team;
    write_agent_transcript(&transcript, AGENT_TRANSCRIPT);
    assert!(start_agent(team, "agentWatch02", serde_json::json!({})).await.success);
    let agent = status_agent(team, "agentWatch02").await;
    assert_eq!(agent["id"], "a1b2c3");
    assert_eq!(agent["tool_calls"], 2, "{agent}");
    assert_eq!(agent["tokens"], 1125, "{agent}");
    assert_eq!(agent["finished"], false);
    assert_eq!(agent["stalled"], false);
    assert!(agent["last_activity_ms"].as_u64().unwrap_or(0) > 0, "{agent}");
    // Reading is incremental: a second look counts nothing twice, and an
    // appended line counts once.
    let again = status_agent(team, "agentWatch02").await;
    assert_eq!(again["tool_calls"], 2);
    let mut text = AGENT_TRANSCRIPT.to_string();
    text.push_str("{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"tool_use\",\"id\":\"t3\",\"name\":\"Bash\",\"input\":{}}],\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}\n");
    write_agent_transcript(&transcript, &text);
    let more = status_agent(team, "agentWatch02").await;
    assert_eq!(more["tool_calls"], 3, "{more}");
    assert_eq!(more["tokens"], 1130, "{more}");
    team.shutdown().await;
}

#[tokio::test]
async fn a_silent_agent_is_flagged_stalled() {
    let (fixture, _projects, transcript) = agent_watch_fixture("agentWatch03").await;
    let team = &fixture.team;
    write_agent_transcript(&transcript, AGENT_TRANSCRIPT);
    assert!(start_agent(team, "agentWatch03", serde_json::json!({})).await.success);
    let mut notices = team.subscribe_board();
    team.agent_watch_tick().await;
    assert!(count_notices(&mut notices, "has written nothing").is_empty());
    assert_eq!(status_agent(team, "agentWatch03").await["stalled"], false);

    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(11 * 60);
    std::fs::File::options()
        .write(true)
        .open(&transcript)
        .expect("open")
        .set_modified(old)
        .expect("set mtime");
    assert_eq!(status_agent(team, "agentWatch03").await["stalled"], true);
    team.agent_watch_tick().await;
    team.agent_watch_tick().await;
    let sent = count_notices(&mut notices, "has written nothing");
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(
        sent[0].contains("agentWatch03's Claude Code agent has written nothing for 11 minutes; it may have stalled or stopped. Check it, then continue it with SendMessage or call team_agent_done."),
        "{}",
        sent[0]
    );
    team.shutdown().await;
}

#[tokio::test]
async fn a_finished_agent_prompts_team_agent_done() {
    let (fixture, _projects, transcript) = agent_watch_fixture("agentWatch04").await;
    let team = &fixture.team;
    write_agent_transcript(&transcript, AGENT_TRANSCRIPT);
    assert!(start_agent(team, "agentWatch04", serde_json::json!({})).await.success);
    let mut notices = team.subscribe_board();
    team.agent_watch_tick().await;
    assert!(count_notices(&mut notices, "finished").is_empty());
    std::fs::write(
        transcript.with_extension("meta.json"),
        "{\"agentType\":\"general-purpose\",\"finished\":true}",
    )
    .expect("meta");
    assert_eq!(status_agent(team, "agentWatch04").await["finished"], true);
    team.agent_watch_tick().await;
    team.agent_watch_tick().await;
    let sent = count_notices(&mut notices, "finished");
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(
        sent[0].contains("agentWatch04's Claude Code agent finished; call team_agent_done with its report."),
        "{}",
        sent[0]
    );
    team.shutdown().await;
}

#[tokio::test]
async fn team_progress_lands_in_the_log_and_notifies() {
    let (fixture, _projects, _transcript) = agent_watch_fixture("agentWatch05").await;
    let team = &fixture.team;
    let mut notices = team.subscribe_board();
    let posted = team
        .handle_lead_host_tool(
            "team_progress",
            serde_json::json!({"worker_id": "agentWatch05", "note": "  the change is in  "}),
        )
        .await;
    assert!(posted.success, "{}", posted.output);
    let worker = team.worker("agentWatch05").await.expect("worker");
    let entry = worker.inbox.last().expect("the note is in the log");
    assert_eq!(entry.sender, MessageSender::ClaudeAgent);
    assert_eq!(entry.kind, InboxKind::Progress);
    assert_eq!(entry.text, "the change is in");
    let sent = count_notices(&mut notices, "progress:");
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0], "worker_id=agentWatch05\nagentWatch05 progress: the change is in");
    for note in ["   ".to_string(), "x".repeat(501)] {
        let refused = team
            .handle_lead_host_tool(
                "team_progress",
                serde_json::json!({"worker_id": "agentWatch05", "note": note}),
            )
            .await;
        assert!(!refused.success, "{}", refused.output);
    }
    assert_eq!(team.worker("agentWatch05").await.expect("worker").inbox.len(), worker.inbox.len());
    team.shutdown().await;
}

#[test]
fn agent_prompt_carries_the_cd_foreground_and_progress_rules() {
    let mut record = worker(std::path::Path::new("D:/proj"), "agentWatch06");
    record.claude_agent = Some("opus".into());
    let prompt = super::record::agent_prompt(&record);
    let worktree = record.worktree.display();
    assert!(
        prompt.contains(&format!("- Work only inside {worktree}. Claude Code doesn't keep your working directory between shell calls, so start every shell command with cd \"{worktree}\" && (in PowerShell: Set-Location \"{worktree}\"; ), and read and edit files only by absolute paths under it. Never touch the project's main checkout or any other checkout.")),
        "{prompt}"
    );
    assert!(prompt.contains("- Run shell commands in the foreground. Start one in the background only when it will run longer than about two minutes (a full build or test suite)."), "{prompt}");
    assert!(prompt.contains("- After each milestone (you have read the code, the change is in, the tests pass, you committed), post a one-line note with the Mnemo tool team_progress (worker_id agentWatch06)."), "{prompt}");
    assert!(prompt.contains("- Leave Mnemo's team_* and plan_* tools to the lead; plan_show and team_progress are the only ones you may use."), "{prompt}");
}
