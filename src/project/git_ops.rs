// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Git operations shared by the workflow engine — Run-All checkpoint,
//! commit-success, and rollback, plus plan branch preparation
//! (`prepare_branch`) and the finish-digest branch probe (`branch_hint`).
//!
//! These run blocking git subprocesses on the blocking thread pool
//! (`tokio::task::spawn_blocking`) so a multi-second `git add -A && git commit`
//! (large repo, Windows process spawn + AV scanning) cannot stall the async
//! runtime — which would block every agent's event fan-in (cap 256) and stall
//! all generation.
//!
//! Console windows are suppressed on Windows so no terminal pops up while the
//! GUI app is running.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use crate::backlog::BacklogItem;

/// Run a git command in `root`, returning its stdout on success, WITHOUT
/// trimming. Trimming is dangerous for output where leading whitespace is
/// data — e.g. `git status --porcelain`, whose first line begins with the
/// two-column status flags that are frequently spaces (` M path`): a trim
/// would eat the leading flag space and shift every path parse.
///
/// `pub(crate)` so the worktree module (parallel run-all, plan ffd7a86f)
/// reuses the same hardened runner (CREATE_NO_WINDOW etc.) instead of
/// duplicating it.
pub(crate) fn git_raw(root: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.args(args).current_dir(root);

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let output = cmd
        .output()
        .map_err(|e| format!("failed to spawn git {args:?}: {e}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        Err(format!(
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// Run a `gh` (GitHub CLI) subcommand in `root`, returning stdout on
/// success — the gh twin of [`git_raw`], same hardening
/// (CREATE_NO_WINDOW on Windows, stderr on failure).
///
/// `pub(crate)` for the same reason as [`git_raw`]: the worktree
/// module's ruleset-aware landing (backlog b52b041a) reuses the
/// hardened runner instead of duplicating it. Failures are EXPECTED
/// and handled by the caller — gh missing / unauthenticated means the
/// landing takes its direct path (mirroring the merge_to_main skill's
/// decision: gh-missing also means `gh pr create` would fail).
pub(crate) fn gh_raw(root: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new("gh");
    cmd.args(args).current_dir(root);

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let output = cmd
        .output()
        .map_err(|e| format!("failed to spawn gh {args:?}: {e}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        Err(format!(
            "gh {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// Abstraction over git command execution, so tests can mock git instead of
/// spawning a real `git` process. Production uses [`RealGit`] (spawns `git`);
/// tests use [`MockGit`] (an in-memory mini-git) so the test suite doesn't
/// depend on git being installed, is fast (no subprocess spawning), and
/// doesn't cause nextest to hang (subprocess pipe handle inheritance on
/// Windows).
///
/// The trait returns **raw** (untrimmed) stdout — callers that need trimmed
/// output use [`git_run`] (mirrors `git`); callers that need raw output use
/// [`git_run_raw`] (mirrors `git_raw`).
pub trait GitRunner: Send + Sync {
    /// Run a git command in `root`, returning raw stdout on success.
    fn run(&self, root: &Path, args: &[&str]) -> Result<String, String>;
}

/// Production git runner: spawns `git` as a subprocess (the historical
/// behavior). Delegates to [`git_raw`] to avoid duplicating the
/// `CREATE_NO_WINDOW` / env-scrubbing hardening.
pub(crate) struct RealGit;

impl GitRunner for RealGit {
    fn run(&self, root: &Path, args: &[&str]) -> Result<String, String> {
        git_raw(root, args)
    }
}

/// Run a git command via a [`GitRunner`], returning **trimmed** stdout on
/// success. Mirrors [`git`] for code that doesn't need leading-whitespace
/// data (i.e. everything except `status --porcelain`).
fn git_run(git: &dyn GitRunner, root: &Path, args: &[&str]) -> Result<String, String> {
    git.run(root, args).map(|s| s.trim().to_string())
}

/// Run a git command via a [`GitRunner`], returning **raw** (untrimmed)
/// stdout on success. Mirrors [`git_raw`] for output where leading whitespace
/// is data (e.g. `status --porcelain`).
fn git_run_raw(git: &dyn GitRunner, root: &Path, args: &[&str]) -> Result<String, String> {
    git.run(root, args)
}

/// Validate a commit-ish used as a diff BASE (backlog 85313a7e).
///
/// The strictness mirrors the `git_read` tools' commit-ish guard: a leading
/// `-` is option injection, and whitespace / shell metacharacters have no
/// place in a revision. The reviewer-round base arrives from a value recorded
/// on the plan frame (`.coding/plans/<id>.md`) — a file a hand-edit can
/// change — so it is treated as untrusted input.
pub(crate) fn validate_base_ref(base: &str) -> Result<(), String> {
    if base.is_empty() {
        return Err("base revision rejected — empty".to_string());
    }
    if base.starts_with('-') {
        return Err(format!(
            "base revision '{base}' rejected — a commit-ish may not start with '-' \
             (option injection)"
        ));
    }
    if base
        .chars()
        .any(|c| c.is_whitespace() || "|&;<>`$(){}*?\"'\\".contains(c))
    {
        return Err(format!(
            "base revision '{base}' rejected — whitespace and shell metacharacters \
             are not allowed in a commit-ish"
        ));
    }
    Ok(())
}

/// The current HEAD sha in `root` — the base revision a reviewer round stamps
/// when it is dispatched (see [`crate::workflow::Workflow::record_review_round`]).
/// Runs on the blocking thread pool like every git op here and owns its
/// argument so the closure is `'static + Send`.
pub async fn head_commit_sha(root: PathBuf) -> Result<String, String> {
    tokio::task::spawn_blocking(move || head_commit_sha_impl(&RealGit, &root))
        .await
        .map_err(|e| format!("head-commit task failed: {e}"))?
}

/// The synchronous core of [`head_commit_sha`] — split out so tests can inject
/// a [`GitRunner`] mock instead of spawning a real `git` process.
fn head_commit_sha_impl(git: &dyn GitRunner, root: &Path) -> Result<String, String> {
    git_run(git, root, &["rev-parse", "HEAD"])
}

/// Everything a reviewer must look at to verify what changed since `base`:
/// `git diff --name-status <base>` (the working tree against the base —
/// staged and unstaged together, which IS a reviewer's scope) plus every
/// untracked file from `git status --porcelain -uall`.
///
/// One entry per line as `<status> <path>` (`M src/a.rs`, `?? docs/new.md`),
/// so a caller can list them verbatim. Gitignored paths never appear —
/// `status` omits them unless `--ignored` is passed.
pub async fn changed_paths_since(root: PathBuf, base: String) -> Result<Vec<String>, String> {
    tokio::task::spawn_blocking(move || changed_paths_since_impl(&RealGit, &root, &base))
        .await
        .map_err(|e| format!("changed-paths task failed: {e}"))?
}

/// The synchronous core of [`changed_paths_since`] — split out so tests can
/// inject a [`GitRunner`] mock.
fn changed_paths_since_impl(
    git: &dyn GitRunner,
    root: &Path,
    base: &str,
) -> Result<Vec<String>, String> {
    validate_base_ref(base)?;
    let changed = git_run(git, root, &["diff", "--name-status", base])?;
    // Raw (untrimmed): the two status columns of `--porcelain` are data.
    let status = git_run_raw(git, root, &["status", "--porcelain", "-uall"])?;
    let mut out: Vec<String> = Vec::new();
    for line in changed.lines() {
        let line = line.trim();
        if !line.is_empty() {
            // `--name-status` separates the status from the path with a tab
            // (and renames carry three fields) — keep the shape, lose the tab.
            out.push(line.replace('\t', " "));
        }
    }
    for line in status.lines() {
        // `?? path` is the untracked marker. Tracked modifications are
        // already covered by the diff above, so everything else is skipped.
        if let Some(path) = line.strip_prefix("?? ") {
            let path = path.trim();
            if !path.is_empty() {
                out.push(format!("?? {path}"));
            }
        }
    }
    Ok(out)
}

/// Create a git checkpoint before dispatching a backlog item — on the
/// per-directory wt/* working branch, never on main (backlog 04bd6977,
/// security review MEDIUM: the Run-All tick used to commit the dirty tree
/// straight to main when the session sat there, e.g. right after
/// merge_to_main landed a branch).
///
/// Forks/reuses the work branch first — the same auto-fork semantics
/// create_plan applies via [`ensure_work_branch`] — then commits the
/// working tree if it's dirty (so there's always a clean sha to roll back
/// to) and returns the current HEAD sha. A refused fork (dirty paths
/// outside `.coding/`) is an error: the caller must not fall through to
/// checkpointing real work onto main.
///
/// Runs the blocking git subprocesses on the blocking thread pool so the
/// async runtime (and thus every agent's event fan-in) is not stalled while
/// a commit runs. Owns its arguments (`root`, `item`) so the
/// `spawn_blocking` closure is `'static + Send`.
pub async fn checkpoint_on_work_branch(
    root: PathBuf,
    item: BacklogItem,
) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        let git = RealGit;
        checkpoint_on_work_branch_impl(&git, &root, &item)
    })
    .await
    .map_err(|e| format!("checkpoint task failed: {e}"))?
}

/// The branch-agnostic commit core of [`checkpoint_on_work_branch`] — split
/// out so tests can inject a [`GitRunner`] mock instead of spawning a real
/// `git` process.
fn checkpoint_impl(git: &dyn GitRunner, root: &Path, item: &BacklogItem) -> Result<String, String> {
    let dirty = !git_run(git, root, &["status", "--porcelain"])?.is_empty();
    if dirty {
        git_run(git, root, &["add", "-A"])?;
        git_run(
            git,
            root,
            &[
                "commit",
                "-m",
                &format!(
                    "backlog: pre-item checkpoint ({} {})",
                    item.id,
                    first_line(&item.text)
                ),
            ],
        )?;
    }
    git_run(git, root, &["rev-parse", "HEAD"])
}

/// The branch guard around [`checkpoint_impl`]: ensure the per-directory
/// wt/* work branch first (fork from main / reuse the current branch —
/// [`ensure_work_branch_impl`], the same semantics create_plan applies),
/// then checkpoint on it. A refused fork (dirty paths outside `.coding/`)
/// propagates as an error so the caller stops the run instead of
/// committing to main; `Ok(None)` (not a git repo) falls through to the
/// plain checkpoint, which fails cleanly on its first git call.
fn checkpoint_on_work_branch_impl(
    git: &dyn GitRunner,
    root: &Path,
    item: &BacklogItem,
) -> Result<String, String> {
    match ensure_work_branch_impl(git, root) {
        // Refused: NEVER checkpoint onto main — surface the refusal so the
        // dispatch stops the run (the item stays Pending, reason in its
        // note).
        Err(reason) => Err(format!("work-branch fork refused: {reason}")),
        // Forked, already on the work branch, or not a git repo at all:
        // checkpoint on whatever branch we're on now.
        Ok(_) => checkpoint_impl(git, root, item),
    }
}

/// Commit the result of a successfully resolved item.
///
/// Runs the blocking git subprocesses on the blocking thread pool (see
/// [`checkpoint_on_work_branch`]). Owns its arguments so the closure is `'static + Send`.
/// The synchronous core of [`commit_success`] — split out so tests can inject
/// a [`GitRunner`] mock.
fn commit_success_impl(git: &dyn GitRunner, root: &Path, item: &BacklogItem) -> Result<(), String> {
    let dirty = !git_run(git, root, &["status", "--porcelain"])?.is_empty();
    if dirty {
        git_run(git, root, &["add", "-A"])?;
        git_run(
            git,
            root,
            &[
                "commit",
                "-m",
                &format!("backlog: {} {}", item.id, first_line(&item.text)),
            ],
        )?;
    }
    Ok(())
}

pub async fn commit_success(root: PathBuf, item: BacklogItem) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let git = RealGit;
        commit_success_impl(&git, &root, &item)
    })
    .await
    .map_err(|e| format!("commit_success task failed: {e}"))?
}

/// Roll the working tree back to `sha` — the manual rollback primitive
/// (git reset --hard to the checkpoint sha; no automatic caller since
/// backlog 45dcf577 removed the rollback arm).
///
/// Runs the blocking git subprocess on the blocking thread pool (see
/// [`checkpoint_on_work_branch`]). Owns its arguments so the closure is `'static + Send`.
/// The synchronous core of [`rollback`] — split out so tests can inject a
/// [`GitRunner`] mock.
fn rollback_impl(git: &dyn GitRunner, root: &Path, sha: &str) -> Result<(), String> {
    git_run(git, root, &["reset", "--hard", sha]).map(|_| ())
}

pub async fn rollback(root: PathBuf, sha: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let git = RealGit;
        rollback_impl(&git, &root, &sha)
    })
    .await
    .map_err(|e| format!("rollback task failed: {e}"))?
}

/// Whether at least one commit exists on the current branch after `sha`
/// (exclusive) — the git-side landed-work evidence for the run-all
/// done-orphan guard (backlog 6c6966b9). The pre-item checkpoint sha
/// anchors the item's dispatch: during an item's flight its session is
/// the only committer (the next item's checkpoint runs only after this
/// item resolves), so any commit in `sha..HEAD` is that item's work — a
/// session that dies between its closing-sequence commit and `finish`
/// leaves exactly this signature. Deliberately NOT "commits referencing
/// the item id": agent commits don't reliably carry the id, and
/// [`commit_success`] is a no-op on a clean tree — precisely the
/// died-after-commit case.
///
/// Runs the blocking git subprocess on the blocking thread pool (see
/// [`checkpoint_on_work_branch`]). Owns its arguments so the closure is `'static + Send`.
/// The synchronous core of [`commits_after_checkpoint`] — split out so tests
/// can inject a [`GitRunner`] mock.
fn commits_after_checkpoint_impl(git: &dyn GitRunner, root: &Path, sha: &str) -> Result<bool, String> {
    let out = git_run(git, root, &["log", "--oneline", &format!("{sha}..HEAD")])?;
    Ok(!out.is_empty())
}

/// Whether at least one commit exists on the current branch after `sha`
/// (exclusive) — the git-side landed-work evidence for the run-all
/// done-orphan guard (backlog 6c6966b9). See
/// [`commits_after_checkpoint_impl`] for the semantics and rationale.
/// Runs the blocking git subprocess on the blocking thread pool (see
/// [`checkpoint_on_work_branch`]); owns its arguments so the closure is
/// `'static + Send`.
pub async fn commits_after_checkpoint(root: PathBuf, sha: String) -> Result<bool, String> {
    tokio::task::spawn_blocking(move || {
        let git = RealGit;
        commits_after_checkpoint_impl(&git, &root, &sha)
    })
    .await
    .map_err(|e| format!("commits_after_checkpoint task failed: {e}"))?
}

/// The first line of a prompt, truncated, for commit messages.
fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    const MAX: usize = 60;
    if line.chars().count() > MAX {
        let mut s: String = line.chars().take(MAX).collect();
        s.push('…');
        s
    } else {
        line.to_string()
    }
}

/// Reject a branch/ref argument that isn't a plausible name — specifically
/// a leading `-` (which git would parse as a flag, e.g. `--no-commit` or
/// `-b`) or embedded whitespace. The args are passed to git as discrete
/// `Command` argv (never a shell string), so there's no shell injection —
/// this guards against git-level *flag* injection so each operation stays
/// a narrow, predictable action.
pub fn valid_branch_name(name: &str) -> bool {
    !name.is_empty() && !name.starts_with('-') && !name.chars().any(|c| c.is_whitespace())
}

/// Derive the stable per-directory working-branch name: `wt/<sanitized
/// basename of the project root>`. The policy is one branch per agent
/// directory (worktree) — two worktrees of the same repo cannot check out the
/// same branch, so the name must be unique per directory, and stable so it is
/// reused across plans (never a fresh branch per plan). The basename is
/// lowercased and every run of non-alphanumeric characters collapses to a
/// single `-` (leading/trailing `-` trimmed); an empty result (a root with no
/// basename) falls back to `wt/work`. The `wt/` prefix keeps it out of
/// `main`/`master`'s namespace.
pub fn work_branch_name(root: &Path) -> String {
    let base = root.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let lower = base.to_ascii_lowercase();
    let mut collapsed = String::with_capacity(lower.len());
    let mut prev_dash = false;
    for c in lower.chars() {
        if c.is_ascii_alphanumeric() {
            collapsed.push(c);
            prev_dash = false;
        } else if !prev_dash {
            collapsed.push('-');
            prev_dash = true;
        }
    }
    let slug = collapsed.trim_matches('-');
    if slug.is_empty() {
        "wt/work".to_string()
    } else {
        format!("wt/{slug}")
    }
}

/// The outcome of a successful [`prepare_branch`] run — reported back
/// through the `create_plan` tool output so the agent knows what happened
/// to its working tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrepareBranchOutcome {
    /// A new branch was created (forked from `base`) and checked out.
    Created {
        /// The new branch name.
        branch: String,
        /// The branch it was forked from.
        base: String,
        /// How many dirty `.coding/` bookkeeping files were carried across.
        carried: usize,
    },
    /// An already-existing branch was checked out (the resume flow — e.g.
    /// retrying a backlog item whose branch survived a previous attempt).
    SwitchedExisting {
        /// The branch that was checked out.
        branch: String,
        /// How many dirty `.coding/` bookkeeping files were carried across.
        carried: usize,
    },
    /// The requested branch was already the current branch; nothing was done.
    AlreadyOnBranch {
        /// The branch name (equal to the current branch).
        branch: String,
    },
}

impl PrepareBranchOutcome {
    /// A human-readable one-liner for the `create_plan` tool output.
    pub fn describe(&self) -> String {
        match self {
            Self::Created {
                branch,
                base,
                carried,
            } => format!(
                "git: created branch '{branch}' forked from '{base}' and switched to it \
                 (carried {carried} dirty .coding file(s))"
            ),
            Self::SwitchedExisting { branch, carried } => format!(
                "git: switched to existing branch '{branch}' \
                 (carried {carried} dirty .coding file(s))"
            ),
            Self::AlreadyOnBranch { branch } => {
                format!("git: already on branch '{branch}' — nothing to do")
            }
        }
    }
}

/// A `.coding/` path that may be carried across a branch switch. Paths whose
/// content is `None` were deleted in the working tree (the deletion is
/// re-applied after the switch).
type Carry = Vec<(String, Option<Vec<u8>>)>;

/// Parse one `git status --porcelain` line into `(x, y, path)`.
///
/// Renames (`R  old -> new`) report the post-image path; quoted paths
/// (git quotes paths containing special characters) are unquoted. Returns
/// `None` for lines too short to be porcelain entries.
fn parse_porcelain_line(line: &str) -> Option<(char, char, String)> {
    let mut chars = line.chars();
    let x = chars.next()?;
    let y = chars.next()?;
    let rest: String = line.chars().skip(3).collect();
    if rest.is_empty() {
        return None;
    }
    // Renames report "old -> new"; the path git would overwrite is the new one.
    let path = match rest.find(" -> ") {
        Some(pos) => rest[pos + 4..].to_string(),
        None => rest,
    };
    Some((x, y, path.trim_matches('"').to_string()))
}

/// Whether a dirty path is `.coding/` bookkeeping that may be carried across
/// a branch switch. Memory stores are excluded categorically — they are never
/// git-touched, even if they show up dirty.
fn is_bookkeeping(path: &str) -> bool {
    let p = path.replace('\\', "/");
    p.starts_with(".coding/")
        && !p.contains("memory")
        && !p.ends_with(".db")
        && !p.ends_with(".db-wal")
        && !p.ends_with(".db-shm")
}

/// Restore carried paths after a (possibly failed) branch switch: re-write
/// the snapshotted bytes (creating parent dirs), or re-delete paths that were
/// deleted. Returns the paths that could NOT be restored (empty = fully
/// restored) so callers surface a silent-loss risk instead of discarding it —
/// a restore failure must never mask the original outcome, but it must not be
/// swallowed either.
fn restore_carries(root: &Path, carry: &Carry) -> Vec<String> {
    let mut failed = Vec::new();
    for (path, content) in carry {
        let full = root.join(path);
        match content {
            Some(bytes) => {
                if let Some(parent) = full.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if std::fs::write(&full, bytes).is_err() {
                    failed.push(path.clone());
                }
            }
            None => {
                // Re-apply the carried deletion. NotFound is success (the
                // switch already left the path absent).
                if let Err(e) = std::fs::remove_file(&full) {
                    if e.kind() != std::io::ErrorKind::NotFound {
                        failed.push(path.clone());
                    }
                }
            }
        }
    }
    failed
}

/// Append a carried-files warning to an error message when a restore did not
/// fully succeed (review finding 2: restore failures were silently discarded).
fn with_restore_warning(mut msg: String, failed: &[String]) -> String {
    if !failed.is_empty() {
        msg.push_str(&format!(
            "; warning: could not restore carried files: {}",
            failed.join(", ")
        ));
    }
    msg
}

/// The synchronous core of [`prepare_branch`] — see its docs for the
/// algorithm. Split out so the async wrapper only owns the thread-pool hop,
/// and so tests can inject a [`GitRunner`] mock.
fn prepare_branch_impl(
    git: &dyn GitRunner,
    root: &Path,
    branch: &str,
    base: &str,
) -> Result<PrepareBranchOutcome, String> {
    if !valid_branch_name(branch) {
        return Err(format!("invalid branch name '{branch}'"));
    }
    if !valid_branch_name(base) {
        return Err(format!("invalid base branch name '{base}'"));
    }

    // Classify the working tree. Untracked files never block `git checkout`
    // (unless the target branch tracks the same path — a UUID-named plan
    // file can't collide, and if anything else does the checkout below
    // simply fails and we restore), so they stay in place. Staged
    // adds/renames and unmerged entries are outside the simple carry model —
    // refuse rather than guess. Any dirty path outside `.coding/` is real
    // work: never move it across a branch switch.
    // Raw (untrimmed) output: porcelain's leading status-flag spaces are data.
    let status = git_run_raw(git, root, &["status", "--porcelain"])?;
    let mut carry: Carry = Vec::new();
    for line in status.lines() {
        let Some((x, y, path)) = parse_porcelain_line(line) else {
            continue;
        };
        if x == '?' && y == '?' {
            continue; // untracked — doesn't block a checkout
        }
        if matches!(x, 'A' | 'R' | 'U' | 'C') || matches!(y, 'A' | 'R' | 'U' | 'C') {
            return Err(format!(
                "staged add/rename or unmerged change present ('{path}') — \
                 commit or resolve it before requesting a branch switch"
            ));
        }
        if !is_bookkeeping(&path) {
            return Err(format!(
                "working tree has changes outside .coding/ bookkeeping ('{path}') — \
                 not switching branches automatically; handle git yourself"
            ));
        }
        let content = match std::fs::read(root.join(&path)) {
            Ok(bytes) => Some(bytes),
            // A tracked path deleted in the worktree: the deletion itself is
            // carried (re-applied after the switch).
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            // Present but unreadable (locked by another process, permissions):
            // carrying would treat it as deleted and destroy the dirty bytes —
            // refuse instead (review finding 2).
            Err(e) => {
                return Err(format!(
                    "cannot read dirty bookkeeping file '{path}' to carry it across \
                     the switch: {e}"
                ));
            }
        };
        carry.push((path, content));
    }

    // Already on the requested branch? Nothing to switch (dirty state stays).
    let current = git_run(git, root, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    if current == branch {
        return Ok(PrepareBranchOutcome::AlreadyOnBranch {
            branch: branch.to_string(),
        });
    }

    // Clean the carried paths (index + worktree → HEAD) so they cannot block
    // the switch; their dirty bytes are re-applied after it. From here on,
    // EVERY failure path must restore the snapshots.
    if !carry.is_empty() {
        let mut args: Vec<&str> = vec!["restore", "--source=HEAD", "--staged", "--worktree", "--"];
        args.extend(carry.iter().map(|(p, _)| p.as_str()));
        if let Err(e) = git_run(git, root, &args) {
            let failed = restore_carries(root, &carry);
            return Err(with_restore_warning(
                format!("failed to clean the carried .coding paths: {e}"),
                &failed,
            ));
        }
    }

    if current != base {
        if let Err(e) = git_run(git, root, &["checkout", base]) {
            // A missing base is always an error now. Under the previous
            // wt/* → develop → main topology this arm auto-created `develop`
            // from main; with wt/* forking straight from main the base always
            // exists, so a failed checkout means a typo'd or deleted ref —
            // which must never silently fork from the wrong place. Restore
            // the carried state and report.
            let failed = restore_carries(root, &carry);
            return Err(with_restore_warning(
                format!("git checkout {base} failed: {e}"),
                &failed,
            ));
        }
    }

    // Fork (or switch to) the requested branch.
    let ref_name = format!("refs/heads/{branch}");
    let branch_exists = git_run(
        git,
        root,
        &["rev-parse", "--verify", "--quiet", ref_name.as_str()],
    )
    .is_ok();
    let switched = if branch_exists {
        git_run(git, root, &["checkout", branch]).map(|_| PrepareBranchOutcome::SwitchedExisting {
            branch: branch.to_string(),
            carried: carry.len(),
        })
    } else {
        git_run(git, root, &["checkout", "-b", branch]).map(|_| PrepareBranchOutcome::Created {
            branch: branch.to_string(),
            base: base.to_string(),
            carried: carry.len(),
        })
    };
    match switched {
        Ok(outcome) => {
            let failed = restore_carries(root, &carry);
            if failed.is_empty() {
                Ok(outcome)
            } else {
                // The switch itself succeeded but a carried file could not be
                // re-written — report it rather than claiming a clean carry
                // (review finding 2).
                Err(format!(
                    "branch '{branch}' is checked out but re-writing carried .coding \
                     files failed: {} — restore them manually",
                    failed.join(", ")
                ))
            }
        }
        Err(e) => {
            // Review finding 1: the base checkout succeeded above, so HEAD is
            // on `base` here — return to the ORIGINAL branch before re-dirtying
            // the carried paths (the tree is clean for them at this point, so
            // this checkout is safe). Without it, an Err left HEAD on `base`
            // (main) and the item's work would commit to main.
            let back = git_run(git, root, &["checkout", &current]).err();
            let failed = restore_carries(root, &carry);
            let mut msg = format!("git checkout {branch} failed: {e}");
            if let Some(b) = back {
                msg.push_str(&format!(
                    "; ALSO failed to return to '{current}' — HEAD is on '{base}', \
                     switch back manually ({b})"
                ));
            }
            Err(with_restore_warning(msg, &failed))
        }
    }
}

/// Fork a feature branch for a new plan — the automated version of the
/// 5-6 command dance an agent previously ran by hand at the start of every
/// backlog item.
///
/// At a backlog item's start the working tree is clean except for harness
/// bookkeeping (the `.coding/backlog.jsonl` InFlight flip) and — once the
/// plan is created — `.coding/plans/<id>.md` + `stack.json`, which differ
/// between the previous item's feature branch and `base`. `git checkout
/// <base>` therefore fails ("local changes would be overwritten"), which is
/// exactly the friction this removes. Called BEFORE the plan file is written,
/// only the bookkeeping is dirty, and the dance is:
///
/// 1. `git status --porcelain` — classify every dirty path. Untracked files
///    stay in place (they cannot block a checkout); dirty `.coding/`
///    bookkeeping is snapshotted (bytes in memory, never a git stash, so a
///    failure leaves no stash entries and needs no merge to undo); staged
///    adds/renames or ANY dirty path outside `.coding/` aborts with a reason
///    (real work must never silently ride a branch switch).
/// 2. `git restore --source=HEAD --staged --worktree` the carried paths.
/// 3. `git checkout <base>` (skipped when already there), then
///    `git checkout -b <branch>` — or plain `git checkout <branch>` when it
///    already exists (the resume flow).
/// 4. Re-write the snapshots, so e.g. the backlog flip is preserved and
///    rides this item's commit.
///
/// Any failure restores the snapshots — and, once the base checkout has run,
/// returns HEAD to the original branch — then returns `Err`; the caller
/// (`create_plan`) still creates the plan, just on the current branch, and
/// reports the reason.
///
/// Runs the git subprocesses on the blocking thread pool (see
/// [`checkpoint_on_work_branch`]). Owns its arguments so the closure is `'static + Send`.
pub async fn prepare_branch_with(
    git: Arc<dyn GitRunner + Send + Sync>,
    root: PathBuf,
    branch: String,
    base: String,
) -> Result<PrepareBranchOutcome, String> {
    tokio::task::spawn_blocking(move || prepare_branch_impl(&*git, &root, &branch, &base))
        .await
        .map_err(|e| format!("prepare_branch task failed: {e}"))?
}

pub async fn prepare_branch(
    root: PathBuf,
    branch: String,
    base: String,
) -> Result<PrepareBranchOutcome, String> {
    prepare_branch_with(Arc::new(RealGit), root, branch, base).await
}

/// The synchronous core of [`ensure_work_branch`] — split out so the async
/// wrapper only owns the thread-pool hop, and so tests can inject a
/// [`GitRunner`] mock.
fn ensure_work_branch_impl(
    git: &dyn GitRunner,
    root: &Path,
) -> Result<Option<PrepareBranchOutcome>, String> {
    // Probe the current branch. A genuine git failure (not a repo, etc.)
    // means we cannot safely auto-fork — return None so `create_plan`
    // proceeds on the current branch with no note (mirrors the
    // no-project-root skip: planning is never blocked on git). NOTE: a
    // detached HEAD is NOT a failure here — `rev-parse --abbrev-ref HEAD`
    // succeeds and prints the literal "HEAD" — so it is handled explicitly
    // below (fork from main, never reuse a detached HEAD).
    let current = match git_run(git, root, &["rev-parse", "--abbrev-ref", "HEAD"]) {
        Ok(b) => b,
        Err(_) => return Ok(None),
    };
    // Already on a non-main, non-detached branch: reuse it (one branch per
    // directory). No mutation — the agent's in-flight work stays put, and a
    // sub-plan's parent branch is preserved. A detached HEAD ("HEAD") is
    // NOT reused — work there is easily lost on the next checkout — so it
    // falls through to the fork-from-main path below.
    if current != "main" && current != "master" && current != "HEAD" {
        return Ok(Some(PrepareBranchOutcome::AlreadyOnBranch {
            branch: current,
        }));
    }
    // On main: fork (or resume) the stable per-directory working branch from
    // main. prepare_branch_impl carries dirty `.coding/` bookkeeping and
    // refuses dirty paths outside `.coding/` (returning Err, which the caller
    // surfaces as a skip note — the plan is still created).
    let branch = work_branch_name(root);
    prepare_branch_impl(git, root, &branch, "main").map(Some)
}

/// Ensure the agent directory is on its single per-directory working branch:
/// if HEAD is on `main`/`master`, fork (or resume) the stable
/// [`work_branch_name`] branch from `main`; if already on a non-main branch,
/// reuse it (no mutation). Returns `Ok(None)` when the branch cannot be
/// probed (not a git repo) so the caller skips silently. This is the
/// auto-fork path `create_plan` uses when no explicit `branch` is given —
/// one branch per agent directory, so work never silently stays on `main`.
///
/// Runs the git subprocesses on the blocking thread pool (see
/// [`prepare_branch`]). Owns its argument so the closure is `'static + Send`.
pub async fn ensure_work_branch_with(
    git: Arc<dyn GitRunner + Send + Sync>,
    root: PathBuf,
) -> Result<Option<PrepareBranchOutcome>, String> {
    tokio::task::spawn_blocking(move || ensure_work_branch_impl(&*git, &root))
        .await
        .map_err(|e| format!("ensure_work_branch task failed: {e}"))?
}

pub async fn ensure_work_branch(root: PathBuf) -> Result<Option<PrepareBranchOutcome>, String> {
    ensure_work_branch_with(Arc::new(RealGit), root).await
}

/// Probe the checked-out branch for a finish-digest branch hint: `Some("branch
/// <name> @ <short-sha> (unmerged — exists only on this branch)")` when HEAD
/// is a non-main branch, `None` on main/master, a detached HEAD
/// (`branch --show-current` prints nothing), an unborn HEAD, or any git
/// failure. Best-effort by design — a finish digest must never fail over the
/// hint. Runs on the blocking thread pool like every git op here; owns its
/// argument so the closure is `'static + Send`.
/// The synchronous core of [`branch_hint`] — split out so tests can inject a
/// [`GitRunner`] mock.
fn branch_hint_impl(git: &dyn GitRunner, root: &Path) -> Option<String> {
    let branch = git_run(git, root, &["branch", "--show-current"]).ok()?;
    if branch.is_empty() || branch == "main" || branch == "master" {
        return None;
    }
    let sha = git_run(git, root, &["rev-parse", "--short", "HEAD"]).ok()?;
    if sha.is_empty() {
        return None;
    }
    Some(format!(
        "branch {branch} @ {sha} (unmerged — exists only on this branch)"
    ))
}

pub async fn branch_hint(root: PathBuf) -> Option<String> {
    tokio::task::spawn_blocking(move || {
        let git = RealGit;
        branch_hint_impl(&git, &root)
    })
    .await
    .ok()
    .flatten()
}

/// In-memory mock git for testing the git-orchestration logic without
/// spawning a real `git` process. Extracted into its own module so both
/// `git_ops::tests` and `tool::workflow::plan::tests` can import it.
#[cfg(test)]
pub(crate) mod mock_git {
    use super::GitRunner;
    use std::path::Path;
    use std::sync::Mutex;

    /// An in-memory mini-git for testing the git-orchestration logic without
    /// spawning a real `git` process. Tracks just enough state (HEAD sha,
    /// current branch, dirty/clean, branches, commit messages) to respond to
    /// the specific commands our code uses. File I/O (the carry mechanism in
    /// `prepare_branch_impl`) stays real — tests create temp dirs with real
    /// files, and the mock only simulates the git commands.
    pub(crate) struct MockGit {
        state: Mutex<MockState>,
    }

    struct MockState {
        /// The current HEAD commit sha.
        head_sha: String,
        /// The currently checked-out branch name.
        current_branch: String,
        /// Whether the working tree is dirty (for `status --porcelain`).
        dirty: bool,
        /// Pre-programmed `status --porcelain` output (for `prepare_branch`
        /// tests that need specific dirty paths). When empty, falls back to
        /// the `dirty` flag.
        porcelain_status: String,
        /// Known branch names (for `rev-parse --verify`).
        branches: Vec<String>,
        /// Commit messages, in order (for verification).
        commits: Vec<String>,
        /// Every command received (for debugging).
        calls: Vec<Vec<String>>,
        /// Simulate "not a git repo" — `rev-parse --abbrev-ref HEAD` fails.
        not_a_repo: bool,
        /// Pre-programmed `diff --name-status <base>` output (tab-separated
        /// lines) for `changed_paths_since` tests.
        diff_name_status: String,
        /// Make `diff --name-status` fail with this message, so error
        /// propagation through `changed_paths_since` is testable.
        diff_error: Option<String>,
    }

    impl MockGit {
        /// A clean repo on `main` with one initial commit.
        pub(crate) fn new() -> Self {
            Self {
                state: Mutex::new(MockState {
                    head_sha: "abc123".to_string(),
                    current_branch: "main".to_string(),
                    dirty: false,
                    porcelain_status: String::new(),
                    branches: vec!["main".to_string()],
                    commits: vec!["initial".to_string()],
                    calls: Vec::new(),
                    not_a_repo: false,
                    diff_name_status: String::new(),
                    diff_error: None,
                }),
            }
        }

        /// Set the working tree dirty (checkpoint/commit_success will commit).
        pub(crate) fn set_dirty(&self, dirty: bool) {
            let mut s = self.state.lock().unwrap();
            s.dirty = dirty;
        }

        /// Set the current branch (for `rev-parse --abbrev-ref HEAD`).
        pub(crate) fn set_branch(&self, branch: &str) {
            let mut s = self.state.lock().unwrap();
            s.current_branch = branch.to_string();
        }

        /// Set pre-programmed `status --porcelain` output (for prepare_branch
        /// tests that need specific dirty paths like ` M .coding/backlog.jsonl`).
        pub(crate) fn set_porcelain(&self, status: &str) {
            let mut s = self.state.lock().unwrap();
            s.porcelain_status = status.to_string();
        }

        /// Mark a branch as existing (for `rev-parse --verify --quiet`).
        pub(crate) fn add_branch(&self, branch: &str) {
            let mut s = self.state.lock().unwrap();
            if !s.branches.contains(&branch.to_string()) {
                s.branches.push(branch.to_string());
            }
        }

        /// Simulate "not a git repo" — `rev-parse --abbrev-ref HEAD` will fail.
        pub(crate) fn set_not_a_repo(&self) {
            let mut s = self.state.lock().unwrap();
            s.not_a_repo = true;
        }

        /// Set the `diff --name-status <base>` output (backlog 85313a7e's
        /// reviewer scope). Tab-separated lines, as git emits them.
        pub(crate) fn set_diff_name_status(&self, out: &str) {
            let mut s = self.state.lock().unwrap();
            s.diff_name_status = out.to_string();
        }

        /// Make `diff --name-status` fail with `msg`.
        pub(crate) fn set_diff_error(&self, msg: &str) {
            let mut s = self.state.lock().unwrap();
            s.diff_error = Some(msg.to_string());
        }

        /// The current HEAD sha.
        pub(crate) fn head_sha(&self) -> String {
            self.state.lock().unwrap().head_sha.clone()
        }

        /// The current branch name.
        pub(crate) fn current_branch(&self) -> String {
            self.state.lock().unwrap().current_branch.clone()
        }

        /// The recorded commit messages (for assertions).
        pub(crate) fn commits(&self) -> Vec<String> {
            self.state.lock().unwrap().commits.clone()
        }
    }

    impl GitRunner for MockGit {
        fn run(&self, _root: &Path, args: &[&str]) -> Result<String, String> {
            let mut s = self.state.lock().unwrap();
            s.calls.push(args.iter().map(|a| a.to_string()).collect());
            let args: Vec<&str> = args.iter().copied().collect();

            // status --porcelain
            if args == ["status", "--porcelain"] {
                if !s.porcelain_status.is_empty() {
                    return Ok(s.porcelain_status.clone());
                }
                return Ok(if s.dirty {
                    " M file.txt\n".to_string()
                } else {
                    String::new()
                });
            }

            // status --porcelain -uall (changed_paths_since_impl: untracked)
            if args == ["status", "--porcelain", "-uall"] {
                if !s.porcelain_status.is_empty() {
                    return Ok(s.porcelain_status.clone());
                }
                return Ok(if s.dirty {
                    " M file.txt\n".to_string()
                } else {
                    String::new()
                });
            }

            // diff --name-status <base> (changed_paths_since_impl: tracked)
            if args.len() == 3 && args[0] == "diff" && args[1] == "--name-status" {
                if let Some(err) = s.diff_error.clone() {
                    return Err(err);
                }
                return Ok(s.diff_name_status.clone());
            }

            // add -A
            if args == ["add", "-A"] {
                return Ok(String::new());
            }

            // commit -m <msg>
            if args.len() >= 3 && args[0] == "commit" && args[1] == "-m" {
                let msg = args[2..].join(" ");
                s.commits.push(msg);
                s.head_sha = format!("sha{}", s.commits.len());
                s.dirty = false;
                s.porcelain_status.clear();
                return Ok(String::new());
            }

            // rev-parse HEAD
            if args == ["rev-parse", "HEAD"] {
                return Ok(s.head_sha.clone());
            }

            // rev-parse --short HEAD
            if args == ["rev-parse", "--short", "HEAD"] {
                return Ok(s.head_sha.chars().take(7).collect());
            }

            // rev-parse --abbrev-ref HEAD
            if args == ["rev-parse", "--abbrev-ref", "HEAD"] {
                if s.not_a_repo {
                    return Err("fatal: not a git repository".to_string());
                }
                return Ok(s.current_branch.clone());
            }

            // rev-parse --verify --quiet <ref>
            if args.len() >= 3
                && args[0] == "rev-parse"
                && args[1] == "--verify"
                && args[2] == "--quiet"
            {
                let ref_name = args[3];
                // refs/heads/<branch> → check if branch exists
                let branch = ref_name.strip_prefix("refs/heads/").unwrap_or(ref_name);
                if s.branches.contains(&branch.to_string()) {
                    return Ok(s.head_sha.clone());
                }
                return Err(format!("rev-parse --verify: unknown ref '{ref_name}'"));
            }

            // reset --hard <sha>
            if args.len() >= 3 && args[0] == "reset" && args[1] == "--hard" {
                s.head_sha = args[2].to_string();
                s.dirty = false;
                s.porcelain_status.clear();
                return Ok(String::new());
            }

            // restore --source=HEAD --staged --worktree -- <paths...>
            if args.len() >= 2 && args[0] == "restore" {
                // No-op: the carry mechanism handles file I/O; the mock just
                // acknowledges the restore succeeded.
                return Ok(String::new());
            }

            // checkout <branch>
            if args.len() == 2 && args[0] == "checkout" && args[1] != "-b" {
                let branch = args[1];
                if !s.branches.contains(&branch.to_string()) {
                    return Err(format!("checkout: branch '{branch}' does not exist"));
                }
                s.current_branch = branch.to_string();
                return Ok(String::new());
            }

            // checkout -b <branch>
            if args.len() == 3 && args[0] == "checkout" && args[1] == "-b" {
                let branch = args[2];
                // Git rejects ref names containing ".." — simulate that so
                // the "failure after base checkout" test can exercise the
                // error-restore path.
                if branch.contains("..") {
                    return Err(format!("fatal: '{branch}' is not a valid branch name"));
                }
                s.branches.push(branch.to_string());
                s.current_branch = branch.to_string();
                return Ok(String::new());
            }

            // branch --show-current
            if args == ["branch", "--show-current"] {
                return Ok(s.current_branch.clone());
            }

            // stash / stash pop
            if args == ["stash"] || args == ["stash", "push"] {
                s.dirty = false;
                return Ok(String::new());
            }
            if args == ["stash", "pop"] {
                return Ok(String::new());
            }

            // diff --cached --quiet (exit non-zero = something staged)
            if args == ["diff", "--cached", "--quiet"] {
                if s.dirty {
                    return Err("diff --cached --quiet: changes staged".to_string());
                }
                return Ok(String::new());
            }

            // log --oneline <sha>..HEAD (commits_after_checkpoint_impl)
            if args.len() == 3 && args[0] == "log" && args[1] == "--oneline" {
                let range = args[2];
                let Some(base) = range.strip_suffix("..HEAD") else {
                    return Err(format!("MockGit: unhandled log range '{range}'"));
                };
                // Map the mock's shas to commit indices: "abc123" is the
                // initial commit (commits[0]); a commit made during the
                // test lands at commits[len] with sha "sha{len+1}" (head
                // is "sha{len}" after the push) — so "shaN" is
                // commits[N-1], and the commits after it are
                // commits[N..]. "sha1" never exists (the initial commit
                // is "abc123").
                let start = if base == "abc123" {
                    Some(1)
                } else {
                    base.strip_prefix("sha")
                        .and_then(|n| n.parse::<usize>().ok())
                        .filter(|n| *n >= 2 && *n <= s.commits.len())
                };
                return match start {
                    Some(start) => {
                        let lines: Vec<String> = s.commits[start..]
                            .iter()
                            .enumerate()
                            .map(|(i, msg)| format!("sha{} {msg}", start + i + 1))
                            .collect();
                        Ok(lines.join("\n"))
                    }
                    None => Err(format!("unknown revision {base}")),
                };
            }

            Err(format!("MockGit: unhandled command {:?}", args))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::mock_git::MockGit;
    use super::*;
    use crate::backlog::BacklogStatus;
    use std::fs;

    /// A temp dir + MockGit combo for testing the git-orchestration logic
    /// without spawning a real `git` process. The temp dir holds real files
    /// (the carry mechanism in `prepare_branch_impl` does real file I/O); the
    /// MockGit simulates the git commands.
    struct MockRepo {
        dir: tempfile::TempDir,
        git: MockGit,
    }

    impl MockRepo {
        fn root(&self) -> &Path {
            self.dir.path()
        }

        fn write(&self, rel: &str, content: &str) {
            let path = self.dir.path().join(rel);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(path, content).unwrap();
        }

        fn read(&self, rel: &str) -> String {
            fs::read_to_string(self.dir.path().join(rel)).unwrap()
        }

        fn path_exists(&self, rel: &str) -> bool {
            self.dir.path().join(rel).exists()
        }

        fn remove_file(&self, rel: &str) {
            fs::remove_file(self.dir.path().join(rel)).unwrap();
        }
    }

    /// A diverged repo state: on `feat/prev` with a dirty `.coding/backlog.jsonl`
    /// (the InFlight flip), an untracked `stack.json`, and `file.txt` tracked on
    /// main. Mirrors the old `diverged_repo()` helper but without spawning git.
    fn mock_diverged() -> MockRepo {
        let dir = tempfile::tempdir().unwrap();
        let git = MockGit::new();
        git.set_branch("feat/prev");
        git.add_branch("feat/prev");
        git.set_porcelain(" M .coding/backlog.jsonl\n");
        let repo = MockRepo { dir, git };
        repo.write("file.txt", "original\n");
        repo.write(
            ".coding/backlog.jsonl",
            "{\"items\":[{\"status\":\"in_flight\"}]}\n",
        );
        repo.write(".coding/plans/stack.json", "{\"stack\":[\"plan-prev\"]}\n");
        repo
    }

    fn item(id: &str, text: &str) -> BacklogItem {
        BacklogItem {
            id: id.to_string(),
            text: text.to_string(),
            images: vec![],
            status: BacklogStatus::Pending,
            created_at: 0,
            note: None,
            deferred: false,
            plan_id: None,
            plan_title: None,
            deleted_at: None,
        }
    }

    #[test]
    fn first_line_truncates_and_strips() {
        assert_eq!(first_line("hello world"), "hello world");
        assert_eq!(first_line("  padded  \nsecond"), "padded");
        assert_eq!(first_line(""), "");
        let long = "a".repeat(100);
        let got = first_line(&long);
        assert!(got.chars().count() <= 61, "truncated to 60 + ellipsis");
        assert!(got.ends_with('…'));
    }

    // ── checkpoint / commit_success / rollback ─────────────────────────────
    //
    // These test our orchestration logic (which git commands we call, in what
    // order, with what arguments) — NOT git's own behavior. The MockGit
    // records commits and tracks state; no real git process is spawned.

    /// Happy path: checkpoint captures a clean HEAD sha, then commit_success
    /// commits the agent's edits so the working tree ends clean.
    #[test]
    fn checkpoint_then_commit_success_commits_edits() {
        let git = MockGit::new();
        let root = Path::new("/tmp/mock");
        let item = item("item-7", "add a feature");

        // Checkpoint on a clean tree just returns the current HEAD sha.
        let sha = checkpoint_impl(&git, root, &item).unwrap();
        assert_eq!(
            sha,
            git.head_sha(),
            "checkpoint returns the current HEAD sha"
        );
        assert!(
            git.commits().len() == 1,
            "checkpoint on clean tree must not commit"
        );

        // Simulate the agent's edits (dirty tree), then commit them as a success.
        git.set_dirty(true);
        commit_success_impl(&git, root, &item).unwrap();

        let commits = git.commits();
        assert_eq!(commits.len(), 2, "commit_success added one commit");
        assert!(
            commits[1].contains("backlog: item-7 add a feature"),
            "commit message: {}",
            commits[1]
        );
    }

    /// Backlog 6c6966b9 (run-all done-orphan guard): the git-side
    /// landed-work evidence. A commit after the pre-item checkpoint sha
    /// means the dead session's work landed (true); no commits after it
    /// means it died before committing (false — requeue); an unknown sha
    /// is an error (cannot verify — the caller requeues).
    #[test]
    fn commits_after_checkpoint_impl_detects_landed_work() {
        let git = MockGit::new();
        let root = Path::new("/tmp/mock");
        let item = item("item-9", "landed work");

        // Checkpoint on a clean tree anchors at the current HEAD.
        let sha = checkpoint_impl(&git, root, &item).unwrap();
        assert_eq!(sha, "abc123");

        // The session died BEFORE committing anything: no commits after
        // the checkpoint → not landed.
        assert_eq!(
            commits_after_checkpoint_impl(&git, root, &sha).unwrap(),
            false,
            "no commits after the checkpoint — work did not land"
        );

        // The session committed its work, then died before finish:
        // commits after the checkpoint → landed.
        git.set_dirty(true);
        commit_success_impl(&git, root, &item).unwrap();
        assert_eq!(
            commits_after_checkpoint_impl(&git, root, &sha).unwrap(),
            true,
            "a commit after the checkpoint — the work landed"
        );

        // The checkpoint sha itself is excluded (it IS the anchor): the
        // range from the new HEAD sees nothing after it.
        let head = git.head_sha();
        assert_eq!(
            commits_after_checkpoint_impl(&git, root, &head).unwrap(),
            false,
            "the anchor commit itself must not count as landed work"
        );

        // An unknown sha cannot be verified — an error, not a false.
        assert!(
            commits_after_checkpoint_impl(&git, root, "deadbee").is_err(),
            "unknown revision must error (cannot verify)"
        );
    }

    /// Dirty tree at checkpoint: the pre-existing changes are committed first
    /// so there's a clean sha, then the item's own edits commit on success.
    #[test]
    fn checkpoint_commits_dirty_tree_before_returning_sha() {
        let git = MockGit::new();
        let root = Path::new("/tmp/mock");
        let item = item("item-3", "fix a bug\nwith details");

        // Leave the tree dirty before checkpointing.
        git.set_dirty(true);
        let sha = checkpoint_impl(&git, root, &item).unwrap();
        let commits = git.commits();
        assert_eq!(commits.len(), 2, "checkpoint must commit a dirty tree");
        assert!(
            commits[1].contains("backlog: pre-item checkpoint (item-3 fix a bug)"),
            "checkpoint commit message: {}",
            commits[1]
        );
        assert_ne!(sha, "abc123", "sha changed after the checkpoint commit");
    }

    /// Failure path: after a checkpoint, the agent's edits are rolled back so
    /// the working tree returns to the checkpoint state.
    #[test]
    fn checkpoint_then_rollback_restores_pre_edit_state() {
        let git = MockGit::new();
        let root = Path::new("/tmp/mock");
        let item = item("item-11", "risky change");

        let sha = checkpoint_impl(&git, root, &item).unwrap();

        // Simulate the agent making edits (dirty tree).
        git.set_dirty(true);

        rollback_impl(&git, root, &sha).unwrap();

        // The mock's head_sha was reset to the checkpoint sha.
        assert_eq!(
            git.head_sha(),
            sha,
            "rollback reset HEAD to the checkpoint sha"
        );
        assert!(git.commits().len() <= 1, "no new commit after rollback");
    }

    /// commit_success on an already-clean tree is a no-op (no empty commit).
    #[test]
    fn commit_success_on_clean_tree_is_noop() {
        let git = MockGit::new();
        let root = Path::new("/tmp/mock");
        let item = item("item-1", "nothing to do");
        let before = git.head_sha();
        let commits_before = git.commits().len();
        commit_success_impl(&git, root, &item).unwrap();
        assert_eq!(git.head_sha(), before, "no new commit");
        assert_eq!(
            git.commits().len(),
            commits_before,
            "no new commit recorded"
        );
    }

    // ── prepare_branch: the create_plan branch-fork dance ───────────────────

    /// Happy path: forked from main (not stacked on feat/prev), the dirty
    /// backlog flip carried across, and main's stack.json restored.
    #[test]
    fn prepare_branch_creates_branch_from_main_and_carries_bookkeeping() {
        let repo = mock_diverged();

        let out = prepare_branch_impl(&repo.git, repo.root(), "fix/new", "main").unwrap();

        assert_eq!(
            out,
            PrepareBranchOutcome::Created {
                branch: "fix/new".into(),
                base: "main".into(),
                carried: 1
            },
            "one dirty .coding file (backlog.json) is carried"
        );
        assert_eq!(repo.git.current_branch(), "fix/new");
        // The dirty backlog flip survived the switch byte-for-byte (our carry
        // mechanism re-wrote it after the mock checkout).
        assert_eq!(
            repo.read(".coding/backlog.jsonl"),
            "{\"items\":[{\"status\":\"in_flight\"}]}\n"
        );
        // stack.json is per-worktree LOCAL state (untracked): it was never
        // carried, so its local content persists untouched across the switch.
        assert_eq!(
            repo.read(".coding/plans/stack.json"),
            "{\"stack\":[\"plan-prev\"]}\n",
            "stack.json stays local — it is not part of the branch carry"
        );
    }

    /// Real source changes must never ride an automatic branch switch: dirty
    /// TRACKED paths outside `.coding/` abort with a reason and leave
    /// everything untouched.
    #[test]
    fn prepare_branch_skips_when_source_outside_coding_is_dirty() {
        let repo = mock_diverged();
        // Work in progress on a tracked source file — the realistic mid-work state.
        repo.write("file.txt", "dirty work in progress\n");
        repo.git
            .set_porcelain(" M file.txt\n M .coding/backlog.jsonl\n");

        let err = prepare_branch_impl(&repo.git, repo.root(), "fix/new", "main").unwrap_err();

        assert!(
            err.contains("outside .coding/"),
            "reason must name the blocking path class: {err}"
        );
        assert!(
            err.contains("file.txt"),
            "reason must name the blocking path: {err}"
        );
        assert_eq!(repo.git.current_branch(), "feat/prev", "branch unchanged");
        assert_eq!(
            repo.read("file.txt"),
            "dirty work in progress\n",
            "dirty file untouched"
        );
        assert_eq!(
            repo.read(".coding/backlog.jsonl"),
            "{\"items\":[{\"status\":\"in_flight\"}]}\n",
            "bookkeeping untouched too"
        );
    }

    /// Flag injection and empty names are rejected before any git mutation.
    #[test]
    fn prepare_branch_rejects_invalid_names() {
        let repo = mock_diverged();
        for bad in ["", "-x", "has space"] {
            assert!(
                prepare_branch_impl(&repo.git, repo.root(), bad, "main").is_err(),
                "branch name {bad:?} must be rejected"
            );
        }
        assert!(
            prepare_branch_impl(&repo.git, repo.root(), "fix/new", "-m").is_err(),
            "invalid base name must be rejected"
        );
        assert_eq!(
            repo.git.current_branch(),
            "feat/prev",
            "no branch was created"
        );
    }

    /// An existing branch is switched to (the resume flow), with the same
    /// bookkeeping carry.
    #[test]
    fn prepare_branch_switches_to_existing_branch() {
        let repo = mock_diverged();
        repo.git.set_branch("main");

        let out = prepare_branch_impl(&repo.git, repo.root(), "feat/prev", "main").unwrap();

        assert_eq!(
            out,
            PrepareBranchOutcome::SwitchedExisting {
                branch: "feat/prev".into(),
                carried: 1
            }
        );
        assert_eq!(repo.git.current_branch(), "feat/prev");
        assert_eq!(
            repo.read(".coding/backlog.jsonl"),
            "{\"items\":[{\"status\":\"in_flight\"}]}\n",
            "the dirty flip was carried onto the existing branch"
        );
    }

    /// Working-branch topology (wt/* → main): a missing base is a HARD
    /// ERROR, never an auto-create.
    #[test]
    fn prepare_branch_errors_on_a_missing_base() {
        let repo = mock_diverged();
        repo.git.set_branch("main");

        let err = prepare_branch_impl(&repo.git, repo.root(), "wt/alice", "develop").unwrap_err();
        assert!(
            err.contains("develop"),
            "the error names the missing base rather than creating it: {err}"
        );
        assert_eq!(
            repo.git.current_branch(),
            "main",
            "a missing base leaves the checkout untouched"
        );
    }

    /// wt/* forks from `main` — the default base under the collapsed topology.
    #[test]
    fn prepare_branch_forks_wt_from_main() {
        let repo = mock_diverged();
        repo.git.set_branch("main");

        let out = prepare_branch_impl(&repo.git, repo.root(), "wt/alice", "main").unwrap();

        assert_eq!(
            out,
            PrepareBranchOutcome::Created {
                branch: "wt/alice".into(),
                base: "main".into(),
                carried: 1
            },
            "wt/* forked straight from main"
        );
        assert_eq!(repo.git.current_branch(), "wt/alice");
    }

    /// A `wt/*` branch is REUSED when it already exists (the one-branch-per-
    /// directory resume flow — a second create_plan with the same branch must
    /// not fork a duplicate).
    #[test]
    fn prepare_branch_reuses_existing_wt_branch() {
        let repo = mock_diverged();
        repo.git.set_branch("main");

        // Create the wt branch once (as the first plan did).
        prepare_branch_impl(&repo.git, repo.root(), "wt/alice", "main").unwrap();
        // Dirty backlog on the wt branch (a second plan's InFlight flip).
        repo.write(
            ".coding/backlog.jsonl",
            "{\"items\":[{\"status\":\"in_flight\"}]}\n",
        );
        repo.git.set_branch("main");

        // Second plan on the SAME working branch: switch, don't fork.
        let out = prepare_branch_impl(&repo.git, repo.root(), "wt/alice", "main").unwrap();
        assert_eq!(
            out,
            PrepareBranchOutcome::SwitchedExisting {
                branch: "wt/alice".into(),
                carried: 1
            },
            "the existing wt branch is reused, not re-forked"
        );
        assert_eq!(repo.git.current_branch(), "wt/alice");
        // The dirty flip rode onto the reused branch.
        assert_eq!(
            repo.read(".coding/backlog.jsonl"),
            "{\"items\":[{\"status\":\"in_flight\"}]}\n"
        );
    }

    /// A missing base branch errors AND restores the carried dirty state —
    /// the flip must not be silently lost.
    #[test]
    fn prepare_branch_missing_base_errors_and_restores_carry() {
        let repo = mock_diverged();

        let err = prepare_branch_impl(&repo.git, repo.root(), "fix/new", "nope").unwrap_err();

        assert!(err.contains("nope"), "reason names the missing base: {err}");
        assert_eq!(repo.git.current_branch(), "feat/prev", "branch unchanged");
        assert_eq!(
            repo.read(".coding/backlog.jsonl"),
            "{\"items\":[{\"status\":\"in_flight\"}]}\n",
            "carried dirty content restored after the failure"
        );
    }

    /// Requesting the branch already checked out is a no-op outcome.
    #[test]
    fn prepare_branch_already_on_branch_is_noop() {
        let repo = mock_diverged();
        let out = prepare_branch_impl(&repo.git, repo.root(), "feat/prev", "main").unwrap();
        assert_eq!(
            out,
            PrepareBranchOutcome::AlreadyOnBranch {
                branch: "feat/prev".into()
            }
        );
        assert_eq!(repo.git.current_branch(), "feat/prev");
    }

    /// Untracked `.coding/` files (the just-created plan `.md` in production)
    /// stay in place across the switch — they cannot block a checkout.
    #[test]
    fn prepare_branch_leaves_untracked_plan_files_in_place() {
        let repo = mock_diverged();
        repo.write(
            ".coding/plans/aaaaaaaa-0000-0000-0000-000000000000.md",
            "# plan\n",
        );
        repo.git.set_porcelain(
            "?? .coding/plans/aaaaaaaa-0000-0000-0000-000000000000.md\n M .coding/backlog.jsonl\n",
        );

        let out = prepare_branch_impl(&repo.git, repo.root(), "fix/new", "main").unwrap();

        assert!(matches!(out, PrepareBranchOutcome::Created { .. }));
        assert_eq!(
            repo.read(".coding/plans/aaaaaaaa-0000-0000-0000-000000000000.md"),
            "# plan\n",
            "untracked plan file survives the switch untouched"
        );
    }

    /// Review finding 1 (2026-08-20 create-plan-branch review): once
    /// `git checkout <base>` has succeeded, a failure of the branch fork must
    /// return HEAD to the ORIGINAL branch — an Err that left HEAD on `base`
    /// (main) would let the item's work commit to main. `fix/a..b` passes our
    /// name validator but git rejects it (`..` is invalid in a ref), so the
    /// fork fails after the base checkout succeeded — the exact path.
    #[test]
    fn prepare_branch_failure_after_base_checkout_restores_original_branch() {
        let repo = mock_diverged();

        let err = prepare_branch_impl(&repo.git, repo.root(), "fix/a..b", "main").unwrap_err();

        assert!(
            err.contains("fix/a..b"),
            "reason names the failed branch: {err}"
        );
        assert_eq!(
            repo.git.current_branch(),
            "feat/prev",
            "HEAD must be back on the original branch, not main"
        );
        assert_eq!(
            repo.read(".coding/backlog.jsonl"),
            "{\"items\":[{\"status\":\"in_flight\"}]}\n",
            "carried dirty content restored after the failure"
        );
    }

    /// Review finding 2d: a tracked `.coding/` file DELETED in the worktree
    /// carries the deletion — it must stay deleted after the switch, not be
    /// resurrected from the base branch.
    #[test]
    fn prepare_branch_carries_a_deletion() {
        let repo = mock_diverged();
        repo.remove_file(".coding/backlog.jsonl");
        repo.git.set_porcelain(" D .coding/backlog.jsonl\n");

        let out = prepare_branch_impl(&repo.git, repo.root(), "fix/new", "main").unwrap();

        assert!(matches!(out, PrepareBranchOutcome::Created { .. }));
        assert!(
            !repo.path_exists(".coding/backlog.jsonl"),
            "the deletion was carried — the file must stay absent on the new branch"
        );
    }

    /// Direct pin for the untrimmed-output contract (review finding 4): the
    /// first porcelain line's leading status-flag space is DATA. The old
    /// trimmed `git()` ate it, so ` M .coding/x` parsed as path
    /// `coding/x` — outside `.coding/` — and every carry was refused.
    #[test]
    fn parse_porcelain_line_keeps_the_leading_flag_space() {
        let (x, y, path) = parse_porcelain_line(" M .coding/backlog.jsonl").unwrap();
        assert_eq!(x, ' ');
        assert_eq!(y, 'M');
        assert_eq!(path, ".coding/backlog.jsonl");

        // Untracked marker columns and rename post-images parse too.
        let (x, y, _) = parse_porcelain_line("?? scratch.txt").unwrap();
        assert_eq!((x, y), ('?', '?'));
        let (_, _, renamed) = parse_porcelain_line("R  old.txt -> new.txt").unwrap();
        assert_eq!(renamed, "new.txt");

        // Too short to be a porcelain entry.
        assert!(parse_porcelain_line("").is_none());
        assert!(parse_porcelain_line(" M").is_none());
    }

    // ── work_branch_name: stable per-directory slug ────────────────────────

    #[test]
    fn work_branch_name_sanitizes_the_root_basename() {
        // Lowercased; non-alphanumeric runs collapse to a single `-`; the
        // `wt/` prefix keeps it out of main's namespace.
        for (input, expected) in [
            ("AgenticCoding", "wt/agenticcoding"),
            ("My Project!", "wt/my-project"),
            ("feat--branch!!", "wt/feat-branch"),
            ("already-lower", "wt/already-lower"),
        ] {
            let root = std::path::Path::new(input);
            assert_eq!(work_branch_name(root), expected, "input {input:?}");
        }
        // A path with no basename (root) or an empty path falls back to wt/work.
        assert_eq!(work_branch_name(std::path::Path::new("/")), "wt/work");
        assert_eq!(work_branch_name(std::path::Path::new("")), "wt/work");
    }

    // ── ensure_work_branch: auto-fork from main, reuse otherwise ───────────

    /// On `main` with a clean tree: forks the stable per-directory branch
    /// straight from main (Created), and the branch name is the slug derived
    /// from the repo's directory.
    #[test]
    fn ensure_work_branch_forks_stable_branch_from_main() {
        let repo = MockRepo {
            dir: tempfile::tempdir().unwrap(),
            git: MockGit::new(),
        };
        let expected = work_branch_name(repo.root());

        let out = ensure_work_branch_impl(&repo.git, repo.root()).unwrap();

        assert!(
            matches!(
                out,
                Some(PrepareBranchOutcome::Created { ref branch, base: ref b, carried: 0 })
                if branch == &expected && b == "main"
            ),
            "forked the stable branch from main: {out:?}"
        );
        assert_eq!(repo.git.current_branch(), expected);
    }

    /// Already on a non-main branch: reused (AlreadyOnBranch), no mutation —
    /// the in-flight work stays put.
    #[test]
    fn ensure_work_branch_reuses_a_non_main_branch() {
        let repo = mock_diverged();
        let dirty_before = repo.read(".coding/backlog.jsonl");

        let out = ensure_work_branch_impl(&repo.git, repo.root()).unwrap();

        assert_eq!(
            out,
            Some(PrepareBranchOutcome::AlreadyOnBranch {
                branch: "feat/prev".into()
            }),
            "an existing non-main branch is reused, not re-forked"
        );
        assert_eq!(repo.git.current_branch(), "feat/prev", "no branch switch");
        // The dirty bookkeeping was not touched (no carry ran).
        assert_eq!(repo.read(".coding/backlog.jsonl"), dirty_before);
    }

    /// A second call after returning to `main` resumes the existing stable
    /// branch (SwitchedExisting) — never a duplicate fork.
    #[test]
    fn ensure_work_branch_resumes_an_existing_stable_branch() {
        let repo = MockRepo {
            dir: tempfile::tempdir().unwrap(),
            git: MockGit::new(),
        };
        let expected = work_branch_name(repo.root());
        // First call forks it.
        ensure_work_branch_impl(&repo.git, repo.root()).unwrap();
        assert_eq!(repo.git.current_branch(), expected);
        // Return to main (clean tree).
        repo.git.set_branch("main");

        let out = ensure_work_branch_impl(&repo.git, repo.root()).unwrap();

        assert!(
            matches!(
                out,
                Some(PrepareBranchOutcome::SwitchedExisting { ref branch, carried: 0 })
                if branch == &expected
            ),
            "the existing stable branch is resumed, not re-forked: {out:?}"
        );
        assert_eq!(repo.git.current_branch(), expected);
    }

    /// A detached HEAD is treated like `main`: fork the stable branch from
    /// main rather than reusing the detached HEAD (work on a detached HEAD
    /// is easily lost on the next checkout).
    #[test]
    fn ensure_work_branch_forks_from_main_when_detached() {
        let repo = MockRepo {
            dir: tempfile::tempdir().unwrap(),
            git: MockGit::new(),
        };
        let expected = work_branch_name(repo.root());
        // Detach HEAD.
        repo.git.set_branch("HEAD");

        let out = ensure_work_branch_impl(&repo.git, repo.root()).unwrap();

        assert!(
            matches!(
                out,
                Some(PrepareBranchOutcome::Created { ref branch, base: ref b, carried: 0 })
                if branch == &expected && b == "main"
            ),
            "forked the stable branch from main, not reused the detached HEAD: {out:?}"
        );
        assert_eq!(repo.git.current_branch(), expected);
    }

    /// Outside a git repo: Ok(None) — create_plan proceeds with no note
    /// (planning is never blocked on git).
    #[test]
    fn ensure_work_branch_returns_none_outside_a_repo() {
        let dir = tempfile::tempdir().unwrap();
        let git = MockGit::new();
        git.set_not_a_repo();
        let out = ensure_work_branch_impl(&git, dir.path()).unwrap();
        assert!(out.is_none(), "not a repo → no auto-fork: {out:?}");
    }

    // ── Run-All pre-item checkpoint: never commit to main (backlog
    // 04bd6977, security review MEDIUM) ──────────────────────────────────
    //
    // run_all_dispatch_next checkpoints BEFORE create_plan's auto-fork
    // runs, so a session sitting on main (e.g. right after merge_to_main
    // lands a branch) committed the dirty tree (typically the
    // .coding/backlog.jsonl status flip) straight to main — violating the
    // never-commit-to-main constitution. The checkpoint must land on the
    // per-directory wt/* working branch (the same auto-fork semantics
    // create_plan applies), and a refused fork must stop the checkpoint
    // rather than commit real work to main.

    /// On main with the typical dirty .coding/ flip, the checkpoint commit
    /// must land on the wt/* work branch — never on main.
    #[test]
    fn checkpoint_on_main_lands_the_commit_on_the_work_branch() {
        let repo = MockRepo {
            dir: tempfile::tempdir().unwrap(),
            git: MockGit::new(),
        };
        // The typical dirty state at dispatch time: the backlog flip.
        repo.git.set_porcelain(" M .coding/backlog.jsonl\n");
        repo.write(
            ".coding/backlog.jsonl",
            "{\"items\":[{\"status\":\"in_flight\"}]}\n",
        );
        let expected = work_branch_name(repo.root());

        let sha =
            checkpoint_on_work_branch_impl(&repo.git, repo.root(), &item("i1", "do the thing"))
                .unwrap();

        assert_eq!(sha, repo.git.head_sha(), "returns the post-commit HEAD");
        let commits = repo.git.commits();
        assert_eq!(
            commits.len(),
            2,
            "the dirty tree was committed (initial + checkpoint)"
        );
        assert!(
            commits[1].contains("backlog: pre-item checkpoint (i1 do the thing)"),
            "commit message: {}",
            commits[1]
        );
        assert_eq!(
            repo.git.current_branch(),
            expected,
            "the checkpoint commit landed on the wt/* work branch, never main"
        );
    }

    /// Already on the wt/* branch: the checkpoint lands there with no
    /// re-fork churn (the branch is unchanged).
    #[test]
    fn checkpoint_on_the_work_branch_stays_put() {
        let repo = MockRepo {
            dir: tempfile::tempdir().unwrap(),
            git: MockGit::new(),
        };
        let expected = work_branch_name(repo.root());
        repo.git.set_branch(&expected);
        repo.git.add_branch(&expected);
        repo.git.set_porcelain(" M .coding/backlog.jsonl\n");
        repo.write(
            ".coding/backlog.jsonl",
            "{\"items\":[{\"status\":\"in_flight\"}]}\n",
        );

        let sha =
            checkpoint_on_work_branch_impl(&repo.git, repo.root(), &item("i1", "do the thing"))
                .unwrap();

        assert_eq!(sha, repo.git.head_sha());
        assert_eq!(
            repo.git.commits().len(),
            2,
            "the dirty tree was committed (initial + checkpoint)"
        );
        assert_eq!(repo.git.current_branch(), expected, "no branch switch");
    }

    /// A dirty tree outside .coding/ refuses the fork — the checkpoint must
    /// NOT fall through to committing real work to main. Nothing is
    /// committed; the error names the refusal.
    #[test]
    fn checkpoint_refused_fork_commits_nothing() {
        let repo = MockRepo {
            dir: tempfile::tempdir().unwrap(),
            git: MockGit::new(),
        };
        // Real work in the tree — must never ride a branch switch or a
        // main commit.
        repo.git.set_porcelain(" M src/lib.rs\n");
        repo.write("src/lib.rs", "fn real_work() {}\n");

        let res =
            checkpoint_on_work_branch_impl(&repo.git, repo.root(), &item("i1", "do the thing"));

        let err = res.expect_err("the refused fork stops the checkpoint");
        assert!(
            err.contains("work-branch fork refused"),
            "the error names the refusal: {err}"
        );
        assert_eq!(
            repo.git.commits(),
            vec!["initial".to_string()],
            "nothing was committed — especially not to main"
        );
        assert_eq!(repo.git.current_branch(), "main", "still on main, untouched");
    }

    // ── branch_hint: finish-digest branch probe ─────────────────────────────
    //
    // These test the success path of branch_hint_impl (the format string +
    // the main/master/empty guards) using MockGit — no real git subprocess.
    // The failure path (not a repo → None) is covered by the
    // capture_outside_a_git_repo test (#[ignore]'d integration).

    #[test]
    fn branch_hint_impl_formats_non_main_branch() {
        let git = MockGit::new();
        git.set_branch("feat/x");
        let root = Path::new("/tmp/mock");
        let hint = branch_hint_impl(&git, root);
        assert!(
            hint.as_ref().unwrap().contains("branch feat/x @ "),
            "hint names the branch + sha: {:?}",
            hint
        );
        assert!(
            hint.unwrap()
                .contains("(unmerged — exists only on this branch)"),
            "hint carries the unmerged marker"
        );
    }

    #[test]
    fn branch_hint_impl_returns_none_on_main() {
        let git = MockGit::new(); // defaults to "main"
        assert!(
            branch_hint_impl(&git, Path::new("/tmp/mock")).is_none(),
            "main → no hint"
        );
    }

    #[test]
    fn branch_hint_impl_returns_none_on_master() {
        let git = MockGit::new();
        git.set_branch("master");
        assert!(
            branch_hint_impl(&git, Path::new("/tmp/mock")).is_none(),
            "master → no hint"
        );
    }

    #[test]
    fn head_commit_sha_impl_reads_rev_parse_head() {
        // The reviewer-round base revision (backlog 85313a7e) is HEAD at
        // dispatch time.
        let git = MockGit::new();
        assert_eq!(
            head_commit_sha_impl(&git, Path::new("/tmp/mock")).unwrap(),
            "abc123"
        );
    }

    #[test]
    fn changed_paths_since_lists_tracked_changes_and_untracked_files() {
        // A re-review's changed-file set = `git diff --name-status <base>`
        // (staged + unstaged against the base) plus untracked files from
        // `status --porcelain -uall`. Gitignored paths are absent by
        // construction, so generated noise never reaches the prompt.
        let git = MockGit::new();
        git.set_diff_name_status("M\tsrc/a.rs\nA\tsrc/new.rs\nR100\told.rs\tnew2.rs\n");
        git.set_porcelain("?? docs/new.md\n M src/a.rs\n");
        let paths = changed_paths_since_impl(&git, Path::new("/tmp/mock"), "abc123").unwrap();
        assert_eq!(
            paths,
            vec![
                "M src/a.rs".to_string(),
                "A src/new.rs".to_string(),
                "R100 old.rs new2.rs".to_string(),
                "?? docs/new.md".to_string(),
            ],
            "tracked changes (tab-normalized, rename fields kept), then untracked"
        );
    }

    #[test]
    fn changed_paths_since_is_empty_on_a_clean_tree() {
        let git = MockGit::new();
        assert!(changed_paths_since_impl(&git, Path::new("/tmp/mock"), "abc123")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn changed_paths_since_rejects_an_injected_base_before_running_git() {
        // The base revision round-trips through the plan file, so it is
        // untrusted input: a `-` prefix is option injection, whitespace /
        // metacharacters are refused, and an empty base is refused — all
        // before git is ever invoked.
        let git = MockGit::new();
        let err =
            changed_paths_since_impl(&git, Path::new("/tmp/mock"), "--output=/tmp/x").unwrap_err();
        assert!(err.contains("option injection"), "{err}");
        let err = changed_paths_since_impl(&git, Path::new("/tmp/mock"), "abc 123").unwrap_err();
        assert!(err.contains("metacharacters"), "{err}");
        let err = changed_paths_since_impl(&git, Path::new("/tmp/mock"), "").unwrap_err();
        assert!(err.contains("empty"), "{err}");
    }

    #[test]
    fn changed_paths_since_propagates_a_git_failure() {
        let git = MockGit::new();
        git.set_diff_error("fatal: bad revision");
        let err = changed_paths_since_impl(&git, Path::new("/tmp/mock"), "abc123").unwrap_err();
        assert!(err.contains("bad revision"), "{err}");
    }
}
