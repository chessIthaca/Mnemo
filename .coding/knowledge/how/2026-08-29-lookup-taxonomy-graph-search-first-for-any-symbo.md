+++
title = "lookup taxonomy — graph_search first for any symbol, search only for literals/docs/edit anchors"
created = "2026-08-29"
+++

USER CORRECTION 2026-08-29: I drift to `search` for symbol lookups where graph tools are mandatory. Discipline for EVERY lookup, before calling search:
1. Does the pattern name a symbol (function/method/type/interface/struct/handler)? → graph_search first (exact name), then graph_context for definition + callers/imports. NEVER a regex search for "where is X defined" — a walk-engine search scanned 1455 files for `connectOauth` that graph_search would have resolved instantly.
2. `search` is ONLY for: string literals (JSX labels, tool names, log text), config keys, doc/markdown text, exact-line edit anchors (e.g. `^import type` for file_edit), or when the graph genuinely comes up empty (then say so).
3. When a search hit surfaces a symbol I then navigate around, SWITCH to graph_context immediately — don't read the whole file.
Self-check each time: "is this pattern a symbol name?" If yes → graph tool. The compiled prompt already mandates this; the failure was mid-flow habit, not missing rules.
