# Review: spawn_agent ToolCard display + PromptDispatched emit

**Date:** 2026-04-04
**Plan:** Make spawn_agent's ToolCard read "Spawn Agent (name)" and emit a PromptDispatched event so the spawned agent's task shows as the first user message in its window.

**Scope reviewed:** `git diff HEAD` — `src-tauri/src/ipc/commands.rs`, `src-tauri/src/main.rs`, `frontend/src/components/chat/Message.tsx`, plus workflow bookkeeping (`.coding/plans/*`, `stack.json`).

---

## Correctness

### C1. PromptDispatched emit fires for the correct agent id ✅
`IpcSpawner::spawn_with_parent` (`commands.rs:406-421`) calls `spawn_agent_shared(...)` which returns `(id, _)`, then emits `emit_prompt_dispatched(&self.app, id, task, &[])` using that returned `id`. The `id` is the freshly-allocated agent id from `mgr.next_id()` (`commands.rs:311-314`), and the same id is registered (`mgr.register(handle)`, `:330`) and used to send the initial `AgentCommand::Prompt` (`:343`). So the emit is tagged with the exact agent the task was dispatched to. Correct.

### C2. No race: event is never dropped for an unknown agent ✅
The concern was whether `PromptDispatched` could arrive before the frontend has registered the new agent in its store, causing the event to be dropped. It is **not** dropped:

- `applyAgentEvent` (`useAgentStore.ts:1076-1077`) opens with `const agent = getOrCreate(s.agents, agentId)`, and `getOrCreate` (`:485-490`) returns `agents[id] ?? emptyAgentState()`. So an event for an agent the frontend has never heard of is handled against a fresh empty state — it is **not** ignored.
- `reducePromptDispatched` (`:974-983`) appends a `kind: "user"` transcript entry to that (possibly fresh) state, and the uniform write path (`:1121-1139`) writes it back as `agents: { ...s.agents, [agentId]: result.agent }`. So the agent entry is created on demand.
- The display-name resolution (`useAgentEvents.ts:142-152`) fires `listAgents()` for any agent whose name is unknown, so the spawned agent's tab will resolve its real name shortly after. `registerAgents` (`:1187-1198`) preserves existing state via `agents[info.id] ?? emptyAgentState()`, so the already-appended prompt entry is **not** clobbered when the name arrives.

No race. The event is always retained.

### C3. Event ordering: PromptDispatched lands before Started ✅
`spawn_agent_shared` sends `AgentCommand::Prompt` (`:343`) and returns. The emit (`:420`) happens synchronously after, via `app.emit` (direct to webview, bypassing the fan-in — `emit_agent_event`, `:1014-1020`). The agent's `Started` event is sent later, from inside the agent task's turn loop (`turn.rs:67-69`) via `fanin_tx.send(...).await`, which goes through the fan-in channel → forwarder → `app.emit`. The direct emit therefore reaches the webview before the fan-in-routed `Started`. Even if ordering were reversed, `reduceStarted` (`:674-685`) spreads `...agent` (preserving the transcript) and only resets streaming/usage fields — it does **not** clear `transcript`. So the prompt entry survives regardless of arrival order. Robust.

### C4. UI-button spawn path correctly avoids the emit ✅
The `spawn_agent` Tauri command (`:264-289`) calls `spawn_agent_shared(..., None, None)` directly and returns an `AgentInfo`. It does **not** go through `IpcSpawner`, so no `PromptDispatched` is emitted. Correct — a UI-button spawn has no task text.

### C5. The `spawn` fallback path also emits (acceptable) ✅
`IpcSpawner::spawn` (`:385-387`) delegates to `self.spawn_with_parent(name, task, None)`, so it also emits. This path is reached only when the `spawn_agent` tool runs without a known `parent_id` (`spawn_agent.rs:129`), which in the IPC wiring never happens (the tool always has a parent id for a real agent). Even if it did, emitting for a real task is the desired behavior. No issue.

### C6. `emit_prompt_dispatched` is best-effort and non-fatal ✅
`emit_agent_event` (`:1014-1020`) logs on `Err` and returns `()`. The call site (`:420`) ignores the (unit) return. A failed emit cannot abort the spawn or propagate an error to the tool result. Correct.

---

## Bugs

### B1. AppHandle clone is correct ✅
`app.handle()` returns `&tauri::AppHandle` (via the `Manager` trait, imported at `main.rs:25`). `AppHandle: Clone` (it's an `Arc`-like handle internally), so `.clone()` produces an owned `AppHandle` cheaply. The `IpcSpawner` stores it by value (`app: tauri::AppHandle`, `:363`), and `emit_agent_event` takes `&tauri::AppHandle` (`:1014`), so `&self.app` coerces correctly. No issue.

### B2. No other `IpcSpawner::new` construction sites missed ✅
`grep` confirms `main.rs:88` is the sole construction site. No other caller needs updating.

---

## Security

### S1. No new attack surface ✅
The `task` text is already agent/user-supplied and is already displayed elsewhere (the parent's `spawn_agent` ToolCard shows the args, and the spawned agent receives it as a prompt). The `PromptDispatched` event merely surfaces the same text as a `kind: "user"` transcript entry in the spawned agent's window — no new data path, no injection vector (React escapes transcript text on render). No new surface.

---

## Constitution compliance

### K1. Public functions have doc comments ✅
- `IpcSpawner::new` (`:367-368`) — has a `///` doc comment. ✅
- `IpcSpawner` struct (`:354-358`) — has a `///` doc comment (updated to mention the new `app` field). ✅
- `emit_prompt_dispatched` (`:1022-1025`) — has a `///` doc comment. ✅
- `emit_agent_event` (`:1004-1013`) — has a `///` doc comment. ✅
- The new `displayName` helper in `Message.tsx` (`:263-265`) has a JSDoc comment. ✅

### K2. No commit to main ✅
No git operations performed by this change set; the diff is uncommitted working-tree changes on a feature branch. No violation.

### K3. Windows/PowerShell rules ✅
N/A — no shell commands introduced; the change is Rust + TS only.

### K4. Line-ending style ✅
`file_edit`/`file_write` normalize to the file's detected style. The diff shows no mixed endings introduced. The git warning about LF→CRLF on the `.coding/plans/*.md` file is a pre-existing bookkeeping-file quirk, not introduced by this change.

### K5. Workflow bookkeeping files ✅
`.coding/plans/9f580f7f-*.md` (step 7 checked off), `.coding/plans/stack.json` (stack pointer swapped to the new plan id), and the new untracked `.coding/plans/1c2857d9-*.md` are all well-formed plan/stack bookkeeping. No corruption.

---

## Summary

**No findings.** The change is correct, race-free, best-effort, and constitution-compliant. The `PromptDispatched` emit fires for the right agent id, is never dropped (the frontend creates the agent entry on demand via `getOrCreate`), lands before `Started` (and survives even if it didn't, since `reduceStarted` preserves the transcript), and the UI-button spawn path correctly avoids it. The `displayName` helper and `argLabel` parenthetical compose correctly to produce "Spawn Agent (reviewer)". `cargo test` (459) and `tsc --noEmit` are reported clean.
