// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Git worktree provisioning + landing for parallel run-all (plan
//! ffd7a86f).
//!
//! Two concurrent agents cannot share one working tree — each dispatched
//! item beyond the first gets its own linked worktree on its own
//! `wt/runall-<item8>` branch, forked from `main` (each item starts from
//! the landed state, not from another item's in-flight work). Landing is
//! app-side and serialized by the caller: the merge_to_main skill cannot
//! run from a linked worktree (git refuses to check out `main` there —
//! it is checked out in the main worktree), so when a spawned item
//! resolves Done the app merges its branch into `main` via a dedicated
//! landing worktree, deletes the branch, and removes the item worktree.
//! Merge conflicts are SURFACED, never auto-resolved: the merge is
//! aborted, the branch + worktree are kept, and the conflicted file list
//! is returned for the item note.
//!
//! All functions run blocking git subprocesses on the blocking thread
//! pool (see `git_ops` for why). Unlike the rest of `git_ops`, the tests
//! here use REAL git in tempdir repos — worktrees are filesystem-level
//! (a linked worktree is a real directory with a real checkout + `.git`
//! file), which the in-memory `MockGit` cannot represent.

use std::path::{Path, PathBuf};

use crate::project::git_ops::git_raw;

/// The worktree root: `<main_root>/.worktrees` (gitignored — see the
/// `.gitignore` entry).
fn worktrees_dir(main_root: &Path) -> PathBuf {
    main_root.join(".worktrees")
}

/// The 8-char item-id prefix shared by the branch name, the worktree dir
/// name, and the spawned agent's name (plan ffd7a86f): unique per item in
/// practice (UUID prefixes), and a collision fails loudly at provision
/// (the branch already exists).
pub fn item_short_id(item_id: &str) -> String {
    item_id.chars().take(8).collect()
}

/// The branch for one dispatched item: `wt/runall-<item8>` — unique per
/// item, so two agents can never share a branch (`git worktree add -b`
/// refuses an existing branch name).
pub fn item_branch(item_id: &str) -> String {
    format!("wt/runall-{}", item_short_id(item_id))
}

/// Provision a linked worktree for one dispatched run-all item, on its
/// own branch forked from `main`.
///
/// Returns `(worktree_root, branch)`. The worktree lands at
/// `<main_root>/.worktrees/runall-<item8>`; the branch is
/// `wt/runall-<item8>`. Fails when the branch already exists (e.g. a
/// previous run of the same item never landed) — the caller surfaces the
/// error instead of silently reusing another agent's branch.
///
/// Runs the blocking git subprocess on the blocking thread pool (see the
/// module doc). Owns its arguments so the closure is `'static + Send`.
pub async fn provision_item_worktree(
    main_root: PathBuf,
    item_id: String,
) -> Result<(PathBuf, String), String> {
    tokio::task::spawn_blocking(move || provision_item_worktree_impl(&main_root, &item_id))
        .await
        .map_err(|e| format!("worktree provision task failed: {e}"))?
}

/// The synchronous core of [`provision_item_worktree`].
fn provision_item_worktree_impl(
    main_root: &Path,
    item_id: &str,
) -> Result<(PathBuf, String), String> {
    let short = item_short_id(item_id);
    let branch = item_branch(item_id);
    let worktree = worktrees_dir(main_root).join(format!("runall-{short}"));
    // `-b` forks the branch from `main` at provision time and refuses an
    // existing branch name — two agents can never share a branch.
    git_raw(
        main_root,
        &[
            "worktree",
            "add",
            "-b",
            &branch,
            worktree.to_str().ok_or("non-UTF-8 worktree path")?,
            "main",
        ],
    )?;
    Ok((worktree, branch))
}

/// Remove a dispatched item's worktree and delete its branch — after the
/// branch landed, or when the item was requeued and the worktree is no
/// longer needed.
///
/// `--force`: the agent may have left untracked build output behind; the
/// branch's commits are what matter (a landed branch is already merged;
/// a requeued item's branch is re-provisioned fresh from `main` on the
/// next dispatch).
pub async fn remove_item_worktree(
    main_root: PathBuf,
    worktree: PathBuf,
    branch: String,
) -> Result<(), String> {
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        let worktree_str = worktree
            .to_str()
            .ok_or("non-UTF-8 worktree path")?
            .to_string();
        // Prune stale worktree metadata first (review R3-L2): a worktree
        // whose directory is already gone (deleted manually) leaves stale
        // metadata that makes BOTH the remove and the branch delete fail
        // ("cannot delete branch used by worktree"). Then run both ops
        // independently — a remove failure must not skip the branch
        // delete: a stale `wt/runall-*` branch blocks every later
        // spawned-lane dispatch of the item ("branch already exists" →
        // the fill's skip arm → permanently undispatchable via lanes).
        let _ = git_raw(&main_root, &["worktree", "prune"]);
        let removed = git_raw(&main_root, &["worktree", "remove", "--force", &worktree_str]);
        let deleted = git_raw(&main_root, &["branch", "-D", &branch]);
        removed?;
        deleted?;
        Ok(())
    })
    .await
    .map_err(|e| format!("worktree remove task failed: {e}"))?
}

/// Why a landing failed — the caller treats the arms differently
/// (conflicts are surfaced on the item and the work still counts as
/// complete; git errors are infrastructure failures).
#[derive(Debug)]
pub enum LandError {
    /// The merge conflicted: it was aborted, the branch is KEPT for
    /// manual resolution, and the conflicted paths are carried for the
    /// item note (surfaced, never auto-resolved).
    Conflict(Vec<String>),
    /// A git infrastructure error (landing-worktree provisioning, the
    /// merge failing for a non-conflict reason, branch delete).
    Git(String),
}

/// Land one dispatched item's branch into `main` via the dedicated
/// landing worktree.
///
/// The landing worktree (`<main_root>/.worktrees/landing`, `main`
/// checked out) is provisioned lazily and reused across landings —
/// `main` cannot be checked out in a linked worktree while the main
/// worktree holds a different branch, and the main worktree itself may
/// be mid-item (the main agent's uncommitted work must never be touched
/// by a landing). The CALLER serializes landings (one at a time).
///
/// `Ok(())` — merged (`--no-ff`, so `main` only ever receives merge
/// commits, mirroring the merge_to_main skill). The branch delete +
/// worktree removal happen in [`remove_item_worktree`] afterwards (git
/// refuses to delete a branch its worktree still has checked out).
/// `Err(LandError::Conflict(files))` — the merge conflicted: it was
/// aborted (the landing worktree is back on clean `main`), the branch is
/// KEPT for manual resolution, and the conflicted paths are returned for
/// the item note.
pub async fn land_item_branch(main_root: PathBuf, branch: String) -> Result<(), LandError> {
    tokio::task::spawn_blocking(move || land_item_branch_impl(&main_root, &branch))
        .await
        .map_err(|e| LandError::Git(format!("landing task failed: {e}")))?
}

/// The synchronous core of [`land_item_branch`].
fn land_item_branch_impl(main_root: &Path, branch: &str) -> Result<(), LandError> {
    let landing = worktrees_dir(main_root).join("landing");
    if !landing.exists() {
        let landing_str = landing
            .to_str()
            .ok_or_else(|| LandError::Git("non-UTF-8 landing path".into()))?
            .to_string();
        git_raw(main_root, &["worktree", "add", &landing_str, "main"])
            .map_err(LandError::Git)?;
    }
    // Re-sync the landing tree to `main` before merging: refs are shared
    // (a landing elsewhere advanced `main`), but this checkout's tree can
    // be stale — and it must be clean for the merge. `reset --hard` is
    // safe here: the landing worktree holds no work of its own.
    git_raw(&landing, &["reset", "--hard", "main"]).map_err(LandError::Git)?;
    // Sync `main` with its upstream BEFORE merging (mirrors the merge_to_main
    // skill's step 2, plan d826b9ad): landing onto a stale local `main` only
    // surfaces later as a rejected push, and these landings are app-managed —
    // nobody is watching for it.
    //
    // Tolerance is NARROW (review H1): a branch with no upstream at all is
    // normal (local-only repos, never-pushed branches) and must never fail a
    // landing, so that case skips the sync entirely. But once `main` TRACKS a
    // remote, a fetch/pull failure is a real problem — a transient network
    // error or expired credentials would otherwise land the branch onto exactly
    // the stale `main` this sync exists to prevent — so it surfaces as a
    // LandError instead of being swallowed.
    let tracks_a_remote = git_raw(
        &landing,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    )
    .is_ok();
    if tracks_a_remote {
        git_raw(&landing, &["fetch", "origin"]).map_err(LandError::Git)?;
        git_raw(&landing, &["pull", "--no-rebase"]).map_err(LandError::Git)?;
    }
    // --no-ff: main only ever receives merge commits (the branch policy),
    // mirroring the merge_to_main skill's merge.
    let merge = git_raw(
        &landing,
        &[
            "merge",
            "--no-ff",
            branch,
            "-m",
            &format!("merge {branch} into main"),
        ],
    );
    match merge {
        // Merged — the branch delete + worktree removal happen in
        // remove_item_worktree (git refuses to delete a branch its
        // worktree still has checked out).
        Ok(_) => Ok(()),
        Err(merge_err) => {
            // Capture the conflicted paths BEFORE aborting (the abort
            // clears the index state that --diff-filter=U reads).
            let files =
                git_raw(&landing, &["diff", "--name-only", "--diff-filter=U"])
                    .unwrap_or_default();
            let conflicts: Vec<String> = files
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect();
            let _ = git_raw(&landing, &["merge", "--abort"]);
            if conflicts.is_empty() {
                // The merge failed for a non-conflict reason (e.g.
                // unrelated histories) — an infrastructure error, not a
                // surfaced conflict.
                Err(LandError::Git(merge_err))
            } else {
                Err(LandError::Conflict(conflicts))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Init a tiny REAL git repo with a `main` branch and one commit.
    /// Worktrees are filesystem-level (a linked worktree is a real
    /// directory with a real checkout) — the in-memory `MockGit` cannot
    /// represent them, so this module's tests spawn real git in tempdirs
    /// (a handful of tests, bounded cost; see the module doc).
    fn init_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git_raw(dir.path(), &["init", "-b", "main"]).unwrap();
        git_raw(dir.path(), &["config", "user.email", "t@t"]).unwrap();
        git_raw(dir.path(), &["config", "user.name", "t"]).unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        git_raw(dir.path(), &["add", "-A"]).unwrap();
        git_raw(dir.path(), &["commit", "-m", "init"]).unwrap();
        dir
    }

    #[tokio::test]
    async fn land_syncs_main_with_its_upstream_before_merging() {
        // Follow-up B (plan f69317d4, 2027-01-11): the app-managed landing used
        // to merge into a possibly STALE local main — the same defect plan
        // d826b9ad fixed in the merge_to_main skill — and nobody is watching
        // these landings for the rejected push that surfaces hours later.
        let upstream = tempfile::tempdir().unwrap();
        git_raw(upstream.path(), &["init", "--bare", "-b", "main"]).unwrap();
        let repo = init_repo();
        let upstream_str = upstream.path().to_str().unwrap();
        git_raw(repo.path(), &["remote", "add", "origin", upstream_str]).unwrap();
        git_raw(repo.path(), &["push", "-u", "origin", "main"]).unwrap();
        // A second clone advances the upstream, so the local main is behind.
        let other = tempfile::tempdir().unwrap();
        let clone = other.path().join("clone");
        let clone_str = clone.to_str().unwrap();
        git_raw(other.path(), &["clone", upstream_str, clone_str]).unwrap();
        git_raw(&clone, &["config", "user.email", "t@t"]).unwrap();
        git_raw(&clone, &["config", "user.name", "t"]).unwrap();
        std::fs::write(clone.join("upstream.txt"), "from upstream\n").unwrap();
        git_raw(&clone, &["add", "-A"]).unwrap();
        git_raw(&clone, &["commit", "-m", "upstream-change"]).unwrap();
        git_raw(&clone, &["push", "origin", "main"]).unwrap();
        // Production shape: the main worktree sits on the agent's working
        // branch, which is what frees `main` for the landing worktree.
        git_raw(repo.path(), &["checkout", "-b", "wt/test"]).unwrap();

        let (worktree, branch) =
            provision_item_worktree(repo.path().to_path_buf(), "abcdef123456".to_string())
                .await
                .unwrap();
        std::fs::write(worktree.join("item.txt"), "item work\n").unwrap();
        git_raw(&worktree, &["add", "-A"]).unwrap();
        git_raw(&worktree, &["commit", "-m", "item work"]).unwrap();

        land_item_branch(repo.path().to_path_buf(), branch)
            .await
            .unwrap();
        let log = git_raw(repo.path(), &["log", "--oneline", "main"]).unwrap();
        assert!(
            log.contains("upstream-change"),
            "the landing must sync main with origin first, got: {log}"
        );
        assert!(log.contains("item work"));
    }

    #[tokio::test]
    async fn land_fails_when_a_configured_remote_cannot_be_reached() {
        // Review H1: the sync must not swallow EVERY error. Once main tracks a
        // remote, an unreachable one (network share gone, credentials expired)
        // must surface — landing anyway would recreate the stale-main defect
        // this sync exists to prevent.
        let upstream = tempfile::tempdir().unwrap();
        git_raw(upstream.path(), &["init", "--bare", "-b", "main"]).unwrap();
        let repo = init_repo();
        let upstream_str = upstream.path().to_str().unwrap().to_string();
        git_raw(repo.path(), &["remote", "add", "origin", &upstream_str]).unwrap();
        git_raw(repo.path(), &["push", "-u", "origin", "main"]).unwrap();
        git_raw(repo.path(), &["checkout", "-b", "wt/test"]).unwrap();
        // The remote disappears from under the landing.
        std::fs::remove_dir_all(&upstream_str).unwrap();

        let (worktree, branch) =
            provision_item_worktree(repo.path().to_path_buf(), "abcdef123456".to_string())
                .await
                .unwrap();
        std::fs::write(worktree.join("item.txt"), "item work\n").unwrap();
        git_raw(&worktree, &["add", "-A"]).unwrap();
        git_raw(&worktree, &["commit", "-m", "item work"]).unwrap();

        let result = land_item_branch(repo.path().to_path_buf(), branch).await;
        assert!(
            result.is_err(),
            "a tracked-but-unreachable remote must surface, not land on stale main"
        );
    }

    #[tokio::test]
    async fn land_still_succeeds_without_any_remote() {
        // The sync must tolerate a repo with no remote/upstream (local-only
        // repos): a landing may never fail because of it.
        let repo = init_repo();
        // Production shape: the main worktree holds the agent's branch, so the
        // landing worktree can check out `main`.
        git_raw(repo.path(), &["checkout", "-b", "wt/test"]).unwrap();
        let (worktree, branch) =
            provision_item_worktree(repo.path().to_path_buf(), "abcdef123456".to_string())
                .await
                .unwrap();
        std::fs::write(worktree.join("item.txt"), "item work\n").unwrap();
        git_raw(&worktree, &["add", "-A"]).unwrap();
        git_raw(&worktree, &["commit", "-m", "item work"]).unwrap();

        land_item_branch(repo.path().to_path_buf(), branch)
            .await
            .unwrap();
        let log = git_raw(repo.path(), &["log", "--oneline", "main"]).unwrap();
        assert!(log.contains("item work"), "got: {log}");
    }

    #[test]
    fn item_short_id_and_branch_share_the_prefix() {
        // Review L2: the branch, the worktree dir, and the spawned agent's
        // name all derive from the one 8-char prefix helper — never
        // triplicated.
        assert_eq!(item_short_id("abcdef123456"), "abcdef12");
        assert_eq!(item_branch("abcdef123456"), "wt/runall-abcdef12");
    }

    #[tokio::test]
    async fn remove_still_deletes_the_branch_when_the_worktree_is_already_gone() {
        // Review R3-L2: a worktree-remove failure (the directory already
        // gone) must not skip the branch delete — a stale branch blocks
        // every later spawned-lane dispatch of the item ("branch already
        // exists" → the fill's skip arm → permanently undispatchable via
        // lanes). The prune clears the stale metadata that would make the
        // branch delete refuse ("cannot delete branch used by worktree").
        let repo = init_repo();
        let (worktree, branch) =
            provision_item_worktree(repo.path().to_path_buf(), "abcdef123456".to_string())
                .await
                .unwrap();
        // Simulate the worktree directory vanishing (deleted manually).
        std::fs::remove_dir_all(&worktree).unwrap();
        let _ = remove_item_worktree(repo.path().to_path_buf(), worktree, branch.clone())
            .await;
        assert!(
            git_raw(repo.path(), &["branch", "--list", &branch])
                .unwrap()
                .is_empty(),
            "the branch is deleted even though the worktree was already gone"
        );
    }

    #[tokio::test]
    async fn provision_creates_worktree_on_its_own_branch() {
        let repo = init_repo();
        let (worktree, branch) =
            provision_item_worktree(repo.path().to_path_buf(), "abcdef123456".to_string())
                .await
                .unwrap();
        assert_eq!(branch, "wt/runall-abcdef12");
        assert!(worktree.ends_with(".worktrees/runall-abcdef12"));
        // The worktree is a real checkout of main.
        assert!(worktree.join("a.txt").exists());
        // The main worktree is untouched (still on main).
        assert_eq!(
            git_raw(repo.path(), &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap(),
            "main\n"
        );
        // A second item gets a DIFFERENT branch (never two agents on one
        // branch).
        let (_, branch2) =
            provision_item_worktree(repo.path().to_path_buf(), "zzzzzzzz1234".to_string())
                .await
                .unwrap();
        assert_ne!(branch, branch2);
    }

    #[tokio::test]
    async fn provision_refuses_an_existing_branch() {
        let repo = init_repo();
        provision_item_worktree(repo.path().to_path_buf(), "abcdef123456".to_string())
            .await
            .unwrap();
        // The same item id again (e.g. a re-dispatch before the previous
        // run landed) must NOT silently reuse the existing branch.
        assert!(
            provision_item_worktree(repo.path().to_path_buf(), "abcdef123456".to_string())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn remove_cleans_up_worktree_and_branch() {
        let repo = init_repo();
        let (worktree, branch) =
            provision_item_worktree(repo.path().to_path_buf(), "abcdef123456".to_string())
                .await
                .unwrap();
        remove_item_worktree(repo.path().to_path_buf(), worktree.clone(), branch.clone())
            .await
            .unwrap();
        assert!(!worktree.exists(), "the worktree directory is gone");
        assert!(
            git_raw(repo.path(), &["branch", "--list", &branch])
                .unwrap()
                .is_empty(),
            "the branch is deleted"
        );
    }

    #[tokio::test]
    async fn land_merges_into_main_and_deletes_branch() {
        let repo = init_repo();
        let (worktree, branch) =
            provision_item_worktree(repo.path().to_path_buf(), "abcdef123456".to_string())
                .await
                .unwrap();
        // Commit work on the item's branch (in its worktree).
        std::fs::write(worktree.join("b.txt"), "item work\n").unwrap();
        git_raw(&worktree, &["add", "-A"]).unwrap();
        git_raw(&worktree, &["commit", "-m", "item work"]).unwrap();
        // Mirror production: the main worktree sits on its wt/* branch
        // (NOT main), leaving `main` free for the landing worktree.
        git_raw(repo.path(), &["checkout", "-b", "wt/test-main"]).unwrap();

        land_item_branch(repo.path().to_path_buf(), branch.clone())
            .await
            .unwrap();
        // The caller's post-landing cleanup: remove the worktree + delete
        // the (now merged) branch.
        remove_item_worktree(repo.path().to_path_buf(), worktree.clone(), branch.clone())
            .await
            .unwrap();

        // main now has the work via a MERGE commit (never a
        // fast-forward).
        let log = git_raw(repo.path(), &["log", "--merges", "--oneline", "main"]).unwrap();
        assert!(
            log.contains("merge wt/runall-abcdef12"),
            "the landing is a merge commit: {log}"
        );
        // The landed file is on main.
        assert_eq!(
            git_raw(repo.path(), &["show", "main:b.txt"]).unwrap(),
            "item work\n"
        );
        // The worktree and branch are cleaned up.
        assert!(!worktree.exists(), "the worktree directory is gone");
        assert!(
            git_raw(repo.path(), &["branch", "--list", &branch])
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn land_conflict_surfaces_files_and_keeps_branch() {
        let repo = init_repo();
        // The item branch changes a.txt; main also changes a.txt
        // differently (e.g. the main agent's item landed first).
        let (worktree, branch) =
            provision_item_worktree(repo.path().to_path_buf(), "abcdef123456".to_string())
                .await
                .unwrap();
        std::fs::write(worktree.join("a.txt"), "item change\n").unwrap();
        git_raw(&worktree, &["add", "-A"]).unwrap();
        git_raw(&worktree, &["commit", "-m", "item change"]).unwrap();
        // Advance main (still checked out in the main worktree at this
        // point) with the conflicting change…
        std::fs::write(repo.path().join("a.txt"), "main change\n").unwrap();
        git_raw(repo.path(), &["add", "-A"]).unwrap();
        git_raw(repo.path(), &["commit", "-m", "main change"]).unwrap();
        // …then mirror production: the main worktree moves to its wt/*
        // branch, leaving `main` free for the landing worktree.
        git_raw(repo.path(), &["checkout", "-b", "wt/test-main"]).unwrap();

        let conflicts = land_item_branch(repo.path().to_path_buf(), branch.clone())
            .await
            .unwrap_err();
        match conflicts {
            LandError::Conflict(files) => assert_eq!(files, vec!["a.txt".to_string()]),
            other => panic!("expected a conflict, got: {other:?}"),
        }
        // The branch is KEPT for manual resolution.
        assert!(
            !git_raw(repo.path(), &["branch", "--list", &branch])
                .unwrap()
                .is_empty(),
            "the conflicted branch is kept"
        );
        // main does NOT contain the item's change (the merge was
        // aborted).
        assert_eq!(
            git_raw(repo.path(), &["show", "main:a.txt"]).unwrap(),
            "main change\n"
        );
    }
}
