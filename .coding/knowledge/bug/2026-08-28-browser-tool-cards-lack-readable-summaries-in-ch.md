+++
title = "browser tool cards lack readable summaries in chat (no argLabel branches)"
created = "2026-08-28"
+++

BUG (plan 5ae26d22): browser/offscreen browser tool cards in chat show a bare name with raw JSON hidden behind expand — no URL/selector/what-happened summary (user: "none of the browser commands show a nice summary"). Root cause: frontend Message.tsx argLabel has branches for shell/git/spawn_agent/search/graph but none for the browser family, and toolCardPaths.ts has no result-side chip parser for browser outputs. Fix: argLabel branches (navigate→url, click→selector, type→selector+text, eval→truncated expression) for browser_*/offscreen_browser_* (the game_* names no longer exist — dropped per review round 2); new browserResultInfo() chip (landed URL / page title / PNG filename) rendered as a trailing ToolCard chip. Regression: messageArgLabel.test.ts "argLabel — browser" + toolCardPaths.test.ts "browserResultInfo".
