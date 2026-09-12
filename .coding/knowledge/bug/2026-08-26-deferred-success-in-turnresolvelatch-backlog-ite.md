+++
title = "deferred-success in TurnResolveLatch — backlog items marked failed when work is done"
created = "2026-08-26"
+++

BUG: backlog items marked failed ("plan loop did not close (workflow: Skill)") even though the work is done and merged into main.

Root cause: TurnResolveLatch (src-tauri/src/ipc/events.rs) defers success resolution when the main agent finishes (Finished) while descendants (reviewers) are running — `on_finished` returns None when `descendants_running=true`. But unlike `on_final_error` (which records `failed_note` before deferring), `on_finished` records NO state for the deferred success. `flush_deferred_main_failure` only checks `failed_note` — it never flushes deferred successes. So the deferred success is lost when the next turn starts (`on_started` clears everything), and the resolution slides to a later turn (e.g. a merge skill → Skill state) where the workflow state is wrong → item marked Failed.

Fix: add `pending_finished: HashSet<AgentId>` to track deferred successes. `on_finished` sets it when deferring. `flush_deferred_main_failure` extended to also flush successes (check `pending_finished` after `failed_note`). `try_flush_deferred_main_failure` (renamed to `try_flush_deferred_main_resolution`) flushes successes only when the workflow is Complete (not Reviewing — the main agent may still need to fix findings and call finish). `main_agent_workflow_state` made pub(crate) for the workflow check.

Regression test: `deferred_success_is_flushed_when_descendants_drain` in events.rs turn_resolve_latch_tests.
