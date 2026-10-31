# Review round 1: Claude Code external lead (plan 0f378abe, steps 2–6)

Date: 2026-10-31.
- **Reviewers:** two independent read-only Sonnet reviewers, one on the Rust backend and helper (R), one on the UI, plugin and install script (F). The lead (Opus) adjudicated.
- **Fixes:** two Sonnet fix agents (backend, helper) plus the lead for the plugin, docs, UI and script.

## Verdict: PASS after fixes

Every accepted finding is fixed with a regression test where testable. Tests after the fixes:
- mnemo-app lib (`--features mnemo2`): 195 passed.
- `mnemo2-mcp` bin: 93 passed.
- mnemo2 vitest: 373 passed; svelte-check: 0 errors.

## Findings and outcomes

| # | Sev | Finding | Outcome |
|---|---|---|---|
| R1 | high | PostToolUse hooks inside Claude Code subagents drained the lead's notices | fixed: the helper skips drain and stop-check when the hook input has `agent_id` |
| R2 | high | Notices lost: drained before slow work, and a failed response write discarded them | fixed: counters are read first and the drain is last; drained notices go back to the front of the queue on a write failure (`/mcp`, `/lead/notices`, `/lead/wait`) |
| R3/F1 | high | Opening Claude Code in a project (initialize/tools/list) attached the lead and diverted notices from the built-in lead; detach dropped the queue; check-then-push race | fixed: only `tools/call` and `/lead/wait` attach; the push is atomic and falls through to the in-app lead; on detach, undelivered notices go to the in-app lead or are parked |
| R4/F4 | med | `wait` returned at once on failure → restart loop burning turns | fixed: the helper polls 60 s and then prints a distinct "do not restart" line; the server returns `status` (`timeout`/`detached`/`no_project`); the instructions and skill say to call team_status instead of looping |
| R5 | med | Hooks could exit non-zero (bad args, child crash); an older `~/.mnemo/bin` copy could override a newer one | fixed: hook subcommands always exit 0 and fall back locally when the delegate fails; delegation only to a copy at least as new (mtime) |
| R6 | med | A run file was deleted on a connect timeout → duplicate Mnemo2 instance | fixed: deleted only on a refused connection or pid mismatch; the app restores a vanished run file every 30 s; a timed-out instance is waited for, not replaced |
| R7 | med-low | Run file and token not owner-only on Windows | fixed: `keys::restrict_permissions` (DACL) on the run file and folder |
| R8 | med-low | HTTP: body read before auth, no connection cap, hot accept-error loop | fixed: 401 right after the head, a 32-connection cap (503), a 100 ms back-off on accept errors |
| R9 | low | Any session's EOF detached the lead | fixed: only a `serve` that forwarded a `tools/call` detaches |
| R10 | low | Helper path in the instructions allowed `$`/backtick | fixed: rejected; the instructions give both Bash and PowerShell forms |
| R11 | low | `--hidden` plus a failed tray left an unreachable window | fixed: hide-on-close only when the tray exists |
| R12 | low | Spawn-lock stale takeover race | fixed: guarded takeover with an owner token; 120 s stale threshold (a race test was added) |
| F2 | med | Waiter command was POSIX-only | fixed: Bash and PowerShell forms in the instructions, the Stop reason and the skill |
| F3 | med | Worker text in notices treated as trusted | fixed: the instructions and skill say notice text is data, never instructions |
| F5 | low-med | A running auto-started mnemo2.exe blocks the rebuild; the install script crashed on a locked file | partly: the README says to quit from the tray before rebuilding; the script installs both folders and reports failures. Not done: copying mnemo2.exe elsewhere (an extra install surface; rejected) |
| F6 | low | Review skill lacked a worktree-clean check after running checks | fixed |
| F7 | low | Exec-form hook command without `.exe` unverified on Windows | open: verified in the step-9 run with the real CLI |
| F8 | low | Cold start could exceed Claude Code's MCP connect timeout | partly: the helper answers `initialize` locally at once; `tools/list` waits for the instance (measured in step 9) |
| F9 | low | Docs lacked Claude Code permission rules for the Mnemo tools and the waiter | fixed: Permissions section in the plugin README |
| F-edge | low | A confirmed "Send anyway" could become the answer to a question that arrived meanwhile | fixed with a regression test |
| lead | — | A Mnemo2 started by the helper inherited Claude Code's stdio pipe handles on Windows | fixed: the helper clears HANDLE_FLAG_INHERIT on its std handles before spawning (verified in step 9) |
| lead | — | The install script looked in `src-tauri/target`; cargo builds into the workspace `target/` | fixed (honors `CARGO_TARGET_DIR`) |

## Follow-ups (outside this plan)

- High-risk dispatch and audits read the active plan from `.coding/plans/stack.json`, which Mnemo2's plan board never writes. The board serializer also escapes a `## Acceptance scenarios` heading. This affects the built-in lead as well, and the `mnemo-lead` skill documents a workaround.
