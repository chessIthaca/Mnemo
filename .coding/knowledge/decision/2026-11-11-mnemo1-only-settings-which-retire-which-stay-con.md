+++
title = "Mnemo1-only settings — which retire, which stay config-only, which move to Mnemo2"
created = "2026-11-11"
evidence = "User approval 2026-11-11. grep of mnemo2/src, src-tauri/src/mnemo2 and src/headless shows no reads of the [ui] chat/steering toggles. src-tauri/src/ipc/run_all.rs reads auto_compact_on_plan_complete."
+++

The user approved this split on 2026-11-11, while preparing Mnemo1's retirement.

RETIRE WITH MNEMO1 (no Mnemo2 port; delete together with frontend/ and src-tauri/src/ipc/):
- routing budgets [general.budget] (escalations per plan, retries per step, tokens per plan): Mnemo1 plan-loop machinery; Mnemo2 routes by tiers and Laya.
- [general] auto_compact_on_plan_complete: live in Mnemo1. Its Run-All reads it in src-tauri/src/ipc/run_all.rs to compact between overnight items. Mnemo2 has no Run-All.
- [general] enable_browser_inspection: Mnemo2 has no on-screen CDP browser, and the headless engine must ignore this flag.
- the [ui] chat and steering display toggles: show_token_usage, show_tool_images, show_tool_activity, show_knowledge_activity, show_delegation_notes, [ui.steering_notes], chat_thread_line, chat_prose_cap, chat_turn_tint, chat_hover_timestamps. No Mnemo2 code reads them. Mnemo2 uses only theme, sound_complete and sound_input_needed from [ui].

KEEP CONFIG-ONLY (the shared engine applies them in both apps; no UI planned):
- [memory] retrieval knobs. Defaults are decay_half_life_days 20 and derived_per_class_cap 10 since 2026-11-11.
- [context] summarize_at_fill_rate.

MOVE TO MNEMO2:
- [git] core_operations is edited on Settings → Safety ("Git commands that always ask").
