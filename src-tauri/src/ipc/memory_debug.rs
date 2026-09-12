// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Memory debug Tauri commands — the backend of the right-panel "Memory" tab.
//!
//! Surfaces the memory system's internals for debugging: the embedder status +
//! model, the stored vector fingerprints (so a model mismatch / cross-machine
//! drift is visible), per-tier counts, a recall test box, and a browsable
//! memory list with detail. Mirrors the Trace tab's architecture (a small IPC
//! module over the shared `MemoryStore` held in `IpcState`).
//!
//! Also serves the Graph tab's "Memory access" section: the
//! [`memory_access_log`] command returns the store's rolling read/write
//! activity log (see `MemoryStore::access_log`).
//!
//! All commands return `Err(IpcError::msg("memory store unavailable"))` when
//! the brain failed to build at startup (no concrete `MemoryStore` handle) so
//! the frontend can show a "not initialized" state instead of crashing —
//! except [`memory_access_log`], which degrades to an empty log (an empty
//! section beats an error banner for auxiliary UI).

use serde::{Deserialize, Serialize};
use tauri::State;

use mnemo::memory::{MemoryAccessEntry, MemoryFilter, MemoryStoreTrait, MemoryTier, ScoredMemory};

use crate::ipc::error::IpcError;
use crate::ipc::state::IpcState;

/// The maximum number of characters of a memory's `content` sent to the
/// frontend. The full content already lives in the memory DB; the debug list
/// only needs a snippet to identify each row, so capping prevents a huge
/// working-tier tool-output snapshot from bloating the IPC payload.
const MAX_CONTENT_CHARS: usize = 500;

/// A memory row as sent to the Memory debug tab (content truncated).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryDebugWire {
    /// The memory id (a UUID string).
    pub id: String,
    /// The tier name (`"working"` / `"episodic"` / `"semantic"` / `"procedural"`).
    pub tier: String,
    /// The memory title (a short label).
    pub title: String,
    /// The memory content, truncated to [`MAX_CONTENT_CHARS`] chars.
    pub content: String,
    /// The stored strength (0.0–1.0; decays by time-since-last-access at
    /// recall time, but the stored value is frozen at write time).
    pub strength: f64,
    /// How many times this memory has been returned by recall.
    pub access_count: u32,
    /// Unix timestamp (seconds) the memory was created.
    pub created_at: i64,
    /// Unix timestamp (seconds) the memory was last accessed by recall.
    pub last_accessed_at: i64,
    /// The session ids that contributed this memory (provenance).
    pub source_session_ids: Vec<String>,
    /// The tier-specific payload as JSON (the `data` column).
    pub data: serde_json::Value,
}

/// A scored memory result from a recall test (the memory + its relevance score).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoredMemoryDebugWire {
    /// The memory (content truncated).
    #[serde(flatten)]
    pub memory: MemoryDebugWire,
    /// The relevance score (higher = more relevant). Computed from semantic
    /// similarity + keyword overlap + decayed strength + tier weight.
    pub score: f64,
}

/// Per-tier memory counts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryCounts {
    /// Working-tier (raw tool-event) count.
    pub working: usize,
    /// Episodic (session-summary) count.
    pub episodic: usize,
    /// Semantic (distilled-fact) count.
    pub semantic: usize,
    /// Procedural (learned-workflow) count.
    pub procedural: usize,
    /// Total across all tiers.
    pub total: usize,
}

/// The overview shown at the top of the Memory debug tab: the embedder status,
/// the active model + dimension, the configured model, the stored vector
/// fingerprints (for mismatch detection), and per-tier counts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryDebugOverview {
    /// The embedder status wire form (`"ready"` / `"checking"` / `"fallback"`
    /// / `"pulling"` / `"failed"`, or `{"downloading": {model, progress}}`).
    pub status: serde_json::Value,
    /// The active embedder's model id (e.g. `"all-MiniLM-L6-v2"`, or `"hash"`
    /// for the offline fallback). `"<none>"` when no memory store is wired.
    pub model_id: String,
    /// The active embedder's vector dimension.
    pub dim: usize,
    /// The configured bundled model id from config (`null` = none / hash mode).
    pub configured_model: Option<String>,
    /// The set of `(model_id, dim)` fingerprints present across stored memories.
    /// Empty when no memories have embeddings yet.
    pub fingerprints: Vec<(String, usize)>,
    /// Whether the active embedder's `(model_id, dim)` is present in the stored
    /// fingerprints. `false` means the stored vectors are in a different space
    /// and recall is degraded (a re-embed is pending or needed).
    pub fingerprint_matches: bool,
    /// Per-tier memory counts.
    pub counts: MemoryCounts,
}

/// The overview shown at the top of the Memory debug tab.
///
/// Reads the embedder status, the active model + dimension, the stored vector
/// fingerprints (for mismatch detection), and per-tier counts. When no memory
/// store is wired (the brain failed to build at startup), returns zeroed counts
/// + an empty fingerprint list + `model_id = "<none>"` so the frontend can show
/// a "not initialized" state.
#[tauri::command]
pub async fn memory_debug_overview(
    state: State<'_, IpcState>,
) -> Result<MemoryDebugOverview, IpcError> {
    // The embedder status is always available (a fresh Arc is created on the
    // fallback path), so read it unconditionally.
    let status = state
        .runtime
        .embedder_status
        .read()
        .expect("embedder status lock poisoned")
        .clone();
    let status_value = serde_json::to_value(&status).unwrap_or(serde_json::Value::Null);

    let configured_model = {
        let config = state.project.config.lock().await;
        config
            .general
            .general
            .bundled_embedding_model
            .clone()
            // The "hash" sentinel (keyword-only opt-out) is not a real model
            // id — map it to None so the debug overview shows "<none>" instead
            // of leaking the sentinel string. Case-insensitive, matching the
            // sentinel contract everywhere else.
            .filter(|m| !m.eq_ignore_ascii_case(mnemo::config::EMBEDDING_MODEL_SENTINEL_HASH))
    };

    let Some(store) = state.runtime.memory_store.as_ref() else {
        return Ok(MemoryDebugOverview {
            status: status_value,
            model_id: "<none>".to_string(),
            dim: 0,
            configured_model,
            fingerprints: Vec::new(),
            fingerprint_matches: false,
            counts: MemoryCounts::default(),
        });
    };

    let embedder = store.embedder_handle();
    let model_id = embedder.model_id().to_string();
    let dim = embedder.dim();

    let fingerprints = store.stored_model_fingerprints().await?;
    let fingerprint_matches = fingerprints
        .iter()
        .any(|(m, d)| m == &model_id && *d == dim);

    let counts = memory_counts(store).await?;

    Ok(MemoryDebugOverview {
        status: status_value,
        model_id,
        dim,
        configured_model,
        fingerprints,
        fingerprint_matches,
        counts,
    })
}

/// List memories, optionally filtered by tier, with content truncated to
/// [`MAX_CONTENT_CHARS`] chars. `tier = None` lists across all four tiers.
/// `limit` caps the result count (applied per-tier when `tier` is `None`, so
/// each tier is fairly represented). Returns `Err` when no memory store is
/// wired.
#[tauri::command]
pub async fn memory_debug_list(
    state: State<'_, IpcState>,
    tier: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<MemoryDebugWire>, IpcError> {
    let store = state
        .runtime
        .memory_store
        .as_ref()
        .ok_or_else(|| IpcError::msg("memory store unavailable"))?;

    let mut out: Vec<MemoryDebugWire> = Vec::new();
    let tiers: Vec<MemoryTier> =
        match tier.as_deref() {
            Some(s) => vec![MemoryTier::from_str(s)
                .ok_or_else(|| IpcError::msg(format!("unknown tier '{s}'")))?],
            None => vec![
                MemoryTier::Working,
                MemoryTier::Episodic,
                MemoryTier::Semantic,
                MemoryTier::Procedural,
            ],
        };
    for t in tiers {
        let mut memories = store.list_by_tier(t).await?;
        if let Some(n) = limit {
            memories.truncate(n);
        }
        for m in memories {
            out.push(to_wire(m));
        }
    }
    Ok(out)
}

/// Run a recall test against the memory store and return the scored results
/// (memory + relevance score). Includes working-tier memories (unlike the
/// production auto-recall, which excludes them by default) so the debug view
/// can surface raw tool-event snapshots too. Returns `Err` when no memory
/// store is wired.
///
/// **Read-only:** goes through `MemoryStore::recall_peek` — the same
/// non-bumping path production auto-recall takes — so a debug recall test
/// never freshens the memories it returns (no `access_count` /
/// `last_accessed_at` update, no decayed-strength boost) and previewing
/// recall ranking cannot change future production rankings.
#[tauri::command]
pub async fn memory_debug_recall(
    state: State<'_, IpcState>,
    query: String,
    limit: Option<usize>,
) -> Result<Vec<ScoredMemoryDebugWire>, IpcError> {
    let store = state
        .runtime
        .memory_store
        .as_ref()
        .ok_or_else(|| IpcError::msg("memory store unavailable"))?;

    let mut filter = MemoryFilter::new().include_working();
    if let Some(n) = limit {
        filter = filter.limit(n);
    }
    // Peek, not bump: a human previewing recall ranking must not freshen the
    // memories it returns and thereby change future production rankings
    // (2026-05-21 review note; same self-reinforcement class as 2026-08-20).
    let results: Vec<ScoredMemory> = store.recall_peek(&query, &filter).await?;
    Ok(results
        .into_iter()
        .map(|sm| ScoredMemoryDebugWire {
            memory: to_wire(sm.memory),
            score: sm.score,
        })
        .collect())
}

/// The Graph tab's "Memory access" log: the rolling last-100 memory reads +
/// writes (newest first) from the store's in-memory ring. Degrades to an
/// empty log when no memory store is wired — the Graph tab's section shows
/// "no accesses yet" instead of an error banner.
///
/// Deliberately NOT `spawn_blocking` (unlike the SQLite-backed commands
/// above): the log is an in-memory `VecDeque` snapshot behind a short std
/// `Mutex` — no SQLite connection is touched, so there is no sync lock that
/// could park a tokio worker (the freeze-diagnosis F1 lesson applies to the
/// SQLite locks, not this one).
#[tauri::command]
pub async fn memory_access_log(
    state: State<'_, IpcState>,
) -> Result<Vec<MemoryAccessEntry>, IpcError> {
    let Some(store) = state.runtime.memory_store.as_ref() else {
        return Ok(Vec::new());
    };
    Ok(store.access_log())
}

/// Resolve one `[[wiki-link]]` target to its current record — the
/// knowledge-links surface of the Memory debug tab. Deterministic (id math
/// + one point lookup); a missing target resolves to a path-only entry.
#[tauri::command]
pub async fn memory_debug_resolve_link(
    state: State<'_, IpcState>,
    target: String,
) -> Result<Option<mnemo::memory::indexer::ResolvedLink>, IpcError> {
    let store = state
        .runtime
        .memory_store
        .as_ref()
        .ok_or_else(|| IpcError::msg("memory store unavailable"))?;
    Ok(mnemo::memory::indexer::resolve_link(store.as_ref(), &target).await?)
}

/// Every live memory whose `data.links` points at `rel` (the "referenced
/// by" reverse surface for the knowledge UI). Degrades to an empty list when
/// no memory store is wired.
#[tauri::command]
pub async fn memory_debug_backlinks(
    state: State<'_, IpcState>,
    rel: String,
) -> Result<Vec<MemoryDebugWire>, IpcError> {
    let Some(store) = state.runtime.memory_store.as_ref() else {
        return Ok(Vec::new());
    };
    let rows = mnemo::memory::indexer::backlinks_for(store.as_ref(), &rel).await?;
    Ok(rows.into_iter().map(to_wire).collect())
}

/// Map a [`mnemo::memory::Memory`] to its wire form, truncating the
/// content to [`MAX_CONTENT_CHARS`] chars (char-boundary safe).
fn to_wire(m: mnemo::memory::Memory) -> MemoryDebugWire {
    let content: String = m.content.chars().take(MAX_CONTENT_CHARS).collect();
    MemoryDebugWire {
        id: m.id,
        tier: m.tier.as_str().to_string(),
        title: m.title,
        content,
        strength: m.strength,
        access_count: m.access_count,
        created_at: m.created_at,
        last_accessed_at: m.last_accessed_at,
        source_session_ids: m.source_session_ids,
        data: m.data,
    }
}

/// Count memories per tier via `count_by_tier` (a `SELECT COUNT(*)` — no row
/// decoding, no embedding BLOB read). Propagates DB errors rather than
/// silently zeroing, so the overview surfaces a real error instead of a
/// misleading zero count.
async fn memory_counts(store: &mnemo::memory::MemoryStore) -> Result<MemoryCounts, IpcError> {
    let working = store.count_by_tier(MemoryTier::Working).await?;
    let episodic = store.count_by_tier(MemoryTier::Episodic).await?;
    let semantic = store.count_by_tier(MemoryTier::Semantic).await?;
    let procedural = store.count_by_tier(MemoryTier::Procedural).await?;
    Ok(MemoryCounts {
        working,
        episodic,
        semantic,
        procedural,
        total: working + episodic + semantic + procedural,
    })
}
