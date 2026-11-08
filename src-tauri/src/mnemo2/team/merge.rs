// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The merge and revert path: the merge gate, the merge and the revert under
//! the merge lock, the lead's branch and worktree cleanups with their retries,
//! and the board notices a merge or a revert posts back to the plan.

use std::path::Path;
use std::sync::atomic::Ordering;

use mnemo::app::plan_board::{CloseOutcome, NO_DURABLE_RECORD_NUDGE};
use mnemo::app::team_ledger::TeamLedger;
use mnemo::app::worktrees::{agent_branch, current_branch, remove_agent_worktree};

use crate::mnemo2::ledger::{self, RecordsLedger};
use crate::mnemo2::phase3::{self, MergeRecord};

use super::audit::validate_audit_linkage;
use super::deletion::{
    branch_exists, checkout_is_registered, remove_leftover_checkout, worker_checkout,
};
use super::external_lead::EXTERNAL_LEAD_MEMORY_REMINDER;
use super::nudge::nudge_ignored_row;
use super::record::{validate_worker_id, WorkerRecord, WorkerStatus};
use super::routing::{logs_label, logs_routing, outcome_row};
use super::{now_key, project_relative, TeamRuntime};

/// Whether a record's branch is exactly the one its id names, so Git
/// cleanup can never touch a branch the worker does not own.
pub(super) fn owns_its_branch(record: &WorkerRecord) -> bool {
    !record.id.is_empty()
        && record
            .id
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
        && record.branch == agent_branch(&record.id)
}

/// The merged worker that landed this record's plan step, when the ledger
/// knows one: another implementation record on the same plan step was
/// merged and never reverted, so the step's work reached the lead branch
/// through that worker's commits — typically a replay worker that
/// re-applied this record's conflicted step by hand. Git cannot prove the
/// two commits are the same work; the ledger can. Purely informative: the
/// caller keeps counting this record's own unmerged commits.
pub(super) fn landing_worker_name(records: &[WorkerRecord], record: &WorkerRecord) -> Option<String> {
    let (plan_id, plan_step) = (record.plan_id.as_deref()?, record.plan_step?);
    records
        .iter()
        .find(|other| {
            other.id != record.id
                && other.kind == "implementation"
                && other.plan_id.as_deref() == Some(plan_id)
                && other.plan_step == Some(plan_step)
                && ledger::merged(other)
        })
        .map(|other| other.name.clone())
}

/// The durable error prefix a failed `git branch -d` leaves on a merged
/// worker: the merge landed and only the branch cleanup is outstanding.
const BRANCH_CLEANUP_ERROR: &str = "merged, but branch cleanup needs attention";

/// The durable notes a failed checkout removal leaves on a merged worker.
/// `merge_locked` writes the first, `recover_worker` the second; both are
/// retried by `retry_checkout_cleanups_locked`.
pub(super) const WORKTREE_CLEANUP_ERROR: &str = "merged, but worktree cleanup needs attention";

pub(super) const RECOVERED_WORKTREE_CLEANUP_ERROR: &str =
    "merge recovered, but worktree cleanup needs attention";

/// Whether an error is one of the checkout-cleanup notes above.
fn is_worktree_cleanup_error(error: &str) -> bool {
    error.starts_with(WORKTREE_CLEANUP_ERROR)
        || error.starts_with(RECOVERED_WORKTREE_CLEANUP_ERROR)
}

pub(super) async fn cleanup_agent_worktree(root: &Path, worker: &WorkerRecord) -> Result<(), String> {
    validate_worker_id(&worker.id)?;
    let worktree = worker_checkout(root, &worker.id);
    match std::fs::symlink_metadata(&worktree) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    }
    if !checkout_is_registered(root, &worktree).await? {
        return remove_leftover_checkout(root, worker).await;
    }
    if current_branch(&worktree)? != agent_branch(&worker.id) {
        return Err("worker checkout is on an unexpected branch; checkout retained".to_string());
    }
    let mut last_error = String::new();
    for _ in 0..12 {
        phase3::require_clean(&worktree, "worker before worktree cleanup").await?;
        match remove_agent_worktree(root.to_path_buf(), worker.id.clone()).await {
            Ok(()) if !worktree.exists() => return Ok(()),
            Ok(()) => last_error = "Git deregistered the checkout but left its folder".to_string(),
            Err(_) if !worktree.exists() => return Ok(()),
            Err(err) => last_error = err,
        }
        if !checkout_is_registered(root, &worktree).await? {
            return remove_leftover_checkout(root, worker).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    Err(last_error)
}

/// Delete a merged worker's branch with Git's safe delete. `Ok(true)` when
/// the branch was deleted. `Ok(false)` when there is nothing for this worker
/// to delete: the branch is not the one its id names, or it is already gone.
/// `git branch -d` refuses a branch that is not fully merged into the
/// checked-out lead branch, or that a worktree still uses; that refusal comes
/// back as `Err` and the branch stays.
pub(super) async fn delete_merged_branch(root: &Path, worker: &WorkerRecord) -> Result<bool, String> {
    if !owns_its_branch(worker) || !branch_exists(root, &worker.branch).await {
        return Ok(false);
    }
    phase3::git_value(root, &["branch", "-d", &worker.branch])
        .await
        .map(|_| true)
}

pub(super) async fn merge_record_from_git(
    root: &std::path::Path,
    worker: &WorkerRecord,
    commit: String,
    target: String,
) -> Result<MergeRecord, String> {
    let review = worker
        .review
        .as_ref()
        .ok_or_else(|| "the merge has no durable review record".to_string())?;
    let (files, additions, deletions) = phase3::merge_diff_stats(root, &commit).await?;
    Ok(MergeRecord {
        commit,
        worker_tip: review.tip.clone(),
        branch: worker.branch.clone(),
        target,
        checks: worker
            .checks
            .iter()
            .map(|check| check.name.clone())
            .collect(),
        review_summary: review.summary.clone(),
        review_risk: Some(worker.review_risk.clone()),
        review_evidence: review.evidence.clone(),
        files,
        additions,
        deletions,
        worker_model: worker.model_id.clone().or_else(|| worker.model.clone()),
        worker_tokens: worker.total_tokens,
        cost_micros: worker.cost_micros,
        merged_at_ms: phase3::now_ms(),
        revert_commit: None,
    })
}

pub(super) async fn merge_conflicts_only(root: &Path) -> Result<bool, String> {
    let status = phase3::git_value(root, &["status", "--porcelain", "-uall"]).await?;
    let mut conflicts = false;
    for line in status.lines().filter(|line| !line.is_empty()) {
        if line.len() < 3 || !matches!(&line[..2], "DD" | "AU" | "UD" | "UA" | "DU" | "AA" | "UU") {
            return Ok(false);
        }
        conflicts = true;
    }
    Ok(conflicts)
}

/// One `## Progress` line for a merged task: step, worker, model, merge
/// commit, and the review summary when one was recorded.
fn merge_outcome_line(number: usize, worker: &WorkerRecord) -> String {
    let merge = worker.merge.as_ref();
    let model = merge
        .and_then(|m| m.worker_model.clone())
        .or_else(|| worker.model_id.clone())
        .or_else(|| worker.model.clone())
        .unwrap_or_else(|| "default model".to_string());
    let commit = merge.map_or("", |m| m.commit.as_str());
    let short: String = commit.chars().take(8).collect();
    let mut line = format!(
        "step {number} merged: {} ({}) on {model}, merge commit {short}",
        worker.name, worker.id
    );
    if let Some(summary) = merge
        .map(|m| m.review_summary.trim())
        .filter(|summary| !summary.is_empty())
    {
        line.push_str(&format!("; review: {summary}"));
    }
    line
}

/// Enforce every source/evidence precondition while the merge lock is held.
pub(super) fn validate_merge(worker: &WorkerRecord, tip: &str, records: &[WorkerRecord]) -> Result<(), String> {
    if worker.status != WorkerStatus::InReview {
        return Err(format!("worker is {:?}, not in review", worker.status));
    }
    let review = worker
        .review
        .as_ref()
        .ok_or_else(|| "the current worker tip has not been reviewed".to_string())?;
    if review.tip != tip {
        return Err("the review is stale; review the current worker tip".to_string());
    }
    // A summary plus passing checks alone cannot merge: every review of the
    // merge-time tip must carry complete structured evidence. Legacy reviews
    // decoded without evidence fail here, so old records need a fresh review.
    let evidence = review.evidence.as_ref().ok_or_else(|| {
        "the review predates structured evidence; re-review the current worker tip with a complete evidence object"
            .to_string()
    })?;
    // High-risk merges additionally require a completed, fresh, read-only
    // audit linked to this exact tip plus a written lead adjudication. Any
    // fix commit moved the tip and made the previous audit stale — this gate
    // blocks the merge until a fresh audit ran at the new tip.
    let require_audit = worker.review_risk == "high";
    phase3::validate_review_evidence(evidence, require_audit)?;
    validate_audit_linkage(records, evidence, &worker.id, tip)?;
    if let Some(finding) = worker
        .findings
        .iter()
        .find(|finding| finding.status == "open")
    {
        return Err(format!("finding '{}' is still open", finding.id));
    }
    phase3::validate_findings(&worker.findings)?;
    if worker.checks.is_empty() {
        return Err("no checks have passed on the merge-time tip".to_string());
    }
    if let Some(check) = worker
        .checks
        .iter()
        .find(|check| check.status != "passed" || check.tip != tip)
    {
        return Err(format!(
            "check '{}' has not passed on the merge-time tip",
            check.name
        ));
    }
    Ok(())
}

impl TeamRuntime {
    /// Merge one reviewed worker into the clean, idle, non-main lead branch.
    pub async fn merge(&self, worker_id: &str) -> Result<WorkerRecord, String> {
        self.merge_as(worker_id, false).await
    }

    /// `by_lead` is true when the lead itself asked, from inside its turn.
    pub(super) async fn merge_as(&self, worker_id: &str, by_lead: bool) -> Result<WorkerRecord, String> {
        let worker = self.merge_locked(worker_id, by_lead).await?;
        self.after_merge(&worker).await;
        self.signal_board_changed();
        Ok(worker)
    }

    async fn merge_locked(&self, worker_id: &str, by_lead: bool) -> Result<WorkerRecord, String> {
        let _guard = self.merge_lock.lock().await;
        // An audit worker is a read-only reviewer: it is never mergeable in ANY
        // state, so this refusal comes before every other gate. A still-running
        // audit (or a differently-named lead branch) must not mask the reason.
        let worker = self.worker(worker_id).await?;
        if worker.kind == "audit" {
            return Err(
                "an audit worker is a read-only reviewer and is never merged; adjudicate it on its implementation worker's review".to_string(),
            );
        }
        if !by_lead && self.lead_busy.load(Ordering::SeqCst) {
            return Err("the lead must be idle before merge".to_string());
        }
        if self.worker_is_busy(worker_id).await {
            return Err("the worker must be idle before merge".to_string());
        }
        let target = current_branch(&self.workspace_root)
            .map_err(|err| format!("could not resolve the lead branch: {err}"))?;
        if target == "main" {
            return Err("Mnemo2 never merges a worker directly into main".to_string());
        }
        phase3::require_clean_lead(&self.workspace_root, "lead").await?;
        phase3::require_clean(&worker.worktree, "worker").await?;
        let tip = phase3::tip(&worker.worktree).await?;
        let records = self.records.lock().await.clone();
        validate_merge(&worker, &tip, &records)?;
        let previous = worker.clone();
        self.update_worker(worker_id, |worker| worker.status = WorkerStatus::Merging)
            .await;
        if let Err(err) = self.persist().await {
            self.update_worker(worker_id, |current| *current = previous.clone())
                .await;
            let _ = self.persist().await;
            return Err(err);
        }
        let message = format!("team: merge {} ({})", worker.name, worker.id);
        let output = match phase3::git(
            &self.workspace_root,
            &["merge", "--no-ff", &worker.branch, "-m", &message],
        )
        .await
        {
            Ok(output) => output,
            Err(err) => {
                self.update_worker(worker_id, |current| *current = previous.clone())
                    .await;
                let _ = self.persist().await;
                return Err(err);
            }
        };
        if !output.status.success() {
            let diagnostic = phase3::git_error(
                &["merge", "--no-ff", &worker.branch, "-m", &message],
                &output,
            );
            let merge_head = phase3::git(
                &self.workspace_root,
                &["rev-parse", "-q", "--verify", "MERGE_HEAD"],
            )
            .await?;
            if !merge_head.status.success() {
                self.update_worker(worker_id, |current| {
                    *current = previous.clone();
                    current.error = Some(diagnostic.clone());
                })
                .await;
                self.persist().await?;
                return Err(diagnostic);
            }
            let abort = match phase3::git(&self.workspace_root, &["merge", "--abort"]).await {
                Ok(output) => output,
                Err(err) => {
                    let diagnostic = format!("{diagnostic}\n{err}");
                    self.update_worker(worker_id, |current| {
                        current.error = Some(diagnostic.clone())
                    })
                    .await;
                    let _ = self.persist().await;
                    return Err(diagnostic);
                }
            };
            if !abort.status.success() {
                let diagnostic = format!(
                    "{diagnostic}\n{}",
                    phase3::git_error(&["merge", "--abort"], &abort)
                );
                self.update_worker(worker_id, |current| {
                    current.error = Some(diagnostic.clone())
                })
                .await;
                let _ = self.persist().await;
                return Err(diagnostic);
            }
            if let Err(err) =
                phase3::require_clean_lead(&self.workspace_root, "lead after conflict").await
            {
                self.update_worker(worker_id, |current| current.error = Some(err.clone()))
                    .await;
                let _ = self.persist().await;
                return Err(err);
            }
            self.update_worker(worker_id, |worker| {
                worker.status = WorkerStatus::Conflict;
                worker.error = Some(diagnostic.clone());
                worker.review = None;
                worker.checks.clear();
                worker.fixer_finding_id = None;
            })
            .await;
            self.persist().await?;
            return Err(diagnostic);
        }
        let commit = phase3::tip(&self.workspace_root).await?;
        let record = merge_record_from_git(&self.workspace_root, &worker, commit, target).await?;
        self.update_worker(worker_id, |worker| {
            worker.status = WorkerStatus::Merged;
            worker.merge = Some(record);
            worker.error = None;
        })
        .await;
        self.persist().await?;
        let live = self.live.lock().await.remove(worker_id);
        if let Some(mut live) = live {
            live.abort();
            if let Err(error) = self.close_session_for_cleanup(&live.session).await {
                live.cleanup_retry_only = true;
                self.live.lock().await.insert(worker_id.to_string(), live);
                self.update_worker(worker_id, |worker| {
                    // The Git merge landed, but keep the worker retryable:
                    // `team.merge` skips Merged records and would otherwise
                    // never retry session shutdown or checkout cleanup.
                    worker.status = WorkerStatus::InReview;
                    worker.merge = None;
                    worker.error = Some(format!(
                        "merge was committed, but cleanup needs attention; retry merge after closing the worker session: {error}"
                    ));
                })
                .await;
                self.persist().await?;
                return self.worker(worker_id).await;
            }
        }
        match cleanup_agent_worktree(&self.workspace_root, &worker).await {
            Err(err) => {
                self.update_worker(worker_id, |worker| {
                    worker.error = Some(format!("{WORKTREE_CLEANUP_ERROR}: {err}"));
                })
                .await;
                self.persist().await?;
            }
            Ok(()) => {
                // The checkout is gone, so Git no longer holds the branch. The
                // merge commit keeps the worker's tip as its second parent, so
                // deleting the branch loses no work.
                let shared = self
                    .records
                    .lock()
                    .await
                    .iter()
                    .any(|other| other.id != worker.id && other.branch == worker.branch);
                if !shared {
                    if let Err(err) = delete_merged_branch(&self.workspace_root, &worker).await {
                        self.update_worker(worker_id, |worker| {
                            worker.error = Some(format!("{BRANCH_CLEANUP_ERROR}: {err}"));
                        })
                        .await;
                        self.persist().await?;
                    }
                }
            }
        }
        self.retry_checkout_cleanups_locked(worker_id).await;
        self.sweep_merged_branches(worker_id).await;
        let record = self.worker(worker_id).await?;
        if record.status == WorkerStatus::Merged {
            // The one terminal end a worker is labelled at (design §4.7):
            // the merge landed and the record is Merged. The retryable
            // InReview cleanup path above logs nothing; its successful retry
            // arrives here. The outcome row follows `logs_label` — a hand
            // reroute is a learning in every mode — while the nudge row is
            // Laya-derived and keeps the `logs_routing` gate.
            if logs_label(&record, self.routing_mode()) {
                if let Some(row) = outcome_row(&record, "merged") {
                    self.append_dispatch_routing_row(&row);
                }
            }
            if logs_routing(&record, self.routing_mode()) {
                if let Some(row) = nudge_ignored_row(&record) {
                    self.append_dispatch_routing_row(&row);
                }
            }
            // A project that keeps .coding/laya carries its decisions in git.
            // Best effort and off the merge path: a failed export never fails
            // a merge. It must follow the appends above, or the merge's own
            // outcome row misses the bundle.
            let (log, root) = (self.routing_log_path.clone(), self.workspace_root.clone());
            let _export = tokio::task::spawn_blocking(move || {
                if let Err(error) =
                    crate::mnemo2::laya_learnings::export_project_learnings(&log, &root)
                {
                    eprintln!("mnemo2: could not refresh the Laya learnings: {error}");
                }
            });
        }
        Ok(record)
    }

    /// Retry the checkout removal for merged workers whose earlier removal
    /// failed: a folder something held at merge time, typically a shell whose
    /// working folder was inside it. Runs with the merge lock held. A removal
    /// that works (or finds the folder already gone) clears the note; one
    /// that still fails leaves the record as it was. Deleting the branch of a
    /// worker whose checkout is now gone is `sweep_merged_branches`'s job.
    async fn retry_checkout_cleanups_locked(&self, skip: &str) {
        let candidates: Vec<WorkerRecord> = {
            let records = self.records.lock().await;
            records
                .iter()
                .filter(|candidate| {
                    candidate.id != skip
                        && candidate.status == WorkerStatus::Merged
                        && candidate
                            .error
                            .as_deref()
                            .is_some_and(is_worktree_cleanup_error)
                })
                .cloned()
                .collect()
        };
        let mut cleared = false;
        for candidate in candidates {
            if cleanup_agent_worktree(&self.workspace_root, &candidate)
                .await
                .is_err()
            {
                continue;
            }
            self.update_worker(&candidate.id, |worker| {
                if worker
                    .error
                    .as_deref()
                    .is_some_and(is_worktree_cleanup_error)
                {
                    worker.error = None;
                }
            })
            .await;
            cleared = true;
        }
        if cleared {
            let _ = self.persist().await;
        }
    }

    /// App start: retry earlier stuck checkout removals, then the branch
    /// deletes they were blocking. Takes the merge lock itself.
    pub(super) async fn retry_leftover_cleanups(&self) {
        let _guard = self.merge_lock.lock().await;
        self.retry_checkout_cleanups_locked("").await;
        self.sweep_merged_branches("").await;
    }

    /// Retry the branch delete for earlier merged workers. Runs inside
    /// `merge_locked`, so the merge lock is held. Best effort and silent: a
    /// branch Git refuses to delete stays until the next merge.
    async fn sweep_merged_branches(&self, just_merged: &str) {
        let candidates: Vec<WorkerRecord> = {
            let records = self.records.lock().await;
            records
                .iter()
                .filter(|candidate| {
                    candidate.id != just_merged
                        && ledger::merged(candidate)
                        && owns_its_branch(candidate)
                        && !records.iter().any(|other| {
                            other.id != candidate.id && other.branch == candidate.branch
                        })
                })
                .cloned()
                .collect()
        };
        let mut cleared = false;
        for candidate in candidates {
            // A checkout folder that is still there means its cleanup failed;
            // leave the whole worker alone.
            if std::fs::symlink_metadata(worker_checkout(&self.workspace_root, &candidate.id))
                .is_ok()
            {
                continue;
            }
            if let Ok(true) = delete_merged_branch(&self.workspace_root, &candidate).await {
                self.update_worker(&candidate.id, |worker| {
                    if worker
                        .error
                        .as_deref()
                        .is_some_and(|error| error.starts_with(BRANCH_CLEANUP_ERROR))
                    {
                        worker.error = None;
                    }
                })
                .await;
                cleared = true;
            }
        }
        if cleared {
            let _ = self.persist().await;
        }
    }

    /// Revert a worker merge on the lead branch before it reaches main.
    pub async fn revert_merge(&self, worker_id: &str) -> Result<WorkerRecord, String> {
        self.revert_merge_as(worker_id, false).await
    }

    pub(super) async fn revert_merge_as(
        &self,
        worker_id: &str,
        by_lead: bool,
    ) -> Result<WorkerRecord, String> {
        let worker = self.revert_locked(worker_id, by_lead).await?;
        self.after_revert(&worker).await;
        self.signal_board_changed();
        Ok(worker)
    }

    async fn revert_locked(&self, worker_id: &str, by_lead: bool) -> Result<WorkerRecord, String> {
        let _guard = self.merge_lock.lock().await;
        if !by_lead && self.lead_busy.load(Ordering::SeqCst) {
            return Err("the lead must be idle before revert".to_string());
        }
        phase3::require_clean_lead(&self.workspace_root, "lead").await?;
        let worker = self.worker(worker_id).await?;
        if worker.status != WorkerStatus::Merged {
            return Err("only a merged worker can be reverted".to_string());
        }
        let merge = worker
            .merge
            .clone()
            .ok_or_else(|| "worker has no merge record".to_string())?;
        if merge.revert_commit.is_some() {
            return Err("this worker merge was already reverted".to_string());
        }
        let target = current_branch(&self.workspace_root)?;
        if target == "main" || target != merge.target {
            return Err("revert requires the original non-main lead branch".to_string());
        }
        let in_history = phase3::git(
            &self.workspace_root,
            &["merge-base", "--is-ancestor", &merge.commit, "HEAD"],
        )
        .await?;
        if !in_history.status.success() {
            return Err("the original merge is not in the current lead history".to_string());
        }
        let landed = phase3::git(
            &self.workspace_root,
            &["merge-base", "--is-ancestor", &merge.commit, "main"],
        )
        .await?;
        if landed.status.success() {
            return Err("the worker merge has already reached main".to_string());
        }
        let output = phase3::git(
            &self.workspace_root,
            &["revert", "--no-edit", "-m", "1", &merge.commit],
        )
        .await?;
        if !output.status.success() {
            let _ = phase3::git(&self.workspace_root, &["revert", "--abort"]).await;
            return Err(phase3::git_error(
                &["revert", "--no-edit", "-m", "1", &merge.commit],
                &output,
            ));
        }
        let revert = phase3::tip(&self.workspace_root).await?;
        self.update_worker(worker_id, |worker| {
            if let Some(merge) = worker.merge.as_mut() {
                merge.revert_commit = Some(revert);
            }
            worker.error = None;
        })
        .await;
        self.persist().await?;
        self.worker(worker_id).await
    }

    /// After a merge: tick the worker's plan step once its tasks are all
    /// closed with at least one merged, then try to close the plan. Board
    /// errors never fail the merge; they go to the lead in the notice.
    async fn after_merge(&self, worker: &WorkerRecord) {
        let (Some(plan_id), Some(step)) = (worker.plan_id.as_deref(), worker.plan_step) else {
            return;
        };
        let Some(board) = self.board() else {
            return;
        };
        let _guard = self.merge_lock.lock().await;
        let tasks: Vec<WorkerRecord> = self
            .records
            .lock()
            .await
            .iter()
            .filter(|w| w.plan_id.as_deref() == Some(plan_id) && w.plan_step == Some(step))
            .cloned()
            .collect();
        let number = step + 1;
        let mut lines = Vec::new();
        if !tasks.iter().any(ledger::merged) {
            return;
        }
        if tasks.iter().any(|w| !ledger::is_closed(w)) {
            return;
        }
        let outcome = merge_outcome_line(number, worker);
        let mut notice = None;
        match board.tick(plan_id, step, &outcome) {
            Ok(true) => {
                lines.push(format!("Plan {plan_id} step {number} is done: {outcome}"));
                notice = Some(format!(
                    "tick:{}:{}",
                    worker.id,
                    worker.merge.as_ref().map_or("", |m| m.commit.as_str())
                ));
            }
            Ok(false) => {}
            Err(err) => {
                lines.push(format!(
                    "Plan {plan_id} step {number} could not be ticked: {err}"
                ));
                notice = Some(format!("tick-error:{}:{}", worker.id, now_key()));
            }
        }
        let ledger = RecordsLedger::new(self.records.clone());
        // `tick` released the board lock; closing reads the records again.
        match board.try_close(plan_id, &ledger as &dyn TeamLedger).await {
            CloseOutcome::Closed {
                report_path,
                retired_notes,
                knowledge_nudge,
            } => {
                let relative = project_relative(&self.workspace_root, &report_path);
                lines.push(format!(
                    "Plan {plan_id} is closed. Review report: {relative}"
                ));
                if retired_notes > 0 {
                    lines.push(format!(
                        "{retired_notes} plan-scoped memory note(s) were retired into that report. Read them and re-store any durable fact without plan or branch references."
                    ));
                }
                // One memory line per close: the nudge when the plan
                // established nothing durable, the standing reminder otherwise.
                if knowledge_nudge {
                    lines.push(NO_DURABLE_RECORD_NUDGE.to_string());
                } else if self.external_lead_attached() {
                    lines.push(EXTERNAL_LEAD_MEMORY_REMINDER.to_string());
                }
                notice = Some(format!("closed:{plan_id}:{relative}"));
            }
            CloseOutcome::Blocked { reasons } => {
                lines.push(format!(
                    "Plan {plan_id} cannot close yet: {}",
                    reasons.join("; ")
                ));
                notice = Some(format!("blocked:{plan_id}:{}", reasons.join("|")));
            }
            CloseOutcome::NotReady { .. } => {}
        }
        if let Some(key) = notice {
            self.notify_lead(
                format!("board:{key}"),
                format!(
                    "{}\nUse plan_show for the plan's current state.",
                    lines.join("\n")
                ),
            );
        }
    }

    /// After a revert: reopen the step when no other merged task remains.
    async fn after_revert(&self, worker: &WorkerRecord) {
        let (Some(plan_id), Some(step)) = (worker.plan_id.as_deref(), worker.plan_step) else {
            return;
        };
        let Some(board) = self.board() else {
            return;
        };
        let _guard = self.merge_lock.lock().await;
        let still_merged = self.records.lock().await.iter().any(|w| {
            w.plan_id.as_deref() == Some(plan_id) && w.plan_step == Some(step) && ledger::merged(w)
        });
        if still_merged {
            return;
        }
        let number = step + 1;
        let note = format!(
            "{} ({}) was reverted, so no merged task remains for it",
            worker.name, worker.id
        );
        let text = match board.untick(plan_id, step, &note) {
            Ok(true) => format!("Plan {plan_id} step {number} is reopened: {note}"),
            Ok(false) => return,
            Err(err) => format!("Plan {plan_id} step {number} could not be reopened: {err}"),
        };
        self.notify_lead(
            format!("board:untick:{}:{}", worker.id, now_key()),
            format!("{text}\nUse plan_show for the plan's current state."),
        );
    }
}
