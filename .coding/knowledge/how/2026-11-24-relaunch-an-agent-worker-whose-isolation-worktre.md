+++
title = "relaunch an agent worker whose isolation worktree is not its pool checkout"
created = "2026-11-24"
evidence = "2026-11-24: the first agent's read-back reported a checkout 12 commits behind the base; the relaunch without isolation plus team_agent_started produced agent_phase running with live tool calls."
+++

Symptom: launching an agent worker's claude_agent block with isolation "worktree" can put the agent in a fresh folder under `.claude/worktrees/` on an old base commit, instead of the worker's warm pool checkout under `.worktrees/`. The agent sees none of the recently merged work and can't touch the pool, because the isolation refuses it. That folder is removed automatically when the agent ends with no changes. Seen on 2026-11-24 after team_resume moved a stopped Mnemo-model worker to the high tier; the plugin helper was current, so the cause is unconfirmed.
Recovery: run `git worktree list` to confirm the pool is on the worker's branch at the expected base. Then launch the Agent WITHOUT isolation, and in its prompt name the pool folder and the worker's branch explicitly, telling it to:
- use absolute paths, `git -C <pool>`, and cargo with the pool as the working directory;
- never touch the main checkout, push, stash, merge or switch branches.
Then call team_agent_started with the agent's id so Mnemo follows its transcript. Until that call, team_status shows agent_phase "awaiting_launch".

Amended 2026-11-24: Risk of this workaround: without isolation, the agent's tools can reach the main checkout, and one did. On 2026-11-24 a worker launched this way edited two lines of a source file in D:\Mnemo itself, besides committing the same change in its pool. That uncommitted stray then blocked every team_merge ("the lead working tree must be clean") until it was discarded, which needed the user's OK because discarding it is destructive. Before merging, run `git status --short` in the main checkout. If a stray source edit appears, compare it with the worker's pool commit; if it is an exact duplicate, ask the user before discarding it.
