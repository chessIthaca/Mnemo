+++
title = "search-usage tally + analysis — 2026-09-15 search-optimization session (F1-F6 findings)"
created = "2026-08-29"
+++

SEARCH-USAGE TALLY + ANALYSIS (2026-09-15, executing session of plan 812e1f0a; per user's standing request; same protocol as HOW 5770de80). ~31 lookup calls. Breakdown: read_files ~12 (all high-yield; whole-file reads of files that were about to be edited), text search/search_read ~12 (10 high, 1 MISS, 1 tool-limited), memory_search 6 (5 high, 1 cap-artifact), graph tools 3 (2 high, 1 gap), git reads ~5 (all high — status/log/pickaxe were decisive for the stale-grounding question).

WHAT WORKED (this round's own features, dogfooded): search literal:true rode engine: index (bm25) repeatedly; read_files SYMBOL NUDGE fired on big indexed reads; search output's symbol nudge fired on a bare-identifier content search; the definition-prefix nudge + literal-tip TIP shipped this session directly cover last session's observed drift patterns ("fn search_content" queries, metachar-free regex walks). The pruned walk will cut the 107k-stat-call class entirely.

REMAINING WASTE CLASSES (findings → follow-up plan):
(F1, BUG) memory_update on a knowledge-successor file fails its own single-file reindex: build_knowledge_metas (indexer.rs:605-672) resolves `supersedes` only within the reindexed subset, tool/memory/mod.rs:301 passes only the touched rel → "supersedes 'slug' not found" error; digest stale until startup reconciliation. Real bug, needs code fix + regression test.
(F2) search is filename-blind: the session's only true MISS was hunting .coding/plans/812e1f0a.md by id in CONTENT (392-file walk, 0 hits — the file never contains its own id). Fix: current_plan should report the plan file path; search should fall back to filename matching on zero content hits.
(F3) graph modules carry no incoming edges: graph_context(module) can't answer "who uses this module" (forced text-search fallback). Optional graph-index polish; low value.
(F4, convention) memory_search result caps cause false "row is gone" conclusions: a limit-3 search hid a rank-4 row and nearly triggered a wrong diagnosis; the "… [truncated]" note must be read as INCOMPLETE, re-run with higher limit before concluding absence.
(F5, convention) plan grounding labeled "verified" can be stale: step 1 was ~90% already-implemented (2da09da) despite "has NO index path (verified)". Absence claims in plan grounding should cite pickaxe/commit evidence (git log -S).
(F6, convention) tool schema descriptions have hard per-state budgets (factory test; ExecutingResearch had ~40 chars headroom) — additions must be paid for by trims; detail belongs in README/PLAN.md.
