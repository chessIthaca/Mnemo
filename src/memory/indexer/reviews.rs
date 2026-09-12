// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Review-report scanning + digest building for the derived-record indexer —
//! the `review:<stem>` source family (`.coding/reviews/*.md`).

use std::path::Path;

use crate::memory::{MemoryRecordType, MemorySearchConfig};

use super::{budgeted_digest, first_content_line, truncate_title, SourceRecord};

/// Scan the reviews dir into review digest sources (+ non-fatal per-source
/// errors).
pub(super) fn scan_reviews(
    dir: &Path,
    out: &mut Vec<SourceRecord>,
    errors: &mut Vec<String>,
    config: &MemorySearchConfig,
) {
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
            Ok(text) => out.push(review_record(&stem, &text, config)),
            Err(e) => errors.push(format!("review {}: unreadable: {e}", path.display())),
        }
    }
}

/// Build the `REVIEW:` digest for one review report. The verdict comes from
/// the Phase-4 `## Verdict:` line when present; legacy reports fall back to
/// a findings summary ("no findings" / "N finding sections").
fn review_record(stem: &str, text: &str, config: &MemorySearchConfig) -> SourceRecord {
    let heading = text
        .lines()
        .map(str::trim)
        .find_map(|l| l.strip_prefix("# "))
        .map(|h| h.trim_start_matches("Review:").trim().to_string())
        .filter(|t| !t.is_empty());
    let gist = heading
        .or_else(|| first_content_line(text).map(str::to_string))
        .unwrap_or_else(|| stem.to_string());
    let summary = if let Some(verdict) = text
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("## Verdict"))
    {
        verdict.trim_start_matches('#').trim().to_string()
    } else if text.contains("**No findings.**") {
        "no findings".to_string()
    } else {
        let n = text.lines().filter(|l| l.starts_with("### ")).count();
        if n > 0 {
            format!("{n} finding sections")
        } else {
            "findings not parsed".to_string()
        }
    };
    let pointer = format!("{summary} · path .coding/reviews/{stem}.md");
    let budget = config
        .digest_budget(MemoryRecordType::Review)
        .unwrap_or(500);
    SourceRecord {
        key: format!("review:{stem}"),
        title: truncate_title(&format!("REVIEW: {stem}")),
        content: budgeted_digest(&gist, &pointer, budget),
    }
}
