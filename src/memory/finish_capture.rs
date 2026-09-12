// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Phase-4 finish auto-capture — deterministic, LLM-free digests written when
//! a plan closes out via `finish`.
//!
//! Design invariants:
//!
//! - **Deterministic + idempotent.** The memory id is the UUIDv5 of a
//!   plan-scoped key (`plan-finish:<plan id>` / `bug-finish:<plan id>`), so a
//!   re-finish (or a crash between capture and Complete) addresses the same
//!   row — INSERT OR REPLACE, never a duplicate.
//! - **Pointer-first + auto-truncated.** Digests reuse the indexer's
//!   [`budgeted_digest`](crate::memory::indexer::budgeted_digest) — the
//!   pointer line (path / regression test / commit) is mandatory, the gist
//!   truncates to fit the configured budget (default PLAN 400, BUG 600).
//! - **Authored, not derived.** The capture writes `record_class = Authored`
//!   — the class weight lifts it over the indexer's derived digests on
//!   relevance ties, and a derived-index rebuild never wipes it.
//! - **Crash-marker supersede.** The plan's `ACTIVE:` / `STEP MARKER:`
//!   working-tier crash markers are superseded (supersede-not-delete) so the
//!   stale "resume me" markers stop recalling. Unrelated markers are never
//!   touched.
//! - **Branch hint.** When the finish lands on a non-main branch, both digest
//!   pointers carry `branch <name> @ <sha> (unmerged — exists only on this
//!   branch)` so a recaller knows the work lives only on that branch until
//!   merged (the `merge_to_main` skill supersedes these records at merge
//!   time). Detection is best-effort — main, a detached HEAD, or a non-git
//!   root yields no hint; the digests are never blocked on it.
//! - **Non-blocking.** The capture runs inside `finish`; a failure logs and
//!   surfaces as a note in the finish output — it never blocks the state
//!   transition (the review gate already passed).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use uuid::Uuid;

use crate::error::Result;
use crate::memory::indexer::{budgeted_digest, first_commit_hash};
use crate::memory::{
    Memory, MemoryClass, MemoryFilter, MemoryRecordType, MemoryStoreTrait, MemoryTier,
};
use crate::workflow::{PlanFile, PlanKind};

/// What a finish capture wrote.
#[derive(Debug, Default)]
pub struct FinishCaptureReport {
    /// The PLAN: digest's memory id.
    pub plan_memory_id: String,
    /// The BUG: digest's memory id (bug_fixing plans only).
    pub bug_memory_id: Option<String>,
    /// How many crash-marker memories were superseded.
    pub markers_superseded: usize,
}

/// Capture the finish digests for a completed plan: the PLAN: digest (every
/// plan), the BUG: digest (bug_fixing plans), and the supersede of the plan's
/// crash markers. Deterministic ids make re-runs idempotent; individual
/// failures are collected and reported, never fatal.
///
/// `repo_root` (the project root implied by the workflow's plans dir) enables
/// the best-effort branch hint — see the module docs. `None` (tests, plans
/// dirs that imply no root) skips the probe.
///
/// `knowledge` wires the knowledge-file backing: the BUG: digest then lands
/// in a knowledge FILE (`.coding/knowledge/bug/<plan-id>.md` — the file is
/// the truth, the row is the indexer's derived digest). `None` keeps the
/// historical authored-row capture (tests).
pub async fn capture_finish(
    store: &dyn MemoryStoreTrait,
    plan: &PlanFile,
    plan_id: &str,
    plans_dir: &Path,
    repo_root: Option<PathBuf>,
    now: i64,
    knowledge: Option<Arc<crate::memory::KnowledgeStore>>,
) -> Result<FinishCaptureReport> {
    let mut report = FinishCaptureReport::default();
    let config = store.memory_search_config();

    // ── Branch hint (best-effort) ───────────────────────────────────────
    // Probed once and appended to BOTH digest pointers: a feature/bug that
    // exists only on a branch must say so — otherwise it is recalled with
    // confidence long after the merge (or mistaken for main before it).
    let branch_hint = match repo_root {
        Some(root) => crate::project::git_ops::branch_hint(root).await,
        None => None,
    };

    // ── PLAN: digest (every plan) ─────────────────────────────────────────
    let plan_file_text =
        std::fs::read_to_string(plans_dir.join(format!("{plan_id}.md"))).unwrap_or_default();
    let gist = plan
        .goal
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string();
    let done = plan.steps.iter().filter(|s| s.done).count();
    let total = plan.steps.len();
    let mut pointer = format!(
        "{done}/{total} steps · {} · path .coding/plans/{plan_id}.md",
        plan.kind
    );
    if let Some(test) = &plan.regression_test {
        pointer.push_str(" · regression test: ");
        pointer.push_str(test);
    }
    if let Some(hash) = first_commit_hash(&plan_file_text) {
        pointer.push_str(" · commit ");
        pointer.push_str(hash);
    }
    if let Some(hint) = &branch_hint {
        pointer.push_str(" · ");
        pointer.push_str(hint);
    }
    let plan_budget = config.digest_budget(MemoryRecordType::Plan).unwrap_or(400);
    let plan_memory_id = Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!("plan-finish:{plan_id}").as_bytes(),
    )
    .to_string();
    write_authored(
        store,
        &plan_memory_id,
        &truncate_title(&format!("PLAN: {}", plan.title)),
        &budgeted_digest(&gist, &pointer, plan_budget),
        now,
    )
    .await?;
    report.plan_memory_id = plan_memory_id;

    // ── BUG: digest (bug_fixing plans) ────────────────────────────────────
    if plan.kind == PlanKind::BugFixing {
        let symptom = plan
            .bug_symptom
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("(no symptom recorded)");
        let test = plan.regression_test.as_deref().unwrap_or("(not recorded)");
        let bug_gist = format!("symptom: {symptom}");
        let mut bug_pointer = format!("regression test: {test} · path .coding/plans/{plan_id}.md");
        if let Some(hint) = &branch_hint {
            bug_pointer.push_str(" · ");
            bug_pointer.push_str(hint);
        }
        let bug_budget = config.digest_budget(MemoryRecordType::Bug).unwrap_or(600);
        let bug_memory_id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("bug-finish:{plan_id}").as_bytes(),
        )
        .to_string();
        // Knowledge-backed capture: the BUG digest becomes a knowledge FILE
        // (the truth) — `.coding/knowledge/bug/<plan-id>.md` — and the
        // derived row comes from the indexer. The slug is the plan id (stable
        // identity; `write_at` is idempotent across re-finishes). The body is
        // unbounded — the full record; the indexer's digest takes the first
        // line as the gist, so the first line carries the key facts. No
        // branch hint: the FILE travels with git, which is what makes the
        // row converge after a merge (the hint exists for DB-only rows).
        if let Some(knowledge) = &knowledge {
            let body = format!(
                "Symptom: {symptom} · regression test: {test}\n\nFull record for plan {plan_id} \
                 (see .coding/plans/{plan_id}.md for the plan file).\n\n{bug_pointer}"
            );
            let rel = knowledge
                .write_at(MemoryRecordType::Bug, plan_id, &plan.title, &body)
                .map_err(|e| {
                    crate::error::Error::Memory(format!(
                        "finish capture: bug knowledge write failed: {e}"
                    ))
                })?;
            // The deterministic row id of the knowledge file (what recall
            // addresses).
            let row_id = crate::memory::indexer::knowledge_id(&rel);
            report.bug_memory_id = Some(row_id);
        } else {
            write_authored(
                store,
                &bug_memory_id,
                &truncate_title(&format!("BUG: {}", plan.title)),
                &budgeted_digest(&bug_gist, &bug_pointer, bug_budget),
                now,
            )
            .await?;
            report.bug_memory_id = Some(bug_memory_id);
        }
    }

    // ── Crash-marker supersede ────────────────────────────────────────────
    // The plan's ACTIVE:/STEP MARKER: working-tier markers are history once
    // the plan finishes — supersede them (never delete) so they stop
    // recalling as "resume me". Unrelated markers are untouched.
    report.markers_superseded = supersede_plan_markers(store, plan_id, "COMPLETE", now).await;

    Ok(report)
}

/// Supersede (never delete) every live `ACTIVE:` / `STEP MARKER:` working-tier
/// crash marker that mentions `plan_id` — the plan's markers are stale the
/// moment it stops being the active plan (finish, or abandon) and must stop
/// recalling as "resume me". Unrelated markers are never touched.
///
/// Shared by `capture_finish` (markers are history when a plan completes) and
/// `AbandonPlanTool` (markers are history when a plan is abandoned without
/// completing — previously they lingered until a later finish, recalling a
/// dead "resume me" plan). Returns how many markers were superseded.
///
/// `reason` becomes the successor's title suffix ("— COMPLETE" at finish,
/// "— ABANDONED" on abandon) and its content verb ("completed" / "abandoned"),
/// keeping the two histories distinguishable.
pub async fn supersede_plan_markers(
    store: &dyn MemoryStoreTrait,
    plan_id: &str,
    reason: &str,
    now: i64,
) -> usize {
    // The candidate list is snapshotted BEFORE any supersede writes: the
    // successor rows ("STEP MARKER: … — {reason}") carry the plan id in
    // their content, so a second list pass would re-match them and supersede
    // the superseder (a self-referential chain).
    let mut candidates = Vec::new();
    for prefix in ["ACTIVE:", "STEP MARKER:"] {
        let markers = match store
            .list_filtered(
                &MemoryFilter::new()
                    .tier(MemoryTier::Working)
                    .title_prefix(prefix),
            )
            .await
        {
            Ok(markers) => markers,
            Err(e) => {
                eprintln!("plan-marker supersede: listing failed: {e}");
                return 0;
            }
        };
        candidates.extend(markers);
    }
    let mut superseded = 0;
    for marker in candidates {
        let mentions_plan = marker.title.contains(plan_id) || marker.content.contains(plan_id);
        if !mentions_plan {
            continue;
        }
        // Strip the marker prefix before re-prefixing — the marker title is
        // already "STEP MARKER: <plan title>" (or "ACTIVE: …"), so a naive
        // format would produce "STEP MARKER: STEP MARKER: … — {reason}"
        // (review L2).
        let base_title = marker
            .title
            .strip_prefix("STEP MARKER: ")
            .or_else(|| marker.title.strip_prefix("ACTIVE: "))
            .unwrap_or(&marker.title);
        let successor = Memory::new(
            MemoryTier::Working,
            format!("STEP MARKER: {base_title} — {reason}"),
            format!(
                "plan {plan_id} {verb} — superseded; no longer the active plan",
                verb = reason.to_lowercase()
            ),
            now,
        );
        match store.supersede_memory(&marker.id, successor).await {
            Ok(_) => superseded += 1,
            Err(e) => eprintln!("plan marker supersede failed: {e}"),
        }
    }
    superseded
}

/// Write one authored digest memory (INSERT OR REPLACE by the deterministic
/// id — idempotent across re-runs).
async fn write_authored(
    store: &dyn MemoryStoreTrait,
    id: &str,
    title: &str,
    content: &str,
    now: i64,
) -> Result<()> {
    let mut memory = Memory::new(MemoryTier::Semantic, title, content, now);
    memory.id = id.to_string();
    memory.record_class = MemoryClass::Authored;
    store.write(memory).await?;
    Ok(())
}

/// Cap a memory title (same rule as the indexer — titles are labels, not
/// digests).
fn truncate_title(title: &str) -> String {
    const MAX_TITLE: usize = 120;
    if title.chars().count() <= MAX_TITLE {
        title.to_string()
    } else {
        title.chars().take(MAX_TITLE).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::embedder::HashEmbedder;
    use crate::memory::MemoryStore;
    use std::sync::Arc;
    use tempfile::tempdir;

    /// The concrete store — tests call the inherent helpers
    /// (`get_memory` / `count_by_class` / `delete_derived`) that live off the
    /// trait; `&store` coerces to `&Arc<dyn MemoryStoreTrait>` for
    /// `capture_finish`.
    fn make_store() -> Arc<MemoryStore> {
        let embedder: Arc<dyn crate::memory::Embedder> = Arc::new(HashEmbedder::new());
        Arc::new(MemoryStore::open_in_memory(embedder).unwrap())
    }

    fn make_plan(kind: PlanKind) -> (PlanFile, String) {
        let mut plan = PlanFile::new(
            "Fix the crash",
            "Make the crash on open go away",
            "C",
            vec!["a".into(), "b".into()],
        );
        plan.kind = kind;
        plan.bug_symptom = Some("the app crashes on open".into());
        plan.regression_test = Some("crash_on_open_regression".into());
        plan.steps[0].done = true;
        (plan, "plan-abc-123".to_string())
    }

    #[tokio::test]
    async fn capture_writes_plan_digest_within_budget_with_pointer() {
        let store = make_store();
        let (plan, id) = make_plan(PlanKind::Implementation);
        let dir = tempdir().unwrap();
        // A plan file with a commit hash — the pointer must carry it.
        std::fs::write(
            dir.path().join(format!("{id}.md")),
            "# Plan: Fix the crash\n\ncommit 1a2b3c4d5e6f7a8b9c0d\n",
        )
        .unwrap();

        let report = capture_finish(store.as_ref(), &plan, &id, dir.path(), None, 1000, None)
            .await
            .unwrap();
        let m = store
            .get_memory(&report.plan_memory_id)
            .await
            .unwrap()
            .expect("plan digest exists");
        assert_eq!(m.title, "PLAN: Fix the crash");
        assert_eq!(m.record_class, MemoryClass::Authored);
        assert_eq!(m.record_type, MemoryRecordType::Plan);
        // Within the PLAN budget (400 chars).
        assert!(
            m.content.chars().count() <= 400,
            "{} chars",
            m.content.chars().count()
        );
        // Pointer-first: path + done/total + commit.
        assert!(m.content.contains("1/2 steps"), "{}", m.content);
        assert!(
            m.content.contains("path .coding/plans/plan-abc-123.md"),
            "{}",
            m.content
        );
        assert!(
            m.content.contains("commit 1a2b3c4d5e6f7a8b9c0d"),
            "{}",
            m.content
        );
        // The gist (goal first line) is present.
        assert!(
            m.content.contains("Make the crash on open go away"),
            "{}",
            m.content
        );
    }

    #[tokio::test]
    async fn capture_is_idempotent_by_plan_id() {
        let store = make_store();
        let (plan, id) = make_plan(PlanKind::Implementation);
        let dir = tempdir().unwrap();
        let r1 = capture_finish(store.as_ref(), &plan, &id, dir.path(), None, 1000, None)
            .await
            .unwrap();
        let r2 = capture_finish(store.as_ref(), &plan, &id, dir.path(), None, 2000, None)
            .await
            .unwrap();
        // Same deterministic id — no duplicates.
        assert_eq!(r1.plan_memory_id, r2.plan_memory_id);
        let (authored, derived) = store.count_by_class().await.unwrap();
        assert_eq!((authored, derived), (1, 0), "one row, not two");
        // The re-run updated the row in place (newer timestamp).
        let m = store.get_memory(&r2.plan_memory_id).await.unwrap().unwrap();
        assert_eq!(m.created_at, 2000);
    }

    #[tokio::test]
    async fn bug_fixing_plan_also_writes_bug_digest() {
        let store = make_store();
        let (plan, id) = make_plan(PlanKind::BugFixing);
        let dir = tempdir().unwrap();
        let report = capture_finish(store.as_ref(), &plan, &id, dir.path(), None, 1000, None)
            .await
            .unwrap();
        let bug_id = report.bug_memory_id.expect("bug digest written");
        let m = store.get_memory(&bug_id).await.unwrap().unwrap();
        assert_eq!(m.title, "BUG: Fix the crash");
        assert_eq!(m.record_type, MemoryRecordType::Bug);
        assert_eq!(m.record_class, MemoryClass::Authored);
        assert!(
            m.content.chars().count() <= 600,
            "{} chars",
            m.content.chars().count()
        );
        assert!(
            m.content.contains("symptom: the app crashes on open"),
            "{}",
            m.content
        );
        assert!(
            m.content
                .contains("regression test: crash_on_open_regression"),
            "{}",
            m.content
        );
        assert!(
            m.content.contains("path .coding/plans/plan-abc-123.md"),
            "{}",
            m.content
        );
        // The PLAN: digest is written too.
        assert!(store
            .get_memory(&report.plan_memory_id)
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn capture_supersedes_plan_crash_markers_only() {
        let store = make_store();
        let (plan, id) = make_plan(PlanKind::Implementation);
        let dir = tempdir().unwrap();
        // Seed: two markers mentioning the plan + one unrelated marker.
        for (title, content) in [
            ("ACTIVE: fix-crash plan", format!("plan {id} in progress")),
            ("STEP MARKER: fix-crash plan", format!("resume plan {id}")),
            (
                "STEP MARKER: other plan",
                "plan other-999 in progress".to_string(),
            ),
        ] {
            store
                .write(Memory::new(MemoryTier::Working, title, content, 500))
                .await
                .unwrap();
        }

        let report = capture_finish(store.as_ref(), &plan, &id, dir.path(), None, 1000, None)
            .await
            .unwrap();
        assert_eq!(report.markers_superseded, 2, "both plan markers superseded");

        // The superseded markers are excluded from default recall; the
        // unrelated one is untouched and still live. (The two successor
        // "— COMPLETE" rows are live too — they are the new state.)
        let all = store
            .list_filtered(
                &MemoryFilter::new()
                    .tier(MemoryTier::Working)
                    .include_superseded(),
            )
            .await
            .unwrap();
        let superseded: Vec<_> = all.iter().filter(|m| m.superseded_by.is_some()).collect();
        assert_eq!(superseded.len(), 2);
        assert!(
            superseded
                .iter()
                .all(|m| m.title.starts_with("ACTIVE:") || m.title.starts_with("STEP MARKER:")),
            "the two plan markers are superseded"
        );
        let live: Vec<_> = all.iter().filter(|m| m.superseded_by.is_none()).collect();
        assert_eq!(live.len(), 3, "1 unrelated + 2 successors");
        assert!(
            live.iter().any(|m| m.title == "STEP MARKER: other plan"),
            "the unrelated marker stays live"
        );
        assert!(
            live.iter().any(|m| m.title.contains("— COMPLETE")),
            "the successors are live"
        );
    }

    #[tokio::test]
    async fn capture_never_deletes_authored_rows() {
        // The capture writes authored records — a derived-index rebuild must
        // never wipe them (delete_derived is class-scoped).
        let store = make_store();
        let (plan, id) = make_plan(PlanKind::BugFixing);
        let dir = tempdir().unwrap();
        let report = capture_finish(store.as_ref(), &plan, &id, dir.path(), None, 1000, None)
            .await
            .unwrap();
        store.delete_derived().await.unwrap();
        // Both digests survive a derived wipe.
        assert!(store
            .get_memory(&report.plan_memory_id)
            .await
            .unwrap()
            .is_some());
        assert!(store
            .get_memory(&report.bug_memory_id.unwrap())
            .await
            .unwrap()
            .is_some());
    }

    /// A minimal git repo checked out on the given branch (mirrors the
    /// git fixtures in plan.rs / git_ops.rs): the hint probe must READ
    /// git, never guess at a branch name.
    fn git_repo_on_branch(branch: &str) -> tempfile::TempDir {
        let dir = tempdir().unwrap();
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test"]);
        run(&["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.path().join("f.txt"), "x\n").unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-q", "-m", "init"]);
        // Normalize the default branch name (git-init default varies).
        run(&["branch", "-M", "main"]);
        if branch != "main" {
            run(&["checkout", "-q", "-b", branch]);
        }
        dir
    }

    #[tokio::test]
    #[ignore = "integration: spawns real git (branch hint probe); run with `cargo test -- --ignored`"]
    async fn capture_on_a_feature_branch_carries_the_unmerged_hint() {
        let store = make_store();
        let (plan, id) = make_plan(PlanKind::Implementation);
        let repo = git_repo_on_branch("feat/hint-probe");
        let report = capture_finish(
            store.as_ref(),
            &plan,
            &id,
            repo.path(),
            Some(repo.path().to_path_buf()),
            1000,
            None,
        )
        .await
        .unwrap();
        let m = store
            .get_memory(&report.plan_memory_id)
            .await
            .unwrap()
            .unwrap();
        assert!(
            m.content.contains("branch feat/hint-probe @ "),
            "{}",
            m.content
        );
        assert!(
            m.content
                .contains("(unmerged — exists only on this branch)"),
            "{}",
            m.content
        );
        // The hint grows the pointer, never the budget: still ≤400 chars.
        assert!(
            m.content.chars().count() <= 400,
            "{} chars",
            m.content.chars().count()
        );
    }

    #[tokio::test]
    #[ignore = "integration: spawns real git (branch hint probe); run with `cargo test -- --ignored`"]
    async fn capture_on_main_has_no_branch_hint() {
        let store = make_store();
        let (plan, id) = make_plan(PlanKind::Implementation);
        let repo = git_repo_on_branch("main");
        let report = capture_finish(
            store.as_ref(),
            &plan,
            &id,
            repo.path(),
            Some(repo.path().to_path_buf()),
            1000,
            None,
        )
        .await
        .unwrap();
        let m = store
            .get_memory(&report.plan_memory_id)
            .await
            .unwrap()
            .unwrap();
        assert!(!m.content.contains("unmerged"), "{}", m.content);
        assert!(!m.content.contains("branch main"), "{}", m.content);
    }

    #[tokio::test]
    #[ignore = "integration: spawns real git (branch hint probe); run with `cargo test -- --ignored`"]
    async fn capture_bug_digest_on_a_feature_branch_carries_the_unmerged_hint() {
        // The hint must land on BOTH digest pointers — the BUG: digest is a
        // core deliverable (its pointer is built on a separate code path).
        let store = make_store();
        let (plan, id) = make_plan(PlanKind::BugFixing);
        let repo = git_repo_on_branch("feat/hint-probe");
        let report = capture_finish(
            store.as_ref(),
            &plan,
            &id,
            repo.path(),
            Some(repo.path().to_path_buf()),
            1000,
            None,
        )
        .await
        .unwrap();
        let bug_id = report.bug_memory_id.expect("bug digest written");
        let m = store.get_memory(&bug_id).await.unwrap().unwrap();
        assert!(
            m.content.contains("branch feat/hint-probe @ "),
            "{}",
            m.content
        );
        assert!(
            m.content
                .contains("(unmerged — exists only on this branch)"),
            "{}",
            m.content
        );
        // The hint grows the pointer, never the budget: still ≤600 chars.
        assert!(
            m.content.chars().count() <= 600,
            "{} chars",
            m.content.chars().count()
        );
    }

    #[tokio::test]
    async fn bug_capture_with_knowledge_writes_a_file_not_a_row() {
        // The knowledge-backed capture: the BUG: digest lands in a knowledge
        // FILE (`.coding/knowledge/bug/<plan-id>.md`) — the file is the
        // truth, the row comes from the indexer. The reported id is the
        // deterministic knowledge row id, and NO authored BUG row exists.
        let store = make_store();
        let (plan, id) = make_plan(PlanKind::BugFixing);
        let dir = tempdir().unwrap();
        let knowledge = Arc::new(crate::memory::KnowledgeStore::new(
            dir.path().join(".coding/knowledge"),
        ));
        let report = capture_finish(
            store.as_ref(),
            &plan,
            &id,
            dir.path(),
            None,
            1000,
            Some(knowledge.clone()),
        )
        .await
        .unwrap();
        let bug_id = report.bug_memory_id.expect("bug digest written");
        // The file exists with the plan-id slug and carries the key facts.
        let file = dir
            .path()
            .join(".coding/knowledge/bug")
            .join(format!("{id}.md"));
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("Symptom: the app crashes on open"), "{text}");
        assert!(
            text.contains("regression test: crash_on_open_regression"),
            "{text}"
        );
        // The reported id is the knowledge row id (not a plan-scoped uuidv5).
        let expected = crate::memory::indexer::knowledge_id(&format!("bug/{id}.md"));
        assert_eq!(bug_id, expected);
        // No authored BUG row was written (the file replaced it).
        let mut authored_bug = false;
        for m in store.list_filtered(&MemoryFilter::new()).await.unwrap() {
            if m.record_type == MemoryRecordType::Bug {
                authored_bug = true;
            }
        }
        assert!(!authored_bug, "no authored BUG row — the file is the truth");
        // The PLAN: digest still lands as an authored row.
        assert!(store
            .get_memory(&report.plan_memory_id)
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    #[ignore = "integration: spawns real git (branch hint probe); run with `cargo test -- --ignored`"]
    async fn capture_outside_a_git_repo_has_no_branch_hint() {
        // The probe-failure arm: Some(root) where root is NOT a git repo —
        // `git branch --show-current` errors and the hint collapses to None.
        let store = make_store();
        let (plan, id) = make_plan(PlanKind::Implementation);
        let dir = tempdir().unwrap();
        let report = capture_finish(
            store.as_ref(),
            &plan,
            &id,
            dir.path(),
            Some(dir.path().to_path_buf()),
            1000,
            None,
        )
        .await
        .unwrap();
        let m = store
            .get_memory(&report.plan_memory_id)
            .await
            .unwrap()
            .unwrap();
        assert!(!m.content.contains("unmerged"), "{}", m.content);
        assert!(!m.content.contains("branch"), "{}", m.content);
    }
}
