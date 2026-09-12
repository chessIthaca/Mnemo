+++
title = "skill prompts terse + rationale in header comments; skill_start never echoes the goal"
created = "2026-08-29"
+++

DECISION (2026-09-10, plan b1b45b69, branch wt/agenticcoder): skill prompts are TERSE operative checklists — the injected `prompt` field carries only instructions (it enters the system prompt EVERY turn the skill runs); rationale/lore lives in the TOML header comment, which TOML parsing never injects and thus costs zero context. Applied to merge_to_main: prompt cut ~50% (18 lines → 8, every operative rule kept, exact phrase "MERGED into main" preserved — pinned by src/skill/mod.rs test shipped_skill_files_parse_and_merge_to_main_cleans_up_memories). Second rule: skill_start's result must NOT echo the prompt (it's already injected per-turn by prompt.rs workflow_section) — result names skill + target only; regression test start_skill_result_does_not_echo_the_prompt in src/tool/workflow/skill.rs. Caveat: seed_skills is write-if-missing, so existing projects keep their old copy until the file is deleted (self-heal) — new projects get the trimmed one.
