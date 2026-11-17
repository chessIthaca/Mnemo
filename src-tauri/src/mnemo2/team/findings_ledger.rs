// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]

//! The append-only review-findings ledger: one row per recorded finding, the
//! raw material for turning a recurring kind of mistake into a proposal.
//!
//! The ledger lives at `.coding/reviews/findings.jsonl` in the lead's
//! workspace root and is committed like the reviews beside it. A row is
//! written when a finding id is first recorded for a worker, or when its
//! `category` or `enforce` changed, so re-recording the same review appends
//! nothing.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use mnemo::app::backlog::BacklogItem;
use serde::{Deserialize, Serialize};

use crate::mnemo2::backlog;
use crate::mnemo2::phase3::{self, Finding};

use super::record::WorkerRecord;

/// One line of `.coding/reviews/findings.jsonl`; an absent value is null.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FindingRow {
    /// Unix epoch milliseconds of the row.
    pub(crate) ts: u64,
    /// The worker whose review recorded the finding.
    pub(crate) worker_id: String,
    /// The plan the worker was dispatched under, when it had one.
    #[serde(default)]
    pub(crate) plan_id: Option<String>,
    /// The 0-based plan step the worker was dispatched under, when it had one.
    #[serde(default)]
    pub(crate) step: Option<usize>,
    /// The finding id, stable across re-reviews.
    pub(crate) finding_id: String,
    /// `high` or `low`.
    pub(crate) severity: String,
    /// Short finding title.
    pub(crate) title: String,
    /// Repository-relative source path.
    pub(crate) file: String,
    /// The kebab-case kind of mistake, when the lead named one.
    #[serde(default)]
    pub(crate) category: Option<String>,
    /// Where the kind of mistake could be stopped, when the lead said.
    #[serde(default)]
    pub(crate) enforce: Option<String>,
    /// The model that actually ran, or its selected spec.
    #[serde(default)]
    pub(crate) model: Option<String>,
    /// The resolved ladder tier, when known.
    #[serde(default)]
    pub(crate) tier: Option<String>,
}

/// The findings ledger of a workspace.
pub(crate) fn ledger_path(root: &Path) -> PathBuf {
    root.join(".coding").join("reviews").join("findings.jsonl")
}

/// Every row of the ledger. A malformed line — a hand edit, or a merge that
/// left a fragment — is skipped rather than failing the read.
pub(crate) fn read_rows(root: &Path) -> Vec<FindingRow> {
    let Ok(text) = std::fs::read_to_string(ledger_path(root)) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| serde_json::from_str::<FindingRow>(line.trim()).ok())
        .collect()
}

/// Append one row for every finding this worker has not logged yet, or whose
/// `category` or `enforce` changed since its last row. The file and its
/// directory are created on first use. It runs on the lead's workspace root,
/// never on a worker checkout.
pub(crate) fn append_new(
    root: &Path,
    worker: &WorkerRecord,
    findings: &[Finding],
) -> std::io::Result<()> {
    let rows = read_rows(root);
    let ts = u64::try_from(phase3::now_ms()).unwrap_or(u64::MAX);
    let mut lines = Vec::new();
    for finding in findings {
        let latest = rows
            .iter()
            .rev()
            .find(|row| row.worker_id == worker.id && row.finding_id == finding.id);
        if let Some(latest) = latest {
            if latest.category == finding.category && latest.enforce == finding.enforce {
                continue;
            }
        }
        let row = FindingRow {
            ts,
            worker_id: worker.id.clone(),
            plan_id: worker.plan_id.clone(),
            step: worker.plan_step,
            finding_id: finding.id.clone(),
            severity: finding.severity.clone(),
            title: finding.title.clone(),
            file: finding.file.clone(),
            category: finding.category.clone(),
            enforce: finding.enforce.clone(),
            model: worker.model_id.clone().or_else(|| worker.model.clone()),
            tier: worker.tier.clone(),
        };
        let line = serde_json::to_string(&row)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
        lines.push(line);
    }
    if lines.is_empty() {
        return Ok(());
    }
    let path = ledger_path(root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    for line in lines {
        writeln!(file, "{line}")?;
    }
    Ok(())
}

/// How many DISTINCT workers recorded each category, most frequent first and
/// ties broken by category name.
pub(crate) fn category_counts(rows: &[FindingRow]) -> Vec<(String, usize)> {
    let mut workers: HashMap<&str, HashSet<&str>> = HashMap::new();
    for row in rows {
        let Some(category) = row.category.as_deref() else {
            continue;
        };
        workers
            .entry(category)
            .or_default()
            .insert(row.worker_id.as_str());
    }
    let mut counts: Vec<(String, usize)> = workers
        .into_iter()
        .map(|(category, workers)| (category.to_string(), workers.len()))
        .collect();
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    counts
}

/// How many distinct workers one category has to come up in before Mnemo2
/// stops correcting it review by review and proposes enforcing it instead.
pub(crate) const RECURRENCE_THRESHOLD: usize = 3;

/// The marker line a recurrence proposal's text ends with. A category is
/// proposed once; the marker is how a later read tells that it was, and how
/// the item's plan is found from the category.
pub(crate) fn proposal_marker(category: &str) -> String {
    format!("[finding-category:{category}]")
}

/// The categories a set of findings carries, in the order they appear, with
/// repeats dropped.
fn finding_categories(findings: &[Finding]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut categories = Vec::new();
    for category in findings.iter().filter_map(|finding| finding.category.as_deref()) {
        if seen.insert(category) {
            categories.push(category.to_string());
        }
    }
    categories
}

/// The latest ledger row of one category — the one whose `enforce` counts.
fn latest_row<'a>(rows: &'a [FindingRow], category: &str) -> Option<&'a FindingRow> {
    rows.iter()
        .rev()
        .find(|row| row.category.as_deref() == Some(category))
}

/// How a proposal suggests stopping the mistake for good, or `None` when only
/// a reviewer can judge it (or the lead said nothing).
fn enforce_wording(enforce: &str) -> Option<&'static str> {
    match enforce {
        "code" => Some("a type or API that makes the mistake impossible to write"),
        "build" => Some("a lint or policy test that fails the build"),
        "test" => Some("a test or check that catches it"),
        _ => None,
    }
}

/// `true` when one live backlog item already carries this category's marker.
fn already_proposed(items: &[BacklogItem], category: &str) -> bool {
    let marker = proposal_marker(category);
    items.iter().any(|item| item.text.contains(&marker))
}

/// The live backlog items of a workspace.
fn live_items(root: &Path) -> Vec<BacklogItem> {
    backlog::read_items(&backlog::backlog_file(root))
}

/// The backlog text proposing a guard for one category: one line per worker
/// that hit it, newest first, and where the mistake could be stopped.
fn proposal_text(category: &str, wording: &str, rows: &[FindingRow]) -> String {
    let mut seen = HashSet::new();
    let mut workers = Vec::new();
    for row in rows.iter().rev() {
        if row.category.as_deref() != Some(category) {
            continue;
        }
        if !seen.insert(row.worker_id.as_str()) {
            continue;
        }
        workers.push(format!("- {}: {} ({})", row.worker_id, row.title, row.file));
    }
    let count = workers.len();
    let workers = workers.join("\n");
    let marker = proposal_marker(category);
    format!(
        "Stop '{category}' mistakes for good (seen in {count} reviews)\n\n\
         The same kind of review finding keeps coming back:\n{workers}\n\n\
         Suggested enforcement: {wording}. Plan this to add the guard, then cite it \
         in the finding's category so later reviews can check it.\n\n\
         {marker}"
    )
}

/// Propose a guard for every category this review's findings carry that has
/// now come up in exactly [`RECURRENCE_THRESHOLD`] distinct workers, when the
/// latest row names `code`, `build` or `test` as where it could be stopped.
///
/// The proposal is ONE backlog item whose text ends with
/// [`proposal_marker`]; a live item that already carries the marker means the
/// category was proposed before, so nothing is added again. Runs on the lead's
/// workspace root, after the review's own rows are in the ledger.
pub(crate) fn propose_recurring(root: &Path, findings: &[Finding]) -> Result<(), String> {
    let categories = finding_categories(findings);
    if categories.is_empty() {
        return Ok(());
    }
    let rows = read_rows(root);
    let counts = category_counts(&rows);
    let items = live_items(root);
    for category in categories {
        let count = count_of(&counts, &category);
        if count != RECURRENCE_THRESHOLD {
            continue;
        }
        let Some(enforce) = latest_row(&rows, &category).and_then(|row| row.enforce.as_deref())
        else {
            continue;
        };
        let Some(wording) = enforce_wording(enforce) else {
            continue;
        };
        if already_proposed(&items, &category) {
            continue;
        }
        let text = proposal_text(&category, wording, &rows);
        backlog::add_item(&backlog::backlog_file(root), &text)?;
    }
    Ok(())
}

/// The count of one category in [`category_counts`]' output, or 0.
fn count_of(counts: &[(String, usize)], category: &str) -> usize {
    counts
        .iter()
        .find(|(name, _)| name == category)
        .map(|(_, count)| *count)
        .unwrap_or(0)
}

/// One line per category this review's findings carry that has come up in
/// [`RECURRENCE_THRESHOLD`] or more workers' reviews, for the review's answer.
///
/// A category whose proposal exists says so; otherwise the line points the
/// lead at enforcing it by hand (above the threshold nothing new is proposed).
pub(crate) fn recurrence_lines(root: &Path, findings: &[Finding]) -> Vec<String> {
    let categories = finding_categories(findings);
    if categories.is_empty() {
        return Vec::new();
    }
    let rows = read_rows(root);
    let counts = category_counts(&rows);
    let items = live_items(root);
    let mut lines = Vec::new();
    for category in categories {
        let count = count_of(&counts, &category);
        if count < RECURRENCE_THRESHOLD {
            continue;
        }
        if already_proposed(&items, &category) {
            lines.push(format!(
                "Finding category '{category}' has now come up in {count} reviews; a backlog item proposes enforcing it."
            ));
            continue;
        }
        lines.push(
            match latest_row(&rows, &category).and_then(|row| row.enforce.as_deref()) {
                Some(enforce) => format!(
                    "Finding category '{category}' has come up in {count} reviews; consider enforcing it (enforce: {enforce})."
                ),
                None => format!(
                    "Finding category '{category}' has come up in {count} reviews; consider enforcing it."
                ),
            },
        );
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mnemo2::team::test_worker_record;

    fn finding(id: &str, category: Option<&str>) -> Finding {
        Finding {
            id: id.to_string(),
            severity: "high".to_string(),
            title: "the unchecked path".to_string(),
            detail: "nobody tested it".to_string(),
            file: "src/lib.rs".to_string(),
            line: 1,
            status: "open".to_string(),
            fix_commit: None,
            evidence: None,
            justification: None,
            category: category.map(str::to_string),
            enforce: Some("test".to_string()),
        }
    }

    #[test]
    fn append_new_writes_once_per_finding() {
        let root = tempfile::tempdir().expect("workspace");
        let worker = test_worker_record("worker01");
        append_new(root.path(), &worker, &[finding("F-1", Some("vacuous-test"))])
            .expect("first write");
        append_new(root.path(), &worker, &[finding("F-1", Some("vacuous-test"))])
            .expect("same review again");
        assert_eq!(read_rows(root.path()).len(), 1, "the same review writes once");
        append_new(
            root.path(),
            &worker,
            &[finding("F-1", Some("missing-error-path"))],
        )
        .expect("changed category");
        let rows = read_rows(root.path());
        assert_eq!(rows.len(), 2, "a changed category is a new row");
        assert_eq!(rows[1].category.as_deref(), Some("missing-error-path"));
        assert_eq!(rows[1].enforce.as_deref(), Some("test"));
        assert_eq!(rows[1].worker_id, "worker01");
    }

    #[test]
    fn category_counts_count_distinct_workers() {
        let worker = test_worker_record("worker01");
        let row = |finding_id: &str, category: &str| FindingRow {
            ts: 1,
            worker_id: worker.id.clone(),
            plan_id: None,
            step: None,
            finding_id: finding_id.to_string(),
            severity: "low".to_string(),
            title: "t".to_string(),
            file: "src/lib.rs".to_string(),
            category: Some(category.to_string()),
            enforce: None,
            model: None,
            tier: None,
        };
        let mut teammate = row("F-2", "vacuous-test");
        teammate.worker_id = "worker02".to_string();
        let mut other = row("F-3", "docs-out-of-sync");
        other.worker_id = "worker01".to_string();
        let rows = vec![
            row("F-1", "vacuous-test"),
            teammate,
            other,
        ];
        assert_eq!(
            category_counts(&rows),
            vec![
                ("vacuous-test".to_string(), 2),
                ("docs-out-of-sync".to_string(), 1),
            ]
        );
    }

    #[test]
    fn read_rows_skips_malformed_lines() {
        let root = tempfile::tempdir().expect("workspace");
        std::fs::create_dir_all(ledger_path(root.path()).parent().expect("parent"))
            .expect("reviews dir");
        std::fs::write(
            ledger_path(root.path()),
            "not json\n{\"worker_id\":\"w1\",\"finding_id\":\"F-1\",\"severity\":\"low\",\"title\":\"t\",\"file\":\"f\",\"ts\":3}\n",
        )
        .expect("ledger file");
        let rows = read_rows(root.path());
        assert_eq!(rows.len(), 1, "the malformed line is skipped");
        assert_eq!(rows[0].finding_id, "F-1");
        assert_eq!(rows[0].category, None);
    }

    /// One review as the runtime records it: the ledger rows, then the
    /// recurrence check. Returns the findings the review carried.
    fn record_review(root: &Path, worker_id: &str, category: &str, enforce: &str) -> Vec<Finding> {
        let worker = test_worker_record(worker_id);
        let mut open = finding("F-1", Some(category));
        open.enforce = Some(enforce.to_string());
        let findings = std::slice::from_ref(&open).to_vec();
        append_new(root, &worker, &findings).expect("ledger row");
        propose_recurring(root, &findings).expect("recurrence check");
        findings
    }

    /// The workspace's live backlog items, read from disk.
    fn backlog_items(root: &Path) -> Vec<BacklogItem> {
        backlog::read_items(&backlog::backlog_file(root))
    }

    #[test]
    fn third_worker_with_category_adds_one_backlog_item() {
        let root = tempfile::tempdir().expect("workspace");
        for worker in ["worker01", "worker02"] {
            record_review(root.path(), worker, "vacuous-test", "test");
        }
        assert!(
            backlog_items(root.path()).is_empty(),
            "two workers are a coincidence, not a pattern"
        );
        let findings = record_review(root.path(), "worker03", "vacuous-test", "test");
        let items = backlog_items(root.path());
        assert_eq!(items.len(), 1, "the third worker proposes the guard once");
        let text = &items[0].text;
        assert!(
            text.starts_with("Stop 'vacuous-test' mistakes for good (seen in 3 reviews)"),
            "{text}"
        );
        assert!(
            text.ends_with(&proposal_marker("vacuous-test")),
            "the item ends with the category marker: {text}"
        );
        assert!(
            text.contains(
                "- worker03: the unchecked path (src/lib.rs)\n\
                 - worker02: the unchecked path (src/lib.rs)\n\
                 - worker01: the unchecked path (src/lib.rs)"
            ),
            "one line per worker, newest first: {text}"
        );
        assert!(
            text.contains(
                "Suggested enforcement: a test or check that catches it. Plan this to add \
                 the guard, then cite it in the finding's category so later reviews can check it."
            ),
            "{text}"
        );
        assert_eq!(
            recurrence_lines(root.path(), &findings),
            vec![
                "Finding category 'vacuous-test' has now come up in 3 reviews; \
                 a backlog item proposes enforcing it."
                    .to_string()
            ]
        );
        let findings = record_review(root.path(), "worker04", "vacuous-test", "test");
        assert_eq!(
            backlog_items(root.path()).len(),
            1,
            "a fourth worker does not add a second item"
        );
        assert_eq!(
            recurrence_lines(root.path(), &findings),
            vec![
                "Finding category 'vacuous-test' has now come up in 4 reviews; \
                 a backlog item proposes enforcing it."
                    .to_string()
            ]
        );
    }

    #[test]
    fn review_category_does_not_propose() {
        let root = tempfile::tempdir().expect("workspace");
        let mut findings = Vec::new();
        for worker in ["worker01", "worker02", "worker03"] {
            findings = record_review(root.path(), worker, "taste-call", "review");
        }
        assert!(
            backlog_items(root.path()).is_empty(),
            "only a person can judge this one, so nothing is proposed"
        );
        assert_eq!(
            recurrence_lines(root.path(), &findings),
            vec![
                "Finding category 'taste-call' has come up in 3 reviews; \
                 consider enforcing it (enforce: review)."
                    .to_string()
            ]
        );
    }
}
