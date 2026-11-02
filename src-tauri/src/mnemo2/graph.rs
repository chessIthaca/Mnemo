// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The Graph tab's command surface: the graph's status line, a name search,
//! one symbol's neighbourhood, its change impact, the graph's entry points
//! and one symbol's 360° context.
//!
//! Every answer is computed from ONE in-memory [`GraphView`] snapshot of the
//! open project's code graph, taken and queried on the blocking pool — the
//! store snapshot is a synchronous SQLite read and the traversals are
//! CPU-bound, so neither may park a tokio worker. The graph algorithms
//! themselves live in `mnemo::codegraph::query`; this module only filters,
//! orders, caps and shapes their answers for the tab, so the Graph tab and the
//! agent's `graph_*` tools can never disagree about what the graph says.
//!
//! Errors vs. empty answers: no open project, or an open project running with
//! `index_codegraph: false`, is a plain error string the frontend renders —
//! except for [`graph_status`] and [`graph_refresh`], which report
//! `available: false` instead, because the status line's job is to explain
//! that state rather than fail. A symbol id the index no longer holds is an
//! error as well: the tab reaches these commands from a search hit, and a
//! silently empty graph would read as "this symbol has no neighbours" when the
//! truth is "that symbol is gone — refresh".

use std::collections::HashSet;
use std::sync::Arc;

use serde::Serialize;
use tauri::State;

use mnemo::codegraph::extract::{EdgeKind, Symbol, SymbolKind};
use mnemo::codegraph::query::{GraphSliceFilter, GraphView};
use mnemo::codegraph::CodeGraph;
use mnemo::headless::{CodegraphStatus, Engine};

use crate::mnemo2::ipc::Mnemo2State;

/// Default result count for [`graph_search`].
pub const SEARCH_DEFAULT_LIMIT: usize = 50;

/// Hard ceiling on [`graph_search`] results (a bigger `limit` is clamped, not
/// rejected — the tab's box is a plain text input).
pub const SEARCH_MAX_LIMIT: usize = 200;

/// Deepest neighbourhood [`graph_neighbourhood`] serves. The tab draws one
/// ring by default and two on demand; anything deeper is a hairball.
pub const NEIGHBOURHOOD_MAX_DEPTH: usize = 2;

/// Node cap for one neighbourhood. A hub's 1-hop ring can be enormous; the
/// cap keeps the CLOSEST nodes (the 1-hop ring first, then the 2-hop ring).
pub const NEIGHBOURHOOD_NODE_CAP: usize = 150;

/// Row cap for one impact answer, nearest hop first.
pub const IMPACT_NODE_CAP: usize = 200;

/// Default entry-point count for [`graph_top`].
pub const TOP_DEFAULT_LIMIT: usize = 20;

/// Hard ceiling on [`graph_top`] entry points.
pub const TOP_MAX_LIMIT: usize = 100;

/// One symbol as the Graph tab renders it: a search hit, a graph node or an
/// impact row. `line` is the definition's start line; `kind` is the graph's
/// canonical lowercase kind string.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GraphSymbol {
    /// Stable symbol id (`{file}::{name}::{line}`) — what every other command
    /// takes.
    pub id: String,
    /// Short declared name.
    pub name: String,
    /// Symbol kind (`function`, `method`, `struct`, …).
    pub kind: SymbolKind,
    /// Project-relative file path.
    pub file: String,
    /// 1-based start line of the definition.
    pub line: usize,
}

impl GraphSymbol {
    /// Project one graph symbol onto the tab's shape.
    fn from_symbol(symbol: &Symbol) -> Self {
        Self {
            id: symbol.id.clone(),
            name: symbol.name.clone(),
            kind: symbol.kind,
            file: symbol.file.clone(),
            line: symbol.start_line,
        }
    }
}

/// A symbol plus its degree (incoming + outgoing edges), for node sizing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GraphNode {
    /// The symbol itself.
    #[serde(flatten)]
    pub symbol: GraphSymbol,
    /// Total edges touching it, both directions.
    pub degree: usize,
}

/// One edge of a selected subgraph, direction kept (`from` → `to`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GraphEdge {
    /// Source symbol id.
    pub from: String,
    /// Target symbol id.
    pub to: String,
    /// Relationship kind (`calls` / `imports` / `contains`).
    pub kind: EdgeKind,
}

/// One symbol a change would touch, with its hop distance from the root.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GraphImpactSymbol {
    /// The affected symbol.
    #[serde(flatten)]
    pub symbol: GraphSymbol,
    /// 1 = a direct caller/dependent, 2 = one more hop, …
    pub distance: usize,
}

/// A focused subgraph: the centre, the nodes kept around it and every edge
/// between kept nodes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GraphNeighbourhood {
    /// The symbol the tab focused — always the first node searched for and
    /// always present in `nodes`.
    pub center: GraphNode,
    /// Kept nodes, the centre's ring first, then the wider ring, id-sorted
    /// within each ring.
    pub nodes: Vec<GraphNode>,
    /// Edges whose BOTH endpoints are kept.
    pub edges: Vec<GraphEdge>,
    /// True when [`NEIGHBOURHOOD_NODE_CAP`] (or the graph's own 300-node slice
    /// cap) dropped nodes from this answer.
    pub truncated: bool,
}

/// One symbol's 360° view: who calls it, what it calls, and the exact totals.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GraphContext {
    /// The focused symbol, with its degree.
    pub symbol: GraphNode,
    /// Symbols with an edge INTO `symbol`, deduped and id-sorted.
    pub callers: Vec<GraphSymbol>,
    /// Symbols `symbol` points at, deduped and id-sorted.
    pub callees: Vec<GraphSymbol>,
    /// Exact incoming edge count (not capped by [`GraphContext::callers`]).
    pub caller_total: usize,
    /// Exact outgoing edge count (not capped by [`GraphContext::callees`]).
    pub callee_total: usize,
}

/// The graph's degree of `symbol_id` (0 when the id is unknown).
///
/// `GraphView` exposes edge counts only through [`GraphView::context`], which
/// reports the true totals even when it caps the listed rows — this is the one
/// per-node cost these commands pay, and only for the nodes they return.
fn degree_of(view: &GraphView, symbol_id: &str) -> usize {
    view.context(symbol_id)
        .map_or(0, |context| context.incoming_total + context.outgoing_total)
}

/// Build the tab's node shape for one symbol.
fn node_of(view: &GraphView, symbol: &Symbol) -> GraphNode {
    GraphNode {
        symbol: GraphSymbol::from_symbol(symbol),
        degree: degree_of(view, &symbol.id),
    }
}

/// Name search: case-insensitive substring match, optionally narrowed to one
/// kind, most-relevant first.
///
/// The candidate set comes from [`GraphView::slice`] (its own name/kind
/// filters). The ORDER mirrors what [`GraphView::resolve`] documents — exact
/// name, then case-insensitive exact, then substring shortest-name-first, id
/// as the tiebreak — because `resolve` itself caps at `RESOLVE_CAP` (20) and
/// takes no kind filter, while the tab's result list runs to
/// [`SEARCH_MAX_LIMIT`]. A blank query and an unknown kind string both answer
/// empty (never an error).
fn search_in(view: &GraphView, query: &str, kind: Option<&str>, limit: usize) -> Vec<GraphSymbol> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }
    let Some(slice) = view.slice(&GraphSliceFilter {
        name: Some(query),
        kind,
        from_symbol: None,
        depth: None,
    }) else {
        return Vec::new();
    };
    let mut hits = slice.nodes;
    // (bucket, name length) — resolve's three buckets (exact, case-insensitive
    // exact, substring) collapsed into one sort key, plus its shortest-name
    // tiebreak inside the substring bucket.
    let rank = |symbol: &Symbol| {
        if symbol.name == query {
            (0u8, 0usize)
        } else if symbol.name.eq_ignore_ascii_case(query) {
            (1, 0)
        } else {
            (2, symbol.name.len())
        }
    };
    hits.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| a.id.cmp(&b.id)));
    hits.truncate(limit);
    hits.iter().map(GraphSymbol::from_symbol).collect()
}

/// One symbol's neighbourhood out to `depth` hops, in both directions.
///
/// Both edge directions are walked (a focused subgraph needs callers AND
/// callees). When the cap bites, the 1-hop ring survives first: the requested
/// depth is sliced twice — once at 1 hop for the closeness ring, once at the
/// full depth — and nodes are ordered centre-first, then ring-first. Edges
/// are taken from the full-depth slice filtered to kept nodes, so the drawn
/// subgraph is always self-contained. The focused symbol ALWAYS survives
/// the cap — it is the one node the tab must always draw. An unknown id is
/// `None` (the command reports it).
fn neighbourhood_capped(
    view: &GraphView,
    symbol_id: &str,
    depth: usize,
    cap: usize,
) -> Option<GraphNeighbourhood> {
    let depth = depth.clamp(1, NEIGHBOURHOOD_MAX_DEPTH);
    let cap = cap.max(1);
    let full = view.slice(&GraphSliceFilter {
        name: None,
        kind: None,
        from_symbol: Some(symbol_id),
        depth: Some(depth),
    })?;
    // The 1-hop ring, used only to keep the CLOSEST nodes under the cap.
    let nearest: HashSet<String> = if depth > 1 {
        view.slice(&GraphSliceFilter {
            name: None,
            kind: None,
            from_symbol: Some(symbol_id),
            depth: Some(1),
        })?
        .nodes
        .into_iter()
        .map(|symbol| symbol.id)
        .collect()
    } else {
        full.nodes.iter().map(|symbol| symbol.id.clone()).collect()
    };
    let mut nodes = full.nodes;
    let all_edges = full.edges;
    // The graph's own slice cap can rank a low-degree centre out of its own
    // neighbourhood; re-attach it so the tab can always highlight it.
    if !nodes.iter().any(|symbol| symbol.id == symbol_id) {
        nodes.insert(0, view.context(symbol_id)?.symbol);
    }
    let ring = |symbol: &Symbol| u8::from(!nearest.contains(&symbol.id));
    // The centre outranks the ring under the cap (plan 0d53d659): a hub's
    // id sorts after its own 1-hop caller ring (`src/...` sorts after
    // `aNNN.rs`), so an id-only sort once let the truncate bite the centre
    // out of its own neighbourhood — and the find-the-centre `?` below
    // misread the cap bite as "no such symbol" for an id the view HAD
    // (the `join` incident).
    let rank = |symbol: &Symbol| (u8::from(symbol.id != symbol_id), ring(symbol));
    nodes.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| a.id.cmp(&b.id)));
    let truncated = nodes.len() > cap;
    nodes.truncate(cap);
    let kept: HashSet<&str> = nodes.iter().map(|symbol| symbol.id.as_str()).collect();
    let center = nodes
        .iter()
        .find(|symbol| symbol.id == symbol_id)
        .map(|symbol| node_of(view, symbol))?;
    let edges = all_edges
        .into_iter()
        .filter(|edge| kept.contains(edge.from_id.as_str()) && kept.contains(edge.to_id.as_str()))
        .map(|edge| GraphEdge {
            from: edge.from_id,
            to: edge.to_id,
            kind: edge.kind,
        })
        .collect();
    Some(GraphNeighbourhood {
        center,
        nodes: nodes.iter().map(|symbol| node_of(view, symbol)).collect(),
        edges,
        truncated,
    })
}

/// [`neighbourhood_capped`] at the tab's own node cap.
fn neighbourhood_in(view: &GraphView, symbol_id: &str, depth: usize) -> Option<GraphNeighbourhood> {
    neighbourhood_capped(view, symbol_id, depth, NEIGHBOURHOOD_NODE_CAP)
}

/// The symbols a change to `symbol_id` would touch: every transitive caller,
/// nearest hop first, capped at `cap` rows.
///
/// The traversal is unbounded ([`GraphView::impact`]'s `max_depth` 0) because
/// "what does a change touch" is a transitive question; the cap only bounds
/// the ANSWER, and the rings arrive nearest-first so truncation keeps the
/// closest callers. An unknown id is `None`.
fn impact_capped(view: &GraphView, symbol_id: &str, cap: usize) -> Option<Vec<GraphImpactSymbol>> {
    let impact = view.impact(symbol_id, 0)?;
    let cap = cap.max(1);
    let mut rows = Vec::new();
    for ring in &impact.depths {
        for symbol in &ring.symbols {
            if rows.len() >= cap {
                return Some(rows);
            }
            rows.push(GraphImpactSymbol {
                symbol: GraphSymbol::from_symbol(symbol),
                distance: ring.depth,
            });
        }
    }
    Some(rows)
}

/// [`impact_capped`] at the tab's own row cap.
fn impact_in(view: &GraphView, symbol_id: &str) -> Option<Vec<GraphImpactSymbol>> {
    impact_capped(view, symbol_id, IMPACT_NODE_CAP)
}

/// The most-connected symbols — the tab's entry points when nothing is
/// selected (instead of Mnemo1's unfiltered 300-node hairball).
///
/// Candidates come from an unfiltered [`GraphView::slice`], which already
/// pre-selects the graph's most-connected nodes when the graph is bigger than
/// its own cap; they are re-ordered by true degree (computed per node) and
/// truncated to `limit`.
fn top_in(view: &GraphView, limit: usize) -> Vec<GraphNode> {
    let Some(slice) = view.slice(&GraphSliceFilter::default()) else {
        return Vec::new();
    };
    let mut nodes: Vec<GraphNode> = slice
        .nodes
        .iter()
        .map(|symbol| node_of(view, symbol))
        .collect();
    nodes.sort_by(|a, b| {
        b.degree
            .cmp(&a.degree)
            .then_with(|| a.symbol.id.cmp(&b.symbol.id))
    });
    nodes.truncate(limit.max(1));
    nodes
}

/// One symbol's callers and callees with the graph's EXACT totals.
///
/// The listed rows inherit the graph's own per-kind group cap
/// (`CONTEXT_GROUP_CAP`), but the totals are untouched — so the details panel
/// can say "42 callers" truthfully even when it lists the first 50. Rows are
/// deduped by symbol id (one symbol can both call and be imported by the same
/// target) and id-sorted.
fn context_in(view: &GraphView, symbol_id: &str) -> Option<GraphContext> {
    let context = view.context(symbol_id)?;
    let mut callers: Vec<GraphSymbol> = Vec::new();
    let mut callees: Vec<GraphSymbol> = Vec::new();
    for group in context.incoming.values() {
        callers.extend(group.iter().map(GraphSymbol::from_symbol));
    }
    for group in context.outgoing.values() {
        callees.extend(group.iter().map(GraphSymbol::from_symbol));
    }
    for rows in [&mut callers, &mut callees] {
        rows.sort_by(|a, b| a.id.cmp(&b.id));
        rows.dedup_by(|a, b| a.id == b.id);
    }
    Some(GraphContext {
        symbol: GraphNode {
            symbol: GraphSymbol::from_symbol(&context.symbol),
            degree: context.incoming_total + context.outgoing_total,
        },
        callers,
        callees,
        caller_total: context.incoming_total,
        callee_total: context.outgoing_total,
    })
}

/// The error a genuinely unknown symbol id produces — one wording for every
/// command, so the tab can show one recovery hint. The id commands refresh
/// the index once and retry BEFORE this fires (plan 0d53d659), so by the
/// time the user reads it the miss is proven against a fresh index and
/// "search again" is the only move left.
fn unknown_symbol(symbol_id: &str) -> String {
    format!(
        "the code graph has no symbol with id `{symbol_id}` even after one automatic refresh — the index has moved on; search for the symbol again"
    )
}

/// The open project's engine, or a clear "nothing is open" error.
async fn engine(state: &Mnemo2State) -> Result<Arc<Engine>, String> {
    let lead = state
        .open_lead()
        .await
        .ok_or_else(|| "Mnemo2 has no open project; open one first".to_string())?;
    lead.engine_handle().await
}

/// The open project's code-graph handle, or a clear "graph unavailable" error.
async fn graph_handle(state: &Mnemo2State) -> Result<Arc<CodeGraph>, String> {
    let engine = engine(state).await?;
    engine.codegraph_handle().ok_or_else(|| {
        "the code graph is unavailable for this project (code indexing is off)".to_string()
    })
}

/// Run a read-only view query on the blocking pool.
///
/// The graph snapshot is a synchronous SQLite read and every traversal is
/// CPU-bound, so both must stay off the tokio workers.
async fn run_view<T, F>(graph: Arc<CodeGraph>, query: F) -> Result<T, String>
where
    F: FnOnce(&GraphView) -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let view = graph
            .view()
            .map_err(|error| format!("could not read the code graph: {error}"))?;
        query(&view)
    })
    .await
    .map_err(|error| format!("code graph task failed: {error}"))?
}

/// [`run_view`] for a symbol-id query that can miss: `None` (the id is
/// unknown to THIS view) instead of a ready-made error, so
/// [`resolve_or_refresh`] can tell "no such symbol" apart from real
/// failures and retry after a refresh.
async fn run_view_opt<T, F>(graph: Arc<CodeGraph>, ok: F) -> Result<Option<T>, String>
where
    F: FnOnce(&GraphView) -> Option<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let view = graph
            .view()
            .map_err(|error| format!("could not read the code graph: {error}"))?;
        Ok(ok(&view))
    })
    .await
    .map_err(|error| format!("code graph task failed: {error}"))?
}

/// One incremental index pass — the [`graph_refresh`] core, reused by
/// [`resolve_or_refresh`]'s automatic retry.
async fn refresh_index(state: &Mnemo2State) -> Result<(), String> {
    let engine = engine(state).await?;
    engine
        .codegraph()
        .refresh()
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// Run one symbol-id query against a fresh view with the honest-error retry
/// (plan 0d53d659): an id that misses gets exactly ONE automatic index
/// refresh and a second attempt before the unknown-symbol error — the Graph
/// tab hands us ids from an earlier snapshot, and the background index pass
/// can move the index between listing and click. A miss on the refreshed
/// index is a genuinely unknown symbol. `first` and `retry` are the SAME
/// query (each owns its id clone — a `FnOnce` attempt cannot run twice).
async fn resolve_or_refresh<T, Q1, Q2>(
    state: &Mnemo2State,
    symbol_id: &str,
    first: Q1,
    retry: Q2,
) -> Result<T, String>
where
    Q1: FnOnce(&GraphView) -> Option<T> + Send + 'static,
    Q2: FnOnce(&GraphView) -> Option<T> + Send + 'static,
    T: Send + 'static,
{
    let graph = graph_handle(state).await?;
    if let Some(value) = run_view_opt(graph.clone(), first).await? {
        return Ok(value);
    }
    refresh_index(state).await?;
    run_view_opt(graph, retry)
        .await?
        .ok_or_else(|| unknown_symbol(symbol_id))
}

/// The Graph tab's status line: availability, size, indexing pulse and the
/// time of the last indexed file.
///
/// A project without a graph (`index_codegraph: false`) answers
/// `available: false` rather than an error — explaining that state is this
/// command's job. Only "no project is open at all" is an error.
#[tauri::command]
pub async fn graph_status(state: State<'_, Mnemo2State>) -> Result<CodegraphStatus, String> {
    let engine = engine(&state).await?;
    engine
        .codegraph()
        .status()
        .await
        .map_err(|error| error.to_string())
}

/// Request an incremental re-index pass and answer the status it starts from
/// (the pass itself runs in the background; poll [`graph_status`] for the
/// pulse).
#[tauri::command]
pub async fn graph_refresh(state: State<'_, Mnemo2State>) -> Result<CodegraphStatus, String> {
    let engine = engine(&state).await?;
    engine
        .codegraph()
        .refresh()
        .await
        .map_err(|error| error.to_string())
}

/// Search the graph by symbol name (case-insensitive substring), optionally
/// narrowed to one kind, most-relevant first.
///
/// `limit` defaults to [`SEARCH_DEFAULT_LIMIT`] and is clamped to
/// [`SEARCH_MAX_LIMIT`].
#[tauri::command]
pub async fn graph_search(
    state: State<'_, Mnemo2State>,
    query: String,
    kind: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<GraphSymbol>, String> {
    let graph = graph_handle(&state).await?;
    let limit = limit
        .unwrap_or(SEARCH_DEFAULT_LIMIT)
        .clamp(1, SEARCH_MAX_LIMIT);
    run_view(graph, move |view| {
        Ok(search_in(view, &query, kind.as_deref(), limit))
    })
    .await
}

/// One symbol's neighbourhood out to `depth` hops (1 by default, 2 max),
/// capped at [`NEIGHBOURHOOD_NODE_CAP`] nodes with the closest kept.
#[tauri::command]
pub async fn graph_neighbourhood(
    state: State<'_, Mnemo2State>,
    symbol_id: String,
    depth: Option<usize>,
) -> Result<GraphNeighbourhood, String> {
    let depth = depth.unwrap_or(1);
    let first_id = symbol_id.clone();
    let retry_id = symbol_id.clone();
    resolve_or_refresh(
        &state,
        &symbol_id,
        move |view| neighbourhood_in(view, &first_id, depth),
        move |view| neighbourhood_in(view, &retry_id, depth),
    )
    .await
}

/// Every symbol a change to `symbol_id` would touch, nearest hop first, capped
/// at [`IMPACT_NODE_CAP`] rows.
#[tauri::command]
pub async fn graph_impact(
    state: State<'_, Mnemo2State>,
    symbol_id: String,
) -> Result<Vec<GraphImpactSymbol>, String> {
    let first_id = symbol_id.clone();
    let retry_id = symbol_id.clone();
    resolve_or_refresh(
        &state,
        &symbol_id,
        move |view| impact_in(view, &first_id),
        move |view| impact_in(view, &retry_id),
    )
    .await
}

/// The graph's most-connected symbols — the tab's entry points when nothing is
/// selected. `limit` defaults to [`TOP_DEFAULT_LIMIT`], clamped to
/// [`TOP_MAX_LIMIT`].
#[tauri::command]
pub async fn graph_top(
    state: State<'_, Mnemo2State>,
    limit: Option<usize>,
) -> Result<Vec<GraphNode>, String> {
    let graph = graph_handle(&state).await?;
    let limit = limit.unwrap_or(TOP_DEFAULT_LIMIT).clamp(1, TOP_MAX_LIMIT);
    run_view(graph, move |view| Ok(top_in(view, limit))).await
}

/// One symbol's callers and callees with exact totals — the details panel's
/// lists, which the capped neighbourhood edges alone cannot give truthfully.
#[tauri::command]
pub async fn graph_context(
    state: State<'_, Mnemo2State>,
    symbol_id: String,
) -> Result<GraphContext, String> {
    let first_id = symbol_id.clone();
    let retry_id = symbol_id.clone();
    resolve_or_refresh(
        &state,
        &symbol_id,
        move |view| context_in(view, &first_id),
        move |view| context_in(view, &retry_id),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use mnemo::codegraph::store::EdgeRow;

    /// A fixture symbol, id-derived the way the extractor derives it.
    fn sym(file: &str, name: &str, kind: SymbolKind, line: usize) -> Symbol {
        Symbol {
            id: format!("{file}::{name}::{line}"),
            name: name.to_string(),
            kind,
            file: file.to_string(),
            start_line: line,
            end_line: line + 2,
        }
    }

    /// A fixture edge (`from` → `to`).
    fn link(from: &Symbol, to: &Symbol, kind: EdgeKind) -> EdgeRow {
        EdgeRow {
            from_id: from.id.clone(),
            to_id: to.id.clone(),
            kind,
        }
    }

    const RUN: &str = "src/main.rs::run::1";
    const BOOTSTRAP: &str = "src/main.rs::bootstrap::9";
    const SAVE: &str = "src/store.rs::save::5";
    const SAVE_ALL: &str = "src/store.rs::save_all::12";

    /// `run` and `save_all` both call `save`; `bootstrap` calls `save_all`;
    /// `Store` is an isolated struct (degree 0).
    fn fixture() -> GraphView {
        let run = sym("src/main.rs", "run", SymbolKind::Function, 1);
        let bootstrap = sym("src/main.rs", "bootstrap", SymbolKind::Function, 9);
        let save = sym("src/store.rs", "save", SymbolKind::Function, 5);
        let save_all = sym("src/store.rs", "save_all", SymbolKind::Function, 12);
        let store = sym("src/store.rs", "Store", SymbolKind::Struct, 1);
        // Edges borrow the symbols, so build them before moving the symbols in.
        let edges = vec![
            link(&run, &save, EdgeKind::Calls),
            link(&save_all, &save, EdgeKind::Calls),
            link(&bootstrap, &save_all, EdgeKind::Calls),
        ];
        GraphView::from_parts(vec![run, bootstrap, save, save_all, store], edges)
    }

    #[test]
    fn a_search_hit_carries_the_definition_site() {
        let view = fixture();
        let hits = search_in(&view, "save", None, 10);
        assert_eq!(hits.len(), 2, "`save` and `save_all` match");
        assert_eq!(hits[0].name, "save", "the exact name ranks first");
        assert_eq!(hits[0].id, SAVE);
        assert_eq!(hits[0].kind, SymbolKind::Function);
        assert_eq!(hits[0].file, "src/store.rs");
        assert_eq!(hits[0].line, 5);
        assert_eq!(hits[1].name, "save_all");
    }

    #[test]
    fn the_search_kind_filter_and_limit_are_honoured() {
        let view = fixture();
        assert!(
            search_in(&view, "save", Some("struct"), 10).is_empty(),
            "no matching kind answers empty, not an error"
        );
        let limited = search_in(&view, "save", Some("function"), 1);
        assert_eq!(limited.len(), 1);
        assert_eq!(limited[0].name, "save");
        assert!(
            search_in(&view, "   ", None, 10).is_empty(),
            "a blank query is not a query"
        );
        assert!(search_in(&view, "zzz", None, 10).is_empty());
    }

    #[test]
    fn a_neighbourhood_stops_at_the_requested_depth() {
        let view = fixture();
        let one = neighbourhood_in(&view, SAVE, 1).expect("save exists");
        assert_eq!(one.center.symbol.id, SAVE, "the centre is re-attached");
        assert_eq!(one.center.degree, 2, "two callers, no callees");
        let ids: Vec<&str> = one
            .nodes
            .iter()
            .map(|node| node.symbol.id.as_str())
            .collect();
        assert_eq!(ids, vec![SAVE, RUN, SAVE_ALL], "centre first, then the ring in id order");
        assert!(
            !ids.contains(&BOOTSTRAP),
            "a two-hop caller stays out of a one-hop neighbourhood"
        );
        assert_eq!(one.edges.len(), 2);
        assert!(one
            .edges
            .iter()
            .any(|edge| edge.from == RUN && edge.to == SAVE && edge.kind == EdgeKind::Calls));
        assert!(!one.truncated);

        let two = neighbourhood_in(&view, SAVE, 2).expect("save exists");
        let ids: Vec<&str> = two
            .nodes
            .iter()
            .map(|node| node.symbol.id.as_str())
            .collect();
        assert_eq!(ids, vec![SAVE, RUN, SAVE_ALL, BOOTSTRAP]);
        assert_eq!(two.edges.len(), 3);
        assert_eq!(
            neighbourhood_in(&view, SAVE, 9).map(|answer| answer.nodes.len()),
            Some(4),
            "depth is clamped to the two-hop maximum"
        );
    }

    #[test]
    fn the_neighbourhood_cap_keeps_the_closest_nodes() {
        let view = fixture();
        let capped = neighbourhood_capped(&view, SAVE, 2, 3).expect("save exists");
        assert!(capped.truncated);
        let ids: Vec<&str> = capped
            .nodes
            .iter()
            .map(|node| node.symbol.id.as_str())
            .collect();
        assert_eq!(ids, vec![SAVE, RUN, SAVE_ALL], "centre first, then the surviving 1-hop ring");
        assert!(
            capped
                .edges
                .iter()
                .all(|edge| ids.contains(&edge.from.as_str()) && ids.contains(&edge.to.as_str())),
            "the drawn subgraph is self-contained"
        );
    }

    #[test]
    fn the_impact_lists_transitive_callers_nearest_first() {
        let view = fixture();
        let impact = impact_in(&view, SAVE).expect("save exists");
        let rows: Vec<(&str, usize)> = impact
            .iter()
            .map(|row| (row.symbol.id.as_str(), row.distance))
            .collect();
        assert_eq!(rows, vec![(RUN, 1), (SAVE_ALL, 1), (BOOTSTRAP, 2)]);

        let capped = impact_capped(&view, SAVE, 2).expect("save exists");
        assert_eq!(capped.len(), 2, "the cap bounds the answer");
        assert!(
            capped.iter().all(|row| row.distance == 1),
            "truncation keeps the nearest callers"
        );
    }

    #[test]
    fn the_entry_points_are_ordered_by_degree() {
        let view = fixture();
        let top = top_in(&view, 3);
        let names: Vec<&str> = top.iter().map(|node| node.symbol.name.as_str()).collect();
        assert_eq!(names, vec!["save", "save_all", "bootstrap"]);
        assert_eq!(top[0].degree, 2);
        assert!(
            top.windows(2).all(|pair| pair[0].degree >= pair[1].degree),
            "degree never rises down the list"
        );
        let all = top_in(&view, TOP_DEFAULT_LIMIT);
        assert_eq!(all.len(), 5);
        assert_eq!(
            all[4].symbol.name, "Store",
            "the isolated symbol ranks last"
        );
        assert_eq!(all[4].degree, 0);
    }

    #[test]
    fn the_context_lists_callers_and_callees_with_exact_totals() {
        let view = fixture();
        let context = context_in(&view, SAVE).expect("save exists");
        assert_eq!(context.symbol.degree, 2);
        assert_eq!(context.caller_total, 2);
        assert_eq!(context.callee_total, 0);
        let callers: Vec<&str> = context
            .callers
            .iter()
            .map(|symbol| symbol.id.as_str())
            .collect();
        assert_eq!(callers, vec![RUN, SAVE_ALL], "id-sorted callers");
        assert!(context.callees.is_empty());

        let run = context_in(&view, RUN).expect("run exists");
        assert_eq!(run.caller_total, 0);
        assert_eq!(run.callee_total, 1);
        assert_eq!(run.callees[0].id, SAVE);
    }

    #[test]
    fn an_unknown_symbol_is_reported_rather_than_answered() {
        let view = fixture();
        let gone = "src/nope.rs::gone::1";
        assert!(neighbourhood_in(&view, gone, 1).is_none());
        assert!(impact_in(&view, gone).is_none());
        assert!(context_in(&view, gone).is_none());
        let message = unknown_symbol(gone);
        assert!(message.contains(gone), "the message names the id");
        assert!(message.contains("refresh"), "and tells the tab what to do");
    }

    #[test]
    fn the_wire_shapes_match_the_graph_tab_contract() {
        let view = fixture();
        let neighbourhood = neighbourhood_in(&view, SAVE, 1).expect("save exists");
        let value = serde_json::to_value(&neighbourhood).expect("serializable");
        assert_eq!(value["center"]["id"], SAVE);
        assert_eq!(value["center"]["line"], 5);
        assert_eq!(value["center"]["kind"], "function");
        assert_eq!(value["center"]["degree"], 2);
        assert!(value["nodes"].is_array());
        assert_eq!(value["edges"][0]["from"], RUN);
        assert_eq!(value["edges"][0]["to"], SAVE);
        assert_eq!(value["edges"][0]["kind"], "calls");
        assert_eq!(value["truncated"], false);

        let hit = serde_json::to_value(GraphSymbol::from_symbol(&sym(
            "a.rs",
            "n",
            SymbolKind::Function,
            3,
        )))
        .expect("serializable");
        assert_eq!(
            hit,
            serde_json::json!({"id": "a.rs::n::3", "name": "n", "kind": "function", "file": "a.rs", "line": 3})
        );
    }

    #[test]
    fn graph_tab_opens_every_listed_symbol() {
        // The incident (plan 0d53d659): the tab listed `join` — a tiny test
        // helper whose bare name had collected hundreds of callers — but
        // clicking it answered `unknown_symbol` for an id the view HAD.
        // The neighbourhood's node cap sorts kept nodes by (ring, id), and
        // the centre sorted after its own 1-hop caller ring (`aNNN.rs`
        // sorts before `src/...`), so the truncate bit it out and the
        // find-the-centre `?` misread the cap bite as "no such symbol".
        let mut symbols = Vec::new();
        let mut edges = Vec::new();
        let hub = sym(
            "src/tool/agent/shell_filter.rs",
            "join",
            SymbolKind::Function,
            683,
        );
        for i in 0..(NEIGHBOURHOOD_NODE_CAP + 10) {
            let caller = sym(&format!("a{i:03}.rs"), "caller", SymbolKind::Function, 1);
            edges.push(link(&caller, &hub, EdgeKind::Calls));
            symbols.push(caller);
        }
        symbols.push(hub);
        let view = GraphView::from_parts(symbols, edges);

        // List symbols the way the tab lists them — the search box and the
        // top entry-points list. Both must surface the hub.
        let listed: Vec<GraphSymbol> = search_in(&view, "join", None, SEARCH_MAX_LIMIT)
            .into_iter()
            .chain(top_in(&view, TOP_DEFAULT_LIMIT).into_iter().map(|node| node.symbol))
            .collect();
        assert!(
            listed
                .iter()
                .any(|hit| hit.id == "src/tool/agent/shell_filter.rs::join::683"),
            "the hub must be listed before it can be clicked"
        );

        // Clicking every listed symbol must open all three views.
        for hit in &listed {
            assert!(
                neighbourhood_in(&view, &hit.id, 1).is_some(),
                "the neighbourhood of {} must open",
                hit.id
            );
            assert!(
                context_in(&view, &hit.id).is_some(),
                "the context of {} must open",
                hit.id
            );
            assert!(
                impact_in(&view, &hit.id).is_some(),
                "the impact of {} must open",
                hit.id
            );
        }
    }

}
