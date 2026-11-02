+++
title = "A shell tool call owns its whole process tree; nothing it starts outlives the call"
created = "2026-11-02"
+++

src/tool/agent/shell.rs: every shell call puts the spawned shell in a Job Object with JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE (Windows) or its own process group (Unix), owned by TreeKillGuard, which kills the tree when the call returns, times out or is interrupted. After the shell exits, the pipe-reader join waits at most READER_JOIN_GRACE (3 s); if a background process still holds the pipes the readers stop gracefully and the output starts with "NOTE: a background process started by the command kept the output pipe open; it was stopped…". Why: backgrounded dev servers (npx vite) inherited the pipes, hung worker calls for 50-90 minutes and kept node.exe alive holding the worker worktree, blocking team_merge cleanup. Consequence: agents cannot run dev servers or other long-lived processes through the shell tool; the lead renders previews. Regression tests: background_child_holding_the_pipes_does_not_hang_the_call, timed_out_command_kills_the_background_grandchild.
