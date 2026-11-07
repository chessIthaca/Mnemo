// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Independent audits: the read-only audit worker dispatch plus the
//! brief and linkage helpers that path needs.

use std::path::Path;
use std::sync::atomic::Ordering;

use mnemo::app::worktrees::{agent_branch, agent_short_id, provision_agent_worktree};
use mnemo::headless::Transcript;

use crate::mnemo2::phase3;
use crate::mnemo2::phase3::ReviewEvidence;

use super::record::{validate_worker_id, WorkerRecord, WorkerStatus};
use super::routing::tier_ids;
use super::TeamRuntime;

/// Cap one brief section, keeping the cut visible to the reviewer.
fn truncate_for_brief(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let mut end = limit;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n[truncated — {end} of {} bytes shown; read the remainder in your checkout at the audited tip]",
        &text[..end],
        text.len()
    )
}

/// Build the audit worker's opening brief: the plan context, the lead's
/// intent brief, and the target diff. It never includes the implementation
/// worker's own summary, findings, or review — the reviewer starts
/// unanchored to the implementer's conclusions.
async fn audit_brief_text(
    root: &Path,
    target: &WorkerRecord,
    tip: &str,
    plan_id: &str,
    plan_doc: &str,
    lead_instructions: Option<&str>,
    reasoning_effort: Option<&str>,
) -> Result<String, String> {
    let merge_base = phase3::git_value(root, &["merge-base", "HEAD", tip])
        .await
        .map_err(|err| format!("could not compute the audit's diff base: {err}"))?;
    let log = phase3::git_value(root, &["log", "--oneline", &format!("{merge_base}..{tip}")])
        .await
        .unwrap_or_default();
    let stat = phase3::git_value(root, &["diff", "--stat", &merge_base, tip])
        .await
        .unwrap_or_default();
    let diff = truncate_for_brief(
        &phase3::git_value(root, &["diff", &merge_base, tip])
            .await
            .unwrap_or_default(),
        48_000,
    );
    let mut text = String::new();
    text.push_str("# Independent read-only audit\n\n");
    text.push_str(
        "You are a Mnemo2 independent audit worker. You did not implement this change; form your own opinion from the plan and the diff alone. You are read-only by construction: you cannot edit files, run shell commands, commit, merge, push, spawn work, ask the user, or tick plan steps. Do not try.\n\n",
    );
    text.push_str(&format!(
        "Implementation worker: {} ({}), branch `{}`, audited tip `{tip}`.\n\n",
        target.name, target.id, target.branch
    ));
    if let Some(effort) = reasoning_effort {
        text.push_str(&format!(
            "Lead-requested reasoning depth: {effort}. Plan the depth of your audit accordingly.\n\n"
        ));
    }
    text.push_str("## The lead's brief to the implementation worker (intent — not a summary of what was done)\n\n");
    text.push_str(&target.brief);
    text.push_str("\n\n");
    if let Some(instructions) = lead_instructions.filter(|value| !value.trim().is_empty()) {
        text.push_str("## Additional lead audit instructions\n\n");
        text.push_str(instructions);
        text.push_str("\n\n");
    }
    text.push_str(&format!(
        "## Plan context — board plan `{plan_id}`\n\n```markdown\n{plan_doc}\n```\n\n"
    ));
    text.push_str(&format!(
        "## Target diff `{merge_base}..{tip}`\n\n### Commits\n\n```\n{log}\n```\n\n### Stat\n\n```\n{stat}\n```\n\n### Full diff\n\n```diff\n{diff}\n```\n\n"
    ));
    text.push_str(
        "## Your task\n\nAudit the change against the plan's intent and its acceptance scenarios where present: correctness of the real execution path, error handling, integration with neighbouring contracts and persistence, concurrency and security interactions, and whether the tests genuinely exercise the change (a passing count alone is insufficient). Report every finding with file/line evidence through message_parent, then end your turn. Never merge, never modify files, never tick plan steps; the lead adjudicates your audit on the implementation worker's review.\n",
    );
    Ok(text)
}

/// Validate the review's independent-audit linkage against the live ledger.
///
/// Any linked reviewer must be a completed audit worker that targets this
/// worker at exactly this tip: a fix commit moves the tip and makes the
/// previous audit stale, blocking review and merge until a fresh audit ran
/// at the new tip. Linking a reviewer also requires a written lead
/// adjudication of its findings.
pub(super) fn validate_audit_linkage(
    records: &[WorkerRecord],
    evidence: &ReviewEvidence,
    worker_id: &str,
    tip: &str,
) -> Result<(), String> {
    let Some(reviewer_id) = evidence
        .independent_reviewer_id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
    else {
        return Ok(());
    };
    if evidence
        .audit_adjudication
        .as_deref()
        .unwrap_or("")
        .trim()
        .is_empty()
    {
        return Err(
            "linking an independent audit requires a written audit_adjudication of its findings"
                .to_string(),
        );
    }
    let audit = records
        .iter()
        .find(|record| record.id == reviewer_id)
        .ok_or_else(|| {
            format!(
                "independent reviewer '{reviewer_id}' is not a tracked worker; dispatch one with team_audit_dispatch first"
            )
        })?;
    if audit.kind != "audit" {
        return Err(format!(
            "independent reviewer '{reviewer_id}' is an implementation worker, not a read-only audit worker"
        ));
    }
    if audit.audit_target.as_deref() != Some(worker_id) {
        return Err(format!(
            "audit worker '{reviewer_id}' reviews '{}' rather than worker '{worker_id}'",
            audit.audit_target.as_deref().unwrap_or("(none)")
        ));
    }
    if audit.audit_tip.as_deref() != Some(tip) {
        return Err(format!(
            "the audit worker '{reviewer_id}' inspected tip '{}' but worker '{worker_id}' is now at tip '{tip}'; the audit is stale — dispatch a fresh audit with team_audit_dispatch at the current tip",
            audit.audit_tip.as_deref().unwrap_or("(none)")
        ));
    }
    if !matches!(audit.status, WorkerStatus::Completed | WorkerStatus::Stopped) {
        return Err(format!(
            "the audit worker '{reviewer_id}' is {:?}, not completed; let it finish before linking it",
            audit.status
        ));
    }
    Ok(())
}

impl TeamRuntime {
    /// Dispatch one fresh read-only independent audit of an implementation
    /// worker's current branch/tip.
    ///
    /// The audit reuses the tracked session/telemetry lifecycle: it gets its
    /// own worktree checked out at the audited branch, its own ledger record
    /// (`kind: "audit"`, no plan step), and the standard worker event pump.
    /// Its opening brief carries the implementation worker's board plan, the
    /// lead's intent brief, and the target diff — never the implementation
    /// worker's own summary, findings, or review — and its tool selection is
    /// read-only by construction: no file mutation, shell, git writes,
    /// spawning, user questions, or plan-step ticks.
    ///
    /// Idempotency: retrying the same `reviewer_id` for the same worker and
    /// the same tip reuses (and restarts, when needed) the existing audit;
    /// a moved tip needs a fresh `reviewer_id` so the next audit has fresh
    /// context.
    pub async fn audit_dispatch(
        &self,
        worker_id: &str,
        reviewer_id: &str,
        model: Option<String>,
        reasoning_effort: Option<String>,
        brief: Option<String>,
    ) -> Result<(WorkerRecord, bool), String> {
        self.audit_dispatch_with_tier(
            worker_id,
            reviewer_id,
            None,
            model,
            reasoning_effort,
            brief,
        )
        .await
    }

    /// Dispatch an audit on an optional ladder tier (default `high`). The
    /// resolved model and tier are stored before the worker starts.
    pub async fn audit_dispatch_with_tier(
        &self,
        worker_id: &str,
        reviewer_id: &str,
        tier: Option<&str>,
        model: Option<String>,
        reasoning_effort: Option<String>,
        brief: Option<String>,
    ) -> Result<(WorkerRecord, bool), String> {
        let _guard = self.merge_lock.lock().await;
        validate_worker_id(reviewer_id)?;
        if let Some(effort) = reasoning_effort.as_deref() {
            if !matches!(effort, "max" | "high" | "medium" | "low" | "minimal" | "none") {
                return Err(format!(
                    "reasoning_effort '{effort}' is not one of max/high/medium/low/minimal/none"
                ));
            }
        }
        let target = self.worker(worker_id).await?;
        if target.kind != "implementation" {
            return Err(format!(
                "worker '{worker_id}' is not an implementation worker; only implementation work is audited"
            ));
        }
        if matches!(target.status, WorkerStatus::Merged | WorkerStatus::Merging) {
            return Err("a merged worker no longer needs an audit; its review already landed".to_string());
        }
        if self.worker_is_busy(worker_id).await {
            return Err("the implementation worker must be idle before audit dispatch".to_string());
        }
        let tip = self.worker_tip(&target).await?;
        // Reuse semantics: the same reviewer may be retried for the same
        // worker at the same tip; a moved tip needs a fresh reviewer id so
        // the stale audit cannot be relinked at the new tip.
        {
            let existing = {
                let records = self.records.lock().await;
                records
                    .iter()
                    .find(|worker| worker.id == reviewer_id)
                    .cloned()
            };
            if let Some(existing) = existing {
                if existing.kind != "audit" {
                    return Err(format!(
                        "worker '{reviewer_id}' already exists as an implementation worker; choose a different reviewer_id"
                    ));
                }
                if existing.audit_target.as_deref() != Some(worker_id) {
                    return Err(format!(
                        "audit worker '{reviewer_id}' already reviews another implementation worker; choose a different reviewer_id"
                    ));
                }
                if existing.audit_tip.as_deref() != Some(tip.as_str()) {
                    return Err(format!(
                        "audit worker '{reviewer_id}' inspected an older tip of worker '{worker_id}'; the fix commit moved the tip — dispatch a fresh audit with a new reviewer_id"
                    ));
                }
                self.validate_model(&existing.model, &existing.reasoning_effort)?;
                if !existing.brief_sent
                    && !self.live.lock().await.contains_key(reviewer_id)
                    && matches!(
                        existing.status,
                        WorkerStatus::Dispatching | WorkerStatus::Failed | WorkerStatus::Interrupted
                    )
                {
                    if !existing.worktree.is_dir() {
                        return Err(format!(
                            "audit worker '{reviewer_id}' has no recoverable worktree"
                        ));
                    }
                    return self
                        .start_worker(existing)
                        .await
                        .map(|worker| (worker, true));
                }
                return Ok((existing, true));
            }
        }
        let requested_tier = tier.unwrap_or("high");
        if self.tier_ladder().get(requested_tier).is_none() {
            return Err(format!(
                "unknown tier '{requested_tier}'; expected one of: {}",
                tier_ids(&self.tier_ladder()).join(", ")
            ));
        }
        let (model, resolved_tier) = if model.is_none() && self.tiered_models().is_empty() {
            (
                self.lead_model_choice(None, None)?,
                Some(requested_tier.to_string()),
            )
        } else {
            self.lead_choice_with_tier(Some(requested_tier), model)?
        };
        let resolved_tier = resolved_tier.or_else(|| Some(requested_tier.to_string()));
        let reasoning_effort = reasoning_effort.or_else(|| {
            resolved_tier.as_deref().and_then(|tier| {
                self.tier_ladder()
                    .get(tier)
                    .and_then(|rung| rung.effort.clone())
            })
        });
        // Without a named model an effort cannot be applied to a session; it
        // stays in the audit brief as the depth the lead asked for.
        let depth_hint = reasoning_effort.clone();
        let reasoning_effort = reasoning_effort.filter(|_| model.is_some());
        self.validate_model(&model, &reasoning_effort)?;
        let branch = agent_branch(reviewer_id);
        if self
            .records
            .lock()
            .await
            .iter()
            .any(|worker| worker.branch == branch)
        {
            return Err(format!("reviewer ID prefix collides with an existing {branch}"));
        }
        // The audit reviews a plan step's change: the implementation
        // worker's board plan is its context, alongside the lead's intent
        // and the target diff.
        let plan_id = target.plan_id.clone().ok_or_else(|| {
            format!(
                "worker '{worker_id}' was not dispatched on a board plan; audits review a plan step's change, so only workers dispatched with plan_id and step can be audited"
            )
        })?;
        let plan_doc = self.plan_document(&plan_id)?;
        let audit_brief = audit_brief_text(
            &self.workspace_root,
            &target,
            &tip,
            &plan_id,
            &plan_doc,
            brief.as_deref(),
            depth_hint.as_deref(),
        )
        .await?;
        let active = self.live.lock().await.len();
        let slots = self.persistence.slots.load(Ordering::SeqCst);
        if active >= slots {
            return Err(format!("all {slots} worker slots are busy"));
        }
        // The audit worktree checks out the audited branch itself so the
        // reviewer reads the exact tree it is auditing.
        let (worktree, branch_created) =
            provision_agent_worktree(self.workspace_root.clone(), reviewer_id.to_string(), target.branch.clone())
                .await
                .map_err(|err| format!("could not provision the audit worktree: {err}"))?;
        let base_tip = phase3::tip(&worktree).await?;
        let millis = phase3::now_ms();
        let record = WorkerRecord {
            id: reviewer_id.to_string(),
            inbox: Vec::new(),
            name: format!("Audit {}", agent_short_id(reviewer_id)),
            brief: audit_brief,
            model,
            model_id: None,
            branch: branch_created,
            worktree,
            status: WorkerStatus::Dispatching,
            generation: 0,
            transcript: Transcript::default(),
            pending: None,
            used_tokens: 0,
            max_tokens: 0,
            brief_sent: false,
            read_back: false,
            compacting: false,
            total_tokens: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
            cached_tokens: 0,
            requests: 0,
            cost_micros: None,
            cost_notified_micros: 0,
            created_at_ms: millis,
            elapsed_ms: 0,
            turns: 0,
            last_activity_ms: millis,
            checks: Vec::new(),
            review_risk: "standard".to_string(),
            kind: "audit".to_string(),
            audit_target: Some(worker_id.to_string()),
            audit_tip: Some(tip),
            report_delivered: false,
            reasoning_effort,
            review_rounds: 0,
            fix_rounds: 0,
            audits: 0,
            models_used: Vec::new(),
            findings: Vec::new(),
            review: None,
            fixer_finding_id: None,
            merge: None,
            error: None,
            tool_errors: Vec::new(),
            opening_prompt: None,
            plan_id: None,
            plan_step: None,
            tier: resolved_tier,
            routing: None,
            reroutes: Vec::new(),
        };
        let audit = self.start_audit_worker(record, base_tip).await?;
        // One audit per audit worker: retrying the same reviewer at the same
        // tip reuses its record, so it is not a new round.
        self.update_worker(worker_id, |worker| worker.audits += 1)
            .await;
        self.persist().await?;
        Ok((audit, false))
    }

    /// Provision and start one audit worker record (the audit counterpart of
    /// [`TeamRuntime::provision_worker`]).
    async fn start_audit_worker(
        &self,
        record: WorkerRecord,
        base_tip: String,
    ) -> Result<WorkerRecord, String> {
        let id = record.id.clone();
        self.records.lock().await.push(record.clone());
        if let Err(err) = self.persist().await {
            self.records.lock().await.retain(|worker| worker.id != id);
            let rollback = mnemo::app::worktrees::rollback_agent_worktree(
                self.workspace_root.clone(),
                id.clone(),
                base_tip,
            )
            .await;
            return Err(match rollback {
                Ok(()) => err,
                Err(rollback) => {
                    let error = format!(
                        "{err}; audit worker '{id}' retains its checkout because rollback was refused: {rollback}"
                    );
                    self.records.lock().await.push(WorkerRecord {
                        status: WorkerStatus::Failed,
                        error: Some(error.clone()),
                        ..record
                    });
                    let _ = self.persist().await;
                    error
                }
            });
        }
        self.start_worker(record).await
    }
}
