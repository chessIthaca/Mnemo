+++
title = "UI skill start with hand-edited target_state='subagent' panics allowed_tools"
created = "2027-01-04"
+++

SYMPTOM: A hand-edited .coding/skills/*.toml with target_state = "subagent" (or "skill"), started via the UI skill button (enter_skill, src-tauri/src/ipc/agent.rs:808), lands a MAIN-agent workflow in WorkflowState::Subagent (or Skill) with no tool allow-list → allowed_tools panics on the next turn ("Subagent-state workflow must carry a tool allow-list", workflow/mod.rs:397-399).

ROOT CAUSE: SkillRegistry::load_dir (src/skill/mod.rs) inserted every spec that parsed without checking target_state. enter_skill passes spec.target_state straight to start_skill with no validation. The LLM-reachable skill_start tool path was validated in plan 3fb064c4 (skill.rs:154-161), but the UI-initiated enter_skill path was not. WorkflowState serde is rename_all="lowercase", so "subagent"/"skill"/"reviewing" all parse to valid variants and flow through unguarded.

FIX (plan 7e9d03ec, branch wt/agenticcoding): Added a matches!(WorkflowState::Planning | Executing | Complete) gate in SkillRegistry::load_dir's Ok(spec) arm — a spec with a non-lifecycle target_state is logged to stderr and skipped, mirroring the existing malformed-file skip. Strongest close: bad TOML never reaches any path (neither enter_skill nor skill_start). The skill_start tool's existing gate (skill.rs:154-161) remains as the LLM-path guard + defense-in-depth for the args override.

REGRESSION TEST: load_dir_skips_skill_with_non_lifecycle_target_state (src/skill/mod.rs tests) — writes good + bad_subagent/bad_skill/bad_reviewing skills to a tempdir, asserts load_dir skips the bad ones. Fails without the fix (bad skills load), passes with it.

Pre-existing (not introduced by 3fb064c4), not LLM-reachable (requires user to click the skill button), no shipped skill affected (merge_to_main targets "planning").
