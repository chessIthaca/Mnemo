+++
title = "spawned reviewers run on the [models.reviewing] model (spawn-time pin)"
created = "2026-08-27"
+++

SPEC: Spawned reviewers run on the [models.reviewing] model — MERGED into main at 8560eaff (2026-08-27, plan 6e644ae3). Rationale: a spawned agent's loop is born in WorkflowState::Planning and never transitions, so per-turn resolution never consulted [models.reviewing] — with no [models.subagent] a role:"reviewer" spawn silently ran on the PLANNING model. Fix (src/tool/agent/spawn_agent.rs, tool layer only — spawn.rs/loop_impl.rs/workflow untouched): when role=="reviewer" && no explicit model arg, pin at spawn via resolver.resolve(Reviewing, skill=None, is_subagent=false).or_else(resolve(Reviewing, ..., true)) — i.e. precedence explicit spawn_agent(model=…) > reviewing.or(executing) (back-compat) > subagent > default provider — fed through the existing set_forced_model seam (parent-aware path only; plain path unchanged). Snapshot-at-spawn: a Settings save mid-review does not re-route. Workflow-state machine and the main agent's exit gate are UNCHANGED (finish structurally impossible for subagents; reviewer role is tool-surface, not state).
