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

use serde::{Deserialize, Serialize};

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
}
