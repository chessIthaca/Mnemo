// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The Memory tab's command surface: the project's knowledge records and
//! memories as one newest-first list, a text search over them, one record's
//! full Markdown (the knowledge FILE's body when the row derives from one),
//! its backlinks, and the Diagnostics card (embedder status + model, stored
//! vector fingerprints, per-tier counts, the derived index's bookkeeping and a
//! recall test).
//!
//! Every answer is read from the open project's memory store — the same store
//! [`mnemo::headless::MemoryApi`] wraps, so the tab and the agent's memory
//! tools can never disagree about what the project knows. The tab is strictly
//! READ-ONLY: no command here writes, edits, supersedes or deletes a record.
//!
//! Why the store trait and not `MemoryApi` everywhere: `MemoryApi::list` is
//! tier-only (no typed-record filter, no paging) and truncates every body to
//! its wire budget, while the tab browses knowledge records by TYPE (spec /
//! decision / bug / how) and reads one body in full. The list, the search and
//! the single-record read therefore go through the store's
//! `list_filtered` / `recall_peek` / `get_memory`, i.e. the same methods
//! `MemoryApi` itself calls; the diagnostics card and the recall test use the
//! API.
//!
//! Defaults that shape what the tab shows: the list excludes the working tier
//! (raw tool-event captures — consolidation input, not knowledge), superseded
//! records (history, reachable through Mnemo1) and quarantined rows (the
//! startup digest sweep's finds), exactly like recall does. An explicit
//! `tier: "working"` filter opts back in.
//!
//! Errors vs. empty answers: no open project, or a project running with memory
//! off, is a plain error string the frontend renders — except for
//! [`memory_overview`], which reports `available: false` with the reason,
//! because the Diagnostics card's job is to explain that state rather than
//! fail.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use serde::Serialize;
use tauri::State;

use mnemo::headless::{Engine, MemoryApi, MemoryCounts, MemoryDebugWire, MemoryIndexStatus};
use mnemo::memory::indexer::backlinks_for;
use mnemo::memory::knowledge::{parse_file, KnowledgeStatus, KNOWLEDGE_DIR_NAME};
use mnemo::memory::{
    Memory, MemoryClass, MemoryFilter, MemoryRecordType, MemoryStoreTrait, MemoryTier,
};

use crate::mnemo2::ipc::Mnemo2State;

/// Default page size for [`memory_list`].
pub const LIST_DEFAULT_LIMIT: usize = 40;

/// Hard ceiling on one [`memory_list`] page (a bigger `limit` is clamped, not
/// rejected — the tab drives this from a scroll, not a form).
pub const LIST_MAX_LIMIT: usize = 200;

/// Default result count for [`memory_search`].
pub const SEARCH_DEFAULT_LIMIT: usize = 30;

/// Hard ceiling on one [`memory_search`] answer.
pub const SEARCH_MAX_LIMIT: usize = 100;

/// Default result count for the Diagnostics recall test.
pub const RECALL_DEFAULT_LIMIT: usize = 8;

/// Hard ceiling on one recall-test answer.
pub const RECALL_MAX_LIMIT: usize = 50;

/// How many chars of a record's body the list row carries, so the list renders
/// without a read per row.
pub const PREVIEW_CHARS: usize = 180;

/// How many rows the "other" chip scans before paging. That chip spans three
/// record types (plan / review / none), which one SQL `record_type = ?` cannot
/// express; plans and plain memories are a minority of a corpus whose bulk is
/// knowledge files, so one capped scan (filtered in memory, newest first) is
/// cheaper than three round-trips per page.
pub const OTHER_SCAN_LIMIT: usize = 600;

/// The tab's filter chips: one typed-record class, the "other" group, or
/// everything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KindGroup {
    /// No type filter.
    All,
    /// Exactly one typed-record class.
    One(MemoryRecordType),
    /// Every record WITHOUT one of the four knowledge types (plan / review /
    /// plain memory).
    Other,
}

impl KindGroup {
    /// The record type the SQL filter can express, when there is exactly one.
    fn record_type(self) -> Option<MemoryRecordType> {
        match self {
            KindGroup::One(record_type) => Some(record_type),
            KindGroup::All | KindGroup::Other => None,
        }
    }

    /// Whether a row belongs to this group — only needed where the SQL filter
    /// cannot say it (the "other" group spans three types).
    fn matches(self, record_type: MemoryRecordType) -> bool {
        match self {
            KindGroup::All => true,
            KindGroup::One(wanted) => record_type == wanted,
            KindGroup::Other => matches!(
                record_type,
                MemoryRecordType::Plan | MemoryRecordType::Review | MemoryRecordType::None
            ),
        }
    }
}

/// One record as the Memory tab's list renders it: the pointer row (title,
/// type, tier, a short preview, the knowledge file's path) without the body.
#[derive(Debug, Clone, Serialize)]
pub struct MemoryRow {
    /// The memory id — what [`memory_read`] and [`memory_backlinks`] take.
    pub id: String,
    /// The stored title, typed prefix included (`SPEC: …`).
    pub title: String,
    /// The typed-record token: `spec` / `decision` / `bug` / `how` / `plan` /
    /// `review` / `none`.
    pub kind: String,
    /// The tier token (`working` / `episodic` / `semantic` / `procedural`).
    pub tier: String,
    /// The body's first [`PREVIEW_CHARS`] chars, whitespace-collapsed.
    pub preview: String,
    /// The knowledge file's repo path (`.coding/knowledge/<type>/<slug>.md`)
    /// when the row derives from one.
    pub path: Option<String>,
    /// Unix timestamp (seconds) the record was written.
    pub created_at: i64,
    /// Unix timestamp (seconds) of the last recall.
    pub last_accessed_at: i64,
    /// How often the record has been recalled.
    pub access_count: u32,
    /// The store's decayed strength (1.0 = fresh).
    pub strength: f64,
    /// True when a successor replaced this record — history, not live truth.
    pub superseded: bool,
    /// The startup digest sweep's quarantine reason, when it flagged the row.
    pub quarantined: Option<String>,
}

impl MemoryRow {
    /// Shape one store row for the list.
    fn from_memory(memory: &Memory) -> Self {
        Self {
            id: memory.id.clone(),
            title: memory.title.clone(),
            kind: memory.record_type.as_str().to_string(),
            tier: memory.tier.as_str().to_string(),
            preview: preview_of(&memory.content),
            path: knowledge_rel(memory).map(|rel| knowledge_path(&rel)),
            created_at: memory.created_at,
            last_accessed_at: memory.last_accessed_at,
            access_count: memory.access_count,
            strength: memory.strength,
            superseded: memory.superseded_by.is_some(),
            quarantined: memory.quarantined.clone(),
        }
    }

    /// Shape one `MemoryApi` wire row for the list — the recall test's path,
    /// where the API has already truncated the body and dropped the supersede
    /// stamp.
    fn from_wire(wire: &MemoryDebugWire) -> Self {
        let memory = Memory {
            id: wire.id.clone(),
            tier: MemoryTier::from_str_opt(&wire.tier).unwrap_or(MemoryTier::Semantic),
            title: wire.title.clone(),
            content: wire.content.clone(),
            strength: wire.strength,
            access_count: wire.access_count,
            created_at: wire.created_at,
            last_accessed_at: wire.last_accessed_at,
            source_session_ids: wire.source_session_ids.clone(),
            data: wire.data.clone(),
            record_class: MemoryClass::default(),
            record_type: MemoryRecordType::from_title(&wire.title),
            superseded_by: None,
            quarantined: None,
            embedding: Vec::new(),
        };
        Self::from_memory(&memory)
    }
}

/// One page of the Memory tab's list, newest first.
#[derive(Debug, Clone, Serialize)]
pub struct MemoryListPage {
    /// The page's rows, newest first.
    pub rows: Vec<MemoryRow>,
    /// The offset this page starts at (echoed for "load more").
    pub offset: usize,
    /// True when more rows match beyond this page.
    pub has_more: bool,
}

/// One search hit: the row plus the recall score it earned.
#[derive(Debug, Clone, Serialize)]
pub struct MemoryHit {
    /// The matched record, shaped exactly like a list row.
    #[serde(flatten)]
    pub row: MemoryRow,
    /// The recall score (higher is closer; not comparable across queries).
    pub score: f64,
}

/// One record as the reading pane renders it.
#[derive(Debug, Clone, Serialize)]
pub struct MemoryDetail {
    /// The memory id.
    pub id: String,
    /// The stored title, typed prefix included.
    pub title: String,
    /// The typed-record token.
    pub kind: String,
    /// The tier token.
    pub tier: String,
    /// The Markdown the pane renders: the knowledge file's body when the row
    /// derives from one (front matter stripped), else the memory's own digest.
    pub markdown: String,
    /// The knowledge file's repo path, when the row derives from one.
    pub path: Option<String>,
    /// The file's creation date (`YYYY-MM-DD`) from its front matter.
    pub created: Option<String>,
    /// The file's publication status; `None` for rows without a file.
    pub status: Option<KnowledgeStatus>,
    /// The successor record's id when this record is history.
    pub superseded_by: Option<String>,
    /// The startup digest sweep's quarantine reason, when it flagged the row.
    pub quarantined: Option<String>,
    /// Unix timestamp (seconds) the record was written.
    pub created_at: i64,
    /// Unix timestamp (seconds) of the last recall.
    pub last_accessed_at: i64,
    /// How often the record has been recalled.
    pub access_count: u32,
    /// The store's decayed strength.
    pub strength: f64,
    /// The wiki-link targets the file body carries (`[[target]]`, verbatim).
    pub links: Vec<String>,
    /// The records whose links point here.
    pub backlinks: Vec<MemoryRow>,
}

/// One stored vector fingerprint: the embedding model and its dimension.
#[derive(Debug, Clone, Serialize)]
pub struct ModelFingerprint {
    /// The model id the vectors were written with.
    pub model: String,
    /// That model's vector dimension.
    pub dim: usize,
}

/// The Diagnostics card's answer.
#[derive(Debug, Clone, Serialize)]
pub struct MemoryOverviewView {
    /// False when the project runs with memory off — the card then shows
    /// `reason` instead of counts.
    pub available: bool,
    /// Why memory is unavailable (empty while it is).
    pub reason: String,
    /// The live embedder's model id (`<none>` when memory is off).
    pub model_id: String,
    /// The live embedder's vector dimension (0 when memory is off).
    pub dim: usize,
    /// The CONFIGURED embedding model (Settings → Memory).
    pub configured_model: Option<String>,
    /// The fingerprints of the vectors actually stored.
    pub fingerprints: Vec<ModelFingerprint>,
    /// True when a stored fingerprint matches the live embedder — false means
    /// the next start re-embeds.
    pub fingerprint_matches: bool,
    /// Per-tier record counts.
    pub counts: MemoryCounts,
    /// The derived index's bookkeeping; `None` when memory is off.
    pub index: Option<MemoryIndexStatus>,
    /// The embedder's status object (kind, readiness, message), passed through
    /// from the engine so the card needs no second command.
    pub embedder: serde_json::Value,
}

impl MemoryOverviewView {
    /// The "memory is off" shape: no counts, no fingerprints, a reason.
    fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            available: false,
            reason: reason.into(),
            model_id: "<none>".to_string(),
            dim: 0,
            configured_model: None,
            fingerprints: Vec::new(),
            fingerprint_matches: false,
            counts: MemoryCounts::default(),
            index: None,
            embedder: serde_json::Value::Null,
        }
    }
}

/// The error that says the project engine is still starting: a project open is
/// in flight, so the runtime (and its memory store) is not installed yet.
///
/// This is a WAIT, not a failure: the tab keeps its spinner up and retries —
/// `mnemo2/src/lib/memoryView.ts` exports the same string as `ENGINE_STARTING`
/// and matches it, so the two must stay identical.
pub const ENGINE_STARTING: &str = "the project engine is still starting";

/// The open project's engine, or a clear "nothing is open" error.
async fn engine(state: &Mnemo2State) -> Result<Arc<Engine>, String> {
    let started = Instant::now();
    let lead = match state.open_lead().await {
        Some(lead) => lead,
        // A project open is in flight: the runtime lands with the engine, so
        // until then there is no store to read. Say so distinctly (not
        // "nothing is open") — the tab then waits instead of erroring.
        None if state.is_opening() => return Err(ENGINE_STARTING.to_string()),
        None => return Err("Mnemo2 has no open project; open one first".to_string()),
    };
    let engine = lead.engine_handle().await?;
    // The first memory command after an app start carries the project-open
    // wait (plan d49ea4a6): timed at debug level so a slow start can be told
    // from a slow query. The store itself answers the first page in
    // milliseconds (measured on this repository's 224 MB memory.db).
    log::debug!(
        "memory_view: the project engine answered in {} ms (opening={})",
        started.elapsed().as_millis(),
        state.is_opening()
    );
    Ok(engine)
}

/// The memory store behind an engine, or a clear "memory is off" error.
fn store_of(engine: &Engine) -> Result<Arc<dyn MemoryStoreTrait>, String> {
    engine.memory_store_handle().ok_or_else(|| {
        "this project runs with memory off, so it has no records to browse".to_string()
    })
}

/// The open project's memory store, or a clear "nothing is open" / "memory is
/// off" error.
async fn store(state: &Mnemo2State) -> Result<Arc<dyn MemoryStoreTrait>, String> {
    let engine = engine(state).await?;
    store_of(&engine)
}

/// Clamp a caller's page size into `[1, max]`, falling back to `default`.
fn clamped(limit: Option<usize>, default: usize, max: usize) -> usize {
    limit.unwrap_or(default).clamp(1, max)
}

/// Parse the `kind` filter: `all` / empty / absent for everything, `other` for
/// the records without a knowledge type, else a typed-record token.
fn parse_kind(kind: Option<&str>) -> Result<KindGroup, String> {
    let Some(kind) = kind.map(str::trim).filter(|k| !k.is_empty() && *k != "all") else {
        return Ok(KindGroup::All);
    };
    if kind == "other" {
        return Ok(KindGroup::Other);
    }
    MemoryRecordType::from_str_opt(kind)
        .map(KindGroup::One)
        .ok_or_else(|| {
            format!(
                "unknown record type `{kind}`; expected spec, decision, bug, how, plan, review, none, other or all"
            )
        })
}

/// Parse the `tier` filter: `all` / empty / absent for every tier the list
/// shows, else a tier token.
fn parse_tier(tier: Option<&str>) -> Result<Option<MemoryTier>, String> {
    let Some(tier) = tier.map(str::trim).filter(|t| !t.is_empty() && *t != "all") else {
        return Ok(None);
    };
    MemoryTier::from_str_opt(tier).map(Some).ok_or_else(|| {
        format!("unknown memory tier `{tier}`; expected working, episodic, semantic or procedural")
    })
}

/// The collapse + cut a list preview needs: runs of whitespace become one
/// space (a digest's line breaks would otherwise shred the row) and the tail
/// past [`PREVIEW_CHARS`] becomes an ellipsis.
fn preview_of(content: &str) -> String {
    let collapsed = content.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut preview: String = collapsed.chars().take(PREVIEW_CHARS).collect();
    if collapsed.chars().count() > PREVIEW_CHARS {
        preview.push('…');
    }
    preview
}

/// The knowledge file a derived row was built from (`<type>/<slug>.md`,
/// relative to the knowledge root), when the row carries one.
fn knowledge_rel(memory: &Memory) -> Option<String> {
    let data = memory.data.as_object()?;
    if data.get("kind").and_then(|kind| kind.as_str()) != Some("knowledge") {
        return None;
    }
    data.get("rel_path")
        .and_then(|rel| rel.as_str())
        .map(str::to_string)
}

/// The repo-relative path of a knowledge file (`rel` is relative to the
/// knowledge root).
fn knowledge_path(rel: &str) -> String {
    format!(".coding/{KNOWLEDGE_DIR_NAME}/{rel}")
}

/// One page of the list, newest first.
async fn list_page(
    store: &dyn MemoryStoreTrait,
    group: KindGroup,
    tier: Option<MemoryTier>,
    offset: usize,
    limit: usize,
) -> Result<MemoryListPage, String> {
    // One row beyond the page tells the tab whether to offer "load more".
    let wanted = offset + limit + 1;
    let scan = match group {
        KindGroup::Other => OTHER_SCAN_LIMIT.max(wanted),
        KindGroup::All | KindGroup::One(_) => wanted,
    };
    let mut filter = MemoryFilter::new().limit(scan);
    if let Some(tier) = tier {
        filter = filter.tier(tier);
    }
    if let Some(record_type) = group.record_type() {
        filter = filter.record_type(record_type);
    }
    let query_started = Instant::now();
    let scanned = store
        .list_filtered(&filter)
        .await
        .map_err(|error| format!("could not list the project's memory: {error}"))?;
    log::debug!(
        "memory_view: list_filtered scanned {} row(s) in {} ms (offset={offset}, limit={limit})",
        scanned.len(),
        query_started.elapsed().as_millis()
    );
    let scan_truncated = scanned.len() >= scan;
    let kept: Vec<Memory> = scanned
        .into_iter()
        .filter(|memory| group.matches(memory.record_type))
        .collect();
    let has_more = kept.len() > offset + limit || (scan_truncated && kept.len() == offset + limit);
    let rows = kept
        .iter()
        .skip(offset)
        .take(limit)
        .map(MemoryRow::from_memory)
        .collect();
    Ok(MemoryListPage {
        rows,
        offset,
        has_more,
    })
}

/// Ranked hits for a free-text query, best first.
async fn search_hits(
    store: &dyn MemoryStoreTrait,
    query: &str,
    limit: usize,
) -> Result<Vec<MemoryHit>, String> {
    // `MemoryFilter::new()` — the same knowledge-first default the list uses
    // (no working-tier captures, no superseded or quarantined history).
    let filter = MemoryFilter::new().limit(limit);
    let mut scored = store
        .recall_peek(query, &filter)
        .await
        .map_err(|error| format!("could not search the project's memory: {error}"))?;
    scored.sort_by(|a, b| b.score.total_cmp(&a.score));
    Ok(scored
        .into_iter()
        .map(|hit| MemoryHit {
            row: MemoryRow::from_memory(&hit.memory),
            score: hit.score,
        })
        .collect())
}

/// The Diagnostics recall test: the `MemoryApi`'s own ranking, working tier
/// included — exactly what Mnemo1's debug view answered, so the two agree.
async fn recall_hits(api: &MemoryApi, query: &str, limit: usize) -> Result<Vec<MemoryHit>, String> {
    let mut scored = api
        .search(query, Some(limit))
        .await
        .map_err(|error| format!("could not run the recall test: {error}"))?;
    scored.sort_by(|a, b| b.score.total_cmp(&a.score));
    Ok(scored
        .iter()
        .map(|hit| MemoryHit {
            row: MemoryRow::from_wire(&hit.memory),
            score: hit.score,
        })
        .collect())
}

/// The Diagnostics card, from a live API handle.
async fn overview_from(api: &MemoryApi) -> MemoryOverviewView {
    let started = Instant::now();
    let overview = match api.overview().await {
        Ok(overview) => overview,
        Err(error) => {
            return MemoryOverviewView::unavailable(format!(
                "could not read the memory store: {error}"
            ))
        }
    };
    let card = MemoryOverviewView {
        available: true,
        reason: String::new(),
        model_id: overview.model_id,
        dim: overview.dim,
        configured_model: overview.configured_model,
        fingerprints: overview
            .fingerprints
            .iter()
            .map(|(model, dim)| ModelFingerprint {
                model: model.clone(),
                dim: *dim,
            })
            .collect(),
        fingerprint_matches: overview.fingerprint_matches,
        counts: overview.counts,
        // A failing index-status read leaves the card's counts intact rather
        // than blanking the whole card.
        index: api.index_status().await.ok(),
        embedder: overview.status,
    };
    // The card's slowest read is the stored-fingerprint scan (a DISTINCT over
    // the whole table); timed so a regression shows up as a number.
    log::debug!(
        "memory_view: the overview answered in {} ms",
        started.elapsed().as_millis()
    );
    card
}

/// The Diagnostics card for an engine — or the "memory is off" shape.
async fn overview_of(engine: &Engine) -> MemoryOverviewView {
    match engine.memory() {
        Ok(api) => overview_from(&api).await,
        Err(error) => {
            MemoryOverviewView::unavailable(format!("this project runs with memory off ({error})"))
        }
    }
}

/// The rows whose links point at `id`'s knowledge file.
async fn backlinks_of(store: &dyn MemoryStoreTrait, id: &str) -> Result<Vec<MemoryRow>, String> {
    let memory = store
        .get_memory(id)
        .await
        .map_err(|error| format!("could not read memory `{id}`: {error}"))?;
    let Some(rel) = memory.as_ref().and_then(knowledge_rel) else {
        // A row without a knowledge file cannot be linked to: knowledge links
        // are file-shaped (`[[spec/<slug>]]`).
        return Ok(Vec::new());
    };
    let rows = backlinks_for(store, &rel)
        .await
        .map_err(|error| format!("could not read the backlinks of `{rel}`: {error}"))?;
    Ok(rows.iter().map(MemoryRow::from_memory).collect())
}

/// One record in full: the knowledge file's Markdown when the row derives from
/// a file, else the memory's own digest, plus its links and backlinks.
async fn read_record(
    store: &dyn MemoryStoreTrait,
    knowledge_dir: Option<&Path>,
    id: &str,
) -> Result<MemoryDetail, String> {
    let memory = store
        .get_memory(id)
        .await
        .map_err(|error| format!("could not read memory `{id}`: {error}"))?
        .ok_or_else(|| {
            format!(
                "no record with id `{id}` — it may have been superseded or the index rebuilt; refresh the list"
            )
        })?;
    let rel = knowledge_rel(&memory);
    let mut markdown = memory.content.clone();
    let mut created = None;
    let mut status = None;
    let mut links = Vec::new();
    if let (Some(rel), Some(dir)) = (rel.as_deref(), knowledge_dir) {
        // The file is the truth for a knowledge record; a file that moved or
        // is on another branch degrades to the row's digest instead of
        // failing the read.
        if let Ok(text) = std::fs::read_to_string(dir.join(rel)) {
            if let Some(record) = parse_file(rel, &text) {
                markdown = record.body;
                created = record.created;
                status = Some(record.status);
                links = record.links.into_iter().map(|link| link.target).collect();
            }
        }
    }
    let backlinks = backlinks_of(store, id).await?;
    Ok(MemoryDetail {
        id: memory.id.clone(),
        title: memory.title.clone(),
        kind: memory.record_type.as_str().to_string(),
        tier: memory.tier.as_str().to_string(),
        markdown,
        path: rel.as_deref().map(knowledge_path),
        created,
        status,
        superseded_by: memory.superseded_by.clone(),
        quarantined: memory.quarantined.clone(),
        created_at: memory.created_at,
        last_accessed_at: memory.last_accessed_at,
        access_count: memory.access_count,
        strength: memory.strength,
        links,
        backlinks,
    })
}

/// The Memory tab's first page: the project's records, newest first.
///
/// `kind` filters by typed-record class (`spec` / `decision` / `bug` / `how` /
/// `plan` / `review` / `none`, the groups `other` and `all`), `tier` by store
/// tier, and `offset` + `limit` page through the matches.
#[tauri::command]
pub async fn memory_list(
    state: State<'_, Mnemo2State>,
    kind: Option<String>,
    tier: Option<String>,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<MemoryListPage, String> {
    let store = store(&state).await?;
    let group = parse_kind(kind.as_deref())?;
    let tier = parse_tier(tier.as_deref())?;
    list_page(
        store.as_ref(),
        group,
        tier,
        offset.unwrap_or(0),
        clamped(limit, LIST_DEFAULT_LIMIT, LIST_MAX_LIMIT),
    )
    .await
}

/// Search the project's records by text, best match first.
#[tauri::command]
pub async fn memory_search(
    state: State<'_, Mnemo2State>,
    query: String,
    limit: Option<usize>,
) -> Result<Vec<MemoryHit>, String> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let store = store(&state).await?;
    search_hits(
        store.as_ref(),
        query,
        clamped(limit, SEARCH_DEFAULT_LIMIT, SEARCH_MAX_LIMIT),
    )
    .await
}

/// One record in full, with its knowledge file's Markdown and backlinks.
#[tauri::command]
pub async fn memory_read(
    state: State<'_, Mnemo2State>,
    id: String,
) -> Result<MemoryDetail, String> {
    let engine = engine(&state).await?;
    let store = store_of(&engine)?;
    let knowledge_dir = engine
        .knowledge_store_handle()
        .map(|knowledge| knowledge.dir().to_path_buf());
    read_record(store.as_ref(), knowledge_dir.as_deref(), &id).await
}

/// The records whose links point at one record.
#[tauri::command]
pub async fn memory_backlinks(
    state: State<'_, Mnemo2State>,
    id: String,
) -> Result<Vec<MemoryRow>, String> {
    let store = store(&state).await?;
    backlinks_of(store.as_ref(), &id).await
}

/// The Diagnostics card: embedder status + model, stored vector fingerprints,
/// per-tier counts and the derived index's bookkeeping.
#[tauri::command]
pub async fn memory_overview(state: State<'_, Mnemo2State>) -> Result<MemoryOverviewView, String> {
    let engine = engine(&state).await?;
    Ok(overview_of(&engine).await)
}

/// The Diagnostics recall test: a query ranked by the memory store's own
/// recall, best first.
#[tauri::command]
pub async fn memory_recall_test(
    state: State<'_, Mnemo2State>,
    query: String,
    limit: Option<usize>,
) -> Result<Vec<MemoryHit>, String> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let engine = engine(&state).await?;
    let api = engine
        .memory()
        .map_err(|error| format!("the memory store is unavailable: {error}"))?;
    recall_hits(
        &api,
        query,
        clamped(limit, RECALL_DEFAULT_LIMIT, RECALL_MAX_LIMIT),
    )
    .await
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, RwLock};

    use tempfile::TempDir;

    use mnemo::memory::{EmbedderStatus, HashEmbedder, MemoryStore};

    use super::*;

    /// One written row's spec: what the list/read/search tests plant.
    struct Seed<'a> {
        tier: MemoryTier,
        title: &'a str,
        content: &'a str,
        created_at: i64,
        data: serde_json::Value,
    }

    impl<'a> Seed<'a> {
        fn new(tier: MemoryTier, title: &'a str, content: &'a str, created_at: i64) -> Self {
            Self {
                tier,
                title,
                content,
                created_at,
                data: serde_json::Value::Null,
            }
        }

        fn data(mut self, data: serde_json::Value) -> Self {
            self.data = data;
            self
        }
    }

    /// A real SQLite store in a temp dir with `seeds` written into it.
    async fn seeded(seeds: &[Seed<'_>]) -> (TempDir, Arc<MemoryStore>) {
        let dir = TempDir::new().expect("temp dir");
        let store = MemoryStore::open(&dir.path().join("memory.db"), Arc::new(HashEmbedder::new()))
            .expect("open store");
        for seed in seeds {
            let mut memory = Memory::new(seed.tier, seed.title, seed.content, seed.created_at);
            memory.created_at = seed.created_at;
            memory.last_accessed_at = seed.created_at;
            memory.data = seed.data.clone();
            store.write(memory).await.expect("write seed");
        }
        (dir, Arc::new(store))
    }

    /// The corpus the list tests page through: four knowledge types, one plan
    /// digest, one plain memory, and one working-tier capture (excluded by
    /// default).
    async fn corpus() -> (TempDir, Arc<MemoryStore>) {
        seeded(&[
            Seed::new(
                MemoryTier::Semantic,
                "SPEC: typed records as files",
                "spec body about record files",
                300,
            ),
            Seed::new(
                MemoryTier::Semantic,
                "DECISION: the memory tab reads the store",
                "decision body about the store",
                200,
            ),
            Seed::new(
                MemoryTier::Procedural,
                "HOW: run the workspace tests",
                "how body about cargo test --workspace",
                100,
            ),
            Seed::new(
                MemoryTier::Semantic,
                "PLAN: memory tab",
                "plan body about the memory tab",
                400,
            ),
            Seed::new(
                MemoryTier::Episodic,
                "session summary",
                "plain body about the session",
                50,
            ),
            Seed::new(
                MemoryTier::Working,
                "tool snapshot",
                "raw tool output",
                500,
            ),
        ])
        .await
    }

    #[tokio::test]
    async fn list_pages_newest_first_and_skips_the_working_tier() {
        let (_dir, store) = corpus().await;
        let page = list_page(store.as_ref(), KindGroup::All, None, 0, 10)
            .await
            .expect("list");
        let titles: Vec<&str> = page.rows.iter().map(|row| row.title.as_str()).collect();
        assert_eq!(
            titles,
            vec![
                "PLAN: memory tab",
                "SPEC: typed records as files",
                "DECISION: the memory tab reads the store",
                "HOW: run the workspace tests",
                "session summary",
            ],
            "newest first, and the working-tier capture is not knowledge"
        );
        assert!(!page.has_more, "five rows fit one page");
        assert_eq!(page.rows[1].kind, "spec");
        assert_eq!(page.rows[3].tier, "procedural");
    }

    #[tokio::test]
    async fn list_filters_by_record_type_and_tier() {
        let (_dir, store) = corpus().await;
        let specs = list_page(store.as_ref(), KindGroup::One(MemoryRecordType::Spec), None, 0, 10)
            .await
            .expect("list specs");
        assert_eq!(specs.rows.len(), 1);
        assert_eq!(specs.rows[0].title, "SPEC: typed records as files");

        let other = list_page(store.as_ref(), KindGroup::Other, None, 0, 10)
            .await
            .expect("list other");
        let titles: Vec<&str> = other.rows.iter().map(|row| row.title.as_str()).collect();
        assert_eq!(
            titles,
            vec!["PLAN: memory tab", "session summary"],
            "the other chip spans plans, reviews and plain memories"
        );

        let procedural = list_page(store.as_ref(), KindGroup::All, Some(MemoryTier::Procedural), 0, 10)
            .await
            .expect("list procedural");
        assert_eq!(procedural.rows.len(), 1);
        assert_eq!(procedural.rows[0].kind, "how");

        let working = list_page(store.as_ref(), KindGroup::All, Some(MemoryTier::Working), 0, 10)
            .await
            .expect("list working");
        assert_eq!(
            working.rows.len(),
            1,
            "an explicit tier wins over the working-tier default exclusion"
        );
    }

    #[tokio::test]
    async fn list_pages_with_offsets_and_reports_more() {
        let (_dir, store) = corpus().await;
        let first = list_page(store.as_ref(), KindGroup::All, None, 0, 2)
            .await
            .expect("first page");
        assert_eq!(first.rows.len(), 2);
        assert_eq!(first.rows[0].title, "PLAN: memory tab");
        assert!(first.has_more, "three rows remain after the first page");

        let second = list_page(store.as_ref(), KindGroup::All, None, 2, 2)
            .await
            .expect("second page");
        assert_eq!(second.rows[0].title, "DECISION: the memory tab reads the store");
        assert!(second.has_more, "one row remains after the second page");

        let last = list_page(store.as_ref(), KindGroup::All, None, 4, 2)
            .await
            .expect("last page");
        assert_eq!(last.rows.len(), 1);
        assert_eq!(last.rows[0].title, "session summary");
        assert!(!last.has_more);

        let past = list_page(store.as_ref(), KindGroup::All, None, 40, 2)
            .await
            .expect("past the end");
        assert!(past.rows.is_empty());
        assert!(!past.has_more);
    }

    #[tokio::test]
    async fn list_rows_carry_a_collapsed_preview_and_the_knowledge_path() {
        let rel = "decision/2026-01-02-pick-a-store.md";
        let (_dir, store) = seeded(&[Seed::new(
            MemoryTier::Semantic,
            "DECISION: pick a store",
            "SQLite\n\nthe   store lives in\n  a single file",
            100,
        )
        .data(serde_json::json!({
            "kind": "knowledge",
            "rel_path": rel,
            "links": [],
        }))])
        .await;
        let page = list_page(store.as_ref(), KindGroup::All, None, 0, 10)
            .await
            .expect("list");
        let row = &page.rows[0];
        assert_eq!(row.preview, "SQLite the store lives in a single file");
        assert_eq!(row.kind, "decision");
        assert_eq!(
            row.path.as_deref(),
            Some(".coding/knowledge/decision/2026-01-02-pick-a-store.md")
        );
        assert!(!row.superseded);
    }

    #[tokio::test]
    async fn read_renders_the_knowledge_file_body_and_its_backlinks() {
        let dir = TempDir::new().expect("temp dir");
        let knowledge = dir.path().join(".coding").join(KNOWLEDGE_DIR_NAME);
        std::fs::create_dir_all(knowledge.join("decision")).expect("knowledge dir");
        std::fs::write(
            knowledge
                .join("decision")
                .join("2026-01-02-pick-a-store.md"),
            "+++
title = \"Pick a store\"
created = \"2026-01-02\"
status = \"live\"
+++

# Pick a store

See [[spec/2026-01-01-typed-records]].
",
        )
        .expect("write record");

        let decision = Seed::new(
            MemoryTier::Semantic,
            "DECISION: pick a store",
            "digest: pick a store",
            200,
        )
        .data(serde_json::json!({
            "kind": "knowledge",
            "rel_path": "decision/2026-01-02-pick-a-store.md",
            "links": [{"target": "spec/2026-01-01-typed-records", "kind": "knowledge"}],
        }));
        let spec = Seed::new(
            MemoryTier::Semantic,
            "SPEC: typed records",
            "digest: typed records",
            100,
        )
        .data(serde_json::json!({
            "kind": "knowledge",
            "rel_path": "spec/2026-01-01-typed-records.md",
            "links": [{"target": "decision/2026-01-02-pick-a-store", "kind": "knowledge"}],
        }));
        let (_store_dir, store) = seeded(&[decision, spec]).await;
        let id = store
            .list_filtered(&MemoryFilter::new().record_type(MemoryRecordType::Decision))
            .await
            .expect("find the decision")
            .first()
            .map(|memory| memory.id.clone())
            .expect("the decision row exists");

        let detail = read_record(store.as_ref(), Some(&knowledge), &id)
            .await
            .expect("read");
        assert_eq!(detail.kind, "decision");
        assert_eq!(
            detail.markdown, "# Pick a store

See [[spec/2026-01-01-typed-records]].",
            "the file body, front matter stripped — not the truncated digest"
        );
        assert_eq!(
            detail.path.as_deref(),
            Some(".coding/knowledge/decision/2026-01-02-pick-a-store.md")
        );
        assert_eq!(detail.created.as_deref(), Some("2026-01-02"));
        assert_eq!(detail.status, Some(KnowledgeStatus::Live));
        assert_eq!(detail.links, vec!["spec/2026-01-01-typed-records"]);
        let backlinks: Vec<&str> = detail
            .backlinks
            .iter()
            .map(|row| row.title.as_str())
            .collect();
        assert_eq!(backlinks, vec!["SPEC: typed records"]);
    }

    #[tokio::test]
    async fn read_falls_back_to_the_digest_when_the_file_is_gone() {
        let dir = TempDir::new().expect("temp dir");
        let (_store_dir, store) = seeded(&[Seed::new(
            MemoryTier::Semantic,
            "DECISION: pick a store",
            "digest: pick a store",
            200,
        )
        .data(serde_json::json!({
            "kind": "knowledge",
            "rel_path": "decision/2026-01-02-pick-a-store.md",
            "links": [],
        }))])
        .await;
        let id = store
            .list_filtered(&MemoryFilter::new())
            .await
            .expect("find the row")
            .first()
            .map(|memory| memory.id.clone())
            .expect("row exists");
        let detail = read_record(store.as_ref(), Some(dir.path()), &id)
            .await
            .expect("read");
        assert_eq!(detail.markdown, "digest: pick a store");
        assert_eq!(detail.status, None);
        assert!(detail.backlinks.is_empty());
    }

    #[tokio::test]
    async fn read_rejects_an_unknown_id() {
        let (_dir, store) = corpus().await;
        let error = read_record(store.as_ref(), None, "nope")
            .await
            .expect_err("an unknown id is an error");
        assert!(error.contains("no record with id `nope`"), "{error}");
    }

    #[tokio::test]
    async fn search_ranks_the_matching_record_first() {
        let (_dir, store) = corpus().await;
        let hits = search_hits(store.as_ref(), "cargo test --workspace", 10)
            .await
            .expect("search");
        assert!(!hits.is_empty(), "the query matches one record");
        assert_eq!(hits[0].row.title, "HOW: run the workspace tests");
        assert_eq!(hits[0].row.kind, "how");
        assert!(
            hits.windows(2).all(|pair| pair[0].score >= pair[1].score),
            "hits arrive best first"
        );
    }

    #[tokio::test]
    async fn overview_counts_the_tiers_of_a_live_store() {
        let (_dir, store) = corpus().await;
        let api = MemoryApi::from_parts(
            Some(store),
            Arc::new(RwLock::new(EmbedderStatus::Ready)),
            Some("hash".to_string()),
        );
        let overview = overview_from(&api).await;
        assert!(overview.available);
        assert_eq!(overview.counts.total, 6);
        assert_eq!(overview.counts.semantic, 3);
        assert_eq!(overview.counts.procedural, 1);
        assert_eq!(overview.counts.working, 1);
        assert_eq!(overview.configured_model.as_deref(), Some("hash"));
        assert!(overview.index.is_some(), "the index bookkeeping rides along");
        assert!(
            !overview.model_id.is_empty() && overview.model_id != "<none>",
            "the live embedder names itself"
        );
    }

    #[tokio::test]
    async fn overview_without_memory_is_a_reason_not_an_error() {
        let overview = MemoryOverviewView::unavailable("this project runs with memory off");
        assert!(!overview.available);
        assert_eq!(overview.reason, "this project runs with memory off");
        assert_eq!(overview.counts.total, 0);
        assert!(overview.index.is_none());
        assert!(overview.fingerprints.is_empty());
    }

    #[tokio::test]
    async fn recall_test_returns_ranked_hits_with_a_score() {
        let (_dir, store) = corpus().await;
        let api = MemoryApi::from_parts(
            Some(store),
            Arc::new(RwLock::new(EmbedderStatus::Ready)),
            None,
        );
        let hits = recall_hits(&api, "typed records as files", 5)
            .await
            .expect("recall");
        assert!(!hits.is_empty());
        assert_eq!(hits[0].row.title, "SPEC: typed records as files");
        assert!(hits[0].score > 0.0);
        assert_eq!(hits[0].row.kind, "spec");
    }

    #[test]
    fn kind_and_tier_filters_parse_and_reject_unknown_tokens() {
        assert_eq!(parse_kind(None).expect("all"), KindGroup::All);
        assert_eq!(parse_kind(Some(" ")).expect("all"), KindGroup::All);
        assert_eq!(parse_kind(Some("all")).expect("all"), KindGroup::All);
        assert_eq!(parse_kind(Some("other")).expect("other"), KindGroup::Other);
        assert_eq!(
            parse_kind(Some("spec")).expect("spec"),
            KindGroup::One(MemoryRecordType::Spec)
        );
        assert!(parse_kind(Some("Spec")).is_err(), "tokens are lowercase");
        assert!(parse_kind(Some("bogus")).is_err());

        assert_eq!(parse_tier(None).expect("all"), None);
        assert_eq!(
            parse_tier(Some("semantic")).expect("semantic"),
            Some(MemoryTier::Semantic)
        );
        assert!(parse_tier(Some("bogus")).is_err());
    }

    #[test]
    fn previews_collapse_whitespace_and_ellipsize_the_tail() {
        assert_eq!(preview_of("a\n\tb   c"), "a b c");
        let long = "x".repeat(PREVIEW_CHARS + 10);
        let preview = preview_of(&long);
        assert_eq!(preview.chars().count(), PREVIEW_CHARS + 1);
        assert!(preview.ends_with('…'));
        assert_eq!(preview_of(&"y".repeat(PREVIEW_CHARS)), "y".repeat(PREVIEW_CHARS));
    }

    /// Plan d49ea4a6: a memory command during a project open says the engine is
    /// still starting, so the tab waits instead of showing an empty list. The
    /// store cannot answer before the runtime lands with its engine.
    #[tokio::test]
    async fn a_project_open_in_flight_reports_the_engine_starting() {
        let state = Mnemo2State::default();
        assert_eq!(
            engine(&state).await.err(),
            Some("Mnemo2 has no open project; open one first".to_string()),
            "with nothing opening the message is the old one, exactly"
        );
        let opening = state.begin_opening();
        assert_eq!(engine(&state).await.err().as_deref(), Some(ENGINE_STARTING));
        drop(opening);
        assert_eq!(
            engine(&state).await.err(),
            Some("Mnemo2 has no open project; open one first".to_string()),
            "the flag clears with the open, so the wait can never stick"
        );
    }

    #[test]
    fn the_engine_starting_error_is_the_string_the_tab_matches() {
        // mnemo2/src/lib/memoryView.ts matches this exact text; changing one
        // side without the other turns a wait into an error message.
        assert_eq!(ENGINE_STARTING, "the project engine is still starting");
    }
}
