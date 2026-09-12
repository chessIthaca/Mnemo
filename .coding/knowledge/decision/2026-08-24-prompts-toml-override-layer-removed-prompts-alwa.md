+++
title = "prompts.toml override layer removed — prompts always compiled constants"
created = "2026-08-24"
status = "superseded"
+++

DECISION (user, 2026-09-05, branch wt/remove-prompts-toml, commit b310adc): remove the ~/.mnemo/prompts.toml override layer entirely — system prompts and tool descriptors are ALWAYS the compiled constants in src/agent/prompt.rs. Deleted: PromptBlocks/PromptSection/WorkflowStateBlocks serde structs + load_str, PromptSource/PromptHolder mtime-reload, apply_tool_descriptors, default_prompts_toml*/ensure_prompts_file* seeding, factory tool_descriptor_map, AgentLoopFactory prompt_source ctor param, with_prompt_source builder. New signatures: build_stable_head(constitution), build_volatile_tail(caps, workflow, memories), build_system_prompt(caps, workflow, constitution, memories); workflow_section uses STATE_* consts directly. Docs updated (README/PLAN/module). 16 prompts.toml tests removed; root 1467 + src-tauri 156 green. Old PLAN memories (f03e0c7f, 8fcab898, c47b3a2a, e2da061b) describing the editable-prompts design are OBSOLETE — superseded by this decision.
