+++
title = "why [models.planning|executing|bug_fixing|complete] can never choose Mnemo2's lead model (stateless TeamLead route)"
created = "2026-11-02"
+++

Mnemo2's built-in team lead is structurally immune to Mnemo1's per-task model slots ([models.planning]/[models.executing]/[models.bug_fixing]/[models.complete]).

Why: src/app/workflow_policy.rs — WorkflowPolicy::route() for TeamLead mode (the `mnemo.workflow` policy built by WorkflowPolicyFactory::with_team_lead, the only policy Mnemo2's lead uses) returns a stateless RouteKey { slots: vec!["lead"] }. ModelsConfig (src/config/general.rs) has no `lead` field, so ConfigModelResolver::resolve resolves nothing for that route (the resolver's tail, config.resolve_model_ref(None), always returns None). Both route consumers in AgentLoop::resolve_turn_provider_routed — the picker-pin yield check and the workflow-state chain — therefore never resolve a per-state model for the lead: it keeps its forced model (the lead picker's choice, threaded spawn_live → SpawnOptions.model → set_forced_model) or its default-slot provider in every workflow state, and an explicit pin never cedes.

Not protected by the route (still reachable, by design): [models.skill.<name>] while a skill runs, and the pre-prompt Laya routing arm (cheap/capable lanes) — both intended Mnemo2 behavior. [models.summarize] rides resolve_summarize_model, separate from routes entirely.

Consequences:
- Adding a `lead` slot to ModelsConfig would silently create a lead-only model override that DOES fire — only do that deliberately.
- Reverting the TeamLead route to the per-state build_route_key would resurrect the Mnemo1 model leak into Mnemo2's lead; the route is pinned by a kernel test in workflow_policy.rs (route test asserting slots == ["lead"]) and by the runtime-seam test `lead_ignores_the_mnemo1_per_state_models_across_a_state_change` in src-tauri/src/mnemo2/runtime.rs.
- The general leak shape still exists for CLASSIC WorkflowPolicy agents (Mnemo1 main agents): their routes carry the state slots and a configured [models.executing] takes over across Planning → Executing.
