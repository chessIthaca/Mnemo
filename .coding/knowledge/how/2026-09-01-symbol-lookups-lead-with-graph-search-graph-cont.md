+++
title = "symbol lookups lead with graph_search → graph_context — search is for text"
created = "2026-09-01"
+++

HOW (backlog 8b8f40d2, plan 08eb13d8): symbol questions LEAD with graph tools, never search/grep.

WHEN a question is symbol-shaped — "where is X defined", "who calls X", "callers/callees/imports/blast radius/reachability of X", for ANY indexed language (.rs/.ts/.tsx) — the first call is graph_search(query=X) (or graph_context(id=...) when an id is already known), NOT search/search_read. search is for TEXT: comments, string literals, config keys, log text. If search/search_read returns a "SEARCH INTERCEPTED" redirect, call graph_context(id=...) immediately (the gate fired because the symbol nudge was ignored twice).

RATIONALE (2026-12-04 run-all/steer investigation, the instances that motivated this): send_suggestion / send_prompt / on_main_turn_resolved / run_all_dispatch_next / backlog_add were all looked up via search when the actual question was callers/callees — graph_context answers definition + callers + callees + imports in ONE indexed call; a grep only returns text lines and costs a follow-up read. In the steer case the search result itself carried the graph_context(id=...) nudge and it was ignored. graph_search is lexical/substring (recalled HOW a146c39d) — on zero hits retry with a shorter name fragment rather than falling back to grep.

AMENDMENT (2026-12-29, backlog b804012f): a symbol-shaped search/search_read now AUTO-DELEGATES — the graph answer (definition + top callers/callees + the graph_context pointer) rides inline and the walk is skipped, so an accidental symbol-shaped search no longer wastes a round-trip. The discipline stands regardless: for callers/blast-radius/reachability questions LEAD with graph_search/graph_context/graph_impact — the delegated block is a compact answer, not the full 360° view; and a memory-targeted query (knowledge/reviews globs, typed prefixes) delegates to memory_search the same way.
