## Verdict: FINDINGS (0 high, 3 low)

All nine review points verified against the code — the fix is correct and complete for its stated scope: the landed arm recovers the slid resolution (e33a07fd), the guard blocks the 5bb1e4cd flip in both arms, the 2026-08-20 hole stays plugged, and the drain/amplifier paths are untouched. Three LOW findings: two pre-existing adjacent holes in the same arm (unguarded Failed transitions; a residual (Complete,false)+Pending stall) and one observability asymmetry in the new blocked path. None block the merge.


## Verification of the review checklist (all 9 items)

**1. The 2026-08-20 hole stays plugged — VERIFIED.**
- `closed_earlier` (run_all.rs:2790-2795) requires `item.status == BacklogStatus::InFlight` (line 2793); a never-planned item is Pending (the Executing-entry stamp never fired) → false.
- `orphan_work_landed` (run_all.rs:3333-3340) returns false for a None plan_id before any I/O.
- A resting-Complete freeform turn with a Pending item → scrutinee `(Complete, false)` → the non-closure arm (2869-2897) annotates + returns — no dispatch, no flip.
- Single-dispatch path: `plan_linkage_allows_done` (391-407) — the status guard (396-398) rejects Pending; `(None, Some(_)) => false` (402) rejects a never-planned InFlight item.

**2. The happy path is unregressed — VERIFIED.**
- `finish()` does NOT pop the plan stack (src/workflow/mod.rs:797-808; doc at 820-823: "The plan stack is left UNTOUCHED. In `Complete` it holds the just-finished plan"), and `top_plan_id()` returns the ROOT plan (src/workflow/mod.rs:319-321) — so `main_agent_top_plan_id` (run_all.rs:3442-3449) reports the completed plan at resolution time. The linkage comparison is apples-to-apples: the stamp records the ROOT plan id (`stamp_backlog_in_flight`, run_all.rs:3812-3821, refreshed on every Executing entry so a re-planned item re-links).
- InFlight + plan_id X == top_plan_id X + loop_evidence → the gate arm (2796-2837): `may_flip=true` → `commit_success` (2827; skips a clean tree — the dirty check in git_ops.rs:~176) → Done (2830-2834) → `terminal_resolution` → done-counter bump (2955-2957) + pointer clear (2966) + dispatch next (3000). The guard does not skip the legitimate own-item done→next chain.

**3. The blocked case — VERIFIED.**
- The guard (2809-2825) precedes `commit_success` (2827) — a blocked flip commits nothing under the item's name.
- Blocked → no transition (the re-queue note is preserved), `terminal_resolution` stays false → no done-counter bump (2955-2957); the stale `current_item` pointer is cleared either way (2966) so the dispatch (3000) resolves against the queue, not the wrong item. The re-queued item is Pending again → eligible for re-dispatch.
- Single-dispatch: the blocked row (3059-3067) rides the existing `(None, note)` machinery — annotate + pointer restore (3120-3127) + auto-feed suppressed (`resolved_terminally=false`). A restored pointer does not strand the machinery: `dispatch_item` overwrites `single_in_flight` on the next ▶ dispatch (backlog_cmds.rs:410-412), and the main-agent Exited handler clears it (events.rs:698-702).

**4. The amplifier behavior unchanged — VERIFIED.**
- `closed_earlier` requires `main_state_opt == Some(WorkflowState::Complete)` (2791), so for every non-Complete state the scrutinee `(main_state_opt, loop_evidence || closed_earlier)` is identical to the old `(main_state, loop_evidence)` → the non-closure arm (2869-2897) still annotates + returns; the run stays armed.
- Turn failures: the else branches (2899-2944) are unchanged — annotate + return, run armed.

**5. The drain's orphan recovery unregressed — VERIFIED.**
- The diff touches only the module doc, the helper, the two arms of `on_main_turn_resolved`, and tests. `orphan_work_landed` (3333-3355) and `drain_run_all_on_main_exit` (3376-3436) are unchanged; the drain's done-orphan guard tests are in the green suites (258+4+2 passed).

**6. Lock discipline — VERIFIED.**
- `orphan_work_landed` is called (2794) with no store lock held (the item snapshot was cloned and the lock released; `root` is cloned with the guard dropped at statement end). Inside, the root lock is taken only to clone and released before the git subprocess (3341-3343) — mirroring the drain's documented pattern (3400-3404).
- Both guard blocks compute `completed_plan` BEFORE taking the store lock (2808, 3037) — no manager/agent_loops/workflow lock is awaited under the store lock.
- The check-then-transition window (may_flip read → commit_success → transition) cannot be raced by the drain: the forwarder awaits `on_main_turn_resolved` and `drain_run_all_on_main_exit` inline (events.rs:598-647, 697) — sequential event processing. Same shape as the pre-existing code (which had no guard at all — strictly narrower now).

**7. Multi-platform neutrality — VERIFIED.** No Windows-only APIs, paths, or shell syntax in the diff; pure Rust logic + git via existing helpers.

**8. Docs sync — VERIFIED.**
- The module doc's success-criterion section (77-128) matches the code: the earlier-turn closure (88-100), the guard (101-107), the abandonment + non-closure rows (108-128).
- The BUG knowledge file (.coding/knowledge/bug/2027-01-07-run-all-stalls-after-finish-complete-slid-resolu.md) is accurate — root cause, slide mechanism, fix, regression tests; its line references (2570, 2611-2639) are pre-fix positions, conventional for a historical record.
- Backlog bookkeeping consistent: 5bb1e4cd → done (fixed under this plan); e33a07fd → in_flight (re-dispatched for live verification, per its note). The BUG memory record exists (id d7b3fc47).

**9. Test quality — VERIFIED.**
- `complete_state_turn_end_with_landed_plan_dispatches_next` (1610-1658) pins the landed-predicate call, the `loop_evidence || closed_earlier` scrutinee, the non-closure annotate (hole plugged), and the `terminal_resolution` counter gating.
- `done_transitions_are_guarded_on_status_and_plan_linkage` (1660-1693) pins ≥2 guard sites and guard-before-commit ordering (the run-all arm is the only arm with a `commit_success`; the single-dispatch path has no git checkpoint).
- `plan_linkage_allows_done_matrix` (1695-1753) covers the full acceptance matrix: pending+X vs Y and pending+X vs X stay pending; in_flight+X vs X → done; in_flight+X vs Y blocked; in_flight+None vs Some blocked (2026-08-20); the unreadable-completed-plan fallback; terminal statuses never flip.
- `fn_body` extraction is sound: the needle `fn on_main_turn_resolved(` cannot match `on_main_turn_resolved_is_plan_tied` (the `_` breaks it) nor the test literals (no `fn ` prefix / `(`), and the column-0 `\n}\n` end anchor is correct for the function's brace structure.
- The source-contract assertions reference strings that did not exist pre-fix — consistent with "confirmed FAILING pre-fix".


## Findings

**LOW-1 (pre-existing, adjacent class — follow-up candidate): the `plan_abandoned` arms transition to Failed with no status/linkage guard.**
- Sites: run_all.rs:2844-2851 (run-all, success path), 2899-2908 (run-all, failure path), 3072-3075 and 3078-3082 (single-dispatch rows).
- `Pending → Failed` is a legal row (src/backlog.rs:308-313). The same steer-pivot scenario as 5bb1e4cd — a stale pointer referencing a re-queued (Pending) item while an unrelated plan runs — but with the unrelated plan ABANDONED instead of finished, flips the re-queued item to Failed and wipes its re-queue note. Same damage class as the fixed bug, narrower trigger (abandonment mid-run vs finish).
- Not a regression: these arms are unchanged by this diff. The new `plan_linkage_allows_done` guard is the template for the same protection on the Failed rows. Recommend a backlog follow-up rather than expanding this fix's scope.

**LOW-2 (new code, observability): the run-all blocked flip is silent.**
- The gate arm's blocked path (`if may_flip { ... }` with no else, run_all.rs:2826-2836) falls through with no annotation on the item, while the single-dispatch blocked row annotates "completing plan is not this item's plan — item kept, no transition" (3059-3067).
- The plan's contract only requires "note preserved" (satisfied), and the pointer clear + next dispatch are observable — but the item itself carries no record that a completing turn skipped it. If the re-queued item is not the next pending, the gap is visible in the Backlog tab until its re-dispatch.
- Caveat if fixing: an annotation is transient — the immediate re-dispatch's `set_note` replaces the note with the fresh checkpoint sha, so the value is limited to the window before re-dispatch. An `annotate` in the blocked path (append-safe: `extract_checkpoint_sha` head-parses) would mirror the single-dispatch arm; skipping with that rationale is also defensible.

**LOW-3 (pre-existing residual, narrow — follow-up candidate): a slid resolution with a re-queued (Pending) item still stalls.**
- The landed arm requires InFlight (run_all.rs:2793). If the item was externally re-queued to Pending mid-run (the 5bb1e4cd-style edit) AND the completing turn's resolution slides (the e33a07fd slide), the notification turn lands at (Complete, false) with the item Pending → `closed_earlier=false` → the non-closure arm waits forever (Complete is resting; no auto-continue).
- This is exactly the combination of the two live incidents. Not a regression — pre-fix the same scenario also stalled; the fix strictly improves (it recovers the InFlight variant, and the (Complete, true) variant recovers via the blocked path's pointer clear + dispatch). Recovery today: stop/restart the run, or agent exit (the drain ends the run; the Pending item stays queued).
- A follow-up could consult landed evidence for the re-queued item, or clear the pointer + dispatch next at (Complete, false) with a Pending item. Suggest a backlog item rather than scope creep here.

## Informational notes (no action required)

- The `(InFlight, _, None) => true` fallback (run_all.rs:405) trusts the status guard alone when the completed plan is unreadable — deliberate (documented in the matrix test as "no happy-path regression") and effectively unreachable in practice: the forwarder serializes events, so the agent cannot exit between the workflow-state read and the top_plan_id read within one resolution.
- `may_flip_done` is computed before the (status, note) match even on paths that never consult it (turn failures, abandonment) — two lock acquisitions of wasted work, no correctness impact; required there so the blocked row can exist.
- The knowledge file's line references (2570, 2611-2639) are pre-fix positions — correct for a historical bug record; post-fix the gate is at 2796.
- e33a07fd sits `in_flight` with a "re-dispatched for verification" note — intentional live-verification bookkeeping; a live re-dispatch replaces the note with the checkpoint sha at dispatch time, so `orphan_work_landed`'s head-parsing is unaffected.

## Verdict rationale

The change is correct, minimal, and well-pinned: the landed arm reuses the drain's proven predicate with the right preconditions (InFlight + own-plan evidence), the guard closes the 5bb1e4cd flip in both arms with clean lock discipline, the 2026-08-20 hole and the amplifier semantics are provably unregressed, and the three tests pin the acceptance criteria (including the confirmed-failing pre-fix source-contract assertions). All three findings are LOW: one small in-scope polish (LOW-2) and two pre-existing adjacent gaps in the same arm that merit backlog follow-ups (LOW-1, LOW-3) rather than blocking this fix.
