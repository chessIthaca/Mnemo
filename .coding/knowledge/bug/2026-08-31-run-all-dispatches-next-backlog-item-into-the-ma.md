+++
title = "run-all dispatches next backlog item into the main agent during steering"
created = "2026-08-31"
+++

BUG: During run-all, steering the main agent causes the backlog to dispatch the NEXT item into the main agent, interleaving the steer with item N+1's prompt.

**Symptom:** "during steering, the backlog is inserting things into the main agent during run-all."

**Root cause:** A mid-turn steer folds into `StopReason::Steer` → the agent soft-stops → emits `Finished`. The forwarder's `TurnResolveLatch::on_finished` (src-tauri/src/ipc/events.rs) returns `ResolveAction::Success` for ANY clean `Finished` — it ignores `FinishReason` and CANNOT distinguish a steer soft-stop from a real turn-end. So `on_main_turn_resolved` (run_all.rs:663) fires → `run_all_dispatch_next` (run_all.rs:475) → dispatches item N+1's Prompt into the main agent. Meanwhile the steer's follow-up turn starts → interleave. The `stopped` flag (run_all.rs:807) is only set by `halt_run_all_for_approval` — steers never set it.

**Detection problem:** The forwarder sees `SuggestionInjected` only AFTER the soft-stop `Finished` (sequence: Finished → SuggestionInjected → Started → Finished), so it can't suppress resolution at the soft-stop. The only place that knows "a steer was just sent" is the IPC command `send_suggestion` (agent.rs:225).

**Fix:** Mirror `halt_run_all_for_approval` — generalize to `halt_run_all(app, reason)`; in `send_suggestion`, when targeting the main agent during an active run-all, call `halt_run_all` BEFORE sending the steer. `end_run` clears `run_all` so the soft-stop `Finished` finds `run_all_active=false` → no resolution/rollback/next-dispatch. Safe because run-all start sets `auto_feed=false` (backlog_cmds.rs:356).

**Regression test:** `send_suggestion_halts_run_all_before_sending` (src-tauri/src/ipc/agent.rs tests) — source-contract test asserting send_suggestion calls halt_run_all before send_cmd. Fails without the fix (no halt_run_all call), passes with it.

Key files: src-tauri/src/ipc/run_all.rs (halt_run_all_for_approval:898, on_main_turn_resolved:663, run_all_dispatch_next:475), src-tauri/src/ipc/agent.rs (send_suggestion:225), src-tauri/src/ipc/events.rs (Finished handler ~673, approval halt call ~773), src/agent/loop_impl.rs (StopReason::Steer:298).
