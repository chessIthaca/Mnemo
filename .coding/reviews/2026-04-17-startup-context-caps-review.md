# Review — "Show stats row at startup" (registration-time ContextUsage + context_caps IPC)

Reviewed **all** uncommitted changes (`git diff HEAD` + `git status --short`): 10 files —
src-tauri/src/ipc/spawn.rs, src-tauri/src/ipc/agent.rs, src-tauri/src/main.rs,
frontend/src/lib/tauri.ts, frontend/src/hooks/useAgentStore.ts, frontend/src/hooks/useAgentStore.test.ts,
frontend/src/App.tsx, frontend/src/components/chat/InflightBar.tsx, frontend/src/components/layout/StatusBar.tsx,
plus the plan bookkeeping file (.coding/plans/b94af7a9-….md — checkboxes only, no code).

---

## Findings

### 1. CORRECTNESS (medium) — latent deadlock: `fanin_tx.send(...).await` while holding the manager mutex
**src-tauri/src/ipc/spawn.rs:171-180** (guard acquired at :151 `let mut mgr = manager.lock().await;`, released at :181)

The registration-time ContextUsage send is awaited **inside** the manager-lock block. The fan-in
channel is bounded (cap 256, main.rs:113) and `mpsc::Sender::send()` blocks when full. The only
drainer is the event forwarder (src-tauri/src/ipc/events.rs:169-398), and several of its per-event
arms take the same manager lock: `Started` (:243), final `Error` (:251), `Finished` (:284), `Exited`
(:329), the WorkflowStateChanged cleanup (:194 → `cleanup_inactive_subagents`), and the
ApprovalRequest main-check (:369).

Deadlock interleaving: fan-in at capacity (256 backlogged events — reachable during heavy
multi-agent streaming bursts) → `spawn_agent_shared` holds the manager lock and parks on
`send().await` waiting for capacity → the forwarder's head-of-line event is a manager-lock arm → it
parks on `manager_arc.lock().await` → nothing drains → permanent wedge of the forwarder, the
manager, and every Tauri command that needs either. events.rs:228-231 itself documents the
codebase invariant this violates ("We lock the manager only briefly here (not while waiting …), so
Tauri commands can still acquire it").

Probability is low (needs a full channel at that instant) but the failure mode is a hard app hang.
**Fix is trivial:** the `mgr` guard is unused after `mgr.register(handle)` (:160), and everything
the send needs (`fanin_tx` clone from :153, `agent_id`, `agent_loop`) outlives the block — move the
send to just **after** the closing `}` at :181 (before the `agent_loops.lock().await.insert` at
:185). The "buffers until the forwarder starts" rationale in the comment is unaffected (the startup
send lands in an empty channel either way).

### 2. MINOR — plan-compliance: the step-1 backend test (or its skip-note) is absent
**src-tauri/src/ipc/spawn.rs:437-671** (test module)

Plan step 1 required a spawn.rs test asserting the registration emission enqueues a ContextUsage
with used=0 and the provider's max — **or** an explicit note in the test module if the harness
can't support it. Neither exists: the diff adds no backend test and no skip-note. Skipping is in
fact defensible (the existing harness builds `AgentLoop` directly, spawn.rs:448-504, with no
`AgentLoopFactory` builder, and `spawn_agent_shared` needs one plus `tauri::async_runtime`) — but
the documented rationale is missing. Add a short comment in the test module stating why the
registration-emission test is skipped (no factory harness; covered indirectly by the frontend
seed test + the /new clear_context emission twin at src/runtime/agent.rs:561-573).

### 3. NIT — redundant term in `showBar`
**frontend/src/components/chat/InflightBar.tsx:119-120**

`showCounts` is by definition `showTokenUsage` (:40), so
`showBar = running || hasActivity || showCounts || hasContext || showTokenUsage` has a dead
trailing term (`showCounts || showTokenUsage` ≡ `showCounts`). Drop one term and tighten the
comment above it, which still reads as if `hasTokens` existed ("when tokens or context have been
seen, OR when showTokenUsage is on").

---

## Verified clean (checked, no findings)

- **spawn.rs emission content** — `fanin_tx` correctly cloned at :155 before being moved into
  `task.run` (the original is used for the send); payload `(agent_id, AgentEvent::ContextUsage {
  used: 0, max, breakdown: ContextBreakdown::default() })` matches the enum
  (src/runtime/channels.rs:142-146) and the `/new` twin (src/runtime/agent.rs:561-573);
  `myharness::runtime::AgentEvent`/`ContextBreakdown` are the correct re-export paths
  (src/runtime/mod.rs:12-15); failure swallowed with `let _`; `agent_loop.context_manager()`
  (loop_impl.rs:719) / `max_tokens()` (context.rs:75) exist and return the shared ContextManager
  snapshotted at factory build; `into_serializable` maps ContextUsage (channels.rs:423-424) so the
  forwarder forwards it; the send precedes the initial-prompt send, so the bar seeds before the
  first turn's events.
- **context_caps command (agent.rs:392-406)** — returns `Vec<(AgentId, u32)>` with **max only**
  (never `used`); takes only the `agent_loops` lock, never the manager — respects the documented
  lock-ordering invariant (state.rs:39-44); doc comment present and accurate; `AgentId` already
  imported (agent.rs:14); registered in main.rs invoke_handler (:392) next to `list_agents`;
  `(u64, u32)` tuple serializes as a 2-element array, matching the frontend `[number, number][]`.
- **Store (useAgentStore.ts:291-294, 548-565)** — `seedContextCaps` replaces only
  `contextUsage.max`, preserves `used` and (via the `...agent` spread) `contextBreakdown`; skips
  unknown ids (no entry created); interface signature matches the implementation; `registerAgents`
  never resets `contextUsage` for existing agents, so the parallel listAgents/seed in StatusBar
  can't clobber it.
- **Store test (useAgentStore.test.ts:712-737)** — covers all three required cases (fresh seed,
  preserve-used across a re-seed after a live `context_usage` event, unknown-id skip); the event
  shape matches `types.ts:118-123` exactly (`breakdown: {system,user,assistant,tool}`).
- **App.tsx:177-187** — seed runs in its own try/catch immediately after the registerAgents block;
  failure logs via `console.error` like its neighbors (non-fatal); empty-agents case is a no-op.
- **StatusBar.tsx:345-350, 382-386** — both model-swap completion handlers (`selectModel`,
  `selectReasoningEffort`) re-seed caps via `getContextCaps → seedContextCaps`, mirroring the
  existing `listAgents` pattern with its own `.catch`; justified because `set_model` rebuilds each
  loop's ContextManager from the new provider's max (agent.rs:506-509). Import added (:10).
- **InflightBar.tsx** — `hasTokens` fully removed (no references anywhere in frontend/src);
  counters render at 0 whenever `showTokenUsage` is on (:176); 🧠 span still gated on
  `tokenUsage.reasoning > 0` (:184); session tok/s segment still gated on
  `sessionTiming.timed_requests > 0` (:191); ctx-bar `ml-auto` conditional uses `!showCounts`
  (:218); `hasContext` gating unchanged.
- **Constitution** — no `#[allow(...)]` added, no dead code (the `let max` binding is used, no
  unused imports: `getContextCaps` used in both App.tsx and StatusBar.tsx); doc comments on all
  three new public items (`context_caps`, `getContextCaps`, `seedContextCaps`); no secrets or
  sensitive paths in error strings; no Windows-path violations. Main agent reports root cargo test
  (833) + src-tauri cargo test (76) + vitest (136) + `npm run build` all green, which under
  `#![deny(warnings)]` implies warning-free — consistent with what I read.
- **Plan file** — only step-checkbox updates; no code.

## Summary

The design is sound and the max-only/no-clobber semantics are correctly implemented on both sides
of the IPC boundary. Three findings to fix before commit: (1) move the registration send outside
the manager-lock guard (deadlock hazard, one-line move), (2) add the missing skip-note for the
absent backend emission test, (3) drop the redundant `showTokenUsage` term in `showBar`.
