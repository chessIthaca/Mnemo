# Plan: Cost and Code graph tabs in Mnemo2

## Goal
Mnemo2 gets two new top-level tabs beside Lead and Team. Cost shows what the project spent and what the token savings and prompt cache saved, in dollars, by model and by day. Graph lets you search the project's code graph and explore a symbol's callers, callees and change impact. Both are redesigned from Mnemo1's Dashboard and Graph views, with the debug-only parts dropped.

## Kind
implementation

## Context
WHY: the user (2026-11-01) asked to port Mnemo1's cost-savings tab and code-graph tab to Mnemo2, redesigned, dropping useless parts. An earlier user note says the same about Cost: open a project cost summary similar to Mnemo1, but refine it rather than port it as is.

MNEMO1 SOURCES (read-only survey of the code; file:line references):
- frontend/src/components/views/DashboardView.tsx: tokens-saved and event cards; a by-kind table of raw lever names; per-day bars; the last 50 savings events; a prompt-cache card with hit rate and ESTIMATED dollars (rateEstimate :62-86, per_model x [[pricing]]). Data comes from get_savings_stats (src-tauri/src/ipc/agent.rs:980 -> MemoryStore::savings_stats src/memory/mod.rs:2457, SavingsStats at src/memory/types.rs:682) and get_project_stats (agent.rs:960, ProjectStats at types.rs:844 with per_model and per_day).
- frontend/src/components/views/GraphView.tsx: status and counts with Refresh; name filter, kind dropdown and focus chip; a d3-force SVG of up to 300 top-degree nodes (GRAPH_SLICE_CAP src/codegraph/query.rs:116); a selected-node panel; a legend; and a bottom pane with source or a "memory access" debug log. Commands codegraph_status, codegraph_refresh and codegraph_graph live in src-tauri/src/ipc/codegraph_cmds.rs and go through CodegraphApi (src/headless/codegraph_api.rs) and GraphView::slice (src/codegraph/query.rs:382).

MNEMO2 TODAY:
- verified: `pub async fn stats(&self) -> Result<EngineStats, EngineError>` at src/headless/engine.rs:680; EngineStats (src/headless/stats.rs:188) holds project_stats: ProjectStats and savings_stats: SavingsStats. No Mnemo2 command calls it yet.
- verified: `pub fn codegraph(&self) -> CodegraphApi` at src/headless/engine.rs:616; Mnemo2 exposes no graph command to its frontend (graph_* exist only as agent and MCP tools).
- verified: request_stats rows are recorded by the shared turn loop (src/agent/turn.rs:3908 record_request_stats), so the engine Mnemo2 runs fills project stats.
- Mnemo2 keeps its own durable per-model usage (src-tauri/src/mnemo2/usage.rs ProjectUsage, models -> prompt/cached/output tokens) and exposes only totals via team_snapshot usage (ProjectUsageView). Pricing comes from [[pricing]] / endpoint configuration in the shared config dir.
- verified: Tauri commands are registered in one `tauri::generate_handler![...]` list in src-tauri/src/mnemo2/mod.rs (around line 390).
- verified: `export type Route = "start" | "lead" | "team";` at mnemo2/src/lib/types.ts:6. TopBar.svelte has Lead and Team buttons; clicking the cost chip opens the project settings sheet, which shows no cost detail.
- verified: d3-force is a dependency of frontend/package.json (line 19) but not of mnemo2/package.json.

DESIGN DECISIONS:
- Navigation: Route gains "cost" and "graph". The top bar shows Lead · Team · Cost · Graph, and the cost chip opens the Cost tab instead of the settings sheet.
- Cost tab, in order:
  1. Headline: project spend in USD with input, cached and output tokens, and a partial-data note for unpriced models.
  2. Spend by model: rows with tokens and dollars from Mnemo2's per-model usage priced by configured pricing, sorted by cost.
  3. Savings: tokens saved and estimated dollars saved, total and per lever. Lever names are friendly with a one-line explanation each, not raw kinds like "delta_read".
  4. Prompt cache: hit rate and estimated dollars saved.
  5. Last 14 days: spend and savings per day as small bars.
  Dropped: the recent-events list, request counters such as "n/m requests reporting cache", and raw lever ids.
- Graph tab: a status line (files, symbols, edges, last indexed, indexing pulse) with Refresh, and a search box that searches symbols by name with kind filter chips. A results list sits on the left. Picking a symbol shows its 1-hop neighbourhood (depth 2 on demand) as a small force graph: labels on every node, draggable nodes, zoom and pan, with edges styled by kind. A details panel lists callers, callees and change impact (what a change to this symbol touches, via the impact query), each clickable to refocus, plus file:line with a copy button. With no selection, it shows the most-connected symbols as entry points instead of a 300-node hairball. Dropped: the memory access log and the unfiltered 300-node view.
- Backend commands are new modules, not edits to ipc.rs: src-tauri/src/mnemo2/insights.rs (cost and savings) and src-tauri/src/mnemo2/graph.rs (graph), each registered in mod.rs.
- Type scale: only --text-meta, --text-body, --text-title, --text-page (SPEC: Mnemo2 type scale). Review UI changes with preview.html scenes (add cost and graph scenes).

VERIFY:
- cargo test --manifest-path src-tauri/Cargo.toml --features mnemo2 --lib mnemo2::
- cargo clippy --manifest-path src-tauri/Cargo.toml --features mnemo2 --lib --tests --bins (clean)
- in mnemo2/: npm run check, npm run check:no-mock, npx vitest run
- UI steps: render the new preview scenes at http://127.0.0.1:5180/preview.html?scene=<name> and report what shows.
- Fresh worker worktrees need `npm ci --no-audit --no-fund` at the worktree root first.

## Acceptance scenarios
- The top bar shows Lead, Team, Cost and Graph; clicking the cost chip opens the Cost tab.
- The Cost tab shows total spend, spend by model, savings in tokens and dollars with friendly lever names, the prompt cache hit rate and the last 14 days, and no raw event log.
- On the Graph tab, searching a symbol name and picking a result shows its neighbourhood graph and lists its callers, callees and impact; clicking one refocuses.
- With no selection the Graph tab lists the most-connected symbols instead of a 300-node graph.

## Steps
- [x] 1. **Cost and savings commands** — `src-tauri/src/mnemo2/insights.rs` (new), `src-tauri/src/mnemo2/mod.rs` (module line and handler names only): `insights_cost` (per-model usage from Mnemo2's ledger priced by configured pricing, totals, unpriced models, per-day spend if available) and `insights_savings` (Engine::stats savings_stats and project_stats cache figures, per lever with estimated dollars, per day); tests.
- [x] 2. **Code graph commands** — `src-tauri/src/mnemo2/graph.rs` (new), `src-tauri/src/mnemo2/mod.rs` (module line and handler names only): `graph_status`, `graph_refresh`, `graph_search(query, kind, limit)`, `graph_neighbourhood(symbol_id, depth)`, `graph_impact(symbol_id)`, `graph_top(limit)` over engine.codegraph() (CodegraphApi); results capped and serialisable; tests.
- [x] 3. **Cost and Graph tabs in the shell** — `mnemo2/src/lib/types.ts` (Route), `mnemo2/src/components/TopBar.svelte` (Lead · Team · Cost · Graph, cost chip opens Cost), `mnemo2/src/App.svelte` (route switch), `mnemo2/src/lib/store.ts` (openCost/openGraph actions only), placeholder `mnemo2/src/components/CostView.svelte` and `mnemo2/src/components/GraphView.svelte`; tests for navigation.
- [x] 4. **Cost view** — `mnemo2/src/components/CostView.svelte`, `mnemo2/src/lib/cost.ts` (transport + view model + lever names), `mnemo2/src/test/previewScenes.ts` (a `cost` scene): the redesigned Cost tab per the decisions; tests.
- [x] 5. **Graph view** — `mnemo2/src/components/GraphView.svelte`, `mnemo2/src/lib/graph.ts` (transport + layout helpers), `mnemo2/package.json` (d3-force), `mnemo2/src/test/previewScenes.ts` (a `graph` scene): the redesigned Graph tab per the decisions; tests.
- [x] 6. **Integration check** — run the combined result (build, tests, or launching the entry point) and verify the parallel steps fit together; fix any seam breakage.

## Decisions
- 2026-11-01: added step 6 (Integration check) because steps 1 and 2 ran in parallel

## Interfaces
- 2026-11-01: src-tauri/src/mnemo2/mod.rs is shared by steps 1 and 2 (and by the model-settings plan): each step adds only its own `pub mod <name>;` line and its own command names to the existing generate_handler! list, never reorders or edits other entries.
- 2026-11-01: mnemo2/src/test/previewScenes.ts is shared by steps 4 and 5: each adds only its own scene; step 5 runs after step 4 merges.
- 2026-11-01: Step 4 consumes step 1's commands and step 3's placeholder CostView; step 5 consumes step 2's commands and step 3's placeholder GraphView.

## Progress
- 2026-11-01 step 3 done: step 3 merged: shellTabs62 (shellTabs62) on deepseek-v4.1-flash:1017, merge commit 8cb61239; review: The top bar has Lead · Team · Cost · Graph, the cost chip opens Cost, and placeholder Cost and Graph views render. The gear now opens the project sheet, which links to the endpoints and tiers dialog.
- 2026-11-01 step 1 done: step 1 merged: costCmds60 (costCmds60) on deepseek-v4.1-flash:1017, merge commit d8450461; review: insights_cost and insights_savings give the Cost tab per-model spend from Mnemo2's durable usage, priced like the header, plus savings per lever, prompt-cache figures and 14-day series from the shared engine's stats.
- 2026-11-02 step 2 done: step 2 merged: graphCmds61 (graphCmds61) on deepseek-v4.1-flash:1017, merge commit 4e1e6449; review: Seven graph commands over the shared engine's code graph, with named caps and blocking-pool traversals. Registrations moved to anchors that merge cleanly with the cost and model-settings commands.
- 2026-11-02 step 4 done: step 4 merged: costView65 (costView65) on glm-5.3, merge commit 36049b65; review: The Cost tab shows project spend, spend by model, savings per lever in plain words with dollars, the prompt cache, and 14-day spend and savings charts. Levers that cost tokens show as re-added with no dollar figure.
- 2026-11-02 step 5 done: step 5 merged: graphView69 (graphView69) on deepseek-v4.1-flash:1017, merge commit c3d843a8; review: The Graph tab searches symbols, lists the most-connected ones, and draws a selected symbol's 1- or 2-hop neighbourhood with labels and edge styles. A details panel shows callers, callees and grouped change impact, each clickable.
- 2026-11-02 step 6 done: wt/mnemo at 9bf4447: both clippy -D warnings clean; cargo test root lib 3185, mnemo2 lib 283, mnemo2-mcp bin 147 passed; svelte-check 0 errors; vitest green. Cost and Graph tabs rendered in the preview during their reviews.
