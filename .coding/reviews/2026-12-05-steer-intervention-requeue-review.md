## Verdict: FINDINGS (2 high, 2 low)

Two HIGH correctness holes in the new intervention routing leave a run-all item permanently stranded at `InFlight` — a status with no UI recovery path (requeue refuses InFlight, dispatch requires Pending). Two LOW: a stale `single_in_flight` pointer leaked on send failure, and a PLAN.md edit spliced into the wrong bullet. The lock budget, preserved behaviors, security, and multi-platform neutrality all check out; the new tests pin the ordering invariants but miss exactly the two HIGH paths.

---

## HIGH findings

### HIGH 1 — the `closed_loop` fall-through strands a run-all item at `InFlight` (never Done, never requeued, no UI recovery)

**Where:** `src-tauri/src/ipc/run_all.rs:856-877` (latch consumption + fall-through) vs. `run_all.rs:1201` (`end_run` inside the steer branch of `halt_run_all`).

**Trace.** Steer on a run-all item mid-execution:
1. `send_suggestion` (agent.rs ~257-269): agent is running → latch = `{item_id: None, "steered by the user"}`, then `halt_run_all(…, stamp_failed=false)`.
2. `halt_run_all` steer branch: annotates the note, fills `latch.item_id = Some(id)`, then `end_run` at line 1201 → **`run_all = None`** (documented at module doc lines ~21-24: "the halt clears the run state … run-all items never enter `single_in_flight`").
3. The steer is **absorbed mid-work as a System message** (runtime comment, src/runtime/agent.rs:597-600) — the turn *continues*, the agent finishes the plan, the workflow reaches `Complete` with transitions observed.
4. Resolution: latch consumed → `closed_loop = success && plan_loop_allows_done(Complete, true)` = **true** → falls through **without** calling `handle_user_intervention`, discarding `iv` (and its captured item id).
5. Fall-through: line 880 `run_all_active` = **false** (the halt already ended the run) → run-all branch skipped. Line 1032 `single_in_flight.take()` = **None** (run-all items never enter it) → single-dispatch branch skipped. Nothing resolves the item.

**Consequence:** the item stays `InFlight` forever. `BacklogStore::requeue` refuses InFlight (src/backlog.rs:364-369 — terminal statuses only), the ▶ dispatch path requires `Pending` (`pending_item`), and the frontend retry affordance is for failed/cant_resolve/done only (BacklogView `handleRetry`). The only recovery is the agent's `backlog_status` tool or hand-editing the JSONL. This directly contradicts the code's own comment (run_all.rs:856-859: "fall through to the normal Done path rather than discarding completed work") and the change summary's claim ("a steer absorbed by a completed task must still mark Done") — the fall-through only works for **single-dispatch** items (whose pointer survives); for run-all items the "normal Done path" was torn down by the very halt that set the latch. The success commit (`commit_success`) is also skipped, leaving the item's work uncommitted on top of its checkpoint.

**Fix direction:** when the consumed intervention has `closed_loop == true` AND `iv.item_id` is `Some` (the run-all halt captured it), resolve that item through the run-all success semantics (commit + `transition Done` + done bump, no next dispatch) instead of falling through to pointer-based branches that cannot see it. Alternatively, don't `end_run` at steer-halt time (set only the stop flag) so the fall-through reaches a live run-all branch — but that changes the documented race-avoidance ordering the `send_suggestion` comment relies on; the first option is surgical.

**Test gap:** none of the 5 new run_all.rs tests pins what the `closed_loop == true` branch does with a captured item id (`resolution_consumes_the_intervention_latch_before_any_stamping` only asserts latch-before-branch ordering). A source-contract test pinning that the closed-loop path resolves `iv.item_id` (not just falls through) would have caught this.

### HIGH 2 — a second intervention overwrites the latch, losing the captured item id → orphaned `InFlight`

**Where:** `src-tauri/src/ipc/agent.rs` — `send_suggestion` (~263-266) and `interrupt` (~307-317): both do `*latch = Some(UserIntervention { item_id: None, … })` — an unconditional overwrite.

**Trace.** Steer #1 on a run-all item mid-execution → latch set (item_id None) → halt fills `item_id = Some(id)` and ends the run. The turn is still running (the steer was absorbed mid-work). Steer #2 — or the user hits interrupt/stop — arrives while the same turn is still in flight: `running == true` → latch **overwritten** with `item_id: None`. The second `halt_run_all` call is a no-op (run_all already cleared → `if let Some(r) = guard.as_ref()` doesn't fire), so the id is never refilled. At resolution: `iv.item_id = None` → run_all None → single_in_flight None → **no item handled**. The item stays `InFlight` — the same unrecoverable stranded state as HIGH 1. The `already_halted`/`iv.item_id.is_none()` fill logic in `halt_run_all` (1190-1199) shows the merge concern was anticipated for the halt, but the two setter sites clobber.

Steer-then-steer and steer-then-interrupt are both ordinary user sequences ("one more thought…", steer then hit stop). Fix: merge instead of overwrite — e.g. `latch.get_or_insert(...)` then update `reason`, preserving any captured `item_id` (mirroring the halt's fill-if-none semantics); apply the same merge in both `send_suggestion` and `interrupt`.


## LOW findings

### LOW 1 — `dispatch_item` leaks the `single_in_flight` pointer when `manager.send` fails

**Where:** `src-tauri/src/ipc/backlog_cmds.rs:309-316`. The pointer is set (before `send`, closing the hyper-fast-resolution race) but the `?` at 316 returns on send error without clearing it. A later unrelated main turn's resolution takes the stale id and resolves a never-dispatched item through the plan gate (potentially even `Done`). Rare (send fails only when the agent loop channel is closed), but the fix is one line: on the send-error path, clear `single_in_flight` before returning the `Err`.

### LOW 2 — PLAN.md edit spliced into the wrong bullet (doc corruption)

**Where:** `PLAN.md:812-819`. The new run-all/intervention sentences were inserted into the middle of the "**Extra workflow tools + sub-plan stack**" bullet, replacing the original tool list ("beyond `create_plan` / …") with "dispatch / run-all / stop-all", leaving the tail "arbitrarily plans resume correctly from disk." as a dangling fragment with no lead-in (the sub-plan-stack description it concluded is gone), and duplicating the run-all checkpoint/commit/rollback/halt text that already lives in the adjacent "**Backlog subsystem + overnight Run-All loop**" bullet (820-824). Fix: restore the original workflow-tools bullet and put the intervention/InFlight-at-Executing-entry sentence in the Backlog subsystem bullet where it belongs.

## Verified good (review focus areas)

1. **Lock budget/nesting (forwarder arm)** — clean. `events.rs:594-601`: the manager lock is scoped inside the `is_main` block and **dropped before** `stamp_backlog_in_flight(&app)` is awaited; the stamp takes `run_all` (guard dropped at the block end) → store — no nesting, no cycle (no path locks store→manager or store→run_all; halt_run_all drops the run_all guard at 1148 before its store locks; the defer path takes store, releases, then run_all sequentially; std mutexes (`current_item`, `single_in_flight`, `user_intervention`) are held only across clone/assign with no await inside). The "at most once" contract holds: `should_stamp_in_flight` fires only on entry into Executing and the stamp only lifts still-Pending items (idempotent re-entry). Lock-ordering `run_all → store` is consistent everywhere observed. No deadlock.
2. **Preserved behavior** — approval halt still passes `stamp_failed=true` (events.rs:790-794) and stamps terminal `Failed` at halt time (load-bearing per SPEC memory: the resolution can no longer see the item). Run-all success/commit path (commit_success, Done, done bump) unchanged. Plan-gate CantResolve + rollback for non-intervention failures unchanged. Auto-feed unchanged. Deferred dispatch clears `current_item` and keeps the sha in the note via `set_note` (run_all.rs:765-777, verified). `extract_checkpoint_sha` head-parses all annotate suffixes (tests 611-641). The `Pending → Done/Failed/CantResolve` rows the new scheme relies on exist in the transition table. All 3 `BacklogContext` sites got the latch init (no Default impl to drift).
3. **Interrupt-on-idle / steer-between-dispatch-and-Started** — correctly gated: latch only set when actually running; an idle interrupt starts no turn. The pre-Started steer is the same accepted residual window the code documents for the busy re-check; outcome is non-terminal (item stays queued).
4. **Security** — no new input surfaces; reason strings are internal constants; no injection paths. Clean.
5. **Multi-platform** — no Windows-only APIs or paths added; `workspace_id: None` fixes are platform-neutral. Clean.
6. **Tests** — the source-contract tests pin the ordering invariants well (latch-before-halt, halt-before-send, running gates, latch-consumed-first, handler non-terminal/never-continues, Pending-only stamp). Gaps noted in HIGH 1/2 (nothing pins the closed-loop disposition of `iv.item_id`, or latch-merge semantics — exactly the two broken paths). Suggested regression tests: (a) after HIGH 1's fix, pin that the closed-loop branch resolves `iv.item_id` (Done + commit, no next dispatch); (b) after HIGH 2's fix, pin that the latch setters preserve a captured `item_id` on overwrite. The `fn_body` needle-composition trick is sound (verified against the real function bodies' ordering). Stated test results (lib 1718 / app 187 green) are consistent with the code but were not independently re-run by the reviewer.
