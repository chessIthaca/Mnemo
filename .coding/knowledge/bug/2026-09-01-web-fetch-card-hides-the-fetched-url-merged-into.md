+++
title = "web_fetch card hides the fetched URL — MERGED into main"
supersedes = "2026-09-01-web-fetch-card-hides-the-fetched-url-no-arglabel"
created = "2026-09-01"
+++

BUG: web_fetch card hides the fetched URL (no argLabel case) — MERGED into main at 4173dda (2026-09-19), branch wt/agenticcoder deleted. Root cause/fix unchanged: ToolCard chips come from buildPathChips + argLabel; web_fetch args {url, max_length} carry no path/file field and argLabel had no web_fetch case → webFetchLabel helper (toolCardPaths.ts, 60-char cap) dispatched from argLabel; regression tests in toolCardPaths.test.ts (incl. argLabel source-contract test).
