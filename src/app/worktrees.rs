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
//! New workers land in POOL CHECKOUTS instead of throw-away folders: fixed
//! `<main_root>/.worktrees/pool-<n>` folders that are REUSED across workers
//! so `target/`, `node_modules/` and the per-checkout code graph stay warm
//! (a fresh folder recompiles the workspace from scratch on every dispatch;
//! Cargo keys its local crates by source path). [`provision_pool_checkout`]
//! hands out the lowest free pool — registered, on a detached HEAD, clean
//! and held by no live record — and [`release_pool_checkout`] detaches a
//! finished worker's checkout instead of deleting it: the folder and its
//! caches survive, and the branch becomes deletable. The `agent-<id8>`
//! functions stay for worker records dispatched before pool checkouts
//! existed; their recorded `worktree` paths keep today's semantics.
//!
//! Every checkout Mnemo prepares also writes the offscreen browser's
//! screenshot folder into the repository's SHARED exclude file
//! ([`ensure_screenshot_exclude`]): a worker's PNGs are untracked, and git
//! must not report them as dirty checkout state. A project's `.gitignore` is
//! never touched, and an exclude only covers untracked paths — a tracked
//! screenshot stays tracked.
//!
//! All functions run blocking git subprocesses on the blocking thread
//! pool (see `git_ops` for why). Unlike the rest of `git_ops`, the tests
//! here use REAL git in tempdir repos — worktrees are filesystem-level
//! (a linked worktree is a real directory with a real checkout + `.git`
//! file), which the in-memory `MockGit` cannot represent.

use std::io::Write;
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
    ignore_screenshots(&worktree);
    Ok(worktree)
}

// ===========================================================================
// Shared Git exclude — screenshots never dirty a checkout (see the module doc)
// ===========================================================================

/// The line Mnemo appends to the repository's shared exclude file: the
/// offscreen browser writes its PNGs into `.coding/browser/screenshots/` (the
/// app crate's `SCREENSHOT_DIR`). Untracked screenshots would otherwise make a
/// worker's checkout dirty and refuse its check run, merge and release gates.
const SCREENSHOT_EXCLUDE_LINE: &str = "/.coding/browser/screenshots/";

/// Make the repository's SHARED exclude file ignore the offscreen browser's
/// screenshot folder, for `checkout_or_root` (a main checkout or any of its
/// linked worktrees).
///
/// The exclude lives at `<git common dir>/info/exclude`, one file for the
/// whole repository, so every pool checkout and legacy worktree picks it up
/// and no user project has to commit a `.gitignore` line. Idempotent: the
/// line is only appended when no existing line already equals it after
/// trimming, and it never glues itself onto a file without a trailing
/// newline. A tracked screenshot stays tracked — an exclude only covers
/// untracked paths.
pub(crate) fn ensure_screenshot_exclude(checkout_or_root: &Path) -> Result<(), String> {
    let common = git_raw(checkout_or_root, &["rev-parse", "--git-common-dir"])?;
    let common = common.trim();
    if common.is_empty() {
        return Err(format!(
            "git reported no common directory for {}",
            checkout_or_root.display()
        ));
    }
    let common = PathBuf::from(common);
    // A relative answer is relative to the directory git ran in.
    let common = if common.is_absolute() {
        common
    } else {
        checkout_or_root.join(common)
    };
    let info = common.join("info");
    std::fs::create_dir_all(&info)
        .map_err(|error| format!("cannot create {}: {error}", info.display()))?;
    let exclude = info.join("exclude");
    let existing = match std::fs::read(&exclude) {
        Ok(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("cannot read {}: {error}", exclude.display())),
    };
    if existing.as_deref().is_some_and(|text| {
        text.lines()
            .any(|line| line.trim() == SCREENSHOT_EXCLUDE_LINE)
    }) {
        return Ok(());
    }
    let mut append = String::new();
    // Keep the file's last line intact when it has no trailing newline.
    if existing
        .as_deref()
        .is_some_and(|text| !text.is_empty() && !text.ends_with('\n'))
    {
        append.push('\n');
    }
    append.push_str(SCREENSHOT_EXCLUDE_LINE);
    append.push('\n');
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&exclude)
        .map_err(|error| format!("cannot open {}: {error}", exclude.display()))?;
    file.write_all(append.as_bytes())
        .map_err(|error| format!("cannot write {}: {error}", exclude.display()))?;
    Ok(())
}

/// Best-effort wrapper for checkout provisioning: the exclude is a
/// convenience for the worker's clean-checkout gates, never a reason to refuse
/// a dispatch — a failure is logged and swallowed.
fn ignore_screenshots(checkout_or_root: &Path) {
    if let Err(error) = ensure_screenshot_exclude(checkout_or_root) {
        eprintln!(
            "mnemo: could not make Git ignore the browser screenshots in {}: {error}",
            checkout_or_root.display()
        );
    }
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

// ===========================================================================
// Pool checkouts — fixed, reused folders (see the module doc)
// ===========================================================================

/// The pool checkout directory name for pool index `n`: `pool-<n>` —
/// decimal, no leading zeros, starting at 1. A pool keeps the SAME absolute
/// path across workers, which is what makes its build caches and code graph
/// reusable.
pub fn pool_checkout_name(n: u32) -> String {
    format!("pool-{n}")
}

/// The pool index encoded in a checkout path: `Some(n)` only for a direct
/// child of a `.worktrees` directory named exactly `pool-<n>` (decimal, no
/// leading zero, `n` ≥ 1). Everything else — `pool-0`, `pool-01`, `pool-x`,
/// `poolx-1`, a `pool-1` under any other parent — is `None` and never
/// treated as a pool.
pub fn pool_index(path: &Path) -> Option<u32> {
    let name = path.file_name()?.to_str()?;
    let digits = name.strip_prefix("pool-")?;
    if digits.is_empty()
        || digits.starts_with('0')
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let parent = path.parent()?.file_name()?.to_str()?;
    if parent != ".worktrees" {
        return None;
    }
    digits.parse().ok()
}

/// Whether `path` names a pool checkout — `pool-<n>` directly under
/// `.worktrees`. A record's `worktree` value decides its checkout kind: a
/// pool path takes the reuse-and-release path here, a legacy
/// `.worktrees/agent-<id8>` path keeps today's remove-and-keep-branch
/// semantics.
pub fn is_pool_checkout(path: &Path) -> bool {
    pool_index(path).is_some()
}

/// Normalize a checkout path for comparison: canonicalize its PARENT and
/// re-join the file name, so a symlink or junction at the final component is
/// never followed (mirrors the team module's legacy-checkout
/// `normalized_checkout_path`). The lib crate does not depend on `dunce`,
/// so this uses [`std::fs::canonicalize`] on BOTH sides of every
/// comparison: Windows' verbatim `\\?\` prefix cancels out, and a path whose
/// parent cannot be resolved keeps its literal spelling and compares
/// unequal to every resolved path (a missing path counts as different).
fn normalized_checkout_path(path: &Path) -> PathBuf {
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => std::fs::canonicalize(parent)
            .unwrap_or_else(|_| parent.to_path_buf())
            .join(name),
        _ => path.to_path_buf(),
    }
}

/// Compare two checkout paths as the team module does for legacy checkouts:
/// normalized, and case-insensitive on Windows (git may print a path with
/// different case or separators than the filesystem reports). Never follows
/// a link at the final component.
fn same_checkout_path(left: &Path, right: &Path) -> bool {
    let left = normalized_checkout_path(left);
    let right = normalized_checkout_path(right);
    #[cfg(windows)]
    {
        left.to_string_lossy()
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_lowercase()
            == right
                .to_string_lossy()
                .replace('/', "\\")
                .trim_end_matches('\\')
                .to_lowercase()
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

/// Whether `metadata` describes a symlink or junction — a pool path that is
/// a link is refused rather than followed (mirrors the team module's legacy
/// checkout guard). Windows junctions do not always report `is_symlink()`,
/// so the reparse-point attribute is checked as well.
#[cfg(windows)]
pub fn is_link_or_junction(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    metadata.file_type().is_symlink() || metadata.file_attributes() & 0x400 != 0
}

/// Whether `metadata` describes a symlink — a pool path that is a link is
/// refused rather than followed.
#[cfg(not(windows))]
pub fn is_link_or_junction(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

/// Every checkout git has registered for the main repo — one
/// `git worktree list --porcelain -z` run, NUL-delimited (paths may contain
/// newlines, so the line-oriented form is not safe).
fn registered_worktrees(main_root: &Path) -> Result<Vec<PathBuf>, String> {
    let list = git_raw(main_root, &["worktree", "list", "--porcelain", "-z"])?;
    Ok(list
        .split('\0')
        .filter_map(|entry| entry.strip_prefix("worktree "))
        .map(PathBuf::from)
        .collect())
}

/// A held pool claim; dropping it releases the cross-instance reservation.
struct PoolClaim {
    path: PathBuf,
}

impl Drop for PoolClaim {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Whether a claim's recorded process is still alive. An unreadable claim is
/// an error (and therefore skipped); an absent or malformed pid has no live
/// owner to protect and can become stale by age.
fn claim_owner_alive(claim: &Path) -> Result<bool, String> {
    let contents = std::fs::read_to_string(claim)
        .map_err(|error| format!("cannot read {}: {error}", claim.display()))?;
    let Some(pid) = contents
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|pid| *pid != 0)
    else {
        return Ok(false);
    };
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        // SAFETY: fixed query flags, no handle inheritance; close every handle.
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            // Only "no such process" proves the owner is gone; any other
            // refusal (access denied, ...) fails closed and counts as live.
            // SAFETY: reads this thread's last-error value, set by OpenProcess.
            let error = unsafe { windows_sys::Win32::Foundation::GetLastError() };
            return Ok(error != windows_sys::Win32::Foundation::ERROR_INVALID_PARAMETER);
        }
        let mut exit_code = 0;
        // SAFETY: the handle is valid and outlives this query.
        let probed = unsafe { GetExitCodeProcess(handle, &mut exit_code) };
        // SAFETY: the handle is no longer used.
        unsafe { CloseHandle(handle) };
        // A failed query also fails closed; only a real exit code means gone.
        Ok(probed == 0 || exit_code == STILL_ACTIVE as u32)
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("ps")
            .arg("-p")
            .arg(pid.to_string())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .map_err(|error| format!("cannot probe claim owner {pid}: {error}"))
    }
}

/// Claim a free pool across Mnemo instances. Only a claim older than ten
/// minutes whose recorded owner is no longer alive is stale; a fresh claim,
/// a live owner, or an unreadable/future-dated claim skips the pool. A stale
/// claim is removed and creation retried exactly once.
fn acquire_pool_claim(pool: &Path) -> Result<Option<PoolClaim>, String> {
    let claim = pool.with_file_name(format!(
        "{}.claim",
        pool.file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| format!("{} has no pool file name", pool.display()))?
    ));
    for attempt in 0..2 {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&claim)
        {
            Ok(mut file) => {
                if let Err(error) = writeln!(file, "{}", std::process::id()) {
                    drop(file);
                    let _ = std::fs::remove_file(&claim);
                    return Err(format!("cannot write {}: {error}", claim.display()));
                }
                return Ok(Some(PoolClaim { path: claim }));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let stale = if attempt == 0 {
                    let modified = std::fs::metadata(&claim)
                        .and_then(|metadata| metadata.modified())
                        .map_err(|error| format!("cannot inspect {}: {error}", claim.display()))?;
                    let aged = modified
                        .elapsed()
                        .map(|age| age > std::time::Duration::from_secs(600))
                        .unwrap_or(false);
                    aged && !claim_owner_alive(&claim)?
                } else {
                    false
                };
                if !stale {
                    return Ok(None);
                }
                std::fs::remove_file(&claim)
                    .map_err(|error| format!("cannot remove stale {}: {error}", claim.display()))?;
            }
            Err(error) => {
                return Err(format!("cannot create {}: {error}", claim.display()));
            }
        }
    }
    Ok(None)
}

/// Every `.worktrees` entry named `pool-<n>`, ascending by index, with
/// whether it is a directory. An entry that merely exists but is not a
/// usable worktree still RESERVES its index — the next pool is `max+1`, so
/// no existing name is ever handed out again (see
/// [`provision_pool_checkout`]).
fn existing_pool_entries(main_root: &Path) -> Result<Vec<(u32, PathBuf, bool)>, String> {
    let worktrees = worktrees_dir(main_root);
    let entries = match std::fs::read_dir(&worktrees) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("cannot list {}: {error}", worktrees.display())),
    };
    let mut pools = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("cannot list {}: {error}", worktrees.display()))?;
        let path = entry.path();
        if let Some(index) = pool_index(&path) {
            let is_dir = entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
            pools.push((index, path, is_dir));
        }
    }
    pools.sort_by_key(|(index, _, _)| *index);
    Ok(pools)
}

/// The free-pool test [`provision_pool_checkout`] and
/// [`trim_pool_checkouts`] share: the checkout is a registered pool worktree,
/// its HEAD is detached, and it carries no uncommitted or untracked work.
/// Ignored build caches — the point of a pool — do not count as work. `Err`
/// names the first precondition that failed.
fn pool_is_free(main_root: &Path, pool: &Path) -> Result<(), String> {
    ensure_registered_pool_checkout(main_root, pool)?;
    let head = git_raw(pool, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    if head.trim() != "HEAD" {
        return Err(format!("pool is on branch {}", head.trim()));
    }
    let dirty = git_raw(pool, &["status", "--porcelain", "-uall"])?;
    if !dirty.trim().is_empty() {
        return Err("pool has uncommitted work".to_string());
    }
    Ok(())
}

/// Provision a POOL checkout for a new worker on `branch`, forked from
/// `base_ref` (the lead branch tip): the lowest-indexed
/// `<main_root>/.worktrees/pool-<n>` that is FREE — not `held` by any live
/// record, registered with git, on a detached HEAD and without uncommitted
/// work — is reused; when none is free, a new `.worktrees/pool-<max+1>` is
/// created. Returns the checkout path on success.
///
/// Reuse re-points the pool's HEAD at `base_ref`, sweeps the previous
/// worker's ignored leftovers and creates `branch` there. The sweep is
/// `git clean -ffdx -e /target/ -e node_modules/ -e /.coding/codegraph.db*`:
/// the workspace's own `target/` (anchored at the checkout root),
/// `node_modules/` at ANY depth and the code-graph database plus its
/// sidecars survive; every other ignored or untracked leftover
/// (`.coding/tmp/`, logs, nested `target/` folders) is removed. A pool that
/// fails any of the reuse commands is left detached, reported on stderr and
/// skipped — the next free pool, or a new one, is used instead.
///
/// Runs the blocking git subprocesses on the blocking thread pool (see the
/// module doc). Owns its arguments so the closure is `'static + Send`.
pub async fn provision_pool_checkout(
    main_root: PathBuf,
    branch: String,
    base_ref: String,
    held: Vec<PathBuf>,
) -> Result<PathBuf, String> {
    tokio::task::spawn_blocking(move || {
        provision_pool_checkout_impl(&main_root, &branch, &base_ref, &held)
    })
    .await
    .map_err(|e| format!("pool checkout provision task failed: {e}"))?
}

/// The synchronous core of [`provision_pool_checkout`].
fn provision_pool_checkout_impl(
    main_root: &Path,
    branch: &str,
    base_ref: &str,
    held: &[PathBuf],
) -> Result<PathBuf, String> {
    if base_ref.trim().is_empty() {
        return Err("worktree base ref is empty".to_string());
    }
    // Fail before touching any pool when the base ref is unknown — the
    // caller gets a clear base-ref error, not a half-provisioned pool
    // (mirrors `provision_worktree_impl`).
    git_raw(
        main_root,
        &["rev-parse", "--verify", &format!("{base_ref}^{{commit}}")],
    )?;
    // Drop registrations for worktree directories that are already gone —
    // stale metadata would make `git worktree add` refuse to create the
    // next pool folder (same hygiene as `remove_worktree_only_impl`).
    let _ = git_raw(main_root, &["worktree", "prune"]);
    let pools = existing_pool_entries(main_root)?;
    for (_, pool, is_dir) in &pools {
        if let Err(error) = ensure_pool_checkout(main_root, pool) {
            eprintln!(
                "mnemo: pool checkout {} failed its safety guard: {error}; skipped",
                pool.display()
            );
            continue;
        }
        if !is_dir {
            eprintln!("mnemo: pool checkout {} is not a directory; skipped", pool.display());
            continue;
        }
        if held.iter().any(|held_path| same_checkout_path(held_path, pool)) {
            continue; // held by a live worker record — the expected case
        }
        if let Err(error) = pool_is_free(main_root, pool) {
            eprintln!(
                "mnemo: pool checkout {} is not free: {error}; skipped",
                pool.display()
            );
            continue;
        }
        let _claim = match acquire_pool_claim(pool) {
            Ok(Some(claim)) => claim,
            Ok(None) => {
                eprintln!(
                    "mnemo: pool checkout {} has a live claim; skipped",
                    pool.display()
                );
                continue;
            }
            Err(error) => {
                eprintln!(
                    "mnemo: could not claim pool checkout {}: {error}; skipped",
                    pool.display()
                );
                continue;
            }
        };
        // Another instance may have changed the checkout before our claim.
        if let Err(error) = pool_is_free(main_root, pool) {
            eprintln!(
                "mnemo: pool checkout {} changed after claiming: {error}; skipped",
                pool.display()
            );
            continue;
        }
        if let Err(error) = reuse_pool_checkout(main_root, pool, branch, base_ref) {
            eprintln!(
                "mnemo: reusing pool checkout {} failed: {error}; skipped",
                pool.display()
            );
            continue;
        }
        return Ok(pool.to_path_buf());
    }
    // Existing folders and held paths both reserve their indices, even when
    // a held folder is temporarily missing, so max+1 can never equal a hold.
    let highest = pools
        .iter()
        .map(|(index, _, _)| *index)
        .chain(held.iter().filter_map(|path| pool_index(path)))
        .max();
    let mut next = match highest {
        Some(index) => index
            .checked_add(1)
            .ok_or_else(|| "no pool checkout indices remain".to_string())?,
        None => 1,
    };
    let mut last_error = None;
    for _ in 0..5 {
        let name = pool_checkout_name(next);
        let path = worktrees_dir(main_root).join(&name);
        match provision_worktree_impl(main_root, &name, branch, base_ref) {
            Ok(checkout) => return Ok(checkout),
            Err(error) => {
                if std::fs::symlink_metadata(&path).is_err() {
                    return Err(error);
                }
                eprintln!(
                    "mnemo: pool checkout {} appeared during provisioning; trying the next index",
                    path.display()
                );
                last_error = Some(error);
                next = next
                    .checked_add(1)
                    .ok_or_else(|| "no pool checkout indices remain".to_string())?;
            }
        }
    }
    Err(format!(
        "could not allocate a new pool checkout after 5 attempts: {}",
        last_error.unwrap_or_else(|| "pool paths kept appearing".to_string())
    ))
}

/// Remove every FREE pool checkout whose index is above `keep` — the current
/// worker slot count — highest index first, and return the removed paths. The
/// cap is SOFT: dispatch never depends on it, so a new pool above `keep` may
/// still be created while the lower pools are held.
///
/// A pool is removed only when ALL of these hold: its index is above `keep`;
/// no path in `held` is it (the checkouts of unmerged worker records);
/// [`pool_is_free`] passes (registered with git, detached HEAD, no uncommitted
/// or untracked work); and the cross-instance claim [`acquire_pool_claim`] is
/// obtained and the free test still passes with the claim held. Anything else
/// is reported on stderr and left untouched: uncommitted work is never
/// discarded, another instance's pool is never raced, and a pool at or below
/// `keep` stays as a warm cache.
///
/// Removal is `git worktree remove --force` from `main_root` (the free test
/// already proved there is no tracked or untracked work), then — when the
/// folder survives with its ignored caches (Windows: an npm workspace junction
/// outlives git's removal) — `std::fs::remove_dir_all` and a `worktree prune`.
/// That removal never follows a symlink or junction (Rust removes the link
/// itself), so a link's target outside the pool survives. One failed removal is
/// reported and skipped; only a failure to LIST the pools is an error.
///
/// Runs the blocking git subprocesses on the blocking thread pool (see the
/// module doc). Owns its arguments so the closure is `'static + Send`.
pub async fn trim_pool_checkouts(
    main_root: PathBuf,
    keep: u32,
    held: Vec<PathBuf>,
) -> Result<Vec<PathBuf>, String> {
    tokio::task::spawn_blocking(move || trim_pool_checkouts_impl(&main_root, keep, &held))
        .await
        .map_err(|e| format!("pool checkout trim task failed: {e}"))?
}

/// The synchronous core of [`trim_pool_checkouts`].
fn trim_pool_checkouts_impl(
    main_root: &Path,
    keep: u32,
    held: &[PathBuf],
) -> Result<Vec<PathBuf>, String> {
    let mut pools = existing_pool_entries(main_root)?;
    // Highest index first, so the cap lands as close to `keep` as the pools
    // allow even when one of them cannot go.
    pools.retain(|(index, _, _)| *index > keep);
    pools.sort_by_key(|(index, _, _)| std::cmp::Reverse(*index));
    let mut removed = Vec::new();
    let mut attempted = false;
    for (_, pool, is_dir) in &pools {
        if held
            .iter()
            .any(|held_path| same_checkout_path(held_path, pool))
        {
            eprintln!(
                "mnemo: idle pool checkout {} stays: a worker record holds it",
                pool.display()
            );
            continue;
        }
        if !is_dir {
            eprintln!(
                "mnemo: idle pool checkout {} stays: it is not a directory",
                pool.display()
            );
            continue;
        }
        if let Err(error) = pool_is_free(main_root, pool) {
            eprintln!(
                "mnemo: idle pool checkout {} stays: it is not free: {error}",
                pool.display()
            );
            continue;
        }
        let _claim = match acquire_pool_claim(pool) {
            Ok(Some(claim)) => claim,
            Ok(None) => {
                eprintln!(
                    "mnemo: idle pool checkout {} stays: it has a live claim",
                    pool.display()
                );
                continue;
            }
            Err(error) => {
                eprintln!(
                    "mnemo: idle pool checkout {} stays: it cannot be claimed: {error}",
                    pool.display()
                );
                continue;
            }
        };
        // Another instance may have changed the checkout before our claim.
        if let Err(error) = pool_is_free(main_root, pool) {
            eprintln!(
                "mnemo: idle pool checkout {} stays: it changed after claiming: {error}",
                pool.display()
            );
            continue;
        }
        attempted = true;
        match remove_pool_checkout(main_root, pool) {
            Ok(()) => removed.push(pool.clone()),
            Err(error) => eprintln!(
                "mnemo: could not remove idle pool checkout {}: {error}; skipped",
                pool.display()
            ),
        }
    }
    if attempted {
        let _ = git_raw(main_root, &["worktree", "prune"]);
    }
    Ok(removed)
}

/// Remove one claimed, free pool checkout: `git worktree remove --force` from
/// `main_root`, then `std::fs::remove_dir_all` for a folder git leaves behind
/// because of its ignored build caches (Windows: an npm workspace junction
/// outlives the removal). `remove_dir_all` never follows a symlink or junction
/// — it removes the link itself, so a link's target outside the pool survives.
fn remove_pool_checkout(main_root: &Path, pool: &Path) -> Result<(), String> {
    let path = pool.to_string_lossy();
    git_raw(main_root, &["worktree", "remove", "--force", path.as_ref()])?;
    match std::fs::symlink_metadata(pool) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    }
    std::fs::remove_dir_all(pool)
        .map_err(|error| format!("git deregistered it, but the folder remains: {error}"))
}

/// Reuse one free pool for a new worker: re-point its detached HEAD at
/// `base_ref`, sweep the previous worker's ignored leftovers (keeping the
/// warm caches) and create `branch` there. The caller has already checked
/// the pool is free.
fn reuse_pool_checkout(
    main_root: &Path,
    pool: &Path,
    branch: &str,
    base_ref: &str,
) -> Result<(), String> {
    ensure_registered_pool_checkout(main_root, pool)?;
    ignore_screenshots(pool);
    git_raw(pool, &["switch", "--detach", base_ref])?;
    git_raw(
        pool,
        &[
            "clean",
            "-ffdx",
            "-e",
            "/target/",
            "-e",
            "node_modules/",
            "-e",
            "/.coding/codegraph.db*",
        ],
    )?;
    git_raw(pool, &["switch", "-c", branch])?;
    Ok(())
}

/// Provision a Mnemo2 team worker's POOL checkout: a reused (or new)
/// `<main_root>/.worktrees/pool-<n>` folder on a fresh `wt/agent-<id8>`
/// branch forked from `base_ref` (the lead branch tip). Returns
/// `(checkout, branch)`.
///
/// `held` lists the pool paths that live worker records still hold — those
/// pools are never handed out again until released. Runs the blocking git
/// subprocesses on the blocking thread pool (see the module doc). Owns its
/// arguments so the closure is `'static + Send`.
pub async fn provision_agent_checkout(
    main_root: PathBuf,
    agent_id: String,
    base_ref: String,
    held: Vec<PathBuf>,
) -> Result<(PathBuf, String), String> {
    let branch = agent_branch(&agent_id);
    let checkout = provision_pool_checkout(main_root, branch.clone(), base_ref, held).await?;
    Ok((checkout, branch))
}

/// Guard shared by the pool release paths: `checkout` must be one of THIS
/// main root's pool folders — a `pool-<n>` directory directly under the
/// canonical `<main_root>/.worktrees`, never a symlink or junction, never
/// anything else. Everything is refused BEFORE any git command runs.
fn ensure_pool_checkout(main_root: &Path, checkout: &Path) -> Result<(), String> {
    if !is_pool_checkout(checkout) {
        return Err(format!(
            "refusing to operate on {}: not a pool checkout",
            checkout.display()
        ));
    }
    let metadata = std::fs::symlink_metadata(checkout)
        .map_err(|error| format!("cannot inspect {}: {error}", checkout.display()))?;
    if is_link_or_junction(&metadata) {
        return Err(format!(
            "refusing to operate on {}: it is a symlink or junction",
            checkout.display()
        ));
    }
    if !metadata.is_dir() {
        return Err(format!(
            "refusing to operate on {}: it is not a directory",
            checkout.display()
        ));
    }
    let worktrees = worktrees_dir(main_root);
    let parent = checkout
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", checkout.display()))?;
    if !same_checkout_path(parent, &worktrees) {
        return Err(format!(
            "refusing to operate on {}: it is not a direct child of {}",
            checkout.display(),
            worktrees.display()
        ));
    }
    Ok(())
}

/// Filesystem, registration, and self-resolution guards for a pool. Git must
/// register this exact path, and `rev-parse --show-toplevel` inside it must
/// canonicalize back to itself rather than to the main checkout or another
/// worktree. The registry check always precedes the pool-local Git command.
fn ensure_registered_pool_checkout(main_root: &Path, checkout: &Path) -> Result<(), String> {
    ensure_pool_checkout(main_root, checkout)?;
    let registered = registered_worktrees(main_root)?;
    if !registered
        .iter()
        .any(|known| same_checkout_path(known, checkout))
    {
        return Err(format!(
            "refusing to operate on {}: it is not a registered worktree",
            checkout.display()
        ));
    }
    let top = git_raw(checkout, &["rev-parse", "--show-toplevel"])?;
    let canonical_top = std::fs::canonicalize(top.trim())
        .map_err(|error| format!("cannot resolve the checkout top level: {error}"))?;
    let canonical_checkout = std::fs::canonicalize(checkout)
        .map_err(|error| format!("cannot resolve {}: {error}", checkout.display()))?;
    if !same_checkout_path(&canonical_top, &canonical_checkout) {
        return Err(format!(
            "refusing to operate on {}: its Git top level is not the pool itself",
            checkout.display()
        ));
    }
    Ok(())
}

/// Run the complete pool registration/self-resolution guard on the blocking
/// thread pool before an asynchronous caller issues any other pool Git command.
pub async fn ensure_pool_checkout_registered(
    main_root: PathBuf,
    checkout: PathBuf,
) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        ensure_registered_pool_checkout(&main_root, &checkout)
    })
    .await
    .map_err(|error| format!("pool checkout guard task failed: {error}"))?
}

/// Fast-forward a worker's pool `checkout` to `lead_ref` (the lead branch).
///
/// Returns `Ok(true)` after `git merge --ff-only <lead_ref>` ran in the
/// checkout. Returns `Ok(false)` without touching anything when the checkout
/// has uncommitted or untracked changes, when `lead_ref` is HEAD itself, or
/// when `lead_ref` does not descend from HEAD. It never rebases, resets or
/// merges non-fast-forward. The pool registration guard runs first; a git
/// failure is an `Err` and leaves the checkout as git left it.
pub async fn fast_forward_checkout(
    main_root: PathBuf,
    checkout: PathBuf,
    lead_ref: String,
) -> Result<bool, String> {
    tokio::task::spawn_blocking(move || {
        ensure_registered_pool_checkout(&main_root, &checkout)?;
        if !git_raw(&checkout, &["status", "--porcelain"])?.trim().is_empty() {
            return Ok(false);
        }
        let behind = git_raw(
            &checkout,
            &["rev-list", "--count", &format!("HEAD..{lead_ref}")],
        )?;
        let ahead = git_raw(
            &checkout,
            &["rev-list", "--count", &format!("{lead_ref}..HEAD")],
        )?;
        if behind.trim() == "0" || ahead.trim() != "0" {
            return Ok(false);
        }
        git_raw(&checkout, &["merge", "--ff-only", &lead_ref])?;
        Ok(true)
    })
    .await
    .map_err(|error| format!("fast-forward task failed: {error}"))?
}

/// Merge `branch` into a worker's pool `checkout` with a merge commit
/// (`git merge --no-ff --no-edit -m <message> <branch>`), for
/// `team_integrate`.
///
/// `Ok(Ok(tip))` gives the new HEAD. `Ok(Err(paths))` lists the conflicting
/// files after `git merge --abort` restored the checkout. `Err` covers a
/// dirty checkout, a branch the checkout already contains, a failed pool
/// guard, and any other git failure (a merge that stopped without conflicts
/// is aborted first). The pool registration guard runs first.
pub async fn merge_branch_into_checkout(
    main_root: PathBuf,
    checkout: PathBuf,
    branch: String,
    message: String,
) -> Result<Result<String, Vec<String>>, String> {
    tokio::task::spawn_blocking(move || {
        ensure_registered_pool_checkout(&main_root, &checkout)?;
        if !git_raw(&checkout, &["status", "--porcelain"])?.trim().is_empty() {
            return Err("the worker's checkout has uncommitted changes".to_string());
        }
        let missing = git_raw(
            &checkout,
            &["rev-list", "--count", &format!("HEAD..{branch}")],
        )?;
        if missing.trim() == "0" {
            return Err(format!("the worker's branch already contains {branch}"));
        }
        match git_raw(
            &checkout,
            &["merge", "--no-ff", "--no-edit", "-m", &message, &branch],
        ) {
            Ok(_) => Ok(Ok(git_raw(&checkout, &["rev-parse", "HEAD"])?
                .trim()
                .to_string())),
            Err(error) => {
                let conflicted: Vec<String> =
                    git_raw(&checkout, &["diff", "--name-only", "--diff-filter=U"])
                        .unwrap_or_default()
                        .lines()
                        .map(str::trim)
                        .filter(|path| !path.is_empty())
                        .map(str::to_string)
                        .collect();
                let aborted = git_raw(&checkout, &["merge", "--abort"]);
                if conflicted.is_empty() {
                    return Err(error);
                }
                aborted?;
                Ok(Err(conflicted))
            }
        }
    })
    .await
    .map_err(|error| format!("merge task failed: {error}"))?
}

/// How many commits of `lead_ref` the worker `branch` does not contain
/// (`git rev-list --count <branch>..<lead_ref>`, run in `main_root`).
pub async fn commits_missing(
    main_root: PathBuf,
    branch: String,
    lead_ref: String,
) -> Result<u64, String> {
    tokio::task::spawn_blocking(move || {
        let count = git_raw(
            &main_root,
            &["rev-list", "--count", &format!("{branch}..{lead_ref}")],
        )?;
        count
            .trim()
            .parse::<u64>()
            .map_err(|error| format!("unreadable commit count '{}': {error}", count.trim()))
    })
    .await
    .map_err(|error| format!("commit count task failed: {error}"))?
}

/// Release a finished worker's pool checkout back to the pool WITHOUT
/// deleting it: the folder's `target/`, `node_modules/` and code graph are
/// the warm cache the next worker builds on. The checkout must be on the
/// worker's `branch` and have no uncommitted work; it is then switched to a
/// detached HEAD, which frees the branch for `git branch -d`. A clean checkout
/// whose HEAD is already detached is already released and returns `Ok(())`
/// unchanged, so retrying after a lost ledger save is safe. Detached but dirty
/// still refuses. Every refusal leaves the checkout and branch as they were.
///
/// Runs the blocking git subprocesses on the blocking thread pool (see the
/// module doc). Owns its arguments so the closure is `'static + Send`.
pub async fn release_pool_checkout(
    main_root: PathBuf,
    checkout: PathBuf,
    branch: String,
) -> Result<(), String> {
    tokio::task::spawn_blocking(move || release_pool_checkout_impl(&main_root, &checkout, &branch))
        .await
        .map_err(|e| format!("pool checkout release task failed: {e}"))?
}

/// The synchronous core of [`release_pool_checkout`].
fn release_pool_checkout_impl(
    main_root: &Path,
    checkout: &Path,
    branch: &str,
) -> Result<(), String> {
    ensure_registered_pool_checkout(main_root, checkout)?;
    let current = git_raw(checkout, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let current = current.trim();
    let already_detached = current == "HEAD";
    if !already_detached && current != branch {
        return Err(format!(
            "checkout is on an unexpected branch; retained (expected {branch}, found {current})"
        ));
    }
    let dirty = git_raw(checkout, &["status", "--porcelain", "-uall"])?;
    if !dirty.trim().is_empty() {
        return Err(format!(
            "uncommitted work is retained in the pool checkout {}",
            checkout.display()
        ));
    }
    if already_detached {
        return Ok(());
    }
    git_raw(checkout, &["switch", "--detach"]).map(|_| ())
}

/// Discard a confirmed-dead worker's uncommitted work, then release its pool
/// checkout. Unlike [`release_pool_checkout`] this resets the checkout
/// (`git reset --hard` + `git clean -ffd`, untracked non-ignored files only
/// — no `-x`, the ignored build caches are the point of the pool), because
/// the user has already confirmed the deletion. The folder itself is never
/// removed.
///
/// Runs the blocking git subprocesses on the blocking thread pool (see the
/// module doc). Owns its arguments so the closure is `'static + Send`.
pub async fn discard_and_release_pool_checkout(
    main_root: PathBuf,
    checkout: PathBuf,
    branch: String,
) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        discard_and_release_pool_checkout_impl(&main_root, &checkout, &branch)
    })
    .await
    .map_err(|e| format!("pool checkout discard task failed: {e}"))?
}

/// The synchronous core of [`discard_and_release_pool_checkout`].
fn discard_and_release_pool_checkout_impl(
    main_root: &Path,
    checkout: &Path,
    branch: &str,
) -> Result<(), String> {
    ensure_registered_pool_checkout(main_root, checkout)?;
    let current = git_raw(checkout, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let current = current.trim();
    if current != "HEAD" && current != branch {
        return Err(format!(
            "checkout is on an unexpected branch; retained (expected {branch}, found {current})"
        ));
    }
    // A detached pool was already released before a lost ledger save. The
    // confirmed discard still resets tracked and untracked work; the final
    // release is idempotent and leaves HEAD detached.
    git_raw(checkout, &["reset", "--hard"])?;
    git_raw(checkout, &["clean", "-ffd"])?;
    release_pool_checkout_impl(main_root, checkout, branch)
}

/// Roll back only a newly provisioned, untouched Mnemo2 worker's POOL
/// checkout — the pool twin of [`rollback_agent_worktree`]: the same
/// identity, branch, clean and tip checks, but the checkout is DETACHED
/// (the folder and its warm caches survive) instead of removed, and the
/// branch is then deleted. The caller must have created this exact worker
/// ID during this dispatch, and `checkout` must be the pool that dispatch
/// provisioned.
///
/// Runs the blocking git subprocesses on the blocking thread pool (see the
/// module doc). Owns its arguments so the closure is `'static + Send`.
pub async fn rollback_agent_checkout(
    main_root: PathBuf,
    agent_id: String,
    checkout: PathBuf,
    base_tip: String,
) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        rollback_agent_checkout_impl(&main_root, &agent_id, &checkout, &base_tip)
    })
    .await
    .map_err(|error| format!("pool rollback task failed: {error}"))?
}

/// The synchronous core of [`rollback_agent_checkout`].
fn rollback_agent_checkout_impl(
    main_root: &Path,
    agent_id: &str,
    checkout: &Path,
    base_tip: &str,
) -> Result<(), String> {
    if agent_id.is_empty() || !agent_id.chars().all(|character| character.is_ascii_alphanumeric())
    {
        return Err("invalid worker identity; branch retained".to_string());
    }
    let branch = agent_branch(agent_id);
    ensure_registered_pool_checkout(main_root, checkout)?;
    if current_branch(checkout)? != branch {
        return Err("worker checkout is on an unexpected branch; branch retained".to_string());
    }
    let dirty = git_raw(checkout, &["status", "--porcelain", "-uall"])?;
    if !dirty.trim().is_empty() {
        return Err("worker checkout has uncommitted work; branch retained".to_string());
    }
    let revision = format!("refs/heads/{branch}^{{commit}}");
    let tip = git_raw(main_root, &["rev-parse", "--verify", &revision])?;
    if tip.trim() != base_tip {
        return Err("worker branch advanced after dispatch; branch retained".to_string());
    }
    // Detach instead of removing: the pool folder and its warm caches
    // survive for the next dispatch.
    git_raw(checkout, &["switch", "--detach"])?;
    let tip = git_raw(main_root, &["rev-parse", "--verify", &revision])?;
    if tip.trim() != base_tip {
        return Err("worker branch advanced during rollback; branch retained".to_string());
    }
    git_raw(
        main_root,
        &["update-ref", "-d", &format!("refs/heads/{branch}"), base_tip],
    )?;
    Ok(())
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
        // Byte-faithful checkouts: the pool tests assert exact file
        // contents (a discard's reset must restore the committed bytes,
        // and a hand-written restore must leave the checkout clean), and a
        // machine's global core.autocrlf would otherwise rewrite every
        // checkout's line endings and make git see those files as modified.
        git_raw(dir.path(), &["config", "core.autocrlf", "false"]).unwrap();
        git_raw(dir.path(), &["config", "user.email", "t@t"]).unwrap();
        git_raw(dir.path(), &["config", "user.name", "t"]).unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        git_raw(dir.path(), &["add", "-A"]).unwrap();
        git_raw(dir.path(), &["commit", "-m", "init"]).unwrap();
        dir
    }

    fn exited_process_id() -> u32 {
        #[cfg(windows)]
        let mut child = std::process::Command::new("cmd")
            .args(["/c", "exit", "0"])
            .spawn()
            .expect("spawn short-lived child");
        #[cfg(not(windows))]
        let mut child = std::process::Command::new("sh")
            .args(["-c", "exit 0"])
            .spawn()
            .expect("spawn short-lived child");
        let pid = child.id();
        child.wait().expect("reap short-lived child");
        pid
    }

    fn create_directory_link(link: &Path, target: &Path) -> Result<(), String> {
        #[cfg(windows)]
        {
            let link = link.to_string_lossy().replace('/', "\\");
            let target = target.to_string_lossy().replace('/', "\\");
            let output = std::process::Command::new("cmd")
                .args(["/c", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .map_err(|error| error.to_string())?;
            if output.status.success() {
                Ok(())
            } else {
                Err(String::from_utf8_lossy(&output.stderr).into_owned())
            }
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).map_err(|error| error.to_string())
        }
        #[cfg(not(any(windows, unix)))]
        {
            let _ = (link, target);
            Err("directory links are unsupported on this platform".to_string())
        }
    }

    /// `init_repo` plus a committed `.gitignore` that ignores the pool build
    /// caches — a released pool is only "clean" when the previous worker's
    /// warm leftovers do not dirty `git status`. The bare `target/` line
    /// also ignores NESTED targets: the reuse sweep's exclusion `/target/`
    /// is anchored at the checkout root, so a nested `target/` is an
    /// ordinary leftover and gets swept (the reuse test proves it).
    fn init_repo_ignoring_pool_caches() -> tempfile::TempDir {
        let dir = init_repo();
        std::fs::write(
            dir.path().join(".gitignore"),
            "/target/\ntarget/\nnode_modules/\n.coding/tmp/\n.coding/codegraph.db*\n*.log\n",
        )
        .unwrap();
        git_raw(dir.path(), &["add", "-A"]).unwrap();
        git_raw(dir.path(), &["commit", "-m", "ignore build caches"]).unwrap();
        dir
    }

    /// Write several files at once, creating parent directories as needed.
    fn write_all(files: &[(PathBuf, &str)]) {
        for (path, contents) in files {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, contents).unwrap();
        }
    }

    /// Write an untracked file the way the offscreen browser does: a PNG under
    /// `.coding/browser/screenshots/`.
    fn write_screenshot(checkout: &Path, name: &str) -> PathBuf {
        let path = checkout
            .join(".coding")
            .join("browser")
            .join("screenshots")
            .join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"png").unwrap();
        path
    }

    #[test]
    fn screenshot_exclude_is_added_once() {
        let repo = init_repo();
        ensure_screenshot_exclude(repo.path()).expect("first ensure");
        ensure_screenshot_exclude(repo.path()).expect("second ensure");
        let exclude = repo.path().join(".git").join("info").join("exclude");
        let text = std::fs::read_to_string(&exclude).expect("the exclude file exists");
        assert_eq!(
            text.lines()
                .filter(|line| line.trim() == SCREENSHOT_EXCLUDE_LINE)
                .count(),
            1,
            "a second call must not duplicate the line: {text:?}"
        );
        write_screenshot(repo.path(), "a.png");
        assert!(
            git_raw(
                repo.path(),
                &["check-ignore", "-q", ".coding/browser/screenshots/a.png"]
            )
            .is_ok(),
            "git ignores a screenshot once the exclude is in place"
        );
    }

    #[test]
    fn screenshot_exclude_keeps_existing_lines() {
        let repo = init_repo();
        let info = repo.path().join(".git").join("info");
        std::fs::create_dir_all(&info).unwrap();
        let exclude = info.join("exclude");
        // No trailing newline — the appended line must start its own line.
        std::fs::write(&exclude, "foo").unwrap();
        ensure_screenshot_exclude(repo.path()).expect("ensure");
        let text = std::fs::read_to_string(&exclude).unwrap();
        assert!(
            text.starts_with("foo\n"),
            "foo keeps its own line: {text:?}"
        );
        assert_eq!(
            text.lines()
                .filter(|line| line.trim() == SCREENSHOT_EXCLUDE_LINE)
                .count(),
            1,
            "{text:?}"
        );
        ensure_screenshot_exclude(repo.path()).expect("second ensure");
        let text = std::fs::read_to_string(&exclude).unwrap();
        assert!(
            text.starts_with("foo\n"),
            "foo is still intact: {text:?}"
        );
        assert_eq!(
            text.lines()
                .filter(|line| line.trim() == SCREENSHOT_EXCLUDE_LINE)
                .count(),
            1,
            "an existing line is never duplicated: {text:?}"
        );
    }

    #[test]
    fn screenshot_exclude_works_from_a_linked_worktree() {
        let repo = init_repo();
        let holder = tempfile::tempdir().unwrap();
        let linked = holder.path().join("linked");
        git_raw(
            repo.path(),
            &[
                "worktree",
                "add",
                "-b",
                "wt/linked",
                linked.to_str().expect("UTF-8 worktree path"),
                "main",
            ],
        )
        .expect("linked worktree");
        ensure_screenshot_exclude(&linked).expect("ensure from a linked worktree");
        let exclude = repo.path().join(".git").join("info").join("exclude");
        let text = std::fs::read_to_string(&exclude).expect("the main repo's exclude file");
        assert_eq!(
            text.lines()
                .filter(|line| line.trim() == SCREENSHOT_EXCLUDE_LINE)
                .count(),
            1,
            "the line lands in the main repository's .git/info/exclude: {text:?}"
        );
        write_screenshot(&linked, "b.png");
        assert!(
            git_raw(
                &linked,
                &["check-ignore", "-q", ".coding/browser/screenshots/b.png"]
            )
            .is_ok(),
            "the shared exclude covers a linked worktree"
        );
    }

    #[tokio::test]
    async fn worker_screenshots_leave_a_pool_checkout_clean() {
        // The defect: the offscreen browser's PNGs were untracked, so a
        // worker's checkout showed up dirty and every clean-checkout gate
        // (check run, merge, release) refused it until someone deleted them.
        let repo = init_repo();
        let (checkout, _branch) = provision_agent_checkout(
            repo.path().to_path_buf(),
            "abcdef123456".to_string(),
            "main".to_string(),
            vec![],
        )
        .await
        .expect("provision");
        write_screenshot(&checkout, "shot.png");
        assert_eq!(
            git_raw(&checkout, &["status", "--porcelain", "-uall"])
                .expect("status")
                .trim(),
            "",
            "a screenshot must not dirty the worker checkout"
        );
    }

    #[tokio::test]
    async fn a_reused_pool_regains_the_screenshot_exclude() {
        // A pool first provisioned by an older Mnemo (before the exclude
        // existed) must not hand out a checkout that a screenshot dirties.
        let repo = init_repo();
        let main = repo.path().to_path_buf();
        let (pool, branch) = provision_agent_checkout(
            main.clone(),
            "abcdef123456".to_string(),
            "main".to_string(),
            vec![],
        )
        .await
        .expect("provision");
        // Simulate the old Mnemo: the exclude line is gone.
        let exclude = repo.path().join(".git").join("info").join("exclude");
        let stripped: String = std::fs::read_to_string(&exclude)
            .expect("the exclude file")
            .lines()
            .filter(|line| line.trim() != SCREENSHOT_EXCLUDE_LINE)
            .map(|line| format!("{line}\n"))
            .collect();
        std::fs::write(&exclude, stripped).expect("strip the exclude line");
        release_pool_checkout(main.clone(), pool.clone(), branch)
            .await
            .expect("release");
        // No record holds it: the next worker gets the SAME pool.
        let (reused, _branch) = provision_agent_checkout(
            main,
            "bbbbbbbb2222".to_string(),
            "main".to_string(),
            vec![],
        )
        .await
        .expect("reuse");
        assert_eq!(reused, pool, "the released pool is reused");
        let text = std::fs::read_to_string(&exclude).expect("the exclude file");
        assert_eq!(
            text.lines()
                .filter(|line| line.trim() == SCREENSHOT_EXCLUDE_LINE)
                .count(),
            1,
            "the reused checkout re-adds the exclude line: {text:?}"
        );
        write_screenshot(&reused, "reused.png");
        assert_eq!(
            git_raw(&reused, &["status", "--porcelain", "-uall"])
                .expect("status")
                .trim(),
            "",
            "a screenshot must not dirty a reused checkout"
        );
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

    // ---------------------------------------------------------------------
    // Pool checkout tests
    // ---------------------------------------------------------------------

    #[test]
    fn pool_index_accepts_only_pool_children_of_worktrees() {
        let dir = tempfile::tempdir().unwrap();
        let worktrees = dir.path().join(".worktrees");
        // The one true shape: pool-<n>, decimal, no leading zero, n ≥ 1,
        // direct child of `.worktrees`.
        assert_eq!(pool_index(&worktrees.join("pool-1")), Some(1));
        assert_eq!(pool_index(&worktrees.join("pool-12")), Some(12));
        assert!(is_pool_checkout(&worktrees.join("pool-1")));
        // Rejected: zero, a leading zero, non-decimal, a wrong prefix.
        assert_eq!(pool_index(&worktrees.join("pool-0")), None);
        assert_eq!(pool_index(&worktrees.join("pool-01")), None);
        assert_eq!(pool_index(&worktrees.join("pool-x")), None);
        assert_eq!(pool_index(&worktrees.join("poolx-1")), None);
        assert!(!is_pool_checkout(&worktrees.join("pool-x")));
        // Rejected: a pool-1 whose parent is not `.worktrees`, and a bare
        // name with no parent at all.
        assert_eq!(pool_index(&dir.path().join("elsewhere").join("pool-1")), None);
        assert_eq!(pool_index(Path::new("pool-1")), None);
        assert_eq!(pool_checkout_name(1), "pool-1");
        assert_eq!(pool_checkout_name(42), "pool-42");
    }

    #[tokio::test]
    async fn first_provision_creates_pool_1_on_the_new_branch() {
        let repo = init_repo();
        let (checkout, branch) = provision_agent_checkout(
            repo.path().to_path_buf(),
            "abcdef123456".to_string(),
            "main".to_string(),
            vec![],
        )
        .await
        .expect("provision");
        assert_eq!(branch, "wt/agent-abcdef12");
        assert_eq!(
            checkout,
            repo.path().join(".worktrees").join("pool-1"),
            "the first provision creates pool-1"
        );
        assert_eq!(current_branch(&checkout).expect("branch"), branch);
        let head = git_raw(&checkout, &["rev-parse", "HEAD"]).expect("head");
        let base = git_raw(repo.path(), &["rev-parse", "main^{commit}"]).expect("base");
        assert_eq!(head.trim(), base.trim(), "the branch starts at base_ref");
    }

    #[tokio::test]
    async fn a_held_pool_is_skipped_and_pool_2_is_created() {
        let repo = init_repo();
        let main = repo.path().to_path_buf();
        let (pool_a, branch_a) = provision_agent_checkout(
            main.clone(),
            "aaaaaaaa1111".to_string(),
            "main".to_string(),
            vec![],
        )
        .await
        .expect("first provision");
        assert_eq!(pool_index(&pool_a), Some(1));
        release_pool_checkout(main.clone(), pool_a.clone(), branch_a)
            .await
            .expect("release");
        // pool-1 now satisfies EVERY free-pool precondition — only the
        // held entry (a live record's worktree) keeps it from being reused.
        let (pool_b, branch_b) = provision_agent_checkout(
            main.clone(),
            "bbbbbbbb2222".to_string(),
            "main".to_string(),
            vec![pool_a.clone()],
        )
        .await
        .expect("second provision");
        assert_eq!(pool_index(&pool_b), Some(2), "the held pool is skipped");
        assert_eq!(
            git_raw(&pool_a, &["rev-parse", "--abbrev-ref", "HEAD"])
                .expect("head a")
                .trim(),
            "HEAD",
            "the held pool stays detached and untouched"
        );
        assert_eq!(current_branch(&pool_b).expect("branch b"), branch_b);
    }

    #[tokio::test]
    async fn a_released_pool_is_reused_and_keeps_build_caches() {
        let repo = init_repo_ignoring_pool_caches();
        let main = repo.path().to_path_buf();
        let (pool, branch) = provision_agent_checkout(
            main.clone(),
            "abcdef123456".to_string(),
            "main".to_string(),
            vec![],
        )
        .await
        .expect("provision");
        assert_eq!(pool_index(&pool), Some(1));
        // The previous worker's warm caches and build leftovers — all
        // ignored, so the checkout is still clean.
        write_all(&[
            (pool.join("target/x"), "x"),
            (pool.join("node_modules/y"), "y"),
            (pool.join("sub/node_modules/z"), "z"),
            (pool.join(".coding/codegraph.db"), "db"),
            (pool.join(".coding/codegraph.db-wal"), "wal"),
            (pool.join("sub/target/w"), "w"),
            (pool.join(".coding/tmp/t"), "t"),
            (pool.join("a.log"), "log"),
        ]);
        release_pool_checkout(main.clone(), pool.clone(), branch)
            .await
            .expect("release");
        assert_eq!(
            git_raw(&pool, &["rev-parse", "--abbrev-ref", "HEAD"])
                .expect("head")
                .trim(),
            "HEAD",
            "release detaches the checkout"
        );
        // No record holds it: the next worker gets the SAME pool.
        let (reused, reused_branch) = provision_agent_checkout(
            main,
            "bbbbbbbb2222".to_string(),
            "main".to_string(),
            vec![],
        )
        .await
        .expect("reuse");
        assert_eq!(reused, pool, "the released pool is reused");
        // The warm caches survive the reuse sweep...
        assert!(
            reused.join("target/x").exists(),
            "the workspace's own target/ survives"
        );
        assert!(reused.join("node_modules/y").exists(), "a root node_modules/ survives");
        assert!(
            reused.join("sub/node_modules/z").exists(),
            "node_modules/ survives at any depth"
        );
        assert!(reused.join(".coding/codegraph.db").exists(), "the code graph survives");
        assert!(reused.join(".coding/codegraph.db-wal").exists(), "the sidecar survives");
        // ...while other ignored leftovers are swept.
        assert!(!reused.join("sub/target/w").exists(), "a nested target/ is swept");
        assert!(!reused.join(".coding/tmp/t").exists(), ".coding/tmp/ is swept");
        assert!(!reused.join("a.log").exists(), "logs are swept");
        assert_eq!(current_branch(&reused).expect("branch"), reused_branch);
        let head = git_raw(&reused, &["rev-parse", "HEAD"]).expect("head");
        let base = git_raw(repo.path(), &["rev-parse", "main^{commit}"]).expect("base");
        assert_eq!(head.trim(), base.trim(), "HEAD is on the new branch at base_ref");
    }

    #[tokio::test]
    async fn a_dirty_or_attached_pool_is_never_reused() {
        let repo = init_repo();
        let main = repo.path().to_path_buf();
        let (pool_a, branch_a) = provision_agent_checkout(
            main.clone(),
            "aaaaaaaa1111".to_string(),
            "main".to_string(),
            vec![],
        )
        .await
        .expect("provision a"); // pool-1: on its branch, skipped for reuse
        let (pool_b, branch_b) = provision_agent_checkout(
            main.clone(),
            "bbbbbbbb2222".to_string(),
            "main".to_string(),
            vec![],
        )
        .await
        .expect("provision b"); // pool-2: pool-1 was attached
        // pool-1: released (detached), then dirtied with an uncommitted
        // tracked edit. pool-2: still on its worker's branch, clean.
        release_pool_checkout(main.clone(), pool_a.clone(), branch_a.clone())
            .await
            .expect("release a");
        std::fs::write(pool_a.join("a.txt"), "dirty edit\n").unwrap();
        // Neither is reusable → a new pool is created, and both pools are
        // left exactly as they were.
        let (pool_c, branch_c) = provision_agent_checkout(
            main,
            "cccccccc3333".to_string(),
            "main".to_string(),
            vec![],
        )
        .await
        .expect("provision c");
        assert_eq!(pool_index(&pool_c), Some(3), "a new pool is created");
        assert_eq!(
            std::fs::read_to_string(pool_a.join("a.txt")).expect("edit"),
            "dirty edit\n",
            "the dirty edit is untouched"
        );
        assert_eq!(
            git_raw(&pool_a, &["rev-parse", "--abbrev-ref", "HEAD"])
                .expect("head a")
                .trim(),
            "HEAD",
            "the dirty pool stays detached and untouched"
        );
        assert_eq!(
            current_branch(&pool_b).expect("branch"),
            branch_b,
            "the attached pool keeps its branch"
        );
        assert_eq!(current_branch(&pool_c).expect("branch c"), branch_c);
    }

    #[tokio::test]
    async fn a_linked_pool_is_skipped_without_touching_its_target() {
        let repo = init_repo();
        let pool = repo.path().join(".worktrees/pool-1");
        git_raw(
            repo.path(),
            &[
                "worktree",
                "add",
                "--detach",
                pool.to_str().expect("pool path"),
                "main",
            ],
        )
        .expect("register detached pool");
        let target = repo.path().join("linked-checkout");
        std::fs::rename(&pool, &target).expect("move the real checkout aside");
        if let Err(error) = create_directory_link(&pool, &target) {
            eprintln!("cannot create the pool directory link ({error}); skipping this test");
            return;
        }
        std::fs::write(target.join("marker.txt"), "untouched\n").expect("target marker");

        let checkout = provision_pool_checkout(
            repo.path().to_path_buf(),
            "wt/agent-linked".to_string(),
            "main".to_string(),
            vec![],
        )
        .await
        .expect("provision past the linked pool");

        assert_eq!(pool_index(&checkout), Some(2));
        assert_eq!(
            std::fs::read_to_string(target.join("marker.txt")).expect("marker survives"),
            "untouched\n"
        );
        assert_eq!(
            git_raw(&target, &["rev-parse", "--abbrev-ref", "HEAD"])
                .expect("target HEAD")
                .trim(),
            "HEAD",
            "no switch runs through the link"
        );
        let metadata = std::fs::symlink_metadata(&pool).expect("linked pool metadata");
        assert!(is_link_or_junction(&metadata), "the link itself is left in place");
    }

    #[tokio::test]
    async fn a_fresh_claim_skips_the_pool_and_creates_the_next_index() {
        let repo = init_repo();
        let pool = provision_pool_checkout(
            repo.path().to_path_buf(),
            "wt/agent-old".to_string(),
            "main".to_string(),
            vec![],
        )
        .await
        .expect("provision pool-1");
        release_pool_checkout(repo.path().to_path_buf(), pool.clone(), "wt/agent-old".to_string())
            .await
            .expect("release pool-1");
        let claim = pool.with_file_name("pool-1.claim");
        std::fs::write(&claim, "12345\n").expect("fresh claim");
        let checkout = provision_pool_checkout(
            repo.path().to_path_buf(),
            "wt/agent-new".to_string(),
            "main".to_string(),
            vec![],
        )
        .await
        .expect("provision past a fresh claim");
        assert_eq!(pool_index(&checkout), Some(2));
        assert!(claim.exists(), "another instance's fresh claim remains");
    }

    #[tokio::test]
    async fn a_stale_claim_is_taken_over_and_removed() {
        let repo = init_repo();
        let pool = provision_pool_checkout(
            repo.path().to_path_buf(),
            "wt/agent-old".to_string(),
            "main".to_string(),
            vec![],
        )
        .await
        .expect("provision pool-1");
        release_pool_checkout(repo.path().to_path_buf(), pool.clone(), "wt/agent-old".to_string())
            .await
            .expect("release pool-1");
        let claim = pool.with_file_name("pool-1.claim");
        std::fs::write(&claim, exited_process_id().to_string()).expect("stale claim");
        let file = std::fs::File::options().write(true).open(&claim).expect("claim file");
        file.set_modified(
            std::time::SystemTime::now()
                .checked_sub(std::time::Duration::from_secs(601))
                .expect("stale timestamp"),
        )
        .expect("age the claim");
        let checkout = provision_pool_checkout(
            repo.path().to_path_buf(),
            "wt/agent-new".to_string(),
            "main".to_string(),
            vec![],
        )
        .await
        .expect("take over a stale claim");
        assert_eq!(checkout, pool);
        assert!(!claim.exists(), "the owned claim is dropped after provisioning");
    }

    #[tokio::test]
    async fn an_old_claim_with_a_live_owner_is_never_taken_over() {
        let repo = init_repo();
        let pool = provision_pool_checkout(
            repo.path().to_path_buf(),
            "wt/agent-old".to_string(),
            "main".to_string(),
            vec![],
        )
        .await
        .expect("provision pool-1");
        release_pool_checkout(repo.path().to_path_buf(), pool.clone(), "wt/agent-old".to_string())
            .await
            .expect("release pool-1");
        let claim = pool.with_file_name("pool-1.claim");
        std::fs::write(&claim, format!("{}\n", std::process::id())).expect("live claim");
        let file = std::fs::File::options().write(true).open(&claim).expect("claim file");
        file.set_modified(
            std::time::SystemTime::now()
                .checked_sub(std::time::Duration::from_secs(601))
                .expect("old timestamp"),
        )
        .expect("age the live claim");

        let checkout = provision_pool_checkout(
            repo.path().to_path_buf(),
            "wt/agent-new".to_string(),
            "main".to_string(),
            vec![],
        )
        .await
        .expect("provision past a live claim");
        assert_eq!(pool_index(&checkout), Some(2));
        assert!(claim.exists(), "the live owner's old claim remains");
    }

    #[tokio::test]
    async fn a_claim_file_does_not_reserve_a_pool_index() {
        let repo = init_repo();
        let worktrees = repo.path().join(".worktrees");
        std::fs::create_dir_all(&worktrees).expect("worktrees folder");
        std::fs::write(worktrees.join("pool-9.claim"), "12345\n").expect("orphan claim");
        let checkout = provision_pool_checkout(
            repo.path().to_path_buf(),
            "wt/agent-new".to_string(),
            "main".to_string(),
            vec![],
        )
        .await
        .expect("claim files are not pools");
        assert_eq!(pool_index(&checkout), Some(1));
        assert!(worktrees.join("pool-9.claim").exists());
    }

    #[tokio::test]
    async fn an_unregistered_pool_folder_reserves_its_index() {
        let repo = init_repo();
        let worktrees = repo.path().join(".worktrees");
        std::fs::create_dir_all(worktrees.join("pool-3")).unwrap();
        // A folder that merely exists (not a registered worktree) is never
        // reused, and its index is never handed out again: the first
        // provision creates pool-4 and leaves pool-3 untouched.
        let (checkout, branch) = provision_agent_checkout(
            repo.path().to_path_buf(),
            "abcdef123456".to_string(),
            "main".to_string(),
            vec![],
        )
        .await
        .expect("provision");
        assert_eq!(pool_index(&checkout), Some(4));
        assert_eq!(current_branch(&checkout).expect("branch"), branch);
        assert!(
            worktrees.join("pool-3").is_dir(),
            "the unregistered folder is left untouched"
        );
    }

    #[tokio::test]
    async fn release_refuses_dirty_or_foreign_branch_checkouts() {
        let repo = init_repo();
        let main = repo.path().to_path_buf();
        let (pool, branch) = provision_agent_checkout(
            main.clone(),
            "abcdef123456".to_string(),
            "main".to_string(),
            vec![],
        )
        .await
        .expect("provision");
        // Uncommitted work: refused, and everything is retained.
        std::fs::write(pool.join("a.txt"), "dirty\n").unwrap();
        let error = release_pool_checkout(main.clone(), pool.clone(), branch.clone())
            .await
            .expect_err("dirty checkouts are refused");
        assert!(error.contains("uncommitted work"), "{error}");
        assert_eq!(
            std::fs::read_to_string(pool.join("a.txt")).expect("edit"),
            "dirty\n"
        );
        assert_eq!(current_branch(&pool).expect("branch"), branch);
        // Restore the edit, then ask for someone else's branch: refused.
        std::fs::write(pool.join("a.txt"), "one\n").unwrap();
        let error = release_pool_checkout(
            main.clone(),
            pool.clone(),
            "wt/agent-somebody".to_string(),
        )
        .await
        .expect_err("foreign-branch releases are refused");
        assert!(error.contains("unexpected branch"), "{error}");
        assert_eq!(current_branch(&pool).expect("branch"), branch);
        // A path that is not a pool checkout is refused outright.
        let error = release_pool_checkout(main, repo.path().join("a.txt"), branch)
            .await
            .expect_err("non-pool paths are refused");
        assert!(error.contains("not a pool checkout"), "{error}");
    }

    #[tokio::test]
    async fn release_keeps_the_folder_and_frees_the_branch() {
        let repo = init_repo();
        let main = repo.path().to_path_buf();
        let (pool, branch) = provision_agent_checkout(
            main.clone(),
            "abcdef123456".to_string(),
            "main".to_string(),
            vec![],
        )
        .await
        .expect("provision");
        release_pool_checkout(main, pool.clone(), branch.clone())
            .await
            .expect("release");
        // The folder survives, detached.
        assert!(pool.is_dir(), "the pool folder is never removed");
        assert_eq!(
            git_raw(&pool, &["rev-parse", "--abbrev-ref", "HEAD"])
                .expect("head")
                .trim(),
            "HEAD"
        );
        // The detached checkout lets the caller delete the branch.
        git_raw(repo.path(), &["branch", "-d", &branch]).expect("branch delete");
        assert!(
            git_raw(repo.path(), &["branch", "--list", &branch])
                .expect("branch list")
                .trim()
                .is_empty(),
            "the branch is gone after git branch -d"
        );
        assert!(
            git_raw(repo.path(), &["worktree", "list", "--porcelain"])
                .expect("worktree list")
                .contains("pool-1"),
            "the pool stays registered as a worktree"
        );
    }

    #[tokio::test]
    async fn release_is_idempotent_only_when_the_detached_pool_is_clean() {
        let repo = init_repo();
        let main = repo.path().to_path_buf();
        let (pool, branch) = provision_agent_checkout(
            main.clone(),
            "abcdef123456".to_string(),
            "main".to_string(),
            vec![],
        )
        .await
        .expect("provision");
        release_pool_checkout(main.clone(), pool.clone(), branch.clone())
            .await
            .expect("first release");
        let released_head = git_raw(&pool, &["rev-parse", "HEAD"]).expect("released HEAD");

        std::fs::write(pool.join("a.txt"), "dirty\n").expect("dirty detached pool");
        let error = release_pool_checkout(main.clone(), pool.clone(), branch.clone())
            .await
            .expect_err("a dirty detached pool is not released");
        assert!(error.contains("uncommitted"), "{error}");
        git_raw(&pool, &["restore", "a.txt"]).expect("restore clean detached pool");

        release_pool_checkout(main, pool.clone(), branch)
            .await
            .expect("a clean detached pool is already released");
        assert_eq!(
            git_raw(&pool, &["rev-parse", "HEAD"]).expect("unchanged HEAD"),
            released_head
        );
    }

    #[tokio::test]
    async fn discard_and_release_drops_uncommitted_work() {
        let repo = init_repo_ignoring_pool_caches();
        let main = repo.path().to_path_buf();
        let (pool, branch) = provision_agent_checkout(
            main.clone(),
            "abcdef123456".to_string(),
            "main".to_string(),
            vec![],
        )
        .await
        .expect("provision");
        // The dead worker's uncommitted work...
        write_all(&[
            (pool.join("newdir/inner.txt"), "inner"),
            (pool.join("untracked.txt"), "untracked"),
            (pool.join("target/x"), "warm"),
        ]);
        std::fs::write(pool.join("a.txt"), "dirty\n").unwrap();
        discard_and_release_pool_checkout(main.clone(), pool.clone(), branch.clone())
            .await
            .expect("discard and release");
        assert_eq!(
            std::fs::read_to_string(pool.join("a.txt")).expect("tracked"),
            "one\n",
            "the tracked edit is reset"
        );
        assert!(
            !pool.join("untracked.txt").exists(),
            "untracked files are swept"
        );
        assert!(
            !pool.join("newdir").exists(),
            "untracked directories are swept"
        );
        assert!(
            pool.join("target/x").exists(),
            "ignored caches survive the discard (clean -ffd has no -x)"
        );
        assert_eq!(
            git_raw(&pool, &["rev-parse", "--abbrev-ref", "HEAD"])
                .expect("head")
                .trim(),
            "HEAD",
            "the pool is detached and free again"
        );
        // The freed pool is reused by the next dispatch.
        let (reused, _) = provision_agent_checkout(
            main,
            "bbbbbbbb2222".to_string(),
            "main".to_string(),
            vec![],
        )
        .await
        .expect("reuse");
        assert_eq!(reused, pool);
    }

    #[tokio::test]
    async fn rollback_agent_checkout_frees_an_untouched_pool_and_deletes_the_branch() {
        let repo = init_repo_ignoring_pool_caches();
        let main = repo.path().to_path_buf();
        let base_tip = git_raw(repo.path(), &["rev-parse", "main^{commit}"])
            .expect("base tip")
            .trim()
            .to_string();
        let (pool, branch) = provision_agent_checkout(
            main.clone(),
            "abcdef123456".to_string(),
            "main".to_string(),
            vec![],
        )
        .await
        .expect("provision");
        // Uncommitted work blocks the rollback (the branch is retained).
        std::fs::write(pool.join("a.txt"), "dirty\n").unwrap();
        let error = rollback_agent_checkout(
            main.clone(),
            "abcdef123456".to_string(),
            pool.clone(),
            base_tip.clone(),
        )
        .await
        .expect_err("dirty checkouts are refused");
        assert!(error.contains("uncommitted work"), "{error}");
        assert_eq!(current_branch(&pool).expect("branch"), branch);
        // An untouched checkout rolls back: detached, branch deleted.
        std::fs::write(pool.join("a.txt"), "one\n").unwrap();
        rollback_agent_checkout(main.clone(), "abcdef123456".to_string(), pool.clone(), base_tip)
            .await
            .expect("rollback");
        assert!(pool.is_dir(), "the pool folder is never removed");
        assert_eq!(
            git_raw(&pool, &["rev-parse", "--abbrev-ref", "HEAD"])
                .expect("head")
                .trim(),
            "HEAD"
        );
        assert!(
            git_raw(repo.path(), &["branch", "--list", &branch])
                .expect("branch list")
                .trim()
                .is_empty(),
            "the branch is deleted"
        );
        // The freed pool is handed to the next dispatch.
        let (reused, _) = provision_agent_checkout(
            main,
            "bbbbbbbb2222".to_string(),
            "main".to_string(),
            vec![],
        )
        .await
        .expect("reuse");
        assert_eq!(reused, pool);
    }

    #[tokio::test]
    async fn trim_removes_free_pools_above_the_keep_count() {
        let repo = init_repo_ignoring_pool_caches();
        let main = repo.path().to_path_buf();
        // Three attached pools reserve pool-1, pool-2 and pool-3 in turn.
        let mut pools = Vec::new();
        for id in ["aaaaaaaa1111", "bbbbbbbb2222", "cccccccc3333"] {
            let (pool, branch) = provision_agent_checkout(
                main.clone(),
                id.to_string(),
                "main".to_string(),
                vec![],
            )
            .await
            .expect("provision");
            assert_eq!(pool_index(&pool), Some(pools.len() as u32 + 1));
            pools.push((pool, branch));
        }
        // pool-1 keeps the previous worker's warm, ignored build cache.
        write_all(&[(pools[0].0.join("target/x"), "x")]);
        for (pool, branch) in &pools {
            release_pool_checkout(main.clone(), pool.clone(), branch.clone())
                .await
                .expect("release");
        }
        let removed = trim_pool_checkouts(main.clone(), 1, vec![])
            .await
            .expect("trim");
        assert_eq!(
            removed,
            vec![pools[2].0.clone(), pools[1].0.clone()],
            "highest index first"
        );
        assert!(!pools[1].0.exists(), "pool-2 is removed");
        assert!(!pools[2].0.exists(), "pool-3 is removed");
        assert!(pools[0].0.is_dir(), "a pool at the keep count stays");
        assert!(
            pools[0].0.join("target/x").exists(),
            "its warm build cache stays"
        );
        let listed = registered_worktrees(repo.path()).expect("registered worktrees");
        for (pool, _) in &pools[1..] {
            assert!(
                !listed
                    .iter()
                    .any(|path| same_checkout_path(path, pool)),
                "{} is absent from git worktree list",
                pool.display()
            );
        }
        assert!(
            listed
                .iter()
                .any(|path| same_checkout_path(path, &pools[0].0)),
            "pool-1 stays registered"
        );
    }

    #[tokio::test]
    async fn trim_never_removes_held_dirty_or_attached_pools() {
        let repo = init_repo();
        let main = repo.path().to_path_buf();
        let mut pools = Vec::new();
        for id in ["aaaaaaaa1111", "bbbbbbbb2222", "cccccccc3333", "dddddddd4444"] {
            let (pool, branch) = provision_agent_checkout(
                main.clone(),
                id.to_string(),
                "main".to_string(),
                vec![],
            )
            .await
            .expect("provision");
            pools.push((pool, branch));
        }
        // pool-2 is free but held by a record; pool-3 is free but dirty;
        // pool-4 still belongs to a worker on its branch.
        release_pool_checkout(main.clone(), pools[1].0.clone(), pools[1].1.clone())
            .await
            .expect("release pool-2");
        release_pool_checkout(main.clone(), pools[2].0.clone(), pools[2].1.clone())
            .await
            .expect("release pool-3");
        std::fs::write(pools[2].0.join("untracked.txt"), "keep\n").expect("untracked work");
        let removed = trim_pool_checkouts(main.clone(), 1, vec![pools[1].0.clone()])
            .await
            .expect("trim");
        assert!(removed.is_empty(), "nothing is removed: {removed:?}");
        for (pool, _) in &pools[1..] {
            assert!(pool.is_dir(), "{} stays", pool.display());
        }
        assert_eq!(
            current_branch(&pools[3].0).expect("branch"),
            pools[3].1,
            "the attached pool is untouched"
        );
    }

    #[tokio::test]
    async fn trim_skips_a_pool_with_a_live_claim() {
        let repo = init_repo();
        let main = repo.path().to_path_buf();
        let (pool_1, _) = provision_agent_checkout(
            main.clone(),
            "aaaaaaaa1111".to_string(),
            "main".to_string(),
            vec![],
        )
        .await
        .expect("first provision");
        let (pool_2, branch_2) = provision_agent_checkout(
            main.clone(),
            "bbbbbbbb2222".to_string(),
            "main".to_string(),
            vec![],
        )
        .await
        .expect("second provision");
        release_pool_checkout(main.clone(), pool_2.clone(), branch_2)
            .await
            .expect("release");
        // Another instance holds the claim: a fresh claim owned by this live
        // process must keep the trim away from the pool.
        let claim = pool_2.with_file_name("pool-2.claim");
        std::fs::write(&claim, format!("{}\n", std::process::id())).expect("claim file");
        let removed = trim_pool_checkouts(main.clone(), 1, vec![])
            .await
            .expect("trim");
        assert!(removed.is_empty(), "nothing is removed: {removed:?}");
        assert!(pool_1.is_dir(), "pool-1 stays");
        assert!(pool_2.is_dir(), "a claimed pool stays");
        assert!(claim.exists(), "another instance's claim is left alone");
    }

    #[tokio::test]
    async fn trim_removes_a_pool_whose_link_points_outside_without_following_it() {
        let repo = init_repo_ignoring_pool_caches();
        let main = repo.path().to_path_buf();
        let (pool_1, _) = provision_agent_checkout(
            main.clone(),
            "aaaaaaaa1111".to_string(),
            "main".to_string(),
            vec![],
        )
        .await
        .expect("first provision");
        let (pool_2, branch_2) = provision_agent_checkout(
            main.clone(),
            "bbbbbbbb2222".to_string(),
            "main".to_string(),
            vec![],
        )
        .await
        .expect("second provision");
        // A dependency linked in from OUTSIDE the pool — the npm workspace
        // junction that outlives git's own removal on Windows.
        let outside = tempfile::tempdir().expect("outside folder");
        std::fs::write(outside.path().join("keep.txt"), "keep\n").expect("outside file");
        let link = pool_2.join("node_modules").join("linked");
        std::fs::create_dir_all(link.parent().expect("link parent")).expect("node_modules");
        create_directory_link(&link, outside.path()).expect("directory link");
        release_pool_checkout(main.clone(), pool_2.clone(), branch_2)
            .await
            .expect("release");
        let removed = trim_pool_checkouts(main.clone(), 1, vec![])
            .await
            .expect("trim");
        assert_eq!(removed, vec![pool_2.clone()]);
        assert!(!pool_2.exists(), "the pool folder is gone, link included");
        assert!(
            outside.path().join("keep.txt").exists(),
            "removal never follows the link: its target survives"
        );
        assert!(pool_1.is_dir(), "a pool at the keep count stays");
    }

    #[tokio::test]
    async fn trim_with_keep_at_or_above_the_highest_index_removes_nothing() {
        let repo = init_repo();
        let main = repo.path().to_path_buf();
        let mut pools = Vec::new();
        for id in ["aaaaaaaa1111", "bbbbbbbb2222"] {
            let (pool, branch) = provision_agent_checkout(
                main.clone(),
                id.to_string(),
                "main".to_string(),
                vec![],
            )
            .await
            .expect("provision");
            release_pool_checkout(main.clone(), pool.clone(), branch)
                .await
                .expect("release");
            pools.push(pool);
        }
        let removed = trim_pool_checkouts(main.clone(), 5, vec![])
            .await
            .expect("trim");
        assert!(removed.is_empty(), "nothing is removed: {removed:?}");
        for pool in &pools {
            assert!(pool.is_dir(), "{} stays below a higher keep", pool.display());
        }
        let removed = trim_pool_checkouts(main.clone(), 2, vec![])
            .await
            .expect("trim");
        assert!(removed.is_empty(), "keep == the highest index: {removed:?}");
        for pool in &pools {
            assert!(pool.is_dir(), "{} stays at the keep count", pool.display());
        }
    }
}
