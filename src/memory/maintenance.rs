// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Manual memory maintenance — the engine behind Settings → Memory.
//!
//! Two operations, both user-triggered from the Settings dialog and run as
//! background tasks by the IPC layer (which streams progress events to the
//! UI):
//!
//! - [`cleanup`]: consolidate every non-live session that still holds raw
//!   working-tier events into an episodic summary (plus distilled
//!   semantic/procedural memories when an LLM is available), deleting the raw
//!   rows, then VACUUM the database. Sessions run with bounded concurrency
//!   (the ~3 LLM calls per session overlap instead of serializing). Sessions
//!   in the skip set — the live agents' sessions — are left untouched: their
//!   working memory is still being written and is consolidated at their own
//!   session end.
//! - [`rebuild_search`]: re-embed every memory with the store's *active*
//!   embedder, rebuild the FTS5 full-text index, and VACUUM — the recovery
//!   path when stored vectors are stale or mixed (an interrupted model
//!   switch, or a memory.db that traveled from another machine).
//!
//! Both report progress as `(phase, done, total)` via a callback so callers
//! can drive a progress bar; see [`MaintenancePhase`].

use std::sync::Arc;

use futures::{StreamExt, TryStreamExt};

use crate::error::Result;
use crate::memory::consolidation;
use crate::memory::{Memory, MemoryStore, MemoryStoreTrait, MemoryTier};
use crate::provider::LlmClient;

/// How many sessions consolidate in parallel during [`cleanup`].
///
/// Bounded (not unbounded) so a large backlog doesn't hammer the provider
/// with dozens of simultaneous requests or exhaust the connection pool.
/// Sessions are independent — each working row belongs to exactly one
/// session, the store serializes SQLite writes on its single write
/// connection, and the LLM client is `Send + Sync` with no shared
/// serialization — so this only multiplies the throughput of the ~3 LLM
/// calls per session. 4 concurrent sessions ≈ 12 in-flight LLM calls,
/// comfortably within typical provider concurrency limits.
const CLEANUP_CONCURRENCY: usize = 4;

/// What [`cleanup`] did — surfaced by the UI as the completion summary.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CleanupReport {
    /// Sessions that had working-tier events compressed into an episodic
    /// summary.
    pub sessions_consolidated: usize,
    /// Raw working-tier rows removed. Their signal lives on in the episodic
    /// summaries (and any extracted semantic/procedural memories).
    pub working_rows_removed: usize,
}

/// What [`rebuild_search`] did — surfaced by the UI as the completion summary.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RebuildReport {
    /// Memories re-embedded with the active embedder.
    pub memories_reembedded: usize,
}

/// The stage a progress tick belongs to (drives the progress bar's label).
/// Tail phases that have no measurable units (`Reindexing`, `Vacuuming`)
/// tick with `(0, 0)` — a label-only update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaintenancePhase {
    /// Consolidating a session into an episodic summary (cleanup).
    Consolidating,
    /// Re-embedding memories (rebuild).
    Embedding,
    /// Rebuilding the FTS5 full-text index (rebuild).
    Reindexing,
    /// Compacting the database with VACUUM (both ops, at the end).
    Vacuuming,
}

/// Consolidate every session that still owns working-tier events, skipping
/// `skip_sessions` (live agents' sessions), then VACUUM the store.
///
/// `provider` is optional: with an LLM the pipeline distills semantic facts +
/// procedural workflows after the episodic summary; without one it still
/// compresses the raw events into a synthetic episodic summary and deletes
/// them. `corpus` is a [`consolidation::corpus_digest`] digest of the
/// project's plans + reviews (may be empty). Sessions consolidate with
/// bounded concurrency ([`CLEANUP_CONCURRENCY`]) — each session's LLM calls
/// overlap, so a large backlog finishes in wall time ~ total / concurrency
/// instead of serializing (101 sessions × 3 calls = hours down to minutes).
/// SQLite writes stay safe: the store serializes them on a single write
/// connection, and each working row is tagged with exactly one session, so
/// concurrent consolidations never contend on the same rows. `progress` ticks
/// `(Consolidating, done, total)` after each *finished* session (done counts
/// completions, so ticks stay monotone but their order across sessions is
/// nondeterministic) and `(Vacuuming, 0, 0)` once before the final VACUUM. A
/// no-op (empty report, no vacuum) when no non-skipped session has working
/// rows.
///
/// Note: `record_tool_event` tags each working row with exactly one session,
/// so per-session consolidation never touches another session's rows in
/// practice. Rows with NO session tag (written before a session started,
/// via `record_tool_event(None, …)`) belong to no session and are left
/// untouched — cleanup never consolidates or removes them.
pub async fn cleanup(
    store: &Arc<MemoryStore>,
    skip_sessions: &[String],
    provider: Option<&dyn LlmClient>,
    corpus: &str,
    progress: &(dyn Fn(MaintenancePhase, usize, usize) + Send + Sync),
) -> Result<CleanupReport> {
    // Snapshot the working tier once and derive the pending session set
    // (deterministic BTreeSet order so the per-session grouping below is
    // stable). Hash-set membership: the skip filter runs once per (row ×
    // session-id) and the ownership filter once per row — O(1) lookups
    // instead of Vec scans as the backlog grows (review perf note).
    let working = store.list_by_tier(MemoryTier::Working).await?;
    let skip: std::collections::HashSet<&str> = skip_sessions.iter().map(String::as_str).collect();
    let sessions: Vec<String> = working
        .iter()
        .flat_map(|m| m.source_session_ids.iter().cloned())
        .filter(|s| !skip.contains(s.as_str()))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let total = sessions.len();
    if total == 0 {
        return Ok(CleanupReport::default());
    }
    // Distinct rows owned by any pending session — each is deleted exactly
    // once (under the first of its owning sessions that gets consolidated;
    // a row tagged with multiple pending sessions is claimed by the first in
    // sorted order, mirroring the sequential loop's behavior).
    let pending: std::collections::HashSet<&str> = sessions.iter().map(String::as_str).collect();
    let working_rows_removed = working
        .iter()
        .filter(|m| {
            m.source_session_ids
                .iter()
                .any(|s| pending.contains(s.as_str()))
        })
        .count();
    // Group the snapshot into per-session event lists, claiming each row for
    // exactly one session (its FIRST pending owner in sorted order) so the
    // deletion-under-the-first-owner semantics hold exactly as before.
    let mut per_session: std::collections::HashMap<&str, Vec<&Memory>> =
        sessions.iter().map(|s| (s.as_str(), Vec::new())).collect();
    for m in &working {
        if let Some(owner) = m
            .source_session_ids
            .iter()
            .filter(|s| pending.contains(s.as_str()))
            .min()
        {
            per_session
                .get_mut(owner.as_str())
                .expect("owner in map")
                .push(m);
        }
    }

    // Run consolidations with bounded concurrency. Sessions are independent
    // (each row belongs to one session; store writes serialize on the single
    // SQLite write connection; the LLM client is Send+Sync), so overlapping
    // them only multiplies the throughput of the ~3 LLM calls per session.
    // An error still aborts the whole cleanup: `try_collect` stops on the
    // first error. Unlike the former sequential loop (which aborted before
    // starting the next session), this also CANCELS the sibling sessions
    // in flight at that moment — a sibling dropped mid-pipeline can be left
    // with its episodic summary written but its working rows intact. That
    // is safe: each SQLite op is atomic, and the next cleanup run simply
    // re-consolidates that session (writing a fresh episodic summary for
    // it). Propagating errors are rare — consolidation swallows
    // extract/delete failures, so only list/write failures abort.
    let completed: Arc<std::sync::atomic::AtomicUsize> = Default::default();
    let episodic_ids = futures::stream::iter(sessions.iter().cloned())
        .map(|sid| {
            let store = store.clone();
            let completed = completed.clone();
            let events = per_session.remove(sid.as_str()).unwrap_or_default();
            async move {
                let episodic_id = consolidation::consolidate_session_with_events(
                    store.as_ref(),
                    &sid,
                    &events,
                    provider,
                    corpus,
                )
                .await?;
                let n = completed.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                progress(MaintenancePhase::Consolidating, n, total);
                Ok::<_, crate::error::Error>(episodic_id)
            }
        })
        .buffer_unordered(CLEANUP_CONCURRENCY)
        .try_collect::<Vec<_>>()
        .await?;
    let sessions_consolidated = episodic_ids.into_iter().filter(|id| !id.is_empty()).count();
    progress(MaintenancePhase::Vacuuming, 0, 0);
    store.vacuum().await?;
    Ok(CleanupReport {
        sessions_consolidated,
        working_rows_removed,
    })
}

/// Re-embed every memory with the store's active embedder, rebuild the FTS5
/// index, and VACUUM.
///
/// The recovery path for degraded semantic search: mixed or stale embedding
/// fingerprints (interrupted model switch, cross-machine DB) or a drifted
/// full-text index. `progress` ticks `(Embedding, done, total)` after each
/// re-embedded row, then label-only `(Reindexing, 0, 0)` / `(Vacuuming, 0, 0)`
/// ticks before the index rebuild + VACUUM.
pub async fn rebuild_search(
    store: &Arc<MemoryStore>,
    progress: &(dyn Fn(MaintenancePhase, usize, usize) + Send + Sync),
) -> Result<RebuildReport> {
    let embedder = store.embedder_handle();
    let embed_tick = |done: usize, total: usize| progress(MaintenancePhase::Embedding, done, total);
    let memories_reembedded = store.reembed_all(embedder, Some(&embed_tick)).await?;
    progress(MaintenancePhase::Reindexing, 0, 0);
    store.rebuild_fts().await?;
    progress(MaintenancePhase::Vacuuming, 0, 0);
    store.vacuum().await?;
    Ok(RebuildReport {
        memories_reembedded,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::embedder::HashEmbedder;
    use crate::memory::{Memory, MemoryFilter};
    use crate::provider::{
        Capabilities, FinishReason, LlmEvent, Message, ProviderKind, ToolChoice, ToolSchema,
    };
    use async_trait::async_trait;
    use futures::stream::BoxStream;
    use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

    fn make_store() -> MemoryStore {
        let embedder: Arc<dyn crate::memory::Embedder> = Arc::new(HashEmbedder::new());
        let now_val = Arc::new(AtomicI64::new(1000));
        MemoryStore::open_in_memory_with_clock(embedder, move || {
            now_val.fetch_add(1, Ordering::SeqCst) + 1
        })
        .unwrap()
    }

    #[tokio::test]
    async fn cleanup_consolidates_stale_sessions_and_skips_live_ones() {
        let store = Arc::new(make_store());
        for _ in 0..2 {
            store
                .record_tool_event(
                    Some("sess-a"),
                    "file_read",
                    serde_json::json!({"path": "a.rs"}),
                    "a",
                    None,
                )
                .await
                .unwrap();
        }
        store
            .record_tool_event(
                Some("sess-b"),
                "file_write",
                serde_json::json!({"path": "b.rs"}),
                "b",
                None,
            )
            .await
            .unwrap();
        // The live session's working row must survive the cleanup.
        store
            .record_tool_event(
                Some("sess-live"),
                "file_read",
                serde_json::json!({"path": "c.rs"}),
                "c",
                None,
            )
            .await
            .unwrap();

        let seen: Arc<std::sync::Mutex<Vec<(MaintenancePhase, usize, usize)>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = seen.clone();
        let report = cleanup(
            &store,
            &["sess-live".to_string()],
            None,
            "",
            &move |phase, done, total| sink.lock().unwrap().push((phase, done, total)),
        )
        .await
        .unwrap();

        assert_eq!(
            report.sessions_consolidated, 2,
            "sess-a + sess-b consolidated"
        );
        assert_eq!(
            report.working_rows_removed, 3,
            "two sess-a rows + one sess-b row"
        );
        // Only the live session's row remains in the working tier.
        let working = store.list_by_tier(MemoryTier::Working).await.unwrap();
        assert_eq!(working.len(), 1);
        assert_eq!(working[0].source_session_ids, vec!["sess-live".to_string()]);
        // Each stale session left an episodic summary behind.
        let episodic = store.list_by_tier(MemoryTier::Episodic).await.unwrap();
        assert_eq!(episodic.len(), 2);
        // Progress: one tick per session, ending complete, then the vacuum
        // tail tick.
        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                (MaintenancePhase::Consolidating, 1, 2),
                (MaintenancePhase::Consolidating, 2, 2),
                (MaintenancePhase::Vacuuming, 0, 0),
            ]
        );
    }

    #[tokio::test]
    async fn cleanup_without_working_rows_is_a_noop() {
        let store = Arc::new(make_store());
        store
            .write(Memory::new(MemoryTier::Semantic, "fact", "jwt auth", 1000))
            .await
            .unwrap();
        let report = cleanup(&store, &[], None, "", &|_phase, _done, _total| {})
            .await
            .unwrap();
        assert_eq!(report, CleanupReport::default());
        // The semantic memory is untouched.
        let semantic = store.list_by_tier(MemoryTier::Semantic).await.unwrap();
        assert_eq!(semantic.len(), 1);
    }

    /// A minimal second embedder with a distinct model_id + dim, so the test
    /// can start from a fingerprint mismatch (hash/768) and verify the
    /// rebuild unifies to the active model.
    struct TinyEmbedder;

    #[async_trait]
    impl crate::memory::Embedder for TinyEmbedder {
        async fn embed(&self, _text: &str) -> Vec<f32> {
            vec![0.5; 4]
        }

        fn dim(&self) -> usize {
            4
        }

        fn model_id(&self) -> &str {
            "tiny-test"
        }
    }

    #[tokio::test]
    async fn rebuild_search_unifies_fingerprints_and_keeps_recall_working() {
        let store = Arc::new(make_store());
        store
            .write(Memory::new(
                MemoryTier::Semantic,
                "jwt",
                "authentication uses jwt tokens",
                1000,
            ))
            .await
            .unwrap();
        store
            .write(Memory::new(
                MemoryTier::Semantic,
                "routes",
                "the routes file defines login",
                1000,
            ))
            .await
            .unwrap();
        // Sanity: rows are hashed under the store's default embedder.
        let fps = store.stored_model_fingerprints().await.unwrap();
        assert_eq!(fps, vec![("hash".to_string(), 768)]);

        // Swap to a different embedder and rebuild.
        store.set_embedder(Arc::new(TinyEmbedder));
        let seen: Arc<std::sync::Mutex<Vec<(MaintenancePhase, usize, usize)>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = seen.clone();
        let report = rebuild_search(&store, &move |phase, done, total| {
            sink.lock().unwrap().push((phase, done, total))
        })
        .await
        .unwrap();

        assert_eq!(report.memories_reembedded, 2);
        // Fingerprints now uniformly match the active embedder…
        let fps = store.stored_model_fingerprints().await.unwrap();
        assert_eq!(fps, vec![("tiny-test".to_string(), 4)]);
        // …and FTS recall still finds rows through the rebuilt index.
        let hits = store.recall("jwt", &MemoryFilter::new()).await.unwrap();
        assert_eq!(hits.len(), 1, "the jwt memory is still findable");
        // Progress: one embedding tick per row, then the label-only tail.
        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                (MaintenancePhase::Embedding, 1, 2),
                (MaintenancePhase::Embedding, 2, 2),
                (MaintenancePhase::Reindexing, 0, 0),
                (MaintenancePhase::Vacuuming, 0, 0),
            ]
        );
    }

    #[tokio::test]
    async fn vacuum_and_fts_rebuild_succeed_on_a_fresh_store() {
        let store = make_store();
        store.vacuum().await.unwrap();
        store.rebuild_fts().await.unwrap();
    }

    /// A mock LLM whose `complete` calls overlap: it bumps a shared
    /// in-flight counter at entry, tracks the concurrent max, sleeps ~50ms
    /// (long enough for the cleanup concurrency to overlap several calls on
    /// a multi-worker tokio runtime), and decrements at exit. Every response
    /// is the same canned synthesis JSON — the semantic/procedural
    /// extraction degrades to `[]` safely, so a session still consolidates.
    struct SlowMockLlm {
        in_flight: Arc<AtomicUsize>,
        max_in_flight: Arc<AtomicUsize>,
        calls: Arc<AtomicUsize>,
        caps: Capabilities,
    }

    #[async_trait]
    impl crate::provider::LlmClient for SlowMockLlm {
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
        fn kind(&self) -> ProviderKind {
            ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            "slow-mock"
        }
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _tool_choice: Option<ToolChoice>,
        ) -> Result<BoxStream<'_, LlmEvent>> {
            let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_in_flight.fetch_max(now, Ordering::SeqCst);
            self.calls.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            let text = serde_json::json!({
                "narrative": "session consolidated",
                "key_decisions": [],
                "files_modified": [],
                "concepts": []
            })
            .to_string();
            let events = vec![
                LlmEvent::TextDelta { text },
                LlmEvent::Finish {
                    reason: FinishReason::Stop,
                },
            ];
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn cleanup_runs_sessions_concurrently() {
        // 4 sessions × 3 LLM calls each = 12 calls. With CLEANUP_CONCURRENCY
        // >= 2 they must OVERLAP (max in-flight >= 2), which is what turns
        // the hours-long serial cleanup into minutes.
        let store = Arc::new(make_store());
        for s in ["sess-1", "sess-2", "sess-3", "sess-4"] {
            store
                .record_tool_event(
                    Some(s),
                    "file_read",
                    serde_json::json!({"path": format!("{s}.rs")}),
                    "contents",
                    None,
                )
                .await
                .unwrap();
        }

        let llm = SlowMockLlm {
            in_flight: Arc::new(AtomicUsize::new(0)),
            max_in_flight: Arc::new(AtomicUsize::new(0)),
            calls: Arc::new(AtomicUsize::new(0)),
            caps: Capabilities::openai(),
        };

        let report = cleanup(&store, &[], Some(&llm), "", &|_phase, _done, _total| {})
            .await
            .unwrap();

        assert_eq!(
            report.sessions_consolidated, 4,
            "all four sessions consolidated"
        );
        assert_eq!(report.working_rows_removed, 4);
        assert_eq!(
            llm.calls.load(Ordering::SeqCst),
            12,
            "4 sessions × 3 LLM calls (synthesize + semantic + procedural)"
        );
        assert!(
            llm.max_in_flight.load(Ordering::SeqCst) >= 2,
            "sessions must consolidate concurrently, max in-flight was {}",
            llm.max_in_flight.load(Ordering::SeqCst)
        );
        // All sessions left an episodic summary; working rows are gone.
        let episodic = store.list_by_tier(MemoryTier::Episodic).await.unwrap();
        assert_eq!(episodic.len(), 4);
        let working = store.list_by_tier(MemoryTier::Working).await.unwrap();
        assert!(working.is_empty());
    }
}
