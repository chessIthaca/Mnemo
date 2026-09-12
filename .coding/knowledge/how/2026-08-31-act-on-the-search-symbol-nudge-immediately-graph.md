+++
title = "act on the search symbol-nudge immediately — graph_context(id) answers callers/callees in one call"
created = "2026-08-31"
status = "superseded"
+++

When a `search`/`search_read` result's first line says "'X' is an indexed symbol — graph_context(id=...) gives its definition + callers in one call", call graph_context(id=...) as your VERY NEXT tool call — do not read the text matches first. The nudge fires only when the pattern exactly names an indexed symbol (case-sensitive bare identifier or def-prefixed name); graph_context returns the definition + incoming/outgoing edges (callers, callees, imports, containment) in one indexed lookup, while search returns text matches you must cross-reference by hand. Callers/callees is the most common symbol-lookup need and the nudge answers it directly.

The reflex to reach for `search` for symbol questions — "where is this defined and who calls it" — is the habit to break. The nudge IS the steering layer telling you the graph tools are cheaper. If you ignore it twice, the C3 escalation note fires (threshold 2): "follow it (its named target tools) on the next call; each repeat wastes a round-trip the target tools would answer directly." Heed it — switch to graph_context/graph_impact for the next symbol lookup.

Concrete miss (2026-12-04 run-all/steer bug investigation): send_suggestion, send_prompt, on_main_turn_resolved, run_all_dispatch_next, backlog_add, steer — all symbol lookups done via `search` despite the nudge firing (confirmed for `steer`). Each was a callers/callees question that graph_context answers in one call. Reference: HOW a146c39d (graph_search is lexical — retry name fragments on zero hits), DECISION c8ddb1f2 (steering layer over prose mandates).
