+++
title = "Eager Complete→Planning nudge (MERGED into main at 14b887c)"
supersedes = "2026-08-26-eager-complete-planning-nudge-work-intent-heuris"
created = "2026-08-26"
+++

MERGED into main at 14b887c (14b887c7ffc6be65e6bd09142c017972726a555f) on 2026-08-26. Branch wt/eager-complete-to-planning (pre-merge tip 206149a) landed via merge into main.

DECISION: The agent now more eagerly switches from Complete to Planning when the user's message implies code changes. Two layers: (1) STATE_COMPLETE (src/agent/prompt.rs) enhanced to a directive — plan for code-change tasks, answer freeform only for pure questions. (2) append_plan_nudge() — a work-intent heuristic (looks_like_work_intent) that injects a "# PLAN NUDGE" into the volatile tail when state == Complete AND the latest user message contains a work verb. Exact-or-suffix matching (verb OR verb+ing/ed/es/s/d/er/ers), NOT raw starts_with — so "address" doesn't match "add". English-only (CJK falls back to STATE_COMPLETE). Wired in turn.rs volatile-tail build block. The nudge is a suggestion, not a forced transition. Tests: root 1518+0, src-tauri 169+0.
