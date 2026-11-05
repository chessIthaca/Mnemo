+++
title = "Mnemo2 lead-facing text locations (LEAD_INSTRUCTIONS, external_summary, guard test)"
created = "2026-11-04"
+++

Where Mnemo2's lead-facing text actually lives (useful when a brief says "in src-tauri/src/bin/mnemo2_mcp/"):

- The MCP server lead brief is `LEAD_INSTRUCTIONS` in `src-tauri/src/mnemo2/mcp_server.rs` (filled by `lead_instructions(helper)`, returned on MCP `initialize`). The `src-tauri/src/bin/mnemo2_mcp/` bin only imports it (`serve.rs`), so a brief that scopes instruction edits to the bin is pointing at the wrong file; edit `mcp_server.rs`.
- The SessionStart tier summary is not built in the bin at all: the server's `TeamRuntime::external_summary` (`src-tauri/src/mnemo2/team.rs`) appends `tier_guidance()`, which iterates `TierLadder::ordered()` and renders each rung's editable name, definition, model, price, effort and fold-down destination. It therefore follows the ladder automatically (five tiers: trivial, routine, medium, high, escalate) — no per-tier text to update when tier names change.
- The bin-side text surface is only the JSON pass-through (e.g. `summary.rs` `team_summary`), so a fixture there may still carry a retired tier id; those are test payloads, not guidance.
- Guard test: `plugin_skills_and_instructions_name_only_listed_tools` in `mcp_server.rs` scans `LEAD_INSTRUCTIONS` plus every `mnemo2/plugin/skills/*/SKILL.md` for `team_*`/`plan_*`/`memory_*`/`graph_*`/`review_fix`/`open_mission_control` tokens and fails if a name is not in `tools/list` — so new skill prose naming a real tool also needs that tool listed, and prose must avoid inventing tool-shaped words.
