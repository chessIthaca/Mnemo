## Decisions

- 2026-11-22: Scheduling across plans: step 1 may run in parallel with plan e5d93df3's step 5 once e5d93df3's step 4 has merged. To avoid conflicts, step 1 adds its TeamRuntime state field directly after `check_runs` in team.rs (e5d93df3 step 5 adds its map elsewhere), and appends tests at the end of team/tests.rs (step 5 inserts at its auto_escalation anchor). Steps 2 and 3 still wait for e52e0be1's steps 1 and 3.
- 2026-11-22: From step 1's audit: (a) Transcript paths are confined. A path is accepted only when its canonical form, with no links or junctions, lies under ~/.claude/projects and its file name is agent-<agent_id>.jsonl. A given path that fails this is ignored, the documented glob is used instead, and the result says so. The sibling agent-<agent_id>.meta.json is the only other file read. (b) Step 4 owns the frontend side of the new inbox wire values: sender claude_agent and kind progress, in mnemo2/src/lib/teamState.ts and WorkerInbox.svelte, labelled as the worker's agent and as a progress note. (c) Step 2 must call ensure_agent_watch whenever a lifecycle tool moves a registered agent worker back to Working or Fixing, so the watcher restarts.

## Interfaces

- 2026-11-22: WorkerRecord gains `claude_agent_id: Option<String>`, `claude_agent_transcript: Option<String>` (step 1) and `integrations: Vec<Integration { branch, commit, at_ms }>` (step 3), all serde default and persisted.
- 2026-11-22: The `agent` status object `{id, transcript, last_activity_ms, tool_calls, tokens, finished, stalled}` is produced by step 1 and rendered by step 4.
- 2026-11-22: `continue_agent: {agent_id, worktree, message}` comes from step 2, in team_message and team_route_fix answers for agent workers.
- 2026-11-22: Ownership: step 1 owns agent_watch.rs; step 2 owns the control.rs message branch and team_agent_done states; step 3 owns integrate.rs and the integration-note prefix in control.rs (placed after the fast-forward note); step 4 owns the frontend files and the transcript converter. Each adds tests only at the end of team/tests.rs, in merge order.
