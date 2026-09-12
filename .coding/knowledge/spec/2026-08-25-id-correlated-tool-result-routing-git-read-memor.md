+++
title = "id-correlated tool_result routing + git_read/memory_search chips"
created = "2026-08-25"
+++

SPEC: tool-card result routing + info chips (plan 233773ab, commit 300eddf, branch wt/toolcard-result-routing, merged pending).
Memory transcript entries now carry their owning call's id/index/args (stamped at tool_call_start; arg deltas accumulate by index); reduceToolResult finalizes a memory entry ONLY on entry.id === tool_call_id — a foreign tool's result always falls through to its own tool card (fixes git_read stuck-running, backlog 12d7ecb8; regression test in useAgentStore.test.ts). UX: git_read cards show an op chip (log -N -- path / show <7-char commit> / diff path) via argLabel + argPaths exclusion; memory_search cards show the searched query (memorySearchLabel from args: "query" · record_type|tier · prefix · limit N) plus "N matched"/"no matches" parsed from the result header ("N memories matched:" / "no memories matched the query").
Path: .coding/knowledge/bug/2026-08-25-tool-result-stolen-by-memory-entry-git-read-card.md + review reports .coding/reviews/2026-08-25-gitread-stuck-card-review{,-pass}.md
