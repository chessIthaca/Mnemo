+++
title = "Mnemo2 has Cost and Graph tabs"
created = "2026-11-02"
+++

Mnemo2's top bar shows Lead · Team · Cost · Graph (Route "cost" | "graph"). The cost chip opens Cost; the settings gear opens the project sheet, which links to the app settings dialog.
Cost tab (mnemo2/src/components/CostView.svelte, lib/cost.ts) uses insights_cost and insights_savings (src-tauri/src/mnemo2/insights.rs). It shows project spend with in/cached/out tokens and a partial-estimate note for unpriced models, spend by model priced like the header, savings per optimizer lever with plain names and estimated dollars (a lever that cost tokens shows as "re-added", with no dollars), prompt-cache hit rate and dollars saved, and 14-day spend (labelled estimated: per-day requests priced at a blended rate) and tokens-saved bars. Workers share the lead's engine, so the stats cover the whole team.
Graph tab (GraphView.svelte, lib/graph.ts, d3-force) uses graph_status/refresh/search/neighbourhood/impact/top/context (src-tauri/src/mnemo2/graph.rs; traversals on the blocking pool, caps: search 200, neighbourhood 150 nodes ring-first, impact 200). It has a status line, symbol search with kind chips, a most-connected list, a 1-hop neighbourhood (2 on toggle) as a labelled draggable force graph, and callers, callees and grouped change impact. Mnemo1's raw savings event log, request counters, 300-node graph and memory access log were dropped. Both tabs have preview scenes (cost, graph).
