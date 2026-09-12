# Review — Backlog plan-loop gate + reset-done (branch `feat/backlog-plan-gate`)

Reviewed ALL uncommitted changes (`git status --short` + `git diff HEAD`): PLAN.md,
frontend/src/components/views/BacklogView.tsx, frontend/src/components/views/BacklogView.test.ts (new),
frontend/vitest.config.ts, src-tauri/src/ipc/run_all.rs, src/backlog.rs, src/config/general.rs.
`.coding/**` changes (backlog.json, stack.json, new plan md) are app-internal bookkeeping — ignored per instructions.

## Findings

### 1. CORRECTNESS / SECURITY — `backlog_retry` was never wired to `requeue`; the stale-click guard does not exist end-to-end

`src-tauri/src/ipc/backlog_cmds.rs:141-152` — `backlog_retry` still calls the unguarded
`set_status(id, BacklogStatus::Pending, None)` directly. `set_status`
(src/backlog.rs:163-169) is a raw setter: any id, any status, note overwritten.
The new guarded `BacklogStore::requeue` (src/backlog.rs:179-192) is **production dead
code** — only its unit tests call it. This was explicitly in the plan ("harden
`backlog_retry` to call `requeue`") and did not happen.

Concrete consequences:

- **Checkpoint-sha destruction.** `halt_run_all_for_approval`
  (src-tauri/src/ipc/run_all.rs:500-504) marks the IN-FLIGHT item `Failed` (with the
  checkpoint sha preserved in its note) while the agent's turn is still running. The
  UI immediately shows the retry button on that card. A user click → item flipped to
  `Pending` and the note (the sha) wiped to `None`. When the turn later resolves,
  run-all state is already cleared (`end_run` in the halt path) → `run_all_active ==
  false` → the single-dispatch branch sees `single_in_flight == 0` (run-all dispatch
  never sets it — only `dispatch_item` at backlog_cmds.rs:279 does) → nothing
  re-marks the item. The item is now `Pending` with its rollback/resume sha
  destroyed, defeating the halt-design in run_all.rs:460-471 ("must still be able to
  see the original sha to decide commit_success vs rollback").
- **Double-queue / status ping-pong.** Any stale `backlog_retry` call can flip an
  `InFlight` item to `Pending`; the next `next_pending()` (auto-feed, a restarted
  run-all, or the ▶ button) can re-dispatch it while its previous turn's resolution
  has no item state left to update — the same prompt runs twice and resolution
  overwrites whatever the user did in between.
- **False invariant documented in three places.** The new UI comment
  (frontend/src/components/views/BacklogView.tsx:414-419: "The backend
  (`BacklogStore::requeue`) … refuses pending/in-flight items, so a stale click is a
  safe no-op"), the new test header (BacklogView.test.ts:5-7), and the requeue doc
  comment all describe a guard that the IPC surface bypasses.
- **Stale doc comment.** `backlog_retry`'s doc (backlog_cmds.rs:139: "Re-queue a
  failed/cant-resolve item…") no longer matches the done-reset semantics the UI now
  exposes.

**Fix:** make `backlog_retry` call `store.requeue(id)`, ignore the returned bool
(stale click = no-op, matching the documented contract), and update its doc comment
to cover done-reset + the terminal-only guard. Add a regression test asserting the
command (or a thin `requeue`-based impl) refuses an in-flight item — currently no
test covers the IPC path, which is exactly why this slipped through.

### 2. CONSTITUTION COMPLIANCE (minor) — CRLF reintroduced into two edited Rust files

Git's own output during the diff: "in the working copy of 'src-tauri/src/ipc/run_all.rs',
CRLF will be replaced by LF the next time Git touches it", and the same for
'src/backlog.rs'. The repo pins LF via `.gitattributes`; this is the recurring
reintroduction pattern noted in project memory. Renormalize the two files (the index
stays LF, but the working copies violate the pin).

### 3. BUGS (minor) — CantResolve note claims a rollback that may not have happened

`src-tauri/src/ipc/run_all.rs:340-355` (Some(other) arm): the note
"…— rolled back to checkpoint" is written unconditionally. If `rollback()` fails it
is only `eprintln!`-ed (line 346), and if `checkpoint_sha` is `None` no rollback is
attempted at all — yet the stored note still asserts the rollback happened. Morning
review trusting that note could assume the tree is clean when it is not. Make the
note reflect reality ("rollback failed: {e}" / "no checkpoint recorded"), as the
error text is already in scope.

## Verified correct (no action needed)

- **Gate is airtight.** `plan_loop_allows_done` (run_all.rs:100-102) is an
  allowlist `matches!(state, Complete)`. `WorkflowState` has exactly five variants
  (src/workflow/mod.rs:22-45: Planning/Executing/Reviewing/Complete/Skill) and
  `plan_loop_rejects_every_non_complete_state` covers all four non-Complete ones —
  the test is exhaustive today, and any future variant fails closed by construction.
- **None-arm early return** (run_all.rs:357-374) skips the done-counter bump
  (390-394) and the `stopped` check (397-400), but `end_run` drops the
  `RunAllState` (the counter's container) and emits the backlog-changed event
  itself, so the skipped work is unobservable. Correct in effect; halting without
  advancing is exactly the requirement ("there is no other way").
- **Halt-for-approval does not double-resolve.** Trace: halt marks the item Failed
  + clears run-all BEFORE the turn resolves → at resolution `run_all_active=false`;
  run-all items never set `single_in_flight` (only `dispatch_item`,
  backlog_cmds.rs:279) → `in_flight_id == 0` → no re-mark; auto-feed was forced
  `false` at run start (backlog_cmds.rs:310) → no re-dispatch. The item correctly
  keeps Failed + its sha note.
- **State-read timing is sound.** `Complete` is a resting state that persists to
  turn end (`finish` is the only Reviewing→Complete exit, src/workflow/mod.rs:996;
  research plans auto-complete via their last step, plan_file.rs), and
  `on_main_turn_resolved` fires only after the turn fully resolves, so the gate
  reads the finished turn's terminal state. No per-turn reset to Planning exists.
- **No leftovers from the config-flag removal.** Repo-wide search:
  `run_all_strict_success`/`strict_success` survives only in `.coding/**` history
  (plans/reviews/logs — historical, fine) and the new compatibility test itself.
  No frontend, fixture, or doc readers remain; PLAN.md's Backlog+Run-All section
  accurately describes the mandatory gate.
- **Tests pin the changes.** `requeue_*` (src/backlog.rs:405-455: terminal→Pending +
  note cleared + persists; refuses Pending/InFlight/unknown with status+note
  intact) and the `plan_loop_*` pair are sound; the config-compat test correctly
  proves old config.toml files still parse; BacklogView.test.ts follows the
  established source-contract style and is registered in vitest.config.ts.
  (Its header's backend-guard claim is wrong until Finding 1 is fixed.)
- **Doc comments / no dead code / warning-free.** All new/changed public functions
  (`plan_loop_allows_done`, `requeue`) have doc comments; `strict_success_allows_commit`
  fully removed; `main_agent_workflow_state` correctly hardened to
  `Option<WorkflowState>` with None treated as unverifiable-never-success on both
  dispatch paths.

## Summary

The plan-loop gate itself is correctly implemented and well-tested; the one real
defect is that the reset-done feature's safety guard (`requeue`) was never connected
to the IPC command that the UI calls — `backlog_retry` remains unguarded, leaving
both the double-queue hazard and the documented-but-absent invariant (Finding 1,
must fix). Findings 2 (CRLF) and 3 (misleading note) are minor.
