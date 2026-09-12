+++
title = "shell/browser/search tool calls always render on their own transcript line (never grouped)"
created = "2026-12-23"
+++

SPEC: shell, browser, and search tool calls never merge into grouped transcript cards (user directive). The never-merge set in reduceToolCallStart (frontend/src/hooks/agentEventReducer.ts, the neverGroups expression): `shell`, `browser_*` / `offscreen_browser_*` (backlog daa38cbe, plan b6aee084, commit f80f2ff), and `search` / `search_read` (backlog 24ddb845, plan dc534943) — each call renders on its own line because the command/target/query of each call is the point of the card and grouped chips bury it. All other tools keep the compact merge behavior (consecutive same-name calls group into one card, split past MAX_CALLS_PER_TOOL_CARD, failed calls break the chain). Regression tests: the never-merge suite in frontend/src/hooks/useAgentStore.test.ts (shell, browser_navigate, offscreen_browser_navigate, search, search_read).
