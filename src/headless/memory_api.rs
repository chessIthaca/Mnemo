// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The memory data API (§5.6 :415-433) — the binding-ready read surface
//! over the engine's memory store, ONE implementation shared by the
//! headless `Engine` and the app's IPC commands (3b.2, D2). The wire
//! types moved here VERBATIM from `src-tauri/src/ipc/memory_debug.rs`
//! (+ `MemoryIndexStatus` from `memory_maintenance.rs`) — their serde
//! shapes are pinned by the row-19 contract fixtures
//! (`stats_memory_codegraph_ipc_unchanged`), so the bytes cannot drift.
//! The engine reaches this via `Engine::memory()` (which errors
//! `memory_disabled` when `EngineOptions.memory` is `Disabled`); the
//! IPC commands construct it directly from their state handles.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::memory::embedder::EmbedderStatus;
use crate::memory::{
    MemoryAccessEntry, MemoryFilter, MemoryStore, MemoryStoreTrait, MemoryTier, ScoredMemory,
};

/// The maximum number of characters of a memory's `content` sent to the
/// frontend. The full content already lives in the memory DB; the debug list
/// only needs a snippet to identify each row, so capping prevents a huge
/// working-tier tool-output snapshot from bloating the IPC payload.
pub const MAX_CONTENT_CHARS: usize = 500;

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
/// fingerprints (so a model mismatch / cross-machine drift is visible), and
/// the per-tier counts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryDebugOverview {
    /// The embedder status (serialized; the tab renders it).
    pub status: serde_json::Value,
    /// The ACTIVE embedding model's id ("<none>" when no store is wired).
    pub model_id: String,
    /// The active embedding model's vector dimension.
    pub dim: usize,
    /// The CONFIGURED model (`bundled_embedding_model`), or `None`.
    pub configured_model: Option<String>,
    /// The stored (model, dim) vector fingerprints.
    pub fingerprints: Vec<(String, usize)>,
    /// Whether the active (model, dim) is among the stored fingerprints.
    pub fingerprint_matches: bool,
    /// Per-tier counts.
    pub counts: MemoryCounts,
}

/// The derived index's bookkeeping state (§5.6's `index_status()`): the
/// authored vs derived counts + the last build time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryIndexStatus {
    /// The number of authored memories.
    pub authored_count: usize,
    /// The number of derived-index memories (plans/reviews/backlog rows).
    pub derived_count: usize,
    /// Unix timestamp (seconds) the derived index was last built.
    pub last_built_at: Option<i64>,
}

/// Map a [`crate::memory::Memory`] to its wire form, truncating the
/// content to [`MAX_CONTENT_CHARS`] chars (char-boundary safe).
pub(crate) fn to_wire(m: crate::memory::Memory) -> MemoryDebugWire {
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
pub(crate) async fn memory_counts(
    store: &MemoryStore,
) -> Result<MemoryCounts, crate::error::Error> {
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
/// The memory data API (§5.6 :415-433) — ONE implementation of the
/// debug/read + maintenance surface, shared by `Engine::memory()` and
/// the app's IPC commands. A `None` store degrades exactly like the old
/// IPC paths did: `overview` → the `"<none>"` shape, `access_log` +
/// `backlinks` → empty, everything else → an error.
pub struct MemoryApi {
    /// The store handle (`None` when memory is `Disabled` or unwired).
    store: Option<Arc<MemoryStore>>,
    /// The shared embedder-status handle (the same Arc the app exposes
    /// to the UI).
    embedder_status: Arc<std::sync::RwLock<EmbedderStatus>>,
    /// The CONFIGURED embedding model (`bundled_embedding_model`, with
    /// the "hash" sentinel mapped to `None`).
    configured_model: Option<String>,
}

impl MemoryApi {
    /// Build from the raw handles — the engine's `Engine::memory()` and
    /// the IPC commands both use this (§5.6's "ONE implementation").
    pub fn from_parts(
        store: Option<Arc<MemoryStore>>,
        embedder_status: Arc<std::sync::RwLock<EmbedderStatus>>,
        configured_model: Option<String>,
    ) -> Self {
        Self {
            store,
            embedder_status,
            configured_model,
        }
    }

    /// §5.6's `overview()` — the debug tab's top card.
    pub async fn overview(&self) -> Result<MemoryDebugOverview, crate::error::Error> {
        let status = self
            .embedder_status
            .read()
            .expect("embedder status lock poisoned")
            .clone();
        let status_value = serde_json::to_value(&status).unwrap_or(serde_json::Value::Null);
        let Some(store) = self.store.as_ref() else {
            return Ok(MemoryDebugOverview {
                status: status_value,
                model_id: "<none>".to_string(),
                dim: 0,
                configured_model: self.configured_model.clone(),
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
            configured_model: self.configured_model.clone(),
            fingerprints,
            fingerprint_matches,
            counts,
        })
    }

    /// §5.6's `list(filter)` — the browsable tier list (content
    /// truncated). `tier = None` lists across ALL FOUR tiers, with `limit`
    /// applied per-tier so each tier is fairly represented.
    pub async fn list(
        &self,
        tier: Option<MemoryTier>,
        limit: Option<usize>,
    ) -> Result<Vec<MemoryDebugWire>, crate::error::Error> {
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| crate::error::Error::Memory("memory store unavailable".to_string()))?;
        let tiers: Vec<MemoryTier> = match tier {
            Some(t) => vec![t],
            None => vec![
                MemoryTier::Working,
                MemoryTier::Episodic,
                MemoryTier::Semantic,
                MemoryTier::Procedural,
            ],
        };
        let mut out: Vec<MemoryDebugWire> = Vec::new();
        for t in tiers {
            let mut memories = store.list_by_tier(t).await?;
            if let Some(n) = limit {
                memories.truncate(n);
            }
            out.extend(memories.into_iter().map(to_wire));
        }
        Ok(out)
    }

    /// §5.6's `search(query, limit)` — a recall test against the store.
    pub async fn search(
        &self,
        query: &str,
        limit: Option<usize>,
    ) -> Result<Vec<ScoredMemoryDebugWire>, crate::error::Error> {
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| crate::error::Error::Memory("memory store unavailable".to_string()))?;
        let mut filter = MemoryFilter::new().include_working();
        if let Some(limit) = limit {
            filter = filter.limit(limit);
        }
        let scored: Vec<ScoredMemory> = store.recall_peek(query, &filter).await?;
        Ok(scored
            .into_iter()
            .map(|s| ScoredMemoryDebugWire {
                memory: to_wire(s.memory),
                score: s.score,
            })
            .collect())
    }

    /// §5.6's `get(id)` — resolve a memory id or `mem://` link to its row
    /// (the debug tab's detail lookup).
    pub async fn resolve_link(
        &self,
        target: &str,
    ) -> Result<Option<crate::memory::indexer::ResolvedLink>, crate::error::Error> {
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| crate::error::Error::Memory("memory store unavailable".to_string()))?;
        crate::memory::indexer::resolve_link(store.as_ref() as &dyn MemoryStoreTrait, target)
            .await
    }

    /// §5.6's `backlinks(id)` — the rows whose content links to `rel`.
    /// Degrades to an EMPTY list when no memory store is wired (the
    /// Graph tab shows "referenced by nothing" instead of an error).
    pub async fn backlinks(
        &self,
        rel: &str,
    ) -> Result<Vec<MemoryDebugWire>, crate::error::Error> {
        let Some(store) = self.store.as_ref() else {
            return Ok(Vec::new());
        };
        let rows =
            crate::memory::indexer::backlinks_for(store.as_ref() as &dyn MemoryStoreTrait, rel)
                .await?;
        Ok(rows.into_iter().map(to_wire).collect())
    }

    /// §5.6's `access_log(limit)` — the recall ring buffer.
    pub async fn access_log(&self, limit: Option<usize>) -> Vec<MemoryAccessEntry> {
        let Some(store) = self.store.as_ref() else {
            return Vec::new();
        };
        let log = store.access_log();
        match limit {
            Some(limit) => log.into_iter().take(limit).collect(),
            None => log,
        }
    }

    /// §5.6's `index_status()` — the derived index's bookkeeping state.
    pub async fn index_status(&self) -> Result<MemoryIndexStatus, crate::error::Error> {
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| crate::error::Error::Memory("memory store unavailable".to_string()))?;
        let (authored_count, derived_count) = store.count_by_class().await?;
        let last_built_at = store.index_state_last_built().await?;
        Ok(MemoryIndexStatus {
            authored_count,
            derived_count,
            last_built_at,
        })
    }

    /// §5.6's `cleanup()` — consolidate aged working-tier memories.
    /// `skip_sessions` names LIVE sessions (their rows stay); the
    /// progress callback ticks `(phase, done, total)`.
    pub async fn cleanup(
        &self,
        skip_sessions: &[String],
        provider: Option<&dyn crate::provider::LlmClient>,
        corpus: &str,
        progress: &(dyn Fn(crate::memory::maintenance::MaintenancePhase, usize, usize) + Send + Sync),
    ) -> Result<crate::memory::maintenance::CleanupReport, crate::error::Error> {
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| crate::error::Error::Memory("memory store unavailable".to_string()))?;
        crate::memory::maintenance::cleanup(store, skip_sessions, provider, corpus, progress)
            .await
    }

    /// §5.6's `rebuild_search()` — re-embed every memory + rebuild the
    /// FTS index + VACUUM (the degraded-search recovery path).
    pub async fn rebuild_search(
        &self,
        progress: &(dyn Fn(crate::memory::maintenance::MaintenancePhase, usize, usize) + Send + Sync),
    ) -> Result<crate::memory::maintenance::RebuildReport, crate::error::Error> {
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| crate::error::Error::Memory("memory store unavailable".to_string()))?;
        crate::memory::maintenance::rebuild_search(store, progress).await
    }

    /// §5.6's `rebuild_index()` — wipe + rescan the derived index
    /// (budgeted digests of `.coding/plans`, `.coding/reviews`, and the
    /// pending backlog); authored memories are never touched.
    pub async fn rebuild_index(
        &self,
        plans_dir: &std::path::Path,
        backlog_path: &std::path::Path,
        progress: &(dyn Fn(usize, usize) + Send + Sync),
    ) -> Result<crate::memory::indexer::IndexReport, crate::error::Error> {
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| crate::error::Error::Memory("memory store unavailable".to_string()))?;
        crate::memory::indexer::rebuild_derived(
            store.as_ref() as &dyn MemoryStoreTrait,
            plans_dir,
            backlog_path,
            progress,
        )
        .await
    }
}