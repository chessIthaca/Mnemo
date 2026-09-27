+++
title = "read-only tools denied inside skills — Skill arm now grants Agent+AutoRun wholesale (mcp__ excluded)"
created = "2027-01-11"
+++

Symptom: inside the Skill workflow state, read-only tools were denied unless the skill's allow-list named them — `read_files` rejected inside merge_to_main ("not allowed in the current workflow state (Skill)"), forcing a Get-Content shell workaround while reading .coding/knowledge/** for landing-record supersedes (incident 2026-09-26, the wt/mnemo → main PR close-out, plan 2f74e10a).

Root cause: the Skill arm of `ToolFilter::allows` (src/tool/mod.rs, `ToolFilter::Skill`) granted `ToolCategory::Memory` wholesale plus a fixed name list, so every Agent-category read tool (`SafetyLevel::AutoRun`) required an explicit allow-list entry — and the hand-maintained lists drift (merge_to_main's entry named the legacy `file_read` shim, so the model's live `read_files` call missed it).

Fix (plan 32f8da56, backlog 834ec126): the Skill arm now auto-grants `ToolCategory::Agent` + `SafetyLevel::AutoRun` wholesale — the read-only contract (every mutating tool is NeedsApproval) — with TRUSTED `mcp__` tools excluded by name, mirroring the Planning/Complete arms (trust ≠ state visibility; a trusted MCP tool is AutoRun for APPROVAL only). Reviewer arm, base-state arms, `skill_create` and `write_review_report` guards untouched. A skill's allow-list now scopes only the MUTATING tools.

Regression tests: `tool::tests::skill_state_grants_the_full_read_surface` (empty allow-list: the read set allowed incl. the graph quartet, the write set denied, `mcp__server__tool` denied, `write_review_report` denied even when named) and `agent::factory::tests::skills_get_the_whole_read_surface_at_the_registry_level` (walks the wired registry: every Agent+AutoRun tool readable, every Agent+NeedsApproval denied, the set asserted EXACTLY so a new tool must be classified consciously). Docs synced: docs/FEATURES.md, PLAN.md, ToolFilter::Skill doc, SKILL_FILE_HEADER, skill_create description, SkillSpec.tools doc, the repo skills' comments.
