# Plan: Fix: run-all dispatches next backlog item into the main agent during steering

## Goal
Stop run-all from dispatching the next backlog item into the main agent when the user steers mid-run. A steer during run-all should halt the run (mirroring the approval halt), so the steer runs cleanly without interleaving with the next backlog item.

## Kind
bug_fixing

## Context
Root cause confirmed: a mid-turn steer folds into StopReason::Steer → agent soft-stops → emits Finished. The forwarder's TurnResolveLatch::on_finished (src-tauri/src/ipc/events.rs) returns ResolveAction::Success for ANY clean Finished — it ignores FinishReason and cannot distinguish a steer soft-stop from a real turn-end. So on_main_turn_resolved (run_all.rs:663) fires → run_all_dispatch_next (run_all.rs:475) → dispatches item N+1 into the main agent. The `stopped` flag (run_all.rs:807) is only set by halt_run_all_for_approval — steers never set it.

Detection problem: the forwarder sees SuggestionInjected only AFTER the soft-stop Finished (sequence: Finished → SuggestionInjected → Started → Finished), so it can't suppress resolution at the soft-stop. The only place that knows "a steer was just sent" is the IPC command send_suggestion (agent.rs:225).

Fix: mirror the approval halt. Generalize halt_run_all_for_approval → halt_run_all(app, reason). In send_suggestion, when targeting the main agent during an active run-all, call halt_run_all BEFORE sending the steer. end_run clears run_all so the soft-stop Finished finds run_all_active=false → no resolution/rollback/next-dispatch. Safe because run-all start sets auto_feed=false (backlog_cmds.rs:356), so no auto-feed dispatch after end_run.

Key files: src-tauri/src/ipc/run_all.rs (halt_run_all_for_approval:898, on_main_turn_resolved:663), src-tauri/src/ipc/agent.rs (send_suggestion:225), src-tauri/src/ipc/events.rs (approval halt call:773). Existing test pattern to mirror: backlog_cmds.rs:396 (source-contract test for AppHandle commands).

## Steps
- [x] 1. **Reproduce with failing regression test** — Write a regression test that reproduces the defect (constitution: every defect gets a regression test that fails without the fix and passes with it). Run it and confirm it FAILS.
- [x] 2. **Document root cause** — Investigate and document the root cause. memory_write a BUG: record (symptom → root cause → fix + regression test name, ≤600 chars).
- [x] 3. **Minimal fix** — Apply the minimal fix that makes the regression test pass. Do not refactor unrelated code.
- [x] 4. **Verify** — Run the regression test + the full test suite (cargo test unpiped, warning-free). Record the regression test name via update_plan (regression_test field) — finish is blocked without it.

## Bug
During run-all, steering the main agent causes the backlog to dispatch the NEXT item's prompt into the main agent, interleaving the steer with item N+1. The user's steer and the next backlog item both run on the main agent simultaneously.
