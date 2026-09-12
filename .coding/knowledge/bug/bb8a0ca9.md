+++
title = "browser tools visible in every workflow state — MERGED (approval gate is the guard)"
created = "2026-08-27"
+++

BUG (plan bb8a0ca9, commit a3f8c31 on wt/agenticcoder, MERGED into main at a8a78e3 2026-09-09 via the plan-5ae26d22 merge, branch deleted): browser tools (both headless offscreen_browser_* and the live Browser-tab browser_*) are visible in EVERY base workflow state (Planning/Executing/Reviewing/Complete). Previously ToolFilter hid mutating browser tools in Planning/Complete (AutoRun-only) and all browser tools in Reviewing — which blocked the user's primary ask ("navigate the Browser tab so I can follow") whenever the agent rested between tasks. Rationale: browser tools drive the user-visible Browser tab / headless pages and never project files, so the "no changes without a plan" rule does not apply; their NeedsApproval level prompts the user on every call — the approval gate (not state hiding) is the guard. Regression: tool::tests::browser_tools_available_in_every_base_state (src/tool/mod.rs). Skill/Reviewer allow-lists unchanged; safety_levels unchanged.
