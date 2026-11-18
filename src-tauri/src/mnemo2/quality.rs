// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]

//! Does merged work hold up? The quality report behind the Quality tab and the
//! lead's `team_quality` tool (plan dac2028d).
//!
//! The routing log already records every dispatch's outcome. This module reads
//! those rows back and answers, per final model and per final tier: how often a
//! merge was first-pass, how much fixing and rework it took, how often it was
//! reverted, and how often a later bug-fix plan merged a change in the same
//! files soon after. The last two are the signals that work only *looked*
//! done.
//!
//! Sources: `outcome` and `reverted` rows of the dispatch routing log, the plan
//! kind from `.coding/plans/<plan_id>.md`, the changed files of a merge from
//! git, and the recurring finding categories from `.coding/reviews/findings.jsonl`.
//! Only `merged` outcomes are tasks; audit workers write no outcome row at all.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::mnemo2::dispatch_routing::DispatchRoutingRow;
use crate::mnemo2::edit_outcomes::{self, EditFileRow, EditOutcomeRow, EditOutcomes};
use crate::mnemo2::team::findings_ledger;
use crate::mnemo2::{dispatch_routing, phase3};

/// One day in the log's millisecond timestamps.
const DAY_MS: u64 = 86_400_000;

/// The window the Quality tab opens with, and the default of both callers.
pub const DEFAULT_WINDOW_DAYS: u32 = 30;

/// How soon after a merge a bug-fix merge in the same files still counts as a
/// follow-up: two weeks is long enough to catch the next real fix and short
/// enough not to blame a merge for later unrelated work.
const FOLLOWUP_DAYS: u64 = 14;

/// The most recurring finding categories the report carries.
const MAX_CATEGORIES: usize = 10;

/// One model's or one tier's record over the window.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QualityRow {
    /// The final tier name, or the final model spec string.
    pub key: String,
    /// Merges in the window.
    pub merged: u32,
    /// Share (0-1) of those merges that needed no fix round and had no high
    /// finding.
    pub first_pass: f64,
    /// Mean fix rounds per merge.
    pub avg_fix_rounds: f64,
    /// Mean findings per merge.
    pub avg_findings: f64,
    /// Findings recorded as high, summed over the merges.
    pub high_findings: u32,
    /// Merges a later `reverted` row reports for the same worker.
    pub reverted: u32,
    /// Merges a later bug-fix plan's merge changed files for, within
    /// [`FOLLOWUP_DAYS`].
    pub bugfix_followups: u32,
    /// Total cost of the merges divided by the merges.
    pub cost_per_merged_micros: u64,
}

/// The whole Quality tab's data.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QualityReport {
    /// The window the report was computed over.
    pub window_days: u32,
    /// Unix epoch milliseconds the report was generated at.
    pub generated_ms: u64,
    /// One row per final model, most merges first.
    pub by_model: Vec<QualityRow>,
    /// One row per final tier, most merges first.
    pub by_tier: Vec<QualityRow>,
    /// The window's most recurring finding categories, as (category, distinct
    /// workers). Empty when no review logged a category.
    pub categories: Vec<(String, u32)>,
    /// Merges whose row predates `merge_commit`, so a bug-fix follow-up cannot
    /// be detected for them. They count for every other metric.
    pub missing_merge_commit: u32,
    /// Edit-tool attempts by model, worker and file, read from the persisted
    /// worker transcripts (plan f89fd63d step 5). Empty in [`compute`], which
    /// never touches the filesystem; [`report`] fills it.
    pub edits: EditOutcomes,
}

/// One merged outcome in the window, as much of it as the metrics need.
struct Task<'a> {
    worker_id: &'a str,
    plan_id: &'a str,
    ts: u64,
    tier: &'a str,
    model: &'a str,
    fix_rounds: u32,
    findings: usize,
    findings_high: usize,
    cost_micros: u64,
    merge_commit: Option<&'a str>,
}

/// Running totals for one model or one tier.
#[derive(Default)]
struct Tally {
    merged: u32,
    first_pass: u32,
    fix_rounds: u64,
    findings: u64,
    high_findings: u64,
    reverted: u32,
    bugfix_followups: u32,
    cost_micros: u64,
}

impl Tally {
    /// Fold one task in; `reverted` and `bugfix_followup` are already decided.
    fn add(&mut self, task: &Task<'_>, reverted: bool, bugfix_followup: bool) {
        self.merged += 1;
        if task.fix_rounds == 0 && task.findings_high == 0 {
            self.first_pass += 1;
        }
        self.fix_rounds += u64::from(task.fix_rounds);
        self.findings += task.findings as u64;
        self.high_findings += task.findings_high as u64;
        self.cost_micros += task.cost_micros;
        if reverted {
            self.reverted += 1;
        }
        if bugfix_followup {
            self.bugfix_followups += 1;
        }
    }

    /// The row this tally becomes.
    fn row(self, key: String) -> QualityRow {
        let merged = self.merged;
        let per_merge = |total: u64| {
            if merged == 0 {
                0.0
            } else {
                total as f64 / f64::from(merged)
            }
        };
        QualityRow {
            key,
            merged,
            first_pass: if merged == 0 {
                0.0
            } else {
                f64::from(self.first_pass) / f64::from(merged)
            },
            avg_fix_rounds: per_merge(self.fix_rounds),
            avg_findings: per_merge(self.findings),
            high_findings: u32::try_from(self.high_findings).unwrap_or(u32::MAX),
            reverted: self.reverted,
            bugfix_followups: self.bugfix_followups,
            cost_per_merged_micros: if merged == 0 {
                0
            } else {
                self.cost_micros / u64::from(merged)
            },
        }
    }
}

/// The window's report, computed from the log rows alone.
///
/// `plan_kind` answers a plan id's kind (`"bug_fixing"`, `"implementation"`,
/// `"research"`) and `changed_files` a merge commit's changed files; both may
/// answer `None` (a plan or a commit this checkout does not have), which drops
/// that task from the follow-up signal only. Pure: every outside fact arrives
/// through those two callbacks, so the metrics are unit-testable.
pub fn compute(
    rows: &[DispatchRoutingRow],
    plan_kind: &dyn Fn(&str) -> Option<String>,
    changed_files: &dyn Fn(&str) -> Option<Vec<String>>,
    categories: Vec<(String, u32)>,
    now_ms: u64,
    window_days: u32,
) -> QualityReport {
    let window_ms = u64::from(window_days).saturating_mul(DAY_MS);
    let start = now_ms.saturating_sub(window_ms);
    let tasks = window_tasks(rows, start, now_ms);
    let reverts = reverts_by_worker(rows);
    let followups = bugfix_followups(&tasks, plan_kind, changed_files);
    let missing_merge_commit = tasks
        .iter()
        .filter(|task| task.merge_commit.is_none())
        .count();

    let mut by_model: BTreeMap<&str, Tally> = BTreeMap::new();
    let mut by_tier: BTreeMap<&str, Tally> = BTreeMap::new();
    for (index, task) in tasks.iter().enumerate() {
        let reverted = reverts
            .get(task.worker_id)
            .is_some_and(|latest| *latest >= task.ts);
        let followup = followups[index];
        by_model
            .entry(task.model)
            .or_default()
            .add(task, reverted, followup);
        by_tier
            .entry(task.tier)
            .or_default()
            .add(task, reverted, followup);
    }

    QualityReport {
        window_days,
        generated_ms: now_ms,
        by_model: ordered_rows(by_model),
        by_tier: ordered_rows(by_tier),
        categories,
        missing_merge_commit: u32::try_from(missing_merge_commit).unwrap_or(u32::MAX),
        edits: EditOutcomes::default(),
    }
}

/// The merged outcomes of the window, oldest first (the log's own order).
fn window_tasks(rows: &[DispatchRoutingRow], start: u64, now_ms: u64) -> Vec<Task<'_>> {
    rows.iter()
        .filter_map(|row| match row {
            DispatchRoutingRow::Outcome {
                ts,
                worker_id,
                plan_id,
                status,
                final_tier,
                models_used,
                fix_rounds,
                findings,
                findings_high,
                cost_micros,
                merge_commit,
                ..
            } if status == "merged" && *ts >= start && *ts <= now_ms => Some(Task {
                worker_id,
                plan_id,
                ts: *ts,
                tier: final_tier,
                model: models_used.last().map(String::as_str).unwrap_or("unknown"),
                fix_rounds: *fix_rounds,
                findings: *findings,
                findings_high: *findings_high,
                cost_micros: *cost_micros,
                merge_commit: merge_commit.as_deref(),
            }),
            _ => None,
        })
        .collect()
}

/// The latest `reverted` row's timestamp per worker: the merge that did not
/// hold up. A revert logged BEFORE the outcome of the same worker says nothing
/// about that outcome, so [`compute`] compares the two timestamps.
fn reverts_by_worker(rows: &[DispatchRoutingRow]) -> HashMap<&str, u64> {
    let mut latest: HashMap<&str, u64> = HashMap::new();
    for row in rows {
        if let DispatchRoutingRow::Reverted { ts, worker_id, .. } = row {
            let entry = latest.entry(worker_id.as_str()).or_insert(*ts);
            if *ts > *entry {
                *entry = *ts;
            }
        }
    }
    latest
}

/// Which tasks a later bug-fix plan's merge followed up on.
///
/// A task counts when another merged task of a DIFFERENT plan is at most
/// [`FOLLOWUP_DAYS`] later, that plan is `bug_fixing`, and the two merges share
/// at least one changed file. A task never counts as its own follow-up; a merge
/// whose commit or whose changed files are unknown is skipped, which is exactly
/// a task whose row predates `merge_commit`.
fn bugfix_followups(
    tasks: &[Task<'_>],
    plan_kind: &dyn Fn(&str) -> Option<String>,
    changed_files: &dyn Fn(&str) -> Option<Vec<String>>,
) -> Vec<bool> {
    let files: Vec<Option<Vec<String>>> = tasks
        .iter()
        .map(|task| task.merge_commit.and_then(changed_files))
        .collect();
    tasks
        .iter()
        .enumerate()
        .map(|(index, task)| {
            let Some(own) = files[index].as_deref() else {
                return false;
            };
            tasks.iter().enumerate().any(|(other_index, other)| {
                if other_index == index || other.plan_id == task.plan_id {
                    return false;
                }
                if other.ts <= task.ts || other.ts - task.ts > FOLLOWUP_DAYS * DAY_MS {
                    return false;
                }
                if plan_kind(other.plan_id).as_deref() != Some("bug_fixing") {
                    return false;
                }
                files[other_index]
                    .as_deref()
                    .is_some_and(|later| later.iter().any(|path| own.contains(path)))
            })
        })
        .collect()
}

/// Rows sorted by merges descending, then by key.
fn ordered_rows(tallies: BTreeMap<&str, Tally>) -> Vec<QualityRow> {
    let mut rows: Vec<QualityRow> = tallies
        .into_iter()
        .map(|(key, tally)| tally.row(key.to_string()))
        .collect();
    rows.sort_by(|a, b| b.merged.cmp(&a.merged).then_with(|| a.key.cmp(&b.key)));
    rows
}

/// The report over the project's routing log, its plans, its git history and
/// its findings ledger.
///
/// The log is read as the union of this computer's machine-wide log and the
/// project's committed copy, filtered to this project's plans (`reverted` and
/// `outcome` rows from another computer are only in the copy until they are
/// imported). Only the plans the log names are parsed, never the whole plans
/// folder.
pub async fn report(root: PathBuf, window_days: u32) -> Result<QualityReport, String> {
    let now_ms = u64::try_from(phase3::now_ms()).unwrap_or(u64::MAX);
    let window_ms = u64::from(window_days).saturating_mul(DAY_MS);
    let start = now_ms.saturating_sub(window_ms);
    let rows = read_log_rows(&root);
    let categories = window_categories(&root, start, now_ms);
    let plan_ids: std::collections::HashSet<&str> = rows
        .iter()
        .filter_map(|row| match row {
            DispatchRoutingRow::Outcome { plan_id, .. }
            | DispatchRoutingRow::Reverted { plan_id, .. } => Some(plan_id.as_str()),
            _ => None,
        })
        .collect();
    let commits: Vec<String> = rows
        .iter()
        .filter_map(|row| match row {
            DispatchRoutingRow::Outcome {
                ts,
                status,
                merge_commit: Some(commit),
                ..
            } if status == "merged" && *ts >= start && *ts <= now_ms => Some(commit.clone()),
            _ => None,
        })
        .collect();
    let changed = changed_files_commits(root.clone(), commits).await;
    let kinds = plan_kinds(&root, &plan_ids);
    let plan_kind = |plan_id: &str| -> Option<String> { kinds.get(plan_id).cloned() };
    let changed_files =
        |commit: &str| -> Option<Vec<String>> { changed.get(commit).cloned().flatten() };
    let mut report = compute(
        &rows,
        &plan_kind,
        &changed_files,
        categories,
        now_ms,
        window_days,
    );
    report.edits = edit_outcomes::collect(
        &edit_outcomes::transcripts_dir(&root),
        &window_worker_models(&rows, start, now_ms),
    );
    Ok(report)
}

/// The window's workers with the model each finished on, deduplicated: a
/// resumed worker can write more than one outcome row, and the latest one wins.
fn window_worker_models(
    rows: &[DispatchRoutingRow],
    start: u64,
    now_ms: u64,
) -> Vec<(String, String)> {
    let mut models: BTreeMap<&str, &str> = BTreeMap::new();
    for task in window_tasks(rows, start, now_ms) {
        models.insert(task.worker_id, task.model);
    }
    models
        .into_iter()
        .map(|(worker, model)| (worker.to_string(), model.to_string()))
        .collect()
}


/// The routing log's rows for THIS project: the machine-wide log plus the
/// project's committed copy, deduplicated by line and filtered to the rows whose
/// plan lives in this project. A malformed line (a torn append, a hand edit) is
/// skipped rather than failing the report.
///
/// The filter matters: the log at [`dispatch_routing::dispatch_routing_log_path`]
/// is machine-wide, so without it another project's merges would be counted here.
fn read_log_rows(root: &Path) -> Vec<DispatchRoutingRow> {
    let plans_dir = root.join(".coding").join("plans");
    let bundle = root
        .join(crate::mnemo2::laya_learnings::LEARNINGS_DIR)
        .join(crate::mnemo2::laya_learnings::LEARNINGS_FILE);
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut rows: Vec<DispatchRoutingRow> = Vec::new();
    for path in [dispatch_routing::dispatch_routing_log_path(), bundle] {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in text.lines() {
            let line = line.trim_end_matches('\r').trim();
            if line.is_empty() || !seen.insert(line.to_string()) {
                continue;
            }
            if !crate::mnemo2::laya_learnings::is_project_row(line, &plans_dir) {
                continue;
            }
            if let Ok(row) = serde_json::from_str::<DispatchRoutingRow>(line) {
                rows.push(row);
            }
        }
    }
    rows
}

/// The plan kind per plan id, read from `.coding/plans/<plan_id>.md`; a plan
/// this checkout lacks is absent, and its tasks simply never count as
/// follow-ups.
fn plan_kinds(root: &Path, wanted: &std::collections::HashSet<&str>) -> HashMap<String, String> {
    let dir = root.join(".coding").join("plans");
    let mut kinds = HashMap::new();
    for plan_id in wanted {
        let path = dir.join(format!("{plan_id}.md"));
        let Ok(markdown) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Ok(plan) = mnemo::memory::plan_format::PlanFile::parse(&markdown) {
            kinds.insert((*plan_id).to_string(), plan.kind.to_string());
        }
    }
    kinds
}

/// The window's most recurring finding categories, as (category, distinct
/// workers); empty when the ledger is absent.
fn window_categories(root: &Path, start: u64, now_ms: u64) -> Vec<(String, u32)> {
    let rows: Vec<findings_ledger::FindingRow> = findings_ledger::read_rows(root)
        .into_iter()
        .filter(|row| row.ts >= start && row.ts <= now_ms)
        .collect();
    findings_ledger::category_counts(&rows)
        .into_iter()
        .take(MAX_CATEGORIES)
        .map(|(category, count)| (category, u32::try_from(count).unwrap_or(u32::MAX)))
        .collect()
}

/// Whether `commit` is a plain hex commit id — 7 to 64 ASCII hex digits, the
/// only shape passed to git as a revision. A commit id cannot look like an
/// option (`--output=…`), so anything else is never handed to git.
fn is_plain_commit_id(commit: &str) -> bool {
    commit.len() >= 7 && commit.len() <= 64 && commit.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Each merge commit's changed files relative to its first parent, cached per
/// commit so the report asks git once. A commit this checkout does not have
/// (a shallow clone, another computer's merge not fetched yet) yields `None`,
/// and its task drops out of the follow-up signal only. So does a commit id
/// that is not plain hex: the routing log is union-merged from other computers,
/// and a log-supplied string must never reach git as an option.
async fn changed_files_commits(
    root: PathBuf,
    commits: Vec<String>,
) -> HashMap<String, Option<Vec<String>>> {
    let mut cache: HashMap<String, Option<Vec<String>>> = HashMap::new();
    let root = root.as_path();
    for commit in commits {
        if cache.contains_key(&commit) {
            continue;
        }
        if !is_plain_commit_id(&commit) {
            cache.insert(commit, None);
            continue;
        }
        let first_parent = format!("{commit}^1");
        let files =
            match phase3::git_value(root, &["diff", "--name-only", &first_parent, &commit, "--"])
                .await
            {
                Ok(output) => Some(
                    output
                        .lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty())
                        .map(str::to_string)
                        .collect::<Vec<String>>(),
                ),
                Err(_) => None,
            };
        cache.insert(commit, files);
    }
    cache
}

/// The Quality tab's data for the open project's log.
#[tauri::command]
pub async fn quality_report(
    state: tauri::State<'_, crate::mnemo2::ipc::Mnemo2State>,
    window_days: Option<u32>,
) -> Result<QualityReport, String> {
    let lead = state
        .open_lead()
        .await
        .ok_or_else(|| "Mnemo2 has no open lead; call lead_open first".to_string())?;
    report(
        lead.workspace_root().to_path_buf(),
        window_days.unwrap_or(DEFAULT_WINDOW_DAYS),
    )
    .await
}

/// The `team_quality` tool's short text: one line per tier, plus the models and
/// the recurring finding categories when there are any.
pub fn summary(report: &QualityReport) -> String {
    let mut lines = vec![format!(
        "Quality over {} days ({} tier(s), {} model(s); {} merge(s) without a merge commit)",
        report.window_days,
        report.by_tier.len(),
        report.by_model.len(),
        report.missing_merge_commit
    )];
    if report.by_tier.is_empty() {
        lines.push("no merges in the window".to_string());
        return lines.join("\n");
    }
    for row in &report.by_tier {
        lines.push(line(row));
    }
    if !report.by_model.is_empty() {
        lines.push("By model:".to_string());
        for row in &report.by_model {
            lines.push(format!("  {}", line(row)));
        }
    }
    if !report.edits.by_model.is_empty() {
        lines.push("Edits:".to_string());
        for row in &report.edits.by_model {
            lines.push(format!("  {}", edit_line(row)));
        }
        if !report.edits.top_files.is_empty() {
            let listed = report
                .edits
                .top_files
                .iter()
                .take(5)
                .map(edit_file_line)
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!("Files with the most edit failures: {listed}"));
        }
    }
    if report.edits.skipped > 0 {
        lines.push(format!(
            "Edits: {} worker transcript(s) were missing or unreadable and skipped",
            report.edits.skipped
        ));
    }
    if !report.categories.is_empty() {
        let listed = report
            .categories
            .iter()
            .map(|(category, count)| format!("{category} ({count})"))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!("Recurring finding categories: {listed}"));
    }
    lines.join("\n")
}

/// One row as the summary's line: `routine: 12 merged, 75% first pass, ...`.
fn line(row: &QualityRow) -> String {
    format!(
        "{}: {} merged, {}% first pass, {:.1} fix rounds, {} reverted, {} followed by a bug fix",
        row.key,
        row.merged,
        (row.first_pass * 100.0).round() as u32,
        row.avg_fix_rounds,
        row.reverted,
        row.bugfix_followups
    )
}

/// One model's or worker's line in the summary's `Edits` block:
/// `glm-5.3: 40 attempts, 16 failed (7 shape, 2 receipt drift, ...)`.
fn edit_line(row: &EditOutcomeRow) -> String {
    let failed = row.failures();
    if failed == 0 {
        return format!("{}: {} attempts, none failed", row.key, row.attempts);
    }
    format!(
        "{}: {} attempts, {} failed ({})",
        row.key,
        row.attempts,
        failed,
        edit_causes(
            row.shape,
            row.receipt_drift,
            row.mismatch,
            row.blocked,
            row.other
        )
    )
}

/// One problem file's line in the summary's `Edits` block:
/// `crates/fg_sim/src/layout.rs (5 failed of 12: 2 shape, 1 blocked)`.
fn edit_file_line(file: &EditFileRow) -> String {
    format!(
        "{} ({} failed of {}: {})",
        file.path,
        file.failures,
        file.attempts,
        edit_causes(
            file.shape,
            file.receipt_drift,
            file.mismatch,
            file.blocked,
            file.other
        )
    )
}

/// The causes that happened, as `7 shape, 2 receipt drift, ...`.
fn edit_causes(shape: u32, receipt_drift: u32, mismatch: u32, blocked: u32, other: u32) -> String {
    let mut parts = Vec::new();
    for (label, count) in [
        ("shape", shape),
        ("receipt drift", receipt_drift),
        ("mismatch", mismatch),
        ("blocked", blocked),
        ("other", other),
    ] {
        if count > 0 {
            parts.push(format!("{count} {label}"));
        }
    }
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000_000;

    /// Run git in the fixture repository and assert it succeeded.
    async fn run_git(root: &Path, args: &[&str]) {
        let output = phase3::git(root, args).await.expect("run git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// A repository with one commit, plus that commit's id.
    async fn one_commit_repo() -> (tempfile::TempDir, String) {
        let root = tempfile::tempdir().expect("repo");
        run_git(root.path(), &["init", "-b", "main"]).await;
        run_git(root.path(), &["config", "user.name", "Quality Test"]).await;
        run_git(
            root.path(),
            &["config", "user.email", "quality@example.invalid"],
        )
        .await;
        std::fs::write(root.path().join("seed.md"), "seed\n").expect("seed file");
        run_git(root.path(), &["add", "seed.md"]).await;
        run_git(root.path(), &["commit", "-m", "seed"]).await;
        let commit = phase3::git_value(root.path(), &["rev-parse", "HEAD"])
            .await
            .expect("head commit");
        (root, commit)
    }

    /// A merged outcome row, built field by field: the defaults are a clean
    /// first-pass merge on the `routine` tier.
    #[derive(Clone, Default)]
    struct Merged {
        worker_id: &'static str,
        plan_id: &'static str,
        ts: u64,
        tier: &'static str,
        model: &'static str,
        fix_rounds: u32,
        findings: usize,
        findings_high: usize,
        cost_micros: u64,
        merge_commit: Option<&'static str>,
    }

    impl Merged {
        /// The `outcome` row this task writes to the log.
        fn row(&self) -> DispatchRoutingRow {
            DispatchRoutingRow::Outcome {
                ts: self.ts,
                worker_id: self.worker_id.to_string(),
                plan_id: self.plan_id.to_string(),
                step: 1,
                status: "merged".to_string(),
                final_tier: self.tier.to_string(),
                models_used: vec![self.model.to_string()],
                review_rounds: 1,
                fix_rounds: self.fix_rounds,
                findings: self.findings,
                findings_high: self.findings_high,
                audits: 0,
                cost_micros: self.cost_micros,
                label: Some("clean".to_string()),
                merge_commit: self.merge_commit.map(str::to_string),
            }
        }
    }

    /// A `reverted` row.
    fn reverted(worker_id: &str, plan_id: &str, ts: u64) -> DispatchRoutingRow {
        DispatchRoutingRow::Reverted {
            ts,
            worker_id: worker_id.to_string(),
            plan_id: plan_id.to_string(),
            step: 1,
            revert_commit: "beef1234".to_string(),
        }
    }

    /// No plan has a kind: nothing can be a bug-fix follow-up.
    fn no_plans(_plan_id: &str) -> Option<String> {
        None
    }

    /// No commit has files.
    fn no_files(_commit: &str) -> Option<Vec<String>> {
        None
    }

    /// Compute over the given rows with the plain callbacks.
    fn plain(rows: &[DispatchRoutingRow]) -> QualityReport {
        compute(rows, &no_plans, &no_files, Vec::new(), NOW, 30)
    }

    #[test]
    fn first_pass_and_averages() {
        let rows = vec![
            Merged {
                worker_id: "w1",
                plan_id: "p1",
                ts: NOW - DAY_MS,
                tier: "routine",
                model: "m-a",
                findings: 2,
                cost_micros: 1_000,
                ..Merged::default()
            }
            .row(),
            Merged {
                worker_id: "w2",
                plan_id: "p1",
                ts: NOW - DAY_MS,
                tier: "routine",
                model: "m-a",
                fix_rounds: 2,
                findings: 4,
                findings_high: 1,
                cost_micros: 3_000,
                ..Merged::default()
            }
            .row(),
            Merged {
                worker_id: "w3",
                plan_id: "p2",
                ts: NOW - DAY_MS,
                tier: "high",
                model: "m-b",
                fix_rounds: 1,
                findings: 1,
                cost_micros: 2_000,
                ..Merged::default()
            }
            .row(),
        ];
        let report = plain(&rows);
        let tier = report
            .by_tier
            .iter()
            .find(|row| row.key == "routine")
            .expect("routine row");
        assert_eq!(tier.merged, 2);
        assert!((tier.first_pass - 0.5).abs() < 1e-9);
        assert!((tier.avg_fix_rounds - 1.0).abs() < 1e-9);
        assert!((tier.avg_findings - 3.0).abs() < 1e-9);
        assert_eq!(tier.high_findings, 1);
        assert_eq!(tier.cost_per_merged_micros, 2_000);
        // Most merges first, then the key.
        assert_eq!(
            report
                .by_tier
                .iter()
                .map(|row| row.key.as_str())
                .collect::<Vec<_>>(),
            vec!["routine", "high"]
        );
        let model = report
            .by_model
            .iter()
            .find(|row| row.key == "m-a")
            .expect("m-a row");
        assert_eq!(model.merged, 2);
        assert_eq!(report.missing_merge_commit, 3);
    }

    #[test]
    fn reverted_counts_only_later_reverts() {
        let rows = vec![
            Merged {
                worker_id: "w1",
                plan_id: "p1",
                ts: NOW - 2 * DAY_MS,
                tier: "routine",
                model: "m",
                ..Merged::default()
            }
            .row(),
            // A revert from before the outcome says nothing about it.
            reverted("w1", "p1", NOW - 3 * DAY_MS),
            Merged {
                worker_id: "w2",
                plan_id: "p2",
                ts: NOW - DAY_MS,
                tier: "routine",
                model: "m",
                ..Merged::default()
            }
            .row(),
            reverted("w2", "p2", NOW),
        ];
        let report = plain(&rows);
        assert_eq!(report.by_tier[0].merged, 2);
        assert_eq!(report.by_tier[0].reverted, 1);
    }

    #[test]
    fn bugfix_followup_needs_overlap_window_and_bug_plan() {
        /// One merged task on `tier` that changed `commit`'s files.
        fn task(
            worker_id: &'static str,
            plan_id: &'static str,
            tier: &'static str,
            days_ago: u64,
            commit: &'static str,
        ) -> Merged {
            Merged {
                worker_id,
                plan_id,
                ts: NOW - days_ago * DAY_MS,
                tier,
                model: "m",
                merge_commit: Some(commit),
                ..Merged::default()
            }
        }
        // Every scenario lives on its own tier so the pairs cannot chain.
        let rows = vec![
            // The real thing: a bug plan, 1 day later, the same file.
            task("w1", "p1", "overlap", 20, "aaa111").row(),
            task("w2", "p2", "overlap", 19, "bbb222").row(),
            // Same files and a bug plan, but 15 days later.
            task("w3", "p3", "outside", 20, "ccc333").row(),
            task("w4", "p4", "outside", 5, "ddd444").row(),
            // Same files, later, bug plan — but the SAME plan as the task.
            task("w5", "p5", "same-plan", 20, "eee555").row(),
            task("w6", "p5", "same-plan", 19, "fff666").row(),
            // Same files, later, different plan — but not a bug-fix plan.
            task("w7", "p7", "non-bug", 20, "ggg777").row(),
            task("w8", "p8", "non-bug", 19, "hhh888").row(),
            // A bug plan 1 day later, but other files only.
            task("w9", "p9", "other-files", 20, "iii999").row(),
            task("w10", "p10", "other-files", 19, "jjj000").row(),
        ];
        let plan_kind = |plan_id: &str| -> Option<String> {
            match plan_id {
                "p2" | "p4" | "p5" | "p10" => Some("bug_fixing".to_string()),
                _ => Some("implementation".to_string()),
            }
        };
        let changed = |commit: &str| -> Option<Vec<String>> {
            match commit {
                "aaa111" | "bbb222" => Some(vec!["src/a.rs".to_string()]),
                "ccc333" | "ddd444" => Some(vec!["src/c.rs".to_string()]),
                "eee555" | "fff666" => Some(vec!["src/d.rs".to_string()]),
                "ggg777" | "hhh888" => Some(vec!["src/e.rs".to_string()]),
                "iii999" => Some(vec!["src/f.rs".to_string()]),
                "jjj000" => Some(vec!["src/g.rs".to_string()]),
                _ => None,
            }
        };
        let report = compute(&rows, &plan_kind, &changed, Vec::new(), NOW, 30);
        let followups = |tier: &str| -> u32 {
            report
                .by_tier
                .iter()
                .find(|row| row.key == tier)
                .unwrap_or_else(|| panic!("{tier} row"))
                .bugfix_followups
        };
        // Only the first task of the overlapping pair is followed by a bug fix.
        assert_eq!(followups("overlap"), 1);
        assert_eq!(followups("outside"), 0);
        assert_eq!(followups("same-plan"), 0);
        assert_eq!(followups("non-bug"), 0);
        assert_eq!(followups("other-files"), 0);
        assert_eq!(report.by_tier.iter().map(|row| row.merged).sum::<u32>(), 10);
    }


    #[test]
    fn rows_without_merge_commit_are_counted_as_missing() {
        let rows = vec![
            Merged {
                worker_id: "w1",
                plan_id: "p1",
                ts: NOW - DAY_MS,
                tier: "routine",
                model: "m",
                merge_commit: Some("aaa111"),
                ..Merged::default()
            }
            .row(),
            Merged {
                worker_id: "w2",
                plan_id: "p2",
                ts: NOW - DAY_MS,
                tier: "routine",
                model: "m",
                ..Merged::default()
            }
            .row(),
        ];
        let report = plain(&rows);
        assert_eq!(report.missing_merge_commit, 1);
        // They still count for every other metric.
        assert_eq!(report.by_tier[0].merged, 2);
    }

    #[tokio::test]
    async fn changed_files_rejects_option_like_commit() {
        // A crafted routing-log row must never reach git as an option:
        // `--output=…` makes `git diff` write a file wherever it names.
        let (root, _head) = one_commit_repo().await;
        let leak = root.path().join("leak.txt");
        let commit = format!("--output={}", leak.display());
        let cache = changed_files_commits(root.path().to_path_buf(), vec![commit.clone()]).await;
        assert!(
            matches!(cache.get(&commit), Some(None)),
            "an option-like commit must count like an unknown commit: {cache:?}"
        );
        assert!(
            !leak.exists(),
            "git wrote {} from a log-supplied commit",
            leak.display()
        );
    }

    #[tokio::test]
    async fn changed_files_accepts_a_real_commit() {
        // A real hex commit id still reaches git and yields its changed files.
        let (root, _seed) = one_commit_repo().await;
        std::fs::write(root.path().join("changed.rs"), "pub fn changed() {}\n")
            .expect("write the change");
        run_git(root.path(), &["add", "changed.rs"]).await;
        run_git(root.path(), &["commit", "-m", "change"]).await;
        let tip = phase3::git_value(root.path(), &["rev-parse", "HEAD"])
            .await
            .expect("tip commit");
        let cache = changed_files_commits(root.path().to_path_buf(), vec![tip.clone()]).await;
        assert_eq!(
            cache.get(&tip),
            Some(&Some(vec!["changed.rs".to_string()])),
            "a real commit's changed files: {cache:?}"
        );
    }

    #[test]
    fn window_excludes_old_rows() {
        let rows = vec![
            Merged {
                worker_id: "w1",
                plan_id: "p1",
                ts: NOW - 40 * DAY_MS,
                tier: "old",
                model: "m-old",
                ..Merged::default()
            }
            .row(),
            Merged {
                worker_id: "w2",
                plan_id: "p2",
                ts: NOW - DAY_MS,
                tier: "routine",
                model: "m",
                ..Merged::default()
            }
            .row(),
        ];
        let report = plain(&rows);
        assert_eq!(report.by_tier.len(), 1);
        assert_eq!(report.by_tier[0].key, "routine");
        assert_eq!(report.by_model.len(), 1);
    }

    #[test]
    fn summary_names_the_edit_attempts_and_the_worst_files() {
        let rows = vec![Merged {
            worker_id: "w1",
            plan_id: "p1",
            ts: NOW - DAY_MS,
            tier: "routine",
            model: "m-a",
            ..Merged::default()
        }
        .row()];
        let mut report = plain(&rows);
        report.edits = EditOutcomes {
            by_model: vec![EditOutcomeRow {
                key: "m-a".to_string(),
                attempts: 40,
                ok: 24,
                shape: 7,
                receipt_drift: 2,
                mismatch: 4,
                blocked: 1,
                other: 2,
            }],
            by_worker: Vec::new(),
            top_files: vec![EditFileRow {
                path: "crates/fg_sim/src/layout.rs".to_string(),
                attempts: 12,
                failures: 5,
                shape: 2,
                receipt_drift: 1,
                mismatch: 1,
                blocked: 1,
                other: 0,
            }],
            skipped: 1,
        };
        let text = summary(&report);
        assert!(text.contains("Edits:"), "the summary gains an Edits block: {text}");
        assert!(
            text.contains("m-a: 40 attempts, 16 failed (7 shape, 2 receipt drift, 4 mismatch, 1 blocked, 2 other)"),
            "{text}"
        );
        assert!(
            text.contains(
                "Files with the most edit failures: crates/fg_sim/src/layout.rs (5 failed of 12: 2 shape, 1 receipt drift, 1 mismatch, 1 blocked)"
            ),
            "{text}"
        );
        assert!(text.contains("1 worker transcript(s) were missing or unreadable"), "{text}");
    }
}
