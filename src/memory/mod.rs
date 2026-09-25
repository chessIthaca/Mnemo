// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The project memory store — four-tier model with strength-ranked retrieval.
//!
//! Architecture: SQLite + FTS5 + in-Rust vector search. Embeddings are stored
//! as BLOBs and KNN is computed in Rust (cosine similarity). This avoids the
//! pre-v1 `sqlite-vec` FFI complexity while staying within SQLite's sweet spot
//! (hundreds to low-thousands of entries per project).

pub mod auto_typing;
pub mod classifier;
pub mod consolidation;
pub mod embedder;
pub mod finish_capture;
pub mod indexer;
pub mod knowledge;
pub mod maintenance;
pub mod schema;
pub mod strength;
pub mod types;

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};

use async_trait::async_trait;
use rusqlite::{params, Connection, OptionalExtension};

use crate::error::{Error, Result};
pub use classifier::{Answer, Classifier, ClassifierStatus, LayaClassifier, NoClassifier, Question};
pub use embedder::{Embedder, EmbedderStatus, HashEmbedder};
pub use knowledge::KnowledgeStore;
pub use types::{
    DayBreakdown, EpisodicData, Memory, MemoryAccessEntry, MemoryClass, MemoryData, MemoryFilter,
    MemoryRecordType, MemorySearchConfig, MemoryTier, ModelBreakdown, ProceduralData, ProjectStats,
    RequestStats, SemanticData, Session, SessionStats, SessionSummary, WorkingData,
};

/// A scored memory result.
#[derive(Debug, Clone)]
pub struct ScoredMemory {
    pub memory: Memory,
    pub score: f64,
}

/// If the stored model fingerprints don't exactly match the embedder's
/// (model_id, dim), re-embed all rows. No-op (returns without re-embedding)
/// when the fingerprint set already matches.
///
/// Extracted from the duplicated startup (main.rs) + post-config-save
/// (rewire.rs) re-embed paths. The caller is responsible for the Ready-status
/// + hash-opt-out + pending-download guards (they differ between the two call
/// sites) and for spawning this on a background task (it awaits DB I/O).
pub async fn reembed_if_needed(store: &Arc<MemoryStore>, embedder: &Arc<dyn Embedder>) {
    let expected_model = embedder.model_id().to_string();
    let expected_dim = embedder.dim();
    let fps = match store.stored_model_fingerprints().await {
        Ok(f) => f,
        Err(e) => {
            eprintln!("warning: stored_model_fingerprints failed: {e}");
            return;
        }
    };
    // Re-embed unless the stored set is EXACTLY the expected fingerprint. This
    // catches both the cross-machine case (all rows from a different model →
    // no matching fingerprint) and the interrupted-reembed case (mixed
    // fingerprints from a crash mid-reembed → more than one distinct
    // fingerprint).
    let needs_reembed = fps.len() != 1 || fps[0].0 != expected_model || fps[0].1 != expected_dim;
    if needs_reembed {
        eprintln!(
            "info: re-embedding memories for model '{expected_model}' (dim {expected_dim}); \
             stored fingerprints: {fps:?}"
        );
        // Do NOT overwrite the status here — the caller already set it to Ready
        // (the model loaded). Re-embedding is a background maintenance task,
        // not a status transition.
        match store.reembed_all(embedder.clone(), None).await {
            Ok(n) => eprintln!("info: re-embedded {n} memories"),
            Err(e) => eprintln!("warning: reembed_all failed: {e}"),
        }
    }
}

/// Maximum number of characters of a tool's output stored in a working-tier
/// memory event. The full output already lives in the conversation history;
/// recall only needs a snippet to match on, so capping prevents huge
/// search/git results from bloating the FTS index + memory.db. Char-boundary
/// safe (the cap is applied via `chars().take(N)`).
const MAX_WORKING_CONTENT_CHARS: usize = 500;

/// Maximum number of entries kept in the in-memory memory-access ring (the
/// Graph tab's "Memory access" section). A rolling window: when full, the
/// oldest entry is evicted on push.
const MAX_ACCESS_LOG_ENTRIES: usize = 100;

/// Maximum number of characters of a `detail` string stored in one
/// memory-access log entry (a recall query or a memory title) — one line,
/// bounded. Char-boundary safe (applied via `chars().take(N)`, mirroring
/// [`MAX_WORKING_CONTENT_CHARS`]).
const MAX_ACCESS_DETAIL_CHARS: usize = 120;

/// Recursively cap every string leaf in a JSON value to `max_chars`
/// (char-boundary safe). Used by `record_tool_event` so a large `file_write`
/// `content` argument doesn't bloat the `data` JSON blob stored in memory.db.
/// Non-string values (numbers, bools, null) pass through unchanged; arrays and
/// objects are recursed into.
fn cap_json_strings(value: serde_json::Value, max_chars: usize) -> serde_json::Value {
    match value {
        serde_json::Value::String(s) => {
            if s.chars().count() <= max_chars {
                serde_json::Value::String(s)
            } else {
                let capped: String = s.chars().take(max_chars).collect();
                serde_json::Value::String(capped)
            }
        }
        serde_json::Value::Array(arr) => serde_json::Value::Array(
            arr.into_iter()
                .map(|v| cap_json_strings(v, max_chars))
                .collect(),
        ),
        serde_json::Value::Object(obj) => {
            let map = obj
                .into_iter()
                .map(|(k, v)| (k, cap_json_strings(v, max_chars)))
                .collect();
            serde_json::Value::Object(map)
        }
        other => other,
    }
}

/// The trait for the memory store (so we can mock it in agent-loop tests).
#[async_trait]
pub trait MemoryStoreTrait: Send + Sync {
    /// Write a memory (embeds the content, inserts into all tables).
    async fn write(&self, memory: Memory) -> Result<String>;

    /// Recall memories matching a query, ranked by strength + relevance.
    /// Bumps the access count + last_accessed_at of every returned memory —
    /// use for deliberate, agent-initiated lookups (the `memory_recall`
    /// tool). Passive surfacing must use [`MemoryStoreTrait::recall_peek`].
    async fn recall(&self, query: &str, filter: &MemoryFilter) -> Result<Vec<ScoredMemory>>;

    /// Recall WITHOUT the access bump — for passive surfacing (the per-turn
    /// auto-recall prompt injection) and debug previews. Being shown is not
    /// evidence of usefulness: if passive surfacing bumped access, a memory
    /// that once won would keep refreshing its own recency and never decay
    /// (the self-reinforcement loop diagnosed 2026-08-20 — months-old
    /// memories permanently topping auto-recall). Ranking is identical to
    /// [`MemoryStoreTrait::recall`]; only the side effect differs.
    async fn recall_peek(&self, query: &str, filter: &MemoryFilter) -> Result<Vec<ScoredMemory>>;

    /// Bump the access count + last_accessed_at for a memory (raises strength).
    async fn access(&self, id: &str) -> Result<()>;

    /// Bump the access count + last_accessed_at for many memories in a single
    /// query. Used by `recall()` to update all returned memories at once
    /// instead of K sequential lock+query cycles.
    async fn batch_access(&self, ids: &[String]) -> Result<()> {
        // Default implementation: fall back to per-id access() so trait
        // objects (e.g. mocks) don't have to implement it.
        for id in ids {
            self.access(id).await?;
        }
        Ok(())
    }

    /// List memories by tier.
    async fn list_by_tier(&self, tier: MemoryTier) -> Result<Vec<Memory>>;

    /// Count memories in a single tier via `SELECT COUNT(*)` — cheaper than
    /// `list_by_tier(tier).await.map(|v| v.len())` (no row decoding, no
    /// embedding BLOB read). Used by the Memory debug tab's overview, which
    /// polls every 2s. The default impl falls back to `list_by_tier` so trait
    /// objects (mocks) don't have to implement it.
    async fn count_by_tier(&self, tier: MemoryTier) -> Result<usize> {
        Ok(self.list_by_tier(tier).await?.len())
    }

    /// The live retrieval/indexing scale knobs (Phase 2 `[memory]` config
    /// section) — read by recall (decay/caps) and the derived-index/
    /// finish-capture auto-truncation. The default returns the compiled-in
    /// defaults so trait objects (mocks) need no implementation.
    fn memory_search_config(&self) -> MemorySearchConfig {
        MemorySearchConfig::default()
    }

    /// The store's mutation counter: bumped on every recall-result-changing
    /// write (write / update_memory / supersede_memory / delete_memory).
    /// The per-turn auto-recall cache (agent loop) records the version it
    /// recalled at and treats a mismatch as a cache miss, so a same-turn
    /// `memory_write` is visible to the next recall. The default 0 keeps
    /// test mocks inert (the version never changes, so the cache behaves
    /// exactly as before — invalidated only on summarize).
    fn version(&self) -> u64 {
        0
    }

    /// Update a memory's title and/or content in place; `None` fields are
    /// left unchanged. A content change re-embeds the row — the embedding is
    /// derived from content, so leaving it stale would silently break
    /// semantic recall. A title change re-derives `record_type` from the new
    /// title's typed prefix (the prefix wins, same rule as
    /// [`MemoryStoreTrait::write`]). FTS stays in sync via the UPDATE
    /// trigger's title/content WHEN guard. Returns `false` when no memory
    /// with `id` exists.
    async fn update_memory(
        &self,
        id: &str,
        title: Option<&str>,
        content: Option<&str>,
    ) -> Result<bool>;

    /// Supersede `old_id` with `new_memory`, atomically: insert the new
    /// memory and set the old row's `superseded_by` to the new id in ONE
    /// SQLite transaction. Supersede-not-delete: the old memory stays
    /// retrievable via `include_superseded` (history) but is excluded from
    /// recall and listing by default. Errors when `old_id` does not exist or
    /// is already superseded — the error names the existing superseder so
    /// the caller can point at it. Returns the new memory's id.
    async fn supersede_memory(&self, old_id: &str, new_memory: Memory) -> Result<String>;

    /// Hard-delete a memory (the FTS delete trigger keeps the index in
    /// sync). For junk/duplicate rows only — stale-but-once-true knowledge
    /// is history and must be superseded, not deleted. Returns `false` when
    /// no memory with `id` exists.
    async fn delete_memory(&self, id: &str) -> Result<bool>;

    // ── Derived-index plumbing ────────────────────────────────────────────
    //
    // The indexer's store surface. Originally inherent methods (every caller
    // held the concrete `MemoryStore`); they moved onto the trait when the
    // knowledge file-writer (`KnowledgeStore`) — which holds the store as a
    // trait object, like the memory tools do — needed to drive indexing.
    // `MemoryStore` is the only implementor, so the trait surface stays
    // honest.

    /// Fetch a single memory by id — `None` when no such row exists. A
    /// point lookup for the indexer, the knowledge writer, and tests;
    /// recall/list remain the query paths.
    async fn get_memory(&self, id: &str) -> Result<Option<Memory>>;

    /// Hard-delete every derived (indexer-built) memory — the rebuild's
    /// wipe step. Authored rows are sacred: the WHERE clause is class-scoped
    /// so they are NEVER touched. Returns the number deleted.
    async fn delete_derived(&self) -> Result<usize>;

    /// Look up the incremental-indexing state for one source key — the
    /// content hash + memory id produced by the last index run. `None`
    /// means the source was never indexed (or a rebuild wiped the table).
    async fn index_state_get(&self, source_key: &str) -> Result<Option<(String, String)>>;

    /// Record (or refresh) the indexing state for one source key, stamped
    /// with the store's clock. Called after the source's derived memory was
    /// (re)written.
    async fn index_state_upsert(
        &self,
        source_key: &str,
        content_hash: &str,
        memory_id: &str,
    ) -> Result<()>;

    /// Drop one source key's indexing state — paired with `delete_memory`
    /// when its source file/item vanished (removal detection).
    async fn index_state_remove(&self, source_key: &str) -> Result<()>;

    /// Drop ALL indexing state — the rebuild's wipe step, paired with
    /// [`MemoryStoreTrait::delete_derived`].
    async fn index_state_clear(&self) -> Result<()>;

    /// Every indexed source key — the indexer diffs these against the
    /// sources seen on the current run to detect removals.
    async fn index_state_keys(&self) -> Result<Vec<String>>;

    /// Set a DERIVED row's `superseded_by` + `data` JSON in one UPDATE —
    /// the knowledge-indexer's reconciliation write. Class-scoped
    /// (`WHERE record_class = 'derived'`) so it can never touch authored
    /// knowledge; returns `false` when the id is unknown or not derived.
    /// Idempotent — rewriting the already-stored values is a row-wise no-op.
    ///
    /// The knowledge supersede model resolves successor FILES at scan time,
    /// so an UNCHANGED predecessor (skipped by the indexer's content-hash
    /// check) still needs its `superseded_by` flipped when a successor file
    /// appears — that reconciliation flows through here.
    async fn set_derived_metadata(
        &self,
        id: &str,
        superseded_by: Option<&str>,
        data: &serde_json::Value,
    ) -> Result<bool>;

    /// List memories matching a filter — optional tier + record_type + title
    /// prefix, with superseded rows excluded unless the filter opts in via
    /// `include_superseded`. Newest first (`created_at DESC`). The
    /// record_type / created_at predicates hit the Phase-1 indexes, never a
    /// full scan. Like recall, an explicit tier wins over the default
    /// working-tier exclusion. `filter.limit` bounds the result — callers
    /// listing a large store should always set one.
    async fn list_filtered(&self, filter: &MemoryFilter) -> Result<Vec<Memory>>;

    /// The top-`limit` strongest memories across the given tiers, ranked by
    /// decayed strength (the same Ebbinghaus decay used by `recall`'s scoring,
    /// so recently-accessed distilled facts surface first). Used to build a
    /// standing "project memory" primer injected once at session start — the
    /// agent begins with project knowledge instead of re-discovering it.
    ///
    /// Unlike `recall`, this is NOT query-driven: it returns the globally
    /// strongest distilled facts regardless of the user's prompt. Working-tier
    /// memories are never included (they are consolidation input, not primer
    /// material); callers should pass only distilled tiers.
    async fn strongest(&self, tiers: &[MemoryTier], limit: usize) -> Result<Vec<Memory>> {
        // Default: union across per-tier list_by_tier, then rank. The SQLite
        // impl overrides this with a single query; this default keeps trait
        // objects (mocks) compiling without a bespoke implementation.
        // Working-tier is filtered out to match the override + the doc
        // contract (working is consolidation input, not primer material).
        let mut all = Vec::new();
        for tier in tiers.iter().copied().filter(|t| *t != MemoryTier::Working) {
            all.extend(self.list_by_tier(tier).await?);
        }
        // Without a clock, mocks can't decay — rank by stored strength desc
        // with a title tiebreaker (deterministic across restarts).
        all.sort_by(|a, b| {
            b.strength
                .partial_cmp(&a.strength)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.title.cmp(&b.title))
        });
        all.truncate(limit);
        Ok(all)
    }

    /// Delete all working-tier memories belonging to a session.
    ///
    /// Called after consolidation so the raw tool events (which have been
    /// compressed into an episodic summary) don't accumulate unboundedly and
    /// slow every recall. Episodic/semantic/procedural memories are kept —
    /// they are the compressed output of consolidation.
    async fn delete_working_for_session(&self, session_id: &str) -> Result<usize>;

    /// The set of (model_id, dim) fingerprints present across stored memories.
    ///
    /// Used at startup to detect when the configured embedding model differs
    /// from what's stored — e.g. the memory DB traveled via git to a machine
    /// running a different model. A mismatch (or NULL rows from a legacy DB)
    /// means the stored vectors are in a different space and must be
    /// re-embedded via [`reembed_all`].
    async fn stored_model_fingerprints(&self) -> Result<Vec<(String, usize)>>;

    /// Re-embed every memory's content with `embedder`, updating the stored
    /// vector + the model fingerprint. Runs on the blocking pool (the embed
    /// call is CPU-bound for a bundled model). Returns the count re-embedded.
    /// Fire-and-forget from the caller's perspective (spawned as a background
    /// task); never blocks the UI or the agent loop. `progress`, when given,
    /// is called with `(done, total)` after each row is re-embedded and
    /// persisted, so callers can drive a progress bar.
    async fn reembed_all(
        &self,
        embedder: Arc<dyn Embedder>,
        progress: Option<&(dyn Fn(usize, usize) + Send + Sync)>,
    ) -> Result<usize>;

    /// Start a new session.
    async fn start_session(&self, project: &str) -> Result<Session>;

    /// End a session.
    async fn end_session(&self, id: &str) -> Result<()>;

    /// Record a working-memory tool event.
    async fn record_tool_event(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        tool_input: serde_json::Value,
        tool_output: &str,
        error: Option<&str>,
    ) -> Result<String>;

    /// Record one LLM-request usage row (tokens + timing + cached tokens).
    async fn record_request_stats(&self, stats: &RequestStats) -> Result<()>;

    /// List a session's raw `request_stats` rows (ordered by creation time)
    /// — the row-level view the aggregates are built from, including the
    /// outcome/purpose tags and the NULL `cached_tokens` on error rows.
    async fn request_stats_rows(&self, session_id: &str) -> Result<Vec<RequestStats>>;

    /// Aggregate token usage + timing for a single session.
    async fn session_stats(&self, session_id: &str) -> Result<SessionStats>;

    /// Aggregate token usage + timing across the whole project (all sessions).
    async fn project_stats(&self) -> Result<ProjectStats>;

    /// List all sessions with their aggregated token counts (for the Stats
    /// tab's session list). Ordered by creation time, newest first.
    async fn session_list(&self) -> Result<Vec<SessionSummary>>;
}

/// The SQLite-backed memory store.
///
/// Uses two connections: `write_conn` is the single serialized writer (all
/// INSERT/UPDATE/DELETE), `read_conn` serves reads (recall / list / stats).
/// With WAL mode enabled (see `schema::apply_pragmas`) readers never block on
/// the writer, so concurrent agents' auto-recalls no longer serialize behind
/// writes the way they did with a single shared connection. For in-memory
/// stores (tests) both connections point at the same shared-cache database,
/// so `read_conn` == `write_conn` and behavior is identical to before.
pub struct MemoryStore {
    /// The single serialized writer. Also serves reads when the store is
    /// in-memory (per-connection database).
    conn: Arc<Mutex<Connection>>,
    /// The read connection. `None` for in-memory stores, where a second
    /// connection would not see the same database; reads then use `conn`.
    read_conn: Option<Arc<Mutex<Connection>>>,
    /// Embedding backend. Behind an `RwLock` so Settings can swap the
    /// embedder (e.g. Ollama model/endpoint) at runtime without reopening
    /// the SQLite database.
    embedder: RwLock<Arc<dyn Embedder>>,
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
    /// The rolling memory-access log (reads + writes), newest-last, capped
    /// at [`MAX_ACCESS_LOG_ENTRIES`]. In-memory only — not persisted to
    /// SQLite — so a restart starts it empty. Guarded by its own short
    /// std `Mutex` (never held across an await) so [`Self::access_log`]
    /// snapshots don't contend with the SQLite connections.
    access_log: Mutex<VecDeque<MemoryAccessEntry>>,
    /// The live retrieval/indexing scale knobs (Phase 2 `[memory]` config
    /// section). Read per recall/index so a Settings save takes effect
    /// without a restart; written only at startup + on config save.
    search_config: RwLock<MemorySearchConfig>,
    /// The mutation counter behind [`MemoryStoreTrait::version`] — bumped
    /// (Relaxed) on every recall-result-changing write so the per-turn
    /// auto-recall cache can detect mid-turn writes.
    store_version: std::sync::atomic::AtomicU64,
}

/// The outcome of an FTS5 candidate lookup, distinguishing "FTS unavailable"
/// (→ fall back to a capped full scan) from "FTS available but no matches"
/// (→ also fall back to the capped semantic scan — a zero-keyword recall is
/// precisely the case the embeddings exist for).
enum FtsResult {
    /// FTS5 is not available (old SQLite / extension missing) — fall back to a
    /// capped full scan.
    Unavailable,
    /// FTS5 is available but the query matched no memories — fall back to the
    /// capped semantic scan (same as Unavailable): the query vector still
    /// gets a brute-force cosine pass over the 200 most-recent memories.
    NoMatches,
    /// FTS5 matched these rowids — load + score only these.
    Matches(Vec<i64>),
}

impl MemoryStore {
    /// Open (or create) a memory store at the given `.db` path.
    ///
    /// Opens two connections (read + write) so reads don't contend with the
    /// serialized writer under WAL.
    pub fn open(path: &Path, embedder: Arc<dyn Embedder>) -> Result<Self> {
        let conn = Connection::open(path)?;
        schema::apply_schema(&conn)?;
        let read_conn = Connection::open(path)?;
        schema::apply_schema(&read_conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            read_conn: Some(Arc::new(Mutex::new(read_conn))),
            embedder: RwLock::new(embedder),
            now: Arc::new(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0)
            }),
            access_log: Mutex::new(VecDeque::new()),
            search_config: RwLock::new(MemorySearchConfig::default()),
            store_version: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// Swap the embedding backend at runtime (Settings → Memory). Takes
    /// effect on the next write/recall; already-embedded rows keep their
    /// previous vectors until re-written.
    pub fn set_embedder(&self, embedder: Arc<dyn Embedder>) {
        *self.embedder.write().expect("embedder lock poisoned") = embedder;
    }

    /// The live retrieval/indexing scale knobs (the Phase 2 `[memory]`
    /// config section). Read per recall/index so a Settings save takes
    /// effect without a restart.
    pub fn memory_search_config(&self) -> MemorySearchConfig {
        self.search_config
            .read()
            .expect("search config lock poisoned")
            .clone()
    }

    /// Update the live search knobs (startup load + Settings save).
    pub fn set_memory_search_config(&self, config: MemorySearchConfig) {
        *self
            .search_config
            .write()
            .expect("search config lock poisoned") = config;
    }

    /// Snapshot the live embedder (diagnostics / tests / the failure-triage
    /// kNN overlay's per-classification getter). Poison-recovering: the
    /// field is a plain `Arc` swap, so a poisoned lock (some other holder
    /// panicked mid-swap) still yields a valid embedder instead of
    /// panicking — the overlay rides a failure-handling path and must
    /// never panic a turn.
    pub fn embedder_handle(&self) -> Arc<dyn Embedder> {
        self.embedder
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Append one entry to the rolling access log, evicting the oldest
    /// entry when the ring is at [`MAX_ACCESS_LOG_ENTRIES`]. `detail` is
    /// truncated (char-boundary safe) to [`MAX_ACCESS_DETAIL_CHARS`].
    /// Called by `write`, `recall`, and `strongest` on their success paths
    /// only — a failed access is not an access worth logging.
    fn log_access(
        &self,
        op: &'static str,
        tier: Option<String>,
        detail: impl AsRef<str>,
        hits: Option<usize>,
    ) {
        let detail: String = detail
            .as_ref()
            .chars()
            .take(MAX_ACCESS_DETAIL_CHARS)
            .collect();
        let mut log = self.access_log.lock().expect("access log lock poisoned");
        if log.len() >= MAX_ACCESS_LOG_ENTRIES {
            log.pop_front();
        }
        log.push_back(MemoryAccessEntry {
            at: (self.now)(),
            op: op.to_string(),
            tier,
            detail,
            hits,
        });
    }

    /// A newest-first snapshot of the rolling memory-access log (reads +
    /// writes, capped at [`MAX_ACCESS_LOG_ENTRIES`]). The log is in-memory
    /// (not SQLite), so this is a clone under a short std `Mutex` with no
    /// blocking-pool hop — safe to call inline from IPC. Empty until
    /// something flows through `write` / `recall` / `strongest`.
    pub fn access_log(&self) -> Vec<MemoryAccessEntry> {
        let log = self.access_log.lock().expect("access log lock poisoned");
        log.iter().rev().cloned().collect()
    }

    /// Rebuild the FTS5 full-text index from the memories table. The index is
    /// an external-content table, so the special `'rebuild'` command re-reads
    /// every row and re-indexes it (same statement the one-time migration in
    /// `schema.rs` uses). A no-op when FTS5 is unavailable — recall falls back
    /// to a capped full scan in that case, so there is nothing to rebuild.
    pub async fn rebuild_fts(&self) -> Result<()> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let conn = conn.lock().expect("conn lock poisoned");
            if !schema::fts_available(&conn) {
                return Ok(());
            }
            conn.execute_batch("INSERT INTO memories_fts(memories_fts) VALUES('rebuild');")?;
            Ok(())
        })
        .await
        .map_err(|e| Error::Memory(format!("rebuild_fts task failed: {e}")))?
    }

    /// VACUUM the database — reclaims the space freed by deleted working-tier
    /// rows after cleanup/consolidation. Runs on the blocking pool holding the
    /// write lock (writers serialize behind it for the duration; recall reads
    /// on the second WAL connection are unaffected).
    pub async fn vacuum(&self) -> Result<()> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let conn = conn.lock().expect("conn lock poisoned");
            conn.execute_batch("VACUUM;")?;
            Ok(())
        })
        .await
        .map_err(|e| Error::Memory(format!("vacuum task failed: {e}")))?
    }

    // ── Phase 2: derived-record indexer plumbing ────────────────────────────
    //
    // These are inherent methods (not `MemoryStoreTrait`) because every caller
    // — the indexer, the maintenance IPC, the startup bootstrap — holds the
    // concrete `MemoryStore` (same shape as `maintenance::rebuild_search`,
    // which takes `&Arc<MemoryStore>`). Keeping them off the trait leaves the
    // agent-facing funnel lean and existing mocks untouched.

    /// Count memories by provenance class — `(authored, derived)`. Backs the
    /// Settings → Memory derived-index card and the lazy-bootstrap check (an
    /// empty derived class means the indexer has never run). The grouped scan
    /// rides `idx_memories_record_class`, never a full table scan.
    pub async fn count_by_class(&self) -> Result<(usize, usize)> {
        let conn = self.read_conn().clone();
        tokio::task::spawn_blocking(move || -> Result<(usize, usize)> {
            let conn = conn.lock().expect("read conn lock poisoned");
            let mut stmt =
                conn.prepare("SELECT record_class, COUNT(*) FROM memories GROUP BY record_class")?;
            let mut rows = stmt.query([])?;
            let mut authored = 0usize;
            let mut derived = 0usize;
            while let Some(row) = rows.next()? {
                let class: String = row.get(0)?;
                let n: i64 = row.get(1)?;
                match class.as_str() {
                    "authored" => authored = n as usize,
                    "derived" => derived = n as usize,
                    _ => {}
                }
            }
            Ok((authored, derived))
        })
        .await
        .map_err(|e| Error::Memory(format!("count_by_class task failed: {e}")))?
    }

    /// Hard-delete every derived (indexer-built) memory — the Phase-2
    /// rebuild's wipe step. Authored rows are sacred: the WHERE clause is
    /// class-scoped so they are NEVER touched. The FTS AFTER DELETE trigger
    /// keeps the index in sync row by row. Returns the number deleted.
    pub async fn delete_derived(&self) -> Result<usize> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<usize> {
            let conn = conn.lock().expect("conn lock poisoned");
            let deleted =
                conn.execute("DELETE FROM memories WHERE record_class = 'derived'", [])?;
            Ok(deleted)
        })
        .await
        .map_err(|e| Error::Memory(format!("delete_derived task failed: {e}")))?
    }

    /// Fetch a single memory by id — `None` when no such row exists. A
    /// point lookup for the indexer and tests; recall/list remain the query
    /// paths.
    pub async fn get_memory(&self, id: &str) -> Result<Option<Memory>> {
        let conn = self.read_conn().clone();
        let id = id.to_string();
        tokio::task::spawn_blocking(move || -> Result<Option<Memory>> {
            let conn = conn.lock().expect("read conn lock poisoned");
            let mut stmt = conn.prepare(
                "SELECT id, tier, title, content, data, strength, access_count, created_at, \
                 last_accessed_at, source_session_ids, embedding, record_class, record_type, \
                 superseded_by FROM memories WHERE id = ?1",
            )?;
            let mut rows = stmt.query(params![id])?;
            match rows.next()? {
                Some(row) => Ok(Some(Self::parse_memory_row(row)?)),
                None => Ok(None),
            }
        })
        .await
        .map_err(|e| Error::Memory(format!("get_memory task failed: {e}")))?
    }

    /// Look up the incremental-indexing state for one source key — the
    /// content hash + memory id produced by the last index run. `None` means
    /// the source was never indexed (or a rebuild wiped the table).
    pub async fn index_state_get(&self, source_key: &str) -> Result<Option<(String, String)>> {
        let conn = self.read_conn().clone();
        let key = source_key.to_string();
        tokio::task::spawn_blocking(move || -> Result<Option<(String, String)>> {
            let conn = conn.lock().expect("read conn lock poisoned");
            let mut stmt = conn.prepare(
                "SELECT content_hash, memory_id FROM derived_index_state WHERE source_key = ?1",
            )?;
            let mut rows = stmt.query(params![key])?;
            match rows.next()? {
                Some(row) => Ok(Some((row.get(0)?, row.get(1)?))),
                None => Ok(None),
            }
        })
        .await
        .map_err(|e| Error::Memory(format!("index_state_get task failed: {e}")))?
    }

    /// Record (or refresh) the indexing state for one source key, stamped
    /// with the store's clock. Called after the source's derived memory was
    /// (re)written.
    pub async fn index_state_upsert(
        &self,
        source_key: &str,
        content_hash: &str,
        memory_id: &str,
    ) -> Result<()> {
        let conn = self.conn.clone();
        let key = source_key.to_string();
        let hash = content_hash.to_string();
        let mem = memory_id.to_string();
        let at = (self.now)();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let conn = conn.lock().expect("conn lock poisoned");
            conn.execute(
                "INSERT INTO derived_index_state (source_key, content_hash, memory_id, indexed_at) \
                 VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT(source_key) DO UPDATE SET \
                   content_hash = excluded.content_hash, \
                   memory_id = excluded.memory_id, \
                   indexed_at = excluded.indexed_at",
                params![key, hash, mem, at],
            )?;
            Ok(())
        })
        .await
        .map_err(|e| Error::Memory(format!("index_state_upsert task failed: {e}")))?
    }

    /// Drop one source key's indexing state — paired with `delete_memory`
    /// when its source file/item vanished (removal detection).
    pub async fn index_state_remove(&self, source_key: &str) -> Result<()> {
        let conn = self.conn.clone();
        let key = source_key.to_string();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let conn = conn.lock().expect("conn lock poisoned");
            conn.execute(
                "DELETE FROM derived_index_state WHERE source_key = ?1",
                params![key],
            )?;
            Ok(())
        })
        .await
        .map_err(|e| Error::Memory(format!("index_state_remove task failed: {e}")))?
    }

    /// Drop ALL indexing state — the rebuild's wipe step, paired with
    /// [`MemoryStore::delete_derived`].
    pub async fn index_state_clear(&self) -> Result<()> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let conn = conn.lock().expect("conn lock poisoned");
            conn.execute("DELETE FROM derived_index_state", [])?;
            Ok(())
        })
        .await
        .map_err(|e| Error::Memory(format!("index_state_clear task failed: {e}")))?
    }

    /// Every indexed source key — the indexer diffs these against the
    /// sources seen on the current run to detect removals.
    pub async fn index_state_keys(&self) -> Result<Vec<String>> {
        let conn = self.read_conn().clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<String>> {
            let conn = conn.lock().expect("read conn lock poisoned");
            let mut stmt = conn.prepare("SELECT source_key FROM derived_index_state")?;
            let mut rows = stmt.query([])?;
            let mut keys = Vec::new();
            while let Some(row) = rows.next()? {
                keys.push(row.get(0)?);
            }
            Ok(keys)
        })
        .await
        .map_err(|e| Error::Memory(format!("index_state_keys task failed: {e}")))?
    }

    /// The most recent index timestamp across all sources (`MAX(indexed_at)`),
    /// shown as "last built" in Settings → Memory. `None` when never indexed.
    pub async fn index_state_last_built(&self) -> Result<Option<i64>> {
        let conn = self.read_conn().clone();
        tokio::task::spawn_blocking(move || -> Result<Option<i64>> {
            let conn = conn.lock().expect("read conn lock poisoned");
            let latest: Option<i64> = conn.query_row(
                "SELECT MAX(indexed_at) FROM derived_index_state",
                [],
                |row| row.get(0),
            )?;
            Ok(latest)
        })
        .await
        .map_err(|e| Error::Memory(format!("index_state_last_built task failed: {e}")))?
    }

    /// Set a DERIVED row's `superseded_by` + `data` JSON in one UPDATE — the
    /// knowledge-indexer's reconciliation write. Class-scoped
    /// (`WHERE record_class = 'derived'`) so it can never touch authored
    /// knowledge; returns `false` when the id is unknown or not derived.
    /// Idempotent — rewriting the already-stored values is a row-wise no-op.
    ///
    /// The knowledge supersede model resolves successor FILES at scan time,
    /// so an UNCHANGED predecessor (skipped by the indexer's content-hash
    /// check) still needs its `superseded_by` flipped when a successor file
    /// appears — that reconciliation flows through here.
    pub async fn set_derived_metadata(
        &self,
        id: &str,
        superseded_by: Option<&str>,
        data: &serde_json::Value,
    ) -> Result<bool> {
        let conn = self.conn.clone();
        let id = id.to_string();
        let superseded_by = superseded_by.map(str::to_string);
        let data = serde_json::to_string(data).unwrap_or_else(|_| "{}".into());
        tokio::task::spawn_blocking(move || -> Result<bool> {
            let conn = conn.lock().expect("conn lock poisoned");
            let updated = conn.execute(
                "UPDATE memories SET superseded_by = ?1, data = ?2 \
                 WHERE id = ?3 AND record_class = 'derived'",
                params![superseded_by, data, id],
            )?;
            Ok(updated > 0)
        })
        .await
        .map_err(|e| Error::Memory(format!("set_derived_metadata task failed: {e}")))?
    }

    /// Open an in-memory store (for tests).
    ///
    /// Uses a shared-cache in-memory URI so the read and write connections
    /// see the SAME database (a plain `:memory:` DB is per-connection, which
    /// would split the store into two empty halves). WAL is a no-op here.
    pub fn open_in_memory(embedder: Arc<dyn Embedder>) -> Result<Self> {
        Self::open_shared_in_memory(embedder, None::<Box<dyn Fn() -> i64 + Send + Sync>>)
    }

    /// Create a store with a fixed clock (for deterministic tests).
    pub fn open_in_memory_with_clock(
        embedder: Arc<dyn Embedder>,
        clock: impl Fn() -> i64 + Send + Sync + 'static,
    ) -> Result<Self> {
        Self::open_shared_in_memory(embedder, Some(Box::new(clock)))
    }

    /// Shared constructor for the in-memory variants. Opens two connections
    /// to one shared-cache in-memory database (unique per store instance via
    /// a UUID name) so reads behave exactly like file-backed reads.
    fn open_shared_in_memory(
        embedder: Arc<dyn Embedder>,
        clock: Option<Box<dyn Fn() -> i64 + Send + Sync + 'static>>,
    ) -> Result<Self> {
        let uri = format!(
            "file:memdb-{}?mode=memory&cache=shared",
            uuid::Uuid::new_v4()
        );
        let conn = Connection::open(&uri)?;
        schema::apply_schema(&conn)?;
        let read_conn = Connection::open(&uri)?;
        schema::apply_schema(&read_conn)?;
        let now: Arc<dyn Fn() -> i64 + Send + Sync> = match clock {
            Some(c) => Arc::new(c),
            None => Arc::new(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0)
            }),
        };
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            read_conn: Some(Arc::new(Mutex::new(read_conn))),
            embedder: RwLock::new(embedder),
            now,
            access_log: Mutex::new(VecDeque::new()),
            search_config: RwLock::new(MemorySearchConfig::default()),
            store_version: std::sync::atomic::AtomicU64::new(0),
        })
    }

    fn now(&self) -> i64 {
        (self.now)()
    }

    /// The connection to use for read-only queries: the dedicated read
    /// connection when present (file-backed), otherwise the write connection
    /// (in-memory fallback). Reads on the read connection never contend with
    /// the writer under WAL.
    fn read_conn(&self) -> &Arc<Mutex<Connection>> {
        self.read_conn.as_ref().unwrap_or(&self.conn)
    }

    /// Encode an embedding as a BLOB (little-endian f32 bytes).
    fn encode_embedding(embedding: &[f32]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(embedding.len() * 4);
        for v in embedding {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        bytes
    }

    /// Decode an embedding BLOB back to Vec<f32>.
    fn decode_embedding(blob: &[u8]) -> Vec<f32> {
        blob.chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    }

    /// Compute cosine similarity between two vectors.
    fn cosine(a: &[f32], b: &[f32]) -> f32 {
        let dot = a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
        let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let nb = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        if na == 0.0 || nb == 0.0 {
            0.0
        } else {
            dot / (na * nb)
        }
    }

    /// The tier-weight component of the recall score. Distilled tiers
    /// (semantic/procedural/episodic) outrank raw working-tier snapshots so a
    /// distilled fact beats a raw tool-output event when both match a query.
    /// Working is weighted 0 — it is also excluded from recall by default,
    /// but is reachable via an explicit tier filter or `include_working()`,
    /// where it must never outscore a distilled fact.
    fn tier_weight(tier: MemoryTier) -> f64 {
        match tier {
            MemoryTier::Semantic => 0.3,
            MemoryTier::Procedural => 0.25,
            MemoryTier::Episodic => 0.2,
            MemoryTier::Working => 0.0,
        }
    }

    /// The class-weight component of the recall score. Authored records
    /// (agent/user-written facts, decisions, workflows) get a small lift
    /// over derived records (indexer-built digests of on-disk truth) so an
    /// authored fact wins relevance ties: derived records are POINTERS to
    /// knowledge that lives in files, authored records ARE the knowledge —
    /// on a tie the knowledge surfaces first. Deliberately small (0.05)
    /// next to `tier_weight` (0.0–0.3): provenance breaks ties, it does not
    /// reorder tiers.
    fn class_weight(class: MemoryClass) -> f64 {
        match class {
            MemoryClass::Authored => 0.05,
            MemoryClass::Derived => 0.0,
        }
    }

    /// Case-insensitive ASCII substring check WITHOUT allocation (avoids the
    /// per-memory `to_lowercase()` allocation in the recall scoring loop —
    /// P2). Returns true if `haystack` contains `needle` ignoring ASCII case.
    /// Non-ASCII chars compare by exact byte (a reasonable fallback; memory
    /// titles/content are predominantly ASCII).
    fn contains_ascii_ci(haystack: &str, needle: &str) -> bool {
        if needle.is_empty() {
            return true;
        }
        let h = haystack.as_bytes();
        let n = needle.as_bytes();
        if n.len() > h.len() {
            return false;
        }
        // Slide a window the length of the needle; compare ASCII-case-insensitively.
        'outer: for i in 0..=(h.len() - n.len()) {
            for j in 0..n.len() {
                if h[i + j].to_ascii_lowercase() != n[j].to_ascii_lowercase() {
                    continue 'outer;
                }
            }
            return true;
        }
        false
    }

    /// Embed `memory.content` when it carries no embedding yet and return
    /// the `(blob, model, dim)` persistence triple. Infallible: a failed
    /// remote embed yields a zero vector (same space, contributes nothing to
    /// cosine) rather than killing the write. A pre-set embedding (e.g. a
    /// test) gets an empty model fingerprint of the embedding's length.
    async fn embedding_for_insert(&self, memory: &mut Memory) -> (Vec<u8>, String, i64) {
        let embed_model;
        let embed_dim;
        if memory.embedding.is_empty() {
            let embedder = self.embedder_handle();
            memory.embedding = embedder.embed(&memory.content).await;
            embed_model = embedder.model_id().to_string();
            embed_dim = embedder.dim() as i64;
        } else {
            // A pre-set embedding (e.g. a test) has no known model fingerprint.
            embed_model = String::new();
            embed_dim = memory.embedding.len() as i64;
        }
        (
            Self::encode_embedding(&memory.embedding),
            embed_model,
            embed_dim,
        )
    }

    /// INSERT OR REPLACE one memory row on an already-held connection (the
    /// write connection, or a transaction on it). Shared by `write` and
    /// `supersede_memory` so both persist the full column set — including
    /// the semantic-record columns (`record_class` / `record_type` /
    /// `superseded_by`) — faithfully from the struct. The FTS sync triggers
    /// keep the index in step.
    fn insert_memory_locked(
        conn: &Connection,
        memory: &Memory,
        embedding_blob: &[u8],
        embed_model: &str,
        embed_dim: i64,
    ) -> Result<()> {
        let data_json = serde_json::to_string(&memory.data).unwrap_or_else(|_| "{}".into());
        let source_json =
            serde_json::to_string(&memory.source_session_ids).unwrap_or_else(|_| "[]".into());
        conn.execute(
            "INSERT OR REPLACE INTO memories (id, tier, title, content, data, strength, access_count, created_at, last_accessed_at, source_session_ids, embedding, embed_model, embed_dim, record_class, record_type, superseded_by) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                memory.id,
                memory.tier.as_str(),
                memory.title,
                memory.content,
                data_json,
                memory.strength,
                memory.access_count,
                memory.created_at,
                memory.last_accessed_at,
                source_json,
                embedding_blob,
                embed_model,
                embed_dim,
                memory.record_class.as_str(),
                memory.record_type.as_str(),
                memory.superseded_by,
            ],
        )?;
        Ok(())
    }

    /// Parse a single memory row into a `Memory` value.
    ///
    /// Column order must match the SELECT used by `load_all_locked` /
    /// `load_by_rowids_locked`:
    /// `id, tier, title, content, data, strength, access_count, created_at,
    ///  last_accessed_at, source_session_ids, embedding, record_class,
    ///  record_type, superseded_by`.
    fn parse_memory_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Memory> {
        let embedding_blob: Option<Vec<u8>> = row.get(10)?;
        let embedding = embedding_blob
            .map(|b| Self::decode_embedding(&b))
            .unwrap_or_default();
        let tier_str: String = row.get(1)?;
        let source_json: String = row.get(9)?;
        let data_json: String = row.get(4)?;
        let class_str: String = row.get(11)?;
        let rtype_str: String = row.get(12)?;
        Ok(Memory {
            id: row.get(0)?,
            tier: MemoryTier::from_str(&tier_str).unwrap_or(MemoryTier::Working),
            title: row.get(2)?,
            content: row.get(3)?,
            data: serde_json::from_str(&data_json).unwrap_or(serde_json::Value::Null),
            strength: row.get(5)?,
            access_count: row.get(6)?,
            created_at: row.get(7)?,
            last_accessed_at: row.get(8)?,
            source_session_ids: serde_json::from_str(&source_json).unwrap_or_default(),
            // Unknown/legacy strings degrade to the defaults (Authored/None)
            // rather than failing the whole load.
            record_class: MemoryClass::from_str(&class_str).unwrap_or_default(),
            record_type: MemoryRecordType::from_str(&rtype_str).unwrap_or_default(),
            superseded_by: row.get(13)?,
            embedding,
        })
    }

    /// Load all memories (optionally filtered by tier) with their embeddings.
    async fn load_all(&self, tier: Option<MemoryTier>) -> Result<Vec<Memory>> {
        let read_conn = self.read_conn().clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<Memory>> {
            let conn = read_conn.lock().expect("read conn lock poisoned");
            Self::load_all_locked(&conn, tier)
        })
        .await
        .map_err(|e| Error::Memory(format!("load_all task failed: {e}")))?
    }

    /// Locked-connection variant of `load_all` (no async lock acquisition).
    fn load_all_locked(conn: &Connection, tier: Option<MemoryTier>) -> Result<Vec<Memory>> {
        let sql = if tier.is_some() {
            "SELECT id, tier, title, content, data, strength, access_count, created_at, last_accessed_at, source_session_ids, embedding, record_class, record_type, superseded_by FROM memories WHERE tier = ?1"
        } else {
            "SELECT id, tier, title, content, data, strength, access_count, created_at, last_accessed_at, source_session_ids, embedding, record_class, record_type, superseded_by FROM memories"
        };
        let mut stmt = conn.prepare(sql)?;
        let mut out = Vec::new();
        let mut rows = if let Some(t) = tier {
            stmt.query(rusqlite::params![t.as_str()])?
        } else {
            stmt.query([])?
        };
        while let Some(row) = rows.next()? {
            out.push(Self::parse_memory_row(row)?);
        }
        Ok(out)
    }

    /// Load the most-recent `limit` memories matching `filter` with their
    /// embeddings — the capped full-scan fallback used when FTS5 is
    /// unavailable. Caps the O(N) fallback at `limit` (e.g. 200) so a large
    /// store doesn't load every row every turn (P2). Applies the same filter
    /// semantics as the FTS path: an explicit `tier` wins over the default
    /// working-tier exclusion (working memories are consolidation input, not
    /// recall output); superseded rows and `record_type` restrict at SQL
    /// level so the fallback never post-filters a full scan.
    fn load_recent_locked(
        conn: &Connection,
        filter: &MemoryFilter,
        limit: usize,
    ) -> Result<Vec<Memory>> {
        // Bare `?` placeholders bind in the order params are pushed.
        let mut sql = String::from(
            "SELECT id, tier, title, content, data, strength, access_count, created_at, \
             last_accessed_at, source_session_ids, embedding, record_class, record_type, \
             superseded_by FROM memories WHERE 1=1",
        );
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(t) = filter.tier {
            sql.push_str(" AND tier = ?");
            params_vec.push(Box::new(t.as_str().to_string()));
        } else if filter.exclude_working {
            sql.push_str(" AND tier != 'working'");
        }
        if filter.exclude_superseded {
            sql.push_str(" AND superseded_by IS NULL");
        }
        if let Some(rt) = filter.record_type {
            sql.push_str(" AND record_type = ?");
            params_vec.push(Box::new(rt.as_str().to_string()));
        }
        sql.push_str(" ORDER BY last_accessed_at DESC LIMIT ?");
        params_vec.push(Box::new(limit as i64));
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query(rusqlite::params_from_iter(params_vec.into_iter()))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(Self::parse_memory_row(row)?);
        }
        Ok(out)
    }

    /// Load a specific set of memories by rowid (optionally filtered by tier).
    /// Used by the FTS pre-filter path to load only the candidate rows.
    fn load_by_rowids_locked(
        conn: &Connection,
        rowids: &[i64],
        tier: Option<MemoryTier>,
    ) -> Result<Vec<Memory>> {
        if rowids.is_empty() {
            return Ok(Vec::new());
        }
        // Build a parameterized IN (?, ?, ...) clause.
        let placeholders: Vec<String> = (0..rowids.len()).map(|i| format!("?{}", i + 1)).collect();
        let mut sql = String::from(
            "SELECT id, tier, title, content, data, strength, access_count, created_at, \
             last_accessed_at, source_session_ids, embedding, record_class, record_type, superseded_by FROM memories \
             WHERE rowid IN (",
        );
        sql.push_str(&placeholders.join(", "));
        sql.push(')');
        if tier.is_some() {
            sql.push_str(" AND tier = ?");
        }
        let mut stmt = conn.prepare(&sql)?;
        let mut out = Vec::new();
        // rusqlite params_from needs a single iterator; build it dynamically.
        let mut rows = if let Some(t) = tier {
            let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = rowids
                .iter()
                .map(|r| Box::new(*r) as Box<dyn rusqlite::ToSql>)
                .collect();
            params_vec.push(Box::new(t.as_str().to_string()));
            stmt.query(rusqlite::params_from_iter(params_vec.into_iter()))?
        } else {
            stmt.query(rusqlite::params_from_iter(rowids.iter().map(|r| *r as i64)))?
        };
        while let Some(row) = rows.next()? {
            out.push(Self::parse_memory_row(row)?);
        }
        Ok(out)
    }

    /// Query the FTS5 index for the top-K candidate rowids matching `query`,
    /// ranked by FTS relevance. Distinguishes "FTS unavailable" (→ caller
    /// falls back to a capped full scan) from "FTS available but no matches"
    /// (→ caller also falls back to the capped semantic scan — zero-keyword
    /// recall is the embeddings' core case). Applies the filter at SQL level so
    /// the candidate pool never needs post-filtering: an explicit `tier`
    /// wins over the default working-tier exclusion (working memories are
    /// consolidation input, not recall output); superseded rows are excluded
    /// unless the filter opted in; a `record_type` restricts the pool.
    fn fts_candidates_locked(
        conn: &Connection,
        query: &str,
        filter: &MemoryFilter,
        limit: usize,
    ) -> FtsResult {
        if !schema::fts_available(conn) {
            return FtsResult::Unavailable;
        }
        // FTS5 MATCH uses its own query syntax. Build an OR query across the
        // query's whitespace tokens so a recall pre-filter catches any memory
        // sharing at least one keyword (a wide candidate net for cosine
        // re-ranking). Each token is double-quoted (with embedded quotes
        // doubled) so special characters like ':', '*', '(' are treated
        // literally and never raise a syntax error.
        let fts_query: String = query
            .split_whitespace()
            .map(|tok| format!("\"{}\"", tok.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" OR ");
        if fts_query.is_empty() {
            // FTS is available but the query had no tokens — treat as "no
            // matches" (semantic scan) rather than unavailable.
            return FtsResult::NoMatches;
        }
        // One SQL shape (always the JOIN) with the filter's WHERE clauses
        // appended in a fixed order; bare `?` placeholders bind in the same
        // order the params are pushed.
        let mut sql = String::from(
            "SELECT m.rowid \
             FROM memories_fts f \
             JOIN memories m ON m.rowid = f.rowid \
             WHERE memories_fts MATCH ?",
        );
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(fts_query)];
        if let Some(t) = filter.tier {
            sql.push_str(" AND m.tier = ?");
            params_vec.push(Box::new(t.as_str().to_string()));
        } else if filter.exclude_working {
            sql.push_str(" AND m.tier != 'working'");
        }
        if filter.exclude_superseded {
            sql.push_str(" AND m.superseded_by IS NULL");
        }
        if let Some(rt) = filter.record_type {
            sql.push_str(" AND m.record_type = ?");
            params_vec.push(Box::new(rt.as_str().to_string()));
        }
        sql.push_str(" ORDER BY rank LIMIT ?");
        params_vec.push(Box::new(limit as i64));
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return FtsResult::Unavailable,
        };
        let mut rows = match stmt.query(rusqlite::params_from_iter(params_vec.into_iter())) {
            Ok(r) => r,
            Err(_) => return FtsResult::Unavailable,
        };
        let mut out = Vec::new();
        while let Ok(Some(row)) = rows.next() {
            match row.get::<_, i64>(0) {
                Ok(id) => out.push(id),
                Err(_) => return FtsResult::Unavailable,
            }
        }
        if out.is_empty() {
            FtsResult::NoMatches
        } else {
            FtsResult::Matches(out)
        }
    }
}

#[async_trait]
impl MemoryStoreTrait for MemoryStore {
    fn version(&self) -> u64 {
        self.store_version.load(std::sync::atomic::Ordering::Relaxed)
    }

    async fn write(&self, mut memory: Memory) -> Result<String> {
        let (embedding_blob, embed_model, embed_dim) = self.embedding_for_insert(&mut memory).await;
        // The typed-record classification derives from the title prefix —
        // the prefix wins over any caller-set `record_type` (the prefix IS
        // the classification mechanism; see `MemoryRecordType::from_title`).
        memory.record_type = MemoryRecordType::from_title(&memory.title);

        let conn = self.conn.clone();
        let id = memory.id.clone();
        // Hoisted before the closure move below — the access-log line after
        // the await needs the tier + title, but `memory` itself is moved
        // into the spawn_blocking closure. `MemoryTier` is `Copy`.
        let tier = memory.tier;
        let title = memory.title.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let conn = conn.lock().expect("conn lock poisoned");
            Self::insert_memory_locked(&conn, &memory, &embedding_blob, &embed_model, embed_dim)
        })
        .await
        .map_err(|e| Error::Memory(format!("write task failed: {e}")))??;
        self.log_access("write", Some(tier.as_str().to_string()), title, None);
        // Invalidate per-turn auto-recall caches (the version counter — a
        // same-turn memory_write must be visible to the next recall). Bumped
        // AFTER the row is committed/visible: combined with the agent loop's
        // pre-recall version read, any write invisible to a recall snapshot
        // necessarily bumps above the stamped version (review LOW-3).
        self.store_version
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(id)
    }

    async fn recall(&self, query: &str, filter: &MemoryFilter) -> Result<Vec<ScoredMemory>> {
        let scored = self.recall_peek(query, filter).await?;
        // Bump access counts for all returned memories in a single query —
        // deliberate, agent-initiated lookups only (the memory_recall tool).
        // Passive surfacing (auto-recall, debug previews) must NOT bump:
        // see recall_peek's doc on the self-reinforcement loop.
        //
        // The bump is advisory and deliberately swallowed (quality review
        // LOW 6 class-closure assessment): a failed batch_access only
        // leaves recency/strength ranking slightly stale — it self-corrects
        // on the next successful access — and must not fail a recall that
        // already succeeded.
        let ids: Vec<String> = scored.iter().map(|sm| sm.memory.id.clone()).collect();
        let _ = self.batch_access(&ids).await;
        Ok(scored)
    }

    fn memory_search_config(&self) -> MemorySearchConfig {
        // Delegate to the inherent method (the live `[memory]` config
        // snapshot); the trait default returns the compiled-in defaults.
        MemoryStore::memory_search_config(self)
    }

    async fn recall_peek(&self, query: &str, filter: &MemoryFilter) -> Result<Vec<ScoredMemory>> {
        // Embed the query. Infallible: a failed remote embed returns a zero
        // vector, which degrades ranking to keyword + tier + strength (the
        // existing fallback path) rather than killing recall.
        let embedder = self.embedder_handle();
        let query_vec = embedder.embed(query).await;

        // Pre-filter candidates via the FTS5 index when available. This reduces
        // the cosine-rank pass from O(N) (every memory) to O(K) where K is the
        // number of keyword-matching candidates (K << N for large stores).
        // We over-fetch (top 50) so the cosine re-rank still has a good pool to
        // choose from, then apply the caller's `limit` after scoring.
        const FTS_CANDIDATE_POOL: usize = 50;
        // Cap the full-scan paths at the 200 most-recent memories so a large
        // store doesn't load every row every turn (P2). Used both when FTS
        // is unavailable AND when FTS returns no matches: a zero-keyword
        // recall is precisely the case the embeddings exist for (paraphrase,
        // stemming misses under unicode61, zero token overlap), so the
        // query vector still gets a capped brute-force cosine pass instead
        // of an empty result.
        const FULL_SCAN_FALLBACK_CAP: usize = 200;

        let memories = {
            // Read path: uses the read connection so a concurrent recall does
            // not serialize behind a writer (WAL). The synchronous FTS + load
            // runs on the blocking pool (Review R2). The filter rides into
            // the blocking task so both the FTS path and the full-scan
            // fallback apply its tier / superseded / record_type
            // restrictions at SQL level (scale: no post-filtered scans).
            let read_conn = self.read_conn().clone();
            let query_owned = query.to_string();
            let filter = filter.clone();
            tokio::task::spawn_blocking(move || -> Result<Vec<Memory>> {
                let conn = read_conn.lock().expect("read conn lock poisoned");
                match Self::fts_candidates_locked(&conn, &query_owned, &filter, FTS_CANDIDATE_POOL)
                {
                    FtsResult::Matches(rowids) => {
                        Self::load_by_rowids_locked(&conn, &rowids, filter.tier)
                    }
                    FtsResult::NoMatches => {
                        // FTS available but no keyword matches — fall through
                        // to the capped semantic scan (same as Unavailable).
                        // Zero keyword overlap is the recall case embeddings
                        // exist for; the query vector is already computed, so
                        // ranking the 200 most-recent by cosine is cheap and
                        // finds what the keyword net missed.
                        Self::load_recent_locked(&conn, &filter, FULL_SCAN_FALLBACK_CAP)
                    }
                    FtsResult::Unavailable => {
                        // FTS unavailable — fall back to a capped full scan (the
                        // 200 most-recent memories).
                        Self::load_recent_locked(&conn, &filter, FULL_SCAN_FALLBACK_CAP)
                    }
                }
            })
            .await
            .map_err(|e| Error::Memory(format!("recall load task failed: {e}")))??
        };

        // Score each by a combination of semantic similarity + strength +
        // tier weight + keyword boost. Use an allocation-free ASCII
        // case-insensitive contains for the keyword boost (P2: avoids a
        // to_lowercase() String allocation per memory per recall).
        //
        // Weight rationale: `sim` is embedder-aware (see `sim_weight` below) —
        // 0.2 under the hash embedder (a random hash projection: token
        // overlap, not meaning — a weak tiebreaker, not the primary signal),
        // 0.35 under real embedding models (bundled MiniLM default, remote),
        // where a strong semantic match (sim > 6/7 ≈ 0.857, the exact
        // crossover 0.3/0.35) legitimately outweighs
        // a bare keyword hit (0.3). Keyword overlap (0.3) stays the sharper
        // textual signal on ties. Tier weight (0.0–0.3) lifts distilled facts
        // above raw snapshots, decayed strength (0.3) provides recency, and
        // class weight (0.05 authored / 0 derived) tips ties toward authored
        // knowledge over indexer-derived pointers.
        // The live [memory] knobs (Phase 2): decay half-life, per-query cap,
        // and the derived per-class backstop — read per recall so a Settings
        // save takes effect without a restart.
        let search_config = self.memory_search_config();
        let decay_tau_secs = search_config.decay_half_life_days * 86_400.0;
        let q_lower = query.to_ascii_lowercase();
        // Embedder-aware sim weight (2026-12-06 re-tune): the hash embedder's
        // cosine is token-overlap noise, so it keeps the historic 0.2; real
        // embedding models (bundled MiniLM default, remote) encode meaning,
        // so they get 0.35 — a strong match (sim > 6/7 ≈ 0.857, the exact
        // crossover 0.3/0.35) then outweighs a
        // bare keyword hit (0.3). `model_id()` is a cheap sync &str compare,
        // computed once per recall.
        let sim_weight: f64 = if embedder.model_id() == "hash" {
            0.2
        } else {
            0.35
        };
        // Decay strength by time-since-last-access so old, never-re-accessed
        // memories (e.g. stale working-tier tool-output snapshots) sink in
        // ranking instead of retaining their write-time strength forever.
        // This is in-memory only — the stored `strength` is NOT mutated; only
        // the ranking score uses the decayed form.
        let now = (self.now)();
        let mut scored: Vec<ScoredMemory> = memories
            .into_iter()
            .map(|m| {
                let sim = if m.embedding.is_empty() {
                    0.0
                } else {
                    Self::cosine(&query_vec, &m.embedding) as f64
                };
                // Boost by keyword match (case-insensitive ASCII substring on
                // title/content) — allocation-free. Under the hash embedder
                // this is the only real textual signal (weighted above sim);
                // under real embedding models the two are near-peers.
                let keyword_boost = if Self::contains_ascii_ci(&m.title, &q_lower)
                    || Self::contains_ascii_ci(&m.content, &q_lower)
                {
                    0.3
                } else {
                    0.0
                };
                let decayed_strength =
                    strength::decay_with_tau(m.strength, now - m.last_accessed_at, decay_tau_secs);
                // Tier weight: distilled tiers (semantic/procedural/episodic)
                // outrank raw working-tier snapshots. Working is weighted 0 so
                // it never outscores a distilled fact when both are present
                // (working is also excluded from recall by default, but is
                // reachable via an explicit tier filter or include_working()).
                let tw = Self::tier_weight(m.tier);
                // Class weight: authored records edge out indexer-derived
                // digests on relevance ties (pointers vs knowledge). A
                // superseded row gets NO penalty here — SQL exclusion is the
                // mechanism; penalizing too would double-punish history.
                let cw = Self::class_weight(m.record_class);
                let score = sim * sim_weight + decayed_strength * 0.3 + keyword_boost + tw + cw;
                ScoredMemory { memory: m, score }
            })
            .collect();

        // Sort by score descending, with a deterministic title tiebreaker so
        // equal-score memories have a stable, cross-restart-deterministic order
        // (DB row order is not guaranteed across restarts).
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.memory.title.cmp(&b.memory.title))
        });

        // Scale backstop (Phase 2): cap DERIVED records per record type so a
        // large indexed corpus can't crowd authored knowledge out of the
        // result set. Authored records are never capped — they ARE the
        // knowledge.
        let mut derived_counts: std::collections::HashMap<MemoryRecordType, usize> =
            std::collections::HashMap::new();
        let per_class_cap = search_config.derived_per_class_cap;
        scored.retain(|sm| {
            if sm.memory.record_class == MemoryClass::Derived {
                let n = derived_counts.entry(sm.memory.record_type).or_insert(0);
                *n += 1;
                *n <= per_class_cap
            } else {
                true
            }
        });

        // Apply limit: an explicit filter limit wins; otherwise the configured
        // per-query cap (default 50 = the FTS candidate pool, so an untouched
        // config changes nothing on the FTS path).
        let limit = filter.limit.unwrap_or(search_config.per_query_cap);
        scored.truncate(limit);

        self.log_access(
            "read",
            filter.tier.map(|t| t.as_str().to_string()),
            query.to_string(),
            Some(scored.len()),
        );
        Ok(scored)
    }

    async fn access(&self, id: &str) -> Result<()> {
        let conn = self.conn.clone();
        let now = self.now();
        let id = id.to_string();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let conn = conn.lock().expect("conn lock poisoned");
            conn.execute(
                "UPDATE memories SET access_count = access_count + 1, last_accessed_at = ?1 WHERE id = ?2",
                params![now, id],
            )?;
            Ok(())
        })
        .await
        .map_err(|e| Error::Memory(format!("access task failed: {e}")))?
    }

    /// Bump access counts for many memories in a single query.
    ///
    /// Replaces the per-result `access()` loop in `recall()`, collapsing K
    /// lock+query cycles into one. Builds a parameterized `IN (...)` clause.
    async fn batch_access(&self, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let conn = self.conn.clone();
        let now = self.now();
        let ids: Vec<String> = ids.to_vec();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let conn = conn.lock().expect("conn lock poisoned");
            // Build "id IN (?1, ?2, ...)" with one placeholder per id.
            let placeholders: Vec<&str> = (0..ids.len()).map(|_| "?").collect();
            let sql = format!(
                "UPDATE memories SET access_count = access_count + 1, last_accessed_at = ?1 \
                 WHERE id IN ({})",
                placeholders.join(", ")
            );
            // First param is `now`; the rest are the ids.
            let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::with_capacity(ids.len() + 1);
            params_vec.push(Box::new(now));
            for id in &ids {
                params_vec.push(Box::new(id.clone()));
            }
            conn.execute(&sql, rusqlite::params_from_iter(params_vec.into_iter()))?;
            Ok(())
        })
        .await
        .map_err(|e| Error::Memory(format!("batch_access task failed: {e}")))?
    }

    async fn list_by_tier(&self, tier: MemoryTier) -> Result<Vec<Memory>> {
        self.load_all(Some(tier)).await
    }

    /// Override the default `count_by_tier` with a `SELECT COUNT(*)` — no row
    /// decoding, no embedding BLOB read. Used by the Memory debug tab's
    /// overview (polled every 2s), so the cheap query avoids loading full rows
    /// + vectors just to count them.
    async fn count_by_tier(&self, tier: MemoryTier) -> Result<usize> {
        let read_conn = self.read_conn().clone();
        tokio::task::spawn_blocking(move || -> Result<usize> {
            let conn = read_conn.lock().expect("read conn lock poisoned");
            let n: i64 = conn.query_row(
                "SELECT COUNT(*) FROM memories WHERE tier = ?1",
                params![tier.as_str()],
                |row| row.get(0),
            )?;
            Ok(n as usize)
        })
        .await
        .map_err(|e| Error::Memory(format!("count_by_tier task failed: {e}")))?
    }

    async fn update_memory(
        &self,
        id: &str,
        title: Option<&str>,
        content: Option<&str>,
    ) -> Result<bool> {
        // A content change re-embeds the row — the embedding derives from
        // content, so skipping this would leave semantic recall matching the
        // OLD text. Infallible (a failed remote embed yields a zero vector).
        let (blob, model, dim) = if let Some(new_content) = content {
            let embedder = self.embedder_handle();
            let vec = embedder.embed(new_content).await;
            (
                Some(Self::encode_embedding(&vec)),
                Some(embedder.model_id().to_string()),
                Some(embedder.dim() as i64),
            )
        } else {
            (None, None, None)
        };
        // A title change re-derives the typed-record classification from the
        // new title's prefix — including back to `none` when the new title
        // is untyped (the prefix wins, same rule as write()).
        let record_type = title.map(|t| MemoryRecordType::from_title(t).as_str().to_string());
        let conn = self.conn.clone();
        let id_owned = id.to_string();
        let title = title.map(|t| t.to_string());
        let content = content.map(|c| c.to_string());
        let updated = tokio::task::spawn_blocking(move || -> Result<bool> {
            let conn = conn.lock().expect("conn lock poisoned");
            if title.is_none() && content.is_none() {
                // Nothing to change — report existence (the contract is
                // "false when the id is unknown") without a no-op UPDATE.
                let exists: bool = conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM memories WHERE id = ?1)",
                    params![id_owned],
                    |row| row.get(0),
                )?;
                return Ok(exists);
            }
            // COALESCE keeps unchanged fields at their stored values; the
            // FTS UPDATE trigger's WHEN guard (title/content changed) then
            // re-indexes only when one of them actually moved.
            let updated = conn.execute(
                "UPDATE memories SET \
                   title = COALESCE(?2, title), \
                   content = COALESCE(?3, content), \
                   record_type = COALESCE(?4, record_type), \
                   embedding = COALESCE(?5, embedding), \
                   embed_model = COALESCE(?6, embed_model), \
                   embed_dim = COALESCE(?7, embed_dim) \
                 WHERE id = ?1",
                params![id_owned, title, content, record_type, blob, model, dim],
            )?;
            Ok(updated > 0)
        })
        .await
        .map_err(|e| Error::Memory(format!("update_memory task failed: {e}")))??;
        if updated {
            // Invalidate per-turn auto-recall caches (the version counter) —
            // only when a row actually changed, and AFTER it is committed/
            // visible (review LOW-3 — see write()).
            self.store_version
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(updated)
    }

    async fn supersede_memory(&self, old_id: &str, mut new_memory: Memory) -> Result<String> {
        let (embedding_blob, embed_model, embed_dim) =
            self.embedding_for_insert(&mut new_memory).await;
        // Same rule as write(): the title prefix classifies the record.
        new_memory.record_type = MemoryRecordType::from_title(&new_memory.title);
        let conn = self.conn.clone();
        let old_id_owned = old_id.to_string();
        let new_id = new_memory.id.clone();
        let tier = new_memory.tier;
        let title = new_memory.title.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut conn = conn.lock().expect("conn lock poisoned");
            // ONE transaction: insert the successor + mark the old row, or
            // neither (a partial supersede would corrupt the history chain;
            // an early Err return rolls `tx` back on drop).
            let tx = conn.transaction()?;
            // The old row must exist and still be live. The error names the
            // existing superseder so the caller can point at it instead of
            // retrying blindly.
            let old_state: Option<Option<String>> = tx
                .query_row(
                    "SELECT superseded_by FROM memories WHERE id = ?1",
                    params![old_id_owned],
                    |row| row.get(0),
                )
                .optional()?;
            match old_state {
                None => {
                    return Err(Error::Memory(format!(
                        "cannot supersede: no memory with id '{old_id_owned}'"
                    )));
                }
                Some(Some(superseder)) => {
                    return Err(Error::Memory(format!(
                        "cannot supersede: memory '{old_id_owned}' is already \
                         superseded by '{superseder}'"
                    )));
                }
                Some(None) => {}
            }
            Self::insert_memory_locked(&tx, &new_memory, &embedding_blob, &embed_model, embed_dim)?;
            tx.execute(
                "UPDATE memories SET superseded_by = ?1 WHERE id = ?2",
                params![new_memory.id, old_id_owned],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
        .map_err(|e| Error::Memory(format!("supersede_memory task failed: {e}")))??;
        self.log_access("write", Some(tier.as_str().to_string()), title, None);
        // Invalidate per-turn auto-recall caches (the version counter).
        // Bumped AFTER the transaction commits (review LOW-3 — see write()).
        self.store_version
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(new_id)
    }

    async fn delete_memory(&self, id: &str) -> Result<bool> {
        let conn = self.conn.clone();
        let id = id.to_string();
        let deleted = tokio::task::spawn_blocking(move || -> Result<bool> {
            let conn = conn.lock().expect("conn lock poisoned");
            // The FTS AFTER DELETE trigger keeps the index in sync.
            let deleted = conn.execute("DELETE FROM memories WHERE id = ?1", params![id])?;
            Ok(deleted > 0)
        })
        .await
        .map_err(|e| Error::Memory(format!("delete_memory task failed: {e}")))??;
        if deleted {
            // Invalidate per-turn auto-recall caches (the version counter) —
            // only when a row actually left, and AFTER the delete is
            // committed/visible (review LOW-3 — see write()).
            self.store_version
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(deleted)
    }

    // ── Derived-index plumbing ─────────────────────────────────────────────
    //
    // Thin delegates to the inherent `MemoryStore` methods (the single
    // implementation) so trait-object holders — the knowledge writer, which
    // holds the store the way the memory tools do — can drive the index.
    // Concrete receivers keep resolving to the inherent methods.

    async fn get_memory(&self, id: &str) -> Result<Option<Memory>> {
        MemoryStore::get_memory(self, id).await
    }

    async fn delete_derived(&self) -> Result<usize> {
        MemoryStore::delete_derived(self).await
    }

    async fn index_state_get(&self, source_key: &str) -> Result<Option<(String, String)>> {
        MemoryStore::index_state_get(self, source_key).await
    }

    async fn index_state_upsert(
        &self,
        source_key: &str,
        content_hash: &str,
        memory_id: &str,
    ) -> Result<()> {
        MemoryStore::index_state_upsert(self, source_key, content_hash, memory_id).await
    }

    async fn index_state_remove(&self, source_key: &str) -> Result<()> {
        MemoryStore::index_state_remove(self, source_key).await
    }

    async fn index_state_clear(&self) -> Result<()> {
        MemoryStore::index_state_clear(self).await
    }

    async fn index_state_keys(&self) -> Result<Vec<String>> {
        MemoryStore::index_state_keys(self).await
    }

    async fn set_derived_metadata(
        &self,
        id: &str,
        superseded_by: Option<&str>,
        data: &serde_json::Value,
    ) -> Result<bool> {
        MemoryStore::set_derived_metadata(self, id, superseded_by, data).await
    }

    async fn list_filtered(&self, filter: &MemoryFilter) -> Result<Vec<Memory>> {
        let read_conn = self.read_conn().clone();
        let filter = filter.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<Memory>> {
            let conn = read_conn.lock().expect("read conn lock poisoned");
            // The WHERE clauses hit the Phase-1 indexes (record_type /
            // created_at) — never a full scan at 1000s of records. Newest
            // first (created_at DESC). An explicit tier wins over the
            // default working-tier exclusion (same rule as recall). Bare `?`
            // placeholders bind in the order params are pushed.
            let mut sql = String::from(
                "SELECT id, tier, title, content, data, strength, access_count, created_at, \
                 last_accessed_at, source_session_ids, embedding, record_class, record_type, \
                 superseded_by FROM memories WHERE 1=1",
            );
            let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
            if let Some(t) = filter.tier {
                sql.push_str(" AND tier = ?");
                params_vec.push(Box::new(t.as_str().to_string()));
            } else if filter.exclude_working {
                sql.push_str(" AND tier != 'working'");
            }
            if filter.exclude_superseded {
                sql.push_str(" AND superseded_by IS NULL");
            }
            if let Some(rt) = filter.record_type {
                sql.push_str(" AND record_type = ?");
                params_vec.push(Box::new(rt.as_str().to_string()));
            }
            if let Some(prefix) = &filter.title_prefix {
                // Position-0 prefix match without LIKE-wildcard escaping:
                // substr compares exactly `prefix.chars()` leading chars.
                sql.push_str(" AND substr(title, 1, ?) = ?");
                params_vec.push(Box::new(prefix.chars().count() as i64));
                params_vec.push(Box::new(prefix.clone()));
            }
            sql.push_str(" ORDER BY created_at DESC");
            if let Some(limit) = filter.limit {
                sql.push_str(" LIMIT ?");
                params_vec.push(Box::new(limit as i64));
            }
            let mut stmt = conn.prepare(&sql)?;
            let mut rows = stmt.query(rusqlite::params_from_iter(params_vec.into_iter()))?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                out.push(Self::parse_memory_row(row)?);
            }
            Ok(out)
        })
        .await
        .map_err(|e| Error::Memory(format!("list_filtered task failed: {e}")))?
    }

    /// Override the default `strongest` with a single-query implementation
    /// that ranks by decayed strength (time-since-last-access, same Ebbinghaus
    /// decay as `recall`'s scoring). Loads all rows matching the requested
    /// tiers (a project store is small — hundreds to low-thousands), scores
    /// in Rust, and truncates to `limit`. Working-tier is never included even
    /// if passed (it is consolidation input, not primer material).
    async fn strongest(&self, tiers: &[MemoryTier], limit: usize) -> Result<Vec<Memory>> {
        if tiers.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        // Filter out working-tier (never primer material). Duplicates in the
        // IN clause are harmless, so no dedup needed (and MemoryTier is not Ord).
        let wanted: Vec<MemoryTier> = tiers
            .iter()
            .copied()
            .filter(|t| *t != MemoryTier::Working)
            .collect();
        if wanted.is_empty() {
            return Ok(Vec::new());
        }
        let read_conn = self.read_conn().clone();
        let mut memories: Vec<Memory> = tokio::task::spawn_blocking(move || -> Result<Vec<Memory>> {
            let conn = read_conn.lock().expect("read conn lock poisoned");
            // Build `WHERE tier IN (?, ?, ...)` with one placeholder per tier.
            // Superseded rows are excluded at SQL level — the primer is the
            // standing context of every new session, and a superseded record
            // must never surface there at full strength (the same invariant
            // recall/list_filtered honor; the primer path was the gap).
            let placeholders: Vec<&str> = (0..wanted.len()).map(|_| "?").collect();
            let sql = format!(
                "SELECT id, tier, title, content, data, strength, access_count, created_at, \
                 last_accessed_at, source_session_ids, embedding, record_class, record_type, superseded_by FROM memories \
                 WHERE tier IN ({}) AND superseded_by IS NULL",
                placeholders.join(", ")
            );
            let mut stmt = conn.prepare(&sql)?;
            let params_vec: Vec<Box<dyn rusqlite::ToSql>> =
                wanted.iter().map(|t| Box::new(t.as_str().to_string()) as Box<dyn rusqlite::ToSql>).collect();
            let mut rows = stmt.query(rusqlite::params_from_iter(params_vec.into_iter()))?;
            let mut memories = Vec::new();
            while let Some(row) = rows.next()? {
                memories.push(Self::parse_memory_row(row)?);
            }
            Ok(memories)
        })
        .await
        .map_err(|e| Error::Memory(format!("strongest load task failed: {e}")))??;
        // The read connection is released above before scoring (no further DB
        // access) — the sort + truncate below is pure Rust.

        let now = self.now();
        // The same configurable decay tau as recall (the live [memory]
        // knobs) so the primer ranks consistently with query-driven recall.
        let decay_tau_secs = self.memory_search_config().decay_half_life_days * 86_400.0;
        // Rank by decayed strength desc, with a deterministic title tiebreaker
        // (stable across restarts — DB row order is not guaranteed).
        memories.sort_by(|a, b| {
            let da = strength::decay_with_tau(a.strength, now - a.last_accessed_at, decay_tau_secs);
            let db = strength::decay_with_tau(b.strength, now - b.last_accessed_at, decay_tau_secs);
            db.partial_cmp(&da)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.title.cmp(&b.title))
        });
        memories.truncate(limit);
        self.log_access("read", None, "session primer", Some(memories.len()));
        Ok(memories)
    }

    async fn delete_working_for_session(&self, session_id: &str) -> Result<usize> {
        let conn = self.conn.clone();
        let session_id = session_id.to_string();
        tokio::task::spawn_blocking(move || -> Result<usize> {
            let conn = conn.lock().expect("conn lock poisoned");
            // source_session_ids is a JSON array (e.g. ["sess-1"]). Use JSON1's
            // json_each to match rows whose array contains session_id, restricted
            // to the working tier. The FTS delete trigger fires automatically.
            let deleted = conn.execute(
                "DELETE FROM memories
                 WHERE tier = 'working'
                   AND EXISTS (
                       SELECT 1 FROM json_each(memories.source_session_ids)
                       WHERE json_each.value = ?1
                   )",
                params![session_id],
            )?;
            Ok(deleted)
        })
        .await
        .map_err(|e| Error::Memory(format!("delete_working_for_session task failed: {e}")))?
    }

    async fn stored_model_fingerprints(&self) -> Result<Vec<(String, usize)>> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<(String, usize)>> {
            let conn = conn.lock().expect("conn lock poisoned");
            let mut stmt = conn.prepare(
                "SELECT embed_model, embed_dim FROM memories \
                 WHERE embedding IS NOT NULL GROUP BY embed_model, embed_dim",
            )?;
            let mut rows = stmt.query([])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                let model: Option<String> = row.get(0)?;
                let dim: Option<i64> = row.get(1)?;
                // NULL model (legacy rows) → empty string; NULL dim → 0.
                out.push((model.unwrap_or_default(), dim.unwrap_or(0) as usize));
            }
            Ok(out)
        })
        .await
        .map_err(|e| Error::Memory(format!("stored_model_fingerprints task failed: {e}")))?
    }

    async fn reembed_all(
        &self,
        embedder: Arc<dyn Embedder>,
        progress: Option<&(dyn Fn(usize, usize) + Send + Sync)>,
    ) -> Result<usize> {
        // Load all (id, content) pairs on the blocking pool, then re-embed each
        // (the embed call itself is async + may be CPU-bound for a bundled
        // model, so it runs on the async runtime, not the blocking lock holder).
        let conn = self.conn.clone();
        let rows: Vec<(String, String)> =
            tokio::task::spawn_blocking(move || -> Result<Vec<(String, String)>> {
                let conn = conn.lock().expect("conn lock poisoned");
                let mut stmt = conn.prepare("SELECT id, content FROM memories")?;
                let mut rows = stmt.query([])?;
                let mut out = Vec::new();
                while let Some(row) = rows.next()? {
                    out.push((row.get::<_, String>(0)?, row.get::<_, String>(1)?));
                }
                Ok(out)
            })
            .await
            .map_err(|e| Error::Memory(format!("reembed_all load task failed: {e}")))??;

        let model_id = embedder.model_id().to_string();
        let dim = embedder.dim() as i64;
        let total = rows.len();
        let mut count = 0usize;
        for (id, content) in rows {
            let vec = embedder.embed(&content).await;
            let blob = Self::encode_embedding(&vec);
            let conn = self.conn.clone();
            let model_id = model_id.clone();
            tokio::task::spawn_blocking(move || -> Result<()> {
                let conn = conn.lock().expect("conn lock poisoned");
                conn.execute(
                    "UPDATE memories SET embedding = ?1, embed_model = ?2, embed_dim = ?3 WHERE id = ?4",
                    params![blob, model_id, dim, id],
                )?;
                Ok(())
            })
            .await
            .map_err(|e| Error::Memory(format!("reembed_all update task failed: {e}")))??;
            count += 1;
            if let Some(report) = progress {
                report(count, total);
            }
        }
        Ok(count)
    }

    async fn start_session(&self, project: &str) -> Result<Session> {
        let session = Session::new(project, self.now());
        let conn = self.conn.clone();
        let (id, project, created_at) = (
            session.id.clone(),
            session.project.clone(),
            session.created_at,
        );
        tokio::task::spawn_blocking(move || -> Result<()> {
            let conn = conn.lock().expect("conn lock poisoned");
            conn.execute(
                "INSERT INTO sessions (id, project, created_at) VALUES (?1, ?2, ?3)",
                params![id, project, created_at],
            )?;
            Ok(())
        })
        .await
        .map_err(|e| Error::Memory(format!("start_session task failed: {e}")))??;
        Ok(session)
    }

    async fn end_session(&self, id: &str) -> Result<()> {
        let conn = self.conn.clone();
        let now = self.now();
        let id = id.to_string();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let conn = conn.lock().expect("conn lock poisoned");
            conn.execute(
                "UPDATE sessions SET ended_at = ?1 WHERE id = ?2",
                params![now, id],
            )?;
            Ok(())
        })
        .await
        .map_err(|e| Error::Memory(format!("end_session task failed: {e}")))?
    }

    async fn record_tool_event(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        tool_input: serde_json::Value,
        tool_output: &str,
        error: Option<&str>,
    ) -> Result<String> {
        let now = self.now();
        // Cap the stored output so huge search/git results don't bloat the
        // FTS index + memory.db. The full output already lives in the
        // conversation history; recall only needs a snippet to match on.
        // Char-boundary safe (never splits a multi-byte char).
        let capped_output: String = tool_output
            .chars()
            .take(MAX_WORKING_CONTENT_CHARS)
            .collect();
        // Likewise cap any string-valued fields in tool_input (e.g. a
        // file_write's `content` argument can be large). The FTS index +
        // embedding derive from `capped_output` (memory.content), so they are
        // already protected; this keeps the `data` JSON blob from growing
        // unboundedly too.
        let capped_input = cap_json_strings(tool_input, MAX_WORKING_CONTENT_CHARS);
        let data = MemoryData::Working(WorkingData {
            tool_name: tool_name.to_string(),
            tool_input: capped_input,
            tool_output: capped_output.clone(),
            error: error.map(|s| s.to_string()),
        });
        let mut memory = Memory::new(
            MemoryTier::Working,
            format!("tool: {tool_name}"),
            capped_output,
            now,
        );
        memory.data = serde_json::to_value(&data).unwrap_or(serde_json::Value::Null);
        if let Some(sid) = session_id {
            memory.source_session_ids = vec![sid.to_string()];
        }
        self.write(memory).await
    }

    async fn record_request_stats(&self, stats: &RequestStats) -> Result<()> {
        let conn = self.conn.clone();
        let stats = stats.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let conn = conn.lock().expect("conn lock poisoned");
            conn.execute(
                "INSERT INTO request_stats \
                 (id, session_id, model, endpoint, prompt_tokens, completion_tokens, reasoning_tokens, \
                  cached_tokens, ttft_ms, generation_ms, created_at, outcome, purpose) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    stats.id,
                    stats.session_id,
                    stats.model,
                    stats.endpoint,
                    stats.prompt_tokens,
                    stats.completion_tokens,
                    stats.reasoning_tokens,
                    stats.cached_tokens,
                    stats.ttft_ms.map(|v| v as i64),
                    stats.generation_ms.map(|v| v as i64),
                    stats.created_at,
                    stats.outcome,
                    stats.purpose,
                ],
            )?;
            Ok(())
        })
        .await
        .map_err(|e| Error::Memory(format!("record_request_stats task failed: {e}")))?
    }

    async fn request_stats_rows(&self, session_id: &str) -> Result<Vec<RequestStats>> {
        let read_conn = self.read_conn().clone();
        let session_id = session_id.to_string();
        tokio::task::spawn_blocking(move || -> Result<Vec<RequestStats>> {
            let conn = read_conn.lock().expect("conn lock poisoned");
            let mut stmt = conn.prepare(
                "SELECT id, session_id, model, endpoint, prompt_tokens, completion_tokens, \
                 reasoning_tokens, cached_tokens, ttft_ms, generation_ms, created_at, outcome, purpose \
                 FROM request_stats WHERE session_id = ?1 ORDER BY created_at, id",
            )?;
            let rows = stmt.query_map(params![session_id], |row| {
                Ok(RequestStats {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    model: row.get(2)?,
                    endpoint: row.get(3)?,
                    prompt_tokens: row.get::<_, i64>(4)? as u32,
                    completion_tokens: row.get::<_, i64>(5)? as u32,
                    reasoning_tokens: row.get::<_, i64>(6)? as u32,
                    cached_tokens: row.get::<_, Option<i64>>(7)?.map(|v| v as u32),
                    ttft_ms: row.get::<_, Option<i64>>(8)?.map(|v| v as u32),
                    generation_ms: row.get::<_, Option<i64>>(9)?.map(|v| v as u32),
                    created_at: row.get(10)?,
                    outcome: row.get(11)?,
                    purpose: row.get(12)?,
                })
            })?;
            let mut out = Vec::new();
            for row in rows {
                out.push(
                    row.map_err(|e| Error::Memory(format!("request_stats_rows read failed: {e}")))?,
                );
            }
            Ok(out)
        })
        .await
        .map_err(|e| Error::Memory(format!("request_stats_rows task failed: {e}")))?
    }

    async fn session_stats(&self, session_id: &str) -> Result<SessionStats> {
        let read_conn = self.read_conn().clone();
        let session_id = session_id.to_string();
        tokio::task::spawn_blocking(move || -> Result<SessionStats> {
            let conn = read_conn.lock().expect("read conn lock poisoned");
            // Session metadata.
            let (created_at, ended_at): (i64, Option<i64>) = conn.query_row(
                "SELECT created_at, ended_at FROM sessions WHERE id = ?1",
                params![session_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let mut stats = SessionStats {
                session_id,
                created_at,
                ended_at,
                ..Default::default()
            };
            // Per-model × endpoint breakdown (also accumulates the session
            // totals). SQLite groups NULLs together, so pre-endpoint rows
            // (and unnamed test mocks) form one row per model — exactly the
            // pre-dimension view.
            let mut stmt = conn.prepare(
                "SELECT model, endpoint, \
                        SUM(prompt_tokens), SUM(completion_tokens), SUM(reasoning_tokens), \
                        SUM(cached_tokens), \
                        SUM(CASE WHEN ttft_ms IS NOT NULL THEN ttft_ms ELSE 0 END), \
                        SUM(CASE WHEN generation_ms IS NOT NULL THEN generation_ms ELSE 0 END), \
                        SUM(CASE WHEN ttft_ms IS NOT NULL THEN 1 ELSE 0 END), \
                        COUNT(*) \
                 FROM request_stats WHERE session_id = ?1 GROUP BY model, endpoint",
            )?;
            let rows = stmt.query_map(params![stats.session_id], |row| {
                Ok(ModelBreakdown {
                    model: row.get(0)?,
                    endpoint: row.get(1)?,
                    prompt_tokens: row.get::<_, Option<i64>>(2)?.unwrap_or(0) as u64,
                    completion_tokens: row.get::<_, Option<i64>>(3)?.unwrap_or(0) as u64,
                    reasoning_tokens: row.get::<_, Option<i64>>(4)?.unwrap_or(0) as u64,
                    cached_tokens: row.get::<_, Option<i64>>(5)?.unwrap_or(0) as u64,
                    ttft_ms_total: row.get::<_, Option<i64>>(6)?.unwrap_or(0) as u64,
                    generation_ms_total: row.get::<_, Option<i64>>(7)?.unwrap_or(0) as u64,
                    timed_requests: row.get::<_, Option<i64>>(8)?.unwrap_or(0) as u64,
                    request_count: row.get::<_, Option<i64>>(9)?.unwrap_or(0) as u64,
                })
            })?;
            for row in rows {
                let mb = row?;
                stats.prompt_tokens += mb.prompt_tokens;
                stats.completion_tokens += mb.completion_tokens;
                stats.reasoning_tokens += mb.reasoning_tokens;
                stats.cached_tokens += mb.cached_tokens;
                stats.ttft_ms_total += mb.ttft_ms_total;
                stats.generation_ms_total += mb.generation_ms_total;
                stats.timed_requests += mb.timed_requests;
                stats.request_count += mb.request_count;
                stats.per_model.push(mb);
            }
            Ok(stats)
        })
        .await
        .map_err(|e| Error::Memory(format!("session_stats task failed: {e}")))?
    }

    async fn project_stats(&self) -> Result<ProjectStats> {
        let read_conn = self.read_conn().clone();
        tokio::task::spawn_blocking(move || -> Result<ProjectStats> {
            let conn = read_conn.lock().expect("read conn lock poisoned");
            let mut stats = ProjectStats::default();
            // Session count.
            stats.session_count = conn.query_row("SELECT COUNT(*) FROM sessions", [], |row| {
                row.get::<_, i64>(0).map(|n| n as u64)
            })?;
            // Per-model × endpoint breakdown (accumulates totals). NULL
            // endpoints group together — the pre-dimension per-model view.
            let mut stmt = conn.prepare(
                "SELECT model, endpoint, \
                        SUM(prompt_tokens), SUM(completion_tokens), SUM(reasoning_tokens), \
                        SUM(cached_tokens), \
                        SUM(CASE WHEN ttft_ms IS NOT NULL THEN ttft_ms ELSE 0 END), \
                        SUM(CASE WHEN generation_ms IS NOT NULL THEN generation_ms ELSE 0 END), \
                        SUM(CASE WHEN ttft_ms IS NOT NULL THEN 1 ELSE 0 END), \
                        COUNT(*) \
                 FROM request_stats GROUP BY model, endpoint",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(ModelBreakdown {
                    model: row.get(0)?,
                    endpoint: row.get(1)?,
                    prompt_tokens: row.get::<_, Option<i64>>(2)?.unwrap_or(0) as u64,
                    completion_tokens: row.get::<_, Option<i64>>(3)?.unwrap_or(0) as u64,
                    reasoning_tokens: row.get::<_, Option<i64>>(4)?.unwrap_or(0) as u64,
                    cached_tokens: row.get::<_, Option<i64>>(5)?.unwrap_or(0) as u64,
                    ttft_ms_total: row.get::<_, Option<i64>>(6)?.unwrap_or(0) as u64,
                    generation_ms_total: row.get::<_, Option<i64>>(7)?.unwrap_or(0) as u64,
                    timed_requests: row.get::<_, Option<i64>>(8)?.unwrap_or(0) as u64,
                    request_count: row.get::<_, Option<i64>>(9)?.unwrap_or(0) as u64,
                })
            })?;
            for row in rows {
                let mb = row?;
                stats.prompt_tokens += mb.prompt_tokens;
                stats.completion_tokens += mb.completion_tokens;
                stats.reasoning_tokens += mb.reasoning_tokens;
                stats.cached_tokens += mb.cached_tokens;
                stats.ttft_ms_total += mb.ttft_ms_total;
                stats.generation_ms_total += mb.generation_ms_total;
                stats.timed_requests += mb.timed_requests;
                stats.request_count += mb.request_count;
                stats.per_model.push(mb);
            }
            // Per-day breakdown. `created_at` is in seconds; bucket by day
            // (86400s). Truncating to the day start via integer division.
            let mut stmt2 = conn.prepare(
                "SELECT (created_at / 86400) * 86400 AS day, \
                        SUM(prompt_tokens), SUM(completion_tokens), SUM(reasoning_tokens), \
                        SUM(cached_tokens), COUNT(*) \
                 FROM request_stats GROUP BY day ORDER BY day",
            )?;
            let day_rows = stmt2.query_map([], |row| {
                Ok(DayBreakdown {
                    day: row.get::<_, i64>(0)?,
                    prompt_tokens: row.get::<_, Option<i64>>(1)?.unwrap_or(0) as u64,
                    completion_tokens: row.get::<_, Option<i64>>(2)?.unwrap_or(0) as u64,
                    reasoning_tokens: row.get::<_, Option<i64>>(3)?.unwrap_or(0) as u64,
                    cached_tokens: row.get::<_, Option<i64>>(4)?.unwrap_or(0) as u64,
                    request_count: row.get::<_, Option<i64>>(5)?.unwrap_or(0) as u64,
                })
            })?;
            for row in day_rows {
                stats.per_day.push(row?);
            }
            Ok(stats)
        })
        .await
        .map_err(|e| Error::Memory(format!("project_stats task failed: {e}")))?
    }

    async fn session_list(&self) -> Result<Vec<SessionSummary>> {
        let read_conn = self.read_conn().clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<SessionSummary>> {
            let conn = read_conn.lock().expect("read conn lock poisoned");
            let mut stmt = conn.prepare(
                "SELECT s.id, s.created_at, s.ended_at, \
                        COUNT(r.id), \
                        COALESCE(SUM(r.prompt_tokens), 0), \
                        COALESCE(SUM(r.completion_tokens), 0), \
                        COALESCE(SUM(r.reasoning_tokens), 0) \
                 FROM sessions s \
                 LEFT JOIN request_stats r ON r.session_id = s.id \
                 GROUP BY s.id \
                 ORDER BY s.created_at DESC",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(SessionSummary {
                    session_id: row.get(0)?,
                    created_at: row.get(1)?,
                    ended_at: row.get::<_, Option<i64>>(2)?,
                    request_count: row.get::<_, i64>(3)? as u64,
                    prompt_tokens: row.get::<_, i64>(4)? as u64,
                    completion_tokens: row.get::<_, i64>(5)? as u64,
                    reasoning_tokens: row.get::<_, i64>(6)? as u64,
                })
            })?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
        .await
        .map_err(|e| Error::Memory(format!("session_list task failed: {e}")))?
    }
}

#[cfg(test)]
mod tests;
