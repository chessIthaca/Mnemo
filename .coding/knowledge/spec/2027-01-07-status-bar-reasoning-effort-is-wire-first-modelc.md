+++
title = "Status-bar reasoning effort is wire-first (ModelChanged/AgentInfo.reasoning_effort, display-space resolution)"
created = "2027-01-07"
+++

SPEC: The status bar's reasoning-effort indicator is WIRE-FIRST (backlog 51dab4da, plan 3e680de3): it displays the effective effort of the model actually serving the active agent's current context, reported by the backend — never the toolbar echo or the endpoint-level default alone.

Backend resolution (DISPLAY-space, UI vocabulary "off"|"low"|"medium"|"high"|"max"): Endpoint::display_reasoning_effort_for(model) (src/config/endpoints.rs) mirrors the wire chain (ModelSpec.reasoning_effort → endpoint.reasoning_effort → "max", supports_reasoning_effort gate → "off", reasoning_efforts allow-list clamp) but keeps "off" verbatim — the off-wire encoding ("none" on DeepSeek / off_wire) is a WIRE concern (client_factory::resolve_effort); display_normalize_reasoning_effort_for is the requested-value twin (early-outs "off" before the clamp). client_factory::resolve_display_effort(endpoint, model_ref) mirrors resolve_effort for per-context ModelRefs; ModelResolver::display_effort_for (trait, default None) exposes it to the loop.

Loop mirrors (loop_impl.rs): resolved_effort (the per-turn mirror, set at every resolve_turn_provider branch alongside resolved_model — skill/pin/forced/sticky/state branches via resolver.display_effort_for, the no-override branch via default_display_effort, the pin branch via pinned_display_effort); default_display_effort (the default slot's effort — factory-stamped at build via build_inner, updated by swap_provider_into_loops + main.rs startup init); pinned_display_effort (the pin's build-time effort — set by swap_provider_into_loop). All cleared/updated by the set_model swap paths.

Wire: AgentEvent/SerializableAgentEvent::ModelChanged gained reasoning_effort: Option<String> (serde default + skip when None); emitted at turn.rs resolve_iteration_provider (fires when the model OR the effort changes — the same model id can serve two contexts with different efforts), set_model (per-agent + global via swap_live_provider), save_endpoints. AgentInfo gained reasoning_effort (filled from resolved_effort().or(default_display_effort()) — pre-first-turn the factory stamp is the best value).

Frontend: agentWireEfforts map (useAgentStore — populated by reduceModelChanged [present upserts, absent CLEARS — never label the OLD effort against the NEW model] + registerAgents [presence-wins]) + StatusBar effectiveEffort precedence: agentWireEfforts[activeAgent] ?? agentEfforts[activeAgent] (toolbar echo) ?? effortForEndpoint(endpoint default); the label still clamps into the target model's allow-list.

Tests: display resolver units (endpoints.rs: display_reasoning_effort_for_mirrors_chain_but_keeps_off, display_reasoning_effort_deepseek_off_stays_off, display_normalize_reasoning_effort_for_clamps_and_keeps_off), mid_turn_skill_model_switch_emits_model_changed_events (asserts the effort rides the events), model_changed_roundtrips_json, useAgentStore.test.ts "model_changed: records the wire-reported reasoning effort for the agent" (the fail-pre-fix regression test).
