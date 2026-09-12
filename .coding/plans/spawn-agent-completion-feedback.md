# Plan: spawn_agent completion feedback loop + auto-combine

## Goal
When a tool-spawned background agent finishes its task, automatically notify its
parent agent (which surfaces in the parent's window and prompts it to combine the
children's results into a plan). Also show the spawned agent's name in the
`spawn_agent` ToolCard, and fold in the quick-win review fixes.

## Why (the gap this closes)
`spawn_agent` was fire-and-forget: the child went idle on `Finished` (staying
alive) and was only removed on `Exited`. There was **no parent linkage, no
completion event back to the spawner, and no "results ready" notification** — so an
orchestrating agent had no signal that its background agents were done and never
auto-combined their reports. This round's three reviewers finished and the parent
had to be told by hand.

## Status: IMPLEMENTED (this pass)

### A. Completion notification + auto-combine
- `src/runtime/channels.rs`: `AgentHandle.parent_id` + `AgentHandle::with_parent`;
  `AgentEvent::ChildFinished{child_id,name,success}` + serializable mirror +
  `into_serializable` arm.
- `src/runtime/mod.rs`: `AgentManager::parent_id(id)`; `AgentSpawner::parent_aware()`
  (default `None`, object-safe) + new `ParentAwareSpawner` trait.
- `src/agent/mod.rs`: `AgentLoop.agent_id` + `agent_id()` + `with_agent_id()`.
- `src/agent/factory.rs`: `build_with_id(id)` / `build_inner(Option<id>)`; registry
  build passes the id to the `spawn_agent` tool (`SpawnAgentTool::with_parent`).
- `src/tool/agent/spawn_agent.rs`: `parent_id` field + `with_parent`; `execute`
  routes through `spawn_with_parent` when the spawner is parent-aware and an id is
  known, else falls back to plain `spawn`.
- `src-tauri/src/ipc/commands.rs`: `spawn_agent_shared(..., parent_id)`;
  `IpcSpawner` implements `ParentAwareSpawner`; UI-button spawn passes `None`;
  `main.rs` builds the main agent with `build_with_id`.
- `src-tauri/src/ipc/events.rs`: `notify_parent_on_completion` — on a child's first
  `Finished` / final `Error`, reads `parent_id`, sends the parent a `Suggestion`
  ("read its report and combine the results into a plan"), and emits `ChildFinished`.
  Deduped per-child (`notified_children` set), cleaned up on `Exited`, no-op when
  there's no parent.
- Frontend: `lib/types.ts` `child_finished` kind; `useAgentStore.ts` handles it
  (marks the child done, adds a ✓/✗ note); `Message.tsx` `argLabel()` shows
  `spawn_agent (name)`.

### B. Quick-win review fixes (done)
- rust#1 (Major): cached-token heuristic gated on `!did_summarize` + regression test.
- rust#2: stats `session_id` uses the `run_turn` param.
- rust#3: `SpawnAgentTool::new` doc comment.
- ipc#4: `start.bat` `tauri.cmd` guard moved above the `dev` branch.

## Verification
`cargo test` 364 passed / 0 failed (was 357; +6 spawn/parent/child_finished tests,
+1 cache-heuristic regression). Frontend `tsc --noEmit` clean.

## Follow-ups (EXECUTED — second pass, see plan 4d32c9b2 style / this file's sibling `b92b4438` in .coding/plans)
All actionable deferred items from CONSOLIDATED.md were executed in the follow-up pass:
- StatsView refresh-split + unified error handling (fe#1/fe#2/fe#3).
- StatusBar set_model failure surfacing + re-sync (fe#4) + dropdown a11y (fe#5).
- setCode*Color consolidation (fe#6); CodeBlock copy via textContent (fe#7).
- memory/types.rs field docs (rust#4); correction.rs trigger tightening (rust#5).
- set_model factory-check reorder + default_model fallback + blank line (ipc#3/ipc#1/ipc#6).
- Production CSP + dev-mode devCsp in tauri.conf.json (ipc#8) — manual dev smoke test recommended.
Reviewed-no-change: ipc#2 (cosmetic spawn race), ipc#7 (eprintln consistency), rust#6 (deliberate fire-and-forget stats).
Verification: cargo test 365/0, tsc --noEmit clean, prod bundle builds.
