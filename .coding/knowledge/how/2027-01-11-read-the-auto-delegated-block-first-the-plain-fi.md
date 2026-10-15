+++
title = "read the AUTO-DELEGATED block first — the plain file matches ride below; re-issue only to drop the block"
supersedes = "2026-12-28-read-the-auto-delegated-block-first-graph-contex"
created = "2027-01-11"
+++

When a `search`/`search_read` result carries a block opening "AUTO-DELEGATED to the code graph — 'X' is an indexed symbol (the plain file matches follow below; re-issue this exact search to drop the block):" (backlog b804012f), the graph answer is ALREADY inline — definition, top callers/callees, the graph_context pointer — read the block FIRST; do not call graph_search/graph_context for the definition, only for the full 360° view. Since backlog c439dda0 (plan 89bbd2a9, wt/mnemo, 2027-01-15) the plain file matches ride BELOW the block in the SAME response — the walk is never skipped, so the old re-issue-for-file-matches dance is gone: re-issue the same query ONLY to get the plain search without the block (the escape hatch — a repeat of a delegated key never re-delegates in-session). The memory twin ("AUTO-DELEGATED to memory — …") behaves identically. Supersedes the 2026-12-29-era guidance that told you to re-issue the identical search for the plain file matches — the fast-path skip that made that necessary was removed from search.rs/search_read.rs execute (the block now always prepends above the results).
