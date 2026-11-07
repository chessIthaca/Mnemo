// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The review and fix path: worker tips, review records, checks,
//! findings, and the fix dispatch that routes a fix back to a worker.

use std::sync::atomic::Ordering;

use mnemo::app::worktrees::agent_branch;

use crate::mnemo2::phase3;
use crate::mnemo2::dispatch_routing::{
    DispatchRoutingRow, RoutingMode, classify_dispatch, routing_text, threshold_for,
};
use crate::mnemo2::phase3::{Finding, ReviewEvidence, ReviewRecord};
use crate::mnemo2::runtime::LeadModel;

use super::{TeamRuntime, unix_ms};
use super::audit::validate_audit_linkage;
use super::nudge::{claim_nudge, deliver_nudge, pending_fix_downshift, route_fix_brief};
use super::record::{
    ChangeSource, InboxKind, MessageSender, WorkerCheck, WorkerRecord, WorkerStatus,
};
use super::routing::NUDGE_LADDER;

impl TeamRuntime {
    pub(super) async fn worker_tip(&self, worker: &WorkerRecord) -> Result<String, String> {
        if worker.status == WorkerStatus::Cancelled && worker.worktree.as_os_str().is_empty() {
            if worker.branch != agent_branch(&worker.id)
                || worker.id.is_empty()
                || !worker.id.chars().all(|c| c.is_ascii_alphanumeric())
            {
                return Err("the worker ledger has an invalid branch identity".to_string());
            }
            return phase3::git_value(
                &self.workspace_root,
                &[
                    "rev-parse",
                    &format!("refs/heads/{}^{{commit}}", worker.branch),
                ],
            )
            .await;
        }
        phase3::tip(&worker.worktree).await
    }

    /// Persist a lead review against the current worker commit.
    ///
    /// Every new review carries complete structured evidence: an unanchored
    /// diff pass plus substantive intent, execution, integration, test-quality,
    /// and residual-risk assessments. High-risk reviews additionally require a
    /// completed fresh independent audit (`independent_reviewer_id` plus a
    /// written `audit_adjudication`) linked at the exact reviewed tip.
    pub async fn review(
        &self,
        worker_id: &str,
        summary: String,
        findings: Vec<Finding>,
        evidence: ReviewEvidence,
    ) -> Result<WorkerRecord, String> {
        let _guard = self.merge_lock.lock().await;
        if summary.trim().is_empty() {
            return Err("the review summary is empty".to_string());
        }
        let worker = self.worker(worker_id).await?;
        if worker.kind == "audit" {
            return Err(
                "an audit worker is a read-only reviewer and is not reviewed; adjudicate it by recording team_review on its implementation worker with independent_reviewer_id".to_string(),
            );
        }
        // High-risk reviews require a completed, fresh independent audit at
        // the exact reviewed tip; every linked reviewer is validated against
        // the live ledger so a stale or foreign audit can never be attached.
        let require_audit = worker.review_risk == "high";
        phase3::validate_review_evidence(&evidence, require_audit)?;
        phase3::validate_findings(&findings)?;
        if matches!(worker.status, WorkerStatus::Merged | WorkerStatus::Merging) {
            return Err("a merged worker cannot be reviewed again".to_string());
        }
        if worker.status == WorkerStatus::Failed {
            return Err("resume the failed worker and complete a fix before review".to_string());
        }
        if self.worker_is_busy(worker_id).await {
            return Err("the worker must be idle before review".to_string());
        }
        let tip = self.worker_tip(&worker).await?;
        let records = self.records.lock().await.clone();
        validate_audit_linkage(&records, &evidence, worker_id, &tip)?;
        let was_cancelled = worker.status == WorkerStatus::Cancelled;
        // Capture every escalation trigger before this review replaces the
        // previous finding set and clears the routed-finding marker.
        let rounds = worker.review_rounds.saturating_add(1);
        let repeated = (rounds >= 2)
            .then(|| {
                findings.iter().find(|finding| {
                    finding.status == "open"
                        && worker.findings.iter().any(|old| {
                            old.id == finding.id && old.status == "open"
                        })
                })
            })
            .flatten()
            .cloned();
        let routed = worker.fixer_finding_id.as_deref().and_then(|id| {
            findings
                .iter()
                .find(|finding| finding.id == id && finding.status == "open")
                .cloned()
        });
        let audited = (worker.fix_rounds > 0 && evidence.independent_reviewer_id.is_some())
            .then(|| {
                findings
                    .iter()
                    .find(|finding| finding.status == "open" && finding.severity == "high")
                    .cloned()
            })
            .flatten();
        let (escalation_finding, escalation_trigger) = match (routed, repeated, audited) {
            (Some(finding), _, _) => (Some(finding), "route_fix"),
            (None, Some(finding), _) => (Some(finding), "review"),
            (None, None, Some(finding)) => (Some(finding), "audit"),
            (None, None, None) => (None, "review"),
        };
        let downshift = pending_fix_downshift(&worker, &findings);
        self.update_worker(worker_id, |worker| {
            worker.status = if was_cancelled {
                WorkerStatus::Cancelled
            } else {
                WorkerStatus::InReview
            };
            worker.review = Some(ReviewRecord {
                summary,
                tip,
                reviewed_at_ms: phase3::now_ms(),
                evidence: Some(evidence),
            });
            worker.findings = findings;
            worker.review_rounds += 1;
            worker.fixer_finding_id = None;
            worker.error = None;
        })
        .await;
        self.persist().await?;
        let updated = self.worker(worker_id).await?;
        let updated = match downshift {
            Some((row, closed)) => self.settle_fix_downshift(worker_id, row, closed).await?,
            None => updated,
        };
        // Off/shadow/suggest leave a lead notice. Auto additionally switches
        // an idle worker, preserving the live session and transcript.
        let settings = self
            .routing
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if settings.mode != RoutingMode::Off && settings.nudges {
            if let Some(finding) = escalation_finding {
                let enabled = self.direct_nudge_tiers(updated.model.as_deref());
                if let Some(claim) =
                    claim_nudge(&self.records, worker_id, enabled, settings.mode, None).await
                {
                    let detail = if escalation_trigger == "audit" {
                        format!(
                            "high-severity audit finding {} remains after a fix round",
                            finding.id
                        )
                    } else if finding.severity == "high" && updated.review_rounds >= 2 {
                        format!(
                            "keeps a high finding after {} review rounds",
                            updated.review_rounds
                        )
                    } else if escalation_trigger == "route_fix" {
                        format!("route-fix finding {} is still open", finding.id)
                    } else {
                        format!(
                            "review finding {} stayed open after {} review rounds",
                            finding.id, updated.review_rounds
                        )
                    };
                    deliver_nudge(
                        &self.signals,
                        &self.routing_log_path,
                        worker_id,
                        &claim,
                        "review",
                        &detail,
                        "",
                    );
                    if settings.mode == RoutingMode::Auto {
                        self.auto_escalate_worker(worker_id).await?;
                    }
                }
                // The claim (and any auto switch) changed the stored record.
                return self.worker(worker_id).await;
            }
        }
        Ok(updated)
    }

    /// Record a real check outcome against the server-observed worker tip.
    pub async fn record_check(
        &self,
        worker_id: &str,
        name: String,
        status: String,
        detail: Option<String>,
    ) -> Result<WorkerRecord, String> {
        let _guard = self.merge_lock.lock().await;
        if name.trim().is_empty() || !matches!(status.as_str(), "passed" | "failed") {
            return Err("a check needs a name and a passed/failed outcome".to_string());
        }
        let worker = self.worker(worker_id).await?;
        if matches!(worker.status, WorkerStatus::Merged | WorkerStatus::Merging) {
            return Err("checks cannot be added during or after merge".to_string());
        }
        let tip = self.worker_tip(&worker).await?;
        self.update_worker(worker_id, |worker| {
            worker.checks.retain(|check| check.name != name);
            worker.checks.push(WorkerCheck {
                name,
                status,
                detail,
                tip,
            });
        })
        .await;
        self.persist().await?;
        self.worker(worker_id).await
    }

    /// Close one finding without changing the reviewed source tip.
    pub async fn resolve_finding(
        &self,
        worker_id: &str,
        finding_id: &str,
        status: &str,
        evidence: Option<String>,
        justification: Option<String>,
    ) -> Result<WorkerRecord, String> {
        let _guard = self.merge_lock.lock().await;
        if !matches!(status, "fixed" | "wontfix") {
            return Err("a finding can be closed as fixed or wontfix".to_string());
        }
        if status == "fixed" && evidence.as_deref().unwrap_or("").trim().is_empty() {
            return Err("fixed requires verification evidence".to_string());
        }
        if status == "wontfix" && justification.as_deref().unwrap_or("").trim().is_empty() {
            return Err("wontfix requires a written justification".to_string());
        }
        let worker = self.worker(worker_id).await?;
        if matches!(worker.status, WorkerStatus::Merged | WorkerStatus::Merging) {
            return Err("a merged worker's findings are immutable".to_string());
        }
        if !worker
            .findings
            .iter()
            .any(|finding| finding.id == finding_id)
        {
            return Err(format!("unknown finding '{finding_id}'"));
        }
        let fix_commit = if status == "fixed" {
            Some(self.worker_tip(&worker).await?)
        } else {
            None
        };
        self.update_worker(worker_id, |worker| {
            if let Some(finding) = worker
                .findings
                .iter_mut()
                .find(|item| item.id == finding_id)
            {
                finding.status = status.to_string();
                finding.fix_commit = fix_commit;
                finding.evidence = evidence;
                finding.justification = justification;
            }
        })
        .await;
        self.persist().await?;
        self.worker(worker_id).await
    }

    /// Apply and commit one constrained source replacement for a finding.
    pub async fn review_fix(
        &self,
        worker_id: &str,
        finding_id: &str,
        relative: &str,
        old_string: &str,
        new_string: &str,
    ) -> Result<WorkerRecord, String> {
        let _guard = self.merge_lock.lock().await;
        let worker = self.worker(worker_id).await?;
        if worker.status != WorkerStatus::InReview {
            return Err("review_fix requires a worker in review".to_string());
        }
        let review_tip = worker
            .review
            .as_ref()
            .map(|review| review.tip.as_str())
            .ok_or_else(|| "review_fix requires a fresh delta review".to_string())?;
        let current_tip = phase3::tip(&worker.worktree).await?;
        if review_tip != current_tip {
            return Err("review_fix requires a review of the current worker tip".to_string());
        }
        phase3::require_clean(&worker.worktree, "worker").await?;
        let finding = worker
            .findings
            .iter()
            .find(|finding| finding.id == finding_id && finding.status == "open")
            .ok_or_else(|| format!("unknown open finding '{finding_id}'"))?;
        if finding.file != relative {
            return Err("review_fix must edit the finding's source file".to_string());
        }
        if old_string.is_empty() || old_string == new_string {
            return Err("review_fix needs a non-empty, distinct replacement".to_string());
        }
        if old_string.lines().count() + new_string.lines().count() > 100 {
            return Err(
                "review_fix is limited to a tiny replacement; route this finding to the worker"
                    .to_string(),
            );
        }
        let path = phase3::review_file(&worker.worktree, relative)?;
        let original = std::fs::read_to_string(&path)
            .map_err(|err| format!("could not read {}: {err}", path.display()))?;
        if original.matches(old_string).count() != 1 {
            return Err("review_fix old_string must occur exactly once".to_string());
        }
        let updated = original.replacen(old_string, new_string, 1);
        std::fs::write(&path, updated)
            .map_err(|err| format!("could not write {}: {err}", path.display()))?;
        let add = match phase3::git(&worker.worktree, &["add", "--", relative]).await {
            Ok(output) => output,
            Err(err) => {
                let _ = std::fs::write(&path, &original);
                return Err(err);
            }
        };
        if !add.status.success() {
            let _ = phase3::git(&worker.worktree, &["reset", "--", relative]).await;
            let _ = std::fs::write(&path, &original);
            return Err(phase3::git_error(&["add", "--", relative], &add));
        }
        let message = format!("review: fix {finding_id}");
        let commit = match phase3::git(&worker.worktree, &["commit", "-m", &message]).await {
            Ok(output) => output,
            Err(err) => {
                let _ = phase3::git(&worker.worktree, &["reset", "--", relative]).await;
                let _ = std::fs::write(&path, &original);
                return Err(err);
            }
        };
        if !commit.status.success() {
            let _ = phase3::git(&worker.worktree, &["reset", "--", relative]).await;
            let _ = std::fs::write(&path, &original);
            return Err(phase3::git_error(&["commit", "-m", &message], &commit));
        }
        let fix_tip = phase3::tip(&worker.worktree).await?;
        self.update_worker(worker_id, |worker| {
            if let Some(finding) = worker
                .findings
                .iter_mut()
                .find(|item| item.id == finding_id)
            {
                finding.status = "fixed".to_string();
                finding.fix_commit = Some(fix_tip.clone());
                finding.evidence = Some(format!("review_fix committed {fix_tip}"));
                finding.justification = None;
            }
            worker.checks.clear();
            worker.fix_rounds += 1;
            worker.review = None;
            worker.fixer_finding_id = None;
            worker.error = None;
        })
        .await;
        self.persist().await?;
        self.worker(worker_id).await
    }

    pub(super) async fn worker_is_busy(&self, worker_id: &str) -> bool {
        self.live
            .lock()
            .await
            .get(worker_id)
            .is_some_and(|live| live.busy.load(Ordering::SeqCst))
    }

    async fn settle_fix_downshift(
        &self,
        worker_id: &str,
        mut row: DispatchRoutingRow,
        finding_closed: bool,
    ) -> Result<WorkerRecord, String> {
        let (ts, from_tier, from_model) = match &row {
            DispatchRoutingRow::LeadReroute {
                ts,
                from_tier,
                from_model,
                ..
            } => (*ts, from_tier.clone(), from_model.clone()),
            _ => return self.worker(worker_id).await,
        };
        if let DispatchRoutingRow::LeadReroute { nudged, .. } = &mut row {
            *nudged = false;
        }
        self.update_worker(worker_id, |worker| {
            if let Some(DispatchRoutingRow::LeadReroute { nudged, .. }) = worker
                .reroutes
                .iter_mut()
                .rev()
                .find(|candidate| {
                    matches!(
                        candidate,
                        DispatchRoutingRow::LeadReroute { ts: candidate_ts, .. }
                            if *candidate_ts == ts
                    )
                })
            {
                *nudged = false;
            }
        })
        .await;
        self.persist().await?;
        self.append_reroute_row(&row, ChangeSource::FixDownshift, Some(finding_closed));
        if finding_closed {
            return self.worker(worker_id).await;
        }

        // The cheaper round did not close its finding: restore the prior
        // model now, while idle, so the next fix round starts there.
        let model = match from_model {
            Some(model) => Some(model),
            None if NUDGE_LADDER.contains(&from_tier.as_str()) => self
                .resolve_tier_choice(&from_tier)
                .map(|(_, choice)| Some(choice.spec))?,
            None => None,
        };
        let effort = self
            .tier_ladder()
            .get(&from_tier)
            .and_then(|rung| rung.effort.clone());
        let status = self.worker(worker_id).await?.status;
        self.change_model_locked(
            worker_id,
            model,
            effort,
            ChangeSource::FixDownshiftRestore,
        )
        .await?;
        self.update_worker(worker_id, |worker| worker.status = status)
            .await;
        self.persist().await?;
        self.worker(worker_id).await
    }

    fn finding_is_shared_contract(&self, worker: &WorkerRecord, finding: &Finding) -> bool {
        let Some(plan_id) = worker.plan_id.as_deref() else {
            return false;
        };
        let Some(board) = self.board() else {
            return false;
        };
        let Ok(markdown) = board.read_markdown(plan_id) else {
            return false;
        };
        let mut interfaces = false;
        for line in markdown.lines() {
            if line.starts_with("## ") {
                interfaces = line.trim() == "## Interfaces";
            } else if interfaces && line.contains(&finding.file) {
                return true;
            }
        }
        false
    }

    /// Classify a routed finding for `team_route_fix(tier: "auto")`. The
    /// classification is logged for shadow/gate diagnostics but is not an
    /// ordinary dispatch-training row (`skip` stays set).
    async fn classify_fix_tier(
        &self,
        worker: &WorkerRecord,
        finding: &Finding,
    ) -> Result<Option<(String, LeadModel)>, String> {
        let settings = self
            .routing
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if settings.mode == RoutingMode::Off {
            return Err("tier 'auto' needs routing mode shadow, suggest, or auto".to_string());
        }
        let text = routing_text(
            &finding.title,
            &format!("{}\nFile: {}", finding.detail, finding.file),
        )
        .unwrap_or_default();
        let enabled = self.enabled_routing_tiers();
        let classifier = self.classifier.as_ref().and_then(|slot| {
            slot.read()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        });
        let decision = if let Some(classifier) = classifier {
            let attempt = async {
                let _permit = self
                    .routing_permit
                    .acquire()
                    .await
                    .map_err(|_| "Laya routing is closed".to_string())?;
                Ok::<_, String>(
                    classify_dispatch(
                        classifier.as_ref(),
                        self.route_knn.as_deref(),
                        &text,
                        enabled,
                        &settings,
                    )
                    .await,
                )
            };
            match tokio::time::timeout(std::time::Duration::from_secs(3), attempt).await {
                Ok(result) => result?,
                Err(_) => None,
            }
        } else {
            None
        };
        let current_tier = worker
            .tier
            .clone()
            .or_else(|| worker.routing.as_ref().and_then(|routing| routing.ran_tier.clone()));
        let protected = finding.severity == "high" || self.finding_is_shared_contract(worker, finding);
        let mut selected = None;
        let skip = if decision.is_none() {
            "fix_downshift_offline"
        } else if !decision.as_ref().is_some_and(|decision| decision.cleared) {
            "fix_downshift_below_gate"
        } else if settings.mode == RoutingMode::Shadow {
            "fix_downshift_shadow"
        } else if protected {
            "fix_downshift_protected"
        } else if let Some(tier) = decision.as_ref().and_then(|decision| decision.tier) {
            let (ran_tier, choice) = self.resolve_tier_choice(tier)?;
            let cheaper = current_tier.as_deref().is_some_and(|current| {
                NUDGE_LADDER.iter().position(|candidate| *candidate == ran_tier)
                    < NUDGE_LADDER.iter().position(|candidate| *candidate == current)
            });
            if cheaper {
                selected = Some((ran_tier, choice));
                "fix_downshift_selected"
            } else {
                "fix_downshift_not_cheaper"
            }
        } else {
            "fix_downshift_unknown_label"
        };
        use sha2::Digest as _;
        let text_sha256 = sha2::Sha256::digest(text.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let ran_model = selected
            .as_ref()
            .map(|(_, choice)| choice.spec.clone())
            .or_else(|| worker.model.clone());
        let row = DispatchRoutingRow::Route {
            ts: unix_ms(),
            worker_id: worker.id.clone(),
            plan_id: worker.plan_id.clone().unwrap_or_default(),
            step: worker.plan_step.map_or(0, |step| step + 1),
            mode: settings.mode,
            text_sha256,
            label: decision
                .as_ref()
                .map(|decision| decision.label.clone())
                .unwrap_or_default(),
            confidence: decision.as_ref().map_or(0.0, |decision| decision.confidence),
            probabilities: decision
                .as_ref()
                .map(|decision| decision.probabilities.clone())
                .unwrap_or_default(),
            tier: decision.as_ref().and_then(|decision| decision.tier).map(str::to_string),
            threshold: decision.as_ref().map_or_else(
                || threshold_for(enabled.offered().count(), &settings),
                |decision| {
                    if decision.source == "knn" {
                        settings.knn_min_share
                    } else {
                        threshold_for(enabled.offered().count(), &settings)
                    }
                },
            ),
            cleared: decision.as_ref().is_some_and(|decision| decision.cleared),
            lead_tier: current_tier.clone(),
            ran_tier: selected
                .as_ref()
                .map(|(tier, _)| tier.clone())
                .or(current_tier),
            ran_model: ran_model.clone(),
            text,
            skip: Some(skip.to_string()),
            source: decision.as_ref().map(|decision| decision.source.to_string()),
            knn_label: decision.as_ref().and_then(|decision| decision.knn_label.clone()),
            knn_share: decision.as_ref().and_then(|decision| decision.knn_share),
            knn_k: decision.as_ref().and_then(|decision| decision.knn_k),
        };
        self.append_dispatch_routing_row(&row);
        Ok(selected)
    }

    /// Route a non-tiny finding to the original tracked worker.
    pub async fn route_fix(
        &self,
        worker_id: &str,
        finding_id: &str,
    ) -> Result<WorkerRecord, String> {
        self.route_fix_with_tier(worker_id, finding_id, None).await
    }

    /// Route a fix on its current model or an explicitly/classifier-selected
    /// ladder tier. The worker keeps its session and transcript.
    pub async fn route_fix_with_tier(
        &self,
        worker_id: &str,
        finding_id: &str,
        tier: Option<&str>,
    ) -> Result<WorkerRecord, String> {
        let _guard = self.merge_lock.lock().await;
        let worker = self.worker(worker_id).await?;
        // Audits only ever produce findings; they never receive them.
        debug_assert_ne!(
            worker.kind,
            "audit",
            "route_fix never targets an audit worker"
        );
        if matches!(worker.status, WorkerStatus::Merged | WorkerStatus::Merging) {
            return Err("a merged worker cannot receive a fixer".to_string());
        }
        let finding = worker
            .findings
            .iter()
            .find(|finding| finding.id == finding_id && finding.status == "open")
            .ok_or_else(|| format!("unknown open finding '{finding_id}'"))?
            .clone();
        if self.worker_is_busy(worker_id).await {
            return Err("the worker must be idle before routing a review fix".to_string());
        }
        if !self.live.lock().await.contains_key(worker_id) {
            self.resume_inner(worker_id).await?;
        }
        let previous = self.worker(worker_id).await?;
        let selected = match tier.map(str::trim).filter(|tier| !tier.is_empty()) {
            None => None,
            Some("auto") => self.classify_fix_tier(&previous, &finding).await?,
            Some(tier) => Some(self.resolve_tier_choice(tier)?),
        };
        if let Some((ran_tier, choice)) = selected {
            let current_tier = previous
                .tier
                .as_deref()
                .or_else(|| previous.routing.as_ref().and_then(|routing| routing.ran_tier.as_deref()));
            let cheaper = current_tier.is_some_and(|current| {
                NUDGE_LADDER.iter().position(|candidate| *candidate == ran_tier)
                    < NUDGE_LADDER.iter().position(|candidate| *candidate == current)
            });
            let effort = self
                .tier_ladder()
                .get(&ran_tier)
                .and_then(|rung| rung.effort.clone());
            self.change_model_locked(
                worker_id,
                Some(choice.spec),
                effort,
                if cheaper {
                    ChangeSource::FixDownshift
                } else {
                    ChangeSource::Lead
                },
            )
            .await?;
        }
        self.update_worker(worker_id, |worker| {
            worker.status = WorkerStatus::Fixing;
            worker.fixer_finding_id = Some(finding_id.to_string());
            worker.checks.clear();
            worker.fix_rounds += 1;
            worker.review = None;
        })
        .await;
        self.persist().await?;
        let brief = route_fix_brief(&worker, &finding);
        if let Err(err) = self
            .message_inner_with_kind(worker_id, brief, MessageSender::Mnemo, Some(InboxKind::Fix))
            .await
        {
            self.update_worker(worker_id, |worker| *worker = previous)
                .await;
            self.persist().await?;
            return Err(err);
        }
        self.worker(worker_id).await
    }
}
/// Drop a review and its checks when this transcript snapshot saw a worker
/// commit that the review does not cover.
///
/// `tip_read_ms` is the moment before the snapshot's `git rev-parse` ran. A
/// review recorded at or after it describes whatever `HEAD` is now, so a tip
/// this probe read earlier cannot make it stale: the review merely landed
/// while the probe was in flight. Before that guard a slow runner let the lead
/// commit and review inside that window, the refresh dropped both, and the
/// merge failed with "the current worker tip has not been reviewed" (macOS CI
/// run 42071163294). Keeping such a review is safe — [`validate_merge`]
/// re-reads the live tip and every check tip before it merges.
pub(super) fn clear_review_for_a_newer_tip(
    worker: &mut WorkerRecord,
    current_tip: Option<&str>,
    tip_read_ms: u128,
) {
    let review_is_newer = worker
        .review
        .as_ref()
        .is_some_and(|review| review.reviewed_at_ms >= tip_read_ms);
    if review_is_newer {
        return;
    }
    let tip_moved = current_tip.is_some_and(|tip| {
        worker
            .review
            .as_ref()
            .is_some_and(|review| review.tip != tip)
            || worker.checks.iter().any(|check| check.tip != tip)
    });
    if tip_moved {
        worker.review = None;
        worker.checks.clear();
    }
}

