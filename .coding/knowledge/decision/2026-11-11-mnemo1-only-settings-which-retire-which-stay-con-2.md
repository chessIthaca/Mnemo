+++
title = "Mnemo1-only settings — which retire, which stay config-only, which move to Mnemo2"
supersedes = "2026-11-11-mnemo1-only-settings-which-retire-which-stay-con"
created = "2026-11-11"
evidence = "User approval 2026-11-11. Correction evidence: src/agent/turn.rs:594-614 (budget_gate) and src/agent/budget.rs; src-tauri/src/mnemo2/model_settings.rs:75,161-175; the inventory report's Settings table."
+++

The user approved this split on 2026-11-11, while retiring Mnemo1. It was corrected the same day after the core inventory (.coding/analysis/2026-11-11-mnemo1-only-core-code.md).

STAY — live, NOT retired:
- [general.budget] (tokens per plan, escalations per plan, retries per step). The core enforces these caps on every turn: src/agent/turn.rs budget_gate and src/agent/budget.rs. They reach every factory-built loop, Mnemo2 workers included, and Mnemo2's Settings → Models page edits them. The first version of this record wrongly listed them as Mnemo1-only.

RETIRE (no live reader; removed in the core-cleanup plan):
- [general] auto_compact_on_plan_complete. Only Mnemo1's Run-All read it.
- [general] enable_browser_inspection. It retires together with the Mnemo1 WebView/CDP tools; headless and Mnemo2 always force it off.
- The [ui] chat and steering display toggles: show_token_usage, show_tool_images, show_tool_activity, show_knowledge_activity, show_delegation_notes, [ui.steering_notes], chat_thread_line, chat_prose_cap, chat_turn_tint, chat_hover_timestamps. Mnemo2 uses only theme and the sound keys from [ui].
- [general.laya] auto_finetune. Mnemo2's Tiers page still shows the toggle, but its only consumer (the Mnemo1 fine-tune) is gone, so the toggle is removed too.

KEEP CONFIG-ONLY (the shared engine applies them; no UI planned):
- [memory] retrieval knobs (defaults decay_half_life_days 20, derived_per_class_cap 10).
- [context] summarize_at_fill_rate.
- The per-task model slots [models.planning|executing|bug_fixing|complete], a kernel feature for embedders.

MOVED TO MNEMO2:
- [git] core_operations, on Settings → Safety ("Git commands that always ask").
