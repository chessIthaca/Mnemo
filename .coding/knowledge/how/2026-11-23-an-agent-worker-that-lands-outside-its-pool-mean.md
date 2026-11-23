+++
title = "an agent worker that lands outside its pool means the plugin copy's helper is stale"
created = "2026-11-23"
evidence = "Observed 2026-11-23 with a Nov 16 helper in the cache copy; scripts/install-mnemo2-mcp.test.mjs"
+++

Claude Code runs the mnemo plugin's hooks from its installed copy under ~/.claude/plugins/cache/mnemo/mnemo/<version>; the WorktreeCreate hook calls that copy's bin/mnemo2-mcp with worktree-create. A helper too old to know the command prints its usage text and Claude Code silently falls back to its own .claude/worktrees folder on a new branch from the default branch; the Mnemo worker stays awaiting_launch and the Desktop workers band looks dead. scripts/install-mnemo2-mcp.mjs (run by the rebuild script) refreshes that copy's bin, so a rebuild fixes it. Quick check: pipe `{}` into `<copy>/bin/mnemo2-mcp worktree-create`; it must answer "unusable worktree name ''", not the usage text. Recovery for a run that already fell back: cherry-pick the agent's commit onto the worker's own branch in its pool, call team_agent_done, review and merge as usual, then remove the stray Claude worktree and its branch.
