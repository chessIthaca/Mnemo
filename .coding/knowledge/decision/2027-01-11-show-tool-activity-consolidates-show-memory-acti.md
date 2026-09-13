+++
title = "show_tool_activity consolidates show_memory_activity — render-time filter, default TRUE"
supersedes = "2026-12-30-show-tool-activity-consolidates-show-memory-acti"
created = "2027-01-11"
+++

DECISION: show_tool_activity consolidates show_memory_activity — render-time filter, default TRUE (flipped 2027-01-13, backlog 57687857, user request "show tool results by default — all we ask for is change the default"). One toggle [ui] show_tool_activity governs ALL agent-activity chat cards (tool calls, memory reads/writes, vision image-parsing, skill announcements). Semantics: absent key in config.toml → Default `true` (serde Option fallback); an EXPLICIT `show_tool_activity = false` in a per-user config still hides the cards — explicit value always wins over the new default (users who already opted out are untouched). GUI-only render-time filter: the transcript store always keeps every entry and the model's context echo is built server-side (unaffected); the Output tab and console log every tool call regardless. The prior decision (default FALSE, shipped with plan 0e71e4f9 / commit a65862d) is superseded by this record — see history. Knowledge record: .coding/knowledge/decision/2027-01-11-show-tool-activity-consolidates-show-memory-activity-render.md (superseded variant keeps the old text).
