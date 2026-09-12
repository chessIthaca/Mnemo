# Review: Plan-only default tools + inactive-subagent cleanup — Round 2 (M1/M2 fixes)

Date: 2026-04-04
Reviewer: read-only reviewer subagent (spawn_agent)
Scope: **all** uncommitted changes in the working tree (`git status` / `git diff HEAD`):
- `frontend/src/hooks/useAgentStore.ts` (startup defaults + `exited` handler)
- `src-tauri/src/ipc/events.rs` (per-agent `prev_workflow_state` tracking, Complete→Executing detection, `cleanup_inactive_subagents`, M1 + M2 fixes)
- `.coding/plans/*.md`, `.coding/plans/stack.json` (plan-tracking artifacts — reviewed for consistency only, not source)

This round fixes both findings from the prior review
(`.coding/reviews/2026-04-04-plan-defaults-review.md`):
- **M1** — `prev_workflow_state.remove(&agent_id)` added to the `Exited` arm.
- **M2** — `cleanup_inactive_subagents` now takes the manager lock ONCE and holds it across the snapshot AND all the `Cancel` sends.

Verification already done by main agent: `cargo test` (392 + 9 + 5, 0 failures), `tsc --noEmit` clean. This review re-checked the two fixes and re-verified the whole still holds together.

---

## M1 fix — verified correct

`src-tauri/src/ipc/events.rs:171` — `prev_workflow_state.remove(&agent_id);` added inside the `Exited` arm, immediately after the symmetric `notified_children.remove(&agent_id)` at line 170.

- **Placement is correct and placement order is irrelevant.** `prev_workflow_state` is a *local* `HashMap` declared in the forwarder closure (lines 58-59), not behind the manager lock. It is read/written only from this single forwarder task (lines 80, 171 — confirmed no other accessor via grep). So whether the `remove` sits before or after `drop(mgr)` (line 165) / `agent_loops.lock()` (line 166) makes no difference to correctness; grouping it with `notified_children.remove` is the clean choice.
- **The leak is now bounded.** Every agent that exits (subagent cleanup, cancellation, or app shutdown) has its `prev_workflow_state` entry dropped, symmetric with `notified_children` and the manager/loops removal. No residual unbounded growth.
- **No behavioural change.** Removing an entry for an agent that is gone is a pure cleanup; it cannot affect any live agent's Complete→Executing detection (the id is monotonic and never reused).

## M2 fix — verified correct

`src-tauri/src/ipc/events.rs:296-313` — the function now acquires `manager_arc.lock().await` once (line 300), builds the `to_cancel` snapshot (301-306), and sends each `Cancel` while still holding the lock (307-312). The lock is dropped implicitly when `mgr` goes out of scope at the function's end.

### Is holding the async Mutex across the send loop actually safe?

Yes — confirmed.
- **`mgr.send` is synchronous and never awaits.** `AgentManager::send` (`src/runtime/mod.rs:55-65`) calls `handle.command_tx.try_send(cmd)`, which is the non-blocking variant of `mpsc::Sender::send`. It returns immediately (success, `Full`, or `Closed`); it does not poll/await. Holding a `tokio::sync::Mutex` across a non-async critical section is sound — no await point exists under the lock, so there is no risk of the future being dropped mid-hold or of deadlock-with-self.
- **No nested lock acquisition.** `cleanup_inactive_subagents` is called at line 82, at the top of the forwarder loop, *before* any `agent_loops.lock()` (that only happens in the `Exited` arm at line 166). The only lock taken inside the function is `manager_arc`. There is no second lock to deadlock against, and no lock-ordering hazard with the `send_prompt`/`cancel` IPC commands, which take the same single `manager` lock.
- **`for id in to_cancel` cannot await.** Each iteration is `let _ = mgr.send(id, …)` — a non-async call returning `Result`. The `to_cancel: Vec<AgentId>` is fully materialised before the loop, so iteration itself adds no await. Confirmed by reading the body: no `.await` appears between `lock().await` (300) and the implicit drop at 313.

### Does holding the lock longer starve other Tauri commands?

Negligibly, and the hold is bounded.
- The loop runs once per inactive subagent — in practice 0-3 iterations (reviewer/spawned subagents from the previous plan). Each iteration is a bounded-capacity channel `try_send`, a few microseconds. Total hold time is microseconds, well under a Tauri command's typical dispatch latency.
- A concurrent `send_prompt` (`src-tauri/src/ipc/commands.rs:117`) needs the same `state.manager` lock, so it blocks until all `Cancel`s are enqueued — then proceeds. This is the **intended** behaviour: it serialises `Cancel` ahead of any later `Prompt` in the per-agent FIFO command channel, so the idle subagent reads `Cancel` first (`src/runtime/agent.rs:105` `while let Some(cmd) = cmd_rx.recv().await`) and exits without running an extra turn. The TOCTOU window from M2 is closed.
- Other IPC commands that don't touch the manager lock (`approve`, `set_safety_mode`, read-only getters) are unaffected. The `cancel`/`interrupt`/`send_suggestion` commands also take the same lock but are equally short-lived; none hold it across an await, so no cascading starvation. Tauri runs each command as its own async task, so a briefly-blocked `send_prompt` simply schedules on the next tick.
- **No deadlock with the forwarder itself.** The forwarder task is the lock-holder here; it is not also a lock-waiter elsewhere during this window. The fan-in `recv()` (line 63) is *not* under the lock (per the module doc, lines 3-6). So agent tasks can still emit events into the fan-in channel while the forwarder holds the manager lock; they buffer in the bounded fan-in channel and are drained once the forwarder returns to `rx.recv().await`.

### FIFO-ordering claim — verified

- The per-agent command channel is `mpsc::channel(64)` (`src-tauri/src/ipc/commands.rs:296`), FIFO. `try_send` enqueues in order; `recv()` dequeues in order.
- Because the `Cancel` sends all complete under the lock, and any concurrent `Prompt` send is blocked on the same lock, the `Cancel`(s) are guaranteed to be enqueued strictly before any `Prompt` that arrived during the cleanup window. The agent task, idling at `recv().await`, therefore processes `Cancel` first and `break`s (`src/runtime/agent.rs:258-282`) without running the extra `Prompt` turn. The M2 extra-turn window is eliminated.

## Re-verification of the wider diff (unchanged from round 1 — still holds together)

- **Running subagents are never cancelled.** `is_running()` is mutated only by the forwarder task (`set_running` at `events.rs:136,143,163`; no other writer — re-confirmed via grep). Cleanup runs synchronously inside that task at line 82, so no `Started` event can flip a subagent to running mid-cleanup. The `!h.is_running()` filter (`events.rs:304`) plus `h.parent_id.is_some()` plus `h.id != trigger_agent` correctly excludes running subagents, the main agent, and the triggering agent. ✅
- **UI-spawned agents are never cancelled by cleanup.** The UI `spawn_agent` Tauri command passes `parent_id: None` (`src-tauri/src/ipc/commands.rs:252-253`), and the tool-spawned path passes `Some(parent_id)` (`commands.rs:301-303`). The cleanup filter `h.parent_id.is_some()` (`events.rs:304`) therefore only ever matches tool-spawned background agents — a user-created agent tab is never cleaned up by this path. ✅
- **Complete→Executing detection.** `WorkflowState` derives `Copy + PartialEq + Eq` (`src/workflow/mod.rs:20`), so `prev_workflow_state.get(&agent_id) == Some(&WorkflowState::Complete)` (line 79) and `new_state == WorkflowState::Executing` (line 81) type-check and compare correctly. First-event safety (`prev.get` is `None` → no spurious cleanup) and sub-plan-push-while-Executing (prev = `Executing` → no cleanup) are both preserved. ✅
- **Frontend `exited` handler drops the agent cleanly.** Deletes from all four per-agent maps (`agents`, `agentNames`, `agentParents`, `workflowStates`) and falls back `activeAgent` to `selectMainAgentId` only if the removed agent was active (`useAgentStore.ts:1102-1131`). `selectMainAgentId` (`useAgentStore.ts:438-448`) returns the smallest parentless id, or smallest known id, or `null`. The main agent's `Exited` is unreachable in normal operation (its command sender is held by the manager for the app lifetime; the `cancel` IPC command is not wired into the UI — confirmed round 1), so this is defensive code. ✅
- **Startup defaults.** `rightPanelVisible: true`, `rightPanelTab: "plan"`, `disabledTabs: ALL_RIGHT_PANEL_TABS.filter(t => t !== "plan")` (`useAgentStore.ts:562-571`). `ALL_RIGHT_PANEL_TABS` includes `"plan"` (`useAgentStore.ts:228-237`), so the Plan tab is enabled and visible; all others disabled. Not persisted to `localStorage`, so applies cleanly each launch. ✅

## Constitution compliance

- **Doc comments.** `cleanup_inactive_subagents` is module-private (not `pub`), so the "all public functions must have doc comments" rule doesn't strictly bind it — but its doc comment (`events.rs:274-295`) was **updated** to accurately describe the new single-lock design and the FIFO-ordering rationale. The inline comment at 307-309 is also accurate. All other new/changed public-ish items remain documented. ✅
- **`cargo test` before marking complete** — reported 392 + 9 + 5 pass, 0 failures. ✅

---

## Findings

**No findings.**

Both prior-round findings (M1, M2) are correctly and completely fixed, with no new correctness, bug, or security issues introduced. The hold-the-lock-across-the-loop design is sound (synchronous `try_send`, no await under the lock, no nested locking, bounded microseconds-long hold), and it correctly closes the TOCTOU window by serialising `Cancel` ahead of any concurrent `Prompt`. The whole feature (frontend defaults + Exited drop + backend Complete→Executing detection + inactive-subagent cleanup) holds together.

---

## Verdict

**Approve — clean.** Both M1 and M2 are resolved; no Major/Minor/Nit findings. The diff is ready to commit.