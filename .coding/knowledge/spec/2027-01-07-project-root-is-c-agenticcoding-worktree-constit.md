+++
title = "project root is C:\\AgenticCoding worktree — constitution path C:\\AgenticCoder\\AgenticCoder is stale/nonexistent"
created = "2027-01-07"
+++

The session project root is C:\AgenticCoding — the single git worktree, branch wt/agenticcoding (verified 2026-09-07 via `git worktree list` → "C:/AgenticCoding 446b805 [wt/agenticcoding]"; shell CWD, git tool, and file tools are all rooted there). The project constitution's (agent.md) claim that "The project root is C:\AgenticCoder\AgenticCoder" is STALE — that path did not exist on disk at all (a Copy-Item to it during the wry-vendoring plan created C:\AgenticCoder\AgenticCoder\vendor\wry wholesale; it was removed and the copy redone at C:\AgenticCoding\vendor\wry). Guidance: never trust the constitution's absolute path — use relative paths for file tools, let the shell default to its CWD, and when in doubt run `git worktree list` / `Get-Location`. Consider fixing the stale path in agent.md (user decision — it's their policy file).
