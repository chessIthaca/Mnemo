+++
title = "missing parent loop + no role gives a permissive Skill([]) filter (open)"
created = "2027-01-11"
+++

Open, not fixed (found by audit spawnFixAudit27 on 2026-10-31). In src/host/runtime.rs, compute_subagent_allowlist returns Some(vec![]) when a parent_id is supplied but that parent's loop isn't in the agent-loop map. With role None, spawn_agent_shared passes that list to set_role_allowlist(false, ..) -> set_tool_allowlist, which becomes ToolFilter::Skill([]) (src/app/workflow/mod.rs:498-502, 3475-3483). Skill auto-grants memory tools, ask_user and backlog mutations (src/tool/mod.rs:1080-1138), so the agent is not denied everything as intended. Reachable only through the public spawn_agent_shared API with a stale parent id; the normal spawn_agent tool route registers the parent loop first. The existing missing_parent_loop_denies_all test checks only the helper with role reviewer, so it can't catch this. Fix idea: reject the spawn when a supplied parent has no loop, plus a shared-path roleless regression test.
