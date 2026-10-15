+++
title = "Job 2a complete — the agent-policy seam landed (the whole-job record)"
created = "2027-01-11"
+++

Job 2a — THE AGENT-SEAM JOB — COMPLETE (steps 2a.0-2a.8; sub-plans 64962938/1011d05a/0068b0a4/ad14536e/339dfe72/b3867b42/5ade43bc/dc98b5fa/2fc1d12d; master 694f5a97 step 4/11 ticked; tree = 4569974 on wt/mnemo).

THE LAND (the kernel's couplings to today's Workflow now flow through AgentPolicy — src/agent/policy.rs's 12 members + NoPolicy + the types; WorkflowPolicy wraps the agent's own workflow, factory-attached since 2a.2):
- 2a.0: the pinned goldens added first (workflow_stable_head_bytes_unchanged — GOLDEN_HEAD len 12351 sha256 7f…; workflow_policy_routing_table_unchanged — the 10-cell table) + 2a.4's volatile_tail_section_order_pinned. 1b90d9a/e4c1c22.
- 2a.1: AgentPolicy verbatim (spec :204-231) + KernelPromptBlocks + assemble_stable_head + ToolSurface. f50f47b/4b2c829/6e2e2dd.
- 2a.2: WorkflowPolicy (the skeleton, register_tools no-op per D5) + the loop's policy/set_policy accessors + the factory attach. 2692274/212552b/622d2c2/d4194ac.
- 2a.3: the tool_surface consults (the dispatch gate + the turn advertisement; advertise = the plan-frozen schema_filter — NOT allowed_tools; the None → the raw-read fallbacks). 154ed54/6ece926.
- 2a.4: the head/tail builders — stable_head consults the policy; the volatile tail's workflow-half moved into WorkflowPolicy::volatile_tail (kernel keeps memory-block + CAPABILITIES + app_tail). 6177d95/1746b2d/0843a08/4a39f05/27da303.
- 2a.5: on_prompt (the unattended marker read; the unwrap_or_else fallback = the runtime's own derivation) + on_turn_end (the workflow_expects_progress predicate VERBATIM via the TurnSummary.unattended rider; TurnSummary::stop_reason became Option<StopReason> — the enum has no plain-Stop variant) + the gate consult (the descendants/subagent/streak mechanics untouched) + the six runtime arm tests' pre-Arc WorkflowPolicy attaches. 11d7131/a79f625/fc2b37d.
- 2a.6: build_route_key extracts the ordered slot chain; ModelContext = {route: RouteKey, is_subagent} (the compat ctor keeps the call sites — the 43-count grep acceptance); the resolver walks the slots; [models.free] + the DTO; the loop back-ref (OnceLock<Weak>) + route()'s real delegate; the golden green unreworked. cad67fc/0d27ff9/7e9dd3b.
- 2a.7: compute_subagent_allowlist's parent consult (the tool_surface gate; the None → the raw read; the lock discipline preserved). b9f76c6/e0f244f.
- 2a.8: spend_tags (the read_plan_scope mirror) + after_tool (ONE post-merge consult point) + on_session_end (the no-op delegate, consulted inside spawn_consolidation before the teardown). AFTER_TOOL HOOK REACH (the reviewer-corrected statement): the hook consults once per ATTEMPTED tool call, with the attempt-final result the model sees — only the not-run synthesized results are skipped; the deny-latch/denial/unknown-tool/malformed-args synthesized error results DO flow through into the consult. 671fa80/6bc15b8/4569974.

Reviews: every step ran rounds to a terminal PASS (2a.4 needed four rounds — the escape-transport saga; 2a.5/2a.6/2a.8 each one doc/test LOW fixed then PASS; 2a.7 PASS straight). The last round before every finish/step-close: PASS.

Job 2a NOT landed: nothing was deleted from the kernel — every consult kept its raw-read fallback for NoPolicy loops; the Workflow still lives app-side. NEXT: job 2b — the app implementation moves under mnemo::app (the module relocation; the step file .coding/specs/headless-free-agent/job-2b-app-move.md NOT YET AUTHORED — the coordinator writes it from the master spec §5 with the anchor-verification pass BEFORE the probes). The INTER-JOB UNIT first: the recall-overhaul backlog item #4100f5e3 (the user's Both call — runs before 2b opens). The forward note from 2a.7's reviewer: when 3a's engine/session builders gain set_policy sites, the gate-is-allowed_tools identity relies on the factory attaching the policy with the loop's OWN workflow_handle — carry the one-line warning into the 3a step file. The goldens trio gates every later job: workflow_stable_head_bytes_unchanged + workflow_policy_routing_table_unchanged + volatile_tail_section_order_pinned.
