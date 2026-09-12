// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Plan-file scanning + digest building for the derived-record indexer —
//! the `plan:<stem>` source family (`.coding/plans/*.md`).

use std::path::Path;

use crate::memory::{MemoryRecordType, MemorySearchConfig};
use crate::workflow::PlanFile;

use super::{budgeted_digest, first_commit_hash, first_content_line, truncate_title, SourceRecord};

/// Scan the plans dir into plan digest sources (+ non-fatal per-source
/// errors).
pub(super) fn scan_plans(
    dir: &Path,
    out: &mut Vec<SourceRecord>,
    errors: &mut Vec<String>,
    config: &MemorySearchConfig,
) {
    // No plans dir → no plan sources (a fresh project is not an error).
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        match std::fs::read_to_string(&path) {
            Ok(text) => out.push(plan_record(&stem, &text, config)),
            Err(e) => errors.push(format!("plan {}: unreadable: {e}", path.display())),
        }
    }
}

/// Build the `PLAN:` digest for one plan file. Structured fields come from
/// the workflow's own parser; a legacy file that parses to empty fields
/// still indexes (the gist falls back to its first content line).
fn plan_record(stem: &str, text: &str, config: &MemorySearchConfig) -> SourceRecord {
    let parsed = PlanFile::parse(text).ok();
    let title = parsed
        .as_ref()
        .map(|p| p.title.trim().to_string())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| stem.to_string());
    let (done, total) = parsed
        .as_ref()
        .map(|p| (p.steps.iter().filter(|s| s.done).count(), p.steps.len()))
        .unwrap_or((0, 0));
    let kind = parsed
        .as_ref()
        .map(|p| p.kind.to_string())
        .unwrap_or_else(|| "implementation".to_string());
    let gist = parsed
        .as_ref()
        .and_then(|p| p.goal.lines().map(str::trim).find(|l| !l.is_empty()))
        .map(str::to_string)
        .or_else(|| first_content_line(text).map(str::to_string))
        .unwrap_or_else(|| title.clone());
    let mut pointer = format!("{done}/{total} steps · {kind} · path .coding/plans/{stem}.md");
    if let Some(hash) = first_commit_hash(text) {
        pointer.push_str(" · commit ");
        pointer.push_str(hash);
    }
    let budget = config.digest_budget(MemoryRecordType::Plan).unwrap_or(400);
    SourceRecord {
        key: format!("plan:{stem}"),
        title: truncate_title(&format!("PLAN: {title}")),
        content: budgeted_digest(&gist, &pointer, budget),
    }
}
