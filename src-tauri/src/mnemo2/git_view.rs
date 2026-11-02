// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]

//! The Git tab of the lead's Workbench: the checked-out branch, every local
//! branch's distance from `main`, the newest commits as a lane list, and the one
//! write the tab performs — a fast-forward of `main` to a branch (plan b5dc77ab,
//! step 3).
//!
//! Every read and the single write run through [`phase3::git`](crate::mnemo2::phase3::git),
//! so no shell is involved. Moving `main` never pushes and never rewrites
//! history: it refuses unless `main` is an ancestor of the target branch, refuses
//! while `main` is checked out in any worktree (a ref update under a live index
//! would leave that tree looking modified), and applies the update with
//! `git update-ref`'s compare-and-swap, so a commit that landed on `main`
//! between the check and the write is never lost.

use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::State;

use crate::mnemo2::ipc::Mnemo2State;
use crate::mnemo2::phase3::{git, git_error, git_value};

/// The most commits one overview returns — the lane list's length.
const MAX_COMMITS: usize = 50;

/// The most branches one overview lists; the current branch and `main` lead.
const MAX_BRANCHES: usize = 40;

/// The field separator inside one `git log` record.
const FIELD_SEPARATOR: char = '\u{1f}';

/// The record separator between `git log` records.
const RECORD_SEPARATOR: char = '\u{1e}';

/// One local branch and its distance from `main`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitBranch {
    /// The short branch name, exactly as Git spells it.
    pub name: String,
    /// True for the branch checked out in the project root.
    pub current: bool,
    /// Commits this branch has that `main` does not — what moving `main` gains.
    pub ahead: u64,
    /// Commits `main` has that this branch does not — nonzero blocks a fast-forward.
    pub behind: u64,
}

/// One row of the commit lane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitCommit {
    /// The full object id.
    pub id: String,
    /// The abbreviated id the row shows.
    pub short: String,
    /// The commit message's first line.
    pub summary: String,
    /// The author's name.
    pub author: String,
    /// The commit date in Git's relative form (`2 hours ago`).
    pub date: String,
    /// The branches and tags that point at this commit.
    pub refs: Vec<String>,
}

/// Everything the Git tab renders in one read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitOverview {
    /// The checked-out branch, or `None` when `HEAD` is detached.
    pub branch: Option<String>,
    /// The `main` tip's abbreviated id, or `None` when there is no `main`.
    pub main: Option<String>,
    /// The worktree that has `main` checked out, if any; moving `main` is
    /// refused while one does.
    pub main_worktree: Option<String>,
    /// True when that worktree has uncommitted changes.
    pub main_dirty: bool,
    /// Local branches: the current one first, then `main`, then by name.
    pub branches: Vec<GitBranch>,
    /// The newest commits, newest first.
    pub commits: Vec<GitCommit>,
    /// True when [`MAX_COMMITS`] cut the lane short.
    pub truncated: bool,
}

/// What a completed move of `main` reports back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitMove {
    /// The branch `main` now points at.
    pub branch: String,
    /// The new `main` tip, abbreviated.
    pub main: String,
    /// How many commits `main` gained.
    pub commits: u64,
}

/// Split `git rev-list --count --left-right main...<branch>` into
/// `(ahead, behind)` — commits only the branch has, then commits only `main` has.
fn parse_distance(raw: &str) -> Option<(u64, u64)> {
    let mut counts = raw.split_whitespace();
    let behind = counts.next()?.parse().ok()?;
    let ahead = counts.next()?.parse().ok()?;
    Some((ahead, behind))
}

/// Split one `git log` stream into commit rows; a malformed record is skipped
/// rather than failing the whole read.
fn parse_commits(raw: &str) -> Vec<GitCommit> {
    raw.split(RECORD_SEPARATOR)
        .map(str::trim)
        .filter(|record| !record.is_empty())
        .filter_map(|record| {
            let mut fields = record.split(FIELD_SEPARATOR);
            let id = fields.next()?.trim();
            let short = fields.next()?.trim();
            let summary = fields.next()?.trim();
            let author = fields.next()?.trim();
            let date = fields.next()?.trim();
            let refs = fields
                .next()
                .unwrap_or_default()
                .split(", ")
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_string)
                .collect();
            Some(GitCommit {
                id: id.to_string(),
                short: short.to_string(),
                summary: summary.to_string(),
                author: author.to_string(),
                date: date.to_string(),
                refs,
            })
        })
        .collect()
}

/// The worktree path that has `refs/heads/main` checked out, from
/// `git worktree list --porcelain` (one blank-line-separated block per tree).
fn parse_main_worktree(raw: &str) -> Option<String> {
    let mut path: Option<String> = None;
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("worktree ") {
            path = Some(rest.trim().to_string());
        } else if line.trim() == "branch refs/heads/main" {
            return path;
        } else if line.trim().is_empty() {
            path = None;
        }
    }
    None
}

/// Order branches the way the tab reads them: the current one, `main`, then names.
fn sort_branches(names: &mut [String], current: Option<&str>) {
    names.sort_by(|left, right| {
        let rank = |name: &str| {
            if Some(name) == current {
                0
            } else if name == "main" {
                1
            } else {
                2
            }
        };
        rank(left).cmp(&rank(right)).then_with(|| left.cmp(right))
    });
}

/// The worktree that has `main` checked out, when one does.
async fn main_worktree(root: &Path) -> Result<Option<String>, String> {
    let args = ["worktree", "list", "--porcelain"];
    let output = git(root, &args).await?;
    if !output.status.success() {
        return Err(git_error(&args, &output));
    }
    Ok(parse_main_worktree(&String::from_utf8_lossy(&output.stdout)))
}

/// True when a worktree holds changes a move of `main` must not walk over; a
/// failed status counts as dirty, which only ever refuses the move.
async fn worktree_dirty(root: &Path, worktree: &str) -> bool {
    match git(root, &["-C", worktree, "status", "--porcelain"]).await {
        Ok(output) => {
            !output.status.success()
                || !String::from_utf8_lossy(&output.stdout).trim().is_empty()
        }
        Err(_) => true,
    }
}

/// Ask Git a yes/no question (`merge-base --is-ancestor`).
async fn git_succeeds(root: &Path, args: &[&str]) -> Result<bool, String> {
    let output = git(root, args).await?;
    Ok(output.status.success())
}

/// Read the tab's overview: the branch, every branch's distance from `main`,
/// the newest commits, and where `main` is checked out.
pub(crate) async fn overview(root: &Path) -> Result<GitOverview, String> {
    let probe = git(root, &["rev-parse", "--git-dir"]).await?;
    if !probe.status.success() {
        return Err("this project is not a Git repository".to_string());
    }
    let branch = git(root, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .await
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|name| !name.is_empty());
    let main = git_value(root, &["rev-parse", "--short", "--verify", "--quiet", "refs/heads/main"])
        .await
        .ok();

    let mut names: Vec<String> = git_value(
        root,
        &["for-each-ref", "--format=%(refname:short)", "refs/heads/"],
    )
    .await?
    .lines()
    .map(str::trim)
    .filter(|name| !name.is_empty())
    .map(str::to_string)
    .collect();
    sort_branches(&mut names, branch.as_deref());
    names.truncate(MAX_BRANCHES);
    let mut branches = Vec::with_capacity(names.len());
    for name in names {
        let (ahead, behind) = match main {
            Some(_) => git_value(
                root,
                &[
                    "rev-list",
                    "--count",
                    "--left-right",
                    &format!("refs/heads/main...refs/heads/{name}"),
                ],
            )
            .await
            .ok()
            .and_then(|raw| parse_distance(&raw))
            .unwrap_or((0, 0)),
            // Without a local `main` there is nothing to measure distance from.
            None => (0, 0),
        };
        branches.push(GitBranch {
            current: branch.as_deref() == Some(name.as_str()),
            name,
            ahead,
            behind,
        });
    }

    let max = format!("--max-count={}", MAX_COMMITS + 1);
    let log = git_value(
        root,
        &[
            "log",
            &max,
            "--pretty=format:%H%x1f%h%x1f%s%x1f%an%x1f%ar%x1f%D%x1e",
            "HEAD",
        ],
    )
    .await
    .unwrap_or_default();
    let mut commits = parse_commits(&log);
    let truncated = commits.len() > MAX_COMMITS;
    commits.truncate(MAX_COMMITS);

    let main_worktree = if main.is_some() {
        main_worktree(root).await?
    } else {
        None
    };
    let main_dirty = match main_worktree.as_deref() {
        Some(worktree) => worktree_dirty(root, worktree).await,
        None => false,
    };
    Ok(GitOverview {
        branch,
        main,
        main_worktree,
        main_dirty,
        branches,
        commits,
        truncated,
    })
}

/// Fast-forward `main` to `branch` — the tab's only write.
pub(crate) async fn move_main(root: &Path, branch: &str) -> Result<GitMove, String> {
    let branch = branch.trim();
    if branch.is_empty() {
        return Err("no branch was named".to_string());
    }
    if branch == "main" {
        return Err("main already points at 'main'".to_string());
    }
    let branch_ref = format!("refs/heads/{branch}");
    let new = git_value(root, &["rev-parse", "--verify", "--quiet", &branch_ref])
        .await
        .map_err(|_| format!("there is no local branch named '{branch}'"))?;
    let old = git_value(root, &["rev-parse", "--verify", "--quiet", "refs/heads/main"])
        .await
        .map_err(|_| "this repository has no local 'main' branch".to_string())?;
    if new == old {
        return Err(format!("main already points at '{branch}'"));
    }
    // A ref update under a live index would leave that worktree looking modified,
    // so main must not be checked out anywhere; name the tree that holds it.
    if let Some(worktree) = main_worktree(root).await? {
        return Err(if worktree_dirty(root, &worktree).await {
            format!(
                "main is checked out in '{worktree}' with uncommitted changes; commit or stash them there first"
            )
        } else {
            format!(
                "main is checked out in '{worktree}'; check out another branch there before moving main"
            )
        });
    }
    if !git_succeeds(
        root,
        &["merge-base", "--is-ancestor", "refs/heads/main", &branch_ref],
    )
    .await?
    {
        return Err(format!(
            "moving main to '{branch}' would not be a fast-forward: main has commits that '{branch}' does not"
        ));
    }
    let commits = git_value(
        root,
        &[
            "rev-list",
            "--count",
            &format!("refs/heads/main..{branch_ref}"),
        ],
    )
    .await
    .ok()
    .and_then(|raw| raw.trim().parse().ok())
    .unwrap_or(0);
    // The expected old value makes this a compare-and-swap: if main moved after
    // the checks above, Git refuses instead of dropping that commit.
    git_value(
        root,
        &[
            "update-ref",
            "-m",
            &format!("mnemo2: move main to {branch}"),
            "refs/heads/main",
            &new,
            &old,
        ],
    )
    .await
    .map_err(|err| {
        format!("main moved while the tab was reading it; refresh and try again ({err})")
    })?;
    let main = git_value(root, &["rev-parse", "--short", "refs/heads/main"]).await?;
    Ok(GitMove {
        branch: branch.to_string(),
        main,
        commits,
    })
}

/// The open project's root, which every Git command runs in.
async fn project_root(state: &Mnemo2State) -> Result<PathBuf, String> {
    state
        .open_lead()
        .await
        .map(|lead| lead.workspace_root().to_path_buf())
        .ok_or_else(|| "Mnemo2 has no open project; open one first".to_string())
}

/// Read the Git tab's overview.
#[tauri::command]
pub async fn git_overview(state: State<'_, Mnemo2State>) -> Result<GitOverview, String> {
    let root = project_root(&state).await?;
    overview(&root).await
}

/// Fast-forward `main` to one branch; never pushes and never rewrites history.
#[tauri::command]
pub async fn git_move_main(
    state: State<'_, Mnemo2State>,
    branch: String,
) -> Result<GitMove, String> {
    let root = project_root(&state).await?;
    move_main(&root, &branch).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// Run Git in a test repository and fail loudly when it refuses.
    fn git_in(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("git should run");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Write a fixture file, creating its directory.
    fn write(root: &Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("fixture directory");
        }
        std::fs::write(path, contents).expect("fixture file");
    }

    /// The full object id `name` resolves to.
    fn rev(root: &Path, name: &str) -> String {
        let output = Command::new("git")
            .args(["rev-parse", name])
            .current_dir(root)
            .output()
            .expect("git should run");
        assert!(
            output.status.success(),
            "git rev-parse {name} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// The abbreviated object id `name` resolves to.
    fn short(root: &Path, name: &str) -> String {
        let output = Command::new("git")
            .args(["rev-parse", "--short", name])
            .current_dir(root)
            .output()
            .expect("git should run");
        assert!(
            output.status.success(),
            "git rev-parse --short {name} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// A repository whose `main` sits at the base commit and whose `work` branch
    /// is two commits ahead of it, checked out in `root`.
    fn repository() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path().join("repo");
        std::fs::create_dir(&root).expect("repository directory");
        git_in(&root, &["init", "-q", "-b", "main"]);
        git_in(&root, &["config", "user.name", "Test"]);
        git_in(&root, &["config", "user.email", "test@example.invalid"]);
        write(&root, "README.md", "# project\n");
        git_in(&root, &["add", "-A"]);
        git_in(&root, &["commit", "-qm", "base"]);
        git_in(&root, &["checkout", "-q", "-b", "work"]);
        write(&root, "src/one.rs", "fn one() {}\n");
        git_in(&root, &["add", "-A"]);
        git_in(&root, &["commit", "-qm", "work one"]);
        write(&root, "src/two.rs", "fn two() {}\n");
        git_in(&root, &["add", "-A"]);
        git_in(&root, &["commit", "-qm", "work two"]);
        (dir, root)
    }

    #[test]
    fn parse_distance_reads_left_right_counts() {
        // `main...branch` counts main-only commits first, then branch-only ones.
        assert_eq!(parse_distance("3\t2\n"), Some((2, 3)));
        assert_eq!(parse_distance("0\t0"), Some((0, 0)));
        assert_eq!(parse_distance(""), None);
        assert_eq!(parse_distance("one two"), None);
    }

    #[test]
    fn parse_commits_reads_one_record_per_commit() {
        let raw = [
            "abc123\u{1f}abc12\u{1f}work two\u{1f}Test\u{1f}2 hours ago\u{1f}HEAD -> work",
            "def456\u{1f}def45\u{1f}base\u{1f}Test\u{1f}3 days ago\u{1f}main, tag: v1",
            "",
        ]
        .join("\u{1e}");
        let commits = parse_commits(&raw);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].id, "abc123");
        assert_eq!(commits[0].short, "abc12");
        assert_eq!(commits[0].summary, "work two");
        assert_eq!(commits[0].refs, vec!["HEAD -> work".to_string()]);
        assert_eq!(
            commits[1].refs,
            vec!["main".to_string(), "tag: v1".to_string()]
        );
        // A record missing its fields is dropped, not half-rendered.
        assert!(parse_commits("only-an-id\u{1e}").is_empty());
    }

    #[test]
    fn parse_main_worktree_finds_the_tree_that_holds_main() {
        let raw = [
            "worktree C:/project",
            "HEAD 1111111",
            "branch refs/heads/work",
            "",
            "worktree C:/project/.worktrees/main",
            "HEAD 2222222",
            "branch refs/heads/main",
            "",
        ]
        .join("\n");
        assert_eq!(
            parse_main_worktree(&raw),
            Some("C:/project/.worktrees/main".to_string())
        );
        let without_main = [
            "worktree C:/project",
            "HEAD 1111111",
            "branch refs/heads/work",
            "",
        ]
        .join("\n");
        assert_eq!(parse_main_worktree(&without_main), None);
    }

    #[tokio::test]
    async fn overview_reports_the_branch_distances_and_newest_commits() {
        let (_dir, root) = repository();
        let read = overview(&root).await.expect("the overview reads");
        assert_eq!(read.branch.as_deref(), Some("work"));
        assert_eq!(read.main, Some(short(&root, "main")));
        assert_eq!(
            read.branches,
            vec![
                GitBranch {
                    name: "work".to_string(),
                    current: true,
                    ahead: 2,
                    behind: 0,
                },
                GitBranch {
                    name: "main".to_string(),
                    current: false,
                    ahead: 0,
                    behind: 0,
                },
            ]
        );
        assert_eq!(read.commits.len(), 3);
        assert_eq!(read.commits[0].summary, "work two");
        assert_eq!(read.commits[0].id, rev(&root, "work"));
        assert!(read.commits[0].refs.iter().any(|name| name.contains("work")));
        assert_eq!(read.commits[2].summary, "base");
        assert!(!read.truncated);
        // `main` is not checked out anywhere, so moving it is not blocked.
        assert_eq!(read.main_worktree, None);
        assert!(!read.main_dirty);
    }

    #[tokio::test]
    async fn overview_refuses_a_directory_that_is_not_a_repository() {
        let dir = tempfile::tempdir().expect("temp dir");
        let error = overview(dir.path()).await.expect_err("not a repository");
        assert!(error.contains("not a Git repository"), "{error}");
    }

    #[tokio::test]
    async fn move_main_fast_forwards_and_never_pushes() {
        let (dir, root) = repository();
        let base = rev(&root, "main");
        // A local remote proves the move stays local: its refs must not change.
        let bare = dir.path().join("origin.git");
        let bare_arg = bare.display().to_string().replace('\\', "/");
        git_in(&root, &["init", "-q", "--bare", &bare_arg]);
        git_in(&root, &["remote", "add", "origin", &bare_arg]);
        git_in(&root, &["push", "-q", "origin", "main:main"]);

        let moved = move_main(&root, "work").await.expect("main fast-forwards");
        assert_eq!(moved.branch, "work");
        assert_eq!(moved.commits, 2);
        assert_eq!(rev(&root, "main"), rev(&root, "work"));
        assert_eq!(moved.main, short(&root, "main"));

        let remote = Command::new("git")
            .args(["rev-parse", "main"])
            .current_dir(&bare)
            .output()
            .expect("git should run");
        assert!(remote.status.success(), "the bare remote still has main");
        assert_eq!(
            String::from_utf8_lossy(&remote.stdout).trim(),
            base,
            "the move must never push"
        );

        let read = overview(&root).await.expect("the overview reads");
        let work = read
            .branches
            .iter()
            .find(|branch| branch.name == "work")
            .expect("work is listed");
        assert_eq!((work.ahead, work.behind), (0, 0));
        assert_eq!(read.main, Some(short(&root, "work")));
    }

    #[tokio::test]
    async fn move_main_refuses_a_non_fast_forward() {
        let (_dir, root) = repository();
        let stale = rev(&root, "main");
        git_in(&root, &["checkout", "-q", "main"]);
        write(&root, "README.md", "# project\n\nmore\n");
        git_in(&root, &["add", "-A"]);
        git_in(&root, &["commit", "-qm", "main moves on"]);
        git_in(&root, &["checkout", "-q", "work"]);
        git_in(&root, &["branch", "stale", &stale]);
        let main_before = rev(&root, "main");

        let error = move_main(&root, "stale")
            .await
            .expect_err("a rewind is not a fast-forward");
        assert!(error.contains("fast-forward"), "{error}");
        assert_eq!(rev(&root, "main"), main_before, "main must not move");

        // `work` predates the new main commit too, so it is refused as well.
        let error = move_main(&root, "work")
            .await
            .expect_err("work is behind main now");
        assert!(error.contains("fast-forward"), "{error}");
        assert_eq!(rev(&root, "main"), main_before);
    }

    #[tokio::test]
    async fn move_main_refuses_a_dirty_checked_out_main() {
        let (_dir, root) = repository();
        let main_before = rev(&root, "main");
        git_in(&root, &["checkout", "-q", "main"]);
        write(&root, "README.md", "# project\n\nuncommitted\n");

        let error = move_main(&root, "work")
            .await
            .expect_err("a dirty checkout blocks the move");
        assert!(error.contains("uncommitted changes"), "{error}");
        assert_eq!(rev(&root, "main"), main_before);

        // Clean is still refused: the worktree's index would go stale.
        git_in(&root, &["checkout", "-q", "--", "README.md"]);
        let error = move_main(&root, "work")
            .await
            .expect_err("a checked-out main blocks the move");
        assert!(error.contains("checked out"), "{error}");
        assert_eq!(rev(&root, "main"), main_before);
    }

    #[tokio::test]
    async fn move_main_refuses_a_main_checked_out_in_a_linked_worktree() {
        let (dir, root) = repository();
        let linked = dir.path().join("linked");
        git_in(
            &root,
            &["worktree", "add", "-q", linked.to_str().expect("utf-8 path"), "main"],
        );
        let read = overview(&root).await.expect("the overview reads");
        assert!(
            read.main_worktree.is_some(),
            "the linked worktree holds main"
        );
        assert!(!read.main_dirty, "the linked worktree is clean");

        let main_before = rev(&root, "main");
        let error = move_main(&root, "work")
            .await
            .expect_err("a linked checkout blocks the move");
        assert!(error.contains("checked out"), "{error}");
        assert_eq!(rev(&root, "main"), main_before);
    }

    #[tokio::test]
    async fn move_main_refuses_an_unknown_or_current_branch() {
        let (_dir, root) = repository();
        assert!(move_main(&root, " ")
            .await
            .expect_err("a blank name is refused")
            .contains("no branch"));
        assert!(move_main(&root, "no-such-branch")
            .await
            .expect_err("an unknown branch is refused")
            .contains("no local branch"));
        assert!(move_main(&root, "main")
            .await
            .expect_err("main onto itself is refused")
            .contains("already points"));
    }
}
