// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Knowledge-corpus indexing for the derived-record indexer — the
//! `knowledge:<type>/<slug>` source family (`.coding/knowledge/<type>/*.md`):
//! scanning, supersede-metadata resolution, the targeted reindex, the
//! `[[wiki-link]]` resolution surface, and the one-time authored-row
//! migration.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::Serialize;
use uuid::Uuid;

use crate::error::{Error, Result};
use crate::memory::knowledge::{self, KnowledgeRecord, KnowledgeStatus};
use crate::memory::{
    MemoryClass, MemoryFilter, MemoryRecordType, MemorySearchConfig, MemoryStoreTrait, MemoryTier,
};

use super::{
    budgeted_digest, content_hash, first_content_line, truncate_title, write_derived, IndexReport,
    SourceRecord,
};

/// Re-index a small set of knowledge files that changed on disk (a write
/// just landed via the file-backed `KnowledgeStore`): parses them into
/// records and runs the full knowledge scan + metadata resolution + write
/// loop. Records in the set that declare `supersedes` pull their
/// PREDECESSOR files in as context (loaded and parsed from disk, chains
/// followed) so supersede resolution and the unchanged-predecessor
/// reconciliation work on the targeted path exactly as the full scan
/// performs them — `index_derived` and this helper can never diverge.
/// Non-knowledge errors from other source families are not touched.
pub async fn reindex_knowledge_files(
    store: &dyn MemoryStoreTrait,
    knowledge_dir: &Path,
    rels: &[String],
) -> Result<IndexReport> {
    // Scan phase on the blocking pool: the files may not be UTF-8 or may
    // vanish mid-write — a failed read is a report error, never a panic.
    let rels = rels.to_vec();
    let dir = knowledge_dir.to_path_buf();
    let config = store.memory_search_config();
    let (sources, records, scan_errors) = {
        let rels_scan = rels.clone();
        tokio::task::spawn_blocking(
            move || -> (Vec<SourceRecord>, Vec<KnowledgeRecord>, Vec<String>) {
                let mut sources = Vec::new();
                let mut records = Vec::new();
                let mut errors = Vec::new();
                for rel in &rels_scan {
                    let text = match std::fs::read_to_string(dir.join(rel)) {
                        Ok(t) => t,
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                            // A vanished file is a REMOVAL — the caller just
                            // deleted the record (the delete tool's file removal
                            // lands here). Skip it; the removal handling below
                            // drops the row + state.
                            continue;
                        }
                        Err(e) => {
                            errors.push(format!("knowledge {rel}: unreadable: {e}"));
                            continue;
                        }
                    };
                    match knowledge::parse_file(rel, &text) {
                        Some(record) => {
                            sources.push(knowledge_source(&record, &config));
                            records.push(record);
                        }
                        None => errors.push(format!("knowledge {rel}: unparseable record shape")),
                    }
                }
                // F1: predecessor context. `supersedes` must resolve even when
                // the predecessor is not among the requested rels — probe the
                // corpus for each unresolved slug (same type dir first, then
                // any other, mirroring `build_knowledge_metas`' preference) and
                // parse the found file as context. A context record may itself
                // supersede an older record, so the loop runs to a fixed point;
                // slugs absent on disk are remembered and left to
                // `build_knowledge_metas` to report, and the context volume is
                // capped so a pathological corpus cannot balloon one reindex.
                let mut missing_slugs: HashSet<String> = HashSet::new();
                let mut context_files = 0usize;
                loop {
                    if context_files >= MAX_PREDECESSOR_CONTEXT {
                        break;
                    }
                    let in_set: HashSet<String> = records.iter().map(|r| r.slug.clone()).collect();
                    let Some((record_type, slug)) = records
                        .iter()
                        .filter_map(|r| {
                            let slug = r.supersedes.as_deref()?;
                            (!in_set.contains(slug) && !missing_slugs.contains(slug))
                                .then(|| (r.record_type, slug.to_string()))
                        })
                        .min_by(|a, b| a.1.cmp(&b.1))
                    else {
                        break;
                    };
                    match predecessor_rel(&dir, knowledge::dir_for_record_type(record_type), &slug)
                    {
                        Some(rel) => match std::fs::read_to_string(dir.join(&rel)) {
                            Ok(text) => match knowledge::parse_file(&rel, &text) {
                                Some(record) => {
                                    sources.push(knowledge_source(&record, &config));
                                    records.push(record);
                                    context_files += 1;
                                }
                                None => {
                                    errors
                                        .push(format!("knowledge {rel}: unparseable record shape"));
                                    missing_slugs.insert(slug);
                                }
                            },
                            Err(e) => {
                                errors.push(format!("knowledge {rel}: unreadable: {e}"));
                                missing_slugs.insert(slug);
                            }
                        },
                        None => {
                            missing_slugs.insert(slug);
                        }
                    }
                }
                (sources, records, errors)
            },
        )
    }
    .await
    .map_err(|e| Error::Memory(format!("knowledge reindex task failed: {e}")))?;

    let mut report = IndexReport {
        errors: scan_errors,
        ..IndexReport::default()
    };
    let metas = build_knowledge_metas(&records, &mut report.errors);
    // The set of rel paths that were DELETED (present in the request, absent
    // on disk) — removal detection for the targeted path.
    let mut seen_keys = HashSet::new();
    for source in &sources {
        seen_keys.insert(source.key.clone());
    }
    for rel in &rels {
        let stem = rel.strip_suffix(".md").unwrap_or(rel);
        let key = format!("{}:{stem}", knowledge::KNOWLEDGE_DIR_NAME);
        if !seen_keys.contains(&key) {
            if let Some((_, memory_id)) = store.index_state_get(&key).await? {
                store.delete_memory(&memory_id).await?;
            }
            store.index_state_remove(&key).await?;
            report.removed += 1;
        }
    }
    for source in &sources {
        let hash = content_hash(source);
        let memory_id = Uuid::new_v5(&Uuid::NAMESPACE_URL, source.key.as_bytes()).to_string();
        match store.index_state_get(&source.key).await? {
            Some((old_hash, _)) if old_hash == hash => report.skipped += 1,
            Some((_, old_id)) => {
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
            None => {
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
    }
    // F1: metadata reconciliation — the targeted twin of `index_derived`'s
    // post-loop pass. An UNCHANGED predecessor (skipped by the hash check
    // above) still needs its `superseded_by` flipped when its successor
    // just arrived or changed in this set. The merge is conservative: a
    // computed value that names a real successor always wins; a computed
    // sentinel/`None` may just mean the row's own successor sits outside
    // this subset, so the row's existing (more specific) value is kept.
    for (key, meta) in &metas {
        let memory_id = Uuid::new_v5(&Uuid::NAMESPACE_URL, key.as_bytes()).to_string();
        let Some(row) = store.get_memory(&memory_id).await? else {
            continue;
        };
        let computed = meta.superseded_by.as_deref();
        let existing = row.superseded_by.as_deref();
        let superseded_by = match (computed, existing) {
            (Some(c), _) if c != knowledge::SUPERSEDED_SENTINEL => Some(c),
            (_, Some(e)) => Some(e),
            (computed, None) => computed,
        };
        store
            .set_derived_metadata(&memory_id, superseded_by, &meta.data)
            .await?;
    }
    Ok(report)
}

/// Cap on predecessor-context files pulled into one targeted reindex —
/// chains are short and rare; the cap only bounds a pathological corpus.
const MAX_PREDECESSOR_CONTEXT: usize = 64;

/// The on-disk rel path of a predecessor referenced by `supersedes = "<slug>"`
/// — `<own type dir>/<slug>.md` first, then any other type dir (sorted, for
/// determinism), `None` when no such file exists. Mirrors the resolution
/// preference of [`build_knowledge_metas`] and the probe
/// [`crate::memory::knowledge::KnowledgeStore::resolve`] performs.
fn predecessor_rel(dir: &Path, own_dir: Option<&str>, slug: &str) -> Option<String> {
    let mut dirs: Vec<String> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    dirs.sort();
    if let Some(own) = own_dir {
        dirs.sort_by_key(|d| d != own); // stable — own type dir first
    }
    for d in dirs {
        let rel = format!("{d}/{slug}.md");
        if dir.join(&rel).is_file() {
            return Some(rel);
        }
    }
    None
}

/// The derived metadata a knowledge row carries beyond title/content: the
/// links `data` JSON and the resolved `superseded_by` (a successor memory id
/// or [`knowledge::SUPERSEDED_SENTINEL`]).
pub(super) struct KnowledgeMeta {
    pub(super) superseded_by: Option<String>,
    pub(super) data: serde_json::Value,
}

/// Scan `.coding/knowledge/<type>/*.md` (the four knowledge type dirs, flat)
/// into both a digest source AND the parsed record — the record feeds the
/// metadata resolution (a successor file changes its predecessor's row), the
/// source feeds the hash-driven write loop. A missing type dir is not an
/// error (fresh projects have no knowledge yet).
pub(super) fn scan_knowledge(
    knowledge_dir: &Path,
    out: &mut Vec<SourceRecord>,
    records: &mut Vec<KnowledgeRecord>,
    errors: &mut Vec<String>,
    config: &MemorySearchConfig,
) {
    const TYPES: [MemoryRecordType; 4] = [
        MemoryRecordType::Spec,
        MemoryRecordType::Decision,
        MemoryRecordType::Bug,
        MemoryRecordType::How,
    ];
    for record_type in TYPES {
        let Some(dir_name) = knowledge::dir_for_record_type(record_type) else {
            continue;
        };
        let dir = knowledge_dir.join(dir_name);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let file_name = entry.file_name().to_string_lossy().to_string();
            // Forward-slash rel path regardless of platform separators —
            // `knowledge::parse_file` splits on '/'.
            let rel = format!("{dir_name}/{file_name}");
            match std::fs::read_to_string(&path) {
                Ok(text) => match knowledge::parse_file(&rel, &text) {
                    Some(record) => {
                        out.push(knowledge_source(&record, config));
                        records.push(record);
                    }
                    None => errors.push(format!("knowledge {rel}: unparseable record shape")),
                },
                Err(e) => errors.push(format!("knowledge {rel}: unreadable: {e}")),
            }
        }
    }
}

/// Build the digest `SourceRecord` for one knowledge record: a budgeted gist
/// (the file body is unbounded — the digest is the pointer) plus the
/// mandatory pointer line to the file, which is the truth. `HOW:` has no
/// typed budget; its digest uses the SPEC/REVIEW default of 500.
fn knowledge_source(record: &KnowledgeRecord, config: &MemorySearchConfig) -> SourceRecord {
    let gist = first_content_line(&record.body)
        .unwrap_or(record.title.as_str())
        .to_string();
    let budget = config.digest_budget(record.record_type).unwrap_or(500);
    SourceRecord {
        key: record.source_key(),
        title: truncate_title(&record.typed_title()),
        content: budgeted_digest(&gist, &record.pointer_line(), budget),
    }
}

/// Resolve the desired metadata for every knowledge record, keyed by source
/// key. The supersede model mirrors the front matter: a successor's
/// `supersedes = "<slug>"` points at its predecessor, and the PREDECESSOR's
/// row carries `superseded_by = <successor memory id>` (or the sentinel when
/// the front matter says superseded but no successor file exists).
/// Deterministic — records are sorted by rel path first because `read_dir`
/// order varies by OS; the first successor to claim a predecessor wins and
/// later claimants surface as report errors, never silent drops.
pub(super) fn build_knowledge_metas(
    records: &[KnowledgeRecord],
    errors: &mut Vec<String>,
) -> HashMap<String, KnowledgeMeta> {
    let mut sorted: Vec<&KnowledgeRecord> = records.iter().collect();
    sorted.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));

    // Predecessor source key → successor memory id (first claimant wins).
    let mut claimed: HashMap<String, String> = HashMap::new();
    for successor in sorted.iter().filter(|r| r.supersedes.is_some()) {
        let slug = successor.supersedes.as_deref().unwrap_or_default();
        if slug == successor.slug {
            errors.push(format!(
                "knowledge {}: supersedes itself",
                successor.rel_path
            ));
            continue;
        }
        // Prefer a predecessor in the same type dir, then any type with
        // that slug.
        let pred_key = sorted
            .iter()
            .find(|r| r.slug == slug && r.record_type == successor.record_type)
            .or_else(|| sorted.iter().find(|r| r.slug == slug))
            .map(|r| r.source_key());
        match pred_key {
            Some(key) => {
                let successor_id =
                    Uuid::new_v5(&Uuid::NAMESPACE_URL, successor.source_key().as_bytes())
                        .to_string();
                match claimed.entry(key) {
                    Entry::Occupied(_) => errors.push(format!(
                        "knowledge {}: predecessor '{slug}' claimed by multiple successors",
                        successor.rel_path
                    )),
                    Entry::Vacant(slot) => {
                        slot.insert(successor_id);
                    }
                }
            }
            None => errors.push(format!(
                "knowledge {}: supersedes '{slug}' not found",
                successor.rel_path
            )),
        }
    }

    sorted
        .iter()
        .map(|record| {
            let superseded_by = match claimed.get(&record.source_key()) {
                Some(successor_id) => Some(successor_id.clone()),
                // Self-declared dead with no successor file — the sentinel
                // keeps the row out of recall without inventing an id.
                None if record.status == KnowledgeStatus::Superseded => {
                    Some(knowledge::SUPERSEDED_SENTINEL.to_string())
                }
                None => None,
            };
            let data = serde_json::json!({
                "kind": "knowledge",
                "rel_path": record.rel_path,
                "links": record.links,
            });
            (
                record.source_key(),
                KnowledgeMeta {
                    superseded_by,
                    data,
                },
            )
        })
        .collect()
}

/// The deterministic derived-memory id for a knowledge file's rel path
/// (`.coding/knowledge/<type>/<slug>.md`) — the UUIDv5 of its source key
/// (`knowledge:<type>/<slug>`, WITHOUT the `.md` extension, matching the
/// indexer's `source_key()`). The extension is stripped so the tool-side
/// id (reported on write) and the indexer-side id (the actual row) always
/// agree. Shared by the indexer and the knowledge-backed memory tools so
/// the tools can report the row id of a record they just wrote.
pub fn knowledge_id(rel_path: &str) -> String {
    let stem = rel_path.strip_suffix(".md").unwrap_or(rel_path);
    let key = format!("{}:{stem}", knowledge::KNOWLEDGE_DIR_NAME);
    Uuid::new_v5(&Uuid::NAMESPACE_URL, key.as_bytes()).to_string()
}

/// A resolved `[[wiki-link]]` target — what the target points at in the
/// current index, derived deterministically (no scans).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedLink {
    /// `"knowledge"` / `"plan"` / `"review"` / `"file"` / `"other"` (mirrors
    /// [`KnowledgeLinkKind`]).
    pub kind: String,
    /// The original target, verbatim.
    pub target: String,
    /// The repo-relative path of the target's file, when meaningful
    /// (`.coding/knowledge/...`, `.coding/plans/...`, or the raw file path).
    pub rel_path: Option<String>,
    /// The derived memory id of the target record, when one exists.
    pub memory_id: Option<String>,
    /// The target record's title, when resolved.
    pub title: Option<String>,
    /// The target record's content (truncated), when resolved.
    pub snippet: Option<String>,
}

/// Resolve a link target to its current record/file. Deterministic: the
/// knowledge/plan/review row ids are UUIDv5s of the source keys, so the
/// resolution is pure id math + one point lookup each — no corpus scan.
/// File-ish targets (anything else) resolve to a `file`-kind entry carrying
/// the path; unknown shapes resolve to `other`. Never fails on a missing
/// target — `Ok(None)` is reserved for malformed targets, a missing row
/// resolves to the path-only entry.
pub async fn resolve_link(
    store: &dyn MemoryStoreTrait,
    target: &str,
) -> Result<Option<ResolvedLink>> {
    let target = target.trim();
    if target.is_empty() {
        return Ok(None);
    }
    // plan/<id> → the plan row (deterministic id).
    if let Some(id) = target.strip_prefix("plan/") {
        let memory_id =
            Uuid::new_v5(&Uuid::NAMESPACE_URL, format!("plan:{id}").as_bytes()).to_string();
        let rel_path = format!(".coding/plans/{id}.md");
        return Ok(Some(
            row_link(store, "plan", target, &rel_path, &memory_id).await,
        ));
    }
    // review/<stem> → the review row.
    if let Some(stem) = target.strip_prefix("review/") {
        let memory_id =
            Uuid::new_v5(&Uuid::NAMESPACE_URL, format!("review:{stem}").as_bytes()).to_string();
        let rel_path = format!(".coding/reviews/{stem}.md");
        return Ok(Some(
            row_link(store, "review", target, &rel_path, &memory_id).await,
        ));
    }
    // spec|decision|bug|how/<slug> → the knowledge row (deterministic id).
    for dir in ["spec/", "decision/", "bug/", "how/"] {
        if target.starts_with(dir) {
            let rel_path = if target.ends_with(".md") {
                target.to_string()
            } else {
                format!("{target}.md")
            };
            let memory_id = knowledge_id(&rel_path);
            return Ok(Some(
                row_link(store, "knowledge", target, &rel_path, &memory_id).await,
            ));
        }
    }
    // A bare path-like target → the file itself.
    Ok(Some(ResolvedLink {
        kind: "file".to_string(),
        target: target.to_string(),
        rel_path: Some(target.to_string()),
        memory_id: None,
        title: None,
        snippet: None,
    }))
}

/// Build a `ResolvedLink` from a deterministic row id: fills title/snippet
/// from the live row (a superseded or deleted row yields an id-less entry —
/// the path itself is still the truth).
async fn row_link(
    store: &dyn MemoryStoreTrait,
    kind: &str,
    target: &str,
    rel_path: &str,
    memory_id: &str,
) -> ResolvedLink {
    let mut link = ResolvedLink {
        kind: kind.to_string(),
        target: target.to_string(),
        rel_path: Some(rel_path.to_string()),
        memory_id: None,
        title: None,
        snippet: None,
    };
    if let Ok(Some(m)) = store.get_memory(memory_id).await {
        link.memory_id = Some(m.id);
        link.title = Some(m.title);
        link.snippet = Some(m.content.chars().take(200).collect());
    }
    link
}

/// Every live memory whose `data.links` includes `rel` (matched on the
/// `.md`-stripped stem, so `decision/2026-08-23-x` matches
/// `decision/2026-08-23-x.md` and vice versa) — the "referenced by"
/// reverse surface for the knowledge UI. Scans the store (project-scale
/// corpus, hundreds of rows), not the filesystem.
pub async fn backlinks_for(
    store: &dyn MemoryStoreTrait,
    rel: &str,
) -> Result<Vec<crate::memory::Memory>> {
    let stem = rel.strip_suffix(".md").unwrap_or(rel).trim();
    let all = store.list_filtered(&MemoryFilter::new()).await?;
    Ok(all
        .into_iter()
        .filter(|m| {
            let Some(links) = m.data.get("links").and_then(|v| v.as_array()) else {
                return false;
            };
            links.iter().any(|l| {
                l.get("target")
                    .and_then(|t| t.as_str())
                    .map(|t| t.trim_end_matches(".md").trim() == stem)
                    .unwrap_or(false)
            })
        })
        .collect())
}

/// What one migration run did.
#[derive(Debug, Default)]
pub struct MigrateReport {
    /// How many authored typed rows became knowledge files.
    pub migrated: usize,
    /// Rows skipped with the reason (`<id>: <reason>`) — never fatal.
    pub skipped: Vec<String>,
}

/// One-time migration of pre-existing AUTHORED typed rows
/// (SPEC:/DECISION:/BUG:/HOW: in the semantic/procedural tiers) into
/// knowledge FILES. After this runs, the file is the truth and the row is
/// deleted — the derived index rebuilds the row from the file, so the
/// migration is naturally idempotent: a migrated row no longer exists and a
/// re-run is a no-op. A partially-failed run (file written, row delete
/// failed) heals on the next run via the same-title overwrite.
///
/// The one-time guard is a marker file in the knowledge dir
/// (`.migration-authored-v1`, plan step 7): once a run completes, later runs
/// skip the scan entirely. The marker is a dotfile — invisible to the
/// corpus scan (which only picks up `*.md`) — and travels with git. The
/// row-level idempotency above stays as the safety net (a future path that
/// intentionally re-inserts authored typed rows must also address the
/// knowledge-file truth, or clear the marker).
///
/// Rows WITHOUT a knowledge home stay put: PLAN:/REVIEW: rows (their plans
/// are already files), untyped rows, working/episodic-tier rows, and
/// superseded rows (history). Nothing is ever destructive beyond the row
/// the file replaced.
pub async fn migrate_authored_typed_rows(
    store: &dyn MemoryStoreTrait,
    knowledge: &crate::memory::KnowledgeStore,
) -> Result<MigrateReport> {
    let mut report = MigrateReport::default();
    // One-time guard: a completed migration never rescans (the row-level
    // idempotency below stays as the safety net).
    let marker = knowledge.dir().join(MIGRATION_MARKER);
    if marker.exists() {
        return Ok(report);
    }
    let rows = store.list_filtered(&MemoryFilter::new()).await?;
    for m in rows {
        // Only live authored rows in distilled tiers.
        if m.record_class != MemoryClass::Authored {
            continue;
        }
        if m.tier != MemoryTier::Semantic && m.tier != MemoryTier::Procedural {
            continue;
        }
        let Some(record_type) = knowledge::dir_for_record_type(m.record_type) else {
            continue; // PLAN/REVIEW/None have no knowledge home
        };
        let Some(bare_title) = strip_knowledge_prefix(&m.title, record_type) else {
            continue;
        };
        // The file slug derives from the row's creation date + title (the
        // same rule the writer uses for new records).
        let date = knowledge_fmt_date(m.created_at);
        let slug = knowledge::slug_for(bare_title, &date);
        // Safety: never overwrite a DIFFERENT record's file (a same-slug
        // collision). The same title+date re-migrates idempotently (the
        // partial-failure heal); anything else is skipped, not destroyed.
        let rel = format!("{}/{}", record_type, slug);
        if let Some(existing) =
            std::fs::read_to_string(knowledge.dir().join(format!("{rel}.md"))).ok()
        {
            let parsed = knowledge::parse_file(&format!("{rel}.md"), &existing);
            if parsed.as_ref().map(|r| r.title.as_str()) != Some(bare_title) {
                report.skipped.push(format!(
                    "{}: slug collision with a different record ({rel}.md)",
                    m.id
                ));
                continue;
            }
        }
        if let Err(e) = knowledge.write_at(m.record_type, &slug, bare_title, &m.content) {
            report.skipped.push(format!("{}: {e}", m.id));
            continue;
        }
        // The row is history — the file is the truth now. Fire-and-forget
        // stays (the file write above already succeeded), but a zombie row
        // duplicating the file must not vanish silently — log it (quality
        // review LOW 6, class-closure).
        if let Err(e) = store.delete_memory(&m.id).await {
            eprintln!("mnemo: failed to delete migrated row {}: {e}", m.id);
        }
        report.migrated += 1;
    }
    // Latch the migration as completed (best-effort — the row-level
    // idempotency keeps re-runs safe even if the marker can't be written).
    let _ = std::fs::create_dir_all(knowledge.dir());
    let _ = std::fs::write(
        &marker,
        "authored typed rows migrated to knowledge files (v1) — the guard for migrate_authored_typed_rows\n",
    );
    Ok(report)
}

/// The migration's one-time guard marker (a knowledge-dir dotfile — not a
/// `*.md`, so the corpus scan ignores it).
const MIGRATION_MARKER: &str = ".migration-authored-v1";

/// The bare title (typed prefix stripped) for a knowledge record type —
/// `None` when the prefix doesn't match the type (defensive; the type came
/// from the same classification).
fn strip_knowledge_prefix<'a>(title: &'a str, record_type: &str) -> Option<&'a str> {
    let prefix = format!("{}: ", record_type.to_uppercase());
    title.strip_prefix(&prefix)
}

/// Format epoch seconds as `YYYY-MM-DD` (the knowledge slug's date part).
/// Mirrors `knowledge::fmt_date` — kept here so the migration doesn't need
/// the writer's private helper.
fn knowledge_fmt_date(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!("{year:04}-{month:02}-{day:02}")
}
