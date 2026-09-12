+++
title = "Eager Complete→Planning nudge (work-intent heuristic + directive STATE_COMPLETE)"
created = "2026-08-26"
status = "superseded"
+++

DECISION (2026-09-06): The agent now more eagerly switches from Complete to Planning when the user's message implies code changes. Two layers:

1. STATE_COMPLETE (src/agent/prompt.rs) enhanced from terse "Write tools need a new plan" to a directive: "If the user's message implies code changes (fix, add, implement, refactor, update + a code reference), treat it as a NEW TASK: explore with read tools, then call create_plan — do NOT answer freeform. Only answer freeform for pure questions (how/where/what)."

2. append_plan_nudge() (src/agent/prompt.rs) — a code-level work-intent heuristic that injects a "# PLAN NUDGE" directive into the volatile tail when state == Complete AND the latest user message contains a work verb. looks_like_work_intent() matches imperative verbs (fix, add, implement, refactor, …) via exact-or-suffix matching (verb OR verb+ing/ed/es/s/d/er/ers), NOT raw starts_with — so "address" doesn't match "add", "wireless" doesn't match "wire". English-only (CJK falls back to STATE_COMPLETE). Wired in turn.rs volatile-tail build block.

The nudge is a suggestion, not a forced transition — false positives are harmless (agent can still answer a question); false negatives fall back to STATE_COMPLETE. Branch wt/eager-complete-to-planning (tip 53d0016), NOT yet merged to main. Tests: root 1518+0, src-tauri 169+0. Review: PASS (rereview at .coding/reviews/2026-09-06-eager-complete-to-planning-rereview.md).
