+++
title = "finish gate on FINDINGS verdict"
created = "2026-08-22"
+++

Re-review after fixing findings must review the FIX COMMIT, not the working tree: if you commit the fixes before re-review, git_diff shows nothing. Spawn the pass-2 reviewer with the commit sha (git show <sha>) and the original findings list.

Amended 2027-01-11: Tool-name correction + process update (plan f4636852, backlog 85313a7e). The reviewer's read-only git surface is the SINGLE `git_read` tool, ops `diff`/`log`/`show`/`status` — `git_diff`, `git_log` and `git_show` are not registered tools (they survive only as internal delegates inside `GitReadTool::new`), so this record's "git_diff shows nothing" means `git_read` with `op="diff"`, and "git show <sha>" means `git_read` with `op="show"`. Its core advice still holds on its own terms (re-reviewing a FIX COMMIT needs the commit, not the working tree), and since this plan it is largely superseded in mechanism: the reviewer spawn now stamps each round's base revision (`git rev-parse HEAD` at dispatch) on the plan frame (`.coding/plans/<id>.md`, the `## Reviews` section) and, from round 2 on, the app renders the delta scope into the reviewer's prompt itself — the base revision, the `git_read`-op instruction and the mechanically-derived changed file set — so the dispatcher no longer hand-writes "spawn the pass-2 reviewer with the sha". A missing or failed stamp only ever WIDENS the next round's scope (fail-safe).
