// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `graph_*` agent tools — read-only access to the CodeGraph knowledge graph.
//!
//! Four tools, mirroring GitNexus's smart tools:
//!
//! - `graph_search` — name → candidate symbols (file + line + kind). The
//!   cheap "where is X defined?" lookup that replaces a grep chain. A
//!   non-empty result carries `next` — the exact `graph_context(id=...)`
//!   call for the first hit (plus `graph_impact` for blast radius) — so
//!   the follow-up chain needs no re-derivation (plan aff95a51: the
//!   under-chaining observed 2027-01 was a result-shape gap, not a
//!   prompt gap).
//! - `graph_context` — the 360° view of one symbol: definition + incoming
//!   and outgoing edges grouped by kind (callers, callees, imports,
//!   containment). One call = complete structural context.
//! - `graph_impact` — blast radius: everything that transitively depends on
//!   a symbol ("who breaks if I change this?"), grouped by hop distance.
//! - `graph_path` — shortest directed path between two symbols, with the
//!   edge kind of each hop ("how does A reach B?").
//!
//! All four are read-only (`SafetyLevel::AutoRun`) and degrade gracefully:
//! an unknown name yields a helpful empty result (with candidates when the
//! name is ambiguous), never an error — and every miss carries the shared
//! self-correcting miss hint: near-misses point at candidate ids, total
//! misses point at `search` (string literals are not the graph's territory).
//! Graph reads are SQLite-backed and run on the blocking pool so a query
//! never stalls the async executor.
//!
//! `graph_search` total misses are staleness-aware (backlog 95f21af0 — the
//! F10 pattern for the symbol index): the watcher's reindex is best-effort
//! and unlike the search tool there is no walk to fall back on, so a miss
//! first sweeps the indexed source files' mtimes (stats only), reindexes up
//! to `STALE_REINDEX_CAP` (32) stale files inline under the shared 500 ms
//! stale-reindex budget — the budget, not the ceiling, is the latency guard —
//! and re-resolves once from the fresh view, disclosing the side effect
//! ("reindexed N stale file(s)"); unrepaired staleness (above the ceiling, or
//! a pass already running) carries a staleness note instead. Files not yet in
//! the index (brand-new) remain watcher-dependent.
//!
//! Indexed languages: Rust (`.rs`) and TypeScript/TSX (`.ts`/`.tsx`). The
//! tool descriptions state this explicitly so the agent reaches for the
//! graph for frontend symbols too, not just Rust ones (2026-08-27 miss: a
//! frontend legend was located via grep + whole-file reads).

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::codegraph::query::{CONTEXT_GROUP_CAP, ContextView, GraphView, RESOLVE_CAP};
use crate::codegraph::CodeGraph;
use crate::provider::ToolSchema;
use crate::tool::agent::tool_contract;
use crate::tool::{SafetyLevel, Tool, ToolCategory, ToolResult};

/// Run a read-only graph query on the blocking pool and shape the outcome.
///
/// `ok` receives the loaded [`GraphView`] and returns the JSON payload on
/// success; store/load errors become `ToolResult::error` (a broken DB must
/// surface, unlike an unknown name, which is a normal empty result).
async fn run_query<F>(graph: Arc<CodeGraph>, ok: F) -> ToolResult
where
    F: FnOnce(&GraphView) -> Result<Value, String> + Send + 'static,
{
    let result = tokio::task::spawn_blocking(move || {
        let view = graph.view().map_err(|e| e.to_string())?;
        ok(&view)
    })
    .await;
    match result {
        Ok(Ok(payload)) => {
            let text =
                serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string());
            ToolResult::success(text)
        }
        Ok(Err(msg)) => ToolResult::error(msg),
        Err(e) => ToolResult::error(format!("graph query task failed: {e}")),
    }
}

/// Resolve a name-or-id argument to a symbol id: ids (containing `::`) are
/// validated directly; names go through [`GraphView::resolve`] (exact first).
fn resolve_id(view: &GraphView, name_or_id: &str) -> Option<String> {
    if name_or_id.contains("::") && view.context(name_or_id).is_some() {
        return Some(name_or_id.to_string());
    }
    view.resolve(name_or_id).into_iter().next().map(|s| s.id)
}

/// Render the compact inline answer for a delegated symbol hunt — the
/// `search`/`search_read` auto-delegation (backlog b804012f): instead of the
/// advisory symbol-hunt note, the search tool resolves the hunt against the
/// graph and prepends the answer itself, skipping the file walk.
///
/// One `def:` line per resolved name (definition, kind, line range); a
/// single-symbol hunt additionally lists its top callers/callees and the
/// `graph_context` pointer for the full 360° view. The FIRST LINE carries
/// the `SEARCH_NUDGE_MARK` substring ("is an indexed symbol") so the
/// steering metric keeps counting delegated answers, and the escape line
/// tells the model that re-issuing the same query gets the plain file
/// search. Best-effort: any store/view failure or a total resolution miss
/// returns `None` and the caller falls through to the normal search.
/// Exact-only resolution for the delegation path: an id form validates
/// directly; a name must match a symbol's name EXACTLY — the same semantics
/// as the advisory nudge's `CodeGraph::symbol_id`. Fuzzy substring
/// candidates (e.g. `update` → `update_index`) keep earning the advisory
/// alternation-absorption note instead of a delegated answer, keeping the
/// delegated fast-path as conservative as the steering it replaces.
fn resolve_exact(view: &GraphView, name: &str) -> Option<String> {
    if name.contains("::") && view.context(name).is_some() {
        return Some(name.to_string());
    }
    view.resolve(name)
        .into_iter()
        .find(|s| s.name == name)
        .map(|s| s.id)
}

pub(crate) fn symbol_delegation_block(graph: &CodeGraph, names: &[String]) -> Option<String> {
    let view = graph.view().ok()?;
    let mut resolved: Vec<(&str, ContextView)> = Vec::new();
    for name in names {
        if let Some(id) = resolve_exact(&view, name) {
            if let Some(ctx) = view.context(&id) {
                resolved.push((name.as_str(), ctx));
            }
        }
    }
    if resolved.is_empty() {
        return None;
    }
    const ESCAPE: &str = " (re-issue this exact search to get the plain file search instead):";
    let mut out = if resolved.len() == 1 {
        let (name, _) = resolved[0];
        format!("AUTO-DELEGATED to the code graph — '{name}' is an indexed symbol{ESCAPE}")
    } else {
        format!("AUTO-DELEGATED to the code graph — each hit below is an indexed symbol{ESCAPE}")
    };
    for (name, ctx) in &resolved {
        let s = &ctx.symbol;
        let def = format!(
            "{} ({}, lines {}-{})",
            s.id,
            s.kind.as_str(),
            s.start_line,
            s.end_line
        );
        if resolved.len() == 1 {
            out.push_str(&format!("\n  def: {def}"));
        } else {
            out.push_str(&format!("\n  '{name}' → {def}"));
        }
    }
    if resolved.len() == 1 {
        let ctx = &resolved[0].1;
        if let Some(line) = edge_summary(ctx, false) {
            out.push_str(&format!("\n  {line}"));
        }
        if let Some(line) = edge_summary(ctx, true) {
            out.push_str(&format!("\n  {line}"));
        }
        out.push_str(&format!(
            "\n  full 360° view: graph_context(id=\"{}\")",
            ctx.symbol.id
        ));
    } else {
        out.push_str("\n  (pass one id to graph_context for its 360° view)");
    }
    Some(out)
}

/// One `callers:`/`callees:` summary line from a [`ContextView`]'s grouped
/// edges: the first non-empty edge-kind group (BTreeMap order puts `calls`
/// first), up to three `name (file:line)` entries, then a `+N more` count.
fn edge_summary(ctx: &ContextView, outgoing: bool) -> Option<String> {
    let groups = if outgoing { &ctx.outgoing } else { &ctx.incoming };
    let (kind, rows) = groups.iter().find(|(_, rows)| !rows.is_empty())?;
    let role = if outgoing { "callees" } else { "callers" };
    let label = if kind == "calls" {
        role.to_string()
    } else {
        format!("{role} ({kind})")
    };
    let mut line = String::new();
    for (i, s) in rows.iter().take(3).enumerate() {
        if i > 0 {
            line.push_str(", ");
        }
        line.push_str(&format!("{} ({}:{})", s.name, s.file, s.start_line));
    }
    // rows is capped at CONTEXT_GROUP_CAP per group; when capped the
    // group's true length is unknown — the kind-total is the closest
    // signal for "how much more is here" (it can only over-count when
    // other edge kinds also exist, which still says: open the 360° view).
    let total = if outgoing {
        ctx.outgoing_total
    } else {
        ctx.incoming_total
    };
    let remainder = if rows.len() >= CONTEXT_GROUP_CAP {
        total.saturating_sub(3)
    } else {
        rows.len().saturating_sub(3)
    };
    if remainder > 0 {
        line.push_str(&format!(", +{remainder} more"));
    }
    Some(format!("{label}: {line}"))
}

/// The self-correcting miss hint shared by all four graph tools: a near-miss
/// (candidates exist) points at the candidates' ids; a total miss points at
/// `search` — the graph indexes symbol definitions only, so string literals
/// (tool names, config keys, log text) are never its territory. A silent
/// miss teaches the model nothing and the next lookup of the same kind burns
/// the same round-trip; the hint makes the failure self-correcting
/// in-context.
fn miss_hint(query: &str, candidates: usize) -> String {
    if candidates == 0 {
        format!(
            "No symbols matched '{query}'. The graph indexes symbol definitions \
             only — string literals (tool names, config keys, log text) are not \
             indexed; use `search` for those. For a partial symbol name, retry \
             with a shorter query."
        )
    } else {
        "no exact symbol; try one of the candidates' ids".to_string()
    }
}

/// Ceiling on graph_search's inline staleness repair (backlog 95f21af0 — the
/// F10 pattern for the symbol index, mirroring the search tool's cap): an
/// ADAPTIVE upper bound, NOT the latency guard itself. Within it the repair
/// runs under [`crate::codegraph::STALE_REINDEX_BUDGET`], whose pass stops
/// between files once the budget is spent, so a slow set degrades to the
/// plain miss instead of a slow lookup. A total miss with MORE stale files
/// than this (or a pass already running) serves the staleness note — the
/// watcher's next pass covers the rest. Raised 8 → 32 on 2026-09-26 to match
/// the content index's adaptive ceiling (commit 06276aa, backlog 9201704f):
/// the old 8 left a checkout-scale drift (61 files) unrepaired and this
/// plan's own regression-test symbol unindexed — accurate results matter
/// more than the note.
const STALE_REINDEX_CAP: usize = 32;

/// Shared arg shape for the single-symbol tools: either an exact `id` or a
/// `name` to resolve.
#[derive(Debug, Deserialize)]
struct SymbolArgs {
    /// The exact symbol id (`file::name::line`), e.g. from `graph_search`.
    id: Option<String>,
    /// A symbol name to look up (exact match preferred).
    name: Option<String>,
}

/// `graph_search` — resolve a name to candidate symbols with file + line.
pub struct GraphSearchTool {
    graph: Arc<CodeGraph>,
}

impl GraphSearchTool {
    /// Create the tool over the shared graph handle.
    pub fn new(graph: Arc<CodeGraph>) -> Self {
        Self { graph }
    }
}

#[async_trait]
impl Tool for GraphSearchTool {
    fn name(&self) -> &str {
        "graph_search"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::AutoRun
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "graph_search",
            format!(
                "{} MANDATORY first step when locating a symbol \
             definition — never read whole files or run grep chains to find \
             one. Find symbols (functions, structs, traits, classes, …) in \
             the project's code knowledge graph (Rust, TypeScript/TSX, JavaScript, \
             Python, Go, Java, C/C++, C#, Ruby, PHP, and HTML script blocks — \
             .rs/.ts/.tsx/.js and other source extensions) by name. Returns up \
             to 20 candidates with file, line range, kind, and the exact symbol \
             id to pass to graph_context / graph_impact / graph_path. Indexes \
             symbol definitions only — string literals (tool names, config \
                 keys, log text) are not indexed; use the `search` tool for those.",
                tool_contract::contract("`query`", "{\"query\":\"DeltaAccumulator\"}")
            ),
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "The symbol name to search for (exact matches first, then case-insensitive, then substring)."}
                },
                "required": ["query"]
            }),
        )
    }

    async fn execute(&self, args: Value) -> ToolResult {
        #[derive(Deserialize)]
        struct Args {
            query: String,
        }
        let args: Args = match serde_json::from_value(args.clone()) {
            Ok(a) => a,
            // Backlog d9ad618e: the recovery rule rides the error (the
            // read_files precedent, backlog 26cdbaf8).
            Err(e) => {
                return ToolResult::error(crate::tool::agent::read_files::invalid_args_error(
                    "graph_search",
                    &e,
                    &args,
                    &tool_contract::recovery_hint("query"),
                ))
            }
        };
        // Freshness-aware query (backlog 95f21af0 — the F10 pattern for the
        // symbol index): a TOTAL miss may be staleness, not absence — the
        // watcher's reindex is best-effort, and unlike the search tool
        // there is no walk to fall back on. Detect stale indexed source
        // files (stats only), reindex up to STALE_REINDEX_CAP inline under
        // the shared stale-reindex budget, and re-resolve ONCE from the
        // fresh view. Best-effort: any error in
        // the freshness path serves the plain miss — a lookup is never
        // failed by its own repair.
        let graph = self.graph.clone();
        let result = tokio::task::spawn_blocking(move || -> Result<Value, String> {
            let view = graph.view().map_err(|e| e.to_string())?;
            let mut matches = view.resolve(&args.query);
            let mut reindexed: Option<usize> = None;
            let mut unrepaired_stale: Option<usize> = None;
            if matches.is_empty() {
                // Best-effort sweep: an error here leaves the plain miss.
                if let Ok(stale) = graph.stale_source_files() {
                    if !stale.is_empty() {
                        if stale.len() <= STALE_REINDEX_CAP {
                            // Ok(0) = a pass was already running (the
                            // indexing flag was claimed) — it will refresh
                            // the index shortly; Err = the reindex itself
                            // failed. Both leave the staleness unrepaired.
                            let repaired = graph
                                .reindex_stale_files(&stale, crate::codegraph::STALE_REINDEX_BUDGET)
                                .ok()
                                .filter(|n| *n > 0);
                            match repaired {
                                Some(n) => {
                                    // Re-resolve ONCE from the fresh view.
                                    // A view failure here must not fail the
                                    // lookup (best-effort invariant) — the
                                    // reindex side effect still happened and
                                    // is disclosed below; serve the plain
                                    // miss instead.
                                    if let Ok(fresh) = graph.view() {
                                        matches = fresh.resolve(&args.query);
                                    }
                                    reindexed = Some(n);
                                }
                                None => unrepaired_stale = Some(stale.len()),
                            }
                        } else {
                            unrepaired_stale = Some(stale.len());
                        }
                    }
                }
            }
            // A silent miss teaches the model nothing — the next lookup of the
            // same kind burns the same round-trip. The shared miss_hint makes
            // the failure self-correcting in-context: string literals go to
            // `search`, partial symbol names retry shorter.
            let hint = matches
                .is_empty()
                .then(|| miss_hint(&args.query, matches.len()));
            // The chaining pointer (plan aff95a51): the result is where the
            // follow-up decision happens, so a hit carries the exact next
            // call. Captured before the json! below moves `matches`.
            let first_id = matches.first().map(|s| s.id.clone());
            let mut out = json!({
                "query": args.query,
                "count": matches.len(),
                "symbols": matches,
            });
            if let Some(id) = first_id {
                out["next"] = json!(format!(
                    "graph_context(id=\"{id}\") → callers/callees/imports; \
                     graph_impact(id) → blast radius"
                ));
            }
            if let Some(hint) = hint {
                out["hint"] = json!(hint);
            }
            // The reindex side effect is ALWAYS disclosed — a read tool
            // mutated the index (the same disclosure rule as the search
            // tool's F10 note).
            if let Some(n) = reindexed {
                out["note"] = json!(format!(
                    "reindexed {n} stale file(s) — serving fresh graph results"
                ));
            } else if let Some(n) = unrepaired_stale {
                out["note"] = json!(format!(
                    "symbol index may be stale — {n} file(s) on disk are newer than the index"
                ));
            }
            Ok(out)
        })
        .await;
        match result {
            Ok(Ok(payload)) => {
                let text =
                    serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string());
                ToolResult::success(text)
            }
            Ok(Err(msg)) => ToolResult::error(msg),
            Err(e) => ToolResult::error(format!("graph query task failed: {e}")),
        }
    }
}

/// `graph_context` — the 360° view of one symbol.
pub struct GraphContextTool {
    graph: Arc<CodeGraph>,
}

impl GraphContextTool {
    /// Create the tool over the shared graph handle.
    pub fn new(graph: Arc<CodeGraph>) -> Self {
        Self { graph }
    }
}

#[async_trait]
impl Tool for GraphContextTool {
    fn name(&self) -> &str {
        "graph_context"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::AutoRun
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "graph_context",
            format!(
                "{} Get the 360° view of a symbol: its \
             definition (file + lines) plus incoming and outgoing edges grouped \
             by kind — callers, callees, imports, containment. Works the same \
             for every indexed language (Rust, TypeScript/TSX, JavaScript, \
                 Python, Go, Java, C/C++, C#, Ruby, PHP, and HTML script blocks). \
                 Call this immediately after graph_search whenever you need \
                 callers/callees/imports — one call replaces a grep chain and \
                 reading whole files.",
                tool_contract::contract(
                    "`id` (or `name`)",
                    "{\"id\":\"src/provider/stream.rs::DeltaAccumulator::52\"}"
                )
            ),
            json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "Exact symbol id (file::name::line) from graph_search."},
                    "name": {"type": "string", "description": "Symbol name to resolve (used when id is absent)."}
                }
            }),
        )
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let args: SymbolArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(crate::tool::error_message::sanitize_arguments_error(self.name(), &e)),
        };
        run_query(self.graph.clone(), move |view| {
            let key = args
                .id
                .clone()
                .or_else(|| args.name.clone())
                .unwrap_or_default();
            if key.is_empty() {
                // Backlog d9ad618e: the recovery rule rides the error (the
                // read_files precedent) — an empty call lands here, not on
                // the serde path (id/name are either-or by design).
                return Err(tool_contract::recovery_hint("`id` (or `name`)"));
            }
            let Some(id) = resolve_id(view, &key) else {
                let candidates = view.resolve(&key);
                let hint = miss_hint(&key, candidates.len());
                return Ok(json!({
                    "query": key,
                    "found": false,
                    "candidates": candidates,
                    "hint": hint,
                }));
            };
            let Some(ctx) = view.context(&id) else {
                return Ok(json!({ "query": key, "found": false }));
            };
            Ok(json!({
                "found": true,
                "context": ctx,
                "alternatives": view
                    .resolve(&ctx.symbol.name)
                    .into_iter()
                    .filter(|s| s.id != ctx.symbol.id)
                    .take(RESOLVE_CAP)
                    .collect::<Vec<_>>(),
            }))
        })
        .await
    }
}

/// `graph_impact` — blast radius of a symbol (reverse transitive closure).
pub struct GraphImpactTool {
    graph: Arc<CodeGraph>,
}

impl GraphImpactTool {
    /// Create the tool over the shared graph handle.
    pub fn new(graph: Arc<CodeGraph>) -> Self {
        Self { graph }
    }
}

#[async_trait]
impl Tool for GraphImpactTool {
    fn name(&self) -> &str {
        "graph_impact"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::AutoRun
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "graph_impact",
            "Compute the blast radius of a symbol: every symbol that transitively depends on it \
             (who breaks if this changes), grouped by hop distance. REQUIRED before editing any \
             shared or public symbol — skip it and you ship breaking changes blind. Pass an \
             exact `id` from graph_search, or a `name` to resolve.",
            json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "Exact symbol id (file::name::line) from graph_search."},
                    "name": {"type": "string", "description": "Symbol name to resolve (used when id is absent)."},
                    "max_depth": {"type": "integer", "description": "Maximum hop distance to traverse (default/0: unbounded)."}
                }
            }),
        )
    }

    async fn execute(&self, args: Value) -> ToolResult {
        #[derive(Deserialize)]
        struct Args {
            id: Option<String>,
            name: Option<String>,
            max_depth: Option<usize>,
        }
        let args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(crate::tool::error_message::sanitize_arguments_error(self.name(), &e)),
        };
        run_query(self.graph.clone(), move |view| {
            let key = args
                .id
                .clone()
                .or_else(|| args.name.clone())
                .unwrap_or_default();
            if key.is_empty() {
                return Err("either 'id' or 'name' is required".to_string());
            }
            let Some(id) = resolve_id(view, &key) else {
                let candidates = view.resolve(&key);
                let hint = miss_hint(&key, candidates.len());
                return Ok(json!({
                    "query": key,
                    "found": false,
                    "candidates": candidates,
                    "hint": hint,
                }));
            };
            let Some(impact) = view.impact(&id, args.max_depth.unwrap_or(0)) else {
                return Ok(json!({ "query": key, "found": false }));
            };
            Ok(json!({
                "found": true,
                "impact": impact,
            }))
        })
        .await
    }
}

/// `graph_path` — shortest directed path between two symbols.
pub struct GraphPathTool {
    graph: Arc<CodeGraph>,
}

impl GraphPathTool {
    /// Create the tool over the shared graph handle.
    pub fn new(graph: Arc<CodeGraph>) -> Self {
        Self { graph }
    }
}

#[async_trait]
impl Tool for GraphPathTool {
    fn name(&self) -> &str {
        "graph_path"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::AutoRun
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "graph_path",
            format!(
                "{} Find the shortest directed path between two symbols \
             in the code knowledge graph (e.g. how does `main` reach \
             `db_connect`?) — use for reachability questions ('can A reach \
             B', 'how does X get to Y'). Each hop lists the edge kind \
                 (calls/imports/contains). Accepts exact ids from graph_search \
                 or names to resolve.",
                tool_contract::contract(
                    "`from` and `to`",
                    "{\"from\":\"main\",\"to\":\"db_connect\"}"
                )
            ),
            json!({
                "type": "object",
                "properties": {
                    "from": {"type": "string", "description": "Source symbol id or name."},
                    "to": {"type": "string", "description": "Target symbol id or name."}
                },
                "required": ["from", "to"]
            }),
        )
    }

    async fn execute(&self, args: Value) -> ToolResult {
        #[derive(Deserialize)]
        struct Args {
            from: String,
            to: String,
        }
        let args: Args = match serde_json::from_value(args.clone()) {
            Ok(a) => a,
            // Backlog d9ad618e: the recovery rule rides the error (the
            // read_files precedent, backlog 26cdbaf8).
            Err(e) => {
                return ToolResult::error(crate::tool::agent::read_files::invalid_args_error(
                    "graph_path",
                    &e,
                    &args,
                    &tool_contract::recovery_hint("from and to"),
                ))
            }
        };
        run_query(self.graph.clone(), move |view| {
            let Some(from_id) = resolve_id(view, &args.from) else {
                let candidates = view.resolve(&args.from);
                let hint = miss_hint(&args.from, candidates.len());
                return Ok(json!({
                    "found": false,
                    "reason": "from symbol not found",
                    "candidates": candidates,
                    "hint": hint,
                }));
            };
            let Some(to_id) = resolve_id(view, &args.to) else {
                let candidates = view.resolve(&args.to);
                let hint = miss_hint(&args.to, candidates.len());
                return Ok(json!({
                    "found": false,
                    "reason": "to symbol not found",
                    "candidates": candidates,
                    "hint": hint,
                }));
            };
            let path = view.path(&from_id, &to_id);
            Ok(json!({
                "found": path.found,
                "path": path,
            }))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a CodeGraph over a fixture tree (helper in lib.rs, called from
    /// main.rs) and return it indexed.
    fn indexed_graph() -> (tempfile::TempDir, Arc<CodeGraph>) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn helper() -> u32 { 1 }\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("src/main.rs"),
            "use crate::lib::helper;\nfn main() { helper(); }\n",
        )
        .unwrap();
        let graph = Arc::new(CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap());
        graph.index(None).unwrap();
        (dir, graph)
    }

    /// Parse the JSON payload out of a successful ToolResult.
    fn payload(result: ToolResult) -> Value {
        assert!(result.success, "tool must succeed: {}", result.output);
        serde_json::from_str(&result.output).unwrap()
    }

    #[test]
    fn schemas_advertise_the_no_zero_argument_rule() {
        // Backlog d9ad618e (the read_files precedent, backlog 26cdbaf8): the
        // three graph tools each carry the rule + example + recovery rule.
        let (_dir, graph) = indexed_graph();
        for (name, schema) in [
            ("graph_search", GraphSearchTool::new(graph.clone()).schema()),
            ("graph_context", GraphContextTool::new(graph.clone()).schema()),
            ("graph_path", GraphPathTool::new(graph.clone()).schema()),
        ] {
            assert!(
                schema.description.contains("No zero-argument form"),
                "{name}: {}",
                schema.description
            );
            assert!(
                schema.description.contains("do not resend the empty shape"),
                "{name}: {}",
                schema.description
            );
            assert!(
                schema.description.contains("e.g. {"),
                "{name}: the inline example shows the exact call shape: {}",
                schema.description
            );
            assert!(
                schema.description.starts_with("Always pass"),
                "{name}: the contract sentence LEADS the description: {}",
                schema.description
            );
            assert!(
                !schema.description.contains("If you catch yourself"),
                "{name}: the content-first clause lives ONCE in the universal \
                 TOOL_CALL_DISCIPLINE block — a per-tool copy is exactly the \
                 redundancy this plan removed: {}",
                schema.description
            );
        }
    }

    #[tokio::test]
    async fn empty_call_errors_carry_the_recovery_hint() {
        // Backlog d9ad618e: an empty argument object errors with the recovery
        // rule riding the error itself, so the FIRST retry succeeds instead
        // of waiting for the circuit breaker.
        let (_dir, graph) = indexed_graph();
        for (name, result) in [
            (
                "graph_search",
                GraphSearchTool::new(graph.clone())
                    .execute(serde_json::json!({}))
                    .await,
            ),
            (
                "graph_context",
                GraphContextTool::new(graph.clone())
                    .execute(serde_json::json!({}))
                    .await,
            ),
            (
                "graph_path",
                GraphPathTool::new(graph.clone())
                    .execute(serde_json::json!({}))
                    .await,
            ),
        ] {
            assert!(!result.success, "{name} must reject an empty call");
            assert!(
                result.output.contains("rewrite the full call"),
                "{name}: {}",
                result.output
            );
            assert!(
                result.output.contains("do not resend the empty shape"),
                "{name}: {}",
                result.output
            );
        }
    }

    #[tokio::test]
    async fn search_hits_carry_the_graph_context_pointer() {
        // Plan aff95a51: the under-chaining observed 2027-01 — search found
        // the symbol but the graph_context follow-up never happened. The
        // result now carries the exact next call so the chain needs no
        // re-derivation; a total miss carries no pointer (its hint already
        // points at `search`).
        let (_dir, graph) = indexed_graph();
        let hit = payload(
            GraphSearchTool::new(graph.clone())
                .execute(serde_json::json!({"query": "helper"}))
                .await,
        );
        let next = hit["next"]
            .as_str()
            .unwrap_or_else(|| panic!("a non-empty result carries the chaining pointer: {hit}"));
        assert!(
            next.starts_with("graph_context(id=\"src/lib.rs::helper::"),
            "the pointer names the first hit's exact call: {next}"
        );
        assert!(
            next.contains("graph_impact(id) → blast radius"),
            "the pointer also names the blast-radius follow-up: {next}"
        );

        let miss = payload(
            GraphSearchTool::new(graph)
                .execute(serde_json::json!({"query": "nonexistent_xyz"}))
                .await,
        );
        assert!(
            miss.get("next").is_none(),
            "a total miss carries no pointer — its hint points at `search`: {miss}"
        );
    }

    #[test]
    fn delegation_block_renders_definition_callers_and_pointer() {
        // The search auto-delegation (backlog b804012f): a symbol hunt the
        // graph can answer gets the answer inline — definition, callers, the
        // graph_context pointer, the steering marker on the first line, and
        // the escape line.
        let (_dir, graph) = indexed_graph();
        let block =
            symbol_delegation_block(&graph, &["helper".to_string()]).expect("helper is indexed");
        assert!(
            block.starts_with("AUTO-DELEGATED to the code graph — 'helper' is an indexed symbol"),
            "{block}"
        );
        assert!(block.contains("re-issue this exact search"), "{block}");
        assert!(block.contains("def: src/lib.rs::helper::"), "{block}");
        assert!(
            block.contains("graph_context(id=\"src/lib.rs::helper::"),
            "{block}"
        );
        assert!(block.contains("callers:"), "{block}");
        assert!(block.contains("main"), "{block}");
    }

    #[test]
    fn delegation_block_none_for_unknown_name() {
        // A total resolution miss is not a delegation — the caller falls
        // through to the normal search (and the advisory nudge).
        let (_dir, graph) = indexed_graph();
        assert!(symbol_delegation_block(&graph, &["nonexistent_xyz".to_string()]).is_none());
    }

    #[test]
    fn delegation_block_alternation_lists_each_resolved_branch() {
        // Multi-name hunts render one def line per resolved branch; the
        // first line still carries the steering marker.
        let (_dir, graph) = indexed_graph();
        let block =
            symbol_delegation_block(&graph, &["helper".to_string(), "main".to_string()])
                .expect("both branches resolve");
        assert!(block.contains("each hit below is an indexed symbol"), "{block}");
        assert!(block.contains("'helper' → src/lib.rs::helper::"), "{block}");
        assert!(block.contains("'main' → src/main.rs::main::"), "{block}");
        assert!(!block.contains("def: "), "{block}");
    }

    #[test]
    fn delegation_block_single_survivor_of_an_alternation_uses_the_single_form() {
        // An alternation whose OTHER branches do not resolve degrades to the
        // single-symbol form (def + callers + pointer) for the one hit —
        // and never mentions the unresolved branches.
        let (_dir, graph) = indexed_graph();
        let block = symbol_delegation_block(
            &graph,
            &["helper".to_string(), "nonexistent_xyz".to_string()],
        )
        .expect("the resolving branch renders");
        assert!(
            block.starts_with("AUTO-DELEGATED to the code graph — 'helper' is an indexed symbol"),
            "{block}"
        );
        assert!(!block.contains("nonexistent_xyz"), "{block}");
    }

    #[test]
    fn schema_descriptions_state_language_coverage() {
        // The descriptions must name the indexed languages explicitly — the
        // 2026-08-27 session grep'd + whole-file-read frontend symbols
        // because nothing said the graph indexes .ts/.tsx too.
        let (_dir, graph) = indexed_graph();
        let search = GraphSearchTool::new(graph.clone());
        let context = GraphContextTool::new(graph);
        assert!(
            search.schema().description.contains("TypeScript"),
            "graph_search description names TypeScript coverage"
        );
        assert!(
            context.schema().description.contains("TypeScript"),
            "graph_context description names TypeScript coverage"
        );
        assert!(
            search.schema().description.contains("TypeScript/TSX"),
            "graph_search description states the extensions"
        );
    }

    #[tokio::test]
    async fn graph_search_finds_symbols_with_ids() {
        let (_dir, graph) = indexed_graph();
        let tool = GraphSearchTool::new(graph);
        let out = payload(tool.execute(json!({ "query": "helper" })).await);
        assert_eq!(out["count"], 1);
        let sym = &out["symbols"][0];
        assert_eq!(sym["name"], "helper");
        assert_eq!(sym["kind"], "function");
        assert!(sym["id"]
            .as_str()
            .unwrap()
            .starts_with("src/lib.rs::helper::"));
    }

    #[tokio::test]
    async fn graph_search_unknown_name_returns_empty_not_error() {
        let (_dir, graph) = indexed_graph();
        let tool = GraphSearchTool::new(graph);
        let out = payload(tool.execute(json!({ "query": "nonexistent_xyz" })).await);
        assert_eq!(out["count"], 0);
        assert_eq!(out["symbols"].as_array().unwrap().len(), 0);
        // A miss must self-correct: the hint names the query, points
        // string-literal lookups at `search`, and partial names at a retry.
        let hint = out["hint"].as_str().expect("a miss carries a hint");
        assert!(hint.contains("nonexistent_xyz"), "hint names the query");
        assert!(hint.contains("`search`"), "hint points at text search");
    }

    #[tokio::test]
    async fn graph_search_hit_carries_no_hint() {
        let (_dir, graph) = indexed_graph();
        let tool = GraphSearchTool::new(graph);
        let out = payload(tool.execute(json!({ "query": "helper" })).await);
        assert_eq!(out["count"], 1);
        // Hits stay lean: no hint key at all (indexing a missing key on a
        // Value yields Null, so assert via get()).
        assert!(out.get("hint").is_none(), "a hit must not carry a hint");
    }

    #[tokio::test]
    async fn stale_symbol_miss_reindexes_and_serves_fresh_results() {
        // Backlog 95f21af0 (F10 for the symbol index): the watcher's
        // reindex is best-effort — an edit it missed leaves the symbol
        // index stale, and a total graph_search miss served silently made
        // the agent conclude the symbol doesn't exist (live case:
        // normalize_new_item_text, added hours earlier, "No symbols
        // matched"). A small staleness must be repaired INLINE: the stale
        // files are re-indexed, the query re-resolved once from the fresh
        // view, and the side effect disclosed.
        let (dir, graph) = indexed_graph();
        // Simulate the edit→watcher gap: add a new symbol to an EXISTING
        // indexed source file, bump its mtime, no reindex.
        let path = dir.path().join("src/lib.rs");
        std::fs::write(
            &path,
            "pub fn helper() -> u32 { 1 }\npub fn fresh_symbol() -> u32 { 2 }\n",
        )
        .unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(later)
            .unwrap();
        let tool = GraphSearchTool::new(graph);
        let out = payload(tool.execute(json!({ "query": "fresh_symbol" })).await);
        // The miss self-healed: the new symbol is served from the fresh
        // index, with the reindex disclosed.
        assert_eq!(out["count"], 1, "the stale miss must self-heal: {out}");
        assert_eq!(out["symbols"][0]["name"], "fresh_symbol");
        let note = out["note"].as_str().expect("the reindex is disclosed");
        assert!(
            note.contains("reindexed 1 stale file(s)"),
            "note discloses the inline re-index: {note}"
        );
    }

    #[tokio::test]
    async fn fresh_miss_stays_fast() {
        // A genuinely absent symbol on a FRESH index: the staleness sweep
        // is stats-only and finds nothing — no reindex, no note (backlog
        // 95f21af0: no full reindex on every miss).
        let (_dir, graph) = indexed_graph();
        let tool = GraphSearchTool::new(graph);
        let out = payload(tool.execute(json!({ "query": "nonexistent_xyz" })).await);
        assert_eq!(out["count"], 0);
        assert!(
            out.get("note").is_none(),
            "a fresh miss carries no note: {out}"
        );
        assert!(out["hint"].as_str().unwrap().contains("nonexistent_xyz"));
    }

    #[tokio::test]
    async fn vanished_stale_file_degrades_gracefully() {
        // A stale file that VANISHED between indexing and the sweep: the
        // sweep flags it (mtime 0 ≠ stored), the inline reindex prunes its
        // rows, and the lookup still serves the normal miss — the repair
        // path never breaks the query (best-effort).
        let (dir, graph) = indexed_graph();
        std::fs::remove_file(dir.path().join("src/main.rs")).unwrap();
        let tool = GraphSearchTool::new(graph);
        let out = payload(tool.execute(json!({ "query": "nonexistent_xyz" })).await);
        assert_eq!(out["count"], 0);
        // The vanished file was pruned (reindexed 1) — disclosed, not an
        // error; the miss is served normally.
        let note = out["note"].as_str().expect("the prune is disclosed");
        assert!(
            note.contains("reindexed 1 stale file(s)"),
            "note discloses the prune: {note}"
        );
        assert!(out["hint"].as_str().unwrap().contains("nonexistent_xyz"));
    }

    #[tokio::test]
    async fn stale_above_the_reindex_cap_notes_instead_of_reindexing() {
        // Backlog 95f21af0 (review LOW 2): staleness ABOVE the ceiling is not
        // repaired inline — the miss is served with the staleness note
        // (pinning the note string + the no-inline-reindex-above-cap
        // behavior, mirroring the search tool's
        // stale_above_the_reindex_cap_walks). The ceiling is now the content
        // index's adaptive 32 (raised 8 → 32 on 2026-09-26), so this needs
        // ceiling + 1 stale files.
        let (dir, graph) = indexed_graph();
        // STALE_REINDEX_CAP + 1 stale source files: the 2 fixture files plus
        // ceiling - 1 extras.
        let mut stale_names = vec!["src/lib.rs".to_string(), "src/main.rs".to_string()];
        for i in 0..(STALE_REINDEX_CAP - 1) {
            let name = format!("src/extra{i}.rs");
            std::fs::write(
                dir.path().join(&name),
                format!("pub fn extra_{i}() -> u32 {{ {i} }}\n"),
            )
            .unwrap();
            stale_names.push(name);
        }
        graph.index(None).unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        for name in &stale_names {
            std::fs::File::options()
                .write(true)
                .open(dir.path().join(name))
                .unwrap()
                .set_modified(later)
                .unwrap();
        }
        let tool = GraphSearchTool::new(graph);
        let out = payload(tool.execute(json!({ "query": "nonexistent_xyz" })).await);
        assert_eq!(out["count"], 0);
        let note = out["note"].as_str().expect("the staleness note is served");
        assert!(
            note.contains(&format!(
                "symbol index may be stale — {} file(s) on disk are newer than the index",
                STALE_REINDEX_CAP + 1
            )),
            "note pins the stale count: {note}"
        );
        assert!(!note.contains("reindexed"), "no inline reindex above the cap");
        assert!(out["hint"].as_str().unwrap().contains("nonexistent_xyz"));
    }

    #[tokio::test]
    async fn stale_within_the_reindex_ceiling_repairs_inline() {
        // 2026-09-26 (user directive: accurate results matter more than the
        // note): a drift WITHIN the adaptive ceiling must be REPAIRED inline
        // and the lookup re-served from the fresh view. Nine stale files is
        // exactly the case the old ceiling of 8 mishandled — the live
        // 61-file drift left this plan's own regression-test symbol
        // unindexed and forced the manual reindex detour.
        let (dir, graph) = indexed_graph();
        for i in 0..7 {
            std::fs::write(
                dir.path().join(format!("src/extra{i}.rs")),
                format!("pub fn extra_{i}() -> u32 {{ {i} }}\n"),
            )
            .unwrap();
        }
        graph.index(None).unwrap();
        // src/extra0.rs gains a symbol the index has never seen — only a
        // re-read of the stale file can answer the query below.
        std::fs::write(
            dir.path().join("src/extra0.rs"),
            "pub fn fresh_symbol_xyz() -> u32 { 7 }\n",
        )
        .unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        for name in [
            "src/lib.rs",
            "src/main.rs",
            "src/extra0.rs",
            "src/extra1.rs",
            "src/extra2.rs",
            "src/extra3.rs",
            "src/extra4.rs",
            "src/extra5.rs",
            "src/extra6.rs",
        ] {
            std::fs::File::options()
                .write(true)
                .open(dir.path().join(name))
                .unwrap()
                .set_modified(later)
                .unwrap();
        }
        let tool = GraphSearchTool::new(graph);
        let out = payload(tool.execute(json!({ "query": "fresh_symbol_xyz" })).await);
        assert_eq!(
            out["count"], 1,
            "the repaired view resolves the symbol: {out}"
        );
        let note = out["note"].as_str().expect("the repair is disclosed");
        assert!(
            note.contains("reindexed") && note.contains("serving fresh graph results"),
            "inline repair is disclosed: {note}"
        );
        assert!(
            !note.contains("may be stale"),
            "no staleness note within the ceiling: {note}"
        );
    }

    #[test]
    fn miss_hint_branches_by_candidates() {
        // Total miss (no candidates) → the `search` pointer for string
        // literals; near-miss (candidates exist) → the candidates' ids.
        let total = miss_hint("ghost", 0);
        assert!(total.contains("ghost"), "total-miss hint names the query");
        assert!(total.contains("`search`"), "total miss points at search");
        let near = miss_hint("ghst", 3);
        assert!(
            near.contains("candidates' ids"),
            "near-miss points at the candidates"
        );
    }

    #[tokio::test]
    async fn graph_context_by_id_gives_360_view() {
        let (_dir, graph) = indexed_graph();
        // Find helper's id via search first (the documented flow).
        let search = GraphSearchTool::new(graph.clone());
        let found = payload(search.execute(json!({ "query": "helper" })).await);
        let id = found["symbols"][0]["id"].as_str().unwrap().to_string();

        let tool = GraphContextTool::new(graph);
        let out = payload(tool.execute(json!({ "id": id })).await);
        assert_eq!(out["found"], true);
        let ctx = &out["context"];
        assert_eq!(ctx["symbol"]["name"], "helper");
        // main() calls helper → incoming calls group has one entry.
        let incoming_calls = ctx["incoming"]["calls"].as_array().unwrap();
        assert_eq!(incoming_calls.len(), 1);
        assert_eq!(incoming_calls[0]["name"], "main");
        // The module import edge (main.rs module → helper) is also incoming.
        assert!(ctx["incoming_total"].as_u64().unwrap() >= 2);
    }

    #[tokio::test]
    async fn graph_context_by_name_resolves() {
        let (_dir, graph) = indexed_graph();
        let tool = GraphContextTool::new(graph);
        let out = payload(tool.execute(json!({ "name": "helper" })).await);
        assert_eq!(out["found"], true);
        assert_eq!(out["context"]["symbol"]["name"], "helper");
    }

    #[tokio::test]
    async fn graph_context_unknown_symbol_returns_helpful_empty() {
        let (_dir, graph) = indexed_graph();
        let tool = GraphContextTool::new(graph);
        let out = payload(tool.execute(json!({ "name": "ghost" })).await);
        assert_eq!(out["found"], false);
        assert!(out["candidates"].is_array());
        // A total miss self-corrects: the hint names the query and points
        // string-literal lookups at `search` (literals are not indexed).
        let hint = out["hint"].as_str().expect("a miss carries a hint");
        assert!(hint.contains("ghost"), "hint names the query");
        assert!(hint.contains("`search`"), "hint points at text search");
        // Neither id nor name → argument error, not a panic.
        let err = tool.execute(json!({})).await;
        assert!(!err.success);
    }

    #[tokio::test]
    async fn graph_impact_unknown_symbol_miss_carries_search_hint() {
        let (_dir, graph) = indexed_graph();
        let tool = GraphImpactTool::new(graph);
        let out = payload(tool.execute(json!({ "name": "ghost" })).await);
        assert_eq!(out["found"], false);
        let hint = out["hint"].as_str().expect("a miss carries a hint");
        assert!(hint.contains("`search`"), "hint points at text search");
    }

    #[tokio::test]
    async fn graph_impact_lists_transitive_dependents() {
        let (_dir, graph) = indexed_graph();
        let tool = GraphImpactTool::new(graph);
        let out = payload(tool.execute(json!({ "name": "helper" })).await);
        assert_eq!(out["found"], true);
        let impact = &out["impact"];
        assert!(
            impact["total"].as_u64().unwrap() >= 2,
            "main() + main.rs module import"
        );
        // Depth 1 must include main (direct caller).
        let d1 = impact["depths"][0]["symbols"].as_array().unwrap();
        assert!(d1.iter().any(|s| s["name"] == "main"));
    }

    #[tokio::test]
    async fn graph_path_connects_main_to_helper() {
        let (_dir, graph) = indexed_graph();
        let tool = GraphPathTool::new(graph);
        let out = payload(
            tool.execute(json!({ "from": "main", "to": "helper" }))
                .await,
        );
        assert_eq!(out["found"], true);
        let hops = out["path"]["hops"].as_array().unwrap();
        assert_eq!(hops.len(), 2, "main calls helper directly");
        assert_eq!(hops[0]["symbol"]["name"], "main");
        assert_eq!(hops[0]["edge_to_next"], "calls");
        assert_eq!(hops[1]["symbol"]["name"], "helper");
        assert!(hops[1]["edge_to_next"].is_null());
    }

    #[tokio::test]
    async fn graph_path_unknown_endpoint_returns_candidates() {
        let (_dir, graph) = indexed_graph();
        let tool = GraphPathTool::new(graph);
        let out = payload(
            tool.execute(json!({ "from": "ghost", "to": "helper" }))
                .await,
        );
        assert_eq!(out["found"], false);
        assert_eq!(out["reason"], "from symbol not found");
        assert!(out["candidates"].is_array());
        // Misses self-correct: a total-miss hint points at `search`.
        let hint = out["hint"].as_str().expect("a miss carries a hint");
        assert!(hint.contains("ghost"), "hint names the failing endpoint");
        assert!(hint.contains("`search`"), "hint points at text search");
    }
}
