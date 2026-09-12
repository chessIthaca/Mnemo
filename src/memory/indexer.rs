// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Phase 2 derived-record indexer — scans the project's `.coding/` corpus
//! (plans, review reports, backlog, and the knowledge corpus of typed
//! semantic records) and emits budgeted typed digest memories
//! (`record_class = Derived`) that point back at the on-disk truth.
//!
//! Design invariants:
//!
//! - **Idempotent + incremental.** Every source has a stable key
//!   (`plan:<id>` / `review:<file-stem>` / `backlog:<id>`); its memory id is
//!   the UUIDv5 of that key, and the content hash lives in
//!   `derived_index_state`. A re-run skips unchanged sources entirely (no
//!   re-embed, no write) and updates changed ones in place.
//! - **Removal detection.** State keys not seen on the current run lose
//!   their derived memory and their state row.
//! - **Pointer-first.** Digests are budgeted by construction — the pointer
//!   line (`path …` / `commit …`) is mandatory and the gist truncates to fit
//!   whatever room remains; a digest must never lose its pointer.
//! - **Graceful degradation.** A file that fails structured parsing still
//!   indexes (title + path + first content line); one bad file lands in
//!   [`IndexReport::errors`] and never aborts the run.
//!
//! Layout: the per-source indexers live in submodules — `plans`, `reviews`,
//! `backlog`, and `knowledge` (which also carries the targeted knowledge
//! reindex, the `[[wiki-link]]` resolution surface, and the authored-row
//! migration) — re-exported here so the public surface stays
//! `crate::memory::indexer::*`.

use std::collections::HashSet;
use std::path::Path;

use uuid::Uuid;

use crate::error::{Error, Result};
use crate::memory::knowledge::{KNOWLEDGE_DIR_NAME, KnowledgeRecord};
use crate::memory::{Memory, MemoryClass, MemorySearchConfig, MemoryStoreTrait, MemoryTier};

mod backlog;
mod knowledge;
mod plans;
mod reviews;

use backlog::scan_backlog;
use knowledge::{build_knowledge_metas, scan_knowledge};
use plans::scan_plans;
use reviews::scan_reviews;

// Public surface: the knowledge-family items live in the `knowledge`
// submodule and are re-exported here so `crate::memory::indexer::*` paths
// are unchanged for every consumer.
pub use knowledge::{
    backlinks_for, knowledge_id, migrate_authored_typed_rows, reindex_knowledge_files,
    resolve_link, MigrateReport, ResolvedLink,
};

/// What an index run did — surfaced by the IPC layer as the completion
/// summary (mirrors [`crate::memory::maintenance::RebuildReport`]).
#[derive(Debug, Default)]
pub struct IndexReport {
    /// Sources (re)written this run (new or changed content).
    pub indexed: usize,
    /// Sources skipped unchanged (content hash matched the state table).
    pub skipped: usize,
    /// Derived memories removed because their source vanished.
    pub removed: usize,
    /// Per-source problems (degraded parses, unreadable files). The run
    /// continues past these; they surface for visibility.
    pub errors: Vec<String>,
}

/// One scanned source, ready to diff against the index state.
struct SourceRecord {
    /// Stable identity: `plan:<id>` / `review:<stem>` / `backlog:<id>`.
    key: String,
    /// The memory title (typed prefix included).
    title: String,
    /// The budgeted digest content (fits its typed budget by construction).
    content: String,
}

/// Index all derived sources of the project: `.coding/plans/*.md`,
/// `.coding/reviews/*.md` (the plans dir's sibling), the pending items of
/// `backlog.jsonl`, and the knowledge corpus (`.coding/knowledge/<type>/*.md`
/// — typed semantic records whose FILES are the truth and whose derived rows
/// are pointers). Incremental: unchanged sources skip via the
/// `derived_index_state` content hash; changed sources update in place
/// (same memory id, re-embedded); vanished sources are removed. `progress`
/// ticks `(done, total)` after each processed source — throttling is the
/// caller's concern (the IPC layer uses its `should_forward_progress`).
pub async fn index_derived(
    store: &dyn MemoryStoreTrait,
    plans_dir: &Path,
    backlog_path: &Path,
    progress: &(dyn Fn(usize, usize) + Send + Sync),
) -> Result<IndexReport> {
    // The scan phase is blocking fs (hundreds of small files) — one hop onto
    // the blocking pool; each store op then does its own per-call hop.
    let plans = plans_dir.to_path_buf();
    let backlog = backlog_path.to_path_buf();
    // The knowledge dir is the plans dir's sibling (`.coding/knowledge/`),
    // mirroring how the reviews dir is derived.
    let knowledge_dir = plans_dir
        .parent()
        .map(|p| p.join(KNOWLEDGE_DIR_NAME));
    let config = store.memory_search_config();
    let (sources, knowledge_records, scan_errors) = tokio::task::spawn_blocking(move || {
        scan_sources(&plans, &backlog, knowledge_dir.as_deref(), &config)
    })
    .await
    .map_err(|e| Error::Memory(format!("index scan task failed: {e}")))?;

    let mut report = IndexReport {
        errors: scan_errors,
        ..IndexReport::default()
    };
    // Knowledge rows carry extra derived metadata (the links `data` JSON +
    // supersede state) resolved from the FULL corpus — a successor file
    // changes its predecessor's row, so resolution happens before the
    // per-source hash loop.
    let metas = build_knowledge_metas(&knowledge_records, &mut report.errors);
    let mut knowledge_touched = false;
    let total = sources.len();
    let mut seen: HashSet<String> = HashSet::with_capacity(total);
    for (done, source) in sources.iter().enumerate() {
        seen.insert(source.key.clone());
        let hash = content_hash(source);
        let memory_id = Uuid::new_v5(&Uuid::NAMESPACE_URL, source.key.as_bytes()).to_string();
        match store.index_state_get(&source.key).await? {
            // Unchanged: the stored hash matches — no re-embed, no write.
            // (A knowledge row may still need metadata reconciliation — the
            // post-loop pass handles that, not the skip path.)
            Some((old_hash, _)) if old_hash == hash => report.skipped += 1,
            // Changed: update in place (re-embeds + re-derives the record
            // type from the title). If the row vanished out from under the
            // state table, fall back to a fresh write. The in-place update
            // leaves `superseded_by`/`data` alone for the post-loop
            // reconciliation pass to re-assert.
            Some((_, old_id)) => {
                knowledge_touched |= metas.contains_key(&source.key);
                let id = if store
                    .update_memory(&old_id, Some(&source.title), Some(&source.content))
                    .await?
                {
                    old_id
                } else {
                    write_derived(
                        store,
                        &memory_id,
                        source,
                        metas.get(&source.key).map(|m| &m.data),
                        metas
                            .get(&source.key)
                            .and_then(|m| m.superseded_by.as_deref()),
                    )
                    .await?;
                    memory_id
                };
                store.index_state_upsert(&source.key, &hash, &id).await?;
                report.indexed += 1;
            }
            // New source.
            None => {
                knowledge_touched |= metas.contains_key(&source.key);
                let meta = metas.get(&source.key);
                write_derived(
                    store,
                    &memory_id,
                    source,
                    meta.map(|m| &m.data),
                    meta.and_then(|m| m.superseded_by.as_deref()),
                )
                .await?;
                store
                    .index_state_upsert(&source.key, &hash, &memory_id)
                    .await?;
                report.indexed += 1;
            }
        }
        progress(done + 1, total);
    }
    // Removal detection: state keys not seen this run lose memory + state.
    for key in store.index_state_keys().await? {
        if !seen.contains(&key) {
            if let Some((_, memory_id)) = store.index_state_get(&key).await? {
                store.delete_memory(&memory_id).await?;
            }
            store.index_state_remove(&key).await?;
            knowledge_touched |= key.starts_with("knowledge:");
            report.removed += 1;
        }
    }
    // Knowledge reconciliation: when the corpus changed at all, re-assert
    // every knowledge row's metadata. This is the git-merge convergence
    // case — an UNCHANGED predecessor (skipped by the hash check above)
    // still needs its `superseded_by` flipped when a successor file arrived
    // via a merge, and rows already carrying the right values rewrite them
    // identically (idempotent point ops keyed by the deterministic id).
    if knowledge_touched {
        for (key, meta) in &metas {
            let memory_id = Uuid::new_v5(&Uuid::NAMESPACE_URL, key.as_bytes()).to_string();
            if store.get_memory(&memory_id).await?.is_some() {
                store
                    .set_derived_metadata(&memory_id, meta.superseded_by.as_deref(), &meta.data)
                    .await?;
            }
        }
    }
    Ok(report)
}

/// Rebuild the derived index from scratch: wipe every derived memory + the
/// incremental state table, then run a full index. Authored memories are
/// sacred — `delete_derived` is class-scoped and never touches them (the
/// wipe/rescan is what the Settings → Memory "Rebuild index" button runs).
pub async fn rebuild_derived(
    store: &dyn MemoryStoreTrait,
    plans_dir: &Path,
    backlog_path: &Path,
    progress: &(dyn Fn(usize, usize) + Send + Sync),
) -> Result<IndexReport> {
    store.delete_derived().await?;
    store.index_state_clear().await?;
    index_derived(store, plans_dir, backlog_path, progress).await
}

/// Insert a new derived memory for a source. `record_type` is derived from
/// the title's typed prefix inside `write` (the prefix wins — the same rule
/// as every write path). Knowledge sources additionally carry their derived
/// metadata: the links `data` JSON and the resolved `superseded_by`
/// (plans/reviews/backlog pass `None`/`None`).
async fn write_derived(
    store: &dyn MemoryStoreTrait,
    id: &str,
    source: &SourceRecord,
    data: Option<&serde_json::Value>,
    superseded_by: Option<&str>,
) -> Result<()> {
    let mut memory = Memory::new(
        MemoryTier::Semantic,
        source.title.clone(),
        source.content.clone(),
        utc_now_secs(),
    );
    memory.id = id.to_string();
    memory.record_class = MemoryClass::Derived;
    if let Some(data) = data {
        memory.data = data.clone();
    }
    memory.superseded_by = superseded_by.map(str::to_string);
    store.write(memory).await?;
    Ok(())
}

// ── Scanning ────────────────────────────────────────────────────────────────

/// Scan all source kinds into records (+ non-fatal per-source errors):
/// plans, reviews, the knowledge corpus, and the backlog.
fn scan_sources(
    plans_dir: &Path,
    backlog_path: &Path,
    knowledge_dir: Option<&Path>,
    config: &MemorySearchConfig,
) -> (Vec<SourceRecord>, Vec<KnowledgeRecord>, Vec<String>) {
    let mut out = Vec::new();
    let mut knowledge = Vec::new();
    let mut errors = Vec::new();
    scan_plans(plans_dir, &mut out, &mut errors, config);
    // The reviews dir is the plans dir's sibling (`.coding/reviews/`).
    if let Some(reviews_dir) = plans_dir.parent().map(|p| p.join("reviews")) {
        scan_reviews(&reviews_dir, &mut out, &mut errors, config);
    }
    if let Some(knowledge_dir) = knowledge_dir {
        scan_knowledge(knowledge_dir, &mut out, &mut knowledge, &mut errors, config);
    }
    scan_backlog(backlog_path, &mut out, &mut errors, config);
    (out, knowledge, errors)
}

/// Cheap staleness pre-check for the startup reconciliation: does the
/// on-disk corpus (plans, reviews, knowledge records, backlog) differ from
/// what the derived index state already covers? Walks the same sources the
/// indexer scans, compares content hashes against `derived_index_state`,
/// and includes removal detection (a state key with no on-disk source is a
/// change). A missing/empty state table (fresh project, or a deleted
/// `memory.db` awaiting a rebuild) counts as stale — the run then rebuilds
/// from the files, which are the truth.
///
/// This is what lets the startup reconcile stay SILENT when nothing
/// changed (no events, no dialog) and only stream progress + show the
/// wait dialog when the corpus actually drifted (a git merge landed, a
/// record was edited, the DB was deleted).
pub async fn corpus_is_stale(
    store: &dyn MemoryStoreTrait,
    plans_dir: &Path,
    backlog_path: &Path,
) -> Result<bool> {
    let plans = plans_dir.to_path_buf();
    let backlog = backlog_path.to_path_buf();
    let knowledge_dir = plans_dir
        .parent()
        .map(|p| p.join(KNOWLEDGE_DIR_NAME));
    let config = store.memory_search_config();
    let (sources, _knowledge, scan_errors) = tokio::task::spawn_blocking(move || {
        scan_sources(&plans, &backlog, knowledge_dir.as_deref(), &config)
    })
    .await
    .map_err(|e| Error::Memory(format!("staleness scan task failed: {e}")))?;

    let state_keys = store.index_state_keys().await?;
    let mut seen: HashSet<String> = HashSet::with_capacity(sources.len());
    for source in &sources {
        seen.insert(source.key.clone());
        match store.index_state_get(&source.key).await? {
            Some((old_hash, _)) if old_hash == content_hash(source) => {}
            _ => return Ok(true), // new or changed source
        }
    }
    // Removal detection: any indexed key whose source vanished is stale.
    if state_keys.iter().any(|k| !seen.contains(k)) {
        return Ok(true);
    }
    // A scan error (unreadable source) is not "stale" — the index run would
    // report it; treat it as unchanged so the dialog doesn't appear for a
    // transient read failure (the error surfaces in the report instead).
    if !scan_errors.is_empty() {
        eprintln!("warning: reconcile staleness scan: {:?}", scan_errors);
    }
    Ok(false)
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// The first non-empty, non-heading line — the fallback gist for legacy
/// files without structured sections.
fn first_content_line(text: &str) -> Option<&str> {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
}

/// Assemble a two-line digest that ALWAYS fits `budget`: the pointer line is
/// mandatory; the gist is char-safely truncated to the room that remains.
/// (A digest that loses its pointer is useless — pointer-first by
/// construction, not by after-the-fact truncation.) Shared with the finish
/// auto-capture (`finish_capture`) so both digest builders stay in sync.
pub(crate) fn budgeted_digest(gist: &str, pointer: &str, budget: usize) -> String {
    let room = budget.saturating_sub(pointer.chars().count() + 1);
    let gist: String = gist.chars().take(room).collect();
    if gist.is_empty() {
        pointer.to_string()
    } else {
        format!("{gist}\n{pointer}")
    }
}

/// Cap a memory title — plan/review headings can run to a paragraph, but the
/// title is a label, not the digest.
fn truncate_title(title: &str) -> String {
    const MAX_TITLE: usize = 120;
    if title.chars().count() <= MAX_TITLE {
        title.to_string()
    } else {
        title.chars().take(MAX_TITLE).collect()
    }
}

/// The first commit-hash-looking token in the text (7–40 lowercase hex with
/// at least one digit, so words like `facaded` don't false-positive).
/// Plan-referenced commits only — the indexer NEVER walks git log. Shared
/// with the finish auto-capture (`finish_capture`).
pub(crate) fn first_commit_hash(text: &str) -> Option<&str> {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"\b[0-9a-f]{7,40}\b").expect("commit-hash regex compiles")
    });
    re.find_iter(text)
        .map(|m| m.as_str())
        .find(|s| s.bytes().any(|b| b.is_ascii_digit()))
}

/// FNV-1a 64-bit over title + content — a tiny persist-stable content hash
/// for the incremental skip check. (std's DefaultHasher is explicitly NOT
/// stable across builds, so it must never be persisted; a sha2 dependency
/// would be overkill here.)
fn content_hash(source: &SourceRecord) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in source
        .title
        .as_bytes()
        .iter()
        .chain(source.content.as_bytes())
    {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Wall-clock seconds since the Unix epoch — `created_at` for new derived
/// memories (indexing happens live; re-indexes keep the original stamp via
/// `update_memory`).
fn utc_now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;
