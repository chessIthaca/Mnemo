+++
title = "agent workers get their pool through Claude Code's WorktreeCreate hook"
created = "2026-11-23"
evidence = "Real run 2026-11-23: worker liveRunReadme01 in pool-1, merged and released with build caches kept. The hook failed first with a usage error (main routing), then with \"did not answer the worktree claim\" (404, route table)."
+++

An agent-tier dispatch returns a claude_agent block. The lead launches Agent with isolation "worktree" and the block's prompt, and the plugin's WorktreeCreate hook (`mnemo2-mcp worktree-create`, mnemo2_mcp/hooks.rs) POSTs /lead/worktree-claim. Mnemo hands out the waiting worker's pool and records claude_agent_id plus the transcript, so team_agent_started isn't needed. A new hook command has to be wired in three places: hooks.json, main()'s command match in mnemo2_mcp.rs, and the server's top-level route table in mcp_server.rs. Unit tests on parse_hook or worktree_claim alone miss a gap in either of the last two. Fresh pools start without node_modules; the checks runner now installs them (src/tool/agent/run_checks.rs). Agents still tend to put `cd <pool> &&` in front of their commands on their own, although the prompt doesn't ask for it.
