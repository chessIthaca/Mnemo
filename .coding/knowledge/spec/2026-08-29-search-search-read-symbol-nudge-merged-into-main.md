+++
title = "search/search_read symbol nudge — MERGED into main (7fd1802)"
supersedes = "2026-08-29-search-search-read-symbol-nudge-bare-identifier"
created = "2026-08-29"
status = "superseded"
+++

SPEC (2026-08-29, plan 719b40e3, commits c89fbc6 + 4ac51c1, MERGED into main at 7fd1802): `search` and `search_read` prepend a one-line SYMBOL NUDGE when the pattern is a bare ASCII identifier that EXACTLY names an indexed symbol (case-sensitive; CodeGraph::symbol_exists — one indexed SELECT, best-effort false on any error, skipped for non-identifier patterns and absent/unindexed graphs): "'<pattern>' is an indexed symbol — graph_search resolves it and graph_context gives its definition + callers; prefer the graph tools for symbol lookups (search is for text)". The note merges with the literal-fallback note (merged_note, joined "; ") and stays PREPENDED above results — tool output is truncated from the END, so prepended notes survive every byte cap. Advisory only: result rows, match counts, engine selection, and exit codes are untouched. Search's schema description was compressed to stay inside the ExecutingResearch tools-array budget (guardrail caught the first version at +151 chars). Detail: src/tool/agent/search.rs module doc, PLAN.md Terminology "Agent tools".
