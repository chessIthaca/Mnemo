+++
title = "skill target_state validation layers (registry load + skill_start tool)"
created = "2027-01-04"
status = "superseded"
+++

Skill target_state invariant is enforced at TWO layers (plan 7e9d03ec, commit e86ea44, branch wt/agenticcoding):

1. **SkillRegistry::load_dir** (src/skill/mod.rs) — a spec whose target_state is not a lifecycle state (planning/executing/complete) is logged to stderr and skipped at load, mirroring the malformed-file skip. Strongest close: bad TOML never reaches any path (closes the UI enter_skill path in src-tauri/src/ipc/agent.rs:808; defense-in-depth for the skill_start spec path).
2. **SkillStartTool::execute** (src/tool/workflow/skill.rs:154-161) — rejects non-lifecycle target_state from the args override OR spec with a tool error (the LLM-reachable path; load_dir cannot see runtime args).

Rationale: WorkflowState::Subagent requires an allow-list invariant a main-agent workflow cannot satisfy (allowed_tools panics, workflow/mod.rs:397-399); Skill is an overlay, never a landing state. Both diagnostics phrase the valid set in the lowercase serde form TOML/JSON accept (rename_all="lowercase"). Regression test: load_dir_skips_skill_with_non_lifecycle_target_state (src/skill/mod.rs tests).
