// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The code-graph data API (§5.6 :435-437) — the binding-ready surface
//! over the engine's [`CodeGraph`], ONE implementation shared by the
//! headless `Engine` and the app's IPC commands (3b.2, D2). The wire
//! types moved here VERBATIM from `src-tauri/src/ipc/codegraph_cmds.rs`
//! — their serde shapes are pinned by the row-19 contract fixtures
//! (`stats_memory_codegraph_ipc_unchanged`), so the bytes cannot drift.
//! The engine reaches this via `Engine::codegraph()`; the IPC commands
//! construct it from their state handles.

use std::sync::Arc;

use serde::Serialize;

use crate::codegraph::query::{GraphSliceFilter, GraphView};
use crate::codegraph::{CodeGraph, IndexStats};

/// The Graph tab's status line: is the graph available, how big is it, and
/// is an indexing pass running right now.
#[derive(Debug, Clone, Serialize)]
pub struct CodegraphStatus {
    /// False when codegraph is disabled (`index_codegraph: false`), the DB
    /// failed to open, or the brain failed to build — the tab shows an
    /// "unavailable" state instead of empty counts.
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
    pub fn unavailable() -> Self {
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
    pub fn from_parts(stats: crate::codegraph::store::GraphStats, indexing: bool) -> Self {
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
    pub nodes: Vec<crate::codegraph::extract::Symbol>,
    /// The edges among them (both endpoints selected), id-sorted.
    pub edges: Vec<crate::codegraph::store::EdgeRow>,
}

/// The code-graph data API (§5.6) — status, refresh, rebuild, and the
/// visualization query, shared by `Engine::codegraph()` and the IPC.
pub struct CodegraphApi {
    /// The graph handle (`None` when `index_codegraph: false` or the DB
    /// failed to open — every method reports the unavailable shape).
    graph: Option<Arc<CodeGraph>>,
}

impl CodegraphApi {
    /// Build from the raw handle — the engine's `Engine::codegraph()` and
    /// the IPC commands both use this.
    pub fn from_parts(graph: Option<Arc<CodeGraph>>) -> Self {
        Self { graph }
    }

    /// §5.6's `status()` — the tab's status line. The stats read runs on
    /// the blocking pool (the sync SQLite lock never rides a tokio
    /// worker); errors propagate (the caller decides how to render).
    pub async fn status(&self) -> Result<CodegraphStatus, crate::error::Error> {
        let Some(graph) = self.graph.clone() else {
            return Ok(CodegraphStatus::unavailable());
        };
        let indexing = graph.is_indexing();
        let stats = tokio::task::spawn_blocking(move || graph.stats())
            .await
            .map_err(|e| {
                crate::error::Error::Runtime(format!("codegraph stats task failed: {e}"))
            })??;
        Ok(CodegraphStatus::from_parts(stats, indexing))
    }

    /// §5.6's `progress()` — the startup/pass progress snapshot
    /// (files done, total), if a pass is (or was recently) running.
    pub fn progress(&self) -> Option<(usize, usize)> {
        crate::host::index_progress::startup_index_snapshot()
    }

    /// §5.6's `refresh()` — schedule an incremental re-index pass (a pass
    /// already running is NOT stacked; the scheduled status is returned
    /// deterministically — the flag is set before the spawn).
    pub async fn refresh(&self) -> Result<CodegraphStatus, crate::error::Error> {
        let Some(graph) = self.graph.clone() else {
            return Ok(CodegraphStatus::unavailable());
        };
        if graph.is_indexing() {
            return self.status().await;
        }
        // Set the flag BEFORE spawning so the returned status (and any
        // poller) deterministically sees the scheduled pass.
        graph.set_indexing(true);
        let runner = graph.clone();
        tokio::spawn(async move {
            // index() owns the indexing flag (set on entry, cleared by a
            // drop guard on every exit path); the parse is CPU + disk
            // bound → blocking pool.
            let result = tokio::task::spawn_blocking(move || runner.index(None)).await;
            match result {
                Ok(Ok(stats)) => log_refresh(&stats),
                Ok(Err(e)) => eprintln!("codegraph: refresh failed: {e}"),
                Err(e) => eprintln!("codegraph: refresh task panicked: {e}"),
            }
        });
        self.status().await
    }

    /// §5.6's `rebuild()` — run a full index pass to completion and return
    /// its stats (the engine-side form of the app's event-streamed
    /// rebuild; the wipe-then-rescan is `index(None)` over the full
    /// corpus).
    pub async fn rebuild(&self) -> Result<IndexStats, crate::error::Error> {
        let graph = self
            .graph
            .clone()
            .ok_or_else(|| crate::error::Error::Runtime("codegraph unavailable".to_string()))?;
        let stats = tokio::task::spawn_blocking(move || graph.index(None))
            .await
            .map_err(|e| {
                crate::error::Error::Runtime(format!("codegraph rebuild task failed: {e}"))
            })??;
        Ok(stats)
    }

    /// §5.6's `query(..)` — the visualization slice. With no filters: the
    /// top [`GRAPH_SLICE_CAP`](crate::codegraph::query::GRAPH_SLICE_CAP)
    /// nodes by degree. A stale/unknown `from_symbol` yields an EMPTY
    /// graph (not an error) — the tab survives a re-index racing a click.
    pub async fn query(
        &self,
        name: Option<&str>,
        kind: Option<&str>,
        from_symbol: Option<&str>,
        depth: Option<usize>,
    ) -> Result<CodegraphGraph, crate::error::Error> {
        let Some(graph) = self.graph.clone() else {
            return Ok(CodegraphGraph {
                nodes: Vec::new(),
                edges: Vec::new(),
            });
        };
        let name = name.map(String::from);
        let kind = kind.map(String::from);
        let from_symbol = from_symbol.map(String::from);
        // Snapshot + slice on the blocking pool (sync SQLite lock +
        // O(nodes) selection — never on a tokio worker). The owned filter
        // strings move into the closure ('static); the borrowed
        // GraphSliceFilter is constructed inside.
        let slice = tokio::task::spawn_blocking(move || -> Result<_, crate::error::Error> {
            let filter = GraphSliceFilter {
                name: name.as_deref(),
                kind: kind.as_deref(),
                from_symbol: from_symbol.as_deref(),
                depth,
            };
            let view: GraphView = graph.view()?;
            Ok(view.slice(&filter))
        })
        .await
        .map_err(|e| {
            crate::error::Error::Runtime(format!("codegraph graph task failed: {e}"))
        })??;
        // Unknown from_symbol (slice → None) yields an EMPTY graph, not
        // an error.
        let slice = slice.unwrap_or_default();
        Ok(CodegraphGraph {
            nodes: slice.nodes,
            edges: slice.edges,
        })
    }
}

/// Log a successful refresh pass (kept out of the method body for clarity).
fn log_refresh(stats: &IndexStats) {
    eprintln!(
        "codegraph: refreshed — {} files scanned, {} re-parsed, {} symbols, {} edges in {}ms",
        stats.files_scanned, stats.files_reindexed, stats.symbols, stats.edges, stats.elapsed_ms
    );
}