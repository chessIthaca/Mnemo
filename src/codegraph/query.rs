// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The CodeGraph query engine — the "relational intelligence" layer.
//!
//! Queries run against a [`GraphView`]: an in-memory snapshot of the whole
//! graph (symbols + edges) loaded from the [`Store`] in two SQL queries. A
//! repo-scale graph is on the order of 10⁴ symbols, so a per-query snapshot
//! costs milliseconds and can never go stale mid-query — no cache
//! invalidation, no lock held across a traversal.
//!
//! Four query shapes, mirroring GitNexus's smart tools:
//!
//! - [`GraphView::resolve`] — name → candidate symbols (exact, then
//!   case-insensitive, then substring; capped).
//! - [`GraphView::context`] — the 360° view of one symbol: its definition plus
//!   incoming/outgoing edges grouped by kind.
//! - [`GraphView::impact`] — blast radius: the reverse transitive closure
//!   ("who breaks if I change this?"), grouped by depth.
//! - [`GraphView::path`] — BFS shortest directed path between two symbols,
//!   with the edge kind of each hop.
//!
//! All return plain serde-`Serialize` structs so the agent tools and IPC
//! layer can hand them straight to `serde_json`.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use serde::Serialize;

use crate::codegraph::extract::{EdgeKind, Symbol, SymbolKind};
use crate::codegraph::store::{EdgeRow, Store};
use crate::error::Result;

/// Maximum candidates returned by [`GraphView::resolve`].
pub const RESOLVE_CAP: usize = 20;

/// Maximum neighbors listed per edge-kind group in [`GraphView::context`]
/// (the totals fields still report the true counts).
pub const CONTEXT_GROUP_CAP: usize = 50;

/// The 360° view of one symbol: its definition, plus the symbols it points
/// to (`outgoing`) and that point at it (`incoming`), grouped by edge-kind
/// string (`"calls"` / `"imports"` / `"contains"`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ContextView {
    /// The symbol the query was about.
    pub symbol: Symbol,
    /// Edge kind → symbols this symbol points at (capped per group).
    pub outgoing: BTreeMap<String, Vec<Symbol>>,
    /// Edge kind → symbols that point at this symbol (capped per group).
    pub incoming: BTreeMap<String, Vec<Symbol>>,
    /// True total of outgoing edges (may exceed the listed rows).
    pub outgoing_total: usize,
    /// True total of incoming edges (may exceed the listed rows).
    pub incoming_total: usize,
}

/// One depth ring of an impact query.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DepthGroup {
    /// Hop distance from the root (1 = direct dependents).
    pub depth: usize,
    /// Symbols at exactly this depth, sorted by id for determinism.
    pub symbols: Vec<Symbol>,
}

/// Blast radius of a symbol: everything that transitively depends on it,
/// grouped by hop distance.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ImpactView {
    /// The symbol the query was about.
    pub root: Symbol,
    /// Depth rings, nearest first; empty when nothing depends on `root`.
    pub depths: Vec<DepthGroup>,
    /// Total distinct affected symbols across all rings.
    pub total: usize,
}

/// One node on a shortest path, with the kind of edge leading to the next
/// node (`None` on the final node).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PathHop {
    /// The symbol at this hop.
    pub symbol: Symbol,
    /// Edge kind from this hop to the next; `None` when this is the target.
    pub edge_to_next: Option<EdgeKind>,
}

/// Result of a path query. `found = false` (with empty `hops`) means no
/// directed path exists or an endpoint id is unknown — never an error.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PathView {
    /// Whether a directed path was found.
    pub found: bool,
    /// The path from source to target, in order, when found.
    pub hops: Vec<PathHop>,
}

/// An in-memory snapshot of the whole graph. Build with [`GraphView::load`]
/// (from the store) or [`GraphView::from_parts`] (tests / IPC assembly).
pub struct GraphView {
    symbols: Vec<Symbol>,
    /// id → index into `symbols`.
    by_id: HashMap<String, usize>,
    /// index → (neighbor index, edge kind) for edges leaving the symbol.
    out_edges: Vec<Vec<(usize, EdgeKind)>>,
    /// index → (neighbor index, edge kind) for edges entering the symbol.
    in_edges: Vec<Vec<(usize, EdgeKind)>>,
}

/// Maximum nodes returned by an unfiltered [`GraphView::slice`] (top-N by
/// degree). A repo-scale graph is far too big to render at once; the Graph
/// tab shows the most-connected symbols first and lets the user focus from
/// there.
pub const GRAPH_SLICE_CAP: usize = 300;

/// A subgraph selected for visualization: the chosen nodes plus every edge
/// whose BOTH endpoints are among them (edges to unselected nodes are
/// dropped, so the slice is self-contained). The IPC command unpacks this
/// into `CodegraphGraph` field-by-field — the slice itself is never
/// serialized (review F4; `EdgeRow` is serialized on its own).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GraphSlice {
    /// The selected symbols (deterministic order — id-sorted).
    pub nodes: Vec<Symbol>,
    /// Edges between the selected symbols only.
    pub edges: Vec<EdgeRow>,
}

/// How [`GraphView::slice`] selects nodes. All filters are combined with AND
/// when several are present; the result is always capped at
/// [`GRAPH_SLICE_CAP`] nodes.
#[derive(Debug, Clone, Default)]
pub struct GraphSliceFilter<'a> {
    /// Keep only symbols whose name contains this substring
    /// (case-insensitive). `None` = no name restriction.
    pub name: Option<&'a str>,
    /// Keep only symbols of this kind ([`SymbolKind::as_str`] form, e.g.
    /// `"function"`, `"struct"`). `None` = any kind.
    pub kind: Option<&'a str>,
    /// Keep the symbol with this id plus its neighborhood out to `depth`
    /// hops (both edge directions — a focused subgraph needs callers AND
    /// callees). Unknown id → `None` (the caller reports "not found").
    pub from_symbol: Option<&'a str>,
    /// Neighborhood radius for `from_symbol` (default 1 when `None`).
    pub depth: Option<usize>,
}

impl GraphView {
    /// Snapshot the store into memory. Edges referencing unknown symbol ids
    /// (possible only if a rebuild raced a read) are skipped defensively.
    pub fn load(store: &Store) -> Result<Self> {
        let symbols = store.all_symbols()?;
        let edges = store.all_edges()?;
        Ok(Self::from_parts(symbols, edges))
    }

    /// Build a view directly from parts. Edge endpoints that don't resolve to
    /// a symbol id are dropped.
    pub fn from_parts(symbols: Vec<Symbol>, edges: Vec<EdgeRow>) -> Self {
        let by_id: HashMap<String, usize> = symbols
            .iter()
            .enumerate()
            .map(|(i, s)| (s.id.clone(), i))
            .collect();
        let mut out_edges = vec![Vec::new(); symbols.len()];
        let mut in_edges = vec![Vec::new(); symbols.len()];
        for e in edges {
            let (Some(&from), Some(&to)) = (by_id.get(&e.from_id), by_id.get(&e.to_id)) else {
                continue;
            };
            out_edges[from].push((to, e.kind));
            in_edges[to].push((from, e.kind));
        }
        Self {
            symbols,
            by_id,
            out_edges,
            in_edges,
        }
    }

    /// Number of symbols in the snapshot.
    pub fn symbol_count(&self) -> usize {
        self.symbols.len()
    }

    /// Resolve a name to candidate symbols: exact case-sensitive matches
    /// first, then exact case-insensitive, then case-insensitive substring
    /// (shortest names first). Capped at [`RESOLVE_CAP`]; empty on a blank or
    /// unmatched query — never an error.
    pub fn resolve(&self, query: &str) -> Vec<Symbol> {
        let query = query.trim();
        if query.is_empty() {
            return Vec::new();
        }
        let lower = query.to_lowercase();
        let mut exact: Vec<&Symbol> = Vec::new();
        let mut exact_ci: Vec<&Symbol> = Vec::new();
        let mut sub: Vec<&Symbol> = Vec::new();
        for s in &self.symbols {
            if s.name == query {
                exact.push(s);
            } else if s.name.eq_ignore_ascii_case(query) {
                exact_ci.push(s);
            } else if s.name.to_lowercase().contains(&lower) {
                sub.push(s);
            }
        }
        exact.sort_by(|a, b| a.id.cmp(&b.id));
        exact_ci.sort_by(|a, b| a.id.cmp(&b.id));
        sub.sort_by(|a, b| a.name.len().cmp(&b.name.len()).then(a.id.cmp(&b.id)));
        let mut out: Vec<Symbol> = Vec::new();
        for bucket in [exact, exact_ci, sub] {
            for s in bucket {
                if out.len() >= RESOLVE_CAP {
                    return out;
                }
                out.push(s.clone());
            }
        }
        out
    }

    /// The 360° view of one symbol, or `None` when the id is unknown.
    /// Neighbors are grouped by edge kind and sorted by id within each group;
    /// each group lists at most [`CONTEXT_GROUP_CAP`] rows (totals carry the
    /// true counts).
    pub fn context(&self, symbol_id: &str) -> Option<ContextView> {
        let idx = *self.by_id.get(symbol_id)?;
        let mut outgoing: BTreeMap<String, Vec<Symbol>> = BTreeMap::new();
        for (to, kind) in &self.out_edges[idx] {
            let group = outgoing.entry(kind.as_str().to_string()).or_default();
            if group.len() < CONTEXT_GROUP_CAP {
                group.push(self.symbols[*to].clone());
            }
        }
        let mut incoming: BTreeMap<String, Vec<Symbol>> = BTreeMap::new();
        for (from, kind) in &self.in_edges[idx] {
            let group = incoming.entry(kind.as_str().to_string()).or_default();
            if group.len() < CONTEXT_GROUP_CAP {
                group.push(self.symbols[*from].clone());
            }
        }
        for group in outgoing.values_mut().chain(incoming.values_mut()) {
            group.sort_by(|a, b| a.id.cmp(&b.id));
        }
        Some(ContextView {
            symbol: self.symbols[idx].clone(),
            outgoing_total: self.out_edges[idx].len(),
            incoming_total: self.in_edges[idx].len(),
            outgoing,
            incoming,
        })
    }

    /// Blast radius of `symbol_id`: every symbol that transitively depends on
    /// it (reverse traversal over incoming edges), grouped by hop distance
    /// from the root. `max_depth` bounds the traversal (`0` means unbounded).
    /// A visited-set makes cycles terminate; a symbol reached at a nearer
    /// depth is not repeated in a farther ring. Returns `None` when the id is
    /// unknown.
    pub fn impact(&self, symbol_id: &str, max_depth: usize) -> Option<ImpactView> {
        let root_idx = *self.by_id.get(symbol_id)?;
        let mut visited: HashSet<usize> = HashSet::from([root_idx]);
        let mut depths: Vec<DepthGroup> = Vec::new();
        let mut frontier: Vec<usize> = vec![root_idx];
        let mut depth = 0usize;
        while !frontier.is_empty() {
            depth += 1;
            if max_depth > 0 && depth > max_depth {
                break;
            }
            let mut next: HashSet<usize> = HashSet::new();
            for &idx in &frontier {
                for (from, _kind) in &self.in_edges[idx] {
                    if !visited.contains(from) {
                        next.insert(*from);
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            for &n in &next {
                visited.insert(n);
            }
            let mut symbols: Vec<Symbol> = next.iter().map(|&i| self.symbols[i].clone()).collect();
            symbols.sort_by(|a, b| a.id.cmp(&b.id));
            depths.push(DepthGroup { depth, symbols });
            frontier = next.into_iter().collect();
        }
        let total = depths.iter().map(|d| d.symbols.len()).sum();
        Some(ImpactView {
            root: self.symbols[root_idx].clone(),
            depths,
            total,
        })
    }

    /// BFS shortest directed path from `from_id` to `to_id` over all edge
    /// kinds. Same id on both ends is a zero-hop found path. `found: false`
    /// (empty hops) when an endpoint is unknown or no path exists — never an
    /// error.
    pub fn path(&self, from_id: &str, to_id: &str) -> PathView {
        let (Some(&start), Some(&goal)) = (self.by_id.get(from_id), self.by_id.get(to_id)) else {
            return PathView {
                found: false,
                hops: Vec::new(),
            };
        };
        if start == goal {
            return PathView {
                found: true,
                hops: vec![PathHop {
                    symbol: self.symbols[start].clone(),
                    edge_to_next: None,
                }],
            };
        }
        // prev[i] = (predecessor index, edge kind predecessor→i).
        let mut prev: HashMap<usize, (usize, EdgeKind)> = HashMap::new();
        let mut visited: HashSet<usize> = HashSet::from([start]);
        let mut queue: VecDeque<usize> = VecDeque::from([start]);
        let mut reached = false;
        while let Some(idx) = queue.pop_front() {
            if idx == goal {
                reached = true;
                break;
            }
            // Sort neighbors by id for a deterministic path among equals.
            let mut neighbors = self.out_edges[idx].clone();
            neighbors.sort_by(|a, b| self.symbols[a.0].id.cmp(&self.symbols[b.0].id));
            for (to, kind) in neighbors {
                if visited.insert(to) {
                    prev.insert(to, (idx, kind));
                    queue.push_back(to);
                }
            }
        }
        if !reached && !prev.contains_key(&goal) {
            return PathView {
                found: false,
                hops: Vec::new(),
            };
        }
        // Walk back from goal to start, then reverse.
        let mut chain: Vec<(usize, Option<EdgeKind>)> = vec![(goal, None)];
        let mut cursor = goal;
        while cursor != start {
            let Some(&(pred, kind)) = prev.get(&cursor) else {
                return PathView {
                    found: false,
                    hops: Vec::new(),
                };
            };
            chain.push((pred, Some(kind)));
            cursor = pred;
        }
        chain.reverse(); // built goal→start; flip to start→goal
                         // Each chain entry already carries the edge to its successor (it was
                         // annotated while walking back: prev[cursor] = (pred, edge pred→cursor)),
                         // so after the reversal the mapping is direct.
        let hops: Vec<PathHop> = chain
            .into_iter()
            .map(|(idx, edge_to_next)| PathHop {
                symbol: self.symbols[idx].clone(),
                edge_to_next,
            })
            .collect();
        PathView { found: true, hops }
    }

    /// Select a visualization subgraph (the Graph tab's `codegraph_graph`
    /// backend). See [`GraphSliceFilter`] for the filter semantics:
    /// `from_symbol` takes a both-directions neighborhood first (unknown id
    /// → `None`), name/kind filters then narrow it, and the result is capped
    /// at [`GRAPH_SLICE_CAP`] nodes — most-connected first (degree = in +
    /// out edges), id as the tiebreaker so the selection is deterministic.
    /// Edges between the selected nodes only are included.
    pub fn slice(&self, filter: &GraphSliceFilter<'_>) -> Option<GraphSlice> {
        // Candidate indices after the neighborhood + name/kind filters.
        let candidates: Vec<usize> = if let Some(root) = filter.from_symbol {
            let &start = self.by_id.get(root)?;
            let depth = filter.depth.unwrap_or(1).max(1);
            // Both-directions BFS from `start`, collecting every visited
            // node (the root included) out to `depth` hops.
            let mut visited: HashSet<usize> = HashSet::from([start]);
            let mut frontier: Vec<usize> = vec![start];
            for _ in 0..depth {
                let mut next: Vec<usize> = Vec::new();
                for &idx in &frontier {
                    for &(n, _) in self.out_edges[idx].iter().chain(self.in_edges[idx].iter()) {
                        if visited.insert(n) {
                            next.push(n);
                        }
                    }
                }
                if next.is_empty() {
                    break;
                }
                frontier = next;
            }
            visited.into_iter().collect()
        } else {
            (0..self.symbols.len()).collect()
        };
        // Name filter (case-insensitive substring).
        let mut candidates = candidates;
        if let Some(name) = filter.name {
            let lower = name.to_lowercase();
            candidates.retain(|&i| self.symbols[i].name.to_lowercase().contains(&lower));
        }
        // Kind filter (as_str form; an unknown kind string matches nothing).
        if let Some(kind) = filter.kind {
            let wanted = SymbolKind::from_str(kind);
            candidates.retain(|&i| Some(self.symbols[i].kind) == wanted);
        }
        // Cap: most-connected first, id as tiebreaker.
        if candidates.len() > GRAPH_SLICE_CAP {
            let degree = |i: usize| self.out_edges[i].len() + self.in_edges[i].len();
            candidates.sort_by(|&a, &b| {
                degree(b)
                    .cmp(&degree(a))
                    .then_with(|| self.symbols[a].id.cmp(&self.symbols[b].id))
            });
            candidates.truncate(GRAPH_SLICE_CAP);
        }
        let selected: HashSet<usize> = candidates.into_iter().collect();
        let mut nodes: Vec<Symbol> = selected.iter().map(|&i| self.symbols[i].clone()).collect();
        nodes.sort_by(|a, b| a.id.cmp(&b.id));
        // Edges between selected nodes only, deterministic order.
        let mut edges: Vec<EdgeRow> = Vec::new();
        for &i in &selected {
            for &(to, kind) in &self.out_edges[i] {
                if selected.contains(&to) {
                    edges.push(EdgeRow {
                        from_id: self.symbols[i].id.clone(),
                        to_id: self.symbols[to].id.clone(),
                        kind,
                    });
                }
            }
        }
        edges.sort_by(|a, b| {
            a.from_id
                .cmp(&b.from_id)
                .then(a.to_id.cmp(&b.to_id))
                // Kind tiebreak: a symbol pair can carry two edge kinds, and
                // the pre-sort order comes from a HashSet iteration — without
                // this the stable sort would preserve randomized relative
                // order run-to-run (review F2; mirrors the store's
                // `ORDER BY from_id, to_id, kind`).
                .then(a.kind.as_str().cmp(b.kind.as_str()))
        });
        Some(GraphSlice { nodes, edges })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegraph::extract::SymbolKind;

    /// Build a symbol with a line-derived id (`file::name::1`).
    fn sym(file: &str, name: &str, kind: SymbolKind) -> Symbol {
        Symbol {
            id: format!("{file}::{name}::1"),
            name: name.to_string(),
            kind,
            file: file.to_string(),
            start_line: 1,
            end_line: 5,
        }
    }

    fn edge(from: &Symbol, to: &Symbol, kind: EdgeKind) -> EdgeRow {
        EdgeRow {
            from_id: from.id.clone(),
            to_id: to.id.clone(),
            kind,
        }
    }

    /// Pure diamond dependency: `app` → `mid_a` → `leaf` and `app` → `mid_b`
    /// → `leaf` (no direct edge). Impact of `leaf` must group mid_* at depth 1
    /// and `app` at depth 2.
    fn diamond() -> (Vec<Symbol>, Vec<EdgeRow>) {
        let leaf = sym("lib.rs", "leaf", SymbolKind::Function);
        let mid_a = sym("a.rs", "mid_a", SymbolKind::Function);
        let mid_b = sym("b.rs", "mid_b", SymbolKind::Function);
        let app = sym("main.rs", "app", SymbolKind::Function);
        let symbols = vec![leaf.clone(), mid_a.clone(), mid_b.clone(), app.clone()];
        let edges = vec![
            edge(&mid_a, &leaf, EdgeKind::Calls),
            edge(&mid_b, &leaf, EdgeKind::Calls),
            edge(&app, &mid_a, EdgeKind::Calls),
            edge(&app, &mid_b, EdgeKind::Calls),
        ];
        (symbols, edges)
    }

    /// Diamond plus a direct `app → leaf` edge — the shortest-path fixture
    /// (BFS must prefer the one-hop path over the two-hop ones).
    fn diamond_with_shortcut() -> (Vec<Symbol>, Vec<EdgeRow>) {
        let (symbols, mut edges) = diamond();
        edges.push(edge(&symbols[3], &symbols[0], EdgeKind::Calls)); // app→leaf
        (symbols, edges)
    }

    #[test]
    fn resolve_prefers_exact_then_ci_then_substring() {
        let symbols = vec![
            sym("a.rs", "build", SymbolKind::Function),
            sym("b.rs", "Build", SymbolKind::Struct),
            sym("c.rs", "build_brain", SymbolKind::Function),
            sym("d.rs", "rebuild", SymbolKind::Function),
        ];
        let view = GraphView::from_parts(symbols, Vec::new());
        let r = view.resolve("build");
        assert_eq!(r.first().map(|s| s.name.as_str()), Some("build"));
        assert!(r.iter().any(|s| s.name == "Build"));
        assert!(r.iter().any(|s| s.name == "build_brain"));
        assert!(r.iter().any(|s| s.name == "rebuild"));
        // Substring bucket is sorted shortest-name-first.
        let sub: Vec<&str> = r
            .iter()
            .skip_while(|s| s.name == "build" || s.name == "Build")
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(sub.first(), Some(&"rebuild"));
    }

    #[test]
    fn resolve_unknown_and_blank_yield_empty_not_error() {
        let view = GraphView::from_parts(vec![sym("a.rs", "x", SymbolKind::Function)], Vec::new());
        assert!(view.resolve("nope").is_empty());
        assert!(view.resolve("").is_empty());
        assert!(view.resolve("   ").is_empty());
    }

    #[test]
    fn context_groups_edges_by_kind_both_directions() {
        let (symbols, edges) = diamond();
        let view = GraphView::from_parts(symbols, edges);
        let ctx = view.context("lib.rs::leaf::1").unwrap();
        assert_eq!(ctx.symbol.name, "leaf");
        let callers = ctx.incoming.get("calls").unwrap();
        assert_eq!(callers.len(), 2, "mid_a and mid_b call leaf (pure diamond)");
        assert_eq!(ctx.incoming_total, 2);
        assert!(ctx.outgoing.is_empty());
        assert_eq!(ctx.outgoing_total, 0);
        assert!(view.context("nope::nope::0").is_none());
    }

    #[test]
    fn impact_groups_by_depth_diamond() {
        let (symbols, edges) = diamond();
        let view = GraphView::from_parts(symbols, edges);
        let impact = view.impact("lib.rs::leaf::1", 0).unwrap();
        assert_eq!(impact.total, 3);
        assert_eq!(impact.depths.len(), 2);
        let d1: Vec<&str> = impact.depths[0]
            .symbols
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(d1, vec!["mid_a", "mid_b"], "depth 1 = direct dependents");
        let d2: Vec<&str> = impact.depths[1]
            .symbols
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(d2, vec!["app"], "depth 2 = transitive dependent");
    }

    #[test]
    fn impact_respects_max_depth() {
        let (symbols, edges) = diamond();
        let view = GraphView::from_parts(symbols, edges);
        let impact = view.impact("lib.rs::leaf::1", 1).unwrap();
        assert_eq!(impact.depths.len(), 1);
        assert_eq!(impact.total, 2, "only direct dependents within depth 1");
    }

    #[test]
    fn impact_terminates_on_cycles() {
        // A→B→A cycle plus a dependent C→A.
        let a = sym("a.rs", "fa", SymbolKind::Function);
        let b = sym("b.rs", "fb", SymbolKind::Function);
        let c = sym("c.rs", "fc", SymbolKind::Function);
        let edges = vec![
            edge(&a, &b, EdgeKind::Calls),
            edge(&b, &a, EdgeKind::Calls),
            edge(&c, &a, EdgeKind::Calls),
        ];
        let view = GraphView::from_parts(vec![a.clone(), b, c], edges);
        let impact = view.impact(&a.id, 0).unwrap();
        // Dependents of fa: fb (direct caller) and fc (direct caller). The
        // cycle must not produce an infinite walk or re-add fa.
        assert_eq!(impact.total, 2);
        assert!(impact
            .depths
            .iter()
            .flat_map(|d| d.symbols.iter())
            .all(|s| s.id != a.id));
    }

    #[test]
    fn path_finds_shortest_not_first() {
        // Direct edge app→leaf plus the long way app→mid_a→leaf: BFS must
        // return the one-hop path even though the long way is listed first.
        let (symbols, mut edges) = diamond_with_shortcut();
        edges.rotate_left(1); // reorder so traversal order favors the long way
        let view = GraphView::from_parts(symbols, edges);
        let p = view.path("main.rs::app::1", "lib.rs::leaf::1");
        assert!(p.found);
        assert_eq!(p.hops.len(), 2, "direct call = 2 nodes (app, leaf)");
        assert_eq!(p.hops[0].symbol.name, "app", "path starts at app");
        assert_eq!(p.hops[0].edge_to_next, Some(EdgeKind::Calls));
        assert_eq!(p.hops[1].symbol.name, "leaf", "path ends at leaf");
        assert_eq!(p.hops[1].edge_to_next, None);
    }

    #[test]
    fn path_reports_no_path_and_unknown_endpoints() {
        let a = sym("a.rs", "fa", SymbolKind::Function);
        let b = sym("b.rs", "fb", SymbolKind::Function);
        let view = GraphView::from_parts(vec![a.clone(), b.clone()], Vec::new());
        assert!(!view.path(&a.id, &b.id).found, "no edges → no path");
        assert!(!view.path("nope", &b.id).found, "unknown source");
        assert!(!view.path(&a.id, "nope").found, "unknown target");
        // Same node is a zero-hop found path.
        let same = view.path(&a.id, &a.id);
        assert!(same.found);
        assert_eq!(same.hops.len(), 1);
    }

    #[test]
    fn view_load_roundtrips_store() {
        // Integration with the real store: upsert two files, rebuild, load.
        use crate::codegraph::store::{FileData, Store};
        let mut store = Store::open_in_memory().unwrap();
        let helper = sym("lib.rs", "helper", SymbolKind::Function);
        let caller = sym("main.rs", "caller", SymbolKind::Function);
        store
            .upsert_file(
                "lib.rs",
                "h1",
                1,
                &FileData {
                    symbols: vec![helper.clone()],
                    refs: vec![],
                    contains: vec![],
                },
                "",
            )
            .unwrap();
        store
            .upsert_file(
                "main.rs",
                "h2",
                1,
                &FileData {
                    symbols: vec![caller.clone()],
                    refs: vec![(caller.id.clone(), "helper".to_string(), "call".to_string())],
                    contains: vec![],
                },
                "",
            )
            .unwrap();
        store.rebuild_edges().unwrap();
        let view = GraphView::load(&store).unwrap();
        assert_eq!(view.symbol_count(), 2);
        let ctx = view.context(&helper.id).unwrap();
        assert_eq!(ctx.incoming_total, 1);
    }

    /// ── slice (the Graph tab's selection backend) ────────────────────────

    #[test]
    fn slice_unfiltered_caps_at_most_connected() {
        // 400 isolated symbols + 1 hub: the cap keeps the hub (degree 3)
        // and drops the tail.
        let mut symbols = Vec::new();
        let mut edges = Vec::new();
        let hub = sym("hub.rs", "hub", SymbolKind::Function);
        for i in 0..3 {
            let leaf = sym(
                &format!("l{i}.rs"),
                &format!("leaf{i}"),
                SymbolKind::Function,
            );
            edges.push(edge(&leaf, &hub, EdgeKind::Calls));
            symbols.push(leaf);
        }
        symbols.push(hub);
        for i in 3..400 {
            symbols.push(sym(
                &format!("t{i}.rs"),
                &format!("tail{i}"),
                SymbolKind::Function,
            ));
        }
        let view = GraphView::from_parts(symbols, edges);
        let slice = view.slice(&GraphSliceFilter::default()).unwrap();
        assert_eq!(slice.nodes.len(), GRAPH_SLICE_CAP);
        // The hub survives (highest degree); every node has an id-sorted
        // position and the edge is included (both endpoints selected).
        assert!(slice.nodes.iter().any(|s| s.name == "hub"));
        assert_eq!(slice.edges.len(), 3, "calls-edges among the kept nodes");
    }

    #[test]
    fn slice_name_and_kind_filters_narrow() {
        let (symbols, edges) = diamond();
        let view = GraphView::from_parts(symbols, edges);
        // Name substring, case-insensitive.
        let f = GraphSliceFilter {
            name: Some("MID"),
            ..Default::default()
        };
        let s = view.slice(&f).unwrap();
        assert_eq!(s.nodes.len(), 2);
        assert!(s.nodes.iter().all(|n| n.name.contains("mid")));
        // Unknown kind string matches nothing (not an error).
        let f = GraphSliceFilter {
            kind: Some("nope"),
            ..Default::default()
        };
        assert_eq!(view.slice(&f).unwrap().nodes.len(), 0);
        // Combined: name + valid kind.
        let f = GraphSliceFilter {
            name: Some("mid"),
            kind: Some("function"),
            ..Default::default()
        };
        assert_eq!(view.slice(&f).unwrap().nodes.len(), 2);
    }

    #[test]
    fn slice_neighborhood_takes_both_directions_and_unknown_root_is_none() {
        // app → mid → leaf (diamond fixture): a depth-1 neighborhood of mid
        // must include app (caller, incoming) and leaf (callee, outgoing).
        let (symbols, edges) = diamond();
        let view = GraphView::from_parts(symbols, edges);
        let mid = view
            .resolve("mid_a")
            .into_iter()
            .next()
            .expect("mid_a resolves");
        let f = GraphSliceFilter {
            from_symbol: Some(&mid.id),
            ..Default::default()
        };
        let s = view.slice(&f).unwrap();
        let names: Vec<&str> = s.nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"app"), "incoming caller included");
        assert!(names.contains(&"leaf"), "outgoing callee included");
        assert!(names.contains(&"mid_a"), "the root included");
        // Unknown root id → None (the caller surfaces "not found").
        let f = GraphSliceFilter {
            from_symbol: Some("nope::nope::1"),
            ..Default::default()
        };
        assert!(view.slice(&f).is_none());
    }

    #[test]
    fn slice_edges_only_between_selected_nodes() {
        // Name-filter to mid_a + leaf: the mid_a→leaf edge is kept, but any
        // edge touching an unselected node (app) is dropped.
        let (symbols, edges) = diamond();
        let view = GraphView::from_parts(symbols, edges);
        let f = GraphSliceFilter {
            name: Some("mid_a"),
            ..Default::default()
        };
        let s = view.slice(&f).unwrap();
        assert_eq!(s.nodes.len(), 1);
        assert!(s.edges.is_empty(), "edges to unselected nodes dropped");
    }

    /// Review F2 regression: two edges sharing the same from/to ids but with
    /// different kinds must come out in kind order (calls < contains <
    /// imports, mirroring the store's `ORDER BY from_id, to_id, kind`).
    /// The pre-sort order comes from HashSet iteration (RandomState), so
    /// without the kind tiebreak the stable sort preserved randomized
    /// relative order run-to-run.
    #[test]
    fn slice_orders_same_pair_edges_by_kind() {
        let module = sym("m.rs", "m", SymbolKind::Module);
        let f_n = sym("f.rs", "f", SymbolKind::Function);
        // Same pair twice: module imports f AND (synthetically) calls f.
        // Insert in an order that would put imports first if unsorted by kind.
        let edges = vec![
            edge(&module, &f_n, EdgeKind::Imports),
            edge(&module, &f_n, EdgeKind::Calls),
        ];
        let view = GraphView::from_parts(vec![module, f_n], edges);
        let s = view.slice(&GraphSliceFilter::default()).unwrap();
        assert_eq!(s.edges.len(), 2);
        assert_eq!(
            s.edges.iter().map(|e| e.kind).collect::<Vec<_>>(),
            vec![EdgeKind::Calls, EdgeKind::Imports],
            "calls sorts before imports for the same node pair"
        );
    }
}
