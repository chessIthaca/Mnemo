// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! CodeGraph Tauri commands — the backend of the right-panel "Graph" tab and
//! the Settings → Memory & Search code-index card.
//!
//! Three read-only commands over the shared [`CodeGraph`] handle (the same
//! `Arc` the agent `graph_*` tools use, reached through the factory), plus
//! one fire-and-forget maintenance command:
//!
//! - [`codegraph_status`] — availability + counts + the indexing flag (the
//!   tab polls this while a pass runs).
//! - [`codegraph_refresh`] — triggers a background re-index and returns
//!   immediately (status reflects `indexing: true` right after).
//! - [`codegraph_graph`] — a visualization subgraph (see
//!   [`GraphView::slice`](mnemo::codegraph::query::GraphView::slice)):
//!   top-N by degree unfiltered, or name/kind/focused-neighborhood filters.
//! - [`codegraph_rebuild_index`] — a full re-index with `started` →
//!   `progress` → `done`/`failed` events on `codegraph://maintenance`
//!   (mirroring the memory-maintenance stream), backing the Settings card's
//!   Rebuild button + progress bar.
//!
//! A third emission path lives here too: the [`IndexProgressEvent`] stream on
//! `codegraph://index-progress`, reported by the STARTUP indexing pass
//! (main.rs) and the create-project seed pass (ipc/projects.rs) so the
//! open-project overlay can show a progress bar + "N/M files indexed"
//! counter. Kept separate from `codegraph://maintenance` so the Settings
//! card is unaffected by startup traffic.
//!
//! Store access goes through `spawn_blocking` — the SQLite lock is sync and
//! must never park a tokio worker (the F1 lesson from the 2026-04-19 freeze
//! diagnosis). Every command degrades gracefully when the graph is not wired
//! (config off / DB open failed / brain failed): [`codegraph_status`]
//! reports `available: false`, the others return a plain error string.

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use mnemo::codegraph::extract::Symbol;
use mnemo::codegraph::query::{GraphSliceFilter, GraphView};
use mnemo::codegraph::store::EdgeRow;
use mnemo::codegraph::{CodeGraph, IndexStats};

use crate::ipc::error::IpcError;
use crate::ipc::memory_maintenance::{should_forward_progress, PROGRESS_EVERY};
use crate::ipc::state::IpcState;

/// The Graph tab's status line: is the graph available, how big is it, and
/// is an indexing pass running right now.
#[derive(Debug, Clone, Serialize)]
pub struct CodegraphStatus {
    /// False when codegraph is disabled (`[general] codegraph = false`),
    /// the DB failed to open, or the brain failed to build — the tab shows
    /// an "unavailable" state instead of empty counts.
    pub available: bool,
    /// True while an indexing pass is running (startup or a refresh).
    pub indexing: bool,
    /// Number of indexed files — every searchable file (source + content-only
    /// rows), 0 when unavailable.
    pub files: usize,
    /// Number of stored symbols (0 when unavailable).
    pub symbols: usize,
    /// Number of stored edges (0 when unavailable).
    pub edges: usize,
    /// Unix seconds of the most recent `indexed_at`, if any file is indexed.
    pub last_indexed_at: Option<i64>,
}

impl CodegraphStatus {
    /// The `available: false` shape (counts zeroed, not indexing).
    fn unavailable() -> Self {
        Self {
            available: false,
            indexing: false,
            files: 0,
            symbols: 0,
            edges: 0,
            last_indexed_at: None,
        }
    }

    /// Build the live shape from the graph's stats + indexing flag.
    fn from_parts(stats: mnemo::codegraph::store::GraphStats, indexing: bool) -> Self {
        Self {
            available: true,
            indexing,
            files: stats.files,
            symbols: stats.symbols,
            edges: stats.edges,
            last_indexed_at: stats.last_indexed_at,
        }
    }
}

/// A visualization subgraph: the selected nodes plus every edge whose both
/// endpoints are among them.
#[derive(Debug, Clone, Serialize)]
pub struct CodegraphGraph {
    /// The selected symbols, id-sorted.
    pub nodes: Vec<Symbol>,
    /// Edges between the selected symbols only.
    pub edges: Vec<EdgeRow>,
}

/// The graph handle from IPC state: `None` when the brain failed to build
/// (no factory) or codegraph is disabled/unavailable (no handle on the
/// factory).
fn graph_handle(state: &IpcState) -> Option<std::sync::Arc<CodeGraph>> {
    state.runtime.factory.as_ref()?.codegraph_handle()
}

/// Read the graph's stats on the blocking pool (the SQLite lock is sync).
async fn stats_via_blocking(graph: std::sync::Arc<CodeGraph>) -> Result<CodegraphStatus, IpcError> {
    let indexing = graph.is_indexing();
    let stats = tokio::task::spawn_blocking(move || graph.stats())
        .await
        .map_err(|e| format!("codegraph stats task failed: {e}"))?
        .map_err(IpcError::from)?;
    Ok(CodegraphStatus::from_parts(stats, indexing))
}

/// The Graph tab's status: availability, counts, and the indexing flag.
/// Never errors — an unavailable graph reports `available: false`.
#[tauri::command]
pub async fn codegraph_status(state: State<'_, IpcState>) -> Result<CodegraphStatus, IpcError> {
    let Some(graph) = graph_handle(&state) else {
        return Ok(CodegraphStatus::unavailable());
    };
    Ok(stats_via_blocking(graph).await?)
}

/// Trigger a background re-index of the graph. Idempotent: when a pass is
/// already running, returns the current status without scheduling another
/// (overlapping passes are safe but duplicated work). Otherwise the indexing
/// flag is set SYNCHRONOUSLY before spawning, so the returned status
/// deterministically reports `indexing: true` for the scheduled pass — the
/// Graph tab's poller keys on it. `index()` re-sets the flag on entry and
/// its drop guard clears it on every exit path, so the flag being true while
/// the pass is merely queued is semantically correct. Errors when the graph
/// is unavailable.
#[tauri::command]
pub async fn codegraph_refresh(state: State<'_, IpcState>) -> Result<CodegraphStatus, IpcError> {
    let graph = graph_handle(&state)
        .ok_or_else(|| IpcError::msg("codegraph unavailable (disabled or failed to open)"))?;
    // A pass is already running — don't stack a second one (the store writes
    // would serialize safely, but the parse work would be duplicated).
    if graph.is_indexing() {
        return Ok(stats_via_blocking(graph).await?);
    }
    // Set the flag before spawning so the status we return (and the tab's
    // poller) deterministically sees the scheduled pass (review F1: the old
    // code raced the spawned task's entry).
    graph.set_indexing(true);
    let runner = graph.clone();
    // index() owns the indexing flag (set on entry, cleared by a drop guard
    // on every exit path); the parse itself is CPU + disk bound → blocking
    // pool, mirroring the startup pass in main.rs.
    tauri::async_runtime::spawn(async move {
        let result = tokio::task::spawn_blocking(move || runner.index(None)).await;
        match result {
            Ok(Ok(stats)) => log_refresh(&stats),
            Ok(Err(e)) => eprintln!("codegraph: refresh failed: {e}"),
            Err(e) => eprintln!("codegraph: refresh task panicked: {e}"),
        }
    });
    Ok(stats_via_blocking(graph).await?)
}

/// Log a successful refresh pass (kept out of the command body for clarity).
fn log_refresh(stats: &IndexStats) {
    eprintln!(
        "codegraph: refreshed — {} files scanned, {} re-parsed, {} symbols, {} edges in {}ms",
        stats.files_scanned, stats.files_reindexed, stats.symbols, stats.edges, stats.elapsed_ms
    );
}

/// A visualization subgraph. With no filters: the top
/// [`GRAPH_SLICE_CAP`](mnemo::codegraph::query::GRAPH_SLICE_CAP) nodes
/// by degree (most-connected first). With `name` (case-insensitive
/// substring) and/or `kind` (`"function"`, `"struct"`, …): the matching
/// nodes. With `from_symbol` (+ optional `depth`, default 1): that symbol's
/// both-directions neighborhood — a stale/unknown id yields an EMPTY graph
/// (not an error), so the tab survives a re-index racing a click.
#[tauri::command]
pub async fn codegraph_graph(
    state: State<'_, IpcState>,
    name: Option<String>,
    kind: Option<String>,
    from_symbol: Option<String>,
    depth: Option<usize>,
) -> Result<CodegraphGraph, IpcError> {
    let graph = graph_handle(&state)
        .ok_or_else(|| IpcError::msg("codegraph unavailable (disabled or failed to open)"))?;
    // Snapshot + slice on the blocking pool (sync SQLite lock + O(nodes)
    // selection — never on a tokio worker). The owned filter strings move
    // into the closure (spawn_blocking requires 'static); the borrowed
    // `GraphSliceFilter` is constructed inside.
    let slice = tokio::task::spawn_blocking(move || -> Result<_, String> {
        let filter = GraphSliceFilter {
            name: name.as_deref(),
            kind: kind.as_deref(),
            from_symbol: from_symbol.as_deref(),
            depth,
        };
        let view: GraphView = graph.view().map_err(|e| e.to_string())?;
        Ok(view.slice(&filter))
    })
    .await
    .map_err(|e| format!("codegraph graph task failed: {e}"))??;
    // Unknown `from_symbol` (slice → None) yields an EMPTY graph, not an
    // error — a re-index can retire the clicked node between render and
    // click, and the tab must survive that.
    let slice = slice.unwrap_or_default();
    Ok(CodegraphGraph {
        nodes: slice.nodes,
        edges: slice.edges,
    })
}

// ── Settings → Memory & Search: code-index rebuild ──────────────────────────

/// The Tauri event channel the rebuild task streams progress on.
const REINDEX_CHANNEL: &str = "codegraph://maintenance";

/// An event on the [`REINDEX_CHANNEL`] stream — the same `"type"`-tagged,
/// kebab-case shape as the memory-maintenance stream, minus the `op` router
/// (a single operation lives on this channel). Pinned by
/// `reindex_event_wire_shape`; the frontend folds it with the same
/// `applyMaintenanceEvent` mapping.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ReindexEvent {
    /// The pass started (the bar appears at 0%).
    Started,
    /// A progress tick after a walked file (throttled — first, last, and
    /// every `PROGRESS_EVERY`-th tick forward).
    Progress {
        /// Files processed so far.
        done: usize,
        /// Total source files to walk.
        total: usize,
        /// The single phase label ("indexing") — kept so the frontend's
        /// shared event → progress-bar mapping applies unchanged.
        phase: &'static str,
    },
    /// Terminal success with a human-readable summary.
    Done {
        /// One-line summary for the result line.
        summary: String,
    },
    /// Terminal failure (engine error or a panicked task).
    Failed {
        /// Error text for the result line.
        error: String,
    },
}

/// Best-effort emit (a dead window drops the event; the pass still runs).
fn emit_reindex(app: &AppHandle, event: &ReindexEvent) {
    let _ = app.emit(REINDEX_CHANNEL, event);
}

// ── Indexing-progress overlay stream (project open) ─────────────────────────

/// The Tauri event channel the STARTUP indexing pass (main.rs) and the
/// create-project seed pass (ipc/projects.rs) stream file progress on, so the
/// open-project overlay can show a progress bar + "N/M files indexed"
/// counter. Distinct from [`REINDEX_CHANNEL`] (the Settings card's manual
/// rebuild) so the two UIs never fold each other's traffic.
pub const INDEX_PROGRESS_CHANNEL: &str = "codegraph://index-progress";

/// An event on the [`INDEX_PROGRESS_CHANNEL`] stream — the indexing progress
/// behind the open-project overlay. The same `"type"`-tagged, kebab-case
/// shape as [`ReconcileEvent`] (memory_maintenance.rs), but with NO `phase`
/// field: a code index has exactly one phase. Pinned by
/// `index_progress_event_wire_shape`; the frontend folds it via
/// `applyIndexProgress` in IndexingOverlay.tsx.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum IndexProgressEvent {
    /// The pass started (the overlay appears, counter "…").
    Started,
    /// A progress tick after a walked file (throttled — first, last, and
    /// every `PROGRESS_EVERY`-th tick forward; the startup pass additionally
    /// gates on 1s elapsed so a sub-second pass emits nothing).
    Progress {
        /// Files processed so far.
        done: usize,
        /// Total source files to walk.
        total: usize,
    },
    /// Terminal success with a human-readable summary.
    Done {
        /// One-line summary for the overlay's result line.
        summary: String,
    },
    /// Terminal failure (engine error or a panicked task).
    Failed {
        /// Error text for the overlay's result line.
        error: String,
    },
}

/// Best-effort emit on the [`INDEX_PROGRESS_CHANNEL`] (a dead window drops
/// the event; the pass still runs). `pub(crate)`: the startup pass (main.rs)
/// and the create-project seed pass (ipc/projects.rs) emit through it.
pub(crate) fn emit_index_progress(app: &AppHandle, event: &IndexProgressEvent) {
    let _ = app.emit(INDEX_PROGRESS_CHANNEL, event);
}

/// Whether a STARTUP-pass progress tick should be forwarded to the overlay.
///
/// Cold start (`switched = false`): only after a full second has elapsed (an
/// already-indexed project's sub-second pass emits nothing, so the overlay
/// never flashes) AND the shared throttle accepts the tick (first /
/// every-50th / last).
///
/// Post-switch startup (`switched = true`, a pending-project marker was
/// consumed — this launch is the reload half of a `switch_project`): the
/// 1-second gate is skipped. The user explicitly opened this project
/// (mirroring the create-project seed pass, which never gates), and the UI
/// they clicked from was just torn down for the restart — delaying feedback
/// would only hide the dialog they expect to see. The shared throttle still
/// applies.
///
/// `pub(crate)`: the startup pass (main.rs) gates its ticks through it.
pub(crate) fn startup_should_forward(
    elapsed: std::time::Duration,
    done: usize,
    total: usize,
    switched: bool,
) -> bool {
    (switched || elapsed >= std::time::Duration::from_secs(1))
        && should_forward_progress(done, total, PROGRESS_EVERY)
}

// ── Startup-pass snapshot (late-overlay catch-up) ────────────────────────────

/// Whether a startup indexing pass is (possibly) running right now.
static STARTUP_INDEX_ACTIVE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
/// Files processed so far by the startup pass (meaningful while active).
static STARTUP_INDEX_DONE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Total files the startup pass will walk (meaningful while active).
static STARTUP_INDEX_TOTAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Mark the STARTUP indexing pass as begun and reset its counters. The
/// snapshot becomes fetchable immediately only when `switched` is true (a
/// post-switch startup always shows the overlay). On a COLD start the
/// snapshot stays invisible until the first tick actually clears the 1s gate
/// ([`ensure_startup_index_visible`]) — a sub-second cold pass on an
/// already-indexed project must never expose the snapshot, or a late
/// overlay's catch-up fetch would flash a dialog the cold-start contract
/// says must not appear.
///
/// WHY a snapshot: the webview listening on `codegraph://index-progress` is
/// (re)created on every app start AND every project switch; React mounts and
/// subscribes some time after the startup pass has already begun, so events
/// emitted before the subscription are lost. The pass mirrors its live
/// progress here so a late-mounting overlay can fetch a snapshot via
/// [`get_index_progress`] and catch up. Only the startup pass records — the
/// Settings-card maintenance stream and the Graph-tab refresh have their own
/// always-mounted UIs and never touch it.
pub(crate) fn mark_startup_index_active(switched: bool) {
    STARTUP_INDEX_DONE.store(0, std::sync::atomic::Ordering::Relaxed);
    STARTUP_INDEX_TOTAL.store(0, std::sync::atomic::Ordering::Relaxed);
    STARTUP_INDEX_ACTIVE.store(switched, std::sync::atomic::Ordering::Relaxed);
}

/// Make the startup-pass snapshot fetchable (idempotent). Called by the
/// progress callback on the first FORWARDED tick, so a cold start whose
/// 1s gate opens (a genuinely slow pass worth showing) becomes catch-up
/// visible exactly when the overlay stream becomes active — while gated
/// sub-second cold passes never publish anything.
pub(crate) fn ensure_startup_index_visible() {
    STARTUP_INDEX_ACTIVE.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Record one startup-pass progress tick. Called on EVERY tick, before any
/// gate, so the snapshot stays complete even for ticks the gate drops.
pub(crate) fn record_startup_index_progress(done: usize, total: usize) {
    STARTUP_INDEX_DONE.store(done as u64, std::sync::atomic::Ordering::Relaxed);
    STARTUP_INDEX_TOTAL.store(total as u64, std::sync::atomic::Ordering::Relaxed);
}

/// Clear the snapshot when the startup pass ends (before its terminal event
/// is emitted), so a fetch after the pass reports nothing rather than stale
/// final counts.
pub(crate) fn clear_startup_index() {
    STARTUP_INDEX_ACTIVE.store(false, std::sync::atomic::Ordering::Relaxed);
}

/// The startup pass's live progress, or `None` when no pass is running (or it
/// already ended — the clear and the terminal event race harmlessly: an
/// overlay that misses both sees nothing, which is correct for a finished
/// pass).
pub(crate) fn startup_index_snapshot() -> Option<(usize, usize)> {
    if STARTUP_INDEX_ACTIVE.load(std::sync::atomic::Ordering::Relaxed) {
        let done = STARTUP_INDEX_DONE.load(std::sync::atomic::Ordering::Relaxed) as usize;
        let total = STARTUP_INDEX_TOTAL.load(std::sync::atomic::Ordering::Relaxed) as usize;
        // The two counters are independent Relaxed atomics, so a concurrent
        // read can transiently pair a new `done` with the previous tick's
        // `total` (done > total for a nanosecond). Clamping keeps the
        // frontend's percentage arithmetic sane; the tear is otherwise
        // harmless and never persists (the next tick re-pairs them).
        Some((done.min(total), total))
    } else {
        None
    }
}

/// The live startup indexing progress, for overlays that mounted late.
///
/// The webview listening on `codegraph://index-progress` is (re)created on
/// every app start and project switch, so React can mount and subscribe after
/// the startup pass has already begun — the early `started`/`progress` events
/// are gone. An overlay that mounts late calls this once after subscribing:
/// `Some` means a pass is in flight and it should seed its bar with the
/// snapshot instead of waiting for the next throttled tick; `None` means no
/// startup pass is running. The create-project seed pass and the Settings /
/// Graph-tab passes are always triggered by an already-mounted UI and never
/// recorded here.
#[derive(Debug, Clone, Serialize)]
pub struct IndexProgressSnapshot {
    /// Files processed so far.
    pub done: usize,
    /// Total source files the pass will walk.
    pub total: usize,
}

/// Snapshot of the startup indexing pass's live progress (see
/// [`IndexProgressSnapshot`]). Never errors — no pass reports `Ok(None)`.
#[tauri::command]
pub fn get_index_progress() -> Option<IndexProgressSnapshot> {
    startup_index_snapshot().map(|(done, total)| IndexProgressSnapshot { done, total })
}

/// Rebuild the code content index (symbols + FTS rows) in the background,
/// streaming `started` → `progress` → `done`/`failed` on
/// `codegraph://maintenance`. Fire-and-forget, mirroring the memory
/// maintenance commands: the command returns immediately and the Settings →
/// Memory & Search card renders the stream. Errors when the graph is
/// unavailable or a pass is already running (startup / Graph-tab refresh /
/// watcher) — retry after it finishes.
#[tauri::command]
pub async fn codegraph_rebuild_index(
    app: AppHandle,
    state: State<'_, IpcState>,
) -> Result<(), IpcError> {
    let graph = graph_handle(&state)
        .ok_or_else(|| IpcError::msg("codegraph unavailable (disabled or failed to open)"))?;
    // Reject a second concurrent pass. Check + set run before the first
    // await, so two invocations cannot interleave (each future's sync prefix
    // is atomic per poll) — the same argument as codegraph_refresh (review
    // F1, 2026-04-19).
    if graph.is_indexing() {
        return Err(IpcError::msg("an indexing pass is already running"));
    }
    emit_reindex(&app, &ReindexEvent::Started);
    graph.set_indexing(true);
    let runner = graph.clone();
    let inner_app = app.clone();
    tauri::async_runtime::spawn(async move {
        // The pass is CPU + disk bound → blocking pool (never a tokio
        // worker). A panic inside index() surfaces as a JoinError → terminal
        // Failed event, so the UI can never wedge on a running bar (the
        // memory-maintenance review's B2 lesson). index() owns the indexing
        // flag: re-set on entry, cleared by a drop guard on every exit path.
        let engine = tokio::task::spawn_blocking(move || {
            runner.index(Some(&|done, total| {
                if should_forward_progress(done, total, PROGRESS_EVERY) {
                    emit_reindex(
                        &inner_app,
                        &ReindexEvent::Progress {
                            done,
                            total,
                            phase: "indexing",
                        },
                    );
                }
            }))
        })
        .await;
        match engine {
            Ok(Ok(stats)) => emit_reindex(
                &app,
                &ReindexEvent::Done {
                    summary: format!(
                        "{} files scanned · {} re-parsed · {} symbols · {} edges",
                        stats.files_scanned, stats.files_reindexed, stats.symbols, stats.edges
                    ),
                },
            ),
            Ok(Err(e)) => emit_reindex(
                &app,
                &ReindexEvent::Failed {
                    error: e.to_string(),
                },
            ),
            Err(join) => emit_reindex(
                &app,
                &ReindexEvent::Failed {
                    error: format!("reindex task panicked: {join}"),
                },
            ),
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the wire shape the frontend parses: `"type"`-tagged, kebab-case
    /// variant + field names (no `op` router — one operation on this
    /// channel). If this changes, the `ReindexEvent` type in tauri.ts and
    /// the MemorySection subscription must change with it.
    #[test]
    fn reindex_event_wire_shape() {
        assert_eq!(
            serde_json::to_value(ReindexEvent::Started).unwrap(),
            serde_json::json!({"type": "started"})
        );
        assert_eq!(
            serde_json::to_value(ReindexEvent::Progress {
                done: 2,
                total: 5,
                phase: "indexing",
            })
            .unwrap(),
            serde_json::json!({
                "type": "progress", "done": 2, "total": 5, "phase": "indexing"
            })
        );
        assert_eq!(
            serde_json::to_value(ReindexEvent::Done {
                summary: "ok".into(),
            })
            .unwrap(),
            serde_json::json!({"type": "done", "summary": "ok"})
        );
        assert_eq!(
            serde_json::to_value(ReindexEvent::Failed {
                error: "boom".into(),
            })
            .unwrap(),
            serde_json::json!({"type": "failed", "error": "boom"})
        );
    }

    /// Pins the overlay stream's wire shape the frontend parses: the same
    /// `"type"`-tagged, kebab-case shape as the reconcile events, with a
    /// phase-less `progress`. If this changes, `IndexProgressEvent` in
    /// tauri.ts and `applyIndexProgress` in IndexingOverlay.tsx must change
    /// with it.
    #[test]
    fn index_progress_event_wire_shape() {
        assert_eq!(
            serde_json::to_value(IndexProgressEvent::Started).unwrap(),
            serde_json::json!({"type": "started"})
        );
        assert_eq!(
            serde_json::to_value(IndexProgressEvent::Progress {
                done: 1,
                total: 1452,
            })
            .unwrap(),
            serde_json::json!({"type": "progress", "done": 1, "total": 1452})
        );
        assert_eq!(
            serde_json::to_value(IndexProgressEvent::Done {
                summary: "ok".into(),
            })
            .unwrap(),
            serde_json::json!({"type": "done", "summary": "ok"})
        );
        assert_eq!(
            serde_json::to_value(IndexProgressEvent::Failed {
                error: "boom".into(),
            })
            .unwrap(),
            serde_json::json!({"type": "failed", "error": "boom"})
        );
    }

    /// Table pin for the startup-pass gate: a tick forwards only after 1s
    /// has elapsed AND the shared throttle accepts it (first / every-50th /
    /// last) — unless the launch is a post-switch startup (`switched`),
    /// which skips the time gate entirely (the user explicitly opened the
    /// project) but still obeys the throttle. Covers the two "no overlay
    /// flash" edges — sub-second passes and non-throttled ticks — plus the
    /// first-visible and last ticks.
    #[test]
    fn startup_gate_requires_one_second_and_throttle() {
        let half = std::time::Duration::from_millis(500);
        let two = std::time::Duration::from_secs(2);
        // Cold start: sub-second passes emit nothing, even the first and
        // final ticks.
        assert!(!startup_should_forward(
            std::time::Duration::ZERO,
            1,
            500,
            false
        ));
        assert!(!startup_should_forward(half, 1, 500, false));
        assert!(!startup_should_forward(half, 500, 500, false));
        // After the gate opens, the throttle decides: first / every-50th /
        // last forward, everything between is dropped.
        assert!(
            startup_should_forward(two, 1, 500, false),
            "first tick forwards"
        );
        assert!(!startup_should_forward(two, 2, 500, false));
        assert!(
            startup_should_forward(two, 50, 500, false),
            "50th tick forwards"
        );
        assert!(!startup_should_forward(two, 51, 500, false));
        assert!(
            startup_should_forward(two, 500, 500, false),
            "last tick forwards"
        );
        // A 0/0 tick (no files at all) has no units — always forwards.
        assert!(startup_should_forward(two, 0, 0, false));
        // Small fast runs never show the overlay at all: 1s beats everything.
        assert!(!startup_should_forward(half, 1, 3, false));
        assert!(!startup_should_forward(half, 3, 3, false));
        // Post-switch startup (`switched = true`): the 1s gate is skipped —
        // immediate feedback mirrors the create-project seed pass — but the
        // throttle still applies.
        assert!(
            startup_should_forward(std::time::Duration::ZERO, 1, 500, true),
            "first tick forwards immediately"
        );
        assert!(startup_should_forward(half, 1, 500, true));
        assert!(
            !startup_should_forward(half, 2, 500, true),
            "throttle still drops non-boundary ticks"
        );
        assert!(startup_should_forward(half, 50, 500, true));
        assert!(
            startup_should_forward(half, 500, 500, true),
            "last tick forwards"
        );
    }

    /// The startup-pass snapshot lifecycle, including the H2 cold-start
    /// invisibility contract: a COLD pass (switched=false) must never expose
    /// the snapshot while its 1s gate is unopened (a sub-second cold pass on
    /// an already-indexed project must reopen silently — no overlay flash,
    /// no stranded dialog). The snapshot becomes fetchable either at mark
    /// time (switched=true) or on the first FORWARDED tick
    /// (ensure_startup_index_visible). This test fails without the
    /// switch-aware mark + ensure-visible design (H2, 2026-08-28 review).
    #[test]
    fn startup_snapshot_records_and_clears() {
        // Clear any residue.
        clear_startup_index();
        assert_eq!(startup_index_snapshot(), None);

        // Cold start: ticks are recorded but the snapshot stays invisible
        // while the 1s gate is unopened.
        mark_startup_index_active(false);
        record_startup_index_progress(12, 500);
        record_startup_index_progress(150, 500);
        assert_eq!(
            startup_index_snapshot(),
            None,
            "a gated cold pass must never publish the snapshot (no-flash contract)"
        );
        // …until the gate opens and a tick actually forwards.
        ensure_startup_index_visible();
        assert_eq!(startup_index_snapshot(), Some((150, 500)));

        // Switched launch: fetchable immediately at mark time.
        clear_startup_index();
        mark_startup_index_active(true);
        assert_eq!(startup_index_snapshot(), Some((0, 0)));
        record_startup_index_progress(150, 500);
        assert_eq!(startup_index_snapshot(), Some((150, 500)));
        // Torn-read clamp: a transient done > total pairing (independent
        // Relaxed atomics read across a tick) never surfaces.
        record_startup_index_progress(600, 500);
        assert_eq!(startup_index_snapshot(), Some((500, 500)));

        // Cleared at pass end → nothing reported (no stale final counts).
        clear_startup_index();
        assert_eq!(startup_index_snapshot(), None);
        // Recording while inactive does not resurrect the snapshot.
        record_startup_index_progress(200, 500);
        assert_eq!(startup_index_snapshot(), None);
    }

    /// Pins the snapshot command's wire shape: snake_case struct fields
    /// (Tauri's camelCase conversion happens on the invoke boundary, the
    /// same convention as `CodegraphStatus`).
    #[test]
    fn index_progress_snapshot_wire_shape() {
        let value = serde_json::to_value(IndexProgressSnapshot { done: 3, total: 7 }).unwrap();
        assert_eq!(value, serde_json::json!({"done": 3, "total": 7}));
    }
}
