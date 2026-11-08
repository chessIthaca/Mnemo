// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Worktree deletion and the trash folder: what deleting a worker would
//! remove, the per-worker Git checks, the deletion itself, and the trash that
//! holds a checkout Git would not let go yet.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use mnemo::app::worktrees::{
    agent_branch, agent_short_id, current_branch, remove_agent_worktree,
};
use serde::Serialize;

use crate::mnemo2::dispatch_routing::RoutingMode;
use crate::mnemo2::lock_holders;
use crate::mnemo2::phase3;

use super::merge::{landing_worker_name, owns_its_branch};
use super::nudge::nudge_ignored_row;
use super::persistence::TeamPersistence;
use super::record::{WorkerRecord, WorkerStatus};
use super::routing::{logs_label, logs_routing, outcome_row};
use super::{TeamRuntime, TeamSnapshot};

/// The worker's registered checkout path inside the project.
pub(super) fn worker_checkout(root: &Path, worker_id: &str) -> PathBuf {
    root.join(".worktrees")
        .join(format!("agent-{}", agent_short_id(worker_id)))
}

pub(super) async fn branch_exists(root: &Path, branch: &str) -> bool {
    let revision = format!("refs/heads/{branch}^{{commit}}");
    phase3::git_value(root, &["rev-parse", "--verify", "--quiet", &revision])
        .await
        .is_ok()
}

/// How many workers a deletion preview checks concurrently. Each checked
/// worker runs two short Git calls; a small bound keeps a large batch
/// from fighting the live sessions it lists for the CPU.
const DELETION_CHECK_CONCURRENCY: usize = 4;

/// The in-memory half of one worker's deletion preview: everything the
/// [`WorkerDeletion`] reports before its per-worker Git checks run.
struct DeletionCore {
    /// The worker's id.
    worker_id: String,
    /// The worker's display name.
    name: String,
    /// The worker's `wt/agent-*` branch.
    branch: String,
    /// The worker's session is live; deleting stops it first.
    live: bool,
    /// The worker's expected checkout path.
    checkout: PathBuf,
    /// The checkout still exists at that path.
    checkout_exists: bool,
    /// The merged worker that landed this record's plan step through its
    /// own commits, when the ledger knows one.
    superseded_by: Option<String>,
    /// Another worker record outside the batch uses the same branch.
    shared_branch: bool,
    /// Why the worker cannot be deleted right now, when already known.
    blocked: Option<String>,
    /// The checkout exists and is registered, so its branch and
    /// uncommitted state still need to be read.
    needs_check: bool,
}

/// One worker's per-worker Git checks: whether its checkout holds
/// anything uncommitted, and why it is blocked when its branch moved.
struct CheckoutCheck {
    /// The checkout holds uncommitted changes.
    uncommitted: bool,
    /// Why the checkout blocks the worker's deletion, when it does.
    blocked: Option<String>,
}

/// Batch-wide, read-only Git facts for one deletion preview. One
/// `git worktree list` run and one `git for-each-ref` run serve the whole
/// batch instead of each worker spawning its own Git processes (each Git
/// call costs about 0.1 s on this codebase).
struct DeletionGitFacts {
    /// Every checkout path Git's worktree registry lists for this repo.
    registered_checkouts: Vec<PathBuf>,
    /// `<full refname> -> commits ahead of the lead branch's HEAD` for
    /// every local branch. `None` on a Git without the `%(ahead-behind)`
    /// format atom — the preview then counts each branch with its own
    /// `rev-list --count` call.
    ahead_counts: Option<HashMap<String, u32>>,
}

impl DeletionGitFacts {
    /// Gather the batch's facts with two Git runs.
    async fn gather(root: &Path) -> Result<Self, String> {
        let registered_checkouts = registered_checkouts(root).await?;
        let ahead_counts = match phase3::git_value(
            root,
            &[
                "for-each-ref",
                "--format=%(refname) %(ahead-behind:HEAD)",
                "refs/heads/",
            ],
        )
        .await
        {
            Ok(list) => parse_ahead_counts(&list),
            Err(_) => None,
        };
        Ok(Self {
            registered_checkouts,
            ahead_counts,
        })
    }

    /// Whether Git's worktree registry lists this checkout path.
    fn lists(&self, checkout: &Path) -> bool {
        let expected = normalized_checkout_path(checkout);
        self.registered_checkouts
            .iter()
            .any(|listed| same_checkout_path(listed, &expected))
    }
}

/// Every local branch's commits ahead of HEAD, read from one
/// `git for-each-ref --format='%(refname) %(ahead-behind:HEAD)'` run.
/// `None` when a line fails to parse — the preview falls back to
/// counting that branch with its own `rev-list` call.
fn parse_ahead_counts(list: &str) -> Option<HashMap<String, u32>> {
    let mut counts = HashMap::with_capacity(list.lines().count());
    for line in list.lines() {
        let mut fields = line.split_whitespace();
        let refname = fields.next()?;
        let ahead = fields.next()?;
        counts.insert(refname.to_string(), ahead.parse().ok()?);
    }
    Some(counts)
}

/// Every checkout path Git's worktree registry lists, in one
/// `git worktree list` run instead of one per worker.
async fn registered_checkouts(root: &Path) -> Result<Vec<PathBuf>, String> {
    let arguments = ["worktree", "list", "--porcelain", "-z"];
    let output = phase3::git(root, &arguments).await?;
    if !output.status.success() {
        return Err(phase3::git_error(&arguments, &output));
    }
    worktree_list_paths(&output.stdout)
}

/// One worker's per-worker Git checks. Untracked files are listed without
/// descending into untracked directories (`--untracked-files=normal`): the
/// preview only needs to know that *something* is uncommitted, not
/// enumerate a `node_modules` tree.
async fn inspect_checkout(checkout: PathBuf, branch: String) -> Result<CheckoutCheck, String> {
    let current = checkout_branch(&checkout).await?;
    if current != branch {
        return Ok(CheckoutCheck {
            uncommitted: false,
            blocked: Some(format!("its checkout is on '{current}', not {branch}")),
        });
    }
    let status = phase3::git_value(
        &checkout,
        &["status", "--porcelain", "--untracked-files=normal"],
    )
    .await?;
    Ok(CheckoutCheck {
        uncommitted: !status.is_empty(),
        blocked: None,
    })
}

/// A checkout's current branch, without blocking the async executor (the
/// shared [`current_branch`] helper runs Git synchronously).
async fn checkout_branch(checkout: &Path) -> Result<String, String> {
    let branch = phase3::git_value(checkout, &["rev-parse", "--abbrev-ref", "HEAD"]).await?;
    let branch = branch.trim().to_string();
    if branch.is_empty() || branch == "HEAD" {
        return Err("the checkout is on a detached HEAD".to_string());
    }
    Ok(branch)
}

/// Whether Git's main-checkout worktree registry still lists this exact path.
/// Never probe an unregistered folder with Git: without its own `.git`, Git
/// walks up and answers for the main checkout instead.
pub(super) async fn checkout_is_registered(root: &Path, checkout: &Path) -> Result<bool, String> {
    let expected = normalized_checkout_path(checkout);
    Ok(registered_checkouts(root)
        .await?
        .iter()
        .any(|listed| same_checkout_path(listed, &expected)))
}

/// Decode NUL-delimited porcelain worktree paths without replacement
/// characters. Unix paths remain arbitrary bytes; platforms whose paths are
/// Unicode reject invalid UTF-8 instead of failing open as an unregistered
/// checkout.
pub(super) fn worktree_list_paths(stdout: &[u8]) -> Result<Vec<PathBuf>, String> {
    stdout
        .split(|byte| *byte == 0)
        .filter_map(|entry| entry.strip_prefix(b"worktree "))
        .map(|path| {
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStringExt;

                Ok(PathBuf::from(std::ffi::OsString::from_vec(path.to_vec())))
            }
            #[cfg(not(unix))]
            {
                std::str::from_utf8(path)
                    .map(PathBuf::from)
                    .map_err(|error| format!("undecodable worktree path: {error}"))
            }
        })
        .collect()
}

/// Normalize a checkout without following a symlink or junction at its final
/// component. Git prints absolute paths; canonicalizing the parent also
/// reconciles Windows short names and verbatim prefixes.
fn normalized_checkout_path(path: &Path) -> PathBuf {
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => dunce::canonicalize(parent)
            .unwrap_or_else(|_| dunce::simplified(parent).to_path_buf())
            .join(name),
        _ => dunce::simplified(path).to_path_buf(),
    }
}

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

#[cfg(windows)]
fn is_link_or_junction(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    metadata.file_type().is_symlink() || metadata.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn is_link_or_junction(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

/// The checkout path after every ownership guard passes: the worker's own
/// direct-child folder of the canonical `.worktrees`, never a link or
/// junction, never anything else. `None` when it no longer exists.
pub(super) fn validated_checkout(root: &Path, worker: &WorkerRecord) -> Result<Option<PathBuf>, String> {
    if !owns_its_branch(worker) {
        return Err("refusing to remove a checkout for an invalid worker identity".to_string());
    }
    let checkout = worker_checkout(root, &worker.id);
    let metadata = match std::fs::symlink_metadata(&checkout) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    if is_link_or_junction(&metadata) {
        return Err(format!(
            "refusing to remove a symlink or junction at {}",
            checkout.display()
        ));
    }
    if !metadata.is_dir() {
        return Err(format!(
            "refusing to remove a non-directory at {}",
            checkout.display()
        ));
    }
    let canonical_root = dunce::canonicalize(root).map_err(|error| error.to_string())?;
    let worktrees = root.join(".worktrees");
    let canonical_worktrees =
        dunce::canonicalize(&worktrees).map_err(|error| error.to_string())?;
    if !same_checkout_path(&canonical_worktrees, &canonical_root.join(".worktrees")) {
        return Err(format!(
            "refusing to remove {}: .worktrees is not a direct child of the workspace",
            checkout.display()
        ));
    }
    let canonical = dunce::canonicalize(&checkout).map_err(|error| error.to_string())?;
    let expected = canonical_worktrees.join(format!("agent-{}", agent_short_id(&worker.id)));
    if !same_checkout_path(&canonical, &expected)
        || canonical.parent() != Some(canonical_worktrees.as_path())
        || canonical.file_name() != expected.file_name()
    {
        return Err(format!(
            "refusing to remove {}: it is not the worker's own checkout",
            checkout.display()
        ));
    }
    Ok(Some(canonical))
}

/// Remove an unregistered checkout with filesystem operations only. The
/// canonical path must remain the expected direct child of this repository's
/// `.worktrees`; links and junctions are refused rather than traversed.
pub(super) async fn remove_leftover_checkout(root: &Path, worker: &WorkerRecord) -> Result<(), String> {
    let Some(canonical) = validated_checkout(root, worker)? else {
        return Ok(());
    };

    let mut last_error = String::new();
    for attempt in 0..12 {
        // Revalidate immediately before every destructive call, then delete
        // only the canonical path that passed the direct-child guard above.
        let current = match dunce::canonicalize(&canonical) {
            Ok(current) => current,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.to_string()),
        };
        let metadata = match std::fs::symlink_metadata(&canonical) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.to_string()),
        };
        if current != canonical || is_link_or_junction(&metadata) || !metadata.is_dir() {
            return Err(format!(
                "refusing to remove {}: the validated checkout changed",
                canonical.display()
            ));
        }
        let path = canonical.clone();
        match tokio::task::spawn_blocking(move || std::fs::remove_dir_all(path)).await {
            Ok(Ok(())) => return Ok(()),
            Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Ok(Err(error)) => last_error = error.to_string(),
            Err(error) => last_error = error.to_string(),
        }
        if !canonical.exists() {
            return Ok(());
        }
        if attempt < 11 {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    }
    // Name what still holds the folder, so the user knows what to close. The
    // probe is best effort and never closes anything itself.
    let probe = canonical.clone();
    let holders = tokio::task::spawn_blocking(move || lock_holders::holders(&[probe]))
        .await
        .unwrap_or_default();
    Err(format!(
        "could not remove leftover checkout {}: {last_error}{}",
        canonical.display(),
        lock_holders::held_by_suffix(&holders)
    ))
}

/// The trash folder renamed-away checkouts wait in for background removal:
/// `.worktrees/.trash` — inside the gitignored `.worktrees`, never a
/// tracked path.
fn trash_root(root: &Path) -> PathBuf {
    root.join(".worktrees").join(".trash")
}

/// Every folder waiting in the trash.
pub(super) fn trash_entries(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(trash_root(root)) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .collect()
}

/// Rename a validated checkout folder into the trash folder with one
/// `rename` (same volume, same parent tree): the delete flow waits for a
/// cheap move instead of the slow file-by-file removal of a large
/// checkout. Fails when something holds the folder — the caller then
/// falls back to the foreground removal path.
pub(super) async fn move_checkout_to_trash(root: &Path, checkout: &Path) -> Result<(), String> {
    tokio::task::spawn_blocking({
        let root = root.to_path_buf();
        let checkout = checkout.to_path_buf();
        move || {
            let trash = trash_root(&root);
            std::fs::create_dir_all(&trash).map_err(|error| error.to_string())?;
            let name = checkout
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| "the checkout path has no folder name".to_string())?;
            let mut target = trash.join(format!("{name}-{}", phase3::now_ms()));
            let mut attempt = 0;
            while target.exists() {
                attempt += 1;
                target = trash.join(format!("{name}-{}-{attempt}", phase3::now_ms()));
            }
            std::fs::rename(checkout, &target).map_err(|error| error.to_string())
        }
    })
    .await
    .map_err(|error| format!("the checkout move task failed: {error}"))?
}

/// Sweep the trash folder in the background: try to remove every folder in
/// it, retrying for a bounded window — a file can be held for a moment by
/// an indexer or a scanner right after the rename. Anything still held
/// stays for the next sweep; every app start schedules one.
pub(super) fn schedule_trash_sweep(root: &Path) {
    let root = root.to_path_buf();
    tokio::spawn(async move {
        for _ in 0..12 {
            if trash_entries(&root).is_empty() {
                return;
            }
            sweep_trash(&root).await;
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    });
}

/// Remove every folder under `.worktrees/.trash/` once. Each folder is
/// removed independently — one locked folder never keeps the others.
pub(super) async fn sweep_trash(root: &Path) {
    for entry in trash_entries(root) {
        let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(entry)).await;
    }
}

/// What deleting one worker removes, reported before anything is deleted so
/// the user can see exactly which unmerged work would be discarded.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct WorkerDeletion {
    /// The worker's id.
    pub worker_id: String,
    /// The worker's display name.
    pub name: String,
    /// The worker's `wt/agent-*` branch.
    pub branch: String,
    /// The worker's session is live; deleting stops it first.
    pub live: bool,
    /// The worker's checkout still exists and will be removed.
    pub checkout: bool,
    /// That checkout has uncommitted changes.
    pub uncommitted: bool,
    /// Commits on the branch that the lead branch does not contain.
    pub unmerged_commits: u32,
    /// The merged worker that landed this record's plan step through its
    /// own commits, when the ledger knows one (a replay worker, for
    /// example): those commits re-apply work that already reached the lead
    /// branch, so the preview says whose instead of presenting them as the
    /// only copy. `None` — serialized as `null` — when no merged worker
    /// shares the step. Purely informative: the commit count and the
    /// discard check are unaffected.
    pub superseded_by: Option<String>,
    /// Another worker record (outside this deletion) uses the same branch,
    /// so the branch and checkout are kept and only this record goes.
    pub shared_branch: bool,
    /// Why the worker cannot be deleted right now, when it cannot.
    pub blocked: Option<String>,
}

impl WorkerDeletion {
    /// Whether deleting this worker discards work the lead branch lacks.
    pub fn discards_work(&self) -> bool {
        self.uncommitted || self.unmerged_commits > 0
    }
}

/// One worker's outcome of a completed deletion batch: `error` is `None`
/// exactly when the worker was deleted.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct WorkerDeletionResult {
    /// The worker the result describes.
    pub worker_id: String,
    /// The worker's display name.
    pub name: String,
    /// Why the worker could not be deleted, or `None` when it was deleted.
    pub error: Option<String>,
}

/// `team_delete`'s outcome: one result per requested worker, in request
/// order, plus the team snapshot after every deletion that succeeded.
#[derive(Clone, Debug, Serialize)]
pub struct TeamDeleteOutcome {
    /// One result per requested worker, in request order.
    pub results: Vec<WorkerDeletionResult>,
    /// The team after every deletion that succeeded.
    pub snapshot: TeamSnapshot,
}

/// One streamed deletion-flow update on [`DELETION_EVENT_CHANNEL`]: a
/// worker's finished preview (checking phase) or its deletion result
/// (deleting phase), emitted the moment it is known so the open sheet
/// can fill its rows in.
#[derive(Clone, Debug, Serialize)]
pub struct DeletionProgress {
    /// The phase the update belongs to.
    pub phase: DeletionPhase,
    /// The worker the update describes.
    pub worker_id: String,
    /// The worker's display name.
    pub name: String,
    /// The worker's finished preview; `None` in the deleting phase.
    pub preview: Option<WorkerDeletion>,
    /// Why the worker's deletion failed; `None` when it was deleted.
    pub error: Option<String>,
}

/// Which phase of a deletion flow an update belongs to.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeletionPhase {
    /// `team_delete_preview` streams a worker's finished preview.
    Checking,
    /// `team_delete` streams a worker's deletion result.
    Deleting,
}

/// Outcome of conservative start-screen cleanup.
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct CleanupReport {
    /// Clean worktrees actually removed.
    pub removed: Vec<String>,
    /// Branches/worktrees retained, with the reason for each.
    pub skipped: Vec<String>,
}

/// Clean only inactive, clean worker checkouts; retain every worker branch.
pub async fn cleanup_workspace(
    root: &Path,
    state_dir: &Path,
    routing_log_path: &Path,
) -> Result<CleanupReport, String> {
    current_branch(root)?;
    let persistence = TeamPersistence::new(state_dir);
    let Some(mut ledger) = persistence.load()? else {
        return Ok(CleanupReport::default());
    };
    let mut report = CleanupReport::default();
    for index in 0..ledger.workers.len() {
        let worker = &mut ledger.workers[index];
        if worker.merge.is_some()
            || worker.worktree.as_os_str().is_empty()
            || matches!(
                worker.status,
                WorkerStatus::Dispatching
                    | WorkerStatus::Working
                    | WorkerStatus::NeedsYou
                    | WorkerStatus::Fixing
                    | WorkerStatus::Merging
                    | WorkerStatus::Conflict
            )
        {
            continue;
        }
        let id = worker.id.clone();
        let branch = worker.branch.clone();
        if branch != agent_branch(&id)
            || id.is_empty()
            || !id.chars().all(|c| c.is_ascii_alphanumeric())
        {
            report
                .skipped
                .push(format!("{branch}: invalid worker identity"));
            continue;
        }
        let checkout = root
            .join(".worktrees")
            .join(format!("agent-{}", agent_short_id(&id)));
        let recorded = match dunce::canonicalize(&worker.worktree) {
            Ok(path) => path,
            Err(_) if !worker.worktree.exists() => checkout.clone(),
            Err(error) => {
                report.skipped.push(format!("{branch}: {error}"));
                continue;
            }
        };
        let expected = if std::fs::symlink_metadata(&checkout).is_ok() {
            dunce::canonicalize(&checkout).map_err(|err| err.to_string())?
        } else {
            checkout.clone()
        };
        if recorded != expected {
            report.skipped.push(format!(
                "{branch}: checkout is outside its registered worktree"
            ));
            continue;
        }
        let revision = format!("refs/heads/{branch}^{{commit}}");
        if let Err(error) = phase3::git_value(root, &["rev-parse", "--verify", &revision]).await {
            report.skipped.push(format!(
                "{branch}: retained checkout because the branch cannot be verified: {error}"
            ));
            continue;
        }
        if std::fs::symlink_metadata(&checkout).is_ok() {
            let registered = checkout_is_registered(root, &checkout).await?;
            let mut removal_error = None;
            if registered {
                match phase3::git_value(&checkout, &["status", "--porcelain", "-uall"]).await {
                    Ok(status) if status.trim().is_empty() => {}
                    Ok(_) => {
                        report
                            .skipped
                            .push(format!("{branch}: uncommitted work is retained for review"));
                        continue;
                    }
                    Err(error) => {
                        report.skipped.push(format!(
                            "{branch}: cleanliness could not be verified: {error}"
                        ));
                        continue;
                    }
                }
                if let Err(error) = remove_agent_worktree(root.to_path_buf(), id.clone()).await {
                    removal_error = Some(error);
                }
            }
            if std::fs::symlink_metadata(&checkout).is_ok() {
                if checkout_is_registered(root, &checkout).await? {
                    report.skipped.push(format!(
                        "{branch}: cleanup failed: {}",
                        removal_error.unwrap_or_else(|| {
                            "the registered checkout folder remains".to_string()
                        })
                    ));
                    continue;
                }
                if let Err(error) = remove_leftover_checkout(root, worker).await {
                    report
                        .skipped
                        .push(format!("{branch}: cleanup failed: {error}"));
                    continue;
                }
            }
            report.removed.push(branch.clone());
        } else {
            report.skipped.push(format!(
                "{branch}: checkout was already absent; branch retained"
            ));
        }
        worker.status = WorkerStatus::Cancelled;
        worker.pending = None;
        worker.worktree = PathBuf::new();
        worker.error =
            Some("inactive checkout cleaned; the branch is retained for review".to_string());
        // Abandoned is terminal (design §4.7): the worker's `outcome` row
        // and, when its one nudge was never acted on, the `nudge_ignored`
        // row land beside the cleaned checkout — the outcome row for a
        // worker routing ever applied to or one that was rerouted by hand
        // (a hand reroute is a learning in every mode), the nudge row only
        // for a worker routing ever applied to (cleanup has no settings
        // handle, so the record decides).
        let outcome = outcome_row(worker, "abandoned")
            .filter(|_| logs_label(worker, RoutingMode::Off));
        let nudge_ignored = nudge_ignored_row(worker)
            .filter(|_| logs_routing(worker, RoutingMode::Off));
        let rows = [outcome, nudge_ignored]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        persistence.save(&ledger)?;
        for row in rows {
            mnemo::model_routing::append_row(routing_log_path, &row);
        }
    }
    Ok(report)
}

impl TeamRuntime {
    /// Describe what deleting these workers would remove, changing nothing.
    ///
    /// The preview is read-only, so it does not take the merge lock:
    /// `delete_workers` re-checks every worker under the lock before
    /// changing anything. Batch-wide Git facts are gathered once and the
    /// workers are inspected concurrently, so a large batch costs about
    /// the same as a small one.
    pub async fn deletion_preview(
        &self,
        worker_ids: &[String],
    ) -> Result<Vec<WorkerDeletion>, String> {
        self.deletion_preview_streaming(worker_ids, |_| {}).await
    }

    /// `deletion_preview` with per-worker progress: `on_preview` receives
    /// each worker's preview the moment its checks finish, so the open
    /// sheet can fill its rows in as they arrive.
    pub async fn deletion_preview_streaming(
        &self,
        worker_ids: &[String],
        on_preview: impl Fn(WorkerDeletion),
    ) -> Result<Vec<WorkerDeletion>, String> {
        self.inspect_batch(worker_ids, on_preview).await
    }

    /// Delete workers and their history: the ledger record (brief,
    /// transcript, telemetry, review, findings), inbox messages, checkout,
    /// and `wt/agent-*` branch. A live session is stopped first. Every
    /// worker is checked before any is deleted; work the lead branch lacks
    /// (uncommitted changes, unmerged commits) is discarded only when
    /// `discard_work` confirms it. A worker that cannot be deleted is
    /// reported with its error and stays listed — every other worker is
    /// still deleted.
    pub async fn delete_workers(
        &self,
        worker_ids: &[String],
        discard_work: bool,
    ) -> Result<TeamDeleteOutcome, String> {
        self.delete_workers_streaming(worker_ids, discard_work, |_| {}).await
    }

    /// `delete_workers` with per-worker progress: `on_result` receives
    /// each worker's outcome the moment it is known, so the open sheet
    /// can show each row's result as it happens.
    pub async fn delete_workers_streaming(
        &self,
        worker_ids: &[String],
        discard_work: bool,
        on_result: impl Fn(WorkerDeletionResult),
    ) -> Result<TeamDeleteOutcome, String> {
        let _guard = self.merge_lock.lock().await;
        // One result per requested worker, in request order: a failure
        // sets its worker's error and never stops the rest of the batch.
        let mut outcomes: Vec<WorkerDeletionResult> = Vec::with_capacity(worker_ids.len());
        for worker_id in worker_ids {
            outcomes.push(WorkerDeletionResult {
                worker_id: worker_id.clone(),
                name: self
                    .worker(worker_id)
                    .await
                    .map(|record| record.name)
                    .unwrap_or_else(|_| worker_id.clone()),
                error: None,
            });
        }
        // Phase A: check every worker before any is deleted.
        let previews = self.inspect_batch(worker_ids, |_| {}).await?;
        let mut stopped: Vec<(usize, WorkerDeletion)> = Vec::new();
        for (position, preview) in previews.into_iter().enumerate() {
            match Self::deletion_decision(&preview, discard_work) {
                Ok(()) => stopped.push((position, preview)),
                Err(error) => {
                    outcomes[position].error = Some(error.clone());
                    on_result(outcomes[position].clone());
                }
            }
        }
        // Phase B: stop every live session.
        let mut pending: Vec<(usize, WorkerDeletion)> = Vec::new();
        for (position, deletion) in stopped {
            if self.live.lock().await.contains_key(&deletion.worker_id) {
                match self.stop_inner(&deletion.worker_id).await {
                    Ok(_stopped) => pending.push((position, deletion)),
                    Err(error) => {
                        outcomes[position].error = Some(error.clone());
                        on_result(outcomes[position].clone());
                    }
                }
            } else {
                pending.push((position, deletion));
            }
        }
        // Phase C: re-check after stopping — a session may have written
        // more work between the preview and its stop. The batch for this
        // pass is the set that still proceeds, so Git state shared with a
        // worker that failed a check or a stop is never touched.
        let survivor_ids: Vec<String> = pending
            .iter()
            .map(|(_, deletion)| deletion.worker_id.clone())
            .collect();
        let rechecked = self.inspect_batch(&survivor_ids, |_| {}).await?;
        let mut deletions: Vec<(usize, WorkerDeletion)> = Vec::new();
        for (index, preview) in rechecked.into_iter().enumerate() {
            let (position, _) = pending[index];
            match Self::deletion_decision(&preview, discard_work) {
                Ok(()) => deletions.push((position, preview)),
                Err(error) => {
                    outcomes[position].error = Some(error.clone());
                    on_result(outcomes[position].clone());
                }
            }
        }
        // Phase D: delete each worker — one failure never stops the rest.
        for (position, deletion) in deletions {
            if let Err(error) = self.delete_one(&deletion).await {
                outcomes[position].error = Some(error.clone());
            }
            on_result(outcomes[position].clone());
        }
        Ok(TeamDeleteOutcome {
            results: outcomes,
            snapshot: self.snapshot().await,
        })
    }

    /// The decision for one worker's preview: `Ok` deletes it, an `Err`
    /// reports why it must stay — blocked, or work the lead branch lacks
    /// that the user did not confirm discarding.
    fn deletion_decision(deletion: &WorkerDeletion, discard_work: bool) -> Result<(), String> {
        if let Some(reason) = &deletion.blocked {
            return Err(format!("{} cannot be deleted: {reason}", deletion.name));
        }
        if deletion.discards_work() && !discard_work {
            return Err(format!(
                "{} has unmerged or uncommitted work on {}; confirm discarding it to delete the worker",
                deletion.name, deletion.branch
            ));
        }
        Ok(())
    }

    /// Inspect every worker of a deletion batch, streaming each preview
    /// as soon as its checks finish. Batch-wide Git facts are gathered
    /// with two Git runs and the per-worker checkout checks run
    /// `DELETION_CHECK_CONCURRENCY` at a time, so a large batch costs
    /// about the same as a small one. Git state shared with a record
    /// outside the batch is never counted or touched.
    async fn inspect_batch(
        &self,
        batch: &[String],
        on_preview: impl Fn(WorkerDeletion),
    ) -> Result<Vec<WorkerDeletion>, String> {
        let facts = DeletionGitFacts::gather(&self.workspace_root).await?;
        let mut cores = Vec::with_capacity(batch.len());
        for worker_id in batch {
            cores.push(self.inspect_core(worker_id, batch, &facts).await?);
        }
        let mut previews: Vec<Option<WorkerDeletion>> = vec![None; cores.len()];
        let mut pending = Vec::new();
        for (index, core) in cores.iter().enumerate() {
            if core.needs_check {
                pending.push((index, core.checkout.clone(), core.branch.clone()));
            }
        }
        // The Git-heavy half runs as one task per worker, bounded: every
        // worker in a chunk is checked concurrently while the chunks run
        // in order. A preview is finished — and emitted — the moment its
        // check lands.
        for chunk in pending.chunks(DELETION_CHECK_CONCURRENCY) {
            let mut handles = Vec::with_capacity(chunk.len());
            for (index, checkout, branch) in chunk {
                handles.push((
                    *index,
                    tokio::spawn(inspect_checkout(checkout.clone(), branch.clone())),
                ));
            }
            for (index, handle) in handles {
                let check = handle
                    .await
                    .map_err(|error| format!("a checkout check task failed: {error}"))??;
                let preview =
                    self.finish_preview(&cores[index], Some(check), &facts).await?;
                on_preview(preview.clone());
                previews[index] = Some(preview);
            }
        }
        // Workers whose Git state needs no per-worker check (no checkout,
        // unregistered, shared branch, merging) finish immediately.
        for index in 0..cores.len() {
            if previews[index].is_none() {
                let preview = self.finish_preview(&cores[index], None, &facts).await?;
                on_preview(preview.clone());
                previews[index] = Some(preview);
            }
        }
        Ok(previews
            .into_iter()
            .map(|preview| preview.expect("every preview finished"))
            .collect())
    }

    /// The in-memory half of one worker's preview: everything except its
    /// per-worker Git checks. Git state shared with a record outside the
    /// batch is never counted or touched.
    async fn inspect_core(
        &self,
        worker_id: &str,
        batch: &[String],
        facts: &DeletionGitFacts,
    ) -> Result<DeletionCore, String> {
        let record = self.worker(worker_id).await?;
        let shared_branch = self.records.lock().await.iter().any(|other| {
            other.id != record.id
                && other.branch == record.branch
                && !batch.iter().any(|id| id == &other.id)
        });
        // Informative in every state, blocked included: another worker may
        // have landed this record's plan step through its own commits.
        let superseded_by = {
            let records = self.records.lock().await;
            landing_worker_name(&records, &record)
        };
        let mut core = DeletionCore {
            worker_id: record.id.clone(),
            name: record.name.clone(),
            branch: record.branch.clone(),
            live: self.live.lock().await.contains_key(worker_id),
            checkout: worker_checkout(&self.workspace_root, &record.id),
            checkout_exists: false,
            superseded_by,
            shared_branch,
            blocked: None,
            needs_check: false,
        };
        if record.status == WorkerStatus::Merging {
            core.blocked = Some("a merge is in progress".to_string());
            return Ok(core);
        }
        if core.shared_branch || !owns_its_branch(&record) {
            // A shared or malformed legacy record: only the record itself is
            // removed; no Git state another record (or no record) owns is
            // touched.
            return Ok(core);
        }
        core.checkout_exists = std::fs::symlink_metadata(&core.checkout).is_ok();
        // Never probe an unregistered folder with Git: without its own
        // `.git`, Git walks up and answers for the main checkout instead.
        core.needs_check = core.checkout_exists && facts.lists(&core.checkout);
        Ok(core)
    }

    /// Merge a core and its finished checkout check into the worker's
    /// preview. The unmerged count comes from the batch's ahead counts; a
    /// Git without the `%(ahead-behind)` format atom falls back to
    /// counting that one branch with its own `rev-list` call.
    async fn finish_preview(
        &self,
        core: &DeletionCore,
        check: Option<CheckoutCheck>,
        facts: &DeletionGitFacts,
    ) -> Result<WorkerDeletion, String> {
        let (uncommitted, blocked) = match check {
            Some(CheckoutCheck { uncommitted, blocked }) => (uncommitted, core.blocked.clone().or(blocked)),
            None => (false, core.blocked.clone()),
        };
        let unmerged_commits = match &facts.ahead_counts {
            Some(counts) => counts
                .get(&format!("refs/heads/{}", core.branch))
                .copied()
                .unwrap_or(0),
            None => {
                if branch_exists(&self.workspace_root, &core.branch).await {
                    let range = format!("HEAD..refs/heads/{}", core.branch);
                    phase3::git_value(&self.workspace_root, &["rev-list", "--count", &range])
                        .await?
                        .parse()
                        .unwrap_or(0)
                } else {
                    0
                }
            }
        };
        Ok(WorkerDeletion {
            worker_id: core.worker_id.clone(),
            name: core.name.clone(),
            branch: core.branch.clone(),
            live: core.live,
            checkout: core.checkout_exists,
            uncommitted,
            unmerged_commits,
            superseded_by: core.superseded_by.clone(),
            shared_branch: core.shared_branch,
            blocked,
        })
    }

    /// Remove one checked worker. Git state goes first and the record last,
    /// so a failure leaves a record that a retry can finish deleting.
    async fn delete_one(&self, deletion: &WorkerDeletion) -> Result<(), String> {
        let record = self.worker(&deletion.worker_id).await?;
        let live = self.live.lock().await.remove(&record.id);
        if let Some(mut live) = live {
            live.abort();
            if let Err(error) = self.close_session_for_cleanup(&live.session).await {
                let message = format!("could not close {}'s session: {error}", record.name);
                live.cleanup_retry_only = true;
                self.live.lock().await.insert(record.id.clone(), live);
                return Err(message);
            }
        }
        if !deletion.shared_branch && owns_its_branch(&record) {
            self.remove_checkout(&record).await?;
            if branch_exists(&self.workspace_root, &record.branch).await {
                phase3::git_value(&self.workspace_root, &["branch", "-D", &record.branch])
                    .await
                    .map_err(|error| {
                        format!("could not delete {}'s branch: {error}", record.name)
                    })?;
            }
        }
        self.records
            .lock()
            .await
            .retain(|worker| worker.id != record.id);
        self.messages
            .lock()
            .await
            .retain(|message| message.worker_id != record.id);
        self.persist().await
    }

    /// Remove the worker's checkout folder from `.worktrees` and deregister
    /// its worktree. The folder is first renamed into `.worktrees/.trash/`
    /// with one cheap filesystem move — the user never waits for the slow
    /// file-by-file delete of a large checkout; the trash folder is
    /// removed in the background instead. When the rename is refused (on
    /// Windows: something holds the folder or a file in it), the removal
    /// falls back to the foreground retry path, which names the holder.
    async fn remove_checkout(&self, worker: &WorkerRecord) -> Result<(), String> {
        let Some(canonical) = validated_checkout(&self.workspace_root, worker)? else {
            return Ok(());
        };
        match move_checkout_to_trash(&self.workspace_root, &canonical).await {
            Ok(()) => {
                // The renamed checkout no longer sits at its registered
                // path: sweep the trash folder in the background and drop
                // the stale worktree registration, so the branch delete
                // below cannot fail on it.
                schedule_trash_sweep(&self.workspace_root);
                let arguments = ["worktree", "prune"];
                let prune = phase3::git(&self.workspace_root, &arguments).await?;
                if !prune.status.success() {
                    return Err(phase3::git_error(&arguments, &prune));
                }
            }
            Err(_held) => {
                // The move was refused — the folder is held. Fall back to
                // the foreground removal, which waits for holders and
                // reports the one it finds.
                self.remove_checkout_in_front(worker, &canonical).await?;
            }
        }
        Ok(())
    }

    /// The foreground checkout removal, used when the rename into trash
    /// was refused. Runs `git worktree remove --force` for a registered
    /// checkout (discarding was confirmed before) and filesystem removal
    /// for a leftover, then names the holder when the folder refuses to
    /// go.
    async fn remove_checkout_in_front(
        &self,
        worker: &WorkerRecord,
        checkout: &Path,
    ) -> Result<(), String> {
        let mut last_error = String::new();
        for _ in 0..12 {
            if std::fs::symlink_metadata(checkout).is_err() {
                break;
            }
            if !checkout_is_registered(&self.workspace_root, checkout).await? {
                remove_leftover_checkout(&self.workspace_root, worker).await?;
                break;
            }
            match remove_agent_worktree(self.workspace_root.clone(), worker.id.clone()).await {
                Ok(()) if !checkout.exists() => break,
                Ok(()) => {
                    last_error =
                        "Git deregistered the checkout but left its folder".to_string()
                }
                Err(error) => last_error = error,
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        if checkout.exists() {
            // Name what still holds the folder, so the user knows what to
            // close. The probe is best effort and never closes anything.
            let probe = checkout.to_path_buf();
            let holders =
                tokio::task::spawn_blocking(move || lock_holders::holders(&[probe]))
                    .await
                    .unwrap_or_default();
            return Err(format!(
                "could not remove {}'s checkout: {last_error}{}",
                worker.name,
                lock_holders::held_by_suffix(&holders)
            ));
        }
        Ok(())
    }
}
