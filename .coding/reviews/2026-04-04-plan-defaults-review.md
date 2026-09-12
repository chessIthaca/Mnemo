# Review: Plan-only default tools + inactive-subagent cleanup on fresh plan from Complete

Date: 2026-04-04
Reviewer: read-only reviewer subagent (spawn_agent)
Scope: **all** uncommitted changes in the working tree (`git status` / `git diff HEAD`):

- `frontend/src/hooks/useAgentStore.ts` (defaults + Exited handler)
- `src-tauri/src/ipc/events.rs` (per-agent prev-state tracking + `cleanup_inactive_subagents`)
- `.coding/plans/...` (plan-tracking artifacts, not source — reviewed for consistency only)

Verification already done by main agent: `cargo test` (392 + 9 + 5, 0 failures), `cargo build` clean, `tsc --noEmit` clean. This review re-checked correctness, races, and the specific concerns called out in the task brief.

---

## Findings

### Minor

**M1. `prev_workflow_state` map leaks an entry per exited agent (unbounded slow growth).**
`src-tauri/src/ipc/events.rs:58-59` (decl), `:76-84` (insert), `:158-171` (Exited arm).
The forwarder's `Exited` arm (lines 158���171) cleans the agent out of the manager, `agent_loops`, `pending_approvals`, and `notified_children` — but does **not** remove the agent's entry from `prev_workflow_state`. Agent ids are monotonic and never reused, so every subagent that ever lived leaves a permanent `WorkflowState` (a 1-byte Copy enum + HashMap node overhead) behind. A reviewer subagent is spawned roughly once per plan, so a long-running session accumulates one entry per historical subagent.
Severity is low (entries are tiny), but it's a genuine unbounded leak and the fix is trivial and symmetric with the existing `notified_children.remove(&agent_id)` at line 170. Recommend adding `prev_workflow_state.remove(&agent_id);` inside the `Exited` arm.

**M2. Narrow TOCTOU: an inactive subagent that receives a Prompt during the cleanup window runs one extra turn before exiting.**
`src-tauri/src/ipc/events.rs:294-309` (`cleanup_inactive_subagents`).
The snapshot of inactive subagent ids is taken under the manager lock (lines 294–305), the lock is released, then each `Cancel` is sent after re-locking (lines 306–309). In the window between snapshot and send, a **concurrent Tauri command** (e.g. a user clicking a stale subagent tab and sending a prompt, or a parent `Suggestion`) can `mgr.send(subagent_id, AgentCommand::Prompt{..})`. The `is_running` flag is only ever flipped by the forwarder task itself (on `Started`/`Finished`), and the forwarder is blocked inside this cleanup (not back at `rx.recv().await`), so the atomic is still `false` at snapshot time and stays `false` through the send — meaning the Prompt is genuinely queued *before* the `Cancel` in the same FIFO channel. The agent task (idle at `cmd_rx.recv().await`, `src/runtime/agent.rs:105`) then processes the `Prompt` first → runs a **full unintended turn** → then reads the `Cancel` and exits (consolidates memory, emits `Finished`→`Exited`).
There is **no** race that cancels a *running* subagent: `is_running` is mutated only by the forwarder's own event processing, which can't interleave with this synchronous cleanup. So the brief's worst-case concern (a running subagent getting Cancelled) does **not** occur. The actual residual is an idle-but-just-prompted subagent doing one extra turn before a clean exit — no hang, no corruption, no crash, and it requires precise concurrent user action on a stale subagent tab during a sub-millisecond window. Low severity. (Acceptable to leave as-is given the rarity; noted for completeness. If hardening is desired, re-check `is_running()` under the lock immediately before each `send` and skip if it flipped — though note that still wouldn't catch a queued-but-not-yet-started Prompt.)

### Nit

**N1. `exited` handler skips `flushStreamingText` — verified safe.**
`frontend/src/hooks/useAgentStore.ts:1102-1131`. The old `exited` arm called `flushStreamingText(next)`; the new one does not. This is fine: `Exited` is always preceded by a `Finished` event (clean turn end flushes, line 1098–1101; or Cancel-during-streaming emits `Finished{Stop}` at `src/agent/mod.rs:646-651` before the task breaks and emits `Exited`), and the frontend processes that preceding `Finished` first, flushing any partial text. So no streaming text is lost when the agent is dropped. No action needed — recording the reasoning so it isn't re-litigated.

---

## Concerns from the brief — verified clean

- **Complete→Executing detection is correct.** `Workflow::create_plan` (`src/workflow/mod.rs:135-161`) clears the stack and sets `state = Executing` whenever `state != Executing` (i.e. from `Complete` or `Planning`); the `WorkflowStateChanged` event is emitted unconditionally after every `create_plan`/`complete_step`/`abandon_plan` tool result (`src/agent/mod.rs:917-933`), carrying the new state. The forwarder's `was_complete = prev == Some(Complete)` + `new == Executing` therefore fires exactly on a fresh plan from Complete. ✅
- **First-event safety.** On the very first `WorkflowStateChanged` for any agent, `prev_workflow_state.get(&agent_id)` is `None`, so `was_complete` is `false` — no spurious cleanup at app start or for a brand-new subagent. ✅ Sub-plan pushes while already `Executing` (prev = `Executing`) correctly do **not** trigger cleanup. ✅
- **Running subagents are never cancelled.** `is_running()` is mutated solely by the forwarder task (`set_running` calls at `events.rs:136,143,163` — confirmed no other caller via grep). Cleanup runs synchronously inside that task, so no `Started` can flip a subagent to running mid-cleanup. The `!h.is_running()` filter plus `h.parent_id.is_some()` plus `h.id != trigger_agent` correctly excludes running subagents, the main agent, and the triggering agent. ✅
- **Main agent is never dropped from the frontend store.** The main agent's `run` loop only exits on `AgentCommand::Cancel` (`src/runtime/agent.rs:258-282`) or sender-drop. The user-facing `cancel` IPC command (`src-tauri/src/ipc/commands.rs:148-158`) can target any id in principle, but it is **not wired into the UI** (`frontend/src/lib/tauri.ts:43` exports `cancel`, but no component imports/invokes it — confirmed via grep of `App.tsx` imports and the whole `frontend/` tree). The main agent's command sender is held by the manager for the app lifetime. So the `Exited`→drop path is unreachable for the main agent in normal operation. The `selectMainAgentId` fallback in the Exited handler (line 1120) is correct defensive code regardless. ✅
- **Frontend cleanup is complete.** All four per-agent-keyed maps in `AppState` are exactly `agents`, `agentNames`, `agentParents`, `workflowStates` (`useAgentStore.ts:263-274`); the Exited handler deletes from all four and adjusts `activeAgent`. No other per-agent map leaks (streaming text / steers live inside `AgentState`, dropped with the agent). ✅
- **Tab removal works.** The top bar renders `Object.entries(agents)` (`MainPanel.tsx:21`), so deleting from `agents` removes the tab. The empty-state (`MainPanel.tsx:47`) only triggers when *all* agents are gone — unreachable while the main agent persists. ✅
- **No stale re-registration.** `registerAgents` (`useAgentStore.ts:598-609`) only *adds* entries from the info list (using `agents[id] ?? emptyAgentState()`, preserving existing state) and never deletes agents absent from the list. The dispatch-time name-resolution `listAgents()` (`useAgentEvents.ts:106-117`) runs its name check *before* `handleAgentEvent` drops the agent, so the `Exited` event itself doesn't trigger a re-fetch; and any in-flight `listAgents` resolved after `Exited` reads a manager that has already removed the agent, so it won't return it. ✅
- **Defaults are not persisted over.** `rightPanelVisible`/`rightPanelTab`/`disabledTabs` are not read from or written to `localStorage` (only fonts/colors/theme/sizes use `readLs`/`writeLs` — confirmed via grep), so the new startup defaults apply cleanly on every launch. `ALL_RIGHT_PANEL_TABS` includes `"plan"` (`useAgentStore.ts:228-235`), so `.filter(t => t !== "plan")` correctly disables all others while `rightPanelTab: "plan"` points at an enabled, visible tab. ✅
- **Constitution: doc comments.** `cleanup_inactive_subagents` (`events.rs:286`) is a module-private `async fn` (not `pub`), so the "all public functions must have doc comments" rule doesn't strictly bind it — but it carries a thorough doc comment anyway. All other new/changed public-ish items are documented. ✅
- **Import correctness.** `selectMainAgentId` is imported/defined in the same module (`useAgentStore.ts:438`) and used in the Exited handler at line 1120 — no missing import. `WorkflowState` import in `events.rs:26` is valid (`pub enum WorkflowState` in `myharness::workflow` via `pub mod workflow` in `src/lib.rs:17`). ✅

---

## Verdict

**Approve with two minor follow-ups.** The two features are implemented correctly and the specific correctness/race concerns from the brief were all verified clean. The only actionable items are:

1. **M1** (recommended fix): add `prev_workflow_state.remove(&agent_id);` to the `Exited` arm in `events.rs` to prevent the per-agent slow leak — symmetric with the existing `notified_children.remove` one line above.
2. **M2** (informational): a narrow window where an idle subagent prompted concurrently during cleanup runs one extra turn before a clean exit. No running subagent is ever cancelled. Acceptable as-is given rarity; noted for the record.

No correctness, security, or blocking bugs found. `cargo test` / `cargo build` / `tsc --noEmit` were reported clean by the main agent and the code paths reviewed are consistent with that.