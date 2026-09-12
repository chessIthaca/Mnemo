+++
title = "SEARCH INTERCEPTED redirect — call graph_context immediately"
created = "2026-08-31"
+++

HOW: When search/search_read is intercepted with a "SEARCH INTERCEPTED" redirect, call graph_context(id=...) immediately — the search was blocked because you ignored the graph-tool symbol nudge >= 2 times this session without switching. The redirect embeds the symbol id (graph_context(id="file::name::line")). If you actually need text occurrences (comments, string literals, config keys), retry the search with a regex metacharacter or glob filter to disambiguate from a symbol lookup. The gate lifts once you switch to a graph tool (graph_search/graph_context/graph_impact/graph_path). This is the C5 graduated gate (plan a41a0d82, 2026-12-04) — the C3 advisory NOTE's teeth: advisory for 2 fires, intercept after.
