+++
title = "read the AUTO-DELEGATED block first — graph_context only for the full 360° view"
supersedes = "2026-08-31-act-on-the-search-symbol-nudge-immediately-graph"
created = "2026-12-28"
+++

When a `search`/`search_read` result opens with "AUTO-DELEGATED to the code graph — 'X' is an indexed symbol …" (backlog b804012f), the graph answer is ALREADY inline — definition, top callers/callees, and the graph_context pointer. Do NOT call graph_context for the basic lookup; call it (graph_context(id="...")) only when you need the FULL 360° view (all callers/callees/imports/containment) or graph_impact before editing a shared symbol. If instead the first line is the advisory note "'X' is an indexed symbol — graph_context(id=...) gives its definition + callers in one call" (fires on the escape repeat — you re-issued a delegated query — or for fuzzy alternation branches), call graph_context(id=...) as your next tool call if you still want the symbol answer; if you deliberately escaped to get file-text results, just continue. Re-issuing the same query a third time keeps giving plain search (the bypass is sticky per exact query — a bounded set of escaped keys, never displaced by other delegating queries; 2026-01-03, plan 2a78cc3c).
