// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]

//! The lead's [`TeamLedger`] over the real worker records.
//!
//! The engine's TeamLead workflow reads the team through this seam: whether a
//! worker turn is live, which tasks a plan still has open, and the plan's team
//! review report that `finish` writes under `.coding/reviews/`. The pure
//! functions work on a slice of [`WorkerRecord`]s so they are testable without
//! a runtime; [`LeadTeamLedger`] resolves the lead's *current* team per call
//! because the lead replaces its `TeamRuntime` when the project changes.

use std::fmt::Write as _;
use std::sync::Arc;

use async_trait::async_trait;
use mnemo::app::team_ledger::{OpenTask, PlanReview, TeamLedger};
use tokio::sync::Mutex;

use crate::mnemo2::dispatch_routing::DispatchRoutingRow;
use crate::mnemo2::team::{TeamRuntime, WorkerRecord, WorkerStatus};

/// The lead's team slot, shared with `LeadRuntime`.
pub(crate) type TeamSlot = Arc<Mutex<Option<Arc<TeamRuntime>>>>;

/// A worker turn is running in these states; a TeamManager notice wakes the
/// lead when it ends.
fn is_live_turn(status: WorkerStatus) -> bool {
    matches!(
        status,
        WorkerStatus::Dispatching
            | WorkerStatus::Working
            | WorkerStatus::Fixing
            | WorkerStatus::Merging
    )
}

/// A task is closed when its work is integrated or deliberately abandoned.
/// Only the user's workspace cleanup moves a worker to `Cancelled`; the lead's
/// own `team_stop` ends in `Stopped`, so that closes a task too (it is listed
/// as not merged in the review).
pub(crate) fn is_closed(worker: &WorkerRecord) -> bool {
    matches!(
        worker.status,
        WorkerStatus::Merged | WorkerStatus::Cancelled | WorkerStatus::Stopped
    )
}

pub(crate) fn merged(worker: &WorkerRecord) -> bool {
    worker.status == WorkerStatus::Merged
        && worker
            .merge
            .as_ref()
            .is_some_and(|merge| merge.revert_commit.is_none())
}

fn status_label(status: WorkerStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{status:?}").to_lowercase())
}

/// How many worker turns are running now.
pub(crate) fn live_turns(workers: &[WorkerRecord]) -> usize {
    workers.iter().filter(|w| is_live_turn(w.status)).count()
}

/// The tasks stamped with `plan_id` (and `step`, when given) that are open.
pub(crate) fn open_tasks(
    workers: &[WorkerRecord],
    plan_id: &str,
    step: Option<usize>,
) -> Vec<OpenTask> {
    workers
        .iter()
        .filter(|w| w.plan_id.as_deref() == Some(plan_id))
        .filter(|w| step.is_none() || w.plan_step == step)
        .filter(|w| !is_closed(w))
        .map(|w| OpenTask {
            worker_id: w.id.clone(),
            name: w.name.clone(),
            status: status_label(w.status),
            step_index: w.plan_step,
        })
        .collect()
}

/// The plan's team review: blocked while a task or finding is open, otherwise
/// a PASS report.
pub(crate) fn plan_review(workers: &[WorkerRecord], plan_id: &str, plan_title: &str) -> PlanReview {
    let tasks: Vec<&WorkerRecord> = workers
        .iter()
        .filter(|w| w.plan_id.as_deref() == Some(plan_id))
        .collect();
    let mut reasons = Vec::new();
    for task in &tasks {
        if !is_closed(task) {
            reasons.push(format!(
                "worker {} ({}) is {} - merge or stop it",
                task.id,
                task.name,
                status_label(task.status)
            ));
        }
        // An abandoned task's findings no longer gate the plan.
        if !matches!(task.status, WorkerStatus::Stopped | WorkerStatus::Cancelled) {
            for finding in task.findings.iter().filter(|f| f.status == "open") {
                reasons.push(format!(
                    "worker {} has open finding {} ({}): {}",
                    task.id, finding.id, finding.severity, finding.title
                ));
            }
        }
    }
    if !reasons.is_empty() {
        return PlanReview::Blocked { reasons };
    }
    PlanReview::Ready {
        report: render_report(&tasks, plan_id, plan_title),
    }
}

/// The effort table: per task, the models it ran on, what Laya said at
/// dispatch and who routed it, and the review, fix, and audit rounds it
/// needed. Audit workers are reviewers, not tasks, so they get no row; the
/// counters read zero for a ledger written before they existed, and both
/// routing columns render `-` for a task dispatched before routing existed
/// or with routing off.
fn render_effort(out: &mut String, tasks: &[&WorkerRecord]) {
    out.push_str("\n## Effort\n\n");
    out.push_str(
        "| Task | Step | Models | Laya | Routed by | Review rounds | Fix rounds | Findings (high) | Audits |\n",
    );
    out.push_str("| --- | --- | --- | --- | --- | --- | --- | --- | --- |\n");
    let (mut reviews, mut fixes, mut audits, mut counted) = (0u32, 0u32, 0u32, 0usize);
    for task in tasks.iter().filter(|task| task.kind != "audit") {
        counted += 1;
        reviews += task.review_rounds;
        fixes += task.fix_rounds;
        audits += task.audits;
        let step = task
            .plan_step
            .map_or_else(|| "-".to_string(), |step| (step + 1).to_string());
        let models = if task.models_used.is_empty() {
            task.model_id
                .as_deref()
                .or(task.model.as_deref())
                .unwrap_or("default model")
                .to_string()
        } else {
            task.models_used.join(" → ")
        };
        let laya = task.routing.as_ref().map_or_else(
            || "-".to_string(),
            |routing| {
                match (routing.laya_tier.as_deref(), routing.confidence, routing.cleared) {
                    (Some(tier), Some(confidence), true) => format!("{tier} {confidence:.2}"),
                    (_, Some(confidence), _) => format!("unsure {confidence:.2}"),
                    _ => "-".to_string(),
                }
            },
        );
        // A reroute that keeps the tier never counts as an override: the
        // column reports who picked the tier, and any hand that moved it
        // afterwards.
        let mut routed_by = task.routing.as_ref().map_or_else(
            || "-".to_string(),
            |routing| match routing.routed_by.as_str() {
                "lead_override" => "lead (override)".to_string(),
                other => other.to_string(),
            },
        );
        if task
            .reroutes
            .iter()
            .any(|row| matches!(row, DispatchRoutingRow::UserReroute { .. }))
        {
            routed_by.push_str(" + user reroute");
        }
        if task
            .reroutes
            .iter()
            .any(|row| matches!(row, DispatchRoutingRow::LeadReroute { .. }))
        {
            routed_by.push_str(" + lead reroute");
        }
        let high = task
            .findings
            .iter()
            .filter(|finding| finding.severity == "high")
            .count();
        let _ = writeln!(
            out,
            "| `{}` | {step} | {models} | {laya} | {routed_by} | {} | {} | {} ({high}) | {} |",
            task.id,
            task.review_rounds,
            task.fix_rounds,
            task.findings.len(),
            task.audits
        );
    }
    let _ = writeln!(
        out,
        "\nTotal: {reviews} review rounds, {fixes} fix rounds, {audits} audits across {counted} tasks."
    );
}

fn render_report(tasks: &[&WorkerRecord], plan_id: &str, plan_title: &str) -> String {
    let mut out = String::from("## Verdict: PASS\n");
    let _ = write!(out, "\n# Team review: {plan_title}\n\nPlan `{plan_id}`.\n");
    if tasks.is_empty() {
        out.push_str("\nNo worker tasks ran under this plan; the lead did the work directly.\n");
        return out;
    }

    out.push_str("\n## Tasks\n");
    for task in tasks {
        let model = task
            .model_id
            .as_deref()
            .or(task.model.as_deref())
            .unwrap_or("default model");
        let _ = write!(
            out,
            "\n- `{}` {} - model {model}, status {}",
            task.id,
            task.name,
            status_label(task.status)
        );
        if let Some(step) = task.plan_step {
            let _ = write!(out, ", plan step {}", step + 1);
        }
        if let Some(merge) = task.merge.as_ref().filter(|m| m.revert_commit.is_none()) {
            let _ = write!(out, ", merge commit `{}`", merge.commit);
            if merge.files > 0 || merge.additions > 0 || merge.deletions > 0 {
                let _ = write!(
                    out,
                    " ({} files, +{} -{})",
                    merge.files, merge.additions, merge.deletions
                );
            }
        }
        out.push('\n');
    }

    render_effort(&mut out, tasks);

    let unmerged: Vec<&&WorkerRecord> = tasks.iter().filter(|t| !merged(t)).collect();
    if !unmerged.is_empty() {
        out.push_str("\n## Not merged\n");
        for task in unmerged {
            let why = match (task.status, task.merge.as_ref()) {
                (WorkerStatus::Merged, Some(_)) => "merged, then reverted by the lead",
                (WorkerStatus::Stopped, _) => "stopped by the lead before integration",
                _ => "cancelled before integration",
            };
            let _ = writeln!(out, "\n- `{}` {}: {why}", task.id, task.name);
        }
    }

    let findings: Vec<(&WorkerRecord, &crate::mnemo2::phase3::Finding)> = tasks
        .iter()
        .flat_map(|t| t.findings.iter().map(move |f| (*t, f)))
        .collect();
    out.push_str("\n## Findings\n");
    if findings.is_empty() {
        out.push_str("\nNo findings were recorded.\n");
    }
    for (task, finding) in findings {
        let _ = write!(
            out,
            "\n- `{}` / {} [{}] {} ({}:{}) - {}",
            task.id,
            finding.id,
            finding.severity,
            finding.title,
            finding.file,
            finding.line,
            finding.status
        );
        if let Some(evidence) = finding.evidence.as_deref().filter(|e| !e.is_empty()) {
            let _ = write!(out, "; evidence: {evidence}");
        }
        if let Some(why) = finding.justification.as_deref().filter(|j| !j.is_empty()) {
            let _ = write!(out, "; wontfix justification: {why}");
        }
        out.push('\n');
    }

    out.push_str("\n## Checks\n");
    let mut any_check = false;
    for task in tasks.iter().filter(|t| merged(t)) {
        let Some(merge) = task.merge.as_ref() else {
            continue;
        };
        for check in task.checks.iter().filter(|c| c.tip == merge.worker_tip) {
            any_check = true;
            let _ = write!(
                out,
                "\n- `{}` {} on tip `{}`: {}",
                task.id, check.name, merge.worker_tip, check.status
            );
            if let Some(detail) = check.detail.as_deref().filter(|d| !d.is_empty()) {
                let _ = write!(out, " ({detail})");
            }
            out.push('\n');
        }
        if task.checks.iter().all(|c| c.tip != merge.worker_tip) && !merge.checks.is_empty() {
            any_check = true;
            let _ = writeln!(
                out,
                "\n- `{}` merge gate checks: {}",
                task.id,
                merge.checks.join(", ")
            );
        }
    }
    if !any_check {
        out.push_str("\nNo exact-tip checks were recorded for merged tasks.\n");
    }
    out
}

/// The [`TeamLedger`] handed to the TeamLead policy: it resolves the lead's
/// current team at call time (no team: nothing live, nothing open, no tasks).
pub(crate) struct LeadTeamLedger {
    team: TeamSlot,
}

impl LeadTeamLedger {
    pub(crate) fn new(team: TeamSlot) -> Self {
        Self { team }
    }

    async fn workers(&self) -> Vec<WorkerRecord> {
        let team = self.team.lock().await.clone();
        match team {
            Some(team) => team.snapshot().await.workers,
            None => Vec::new(),
        }
    }
}

#[async_trait]
impl TeamLedger for LeadTeamLedger {
    async fn live_worker_turns(&self) -> usize {
        live_turns(&self.workers().await)
    }

    async fn open_tasks(&self, plan_id: &str, step_index: Option<usize>) -> Vec<OpenTask> {
        open_tasks(&self.workers().await, plan_id, step_index)
    }

    async fn plan_review(&self, plan_id: &str, plan_title: &str) -> PlanReview {
        plan_review(&self.workers().await, plan_id, plan_title)
    }
}

/// A [`TeamLedger`] over a team's own record list. The team uses it to close
/// plans after a merge, with no round trip through the lead's team slot.
pub(crate) struct RecordsLedger {
    records: Arc<Mutex<Vec<WorkerRecord>>>,
}

impl RecordsLedger {
    pub(crate) fn new(records: Arc<Mutex<Vec<WorkerRecord>>>) -> Self {
        Self { records }
    }
}

#[async_trait]
impl TeamLedger for RecordsLedger {
    async fn live_worker_turns(&self) -> usize {
        live_turns(&self.records.lock().await)
    }

    async fn open_tasks(&self, plan_id: &str, step_index: Option<usize>) -> Vec<OpenTask> {
        open_tasks(&self.records.lock().await, plan_id, step_index)
    }

    async fn plan_review(&self, plan_id: &str, plan_title: &str) -> PlanReview {
        plan_review(&self.records.lock().await, plan_id, plan_title)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mnemo2::phase3::{Finding, MergeRecord};
    use crate::mnemo2::team::{WorkerCheck, WorkerRouting};
    use serde_json::json;

    fn worker(id: &str, status: &str, plan: Option<&str>, step: Option<usize>) -> WorkerRecord {
        serde_json::from_value(json!({
            "id": id,
            "name": format!("Worker {id}"),
            "brief": "task",
            "model": "test/scripted-a",
            "branch": format!("wt/agent-{id}"),
            "worktree": "",
            "status": status,
            "generation": 1,
            "transcript": { "messages": [] },
            "pending": null,
            "used_tokens": 0,
            "max_tokens": 0,
            "checks": [],
            "error": null,
            "plan_id": plan,
            "plan_step": step,
        }))
        .expect("worker record")
    }

    fn merge_record() -> MergeRecord {
        MergeRecord {
            commit: "abc1234".into(),
            worker_tip: "tip5678".into(),
            branch: "wt/agent-w1".into(),
            target: "wt/lead".into(),
            checks: vec!["cargo test".into()],
            review_summary: "ok".into(),
            review_risk: None,
            review_evidence: None,
            files: 3,
            additions: 20,
            deletions: 4,
            worker_model: None,
            worker_tokens: 0,
            cost_micros: None,
            merged_at_ms: 1,
            revert_commit: None,
        }
    }

    fn finding(id: &str, status: &str) -> Finding {
        Finding {
            id: id.into(),
            severity: "high".into(),
            title: "missing guard".into(),
            detail: "d".into(),
            file: "src/lib.rs".into(),
            line: 7,
            status: status.into(),
            fix_commit: None,
            evidence: (status == "fixed").then(|| "test passes".to_string()),
            justification: (status == "wontfix").then(|| "out of scope".to_string()),
        }
    }

    #[test]
    fn live_turns_count_only_running_states() {
        let workers = [
            worker("a", "dispatching", None, None),
            worker("b", "working", None, None),
            worker("c", "fixing", None, None),
            worker("d", "merging", None, None),
            worker("e", "completed", None, None),
            worker("f", "needs_you", None, None),
            worker("g", "idle", None, None),
        ];
        assert_eq!(live_turns(&workers), 4);
    }

    #[test]
    fn open_tasks_filter_by_plan_step_and_closed_states() {
        let workers = [
            worker("a", "working", Some("p1"), Some(0)),
            worker("b", "completed", Some("p1"), Some(1)),
            worker("c", "merged", Some("p1"), Some(0)),
            worker("d", "stopped", Some("p1"), Some(0)),
            worker("e", "cancelled", Some("p1"), Some(0)),
            worker("f", "working", Some("p2"), Some(0)),
            worker("g", "working", None, None),
        ];
        let all = open_tasks(&workers, "p1", None);
        assert_eq!(
            all.iter().map(|t| t.worker_id.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
        let step0 = open_tasks(&workers, "p1", Some(0));
        assert_eq!(step0.len(), 1);
        assert_eq!(step0[0].worker_id, "a");
        assert_eq!(step0[0].status, "working");
        assert_eq!(step0[0].step_index, Some(0));
        assert!(open_tasks(&workers, "none", None).is_empty());
    }

    #[test]
    fn review_with_no_tasks_is_ready_and_says_so() {
        let PlanReview::Ready { report } = plan_review(&[], "p1", "Ship it") else {
            panic!("an empty plan is ready");
        };
        assert!(report.starts_with("## Verdict: PASS\n"), "{report}");
        assert!(report.contains("Ship it") && report.contains("p1"), "{report}");
        assert!(report.contains("No worker tasks ran under this plan"), "{report}");
    }

    #[test]
    fn review_blocks_on_open_tasks_and_open_findings() {
        let mut flagged = worker("m", "merged", Some("p1"), Some(0));
        flagged.merge = Some(merge_record());
        flagged.findings = vec![finding("F1", "open")];
        let workers = [worker("w", "in_review", Some("p1"), Some(0)), flagged];
        let PlanReview::Blocked { reasons } = plan_review(&workers, "p1", "T") else {
            panic!("open work blocks");
        };
        assert_eq!(reasons.len(), 2, "{reasons:?}");
        assert!(reasons[0].contains("worker w") && reasons[0].contains("in_review"), "{reasons:?}");
        assert!(reasons[1].contains("worker m") && reasons[1].contains("F1"), "{reasons:?}");
    }

    #[test]
    fn review_report_lists_tasks_findings_checks_and_unmerged() {
        let mut done = worker("w1", "merged", Some("p1"), Some(0));
        done.model_id = Some("scripted-a".into());
        done.merge = Some(merge_record());
        done.findings = vec![finding("F1", "fixed"), finding("F2", "wontfix")];
        done.checks = vec![
            WorkerCheck {
                name: "cargo test".into(),
                status: "passed".into(),
                detail: Some("12 ok".into()),
                tip: "tip5678".into(),
            },
            WorkerCheck {
                name: "stale".into(),
                status: "passed".into(),
                detail: None,
                tip: "older".into(),
            },
        ];
        let abandoned = worker("w2", "stopped", Some("p1"), Some(1));
        let other_plan = worker("w3", "working", Some("p2"), Some(0));
        let PlanReview::Ready { report } =
            plan_review(&[done, abandoned, other_plan], "p1", "Ship it")
        else {
            panic!("closed work is ready");
        };
        assert!(report.starts_with("## Verdict: PASS\n"), "{report}");
        assert!(report.contains("`w1`") && report.contains("scripted-a"), "{report}");
        assert!(report.contains("merge commit `abc1234`"), "{report}");
        assert!(report.contains("3 files, +20 -4"), "{report}");
        assert!(report.contains("evidence: test passes"), "{report}");
        assert!(report.contains("wontfix justification: out of scope"), "{report}");
        assert!(report.contains("cargo test on tip `tip5678`: passed (12 ok)"), "{report}");
        assert!(!report.contains("stale"), "{report}");
        assert!(report.contains("## Not merged"), "{report}");
        assert!(report.contains("`w2`") && report.contains("stopped by the lead"), "{report}");
        assert!(!report.contains("`w3`"), "{report}");
    }

    #[test]
    fn review_report_lists_effort_per_task() {
        let mut first = worker("w1", "merged", Some("p1"), Some(0));
        first.merge = Some(merge_record());
        first.review_rounds = 2;
        first.fix_rounds = 1;
        first.audits = 1;
        first.models_used = vec!["test/scripted-a".into(), "test/scripted-b".into()];
        first.findings = vec![finding("F1", "fixed")];
        let mut second = worker("w2", "merged", Some("p1"), Some(1));
        second.merge = Some(merge_record());
        second.review_rounds = 1;
        // An audit worker is a reviewer, never a task: it gets no effort row
        // even when it is stamped with the plan.
        let mut audit = worker("r1", "merged", Some("p1"), None);
        audit.kind = "audit".to_string();
        audit.audit_target = Some("w1".to_string());
        audit.review_rounds = 9;
        audit.fix_rounds = 9;
        audit.audits = 9;
        audit.models_used = vec!["test/scripted-z".into()];
        let PlanReview::Ready { report } = plan_review(&[first, second, audit], "p1", "Ship it")
        else {
            panic!("closed work is ready");
        };
        assert!(report.contains("\n## Effort\n"), "{report}");
        assert!(
            report.contains(
                "| Task | Step | Models | Laya | Routed by | Review rounds | Fix rounds | Findings (high) | Audits |"
            ),
            "{report}"
        );
        assert!(
            report.contains("| `w1` | 1 | test/scripted-a → test/scripted-b | - | - | 2 | 1 | 1 (1) | 1 |"),
            "{report}"
        );
        assert!(
            report.contains("| `w2` | 2 | test/scripted-a | - | - | 1 | 0 | 0 (0) | 0 |"),
            "{report}"
        );
        assert!(!report.contains("| `r1` |"), "{report}");
        assert!(!report.contains("scripted-z"), "{report}");
        assert!(
            report.contains("Total: 3 review rounds, 1 fix rounds, 1 audits across 2 tasks."),
            "{report}"
        );
    }

    #[test]
    fn old_records_without_counters_render_zero_effort() {
        let legacy: WorkerRecord = serde_json::from_value(json!({
            "id": "old1",
            "name": "Worker old1",
            "brief": "task",
            "model": "test/scripted-a",
            "branch": "wt/agent-old1",
            "worktree": "",
            "status": "merged",
            "generation": 1,
            "transcript": { "messages": [] },
            "pending": null,
            "used_tokens": 0,
            "max_tokens": 0,
            "checks": [],
            "error": null,
            "plan_id": "p1",
            "plan_step": 0,
        }))
        .expect("a record written before the effort counters existed");
        assert_eq!(legacy.review_rounds, 0);
        assert_eq!(legacy.fix_rounds, 0);
        assert_eq!(legacy.audits, 0);
        assert!(legacy.models_used.is_empty());
        let PlanReview::Ready { report } = plan_review(&[legacy], "p1", "T") else {
            panic!("a merged task is closed");
        };
        assert!(
            report.contains("| `old1` | 1 | test/scripted-a | - | - | 0 | 0 | 0 (0) | 0 |"),
            "{report}"
        );
        assert!(
            report.contains("Total: 0 review rounds, 0 fix rounds, 0 audits across 1 tasks."),
            "{report}"
        );
    }

    #[test]
    fn effort_table_shows_laya_and_routed_by() {
        let mut lead_pick = worker("w1", "merged", Some("p1"), Some(0));
        lead_pick.merge = Some(merge_record());
        lead_pick.routing = Some(WorkerRouting {
            laya_tier: Some("routine".into()),
            confidence: Some(0.62),
            cleared: true,
            lead_tier: Some("routine".into()),
            routed_by: "lead".into(),
            ran_tier: Some("routine".into()),
            nudged: None,
        });
        lead_pick.reroutes = vec![DispatchRoutingRow::UserReroute {
            ts: 2,
            worker_id: "w1".into(),
            plan_id: "p1".into(),
            step: 1,
            from_tier: "routine".into(),
            to_tier: "high".into(),
            from_model: Some("test/scripted-a".into()),
            to_model: Some("test/scripted-difficult".into()),
            status: "idle".into(),
            review_rounds: 0,
            fix_rounds: 0,
            nudged: false,
        }];
        let mut laya_pick = worker("w2", "merged", Some("p1"), Some(1));
        laya_pick.merge = Some(merge_record());
        laya_pick.routing = Some(WorkerRouting {
            laya_tier: Some("high".into()),
            confidence: Some(0.41),
            cleared: false,
            lead_tier: None,
            routed_by: "laya".into(),
            ran_tier: None,
            nudged: None,
        });
        let mut override_pick = worker("w3", "merged", Some("p1"), Some(2));
        override_pick.merge = Some(merge_record());
        override_pick.routing = Some(WorkerRouting {
            laya_tier: Some("high".into()),
            confidence: Some(0.91),
            cleared: true,
            lead_tier: Some("routine".into()),
            routed_by: "lead_override".into(),
            ran_tier: Some("routine".into()),
            nudged: None,
        });
        override_pick.reroutes = vec![DispatchRoutingRow::LeadReroute {
            ts: 3,
            worker_id: "w3".into(),
            plan_id: "p1".into(),
            step: 3,
            from_tier: "routine".into(),
            to_tier: "high".into(),
            from_model: Some("test/scripted-a".into()),
            to_model: Some("test/scripted-difficult".into()),
            status: "idle".into(),
            review_rounds: 0,
            fix_rounds: 0,
            nudged: false,
        }];
        let PlanReview::Ready { report } =
            plan_review(&[lead_pick, laya_pick, override_pick], "p1", "Ship it")
        else {
            panic!("closed work is ready");
        };
        assert!(
            report.contains(
                "| `w1` | 1 | test/scripted-a | routine 0.62 | lead + user reroute | 0 | 0 | 0 (0) | 0 |"
            ),
            "{report}"
        );
        assert!(
            report.contains("| `w2` | 2 | test/scripted-a | unsure 0.41 | laya | 0 | 0 | 0 (0) | 0 |"),
            "{report}"
        );
        assert!(
            report.contains(
                "| `w3` | 3 | test/scripted-a | high 0.91 | lead (override) + lead reroute | 0 | 0 | 0 (0) | 0 |"
            ),
            "{report}"
        );
    }

    #[test]
    fn old_records_render_dashes_for_laya() {
        let mut legacy = worker("old2", "merged", Some("p1"), Some(0));
        legacy.merge = Some(merge_record());
        let PlanReview::Ready { report } = plan_review(&[legacy], "p1", "T") else {
            panic!("a merged task is closed");
        };
        // A record from before routing existed renders dashes in both
        // routing columns, and never in the counter columns it predates.
        assert!(
            report.contains("| `old2` | 1 | test/scripted-a | - | - | 0 | 0 | 0 (0) | 0 |"),
            "{report}"
        );
    }

    #[test]
    fn a_reverted_merge_is_not_merged() {
        let mut reverted = worker("w1", "merged", Some("p1"), Some(0));
        let mut merge = merge_record();
        merge.revert_commit = Some("rev999".into());
        reverted.merge = Some(merge);
        let PlanReview::Ready { report } = plan_review(&[reverted], "p1", "T") else {
            panic!("a reverted task is closed");
        };
        assert!(report.contains("merged, then reverted"), "{report}");
        assert!(!report.contains("merge commit"), "{report}");
    }

    #[tokio::test]
    async fn the_ledger_without_a_team_is_empty() {
        let ledger = LeadTeamLedger::new(Arc::new(Mutex::new(None)));
        assert_eq!(ledger.live_worker_turns().await, 0);
        assert!(ledger.open_tasks("p", None).await.is_empty());
        assert!(matches!(ledger.plan_review("p", "T").await, PlanReview::Ready { .. }));
    }
}
