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
//! resolves Done the app lands its branch itself. Two paths (backlog
//! b52b041a, mirroring the skill's DECISION 2027-01-11): an UNPROTECTED
//! `main` gets the direct landing — a `--no-ff` merge via the dedicated
//! landing worktree, then branch delete + worktree removal; a PROTECTED
//! `main` (an ACTIVE GitHub ruleset with a `pull_request` rule — this
//! repo's 23755694) cannot receive direct pushes (GH013), so the
//! landing pushes the branch and opens a PR the human approves and
//! merges — the branch is KEPT (the PR's head), only the worktree is
//! removed. Merge conflicts are SURFACED, never auto-resolved: the merge
//! is aborted, the branch + worktree are kept, and the conflicted file
//! list is returned for the item note.
//!
//! Known residual (review L3, 2026-09-21): a detection FALSE NEGATIVE
//! (gh transiently failing while the repo IS protected) takes the direct
//! path — the local merge strands with no later signal (the direct path
//! never pushes `main`, so no GH013 ever fires) and a later merge_to_main
//! GH013 recovery would discard it. Fail-open is still the right call
//! (gh-missing forces it; a false POSITIVE would push branches + open
//! PRs on every unprotected repo), but the residual is real and
//! accepted.
//!
//! All functions run blocking git subprocesses on the blocking thread
//! pool (see `git_ops` for why). Unlike the rest of `git_ops`, the tests
//! here use REAL git in tempdir repos — worktrees are filesystem-level
//! (a linked worktree is a real directory with a real checkout + `.git`
//! file), which the in-memory `MockGit` cannot represent.

use std::path::{Path, PathBuf};

use crate::project::git_ops::{gh_raw, git_raw};

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
        // Both ops run independently (review R3-L2): a remove failure
        // must not skip the branch delete — a stale `wt/runall-*` branch
        // blocks every later spawned-lane dispatch of the item ("branch
        // already exists" → the fill's skip arm → permanently
        // undispatchable via lanes).
        let removed = remove_worktree_only_impl(&main_root, &worktree);
        let deleted = git_raw(&main_root, &["branch", "-D", &branch]);
        removed?;
        deleted?;
        Ok(())
    })
    .await
    .map_err(|e| format!("worktree remove task failed: {e}"))?
}

/// Remove a dispatched item's worktree WITHOUT deleting its branch —
/// the PR-path landing (backlog b52b041a): the branch is the PR's head
/// and must survive the human merge. Merged `wt/runall-*` refs are
/// left in place — no sweeper deletes them yet (review L4,
/// 2026-09-21; a follow-up is queued); a stale branch only blocks
/// re-dispatch of a Done item, which never re-dispatches.
pub async fn remove_worktree_only(
    main_root: PathBuf,
    worktree: PathBuf,
) -> Result<(), String> {
    tokio::task::spawn_blocking(move || remove_worktree_only_impl(&main_root, &worktree))
        .await
        .map_err(|e| format!("worktree remove task failed: {e}"))?
}

/// The synchronous core of [`remove_worktree_only`] — shared with
/// [`remove_item_worktree`] (which adds the branch delete). Prunes
/// stale worktree metadata first (review R3-L2): a worktree whose
/// directory is already gone (deleted manually) leaves stale metadata
/// that makes the remove (and any branch delete) fail ("cannot delete
/// branch used by worktree").
fn remove_worktree_only_impl(main_root: &Path, worktree: &Path) -> Result<(), String> {
    let worktree_str = worktree
        .to_str()
        .ok_or("non-UTF-8 worktree path")?
        .to_string();
    let _ = git_raw(main_root, &["worktree", "prune"]);
    git_raw(main_root, &["worktree", "remove", "--force", &worktree_str]).map(|_| ())
}

/// Parse a GitHub remote URL into `(owner, repo)` — the https, ssh,
/// and scp-like forms, with or without the trailing `.git`, a trailing
/// `/`, an ssh port (`ssh://git@github.com:22/o/r`), and a mixed-case
/// host (`https://GitHub.com/o/r` — host matching is case-insensitive;
/// the owner/repo segment stays case-sensitive) (backlog b52b041a: the
/// ruleset-aware landing needs the slug for `gh api`; the edge shapes
/// are review L2, 2026-09-21).
///
/// Non-GitHub remotes (local paths, other hosts) return `None` — the
/// caller treats that as "nothing to check" (the direct path).
fn parse_github_remote_url(url: &str) -> Option<(String, String)> {
    let url = url.trim();
    // Host matching is case-insensitive; `to_ascii_lowercase`
    // preserves length, so the lowercase suffix lengths index `url`
    // directly.
    let lower = url.to_ascii_lowercase();
    let mut rest = None;
    for prefix in [
        "https://github.com/",
        "ssh://git@github.com/",
        "git@github.com:",
    ] {
        if let Some(suffix) = lower.strip_prefix(prefix) {
            rest = Some(&url[url.len() - suffix.len()..]);
            break;
        }
    }
    // The ssh port form: ssh://git@github.com:<port>/o/r — drop the
    // port segment (only when it is all digits, so a malformed
    // ssh://git@github.com:o/r does not silently drop its owner).
    if rest.is_none() {
        if let Some(after) = lower.strip_prefix("ssh://git@github.com:") {
            let url_after = &url[url.len() - after.len()..];
            if let Some(slash) = url_after.find('/') {
                let port = &url_after[..slash];
                if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) {
                    rest = Some(&url_after[slash + 1..]);
                }
            }
        }
    }
    let rest = rest?;
    let rest = rest.trim_end_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let (owner, repo) = rest.split_once('/')?;
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

/// The repo's GitHub `(owner, repo)` slug, from the `origin` remote —
/// `None` when there is no origin or it is not a GitHub remote (the
/// ruleset check only applies to GitHub repos; everything else takes
/// the direct path).
fn remote_github_slug(main_root: &Path) -> Option<(String, String)> {
    let url = git_raw(main_root, &["remote", "get-url", "origin"]).ok()?;
    parse_github_remote_url(url.trim())
}

/// Whether a `gh api repos/{owner}/{repo}/rulesets` payload shows a
/// ruleset that blocks direct pushes to main: an ACTIVE ruleset
/// carrying a `pull_request` rule (this repo: 23755694 — "Protect
/// from direct pushing.", required_approving_review_count 1).
///
/// Fail-open by design (backlog b52b041a, mirroring the merge_to_main
/// skill's decision): malformed JSON or an unexpected shape reads as
/// "no ruleset" → the direct path. A false negative strands one local
/// merge on the protected repo, but a false POSITIVE would push
/// branches + open PRs on every unprotected repo — the worse failure.
fn rulesets_json_blocks_direct_push(json: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return false;
    };
    let Some(rulesets) = v.as_array() else {
        return false;
    };
    rulesets.iter().any(|rs| {
        rs.get("enforcement").and_then(|e| e.as_str()) == Some("active")
            && rs
                .get("rules")
                .and_then(|r| r.as_array())
                .is_some_and(|rules| {
                    rules.iter().any(|rule| {
                        rule.get("type").and_then(|t| t.as_str()) == Some("pull_request")
                    })
                })
    })
}

/// Whether `main` is protected against direct pushes (backlog b52b041a):
/// an ACTIVE GitHub ruleset with a `pull_request` rule on the repo.
///
/// Mirrors the merge_to_main skill's up-front check (DECISION
/// 2027-01-11): `gh repo view --json nameWithOwner` for the slug
/// (falling back to parsing the origin remote URL), then
/// `gh api repos/{owner}/{repo}/rulesets`. ANY failure — gh missing,
/// unauthenticated, no GitHub remote, a network error — reads as
/// `false` (the direct path): gh-missing also means `gh pr create`
/// would fail, so the PR path is not an option (the skill made the
/// same call). Runs 2-3 subprocesses per landing — landings are
/// serialized and rare; no caching by design (a ruleset added between
/// two landings must be seen by the second).
///
/// Residual (review L3, 2026-09-21): a transient failure on a
/// protected repo strands the local merge with no recovery signal —
/// accepted, because the alternative (fail-closed) would PR-flow
/// every unprotected repo on any gh hiccup.
fn main_is_protected(main_root: &Path) -> bool {
    let slug: Option<(String, String)> = gh_raw(
        main_root,
        &["repo", "view", "--json", "nameWithOwner"],
    )
    .ok()
    .and_then(|out| {
        let name = serde_json::from_str::<serde_json::Value>(&out)
            .ok()?
            .get("nameWithOwner")?
            .as_str()?
            .to_string();
        let (owner, repo) = name.split_once('/')?;
        Some((owner.to_string(), repo.to_string()))
    });
    let Some((owner, repo)) = slug.or_else(|| remote_github_slug(main_root)) else {
        return false;
    };
    let Ok(json) = gh_raw(
        main_root,
        &["api", &format!("repos/{owner}/{repo}/rulesets")],
    ) else {
        return false;
    };
    rulesets_json_blocks_direct_push(&json)
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

/// How a landing landed (backlog b52b041a): the direct path merges into
/// local `main`; the protected path opens a PR the human merges.
#[derive(Debug)]
pub enum Landed {
    /// Merged into local `main` (`--no-ff`) — the direct path; the
    /// branch delete + worktree removal follow in
    /// [`remove_item_worktree`].
    Merged,
    /// A PR was opened (the protected path): its URL. The branch is
    /// KEPT for the human merge — only the worktree is removed
    /// ([`remove_worktree_only`]).
    PullRequest(String),
}

/// Land one dispatched item's branch — via the dedicated landing
/// worktree on an unprotected `main`, or via a PR on a protected one
/// (backlog b52b041a).
///
/// The landing worktree (`<main_root>/.worktrees/landing`, `main`
/// checked out) is provisioned lazily and reused across landings —
/// `main` cannot be checked out in a linked worktree while the main
/// worktree holds a different branch, and the main worktree itself may
/// be mid-item (the main agent's uncommitted work must never be touched
/// by a landing). The CALLER serializes landings (one at a time).
///
/// `Ok(Landed::Merged)` — the direct path: merged (`--no-ff`, so `main`
/// only ever receives merge commits, mirroring the merge_to_main skill).
/// The branch delete + worktree removal happen in
/// [`remove_item_worktree`] afterwards (git refuses to delete a branch
/// its worktree still has checked out).
/// `Ok(Landed::PullRequest(url))` — the protected path: the branch was
/// pushed and a PR opened (the human approves and merges); the branch
/// is KEPT and only the worktree is removed ([`remove_worktree_only`]).
/// `Err(LandError::Conflict(files))` — the merge conflicted: it was
/// aborted (the landing worktree is back on clean `main`), the branch is
/// KEPT for manual resolution, and the conflicted paths are returned for
/// the item note.
pub async fn land_item_branch(
    main_root: PathBuf,
    branch: String,
) -> Result<Landed, LandError> {
    tokio::task::spawn_blocking(move || land_item_branch_impl(&main_root, &branch))
        .await
        .map_err(|e| LandError::Git(format!("landing task failed: {e}")))?
}

/// The synchronous core of [`land_item_branch`].
fn land_item_branch_impl(main_root: &Path, branch: &str) -> Result<Landed, LandError> {
    // Backlog b52b041a: a protected `main` cannot receive the app-managed
    // direct merge — the local merge commits are unpushable (GH013 on
    // any direct push) and the post-landing branch delete would strand
    // them — so land via a PR instead (the merge_to_main skill's design,
    // DECISION 2027-01-11). Checked BEFORE the landing worktree: the PR
    // path never touches it.
    if main_is_protected(main_root) {
        return land_via_pull_request(main_root, branch);
    }
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
        Ok(_) => Ok(Landed::Merged),
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

/// The protected-main landing (backlog b52b041a): push the branch and
/// open a PR — the human approves and merges (the app cannot
/// self-approve, required_approving_review_count 1). NEVER bypass the
/// ruleset (no force-push, no admin override, no ruleset edits).
///
/// The branch is KEPT (the PR's head; merged `wt/runall-*` refs are
/// left in place — no sweeper yet, review L4 2026-09-21). An
/// already-open PR for the branch is reported instead of erroring
/// (idempotent re-landing, the skill's rule). Any failure surfaces as
/// [`LandError::Git`] naming the PR requirement and the exact command
/// to run manually.
///
/// Unlike the skill's PR path (which builds the branch first — PRs get
/// no CI), this does NOT build: the spawned agent's own closing
/// sequence already ran the tests in the worktree before the plan
/// completed — the producer-side guarantee the skill lacks (review
/// L5, 2026-09-21, a conscious deviation from the mirrored design).
fn land_via_pull_request(main_root: &Path, branch: &str) -> Result<Landed, LandError> {
    let pr_requirement = format!(
        "main is protected (an ACTIVE pull_request ruleset blocks direct \
         pushes) — the branch {branch} must land via a pull request; run \
         manually: gh pr create --base main --head {branch}"
    );
    // Push the branch (the PR's head); -u ties it to origin for the
    // human's later operations.
    git_raw(main_root, &["push", "-u", "origin", branch])
        .map_err(|e| LandError::Git(format!("{pr_requirement} (push failed: {e})")))?;
    // Title: the branch's latest commit subject; body: the commits the
    // PR brings + the review note.
    let mut title = git_raw(main_root, &["log", "-1", "--pretty=%s", branch])
        .map_err(LandError::Git)?
        .trim()
        .to_string();
    // A tip commit with an empty subject (git allows it via
    // --allow-empty-message) would send `--title ""` — gh rejects a
    // blank title. Fall back to the branch name (review L1,
    // 2026-09-21): the landing stays robust instead of degrading to
    // the error path.
    if title.is_empty() {
        title = branch.to_string();
    }
    let commits = git_raw(main_root, &["log", &format!("main..{branch}"), "--oneline"])
        .map_err(LandError::Git)?;
    let body = format!(
        "{commits}\n\nApp-managed run-all landing on a protected main — \
         awaiting human review (the app cannot self-approve)."
    );
    let url = match gh_raw(
        main_root,
        &[
            "pr",
            "create",
            "--base",
            "main",
            "--head",
            branch,
            "--title",
            &title,
            "--body",
            &body,
        ],
    ) {
        Ok(out) => out.trim().to_string(),
        Err(create_err) => {
            // A PR may already exist for the branch (an earlier landing
            // attempt, or a re-landing) — report it instead of failing
            // (the skill's idempotence rule).
            let existing = gh_raw(main_root, &["pr", "view", branch, "--json", "url"])
                .ok()
                .and_then(|out| {
                    serde_json::from_str::<serde_json::Value>(&out)
                        .ok()?
                        .get("url")?
                        .as_str()
                        .map(str::to_string)
                });
            match existing {
                Some(url) => url,
                None => {
                    return Err(LandError::Git(format!(
                        "{pr_requirement} (gh pr create failed: {create_err})"
                    )))
                }
            }
        }
    };
    Ok(Landed::PullRequest(url))
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

    #[test]
    fn parse_github_remote_url_takes_both_forms() {
        // Backlog b52b041a: the ruleset check needs the owner/repo slug
        // from whatever remote form the clone uses.
        let expect = Some(("chessIthaca".to_string(), "Mnemo".to_string()));
        assert_eq!(
            parse_github_remote_url("https://github.com/chessIthaca/Mnemo"),
            expect
        );
        assert_eq!(
            parse_github_remote_url("https://github.com/chessIthaca/Mnemo.git"),
            expect
        );
        assert_eq!(
            parse_github_remote_url("git@github.com:chessIthaca/Mnemo.git"),
            expect
        );
        assert_eq!(
            parse_github_remote_url("ssh://git@github.com/chessIthaca/Mnemo.git"),
            expect
        );
        // Review L2 (2026-09-21): trailing slash, ssh port, and
        // mixed-case host — realistic remote forms that must all yield
        // the slug (a None here fail-opens the landing onto the direct
        // path on a protected repo).
        assert_eq!(
            parse_github_remote_url("https://github.com/chessIthaca/Mnemo/"),
            expect
        );
        assert_eq!(
            parse_github_remote_url("ssh://git@github.com:22/chessIthaca/Mnemo.git"),
            expect
        );
        assert_eq!(
            parse_github_remote_url("https://GitHub.com/chessIthaca/Mnemo.git"),
            expect
        );
        // Non-GitHub remotes and garbage: no slug → the direct path.
        assert_eq!(parse_github_remote_url("https://gitlab.com/o/r"), None);
        assert_eq!(parse_github_remote_url("C:\\some\\path"), None);
        assert_eq!(parse_github_remote_url(""), None);
    }

    #[test]
    fn remote_github_slug_reads_the_origin_remote() {
        let repo = init_repo();
        assert_eq!(
            remote_github_slug(repo.path()),
            None,
            "no remote → no slug"
        );
        git_raw(
            repo.path(),
            &["remote", "add", "origin", "https://github.com/chessIthaca/Mnemo.git"],
        )
        .unwrap();
        assert_eq!(
            remote_github_slug(repo.path()),
            Some(("chessIthaca".to_string(), "Mnemo".to_string()))
        );
    }

    #[test]
    fn rulesets_json_detects_only_active_pull_request_rulesets() {
        // Backlog b52b041a: the real ruleset 23755694 shape (verified via
        // gh api during plan 8ad6fa91) must read as blocking; the
        // non-blocking shapes must not.
        let blocking = r#"[{"id":23755694,"source":"Repository","name":"Protect from direct pushing.","enforcement":"active","conditions":{"ref_name":{"include":["~DEFAULT_BRANCH"],"exclude":[]}},"rules":[{"type":"pull_request","parameters":{"required_approving_review_count":1}}]}]"#;
        assert!(rulesets_json_blocks_direct_push(blocking));
        // Inactive enforcement: the ruleset is disabled — direct pushes pass.
        let inactive = blocking.replace("\"active\"", "\"disabled\"");
        assert!(!rulesets_json_blocks_direct_push(&inactive));
        // Active but no pull_request rule (e.g. deletion-only): not a PR gate.
        let no_pr = r#"[{"id":1,"enforcement":"active","rules":[{"type":"deletion"}]}]"#;
        assert!(!rulesets_json_blocks_direct_push(no_pr));
        // No rulesets at all.
        assert!(!rulesets_json_blocks_direct_push("[]"));
        // Malformed / non-array: fail-open (the direct path), never a
        // false-positive PR flow on every repo.
        assert!(!rulesets_json_blocks_direct_push("not json"));
        assert!(!rulesets_json_blocks_direct_push("{}"));
    }

    #[tokio::test]
    async fn land_via_pull_request_surfaces_the_pr_requirement_when_the_push_fails() {
        // Backlog b52b041a: on a protected main the PR path's failure
        // mode must NAME the PR requirement (the acceptance criteria),
        // not surface a raw push error. The dead local remote makes the
        // push fail fast, deterministically, with no network.
        let repo = init_repo();
        git_raw(repo.path(), &["checkout", "-b", "wt/test"]).unwrap();
        let (worktree, branch) =
            provision_item_worktree(repo.path().to_path_buf(), "abcdef123456".to_string())
                .await
                .unwrap();
        std::fs::write(worktree.join("item.txt"), "item work\n").unwrap();
        git_raw(&worktree, &["add", "-A"]).unwrap();
        git_raw(&worktree, &["commit", "-m", "item work"]).unwrap();
        let dead_upstream = repo.path().join("no-such-upstream");
        git_raw(
            repo.path(),
            &["remote", "add", "origin", dead_upstream.to_str().unwrap()],
        )
        .unwrap();

        let err = land_via_pull_request(repo.path(), &branch).unwrap_err();
        match err {
            LandError::Git(e) => {
                assert!(
                    e.contains("must land via a pull request"),
                    "the error names the PR requirement: {e}"
                );
                assert!(
                    e.contains(&format!("gh pr create --base main --head {branch}")),
                    "the error carries the exact command: {e}"
                );
            }
            other => panic!("expected a git error, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn remove_worktree_only_keeps_the_branch() {
        // Backlog b52b041a: the PR-path landing keeps the branch (the
        // PR's head) — only the worktree goes.
        let repo = init_repo();
        let (worktree, branch) =
            provision_item_worktree(repo.path().to_path_buf(), "abcdef123456".to_string())
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
            "the branch is KEPT (the PR needs it)"
        );
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
