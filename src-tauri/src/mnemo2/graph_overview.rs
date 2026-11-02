// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The Graph tab's opening view: the whole code graph as a map of modules.
//!
//! The tab used to draw only the selected symbol's 1- or 2-hop neighbourhood,
//! so with nothing selected the canvas was empty and the graph itself was
//! invisible. A repo-scale graph cannot be drawn symbol by symbol — an
//! unfiltered [`GraphView::slice`] is capped at `GRAPH_SLICE_CAP` nodes — so
//! this command aggregates that degree-ranked slice into directory modules:
//! one node per directory, sized by its symbol count, with the symbol edges
//! between directories merged into counted bundles. Each module also carries
//! its most-connected symbols, so a click can drill into a module without a
//! second round-trip.
//!
//! `level` picks the directory depth to aggregate at (`0` draws the slice
//! symbol by symbol, the "top symbols" view). Depth is measured in path
//! segments: `level = 2` turns `mnemo2/src/lib/graph.ts` and
//! `mnemo2/src/App.svelte` into one `mnemo2/src` node, while
//! `src-tauri/src/mnemo2/graph.rs` lands in `src-tauri/src`. Two segments is
//! the default because it keeps this repository at roughly forty modules for
//! its 2-3k files — dense enough to show the architecture, sparse enough to
//! read — whereas depth 3 splits single packages into dozens of near-empty
//! leaves and depth 1 collapses every `src/*` directory into one blob.
//!
//! The snapshot read and the aggregation run on the blocking pool: the
//! SQLite read is synchronous and the traversals are CPU-bound. Like
//! [`crate::mnemo2::graph`], the graph algorithms stay in
//! `mnemo::codegraph::query`; this module only aggregates, orders and caps
//! their answers for the tab.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use serde::Serialize;
use tauri::State;

use mnemo::codegraph::extract::{EdgeKind, Symbol, SymbolKind};
use mnemo::codegraph::query::{GraphSliceFilter, GraphView};
use mnemo::codegraph::CodeGraph;
use mnemo::headless::Engine;

use crate::mnemo2::graph::{GraphNode, GraphSymbol};
use crate::mnemo2::ipc::Mnemo2State;

/// Default directory depth of the module overview.
///
/// Two segments reads as architecture for this repository: `src/codegraph`,
/// `mnemo2/src`, `src-tauri/src`, `tests/integration` — about forty modules.
pub const OVERVIEW_DEFAULT_LEVEL: usize = 2;

/// Deepest directory depth [`graph_overview`] serves. `0` is the symbol level
/// (the degree-ranked slice itself), `1..=3` aggregate directories.
pub const OVERVIEW_MAX_LEVEL: usize = 3;

/// Default node cap for one overview answer (60 modules is a readable map at
/// the tab's canvas size; the `limit` argument can raise it to
/// [`OVERVIEW_MAX_LIMIT`]).
pub const OVERVIEW_DEFAULT_LIMIT: usize = 60;

/// Hard ceiling on overview nodes — the slice's own node cap, so the overview
/// never draws more than the graph slice it aggregates.
pub const OVERVIEW_MAX_LIMIT: usize = 300;

/// Symbols embedded per module for the click-to-drill-in ring.
pub const OVERVIEW_MODULE_SYMBOLS: usize = 12;

/// One node of the overview: a directory module, or one symbol at level 0.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OverviewNode {
    /// Module directory path, or the symbol id at level 0.
    pub id: String,
    /// Short label: the directory's last segment (module), or the symbol name.
    pub label: String,
    /// Symbol kind at level 0; `None` for a directory module.
    pub kind: Option<SymbolKind>,
    /// Project-relative file (symbol nodes) or directory path (modules).
    pub file: String,
    /// Definition start line at level 0; `0` for a module.
    pub line: usize,
    /// Symbols the node stands for (`1` for a symbol node).
    pub symbols: usize,
    /// Aggregated edge weight touching the node.
    pub degree: usize,
    /// Module nodes: the most-connected symbols inside, for the drill-in ring.
    pub top: Vec<GraphNode>,
}

/// One edge of the overview: a counted bundle between two modules, or one
/// symbol edge at level 0.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OverviewEdge {
    /// Source module id (or symbol id at level 0).
    pub from: String,
    /// Target module id (or symbol id at level 0).
    pub to: String,
    /// Relationship kind (`calls` / `imports` / `contains`).
    pub kind: EdgeKind,
    /// How many symbol edges this row stands for (always 1 at level 0).
    pub count: usize,
}

/// The Graph tab's opening view: the whole graph as modules (or as symbols at
/// level 0), with aggregated and counted edges between them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GraphOverview {
    /// Directory depth aggregated at (`0` = one node per symbol).
    pub level: usize,
    /// The drawn nodes, most prominent first.
    pub nodes: Vec<OverviewNode>,
    /// Edges between the drawn nodes only, deterministic order.
    pub edges: Vec<OverviewEdge>,
    /// Candidate nodes before the `limit` cap.
    pub total_nodes: usize,
    /// True when the cap dropped nodes.
    pub truncated: bool,
    /// Symbols in the whole graph snapshot.
    pub total_symbols: usize,
    /// Symbols this overview aggregated (the degree-ranked slice); less than
    /// `total_symbols` on a repo-scale graph, because the slice is capped.
    pub sampled_symbols: usize,
}

/// The directory module a file belongs to at `level` directory segments.
///
/// A file shallower than `level` keeps its deepest directory, so `src/main.rs`
/// is `src` at every level; a file at the project root belongs to the `"."`
/// module.
fn module_of(file: &str, level: usize) -> String {
    let segments: Vec<&str> = file.split('/').filter(|part| !part.is_empty()).collect();
    let directories = segments.len().saturating_sub(1).min(level);
    if directories == 0 {
        ".".to_string()
    } else {
        segments[..directories].join("/")
    }
}

/// A module's short label: its last directory segment (`"."` → `root`).
fn module_label(id: &str) -> String {
    if id == "." {
        return "root".to_string();
    }
    id.rsplit('/').next().unwrap_or(id).to_string()
}

/// Project a graph symbol onto the tab's wire shape.
fn project(symbol: &Symbol) -> GraphSymbol {
    GraphSymbol {
        id: symbol.id.clone(),
        name: symbol.name.clone(),
        kind: symbol.kind,
        file: symbol.file.clone(),
        line: symbol.start_line,
    }
}

/// Aggregate `view`'s degree-ranked slice into modules at `level`.
///
/// Level 0 keeps the slice's symbols as nodes (the tab's "top symbols" view);
/// level 1..=[`OVERVIEW_MAX_LEVEL`] groups them by directory, sizes each module
/// by its symbol count and merges the symbol edges that cross modules into
/// `(from, to, kind)` bundles with a `count`. Edges inside one module are not
/// drawn (a self-loop says nothing on a module map), so a module's `degree`
/// counts only the cross-module weight it carries.
///
/// Deterministic: modules rank by symbol count, then degree, then id, and
/// edges by `(from, to, kind)` — the same snapshot always draws the same map.
pub fn overview_in(view: &GraphView, level: usize, limit: usize) -> GraphOverview {
    let level = level.min(OVERVIEW_MAX_LEVEL);
    let limit = limit.clamp(1, OVERVIEW_MAX_LIMIT);
    let slice = view.slice(&GraphSliceFilter::default()).unwrap_or_default();
    let total_symbols = view.symbol_count();
    let sampled_symbols = slice.nodes.len();

    // Degree inside the slice, both directions: the tie-breaker for symbol
    // ranking and the "most-connected" order inside a module.
    let mut degree: HashMap<&str, usize> = HashMap::new();
    for edge in &slice.edges {
        *degree.entry(edge.from_id.as_str()).or_default() += 1;
        *degree.entry(edge.to_id.as_str()).or_default() += 1;
    }

    if level == 0 {
        let mut nodes: Vec<OverviewNode> = slice
            .nodes
            .iter()
            .map(|symbol| OverviewNode {
                id: symbol.id.clone(),
                label: symbol.name.clone(),
                kind: Some(symbol.kind),
                file: symbol.file.clone(),
                line: symbol.start_line,
                symbols: 1,
                degree: degree.get(symbol.id.as_str()).copied().unwrap_or(0),
                top: Vec::new(),
            })
            .collect();
        nodes.sort_by(|a, b| b.degree.cmp(&a.degree).then_with(|| a.id.cmp(&b.id)));
        let total_nodes = nodes.len();
        let truncated = total_nodes > limit;
        nodes.truncate(limit);
        let kept: std::collections::HashSet<&str> =
            nodes.iter().map(|node| node.id.as_str()).collect();
        let mut edges: Vec<OverviewEdge> = slice
            .edges
            .iter()
            .filter(|edge| {
                kept.contains(edge.from_id.as_str()) && kept.contains(edge.to_id.as_str())
            })
            .map(|edge| OverviewEdge {
                from: edge.from_id.clone(),
                to: edge.to_id.clone(),
                kind: edge.kind,
                count: 1,
            })
            .collect();
        edges.sort_by(|a, b| {
            a.from
                .cmp(&b.from)
                .then_with(|| a.to.cmp(&b.to))
                .then_with(|| a.kind.as_str().cmp(b.kind.as_str()))
        });
        return GraphOverview {
            level,
            nodes,
            edges,
            total_nodes,
            truncated,
            total_symbols,
            sampled_symbols,
        };
    }

    let mut members: BTreeMap<String, Vec<&Symbol>> = BTreeMap::new();
    let mut module_of_symbol: HashMap<&str, String> = HashMap::new();
    for symbol in &slice.nodes {
        let module = module_of(&symbol.file, level);
        module_of_symbol.insert(symbol.id.as_str(), module.clone());
        members.entry(module).or_default().push(symbol);
    }

    let mut weight: HashMap<&str, usize> = HashMap::new();
    let mut bundles: HashMap<(&str, &str, &'static str), (EdgeKind, usize)> = HashMap::new();
    for edge in &slice.edges {
        let (Some(from), Some(to)) = (
            module_of_symbol.get(edge.from_id.as_str()),
            module_of_symbol.get(edge.to_id.as_str()),
        ) else {
            continue;
        };
        if from == to {
            continue;
        }
        // Only cross-module edges are drawn, so only they carry a module's
        // weight.
        *weight.entry(from.as_str()).or_default() += 1;
        *weight.entry(to.as_str()).or_default() += 1;
        let row = bundles
            .entry((from.as_str(), to.as_str(), edge.kind.as_str()))
            .or_insert((edge.kind, 0));
        row.1 += 1;
    }

    let mut nodes: Vec<OverviewNode> = members
        .into_iter()
        .map(|(id, mut members)| {
            members.sort_by(|a, b| {
                let a_degree = degree.get(a.id.as_str()).copied().unwrap_or(0);
                let b_degree = degree.get(b.id.as_str()).copied().unwrap_or(0);
                b_degree.cmp(&a_degree).then_with(|| a.id.cmp(&b.id))
            });
            let top = members
                .iter()
                .take(OVERVIEW_MODULE_SYMBOLS)
                .map(|symbol| GraphNode {
                    symbol: project(symbol),
                    degree: degree.get(symbol.id.as_str()).copied().unwrap_or(0),
                })
                .collect();
            let label = module_label(&id);
            OverviewNode {
                file: id.clone(),
                symbols: members.len(),
                degree: weight.get(id.as_str()).copied().unwrap_or(0),
                label,
                top,
                id,
                kind: None,
                line: 0,
            }
        })
        .collect();
    nodes.sort_by(|a, b| {
        b.symbols
            .cmp(&a.symbols)
            .then_with(|| b.degree.cmp(&a.degree))
            .then_with(|| a.id.cmp(&b.id))
    });
    let total_nodes = nodes.len();
    let truncated = total_nodes > limit;
    nodes.truncate(limit);

    let kept: std::collections::HashSet<&str> = nodes.iter().map(|node| node.id.as_str()).collect();
    let mut edges: Vec<OverviewEdge> = bundles
        .into_iter()
        .filter(|((from, to, _), _)| kept.contains(from) && kept.contains(to))
        .map(|((from, to, _), (kind, count))| OverviewEdge {
            from: from.to_string(),
            to: to.to_string(),
            kind,
            count,
        })
        .collect();
    edges.sort_by(|a, b| {
        a.from
            .cmp(&b.from)
            .then_with(|| a.to.cmp(&b.to))
            .then_with(|| a.kind.as_str().cmp(b.kind.as_str()))
    });

    GraphOverview {
        level,
        nodes,
        edges,
        total_nodes,
        truncated,
        total_symbols,
        sampled_symbols,
    }
}

/// The open project's engine, or a clear "no project" error.
///
/// A local copy of `graph.rs`'s helper: that module belongs to another change
/// right now, and ten duplicated lines beat a cross-plan edit of it.
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

/// Run a read-only view query on the blocking pool (see `graph.rs`).
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

/// The whole-graph overview: the degree-ranked graph slice aggregated into
/// directory modules at `level` (`0` = the slice symbol by symbol).
///
/// A project running with `index_codegraph: false` is a plain error string the
/// tab renders, exactly like the other `graph_*` commands.
#[tauri::command]
pub async fn graph_overview(
    state: State<'_, Mnemo2State>,
    level: Option<usize>,
    limit: Option<usize>,
) -> Result<GraphOverview, String> {
    let graph = graph_handle(&state).await?;
    let level = level.unwrap_or(OVERVIEW_DEFAULT_LEVEL);
    let limit = limit.unwrap_or(OVERVIEW_DEFAULT_LIMIT);
    run_view(graph, move |view| Ok(overview_in(view, level, limit))).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use mnemo::codegraph::store::EdgeRow;

    fn sym(file: &str, name: &str, kind: SymbolKind, line: usize) -> Symbol {
        Symbol {
            id: format!("{file}::{name}::{line}"),
            name: name.to_string(),
            kind,
            file: file.to_string(),
            start_line: line,
            end_line: line,
        }
    }

    fn edge(from: &Symbol, to: &Symbol, kind: EdgeKind) -> EdgeRow {
        EdgeRow {
            from_id: from.id.clone(),
            to_id: to.id.clone(),
            kind,
        }
    }

    struct Fixture {
        run: Symbol,
        start: Symbol,
        save: Symbol,
        stats: Symbol,
        view: GraphView,
    }

    /// Five modules over seven symbols: `src/agent` and `src/memory` talk to
    /// each other (two kinds each way), `src/codegraph` and `src` sit one
    /// directory shallower, and a root-level `build.rs` keeps the `"."`
    /// module covered.
    fn fixture() -> Fixture {
        let run = sym("src/agent/turn.rs", "run_turn", SymbolKind::Function, 812);
        let start = sym("src/agent/mod.rs", "start_agent", SymbolKind::Function, 96);
        let save = sym("src/memory/store.rs", "save", SymbolKind::Method, 30);
        let stats = sym("src/memory/types.rs", "Stats", SymbolKind::Struct, 12);
        let slice = sym("src/codegraph/query.rs", "slice", SymbolKind::Method, 382);
        let main = sym("src/main.rs", "main", SymbolKind::Function, 1);
        let build = sym("build.rs", "build", SymbolKind::Function, 1);
        let view = GraphView::from_parts(
            vec![
                run.clone(),
                start.clone(),
                save.clone(),
                stats.clone(),
                slice.clone(),
                main.clone(),
                build.clone(),
            ],
            vec![
                edge(&run, &save, EdgeKind::Calls),
                edge(&run, &stats, EdgeKind::Imports),
                // Inside one module: aggregated away, never drawn.
                edge(&start, &run, EdgeKind::Calls),
                edge(&save, &start, EdgeKind::Calls),
                edge(&slice, &run, EdgeKind::Calls),
                edge(&main, &slice, EdgeKind::Calls),
                edge(&build, &main, EdgeKind::Calls),
            ],
        );
        Fixture {
            run,
            start,
            save,
            stats,
            view,
        }
    }

    fn node<'a>(overview: &'a GraphOverview, id: &str) -> &'a OverviewNode {
        overview
            .nodes
            .iter()
            .find(|node| node.id == id)
            .unwrap_or_else(|| panic!("no node {id} in {:?}", overview.nodes))
    }

    #[test]
    fn modules_of_a_file_at_each_level() {
        assert_eq!(module_of("mnemo2/src/lib/graph.ts", 1), "mnemo2");
        assert_eq!(module_of("mnemo2/src/lib/graph.ts", 2), "mnemo2/src");
        assert_eq!(module_of("mnemo2/src/lib/graph.ts", 3), "mnemo2/src/lib");
        // A shallow file keeps its deepest directory at every level.
        assert_eq!(module_of("src/main.rs", 3), "src");
        // A root-level file has no directory at all.
        assert_eq!(module_of("build.rs", 2), ".");
        assert_eq!(module_label("."), "root");
        assert_eq!(module_label("src/codegraph"), "codegraph");
        assert_eq!(module_label("src"), "src");
    }

    #[test]
    fn overview_aggregates_the_slice_into_directory_modules() {
        let fixture = fixture();
        let overview = overview_in(&fixture.view, 2, 60);

        assert_eq!(overview.level, 2);
        assert_eq!(overview.total_symbols, 7);
        assert_eq!(overview.sampled_symbols, 7);
        assert!(!overview.truncated);

        let ids: Vec<&str> = overview.nodes.iter().map(|node| node.id.as_str()).collect();
        assert_eq!(
            ids,
            ["src/agent", "src/memory", "src", "src/codegraph", "."],
            "modules rank by symbol count, then degree, then id"
        );
        assert_eq!(node(&overview, "src/agent").symbols, 2);
        assert_eq!(node(&overview, "src/agent").label, "agent");
        assert_eq!(node(&overview, "src/agent").kind, None);
        assert_eq!(node(&overview, "src/agent").line, 0);
        // Cross-module weight only: start→run stays inside src/agent.
        assert_eq!(node(&overview, "src/agent").degree, 4);
        assert_eq!(node(&overview, "src/memory").degree, 3);
        assert_eq!(node(&overview, "src").symbols, 1);
        assert_eq!(node(&overview, ".").label, "root");
    }

    #[test]
    fn overview_merges_symbol_edges_into_counted_bundles() {
        let fixture = fixture();
        let overview = overview_in(&fixture.view, 2, 60);

        let rows: Vec<(&str, &str, &str, usize)> = overview
            .edges
            .iter()
            .map(|edge| {
                (
                    edge.from.as_str(),
                    edge.to.as_str(),
                    edge.kind.as_str(),
                    edge.count,
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                (".", "src", "calls", 1),
                ("src", "src/codegraph", "calls", 1),
                ("src/agent", "src/memory", "calls", 1),
                ("src/agent", "src/memory", "imports", 1),
                ("src/codegraph", "src/agent", "calls", 1),
                ("src/memory", "src/agent", "calls", 1),
            ],
            "both kinds between the same modules are kept apart, with counts"
        );
    }

    #[test]
    fn overview_counts_repeated_module_edges() {
        let fixture = fixture();
        // Two calls from src/agent into src/memory share one bundle.
        let view = GraphView::from_parts(
            vec![
                fixture.run.clone(),
                fixture.start.clone(),
                fixture.save.clone(),
                fixture.stats.clone(),
            ],
            vec![
                edge(&fixture.run, &fixture.save, EdgeKind::Calls),
                edge(&fixture.start, &fixture.stats, EdgeKind::Calls),
            ],
        );
        let overview = overview_in(&view, 2, 60);
        assert_eq!(overview.edges.len(), 1);
        let bundle = &overview.edges[0];
        assert_eq!(
            (bundle.from.as_str(), bundle.to.as_str(), bundle.kind),
            ("src/agent", "src/memory", EdgeKind::Calls)
        );
        assert_eq!(bundle.count, 2);
    }

    #[test]
    fn overview_caps_modules_and_keeps_only_edges_between_kept_nodes() {
        let fixture = fixture();
        let overview = overview_in(&fixture.view, 2, 2);

        assert_eq!(overview.total_nodes, 5);
        assert!(overview.truncated);
        let ids: Vec<&str> = overview.nodes.iter().map(|node| node.id.as_str()).collect();
        assert_eq!(ids, ["src/agent", "src/memory"]);
        assert!(
            overview
                .edges
                .iter()
                .all(|edge| ids.contains(&edge.from.as_str()) && ids.contains(&edge.to.as_str())),
            "an edge to a dropped module must not dangle"
        );
        assert_eq!(overview.edges.len(), 3);
    }

    #[test]
    fn overview_carries_each_module_top_symbols_by_degree() {
        let fixture = fixture();
        let overview = overview_in(&fixture.view, 2, 60);
        let agent = node(&overview, "src/agent");
        let top: Vec<&str> = agent
            .top
            .iter()
            .map(|node| node.symbol.id.as_str())
            .collect();
        assert_eq!(
            top,
            [fixture.run.id.as_str(), fixture.start.id.as_str()],
            "highest degree first, id as the tiebreak"
        );
        assert_eq!(agent.top[0].degree, 4);
        assert_eq!(agent.top[0].symbol.name, "run_turn");
        assert_eq!(agent.top[0].symbol.kind, SymbolKind::Function);
        assert_eq!(agent.top[0].symbol.line, 812);
        assert_eq!(node(&overview, "src/memory").top[0].symbol.id, fixture.save.id);
    }

    #[test]
    fn overview_at_level_zero_is_the_degree_ranked_symbol_slice() {
        let fixture = fixture();
        let overview = overview_in(&fixture.view, 0, 60);

        assert_eq!(overview.level, 0);
        assert_eq!(overview.nodes.len(), 7);
        assert_eq!(overview.nodes[0].id, fixture.run.id);
        assert_eq!(overview.nodes[0].degree, 4);
        assert_eq!(overview.nodes[0].kind, Some(SymbolKind::Function));
        assert_eq!(overview.nodes[0].label, "run_turn");
        assert_eq!(overview.nodes[0].file, "src/agent/turn.rs");
        assert_eq!(overview.nodes[0].line, 812);
        assert!(overview.nodes[0].top.is_empty());
        assert!(overview.edges.iter().all(|edge| edge.count == 1));
        assert_eq!(
            overview.edges.len(),
            7,
            "every slice edge whose endpoints are drawn"
        );
    }

    #[test]
    fn overview_clamps_level_and_limit() {
        let fixture = fixture();
        assert_eq!(
            overview_in(&fixture.view, 9, 60).level,
            OVERVIEW_MAX_LEVEL
        );
        let capped = overview_in(&fixture.view, 2, 0);
        assert_eq!(capped.nodes.len(), 1, "a zero limit still draws one node");
        assert!(capped.truncated);
        let wide = overview_in(&fixture.view, 2, 10_000);
        assert_eq!(wide.nodes.len(), 5, "limit is clamped, not rejected");
        assert!(!wide.truncated);
    }

    #[test]
    fn empty_graph_overview_is_empty_not_an_error() {
        let view = GraphView::from_parts(Vec::new(), Vec::new());
        let overview = overview_in(&view, OVERVIEW_DEFAULT_LEVEL, OVERVIEW_DEFAULT_LIMIT);
        assert!(overview.nodes.is_empty());
        assert!(overview.edges.is_empty());
        assert_eq!(overview.total_nodes, 0);
        assert_eq!(overview.total_symbols, 0);
        assert!(!overview.truncated);
    }

    #[test]
    fn overview_is_deterministic() {
        let fixture = fixture();
        assert_eq!(
            overview_in(&fixture.view, 2, 60),
            overview_in(&fixture.view, 2, 60)
        );
    }
}
