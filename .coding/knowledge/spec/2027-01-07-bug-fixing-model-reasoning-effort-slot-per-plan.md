+++
title = "bug-fixing model + reasoning-effort slot (per-plan-kind override)"
created = "2027-01-07"
+++

SPEC (2027-01-07, commit 405be6d on wt/agenticcoding, plan 104dbc43): `[models] bug_fixing` slot — per-plan-kind model + reasoning-effort override (user request: implementation plans on a lesser model/low effort, bug_fixing plans on max reasoning).

Resolution chain (src/model_resolver.rs): skill > subagent > bug-fixing plan kind > state > default. The bug-fixing arm engages ONLY while the active (top-of-stack) plan's kind is BugFixing AND the state is Executing or Reviewing — not Complete (plan finished → complete slot), not Subagent (role state), not Skill. Unset or dangling (endpoint deleted) inherits the executing slot, then default — each link validated independently via Config::resolve_model_ref.

Mechanics: ModelContext::new takes a 4th arg `plan_kind: Option<PlanKind>` (compiler forces every site to consider it); Workflow::active_plan_kind() (src/workflow/mod.rs) reads the top of the plan stack; the agent loop threads it through resolve_turn_provider into the pin-check + main-chain ModelContext (skill_only probe passes None). The switch lands on the NEXT provider request after create_plan(kind=bug_fixing) — same turn.

Run-all dispatch (src-tauri/src/ipc/run_all.rs) needs NO code change: the dispatch sends a plain Prompt through the main agent's per-iteration resolution, which picks up the slot the moment the RUN_ALL_STEER-steered create_plan flips the workflow. Pre-classifying the item at dispatch was rejected — it would duplicate the agent's create_plan judgment and could pin the wrong model for the whole session. The reviewer chain (resolve_reviewer_model) is deliberately unchanged.

Wire/DTO: ModelsConfigWire.bug_fixing + ModelsConfigDto.bug_fixing (nullable-optional set/clear/keep). Frontend: FIXED_SLOTS row "Bug fixing" after Executing (ModelsSection.tsx); ModelsConfig.bug_fixing (tauri.ts).

Tests: model_resolver unit tests (bug_fixing_plan_kind_resolves_the_bug_fixing_slot, bug_fixing_slot_unset_or_dangling_inherits_the_state_chain, bug_fixing_slot_only_engages_in_the_plan_lifecycle), run_all_dispatch_lands_bug_items_on_the_bug_fixing_slot (acceptance scenario), picker_pin_yields_to_bug_fixing_slot_in_a_bug_plan (review LOW-2), bug_fixing_patch_set_clear_keep (DTO), vitest round-trip + source contracts. Docs: README.md per-context sentence, PLAN.md model-routing section.
