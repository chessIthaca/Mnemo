// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Backlog scanning + digest building for the derived-record indexer — the
//! `backlog:<id>` source family (`.coding/backlog.jsonl`, live pending items
//! only).

use std::path::Path;

use serde::Deserialize;

use crate::memory::{MemoryRecordType, MemorySearchConfig};

use super::{budgeted_digest, truncate_title, SourceRecord};

/// The backlog is parsed with a minimal local struct (not `BacklogStore`) so
/// the indexer stays read-only and a corrupt file surfaces as a report error
/// instead of a panic. Reads the jsonl format (one item per line); a legacy
/// `.json` envelope is also accepted (the store migrates on open, but the
/// indexer may run before the app ever opened the store).
#[derive(serde::Deserialize)]
struct BacklogEntry {
    /// Legacy production files carried NUMERIC ids (`"id": 1`) — accept both
    /// shapes so a pre-upgrade backlog still indexes (the store's migration
    /// stringifies; the key is `backlog:<id>` either way).
    #[serde(deserialize_with = "de_backlog_id")]
    id: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    status: String,
    /// Soft-delete marker (secs since epoch) — set by the store's soft
    /// delete; a deleted item is invisible work and must not be indexed.
    #[serde(default)]
    deleted_at: Option<u64>,
}

/// Backlog ids are UUID strings in the jsonl format but were monotonic `u64`s
/// in the legacy envelope — stringify numbers so both load.
fn de_backlog_id<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Id {
        Num(u64),
        Str(String),
    }
    match Deserialize::deserialize(deserializer)? {
        Id::Num(n) => Ok(n.to_string()),
        Id::Str(s) => Ok(s),
    }
}

/// The legacy envelope (pre-jsonl), for migration reads.
#[derive(serde::Deserialize)]
struct BacklogFile {
    #[serde(default)]
    items: Vec<BacklogEntry>,
}

/// Scan the backlog file into backlog digest sources (+ non-fatal per-source
/// errors) — only live pending items index.
pub(super) fn scan_backlog(
    path: &Path,
    out: &mut Vec<SourceRecord>,
    errors: &mut Vec<String>,
    config: &MemorySearchConfig,
) {
    // No backlog → no sources (not an error).
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let items = match parse_backlog_lines(&text) {
        Some(items) => items,
        None => match serde_json::from_str::<BacklogFile>(&text) {
            Ok(file) => file.items,
            Err(e) => {
                errors.push(format!("backlog {}: unparseable: {e}", path.display()));
                return;
            }
        },
    };
    for item in items
        .iter()
        .filter(|i| i.status == "pending" && i.deleted_at.is_none())
    {
        // Only live work is knowledge worth recalling — dispatched/resolved
        // items are history (the plans they spawned are indexed separately).
        let gist = item
            .text
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("")
            .to_string();
        let pointer = "status pending · path .coding/backlog.jsonl".to_string();
        let budget = config.digest_budget(MemoryRecordType::Plan).unwrap_or(400);
        out.push(SourceRecord {
            key: format!("backlog:{}", item.id),
            title: truncate_title(&format!("PLAN: backlog {}", item.id)),
            content: budgeted_digest(&gist, &pointer, budget),
        });
    }
}

/// Parse the jsonl backlog format (one JSON item per line). `None` when the
/// text is not line-shaped (an empty file parses to an empty vec — a
/// jsonl-format store with no items; a non-empty single-line JSON object
/// falls through to the legacy envelope parse).
fn parse_backlog_lines(text: &str) -> Option<Vec<BacklogEntry>> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // A legacy envelope starts with `{` and is a single JSON document —
        // if the first non-empty line parses as an OBJECT with `items`, the
        // whole thing is legacy (the caller handles it).
        match serde_json::from_str::<BacklogEntry>(line) {
            Ok(item) => out.push(item),
            Err(_) => return None,
        }
    }
    Some(out)
}
