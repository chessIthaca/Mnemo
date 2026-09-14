+++
title = "search vs graph/memory — the avoidable calls (19-call self-audit)"
created = "2027-01-11"
+++

Self-audit from plan d826b9ad (merge_to_main remote sync, 2026-09-13-derived): of 19 `search` calls in the session, ~8 were avoidable, ~3 could have been cheaper, ~8 were legitimate. The rules that would have caught them:

1. SYMBOLS → graph_search with the EXACT symbol name, not a pattern merely containing it. `merge_to_main` over **/*.rs = 89 noise matches; `SHIPPED_SKILLS|seed|write_skill_file` auto-delegated to retrieval.rs::seed — the WRONG symbol, one round-trip lost; `\.coding/reviews|is_protected` = 101 matches/18 files when `is_protected_write_target` was one lookup away. Naming the symbol beats containing it.
2. TEXT → always a scoped `glob` + `literal:true` (index engine, one lookup instead of a tree walk). `merge_to_main` over **/*.md = 101 matches; the `{README.md,docs/**}`-scoped query found docs/FEATURES.md:34 first try. `git (fetch|pull)` was the right question run as a 328-file walk.
3. MEMORY DIGESTS ARE TRUNCATED BY DESIGN — the digest IS the answer. Probing for a record's full body (repo-wide `pull --no-rebase`, 2773-file walk, empty) is pure waste.
4. READ SHAPE: a 53-line skill file cost 5 chunked reads because long prompt lines get truncated — read the needed line range (e.g. 45-53) up front instead of the whole file in chunks.
Counter-data: harness auto-delegation was OPTIMAL for `mergePrompt`, `GitView`, `enterSkill` (def + callers in one call) — the miss is only when the pattern names a symbol OTHER than the one wanted.

Related: HOW a146c39d (graph_search is lexical substring, not semantic); backlog 63cbc20f (steering-optimization analysis, HOW 706d24ad "track search usage").
