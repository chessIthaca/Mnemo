# Review: Merge-to-main stale-workflow-state fix

**Date:** 2026-08-11
**Scope:** All uncommitted changes (`git diff HEAD`)
**Plan:** `.coding/plans/45ad64d5-a5df-436c-b1fd-5b1b8368bf54.md` — bump `planVersion` on the `finished` event so StatusBar re-fetches the true backend workflow state when the agent goes idle.

## Files changed
- `frontend/src/hooks/agentEventReducer.ts` — `reduceFinished` now returns `effects: { planVersionBump: true }` (+ doc comment).
- `frontend/src/hooks/useAgentStore.test.ts` — `finished` test extended to assert `planVersion` bumps.
- `.coding/plans/39f3c881-….md`, `.coding/plans/stack.json`, `.coding/plans/45ad64d5-….md` — plan bookkeeping (checkbox flip + stack pointer + new plan file). Expected, not code.

---

## Correctness

**The fix is correct and the trigger chain is sound.** Verified end-to-end:

1. `reduceFinished` (`agentEventReducer.ts:439-452`) returns `effects: { planVersionBump: true }`.
2. `applyAgentEvent` (`agentEventReducer.ts:581`) merges it: `...(effects?.planVersionBump ? { planVersion: s.planVersion + 1 } : {})` — so `planVersion` increments on every `finished` event. ✓
3. StatusBar's `useEffect` (`StatusBar.tsx:154-160`) watches `planVersion` and calls `refreshPlan()` when it changes (guarded by `lastFetchedVersion` to avoid duplicate fetches for the same version). ✓
4. `refreshPlan()` (`StatusBar.tsx:141-150`) guards `if (activeAgent === null) return;`, then calls `getWorkflowState(activeAgent)` and `setWorkflowState(activeAgent, wf.state)`, which writes `workflowStates[activeAgent]` (`useAgentStore.ts:431-432`). ✓
5. The button visibility (`StatusBar.tsx:606`) reads `workflowState`, derived from `workflowStates[activeAgent]` (`StatusBar.tsx:33-35`). So a corrected `workflowStates` value re-evaluates the button. ✓

**Root-cause confirmed.** `WorkflowStateChanged` is emitted ONLY in `src/agent/turn.rs:780-809`, after the *agent* dispatches a workflow tool during `run_turn`. The harness/outer-agent dispatch path bypasses this, so no event fires and `planVersion` never bumped — leaving the UI stale. The `finished`-bump covers this path (and any future state-mutation path) because it re-fetches the authoritative backend state on every turn end. ✓

**Casing is correct (prior H2 bug already fixed).** `WorkflowStateInfo.state` is typed `WorkflowState` (`src-tauri/src/ipc/agent.rs:92`), serialized via `#[serde(rename_all = "lowercase")]` (`src/workflow/mod.rs:21`) → `"planning"`/`"executing"`/`"complete"`/`"skill"`. `get_workflow_state` uses `workflow.state()` (`agent.rs:356`), not `.to_string()`. The TS `WorkflowState` union is lowercase (`types.ts`). So `setWorkflowState(activeAgent, wf.state)` writes the correct lowercase value the button gate compares against. No `as never` cast at the call site (`StatusBar.tsx:146`). ✓

**`finished` fires on every turn-end path.** Confirmed at `turn.rs:465` (Cancel), `turn.rs:521` (interrupted), `turn.rs:649` (normal end-of-turn, no tool calls). So the bump covers all normal idle transitions. ✓

**No findings.**

---

## Bugs

**No infinite loop / feedback cycle.** `planVersion` bumps once per `finished` event. `refreshPlan()` calls `getWorkflowState` (Tauri command) + `setWorkflowState` (store setter). `setWorkflowState` (`useAgentStore.ts:431-432`) updates `workflowStates` but does NOT bump `planVersion`, so the `useEffect` at `StatusBar.tsx:154-160` does not re-fire. No feedback loop. ✓

**No redundant-fetch storm.** Each `finished` triggers exactly one `refreshPlan()` (the `lastFetchedVersion` ref dedupes same-version re-entries). `getWorkflowState` is a cheap lock+read (`agent.rs:344-361`). One extra fetch per turn end is negligible. ✓

**`activeAgent === null` race is guarded.** `refreshPlan()` returns early if `activeAgent` is null (`StatusBar.tsx:142`). The `planVersion` bump still happens (harmless), but no fetch/crash occurs. When an agent is later selected, the `activeAgent` `useEffect` (`StatusBar.tsx:164-171`) forces a refresh. ✓

**Subagent `finished` is harmless.** `planVersion` is global, so a subagent's `finished` bumps it and `refreshPlan()` re-fetches the *active* agent's state. If the active agent differs from the finished subagent, this is a redundant (but correct) fetch of the active agent's unchanged state. The subagent's own stale state is refreshed when it becomes active (agent-switch effect). No correctness issue. ✓

**[Minor — out of scope] Terminal `Error` path does not bump `planVersion`.** A final, non-retrying `Error` also ends the turn (agent idle) but emits `Error`, not `Finished` (terminal exclusivity, `turn.rs:815-820`). `reduceError` (`agentEventReducer.ts:469-489`) sets `running = false` but does NOT return `planVersionBump`. So if the agent's turn ends via terminal error *after* a harness `complete_step` mutated the workflow to Complete, the UI would stay stale until the next `finished`/`workflow_state_changed`/agent-switch/restart. This is a rarer path (error condition + concurrent harness mutation) and the plan explicitly scopes the fix to the `finished` event. Not a blocker, but worth a follow-up if terminal-error turns are expected to reflect workflow transitions. The IPC forwarder does flip `running = false` on terminal Error (`events.rs:241-272`), mirroring Finished, so the asymmetry is purely in the `planVersion` bump.

**No other findings.**

---

## Security

**No findings.** The change adds a boolean effect flag consumed by an existing, already-audited merge path. No new IPC surface, no user input flows into the bump, no mutation of protected state. `getWorkflowState` reads the workflow behind its existing `tokio::Mutex` (`agent.rs:349`) — no new lock-ordering or contention introduced. The `refreshPlan` guard prevents a null-`activeAgent` fetch.

---

## Constitution compliance

**Doc comments.** `reduceFinished` has a thorough multi-line doc comment explaining the rationale, the root cause, and the consequence (`agentEventReducer.ts:442-451`). The test has an explanatory comment (`useAgentStore.test.ts:311-316`). ✓

**Line-ending preservation.** The diff is clean; the file tools normalize to the file's detected style. No mixed endings introduced. (Git warns about LF→CRLF on the `.coding/plans/*.md` file, but that's a pre-existing bookkeeping file, not source.) ✓

**No drive-by refactors.** The change is minimal and targeted: one reducer return value + one test assertion. No unrelated code touched. ✓

**Tests.** The `finished` vitest asserts `planVersion === before + 1` (`useAgentStore.test.ts:317`), directly covering the new behavior. The existing assertions (`running === false`, streaming flush, transcript) are preserved. ✓

**No findings.**

---

## Overall verdict

**APPROVE.** The fix is correct, minimal, well-documented, and well-tested. The trigger chain (`finished` → `planVersion` bump → `refreshPlan()` → `getWorkflowState` → `setWorkflowState` → button re-evaluates) is sound, the casing is correct, and there is no infinite loop or race. The only gap — terminal `Error` turns not bumping `planVersion` — is a rarer, out-of-scope path explicitly excluded by the plan; it warrants a follow-up only if error-ending turns are expected to reflect workflow transitions. The `.coding/plans/*` changes are expected bookkeeping.
