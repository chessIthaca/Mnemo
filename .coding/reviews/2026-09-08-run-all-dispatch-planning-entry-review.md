## Verdict: FINDINGS (0 high, 1 low)

Review of plan 9057fa1f "Deterministic Complete→Planning on run-all backlog dispatch" (wt/agenticcoder, all uncommitted changes via `git diff HEAD`). All four key correctness points verified by code reading — gate safety, lock ordering, best-effort dispatch, non-persistence — and all constitution checks pass. One low finding: a variable shadowing hazard at run_all.rs:614.


# Review — plan 9057fa1f "Deterministic Complete→Planning on run-all backlog dispatch"

Scope reviewed via full `git diff HEAD` (6 files, +261/−10): src/workflow/mod.rs (+103: `enter_planning_for_task` + 2 tests), src-tauri/src/ipc/run_all.rs (+140: `enter_planning_if_complete`, call site, RUN_ALL_STEER rule 5, 2 tests), src-tauri/src/ipc/events.rs (+20: latch-wipe pinning test), README.md bullet, plus .coding side-car churn (backlog note on item 037fee62 — accurate; git-restore decision doc refresh — accurate for landed commits e039f56/07fcb9e; untracked plan/spec files — normal side-car).

## Key correctness points — all verified

**1. Gate safety — PASS.** The dispatch-time transition cannot become loop_evidence:
- `emit_agent_event` (events.rs:1727-1736) is `app.emit(AGENT_EVENT_CHANNEL, …)` — UI-only. The forwarder notes `workflow_changed` exclusively for events arriving through the runtime channel (events.rs:552-585), so this emit is invisible to the latch by construction.
- Belt-and-braces: `turn_resolve.on_started(agent_id)` (events.rs:635; impl 131-136 wipes `workflow_changed`) fires when the dispatched turn's Started arrives — the send is at run_all.rs:630, downstream of the emit. Pinned by the new test `dispatch_time_planning_entry_is_not_loop_evidence`.
- Outcome check: `plan_loop_allows_done` (run_all.rs:149-151) requires `Complete && changed_this_turn`. Post-flip the resting state is `Planning`, so a never-planning turn fails on both axes (before the change: `Complete` + no evidence — also rejected). No regression; `Done` still requires a real finish cycle.
- Bonus preservation: the forwarder's `prev_workflow_state` map is untouched by the UI-only emit, so on the agent's `create_plan` the map still reads `Complete` → the Complete→Executing inactive-subagent cleanup (events.rs:586-588) still fires. Behavior preserved.

**2. Locking/deadlock — PASS.** Call-site order is manager guard (run_all.rs:552) → `agent_loops` (604) → workflow mutex (608), matching the documented invariant (state.rs:38-43, quoted at startup.rs:51-54: manager first, never agent_loops→manager). All 18 other `agent_loops.lock().await` sites checked: each either drops the manager guard first (agent.rs:340-350; events.rs:729-732; events.rs:837-898 where the mgr guard ends at 853 before the 862/879 scopes, re-acquired at 896 only after they end; console.rs:720-725; run_all.rs:179 statement-scoped guard) or never touches the manager while holding agent_loops (config_io.rs:192/240, spawn.rs:327, agent.rs:545/605/657/720, startup.rs:55-66, console.rs:765/2350). No inversion exists. The `planning_event` block scope (603-613) drops agent_loops + workflow before the emit (614-624) and send (630); the emit is best-effort and cannot block dispatch. The agent is idle at this point (busy re-check passed under the same lock hold), so the workflow mutex is uncontended.

**3. Best-effort dispatch — PASS.** `enter_planning_if_complete` returns `None` for non-Complete and for `Err` (eprintln + None); the send at 630 runs unconditionally. Missing loop / unexpected state never fails the dispatch.

**4. Not persisted, stack untouched — PASS.** `enter_planning_for_task` (src/workflow/mod.rs) only assigns `self.state`; no file write. Tests confirm the finished plan stays visible (`wf.plan().is_some()`) and `create_plan` from Planning clears the stack for a fresh root (depth 1). Crash-reload semantics documented and sound (load_latest restores Complete; identical to pre-change crash behavior).

## Constitution checks — PASS

- **Docs sync:** README Backlog/run-all bullet updated and accurate; public `enter_planning_for_task` fully documented (rationale + `# Errors`); private helper documented. PLAN.md / config examples unaffected.
- **Multi-platform:** no `cfg(windows)`, no Windows-only APIs; `eprintln!` is portable.
- **Style/lints:** finish()-style guard matches existing patterns; no `#[allow]`; reported builds green under `#![deny(warnings)]` (root 1564 passed, src-tauri 176 passed as separate runs; frontend untouched, tsc + 636 tests clean).
- **Tests:** all four non-Complete states rejected (Planning/Executing/Reviewing/Skill), Complete→Planning + fresh-root flow, steer-text content (bug_fixing + `bug` param), and the latch-wipe pin — good coverage of all four deliverables.

## Findings

### Low 1 — `state` shadows the `IpcState` parameter in run_all.rs:614

`if let Some((state, top_plan_id)) = planning_event` shadows the function parameter `state: &IpcState` with a `WorkflowState` inside the emit block. Correct today (nothing in the block uses IpcState, and a future misuse would fail at compile time, not silently), but in a ~930-line function where `state` means `&IpcState` everywhere else, a rename to `new_state` — matching the forwarder's own naming (events.rs:575) — removes the hazard. Trivial rename, no behavior change.

## Recommendation

Fix Low 1 (one-line rename, no test churn expected), then commit. The core mechanism — deterministic task-entry, gate invisibility, lock discipline, best-effort transition, non-persistence — is correct and well-documented.
