+++
title = "Anthropic automatic caching rejected — manual breakpoint before the varying tail"
created = "2027-01-07"
+++

DECISION (2027-01-10, plan ee33d615): Anthropic AUTOMATIC caching (a single top-level cache_control field; the system applies the breakpoint to the last cacheable block and moves it forward) was evaluated for the agent loop's request shape and REJECTED. Rationale: our last cacheable block is the relocated volatile tail (per-turn-mutating workflow state/progress/memories), so the automatic breakpoint would land on the varying block — the docs' "Common mistake" trap (platform.claude.com/docs/en/build-with-claude/prompt-caching, fetched 2027-01-10 and re-verified live by the round-1 reviewer): a fresh write every turn and never a read. Chosen instead: explicit breakpoint 3 on the final message's last REAL block, just before the relocated tail — the documented "static prefix + varying suffix" rule ("place the breakpoint at the end of the static prefix, not on the varying block"). The 4th breakpoint slot stays unused: post-relocation the final block IS the varying tail, so a breakpoint there hits the same trap. Commit d5cfd52 on wt/agenticcoding.
