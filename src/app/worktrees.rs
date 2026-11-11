// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Git worktree provisioning for Mnemo2 team workers.
//!
//! Two concurrent agents cannot share one working tree — each dispatched
//! worker gets its own linked worktree at `<main_root>/.worktrees/agent-<id8>`
//! on its own `wt/agent-<id8>` branch, forked from the lead's current
//! working-branch tip (not from `main`) so the worker starts from the work
//! the lead is building on.
//!
//! The branch is the worker's audit trail and the head the lead reviews and
//! merges: [`remove_agent_worktree`] drops only the checkout and KEEPS the
//! branch, while [`rollback_agent_worktree`] undoes a worker that never got
//! going (untouched checkout, branch still at its dispatch tip).
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

/// The 8-char agent-id prefix for a Mnemo2 team worker. Unique per worker
/// in practice (UUID prefixes), and a collision fails loudly at provision.
pub fn agent_short_id(agent_id: &str) -> String {
    agent_id.chars().take(8).collect()
}

/// The branch for one Mnemo2 team worker: `wt/agent-<id8>`, forked from the
/// lead's current branch tip (not from `main`) so the worker starts from the
/// work the lead is building on.
pub fn agent_branch(agent_id: &str) -> String {
    format!("wt/agent-{}", agent_short_id(agent_id))
}

/// The current branch of `main_root` — the lead's working-branch tip that a
/// new worker forks from. A detached HEAD is refused: a worker must have a
/// branch to fork and to land back onto.
pub fn current_branch(main_root: &Path) -> Result<String, String> {
    let branch = git_raw(main_root, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let branch = branch.trim().to_string();
    if branch.is_empty() || branch == "HEAD" {
        return Err("the project is on a detached HEAD; check out a branch first".to_string());
    }
    Ok(branch)
}

/// Provision a linked worktree for one Mnemo2 team worker, on its own branch
/// forked from `base_ref` (the lead branch tip). Returns
/// `(worktree_root, branch)` at `<main_root>/.worktrees/agent-<id8>`.
///
/// Runs the blocking git subprocess on the blocking thread pool (see the
/// module doc). Owns its arguments so the closure is `'static + Send`.
pub async fn provision_agent_worktree(
    main_root: PathBuf,
    agent_id: String,
    base_ref: String,
) -> Result<(PathBuf, String), String> {
    tokio::task::spawn_blocking(move || {
        provision_agent_worktree_impl(&main_root, &agent_id, &base_ref)
    })
    .await
    .map_err(|e| format!("worktree provision task failed: {e}"))?
}

/// The synchronous core of [`provision_agent_worktree`].
fn provision_agent_worktree_impl(
    main_root: &Path,
    agent_id: &str,
    base_ref: &str,
) -> Result<(PathBuf, String), String> {
    let short = agent_short_id(agent_id);
    let branch = agent_branch(agent_id);
    let worktree =
        provision_worktree_impl(main_root, &format!("agent-{short}"), &branch, base_ref)?;
    Ok((worktree, branch))
}

/// Shared worktree provisioning: create `<main_root>/.worktrees/<dir_name>`
/// on a new `branch` forked from `base_ref`. `-b` refuses an existing branch
/// name, so two agents can never share a branch.
fn provision_worktree_impl(
    main_root: &Path,
    dir_name: &str,
    branch: &str,
    base_ref: &str,
) -> Result<PathBuf, String> {
    if base_ref.trim().is_empty() {
        return Err("worktree base ref is empty".to_string());
    }
    // Fail before creating the worktree when the base ref is unknown — the
    // caller gets a clear base-ref error instead of a half-provisioned tree.
    git_raw(
        main_root,
        &["rev-parse", "--verify", &format!("{base_ref}^{{commit}}")],
    )?;
    let worktree = worktrees_dir(main_root).join(dir_name);
    git_raw(
        main_root,
        &[
            "worktree",
            "add",
            "-b",
            branch,
            worktree.to_str().ok_or("non-UTF-8 worktree path")?,
            base_ref,
        ],
    )?;
    Ok(worktree)
}

/// Remove a worker's worktree WITHOUT deleting its branch — the branch
/// is the audit trail of what the worker did and the head the lead
/// reviewed or merged. Used by [`remove_agent_worktree`].
pub async fn remove_worktree_only(main_root: PathBuf, worktree: PathBuf) -> Result<(), String> {
    tokio::task::spawn_blocking(move || remove_worktree_only_impl(&main_root, &worktree))
        .await
        .map_err(|e| format!("worktree remove task failed: {e}"))?
}

/// Remove a Mnemo2 worker's worktree but KEEP its `wt/agent-*` branch: the
/// branch is the audit trail of what the worker did and the head the lead
/// reviewed or merged. Reuses [`remove_worktree_only`] so the branch survives
/// cleanup after integration.
pub async fn remove_agent_worktree(main_root: PathBuf, agent_id: String) -> Result<(), String> {
    let worktree = worktrees_dir(&main_root).join(format!("agent-{}", agent_short_id(&agent_id)));
    remove_worktree_only(main_root, worktree).await
}

/// Roll back only a newly provisioned, untouched Mnemo2 worker checkout.
/// The caller must have created this exact worker ID during this dispatch.
pub async fn rollback_agent_worktree(
    main_root: PathBuf,
    agent_id: String,
    base_tip: String,
) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        if agent_id.is_empty()
            || !agent_id.chars().all(|character| character.is_ascii_alphanumeric())
        {
            return Err("invalid worker identity; branch retained".to_string());
        }
        let branch = agent_branch(&agent_id);
        let worktree = worktrees_dir(&main_root).join(format!("agent-{}", agent_short_id(&agent_id)));
        if current_branch(&worktree)? != branch {
            return Err("worker checkout is on an unexpected branch; branch retained".to_string());
        }
        let dirty = git_raw(&worktree, &["status", "--porcelain", "-uall"])?;
        if !dirty.trim().is_empty() {
            return Err("worker checkout has uncommitted work; branch retained".to_string());
        }
        let revision = format!("refs/heads/{branch}^{{commit}}");
        let tip = git_raw(&main_root, &["rev-parse", "--verify", &revision])?;
        if tip.trim() != base_tip {
            return Err("worker branch advanced after dispatch; branch retained".to_string());
        }
        remove_worktree_only_impl(&main_root, &worktree)?;
        let tip = git_raw(&main_root, &["rev-parse", "--verify", &revision])?;
        if tip.trim() != base_tip {
            return Err("worker branch advanced during rollback; branch retained".to_string());
        }
        git_raw(
            &main_root,
            &["update-ref", "-d", &format!("refs/heads/{branch}"), &base_tip],
        )?;
        Ok(())
    })
    .await
    .map_err(|error| format!("worker rollback task failed: {error}"))?
}

/// The synchronous core of [`remove_worktree_only`]. Prunes stale
/// worktree metadata first (review R3-L2): a worktree whose directory
/// is already gone (deleted manually) leaves stale metadata that makes
/// the remove (and any branch delete) fail ("cannot delete branch used
/// by worktree").
fn remove_worktree_only_impl(main_root: &Path, worktree: &Path) -> Result<(), String> {
    let worktree_str = worktree
        .to_str()
        .ok_or("non-UTF-8 worktree path")?
        .to_string();
    let _ = git_raw(main_root, &["worktree", "prune"]);
    git_raw(main_root, &["worktree", "remove", "--force", &worktree_str]).map(|_| ())
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
    async fn remove_worktree_only_keeps_the_branch() {
        // A worker's branch is its audit trail and the head the lead
        // reviewed/merged — removing the checkout must leave it in place.
        let repo = init_repo();
        let (worktree, branch) = provision_agent_worktree(
            repo.path().to_path_buf(),
            "abcdef123456".to_string(),
            "main".to_string(),
        )
        .await
        .unwrap();
        remove_worktree_only(repo.path().to_path_buf(), worktree.clone())
            .await
            .unwrap();
        assert!(!worktree.exists(), "the worktree directory is gone");
        assert!(
            !git_raw(repo.path(), &["branch", "--list", &branch])
                .unwrap()
                .is_empty(),
            "the branch is KEPT (the audit trail)"
        );
    }

    #[tokio::test]
    async fn provision_agent_worktree_forks_from_the_supplied_base() {
        let repo = init_repo();
        git_raw(repo.path(), &["checkout", "-b", "wt/lead"]).unwrap();
        std::fs::write(repo.path().join("lead.txt"), "lead\n").unwrap();
        git_raw(repo.path(), &["add", "-A"]).unwrap();
        git_raw(repo.path(), &["commit", "-m", "lead work"]).unwrap();

        let (worktree, branch) = provision_agent_worktree(
            repo.path().to_path_buf(),
            "abcdef123456".to_string(),
            "wt/lead".to_string(),
        )
        .await
        .expect("provision");
        assert_eq!(branch, "wt/agent-abcdef12");
        let lead = std::fs::read_to_string(worktree.join("lead.txt")).unwrap();
        assert_eq!(
            lead.trim_end(),
            "lead",
            "the worker forks from the lead tip, not main"
        );
        assert!(
            git_raw(repo.path(), &["show", "main:lead.txt"]).is_err(),
            "the lead commit is not on main"
        );
    }

    #[tokio::test]
    async fn provision_agent_worktree_refuses_a_duplicate_branch() {
        let repo = init_repo();
        provision_agent_worktree(
            repo.path().to_path_buf(),
            "abcdef123456".to_string(),
            "main".to_string(),
        )
        .await
        .expect("first provision");
        let err = provision_agent_worktree(
            repo.path().to_path_buf(),
            "abcdef123456".to_string(),
            "main".to_string(),
        )
        .await
        .expect_err("a duplicate branch must fail");
        assert!(err.contains("already exists"), "{err}");
    }

    #[tokio::test]
    async fn remove_agent_worktree_keeps_the_branch() {
        let repo = init_repo();
        let (worktree, branch) = provision_agent_worktree(
            repo.path().to_path_buf(),
            "abcdef123456".to_string(),
            "main".to_string(),
        )
        .await
        .expect("provision");
        remove_agent_worktree(repo.path().to_path_buf(), "abcdef123456".to_string())
            .await
            .expect("cleanup");
        assert!(!worktree.exists(), "the worker checkout is gone");
        assert!(
            !git_raw(repo.path(), &["branch", "--list", &branch])
                .unwrap()
                .trim()
                .is_empty(),
            "the worker branch is kept for audit"
        );
    }
}
