+++
title = "worker checkout removal waits for every owner of the worktree code graph"
created = "2027-01-11"
+++

On Windows a Mnemo2 worker's folder under .worktrees/agent-<id> cannot be deleted while anything holds its per-worktree code graph (.coding/codegraph.db) or reads its files. Rule: before merge/delete/cleanup removes a worker checkout, close the worker session with AgentSession::close_and_wait (src/headless/session.rs), which cancels once per agent and waits until the worker loop and every descendant (found via stable family lineage, so grandchildren of exited sub-agents count) are dropped; session-end consolidation must hold only the policy and an owned corpus, never the AgentLoop or worktree paths. Decide worktree registration from the main repo with `git worktree list --porcelain -z`; never run git inside an unregistered folder (a folder without .git resolves to the main checkout). Leftover removal is a guarded filesystem delete limited to the canonical <root>/.worktrees/agent-<id> path, refusing links and junctions.
